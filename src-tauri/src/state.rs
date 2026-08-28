//! App-wide state shared by every Tauri command.
//!
//! Persistence lives at `~/.ccload-client/settings.json`. The kernel's
//! generated password is stored there so a restart of the desktop app
//! reconnects to the same managed instance without prompting.

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::error::AppError;
use crate::services::admin::AdminClient;
use crate::services::cli_backup::BackupStore;
use crate::services::cli_config::ConfigRoot;
use crate::services::cli_io::write_atomic;
use crate::services::cli_proxy::CliProxy;
use crate::services::embed_proxy::EmbedProxy;
use crate::services::node_services::NodeServices;
use crate::services::kernel::{Kernel, KernelConfig};

/// Device-level settings. Not synced, not secrets-except-the-kernel-password
/// which we generate ourselves and never send off-box in managed mode.
///
/// 容器级 `#[serde(default)]`（而不是每个字段各挂一个）：字段级的那个填的是
/// **字段类型**的 Default，`bool` 就是 `false` —— 于是一份缺 `sandbox_cli_writes`
/// 的旧 settings.json 会解析成「沙箱关闭」，和下面 Default 里写的 true 正好相反，
/// 首次运行就会直接改写正在服务本次会话的 ~/.claude。容器级的才会去问
/// `AppSettings::default()`。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppSettings {
    pub kernel: KernelConfig,
    /// When true, CLI takeovers write under `~/.ccload-client/sandbox/`
    /// instead of the real home. Used by tests and cautious first-runs.
    pub sandbox_cli_writes: bool,
    /// The ccLoad API token we inject into CLI configs. Created on first
    /// successful kernel login and reused afterwards.
    pub client_api_token: Option<String>,
    /// CLI 的接管地址是否指向本地代理。
    ///
    /// 关掉不会停掉代理进程 —— 代理一直在跑，这个开关只决定**写进 CLI 配置的
    /// 地址**是代理还是内核。关着时会话归因和模型名改写都拿不到（内核日志里
    /// 没有 session_id），但 CLI 仍然能用。
    ///
    /// 默认 false：升级上来的用户配置里没有这一项，不该在他毫不知情时被改写
    /// 成另一个地址。第一次打开由用户在「CLI 接管」页显式点。
    #[serde(default)]
    pub route_cli_through_proxy: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            kernel: KernelConfig::default(),
            // Default on: a first-run must not rewrite a live ~/.claude that
            // is currently serving the session the user is running this from.
            sandbox_cli_writes: true,
            client_api_token: None,
            route_cli_through_proxy: false,
        }
    }
}

pub struct AppState {
    pub kernel: Arc<Kernel>,
    pub admin: Arc<AdminClient>,
    /// Loopback iframe proxy for the embedded admin UI. Started in setup();
    /// retargeted whenever kernel settings change.
    pub embed_proxy: RwLock<Option<Arc<EmbedProxy>>>,
    /// 数据面代理：CLI 都指向它，它转发到内核。会话标识只能在这一层拿到。
    pub cli_proxy: RwLock<Option<Arc<CliProxy>>>,
    /// 用户自己的 Node 常驻服务（MCP over http/sse、自定义后端）。
    pub node_services: Arc<NodeServices>,
    pub backups: BackupStore,
    pub settings: RwLock<AppSettings>,
    settings_path: PathBuf,
}

impl AppState {
    pub fn load() -> Result<Self, AppError> {
        let dir = dirs::home_dir()
            .ok_or_else(|| AppError::Config("no home directory".into()))?
            .join(".ccload-client");
        std::fs::create_dir_all(&dir)?;

        let settings_path = dir.join("settings.json");
        let existed = settings_path.exists();
        let mut settings = if existed {
            let raw = std::fs::read_to_string(&settings_path)?;
            serde_json::from_str(&raw)
                .map_err(|e| AppError::Config(format!("settings.json: {e}")))?
        } else {
            AppSettings::default()
        };

        if settings.kernel.data_dir.is_none() {
            settings.kernel.data_dir = Some(dir.join("data"));
        }
        // Heal a remote_url stored before base_url() learned to trim: the raw
        // string is what the settings form shows and what older builds wrote
        // into CLI configs, so clean it once here rather than at every use.
        settings.kernel.remote_url = settings
            .kernel
            .remote_url
            .map(|u| u.trim().trim_end_matches('/').to_string())
            .filter(|u| !u.is_empty());

        // First run: persist the generated admin password so a restart
        // reconnects to the same managed instance instead of minting a
        // new password that no longer matches the hashed one on disk.
        if !existed {
            let body = serde_json::to_string_pretty(&settings)
                .map_err(|e| AppError::Config(e.to_string()))?;
            // 走 write_atomic 而不是 fs::write：这份文件里有内核管理密码和
            // client_api_token，必须落成 0600 而不是 umask 给的 0644。
            write_atomic(&settings_path, &body)?;
        }

        let kernel = Kernel::new();
        let admin = AdminClient::new(Arc::clone(&kernel));
        let backups = BackupStore::new(dir.join("backups"));
        // 启动时就把损坏的清单救回来。否则用户点「移除」才会撞上
        // `trailing characters`，而那次写入本身又会把尾巴踩得更乱。
        let _ = backups.heal();
        Ok(Self {
            kernel,
            admin,
            embed_proxy: RwLock::new(None),
            cli_proxy: RwLock::new(None),
            node_services: NodeServices::new(),
            backups,
            settings: RwLock::new(settings),
            settings_path,
        })
    }

    /// Directory that owns settings.json; fallback chains live next to it.
    pub fn config_dir(&self) -> &std::path::Path {
        self.settings_path
            .parent()
            .expect("settings path always has a parent")
    }

    pub async fn persist(&self) -> Result<(), AppError> {
        let snap = self.settings.read().await.clone();
        let body = serde_json::to_string_pretty(&snap)
            .map_err(|e| AppError::Config(e.to_string()))?;
        // 同目录 rename 保证原子性，并且把 0600 贴回去 —— 见 cli_io::write_atomic。
        let path = self.settings_path.clone();
        tokio::task::spawn_blocking(move || write_atomic(&path, &body))
            .await
            .map_err(|e| AppError::Config(e.to_string()))??;
        Ok(())
    }

    pub async fn config_root(&self) -> Result<ConfigRoot, AppError> {
        let s = self.settings.read().await;
        if s.sandbox_cli_writes {
            let dir = self
                .settings_path
                .parent()
                .unwrap()
                .join("sandbox");
            tokio::fs::create_dir_all(&dir).await?;
            Ok(ConfigRoot::sandbox(dir))
        } else {
            ConfigRoot::home()
        }
    }
}
