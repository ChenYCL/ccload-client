//! 统一管理 5 个 CLI 的扩展：MCP 服务器、Skill、Agent、Hook。
//!
//! `vision_mcp.rs` 只解决"把我们自己那一个 MCP 写进去"；这里是它的通用化版本：
//! 任意扩展、任意 CLI、可列举、可卸载，并且能把同一个扩展**一处配置推到多个
//! CLI**（`sync`）——这是本模块存在的理由，因为 5 个 CLI 的扩展格式互不兼容：
//!
//! | CLI         | MCP                                    | Skill                      | Agent                     | Hook                              |
//! |-------------|----------------------------------------|----------------------------|---------------------------|-----------------------------------|
//! | Claude Code | `~/.claude.json` → `mcpServers`        | `~/.claude/skills/`        | `~/.claude/agents/`       | `~/.claude/settings.json` → `hooks` |
//! | Codex       | `~/.codex/config.toml` → `mcp_servers` | `~/.codex/skills/`         | 不支持                    | `~/.codex/config.toml` → `[[hooks.X]]` |
//! | Gemini CLI  | `~/.gemini/settings.json` → `mcpServers`| `~/.gemini/skills/`       | `~/.gemini/agents/`       | `~/.gemini/settings.json` → `hooks` |
//! | Grok Build  | `~/.grok/config.toml` → `mcp_servers`  | `~/.grok/skills/`          | `~/.grok/agents/`         | `~/.grok/config.toml` → `[[hooks.X]]` |
//! | OpenCode    | `opencode.json` → `mcp`                | `~/.config/opencode/skill/`| `~/.config/opencode/agent/`| 不支持                            |
//!
//! Hook 事件名各家不同（Gemini 叫 `BeforeTool` 而非 `PreToolUse`），所以内部用
//! 一套规范事件名 `HookEvent`，写入时再翻译成目标 CLI 的原生名；目标 CLI 没有
//! 对应事件时报中文错误而不是悄悄丢掉。
//!
//! 写入安全性沿用 takeover 的三条规矩：先快照、只合并不覆盖、原子写。落在
//! `CliTarget::relative_paths()` 里的文件走 `BackupStore::snapshot`，落在外面的
//! （`~/.claude.json`）走 `backup_extra`。Skill/Agent 是目录/文件而不是配置项，
//! BackupStore 管不了，所以卸载与覆盖安装一律**改名归档**到
//! `~/.ccload-client/removed-extensions/<stamp>/`，绝不 `remove_dir_all`。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::error::AppError;
use crate::services::cli_backup::BackupStore;
use crate::services::cli_io::{object_at, read_json, write_atomic, write_pretty_json};
use crate::services::cli_types::{CliTarget, ConfigRoot};

/// 全部接管目标，顺序即 `sync` 自动找来源时的搜索顺序。
pub const ALL_TARGETS: [CliTarget; 5] = [
    CliTarget::ClaudeCode,
    CliTarget::Codex,
    CliTarget::GeminiCli,
    CliTarget::GrokBuild,
    CliTarget::OpenCode,
];

// ---------------------------------------------------------------------------
// 类型
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExtensionKind {
    Mcp,
    Skill,
    Agent,
    Hook,
}

impl ExtensionKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Mcp => "MCP 服务器",
            Self::Skill => "Skill",
            Self::Agent => "Agent",
            Self::Hook => "Hook",
        }
    }

    fn all() -> [Self; 4] {
        [Self::Mcp, Self::Skill, Self::Agent, Self::Hook]
    }
}

/// 规范化的 hook 事件。各 CLI 的原生名由 `native_name` 翻译。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HookEvent {
    PreToolUse,
    PostToolUse,
    UserPromptSubmit,
    SessionStart,
    SessionEnd,
    Stop,
    SubagentStop,
    PreCompact,
    Notification,
}

impl HookEvent {
    fn key(self) -> &'static str {
        match self {
            Self::PreToolUse => "PreToolUse",
            Self::PostToolUse => "PostToolUse",
            Self::UserPromptSubmit => "UserPromptSubmit",
            Self::SessionStart => "SessionStart",
            Self::SessionEnd => "SessionEnd",
            Self::Stop => "Stop",
            Self::SubagentStop => "SubagentStop",
            Self::PreCompact => "PreCompact",
            Self::Notification => "Notification",
        }
    }

    fn all() -> [Self; 9] {
        [
            Self::PreToolUse,
            Self::PostToolUse,
            Self::UserPromptSubmit,
            Self::SessionStart,
            Self::SessionEnd,
            Self::Stop,
            Self::SubagentStop,
            Self::PreCompact,
            Self::Notification,
        ]
    }

    /// 目标 CLI 里这个事件叫什么；`None` 表示该 CLI 根本没有这个事件。
    /// 事件名来自实测：Claude Code / Grok 用同一套名字，Codex 的事件枚举取自
    /// 其二进制内的 `HookEventsToml`，Gemini 的取自其 bundle 里的 settings schema。
    fn native_name(self, target: CliTarget) -> Option<&'static str> {
        match target {
            CliTarget::ClaudeCode | CliTarget::GrokBuild => Some(self.key()),
            CliTarget::Codex => match self {
                // Codex 有 PermissionRequest/SubagentStart，但没有 Notification。
                Self::Notification => None,
                other => Some(other.key()),
            },
            CliTarget::GeminiCli => match self {
                Self::PreToolUse => Some("BeforeTool"),
                Self::PostToolUse => Some("AfterTool"),
                Self::SessionStart => Some("SessionStart"),
                Self::SessionEnd => Some("SessionEnd"),
                Self::PreCompact => Some("PreCompress"),
                Self::Notification => Some("Notification"),
                // Gemini 的事件模型里没有"用户提交提示词"和"回合结束"。
                Self::UserPromptSubmit | Self::Stop | Self::SubagentStop => None,
            },
            CliTarget::OpenCode => None,
        }
    }

    /// 反向查表：把目标 CLI 的原生事件名还原成规范事件。
    fn from_native(target: CliTarget, name: &str) -> Option<Self> {
        Self::all()
            .into_iter()
            .find(|e| e.native_name(target) == Some(name))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum McpTransport {
    Stdio,
    Http,
}

/// 一个扩展的规范化描述。字段按 kind 取用：install 前 `validate` 会检查该
/// kind 需要的字段是否齐全，缺了就报中文错误，不做静默降级。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ExtensionSpec {
    /// MCP 服务器名 / skill 目录名 / agent 文件名 / hook 的稳定 id。
    pub id: String,
    pub description: Option<String>,

    // ---- MCP ----
    pub transport: Option<McpTransport>,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub url: Option<String>,
    pub headers: BTreeMap<String, String>,
    /// 只有显式 `false` 才会写进配置（OpenCode 例外，它惯例上总写 enabled）。
    pub enabled: Option<bool>,

    // ---- Skill / Agent ----
    /// 完整 markdown。已经带 `---` frontmatter 就原样写入，否则用 id +
    /// description 合成一份最小 frontmatter。
    pub body: Option<String>,

    // ---- Hook ----
    pub event: Option<HookEvent>,
    /// 匹配哪些工具，如 `Bash|Write`。省略等于匹配全部。
    pub matcher: Option<String>,
    pub hook_command: Option<String>,
    pub timeout: Option<u64>,
}

/// 列表项。`detail` 是该条目在配置文件里的原始片段，UI 直接展示即可。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionItem {
    pub target: CliTarget,
    pub kind: ExtensionKind,
    pub id: String,
    pub label: String,
    pub description: Option<String>,
    /// 该条目所在文件/目录的绝对路径。
    pub source: String,
    pub enabled: bool,
    pub detail: Value,
}

/// 支持矩阵的一行，给前端置灰按钮用。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionSupport {
    pub target: CliTarget,
    pub label: &'static str,
    pub kind: ExtensionKind,
    pub supported: bool,
    /// 支持时是写到哪个文件/目录（相对 home）。
    pub path: Option<&'static str>,
}

/// `sync` 的逐目标结果。一个目标失败不影响其他目标，失败原因原样带回。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncOutcome {
    pub target: CliTarget,
    pub label: &'static str,
    pub ok: bool,
    pub written: Vec<String>,
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------
// 支持矩阵与路径
// ---------------------------------------------------------------------------

/// Codex 解析 HTTP MCP 请求头用的键名。`headers` 它根本不看。
const HTTP_HEADERS_KEY: &str = "http_headers";

/// MCP 配置落在哪个文件（相对 home）。5 家都支持 MCP。
fn mcp_path(target: CliTarget) -> &'static str {
    match target {
        // 注意：Claude Code 的 MCP 不在 ~/.claude/settings.json 里（实测该文件
        // 只有 env/model/statusLine 之类），全局 MCP 在 ~/.claude.json 顶层。
        CliTarget::ClaudeCode => ".claude.json",
        CliTarget::Codex => ".codex/config.toml",
        CliTarget::GeminiCli => ".gemini/settings.json",
        CliTarget::GrokBuild => ".grok/config.toml",
        CliTarget::OpenCode => ".config/opencode/opencode.json",
    }
}

/// Skill 目录。第一个是写入目标，其余只在列举/卸载时扫描——OpenCode 官方用
/// 单数 `skill/`，但机器上常见被别的工具建出来的 `skills/`，两个都要认。
fn skill_dirs(target: CliTarget) -> &'static [&'static str] {
    match target {
        CliTarget::ClaudeCode => &[".claude/skills"],
        CliTarget::Codex => &[".codex/skills"],
        CliTarget::GeminiCli => &[".gemini/skills"],
        CliTarget::GrokBuild => &[".grok/skills"],
        CliTarget::OpenCode => &[".config/opencode/skill", ".config/opencode/skills"],
    }
}

/// Agent 目录。Codex 没有子 agent 定义目录（只有 AGENTS.md 那种项目说明）。
fn agent_dirs(target: CliTarget) -> &'static [&'static str] {
    match target {
        CliTarget::ClaudeCode => &[".claude/agents"],
        CliTarget::Codex => &[],
        CliTarget::GeminiCli => &[".gemini/agents"],
        CliTarget::GrokBuild => &[".grok/agents"],
        CliTarget::OpenCode => &[".config/opencode/agent", ".config/opencode/agents"],
    }
}

/// Hook 配置文件。OpenCode 只有 `experimental.hook.file_edited` /
/// `session_completed` 那套完全不同的模型，无法映射，按不支持处理。
fn hook_path(target: CliTarget) -> Option<&'static str> {
    match target {
        CliTarget::ClaudeCode => Some(".claude/settings.json"),
        CliTarget::Codex => Some(".codex/config.toml"),
        CliTarget::GeminiCli => Some(".gemini/settings.json"),
        CliTarget::GrokBuild => Some(".grok/config.toml"),
        CliTarget::OpenCode => None,
    }
}

pub fn supports(target: CliTarget, kind: ExtensionKind) -> bool {
    primary_path(target, kind).is_some()
}

/// 该 (target, kind) 的主写入路径，`None` 即不支持。
fn primary_path(target: CliTarget, kind: ExtensionKind) -> Option<&'static str> {
    match kind {
        ExtensionKind::Mcp => Some(mcp_path(target)),
        ExtensionKind::Skill => skill_dirs(target).first().copied(),
        ExtensionKind::Agent => agent_dirs(target).first().copied(),
        ExtensionKind::Hook => hook_path(target),
    }
}

/// 完整支持矩阵，前端拿它决定哪些按钮可点。
pub fn support_matrix() -> Vec<ExtensionSupport> {
    let mut rows = Vec::new();
    for target in ALL_TARGETS {
        for kind in ExtensionKind::all() {
            let path = primary_path(target, kind);
            rows.push(ExtensionSupport {
                target,
                label: target.label(),
                kind,
                supported: path.is_some(),
                path,
            });
        }
    }
    rows
}

