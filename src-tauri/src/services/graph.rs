//! 调度图（Dual-Graph Auto Dispatch）的客户端实现。
//!
//! # 这是 PRD 的哪一部分，为什么是这一部分
//!
//! 原 PRD 把 graph 放在内核请求路径上（`graph_resolver.go` 在取渠道快照前改写
//! model 并收窄渠道集）。约束是内核不动，所以这里换一条路：**把 graph 静态编译
//! 成内核已经认识的配置**。
//!
//! 内核现成的两块原语正好够用：
//!   * 渠道的 `models[] = {model, redirect_model}` —— 就是「别名 → 某家的真实
//!     模型」，等价于 PRD 里的 provider 投影表（§7）。
//!   * 渠道 `priority` + 既有的冷却/SWRR —— 就是「同档 fallback，失败换节点」。
//!     一个别名只挂在它自己那一档的模型上，所以内核换渠道时**不可能降档**，
//!     PRD §10 「禁止降档」自动成立。
//!
//! 再加上客户端本来就在写 CLI 配置：把四档别名写进 Claude 的 tier env、把
//! `gb-*` 写进 Grok 的 config.toml，角色则落到 agent 文件的 `model:` 上。
//!
//! # 做不到的部分（必须诚实标出来）
//!
//! 以下依赖**请求时**的信息，静态配置无法表达，内核不改就没有：
//!   * `when: {lang: zh}` / `tokens_gt` —— 要看请求体才知道
//!   * session 亲和（同一会话粘同一条路）
//!   * `X-CCLoad-Role` 请求头、`X-CCLoad-*` 调试响应头
//!   * 按 graph 投影 `/v1/models`（这里是别名真的存在于渠道上，所以自然会出现）
//!
//! # 一个硬约束：优先级是渠道级的
//!
//! 内核的 `ModelEntry` 只有 `{model, redirect_model, disabled}`，没有 per-model
//! 优先级；选渠道是 `ORDER BY c.priority DESC`。也就是说「同一个 provider 在
//! fast 档排第一、在 daily 档排第三」这种**逐档不同的顺序**，用一个渠道表达不了。
//!
//! 所以保存前要做一致性校验：把每一档的 provider 顺序当成偏序约束，检查是否存在
//! 一个全局顺序同时满足所有档。PRD 的 Graph A 恰好存在（grok > glm > claude >
//! kimi > gpt）；Graph B 的种子边不存在（fast 要 grok>glm，daily 要 glm>grok）。
//! 冲突时**拒绝保存**并指出是哪两档打架，让用户自己决定改哪一边 —— 而不是偷偷
//! 挑一个顺序，让用户以为配好了。

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::services::cli_io::write_atomic;

/// 一家 provider。`channel_id` 指向内核里已经存在的渠道 —— 客户端不发明凭据，
/// 也不替用户建渠道。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphProvider {
    pub id: String,
    pub label: String,
    pub enabled: bool,
    pub channel_id: Option<i64>,
    /// 档位 id → 该 provider 在该档的真实上游模型名。
    #[serde(default)]
    pub models: BTreeMap<String, String>,
}

/// 一档。`alias` 是 CLI 侧实际请求的模型名，`providers` 是有序的候选队列。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphTier {
    pub id: String,
    pub label: String,
    pub alias: String,
    pub providers: Vec<String>,
}

