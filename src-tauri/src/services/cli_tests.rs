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

#[test]
fn extra_env_writes_official_knobs_for_every_cli() {
    let (_keep, root, bk) = sandbox();
    let pair = |k: &str, v: &str| {
        let mut m = std::collections::BTreeMap::new();
        m.insert(k.into(), v.into());
        m
    };

    apply_takeover(
        &root,
        CliTarget::Codex,
        "http://127.0.0.1:15722",
        "tok",
        "c1",
        &bk,
        TakeoverOptions {
            extra_env: Some(pair("sandbox_mode", "read-only")),
            ..Default::default()
        },
    )
    .unwrap();
    let codex = read(&root, ".codex/config.toml");
    assert!(codex.contains("sandbox_mode = \"read-only\""), "{codex}");

    apply_takeover(
        &root,
        CliTarget::GrokBuild,
        "http://127.0.0.1:15722",
        "tok",
        "g1",
        &bk,
        TakeoverOptions {
            extra_env: Some(pair("ui.simple_mode", "false")),
            ..Default::default()
        },
    )
    .unwrap();
    let grok = read(&root, ".grok/config.toml");
    assert!(grok.contains("simple_mode = false"), "{grok}");

    apply_takeover(
        &root,
        CliTarget::GeminiCli,
        "http://127.0.0.1:15722",
        "tok",
        "m1",
        &bk,
        TakeoverOptions {
            extra_env: Some(pair("model.name", "gemini-3")),
            ..Default::default()
        },
    )
    .unwrap();
    let gemini: serde_json::Value =
        serde_json::from_str(&read(&root, ".gemini/settings.json")).unwrap();
    assert_eq!(gemini.pointer("/model/name").unwrap(), "gemini-3");

    apply_takeover(
        &root,
        CliTarget::OpenCode,
        "http://127.0.0.1:15722",
        "tok",
        "o1",
        &bk,
        TakeoverOptions {
            extra_env: Some(pair("small_model", "ccload/haiku")),
            ..Default::default()
        },
    )
    .unwrap();
    let oc: serde_json::Value =
        serde_json::from_str(&read(&root, ".config/opencode/opencode.json")).unwrap();
    assert_eq!(oc.pointer("/small_model").unwrap(), "ccload/haiku");
    assert!(oc.pointer("/provider/ccload").is_some());
}

/// An emptied field in the advanced form means "leave it unset". Writing `""`
/// hands Claude Code an env var that is set-but-empty and used verbatim — the
/// same reason the model tiers skip empty strings. And `CLAUDE_PID` & friends
/// are injected by the host process: typing one into the custom-key box must
/// not put it on disk, where it would fight the running CLI.
#[test]
fn claude_extra_env_skips_empty_values_and_host_owned_keys() {
    let (_keep, root, bk) = sandbox();
    write(
        &root,
        ".claude/settings.json",
        r#"{"env":{"DISABLE_TELEMETRY":"1"}}"#,
    );
    let mut extra = std::collections::BTreeMap::new();
    extra.insert("API_TIMEOUT_MS".to_string(), "1200000".to_string());
    extra.insert("CLAUDE_CODE_RETRY_WATCHDOG".to_string(), String::new());
    extra.insert("CLAUDE_PID".to_string(), "4242".to_string());
    extra.insert("CLAUDECODE".to_string(), "1".to_string());
    extra.insert("ANTHROPIC_AUTH_TOKEN".to_string(), "hijacked".to_string());
    apply_takeover(
        &root,
        CliTarget::ClaudeCode,
        "http://127.0.0.1:15722",
        "tok",
        "e1",
        &bk,
        TakeoverOptions {
            extra_env: Some(extra),
            ..Default::default()
        },
    )
    .unwrap();

    let doc: serde_json::Value =
        serde_json::from_str(&read(&root, ".claude/settings.json")).unwrap();
    assert_eq!(doc["env"]["API_TIMEOUT_MS"], "1200000");
    assert!(
        doc["env"].get("CLAUDE_CODE_RETRY_WATCHDOG").is_none(),
        "an emptied field must not land as an empty string"
    );
    assert!(
        doc["env"].get("CLAUDE_PID").is_none(),
        "host-injected identity must never be written"
    );
    assert!(doc["env"].get("CLAUDECODE").is_none());
    assert_eq!(
        doc["env"]["ANTHROPIC_AUTH_TOKEN"], "tok",
        "extra_env cannot overwrite the token takeover just wrote"
    );
    assert_eq!(
        doc["env"]["DISABLE_TELEMETRY"], "1",
        "skipping is not deleting: what the user already had stays"
    );
}

