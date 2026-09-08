//! Codex 的会话救援。病因和两种救法与 Claude Code 那边同一套，见
//! [`crate::services::session_rescue`]；这里只负责「Codex 的会话长什么样」。
//!
//! # 一条会话是一份 rollout
//!
//! ```text
//! ~/.codex/sessions/YYYY/MM/DD/rollout-<时间>-<uuid>.jsonl
//! ```
//!
//! 每行是 `{timestamp, type, payload}`，`type` 有五种，只有一种进模型上下文：
//!
//! * `response_item` —— **对话正文**（OpenAI Responses 的条目：message /
//!   function_call / function_call_output / reasoning）。救援动的就是它。
//! * `event_msg` —— UI 事件流。`token_count` 那种带着用量，是取真实上下文的
//!   唯一来源；其余不进上下文。
//! * `session_meta` / `turn_context` / `world_state` —— 元信息，原样留着。
//!
//! 所以改写时**只碰 `response_item` 行**，其它行一个字节不动 —— 它们是 Codex
//! 复原界面和工作区状态用的，删了会话能续但界面是空的。
//!
//! # 真实上下文来自 token_count
//!
//! `event_msg.payload.type == "token_count"` 里有 `info.last_token_usage`，
//! 结构是 `{input_tokens, cached_input_tokens, output_tokens, …}`。和 Claude Code
//! 一样，真实占用要把 cached 加回来 —— 只看 `input_tokens` 会小一个数量级。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::error::AppError;
use crate::services::session_rescue::{
    backup_and_write, est_text_tokens, is_b64_image, load_jsonl, now_iso, map_nodes, rewrite, CompactPlan,
    SessionCli, SessionInfo, SlimReport,
};

/// `~/.codex/sessions`。
pub(crate) fn sessions_root() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".codex").join("sessions"))
}

/// 正被 codex 进程拿着的会话 id。
///
/// Codex 没有 Claude Code 的 `sessions/<pid>.json`、也没有 Grok 的
/// `active_sessions.json`，只能扫命令行。扫得到的只有显式带 id 的那种
/// （`codex resume <uuid>`）——交互式进程的命令行里没有 id，认不出来。
///
/// 认不出就当没在跑：这一层只是「别去改正开着的会话」的护栏，漏判的后果是
/// 用户改了个活会话、被进程盖回去（可恢复，有备份）；反过来把所有会话都当
/// 活的，整个功能就废了。
pub(crate) fn live_ids() -> HashSet<String> {
    let mut out = HashSet::new();
    let Ok(o) = std::process::Command::new("ps")
        .args(["-eo", "command"])
        .output()
    else {
        return out;
    };
    let text = String::from_utf8_lossy(&o.stdout);
    for line in text.lines() {
        if !line.contains("codex") {
            continue;
        }
        for tok in line.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-')) {
            if tok.len() == 36 && tok.matches('-').count() == 4 {
                out.insert(tok.to_string());
            }
        }
    }
    out
}

