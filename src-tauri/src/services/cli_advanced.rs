//! Reading and writing a CLI's config file as raw text, plus the structured
//! knobs (model tiers, timeouts) that a takeover writes.
//!
//! Two levels of control, because they fail differently:
//!   * `TakeoverOptions` — typed fields we know how to merge safely. A bad value
//!     is rejected before anything is written.
//!   * raw text read/write — the escape hatch. ccLoad and the CLIs both add
//!     fields faster than any form can track (cc-switch pins things like
//!     CLAUDE_CODE_AUTO_COMPACT_WINDOW that deliberately have no form field),
//!     so the user must be able to edit the document directly.
//!
//! Raw writes are validated by parsing before the file is replaced: a JSON
//! target must parse as JSON, a TOML target as TOML. Saving a broken config is
//! how you brick a CLI, and the editor is the one place a typo is likely.

use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::services::cli_io::write_atomic;
use crate::services::cli_types::{CliTarget, ConfigRoot};

/// Config formats we can validate. Mirrors the file extension, not the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConfigFormat {
    Json,
    Toml,
}

impl ConfigFormat {
    fn of(rel: &str) -> Self {
        if rel.ends_with(".toml") {
            Self::Toml
        } else {
            Self::Json
        }
    }

    fn validate(self, body: &str) -> Result<(), AppError> {
        // Empty is legal: it means "the CLI falls back to its defaults".
        if body.trim().is_empty() {
            return Ok(());
        }
        match self {
            Self::Json => serde_json::from_str::<serde_json::Value>(body)
                .map(|_| ())
                .map_err(|e| AppError::Config(format!("不是合法 JSON：{e}"))),
            Self::Toml => body
                .parse::<toml_edit::DocumentMut>()
                .map(|_| ())
                .map_err(|e| AppError::Config(format!("不是合法 TOML：{e}"))),
        }
    }
}

/// One editable config file belonging to a CLI.
#[derive(Debug, Serialize)]
pub struct ConfigFileView {
    /// Path relative to the config root, e.g. `.claude/settings.json`.
    pub rel: String,
    /// Absolute path, shown in the UI so the user knows what they are editing.
    pub path: String,
    pub format: ConfigFormat,
    pub exists: bool,
    /// File contents, or empty string when the file does not exist yet.
    pub body: String,
}

/// Every config file of a target, in the order the CLI reads them.
pub fn read_files(root: &ConfigRoot, target: CliTarget) -> Result<Vec<ConfigFileView>, AppError> {
    let mut out = Vec::new();
    for rel in target.relative_paths() {
        let path = root.join(rel);
        let exists = path.exists();
        let body = if exists {
            std::fs::read_to_string(&path)?
        } else {
            String::new()
        };
        out.push(ConfigFileView {
            rel: (*rel).to_string(),
            path: path.display().to_string(),
            format: ConfigFormat::of(rel),
            exists,
            body,
        });
    }
    Ok(out)
}

/// Replace one config file wholesale. Snapshots first so the edit is undoable.
///
/// `rel` must be one of the target's known paths — otherwise a renderer bug
/// (or a crafted call) could write anywhere under the home directory.
pub fn write_file(
    root: &ConfigRoot,
    target: CliTarget,
    rel: &str,
    body: &str,
    stamp: &str,
    backups: &crate::services::cli_backup::BackupStore,
) -> Result<String, AppError> {
    if !target.relative_paths().contains(&rel) {
        return Err(AppError::Config(format!(
            "{rel} 不属于 {}",
            target.label()
        )));
    }
    ConfigFormat::of(rel).validate(body)?;

    let snapshot = backups.snapshot(root, target, stamp, "manual-edit")?;
    let path = root.join(rel);
    // Keep a trailing newline: these files are diffed and hand-edited.
    let body = if body.ends_with('\n') || body.is_empty() {
        body.to_string()
    } else {
        format!("{body}\n")
    };
    write_atomic(&path, &body)?;
    Ok(snapshot.id)
}

