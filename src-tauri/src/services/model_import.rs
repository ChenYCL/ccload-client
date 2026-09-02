//! One-click model catalog import: take the aliases the kernel can serve
//! (channel `models[].model` entries) and write them into a CLI's config so
//! the CLI can select any of them.
//!
//! Each CLI has a different notion of a "model catalog", grounded in the
//! respective official docs:
//!   * Claude Code — no catalog file at all. The only model surface is the
//!     five tier slots (ANTHROPIC_MODEL + ANTHROPIC_DEFAULT_{FABLE,SONNET,
//!     OPUS,HAIKU}_MODEL) plus one `ANTHROPIC_CUSTOM_MODEL_OPTION`. Those
//!     are *selections*, not a catalog. Custom / gateway ids also need
//!     `*_SUPPORTED_CAPABILITIES=effort,thinking` or `/effort` is dropped.
//!   * Codex — `[profiles.<name>]` tables, each with its own `model`,
//!     `model_context_window`, and `model_reasoning_effort`.
//!   * OpenCode — `provider.<id>.models` object with per-model
//!     `limit.context` / `reasoning` / `variants`.
//!   * Grok Build — `[model.<alias>]` tables (docs: custom models). A
//!     custom id that doesn't declare `supports_reasoning_effort` makes
//!     `/effort` a no-op. Context window drives auto-compaction.
//!   * Gemini CLI — one `model.name` slot, no catalog. Rejected explicitly
//!     so the UI can hide it instead of failing at apply time.
//!
//! # 追加，不改写
//!
//! 导入是**纯追加**的：只往目录里加新条目，绝不动用户已经在用的选择。
//! 早先的版本会把 `entries[0]` 写进 Codex 的顶层 `model`、OpenCode 的顶层
//! `model`，并把每一行都写一遍 Claude 的 `ANTHROPIC_MODEL` —— 导入 45 个模型
//! 的结果是 `ANTHROPIC_MODEL` 被覆盖 45 次、最后停在表格最后一行上，用户精心
//! 调过的 tier 绑定当场没了。现在的规则：
//!   * Claude Code 只写**用户在 Tier 列显式指定**的槽位，没指定的行不写；
//!     一个槽位被指了两次直接报错，而不是静默后来居上。
//!   * Codex 每个别名一个 profile，顶层 `model` 不碰；已存在的 profile 只更新
//!     model/context 两个键，profile 里其它设置保留。
//!   * OpenCode 合并进 `models`，顶层 `model` 只在缺失时才补。

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::AppError;
use crate::services::cli_backup::BackupStore;
use crate::services::cli_config::{current_endpoint, current_token};
use crate::services::cli_grok;
use crate::services::cli_io::{object_at, read_json, write_atomic, write_pretty_json};
use crate::services::cli_types::{CliTarget, ConfigRoot};
use crate::services::context_window::auto_compact_window;
use crate::services::model_caps::{claude_capabilities, reasoning_menu};

/// One row of the import table.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportEntry {
    pub alias: String,
    pub context_window: Option<i64>,
    /// Claude Code only: which tier slot this alias is bound to.
    /// `None` / `""` / `"none"` means "just a catalog entry, don't bind" —
    /// which for Claude Code means the row is skipped entirely.
    pub tier: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ImportResult {
    pub written: Vec<String>,
    pub backup_id: String,
    /// Aliases this target had no place for (Claude Code rows with no tier).
    /// Reported instead of silently dropped, so "导入 45 个" can't quietly
    /// mean "写了 1 个".
    pub skipped: Vec<String>,
    /// 本次 prune 掉的旧别名 —— 内核已经不认它们了。默认为空。
    #[serde(default)]
    pub removed: Vec<String>,
}

/// The env key for a tier, or `None` when the row isn't bound to any slot.
///
/// FABLE is in the list because Claude Code ships it — 用户自己的
/// settings.json 里就有 `ANTHROPIC_DEFAULT_FABLE_MODEL`。
fn claude_tier_key(tier: &str) -> Result<Option<&'static str>, AppError> {
    match tier {
        "" | "none" => Ok(None),
        "default" => Ok(Some("ANTHROPIC_MODEL")),
        "fable" => Ok(Some("ANTHROPIC_DEFAULT_FABLE_MODEL")),
        "sonnet" => Ok(Some("ANTHROPIC_DEFAULT_SONNET_MODEL")),
        "opus" => Ok(Some("ANTHROPIC_DEFAULT_OPUS_MODEL")),
        "haiku" => Ok(Some("ANTHROPIC_DEFAULT_HAIKU_MODEL")),
        other => Err(AppError::Config(format!("unknown Claude tier: {other}"))),
    }
}

/// OpenCode `limit.output` when the alias carries a context window. Absent
/// windows leave the whole `limit` block out so OpenCode keeps its defaults.
const DEFAULT_OUTPUT_TOKENS: i64 = 32_000;

/// The alias with any `vendor/` prefix removed (`amazon/nova-2-lite-v1` →
/// `nova-2-lite-v1`), or None when there is no prefix to strip.
///
/// Relays commonly expose the bare id while the kernel's channel lists the
/// namespaced one (or the reverse), so catalogs that can hold many keys get
/// both spellings and a CLI selection matches either way. Single-valued
/// surfaces (Claude tier env, Codex `model`) keep the prefixed form, since
/// that is the string the kernel actually matches channels against.
fn bare_alias(alias: &str) -> Option<&str> {
    let (_, rest) = alias.rsplit_once('/')?;
    (!rest.is_empty() && rest != alias).then_some(rest)
}

