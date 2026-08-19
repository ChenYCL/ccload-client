//! Fallback-chain commands. Persist locally, apply by writing N channels
//! of descending priority through the existing thin admin passthrough.

use tauri::State;

use crate::error::{AppError, AppResult};
use crate::services::fallback::{
    hop_priority, model_entry, validate_chain, FallbackChain, FallbackStore,
};
use crate::state::AppState;

fn store_path(state: &AppState) -> std::path::PathBuf {
    // Sibling of settings.json so a wipe of ~/.ccload-client takes both.
    state.config_dir().join("fallback.json")
}

#[tauri::command]
pub async fn fallback_list(state: State<'_, AppState>) -> AppResult<Vec<FallbackChain>> {
    let path = store_path(&state);
    Ok(FallbackStore::load(&path)?.chains)
}

#[tauri::command]
pub async fn fallback_save(
    state: State<'_, AppState>,
    chain: FallbackChain,
) -> AppResult<Vec<FallbackChain>> {
    validate_chain(&chain)?;
    let path = store_path(&state);
    let mut store = FallbackStore::load(&path)?;
    store.upsert(chain);
    store.save(&path)?;
    Ok(store.chains)
}

#[tauri::command]
pub async fn fallback_delete(
    state: State<'_, AppState>,
    alias: String,
) -> AppResult<Vec<FallbackChain>> {
    let path = store_path(&state);
    let mut store = FallbackStore::load(&path)?;
    store.remove(&alias);
    store.save(&path)?;
    Ok(store.chains)
}

