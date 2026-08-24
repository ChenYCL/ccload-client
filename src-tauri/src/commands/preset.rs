//! 会话预设命令。实现见 `services::session_preset`。

use std::path::PathBuf;

use tauri::State;

use crate::error::{AppError, AppResult};
use crate::services::cli_extensions::ALL_TARGETS;
use crate::services::cli_types::CliTarget;
use crate::services::session_preset::{
    find_preset, merged_list, uuid_for_new, PresetStore, SessionPreset, SpawnResult,
};
use crate::state::AppState;
fn store_path(state: &AppState) -> PathBuf {
    state.config_dir().join("presets.json")
}

#[tauri::command]
pub async fn preset_list(state: State<'_, AppState>) -> AppResult<Vec<SessionPreset>> {
    let path = store_path(&state);
    let store = PresetStore::load(&path)?;
    Ok(merged_list(&store))
}

#[derive(serde::Serialize)]
pub struct PresetPrefs {
    pub last_cwd: String,
    pub last_targets: Vec<CliTarget>,
    pub hide_builtins: bool,
}

#[tauri::command]
pub async fn preset_prefs(state: State<'_, AppState>) -> AppResult<PresetPrefs> {
    let store = PresetStore::load(&store_path(&state))?;
    Ok(PresetPrefs {
        last_cwd: store.last_cwd,
        last_targets: store.last_targets,
        hide_builtins: store.hide_builtins,
    })
}

/// 藏 / 显内置预设。视图偏好，不动二进制里的内置，也不碰用户预设。
/// 返回更新后的列表，前端直接替换缓存。
#[tauri::command]
pub async fn preset_set_hide_builtins(
    state: State<'_, AppState>,
    hide: bool,
) -> AppResult<Vec<SessionPreset>> {
    let path = store_path(&state);
    let mut store = PresetStore::load(&path)?;
    store.hide_builtins = hide;
    store.save(&path)?;
    Ok(merged_list(&store))
}

#[tauri::command]
pub async fn preset_save(
    state: State<'_, AppState>,
    mut preset: SessionPreset,
) -> AppResult<Vec<SessionPreset>> {
    if preset.id.trim().is_empty() {
        preset.id = uuid_for_new();
    }
    preset.builtin = false;
    let path = store_path(&state);
    let mut store = PresetStore::load(&path)?;
    store.upsert(preset)?;
    store.save(&path)?;
    Ok(merged_list(&store))
}

#[tauri::command]
pub async fn preset_delete(
    state: State<'_, AppState>,
    id: String,
) -> AppResult<Vec<SessionPreset>> {
    let path = store_path(&state);
    let mut store = PresetStore::load(&path)?;
    store.remove(&id)?;
    store.save(&path)?;
    Ok(merged_list(&store))
}

#[tauri::command]
pub async fn preset_spawn(
    state: State<'_, AppState>,
    id: String,
    cwd: String,
    extra_user: Option<String>,
    launch: bool,
    confine: Option<bool>,
    targets: Option<Vec<CliTarget>>,
) -> AppResult<SpawnResult> {
    if cwd.trim().is_empty() {
        return Err(AppError::Config("先选一个工作目录。".into()).into());
    }
    let path = store_path(&state);
    let mut store = PresetStore::load(&path)?;
    let preset =
        find_preset(&store, &id).ok_or_else(|| AppError::Config(format!("没有叫 {id} 的预设")))?;
    let cwd_path = PathBuf::from(&cwd);
    let picked: Vec<CliTarget> = match targets {
        Some(t) if !t.is_empty() => t,
        _ => ALL_TARGETS.to_vec(),
    };
    let picked_save = picked.clone();
    // 缺省锁定。老前端不传这个字段，而「不锁」才是危险的那一档 —— 默认值必须
    // 是安全的那个，不能靠调用方记得传。
    let confine = confine.unwrap_or(true);
    // 走 config_root() 而不是 dirs::home_dir()：设置页那个「走沙箱，不改真实
    // CLI 配置」的开关得对这一页也算数。以前这里直接读 $HOME，于是用户以为
    // 自己在沙箱里试破禁，会话却写进了真的 ~/.claude。
    let root = state.config_root().await?;
    let result = tokio::task::spawn_blocking(move || {
        crate::services::session_preset::spawn_in(
            root,
            &preset,
            &cwd_path,
            extra_user.as_deref(),
            launch,
            confine,
            &picked,
        )
    })
    .await
    .map_err(|e| AppError::Config(format!("写出会话失败：{e}")))??;
    store.last_cwd = result.cwd.clone();
    store.last_targets = picked_save;
    store.save(&path)?;
    Ok(result)
}
