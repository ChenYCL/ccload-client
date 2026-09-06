//! 拒绝接管的命令面：读 / 写 / 取内置破甲提示词。
//!
//! 机制在 `services::rescue`，数据面在 `cli_proxy`。这里只管两处状态的一致：
//! 磁盘上的 `rescue.json` 和代理内存里的配置。保存 = 归一化 + 落盘 + 刷代理，
//! 不用重启代理。

use std::path::PathBuf;

use tauri::State;

use crate::error::AppResult;
use crate::services::rescue::{RescueConfig, DEFAULT_ARMOR_PROMPT};
use crate::state::AppState;

pub(crate) fn store_path(state: &AppState) -> PathBuf {
    state.config_dir().join("rescue.json")
}

/// 把磁盘上的配置装进代理。代理没起来就什么都不做（起来时 ensure_cli_proxy 会再调）。
pub(crate) async fn refresh_proxy_rescue(state: &AppState) {
    if let Ok(cfg) = RescueConfig::load(&store_path(state)) {
        if let Some(proxy) = state.cli_proxy.read().await.as_ref() {
            proxy.set_rescue(cfg).await;
        }
    }
}

#[tauri::command]
pub async fn rescue_get(state: State<'_, AppState>) -> AppResult<RescueConfig> {
    Ok(RescueConfig::load(&store_path(&state))?)
}

#[tauri::command]
pub async fn rescue_set(state: State<'_, AppState>, cfg: RescueConfig) -> AppResult<RescueConfig> {
    let cfg = cfg.normalized();
    cfg.save(&store_path(&state))?;
    refresh_proxy_rescue(&state).await;
    Ok(cfg)
}

/// 内置默认破甲提示词。界面「恢复默认」拿它回填。
#[tauri::command]
pub async fn rescue_default_prompt() -> AppResult<String> {
    Ok(DEFAULT_ARMOR_PROMPT.to_string())
}
