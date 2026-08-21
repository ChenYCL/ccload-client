//! 统一生图 / 改图能力，供五家 CLI 共用。
//!
//! # 为什么是一个 MCP 而不是各 CLI 各配一遍
//!
//! 和 `vision_mcp` 是同一个理由的镜像：视觉解决的是「模型看不见图」，这里解决
//! 的是「模型画不出图」。Claude Code / Codex / Gemini CLI / Grok Build /
//! OpenCode 本身都没有生图能力，而 ccLoad 手里已经有能生图的渠道 —— 把这条能力
//! 包成一个 stdio MCP 装进五家，写一次，五家都有了。
//!
//! 服务器就是客户端二进制自己（`ccload-client image-mcp`），所以不用额外装
//! 任何东西；MCP 条目怎么写进各家 CLI 的原生格式，交给 `cli_extensions`。
//!
//! # 内核的两条生图路径（这决定了整个模块的形状）
//!
//! 逐个对照过 `vendor/ccLoad/internal/app/admin_testing_image.go` 和
//! `internal/testutil/api_tester.go`，不是照着 OpenAI 文档猜的：
//!
//! | | 端点 | `size` 的写法 | 能改图吗 |
//! |---|---|---|---|
//! | [`ImageApi::Chat`] | `/v1/chat/completions` | `1:1@2k`（宽高比@档位） | **能** |
//! | [`ImageApi::Images`] | `/v1/images/generations` | `1024x1024`（像素） | 不能 |
//!
//! * chat 路径靠 `modalities:["image"]` + `image_config:{aspect_ratio,image_size}`
//!   开启，图从 `choices[].message.images[].image_url.url` 回来（data URL），
//!   老一点的上游放在 `content[]` 里的 `image_url` 块。
//! * images 路径是标准 OpenAI 形状，回 `data[].b64_json` 或 `data[].url`。
//!
//! **默认走 chat**：改图必须走它 —— images API 的 JSON body 塞不进一张输入图。
//! 只会生成不会改的上游（DALL·E 那类）才切到 images。
//!
//! # 为什么结果只回路径，不回图本身
//!
//! 一张 1024×1024 的 PNG 变成 base64 是一兆多。塞回工具结果里，每生成一张就往
//! transcript 里灌一兆 —— 这正是「会话救援」那一页要清理的东西（见
//! `session_rescue`）。所以图写到磁盘，只回绝对路径和尺寸；宿主模型想看自己画的
//! 是什么，调 `ccload-vision` 的 `describe_image` 就行。两个 MCP 就是这么配合的，
//! 工具描述里也这么写了。

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{json, Value};

use crate::error::AppError;
use crate::services::cli_backup::BackupStore;
use crate::services::cli_extensions::{self, ExtensionKind, ExtensionSpec, McpTransport};
use crate::services::cli_types::{CliTarget, ConfigRoot};
use crate::services::vision_mcp::{load_source, mcp_text, record_call, same_endpoint, Image};

pub const MCP_NAME: &str = "ccload-image";

const ENV_BASE_URL: &str = "CCLOAD_IMAGE_BASE_URL";
const ENV_TOKEN: &str = "CCLOAD_IMAGE_TOKEN";
const ENV_MODEL: &str = "CCLOAD_IMAGE_MODEL";
const ENV_API: &str = "CCLOAD_IMAGE_API";
const ENV_DIR: &str = "CCLOAD_IMAGE_DIR";

/// 生图走哪条路。取值对应内核 admin 生图测试里的两个 API 分支。
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageApi {
    /// `/v1/chat/completions` + `modalities:["image"]`。**能改图**，所以是默认。
    Chat,
    /// `/v1/images/generations`。标准 OpenAI 形状，只能生成。
    Images,
}

impl ImageApi {
    fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Images => "images",
        }
    }

    /// 认不出来一律当 chat：那是能力更全的一条，猜错的代价小。
    fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "images" => Self::Images,
            _ => Self::Chat,
        }
    }
}

pub struct ImageConfig {
    pub base_url: String,
    pub token: String,
    pub model: String,
    pub api: ImageApi,
    /// 生成的图往哪写。空 = `~/.ccload-client/images`。
    pub out_dir: String,
}

/// 一个 CLI 上生图 MCP 的当前状态。和 `VisionTargetState` 同构 —— 状态从磁盘
/// 读，不靠按钮的记忆。
#[derive(Debug, serde::Serialize)]
pub struct ImageTargetState {
    pub target: CliTarget,
    pub label: &'static str,
    pub installed: bool,
    pub model: Option<String>,
    pub api: Option<String>,
    /// 装了，但里面存的内核地址 / 令牌已经过期 —— 每次生图都会 401。
    pub stale: bool,
}

