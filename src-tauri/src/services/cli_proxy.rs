//! CLI 代理：所有 code CLI 指向本机这一个端口，由它转发到内核。
//!
//! 为什么不让 CLI 直接指内核：直连时内核只看得见「一个 HTTP 请求」，日志里
//! 没有会话的任何痕迹（`/admin/logs` 的记录里既没有 session_id 也没有上游
//! request id，实测 1000 条里 0 条）。而 CLI **自己**在请求上带了会话标识：
//!
//! * Claude Code：`X-Claude-Code-Session-Id`，值就是
//!   `~/.claude/projects/<slug>/<那个 uuid>.jsonl` 的文件名；
//! * Codex：`session-id` / `thread-id` / `x-codex-turn-metadata`。
//!
//! 插在中间就能把这个标识旁路记下来，日志才点得到会话。顺带解决第二件事：
//! CLI 发的模型名内核不一定认（`claude-opus-5[1m]` 这种带窗口后缀的实测 503，
//! 和不存在的模型同样报错），转发前按映射表改写。
//!
//! 和 `embed_proxy` 的分工：那个是给 admin iframe 剥 `X-Frame-Options` 的，
//! 只服务我们自己的窗口；这个是数据面，要扛 CLI 的长连接和 SSE。两者都手写
//! HTTP/1.1 但目标不同，共用一份会把「安全边界」和「转发性能」搅在一起。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::RwLock;

use crate::error::AppError;
use crate::services::kernel::{http_client_for_kernel, HttpClientOpts, KernelConfig};

/// 固定端口。CLI 配置里写死的就是它，换端口等于所有接管配置失效，所以不
/// 像 embed_proxy 那样在一个区间里试探 —— 端口被占就是硬错误，得让用户看见。
pub const PROXY_PORT: u16 = 15777;

/// 逐条转发记录里保留多少条。日志页只回看最近的请求，再多就是白占内存。
const MAX_RECORDS: usize = 2000;

/// 连内核失败时重试几次、每次退避多久。三次 × 递增退避共约 1.2s，够托管内核
/// 从 `syscall.Exec` 自重启里回来、也够隧道抖一下；再长就该把错误交给 CLI 了。
const CONNECT_RETRIES: u32 = 3;
const CONNECT_RETRY_BACKOFF: std::time::Duration = std::time::Duration::from_millis(200);

/// 请求**还没送达内核**就失败了——这类可以安全重试。
///
/// `is_connect()` 覆盖连接拒绝/握手失败；`is_request()` 且非超时覆盖
/// 「连接池里的旧连接被对端关了」（reqwest 报成 request error）。
/// 超时不算：请求可能已经在内核里跑了，重放等于双倍账单。
fn is_connect_failure(e: &reqwest::Error) -> bool {
    if e.is_timeout() {
        return false;
    }
    if e.is_connect() {
        return true;
    }
    // hyper 的 "connection closed before message completed" / "connection reset"
    // 在 reqwest 里是 request error；只认还没拿到状态的那种。
    e.is_request() && e.status().is_none()
}

/// 请求体上限。CLI 的对话请求（含整段上下文）实测最大几百 MB 量级；
/// 上限挡的是异常与恶意 —— 声明 10GB 的 Content-Length 不该把进程内存
/// 吃穿。超过就 413，客户端自己会重试或报错，比 OOM 强。
const MAX_BODY: usize = 512 * 1024 * 1024;

/// 一次转发留下的会话痕迹。内核日志给不了这些，全靠代理这一层。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProxyRecord {
    /// 收到请求的时刻（unix 秒）。和内核日志对齐时用。
    pub time: i64,
    /// 发起的 CLI：claude-code / codex / grok / opencode / unknown。
    pub cli: String,
    /// CLI 自己的会话 id。Claude Code 的这个值就是 session jsonl 的文件名。
    pub session_id: Option<String>,
    /// CLI 请求里写的模型名（改写前）。
    pub model: Option<String>,
    /// 实际发给内核的模型名（改写后）。和 `model` 不同才说明发生了改写。
    pub sent_model: Option<String>,
    pub path: String,
    pub status: u16,
    /// 与内核日志对齐后回填的消耗（美元，倍率后）。对不上就是 None ——
    /// 代理自己算不了成本，成本数字只认内核的。
    #[serde(default)]
    pub cost: Option<f64>,
    /// 同上：输出 tokens。输入大头在缓存里，分项看内核日志。
    #[serde(default)]
    pub output_tokens: Option<i64>,
}

/// 模型名改写规则：CLI 发的名字 -> 内核认的名字。
pub type ModelRewrites = HashMap<String, String>;

/// 把请求里的 `cache_control` 升到 1 小时窗口。
///
/// **默认关掉，而且多数时候就该关着。** 内核不改写缓存窗口，完全跟随调用方
/// （`anthropic_wire.go:988`），所以这是唯一能改的地方 —— 但实测数据说明改了
/// 通常更贵：本机 101,259 次「同会话相邻请求」间隔里，98.1% 短于 5 分钟，
/// 只有 1.6% 落在 5 分钟到 1 小时之间。而 1h 档的写入价是 2×、5m 档是 1.25×
/// （读都是 0.1×）。为那 1.6% 把**全部**写入涨价 60%，算下来是净亏。
///
/// 真正划算的场景是「一轮聊很久、中间长时间没人说话」——比如按小时轮询的
/// 定时任务。留这个开关是为那种用法，不是给交互式会话开的。
fn upgrade_cache_ttl(v: &mut serde_json::Value) {
    match v {
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::Object(cc)) = map.get_mut("cache_control") {
                if cc.get("type").and_then(|t| t.as_str()) == Some("ephemeral") {
                    cc.insert("ttl".into(), serde_json::Value::String("1h".into()));
                }
            }
            for (_, child) in map.iter_mut() {
                upgrade_cache_ttl(child);
            }
        }
        serde_json::Value::Array(items) => {
            for it in items.iter_mut() {
                upgrade_cache_ttl(it);
            }
        }
        _ => {}
    }
}

pub struct CliProxy {
    target: Arc<RwLock<String>>,
    rewrites: Arc<RwLock<ModelRewrites>>,
    /// 见 `upgrade_cache_ttl` —— 默认 false，实测多数场景开着更贵。
    long_cache: Arc<std::sync::atomic::AtomicBool>,
    records: Arc<RwLock<Vec<ProxyRecord>>>,
    http: Arc<RwLock<reqwest::Client>>,
    handle: tokio::task::JoinHandle<()>,
}

