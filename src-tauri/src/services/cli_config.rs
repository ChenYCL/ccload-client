//! Point AI coding CLIs at our local kernel by rewriting their config files.
//!
//! Safety: atomic writes, timestamped backups, merge-not-replace, optional
//! sandbox root so a dev build cannot touch a live `~/.claude/settings.json`.

use serde_json::Value;

use crate::error::AppError;
use crate::services::cli_advanced::{
    blocked_for_user, merge_extra_json, merge_extra_toml, TakeoverOptions,
};
use crate::services::cli_grok;
use crate::services::cli_io::{
    object_at, read_json, write_atomic, write_pretty_json,
};
pub use crate::services::cli_types::{CliTarget, ConfigRoot, TakeoverPreview, TakeoverResult};

/// Anthropic-native clients talk to the kernel root; OpenAI-compatible ones
/// need the `/v1` suffix. Grok's `api_backend = "responses"` also wants `/v1`.
fn expected_endpoint(target: CliTarget, base_url: &str) -> String {
    let base = base_url.trim_end_matches('/');
    match target {
        CliTarget::ClaudeCode | CliTarget::GeminiCli => base.to_string(),
        CliTarget::Codex | CliTarget::GrokBuild | CliTarget::OpenCode => format!("{base}/v1"),
    }
}

fn toml_string_at(path: &std::path::Path, keys: &[&str]) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    let doc = raw.parse::<toml_edit::DocumentMut>().ok()?;
    let mut item = doc.as_item();
    for k in keys {
        item = item.as_table_like()?.get(k)?;
    }
    item.as_str().map(str::to_string)
}

pub fn current_endpoint(root: &ConfigRoot, target: CliTarget) -> Option<String> {
    match target {
        CliTarget::ClaudeCode => read_json(&root.join(".claude/settings.json"))
            .ok()?
            .pointer("/env/ANTHROPIC_BASE_URL")?
            .as_str()
            .map(str::to_string),
        CliTarget::GeminiCli => read_json(&root.join(".gemini/settings.json"))
            .ok()?
            .pointer("/env/GOOGLE_GEMINI_BASE_URL")?
            .as_str()
            .map(str::to_string),
        CliTarget::OpenCode => read_json(&root.join(".config/opencode/opencode.json"))
            .ok()?
            .pointer("/provider/ccload/options/baseURL")?
            .as_str()
            .map(str::to_string),
        CliTarget::Codex => toml_string_at(
            &root.join(".codex/config.toml"),
            &["model_providers", "ccload", "base_url"],
        ),
        CliTarget::GrokBuild => cli_grok::current_endpoint(root),
    }
}

/// The credential each target is expected to carry once taken over. Endpoint
/// equality alone is not "already active": a config can point at the kernel
/// while still holding a token minted for a *previous* kernel instance, which
/// 401s on every call. Preview compares both.
pub fn current_token(root: &ConfigRoot, target: CliTarget) -> Option<String> {
    match target {
        CliTarget::ClaudeCode => read_json(&root.join(".claude/settings.json"))
            .ok()?
            .pointer("/env/ANTHROPIC_AUTH_TOKEN")?
            .as_str()
            .map(str::to_string),
        CliTarget::GeminiCli => read_json(&root.join(".gemini/settings.json"))
            .ok()?
            .pointer("/env/GEMINI_API_KEY")?
            .as_str()
            .map(str::to_string),
        CliTarget::OpenCode => read_json(&root.join(".config/opencode/opencode.json"))
            .ok()?
            .pointer("/provider/ccload/options/apiKey")?
            .as_str()
            .map(str::to_string),
        CliTarget::Codex => read_json(&root.join(".codex/auth.json"))
            .ok()?
            .get("CCLOAD_API_KEY")?
            .as_str()
            .map(str::to_string),
        CliTarget::GrokBuild => cli_grok::current_token(root),
    }
}

/// Compare two endpoints ignoring surrounding whitespace and a trailing slash,
/// so a hand-edited config is not reported as "not taken over" over cosmetics.
fn same_endpoint(a: &str, b: &str) -> bool {
    a.trim().trim_end_matches('/') == b.trim().trim_end_matches('/')
}

