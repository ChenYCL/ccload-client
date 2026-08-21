//! 本地混用：把「远端内核」和「本机跑的服务」拼进同一个渠道池。
//!
//! # 为什么需要这个东西
//!
//! 用户的场景是：ccLoad 内核跑在远端（VPS / HF Space），但本机还跑着一个
//! cursor2api 之类的 OpenAI 兼容服务，想两边一起用。
//!
//! 直觉做法是去远端后台加一个渠道、地址填 `http://127.0.0.1:3000` —— 这**一定
//! 不通**，而且失败方式很迷惑：远端内核解析这个地址得到的是**它自己**的回环口，
//! 不是用户的机器。它会去连自己的 3000 端口，然后报连接被拒。地址看着没错，
//! 日志也不会说「你搞错了拓扑」。
//!
//! 能走通的形状只有两种：
//!
//!   1. 把本机服务用隧道暴露到公网（ngrok / cloudflared / frp），远端才够得着。
//!      代价是本机服务上了公网，还多一个要维护的东西。
//!   2. **反过来**：本机也跑一个内核，把**远端 ccLoad 当成它的一个渠道**，
//!      cursor2api 当成另一个渠道，CLI 指向本机内核。
//!
//! 这个模块做的是第 2 种。它成立的原因是 ccLoad 内核本身就是一个标准的
//! OpenAI / Anthropic 兼容网关 —— 对本机内核来说，远端 ccLoad 和任何一家上游
//! 供应商没有区别。而本机内核既够得着 `127.0.0.1`，也够得着公网，是拓扑上
//! 唯一能同时看见两边的位置。
//!
//! 故障转移、协议转换、成本统计全部照旧由内核完成，客户端只负责把这两个渠道
//! 建出来 —— 建渠道走的是 `POST /admin/channels`，属于「把 Admin API 包装成
//! 界面」，不是在壳体里重做内核的事。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::error::AppError;
use crate::state::AppState;

/// 探测结果。
#[derive(Debug, Serialize)]
pub struct ProbeResult {
    /// 规范化之后我们真正会写进渠道的地址。
    pub base_url: String,
    pub ok: bool,
    /// 上游 `/v1/models` 给出的模型 id。
    pub models: Vec<String>,
    /// 失败原因，成功时为空。
    pub error: String,
}

/// 去掉尾斜杠和重复的 `/v1`。
///
/// 用户十有八九会把 CLI 里用的那个地址整个粘过来（`http://127.0.0.1:3000/v1`），
/// 而渠道里要填的是**根地址** —— 内核转发时自己会拼 `/v1/...`。不在这里剥掉的话
/// 上游收到的是 `/v1/v1/chat/completions`，报 404，而地址看上去完全正常。
pub fn normalize_base(raw: &str) -> String {
    let s = raw.trim().trim_end_matches('/');
    s.strip_suffix("/v1").unwrap_or(s).trim_end_matches('/').to_string()
}

/// 问一次 `{base}/v1/models`，确认这个地址真的是个 OpenAI 兼容服务。
///
/// 为什么要探测而不是直接建渠道：建完才发现地址错，用户看到的是几小时后某次
/// 请求莫名失败。这里 2 秒就能给出「通/不通 + 有哪些模型」，而且模型清单正好
/// 是建渠道必填的 `models`（内核要求 `min=1`）。
pub async fn probe(base_url: &str, api_key: Option<&str>) -> ProbeResult {
    let base = normalize_base(base_url);
    let endpoint = format!("{base}/v1/models");

    // no_proxy：这台机器上常年挂着 HTTP_PROXY，而这里多半打的是 127.0.0.1。
    // 默认客户端会把回环请求也交给代理，表现是「服务明明起着却连不上」。
    let client = match reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return ProbeResult {
                base_url: base,
                ok: false,
                models: Vec::new(),
                error: format!("构建 HTTP 客户端失败：{e}"),
            }
        }
    };

    let mut req = client.get(&endpoint);
    if let Some(k) = api_key.filter(|k| !k.is_empty()) {
        req = req.header("authorization", format!("Bearer {k}"));
        req = req.header("x-api-key", k);
    }

    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            return ProbeResult {
                base_url: base,
                ok: false,
                models: Vec::new(),
                error: format!("连不上 {endpoint}：{e}"),
            }
        }
    };
    let status = resp.status();
    let body: Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            return ProbeResult {
                base_url: base,
                ok: false,
                models: Vec::new(),
                error: format!("{endpoint} 返回的不是 JSON：{e}"),
            }
        }
    };
    if !status.is_success() {
        return ProbeResult {
            base_url: base,
            ok: false,
            models: Vec::new(),
            error: format!("{endpoint} 返回 HTTP {status}"),
        };
    }

    let models = extract_model_ids(&body);
    let error = if models.is_empty() {
        "服务通了，但 /v1/models 里没有任何模型；建渠道至少要有一个模型名，请手填".into()
    } else {
        String::new()
    };
    ProbeResult {
        base_url: base,
        ok: !models.is_empty(),
        models,
        error,
    }
}

