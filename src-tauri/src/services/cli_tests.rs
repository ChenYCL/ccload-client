//! Takeover tests. These run entirely inside a temp sandbox root so the real
//! `~/.claude/settings.json` is never touched.

use crate::services::cli_advanced::TakeoverOptions;
use crate::services::cli_backup::BackupStore;
use crate::services::cli_config::{apply_takeover, current_endpoint, preview};
use crate::services::cli_types::{CliTarget, ConfigRoot};

fn sandbox() -> (tempfile::TempDir, ConfigRoot, BackupStore) {
    let dir = tempfile::tempdir().unwrap();
    let root = ConfigRoot::sandbox(dir.path().to_path_buf());
    let bk = BackupStore::new(dir.path().join(".ccload-test-backups"));
    (dir, root, bk)
}

fn write(root: &ConfigRoot, rel: &str, body: &str) {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

fn read(root: &ConfigRoot, rel: &str) -> String {
    std::fs::read_to_string(root.join(rel)).unwrap()
}

fn takeover(root: &ConfigRoot, bk: &BackupStore, target: CliTarget) {
    apply_takeover(root, target, "http://127.0.0.1:15722", "tok", "s", bk, TakeoverOptions::default()).unwrap();
}

// ---------------------------------------------------------------------------

#[test]
fn claude_takeover_preserves_unrelated_settings() {
    let (_keep, root, bk) = sandbox();
    write(
        &root,
        ".claude/settings.json",
        r#"{"env":{"ANTHROPIC_BASE_URL":"http://127.0.0.1:15721",
                   "ANTHROPIC_API_KEY":"stale-key",
                   "API_TIMEOUT_MS":"600000"},
            "statusLine":{"type":"command"},
            "teammateMode":true}"#,
    );
    takeover(&root, &bk, CliTarget::ClaudeCode);
    let doc: serde_json::Value =
        serde_json::from_str(&read(&root, ".claude/settings.json")).unwrap();
    assert_eq!(doc["env"]["ANTHROPIC_BASE_URL"], "http://127.0.0.1:15722");
    assert_eq!(doc["env"]["ANTHROPIC_AUTH_TOKEN"], "tok");
    assert!(doc["env"].get("ANTHROPIC_API_KEY").is_none());
    assert_eq!(doc["env"]["API_TIMEOUT_MS"], "600000");
    assert_eq!(doc["statusLine"]["type"], "command");
    assert_eq!(doc["teammateMode"], true);
}

#[test]
fn snapshot_captures_all_target_files() {
    let (_keep, root, bk) = sandbox();
    write(&root, ".claude/settings.json", r#"{"env":{"X":"1"}}"#);
    let result = apply_takeover(
        &root,
        CliTarget::ClaudeCode,
        "http://x",
        "t",
        "snap1",
        &bk,
        TakeoverOptions::default(),
    )
    .unwrap();
    let entries = bk.list(Some(CliTarget::ClaudeCode)).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, result.backup_id);
    assert!(entries[0].pristine, "first snapshot must be marked pristine");
    assert!(entries[0].files[0].existed);
}

#[test]
fn restore_returns_file_to_original_content() {
    let (_keep, root, bk) = sandbox();
    let original = r#"{"env":{"ANTHROPIC_BASE_URL":"http://old-proxy","X":"kept"}}"#;
    write(&root, ".claude/settings.json", original);
    let result = takeover_id(&root, &bk, CliTarget::ClaudeCode, "r1");
    // After takeover the file is different.
    let after = read(&root, ".claude/settings.json");
    assert_ne!(after, original);
    // Restore brings it back exactly.
    bk.restore(&root, &result).unwrap();
    assert_eq!(read(&root, ".claude/settings.json"), original);
}

#[test]
fn restore_deletes_file_that_did_not_exist_before() {
    let (_keep, root, bk) = sandbox();
    // settings.json does not exist before the first takeover.
    let result = takeover_id(&root, &bk, CliTarget::ClaudeCode, "r2");
    assert!(root.join(".claude/settings.json").exists());
    bk.restore(&root, &result).unwrap();
    assert!(
        !root.join(".claude/settings.json").exists(),
        "restore must delete what the takeover created"
    );
}