pub fn read_image_mcp(
    root: &ConfigRoot,
    target: CliTarget,
) -> Result<Option<ImageConfig>, AppError> {
    let installed = cli_extensions::list(root, target, Some(ExtensionKind::Mcp))?
        .iter()
        .any(|i| i.id == MCP_NAME);
    if !installed {
        return Ok(None);
    }
    let spec = cli_extensions::read_spec(root, target, ExtensionKind::Mcp, MCP_NAME)?;
    let get = |k: &str| spec.env.get(k).cloned().unwrap_or_default();
    Ok(Some(ImageConfig {
        base_url: get(ENV_BASE_URL),
        token: get(ENV_TOKEN),
        model: get(ENV_MODEL),
        api: ImageApi::parse(&get(ENV_API)),
        out_dir: get(ENV_DIR),
    }))
}

/// 五个 CLI 各自的状态。某家读不动算「没装」而不是整次失败。
pub fn image_states(
    root: &ConfigRoot,
    targets: &[CliTarget],
    kernel_base_url: &str,
    kernel_token: Option<&str>,
) -> Vec<ImageTargetState> {
    targets
        .iter()
        .copied()
        .map(|target| match read_image_mcp(root, target) {
            Ok(Some(cfg)) => {
                let base_ok = same_endpoint(&cfg.base_url, kernel_base_url);
                let token_ok = kernel_token.is_none_or(|want| cfg.token == want);
                ImageTargetState {
                    target,
                    label: target.label(),
                    installed: true,
                    model: Some(cfg.model).filter(|m| !m.is_empty()),
                    api: Some(cfg.api.as_str().to_string()),
                    stale: !(base_ok && token_ok),
                }
            }
            _ => ImageTargetState {
                target,
                label: target.label(),
                installed: false,
                model: None,
                api: None,
                stale: false,
            },
        })
        .collect()
}

/// 装 / 卸生图 MCP。返回写过的文件。
pub fn set_image_mcp(
    root: &ConfigRoot,
    target: CliTarget,
    enabled: bool,
    cfg: &ImageConfig,
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
            description: Some("ccLoad 生图：文生图与改图，结果写到磁盘".into()),
            transport: Some(McpTransport::Stdio),
            command: Some(exe),
            args: vec!["image-mcp".into()],
            env: image_env(cfg),
            ..Default::default()
        };
        return cli_extensions::install(root, target, ExtensionKind::Mcp, &spec, stamp, backups);
    }

    // 没装过就当已经达成目标 —— 用户点「移除」要的是「最终它不在」。
    let installed = cli_extensions::list(root, target, Some(ExtensionKind::Mcp))?
        .iter()
        .any(|i| i.id == MCP_NAME);
    if !installed {
        return Ok(Vec::new());
    }
    cli_extensions::remove(root, target, ExtensionKind::Mcp, MCP_NAME, stamp, backups)
}

fn image_env(cfg: &ImageConfig) -> BTreeMap<String, String> {
    BTreeMap::from([
        (ENV_BASE_URL.to_string(), cfg.base_url.clone()),
        (ENV_TOKEN.to_string(), cfg.token.clone()),
        (ENV_MODEL.to_string(), cfg.model.clone()),
        (ENV_API.to_string(), cfg.api.as_str().to_string()),
        (ENV_DIR.to_string(), cfg.out_dir.clone()),
    ])
}

// ---------------------------------------------------------------------------
// stdio MCP server (`ccload-client image-mcp`)
// ---------------------------------------------------------------------------

/// Serve MCP over stdin/stdout until EOF.
pub fn serve_stdio() -> i32 {
    use std::io::{BufRead, Write};

    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("image-mcp: no runtime: {e}");
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
        let Ok(msg) = serde_json::from_str::<Value>(&line) else {
            continue; // 不是 JSON-RPC，按 MCP 的惯例忽略
        };
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        // 通知（没有 id）按 JSON-RPC 不回。
        let Some(id) = msg.get("id").cloned() else {
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
                let args = params.get("arguments").unwrap_or(&params);
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("generate_image")
                    .to_string();
                let started = std::time::Instant::now();
                let out = runtime.block_on(dispatch(&name, args));
                record_call(&name, started, &out);
                out
            }
            _ => Err("method not found".to_string()),
        };

        let body = match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(message) => json!({
                "jsonrpc": "2.0", "id": id,
                "error": { "code": -32603, "message": message },
            }),
        };
        let mut line = body.to_string();
        line.push('\n');
        if out.write_all(line.as_bytes()).is_err() || out.flush().is_err() {
            break;
        }
    }
    0
}

