//! Snapshot / restore for CLI config files.
//!
//! `backup_once` (sibling `.ccload-bak-*` files) could preserve bytes but gave
//! no way back: nothing recorded which files a takeover touched, and a file
//! that did NOT exist beforehand had no representation at all — restoring it
//! must DELETE what we wrote, not resurrect content.
//!
//! So every takeover first writes a snapshot: all of the target's config files
//! copied into `~/.ccload-client/backups/<id>/`, plus a manifest row recording
//! each file's prior existence. The first snapshot per target is marked
//! `pristine` — that is the "before ccLoad ever touched this machine" state.

use std::path::PathBuf;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::services::cli_io::{copy_atomic, write_atomic};
use crate::services::cli_types::{CliTarget, ConfigRoot};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupFile {
    /// Path relative to the config root, e.g. `.claude/settings.json`.
    pub rel: String,
    /// File name inside the snapshot dir. Absent when the file did not exist.
    pub stored: Option<String>,
    /// False means: restoring this entry must delete the file.
    pub existed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupEntry {
    pub id: String,
    pub target: CliTarget,
    pub created_at: u64,
    /// Why the snapshot was taken, shown in the UI.
    pub reason: String,
    /// True for the first snapshot of a target: the original user state.
    pub pristine: bool,
    /// 拍这份快照时的配置根（沙箱路径或真实 home）。
    ///
    /// 沙箱和真实 home 共用同一个 BackupStore，而恢复是 `root.join(rel)`。
    /// 不记根的话，沙箱里那份「原始」快照（沙箱是空的，条目全是 existed:false）
    /// 可以被拿去恢复真实 home —— 结果是**删掉** ~/.claude/settings.json 和
    /// ~/.codex/auth.json（ChatGPT 的 OAuth 在里面），而且 restore 之前没有
    /// 任何后路。空字符串 = 旧清单里的条目，按「和当前根匹配」处理。
    #[serde(default)]
    pub root: String,
    pub files: Vec<BackupFile>,
}

impl BackupEntry {
    /// 这份快照是不是在 `tag` 这个根下拍的。
    ///
    /// 空 root = 旧版本写的条目，那时只有一个根在用，按「匹配」处理，
    /// 免得升级上来的用户突然看不到自己的历史快照。
    pub(crate) fn matches_root(&self, tag: &str) -> bool {
        self.root.is_empty() || self.root == tag
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Manifest {
    entries: Vec<BackupEntry>,
}

/// 每个 CLI 最多留几份快照。超出后按时间从旧到新淘汰。
///
/// 不设上限的话每次「应用」「导入模型」「装扩展」都会留一份完整拷贝 ——
/// `~/.claude.json` 一份就 300 KB，几十次之后目录会大到没人想去翻。
pub const MAX_SNAPSHOTS_PER_TARGET: usize = 5;

pub struct BackupStore {
    dir: PathBuf,
    /// 串行化清单的「读—改—写」—— 别删。
    ///
    /// 快照/回滚共用一份 `manifest.json`，视觉 MCP 批量装卸会同时给五个 target
    /// 各发一次写入，而每个公开方法都是 `load()` → 改 entries → `save()`。没有这
    /// 把锁，两个并发调用会读到同一份清单、各自追加自己那条、后写的把先写的整个
    /// 盖掉：先写的那次快照文件还在磁盘上，却再也没有回滚入口。
    ///
    /// `write_atomic` 挡不住这个。它现在的临时文件名是唯一的（pid + 计数器），
    /// 所以清单一定是完整的 JSON、不会再出现两个文档首尾相接的
    /// `trailing characters at line N`；但「谁的 entry 活下来」是它管不着的事。
    ///
    /// 边界：只管得住本进程。两个客户端实例同时跑仍然会丢条目 —— 那时清单本身
    /// 还是合法 JSON，`parse_manifest` 的抢救兜不住，只是没人再引用那份快照。
    lock: Mutex<()>,
}

impl BackupStore {
    pub fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            lock: Mutex::new(()),
        }
    }

    fn guard(&self) -> std::sync::MutexGuard<'_, ()> {
        self.lock.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// 快照根目录。`cli_backup_diff` 要按 `<dir>/<id>/<stored>` 直接读快照里的
    /// 文件内容 —— 只读，不经过 restore（restore 会往真实配置写）。
    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }

    fn manifest_path(&self) -> PathBuf {
        self.dir.join("manifest.json")
    }

    fn load(&self) -> Result<Manifest, AppError> {
        let p = self.manifest_path();
        if !p.exists() {
            return Ok(Manifest::default());
        }
        let raw = std::fs::read_to_string(&p)?;
        if raw.trim().is_empty() {
            return Ok(Manifest::default());
        }
        let (m, healed) = parse_manifest(&raw)?;
        // A recovered file must be rewritten now: leaving the junk on disk means
        // every command after this one has to salvage again (and a build without
        // the salvage path still dies on it).
        if healed {
            tracing::warn!(
                "backup manifest had a concatenated tail (written by an older build); \
                 salvaged {} entries and rewrote it",
                m.entries.len()
            );
            self.save(&m)?;
            self.sweep_orphan_dirs(&m);
        }
        Ok(m)
    }

    fn save(&self, m: &Manifest) -> Result<(), AppError> {
        std::fs::create_dir_all(&self.dir)?;
        let body =
            serde_json::to_string_pretty(m).map_err(|e| AppError::Config(e.to_string()))?;
        // write_atomic 给的是权限保留（rename 换 inode，清单不会从 0600 退回
        // 0644）+ 唯一的临时文件名。后者保证落地的永远是一份完整 JSON；至于并发
        // 调用之间谁的 entry 活下来，那是 `lock` 的事，见该字段的注释。
        write_atomic(&self.manifest_path(), &format!("{body}\n"))
    }

    /// 抢救之后收掉没人认领的快照目录。
    ///
    /// 被丢掉的那截尾巴里的 entry 已经不在清单里了，可它们的 `backups/<id>/`
    /// 还在磁盘上 —— `prune` 只删自己刚移除的那几条，这批永远等不到人来收。
    /// 只在抢救成功后调用：正常路径上 `snapshot` 是先 `load` 再建目录，此时扫一遍
    /// 会把它正要写的目录当成孤儿。
    ///
    /// `extra/` 是 `backup_extra` 的地盘、本来就不进清单，必须跳过。
    fn sweep_orphan_dirs(&self, m: &Manifest) {
        let Ok(rd) = std::fs::read_dir(&self.dir) else {
            return;
        };
        for e in rd.filter_map(Result::ok) {
            if !e.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let Ok(name) = e.file_name().into_string() else {
                continue;
            };
            if name == "extra" || m.entries.iter().any(|x| x.id == name) {
                continue;
            }
            // 跟 prune 一样：删不掉（权限/已被手动清理）不该让这次读取失败，
            // 清单已经不指向它了，留下的只是磁盘上的孤儿。
            let _ = std::fs::remove_dir_all(e.path());
        }
    }

    /// Rewrite a concatenated leftover tail if present. Safe to call at
    /// process start; a fully unreadable file is left for the next write
    /// to report rather than taking the app down with it.
    pub fn heal(&self) -> Result<(), AppError> {
        let _g = self.guard();
        self.load().map(|_| ())
    }

    /// 某个根下的快照。**必须传当前根** —— 沙箱和真实 home 共用一个 store，
    /// 把沙箱的快照列给真实 home 看，用户点一下就会删掉真实配置。
    pub fn list_in(
        &self,
        root: &ConfigRoot,
        target: Option<CliTarget>,
    ) -> Result<Vec<BackupEntry>, AppError> {
        let tag = root.tag();
        let mut entries = self.list(target)?;
        entries.retain(|e| e.matches_root(&tag));
        Ok(entries)
    }

    pub fn list(&self, target: Option<CliTarget>) -> Result<Vec<BackupEntry>, AppError> {
        let _g = self.guard();
        let mut entries = self.load()?.entries;
        if let Some(t) = target {
            entries.retain(|e| e.target == t);
        }
        // Newest first — the UI restores from the top.
        entries.reverse();
        Ok(entries)
    }

    /// Snapshot every config file of `target` before it is modified.
    /// `id` must be unique and filesystem-safe (we use a unix timestamp).
    pub fn snapshot(
        &self,
        root: &ConfigRoot,
        target: CliTarget,
        id: &str,
        reason: &str,
    ) -> Result<BackupEntry, AppError> {
        self.snapshot_paths(root, target, id, reason, target.relative_paths())
    }

    /// Snapshot an explicit file list. Only takeover uses the full list; tests
    /// use this to build an entry shaped like one an older build wrote, back
    /// when a target had fewer known files.
    pub fn snapshot_paths(
        &self,
        root: &ConfigRoot,
        target: CliTarget,
        id: &str,
        reason: &str,
        paths: &[&str],
    ) -> Result<BackupEntry, AppError> {
        let _g = self.guard();
        let mut manifest = self.load()?;
        let tag = root.tag();
        // The first snapshot of a target captures the user's original setup.
        // **按根分别算**：在沙箱里拍过的快照不能让真实 home 的首次接管失去
        // pristine 资格 —— 那会让用户真正的原始配置在 5 次写入后被轮换掉。
        let pristine = !manifest
            .entries
            .iter()
            .any(|e| e.target == target && e.matches_root(&tag));

        let snap_dir = self.dir.join(id);
        std::fs::create_dir_all(&snap_dir)?;

        let mut files = Vec::new();
        for rel in paths {
            let src = root.join(rel);
            if src.exists() {
                // Flatten the relative path so nested dirs need no mkdir.
                let stored = rel.replace(['/', '\\'], "__");
                std::fs::copy(&src, snap_dir.join(&stored))?;
                files.push(BackupFile {
                    rel: (*rel).to_string(),
                    stored: Some(stored),
                    existed: true,
                });
            } else {
                // Recorded explicitly: restoring must delete what we create.
                files.push(BackupFile {
                    rel: (*rel).to_string(),
                    stored: None,
                    existed: false,
                });
            }
        }

        let entry = BackupEntry {
            id: id.to_string(),
            target,
            created_at: now_secs(),
            reason: reason.to_string(),
            pristine,
            root: tag,
            files,
        };
        manifest.entries.push(entry.clone());
        self.prune(&mut manifest, target);
        self.save(&manifest)?;
        Ok(entry)
    }

    /// 把该 target 的快照裁到 `MAX_SNAPSHOTS_PER_TARGET` 份，最旧的先删。
    ///
    /// pristine 那一条不参与淘汰：它是「ccLoad 从没碰过这台机器」的样子，删掉就
    /// 再也回不去了，而它恰好总是最旧的一条 —— 纯按时间淘汰第一个删的就是它。
    /// 所以额度是「pristine + 最近的 N-1 条」。
    fn prune(&self, manifest: &mut Manifest, target: CliTarget) {
        let mut candidates: Vec<(u64, usize)> = manifest
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.target == target && !e.pristine)
            .map(|(i, e)| (e.created_at, i))
            .collect();
        let pristine = manifest
            .entries
            .iter()
            .filter(|e| e.target == target && e.pristine)
            .count();
        let keep = MAX_SNAPSHOTS_PER_TARGET.saturating_sub(pristine);
        if candidates.len() <= keep {
            return;
        }
        // 同一秒内的多次写入 created_at 相同，用下标兜底保持稳定顺序。
        candidates.sort_unstable();
        let doomed = candidates.len() - keep;
        let mut idx: Vec<usize> = candidates[..doomed].iter().map(|(_, i)| *i).collect();
        // 从大到小删，否则前面的 remove 会把后面的下标顶偏。
        idx.sort_unstable_by(|a, b| b.cmp(a));
        for i in idx {
            let e = manifest.entries.remove(i);
            // 目录删不掉（权限/已被手动清理）不该让这次写入失败 —— 清单已经不
            // 指向它了，留下的只是磁盘上的孤儿。
            let _ = std::fs::remove_dir_all(self.dir.join(&e.id));
        }
    }

    /// 快照一个不在 target `relative_paths()` 里的文件（典型是 Claude Code 的
    /// `~/.claude.json`，我们只往里合并一个 key）。
    ///
    /// 它同样要**进清单**。以前这里只是往 `extra/` 拷一份就完事，既不写
    /// manifest 也不产生 BackupEntry —— 于是「CLI 接管页可回滚」这句话对
    /// `~/.claude.json` 是假的：那个文件装着所有项目记录、oauthAccount 和一堆
    /// MCP 的 Authorization 头，我们改写了它，用户却没有任何回滚入口。
    /// 路径本来就在 root 之下，`restore` 的 `root.join(rel)` 直接就能还原。
    pub fn snapshot_extra(
        &self,
        root: &ConfigRoot,
        target: CliTarget,
        rel: &str,
        stamp: &str,
        reason: &str,
    ) -> Result<(), AppError> {
        let _g = self.guard();
        let src = root.join(rel);
        let mut manifest = self.load()?;
        let snap_dir = self.dir.join(stamp);
        std::fs::create_dir_all(&snap_dir)?;

        let stored = rel.replace(['/', '\\'], "__");
        let existed = src.exists();
        if existed {
            std::fs::copy(&src, snap_dir.join(&stored))?;
        }
        manifest.entries.push(BackupEntry {
            id: stamp.to_string(),
            target,
            created_at: now_secs(),
            reason: reason.to_string(),
            // 这类文件不在接管的文件集里，不代表「机器的原始状态」，
            // 所以永远不标 pristine —— 那个名额留给真正的首次接管快照。
            pristine: false,
            root: root.tag(),
            files: vec![BackupFile {
                rel: rel.to_string(),
                stored: existed.then_some(stored),
                existed,
            }],
        });
        self.prune(&mut manifest, target);
        self.save(&manifest)?;
        Ok(())
    }

    /// 纯拷贝、不进清单的旧接口。只剩测试在用；新代码一律走 `snapshot_extra`。
    pub fn backup_extra(&self, path: &std::path::Path, stamp: &str) -> Result<(), AppError> {
        let _g = self.guard();
        if !path.exists() {
            return Ok(());
        }
        let dir = self.dir.join("extra");
        std::fs::create_dir_all(&dir)?;
        let name = path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| AppError::Config("extra backup needs a file name".into()))?;
        std::fs::copy(path, dir.join(format!("{stamp}.{name}")))?;
        prune_extra(&dir, name);
        Ok(())
    }

    /// Put the files back exactly as the snapshot found them.
    pub fn restore(&self, root: &ConfigRoot, id: &str) -> Result<Vec<String>, AppError> {
        // 先给**现状**留一份，再覆盖。
        //
        // 恢复是不可逆地覆盖当前配置，而且「原始」那一份的语义是 existed:false ——
        // 恢复它会**删掉**文件（Codex 的 auth.json 里是 ChatGPT 的 OAuth）。点错
        // 一行之前没有任何后路。这一份快照就是那条后路。
        //
        // 拿不到 target 或快照失败不该挡住恢复本身：用户点恢复通常是因为当前
        // 状态已经坏了，为了留档案把唯一的修复手段也堵死是本末倒置。
        if let Ok(manifest) = self.load() {
            if let Some(target) = manifest.entries.iter().find(|e| e.id == id).map(|e| e.target) {
                let _ = self.snapshot(root, target, &unique_stamp(), "pre-restore");
            }
        }
        let _g = self.guard();
        let manifest = self.load()?;
        let entry = manifest
            .entries
            .iter()
            .find(|e| e.id == id)
            .ok_or_else(|| AppError::Config(format!("no backup with id {id}")))?;
        // 跨根恢复必须挡住，这是最后一道。沙箱里那份「原始」快照的条目全是
        // existed:false（沙箱当时是空的），拿它恢复真实 home 就是逐个 remove_file
        // —— ~/.claude/settings.json 和 ~/.codex/auth.json（ChatGPT 的 OAuth）
        // 当场消失。界面已经按根过滤了，但命令是公开的，别只靠界面。
        if !entry.matches_root(&root.tag()) {
            return Err(AppError::Config(
                "这份快照是在另一个配置根下拍的（沙箱 ↔ 真实目录），不能拿来恢复当前目录 —— \
                 它记录的「文件当时不存在」会变成删除。先在设置里切回拍它时的那个模式。"
                    .into(),
            ));
        }

        let snap_dir = self.dir.join(&entry.id);
        let mut touched = Vec::new();
        for f in &entry.files {
            let dest = root.join(&f.rel);
            match (&f.stored, f.existed) {
                (Some(stored), true) => {
                    let src = snap_dir.join(stored);
                    if !src.exists() {
                        return Err(AppError::Config(format!(
                            "backup file missing: {}",
                            src.display()
                        )));
                    }
                    if let Some(parent) = dest.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    // Same-dir temp + rename keeps the restore atomic too.
                    copy_atomic(&src, &dest)?;
                    touched.push(dest.display().to_string());
                }
                _ => {
                    // Did not exist before the takeover → remove ours.
                    if dest.exists() {
                        std::fs::remove_file(&dest)?;
                        touched.push(format!("{} (removed)", dest.display()));
                    }
                }
            }
        }
        // Snapshots only replay the files they captured. A backup older than a
        // target's current file list would otherwise leave the takeover half
        // standing — see `heal_unsnapshotted`.
        let recorded: Vec<String> = entry.files.iter().map(|f| f.rel.clone()).collect();
        crate::services::cli_config::heal_unsnapshotted(root, entry.target, &recorded)?;
        Ok(touched)
    }
}

