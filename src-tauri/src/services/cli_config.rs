//! Point AI coding CLIs at our local kernel by rewriting their config files.
//!
//! Safety: atomic writes, timestamped backups, merge-not-replace, optional
//! sandbox root so a dev build cannot touch a live `~/.claude/settings.json`.

use serde_json::Value;

use crate::error::AppError;
use crate::services::cli_advanced::{
    blocked_for_user, is_env_key, merge_extra_json, merge_extra_toml, normalize_fallback_models,
    TakeoverOptions,
};
use crate::services::cli_dotenv;
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
        CliTarget::GeminiCli => {
            cli_dotenv::get(&root.join(GEMINI_ENV), "GOOGLE_GEMINI_BASE_URL")
        }
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
        CliTarget::GeminiCli => cli_dotenv::get(&root.join(GEMINI_ENV), "GEMINI_API_KEY"),
        CliTarget::OpenCode => read_json(&root.join(".config/opencode/opencode.json"))
            .ok()?
            .pointer("/provider/ccload/options/apiKey")?
            .as_str()
            .map(str::to_string),
        CliTarget::Codex => toml_string_at(
            &root.join(".codex/config.toml"),
            &["model_providers", "ccload", "experimental_bearer_token"],
        ),
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
        current_model: current_model(root, target),
    }
}

/// The model this CLI will send, for the takeover card's combo box.
pub(crate) fn current_model(root: &ConfigRoot, target: CliTarget) -> Option<String> {
    match target {
        CliTarget::ClaudeCode => read_json(&root.join(".claude/settings.json"))
            .ok()?
            .pointer("/env/ANTHROPIC_MODEL")?
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        CliTarget::Codex => toml_string_at(&root.join(".codex/config.toml"), &["model"])
            .filter(|s| !s.trim().is_empty()),
        CliTarget::GeminiCli => {
            let from_settings = read_json(&root.join(".gemini/settings.json"))
                .ok()
                .and_then(|d| {
                    d.pointer("/model/name")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                });
            from_settings.or_else(|| {
                cli_dotenv::get(&root.join(GEMINI_ENV), "GEMINI_MODEL")
                    .filter(|s| !s.trim().is_empty())
            })
        }
        CliTarget::GrokBuild => cli_grok::current_model(root),
        CliTarget::OpenCode => read_json(&root.join(".config/opencode/opencode.json"))
            .ok()?
            .get("model")
            .and_then(Value::as_str)
            .map(|s| {
                s.strip_prefix("ccload/")
                    .unwrap_or(s)
                    .trim()
                    .to_string()
            })
            .filter(|s| !s.is_empty()),
    }
}