/// 在快照之前把 config.toml 解析出来：解析失败时什么都还没写，不该留下
/// 一份空快照占 5 份额度。
fn parse_codex(root: &ConfigRoot) -> Result<toml_edit::DocumentMut, AppError> {
    parse_toml(&root.join(".codex/config.toml"))
}

fn parse_grok(root: &ConfigRoot) -> Result<toml_edit::DocumentMut, AppError> {
    parse_toml(&root.join(".grok/config.toml"))
}

fn parse_toml(path: &std::path::Path) -> Result<toml_edit::DocumentMut, AppError> {
    let raw = if path.exists() {
        std::fs::read_to_string(path)?
    } else {
        String::new()
    };
    raw.parse::<toml_edit::DocumentMut>()
        .map_err(|e| AppError::Config(format!("{}: {e}", path.display())))
}

pub fn apply_import(
    root: &ConfigRoot,
    target: CliTarget,
    entries: &[ImportEntry],
    stamp: &str,
    backups: &BackupStore,
    prune: bool,
) -> Result<ImportResult, AppError> {
    let entries: Vec<&ImportEntry> = entries
        .iter()
        .filter(|e| !e.alias.trim().is_empty())
        .collect();
    if entries.is_empty() {
        return Err(AppError::Config("没有可导入的模型".into()));
    }
    if matches!(target, CliTarget::GeminiCli) {
        return Err(AppError::Config(
            "Gemini CLI 只有当前模型一个槽位（model.name），没有可追加的目录。请在 CLI 接管页的高级配置里改"
                .into(),
        ));
    }
    // Import only makes sense once the CLI points at the kernel; otherwise the
    // aliases would be sent to whatever upstream the CLI currently uses.
    if current_endpoint(root, target).is_none() {
        return Err(AppError::Config(format!(
            "{} 还未接管（配置里没有 ccLoad 端点），请先在 CLI 接管页应用",
            target.label()
        )));
    }

    // Claude 的槽位冲突 / 全空必须在快照之前判完：否则用户点一次「导入」却
    // 什么都没写，快照历史里会多出一份空的 model-import，还可能把一份有用的
    // 旧快照挤出 5 份上限。
    let mut skipped = Vec::new();
    let claude_bind: Option<Vec<(&'static str, &ImportEntry)>> =
        if matches!(target, CliTarget::ClaudeCode) {
            let mut bind: Vec<(&'static str, &ImportEntry)> = Vec::new();
            for e in &entries {
                match claude_tier_key(e.tier.as_deref().unwrap_or(""))? {
                    Some(key) => {
                        if let Some((_, prev)) = bind.iter().find(|(k, _)| *k == key) {
                            return Err(AppError::Config(format!(
                                "{key} 只能绑一个模型，但 {} 和 {} 都选了它",
                                prev.alias, e.alias
                            )));
                        }
                        bind.push((key, e));
                    }
                    None => skipped.push(e.alias.clone()),
                }
            }
            if bind.is_empty() {
                return Err(AppError::Config(
                    "Claude Code 没有模型目录文件，模型只能挂在 default/fable/sonnet/opus/haiku \
                     这 5 个槽位上。请在表格的 Tier 列给要绑定的模型选一个槽位（其余模型不会被写入，\
                     也不会动你现有的绑定）。"
                        .into(),
                ));
            }
            Some(bind)
        } else {
            None
        };

    // Codex/OpenCode 的目标文件解析也放在快照之前：解析失败同样什么都不写，
    // 不该占掉 5 份快照额度的一条。
    let codex_doc = match target {
        CliTarget::Codex => Some(parse_codex(root)?),
        _ => None,
    };
    let grok_doc = match target {
        CliTarget::GrokBuild => Some(parse_grok(root)?),
        _ => None,
    };
    let opencode_doc = match target {
        CliTarget::OpenCode => Some(read_json(&root.join(".config/opencode/opencode.json"))?),
        _ => None,
    };

    let snapshot = backups.snapshot(root, target, stamp, "model-import")?;
    let mut written = Vec::new();
    let mut removed: Vec<String> = Vec::new();
    match target {
        CliTarget::ClaudeCode => {
            let bind = claude_bind.expect("precomputed above");
            let path = root.join(".claude/settings.json");
            let mut doc = read_json(&path)?;
            {
                let env = object_at(&mut doc, "env")?;
                for (key, e) in &bind {
                    env.insert((*key).into(), Value::String(e.alias.clone()));
                    // ANTHROPIC_MODEL 没有 *_SUPPORTED_CAPABILITIES 这个键，
                    // 主模型靠 CLAUDE_CODE_ALWAYS_ENABLE_EFFORT 发 effort。
                    if *key != "ANTHROPIC_MODEL" {
                        if let Some(caps) = claude_capabilities(&e.alias) {
                            env.insert(
                                format!("{key}_SUPPORTED_CAPABILITIES"),
                                Value::String(caps.into()),
                            );
                        }
                    }
                }
                // 走 ccLoad 时模型 id 不是 Anthropic 官方那几个，Claude Code
                // 不认就不会发 effort，表现就是 `/effort` 没反应。这两个是
                // 官方给网关别名准备的开关，写成 1 是幂等的。
                env.insert(
                    "CLAUDE_CODE_ALWAYS_ENABLE_EFFORT".into(),
                    Value::String("1".into()),
                );
                env.insert(
                    "CLAUDE_CODE_DISABLE_UNKNOWN_MODEL_WINDOW_ENFORCEMENT".into(),
                    Value::String("1".into()),
                );
                // 上下文上限在 Claude Code 里是**全局**一个开关，不是 per-model。
                // 只有用户明确把某个模型绑到 default 槽位、且那一行填了窗口，才
                // 跟着改；否则保留用户原值。
                if let Some(w) = bind
                    .iter()
                    .find(|(k, _)| *k == "ANTHROPIC_MODEL")
                    .and_then(|(_, e)| e.context_window)
                    .filter(|n| *n > 0)
                {
                    env.insert(
                        "CLAUDE_CODE_MAX_CONTEXT_TOKENS".into(),
                        Value::String(w.to_string()),
                    );
                    if let Some(compact) = auto_compact_window(w) {
                        env.insert(
                            "CLAUDE_CODE_AUTO_COMPACT_WINDOW".into(),
                            Value::String(compact.to_string()),
                        );
                    }
                }
                // 第 6 个槽：一个没绑 tier 的别名，留给 /model 选择器。已有值
                // 是用户的，不覆盖。
                if !env.contains_key("ANTHROPIC_CUSTOM_MODEL_OPTION") {
                    if let Some(extra) = skipped.first() {
                        env.insert(
                            "ANTHROPIC_CUSTOM_MODEL_OPTION".into(),
                            Value::String(extra.clone()),
                        );
                        if let Some(caps) = claude_capabilities(extra) {
                            env.insert(
                                "ANTHROPIC_CUSTOM_MODEL_OPTION_SUPPORTED_CAPABILITIES".into(),
                                Value::String(caps.into()),
                            );
                        }
                        skipped.remove(0);
                    }
                }
            }
            write_pretty_json(&path, &doc)?;
            written.push(path.display().to_string());
        }
        CliTarget::Codex => {
            let mut doc = codex_doc.expect("parsed above");
            // Codex 的可追加模型面是 profile：`codex --profile <别名>`。顶层
            // `model` 是用户当前在用的那个，导入不碰它。
            let had_profiles = doc.contains_key("profiles");
            let profiles = doc
                .entry("profiles")
                .or_insert(toml_edit::Item::Table(toml_edit::Table::new()))
                .as_table_mut()
                .ok_or_else(|| {
                    AppError::Config("config.toml 里的 profiles 不是表，拒绝写入".into())
                })?;
            // implicit 只对本次新建的表设：用户文件里已有显式 `[profiles]` 表头时
            // 无条件 implicit 会在写回时删掉那一行（语义等价，但用户 diff 自己的
            // 配置会看到一处来历不明的删除 —— 「追加不动你的文件」就不成立了）。
            if !had_profiles {
                profiles.set_implicit(true);
            }
            for e in &entries {
                let tbl = profiles
                    .entry(&e.alias)
                    .or_insert(toml_edit::Item::Table(toml_edit::Table::new()))
                    .as_table_mut()
                    .ok_or_else(|| {
                        AppError::Config(format!("profiles.{} 不是表，拒绝写入", e.alias))
                    })?;
                // 只动这两个键，profile 里用户自己加的 approval_policy 之类保留。
                tbl["model"] = toml_edit::value(e.alias.as_str());
                if let Some(w) = e.context_window.filter(|n| *n > 0) {
                    tbl["model_context_window"] = toml_edit::value(w);
                }
                if let Some(menu) = reasoning_menu(&e.alias) {
                    tbl["model_reasoning_effort"] = toml_edit::value(menu.default);
                }
            }
            write_atomic(&root.join(".codex/config.toml"), &doc.to_string())?;
            written.push(root.join(".codex/config.toml").display().to_string());
        }
        CliTarget::OpenCode => {
            let path = root.join(".config/opencode/opencode.json");
            let mut doc = opencode_doc.expect("parsed above");
            // 顶层 model / small_model 正指着的别名。prune 绝不能删它们 ——
            // 否则用户当前的选择指向一个不存在的模型，下一次请求直接失败。
            let protected: std::collections::HashSet<String> = ["model", "small_model"]
                .iter()
                .filter_map(|k| doc.get(*k).and_then(Value::as_str))
                .map(|m| m.strip_prefix("ccload/").unwrap_or(m).trim().to_string())
                .filter(|m| !m.is_empty())
                .collect();
            {
                // provider.ccload exists because the takeover check above passed.
                let provider = doc
                    .pointer_mut("/provider/ccload")
                    .and_then(Value::as_object_mut)
                    .ok_or_else(|| {
                        AppError::Config("opencode.json 里没有 provider.ccload，请先接管".into())
                    })?;
                let entry_for = |alias: &str, e: &ImportEntry, existing: Option<&Value>| {
                    let mut m = existing
                        .and_then(Value::as_object)
                        .cloned()
                        .unwrap_or_default();
                    m.insert("name".into(), Value::String(alias.to_string()));
                    if let Some(w) = e.context_window.filter(|n| *n > 0) {
                        m.insert(
                            "limit".into(),
                            serde_json::json!({ "context": w, "output": DEFAULT_OUTPUT_TOKENS }),
                        );
                    }
                    if let Some(menu) = reasoning_menu(alias) {
                        m.insert("reasoning".into(), Value::Bool(true));
                        let options = m
                            .entry("options".to_string())
                            .or_insert_with(|| Value::Object(Default::default()));
                        if let Some(obj) = options.as_object_mut() {
                            obj.insert(
                                "reasoningEffort".into(),
                                Value::String(menu.default.into()),
                            );
                        }
                        let mut variants = serde_json::Map::new();
                        for lvl in menu.levels {
                            variants.insert(
                                lvl.id.to_string(),
                                serde_json::json!({ "reasoningEffort": lvl.value }),
                            );
                        }
                        m.insert("variants".into(), Value::Object(variants));
                    }
                    Value::Object(m)
                };
                // 合并进已有目录，而不是整块替换 —— 用户手工加过的模型不能被
                // 一次导入抹掉。同名条目按本次导入的窗口刷新。
                let catalog = match provider.get_mut("models").and_then(Value::as_object_mut) {
                    Some(existing) => existing,
                    None => {
                        provider.insert("models".into(), Value::Object(Default::default()));
                        provider["models"].as_object_mut().unwrap()
                    }
                };
                // 只增不删会让目录一路涨：上游退役的别名留在里面，用户选中就是
                // 一个 503（内核只认渠道里真实存在的名字）。prune 时按本次清单
                // 收敛 —— 本次没有的就是内核已经不认的。
                if prune {
                    let keep: std::collections::HashSet<String> = entries
                        .iter()
                        .flat_map(|e| {
                            std::iter::once(e.alias.clone())
                                .chain(bare_alias(&e.alias).map(str::to_string))
                        })
                        .collect();
                    // 只删**我们建的**条目：导入写出来的条目一定带 `name` 且
                    // name == key（见 entry_for）。用户手工加的、或形状不同的，
                    // 一律不碰 —— prune 的语义是「收敛我们自己的目录」，不是清空。
                    let ours = |k: &str, v: &Value| {
                        v.get("name").and_then(Value::as_str) == Some(k)
                    };
                    let dropped: Vec<String> = catalog
                        .iter()
                        .filter(|(k, v)| {
                            !keep.contains(*k) && !protected.contains(*k) && ours(k, v)
                        })
                        .map(|(k, _)| k.clone())
                        .collect();
                    for k in &dropped {
                        catalog.remove(k);
                    }
                    removed = dropped;
                }
                for e in &entries {
                    let prev = catalog.get(&e.alias).cloned();
                    catalog.insert(e.alias.clone(), entry_for(&e.alias, e, prev.as_ref()));
                    // Also expose the un-namespaced spelling so selecting
                    // `nova-2-lite-v1` resolves as well as `amazon/nova-2-lite-v1`.
                    if let Some(bare) = bare_alias(&e.alias) {
                        if !catalog.contains_key(bare) {
                            catalog.insert(bare.to_string(), entry_for(bare, e, None));
                        }
                    }
                }
            }
            {
                let root_obj = doc
                    .as_object_mut()
                    .ok_or_else(|| AppError::Config("opencode.json 顶层不是对象".into()))?;
                // OpenCode parses `model` as `provider/model`; a namespaced
                // alias would make `ccload/amazon/nova-…` ambiguous, so the
                // default points at the bare key registered above.
                // 只在用户还没选过默认模型时补一个，已经选过就不动。
                if !root_obj.contains_key("model") {
                    let first = &entries[0].alias;
                    let default_alias = bare_alias(first).unwrap_or(first);
                    root_obj.insert(
                        "model".into(),
                        Value::String(format!("ccload/{default_alias}")),
                    );
                }
            }
            write_pretty_json(&path, &doc)?;
            written.push(path.display().to_string());
        }
        CliTarget::GrokBuild => {
            let mut doc = grok_doc.expect("parsed above");
            let endpoint = current_endpoint(root, target).expect("takeover checked above");
            let token = current_token(root, target).unwrap_or_default();
            for e in &entries {
                cli_grok::write_catalog_entry(
                    &mut doc,
                    &e.alias,
                    &endpoint,
                    &token,
                    e.context_window,
                )?;
            }
            if prune {
                let keep: std::collections::HashSet<String> =
                    entries.iter().map(|e| e.alias.clone()).collect();
                removed = cli_grok::prune_catalog(&mut doc, &keep, &endpoint);
            }
            let path = root.join(".grok/config.toml");
            write_atomic(&path, &doc.to_string())?;
            written.push(path.display().to_string());
        }
        // Unreachable: filtered above, and the compiler wants the match total.
        CliTarget::GeminiCli => unreachable!(),
    }

    Ok(ImportResult {
        written,
        backup_id: snapshot.id,
        skipped,
        removed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root_in(dir: &tempfile::TempDir) -> (ConfigRoot, BackupStore) {
        (
            ConfigRoot::sandbox(dir.path().to_path_buf()),
            BackupStore::new(dir.path().join("bk")),
        )
    }

    #[test]
    fn rejects_targets_without_catalog_surface() {
        let dir = tempfile::tempdir().unwrap();
        let (root, bk) = root_in(&dir);
        let err = apply_import(
            &root,
            CliTarget::GeminiCli,
            &[ImportEntry {
                alias: "kimi-k3".into(),
                context_window: None,
                tier: None,
            }],
            "s1",
            &bk,
            false,
        )
        .unwrap_err();
        assert!(err.to_string().contains("model.name"), "{err}");
    }

    #[test]
    fn requires_takeover_first() {
        let dir = tempfile::tempdir().unwrap();
        let (root, bk) = root_in(&dir);
        let err = apply_import(
            &root,
            CliTarget::OpenCode,
            &[ImportEntry {
                alias: "kimi-k3".into(),
                context_window: None,
                tier: None,
            }],
            "s1",
            &bk,
            false,
        )
        .unwrap_err();
        assert!(err.to_string().contains("接管"));
    }

    fn claude_root(dir: &tempfile::TempDir) -> (ConfigRoot, BackupStore, std::path::PathBuf) {
        let (root, bk) = root_in(dir);
        let settings = root.join(".claude/settings.json");
        std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
        std::fs::write(
            &settings,
            r#"{"env":{"ANTHROPIC_BASE_URL":"http://x",
                       "ANTHROPIC_DEFAULT_OPUS_MODEL":"my-opus",
                       "CLAUDE_CODE_MAX_CONTEXT_TOKENS":"777"}}"#,
        )
        .unwrap();
        (root, bk, settings)
    }

    fn entry(alias: &str, w: Option<i64>, tier: Option<&str>) -> ImportEntry {
        ImportEntry {
            alias: alias.into(),
            context_window: w,
            tier: tier.map(str::to_string),
        }
    }

    #[test]
    fn claude_import_writes_only_the_tiers_the_user_picked() {
        let dir = tempfile::tempdir().unwrap();
        let (root, bk, settings) = claude_root(&dir);

        let r = apply_import(
            &root,
            CliTarget::ClaudeCode,
            &[
                entry("kimi-k3", Some(262_144), Some("default")),
                entry("glm-5.3", None, Some("haiku")),
                // 没选槽位的行：不该产生任何写入。
                entry("gpt-5.6", Some(400_000), None),
                entry("grok-4.6", Some(2_000_000), Some("")),
            ],
            "s1",
            &bk,
            false,
        )
        .unwrap();
        // 没绑 slot 的第一行进 CUSTOM_MODEL_OPTION，其余才算 skipped。
        assert_eq!(r.skipped, vec!["grok-4.6"]);

        let doc: Value =
            serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        assert_eq!(doc.pointer("/env/ANTHROPIC_MODEL").unwrap(), "kimi-k3");
        assert_eq!(
            doc.pointer("/env/ANTHROPIC_DEFAULT_HAIKU_MODEL").unwrap(),
            "glm-5.3"
        );
        assert_eq!(
            doc.pointer("/env/ANTHROPIC_DEFAULT_HAIKU_MODEL_SUPPORTED_CAPABILITIES")
                .unwrap(),
            "effort,thinking"
        );
        assert_eq!(
            doc.pointer("/env/ANTHROPIC_CUSTOM_MODEL_OPTION").unwrap(),
            "gpt-5.6"
        );
        assert_eq!(
            doc.pointer("/env/CLAUDE_CODE_ALWAYS_ENABLE_EFFORT").unwrap(),
            "1"
        );
        // 用户原来绑好的 opus 槽位没人认领，必须原样留着。
        assert_eq!(
            doc.pointer("/env/ANTHROPIC_DEFAULT_OPUS_MODEL").unwrap(),
            "my-opus"
        );
        // default 那一行填了窗口，全局上限跟着它走。
        assert_eq!(
            doc.pointer("/env/CLAUDE_CODE_MAX_CONTEXT_TOKENS").unwrap(),
            "262144"
        );
        assert_eq!(
            doc.pointer("/env/CLAUDE_CODE_AUTO_COMPACT_WINDOW").unwrap(),
            "182144"
        );
    }

    /// 没人绑 default 槽位时，全局上下文上限保持用户原值 —— 不能被某一行的
    /// 窗口顺手改掉（这个键在 Claude Code 里是全局的，不是 per-model）。
    #[test]
    fn claude_import_leaves_the_global_context_cap_alone() {
        let dir = tempfile::tempdir().unwrap();
        let (root, bk, settings) = claude_root(&dir);
        apply_import(
            &root,
            CliTarget::ClaudeCode,
            &[entry("glm-5.3", Some(131_072), Some("haiku"))],
            "s1",
            &bk,
            false,
        )
        .unwrap();
        let doc: Value =
            serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        assert_eq!(
            doc.pointer("/env/CLAUDE_CODE_MAX_CONTEXT_TOKENS").unwrap(),
            "777"
        );
    }

    /// 老实现里，45 行全是 default 会让 ANTHROPIC_MODEL 被覆盖 45 次、静默停在
    /// 最后一行。现在直接报错，并且**一个字都不写**。
    #[test]
    fn two_models_on_one_slot_is_an_error_not_last_wins() {
        let dir = tempfile::tempdir().unwrap();
        let (root, bk, settings) = claude_root(&dir);
        let before = std::fs::read_to_string(&settings).unwrap();

        let err = apply_import(
            &root,
            CliTarget::ClaudeCode,
            &[
                entry("kimi-k3", None, Some("default")),
                entry("glm-5.3", None, Some("default")),
            ],
            "s1",
            &bk,
            false,
        )
        .unwrap_err();
        assert!(err.to_string().contains("只能绑一个模型"), "{err}");
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), before);
        // 校验失败不能留下空快照，否则 5 份上限会被这种「什么都没写」的条目挤掉。
        assert!(
            bk.list(Some(CliTarget::ClaudeCode)).unwrap().is_empty(),
            "failed import must not snapshot"
        );
    }

    #[test]
    fn claude_import_without_any_slot_explains_itself() {
        let dir = tempfile::tempdir().unwrap();
        let (root, bk, settings) = claude_root(&dir);
        let before = std::fs::read_to_string(&settings).unwrap();
        let err = apply_import(
            &root,
            CliTarget::ClaudeCode,
            &[entry("kimi-k3", None, None), entry("glm-5.3", None, None)],
            "s1",
            &bk,
            false,
        )
        .unwrap_err();
        assert!(err.to_string().contains("Tier"), "{err}");
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), before);
        assert!(
            bk.list(Some(CliTarget::ClaudeCode)).unwrap().is_empty(),
            "failed import must not snapshot"
        );
    }

    /// Codex 的可追加面是 profile。顶层 `model`（用户当前在用的）不能被碰，
    /// 已有 profile 里的其它设置也要留着。
    #[test]
    fn codex_import_adds_profiles_and_keeps_the_active_model() {
        let dir = tempfile::tempdir().unwrap();
        let (root, bk) = root_in(&dir);
        let path = root.join(".codex/config.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "model = \"gpt-5.6-sol\"\nmodel_reasoning_effort = \"xhigh\"\n\n\
             [model_providers.ccload]\nbase_url = \"http://x/v1\"\n\n\
             [profiles.\"kimi-k3\"]\napproval_policy = \"never\"\n",
        )
        .unwrap();

        apply_import(
            &root,
            CliTarget::Codex,
            &[
                entry("kimi-k3", Some(262_144), None),
                entry("glm-5.3", None, None),
            ],
            "s1",
            &bk,
            false,
        )
        .unwrap();

        let doc = std::fs::read_to_string(&path)
            .unwrap()
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        // 当前在用的模型和它的推理档没被动。
        assert_eq!(doc["model"].as_str(), Some("gpt-5.6-sol"));
        assert_eq!(doc["model_reasoning_effort"].as_str(), Some("xhigh"));
        // 每个别名一个 profile。
        assert_eq!(doc["profiles"]["kimi-k3"]["model"].as_str(), Some("kimi-k3"));
        assert_eq!(
            doc["profiles"]["kimi-k3"]["model_context_window"]
                .as_integer(),
            Some(262_144)
        );
        // 已存在的 profile 里用户自己的设置保留。
        assert_eq!(
            doc["profiles"]["kimi-k3"]["approval_policy"].as_str(),
            Some("never")
        );
        assert_eq!(doc["profiles"]["glm-5.3"]["model"].as_str(), Some("glm-5.3"));
        // 没窗口就不写这个键，让 Codex 用它自己的默认。
        assert!(doc["profiles"]["glm-5.3"]
            .get("model_context_window")
            .is_none());
        assert_eq!(
            doc["profiles"]["kimi-k3"]["model_reasoning_effort"].as_str(),
            Some("high")
        );
    }

    /// 导入只往目录里加。用户手工加的模型、以及他当前选中的默认模型，
    /// 都不能被一次导入抹掉。
    #[test]
    fn opencode_import_merges_and_keeps_the_active_model() {
        let dir = tempfile::tempdir().unwrap();
        let (root, bk) = root_in(&dir);
        let path = root.join(".config/opencode/opencode.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"model":"ccload/my-pick",
                "provider":{"ccload":{"options":{"baseURL":"http://x/v1"},
                                      "models":{"hand-added":{"name":"hand-added"}}}}}"#,
        )
        .unwrap();

        apply_import(
            &root,
            CliTarget::OpenCode,
            &[entry("kimi-k3", Some(262_144), None)],
            "s1",
            &bk,
            false,
        )
        .unwrap();

        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(doc.pointer("/model").unwrap(), "ccload/my-pick");
        assert_eq!(
            doc.pointer("/provider/ccload/models/hand-added/name").unwrap(),
            "hand-added"
        );
        assert_eq!(
            doc.pointer("/provider/ccload/models/kimi-k3/limit/context")
                .unwrap(),
            262_144
        );
    }

    /// A namespaced alias must reach OpenCode under both spellings, and the
    /// top-level `model` must stay a two-segment `provider/model` string —
    /// `ccload/amazon/nova-2-lite-v1` would be parsed ambiguously.
    #[test]
    fn opencode_registers_bare_and_namespaced_aliases() {
        let dir = tempfile::tempdir().unwrap();
        let (root, bk) = root_in(&dir);
        let path = root.join(".config/opencode/opencode.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"provider":{"ccload":{"options":{"baseURL":"http://x/v1"}}}}"#,
        )
        .unwrap();

        apply_import(
            &root,
            CliTarget::OpenCode,
            &[ImportEntry {
                alias: "amazon/nova-2-lite-v1".into(),
                context_window: Some(1_000_000),
                tier: None,
            }],
            "s1",
            &bk,
            false,
        )
        .unwrap();

        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(doc.pointer("/model").unwrap(), "ccload/nova-2-lite-v1");
        for key in ["amazon~1nova-2-lite-v1", "nova-2-lite-v1"] {
            assert_eq!(
                doc.pointer(&format!("/provider/ccload/models/{key}/limit/context"))
                    .unwrap(),
                1_000_000,
                "{key} must be selectable"
            );
        }
    }

    #[test]
    fn bare_alias_only_strips_a_real_prefix() {
        assert_eq!(bare_alias("amazon/nova-2-lite-v1"), Some("nova-2-lite-v1"));
        // Deeper namespaces keep only the final segment.
        assert_eq!(bare_alias("a/b/c"), Some("c"));
        assert_eq!(bare_alias("glm-5.2"), None);
        // A trailing slash has no id to fall back to.
        assert_eq!(bare_alias("amazon/"), None);
    }

    #[test]
    fn opencode_import_builds_catalog_and_default_model() {
        let dir = tempfile::tempdir().unwrap();
        let (root, bk) = root_in(&dir);
        let path = root.join(".config/opencode/opencode.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"provider":{"ccload":{"options":{"baseURL":"http://x/v1"}}}}"#,
        )
        .unwrap();

        apply_import(
            &root,
            CliTarget::OpenCode,
            &[
                ImportEntry {
                    alias: "kimi-k3".into(),
                    context_window: Some(262_144),
                    tier: None,
                },
                ImportEntry {
                    alias: "fable-5".into(),
                    context_window: None,
                    tier: None,
                },
            ],
            "s1",
            &bk,
            false,
        )
        .unwrap();

        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(doc.pointer("/model").unwrap(), "ccload/kimi-k3");
        assert_eq!(
            doc.pointer("/provider/ccload/models/kimi-k3/limit/context")
                .unwrap(),
            262_144
        );
        assert_eq!(
            doc.pointer("/provider/ccload/models/kimi-k3/reasoning")
                .and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            doc.pointer("/provider/ccload/models/kimi-k3/options/reasoningEffort")
                .unwrap(),
            "high"
        );
        // No context window → no limit block, only the name.
        assert!(doc.pointer("/provider/ccload/models/fable-5/limit").is_none());
        assert_eq!(
            doc.pointer("/provider/ccload/models/fable-5/name").unwrap(),
            "fable-5"
        );
    }


    fn seeded(dir: &tempfile::TempDir) -> (ConfigRoot, BackupStore, std::path::PathBuf) {
        let (root, bk) = root_in(dir);
        let path = root.join(".config/opencode/opencode.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // 目录里混着三类：内核还认的、内核已经退役的（我们导入建的，name == key）、
        // 用户手工加的（name 是自己起的显示名，不等于 key —— prune 绝不能碰）。
        std::fs::write(
            &path,
            r#"{"model":"ccload/kimi-k3",
                "provider":{"ccload":{"options":{"baseURL":"http://x/v1"},
                  "models":{"kimi-k3":{"name":"kimi-k3"},
                            "claude-4.6-opus-max":{"name":"claude-4.6-opus-max"},
                            "auto":{"name":"auto"},
                            "my-pick":{"name":"My hand-tuned pick"}}}}}"#,
        )
        .unwrap();
        (root, bk, path)
    }

    /// 退役别名留在目录里，用户选中就是一个 503。prune 按本次清单收敛。
    #[test]
    fn prune_drops_aliases_the_kernel_no_longer_serves() {
        let dir = tempfile::tempdir().unwrap();
        let (root, bk, path) = seeded(&dir);

        let out = apply_import(
            &root,
            CliTarget::OpenCode,
            &[entry("kimi-k3", Some(262_144), None)],
            "s1",
            &bk,
            true,
        )
        .unwrap();

        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let models = doc.pointer("/provider/ccload/models").unwrap();
        assert!(models.get("kimi-k3").is_some(), "本次清单里的要留下");
        assert!(models.get("claude-4.6-opus-max").is_none(), "退役的要清掉");
        assert!(models.get("auto").is_none(), "退役的要清掉");
        assert!(
            models.get("my-pick").is_some(),
            "用户手工加的条目被 prune 删了 —— 它根本不是我们建的"
        );
        let mut removed = out.removed.clone();
        removed.sort();
        assert_eq!(removed, vec!["auto", "claude-4.6-opus-max"]);
    }

    /// 顶层 `model` 正指着的别名，哪怕本次清单没有它也不能删 —— 删了用户当前的
    /// 选择就指向一个不存在的模型，下一次请求直接失败。
    #[test]
    fn prune_never_removes_the_active_selection() {
        let dir = tempfile::tempdir().unwrap();
        let (root, bk) = root_in(&dir);
        let path = root.join(".config/opencode/opencode.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // 当前选中的是 old-pick（我们建的形状），本次清单里没有它。
        std::fs::write(
            &path,
            r#"{"model":"ccload/old-pick",
                "provider":{"ccload":{"options":{"baseURL":"http://x/v1"},
                  "models":{"old-pick":{"name":"old-pick"}}}}}"#,
        )
        .unwrap();

        let out = apply_import(
            &root,
            CliTarget::OpenCode,
            &[entry("kimi-k3", Some(262_144), None)],
            "s1",
            &bk,
            true,
        )
        .unwrap();

        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(
            doc.pointer("/provider/ccload/models/old-pick").is_some(),
            "顶层 model 指着的条目被删了，用户的选择成了悬空引用"
        );
        assert!(out.removed.is_empty());
    }

    /// prune 是显式开关。不开时行为和以前一模一样 —— 一个都不删。
    #[test]
    fn without_prune_nothing_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let (root, bk, path) = seeded(&dir);

        let out = apply_import(
            &root,
            CliTarget::OpenCode,
            &[entry("kimi-k3", Some(262_144), None)],
            "s1",
            &bk,
            false,
        )
        .unwrap();

        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let models = doc.pointer("/provider/ccload/models").unwrap();
        assert!(models.get("claude-4.6-opus-max").is_some());
        assert!(out.removed.is_empty());
    }

    /// prune 前先快照 —— 删错了要能回滚。
    #[test]
    fn prune_still_snapshots_first() {
        let dir = tempfile::tempdir().unwrap();
        let (root, bk, _) = seeded(&dir);
        let out = apply_import(
            &root,
            CliTarget::OpenCode,
            &[entry("kimi-k3", None, None)],
            "s1",
            &bk,
            true,
        )
        .unwrap();
        assert!(!out.backup_id.is_empty(), "prune 必须留下可回滚的快照");
    }

    fn grok_root(dir: &tempfile::TempDir) -> (ConfigRoot, BackupStore, std::path::PathBuf) {
        let (root, bk) = root_in(dir);
        let path = root.join(".grok/config.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "[models]\ndefault = \"ccload\"\n\n\
             [model.ccload]\nmodel = \"grok-4.6\"\n\
             base_url = \"http://x/v1\"\napi_key = \"tok\"\n",
        )
        .unwrap();
        (root, bk, path)
    }

    /// Grok 的可追加面是 `[model.<别名>]`。顶层 default（用户当前在用的）不能被碰。
    #[test]
    fn grok_import_writes_custom_tables_and_keeps_the_active_model() {
        let dir = tempfile::tempdir().unwrap();
        let (root, bk, path) = grok_root(&dir);

        apply_import(
            &root,
            CliTarget::GrokBuild,
            &[
                entry("glm-5.3-flash[1M]", Some(1_000_000), None),
                entry("claude-opus-5", Some(1_000_000), None),
                entry("grok-4.5", Some(500_000), None),
            ],
            "s1",
            &bk,
            false,
        )
        .unwrap();

        let doc = std::fs::read_to_string(&path)
            .unwrap()
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        assert_eq!(doc["models"]["default"].as_str(), Some("ccload"));
        let glm = &doc["model"]["glm-5.3-flash[1M]"];
        assert_eq!(glm["model"].as_str(), Some("glm-5.3-flash[1M]"));
        assert_eq!(glm["api_backend"].as_str(), Some("chat_completions"));
        assert_eq!(glm["context_window"].as_integer(), Some(1_000_000));
        assert_eq!(glm["supports_reasoning_effort"].as_bool(), Some(true));
        assert_eq!(glm["base_url"].as_str(), Some("http://x/v1"));
        let opus = &doc["model"]["claude-opus-5"];
        assert_eq!(opus["api_backend"].as_str(), Some("chat_completions"));
        assert!(opus["reasoning_efforts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v
                .as_inline_table()
                .and_then(|t| t.get("id"))
                .and_then(|x| x.as_str())
                == Some("xhigh")));
        let g45 = &doc["model"]["grok-4.5"];
        assert_eq!(g45["api_backend"].as_str(), Some("responses"));
        assert!(
            !g45["reasoning_efforts"].as_array().unwrap().iter().any(|v| {
                v.as_inline_table()
                    .and_then(|t| t.get("id"))
                    .and_then(|x| x.as_str())
                    == Some("xhigh")
            }),
            "official grok-4.5 menu has no xhigh"
        );
    }

    #[test]
    fn grok_import_prune_drops_retired_aliases_not_the_profile() {
        let dir = tempfile::tempdir().unwrap();
        let (root, bk, path) = grok_root(&dir);
        let mut raw = std::fs::read_to_string(&path).unwrap();
        // 导入建的完整目录表才归 prune 管：带 name + model。
        raw.push_str(
            "\n[model.retired]\nname = \"retired\"\nmodel = \"retired\"\nbase_url = \"http://x/v1\"\n",
        );
        std::fs::write(&path, raw).unwrap();

        let out = apply_import(
            &root,
            CliTarget::GrokBuild,
            &[entry("glm-5.3-flash[1M]", Some(1_000_000), None)],
            "s1",
            &bk,
            true,
        )
        .unwrap();

        let doc = std::fs::read_to_string(&path)
            .unwrap()
            .parse::<toml_edit::DocumentMut>()
            .unwrap();
        assert!(doc["model"].get("retired").is_none());
        assert!(doc["model"].get("ccload").is_some());
        assert!(doc["model"].get("glm-5.3-flash[1M]").is_some());
        assert_eq!(out.removed, vec!["retired"]);
    }

}
