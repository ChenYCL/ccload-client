//! 动态注入的命令面：读 / 写。机制在 `services::dynamic_inject`，数据面在
//! `cli_proxy`。这里只管两处状态的一致：磁盘上的 `dynamic-inject.json` 和
//! 代理内存里的配置。保存 = 归一化 + 落盘 + 刷代理，不用重启代理。

use std::path::PathBuf;

use tauri::State;

use crate::error::AppResult;
use crate::services::dynamic_inject::InjectConfig;
use crate::state::AppState;

pub(crate) fn store_path(state: &AppState) -> PathBuf {
    state.config_dir().join("dynamic-inject.json")
}

/// 把磁盘上的配置装进代理。代理没起来就什么都不做（起来时 ensure_cli_proxy
/// 会再调，和钉住表、拒绝接管同一条路径）。
pub(crate) async fn refresh_proxy_dynamic_inject(state: &AppState) {
    if let Ok(cfg) = InjectConfig::load(&store_path(state)) {
        if let Some(proxy) = state.cli_proxy.read().await.as_ref() {
            proxy.set_inject(cfg).await;
        }
    }
}

#[tauri::command]
pub async fn dynamic_inject_get(state: State<'_, AppState>) -> AppResult<InjectConfig> {
    Ok(InjectConfig::load(&store_path(&state))?)
}

#[tauri::command]
pub async fn dynamic_inject_set(
    state: State<'_, AppState>,
    cfg: InjectConfig,
) -> AppResult<InjectConfig> {
    let cfg = cfg.normalized();
    cfg.save(&store_path(&state))?;
    refresh_proxy_dynamic_inject(&state).await;
    Ok(cfg)
}
