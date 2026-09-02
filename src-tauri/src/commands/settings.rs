//! Settings commands. Connection mode (managed vs remote) lives here so the
//! renderer can flip it without restarting the process.

use tauri::State;

use crate::error::AppResult;
use crate::services::kernel::{kernel_identity_changed, KernelConfig, KernelMode};
use crate::state::{AppSettings, AppState};

#[tauri::command]
pub async fn settings_get(state: State<'_, AppState>) -> AppResult<AppSettings> {
    Ok(state.settings.read().await.clone())
}

/// Persist a new kernel config and invalidate the admin session. The caller
/// is expected to `kernel_stop` + `kernel_start` afterwards if the mode or
/// port changed.
#[tauri::command]
pub async fn settings_set_kernel(
    state: State<'_, AppState>,
    kernel: KernelConfig,
) -> AppResult<AppSettings> {
    apply_kernel_config(&state, kernel).await?;
    Ok(state.settings.read().await.clone())
}

/// 换内核连接设置的**唯一入口**：校验、归一化、失效令牌、重建 HTTP 客户端、
/// 重定向两个代理。设置页和配置导入都走这里 —— 以前导入自己直接
/// `s.kernel = bundle.kernel`，跳过全部失效步骤：托管→远端切换后旧 token
/// 留着（每个 CLI 都 401）、admin 会话没失效、代理还指着旧地址、
/// outbound_proxy 没校验也没在 Managed 模式清空。
pub(crate) async fn apply_kernel_config(
    state: &AppState,
    kernel: KernelConfig,
) -> Result<(), crate::error::AppError> {
    {
        let mut s = state.settings.write().await;
        // Normalize at the boundary so a pasted " https://host " never reaches
        // storage — it would otherwise be written verbatim into CLI configs.
        let mut kernel = kernel;
        kernel.remote_url = kernel
            .remote_url
            .map(|u| u.trim().trim_end_matches('/').to_string())
            .filter(|u| !u.is_empty());
        kernel.outbound_proxy = kernel
            .outbound_proxy
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        if kernel.mode == KernelMode::Remote {
            crate::services::kernel::parse_outbound_proxy(
                kernel.outbound_proxy.as_deref().unwrap_or(""),
            )?;
        } else {
            kernel.outbound_proxy = None;
        }
        if kernel.mode == KernelMode::Remote && kernel.remote_url.as_deref().unwrap_or("").is_empty()
        {
            return Err(crate::error::AppError::Config(
                "remote_url is required in remote mode".into(),
            ));
        }
        // API tokens are per kernel instance: one minted for the managed
        // kernel means nothing to a remote one (and vice versa), which
        // otherwise surfaces as 401s from every CLI we wrote it into.
        // admin_password alone doesn't change the instance — the token
        // stays valid across a password change.
        if kernel_identity_changed(&s.kernel, &kernel) {
            s.client_api_token = None;
        }
        s.kernel = kernel;
    }
    state.persist().await?;
    state.admin.invalidate().await;
    let cfg = state.settings.read().await.kernel.clone();
    state.kernel.rebuild_http(&cfg).await?;
    let _ = crate::commands::cli_proxy::ensure_cli_proxy(state).await;
    let _ = crate::commands::kernel::ensure_embed_proxy(state).await;
    Ok(())
}

#[tauri::command]
pub async fn settings_set_sandbox(
    state: State<'_, AppState>,
    sandbox: bool,
) -> AppResult<AppSettings> {
    state.settings.write().await.sandbox_cli_writes = sandbox;
    state.persist().await?;
    Ok(state.settings.read().await.clone())
}

#[tauri::command]
pub async fn settings_set_client_token(
    state: State<'_, AppState>,
    token: String,
) -> AppResult<()> {
    state.settings.write().await.client_api_token = Some(token);
    state.persist().await?;
    Ok(())
}