/// 工具清单。
///
/// 名字按**真实意图**取，不是按实现取 —— 和 `vision_mcp::tool_specs` 同一条
/// 教训：宿主模型是看名字和描述决定调不调的。用户说「给我画个图标」时，一个叫
/// `generate_image` 的工具会被叫起来；一个泛化的 `image_op(action=...)` 不会。
fn tool_specs() -> Value {
    json!([
        {
            "name": "generate_image",
            "description":
                "Generate an image from a text description and save it to disk. Use this \
                 whenever the user asks for a picture, icon, sprite, texture, game asset, \
                 UI mockup, logo, illustration, or any other visual that does not exist yet. \
                 Returns the absolute path of the saved file — you cannot see the image \
                 itself, so call the ccload-vision tool `describe_image` on that path if you \
                 need to check the result before showing it to the user.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "prompt": {
                        "type": "string",
                        "description":
                            "What to draw. Be specific about subject, style, composition, \
                             colours and background (e.g. 'flat vector inventory icon of a \
                             health potion, 2px outline, transparent background').",
                    },
                    "size": {
                        "type": "string",
                        "description":
                            "Chat API: aspect ratio and tier, one of 1:1 16:9 9:16 3:2 2:3 \
                             optionally with @1k or @2k (default 1:1@2k). Images API: pixel \
                             dimensions like 1024x1024. Omit for the default.",
                    },
                    "out_path": {
                        "type": "string",
                        "description":
                            "Absolute path to write the image to. Omit to auto-name it under \
                             the configured output directory.",
                    },
                },
                "required": ["prompt"],
            },
        },
        {
            "name": "edit_image",
            "description":
                "Edit an existing image according to an instruction and save the result as a \
                 NEW file — the original is never modified. Use this for 'make the button \
                 rounder', 'change the background to night', 'remove the watermark', 'turn \
                 this sketch into a finished sprite', or to combine several reference images \
                 into one. If the chat only shows [Image 1] with no file path, pass \
                 image=\"1\" instead of asking the user to save a copy.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "prompt": {
                        "type": "string",
                        "description": "What to change. Describe the desired result, not the steps.",
                    },
                    "path": { "type": "string", "description": "Absolute path of the image to edit" },
                    "url": { "type": "string", "description": "Remote URL or data: URL of the image to edit" },
                    "image": {
                        "type": "string",
                        "description":
                            "Pasted-image index when the transcript only shows [Image 1]: \
                             \"1\" is [Image 1], \"latest\" is the newest paste.",
                    },
                    "extra_paths": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description":
                            "Absolute paths of additional reference images, e.g. a character \
                             sheet plus a background to place it in.",
                    },
                    "size": { "type": "string", "description": "Same format as generate_image" },
                    "out_path": { "type": "string", "description": "Absolute path for the result" },
                },
                "required": ["prompt"],
            },
        },
    ])
}

async fn dispatch(name: &str, params: &Value) -> Result<Value, String> {
    let cfg = env_config()?;
    let prompt = params
        .get("prompt")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or("prompt is required")?;

    let inputs: Vec<Image> = match name {
        "generate_image" => Vec::new(),
        "edit_image" => {
            let mut v = vec![load_source(params, "path", "url").await?];
            for extra in params
                .get("extra_paths")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or(&[])
            {
                let Some(p) = extra.as_str().filter(|s| !s.is_empty()) else {
                    continue;
                };
                v.push(load_source(&json!({ "path": p }), "path", "url").await?);
            }
            v
        }
        other => return Err(format!("unknown tool: {other}")),
    };

    // 改图必须走 chat：images API 的 JSON body 没有放输入图的位置。与其发出去
    // 让上游回一个语焉不详的 400，不如在这里说清楚该怎么改配置。
    if !inputs.is_empty() && cfg.api == ImageApi::Images {
        return Err(
            "edit_image needs the chat API, but this server is configured with \
             CCLOAD_IMAGE_API=images (the Images API cannot take an input image). \
             Switch it to chat in the ccLoad client."
                .into(),
        );
    }

    let size = params.get("size").and_then(Value::as_str).unwrap_or("");
    let images = match cfg.api {
        ImageApi::Chat => request_chat(&cfg, prompt, &inputs, size).await?,
        ImageApi::Images => request_images(&cfg, prompt, size).await?,
    };
    if images.is_empty() {
        return Err("the model returned no image".into());
    }

    let explicit = params
        .get("out_path")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let mut lines = Vec::new();
    for (i, raw) in images.iter().enumerate() {
        let path = save_image(&cfg, raw, explicit.filter(|_| i == 0), name, i)?;
        lines.push(path);
    }
    Ok(mcp_text(format!(
        "Saved {} image(s):\n{}\n\nYou cannot see these files. To check the result, call the \
         ccload-vision tool describe_image with path set to one of the paths above.",
        lines.len(),
        lines.join("\n"),
    )))
}

fn env_config() -> Result<ImageConfig, String> {
    let need = |k: &str| std::env::var(k).map_err(|_| format!("{k} not set"));
    Ok(ImageConfig {
        base_url: need(ENV_BASE_URL)?,
        token: need(ENV_TOKEN)?,
        model: need(ENV_MODEL)?,
        api: ImageApi::parse(&std::env::var(ENV_API).unwrap_or_default()),
        out_dir: std::env::var(ENV_DIR).unwrap_or_default(),
    })
}