impl CliProxy {
    pub async fn start(cfg: &KernelConfig) -> Result<Arc<Self>, AppError> {
        let listener = TcpListener::bind(("127.0.0.1", PROXY_PORT))
            .await
            .map_err(|e| {
                AppError::Config(format!(
                    "CLI 代理端口 {PROXY_PORT} 占不住（{e}）。接管配置里写死的就是这个端口，\
                     换一个等于所有 CLI 都失联 —— 先腾出它再启动。"
                ))
            })?;

        let target = Arc::new(RwLock::new(cfg.base_url()));
        let rewrites: Arc<RwLock<ModelRewrites>> = Arc::new(RwLock::new(HashMap::new()));
        let long_cache = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let records: Arc<RwLock<Vec<ProxyRecord>>> = Arc::new(RwLock::new(Vec::new()));
        let http = Arc::new(RwLock::new(cli_proxy_client(cfg)?));

        let (t, w, r, lc, h) = (
            Arc::clone(&target),
            Arc::clone(&rewrites),
            Arc::clone(&records),
            Arc::clone(&long_cache),
            Arc::clone(&http),
        );
        let handle = tokio::spawn(async move {
            loop {
                // accept 出错**不能**退出循环：一次瞬时错误（并发一多就撞的
                // EMFILE、客户端在握手中途走掉的 ECONNABORTED）就会让 listener
                // 被 drop、15777 空出来，而 `state.cli_proxy` 还是 Some ——
                // 于是接管地址照旧指向它，每个 CLI 都 ECONNREFUSED，界面上
                // 什么都看不出来，只能重启客户端。
                let stream = match listener.accept().await {
                    Ok((stream, _)) => stream,
                    Err(e) => {
                        tracing::warn!("cli proxy accept: {e}");
                        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        continue;
                    }
                };
                let (t, w, r, lc, h) = (
                    Arc::clone(&t),
                    Arc::clone(&w),
                    Arc::clone(&r),
                    Arc::clone(&lc),
                    Arc::clone(&h),
                );
                tokio::spawn(async move {
                    let _ = handle_conn(stream, t, w, r, lc, h).await;
                });
            }
        });

        Ok(Arc::new(Self {
            target,
            rewrites,
            long_cache,
            records,
            http,
            handle,
        }))
    }

    /// 内核地址变了（切换本地/远端、改端口）时重新指向，不用重启监听。
    pub async fn retarget(&self, cfg: &KernelConfig) -> Result<(), AppError> {
        *self.target.write().await = cfg.base_url();
        *self.http.write().await = cli_proxy_client(cfg)?;
        Ok(())
    }

    /// 打开/关掉 1 小时缓存窗口。见 `upgrade_cache_ttl` 里的实测数据 ——
    /// 交互式会话开着通常更贵，这个开关是给长间隔的定时任务用的。
    pub fn long_cache_enabled(&self) -> bool {
        self.long_cache.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn set_long_cache(&self, on: bool) {
        self.long_cache
            .store(on, std::sync::atomic::Ordering::Relaxed);
    }

    pub async fn set_rewrites(&self, rules: ModelRewrites) {
        *self.rewrites.write().await = rules;
    }

    /// 最近的转发记录，最新的在前。
    pub async fn records(&self) -> Vec<ProxyRecord> {
        let mut v = self.records.read().await.clone();
        v.reverse();
        v
    }

    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{PROXY_PORT}")
    }
}

impl Drop for CliProxy {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// 从 User-Agent / 特征头认出是哪个 CLI。认不出不影响转发，只是记录里标 unknown。
fn detect_cli(headers: &[(String, String)]) -> String {
    let get = |want: &str| {
        headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(want))
            .map(|(_, v)| v.as_str())
            .unwrap_or("")
    };
    let ua = get("user-agent").to_ascii_lowercase();
    if !get("x-claude-code-session-id").is_empty() || ua.contains("claude-cli") {
        return "claude-code".into();
    }
    // Codex 把发起方写在 originator 里（codex_exec / codex_cli...）。
    let originator = get("originator").to_ascii_lowercase();
    if originator.contains("codex") || ua.contains("codex") {
        return "codex".into();
    }
    if ua.contains("grok") {
        return "grok".into();
    }
    if ua.contains("opencode") {
        return "opencode".into();
    }
    "unknown".into()
}

/// 会话 id。各家放的位置不同，按可靠性从高到低试。
fn detect_session(headers: &[(String, String)]) -> Option<String> {
    let get = |want: &str| {
        headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(want))
            .map(|(_, v)| v.trim().to_string())
            .filter(|v| !v.is_empty())
    };
    // Claude Code 直接给一个头，值即 session jsonl 文件名。
    get("x-claude-code-session-id")
        // Codex 三个头同值，thread-id 语义最稳（一个 thread 一个会话文件）。
        .or_else(|| get("thread-id"))
        .or_else(|| get("session-id"))
}

/// 请求体的改写：模型名按映射表换，可选地把缓存窗口升到 1h。
/// 返回「CLI 原本写的模型名」和「改写后的整个 body」（没有任何改动时是 None，
/// 原字节直接透传）。除这两处外一个字节都不动 —— `messages` / `system` /
/// `tools` 原样过，会话内容不受影响。
fn rewrite_body(
    body: &[u8],
    rules: &ModelRewrites,
    long_cache: bool,
) -> (Option<String>, Option<Vec<u8>>) {
    if body.is_empty() {
        return (None, None);
    }
    let Ok(mut v) = serde_json::from_slice::<serde_json::Value>(body) else {
        return (None, None);
    };
    let model = v.get("model").and_then(|m| m.as_str()).map(str::to_string);
    let next = model
        .as_deref()
        .and_then(|m| resolve_rewrite(m, rules))
        .filter(|n| Some(n.as_str()) != model.as_deref());

    let mut touched = false;
    if let Some(n) = next {
        v["model"] = serde_json::Value::String(n);
        touched = true;
    }
    if long_cache {
        upgrade_cache_ttl(&mut v);
        touched = true;
    }
    if !touched {
        return (model, None);
    }
    match serde_json::to_vec(&v) {
        Ok(bytes) => (model, Some(bytes)),
        Err(_) => (model, None),
    }
}

/// 先查显式映射；没有就剥掉窗口后缀 —— `claude-opus-5[1m]` 内核不认，
/// 剥成 `claude-opus-5` 才有渠道接得住。后缀本身只是给客户端算窗口用的。
fn resolve_rewrite(model: &str, rules: &ModelRewrites) -> Option<String> {
    if let Some(hit) = rules.get(model) {
        return Some(hit.clone());
    }
    let trimmed = model.trim_end();
    if trimmed.ends_with(']') {
        if let Some(open) = trimmed.rfind('[') {
            let bare = trimmed[..open].trim_end();
            if !bare.is_empty() {
                return Some(bare.to_string());
            }
        }
    }
    None
}

/// 连接超时 5s，响应体不设上限 —— 一次长回答流上几分钟是常态。
#[cfg(test)]
fn test_proxy_http() -> Arc<RwLock<reqwest::Client>> {
    Arc::new(RwLock::new(
        reqwest::Client::builder()
            .no_proxy()
            .connect_timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap(),
    ))
}

fn cli_proxy_client(cfg: &KernelConfig) -> Result<reqwest::Client, AppError> {
    http_client_for_kernel(
        cfg,
        HttpClientOpts {
            timeout: None,
            connect_timeout: std::time::Duration::from_secs(5),
            follow_system_proxy: true,
        },
    )
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4)
}

