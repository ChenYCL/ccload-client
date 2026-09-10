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
//!      而 Claude Code 的 `/model` 菜单是 6 个具名槽位加一份列表。
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
//! | `targets` | 哪几家 CLI 要写它。OpenCode 装进目录；Claude Code 见下 |
//! | `tiers` | Claude Code 的槽位（可以几个同占一行）；一个都没占就进 `modelPicker` 列表 |
//!
//! # Claude Code 那一侧：6 个槽位归这张表**独占**
//!
//! Claude Code 没有目录文件。`/model` 菜单由三部分拼成：5 个 tier 环境变量、1 个
//! 自定义项、settings.json 里的 `modelPicker` 列表（2.1.243 起）。前六个是具名位置，
//! 一个名字可以同时占几个 —— 「主模型和 opus 都是 claude-opus-5」是最常见的配法，
//! 单槽位的模型表达不了它，表现就是用户磁盘上明明有值、这里却显示空着。
//!
//! 所以这张表和磁盘之间是双向的：读表时先把磁盘上已有、表里没认领的槽位收进来
//! （[`adopt_disk_slots`]），写入时表里空着的槽位会被清掉（`claude_bridge::write`）。
//! 「导入」命令（`model_import`）那条路仍然是纯追加，两者别混。
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
use crate::services::cli_types::{CliTarget, ConfigRoot};
use crate::services::context_floor::alias_key;
use crate::services::context_window::ContextPolicy;
use crate::services::model_import::ImportEntry;

/// Claude Code 的 6 个具名槽位：5 个 tier 环境变量加 `ANTHROPIC_CUSTOM_MODEL_OPTION`。
///
/// `custom` 也算槽位：它就是 `/model` 末尾那一行，以前靠「第一条没绑 tier 的行」
/// 隐式推出来 —— 表里另一行勾上 Claude Code，自定义项就悄悄换了人。
pub const CLAUDE_TIERS: [&str; 6] = ["default", "fable", "sonnet", "opus", "haiku", "custom"];

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
    /// 这一行占的 Claude Code 槽位，可以几个同占。空 = 不占槽位；此时勾了
    /// Claude Code 的行进 `modelPicker` 列表。
    ///
    /// 旧文件里是单值 `tier`（`"opus"` / `null` / `"none"`），照旧读得进来；写出去
    /// 只有 `tiers`，两个键同时出现会被 serde 当成重复字段拒收。
    #[serde(default, alias = "tier", deserialize_with = "de_tiers")]
    pub tiers: BTreeSet<String>,
}

/// `tier: "opus"` / `tier: null` / `tiers: ["opus", "haiku"]` 三种写法都收，并把
/// `""` / `"none"` 这两种「没绑」的旧拼法丢掉。
fn de_tiers<'de, D: serde::Deserializer<'de>>(d: D) -> Result<BTreeSet<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Many(Vec<String>),
        One(Option<String>),
    }
    let raw: Vec<String> = match Raw::deserialize(d)? {
        Raw::Many(v) => v,
        Raw::One(s) => s.into_iter().collect(),
    };
    Ok(raw.into_iter().filter_map(|s| normalize_slot(&s)).collect())
}

/// `""` / `"none"` 都是「没绑」。
fn normalize_slot(s: &str) -> Option<String> {
    match s.trim() {
        "" | "none" => None,
        s => Some(s.to_string()),
    }
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

    /// 这一行占的 Claude 槽位，去掉 `""` / `"none"` 这两种「没绑」的拼法。
    /// 反序列化已经洗过一遍，这里再洗是给代码里直接构造的记录兜底。
    pub fn slots(&self) -> impl Iterator<Item = &str> {
        self.tiers
            .iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty() && *s != "none")
    }

    pub fn has_slot(&self, slot: &str) -> bool {
        self.slots().any(|s| s == slot)
    }

    /// 勾了 Claude Code 但一个槽位都没占 —— 这些行进 `modelPicker` 列表。
    pub fn in_claude_picker(&self) -> bool {
        self.targets.contains(&CliTarget::ClaudeCode)
            && !self.alias.trim().is_empty()
            && self.slots().next().is_none()
    }
}

