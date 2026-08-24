//! 快照对比 —— 一份快照相对「上一份 / 原始 / 磁盘现状」改了什么。
//!
//! 为什么需要：`cli_backups` 那一页原来只有一个「恢复」按钮，而恢复是**不可逆
//! 地覆盖当前配置**。列表上能看到的只有时间、原因和「N 个文件」，看不出这一份
//! 和现在到底差在哪 —— 于是要么凭时间戳赌一把，要么根本不敢点。先看 diff 再决
//! 定，才是这一页应该有的用法。
//!
//! 三个对比基准各回答一个不同的问题：
//!   * `Current`（默认）—— **点「恢复」会把我现在的配置改成什么样？** 这是决定
//!     要不要恢复时唯一相关的问题，所以是默认。
//!   * `Previous` —— 这次快照记录的那一步动作，改了什么？（快照是在写入**之前**
//!     拍的，所以「这一份 vs 上一份」= 上一份到这一份之间那次写入的效果。）
//!   * `Pristine` —— 相对我们接管之前的原始配置，一共漂了多少。

use serde::Serialize;

use crate::error::AppError;
use crate::services::cli_backup::{BackupEntry, BackupStore};
use crate::services::cli_types::{CliTarget, ConfigRoot};

/// 单个文件最多回多少行 diff。
///
/// `~/.claude.json` 动辄几百 KB、上万行，整份灌进 IPC 再让 React 渲染出来，页面
/// 会卡住而没人真的会去读那一万行。超出就截断并如实说明 —— 悄悄截等于骗人。
const MAX_LINES: usize = 4000;

