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
    /// Free-form extra env entries merged into Claude/Gemini `env`.
    /// Also how the timeout/retry knobs get set — see `RESILIENT_ENV`.
    pub extra_env: Option<std::collections::BTreeMap<String, String>>,
    /// Codex: model selection and reasoning depth.
    pub codex_model: Option<String>,
    pub codex_reasoning_effort: Option<String>,
    pub codex_context_window: Option<i64>,
}

/// cc-switch's "resilient preset": long timeouts and retries for slow upstreams.
/// Offered as a one-click default because these three together are what stop a
/// long agent turn from being killed mid-stream.
pub const RESILIENT_ENV: &[(&str, &str)] = &[
    ("API_TIMEOUT_MS", "1200000"),
    ("API_FORCE_IDLE_TIMEOUT", "0"),
    ("CLAUDE_CODE_RETRY_WATCHDOG", "1"),
];

/// Official Claude Code env catalog, scraped from
/// https://code.claude.com/docs/en/env-vars and checked against 2.1.226.
/// The form lists every documented key — not a short "common" subset —
/// because settings.json `env` accepts the whole set.
const CLAUDE_ENV_CATALOG_JSON: &str = include_str!("../../data/claude-code-env.json");

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

#[derive(Deserialize)]
struct ClaudeEnvCatalogFile {
    keys: Vec<ClaudeEnvCatalogEntry>,
}

#[derive(Deserialize)]
struct ClaudeEnvCatalogEntry {
    key: String,
    description: String,
    #[serde(default)]
    default: String,
}

fn claude_env_catalog() -> &'static [ClaudeEnvCatalogEntry] {
    use std::sync::OnceLock;
    static CATALOG: OnceLock<Vec<ClaudeEnvCatalogEntry>> = OnceLock::new();
    CATALOG
        .get_or_init(|| {
            let file: ClaudeEnvCatalogFile = serde_json::from_str(CLAUDE_ENV_CATALOG_JSON)
                .expect("claude-code-env.json must parse");
            file.keys
        })
        .as_slice()
}

/// Codex knobs that live as top-level keys in `~/.codex/config.toml`.
/// `(key, description, default)`.
pub const KNOWN_CODEX_KEYS: &[(&str, &str, &str)] = &[
    ("model", "默认模型", ""),
    ("model_reasoning_effort", "推理深度 minimal|low|medium|high|xhigh", "high"),
    ("model_context_window", "上下文窗口（tokens）", ""),
    ("model_max_output_tokens", "单次输出上限", ""),
    ("sandbox_mode", "沙箱模式 read-only|workspace-write|danger-full-access", "workspace-write"),
    ("approval_policy", "审批策略 untrusted|on-failure|on-request|never", "on-request"),
    ("disable_response_storage", "true 关闭响应存储", ""),
];

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

/// Read the values a target currently has on disk, so the form opens
/// pre-filled instead of making the user retype what is already configured.
fn current_values(root: &ConfigRoot, target: CliTarget) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    match target {
        CliTarget::ClaudeCode => {
            if let Ok(doc) = crate::services::cli_io::read_json(&root.join(".claude/settings.json"))
            {
                if let Some(env) = doc.get("env").and_then(|e| e.as_object()) {
                    for (k, v) in env {
                        if let Some(s) = v.as_str() {
                            out.insert(k.clone(), s.to_string());
                        }
                    }
                }
            }
        }
        CliTarget::Codex => {
            if let Ok(raw) = std::fs::read_to_string(root.join(".codex/config.toml")) {
                if let Ok(doc) = raw.parse::<toml_edit::DocumentMut>() {
                    for (k, _, _) in KNOWN_CODEX_KEYS {
                        // Scalars only: a table would not round-trip through a
                        // single text input.
                        if let Some(item) = doc.get(k) {
                            if let Some(v) = item.as_str() {
                                out.insert((*k).to_string(), v.to_string());
                            } else if let Some(v) = item.as_integer() {
                                out.insert((*k).to_string(), v.to_string());
                            } else if let Some(v) = item.as_bool() {
                                out.insert((*k).to_string(), v.to_string());
                            }
                        }
                    }
                }
            }
        }
        _ => {}
    }
    out
}

pub fn known_env_keys(root: &ConfigRoot, target: CliTarget) -> Vec<EnvKeyInfo> {
    let current = current_values(root, target);
    let mut out: Vec<EnvKeyInfo> = match target {
        CliTarget::ClaudeCode => claude_env_catalog()
            .iter()
            .filter(|e| {
                !TAKEOVER_OWNED_ENV.contains(&e.key.as_str())
                    && !HOST_OWNED_ENV.contains(&e.key.as_str())
            })
            .map(|e| EnvKeyInfo {
                key: e.key.clone(),
                description: e.description.clone(),
                default: e.default.clone(),
                current: current.get(&e.key).cloned(),
            })
            .collect(),
        CliTarget::Codex => KNOWN_CODEX_KEYS
            .iter()
            .map(|(key, description, default)| EnvKeyInfo {
                key: (*key).to_string(),
                description: (*description).to_string(),
                default: (*default).to_string(),
                current: current.get(*key).cloned(),
            })
            .collect(),
        // Other CLIs carry their knobs in their own config files rather than
        // an env block; the raw editor covers them.
        _ => Vec::new(),
    };
    // Anything already on disk but not in the official catalog (FIGMA_TOKEN,
    // a brand-new Claude Code flag, …) still has to show up.
    if target == CliTarget::ClaudeCode {
        for (key, value) in &current {
            if TAKEOVER_OWNED_ENV.contains(&key.as_str()) || HOST_OWNED_ENV.contains(&key.as_str())
            {
                continue;
            }
            if out.iter().any(|row| row.key == *key) {
                continue;
            }
            out.push(EnvKeyInfo {
                key: key.clone(),
                description: "本机已有（官方 env 文档未收录，原样保留）".into(),
                default: String::new(),
                current: Some(value.clone()),
            });
        }
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
        let keys: Vec<_> = claude_env_catalog().iter().map(|e| e.key.as_str()).collect();
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
}
