//! 出口别名：**我们**说了算的那一份模型名单，架在 CLI 和内核之间。
//!
//! # 为什么需要这一层
//!
//! 以前 CLI 里能选的模型名 == 内核渠道上的别名。于是三件事被焊死在一起：
//!
//!   1. 内核换个渠道、别名改个名字，所有 CLI 的配置当场失效；
//!   2. 上下文窗口只有一份全局口径（`ContextPolicy`），可 `grok-4.6` 是 500k、
//!      `claude-opus-5` 是 1M，写进 CLI 的却是同一个数或者同一套推断；
//!   3. 「这个模型给谁用」没法表达 —— 一次导入把 83 个别名同时推给 5 家 CLI，
//!      而 Claude Code 只有 5 个槽位。
//!
//! 客户端本来就是代理（`services::cli_proxy`，CLI 全部指向它）。代理转发前会按
//! `ProxyRules.rewrites` 改模型名 —— 那张表一直存在，却从来没有人往里写过。这个
//! 模块就是它的配置面：
//!
//! ```text
//!   CLI 配置里写 alias ──► 代理把 alias 换成 target ──► 内核按 target 选渠道
//!      ccload-fast              grok-4.6                  xAI 渠道
//! ```
//!
//! # 一条记录管三件事
//!
//! | 字段 | 落到哪 |
//! | --- | --- |
//! | `alias` | 写进各 CLI 的模型目录 / tier 槽位 —— 用户在 `/model` 里看到的就是它 |
//! | `target` | 代理的改写表（`alias → target`），也是窗口推断的依据 |
//! | `context_window` + `compact_percent` | 写进各 CLI 的窗口键和压缩阈值 |
//! | `targets` | 哪几家 CLI 要写它。Claude Code 5 个槽位，OpenCode 可以全给 |
//!
//! 窗口跟着 **target**（真正跑的那个模型）算，不跟着 alias：`ccload-fast` 这个名字
//! 什么都推不出来，而它背后的 `grok-4.6` 是 500k。阈值默认 90%，也就是 450k
//! 触发压缩；`claude-opus-5` 那条是 1M / 900k。这正是「按各自的上下文自动拆分」。
//!
//! # 硬约束：改名依赖代理
//!
//! `alias != target` 的记录**只在 CLI 走本地代理时成立**。直连内核的 CLI 发出去的
//! 就是 `alias` 本身，而内核根本不认这个名字 —— 结果是每一条请求 503。所以
//! [`validate`] 在 `route_cli_through_proxy` 关着时拒绝保存任何改名记录，而不是让
//! 用户配好之后再去日志里找原因。同名记录（`alias == target`）没有这个问题：
//! 改写表里根本不会有它，走不走代理都一样。

use std::collections::{BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::services::cli_io::write_atomic;
use crate::services::cli_types::CliTarget;
use crate::services::context_floor::alias_key;
use crate::services::context_window::ContextPolicy;
use crate::services::model_import::ImportEntry;

/// Claude Code 的 5 个槽位。它没有模型目录文件，能承载模型的地方只有这几个
/// 环境变量，所以「写进 Claude Code」= 「绑到某个槽位」。
pub const CLAUDE_TIERS: [&str; 5] = ["default", "fable", "sonnet", "opus", "haiku"];

/// 一条出口别名。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BridgeEntry {
    /// 写进 CLI 的名字。用户在 `/model` 里看到的就是它。
    pub alias: String,
    /// 转发给内核时换成这个 —— 内核渠道上真实存在的别名。
    ///
    /// 和 `alias` 相同时不产生改写规则（也就不依赖代理）。
    pub target: String,
    /// 写进 CLI 的上下文窗口。0 = 按 `target` 的名字自动推断。
    #[serde(default)]
    pub context_window: u64,
    /// 自动压缩在窗口的百分之几触发。0 = 用总控里的默认值。
    #[serde(default)]
    pub compact_percent: u8,
    /// 哪几家 CLI 要写它。空 = 谁都不写（记录留着，但不落进任何配置）。
    #[serde(default)]
    pub targets: BTreeSet<CliTarget>,
    /// Claude Code 的槽位。`None` / `"none"` = 不绑，也就是 Claude Code 不写它。
    #[serde(default)]
    pub tier: Option<String>,
}