/// 判定「这是二进制，别当文本 diff」的阈值：前若干字节里出现 NUL。
const SNIFF_BYTES: usize = 8192;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DiffBase {
    /// 磁盘上的当前配置。回答「恢复会改成什么样」。
    Current,
    /// 时间上紧邻的上一份快照。回答「这一步动作改了什么」。
    Previous,
    /// 首次接管前的原始配置（`pristine`）。
    Pristine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LineKind {
    Same,
    Add,
    Del,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiffLine {
    pub kind: LineKind,
    pub text: String,
    /// 基准侧行号（1 起）。新增行没有。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_no: Option<usize>,
    /// 快照侧行号（1 起）。删除行没有。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_no: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileDiff {
    /// 相对配置根的路径，例如 `.claude/settings.json`。
    pub rel: String,
    /// 基准侧存在吗。false = 这份快照会**新建**这个文件。
    pub base_exists: bool,
    /// 快照侧存在吗。false = 恢复这份会**删掉**这个文件。
    pub target_exists: bool,
    /// 两边都是文本且能读时才有内容；二进制 / 读不动时为空并置 `note`。
    pub lines: Vec<DiffLine>,
    pub added: usize,
    pub removed: usize,
    /// 内容一致（没有任何增删）。
    pub identical: bool,
    /// 行数超过上限被截断了。
    pub truncated: bool,
    /// 没法做文本 diff 的原因（二进制、读取失败）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct BackupDiff {
    pub id: String,
    pub target: CliTarget,
    pub base: DiffBase,
    /// 基准是什么（「磁盘现状」「原始」…）。时间戳交给前端格式化 —— 快照列表
    /// 已经在那边按本地时区渲染了，这里再实现一次只会出现两种格式。
    pub base_label: String,
    /// 基准快照的 unix 秒，前端据此显示时间。磁盘现状 / 基准缺失时为空。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_created_at: Option<u64>,
    /// 基准不存在时的说明（例如只有一份快照，没有「上一份」）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_missing: Option<String>,
    pub files: Vec<FileDiff>,
}

fn looks_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(SNIFF_BYTES).any(|b| *b == 0)
}

/// 读一个快照里某个文件的内容。`Ok(None)` = 那次快照时这个文件本就不存在。
fn read_snapshot_file(
    store: &BackupStore,
    entry: &BackupEntry,
    rel: &str,
) -> Result<Option<Vec<u8>>, AppError> {
    let Some(f) = entry.files.iter().find(|f| f.rel == rel) else {
        return Ok(None);
    };
    match (&f.stored, f.existed) {
        (Some(stored), true) => {
            let p = store.dir().join(&entry.id).join(stored);
            match std::fs::read(&p) {
                Ok(b) => Ok(Some(b)),
                // 快照文件被手工删了之类 —— 当成「读不到」而不是整次失败。
                Err(_) => Ok(None),
            }
        }
        _ => Ok(None),
    }
}

/// 一次文本 diff。两边都给 `None` 的文件不会走到这里（调用方已过滤）。
fn diff_one(rel: &str, base: Option<Vec<u8>>, target: Option<Vec<u8>>) -> FileDiff {
    let base_exists = base.is_some();
    let target_exists = target.is_some();
    let b = base.unwrap_or_default();
    let t = target.unwrap_or_default();

    if looks_binary(&b) || looks_binary(&t) {
        return FileDiff {
            rel: rel.to_string(),
            base_exists,
            target_exists,
            lines: Vec::new(),
            added: 0,
            removed: 0,
            identical: b == t,
            truncated: false,
            note: Some("二进制文件，不做逐行对比".into()),
        };
    }

    // from_utf8_lossy 而不是拒绝：配置文件里混进一个坏字节不该让整个对比失败。
    let bt = String::from_utf8_lossy(&b).replace("\r\n", "\n");
    let tt = String::from_utf8_lossy(&t).replace("\r\n", "\n");

    if bt == tt {
        return FileDiff {
            rel: rel.to_string(),
            base_exists,
            target_exists,
            lines: Vec::new(),
            added: 0,
            removed: 0,
            identical: true,
            truncated: false,
            note: None,
        };
    }

    let diff = similar::TextDiff::from_lines(&bt, &tt);
    let mut lines = Vec::new();
    let (mut added, mut removed) = (0usize, 0usize);
    let mut truncated = false;

    for change in diff.iter_all_changes() {
        let kind = match change.tag() {
            similar::ChangeTag::Equal => LineKind::Same,
            similar::ChangeTag::Insert => LineKind::Add,
            similar::ChangeTag::Delete => LineKind::Del,
        };
        match kind {
            LineKind::Add => added += 1,
            LineKind::Del => removed += 1,
            LineKind::Same => {}
        }
        // 计数要算完（用户要看到真实的 +N/-M），但只回前 MAX_LINES 行内容。
        if lines.len() >= MAX_LINES {
            truncated = true;
            continue;
        }
        lines.push(DiffLine {
            kind,
            text: change.value().trim_end_matches('\n').to_string(),
            old_no: change.old_index().map(|i| i + 1),
            new_no: change.new_index().map(|i| i + 1),
        });
    }

    FileDiff {
        rel: rel.to_string(),
        base_exists,
        target_exists,
        lines,
        added,
        removed,
        identical: false,
        truncated,
        note: None,
    }
}

/// 算一份快照相对某个基准的 diff。
pub fn diff_backup(
    store: &BackupStore,
    root: &ConfigRoot,
    id: &str,
    base: DiffBase,
) -> Result<BackupDiff, AppError> {
    let all = store.list(None)?;
    let entry = all
        .iter()
        .find(|e| e.id == id)
        .ok_or_else(|| AppError::Config(format!("没有 id 为 {id} 的快照")))?
        .clone();

    // 同一个 target 的快照，按时间从新到旧（`list` 已经是这个顺序）。
    let same: Vec<&BackupEntry> = all.iter().filter(|e| e.target == entry.target).collect();

    let (base_entry, base_label, base_created_at, base_missing) = match base {
        DiffBase::Current => (None, "磁盘现状".to_string(), None, None),
        DiffBase::Previous => {
            let idx = same.iter().position(|e| e.id == entry.id).unwrap_or(0);
            match same.get(idx + 1) {
                Some(prev) => (
                    Some((*prev).clone()),
                    "上一份快照".to_string(),
                    Some(prev.created_at),
                    None,
                ),
                None => (
                    None,
                    "没有更早的快照".to_string(),
                    None,
                    Some("这是该 CLI 最早的一份快照，没有上一份可比。".to_string()),
                ),
            }
        }
        DiffBase::Pristine => match same.iter().find(|e| e.pristine) {
            Some(p) if p.id != entry.id => (
                Some((*p).clone()),
                "原始配置".to_string(),
                Some(p.created_at),
                None,
            ),
            Some(_) => (
                None,
                "这份就是原始".to_string(),
                None,
                Some("这一份本身就是原始快照。".to_string()),
            ),
            None => (
                None,
                "没有原始快照".to_string(),
                None,
                Some("这个 CLI 没有标记为原始的快照。".to_string()),
            ),
        },
    };

    // 要比的文件集合 = 快照里的 ∪ 基准里的。基准里多出来的文件同样重要 ——
    // 那意味着「恢复这一份会把它删掉」。
    let mut rels: Vec<String> = entry.files.iter().map(|f| f.rel.clone()).collect();
    if let Some(be) = &base_entry {
        for f in &be.files {
            if !rels.contains(&f.rel) {
                rels.push(f.rel.clone());
            }
        }
    }
    rels.sort();

    let mut files = Vec::new();
    for rel in rels {
        let target_bytes = read_snapshot_file(store, &entry, &rel)?;
        let base_bytes = match (&base_entry, base) {
            (Some(be), _) => read_snapshot_file(store, be, &rel)?,
            (None, DiffBase::Current) => std::fs::read(root.join(&rel)).ok(),
            // 基准缺失（没有上一份 / 没有原始）：拿空当基准没意义，直接跳过内容比对，
            // 上面的 base_missing 已经把原因说清楚了。
            (None, _) => None,
        };
        if base_bytes.is_none() && target_bytes.is_none() {
            continue;
        }
        files.push(diff_one(&rel, base_bytes, target_bytes));
    }

    Ok(BackupDiff {
        id: entry.id.clone(),
        target: entry.target,
        base,
        base_label,
        base_created_at,
        base_missing,
        files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_is_detected_by_nul_byte() {
        assert!(looks_binary(b"abc\0def"));
        assert!(!looks_binary(b"{\"a\": 1}\n"));
    }

    /// 内容一致时不产生任何行 —— 界面据此显示「没有差异」，而不是画一堆等号行。
    #[test]
    fn identical_text_yields_no_lines() {
        let d = diff_one("a.json", Some(b"x\ny\n".to_vec()), Some(b"x\ny\n".to_vec()));
        assert!(d.identical);
        assert_eq!(d.added, 0);
        assert_eq!(d.removed, 0);
        assert!(d.lines.is_empty());
    }

    /// 增删要分别计数，行号要对得上。
    #[test]
    fn counts_and_line_numbers_line_up() {
        let d = diff_one(
            "a.txt",
            Some(b"one\ntwo\nthree\n".to_vec()),
            Some(b"one\nTWO\nthree\n".to_vec()),
        );
        assert!(!d.identical);
        assert_eq!(d.added, 1);
        assert_eq!(d.removed, 1);
        let del = d.lines.iter().find(|l| l.kind == LineKind::Del).unwrap();
        let add = d.lines.iter().find(|l| l.kind == LineKind::Add).unwrap();
        assert_eq!(del.text, "two");
        assert_eq!(del.old_no, Some(2));
        assert_eq!(add.text, "TWO");
        assert_eq!(add.new_no, Some(2));
    }

    /// CRLF 不该被当成整file差异 —— 否则 Windows 上写过的配置每一行都是红绿。
    #[test]
    fn crlf_is_normalized_before_comparing() {
        let d = diff_one("a.txt", Some(b"x\r\ny\r\n".to_vec()), Some(b"x\ny\n".to_vec()));
        assert!(d.identical, "只有换行符不同却报出了差异");
    }

    /// 文件只在一边存在：另一边视为空，且 exists 标志要如实反映。
    #[test]
    fn missing_side_is_reported_not_crashed() {
        let created = diff_one("new.json", None, Some(b"{}\n".to_vec()));
        assert!(!created.base_exists);
        assert!(created.target_exists);
        assert_eq!(created.added, 1);

        let deleted = diff_one("gone.json", Some(b"{}\n".to_vec()), None);
        assert!(deleted.base_exists);
        assert!(!deleted.target_exists);
        assert_eq!(deleted.removed, 1);
    }

    /// 截断要计数完整、内容截断，并把 truncated 标出来 —— 悄悄少给几千行
    /// 会让人以为改动就这么点。
    #[test]
    fn truncation_keeps_counts_honest() {
        let big: String = (0..(MAX_LINES + 500)).map(|i| format!("line {i}\n")).collect();
        let d = diff_one("big.txt", Some(String::new().into_bytes()), Some(big.into_bytes()));
        assert!(d.truncated);
        assert_eq!(d.lines.len(), MAX_LINES);
        assert_eq!(d.added, MAX_LINES + 500, "计数被截断影响了");
    }
}
