//! CLI takeover commands. Preview-first, snapshot-always, restorable.

use std::time::Duration;

use serde_json::Value;
use tauri::State;

use crate::error::{AppError, AppResult};
use crate::services::cli_advanced::{
    known_env_keys, read_files, write_file, ConfigFileView, EnvKeyInfo, TakeoverOptions,
};
use crate::services::cli_backup::unique_stamp;
use crate::services::cli_backup::BackupEntry;
use crate::services::cli_backup_diff::{diff_backup, BackupDiff, DiffBase};
use crate::services::cli_config::{
    apply_takeover, current_context_tokens, current_model, preview, CliTarget, ConfigRoot,
    TakeoverPreview, TakeoverResult,
};
use crate::services::context_floor::{Candidate, Floor, FloorInputs, KernelRoutes, RouteHit};
use crate::services::context_window::{tier_key, ContextMode, ContextPolicy, WindowSource};
use crate::services::fallback::{FallbackChain, FallbackStore};
use crate::services::forced_route::{ForcedRoute, ForcedRouteStore};
use crate::state::AppState;

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
    // 显式点「写入」= 重新表态要接管这一家，解除之前恢复留下的退出标记。
    {
        let mut s = state.settings.write().await;
        if s.takeover_opted_out.remove(&target) {
            drop(s);
            state.persist().await?;
        }
    }
    let ctx = WindowContext::load(&state).await;
    let options = ctx.with_window(&root, target, options.unwrap_or_default());
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

/// 解析窗口要用到的全部输入，一次加载、多处复用。
///
/// 内核那份（`GET /admin/channels`）要联网，拿不到就 `None`，按本地的链和路由算 ——
/// 窗口是个优化，不该因为内核没起来就让接管整体失败。但少算了一块的结果偏宽，
/// 启动自愈拿它比对磁盘会判成「漂了」；所以 [`Self::complete`] 要告诉调用方这次
/// 算得全不全。
pub(crate) struct WindowContext {
    pub(crate) policy: ContextPolicy,
    /// 生效中的钉住。CLI 不走代理时为空 —— 钉住只在代理那一层起作用。
    pins: Vec<crate::services::pins::Pin>,
    chains: Vec<FallbackChain>,
    routes: Vec<ForcedRoute>,
    kernel: Option<KernelRoutes>,
}

impl WindowContext {
    pub(crate) async fn load(state: &AppState) -> Self {
        let (policy, via_proxy) = {
            let s = state.settings.read().await;
            (s.context_policy.clone(), s.route_cli_through_proxy)
        };
        // 钉住只在 CLI 走代理时影响落点；直连的 CLI 发的是原名，内核照默认顺序选。
        let pins = if via_proxy {
            crate::services::pins::PinStore::load(&crate::commands::pins::store_path(state))
                .map(|s| s.pins)
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        // 链文件坏了就当没有链：窗口是优化，别让它拖垮接管。
        let chains = FallbackStore::load(&crate::commands::fallback::store_path(state))
            .map(|s| s.chains)
            .unwrap_or_default();
        let routes = ForcedRouteStore::load(&crate::commands::forced_route::store_path(state))
            .map(|s| s.routes)
            .unwrap_or_default();
        // 只有自动档要看内核落点；固定和不写入不用为此联网。
        let kernel = if policy.mode == ContextMode::Auto {
            fetch_kernel_routes(state).await
        } else {
            None
        };
        Self {
            policy,
            pins,
            chains,
            routes,
            kernel,
        }
    }

    fn inputs(&self) -> FloorInputs<'_> {
        FloorInputs {
            policy: &self.policy,
            pins: &self.pins,
            chains: &self.chains,
            routes: &self.routes,
            kernel: self.kernel.as_ref(),
        }
    }

    /// 这次解析用到了该用的全部来源。自动档要求内核连上；别的档不看内核。
    pub(crate) fn complete(&self) -> bool {
        self.policy.mode != ContextMode::Auto || self.kernel.is_some()
    }

    pub(crate) fn floor_for(&self, model: &str) -> Option<Floor> {
        self.inputs().floor(model)
    }