/// Parse the manifest, salvaging a concatenated leftover tail.
///
/// This is now a **repair path for manifests written by older builds**, not a
/// live failure mode. Those builds shared one tmp file per target path
/// (`.manifest.json.ccload-tmp`): two writers landing on it left the longer
/// document's tail after the shorter one's closing `}`, and serde_json died
/// with `trailing characters at line N`, which blocked every later snapshot.
/// `cli_io::write_atomic` now gives each writer a unique tmp, so we no longer
/// produce this — but a machine that ran an older build still has the broken
/// file on disk, and it must survive the upgrade.
///
/// The leading value is whichever writer's bytes won at offset 0: a complete,
/// self-consistent snapshot list, though not necessarily the newest one. We
/// take it and let `load` rewrite the file — entries only in the discarded tail
/// are gone, so `load` also sweeps the snapshot dirs they left behind.
fn parse_manifest(raw: &str) -> Result<(Manifest, bool), AppError> {
    match serde_json::from_str::<Manifest>(raw) {
        Ok(m) => Ok((m, false)),
        Err(e) => {
            let mut de = serde_json::Deserializer::from_str(raw);
            match Manifest::deserialize(&mut de) {
                Ok(m) => Ok((m, true)),
                Err(_) => Err(AppError::Config(format!("backup manifest is corrupt: {e}"))),
            }
        }
    }
}

