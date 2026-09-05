//! 往内核渠道里写「别名 → 上游模型」和优先级的唯一入口。
//!
//! 模型链、强制路由、调度图和首选渠道钉住都要做同一件事：让某个渠道服务某个
//! 别名（或者不再服务）。这条路上有一串靠踩坑换来的规则，只能有一份实现：
//!
//! * `PUT /admin/channels/{id}` 是**整体更新**：body 里缺 name 会被 Validate
//!   拒掉，api-key 渠道缺 api_keys 也会（admin_types.go:177,186）。
//! * 明文 key 不会从 `GET /channels/{id}` 回来，只有 `GET /channels/{id}/editor`
//!   有（内核自带的 web 编辑器走的就是它）。所以先读 editor 再覆盖两个字段。
//! * OAuth 渠道（anthropic_oauth / codex_oauth / xai_oauth / antigravity_oauth）
//!   的 api_keys 是只读的，非空必 409；而 editor 会**合成**一个装着 access token
//!   的伪 key 用于展示，原样回传等于必炸。这类渠道要送空数组。
//! * 一批 OAuth 相关字段是按**是否出现**判断的，不看值，echo 回去照样 409。
//! * `models` 必须 upsert 而不是整体替换：一个渠道通常还服务着别的模型。
//! * api_keys 要**全量**回传，包括被禁用的：内核的整体更新会先
//!   DeleteAllAPIKeys 再按提交清单重建，少送一把就是永久删除。禁用状态内核会
//!   按 key 的值自己回填。

use serde_json::{Map, Value};

use crate::error::AppError;
use crate::state::AppState;

/// 一次写入的结果，供调用方生成人话日志。
pub struct ChannelPatch {
    pub channel_name: String,
    pub old_priority: Option<i64>,
}

/// 把 `entries`（别名 → 上游模型）合并进渠道的 models，并可选地设置优先级。
///
/// `priority` 传 `None` 表示不动优先级。注意优先级是**渠道级**属性，会影响该
/// 渠道服务的所有模型 —— 内核没有 per-model 优先级（`ModelEntry` 只有
/// `{model, redirect_model, disabled}`，选渠道是 `ORDER BY c.priority DESC`）。
pub async fn patch_channel(
    state: &AppState,
    channel_id: i64,
    priority: Option<i32>,
    entries: &[(String, String)],
) -> Result<ChannelPatch, AppError> {
    edit_channel(state, channel_id, |obj| {
        if let Some(p) = priority {
            obj.insert("priority".into(), serde_json::json!(p));
        }
        let mut models = models_of(obj);
        for (alias, upstream) in entries {
            let entry = model_entry(alias, upstream);
            match models.iter().position(|m| same_entry(m, alias)) {
                Some(idx) => models[idx] = entry,
                None => models.push(entry),
            }
        }
        obj.insert("models".into(), Value::Array(models));
        true
    })
    .await
}

/// 把这些别名的条目从渠道的 models 里拿掉。一个都不在时不发 PUT（幂等，且不
/// 白白触发一次 DeleteAllAPIKeys + 重建）。
pub async fn remove_models(
    state: &AppState,
    channel_id: i64,
    aliases: &[String],
) -> Result<ChannelPatch, AppError> {
    edit_channel(state, channel_id, |obj| {
        let mut models = models_of(obj);
        let before = models.len();
        models.retain(|m| !aliases.iter().any(|a| same_entry(m, a)));
        let changed = models.len() != before;
        if changed {
            obj.insert("models".into(), Value::Array(models));
        }
        changed
    })
    .await
}

