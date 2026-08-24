//! 会话救援 —— 把撑爆上游窗口、连 `/compact` 都发不出去的会话弄回来。
//!
//! # 病是怎么得的
//!
//! Claude Code 按**模型声明的窗口**决定什么时候自动压缩，而走 ccLoad 时真正
//! 拦你的是**中转那一家的上限**。两个数不一样的时候（典型：模型名挂了 `[1m]`，
//! 中转其实只给 500k），压缩阈值就被算在一个不存在的分母上，等它触发时已经
//! 越过真实天花板了。
//!
//! 越过之后是死锁：`/compact` 自己也是一次请求，也要把整段 transcript 发上去，
//! 所以它同样超限。会话再也发不出任何东西：
//!
//! ```text
//! 400 {"code":"invalid-argument","error":"This model's maximum prompt
//!      length is 500000 but the request contains 517306 tokens."}
//! ```
//!
//! # 两种救法
//!
//! * [`slim`] —— **瘦身**。纯本地，不调模型：图片换成占位符、超长工具结果留
//!   首尾。秒级、不花 token，但信息是丢掉的。
//! * [`compact`] —— **分块总结**。`/compact` 死锁是因为它一次把全部发上去；
//!   分块就不会，每块单独总结再合并，任何一次请求都不碰天花板。
//!
//! # 为什么两种都不删行
//!
//! transcript 是靠 `uuid`/`parentUuid` 串起来的链表，删行会把链断开，恢复出来
//! 的会话缺胳膊少腿。所以：
//!
//! * 瘦身**就地改写 payload**，条目和它的 uuid 原样留着；
//! * 分块总结**只追加两条**（见 [`compact`]），旧内容一个字节不动。
//!
//! 两者都先把原文件另存 `.bak-<时间戳>`。

use std::collections::HashSet;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use rand::Rng;
use serde::Serialize;
use serde_json::{json, Value};

use crate::error::AppError;

/// 一张图的 token 数 ≈ 像素数 / 750，Anthropic 文档口径。
const PIXELS_PER_TOKEN: u64 = 750;
/// 拿不到尺寸时的兜底。base64 长度和像素数不是线性关系（PNG 压缩率差很多），
/// 与其算错不如给个保守常数 —— 它只参与「先砍谁」的排序。
const FALLBACK_IMAGE_TOKENS: u64 = 1500;
/// 文本按字符估 token。中英文混排 + 代码，3.5 是实测比较接近的系数。
const CHARS_PER_TOKEN: f64 = 3.5;
/// 短于这个长度的字符串不可能是 base64 图片，先挡掉省得逐个查兄弟字段。
const MIN_B64_LEN: usize = 4096;

/// 一条会话的概况。
#[derive(Debug, Clone, Serialize)]
pub struct SessionInfo {
    /// 会话 uuid，也就是文件名去掉 `.jsonl`。`--resume` 认它。
    pub id: String,
    pub path: String,
    /// 会话的工作目录，从记录里读，比从目录名反解可靠。
    pub cwd: String,
    /// Claude Code 给会话起的短名，界面上比 uuid 好认。
    pub slug: String,
    pub entries: usize,
    pub bytes: u64,
    /// **真实**上下文大小，来自上游回报的 usage —— 不是估的。
    pub last_context: u64,
    pub peak_context: u64,
    /// unix 秒。
    pub modified_at: i64,
    /// 有 claude 进程正拿着它。改活着的会话是白改：进程里有内存态，
    /// 下一次落盘会把修改盖掉。
    pub live: bool,
    /// 已经成功压缩过几次。>0 说明最后一个边界之前的内容本来就不进上下文。
    pub compactions: usize,
}

/// 瘦身结果。
#[derive(Debug, Clone, Serialize)]
pub struct SlimReport {
    pub images_stripped: usize,
    pub texts_truncated: usize,
    pub bytes_before: u64,
    pub bytes_after: u64,
    /// 按换算系数折算回真实口径的预计上下文。
    pub context_before: u64,
    pub context_after: u64,
    pub backup: String,
}

/// 分块总结结果。
#[derive(Debug, Clone, Serialize)]
pub struct CompactReport {
    pub chunks: usize,
    pub kept_tail: usize,
    pub context_before: u64,
    /// 摘要本身的估算大小 —— 压缩完大概会落在这个量级。
    pub summary_tokens: u64,
    pub backup: String,
    /// 真正打成功的那个模型。自动挑选时和用户点的可能不是同一个。
    #[serde(default)]
    pub model: String,
}

/// 批量删除结果。一条失败不拖累其余 —— 清 20 个旧会话时，不该因为其中一个
/// 被占用就整批停住。
#[derive(Debug, Clone, Serialize)]
pub struct DeleteReport {
    pub deleted: usize,
    pub bytes: u64,
    pub skipped_live: Vec<String>,
    pub errors: Vec<String>,
}

/// `~/.claude/projects`。造新会话和扫旧会话走同一个根，slug 对不上 `--resume` 就找不着。
pub(crate) fn sessions_root() -> Result<PathBuf, AppError> {
    let home = dirs::home_dir().ok_or_else(|| AppError::Config("找不到用户主目录".into()))?;
    Ok(home.join(".claude").join("projects"))
}

fn est_text_tokens(s: &str) -> u64 {
    (s.chars().count() as f64 / CHARS_PER_TOKEN) as u64
}

/// 确认这是一段 base64 图片，而不是碰巧叫 `data` 的长字符串。
///
/// 判据是同级字段里有 `image/` 或 `type: "base64"` —— 只看键名会把别的长
/// 字符串也当成图砍掉。
fn is_b64_image(parent: &serde_json::Map<String, Value>, key: &str, val: &Value) -> bool {
    let Some(s) = val.as_str() else { return false };
    if s.len() < MIN_B64_LEN || (key != "data" && key != "base64") {
        return false;
    }
    parent.get("type").and_then(Value::as_str) == Some("base64")
        || parent
            .iter()
            .any(|(k, v)| k != key && v.as_str().is_some_and(|t| t.contains("image/")))
}

/// 这一条里的图片值多少 token。有尺寸就按尺寸算，没有就用兜底常数。
fn image_tokens(entry: &Value) -> u64 {
    let mut has_image = false;
    let mut dims = None;
    visit(entry, &mut |parent, key, val| {
        if is_b64_image(parent, key, val) {
            has_image = true;
        }
        if key == "dimensions" {
            if let Some(d) = val.as_object() {
                let w = d.get("originalWidth").or_else(|| d.get("displayWidth"));
                let h = d.get("originalHeight").or_else(|| d.get("displayHeight"));
                if let (Some(w), Some(h)) = (w.and_then(Value::as_u64), h.and_then(Value::as_u64)) {
                    dims = Some(w * h / PIXELS_PER_TOKEN);
                }
            }
        }
    });
    if !has_image {
        return 0;
    }
    // `message` 里和 `toolUseResult` 里各存一份同一张图，只有前者进上下文，
    // 所以按「一条一张」算，不按出现次数算。
    dims.unwrap_or(FALLBACK_IMAGE_TOKENS)
}

