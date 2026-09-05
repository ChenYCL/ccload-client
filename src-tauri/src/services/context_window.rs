//! 模型窗口：从名字读出真实上限，写进 CLI，压缩时按这个数挑模型。
//!
//! Claude Code 按**模型名声明的窗口**决定何时 `/compact`。走 ccLoad 时真正拦你
//! 的是**这一跳上游的上限**。名字挂着 `[1m]`、上游其实 500k，阈值就被算在一个
//! 不存在的分母上 —— 等它触发已经越过天花板，而 `/compact` 自己也要把整段发出
//! 去，从此死锁。
//!
//! 所以：
//! * 写进 CLI 的窗口取**这个别名在内核里所有可能落到的上游**里最窄的那个
//!   （模型本身、模型链每一跳、强制路由每个目标、内核里服务这个别名的每个渠道），
//!   见 [`crate::services::context_floor`]。CLI 的窗口是启动时读的静态值，没法在
//!   内核把请求分流到窄模型的那一刻再改，只能事先按最窄的写；
//! * 压缩触发点是窗口的一个百分比（默认 90%），窗口跟着落点变窄，阈值自然跟着降
//!   （1M → 900k，500k → 450k）；
//! * 名字推断不准的模型（本地 qwen、中转自起的别名）由用户在「分档」表里手填，
//!   手填盖过名字后缀、models.dev 和内置猜测表；
//! * 已经撑爆、原生 compact 自己也发不出去时，按链从前往后挑**窗口够的**那一
//!   跳做分块总结。原生优先，撑不住再往后。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::services::fallback::FallbackHop;

/// 分块总结时给压缩请求自己留的余量：请求本身也占上下文，顶着上限做不成任何事。
pub const COMPACT_HEADROOM: u64 = 80_000;

/// 压缩触发点默认在窗口的 90%。用户口径就是「九成」；再往上留不出压缩请求
/// 自己的空间，再往下白扔可用窗口。
pub const DEFAULT_COMPACT_PERCENT: u8 = 90;

/// Claude Code 的 `CLAUDE_CODE_AUTO_COMPACT_WINDOW` 官方接受区间。
pub const CLAUDE_COMPACT_WINDOW_MIN: u64 = 100_000;
pub const CLAUDE_COMPACT_WINDOW_MAX: u64 = 1_000_000;

/// 上下文窗口总控的三档。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextMode {
    /// 一个字都不写，各 CLI 保持现状。老行为，给不想让我们碰这个键的人。
    Off,
    /// 按模型名推断，并对这个别名所有可能落到的上游取最窄。默认。
    Auto,
    /// 不管选了什么模型，一律写 `fixed_tokens`。
    Fixed,
}

/// 上下文窗口**总控**。
///
/// 为什么需要一个总控：窗口这件事在五家 CLI 里是五个不同的键（Claude 的
/// `CLAUDE_CODE_MAX_CONTEXT_TOKENS`、Codex 的 `model_context_window`、
/// OpenCode 的 `limit.context`、Grok 目录项的 context…），过去只有「模型导入」
/// 那条路会写，接管页换模型**不写**。于是出现这个症状：模型从 200k 的换成 1M
/// 的，CLI 里还留着上次导入写下的 200k —— 界面显示 200k，实际能吃 1M，早压缩
/// 五分之四的可用窗口。现在接管写入时按这里的策略一次落到所有支持的 CLI。
///
/// 容器级 `#[serde(default)]`：老 settings.json 里没有 `compact_percent` /
/// `overrides` 时要拿到 `Default` 里的 90 和空表，而不是字段类型的 0。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ContextPolicy {
    pub mode: ContextMode,
    /// `Fixed` 档写的数。
    pub fixed_tokens: u64,
    /// `Auto` 推断结果的上限；0 = 不夹。
    ///
    /// 给「模型名声称 1M、但我这条中转其实只给 500k」的人用 —— 按名字写 1M 会
    /// 让 CLI 一路撑到打不出去为止，那正是模块开头讲的死锁。
    pub cap_tokens: u64,
    /// 自动压缩在窗口的百分之几触发。0 当作默认值。
    pub compact_percent: u8,
    /// 手动分档：模型名 → 窗口。盖过名字后缀、models.dev 和内置猜测表。
    ///
    /// 键按「去掉厂商前缀和 `[..]` 后缀、忽略大小写」匹配，所以填 `qwen3.8-27b`
    /// 能对上 `local/Qwen3.8-27B[200k]`。
    pub overrides: BTreeMap<String, u64>,
}