/// 读 editor → 让 `edit` 改 body → PUT 回去。`edit` 返回 false 表示没改动、跳过 PUT。
async fn edit_channel(
    state: &AppState,
    channel_id: i64,
    edit: impl FnOnce(&mut Map<String, Value>) -> bool,
) -> Result<ChannelPatch, AppError> {
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
            &format!("channels/{channel_id}/editor"),
            None,
            None,
        )
        .await
        .map_err(|e| AppError::Config(format!("渠道 {channel_id} 读取失败：{e}")))?;

    let data = editor
        .get("data")
        .cloned()
        .ok_or_else(|| AppError::Config(format!("渠道 {channel_id}：editor 返回为空")))?;
    let channel = data
        .get("channel")
        .cloned()
        .ok_or_else(|| AppError::Config(format!("渠道 {channel_id}：editor 里没有 channel")))?;

    let uses_oauth = channel
        .get("auth_type")
        .and_then(Value::as_str)
        .is_some_and(|t| t != "api_key" && !t.is_empty());
    let api_keys: Vec<Value> = if uses_oauth {
        Vec::new()
    } else {
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

    let channel_name = channel
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("?")
        .to_string();
    let old_priority = channel.get("priority").and_then(Value::as_i64);
    let patch = ChannelPatch {
        channel_name,
        old_priority,
    };

    let mut body = channel;
    let Some(obj) = body.as_object_mut() else {
        return Err(AppError::Config(format!(
            "渠道 {channel_id}：editor 里的 channel 不是对象"
        )));
    };
    if !edit(obj) {
        return Ok(patch);
    }
    obj.insert("api_keys".into(), serde_json::json!(api_keys));

    // 服务端自己维护的字段，Validate/ToConfig 都不读。
    obj.remove("id");
    obj.remove("created_at");
    obj.remove("updated_at");
    obj.remove("key_count");

    if uses_oauth {
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
        obj.remove("oauth_usage");
        obj.remove("anthropic_plan_type");
    }

    state
        .admin
        .request(
            &base_url,
            &password,
            "PUT",
            &format!("channels/{channel_id}"),
            None,
            Some(body),
        )
        .await
        .map_err(|e| AppError::Config(format!("渠道 {channel_id} 写入失败：{e}")))?;

    Ok(patch)
}

fn models_of(obj: &Map<String, Value>) -> Vec<Value> {
    obj.get("models")
        .and_then(|m| m.as_array())
        .cloned()
        .unwrap_or_default()
}

/// 内核按条目自己的写法匹配别名（EqualFold）；这里同样忽略大小写，免得同一个别名
/// 因为大小写不同在渠道里落成两条。
fn same_entry(entry: &Value, alias: &str) -> bool {
    entry
        .get("model")
        .and_then(Value::as_str)
        .is_some_and(|m| m.trim().eq_ignore_ascii_case(alias.trim()))
}

