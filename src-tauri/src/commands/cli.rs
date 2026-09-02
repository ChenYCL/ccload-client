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
use crate::services::context_window::ContextPolicy;
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
pub(crate) async fn takeover_base(state: &AppState) -> String {
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
    let mut options = options.unwrap_or_default();
    options.context_tokens = resolve_context_tokens(&state, &root, target, &options).await;
    Ok(apply_takeover(
        &root,
        target,
        &base,
        &token,
        &unique_stamp(),
        &state.backups,
        options,
    )?)
}

/// 这次接管该把上下文窗口写成多少。`None` = 不写，保留 CLI 现状。
///
/// 模型名的来源有先后：**这次表单里选的** → 磁盘上现有的。用户只点「写入」
/// 没动模型时也要按现有模型重算 —— 那正是「上次导入写了 200k，后来在别处把模型
/// 换成了 1M 的，窗口却一直停在 200k」的修法。
///
/// Claude Code 还要多一步：和**模型链最窄的那一跳**取 min。名字上写着 1M、真跑
/// 起来会掉到一条 500k 的渠道上时，按 1M 写就是本模块开头讲的那个死锁 —— 等
/// compact 触发时早就越过了真实天花板，而 compact 自己也要把整段发出去。
async fn resolve_context_tokens(
    state: &AppState,
    root: &crate::services::cli_config::ConfigRoot,
    target: CliTarget,
    opts: &TakeoverOptions,
) -> Option<i64> {
    let policy = state.settings.read().await.context_policy;
    let picked = match target {
        CliTarget::ClaudeCode => opts.anthropic_model.clone(),
        CliTarget::Codex => opts.codex_model.clone(),
        CliTarget::GeminiCli => opts.gemini_model.clone(),
        CliTarget::GrokBuild => opts.grok_model.clone(),
        CliTarget::OpenCode => opts.opencode_model.clone(),
    };
    let model = picked
        .filter(|s| !s.trim().is_empty())
        .or_else(|| crate::services::cli_config::current_model(root, target))
        .unwrap_or_default();

    let mut tokens = policy.resolve(&model)?;
    if matches!(target, CliTarget::ClaudeCode) {
        if let Some(ceiling) = chain_ceiling_for(state, &model) {
            tokens = tokens.min(ceiling);
        }
    }
    i64::try_from(tokens).ok()
}

/// 这个别名配了模型链的话，链上最窄那一跳的窗口。读不到就当没有 —— 窗口是
/// 个优化，不该因为链文件坏了就让接管整体失败。
fn chain_ceiling_for(state: &AppState, alias: &str) -> Option<u64> {
    let path = crate::commands::fallback::store_path(state);
    let store = crate::services::fallback::FallbackStore::load(&path).ok()?;
    let chain = store.chains.iter().find(|c| c.alias == alias)?;
    crate::services::context_window::chain_ceiling(&chain.hops)
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
        // 自愈也要带上窗口 —— 不然重写回来的配置窗口是空的。
        let mut opts = TakeoverOptions::default();
        opts.context_tokens = resolve_context_tokens(&state, &root, target, &opts).await;
        // 「窗口漂了」也算需要重写。
        //
        // 这条是「存量 200k 自动清掉」的实现：接管地址和令牌都没问题、
        // `already_active` 为真，但磁盘上的窗口停在某次旧写入的数字上（最常见的是
        // 「模型导入」当天写的），而策略现在算出来是另一个数。光靠 already_active
        // 判断的话，用户必须手动去点每一家的「写入」才修得掉。
        let window_drifted = opts.context_tokens.is_some_and(|want| {
            crate::services::cli_config::current_context_tokens(&root, target) != Some(want)
        });
        if p.already_active && !window_drifted {
            continue;
        }
        match apply_takeover(
            &root,
            target,
            &base,
            &token,
            &unique_stamp(),
            &state.backups,
            opts,
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

/// 上下文窗口总控的现值。
#[tauri::command]
pub async fn context_policy_get(state: State<'_, AppState>) -> AppResult<ContextPolicy> {
    Ok(state.settings.read().await.context_policy)
}

/// 改总控。**只存策略，不写 CLI** —— 真正落盘要用户去点每家的「写入」，和这一
/// 页其它开关一个规矩（配置是用户的，我们不背着他改）。返回值是「有几家已接管
/// 的 CLI 还没按新策略重写」，前端拿它提示要不要去点写入。
#[tauri::command]
pub async fn context_policy_set(
    state: State<'_, AppState>,
    policy: ContextPolicy,
) -> AppResult<usize> {
    state.settings.write().await.context_policy = policy;
    state.persist().await?;
    let root = state.config_root().await?;
    let mut stale = 0usize;
    for t in TARGETS {
        if crate::services::cli_config::current_endpoint(&root, t).is_some() {
            stale += 1;
        }
    }
    Ok(stale)
}

/// 每家 CLI 按当前策略**会写成多少**，给界面显示用。
///
/// 和 `resolve_context_tokens` 走同一条路 —— 显示和实际写入必须是同一个数，
/// 否则又回到「界面说 200k、实际 1M」那种谁也不信谁的状态。
#[tauri::command]
pub async fn context_window_preview(state: State<'_, AppState>) -> AppResult<Vec<WindowPreview>> {
    let root = state.config_root().await?;
    let mut out = Vec::new();
    for target in TARGETS {
        let opts = TakeoverOptions::default();
        let model = crate::services::cli_config::current_model(&root, target);
        out.push(WindowPreview {
            target,
            source: model
                .as_deref()
                .map(crate::services::context_window::window_source),
            model,
            tokens: resolve_context_tokens(&state, &root, target, &opts).await,
            on_disk: crate::services::cli_config::current_context_tokens(&root, target),
        });
    }
    Ok(out)
}

/// 手动刷新 models.dev 目录。返回收录了多少个模型。
#[tauri::command]
pub async fn model_catalog_refresh(state: State<'_, AppState>) -> AppResult<usize> {
    let path = state.config_dir().join("models-dev.json");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| AppError::Config(format!("build http client: {e}")))?;
    Ok(crate::services::model_catalog::refresh(&client, &path).await?)
}

#[derive(serde::Serialize)]
pub struct WindowPreview {
    pub target: CliTarget,
    /// 磁盘上现在选的模型，窗口就是按它算的。
    pub model: Option<String>,
    /// None = 这一家不写窗口（Off 档，或者压根没选模型）。
    pub tokens: Option<i64>,
    /// 这个数是哪来的：模型名后缀 / models.dev / 内置猜测表。
    pub source: Option<crate::services::context_window::WindowSource>,
    /// 磁盘上**现在**写着的数。和 `tokens` 不一致就说明还没写入（或者是存量旧值）。
    pub on_disk: Option<i64>,
}
