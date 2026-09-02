//! Grok Build lives at `~/.grok/config.toml`.
//!
//! Shape (from cc-switch + a live official install):
//!   [models]
//!   default = "grok-4.6"
//!   [model."grok-4.6"]
//!   model / base_url / name / api_key|env_key / api_backend / context_window
//!
//! Official login has `[models]` but no `[model.*]` table. We must CREATE the
//! custom table rather than update fields that do not exist. Unrelated tables
//! (`cli`, `ui`, `marketplace`, `plugins`) are left untouched.
//!
//! `[models] default` alone does not take over Grok: it only picks the model for
//! *new* sessions, `grok --resume` restores the model the session was pinned to,
//! and Grok rewrites `default` itself whenever the user picks something in
//! `/model`. Any of those leaves inference on the official session token. So we
//! also override the built-in entry for the model the user is actually on —
//! Grok merges a `[model.<builtin>]` table over its defaults, so setting just
//! `base_url` + `api_key` there reroutes that model without redefining it.

use std::collections::HashSet;

use crate::error::AppError;
use crate::services::cli_io::write_atomic;
use crate::services::cli_types::ConfigRoot;
use crate::services::model_caps::{grok_api_backend, write_grok_effort_menu};

const PROFILE: &str = "ccload";
/// Only a fallback: used when there is no `[models] default` to inherit (fresh
/// install, empty file). Grok's own documented default for a new config.
const FALLBACK_MODEL: &str = "grok-4.6";
const DEFAULT_BACKEND: &str = "responses";
const DEFAULT_WINDOW: i64 = 500_000;

pub fn current_endpoint(root: &ConfigRoot) -> Option<String> {
    let raw = std::fs::read_to_string(root.join(".grok/config.toml")).ok()?;
    let doc: toml_edit::DocumentMut = raw.parse().ok()?;
    let default = doc
        .get("models")?
        .get("default")?
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    doc.get("model")?
        .get(default)?
        .get("base_url")?
        .as_str()
        .map(|s| s.trim_end_matches('/').to_string())
}

/// The api_key stored on the active profile, for takeover staleness checks.
pub fn current_token(root: &ConfigRoot) -> Option<String> {
    let raw = std::fs::read_to_string(root.join(".grok/config.toml")).ok()?;
    let doc: toml_edit::DocumentMut = raw.parse().ok()?;
    let default = doc
        .get("models")?
        .get("default")?
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    doc.get("model")?
        .get(default)?
        .get("api_key")?
        .as_str()
        .map(str::to_string)
}

pub fn apply(
    root: &ConfigRoot,
    endpoint: &str,
    api_token: &str,
) -> Result<Vec<String>, AppError> {
    apply_with_model(root, endpoint, api_token, None, None)
}

/// `model` is the kernel alias the ccload profile should send. `None` keeps
/// inheriting whatever the user last picked in Grok's `/model`.
pub fn apply_with_model(
    root: &ConfigRoot,
    endpoint: &str,
    api_token: &str,
    model: Option<&str>,
    // 总控算好的窗口。None 时退回按模型名推断（老行为）。
    context_tokens: Option<i64>,
) -> Result<Vec<String>, AppError> {
    let path = root.join(".grok/config.toml");
    let raw = if path.exists() {
        std::fs::read_to_string(&path)?
    } else {
        String::new()
    };
    let mut doc: toml_edit::DocumentMut = raw
        .parse()
        .map_err(|e| AppError::Config(format!("{}: {e}", path.display())))?;

    let profile = existing_profile(&doc).unwrap_or(PROFILE).to_string();
    // Both read before the writers below clobber `default` and the profile's
    // base_url.
    let picked = model.map(str::trim).filter(|s| !s.is_empty());
    let inherited = picked
        .map(|s| s.to_string())
        .or_else(|| inherited_model(&doc, &profile));
    let previous = profile_endpoint(&doc, &profile);
    ensure_models_default(&mut doc, &profile);
    let routed = upsert_model_table(
        &mut doc,
        &profile,
        inherited.as_deref(),
        endpoint,
        api_token,
    )?;
    if routed != profile {
        if is_grok_builtin(&routed) {
            override_builtin(&mut doc, &routed, endpoint, api_token)?;
        } else {
            // A non-grok alias (opus-5, glm-5.3-flash[1M], …) has no built-in
            // catalog to inherit. Write a full custom table so `/model` and
            // `/effort` both work without a separate import.
            let w = context_tokens
                .filter(|n| *n > 0)
                .unwrap_or_else(|| crate::services::context_window::parse_window(&routed) as i64);
            write_catalog_entry(&mut doc, &routed, endpoint, api_token, Some(w))?;
        }
    }
    if let Some(previous) = previous {
        resync_previous_overrides(&mut doc, &previous, endpoint, api_token)?;
    }

    write_atomic(&path, &doc.to_string())?;
    Ok(vec![path.display().to_string()])
}