impl BridgeEntry {
    /// 转发给内核的名字。`target` 空着就是别名自己。
    pub fn upstream_alias(&self) -> &str {
        let t = self.target.trim();
        if t.is_empty() {
            self.alias.trim()
        } else {
            t
        }
    }

    /// 这条记录会不会产生一条代理改写规则。同名的不需要改写。
    pub fn renames(&self) -> bool {
        let (a, t) = (self.alias.trim(), self.upstream_alias());
        !a.is_empty() && !t.is_empty() && a != t
    }

    /// 写进 CLI 的窗口。
    ///
    /// 手填的那一行最优先 —— 那是用户对着这一行敲的数。没填就**交给总控**
    /// （`ContextPolicy::resolve`）：自动档按 target 推断（`ccload-fast` 这个名字
    /// 什么都推不出来，它背后的 `grok-4.6` 才是 500k 的那个）、固定档一律写那个
    /// 固定值、不写档返回 0（这一行就不带窗口，CLI 保持自己的默认）。
    ///
    /// 必须走 `resolve` 而不是自己 `window_of` + `cap`：总控和这张表写的是**同一
    /// 批键**，各算各的就会出现「总控说固定 1M、桥接表写 200k，谁后跑谁赢」——
    /// 用户看到的就是「保存了但没生效」。总控是默认值，这一行是覆盖，只有一层。
    pub fn window(&self, policy: &ContextPolicy) -> u64 {
        if self.context_window > 0 {
            return self.context_window;
        }
        policy.resolve(self.upstream_alias()).unwrap_or(0)
    }

    /// 生效的压缩百分比。0 和越界值退回总控 —— 0% 是「每条都压缩」、100% 是
    /// 「永不压缩」，两个都不是能写进 CLI 的数。
    pub fn percent(&self, policy: &ContextPolicy) -> u8 {
        if (1..100).contains(&self.compact_percent) {
            self.compact_percent
        } else {
            policy.percent()
        }
    }

    /// 归一化后的 Claude 槽位。`""` / `"none"` 都当成没绑。
    pub fn slot(&self) -> Option<&str> {
        match self.tier.as_deref().map(str::trim) {
            Some("") | Some("none") | None => None,
            Some(s) => Some(s),
        }
    }
}

/// `~/.ccload-client/bridge.json`。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BridgeStore {
    #[serde(default)]
    pub entries: Vec<BridgeEntry>,
}

impl BridgeStore {
    pub fn load(path: &std::path::Path) -> Result<Self, AppError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(path)?;
        if raw.trim().is_empty() {
            return Ok(Self::default());
        }
        serde_json::from_str(&raw)
            .map_err(|e| AppError::Config(format!("bridge store is corrupt: {e}")))
    }

    pub fn save(&self, path: &std::path::Path) -> Result<(), AppError> {
        let body =
            serde_json::to_string_pretty(self).map_err(|e| AppError::Config(e.to_string()))?;
        write_atomic(path, &format!("{body}\n"))
    }

    /// 代理的改写表：`CLI 发的名字 → 内核认的名字`。
    ///
    /// 只收改名的记录。同名的放进去只是让代理多做一次无意义的查表，而且会把
    /// 「这张表非空 == 有改名」这个判断搞坏。
    pub fn rewrites(&self) -> HashMap<String, String> {
        self.entries
            .iter()
            .filter(|e| e.renames())
            .map(|e| (e.alias.trim().to_string(), e.upstream_alias().to_string()))
            .collect()
    }

    /// 要写进某一家 CLI 的那些记录，已经翻译成导入条目。
    ///
    /// 注意写进 CLI 的是 **alias**（用户在 `/model` 里选的名字），不是 target ——
    /// target 只活在代理的改写表里。
    pub fn import_entries(&self, target: CliTarget, policy: &ContextPolicy) -> Vec<ImportEntry> {
        self.entries
            .iter()
            .filter(|e| e.targets.contains(&target) && !e.alias.trim().is_empty())
            .map(|e| ImportEntry {
                alias: e.alias.trim().to_string(),
                context_window: Some(e.window(policy) as i64).filter(|n| *n > 0),
                tier: e.slot().map(str::to_string),
                compact_percent: Some(e.percent(policy)),
            })
            .collect()
    }
}

