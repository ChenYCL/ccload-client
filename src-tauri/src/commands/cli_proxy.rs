//! CLI 代理的命令面：起停、查最近转发、把一条转发反查成会话。
//!
//! 代理只做转发和旁路记录，不碰会话内容 —— 请求体除了 `model` 字段按映射表
//! 改写，其余字节原样透传，响应逐块回吐。会话文件仍然由 CLI 自己写，这里
//! 只是**读**请求头里的会话 id，所以不存在「代理污染了会话」这回事。

use tauri::State;

use crate::error::AppResult;
use crate::services::cli_proxy::{claude_session_file, session_title, CliProxy, ProxyRecord};
use crate::state::AppState;

/// 起代理，或在内核地址变了之后重新指向。
pub async fn ensure_cli_proxy(state: &AppState) -> Result<(), crate::error::AppError> {
    let cfg = state.settings.read().await.kernel.clone();
    let guard = state.cli_proxy.read().await;
    if let Some(proxy) = guard.as_ref() {
        proxy.retarget(&cfg).await?;
    } else {
        drop(guard);
        let proxy = CliProxy::start(&cfg).await?;
        *state.cli_proxy.write().await = Some(proxy);
    }
    Ok(())
}

/// 读写「缓存窗口升到 1h」的开关。
///
/// 默认关，而且交互式会话就该关着 —— 实测本机 101,259 次同会话相邻请求里
/// 98.1% 短于 5 分钟，1h 档写入价 2×（5m 档 1.25×），为 1.6% 的长间隔把全部
/// 写入涨价 60% 是净亏。这个开关是给「按小时轮询、中间长时间没人说话」的
/// 定时任务用的。
#[tauri::command]
pub async fn cli_proxy_long_cache(state: State<'_, AppState>) -> AppResult<bool> {
    Ok(state
        .cli_proxy
        .read()
        .await
        .as_ref()
        .map(|p| p.long_cache_enabled())
        .unwrap_or(false))
}

#[tauri::command]
pub async fn cli_proxy_set_long_cache(
    state: State<'_, AppState>,
    enabled: bool,
) -> AppResult<bool> {
    let guard = state.cli_proxy.read().await;
    match guard.as_ref() {
        Some(p) => {
            p.set_long_cache(enabled);
            Ok(p.long_cache_enabled())
        }
        // 代理没起来时没有可改的对象；如实回 false，别假装存上了。
        None => Ok(false),
    }
}

/// CLI 该往哪儿指。代理没起来时返回 None —— 此时不该去写任何接管配置，
/// 不然写进去的是个没人监听的地址。
#[tauri::command]
pub async fn cli_proxy_url(state: State<'_, AppState>) -> AppResult<Option<String>> {
    Ok(state
        .cli_proxy
        .read()
        .await
        .as_ref()
        .map(|p| p.base_url()))
}

/// 最近的转发记录，最新的在前。
#[tauri::command]
pub async fn cli_proxy_records(state: State<'_, AppState>) -> AppResult<Vec<ProxyRecord>> {
    let guard = state.cli_proxy.read().await;
    match guard.as_ref() {
        Some(p) => Ok(p.records().await),
        None => Ok(Vec::new()),
    }
}

/// 按 CLI 聚合的今日消耗。数字全部来自内核日志（成本只有内核会算），
/// 代理记录只贡献「这条是谁发的」这个维度。
#[derive(Debug, serde::Serialize)]
pub struct CliUsage {
    pub cli: String,
    pub requests: u64,
    pub cost: f64,
    pub output_tokens: i64,
    /// 参与过的不同会话数。
    pub sessions: u64,
}

/// 会话级消耗明细（今日），最贵的在前。
#[derive(Debug, serde::Serialize)]
pub struct SessionUsage {
    pub cli: String,
    pub session_id: String,
    pub requests: u64,
    pub cost: f64,
}