/// Apply one chain: for each hop, PATCH the bound channel so it serves
/// `alias` by redirecting to that hop's upstream, at a priority that
/// encodes hop order. Channels that don't exist yet are left for the
/// user to create in the kernel admin — we never invent credentials.
#[tauri::command]
pub async fn fallback_apply(
    state: State<'_, AppState>,
    alias: String,
) -> AppResult<Vec<String>> {
    let path = store_path(&state);
    let store = FallbackStore::load(&path)?;
    let chain = store
        .chains
        .iter()
        .find(|c| c.alias == alias)
        .ok_or_else(|| AppError::Config(format!("no chain named {alias}")))?
        .clone();
    validate_chain(&chain)?;

    let mut log = Vec::new();
    for (i, hop) in chain.hops.iter().enumerate() {
        let Some(id) = hop.channel_id else {
            log.push(format!(
                "hop {i} ({}): skipped — no channel bound",
                hop.upstream
            ));
            continue;
        };
        // PUT channels/{id} is a FULL update: Validate() rejects a body
        // without name, and without api_keys on api-key channels
        // (admin_types.go:177,186). Plaintext keys never come back from
        // GET /channels/{id}; the kernel's own editor endpoint
        // GET /channels/{id}/editor returns them (that's how the official
        // web UI edits a channel). Build the complete request from that
        // snapshot and overlay only priority + models, so an apply never
        // wipes credentials or urls.
        let (base_url, password) = {
            let s = state.settings.read().await;
            (s.kernel.base_url(), s.kernel.admin_password.clone())
        };
        let editor = state
            .admin
            .request(
                &base_url,
                &password,
                "GET",
                &format!("channels/{id}/editor"),
                None,
                None,
            )
            .await
            .map_err(|e| AppError::Config(format!("hop {i} channel {id}: editor read: {e}")))?;
        let data = editor
            .get("data")
            .cloned()
            .ok_or_else(|| AppError::Config(format!("hop {i} channel {id}: empty editor")))?;
        let channel = data
            .get("channel")
            .cloned()
            .ok_or_else(|| AppError::Config(format!("hop {i} channel {id}: no channel")))?;
        // OAuth channels (anthropic_oauth / codex_oauth / xai_oauth /
        // antigravity_oauth) own their credentials: PUT rejects a non-empty
        // api_keys with 409 "OAuth channel API keys are read-only"
        // (admin_channels.go:1001). And the editor endpoint *synthesizes* a
        // pseudo-key holding the OAuth access token for display
        // (admin_channels.go:706-716), so echoing it back is guaranteed to
        // trip that check. Send keys only for api-key channels.
        let uses_oauth = channel
            .get("auth_type")
            .and_then(|v| v.as_str())
            .is_some_and(|t| t != "api_key" && !t.is_empty());
        let api_keys: Vec<serde_json::Value> = if uses_oauth {
            Vec::new()
        } else {
            // 必须把**全部** key 都送回去，包括被禁用的。内核的整体更新路径是
            // 「DeleteAllAPIKeys 再按提交的清单重建」（admin_channels.go:1112），
            // 少送一把就等于把那把 key 从库里永久删掉，而禁用只是暂时停用、用户
            // 随时可能再启用。禁用状态不会丢：内核按 api_key 的值回查旧记录来
            // 恢复 Disabled（:1124），我们只要把 key 本身带回去就行。
            data.get("keys")
                .and_then(|k| k.as_array())
                .map(|ks| {
                    ks.iter()
                        .map(|k| {
                            serde_json::json!({
                                "api_key": k.get("api_key").cloned().unwrap_or_default(),
                                "note": k.get("note").cloned().unwrap_or_default(),
                            })
                        })
                        .collect()
                })
                .unwrap_or_default()
        };

        let priority = hop_priority(i);
        // priority 是**渠道级**属性，改它会影响该渠道服务的所有模型，不只是这条链
        // 上的别名。这是设计上的取舍（内核只按渠道优先级选路，没有 per-model 优先
        // 级），但不能悄悄发生 —— 把原值一起记进日志，用户看得见自己动了什么。
        let old_priority = channel
            .get("priority")
            .and_then(serde_json::Value::as_i64);
        let mut body = channel;
        if let Some(obj) = body.as_object_mut() {
            obj.insert("priority".into(), serde_json::json!(priority));
            // MERGE, never replace. A hop is usually bound to a channel the
            // user already uses for other models (channel 15 serves 10), and
            // overwriting `models` with just this alias would silently delete
            // every one of them. Upsert the alias entry and leave the rest.
            let mut models: Vec<serde_json::Value> = obj
                .get("models")
                .and_then(|m| m.as_array())
                .cloned()
                .unwrap_or_default();
            let entry = model_entry(&chain.alias, &hop.upstream);
            match models
                .iter()
                .position(|m| m.get("model") == entry.get("model"))
            {
                Some(idx) => models[idx] = entry,
                None => models.push(entry),
            }
            obj.insert("models".into(), serde_json::json!(models));
            obj.insert("api_keys".into(), serde_json::json!(api_keys));
            // Server-owned fields that Validate/ToConfig don't read.
            obj.remove("id");
            obj.remove("created_at");
            obj.remove("updated_at");
            obj.remove("key_count");
            if uses_oauth {
                // These are rejected on *presence*, not on value: the kernel
                // checks `if _, submitted := rawReq[field]` and 409s even when
                // we echo back exactly what it gave us. The editor payload
                // carries them, so they have to come out.
                obj.remove("key_strategy");
                for f in [
                    "oauth_credential",
                    "credential",
                    "access_token",
                    "refresh_token",
                    "id_token",
                ] {
                    obj.remove(f);
                }
                // Display-only fields the editor adds for OAuth channels.
                obj.remove("oauth_usage");
                obj.remove("anthropic_plan_type");
            }
        }
        state
            .admin
            .request(
                &base_url,
                &password,
                "PUT",
                &format!("channels/{id}"),
                None,
                Some(body),
            )
            .await
            .map_err(|e| AppError::Config(format!("hop {i} channel {id}: {e}")))?;
        log.push(format!(
            "hop {i} ({}): channel {id} priority={}→{priority}（影响该渠道全部模型） alias={} → {}",
            old_priority
                .map(|p| p.to_string())
                .unwrap_or_else(|| "?".into()),
            hop.upstream, chain.alias, hop.upstream
        ));
    }
    Ok(log)
}
