//! CLI takeover commands. Preview-first, snapshot-always, restorable.

use tauri::State;

use crate::error::{AppError, AppResult};
use crate::services::cli_advanced::{
    known_env_keys, read_files, write_file, ConfigFileView, EnvKeyInfo, TakeoverOptions,
};
use crate::services::cli_backup::BackupEntry;
use crate::services::cli_config::{
    apply_takeover, preview, CliTarget, TakeoverPreview, TakeoverResult,
};
use crate::state::AppState;
use crate::services::cli_backup::unique_stamp;

const TARGETS: [CliTarget; 5] = [
    CliTarget::ClaudeCode,
    CliTarget::Codex,
    CliTarget::GeminiCli,
    CliTarget::GrokBuild,
    CliTarget::OpenCode,
];

#[tauri::command]
pub async fn cli_preview(
    state: State<'_, AppState>,
    target: CliTarget,
) -> AppResult<TakeoverPreview> {
    let root = state.config_root().await?;
    let (base, token) = {
        let s = state.settings.read().await;
        (s.kernel.base_url(), s.client_api_token.clone())
    };
    Ok(preview(&root, target, &base, token.as_deref()))
}

#[tauri::command]
pub async fn cli_preview_all(state: State<'_, AppState>) -> AppResult<Vec<TakeoverPreview>> {
    let root = state.config_root().await?;
    let (base, token) = {
        let s = state.settings.read().await;
        (s.kernel.base_url(), s.client_api_token.clone())
    };
    Ok(TARGETS
        .into_iter()
        .map(|t| preview(&root, t, &base, token.as_deref()))
        .collect())
}

#[tauri::command]
pub async fn cli_apply(
    state: State<'_, AppState>,
    target: CliTarget,
    options: Option<TakeoverOptions>,
) -> AppResult<TakeoverResult> {
    let (token, base) = {
        let s = state.settings.read().await;
        (s.client_api_token.clone(), s.kernel.base_url())
    };
    let token = token.ok_or_else(|| {
        AppError::Config(
            "no client API token yet — start the kernel first so one can be created".into(),
        )
    })?;
    let root = state.config_root().await?;
    Ok(apply_takeover(
        &root,
        target,
        &base,
        &token,
        &unique_stamp(),
        &state.backups,
        options.unwrap_or_default(),
    )?)
}

/// Snapshots for one target, or all targets when `target` is omitted.
#[tauri::command]
pub async fn cli_backups(
    state: State<'_, AppState>,
    target: Option<CliTarget>,
) -> AppResult<Vec<BackupEntry>> {
    Ok(state.backups.list(target)?)
}

/// Roll a target's config files back to a snapshot. Files that did not exist
/// when the snapshot was taken are deleted, not recreated empty.
#[tauri::command]
pub async fn cli_restore(
    state: State<'_, AppState>,
    backup_id: String,
) -> AppResult<Vec<String>> {
    let root = state.config_root().await?;
    Ok(state.backups.restore(&root, &backup_id)?)
}

/// Read every config file for a target as raw text, for the editor UI.
#[tauri::command]
pub async fn cli_read_files(
    state: State<'_, AppState>,
    target: CliTarget,
) -> AppResult<Vec<ConfigFileView>> {
    let root = state.config_root().await?;
    Ok(read_files(&root, target)?)
}

/// Replace one config file with user-edited text. Validates JSON/TOML before
/// touching the file, and snapshots first so the edit is undoable.
#[tauri::command]
pub async fn cli_write_file(
    state: State<'_, AppState>,
    target: CliTarget,
    rel: String,
    body: String,
) -> AppResult<String> {
    let root = state.config_root().await?;
    Ok(write_file(&root, target, &rel, &body, &unique_stamp(), &state.backups)?)
}

/// Metadata for the advanced-settings UI: which knobs exist for a target,
/// our suggested default, and what the machine currently has.
#[tauri::command]
pub async fn cli_env_keys(
    state: State<'_, AppState>,
    target: CliTarget,
) -> AppResult<Vec<EnvKeyInfo>> {
    let root = state.config_root().await?;
    Ok(known_env_keys(&root, target))
}