/// 全进程唯一的备份 id。
///
/// 必须只有这一个计数器。以前 commands/cli.rs、commands/models.rs、
/// commands/extensions.rs 各自持有一个从 0 开始的 AtomicU32，于是「接管」和
/// 「导入模型」在同一秒里都会产出 `{secs}-0`：后一次快照按同样的扁平文件名写进
/// 同一个目录，把前一次的原始副本盖掉，清单里还多出一个重复 id。之后
/// `restore("…-0")` 用 `.find()` 命中第一条，读到的却是第二条的字节 ——
/// 「回滚到接管前」实际恢复的是接管后的文件，而且报成功。
pub fn unique_stamp() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static SEQ: AtomicU32 = AtomicU32::new(0);
    format!("{}-{}", now_secs(), SEQ.fetch_add(1, Ordering::Relaxed))
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// `extra/` 里同一个文件名的拷贝也按同样的额度裁剪。
///
/// 这里没有清单可查，只能按文件名后缀分组、用文件名里的 stamp 排序 —— stamp 是
/// `{unix秒}-{序号}`，字典序和时间序一致（秒数位数在 2001 年之后就不变了）。
/// `~/.claude.json` 一份 300 KB，不裁的话装十个扩展就是 3 MB 的重复拷贝。
fn prune_extra(dir: &std::path::Path, name: &str) {
    let suffix = format!(".{name}");
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut mine: Vec<String> = rd
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|f| f.ends_with(&suffix))
        .collect();
    if mine.len() <= MAX_SNAPSHOTS_PER_TARGET {
        return;
    }
    mine.sort_unstable();
    let doomed = mine.len() - MAX_SNAPSHOTS_PER_TARGET;
    for f in &mine[..doomed] {
        let _ = std::fs::remove_file(dir.join(f));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, BackupStore, ConfigRoot) {
        let dir = tempfile::tempdir().unwrap();
        let store = BackupStore::new(dir.path().join("bk"));
        let root = ConfigRoot::sandbox(dir.path().join("home"));
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        std::fs::write(root.join(".claude/settings.json"), "{}").unwrap();
        (dir, store, root)
    }

    #[test]
    fn each_target_keeps_at_most_five_snapshots() {
        let (_d, store, root) = store();
        for i in 0..9 {
            store
                .snapshot(&root, CliTarget::ClaudeCode, &format!("s{i}"), "test")
                .unwrap();
        }
        let kept = store.list(Some(CliTarget::ClaudeCode)).unwrap();
        assert_eq!(kept.len(), MAX_SNAPSHOTS_PER_TARGET);
        // s0 是 pristine（见下一个测试），淘汰从最旧的非 pristine 开始：
        // 留下 pristine + 最近 4 条 = s0, s5, s6, s7, s8。
        let ids: Vec<&str> = kept.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["s8", "s7", "s6", "s5", "s0"], "newest first");
        // 被淘汰的那几份连目录一起删掉，不留孤儿。
        assert!(!store.dir.join("s1").exists());
        assert!(!store.dir.join("s4").exists());
        assert!(store.dir.join("s8").exists());
    }

    /// 原始状态那一份是最旧的，纯按时间淘汰第一个删的就是它 —— 必须留住。
    #[test]
    fn the_pristine_snapshot_survives_eviction() {
        let (_d, store, root) = store();
        for i in 0..9 {
            store
                .snapshot(&root, CliTarget::ClaudeCode, &format!("s{i}"), "test")
                .unwrap();
        }
        let kept = store.list(Some(CliTarget::ClaudeCode)).unwrap();
        assert!(kept.iter().any(|e| e.pristine && e.id == "s0"));
        assert!(store.dir.join("s0").exists(), "pristine dir must stay");
    }

    /// 额度是按 CLI 分的，装满 Claude Code 不该动到 Codex 的历史。
    #[test]
    fn the_quota_is_per_target() {
        let (_d, store, root) = store();
        std::fs::create_dir_all(root.join(".codex")).unwrap();
        std::fs::write(root.join(".codex/config.toml"), "").unwrap();
        store.snapshot(&root, CliTarget::Codex, "c0", "test").unwrap();
        for i in 0..9 {
            store
                .snapshot(&root, CliTarget::ClaudeCode, &format!("s{i}"), "test")
                .unwrap();
        }
        assert_eq!(store.list(Some(CliTarget::Codex)).unwrap().len(), 1);
    }

    #[test]
    fn extra_copies_are_capped_too() {
        let (d, store, _root) = store();
        let src = d.path().join(".claude.json");
        std::fs::write(&src, "{}").unwrap();
        for i in 0..9 {
            store.backup_extra(&src, &format!("178704{i:04}-0")).unwrap();
        }
        let n = std::fs::read_dir(store.dir.join("extra")).unwrap().count();
        assert_eq!(n, MAX_SNAPSHOTS_PER_TARGET);
    }

    /// A short overwrite that didn't truncate leaves the previous document's
    /// tail after the new `}`. Vision MCP install used to die on this and
    /// block every later snapshot.
    #[test]
    fn load_salvages_trailing_characters_and_rewrites_clean() {
        let (_d, store, _root) = store();
        let clean = r#"{
  "entries": [
    {
      "id": "s0",
      "target": "claude-code",
      "created_at": 1,
      "reason": "takeover",
      "pristine": true,
      "files": [
        { "rel": ".claude/settings.json", "stored": ".claude__settings.json", "existed": true }
      ]
    }
  ]
}"#;
        let leftover = r#"    "rel": ".codex/config.toml",
          "stored": ".codex__config.toml",
          "existed": true
        }
      ]
    }
  ]
}"#;
        std::fs::create_dir_all(&store.dir).unwrap();
        std::fs::write(store.manifest_path(), format!("{clean}{leftover}")).unwrap();
        let listed = store.list(None).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "s0");
        let rewritten = std::fs::read_to_string(store.manifest_path()).unwrap();
        serde_json::from_str::<Manifest>(&rewritten).unwrap();
        assert!(
            !rewritten.contains(".codex/config.toml"),
            "leftover tail must be dropped: {rewritten}"
        );
    }

    /// 抢救丢掉的尾巴里那些 entry 的快照目录没人会再来收 —— `prune` 只管自己
    /// 移除的那几条。`extra/` 不进清单，扫的时候不能连它一起端了。
    #[test]
    fn salvaging_the_manifest_sweeps_the_orphaned_snapshot_dirs() {
        let (_d, store, _root) = store();
        let clean = r#"{
  "entries": [
    {
      "id": "s0",
      "target": "claude-code",
      "created_at": 1,
      "reason": "takeover",
      "pristine": true,
      "files": [
        { "rel": ".claude/settings.json", "stored": ".claude__settings.json", "existed": true }
      ]
    }
  ]
}"#;
        let leftover = r#"    "rel": ".codex/config.toml",
          "stored": ".codex__config.toml",
          "existed": true
        }
      ]
    }
  ]
}"#;
        std::fs::create_dir_all(&store.dir).unwrap();
        std::fs::write(store.manifest_path(), format!("{clean}{leftover}")).unwrap();
        // s1 只出现在被丢掉的尾巴里；extra/ 从来就不在清单里。
        for d in ["s0", "s1", "extra"] {
            std::fs::create_dir_all(store.dir.join(d)).unwrap();
            std::fs::write(store.dir.join(d).join("keep"), "x").unwrap();
        }

        store.list(None).unwrap();

        assert!(store.dir.join("s0").exists(), "surviving entry must keep it");
        assert!(!store.dir.join("s1").exists(), "orphan must be swept");
        assert!(
            store.dir.join("extra").join("keep").exists(),
            "extra/ is backup_extra's, not an orphan"
        );
    }

    #[test]
    fn concurrent_snapshots_do_not_leave_trailing_characters() {
        let (_d, store, root) = store();
        let store = std::sync::Arc::new(store);
        let root = std::sync::Arc::new(root);
        std::thread::scope(|scope| {
            for i in 0..16 {
                let store = std::sync::Arc::clone(&store);
                let root = std::sync::Arc::clone(&root);
                scope.spawn(move || {
                    store
                        .snapshot(root.as_ref(), CliTarget::ClaudeCode, &format!("c{i}"), "test")
                        .unwrap();
                });
            }
        });
        let raw = std::fs::read_to_string(store.manifest_path()).unwrap();
        serde_json::from_str::<Manifest>(&raw).expect("manifest must stay valid JSON");
        assert!(store.list(Some(CliTarget::ClaudeCode)).unwrap().len() <= MAX_SNAPSHOTS_PER_TARGET);
    }
}