/// 角色 → 档。用来生成 agent 文件的 `model:`（PRD §8.1）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphRole {
    pub id: String,
    pub label: String,
    pub tier: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphDoc {
    pub id: String,
    pub label: String,
    pub enabled: bool,
    pub providers: Vec<GraphProvider>,
    pub tiers: Vec<GraphTier>,
    #[serde(default)]
    pub roles: Vec<GraphRole>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct GraphStore {
    #[serde(default)]
    pub graphs: Vec<GraphDoc>,
}

impl GraphStore {
    pub fn load(path: &Path) -> Result<Self, AppError> {
        if !path.exists() {
            return Ok(Self { graphs: seeds() });
        }
        let raw = std::fs::read_to_string(path)?;
        if raw.trim().is_empty() {
            return Ok(Self { graphs: seeds() });
        }
        serde_json::from_str(&raw)
            .map_err(|e| AppError::Config(format!("graphs.json 损坏：{e}")))
    }

    pub fn save(&self, path: &Path) -> Result<(), AppError> {
        let body = serde_json::to_string_pretty(self)
            .map_err(|e| AppError::Config(e.to_string()))?;
        write_atomic(path, &format!("{body}\n"))
    }

    pub fn upsert(&mut self, doc: GraphDoc) {
        match self.graphs.iter().position(|g| g.id == doc.id) {
            Some(i) => self.graphs[i] = doc,
            None => self.graphs.push(doc),
        }
    }
}

// ---------------------------------------------------------------------------
// 校验
// ---------------------------------------------------------------------------

/// 校验结果。`ok=false` 时禁止应用 —— PRD §11.2 要求 fail-fast。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphValidation {
    pub ok: bool,
    pub problems: Vec<String>,
    /// 通过时给出的全局 provider 顺序（越靠前优先级越高）。
    pub global_order: Vec<String>,
    /// 每个 provider 会被写成的渠道优先级。
    pub priorities: BTreeMap<String, i32>,
}

/// 优先级基线。留出上下空间，方便用户手工插别的渠道。
const PRIORITY_TOP: i32 = 90;
const PRIORITY_STEP: i32 = 10;

pub fn validate(doc: &GraphDoc) -> GraphValidation {
    // 用 Vec + 去重而不是 HashSet：顺序要稳定（先说缺什么，再说顺序冲突），
    // 同一条不能因为 provider 参与了三档就重复三遍 —— 那读起来像三个问题。
    let mut problems: Vec<String> = Vec::new();
    let push = |problems: &mut Vec<String>, msg: String| {
        if !problems.contains(&msg) {
            problems.push(msg);
        }
    };
    let by_id: HashMap<&str, &GraphProvider> =
        doc.providers.iter().map(|p| (p.id.as_str(), p)).collect();

    let mut used: HashSet<&str> = HashSet::new();
    for tier in &doc.tiers {
        if tier.alias.trim().is_empty() {
            push(&mut problems, format!("{} 档没有别名，CLI 侧无从请求", tier.label));
        }
        if tier.providers.is_empty() {
            push(&mut problems, format!("{} 档没有任何 provider，该档不可用", tier.label));
            continue;
        }
        for pid in &tier.providers {
            let Some(p) = by_id.get(pid.as_str()) else {
                push(&mut problems, format!("{} 档引用了不存在的 provider「{pid}」", tier.label));
                continue;
            };
            if !p.enabled {
                push(&mut problems, format!("{} 档用到了未启用的 {}", tier.label, p.label));
            }
            if p.channel_id.is_none() {
                push(&mut problems, format!("{} 还没有绑定渠道", p.label));
            }
            match p.models.get(&tier.id) {
                Some(m) if !m.trim().is_empty() => {}
                _ => push(
                    &mut problems,
                    format!("{} 在 {} 档没有填真实上游模型", p.label, tier.label),
                ),
            }
            used.insert(pid.as_str());
        }
    }

    // 每档的顺序 → 两两偏序约束 → 找全局顺序。
    let (order, conflicts) = global_order(&doc.tiers);
    for c in conflicts {
        push(&mut problems, c);
    }

    let mut priorities = BTreeMap::new();
    for (i, pid) in order.iter().enumerate() {
        priorities.insert(pid.clone(), PRIORITY_TOP - (i as i32) * PRIORITY_STEP);
    }

    GraphValidation {
        ok: problems.is_empty(),
        problems,
        global_order: order,
        priorities,
    }
}