/// 读一个 chunked 请求体并**解出原始字节**。转发头里 `Transfer-Encoding` 已经被
/// 剥掉，发给上游的是我们用 Content-Length 重新分帧的 plain body —— 所以上游
/// 看到的一直是同一种形态，模型改写也不会撞上分块边界。
///
/// `head` 是头部之后已经读进来的字节，先于任何新读取被消费。终止条件是
/// 0 长度块；trailers 一律丢弃（模型改写用不上它们）。
/// 读一个 chunked 请求体。
///
/// `limit` 是**边读边算**的上限，不是读完再查：没有它的话，一个声称 4GB 的
/// chunked 请求会把内存吃干才轮到外面那句 413；而 `raw` 还留着已经消费过的
/// 字节，峰值是体积的两倍。size 行也要挡：`ffffffffffffffff` 解析出来是
/// usize::MAX，`pos + size + 2` 直接溢出（debug 崩，release 回绕后越界崩）。
async fn read_chunked_body(
    client: &mut TcpStream,
    head: Vec<u8>,
    tmp: &mut [u8; 16 * 1024],
    limit: usize,
) -> std::io::Result<Vec<u8>> {
    // 把「当前消费位置」留在 chunk 量这一行，用最小实现：手写游标推进。
    let mut raw = head;
    let mut pos = 0usize;

    // 取到下一对 \r\n 之间的内容，不够就从 socket 补。
    macro_rules! read_line {
        () => {{
            loop {
                if let Some(rel) = raw[pos..]
                    .windows(2)
                    .position(|w| w == b"\r\n")
                {
                    let line = String::from_utf8_lossy(&raw[pos..pos + rel]).to_string();
                    pos += rel + 2;
                    break Some(line);
                }
                let n = client.read(tmp).await?;
                if n == 0 {
                    break None;
                }
                raw.extend_from_slice(&tmp[..n]);
            }
        }};
    }

    let mut out: Vec<u8> = Vec::new();
    while let Some(line) = read_line!() {
        // 量这一行可能带分号后的扩展（`1a;ext=…`），取分号前的十六进制。
        let size_part = line.split(';').next().unwrap_or("").trim();
        // 16 位十六进制就是 u64 的全宽；再长的只可能是垃圾或攻击，别去 parse。
        let Some(size) = (size_part.len() <= 16)
            .then(|| usize::from_str_radix(size_part, 16).ok())
            .flatten()
        else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("bad chunk size line: {size_part:?}"),
            ));
        };
        // 预算检查放在**读之前**：读完再查等于已经把内存吃掉了。
        if size > limit.saturating_sub(out.len()) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "chunked body exceeds the size limit",
            ));
        }
        if size == 0 {
            // 0 块之后是可选的 trailer 区，以空行结束。读到空行（或连接断）为止。
            while let Some(t) = read_line!() {
                if t.is_empty() {
                    break;
                }
            }
            break;
        }
        // 补齐这一块的数据 + 结尾的 \r\n。
        while raw.len() < pos + size + 2 {
            let n = client.read(tmp).await?;
            if n == 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "chunked body truncated mid-chunk",
                ));
            }
            raw.extend_from_slice(&tmp[..n]);
        }
        out.extend_from_slice(&raw[pos..pos + size]);
        pos += size + 2; // 跳过块尾的 \r\n
        // 已经消费掉的前缀就别留着了 —— 不然 raw 和 out 各存一份整个请求体。
        raw.drain(..pos);
        pos = 0;
    }
    Ok(out)
}

