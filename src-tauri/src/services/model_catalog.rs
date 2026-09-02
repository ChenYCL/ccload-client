//! models.dev 目录：模型上下文窗口的**第三方**来源。
//!
//! 为什么需要它：内置的家族猜测表（`context_window::family_window`）是靠名字
//! 里的关键字猜的，而模型迭代比这张表快。实测拿 models.dev 的第一方数据对了
//! 一遍，134 条里 64 条对不上 —— 有低估（`claude-sonnet-4-5` 我们估 200k，实际
//! 1M，这就是「界面显示 200k、实际能吃 1M」的来历），更要命的是高估
//! （`gpt-5` 我们估 1M、实际 400k；`glm-4.5` 估 200k、实际 131k）。高估的方向
//! 会直接踩进 `context_window` 模块开头讲的死锁：等 CLI 按假天花板触发 compact
//! 时，会话早已超过真实上限，而 compact 自己也要把整段发出去。
//!
//! 所以优先级是：**名字里显式挂的 `[1m]`/`[500k]` → models.dev → 家族猜测**。
//! 猜测只在离线、或目录里查不到这个 id（自建别名、中转改过名）时兜底。
//!
//! 目录是**尽力而为**的：拉不到就用磁盘缓存，没缓存就退回猜测表。窗口是个
//! 优化，不该因为一次网络失败让接管写不下去。

use std::collections::HashMap;
use std::path::Path;
use std::sync::RwLock;

use serde::{Deserialize, Serialize};

use crate::error::AppError;

const API_URL: &str = "https://models.dev/api.json";

/// 缓存多久算新鲜。模型清单是天级变化的东西，一天拉一次足够。
const TTL_SECS: i64 = 24 * 60 * 60;

/// 同一个模型 id 在多个 provider 下出现时，优先信这些「第一方」的数字。
///
/// 中转/聚合站常常把窗口标成自己截断后的值（同一个 `glm-5.2` 有报 262k 的，
/// 也有报 1M 的）。厂商自己的那条才是模型的真实天花板；「我这条中转其实更小」
/// 由用户在总控里用「上限」夹，而不是让目录去猜。
const FIRST_PARTY: &[&str] = &[
    "anthropic",
    "openai",
    "google",
    "xai",
    "zhipuai",
    "z-ai",
    "deepseek",
    "moonshotai",
    "mistral",
    "meta",
    "alibaba",
    "qwen",
];

/// 蒸馏后的目录：模型 id → 上下文窗口。
///
/// 原始 api.json 有 4MB+，99% 的字段我们不用。只留这一张表，落盘就是一百来 KB
/// —— 下次启动不必再拉一遍 4MB 才能回答「opus-5 多大」。
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Catalog {
    /// unix 秒。判过期用。
    pub fetched_at: i64,
    pub windows: HashMap<String, u64>,
}

static CATALOG: RwLock<Option<Catalog>> = RwLock::new(None);

/// 目录里有多少条，以及是什么时候拉的。给设置页显示「数据来源」用。
pub fn stats() -> Option<(usize, i64)> {
    let g = CATALOG.read().ok()?;
    let c = g.as_ref()?;
    Some((c.windows.len(), c.fetched_at))
}

/// 查一个别名的窗口。查不到返回 None，调用方退回家族猜测。
///
/// 三段匹配，越靠前越可信：
///   1. 精确命中；
///   2. 目录里某个 id 是这个名字的**前缀**（`claude-opus-5-20260115` → `claude-opus-5`）；
///   3. 目录里某个 id 被这个名字**包含**（中转在前后加了料）。
///
/// 2、3 都取**最长**的那个键，而不是「唯一匹配才算」：`glm-5.3-flash` 同时被
/// `glm-5` 和 `glm-5.3-flash` 包含，最长的那个才是对的。
pub fn lookup(alias: &str) -> Option<u64> {
    let g = CATALOG.read().ok()?;
    let cat = g.as_ref()?;
    let name = normalize(alias);
    if name.is_empty() {
        return None;
    }
    if let Some(w) = cat.windows.get(&name) {
        return Some(*w);
    }
    let mut best: Option<(usize, u64)> = None;
    for (k, v) in &cat.windows {
        // 太短的键会乱咬（"o1" 能被无数名字包含）。前缀匹配至少 4 个字符起。
        if k.len() < 4 {
            continue;
        }
        let hit = name.starts_with(k.as_str()) || name.contains(k.as_str());
        if hit && best.is_none_or(|(len, _)| k.len() > len) {
            best = Some((k.len(), *v));
        }
    }
    best.map(|(_, w)| w)
}