/// 落盘之前把写不出去的表挡掉。
///
/// 返回的是**警告**（能存，但用户该知道），错误则直接拒绝保存。分两档的理由：
/// 「这个 target 内核现在没有」很可能只是内核没连上或者用户打算稍后建渠道，
/// 拦下来毫无道理；而「重名」和「关着代理却要改名」是保存之后必定不工作的。
pub fn validate(entries: &[BridgeEntry], proxy_on: bool) -> Result<Vec<String>, AppError> {
    let mut warnings = Vec::new();
    let mut seen: HashMap<String, &str> = HashMap::new();
    // Claude 的 5 个槽位各只有一个值，两条记录抢同一个槽位是静默后来居上。
    let mut slots: HashMap<&str, &str> = HashMap::new();
    let mut renamed = Vec::new();

    for e in entries {
        let alias = e.alias.trim();
        if alias.is_empty() {
            return Err(AppError::Config("出口别名不能为空".into()));
        }
        // `@chNN` 是钉住写进内核的私有别名（见 `services::pins`）。让用户在这里
        // 造一个同形状的名字，两套机制会在代理里互相盖。
        if alias.contains("@ch") {
            return Err(AppError::Config(format!(
                "「{alias}」不能用：`@chNN` 是首选渠道钉住占用的私有名字"
            )));
        }
        let key = alias_key(alias);
        if let Some(prev) = seen.insert(key, alias) {
            return Err(AppError::Config(format!(
                "「{prev}」和「{alias}」是同一个名字（比较时忽略大小写和 [1M] 这类后缀），CLI 里只能留一个"
            )));
        }
        if e.upstream_alias().is_empty() {
            return Err(AppError::Config(format!("「{alias}」没有落点：请选一个内核别名")));
        }
        if let Some(slot) = e.slot() {
            if !CLAUDE_TIERS.contains(&slot) {
                return Err(AppError::Config(format!("未知的 Claude 槽位：{slot}")));
            }
            if !e.targets.contains(&CliTarget::ClaudeCode) {
                warnings.push(format!(
                    "「{alias}」选了 {slot} 槽位，但没勾 Claude Code —— 槽位不会被写入"
                ));
            } else if let Some(prev) = slots.insert(slot, alias) {
                return Err(AppError::Config(format!(
                    "Claude Code 的 {slot} 槽位只能绑一个模型，但「{prev}」和「{alias}」都选了它"
                )));
            }
        }
        if e.renames() {
            renamed.push(alias.to_string());
        }
        if e.compact_percent >= 100 {
            return Err(AppError::Config(format!(
                "「{alias}」的压缩阈值是 {}% —— 100% 等于永不压缩，请填 1..99（留 0 用默认）",
                e.compact_percent
            )));
        }
    }

    // 改名只有经过代理才成立。直连内核时 CLI 发出去的就是 alias 本身，内核不认，
    // 每一条请求都是 503 —— 让它保存下去等于埋一个「配好了却全线失败」。
    if !renamed.is_empty() && !proxy_on {
        return Err(AppError::Config(format!(
            "有 {} 条记录改了名（{}…），但 CLI 现在直连内核 —— 改写只发生在本地代理里，\
             直连时内核收到的是这个新名字、根本不认它，每条请求都会失败。\
             请先在「CLI 接管」页打开「通过本地代理」，或者把这些记录的出口名改回和落点一致。",
            renamed.len(),
            renamed.first().map(String::as_str).unwrap_or("")
        )));
    }

    Ok(warnings)
}

/// 给一批内核别名生成默认记录：出口名 == 内核别名，窗口和阈值都走自动。
///
/// 这是「填充默认值」那个按钮的语义 —— 先把最保守的一份表铺出来（同名 = 不依赖
/// 代理、不改变任何现有行为），用户再挑要改名 / 改窗口的那几条。
pub fn seed(aliases: &[String], targets: &BTreeSet<CliTarget>) -> Vec<BridgeEntry> {
    aliases
        .iter()
        .map(|a| BridgeEntry {
            alias: a.clone(),
            target: a.clone(),
            context_window: 0,
            compact_percent: 0,
            targets: targets.clone(),
            tier: None,
        })
        .collect()
}

