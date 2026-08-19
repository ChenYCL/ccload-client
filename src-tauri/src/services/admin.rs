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

        let token = body
            .pointer("/data/token")
            .or_else(|| body.get("token"))
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                AppError::Upstream {
                    status: status.as_u16(),
                    message: "login response did not contain a token".into(),
                }
            })?
            .to_string();

        *self.session.write().await = Some(Session {
            token: token.clone(),
            obtained: tokio::time::Instant::now(),
        });
        Ok(token)
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
