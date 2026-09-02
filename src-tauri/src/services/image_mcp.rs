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
    /// 按模型挑，挑错了当场换另一条重试。**默认**，也是唯一一个不需要用户
    /// 懂端点差异的选项。
    Auto,
    /// `/v1/chat/completions` + `modalities:["image"]`。**能改图**。
    Chat,
    /// `/v1/images/generations`。只能生成。
    Images,
}

impl ImageApi {
    fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Chat => "chat",
            Self::Images => "images",
        }
    }

    /// 认不出来一律当 auto。
    ///
    /// 以前这里默认 chat，代价是：装的时候选错一次，之后每次生图都失败，而错误
    /// 信息说的是上游的话（「这个模型不在这个端点上」），没人会想到要回客户端改
    /// 一个下拉框。让它自己试才是对的。
    fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "images" => Self::Images,
            "chat" => Self::Chat,
            _ => Self::Auto,
        }
    }

    /// 报错里写给人看的端点名。`Auto` 到不了这里（`plan()` 只吐具体的两条），
    /// 但补一个兜底比 `unreachable!()` 好 —— 生图失败不值得 panic 掉整个 MCP。
    fn endpoint_label(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Chat => "/v1/chat/completions",
            Self::Images => "/v1/images/generations",
        }
    }

    /// 这一次调用按什么顺序试。
    ///
    /// `editing` 时只可能是 chat —— images 端点的 JSON 里根本没有放输入图的位置，
    /// 排上去也是白跑一趟 400。
    fn plan(self, model: &str, editing: bool) -> &'static [ImageApi] {
        match self {
            Self::Chat => &[Self::Chat],
            Self::Images => &[Self::Images],
            Self::Auto if editing => &[Self::Chat],
            Self::Auto if images_only_model(model) => &[Self::Images, Self::Chat],
            Self::Auto => &[Self::Chat, Self::Images],
        }
    }
}

/// 这个模型是不是「只认 images 端点」的那一类。
///
/// 不是拍脑袋分的，是这两家上游会把 chat 请求直接顶回来：
///
/// * xAI 的 `grok-imagine-*` —— 原话是 “is an image model and is therefore not
///   available on this endpoint. Please use ... /v1/images/generations”。
/// * OpenAI 的 `gpt-image-*` / `dall-e-*` —— 从来就没上过 chat completions；
///   内核自己也把它们钉在 images 端点上（`admin_testing_image.go:742`
///   `canonicalCodexImageModel`）。
///
/// 反过来 Gemini 那一挂（`*-image-preview`、nano banana）只有 chat 这一条路，
/// 所以剩下的全部先试 chat —— 顺带保住改图能力。
fn images_only_model(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    m.contains("imagine") || m.contains("gpt-image") || m.contains("dall-e") || m.contains("dalle")
}

/// images 端点上的 body 要写成哪一家的形状。
///
/// 这一条是整个模块最容易想当然的地方：MCP 打的是内核的**代理**口，而 images
/// 请求族在 `protocol/types.go:79` 那张转换表里**一条登记都没有** —— 内核不会
/// 替我们改写 body，原样转给上游。所以形状必须在这里就写对，照抄内核自己发请求
/// 时的 `admin_testing_image.go:719 imageGenerationRequestBody`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImageFamily {
    /// xAI：`aspect_ratio` + `resolution`，没有 `size`，也没有 `n`。
    Xai,
    /// 标准 OpenAI：`size` 写像素。
    OpenAi,
}

