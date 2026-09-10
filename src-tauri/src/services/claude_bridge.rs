//! 桥接表落到 Claude Code：6 个具名槽位 + `modelPicker` 列表。
//!
//! Claude Code 没有模型目录文件。它的 `/model` 菜单由三部分拼成（本机 2.1.258
//! 二进制里翻出来的，不是文档）：
//!
//!   1. 5 个 tier 环境变量：`ANTHROPIC_MODEL` + `ANTHROPIC_DEFAULT_{OPUS,SONNET,
//!      HAIKU,FABLE}_MODEL`。每个是一个具名位置，不是数组。
//!   2. `ANTHROPIC_CUSTOM_MODEL_OPTION`：菜单末尾多出来的一行。
//!   3. settings.json 顶层 `modelPicker.options[]`（2.1.243 起）：想放多少行放多少行，
//!      每行 `{ model, label?, description?, behavesAs? }`。用户级 settings.json 生效，
//!      项目级不认；多来源不合并，优先级最高的整体胜出。
//!
//! 还有一条「网关发现」（`CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY` + `/v1/models`），
//! 但它只保留 id 匹配 `/(claude|anthropic)/i` 的条目，grok / gpt / gemini 全被丢，
//! 对多 provider 没用，所以不走它。
//!
//! # 独占，不追加
//!
//! 这里和 `model_import` 那条路的语义**相反**：桥接表是这 6 个槽位的唯一主人 ——
//! 表里认领了就写，没认领就清掉。能这么做的前提是读表时先把磁盘上已有的槽位收进
//! 表里（`bridge::adopt_disk_slots`），否则用户在别处配好的 fable / haiku 会在第一次
//! 写入时被抹掉。`modelPicker` 里只动我们自己写的行（描述以 `ccLoad` 开头），用户或
//! 管理员手写的行原样保留。

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};

use crate::error::AppError;
use crate::services::bridge::BridgeEntry;
use crate::services::cli_backup::BackupStore;
use crate::services::cli_config::{write_claude_slot, write_claude_slot_note, write_claude_window_env};
use crate::services::cli_io::{object_at, read_json, write_pretty_json};
use crate::services::cli_types::{CliTarget, ConfigRoot};
use crate::services::context_floor::alias_key;
use crate::services::context_window::ContextPolicy;
use crate::services::model_caps::claude_capabilities;

/// 槽位名 → 环境变量键。顺序就是 `/model` 菜单里的顺序。
pub const SLOT_KEYS: [(&str, &str); 6] = [
    ("default", "ANTHROPIC_MODEL"),
    ("opus", "ANTHROPIC_DEFAULT_OPUS_MODEL"),
    ("sonnet", "ANTHROPIC_DEFAULT_SONNET_MODEL"),
    ("haiku", "ANTHROPIC_DEFAULT_HAIKU_MODEL"),
    ("fable", "ANTHROPIC_DEFAULT_FABLE_MODEL"),
    ("custom", "ANTHROPIC_CUSTOM_MODEL_OPTION"),
];

/// `modelPicker` 里我们写的行，描述都以它开头 —— 回读靠它认领，不靠比对整行。
pub const PICKER_MARK: &str = "ccLoad";

/// 从这个版本起 Claude Code 才认 `modelPicker`；更早的版本静默忽略这个键。
pub const PICKER_MIN_VERSION: &str = "2.1.243";

/// 不认识的 id 借哪个已知模型的客户端档（effort / thinking 开关、提示词档）。
/// 只给会推理的非 Claude 模型；Claude 自家的 id 它本来就认识。
const BEHAVES_AS_REASONING: &str = "claude-opus-5";

const SETTINGS: &str = ".claude/settings.json";

/// 磁盘上 6 个槽位现在写着什么（槽位名 → 模型名）。读不到文件就是空表。
pub fn slots_on_disk(root: &ConfigRoot) -> BTreeMap<String, String> {
    read_json(&root.join(SETTINGS))
        .map(|doc| slots_in(&doc))
        .unwrap_or_default()
}

