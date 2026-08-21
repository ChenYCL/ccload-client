//! 渠道自报用量命令。契约与理由见 `services::channel_usage`。

use tauri::State;

use crate::error::AppResult;
use crate::services::channel_usage::{self, SelfReportedUsage};
use crate::state::AppState;

/// 逐个渠道问它的上游有没有 `/usage`。
///
/// 逐个独立成败：绝大多数渠道不实现这个契约（返回 404），那是**正常状态**，
/// 不该让一次探测整体失败；真出错的那几个把原因带回去。
#[tauri::command]
pub async fn channel_usage_probe(
    state: State<'_, AppState>,
    channel_ids: Vec<i64>,
) -> AppResult<ProbeReport> {
    let mut found = Vec::new();
    let mut errors = Vec::new();
    for id in channel_ids {
        match channel_usage::probe(&state, id).await {
            Ok(Some(u)) => found.push(u),
            Ok(None) => {}
            Err(e) => errors.push(e.to_string()),
        }
    }
    Ok(ProbeReport { found, errors })
}

#[derive(Debug, serde::Serialize)]
pub struct ProbeReport {
    pub found: Vec<SelfReportedUsage>,
    /// 只放**真的出错**的；「上游没实现」不进这里。
    pub errors: Vec<String>,
}