    pub(crate) fn candidates_for(&self, model: &str) -> Vec<Candidate> {
        self.inputs().candidates(model)
    }

    /// 这次接管该把上下文窗口写成多少。
    ///
    /// 模型名的来源有先后：**这次表单里选的** → 磁盘上现有的。用户只点「写入」
    /// 没动模型时也要按现有模型重算 —— 那正是「上次导入写了 200k，后来在别处把
    /// 模型换成了 1M 的，窗口却一直停在 200k」的修法。
    pub(crate) fn floor_for_target(
        &self,
        root: &ConfigRoot,
        target: CliTarget,
        opts: &TakeoverOptions,
    ) -> Option<Floor> {
        let picked = match target {
            CliTarget::ClaudeCode => opts.anthropic_model.clone(),
            CliTarget::Codex => opts.codex_model.clone(),
            CliTarget::GeminiCli => opts.gemini_model.clone(),
            CliTarget::GrokBuild => opts.grok_model.clone(),
            CliTarget::OpenCode => opts.opencode_model.clone(),
        };
        let model = picked
            .filter(|s| !s.trim().is_empty())
            .or_else(|| current_model(root, target))
            .unwrap_or_default();
        self.floor_for(&model)
    }

    /// 把算好的窗口和压缩百分比填进接管选项。`context_tokens = None` = 不写。
    pub(crate) fn with_window(
        &self,
        root: &ConfigRoot,
        target: CliTarget,
        mut opts: TakeoverOptions,
    ) -> TakeoverOptions {
        opts.context_tokens = self
            .floor_for_target(root, target, &opts)
            .and_then(|f| i64::try_from(f.tokens).ok());
        opts.compact_percent = Some(self.policy.percent());
        opts
    }
}

/// `GET /admin/channels` → 别名落点表。失败只记 warn：内核没起来、远端断了、
/// 密码改了，都不该让「写一份 CLI 配置」这件事失败。
async fn fetch_kernel_routes(state: &AppState) -> Option<KernelRoutes> {
    let (base_url, password) = {
        let s = state.settings.read().await;
        (s.kernel.base_url(), s.kernel.admin_password.clone())
    };
    let fut = state
        .admin
        .request(&base_url, &password, "GET", "channels", None, None);
    // 启动自愈也走这条路；内核不在时不能让它卡住整个启动。
    match tokio::time::timeout(Duration::from_secs(8), fut).await {
        Ok(Ok(resp)) => Some(KernelRoutes::parse(resp.get("data").unwrap_or(&Value::Null))),
        Ok(Err(e)) => {
            tracing::warn!("context window: kernel channels unavailable, local-only floor: {e}");
            None
        }
        Err(_) => {
            tracing::warn!("context window: kernel channels timed out, local-only floor");
            None
        }
    }
}

/// 给人看的窗口数：1M / 500k / 262144。
fn fmt_tokens(n: u64) -> String {
    if n >= 1_000_000 && n % 1_000_000 == 0 {
        format!("{}M", n / 1_000_000)
    } else if n >= 1_000 && n % 1_000 == 0 {
        format!("{}k", n / 1_000)
    } else {
        n.to_string()
    }
}

fn describe_floor(f: &Floor) -> String {
    match &f.narrowest {
        Some(c) => match &c.channel_name {
            Some(ch) => format!("最窄：{} · {ch}", c.model),
            None => format!("最窄：{}", c.model),
        },
        None => "固定值".into(),
    }
}

/// Snapshots for one target, or all targets when `target` is omitted.
#[tauri::command]
pub async fn cli_backups(
    state: State<'_, AppState>,
    target: Option<CliTarget>,
) -> AppResult<Vec<BackupEntry>> {
    // 按当前配置根过滤：沙箱和真实 home 共用一个 store，把沙箱快照列出来
    // 让用户点「恢复」，会按它记录的 existed:false 去删真实配置。
    let root = state.config_root().await?;
    Ok(state.backups.list_in(&root, target)?)
}

