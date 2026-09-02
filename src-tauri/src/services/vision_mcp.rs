//! Vision augmentation for non-multimodal models.
//!
//! The kernel routes text fine, but a model like deepseek-r1 cannot read an
//! image the user pastes into the CLI. Instead of forcing a multimodal model
//! everywhere, this ships a tiny stdio MCP server *inside the client binary*
//! (`ccload-client vision-mcp`) exposing vision tools: they read the image,
//! send it to a vision-capable model through the kernel, and return text.
//! Pasted images often show up in the transcript as `[Image 1]` with no path;
//! the tools resolve that to the file the CLI already wrote into the session
//! directory, so the host model does not have to ask the user to save a copy.
//!
//! Each CLI gets the server registered in its own MCP format. The per-CLI
//! shapes (JSON vs TOML, `mcpServers` vs `mcp` vs `mcp_servers`, OpenCode's
//! array-form `command`) all live in `cli_extensions`, which already writes
//! arbitrary MCP entries for all five CLIs — this module just describes the
//! one entry it wants and hands it over. Writing a second copy here is how
//! Gemini CLI and Grok Build ended up unsupported while the extension page
//! happily wrote MCP servers into both.
//!
//! The base URL / token / vision model travel as env vars, so the server never
//! reads the client's settings store (it runs as a separate process spawned by
//! the CLI, possibly long after the client UI closed).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::{json, Value};

use crate::error::AppError;
use crate::services::cli_backup::BackupStore;
use crate::services::cli_extensions::{
    self, ExtensionKind, ExtensionSpec, McpTransport,
};
use crate::services::cli_types::{CliTarget, ConfigRoot};
use crate::services::mcp_usage;

pub const MCP_NAME: &str = "ccload-vision";

const ENV_BASE_URL: &str = "CCLOAD_VISION_BASE_URL";
const ENV_TOKEN: &str = "CCLOAD_VISION_TOKEN";
const ENV_MODEL: &str = "CCLOAD_VISION_MODEL";

pub struct VisionConfig {
    pub base_url: String,
    pub token: String,
    pub model: String,
}

/// 一个 CLI 上视觉 MCP 的当前状态。
///
/// 存在的理由：模型选择以前只活在渲染进程的 `useState` 里，装完再进这一页就
/// 回到「选择多模态模型」占位符，用户以为没保存上。真相一直写在各 CLI 的配置
/// 文件里（`CCLOAD_VISION_MODEL`），读回来就是了 —— 和「装没装」用同一条原则：
/// 状态从磁盘读，不靠按钮的记忆。
#[derive(Debug, serde::Serialize)]
pub struct VisionTargetState {
    pub target: CliTarget,
    pub label: &'static str,
    pub installed: bool,
    /// 已装的话，它现在用哪个模型看图。
    pub model: Option<String>,
    /// 装了，但里面存的 base_url / token 已经不是当前内核的了 —— 配置看着
    /// 是好的，`describe_image` 每次都会 401 或连不上。和 CLI 接管页的
    /// `token_stale` 是同一类问题，同样要在界面上说出来。
    pub stale: bool,
}

/// 读回一个 CLI 里已装的视觉 MCP 配置；没装返回 `None`。
///
/// 走 `read_spec` 而不是自己解析：各 CLI 的环境变量键名不统一（OpenCode 是
/// `environment`，其余是 `env`），那套归一化 `cli_extensions` 已经做过一遍。
pub fn read_vision_mcp(
    root: &ConfigRoot,
    target: CliTarget,
) -> Result<Option<VisionConfig>, AppError> {
    let installed = cli_extensions::list(root, target, Some(ExtensionKind::Mcp))?
        .iter()
        .any(|i| i.id == MCP_NAME);
    if !installed {
        return Ok(None);
    }
    let spec = cli_extensions::read_spec(root, target, ExtensionKind::Mcp, MCP_NAME)?;
    let get = |k: &str| spec.env.get(k).cloned().unwrap_or_default();
    Ok(Some(VisionConfig {
        base_url: get(ENV_BASE_URL),
        token: get(ENV_TOKEN),
        model: get(ENV_MODEL),
    }))
}

/// 五个 CLI 各自的视觉 MCP 状态。
///
/// 一个目标读不动（配置文件不存在、被手工编辑成非法 JSON）算「没装」而不是
/// 整次失败：这一格只是用来点亮界面的，不该因为某家 CLI 的配置坏了就让另外
/// 四家的状态也消失。
pub fn vision_states(
    root: &ConfigRoot,
    targets: &[CliTarget],
    kernel_base_url: &str,
    kernel_token: Option<&str>,
) -> Vec<VisionTargetState> {
    targets
        .iter()
        .copied()
        .map(|target| match read_vision_mcp(root, target) {
            Ok(Some(cfg)) => {
                let base_ok = same_endpoint(&cfg.base_url, kernel_base_url);
                let token_ok = kernel_token.is_none_or(|want| cfg.token == want);
                VisionTargetState {
                    target,
                    label: target.label(),
                    installed: true,
                    model: Some(cfg.model).filter(|m| !m.is_empty()),
                    stale: !(base_ok && token_ok),
                }
            }
            _ => VisionTargetState {
                target,
                label: target.label(),
                installed: false,
                model: None,
                stale: false,
            },
        })
        .collect()
}