/// 按名字把 Claude 的空槽位填上。每个槽位只认领一次，已经被占的不动。
///
/// 认领的同时**把 Claude Code 勾上** —— 槽位是它唯一的承载方式（没有目录文件），
/// 认领了槽位却不勾等于白认领。反过来也成立：这就是为什么「补齐」给 Claude Code
/// 铺表时不该把 83 行全勾上 —— 其中 78 行没有槽位可占，一个字都写不进去，只会让
/// 用户在 83 个复选框里找那 5 个真的有用的。
///
/// 猜错的代价是用户在槽位那一行换一个；猜不出来的代价是那个槽位空着。都比让人
/// 对着 83 行手点五次强。
pub fn guess_slots(entries: &mut [BridgeEntry]) {
    let mut used: BTreeSet<String> = entries
        .iter()
        .filter_map(|e| e.slot().map(str::to_string))
        .collect();
    for e in entries.iter_mut() {
        if e.slot().is_some() {
            continue;
        }
        let n = e.alias.to_ascii_lowercase();
        let guess = ["fable", "opus", "sonnet", "haiku"]
            .into_iter()
            .find(|k| n.contains(k) && !used.contains(*k));
        if let Some(g) = guess {
            used.insert(g.to_string());
            e.tier = Some(g.to_string());
            e.targets.insert(CliTarget::ClaudeCode);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(alias: &str, target: &str) -> BridgeEntry {
        BridgeEntry {
            alias: alias.into(),
            target: target.into(),
            context_window: 0,
            compact_percent: 0,
            targets: BTreeSet::from([CliTarget::OpenCode]),
            tier: None,
        }
    }

    /// 改写表只收改名的那些。同名记录进了表只会让代理白查一次，还会让
    /// 「表非空 == 有改名」这个判断失真。
    #[test]
    fn only_renaming_entries_become_rewrites() {
        let store = BridgeStore {
            entries: vec![
                entry("ccload-fast", "grok-4.6"),
                entry("claude-opus-5", "claude-opus-5"),
                // target 空着就是「和别名同名」。
                entry("glm-5.3", ""),
            ],
        };
        let r = store.rewrites();
        assert_eq!(r.len(), 1);
        assert_eq!(r.get("ccload-fast").map(String::as_str), Some("grok-4.6"));
    }

    /// 窗口跟着 **target** 算：出口名是我们编的，什么都推不出来（128k 兜底），
    /// 背后那个 grok-4.6 才是 500k。这正是用户要的「按各自的上下文拆分」。
    #[test]
    fn the_window_follows_the_target_not_the_made_up_alias() {
        let p = ContextPolicy::default();
        assert_eq!(entry("ccload-fast", "grok-4.6").window(&p), 500_000);
        assert_eq!(entry("ccload-big", "claude-opus-5").window(&p), 1_000_000);
        // 手填盖过推断。
        let mut manual = entry("ccload-fast", "grok-4.6");
        manual.context_window = 300_000;
        assert_eq!(manual.window(&p), 300_000);
    }

    /// 总控和这张表写的是同一批键，所以窗口只有一层：总控是默认值，行内是覆盖。
    ///
    /// 各算各的就会出现用户报的那个「保存了但没生效」——「CLI 接管」页写着
    /// 「固定 1M，五家一律写这个数」，桥接表却按模型名给 haiku 写 200k，
    /// 谁后跑谁赢。
    #[test]
    fn an_unfilled_row_follows_the_global_policy_including_fixed_mode() {
        use crate::services::context_window::ContextMode;
        let fixed = ContextPolicy {
            mode: ContextMode::Fixed,
            fixed_tokens: 1_000_000,
            ..Default::default()
        };
        // 落点按名字只有 200k，但总控说固定 1M —— 听总控的。
        let row = entry("ccload-haiku", "claude-haiku-4-5-20251001");
        assert_eq!(row.window(&fixed), 1_000_000);

        // 行内手填仍然最优先：那是用户对着这一行敲的数。
        let mut manual = row.clone();
        manual.context_window = 200_000;
        assert_eq!(manual.window(&fixed), 200_000);

        // 「不写入」档就是不写：这一行不带窗口，CLI 保持自己的默认。
        let off = ContextPolicy { mode: ContextMode::Off, ..Default::default() };
        assert_eq!(row.window(&off), 0);
        assert_eq!(manual.window(&off), 200_000, "手填盖过「不写入」");

        // 自动档照旧按落点推断，并且尊重上限夹子。
        let capped = ContextPolicy { cap_tokens: 500_000, ..Default::default() };
        assert_eq!(entry("ccload-big", "claude-opus-5").window(&capped), 500_000);
    }

    /// 阈值：留 0 用总控的 90%，填了就用自己的，越界退回默认。
    #[test]
    fn the_percent_falls_back_to_the_global_default() {
        let p = ContextPolicy::default();
        let mut e = entry("a", "grok-4.6");
        assert_eq!(e.percent(&p), 90);
        e.compact_percent = 80;
        assert_eq!(e.percent(&p), 80);
        e.compact_percent = 0;
        assert_eq!(e.percent(&p), 90);
    }

    /// 用户口径的那两行：grok-4.6 500k / 90% → 450k，opus-5 1M / 90% → 900k。
    #[test]
    fn the_two_rows_from_the_report_resolve_as_described() {
        let p = ContextPolicy::default();
        let fast = entry("ccload-fast", "grok-4.6");
        assert_eq!(fast.window(&p), 500_000);
        assert_eq!(p.compact_tokens(fast.window(&p)) , 450_000);
        let big = entry("ccload-big", "claude-opus-5");
        assert_eq!(big.window(&p), 1_000_000);
        assert_eq!(p.compact_tokens(big.window(&p)), 900_000);
    }

    /// 写进 CLI 的是 alias（用户 `/model` 里选的），不是 target。
    /// 每家 CLI 只拿勾了自己的那些行 —— 「分 CLI 区别注入」就是这一句。
    #[test]
    fn each_cli_only_gets_the_rows_that_picked_it() {
        let p = ContextPolicy::default();
        let mut opus = entry("ccload-big", "claude-opus-5");
        opus.targets = BTreeSet::from([CliTarget::ClaudeCode]);
        opus.tier = Some("opus".into());
        let store = BridgeStore {
            entries: vec![entry("ccload-fast", "grok-4.6"), opus],
        };

        let oc = store.import_entries(CliTarget::OpenCode, &p);
        assert_eq!(oc.len(), 1);
        assert_eq!(oc[0].alias, "ccload-fast", "写进 CLI 的必须是出口名");
        assert_eq!(oc[0].context_window, Some(500_000));
        assert_eq!(oc[0].compact_percent, Some(90));

        let cc = store.import_entries(CliTarget::ClaudeCode, &p);
        assert_eq!(cc.len(), 1);
        assert_eq!(cc[0].alias, "ccload-big");
        assert_eq!(cc[0].tier.as_deref(), Some("opus"));
        // Codex 一行都没勾。
        assert!(store.import_entries(CliTarget::Codex, &p).is_empty());
    }

    /// 直连内核时改名必定 503 —— 保存前就拦住，别让人去日志里找原因。
    #[test]
    fn renaming_without_the_proxy_is_refused() {
        let entries = vec![entry("ccload-fast", "grok-4.6")];
        let err = validate(&entries, false).unwrap_err();
        assert!(err.to_string().contains("本地代理"), "{err}");
        // 走代理就没问题。
        assert!(validate(&entries, true).unwrap().is_empty());
        // 同名记录不依赖代理，关着也能存。
        assert!(validate(&[entry("grok-4.6", "grok-4.6")], false).is_ok());
    }

    /// 重名在 CLI 目录里是一个键，后写的会盖掉先写的 —— 静默丢一行配置。
    /// 比较和代理查表一样忽略大小写与 `[1M]` 后缀。
    #[test]
    fn duplicate_aliases_are_refused_case_and_suffix_insensitively() {
        let dup = vec![entry("Grok-4.6", "grok-4.6"), entry("grok-4.6[1m]", "grok-4.6")];
        let err = validate(&dup, true).unwrap_err();
        assert!(err.to_string().contains("同一个名字"), "{err}");
    }

    /// 一个 Claude 槽位两条记录 = 静默后来居上，正是导入那条路上修过的老 bug。
    #[test]
    fn two_entries_on_one_claude_slot_is_an_error() {
        let mut a = entry("a", "x");
        let mut b = entry("b", "y");
        for e in [&mut a, &mut b] {
            e.targets = BTreeSet::from([CliTarget::ClaudeCode]);
            e.tier = Some("opus".into());
        }
        let err = validate(&[a, b], true).unwrap_err();
        assert!(err.to_string().contains("opus 槽位"), "{err}");
    }

    /// 选了槽位却没勾 Claude Code：能存（用户可能正要去勾），但要说一声。
    #[test]
    fn a_slot_without_claude_code_is_a_warning_not_an_error() {
        let mut e = entry("a", "x");
        e.tier = Some("opus".into());
        let w = validate(&[e], true).unwrap();
        assert_eq!(w.len(), 1);
        assert!(w[0].contains("没勾 Claude Code"), "{}", w[0]);
    }

    /// `@chNN` 归钉住用。让用户在这里造同形状的名字，两套机制会在代理里打架。
    #[test]
    fn pinned_private_alias_shape_is_reserved() {
        let err = validate(&[entry("grok-4.6@ch21", "grok-4.6")], true).unwrap_err();
        assert!(err.to_string().contains("钉住"), "{err}");
    }

    #[test]
    fn a_hundred_percent_never_compacts_so_it_is_refused() {
        let mut e = entry("a", "x");
        e.compact_percent = 100;
        assert!(validate(&[e], true).is_err());
    }

    /// 落点为空的记录存下去就是一条永远 404 的别名。
    #[test]
    fn an_entry_without_a_target_is_refused() {
        let mut e = entry("a", "");
        e.alias = "   ".into();
        assert!(validate(&[e], true).is_err());
    }

    /// 种子表是最保守的一份：同名，所以不依赖代理，也不改变任何现有行为。
    #[test]
    fn seed_is_identity_so_it_never_needs_the_proxy() {
        let aliases = vec!["claude-opus-5".to_string(), "grok-4.6".to_string()];
        let seeded = seed(&aliases, &BTreeSet::from([CliTarget::OpenCode]));
        assert_eq!(seeded.len(), 2);
        assert!(seeded.iter().all(|e| !e.renames()));
        assert!(validate(&seeded, false).is_ok());
        assert!(BridgeStore { entries: seeded }.rewrites().is_empty());
    }

    /// 槽位猜测：每个槽位只认领一次，已经绑好的不动，认领了就顺手勾上 Claude Code。
    ///
    /// 勾上这一步是必须的：Claude Code 没有目录文件，槽位是它唯一的承载方式，
    /// 认领了槽位却不勾，导入时这一行会被当成「没勾这家」直接跳过。
    #[test]
    fn slot_guessing_claims_each_slot_once_and_ticks_claude_code() {
        let mut es = vec![
            entry("my-opus-5", "claude-opus-5"),
            entry("another-opus", "claude-opus-4-8"),
            entry("my-haiku", "claude-haiku-4-5"),
            entry("some-sonnet", "claude-sonnet-5"),
            entry("grok-4.6", "grok-4.6"),
        ];
        guess_slots(&mut es);
        assert_eq!(es[0].slot(), Some("opus"));
        assert!(es[0].targets.contains(&CliTarget::ClaudeCode));
        assert_eq!(es[1].slot(), None, "opus 已经被认领了");
        assert!(
            !es[1].targets.contains(&CliTarget::ClaudeCode),
            "没认领到槽位的不该被勾上 —— 它一个字都写不进 Claude Code"
        );
        assert_eq!(es[2].slot(), Some("haiku"));
        assert_eq!(es[3].slot(), Some("sonnet"));
        // 名字里没有档位关键字的猜不出来，也就不勾。
        assert_eq!(es[4].slot(), None);
        assert!(!es[4].targets.contains(&CliTarget::ClaudeCode));
    }

    /// 已经手绑好的槽位不能被再猜一次盖掉。
    #[test]
    fn slot_guessing_leaves_existing_bindings_alone() {
        let mut mine = entry("my-pick", "claude-opus-5");
        mine.tier = Some("opus".into());
        let mut es = vec![mine, entry("another-opus", "claude-opus-4-8")];
        guess_slots(&mut es);
        assert_eq!(es[0].slot(), Some("opus"));
        assert_eq!(es[1].slot(), None);
    }

    /// 老 bridge.json 里没有新字段时读成默认值，不是反序列化失败。
    #[test]
    fn a_minimal_record_round_trips() {
        let store: BridgeStore =
            serde_json::from_str(r#"{"entries":[{"alias":"a","target":"b"}]}"#).unwrap();
        let e = &store.entries[0];
        assert_eq!(e.context_window, 0);
        assert_eq!(e.compact_percent, 0);
        assert!(e.targets.is_empty());
        assert_eq!(e.slot(), None);
    }
}