async fn write_simple(client: &mut TcpStream, status: u16, msg: &str) -> std::io::Result<()> {
    let body = format!("{{\"error\":{}}}", serde_json::json!(msg));
    let head = format!(
        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    client.write_all(head.as_bytes()).await?;
    client.write_all(body.as_bytes()).await
}

async fn handle_conn(
    mut client: TcpStream,
    target: Arc<RwLock<String>>,
    rewrites: Arc<RwLock<ModelRewrites>>,
    records: Arc<RwLock<Vec<ProxyRecord>>>,
    long_cache: Arc<std::sync::atomic::AtomicBool>,
    http: Arc<RwLock<reqwest::Client>>,
) -> std::io::Result<()> {
    let mut buf: Vec<u8> = Vec::with_capacity(16 * 1024);
    let mut tmp = [0u8; 16 * 1024];
    let header_end = loop {
        let n = client.read(&mut tmp).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = find_header_end(&buf) {
            break pos;
        }
        if buf.len() > 128 * 1024 {
            return write_simple(&mut client, 431, "headers too large").await;
        }
    };

    let headers_text = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = headers_text.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let path = parts.next().unwrap_or("/").to_string();

    // 请求目标必须是 origin-form（`/v1/messages`）。
    //
    // 它会被 `format!("{upstream}{path}")` 直接拼进 URL：`GET @evil.com/x` 拼出
    // `http://127.0.0.1:8080@evil.com/x`，按 URL 规则 host 是 evil.com、前面那截
    // 成了 userinfo —— 任意本地进程都能借这个代理连到外网，Remote 模式下还会走
    // 用户配好的出口代理（SOCKS/SSH 隧道）。`//x` 是 protocol-relative，同理。
    if !path.starts_with('/') || path.starts_with("//") {
        return write_simple(&mut client, 400, "bad request target").await;
    }

    let mut content_length = 0usize;
    let mut chunked = false;
    let mut fwd: Vec<(String, String)> = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let lower = name.trim().to_ascii_lowercase();
        // Host 要按上游重算；Connection/Transfer-Encoding 由我们这层重新决定；
        // content-length 改写后会变，转发时重新给。
        if lower == "host" || lower == "connection" {
            continue;
        }
        if lower == "transfer-encoding" {
            chunked = value.to_ascii_lowercase().split(',').any(|t| t.trim() == "chunked");
            continue;
        }
        if lower == "content-length" {
            content_length = value.trim().parse().unwrap_or(0);
            continue;
        }
        fwd.push((name.trim().to_string(), value.trim().to_string()));
    }

    if content_length > MAX_BODY {
        return write_simple(&mut client, 413, "request body too large").await;
    }

    // RFC 9112：chunked 和 Content-Length 同时出现时以 chunked 为准。
    let leftover = buf[header_end..].to_vec();
    let body_bytes: Vec<u8> = if chunked {
        // 上限传进去边读边算 —— 超了当场报错，而不是把内存吃干再回 413。
        match read_chunked_body(&mut client, leftover, &mut tmp, MAX_BODY).await {
            Ok(body) => body,
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                return write_simple(&mut client, 413, "request body too large").await;
            }
            Err(e) => return Err(e),
        }
    } else if content_length > 0 {
        let mut body = leftover;
        while body.len() < content_length {
            let n = client.read(&mut tmp).await?;
            if n == 0 {
                break;
            }
            body.extend_from_slice(&tmp[..n]);
        }
        body.truncate(content_length);
        body
    } else {
        Vec::new()
    };

    let cli = detect_cli(&fwd);
    let session_id = detect_session(&fwd);
    let rules = rewrites.read().await.clone();
    let (model, rewritten) = rewrite_body(
        &body_bytes,
        &rules,
        long_cache.load(std::sync::atomic::Ordering::Relaxed),
    );
    let sent_model = rewritten.as_ref().and_then(|b| {
        serde_json::from_slice::<serde_json::Value>(b)
            .ok()
            .and_then(|v| v.get("model").and_then(|m| m.as_str()).map(str::to_string))
    });
    let out_body = rewritten.unwrap_or(body_bytes);

    let upstream = target.read().await.clone();
    let url = format!("{upstream}{path}");
    let Ok(method_parsed) = method.parse::<reqwest::Method>() else {
        return write_simple(&mut client, 400, "bad method").await;
    };
    let http = http.read().await.clone();
    // 和内核同一个宗旨：**能重试的绝不直接抛错给 CLI**。内核对上游是 Key →
    // 模型 → 渠道三级冷却切换；到了这一层，唯一能透明兜住的是「连内核这一跳」
    // 的瞬时失败——托管内核刚重启、远端隧道抖了一下、连接池里的旧连接被对端
    // 关了。这些都是几百毫秒内自愈的事，一次就 502 会让 Claude Code 把整个
    // turn 判失败，用户看到红字重发，prompt cache 还得重建。
    //
    // 只重试**请求还没发出去**就失败的情况（连不上、握手失败）：请求体已经进
    // 了内核就不能再发第二遍——那是内核自己的故障转移在管的事，重放会让一次
    // 请求变两次账单。
    let resp = {
        let mut attempt = 0u32;
        loop {
            let mut r = http.request(method_parsed.clone(), &url);
            for (name, value) in &fwd {
                if let Ok(hv) = reqwest::header::HeaderValue::from_str(value) {
                    r = r.header(name, hv);
                }
            }
            if !out_body.is_empty() {
                r = r.header(reqwest::header::CONTENT_LENGTH, out_body.len());
                r = r.body(out_body.clone());
            }
            match r.send().await {
                Ok(resp) => break Ok(resp),
                Err(e) if attempt < CONNECT_RETRIES && is_connect_failure(&e) => {
                    attempt += 1;
                    tracing::debug!("cli proxy: connect to kernel failed, retry {attempt}: {e}");
                    tokio::time::sleep(CONNECT_RETRY_BACKOFF * attempt).await;
                }
                Err(e) => break Err(e),
            }
        }
    };

    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            push_record(
                &records,
                ProxyRecord {
                    time: now_secs(),
                    cli,
                    session_id,
                    model,
                    sent_model,
                    path,
                    status: 502,
                    cost: None,
                    output_tokens: None,
                },
            )
            .await;
            return write_simple(
                &mut client,
                502,
                &format!("kernel unreachable ({url}): {e}"),
            )
            .await;
        }
    };

    let status = resp.status();
    push_record(
        &records,
        ProxyRecord {
            time: now_secs(),
            cli,
            session_id,
            model,
            sent_model,
            path,
            status: status.as_u16(),
            cost: None,
            output_tokens: None,
        },
    )
    .await;

    let has_len = resp
        .headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .is_some();
    let mut head = format!("HTTP/1.1 {status}\r\n");
    for (name, value) in resp.headers() {
        let lower = name.as_str().to_ascii_lowercase();
        if lower == "transfer-encoding" || lower == "connection" {
            continue;
        }
        if let Ok(vs) = value.to_str() {
            head.push_str(&format!("{name}: {vs}\r\n"));
        }
    }
    if !has_len {
        head.push_str("Transfer-Encoding: chunked\r\n");
    }
    head.push_str("Connection: close\r\n\r\n");
    client.write_all(head.as_bytes()).await?;

    // 逐块转发，绝不整体缓冲 —— CLI 全是流式，缓冲会让首字节等到最后。
    let mut stream = resp.bytes_stream();
    let mut upstream_broke = false;
    while let Some(item) = stream.next().await {
        let bytes = match item {
            Ok(b) => b,
            Err(e) => {
                // 上游中途断了（内核崩了、隧道掉了）。**不能**照常收尾：
                // 补上 `0\r\n\r\n` 会把一个截断的回答包装成格式完整的响应，
                // CLI 分不出来，把半截输出当成最终结果。留着不收尾直接断开，
                // 客户端才会看到 truncated body 并按错误处理。
                tracing::warn!("cli proxy: upstream stream broke mid-response: {e}");
                upstream_broke = true;
                break;
            }
        };
        if !has_len {
            client
                .write_all(format!("{:x}\r\n", bytes.len()).as_bytes())
                .await?;
        }
        client.write_all(&bytes).await?;
        if !has_len {
            client.write_all(b"\r\n").await?;
        }
        client.flush().await?;
    }
    if upstream_broke {
        // 不写终止块：让对端看到「连接断在半路」而不是「干净地结束了」。
        let _ = client.flush().await;
        return Ok(());
    }
    if !has_len {
        client.write_all(b"0\r\n\r\n").await?;
    }
    client.flush().await
}

