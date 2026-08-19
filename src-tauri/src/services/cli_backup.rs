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

use serde::{Deserialize, Serialize};

use crate::error::AppError;
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
    pub files: Vec<BackupFile>,
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
}

impl BackupStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
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
        serde_json::from_str(&raw)
            .map_err(|e| AppError::Config(format!("backup manifest is corrupt: {e}")))
    }

    fn save(&self, m: &Manifest) -> Result<(), AppError> {
        std::fs::create_dir_all(&self.dir)?;
        let body =
            serde_json::to_string_pretty(m).map_err(|e| AppError::Config(e.to_string()))?;
        let tmp = self.manifest_path().with_extension("json.tmp");
        std::fs::write(&tmp, body)?;
        std::fs::rename(&tmp, self.manifest_path())?;
        Ok(())
    }

    pub fn list(&self, target: Option<CliTarget>) -> Result<Vec<BackupEntry>, AppError> {
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
        let mut manifest = self.load()?;
        // The first snapshot of a target captures the user's original setup.
        let pristine = !manifest.entries.iter().any(|e| e.target == target);

        let snap_dir = self.dir.join(id);
        std::fs::create_dir_all(&snap_dir)?;

        let mut files = Vec::new();
        for rel in target.relative_paths() {
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
        let manifest = self.load()?;
        let entry = manifest
            .entries
            .iter()
            .find(|e| e.id == id)
            .ok_or_else(|| AppError::Config(format!("no backup with id {id}")))?;

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
                    let tmp = dest.with_extension("ccload-restore-tmp");
                    std::fs::copy(&src, &tmp)?;
                    std::fs::rename(&tmp, &dest)?;
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
        Ok(touched)
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
}
