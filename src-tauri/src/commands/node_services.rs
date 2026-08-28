//! Node 轻量服务的命令面：增删改、起停、看状态。
//!
//! 服务清单存 `~/.ccload-client/node-services.json`，和 settings.json 同目录。
//! 起停是即时的，清单是持久的 —— 两者分开，改一条配置不会顺手重启服务。

use tauri::State;

use crate::error::AppResult;
use crate::services::node_services::{
    load_services, save_services, NodeService, ServiceStatus,
};
use crate::state::AppState;

#[tauri::command]
pub async fn node_service_list(state: State<'_, AppState>) -> AppResult<Vec<NodeService>> {
    Ok(load_services(state.config_dir()))
}

/// 新增或按 id 覆盖一条。返回写回去的完整清单。
#[tauri::command]
pub async fn node_service_save(
    state: State<'_, AppState>,
    service: NodeService,
) -> AppResult<Vec<NodeService>> {
    let mut list = load_services(state.config_dir());
    match list.iter_mut().find(|s| s.id == service.id) {
        Some(slot) => *slot = service,
        None => list.push(service),
    }
    save_services(state.config_dir(), &list)?;
    Ok(list)
}

/// 删一条，顺手把它停掉 —— 不然清单没了进程还在跑，谁都管不着它。
#[tauri::command]
pub async fn node_service_delete(
    state: State<'_, AppState>,
    id: String,
) -> AppResult<Vec<NodeService>> {
    state.node_services.stop(&id).await?;
    let mut list = load_services(state.config_dir());
    list.retain(|s| s.id != id);
    save_services(state.config_dir(), &list)?;
    Ok(list)
}

#[tauri::command]
pub async fn node_service_start(
    state: State<'_, AppState>,
    id: String,
) -> AppResult<ServiceStatus> {
    let list = load_services(state.config_dir());
    let spec = list
        .into_iter()
        .find(|s| s.id == id)
        .ok_or_else(|| crate::error::AppError::Config(format!("没有这条服务：{id}")))?;
    Ok(state.node_services.start(&spec).await?)
}

#[tauri::command]
pub async fn node_service_stop(state: State<'_, AppState>, id: String) -> AppResult<()> {
    state.node_services.stop(&id).await?;
    Ok(())
}

/// 所有服务的当前状态，给列表页轮询用。
#[tauri::command]
pub async fn node_service_status(state: State<'_, AppState>) -> AppResult<Vec<ServiceStatus>> {
    let list = load_services(state.config_dir());
    let mut out = Vec::with_capacity(list.len());
    for s in &list {
        out.push(state.node_services.status(&s.id).await);
    }
    Ok(out)
}

/// 客户端启动时把标了 enabled 的服务拉起来。
/// 一条起不来不该拖垮其余的 —— 记一条日志接着走。
pub async fn autostart(state: &AppState) {
    for s in load_services(state.config_dir()) {
        if !s.enabled {
            continue;
        }
        if let Err(e) = state.node_services.start(&s).await {
            tracing::warn!("node service {} 没起来：{e:?}", s.id);
        }
    }
}