/// 只读遍历：对每个 (所在对象, 键, 值) 调一次 `f`。
fn visit(node: &Value, f: &mut impl FnMut(&serde_json::Map<String, Value>, &str, &Value)) {
    match node {
        Value::Object(map) => {
            for (k, v) in map {
                f(map, k, v);
                visit(v, f);
            }
        }
        Value::Array(items) => items.iter().for_each(|v| visit(v, f)),
        _ => {}
    }
}

/// 这一条进上下文时大约值多少 token —— **只用来排序**，不用来报数。
///
/// 只看 `message`：那才是发给模型的东西。`toolUseResult` 是 Claude Code 留的
/// 本地副本，`cwd`/`sessionId`/`version`/`gitBranch` 每行都有一份，
/// `file-history-snapshot` 之类根本不进上下文 —— 全算进来实测高出三倍多，
/// 照着它砍会砍过头。
fn context_weight(entry: &Value) -> u64 {
    let kind = entry.get("type").and_then(Value::as_str).unwrap_or("");
    if kind != "user" && kind != "assistant" {
        return 0;
    }
    let Some(msg) = entry.get("message") else {
        return 0;
    };
    let mut total = image_tokens(entry);
    visit(msg, &mut |parent, key, val| {
        if let Some(s) = val.as_str() {
            if !is_b64_image(parent, key, val) {
                total += est_text_tokens(s);
            }
        }
    });
    total
}

/// 从上游回报的 usage 里取真实上下文大小。返回 (最后一轮, 峰值)。
///
/// 这是和 400 报错里那个数字对齐的唯一口径：`input_tokens` 只算没命中缓存的
/// 部分，光看它会小一个数量级，必须把两个 cache 字段加回来。
fn real_context(entries: &[Value]) -> (u64, u64) {
    let (mut last, mut peak) = (0, 0);
    for e in entries {
        if e.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let Some(u) = e.pointer("/message/usage").and_then(Value::as_object) else {
            continue;
        };
        let n: u64 = [
            "input_tokens",
            "cache_read_input_tokens",
            "cache_creation_input_tokens",
        ]
        .iter()
        .filter_map(|k| u.get(*k).and_then(Value::as_u64))
        .sum();
        if n > 0 {
            last = n;
            peak = peak.max(n);
        }
    }
    (last, peak)
}

fn is_boundary(e: &Value) -> bool {
    e.get("subtype").and_then(Value::as_str) == Some("compact_boundary")
}

/// 读一份 transcript。坏行直接报错而不是跳过 —— 一份解析不了的 transcript
/// 说明文件已经损坏，继续操作只会把损坏写回去。
fn load(path: &Path) -> Result<Vec<Value>, AppError> {
    let file = std::fs::File::open(path)
        .map_err(|e| AppError::Io(format!("打不开 {}：{e}", path.display())))?;
    let mut out = Vec::new();
    for (i, line) in BufReader::new(file).lines().enumerate() {
        let line = line.map_err(|e| AppError::Io(format!("读第 {} 行失败：{e}", i + 1)))?;
        if line.trim().is_empty() {
            continue;
        }
        out.push(serde_json::from_str(&line).map_err(|e| {
            AppError::Config(format!(
                "第 {} 行不是合法 JSON（{e}）—— 文件可能已损坏，先恢复备份",
                i + 1
            ))
        })?);
    }
    Ok(out)
}

/// 备份 + 原子写回。
///
/// 先复制再写临时文件最后 rename：直接就地改写的话，写到一半断电就把用户
/// 几十小时的会话变成半个文件，而这东西没有别的副本。
fn save(path: &Path, entries: &[Value]) -> Result<String, AppError> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let backup = path.with_extension(format!("jsonl.bak-{stamp}"));
    std::fs::copy(path, &backup)
        .map_err(|e| AppError::Io(format!("备份失败，已中止：{e}")))?;

    let tmp = path.with_extension("jsonl.tmp");
    let mut body = String::new();
    for e in entries {
        body.push_str(&serde_json::to_string(e).map_err(|e| AppError::Config(e.to_string()))?);
        body.push('\n');
    }
    std::fs::write(&tmp, body).map_err(|e| AppError::Io(format!("写临时文件失败：{e}")))?;
    // 保留原权限：transcript 是 0600，写成默认的 0644 等于把对话内容对同机
    // 其他用户开放。
    if let Ok(meta) = std::fs::metadata(path) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    std::fs::rename(&tmp, path).map_err(|e| AppError::Io(format!("落盘失败：{e}")))?;
    Ok(backup.display().to_string())
}

/// 哪些会话正被 claude 进程拿着。一次 `ps` 查完，别对每个文件各查一次。
fn live_session_ids() -> HashSet<String> {
    let mut out = HashSet::new();
    let Ok(o) = std::process::Command::new("ps")
        .args(["-eo", "command"])
        .output()
    else {
        return out;
    };
    let text = String::from_utf8_lossy(&o.stdout);
    for line in text.lines() {
        if !line.contains("share/claude/versions") && !line.contains("ClaudeCode.app") {
            continue;
        }
        // 命令行里 uuid 可能以 `--session-id <id>` 或 `--resume <path>` 出现，
        // 两种都只需要认出那串 uuid 本身。
        for tok in line.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-')) {
            if tok.len() == 36 && tok.matches('-').count() == 4 {
                out.insert(tok.to_string());
            }
        }
    }
    out
}

/// 扫出所有会话。读不动的文件跳过而不是整个失败 —— 一份坏 transcript 不该
/// 让整页打不开。
pub fn list_sessions() -> Result<Vec<SessionInfo>, AppError> {
    let root = sessions_root()?;
    let live = live_session_ids();
    let mut out = Vec::new();
    let Ok(projects) = std::fs::read_dir(&root) else {
        return Ok(out);
    };
    for proj in projects.flatten() {
        let Ok(files) = std::fs::read_dir(proj.path()) else {
            continue;
        };
        for f in files.flatten() {
            let path = f.path();
            if path.extension().and_then(|s| s.to_str()) != Some("jsonl") {
                continue;
            }
            if let Some(info) = scan(&path, &live) {
                out.push(info);
            }
        }
    }
    // 大的排前面：这一页的用途就是找出哪条快撑爆了。
    out.sort_by_key(|s| std::cmp::Reverse(s.peak_context));
    Ok(out)
}

/// 删掉选中的会话。不可恢复 —— Claude Code 没有回收站，所以调用方必须先弹确认。
///
/// 硬约束：
/// * 路径 canonicalize 之后必须落在 `~/.claude/projects` 下面，防止 `../` 逃出；
/// * 只认 `uuid.jsonl`，不认备份、目录、别的扩展名；
/// * 活着的会话跳过：进程里有内存态，删了它一落盘又写回来，用户会以为没删掉。
///
/// 同 uuid 旁边的 `.jsonl.bak-*` 一起清 —— 那是救援留下的备份，只删 jsonl
/// 腾不出真正的空间。
pub fn delete_sessions(paths: &[String]) -> Result<DeleteReport, AppError> {
    delete_under(&sessions_root()?, &live_session_ids(), paths)
}