/// `model` is dedicated to Codex's own TakeoverOptions slot. Grok has no such
/// slot, so its `model` key is the user's to set.
#[test]
fn grok_extra_env_keeps_a_custom_model_key() {
    let (_keep, root, bk) = sandbox();
    let mut extra = std::collections::BTreeMap::new();
    extra.insert("model".to_string(), "grok-4-fast".to_string());
    apply_takeover(
        &root,
        CliTarget::GrokBuild,
        "http://127.0.0.1:15722",
        "tok",
        "g2",
        &bk,
        TakeoverOptions {
            extra_env: Some(extra),
            ..Default::default()
        },
    )
    .unwrap();
    let raw = read(&root, ".grok/config.toml");
    let doc: toml_edit::DocumentMut = raw.parse().unwrap();
    assert_eq!(doc["model"].as_str(), Some("grok-4-fast"), "{raw}");
}

fn takeover_id(root: &ConfigRoot, bk: &BackupStore, target: CliTarget, id: &str) -> String {
    apply_takeover(root, target, "http://127.0.0.1:15722", "tok", id, bk, TakeoverOptions::default())
        .unwrap()
        .backup_id
}

// ---------------------------------------------------------------------------
// 强制 fallback 模型
//
// `fallbackModel` / `switchModelsOnFlag` 是 settings.json 的**顶层**键，不是
// env。写错层级的话 Claude Code 一个字都不会读，而配置看起来完全正常 ——
// 这类错误只有测试抓得住。
// ---------------------------------------------------------------------------

#[test]
fn claude_fallback_chain_lands_at_top_level_not_env() {
    let (_keep, root, bk) = sandbox();
    apply_takeover(
        &root,
        CliTarget::ClaudeCode,
        "http://127.0.0.1:15722",
        "tok",
        "f1",
        &bk,
        TakeoverOptions {
            fallback_models: Some(vec!["fable-5".into(), "kimi-k3".into()]),
            ..Default::default()
        },
    )
    .unwrap();

    let raw = read(&root, ".claude/settings.json");
    let doc: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        doc.pointer("/fallbackModel").unwrap(),
        &serde_json::json!(["fable-5", "kimi-k3"]),
        "{raw}"
    );
    assert!(
        doc.pointer("/env/fallbackModel").is_none(),
        "顶层键写进 env 等于没写：{raw}"
    );
}

/// Claude Code 去重后只认 3 个。界面上能排 5 个，但写进去必须是它真会用的
/// 那 3 个，否则用户以为后两个也在生效。
#[test]
fn claude_fallback_chain_is_deduped_then_capped() {
    let (_keep, root, bk) = sandbox();
    apply_takeover(
        &root,
        CliTarget::ClaudeCode,
        "http://127.0.0.1:15722",
        "tok",
        "f2",
        &bk,
        TakeoverOptions {
            fallback_models: Some(vec![
                "  fable-5 ".into(),
                "fable-5".into(), // 重复
                "".into(),        // 空槽
                "kimi-k3".into(),
                "glm-5.3".into(),
                "opus-5".into(), // 第 4 个，被截掉
            ]),
            ..Default::default()
        },
    )
    .unwrap();

    let doc: serde_json::Value =
        serde_json::from_str(&read(&root, ".claude/settings.json")).unwrap();
    assert_eq!(
        doc.pointer("/fallbackModel").unwrap(),
        &serde_json::json!(["fable-5", "kimi-k3", "glm-5.3"]),
    );
}

/// 留空 = 不改。和其它每个可选项一条规矩：接管不该把用户手写的链清掉。
#[test]
fn claude_empty_fallback_leaves_existing_chain_alone() {
    let (_keep, root, bk) = sandbox();
    write(
        &root,
        ".claude/settings.json",
        r#"{"fallbackModel":["mine"],"switchModelsOnFlag":false}"#,
    );
    takeover(&root, &bk, CliTarget::ClaudeCode);

    let doc: serde_json::Value =
        serde_json::from_str(&read(&root, ".claude/settings.json")).unwrap();
    assert_eq!(doc.pointer("/fallbackModel").unwrap(), &serde_json::json!(["mine"]));
    assert_eq!(doc.pointer("/switchModelsOnFlag").unwrap(), &serde_json::json!(false));
}

