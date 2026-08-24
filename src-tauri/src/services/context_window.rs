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

use serde_json::Value;

use crate::error::AppError;
use crate::services::cli_backup::BackupStore;
use crate::services::cli_config::current_endpoint;
use crate::services::cli_io::{object_at, read_json, write_pretty_json};
use crate::services::cli_types::{CliTarget, ConfigRoot};
use crate::services::fallback::FallbackHop;

/// 压缩请求本身也占上下文，顶着上限做不成任何事。给挑模型留的余量。
pub const COMPACT_HEADROOM: u64 = 80_000;

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
    if n.contains("glm-5") {
        return 200_000;
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
        return 400_000;
    }
    if n.contains("claude") || n.contains("opus") || n.contains("sonnet") || n.contains("fable") {
        return 200_000;
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

    fn hop(up: &str) -> FallbackHop {
        FallbackHop {
            upstream: up.into(),
            channel_id: None,
            channel_name: None,
        }
    }

    /// 842k 的会话：opus 200k / grok 500k 都不够，glm 1M 和 deepseek 1M 够。
    /// 顺序跟链走，原生在前。
    #[test]
    fn pick_skips_hops_that_cannot_hold_the_session() {
        let hops = vec![
            hop("claude-opus-5"),
            hop("glm-5.3[1m]"),
            hop("grok-4.6"),
            hop("deepseek-v4-flash"),
        ];
        let got = pick_compact_models(842_000, &hops);
        assert_eq!(got, vec!["glm-5.3[1m]", "deepseek-v4-flash"]);
    }

    /// 400k：opus 200k 仍不够，glm 1M 够，grok 500k 也够（400k+80k=480k < 500k）。
    #[test]
    fn pick_keeps_later_hops_that_still_fit() {
        let hops = vec![
            hop("claude-opus-5"),
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