/// Optional knobs applied on top of endpoint+token during a takeover.
///
/// All fields are optional and empty values are skipped rather than written as
/// empty strings — an empty `ANTHROPIC_MODEL` is not the same as unset, and the
/// CLI treats the former as a model literally named "".
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TakeoverOptions {
    /// Claude Code: the default model, and the per-tier overrides.
    pub anthropic_model: Option<String>,
    pub sonnet_model: Option<String>,
    pub opus_model: Option<String>,
    pub haiku_model: Option<String>,
    /// Claude Code 的**可用性** fallback 链：主力过载 / 不可用时按顺序换人。
    /// 落在 settings.json 顶层的 `fallbackModel` 数组（不是 env）。Claude Code
    /// 去重后最多取 3 个，多余的忽略；`"default"` 展开成默认模型。
    ///
    /// 注意它治不了「总是跳到 claude-opus-4-8」那个症状 —— 那是**内容分类器**
    /// fallback，走的是另一条路，见 `MODEL_TIER_NOTE`。
    pub fallback_models: Option<Vec<String>>,
    /// `switchModelsOnFlag`：请求被安全分类器标记时，`false` 表示先停下来问一句
    /// 而不是自动换模型。第三方供应商上「自动换」多半换到一个不存在的模型名，
    /// 停下来问反而是能用的那个选项。
    pub switch_models_on_flag: Option<bool>,
    /// Free-form extra env entries merged into Claude/Gemini `env`.
    /// Also how the timeout/retry knobs get set — see `RESILIENT_ENV`.
    pub extra_env: Option<std::collections::BTreeMap<String, String>>,
    /// Codex: model selection and reasoning depth.
    pub codex_model: Option<String>,
    pub codex_reasoning_effort: Option<String>,
    pub codex_context_window: Option<i64>,
    /// Grok Build: the kernel alias `[model.ccload]` should send. Empty = keep
    /// inheriting whatever the user last picked in `/model`.
    pub grok_model: Option<String>,
    /// Gemini CLI: `model.name` + `GEMINI_MODEL`. It has no catalog, only this
    /// one slot.
    pub gemini_model: Option<String>,
    /// OpenCode: top-level `model` (`ccload/<alias>`). Empty = don't touch the
    /// user's current pick.
    pub opencode_model: Option<String>,
    /// 这次接管要写进 CLI 的上下文窗口，**已经解析完**的最终值。
    ///
    /// 由 `commands::cli::cli_apply` 按用户的总控策略（`ContextPolicy`）算好再
    /// 传进来 —— 策略要看设置、要看模型链最窄的那一跳，这些在纯函数里都拿不到。
    /// `None` = 不写，各 CLI 保留现状。
    pub context_tokens: Option<i64>,
}

/// Claude Code 有**两种**换模型的机制，配错格子的人都在同一个坑里：
///
/// 1. **可用性 fallback** —— 主力过载 / 不可用 / 服务端错误时换人。归
///    `fallbackModel` 管（settings.json 顶层数组，最多 3 个）。
/// 2. **内容分类器 fallback** —— Fable 5 / Opus 5 的请求被安全分类器标记时
///    换人。它**根本不看** `fallbackModel`：Fable 5 被 cybersecurity 标记就跳
///    Opus 4.8，被 biology 标记就跳 Opus 5，写死在 Claude Code 里。
///
/// 第三方供应商（也就是走 ccLoad 的所有人）上没有 `claude-opus-4-8` 这个模型
/// 名，于是第 2 种每次都跳进一个不存在的模型 —— 这就是「总是自动跳到
/// claude-opus-4-8」的来历。唯一的改法是把 `ANTHROPIC_DEFAULT_OPUS_MODEL`
/// 钉成你自己有的模型：官方文档明确说，设了它之后**所有**有 fallback 的分类
/// 都改跑这个钉住的模型。想干脆不自动换，就把 `switchModelsOnFlag` 关掉。
pub const MODEL_TIER_NOTE: &str = "\
Claude Code 的 fallback 分两种：过载/不可用走 fallbackModel 链；\
被安全分类器标记走的是写死的 Opus 4.8 / Opus 5，只认 ANTHROPIC_DEFAULT_OPUS_MODEL。";

/// Claude Code 去重后最多认 3 个 fallback，多余的直接忽略。写之前就截断，
/// 免得用户在界面上排了 5 个、以为后两个也在生效。
pub const MAX_FALLBACK_MODELS: usize = 3;

/// 规范化 fallback 链：去空白、丢空串、去重、截断到 3 个。
///
/// 去重必须在截断**之前**：Claude Code 就是这个顺序，先截断会让
/// `[a, a, b]` 只剩 `[a]` 而不是 `[a, b]`。
pub fn normalize_fallback_models(raw: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for m in raw {
        let m = m.trim();
        if m.is_empty() || out.iter().any(|seen| seen == m) {
            continue;
        }
        out.push(m.to_string());
    }
    out.truncate(MAX_FALLBACK_MODELS);
    out
}

/// cc-switch's "resilient preset": long timeouts and retries for slow upstreams.
/// Offered as a one-click default because these three together are what stop a
/// long agent turn from being killed mid-stream.
pub const RESILIENT_ENV: &[(&str, &str)] = &[
    ("API_TIMEOUT_MS", "1200000"),
    ("API_FORCE_IDLE_TIMEOUT", "0"),
    ("CLAUDE_CODE_RETRY_WATCHDOG", "1"),
];

