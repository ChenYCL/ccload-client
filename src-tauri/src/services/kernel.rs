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
use tokio::sync::Mutex;

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
    http: reqwest::Client,
}

impl Kernel {
    pub fn new() -> Arc<Self> {
        // no_proxy: the kernel is on loopback, and a system HTTP_PROXY (common
        // on this class of machine) would otherwise swallow admin calls.
        let http = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("build reqwest client");

        Arc::new(Self {
            child: Mutex::new(None),
            status: Mutex::new(KernelStatus::Stopped),
            http,
        })
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.http
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

            if let Ok(resp) = self.http.get(&health).send().await {
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
        let Ok(resp) = self.http.get(&url).send().await else {
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
    }
}