fn delete_under(
    root: &Path,
    live: &HashSet<String>,
    paths: &[String],
) -> Result<DeleteReport, AppError> {
    if paths.is_empty() {
        return Err(AppError::Config("没有选中任何会话".into()));
    }
    let root = root.canonicalize().map_err(|e| {
        AppError::Io(format!("找不到会话目录 {}：{e}", root.display()))
    })?;

    let mut report = DeleteReport {
        deleted: 0,
        bytes: 0,
        skipped_live: Vec::new(),
        errors: Vec::new(),
    };
    // 同一条会话被勾两次只删一次。
    let mut seen = HashSet::new();
    for raw in paths {
        let path = PathBuf::from(raw);
        let canon = match path.canonicalize() {
            Ok(p) => p,
            Err(e) => {
                report.errors.push(format!("{raw}：找不到（{e}）"));
                continue;
            }
        };
        if !seen.insert(canon.clone()) {
            continue;
        }
        match delete_one(&root, live, &canon) {
            Ok(DeleteOne::Deleted { bytes }) => {
                report.deleted += 1;
                report.bytes += bytes;
            }
            Ok(DeleteOne::Live(id)) => report.skipped_live.push(id),
            Err(e) => report.errors.push(format!("{}：{e}", canon.display())),
        }
    }
    Ok(report)
}

enum DeleteOne {
    Deleted { bytes: u64 },
    Live(String),
}

