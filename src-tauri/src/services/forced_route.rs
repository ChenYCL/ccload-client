//! 强制路由 —— ai-go 那种「CLI 请求某个模型，就把它钉到你选的渠道 + 上游模型」。
//!
//! 和「模型链」的区别是**心智**，不是机制：模型链是「主力冷了往下降级」，要校验
//! 上游、讲优雅退化；强制路由是「我说发去哪就发去哪」，不管上游认不认这个名字，
//! 照发（`不管上游如何，反正就是发`）。底层落到渠道上的写法两者一样 —— 都靠
//! [`crate::services::channel_writer::patch_channel`] 把 `{model: 别名,
//! redirect_model: 上游}` 合并进渠道并设优先级，所以这里只管**存**，不重抄那段
//! 写渠道的坑。
//!
//! 一条路由 = 一个请求别名 → 一组目标（渠道 + 上游模型）。目标可以多于一个：
//! 第一个优先级最高，命中即用；后面的是同一个别名的备用落点。绝大多数场景只配
//! 一个目标（纯强制到一处）。

use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::services::cli_io::write_atomic;

/// 一个落点：把请求别名强制发到这个渠道的这个上游模型。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ForcedTarget {
    /// 绑定的渠道。`None` 时 apply 会跳过这一个并记进日志 —— 没绑渠道就没有落点。
    #[serde(default)]
    pub channel_id: Option<i64>,
    /// 渠道名，存一份让列表页不必再查渠道表。
    #[serde(default)]
    pub channel_name: Option<String>,
    /// 发给上游的真实模型名。**不校验上游清单**：级联下拉只是给候选，手填任意
    /// 名字照样存、照样发。
    pub model: String,
}

/// 一条强制路由。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ForcedRoute {
    /// CLI 请求时写的模型名（例如 `claude-fable-5`）。命中它就强制走 `targets`。
    pub from: String,
    /// 落点，按顺序：第一个优先级最高。
    pub targets: Vec<ForcedTarget>,
}

/// 落在 `~/.ccload-client/forced_route.json`，和模型链的 `fallback.json` 分开 ——
/// 是两个概念，不该混进同一个列表。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ForcedRouteStore {
    #[serde(default)]
    pub routes: Vec<ForcedRoute>,
}

impl ForcedRouteStore {
    pub fn load(path: &std::path::Path) -> Result<Self, AppError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(path)?;
        if raw.trim().is_empty() {
            return Ok(Self::default());
        }
        serde_json::from_str(&raw)
            .map_err(|e| AppError::Config(format!("forced_route.json 坏了：{e}")))
    }

    pub fn save(&self, path: &std::path::Path) -> Result<(), AppError> {
        let body =
            serde_json::to_string_pretty(self).map_err(|e| AppError::Config(e.to_string()))?;
        // write_atomic：两个并发 save 各拼一个由目标名推出的 `.tmp` 会互相截断，
        // rename 过去就是两个文档首尾相接 —— 也就是 load 那句「坏了」的来历。
        write_atomic(path, &format!("{body}\n"))
    }

    /// 按 `from` upsert：同一个请求别名只保留一条路由，重存即覆盖。
    pub fn upsert(&mut self, route: ForcedRoute) {
        if let Some(existing) = self.routes.iter_mut().find(|r| r.from == route.from) {
            *existing = route;
        } else {
            self.routes.push(route);
        }
    }

    pub fn remove(&mut self, from: &str) {
        self.routes.retain(|r| r.from != from);
    }
}

/// 第 `i` 个目标的优先级。第一个最高（内核按 priority DESC 选渠道），后面的依次
/// 递减，和模型链用同一把梯子 —— 复用它，别让两处的公式各走各的。
pub use crate::services::fallback::hop_priority as target_priority;

/// 每个目标该写的优先级，保证**压过**现有服务该别名的渠道。
///
/// `incumbent_max` = 其它（非本路由目标）启用渠道里，也服务这个别名的最高优先级；
/// `None` 表示没有别的渠道抢，退回 100/90/80… 的老梯子。
///
/// 为什么不能一律写 100：内核对**同优先级**的渠道做加权轮询
/// （`selector_balancer.go:balanceSamePriorityChannels` 同优先级组内 RR）。如果现有
/// 渠道已经在 100 上服务这个别名（比如 Anthropic 渠道就在 100 上服务
/// `claude-fable-5`），你把目标也钉在 100 就是和它平分流量，而不是独占 —— 一个只
/// 赢一半的「强制」不算强制。所以有 incumbent 时，所有目标都排到它上面。
pub fn winning_priorities(incumbent_max: Option<i32>, n: usize) -> Vec<i32> {
    let start = match incumbent_max {
        // 压过 incumbent：最高目标 = incumbent + 10*n，逐个 -10，最低也仍 > incumbent。
        Some(p) => p.saturating_add(10 * n as i32),
        // 无人争：老梯子，第一个 100。
        None => 100,
    };
    (0..n as i32).map(|i| start - i * 10).collect()
}

