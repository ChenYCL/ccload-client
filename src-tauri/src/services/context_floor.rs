//! 「这个 CLI 发出去的模型名，在内核里可能落到哪些上游」→ 取最窄的那个窗口。
//!
//! # 为什么是「最窄」而不是「当前那一跳」
//!
//! 五家 CLI 的窗口都是**启动时读的静态配置**（Claude 的 env、Codex 的
//! `model_context_window`、Grok 目录表的 `context_window`……）。内核把一条请求从
//! Anthropic 分流到 grok-4.6 的那一刻，CLI 不知道、也没有任何键能在会话中途改。
//! 所以唯一不会死锁的写法是：把这个别名**所有可能**落到的上游都算进来，按最窄的
//! 写 —— 1M 的 claude-opus-5 链上挂着一跳 500k 的 grok，Claude Code 就按 500k
//! 跑、在 450k 压缩。代价是主力健康时少用一半窗口；不这么写的代价是分流那一刻
//! 会话直接 400 too long，`/compact` 自己也发不出去。
//!
//! # 四个来源
//!
//! | 来源 | 什么时候算进来 |
//! | --- | --- |
//! | 模型本身 | 永远 |
//! | 首选渠道钉住（`pins.json`） | 钉住的别名 == 这个模型名，且 CLI 走本地代理 |
//! | 模型链（`fallback.json`） | 链的别名 == 这个模型名 |
//! | 强制路由（`forced_route.json`） | 路由的 `from` == 这个模型名 |
//! | 内核渠道（`GET /admin/channels`） | 启用渠道里有 `models[].model == 别名` 的条目 |
//!
//! 钉住且**不退让**时，请求只会落到钉住的渠道，链 / 路由 / 内核那些落点根本到不了，
//! 不算进来 —— 否则钉在 1M 的渠道上还会被链上一跳 500k 的备胎压窄。
//!
//! 前四个在本地，内核那一个要联网，拿不到就跳过（`kernel: None`）—— 窗口是个
//! 优化，不该因为内核没起来就让接管整体失败。但调用方要知道少算了一块：启动自愈
//! 比对「磁盘值 ≠ 算出来的值」时，内核没连上那次的值是偏宽的，拿它去判漂移会
//! 每次启动都重写一遍。
//!
//! 别名比较忽略大小写、忽略 `[1m]` 这类窗口后缀：CLI 里写的是 `claude-opus-5[1M]`
//! （后缀是客户端算窗口用的，代理转发前会剥掉），链和内核里叫 `claude-opus-5`。
//! 以前这里是逐字比较，于是链的窄口从来没对上过 —— 用户看到的就是「链上明明有
//! 500k 的 grok，Claude Code 还是按 1M 写」。

use std::collections::HashMap;

use serde::Serialize;
use serde_json::Value;

use crate::services::context_window::{ContextMode, ContextPolicy, WindowSource};
use crate::services::fallback::FallbackChain;
use crate::services::forced_route::ForcedRoute;
use crate::services::pins::Pin;

/// 内核里某个渠道对某个别名的一条服务记录。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct RouteHit {
    pub channel_id: i64,
    pub channel_name: String,
    pub priority: i64,
    /// 渠道条目里**原样**写的别名。停用/启用要拿它去打内核（内核按条目自己的
    /// 写法找），CLI 那边可能带着 `[1M]` 后缀而条目没有。
    pub alias: String,
    /// 真正发给上游的名字（`redirect_model`，没有就是别名自己）。
    pub upstream: String,
    /// 这条模型条目被停用了。内核当它不存在，但界面上要能看见并能重新启用。
    pub disabled: bool,
}

/// `GET /admin/channels` 解析成「别名 → 谁在服务它」。只收启用的渠道。
#[derive(Debug, Default, Clone)]
pub struct KernelRoutes {
    by_alias: HashMap<String, Vec<RouteHit>>,
}