async fn push_record(records: &Arc<RwLock<Vec<ProxyRecord>>>, rec: ProxyRecord) {
    let mut v = records.write().await;
    v.push(rec);
    if v.len() > MAX_RECORDS {
        let cut = v.len() - MAX_RECORDS;
        v.drain(..cut);
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Claude Code 的会话文件：`~/.claude/projects/<slug>/<session_id>.jsonl`。
/// slug 是 cwd 把 `/` 换成 `-`，但我们不知道 CLI 当时的 cwd，所以按文件名找。
pub fn claude_session_file(session_id: &str) -> Option<PathBuf> {
    // session_id 来自请求头，是**外部输入**。`join` 遇到绝对路径会直接丢掉
    // 前面的基准目录，`../` 也不会被拒 —— 不校验的话，任意本地进程发一个
    // `X-Claude-Code-Session-Id: /Users/x/private/notes` 就能让日志页去读
    // 并显示磁盘上任意一个文件。uuid 只有这些字符，多一个都不认。
    if !is_safe_session_id(session_id) {
        return None;
    }
    let projects = dirs::home_dir()?.join(".claude/projects");
    let entries = std::fs::read_dir(projects).ok()?;
    for e in entries.flatten() {
        let candidate = e.path().join(format!("{session_id}.jsonl"));
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// 会话 id 的形状：uuid 用到的字符集，长度设个上限。路径分隔符、`.`、`~`
/// 全都不在里面，所以拼进路径之后跑不出 `~/.claude/projects`。
fn is_safe_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// 会话标题。Claude Code 自己会生成 `ai-title`，但磁盘上只有极少数会话有
/// （实测 1416 个里 100 个），所以取不到就退回首条用户消息的开头。
pub fn session_title(path: &std::path::Path) -> Option<String> {
    // 逐行读、读够就停：会话 jsonl 动辄几百 MB，为了一个 60 字的标题
    // `read_to_string` 整个文件是白吃内存（日志页每点一次都来一遍）。
    use std::io::BufRead;
    const MAX_SCAN_BYTES: usize = 2 * 1024 * 1024;
    let file = std::fs::File::open(path).ok()?;
    let mut reader = std::io::BufReader::new(file);
    let mut scanned = 0usize;
    let mut lines: Vec<String> = Vec::new();
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(n) => {
                scanned += n;
                lines.push(line);
                if scanned >= MAX_SCAN_BYTES {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let mut first_user: Option<String> = None;
    for line in lines.iter().map(|l| l.as_str()) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if v.get("type").and_then(|t| t.as_str()) == Some("ai-title") {
            if let Some(t) = v.get("aiTitle").and_then(|t| t.as_str()) {
                return Some(t.to_string());
            }
        }
        if first_user.is_none() && v.get("type").and_then(|t| t.as_str()) == Some("user") {
            if let Some(c) = v.pointer("/message/content").and_then(|c| c.as_str()) {
                first_user = Some(c.chars().take(60).collect());
            }
        }
    }
    first_user
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    #[test]
    fn claude_code_is_detected_by_its_session_header() {
        let hs = h(&[
            ("User-Agent", "claude-cli/2.1.226 (external, sdk-cli)"),
            ("X-Claude-Code-Session-Id", "b419e406-5a41-43fe-8278-b2a0188101a2"),
        ]);
        assert_eq!(detect_cli(&hs), "claude-code");
        assert_eq!(
            detect_session(&hs).as_deref(),
            Some("b419e406-5a41-43fe-8278-b2a0188101a2")
        );
    }

    /// Codex 三个头同值，取哪个都行，但 thread-id 要优先于 session-id。
    #[test]
    fn codex_is_detected_by_originator() {
        let hs = h(&[
            ("user-agent", "codex_exec/0.144.6 (Mac OS 26.3.1; arm64)"),
            ("originator", "codex_exec"),
            ("session-id", "01a046c9-e7a9-7333-9add-7c0a87b0ef3a"),
            ("thread-id", "01a046c9-e7a9-7333-9add-7c0a87b0ef3a"),
        ]);
        assert_eq!(detect_cli(&hs), "codex");
        assert_eq!(
            detect_session(&hs).as_deref(),
            Some("01a046c9-e7a9-7333-9add-7c0a87b0ef3a")
        );
    }

    #[test]
    fn unknown_cli_still_forwards() {
        let hs = h(&[("user-agent", "curl/8.7.1")]);
        assert_eq!(detect_cli(&hs), "unknown");
        assert_eq!(detect_session(&hs), None);
    }

    /// 内核不认带窗口后缀的名字（实测 503），转发前必须剥掉。
    #[test]
    fn window_suffix_is_stripped_without_a_rule() {
        let rules = ModelRewrites::new();
        let body = br#"{"model":"claude-opus-5[1m]","max_tokens":8}"#;
        let (seen, out) = rewrite_body(body, &rules, false);
        assert_eq!(seen.as_deref(), Some("claude-opus-5[1m]"));
        let out = out.expect("body should be rewritten");
        let v: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(v["model"], "claude-opus-5");
        // 其余字段不能在改写中丢掉。
        assert_eq!(v["max_tokens"], 8);
    }

    #[test]
    fn explicit_rule_wins_over_suffix_stripping() {
        let mut rules = ModelRewrites::new();
        rules.insert("claude-opus-5[1m]".into(), "glm-5.3-flash".into());
        let (_, out) = rewrite_body(br#"{"model":"claude-opus-5[1m]"}"#, &rules, false);
        let v: serde_json::Value = serde_json::from_slice(&out.unwrap()).unwrap();
        assert_eq!(v["model"], "glm-5.3-flash");
    }

    #[test]
    fn plain_model_is_left_alone() {
        let rules = ModelRewrites::new();
        let (seen, out) = rewrite_body(br#"{"model":"claude-opus-5"}"#, &rules, false);
        assert_eq!(seen.as_deref(), Some("claude-opus-5"));
        assert!(out.is_none(), "no rewrite means the body must pass through");
    }

    /// 缓存窗口默认不碰 —— 实测 98.1% 的相邻请求间隔短于 5 分钟，
    /// 升到 1h 只会让写入价从 1.25× 涨到 2×。
    #[test]
    fn cache_ttl_is_untouched_by_default() {
        let rules = ModelRewrites::new();
        let body = br#"{"model":"claude-opus-5","system":[{"type":"text","cache_control":{"type":"ephemeral"}}]}"#;
        let (_, out) = rewrite_body(body, &rules, false);
        assert!(out.is_none(), "关着时一个字节都不该动");
    }

    #[test]
    fn long_cache_upgrades_every_breakpoint() {
        let rules = ModelRewrites::new();
        let body = br#"{"model":"claude-opus-5",
            "system":[{"type":"text","cache_control":{"type":"ephemeral"}}],
            "tools":[{"name":"x","cache_control":{"type":"ephemeral"}}]}"#;
        let (_, out) = rewrite_body(body, &rules, true);
        let v: serde_json::Value = serde_json::from_slice(&out.expect("body rewritten")).unwrap();
        assert_eq!(v["system"][0]["cache_control"]["ttl"], "1h");
        assert_eq!(v["tools"][0]["cache_control"]["ttl"], "1h");
        assert_eq!(v["model"], "claude-opus-5");
    }

    /// 非 ephemeral 的 cache_control 不该被贴上 ttl。
    #[test]
    fn long_cache_only_touches_ephemeral_breakpoints() {
        let rules = ModelRewrites::new();
        let body = br#"{"system":[{"cache_control":{"type":"persistent"}}]}"#;
        let (_, out) = rewrite_body(body, &rules, true);
        let v: serde_json::Value = serde_json::from_slice(&out.expect("parsed")).unwrap();
        assert!(v["system"][0]["cache_control"].get("ttl").is_none());
    }

    /// 非 JSON 体（或没有 model 字段）不能把请求搞坏。
    #[test]
    fn non_json_body_passes_through() {
        let rules = ModelRewrites::new();
        assert_eq!(rewrite_body(b"not json at all", &rules, false).1, None);
        assert_eq!(rewrite_body(br#"{"messages":[]}"#, &rules, false).1, None);
        assert_eq!(rewrite_body(b"", &rules, false).0, None);
    }
}

#[cfg(test)]
mod e2e {
    use super::*;

    /// 端到端：起一个假上游当「内核」，代理转发过去，验证
    /// 模型名被改写、会话头被记录、SSE 逐块透传。
    #[tokio::test]
    async fn forwards_rewrites_and_records() {
        // 假上游：把收到的 model 回显出来，并以 SSE 分两块返回。
        let up = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let up_port = up.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut s, _) = up.accept().await.unwrap();
            let mut buf = vec![0u8; 65536];
            let n = s.read(&mut buf).await.unwrap();
            let text = String::from_utf8_lossy(&buf[..n]).to_string();
            let model = text
                .rsplit_once("\"model\":\"")
                .and_then(|(_, r)| r.split('"').next().map(str::to_string))
                .unwrap_or_default();
            let body = format!("data: {{\"model\":\"{model}\"}}\n\n");
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n",
                body.len()
            );
            s.write_all(head.as_bytes()).await.unwrap();
            s.write_all(body.as_bytes()).await.unwrap();
            s.flush().await.unwrap();
        });

        let target = Arc::new(RwLock::new(format!("http://127.0.0.1:{up_port}")));
        let rewrites = Arc::new(RwLock::new(ModelRewrites::new()));
        let records = Arc::new(RwLock::new(Vec::new()));

        let front = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let front_port = front.local_addr().unwrap().port();
        let (t, w, r) = (
            Arc::clone(&target),
            Arc::clone(&rewrites),
            Arc::clone(&records),
        );
        tokio::spawn(async move {
            let (stream, _) = front.accept().await.unwrap();
            handle_conn(stream, t, w, r, Arc::new(std::sync::atomic::AtomicBool::new(false)), test_proxy_http())
                .await
                .unwrap();
        });

        // 扮成 Claude Code：带会话头，模型名挂 [1m] 后缀。
        let mut c = TcpStream::connect(("127.0.0.1", front_port)).await.unwrap();
        let body = br#"{"model":"claude-opus-5[1m]","max_tokens":8}"#;
        let req = format!(
            "POST /v1/messages HTTP/1.1\r\nHost: x\r\nX-Claude-Code-Session-Id: sess-abc\r\n\
             User-Agent: claude-cli/2.1.226\r\ncontent-length: {}\r\n\r\n",
            body.len()
        );
        c.write_all(req.as_bytes()).await.unwrap();
        c.write_all(body).await.unwrap();

        let mut got = Vec::new();
        c.read_to_end(&mut got).await.unwrap();
        let got = String::from_utf8_lossy(&got).to_string();

        // 上游收到的应该是剥掉后缀的名字。
        assert!(
            got.contains("\"model\":\"claude-opus-5\""),
            "upstream should have seen the stripped name, got: {got}"
        );
        assert!(got.contains("200 OK"), "got: {got}");

        let recs = records.read().await;
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].cli, "claude-code");
        assert_eq!(recs[0].session_id.as_deref(), Some("sess-abc"));
        assert_eq!(recs[0].model.as_deref(), Some("claude-opus-5[1m]"));
        assert_eq!(recs[0].sent_model.as_deref(), Some("claude-opus-5"));
        assert_eq!(recs[0].status, 200);
    }
}

/// 手动联调用：`cargo test --lib live_proxy -- --ignored --nocapture` 会把代理
/// 挂在真实端口上转发到真实内核，方便拿真的 CLI 打一遍。
#[cfg(test)]
mod live {
    use super::*;

    #[tokio::test]
    #[ignore = "需要真实内核和凭据，手动联调时才跑"]
    async fn live_proxy() {
        let raw = std::fs::read_to_string(
            dirs::home_dir().unwrap().join(".ccload-client/settings.json"),
        )
        .unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        let cfg: crate::services::kernel::KernelConfig =
            serde_json::from_value(v["kernel"].clone()).unwrap();
        let proxy = CliProxy::start(&cfg).await.unwrap();
        eprintln!("proxy up at {} -> {}", proxy.base_url(), cfg.base_url());
        tokio::time::sleep(std::time::Duration::from_secs(120)).await;
        for r in proxy.records().await {
            eprintln!("{r:?}");
        }
    }
}

#[cfg(test)]
mod lookup {
    use super::*;

    /// 会话反查：代理记下的 id 必须能落到磁盘上的 jsonl，并读出标题 ——
    /// 「点日志跳到会话」全靠这一步。
    #[test]
    #[ignore = "读真实 ~/.claude，手动验证时才跑"]
    fn resolves_a_real_session() {
        let sid = "39e5aab6-18e2-46b3-88c4-408ebbfb495b";
        let path = claude_session_file(sid).expect("session file should exist");
        eprintln!("path: {}", path.display());
        let title = session_title(&path);
        eprintln!("title: {title:?}");
        assert!(title.is_some(), "a title (ai-title or first user msg) is required");
    }
}

#[cfg(test)]
mod sse {
    use super::*;

    /// SSE 必须**逐块**透传：一块到就往下写一块，不能攒完再吐。
    /// 攒着发的话，CLI 侧的「正在输出」会卡成一次性弹出，长回答尤其明显。
    #[tokio::test]
    async fn chunks_arrive_incrementally_not_batched() {
        // 上游：每 300ms 吐一块，共 4 块。
        let up = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let up_port = up.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut s, _) = up.accept().await.unwrap();
            let mut buf = vec![0u8; 8192];
            let _ = s.read(&mut buf).await.unwrap();
            s.write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                  transfer-encoding: chunked\r\n\r\n",
            )
            .await
            .unwrap();
            s.flush().await.unwrap();
            for i in 0..4u8 {
                let body = format!("data: tick-{i}\n\n");
                s.write_all(format!("{:x}\r\n", body.len()).as_bytes())
                    .await
                    .unwrap();
                s.write_all(body.as_bytes()).await.unwrap();
                s.write_all(b"\r\n").await.unwrap();
                s.flush().await.unwrap();
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            }
            s.write_all(b"0\r\n\r\n").await.unwrap();
            s.flush().await.unwrap();
        });

        let target = Arc::new(RwLock::new(format!("http://127.0.0.1:{up_port}")));
        let front = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let front_port = front.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (stream, _) = front.accept().await.unwrap();
            handle_conn(
                stream,
                target,
                Arc::new(RwLock::new(ModelRewrites::new())),
                Arc::new(RwLock::new(Vec::new())),
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
                test_proxy_http(),
            )
            .await
            .unwrap();
        });

        let mut c = TcpStream::connect(("127.0.0.1", front_port)).await.unwrap();
        let body = br#"{"model":"m","stream":true}"#;
        c.write_all(
            format!(
                "POST /v1/messages HTTP/1.1\r\nHost: x\r\ncontent-length: {}\r\n\r\n",
                body.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
        c.write_all(body).await.unwrap();

        // 记录「第一次读到 tick-0」和「读到 tick-3」的时间差。
        let start = tokio::time::Instant::now();
        let mut seen = String::new();
        let mut first_tick: Option<std::time::Duration> = None;
        let mut buf = [0u8; 4096];
        loop {
            let n = c.read(&mut buf).await.unwrap();
            if n == 0 {
                break;
            }
            seen.push_str(&String::from_utf8_lossy(&buf[..n]));
            if first_tick.is_none() && seen.contains("tick-0") {
                first_tick = Some(start.elapsed());
            }
            if seen.contains("tick-3") {
                break;
            }
        }
        let last = start.elapsed();
        let first = first_tick.expect("tick-0 应当先到");

        assert!(seen.contains("tick-0") && seen.contains("tick-3"), "全部块都要到齐");
        // 真流式：第一块远早于最后一块（上游 4 块跨 ~900ms）。
        // 若被整体缓冲，两者会几乎同时到达。
        assert!(
            last.saturating_sub(first) > std::time::Duration::from_millis(400),
            "首块 {first:?}、末块 {last:?} —— 间隔太小，说明被攒着一次性发了"
        );
    }
}