/// `~/.ccload-client/bridge.json`。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BridgeStore {
    #[serde(default)]
    pub entries: Vec<BridgeEntry>,
    /// 用户清空过、但磁盘上还留着旧值的 Claude 槽位（「墓碑」）。
    ///
    /// 没有它，「用户刚清的」和「从没认领过」对读表的人来说长得一样：后者该把
    /// 磁盘上的槽位收进表里，前者收进来等于清空操作永远不生效。墓碑在两种情况
    /// 下作废：槽位被重新认领，或写入把磁盘上的旧值清掉了（`sync_entries`）。
    #[serde(default)]
    pub cleared_slots: BTreeSet<String>,
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
    ///
    /// Claude Code 一行占几个槽位就出几条（`ImportEntry::tier` 是单值）；一个都
    /// 没占的出一条 `tier: None`。桥接写 Claude Code 走的是 `claude_bridge::write`
    /// 而不是这里 —— 这条路留给纯追加的 `model_import` 命令。
    pub fn import_entries(&self, target: CliTarget, policy: &ContextPolicy) -> Vec<ImportEntry> {
        self.entries
            .iter()
            .filter(|e| e.targets.contains(&target) && !e.alias.trim().is_empty())
            .flat_map(|e| {
                let make = |tier: Option<String>| ImportEntry {
                    alias: e.alias.trim().to_string(),
                    context_window: Some(e.window(policy) as i64).filter(|n| *n > 0),
                    tier,
                    compact_percent: Some(e.percent(policy)),
                };
                let tiers: Vec<Option<String>> = if target == CliTarget::ClaudeCode {
                    e.slots().map(|s| Some(s.to_string())).collect()
                } else {
                    Vec::new()
                };
                if tiers.is_empty() {
                    vec![make(None)]
                } else {
                    tiers.into_iter().map(make).collect()
                }
            })
            .collect()
    }
}

/// 把磁盘上已有、表里没人认领的 Claude 槽位收进表里。返回收了哪几个槽位。
///
/// `on_disk` 是 settings.json 里那 6 个键现在的值（槽位名 → 模型名）。同名的行
/// （忽略大小写和 `[1M]` 这类后缀）直接多占一个槽位；没有这一行就补一条同名落点。
/// 已经有人认领的槽位不动 —— 那是用户在表里做过的选择，磁盘上的旧值等写入时覆盖。
/// `cleared` 里的槽位也不动：那是用户刚清空的，收回来等于清空永远不生效。
///
/// 之所以要收而不是只显示「磁盘：X」：写入会清掉表里空着的槽位，不先收进来，
/// 用户在别处配好的 fable / haiku 会在第一次点「写进 Claude Code」时被抹掉。
pub fn adopt_disk_slots(
    entries: &mut Vec<BridgeEntry>,
    on_disk: &std::collections::BTreeMap<String, String>,
    cleared: &BTreeSet<String>,
) -> Vec<String> {
    let mut adopted = Vec::new();
    for slot in CLAUDE_TIERS {
        let Some(model) = on_disk.get(slot).map(|m| m.trim()).filter(|m| !m.is_empty()) else {
            continue;
        };
        // `@chNN` 是钉住的私有名字，`validate` 不让它进表；收进来只会让下一次
        // 保存整体失败。留在磁盘上由「磁盘：X」提示。
        if cleared.contains(slot) || model.contains("@ch") || entries.iter().any(|e| e.has_slot(slot))
        {
            continue;
        }
        let key = alias_key(model);
        match entries.iter_mut().find(|e| alias_key(&e.alias) == key) {
            Some(e) => {
                e.tiers.insert(slot.into());
                e.targets.insert(CliTarget::ClaudeCode);
            }
            None => entries.push(BridgeEntry {
                alias: model.to_string(),
                target: model.to_string(),
                context_window: 0,
                compact_percent: 0,
                targets: BTreeSet::from([CliTarget::ClaudeCode]),
                tiers: BTreeSet::from([slot.to_string()]),
            }),
        }
        adopted.push(slot.to_string());
    }
    adopted
}

/// 读路径共用的对齐：收磁盘上的槽位、修剪已作废的墓碑。返回有没有改动。
///
/// 墓碑作废的两个时机都在这：槽位被重新认领，或写入已把磁盘上的旧值清掉。
pub fn sync_entries(store: &mut BridgeStore, root: &ConfigRoot) -> bool {
    let disk = crate::services::claude_bridge::slots_on_disk(root);
    let adopted = adopt_disk_slots(&mut store.entries, &disk, &store.cleared_slots);
    let before = store.cleared_slots.len();
    store.cleared_slots.retain(|s| {
        disk.get(s).is_some_and(|v| !v.trim().is_empty())
            && !store.entries.iter().any(|e| e.has_slot(s))
    });
    if !adopted.is_empty() {
        tracing::info!("bridge: adopted Claude Code slots from disk: {}", adopted.join(", "));
    }
    !adopted.is_empty() || store.cleared_slots.len() != before
}

