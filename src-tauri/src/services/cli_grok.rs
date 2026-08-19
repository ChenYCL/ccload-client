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

use crate::error::AppError;
use crate::services::cli_io::write_atomic;
use crate::services::cli_types::ConfigRoot;

const PROFILE: &str = "ccload";
const DEFAULT_MODEL: &str = "grok-4.5";
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
    ensure_models_default(&mut doc, &profile);
    upsert_model_table(&mut doc, &profile, endpoint, api_token)?;

    write_atomic(&path, &doc.to_string())?;
    Ok(vec![path.display().to_string()])
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

fn ensure_models_default(doc: &mut toml_edit::DocumentMut, profile: &str) {
    let models = doc["models"].or_insert(toml_edit::table());
    if let Some(t) = models.as_table_like_mut() {
        t.insert("default", toml_edit::value(profile));
    }
}

fn upsert_model_table(
    doc: &mut toml_edit::DocumentMut,
    profile: &str,
    endpoint: &str,
    api_token: &str,
) -> Result<(), AppError> {
    let models = doc["model"].or_insert(toml_edit::table());
    let table = models
        .as_table_like_mut()
        .ok_or_else(|| AppError::Config("[model] is not a table".into()))?;
    if table.get(profile).is_none() {
        let mut created = toml_edit::table();
        if let Some(t) = created.as_table_mut() {
            t["model"] = toml_edit::value(DEFAULT_MODEL);
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
    selected.insert("base_url", toml_edit::value(endpoint));
    selected.insert("api_key", toml_edit::value(api_token));
    selected.insert("api_backend", toml_edit::value(DEFAULT_BACKEND));
    Ok(())
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

    #[test]
    fn official_state_creates_model_table() {
        let (_keep, root) = tmp_root();
        let path = root.join(".grok/config.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "[cli]\nauto_update = true\n\n[models]\ndefault = \"grok-4.6\"\n",
        )
        .unwrap();

        apply(&root, "http://127.0.0.1:15722/v1", "tok-abc").unwrap();
        let out = std::fs::read_to_string(&path).unwrap();
        assert!(out.contains("auto_update = true"), "unrelated tables must survive");
        assert!(out.contains("[model.ccload]") || out.contains("[model.\"ccload\"]"));
        assert!(out.contains("base_url = \"http://127.0.0.1:15722/v1\""));
        assert!(out.contains("api_key = \"tok-abc\""));
        assert_eq!(
            current_endpoint(&root).as_deref(),
            Some("http://127.0.0.1:15722/v1")
        );
    }
}
