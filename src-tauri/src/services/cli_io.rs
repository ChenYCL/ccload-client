//! Atomic writes and JSON helpers used by every CLI writer.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Map, Value};

use crate::error::AppError;

/// 新建文件时的权限。这些文件里躺着 OAuth 凭据、MCP 的 bearer token、管理密码，
/// 只应该属主可读。
#[cfg(unix)]
const PRIVATE_MODE: u32 = 0o600;

/// 每个 writer 一个独一无二的临时文件名。
///
/// 以前是 `.{filename}.ccload-tmp`，完全由目标路径推出来 —— 两个并发 writer 拿到
/// 的是**同一个**文件，各自持有独立的写偏移：短的那份把长的那份前半段盖掉，长的
/// 那份的尾巴留在后面，rename 过去就成了两个文档首尾相接。备份清单那个
/// `trailing characters at line N` 就是这么来的。
///
/// pid 挡跨进程（两个客户端实例），计数器挡同进程内。
fn unique_tmp(path: &Path, parent: &Path) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("config");
    parent.join(format!(
        ".{name}.{}.{}.ccload-tmp",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ))
}

/// 临时文件的清道夫。
///
/// 名字唯一之后，任何一条提前返回的路径漏掉 remove，都会在用户的配置目录里留下
/// 一个永远没人清理的 `.xxx.ccload-tmp`（以前名字固定，下一次写入会顺手覆盖掉，
/// 漏了也看不出来）。交给 Drop 就不用每个出错分支各自记得删。
///
/// rename 成功之后 tmp 已经不在了，那次 remove 是个无害的空操作。
struct TmpFile(PathBuf);

impl Drop for TmpFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Sibling temp file + rename, so readers never see a truncated document.
///
/// 关键细节：rename 换掉的是 inode，所以**目标文件原来的权限不会跟过来** ——
/// 临时文件是按 umask 建的（通常 0644）。`~/.claude.json` 原本是 0600，被我们
/// 写过一次之后就变成了同机器上任何用户可读，里面有 oauthAccount 和一堆 MCP 的
/// Authorization 头。所以这里必须显式把权限贴回去：目标存在就沿用它的，
/// 不存在就按 0600 建。
pub fn write_atomic(path: &Path, contents: &str) -> Result<(), AppError> {
    // 先把符号链接解开，对**真身**做换 inode。
    //
    // 用 dotfile 管理器（chezmoi / stow / 手工 ln -s）的人，`~/.claude/settings.json`
    // 常常是一条指向 git 仓库里那份的链接。直接 rename 到 `path` 会把**链接本身**
    // 替换成普通文件：用户的仓库从此再也收不到改动，而他以为一切还被管理着 ——
    // 这种「安静地脱管」比报错难发现得多。链接断了（指向不存在的路径）时
    // canonicalize 会失败，那就按原路径处理，等价于新建。
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let parent = target
        .parent()
        .ok_or_else(|| AppError::Config(format!("{} has no parent dir", target.display())))?;
    std::fs::create_dir_all(parent)?;
    let tmp = TmpFile(unique_tmp(&target, parent));
    std::fs::write(&tmp.0, contents)?;
    // 权限贴不回去就别把文件换过去：宁可这次写入失败，也不要让一个带凭据的
    // 文件以比原来宽松的权限落地。失败时的清理由 TmpFile 的 Drop 负责。
    carry_permissions(&target, &tmp.0)?;
    std::fs::rename(&tmp.0, &target)?;
    Ok(())
}

/// 同样的换 inode 语义，但内容是从另一个文件拷过来的（快照回滚用）。
///
/// **不**调 `carry_permissions`：`fs::copy` 会把 src 的权限一起带过来，而 src 是
/// 快照里那份拷贝，它的权限又是当初从用户原文件拷来的 —— 那正是「按快照原样放
/// 回去」要的东西。贴 dest 现在的权限反而会把我们自己写过的 mode 固化下来。
pub fn copy_atomic(src: &Path, dest: &Path) -> Result<(), AppError> {
    let parent = dest
        .parent()
        .ok_or_else(|| AppError::Config(format!("{} has no parent dir", dest.display())))?;
    std::fs::create_dir_all(parent)?;
    let tmp = TmpFile(unique_tmp(dest, parent));
    std::fs::copy(src, &tmp.0)?;
    std::fs::rename(&tmp.0, dest)?;
    Ok(())
}