/// 扫一份 rollout。为了不把几十 MB 全解析一遍，只解析可能有用的行。
fn scan(path: &Path, live: &HashSet<String>) -> Option<SessionInfo> {
    use std::io::{BufRead, BufReader};
    let meta = std::fs::metadata(path).ok()?;
    let file = std::fs::File::open(path).ok()?;

    let (mut id, mut cwd, mut slug) = (String::new(), String::new(), String::new());
    let (mut entries, mut last, mut peak) = (0usize, 0u64, 0u64);

    for line in BufReader::new(file).lines().map_while(Result::ok) {
        if line.trim().is_empty() {
            continue;
        }
        if line.contains("\"response_item\"") {
            entries += 1;
        }
        let interesting = id.is_empty()
            || slug.is_empty()
            || line.contains("token_count")
            || line.contains("session_meta");
        if !interesting {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(payload) = v.get("payload") else {
            continue;
        };
        match v.get("type").and_then(Value::as_str) {
            Some("session_meta") => {
                if let Some(s) = payload.get("session_id").and_then(Value::as_str) {
                    id = s.to_string();
                }
                if let Some(s) = payload.get("cwd").and_then(Value::as_str) {
                    cwd = s.to_string();
                }
            }
            Some("event_msg") => match payload.get("type").and_then(Value::as_str) {
                // 第一条用户消息当名字 —— Codex 不给会话起短名。
                Some("user_message") if slug.is_empty() => {
                    if let Some(m) = payload.get("message").and_then(Value::as_str) {
                        slug = m.chars().take(60).collect::<String>().replace('\n', " ");
                    }
                }
                Some("token_count") => {
                    if let Some(u) = payload.pointer("/info/last_token_usage") {
                        let n: u64 = ["input_tokens", "cached_input_tokens"]
                            .iter()
                            .filter_map(|k| u.get(*k).and_then(Value::as_u64))
                            .sum();
                        if n > 0 {
                            last = n;
                            peak = peak.max(n);
                        }
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    // session_meta 缺失时从文件名兜底：rollout-<时间>-<uuid>.jsonl。
    if id.is_empty() {
        let stem = path.file_stem()?.to_str()?;
        id = stem.rsplit('-').take(5).collect::<Vec<_>>().join("-");
    }

    Some(SessionInfo {
        cli: SessionCli::Codex,
        live: live.contains(&id),
        id,
        path: path.display().to_string(),
        cwd,
        slug,
        entries,
        bytes: meta.len(),
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

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else if p
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|n| n.starts_with("rollout-") && n.ends_with(".jsonl"))
        {
            out.push(p);
        }
    }
}

/// 扫出本机所有 Codex 会话。
pub(crate) fn list() -> Vec<SessionInfo> {
    let Some(root) = sessions_root() else {
        return Vec::new();
    };
    let live = live_ids();
    let mut files = Vec::new();
    walk(&root, &mut files);
    files.iter().filter_map(|p| scan(p, &live)).collect()
}

/// 这条路径是不是 Codex 的 rollout。
pub(crate) fn owns(path: &Path) -> bool {
    let Some(root) = sessions_root() else {
        return false;
    };
    path.starts_with(&root)
        && path
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|n| n.starts_with("rollout-") && n.ends_with(".jsonl"))
}

/// 从磁盘上直接读这条 rollout 的会话 id。删除时要拿它查活会话表，而那时候
/// 没必要把整份文件读进内存 —— `session_meta` 是第一行。
pub(crate) fn id_of(path: &Path) -> String {
    use std::io::{BufRead, BufReader};
    let Ok(file) = std::fs::File::open(path) else {
        return String::new();
    };
    for line in BufReader::new(file).lines().map_while(Result::ok).take(4) {
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if v.get("type").and_then(Value::as_str) == Some("session_meta") {
            return v
                .pointer("/payload/session_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
        }
    }
    String::new()
}

fn session_id_of(entries: &[Value]) -> String {
    entries
        .iter()
        .find(|v| v.get("type").and_then(Value::as_str) == Some("session_meta"))
        .and_then(|v| v.pointer("/payload/session_id"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn guard_live(entries: &[Value]) -> Result<(), AppError> {
    let id = session_id_of(entries);
    if !id.is_empty() && live_ids().contains(&id) {
        return Err(AppError::Config(
            "这个会话正被一个 Codex 进程使用。先退出那个窗口再来 —— 进程里有内存态，现在改会被它盖回去。".into(),
        ));
    }
    Ok(())
}

fn is_response_item(v: &Value) -> bool {
    v.get("type").and_then(Value::as_str) == Some("response_item")
}

fn weight(entry: &Value) -> u64 {
    est_text_tokens(&entry.to_string())
}

/// Codex 的图片是 Responses 协议的 `input_image`，data URL 挂在 `image_url` 上。
fn is_data_url_image(val: &Value) -> bool {
    val.get("type").and_then(Value::as_str) == Some("input_image")
        && val
            .get("image_url")
            .and_then(Value::as_str)
            .is_some_and(|u| u.starts_with("data:"))
}

fn strip_images(entry: &mut Value) -> u64 {
    let before = weight(entry);
    // 同 Grok：input_image 是 content 数组的元素，rewrite 传不到它。
    map_nodes(entry, &mut |val| {
        is_data_url_image(val)
            .then(|| json!({ "type": "input_text", "text": "[图片已被会话救援移除以腾出上下文]" }))
    });
    rewrite(entry, &mut |parent, key, val| {
        is_b64_image(parent, key, val).then(|| Value::String(String::new()))
    });
    before.saturating_sub(weight(entry))
}

/// 超长文本留首尾。`encrypted_content` 是思维链签名，一个字符都不能动。
fn truncate_texts(entry: &mut Value, limit: usize) -> u64 {
    let before = weight(entry);
    rewrite(entry, &mut |_, key, val| {
        if key == "encrypted_content" || key == "id" || key == "call_id" {
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

fn last_context(entries: &[Value]) -> u64 {
    let mut last = 0;
    for v in entries {
        if v.get("type").and_then(Value::as_str) != Some("event_msg") {
            continue;
        }
        if v.pointer("/payload/type").and_then(Value::as_str) != Some("token_count") {
            continue;
        }
        if let Some(u) = v.pointer("/payload/info/last_token_usage") {
            let n: u64 = ["input_tokens", "cached_input_tokens"]
                .iter()
                .filter_map(|k| u.get(*k).and_then(Value::as_u64))
                .sum();
            if n > 0 {
                last = n;
            }
        }
    }
    last
}

/// 瘦身。只改 `response_item` 行 —— 别的行不进上下文，砍了白砍还丢界面状态。
pub(crate) fn slim(path: &str, target: u64, text_limit: usize) -> Result<SlimReport, AppError> {
    let path = PathBuf::from(path);
    let mut entries = load_jsonl(&path)?;
    guard_live(&entries)?;

    let bytes_before = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let last = last_context(&entries);
    if last == 0 {
        return Err(AppError::Config(
            "这份 rollout 里没有 token_count 记录，拿不到真实上下文 —— 不敢下手。".into(),
        ));
    }

    let est: u64 = entries
        .iter()
        .filter(|e| is_response_item(e))
        .map(weight)
        .sum();
    let scale = if est > 0 { last as f64 / est as f64 } else { 1.0 };
    let need_est = if last > target {
        ((last - target) as f64 / scale) as u64
    } else {
        0
    };

    let mut order: Vec<usize> = (0..entries.len())
        .filter(|&i| is_response_item(&entries[i]))
        .collect();
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

/// 渲染一条 `response_item` 给总结模型看。别的行返回 None。
pub(crate) fn render(entry: &Value) -> Option<String> {
    if !is_response_item(entry) {
        return None;
    }
    let p = entry.get("payload")?;
    let mut buf = String::new();
    match p.get("type").and_then(Value::as_str) {
        Some("message") => {
            let role = p.get("role").and_then(Value::as_str).unwrap_or("?");
            for part in p.get("content").and_then(Value::as_array).into_iter().flatten() {
                match part.get("type").and_then(Value::as_str) {
                    Some("input_text") | Some("output_text") => {
                        if let Some(t) = part.get("text").and_then(Value::as_str) {
                            buf.push_str(t);
                            buf.push('\n');
                        }
                    }
                    Some("input_image") => buf.push_str("[图片]\n"),
                    _ => {}
                }
            }
            let buf = buf.trim();
            if buf.is_empty() {
                return None;
            }
            return Some(format!("{role}: {buf}"));
        }
        Some("function_call") => {
            let name = p.get("name").and_then(Value::as_str).unwrap_or("?");
            buf.push_str(&format!("[调用工具 {name}]"));
        }
        Some("function_call_output") => {
            let t = p
                .get("output")
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
        // reasoning 是密文，进不了摘要。
        _ => return None,
    }
    Some(buf)
}

/// 排出要怎么压。尾巴取连续的一段，且不能以孤儿 `function_call_output` 开头。
pub(crate) fn plan(path: &str, keep_tail: usize) -> Result<CompactPlan, AppError> {
    let path = PathBuf::from(path);
    let entries = load_jsonl(&path)?;
    guard_live(&entries)?;

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
    while cut < entries.len()
        && entries[cut].pointer("/payload/type").and_then(Value::as_str)
            == Some("function_call_output")
    {
        cut += 1;
    }

    let lines: Vec<String> = entries[..cut].iter().filter_map(render).collect();
    Ok(CompactPlan {
        lines,
        cut,
        kept_tail: entries.len() - cut,
        context_before: last_context(&entries),
    })
}

/// 把摘要写回去。
///
/// 只丢切点之前的 `response_item`，其余行（`session_meta` / `turn_context` /
/// `world_state` / `event_msg`）原样保留 —— 它们不进上下文，但 Codex 靠它们
/// 复原界面和工作区。摘要作为一条 `response_item` 的 user message 插在第一条
/// 被保留的对话之前。
pub(crate) fn apply(path: &str, summary: &str, cut: usize) -> Result<String, AppError> {
    let path = PathBuf::from(path);
    let entries = load_jsonl(&path)?;
    guard_live(&entries)?;
    if cut >= entries.len() {
        return Err(AppError::Config("切点越界，已中止".into()));
    }

    let summary_line = json!({
        "timestamp": now_iso(),
        "type": "response_item",
        "payload": {
            "type": "message",
            "role": "user",
            "content": [{
                "type": "input_text",
                "text": format!(
                    "This session is being continued from a previous conversation that ran out of \
                     context. The summary below covers the earlier portion of the conversation.\n\n\
                     Summary:\n{summary}"
                ),
            }],
        },
    });

    let mut out: Vec<Value> = Vec::with_capacity(entries.len());
    let mut inserted = false;
    for (i, e) in entries.iter().enumerate() {
        if i < cut {
            // 切点之前：只丢对话正文，元信息和事件流留着。
            if !is_response_item(e) {
                out.push(e.clone());
            }
            continue;
        }
        if !inserted {
            out.push(summary_line.clone());
            inserted = true;
        }
        out.push(e.clone());
    }
    if !inserted {
        out.push(summary_line);
    }

    let body: String = out
        .iter()
        .map(|e| serde_json::to_string(e).unwrap_or_default() + "\n")
        .collect();
    backup_and_write(&path, &body)
}

/// 删一条会话：rollout 文件 + 救援留下的备份。
pub(crate) fn delete(path: &Path) -> Result<u64, AppError> {
    let mut bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    std::fs::remove_file(path).map_err(|e| AppError::Io(format!("删不掉：{e}")))?;
    if let (Some(dir), Some(name)) = (path.parent(), path.file_name().and_then(|s| s.to_str())) {
        let prefix = format!("{name}.bak-");
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                if e.file_name()
                    .to_str()
                    .is_some_and(|n| n.starts_with(&prefix))
                {
                    bytes += e.metadata().map(|m| m.len()).unwrap_or(0);
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_rollout(lines: &[Value]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ccload-codex-{}",
            crate::services::session_rescue::uuid_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rollout-2026-09-08T00-00-00-abcdefab-1234-5678-9abc-def012345678.jsonl");
        let body: String = lines.iter().map(|v| v.to_string() + "\n").collect();
        std::fs::write(&path, body).unwrap();
        path
    }

    fn base() -> Vec<Value> {
        let mut v = vec![
            json!({"type":"session_meta","payload":{"session_id":"abcdefab-1234-5678-9abc-def012345678","cwd":"/tmp/p"}}),
            json!({"type":"event_msg","payload":{"type":"user_message","message":"第一句"}}),
        ];
        for i in 0..8 {
            v.push(json!({"type":"response_item","payload":{"type":"message","role":"user",
                "content":[{"type":"input_text","text":format!("问题 {i}")}]}}));
            v.push(json!({"type":"response_item","payload":{"type":"function_call","name":"shell","call_id":format!("c{i}")}}));
            v.push(json!({"type":"response_item","payload":{"type":"function_call_output","call_id":format!("c{i}"),"output":"ok"}}));
        }
        v.push(json!({"type":"event_msg","payload":{"type":"token_count",
            "info":{"last_token_usage":{"input_tokens":40_000,"cached_input_tokens":360_000}}}}));
        v
    }

    /// 元信息行必须原样活下来，只有切点前的对话正文被摘要顶替。
    #[test]
    fn compaction_keeps_meta_and_drops_only_conversation() {
        let path = temp_rollout(&base());
        let p = plan(path.to_str().unwrap(), 6).unwrap();
        assert!(!p.lines.is_empty());
        assert_eq!(p.context_before, 400_000, "上下文要把 cached 加回来");

        apply(path.to_str().unwrap(), "摘要正文", p.cut).unwrap();
        let after = load_jsonl(&path).unwrap();
        assert_eq!(after[0]["type"], "session_meta", "元信息丢了");
        assert!(
            after.iter().any(|v| v["payload"]["type"] == "token_count"),
            "事件流被误删"
        );
        let summary = after
            .iter()
            .find(|v| v.pointer("/payload/content/0/text").is_some_and(|t| t
                .as_str()
                .unwrap_or_default()
                .contains("摘要正文")));
        assert!(summary.is_some(), "摘要没写进去");
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    /// 尾巴不能以孤儿工具结果开头 —— 它的 function_call 已经被摘要顶替了。
    #[test]
    fn the_cut_never_leaves_an_orphan_output() {
        let path = temp_rollout(&base());
        let entries = load_jsonl(&path).unwrap();
        let p = plan(path.to_str().unwrap(), 4).unwrap();
        assert_ne!(
            entries[p.cut].pointer("/payload/type").and_then(Value::as_str),
            Some("function_call_output"),
        );
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    /// 签名不能被截断，图片要换成占位符。
    #[test]
    fn signature_survives_truncation() {
        let mut item = json!({"type":"response_item","payload":{"type":"reasoning",
            "encrypted_content":"S".repeat(20_000)}});
        truncate_texts(&mut item, 100);
        assert_eq!(item["payload"]["encrypted_content"].as_str().unwrap().len(), 20_000);

        let mut msg = json!({"type":"response_item","payload":{"type":"message","role":"user",
            "content":[{"type":"input_image","image_url":format!("data:image/png;base64,{}", "A".repeat(20_000))}]}});
        assert!(strip_images(&mut msg) > 0);
        assert!(!msg.to_string().contains(&"A".repeat(200)));
    }
}
