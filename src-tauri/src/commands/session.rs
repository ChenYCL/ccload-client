//! 会话救援命令。病因、格式与两种救法的取舍见 `services::session_rescue`。

use tauri::State;

use crate::error::{AppError, AppResult};
use crate::services::context_window::pick_compact_models;
use crate::services::fallback::FallbackStore;
use crate::services::session_rescue::{
    self, CompactReport, DeleteReport, SessionInfo, SlimReport,
};
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
/// 模型走客户端令牌打内核的 `/v1/messages`。调用方可以指定一个；空着则按模型链
/// 从前往后挑窗口够的那几跳。400 too-long 时换下一个 —— 内核不把这种 400 当
/// 故障转移，压缩这边必须自己认。
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
    // 按**分块大小**挑模型，不是按整段会话 —— 分块总结的每次请求只发一块。
    let candidates = compact_candidates(&state, &model, chunk_tokens)?;
    let mut last_err: Option<AppError> = None;
    let mut tried = Vec::new();
    for m in &candidates {
        tried.push(m.clone());
        match session_rescue::compact(&base_url, &token, m, &path, keep_tail, chunk_tokens)
            .await
        {
            Ok(r) => return Ok(r),
            Err(e) if session_rescue::is_prompt_too_long(&e) => {
                last_err = Some(e);
            }
            Err(e) => return Err(e.into()),
        }
    }
    let detail = last_err
        .map(|e| e.to_string())
        .unwrap_or_else(|| "没有可用的压缩模型".into());
    Err(AppError::Config(format!(
        "分块总结失败（试过 {}）：{detail}",
        tried.join(" → ")
    ))
    .into())
}

/// 用户点的那个在前，链上窗口够的接在后面。点的那个自己也会 400，所以不能只试它。
/// `chunk_tokens` 是**一块**的大小，也就是压缩请求实际会发出去的量。
///
/// 以前这里传的是整段会话的真实上下文（`last_context_of`）—— 那正好把分块总结
/// 的意义抵消掉了：517k 的会话在一条 500k 的链上会得到「没有窗口够的一跳」，
/// 可每一块只有 120k，200k 的模型绰绰有余。分块存在的理由就是整段发不出去。
fn compact_candidates(
    state: &AppState,
    model: &str,
    chunk_tokens: u64,
) -> Result<Vec<String>, AppError> {
    let needed = chunk_tokens;
    let store = FallbackStore::load(&state.config_dir().join("fallback.json"))?;
    let mut out = Vec::new();
    let picked = model.trim();
    if !picked.is_empty() {
        out.push(picked.to_string());
    }
    for chain in &store.chains {
        for m in pick_compact_models(needed, &chain.hops) {
            if !out.iter().any(|s| s == &m) {
                out.push(m);
            }
        }
    }
    if out.is_empty() {
        return Err(AppError::Config(
            "没有指定压缩模型，模型链里也没有窗口够的一跳。先在上面选一个，或先应用一条模型链。".into(),
        ));
    }
    Ok(out)
}

/// 删掉选中的会话。不可恢复，调用方必须先弹确认。
///
/// 一条失败不拖累其余：清一堆旧会话时，不该因为其中一个被占用就整批停住。
/// 活着的会话后端会跳过，不报错。
#[tauri::command]
pub async fn session_delete(paths: Vec<String>) -> AppResult<DeleteReport> {
    Ok(
        tokio::task::spawn_blocking(move || session_rescue::delete_sessions(&paths))
            .await
            .map_err(|e| AppError::Config(format!("删除会话失败：{e}")))??,
    )
}