/// 只比较到「去掉尾斜杠」这一层。写进去的就是我们自己拼的 base_url，不需要
/// CLI 接管那边那套 host/port 归一。
pub(crate) fn same_endpoint(a: &str, b: &str) -> bool {
    a.trim().trim_end_matches('/') == b.trim().trim_end_matches('/')
}

/// Register (or unregister) the vision MCP server for one CLI.
/// Returns the files written.
pub fn set_vision_mcp(
    root: &ConfigRoot,
    target: CliTarget,
    enabled: bool,
    cfg: &VisionConfig,
    stamp: &str,
    backups: &BackupStore,
) -> Result<Vec<String>, AppError> {
    if enabled {
        let exe = std::env::current_exe()
            .map_err(|e| AppError::Config(format!("cannot resolve own executable: {e}")))?
            .display()
            .to_string();
        let spec = ExtensionSpec {
            id: MCP_NAME.into(),
            description: Some("ccLoad 视觉辅助：把图片交给多模态模型描述".into()),
            transport: Some(McpTransport::Stdio),
            command: Some(exe),
            args: vec!["vision-mcp".into()],
            env: vision_env(cfg),
            ..Default::default()
        };
        return cli_extensions::install(root, target, ExtensionKind::Mcp, &spec, stamp, backups);
    }

    // 没装过就当已经达成目标。`remove` 找不到条目会报错，但用户点「移除」要的是
    // 「最终它不在」，不是「刚才确实删掉了一条」。
    let installed = cli_extensions::list(root, target, Some(ExtensionKind::Mcp))?
        .iter()
        .any(|i| i.id == MCP_NAME);
    if !installed {
        return Ok(Vec::new());
    }
    cli_extensions::remove(root, target, ExtensionKind::Mcp, MCP_NAME, stamp, backups)
}

fn vision_env(cfg: &VisionConfig) -> BTreeMap<String, String> {
    BTreeMap::from([
        (ENV_BASE_URL.to_string(), cfg.base_url.clone()),
        (ENV_TOKEN.to_string(), cfg.token.clone()),
        (ENV_MODEL.to_string(), cfg.model.clone()),
    ])
}

// ---------------------------------------------------------------------------
// stdio MCP server (`ccload-client vision-mcp`)
// ---------------------------------------------------------------------------

/// Serve MCP over stdin/stdout until EOF. Exposed as a CLI subcommand so the
/// host CLI spawns this very binary — nothing extra to install or bundle.
pub fn serve_stdio() -> i32 {
    use std::io::BufRead;

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("vision-mcp: no runtime: {e}");
            return 1;
        }
    };

    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue, // not JSON-RPC; ignore like MCP servers do
        };
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(Value::Null);

        // Notifications (no id) get no response per JSON-RPC.
        let Some(id) = id else {
            continue;
        };

        let result = match method {
            "initialize" => Ok(json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": MCP_NAME, "version": "0.1.0" },
            })),
            "tools/list" => Ok(json!({ "tools": tool_specs() })),
            "tools/call" => {
                // MCP nests the tool payload under `arguments`.
                let args = params.get("arguments").unwrap_or(&params);
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    // 旧版只有一个工具，`name` 当时可以不看。装过旧版的配置
                    // 还在磁盘上，缺 name 时按老行为兜底而不是报错。
                    .unwrap_or("describe_image")
                    .to_string();
                let started = std::time::Instant::now();
                let out = runtime.block_on(dispatch(&name, args));
                record_call(&name, started, &out);
                out
            }
            // 规范：ping 回空对象。以前落到 method not found，客户端的心跳
            // 探测会把我们判成坏掉的服务器。
            "ping" => Ok(json!({})),
            _ => Err(format!("method not found: {method}")),
        };

        let body = match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(message) => {
                // 未知方法是 -32601（Method not found），别的才是 -32603（Internal）。
                // 客户端会按码决定要不要重试 —— 把「你不支持这个」报成「我内部
                // 出错了」，它就会一直重试一个永远不会成功的调用。
                let code = if message.starts_with("method not found") { -32601 } else { -32603 };
                json!({
                    "jsonrpc": "2.0", "id": id,
                    "error": { "code": code, "message": message },
                })
            }
        };
        let mut line = body.to_string();
        line.push('\n');
        use std::io::Write;
        if out.write_all(line.as_bytes()).is_err() || out.flush().is_err() {
            break;
        }
    }
    0
}