impl KernelRoutes {
    /// `channels` 是响应里的 `data` 数组。形状认不出来的条目跳过，不报错。
    pub fn parse(channels: &Value) -> Self {
        let mut by_alias: HashMap<String, Vec<RouteHit>> = HashMap::new();
        for ch in channels.as_array().into_iter().flatten() {
            if !ch.get("enabled").and_then(Value::as_bool).unwrap_or(true) {
                continue;
            }
            let Some(channel_id) = ch.get("id").and_then(Value::as_i64) else {
                continue;
            };
            let channel_name = ch
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_string();
            let priority = ch.get("priority").and_then(Value::as_i64).unwrap_or(0);
            for m in ch.get("models").and_then(Value::as_array).into_iter().flatten() {
                let Some(alias) = m.get("model").and_then(Value::as_str).map(str::trim) else {
                    continue;
                };
                if alias.is_empty() {
                    continue;
                }
                let upstream = m
                    .get("redirect_model")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .unwrap_or(alias)
                    .to_string();
                let disabled = m.get("disabled").and_then(Value::as_bool).unwrap_or(false);
                by_alias
                    .entry(alias_key(alias))
                    .or_default()
                    .push(RouteHit {
                        channel_id,
                        channel_name: channel_name.clone(),
                        priority,
                        alias: alias.to_string(),
                        upstream,
                        disabled,
                    });
            }
        }
        for hits in by_alias.values_mut() {
            // 内核按 priority DESC 选渠道；同优先级加权轮询。列表就按这个顺序给，
            // 用户看到的第一条就是请求默认落到的地方。
            hits.sort_by(|a, b| b.priority.cmp(&a.priority).then(a.channel_id.cmp(&b.channel_id)));
        }
        Self { by_alias }
    }

    /// 服务这个别名的渠道，按优先级从高到低。停用的条目也在，带 `disabled` 标记。
    pub fn hits(&self, alias: &str) -> Vec<RouteHit> {
        let mut out: Vec<RouteHit> = Vec::new();
        // 内核里可能同时存着带后缀和不带后缀两种写法，两种都算是「服务这个别名」。
        for key in [alias.trim().to_ascii_lowercase(), alias_key(alias)] {
            if let Some(hits) = self.by_alias.get(&key) {
                for h in hits {
                    if !out.contains(h) {
                        out.push(h.clone());
                    }
                }
            }
        }
        out.sort_by(|a, b| b.priority.cmp(&a.priority).then(a.channel_id.cmp(&b.channel_id)));
        out
    }

    pub fn is_empty(&self) -> bool {
        self.by_alias.is_empty()
    }
}

/// 去掉末尾 `[..]` 后缀：`claude-opus-5[1M]` → `claude-opus-5`。
pub fn bare_alias(name: &str) -> &str {
    let s = name.trim();
    match (s.rfind('['), s.ends_with(']')) {
        (Some(i), true) => s[..i].trim_end(),
        _ => s,
    }
}