/// 存之前先验：空别名会匹配到一切、没有目标等于什么都不发、目标模型为空是把一个
/// 空字符串发给上游。三者都比拒绝保存更糟。
pub fn validate_route(route: &ForcedRoute) -> Result<(), AppError> {
    if route.from.trim().is_empty() {
        return Err(AppError::Config("请求别名不能为空".into()));
    }
    if route.targets.is_empty() {
        return Err(AppError::Config("至少要有一个目标，否则这条路由什么都不发".into()));
    }
    for (i, tgt) in route.targets.iter().enumerate() {
        if tgt.model.trim().is_empty() {
            return Err(AppError::Config(format!("第 {} 个目标的模型名是空的", i + 1)));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(model: &str, ch: Option<i64>) -> ForcedTarget {
        ForcedTarget {
            channel_id: ch,
            channel_name: ch.map(|id| format!("ch{id}")),
            model: model.into(),
        }
    }

    /// 优先级梯子和模型链是同一把 —— 复用而不是各写一份，防止两处漂开。
    #[test]
    fn target_priority_matches_the_chain_ladder() {
        assert_eq!(target_priority(0), 100);
        assert_eq!(target_priority(1), 90);
        assert_eq!(target_priority(2), 80);
    }

    /// 无人争抢这个别名时，退回老梯子 100/90/80。
    #[test]
    fn winning_priorities_no_incumbent_uses_legacy_ladder() {
        assert_eq!(winning_priorities(None, 1), vec![100]);
        assert_eq!(winning_priorities(None, 3), vec![100, 90, 80]);
    }

    /// 有 incumbent 时，**所有**目标都要压过它 —— 否则「强制」只是「平分」。
    /// 这条盯的是活体验证里暴露的真问题：Anthropic 渠道在 100 上服务 claude-fable-5，
    /// 目标也钉 100 就同组轮询各拿一半。
    #[test]
    fn winning_priorities_beat_the_incumbent() {
        // 单目标压过 100 的在位者
        assert_eq!(winning_priorities(Some(100), 1), vec![110]);
        // 多目标：全部 > 100，且自身降序
        let p = winning_priorities(Some(100), 3);
        assert_eq!(p, vec![130, 120, 110]);
        assert!(p.iter().all(|&x| x > 100), "有目标没压过在位者：{p:?}");
        assert!(p.windows(2).all(|w| w[0] > w[1]), "目标之间不是降序：{p:?}");
        // 在位者优先级低时也只要压过它即可，不必虚高到 100
        assert_eq!(winning_priorities(Some(40), 1), vec![50]);
    }

    #[test]
    fn rejects_empty_from() {
        let r = ForcedRoute {
            from: "  ".into(),
            targets: vec![target("grok-4.5", Some(11))],
        };
        assert!(validate_route(&r).is_err());
    }

    #[test]
    fn rejects_no_targets() {
        let r = ForcedRoute {
            from: "claude-fable-5".into(),
            targets: vec![],
        };
        assert!(validate_route(&r).is_err());
    }

    #[test]
    fn rejects_empty_target_model() {
        let r = ForcedRoute {
            from: "claude-fable-5".into(),
            targets: vec![target("  ", Some(11))],
        };
        assert!(validate_route(&r).is_err());
    }

    /// 不校验上游：目标模型是个上游根本没有的名字，照样存下来（照发是用户的决定）。
    #[test]
    fn accepts_a_model_the_upstream_never_advertised() {
        let r = ForcedRoute {
            from: "claude-fable-5".into(),
            targets: vec![target("some-model-upstream-never-heard-of", Some(11))],
        };
        assert!(validate_route(&r).is_ok());
    }

    /// 没绑渠道的目标也允许存 —— apply 时跳过并记日志，而不是保存时就拦。用户可能
    /// 先把名字填好、渠道晚点再选。
    #[test]
    fn accepts_a_target_without_a_channel() {
        let r = ForcedRoute {
            from: "claude-fable-5".into(),
            targets: vec![target("grok-4.5", None)],
        };
        assert!(validate_route(&r).is_ok());
    }

    #[test]
    fn upsert_replaces_same_from() {
        let mut store = ForcedRouteStore::default();
        store.upsert(ForcedRoute {
            from: "claude-fable-5".into(),
            targets: vec![target("grok-4.5", Some(11))],
        });
        store.upsert(ForcedRoute {
            from: "claude-fable-5".into(),
            targets: vec![target("grok-4.5", Some(11)), target("opus-5", Some(12))],
        });
        assert_eq!(store.routes.len(), 1);
        assert_eq!(store.routes[0].targets.len(), 2);
    }

    #[test]
    fn remove_drops_the_route() {
        let mut store = ForcedRouteStore::default();
        store.upsert(ForcedRoute {
            from: "a".into(),
            targets: vec![target("m", Some(1))],
        });
        store.remove("a");
        assert!(store.routes.is_empty());
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = std::env::temp_dir().join(format!(
            "ccload-forced-{}",
            crate::services::session_rescue::uuid_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("forced_route.json");
        let mut store = ForcedRouteStore::default();
        store.upsert(ForcedRoute {
            from: "claude-fable-5".into(),
            targets: vec![target("grok-4.5", Some(11))],
        });
        store.save(&path).unwrap();
        let loaded = ForcedRouteStore::load(&path).unwrap();
        assert_eq!(loaded.routes, store.routes);
        std::fs::remove_dir_all(&dir).ok();
    }
}
