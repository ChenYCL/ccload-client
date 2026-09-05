//! 首选渠道钉住：某个别名默认走用户选中的那个渠道，别的渠道只在它不可用时才轮到。
//!
//! # 为什么内核自己做不到
//!
//! 内核选渠道只看**渠道级** `priority`（`ORDER BY c.priority DESC`），`ModelEntry`
//! 只有 `{model, redirect_model, disabled}`，没有 per-model 的优先级，也没有「只当
//! 备胎」的标记。于是两个别名想要相反的顺序（claude 想 Z.ai > xAI，grok 想
//! xAI > Z.ai）在内核里表达不出来：改渠道优先级是把该渠道服务的**全部**模型一起
//! 挪。用户看到的就是「Grok Build 选了 grok-4.6，却一直跑在 Z.ai 的 glm 上」——
//! 那不是故障转移，是高优先级渠道上的一条改写成了主路由。
//!
//! # 做法
//!
//! 1. 在首选渠道上写一条**私有别名** `{alias}@ch{channel_id}` → 上游模型
//!    （`channel_writer::patch_channel`，和模型链共用同一条写入路径）。这个名字只有
//!    这一个渠道认，所以发它就等于「只许落到这个渠道」。
//! 2. 本地 CLI 代理把 CLI 发来的 `alias` 改写成私有别名先发一次；内核回
//!    「这条别名没有可用上游」类的失败（503 / 429 / 5xx…，见
//!    `cli_proxy::is_fallback_status`）时，再用原别名重发一次 —— 这一次就是内核自己
//!    的默认顺序。`fallback = false` 时不重发，首选不可用就直接把错误交给 CLI。
//!
//! 请求体除 `model` 字段外一个字节不动；重发用的是同一份已缓冲的 body。
//!
//! # 已知边界
//!
//! * 只在 CLI 走本地代理时生效（`route_cli_through_proxy`）。直连内核的 CLI 发的是
//!   原别名，钉住对它没有作用 —— 界面上要提醒。
//! * 内核日志里记的是私有别名（`grok-4.6@ch21`），日志页显示时剥掉 `@chNN`。
//! * 令牌若限制了可用模型（`IsModelAllowed`），私有别名也要在白名单里，否则 403 ——
//!   403 属于会退让的状态，开着退让时不影响使用，只是日志里多一条。
//! * 内核开了 `model_fuzzy_match` 时，别的渠道上「包含」原别名的条目会被算作服务它；
//!   私有别名恰好包含原别名，所以钉住的渠道也会以低优先级出现在原别名的候选里。
//!   影响仅限于「退让那一跳多一个候选」，不影响首选。
//! * `@ch` 不能撞上内核自己认的模型名后缀：内核的 thinking 后缀语法是
//!   `model(...)`（见 `thinking_suffix.go`），`@` 不在里面。

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::services::cli_io::write_atomic;
use crate::services::context_floor::{alias_key, routing_base, split_thinking_suffix};

/// 钉住的一个落点：哪个渠道、发什么上游名。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PinTarget {
    pub channel_id: i64,
    /// 只为显示；内核里改了名也不影响钉住本身。
    #[serde(default)]
    pub channel_name: String,
    /// 私有别名在这个渠道上要 redirect 到的真实模型名。
    pub upstream: String,
}

/// 一条钉住规则。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Pin {
    /// CLI 发来的别名（`grok-4.6`）。带 `[1m]` 后缀也认，比较时剥掉、忽略大小写。
    pub alias: String,
    /// 首选落点，按顺序试。界面现在只暴露一个；存成列表是给「模型链迁到按序试」留的。
    pub targets: Vec<PinTarget>,
    /// 首选全部失败后是否退到内核默认顺序（用原别名再发一次）。
    #[serde(default = "default_true")]
    pub fallback: bool,
}

fn default_true() -> bool {
    true
}

/// `~/.ccload-client/pins.json`。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PinStore {
    pub pins: Vec<Pin>,
}