#[cfg(test)]
mod root_scope_tests {
    use super::*;

    fn store(dir: &std::path::Path) -> BackupStore {
        BackupStore::new(dir.join("backups"))
    }

    /// 沙箱和真实 home 共用一个 store。沙箱里那份「原始」快照的条目全是
    /// existed:false（沙箱当时是空的）—— 拿它恢复真实 home 就是逐个 remove_file：
    /// ~/.claude/settings.json 和 ~/.codex/auth.json（ChatGPT 的 OAuth）当场消失。
    /// 而 sandbox_cli_writes 默认开着，这是**默认路径**，不是边角情况。
    #[test]
    fn a_sandbox_snapshot_cannot_be_restored_over_the_real_root() {
        let dir = tempfile::tempdir().unwrap();
        let st = store(dir.path());
        let sandbox = ConfigRoot::sandbox(dir.path().join("sandbox"));
        let realish = ConfigRoot::sandbox(dir.path().join("home"));

        // 真实根下有一份用户的配置。
        let real_file = realish.join(".claude/settings.json");
        std::fs::create_dir_all(real_file.parent().unwrap()).unwrap();
        std::fs::write(&real_file, "{\"mine\":true}").unwrap();

        // 沙箱是空的 → 快照里全是 existed:false。
        let snap = st
            .snapshot(&sandbox, CliTarget::ClaudeCode, "s1", "takeover")
            .unwrap();
        assert!(snap.pristine);
        assert!(snap.files.iter().all(|f| !f.existed));

        // 拿它去恢复真实根：必须被拒，文件必须还在。
        let err = st.restore(&realish, "s1").unwrap_err();
        assert!(
            format!("{err}").contains("另一个配置根"),
            "跨根恢复没有被挡住：{err}"
        );
        assert!(real_file.exists(), "真实配置被跨根恢复删掉了");
    }