/// 全是空串时也不写 —— 三个输入框都留空和「没填」是同一件事，不该产出
/// 一个空数组把 Claude Code 的默认行为关掉。
#[test]
fn claude_all_blank_fallback_writes_nothing() {
    let (_keep, root, bk) = sandbox();
    apply_takeover(
        &root,
        CliTarget::ClaudeCode,
        "http://127.0.0.1:15722",
        "tok",
        "f3",
        &bk,
        TakeoverOptions {
            fallback_models: Some(vec!["".into(), "  ".into()]),
            ..Default::default()
        },
    )
    .unwrap();

    let doc: serde_json::Value =
        serde_json::from_str(&read(&root, ".claude/settings.json")).unwrap();
    assert!(doc.pointer("/fallbackModel").is_none());
}

#[test]
fn claude_switch_models_on_flag_is_written() {
    let (_keep, root, bk) = sandbox();
    apply_takeover(
        &root,
        CliTarget::ClaudeCode,
        "http://127.0.0.1:15722",
        "tok",
        "f4",
        &bk,
        TakeoverOptions {
            switch_models_on_flag: Some(false),
            ..Default::default()
        },
    )
    .unwrap();

    let doc: serde_json::Value =
        serde_json::from_str(&read(&root, ".claude/settings.json")).unwrap();
    assert_eq!(doc.pointer("/switchModelsOnFlag").unwrap(), &serde_json::json!(false));
}

// --- takeover must land where the CLI actually reads ----------------------
//
// Every case below is a bug we shipped: the write succeeded, the file looked
// right, and the CLI went on using its own OAuth login because nothing read
// that key. Endpoint-in-the-config is not evidence of takeover.

