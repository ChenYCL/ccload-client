//! 本客户端自带 MCP 工具的调用流水与聚合。
//!
//! 为什么要自己记：MCP 服务器是**独立进程**，由 CLI 在需要时拉起，跑完就退。
//! 没有任何一方天然知道「describe_image 今天被调了几次、每次多久」——调用方
//! （Claude Code / Codex …）不外传这个，内核那边只看得见一次普通的
//! `/v1/messages` 请求，分不清是用户在对话还是 MCP 在看图。
//!
//! 于是由工具进程自己在每次调用结束后追加一行 JSONL，桌面端再聚合它。
//!
//! # 并发
//!
//! 五个 CLI 可能同时各拉起一个 MCP 进程。文件用 `O_APPEND` 打开、一次
//! `write_all` 写完整行：同一个 fd 的追加写在 offset 上是原子的，短行不会
//! 交叉。所以这里**不加锁**，也不需要 —— 加锁反而要处理进程崩溃留下的死锁。
//!
//! # 有界
//!
//! 流水会一直长，所以写之前先看大小，超过 `MAX_BYTES` 就把最老的一半丢掉。
//! 统计口径因此是「最近若干次调用」，不是「有史以来」，UI 上要说清楚。

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 单次工具调用的流水。字段短是有意的：这个文件按行追加，且会被截断。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpCall {
    /// 工具名，如 `describe_image`。
    pub tool: String,
    /// unix 秒，调用**结束**的时刻。
    pub at: i64,
    /// 耗时毫秒。
    pub ms: u64,
    pub ok: bool,
    /// 失败原因（截断到一行），成功时为 None。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub err: Option<String>,
}

/// 超过这个大小就丢掉最老的一半。约等于两万次调用的流水。
const MAX_BYTES: u64 = 2 * 1024 * 1024;

/// 流水文件。和 settings.json 同目录 —— MCP 进程读不到桌面端的状态，
/// 只能靠这个约定俗成的位置。
pub fn log_path() -> Option<PathBuf> {
    Some(dirs::home_dir()?.join(".ccload-client").join("mcp-usage.jsonl"))
}

/// 追加一条流水。**任何失败都静默吞掉**：统计是附带品，不能让它把一次
/// 真正成功的看图变成工具调用失败。
pub fn record(call: &McpCall) {
    let Some(path) = log_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    trim_if_large(&path);
    let Ok(mut line) = serde_json::to_string(call) else {
        return;
    };
    line.push('\n');
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = f.write_all(line.as_bytes());
    }
}

/// 超限时保留后一半。整行读写：截断点必须落在换行上，否则下一次解析会
/// 撞上半条 JSON。
fn trim_if_large(path: &Path) {
    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    if meta.len() <= MAX_BYTES {
        return;
    }
    let Ok(raw) = std::fs::read_to_string(path) else {
        return;
    };
    let lines: Vec<&str> = raw.lines().collect();
    let keep = &lines[lines.len() / 2..];
    let body = keep.join("\n");
    // 直接覆写，不走 write_atomic：这是纯统计数据，丢了不影响任何配置，
    // 而 rename 会让正在 append 的其它进程写进一个已被换掉的 inode。
    let _ = std::fs::write(path, format!("{body}\n"));
}