impl PinStore {
    pub fn load(path: &std::path::Path) -> Result<Self, AppError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(path)?;
        if raw.trim().is_empty() {
            return Ok(Self::default());
        }
        serde_json::from_str(&raw)
            .map_err(|e| AppError::Config(format!("pins store is corrupt: {e}")))
    }

    pub fn save(&self, path: &std::path::Path) -> Result<(), AppError> {
        let body = serde_json::to_string_pretty(self)
            .map_err(|e| AppError::Config(e.to_string()))?;
        write_atomic(path, &format!("{body}\n"))
    }

    /// 按别名找（忽略后缀与大小写）。
    pub fn find(&self, alias: &str) -> Option<&Pin> {
        let key = alias_key(alias);
        self.pins.iter().find(|p| alias_key(&p.alias) == key)
    }

    pub fn upsert(&mut self, pin: Pin) {
        let key = alias_key(&pin.alias);
        match self.pins.iter_mut().find(|p| alias_key(&p.alias) == key) {
            Some(existing) => *existing = pin,
            None => self.pins.push(pin),
        }
    }

    /// 删掉并返回被删的那条（调用方要拿它去内核清私有别名）。
    pub fn remove(&mut self, alias: &str) -> Option<Pin> {
        let key = alias_key(alias);
        let idx = self.pins.iter().position(|p| alias_key(&p.alias) == key)?;
        Some(self.pins.remove(idx))
    }
}

/// 落盘之前把明显写不进内核的规则挡掉。
pub fn validate_pin(pin: &Pin) -> Result<(), AppError> {
    let alias = routing_base(&pin.alias);
    if alias.is_empty() {
        return Err(AppError::Config("别名不能为空".into()));
    }
    if alias.contains("@ch") {
        // 别名里已经带了私有后缀 —— 多半是把日志里的名字复制过来了。
        return Err(AppError::Config(format!(
            "「{alias}」看起来已经是一条私有别名，钉住要用原别名"
        )));
    }
    if pin.targets.is_empty() {
        return Err(AppError::Config("至少要选一个首选渠道".into()));
    }
    let mut seen = std::collections::HashSet::new();
    for (i, t) in pin.targets.iter().enumerate() {
        if t.channel_id <= 0 {
            return Err(AppError::Config(format!("第 {} 个落点没有绑渠道", i + 1)));
        }
        if t.upstream.trim().is_empty() {
            return Err(AppError::Config(format!(
                "第 {} 个落点（渠道 {}）没有上游模型名",
                i + 1,
                t.channel_id
            )));
        }
        if !seen.insert(t.channel_id) {
            return Err(AppError::Config(format!("渠道 {} 出现了两次", t.channel_id)));
        }
    }
    Ok(())
}

/// 写进内核的私有别名条目名：`grok-4.6[1m]` 钉在渠道 21 上 → `grok-4.6@ch21`。
///
/// 两种后缀都剥掉：窗口后缀内核本来就不认；thinking 后缀（`(max)`）是请求期的等级
/// 修饰，内核选路时自己会剥（`RoutingModelName`），条目里要的是基名。大小写按用户
/// 写的留 —— 内核的条目索引是精确匹配。
pub fn pinned_alias(alias: &str, channel_id: i64) -> String {
    format!("{}@ch{channel_id}", routing_base(alias))
}

/// 请求里真正要发的名字：私有标记插在 thinking 后缀**之前**，后缀原样留在最末 ——
/// `gpt-5.6(max)` 钉在渠道 9 → `gpt-5.6@ch9(max)`，内核剥掉 `(max)` 后查到条目
/// `gpt-5.6@ch9`，等级照常生效。`base` 用钉住表里存的基名（和内核条目同一份大小写）。
fn pinned_request_name(base: &str, channel_id: i64, requested: &str) -> String {
    let (_, thinking) = split_thinking_suffix(requested.trim());
    format!("{base}@ch{channel_id}{thinking}")
}

