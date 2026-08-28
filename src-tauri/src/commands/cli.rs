//! CLI takeover commands. Preview-first, snapshot-always, restorable.

use tauri::State;

use crate::error::{AppError, AppResult};
use crate::services::cli_advanced::{
    known_env_keys, read_files, write_file, ConfigFileView, EnvKeyInfo, TakeoverOptions,
};
use crate::services::cli_backup::BackupEntry;
use crate::services::cli_backup_diff::{diff_backup, BackupDiff, DiffBase};
use crate::services::cli_config::{
    apply_takeover, preview, CliTarget, TakeoverPreview, TakeoverResult,
};
use crate::state::AppState;
use crate::services::cli_backup::unique_stamp;

const TARGETS: [CliTarget; 5] = [
    CliTarget::ClaudeCode,
    CliTarget::Codex,
    CliTarget::GeminiCli,
    CliTarget::GrokBuild,
    CliTarget::OpenCode,
];

/// CLI 该被指到哪儿。
///
/// 由 `route_cli_through_proxy` 决定，**不是**由代理跑没跑决定 —— 代理一直在
/// 跑，这个开关只管「写进 CLI 配置的地址」。开着才拿得到会话归因和模型名改写
/// （内核日志里没有 session_id）。开着但代理没起来时退回直连内核：功能少一截，
/// 总好过把 CLI 指到一个没人监听的地址。
async fn takeover_base(state: &AppState) -> String {
    let s = state.settings.read().await;
    if s.route_cli_through_proxy {
        drop(s);
        if let Some(p) = state.cli_proxy.read().await.as_ref() {
            return p.base_url();
        }
        // 开关开着但代理没起来（端口被占）：宁可直连内核，也不能把 CLI 指到
        // 一个没人监听的地址上。
        return state.settings.read().await.kernel.base_url();
    }
    s.kernel.base_url()
}

#[tauri::command]
pub async fn cli_preview(
    state: State<'_, AppState>,
    target: CliTarget,
) -> AppResult<TakeoverPreview> {
    let root = state.config_root().await?;
    let base = takeover_base(&state).await;
    let token = state.settings.read().await.client_api_token.clone();
    Ok(preview(&root, target, &base, token.as_deref()))
}

#[tauri::command]
pub async fn cli_preview_all(state: State<'_, AppState>) -> AppResult<Vec<TakeoverPreview>> {
    let root = state.config_root().await?;
    let base = takeover_base(&state).await;
    let token = state.settings.read().await.client_api_token.clone();
    Ok(TARGETS
        .into_iter()
        .map(|t| preview(&root, t, &base, token.as_deref()))
        .collect())
}

#[tauri::command]
pub async fn cli_apply(
    state: State<'_, AppState>,
    target: CliTarget,
    options: Option<TakeoverOptions>,
) -> AppResult<TakeoverResult> {
    let base = takeover_base(&state).await;
    let token = state.settings.read().await.client_api_token.clone();
    let token = token.ok_or_else(|| {
        AppError::Config(
            "no client API token yet — start the kernel first so one can be created".into(),
        )
    })?;
    let root = state.config_root().await?;
    Ok(apply_takeover(
        &root,
        target,
        &base,
        &token,
        &unique_stamp(),
        &state.backups,
        options.unwrap_or_default(),
    )?)
}

/// Snapshots for one target, or all targets when `target` is omitted.
#[tauri::command]
pub async fn cli_backups(
    state: State<'_, AppState>,
    target: Option<CliTarget>,
) -> AppResult<Vec<BackupEntry>> {
    Ok(state.backups.list(target)?)
}

/// Roll a target's config files back to a snapshot. Files that did not exist
/// when the snapshot was taken are deleted, not recreated empty.
#[tauri::command]
pub async fn cli_restore(
    state: State<'_, AppState>,
    backup_id: String,
) -> AppResult<Vec<String>> {
    let root = state.config_root().await?;
    Ok(state.backups.restore(&root, &backup_id)?)
}