/// 目录里的键和查询名统一成小写、去掉厂商前缀和我们自己挂的 `[1m]` 后缀。
fn normalize(alias: &str) -> String {
    let s = alias.trim();
    let s = match s.rsplit_once('/') {
        Some((_, rest)) if !rest.is_empty() => rest,
        _ => s,
    };
    let s = match (s.rfind('['), s.ends_with(']')) {
        (Some(i), true) => &s[..i],
        _ => s,
    };
    s.trim().to_ascii_lowercase()
}

/// 把磁盘缓存装进内存。启动时先跑这个 —— 哪怕后面拉取失败，也已经有数据可用。
pub fn load_cache(path: &Path) -> bool {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return false;
    };
    match serde_json::from_str::<Catalog>(&raw) {
        Ok(c) if !c.windows.is_empty() => {
            *CATALOG.write().expect("catalog lock") = Some(c);
            true
        }
        _ => false,
    }
}

/// 缓存是不是已经旧了（或者压根没有）。
pub fn is_stale() -> bool {
    let Ok(g) = CATALOG.read() else {
        return true;
    };
    match g.as_ref() {
        Some(c) => now_secs() - c.fetched_at > TTL_SECS,
        None => true,
    }
}

/// 拉一次 models.dev，蒸馏、装进内存、写缓存。返回收录了多少个模型。
pub async fn refresh(client: &reqwest::Client, cache_path: &Path) -> Result<usize, AppError> {
    let body: HashMap<String, Provider> = client
        .get(API_URL)
        .send()
        .await
        .map_err(|e| AppError::Io(format!("models.dev 拉取失败：{e}")))?
        .json()
        .await
        .map_err(|e| AppError::Config(format!("models.dev 返回的不是预期的 JSON：{e}")))?;

    let cat = distill(body);
    let n = cat.windows.len();
    if n == 0 {
        return Err(AppError::Config("models.dev 一条带窗口的模型都没有".into()));
    }
    if let Some(dir) = cache_path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // 缓存写失败不算错：内存里已经有了，下次启动大不了再拉一遍。
    if let Ok(body) = serde_json::to_string(&cat) {
        let _ = crate::services::cli_io::write_atomic(cache_path, &body);
    }
    *CATALOG.write().expect("catalog lock") = Some(cat);
    Ok(n)
}

/// 启动时跑一次：先把磁盘缓存装进内存（离线也能用），旧了再去网上拉。
///
/// 全程不返回错误，也绝不阻塞启动 —— 拉不到就用缓存，没缓存就退回猜测表。
/// 窗口是个优化，不该因为一次网络失败让接管写不下去。
pub async fn startup(cache_path: &Path) {
    let had_cache = load_cache(cache_path);
    if !is_stale() {
        return;
    }
    // 这里**不**用内核那个客户端：它带 `.no_proxy()`（内核在环回或用户指定的
    // 出口代理上），而 models.dev 是公网站点，该跟随系统代理。
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("models.dev 客户端构建失败：{e}");
            return;
        }
    };
    match refresh(&client, cache_path).await {
        Ok(n) => tracing::info!("models.dev 目录已更新：{n} 个模型"),
        Err(e) if had_cache => {
            tracing::warn!("models.dev 更新失败，继续用磁盘缓存：{e}")
        }
        Err(e) => tracing::warn!("models.dev 拉取失败，本次退回内置猜测表：{e}"),
    }
}

#[derive(Deserialize)]
struct Provider {
    #[serde(default)]
    models: HashMap<String, Model>,
}

#[derive(Deserialize)]
struct Model {
    #[serde(default)]
    limit: Option<Limit>,
}

#[derive(Deserialize)]
struct Limit {
    #[serde(default)]
    context: Option<u64>,
}

