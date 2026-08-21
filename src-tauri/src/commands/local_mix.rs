//! 本地混用命令：探测本机服务、把远端 ccLoad 和本机服务建成本机内核的渠道。
//!
//! 拓扑上为什么只能这么接，见 `services::local_mix` 的模块注释。

use tauri::State;

use crate::error::AppResult;
use crate::services::local_mix::{self, ChannelSpec, ProbeResult};
use crate::state::AppState;

/// 问一次 `{base}/v1/models`，确认地址真的是个 OpenAI 兼容服务，顺带把模型
/// 清单带回来 —— 那正好是建渠道必填的 `models`。
#[tauri::command]
pub async fn local_mix_probe(base_url: String, api_key: Option<String>) -> AppResult<ProbeResult> {
    Ok(local_mix::probe(&base_url, api_key.as_deref()).await)
}

/// 一个渠道的建立结果。逐个独立成败：远端那条建好了、本地那条重名，
/// 不该让已经成功的那条也回滚 —— 内核里它确实已经建出来了。
#[derive(Debug, serde::Serialize)]
pub struct MixOutcome {
    pub name: String,
    pub ok: bool,
    pub channel_id: Option<i64>,
    pub error: Option<String>,
}

/// 按给定的清单建渠道。
///
/// 调用方（界面）负责保证此时内核已经是**本机**模式且已启动 —— 远端内核够不着
/// `127.0.0.1`，往远端建一条指向回环的渠道是这个功能要解决的那个错误本身。
#[tauri::command]
pub async fn local_mix_setup(
    state: State<'_, AppState>,
    channels: Vec<ChannelSpec>,
) -> AppResult<Vec<MixOutcome>> {
    let mut out = Vec::new();
    for spec in &channels {
        match local_mix::create_channel(&state, spec).await {
            Ok(id) => out.push(MixOutcome {
                name: spec.name.clone(),
                ok: true,
                channel_id: Some(id),
                error: None,
            }),
            Err(e) => out.push(MixOutcome {
                name: spec.name.clone(),
                ok: false,
                channel_id: None,
                error: Some(e.to_string()),
            }),
        }
    }
    Ok(out)
}