impl ImageFamily {
    fn of(model: &str) -> Self {
        let m = model.to_ascii_lowercase();
        if m.contains("grok") || m.contains("imagine") {
            Self::Xai
        } else {
            Self::OpenAi
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
    /// 图往哪写。空 = 后端默认目录。
    ///
    /// 要读回来是因为重装会整条重写 env —— 不知道原值就只能写默认目录，
    /// 用户自己设的那个目录会被悄悄换掉，而他是从别的入口（比如「改成自动」）
    /// 触发的重装，根本没想过会动到这一项。
    pub out_dir: Option<String>,
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
                    out_dir: Some(cfg.out_dir).filter(|d| !d.is_empty()),
                    stale: !(base_ok && token_ok),
                }
            }
            _ => ImageTargetState {
                target,
                label: target.label(),
                installed: false,
                model: None,
                api: None,
                out_dir: None,
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
                            "Either an aspect ratio — one of 1:1 16:9 9:16 3:2 2:3, optionally \
                             with @1k or @2k — or pixel dimensions like 1024x1536. Both forms \
                             work whichever model is configured; they are converted to whatever \
                             that model's endpoint accepts. Omit for 1:1 at the default tier.",
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

    // 改图必须走 chat：images API 的 JSON body 没有放输入图的位置。auto 会自己
    // 避开（`plan()` 在 editing 时只排 chat），只有用户手动钉死 images 才会撞上
    // 这里 —— 与其发出去让上游回一句语焉不详的 400，不如直接说该改哪。
    if !inputs.is_empty() && cfg.api == ImageApi::Images {
        return Err(
            "edit_image needs the chat API, but this server is pinned to \
             CCLOAD_IMAGE_API=images (the Images API has no slot for an input image). \
             In the ccLoad client, set \"Which endpoint\" back to Auto."
                .into(),
        );
    }

    let size = params.get("size").and_then(Value::as_str).unwrap_or("");

    // 按模型排好的顺序逐条试。只有「端点选错了」这一类错误才继续下一条：余额、
    // 限流、提示词被拒换个端点也是一样的下场，白花一次钱。
    let plan = cfg.api.plan(&cfg.model, !inputs.is_empty());
    let mut images = Vec::new();
    let mut failures: Vec<ApiErr> = Vec::new();
    for (i, api) in plan.iter().enumerate() {
        let attempt = match api {
            ImageApi::Images => request_images(&cfg, prompt, size).await,
            // Auto 不会进到这里（plan 里只排具体端点），chat 兜底。
            _ => request_chat(&cfg, prompt, &inputs, size).await,
        };
        match attempt {
            Ok(got) if !got.is_empty() => {
                images = got;
                break;
            }
            // 请求本身成功但一张图都没有：换端点也解决不了，当场停。
            Ok(_) => {
                failures.push(ApiErr {
                    status: 200,
                    message: "the model returned no image".into(),
                    api: *api,
                });
                break;
            }
            Err(e) => {
                let last = i + 1 == plan.len();
                let retryable = e.wrong_surface();
                failures.push(e);
                if last || !retryable {
                    break;
                }
            }
        }
    }
    if images.is_empty() {
        return Err(render_failures(&cfg, &failures));
    }

    let explicit = params
        .get("out_path")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let mut lines = Vec::new();
    for (i, raw) in images.iter().enumerate() {
        let payload = materialize(raw.clone()).await?;
        let path = save_image(&cfg, &payload, explicit.filter(|_| i == 0), name, i)?;
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

/// 那五个宽高比里的一个，或者不是。
fn valid_aspect(s: &str) -> Option<&'static str> {
    match s {
        "1:1" => Some("1:1"),
        "16:9" => Some("16:9"),
        "9:16" => Some("9:16"),
        "3:2" => Some("3:2"),
        "2:3" => Some("2:3"),
        _ => None,
    }
}

/// `1024x1536` → 那五个宽高比里最接近的一个。
///
/// 不能只认死几个常见值：`size` 在工具描述里只有一个参数，模型按 OpenAI 的习惯
/// 写像素是完全合理的，而 auto 可能把这次请求送去 chat 端点 —— 那边只认宽高比。
/// 认不出来就当 1:1 的话，用户要的横图会变成方图，而且没有任何提示。
fn aspect_of_pixels(raw: &str) -> Option<&'static str> {
    let (w, h) = raw.split_once('x')?;
    let w: f64 = w.trim().parse().ok()?;
    let h: f64 = h.trim().parse().ok()?;
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let ratio = w / h;
    const TABLE: [(&str, f64); 5] = [
        ("1:1", 1.0),
        ("16:9", 16.0 / 9.0),
        ("9:16", 9.0 / 16.0),
        ("3:2", 1.5),
        ("2:3", 2.0 / 3.0),
    ];
    TABLE
        .iter()
        .min_by(|a, b| (ratio - a.1).abs().total_cmp(&(ratio - b.1).abs()))
        .map(|(name, _)| *name)
}

/// `1:1@2k` / `16:9` / `2k` / `1024x1536` → `(aspect_ratio, image_size)`。
///
/// 取值范围抄自内核的 `validChatImageGenerationSize`：宽高比只认那五个，档位只认
/// 1k/2k，而且内核会把档位转成大写。发一个它不认的值，上游直接 400。
fn chat_size(raw: &str) -> (String, String) {
    let s = raw.trim().to_ascii_lowercase();
    if s.is_empty() || s == "auto" {
        return ("1:1".into(), "2K".into());
    }
    let (head, tier) = match s.split_once('@') {
        Some((a, t)) => (a.to_string(), t.to_string()),
        // 只给了一半：像 `16:9`、`1024x1536` 是宽高比那一半，像 `2k` 是档位那一半。
        None if s.contains(':') || s.contains('x') => (s.clone(), "2k".to_string()),
        None => ("1:1".to_string(), s.clone()),
    };
    let aspect = valid_aspect(&head)
        .or_else(|| aspect_of_pixels(&head))
        .unwrap_or("1:1");
    let tier = if tier == "1k" { "1K" } else { "2K" };
    (aspect.into(), tier.into())
}

/// 像素尺寸。内核的 `validImageGenerationSize` 要求 64–8192，非法就退回默认，
/// 不要把一个注定 400 的值发出去。
///
/// 也认宽高比写法（`16:9`、`16:9@2k`）—— `size` 这个参数在工具描述里只有一个，
/// 而 auto 会替模型挑端点，所以模型没法知道该用哪种写法。宽高比换成像素时必须
/// 挑一个**这个模型收得下**的值：gpt-image 只认 1024x1024 / 1536x1024 /
/// 1024x1536，dall-e 3 只认 1024x1024 / 1792x1024 / 1024x1792，给别的直接 400。
/// 所以这里不做等比换算，只判断横竖。档位（@1k/@2k）在这条路上没有对应字段，丢掉。
fn images_size(model: &str, raw: &str) -> String {
    let s = raw.trim().to_ascii_lowercase();
    if s.is_empty() || s == "auto" {
        return "1024x1024".into();
    }
    // 用户显式给了像素就照发，别替他改。
    if let Some((w, h)) = s.split_once('x') {
        if let (Ok(w), Ok(h)) = (w.parse::<u32>(), h.parse::<u32>()) {
            if (64..=8192).contains(&w) && (64..=8192).contains(&h) {
                return format!("{w}x{h}");
            }
        }
    }
    let aspect = s.split_once('@').map(|(a, _)| a).unwrap_or(s.as_str());
    let dalle = {
        let m = model.to_ascii_lowercase();
        m.contains("dall-e") || m.contains("dalle")
    };
    match aspect {
        "16:9" | "3:2" => if dalle { "1792x1024" } else { "1536x1024" },
        "9:16" | "2:3" => if dalle { "1024x1792" } else { "1024x1536" },
        _ => "1024x1024",
    }
    .into()
}

/// xAI 的 `aspect_ratio`。三种写法都要认：我们自己的 `16:9@2k`、光一个宽高比、
/// 以及用户按 OpenAI 习惯写的像素。取值范围同内核 `xaiImageAspectRatio`。
fn xai_aspect(raw: &str) -> String {
    let s = raw.trim().to_ascii_lowercase();
    let head = s.split_once('@').map(|(a, _)| a).unwrap_or(s.as_str());
    valid_aspect(head)
        .or_else(|| aspect_of_pixels(head))
        .unwrap_or("1:1")
        .into()
}

/// xAI 的 `resolution`（`1k` / `2k`）。同样抄 `xaiImageResolution`：注意它和
/// chat 那条不一样，这里是**小写**。
fn xai_resolution(raw: &str) -> String {
    let s = raw.trim().to_ascii_lowercase();
    if let Some((_, tier)) = s.split_once('@') {
        return tier.to_string();
    }
    if s.contains("2048") || s == "2k" {
        return "2k".into();
    }
    "1k".into()
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
) -> Result<Vec<String>, ApiErr> {
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

/// `/v1/images/generations` 的请求体。形状按模型所属的那一家写 —— 内核对 images
/// 请求族没有注册任何跨协议转换（`protocol/types.go` 的
/// `supportedTransformFamiliesByClientAndUpstream` 里一条都没有），也就是说我们
/// 发什么上游就原样收到什么，形状错了没人会替我们纠。
fn images_body(model: &str, prompt: &str, size: &str) -> Value {
    match ImageFamily::of(model) {
        // 抄自内核的 `xaiImageGenerationRequestBody`：xAI 不认 `size`，尺寸拆成
        // 宽高比 + 档位两个字段；`n` 也不要。
        ImageFamily::Xai => json!({
            "model": model,
            "prompt": prompt,
            "response_format": "b64_json",
            "aspect_ratio": xai_aspect(size),
            "resolution": xai_resolution(size),
        }),
        ImageFamily::OpenAi => {
            let mut body = json!({
                "model": model,
                "prompt": prompt,
                "size": images_size(model, size),
                "n": 1,
            });
            // dall-e 默认回的是**链接**，不显式要 base64 就得多跑一趟下载；
            // gpt-image 系列正好相反 —— 它们只回 base64，多送一个
            // `response_format` 会被顶回来（“Unknown parameter”）。
            let m = model.to_ascii_lowercase();
            if m.contains("dall-e") || m.contains("dalle") {
                body["response_format"] = Value::from("b64_json");
            }
            body
        }
    }
}

/// `/v1/images/generations`。
async fn request_images(
    cfg: &ImageConfig,
    prompt: &str,
    size: &str,
) -> Result<Vec<String>, ApiErr> {
    let body = images_body(&cfg.model, prompt, size);
    let resp = post(cfg, "/v1/images/generations", &body).await?;
    Ok(extract_images_api(&resp))
}

/// 上游回链接的时候把图下下来，换成 data URL 交给 `save_image`。
///
/// 不是所有上游都听 `response_format` —— 网关、代理、老一点的实现都可能塞一个
/// 短期有效的 URL 回来。我们要落盘的是文件，链接放着不管等于生图失败，而且失败
/// 得莫名其妙（「明明画出来了」）。
async fn materialize(raw: String) -> Result<String, String> {
    if !raw.starts_with("http://") && !raw.starts_with("https://") {
        return Ok(raw);
    }
    // 这里**不**关代理：图在上游的 CDN 上，不像内核那样一定在 127.0.0.1。
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .map_err(|e| format!("build http client: {e}"))?;
    let resp = client
        .get(&raw)
        .send()
        .await
        .map_err(|e| format!("cannot download the generated image ({raw}): {e}"))?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!(
            "cannot download the generated image ({raw}): HTTP {status}"
        ));
    }
    let mime = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("image/png")
        .to_string();
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("cannot read the generated image ({raw}): {e}"))?;
    Ok(format!("data:{mime};base64,{}", b64(&bytes)))
}