fn unsupported(target: CliTarget, kind: ExtensionKind) -> AppError {
    AppError::Config(format!(
        "{} 不支持{}，无法写入（该 CLI 没有对应的配置位置）",
        target.label(),
        kind.label()
    ))
}

// ---------------------------------------------------------------------------
// 备份
// ---------------------------------------------------------------------------

/// 写配置文件之前的快照。文件在该 target 的备份清单里就走 `snapshot`（可被
/// `cli_restore` 一键回滚），否则走 `backup_extra` 留一份带时间戳的原始拷贝。
fn snapshot_before_write(
    root: &ConfigRoot,
    target: CliTarget,
    path: &Path,
    stamp: &str,
    reason: &str,
    backups: &BackupStore,
) -> Result<(), AppError> {
    let tracked = target
        .relative_paths()
        .iter()
        .any(|rel| root.join(rel) == path);
    if tracked {
        backups.snapshot(root, target, stamp, reason)?;
    } else {
        // 不在接管文件集里的文件也要进清单，否则「可回滚」就是空话。
        // path 一定是 root.join(rel) 来的，反推 rel 交给 snapshot_extra 存。
        let rel = path
            .strip_prefix(root.join(""))
            .map(|p| p.display().to_string())
            .unwrap_or_else(|_| {
                path.file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("config")
                    .to_string()
            });
        backups.snapshot_extra(root, target, &rel, stamp, reason)?;
    }
    Ok(())
}

/// Skill 目录 / Agent 文件的"删除" = 整体改名搬进归档目录。
/// 用 rename 而不是复制+删除：原子、保留符号链接本身（不会误删链接指向的
/// 真实技能目录）、也不会漏掉 skill 目录里的 references/scripts 等附属文件。
fn archive(root: &ConfigRoot, src: &Path, target: CliTarget, kind: ExtensionKind, id: &str, stamp: &str) -> Result<String, AppError> {
    let dir = root
        .join(".ccload-client/removed-extensions")
        .join(stamp);
    std::fs::create_dir_all(&dir)?;
    let name = format!("{}-{:?}-{}", id, kind, target.label().replace(' ', "-"));
    let dest = dir.join(name);
    std::fs::rename(src, &dest)?;
    Ok(dest.display().to_string())
}

/// 归档一份**拷贝**（原件留在原地）。覆盖写 skill 目录里的 SKILL.md 时用它：
/// 旧内容要留得住，但整个目录不能被搬走。
fn archive_copy(
    root: &ConfigRoot,
    src: &Path,
    target: CliTarget,
    kind: ExtensionKind,
    id: &str,
    stamp: &str,
) -> Result<String, AppError> {
    let dir = root.join(".ccload-client/removed-extensions").join(stamp);
    std::fs::create_dir_all(&dir)?;
    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| format!(".{e}"))
        .unwrap_or_default();
    let name = format!(
        "{}-{:?}-{}{ext}",
        id,
        kind,
        target.label().replace(' ', "-")
    );
    let dest = dir.join(name);
    std::fs::copy(src, &dest)?;
    Ok(dest.display().to_string())
}

// ---------------------------------------------------------------------------
// 校验
// ---------------------------------------------------------------------------

/// id 会直接变成文件名/目录名/配置 key，必须挡住路径穿越。
fn validate_id(id: &str) -> Result<(), AppError> {
    let id = id.trim();
    if id.is_empty() {
        return Err(AppError::Config("扩展 id 不能为空".into()));
    }
    if id.contains('/') || id.contains('\\') || id.contains("..") || id.starts_with('.') {
        return Err(AppError::Config(format!(
            "扩展 id 非法：{id}（不能包含 / \\ .. 或以 . 开头）"
        )));
    }
    Ok(())
}

