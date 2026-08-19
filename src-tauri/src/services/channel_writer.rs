//! 往内核渠道里写「别名 → 上游模型」和优先级的唯一入口。
//!
//! 模型链和调度图都要做同一件事：让某个渠道以某个优先级服务某个别名。这条路上
//! 有一串靠踩坑换来的规则，只能有一份实现：
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

use serde_json::Value;

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

    let mut body = channel;
    if let Some(obj) = body.as_object_mut() {
        if let Some(p) = priority {
            obj.insert("priority".into(), serde_json::json!(p));
        }

        let mut models: Vec<Value> = obj
            .get("models")
            .and_then(|m| m.as_array())
            .cloned()
            .unwrap_or_default();
        for (alias, upstream) in entries {
            let entry = model_entry(alias, upstream);
            match models
                .iter()
                .position(|m| m.get("model").and_then(Value::as_str) == Some(alias.as_str()))
            {
                Some(idx) => models[idx] = entry,
                None => models.push(entry),
            }
        }
        obj.insert("models".into(), serde_json::json!(models));
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

    Ok(ChannelPatch {
        channel_name,
        old_priority,
    })
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