impl Default for ContextPolicy {
    fn default() -> Self {
        Self {
            mode: ContextMode::Auto,
            fixed_tokens: 1_000_000,
            cap_tokens: 0,
            compact_percent: DEFAULT_COMPACT_PERCENT,
            overrides: BTreeMap::new(),
        }
    }
}

impl ContextPolicy {
    /// 生效的压缩百分比。0 和越界值都不该写进任何 CLI —— 0% 等于每条请求都压缩，
    /// 100% 等于永远不压缩。
    pub fn percent(&self) -> u8 {
        // 100 = 永不压缩，和 0 一样不是一个能写进 CLI 的数。
        if self.compact_percent == 0 || self.compact_percent >= 100 {
            DEFAULT_COMPACT_PERCENT
        } else {
            self.compact_percent
        }
    }

    /// 窗口 × 百分比 = 自动压缩触发点。
    pub fn compact_tokens(&self, window: u64) -> u64 {
        window.saturating_mul(u64::from(self.percent())) / 100
    }

    /// 用户在分档表里给这个名字填的窗口。
    pub fn override_for(&self, name: &str) -> Option<u64> {
        let want = tier_key(name);
        if want.is_empty() {
            return None;
        }
        self.overrides
            .iter()
            .find(|(k, v)| **v > 0 && tier_key(k) == want)
            .map(|(_, v)| *v)
    }

    /// 这个名字的窗口：手填 → 名字后缀 → models.dev → 内置猜测表。
    pub fn window_of(&self, name: &str) -> u64 {
        self.override_for(name).unwrap_or_else(|| parse_window(name))
    }

    /// 这个窗口是哪来的，和 [`Self::window_of`] 同一套优先级。
    pub fn source_of(&self, name: &str) -> WindowSource {
        if self.override_for(name).is_some() {
            WindowSource::Manual
        } else {
            window_source(name)
        }
    }

    /// 只看**一个**模型名时该写多少。`None` = 不写（Off 档，或者 Auto 档但没有
    /// 模型名）。多个落点取最窄是 [`crate::services::context_floor`] 的事。
    pub fn resolve(&self, model: &str) -> Option<u64> {
        match self.mode {
            ContextMode::Off => None,
            ContextMode::Fixed => (self.fixed_tokens > 0).then_some(self.fixed_tokens),
            ContextMode::Auto => {
                let m = model.trim();
                if m.is_empty() {
                    return None;
                }
                Some(self.cap(self.window_of(m)))
            }
        }
    }

    /// 上限夹子。只往下夹，不往上抬。
    pub fn cap(&self, window: u64) -> u64 {
        if self.cap_tokens > 0 {
            window.min(self.cap_tokens)
        } else {
            window
        }
    }
}

/// 分档表键的归一化：去厂商前缀、去 `[..]` 后缀、小写。分档表和界面上的行都按
/// 它去重 —— `claude-opus-5[1M]`、`anthropic/claude-opus-5` 和 `Claude-Opus-5` 是
/// 同一档。
pub fn tier_key(name: &str) -> String {
    let bare = strip_vendor(name.trim());
    let bare = match (bare.rfind('['), bare.ends_with(']')) {
        (Some(i), true) => &bare[..i],
        _ => bare,
    };
    bare.trim().to_ascii_lowercase()
}

/// Claude Code 的 auto-compact 窗口是个全局整数，官方区间 `[100000, 1000000]`。
///
/// 它是压缩阈值的**分母**，不是阈值本身 —— 百分比在 `CLAUDE_AUTOCOMPACT_PCT_OVERRIDE`
/// 里另写。窄到官方下限以下的窗口写不进去，那就不写这个键，靠
/// `CLAUDE_CODE_MAX_CONTEXT_TOKENS` + 百分比撑着。
pub fn claude_auto_compact_window(context: u64) -> Option<u64> {
    (context >= CLAUDE_COMPACT_WINDOW_MIN).then_some(context.min(CLAUDE_COMPACT_WINDOW_MAX))
}

