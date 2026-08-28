//! CLI 代理的命令面：起停、查最近转发、把一条转发反查成会话。
//!
//! 代理只做转发和旁路记录，不碰会话内容 —— 请求体除了 `model` 字段按映射表
//! 改写，其余字节原样透传，响应逐块回吐。会话文件仍然由 CLI 自己写，这里
//! 只是**读**请求头里的会话 id，所以不存在「代理污染了会话」这回事。

use tauri::State;

use crate::error::AppResult;
use crate::services::cli_proxy::{claude_session_file, session_title, CliProxy, ProxyRecord};
use crate::state::AppState;

/// 起代理，或在内核地址变了之后重新指向。
pub async fn ensure_cli_proxy(state: &AppState) -> Result<(), crate::error::AppError> {
    let base = state.settings.read().await.kernel.base_url();
    let guard = state.cli_proxy.read().await;
    if let Some(proxy) = guard.as_ref() {
        proxy.retarget(&base).await;
    } else {
        drop(guard);
        let proxy = CliProxy::start(&base).await?;
        *state.cli_proxy.write().await = Some(proxy);
    }
    Ok(())
}

/// CLI 该往哪儿指。代理没起来时返回 None —— 此时不该去写任何接管配置，
/// 不然写进去的是个没人监听的地址。
#[tauri::command]
pub async fn cli_proxy_url(state: State<'_, AppState>) -> AppResult<Option<String>> {
    Ok(state
        .cli_proxy
        .read()
        .await
        .as_ref()
        .map(|p| p.base_url()))
}

/// 最近的转发记录，最新的在前。
#[tauri::command]
pub async fn cli_proxy_records(state: State<'_, AppState>) -> AppResult<Vec<ProxyRecord>> {
    let guard = state.cli_proxy.read().await;
    match guard.as_ref() {
        Some(p) => Ok(p.records().await),
        None => Ok(Vec::new()),
    }
}

/// 一条日志点进去要展示的东西：会话在磁盘上的位置和它的标题。
#[derive(Debug, serde::Serialize)]
pub struct SessionRef {
    pub session_id: String,
    /// 会话 jsonl 的绝对路径。找不到就是 None —— Codex 的会话不在
    /// `~/.claude/projects` 下，这条路径只对 Claude Code 有意义。
    pub path: Option<String>,
    /// 标题。优先用 Claude Code 自己生成的 `ai-title`，没有就退回首条用户消息。
    pub title: Option<String>,
}

/// 把一个会话 id 解析成可展示、可跳转的引用。
#[tauri::command]
pub async fn cli_proxy_session(session_id: String) -> AppResult<SessionRef> {
    let found = tokio::task::spawn_blocking(move || {
        let path = claude_session_file(&session_id);
        let title = path.as_deref().and_then(session_title);
        SessionRef {
            session_id,
            path: path.map(|p| p.to_string_lossy().into_owned()),
            title,
        }
    })
    .await
    .map_err(|e| crate::error::AppError::Config(e.to_string()))?;
    Ok(found)
}