/// The id Grok will send, not the profile name. `models.default = "ccload"`
/// plus `[model.ccload].model = "glm-5.3-flash[1M]"` should report the latter.
pub fn current_model(root: &ConfigRoot) -> Option<String> {
    let raw = std::fs::read_to_string(root.join(".grok/config.toml")).ok()?;
    let doc: toml_edit::DocumentMut = raw.parse().ok()?;
    let default = doc
        .get("models")?
        .get("default")?
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    if let Some(m) = doc
        .get("model")
        .and_then(|t| t.get(default))
        .and_then(|t| t.get("model"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Some(m.to_string());
    }
    Some(default.to_string())
}

fn is_grok_builtin(alias: &str) -> bool {
    let n = match alias.rsplit_once('/') {
        Some((_, rest)) if !rest.is_empty() => rest,
        _ => alias,
    };
    let n = n.to_ascii_lowercase();
    n == "grok" || n.starts_with("grok-")
}

fn trimmed(url: &str) -> &str {
    url.trim().trim_end_matches('/')
}

/// The endpoint the profile pointed at before this run — the fingerprint for
/// telling apart the entries we wrote from ones the user maintains by hand.
fn profile_endpoint(doc: &toml_edit::DocumentMut, profile: &str) -> Option<String> {
    let url = doc
        .get("model")?
        .get(profile)?
        .get("base_url")?
        .as_str()
        .map(trimmed)?;
    (!url.is_empty()).then(|| url.to_string())
}

/// Users switch models over time, so each takeover can leave a built-in override
/// behind. Move every entry that still carries our old endpoint onto the new one
/// — otherwise a rotated token 401s on every model but the current one, and the
/// user sees it as "接管只对一个模型生效".
///
/// Matching on the old endpoint keeps this narrow: an entry pointing anywhere
/// else is the user's own and is left alone.
fn resync_previous_overrides(
    doc: &mut toml_edit::DocumentMut,
    previous: &str,
    endpoint: &str,
    api_token: &str,
) -> Result<(), AppError> {
    let Some(table) = doc.get_mut("model").and_then(|m| m.as_table_like_mut()) else {
        return Ok(());
    };
    let stale: Vec<String> = table
        .iter()
        .filter(|(_, v)| {
            v.as_table_like()
                .and_then(|t| t.get("base_url"))
                .and_then(|v| v.as_str())
                .is_some_and(|url| trimmed(url) == previous)
        })
        .map(|(k, _)| k.to_string())
        .collect();
    for key in stale {
        if let Some(entry) = table.get_mut(&key).and_then(|i| i.as_table_like_mut()) {
            entry.insert("base_url", toml_edit::value(endpoint));
            entry.insert("api_key", toml_edit::value(api_token));
        }
    }
    Ok(())
}

fn existing_profile(doc: &toml_edit::DocumentMut) -> Option<&str> {
    let default = doc.get("models")?.get("default")?.as_str()?.trim();
    if default.is_empty() {
        return None;
    }
    // Official state: default is set but the matching [model.*] table is absent.
    doc.get("model")?.get(default)?.as_table_like()?;
    Some(default)
}

/// The model id the user was on before this takeover.
///
/// When `default` is not the profile we are about to write into, `existing_profile`
/// already proved there is no `[model.<default>]` table — so the header name *is*
/// the id Grok sends upstream, i.e. a built-in.
fn inherited_model(doc: &toml_edit::DocumentMut, profile: &str) -> Option<String> {
    let default = doc.get("models")?.get("default")?.as_str()?.trim();
    (!default.is_empty() && default != profile).then(|| default.to_string())
}

fn ensure_models_default(doc: &mut toml_edit::DocumentMut, profile: &str) {
    let models = doc["models"].or_insert(toml_edit::table());
    if let Some(t) = models.as_table_like_mut() {
        t.insert("default", toml_edit::value(profile));
    }
}

/// Writes `[model.<profile>]` and returns the upstream model id it routes to, so
/// the caller knows which built-in entry needs the same endpoint.
fn upsert_model_table(
    doc: &mut toml_edit::DocumentMut,
    profile: &str,
    inherited: Option<&str>,
    endpoint: &str,
    api_token: &str,
) -> Result<String, AppError> {
    let models = doc["model"].or_insert(toml_edit::table());
    let table = models
        .as_table_like_mut()
        .ok_or_else(|| AppError::Config("[model] is not a table".into()))?;
    if table.get(profile).is_none() {
        let mut created = toml_edit::table();
        if let Some(t) = created.as_table_mut() {
            t["name"] = toml_edit::value("ccLoad");
            t["api_backend"] = toml_edit::value(DEFAULT_BACKEND);
            t["context_window"] = toml_edit::value(DEFAULT_WINDOW);
        }
        table.insert(profile, created);
    }
    let selected = table
        .get_mut(profile)
        .and_then(|i| i.as_table_like_mut())
        .ok_or_else(|| AppError::Config(format!("[model.{profile}] is not a table")))?;
    // Follow whatever the user last picked in Grok. Only when there is nothing to
    // inherit do we keep the stored value, so a re-apply after `/model grok-4.7`
    // moves the profile forward instead of pinning it to a version we shipped.
    let routed = inherited
        .map(str::to_string)
        .or_else(|| {
            selected
                .get("model")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| FALLBACK_MODEL.to_string());
    selected.insert("model", toml_edit::value(&routed));
    selected.insert("base_url", toml_edit::value(endpoint));
    selected.insert("api_key", toml_edit::value(api_token));
    selected.insert("api_backend", toml_edit::value(DEFAULT_BACKEND));
    // Custom profile ids (ccload) do not inherit Grok's built-in catalog, so
    // `/effort` is dropped unless the table itself declares the menu. Follow
    // the routed model: grok-4.6 has xhigh, grok-4.5 does not, and a re-apply
    // after `/model grok-4.5` must not keep advertising a level 4.5 rejects.
    write_grok_effort_menu(selected, &routed);
    Ok(routed)
}

/// Point a built-in model at ccLoad by merging over Grok's defaults. Only
/// `base_url`/`api_key` — `api_backend`, `context_window` and the rest must keep
/// inheriting, or a non-`responses` built-in would break.
fn override_builtin(
    doc: &mut toml_edit::DocumentMut,
    model: &str,
    endpoint: &str,
    api_token: &str,
) -> Result<(), AppError> {
    let models = doc["model"].or_insert(toml_edit::table());
    let table = models
        .as_table_like_mut()
        .ok_or_else(|| AppError::Config("[model] is not a table".into()))?;
    if table.get(model).is_none() {
        table.insert(model, toml_edit::table());
    }
    let entry = table
        .get_mut(model)
        .and_then(|i| i.as_table_like_mut())
        .ok_or_else(|| AppError::Config(format!("[model.{model}] is not a table")))?;
    entry.insert("base_url", toml_edit::value(endpoint));
    entry.insert("api_key", toml_edit::value(api_token));
    Ok(())
}

/// One kernel alias as a selectable `[model.<alias>]` table.
///
/// Takeover only reroutes the active built-in + the `ccload` profile. Import
/// is how `glm-5.3-flash[1M]` / `claude-opus-5` show up in Grok's `/model`
/// picker: a full custom table (id, window, effort menu, backend), pointing
/// at the same ccLoad endpoint. `models.default` is not touched.
pub fn write_catalog_entry(
    doc: &mut toml_edit::DocumentMut,
    alias: &str,
    endpoint: &str,
    api_token: &str,
    context_window: Option<i64>,
) -> Result<(), AppError> {
    if alias.trim().is_empty() {
        return Ok(());
    }
    if doc.get("model").is_some_and(|m| m.is_str()) {
        return Err(AppError::Config(
            "~/.grok/config.toml 顶层的 model 是字符串，不是模型表。Grok 的自定义模型写在 [model.别名] 下；请先在高级配置里去掉顶层 model 键再导入"
                .into(),
        ));
    }
    let models = doc["model"].or_insert(toml_edit::table());
    let table = models
        .as_table_like_mut()
        .ok_or_else(|| AppError::Config("[model] is not a table".into()))?;
    let created = table.get(alias).is_none();
    if created {
        table.insert(alias, toml_edit::table());
    }
    let entry = table
        .get_mut(alias)
        .and_then(|i| i.as_table_like_mut())
        .ok_or_else(|| AppError::Config(format!("[model.{alias}] is not a table")))?;
    entry.insert("name", toml_edit::value(alias));
    entry.insert("model", toml_edit::value(alias));
    entry.insert("base_url", toml_edit::value(endpoint));
    if !api_token.is_empty() {
        entry.insert("api_key", toml_edit::value(api_token));
    }
    if created || entry.get("api_backend").is_none() {
        entry.insert("api_backend", toml_edit::value(grok_api_backend(alias)));
    }
    if let Some(w) = context_window.filter(|n| *n > 0) {
        entry.insert("context_window", toml_edit::value(w));
    }
    // Official grok models compact at 80% of the window. Custom tables default
    // to Grok's 200k if we omit context_window, and to the session 85% if we
    // omit this — both wrong for a 1M glm/claude alias.
    if created || entry.get("auto_compact_threshold_percent").is_none() {
        entry.insert("auto_compact_threshold_percent", toml_edit::value(80));
    }
    write_grok_effort_menu(entry, alias);
    Ok(())
}

/// Drop `[model.*]` tables that still point at our endpoint but are not in
/// this import. Never touches the active default or the `ccload` profile —
/// those are takeover's, and pruning them would send the current session
/// back to official xAI.
pub fn prune_catalog(
    doc: &mut toml_edit::DocumentMut,
    keep: &HashSet<String>,
    endpoint: &str,
) -> Vec<String> {
    let default = doc
        .get("models")
        .and_then(|m| m.get("default"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let Some(table) = doc.get_mut("model").and_then(|m| m.as_table_like_mut()) else {
        return Vec::new();
    };
    let dropped: Vec<String> = table
        .iter()
        .filter(|(k, v)| {
            if *k == PROFILE || *k == default {
                return false;
            }
            if keep.contains(*k) {
                return false;
            }
            v.as_table_like()
                .and_then(|t| t.get("base_url"))
                .and_then(|v| v.as_str())
                .is_some_and(|url| trimmed(url) == trimmed(endpoint))
        })
        .map(|(k, _)| k.to_string())
        .collect();
    for k in &dropped {
        table.remove(k);
    }
    dropped
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::cli_types::ConfigRoot;

    fn tmp_root() -> (tempfile::TempDir, ConfigRoot) {
        let dir = tempfile::tempdir().unwrap();
        let root = ConfigRoot::sandbox(dir.path().to_path_buf());
        (dir, root)
    }

    fn seed(root: &ConfigRoot, body: &str) -> std::path::PathBuf {
        let path = root.join(".grok/config.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, body).unwrap();
        path
    }

    /// A `[model.x]` header is quoted only when the id needs it (`grok-4.6` has a
    /// dot, `ccload` does not), so tests must accept both spellings.
    fn has_table(body: &str, name: &str) -> bool {
        body.contains(&format!("[model.{name}]")) || body.contains(&format!("[model.\"{name}\"]"))
    }

    #[test]
    fn official_state_creates_model_table() {
        let (_keep, root) = tmp_root();
        let path = seed(
            &root,
            "[cli]\nauto_update = true\n\n[models]\ndefault = \"grok-4.6\"\n",
        );

        apply(&root, "http://127.0.0.1:15722/v1", "tok-abc").unwrap();
        let out = std::fs::read_to_string(&path).unwrap();
        assert!(out.contains("auto_update = true"), "unrelated tables must survive");
        assert!(has_table(&out, "ccload"));
        assert!(out.contains("base_url = \"http://127.0.0.1:15722/v1\""));
        assert!(out.contains("api_key = \"tok-abc\""));
        assert_eq!(
            current_endpoint(&root).as_deref(),
            Some("http://127.0.0.1:15722/v1")
        );
    }

    #[test]
    fn profile_inherits_the_model_the_user_was_on() {
        let (_keep, root) = tmp_root();
        let path = seed(&root, "[models]\ndefault = \"grok-4.6\"\n");

        apply(&root, "https://proxy.test/v1", "tok").unwrap();
        let doc: toml_edit::DocumentMut = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        assert_eq!(
            doc["model"]["ccload"]["model"].as_str(),
            Some("grok-4.6"),
            "must follow the user's model, not a version we hardcoded"
        );
    }

    #[test]
    fn builtin_entry_is_rerouted_so_resumed_sessions_follow() {
        let (_keep, root) = tmp_root();
        let path = seed(&root, "[models]\ndefault = \"grok-4.6\"\n");

        apply(&root, "https://proxy.test/v1", "tok").unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(has_table(&body, "grok-4.6"), "missing built-in override:\n{body}");
        let doc: toml_edit::DocumentMut = body.parse().unwrap();
        let builtin = &doc["model"]["grok-4.6"];
        assert_eq!(builtin["base_url"].as_str(), Some("https://proxy.test/v1"));
        assert_eq!(builtin["api_key"].as_str(), Some("tok"));
        // Everything else keeps inheriting Grok's own defaults for that model.
        assert!(builtin.get("api_backend").is_none());
        assert!(builtin.get("context_window").is_none());
    }

    #[test]
    fn reapply_refreshes_both_tables_and_keeps_the_model() {
        let (_keep, root) = tmp_root();
        let path = seed(&root, "[models]\ndefault = \"grok-4.6\"\n");

        apply(&root, "https://old.test/v1", "old-tok").unwrap();
        apply(&root, "https://new.test/v1", "new-tok").unwrap();

        let doc: toml_edit::DocumentMut = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        assert_eq!(doc["models"]["default"].as_str(), Some("ccload"));
        assert_eq!(doc["model"]["ccload"]["model"].as_str(), Some("grok-4.6"));
        for table in ["ccload", "grok-4.6"] {
            assert_eq!(
                doc["model"][table]["base_url"].as_str(),
                Some("https://new.test/v1"),
                "[model.{table}] kept a stale endpoint"
            );
            assert_eq!(doc["model"][table]["api_key"].as_str(), Some("new-tok"));
        }
        assert!(!std::fs::read_to_string(&path).unwrap().contains("old-tok"));
    }

    /// Grok rewrites `[models] default` itself when the user picks a model in
    /// `/model`. The next apply must move the profile onto that model rather
    /// than leave it pointing at whatever it was created with.
    fn grok_switched_the_default_away(from: &str, to: &str) {
        let (_keep, root) = tmp_root();
        let path = seed(&root, &format!("[models]\ndefault = \"{from}\"\n"));
        apply(&root, "https://proxy.test/v1", "tok").unwrap();

        let mut doc: toml_edit::DocumentMut =
            std::fs::read_to_string(&path).unwrap().parse().unwrap();
        doc["models"]["default"] = toml_edit::value(to);
        std::fs::write(&path, doc.to_string()).unwrap();

        apply(&root, "https://proxy.test/v1", "tok").unwrap();
        let doc: toml_edit::DocumentMut = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        assert_eq!(doc["models"]["default"].as_str(), Some("ccload"));
        assert_eq!(doc["model"]["ccload"]["model"].as_str(), Some(to));
        assert_eq!(
            doc["model"][to]["base_url"].as_str(),
            Some("https://proxy.test/v1")
        );
    }

    #[test]
    fn reapply_follows_a_model_switch() {
        grok_switched_the_default_away("grok-4.6", "grok-4.5");
    }

    #[test]
    fn reapply_after_a_switch_to_grok_45_drops_xhigh_from_the_profile_menu() {
        let (_keep, root) = tmp_root();
        let path = seed(&root, "[models]\ndefault = \"grok-4.6\"\n");
        apply(&root, "https://proxy.test/v1", "tok").unwrap();

        let mut doc: toml_edit::DocumentMut =
            std::fs::read_to_string(&path).unwrap().parse().unwrap();
        doc["models"]["default"] = toml_edit::value("grok-4.5");
        std::fs::write(&path, doc.to_string()).unwrap();

        apply(&root, "https://proxy.test/v1", "tok").unwrap();
        let doc: toml_edit::DocumentMut = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        assert_eq!(doc["model"]["ccload"]["model"].as_str(), Some("grok-4.5"));
        assert_eq!(doc["model"]["ccload"]["reasoning_effort"].as_str(), Some("high"));
        let efforts = doc["model"]["ccload"]["reasoning_efforts"].as_array().unwrap();
        assert!(
            !efforts.iter().any(|v| v
                .as_inline_table()
                .and_then(|t| t.get("id"))
                .and_then(|x| x.as_str())
                == Some("xhigh")),
            "grok-4.5 must not advertise xhigh: {efforts}"
        );
    }

    /// Overrides left behind by earlier model switches must follow a rotated
    /// token, or every model but the current one starts 401ing.
    #[test]
    fn a_rotated_token_reaches_overrides_left_by_earlier_switches() {
        let (_keep, root) = tmp_root();
        let path = seed(&root, "[models]\ndefault = \"grok-4.6\"\n");
        apply(&root, "https://proxy.test/v1", "tok-1").unwrap();

        // User switches to 4.5 in the TUI; Grok rewrites `default`.
        let mut doc: toml_edit::DocumentMut =
            std::fs::read_to_string(&path).unwrap().parse().unwrap();
        doc["models"]["default"] = toml_edit::value("grok-4.5");
        // ...and keeps a hand-maintained entry of their own.
        doc["model"]["scratch"]["base_url"] = toml_edit::value("https://mine.test/v1");
        doc["model"]["scratch"]["api_key"] = toml_edit::value("mine");
        std::fs::write(&path, doc.to_string()).unwrap();

        apply(&root, "https://proxy.test/v1", "tok-2").unwrap();

        let doc: toml_edit::DocumentMut = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        for table in ["ccload", "grok-4.6", "grok-4.5"] {
            assert_eq!(
                doc["model"][table]["api_key"].as_str(),
                Some("tok-2"),
                "[model.{table}] kept a revoked token"
            );
        }
        assert_eq!(
            doc["model"]["scratch"]["api_key"].as_str(),
            Some("mine"),
            "entries pointing elsewhere are the user's, not ours"
        );
    }

    #[test]
    fn an_endpoint_change_moves_every_entry_we_wrote() {
        let (_keep, root) = tmp_root();
        let path = seed(&root, "[models]\ndefault = \"grok-4.6\"\n");
        apply(&root, "https://old.test/v1", "tok").unwrap();
        apply(&root, "https://new.test/v1", "tok").unwrap();

        let body = std::fs::read_to_string(&path).unwrap();
        assert!(!body.contains("old.test"), "stale endpoint survived:\n{body}");
    }

    #[test]
    fn empty_config_falls_back_without_a_bogus_override() {
        let (_keep, root) = tmp_root();
        let path = root.join(".grok/config.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        apply(&root, "https://proxy.test/v1", "tok").unwrap();
        let doc: toml_edit::DocumentMut = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        assert_eq!(doc["model"]["ccload"]["model"].as_str(), Some(FALLBACK_MODEL));
        assert_eq!(
            doc["model"][FALLBACK_MODEL]["base_url"].as_str(),
            Some("https://proxy.test/v1")
        );
    }

    /// A user who already keeps their own `[model.mine]` profile: we write into
    /// it instead of minting `ccload`, and must not fabricate a `[model.mine]`
    /// "built-in" override for the profile name itself.
    #[test]
    fn user_owned_profile_is_not_overridden_as_a_builtin() {
        let (_keep, root) = tmp_root();
        let path = seed(
            &root,
            "[models]\ndefault = \"mine\"\n\n[model.mine]\nmodel = \"grok-4.5\"\n",
        );

        apply(&root, "https://proxy.test/v1", "tok").unwrap();
        let doc: toml_edit::DocumentMut = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        assert_eq!(doc["models"]["default"].as_str(), Some("mine"));
        assert_eq!(
            doc["model"]["mine"]["model"].as_str(),
            Some("grok-4.5"),
            "an existing profile's model must not be rewritten"
        );
        assert!(doc["model"].get("ccload").is_none());
        // The built-in it routes to still gets rerouted.
        assert_eq!(
            doc["model"]["grok-4.5"]["base_url"].as_str(),
            Some("https://proxy.test/v1")
        );
    }

    /// Custom `ccload` does not inherit the built-in catalog, so without
    /// `supports_reasoning_effort` the TUI drops `/effort` on the floor.
    #[test]
    fn ccload_profile_declares_the_routed_models_effort_menu() {
        let (_keep, root) = tmp_root();
        let path = seed(&root, "[models]\ndefault = \"grok-4.6\"\n");
        apply(&root, "https://proxy.test/v1", "tok").unwrap();
        let doc: toml_edit::DocumentMut = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        let p = &doc["model"]["ccload"];
        assert_eq!(p["supports_reasoning_effort"].as_bool(), Some(true));
        assert_eq!(p["reasoning_effort"].as_str(), Some("xhigh"));
        let efforts = p["reasoning_efforts"].as_array().expect("menu");
        assert!(
            efforts.iter().any(|v| v
                .as_inline_table()
                .and_then(|t| t.get("id"))
                .and_then(|x| x.as_str())
                == Some("xhigh")),
            "{efforts}"
        );
        // Built-in override keeps inheriting Grok's own menu.
        assert!(doc["model"]["grok-4.6"].get("supports_reasoning_effort").is_none());
    }

    #[test]
    fn catalog_entry_for_a_non_grok_alias_uses_chat_completions_and_its_window() {
        let mut doc = toml_edit::DocumentMut::new();
        write_catalog_entry(
            &mut doc,
            "glm-5.3-flash[1M]",
            "https://proxy.test/v1",
            "tok",
            Some(1_000_000),
        )
        .unwrap();
        let e = &doc["model"]["glm-5.3-flash[1M]"];
        assert_eq!(e["model"].as_str(), Some("glm-5.3-flash[1M]"));
        assert_eq!(e["api_backend"].as_str(), Some("chat_completions"));
        assert_eq!(e["context_window"].as_integer(), Some(1_000_000));
        assert_eq!(e["supports_reasoning_effort"].as_bool(), Some(true));
        assert_eq!(e["auto_compact_threshold_percent"].as_integer(), Some(80));
    }

    #[test]
    fn catalog_entry_does_not_move_the_active_default() {
        let mut doc = "[models]\ndefault = \"ccload\"\n"
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        write_catalog_entry(&mut doc, "claude-opus-5", "https://proxy.test/v1", "tok", Some(1_000_000))
            .unwrap();
        assert_eq!(doc["models"]["default"].as_str(), Some("ccload"));
    }

    #[test]
    fn prune_drops_stale_aliases_but_keeps_the_profile_and_the_active_default() {
        let mut doc = r#"
[models]
default = "ccload"
[model.ccload]
base_url = "https://proxy.test/v1"
[model.retired]
base_url = "https://proxy.test/v1"
[model.mine]
base_url = "https://elsewhere.test/v1"
"#
        .parse::<toml_edit::DocumentMut>()
        .unwrap();
        let keep = HashSet::from(["glm-5.3-flash[1M]".into()]);
        let dropped = prune_catalog(&mut doc, &keep, "https://proxy.test/v1");
        assert_eq!(dropped, vec!["retired"]);
        assert!(doc["model"].get("ccload").is_some());
        assert!(doc["model"].get("mine").is_some());
        assert!(doc["model"].get("retired").is_none());
    }
}