/// 从各档的 provider 顺序推一个全局顺序。
///
/// 每档 `[a, b, c]` 意味着 a≻b、a≻c、b≻c。所有档的约束合起来做拓扑排序：
/// 有环就说明「逐档不同的顺序」无法用渠道级优先级表达，把成环的那对约束连同
/// 它们来自哪一档一起报出来。
fn global_order(tiers: &[GraphTier]) -> (Vec<String>, Vec<String>) {
    // (先, 后) → 来自哪一档
    let mut constraint: HashMap<(String, String), String> = HashMap::new();
    let mut conflicts = Vec::new();
    let mut nodes: Vec<String> = Vec::new();

    for tier in tiers {
        for (i, a) in tier.providers.iter().enumerate() {
            if !nodes.contains(a) {
                nodes.push(a.clone());
            }
            for b in tier.providers.iter().skip(i + 1) {
                if let Some(other) = constraint.get(&(b.clone(), a.clone())) {
                    conflicts.push(format!(
                        "{} 档要求 {a} 排在 {b} 前面，但 {other} 档要求反过来；\
                         内核只有渠道级优先级，两者不能同时成立 —— 改其中一档的顺序",
                        tier.label
                    ));
                } else {
                    constraint.insert((a.clone(), b.clone()), tier.label.clone());
                }
            }
        }
    }
    if !conflicts.is_empty() {
        conflicts.sort();
        conflicts.dedup();
        return (Vec::new(), conflicts);
    }

    // Kahn 拓扑排序。入度相同时按首次出现顺序，结果稳定可复现。
    let mut indeg: HashMap<&str, usize> = nodes.iter().map(|n| (n.as_str(), 0)).collect();
    for (a, b) in constraint.keys() {
        if nodes.iter().any(|n| n == a) && nodes.iter().any(|n| n == b) {
            *indeg.get_mut(b.as_str()).unwrap() += 1;
        }
    }
    let mut order = Vec::new();
    let mut remaining: Vec<&str> = nodes.iter().map(String::as_str).collect();
    while !remaining.is_empty() {
        let Some(pos) = remaining.iter().position(|n| indeg[n] == 0) else {
            // 两两不冲突却仍成环（a≻b、b≻c、c≻a 各来自不同档）。
            conflicts.push(format!(
                "provider 顺序在多档之间构成了环（涉及 {}），无法折成一个全局优先级",
                remaining.join("、")
            ));
            return (Vec::new(), conflicts);
        };
        let n = remaining.remove(pos);
        for (a, b) in constraint.keys() {
            if a == n {
                if let Some(d) = indeg.get_mut(b.as_str()) {
                    *d = d.saturating_sub(1);
                }
            }
        }
        order.push(n.to_string());
    }
    (order, conflicts)
}

/// 「某别名会依次打到谁」。纯计算，给 UI 的预览用（PRD §11.1 的 preview）。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewStep {
    pub provider: String,
    pub label: String,
    pub upstream_model: String,
    pub priority: i32,
}

pub fn preview(doc: &GraphDoc, tier_id: &str) -> Vec<PreviewStep> {
    let v = validate(doc);
    let by_id: HashMap<&str, &GraphProvider> =
        doc.providers.iter().map(|p| (p.id.as_str(), p)).collect();
    let Some(tier) = doc.tiers.iter().find(|t| t.id == tier_id) else {
        return Vec::new();
    };
    let mut steps: Vec<PreviewStep> = tier
        .providers
        .iter()
        .filter_map(|pid| {
            let p = by_id.get(pid.as_str())?;
            Some(PreviewStep {
                provider: p.id.clone(),
                label: p.label.clone(),
                upstream_model: p.models.get(&tier.id).cloned().unwrap_or_default(),
                priority: v.priorities.get(&p.id).copied().unwrap_or(0),
            })
        })
        .collect();
    // 实际生效顺序由优先级决定，预览必须按它排，而不是按用户在表里的排列 ——
    // 校验通过时两者一致，不一致正说明有问题。
    steps.sort_by_key(|s| std::cmp::Reverse(s.priority));
    steps
}

// ---------------------------------------------------------------------------
// 种子
// ---------------------------------------------------------------------------

fn provider(id: &str, label: &str, models: &[(&str, &str)]) -> GraphProvider {
    GraphProvider {
        id: id.into(),
        label: label.into(),
        enabled: true,
        channel_id: None,
        models: models
            .iter()
            .map(|(t, m)| ((*t).to_string(), (*m).to_string()))
            .collect(),
    }
}