#[cfg(test)]
mod chunked_req {
    use super::*;

    /// 用 chunked 发上来的请求体不能被吞掉。
    ///
    /// 代理只按 `Content-Length` 读 body，并且把 `Transfer-Encoding` 从转发头里
    /// 剥掉了 —— 客户端若用 chunked，两件事叠在一起就是「空 body 静默发出去」，
    /// 上游收到一个没有 messages 的请求。实测的几家 CLI 目前都发
    /// Content-Length，但这条不该靠运气。
    #[tokio::test]
    async fn a_chunked_request_body_is_not_silently_dropped() {
        // 假上游：把收到的 body 长度回显出来。
        let up = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let up_port = up.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut s, _) = up.accept().await.unwrap();
            let mut buf = vec![0u8; 65536];
            let n = s.read(&mut buf).await.unwrap();
            let text = String::from_utf8_lossy(&buf[..n]).to_string();
            let body = text.split("\r\n\r\n").nth(1).unwrap_or("").to_string();
            let out = format!("{{\"got\":{}}}", body.trim().len());
            s.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n{out}",
                    out.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
            s.flush().await.unwrap();
        });

        let front = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let front_port = front.local_addr().unwrap().port();
        let target = Arc::new(RwLock::new(format!("http://127.0.0.1:{up_port}")));
        tokio::spawn(async move {
            let (stream, _) = front.accept().await.unwrap();
            let _ = handle_conn(
                stream,
                target,
                Arc::new(RwLock::new(ModelRewrites::new())),
                Arc::new(RwLock::new(Vec::new())),
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
                test_proxy_http(),
            )
            .await;
        });

        // 用 chunked 发一个 body，不给 Content-Length。
        let mut c = TcpStream::connect(("127.0.0.1", front_port)).await.unwrap();
        let payload = br#"{"model":"m","messages":[{"role":"user","content":"hi"}]}"#;
        c.write_all(
            b"POST /v1/messages HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n",
        )
        .await
        .unwrap();
        c.write_all(format!("{:x}\r\n", payload.len()).as_bytes())
            .await
            .unwrap();
        c.write_all(payload).await.unwrap();
        c.write_all(b"\r\n0\r\n\r\n").await.unwrap();
        c.flush().await.unwrap();

        let mut got = Vec::new();
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            c.read_to_end(&mut got),
        )
        .await;
        let got = String::from_utf8_lossy(&got).to_string();
        assert!(
            !got.contains("\"got\":0"),
            "chunked 的 body 被吞成空了，上游收到 0 字节。响应：{got}"
        );
    }
}