fn validate(kind: ExtensionKind, spec: &ExtensionSpec) -> Result<(), AppError> {
    validate_id(&spec.id)?;
    match kind {
        ExtensionKind::Mcp => match spec.transport {
            Some(McpTransport::Stdio) | None => {
                if spec.command.as_deref().unwrap_or("").trim().is_empty() {
                    return Err(AppError::Config(
                        "stdio 类型的 MCP 必须提供 command".into(),
                    ));
                }
            }
            Some(McpTransport::Http) => {
                if spec.url.as_deref().unwrap_or("").trim().is_empty() {
                    return Err(AppError::Config("http 类型的 MCP 必须提供 url".into()));
                }
            }
        },
        ExtensionKind::Skill | ExtensionKind::Agent => {
            if spec.body.as_deref().unwrap_or("").trim().is_empty() {
                return Err(AppError::Config(format!(
                    "{}必须提供 body（markdown 正文）",
                    kind.label()
                )));
            }
        }
        ExtensionKind::Hook => {
            if spec.event.is_none() {
                return Err(AppError::Config("Hook 必须指定 event（触发事件）".into()));
            }
            if spec.hook_command.as_deref().unwrap_or("").trim().is_empty() {
                return Err(AppError::Config(
                    "Hook 必须提供 hookCommand（要执行的命令）".into(),
                ));
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// 对外入口
// ---------------------------------------------------------------------------

/// 列出某个 CLI 已装的扩展。`kind` 为 `None` 时列全部四类。
/// 不支持的组合直接跳过（列举场景返回空比报错有用）。
pub fn list(
    root: &ConfigRoot,
    target: CliTarget,
    kind: Option<ExtensionKind>,
) -> Result<Vec<ExtensionItem>, AppError> {
    let kinds: Vec<ExtensionKind> = match kind {
        Some(k) => vec![k],
        None => ExtensionKind::all().to_vec(),
    };
    let mut items = Vec::new();
    for k in kinds {
        if !supports(target, k) {
            continue;
        }
        match k {
            ExtensionKind::Mcp => items.extend(list_mcp(root, target)?),
            ExtensionKind::Skill => items.extend(list_docs(root, target, ExtensionKind::Skill)?),
            ExtensionKind::Agent => items.extend(list_docs(root, target, ExtensionKind::Agent)?),
            ExtensionKind::Hook => items.extend(list_hooks(root, target)?),
        }
    }
    Ok(items)
}

/// 安装/覆盖一个扩展，返回被写入的文件（或归档路径）。
pub fn install(
    root: &ConfigRoot,
    target: CliTarget,
    kind: ExtensionKind,
    spec: &ExtensionSpec,
    stamp: &str,
    backups: &BackupStore,
) -> Result<Vec<String>, AppError> {
    if !supports(target, kind) {
        return Err(unsupported(target, kind));
    }
    validate(kind, spec)?;
    match kind {
        ExtensionKind::Mcp => write_mcp(root, target, spec, Some(spec), stamp, backups),
        ExtensionKind::Skill | ExtensionKind::Agent => {
            write_doc(root, target, kind, spec, stamp, backups)
        }
        ExtensionKind::Hook => write_hook(root, target, spec, stamp, backups),
    }
}

/// 卸载一个扩展。找不到时报中文错误，不当成成功。
pub fn remove(
    root: &ConfigRoot,
    target: CliTarget,
    kind: ExtensionKind,
    id: &str,
    stamp: &str,
    backups: &BackupStore,
) -> Result<Vec<String>, AppError> {
    if !supports(target, kind) {
        return Err(unsupported(target, kind));
    }
    validate_id(id)?;
    match kind {
        ExtensionKind::Mcp => {
            let spec = ExtensionSpec {
                id: id.to_string(),
                ..Default::default()
            };
            write_mcp(root, target, &spec, None, stamp, backups)
        }
        ExtensionKind::Skill | ExtensionKind::Agent => {
            remove_doc(root, target, kind, id, stamp)
        }
        ExtensionKind::Hook => remove_hook(root, target, id, stamp, backups),
    }
}

/// 把某个已装扩展读回规范化的 `ExtensionSpec`——`sync` 的第一步，也让 UI 能
/// 把现有配置填进编辑框。
pub fn read_spec(
    root: &ConfigRoot,
    target: CliTarget,
    kind: ExtensionKind,
    id: &str,
) -> Result<ExtensionSpec, AppError> {
    if !supports(target, kind) {
        return Err(unsupported(target, kind));
    }
    validate_id(id)?;
    match kind {
        ExtensionKind::Mcp => read_mcp_spec(root, target, id),
        ExtensionKind::Skill | ExtensionKind::Agent => read_doc_spec(root, target, kind, id),
        ExtensionKind::Hook => read_hook_spec(root, target, id),
    }
}

/// 把一个扩展同步到多个 CLI。`source` 省略时按 `ALL_TARGETS` 顺序找第一个
/// 装了这个 id 的 CLI 当来源。逐目标独立成败，不会因为某家不支持就整体失败。
pub fn sync(
    root: &ConfigRoot,
    kind: ExtensionKind,
    id: &str,
    targets: &[CliTarget],
    source: Option<CliTarget>,
    stamp: &str,
    backups: &BackupStore,
) -> Result<Vec<SyncOutcome>, AppError> {
    validate_id(id)?;
    if targets.is_empty() {
        return Err(AppError::Config("请至少选择一个要同步到的 CLI".into()));
    }

    let source = match source {
        Some(s) => {
            if !supports(s, kind) {
                return Err(unsupported(s, kind));
            }
            s
        }
        None => ALL_TARGETS
            .into_iter()
            .find(|t| {
                supports(*t, kind)
                    && list(root, *t, Some(kind))
                        .map(|items| items.iter().any(|i| i.id == id))
                        .unwrap_or(false)
            })
            .ok_or_else(|| {
                AppError::Config(format!(
                    "没有任何 CLI 装了名为 {id} 的{}，无法确定同步来源",
                    kind.label()
                ))
            })?,
    };

    let spec = read_spec(root, source, kind, id)?;

    let mut outcomes = Vec::new();
    for (i, target) in targets.iter().copied().enumerate() {
        if target == source {
            outcomes.push(SyncOutcome {
                target,
                label: target.label(),
                ok: true,
                written: Vec::new(),
                error: Some("同步来源，已跳过".into()),
            });
            continue;
        }
        // 每个目标一个独立 stamp：backup_extra 的文件名以 stamp 开头，同一个
        // stamp 写多个目标会互相覆盖备份。
        let target_stamp = format!("{stamp}-{i}");
        match install(root, target, kind, &spec, &target_stamp, backups) {
            Ok(written) => outcomes.push(SyncOutcome {
                target,
                label: target.label(),
                ok: true,
                written,
                error: None,
            }),
            Err(e) => outcomes.push(SyncOutcome {
                target,
                label: target.label(),
                ok: false,
                written: Vec::new(),
                error: Some(e.to_string()),
            }),
        }
    }
    Ok(outcomes)
}

// ---------------------------------------------------------------------------
// MCP
// ---------------------------------------------------------------------------

fn mcp_json_key(target: CliTarget) -> &'static str {
    match target {
        CliTarget::OpenCode => "mcp",
        _ => "mcpServers",
    }
}

fn list_mcp(root: &ConfigRoot, target: CliTarget) -> Result<Vec<ExtensionItem>, AppError> {
    let rel = mcp_path(target);
    let path = root.join(rel);
    let mut items = Vec::new();

    if matches!(target, CliTarget::Codex | CliTarget::GrokBuild) {
        let doc = read_toml(&path)?;
        let Some(servers) = doc.get("mcp_servers").and_then(|i| i.as_table_like()) else {
            return Ok(items);
        };
        for (id, item) in servers.iter() {
            let Some(table) = item.as_table_like() else {
                continue;
            };
            let detail = toml_table_to_json(table);
            items.push(ExtensionItem {
                target,
                kind: ExtensionKind::Mcp,
                id: id.to_string(),
                label: id.to_string(),
                description: mcp_summary(&detail),
                source: path.display().to_string(),
                enabled: detail
                    .get("enabled")
                    .and_then(Value::as_bool)
                    .unwrap_or(true),
                detail,
            });
        }
        return Ok(items);
    }

    let doc = read_json(&path)?;
    let Some(servers) = doc.get(mcp_json_key(target)).and_then(Value::as_object) else {
        return Ok(items);
    };
    for (id, detail) in servers {
        items.push(ExtensionItem {
            target,
            kind: ExtensionKind::Mcp,
            id: id.clone(),
            label: id.clone(),
            description: mcp_summary(detail),
            source: path.display().to_string(),
            enabled: detail
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            detail: detail.clone(),
        });
    }
    Ok(items)
}

/// 一行摘要：URL 或 `command args…`，给列表页当副标题。
fn mcp_summary(detail: &Value) -> Option<String> {
    if let Some(url) = detail.get("url").and_then(Value::as_str) {
        return Some(url.to_string());
    }
    let command = match detail.get("command") {
        // OpenCode 的 command 是数组 ["exe", "arg"]，其余是字符串。
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" "),
        Some(Value::String(s)) => {
            let args = detail
                .get("args")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            if args.is_empty() {
                s.clone()
            } else {
                format!("{s} {args}")
            }
        }
        _ => String::new(),
    };
    (!command.is_empty()).then_some(command)
}

/// 写入（`spec` 为 `Some`）或删除（`None`）一个 MCP 条目。
/// 只动 `mcpServers/mcp/mcp_servers` 下的这一个 key，其余原样保留。
fn write_mcp(
    root: &ConfigRoot,
    target: CliTarget,
    ident: &ExtensionSpec,
    spec: Option<&ExtensionSpec>,
    stamp: &str,
    backups: &BackupStore,
) -> Result<Vec<String>, AppError> {
    let path = root.join(mcp_path(target));
    snapshot_before_write(root, target, &path, stamp, "extension-mcp", backups)?;

    if matches!(target, CliTarget::Codex | CliTarget::GrokBuild) {
        let mut doc = read_toml(&path)?;
        match spec {
            Some(spec) => {
                // 从**已有条目**出发，只覆盖我们建模的字段。整表替换会把
                // `startup_timeout_sec` / `cwd` / `bearer_token_env_var` 这些
                // ExtensionSpec 没有的字段一起抹掉 —— 用户只是进来加个 header，
                // 存一下就丢了超时配置。
                let mut table = doc
                    .get("mcp_servers")
                    .and_then(|s| s.as_table_like())
                    .and_then(|t| t.get(&ident.id))
                    .and_then(|i| i.as_table())
                    .cloned()
                    .unwrap_or_default();
                match spec.transport.unwrap_or(McpTransport::Stdio) {
                    McpTransport::Stdio => {
                        table["command"] =
                            toml_edit::value(spec.command.clone().unwrap_or_default());
                        if spec.args.is_empty() {
                            table.remove("args");
                        } else {
                            table["args"] = toml_edit::value(toml_array(&spec.args));
                        }
                        if spec.env.is_empty() {
                            table.remove("env");
                        } else {
                            table["env"] = toml_edit::Item::Value(toml_edit::Value::InlineTable(
                                toml_inline(&spec.env),
                            ));
                        }
                        // 换成 stdio 后，上一形态的 http 字段必须清掉，否则
                        // 两套字段同时存在，CLI 按哪套走全看它自己的优先级。
                        for k in ["type", "url", HTTP_HEADERS_KEY, "headers"] {
                            table.remove(k);
                        }
                    }
                    McpTransport::Http => {
                        // Codex 需要显式 type="http"；Grok 靠 url 字段自己判断，
                        // 多写一个 type 也无害，两家统一。
                        table["type"] = toml_edit::value("http");
                        table["url"] = toml_edit::value(spec.url.clone().unwrap_or_default());
                        // Codex 解析的键是 `http_headers`，不是 `headers` ——
                        // 写成 headers 的话它一个头都不会发，带鉴权的服务全部 401。
                        // 用户机器上的 `[mcp_servers.zread.http_headers]` 就是证据。
                        let key = if target == CliTarget::Codex {
                            HTTP_HEADERS_KEY
                        } else {
                            "headers"
                        };
                        table.remove(if key == "headers" { HTTP_HEADERS_KEY } else { "headers" });
                        if spec.headers.is_empty() {
                            table.remove(key);
                        } else {
                            table[key] = toml_edit::Item::Value(toml_edit::Value::InlineTable(
                                toml_inline(&spec.headers),
                            ));
                        }
                        for k in ["command", "args", "env"] {
                            table.remove(k);
                        }
                    }
                }
                if spec.enabled == Some(false) {
                    table["enabled"] = toml_edit::value(false);
                } else {
                    table.remove("enabled");
                }
                // 必须先把 mcp_servers 落成真正的 Table：直接
                // `doc["mcp_servers"][id] = Item::Table(..)` 在父表还不存在时
                // 会塌缩成 `mcp_servers = {}`，条目全丢。
                let servers = doc["mcp_servers"]
                    .or_insert(toml_edit::table())
                    .as_table_mut()
                    .ok_or_else(|| {
                        AppError::Config(format!("{} 的 mcp_servers 不是表", path.display()))
                    })?;
                servers[&ident.id] = toml_edit::Item::Table(table);
            }
            None => {
                let existed = doc
                    .get("mcp_servers")
                    .and_then(|s| s.as_table_like())
                    .map(|t| t.contains_key(&ident.id))
                    .unwrap_or(false);
                if !existed {
                    return Err(not_found(target, ExtensionKind::Mcp, &ident.id));
                }
                if let Some(t) = doc["mcp_servers"].as_table_like_mut() {
                    t.remove(&ident.id);
                }
            }
        }
        write_atomic(&path, &doc.to_string())?;
        return Ok(vec![path.display().to_string()]);
    }

    let mut doc = read_json(&path)?;
    let key = mcp_json_key(target);
    match spec {
        Some(spec) => {
            // 同样从已有条目出发合并：Claude Code 的条目上可能挂着我们没建模的
            // 字段（`disabled`、`scope`、各家自定义的扩展键），整体替换会连同
            // 它们一起消失。
            let servers = object_at(&mut doc, key)?;
            let existing = servers.get(&ident.id).cloned();
            servers.insert(
                ident.id.clone(),
                mcp_json_entry(target, spec, existing.as_ref()),
            );
        }
        None => {
            let removed = doc
                .get_mut(key)
                .and_then(Value::as_object_mut)
                .and_then(|m| m.remove(&ident.id));
            if removed.is_none() {
                return Err(not_found(target, ExtensionKind::Mcp, &ident.id));
            }
        }
    }
    write_pretty_json(&path, &doc)?;
    Ok(vec![path.display().to_string()])
}

/// 各家 JSON MCP 的形状不同：Claude Code / Gemini 用 `{type,command,args,env}`，
/// OpenCode 用 `{type:"local"|"remote", command:[...], environment}`。
///
/// `existing` 是这个 id 当前在配置里的样子（新装时为 None）。我们**在它之上覆盖**
/// 自己建模的字段，而不是造一个全新对象丢回去 —— ExtensionSpec 只表达了 MCP 的
/// 公共子集，各家还有一堆自有字段，整体替换等于每次编辑都把它们清空一次。
fn mcp_json_entry(target: CliTarget, spec: &ExtensionSpec, existing: Option<&Value>) -> Value {
    let transport = spec.transport.unwrap_or(McpTransport::Stdio);
    let mut entry: Map<String, Value> = existing
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    if target == CliTarget::OpenCode {
        match transport {
            McpTransport::Stdio => {
                let mut command = vec![spec.command.clone().unwrap_or_default()];
                command.extend(spec.args.iter().cloned());
                entry.insert("type".into(), Value::String("local".into()));
                entry.insert(
                    "command".into(),
                    Value::Array(command.into_iter().map(Value::String).collect()),
                );
                entry.insert("environment".into(), string_map_to_json(&spec.env));
                // 换形态时清掉另一形态残留的字段。
                entry.remove("url");
                entry.remove("headers");
            }
            McpTransport::Http => {
                entry.insert("type".into(), Value::String("remote".into()));
                entry.insert(
                    "url".into(),
                    Value::String(spec.url.clone().unwrap_or_default()),
                );
                entry.insert("headers".into(), string_map_to_json(&spec.headers));
                entry.remove("command");
                entry.remove("environment");
            }
        }
        // OpenCode 惯例上总写 enabled。
        entry.insert("enabled".into(), Value::Bool(spec.enabled.unwrap_or(true)));
        return Value::Object(entry);
    }

    match transport {
        McpTransport::Stdio => {
            entry.insert("type".into(), Value::String("stdio".into()));
            entry.insert(
                "command".into(),
                Value::String(spec.command.clone().unwrap_or_default()),
            );
            if spec.args.is_empty() {
                entry.remove("args");
            } else {
                entry.insert(
                    "args".into(),
                    Value::Array(spec.args.iter().cloned().map(Value::String).collect()),
                );
            }
            if spec.env.is_empty() {
                entry.remove("env");
            } else {
                entry.insert("env".into(), string_map_to_json(&spec.env));
            }
            entry.remove("url");
            entry.remove("headers");
        }
        McpTransport::Http => {
            // 只在原本不是 http 系（sse/http）时才改写 type：一个
            // `{"type":"sse","url":…}` 的条目被我们读成 Http（只看 url 有没有），
            // 存回去若强行写成 "http"，用户只是来加个 header，工作正常的 SSE
            // 服务器就被悄悄换掉了。
            let keeps_type = entry
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|t| t == "sse" || t == "http" || t == "streamable-http");
            if !keeps_type {
                entry.insert("type".into(), Value::String("http".into()));
            }
            entry.insert(
                "url".into(),
                Value::String(spec.url.clone().unwrap_or_default()),
            );
            if spec.headers.is_empty() {
                entry.remove("headers");
            } else {
                entry.insert("headers".into(), string_map_to_json(&spec.headers));
            }
            entry.remove("command");
            entry.remove("args");
            entry.remove("env");
        }
    }
    if spec.enabled == Some(false) {
        entry.insert("enabled".into(), Value::Bool(false));
    } else {
        entry.remove("enabled");
    }
    Value::Object(entry)
}

fn read_mcp_spec(
    root: &ConfigRoot,
    target: CliTarget,
    id: &str,
) -> Result<ExtensionSpec, AppError> {
    let detail = list_mcp(root, target)?
        .into_iter()
        .find(|i| i.id == id)
        .ok_or_else(|| not_found(target, ExtensionKind::Mcp, id))?
        .detail;

    let mut spec = ExtensionSpec {
        id: id.to_string(),
        enabled: detail.get("enabled").and_then(Value::as_bool),
        ..Default::default()
    };
    if let Some(url) = detail.get("url").and_then(Value::as_str) {
        spec.transport = Some(McpTransport::Http);
        spec.url = Some(url.to_string());
        // OpenCode / Codex / Grok 都叫 headers；Codex 另有 http_headers 变体。
        for key in ["headers", "http_headers"] {
            if let Some(h) = detail.get(key).and_then(Value::as_object) {
                spec.headers.extend(json_to_string_map(h));
            }
        }
    } else {
        spec.transport = Some(McpTransport::Stdio);
        match detail.get("command") {
            // OpenCode 把 command 和 args 合成一个数组，拆回来。
            Some(Value::Array(parts)) => {
                let mut it = parts.iter().filter_map(Value::as_str);
                spec.command = it.next().map(str::to_string);
                spec.args = it.map(str::to_string).collect();
            }
            Some(Value::String(s)) => {
                spec.command = Some(s.clone());
                if let Some(args) = detail.get("args").and_then(Value::as_array) {
                    spec.args = args.iter().filter_map(Value::as_str).map(str::to_string).collect();
                }
            }
            _ => {}
        }
        for key in ["env", "environment"] {
            if let Some(e) = detail.get(key).and_then(Value::as_object) {
                spec.env.extend(json_to_string_map(e));
            }
        }
    }
    Ok(spec)
}

// ---------------------------------------------------------------------------
// Skill / Agent（都是磁盘上的 markdown，只是布局不同）
// ---------------------------------------------------------------------------

fn doc_dirs(target: CliTarget, kind: ExtensionKind) -> &'static [&'static str] {
    match kind {
        ExtensionKind::Skill => skill_dirs(target),
        _ => agent_dirs(target),
    }
}