/// 一次上游尝试的失败。留着 status 是为了判断「是不是端点选错了」——
/// 光有一句人话读不出这个。
struct ApiErr {
    status: u16,
    message: String,
    /// 这一条是打哪个端点打出来的，进最终错误信息时要说清楚。
    api: ImageApi,
}

impl ApiErr {
    /// 这条错误是不是在说「你打错端点了」。是的话换另一条还有救，不是的话
    /// （余额、限流、提示词被拒）换了也一样，别浪费一次调用和一次计费。
    fn wrong_surface(&self) -> bool {
        let m = self.message.to_ascii_lowercase();
        // 内核在请求发出去之前自己拒的：这条渠道没有 URL 声明 openai 协议。
        // 换 chat 端点是有意义的 —— chat 请求族有跨协议转换，走得通。
        if self.status == 404 && m.contains("upstream endpoint unsupported") {
            return true;
        }
        if !(self.status == 400 || self.status == 404 || self.status == 405) {
            return false;
        }
        m.contains("not available on this endpoint")
            || m.contains("images/generations")
            || m.contains("is an image model")
            || m.contains("unsupported endpoint")
            || m.contains("no such endpoint")
            || m.contains("does not support")
    }
}

/// 把几次失败的尝试拼成一句人话。
///
/// 这条错误是在 CLI 里被读的 —— 用户看不到客户端的日志，也看不到内核的日志。
/// 所以「打了哪个端点」「上游怎么说的」「下一步该动哪里」三样都得写进去，
/// 否则读到的只有一个 404，谁也不知道是模型不对、渠道不对还是钱不够。
fn render_failures(cfg: &ImageConfig, failures: &[ApiErr]) -> String {
    let mut out = format!("image generation failed for model `{}`", cfg.model);
    for f in failures {
        out.push_str(&format!(
            "\n  · via {} endpoint: {}",
            f.api.endpoint_label(),
            f.message
        ));
    }
    // 内核在请求发出去之前自己拒的。这个 404 长得像上游返回的，其实不是 ——
    // 不点破的话用户会去查上游账号，而要改的是渠道那一行。
    if failures
        .iter()
        .any(|f| f.status == 404 && f.message.to_ascii_lowercase().contains("upstream endpoint unsupported"))
    {
        out.push_str(
            "\nhint: the kernel refused to route /v1/images/generations — the channel serving \
             this model has no URL declaring the `openai` protocol, and the images request \
             family has no cross-protocol conversion. Add an openai-protocol URL to that \
             channel, or pick a model whose channel has one.",
        );
    }
    out
}