/// 工具清单。
///
/// 为什么不是「一个 describe_image 加个 prompt 参数就够了」：宿主模型是按
/// **名字和描述**决定调不调工具的。用户说「帮我看看这个报错截图上写了什么」时，
/// 一个叫 `read_image_text` 的工具会被叫起来，而一个泛化的 `describe_image`
/// 经常不会 —— 模型不觉得自己需要「描述」。所以每一种真实意图给一个名字，
/// 内部再共用同一条实现。
fn tool_specs() -> Value {
    // 三种取图方式共用一套参数说明，逐个工具重抄会漂。
    let source_props = || {
        json!({
            "path": {
                "type": "string",
                "description": "Absolute path to a local image file (png/jpg/gif/webp)",
            },
            "url": {
                "type": "string",
                "description": "Remote URL or a data:image/...;base64,... URL of the image",
            },
            "image": {
                "type": "string",
                "description":
                    "Pasted-image index when the transcript only shows [Image 1] with no path. \
                     \"1\" is [Image 1], \"2\" is [Image 2], \"latest\" is the most recent paste. \
                     Omit to use the latest paste.",
            },
        })
    };
    json!([
        {
            "name": "describe_image",
            "description":
                "Describe an image with a vision-capable model. Use this whenever the user \
                 pastes or mentions an image (screenshot, photo, diagram, chart) and you \
                 cannot see images yourself. If the chat only shows [Image 1] with no file \
                 path, pass image=\"1\" — do not ask the user to save the file.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Absolute path to the image file" },
                    "url": { "type": "string", "description": "Remote URL or data: URL of the image" },
                    "image": {
                        "type": "string",
                        "description": "Pasted-image index: \"1\" for [Image 1], \"latest\" for the newest. Omit to use the latest paste.",
                    },
                    "prompt": {
                        "type": "string",
                        "description": "What to look for; default is a detailed description",
                    },
                },
            },
            "required": [],
        },
        {
            "name": "read_image_text",
            "description":
                "Transcribe every piece of text in an image, verbatim and in reading order. \
                 Use this for screenshots of errors, stack traces, logs, terminal output, \
                 code, forms, or any image where the exact characters matter. For a pasted \
                 [Image N] placeholder, pass image=\"N\".",
            "inputSchema": { "type": "object", "properties": source_props() },
            "required": [],
        },
        {
            "name": "compare_images",
            "description":
                "Compare two images and report what changed. Use this for before/after \
                 screenshots, visual regressions, or 'why does my UI look different' questions.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "before_path": { "type": "string", "description": "Absolute path to the first image" },
                    "before_url": { "type": "string", "description": "URL of the first image" },
                    "after_path": { "type": "string", "description": "Absolute path to the second image" },
                    "after_url": { "type": "string", "description": "URL of the second image" },
                    "prompt": {
                        "type": "string",
                        "description": "What to focus on; default is every visible difference",
                    },
                },
            },
            "required": [],
        },
        {
            "name": "list_pasted_images",
            "description":
                "List images the user recently pasted into this CLI session, with the on-disk \
                 paths the other vision tools accept. Call this when you see [Image 1] / \
                 [Image 2] and need to know which file is which.",
            "inputSchema": { "type": "object", "properties": {} },
            "required": [],
        },
        {
            "name": "describe_screen",
            "description":
                "Capture the user's screen right now and describe it. Use this when the user \
                 refers to what is currently on their screen ('look at this', 'what does this \
                 dialog say') without giving you a file.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "prompt": {
                        "type": "string",
                        "description": "What to look for; default is a detailed description",
                    },
                },
            },
            "required": [],
        },
    ])
}

/// 默认提示词。抽出来是为了让四个工具的差异只剩「问什么」这一件事。
const PROMPT_DESCRIBE: &str = "Describe this image in detail: the scene, any visible text \
     (transcribe it verbatim), UI elements, and chart values.";
const PROMPT_OCR: &str = "Transcribe ALL text visible in this image, verbatim, in reading order. \
     Preserve line breaks, indentation, and punctuation exactly. Do not summarise, translate, \
     correct typos, or add commentary. If a character is genuinely unreadable, write [?].";
const PROMPT_COMPARE: &str = "These are two versions of the same thing (first = before, \
     second = after). List every visible difference: layout, spacing, colour, text content, \
     and anything present in one but not the other. Be specific about where each change is.";

async fn dispatch(name: &str, params: &Value) -> Result<Value, String> {
    match name {
        "describe_image" => {
            let img = load_source(params, "path", "url").await?;
            ask_vision(&[img], prompt_or(params, PROMPT_DESCRIBE)).await
        }
        "read_image_text" => {
            let img = load_source(params, "path", "url").await?;
            // OCR 不接受自定义 prompt：这个工具的全部价值就是「一字不改地抄
            // 下来」，让调用方改写提示词等于把它变回 describe_image。
            ask_vision(&[img], PROMPT_OCR).await
        }
        "compare_images" => {
            let before = load_source(params, "before_path", "before_url").await?;
            let after = load_source(params, "after_path", "after_url").await?;
            ask_vision(&[before, after], prompt_or(params, PROMPT_COMPARE)).await
        }
        "list_pasted_images" => Ok(mcp_text(format_pasted_list(&collect_pasted()))),
        "describe_screen" => {
            let shot = capture_screen()?;
            ask_vision(&[shot], prompt_or(params, PROMPT_DESCRIBE)).await
        }
        other => Err(format!("unknown tool: {other}")),
    }
}

fn prompt_or<'a>(params: &'a Value, fallback: &'a str) -> &'a str {
    params
        .get("prompt")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(fallback)
}

/// 一张待发送的图。
///
/// `pub(crate)` 是给生图 MCP 用的：它的 `edit_image` 要接受和这里完全一样的
/// 三种来源（路径 / URL / `[Image N]` 编号）。再抄一份的下场，模块头那段注释
/// 已经写过一次了。
pub(crate) struct Image {
    pub(crate) bytes: Vec<u8>,
    pub(crate) media_type: &'static str,
}

pub(crate) fn mcp_text(s: String) -> Value {
    json!({ "content": [{ "type": "text", "text": s }] })
}