/// Official catalogs. Each JSON is scraped from that CLI's own docs/schema —
/// the form lists every scalar the official surface documents, not a short
/// "common" subset. Tables/arrays stay in the raw editor (they cannot
/// round-trip through a single text input).
const CLAUDE_ENV_CATALOG_JSON: &str = include_str!("../../data/claude-code-env.json");
const CODEX_CATALOG_JSON: &str = include_str!("../../data/codex-config.json");
const GEMINI_CATALOG_JSON: &str = include_str!("../../data/gemini-settings.json");
const GROK_CATALOG_JSON: &str = include_str!("../../data/grok-config.json");
const OPENCODE_CATALOG_JSON: &str = include_str!("../../data/opencode-config.json");

/// Takeover already owns these; they must not show up as "advanced knobs"
/// or a 复原 click would fight the endpoint/token we just wrote.
const TAKEOVER_OWNED_ENV: &[&str] = &[
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_BASE_URL",
    "ANTHROPIC_API_KEY",
];

/// Hosting identity — Claude Code ignores these in settings.json `env`.
const HOST_OWNED_ENV: &[&str] = &[
    "CLAUDE_CODE_REMOTE",
    "CLAUDE_CODE_ACCOUNT_UUID",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDECODE",
    "CLAUDE_PID",
];

const CODEX_OWNED: &[&str] = &["model_provider", "model_providers"];
const GEMINI_OWNED: &[&str] = &[
    "GEMINI_API_KEY",
    "GOOGLE_GEMINI_BASE_URL",
    "GOOGLE_API_KEY",
];
const GROK_OWNED: &[&str] = &["models.default"];
const OPENCODE_OWNED: &[&str] = &["provider"];

/// Codex fields with dedicated TakeoverOptions slots. extra_env must not
/// overwrite them a second time.
const CODEX_DEDICATED: &[&str] = &[
    "model",
    "model_reasoning_effort",
    "model_context_window",
];

/// A top-level `model = "…"` string would replace the `[model.*]` table and
/// take over would look like it never wrote an endpoint.
const GROK_DEDICATED: &[&str] = &["model"];
const GEMINI_DEDICATED: &[&str] = &["model.name", "GEMINI_MODEL"];
const OPENCODE_DEDICATED: &[&str] = &["model"];

fn dedicated_keys(target: CliTarget) -> &'static [&'static str] {
    match target {
        CliTarget::Codex => CODEX_DEDICATED,
        CliTarget::GrokBuild => GROK_DEDICATED,
        CliTarget::GeminiCli => GEMINI_DEDICATED,
        CliTarget::OpenCode => OPENCODE_DEDICATED,
        _ => &[],
    }
}

#[derive(Deserialize)]
struct CatalogFile {
    keys: Vec<CatalogEntry>,
}

#[derive(Deserialize, Clone)]
struct CatalogEntry {
    key: String,
    description: String,
    #[serde(default)]
    default: String,
}

fn parse_catalog(raw: &'static str, name: &str) -> Vec<CatalogEntry> {
    serde_json::from_str::<CatalogFile>(raw)
        .unwrap_or_else(|e| panic!("{name} must parse: {e}"))
        .keys
}

fn catalog_for(target: CliTarget) -> &'static [CatalogEntry] {
    use std::sync::OnceLock;
    fn load(
        lock: &'static OnceLock<Vec<CatalogEntry>>,
        raw: &'static str,
        name: &str,
    ) -> &'static [CatalogEntry] {
        lock.get_or_init(|| parse_catalog(raw, name)).as_slice()
    }
    match target {
        CliTarget::ClaudeCode => {
            static C: OnceLock<Vec<CatalogEntry>> = OnceLock::new();
            load(&C, CLAUDE_ENV_CATALOG_JSON, "claude-code-env.json")
        }
        CliTarget::Codex => {
            static C: OnceLock<Vec<CatalogEntry>> = OnceLock::new();
            load(&C, CODEX_CATALOG_JSON, "codex-config.json")
        }
        CliTarget::GeminiCli => {
            static C: OnceLock<Vec<CatalogEntry>> = OnceLock::new();
            load(&C, GEMINI_CATALOG_JSON, "gemini-settings.json")
        }
        CliTarget::GrokBuild => {
            static C: OnceLock<Vec<CatalogEntry>> = OnceLock::new();
            load(&C, GROK_CATALOG_JSON, "grok-config.json")
        }
        CliTarget::OpenCode => {
            static C: OnceLock<Vec<CatalogEntry>> = OnceLock::new();
            load(&C, OPENCODE_CATALOG_JSON, "opencode-config.json")
        }
    }
}

fn owned_keys(target: CliTarget) -> &'static [&'static str] {
    match target {
        CliTarget::ClaudeCode => TAKEOVER_OWNED_ENV,
        CliTarget::Codex => CODEX_OWNED,
        CliTarget::GeminiCli => GEMINI_OWNED,
        CliTarget::GrokBuild => GROK_OWNED,
        CliTarget::OpenCode => OPENCODE_OWNED,
    }
}

