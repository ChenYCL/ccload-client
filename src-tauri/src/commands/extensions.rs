//! 扩展管理命令：MCP / Skill / Agent / Hook 的列举、装、卸、跨 CLI 同步。
//!
//! 与 `commands/cli.rs` 同样的规矩：命令层只负责取 state、拿 config_root、生成
//! 备份 stamp，真正的合并与写入逻辑全在 `services::cli_extensions` 里。

use tauri::State;

use crate::error::AppResult;
use crate::services::cli_extensions::{
    self, ExtensionItem, ExtensionKind, ExtensionSpec, ExtensionSupport, SyncOutcome,
};
use crate::services::cli_types::CliTarget;
use crate::state::AppState;
use crate::services::cli_backup::unique_stamp;

/// 列出某个 CLI 已装的扩展。`kind` 省略时返回四类的全集；该 CLI 不支持的类型
/// 直接不出现在结果里（列举场景返回空比抛错有用）。
#[tauri::command]
pub async fn extensions_list(
    state: State<'_, AppState>,
    target: CliTarget,
    kind: Option<ExtensionKind>,
) -> AppResult<Vec<ExtensionItem>> {
    let root = state.config_root().await?;
    Ok(cli_extensions::list(&root, target, kind)?)
}

/// 支持矩阵：5 个 CLI × 4 类扩展，前端拿它决定哪些按钮可点。
#[tauri::command]
pub async fn extensions_support() -> AppResult<Vec<ExtensionSupport>> {
    Ok(cli_extensions::support_matrix())
}

/// 装一个扩展（同 id 存在即覆盖）。返回被写入的文件；skill/agent 覆盖时被换下
/// 的旧版本会以 `… (已归档)` 的形式出现在返回值里。
#[tauri::command]
pub async fn extension_install(
    state: State<'_, AppState>,
    target: CliTarget,
    kind: ExtensionKind,
    spec: ExtensionSpec,
) -> AppResult<Vec<String>> {
    let root = state.config_root().await?;
    Ok(cli_extensions::install(
        &root,
        target,
        kind,
        &spec,
        &unique_stamp(),
        &state.backups,
    )?)
}

/// 卸一个扩展。找不到会报中文错误，不会静默成功。
#[tauri::command]
pub async fn extension_remove(
    state: State<'_, AppState>,
    target: CliTarget,
    kind: ExtensionKind,
    id: String,
) -> AppResult<Vec<String>> {
    let root = state.config_root().await?;
    Ok(cli_extensions::remove(
        &root,
        target,
        kind,
        &id,
        &unique_stamp(),
        &state.backups,
    )?)
}

/// 读回一个已装扩展的规范化描述，供编辑框回填。
#[tauri::command]
pub async fn extension_read(
    state: State<'_, AppState>,
    target: CliTarget,
    kind: ExtensionKind,
    id: String,
) -> AppResult<ExtensionSpec> {
    let root = state.config_root().await?;
    Ok(cli_extensions::read_spec(&root, target, kind, &id)?)
}

/// 把一个扩展同步到多个 CLI。`source` 省略时自动挑第一个装了它的 CLI 当来源。
/// 逐目标独立成败：某家不支持只会让那一行 `ok=false`，其余照常写入。
#[tauri::command]
pub async fn extension_sync(
    state: State<'_, AppState>,
    kind: ExtensionKind,
    id: String,
    targets: Vec<CliTarget>,
    source: Option<CliTarget>,
) -> AppResult<Vec<SyncOutcome>> {
    let root = state.config_root().await?;
    Ok(cli_extensions::sync(
        &root,
        kind,
        &id,
        &targets,
        source,
        &unique_stamp(),
        &state.backups,
    )?)
}