/// 这个 CLI 磁盘上**现在**写着多大的上下文窗口。
///
/// 给启动自愈比对用：策略算出来的数和它对不上，就说明配置停在某次旧写入上
/// （最常见的是「模型导入」当天写的那个数），该重写一遍。
/// Gemini CLI 没有这个旋钮，永远返回 None。
pub(crate) fn current_context_tokens(root: &ConfigRoot, target: CliTarget) -> Option<i64> {
    match target {
        CliTarget::ClaudeCode => read_json(&root.join(".claude/settings.json"))
            .ok()?
            .pointer("/env/CLAUDE_CODE_MAX_CONTEXT_TOKENS")?
            .as_str()?
            .trim()
            .parse()
            .ok(),
        CliTarget::Codex => {
            let raw = std::fs::read_to_string(root.join(".codex/config.toml")).ok()?;
            raw.parse::<toml_edit::DocumentMut>()
                .ok()?
                .get("model_context_window")?
                .as_integer()
        }
        CliTarget::OpenCode => {
            let doc = read_json(&root.join(".config/opencode/opencode.json")).ok()?;
            let alias = current_model(root, target)?;
            doc.pointer(&format!("/provider/ccload/models/{alias}/limit/context"))?
                .as_i64()
        }
        CliTarget::GrokBuild => cli_grok::current_context_tokens(root),
        CliTarget::GeminiCli => None,
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
                // 上下文窗口总控。Claude Code 的窗口是**全局一个键**，不是
                // per-model —— 换模型不跟着改，就会留着上次导入写的那个数（症状：
                // 明明换成了 1M 的模型，界面还显示 200k，四分之一处就开始压缩）。
                if let Some(w) = opts.context_tokens.filter(|n| *n > 0) {
                    env.insert(
                        "CLAUDE_CODE_MAX_CONTEXT_TOKENS".into(),
                        Value::String(w.to_string()),
                    );
                    // 网关别名不在官方目录里，不解除强制校验的话上面那个数会被
                    // Claude Code 按「未知模型」重新夹回去。
                    env.insert(
                        "CLAUDE_CODE_DISABLE_UNKNOWN_MODEL_WINDOW_ENFORCEMENT".into(),
                        Value::String("1".into()),
                    );
                    if let Some(compact) = crate::services::context_window::auto_compact_window(w) {
                        env.insert(
                            "CLAUDE_CODE_AUTO_COMPACT_WINDOW".into(),
                            Value::String(compact.to_string()),
                        );
                    }
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
            // 顶层键，不是 env —— `fallbackModel` 和 `switchModelsOnFlag` 都是
            // settings.json 的设置项，Claude Code 不从环境变量读它们。
            //
            // 只在用户给了值时才写：空链沿用「留空 = 不改」的老规矩，要清空
            // 请去配置编辑器删那一行（和其它每个可选项一致）。
            if let Some(models) = opts.fallback_models.as_deref() {
                let models = normalize_fallback_models(models);
                if !models.is_empty() {
                    let obj = doc
                        .as_object_mut()
                        .ok_or_else(|| AppError::Config("settings.json 顶层不是对象".into()))?;
                    obj.insert(
                        "fallbackModel".into(),
                        Value::Array(models.into_iter().map(Value::String).collect()),
                    );
                }
            }
            if let Some(on) = opts.switch_models_on_flag {
                let obj = doc
                    .as_object_mut()
                    .ok_or_else(|| AppError::Config("settings.json 顶层不是对象".into()))?;
                obj.insert("switchModelsOnFlag".into(), Value::Bool(on));
            }
            write_pretty_json(&path, &doc)?;
            written.push(path.display().to_string());
        }
        CliTarget::GeminiCli => {
            let settings_path = root.join(".gemini/settings.json");
            let mut doc = read_json(&settings_path)?;
            // Gemini has no `env` block in settings.json — env vars only ever
            // come from a `.env` file or the real process environment. Anything
            // an older build wrote into `env` was dead on arrival; migrate it
            // rather than leave two copies where only one is read.
            let mut vars = drain_settings_env(&mut doc);
            vars.insert("GOOGLE_GEMINI_BASE_URL".into(), endpoint.clone());
            vars.insert("GEMINI_API_KEY".into(), api_token.to_string());
            if let Some(extra) = &opts.extra_env {
                collect_gemini_env(extra, &mut vars);
                merge_extra_json(&mut doc, extra, CliTarget::GeminiCli, false)?;
            }
            if let Some(m) = opts.gemini_model.as_ref().filter(|s| !s.is_empty()) {
                vars.insert("GEMINI_MODEL".into(), m.clone());
                let model = object_at(&mut doc, "model")?;
                model.insert("name".into(), Value::String(m.clone()));
            }
            let env_path = root.join(GEMINI_ENV);
            cli_dotenv::merge(&env_path, &vars)?;
            written.push(env_path.display().to_string());

            // `selectedType` outranks the base-URL sniffing: left on
            // `oauth-personal` Gemini keeps using the Google login and never
            // looks at the key we just wrote.
            set_gemini_auth_type(&mut doc)?;
            write_pretty_json(&settings_path, &doc)?;
            written.push(settings_path.display().to_string());
        }
        CliTarget::OpenCode => {
            let path = root.join(".config/opencode/opencode.json");
            let mut doc = read_json(&path)?;
            {
                // Merge key-by-key. Replacing the whole provider entry would
                // drop `models` — the catalog 模型导入 builds — and leave the
                // top-level `model` pointing at something that no longer exists.
                let provider = object_at(&mut doc, "provider")?;
                let slot = provider
                    .entry("ccload".to_string())
                    .or_insert_with(|| Value::Object(Default::default()));
                if !slot.is_object() {
                    *slot = Value::Object(Default::default());
                }
                let ccload = slot.as_object_mut().expect("just normalized");
                ccload.insert("npm".into(), Value::String("@ai-sdk/openai-compatible".into()));
                ccload.insert("name".into(), Value::String("ccLoad".into()));
                let options = object_at(slot, "options")?;
                options.insert("baseURL".into(), Value::String(endpoint));
                options.insert("apiKey".into(), Value::String(api_token.into()));
            }
            if let Some(m) = opts.opencode_model.as_ref().filter(|s| !s.is_empty()) {
                let alias = m.strip_prefix("ccload/").unwrap_or(m).trim();
                if !alias.is_empty() {
                    ensure_opencode_catalog_stub(&mut doc, alias, opts.context_tokens)?;
                    let root_obj = doc.as_object_mut().ok_or_else(|| {
                        AppError::Config("opencode.json 顶层不是对象".into())
                    })?;
                    root_obj.insert(
                        "model".into(),
                        Value::String(format!("ccload/{alias}")),
                    );
                }
            } else {
                ensure_opencode_default_model(&mut doc)?;
                // 没换模型也要把窗口刷到**现在选中的**那条目录项上 —— 启动自愈
                // 走的就是这条路（opencode_model 为 None）。不写的话，自愈每次
                // 都判「窗口漂了」、每次都重写、每次都占一份快照，而 limit.context
                // 从来没被碰过。
                if let Some(alias) = current_model(root, target) {
                    ensure_opencode_catalog_stub(&mut doc, &alias, opts.context_tokens)?;
                }
            }
            if let Some(extra) = &opts.extra_env {
                merge_extra_json(&mut doc, extra, CliTarget::OpenCode, false)?;
            }
            write_pretty_json(&path, &doc)?;
            written.push(path.display().to_string());
        }
        CliTarget::Codex => written.extend(write_codex(root, &endpoint, api_token, &opts)?),
        CliTarget::GrokBuild => {
            written.extend(cli_grok::apply_with_model(
                root,
                &endpoint,
                api_token,
                opts.grok_model.as_deref(),
                opts.context_tokens,
            )?);
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
    if providers.get("ccload").is_none() {
        providers["ccload"] = toml_edit::table();
    }
    let entry = providers
        .get_mut("ccload")
        .and_then(|i| i.as_table_like_mut())
        .ok_or_else(|| AppError::Config("[model_providers.ccload] is not a table".into()))?;
    entry.insert("name", toml_edit::value("ccLoad"));
    entry.insert("base_url", toml_edit::value(endpoint));
    // `env_key` names an *environment variable*: Codex resolves it with
    // std::env::var and never looks in auth.json, so a key parked there just
    // gets `Missing environment variable: …` at the first request. The token
    // has to travel in the config itself.
    entry.insert("experimental_bearer_token", toml_edit::value(api_token));
    entry.remove("env_key");
    // Official config-reference: `responses` is the only supported wire API.
    // ccLoad serves the Responses protocol at the /v1 prefix, so "chat" here
    // was wrong against current Codex builds.
    entry.insert("wire_api", toml_edit::value("responses"));
    doc["model_provider"] = toml_edit::value("ccload");
    // Model selection + context window. Empty strings skipped, same rule as the
    // Claude env keys — an empty model is not "unset" for Codex either.
    if let Some(m) = opts.codex_model.as_ref().filter(|s| !s.is_empty()) {
        doc["model"] = toml_edit::value(m);
    }
    if let Some(e) = opts.codex_reasoning_effort.as_ref().filter(|s| !s.is_empty()) {
        doc["model_reasoning_effort"] = toml_edit::value(e);
    }
    // 窗口：表单里显式填过的 `model_context_window` 是用户的明确意愿，优先；
    // 没填才用总控算出来的值。
    if let Some(c) = opts
        .codex_context_window
        .filter(|n| *n > 0)
        .or(opts.context_tokens.filter(|n| *n > 0))
    {
        doc["model_context_window"] = toml_edit::value(c);
    }
    if let Some(extra) = &opts.extra_env {
        merge_extra_toml(&mut doc, extra, CliTarget::Codex)?;
    }
    write_atomic(&toml_path, &doc.to_string())?;
    written.push(toml_path.display().to_string());

    // Older builds parked the token in auth.json. Codex deserializes that file
    // into a fixed struct, so the stray key was never read — and would be
    // dropped silently the next time Codex refreshed its tokens. Clean it out
    // rather than leave a dead secret lying in a file we do not own.
    let auth_path = root.join(".codex/auth.json");
    if auth_path.exists() {
        let mut auth = read_json(&auth_path)?;
        let removed = auth
            .as_object_mut()
            .is_some_and(|o| o.remove("CCLOAD_API_KEY").is_some());
        if removed {
            write_pretty_json(&auth_path, &auth)?;
            written.push(auth_path.display().to_string());
        }
    }
    Ok(written)
}

/// Gemini reads env vars from a dotenv file, never from `settings.json`.
pub(crate) const GEMINI_ENV: &str = ".gemini/.env";

/// The env vars takeover owns in `~/.gemini/.env`.
const GEMINI_TAKEOVER_VARS: &[&str] = &["GOOGLE_GEMINI_BASE_URL", "GEMINI_API_KEY"];

/// Undo what a snapshot could not record.
///
/// `restore` replays the file list captured at snapshot time, so a backup taken
/// before `.gemini/.env` joined `relative_paths()` lists only `settings.json` —
/// restoring it puts `selectedType` back to the Google login while the `.env`
/// keeps pointing Gemini at the kernel. The undo has to reach the file the
/// snapshot never knew about, without touching vars the user put there.
pub fn heal_unsnapshotted(
    root: &ConfigRoot,
    target: CliTarget,
    recorded: &[String],
) -> Result<(), AppError> {
    if target != CliTarget::GeminiCli || recorded.iter().any(|r| r == GEMINI_ENV) {
        return Ok(());
    }
    cli_dotenv::remove_keys(&root.join(GEMINI_ENV), GEMINI_TAKEOVER_VARS)
}

/// Pull ALL_CAPS entries out of a legacy `settings.json` `env` block so they can
/// be re-homed in `.env`. The block is dropped once emptied — leaving it would
/// keep showing stale values in the advanced form that Gemini never reads.
fn drain_settings_env(doc: &mut Value) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    let Some(root) = doc.as_object_mut() else {
        return out;
    };
    let Some(env) = root.get_mut("env").and_then(Value::as_object_mut) else {
        return out;
    };
    for (k, v) in env.iter() {
        if let Some(s) = v.as_str() {
            out.insert(k.clone(), s.to_string());
        }
    }
    env.retain(|k, _| !out.contains_key(k));
    if env.is_empty() {
        root.remove("env");
    }
    out
}

/// ALL_CAPS advanced knobs are env vars and belong in `.env`; dotted ones are
/// real settings.json paths and stay in the JSON.
fn collect_gemini_env(
    extra: &std::collections::BTreeMap<String, String>,
    out: &mut std::collections::BTreeMap<String, String>,
) {
    for (k, v) in extra {
        if k.is_empty() || v.is_empty() || blocked_for_user(CliTarget::GeminiCli, k) {
            continue;
        }
        if is_env_key(k) {
            out.insert(k.clone(), v.clone());
        }
    }
}

/// Gemini's own `getAuthTypeFromEnv` infers `gateway` from a base URL, but
/// `validateAuthMethod` has no branch for it and bails with "Invalid auth
/// method selected" — verified on gemini-cli 0.50.0. `gemini-api-key` is the
/// type that both validates (it only wants `GEMINI_API_KEY`, which we write
/// alongside) and honours `GOOGLE_GEMINI_BASE_URL`.
fn set_gemini_auth_type(doc: &mut Value) -> Result<(), AppError> {
    let security = object_at(doc, "security")?;
    let slot = security
        .entry("auth".to_string())
        .or_insert_with(|| Value::Object(Default::default()));
    if !slot.is_object() {
        *slot = Value::Object(Default::default());
    }
    slot.as_object_mut()
        .expect("just normalized")
        .insert("selectedType".into(), Value::String("gemini-api-key".into()));
    Ok(())
}

/// OpenCode only lists models that exist under `provider.ccload.models`.
/// Picking a kernel alias on takeover must also stub the catalog entry, or
/// `/models` would offer a `ccload/<alias>` that 503s when selected.
fn ensure_opencode_catalog_stub(
    doc: &mut Value,
    alias: &str,
    context_tokens: Option<i64>,
) -> Result<(), AppError> {
    let provider = doc
        .pointer_mut("/provider/ccload")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| AppError::Config("opencode.json 里没有 provider.ccload".into()))?;
    let models = match provider.get_mut("models").and_then(Value::as_object_mut) {
        Some(m) => m,
        None => {
            provider.insert("models".into(), Value::Object(Default::default()));
            provider.get_mut("models").and_then(Value::as_object_mut).unwrap()
        }
    };
    let entry = models
        .entry(alias.to_string())
        .or_insert_with(|| serde_json::json!({ "name": alias }));
    // 窗口写在**已存在**的条目上也要更新：目录项多半是「模型导入」早先建的，
    // 里面那个 limit.context 停在导入那天的值。换模型不改它，OpenCode 就一直
    // 按旧数字提前压缩（这正是「显示 200k、实际 1M」的那个症状）。
    if let Some(w) = context_tokens.filter(|n| *n > 0) {
        if let Some(obj) = entry.as_object_mut() {
            let limit = obj
                .entry("limit".to_string())
                .or_insert_with(|| Value::Object(Default::default()));
            if let Some(limit) = limit.as_object_mut() {
                limit.insert("context".into(), Value::from(w));
                limit
                    .entry("output".to_string())
                    .or_insert_with(|| Value::from(32_000));
            }
        }
    }
    Ok(())
}

/// A provider nobody selects routes nothing. Only fills a gap: an existing
/// choice is the user's and stays put.
fn ensure_opencode_default_model(doc: &mut Value) -> Result<(), AppError> {
    let root = doc
        .as_object_mut()
        .ok_or_else(|| AppError::Config("opencode.json 顶层不是对象".into()))?;
    if root.contains_key("model") {
        return Ok(());
    }
    let first = root
        .get("provider")
        .and_then(|p| p.get("ccload"))
        .and_then(|c| c.get("models"))
        .and_then(Value::as_object)
        .and_then(|m| m.keys().next().cloned());
    if let Some(alias) = first {
        root.insert("model".into(), Value::String(format!("ccload/{alias}")));
    }
    Ok(())
}