fn is_owned(key: &str, owned: &[&str]) -> bool {
    owned
        .iter()
        .any(|o| key == *o || key.starts_with(&format!("{o}.")))
}

/// Keys the user must not be able to write through the advanced form: the ones
/// takeover itself owns, plus the ones Claude Code's host process injects.
///
/// The form and the writer both call this. Keeping two lists is exactly how
/// `CLAUDE_PID` ended up typeable into `settings.json` — hidden from the form,
/// happily written by the takeover.
pub(crate) fn blocked_for_user(target: CliTarget, key: &str) -> bool {
    is_owned(key, owned_keys(target))
        || (target == CliTarget::ClaudeCode && HOST_OWNED_ENV.contains(&key))
}

pub(crate) fn is_env_key(key: &str) -> bool {
    !key.contains('.')
        && key
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        && key.chars().any(|c| c.is_ascii_alphabetic())
}

enum Scalar {
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
}

fn parse_scalar(raw: &str) -> Scalar {
    let s = raw.trim();
    if s.eq_ignore_ascii_case("true") {
        return Scalar::Bool(true);
    }
    if s.eq_ignore_ascii_case("false") {
        return Scalar::Bool(false);
    }
    if let Ok(i) = s.parse::<i64>() {
        return Scalar::Int(i);
    }
    if s.contains('.') {
        if let Ok(f) = s.parse::<f64>() {
            if f.is_finite() {
                return Scalar::Float(f);
            }
        }
    }
    Scalar::Str(raw.to_string())
}

impl Scalar {
    fn to_json(&self) -> serde_json::Value {
        match self {
            Scalar::Bool(b) => serde_json::Value::Bool(*b),
            Scalar::Int(i) => serde_json::json!(i),
            Scalar::Float(f) => serde_json::json!(f),
            Scalar::Str(s) => serde_json::Value::String(s.clone()),
        }
    }

    fn to_toml(&self) -> toml_edit::Value {
        match self {
            Scalar::Bool(b) => toml_edit::Value::from(*b),
            Scalar::Int(i) => toml_edit::Value::from(*i),
            Scalar::Float(f) => toml_edit::Value::from(*f),
            Scalar::Str(s) => toml_edit::Value::from(s.as_str()),
        }
    }
}