/// 磁盘上 `modelPicker.options[]` 里**我们写的**那些行的模型名，按文件顺序。
///
/// 只数我们的：界面拿它和表里的列表对照「写进去了没有」，管理员手写的行不在
/// 表里，数进来就永远对不上。
pub fn picker_on_disk(root: &ConfigRoot) -> Vec<String> {
    read_json(&root.join(SETTINGS))
        .map(|doc| {
            doc.pointer("/modelPicker/options")
                .and_then(Value::as_array)
                .map(|rows| {
                    rows.iter()
                        .filter(|r| is_ours(r))
                        .filter_map(model_of)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default()
        })
        .unwrap_or_default()
}

fn slots_in(doc: &Value) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (slot, key) in SLOT_KEYS {
        if let Some(v) = doc
            .pointer(&format!("/env/{key}"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            out.insert(slot.to_string(), v.to_string());
        }
    }
    out
}

/// 列表里**所有**行的模型名，包括别人写的。测试用它核对合并结果。
#[cfg(test)]
fn picker_in(doc: &Value) -> Vec<String> {
    doc.pointer("/modelPicker/options")
        .and_then(Value::as_array)
        .map(|rows| rows.iter().filter_map(model_of).map(str::to_string).collect())
        .unwrap_or_default()
}

fn model_of(row: &Value) -> Option<&str> {
    row.get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn is_ours(row: &Value) -> bool {
    row.get("description")
        .and_then(Value::as_str)
        .is_some_and(|d| d.trim_start().starts_with(PICKER_MARK))
}

/// 一次写入动了什么。文案在命令层拼，这里只给数。
#[derive(Debug)]
pub struct ClaudeWrite {
    pub written: Vec<String>,
    pub backup_id: String,
    /// 写了几个槽位。
    pub slots: usize,
    /// 清掉了哪几个槽位（磁盘上原来有、表里没人认领）。
    pub cleared: Vec<String>,
    /// `modelPicker` 里我们的行数。
    pub picker: usize,
}

/// `/model` 菜单副标题：改了名的行写落点，同名的留给 Claude Code 自己的默认文案
/// （「Custom Opus model (1M context)」那种，带着它从后缀推出来的窗口提示）。
fn note_for(e: &BridgeEntry) -> Option<String> {
    e.renames()
        .then(|| format!("{PICKER_MARK} → {}", e.upstream_alias()))
}

/// 能力按**落点**判断，不按出口名：`ccload-fast` 这个名字什么都推不出来，它背后的
/// `grok-4.6` 才决定能不能吃 effort / thinking。
fn capabilities_of(e: &BridgeEntry) -> Option<&'static str> {
    claude_capabilities(e.upstream_alias())
}

fn behaves_as(e: &BridgeEntry) -> Option<&'static str> {
    // 厂商前缀不算名字的一部分：`anthropic/claude-sonnet-5` 也是 Claude 自家的。
    let key = alias_key(&e.alias);
    let family = key.rsplit('/').next().unwrap_or(&key);
    (!family.starts_with("claude") && capabilities_of(e).is_some()).then_some(BEHAVES_AS_REASONING)
}

fn picker_row(e: &BridgeEntry) -> Value {
    let alias = e.alias.trim();
    let mut row = Map::new();
    row.insert("model".into(), Value::String(alias.into()));
    row.insert("label".into(), Value::String(alias.into()));
    row.insert(
        "description".into(),
        Value::String(note_for(e).unwrap_or_else(|| PICKER_MARK.to_string())),
    );
    if let Some(b) = behaves_as(e) {
        row.insert("behavesAs".into(), Value::String(b.into()));
    }
    Value::Object(row)
}

/// 把我们的行合进 `modelPicker.options`：
///
///   * 别人的行（没有 `ccLoad` 记号、模型名也不在我们这批里）原样留在原位；
///   * 我们上次写的行整批换掉 —— 表里删了的就消失，不会越攒越多；
///   * 一行都没有时把整个键删掉，别留一个空壳让用户以为我们还在管。
///
/// `replaceBuiltInOptions` 之类其它键不碰：那是用户 / 管理员的选择。
fn merge_picker(doc: &mut Value, rows: &[&BridgeEntry]) -> Result<(), AppError> {
    let ours: Vec<Value> = rows.iter().map(|e| picker_row(e)).collect();
    let our_models: BTreeSet<String> = rows.iter().map(|e| alias_key(&e.alias)).collect();
    let top = doc
        .as_object_mut()
        .ok_or_else(|| AppError::Config("settings.json 顶层不是对象".into()))?;

    let (mut picker, existing): (Map<String, Value>, Vec<Value>) = match top.get("modelPicker") {
        Some(Value::Object(o)) => (
            o.clone(),
            o.get("options").and_then(Value::as_array).cloned().unwrap_or_default(),
        ),
        // 形状不对的值 Claude Code 自己也会忽略。我们没东西要写时不去碰它 ——
        // 那毕竟是用户写的；要写时它反正没在生效，换掉。
        Some(_) if ours.is_empty() => return Ok(()),
        _ => (Map::new(), Vec::new()),
    };

    let mut options: Vec<Value> = existing
        .into_iter()
        .filter(|row| !is_ours(row) && !model_of(row).is_some_and(|m| our_models.contains(&alias_key(m))))
        .collect();
    options.extend(ours);

    if options.is_empty() {
        top.remove("modelPicker");
    } else {
        picker.insert("options".into(), Value::Array(options));
        top.insert("modelPicker".into(), Value::Object(picker));
    }
    Ok(())
}

/// 按桥接表把 Claude Code 那一份写盘。`entries` 是整张表，这里只取勾了 Claude Code
/// 的行。先快照，再原子写。
pub fn write(
    root: &ConfigRoot,
    entries: &[BridgeEntry],
    policy: &ContextPolicy,
    stamp: &str,
    backups: &BackupStore,
) -> Result<ClaudeWrite, AppError> {
    let mine: Vec<&BridgeEntry> = entries
        .iter()
        .filter(|e| e.targets.contains(&CliTarget::ClaudeCode) && !e.alias.trim().is_empty())
        .collect();
    if mine.is_empty() {
        return Err(AppError::Config("这一家一条都没勾，配置未改动".into()));
    }

    // 快照之前把冲突判完：一个槽位两条记录是 `validate` 在保存时就拦的，这里再
    // 拦一次是给直接改过 bridge.json 的人兜底，别让一次点击留下一份空快照。
    let mut by_slot: BTreeMap<&str, &BridgeEntry> = BTreeMap::new();
    for e in &mine {
        for s in e.slots() {
            if let Some(prev) = by_slot.insert(s, e) {
                return Err(AppError::Config(format!(
                    "Claude Code 的 {s} 槽位只能绑一个模型，但「{}」和「{}」都选了它",
                    prev.alias, e.alias
                )));
            }
        }
    }
    let picker_rows: Vec<&BridgeEntry> = mine.iter().copied().filter(|e| e.in_claude_picker()).collect();

    let snapshot = backups.snapshot(root, CliTarget::ClaudeCode, stamp, "bridge")?;
    let path = root.join(SETTINGS);
    let mut doc = read_json(&path)?;
    let mut cleared = Vec::new();
    let mut slots = 0;
    {
        let env = object_at(&mut doc, "env")?;
        for (slot, key) in SLOT_KEYS {
            let caps_key = format!("{key}_SUPPORTED_CAPABILITIES");
            match by_slot.get(slot) {
                Some(e) => {
                    slots += 1;
                    write_claude_slot(env, key, Some(e.alias.trim()));
                    write_claude_slot_note(env, key, note_for(e).as_deref());
                    // 主模型没有这个伴生键，靠 CLAUDE_CODE_ALWAYS_ENABLE_EFFORT。
                    // 不会推理的落点要把上一任留下的 `effort,thinking` 收走，否则
                    // `/effort` 会对着一个吃不下它的模型出现。
                    if key != "ANTHROPIC_MODEL" {
                        match capabilities_of(e) {
                            Some(c) => {
                                env.insert(caps_key, Value::String(c.into()));
                            }
                            None => {
                                env.remove(&caps_key);
                            }
                        }
                    }
                }
                None => {
                    if env.contains_key(key) {
                        cleared.push(slot.to_string());
                    }
                    write_claude_slot(env, key, None);
                    write_claude_slot_note(env, key, None);
                    env.remove(&caps_key);
                }
            }
        }
        // 走 ccLoad 时模型 id 不是 Anthropic 官方那几个：不认就不发 effort，
        // 窗口也会被按「未知模型」夹回去。这两个是官方给网关别名准备的开关。
        env.insert("CLAUDE_CODE_ALWAYS_ENABLE_EFFORT".into(), Value::String("1".into()));
        env.insert(
            "CLAUDE_CODE_DISABLE_UNKNOWN_MODEL_WINDOW_ENFORCEMENT".into(),
            Value::String("1".into()),
        );
        // 上下文上限是全局一个键，只能跟着主模型那一行走。
        if let Some(main) = by_slot.get("default") {
            let w = main.window(policy);
            if w > 0 {
                write_claude_window_env(env, w, Some(main.percent(policy)));
            }
        }
    }
    merge_picker(&mut doc, &picker_rows)?;
    write_pretty_json(&path, &doc)?;

    Ok(ClaudeWrite {
        written: vec![path.display().to_string()],
        backup_id: snapshot.id,
        slots,
        cleared,
        picker: picker_rows.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(alias: &str, target: &str, slots: &[&str]) -> BridgeEntry {
        BridgeEntry {
            alias: alias.into(),
            target: target.into(),
            context_window: 0,
            compact_percent: 0,
            targets: BTreeSet::from([CliTarget::ClaudeCode]),
            tiers: slots.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn sandbox(dir: &tempfile::TempDir, settings: &str) -> (ConfigRoot, BackupStore, std::path::PathBuf) {
        let root = ConfigRoot::sandbox(dir.path().to_path_buf());
        let path = root.join(SETTINGS);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, settings).unwrap();
        (root, BackupStore::new(dir.path().join("bk")), path)
    }

    fn read(path: &std::path::Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn env_str<'a>(doc: &'a Value, key: &str) -> Option<&'a str> {
        doc.pointer(&format!("/env/{key}")).and_then(Value::as_str)
    }

    const BASE: &str = r#"{"env":{"ANTHROPIC_BASE_URL":"http://x"}}"#;

    /// 主模型和 opus 同一个名字：两个键都写，标签跟着 id 走。
    #[test]
    fn one_row_in_two_slots_writes_both_keys() {
        let dir = tempfile::tempdir().unwrap();
        let (root, bk, path) = sandbox(&dir, BASE);
        let r = write(
            &root,
            &[row("claude-opus-5", "claude-opus-5", &["default", "opus"])],
            &ContextPolicy::default(),
            "s1",
            &bk,
        )
        .unwrap();
        assert_eq!(r.slots, 2);
        let doc = read(&path);
        assert_eq!(env_str(&doc, "ANTHROPIC_MODEL"), Some("claude-opus-5"));
        assert_eq!(env_str(&doc, "ANTHROPIC_DEFAULT_OPUS_MODEL"), Some("claude-opus-5"));
        assert_eq!(env_str(&doc, "ANTHROPIC_DEFAULT_OPUS_MODEL_NAME"), Some("claude-opus-5"));
        // 同名的行不写副标题，留 Claude Code 自己的「Custom Opus model」。
        assert!(env_str(&doc, "ANTHROPIC_DEFAULT_OPUS_MODEL_DESCRIPTION").is_none());
        // 快照先于写入。
        assert_eq!(bk.list(Some(CliTarget::ClaudeCode)).unwrap().len(), 1);
    }

    /// 表里空着的槽位在磁盘上被清掉，连同标签、副标题、能力键一起；主模型跟着走。
    #[test]
    fn an_unclaimed_slot_is_cleared_with_all_its_companions() {
        let dir = tempfile::tempdir().unwrap();
        let (root, bk, path) = sandbox(
            &dir,
            r#"{"env":{"ANTHROPIC_BASE_URL":"http://x",
                "ANTHROPIC_DEFAULT_HAIKU_MODEL":"old-haiku",
                "ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME":"old-haiku",
                "ANTHROPIC_DEFAULT_HAIKU_MODEL_DESCRIPTION":"ccLoad → x",
                "ANTHROPIC_DEFAULT_HAIKU_MODEL_SUPPORTED_CAPABILITIES":"effort,thinking",
                "ANTHROPIC_CUSTOM_MODEL_OPTION":"old-custom",
                "ANTHROPIC_DEFAULT_OPUS_MODEL":"keep-me"}}"#,
        );
        let r = write(
            &root,
            &[row("keep-me", "keep-me", &["opus"])],
            &ContextPolicy::default(),
            "s1",
            &bk,
        )
        .unwrap();
        let mut cleared = r.cleared.clone();
        cleared.sort();
        assert_eq!(cleared, vec!["custom", "haiku"]);
        let doc = read(&path);
        for k in [
            "ANTHROPIC_DEFAULT_HAIKU_MODEL",
            "ANTHROPIC_DEFAULT_HAIKU_MODEL_NAME",
            "ANTHROPIC_DEFAULT_HAIKU_MODEL_DESCRIPTION",
            "ANTHROPIC_DEFAULT_HAIKU_MODEL_SUPPORTED_CAPABILITIES",
            "ANTHROPIC_CUSTOM_MODEL_OPTION",
        ] {
            assert!(env_str(&doc, k).is_none(), "{k} 该被清掉");
        }
        assert_eq!(env_str(&doc, "ANTHROPIC_DEFAULT_OPUS_MODEL"), Some("keep-me"));
        // 无关的键一个都不动。
        assert_eq!(env_str(&doc, "ANTHROPIC_BASE_URL"), Some("http://x"));
    }

    /// 改了名的行把落点写进副标题；自定义项也一样，而且会覆盖磁盘上的旧值。
    #[test]
    fn renaming_rows_get_a_landing_note_and_custom_is_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let (root, bk, path) = sandbox(
            &dir,
            r#"{"env":{"ANTHROPIC_BASE_URL":"http://x","ANTHROPIC_CUSTOM_MODEL_OPTION":"someone-else"}}"#,
        );
        write(
            &root,
            &[
                row("ccload-fast", "grok-4.6", &["haiku"]),
                row("ccload-custom", "glm-5.3-flash", &["custom"]),
            ],
            &ContextPolicy::default(),
            "s1",
            &bk,
        )
        .unwrap();
        let doc = read(&path);
        assert_eq!(env_str(&doc, "ANTHROPIC_DEFAULT_HAIKU_MODEL"), Some("ccload-fast"));
        assert_eq!(
            env_str(&doc, "ANTHROPIC_DEFAULT_HAIKU_MODEL_DESCRIPTION"),
            Some("ccLoad → grok-4.6")
        );
        assert_eq!(env_str(&doc, "ANTHROPIC_CUSTOM_MODEL_OPTION"), Some("ccload-custom"));
        assert_eq!(
            env_str(&doc, "ANTHROPIC_CUSTOM_MODEL_OPTION_DESCRIPTION"),
            Some("ccLoad → glm-5.3-flash")
        );
        // 能力按落点判断：grok-4.6 会推理。
        assert_eq!(
            env_str(&doc, "ANTHROPIC_DEFAULT_HAIKU_MODEL_SUPPORTED_CAPABILITIES"),
            Some("effort,thinking")
        );
    }

    /// 没占槽位的行全部进 modelPicker；别人的行留着，我们上次写的整批换掉，
    /// 一行都没有时整个键消失。
    #[test]
    fn unslotted_rows_land_in_model_picker_and_foreign_rows_survive() {
        let dir = tempfile::tempdir().unwrap();
        let (root, bk, path) = sandbox(
            &dir,
            r#"{"env":{"ANTHROPIC_BASE_URL":"http://x"},
                "modelPicker":{"replaceBuiltInOptions":false,"options":[
                  {"model":"admin-model","label":"Admin","description":"Managed"},
                  {"model":"stale-ours","description":"ccLoad → gone"},
                  {"model":"grok-4.6","description":"hand written, same model as ours"}
                ]}}"#,
        );
        let rows = [
            row("claude-opus-5", "claude-opus-5", &["opus"]),
            row("grok-4.6", "grok-4.6", &[]),
            row("ccload-fast", "grok-4.6", &[]),
            row("anthropic/claude-sonnet-5", "claude-sonnet-5", &[]),
            row("tts-1", "tts-1", &[]),
        ];
        let r = write(&root, &rows, &ContextPolicy::default(), "s1", &bk).unwrap();
        assert_eq!(r.picker, 4);
        let doc = read(&path);
        let options = doc.pointer("/modelPicker/options").unwrap().as_array().unwrap();
        let models: Vec<&str> = options.iter().map(|o| o["model"].as_str().unwrap()).collect();
        // 管理员的行留在最前面；stale-ours 消失；同名的手写行被我们的那行顶掉。
        assert_eq!(
            models,
            vec!["admin-model", "grok-4.6", "ccload-fast", "anthropic/claude-sonnet-5", "tts-1"]
        );
        assert_eq!(options[0]["label"], "Admin");
        // 我们的行：label 是出口名，描述带记号，改名的写落点。
        assert_eq!(options[1]["description"], "ccLoad");
        assert_eq!(options[2]["description"], "ccLoad → grok-4.6");
        // behavesAs 只给会推理的非 Claude 模型。
        assert_eq!(options[1]["behavesAs"], "claude-opus-5");
        assert_eq!(options[2]["behavesAs"], "claude-opus-5");
        assert!(options[3].get("behavesAs").is_none(), "Claude 自家的 id 不用借档");
        assert!(options[4].get("behavesAs").is_none(), "tts 不推理");
        // 其它键原样。
        assert_eq!(doc.pointer("/modelPicker/replaceBuiltInOptions"), Some(&Value::Bool(false)));

        // 第二次：没占槽位的行全删了 → 只剩管理员那一行。
        write(&root, &rows[..1], &ContextPolicy::default(), "s2", &bk).unwrap();
        let doc = read(&path);
        let models: Vec<String> = picker_in(&doc);
        assert_eq!(models, vec!["admin-model"]);

        // 管理员那行也没有的话整个键消失。
        let mut stripped = read(&path);
        stripped.as_object_mut().unwrap().remove("modelPicker");
        std::fs::write(&path, stripped.to_string()).unwrap();
        write(&root, &rows[..1], &ContextPolicy::default(), "s3", &bk).unwrap();
        assert!(read(&path).get("modelPicker").is_none());
    }

    /// 上下文上限只跟主模型那一行走；没绑主模型就不碰全局键。
    #[test]
    fn the_window_env_follows_the_default_slot_row_only() {
        let dir = tempfile::tempdir().unwrap();
        let (root, bk, path) = sandbox(
            &dir,
            r#"{"env":{"ANTHROPIC_BASE_URL":"http://x","CLAUDE_CODE_MAX_CONTEXT_TOKENS":"777"}}"#,
        );
        write(
            &root,
            &[row("grok-4.6", "grok-4.6", &["opus"])],
            &ContextPolicy::default(),
            "s1",
            &bk,
        )
        .unwrap();
        assert_eq!(env_str(&read(&path), "CLAUDE_CODE_MAX_CONTEXT_TOKENS"), Some("777"));

        let mut main = row("grok-4.6", "grok-4.6", &["default"]);
        main.context_window = 300_000;
        write(&root, &[main], &ContextPolicy::default(), "s2", &bk).unwrap();
        assert_eq!(env_str(&read(&path), "CLAUDE_CODE_MAX_CONTEXT_TOKENS"), Some("300000"));
    }

    /// 回读：磁盘上的槽位，以及 picker 里我们写的行（管理员的不算）。
    #[test]
    fn disk_readers_see_slots_and_our_picker_rows() {
        let dir = tempfile::tempdir().unwrap();
        let (root, _bk, _path) = sandbox(
            &dir,
            r#"{"env":{"ANTHROPIC_MODEL":" claude-opus-5[1M] ","ANTHROPIC_DEFAULT_HAIKU_MODEL":""},
                "modelPicker":{"options":[
                  {"model":"a","description":"ccLoad"},
                  {"label":"no model","description":"ccLoad"},
                  {"model":"admin","description":"Managed"},
                  {"model":" b ","description":"ccLoad → x"}]}}"#,
        );
        let slots = slots_on_disk(&root);
        assert_eq!(slots.get("default").map(String::as_str), Some("claude-opus-5[1M]"));
        assert!(!slots.contains_key("haiku"), "空串等于没有");
        assert_eq!(picker_on_disk(&root), vec!["a", "b"]);
        assert_eq!(picker_in(&read(&root.join(SETTINGS))), vec!["a", "admin", "b"]);
        // 没有文件就是空。
        let empty = ConfigRoot::sandbox(dir.path().join("nothing"));
        assert!(slots_on_disk(&empty).is_empty());
        assert!(picker_on_disk(&empty).is_empty());
    }
}
