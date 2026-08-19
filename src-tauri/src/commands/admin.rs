//! The one command the renderer uses for every admin-API interaction.
//!
//! Path, method, query and body are forwarded as-is. Adding a new ccLoad
//! endpoint never requires a new Tauri command — only a new frontend hook.

use serde_json::Value;
use tauri::State;

use crate::error::{AppError, AppResult};
use crate::state::AppState;

#[tauri::command]
pub async fn admin_request(
    state: State<'_, AppState>,
    method: String,
    path: String,
    query: Option<String>,
    body: Option<Value>,
) -> AppResult<Value> {
    let (base_url, password) = {
        let s = state.settings.read().await;
        (s.kernel.base_url(), s.kernel.admin_password.clone())
    };
    if base_url.is_empty() {
        return Err(AppError::Config("kernel base_url is empty".into()).into());
    }
    Ok(state
        .admin
        .request(&base_url, &password, &method, &path, query.as_deref(), body)
        .await?)
}

/// Convenience wrapper used by the renderer to confirm the session works
/// without caring about a specific resource. Hits `/admin/settings` because
/// it is cheap and always present.
#[tauri::command]
pub async fn admin_ping(state: State<'_, AppState>) -> AppResult<bool> {
    admin_request(state, "GET".into(), "settings".into(), None, None)
        .await
        .map(|_| true)
}
