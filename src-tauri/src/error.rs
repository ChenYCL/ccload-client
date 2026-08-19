use serde::Serialize;

/// Every failure surfaced to the renderer. Kept deliberately small: the
/// renderer only needs a stable machine-readable kind plus a human message.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("kernel is not running")]
    KernelNotRunning,

    #[error("kernel failed to become ready: {0}")]
    KernelNotReady(String),

    #[error("not authenticated against the ccLoad admin API")]
    NotAuthenticated,

    #[error("ccLoad returned {status}: {message}")]
    Upstream { status: u16, message: String },

    #[error("io error: {0}")]
    Io(String),

    #[error("{0}")]
    Config(String),

    #[error("network error: {0}")]
    Network(String),
}

impl AppError {
    fn kind(&self) -> &'static str {
        match self {
            Self::KernelNotRunning => "kernel_not_running",
            Self::KernelNotReady(_) => "kernel_not_ready",
            Self::NotAuthenticated => "not_authenticated",
            Self::Upstream { .. } => "upstream",
            Self::Io(_) => "io",
            Self::Config(_) => "config",
            Self::Network(_) => "network",
        }
    }
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}

impl From<reqwest::Error> for AppError {
    fn from(e: reqwest::Error) -> Self {
        Self::Network(e.to_string())
    }
}

/// Serialized shape the renderer receives on a rejected command.
#[derive(Serialize)]
pub struct SerializedError {
    kind: &'static str,
    message: String,
    /// Present only for upstream HTTP failures, so the UI can special-case 401.
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<u16>,
}

impl Serialize for AppErrorWire {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let status = match &self.0 {
            AppError::Upstream { status, .. } => Some(*status),
            _ => None,
        };
        SerializedError {
            kind: self.0.kind(),
            message: self.0.to_string(),
            status,
        }
        .serialize(s)
    }
}

/// Newtype so `AppError` can stay a plain `thiserror` enum while still
/// serializing structurally across the Tauri IPC boundary.
pub struct AppErrorWire(pub AppError);

impl<T: Into<AppError>> From<T> for AppErrorWire {
    fn from(e: T) -> Self {
        Self(e.into())
    }
}

pub type AppResult<T> = Result<T, AppErrorWire>;