/// `1:1@2k` / `16:9` / `2k` → `(aspect_ratio, image_size)`。
///
/// 取值范围抄自内核的 `validChatImageGenerationSize`：宽高比只认那五个，档位只认
/// 1k/2k，而且内核会把档位转成大写。发一个它不认的值，上游直接 400。
fn chat_size(raw: &str) -> (String, String) {
    let s = raw.trim().to_ascii_lowercase();
    if s.is_empty() || s == "auto" {
        return ("1:1".into(), "2K".into());
    }
    let (aspect, tier) = match s.split_once('@') {
        Some((a, t)) => (a.to_string(), t.to_string()),
        // 只给了一半：像 `16:9` 就是宽高比，像 `2k` 就是档位。
        None if s.contains(':') => (s.clone(), "2k".to_string()),
        None => ("1:1".to_string(), s.clone()),
    };
    let aspect = match aspect.as_str() {
        "1:1" | "16:9" | "9:16" | "3:2" | "2:3" => aspect,
        _ => "1:1".into(),
    };
    let tier = if tier == "1k" { "1K" } else { "2K" };
    (aspect, tier.into())
}

/// 像素尺寸。内核的 `validImageGenerationSize` 要求 64–8192，非法就退回默认，
/// 不要把一个注定 400 的值发出去。
fn images_size(raw: &str) -> String {
    let s = raw.trim().to_ascii_lowercase();
    if s.is_empty() || s == "auto" {
        return "1024x1024".into();
    }
    if let Some((w, h)) = s.split_once('x') {
        if let (Ok(w), Ok(h)) = (w.parse::<u32>(), h.parse::<u32>()) {
            if (64..=8192).contains(&w) && (64..=8192).contains(&h) {
                return format!("{w}x{h}");
            }
        }
    }
    "1024x1024".into()
}

fn http() -> Result<reqwest::Client, String> {
    // no_proxy + 长超时：内核多半在 127.0.0.1，而这台机器上常年挂着 HTTP_PROXY；
    // 生图本身也慢，30s 根本不够。
    reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .map_err(|e| format!("build http client: {e}"))
}

fn b64(bytes: &[u8]) -> String {
    base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes)
}

/// `/v1/chat/completions` + `modalities:["image"]`。生成和改图共用。
async fn request_chat(
    cfg: &ImageConfig,
    prompt: &str,
    inputs: &[Image],
    size: &str,
) -> Result<Vec<String>, String> {
    let (aspect_ratio, image_size) = chat_size(size);
    let mut content: Vec<Value> = inputs
        .iter()
        .map(|img| {
            json!({ "type": "image_url", "image_url": {
                "url": format!("data:{};base64,{}", img.media_type, b64(&img.bytes)),
            }})
        })
        .collect();
    content.push(json!({ "type": "text", "text": prompt }));

    let body = json!({
        "model": cfg.model,
        "modalities": ["image"],
        "image_config": { "aspect_ratio": aspect_ratio, "image_size": image_size },
        "messages": [{ "role": "user", "content": content }],
    });
    let resp = post(cfg, "/v1/chat/completions", &body).await?;
    Ok(extract_chat_images(&resp))
}

/// `/v1/images/generations`，标准 OpenAI 形状。
async fn request_images(cfg: &ImageConfig, prompt: &str, size: &str) -> Result<Vec<String>, String> {
    let body = json!({
        "model": cfg.model,
        "prompt": prompt,
        "size": images_size(size),
        "n": 1,
    });
    let resp = post(cfg, "/v1/images/generations", &body).await?;
    Ok(extract_images_api(&resp))
}

async fn post(cfg: &ImageConfig, path: &str, body: &Value) -> Result<Value, String> {
    let endpoint = format!("{}{path}", cfg.base_url.trim_end_matches('/'));
    let resp = http()?
        .post(&endpoint)
        .header("authorization", format!("Bearer {}", cfg.token))
        .header("x-api-key", &cfg.token)
        .json(body)
        .send()
        .await
        .map_err(|e| format!("request to kernel failed: {e}"))?;
    let status = resp.status();
    let parsed: Value = resp
        .json()
        .await
        .map_err(|e| format!("bad kernel response: {e}"))?;
    if !status.is_success() {
        let msg = parsed
            .pointer("/error/message")
            .and_then(Value::as_str)
            .or_else(|| parsed.get("error").and_then(Value::as_str))
            .unwrap_or("unknown error");
        return Err(format!("kernel returned HTTP {status}: {msg}"));
    }
    Ok(parsed)
}

