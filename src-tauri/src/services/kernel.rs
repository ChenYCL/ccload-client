//! Ownership of the ccLoad kernel process.
//!
//! Two modes are supported and both end up behind the same `base_url`:
//!   * `Managed`  — we spawn the bundled `ccload` binary and own its lifetime.
//!   * `Remote`   — the user points us at an existing instance (VPS / HF Space).
//!
//! Measured on macOS with ccLoad @b67ac30: the kernel prints its banner and
//! finishes migrations within ~1s, but only binds the listener after two
//! network fetches (Antigravity manifest, model catalog sync) which cost
//! ~20s cold. So readiness MUST be polled against /health — never assume the
//! port is up just because the child process spawned.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use rand::Rng;
use serde::{Deserialize, Serialize};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, RwLock};

use crate::error::AppError;

/// How long we wait for /health after spawning before declaring failure.
const READY_TIMEOUT: Duration = Duration::from_secs(90);
const READY_POLL_INTERVAL: Duration = Duration::from_millis(400);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KernelMode {
    Managed,
    Remote,
}

/// Persisted connection settings. `admin_password` is only meaningful for
/// `Managed` mode, where we generate it once and reuse it across launches.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KernelConfig {
    pub mode: KernelMode,
    /// Managed mode: the loopback port we bind.
    pub port: u16,
    /// Remote mode: full origin, e.g. `https://user-ccload.hf.space`.
    pub remote_url: Option<String>,
    pub admin_password: String,
    /// Where the kernel keeps its SQLite file (managed mode only).
    pub data_dir: Option<PathBuf>,
    /// How to reach a *remote* kernel when the office network cannot.
    /// `http://` / `https://` / `socks5://127.0.0.1:1080` (`ssh -D` / Termius).
    /// Empty = direct (and we still ignore the system `HTTP_PROXY` — that
    /// path timed out against remote kernels and looked like a rejected token).
    /// Managed/loopback ignores this.
    #[serde(default)]
    pub outbound_proxy: Option<String>,
}

/// Whether two configs point at a different kernel *instance* (mode, port
/// or remote URL). Callers use this to invalidate per-instance state, e.g.
/// the client API token: one minted by kernel A is rejected by kernel B.
/// Password and data_dir are excluded — they don't change which instance
/// answers, and a token survives both.
pub fn kernel_identity_changed(a: &KernelConfig, b: &KernelConfig) -> bool {
    a.mode != b.mode || a.port != b.port || a.remote_url != b.remote_url
}

impl Default for KernelConfig {
    fn default() -> Self {
        Self {
            mode: KernelMode::Managed,
            port: 15722,
            remote_url: None,
            admin_password: generate_password(),
            data_dir: None,
            outbound_proxy: None,
        }
    }
}

impl KernelConfig {
    /// Origin every admin/proxy request is issued against.
    ///
    /// Surrounding whitespace is stripped before the trailing slash: a URL
    /// pasted with a stray leading space still parses for reqwest (the WHATWG
    /// URL parser trims it) but we also write this string verbatim into CLI
    /// config files, where ` https://host` is not the same value as
    /// `https://host` — and it silently breaks the takeover comparison.
    pub fn base_url(&self) -> String {
        match self.mode {
            KernelMode::Managed => format!("http://127.0.0.1:{}", self.port),
            KernelMode::Remote => self
                .remote_url
                .as_deref()
                .unwrap_or_default()
                .trim()
                .trim_end_matches('/')
                .to_string(),
        }
    }
}

/// Options for [`http_client_for_kernel`]. CLI proxy wants no response
/// timeout (SSE can run minutes); admin/health want a 30s cap.
pub struct HttpClientOpts {
    pub timeout: Option<Duration>,
    pub connect_timeout: Duration,
    /// When no explicit `outbound_proxy` is set: follow `HTTP_PROXY` or not.
    /// Admin/health must not — a system proxy once timed out against a remote
    /// kernel and looked like a rejected token. The CLI proxy historically
    /// did follow it.
    pub follow_system_proxy: bool,
}

fn direct_loopback_client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("build reqwest client")
}

/// `socks5://127.0.0.1:1080` / `http://127.0.0.1:7890`. Empty is `Ok(None)`.
pub fn parse_outbound_proxy(raw: &str) -> Result<Option<reqwest::Proxy>, AppError> {
    let s = raw.trim();
    if s.is_empty() {
        return Ok(None);
    }
    let scheme = s.split("://").next().unwrap_or("").to_ascii_lowercase();
    if !matches!(scheme.as_str(), "http" | "https" | "socks5" | "socks5h") {
        return Err(AppError::Config(
            "出口代理只认 http://、https://、socks5://（Termius / ssh -D 开出来的本地 SOCKS）".into(),
        ));
    }
    reqwest::Proxy::all(s).map(Some).map_err(|e| {
        AppError::Config(format!("出口代理地址无效：{e}"))
    })
}

