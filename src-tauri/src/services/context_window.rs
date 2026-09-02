//! 模型窗口：从名字读出真实上限，写进 CLI，压缩时按这个数挑模型。
//!
//! Claude Code 按**模型名声明的窗口**决定何时 `/compact`。走 ccLoad 时真正拦你
//! 的是**这一跳上游的上限**。名字挂着 `[1m]`、上游其实 500k，阈值就被算在一个
//! 不存在的分母上 —— 等它触发已经越过天花板，而 `/compact` 自己也要把整段发出
//! 去，从此死锁。
//!
//! 所以：
//! * 应用模型链时，把链上**最窄**那一跳的窗口写进
//!   `CLAUDE_CODE_MAX_CONTEXT_TOKENS`，让 CLI 的原生 compact 按真实天花板提前
//!   动手；
//! * 已经撑爆、原生 compact 自己也发不出去时，按链从前往后挑**窗口够的**那一
//!   跳做分块总结。原生优先，撑不住再往后。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::AppError;
use crate::services::cli_backup::BackupStore;
use crate::services::cli_config::current_endpoint;
use crate::services::cli_io::{object_at, read_json, write_pretty_json};
use crate::services::cli_types::{CliTarget, ConfigRoot};
use crate::services::fallback::FallbackHop;

/// 压缩请求本身也占上下文，顶着上限做不成任何事。给挑模型留的余量。
pub const COMPACT_HEADROOM: u64 = 80_000;

/// 上下文窗口总控的三档。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextMode {
    /// 一个字都不写，各 CLI 保持现状。老行为，给不想让我们碰这个键的人。
    Off,
    /// 按当前选中的模型名推断（`parse_window`）。默认。
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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
}

impl Default for ContextPolicy {
    fn default() -> Self {
        Self {
            mode: ContextMode::Auto,
            fixed_tokens: 1_000_000,
            cap_tokens: 0,
        }
    }
}

impl ContextPolicy {
    /// 这次接管该写多少。`None` = 不写（Off 档，或者 Auto 档但没有模型名）。
    pub fn resolve(&self, model: &str) -> Option<u64> {
        match self.mode {
            ContextMode::Off => None,
            ContextMode::Fixed => (self.fixed_tokens > 0).then_some(self.fixed_tokens),
            ContextMode::Auto => {
                let m = model.trim();
                if m.is_empty() {
                    return None;
                }
                let w = parse_window(m);
                Some(if self.cap_tokens > 0 {
                    w.min(self.cap_tokens)
                } else {
                    w
                })
            }
        }
    }
}

/// Claude Code 的 auto-compact 窗口是个全局整数，官方区间 `[100000, 1000000]`。
/// 给压缩请求本身留出余量；余量扣完低于官方下限就干脆不写这个键。
pub fn auto_compact_window(context: i64) -> Option<i64> {
    if context <= 0 {
        return None;
    }
    let w = (context as u64).saturating_sub(COMPACT_HEADROOM);
    (w >= 100_000).then_some(w.min(1_000_000) as i64)
}