pub fn preview(
    root: &ConfigRoot,
    target: CliTarget,
    base_url: &str,
    api_token: Option<&str>,
) -> TakeoverPreview {
    let primary = root.join(target.relative_paths()[0]);
    let current = current_endpoint(root, target);
    let next = expected_endpoint(target, base_url);
    let endpoint_ok = current
        .as_deref()
        .is_some_and(|c| same_endpoint(c, &next));
    // No token yet means we cannot claim the config is current; the write is
    // what mints/propagates it.
    let token_ok = match api_token {
        Some(want) => current_token(root, target).as_deref() == Some(want),
        None => false,
    };
    TakeoverPreview {
        target,
        label: target.label(),
        path: primary.display().to_string(),
        exists: primary.exists(),
        already_active: endpoint_ok && token_ok,
        token_stale: endpoint_ok && !token_ok,
        current_endpoint: current,
        next_endpoint: next,
    }
}

pub fn apply_takeover(
    root: &ConfigRoot,
    target: CliTarget,
    base_url: &str,
    api_token: &str,
    stamp: &str,
    backups: &crate::services::cli_backup::BackupStore,
    opts: TakeoverOptions,
) -> Result<TakeoverResult, AppError> {
    // Snapshot the whole target (including files that do not exist yet, so a
    // restore can delete what we are about to create) before touching anything.
    let snapshot = backups.snapshot(root, target, stamp, "takeover")?;

    let endpoint = expected_endpoint(target, base_url);
    let mut written = Vec::new();

    match target {
        CliTarget::ClaudeCode => {
            let path = root.join(".claude/settings.json");
            let mut doc = read_json(&path)?;
            {
                let env = object_at(&mut doc, "env")?;
                env.insert("ANTHROPIC_BASE_URL".into(), Value::String(endpoint));
                env.insert("ANTHROPIC_AUTH_TOKEN".into(), Value::String(api_token.into()));
                env.remove("ANTHROPIC_API_KEY");
                // Optional model tier overrides. Empty string is intentionally
                // skipped — an empty model name is not "unset", it is a
                // literal empty string that the CLI would use verbatim.
                if let Some(m) = opts.anthropic_model.filter(|s| !s.is_empty()) {
                    env.insert("ANTHROPIC_MODEL".into(), Value::String(m));
                }
                if let Some(m) = opts.sonnet_model.filter(|s| !s.is_empty()) {
                    env.insert("ANTHROPIC_DEFAULT_SONNET_MODEL".into(), Value::String(m));
                }
                if let Some(m) = opts.opus_model.filter(|s| !s.is_empty()) {
                    env.insert("ANTHROPIC_DEFAULT_OPUS_MODEL".into(), Value::String(m));
                }
                if let Some(m) = opts.haiku_model.filter(|s| !s.is_empty()) {
                    env.insert("ANTHROPIC_DEFAULT_HAIKU_MODEL".into(), Value::String(m));
                }
                // Free-form extra env (timeout, retry, telemetry flags, …).
                // Same rules as every other CLI's merge: an emptied form field
                // means "leave it unset", not `"KEY": ""` — Claude Code would
                // use that literal empty string. Host-injected identity keys
                // (CLAUDE_PID …) are refused here too, not just hidden from
                // the form, because the custom-key input can type anything.
                if let Some(extra) = &opts.extra_env {
                    for (k, v) in extra {
                        if k.is_empty()
                            || v.is_empty()
                            || blocked_for_user(CliTarget::ClaudeCode, k)
                        {
                            continue;
                        }
                        env.insert(k.clone(), Value::String(v.clone()));
                    }
                }
            }
            write_pretty_json(&path, &doc)?;
            written.push(path.display().to_string());
        }
        CliTarget::GeminiCli => {
            let path = root.join(".gemini/settings.json");
            let mut doc = read_json(&path)?;
            {
                let env = object_at(&mut doc, "env")?;
                env.insert("GOOGLE_GEMINI_BASE_URL".into(), Value::String(endpoint));
                env.insert("GEMINI_API_KEY".into(), Value::String(api_token.into()));
            }
            if let Some(extra) = &opts.extra_env {
                merge_extra_json(&mut doc, extra, CliTarget::GeminiCli, true)?;
            }
            write_pretty_json(&path, &doc)?;
            written.push(path.display().to_string());
        }
        CliTarget::OpenCode => {
            let path = root.join(".config/opencode/opencode.json");
            let mut doc = read_json(&path)?;
            object_at(&mut doc, "provider")?.insert(
                "ccload".into(),
                serde_json::json!({
                    "npm": "@ai-sdk/openai-compatible",
                    "name": "ccLoad",
                    "options": { "baseURL": endpoint, "apiKey": api_token }
                }),
            );
            if let Some(extra) = &opts.extra_env {
                merge_extra_json(&mut doc, extra, CliTarget::OpenCode, false)?;
            }
            write_pretty_json(&path, &doc)?;
            written.push(path.display().to_string());
        }
        CliTarget::Codex => written.extend(write_codex(root, &endpoint, api_token, &opts)?),
        CliTarget::GrokBuild => {
            written.extend(cli_grok::apply(root, &endpoint, api_token)?);
            if let Some(extra) = &opts.extra_env {
                let path = root.join(".grok/config.toml");
                let raw = if path.exists() {
                    std::fs::read_to_string(&path)?
                } else {
                    String::new()
                };
                let mut doc = raw
                    .parse::<toml_edit::DocumentMut>()
                    .map_err(|e| AppError::Config(format!("{}: {e}", path.display())))?;
                merge_extra_toml(&mut doc, extra, CliTarget::GrokBuild)?;
                write_atomic(&path, &doc.to_string())?;
            }
        }
    }

    Ok(TakeoverResult {
        target,
        written,
        backup_id: snapshot.id,
        restart_required: target != CliTarget::ClaudeCode,
    })
}