    /// 列表也要按根过滤 —— 界面上根本不该出现另一个根的快照。
    #[test]
    fn list_in_only_shows_the_current_roots_snapshots() {
        let dir = tempfile::tempdir().unwrap();
        let st = store(dir.path());
        let a = ConfigRoot::sandbox(dir.path().join("a"));
        let b = ConfigRoot::sandbox(dir.path().join("b"));
        st.snapshot(&a, CliTarget::Codex, "a1", "takeover").unwrap();
        st.snapshot(&b, CliTarget::Codex, "b1", "takeover").unwrap();

        let in_a = st.list_in(&a, Some(CliTarget::Codex)).unwrap();
        assert_eq!(in_a.len(), 1);
        assert_eq!(in_a[0].id, "a1");
    }

    /// pristine 按根各算一份：在沙箱试过之后，真实根的首次接管仍然是「原始」，
    /// 否则用户真正的原始配置会在 5 次写入后被轮换掉。
    #[test]
    fn each_root_gets_its_own_pristine() {
        let dir = tempfile::tempdir().unwrap();
        let st = store(dir.path());
        let sandbox = ConfigRoot::sandbox(dir.path().join("sandbox"));
        let realish = ConfigRoot::sandbox(dir.path().join("home"));

        let s = st.snapshot(&sandbox, CliTarget::GrokBuild, "s1", "takeover").unwrap();
        let r = st.snapshot(&realish, CliTarget::GrokBuild, "r1", "takeover").unwrap();
        assert!(s.pristine, "沙箱的首份是它自己那个根的原始状态");
        assert!(r.pristine, "真实根的首次接管必须也算原始 —— 否则会被轮换掉");
    }

    /// 恢复之前先给现状留一份，点错了还有后路。
    #[test]
    fn restore_snapshots_the_current_state_first() {
        let dir = tempfile::tempdir().unwrap();
        let st = store(dir.path());
        let root = ConfigRoot::sandbox(dir.path().join("home"));
        let f = root.join(".claude/settings.json");
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();

        std::fs::write(&f, "{\"v\":1}").unwrap();
        st.snapshot(&root, CliTarget::ClaudeCode, "v1", "takeover").unwrap();
        std::fs::write(&f, "{\"v\":2}").unwrap();

        st.restore(&root, "v1").unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "{\"v\":1}");

        let pre = st
            .list_in(&root, Some(CliTarget::ClaudeCode))
            .unwrap()
            .into_iter()
            .find(|e| e.reason == "pre-restore")
            .expect("恢复前没有留下后路快照");
        // 那份后路里存的必须是恢复**之前**的内容。
        let stored = st.dir().join(&pre.id).join("__claude__settings.json".replace("__claude", ".claude"));
        let body = std::fs::read_to_string(stored).unwrap();
        assert_eq!(body, "{\"v\":2}", "后路快照存的不是恢复前的现状");
    }
}
