//! 「渠道自报用量」—— 问上游自己有没有 `/usage`。
//!
//! # 为什么需要这条路
//!
//! ccLoad 内核只会向 Codex / Anthropic / Antigravity / xAI / Z.ai 这 5 家
//! **OAuth** 渠道采样订阅额度。别的渠道（api_key 型）在它眼里就是个转发目标，
//! 整条链上没有任何位置拿得到它们的订阅用量 —— 订阅用量页对它们永远是空的。
//!
//! 但有些上游自己是知道的：比如 cursor2Oauth 这类代理，它手里有 Cursor 的
//! 凭证，能问出「本周期用了 90%」。所以这里定一个**自报契约**：上游只要在
//! 根地址下提供 `GET /usage`，返回的 `windows[]` 形状和内核给 OAuth 渠道的
//! `oauth_usage.windows[]` 一致，这一页就能把它和别的供应商并排画出来。
//!
//! 不写死「哪个渠道是 cursor」：按名字猜必然漂，而且换个代理就得再改一次。
//! 谁实现了这个契约谁就有额度可看。
//!
//! # 为什么是「点了才探」
//!
//! 不在进页面时自动探测所有渠道：那会朝一堆第三方上游发它们根本不认识的
//! `/usage` 请求，是在给别人的服务器添噪声。用户点一下才发。

use serde::Serialize;
use serde_json::Value;

use crate::error::AppError;
use crate::state::AppState;

/// 上游自报的用量。字段是内核 `oauthUsageWindow` 的子集 —— 客户端那一页照着
/// 它渲染，所以这里只收它认识的东西，多余的原样丢掉。
#[derive(Debug, Clone, Serialize)]
pub struct SelfReportedUsage {
    pub channel_id: i64,
    pub channel_name: String,
    /// 上游自称是谁（cursor / …），只用于显示。
    pub provider: String,
    pub plan_type: String,
    pub windows: Vec<Value>,
    /// 上游给的原话（Cursor 有 "You've used 90% of your included usage"）。
    /// 我们算出来的百分比要和它对得上；对不上就是我们解析错了。
    pub display_message: String,
}

/// 渠道的根地址 + 明文 key。
///
/// key 只有 `/editor` 才给（`GET /channels/{id}` 不返回明文，内核自带的编辑器
/// 走的就是 editor 那条）—— 这一点和 `channel_writer` 里的注释是同一个坑。
async fn channel_endpoint(
    state: &AppState,
    channel_id: i64,
) -> Result<(String, String, String), AppError> {
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
    let data = editor.get("data").unwrap_or(&editor);

    let name = data
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let url = data
        .pointer("/urls/0/url")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| AppError::Config(format!("渠道「{name}」没有配置 URL")))?;
    // 取第一把可用的 key。多 key 渠道随便哪把都能过 /usage 的鉴权。
    let key = data
        .pointer("/api_keys/0/api_key")
        .and_then(Value::as_str)
        .or_else(|| data.get("api_key").and_then(Value::as_str))
        .unwrap_or("")
        .to_string();
    Ok((name, url, key))
}

/// 去掉尾斜杠和结尾的 `/v1`。
///
/// 渠道里存的是内核转发用的根地址，多数人会连 `/v1` 一起填（OpenAI 兼容端点
/// 就长那样）。`/usage` 挂在**根**上，不剥掉就会去请求 `/v1/usage`，404。
fn root_of(raw: &str) -> String {
    let s = raw.trim().trim_end_matches('/');
    s.strip_suffix("/v1").unwrap_or(s).trim_end_matches('/').to_string()
}

/// 问一个渠道的上游要用量。上游没实现这个契约时返回 `Ok(None)`。
///
/// 区分「没实现」和「实现了但坏了」很重要：前者是绝大多数渠道的正常状态，
/// 不该在界面上报错；后者要把原因说出来。所以 404 / 405 归 None，其余归 Err。
pub async fn probe(state: &AppState, channel_id: i64) -> Result<Option<SelfReportedUsage>, AppError> {
    let (name, url, key) = channel_endpoint(state, channel_id).await?;
    let endpoint = format!("{}/usage", root_of(&url));

    // no_proxy：这类代理多半跑在 127.0.0.1，而这台机器上常年挂着 HTTP_PROXY，
    // 默认客户端会把回环请求也交给代理，表现是「服务明明起着却连不上」。
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| AppError::Config(format!("构建 HTTP 客户端失败：{e}")))?;

    let mut req = client.get(&endpoint);
    if !key.is_empty() {
        req = req.header("authorization", format!("Bearer {key}"));
    }
    let resp = req
        .send()
        .await
        .map_err(|e| AppError::Config(format!("{name}：连不上 {endpoint}（{e}）")))?;

    let status = resp.status();
    if status == reqwest::StatusCode::NOT_FOUND || status == reqwest::StatusCode::METHOD_NOT_ALLOWED
    {
        return Ok(None);
    }
    let body: Value = resp
        .json()
        .await
        .map_err(|_| AppError::Config(format!("{name}：{endpoint} 返回的不是 JSON")))?;
    if !status.is_success() {
        let msg = body
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("未知错误");
        return Err(AppError::Config(format!("{name}：{msg}")));
    }

    // 没有 windows 就当它不是这个契约 —— 别的服务也可能碰巧有个 /usage。
    let Some(windows) = body.get("windows").and_then(Value::as_array) else {
        return Ok(None);
    };
    if windows.is_empty() {
        return Ok(None);
    }

    Ok(Some(SelfReportedUsage {
        channel_id,
        channel_name: name,
        provider: body
            .get("provider")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        plan_type: body
            .get("plan_type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        windows: windows.clone(),
        display_message: body
            .get("display_message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `/usage` 挂在根上。渠道里存的地址多半带 `/v1`（OpenAI 兼容端点就长那样），
    /// 不剥掉就会去请求 `/v1/usage`，404。
    #[test]
    fn usage_hangs_off_the_root_not_v1() {
        for (raw, want) in [
            ("http://127.0.0.1:3000/v1", "http://127.0.0.1:3000"),
            ("http://127.0.0.1:3000/v1/", "http://127.0.0.1:3000"),
            ("http://127.0.0.1:3000/", "http://127.0.0.1:3000"),
            ("  http://127.0.0.1:3000  ", "http://127.0.0.1:3000"),
            // 只剥结尾那一个；路径中间的 v1 是人家的真实前缀。
            ("http://h/v1/proxy", "http://h/v1/proxy"),
        ] {
            assert_eq!(root_of(raw), want, "{raw}");
        }
    }
}