/// [`pinned_alias`] 的反函数：`grok-4.6@ch21` → `("grok-4.6", 21)`；
/// `gpt-5.6@ch21(max)` → `("gpt-5.6(max)", 21)`。不是私有别名就 None。
pub fn split_pinned(name: &str) -> Option<(String, i64)> {
    let (head, thinking) = split_thinking_suffix(name.trim());
    let (base, tail) = head.rsplit_once("@ch")?;
    if base.is_empty() || tail.is_empty() || !tail.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((format!("{base}{thinking}"), tail.parse().ok()?))
}

/// 代理要用的形态：别名键 → 钉在哪些渠道、失败后要不要退回原别名。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PinRule {
    /// 钉住表里存的基名（内核条目用的那份大小写）。
    pub base: String,
    /// 首选渠道，按顺序试。
    pub channels: Vec<i64>,
    pub fallback: bool,
}

impl PinRule {
    /// 这次请求依次要发的模型名。`plain` 是 CLI 那边剥完窗口后缀的原名，可能还带着
    /// thinking 后缀 —— 那个要跟到每个私有别名后面去。
    pub fn sequence(&self, plain: &str) -> Vec<String> {
        let mut out: Vec<String> = self
            .channels
            .iter()
            .map(|id| pinned_request_name(&self.base, *id, plain))
            .collect();
        if self.fallback || out.is_empty() {
            out.push(plain.to_string());
        }
        out
    }
}

/// 键是 [`alias_key`]（剥后缀、小写），代理拿请求里的名字算同一个键来查。
pub type PinRules = HashMap<String, PinRule>;