/// 兼容两种常见形状：OpenAI 的 `{data:[{id}]}` 和裸数组 `[{id}]` / `["a","b"]`。
fn extract_model_ids(body: &Value) -> Vec<String> {
    let arr = body
        .get("data")
        .and_then(Value::as_array)
        .or_else(|| body.as_array());
    let Some(arr) = arr else { return Vec::new() };
    arr.iter()
        .filter_map(|m| {
            m.get("id")
                .and_then(Value::as_str)
                .or_else(|| m.as_str())
                .map(str::to_string)
        })
        .filter(|s| !s.is_empty())
        .collect()
}

/// 要建的一个渠道。
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelSpec {
    pub name: String,
    pub base_url: String,
    #[serde(default)]
    pub api_key: String,
    pub models: Vec<String>,
    /// 越大越先用。远端和本地谁优先由用户决定。
    #[serde(default)]
    pub priority: i32,
}

/// 建一个渠道。已存在同名渠道时直接报错而不是覆盖 —— 覆盖会把用户在后台
/// 调过的 key、限额、冷却规则一起抹掉，而那些东西这里根本不知道。
pub async fn create_channel(state: &AppState, spec: &ChannelSpec) -> Result<i64, AppError> {
    let (base_url, password) = {
        let s = state.settings.read().await;
        (s.kernel.base_url(), s.kernel.admin_password.clone())
    };

    let name = spec.name.trim();
    if name.is_empty() {
        return Err(AppError::Config("渠道名不能为空".into()));
    }
    if spec.models.is_empty() {
        return Err(AppError::Config(format!(
            "{name}：至少要有一个模型名，内核对 models 的要求是 min=1"
        )));
    }

    let existing = state
        .admin
        .request(&base_url, &password, "GET", "channels", None, None)
        .await
        .map_err(|e| AppError::Config(format!("读取渠道列表失败：{e}")))?;
    if let Some(arr) = existing.get("data").and_then(Value::as_array) {
        if arr
            .iter()
            .any(|c| c.get("name").and_then(Value::as_str) == Some(name))
        {
            return Err(AppError::Config(format!(
                "内核里已经有一个叫「{name}」的渠道了。改个名字，或去内核后台自己改那一条 —— \
                 这里不覆盖，免得把你在后台调过的 key / 限额 / 冷却规则一起抹掉。"
            )));
        }
    }

    let body = json!({
        "name": name,
        "api_key": spec.api_key,
        "urls": [{ "url": normalize_base(&spec.base_url) }],
        "models": spec.models.iter().map(|m| json!({ "model": m })).collect::<Vec<_>>(),
        "priority": spec.priority,
        "enabled": true,
    });

    let created = state
        .admin
        .request(&base_url, &password, "POST", "channels", None, Some(body))
        .await
        .map_err(|e| AppError::Config(format!("创建渠道「{name}」失败：{e}")))?;

    Ok(created
        .pointer("/data/id")
        .and_then(Value::as_i64)
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 用户会把 CLI 里那个带 `/v1` 的地址整个粘过来。不剥掉的话上游收到的是
    /// `/v1/v1/chat/completions`，404，而地址看上去完全正常。
    #[test]
    fn base_url_drops_trailing_v1_and_slash() {
        for (raw, want) in [
            ("http://127.0.0.1:3000/v1", "http://127.0.0.1:3000"),
            ("http://127.0.0.1:3000/v1/", "http://127.0.0.1:3000"),
            ("http://127.0.0.1:3000/", "http://127.0.0.1:3000"),
            ("  http://127.0.0.1:3000  ", "http://127.0.0.1:3000"),
            ("https://x.hf.space/v1", "https://x.hf.space"),
            // 只剥**结尾**那一个 v1；路径中间的 v1 是人家的真实前缀。
            ("http://h/v1/proxy", "http://h/v1/proxy"),
        ] {
            assert_eq!(normalize_base(raw), want, "{raw}");
        }
    }

    #[test]
    fn model_ids_from_both_shapes() {
        let openai = json!({ "data": [{ "id": "gpt-5" }, { "id": "claude-opus-5" }] });
        assert_eq!(extract_model_ids(&openai), ["gpt-5", "claude-opus-5"]);

        let bare = json!([{ "id": "a" }, { "id": "b" }]);
        assert_eq!(extract_model_ids(&bare), ["a", "b"]);

        let strings = json!(["a", "b"]);
        assert_eq!(extract_model_ids(&strings), ["a", "b"]);

        assert!(extract_model_ids(&json!({ "error": "nope" })).is_empty());
    }
}