/// 一个工具的聚合结果。
#[derive(Debug, Clone, Serialize)]
pub struct ToolStat {
    pub tool: String,
    pub calls: u64,
    pub failed: u64,
    /// 毫秒。只统计成功的调用 —— 失败往往是「路径不存在」这种 1ms 就返回的，
    /// 混进去会把平均耗时压得看不出真实开销。
    pub avg_ms: u64,
    pub max_ms: u64,
    /// 累计耗时毫秒（含失败），用来算「这些工具一共花了多久」。
    pub total_ms: u64,
    /// unix 秒，最后一次调用。
    pub last_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct McpUsage {
    pub tools: Vec<ToolStat>,
    pub calls: u64,
    pub failed: u64,
    pub total_ms: u64,
    /// 流水里最早一条的时刻，UI 用它说明「统计自何时起」。0 表示没有数据。
    pub since: i64,
    /// 流水被截断过（老记录已经丢了），统计口径不是「有史以来」。
    pub truncated: bool,
}

/// 读回并聚合。解析不了的行直接跳过：这个文件可能在进程被 kill 时留下半行。
pub fn aggregate() -> McpUsage {
    let raw = log_path()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();

    let mut by_tool: BTreeMap<String, Acc> = BTreeMap::new();
    let mut calls = 0u64;
    let mut failed = 0u64;
    let mut total_ms = 0u64;
    let mut since = 0i64;

    for line in raw.lines() {
        let Ok(c) = serde_json::from_str::<McpCall>(line) else {
            continue;
        };
        calls += 1;
        total_ms += c.ms;
        if !c.ok {
            failed += 1;
        }
        if since == 0 || (c.at > 0 && c.at < since) {
            since = c.at;
        }
        let acc = by_tool.entry(c.tool.clone()).or_default();
        acc.calls += 1;
        acc.total_ms += c.ms;
        if c.ok {
            acc.ok_calls += 1;
            acc.ok_ms += c.ms;
            acc.max_ms = acc.max_ms.max(c.ms);
        } else {
            acc.failed += 1;
        }
        acc.last_at = acc.last_at.max(c.at);
    }

    let mut tools: Vec<ToolStat> = by_tool
        .into_iter()
        .map(|(tool, a)| ToolStat {
            tool,
            calls: a.calls,
            failed: a.failed,
            avg_ms: a.ok_ms.checked_div(a.ok_calls).unwrap_or(0),
            max_ms: a.max_ms,
            total_ms: a.total_ms,
            last_at: a.last_at,
        })
        .collect();
    // 调用次数降序：总览上一眼要看到「谁被用得最多」。
    tools.sort_by(|a, b| b.calls.cmp(&a.calls).then_with(|| a.tool.cmp(&b.tool)));

    let truncated = log_path()
        .and_then(|p| std::fs::metadata(p).ok())
        .is_some_and(|m| m.len() >= MAX_BYTES / 2);

    McpUsage {
        tools,
        calls,
        failed,
        total_ms,
        since,
        truncated,
    }
}

#[derive(Default)]
struct Acc {
    calls: u64,
    failed: u64,
    ok_calls: u64,
    ok_ms: u64,
    total_ms: u64,
    max_ms: u64,
    last_at: i64,
}

/// 清空流水。用户主动点「重置统计」时调用。
pub fn clear() -> Result<(), std::io::Error> {
    let Some(path) = log_path() else {
        return Ok(());
    };
    if !path.exists() {
        return Ok(());
    }
    std::fs::write(path, "")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 半条 JSON（进程被 kill 留下的）不能让整份统计报废。
    #[test]
    fn broken_lines_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("u.jsonl");
        std::fs::write(
            &path,
            "{\"tool\":\"describe_image\",\"at\":10,\"ms\":100,\"ok\":true}\n\
             {\"tool\":\"describe_i\n\
             {\"tool\":\"describe_image\",\"at\":20,\"ms\":300,\"ok\":true}\n",
        )
        .unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        let parsed = raw
            .lines()
            .filter_map(|l| serde_json::from_str::<McpCall>(l).ok())
            .count();
        assert_eq!(parsed, 2);
    }

    /// 平均耗时只看成功的调用：失败往往 1ms 就返回，混进去会把均值压垮。
    #[test]
    fn avg_ignores_failures() {
        let mut acc = Acc::default();
        for (ms, ok) in [(1000u64, true), (1400, true), (1, false)] {
            acc.calls += 1;
            acc.total_ms += ms;
            if ok {
                acc.ok_calls += 1;
                acc.ok_ms += ms;
                acc.max_ms = acc.max_ms.max(ms);
            } else {
                acc.failed += 1;
            }
        }
        assert_eq!(acc.ok_ms / acc.ok_calls, 1200);
        assert_eq!(acc.total_ms, 2401, "总耗时仍然含失败");
    }

    /// 截断必须落在换行上，否则下次解析撞上半条 JSON。
    #[test]
    fn trim_keeps_whole_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("u.jsonl");
        let line = "{\"tool\":\"describe_image\",\"at\":1,\"ms\":5,\"ok\":true}";
        let big: String = std::iter::repeat_n(line, 60_000)
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&path, format!("{big}\n")).unwrap();
        assert!(std::fs::metadata(&path).unwrap().len() > MAX_BYTES);

        trim_if_large(&path);

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.lines().all(|l| serde_json::from_str::<McpCall>(l).is_ok()));
        assert!(std::fs::metadata(&path).unwrap().len() < MAX_BYTES);
    }
}
