//! Shared types for CLI takeover.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::AppError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CliTarget {
    ClaudeCode,
    Codex,
    GeminiCli,
    GrokBuild,
    /// kebab-case 会得到 `open-code`，但产品名就是一个词 `opencode`（配置目录也是
    /// `~/.config/opencode`）。前端全线用 `opencode`，这里显式对齐 —— 磁盘上的
    /// 备份清单从没出现过 `open-code`（因为这条链路一直是坏的），不需要迁移。
    #[serde(rename = "opencode")]
    OpenCode,
}

impl CliTarget {
    pub fn label(self) -> &'static str {
        match self {
            Self::ClaudeCode => "Claude Code",
            Self::Codex => "Codex",
            Self::GeminiCli => "Gemini CLI",
            Self::GrokBuild => "Grok Build",
            Self::OpenCode => "OpenCode",
        }
    }

    pub(crate) fn relative_paths(self) -> &'static [&'static str] {
        match self {
            Self::ClaudeCode => &[".claude/settings.json"],
            Self::Codex => &[".codex/auth.json", ".codex/config.toml"],
            // `.env` carries the credential Gemini actually reads, so a restore
            // that skipped it would leave the takeover half-undone.
            Self::GeminiCli => &[".gemini/settings.json", ".gemini/.env"],
            Self::GrokBuild => &[".grok/config.toml"],
            Self::OpenCode => &[".config/opencode/opencode.json"],
        }
    }
}

/// Production uses the real home dir; development can redirect into a sandbox.
#[derive(Debug, Clone)]
pub struct ConfigRoot(PathBuf);

impl ConfigRoot {
    pub fn home() -> Result<Self, AppError> {
        dirs::home_dir()
            .map(Self)
            .ok_or_else(|| AppError::Config("could not resolve home directory".into()))
    }

    pub fn sandbox(path: PathBuf) -> Self {
        Self(path)
    }

    pub(crate) fn join(&self, rel: &str) -> PathBuf {
        self.0.join(rel)
    }
}

#[derive(Debug, Serialize)]
pub struct TakeoverPreview {
    pub target: CliTarget,
    pub label: &'static str,
    pub path: String,
    pub exists: bool,
    pub current_endpoint: Option<String>,
    pub next_endpoint: String,
    pub already_active: bool,
    /// Endpoint matches but the stored credential does not — the config looks
    /// taken over yet every call 401s. The UI surfaces this as a re-write hint.
    pub token_stale: bool,
    /// The model this CLI will send on the next launch, if we can read one.
    /// Grok's `models.default` is the profile name (`ccload`); this is the
    /// routed id (`grok-4.6` / `glm-5.3-flash[1M]`), which is what the user
    /// actually wants to change.
    pub current_model: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TakeoverResult {
    pub target: CliTarget,
    pub written: Vec<String>,
    /// Snapshot taken before this write; pass it to `cli_restore` to undo.
    pub backup_id: String,
    pub restart_required: bool,
}