/// Roll a target's config files back to a snapshot. Files that did not exist
/// when the snapshot was taken are deleted, not recreated empty.
#[tauri::command]
pub async fn cli_restore(
    state: State<'_, AppState>,
    backup_id: String,
) -> AppResult<Vec<String>> {
    let root = state.config_root().await?;
    let restored = state.backups.restore(&root, &backup_id)?;
    // 恢复 = 用户要的是「回到那个样子」。启动自愈的触发条件是「这一家有过快照」，
    // 而恢复并不会删快照 —— 不记一笔的话，下次启动又被接管回去，恢复按钮等于失灵。
    // 直到用户再显式点一次「写入」才解除。
    if let Some(target) = state
        .backups
        .list(None)
        .ok()
        .and_then(|all| all.into_iter().find(|b| b.id == backup_id).map(|b| b.target))
    {
        state.settings.write().await.takeover_opted_out.insert(target);
        state.persist().await?;
    }
    Ok(restored)
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

    let opted_out = state.settings.read().await.takeover_opted_out.clone();
    let ctx = WindowContext::load(&state).await;
    let mut healed = Vec::new();
    for target in TARGETS {
        // 用户点过「恢复」= 明确不要这一家被接管了。快照还在不代表他还想要。
        if opted_out.contains(&target) {
            continue;
        }
        // 没有快照 = 用户从没让我们接管过这一家，别自作主张。
        let taken_before = state
            .backups
            .list_in(&root, Some(target))
            .map(|v| !v.is_empty())
            .unwrap_or(false);
        if !taken_before {
            continue;
        }
        let p = preview(&root, target, &base, Some(token.as_str()));
        // 自愈也要带上窗口 —— 不然重写回来的配置窗口是空的。
        let opts = ctx.with_window(&root, target, TakeoverOptions::default());
        // 「窗口漂了」也算需要重写。
        //
        // 这条是「存量 200k 自动清掉」的实现：接管地址和令牌都没问题、
        // `already_active` 为真，但磁盘上的窗口停在某次旧写入的数字上（最常见的是
        // 「模型导入」当天写的），而策略现在算出来是另一个数。光靠 already_active
        // 判断的话，用户必须手动去点每一家的「写入」才修得掉。
        //
        // 只对**真有窗口键**的 CLI 做这个比对。Gemini 没有这个旋钮：磁盘上永远读
        // 不到、策略却永远算得出，一比就是「漂了」—— 每次启动都重写、都占一份快照，
        // 而实际上什么都改不了。
        //
        // 内核没连上那次也不比：少算了内核落点的窗口偏宽，拿它判漂移会在内核每次
        // 没起来时都重写一遍、再在内核起来后写回去 —— 两边各占一份快照。
        let window_drifted = ctx.complete()
            && target.has_context_window_key()
            && opts.context_tokens.is_some_and(|want| {
                current_context_tokens(&root, target) != Some(want)
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

/// 链 / 强制路由 / 调度图应用之后，把每家**已接管**CLI 的窗口按新的落点重算并落盘。
///
/// 落点变了窗口就变了：给 claude-opus-5 的链加一跳 500k 的 grok，Claude Code 的
/// 窗口必须从 1M 降到 500k，否则分流到那一跳的那一刻就是 400 too long。以前只有
/// 模型链会顺手改 Claude Code 一家，强制路由和调度图改了落点什么都不动；现在三处
/// 都走这一条，五家一起对齐。
///
/// 只碰**现在正被接管**（地址、令牌都对得上）的 CLI；用户已经把配置改到别处的
/// 不管。内核没连上就一家都不写 —— 那次算出来的值是偏宽的。
pub(crate) async fn resync_windows(state: &AppState) -> Vec<String> {
    let Ok(root) = state.config_root().await else {
        return Vec::new();
    };
    let base = takeover_base(state).await;
    let Some(token) = state.settings.read().await.client_api_token.clone() else {
        return Vec::new();
    };
    let opted_out = state.settings.read().await.takeover_opted_out.clone();
    let ctx = WindowContext::load(state).await;
    if !ctx.complete() {
        return vec!["内核没连上，各 CLI 的上下文窗口这次没有重算；内核起来后在「CLI 接管」页点「写入」".into()];
    }
    let mut log = Vec::new();
    for target in TARGETS {
        if !target.has_context_window_key() || opted_out.contains(&target) {
            continue;
        }
        let p = preview(&root, target, &base, Some(token.as_str()));
        if !p.already_active {
            continue;
        }
        let opts = TakeoverOptions::default();
        let Some(floor) = ctx.floor_for_target(&root, target, &opts) else {
            continue;
        };
        let Ok(want) = i64::try_from(floor.tokens) else {
            continue;
        };
        let on_disk = current_context_tokens(&root, target);
        if on_disk == Some(want) {
            continue;
        }
        let opts = ctx.with_window(&root, target, opts);
        match apply_takeover(&root, target, &base, &token, &unique_stamp(), &state.backups, opts) {
            Ok(_) => log.push(format!(
                "{} 上下文窗口 {} → {}（{}，压缩在 {} 触发）",
                target.label(),
                on_disk
                    .and_then(|n| u64::try_from(n).ok())
                    .map(fmt_tokens)
                    .unwrap_or_else(|| "未写".into()),
                fmt_tokens(floor.tokens),
                describe_floor(&floor),
                fmt_tokens(floor.compact_tokens),
            )),
            Err(e) => log.push(format!("{} 窗口未能同步：{e}", target.label())),
        }
    }
    if log.is_empty() {
        log.push("各 CLI 的上下文窗口已是最新，无需改动".into());
    }
    log
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
    Ok(state.settings.read().await.context_policy.clone())
}

/// 改总控。**只存策略，不写 CLI** —— 真正落盘要用户去点每家的「写入」，和这一
/// 页其它开关一个规矩（配置是用户的，我们不背着他改）。返回值是「有几家已接管
/// 的 CLI 还没按新策略重写」，前端拿它提示要不要去点写入。
#[tauri::command]
pub async fn context_policy_set(
    state: State<'_, AppState>,
    policy: ContextPolicy,
) -> AppResult<usize> {
    let mut policy = policy;
    // 界面上夹在 50–95；IPC 这一层再挡一次：0 和 100 写进 CLI 分别是「立刻压缩」
    // 和「永不压缩」，都不是用户想要的。
    if !(1..=99).contains(&policy.compact_percent) {
        return Err(AppError::Config(format!(
            "压缩百分比 {} 不在 1–99 之间",
            policy.compact_percent
        ))
        .into());
    }
    // 分档表里的空名字和 0 都是「没填」，不存。
    policy
        .overrides
        .retain(|k, v| !k.trim().is_empty() && *v > 0);
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
/// 和 `cli_apply` 走同一条路 —— 显示和实际写入必须是同一个数，
/// 否则又回到「界面说 200k、实际 1M」那种谁也不信谁的状态。
#[tauri::command]
pub async fn context_window_preview(state: State<'_, AppState>) -> AppResult<Vec<WindowPreview>> {
    let root = state.config_root().await?;
    let ctx = WindowContext::load(&state).await;
    let mut out = Vec::new();
    for target in TARGETS {
        let opts = TakeoverOptions::default();
        let model = current_model(&root, target);
        let floor = ctx.floor_for_target(&root, target, &opts);
        out.push(WindowPreview {
            target,
            source: model.as_deref().map(|m| ctx.policy.source_of(m)),
            model,
            tokens: floor.as_ref().and_then(|f| i64::try_from(f.tokens).ok()),
            compact_tokens: floor
                .as_ref()
                .and_then(|f| i64::try_from(f.compact_tokens).ok()),
            narrowest: floor.as_ref().and_then(|f| f.narrowest.clone()),
            candidates: floor.as_ref().map(|f| f.candidates.clone()).unwrap_or_default(),
            capped: floor.as_ref().is_some_and(|f| f.capped),
            on_disk: current_context_tokens(&root, target),
            kernel_checked: ctx.complete(),
        });
    }
    Ok(out)
}

/// 分档表：每个「五家 CLI 可能发到或落到」的模型一行，加上用户手填的那些。
///
/// 每行同时给「现在生效的窗口」和「不手填时自动会算成多少」，界面上「恢复自动」
/// 才知道要回到哪个数。同一个模型的不同写法（`claude-opus-5[1M]`、
/// `anthropic/claude-opus-5`）合成一行，因为分档表的键就是这么匹配的。
#[tauri::command]
pub async fn context_tiers(state: State<'_, AppState>) -> AppResult<Vec<TierRow>> {
    let root = state.config_root().await?;
    let ctx = WindowContext::load(&state).await;
    let mut rows: Vec<TierRow> = Vec::new();
    let add = |rows: &mut Vec<TierRow>, name: &str, target: Option<CliTarget>| {
        let key = tier_key(name);
        if key.is_empty() {
            return;
        }
        if let Some(row) = rows.iter_mut().find(|r| tier_key(&r.model) == key) {
            if let Some(t) = target {
                if !row.used_by.contains(&t) {
                    row.used_by.push(t);
                }
            }
            return;
        }
        let window = ctx.policy.window_of(name);
        rows.push(TierRow {
            model: name.to_string(),
            window,
            source: ctx.policy.source_of(name),
            auto_window: crate::services::context_window::parse_window(name),
            auto_source: crate::services::context_window::window_source(name),
            compact_tokens: ctx.policy.compact_tokens(window),
            used_by: target.into_iter().collect(),
        });
    };
    for target in TARGETS {
        if let Some(model) = current_model(&root, target) {
            // 不参与取最窄的虚拟别名（`gb-review` 这种）不列：它那个 128k 的兜底数
            // 摆在表里，看着像这家 CLI 会被写成 128k。手填过的会因为 Manual 而算数。
            for c in ctx.candidates_for(&model).into_iter().filter(|c| c.counted) {
                add(&mut rows, &c.model, Some(target));
            }
        }
    }
    let manual: Vec<String> = ctx.policy.overrides.keys().cloned().collect();
    for name in &manual {
        add(&mut rows, name, None);
    }
    rows.sort_by_key(|r| tier_key(&r.model));
    Ok(rows)
}

/// 某个别名在内核里会落到哪些渠道，按优先级从高到低。
///
/// 这是「Grok Build 选了 grok-4.6 却一直在跑 glm」那类问题的诊断面：内核只按
/// 渠道优先级选路，一个高优先级渠道上多了一条 `grok-4.6 → glm-5.3-flash` 的改写，
/// 真正的 xAI 渠道就永远排在后面。这里把落点顺序摆出来，界面上才能指着第一条说
/// 「请求默认去这儿」。内核没连上直接报错 —— 这个面板没有「本地算」的退路。
#[tauri::command]
pub async fn alias_routes(state: State<'_, AppState>, alias: String) -> AppResult<Vec<RouteHit>> {
    let alias = alias.trim().to_string();
    if alias.is_empty() {
        return Ok(Vec::new());
    }
    let routes = fetch_kernel_routes(&state)
        .await
        .ok_or_else(|| AppError::Config("内核没连上，读不到渠道清单".into()))?;
    Ok(routes.hits(&alias))
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
    /// 自动压缩触发点（`tokens × 百分比`）。
    pub compact_tokens: Option<i64>,
    /// `model` 自己的窗口是哪来的：手填 / 模型名后缀 / models.dev / 内置猜测表。
    pub source: Option<WindowSource>,
    /// 磁盘上**现在**写着的数。和 `tokens` 不一致就说明还没写入（或者是存量旧值）。
    pub on_disk: Option<i64>,
    /// 定下 `tokens` 的那个落点。
    pub narrowest: Option<Candidate>,
    /// 全部可能的落点。
    pub candidates: Vec<Candidate>,
    /// 上限夹子生效了。
    pub capped: bool,
    /// false = 内核没连上，只按本地的链和路由算，可能偏宽。
    pub kernel_checked: bool,
}

#[derive(serde::Serialize)]
pub struct TierRow {
    pub model: String,
    /// 现在生效的窗口（手填优先）。
    pub window: u64,
    pub source: WindowSource,
    /// 不手填时自动会算成多少。
    pub auto_window: u64,
    pub auto_source: WindowSource,
    pub compact_tokens: u64,
    /// 哪几家 CLI 可能发到 / 落到它。空 = 只是用户手填的一行。
    pub used_by: Vec<CliTarget>,
}
