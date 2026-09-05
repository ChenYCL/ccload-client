//! 强制路由命令。本地落盘，apply 时通过共享的 `channel_writer::patch_channel`
//! 把「别名 → 上游模型」写进每个目标渠道并按序设优先级 —— 和模型链、调度图走
//! 同一条经过验证的写渠道路径，不重抄那段处理 OAuth / 保留 key 的坑。

use tauri::State;

use crate::error::{AppError, AppResult};
use crate::services::channel_writer::patch_channel;
use crate::services::forced_route::{
    validate_route, winning_priorities, ForcedRoute, ForcedRouteStore,
};
use crate::state::AppState;

pub(crate) fn store_path(state: &AppState) -> std::path::PathBuf {
    // 和 settings.json / fallback.json 同目录，清空 ~/.ccload-client 会一起带走。
    state.config_dir().join("forced_route.json")
}

#[tauri::command]
pub async fn forced_route_list(state: State<'_, AppState>) -> AppResult<Vec<ForcedRoute>> {
    Ok(ForcedRouteStore::load(&store_path(&state))?.routes)
}

#[tauri::command]
pub async fn forced_route_save(
    state: State<'_, AppState>,
    route: ForcedRoute,
) -> AppResult<Vec<ForcedRoute>> {
    validate_route(&route)?;
    let path = store_path(&state);
    let mut store = ForcedRouteStore::load(&path)?;
    store.upsert(route);
    store.save(&path)?;
    Ok(store.routes)
}

#[tauri::command]
pub async fn forced_route_delete(
    state: State<'_, AppState>,
    from: String,
) -> AppResult<Vec<ForcedRoute>> {
    let path = store_path(&state);
    let mut store = ForcedRouteStore::load(&path)?;
    store.remove(&from);
    store.save(&path)?;
    Ok(store.routes)
}

/// 应用一条路由：对每个目标，把 `from` 别名以**压过在位渠道**的优先级写进它绑定
/// 的渠道，redirect 到该目标的上游模型。没绑渠道的目标跳过并记日志 —— 我们从不
/// 替用户凭空造渠道或猜凭证。
///
/// 优先级不能一律写 100：内核对同优先级渠道做加权轮询，如果已经有渠道在 100 上
/// 服务这个别名（Anthropic 就在 100 上服务 claude-fable-5），钉 100 只是和它平分。
/// 所以先问一次 `GET /admin/channels`，找出**其它**服务该别名的启用渠道的最高优先
/// 级，再把目标排到它上面 —— 这才是「强制」而不是「五五开」。
#[tauri::command]
pub async fn forced_route_apply(
    state: State<'_, AppState>,
    from: String,
) -> AppResult<Vec<String>> {
    let store = ForcedRouteStore::load(&store_path(&state))?;
    let route = store
        .routes
        .iter()
        .find(|r| r.from == from)
        .ok_or_else(|| AppError::Config(format!("没有叫 {from} 的强制路由")))?
        .clone();
    validate_route(&route)?;

    // 本路由自己的目标渠道要从「在位者」里排除 —— 它们 apply 完当然会服务这个别名，
    // 拿它们当竞争对手会把优先级越推越高。
    let target_ids: std::collections::HashSet<i64> =
        route.targets.iter().filter_map(|t| t.channel_id).collect();
    let incumbent_max = incumbent_priority_for(&state, &route.from, &target_ids).await?;

    let priorities = winning_priorities(incumbent_max, route.targets.len());
    let mut log = Vec::new();
    if let Some(p) = incumbent_max {
        log.push(format!(
            "已有启用渠道在优先级 {p} 上服务「{}」，把目标排到它上面才是独占（否则同优先级会被平分）",
            route.from
        ));
    }
    for (i, tgt) in route.targets.iter().enumerate() {
        let Some(id) = tgt.channel_id else {
            log.push(format!("目标 {}（{}）：跳过 —— 没绑渠道", i + 1, tgt.model));
            continue;
        };
        let priority = priorities[i];
        let patch = patch_channel(
            &state,
            id,
            Some(priority),
            &[(route.from.clone(), tgt.model.clone())],
        )
        .await
        .map_err(|e| AppError::Config(format!("目标 {}（渠道 {id}）：{e}", i + 1)))?;
        // 优先级是**渠道级**属性，改它会影响该渠道服务的所有模型 —— 把原值一并记进
        // 日志，用户看得见自己动了什么，而不是应用完了才从别处发现。
        log.push(format!(
            "目标 {}：渠道 {} (#{id}) priority={}→{priority}（影响该渠道全部模型） {} → {}",
            i + 1,
            patch.channel_name,
            patch
                .old_priority
                .map(|p| p.to_string())
                .unwrap_or_else(|| "?".into()),
            route.from,
            tgt.model
        ));
    }
    // 落点变了，窗口就变了：给别名多钉一个 500k 的目标，发这个别名的 CLI 就得按
    // 500k 写，否则分流到那儿的那一刻直接 400 too long。
    log.extend(crate::commands::cli::resync_windows(&state).await);
    Ok(log)
}

/// 找出**其它**启用渠道里，也把 `alias` 当作对外模型名（`models[].model`）提供的
/// 那些，取它们的最高优先级。`exclude` 是本路由自己的目标渠道，不算竞争对手。
///
/// 读的是 `models[].model`（对外别名，CLI 按它选渠道），不是 `redirect_model`
/// —— 抢同一个请求的正是「对外也叫这个名字」的渠道。
async fn incumbent_priority_for(
    state: &AppState,
    alias: &str,
    exclude: &std::collections::HashSet<i64>,
) -> Result<Option<i32>, AppError> {
    let (base_url, password) = {
        let s = state.settings.read().await;
        (s.kernel.base_url(), s.kernel.admin_password.clone())
    };
    let resp = state
        .admin
        .request(&base_url, &password, "GET", "channels", None, None)
        .await
        .map_err(|e| AppError::Config(format!("读渠道清单失败：{e}")))?;
    let channels = resp
        .get("data")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut max: Option<i32> = None;
    for ch in &channels {
        let enabled = ch.get("enabled").and_then(serde_json::Value::as_bool).unwrap_or(true);
        if !enabled {
            continue;
        }
        let id = ch.get("id").and_then(serde_json::Value::as_i64);
        if let Some(id) = id {
            if exclude.contains(&id) {
                continue;
            }
        }
        let serves = ch
            .get("models")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|ms| {
                ms.iter().any(|m| {
                    // 停用的模型条目内核当它不存在，不参与竞争。
                    let disabled = m.get("disabled").and_then(serde_json::Value::as_bool).unwrap_or(false);
                    !disabled
                        && m.get("model").and_then(serde_json::Value::as_str) == Some(alias)
                })
            });
        if !serves {
            continue;
        }
        let prio = ch
            .get("priority")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0) as i32;
        max = Some(max.map_or(prio, |m| m.max(prio)));
    }
    Ok(max)
}
