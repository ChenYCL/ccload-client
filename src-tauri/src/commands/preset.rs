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
}

#[tauri::command]
pub async fn preset_prefs(state: State<'_, AppState>) -> AppResult<PresetPrefs> {
    let store = PresetStore::load(&store_path(&state))?;
    Ok(PresetPrefs {
        last_cwd: store.last_cwd,
        last_targets: store.last_targets,
    })
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
    let result = tokio::task::spawn_blocking(move || {
        crate::services::session_preset::spawn_at_home(
            &preset,
            &cwd_path,
            extra_user.as_deref(),
            launch,
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