pub(crate) async fn load_source(
    params: &Value,
    path_key: &str,
    url_key: &str,
) -> Result<Image, String> {
    let path = params.get(path_key).and_then(Value::as_str).filter(|s| !s.is_empty());
    let url = params.get(url_key).and_then(Value::as_str).filter(|s| !s.is_empty());
    if let Some(p) = path {
        let (bytes, media_type) = read_image_file(p)?;
        return Ok(Image { bytes, media_type });
    }
    if let Some(u) = url {
        let (bytes, media_type) = if u.starts_with("data:") {
            decode_data_url(u)?
        } else {
            fetch_image(u).await?
        };
        return Ok(Image { bytes, media_type });
    }
    // 对话里只有 `[Image 1]` 时模型手里没有路径。图其实已经落在会话目录，
    // 按编号从那里取，不要把「请存到 Downloads」当成标准流程。
    let files = collect_pasted();
    let refer = match params.get("image") {
        Some(Value::String(s)) => parse_image_ref(s).unwrap_or(PasteRef::Latest),
        Some(Value::Number(n)) => n
            .as_u64()
            .filter(|&v| v >= 1)
            .map(|v| PasteRef::Index(v as usize))
            .unwrap_or(PasteRef::Latest),
        _ => PasteRef::Latest,
    };
    let picked = pick_pasted(&files, refer)?;
    let (bytes, media_type) = read_image_file(&picked.to_string_lossy())?;
    Ok(Image { bytes, media_type })
}

/// `[Image 1]` / `1` / `latest` → 取第几张贴进来的图。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PasteRef {
    Latest,
    Index(usize),
}

fn parse_image_ref(raw: &str) -> Option<PasteRef> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    if s.eq_ignore_ascii_case("latest") || s.eq_ignore_ascii_case("last") {
        return Some(PasteRef::Latest);
    }
    let mut t = s.trim_matches(|c: char| c == '[' || c == ']').trim().to_string();
    for prefix in ["Image", "image", "IMAGE"] {
        if let Some(rest) = t.strip_prefix(prefix) {
            t = rest.trim().to_string();
            break;
        }
    }
    let t = t.trim_start_matches('#').trim();
    t.parse::<usize>().ok().filter(|&n| n >= 1).map(PasteRef::Index)
}

fn pick_pasted(files: &[PathBuf], refer: PasteRef) -> Result<PathBuf, String> {
    if files.is_empty() {
        return Err(
            "没有找到最近贴进来的图。有本地路径用 path，有网址用 url；\
             只有 [Image N] 时用 image=\"N\"。"
                .into(),
        );
    }
    match refer {
        PasteRef::Latest => Ok(files[files.len() - 1].clone()),
        PasteRef::Index(n) => files.get(n - 1).cloned().ok_or_else(|| {
            format!(
                "没有 [Image {n}]。{}\n改用 image=\"latest\" 或先调 list_pasted_images。",
                format_pasted_list(files)
            )
        }),
    }
}

fn format_pasted_list(files: &[PathBuf]) -> String {
    if files.is_empty() {
        return "没有最近贴进来的图。".into();
    }
    let now = SystemTime::now();
    let mut lines = vec![format!("最近贴进来的图（{} 张，[Image 1] 是最早那张）：", files.len())];
    for (i, p) in files.iter().enumerate() {
        let ago = std::fs::metadata(p)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .map(|d| {
                let s = d.as_secs();
                if s < 60 {
                    format!("{s}s 前")
                } else {
                    format!("{}min 前", s / 60)
                }
            })
            .unwrap_or_else(|| "?".into());
        lines.push(format!("{}. {} ({ago})", i + 1, p.display()));
    }
    lines.join("\n")
}

const PASTE_WINDOW_SECS: u64 = 30 * 60;

/// 当前会话里刚贴进来的图，按时间从早到晚 = [Image 1] … [Image N]。
fn collect_pasted() -> Vec<PathBuf> {
    let mut files = Vec::new();
    grok_paste_dirs().into_iter().for_each(|d| collect_images_in(&d, &mut files));
    claude_paste_dirs().into_iter().for_each(|d| collect_images_in(&d, &mut files));
    finalize_pasted(files)
}

fn collect_images_in(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for ent in rd.flatten() {
        let p = ent.path();
        if !p.is_file() {
            continue;
        }
        if is_image_file(&p) {
            out.push(p);
        }
    }
}

fn is_image_file(p: &Path) -> bool {
    matches!(
        p.extension().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase().as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "webp"
    )
}

fn finalize_pasted(mut files: Vec<PathBuf>) -> Vec<PathBuf> {
    files.sort_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok());
    files.dedup();
    let now = SystemTime::now();
    let recent: Vec<PathBuf> = files
        .iter()
        .filter(|p| {
            std::fs::metadata(p)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| now.duration_since(t).ok())
                .is_some_and(|d| d.as_secs() <= PASTE_WINDOW_SECS)
        })
        .cloned()
        .collect();
    if !recent.is_empty() {
        return recent;
    }
    let n = files.len();
    files.into_iter().skip(n.saturating_sub(10)).collect()
}

