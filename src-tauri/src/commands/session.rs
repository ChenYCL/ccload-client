//! 会话救援命令。病因、格式与两种救法的取舍见 `services::session_rescue`。

use tauri::State;

use crate::error::{AppError, AppResult};
use crate::services::session_rescue::{self, CompactReport, SessionInfo, SlimReport};
use crate::state::AppState;

/// 扫出本机所有 Claude Code 会话。
///
/// 阻塞 IO（可能要读几十 MB），所以扔到 blocking 线程池 —— 直接在 async 里
/// 跑会把 Tauri 的运行时卡住，界面表现为整个窗口假死。
#[tauri::command]
pub async fn session_list() -> AppResult<Vec<SessionInfo>> {
    Ok(tokio::task::spawn_blocking(session_rescue::list_sessions)
        .await
        .map_err(|e| AppError::Config(format!("扫描会话失败：{e}")))??)
}

/// 瘦身：砍图 + 截长工具结果，直到预计上下文降到 `target` 以下。
///
/// 纯本地，不花 token，秒级。代价是信息真的丢了 —— 要保信息用
/// [`session_compact`]。
#[tauri::command]
pub async fn session_slim(
    path: String,
    target: u64,
    text_limit: usize,
) -> AppResult<SlimReport> {
    Ok(
        tokio::task::spawn_blocking(move || session_rescue::slim(&path, target, text_limit))
            .await
            .map_err(|e| AppError::Config(format!("瘦身失败：{e}")))??,
    )
}

/// 分块总结：把活动链切成小段各自总结，再追加原生的压缩边界 + 摘要。
///
/// 模型走客户端令牌打内核的 `/v1/messages`，所以路由和故障转移都归内核管。
/// 用哪个模型由调用方给 —— 这一页知道哪个渠道现在是好的，比这里猜准。
#[tauri::command]
pub async fn session_compact(
    state: State<'_, AppState>,
    path: String,
    model: String,
    keep_tail: usize,
    chunk_tokens: u64,
) -> AppResult<CompactReport> {
    let (base_url, token) = {
        let s = state.settings.read().await;
        (s.kernel.base_url(), s.client_api_token.clone().unwrap_or_default())
    };
    Ok(
        session_rescue::compact(&base_url, &token, &model, &path, keep_tail, chunk_tokens)
            .await?,
    )
}
