//! Grok Build 的会话救援。病因和两种救法与 Claude Code 那边同一套，见
//! [`crate::services::session_rescue`]；这里只负责「Grok 的会话长什么样」。
//!
//! # 磁盘布局和 Claude Code 完全不同
//!
//! Claude Code 一条会话就是一个 `<uuid>.jsonl`；Grok 的一条会话是**一个目录**：
//!
//! ```text
//! ~/.grok/sessions/<urlencode(cwd)>/<uuid>/
//!     chat_history.jsonl   ← 只有这份进模型上下文，救援动的就是它
//!     summary.json         ← 元信息 + num_chat_messages 计数，改完要同步
//!     updates.jsonl        ← UI 事件流，几十 MB，不进上下文
//!     events.jsonl         ← 同上
//! ```
//!
//! 所以「路径」这个概念在这里指 `chat_history.jsonl`：它是唯一的对话正文，
//! 也是唯一需要备份和改写的东西。删除时才按整个目录算（updates/events 才是
//! 占地方的大头，只删 chat_history 腾不出空间）。
//!
//! # 真实上下文只能算出来，不能直接读
//!
//! Grok 不像 Claude Code 那样每轮回报一次 usage。它在 `updates.jsonl` 里给两种
//! 数：
//!
//! * `auto_compact_started.tokens_used` —— **精确值**，但只在自动压缩触发的那
//!   一刻才有，一条会话可能一次都没有；
//! * `turn_completed.usage` —— 一轮里**累计**的量（`inputTokens` 实测能到 470 万），
//!   除以 `modelCalls` 才是单次请求的规模。
//!
//! 两个都取，谁大用谁：精确值优先，没有就用均值兜底。这个数只用来排序和判断
//! 「哪条快撑爆了」，不参与写入决策，所以均值够用 —— 但界面上不能把它说成
//! 和 Claude Code 那个 usage 一样精确。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::error::AppError;
use crate::services::session_rescue::{
    backup_and_write, est_text_tokens, is_b64_image, load_jsonl, pid_alive, map_nodes, rewrite, CompactPlan,
    SessionCli, SessionInfo, SlimReport,
};

/// `~/.grok/sessions`。
pub(crate) fn sessions_root() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".grok").join("sessions"))
}

/// 一个会话目录里那份对话正文。
fn chat_path(dir: &Path) -> PathBuf {
    dir.join("chat_history.jsonl")
}

/// 正被 grok 进程拿着的会话 id。
///
/// `~/.grok/active_sessions.json` 是一个数组，每项带 `session_id` 和 `pid`。
/// 和 Claude Code 那边同样的规矩：pid 已经不在了的条目是崩溃残留，不能让一条
/// 死掉的会话永远救不了。
pub(crate) fn live_ids() -> HashSet<String> {
    let mut out = HashSet::new();
    let Some(path) = dirs::home_dir().map(|h| h.join(".grok/active_sessions.json")) else {
        return out;
    };
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return out;
    };
    let Ok(list) = serde_json::from_str::<Vec<Value>>(&raw) else {
        return out;
    };
    for item in list {
        let Some(id) = item.get("session_id").and_then(Value::as_str) else {
            continue;
        };
        let Some(pid) = item.get("pid").and_then(Value::as_i64) else {
            continue;
        };
        if pid > 0 && pid_alive(pid) {
            out.insert(id.to_string());
        }
    }
    out
}

/// 目录总大小。删除时报的「腾出多少」按它算 —— updates/events 才是大头。
fn dir_size(dir: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    rd.flatten()
        .map(|e| {
            let p = e.path();
            if p.is_dir() {
                dir_size(&p)
            } else {
                e.metadata().map(|m| m.len()).unwrap_or(0)
            }
        })
        .sum()
}