/// 4MB 的响应 → 一张 id→窗口 的表。同 id 多 provider 时第一方优先，
/// 都不是第一方就取最大的那个（中转标小多半是它自己截断的，不是模型的上限）。
fn distill(body: HashMap<String, Provider>) -> Catalog {
    let mut windows: HashMap<String, u64> = HashMap::new();
    let mut from_first_party: HashMap<String, bool> = HashMap::new();

    for (pid, provider) in body {
        let first = FIRST_PARTY.contains(&pid.to_ascii_lowercase().as_str());
        for (mid, m) in provider.models {
            let Some(ctx) = m.limit.and_then(|l| l.context).filter(|c| *c > 0) else {
                continue;
            };
            let key = normalize(&mid);
            if key.is_empty() {
                continue;
            }
            match (from_first_party.get(&key).copied(), first) {
                // 已经有第一方的了，非第一方别覆盖。
                (Some(true), false) => {}
                // 第一方来了，直接顶掉之前的猜测。
                (_, true) => {
                    windows.insert(key.clone(), ctx);
                    from_first_party.insert(key, true);
                }
                // 都不是第一方：取大的。
                _ => {
                    let e = windows.entry(key.clone()).or_insert(ctx);
                    *e = (*e).max(ctx);
                    from_first_party.entry(key).or_insert(false);
                }
            }
        }
    }
    Catalog {
        fetched_at: now_secs(),
        windows,
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
pub(crate) fn set_for_test(pairs: &[(&str, u64)]) {
    let windows = pairs
        .iter()
        .map(|(k, v)| (k.to_string(), *v))
        .collect::<HashMap<_, _>>();
    *CATALOG.write().unwrap() = Some(Catalog {
        fetched_at: now_secs(),
        windows,
    });
}

#[cfg(test)]
pub(crate) fn clear_for_test() {
    *CATALOG.write().unwrap() = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(models: &[(&str, u64)]) -> Provider {
        Provider {
            models: models
                .iter()
                .map(|(id, ctx)| {
                    (
                        id.to_string(),
                        Model {
                            limit: Some(Limit { context: Some(*ctx) }),
                        },
                    )
                })
                .collect(),
        }
    }

    /// 同一个 id 在中转和厂商下各报一个数时，必须信厂商那条。中转报的多半是
    /// 它自己截断后的值 —— 拿它当模型上限，等于替所有人把窗口砍掉。
    #[test]
    fn first_party_beats_a_relay_for_the_same_id() {
        let body = HashMap::from([
            ("some-relay".to_string(), provider(&[("glm-5.2", 262_144)])),
            ("zhipuai".to_string(), provider(&[("glm-5.2", 1_000_000)])),
        ]);
        assert_eq!(distill(body).windows.get("glm-5.2"), Some(&1_000_000));
    }

    /// 顺序反过来结果必须一样 —— HashMap 的遍历顺序是随机的，靠「谁先来谁赢」
    /// 会让同一份输入每次跑出不同的窗口。
    #[test]
    fn first_party_wins_regardless_of_iteration_order() {
        for _ in 0..8 {
            let body = HashMap::from([
                ("zhipuai".to_string(), provider(&[("glm-5.2", 1_000_000)])),
                ("aaa-relay".to_string(), provider(&[("glm-5.2", 262_144)])),
                ("zzz-relay".to_string(), provider(&[("glm-5.2", 131_072)])),
            ]);
            assert_eq!(distill(body).windows.get("glm-5.2"), Some(&1_000_000));
        }
    }

    /// 都不是第一方时取最大：小的那个通常是某家自己的截断。
    #[test]
    fn without_a_first_party_entry_the_widest_wins() {
        let body = HashMap::from([
            ("relay-a".to_string(), provider(&[("mystery-1", 128_000)])),
            ("relay-b".to_string(), provider(&[("mystery-1", 400_000)])),
        ]);
        assert_eq!(distill(body).windows.get("mystery-1"), Some(&400_000));
    }

    #[test]
    fn lookup_matches_exact_then_prefix_then_substring() {
        set_for_test(&[
            ("claude-opus-5", 1_000_000),
            ("glm-5.3-flash", 1_000_000),
            ("glm-5", 204_800),
        ]);
        assert_eq!(lookup("claude-opus-5"), Some(1_000_000));
        // 带日期后缀的变体走前缀。
        assert_eq!(lookup("claude-opus-5-20260115"), Some(1_000_000));
        // 厂商前缀和我们自己挂的 [1m] 后缀都要先剥掉。
        assert_eq!(lookup("anthropic/claude-opus-5[1m]"), Some(1_000_000));
        // 同时被 glm-5 和 glm-5.3-flash 命中时，最长的那个才是对的。
        assert_eq!(lookup("glm-5.3-flash"), Some(1_000_000));
        assert_eq!(lookup("unknown-model-xyz"), None);
        clear_for_test();
    }

    /// 空目录必须干干净净地返回 None，好让调用方退回猜测表 —— 而不是 panic
    /// 或者给一个 0。离线首次启动就是这个状态。
    #[test]
    fn an_empty_catalog_defers_to_the_caller() {
        clear_for_test();
        assert_eq!(lookup("claude-opus-5"), None);
        assert!(is_stale(), "没有目录时必须算过期，否则永远不会去拉");
    }
}