/// 一条 `models[]` 条目。别名与上游相同时不写 redirect_model —— 内核把
/// `redirect_model == model` 当成一次无意义的重定向，留空更干净。
pub fn model_entry(alias: &str, upstream: &str) -> Value {
    if alias == upstream {
        serde_json::json!({ "model": alias })
    } else {
        serde_json::json!({ "model": alias, "redirect_model": upstream })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_matching_ignores_case_and_padding() {
        let e = serde_json::json!({"model": "Grok-4.6@ch21 "});
        assert!(same_entry(&e, "grok-4.6@ch21"));
        assert!(!same_entry(&e, "grok-4.6"));
        assert!(!same_entry(&serde_json::json!({"redirect_model": "x"}), "x"));
    }
}

/// 手动联调：`cargo test --lib live_private_alias -- --ignored --nocapture`。
///
/// 在真正服务 grok-4.6 的渠道上临时写一条私有别名，拿客户端令牌发一次 1 token 的
/// 请求，确认内核**只**把它路由到那个渠道，再把条目清掉。顺带确认不存在的私有别名
/// 会拿到 503（代理退让的触发条件）。
#[cfg(test)]
mod live {
    use super::*;

    #[tokio::test]
    #[ignore = "需要真实内核和凭据；会临时改一条渠道条目并发一次 1 token 请求"]
    async fn live_private_alias_round_trip() {
        use crate::services::pins::pinned_alias;
        let state = AppState::load().unwrap();
        let (cfg, token) = {
            let s = state.settings.read().await;
            (s.kernel.clone(), s.client_api_token.clone().unwrap())
        };
        let (base_url, password) = (cfg.base_url(), cfg.admin_password.clone());
        let channels = state
            .admin
            .request(&base_url, &password, "GET", "channels", None, None)
            .await
            .unwrap();
        let channels = channels["data"].as_array().cloned().unwrap_or_default();
        let (id, name) = channels
            .iter()
            .filter(|c| c["enabled"].as_bool().unwrap_or(true))
            .find(|c| {
                c["models"].as_array().is_some_and(|ms| {
                    ms.iter().any(|m| {
                        // 内核回来的条目里 redirect_model 常常等于 model 本身。
                        let upstream = m["redirect_model"].as_str().filter(|s| !s.is_empty());
                        m["model"].as_str() == Some("grok-4.6")
                            && upstream.is_none_or(|u| u == "grok-4.6")
                            && !m["disabled"].as_bool().unwrap_or(false)
                    })
                })
            })
            .map(|c| (c["id"].as_i64().unwrap(), c["name"].as_str().unwrap_or("?").to_string()))
            .expect("a channel that natively serves grok-4.6");
        eprintln!("native grok-4.6 channel: {id} ({name})");
        let private = pinned_alias("grok-4.6", id);

        let patch = patch_channel(&state, id, None, &[(private.clone(), "grok-4.6".into())])
            .await
            .unwrap();
        eprintln!("written {private} on {}", patch.channel_name);
        let ch = state
            .admin
            .request(&base_url, &password, "GET", &format!("channels/{id}"), None, None)
            .await
            .unwrap();
        let has = |ch: &serde_json::Value| {
            ch["data"]["models"]
                .as_array()
                .is_some_and(|ms| ms.iter().any(|m| m["model"].as_str() == Some(private.as_str())))
        };
        assert!(has(&ch), "entry should be present after patch");

        let http = crate::services::kernel::http_client_for_kernel(
            &cfg,
            crate::services::kernel::HttpClientOpts {
                timeout: Some(std::time::Duration::from_secs(90)),
                connect_timeout: std::time::Duration::from_secs(10),
                follow_system_proxy: true,
            },
        )
        .unwrap();
        let send = |model: String| {
            let http = http.clone();
            let url = format!("{base_url}/v1/messages");
            let token = token.clone();
            async move {
                let r = http
                    .post(&url)
                    .header("x-api-key", token)
                    .header("anthropic-version", "2023-06-01")
                    .json(&serde_json::json!({
                        "model": model,
                        "max_tokens": 1,
                        "messages": [{"role": "user", "content": "hi"}]
                    }))
                    .send()
                    .await
                    .unwrap();
                let status = r.status().as_u16();
                let body = r.text().await.unwrap_or_default();
                (status, body.chars().take(200).collect::<String>())
            }
        };
        let (st, body) = send(private.clone()).await;
        eprintln!("private alias -> {st}: {body}");
        assert_eq!(st, 200, "{body}");
        let (st2, body2) = send("grok-4.6@ch999999".into()).await;
        eprintln!("nonexistent private alias -> {st2}: {body2}");
        assert_eq!(st2, 503, "{body2}");

        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        let logs = state
            .admin
            .request(&base_url, &password, "GET", "logs", Some("limit=20"), None)
            .await
            .unwrap();
        let hit = logs["data"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|l| l["model"].as_str() == Some(private.as_str()) && l["status_code"].as_i64() == Some(200))
            .cloned()
            .expect("kernel log for the private alias");
        eprintln!(
            "log: channel_id={} model={} actual={} status={}",
            hit["channel_id"], hit["model"], hit["actual_model"], hit["status_code"]
        );
        assert_eq!(hit["channel_id"].as_i64(), Some(id), "must land on the pinned channel only");

        remove_models(&state, id, std::slice::from_ref(&private)).await.unwrap();
        let ch = state
            .admin
            .request(&base_url, &password, "GET", &format!("channels/{id}"), None, None)
            .await
            .unwrap();
        assert!(!has(&ch), "entry should be gone after remove");
        eprintln!("cleaned up");
    }
}