/// 落盘之前把写不出去的表挡掉。
///
/// 返回的是**警告**（能存，但用户该知道），错误则直接拒绝保存。分两档的理由：
/// 「这个 target 内核现在没有」很可能只是内核没连上或者用户打算稍后建渠道，
/// 拦下来毫无道理；而「重名」和「关着代理却要改名」是保存之后必定不工作的。
pub fn validate(entries: &[BridgeEntry], proxy_on: bool) -> Result<Vec<String>, AppError> {
    let mut warnings = Vec::new();
    let mut seen: HashMap<String, &str> = HashMap::new();
    // Claude 的 6 个槽位各只有一个值，两条记录抢同一个槽位是静默后来居上。
    // 反过来（一条记录占几个槽位）是允许的：主模型和 opus 同一个名字很常见。
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
        for slot in e.slots() {
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
            tiers: BTreeSet::new(),
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
        .flat_map(|e| e.slots().map(str::to_string))
        .collect();
    for e in entries.iter_mut() {
        if e.slots().next().is_some() {
            continue;
        }
        let n = e.alias.to_ascii_lowercase();
        let guess = ["fable", "opus", "sonnet", "haiku"]
            .into_iter()
            .find(|k| n.contains(k) && !used.contains(*k));
        if let Some(g) = guess {
            used.insert(g.to_string());
            e.tiers.insert(g.to_string());
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
            tiers: BTreeSet::new(),
        }
    }

    /// 单槽位的行「它占的那个槽位」。多槽位的行别用它，只会看到排序最靠前的那个。
    fn slot(e: &BridgeEntry) -> Option<&str> {
        e.slots().next()
    }

    fn slotted(alias: &str, target: &str, slots: &[&str]) -> BridgeEntry {
        let mut e = entry(alias, target);
        e.targets = BTreeSet::from([CliTarget::ClaudeCode]);
        e.tiers = slots.iter().map(|s| s.to_string()).collect();
        e
    }

    /// 旧 `bridge.json` 里是单值 `tier`。三种旧拼法都要读得进来，`""` / `"none"`
    /// 要洗成「没绑」，否则校验会报「未知的 Claude 槽位：」。
    #[test]
    fn legacy_single_tier_files_still_load() {
        let load = |raw: &str| -> BridgeEntry { serde_json::from_str(raw).unwrap() };
        let base = r#""alias":"a","target":"a""#;
        let one = load(&format!(r#"{{{base},"tier":"opus"}}"#));
        assert_eq!(one.slots().collect::<Vec<_>>(), vec!["opus"]);
        assert!(load(&format!(r#"{{{base},"tier":null}}"#)).slots().next().is_none());
        assert!(load(&format!(r#"{{{base},"tier":"none"}}"#)).slots().next().is_none());
        assert!(load(&format!(r#"{{{base}}}"#)).slots().next().is_none());
        let many = load(&format!(r#"{{{base},"tiers":["opus","","haiku"]}}"#));
        assert_eq!(many.slots().collect::<Vec<_>>(), vec!["haiku", "opus"]);
        // 写出去只有 `tiers`。
        let json = serde_json::to_string(&many).unwrap();
        assert!(json.contains("\"tiers\""));
        assert!(!json.contains("\"tier\":"));
    }

    /// 主模型和 opus 都是 claude-opus-5：一行占两个槽位，校验放行，导入条目出两条。
    #[test]
    fn one_row_may_hold_several_claude_slots() {
        let row = slotted("claude-opus-5", "claude-opus-5", &["default", "opus"]);
        assert!(validate(std::slice::from_ref(&row), true).unwrap().is_empty());
        let store = BridgeStore { entries: vec![row], cleared_slots: Default::default() };
        let cc = store.import_entries(CliTarget::ClaudeCode, &ContextPolicy::default());
        let mut tiers: Vec<_> = cc.iter().map(|e| e.tier.clone().unwrap()).collect();
        tiers.sort();
        assert_eq!(tiers, vec!["default", "opus"]);
        // 别的 CLI 不关心槽位，还是一条。
        let mut oc = store.entries[0].clone();
        oc.targets.insert(CliTarget::OpenCode);
        let store = BridgeStore { entries: vec![oc], cleared_slots: Default::default() };
        assert_eq!(store.import_entries(CliTarget::OpenCode, &ContextPolicy::default()).len(), 1);
    }

    /// 磁盘上有、表里没认领的槽位要收进表里：同名行多占一个，没有的补一行。
    /// 已认领的槽位不动 —— 那是用户的选择，磁盘上的旧值留给写入覆盖。
    #[test]
    fn slots_on_disk_are_adopted_into_the_table() {
        let mut rows = vec![
            slotted("claude-opus-5", "claude-opus-5", &["opus"]),
            slotted("claude-sonnet-5", "claude-sonnet-5", &["sonnet"]),
        ];
        let disk = std::collections::BTreeMap::from([
            ("default".to_string(), "claude-opus-5[1M]".to_string()),
            ("haiku".to_string(), "Claude-Opus-5".to_string()),
            ("sonnet".to_string(), "some-old-sonnet".to_string()),
            ("fable".to_string(), "claude-fable-5-1[1M]".to_string()),
            ("custom".to_string(), "  ".to_string()),
        ]);
        let mut adopted = adopt_disk_slots(&mut rows, &disk, &Default::default());
        adopted.sort();
        assert_eq!(adopted, vec!["default", "fable", "haiku"]);
        // `[1M]` 和大小写都不算另一个名字：收进已有的那一行，别名保持用户写的。
        let opus = &rows[0];
        assert_eq!(opus.alias, "claude-opus-5");
        let mut got: Vec<_> = opus.slots().collect();
        got.sort();
        assert_eq!(got, vec!["default", "haiku", "opus"]);
        // sonnet 已经有人认领，磁盘上的旧值不能抢回来。
        assert!(rows[1].has_slot("sonnet"));
        assert_eq!(rows.iter().filter(|r| r.has_slot("sonnet")).count(), 1);
        // fable 表里没有这一行：补一条同名落点，只勾 Claude Code。
        let fable = rows.iter().find(|r| r.has_slot("fable")).expect("补出来的行");
        assert_eq!(fable.alias, "claude-fable-5-1[1M]");
        assert_eq!(fable.target, "claude-fable-5-1[1M]");
        assert_eq!(fable.targets, BTreeSet::from([CliTarget::ClaudeCode]));
        // 空白值等于磁盘上没有。
        assert!(!rows.iter().any(|r| r.has_slot("custom")));
        // 再跑一遍什么都不会变。
        assert!(adopt_disk_slots(&mut rows, &disk, &Default::default()).is_empty());
        // 墓碑里的槽位不收：那是用户刚清空的。
        let mut rows2 = vec![slotted("claude-opus-5", "claude-opus-5", &["opus"])];
        let tombstones: BTreeSet<String> = ["haiku".to_string()].into();
        assert!(adopt_disk_slots(&mut rows2, &disk, &tombstones).contains(&"fable".to_string()));
        assert!(!rows2.iter().any(|r| r.has_slot("haiku")), "墓碑挡住收回");
    }

    /// 墓碑的完整生命周期：清空 → 保存记下 → 读表不收回 → 写入清掉磁盘旧值 →
    /// 墓碑作废；重新认领同样让它作废。
    #[test]
    fn a_cleared_slot_stays_cleared_until_reclaimed_or_written() {
        use crate::services::cli_types::ConfigRoot as TestRoot;
        let opus_row = || slotted("claude-opus-5", "claude-opus-5", &["opus"]);
        let mut store = BridgeStore {
            entries: vec![opus_row()],
            cleared_slots: ["haiku".to_string()].into(),
        };
        let dir = tempfile::tempdir().unwrap();
        let root = TestRoot::sandbox(dir.path().to_path_buf());
        let settings = root.join(".claude/settings.json");
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        std::fs::write(
            &settings,
            r#"{"env":{"ANTHROPIC_DEFAULT_HAIKU_MODEL":"old-haiku"}}"#,
        )
        .unwrap();

        // 读表不把 haiku 收回来。
        assert!(!sync_entries(&mut store, &root));
        assert!(!store.entries.iter().any(|r| r.has_slot("haiku")));

        // 用户改主意，重新认领 haiku → 墓碑作废（被修剪属于「有改动」，会存回）。
        store.entries[0].tiers.insert("haiku".into());
        assert!(sync_entries(&mut store, &root));
        assert!(!store.cleared_slots.contains("haiku"));
        // 再跑一遍：槽位认领着、墓碑也没了，真正没什么可对齐的。
        assert!(!sync_entries(&mut store, &root));

        // 再清空一次，这次走写入：写入会清掉磁盘上的旧值、并作废墓碑
        // （`commands::bridge` 里做的事）。这里只验证随后的读表不再有动作。
        store.cleared_slots.insert("haiku".into());
        store.entries[0].tiers.remove("haiku");
        assert!(!sync_entries(&mut store, &root));
        assert!(!store.entries.iter().any(|r| r.has_slot("haiku")));
    }

    /// 改写表只收改名的那些。同名记录进了表只会让代理白查一次，还会让
    /// 「表非空 == 有改名」这个判断失真。
    #[test]
    fn only_renaming_entries_become_rewrites() {
        let store = BridgeStore {
            cleared_slots: Default::default(),
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
        // 总控说固定 1M，可落点真实只有 200k —— **听真实上限的**。
        // 高估会死锁（CLI 以为 1M、900k 才压缩，上游 200k 就 400），见
        // `ContextPolicy::fixed_for`。
        let row = entry("ccload-haiku", "claude-haiku-4-5-20251001");
        assert_eq!(row.window(&fixed), 200_000);
        // 认得出的模型都夹：grok-4.6 真实 500k。用户报的那个 bug 就是这一条 ——
        // Grok Build 被写了 1M，会话涨到 470K/1.0M 之后没救。
        assert_eq!(entry("ccload-fast", "grok-4.6").window(&fixed), 500_000);
        // 真实上限比固定值宽时不往上抬：夹子只往下夹。
        let narrow = ContextPolicy { fixed_tokens: 300_000, ..fixed.clone() };
        assert_eq!(entry("ccload-big", "claude-opus-5").window(&narrow), 300_000);
        // 认不出的名字不夹 —— 那时我们没把握，不该拿 128k 猜测推翻用户的设定。
        assert_eq!(entry("ccload-x", "some-private-relay-model").window(&fixed), 1_000_000);

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
        let opus = slotted("ccload-big", "claude-opus-5", &["opus"]);
        let store = BridgeStore {
            entries: vec![entry("ccload-fast", "grok-4.6"), opus],
            cleared_slots: Default::default(),
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
        let a = slotted("a", "x", &["opus"]);
        let b = slotted("b", "y", &["opus", "haiku"]);
        let err = validate(&[a, b], true).unwrap_err();
        assert!(err.to_string().contains("opus 槽位"), "{err}");
    }

    /// 选了槽位却没勾 Claude Code：能存（用户可能正要去勾），但要说一声。
    #[test]
    fn a_slot_without_claude_code_is_a_warning_not_an_error() {
        let mut e = entry("a", "x");
        e.tiers.insert("opus".into());
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
        assert!(BridgeStore { entries: seeded, cleared_slots: Default::default() }.rewrites().is_empty());
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
        assert_eq!(slot(&es[0]), Some("opus"));
        assert!(es[0].targets.contains(&CliTarget::ClaudeCode));
        assert_eq!(slot(&es[1]), None, "opus 已经被认领了");
        assert!(
            !es[1].targets.contains(&CliTarget::ClaudeCode),
            "没认领到槽位的不该被勾上 —— 它一个字都写不进 Claude Code"
        );
        assert_eq!(slot(&es[2]), Some("haiku"));
        assert_eq!(slot(&es[3]), Some("sonnet"));
        // 名字里没有档位关键字的猜不出来，也就不勾。
        assert_eq!(slot(&es[4]), None);
        assert!(!es[4].targets.contains(&CliTarget::ClaudeCode));
    }

    /// 已经手绑好的槽位不能被再猜一次盖掉。
    #[test]
    fn slot_guessing_leaves_existing_bindings_alone() {
        let mut mine = entry("my-pick", "claude-opus-5");
        mine.tiers.insert("opus".into());
        let mut es = vec![mine, entry("another-opus", "claude-opus-4-8")];
        guess_slots(&mut es);
        assert_eq!(slot(&es[0]), Some("opus"));
        assert_eq!(slot(&es[1]), None);
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
        assert_eq!(slot(e), None);
    }
}