#[cfg(unix)]
fn carry_permissions(target: &Path, tmp: &Path) -> Result<(), AppError> {
    use std::os::unix::fs::PermissionsExt;
    let mode = match std::fs::metadata(target) {
        // **收紧而不是原样沿用**。这些文件里有我们写进去的 ccload 凭据，而多数
        // 是 CLI 自己按 umask 建的 0644 —— 实测本机 ~/.config/opencode/opencode.json
        // 和 ~/.grok/config.toml 都是 0644 且各含 3~4 处 api key，同机任何用户
        // 可读。AGENTS.md 那条「保留原权限」写在我们往里塞凭据之前；保留用户
        // 自己加的位（比如给某个组开的读权限做不到，但至少不倒退），去掉
        // group/other 的一切权限。
        Ok(m) => (m.permissions().mode() & 0o7777) & !0o077,
        // 不存在（首次写入）或读不到元数据，都按私有建。
        Err(_) => PRIVATE_MODE,
    };
    std::fs::set_permissions(tmp, std::fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(not(unix))]
fn carry_permissions(target: &Path, tmp: &Path) -> Result<(), AppError> {
    // Windows 上没有 mode 位可搬；只保留只读标志，避免把只读文件写成可写。
    if let Ok(m) = std::fs::metadata(target) {
        std::fs::set_permissions(tmp, m.permissions())?;
    }
    Ok(())
}

pub fn read_json(path: &Path) -> Result<Value, AppError> {
    if !path.exists() {
        return Ok(Value::Object(Map::new()));
    }
    let raw = std::fs::read_to_string(path)?;
    if raw.trim().is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    serde_json::from_str(&raw)
        .map_err(|e| AppError::Config(format!("{} is not valid JSON: {e}", path.display())))
}

/// 取出（必要时创建）`obj[key]` 这个对象。
///
/// 根必须是对象。serde_json 的 `IndexMut` 只会把 `null` 升级成对象，遇到数组 /
/// 字符串 / 数字会直接 **panic**；而配置文件编辑器只校验「能不能解析成 JSON」，
/// 用户完全可以把 settings.json 存成 `[]`。Tauri 的异步命令跑在 detached 的
/// tokio 任务里，panic 之后连响应都发不回来，前端会永远停在「写入中…」。
pub fn object_at<'a>(obj: &'a mut Value, key: &str) -> Result<&'a mut Map<String, Value>, AppError> {
    let root = obj
        .as_object_mut()
        .ok_or_else(|| AppError::Config("配置文件的顶层不是 JSON 对象，拒绝写入".into()))?;
    let slot = root.entry(key.to_string()).or_insert_with(|| Value::Object(Map::new()));
    if !slot.is_object() {
        *slot = Value::Object(Map::new());
    }
    Ok(slot.as_object_mut().expect("just ensured object"))
}