/// HTTP client used to talk to the kernel (admin, health, CLI/embed proxies).
pub fn http_client_for_kernel(
    cfg: &KernelConfig,
    opts: HttpClientOpts,
) -> Result<reqwest::Client, AppError> {
    let mut b = reqwest::Client::builder().connect_timeout(opts.connect_timeout);
    if let Some(t) = opts.timeout {
        b = b.timeout(t);
    }
    b = b.pool_idle_timeout(Duration::from_secs(90));
    if cfg.mode == KernelMode::Managed {
        b = b.no_proxy();
    } else if let Some(p) = parse_outbound_proxy(cfg.outbound_proxy.as_deref().unwrap_or(""))? {
        // Explicit proxy + ignore the system one so Clash/office HTTP_PROXY
        // cannot steal the connection and 502 it.
        b = b.no_proxy().proxy(p);
    } else if !opts.follow_system_proxy {
        b = b.no_proxy();
    }
    b.build()
        .map_err(|e| AppError::Config(format!("build http client: {e}")))
}

fn generate_password() -> String {
    const CHARS: &[u8] = b"abcdefghijkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut rng = rand::thread_rng();
    (0..32)
        .map(|_| CHARS[rng.gen_range(0..CHARS.len())] as char)
        .collect()
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum KernelStatus {
    Stopped,
    Starting,
    Running { base_url: String, version: String },
    Failed { message: String },
}

pub struct Kernel {
    child: Mutex<Option<Child>>,
    status: Mutex<KernelStatus>,
    http: RwLock<reqwest::Client>,
}

impl Kernel {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            child: Mutex::new(None),
            status: Mutex::new(KernelStatus::Stopped),
            http: RwLock::new(direct_loopback_client()),
        })
    }

    pub async fn http(&self) -> reqwest::Client {
        self.http.read().await.clone()
    }

    /// Rebuild the admin/health client after connection settings change.
    pub async fn rebuild_http(&self, cfg: &KernelConfig) -> Result<(), AppError> {
        *self.http.write().await = http_client_for_kernel(
            cfg,
            HttpClientOpts {
                timeout: Some(Duration::from_secs(30)),
                connect_timeout: Duration::from_secs(5),
                // Admin calls to a managed kernel are loopback; never follow
                // HTTP_PROXY. Remote uses the explicit outbound proxy only.
                follow_system_proxy: false,
            },
        )?;
        Ok(())
    }

    pub async fn status(&self) -> KernelStatus {
        self.status.lock().await.clone()
    }

    async fn set_status(&self, s: KernelStatus) {
        *self.status.lock().await = s;
    }

    /// Bring the kernel up. For `Remote` mode this only verifies reachability;
    /// no process is spawned. Idempotent: a already-running managed child is
    /// left alone.
    pub async fn start(&self, cfg: &KernelConfig, binary: Option<PathBuf>) -> Result<(), AppError> {
        if let KernelStatus::Running { .. } = self.status().await {
            return Ok(());
        }
        self.rebuild_http(cfg).await?;
        self.set_status(KernelStatus::Starting).await;

        if cfg.mode == KernelMode::Managed {
            let bin = binary.ok_or_else(|| {
                AppError::Config("bundled ccload binary not found in resources".into())
            })?;
            self.spawn_managed(cfg, &bin).await?;
        }

        match self.await_ready(cfg).await {
            Ok(version) => {
                self.set_status(KernelStatus::Running {
                    base_url: cfg.base_url(),
                    version,
                })
                .await;
                Ok(())
            }
            Err(e) => {
                // Don't leave a half-alive child behind on a failed start.
                let _ = self.stop().await;
                self.set_status(KernelStatus::Failed {
                    message: e.to_string(),
                })
                .await;
                Err(e)
            }
        }
    }

    async fn spawn_managed(&self, cfg: &KernelConfig, bin: &PathBuf) -> Result<(), AppError> {
        let data_dir = cfg
            .data_dir
            .clone()
            .ok_or_else(|| AppError::Config("data_dir is required in managed mode".into()))?;
        tokio::fs::create_dir_all(&data_dir).await?;

        let mut cmd = Command::new(bin);
        cmd.current_dir(&data_dir)
            .env("CCLOAD_PASS", &cfg.admin_password)
            .env("PORT", cfg.port.to_string())
            .env("SQLITE_PATH", data_dir.join("ccload.db"))
            .env("GIN_MODE", "release")
            .env("GIN_LOG", "false")
            // Loopback only: the kernel must never be reachable off-box, since
            // its /v1/* surface forwards real upstream credentials.
            .env("TRUSTED_PROXIES", "none")
            .kill_on_drop(true);

        // Own process group so we can signal the whole tree on shutdown.
        #[cfg(unix)]
        cmd.process_group(0);

        let child = cmd.spawn().map_err(|e| {
            AppError::Io(format!("failed to spawn ccload at {}: {e}", bin.display()))
        })?;
        *self.child.lock().await = Some(child);
        Ok(())
    }

    /// Poll /health until it answers or we time out.
    async fn await_ready(&self, cfg: &KernelConfig) -> Result<String, AppError> {
        let health = format!("{}/health", cfg.base_url());
        let deadline = tokio::time::Instant::now() + READY_TIMEOUT;

        loop {
            // A managed child that already exited will never become ready.
            if cfg.mode == KernelMode::Managed {
                if let Some(child) = self.child.lock().await.as_mut() {
                    if let Ok(Some(exit)) = child.try_wait() {
                        return Err(AppError::KernelNotReady(format!(
                            "kernel exited early with {exit}"
                        )));
                    }
                }
            }

            if let Ok(resp) = self.http().await.get(&health).send().await {
                if resp.status().is_success() {
                    return Ok(self.probe_version(cfg).await);
                }
            }

            if tokio::time::Instant::now() >= deadline {
                return Err(AppError::KernelNotReady(format!(
                    "/health did not respond within {}s",
                    READY_TIMEOUT.as_secs()
                )));
            }
            tokio::time::sleep(READY_POLL_INTERVAL).await;
        }
    }

    /// Best-effort version read; an unknown version must not block startup.
    async fn probe_version(&self, cfg: &KernelConfig) -> String {
        let url = format!("{}/public/version", cfg.base_url());
        let Ok(resp) = self.http().await.get(&url).send().await else {
            return "unknown".into();
        };
        let Ok(body) = resp.json::<serde_json::Value>().await else {
            return "unknown".into();
        };
        body.pointer("/data/version")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string()
    }

    /// Terminate a managed child. No-op in remote mode.
    pub async fn stop(&self) -> Result<(), AppError> {
        if let Some(mut child) = self.child.lock().await.take() {
            let _ = child.start_kill();
            // Give it a moment to flush SQLite before it is reaped.
            let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
        }
        self.set_status(KernelStatus::Stopped).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(mode: KernelMode, port: u16, remote: Option<&str>) -> KernelConfig {
        KernelConfig {
            mode,
            port,
            remote_url: remote.map(str::to_string),
            admin_password: "x".into(),
            data_dir: None,
            outbound_proxy: None,
        }
    }

    /// A URL pasted with a stray leading space parses fine for reqwest but is
    /// also written verbatim into CLI config files, where the space survives
    /// and breaks the "is this already pointing at us?" comparison.
    #[test]
    fn base_url_strips_surrounding_whitespace() {
        let mut c = cfg(KernelMode::Remote, 0, Some(" https://example.com:8992 "));
        assert_eq!(c.base_url(), "https://example.com:8992");
        c.remote_url = Some("https://example.com:8992/".into());
        assert_eq!(c.base_url(), "https://example.com:8992");
    }

    #[test]
    fn identity_change_covers_instance_switches() {        // Same instance: only password/data_dir differ → token survives.
        let a = cfg(KernelMode::Managed, 15722, None);
        let mut b = a.clone();
        b.admin_password = "y".into();
        b.data_dir = Some(PathBuf::from("/tmp/d"));
        assert!(!kernel_identity_changed(&a, &b));

        // Different port, mode, or remote URL → different kernel.
        let mut c = a.clone();
        c.port = 15723;
        assert!(kernel_identity_changed(&a, &c));
        let mut d = a.clone();
        d.mode = KernelMode::Remote;
        d.remote_url = Some("https://r.example".into());
        assert!(kernel_identity_changed(&a, &d));
        let mut e = d.clone();
        e.remote_url = Some("https://other.example".into());
        assert!(kernel_identity_changed(&d, &e));

        // Outbound proxy is a path to the same instance, not a new one.
        let mut f = d.clone();
        f.outbound_proxy = Some("socks5://127.0.0.1:1080".into());
        assert!(!kernel_identity_changed(&d, &f));
    }

    #[test]
    fn outbound_proxy_rejects_unknown_schemes() {
        assert!(parse_outbound_proxy("").unwrap().is_none());
        assert!(parse_outbound_proxy("  ").unwrap().is_none());
        assert!(parse_outbound_proxy("socks5://127.0.0.1:1080").unwrap().is_some());
        assert!(parse_outbound_proxy("http://127.0.0.1:7890").unwrap().is_some());
        let err = parse_outbound_proxy("ftp://127.0.0.1:21").unwrap_err();
        assert!(err.to_string().contains("socks5"), "{err}");
    }
}