#[cfg(test)]
mod key_order_tests {
    use super::*;

    /// 改写必须保持键序 —— Anthropic 的 prompt cache 是**前缀字节匹配**，
    /// serde_json 默认用 BTreeMap，to_vec 会按字母序重排键：`{"model":…,
    /// "messages":…,"max_tokens":…}` 改完变成 `{"max_tokens":…,"messages":…,
    /// "model":…}`。字段值全对，缓存照样全 miss。preserve_order 开启后，
    /// 除被改的字段外，字节序必须与输入一致。
    #[test]
    fn rewriting_preserves_key_order() {
        let rules = ModelRewrites::new();
        // 键序故意按「非字母序」排:model 在前,max_tokens 在中,messages 在后。
        let body = br#"{"model":"claude-opus-5[1m]","max_tokens":8,"messages":[{"role":"user","content":"hi"}],"system":"s"}"#;

        let (_, out) = rewrite_body(body, &rules, false);
        let out = out.expect("body 应当被改写(剥后缀)");

        let out_str = String::from_utf8(out).unwrap();
        // 逐键检查相对顺序:model 仍在最前,max_tokens 仍在 messages 之前,
        // system 仍在最后。字母序重排的话 max_tokens 会跑到最前面。
        let pos_model = out_str.find("\"model\"").unwrap();
        let pos_max = out_str.find("\"max_tokens\"").unwrap();
        let pos_messages = out_str.find("\"messages\"").unwrap();
        let pos_system = out_str.find("\"system\"").unwrap();
        assert!(
            pos_model < pos_max && pos_max < pos_messages && pos_messages < pos_system,
            "键序被重排了,缓存前缀会失效。输出:{out_str}"
        );
        assert!(out_str.contains("\"model\":\"claude-opus-5\""), "改写本身仍要生效");
    }

    /// 嵌套对象(顶层 system 是数组包对象)同样保序 —— cache_control 就住在
    /// 嵌套里。
    #[test]
    fn nested_objects_keep_order_too() {
        let rules = ModelRewrites::new();
        let body = br#"{"model":"m","system":[{"type":"text","cache_control":{"type":"ephemeral"}}]}"#;
        let (_, out) = rewrite_body(body, &rules, true);
        let out_str = String::from_utf8(out.unwrap()).unwrap();
        // type 在 cache_control 之前(声明序),ttl 追加在 type 之后(插入序)。
        let pos_type = out_str.find("\"type\":\"ephemeral\"").unwrap();
        let pos_ttl = out_str.find("\"ttl\":\"1h\"").unwrap();
        assert!(pos_type < pos_ttl, "ttl 应当追加在原键之后而非重排:{out_str}");
    }
}

#[cfg(test)]
mod hardening_tests {
    use super::*;

    /// session id 来自请求头。绝对路径会让 `join` 丢掉基准目录，`../` 能往上爬 ——
    /// 任意本地进程发一个头，就能让日志页读并显示磁盘上任意文件。
    #[test]
    fn a_header_supplied_session_id_cannot_escape_the_projects_dir() {
        for evil in [
            "/Users/x/private/notes",
            "../../../../etc/passwd",
            "..",
            "a/b",
            "a\\b",
            "with space",
            "",
            &"x".repeat(65),
        ] {
            assert!(!is_safe_session_id(evil), "{evil:?} 应当被拒");
            assert!(claude_session_file(evil).is_none(), "{evil:?} 竟然解析出了路径");
        }
        // 正常的 uuid 要放行。
        assert!(is_safe_session_id("39e5aab6-18e2-46b3-88c4-408ebbfb495b"));
    }

