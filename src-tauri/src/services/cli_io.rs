//! Atomic writes and JSON helpers used by every CLI writer.

use std::path::Path;

use serde_json::{Map, Value};

use crate::error::AppError;

/// 新建文件时的权限。这些文件里躺着 OAuth 凭据、MCP 的 bearer token、管理密码，
/// 只应该属主可读。
#[cfg(unix)]
const PRIVATE_MODE: u32 = 0o600;

/// Sibling temp file + rename, so readers never see a truncated document.
///
/// 关键细节：rename 换掉的是 inode，所以**目标文件原来的权限不会跟过来** ——
/// 临时文件是按 umask 建的（通常 0644）。`~/.claude.json` 原本是 0600，被我们
/// 写过一次之后就变成了同机器上任何用户可读，里面有 oauthAccount 和一堆 MCP 的
/// Authorization 头。所以这里必须显式把权限贴回去：目标存在就沿用它的，
/// 不存在就按 0600 建。
pub fn write_atomic(path: &Path, contents: &str) -> Result<(), AppError> {
    let parent = path
        .parent()
        .ok_or_else(|| AppError::Config(format!("{} has no parent dir", path.display())))?;
    std::fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(
        ".{}.ccload-tmp",
        path.file_name().and_then(|s| s.to_str()).unwrap_or("config")
    ));
    std::fs::write(&tmp, contents)?;
    if let Err(e) = carry_permissions(path, &tmp) {
        // 权限贴不回去就别把文件换过去：宁可这次写入失败，也不要让一个带凭据的
        // 文件以比原来宽松的权限落地。
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(unix)]
fn carry_permissions(target: &Path, tmp: &Path) -> Result<(), AppError> {
    use std::os::unix::fs::PermissionsExt;
    let mode = match std::fs::metadata(target) {
        Ok(m) => m.permissions().mode() & 0o7777,
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
}