/// 只读 `updates.jsonl` 末尾这么多字节。
///
/// 实测单个 updates.jsonl 能到 99 MB，而本机有 800 多条 Grok 会话 —— 全量读一遍
/// 让「重新扫描」要跑将近一分钟，进页面就是卡死。用量记录是按时间追加的，末尾
/// 那几 MB 覆盖的正是最近若干轮，也就是「这条会话现在多大」这个问题的答案。
///
/// 代价是 `peak` 变成**近期峰值**而不是全历史峰值。这一页的用途是「找出哪条
/// 快撑爆了」，近期峰值比几个月前的历史高点更贴题，所以这个取舍是划算的。
const UPDATES_TAIL_BYTES: u64 = 4 * 1024 * 1024;

/// 从 `updates.jsonl` 里把上下文规模捞出来。返回 (最近一轮, 近期峰值)。
///
/// 逐行只做字节判断再解析：需要解析的行占比很低，全量 JSON 解析纯属浪费。
fn context_from_updates(dir: &Path) -> (u64, u64) {
    use std::io::{BufRead, BufReader, Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(dir.join("updates.jsonl")) else {
        return (0, 0);
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let seeked = len > UPDATES_TAIL_BYTES;
    if seeked && file.seek(SeekFrom::Start(len - UPDATES_TAIL_BYTES)).is_err() {
        return (0, 0);
    }
    let mut reader = BufReader::new(file);
    if seeked {
        // 跳到中间必然落在某一行里，甚至可能劈开一个多字节字符。先把这半行丢掉。
        // 用 read_until 而不是 lines()：后者遇到非法 UTF-8 会返回 Err，而
        // `map_while(Result::ok)` 见到 Err 就**整个停下**，等于一条都读不到。
        let mut partial = Vec::new();
        let _ = reader.read_until(b'\n', &mut partial);
    }
    let (mut last, mut peak) = (0u64, 0u64);
    let mut raw = Vec::new();
    loop {
        raw.clear();
        match reader.read_until(b'\n', &mut raw) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let line = String::from_utf8_lossy(&raw);
        let has_compact = line.contains("auto_compact_started");
        let has_turn = line.contains("turn_completed");
        if !has_compact && !has_turn {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(u) = v.pointer("/params/update") else {
            continue;
        };
        // 精确值：压缩触发那一刻的真实占用。
        if let Some(n) = u.get("tokens_used").and_then(Value::as_u64) {
            if n > 0 {
                last = n;
                peak = peak.max(n);
            }
            continue;
        }
        // 兜底：一轮的累计量除以这一轮打了几次模型。
        let Some(usage) = u.get("usage") else { continue };
        let calls = usage
            .get("modelCalls")
            .and_then(Value::as_u64)
            .filter(|n| *n > 0)
            .unwrap_or(1);
        let sum: u64 = ["inputTokens", "cachedReadTokens"]
            .iter()
            .filter_map(|k| usage.get(*k).and_then(Value::as_u64))
            .sum();
        let avg = sum / calls;
        if avg > 0 {
            last = avg;
            peak = peak.max(avg);
        }
    }
    (last, peak)
}

/// 扫一个会话目录。读不动就返回 None —— 一个坏目录不该让整页打不开。
fn scan(dir: &Path, live: &HashSet<String>) -> Option<SessionInfo> {
    let chat = chat_path(dir);
    if !chat.is_file() {
        return None;
    }
    let id = dir.file_name()?.to_str()?.to_string();
    let meta = std::fs::metadata(&chat).ok()?;

    let summary: Value = std::fs::read_to_string(dir.join("summary.json"))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| json!({}));
    let cwd = summary
        .pointer("/info/cwd")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let slug = summary
        .get("session_summary")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let entries = summary
        .get("num_chat_messages")
        .and_then(Value::as_u64)
        .map(|n| n as usize)
        .unwrap_or(0);

    let (last, peak) = context_from_updates(dir);

    Some(SessionInfo {
        cli: SessionCli::GrokBuild,
        live: live.contains(&id),
        id,
        path: chat.display().to_string(),
        cwd,
        slug,
        entries,
        bytes: dir_size(dir),
        last_context: last,
        peak_context: peak,
        modified_at: meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
        compactions: 0,
    })
}

/// 扫出本机所有 Grok 会话。
pub(crate) fn list() -> Vec<SessionInfo> {
    let Some(root) = sessions_root() else {
        return Vec::new();
    };
    let live = live_ids();
    let mut out = Vec::new();
    let Ok(projects) = std::fs::read_dir(&root) else {
        return out;
    };
    for proj in projects.flatten() {
        let Ok(dirs) = std::fs::read_dir(proj.path()) else {
            continue;
        };
        for d in dirs.flatten() {
            let p = d.path();
            if p.is_dir() {
                if let Some(info) = scan(&p, &live) {
                    out.push(info);
                }
            }
        }
    }
    out
}

/// 这条路径是不是 Grok 的对话正文。
pub(crate) fn owns(path: &Path) -> bool {
    let Some(root) = sessions_root() else {
        return false;
    };
    path.file_name().and_then(|s| s.to_str()) == Some("chat_history.jsonl")
        && path.starts_with(&root)
}

fn session_dir(path: &Path) -> Result<PathBuf, AppError> {
    path.parent()
        .map(Path::to_path_buf)
        .ok_or_else(|| AppError::Config("会话路径不合法".into()))
}

fn guard_live(dir: &Path) -> Result<(), AppError> {
    let id = dir
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string();
    if live_ids().contains(&id) {
        return Err(AppError::Config(
            "这个会话正被一个 Grok 进程使用。先退出那个窗口再来 —— 进程里有内存态，现在改会被它盖回去。".into(),
        ));
    }
    Ok(())
}

/// Grok 的图片长这样：`{"type":"image","url":"data:image/jpeg;base64,…"}`。
/// 和 Claude Code 的 `{"type":"base64","data":…}` 不是一个形状，得单独认。
fn is_data_url_image(val: &Value) -> bool {
    val.get("type").and_then(Value::as_str) == Some("image")
        && val
            .get("url")
            .and_then(Value::as_str)
            .is_some_and(|u| u.starts_with("data:"))
}

/// 一条记录的估算权重。`encrypted_content` 也算进去 —— 它是实打实占上下文的
/// 密文，只是不能被截断。
fn weight(entry: &Value) -> u64 {
    est_text_tokens(&entry.to_string())
}

/// 把图片换成占位符，返回省下的估算 token。
fn strip_images(entry: &mut Value) -> u64 {
    let before = weight(entry);
    // 图片是 content 数组的一个元素，只能用 map_nodes 整个换掉 —— rewrite 看不见它。
    map_nodes(entry, &mut |val| {
        is_data_url_image(val).then(|| {
            json!({ "type": "text", "text": "[图片已被会话救援移除以腾出上下文]" })
        })
    });
    // 顺带认一下通用的 base64 块，别的工具塞进来的图也砍掉。
    rewrite(entry, &mut |parent, key, val| {
        is_b64_image(parent, key, val).then(|| Value::String(String::new()))
    });
    before.saturating_sub(weight(entry))
}

/// 超长文本留首尾。
///
/// **`encrypted_content` 一个字符都不能动**：它是上游对思维链的签名，改了下一次
/// 请求整体 400。`summary` 里的摘要文本同理留着 —— 它很短，而且是换模型之后
/// 唯一还能读的思维线索。
fn truncate_texts(entry: &mut Value, limit: usize) -> u64 {
    let before = weight(entry);
    rewrite(entry, &mut |_, key, val| {
        if key == "encrypted_content" || key == "id" || key == "tool_call_id" {
            return None;
        }
        let s = val.as_str()?;
        if s.chars().count() <= limit {
            return None;
        }
        let chars: Vec<char> = s.chars().collect();
        let (head, tail) = (limit / 2, limit - limit / 2);
        let cut = chars.len() - limit;
        let new: String = chars[..head].iter().collect::<String>()
            + &format!("\n\n… [会话救援截掉中间 {cut} 字符以腾出上下文] …\n\n")
            + &chars[chars.len() - tail..].iter().collect::<String>();
        Some(Value::String(new))
    });
    before.saturating_sub(weight(entry))
}

/// 改完 chat_history 之后把 `summary.json` 的条数对上。
///
/// 不同步的话 Grok 读回来会发现「记的是 1814 条、实际只有 40 条」，界面上的
/// 计数和滚动位置全是错的。
fn sync_summary(dir: &Path, chat_len: usize) -> Result<(), AppError> {
    let path = dir.join("summary.json");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Ok(()); // 没有就不管，chat_history 才是正文
    };
    let Ok(mut v) = serde_json::from_str::<Value>(&raw) else {
        return Ok(());
    };
    let Some(obj) = v.as_object_mut() else {
        return Ok(());
    };
    obj.insert("num_chat_messages".into(), json!(chat_len));
    let body = serde_json::to_string(&v).map_err(|e| AppError::Config(e.to_string()))?;
    backup_and_write(&path, &body)?;
    Ok(())
}

/// 瘦身。语义和 Claude Code 那边一致：砍图 + 截长文本，直到估算降到目标以下。
pub(crate) fn slim(path: &str, target: u64, text_limit: usize) -> Result<SlimReport, AppError> {
    let path = PathBuf::from(path);
    let dir = session_dir(&path)?;
    guard_live(&dir)?;

    let mut entries = load_jsonl(&path)?;
    let bytes_before = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let (last, _) = context_from_updates(&dir);
    if last == 0 {
        return Err(AppError::Config(
            "这条 Grok 会话还没有可用的用量记录，拿不到上下文规模 —— 不敢下手。".into(),
        ));
    }

    let est: u64 = entries.iter().map(weight).sum();
    let scale = if est > 0 { last as f64 / est as f64 } else { 1.0 };
    let need_est = if last > target {
        ((last - target) as f64 / scale) as u64
    } else {
        0
    };

    let mut order: Vec<usize> = (0..entries.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(weight(&entries[i])));

    let (mut cut, mut n_img, mut n_txt) = (0u64, 0usize, 0usize);
    for i in order {
        if cut >= need_est {
            break;
        }
        let s = strip_images(&mut entries[i]);
        if s > 0 {
            n_img += 1;
            cut += s;
        }
        if cut >= need_est {
            break;
        }
        let s = truncate_texts(&mut entries[i], text_limit);
        if s > 0 {
            n_txt += 1;
            cut += s;
        }
    }

    let body: String = entries
        .iter()
        .map(|e| serde_json::to_string(e).unwrap_or_default() + "\n")
        .collect();
    let backup = backup_and_write(&path, &body)?;

    Ok(SlimReport {
        images_stripped: n_img,
        texts_truncated: n_txt,
        bytes_before,
        bytes_after: std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0),
        context_before: last,
        context_after: last.saturating_sub((cut as f64 * scale) as u64),
        backup,
    })
}