/// 从模型名读窗口。
///
/// 先看后缀 `[1m]` / `[500k]` —— 这是上游自己挂在名字上的声明，比家族猜测准。
/// 没有后缀再按家族给一个保守默认。保守的意思是：估小了最多提前 compact，
/// 估大了会再次死锁。
pub fn parse_window(name: &str) -> u64 {
    let bare = strip_vendor(name);
    if let Some(n) = suffix_window(bare) {
        return n;
    }
    family_window(bare)
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

fn family_window(name: &str) -> u64 {
    let n = name.to_ascii_lowercase();
    // grok-4.6 实测卡在 500k，比家族默认的 256k 大、比 1M 小。必须钉死，
    // 不然按 grok 家族估会提前压，按 1M 估会再次死锁。
    if n.contains("grok-4.6") || n.contains("grok-4.5") || n.contains("grok-4-6") {
        return 500_000;
    }
    if n.contains("grok") {
        return 256_000;
    }
    if n.contains("deepseek-v4") || n.contains("deepseek-v3") {
        return 1_000_000;
    }
    if n.contains("deepseek") {
        return 128_000;
    }
    // glm-5.2/5.3 上游标的是 1M，5.0/5.1 仍是 200k 档。整个 glm-5 按 200k 估会
    // 把 5.3 压掉五分之四的可用窗口 —— 调度图里 flash 档常年挂的就是 5.3。
    if n.contains("glm-5.2") || n.contains("glm-5.3") {
        return 1_000_000;
    }
    if n.contains("glm") {
        return 200_000;
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
    if n.contains("gpt-5") {
        return 1_000_000;
    }
    // Claude 家族早就分了两档：4.5 及更早是 200k，4.6 起（含 opus-5 / fable-5 /
    // sonnet-5）是 1M。一刀切 200k 会让 1M 的模型提前四倍触发 compact。
    if n.contains("claude") || n.contains("opus") || n.contains("sonnet") || n.contains("fable") {
        if n.contains("haiku") || n.contains("-4-5") || n.contains("-4.5") {
            return 200_000;
        }
        return 1_000_000;
    }
    128_000
}

/// 链上每一跳的窗口。空跳跳过。
pub fn hop_windows(hops: &[FallbackHop]) -> Vec<(String, u64)> {
    hops.iter()
        .map(|h| h.upstream.trim())
        .filter(|s| !s.is_empty())
        .map(|s| (s.to_string(), parse_window(s)))
        .collect()
}

/// 链上最窄的那一跳。CLI 的全局窗口必须按这个写 —— 按最宽的写，落到窄的那一
/// 跳照样死锁。
pub fn chain_ceiling(hops: &[FallbackHop]) -> Option<u64> {
    hop_windows(hops).into_iter().map(|(_, w)| w).min()
}

/// 已经撑爆时，按链从前往后挑窗口够的模型。原生（链头）优先。
///
/// `needed` 是会话当前真实上下文。候选是 hop 的 **upstream 名** —— 压缩请求
/// 打这个名字，才不会再被别名路由到一条窗口不够的渠道上（上次 842k 打 grok
/// 就是因为请求的还是 `claude-opus-5` 这个别名）。
pub fn pick_compact_models(needed: u64, hops: &[FallbackHop]) -> Vec<String> {
    let want = needed.saturating_add(COMPACT_HEADROOM);
    let mut out = Vec::new();
    for (name, w) in hop_windows(hops) {
        if w >= want && !out.iter().any(|s| s == &name) {
            out.push(name);
        }
    }
    out
}

/// 把真实天花板写进 Claude Code。
///
/// Claude Code 的窗口是**全局一个键**，不是 per-model。所以只能按这条链最窄
/// 的那一跳写。没接管就跳过，不报错 —— 模型链本身不依赖 CLI 配置。
pub fn inject_claude_window(
    root: &ConfigRoot,
    backups: &BackupStore,
    stamp: &str,
    tokens: u64,
) -> Result<String, AppError> {
    if tokens == 0 {
        return Err(AppError::Config("窗口是 0，拒绝写入".into()));
    }
    if current_endpoint(root, CliTarget::ClaudeCode).is_none() {
        return Ok("Claude Code 还未接管，跳过窗口注入".into());
    }
    let path = root.join(".claude/settings.json");
    backups.snapshot(root, CliTarget::ClaudeCode, stamp, "inject-context-window")?;
    let mut doc = read_json(&path)?;
    {
        let env = object_at(&mut doc, "env")?;
        env.insert(
            "CLAUDE_CODE_MAX_CONTEXT_TOKENS".into(),
            Value::String(tokens.to_string()),
        );
    }
    write_pretty_json(&path, &doc)?;
    Ok(format!(
        "已把 Claude Code 的窗口写成 {tokens}（CLAUDE_CODE_MAX_CONTEXT_TOKENS）。原生 /compact 会按这个数提前动手。"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::cli_backup::BackupStore;
    use crate::services::cli_types::ConfigRoot;

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
            cap_tokens: 0,
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
            fixed_tokens: 0,
            cap_tokens: 500_000,
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
            cap_tokens: 0,
        };
        assert_eq!(fixed.resolve("claude-opus-5"), None);
    }

    /// auto-compact 是 Claude Code 的全局整数，官方区间 [100k, 1M]，还要扣掉
    /// 压缩请求自己的余量。扣完低于下限就不写这个键。
    #[test]
    fn auto_compact_respects_the_official_floor_and_ceiling() {
        assert_eq!(auto_compact_window(1_000_000), Some(920_000));
        assert_eq!(auto_compact_window(200_000), Some(120_000));
        // 180k - 80k = 100k，正好压在下限上，算够。
        assert_eq!(auto_compact_window(180_000), Some(100_000));
        // 再窄一点就没有余量可言了。
        assert_eq!(auto_compact_window(179_000), None);
        assert_eq!(auto_compact_window(0), None);
    }

    fn hop(up: &str) -> FallbackHop {
        FallbackHop {
            upstream: up.into(),
            channel_id: None,
            channel_name: None,
        }
    }

    /// 842k 的会话：sonnet-4.5 200k / grok 500k 都不够，glm-5.3 1M 和 deepseek 1M
    /// 够。顺序跟链走，原生在前。
    #[test]
    fn pick_skips_hops_that_cannot_hold_the_session() {
        let hops = vec![
            hop("claude-sonnet-4-5-20250929"),
            hop("glm-5.3[1m]"),
            hop("grok-4.6"),
            hop("deepseek-v4-flash"),
        ];
        let got = pick_compact_models(842_000, &hops);
        assert_eq!(got, vec!["glm-5.3[1m]", "deepseek-v4-flash"]);
    }

    /// 400k：sonnet-4.5 200k 仍不够，glm 1M 够，grok 500k 也够（400k+80k=480k < 500k）。
    #[test]
    fn pick_keeps_later_hops_that_still_fit() {
        let hops = vec![
            hop("claude-sonnet-4-5-20250929"),
            hop("glm-5.3[1m]"),
            hop("grok-4.6"),
        ];
        let got = pick_compact_models(400_000, &hops);
        assert_eq!(got, vec!["glm-5.3[1m]", "grok-4.6"]);
    }

    #[test]
    fn chain_ceiling_is_the_narrowest_hop() {
        let hops = vec![hop("glm-5.3[1m]"), hop("grok-4.6"), hop("deepseek-v4-flash")];
        assert_eq!(chain_ceiling(&hops), Some(500_000));
    }

    #[test]
    fn inject_skips_when_claude_is_not_taken_over() {
        let dir = tempfile::tempdir().unwrap();
        let root = ConfigRoot::sandbox(dir.path().to_path_buf());
        let bk = BackupStore::new(dir.path().join("bk"));
        let msg = inject_claude_window(&root, &bk, "s1", 500_000).unwrap();
        assert!(msg.contains("还未接管"), "{msg}");
        assert!(!root.join(".claude/settings.json").exists());
    }

    #[test]
    fn inject_writes_the_global_cap_and_snapshots() {
        let dir = tempfile::tempdir().unwrap();
        let root = ConfigRoot::sandbox(dir.path().to_path_buf());
        let bk = BackupStore::new(dir.path().join("bk"));
        let path = root.join(".claude/settings.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"env":{"ANTHROPIC_BASE_URL":"http://127.0.0.1:15722","CLAUDE_CODE_MAX_CONTEXT_TOKENS":"1000000"}}"#,
        )
        .unwrap();

        let msg = inject_claude_window(&root, &bk, "s1", 500_000).unwrap();
        assert!(msg.contains("500000"), "{msg}");
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            doc.pointer("/env/CLAUDE_CODE_MAX_CONTEXT_TOKENS").unwrap(),
            "500000"
        );
        assert_eq!(bk.list(Some(CliTarget::ClaudeCode)).unwrap().len(), 1);
    }
}