/// PRD §7 的投影表。用户可以在界面上改成自己渠道里真实存在的模型 ID。
fn seed_providers() -> Vec<GraphProvider> {
    vec![
        provider(
            "claude",
            "Claude",
            &[
                ("fast", "claude-haiku-4-5"),
                ("daily", "claude-sonnet-5"),
                ("deep", "claude-opus-5"),
                ("flagship", "claude-fable-5"),
            ],
        ),
        provider(
            "gpt",
            "GPT",
            &[
                ("fast", "gpt-5.6-mini"),
                ("daily", "gpt-5.6"),
                ("deep", "gpt-5.6-sol"),
                ("flagship", "gpt-5.6-sol"),
            ],
        ),
        provider(
            "grok",
            "Grok",
            &[
                ("fast", "grok-4.1-fast"),
                ("daily", "grok-code-fast-1"),
                ("deep", "grok-4.6"),
                ("flagship", "grok-4.6"),
            ],
        ),
        provider(
            "glm",
            "GLM",
            &[
                ("fast", "glm-4.7-flash"),
                ("daily", "glm-5.2"),
                ("deep", "glm-5.3"),
                ("flagship", "glm-5.3"),
            ],
        ),
        provider(
            "kimi",
            "Kimi",
            &[
                ("fast", "kimi-k3"),
                ("daily", "kimi-k3"),
                ("deep", "kimi-k3"),
                ("flagship", "kimi-k3"),
            ],
        ),
    ]
}

fn tier(id: &str, label: &str, alias: &str, providers: &[&str]) -> GraphTier {
    GraphTier {
        id: id.into(),
        label: label.into(),
        alias: alias.into(),
        providers: providers.iter().map(|s| (*s).to_string()).collect(),
    }
}

fn role(id: &str, label: &str, tier: &str) -> GraphRole {
    GraphRole {
        id: id.into(),
        label: label.into(),
        tier: tier.into(),
    }
}