/// 把一条记录渲染成给总结模型看的一行。认不出的返回 None。
pub(crate) fn render(entry: &Value) -> Option<String> {
    let kind = entry.get("type").and_then(Value::as_str)?;
    let mut buf = String::new();
    match kind {
        "user" | "assistant" => {
            match entry.get("content") {
                Some(Value::String(s)) => buf.push_str(s),
                Some(Value::Array(parts)) => {
                    for p in parts {
                        match p.get("type").and_then(Value::as_str) {
                            Some("text") => {
                                if let Some(t) = p.get("text").and_then(Value::as_str) {
                                    buf.push_str(t);
                                    buf.push('\n');
                                }
                            }
                            Some("image") => buf.push_str("[图片]\n"),
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            for tc in entry
                .get("tool_calls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let name = tc.get("name").and_then(Value::as_str).unwrap_or("?");
                buf.push_str(&format!("[调用工具 {name}]\n"));
            }
        }
        "tool_result" => {
            // 只留个头：总结要知道「跑过什么、结论是什么」，不需要几百行原始输出。
            let t = entry
                .get("content")
                .map(|c| match c {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .unwrap_or_default();
            buf.push_str(&format!(
                "[工具结果 {}]",
                t.chars().take(300).collect::<String>()
            ));
        }
        // system 是每次都会重新注入的提示词，reasoning 是密文，都不进摘要。
        _ => return None,
    }
    let buf = buf.trim();
    if buf.is_empty() {
        return None;
    }
    Some(format!("{kind}: {buf}"))
}

/// 排出这条会话要怎么压：哪些进摘要、保留哪一段尾巴。
///
/// 尾巴取的是**连续的一段**而不是「最后 N 条能渲染的」：`tool_result` 必须和
/// 它对应的 `tool_calls` 待在一起，隔着挑会留下一个孤儿工具结果，Grok 读回去
/// 直接报错。所以先按能渲染的条数定一个切点，再把切点往后推到不是孤儿为止。
pub(crate) fn plan(path: &str, keep_tail: usize) -> Result<CompactPlan, AppError> {
    let path = PathBuf::from(path);
    let dir = session_dir(&path)?;
    guard_live(&dir)?;

    let entries = load_jsonl(&path)?;
    let (last, _) = context_from_updates(&dir);

    let renderable: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| render(e).is_some())
        .map(|(i, _)| i)
        .collect();
    if renderable.len() <= keep_tail {
        return Err(AppError::Config(
            "这条会话里没有够得上总结的内容 —— 它本来就不长。".into(),
        ));
    }
    let mut cut = renderable[renderable.len() - keep_tail];
    // 孤儿 tool_result：它的 tool_calls 在切点前面，留着会对不上。
    while cut < entries.len()
        && entries[cut].get("type").and_then(Value::as_str) == Some("tool_result")
    {
        cut += 1;
    }

    let lines: Vec<String> = entries[..cut].iter().filter_map(render).collect();
    Ok(CompactPlan {
        lines,
        cut,
        kept_tail: entries.len() - cut,
        context_before: last,
    })
}

/// 把摘要写回去。
///
/// Claude Code 那边是**追加**两条、旧内容一个字节不动（靠 `parentUuid: null`
/// 剪链）。Grok 没有这种链结构 —— 它把 `chat_history.jsonl` 整份当上下文，所以
/// 只能**重写**：系统提示 + 一条装着摘要的 user + 原样的尾巴。
///
/// 正因为是重写，备份是唯一的后悔药，所以先备份再落盘，且 `summary.json` 的
/// 计数要跟着改。
pub(crate) fn apply(path: &str, summary: &str, cut: usize) -> Result<String, AppError> {
    let path = PathBuf::from(path);
    let dir = session_dir(&path)?;
    guard_live(&dir)?;

    let entries = load_jsonl(&path)?;
    if cut >= entries.len() {
        return Err(AppError::Config("切点越界，已中止".into()));
    }

    let mut out: Vec<Value> = Vec::with_capacity(entries.len() - cut + 2);
    // 系统提示留着：它是这条会话的人格和规则，摘要替代不了。
    if let Some(first) = entries.first() {
        if first.get("type").and_then(Value::as_str) == Some("system") {
            out.push(first.clone());
        }
    }
    out.push(json!({
        "type": "user",
        "content": [{
            "type": "text",
            "text": format!(
                "This session is being continued from a previous conversation that ran out of \
                 context. The summary below covers the earlier portion of the conversation.\n\n\
                 Summary:\n{summary}"
            ),
        }],
    }));
    out.extend(entries[cut..].iter().cloned());

    let body: String = out
        .iter()
        .map(|e| serde_json::to_string(e).unwrap_or_default() + "\n")
        .collect();
    let backup = backup_and_write(&path, &body)?;
    sync_summary(&dir, out.len())?;
    Ok(backup)
}

/// 删一条会话。整个目录一起删 —— updates/events 才是占地方的大头。
pub(crate) fn delete(path: &Path) -> Result<u64, AppError> {
    let dir = session_dir(path)?;
    let bytes = dir_size(&dir);
    std::fs::remove_dir_all(&dir).map_err(|e| AppError::Io(format!("删不掉：{e}")))?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).unwrap();
    }

    fn temp_session() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ccload-grok-{}",
            crate::services::session_rescue::uuid_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 图片是 data URL 形状，得认出来换成占位符；`encrypted_content` 一个字符
    /// 都不能动 —— 改了它下一次请求整体 400。
    #[test]
    fn images_go_but_the_signature_survives() {
        let mut entry = json!({
            "type": "user",
            "content": [
                { "type": "text", "text": "x".repeat(9_000) },
                { "type": "image", "url": format!("data:image/jpeg;base64,{}", "A".repeat(20_000)) }
            ],
        });
        assert!(strip_images(&mut entry) > 0, "图没砍掉");
        let dumped = entry.to_string();
        assert!(!dumped.contains(&"A".repeat(200)), "base64 还在");
        assert!(dumped.contains("图片已被会话救援移除"));

        let mut reasoning = json!({
            "type": "reasoning",
            "encrypted_content": "S".repeat(20_000),
            "summary": [{ "type": "summary_text", "text": "计划" }],
        });
        truncate_texts(&mut reasoning, 100);
        assert_eq!(
            reasoning["encrypted_content"].as_str().unwrap().len(),
            20_000,
            "签名被截断了 —— 这会让整条会话永久 400"
        );
    }

    /// 切点必须落在完整的一轮上：尾巴不能以孤儿 tool_result 开头。
    #[test]
    fn the_cut_never_leaves_an_orphan_tool_result() {
        let dir = temp_session();
        let mut lines = vec![json!({"type":"system","content":"sys"})];
        for i in 0..8 {
            lines.push(json!({
                "type": "assistant",
                "content": format!("step {i}"),
                "tool_calls": [{ "id": format!("t{i}"), "name": "run" }],
            }));
            lines.push(json!({
                "type": "tool_result",
                "tool_call_id": format!("t{i}"),
                "content": "ok",
            }));
        }
        let body: String = lines
            .iter()
            .map(|v| v.to_string() + "\n")
            .collect();
        write(&dir, "chat_history.jsonl", &body);
        write(
            &dir,
            "summary.json",
            &json!({ "info": { "cwd": "/tmp" }, "num_chat_messages": lines.len() }).to_string(),
        );
        // 造一条 usage，否则 plan 认为没有上下文可算。
        write(
            &dir,
            "updates.jsonl",
            &json!({"params":{"update":{"sessionUpdate":"auto_compact_started","tokens_used":400_000}}})
                .to_string(),
        );

        let chat = chat_path(&dir);
        let p = plan(chat.to_str().unwrap(), 4).unwrap();
        let entries = load_jsonl(&chat).unwrap();
        assert_ne!(
            entries[p.cut].get("type").and_then(Value::as_str),
            Some("tool_result"),
            "尾巴以孤儿工具结果开头，Grok 读回去会报错"
        );
        assert!(!p.lines.is_empty(), "没有内容进摘要");

        // 写回：系统提示 + 摘要 + 尾巴，条数对得上，summary.json 跟着改。
        let backup = apply(chat.to_str().unwrap(), "摘要正文", p.cut).unwrap();
        assert!(std::fs::metadata(&backup).is_ok(), "备份没落地");
        let after = load_jsonl(&chat).unwrap();
        assert_eq!(after[0]["type"], "system", "系统提示丢了");
        assert!(
            after[1]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("摘要正文"),
            "摘要没写进去"
        );
        assert_eq!(after.len(), 2 + (entries.len() - p.cut));
        let summary: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("summary.json")).unwrap())
                .unwrap();
        assert_eq!(summary["num_chat_messages"], json!(after.len()));

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 上下文取数：精确值优先，没有就用「累计 / 调用次数」的均值。
    #[test]
    fn context_prefers_the_exact_number() {
        let dir = temp_session();
        write(
            &dir,
            "updates.jsonl",
            &format!(
                "{}\n{}\n",
                json!({"params":{"update":{"sessionUpdate":"turn_completed",
                    "usage":{"inputTokens":900,"cachedReadTokens":100,"modelCalls":10}}}}),
                json!({"params":{"update":{"sessionUpdate":"auto_compact_started","tokens_used":404_839}}}),
            ),
        );
        let (last, peak) = context_from_updates(&dir);
        assert_eq!(last, 404_839, "精确值没被采用");
        assert_eq!(peak, 404_839);
        std::fs::remove_dir_all(&dir).ok();
    }
}