/// 内核认的 thinking 后缀：`gpt-5.6(max)` 的 `(max)`。只认内核 `thinking/suffix.go`
/// 里那几个词和非负整数预算 —— `Default (recommended)` 这种带括号的名字不是后缀，
/// 内核也不会剥它。
fn is_thinking_suffix(inner: &str) -> bool {
    let s = inner.trim().to_ascii_lowercase();
    matches!(
        s.as_str(),
        "none" | "auto" | "-1" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
    ) || (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
}

/// 把末尾的 thinking 后缀切下来：`gpt-5.6(max)` → `("gpt-5.6", "(max)")`；没有就
/// `(name, "")`。内核选路、鉴权、冷却用的都是去掉它之后的名字（`RoutingModelName`）。
pub fn split_thinking_suffix(name: &str) -> (&str, &str) {
    match (name.rfind('('), name.ends_with(')')) {
        (Some(open), true) if open > 0 && is_thinking_suffix(&name[open + 1..name.len() - 1]) => {
            (&name[..open], &name[open..])
        }
        _ => (name, ""),
    }
}

/// 内核按它选路的名字：剥掉客户端的窗口后缀 `[1m]` 和内核的 thinking 后缀 `(max)`。
/// 两种后缀谁在外面都行（`gpt-5.6[400k](high)` / `gpt-5.6(high)[400k]`），剥到没有为止。
pub fn routing_base(name: &str) -> &str {
    let mut s = name.trim();
    loop {
        let next = split_thinking_suffix(bare_alias(s)).0.trim_end();
        if next == s {
            return s;
        }
        s = next;
    }
}

/// 别名比较用的键：剥两种后缀、小写。钉住表和代理也用它，三处要是同一个定义。
pub fn alias_key(name: &str) -> String {
    routing_base(name).to_ascii_lowercase()
}

fn same_alias(a: &str, b: &str) -> bool {
    let (a, b) = (alias_key(a), alias_key(b));
    !a.is_empty() && a == b
}

/// 一个候选落点是从哪条路算进来的。界面上「最窄来自哪」要说得出来。
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Via {
    /// CLI 选中的模型本身。
    Model,
    /// 首选渠道钉住的一个落点。
    Pinned,
    /// 模型链的一跳。
    Chain,
    /// 强制路由的一个目标。
    ForcedRoute,
    /// 内核渠道上服务这个别名的一条 `redirect_model`。
    Kernel,
}

/// 一个可能的落点和它的窗口。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Candidate {
    /// 真正会发给上游的模型名。
    pub model: String,
    pub via: Via,
    /// 绑的渠道名，有就带上 —— 「grok-4.6（xAI-l）」比光一个名字好认。
    pub channel_name: Option<String>,
    pub window: u64,
    pub source: WindowSource,
    /// 这一条参与取最窄。
    ///
    /// CLI 发的名字常常是个**虚拟别名**（`gb-review`、`Default (recommended)`），
    /// 请求从不真的跑在它上面，跑的是它映射到的上游。这种名字按家族猜出来的
    /// 128k 兜底毫无意义，一旦知道了真实落点就不该再拿它压低窗口。只有三种情况
    /// 它自己的窗口算数：没有任何别的落点可知；窗口是明确声明的（名字后缀、
    /// 分档表手填）；或者某条链 / 路由 / 内核渠道**确实**把请求落到它自己身上
    /// （`landed`）—— 「gpt-5 → [gpt-5, gemini]」这种主力同名的链，主力就是它。
    pub counted: bool,
    /// 有一条真实落点和它同名（合并进来了）。只在算 `counted` 时用。
    #[serde(skip)]
    pub landed: bool,
}

/// 一次解析的结果。
#[derive(Debug, Clone, Serialize)]
pub struct Floor {
    /// 最终要写进 CLI 的窗口。
    pub tokens: u64,
    /// 自动压缩触发点（`tokens × 百分比`）。
    pub compact_tokens: u64,
    /// 定下 `tokens` 的那个落点。Fixed 档没有。
    pub narrowest: Option<Candidate>,
    /// 全部候选，按算进来的顺序。
    pub candidates: Vec<Candidate>,
    /// 上限夹子生效了（`tokens` 比最窄候选还小）。
    pub capped: bool,
}

/// 解析需要的全部输入。内核那份可以缺。
pub struct FloorInputs<'a> {
    pub policy: &'a ContextPolicy,
    /// 生效中的钉住。CLI 不走代理时传空 —— 钉住对直连的 CLI 没有作用。
    pub pins: &'a [Pin],
    pub chains: &'a [FallbackChain],
    pub routes: &'a [ForcedRoute],
    pub kernel: Option<&'a KernelRoutes>,
}

/// 候选齐了之后定 `counted`：模型自己那条只在孤身一人、窗口是明确声明、或者确有
/// 落点落在它身上时算数。
fn finish(mut out: Vec<Candidate>) -> Vec<Candidate> {
    let alone = out.len() == 1;
    for c in &mut out {
        c.counted = c.via != Via::Model
            || alone
            || c.landed
            || matches!(c.source, WindowSource::Manual | WindowSource::Suffix);
    }
    out
}