pub fn seeds() -> Vec<GraphDoc> {
    vec![
        GraphDoc {
            id: "claude-code".into(),
            label: "Claude Code".into(),
            enabled: false,
            providers: seed_providers(),
            // PRD §8.2 的种子边。这一组恰好存在全局顺序
            // （grok > glm > claude > kimi > gpt），可以直接应用。
            tiers: vec![
                tier("fast", "Fast", "haiku", &["grok", "glm", "kimi"]),
                tier("daily", "Daily", "sonnet", &["glm", "claude", "gpt"]),
                tier("deep", "Deep", "opus", &["claude", "kimi", "gpt"]),
                tier("flagship", "Flagship", "fable", &["claude", "kimi"]),
            ],
            roles: vec![
                role("explore", "explore（检索）", "fast"),
                role("scribe", "scribe（记录）", "fast"),
                role("builder", "builder（写代码）", "daily"),
                role("reviewer", "reviewer（评审）", "daily"),
                role("planner", "planner（方案）", "deep"),
                role("lead", "lead（主会话）", "flagship"),
            ],
        },
        GraphDoc {
            id: "grok-build".into(),
            label: "Grok Build".into(),
            enabled: false,
            providers: seed_providers(),
            // PRD §9.2 的种子边**故意保留原样**，尽管它存在矛盾
            // （fast 要 grok≻glm，daily 要 glm≻grok）。保存时校验会把这条冲突
            // 指出来，让用户自己决定改哪一档 —— 偷偷替他调顺序才是坑。
            tiers: vec![
                tier("fast", "Fast", "gb-fast", &["grok", "glm"]),
                tier("daily", "Daily", "gb-daily", &["glm", "grok", "gpt"]),
                tier("deep", "Deep", "gb-deep", &["grok", "claude", "gpt"]),
                tier("flagship", "Flagship", "gb-flagship", &["grok", "claude"]),
                tier("review", "Review", "gb-review", &["gpt", "claude"]),
            ],
            roles: vec![
                role("subagent", "subagent（并行子任务）", "fast"),
                role("reviewer", "reviewer（评审）", "review"),
                role("lead", "lead（主会话）", "deep"),
            ],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claude_graph() -> GraphDoc {
        seeds().into_iter().find(|g| g.id == "claude-code").unwrap()
    }

    #[test]
    fn the_prd_claude_edges_fold_into_one_global_order() {
        let (order, conflicts) = global_order(&claude_graph().tiers);
        assert!(conflicts.is_empty(), "{conflicts:?}");
        // grok ≻ glm ≻ claude ≻ kimi ≻ gpt
        let pos = |p: &str| order.iter().position(|x| x == p).unwrap();
        assert!(pos("grok") < pos("glm"));
        assert!(pos("glm") < pos("claude"));
        assert!(pos("claude") < pos("kimi"));
        assert!(pos("kimi") < pos("gpt"));
    }

    /// PRD 的 Grok Build 种子边自相矛盾，必须报出来而不是随便挑一个顺序。
    #[test]
    fn the_prd_grok_edges_are_reported_as_conflicting() {
        let g = seeds().into_iter().find(|x| x.id == "grok-build").unwrap();
        let (order, conflicts) = global_order(&g.tiers);
        assert!(order.is_empty());
        assert!(!conflicts.is_empty());
        let joined = conflicts.join("\n");
        assert!(joined.contains("grok") && joined.contains("glm"), "{joined}");
    }

    /// 一个 provider 参与三档就把「没绑渠道」说三遍，读起来像三个问题。
    #[test]
    fn the_same_problem_is_reported_once() {
        let g = claude_graph(); // 种子里全都没绑渠道
        let v = validate(&g);
        let claude_msgs = v
            .problems
            .iter()
            .filter(|p| p.contains("Claude 还没有绑定渠道"))
            .count();
        assert_eq!(claude_msgs, 1, "{:?}", v.problems);
    }

    #[test]
    fn a_tier_without_providers_is_rejected() {
        let mut g = claude_graph();
        g.tiers[0].providers.clear();
        let v = validate(&g);
        assert!(!v.ok);
        assert!(v.problems.iter().any(|p| p.contains("没有任何 provider")));
    }

    #[test]
    fn a_provider_without_a_channel_is_rejected() {
        let g = claude_graph();
        let v = validate(&g);
        assert!(!v.ok, "种子里没绑渠道，必须拦下来");
        assert!(v.problems.iter().any(|p| p.contains("还没有绑定渠道")));
    }

    #[test]
    fn a_fully_bound_graph_validates_and_assigns_descending_priorities() {
        let mut g = claude_graph();
        for (i, p) in g.providers.iter_mut().enumerate() {
            p.channel_id = Some(i as i64 + 1);
        }
        let v = validate(&g);
        assert!(v.ok, "{:?}", v.problems);
        // 队首的 provider 优先级最高。
        assert_eq!(v.priorities["grok"], PRIORITY_TOP);
        assert!(v.priorities["grok"] > v.priorities["gpt"]);
    }

    /// 预览按**实际生效的优先级**排序，而不是按表里的排列。
    #[test]
    fn preview_lists_the_queue_in_priority_order() {
        let mut g = claude_graph();
        for (i, p) in g.providers.iter_mut().enumerate() {
            p.channel_id = Some(i as i64 + 1);
        }
        let steps = preview(&g, "deep");
        let names: Vec<&str> = steps.iter().map(|s| s.provider.as_str()).collect();
        assert_eq!(names, ["claude", "kimi", "gpt"]);
        assert_eq!(steps[0].upstream_model, "claude-opus-5");
    }

    /// 一个别名只挂它自己那一档的模型 —— 内核换渠道时不可能换成别档的模型，
    /// 这就是 PRD §10「禁止降档」在这套静态实现下成立的原因。
    #[test]
    fn an_alias_only_ever_maps_to_its_own_tier() {
        let g = claude_graph();
        for tier in &g.tiers {
            for pid in &tier.providers {
                let p = g.providers.iter().find(|p| &p.id == pid).unwrap();
                assert!(
                    p.models.contains_key(&tier.id),
                    "{} 档缺 {} 的模型",
                    tier.id,
                    pid
                );
            }
        }
    }
}