#[test]
fn preview_does_not_write() {
    let (_keep, root, _bk) = sandbox();
    write(&root, ".claude/settings.json", r#"{"env":{}}"#);
    let before = read(&root, ".claude/settings.json");
    let p = preview(&root, CliTarget::ClaudeCode, "http://127.0.0.1:15722", None);
    assert!(!p.already_active);
    assert_eq!(read(&root, ".claude/settings.json"), before);
}

/// A config pointing at the right kernel but carrying a token minted for a
/// *different* one 401s on every call. Endpoint equality alone must not report
/// "already taken over", or the UI tells the user everything is fine while the
/// CLI is broken.
#[test]
fn stale_token_is_not_already_active() {
    let (_keep, root, bk) = sandbox();
    let base = "http://127.0.0.1:15722";
    apply_takeover(
        &root,
        CliTarget::ClaudeCode,
        base,
        "tok-old",
        "s1",
        &bk,
        TakeoverOptions::default(),
    )
    .unwrap();

    let same = preview(&root, CliTarget::ClaudeCode, base, Some("tok-old"));
    assert!(same.already_active, "matching endpoint + token is active");
    assert!(!same.token_stale);

    let rotated = preview(&root, CliTarget::ClaudeCode, base, Some("tok-new"));
    assert!(!rotated.already_active, "a rotated token is not active");
    assert!(rotated.token_stale, "and must be reported as stale");
}

/// A remote URL pasted with surrounding whitespace still points at the same
/// kernel; the takeover must not be reported as pending over cosmetics.
#[test]
fn endpoint_comparison_tolerates_whitespace_and_trailing_slash() {
    let (_keep, root, bk) = sandbox();
    apply_takeover(
        &root,
        CliTarget::ClaudeCode,
        "https://example.com:8992",
        "tok",
        "s1",
        &bk,
        TakeoverOptions::default(),
    )
    .unwrap();
    let p = preview(
        &root,
        CliTarget::ClaudeCode,
        "https://example.com:8992/",
        Some("tok"),
    );
    assert!(p.already_active, "trailing slash must not count as a change");
}

#[test]
fn codex_and_opencode_keep_user_sections() {
    let (_keep, root, bk) = sandbox();
    write(
        &root,
        ".codex/config.toml",
        "model = \"gpt-5.6\"\nsandbox_mode = \"workspace-write\"\n\n[plugins.\"docs\"]\nenabled = true\n",
    );
    write(
        &root,
        ".config/opencode/opencode.json",
        r#"{"theme":"dark","mcp":{"zread":{"type":"remote"}}}"#,
    );
    apply_takeover(&root, CliTarget::Codex, "http://127.0.0.1:15722", "tok", "s1", &bk, TakeoverOptions::default()).unwrap();
    apply_takeover(&root, CliTarget::OpenCode, "http://127.0.0.1:15722", "tok", "s2", &bk, TakeoverOptions::default()).unwrap();

    let codex = read(&root, ".codex/config.toml");
    assert!(codex.contains("sandbox_mode = \"workspace-write\""));
    assert!(codex.contains("[plugins.\"docs\"]"));
    assert!(codex.contains("model_provider = \"ccload\""));
    assert_eq!(
        current_endpoint(&root, CliTarget::Codex).as_deref(),
        Some("http://127.0.0.1:15722/v1")
    );
    let oc: serde_json::Value =
        serde_json::from_str(&read(&root, ".config/opencode/opencode.json")).unwrap();
    assert_eq!(oc["theme"], "dark");
    assert_eq!(oc["mcp"]["zread"]["type"], "remote");
    assert_eq!(
        oc["provider"]["ccload"]["options"]["baseURL"],
        "http://127.0.0.1:15722/v1"
    );
}

#[test]
fn endpoint_suffix_matches_protocol_family() {
    let (_keep, root, _bk) = sandbox();
    let base = "http://127.0.0.1:15722";
    for (target, expected) in [
        (CliTarget::ClaudeCode, base),
        (CliTarget::GeminiCli, base),
        (CliTarget::Codex, "http://127.0.0.1:15722/v1"),
        (CliTarget::GrokBuild, "http://127.0.0.1:15722/v1"),
        (CliTarget::OpenCode, "http://127.0.0.1:15722/v1"),
    ] {
        assert_eq!(preview(&root, target, base, None).next_endpoint, expected);
    }
}

#[test]
fn codex_writes_responses_wire_api_and_model_knobs() {
    let (_keep, root, bk) = sandbox();
    apply_takeover(
        &root,
        CliTarget::Codex,
        "http://127.0.0.1:15722",
        "tok",
        "s1",
        &bk,
        TakeoverOptions {
            codex_model: Some("kimi-k3".into()),
            codex_reasoning_effort: Some("high".into()),
            codex_context_window: Some(262_144),
            ..Default::default()
        },
    )
    .unwrap();
    let codex = read(&root, ".codex/config.toml");
    // Official config-reference: `responses` is the only supported wire API.
    assert!(codex.contains("wire_api = \"responses\""), "{codex}");
    assert!(codex.contains("model = \"kimi-k3\""));
    assert!(codex.contains("model_reasoning_effort = \"high\""));
    assert!(codex.contains("model_context_window = 262144"));
}

fn takeover_id(root: &ConfigRoot, bk: &BackupStore, target: CliTarget, id: &str) -> String {
    apply_takeover(root, target, "http://127.0.0.1:15722", "tok", id, bk, TakeoverOptions::default())
        .unwrap()
        .backup_id
}