fn grok_paste_dirs() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let sessions = home.join(".grok/sessions");
    let mut dirs = Vec::new();
    if let Ok(sid) = std::env::var("GROK_SESSION_ID") {
        if !sid.is_empty() {
            if let Ok(rd) = std::fs::read_dir(&sessions) {
                for proj in rd.flatten() {
                    let cand = proj.path().join(&sid);
                    if cand.is_dir() {
                        dirs.push(cand.join("images"));
                        dirs.push(cand.join("assets"));
                    }
                }
            }
        }
    }
    if dirs.is_empty() {
        if let Some(cwd) = std::env::var("GROK_WORKSPACE_ROOT")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| std::env::current_dir().ok().map(|p| p.display().to_string()))
        {
            let slug = grok_cwd_slug(Path::new(&cwd));
            let proj = sessions.join(&slug);
            if let Some(latest) = newest_subdir(&proj) {
                dirs.push(latest.join("images"));
                dirs.push(latest.join("assets"));
            }
        }
    }
    dirs
}

fn claude_paste_dirs() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let cache = home.join(".claude/image-cache");
    if let Some(latest) = newest_subdir(&cache) {
        return vec![latest];
    }
    Vec::new()
}

fn newest_subdir(parent: &Path) -> Option<PathBuf> {
    let rd = std::fs::read_dir(parent).ok()?;
    rd.flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .max_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
}

/// Grok 把 cwd 编进 `~/.grok/sessions/<slug>/`：除字母数字 `.-_~` 以外都百分号编码，
/// `/Users/foo` → `%2FUsers%2Ffoo`。编错就会扫到别人的会话。
fn grok_cwd_slug(cwd: &Path) -> String {
    let mut out = String::new();
    for b in cwd.to_string_lossy().bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn decode_data_url(url: &str) -> Result<(Vec<u8>, &'static str), String> {
    let rest = url
        .strip_prefix("data:")
        .ok_or_else(|| "not a data: URL".to_string())?;
    let (meta, payload) = rest
        .split_once(',')
        .ok_or_else(|| "data: URL missing comma".to_string())?;
    let media_type = if meta.contains("png") {
        "image/png"
    } else if meta.contains("gif") {
        "image/gif"
    } else if meta.contains("webp") {
        "image/webp"
    } else {
        "image/jpeg"
    };
    let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, payload)
        .map_err(|e| format!("data: URL base64: {e}"))?;
    if bytes.is_empty() {
        return Err("data: URL decoded to empty".into());
    }
    Ok((bytes, media_type))
}

/// 抓一张全屏截图。
///
/// 只在 macOS 上实现：`screencapture` 是系统自带的，不引入依赖。`-x` 关快门
/// 声（MCP 是后台进程，响一声会吓到人）。第一次调用会弹系统的「录屏权限」
/// 授权框，没给权限时 screencapture 仍返回 0 但截出一张纯桌面图 —— 这一点
/// 没法从退出码判断，只能在错误文案里提醒。
#[cfg(target_os = "macos")]
fn capture_screen() -> Result<Image, String> {
    let path = std::env::temp_dir().join(format!("ccload-vision-{}.png", std::process::id()));
    let status = std::process::Command::new("/usr/sbin/screencapture")
        .args(["-x", "-t", "png"])
        .arg(&path)
        .status()
        .map_err(|e| format!("screencapture failed to start: {e}"))?;
    if !status.success() {
        return Err("screencapture failed — 检查系统设置里本应用的「屏幕录制」权限".into());
    }
    let bytes = std::fs::read(&path).map_err(|e| format!("cannot read screenshot: {e}"))?;
    // 临时文件删不掉不算失败：图已经读进内存了。
    let _ = std::fs::remove_file(&path);
    if bytes.is_empty() {
        return Err("screenshot is empty — 检查系统设置里本应用的「屏幕录制」权限".into());
    }
    Ok(Image {
        bytes,
        media_type: "image/png",
    })
}

#[cfg(not(target_os = "macos"))]
fn capture_screen() -> Result<Image, String> {
    Err("describe_screen 目前只在 macOS 上可用；请改用 describe_image 并给出文件路径".into())
}

/// 把若干张图 + 一句提示交给视觉模型，返回 MCP 的 text 内容块。
///
/// 走内核的 `/v1/messages`：模型别名、路由、故障转移都归内核管，这里只负责
/// 把图装进 Anthropic 的 content 数组。
async fn ask_vision(images: &[Image], prompt: &str) -> Result<Value, String> {
    if images.is_empty() {
        return Err("no image to look at".into());
    }
    let base = std::env::var(ENV_BASE_URL).map_err(|_| format!("{ENV_BASE_URL} not set"))?;
    let token = std::env::var(ENV_TOKEN).map_err(|_| format!("{ENV_TOKEN} not set"))?;
    let model = std::env::var(ENV_MODEL).map_err(|_| format!("{ENV_MODEL} not set"))?;

    let mut content: Vec<Value> = images
        .iter()
        .map(|img| {
            let encoded =
                base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &img.bytes);
            json!({ "type": "image", "source": {
                "type": "base64", "media_type": img.media_type, "data": encoded,
            }})
        })
        .collect();
    content.push(json!({ "type": "text", "text": prompt }));

    // no_proxy + 长超时，和内核共享客户端同一套理由：这台机器上常年挂着
    // HTTP_PROXY，而这里打的是本机内核；默认客户端会把请求交给代理，直接失败。
    // 看图是长任务（要传一张 base64 图再等模型生成描述），30s 太短。
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(180))
        .build()
        .map_err(|e| format!("build http client: {e}"))?;
    let endpoint = format!("{}/v1/messages", base.trim_end_matches('/'));
    let resp = client
        .post(&endpoint)
        .header("x-api-key", &token)
        .header("authorization", format!("Bearer {token}"))
        .header("anthropic-version", "2023-06-01")
        .json(&json!({
            "model": model,
            "max_tokens": 4096,
            "messages": [{ "role": "user", "content": content }],
        }))
        .send()
        .await
        .map_err(|e| format!("request to kernel failed: {e}"))?;

    let status = resp.status();
    let body: Value = resp.json().await.map_err(|e| format!("bad kernel response: {e}"))?;
    if !status.is_success() {
        return Err(body
            .pointer("/error/message")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("kernel returned HTTP {status}")));
    }
    let text = body
        .pointer("/content")
        .and_then(Value::as_array)
        .and_then(|parts| {
            parts
                .iter()
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
                .into()
        })
        .filter(|s: &String| !s.is_empty())
        .unwrap_or_else(|| "(vision model returned no text)".into());

    Ok(json!({ "content": [{ "type": "text", "text": text }] }))
}