pub fn write_pretty_json(path: &Path, doc: &Value) -> Result<(), AppError> {
    let body = serde_json::to_string_pretty(doc).map_err(|e| AppError::Config(e.to_string()))?;
    write_atomic(path, &format!("{body}\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `~/.claude.json` 是 0600 的，被我们写过一次后变成了 0644 —— 同机器上任何
    /// 用户都能读到里面的 OAuth 凭据。rename 换 inode，权限必须显式搬过去。
    #[cfg(unix)]
    #[test]
    fn atomic_write_keeps_the_target_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("secrets.json");
        std::fs::write(&path, "{}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();

        write_atomic(&path, "{\"a\":1}").unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "0600 file must not come back world-readable");
    }

    /// CLI 自己按 umask 建出来的 0644 文件，被我们塞进凭据之后必须收紧。
    ///
    /// 实测本机 ~/.config/opencode/opencode.json 和 ~/.grok/config.toml 都是
    /// 0644 且各含 3~4 处 api key —— 同机任何用户直接可读。
    #[cfg(unix)]
    #[test]
    fn a_world_readable_config_is_tightened_when_we_write_credentials() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("opencode.json");
        std::fs::write(&path, "{}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        write_atomic(&path, "{\"apiKey\":\"secret\"}").unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "写进凭据之后还留着 group/other 的读权限");
    }

    /// 但用户自己给 owner 加的执行位之类不该被抹掉 —— 只去 group/other。
    #[cfg(unix)]
    #[test]
    fn tightening_keeps_the_owner_bits() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("hook.json");
        std::fs::write(&path, "{}").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();

        write_atomic(&path, "{\"a\":1}").unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "owner 的位要留着，只去掉 group/other");
    }

    /// 新建的配置文件同样带凭据，不能按 umask 落成 0644。
    #[cfg(unix)]
    #[test]
    fn a_brand_new_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("new.json");
        write_atomic(&path, "{}").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    /// 顶层是数组时以前会 panic，命令挂死、前端永远转圈。
    #[test]
    fn a_non_object_root_is_an_error_not_a_panic() {
        let mut doc: Value = serde_json::from_str("[]").unwrap();
        let err = object_at(&mut doc, "mcpServers").unwrap_err();
        assert!(err.to_string().contains("顶层不是 JSON 对象"), "{err}");
    }

    /// 顶层是对象、但该 key 上是别的类型时，覆盖成对象是既有行为，保持不变。
    #[test]
    fn a_non_object_value_at_the_key_is_replaced() {
        let mut doc: Value = serde_json::from_str(r#"{"mcpServers": 3}"#).unwrap();
        object_at(&mut doc, "mcpServers").unwrap().insert("a".into(), Value::Bool(true));
        assert_eq!(doc.pointer("/mcpServers/a").unwrap(), &Value::Bool(true));
    }

    fn leftover_tmps(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".ccload-tmp"))
            .collect()
    }

    /// 共享的临时文件名是备份清单 `trailing characters at line N` 的根因：两个
    /// writer 落在同一个 tmp 上，各自持有独立的写偏移，短的那份盖掉长的那份前半
    /// 段、长的那份的尾巴留在后面，rename 过去就是两个文档首尾相接。
    ///
    /// 长度差得越大越容易撞出来，所以两种内容一长一短。
    #[test]
    fn concurrent_writes_to_one_path_never_concatenate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let long = format!(r#"{{"who":"long","pad":"{}"}}"#, "x".repeat(20_000));
        let short = r#"{"who":"short"}"#.to_string();

        std::thread::scope(|scope| {
            for i in 0..32 {
                let path = &path;
                let body = if i % 2 == 0 { &long } else { &short };
                scope.spawn(move || write_atomic(path, body).unwrap());
            }
        });

        // 每一次 rename 都是原子的，所以落地的必须是**某一次**完整的写入，
        // 不能是两次写入拼起来的东西。
        let raw = std::fs::read_to_string(&path).unwrap();
        let v: Value = serde_json::from_str(&raw).expect("must be one whole document");
        assert!(matches!(v["who"].as_str(), Some("long" | "short")), "{raw}");
        assert!(leftover_tmps(dir.path()).is_empty(), "tmp files leaked");
    }

    /// 名字唯一之后就没有「下一次写入顺手覆盖掉」这回事了 —— 出错路径漏删一次，
    /// 用户的配置目录里就永久多一个 `.xxx.ccload-tmp`。
    #[test]
    fn a_failed_write_leaves_no_tmp_behind() {
        let dir = tempfile::tempdir().unwrap();
        // rename 一个普通文件到已存在的目录上必然失败。
        let path = dir.path().join("occupied");
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("child"), "x").unwrap();

        assert!(write_atomic(&path, "{}").is_err());
        assert!(leftover_tmps(dir.path()).is_empty(), "tmp survived the failure");
    }

    /// 回滚要把文件按快照原样放回去，包括权限 —— 那是用户原本的 mode，不是我们
    /// 写过之后的 mode。
    #[cfg(unix)]
    #[test]
    fn copy_atomic_carries_the_snapshots_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("snapshot");
        std::fs::write(&src, "restored").unwrap();
        std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o640)).unwrap();
        let dest = dir.path().join("nested").join("config.json");
        std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
        std::fs::write(&dest, "ours").unwrap();
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o600)).unwrap();

        copy_atomic(&src, &dest).unwrap();

        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "restored");
        let mode = std::fs::metadata(&dest).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o640, "snapshot's mode wins, not the one we had written");
        assert!(leftover_tmps(dest.parent().unwrap()).is_empty());
    }
}

#[cfg(test)]
mod symlink_tests {
    use super::*;

    /// 用 dotfile 管理器（chezmoi / stow / 自己 ln -s）的人，`~/.claude/settings.json`
    /// 常常是一条指向 git 仓库里那份的符号链接。`rename` 换 inode 会把**链接本身**
    /// 替换成普通文件：用户的仓库从此收不到改动，而他以为还在被管理。
    ///
    /// 这条测试钉住我们的选择：写穿到链接指向的真实文件，链接保持是链接。
    #[cfg(unix)]
    #[test]
    fn writing_through_a_symlink_keeps_the_link() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.json");
        let link = dir.path().join("settings.json");
        std::fs::write(&real, "{\"a\":1}").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        write_atomic(&link, "{\"a\":2}").unwrap();

        assert!(
            std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink(),
            "符号链接被换成了普通文件 —— 用户的 dotfile 仓库从此收不到改动"
        );
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "{\"a\":2}", "改动没写到链接指向的真文件上");
    }
}
