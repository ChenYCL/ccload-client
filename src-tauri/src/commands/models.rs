//! Model catalog import + vision MCP registration commands.

use tauri::State;

use crate::error::{AppError, AppResult};
use crate::services::cli_types::CliTarget;
use crate::services::model_import::{apply_import, ImportEntry, ImportResult};
use crate::services::vision_mcp::{set_vision_mcp, VisionConfig};
use crate::state::AppState;
use crate::services::cli_backup::unique_stamp;

/// Write the selected kernel aliases into a CLI's model config. The renderer
/// builds the entry list from the channel list it already has.
#[tauri::command]
pub async fn model_import(
    state: State<'_, AppState>,
    target: CliTarget,
    entries: Vec<ImportEntry>,
) -> AppResult<ImportResult> {
    let root = state.config_root().await?;
    Ok(apply_import(
        &root,
        target,
        &entries,
        &unique_stamp(),
        &state.backups,
    )?)
}

/// Register or unregister the vision-augmentation MCP server for a CLI.
/// `model` is a kernel alias with vision capability, chosen in the UI.
#[tauri::command]
pub async fn vision_mcp_set(
    state: State<'_, AppState>,
    target: CliTarget,
    enabled: bool,
    model: Option<String>,
) -> AppResult<Vec<String>> {
    if !enabled {
        // Disable path never needs a token or model — the kernel may not even
        // be running when the user turns the toggle off.
        let root = state.config_root().await?;
        let cfg = VisionConfig {
            base_url: String::new(),
            token: String::new(),
            model: String::new(),
        };
        return Ok(set_vision_mcp(&root, target, false, &cfg, &unique_stamp(), &state.backups)?);
    }
    let (token, base) = {
        let s = state.settings.read().await;
        (s.client_api_token.clone(), s.kernel.base_url())
    };
    let token = token.ok_or_else(|| {
        AppError::Config("no client API token yet — start the kernel first".into())
    })?;
    let model = model.filter(|m| !m.is_empty()).ok_or_else(|| {
        AppError::Config("请选择一个支持视觉的模型（内核里的多模态模型别名）".into())
    })?;
    let root = state.config_root().await?;
    let cfg = VisionConfig {
        base_url: base,
        token,
        model,
    };
    Ok(set_vision_mcp(&root, target, true, &cfg, &unique_stamp(), &state.backups)?)
}