/// 记一条调用流水。失败原因截到一行 200 字：流水是 JSONL，一条上游返回的
/// 多行报错会把文件搅成解析不了的样子。
///
/// 生图 MCP 也用它 —— 两个服务器写同一份流水，统计页面才是一个口径。
pub(crate) fn record_call(tool: &str, started: std::time::Instant, out: &Result<Value, String>) {
    let err = out.as_ref().err().map(|e| {
        let one_line: String = e.chars().map(|c| if c == '\n' { ' ' } else { c }).collect();
        one_line.chars().take(200).collect::<String>()
    });
    mcp_usage::record(&mcp_usage::McpCall {
        tool: tool.to_string(),
        at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0),
        ms: started.elapsed().as_millis() as u64,
        ok: out.is_ok(),
        err,
    });
}

fn read_image_file(path: &str) -> Result<(Vec<u8>, &'static str), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    let media_type = match path.rsplit('.').next().unwrap_or("").to_ascii_lowercase().as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        other => return Err(format!("unsupported image type: {other}")),
    };
    Ok((bytes, media_type))
}

async fn fetch_image(url: &str) -> Result<(Vec<u8>, &'static str), String> {
    let resp = reqwest::get(url).await.map_err(|e| format!("fetch failed: {e}"))?;
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let bytes = resp.bytes().await.map_err(|e| format!("download failed: {e}"))?;
    let media_type = match content_type.as_str() {
        t if t.contains("png") => "image/png",
        t if t.contains("gif") => "image/gif",
        t if t.contains("webp") => "image/webp",
        _ => "image/jpeg",
    };
    Ok((bytes.to_vec(), media_type))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_disable_removes_entry() {
        let dir = tempfile::tempdir().unwrap();
        let root = ConfigRoot::sandbox(dir.path().to_path_buf());
        let bk = BackupStore::new(dir.path().join("bk"));
        let path = root.join(".claude.json");
        std::fs::write(
            &path,
            r#"{"mcpServers":{"other":{"command":"x"},"ccload-vision":{"command":"old"}}}"#,
        )
        .unwrap();

        let cfg = VisionConfig {
            base_url: "http://127.0.0.1:8990".into(),
            token: "t".into(),
            model: "m".into(),
        };
        set_vision_mcp(&root, CliTarget::ClaudeCode, false, &cfg, "s1", &bk).unwrap();

        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(doc.pointer("/mcpServers/ccload-vision").is_none());
        assert!(doc.pointer("/mcpServers/other").is_some(), "untouched entries survive");
    }

    #[test]
    fn opencode_write_then_disable() {
        let dir = tempfile::tempdir().unwrap();
        let root = ConfigRoot::sandbox(dir.path().to_path_buf());
        let bk = BackupStore::new(dir.path().join("bk"));
        let path = root.join(".config/opencode/opencode.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{}").unwrap();

        let cfg = VisionConfig {
            base_url: "http://127.0.0.1:8990".into(),
            token: "tok".into(),
            model: "vision-model".into(),
        };
        set_vision_mcp(&root, CliTarget::OpenCode, true, &cfg, "s1", &bk).unwrap();
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            doc.pointer("/mcp/ccload-vision/environment/CCLOAD_VISION_MODEL").unwrap(),
            "vision-model"
        );

        set_vision_mcp(&root, CliTarget::OpenCode, false, &cfg, "s2", &bk).unwrap();
        let doc: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(doc.pointer("/mcp/ccload-vision").is_none());
    }

    /// 这两家以前被 `set_vision_mcp` 直接拒绝，而扩展管理那边一直能写。
    #[test]
    fn gemini_and_grok_are_supported() {
        let dir = tempfile::tempdir().unwrap();
        let root = ConfigRoot::sandbox(dir.path().to_path_buf());
        let bk = BackupStore::new(dir.path().join("bk"));
        let cfg = VisionConfig {
            base_url: "http://127.0.0.1:8990".into(),
            token: "t".into(),
            model: "m".into(),
        };

        set_vision_mcp(&root, CliTarget::GeminiCli, true, &cfg, "s1", &bk).unwrap();
        let gemini: Value = serde_json::from_str(
            &std::fs::read_to_string(root.join(".gemini/settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            gemini.pointer("/mcpServers/ccload-vision/env/CCLOAD_VISION_MODEL").unwrap(),
            "m"
        );

        set_vision_mcp(&root, CliTarget::GrokBuild, true, &cfg, "s2", &bk).unwrap();
        let grok = std::fs::read_to_string(root.join(".grok/config.toml")).unwrap();
        assert!(grok.contains("[mcp_servers.ccload-vision]"), "{grok}");
        assert!(grok.contains("CCLOAD_VISION_MODEL"), "{grok}");
    }

    /// 没装过就点「移除」是常见操作，不该弹一个「找不到」的红字。
    #[test]
    fn disabling_a_never_installed_target_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let root = ConfigRoot::sandbox(dir.path().to_path_buf());
        let bk = BackupStore::new(dir.path().join("bk"));
        let cfg = VisionConfig {
            base_url: "u".into(),
            token: "t".into(),
            model: "m".into(),
        };
        let written = set_vision_mcp(&root, CliTarget::Codex, false, &cfg, "s1", &bk).unwrap();
        assert!(written.is_empty());
    }

    /// 前端全线用 `opencode`；写成 kebab-case 的 `open-code` 会让每一条带
    /// target 的命令在 OpenCode 上静默失败（实际发生过）。
    #[test]
    fn opencode_serializes_as_one_word() {
        assert_eq!(
            serde_json::to_string(&CliTarget::OpenCode).unwrap(),
            "\"opencode\""
        );
        assert_eq!(
            serde_json::from_str::<CliTarget>("\"opencode\"").unwrap(),
            CliTarget::OpenCode
        );
    }

    // -----------------------------------------------------------------------
    // 读回状态
    //
    // 「选了模型但没保存上」的根因是模型只活在渲染进程里。真值一直在配置
    // 文件里，下面这几条钉住「装完就读得回来」这件事。
    // -----------------------------------------------------------------------

    fn cfg(model: &str) -> VisionConfig {
        VisionConfig {
            base_url: "http://127.0.0.1:8990".into(),
            token: "tok".into(),
            model: model.into(),
        }
    }

    /// 五家的 env 键名并不统一（OpenCode 是 `environment`），读回必须都认。
    #[test]
    fn installed_model_reads_back_on_every_target() {
        for target in crate::services::cli_extensions::ALL_TARGETS {
            let dir = tempfile::tempdir().unwrap();
            let root = ConfigRoot::sandbox(dir.path().to_path_buf());
            let bk = BackupStore::new(dir.path().join("bk"));

            assert!(
                read_vision_mcp(&root, target).unwrap().is_none(),
                "{target:?} 没装时应当是 None"
            );

            set_vision_mcp(&root, target, true, &cfg("qwen3-vl"), "s1", &bk).unwrap();
            let got = read_vision_mcp(&root, target)
                .unwrap()
                .unwrap_or_else(|| panic!("{target:?} 装完却读不回来"));
            assert_eq!(got.model, "qwen3-vl", "{target:?}");
            assert_eq!(got.base_url, "http://127.0.0.1:8990", "{target:?}");
            assert_eq!(got.token, "tok", "{target:?}");
        }
    }

    /// 装着旧内核的地址/令牌，和「没装」是两回事：配置看着好，每次看图都 401。
    #[test]
    fn stale_credentials_are_flagged_not_hidden() {
        let dir = tempfile::tempdir().unwrap();
        let root = ConfigRoot::sandbox(dir.path().to_path_buf());
        let bk = BackupStore::new(dir.path().join("bk"));
        set_vision_mcp(&root, CliTarget::ClaudeCode, true, &cfg("qwen3-vl"), "s1", &bk).unwrap();

        let fresh = vision_states(
            &root,
            &[CliTarget::ClaudeCode],
            "http://127.0.0.1:8990",
            Some("tok"),
        );
        assert!(fresh[0].installed);
        assert!(!fresh[0].stale, "地址和令牌都对，不该报过期");
        assert_eq!(fresh[0].model.as_deref(), Some("qwen3-vl"));

        // 令牌换了（内核重启后重新签发）→ 装着，但打不通。
        let stale = vision_states(
            &root,
            &[CliTarget::ClaudeCode],
            "http://127.0.0.1:8990",
            Some("another-token"),
        );
        assert!(stale[0].installed);
        assert!(stale[0].stale);

        // 端口换了同理。尾斜杠不算差异。
        let moved = vision_states(
            &root,
            &[CliTarget::ClaudeCode],
            "http://127.0.0.1:9999",
            Some("tok"),
        );
        assert!(moved[0].stale);
        let same = vision_states(
            &root,
            &[CliTarget::ClaudeCode],
            "http://127.0.0.1:8990/",
            Some("tok"),
        );
        assert!(!same[0].stale, "只差一个尾斜杠不该被判成过期");
    }

    /// 一家的配置坏了不该让另外四家的状态一起消失 —— 这一格只是用来点亮
    /// 界面的，整次失败等于用户什么都看不到。
    #[test]
    fn one_broken_config_does_not_sink_the_others() {
        let dir = tempfile::tempdir().unwrap();
        let root = ConfigRoot::sandbox(dir.path().to_path_buf());
        let bk = BackupStore::new(dir.path().join("bk"));
        set_vision_mcp(&root, CliTarget::ClaudeCode, true, &cfg("qwen3-vl"), "s1", &bk).unwrap();
        // Codex 的 config.toml 被手工编辑坏了。
        let codex = root.join(".codex/config.toml");
        std::fs::create_dir_all(codex.parent().unwrap()).unwrap();
        std::fs::write(&codex, "this = = not toml").unwrap();

        let states = vision_states(
            &root,
            &[CliTarget::ClaudeCode, CliTarget::Codex],
            "http://127.0.0.1:8990",
            Some("tok"),
        );
        assert!(states[0].installed, "好的那家照常显示");
        assert_eq!(states[0].model.as_deref(), Some("qwen3-vl"));
        assert!(!states[1].installed, "坏的那家算没装，而不是整次报错");
    }

    /// 四个工具都要在 tools/list 里，且名字和 `dispatch` 认的一致 —— 少一个
    /// 宿主模型就永远不会调它，多一个则会调到一个返回 "unknown tool" 的名字。
    #[test]
    fn every_advertised_tool_is_dispatchable() {
        let specs = tool_specs();
        let names: Vec<&str> = specs
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            vec![
                "describe_image",
                "read_image_text",
                "compare_images",
                "list_pasted_images",
                "describe_screen"
            ]
        );

        // dispatch 对未知名字必须报错，对已知名字必须走到取图那一步。
        // 给一个不存在的 path，这样不会扫到本机真的贴图再去打网关。
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        for name in &names {
            if *name == "describe_screen" {
                continue; // 会真的去截屏，不在单测里跑
            }
            if *name == "list_pasted_images" {
                let out = rt
                    .block_on(dispatch(name, &serde_json::json!({})))
                    .unwrap();
                assert!(
                    out.pointer("/content/0/text")
                        .and_then(Value::as_str)
                        .is_some(),
                    "{name} 应当返回文本列表"
                );
                continue;
            }
            let err = rt
                .block_on(dispatch(
                    name,
                    &serde_json::json!({ "path": "/no/such/ccload-vision-test.png",
                        "before_path": "/no/such/ccload-vision-test.png",
                        "after_path": "/no/such/ccload-vision-test-2.png" }),
                ))
                .unwrap_err();
            assert!(
                err.contains("cannot read"),
                "{name} 应当落到取图那一步，实际：{err}"
            );
        }
        let err = rt
            .block_on(dispatch("nope", &serde_json::json!({})))
            .unwrap_err();
        assert!(err.contains("unknown tool"), "{err}");
    }

    /// OCR 的全部价值就是「一字不改地抄下来」。允许调用方改写提示词等于把它
    /// 变回 describe_image，所以 read_image_text 明确忽略 prompt。
    #[test]
    fn ocr_prompt_is_not_overridable() {
        assert_eq!(prompt_or(&serde_json::json!({"prompt": "总结一下"}), PROMPT_OCR), "总结一下");
        // dispatch 里 read_image_text 走的是常量而不是 prompt_or —— 这条断言
        // 守的是那个选择本身。
        assert!(PROMPT_OCR.contains("verbatim"));
        assert!(PROMPT_OCR.contains("Do not summarise"));
    }

    #[test]
    fn image_ref_parses_placeholder_and_number() {
        assert_eq!(parse_image_ref("1"), Some(PasteRef::Index(1)));
        assert_eq!(parse_image_ref("2"), Some(PasteRef::Index(2)));
        assert_eq!(parse_image_ref("[Image 1]"), Some(PasteRef::Index(1)));
        assert_eq!(parse_image_ref("[Image #2]"), Some(PasteRef::Index(2)));
        assert_eq!(parse_image_ref("Image 3"), Some(PasteRef::Index(3)));
        assert_eq!(parse_image_ref("latest"), Some(PasteRef::Latest));
        assert_eq!(parse_image_ref("LAST"), Some(PasteRef::Latest));
        assert_eq!(parse_image_ref(""), None);
        assert_eq!(parse_image_ref("0"), None);
    }

    #[test]
    fn grok_cwd_slug_percent_encodes_slashes() {
        assert_eq!(
            grok_cwd_slug(Path::new("/Users/light/Documents/2026-project/ccload-client")),
            "%2FUsers%2Flight%2FDocuments%2F2026-project%2Fccload-client"
        );
    }

    #[test]
    fn pasted_burst_is_numbered_oldest_first() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.png");
        let b = dir.path().join("b.png");
        std::fs::write(&a, b"a").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&b, b"b").unwrap();
        let mut files = Vec::new();
        collect_images_in(dir.path(), &mut files);
        let files = finalize_pasted(files);
        assert_eq!(files.len(), 2);
        assert_eq!(pick_pasted(&files, PasteRef::Index(1)).unwrap(), files[0]);
        assert_eq!(pick_pasted(&files, PasteRef::Index(2)).unwrap(), files[1]);
        assert_eq!(pick_pasted(&files, PasteRef::Latest).unwrap(), files[1]);
        let err = pick_pasted(&files, PasteRef::Index(9)).unwrap_err();
        assert!(err.contains("[Image 9]"), "{err}");
        assert!(err.contains("list_pasted_images"), "{err}");
    }

    #[test]
    fn data_url_decodes_base64_payload() {
        let raw = b"hello-img";
        let b64 = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, raw);
        let url = format!("data:image/png;base64,{b64}");
        let (bytes, media) = decode_data_url(&url).unwrap();
        assert_eq!(bytes, raw);
        assert_eq!(media, "image/png");
    }

    #[test]
    fn pick_pasted_empty_tells_the_model_what_to_pass() {
        let err = pick_pasted(&[], PasteRef::Latest).unwrap_err();
        assert!(err.contains("image="), "{err}");
        assert!(!err.contains("Downloads"), "{err}");
    }
}