fn write_codex(
    root: &ConfigRoot,
    endpoint: &str,
    api_token: &str,
    opts: &TakeoverOptions,
) -> Result<Vec<String>, AppError> {
    let mut written = Vec::new();
    let toml_path = root.join(".codex/config.toml");
    let raw = if toml_path.exists() {
        std::fs::read_to_string(&toml_path)?
    } else {
        String::new()
    };
    let mut doc = raw
        .parse::<toml_edit::DocumentMut>()
        .map_err(|e| AppError::Config(format!("{}: {e}", toml_path.display())))?;
    let providers = doc["model_providers"]
        .or_insert(toml_edit::table())
        .as_table_mut()
        .ok_or_else(|| AppError::Config("model_providers is not a table".into()))?;
    let mut ccload = toml_edit::table();
    if let Some(t) = ccload.as_table_mut() {
        t["name"] = toml_edit::value("ccLoad");
        t["base_url"] = toml_edit::value(endpoint);
        t["env_key"] = toml_edit::value("CCLOAD_API_KEY");
        // Official config-reference: `responses` is the only supported wire API.
        // ccLoad serves the Responses protocol at the /v1 prefix, so "chat" here
        // was wrong against current Codex builds.
        t["wire_api"] = toml_edit::value("responses");
    }
    providers["ccload"] = ccload;
    doc["model_provider"] = toml_edit::value("ccload");
    // Model selection + context window. Empty strings skipped, same rule as the
    // Claude env keys — an empty model is not "unset" for Codex either.
    if let Some(m) = opts.codex_model.as_ref().filter(|s| !s.is_empty()) {
        doc["model"] = toml_edit::value(m);
    }
    if let Some(e) = opts.codex_reasoning_effort.as_ref().filter(|s| !s.is_empty()) {
        doc["model_reasoning_effort"] = toml_edit::value(e);
    }
    if let Some(c) = opts.codex_context_window.filter(|n| *n > 0) {
        doc["model_context_window"] = toml_edit::value(c);
    }
    if let Some(extra) = &opts.extra_env {
        merge_extra_toml(&mut doc, extra, CliTarget::Codex)?;
    }
    write_atomic(&toml_path, &doc.to_string())?;
    written.push(toml_path.display().to_string());

    let auth_path = root.join(".codex/auth.json");
    let mut auth = read_json(&auth_path)?;
    if let Some(obj) = auth.as_object_mut() {
        obj.insert("CCLOAD_API_KEY".into(), Value::String(api_token.into()));
    }
    write_pretty_json(&auth_path, &auth)?;
    written.push(auth_path.display().to_string());
    Ok(written)
}
