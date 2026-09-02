//! Node 轻量服务的命令面：增删改、起停、看状态。
//!
//! 服务清单存 `~/.ccload-client/node-services.json`，和 settings.json 同目录。
//! 起停是即时的，清单是持久的 —— 两者分开，改一条配置不会顺手重启服务。

use tauri::State;

use crate::error::{AppError, AppResult};
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
    // 校验放在后端而不是只放在表单里：命令是公开的，陈旧的界面或直接 invoke
    // 都能绕过前端那几个判断。端口撞了不会当场报错，而是等到启动时健康检查
    // 超时 —— 那个错误信息指不到真正的原因上。
    if service.id.trim().is_empty() {
        return Err(AppError::Config("服务名不能为空".into()).into());
    }
    if service.port < 1024 {
        return Err(AppError::Config(format!(
            "端口 {} 不可用：1024 以下是特权端口",
            service.port
        ))
        .into());
    }
    if let Some(other) = list
        .iter()
        .find(|s| s.id != service.id && s.port == service.port)
    {
        return Err(AppError::Config(format!(
            "端口 {} 已经被「{}」占了",
            service.port, other.id
        ))
        .into());
    }
    // 壳体自己占着的端口：CLI 数据面代理，以及当前内核（托管模式下是本机端口）。
    let kernel_port = state.settings.read().await.kernel.port;
    for (taken, who) in [
        (crate::services::cli_proxy::PROXY_PORT, "CLI 本地代理"),
        (kernel_port, "内核"),
    ] {
        if service.port == taken {
            return Err(AppError::Config(format!(
                "端口 {} 是{}在用的，换一个",
                service.port, who
            ))
            .into());
        }
    }
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

/// 注入给每个托管服务的平台环境变量。
///
/// `CCLOAD_BASE_URL` 指向 CLI 该走的入口（代理开着就是代理，否则是内核），
/// `CCLOAD_API_TOKEN` 是配套凭据，`CCLOAD_IMAGE_MCP` 是本客户端二进制
/// （脚本想直接用生图 MCP 时省得自己找路径）。脚本自己的同名 env 覆盖
/// 这些值 —— 想指向别的内核时不用改平台代码。
async fn platform_env(state: &AppState) -> Vec<(String, String)> {
    use crate::commands::cli::takeover_base;
    let mut env = vec![
        ("CCLOAD_BASE_URL".to_string(), takeover_base(state).await),
    ];
    if let Some(token) = state.settings.read().await.client_api_token.clone() {
        env.push(("CCLOAD_API_TOKEN".to_string(), token));
    }
    if let Ok(exe) = std::env::current_exe() {
        env.push(("CCLOAD_CLIENT_BIN".to_string(), exe.to_string_lossy().into_owned()));
    }
    env
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
    Ok(state.node_services.start(&spec, platform_env(&state).await).await?)
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
        let penv = platform_env(state).await;
        if let Err(e) = state.node_services.start(&s, penv).await {
            tracing::warn!("node service {} 没起来：{e:?}", s.id);
        }
    }
}

/// 把模板脚本写到用户选的位置。返回写到的路径，给 entry 用。
///
/// 为什么走后端：WebView 里没有任意路径写权限，而这正是「从模板建服务」的
/// 最后一步 —— 脚本不落盘，entry 就没法填。
#[tauri::command]
pub async fn node_service_write_script(
    state: State<'_, AppState>,
    suggested_name: String,
    body: String,
) -> AppResult<String> {
    let dir = state
        .config_dir()
        .join("node-services");
    tokio::fs::create_dir_all(&dir).await?;
    let safe_name = suggested_name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '-' })
        .collect::<String>();
    let path = dir.join(format!("{safe_name}.js"));
    // 已存在就轮一个后缀，不覆盖用户改过的旧脚本。
    let mut final_path = path.clone();
    let mut n = 1;
    while final_path.exists() {
        final_path = dir.join(format!("{safe_name}-{n}.js"));
        n += 1;
    }
    tokio::fs::write(&final_path, body).await?;
    // 脚本是可执行代码，且同机多用户的机器上不该让别的用户读改。0600 与
    // settings.json 同一标准（cli_io::PRIVATE_MODE）。
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&final_path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(final_path.to_string_lossy().into_owned())
}