/// Codex resolves `env_key` with std::env::var and deserializes auth.json into
/// a fixed struct, so a token parked there is invisible. Verified live against
/// codex-cli 0.144.6: `ERROR: Missing environment variable: CCLOAD_API_KEY.`
#[test]
fn codex_carries_the_token_in_config_not_auth_json() {
    let (_keep, root, bk) = sandbox();
    write(&root, ".codex/auth.json", r#"{"CCLOAD_API_KEY":"stale","auth_mode":"chatgpt"}"#);
    takeover(&root, &bk, CliTarget::Codex);

    let toml = read(&root, ".codex/config.toml");
    assert!(
        toml.contains("experimental_bearer_token = \"tok\""),
        "token must travel in the provider table:\n{toml}"
    );
    assert!(
        !toml.contains("env_key"),
        "a leftover env_key makes Codex demand an unset variable:\n{toml}"
    );
    let auth: serde_json::Value = serde_json::from_str(&read(&root, ".codex/auth.json")).unwrap();
    assert!(auth.get("CCLOAD_API_KEY").is_none(), "dead secret must be swept");
    assert_eq!(auth["auth_mode"], "chatgpt", "codex's own keys stay put");
    assert_eq!(
        crate::services::cli_config::current_token(&root, CliTarget::Codex).as_deref(),
        Some("tok")
    );
}

/// Takeover must not create auth.json just to clean it.
#[test]
fn codex_leaves_a_missing_auth_json_alone() {
    let (_keep, root, bk) = sandbox();
    takeover(&root, &bk, CliTarget::Codex);
    assert!(!root.join(".codex/auth.json").exists());
}

/// Gemini's settings.json has no `env` block — `loadEnvironment()` only reads a
/// dotenv file. Verified against gemini-cli 0.50.0.
#[test]
fn gemini_env_lands_in_dotenv_and_auth_type_is_switched() {
    let (_keep, root, bk) = sandbox();
    write(
        &root,
        ".gemini/settings.json",
        r#"{"security":{"auth":{"selectedType":"oauth-personal"}},"mcpServers":{"x":{"command":"y"}}}"#,
    );
    takeover(&root, &bk, CliTarget::GeminiCli);

    let env = read(&root, ".gemini/.env");
    assert!(env.contains("GOOGLE_GEMINI_BASE_URL=http://127.0.0.1:15722"), "{env}");
    assert!(env.contains("GEMINI_API_KEY=tok"), "{env}");

    let settings: serde_json::Value =
        serde_json::from_str(&read(&root, ".gemini/settings.json")).unwrap();
    assert_eq!(
        settings.pointer("/security/auth/selectedType").unwrap(),
        "gemini-api-key",
        "oauth-personal outranks the base-URL sniffing and would keep the Google login; \
         `gateway` is inferred internally but validateAuthMethod rejects it"
    );
    assert_eq!(settings.pointer("/mcpServers/x/command").unwrap(), "y");
    assert_eq!(
        current_endpoint(&root, CliTarget::GeminiCli).as_deref(),
        Some("http://127.0.0.1:15722")
    );
}

/// An older build wrote these into settings.json, where nothing reads them.
#[test]
fn gemini_migrates_a_legacy_settings_env_block() {
    let (_keep, root, bk) = sandbox();
    write(
        &root,
        ".gemini/settings.json",
        r#"{"env":{"GEMINI_API_KEY":"old","FIGMA_TOKEN":"keep-me"},"ui":{"theme":"dark"}}"#,
    );
    takeover(&root, &bk, CliTarget::GeminiCli);

    let env = read(&root, ".gemini/.env");
    assert!(env.contains("FIGMA_TOKEN=keep-me"), "user's own var must survive:\n{env}");
    assert!(env.contains("GEMINI_API_KEY=tok"), "and be refreshed, not duplicated:\n{env}");
    assert!(!env.contains("old"));

    let settings: serde_json::Value =
        serde_json::from_str(&read(&root, ".gemini/settings.json")).unwrap();
    assert!(settings.get("env").is_none(), "the dead block must not linger");
    assert_eq!(settings.pointer("/ui/theme").unwrap(), "dark");
}

/// Advanced-form env knobs have to follow the credential into `.env`.
#[test]
fn gemini_extra_env_splits_between_dotenv_and_settings() {
    let (_keep, root, bk) = sandbox();
    let mut extra = std::collections::BTreeMap::new();
    extra.insert("GEMINI_MODEL".to_string(), "gemini-3-pro".to_string());
    extra.insert("ui.theme".to_string(), "dark".to_string());
    apply_takeover(
        &root,
        CliTarget::GeminiCli,
        "http://127.0.0.1:15722",
        "tok",
        "g1",
        &bk,
        TakeoverOptions { extra_env: Some(extra), ..Default::default() },
    )
    .unwrap();

    assert!(read(&root, ".gemini/.env").contains("GEMINI_MODEL=gemini-3-pro"));
    let settings: serde_json::Value =
        serde_json::from_str(&read(&root, ".gemini/settings.json")).unwrap();
    assert_eq!(settings.pointer("/ui/theme").unwrap(), "dark");
    assert!(settings.get("env").is_none(), "env keys must not go back into the JSON");
}

/// The takeover used to replace `provider.ccload` wholesale, wiping the catalog
/// 模型导入 had built and orphaning the top-level `model` that pointed into it.
#[test]
fn opencode_takeover_keeps_the_imported_model_catalog() {
    let (_keep, root, bk) = sandbox();
    write(
        &root,
        ".config/opencode/opencode.json",
        r#"{"model":"ccload/opus","provider":{"ccload":{"npm":"@ai-sdk/openai-compatible","name":"ccLoad","models":{"opus":{"name":"Opus"},"haiku":{"name":"Haiku"}},"options":{"baseURL":"http://old/v1","apiKey":"old"}}}}"#,
    );
    apply_takeover(
        &root,
        CliTarget::OpenCode,
        "http://127.0.0.1:15722",
        "new-tok",
        "o1",
        &bk,
        TakeoverOptions::default(),
    )
    .unwrap();

    let oc: serde_json::Value =
        serde_json::from_str(&read(&root, ".config/opencode/opencode.json")).unwrap();
    let models = oc.pointer("/provider/ccload/models").expect("catalog was wiped");
    assert!(models.get("opus").is_some() && models.get("haiku").is_some());
    assert_eq!(oc.pointer("/model").unwrap(), "ccload/opus", "an existing choice is the user's");
    assert_eq!(
        oc.pointer("/provider/ccload/options/baseURL").unwrap(),
        "http://127.0.0.1:15722/v1"
    );
    assert_eq!(oc.pointer("/provider/ccload/options/apiKey").unwrap(), "new-tok");
}

/// A provider nobody selects routes nothing — fill the gap when we can.
#[test]
fn opencode_defaults_to_ccload_only_when_no_model_is_chosen() {
    let (_keep, root, bk) = sandbox();
    write(
        &root,
        ".config/opencode/opencode.json",
        r#"{"provider":{"ccload":{"models":{"auto":{"name":"Auto"}}}}}"#,
    );
    takeover(&root, &bk, CliTarget::OpenCode);
    let oc: serde_json::Value =
        serde_json::from_str(&read(&root, ".config/opencode/opencode.json")).unwrap();
    assert_eq!(oc.pointer("/model").unwrap(), "ccload/auto");

    // With no catalog there is nothing to point at; do not invent one.
    let (_keep2, root2, bk2) = sandbox();
    takeover(&root2, &bk2, CliTarget::OpenCode);
    let bare: serde_json::Value =
        serde_json::from_str(&read(&root2, ".config/opencode/opencode.json")).unwrap();
    assert!(bare.get("model").is_none());
}

/// `.env` is created by the takeover, so a restore has to delete it again —
/// leaving it behind would keep Gemini pointed at the kernel after an undo.
#[test]
fn gemini_restore_removes_the_dotenv_the_takeover_created() {
    let (_keep, root, bk) = sandbox();
    write(&root, ".gemini/settings.json", r#"{"ui":{"theme":"dark"}}"#);
    let id = takeover_id(&root, &bk, CliTarget::GeminiCli, "gr1");
    assert!(root.join(".gemini/.env").exists());

    bk.restore(&root, &id).unwrap();
    assert!(
        !root.join(".gemini/.env").exists(),
        "restore must delete what the takeover created"
    );
    assert_eq!(read(&root, ".gemini/settings.json"), r#"{"ui":{"theme":"dark"}}"#);
}

/// Restoring a backup taken before `.env` joined the target's file list must
/// still undo the takeover — the snapshot cannot replay a file it never saw,
/// so the redirect would otherwise outlive the restore and keep Gemini pointed
/// at the kernel while settings.json claims the Google login is back.
#[test]
fn restoring_a_pre_dotenv_backup_still_undoes_the_redirect() {
    let (_keep, root, bk) = sandbox();
    write(
        &root,
        ".gemini/settings.json",
        r#"{"security":{"auth":{"selectedType":"oauth-personal"}}}"#,
    );
    // A snapshot from before `.gemini/.env` was a known path.
    let legacy = bk
        .snapshot_paths(&root, CliTarget::GeminiCli, "legacy", "takeover", &[".gemini/settings.json"])
        .unwrap();
    takeover(&root, &bk, CliTarget::GeminiCli);
    assert!(root.join(".gemini/.env").exists());

    bk.restore(&root, &legacy.id).unwrap();
    let settings: serde_json::Value =
        serde_json::from_str(&read(&root, ".gemini/settings.json")).unwrap();
    assert_eq!(settings.pointer("/security/auth/selectedType").unwrap(), "oauth-personal");
    assert!(
        !root.join(".gemini/.env").exists(),
        "the redirect survived a restore"
    );
}

/// ...but a `.env` the user also keeps their own vars in must survive; only the
/// two keys takeover owns get swept.
#[test]
fn healing_keeps_the_users_own_env_vars() {
    let (_keep, root, bk) = sandbox();
    write(&root, ".gemini/.env", "# mine\nFIGMA_TOKEN=keep\n");
    write(&root, ".gemini/settings.json", "{}");
    let legacy = bk
        .snapshot_paths(&root, CliTarget::GeminiCli, "legacy2", "takeover", &[".gemini/settings.json"])
        .unwrap();
    takeover(&root, &bk, CliTarget::GeminiCli);

    bk.restore(&root, &legacy.id).unwrap();
    let env = read(&root, ".gemini/.env");
    assert!(env.contains("FIGMA_TOKEN=keep"), "{env}");
    assert!(env.contains("# mine"), "comments survive too:\n{env}");
    assert!(!env.contains("GEMINI_API_KEY"), "{env}");
    assert!(!env.contains("GOOGLE_GEMINI_BASE_URL"), "{env}");
}

/// 实测发现磁盘上 Codex 退回了 `auth_mode: chatgpt`、Gemini 的 `.env` 是空的，
/// 但写入器的单测全绿 —— 说明配置是**被 CLI 自己覆盖回去的**，不是我们没写对。
/// 所以接管必须幂等：对着一份「已经被冲掉」的配置再写一次要能重新接管，而不是
/// 因为「看起来配过」就跳过。
#[test]
fn codex_takeover_recovers_a_config_the_cli_reverted() {
    let (_keep, root, bk) = sandbox();
    takeover(&root, &bk, CliTarget::Codex);
    let first = read(&root, ".codex/config.toml");
    assert!(first.contains("[model_providers.ccload]"));

    // 模拟 Codex 把配置冲回官方登录：provider 段没了，auth.json 退回 chatgpt。
    let reverted: String = first
        .lines()
        .take_while(|l| !l.starts_with("[model_providers"))
        .collect::<Vec<_>>()
        .join("\n");
    write(&root, ".codex/config.toml", &reverted);
    write(
        &root,
        ".codex/auth.json",
        r#"{"auth_mode":"chatgpt","OPENAI_API_KEY":null}"#,
    );

    takeover(&root, &bk, CliTarget::Codex);
    let again = read(&root, ".codex/config.toml");
    assert!(
        again.contains("[model_providers.ccload]"),
        "被 CLI 冲掉之后再写一次必须能重新接管，实际：{again}"
    );
    assert!(again.contains("wire_api = \"responses\""));
    assert!(current_endpoint(&root, CliTarget::Codex).is_some());
}

/// Gemini 的凭据在 `.env` 里。实测那个文件 size=0 —— 空文件不能被当成
/// 「已经配好了」，重写必须补回去。
#[test]
fn gemini_takeover_refills_an_emptied_dotenv() {
    let (_keep, root, bk) = sandbox();
    takeover(&root, &bk, CliTarget::GeminiCli);
    assert!(read(&root, ".gemini/.env").contains("GEMINI_API_KEY"));

    write(&root, ".gemini/.env", "");

    takeover(&root, &bk, CliTarget::GeminiCli);
    let refilled = read(&root, ".gemini/.env");
    assert!(
        refilled.contains("GEMINI_API_KEY") && refilled.contains("GOOGLE_GEMINI_BASE_URL"),
        "清空后重写必须补回凭据，实际：{refilled:?}"
    );
}

/// 接管地址由「开关」决定，不是由「代理跑没跑」决定。
///
/// 这两件事很容易混：代理进程一直在跑，但用户可能不想让 CLI 走它。开关关着时
/// 写进配置的必须是内核地址，否则用户以为关了、实际还在被代理转发。
#[test]
fn takeover_endpoint_follows_the_switch_not_the_proxy() {
    let (_keep, root, bk) = sandbox();

    // 关：写内核地址。
    apply_takeover(
        &root,
        CliTarget::ClaudeCode,
        "https://kernel:8992",
        "tok",
        "s1",
        &bk,
        TakeoverOptions::default(),
    )
    .unwrap();
    assert_eq!(
        current_endpoint(&root, CliTarget::ClaudeCode).as_deref(),
        Some("https://kernel:8992")
    );

    // 开：写代理地址。同一份配置被覆盖，不是并存。
    apply_takeover(
        &root,
        CliTarget::ClaudeCode,
        "http://127.0.0.1:15777",
        "tok",
        "s2",
        &bk,
        TakeoverOptions::default(),
    )
    .unwrap();
    assert_eq!(
        current_endpoint(&root, CliTarget::ClaudeCode).as_deref(),
        Some("http://127.0.0.1:15777")
    );

    // 再关回去：必须能切回内核，不能卡在代理上。
    apply_takeover(
        &root,
        CliTarget::ClaudeCode,
        "https://kernel:8992",
        "tok",
        "s3",
        &bk,
        TakeoverOptions::default(),
    )
    .unwrap();
    assert_eq!(
        current_endpoint(&root, CliTarget::ClaudeCode).as_deref(),
        Some("https://kernel:8992")
    );
}

/// 切换地址之后，preview 必须报「需要重写」——否则用户点了开关却看不出
/// 还有一步要做，会以为已经生效了。
#[test]
fn switching_the_endpoint_makes_preview_report_not_active() {
    let (_keep, root, bk) = sandbox();
    apply_takeover(
        &root,
        CliTarget::ClaudeCode,
        "https://kernel:8992",
        "tok",
        "s1",
        &bk,
        TakeoverOptions::default(),
    )
    .unwrap();

    // 对着内核地址预览：已生效。
    let p = preview(&root, CliTarget::ClaudeCode, "https://kernel:8992", Some("tok"));
    assert!(p.already_active);

    // 换成代理地址预览：没生效，要重写。
    let p = preview(&root, CliTarget::ClaudeCode, "http://127.0.0.1:15777", Some("tok"));
    assert!(!p.already_active, "地址变了就该报「未生效」");
    assert!(p.exists, "文件是在的，只是指向不对");
}