pub fn pin_rules(store: &PinStore) -> PinRules {
    store
        .pins
        .iter()
        .filter(|p| validate_pin(p).is_ok())
        .map(|p| {
            (
                alias_key(&p.alias),
                PinRule {
                    base: routing_base(&p.alias).to_string(),
                    channels: p.targets.iter().map(|t| t.channel_id).collect(),
                    fallback: p.fallback,
                },
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pin(alias: &str, targets: &[(i64, &str)], fallback: bool) -> Pin {
        Pin {
            alias: alias.into(),
            targets: targets
                .iter()
                .map(|(id, up)| PinTarget {
                    channel_id: *id,
                    channel_name: format!("ch{id}"),
                    upstream: (*up).into(),
                })
                .collect(),
            fallback,
        }
    }

    /// 私有别名的形状：剥后缀、留大小写；反解要能还原。
    #[test]
    fn private_alias_round_trips() {
        assert_eq!(pinned_alias("grok-4.6[1m]", 21), "grok-4.6@ch21");
        assert_eq!(pinned_alias("Claude-Opus-5", 15), "Claude-Opus-5@ch15");
        assert_eq!(split_pinned("grok-4.6@ch21"), Some(("grok-4.6".to_string(), 21)));
        assert_eq!(split_pinned("grok-4.6"), None);
        assert_eq!(split_pinned("@ch21"), None);
        assert_eq!(split_pinned("x@chab"), None);
        // 名字里本来就有 @ 的模型（少见但存在）不被误判。
        assert_eq!(split_pinned("user@example"), None);
        // 内核条目是基名；请求里 thinking 后缀留在最末、私有标记插在它前面，反解时拼回去。
        assert_eq!(pinned_alias("gpt-5.6(max)", 9), "gpt-5.6@ch9");
        assert_eq!(pinned_request_name("gpt-5.6", 9, "gpt-5.6(max)"), "gpt-5.6@ch9(max)");
        assert_eq!(pinned_request_name("gpt-5.6", 9, "GPT-5.6"), "gpt-5.6@ch9");
        assert_eq!(split_pinned("gpt-5.6@ch9(max)"), Some(("gpt-5.6(max)".to_string(), 9)));
        // 不是 thinking 词的括号不算后缀。
        assert_eq!(pinned_alias("Default (recommended)", 5), "Default (recommended)@ch5");
    }

    /// 代理的发送序列：先私有别名，退让开着时最后补原别名；关着就只有私有的。
    #[test]
    fn rules_encode_the_send_sequence() {
        let store = PinStore {
            pins: vec![
                pin("grok-4.6", &[(21, "grok-4.6")], true),
                pin("claude-opus-5", &[(15, "claude-opus-5"), (17, "glm-5.3-flash")], false),
            ],
        };
        let rules = pin_rules(&store);
        let grok = rules.get("grok-4.6").unwrap();
        assert_eq!(grok.sequence("grok-4.6"), vec!["grok-4.6@ch21", "grok-4.6"]);
        // 请求里带后缀 / 大小写不同，也查得到同一条规则。
        assert!(rules.contains_key(&alias_key("Grok-4.6[1m]")));
        let claude = rules.get("claude-opus-5").unwrap();
        assert_eq!(
            claude.sequence("claude-opus-5"),
            vec!["claude-opus-5@ch15", "claude-opus-5@ch17"]
        );
        // 请求带 thinking 后缀：钉住照样命中，后缀跟到每个私有别名后面，退让也带着。
        let store = PinStore { pins: vec![pin("gpt-5.6", &[(9, "gpt-5.6")], true)] };
        let rules = pin_rules(&store);
        let rule = rules.get(&alias_key("gpt-5.6(max)")).expect("thinking suffix must not hide the pin");
        assert_eq!(rule.sequence("gpt-5.6(max)"), vec!["gpt-5.6@ch9(max)", "gpt-5.6(max)"]);
    }

    /// 校验：空别名、没落点、重复渠道、缺上游、把私有别名当原别名，都拒。
    #[test]
    fn validation_rejects_unwritable_pins() {
        assert!(validate_pin(&pin("  ", &[(1, "m")], true)).is_err());
        assert!(validate_pin(&pin("grok-4.6", &[], true)).is_err());
        assert!(validate_pin(&pin("grok-4.6", &[(1, "m"), (1, "n")], true)).is_err());
        assert!(validate_pin(&pin("grok-4.6", &[(1, " ")], true)).is_err());
        assert!(validate_pin(&pin("grok-4.6", &[(0, "m")], true)).is_err());
        assert!(validate_pin(&pin("grok-4.6@ch21", &[(21, "m")], true)).is_err());
        assert!(validate_pin(&pin("grok-4.6[1m]", &[(21, "grok-4.6")], true)).is_ok());
        // 坏规则不进代理表，好的照常。
        let store = PinStore {
            pins: vec![pin("bad", &[], true), pin("good", &[(2, "g")], true)],
        };
        assert_eq!(pin_rules(&store).len(), 1);
    }

    /// 存取按别名键合并：同一个别名换了写法也只有一条。缺 `fallback` 的旧文件按 true 读。
    #[test]
    fn store_upserts_by_alias_key_and_defaults_fallback_on() {
        let mut store = PinStore::default();
        store.upsert(pin("grok-4.6", &[(21, "grok-4.6")], true));
        store.upsert(pin("Grok-4.6[1m]", &[(17, "glm-5.3-flash")], false));
        assert_eq!(store.pins.len(), 1);
        assert_eq!(store.pins[0].targets[0].channel_id, 17);
        assert!(store.find("GROK-4.6").is_some());
        assert_eq!(store.remove("grok-4.6").map(|p| p.fallback), Some(false));
        assert!(store.pins.is_empty());

        let parsed: PinStore = serde_json::from_str(
            r#"{"pins":[{"alias":"a","targets":[{"channel_id":1,"upstream":"a"}]}]}"#,
        )
        .unwrap();
        assert!(parsed.pins[0].fallback);
        assert_eq!(parsed.pins[0].targets[0].channel_name, "");

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pins.json");
        assert!(PinStore::load(&path).unwrap().pins.is_empty());
        parsed.save(&path).unwrap();
        assert_eq!(PinStore::load(&path).unwrap().pins.len(), 1);
    }
}