/// Skill 是 `<dir>/<id>/SKILL.md`，Agent 是 `<dir>/<id>.md`。
fn doc_file(base: &Path, kind: ExtensionKind, id: &str) -> PathBuf {
    match kind {
        ExtensionKind::Skill => base.join(id).join("SKILL.md"),
        _ => base.join(format!("{id}.md")),
    }
}

/// 卸载/覆盖时要搬走的东西：skill 搬整个目录，agent 搬那个 .md。
fn doc_root(base: &Path, kind: ExtensionKind, id: &str) -> PathBuf {
    match kind {
        ExtensionKind::Skill => base.join(id),
        _ => base.join(format!("{id}.md")),
    }
}

fn list_docs(
    root: &ConfigRoot,
    target: CliTarget,
    kind: ExtensionKind,
) -> Result<Vec<ExtensionItem>, AppError> {
    let mut items = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for rel in doc_dirs(target, kind) {
        let base = root.join(rel);
        let Ok(entries) = std::fs::read_dir(&base) else {
            continue; // 目录不存在 = 没装扩展，不是错误
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            // 用 metadata（跟随符号链接）判断类型：这些目录里大量条目是软链。
            let Ok(meta) = std::fs::metadata(entry.path()) else {
                continue;
            };
            let id = match kind {
                ExtensionKind::Skill => {
                    if !meta.is_dir() {
                        continue;
                    }
                    name
                }
                _ => {
                    if !meta.is_file() || !name.ends_with(".md") {
                        continue;
                    }
                    name.trim_end_matches(".md").to_string()
                }
            };
            if !seen.insert(id.clone()) {
                continue; // 高优先级目录已经收过同名条目
            }
            let file = doc_file(&base, kind, &id);
            let raw = std::fs::read_to_string(&file).unwrap_or_default();
            if kind == ExtensionKind::Skill && raw.is_empty() && !file.exists() {
                continue; // 目录里没有 SKILL.md，不是一个技能
            }
            let (frontmatter, _) = split_frontmatter(&raw);
            items.push(ExtensionItem {
                target,
                kind,
                id: id.clone(),
                label: frontmatter
                    .as_deref()
                    .and_then(|fm| frontmatter_value(fm, "name"))
                    .unwrap_or_else(|| id.clone()),
                description: frontmatter
                    .as_deref()
                    .and_then(|fm| frontmatter_value(fm, "description")),
                source: doc_root(&base, kind, &id).display().to_string(),
                enabled: true,
                detail: json!({
                    "file": file.display().to_string(),
                    "symlink": std::fs::symlink_metadata(doc_root(&base, kind, &id))
                        .map(|m| m.file_type().is_symlink())
                        .unwrap_or(false),
                }),
            });
        }
    }
    Ok(items)
}

fn write_doc(
    root: &ConfigRoot,
    target: CliTarget,
    kind: ExtensionKind,
    spec: &ExtensionSpec,
    stamp: &str,
    backups: &BackupStore,
) -> Result<Vec<String>, AppError> {
    let _ = backups; // 文件型扩展用归档而不是 BackupStore，见模块头注释
    let base = root.join(
        doc_dirs(target, kind)
            .first()
            .ok_or_else(|| unsupported(target, kind))?,
    );
    let mut written = Vec::new();

    // 覆盖安装时怎么处理旧版本，取决于旧版本是什么：
    //   * 符号链接 —— 整体搬走。顺着链接写会改掉别人的源目录。
    //   * skill 目录 —— **只归档 SKILL.md 的旧内容**，目录本身留在原地。
    //     一个 skill 常常还带 `scripts/`、`references/`、数据文件，它们是这个
    //     skill 的组成部分；整体 rename 走再只写回一个 SKILL.md，等于用户点一次
    //     「保存」就把自己的脚本搬进了 removed-extensions，而 SKILL.md 里还写着
    //     「运行 scripts/xxx.py」。
    //   * 单文件 agent —— 整体搬走，本来就没有附属文件。
    let existing = doc_root(&base, kind, &spec.id);
    match std::fs::symlink_metadata(&existing) {
        Ok(m) if m.file_type().is_symlink() || !m.is_dir() => {
            written.push(format!(
                "{} (已归档)",
                archive(root, &existing, target, kind, &spec.id, stamp)?
            ));
        }
        Ok(_) => {
            let doc = doc_file(&base, kind, &spec.id);
            if doc.exists() {
                written.push(format!(
                    "{} (旧版本已归档)",
                    archive_copy(root, &doc, target, kind, &spec.id, stamp)?
                ));
            }
        }
        Err(_) => {}
    }

    let file = doc_file(&base, kind, &spec.id);
    let body = spec.body.clone().unwrap_or_default();
    write_atomic(
        &file,
        &ensure_frontmatter(&spec.id, spec.description.as_deref(), &body),
    )?;
    written.push(file.display().to_string());
    Ok(written)
}

fn remove_doc(
    root: &ConfigRoot,
    target: CliTarget,
    kind: ExtensionKind,
    id: &str,
    stamp: &str,
) -> Result<Vec<String>, AppError> {
    // 所有候选目录都要清。OpenCode 官方用单数 `skill/`，机器上又常被别的工具
    // 建出 `skills/`，同一个 id 可能两边都有；只删第一个的话删完再列还在。
    let mut archived = Vec::new();
    for (n, rel) in doc_dirs(target, kind).iter().enumerate() {
        let existing = doc_root(&root.join(rel), kind, id);
        if std::fs::symlink_metadata(&existing).is_ok() {
            // 同一个 stamp 下的归档名只由 id/kind/target 决定，两个目录都命中
            // 就会撞名 —— 只给第二个起加后缀，常见情形的归档路径保持不变。
            let stamp = if n == 0 {
                stamp.to_string()
            } else {
                format!("{stamp}-{n}")
            };
            archived.push(format!(
                "{} (已归档)",
                archive(root, &existing, target, kind, id, &stamp)?
            ));
        }
    }
    if archived.is_empty() {
        return Err(not_found(target, kind, id));
    }
    Ok(archived)
}

fn read_doc_spec(
    root: &ConfigRoot,
    target: CliTarget,
    kind: ExtensionKind,
    id: &str,
) -> Result<ExtensionSpec, AppError> {
    for rel in doc_dirs(target, kind) {
        let file = doc_file(&root.join(rel), kind, id);
        if let Ok(raw) = std::fs::read_to_string(&file) {
            let (frontmatter, _) = split_frontmatter(&raw);
            return Ok(ExtensionSpec {
                id: id.to_string(),
                description: frontmatter
                    .as_deref()
                    .and_then(|fm| frontmatter_value(fm, "description")),
                // 整份 markdown 原样带走，同步到别的 CLI 时逐字复制，
                // 不丢 model/tools 之类我们不认识的 frontmatter 字段。
                body: Some(raw),
                ..Default::default()
            });
        }
    }
    Err(not_found(target, kind, id))
}

/// `---\n…\n---\n` 头部与正文。没有 frontmatter 时返回 `(None, 全文)`。
fn split_frontmatter(raw: &str) -> (Option<String>, String) {
    let trimmed = raw.trim_start_matches('\u{feff}');
    let Some(rest) = trimmed.strip_prefix("---") else {
        return (None, raw.to_string());
    };
    let rest = rest.strip_prefix('\n').or_else(|| rest.strip_prefix("\r\n"));
    let Some(rest) = rest else {
        return (None, raw.to_string());
    };
    match rest.find("\n---") {
        Some(end) => {
            let body = rest[end + 4..].trim_start_matches(['\r', '\n']).to_string();
            (Some(rest[..end].to_string()), body)
        }
        None => (None, raw.to_string()),
    }
}

/// 只取顶层 `key: value`，够用来读 name/description；不做完整 YAML 解析。
fn frontmatter_value(frontmatter: &str, key: &str) -> Option<String> {
    for line in frontmatter.lines() {
        let Some(rest) = line.strip_prefix(key) else {
            continue;
        };
        let Some(rest) = rest.strip_prefix(':') else {
            continue;
        };
        let v = rest.trim();
        let v = v
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .map(|s| s.replace("\\\"", "\"").replace("\\\\", "\\"))
            .unwrap_or_else(|| v.to_string());
        if !v.is_empty() {
            return Some(v);
        }
    }
    None
}

/// 保证正文带 frontmatter，并让表单里的「描述」真的落到 `description:` 上。
///
/// 5 家 CLI 都靠 `name` / `description` 决定何时加载一个 skill/agent，所以描述
/// 就是加载触发器，不是装饰。以前这里遇到「正文已带 frontmatter」就原样返回，
/// 而 `read_doc_spec` 读回的 body 恰恰**包含** frontmatter —— 于是任何一次编辑
/// 都走这条短路，改描述永远是空操作，界面还报「✓ 已写入」。
fn ensure_frontmatter(id: &str, description: Option<&str>, body: &str) -> String {
    if let (Some(front), rest) = split_frontmatter(body) {
        let front = match description {
            Some(d) => upsert_yaml_key(&front, "description", d),
            None => front,
        };
        let mut out = format!("---\n{}\n---\n\n{}", front.trim_end(), rest.trim_end());
        out.push('\n');
        return out;
    }
    format!(
        "---\nname: {}\ndescription: {}\n---\n\n{}\n",
        yaml_scalar(id),
        yaml_scalar(description.unwrap_or("")),
        body.trim_end()
    )
}

/// 在 frontmatter 里替换（或追加）一个顶层标量键，其余行逐字保留。
///
/// 故意不做「解析成 YAML 再序列化」：那会重排键、丢注释、把用户写的块标量改成
/// 别的形状。这里只动目标那一行。缩进过的同名键是嵌套结构的一部分，不能碰。
fn upsert_yaml_key(front: &str, key: &str, value: &str) -> String {
    let prefix = format!("{key}:");
    let mut out: Vec<String> = Vec::new();
    let mut replaced = false;
    for line in front.lines() {
        if !replaced && line.starts_with(&prefix) {
            out.push(format!("{key}: {}", yaml_scalar(value)));
            replaced = true;
        } else {
            out.push(line.to_string());
        }
    }
    if !replaced {
        out.push(format!("{key}: {}", yaml_scalar(value)));
    }
    out.join("\n")
}

/// 双引号 YAML 标量，避免描述里的冒号/换行破坏 frontmatter。
fn yaml_scalar(s: &str) -> String {
    format!(
        "\"{}\"",
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace(['\n', '\r'], " ")
    )
}

// ---------------------------------------------------------------------------
// Hook
// ---------------------------------------------------------------------------

/// 磁盘上一条 hook 的身份是 `事件 + matcher + 命令`：同一个脚本挂在
/// `Bash` 和 `Write` 两个 matcher 下是**两条**独立的 hook。
fn norm_matcher(matcher: &str) -> &str {
    let m = matcher.trim();
    if m.is_empty() {
        "*"
    } else {
        m
    }
}

