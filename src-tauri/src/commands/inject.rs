//! 系统注入命令：把受管的说明块写进各 CLI 的全局指令文件。
//!
//! 为什么要有这一层：给 CLI 装上 `ccload-vision` 不等于它会用 —— 宿主模型
//! 只看得见工具名和一句描述，遇到图片会不会想起来调，全看运气。写进系统
//! 提示的一条规则比工具描述强得多。细节见 `services::system_inject`。

use tauri::State;

use crate::error::AppResult;
use crate::services::cli_backup::unique_stamp;
use crate::services::cli_extensions::ALL_TARGETS;
use crate::services::cli_types::CliTarget;
use crate::services::system_inject::{self, InjectSpec, InjectState};
use crate::state::AppState;

/// 五个 CLI 的注入状态（文件在哪、注没注、块里现在写着什么）。
#[tauri::command]
pub async fn inject_state(state: State<'_, AppState>) -> AppResult<Vec<InjectState>> {
    let root = state.config_root().await?;
    Ok(system_inject::states(&root, &ALL_TARGETS))
}

/// 预览将要写进块里的内容。
///
/// 单列一个命令而不是让前端自己拼：视觉那段里的工具名必须和 MCP 真正暴露的
/// 一致，前端再抄一份必然会漂 —— 漂了就是教模型去调一个不存在的工具。
#[tauri::command]
pub fn inject_preview(spec: InjectSpec) -> AppResult<String> {
    Ok(system_inject::render_block(&spec))
}

/// 写入（spec 全空则移除）。逐目标独立成败：一家的指令文件坏了不该拖累其余。
///
/// 必须串行 —— 五路并行会同时改 `backups/manifest.json`，短写入叠在旧文件
/// 尾巴上就把备份清单写坏了（视觉 MCP 那边踩过一模一样的坑）。
#[tauri::command]
pub async fn inject_apply(
    state: State<'_, AppState>,
    targets: Vec<CliTarget>,
    spec: InjectSpec,
) -> AppResult<Vec<InjectOutcome>> {
    let root = state.config_root().await?;
    let mut out = Vec::new();
    for (i, target) in targets.into_iter().enumerate() {
        let stamp = format!("{}-{i}", unique_stamp());
        match system_inject::apply(&root, target, &spec, &stamp, &state.backups) {
            Ok(path) => out.push(InjectOutcome {
                target,
                label: target.label(),
                ok: true,
                path: Some(path),
                error: None,
            }),
            Err(e) => out.push(InjectOutcome {
                target,
                label: target.label(),
                ok: false,
                path: None,
                error: Some(e.to_string()),
            }),
        }
    }
    Ok(out)
}

#[derive(Debug, serde::Serialize)]
pub struct InjectOutcome {
    pub target: CliTarget,
    pub label: &'static str,
    pub ok: bool,
    pub path: Option<String>,
    pub error: Option<String>,
}