/// 从 chat completions 响应里挖图。
///
/// 两种放法都要认，顺序也要和内核的 `extractChatCompletionsImageData` 一致：
/// 先看 `message.images[]`，没有再看 `content[]` 里 `type=image_url` 的块；
/// `message` 找不到就试 `delta`（有的上游把非流式响应也按增量格式回）。
fn extract_chat_images(resp: &Value) -> Vec<String> {
    let mut out = Vec::new();
    for choice in resp
        .get("choices")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        for key in ["message", "delta"] {
            let Some(container) = choice.get(key) else {
                continue;
            };
            let before = out.len();
            for item in container
                .get("images")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or(&[])
            {
                if let Some(u) = item
                    .pointer("/image_url/url")
                    .and_then(Value::as_str)
                    .or_else(|| item.get("url").and_then(Value::as_str))
                {
                    out.push(u.to_string());
                }
            }
            if out.len() == before {
                for item in container
                    .get("content")
                    .and_then(Value::as_array)
                    .map(Vec::as_slice)
                    .unwrap_or(&[])
                {
                    let is_image = item
                        .get("type")
                        .and_then(Value::as_str)
                        .is_some_and(|t| t.eq_ignore_ascii_case("image_url"));
                    if !is_image {
                        continue;
                    }
                    if let Some(u) = item.pointer("/image_url/url").and_then(Value::as_str) {
                        out.push(u.to_string());
                    }
                }
            }
            if out.len() > before {
                break;
            }
        }
    }
    out
}

/// `{data:[{b64_json|url}]}`。
fn extract_images_api(resp: &Value) -> Vec<String> {
    resp.get("data")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .filter_map(|d| {
            d.get("b64_json")
                .and_then(Value::as_str)
                .map(|b| format!("data:image/png;base64,{b}"))
                .or_else(|| d.get("url").and_then(Value::as_str).map(str::to_string))
        })
        .collect()
}