/// hook 在配置里没有名字，所以 id 由 `事件 + matcher + 命令的哈希` 推导：确定性、
/// 跨 CLI 稳定（规范事件名不随目标变化）、也不用往别人的配置里塞额外字段。
///
/// matcher 必须参与哈希。漏掉它的话，同一条命令挂在两个 matcher 下会得到同一个
/// id：列表里出现两行完全一样的条目（React 还会撞 key），删任意一行都会把两条
/// 一起删掉，点「编辑」第二行加载到的却是第一行的 matcher。
fn hook_id(event: HookEvent, matcher: &str, command: &str) -> String {
    let mut h: u32 = 0x811c_9dc5;
    for b in norm_matcher(matcher)
        .as_bytes()
        .iter()
        .chain(b"\0")
        .chain(command.trim().as_bytes())
    {
        h ^= u32::from(*b);
        h = h.wrapping_mul(0x0100_0193);
    }
    format!("{}-{h:08x}", event.key())
}

/// `{matcher, hooks:[{type,command,timeout}]}` 分组 → 逐条 ExtensionItem。
fn hook_items_from_groups(
    target: CliTarget,
    event: HookEvent,
    groups: &[Value],
    source: &str,
) -> Vec<ExtensionItem> {
    let mut items = Vec::new();
    for group in groups {
        let matcher = group
            .get("matcher")
            .and_then(Value::as_str)
            .unwrap_or("*")
            .to_string();
        let Some(hooks) = group.get("hooks").and_then(Value::as_array) else {
            continue;
        };
        for hook in hooks {
            let Some(command) = hook.get("command").and_then(Value::as_str) else {
                continue;
            };
            items.push(ExtensionItem {
                target,
                kind: ExtensionKind::Hook,
                id: hook_id(event, &matcher, command),
                label: format!("{} · {matcher}", event.key()),
                description: Some(command.to_string()),
                source: source.to_string(),
                enabled: true,
                detail: json!({
                    "event": event.key(),
                    "nativeEvent": event.native_name(target),
                    "matcher": matcher,
                    "command": command,
                    "timeout": hook.get("timeout").and_then(Value::as_u64),
                }),
            });
        }
    }
    items
}

fn list_hooks(root: &ConfigRoot, target: CliTarget) -> Result<Vec<ExtensionItem>, AppError> {
    let rel = hook_path(target).ok_or_else(|| unsupported(target, ExtensionKind::Hook))?;
    let path = root.join(rel);
    let source = path.display().to_string();
    let mut items = Vec::new();

    if matches!(target, CliTarget::Codex | CliTarget::GrokBuild) {
        let doc = read_toml(&path)?;
        let Some(hooks) = doc.get("hooks").and_then(|i| i.as_table_like()) else {
            return Ok(items);
        };
        for (native, item) in hooks.iter() {
            // `[hooks.state]` 之类的非事件表要跳过。
            let Some(event) = HookEvent::from_native(target, native) else {
                continue;
            };
            let Some(array) = item.as_array_of_tables() else {
                continue;
            };
            let groups: Vec<Value> = array
                .iter()
                .map(|t| toml_table_to_json(t as &dyn toml_edit::TableLike))
                .collect();
            items.extend(hook_items_from_groups(target, event, &groups, &source));
        }
        return Ok(items);
    }

    let doc = read_json(&path)?;
    let Some(hooks) = doc.get("hooks").and_then(Value::as_object) else {
        return Ok(items);
    };
    for (native, value) in hooks {
        let Some(event) = HookEvent::from_native(target, native) else {
            continue;
        };
        let Some(groups) = value.as_array() else {
            continue;
        };
        items.extend(hook_items_from_groups(target, event, groups, &source));
    }
    Ok(items)
}

fn hook_entry_json(spec: &ExtensionSpec) -> Value {
    let mut entry = Map::new();
    entry.insert("type".into(), Value::String("command".into()));
    entry.insert(
        "command".into(),
        Value::String(spec.hook_command.clone().unwrap_or_default()),
    );
    if let Some(t) = spec.timeout {
        entry.insert("timeout".into(), Value::from(t));
    }
    Value::Object(entry)
}

fn write_hook(
    root: &ConfigRoot,
    target: CliTarget,
    spec: &ExtensionSpec,
    stamp: &str,
    backups: &BackupStore,
) -> Result<Vec<String>, AppError> {
    let event = spec.event.expect("validate 已确认 event 存在");
    let native = event.native_name(target).ok_or_else(|| {
        AppError::Config(format!(
            "{} 没有 {} 这个 hook 事件，无法写入",
            target.label(),
            event.key()
        ))
    })?;
    let rel = hook_path(target).ok_or_else(|| unsupported(target, ExtensionKind::Hook))?;
    let path = root.join(rel);
    snapshot_before_write(root, target, &path, stamp, "extension-hook", backups)?;

    let matcher = spec.matcher.clone().unwrap_or_else(|| "*".into());
    let command = spec.hook_command.clone().unwrap_or_default();

    if matches!(target, CliTarget::Codex | CliTarget::GrokBuild) {
        let mut doc = read_toml(&path)?;
        // toml_edit 不会自动把缺失的 hooks 建成表，先补上。
        if doc.get("hooks").and_then(|i| i.as_table()).is_none() {
            doc["hooks"] = toml_edit::Item::Table(toml_edit::Table::new());
        }
        let hooks = doc["hooks"]
            .as_table_mut()
            .ok_or_else(|| AppError::Config(format!("{} 的 hooks 不是表", path.display())))?;
        if hooks.get(native).and_then(|i| i.as_array_of_tables()).is_none() {
            hooks[native] = toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
        }
        let groups = hooks[native]
            .as_array_of_tables_mut()
            .ok_or_else(|| AppError::Config(format!("{native} 不是 hook 数组")))?;

        let mut handler = toml_edit::Table::new();
        handler["type"] = toml_edit::value("command");
        handler["command"] = toml_edit::value(command.clone());
        if let Some(t) = spec.timeout {
            handler["timeout"] = toml_edit::value(t as i64);
        }

        // 合并进已有的同 matcher 分组，而不是再开一组——否则同一个 matcher
        // 会在文件里散成好几段，用户手工维护时很难读。
        let existing = groups.iter_mut().find(|g| {
            g.get("matcher").and_then(|m| m.as_str()).unwrap_or("*") == matcher
        });
        let group = match existing {
            Some(g) => g,
            None => {
                let mut g = toml_edit::Table::new();
                g["matcher"] = toml_edit::value(matcher.clone());
                g["hooks"] = toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
                groups.push(g);
                groups.iter_mut().last().expect("刚 push 过")
            }
        };
        if group.get("hooks").and_then(|i| i.as_array_of_tables()).is_none() {
            group["hooks"] = toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new());
        }
        let handlers = group["hooks"]
            .as_array_of_tables_mut()
            .ok_or_else(|| AppError::Config("hooks 不是数组".into()))?;
        // 同一条命令重复安装是幂等的：替换而不是追加。
        let dup = handlers
            .iter()
            .position(|h| h.get("command").and_then(|c| c.as_str()) == Some(command.as_str()));
        match dup {
            Some(i) => {
                handlers.remove(i);
                handlers.push(handler);
            }
            None => handlers.push(handler),
        }

        write_atomic(&path, &doc.to_string())?;
        return Ok(vec![path.display().to_string()]);
    }

    let mut doc = read_json(&path)?;
    let hooks = object_at(&mut doc, "hooks")?;
    let groups = hooks
        .entry(native.to_string())
        .or_insert_with(|| Value::Array(Vec::new()));
    if !groups.is_array() {
        return Err(AppError::Config(format!(
            "{} 里的 hooks.{native} 不是数组，拒绝覆盖",
            path.display()
        )));
    }
    let groups = groups.as_array_mut().expect("刚检查过");
    let entry = hook_entry_json(spec);
    match groups
        .iter_mut()
        .find(|g| g.get("matcher").and_then(Value::as_str).unwrap_or("*") == matcher)
    {
        Some(group) => {
            // 注意不能用 object_at(group, "hooks")：那个 helper 见到非对象就会
            // 整个替换掉，而这里的 hooks 正是一个数组——会把用户已有的 hook
            // 全部抹掉。
            match group.get_mut("hooks").and_then(Value::as_array_mut) {
                Some(arr) => {
                    match arr.iter().position(|h| {
                        h.get("command").and_then(Value::as_str) == Some(command.as_str())
                    }) {
                        Some(i) => arr[i] = entry,
                        None => arr.push(entry),
                    }
                }
                None => {
                    group["hooks"] = Value::Array(vec![entry]);
                }
            }
        }
        None => groups.push(json!({ "matcher": matcher, "hooks": [entry] })),
    }
    write_pretty_json(&path, &doc)?;
    Ok(vec![path.display().to_string()])
}

fn remove_hook(
    root: &ConfigRoot,
    target: CliTarget,
    id: &str,
    stamp: &str,
    backups: &BackupStore,
) -> Result<Vec<String>, AppError> {
    let rel = hook_path(target).ok_or_else(|| unsupported(target, ExtensionKind::Hook))?;
    let path = root.join(rel);
    // 先确认真有这一条，免得为一次空操作留下快照。
    if !list_hooks(root, target)?.iter().any(|i| i.id == id) {
        return Err(not_found(target, ExtensionKind::Hook, id));
    }
    snapshot_before_write(root, target, &path, stamp, "extension-hook", backups)?;

    if matches!(target, CliTarget::Codex | CliTarget::GrokBuild) {
        let mut doc = read_toml(&path)?;
        let Some(hooks) = doc.get_mut("hooks").and_then(|i| i.as_table_mut()) else {
            return Err(not_found(target, ExtensionKind::Hook, id));
        };
        let natives: Vec<String> = hooks.iter().map(|(k, _)| k.to_string()).collect();
        for native in natives {
            let Some(event) = HookEvent::from_native(target, &native) else {
                continue;
            };
            let Some(groups) = hooks[&native].as_array_of_tables_mut() else {
                continue;
            };
            for group in groups.iter_mut() {
                let matcher = group
                    .get("matcher")
                    .and_then(|m| m.as_str())
                    .unwrap_or("*")
                    .to_string();
                if let Some(handlers) = group["hooks"].as_array_of_tables_mut() {
                    handlers.retain(|h| {
                        h.get("command")
                            .and_then(|c| c.as_str())
                            .map(|c| hook_id(event, &matcher, c) != id)
                            .unwrap_or(true)
                    });
                }
            }
            // 清掉被掏空的分组和事件表，避免配置里堆一堆空壳。
            groups.retain(|g| {
                g.get("hooks")
                    .and_then(|h| h.as_array_of_tables())
                    .map(|h| !h.is_empty())
                    .unwrap_or(false)
            });
            if groups.is_empty() {
                hooks.remove(&native);
            }
        }
        write_atomic(&path, &doc.to_string())?;
        return Ok(vec![path.display().to_string()]);
    }

    let mut doc = read_json(&path)?;
    if let Some(hooks) = doc.get_mut("hooks").and_then(Value::as_object_mut) {
        let natives: Vec<String> = hooks.keys().cloned().collect();
        for native in natives {
            let Some(event) = HookEvent::from_native(target, &native) else {
                continue;
            };
            let Some(groups) = hooks.get_mut(&native).and_then(Value::as_array_mut) else {
                continue;
            };
            for group in groups.iter_mut() {
                let matcher = group
                    .get("matcher")
                    .and_then(Value::as_str)
                    .unwrap_or("*")
                    .to_string();
                if let Some(arr) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                    arr.retain(|h| {
                        h.get("command")
                            .and_then(Value::as_str)
                            .map(|c| hook_id(event, &matcher, c) != id)
                            .unwrap_or(true)
                    });
                }
            }
            groups.retain(|g| {
                g.get("hooks")
                    .and_then(Value::as_array)
                    .map(|a| !a.is_empty())
                    .unwrap_or(false)
            });
            if groups.is_empty() {
                hooks.remove(&native);
            }
        }
    }
    write_pretty_json(&path, &doc)?;
    Ok(vec![path.display().to_string()])
}