/// 从模型名读窗口。
///
/// 三段优先级，越靠前越可信：
///   1. 名字末尾的 `[1m]` / `[500k]` —— 上游自己挂上去的声明，最准；
///   2. **models.dev 目录**（[`crate::services::model_catalog`]）—— 第三方数据，
///      离线或查不到时才轮到下一档；
///   3. 家族猜测 —— 关键字匹配，兜底用。
///
/// 用户手填的分档不在这里，在 [`ContextPolicy::window_of`] —— 这个函数是「不看
/// 设置、只看名字」的纯函数，给分档表显示「自动会算成多少」用。
///
/// 为什么不只靠第 3 档：模型迭代比这张表快。拿 models.dev 的第一方数据对过一遍，
/// 134 条里 64 条对不上 —— 低估只是浪费窗口（`claude-sonnet-4-5` 我们估 200k、
/// 实际 1M，这就是「界面 200k 实际 1M」的来历），高估会直接死锁（`gpt-5` 估 1M、
/// 实际 400k）。所以猜测表只在没有更好来源时用，而且宁可估小。
pub fn parse_window(name: &str) -> u64 {
    let bare = strip_vendor(name);
    if let Some(n) = suffix_window(bare) {
        return n;
    }
    if let Some(n) = crate::services::model_catalog::lookup(bare) {
        return n;
    }
    family_window(bare)
}

/// 这个窗口是哪来的。界面上要能一眼看出「这数字是第三方查的还是我们猜的」。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowSource {
    /// 用户在分档表里手填的。
    Manual,
    /// 模型名自己带的 `[1m]` / `[500k]`。
    Suffix,
    /// models.dev。
    Catalog,
    /// 内置家族猜测表。
    Preset,
}

pub fn window_source(name: &str) -> WindowSource {
    let bare = strip_vendor(name);
    if suffix_window(bare).is_some() {
        WindowSource::Suffix
    } else if crate::services::model_catalog::lookup(bare).is_some() {
        WindowSource::Catalog
    } else {
        WindowSource::Preset
    }
}

fn strip_vendor(name: &str) -> &str {
    match name.rsplit_once('/') {
        Some((_, rest)) if !rest.is_empty() => rest,
        _ => name,
    }
}

/// `[1m]`、`[500k]`、`[200000]`。只认名字**末尾**那一组方括号，免得误伤
/// `model[beta]-v1`。
fn suffix_window(name: &str) -> Option<u64> {
    let start = name.rfind('[')?;
    if !name.ends_with(']') {
        return None;
    }
    parse_size(&name[start + 1..name.len() - 1])
}

fn parse_size(raw: &str) -> Option<u64> {
    let s = raw.trim().to_ascii_lowercase().replace(['_', ','], "");
    if s.is_empty() {
        return None;
    }
    let (num, mul) = if let Some(n) = s.strip_suffix('m') {
        (n, 1_000_000u64)
    } else if let Some(n) = s.strip_suffix('k') {
        (n, 1_000)
    } else {
        (s.as_str(), 1)
    };
    let n: f64 = num.parse().ok()?;
    if n <= 0.0 {
        return None;
    }
    Some((n * mul as f64) as u64)
}

/// 兜底猜测。**只在 models.dev 查不到时用**，所以规则宁可保守：估小了最多提前
/// compact，估大了会死锁。下面每条都拿 models.dev 的第一方数据核对过。
fn family_window(name: &str) -> u64 {
    let n = name.to_ascii_lowercase();
    // grok-4.6/4.5 实测卡在 500k；4.2x/4.3 那批是 1M；grok-build 是 256k。
    // 必须按从具体到宽泛的顺序，不然全被后面的 `grok` 一把吃成 256k。
    if n.contains("grok-4.6") || n.contains("grok-4.5") || n.contains("grok-4-6") {
        return 500_000;
    }
    if n.contains("grok-4.2") || n.contains("grok-4.3") {
        return 1_000_000;
    }
    if n.contains("grok") {
        return 256_000;
    }
    // v4 才是 1M。v3 系列是 128k~164k —— 以前这两条并在一起写成 1M，是**高估**，
    // 也就是会死锁的那个方向。
    if n.contains("deepseek-v4") {
        return 1_000_000;
    }
    if n.contains("deepseek") {
        return 128_000;
    }
    // glm-5.2/5.3 上游标的是 1M，5.0/5.1 仍是 200k 档，4.5 那批只有 131k。
    if n.contains("glm-5.2") || n.contains("glm-5.3") {
        return 1_000_000;
    }
    if n.contains("glm-4.5") {
        return 131_072;
    }
    if n.contains("glm") {
        return 200_000;
    }
    if n.contains("kimi-k3") {
        return 1_000_000;
    }
    if n.contains("kimi") {
        return 262_144;
    }
    if n.contains("gemini") {
        return 1_000_000;
    }
    if n.contains("gpt-4.1") {
        return 1_000_000;
    }
    // gpt-5 是 400k，不是 1M —— 5.4 起才回到 1M 档。以前一刀切 1M 是高估。
    if n.contains("gpt-5.4") || n.contains("gpt-5.5") || n.contains("gpt-5.6") {
        return 1_000_000;
    }
    if n.contains("gpt-5") {
        return 400_000;
    }
    if n.contains("o1") || n.contains("o3") || n.contains("o4-mini") {
        return 200_000;
    }
    // Claude 家族：haiku 和 opus-4.5 是 200k，其余（sonnet-4.5 起、4.6+、
    // opus-5 / fable-5 / sonnet-5）都是 1M。
    //
    // 注意 **sonnet-4.5 是 1M**：以前这里把整个 `-4-5` 都判成 200k，于是
    // sonnet-4.5 被压掉五分之四的可用窗口 —— 「界面显示 200k、实际是 1M」
    // 说的就是它。
    if n.contains("claude") || n.contains("opus") || n.contains("sonnet") || n.contains("fable") {
        if n.contains("haiku") {
            return 200_000;
        }
        if n.contains("opus") && (n.contains("-4-5") || n.contains("-4.5")) {
            return 200_000;
        }
        return 1_000_000;
    }
    128_000
}