async fn post(cfg: &ImageConfig, path: &str, body: &Value) -> Result<Value, ApiErr> {
    let api = if path.contains("/images/") {
        ImageApi::Images
    } else {
        ImageApi::Chat
    };
    let fail = |status: u16, message: String| ApiErr {
        status,
        message,
        api,
    };
    let endpoint = format!("{}{path}", cfg.base_url.trim_end_matches('/'));
    let resp = http()
        .map_err(|e| fail(0, e))?
        .post(&endpoint)
        .header("authorization", format!("Bearer {}", cfg.token))
        .header("x-api-key", &cfg.token)
        .json(body)
        .send()
        .await
        .map_err(|e| fail(0, format!("request to kernel failed: {e}")))?;
    let status = resp.status();
    let parsed: Value = resp
        .json()
        .await
        .map_err(|e| fail(status.as_u16(), format!("bad kernel response: {e}")))?;
    if !status.is_success() {
        let msg = parsed
            .pointer("/error/message")
            .and_then(Value::as_str)
            .or_else(|| parsed.get("error").and_then(Value::as_str))
            .unwrap_or("unknown error");
        return Err(fail(status.as_u16(), format!("HTTP {status}: {msg}")));
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
    // 认出来的以文件头为准，认不出来才退回声明的 MIME。
    let ext = sniff_ext(&bytes).unwrap_or(ext);
    Ok((bytes, ext))
}

/// 按文件头认格式。
///
/// 不能信 MIME：images 端点回的是**裸 base64**，`extract_images_api` 只能给它安
/// 一个 `data:image/png` 的壳，而 xAI 那边回的其实是 JPEG。存成 .png 的 JPEG 看图
/// 工具大多能打开，按扩展名分发的那些（打包器、上传接口、素材流水线）会当场报错，
/// 而错误信息指向的是文件本身，没人会想到是生图那一步给错了名字。
fn sniff_ext(bytes: &[u8]) -> Option<&'static str> {
    match bytes {
        [0xFF, 0xD8, 0xFF, ..] => Some("jpg"),
        [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, ..] => Some("png"),
        [b'G', b'I', b'F', b'8', ..] => Some("gif"),
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => Some("webp"),
        _ => None,
    }
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

    /// 认不出来的 API 名一律当 auto —— 让它自己按模型试，比钉死在一条上强。
    #[test]
    fn unknown_api_falls_back_to_auto() {
        assert_eq!(ImageApi::parse("images"), ImageApi::Images);
        assert_eq!(ImageApi::parse("IMAGES"), ImageApi::Images);
        assert_eq!(ImageApi::parse("chat"), ImageApi::Chat);
        assert_eq!(ImageApi::parse("auto"), ImageApi::Auto);
        // 老版本装进去的配置里这一项可能压根没有，或者写的是别的东西。
        assert_eq!(ImageApi::parse(""), ImageApi::Auto);
        assert_eq!(ImageApi::parse("dall-e"), ImageApi::Auto);
    }

    /// 用户钉死了就一条都不多试 —— 自动换端点会换掉一次计费，钉死的意思
    /// 就是「别自作主张」。
    #[test]
    fn pinned_api_never_falls_back() {
        assert_eq!(ImageApi::Chat.plan("grok-imagine-image", false), &[ImageApi::Chat]);
        assert_eq!(
            ImageApi::Images.plan("gemini-3-pro-image-preview", false),
            &[ImageApi::Images]
        );
    }

    /// auto 的顺序：先试那条更可能成的，另一条兜底。
    #[test]
    fn auto_orders_endpoints_by_model() {
        // 只认 images 端点的两家：images 优先，chat 兜底（万一上游后来支持了）
        assert_eq!(
            ImageApi::Auto.plan("grok-imagine-image-2.0", false),
            &[ImageApi::Images, ImageApi::Chat]
        );
        assert_eq!(
            ImageApi::Auto.plan("gpt-image-1.5", false),
            &[ImageApi::Images, ImageApi::Chat]
        );
        // 其余先 chat：Gemini 那一挂只有 chat 这一条路
        assert_eq!(
            ImageApi::Auto.plan("gemini-3-pro-image-preview", false),
            &[ImageApi::Chat, ImageApi::Images]
        );
        // 改图只可能是 chat —— images 端点的 JSON 里没有放输入图的位置
        assert_eq!(ImageApi::Auto.plan("gpt-image-1.5", true), &[ImageApi::Chat]);
    }

    /// 「端点选错了」和「余额不够」必须分得开：前者换一条还有救，后者换了
    /// 也一样，白烧一次调用。
    #[test]
    fn only_wrong_endpoint_errors_are_retried() {
        let err = |status: u16, message: &str| ApiErr {
            status,
            message: message.into(),
            api: ImageApi::Chat,
        };
        // 内核在发出去之前自己拒的
        assert!(err(404, "HTTP 404: upstream endpoint unsupported").wrong_surface());
        // xAI 的原话
        assert!(err(
            400,
            "HTTP 400: grok-imagine-image is an image model and is therefore not available on this endpoint",
        )
        .wrong_surface());
        // 换端点救不了的
        assert!(!err(401, "HTTP 401: invalid api key").wrong_surface());
        assert!(!err(429, "HTTP 429: rate limit exceeded").wrong_surface());
        assert!(!err(400, "HTTP 400: your prompt was rejected").wrong_surface());
        assert!(!err(500, "HTTP 500: internal error").wrong_surface());
    }

    /// xAI 的 images 端点不认 `size`，认的是 `aspect_ratio` + `resolution`
    /// （内核 `xaiImageAspectRatio` / `xaiImageResolution` 就是这么转的）。
    #[test]
    fn xai_size_splits_into_aspect_and_resolution() {
        assert_eq!(xai_aspect("16:9@1k"), "16:9");
        assert_eq!(xai_resolution("16:9@1k"), "1k");
        assert_eq!(xai_aspect("1792x1024"), "16:9");
        assert_eq!(xai_aspect("1024x1536"), "2:3");
        assert_eq!(xai_aspect("garbage"), "1:1");
        assert_eq!(xai_resolution(""), "1k");
        assert_eq!(xai_resolution("2048x2048"), "2k");
    }

    /// 模型名决定 images 端点发什么形状的 body —— 内核对 images 请求族没有
    /// 跨协议转换，我们发什么上游就收到什么。
    #[test]
    fn image_family_is_picked_by_model_name() {
        assert!(matches!(ImageFamily::of("grok-imagine-image"), ImageFamily::Xai));
        assert!(matches!(ImageFamily::of("Grok-2-Image"), ImageFamily::Xai));
        assert!(matches!(ImageFamily::of("gpt-image-1.5"), ImageFamily::OpenAi));
        assert!(matches!(ImageFamily::of("dall-e-3"), ImageFamily::OpenAi));
    }

    /// xAI 的 body 和 OpenAI 的完全不是一回事，而且 `response_format` 只能给
    /// 认它的那些模型 —— gpt-image 系列收到会回「Unknown parameter」。
    #[test]
    fn images_body_is_shaped_per_family() {
        let xai = images_body("grok-imagine-image", "a cat", "16:9@2k");
        assert_eq!(xai["aspect_ratio"], "16:9");
        assert_eq!(xai["resolution"], "2k");
        assert_eq!(xai["response_format"], "b64_json");
        assert!(xai.get("size").is_none(), "xAI 不认 size");
        assert!(xai.get("n").is_none(), "xAI 不认 n");

        let gpt = images_body("gpt-image-1.5", "a cat", "");
        assert_eq!(gpt["size"], "1024x1024");
        assert_eq!(gpt["n"], 1);
        assert!(
            gpt.get("response_format").is_none(),
            "gpt-image 只回 base64，多送这个字段会被顶回来"
        );

        // dall-e 反过来：不显式要 base64 就回一个链接。
        let dalle = images_body("dall-e-3", "a cat", "1024x1792");
        assert_eq!(dalle["size"], "1024x1792");
        assert_eq!(dalle["response_format"], "b64_json");
    }

    /// 内核拒路由时那个 404 长得像上游返回的，其实不是。不点破的话用户会去
    /// 查上游账号，而要改的是渠道那一行。
    #[test]
    fn kernel_routing_refusal_gets_an_actionable_hint() {
        let cfg = ImageConfig {
            base_url: "https://example.test".into(),
            token: "t".into(),
            model: "grok-imagine-image".into(),
            api: ImageApi::Auto,
            out_dir: String::new(),
        };
        let msg = render_failures(
            &cfg,
            &[ApiErr {
                status: 404,
                message: "HTTP 404 Not Found: upstream endpoint unsupported".into(),
                api: ImageApi::Images,
            }],
        );
        assert!(msg.contains("grok-imagine-image"));
        assert!(msg.contains("/v1/images/generations"));
        assert!(msg.contains("openai"));
    }

    /// 反过来也要成立：模型按 OpenAI 的习惯写像素，chat 端点只认宽高比 ——
    /// 认不出来就当 1:1 的话，要的横图会变成方图，还没有任何提示。
    #[test]
    fn pixels_are_understood_on_the_chat_endpoint_too() {
        assert_eq!(chat_size("1792x1024"), ("16:9".into(), "2K".into()));
        assert_eq!(chat_size("1024x1792"), ("9:16".into(), "2K".into()));
        assert_eq!(chat_size("1536x1024"), ("3:2".into(), "2K".into()));
        assert_eq!(chat_size("1024x1536"), ("2:3".into(), "2K".into()));
        assert_eq!(chat_size("1024x1024"), ("1:1".into(), "2K".into()));
        // 不在表上的比例挑最接近的，而不是一律退回 1:1
        assert_eq!(chat_size("1920x1080@1k"), ("16:9".into(), "1K".into()));
        assert_eq!(chat_size("800x600"), ("3:2".into(), "2K".into()), "4:3 最近的是 3:2");
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
        assert_eq!(images_size("gpt-image-1.5", ""), "1024x1024");
        assert_eq!(images_size("gpt-image-1.5", "1536x1024"), "1536x1024");
        assert_eq!(images_size("gpt-image-1.5", "64x64"), "64x64");
        assert_eq!(images_size("gpt-image-1.5", "8192x8192"), "8192x8192");
        assert_eq!(images_size("gpt-image-1.5", "32x32"), "1024x1024", "小于 64 要退回");
        assert_eq!(images_size("gpt-image-1.5", "9000x9000"), "1024x1024", "大于 8192 要退回");
        assert_eq!(images_size("gpt-image-1.5", "big"), "1024x1024");
    }

    /// 宽高比写法在 images 端点上也得认 —— `size` 在工具描述里只有一个参数，
    /// auto 替模型挑端点，模型没法知道该写哪种。挑的值必须是这个模型收得下的：
    /// 等比算一个「数学上对」的尺寸发出去只会 400。
    #[test]
    fn an_aspect_ratio_becomes_pixels_the_model_accepts() {
        assert_eq!(images_size("gpt-image-1.5", "16:9"), "1536x1024");
        assert_eq!(images_size("gpt-image-1.5", "16:9@2k"), "1536x1024");
        assert_eq!(images_size("gpt-image-1.5", "2:3"), "1024x1536");
        assert_eq!(images_size("gpt-image-1.5", "1:1"), "1024x1024");
        // dall-e 3 的横竖两个值和 gpt-image 不一样
        assert_eq!(images_size("dall-e-3", "16:9"), "1792x1024");
        assert_eq!(images_size("dall-e-3", "9:16"), "1024x1792");
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

    /// 文件头和 MIME 打架时以文件头为准。
    ///
    /// 这不是假设出来的：xAI 的 images 端点回裸 base64，`extract_images_api` 给它
    /// 安的壳写死是 `image/png`，实际字节是 JPEG（`ff d8 ff e0`）。
    #[test]
    fn the_magic_bytes_win_over_a_wrong_mime() {
        let jpeg = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            [0xFFu8, 0xD8, 0xFF, 0xE0, 0x00, 0x10].as_slice(),
        );
        // 走 images 端点时套的就是这个壳
        assert_eq!(
            decode_image_payload(&format!("data:image/png;base64,{jpeg}")).unwrap().1,
            "jpg",
        );
        // 裸 base64 同样认得出来，不再一律当 png
        assert_eq!(decode_image_payload(&jpeg).unwrap().1, "jpg");

        let png = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            [0x89u8, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A].as_slice(),
        );
        assert_eq!(decode_image_payload(&png).unwrap().1, "png");
    }

    /// 文件头认不出来时才轮到 MIME —— 那是兜底，不是主依据。
    #[test]
    fn extension_follows_the_mime_type() {
        // `aGk=` 是 "hi"，没有任何图片文件头，所以走的都是兜底那条
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

    /// 界面要按磁盘上的真值回显，`out_dir` 也一样 —— 读不回来的话，为了改别的
    /// 项按一次安装就会把用户自己设的目录换成默认目录，而且什么都不说。
    #[test]
    fn the_state_reads_back_the_configured_directory() {
        let dir = tempfile::tempdir().unwrap();
        let root = ConfigRoot::sandbox(dir.path().to_path_buf());
        let bk = BackupStore::new(dir.path().join("bk"));
        let target = CliTarget::ClaudeCode;

        set_image_mcp(
            &root,
            target,
            true,
            &icfg("m1", ImageApi::Auto, "/tmp/pics"),
            "s1",
            &bk,
        )
        .unwrap();
        let st = image_states(&root, &[target], "http://127.0.0.1:1", None);
        assert_eq!(st[0].out_dir.as_deref(), Some("/tmp/pics"));
        assert_eq!(st[0].api.as_deref(), Some("auto"));

        // 留空是「用默认目录」，回显成 None 而不是空字符串 —— 前端拿它当
        // 「没设过」来判断。
        set_image_mcp(&root, target, true, &icfg("m1", ImageApi::Auto, ""), "s2", &bk).unwrap();
        assert_eq!(image_states(&root, &[target], "http://127.0.0.1:1", None)[0].out_dir, None);
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