fn read_hook_spec(
    root: &ConfigRoot,
    target: CliTarget,
    id: &str,
) -> Result<ExtensionSpec, AppError> {
    let item = list_hooks(root, target)?
        .into_iter()
        .find(|i| i.id == id)
        .ok_or_else(|| not_found(target, ExtensionKind::Hook, id))?;
    let event = item
        .detail
        .get("event")
        .and_then(Value::as_str)
        .and_then(|k| HookEvent::all().into_iter().find(|e| e.key() == k))
        .ok_or_else(|| AppError::Config(format!("hook {id} 的事件无法识别")))?;
    Ok(ExtensionSpec {
        id: id.to_string(),
        event: Some(event),
        matcher: item
            .detail
            .get("matcher")
            .and_then(Value::as_str)
            .map(str::to_string),
        hook_command: item
            .detail
            .get("command")
            .and_then(Value::as_str)
            .map(str::to_string),
        timeout: item.detail.get("timeout").and_then(Value::as_u64),
        ..Default::default()
    })
}

// ---------------------------------------------------------------------------
// 小工具
// ---------------------------------------------------------------------------

fn not_found(target: CliTarget, kind: ExtensionKind, id: &str) -> AppError {
    AppError::Config(format!(
        "{} 里没有名为 {id} 的{}",
        target.label(),
        kind.label()
    ))
}

fn read_toml(path: &Path) -> Result<toml_edit::DocumentMut, AppError> {
    let raw = if path.exists() {
        std::fs::read_to_string(path)?
    } else {
        String::new()
    };
    raw.parse::<toml_edit::DocumentMut>()
        .map_err(|e| AppError::Config(format!("{}: {e}", path.display())))
}

fn toml_array(items: &[String]) -> toml_edit::Array {
    let mut arr = toml_edit::Array::new();
    for item in items {
        arr.push(item.as_str());
    }
    arr
}

fn toml_inline(map: &BTreeMap<String, String>) -> toml_edit::InlineTable {
    let mut t = toml_edit::InlineTable::new();
    for (k, v) in map {
        t.insert(k, toml_edit::Value::from(v.as_str()));
    }
    t
}

/// TOML 片段 → JSON，好让 list/read_spec 对 JSON 与 TOML 两家走同一段逻辑。
fn toml_table_to_json(table: &dyn toml_edit::TableLike) -> Value {
    let mut map = Map::new();
    for (k, v) in table.iter() {
        map.insert(k.to_string(), toml_item_to_json(v));
    }
    Value::Object(map)
}

fn toml_item_to_json(item: &toml_edit::Item) -> Value {
    if let Some(table) = item.as_table_like() {
        return toml_table_to_json(table);
    }
    if let Some(array) = item.as_array() {
        return Value::Array(array.iter().map(toml_value_to_json).collect());
    }
    if let Some(aot) = item.as_array_of_tables() {
        return Value::Array(
            aot.iter()
                .map(|t| toml_table_to_json(t as &dyn toml_edit::TableLike))
                .collect(),
        );
    }
    item.as_value().map(toml_value_to_json).unwrap_or(Value::Null)
}

fn toml_value_to_json(v: &toml_edit::Value) -> Value {
    match v {
        toml_edit::Value::String(s) => Value::String(s.value().clone()),
        toml_edit::Value::Integer(i) => Value::from(*i.value()),
        toml_edit::Value::Float(f) => serde_json::Number::from_f64(*f.value())
            .map(Value::Number)
            .unwrap_or(Value::Null),
        toml_edit::Value::Boolean(b) => Value::Bool(*b.value()),
        toml_edit::Value::Datetime(d) => Value::String(d.value().to_string()),
        toml_edit::Value::Array(a) => Value::Array(a.iter().map(toml_value_to_json).collect()),
        toml_edit::Value::InlineTable(t) => {
            let mut map = Map::new();
            for (k, v) in t.iter() {
                map.insert(k.to_string(), toml_value_to_json(v));
            }
            Value::Object(map)
        }
    }
}

fn string_map_to_json(map: &BTreeMap<String, String>) -> Value {
    Value::Object(
        map.iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
            .collect(),
    )
}

fn json_to_string_map(map: &Map<String, Value>) -> BTreeMap<String, String> {
    map.iter()
        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
        .collect()
}

