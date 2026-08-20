//! Thin pass-through to the ccLoad admin API.
//!
//! Deliberately schema-agnostic: bodies in and out are `serde_json::Value`.
//! ccLoad grows fields every few releases (service_tier, cost_multiplier,
//! allowed_channel_ids, max_concurrency ...). Mirroring each one here would
//! mean a three-layer edit per upstream change, so this layer only owns
//! session handling, envelope unwrapping, and error mapping.
//!
//! Session shape verified against ccLoad @b67ac30:
//!   POST /login {"mode":"admin","password":"..."} -> {success, data:{token}}
//!   Note the `mode` field — README documents only `password`, which 400s.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::sync::RwLock;

use crate::error::AppError;
use crate::services::kernel::Kernel;

/// Admin sessions last 24h server-side; refresh well before that.
const SESSION_TTL: Duration = Duration::from_secs(20 * 60 * 60);

/// 内核自带 Web UI 的那条会话，跟下面 `Session` 不是一回事。
///
/// 内核后台是内核自己的页面，它的登录态是**浏览器侧**的三个 localStorage 键
/// （`ccload_token` / `ccload_token_expiry` / `ccload_web_role`，见
/// `vendor/ccLoad/web/assets/js/web-auth.js` 的 `storeWebSession`）。而
/// `AdminClient` 缓存的是 Rust 侧 reqwest 用的 bearer —— 两者各持一份，互不知道
/// 对方存在。所以「客户端连着内核」并不意味着「管理窗口登录了」，这也是用户会在
/// 远端模式下对着一个登录框发懵的原因。
pub struct WebSession {
    pub token: String,
    /// 内核回的 `expiresIn`（秒）。拿不到就是 0 —— 调用方**不要**在这种情况下
    /// 预置会话：Web UI 算的是 `now + expiresIn * 1000`，0 等于「刚建好就过期」，
    /// 用户开窗即被踢回登录页，比不预置更糟。
    pub expires_in_secs: u64,
    pub role: String,
}

struct Session {
    token: String,
    obtained: tokio::time::Instant,
}

impl Session {
    fn is_fresh(&self) -> bool {
        self.obtained.elapsed() < SESSION_TTL
    }
}

pub struct AdminClient {
    kernel: Arc<Kernel>,
    session: RwLock<Option<Session>>,
}

impl AdminClient {
    pub fn new(kernel: Arc<Kernel>) -> Arc<Self> {
        Arc::new(Self {
            kernel,
            session: RwLock::new(None),
        })
    }

    /// Drop the cached session, e.g. after the user edits connection settings.
    pub async fn invalidate(&self) {
        *self.session.write().await = None;
    }

    async fn cached_token(&self) -> Option<String> {
        let guard = self.session.read().await;
        guard
            .as_ref()
            .filter(|s| s.is_fresh())
            .map(|s| s.token.clone())
    }

    /// Log in as admin and cache the bearer token.
    pub async fn login(&self, base_url: &str, password: &str) -> Result<String, AppError> {
        let session = self.post_login(base_url, password).await?;
        *self.session.write().await = Some(Session {
            token: session.token.clone(),
            obtained: tokio::time::Instant::now(),
        });
        Ok(session.token)
    }

    /// 单独登一次，**不碰缓存** —— 给管理窗口预置登录态用。
    ///
    /// 不复用 `cached_token()`：那里只留了 token，没留 `expiresIn` / `role`，而 Web UI
    /// 三个键都要；而且缓存那条可能只剩几分钟寿命，塞进窗口等于让用户开窗即掉线。
    /// 多一次 `/login` 的代价远小于此。
    pub async fn web_session(
        &self,
        base_url: &str,
        password: &str,
    ) -> Result<WebSession, AppError> {
        self.post_login(base_url, password).await
    }

    async fn post_login(&self, base_url: &str, password: &str) -> Result<WebSession, AppError> {
        let resp = self
            .kernel
            .http()
            .post(format!("{base_url}/login"))
            .json(&json!({ "mode": "admin", "password": password }))
            .send()
            .await?;

        let status = resp.status();
        let body: Value = resp.json().await.unwrap_or(Value::Null);

        if !status.is_success() {
            return Err(AppError::Upstream {
                status: status.as_u16(),
                message: envelope_error(&body)
                    .unwrap_or_else(|| "login rejected".into()),
            });
        }

        let pick = |key: &str| body.pointer(&format!("/data/{key}")).or_else(|| body.get(key));

        let token = pick("token")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AppError::Upstream {
                    status: status.as_u16(),
                    message: "login response did not contain a token".into(),
                }
            })?
            .to_string();

        Ok(WebSession {
            expires_in_secs: pick("expiresIn").and_then(|v| v.as_u64()).unwrap_or(0),
            role: pick("role")
                .and_then(|v| v.as_str())
                .unwrap_or("admin")
                .to_string(),
            token,
        })
    }

    async fn token_for(&self, base_url: &str, password: &str) -> Result<String, AppError> {
        if let Some(t) = self.cached_token().await {
            return Ok(t);
        }
        self.login(base_url, password).await
    }
}

/// Pull `error` out of ccLoad's `StandardResponse` envelope.
fn envelope_error(body: &Value) -> Option<String> {
    body.get("error")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

impl AdminClient {
    /// Forward one admin request. Path is relative to `/admin`, e.g. `"channels"`.
    /// Query string is appended as-is when present. The body is sent only for
    /// methods that typically carry one — we still pass `None` for GET/DELETE.
    pub async fn request(
        &self,
        base_url: &str,
        password: &str,
        method: &str,
        path: &str,
        query: Option<&str>,
        body: Option<Value>,
    ) -> Result<Value, AppError> {
        let token = self.token_for(base_url, password).await?;
        let result = self
            .dispatch(base_url, &token, method, path, query, body.clone())
            .await;

        // A 401 after a successful login almost always means the session was
        // evicted (kernel restart, token TTL). Retry once after a fresh login.
        if let Err(AppError::Upstream { status: 401, .. }) = &result {
            self.invalidate().await;
            let token = self.login(base_url, password).await?;
            return self.dispatch(base_url, &token, method, path, query, body).await;
        }
        result
    }

    async fn dispatch(
        &self,
        base_url: &str,
        token: &str,
        method: &str,
        path: &str,
        query: Option<&str>,
        body: Option<Value>,
    ) -> Result<Value, AppError> {
        let path = path.trim_start_matches('/');
        let mut url = format!("{base_url}/admin/{path}");
        if let Some(q) = query.filter(|s| !s.is_empty()) {
            url.push('?');
            url.push_str(q);
        }

        let verb = method
            .parse::<reqwest::Method>()
            .map_err(|_| AppError::Config(format!("unsupported HTTP method {method}")))?;

        let mut req = self
            .kernel
            .http()
            .request(verb, &url)
            .bearer_auth(token);
        if let Some(b) = body {
            req = req.json(&b);
        }

        let resp = req.send().await?;
        let status = resp.status();
        let parsed: Value = resp.json().await.unwrap_or(Value::Null);

        if !status.is_success() {
            return Err(AppError::Upstream {
                status: status.as_u16(),
                message: envelope_error(&parsed)
                    .unwrap_or_else(|| format!("HTTP {status}")),
            });
        }
        Ok(parsed)
    }
}