impl FloorInputs<'_> {
    /// 这个模型名所有可能落到的上游。第一条永远是它自己。
    pub fn candidates(&self, model: &str) -> Vec<Candidate> {
        let model = model.trim();
        if model.is_empty() {
            return Vec::new();
        }
        let mut out: Vec<Candidate> = Vec::new();
        let mut push = |name: &str, via: Via, channel: Option<&str>| {
            let name = name.trim();
            if name.is_empty() {
                return;
            }
            // 同一个上游从两条路算进来（链上写了、内核里也应用过了）只留第一条 ——
            // 窗口是一样的，重复只会让「候选」列表看着像有六个落点。后来的那条
            // 要是带渠道名而先前那条没有，把渠道名补上，显示更好认。比较剥掉
            // `[1M]` 后缀：CLI 里的 `claude-opus-5[1M]` 和渠道上的 `claude-opus-5`
            // 是同一个上游，后缀只是客户端算窗口用的。
            let key = alias_key(name);
            if let Some(seen) = out.iter_mut().find(|c| alias_key(&c.model) == key) {
                if seen.channel_name.is_none() {
                    seen.channel_name = channel.map(str::to_string);
                }
                // 一条真实落点和模型自己同名：请求确实会落到它身上，它的窗口要算数。
                // 以前这里只补渠道名，于是「gpt-5 → [gpt-5, gemini 1M]」的主力被当成
                // 虚拟别名扔掉，窗口按更宽的备胎写 —— 正是这个功能要防的 400。
                if via != Via::Model {
                    seen.landed = true;
                }
                return;
            }
            out.push(Candidate {
                model: name.to_string(),
                via,
                channel_name: channel.map(str::to_string),
                window: self.policy.window_of(name),
                source: self.policy.source_of(name),
                counted: true,
                landed: false,
            });
        };
        push(model, Via::Model, None);
        let mut only_pinned = false;
        for pin in self.pins.iter().filter(|p| same_alias(&p.alias, model)) {
            for tgt in &pin.targets {
                push(&tgt.upstream, Via::Pinned, Some(&tgt.channel_name));
            }
            only_pinned |= !pin.fallback && !pin.targets.is_empty();
        }
        if only_pinned {
            return finish(out);
        }
        for chain in self.chains.iter().filter(|c| same_alias(&c.alias, model)) {
            for hop in &chain.hops {
                push(&hop.upstream, Via::Chain, hop.channel_name.as_deref());
            }
        }
        for route in self.routes.iter().filter(|r| same_alias(&r.from, model)) {
            for tgt in &route.targets {
                push(&tgt.model, Via::ForcedRoute, tgt.channel_name.as_deref());
            }
        }
        if let Some(k) = self.kernel {
            for hit in k.hits(model).into_iter().filter(|h| !h.disabled) {
                push(&hit.upstream, Via::Kernel, Some(&hit.channel_name));
            }
        }
        finish(out)
    }

    /// 这个模型该写多大的窗口。`None` = 不写（Off 档，或者没有模型名）。
    pub fn floor(&self, model: &str) -> Option<Floor> {
        let policy = self.policy;
        match policy.mode {
            ContextMode::Off => None,
            ContextMode::Fixed => {
                let tokens = policy.fixed_tokens;
                (tokens > 0).then(|| Floor {
                    tokens,
                    compact_tokens: policy.compact_tokens(tokens),
                    narrowest: None,
                    candidates: Vec::new(),
                    capped: false,
                })
            }
            ContextMode::Auto => {
                let candidates = self.candidates(model);
                let narrowest = candidates
                    .iter()
                    .filter(|c| c.counted)
                    .min_by_key(|c| c.window)?
                    .clone();
                let tokens = policy.cap(narrowest.window);
                Some(Floor {
                    tokens,
                    compact_tokens: policy.compact_tokens(tokens),
                    capped: tokens < narrowest.window,
                    narrowest: Some(narrowest),
                    candidates,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::fallback::FallbackHop;
    use crate::services::forced_route::ForcedTarget;

    fn chain(alias: &str, hops: &[(&str, &str)]) -> FallbackChain {
        FallbackChain {
            alias: alias.into(),
            hops: hops
                .iter()
                .map(|(up, ch)| FallbackHop {
                    upstream: (*up).into(),
                    channel_id: None,
                    channel_name: Some((*ch).into()),
                })
                .collect(),
        }
    }

    fn route(from: &str, targets: &[(&str, &str)]) -> ForcedRoute {
        ForcedRoute {
            from: from.into(),
            targets: targets
                .iter()
                .map(|(m, ch)| ForcedTarget {
                    channel_id: None,
                    channel_name: Some((*ch).into()),
                    model: (*m).into(),
                })
                .collect(),
        }
    }

    /// 真机上抓的形状（脱敏）：Z.ai 在 80 上把 grok-4.6 改写成 glm，xAI 在 60 上
    /// 原样服务 grok-4.6，还有一个停用渠道和一条停用条目要被忽略。
    fn kernel() -> KernelRoutes {
        KernelRoutes::parse(&serde_json::json!([
            {"id": 15, "name": "Anthropic", "enabled": true, "priority": 90,
             "models": [{"model": "claude-opus-5"}, {"model": "Opus 5", "redirect_model": "claude-opus-5"}]},
            {"id": 17, "name": "Z.ai", "enabled": true, "priority": 80,
             "models": [{"model": "claude-opus-5", "redirect_model": "glm-5.3-flash"},
                        {"model": "grok-4.6", "redirect_model": "glm-5.3-flash"},
                        {"model": "glm-5.3-flash"}]},
            {"id": 21, "name": "xAI", "enabled": true, "priority": 60,
             "models": [{"model": "grok-4.6"}, {"model": "claude-opus-5", "redirect_model": "grok-4.6"},
                        {"model": "claude-haiku-4-5", "redirect_model": "grok-3-mini", "disabled": true}]},
            {"id": 12, "name": "off", "enabled": false, "priority": 70,
             "models": [{"model": "claude-opus-5", "redirect_model": "kimi-k2"}]}
        ]))
    }

    /// 两种后缀都要剥：客户端的 `[1m]` 和内核的 `(max)`；带括号但不是 thinking 词的
    /// 名字（`Default (recommended)`）原样保留，内核也不会剥它。
    #[test]
    fn alias_key_strips_window_and_thinking_suffixes_only() {
        assert_eq!(alias_key("Claude-Opus-5[1M]"), "claude-opus-5");
        assert_eq!(alias_key("gpt-5.6(max)"), "gpt-5.6");
        assert_eq!(alias_key("gpt-5.6(8192)"), "gpt-5.6");
        assert_eq!(alias_key("gpt-5.6[400k](High)"), "gpt-5.6");
        assert_eq!(alias_key("gpt-5.6(high)[400k]"), "gpt-5.6");
        assert_eq!(alias_key("Default (recommended)"), "default (recommended)");
        assert_eq!(split_thinking_suffix("gpt-5.6(max)"), ("gpt-5.6", "(max)"));
        assert_eq!(split_thinking_suffix("foo(bar)"), ("foo(bar)", ""));
        assert_eq!(split_thinking_suffix("(max)"), ("(max)", ""));
    }

    /// 内核选路按优先级从高到低，列表就得按这个顺序 —— 第一条是请求默认落到的地方。
    #[test]
    fn kernel_hits_are_ordered_by_priority_and_skip_disabled_channels() {
        let k = kernel();
        let hits = k.hits("grok-4.6");
        assert_eq!(hits.len(), 2);
        assert_eq!((hits[0].channel_name.as_str(), hits[0].upstream.as_str()), ("Z.ai", "glm-5.3-flash"));
        assert_eq!((hits[1].channel_name.as_str(), hits[1].upstream.as_str()), ("xAI", "grok-4.6"));
        // 停用渠道 12 不在
        assert!(k.hits("claude-opus-5").iter().all(|h| h.channel_id != 12));
        // 停用的条目在，但带标记
        let haiku = k.hits("claude-haiku-4-5");
        assert_eq!(haiku.len(), 1);
        assert!(haiku[0].disabled);
    }

    /// CLI 里写的是 `claude-opus-5[1M]`，链和内核里叫 `claude-opus-5`。以前这里逐字
    /// 比较，链的窄口从来没对上过 —— 「链上有 500k 的 grok，Claude Code 还是按 1M 写」。
    #[test]
    fn suffix_and_case_do_not_break_alias_matching() {
        let policy = ContextPolicy::default();
        let chains = [chain("claude-opus-5", &[("claude-opus-5", "Anthropic"), ("grok-4.6", "xAI")])];
        let inputs = FloorInputs { policy: &policy, pins: &[], chains: &chains, routes: &[], kernel: None };
        let f = inputs.floor("Claude-Opus-5[1M]").unwrap();
        assert_eq!(f.tokens, 500_000);
        let n = f.narrowest.unwrap();
        assert_eq!(n.model, "grok-4.6");
        assert_eq!(n.via, Via::Chain);
        assert_eq!(n.channel_name.as_deref(), Some("xAI"));
        // 模型自己那条带后缀，按后缀声明 1M。
        assert_eq!(f.candidates[0].window, 1_000_000);
        assert_eq!(f.candidates[0].source, WindowSource::Suffix);
    }

    /// 四个来源都要算进来；同一个上游从两条路进来只留一条。
    #[test]
    fn floor_takes_the_narrowest_across_chain_route_and_kernel() {
        let policy = ContextPolicy::default();
        let chains = [chain("claude-opus-5", &[("claude-opus-5", "Anthropic"), ("glm-5.3-flash", "Z.ai")])];
        let routes = [route("claude-opus-5", &[("claude-opus-5", "Anthropic"), ("grok-4.6", "xAI")])];
        let k = kernel();
        let inputs = FloorInputs { policy: &policy, pins: &[], chains: &chains, routes: &routes, kernel: Some(&k) };
        let f = inputs.floor("claude-opus-5").unwrap();
        assert_eq!(f.tokens, 500_000);
        assert_eq!(f.compact_tokens, 450_000);
        let n = f.narrowest.unwrap();
        assert_eq!(n.model, "grok-4.6");
        // 强制路由先算进来，内核那条 xAI→grok-4.6 是重复的，不再列。
        assert_eq!(n.via, Via::ForcedRoute);
        let names: Vec<_> = f.candidates.iter().map(|c| (c.model.as_str(), c.via)).collect();
        assert_eq!(
            names,
            vec![
                ("claude-opus-5", Via::Model),
                ("glm-5.3-flash", Via::Chain),
                ("grok-4.6", Via::ForcedRoute),
            ],
            "{names:?}"
        );
        // 链上那条 claude-opus-5（Anthropic）和模型自己合并了，渠道名补到了第一条上。
        assert_eq!(f.candidates[0].channel_name.as_deref(), Some("Anthropic"));
    }

    /// CLI 发的是个虚拟别名（`gb-review`），名字本身猜不出窗口（128k 兜底）。知道了
    /// 真实落点之后，这个兜底数不能再拿来压窗口 —— 否则每个用调度图别名的 CLI 都会
    /// 被写成 128k。名字上挂了后缀或手填了分档的仍然算数。
    #[test]
    fn a_virtual_alias_does_not_count_once_real_upstreams_are_known() {
        let policy = ContextPolicy::default();
        let chains = [chain("gb-review", &[("claude-opus-5", "Anthropic"), ("grok-4.6", "xAI")])];
        let inputs = FloorInputs { policy: &policy, pins: &[], chains: &chains, routes: &[], kernel: None };
        let f = inputs.floor("gb-review").unwrap();
        assert_eq!(f.tokens, 500_000, "{:?}", f.candidates);
        assert!(!f.candidates[0].counted);
        assert!(f.candidates[1].counted);

        // 一个落点都不知道时，只能按名字算。
        let inputs = FloorInputs { policy: &policy, pins: &[], chains: &[], routes: &[], kernel: None };
        let f = inputs.floor("gb-review").unwrap();
        assert_eq!(f.tokens, 128_000);
        assert!(f.candidates[0].counted);

        // 名字后缀是明确声明：`[300k]` 比链上任何一跳都窄，就按它。
        let f = FloorInputs { policy: &policy, pins: &[], chains: &chains, routes: &[], kernel: None }
            .floor("gb-review[300k]")
            .unwrap();
        assert_eq!(f.tokens, 300_000);

        // 手填也是明确声明。
        let mut manual = ContextPolicy::default();
        manual.overrides.insert("gb-review".into(), 250_000);
        let f = FloorInputs { policy: &manual, pins: &[], chains: &chains, routes: &[], kernel: None }
            .floor("gb-review")
            .unwrap();
        assert_eq!(f.tokens, 250_000);
        assert_eq!(f.narrowest.unwrap().source, WindowSource::Manual);
    }

    /// 只有内核知道的落点（用户在内核后台手加的改写）也得算 —— 那正是「突然分流
    /// 到 grok」的来源，客户端本地文件里根本没有它。
    #[test]
    fn kernel_only_routes_still_lower_the_floor() {
        let policy = ContextPolicy::default();
        let k = kernel();
        let inputs = FloorInputs { policy: &policy, pins: &[], chains: &[], routes: &[], kernel: Some(&k) };
        let f = inputs.floor("claude-opus-5").unwrap();
        assert_eq!(f.tokens, 500_000);
        let n = f.narrowest.unwrap();
        assert_eq!((n.model.as_str(), n.via, n.channel_name.as_deref()), ("grok-4.6", Via::Kernel, Some("xAI")));
    }

    /// 内核没连上：只按本地算，宁可偏宽也不能报错让接管整体失败。
    #[test]
    fn without_kernel_the_floor_is_local_only() {
        let policy = ContextPolicy::default();
        let inputs = FloorInputs { policy: &policy, pins: &[], chains: &[], routes: &[], kernel: None };
        let f = inputs.floor("claude-opus-5").unwrap();
        assert_eq!(f.tokens, 1_000_000);
        assert_eq!(f.candidates.len(), 1);
    }

    /// 分档表里的手填值参与取最窄：本地 qwen 名字推不出来（128k 兜底），用户说它
    /// 有 200k，就按 200k 算。
    #[test]
    fn manual_tiers_feed_the_floor() {
        let mut policy = ContextPolicy::default();
        policy.overrides.insert("Qwen3.8-27B-Claude".into(), 200_000);
        let chains = [chain("gb-review", &[("claude-opus-5", "Anthropic"), ("Qwen3.8-27B-Claude", "local")])];
        let inputs = FloorInputs { policy: &policy, pins: &[], chains: &chains, routes: &[], kernel: None };
        let f = inputs.floor("gb-review").unwrap();
        assert_eq!(f.tokens, 200_000);
        assert_eq!(f.compact_tokens, 180_000);
        assert_eq!(f.narrowest.unwrap().source, WindowSource::Manual);
    }

    fn pin(alias: &str, targets: &[(i64, &str, &str)], fallback: bool) -> Pin {
        Pin {
            alias: alias.into(),
            targets: targets
                .iter()
                .map(|(id, ch, up)| crate::services::pins::PinTarget {
                    channel_id: *id,
                    channel_name: (*ch).into(),
                    upstream: (*up).into(),
                })
                .collect(),
            fallback,
        }
    }

    /// 钉住且不退让：请求只会落到钉住的渠道，链上那跳 500k 的 grok 到不了，
    /// 不能拿它压窄 1M 的 claude。退让开着时它又回来了。
    #[test]
    fn a_pin_without_fallback_hides_every_other_landing() {
        let policy = ContextPolicy::default();
        let chains = [chain("claude-opus-5", &[("claude-opus-5", "Anthropic"), ("grok-4.6", "xAI")])];
        let k = kernel();
        let pinned = [pin("claude-opus-5", &[(15, "Anthropic", "claude-opus-5")], false)];
        let inputs = FloorInputs { policy: &policy, pins: &pinned, chains: &chains, routes: &[], kernel: Some(&k) };
        let f = inputs.floor("claude-opus-5[1M]").unwrap();
        assert_eq!(f.tokens, 1_000_000, "{:?}", f.candidates);
        assert_eq!(f.candidates.len(), 1);
        // 钉住的落点和模型自己合并成一条，渠道名补上了。
        assert_eq!(f.candidates[0].channel_name.as_deref(), Some("Anthropic"));

        let open = [pin("claude-opus-5", &[(15, "Anthropic", "claude-opus-5")], true)];
        let inputs = FloorInputs { policy: &policy, pins: &open, chains: &chains, routes: &[], kernel: Some(&k) };
        let f = inputs.floor("claude-opus-5[1M]").unwrap();
        assert_eq!(f.tokens, 500_000);
        assert_eq!(f.narrowest.unwrap().model, "grok-4.6");
    }

    /// 主力和别名同名、备胎更宽（grok-4.6 → [grok-4.6 @ xAI, claude-opus-5 @ Anthropic]）：
    /// 主力那条合并进模型自己之后必须还算数，否则窗口按 1M 的备胎写，日常落在 500k
    /// 的主力上照样 400。内核渠道同理。
    #[test]
    fn a_same_named_primary_landing_still_counts() {
        let policy = ContextPolicy::default();
        let chains = [chain("grok-4.6", &[("grok-4.6", "xAI"), ("claude-opus-5", "Anthropic")])];
        let inputs = FloorInputs { policy: &policy, pins: &[], chains: &chains, routes: &[], kernel: None };
        let f = inputs.floor("grok-4.6").unwrap();
        assert_eq!(f.tokens, 500_000, "{:?}", f.candidates);
        assert!(f.candidates[0].counted);
        assert_eq!(f.candidates[0].channel_name.as_deref(), Some("xAI"));

        // 只有内核知道：xAI 原样服务 grok-4.6，Z.ai 把它改写成 1M 的 glm。
        let k = kernel();
        let inputs = FloorInputs { policy: &policy, pins: &[], chains: &[], routes: &[], kernel: Some(&k) };
        let f = inputs.floor("grok-4.6").unwrap();
        assert_eq!(f.tokens, 500_000, "{:?}", f.candidates);

        // 反例不变：虚拟别名没人落到它身上，仍然不算。
        let chains = [chain("gb-review", &[("claude-opus-5", "Anthropic")])];
        let inputs = FloorInputs { policy: &policy, pins: &[], chains: &chains, routes: &[], kernel: None };
        let f = inputs.floor("gb-review").unwrap();
        assert_eq!(f.tokens, 1_000_000);
        assert!(!f.candidates[0].counted);
    }

    /// 钉住的落点本身是个更窄的模型时，它就是最窄 —— 来源标成 Pinned。
    #[test]
    fn a_pinned_upstream_counts_as_a_landing() {
        let policy = ContextPolicy::default();
        let pinned = [pin("gb-review", &[(21, "xAI", "grok-4.6")], false)];
        let inputs = FloorInputs { policy: &policy, pins: &pinned, chains: &[], routes: &[], kernel: None };
        let f = inputs.floor("gb-review").unwrap();
        assert_eq!(f.tokens, 500_000);
        let n = f.narrowest.unwrap();
        assert_eq!((n.model.as_str(), n.via, n.channel_name.as_deref()), ("grok-4.6", Via::Pinned, Some("xAI")));
        // 虚拟别名自己的 128k 兜底没算进来。
        assert!(!f.candidates[0].counted);
    }

    /// 上限夹子在最窄之上再夹一次；Fixed 不看任何落点；Off 什么都不给。
    #[test]
    fn cap_fixed_and_off_behave() {
        let k = kernel();
        let capped = ContextPolicy { cap_tokens: 300_000, ..Default::default() };
        let inputs = FloorInputs { policy: &capped, pins: &[], chains: &[], routes: &[], kernel: Some(&k) };
        let f = inputs.floor("claude-opus-5").unwrap();
        assert_eq!(f.tokens, 300_000);
        assert!(f.capped);

        let fixed = ContextPolicy { mode: ContextMode::Fixed, fixed_tokens: 400_000, ..Default::default() };
        let inputs = FloorInputs { policy: &fixed, pins: &[], chains: &[], routes: &[], kernel: Some(&k) };
        let f = inputs.floor("claude-opus-5").unwrap();
        assert_eq!(f.tokens, 400_000);
        assert!(f.narrowest.is_none());

        let off = ContextPolicy { mode: ContextMode::Off, ..Default::default() };
        let inputs = FloorInputs { policy: &off, pins: &[], chains: &[], routes: &[], kernel: Some(&k) };
        assert!(inputs.floor("claude-opus-5").is_none());
        // 没模型名也没得算。
        let auto = ContextPolicy::default();
        let inputs = FloorInputs { policy: &auto, pins: &[], chains: &[], routes: &[], kernel: Some(&k) };
        assert!(inputs.floor("  ").is_none());
    }
}