fn delete_one(root: &Path, live: &HashSet<String>, canon: &Path) -> Result<DeleteOne, AppError> {
    if !is_session_file(root, canon) {
        return Err(AppError::Config(
            "不是会话文件（只接受 ~/.claude/projects 下的 .jsonl）".into(),
        ));
    }
    let id = canon
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string();
    if live.contains(&id) {
        return Ok(DeleteOne::Live(id));
    }

    let mut bytes = std::fs::metadata(canon).map(|m| m.len()).unwrap_or(0);
    std::fs::remove_file(canon)
        .map_err(|e| AppError::Io(format!("删不掉：{e}")))?;

    // 救援留下的备份跟会话是一对，只删 jsonl 磁盘上还是那几十 MB。
    if let Some(dir) = canon.parent() {
        let prefix = format!("{id}.jsonl.bak-");
        if let Ok(entries) = std::fs::read_dir(dir) {
            for e in entries.flatten() {
                let name = e.file_name();
                let Some(name) = name.to_str() else { continue };
                if name.starts_with(&prefix) {
                    bytes += e.metadata().map(|m| m.len()).unwrap_or(0);
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }
    Ok(DeleteOne::Deleted { bytes })
}

/// `root/<project>/<uuid>.jsonl`，和 [`list_sessions`] 扫的形状对齐。
/// canonicalize 之后再比前缀，避免 `../` 和指向根外的符号链接。
fn is_session_file(root: &Path, canon: &Path) -> bool {
    if !canon.is_file() {
        return false;
    }
    let name = canon.file_name().and_then(|s| s.to_str()).unwrap_or("");
    // 备份是 `uuid.jsonl.bak-<ts>`，ends_with(".jsonl") 过不了，这正是要的。
    if !name.ends_with(".jsonl") {
        return false;
    }
    let Some(parent) = canon.parent() else {
        return false;
    };
    // 允许 root 下一层项目目录，也容忍误放在 root 根上的文件。
    parent == root || parent.parent() == Some(root)
}

/// 扫一份文件。为了不把几十 MB 全解析一遍，只对可能有用的行做 JSON 解析。
fn scan(path: &Path, live: &HashSet<String>) -> Option<SessionInfo> {
    let meta = std::fs::metadata(path).ok()?;
    let file = std::fs::File::open(path).ok()?;
    let id = path.file_stem()?.to_str()?.to_string();

    let (mut entries, mut last, mut peak, mut compactions) = (0usize, 0u64, 0u64, 0usize);
    let (mut cwd, mut slug) = (String::new(), String::new());
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        if line.trim().is_empty() {
            continue;
        }
        entries += 1;
        // 先做字节判断再解析：几十 MB 里真正需要解析的通常不到一半。
        let interesting = line.contains("\"usage\"")
            || line.contains("compact_boundary")
            || cwd.is_empty()
            || slug.is_empty();
        if !interesting {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if cwd.is_empty() {
            if let Some(s) = v.get("cwd").and_then(Value::as_str) {
                cwd = s.to_string();
            }
        }
        if slug.is_empty() {
            if let Some(s) = v.get("slug").and_then(Value::as_str) {
                slug = s.to_string();
            }
        }
        if is_boundary(&v) {
            compactions += 1;
        }
        let (l, p) = real_context(std::slice::from_ref(&v));
        if l > 0 {
            last = l;
            peak = peak.max(p);
        }
    }

    Some(SessionInfo {
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
        compactions,
    })
}

/// 就地改写：对每个 (所在对象, 键) 调 `f`，返回 `Some(新值)` 就替换。
fn rewrite(node: &mut Value, f: &mut impl FnMut(&serde_json::Map<String, Value>, &str, &Value) -> Option<Value>) {
    match node {
        Value::Object(map) => {
            // 先算出要改什么，再改 —— 边遍历边改会借用冲突。
            let plan: Vec<(String, Value)> = map
                .iter()
                .filter_map(|(k, v)| f(map, k, v).map(|nv| (k.clone(), nv)))
                .collect();
            for (k, nv) in plan {
                map.insert(k, nv);
            }
            for (_, v) in map.iter_mut() {
                rewrite(v, f);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|v| rewrite(v, f)),
        _ => {}
    }
}

/// 瘦身。图片换占位符、超长文本留首尾，直到预计上下文降到 `target` 以下。
///
/// `target` 是**真实**口径。内部按字符估算排序，再用「真实 / 估算」的换算
/// 系数折算回来 —— 直接按估算砍会砍过头（实测估算比真值高三倍多）。
pub fn slim(path: &str, target: u64, text_limit: usize) -> Result<SlimReport, AppError> {
    let path = PathBuf::from(path);
    let id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string();
    if live_session_ids().contains(&id) {
        return Err(AppError::Config(
            "这个会话正被一个 Claude Code 进程使用。先退出那个窗口再来 —— 进程里有内存态，现在改会被它盖回去。".into(),
        ));
    }

    let mut entries = load(&path)?;
    let bytes_before = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let (last, _) = real_context(&entries);
    if last == 0 {
        return Err(AppError::Config(
            "这份 transcript 里没有 usage 记录，拿不到真实上下文 —— 不敢下手。".into(),
        ));
    }
    let weight: u64 = entries.iter().map(context_weight).sum();
    let scale = if weight > 0 {
        last as f64 / weight as f64
    } else {
        1.0
    };
    let need_est = if last > target {
        ((last - target) as f64 / scale) as u64
    } else {
        0
    };

    // 从最肥的开始砍，砍够就停 —— 能少动一条是一条。
    let mut order: Vec<usize> = (0..entries.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(context_weight(&entries[i])));

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

    let backup = save(&path, &entries)?;
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

/// 把图片换成一句占位符。返回省下的估算 token。
fn strip_images(entry: &mut Value) -> u64 {
    let saved = image_tokens(entry);
    if saved == 0 {
        return 0;
    }
    rewrite(entry, &mut |parent, key, val| {
        is_b64_image(parent, key, val).then(|| Value::String(String::new()))
    });
    // base64 节点整个换成文本块，模型看到的是「这里原本有张图」，
    // 而不是一个来路不明的空字段。
    rewrite(entry, &mut |_, _, val| {
        let obj = val.as_object()?;
        (obj.get("type").and_then(Value::as_str) == Some("base64")).then(|| {
            json!({ "type": "text", "text": "[图片已被会话救援移除以腾出上下文]" })
        })
    });
    saved
}

/// 超长文本留首尾，中间换成一行说明。只动 `message`（进上下文的那份）。
fn truncate_texts(entry: &mut Value, limit: usize) -> u64 {
    let Some(msg) = entry.get_mut("message") else {
        return 0;
    };
    let mut saved = 0u64;
    rewrite(msg, &mut |parent, key, val| {
        let s = val.as_str()?;
        if s.chars().count() <= limit || is_b64_image(parent, key, val) {
            return None;
        }
        let chars: Vec<char> = s.chars().collect();
        let (head, tail) = (limit / 2, limit - limit / 2);
        let cut = chars.len() - limit;
        let new: String = chars[..head]
            .iter()
            .collect::<String>()
            + &format!("\n\n… [会话救援截掉中间 {cut} 字符以腾出上下文] …\n\n")
            + &chars[chars.len() - tail..].iter().collect::<String>();
        saved += est_text_tokens(s).saturating_sub(est_text_tokens(&new));
        Some(Value::String(new))
    });
    saved
}

/// 造一个 v4 uuid。只为了给新记录一个不撞的 id，不需要密码学强度。
pub(crate) fn uuid_v4() -> String {
    let mut b = [0u8; 16];
    rand::thread_rng().fill(&mut b);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

pub(crate) fn now_iso() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // 只需要一个格式合法、单调递增的时间戳；不引日期库。
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let (mut y, mut d) = (1970u64, days);
    loop {
        let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
        let len = if leap { 366 } else { 365 };
        if d < len {
            break;
        }
        d -= len;
        y += 1;
    }
    let leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
    let ml = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut m = 0;
    while m < 12 && d >= ml[m] {
        d -= ml[m];
        m += 1;
    }
    format!(
        "{y:04}-{:02}-{:02}T{:02}:{:02}:{:02}.000Z",
        m + 1,
        d + 1,
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// 把一条记录渲染成给总结模型看的一行。不认识的类型返回 None。
fn render(entry: &Value) -> Option<String> {
    let kind = entry.get("type").and_then(Value::as_str)?;
    if kind != "user" && kind != "assistant" {
        return None;
    }
    let content = entry.pointer("/message/content")?;
    let mut buf = String::new();
    match content {
        Value::String(s) => buf.push_str(s),
        Value::Array(parts) => {
            for p in parts {
                match p.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(t) = p.get("text").and_then(Value::as_str) {
                            buf.push_str(t);
                            buf.push('\n');
                        }
                    }
                    Some("tool_use") => {
                        let name = p.get("name").and_then(Value::as_str).unwrap_or("?");
                        buf.push_str(&format!("[调用工具 {name}]\n"));
                    }
                    Some("tool_result") => {
                        // 工具结果只留个头：总结需要知道「跑过什么、结论是什么」，
                        // 不需要那几百行原始输出。
                        let t = p
                            .get("content")
                            .map(|c| c.to_string())
                            .unwrap_or_default();
                        buf.push_str(&format!("[工具结果 {}]\n", t.chars().take(300).collect::<String>()));
                    }
                    // 图片在这一层已经没有意义：总结模型看不到它，
                    // 而 base64 会把这一块撑爆。
                    Some("image") => buf.push_str("[图片]\n"),
                    _ => {}
                }
            }
        }
        _ => return None,
    }
    let buf = buf.trim();
    if buf.is_empty() {
        return None;
    }
    Some(format!("{kind}: {buf}"))
}

/// 分块总结。
///
/// # 为什么这样不会死锁
///
/// `/compact` 是**一次**请求发全部，所以上下文越限时它自己也发不出去。这里把
/// 需要总结的部分切成每块 `chunk_tokens` 的小段，各自总结再合并 —— 任何一次
/// 请求都远在天花板以下。
///
/// # 产物
///
/// 追加两条，**旧内容一个字节不动**（这是 Claude Code 自己压缩时的原生做法，
/// 格式逐字段对照过真实 transcript）：
///
/// 1. `type=system, subtype=compact_boundary`，`parentUuid: null` —— 靠这个
///    null 把链剪断，边界以前的条目就不在活动链上了，自然不进上下文；
///    `logicalParentUuid` 留着指回剪断前的最后一条，历史仍可回溯。
/// 2. `type=user, isCompactSummary: true`，正文就是摘要。
///
/// 保留尾巴靠 `compactMetadata.preservedMessages.uuids` 点名，不靠 parent 链 ——
/// 那些条目已经在文件里了，重复写一遍会出现两个相同 uuid。
pub async fn compact(
    base_url: &str,
    token: &str,
    model: &str,
    path: &str,
    keep_tail: usize,
    chunk_tokens: u64,
) -> Result<CompactReport, AppError> {
    let path = PathBuf::from(path);
    let id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string();
    if live_session_ids().contains(&id) {
        return Err(AppError::Config(
            "这个会话正被一个 Claude Code 进程使用。先退出那个窗口再来。".into(),
        ));
    }
    if token.is_empty() {
        return Err(AppError::Config(
            "没有可用的客户端令牌 —— 分块总结要走内核的 /v1/messages。先在设置里生成一个。".into(),
        ));
    }

    let entries = load(&path)?;
    let (last, _) = real_context(&entries);

    // 只总结**当前活动链**：最后一个压缩边界之后的部分。边界以前的本来就
    // 不进上下文，再总结一遍纯属烧 token。
    let start = entries
        .iter()
        .rposition(is_boundary)
        .map(|i| i + 1)
        .unwrap_or(0);
    let live_slice = &entries[start..];

    // 尾巴留原文：最近几轮是用户正在做的事，摘要替代不了。
    let msg_idx: Vec<usize> = live_slice
        .iter()
        .enumerate()
        .filter(|(_, e)| render(e).is_some())
        .map(|(i, _)| i)
        .collect();
    let tail_start = msg_idx.len().saturating_sub(keep_tail);
    let tail_idx: Vec<usize> = msg_idx[tail_start..].to_vec();
    let to_summarize: Vec<&Value> = msg_idx[..tail_start]
        .iter()
        .map(|&i| &live_slice[i])
        .collect();

    if to_summarize.is_empty() {
        return Err(AppError::Config(
            "这条会话里没有够得上总结的内容 —— 要么已经压过了，要么它本来就不长。".into(),
        ));
    }

    // 切块。按估算权重累加，超过一块就换下一块。
    let mut chunks: Vec<String> = Vec::new();
    let (mut buf, mut acc) = (String::new(), 0u64);
    for e in &to_summarize {
        let Some(line) = render(e) else { continue };
        let w = est_text_tokens(&line);
        if acc + w > chunk_tokens && !buf.is_empty() {
            chunks.push(std::mem::take(&mut buf));
            acc = 0;
        }
        buf.push_str(&line);
        buf.push_str("\n\n");
        acc += w;
    }
    if !buf.is_empty() {
        chunks.push(buf);
    }

    // 逐块总结。串行而不是并发：这些请求打的是同一个渠道，并发只会把它
    // 顶到限流，而这里本来就不赶时间。
    let mut partials = Vec::new();
    for (i, c) in chunks.iter().enumerate() {
        let prompt = format!(
            "下面是一段编程对话的第 {}/{} 段。请写一份**事实性**摘要，覆盖：\
             用户提出的每一个要求与意图、改过的文件与函数、做过的决定和它的理由、\
             遇到的错误与最终怎么解决的、以及尚未完成的事。不要评价，不要省略具体的\
             文件名和命令。\n\n---\n{}",
            i + 1,
            chunks.len(),
            c
        );
        partials.push(ask(base_url, token, model, &prompt).await?);
    }

    // 合并。一段的时候不用再问一次模型。
    let summary = if partials.len() == 1 {
        partials.remove(0)
    } else {
        let joined = partials.join("\n\n---\n\n");
        ask(
            base_url,
            token,
            model,
            &format!(
                "下面是同一段编程对话按先后顺序切成几段后各自的摘要。\
                 把它们合并成一份连贯的摘要，保留所有具体的文件名、命令、决定和未完成事项，\
                 去掉重复。\n\n---\n{joined}"
            ),
        )
        .await?
    };

    let body = format!(
        "This session is being continued from a previous conversation that ran out of context. \
         The summary below covers the earlier portion of the conversation.\n\nSummary:\n{summary}"
    );

    // 接线。字段名和取值逐个对照真实 transcript，不是照着印象写的。
    let meta = entries
        .iter()
        .rev()
        .find(|e| e.get("cwd").is_some())
        .cloned()
        .unwrap_or_else(|| json!({}));
    let field = |k: &str| meta.get(k).cloned().unwrap_or(Value::Null);

    let tail_uuids: Vec<Value> = tail_idx
        .iter()
        .filter_map(|&i| live_slice[i].get("uuid").cloned())
        .collect();
    let logical_parent = tail_uuids.last().cloned().unwrap_or(Value::Null);
    let head_uuid = tail_uuids.first().cloned().unwrap_or(Value::Null);

    let boundary_uuid = uuid_v4();
    let summary_uuid = uuid_v4();
    let ts = now_iso();

    let boundary = json!({
        "parentUuid": Value::Null,            // ← 就是这个 null 把链剪断
        "logicalParentUuid": logical_parent,
        "isSidechain": false,
        "type": "system",
        "subtype": "compact_boundary",
        "content": "Conversation compacted",
        "level": "info",
        "compactMetadata": {
            "trigger": "manual",
            "preTokens": last,
            "postTokens": est_text_tokens(&body),
            "cumulativeDroppedTokens": last.saturating_sub(est_text_tokens(&body)),
            "durationMs": 0,
            "preCompactDiscoveredTools": [],
            "preservedSegment": {
                "headUuid": head_uuid,
                "anchorUuid": summary_uuid,
                "tailUuid": logical_parent,
            },
            "preservedMessages": {
                "anchorUuid": summary_uuid,
                "uuids": tail_uuids,
                "allUuids": tail_uuids,
            },
        },
        "uuid": boundary_uuid,
        "timestamp": ts,
        "userType": field("userType"),
        "entrypoint": field("entrypoint"),
        "cwd": field("cwd"),
        "sessionId": field("sessionId"),
        "version": field("version"),
        "gitBranch": field("gitBranch"),
        "slug": field("slug"),
    });
    let summary_entry = json!({
        "parentUuid": boundary_uuid,
        "isSidechain": false,
        "promptId": uuid_v4(),
        "type": "user",
        "message": { "role": "user", "content": body },
        "isVisibleInTranscriptOnly": true,
        "isCompactSummary": true,
        "uuid": summary_uuid,
        "timestamp": ts,
        "session_id": field("sessionId"),
        "sessionId": field("sessionId"),
        "userType": field("userType"),
        "entrypoint": field("entrypoint"),
        "cwd": field("cwd"),
        "version": field("version"),
        "gitBranch": field("gitBranch"),
        "slug": field("slug"),
    });

    let mut out = entries;
    out.push(boundary);
    out.push(summary_entry);
    let summary_tokens = est_text_tokens(&body);
    let backup = save(&path, &out)?;

    Ok(CompactReport {
        chunks: chunks.len(),
        kept_tail: tail_idx.len(),
        context_before: last,
        summary_tokens,
        backup,
        model: model.to_string(),
    })
}

/// 问内核要一段总结。走 `/v1/messages` —— 模型别名、路由、故障转移都归内核管。
async fn ask(base_url: &str, token: &str, model: &str, prompt: &str) -> Result<String, AppError> {
    // no_proxy：内核多半在 127.0.0.1，而这台机器上常年挂着 HTTP_PROXY，
    // 默认客户端会把回环请求也交给代理，表现是「服务明明起着却连不上」。
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .map_err(|e| AppError::Network(format!("构建 HTTP 客户端失败：{e}")))?;
    let endpoint = format!("{}/v1/messages", base_url.trim_end_matches('/'));
    let resp = client
        .post(&endpoint)
        .header("x-api-key", token)
        .header("authorization", format!("Bearer {token}"))
        .header("anthropic-version", "2023-06-01")
        .json(&json!({
            "model": model,
            "max_tokens": 8192,
            "messages": [{ "role": "user", "content": prompt }],
        }))
        .send()
        .await
        .map_err(|e| AppError::Network(format!("请求内核失败：{e}")))?;

    let status = resp.status();
    let body: Value = resp
        .json()
        .await
        .map_err(|e| AppError::Network(format!("内核返回的不是 JSON：{e}")))?;
    if !status.is_success() {
        let msg = body
            .pointer("/error/message")
            .and_then(Value::as_str)
            .unwrap_or("未知错误");
        return Err(AppError::Upstream {
            status: status.as_u16(),
            message: msg.to_string(),
        });
    }
    let text = body
        .get("content")
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    if text.trim().is_empty() {
        return Err(AppError::Config("模型没有返回任何摘要文本".into()));
    }
    Ok(text)
}

/// 内核把 prompt-too-long 当客户端错误（400 invalid-argument），不切渠道。
/// 压缩这边要自己认出来再换下一个窗口够的模型。
pub fn is_prompt_too_long(err: &AppError) -> bool {
    match err {
        AppError::Upstream { status, message } => {
            *status == 400
                && (message.contains("maximum prompt length")
                    || message.contains("too long")
                    || message.contains("context length")
                    || message.contains("context window"))
        }
        _ => false,
    }
}

/// 读一份 transcript 的真实上下文。给自动挑选压缩模型用，避免再估一遍。
pub fn last_context_of(path: &str) -> Result<u64, AppError> {
    let entries = load(std::path::Path::new(path))?;
    Ok(real_context(&entries).0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assistant(input: u64, cache_read: u64) -> Value {
        json!({
            "type": "assistant",
            "message": { "usage": {
                "input_tokens": input,
                "cache_read_input_tokens": cache_read,
                "cache_creation_input_tokens": 0,
            }},
        })
    }

    /// 真实上下文必须把两个 cache 字段加回来。
    ///
    /// `input_tokens` 只算没命中缓存的部分 —— 长会话里它常年是个位数，光看它
    /// 会得出「上下文只有 2 tokens」这种结论，而同一刻上游正因为 51 万超限报 400。
    #[test]
    fn real_context_counts_cache_not_just_input() {
        let entries = vec![assistant(2, 164_171), assistant(5, 3)];
        let (last, peak) = real_context(&entries);
        assert_eq!(peak, 164_173, "峰值要取所有轮次里最大的那个");
        assert_eq!(last, 8, "最后一轮取最后一条，不是最大的那条");
    }

    /// 没有 usage 就返回 0，让调用方拒绝动手而不是拿估算值去砍。
    #[test]
    fn real_context_is_zero_without_usage() {
        let entries = vec![json!({"type": "user", "message": {"content": "hi"}})];
        assert_eq!(real_context(&entries), (0, 0));
    }

    /// 权重只看 `message`。`toolUseResult` 是本地副本、元数据每行都有一份，
    /// 算进去会高出好几倍，照着它砍会砍过头。
    #[test]
    fn weight_ignores_local_copies_and_metadata() {
        let long = "x".repeat(3500);
        let e = json!({
            "type": "user",
            "message": { "content": [{ "type": "text", "text": long }] },
            "toolUseResult": { "stdout": "y".repeat(35_000) },
            "cwd": "/Users/someone/a/very/long/path/that/repeats/every/single/line",
        });
        // 3500 字符 / 3.5 = 1000，外加几个结构字段（"text" 这类）的零头。
        // 关键是它必须停在这个量级：把 toolUseResult 也算进去会是十倍。
        let w = context_weight(&e);
        assert!((1000..1010).contains(&w), "权重跑偏了：{w}");
    }

    /// 非对话记录不进上下文，权重必须是 0 —— 否则排序会去砍
    /// file-history-snapshot 这种砍了也没用的东西。
    #[test]
    fn weight_of_non_conversation_entries_is_zero() {
        for kind in ["file-history-snapshot", "attachment", "system", "mode"] {
            let e = json!({ "type": kind, "message": { "content": "x".repeat(10_000) } });
            assert_eq!(context_weight(&e), 0, "{kind}");
        }
    }

    /// 只按键名判 base64 会误伤：正常的长文本字段也可能叫 `data`。
    #[test]
    fn b64_detection_needs_an_image_sibling() {
        let img = json!({ "type": "base64", "media_type": "image/png", "data": "A".repeat(5000) });
        let obj = img.as_object().unwrap();
        assert!(is_b64_image(obj, "data", obj.get("data").unwrap()));

        let not_img = json!({ "type": "text", "data": "A".repeat(5000) });
        let obj = not_img.as_object().unwrap();
        assert!(!is_b64_image(obj, "data", obj.get("data").unwrap()));

        // 短的一律不是 —— 省得为每个小字符串去翻兄弟字段。
        let short = json!({ "type": "base64", "data": "A".repeat(10) });
        let obj = short.as_object().unwrap();
        assert!(!is_b64_image(obj, "data", obj.get("data").unwrap()));
    }

    /// 砍图之后 uuid 必须原样在：链断了，恢复出来的会话就缺胳膊少腿。
    #[test]
    fn stripping_images_keeps_the_uuid_chain() {
        let mut e = json!({
            "type": "user",
            "uuid": "u-1",
            "parentUuid": "u-0",
            "message": { "content": [
                { "type": "image", "source": {
                    "type": "base64", "media_type": "image/png", "data": "A".repeat(9000) }},
            ]},
        });
        assert!(strip_images(&mut e) > 0);
        assert_eq!(e["uuid"], "u-1");
        assert_eq!(e["parentUuid"], "u-0");
        let s = serde_json::to_string(&e).unwrap();
        assert!(!s.contains(&"A".repeat(100)), "base64 还在");
        assert!(s.contains("图片已被会话救援移除"), "没留下占位符");
    }

    /// 有尺寸就按像素算。一张 2000×1288 的截图约 3435 tokens，
    /// 用兜底常数 1500 会把它的优先级排低一半。
    #[test]
    fn image_tokens_prefer_real_dimensions() {
        let e = json!({
            "type": "user",
            "message": { "content": [{ "type": "image", "source": {
                "type": "base64", "media_type": "image/png", "data": "A".repeat(9000) }}]},
            "toolUseResult": { "file": { "dimensions": {
                "originalWidth": 2000, "originalHeight": 1288 }}},
        });
        assert_eq!(image_tokens(&e), 2000 * 1288 / 750);
    }

    /// 截断保首尾。中间几百行 grep 输出可以丢，
    /// 「跑了什么、结论是什么」不能丢。
    #[test]
    fn truncation_keeps_both_ends() {
        let text = format!("{}{}{}", "HEAD".repeat(50), "M".repeat(9000), "TAIL".repeat(50));
        let mut e = json!({
            "type": "user",
            "message": { "content": [{ "type": "text", "text": text }] },
        });
        assert!(truncate_texts(&mut e, 400) > 0);
        let got = e.pointer("/message/content/0/text").unwrap().as_str().unwrap();
        assert!(got.starts_with("HEAD"), "头没了");
        assert!(got.ends_with("TAIL"), "尾没了");
        assert!(got.contains("会话救援截掉"), "没说明截过");
        assert!(!got.contains(&"M".repeat(500)), "中间没截掉");
    }

    /// 短文本不动 —— 否则每条消息都会被塞进一句「截掉 0 字符」。
    #[test]
    fn truncation_leaves_short_text_alone() {
        let mut e = json!({
            "type": "user",
            "message": { "content": [{ "type": "text", "text": "短" }] },
        });
        assert_eq!(truncate_texts(&mut e, 400), 0);
        assert_eq!(e.pointer("/message/content/0/text").unwrap(), "短");
    }

    /// 渲染给总结模型看的时候，图片和超长工具结果都必须收敛掉 ——
    /// 否则 base64 会把「分块」这件事本身撑爆。
    #[test]
    fn rendering_collapses_images_and_tool_results() {
        let e = json!({
            "type": "assistant",
            "message": { "content": [
                { "type": "text", "text": "在改这个函数" },
                { "type": "tool_use", "name": "Edit" },
                { "type": "tool_result", "content": "z".repeat(10_000) },
                { "type": "image", "source": { "type": "base64", "data": "A".repeat(9000) }},
            ]},
        });
        let out = render(&e).unwrap();
        assert!(out.starts_with("assistant: "));
        assert!(out.contains("在改这个函数"));
        assert!(out.contains("[调用工具 Edit]"));
        assert!(out.contains("[图片]"));
        assert!(out.len() < 1200, "工具结果没收敛：{}", out.len());
    }

    /// 非对话记录渲染成 None，不然切块时会混进一堆 mode / attachment。
    #[test]
    fn rendering_skips_non_conversation() {
        assert!(render(&json!({ "type": "mode", "message": { "content": "x" }})).is_none());
        assert!(render(&json!({ "type": "user" })).is_none());
        assert!(render(&json!({ "type": "user", "message": { "content": [] }})).is_none());
    }

    /// uuid 得是合法 v4 且不重复 —— 撞了就等于把两条记录接成一条。
    #[test]
    fn uuid_is_v4_and_unique() {
        let a = uuid_v4();
        assert_eq!(a.len(), 36);
        assert_eq!(a.as_bytes()[14], b'4', "版本位不对：{a}");
        assert!(matches!(a.as_bytes()[19], b'8' | b'9' | b'a' | b'b'), "变体位不对：{a}");
        let set: HashSet<String> = (0..500).map(|_| uuid_v4()).collect();
        assert_eq!(set.len(), 500);
    }

    /// 时间戳要能被解析成 ISO8601。格式写错的话 Claude Code 读到的是一条
    /// 时间未知的记录，排序会乱。
    #[test]
    fn timestamp_looks_like_iso8601() {
        let t = now_iso();
        assert_eq!(t.len(), 24, "{t}");
        assert!(t.ends_with('Z') && t.contains('T'), "{t}");
        let (date, time) = t.split_once('T').unwrap();
        let d: Vec<&str> = date.split('-').collect();
        assert_eq!(d.len(), 3);
        assert!((2020..2100).contains(&d[0].parse::<u32>().unwrap()), "{t}");
        assert!((1..=12).contains(&d[1].parse::<u32>().unwrap()), "{t}");
        assert!((1..=31).contains(&d[2].parse::<u32>().unwrap()), "{t}");
        assert!((0..24).contains(&time[0..2].parse::<u32>().unwrap()), "{t}");
    }

    /// 压缩边界靠 subtype 认，不靠 content 文案 —— 文案是会变的。
    #[test]
    fn boundary_detection_uses_subtype() {
        assert!(is_boundary(&json!({ "type": "system", "subtype": "compact_boundary" })));
        assert!(!is_boundary(&json!({ "type": "system", "content": "Conversation compacted" })));
    }

    /// 端到端：造一份形状贴近真实的 transcript，落盘、瘦身、再读回来。
    ///
    /// 单元测试各自只盯一个函数，但真正会出事的是接缝：备份有没有真的生成、
    /// 权限有没有保住、写回去的文件还能不能解析、uuid 链有没有断。这些只有
    /// 走完整条路才看得见。
    #[test]
    fn slim_round_trips_a_real_shaped_transcript() {
        let dir = std::env::temp_dir().join(format!("ccload-rescue-{}", uuid_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        // 文件名必须是 uuid：`slim` 拿它去比对活动会话。
        let path = dir.join(format!("{}.jsonl", uuid_v4()));

        let mut lines: Vec<Value> = Vec::new();
        let mut prev = Value::Null;
        for i in 0..40 {
            let uuid = uuid_v4();
            // 交替放：一条带大图的工具结果，一条带超长文本的助手回复。
            let content = if i % 2 == 0 {
                json!([{ "type": "image", "source": {
                    "type": "base64", "media_type": "image/png", "data": "A".repeat(20_000) }}])
            } else {
                json!([{ "type": "text", "text": "B".repeat(20_000) }])
            };
            lines.push(json!({
                "parentUuid": prev,
                "type": if i % 2 == 0 { "user" } else { "assistant" },
                "uuid": uuid,
                "cwd": "/tmp/project",
                "sessionId": "s-1",
                "message": { "role": "user", "content": content },
                // 本地副本：瘦身要把它也砍掉（文件才会变小），但它不该被计入权重。
                "toolUseResult": { "stdout": "C".repeat(20_000) },
            }));
            prev = Value::String(uuid);
        }
        // 真实上下文只能从 usage 来。给最后一条 assistant 挂一个。
        lines.push(json!({
            "parentUuid": prev,
            "type": "assistant",
            "uuid": uuid_v4(),
            "message": { "usage": {
                "input_tokens": 10,
                "cache_read_input_tokens": 400_000,
                "cache_creation_input_tokens": 0,
            }},
        }));

        let body: String = lines
            .iter()
            .map(|v| serde_json::to_string(v).unwrap() + "\n")
            .collect();
        std::fs::write(&path, &body).unwrap();
        let before_bytes = std::fs::metadata(&path).unwrap().len();

        let report = slim(path.to_str().unwrap(), 100_000, 2_000).unwrap();

        // 备份必须真的在，而且是**改之前**的内容 —— 这是唯一的后悔药。
        assert!(std::fs::metadata(&report.backup).unwrap().len() == before_bytes);
        assert_eq!(std::fs::read_to_string(&report.backup).unwrap(), body);

        assert!(report.images_stripped > 0, "一张图都没砍");
        assert!(report.texts_truncated > 0, "一处长文本都没截");
        assert!(report.bytes_after < report.bytes_before, "文件没变小");
        assert_eq!(report.context_before, 400_010, "真实上下文取错了");
        assert!(report.context_after <= 100_000, "没砍到目标以下");

        // 写回去的东西必须还能解析，且链一条不少、一条不错。
        let after = load(&path).unwrap();
        assert_eq!(after.len(), lines.len(), "条数变了");
        for (a, b) in lines.iter().zip(after.iter()) {
            assert_eq!(a["uuid"], b["uuid"], "uuid 被动过");
            assert_eq!(a["parentUuid"], b["parentUuid"], "parentUuid 被动过");
            assert_eq!(a["type"], b["type"], "type 被动过");
        }
        let dumped = serde_json::to_string(&after).unwrap();

        // 砍够就停，不是能砍的都砍 —— 每多改一条就多丢一份信息。
        // 所以这里钉的是「剩下的图片数 = 总数 − 报告说砍掉的数」，
        // 而不是「一张图都不剩」。
        let remaining = after
            .iter()
            .filter(|e| serde_json::to_string(e).unwrap().contains(&"A".repeat(200)))
            .count();
        assert_eq!(
            remaining,
            20 - report.images_stripped,
            "砍掉的图片数和报告对不上"
        );
        assert!(report.images_stripped < 20, "全砍了，没有做到最小改动");

        // `toolUseResult` 的**文本**是故意留着的：它不进上下文、不花一个 token，
        // 却是 transcript 界面回滚时渲染用的那份。砍它只会让用户翻不了旧账，
        // 换不来任何额度。图片是例外 —— 同一张图在两处各存一份，纯占地方。
        assert!(
            dumped.contains(&"C".repeat(10_000)),
            "本地副本的文本被误砍了"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// 没有 usage 就必须拒绝动手：拿估算值当真实值去砍，砍多砍少都是瞎砍。
    #[test]
    fn slim_refuses_without_ground_truth() {
        let dir = std::env::temp_dir().join(format!("ccload-rescue-{}", uuid_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{}.jsonl", uuid_v4()));
        std::fs::write(
            &path,
            serde_json::to_string(&json!({
                "type": "user", "uuid": "u1",
                "message": { "content": [{ "type": "text", "text": "x".repeat(50_000) }] },
            }))
            .unwrap()
                + "\n",
        )
        .unwrap();

        let err = slim(path.to_str().unwrap(), 1_000, 500).unwrap_err();
        assert!(err.to_string().contains("没有 usage"), "报错没说清原因：{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 坏行必须整体报错并指出行号，而不是跳过。
    ///
    /// 跳过的后果是：写回去的时候那一行就永久消失了，而它可能正是链上的一环。
    #[test]
    fn load_rejects_a_corrupt_line_instead_of_skipping_it() {
        let dir = std::env::temp_dir().join(format!("ccload-rescue-{}", uuid_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("broken.jsonl");
        std::fs::write(&path, "{\"type\":\"user\"}\n{not json}\n").unwrap();

        let err = load(&path).unwrap_err();
        assert!(err.to_string().contains("第 2 行"), "没指出是哪一行：{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn prompt_too_long_is_a_400_with_the_upstream_wording() {
        assert!(is_prompt_too_long(&AppError::Upstream {
            status: 400,
            message: "This model's maximum prompt length is 500000 but the request contains 517306 tokens.".into(),
        }));
        assert!(!is_prompt_too_long(&AppError::Upstream {
            status: 429,
            message: "rate limited".into(),
        }));
        assert!(!is_prompt_too_long(&AppError::Config("nope".into())));
    }

    fn write_session(dir: &Path, id: &str, body: &str) -> PathBuf {
        let path = dir.join(format!("{id}.jsonl"));
        std::fs::write(&path, body).unwrap();
        path
    }

    /// 删 jsonl 的同时清掉救援留下的备份，否则磁盘上还是那几十 MB。
    #[test]
    fn delete_removes_jsonl_and_its_backups() {
        let root = std::env::temp_dir().join(format!("ccload-del-{}", uuid_v4()));
        let proj = root.join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        let id = uuid_v4();
        let path = write_session(&proj, &id, "{\"type\":\"user\"}\n");
        let bak = proj.join(format!("{id}.jsonl.bak-1"));
        std::fs::write(&bak, "old").unwrap();
        // 别的会话的备份不能被捎走。
        let other = uuid_v4();
        let other_bak = proj.join(format!("{other}.jsonl.bak-1"));
        std::fs::write(&other_bak, "keep").unwrap();

        let report = delete_under(&root, &HashSet::new(), &[path.display().to_string()]).unwrap();
        assert_eq!(report.deleted, 1);
        assert!(report.bytes > 0);
        assert!(!path.exists(), "jsonl 还在");
        assert!(!bak.exists(), "备份还在");
        assert!(other_bak.exists(), "别的会话的备份被误删");
        std::fs::remove_dir_all(&root).ok();
    }

    /// 活着的会话必须跳过：删了进程一落盘又写回来，用户会以为没删掉。
    #[test]
    fn delete_skips_live_sessions() {
        let root = std::env::temp_dir().join(format!("ccload-del-{}", uuid_v4()));
        let proj = root.join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        let id = uuid_v4();
        let path = write_session(&proj, &id, "{\"type\":\"user\"}\n");
        let live = HashSet::from([id.clone()]);

        let report = delete_under(&root, &live, &[path.display().to_string()]).unwrap();
        assert_eq!(report.deleted, 0);
        assert_eq!(report.skipped_live, vec![id]);
        assert!(path.exists(), "活着的会话不该被动");
        std::fs::remove_dir_all(&root).ok();
    }

    /// 路径必须落在会话根下面。`../` 或根外的文件一律拒绝，不能让这个按钮
    /// 变成任意文件删除器。
    #[test]
    fn delete_refuses_paths_outside_the_sessions_root() {
        let root = std::env::temp_dir().join(format!("ccload-del-{}", uuid_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let outside = std::env::temp_dir().join(format!("ccload-out-{}.jsonl", uuid_v4()));
        std::fs::write(&outside, "secret\n").unwrap();

        let report =
            delete_under(&root, &HashSet::new(), &[outside.display().to_string()]).unwrap();
        assert_eq!(report.deleted, 0);
        assert_eq!(report.errors.len(), 1);
        assert!(outside.exists(), "根外的文件被删了");
        std::fs::remove_file(&outside).ok();
        std::fs::remove_dir_all(&root).ok();
    }

    /// 备份文件本身不能当会话删 —— 它不是 resume 认的那份。
    #[test]
    fn delete_refuses_backup_files() {
        let root = std::env::temp_dir().join(format!("ccload-del-{}", uuid_v4()));
        let proj = root.join("proj");
        std::fs::create_dir_all(&proj).unwrap();
        let bak = proj.join(format!("{}.jsonl.bak-9", uuid_v4()));
        std::fs::write(&bak, "old").unwrap();

        let report = delete_under(&root, &HashSet::new(), &[bak.display().to_string()]).unwrap();
        assert_eq!(report.deleted, 0);
        assert!(bak.exists());
        std::fs::remove_dir_all(&root).ok();
    }

    /// 空选必须报错，而不是假装成功删了 0 个。
    #[test]
    fn delete_refuses_an_empty_selection() {
        let root = std::env::temp_dir().join(format!("ccload-del-{}", uuid_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let err = delete_under(&root, &HashSet::new(), &[]).unwrap_err();
        assert!(err.to_string().contains("没有选中"), "{err}");
        std::fs::remove_dir_all(&root).ok();
    }
}