// ---------------------------------------------------------------------------
// 测试：全部跑在 tempdir 沙箱里，绝不碰真实的 ~ 目录。
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sandbox() -> (tempfile::TempDir, ConfigRoot, BackupStore) {
        let dir = tempfile::tempdir().unwrap();
        let root = ConfigRoot::sandbox(dir.path().to_path_buf());
        let bk = BackupStore::new(dir.path().join(".ccload-test-backups"));
        (dir, root, bk)
    }

    fn write(root: &ConfigRoot, rel: &str, body: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    fn read(root: &ConfigRoot, rel: &str) -> String {
        std::fs::read_to_string(root.join(rel)).unwrap()
    }

    fn stdio_spec(id: &str) -> ExtensionSpec {
        ExtensionSpec {
            id: id.into(),
            description: Some("测试用 MCP".into()),
            transport: Some(McpTransport::Stdio),
            command: Some("npx".into()),
            args: vec!["-y".into(), "@demo/mcp".into()],
            env: BTreeMap::from([("DEMO_KEY".to_string(), "v1".to_string())]),
            ..Default::default()
        }
    }

    // ---- 合并语义：绝不能吃掉用户已有的配置 ----

    #[test]
    fn claude_mcp_install_keeps_other_servers_and_unrelated_keys() {
        let (_keep, root, bk) = sandbox();
        write(
            &root,
            ".claude.json",
            r#"{"numStartups":42,"mcpServers":{"zread":{"type":"http","url":"https://z"}}}"#,
        );
        install(
            &root,
            CliTarget::ClaudeCode,
            ExtensionKind::Mcp,
            &stdio_spec("demo"),
            "s1",
            &bk,
        )
        .unwrap();

        let doc: Value = serde_json::from_str(&read(&root, ".claude.json")).unwrap();
        assert_eq!(doc["numStartups"], 42, "无关的顶层字段必须原样保留");
        assert_eq!(doc["mcpServers"]["zread"]["url"], "https://z");
        assert_eq!(doc["mcpServers"]["demo"]["command"], "npx");
        assert_eq!(doc["mcpServers"]["demo"]["env"]["DEMO_KEY"], "v1");
    }

    #[test]
    fn codex_mcp_install_keeps_existing_toml_sections() {
        let (_keep, root, bk) = sandbox();
        write(
            &root,
            ".codex/config.toml",
            "model = \"gpt-5.6\"\n\n[mcp_servers.zread]\ntype = \"http\"\nurl = \"https://z\"\n\n[plugins.\"pdf\"]\nenabled = true\n",
        );
        install(
            &root,
            CliTarget::Codex,
            ExtensionKind::Mcp,
            &stdio_spec("demo"),
            "s1",
            &bk,
        )
        .unwrap();

        let raw = read(&root, ".codex/config.toml");
        assert!(raw.contains("model = \"gpt-5.6\""), "{raw}");
        assert!(raw.contains("[mcp_servers.zread]"), "{raw}");
        assert!(raw.contains("[plugins.\"pdf\"]"), "{raw}");
        assert!(raw.contains("[mcp_servers.demo]"), "{raw}");
    }

    #[test]
    fn claude_hook_install_merges_into_existing_event_and_matcher() {
        let (_keep, root, bk) = sandbox();
        write(
            &root,
            ".claude/settings.json",
            r#"{"env":{"X":"1"},
                "hooks":{"PreToolUse":[{"matcher":"Bash",
                  "hooks":[{"type":"command","command":"python3 guard.py"}]}]}}"#,
        );
        install(
            &root,
            CliTarget::ClaudeCode,
            ExtensionKind::Hook,
            &ExtensionSpec {
                id: "ignored".into(),
                event: Some(HookEvent::PreToolUse),
                matcher: Some("Bash".into()),
                hook_command: Some("python3 audit.py".into()),
                timeout: Some(5),
                ..Default::default()
            },
            "s1",
            &bk,
        )
        .unwrap();

        let doc: Value = serde_json::from_str(&read(&root, ".claude/settings.json")).unwrap();
        assert_eq!(doc["env"]["X"], "1", "hook 写入不得动 env");
        let groups = doc["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(groups.len(), 1, "同 matcher 必须合并进同一组");
        let hooks = groups[0]["hooks"].as_array().unwrap();
        assert_eq!(hooks.len(), 2, "用户原有的 guard.py 必须还在");
        assert_eq!(hooks[0]["command"], "python3 guard.py");
        assert_eq!(hooks[1]["command"], "python3 audit.py");
        assert_eq!(hooks[1]["timeout"], 5);
    }

    // ---- 不支持的组合必须报错，不能静默成功 ----

    #[test]
    fn unsupported_combinations_error_out() {
        let (_keep, root, bk) = sandbox();

        // Codex 没有子 agent 定义目录。
        let err = install(
            &root,
            CliTarget::Codex,
            ExtensionKind::Agent,
            &ExtensionSpec {
                id: "reviewer".into(),
                body: Some("# reviewer".into()),
                ..Default::default()
            },
            "s1",
            &bk,
        )
        .unwrap_err();
        assert!(err.to_string().contains("不支持"), "{err}");

        // OpenCode 的 hook 模型无法映射。
        let err = install(
            &root,
            CliTarget::OpenCode,
            ExtensionKind::Hook,
            &ExtensionSpec {
                id: "h".into(),
                event: Some(HookEvent::PreToolUse),
                hook_command: Some("echo hi".into()),
                ..Default::default()
            },
            "s1",
            &bk,
        )
        .unwrap_err();
        assert!(err.to_string().contains("不支持"), "{err}");

        assert!(!supports(CliTarget::Codex, ExtensionKind::Agent));
        assert!(!supports(CliTarget::OpenCode, ExtensionKind::Hook));
        assert!(supports(CliTarget::GeminiCli, ExtensionKind::Agent));
    }

    #[test]
    fn hook_event_missing_on_target_errors_with_event_name() {
        let (_keep, root, bk) = sandbox();
        // Gemini 没有 UserPromptSubmit。
        let err = install(
            &root,
            CliTarget::GeminiCli,
            ExtensionKind::Hook,
            &ExtensionSpec {
                id: "h".into(),
                event: Some(HookEvent::UserPromptSubmit),
                hook_command: Some("echo hi".into()),
                ..Default::default()
            },
            "s1",
            &bk,
        )
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("UserPromptSubmit"), "{msg}");
        assert!(msg.contains("Gemini CLI"), "{msg}");
    }

    #[test]
    fn invalid_spec_and_id_are_rejected() {
        let (_keep, root, bk) = sandbox();
        // 路径穿越
        let err = install(
            &root,
            CliTarget::ClaudeCode,
            ExtensionKind::Skill,
            &ExtensionSpec {
                id: "../../evil".into(),
                body: Some("x".into()),
                ..Default::default()
            },
            "s1",
            &bk,
        )
        .unwrap_err();
        assert!(err.to_string().contains("非法"), "{err}");

        // stdio MCP 缺 command
        let err = install(
            &root,
            CliTarget::ClaudeCode,
            ExtensionKind::Mcp,
            &ExtensionSpec {
                id: "demo".into(),
                transport: Some(McpTransport::Stdio),
                ..Default::default()
            },
            "s1",
            &bk,
        )
        .unwrap_err();
        assert!(err.to_string().contains("command"), "{err}");
    }

    #[test]
    fn removing_a_missing_extension_errors() {
        let (_keep, root, bk) = sandbox();
        let err = remove(
            &root,
            CliTarget::ClaudeCode,
            ExtensionKind::Mcp,
            "nope",
            "s1",
            &bk,
        )
        .unwrap_err();
        assert!(err.to_string().contains("没有名为 nope"), "{err}");
    }

    // ---- install → list → remove 往返 ----

    #[test]
    fn mcp_roundtrip_on_every_target() {
        for target in ALL_TARGETS {
            let (_keep, root, bk) = sandbox();
            install(&root, target, ExtensionKind::Mcp, &stdio_spec("demo"), "s1", &bk).unwrap();

            let items = list(&root, target, Some(ExtensionKind::Mcp)).unwrap();
            assert_eq!(items.len(), 1, "{:?} 列举失败", target);
            assert_eq!(items[0].id, "demo");
            assert!(items[0].enabled);

            // 读回来的 spec 必须还原 command/args/env，否则 sync 会丢信息。
            let spec = read_spec(&root, target, ExtensionKind::Mcp, "demo").unwrap();
            assert_eq!(spec.command.as_deref(), Some("npx"), "{:?}", target);
            assert_eq!(spec.args, vec!["-y", "@demo/mcp"], "{:?}", target);
            assert_eq!(spec.env.get("DEMO_KEY").map(String::as_str), Some("v1"), "{:?}", target);

            remove(&root, target, ExtensionKind::Mcp, "demo", "s2", &bk).unwrap();
            assert!(
                list(&root, target, Some(ExtensionKind::Mcp)).unwrap().is_empty(),
                "{:?} 卸载后应为空",
                target
            );
        }
    }

    #[test]
    fn hook_roundtrip_on_every_supporting_target() {
        for target in ALL_TARGETS {
            if !supports(target, ExtensionKind::Hook) {
                continue;
            }
            let (_keep, root, bk) = sandbox();
            let spec = ExtensionSpec {
                id: "seed".into(),
                event: Some(HookEvent::PreToolUse),
                matcher: Some("Bash".into()),
                hook_command: Some("/opt/guard.sh".into()),
                timeout: Some(10),
                ..Default::default()
            };
            install(&root, target, ExtensionKind::Hook, &spec, "s1", &bk).unwrap();

            let items = list(&root, target, Some(ExtensionKind::Hook)).unwrap();
            assert_eq!(items.len(), 1, "{:?}", target);
            let id = items[0].id.clone();
            assert_eq!(id, hook_id(HookEvent::PreToolUse, "Bash", "/opt/guard.sh"));

            let back = read_spec(&root, target, ExtensionKind::Hook, &id).unwrap();
            assert_eq!(back.event, Some(HookEvent::PreToolUse), "{:?}", target);
            assert_eq!(back.matcher.as_deref(), Some("Bash"), "{:?}", target);
            assert_eq!(back.hook_command.as_deref(), Some("/opt/guard.sh"));
            assert_eq!(back.timeout, Some(10), "{:?}", target);

            remove(&root, target, ExtensionKind::Hook, &id, "s2", &bk).unwrap();
            assert!(
                list(&root, target, Some(ExtensionKind::Hook)).unwrap().is_empty(),
                "{:?} 卸载后应为空",
                target
            );
        }
    }

    #[test]
    fn hook_install_is_idempotent() {
        let (_keep, root, bk) = sandbox();
        let spec = ExtensionSpec {
            id: "x".into(),
            event: Some(HookEvent::PreToolUse),
            matcher: Some("Bash".into()),
            hook_command: Some("/opt/guard.sh".into()),
            ..Default::default()
        };
        install(&root, CliTarget::GrokBuild, ExtensionKind::Hook, &spec, "s1", &bk).unwrap();
        install(&root, CliTarget::GrokBuild, ExtensionKind::Hook, &spec, "s2", &bk).unwrap();
        assert_eq!(
            list(&root, CliTarget::GrokBuild, Some(ExtensionKind::Hook)).unwrap().len(),
            1,
            "重复安装同一条 hook 不应留下两份"
        );
    }

    #[test]
    fn skill_and_agent_roundtrip() {
        let (_keep, root, bk) = sandbox();
        let spec = ExtensionSpec {
            id: "commit".into(),
            description: Some("生成规范化的 git commit：注意冒号: 与\"引号\"".into()),
            body: Some("# Commit Skill\n\n步骤一…".into()),
            ..Default::default()
        };
        install(&root, CliTarget::ClaudeCode, ExtensionKind::Skill, &spec, "s1", &bk).unwrap();

        let raw = read(&root, ".claude/skills/commit/SKILL.md");
        assert!(raw.starts_with("---\n"), "{raw}");
        assert!(raw.contains("# Commit Skill"), "{raw}");

        let items = list(&root, CliTarget::ClaudeCode, Some(ExtensionKind::Skill)).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "commit");
        assert_eq!(
            items[0].description.as_deref(),
            Some("生成规范化的 git commit：注意冒号: 与\"引号\""),
            "带冒号和引号的描述必须能原样读回"
        );

        // Agent 走同一条路径，只是落成 <id>.md。
        install(
            &root,
            CliTarget::GrokBuild,
            ExtensionKind::Agent,
            &ExtensionSpec {
                id: "reviewer".into(),
                description: Some("代码审查".into()),
                body: Some("你是审查员".into()),
                ..Default::default()
            },
            "s2",
            &bk,
        )
        .unwrap();
        assert!(root.join(".grok/agents/reviewer.md").exists());

        remove(&root, CliTarget::ClaudeCode, ExtensionKind::Skill, "commit", "s3", &bk).unwrap();
        assert!(
            !root.join(".claude/skills/commit").exists(),
            "卸载后原位置应消失"
        );
        assert!(
            list(&root, CliTarget::ClaudeCode, Some(ExtensionKind::Skill)).unwrap().is_empty()
        );
    }

    /// 卸载 skill 不能是 `remove_dir_all`：附属文件要能在归档里找回来。
    #[test]
    fn removing_a_skill_archives_the_whole_directory() {
        let (_keep, root, bk) = sandbox();
        install(
            &root,
            CliTarget::ClaudeCode,
            ExtensionKind::Skill,
            &ExtensionSpec {
                id: "demo".into(),
                body: Some("# demo".into()),
                ..Default::default()
            },
            "s1",
            &bk,
        )
        .unwrap();
        std::fs::write(root.join(".claude/skills/demo/references.md"), "重要资料").unwrap();

        let written = remove(&root, CliTarget::ClaudeCode, ExtensionKind::Skill, "demo", "arch1", &bk)
            .unwrap();
        assert_eq!(written.len(), 1);
        let archived = root.join(".ccload-client/removed-extensions/arch1");
        let entry = std::fs::read_dir(&archived).unwrap().next().unwrap().unwrap();
        assert_eq!(
            std::fs::read_to_string(entry.path().join("references.md")).unwrap(),
            "重要资料"
        );
    }

    /// frontmatter 已经写好的 markdown 必须逐字保留——agent 的 model/tools
    /// 之类我们不认识的字段不能在同步过程中被抹掉。
    #[test]
    fn existing_frontmatter_is_preserved_verbatim() {
        let (_keep, root, bk) = sandbox();
        let original = "---\nname: reviewer\ndescription: 审查\nmodel: opus\ntools: [Read, Grep]\n---\n\n正文\n";
        install(
            &root,
            CliTarget::ClaudeCode,
            ExtensionKind::Agent,
            &ExtensionSpec {
                id: "reviewer".into(),
                body: Some(original.into()),
                ..Default::default()
            },
            "s1",
            &bk,
        )
        .unwrap();
        assert_eq!(read(&root, ".claude/agents/reviewer.md"), original);
    }

    // ---- sync ----

    #[test]
    fn sync_pushes_one_mcp_to_every_cli_in_its_native_format() {
        let (_keep, root, bk) = sandbox();
        // 每个目标都先放一点用户自己的配置，验证同步不会踩掉。
        write(&root, ".claude.json", r#"{"mcpServers":{"keep":{"command":"x"}}}"#);
        write(&root, ".codex/config.toml", "model = \"gpt-5.6\"\n");
        write(&root, ".gemini/settings.json", r#"{"security":{"auth":{}}}"#);
        write(&root, ".grok/config.toml", "[ui]\ntheme = \"grokday\"\n");
        write(
            &root,
            ".config/opencode/opencode.json",
            r#"{"theme":"dark","mcp":{"zread":{"type":"remote"}}}"#,
        );

        install(&root, CliTarget::ClaudeCode, ExtensionKind::Mcp, &stdio_spec("demo"), "s0", &bk)
            .unwrap();
        let outcomes = sync(
            &root,
            ExtensionKind::Mcp,
            "demo",
            &ALL_TARGETS,
            None,
            "sync1",
            &bk,
        )
        .unwrap();
        assert!(outcomes.iter().all(|o| o.ok), "{outcomes:?}");

        // Codex / Grok → TOML 表
        assert!(read(&root, ".codex/config.toml").contains("[mcp_servers.demo]"));
        assert!(read(&root, ".codex/config.toml").contains("model = \"gpt-5.6\""));
        assert!(read(&root, ".grok/config.toml").contains("[mcp_servers.demo]"));
        assert!(read(&root, ".grok/config.toml").contains("theme = \"grokday\""));

        // Gemini → mcpServers.command 字符串
        let gem: Value = serde_json::from_str(&read(&root, ".gemini/settings.json")).unwrap();
        assert_eq!(gem["mcpServers"]["demo"]["command"], "npx");
        assert!(gem["security"].is_object(), "同步不得吃掉 security 段");

        // OpenCode → command 是数组，env 叫 environment
        let oc: Value =
            serde_json::from_str(&read(&root, ".config/opencode/opencode.json")).unwrap();
        assert_eq!(oc["mcp"]["demo"]["type"], "local");
        assert_eq!(oc["mcp"]["demo"]["command"][0], "npx");
        assert_eq!(oc["mcp"]["demo"]["command"][2], "@demo/mcp");
        assert_eq!(oc["mcp"]["demo"]["environment"]["DEMO_KEY"], "v1");
        assert_eq!(oc["theme"], "dark", "同步不得吃掉 theme");
        assert_eq!(oc["mcp"]["zread"]["type"], "remote", "同步不得吃掉别的 MCP");

        // 来源本身跳过，且其他 MCP 完好。
        let claude: Value = serde_json::from_str(&read(&root, ".claude.json")).unwrap();
        assert_eq!(claude["mcpServers"]["keep"]["command"], "x");
    }

    #[test]
    fn sync_reports_per_target_failure_without_aborting() {
        let (_keep, root, bk) = sandbox();
        install(
            &root,
            CliTarget::ClaudeCode,
            ExtensionKind::Agent,
            &ExtensionSpec {
                id: "reviewer".into(),
                description: Some("审查".into()),
                body: Some("你是审查员".into()),
                ..Default::default()
            },
            "s0",
            &bk,
        )
        .unwrap();

        let outcomes = sync(
            &root,
            ExtensionKind::Agent,
            "reviewer",
            &[CliTarget::Codex, CliTarget::GeminiCli],
            None,
            "sync1",
            &bk,
        )
        .unwrap();

        let codex = outcomes.iter().find(|o| o.target == CliTarget::Codex).unwrap();
        assert!(!codex.ok);
        assert!(codex.error.as_deref().unwrap().contains("不支持"), "{codex:?}");

        let gemini = outcomes.iter().find(|o| o.target == CliTarget::GeminiCli).unwrap();
        assert!(gemini.ok, "{gemini:?}");
        assert!(root.join(".gemini/agents/reviewer.md").exists());
    }

    #[test]
    fn sync_without_a_source_anywhere_errors() {
        let (_keep, root, bk) = sandbox();
        let err = sync(
            &root,
            ExtensionKind::Mcp,
            "ghost",
            &[CliTarget::Codex],
            None,
            "s1",
            &bk,
        )
        .unwrap_err();
        assert!(err.to_string().contains("没有任何 CLI"), "{err}");
    }

    #[test]
    fn http_mcp_syncs_url_and_headers() {
        let (_keep, root, bk) = sandbox();
        let spec = ExtensionSpec {
            id: "zread".into(),
            transport: Some(McpTransport::Http),
            url: Some("https://api.example.com/mcp".into()),
            headers: BTreeMap::from([("Authorization".to_string(), "Bearer t".to_string())]),
            ..Default::default()
        };
        install(&root, CliTarget::GeminiCli, ExtensionKind::Mcp, &spec, "s1", &bk).unwrap();
        let outcomes = sync(
            &root,
            ExtensionKind::Mcp,
            "zread",
            &[CliTarget::Codex, CliTarget::OpenCode],
            Some(CliTarget::GeminiCli),
            "s2",
            &bk,
        )
        .unwrap();
        assert!(outcomes.iter().all(|o| o.ok), "{outcomes:?}");

        assert!(read(&root, ".codex/config.toml").contains("url = \"https://api.example.com/mcp\""));
        let oc: Value =
            serde_json::from_str(&read(&root, ".config/opencode/opencode.json")).unwrap();
        assert_eq!(oc["mcp"]["zread"]["type"], "remote");
        assert_eq!(oc["mcp"]["zread"]["headers"]["Authorization"], "Bearer t");
    }

    // ---- 备份 ----

    #[test]
    fn install_snapshots_tracked_files_before_writing() {
        let (_keep, root, bk) = sandbox();
        let original = "model = \"gpt-5.6\"\n";
        write(&root, ".codex/config.toml", original);
        install(&root, CliTarget::Codex, ExtensionKind::Mcp, &stdio_spec("demo"), "snap1", &bk)
            .unwrap();

        let entries = bk.list(Some(CliTarget::Codex)).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].reason, "extension-mcp");
        // 快照可以把文件原样还原回去。
        bk.restore(&root, &entries[0].id).unwrap();
        assert_eq!(read(&root, ".codex/config.toml"), original);
    }

    /// Codex 读的是 `http_headers`；写成 `headers` 等于一个请求头都不发。
    /// 用户机器上的 `[mcp_servers.zread.http_headers]` 就是这个格式。
    /// `~/.claude.json` 不在 relative_paths 里，但 UI 承诺「写入前会自动快照，
    /// 可在 CLI 接管页回滚」—— 那它就必须真的能被列出来、被还原。
    #[test]
    fn claude_json_snapshots_are_listable_and_restorable() {
        let dir = tempfile::tempdir().unwrap();
        let root = ConfigRoot::sandbox(dir.path().to_path_buf());
        let bk = BackupStore::new(dir.path().join("bk"));
        let path = root.join(".claude.json");
        std::fs::write(&path, r#"{"mcpServers":{},"oauthAccount":"原样"}"#).unwrap();

        let spec = ExtensionSpec {
            id: "srv".into(),
            transport: Some(McpTransport::Stdio),
            command: Some("node".into()),
            ..Default::default()
        };
        install(&root, CliTarget::ClaudeCode, ExtensionKind::Mcp, &spec, "s1", &bk).unwrap();
        assert!(std::fs::read_to_string(&path).unwrap().contains("srv"));

        let entries = bk.list(Some(CliTarget::ClaudeCode)).unwrap();
        let entry = entries
            .iter()
            .find(|e| e.files.iter().any(|f| f.rel == ".claude.json"))
            .expect("快照要出现在清单里");

        bk.restore(&root, &entry.id).unwrap();
        let back = std::fs::read_to_string(&path).unwrap();
        assert!(!back.contains("srv"), "回滚后不该还有我们写的条目：{back}");
        assert!(back.contains("原样"));
    }

    /// 同一条命令挂在两个 matcher 下是两条独立的 hook：id 必须不同，删一条也
    /// 只能删掉那一条。
    #[test]
    fn hooks_with_the_same_command_under_different_matchers_are_distinct() {
        let dir = tempfile::tempdir().unwrap();
        let root = ConfigRoot::sandbox(dir.path().to_path_buf());
        let bk = BackupStore::new(dir.path().join("bk"));
        // hook 的 id 由「事件+matcher+命令」推导，spec.id 只是占位。
        let mk = |matcher: &str| ExtensionSpec {
            id: "placeholder".into(),
            event: Some(HookEvent::PreToolUse),
            matcher: Some(matcher.into()),
            hook_command: Some("~/guard.sh".into()),
            ..Default::default()
        };
        install(&root, CliTarget::ClaudeCode, ExtensionKind::Hook, &mk("Bash"), "s1", &bk).unwrap();
        install(&root, CliTarget::ClaudeCode, ExtensionKind::Hook, &mk("Write"), "s2", &bk).unwrap();

        let items = list(&root, CliTarget::ClaudeCode, Some(ExtensionKind::Hook)).unwrap();
        assert_eq!(items.len(), 2, "两个 matcher 两条 hook");
        assert_ne!(items[0].id, items[1].id, "id 不能撞");

        remove(&root, CliTarget::ClaudeCode, ExtensionKind::Hook, &items[0].id, "s3", &bk).unwrap();
        let left = list(&root, CliTarget::ClaudeCode, Some(ExtensionKind::Hook)).unwrap();
        assert_eq!(left.len(), 1, "只该删掉一条");
        assert_eq!(left[0].id, items[1].id);
    }

    /// skill 常常带 scripts/ references/ —— 编辑 SKILL.md 不能把它们搬走。
    #[test]
    fn saving_a_skill_keeps_its_auxiliary_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = ConfigRoot::sandbox(dir.path().to_path_buf());
        let bk = BackupStore::new(dir.path().join("bk"));
        let skill = root.join(".claude/skills/analyzer");
        std::fs::create_dir_all(skill.join("references")).unwrap();
        std::fs::write(skill.join("SKILL.md"), "---\nname: analyzer\n---\n旧正文\n").unwrap();
        std::fs::write(skill.join("run.py"), "print(1)").unwrap();
        std::fs::write(skill.join("references/notes.md"), "note").unwrap();

        let spec = ExtensionSpec {
            id: "analyzer".into(),
            body: Some("---\nname: analyzer\n---\n新正文\n".into()),
            ..Default::default()
        };
        install(&root, CliTarget::ClaudeCode, ExtensionKind::Skill, &spec, "s1", &bk).unwrap();

        assert!(skill.join("run.py").exists(), "脚本被搬走了");
        assert!(skill.join("references/notes.md").exists(), "附属目录被搬走了");
        let md = std::fs::read_to_string(skill.join("SKILL.md")).unwrap();
        assert!(md.contains("新正文"), "{md}");
    }

    /// 改描述以前是空操作：读回的 body 自带 frontmatter，写入时直接短路返回。
    #[test]
    fn editing_the_description_actually_lands() {
        let dir = tempfile::tempdir().unwrap();
        let root = ConfigRoot::sandbox(dir.path().to_path_buf());
        let bk = BackupStore::new(dir.path().join("bk"));
        let spec = ExtensionSpec {
            id: "analyzer".into(),
            description: Some("新的加载触发条件".into()),
            body: Some("---\nname: analyzer\ndescription: 旧描述\nlicense: MIT\n---\n正文\n".into()),
            ..Default::default()
        };
        install(&root, CliTarget::ClaudeCode, ExtensionKind::Skill, &spec, "s1", &bk).unwrap();

        let md = std::fs::read_to_string(root.join(".claude/skills/analyzer/SKILL.md")).unwrap();
        assert!(md.contains("新的加载触发条件"), "{md}");
        assert!(!md.contains("旧描述"), "{md}");
        // 其余 frontmatter 行逐字保留，不重排、不丢。
        assert!(md.contains("license: MIT"), "{md}");
        assert!(md.contains("name: analyzer"), "{md}");
        assert!(md.contains("正文"), "{md}");
    }

    #[test]
    fn codex_http_mcp_uses_http_headers() {
        let dir = tempfile::tempdir().unwrap();
        let root = ConfigRoot::sandbox(dir.path().to_path_buf());
        let bk = BackupStore::new(dir.path().join("bk"));
        let spec = ExtensionSpec {
            id: "zread".into(),
            transport: Some(McpTransport::Http),
            url: Some("https://example.com/mcp".into()),
            headers: BTreeMap::from([("Authorization".into(), "Bearer x".into())]),
            ..Default::default()
        };
        install(&root, CliTarget::Codex, ExtensionKind::Mcp, &spec, "s1", &bk).unwrap();
        let raw = std::fs::read_to_string(root.join(".codex/config.toml")).unwrap();
        assert!(raw.contains("http_headers"), "{raw}");
        assert!(
            !raw.contains("\nheaders") && !raw.contains(" headers"),
            "裸 headers 不该出现：{raw}"
        );
    }

    /// 编辑一个条目时，ExtensionSpec 没建模的字段必须原样留着。
    #[test]
    fn editing_a_toml_mcp_keeps_unmodelled_fields() {
        let dir = tempfile::tempdir().unwrap();
        let root = ConfigRoot::sandbox(dir.path().to_path_buf());
        let bk = BackupStore::new(dir.path().join("bk"));
        let path = root.join(".codex/config.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "[mcp_servers.node_repl]\ncommand = \"node\"\nstartup_timeout_sec = 120\ncwd = \".\"\n",
        )
        .unwrap();

        let spec = ExtensionSpec {
            id: "node_repl".into(),
            transport: Some(McpTransport::Stdio),
            command: Some("node".into()),
            args: vec!["--experimental".into()],
            ..Default::default()
        };
        install(&root, CliTarget::Codex, ExtensionKind::Mcp, &spec, "s1", &bk).unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(raw.contains("startup_timeout_sec = 120"), "{raw}");
        assert!(raw.contains("cwd = \".\""), "{raw}");
        assert!(raw.contains("--experimental"), "{raw}");
    }

    /// JSON 侧同理，并且 `sse` 不该被我们改写成 `http`。
    #[test]
    fn editing_a_json_mcp_keeps_type_and_unmodelled_fields() {
        let dir = tempfile::tempdir().unwrap();
        let root = ConfigRoot::sandbox(dir.path().to_path_buf());
        let bk = BackupStore::new(dir.path().join("bk"));
        let path = root.join(".claude.json");
        std::fs::write(
            &path,
            r#"{"mcpServers":{"remote":{"type":"sse","url":"https://a/","scope":"user"}}}"#,
        )
        .unwrap();

        let spec = ExtensionSpec {
            id: "remote".into(),
            transport: Some(McpTransport::Http),
            url: Some("https://a/".into()),
            headers: BTreeMap::from([("X-Key".into(), "v".into())]),
            ..Default::default()
        };
        install(&root, CliTarget::ClaudeCode, ExtensionKind::Mcp, &spec, "s1", &bk).unwrap();

        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(doc.pointer("/mcpServers/remote/type").unwrap(), "sse");
        assert_eq!(doc.pointer("/mcpServers/remote/scope").unwrap(), "user");
        assert_eq!(doc.pointer("/mcpServers/remote/headers/X-Key").unwrap(), "v");
    }

    #[test]
    fn support_matrix_covers_every_target_and_kind() {
        let rows = support_matrix();
        assert_eq!(rows.len(), 20);
        assert!(rows
            .iter()
            .all(|r| r.supported == r.path.is_some()));
        // MCP 是唯一 5 家全支持的类型。
        assert_eq!(
            rows.iter()
                .filter(|r| r.kind == ExtensionKind::Mcp && r.supported)
                .count(),
            5
        );
    }
}