fn json_as_text(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn get_json_path(root: &serde_json::Value, path: &str) -> Option<String> {
    let mut cur = root;
    for p in path.split('.') {
        cur = cur.get(p)?;
    }
    json_as_text(cur)
}

fn set_json_path(
    root: &mut serde_json::Value,
    path: &str,
    val: serde_json::Value,
) -> Result<(), AppError> {
    if !root.is_object() {
        *root = serde_json::Value::Object(serde_json::Map::new());
    }
    let parts: Vec<&str> = path.split('.').collect();
    let mut cur = root;
    for (i, p) in parts.iter().enumerate() {
        if i + 1 == parts.len() {
            cur.as_object_mut()
                .ok_or_else(|| AppError::Config(format!("{path} 不是对象，拒绝写入")))?
                .insert((*p).into(), val);
            return Ok(());
        }
        let obj = cur
            .as_object_mut()
            .ok_or_else(|| AppError::Config(format!("{path}: {p} 不是对象，拒绝写入")))?;
        // Only a missing (or null) parent may be created. Replacing an existing
        // scalar/array with an empty object would silently drop what the user
        // had there — the same case the TOML side refuses.
        match obj.get(*p) {
            Some(v) if v.is_object() => {}
            None | Some(serde_json::Value::Null) => {
                obj.insert((*p).into(), serde_json::Value::Object(serde_json::Map::new()));
            }
            Some(_) => {
                return Err(AppError::Config(format!(
                    "{path}: {p} 不是对象，拒绝写入"
                )))
            }
        }
        cur = obj.get_mut(*p).expect("parent exists or was just created");
    }
    Ok(())
}

fn get_toml_path(doc: &toml_edit::DocumentMut, path: &str) -> Option<String> {
    let mut item: Option<&toml_edit::Item> = None;
    for (i, p) in path.split('.').enumerate() {
        item = if i == 0 { doc.get(p) } else { item?.get(p) };
    }
    let item = item?;
    if let Some(s) = item.as_str() {
        return Some(s.to_string());
    }
    if let Some(i) = item.as_integer() {
        return Some(i.to_string());
    }
    if let Some(b) = item.as_bool() {
        return Some(b.to_string());
    }
    if let Some(f) = item.as_float() {
        return Some(f.to_string());
    }
    None
}

fn set_toml_path(
    doc: &mut toml_edit::DocumentMut,
    path: &str,
    val: toml_edit::Value,
) -> Result<(), AppError> {
    let parts: Vec<&str> = path.split('.').collect();
    if parts.is_empty() {
        return Ok(());
    }
    if parts.len() == 1 {
        doc[parts[0]] = toml_edit::Item::Value(val);
        return Ok(());
    }
    let mut cur: &mut dyn toml_edit::TableLike = doc.as_table_mut();
    for p in &parts[..parts.len() - 1] {
        if cur.get(p).is_none() {
            cur.insert(p, toml_edit::table());
        }
        cur = cur
            .get_mut(p)
            .and_then(|i| i.as_table_like_mut())
            .ok_or_else(|| AppError::Config(format!("{path}: {p} 不是表，拒绝写入")))?;
    }
    cur.insert(parts[parts.len() - 1], toml_edit::Item::Value(val));
    Ok(())
}

/// Merge typed advanced knobs into a JSON config (Gemini / OpenCode).
/// For Gemini, ALL_CAPS keys are env vars — they live in `~/.gemini/.env`
/// (settings.json has no `env` block), so takeover routes them there itself and
/// this helper must only take the dotted settings paths.
pub(crate) fn merge_extra_json(
    doc: &mut serde_json::Value,
    extra: &std::collections::BTreeMap<String, String>,
    target: CliTarget,
    all_caps_to_env: bool,
) -> Result<(), AppError> {
    for (k, v) in extra {
        if k.is_empty()
            || v.is_empty()
            || blocked_for_user(target, k)
            || dedicated_keys(target).contains(&k.as_str())
        {
            continue;
        }
        if all_caps_to_env && is_env_key(k) {
            crate::services::cli_io::object_at(doc, "env")?
                .insert(k.clone(), serde_json::Value::String(v.clone()));
        } else {
            set_json_path(doc, k, parse_scalar(v).to_json())?;
        }
    }
    Ok(())
}

/// Merge typed advanced knobs into a TOML config (Codex / Grok).
pub(crate) fn merge_extra_toml(
    doc: &mut toml_edit::DocumentMut,
    extra: &std::collections::BTreeMap<String, String>,
    target: CliTarget,
) -> Result<(), AppError> {
    for (k, v) in extra {
        if k.is_empty()
            || v.is_empty()
            || blocked_for_user(target, k)
            || dedicated_keys(target).contains(&k.as_str())
        {
            continue;
        }
        set_toml_path(doc, k, parse_scalar(v).to_toml())?;
    }
    Ok(())
}

/// Metadata for the settings UI: which knobs exist for a target, what we
/// suggest, and what the machine currently has. `current` is what makes the
/// form a *view of reality* rather than a blank slate — the renderer shows
/// `current` when present and falls back to `default`.
#[derive(Debug, Serialize)]
pub struct EnvKeyInfo {
    pub key: String,
    pub description: String,
    pub default: String,
    pub current: Option<String>,
}

fn json_doc(root: &ConfigRoot, rel: &str) -> Option<serde_json::Value> {
    crate::services::cli_io::read_json(&root.join(rel)).ok()
}

fn toml_doc(root: &ConfigRoot, rel: &str) -> Option<toml_edit::DocumentMut> {
    std::fs::read_to_string(root.join(rel))
        .ok()?
        .parse()
        .ok()
}

/// Read the values a target currently has on disk, so the form opens
/// pre-filled instead of making the user retype what is already configured.
fn current_values(root: &ConfigRoot, target: CliTarget) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    let catalog = catalog_for(target);
    match target {
        CliTarget::ClaudeCode => {
            if let Some(doc) = json_doc(root, ".claude/settings.json") {
                if let Some(env) = doc.get("env").and_then(|e| e.as_object()) {
                    for (k, v) in env {
                        if let Some(s) = json_as_text(v) {
                            out.insert(k.clone(), s);
                        }
                    }
                }
            }
        }
        CliTarget::GeminiCli => {
            // env vars live in `~/.gemini/.env`, settings in settings.json.
            let dotenv =
                crate::services::cli_dotenv::read(&root.join(".gemini/.env"));
            if let Some(doc) = json_doc(root, ".gemini/settings.json") {
                for e in catalog {
                    if let Some(v) = if is_env_key(&e.key) {
                        dotenv.get(&e.key).cloned()
                    } else {
                        get_json_path(&doc, &e.key)
                    } {
                        out.insert(e.key.clone(), v);
                    }
                }
            }
            for (k, v) in dotenv {
                out.entry(k).or_insert(v);
            }
        }
        CliTarget::OpenCode => {
            if let Some(doc) = json_doc(root, ".config/opencode/opencode.json") {
                for e in catalog {
                    if let Some(v) = get_json_path(&doc, &e.key) {
                        out.insert(e.key.clone(), v);
                    }
                }
                if let Some(obj) = doc.as_object() {
                    for (k, v) in obj {
                        if let Some(s) = json_as_text(v) {
                            out.entry(k.clone()).or_insert(s);
                        }
                    }
                }
            }
        }
        CliTarget::Codex => {
            if let Some(doc) = toml_doc(root, ".codex/config.toml") {
                for e in catalog {
                    if let Some(v) = get_toml_path(&doc, &e.key) {
                        out.insert(e.key.clone(), v);
                    }
                }
                for (k, item) in doc.as_table().iter() {
                    let text = item.as_str().map(|s| s.to_string()).or_else(|| {
                        item.as_integer()
                            .map(|i| i.to_string())
                            .or_else(|| item.as_bool().map(|b| b.to_string()))
                    });
                    if let Some(s) = text {
                        out.entry(k.to_string()).or_insert(s);
                    }
                }
            }
        }
        CliTarget::GrokBuild => {
            if let Some(doc) = toml_doc(root, ".grok/config.toml") {
                for e in catalog {
                    if let Some(v) = get_toml_path(&doc, &e.key) {
                        out.insert(e.key.clone(), v);
                    }
                }
            }
        }
    }
    out
}