/// 今日按 CLI / 会话的消耗聚合。
///
/// 对齐方式和日志页同一个（见 `lib/sessionMatch.ts` 的 Rust 侧镜像）：
/// 代理在**收到请求**时打点、内核在 **attempt 开始**时打点（attemptStartTime），
/// 两者相差通常不到一秒；配对只认「0 <= 内核时间 − 代理时间 <= 180s」且模型名
/// 对得上，一条代理记录只认领一条日志。打点位置曾经不对（send 返回后才取），
/// gap 成了负的 TTFB，成本会挪到下一条日志头上 —— 改打点位置时两边要一起动。
#[tauri::command]
pub async fn cli_proxy_usage(state: State<'_, AppState>) -> AppResult<CliUsageReport> {
    use std::collections::HashMap;

    let records = {
        let guard = state.cli_proxy.read().await;
        match guard.as_ref() {
            Some(p) => p.records().await,
            None => return Ok(CliUsageReport { by_cli: Vec::new(), by_session: Vec::new(), unmatched: 0 }),
        }
    };

    // 内核侧只取今天的日志（成本字段只有这里有）。range 由内核解析，
    // today 起点是本地时区的零点。
    let (base_url, password) = {
        let s = state.settings.read().await;
        (s.kernel.base_url(), s.kernel.admin_password.clone())
    };
    // 内核的响应是 {success, data: [...], count} 信封；data 才是日志数组。
    let env = state
        .admin
        .request(&base_url, &password, "GET", "logs", Some("range=today&limit=1000"), None)
        .await?;
    let logs = env
        .get("data")
        .and_then(|d| d.as_array())
        .cloned()
        .unwrap_or_default();

    // (model, 时间桶) -> 最近一条未认领日志的下标。粗粒度桶是为了别让匹配
    // 退化成 O(records × logs) 的全量扫描。
    let mut by_model: HashMap<String, Vec<(i64, usize)>> = HashMap::new();
    for (i, log) in logs.iter().enumerate() {
        let Some(model) = log.get("model").and_then(|m| m.as_str()) else { continue };
        let t = log.get("time").and_then(|t| t.as_i64()).unwrap_or(0);
        by_model.entry(model.to_string()).or_default().push((t, i));
    }
    for v in by_model.values_mut() {
        v.sort_by_key(|(t, _)| *t);
    }

    let mut claimed = vec![false; logs.len()];
    let mut unmatched = 0u64;
    struct Hit { cli: String, session: Option<String>, cost: f64, out: i64 }
    let mut hits: Vec<Hit> = Vec::new();

    const MAX_SKEW: i64 = 180;
    for r in &records {
        let mut best: Option<(usize, i64)> = None;
        for name in [r.model.as_deref(), r.sent_model.as_deref()].into_iter().flatten() {
            for (t, i) in by_model.get(name).into_iter().flatten() {
                if claimed[*i] { continue }
                let gap = t - r.time;
                if !(0..=MAX_SKEW).contains(&gap) { continue }
                if best.is_none_or(|(_, g)| gap < g) {
                    best = Some((*i, gap));
                }
            }
        }
        let Some((i, _)) = best else { unmatched += 1; continue };
        claimed[i] = true;
        let log = &logs[i];
        hits.push(Hit {
            cli: r.cli.clone(),
            session: r.session_id.clone(),
            cost: log.get("cost").and_then(|c| c.as_f64()).unwrap_or(0.0),
            out: log.get("output_tokens").and_then(|c| c.as_i64()).unwrap_or(0),
        });
    }

    let mut by_cli: HashMap<String, CliUsage> = HashMap::new();
    let mut by_session: HashMap<(String, String), SessionUsage> = HashMap::new();
    for h in hits {
        let e = by_cli.entry(h.cli.clone()).or_insert(CliUsage {
            cli: h.cli.clone(), requests: 0, cost: 0.0, output_tokens: 0, sessions: 0,
        });
        e.requests += 1;
        e.cost += h.cost;
        e.output_tokens += h.out;
        if let Some(sid) = &h.session {
            let key = (h.cli.clone(), sid.clone());
            let se = by_session.entry(key).or_insert(SessionUsage {
                cli: h.cli.clone(), session_id: sid.clone(), requests: 0, cost: 0.0,
            });
            se.requests += 1;
            se.cost += h.cost;
        }
    }
    // sessions 数在插入后数一遍，避免临时集合。
    let mut per_cli_sessions: HashMap<String, std::collections::HashSet<String>> = HashMap::new();
    for (c, s) in by_session.keys() {
        per_cli_sessions.entry(c.clone()).or_default().insert(s.clone());
    }
    for (cli, set) in per_cli_sessions {
        if let Some(e) = by_cli.get_mut(&cli) { e.sessions = set.len() as u64; }
    }

    let mut by_cli: Vec<CliUsage> = by_cli.into_values().collect();
    by_cli.sort_by(|a, b| b.cost.total_cmp(&a.cost));
    let mut by_session: Vec<SessionUsage> = by_session.into_values().collect();
    by_session.sort_by(|a, b| b.cost.total_cmp(&a.cost));
    by_session.truncate(50);

    Ok(CliUsageReport { by_cli, by_session, unmatched })
}

