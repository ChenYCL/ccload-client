//! Vision augmentation for non-multimodal models.
//!
//! The kernel routes text fine, but a model like deepseek-r1 cannot read an
//! image the user pastes into the CLI. Instead of forcing a multimodal model
//! everywhere, this ships a tiny stdio MCP server *inside the client binary*
//! (`ccload-client vision-mcp`) exposing one tool, `describe_image`: it reads
//! the image, sends it to a vision-capable model through the kernel, and
//! returns the description as text. The host model then works with that text.
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

use serde_json::{json, Value};

use crate::error::AppError;
use crate::services::cli_backup::BackupStore;
use crate::services::cli_extensions::{
    self, ExtensionKind, ExtensionSpec, McpTransport,
};
use crate::services::cli_types::{CliTarget, ConfigRoot};

pub const MCP_NAME: &str = "ccload-vision";

const ENV_BASE_URL: &str = "CCLOAD_VISION_BASE_URL";
const ENV_TOKEN: &str = "CCLOAD_VISION_TOKEN";
const ENV_MODEL: &str = "CCLOAD_VISION_MODEL";

pub struct VisionConfig {
    pub base_url: String,
    pub token: String,
    pub model: String,
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
            "tools/list" => Ok(json!({
                "tools": [{
                    "name": "describe_image",
                    "description":
                        "Describe an image file with a vision-capable model. \
                         Use this whenever the user pastes/mentions an image \
                         (screenshot, photo, chart) and you cannot see images.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "path": {
                                "type": "string",
                                "description": "Absolute path to the image file",
                            },
                            "url": {
                                "type": "string",
                                "description": "Remote URL of the image",
                            },
                            "prompt": {
                                "type": "string",
                                "description": "What to look for; default is a detailed description",
                            },
                        },
                    },
                    "required": [],
                }],
            })),
            "tools/call" => {
                // MCP nests the tool payload under `arguments`.
                let args = params.get("arguments").unwrap_or(&params);
                runtime.block_on(call_describe_image(args))
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
        use std::io::Write;
        if out.write_all(line.as_bytes()).is_err() || out.flush().is_err() {
            break;
        }
    }
    0
}

async fn call_describe_image(params: &Value) -> Result<Value, String> {
    let path = params.get("path").and_then(Value::as_str);
    let url = params.get("url").and_then(Value::as_str);
    let prompt = params
        .get("prompt")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(
            "Describe this image in detail: the scene, any visible text \
             (transcribe it verbatim), UI elements, and chart values.",
        );

    let base = std::env::var(ENV_BASE_URL).map_err(|_| format!("{ENV_BASE_URL} not set"))?;
    let token = std::env::var(ENV_TOKEN).map_err(|_| format!("{ENV_TOKEN} not set"))?;
    let model = std::env::var(ENV_MODEL).map_err(|_| format!("{ENV_MODEL} not set"))?;

    let (bytes, media_type) = if let Some(p) = path {
        read_image_file(p)?
    } else if let Some(u) = url {
        fetch_image(u).await?
    } else {
        return Err("provide either path or url".into());
    };
    let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes);

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
            "messages": [{
                "role": "user",
                "content": [
                    { "type": "image", "source": {
                        "type": "base64", "media_type": media_type, "data": encoded,
                    }},
                    { "type": "text", "text": prompt },
                ],
            }],
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
}
