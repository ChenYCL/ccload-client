//! Model catalog import + vision / image MCP registration commands.

use tauri::State;

use crate::error::{AppError, AppResult};
use crate::services::cli_extensions::ALL_TARGETS;
use crate::services::cli_types::CliTarget;
use crate::services::image_mcp::{set_image_mcp, image_states, ImageApi, ImageConfig, ImageTargetState};
use crate::services::mcp_usage::{self, McpUsage};
use crate::services::model_import::{apply_import, ImportEntry, ImportResult};
use crate::services::vision_mcp::{set_vision_mcp, vision_states, VisionConfig, VisionTargetState};
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

/// 五个 CLI 上视觉 MCP 的真实状态：装没装、正在用哪个模型看图、里面存的
/// 内核地址/令牌还对不对。
///
/// 模型这一项以前只活在渲染进程里，切走再回来就没了，用户以为「选了没保存上」。
/// 真值一直在各 CLI 的配置文件里，读回来即可。
#[tauri::command]
pub async fn vision_mcp_state(state: State<'_, AppState>) -> AppResult<Vec<VisionTargetState>> {
    let (token, base) = {
        let s = state.settings.read().await;
        (s.client_api_token.clone(), s.kernel.base_url())
    };
    let root = state.config_root().await?;
    Ok(vision_states(&root, &ALL_TARGETS, &base, token.as_deref()))
}

/// 装 / 卸生图 MCP。
///
/// `model` 必须是内核里一个**能出图**的别名；`api` 不传就是 auto —— 按模型挑端点，
/// 挑错了当场换另一条。两条路的差别（尤其「改图只有 chat 能做」）写在
/// `services::image_mcp` 头上。
#[tauri::command]
pub async fn image_mcp_set(
    state: State<'_, AppState>,
    target: CliTarget,
    enabled: bool,
    model: Option<String>,
    api: Option<ImageApi>,
    out_dir: Option<String>,
) -> AppResult<Vec<String>> {
    if !enabled {
        // 关的时候不需要令牌，也不需要内核在跑 —— 和视觉那边同一个理由。
        let root = state.config_root().await?;
        let cfg = ImageConfig {
            base_url: String::new(),
            token: String::new(),
            model: String::new(),
            api: ImageApi::Auto,
            out_dir: String::new(),
        };
        return Ok(set_image_mcp(&root, target, false, &cfg, &unique_stamp(), &state.backups)?);
    }
    let (token, base) = {
        let s = state.settings.read().await;
        (s.client_api_token.clone(), s.kernel.base_url())
    };
    let token = token.ok_or_else(|| {
        AppError::Config("no client API token yet — start the kernel first".into())
    })?;
    let model = model.filter(|m| !m.is_empty()).ok_or_else(|| {
        AppError::Config("请选择一个能生图的模型（内核里的生图模型别名）".into())
    })?;
    let root = state.config_root().await?;
    let cfg = ImageConfig {
        base_url: base,
        token,
        model,
        api: api.unwrap_or(ImageApi::Auto),
        out_dir: out_dir.unwrap_or_default(),
    };
    Ok(set_image_mcp(&root, target, true, &cfg, &unique_stamp(), &state.backups)?)
}

/// 五个 CLI 上生图 MCP 的真实状态。理由同 [`vision_mcp_state`]：真值在磁盘上。
#[tauri::command]
pub async fn image_mcp_state(state: State<'_, AppState>) -> AppResult<Vec<ImageTargetState>> {
    let (token, base) = {
        let s = state.settings.read().await;
        (s.client_api_token.clone(), s.kernel.base_url())
    };
    let root = state.config_root().await?;
    Ok(image_states(&root, &ALL_TARGETS, &base, token.as_deref()))
}

/// 本客户端自带 MCP 工具的调用统计（次数 / 耗时 / 失败）。
///
/// 口径只覆盖客户端自带的两个服务器（`ccload-vision` / `ccload-image`）——
/// 别家 MCP 服务器是独立进程，既不经过内核也不经过我们，客户端没有任何位置
/// 能看见它们的调用。UI 上要把这个边界写出来，别让「MCP 调用统计」被读成
/// 「所有 MCP 的统计」。
#[tauri::command]
pub fn mcp_usage_stats() -> AppResult<McpUsage> {
    Ok(mcp_usage::aggregate())
}

/// 清空调用流水。
#[tauri::command]
pub fn mcp_usage_clear() -> AppResult<()> {
    mcp_usage::clear().map_err(AppError::from)?;
    Ok(())
}