/// 一份快照相对某个基准改了什么。
///
/// 「恢复」是不可逆地覆盖当前配置，而列表上只看得到时间和「N 个文件」——
/// 先看 diff 再决定，才敢点那个按钮。基准默认是**磁盘现状**（回答「恢复会把我
/// 现在的配置改成什么样」），也能选上一份快照或原始配置。
#[tauri::command]
pub async fn cli_backup_diff(
    state: State<'_, AppState>,
    backup_id: String,
    base: Option<DiffBase>,
) -> AppResult<BackupDiff> {
    let root = state.config_root().await?;
    Ok(diff_backup(
        &state.backups,
        &root,
        &backup_id,
        base.unwrap_or(DiffBase::Current),
    )?)
}

/// Read every config file for a target as raw text, for the editor UI.
#[tauri::command]
pub async fn cli_read_files(
    state: State<'_, AppState>,
    target: CliTarget,
) -> AppResult<Vec<ConfigFileView>> {
    let root = state.config_root().await?;
    Ok(read_files(&root, target)?)
}

/// Replace one config file with user-edited text. Validates JSON/TOML before
/// touching the file, and snapshots first so the edit is undoable.
#[tauri::command]
pub async fn cli_write_file(
    state: State<'_, AppState>,
    target: CliTarget,
    rel: String,
    body: String,
) -> AppResult<String> {
    let root = state.config_root().await?;
    Ok(write_file(&root, target, &rel, &body, &unique_stamp(), &state.backups)?)
}

/// Metadata for the advanced-settings UI: which knobs exist for a target,
/// our suggested default, and what the machine currently has.
#[tauri::command]
pub async fn cli_env_keys(
    state: State<'_, AppState>,
    target: CliTarget,
) -> AppResult<Vec<EnvKeyInfo>> {
    let root = state.config_root().await?;
    Ok(known_env_keys(&root, target))
}


/// 把「曾经接管过、后来被 CLI 自己冲掉」的目标重新写回去。
///
/// 为什么需要：实测 Codex 的 `auth.json` 退回了 `auth_mode: chatgpt`、
/// Gemini 的 `.env` 被清成了 0 字节，而两边的写入器单测全绿 —— 配置不是没写对，
/// 是写完之后被 CLI 自己覆盖了（重新登录、自动升级都会）。ccLoad 本来就是接管
/// 方，官方配置留着快照能还原就够了，所以这里直接覆盖回去。
///
/// 只碰**有过快照**的目标：没接管过的 CLI 不该被我们悄悄接管。
#[tauri::command]
pub async fn cli_reconcile(state: State<'_, AppState>) -> AppResult<Vec<String>> {
    let root = state.config_root().await?;
    let base = takeover_base(&state).await;
    let token = state.settings.read().await.client_api_token.clone();
    let Some(token) = token else {
        return Ok(Vec::new());
    };

    let mut healed = Vec::new();
    for target in TARGETS {
        // 没有快照 = 用户从没让我们接管过这一家，别自作主张。
        let taken_before = state
            .backups
            .list(Some(target))
            .map(|v| !v.is_empty())
            .unwrap_or(false);
        if !taken_before {
            continue;
        }
        let p = preview(&root, target, &base, Some(token.as_str()));
        if p.already_active {
            continue;
        }
        match apply_takeover(
            &root,
            target,
            &base,
            &token,
            &unique_stamp(),
            &state.backups,
            TakeoverOptions::default(),
        ) {
            Ok(_) => healed.push(target.label().to_string()),
            // 一家写不动不该拖垮其余四家。
            Err(e) => tracing::warn!("reconcile {}: {e}", target.label()),
        }
    }
    Ok(healed)
}

/// 切换「CLI 走本地代理」。
///
/// 只改设置，不动磁盘上的 CLI 配置 —— 改地址是有后果的操作，得让用户在
/// 接管页显式点「写入」。返回是否需要重写（有接管过但地址对不上的目标就需要）。
#[tauri::command]
pub async fn cli_set_proxy_routing(
    state: State<'_, AppState>,
    enabled: bool,
) -> AppResult<bool> {
    state.settings.write().await.route_cli_through_proxy = enabled;
    state.persist().await?;
    let root = state.config_root().await?;
    let base = takeover_base(&state).await;
    let token = state.settings.read().await.client_api_token.clone();
    let needs_rewrite = TARGETS.iter().any(|t| {
        let p = preview(&root, *t, &base, token.as_deref());
        p.exists && !p.already_active
    });
    Ok(needs_rewrite)
}