    /// 声称 4GB 的 chunked 请求必须在**读之前**就被挡掉，而不是把内存吃干
    /// 之后才轮到 413；离谱的 size 行也不能把 `pos + size + 2` 溢出。
    #[tokio::test]
    async fn an_oversized_chunk_is_refused_before_it_is_read() {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut tmp = [0u8; 16 * 1024];
            read_chunked_body(&mut sock, Vec::new(), &mut tmp, 1024).await
        });

        let mut c = tokio::net::TcpStream::connect(addr).await.unwrap();
        // 声称 16MB，上限是 1KB。
        c.write_all(b"1000000\r\n").await.unwrap();
        let err = server.await.unwrap().unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData, "{err}");
    }

    /// `ffffffffffffffff` 解析成 usize::MAX，旧代码 `pos + size + 2` 当场溢出。
    #[tokio::test]
    async fn an_absurd_chunk_size_does_not_overflow() {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut tmp = [0u8; 16 * 1024];
            read_chunked_body(&mut sock, Vec::new(), &mut tmp, MAX_BODY).await
        });
        let mut c = tokio::net::TcpStream::connect(addr).await.unwrap();
        c.write_all(b"ffffffffffffffffff\r\n").await.unwrap();
        let err = server.await.unwrap().unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData, "{err}");
    }

    /// 请求目标被原样拼进上游 URL。`@evil.com/x` 会让 host 变成 evil.com
    /// （前面那截成了 userinfo）—— 任意本地进程借这个代理连外网，Remote 模式下
    /// 还走用户配的出口代理。用 url crate 确认这个解析结果，再钉住我们的判据。
    #[test]
    fn a_non_origin_form_target_would_change_the_host() {
        let upstream = "http://127.0.0.1:8080";
        // `@` 那两个是真的会换 host：前面那截被解析成 userinfo。
        for evil in ["@evil.com/x", ":9@evil.com/"] {
            let joined = format!("{upstream}{evil}");
            let u = reqwest::Url::parse(&joined).expect("仍是合法 URL");
            assert_eq!(
                u.host_str(),
                Some("evil.com"),
                "{joined} 没有换 host —— 这条用例失去意义了"
            );
            assert!(!evil.starts_with('/'), "{evil} 应当被判为非法目标");
        }
        // `//host/x` 接在已有 host 的 URL 后面不会换 host（只是多一层空路径段），
        // 但它是 protocol-relative 形式，同样不是合法的 origin-form，一并挡掉。
        assert!("//evil.com/x".starts_with("//"));
        // 正常路径要放行。
        for ok in ["/v1/messages", "/health", "/"] {
            assert!(ok.starts_with('/') && !ok.starts_with("//"));
        }
    }

    /// 正常的 chunked 请求体还要能读对（drain 之后游标别算错）。
    #[tokio::test]
    async fn a_normal_chunked_body_still_round_trips() {
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut tmp = [0u8; 16 * 1024];
            read_chunked_body(&mut sock, Vec::new(), &mut tmp, MAX_BODY).await
        });
        let mut c = tokio::net::TcpStream::connect(addr).await.unwrap();
        // 三块 + 带扩展的量行 + trailer。
        c.write_all(b"5\r\nhello\r\n3;ext=1\r\n va\r\n2\r\nl!\r\n0\r\n\r\n")
            .await
            .unwrap();
        let body = server.await.unwrap().unwrap();
        assert_eq!(String::from_utf8_lossy(&body), "hello val!");
    }
}

#[cfg(test)]
mod availability_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    /// 内核「晚起」几百毫秒（托管内核自重启、隧道抖一下）时，CLI 必须拿到 200，
    /// 而不是一次 502 让整个 turn 失败、prompt cache 重建。
    ///
    /// 做法：先把端口号定下来但**不监听**，让代理第一次连被拒；300ms 后再真的
    /// 监听。代理的连接重试要把这段空窗吞掉。
    #[tokio::test]
    async fn a_briefly_unreachable_kernel_still_yields_200() {
        // 占一个端口拿到号，然后立刻释放，制造「拒绝连接」的空窗。
        let probe = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let up_port = probe.local_addr().unwrap().port();
        drop(probe);

        // 300ms 后才在同一个端口起上游。
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            let up = TcpListener::bind(("127.0.0.1", up_port)).await.unwrap();
            let (mut s, _) = up.accept().await.unwrap();
            let mut buf = vec![0u8; 8192];
            let _ = s.read(&mut buf).await;
            let body = b"{\"ok\":true}";
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\r\n",
                body.len()
            );
            s.write_all(head.as_bytes()).await.unwrap();
            s.write_all(body).await.unwrap();
        });

        let target = Arc::new(RwLock::new(format!("http://127.0.0.1:{up_port}")));
        let rewrites = Arc::new(RwLock::new(ModelRewrites::new()));
        let records = Arc::new(RwLock::new(Vec::new()));
        let front = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let front_port = front.local_addr().unwrap().port();
        let (t, w, r) = (Arc::clone(&target), Arc::clone(&rewrites), Arc::clone(&records));
        tokio::spawn(async move {
            let (stream, _) = front.accept().await.unwrap();
            let _ = handle_conn(
                stream,
                t,
                w,
                r,
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
                test_proxy_http(),
            )
            .await;
        });

        let mut c = TcpStream::connect(("127.0.0.1", front_port)).await.unwrap();
        let body = br#"{"model":"claude-opus-5","max_tokens":8}"#;
        c.write_all(
            format!(
                "POST /v1/messages HTTP/1.1\r\nHost: x\r\ncontent-length: {}\r\n\r\n",
                body.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
        c.write_all(body).await.unwrap();
        let mut got = Vec::new();
        c.read_to_end(&mut got).await.unwrap();
        let got = String::from_utf8_lossy(&got);
        assert!(got.contains("200 OK"), "空窗没有被重试吞掉，CLI 看到了：{got}");
        assert_eq!(records.read().await.last().map(|r| r.status), Some(200));
    }

    /// 内核真的不在（超过重试预算），要干净地给 502，不能挂死。
    #[tokio::test]
    async fn a_truly_dead_kernel_gets_a_prompt_502() {
        let probe = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let up_port = probe.local_addr().unwrap().port();
        drop(probe);

        let target = Arc::new(RwLock::new(format!("http://127.0.0.1:{up_port}")));
        let rewrites = Arc::new(RwLock::new(ModelRewrites::new()));
        let records = Arc::new(RwLock::new(Vec::new()));
        let front = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let front_port = front.local_addr().unwrap().port();
        let (t, w, r) = (Arc::clone(&target), Arc::clone(&rewrites), Arc::clone(&records));
        tokio::spawn(async move {
            let (stream, _) = front.accept().await.unwrap();
            let _ = handle_conn(
                stream,
                t,
                w,
                r,
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
                test_proxy_http(),
            )
            .await;
        });

        let started = std::time::Instant::now();
        let mut c = TcpStream::connect(("127.0.0.1", front_port)).await.unwrap();
        c.write_all(b"POST /v1/messages HTTP/1.1\r\nHost: x\r\ncontent-length: 2\r\n\r\n{}")
            .await
            .unwrap();
        let mut got = Vec::new();
        c.read_to_end(&mut got).await.unwrap();
        let got = String::from_utf8_lossy(&got);
        assert!(got.contains("502"), "{got}");
        // 3 次退避 200/400/600ms ≈ 1.2s，加连接本身；别超过 5s。
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }
}