/// 链上每一跳的窗口（按总控里的分档算）。空跳跳过。
pub fn hop_windows(hops: &[FallbackHop], policy: &ContextPolicy) -> Vec<(String, u64)> {
    hops.iter()
        .map(|h| h.upstream.trim())
        .filter(|s| !s.is_empty())
        .map(|s| (s.to_string(), policy.window_of(s)))
        .collect()
}

/// 已经撑爆时，按链从前往后挑窗口够的模型。原生（链头）优先。
///
/// `needed` 是会话当前真实上下文。候选是 hop 的 **upstream 名** —— 压缩请求
/// 打这个名字，才不会再被别名路由到一条窗口不够的渠道上（上次 842k 打 grok
/// 就是因为请求的还是 `claude-opus-5` 这个别名）。
pub fn pick_compact_models(needed: u64, hops: &[FallbackHop], policy: &ContextPolicy) -> Vec<String> {
    let want = needed.saturating_add(COMPACT_HEADROOM);
    let mut out = Vec::new();
    for (name, w) in hop_windows(hops, policy) {
        if w >= want && !out.iter().any(|s| s == &name) {
            out.push(name);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suffix_1m_wins_over_family() {
        assert_eq!(parse_window("glm-5.3[1m]"), 1_000_000);
        assert_eq!(parse_window("claude-opus-5[1m]"), 1_000_000);
        assert_eq!(parse_window("deepseek-v4-flash[1m]"), 1_000_000);
    }

    #[test]
    fn grok_46_is_500k_not_the_family_default() {
        assert_eq!(parse_window("grok-4.6"), 500_000);
        assert_eq!(parse_window("grok-4.5"), 500_000);
        assert_eq!(parse_window("grok-3"), 256_000);
    }

    #[test]
    fn vendor_prefix_is_stripped_before_matching() {
        assert_eq!(parse_window("xai/grok-4.6"), 500_000);
        assert_eq!(parse_window("zhipu/glm-5.3[1m]"), 1_000_000);
    }

    #[test]
    fn k_and_raw_numbers() {
        assert_eq!(parse_window("whatever[500k]"), 500_000);
        assert_eq!(parse_window("whatever[200000]"), 200_000);
        assert_eq!(parse_window("whatever[0.2m]"), 200_000);
    }

    /// 上游 4.6 起给到 1M，4.5 及更早、以及 haiku 仍是 200k。整族按 200k 估会
    /// 让 1M 的模型提前四倍 compact。
    #[test]
    fn claude_family_splits_at_4_6() {
        assert_eq!(parse_window("claude-opus-4-8"), 1_000_000);
        assert_eq!(parse_window("claude-opus-5"), 1_000_000);
        assert_eq!(parse_window("claude-fable-5"), 1_000_000);
        assert_eq!(parse_window("claude-sonnet-4-6"), 1_000_000);
        assert_eq!(parse_window("claude-opus-4-5-20251101"), 200_000);
        assert_eq!(parse_window("claude-haiku-4-5-20251001"), 200_000);
    }

    /// **sonnet-4.5 是 1M**，不是 200k。以前这里把整个 `-4-5` 一刀切成 200k，
    /// 于是它被压掉五分之四的可用窗口 —— 用户报的「界面显示 200k、实际是 1M」
    /// 说的就是它。models.dev 的第一方数据：claude-sonnet-4-5 = 1,000,000。
    #[test]
    fn sonnet_45_is_1m_only_opus_and_haiku_are_capped_at_200k() {
        assert_eq!(parse_window("claude-sonnet-4-5"), 1_000_000);
        assert_eq!(parse_window("claude-sonnet-4-5-20250929"), 1_000_000);
        assert_eq!(parse_window("claude-opus-4-5"), 200_000);
        assert_eq!(parse_window("claude-haiku-4-5"), 200_000);
    }

    /// 高估是会死锁的那个方向，这几条以前全是高估。数字取自 models.dev 第一方。
    #[test]
    fn the_presets_no_longer_overestimate() {
        // gpt-5 是 400k，不是 1M。
        assert_eq!(parse_window("gpt-5"), 400_000);
        assert_eq!(parse_window("gpt-5.2-pro"), 400_000);
        // 5.4 起才回到 1M 档。
        assert_eq!(parse_window("gpt-5.5"), 1_000_000);
        // deepseek 只有 v4 是 1M；v3 系列是 128k 档，以前和 v4 并成了一条。
        assert_eq!(parse_window("deepseek-v4-flash"), 1_000_000);
        assert_eq!(parse_window("deepseek-v3.2"), 128_000);
        // glm-4.5 那批只有 131k，不是 200k。
        assert_eq!(parse_window("glm-4.5-air"), 131_072);
    }

    /// 低估只是浪费窗口，但也是错的。
    #[test]
    fn the_presets_no_longer_underestimate_the_wide_ones() {
        assert_eq!(parse_window("grok-4.3"), 1_000_000);
        assert_eq!(parse_window("grok-4.20-0309-reasoning"), 1_000_000);
        assert_eq!(parse_window("kimi-k3"), 1_000_000);
        assert_eq!(parse_window("o3-pro"), 200_000);
        // grok-build 仍然是 256k 档，别被上面那条 4.2x 规则带跑。
        assert_eq!(parse_window("grok-build-0.1"), 256_000);
    }

    /// 目录（第三方数据）必须**盖过**猜测表，而名字自带的后缀又盖过目录 ——
    /// 这个顺序错了，用户在名字里写 `[500k]` 的意图就会被目录悄悄推翻。
    #[test]
    fn catalog_outranks_presets_but_the_suffix_outranks_everything() {
        crate::services::model_catalog::set_for_test(&[("mystery-model", 777_000)]);
        // 猜测表根本不认识它，只会给 128k 兜底；目录说 777k。
        assert_eq!(parse_window("mystery-model"), 777_000);
        assert_eq!(
            window_source("mystery-model"),
            WindowSource::Catalog
        );
        // 名字上挂了声明，以声明为准。
        assert_eq!(parse_window("mystery-model[200k]"), 200_000);
        assert_eq!(
            window_source("mystery-model[200k]"),
            WindowSource::Suffix
        );
        crate::services::model_catalog::clear_for_test();
        // 目录没了就回落猜测表，不会 panic 也不会给 0。
        assert_eq!(parse_window("mystery-model"), 128_000);
        assert_eq!(window_source("mystery-model"), WindowSource::Preset);
    }

    /// 渠道别名是人手填的，带空格和括号 —— 这些名字连 `claude` 都匹配不上时
    /// 会掉进 128k 兜底，UI 上就显示成 128000。
    #[test]
    fn human_written_aliases_still_resolve() {
        assert_eq!(parse_window("Fable 5"), 1_000_000);
        assert_eq!(parse_window("Opus 4.8 (1M context)"), 1_000_000);
    }

    #[test]
    fn glm_53_is_1m_not_the_200k_family_default() {
        assert_eq!(parse_window("glm-5.3"), 1_000_000);
        assert_eq!(parse_window("glm-5.3-flash"), 1_000_000);
        assert_eq!(parse_window("glm-5.1"), 200_000);
        assert_eq!(parse_window("glm-4.6"), 200_000);
    }

    /// 总控三档的行为。Auto 按名字推断，Fixed 一律照写，Off 什么都不写。
    #[test]
    fn policy_modes_do_what_they_say() {
        let auto = ContextPolicy::default();
        assert_eq!(auto.mode, ContextMode::Auto, "默认必须是自动，不是不写");
        assert_eq!(auto.resolve("claude-opus-5"), Some(1_000_000));
        assert_eq!(auto.resolve("grok-4.6"), Some(500_000));
        // 没模型名就没得推断 —— 硬写一个默认值等于替用户瞎猜。
        assert_eq!(auto.resolve("  "), None);

        let fixed = ContextPolicy {
            mode: ContextMode::Fixed,
            fixed_tokens: 500_000,
            ..Default::default()
        };
        assert_eq!(fixed.resolve("claude-opus-5"), Some(500_000));
        assert_eq!(fixed.resolve(""), Some(500_000), "固定档不看模型名");

        let off = ContextPolicy {
            mode: ContextMode::Off,
            ..Default::default()
        };
        assert_eq!(off.resolve("claude-opus-5"), None);
    }

    /// 名字声称 1M、但这条中转其实只给 500k：夹子必须生效，否则就是模块开头
    /// 讲的那个死锁（等 compact 触发时早已越过真实天花板）。
    #[test]
    fn the_cap_clamps_auto_but_never_inflates() {
        let capped = ContextPolicy {
            mode: ContextMode::Auto,
            cap_tokens: 500_000,
            ..Default::default()
        };
        assert_eq!(capped.resolve("claude-opus-5[1m]"), Some(500_000));
        // 比夹子窄的不动 —— 夹子是上限，不是目标值。
        assert_eq!(capped.resolve("claude-haiku-4-5-20251001"), Some(200_000));
    }

    /// 0 是「没填」不是「不写」：固定档填 0 时不该写一个 0 进 CLI。
    #[test]
    fn zero_is_not_a_window() {
        let fixed = ContextPolicy {
            mode: ContextMode::Fixed,
            fixed_tokens: 0,
            ..Default::default()
        };
        assert_eq!(fixed.resolve("claude-opus-5"), None);
    }

    /// 手填的分档盖过一切 —— 包括名字上挂的 `[1m]`。本地 qwen 的名字什么都推不
    /// 出来（128k 兜底），中转给的 `[1m]` 也可能是假的，用户亲手填的数才是他要的。
    #[test]
    fn manual_overrides_beat_suffix_catalog_and_presets() {
        crate::services::model_catalog::set_for_test(&[("mystery-model", 777_000)]);
        let mut p = ContextPolicy::default();
        p.overrides.insert("Qwen3.8-27B".into(), 200_000);
        p.overrides.insert("mystery-model".into(), 300_000);
        p.overrides.insert("claude-opus-5".into(), 500_000);

        // 大小写、厂商前缀、后缀都不影响匹配。
        assert_eq!(p.window_of("qwen3.8-27b"), 200_000);
        assert_eq!(p.window_of("local/Qwen3.8-27B[1m]"), 200_000);
        assert_eq!(p.source_of("qwen3.8-27b"), WindowSource::Manual);
        // 盖过目录。
        assert_eq!(p.window_of("mystery-model"), 300_000);
        // 盖过名字后缀。
        assert_eq!(p.window_of("claude-opus-5[1M]"), 500_000);
        // 没填的照旧走自动。
        assert_eq!(p.window_of("grok-4.6"), 500_000);
        assert_eq!(p.source_of("grok-4.6"), WindowSource::Preset);
        // 填 0 等于没填。
        p.overrides.insert("grok-4.6".into(), 0);
        assert_eq!(p.window_of("grok-4.6"), 500_000);
        crate::services::model_catalog::clear_for_test();
    }

    /// 压缩触发点 = 窗口 × 百分比。用户说的「九成」就是这个数：1M → 900k，
    /// 500k → 450k。0 和越界值退回默认，不能写出「每条都压缩」或「永不压缩」。
    #[test]
    fn compact_tokens_follow_the_percent() {
        let p = ContextPolicy::default();
        assert_eq!(p.percent(), 90);
        assert_eq!(p.compact_tokens(1_000_000), 900_000);
        assert_eq!(p.compact_tokens(500_000), 450_000);
        let p = ContextPolicy { compact_percent: 80, ..Default::default() };
        assert_eq!(p.compact_tokens(500_000), 400_000);
        let p = ContextPolicy { compact_percent: 0, ..Default::default() };
        assert_eq!(p.percent(), DEFAULT_COMPACT_PERCENT);
        let p = ContextPolicy { compact_percent: 250, ..Default::default() };
        assert_eq!(p.percent(), DEFAULT_COMPACT_PERCENT);
    }

    /// 老 settings.json 里没有新字段：要读成默认 90% 和空表，不能是 0% 。
    #[test]
    fn old_settings_without_the_new_fields_get_the_defaults() {
        let p: ContextPolicy =
            serde_json::from_str(r#"{"mode":"auto","fixed_tokens":1000000,"cap_tokens":0}"#).unwrap();
        assert_eq!(p.compact_percent, DEFAULT_COMPACT_PERCENT);
        assert!(p.overrides.is_empty());
    }

    /// auto-compact 窗口是 Claude Code 的全局整数，官方区间 [100k, 1M]。它是分母，
    /// 阈值靠百分比另写；窄到下限以下就不写这个键。
    #[test]
    fn claude_auto_compact_window_respects_the_official_range() {
        assert_eq!(claude_auto_compact_window(1_000_000), Some(1_000_000));
        assert_eq!(claude_auto_compact_window(2_000_000), Some(1_000_000));
        assert_eq!(claude_auto_compact_window(500_000), Some(500_000));
        assert_eq!(claude_auto_compact_window(100_000), Some(100_000));
        assert_eq!(claude_auto_compact_window(99_999), None);
        assert_eq!(claude_auto_compact_window(0), None);
    }

    fn hop(up: &str) -> FallbackHop {
        FallbackHop {
            upstream: up.into(),
            channel_id: None,
            channel_name: None,
        }
    }

    /// 842k 的会话：haiku-4.5 200k / grok 500k 都不够，glm-5.3 1M 和 deepseek 1M
    /// 够。顺序跟链走，原生在前。
    #[test]
    fn pick_skips_hops_that_cannot_hold_the_session() {
        let hops = vec![
            hop("claude-haiku-4-5-20251001"),
            hop("glm-5.3[1m]"),
            hop("grok-4.6"),
            hop("deepseek-v4-flash"),
        ];
        let got = pick_compact_models(842_000, &hops, &ContextPolicy::default());
        assert_eq!(got, vec!["glm-5.3[1m]", "deepseek-v4-flash"]);
    }

    /// 分块总结按**一块**的大小挑模型，不是按整段会话。
    ///
    /// 模块开头那个场景：517k 的会话、链上最宽 500k。按整段挑 → 一跳都不够
    /// （517k+80k > 500k），用户看到「没有窗口够的一跳」；可每块只有 120k，
    /// 连 200k 的 haiku 都装得下。分块存在的理由就是整段发不出去。
    #[test]
    fn a_chunk_sized_need_finds_hops_the_whole_session_never_would() {
        let hops = vec![
            hop("claude-haiku-4-5-20251001"), // 200k
            hop("grok-4.6"),                  // 500k
        ];
        let p = ContextPolicy::default();
        // 整段口径：一个都挑不出来。
        assert!(pick_compact_models(517_000, &hops, &p).is_empty());
        // 分块口径（120k + 80k 余量 = 200k）：两跳都够。
        assert_eq!(
            pick_compact_models(120_000, &hops, &p),
            vec!["claude-haiku-4-5-20251001", "grok-4.6"]
        );
    }

    /// 400k：haiku-4.5 200k 仍不够，glm 1M 够，grok 500k 也够（400k+80k=480k < 500k）。
    #[test]
    fn pick_keeps_later_hops_that_still_fit() {
        let hops = vec![
            hop("claude-haiku-4-5-20251001"),
            hop("glm-5.3[1m]"),
            hop("grok-4.6"),
        ];
        let got = pick_compact_models(400_000, &hops, &ContextPolicy::default());
        assert_eq!(got, vec!["glm-5.3[1m]", "grok-4.6"]);
    }

    /// 挑压缩模型也要认分档表：用户说 grok 这条中转其实只有 300k，那 400k 的块
    /// 就不该发给它。
    #[test]
    fn pick_honours_manual_overrides() {
        let hops = vec![hop("glm-5.3[1m]"), hop("grok-4.6")];
        let mut p = ContextPolicy::default();
        p.overrides.insert("grok-4.6".into(), 300_000);
        assert_eq!(pick_compact_models(400_000, &hops, &p), vec!["glm-5.3[1m]"]);
    }
}