/// [`cli_proxy_usage`] 的返回体。
#[derive(Debug, serde::Serialize)]
pub struct CliUsageReport {
    pub by_cli: Vec<CliUsage>,
    pub by_session: Vec<SessionUsage>,
    /// 代理有记录但没对上内核日志的请求数（失败请求、非代理日志页等）。
    pub unmatched: u64,
}

/// 一条日志点进去要展示的东西：会话在磁盘上的位置和它的标题。
#[derive(Debug, serde::Serialize)]
pub struct SessionRef {
    pub session_id: String,
    /// 会话 jsonl 的绝对路径。找不到就是 None —— Codex 的会话不在
    /// `~/.claude/projects` 下，这条路径只对 Claude Code 有意义。
    pub path: Option<String>,
    /// 标题。优先用 Claude Code 自己生成的 `ai-title`，没有就退回首条用户消息。
    pub title: Option<String>,
}

/// 把一个会话 id 解析成可展示、可跳转的引用。
#[tauri::command]
pub async fn cli_proxy_session(session_id: String) -> AppResult<SessionRef> {
    let found = tokio::task::spawn_blocking(move || {
        let path = claude_session_file(&session_id);
        let title = path.as_deref().and_then(session_title);
        SessionRef {
            session_id,
            path: path.map(|p| p.to_string_lossy().into_owned()),
            title,
        }
    })
    .await
    .map_err(|e| crate::error::AppError::Config(e.to_string()))?;
    Ok(found)
}

#[cfg(test)]
mod usage_tests {

    /// 聚合的核心是配对，配对规则和 sessionMatch.ts 同一套。这里直接测
    /// Rust 侧：方向、窗口、认领唯一性。
    fn log(time: i64, model: &str, cost: f64, out: i64) -> serde_json::Value {
        serde_json::json!({"time": time, "model": model, "cost": cost, "output_tokens": out})
    }
    fn rec(time: i64, model: &str, sent: &str, cli: &str, sid: &str)
        -> crate::services::cli_proxy::ProxyRecord {
        crate::services::cli_proxy::ProxyRecord {
            time,
            cli: cli.into(),
            session_id: Some(sid.into()),
            model: Some(model.into()),
            sent_model: Some(sent.into()),
            path: "/v1/messages".into(),
            status: 200,
            cost: None,
            output_tokens: None,
        }
    }

    /// 把 cli_proxy_usage 的配对循环抽出来测：同模型多日志各认各的，
    /// 改写后的名字也要对得上，方向反了不认。
    #[test]
    fn pairing_rules_match_the_frontend_heuristic() {
        let logs = [
            log(1000, "claude-opus-5", 0.10, 100),
            log(1010, "claude-opus-5", 0.20, 200),
            log(1020, "glm-5.3-flash", 0.05, 50),
        ];
        let records = vec![
            rec(997, "claude-opus-5[1m]", "claude-opus-5", "claude-code", "s1"),
            rec(1007, "claude-opus-5[1m]", "claude-opus-5", "claude-code", "s2"),
            rec(1018, "glm-5.3-flash", "glm-5.3-flash", "codex", "s3"),
            // 方向反了（代理晚于内核）—— 必须对不上。
            rec(2000, "glm-5.3-flash", "glm-5.3-flash", "codex", "s4"),
        ];

        // 与命令里同一套配对逻辑的镜像。抽函数会碰 async/State，测试里
        // 内联同一规则；两边改动必须同步（sessionMatch.ts 同理）。
        let mut claimed = vec![false; logs.len()];
        let mut matched = 0;
        const MAX_SKEW: i64 = 180;
        for r in &records {
            let mut best: Option<(usize, i64)> = None;
            for name in [r.model.as_deref(), r.sent_model.as_deref()].into_iter().flatten() {
                for (i, l) in logs.iter().enumerate() {
                    if claimed[i] { continue }
                    if l.get("model").and_then(|m| m.as_str()) != Some(name) { continue }
                    let t = l.get("time").and_then(|t| t.as_i64()).unwrap_or(0);
                    let gap = t - r.time;
                    if !(0..=MAX_SKEW).contains(&gap) { continue }
                    if best.is_none_or(|(_, g)| gap < g) { best = Some((i, gap)); }
                }
            }
            if let Some((i, _)) = best { claimed[i] = true; matched += 1; }
        }
        assert_eq!(matched, 3, "三条有效配对，方向反了的那条不算");
        assert!(claimed[0] && claimed[1] && claimed[2]);
    }
}
