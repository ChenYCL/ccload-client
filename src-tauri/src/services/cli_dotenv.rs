//! Minimal `.env` reader/writer for Gemini CLI.
//!
//! Gemini has no `env` block in `settings.json` — `loadEnvironment()` only ever
//! reads a `.env` file (walking up from the workspace, falling back to
//! `~/.gemini/.env`) plus the real process environment. So env-var takeover has
//! to land in a dotenv file, not in the settings JSON.
//!
//! Merge by key, never rewrite the file wholesale: users keep their own vars,
//! comments and blank lines in there.

use std::collections::BTreeMap;
use std::path::Path;

use crate::error::AppError;
use crate::services::cli_io::write_atomic;

/// Values that survive a bare `KEY=value` round-trip through dotenv. Anything
/// else gets single-quoted, which dotenv treats as a literal — no `\n` or `$`
/// expansion to corrupt a token.
fn needs_quoting(value: &str) -> bool {
    value.is_empty()
        || value.trim() != value
        || !value.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':' | '/' | '@' | '+' | '~')
        })
}

fn render(key: &str, value: &str) -> String {
    if !needs_quoting(value) {
        return format!("{key}={value}");
    }
    // A single quote inside the value would end the literal early; the only
    // portable escape dotenv accepts is to fall back to double quotes.
    if value.contains('\'') {
        let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
        format!("{key}=\"{escaped}\"")
    } else {
        format!("{key}='{value}'")
    }
}

/// The key of a `KEY=…` assignment line, ignoring comments, blanks and the
/// `export ` prefix dotenv also accepts.
fn key_of(line: &str) -> Option<&str> {
    let t = line.trim();
    if t.is_empty() || t.starts_with('#') {
        return None;
    }
    let t = t.strip_prefix("export ").unwrap_or(t).trim_start();
    let key = t.split('=').next()?.trim();
    (!key.is_empty()
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.'))
    .then_some(key)
}

fn unquote(raw: &str) -> String {
    let v = raw.trim();
    for q in ['\'', '"'] {
        if v.len() >= 2 && v.starts_with(q) && v.ends_with(q) {
            let inner = &v[1..v.len() - 1];
            return if q == '"' {
                inner.replace("\\\"", "\"").replace("\\\\", "\\")
            } else {
                inner.to_string()
            };
        }
    }
    // A trailing `# comment` is only a comment on an unquoted value.
    match v.split_once(" #") {
        Some((head, _)) => head.trim().to_string(),
        None => v.to_string(),
    }
}

pub fn read(path: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Ok(raw) = std::fs::read_to_string(path) else {
        return out;
    };
    for line in raw.lines() {
        if let Some(key) = key_of(line) {
            if let Some((_, value)) = line.split_once('=') {
                out.insert(key.to_string(), unquote(value));
            }
        }
    }
    out
}

pub fn get(path: &Path, key: &str) -> Option<String> {
    read(path).remove(key)
}

/// Upsert `vars` in place. Existing assignments are rewritten where they sit so
/// the file keeps its order and comments; new keys are appended.
pub fn merge(path: &Path, vars: &BTreeMap<String, String>) -> Result<(), AppError> {
    let raw = std::fs::read_to_string(path).unwrap_or_default();
    let mut pending: BTreeMap<&str, &String> =
        vars.iter().map(|(k, v)| (k.as_str(), v)).collect();

    let mut lines: Vec<String> = Vec::new();
    for line in raw.lines() {
        match key_of(line).and_then(|k| pending.remove_entry(k)) {
            Some((key, value)) => lines.push(render(key, value)),
            None => lines.push(line.to_string()),
        }
    }
    for (key, value) in pending {
        lines.push(render(key, value));
    }

    let mut body = lines.join("\n");
    if !body.is_empty() {
        body.push('\n');
    }
    write_atomic(path, &body)
}

/// Drop `keys` from the file. Deletes it outright once nothing but whitespace
/// is left, so an undo does not leave an empty file we created lying around —
/// but a file still holding the user's own vars or comments stays.
pub fn remove_keys(path: &Path, keys: &[&str]) -> Result<(), AppError> {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Ok(());
    };
    let kept: Vec<&str> = raw
        .lines()
        .filter(|line| !key_of(line).is_some_and(|k| keys.contains(&k)))
        .collect();
    if kept.iter().all(|l| l.trim().is_empty()) {
        std::fs::remove_file(path)?;
        return Ok(());
    }
    let mut body = kept.join("\n");
    body.push('\n');
    write_atomic(path, &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        (dir, path)
    }

    #[test]
    fn merge_keeps_comments_order_and_foreign_keys() {
        let (_keep, path) = tmp();
        std::fs::write(
            &path,
            "# my notes\nFOO=bar\n\nGEMINI_API_KEY=old\nexport BAZ=qux\n",
        )
        .unwrap();

        let mut vars = BTreeMap::new();
        vars.insert("GEMINI_API_KEY".to_string(), "new".to_string());
        vars.insert(
            "GOOGLE_GEMINI_BASE_URL".to_string(),
            "https://proxy.test".to_string(),
        );
        merge(&path, &vars).unwrap();

        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.starts_with("# my notes\nFOO=bar\n\n"), "got:\n{body}");
        assert!(body.contains("GEMINI_API_KEY=new"));
        assert!(!body.contains("GEMINI_API_KEY=old"));
        assert!(body.contains("export BAZ=qux"), "foreign keys survive");
        assert!(body.contains("GOOGLE_GEMINI_BASE_URL=https://proxy.test"));
        assert!(body.ends_with('\n'));

        let parsed = read(&path);
        assert_eq!(parsed.get("GEMINI_API_KEY").unwrap(), "new");
        assert_eq!(parsed.get("FOO").unwrap(), "bar");
        assert_eq!(parsed.get("BAZ").unwrap(), "qux");
    }

    #[test]
    fn creates_the_file_when_absent() {
        let (_keep, path) = tmp();
        let mut vars = BTreeMap::new();
        vars.insert("GEMINI_API_KEY".to_string(), "tok".to_string());
        merge(&path, &vars).unwrap();
        assert_eq!(get(&path, "GEMINI_API_KEY").as_deref(), Some("tok"));
    }

    /// A token with `#` or spaces must not be re-read as a truncated value.
    #[test]
    fn awkward_values_round_trip() {
        let (_keep, path) = tmp();
        let mut vars = BTreeMap::new();
        for (k, v) in [
            ("A", "has space"),
            ("B", "has#hash"),
            ("C", "it's quoted"),
            ("D", ""),
            ("E", "plain-tok.123:/x"),
        ] {
            vars.insert(k.to_string(), v.to_string());
        }
        merge(&path, &vars).unwrap();
        let parsed = read(&path);
        for (k, v) in [
            ("A", "has space"),
            ("B", "has#hash"),
            ("C", "it's quoted"),
            ("D", ""),
            ("E", "plain-tok.123:/x"),
        ] {
            assert_eq!(parsed.get(k).map(String::as_str), Some(v), "key {k}");
        }
    }

    #[test]
    fn a_quoted_existing_value_reads_back_unquoted() {
        let (_keep, path) = tmp();
        std::fs::write(&path, "X='lit #1'\nY=\"esc\\\"q\"\nZ=bare # trailing\n").unwrap();
        let parsed = read(&path);
        assert_eq!(parsed.get("X").unwrap(), "lit #1");
        assert_eq!(parsed.get("Y").unwrap(), "esc\"q");
        assert_eq!(parsed.get("Z").unwrap(), "bare");
    }
}