pub fn known_env_keys(root: &ConfigRoot, target: CliTarget) -> Vec<EnvKeyInfo> {
    let current = current_values(root, target);
    let mut out: Vec<EnvKeyInfo> = catalog_for(target)
        .iter()
        .filter(|e| !blocked_for_user(target, &e.key))
        .map(|e| EnvKeyInfo {
            key: e.key.clone(),
            description: e.description.clone(),
            default: e.default.clone(),
            current: current.get(&e.key).cloned(),
        })
        .collect();
    // Anything already on disk but not in the official catalog (FIGMA_TOKEN,
    // a brand-new flag, …) still has to show up.
    for (key, value) in &current {
        if blocked_for_user(target, key) {
            continue;
        }
        if out.iter().any(|row| row.key == *key) {
            continue;
        }
        out.push(EnvKeyInfo {
            key: key.clone(),
            description: "本机已有（官方文档未收录，原样保留）".into(),
            default: String::new(),
            current: Some(value.clone()),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_json_before_writing() {
        let dir = tempfile::tempdir().unwrap();
        let root = ConfigRoot::sandbox(dir.path().to_path_buf());
        let bk = crate::services::cli_backup::BackupStore::new(dir.path().join("bk"));
        let err = write_file(
            &root,
            CliTarget::ClaudeCode,
            ".claude/settings.json",
            "{not json",
            "s1",
            &bk,
        )
        .unwrap_err();
        assert!(err.to_string().contains("JSON"));
        // Nothing was written, and no snapshot was taken.
        assert!(!root.join(".claude/settings.json").exists());
    }

    #[test]
    fn rejects_path_outside_target() {
        let dir = tempfile::tempdir().unwrap();
        let root = ConfigRoot::sandbox(dir.path().to_path_buf());
        let bk = crate::services::cli_backup::BackupStore::new(dir.path().join("bk"));
        let err = write_file(
            &root,
            CliTarget::ClaudeCode,
            "../.ssh/authorized_keys",
            "{}",
            "s1",
            &bk,
        )
        .unwrap_err();
        assert!(err.to_string().contains("不属于"));
    }

    #[test]
    fn accepts_valid_toml_and_keeps_trailing_newline() {
        let dir = tempfile::tempdir().unwrap();
        let root = ConfigRoot::sandbox(dir.path().to_path_buf());
        let bk = crate::services::cli_backup::BackupStore::new(dir.path().join("bk"));
        write_file(
            &root,
            CliTarget::GrokBuild,
            ".grok/config.toml",
            "[models]\ndefault = \"x\"",
            "s1",
            &bk,
        )
        .unwrap();
        let body = std::fs::read_to_string(root.join(".grok/config.toml")).unwrap();
        assert!(body.ends_with('\n'));
    }

    #[test]
    fn read_files_reports_missing_without_erroring() {
        let dir = tempfile::tempdir().unwrap();
        let root = ConfigRoot::sandbox(dir.path().to_path_buf());
        let files = read_files(&root, CliTarget::Codex).unwrap();
        assert_eq!(files.len(), 2, "codex has auth.json + config.toml");
        assert!(files.iter().all(|f| !f.exists && f.body.is_empty()));
        assert_eq!(files[1].format, ConfigFormat::Toml);
    }

    #[test]
    fn form_lists_catalog_and_on_disk_extras_but_not_takeover_owned_keys() {
        let dir = tempfile::tempdir().unwrap();
        let root = ConfigRoot::sandbox(dir.path().to_path_buf());
        let path = root.join(".claude/settings.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{
              "env": {
                "ANTHROPIC_BASE_URL": "http://127.0.0.1:15722",
                "ANTHROPIC_AUTH_TOKEN": "secret",
                "ENABLE_TOOL_SEARCH": "true",
                "CLAUDE_AUTOCOMPACT_PCT_OVERRIDE": "48",
                "SOME_CUSTOM_FLAG": "1"
              }
            }"#,
        )
        .unwrap();
        let keys = known_env_keys(&root, CliTarget::ClaudeCode);
        let by: std::collections::BTreeMap<_, _> =
            keys.into_iter().map(|k| (k.key.clone(), k)).collect();
        assert!(by.contains_key("ENABLE_TOOL_SEARCH"));
        assert_eq!(
            by["ENABLE_TOOL_SEARCH"].current.as_deref(),
            Some("true")
        );
        assert_eq!(
            by["CLAUDE_AUTOCOMPACT_PCT_OVERRIDE"].current.as_deref(),
            Some("48")
        );
        assert_eq!(
            by["SOME_CUSTOM_FLAG"].current.as_deref(),
            Some("1"),
            "unknown on-disk keys must still appear"
        );
        assert!(
            !by.contains_key("ANTHROPIC_AUTH_TOKEN"),
            "takeover-owned secrets stay off the advanced list"
        );
        assert!(!by.contains_key("ANTHROPIC_BASE_URL"));
    }

    #[test]
    fn claude_catalog_covers_official_docs_not_a_short_subset() {
        let keys: Vec<_> = catalog_for(CliTarget::ClaudeCode)
            .iter()
            .map(|e| e.key.as_str())
            .collect();
        assert!(
            keys.len() >= 300,
            "catalog shrank: {} keys",
            keys.len()
        );
        for must in [
            "ENABLE_TOOL_SEARCH",
            "CLAUDE_AUTOCOMPACT_PCT_OVERRIDE",
            "CLAUDE_CODE_AUTO_COMPACT_WINDOW",
            "CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS",
            "DISABLE_TELEMETRY",
            "API_FORCE_IDLE_TIMEOUT",
            "CLAUDE_CODE_RETRY_WATCHDOG",
        ] {
            assert!(keys.contains(&must), "missing {must}");
        }
    }

    #[test]
    fn other_cli_catalogs_cover_official_docs_not_a_short_subset() {
        let dir = tempfile::tempdir().unwrap();
        let root = ConfigRoot::sandbox(dir.path().to_path_buf());
        let codex: Vec<_> = known_env_keys(&root, CliTarget::Codex)
            .into_iter()
            .map(|k| k.key)
            .collect();
        assert!(codex.len() >= 40, "codex catalog shrank: {}", codex.len());
        for must in ["sandbox_mode", "approval_policy", "model", "web_search"] {
            assert!(codex.iter().any(|k| k == must), "codex missing {must}");
        }
        assert!(
            !codex.iter().any(|k| k == "model_provider"),
            "takeover-owned model_provider stays off the form"
        );

        let gemini: Vec<_> = known_env_keys(&root, CliTarget::GeminiCli)
            .into_iter()
            .map(|k| k.key)
            .collect();
        assert!(gemini.len() >= 80, "gemini catalog shrank: {}", gemini.len());
        for must in ["model.name", "general.vimMode", "GEMINI_MODEL"] {
            assert!(gemini.iter().any(|k| k == must), "gemini missing {must}");
        }
        assert!(!gemini.iter().any(|k| k == "GEMINI_API_KEY"));

        let grok: Vec<_> = known_env_keys(&root, CliTarget::GrokBuild)
            .into_iter()
            .map(|k| k.key)
            .collect();
        assert!(grok.len() >= 50, "grok catalog shrank: {}", grok.len());
        for must in ["ui.simple_mode", "session.auto_compact_threshold_percent"] {
            assert!(grok.iter().any(|k| k == must), "grok missing {must}");
        }
        assert!(!grok.iter().any(|k| k == "models.default"));

        let oc: Vec<_> = known_env_keys(&root, CliTarget::OpenCode)
            .into_iter()
            .map(|k| k.key)
            .collect();
        assert!(oc.len() >= 45, "opencode catalog shrank: {}", oc.len());
        for must in [
            "model",
            "small_model",
            "autoupdate",
            // 点路径的三层结构 —— 只铺顶层标量的话这些会一起消失。
            "permission.bash",
            "server.port",
            "attachment.image.max_width",
        ] {
            assert!(oc.iter().any(|k| k == must), "opencode missing {must}");
        }
        assert!(!oc.iter().any(|k| k == "provider"));
    }

    #[test]
    fn merge_extra_json_writes_dotted_paths_and_env_keys() {
        let mut doc = serde_json::json!({});
        let mut extra = std::collections::BTreeMap::new();
        extra.insert("model.name".into(), "gemini-3".into());
        extra.insert("general.vimMode".into(), "true".into());
        extra.insert("GEMINI_MODEL".into(), "gemini-3".into());
        extra.insert("GEMINI_API_KEY".into(), "should-skip".into());
        merge_extra_json(&mut doc, &extra, CliTarget::GeminiCli, true).unwrap();
        // model.name / GEMINI_MODEL are the dedicated combo; extra_env must
        // not write them a second time (or fight the ComboBox).
        assert!(doc.pointer("/model/name").is_none());
        assert_eq!(doc.pointer("/general/vimMode").unwrap(), true);
        assert!(doc.pointer("/env/GEMINI_MODEL").is_none());
        assert!(doc.pointer("/env/GEMINI_API_KEY").is_none());
    }

    #[test]
    fn merge_extra_toml_writes_dotted_paths() {
        let mut doc = toml_edit::DocumentMut::new();
        let mut extra = std::collections::BTreeMap::new();
        extra.insert("ui.simple_mode".into(), "false".into());
        extra.insert("sandbox_mode".into(), "read-only".into());
        extra.insert("models.default".into(), "should-skip".into());
        merge_extra_toml(&mut doc, &extra, CliTarget::GrokBuild).unwrap();
        assert_eq!(
            doc["ui"]["simple_mode"].as_bool(),
            Some(false)
        );
        assert_eq!(doc["sandbox_mode"].as_str(), Some("read-only"));
        assert!(doc.get("models").is_none());
    }

    /// Dedicated slots live on TakeoverOptions. extra_env must not write them
    /// a second time — Grok's top-level `model = "…"` would replace the
    /// `[model.*]` table and take over would look like it never wrote an endpoint.
    #[test]
    fn dedicated_model_keys_are_skipped_in_extra_env() {
        let mut extra = std::collections::BTreeMap::new();
        extra.insert("model".into(), "grok-4-fast".into());
        extra.insert("model_reasoning_effort".into(), "high".into());

        let mut codex = toml_edit::DocumentMut::new();
        merge_extra_toml(&mut codex, &extra, CliTarget::Codex).unwrap();
        assert!(codex.get("model").is_none(), "codex writes it from its own slot");
        assert!(codex.get("model_reasoning_effort").is_none());

        let mut grok = toml_edit::DocumentMut::new();
        merge_extra_toml(&mut grok, &extra, CliTarget::GrokBuild).unwrap();
        assert!(grok.get("model").is_none(), "grok model is the dedicated combo");
        assert_eq!(grok["model_reasoning_effort"].as_str(), Some("high"));
    }

    /// Merging must never destroy what it did not write. A parent that is
    /// already a scalar is a refusal, exactly like the TOML side — overwriting
    /// it with an empty object is the "整块替换" this codebase forbids.
    #[test]
    fn set_json_path_refuses_a_scalar_parent_and_leaves_it_intact() {
        let mut doc = serde_json::json!({"model": "gemini-2.5-pro", "tools": ["a"]});
        let err =
            set_json_path(&mut doc, "model.name", serde_json::json!("gemini-3")).unwrap_err();
        assert!(err.to_string().contains("拒绝写入"), "{err}");
        assert_eq!(doc["model"], "gemini-2.5-pro", "the old value survives");

        let err = set_json_path(&mut doc, "tools.core", serde_json::json!("x")).unwrap_err();
        assert!(err.to_string().contains("拒绝写入"), "{err}");
        assert_eq!(doc["tools"], serde_json::json!(["a"]));

        // Missing and null parents are still created — that is the normal case.
        doc["nulled"] = serde_json::Value::Null;
        set_json_path(&mut doc, "nulled.deep.x", serde_json::json!(1)).unwrap();
        set_json_path(&mut doc, "general.vimMode", serde_json::json!(true)).unwrap();
        assert_eq!(doc.pointer("/nulled/deep/x").unwrap(), 1);
        assert_eq!(doc.pointer("/general/vimMode").unwrap(), true);
    }

    /// The form and the writer share one predicate; `CLAUDE_PID` typed into the
    /// custom-key box must be refused on the way to disk, not merely hidden.
    #[test]
    fn host_owned_keys_are_blocked_for_claude_code_only() {
        assert!(blocked_for_user(CliTarget::ClaudeCode, "CLAUDE_PID"));
        assert!(blocked_for_user(CliTarget::ClaudeCode, "CLAUDECODE"));
        assert!(blocked_for_user(CliTarget::ClaudeCode, "ANTHROPIC_AUTH_TOKEN"));
        assert!(!blocked_for_user(CliTarget::ClaudeCode, "API_TIMEOUT_MS"));
        // Only Claude Code's host injects these; elsewhere the name means nothing.
        assert!(!blocked_for_user(CliTarget::Codex, "CLAUDE_PID"));
        // Dotted children of an owned prefix are owned too.
        assert!(blocked_for_user(CliTarget::GrokBuild, "models.default"));
        assert!(blocked_for_user(CliTarget::OpenCode, "provider.ccload"));
    }
}