/// 把上游给的东西落成一个文件，返回绝对路径。
///
/// 三种形态都要接：data URL、裸 base64（有的上游 `url` 字段里直接放 base64）、
/// http(s) 链接。
fn save_image(
    cfg: &ImageConfig,
    raw: &str,
    explicit: Option<&str>,
    tool: &str,
    index: usize,
) -> Result<String, String> {
    let (bytes, ext) = decode_image_payload(raw)?;

    let path = match explicit {
        Some(p) => PathBuf::from(p),
        None => {
            let dir = out_dir(cfg);
            std::fs::create_dir_all(&dir)
                .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            dir.join(format!("{tool}-{stamp}-{index}.{ext}"))
        }
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    std::fs::write(&path, &bytes).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    Ok(path.display().to_string())
}

fn out_dir(cfg: &ImageConfig) -> PathBuf {
    if !cfg.out_dir.trim().is_empty() {
        return PathBuf::from(cfg.out_dir.trim());
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".ccload-client")
        .join("images")
}

/// data URL / 裸 base64 → (字节, 扩展名)。http 链接这里不下载 —— 返回错误让
/// 上层说清楚，比静默写一个 0 字节文件好。
fn decode_image_payload(raw: &str) -> Result<(Vec<u8>, &'static str), String> {
    if raw.starts_with("http://") || raw.starts_with("https://") {
        return Err(format!(
            "upstream returned a link instead of image data ({raw}); \
             ask it for base64 output (response_format=b64_json)"
        ));
    }
    let (payload, ext) = match raw.strip_prefix("data:") {
        Some(rest) => {
            let (meta, data) = rest
                .split_once(',')
                .ok_or("malformed data URL: no comma")?;
            (data, ext_of(meta))
        }
        None => (raw, "png"),
    };
    let bytes = base64::Engine::decode(
        &base64::engine::general_purpose::STANDARD,
        payload.trim(),
    )
    .map_err(|e| format!("cannot decode image data: {e}"))?;
    if bytes.is_empty() {
        return Err("upstream returned an empty image".into());
    }
    Ok((bytes, ext))
}

fn ext_of(meta: &str) -> &'static str {
    let m = meta.to_ascii_lowercase();
    if m.contains("jpeg") || m.contains("jpg") {
        "jpg"
    } else if m.contains("webp") {
        "webp"
    } else if m.contains("gif") {
        "gif"
    } else {
        "png"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 认不出来的 API 名一律当 chat —— 那是能力更全的一条（改图只有它能做），
    /// 猜错的代价比反过来小。
    #[test]
    fn unknown_api_falls_back_to_chat() {
        assert_eq!(ImageApi::parse("images"), ImageApi::Images);
        assert_eq!(ImageApi::parse("IMAGES"), ImageApi::Images);
        assert_eq!(ImageApi::parse("chat"), ImageApi::Chat);
        assert_eq!(ImageApi::parse(""), ImageApi::Chat);
        assert_eq!(ImageApi::parse("dall-e"), ImageApi::Chat);
    }

    /// 尺寸必须落在内核 `validChatImageGenerationSize` 认的取值里，
    /// 而且档位要大写 —— 内核就是这么转的，发别的上游直接 400。
    #[test]
    fn chat_size_stays_inside_what_the_kernel_accepts() {
        assert_eq!(chat_size(""), ("1:1".into(), "2K".into()));
        assert_eq!(chat_size("auto"), ("1:1".into(), "2K".into()));
        assert_eq!(chat_size("16:9@1k"), ("16:9".into(), "1K".into()));
        assert_eq!(chat_size("3:2@2K"), ("3:2".into(), "2K".into()));
        // 只给一半
        assert_eq!(chat_size("9:16"), ("9:16".into(), "2K".into()));
        assert_eq!(chat_size("1k"), ("1:1".into(), "1K".into()));
        // 非法的宽高比退回 1:1，而不是原样发出去
        assert_eq!(chat_size("4:3"), ("1:1".into(), "2K".into()));
        assert_eq!(chat_size("garbage"), ("1:1".into(), "2K".into()));
    }

    /// 像素尺寸同理：64–8192 之外的值退回默认。
    #[test]
    fn images_size_clamps_to_the_kernels_range() {
        assert_eq!(images_size(""), "1024x1024");
        assert_eq!(images_size("1536x1024"), "1536x1024");
        assert_eq!(images_size("64x64"), "64x64");
        assert_eq!(images_size("8192x8192"), "8192x8192");
        assert_eq!(images_size("32x32"), "1024x1024", "小于 64 要退回");
        assert_eq!(images_size("9000x9000"), "1024x1024", "大于 8192 要退回");
        assert_eq!(images_size("big"), "1024x1024");
    }

    /// 挖图的顺序必须和内核一致：先 `images[]`，它有了就不再看 `content[]`，
    /// 否则同一张图会被存两遍。
    #[test]
    fn chat_extraction_prefers_images_over_content() {
        let resp = json!({ "choices": [{ "message": {
            "images": [{ "image_url": { "url": "data:image/png;base64,AAA" }}],
            "content": [{ "type": "image_url", "image_url": { "url": "data:image/png;base64,BBB" }}],
        }}]});
        assert_eq!(extract_chat_images(&resp), vec!["data:image/png;base64,AAA"]);
    }

    /// 老一点的上游只有 `content[]`，那条兜底不能丢。
    #[test]
    fn chat_extraction_falls_back_to_content_blocks() {
        let resp = json!({ "choices": [{ "message": { "content": [
            { "type": "text", "text": "here you go" },
            { "type": "image_url", "image_url": { "url": "data:image/png;base64,CCC" }},
        ]}}]});
        assert_eq!(extract_chat_images(&resp), vec!["data:image/png;base64,CCC"]);
    }

    /// 有的上游把非流式响应也按增量格式回，`delta` 那条兜底同样不能丢。
    #[test]
    fn chat_extraction_reads_delta_when_message_is_absent() {
        let resp = json!({ "choices": [{ "delta": {
            "images": [{ "url": "data:image/png;base64,DDD" }],
        }}]});
        assert_eq!(extract_chat_images(&resp), vec!["data:image/png;base64,DDD"]);
    }

    /// 没有图就是没有图，不能凭空造一个空字符串出来 —— 上层要靠它报错。
    #[test]
    fn chat_extraction_returns_nothing_for_a_text_only_reply() {
        let resp = json!({ "choices": [{ "message": { "content": "sorry, I can't" }}]});
        assert!(extract_chat_images(&resp).is_empty());
    }

    #[test]
    fn images_api_extraction_handles_both_shapes() {
        let resp = json!({ "data": [
            { "b64_json": "EEE" },
            { "url": "data:image/webp;base64,FFF" },
        ]});
        assert_eq!(
            extract_images_api(&resp),
            vec!["data:image/png;base64,EEE", "data:image/webp;base64,FFF"]
        );
    }

    /// 扩展名跟着 MIME 走：存成 .png 的 webp 会让一部分工具打不开。
    #[test]
    fn extension_follows_the_mime_type() {
        let png = "data:image/png;base64,aGk=";
        assert_eq!(decode_image_payload(png).unwrap().1, "png");
        assert_eq!(decode_image_payload("data:image/jpeg;base64,aGk=").unwrap().1, "jpg");
        assert_eq!(decode_image_payload("data:image/webp;base64,aGk=").unwrap().1, "webp");
        // 裸 base64 没有 MIME 可依据，按 png 兜底
        assert_eq!(decode_image_payload("aGk=").unwrap().1, "png");
    }

    /// 上游回链接而不是数据时必须报错。写一个 0 字节文件再说「成功了」
    /// 是最坏的结果 —— 用户要到打开图片那一刻才发现。
    #[test]
    fn a_link_instead_of_data_is_an_error() {
        let err = decode_image_payload("https://example.com/a.png").unwrap_err();
        assert!(err.contains("link"), "{err}");
    }

    /// 空图也要报错，理由同上。
    #[test]
    fn empty_payload_is_an_error() {
        assert!(decode_image_payload("data:image/png;base64,").is_err());
    }

    /// 环境变量键名是和已装配置之间的契约：改一个字，已经装好的 CLI 就全变成
    /// 「读不到模型」。锁住它们。
    #[test]
    fn env_keys_are_a_contract() {
        assert_eq!(ENV_BASE_URL, "CCLOAD_IMAGE_BASE_URL");
        assert_eq!(ENV_TOKEN, "CCLOAD_IMAGE_TOKEN");
        assert_eq!(ENV_MODEL, "CCLOAD_IMAGE_MODEL");
        assert_eq!(ENV_API, "CCLOAD_IMAGE_API");
        assert_eq!(ENV_DIR, "CCLOAD_IMAGE_DIR");
        assert_eq!(MCP_NAME, "ccload-image");
    }

    /// 两个工具的名字也是契约：系统注入那段说明里逐字写着它们，
    /// 改名字而不改说明 = 教模型调一个不存在的工具。
    #[test]
    fn tool_names_match_what_the_injected_guidance_promises() {
        let specs = tool_specs();
        let names: Vec<&str> = specs
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["generate_image", "edit_image"]);
        // 每个工具都必须要求 prompt，否则模型会发一个空请求过去
        for t in specs.as_array().unwrap() {
            assert_eq!(t["inputSchema"]["required"][0], "prompt", "{}", t["name"]);
        }
    }

    // -----------------------------------------------------------------------
    // 安装与读回
    //
    // 生图的配置比视觉多两个键（`_API` / `_DIR`）。它们只在写盘那一刻存在，
    // 界面上的「走哪条路 / 存到哪」全靠读回来点亮 —— 少读回一个，用户就会
    // 看到自己明明选了 images 却显示 chat，然后再点一次、再被改回去。
    // -----------------------------------------------------------------------

    fn icfg(model: &str, api: ImageApi, out_dir: &str) -> ImageConfig {
        ImageConfig {
            base_url: "http://127.0.0.1:8990".into(),
            token: "tok".into(),
            model: model.into(),
            api,
            out_dir: out_dir.into(),
        }
    }

    /// 五家的 env 键名并不统一（OpenCode 是 `environment`，Grok 是 TOML 内联表），
    /// 五个键一个都不能在往返里掉。
    #[test]
    fn every_env_key_reads_back_on_every_target() {
        for target in cli_extensions::ALL_TARGETS {
            let dir = tempfile::tempdir().unwrap();
            let root = ConfigRoot::sandbox(dir.path().to_path_buf());
            let bk = BackupStore::new(dir.path().join("bk"));

            assert!(
                read_image_mcp(&root, target).unwrap().is_none(),
                "{target:?} 没装时应当是 None"
            );

            let cfg = icfg("qwen-image", ImageApi::Images, "/tmp/art");
            set_image_mcp(&root, target, true, &cfg, "s1", &bk).unwrap();

            let got = read_image_mcp(&root, target)
                .unwrap()
                .unwrap_or_else(|| panic!("{target:?} 装完却读不回来"));
            assert_eq!(got.base_url, "http://127.0.0.1:8990", "{target:?}");
            assert_eq!(got.token, "tok", "{target:?}");
            assert_eq!(got.model, "qwen-image", "{target:?}");
            assert_eq!(got.api, ImageApi::Images, "{target:?} 的 API 没读回来");
            assert_eq!(got.out_dir, "/tmp/art", "{target:?} 的出图目录没读回来");
        }
    }

    /// 改完再装必须是**改写**而不是叠加：留着上一次的 `images` 会让用户以为
    /// 切成 chat 了，然后 `edit_image` 继续报「这条路不能改图」。
    #[test]
    fn reinstalling_rewrites_the_api_and_dir() {
        for target in cli_extensions::ALL_TARGETS {
            let dir = tempfile::tempdir().unwrap();
            let root = ConfigRoot::sandbox(dir.path().to_path_buf());
            let bk = BackupStore::new(dir.path().join("bk"));

            set_image_mcp(
                &root,
                target,
                true,
                &icfg("m1", ImageApi::Images, "/tmp/old"),
                "s1",
                &bk,
            )
            .unwrap();
            set_image_mcp(
                &root,
                target,
                true,
                &icfg("m2", ImageApi::Chat, ""),
                "s2",
                &bk,
            )
            .unwrap();

            let got = read_image_mcp(&root, target).unwrap().unwrap();
            assert_eq!(got.model, "m2", "{target:?}");
            assert_eq!(got.api, ImageApi::Chat, "{target:?} 还留着上一次的 API");
            assert_eq!(got.out_dir, "", "{target:?} 还留着上一次的目录");
        }
    }

    /// 视觉和生图是两个独立的服务器，装一个不该动另一个 —— 它们在同一份
    /// 配置文件的同一张 `mcpServers` 表里，整块替换的写法会互相抹掉。
    #[test]
    fn installing_image_does_not_disturb_vision() {
        let dir = tempfile::tempdir().unwrap();
        let root = ConfigRoot::sandbox(dir.path().to_path_buf());
        let bk = BackupStore::new(dir.path().join("bk"));

        crate::services::vision_mcp::set_vision_mcp(
            &root,
            CliTarget::ClaudeCode,
            true,
            &crate::services::vision_mcp::VisionConfig {
                base_url: "http://127.0.0.1:8990".into(),
                token: "tok".into(),
                model: "qwen3-vl".into(),
            },
            "s1",
            &bk,
        )
        .unwrap();
        set_image_mcp(
            &root,
            CliTarget::ClaudeCode,
            true,
            &icfg("qwen-image", ImageApi::Chat, ""),
            "s2",
            &bk,
        )
        .unwrap();

        let vision = crate::services::vision_mcp::read_vision_mcp(&root, CliTarget::ClaudeCode)
            .unwrap()
            .expect("装生图不该把视觉挤掉");
        assert_eq!(vision.model, "qwen3-vl");

        // 反过来，卸生图也不该带走视觉。
        set_image_mcp(
            &root,
            CliTarget::ClaudeCode,
            false,
            &icfg("", ImageApi::Chat, ""),
            "s3",
            &bk,
        )
        .unwrap();
        assert!(read_image_mcp(&root, CliTarget::ClaudeCode).unwrap().is_none());
        assert!(
            crate::services::vision_mcp::read_vision_mcp(&root, CliTarget::ClaudeCode)
                .unwrap()
                .is_some(),
            "卸生图把视觉一起卸了"
        );
    }

    /// 装着旧内核的地址/令牌和「没装」是两回事：配置看着好，每次生图都 401。
    #[test]
    fn stale_credentials_are_flagged_not_hidden() {
        let dir = tempfile::tempdir().unwrap();
        let root = ConfigRoot::sandbox(dir.path().to_path_buf());
        let bk = BackupStore::new(dir.path().join("bk"));
        set_image_mcp(
            &root,
            CliTarget::ClaudeCode,
            true,
            &icfg("qwen-image", ImageApi::Images, ""),
            "s1",
            &bk,
        )
        .unwrap();
        let only = [CliTarget::ClaudeCode];

        let fresh = image_states(&root, &only, "http://127.0.0.1:8990", Some("tok"));
        assert!(fresh[0].installed);
        assert!(!fresh[0].stale, "地址和令牌都对，不该报过期");
        assert_eq!(fresh[0].model.as_deref(), Some("qwen-image"));
        assert_eq!(fresh[0].api.as_deref(), Some("images"), "界面靠它回显走哪条路");

        // 令牌换了（内核重启后重新签发）→ 装着，但打不通。
        let stale = image_states(&root, &only, "http://127.0.0.1:8990", Some("another"));
        assert!(stale[0].installed);
        assert!(stale[0].stale);

        // 端口换了同理；尾斜杠不算差异。
        assert!(image_states(&root, &only, "http://127.0.0.1:9999", Some("tok"))[0].stale);
        assert!(
            !image_states(&root, &only, "http://127.0.0.1:8990/", Some("tok"))[0].stale,
            "只差一个尾斜杠不该被判成过期"
        );
    }

    /// 一家的配置坏了不该让另外四家的状态一起消失 —— 这一格只是用来点亮界面
    /// 的，整次失败等于用户什么都看不到。
    #[test]
    fn one_broken_config_does_not_sink_the_others() {
        let dir = tempfile::tempdir().unwrap();
        let root = ConfigRoot::sandbox(dir.path().to_path_buf());
        let bk = BackupStore::new(dir.path().join("bk"));
        set_image_mcp(
            &root,
            CliTarget::ClaudeCode,
            true,
            &icfg("qwen-image", ImageApi::Chat, ""),
            "s1",
            &bk,
        )
        .unwrap();
        // Codex 的 config.toml 被手工编辑坏了。
        let codex = root.join(".codex/config.toml");
        std::fs::create_dir_all(codex.parent().unwrap()).unwrap();
        std::fs::write(&codex, "this = = not toml").unwrap();

        let states = image_states(
            &root,
            &[CliTarget::ClaudeCode, CliTarget::Codex],
            "http://127.0.0.1:8990",
            Some("tok"),
        );
        assert!(states[0].installed, "好的那家照常显示");
        assert_eq!(states[0].model.as_deref(), Some("qwen-image"));
        assert!(!states[1].installed, "坏的那家算没装，而不是整次报错");
    }

    /// 没装过就点「移除」是常见操作，不该弹一个「找不到」的红字。
    #[test]
    fn disabling_a_never_installed_target_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let root = ConfigRoot::sandbox(dir.path().to_path_buf());
        let bk = BackupStore::new(dir.path().join("bk"));
        let written = set_image_mcp(
            &root,
            CliTarget::Codex,
            false,
            &icfg("", ImageApi::Chat, ""),
            "s1",
            &bk,
        )
        .unwrap();
        assert!(written.is_empty());
    }
}
