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
//! 第三件事是**首选渠道钉住**（`services::pins`）：别名钉了渠道时，先把模型名换成
//! 那个渠道的私有别名（`grok-4.6@ch21`）发一次；内核回「这条没人接得住」类的失败
//! （见 [`is_fallback_status`]）再用原别名重发，退回内核自己的顺序。内核只按渠道
//! 优先级选路，「选了哪个渠道就默认走它」只能在这一层做。
//!
//! 第四件事是**拒绝接管**（`services::rescue`，默认关）：聊天类响应整体缓冲后判
//! 拒绝，命中就丢弃它（不进 CLI 上下文）并把破甲提示词追加进请求重发内核。判定、
//! 抽取、对上游的可见性分析都在那个模块里，这里只管数据面。
//!
//! 另外内核还给 Codex / pi 开了一条 WebSocket 低延迟通道（`GET /v1/responses` +
//! Upgrade）：这一层对它做**裸透传**（转发握手 + 101 后双向字节拷贝），不做拒绝
//! 接管 —— 那需要攒 WS 帧到 `response.completed` 再判、在同一 socket 上重发，留待
//! 下一步。
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
use crate::services::context_floor::alias_key;
use crate::services::dynamic_inject::{self, InjectConfig};
use crate::services::kernel::{http_client_for_kernel, HttpClientOpts, KernelConfig};
use crate::services::pins::PinRules;
use crate::services::rescue::{self, RescueConfig};

/// 固定端口。CLI 配置里写死的就是它，换端口等于所有接管配置失效，所以不
/// 像 embed_proxy 那样在一个区间里试探 —— 端口被占就是硬错误，得让用户看见。
pub const PROXY_PORT: u16 = 15777;

/// 逐条转发记录里保留多少条。日志页只回看最近的请求，再多就是白占内存。
const MAX_RECORDS: usize = 2000;

/// 连内核失败时重试几次、每次退避多久。三次 × 递增退避共约 1.2s，够托管内核
/// 从 `syscall.Exec` 自重启里回来、也够隧道抖一下；再长就该把错误交给 CLI 了。
const CONNECT_RETRIES: u32 = 3;
const CONNECT_RETRY_BACKOFF: std::time::Duration = std::time::Duration::from_millis(200);
/// 内核回 408（请求体没在 `http_read_timeout_seconds` 内传完）时重发几次。
///
/// 重发是安全的：这个 408 产生在内核**选渠道之前**（`parseIncomingRequest` 里
/// 读 body 就失败了），既没有 attempt 也没有计费，重发不会产生第二次上游调用。
///
/// 只重发一次，不是三次。上行被打满时每次尝试都可能烧掉内核那整段读取超时
/// （默认 120s，实测环境配到 300s），重发三次等于让 CLI 干等一刻钟 —— 那比直接
/// 失败更糟。一次重发换的是「偶发卡顿」那一类，链路真的持续拥塞时就该让它失败，
/// 由人去降并发。
const UPLOAD_RETRIES: u32 = 1;
const UPLOAD_RETRY_BACKOFF: std::time::Duration = std::time::Duration::from_secs(1);
/// WebSocket 握手里等上游回话的上限。上游接了 TCP 却不回响应时不能无限挂着：
/// 客户端早就放弃了，这个任务和两条连接却会一直留着。
const WS_HANDSHAKE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// 请求**还没送达内核**就失败了——这类可以安全重试。
///
/// `is_connect()` 覆盖连接拒绝/握手失败；`is_request()` 且非超时覆盖
/// 「连接池里的旧连接被对端关了」（reqwest 报成 request error）。
/// 超时不算：请求可能已经在内核里跑了，重放等于双倍账单。
fn is_connect_failure(e: &reqwest::Error) -> bool {
    // 握手阶段的失败一律可重试，**包括 connect_timeout 打出来的超时**。这一层只设
    // 了 connect_timeout（整体 timeout 是 None），所以「超时」只可能来自建连；
    // 先判 is_timeout 再判 is_connect 会让 5 秒握不上手的请求直接 502，而上行被
    // 打满时握手慢正是最该重试的一种。
    if e.is_connect() {
        return true;
    }
    if e.is_timeout() {
        return false;
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
    /// 钉住的首选渠道没接住、退让到了下一个名字：记的是**被放弃的**那个私有别名。
    /// None = 第一发就成了（或根本没钉住）。
    #[serde(default)]
    pub fallback_from: Option<String>,
    /// 拒绝接管命中次数：这条转发里丢掉了几次拒绝、重发了几次。0 = 没接管（或关着）。
    #[serde(default)]
    pub rescued: u32,
}

/// 模型名改写规则：CLI 发的名字 -> 内核认的名字。
pub type ModelRewrites = HashMap<String, String>;

/// 代理改模型名要看的全部规则。一把锁装两张表，改哪张都不用换 handle_conn 的签名。
#[derive(Debug, Clone, Default)]
pub struct ProxyRules {
    pub rewrites: ModelRewrites,
    /// 首选渠道钉住，键是 `alias_key`（剥后缀、小写）。
    pub pins: PinRules,
}

/// 内核这次回的状态说明「这个别名没接住」，可以换下一个名字再发。
///
/// 照抄内核自己的分级表（`util/classifier.go`）：401/402/403/429 是 Key 级、5xx 是
/// 渠道级 —— 内核遇到这些会自己换 Key / 换渠道；私有别名只有一个渠道可换，换完就
/// 把状态原样交出来，于是轮到我们换名字。400 / 404 / 413 这些是客户端级：请求本身
/// 有问题（比如上下文太长），换个落点再发一遍只会把同样的错再收一次，还多付一次账。
///
/// 已知代价：内核**自己**回的 401（客户端令牌不对）/ 429（令牌级限流）在这里分不出来，
/// 会用原名再发一次、再收一次同样的错 —— 令牌配错期间每个请求翻倍，但一个字都到不了
/// 上游，没有账单影响；令牌一修好就消失。不按响应体猜「这是内核的还是上游的」：那段
/// 文案每换一版内核都可能变，猜错了退让就悄悄失灵。
pub fn is_fallback_status(status: u16) -> bool {
    matches!(status, 401 | 402 | 403 | 429) || status >= 500
}

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


/// 一条代理连接要用的全部共享状态。收进一个结构体而不是继续往 handle_conn
/// 的参数表里堆 Arc —— 每加一个自动插件就多一个把柄，参数列表早晚会爆
/// clippy 的 too_many_arguments；测试里也只需要 clone 一个东西。
struct ProxyState {
    target: Arc<RwLock<String>>,
    rules: Arc<RwLock<ProxyRules>>,
    records: Arc<RwLock<Vec<ProxyRecord>>>,
    long_cache: Arc<std::sync::atomic::AtomicBool>,
    rescue: Arc<RwLock<RescueConfig>>,
    inject: Arc<RwLock<InjectConfig>>,
    http: Arc<RwLock<reqwest::Client>>,
}

pub struct CliProxy {
    state: Arc<ProxyState>,
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

        let state = Arc::new(ProxyState {
            target: Arc::new(RwLock::new(cfg.base_url())),
            rules: Arc::new(RwLock::new(ProxyRules::default())),
            long_cache: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            // 代理先以「全关」状态起来，`ensure_cli_proxy` 随后会把磁盘上的配置
            // 推进来（和钉住表同一条路径）—— 这里不读文件，是 start() 不知道
            // 配置目录在哪。
            rescue: Arc::new(RwLock::new(RescueConfig::default())),
            inject: Arc::new(RwLock::new(InjectConfig::default())),
            records: Arc::new(RwLock::new(Vec::new())),
            http: Arc::new(RwLock::new(cli_proxy_client(cfg)?)),
        });
        let handle = {
            let st = Arc::clone(&state);
            tokio::spawn(async move {
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
                let st = Arc::clone(&st);
                tokio::spawn(async move {
                    let _ = handle_conn(stream, st).await;
                });
            }
            })
        };

        Ok(Arc::new(Self { state, handle }))
    }

    /// 内核地址变了（切换本地/远端、改端口）时重新指向，不用重启监听。
    pub async fn retarget(&self, cfg: &KernelConfig) -> Result<(), AppError> {
        *self.state.target.write().await = cfg.base_url();
        *self.state.http.write().await = cli_proxy_client(cfg)?;
        Ok(())
    }

    /// 打开/关掉 1 小时缓存窗口。见 `upgrade_cache_ttl` 里的实测数据 ——
    /// 交互式会话开着通常更贵，这个开关是给长间隔的定时任务用的。
    pub fn long_cache_enabled(&self) -> bool {
        self.state.long_cache.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn set_long_cache(&self, on: bool) {
        self.state
            .long_cache
            .store(on, std::sync::atomic::Ordering::Relaxed);
    }

    pub async fn set_rewrites(&self, rules: ModelRewrites) {
        self.state.rules.write().await.rewrites = rules;
    }

    /// 换一整张钉住表。保存 / 删除钉住之后调用，不用重启代理。
    pub async fn set_pins(&self, pins: PinRules) {
        self.state.rules.write().await.pins = pins;
    }

    /// 换拒绝接管配置。保存之后调用，不用重启代理。
    pub async fn set_rescue(&self, cfg: RescueConfig) {
        *self.state.rescue.write().await = cfg.normalized();
    }

    /// 换动态注入配置。保存之后调用，不用重启代理。
    pub async fn set_inject(&self, cfg: InjectConfig) {
        *self.state.inject.write().await = cfg.normalized();
    }

    /// 最近的转发记录，最新的在前。
    pub async fn records(&self) -> Vec<ProxyRecord> {
        let mut v = self.state.records.read().await.clone();
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

/// 这次请求依次要发的模型名。第一个是 CLI 那边改写 / 剥后缀后的名字（和
/// `rewrite_body` 算出来的一致）；钉了渠道就先私有别名、后（可选）原别名。
fn alias_sequence(model: &str, rules: &ProxyRules) -> Vec<String> {
    let plain = resolve_rewrite(model, &rules.rewrites).unwrap_or_else(|| model.to_string());
    match rules.pins.get(&alias_key(&plain)) {
        Some(rule) => rule.sequence(&plain),
        None => vec![plain],
    }
}

/// 把已经改写过的 body 再换一个模型名（重发用）。除 `model` 外一个字节不动，
/// 键序保持（缓存前缀匹配靠它）。解析不了就原样返回。
fn with_model(body: &[u8], model: &str) -> Vec<u8> {
    let Ok(mut v) = serde_json::from_slice::<serde_json::Value>(body) else {
        return body.to_vec();
    };
    if v.get("model").is_none() {
        return body.to_vec();
    }
    v["model"] = serde_json::Value::String(model.to_string());
    serde_json::to_vec(&v).unwrap_or_else(|_| body.to_vec())
}

/// xAI Responses 协议里的 `encrypted_content` 和 `previous_response_id` 是按
/// **签发它们的那个模型**加密 / 绑定的。会话从 grok-4.6 切到 opus-5 之后，Grok CLI
/// 仍会把 250+ 条 grok 的加密思维链原样塞进下一次请求；上游解不开就 400，CLI 把
/// 这个 400 映射成 "conversation history is incompatible with the current model"。
///
/// 只在发往非 grok 家族时剥：留在 grok 上时这两样是思维链和提示缓存的一部分，
/// 剥了会让同模型续写变差。有人类可读 summary 的 reasoning 项留下（摘要文本
/// 换模型之后仍然有用），只带着密文、摘要是空的整条丢掉 —— 空壳 reasoning
/// 上游照样拒。
/// 请求体里带没带上一轮的加密思维链。
///
/// 只做一次子串扫描：绝大多数请求根本没有这东西，为它们各解析一遍几 MB 的
/// JSON 是纯浪费。
fn carries_encrypted_reasoning(body: &[u8]) -> bool {
    const NEEDLE: &[u8] = b"encrypted_content";
    body.windows(NEEDLE.len()).any(|w| w == NEEDLE)
}

/// 上游明说「解不开你带来的 encrypted_content」。
///
/// 实测原文（内核转发的上游 400）：
/// `Could not decrypt the provided encrypted_content. Ensure the value is the
/// unmodified encrypted_content from a previous response.`
fn is_decrypt_failure(body: &[u8]) -> bool {
    let text = String::from_utf8_lossy(body);
    text.contains("Could not decrypt the provided encrypted_content")
        || (text.contains("encrypted_content") && text.contains("decrypt"))
}

fn strip_encrypted_reasoning(body: &[u8]) -> Vec<u8> {
    let Ok(mut v) = serde_json::from_slice::<serde_json::Value>(body) else {
        return body.to_vec();
    };
    let Some(obj) = v.as_object_mut() else {
        return body.to_vec();
    };
    let mut changed = obj.remove("previous_response_id").is_some();
    for key in ["input", "messages"] {
        if let Some(arr) = obj.get_mut(key).and_then(|x| x.as_array_mut()) {
            if strip_foreign_reasoning(arr) {
                changed = true;
            }
        }
    }
    if !changed {
        return body.to_vec();
    }
    serde_json::to_vec(&v).unwrap_or_else(|_| body.to_vec())
}

/// 把 reasoning 条目里的密文拿掉。只剩密文、摘要是空的整条丢掉 —— 空壳
/// reasoning 上游照样拒；有可读摘要的留下，换了上游之后它仍是有用的思维线索。
fn strip_foreign_reasoning(items: &mut Vec<serde_json::Value>) -> bool {
    let before = items.len();
    let mut stripped = false;
    items.retain_mut(|item| {
        let Some(obj) = item.as_object_mut() else {
            return true;
        };
        if obj.get("type").and_then(|t| t.as_str()) != Some("reasoning") {
            return true;
        }
        if obj.remove("encrypted_content").is_some() {
            stripped = true;
        }
        let has_summary = obj.get("summary").and_then(|s| s.as_array()).is_some_and(|parts| {
            parts.iter().any(|p| {
                p.get("text")
                    .and_then(|t| t.as_str())
                    .is_some_and(|t| !t.trim().is_empty())
            })
        });
        has_summary
    });
    stripped || items.len() != before
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

async fn handle_conn(mut client: TcpStream, state: Arc<ProxyState>) -> std::io::Result<()> {
    // 按调用方原来的名字解构，函数体保持原样 —— Arc 引用计数 +1 换来的
    // 是「加插件不动参数表」。
    let (target, rules, records, long_cache, rescue_cfg, inject_cfg, http) = (
        Arc::clone(&state.target),
        Arc::clone(&state.rules),
        Arc::clone(&state.records),
        Arc::clone(&state.long_cache),
        Arc::clone(&state.rescue),
        Arc::clone(&state.inject),
        Arc::clone(&state.http),
    );
    let mut buf: Vec<u8> = Vec::with_capacity(16 * 1024);
    let mut tmp = [0u8; 16 * 1024];
    // 打点在**收到请求**时，不是 send 返回之后 —— 内核日志的时间是
    // attemptStartTime（attempt 开始），会话配对做的是 `内核时间 − 代理时间 ∈
    // [0, 180s]` 的正向配对。以前在 send 之后取值，gap 成了负的 TTFB：
    // 大多数请求配不上自己的日志，转而去抓最近的一条**更晚的**同模型日志
    // —— 那可能是下一个 turn、也可能是别的会话的，成本被悄悄挪了账。
    let started = now_secs();
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

    let upstream = target.read().await.clone();
    let cli = detect_cli(&fwd);
    let session_id = detect_session(&fwd);

    // WebSocket 升级（Codex / pi 的低延迟通道，`GET /v1/responses` + Upgrade）：
    // 握手原样转发，101 之后整个连接归透传管。升级请求没有 body，所以必须在
    // 读 body 之前分走。
    let ws_upgrade = fwd.iter().any(|(k, v)| {
        k.eq_ignore_ascii_case("upgrade")
            && v
                .split(',')
                .any(|t| t.trim().eq_ignore_ascii_case("websocket"))
    });
    if ws_upgrade {
        // 升级请求没有 body，但 CLI 可能把请求头和抢跑的首批帧放在同一个包里发来
        // （低延迟客户端常见且合法）。`buf[header_end..]` 里这批字节属于
        // client→upstream 的字节流，必须一并交给隧道转发上去，否则首帧丢。
        return tunnel_ws(
            &mut client,
            ClientHead {
                head: &buf[..header_end],
                leftover: &buf[header_end..],
            },
            &upstream,
            &records,
            &cli,
            &session_id,
            &path,
        )
        .await;
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

    let rules = rules.read().await.clone();
    let (model, rewritten) = rewrite_body(
        &body_bytes,
        &rules.rewrites,
        long_cache.load(std::sync::atomic::Ordering::Relaxed),
    );
    // out_body 里现在写着的模型名：改写过就是改写后的，否则还是 CLI 的原名。
    let name_in_body: Option<String> = match &rewritten {
        Some(b) => serde_json::from_slice::<serde_json::Value>(b)
            .ok()
            .and_then(|v| v.get("model").and_then(|m| m.as_str()).map(str::to_string)),
        None => model.clone(),
    };
    let out_body = rewritten.unwrap_or(body_bytes);
    // 动态注入（自动插件第二条）是 body 进转发管线的第一步：后面的 fallback
    // 序列、破甲重发全都基于注入后的 body —— 重发的每一跳带上注入内容，链路
    // 才闭环。注入从不报错（不生效就原样过），这里不需要错误分支。
    let out_body = {
        let icfg = inject_cfg.read().await;
        match dynamic_inject::apply(&icfg, &out_body, model.as_deref(), &cli, &path) {
            Some(injected) => injected,
            None => out_body,
        }
    };
    // 钉住序列：没钉就只有一个名字（等于 name_in_body）；没有 model 字段就空，原样发。
    let attempts: Vec<String> = model
        .as_deref()
        .map(|m| alias_sequence(m, &rules))
        .unwrap_or_default();

    let url = format!("{upstream}{path}");
    let Ok(method_parsed) = method.parse::<reqwest::Method>() else {
        return write_simple(&mut client, 400, "bad method").await;
    };
    let http = http.read().await.clone();
    let call = KernelCall {
        method: &method_parsed,
        url: &url,
        fwd: &fwd,
    };

    // 依次试每个名字。首选（私有别名）被内核以「没接住」类状态拒了，就换下一个；
    // 其它状态（成功、或客户端级错误）当场定案。连内核都连不上时换名字没意义，直接
    // 502。每次重发用的都是同一份已缓冲的 body，只换 model 字段。
    let mut sent_model: Option<String> = None;
    let mut fallback_from: Option<String> = None;
    // 定案那一次发出的 body —— 拒绝接管要基于它追加破甲提示词重发（名字、工具、
    // 上下文都跟实际送达内核的那份一致）。
    let mut last_body: Vec<u8> = out_body.clone();
    let mut outcome: Option<Result<reqwest::Response, reqwest::Error>> = None;
    if attempts.is_empty() {
        outcome = Some(send_to_kernel(&http, &call, &out_body).await);
    } else {
        let total = attempts.len();
        for (idx, alias) in attempts.iter().enumerate() {
            last_body = if name_in_body.as_deref() == Some(alias.as_str()) {
                out_body.clone()
            } else {
                with_model(&out_body, alias)
            };
            let r = send_to_kernel(&http, &call, &last_body).await;
            sent_model = Some(alias.clone());
            match &r {
                Ok(resp) if idx + 1 < total && is_fallback_status(resp.status().as_u16()) => {
                    tracing::info!(
                        "cli proxy: {alias} answered {}, falling back to {}",
                        resp.status().as_u16(),
                        attempts[idx + 1]
                    );
                    fallback_from = Some(alias.clone());
                    continue;
                }
                _ => {
                    outcome = Some(r);
                    break;
                }
            }
        }
    }
    // 和 CLI 原名一样就不算改写；记录里 None 表示「没改」。
    let sent_model = sent_model.filter(|n| Some(n.as_str()) != model.as_deref());

    // 拒绝接管（默认关）：聊天类响应整体缓冲查拒绝；命中就丢弃它（不进 CLI 上下文），
    // 把破甲提示词追加进请求重发内核。关着、非聊天路径、或声明长度大到离谱时不缓冲
    // —— 下面那条流式路径原样照走。判定与重发循环见 `rescue_loop`。
    // 用 Stage 枚举而不是 Option<(buf)> + resp：resp 要么被 rescue_loop 消费、要么
    // 留在流式路径，借用检查器才认得出「恰好用一次」。
    enum Stage {
        Stream(reqwest::Response),
        Buf(reqwest::StatusCode, reqwest::header::HeaderMap, Vec<u8>),
    }
    let mut rescued = 0u32;
    let stage: Stage = match outcome.expect("at least one attempt is always made") {
        Err(e) => {
            push_record(
                &records,
                ProxyRecord {
                    time: started,
                    cli,
                    session_id,
                    model,
                    sent_model,
                    path,
                    status: 502,
                    cost: None,
                    output_tokens: None,
                    fallback_from,
                    rescued: 0,
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
        Ok(resp) => {
            let cfg = rescue_cfg.read().await;
            let huge = resp
                .headers()
                .get(reqwest::header::CONTENT_LENGTH)
                .and_then(|v| v.to_str().ok().and_then(|s| s.parse::<usize>().ok()))
                .is_some_and(|n| n > 32 * 1024 * 1024);
            if cfg.enabled
                && method_parsed == reqwest::Method::POST
                && rescue::family(&path).is_some()
                && !huge
            {
                match rescue_loop(&http, &call, &last_body, resp, &cfg, &path).await {
                    Ok((st, hd, bytes, hits)) => {
                        rescued = hits;
                        Stage::Buf(st, hd, bytes)
                    }
                    Err(e) => {
                        // 响应没读完（上游中途断了）：不能伪造一个完整响应，把错误交给 CLI。
                        tracing::warn!("cli proxy: rescue buffer broke: {e}");
                        push_record(
                            &records,
                            ProxyRecord {
                                time: started,
                                cli,
                                session_id,
                                model,
                                sent_model,
                                path,
                                status: 502,
                                cost: None,
                                output_tokens: None,
                                fallback_from,
                                rescued,
                            },
                        )
                        .await;
                        return write_simple(
                            &mut client,
                            502,
                            "response stream broke before completion",
                        )
                        .await;
                    }
                }
            } else {
                Stage::Stream(resp)
            }
        }
    };

    let (status, headers) = match &stage {
        Stage::Buf(s, h, _) => (*s, h.clone()),
        Stage::Stream(r) => (r.status(), r.headers().clone()),
    };

    push_record(
        &records,
        ProxyRecord {
            time: started,
            cli,
            session_id,
            model,
            sent_model,
            path,
            status: status.as_u16(),
            cost: None,
            output_tokens: None,
            fallback_from,
            rescued,
        },
    )
    .await;

    let mut head = format!("HTTP/1.1 {status}\r\n");
    for (name, value) in &headers {
        let lower = name.as_str().to_ascii_lowercase();
        if lower == "transfer-encoding" || lower == "connection" {
            continue;
        }
        if let Ok(vs) = value.to_str() {
            head.push_str(&format!("{name}: {vs}\r\n"));
        }
    }
    if let Stage::Buf(_, _, bytes) = &stage {
        // 整份响应：长度已知，直接挂 Content-Length 一次写完，不重分块。
        head.push_str(&format!("content-length: {}\r\n", bytes.len()));
        head.push_str("Connection: close\r\n\r\n");
        client.write_all(head.as_bytes()).await?;
        client.write_all(bytes).await?;
        return client.flush().await;
    }
    let resp = match stage {
        Stage::Buf(..) => unreachable!("buffered stage returned above"),
        Stage::Stream(resp) => resp,
    };
    let has_len = headers.get(reqwest::header::CONTENT_LENGTH).is_some();
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

/// 发一次请求到内核，带「连不上就重试」。
///
/// 和内核同一个宗旨：**能重试的绝不直接抛错给 CLI**。内核对上游是 Key →
/// 模型 → 渠道三级冷却切换；到了这一层，唯一能透明兜住的是「连内核这一跳」
/// 的瞬时失败——托管内核刚重启、远端隧道抖了一下、连接池里的旧连接被对端
/// 关了。这些都是几百毫秒内自愈的事，一次就 502 会让 Claude Code 把整个
/// turn 判失败，用户看到红字重发，prompt cache 还得重建。
///
/// 只重试**请求还没发出去**就失败的情况（连不上、握手失败）：请求体已经进
/// 了内核就不能再发第二遍——那是内核自己的故障转移在管的事，重放会让一次
/// 请求变两次账单。
/// 一次内核调用的固定部分：fallback 序列、rescue 重发改的只有 body。
struct KernelCall<'a> {
    method: &'a reqwest::Method,
    url: &'a str,
    fwd: &'a [(String, String)],
}

async fn send_to_kernel(
    http: &reqwest::Client,
    call: &KernelCall<'_>,
    body: &[u8],
) -> Result<reqwest::Response, reqwest::Error> {
    // 可能被剥掉密文后重发，所以要有自己的一份。
    let mut body: Vec<u8> = body.to_vec();
    let mut upload_retried = 0u32;
    let mut decrypt_retried = false;
    loop {
        let resp = send_once(http, call, &body).await?;
        let status = resp.status();

        // 408 = 内核没能在读取超时内把请求体读完。它发生在选渠道之前，没有上游
        // 调用、没有计费，所以重发是干净的；而对 CLI 来说这一轮本来就是死的。
        if status == reqwest::StatusCode::REQUEST_TIMEOUT
            && upload_retried < UPLOAD_RETRIES
            && !body.is_empty()
        {
            upload_retried += 1;
            tracing::info!(
                "cli proxy: kernel could not finish reading the body ({} bytes), resending {upload_retried}/{UPLOAD_RETRIES}",
                body.len()
            );
            tokio::time::sleep(UPLOAD_RETRY_BACKOFF).await;
            continue;
        }

        // 400 且请求里带着上一轮的加密思维链：多半是「这一发落到了另一家上游，
        // 而密文是上一家签发的」。实测原文就是 "Could not decrypt the provided
        // encrypted_content"，CLI 会把它显示成「会话历史与当前模型不兼容，请新开
        // 会话」，一整条长会话就此报废。
        //
        // 为什么只能事后重发、不能事前剥：签发方和这一发的落点都由内核按优先级
        // 和冷却状态临时决定（钉住的私有别名 429 之后会退到别的渠道），代理这一层
        // 根本不知道会落到谁家。按「目标模型家族」猜是错的 —— 实测 claude-opus-5
        // 的请求落到过 xAI 渠道。所以等上游明确说「解不开」再剥，剥完重发一次。
        //
        // 剥掉的代价是这一轮丢掉思维链续写；不剥的代价是整条会话再也发不出去。
        if status == reqwest::StatusCode::BAD_REQUEST
            && !decrypt_retried
            && carries_encrypted_reasoning(&body)
        {
            decrypt_retried = true;
            let bytes = resp.bytes().await?;
            if is_decrypt_failure(&bytes) {
                tracing::info!(
                    "cli proxy: upstream could not decrypt the carried reasoning, resending without it"
                );
                body = strip_encrypted_reasoning(&body);
            }
            // 不是这个错的话，响应体已经被读掉、没法还给调用方了，就原样再发一次
            // 把新的响应交出去。400 是被拒的请求，没有生成任何 token，重发不计费。
            continue;
        }
        return Ok(resp);
    }
}

/// 发一次，只在连接级失败时重试（还没拿到任何状态，重试不会重复任何已发生的事）。
async fn send_once(
    http: &reqwest::Client,
    call: &KernelCall<'_>,
    body: &[u8],
) -> Result<reqwest::Response, reqwest::Error> {
    let mut attempt = 0u32;
    loop {
        let mut r = http.request(call.method.clone(), call.url);
        for (name, value) in call.fwd {
            if let Ok(hv) = reqwest::header::HeaderValue::from_str(value) {
                r = r.header(name.as_str(), hv);
            }
        }
        if !body.is_empty() {
            r = r.header(reqwest::header::CONTENT_LENGTH, body.len());
            r = r.body(body.to_vec());
        }
        match r.send().await {
            Ok(resp) => return Ok(resp),
            Err(e) if attempt < CONNECT_RETRIES && is_connect_failure(&e) => {
                attempt += 1;
                tracing::debug!("cli proxy: connect to kernel failed, retry {attempt}: {e}");
                tokio::time::sleep(CONNECT_RETRY_BACKOFF * attempt).await;
            }
            Err(e) => return Err(e),
        }
    }
}

/// 拒绝接管：把第一个响应读到结尾，判成拒绝就丢弃，把破甲提示词追加进**原请求**
/// 重发内核。循环到不再拒绝或达到重试上限。返回最终响应的
/// （状态、头、字节、命中次数）。
///
/// 重发本身失败（非 2xx 或连接断）：保留原拒绝 —— CLI 看见「某个东西」好过
/// 什么都没有，而且此时原拒绝已经读完，丢弃它反而让这轮彻底没结果。
/// 判定、抽取、追加的形状与理由都在 `services::rescue`。
async fn rescue_loop(
    http: &reqwest::Client,
    call: &KernelCall<'_>,
    last_body: &[u8],
    first: reqwest::Response,
    cfg: &RescueConfig,
    path: &str,
) -> Result<(reqwest::StatusCode, reqwest::header::HeaderMap, Vec<u8>, u32), reqwest::Error> {
    let mut status = first.status();
    let mut headers = first.headers().clone();
    let mut bytes = first.bytes().await?.to_vec();
    let max = cfg.max_retries.clamp(1, 5);
    let mut hits = 0u32;
    for _ in 0..max {
        if !status.is_success() {
            break;
        }
        let Some(text) = rescue::extract_text(path, &bytes) else {
            break;
        };
        if text.trim().is_empty() || !rescue::looks_like_refusal(&text, &cfg.markers) {
            break;
        }
        let Some(next_body) = rescue::append_user_turn(path, last_body, cfg.effective_prompt())
        else {
            break;
        };
        tracing::info!("cli proxy: refusal detected, re-sending with the armor prompt (hit {hits})");
        let r = send_to_kernel(http, call, &next_body).await?;
        let st = r.status();
        if !st.is_success() {
            tracing::warn!("cli proxy: rescue re-send answered {st}, keeping the original refusal");
            break;
        }
        status = st;
        headers = r.headers().clone();
        bytes = r.bytes().await?.to_vec();
        hits += 1;
    }
    Ok((status, headers, bytes, hits))
}

/// WS 透传的上游端：明文 TCP 或自开的 TLS。手写的委托实现 —— tokio 没有
/// 现成的「读+写」复合对象类型，而 `copy_bidirectional` 需要两个完整端点。
enum WsUpstream {
    Plain(TcpStream),
    // Box：TlsStream 里装着整个 rustls 会话，裸放会让每条 WS 透传多占一 KiB。
    Tls(Box<tokio_rustls::client::TlsStream<TcpStream>>),
}

impl tokio::io::AsyncRead for WsUpstream {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            WsUpstream::Plain(s) => {
                tokio::io::AsyncRead::poll_read(std::pin::Pin::new(s), cx, buf)
            }
            WsUpstream::Tls(s) => {
                tokio::io::AsyncRead::poll_read(std::pin::Pin::new(&mut **s), cx, buf)
            }
        }
    }
}

impl tokio::io::AsyncWrite for WsUpstream {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        match self.get_mut() {
            WsUpstream::Plain(s) => {
                tokio::io::AsyncWrite::poll_write(std::pin::Pin::new(s), cx, buf)
            }
            WsUpstream::Tls(s) => {
                tokio::io::AsyncWrite::poll_write(std::pin::Pin::new(&mut **s), cx, buf)
            }
        }
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            WsUpstream::Plain(s) => {
                tokio::io::AsyncWrite::poll_flush(std::pin::Pin::new(s), cx)
            }
            WsUpstream::Tls(s) => {
                tokio::io::AsyncWrite::poll_flush(std::pin::Pin::new(&mut **s), cx)
            }
        }
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            WsUpstream::Plain(s) => {
                tokio::io::AsyncWrite::poll_shutdown(std::pin::Pin::new(s), cx)
            }
            WsUpstream::Tls(s) => {
                tokio::io::AsyncWrite::poll_shutdown(std::pin::Pin::new(&mut **s), cx)
            }
        }
    }
}

/// WebSocket 升级的裸透传：握手原样转发，上游回 101 之后两端字节流互拷到断开。
///
/// 只对 WS 连接做「接两截」：不解析帧、不做拒绝接管 —— WS Responses 协议上的
/// 接管要把帧攒到 `response.completed` 再判、还要在同一 socket 上发新的
/// `response.create` 重发，是协议级的活，先保证「能用」。
///
/// `req_head` 是 CLI 发来的请求头原始字节，只重写 Host（目标可能是远端内核），
/// 其余原样 —— 令牌照旧让上游鉴权。已知边界：远端内核配了出口代理（socks/http）
/// 时隧道不走那个代理，是裸连接；连不直就 502，不做静默黑洞。
/// 拒绝接管对 WS 不可用的说明见模块头注释。
/// 非 101 响应的 body 分帧（RFC 7230 §3.3.3）。三种要分开对待：按错一种，轻则
/// 客户端挂着等尾巴，重则把截断当成完整。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BodyFraming {
    /// 1xx / 204 / 304：按状态码就没有 body，头后面什么都别等。
    Empty,
    /// Content-Length：读够声明的字节数。
    Length(usize),
    /// Transfer-Encoding 以 chunked 结尾：按块边界找到终止块。
    Chunked,
    /// 两种分帧头都没有（或 Transfer-Encoding 不是 chunked）：读到上游关闭为止。
    UntilClose,
}

fn response_framing(status: u16, head_lc: &str) -> BodyFraming {
    if (100..200).contains(&status) || status == 204 || status == 304 {
        return BodyFraming::Empty;
    }
    // Transfer-Encoding 在场时 Content-Length 作废（§3.3.3 第 3 条）。
    if let Some(te) = header_value(head_lc, "transfer-encoding") {
        return if te.rsplit(',').next().is_some_and(|t| t.trim() == "chunked") {
            BodyFraming::Chunked
        } else {
            BodyFraming::UntilClose
        };
    }
    match header_value(head_lc, "content-length").map(|v| v.parse::<usize>()) {
        Some(Ok(n)) => BodyFraming::Length(n),
        // 写了个解析不了的 Content-Length：不能当已知长度，只能读到关闭。
        Some(Err(_)) | None => BodyFraming::UntilClose,
    }
}

/// 头块（已转小写）里某个字段的值，只取第一处。字段名必须顶着行首，
/// 免得 `x-content-length:` 之类误中。
fn header_value<'a>(head_lc: &'a str, name: &str) -> Option<&'a str> {
    head_lc.split("\r\n").skip(1).find_map(|line| {
        let value = line.strip_prefix(name)?.strip_prefix(':')?;
        Some(value.trim())
    })
}

/// 把响应头里的 Connection 改成 close。非 101 的连接在本条响应之后一定会关，
/// 上游写的 keep-alive 在这里是假话；两种分帧头都没有时它同时是「读到关闭为止」
/// 的明示。分帧头（Content-Length / Transfer-Encoding）原样保留。
fn force_connection_close(head: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(head);
    let mut out = String::with_capacity(text.len() + 24);
    for line in text.trim_end_matches("\r\n").split("\r\n") {
        let is_connection = line
            .get(..11)
            .is_some_and(|p| p.eq_ignore_ascii_case("connection:"));
        if is_connection {
            continue;
        }
        out.push_str(line);
        out.push_str("\r\n");
    }
    out.push_str("Connection: close\r\n\r\n");
    out.into_bytes()
}

/// chunked 消息体的增量扫描器。字节本身原样转发（透传不解码），扫描只为找到
/// 这条消息在字节流里的结束位置；块长度行、块尾 CRLF、终止块和 trailer 的边界
/// 落在哪次读里都无所谓。
#[derive(Default)]
struct ChunkedScanner {
    state: ChunkState,
    /// 正在积累的那一行（块长度行 / 块尾 CRLF / trailer 行），不含换行。
    line: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum ChunkState {
    /// 在读块长度行。
    #[default]
    Size,
    /// 在块数据里，还剩这么多字节。
    Data(usize),
    /// 块数据读完了，等它后面那对 CRLF。
    DataEnd,
    /// 终止块之后：读 trailer 行，直到空行。
    Trailers,
    Done,
    /// 格式不对，找不到结尾了。
    Broken,
}

impl ChunkedScanner {
    /// 单行上限：正常的块长度行只有几个字节，超过这个数一定不是 chunked。
    const MAX_LINE: usize = 8 * 1024;

    /// 喂进一段字节，返回其中属于本条消息的字节数（含终止序列）。没结束时就是
    /// 全部；结束后多出来的不属于这条响应。
    fn feed(&mut self, buf: &[u8]) -> usize {
        let mut i = 0;
        while i < buf.len() {
            match self.state {
                ChunkState::Done | ChunkState::Broken => break,
                ChunkState::Data(remaining) => {
                    let take = remaining.min(buf.len() - i);
                    i += take;
                    self.state = if take == remaining {
                        ChunkState::DataEnd
                    } else {
                        ChunkState::Data(remaining - take)
                    };
                }
                ChunkState::Size | ChunkState::DataEnd | ChunkState::Trailers => {
                    let b = buf[i];
                    i += 1;
                    if b != b'\n' {
                        if self.line.len() >= Self::MAX_LINE {
                            self.state = ChunkState::Broken;
                            break;
                        }
                        self.line.push(b);
                        continue;
                    }
                    if self.line.last() == Some(&b'\r') {
                        self.line.pop();
                    }
                    let line = std::mem::take(&mut self.line);
                    self.state = match self.state {
                        ChunkState::Size => {
                            // 块长度是十六进制，后面可以带 `;ext=val` 扩展。
                            let hex = line.split(|&c| c == b';').next().unwrap_or(&[]);
                            match std::str::from_utf8(hex)
                                .ok()
                                .and_then(|h| usize::from_str_radix(h.trim(), 16).ok())
                            {
                                Some(0) => ChunkState::Trailers,
                                Some(n) => ChunkState::Data(n),
                                None => ChunkState::Broken,
                            }
                        }
                        ChunkState::DataEnd if line.is_empty() => ChunkState::Size,
                        ChunkState::DataEnd => ChunkState::Broken,
                        _ if line.is_empty() => ChunkState::Done,
                        _ => ChunkState::Trailers,
                    };
                }
            }
        }
        i
    }

    fn is_done(&self) -> bool {
        self.state == ChunkState::Done
    }

    fn is_broken(&self) -> bool {
        self.state == ChunkState::Broken
    }
}

/// CLI 发来的升级请求：原始请求头，加上读头时一并读进来的、位于头之后的字节
/// （管线化的抢跑帧）。后者属于 client→upstream 流，握手转发后要立刻补给上游。
struct ClientHead<'a> {
    head: &'a [u8],
    leftover: &'a [u8],
}

async fn tunnel_ws(
    client: &mut TcpStream,
    req: ClientHead<'_>,
    upstream: &str,
    records: &Arc<RwLock<Vec<ProxyRecord>>>,
    cli: &str,
    session_id: &Option<String>,
    path: &str,
) -> std::io::Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let url = reqwest::Url::parse(upstream)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let host = url
        .host_str()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "no host in target"))?
        .to_string();
    let port = url.port_or_known_default().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, "no port in target")
    })?;
    let is_tls = url.scheme() == "https";

    // 和 send_to_kernel 同一个重试口径：连接级失败才重试。
    let mut attempt = 0u32;
    let mut up = loop {
        let attempt_res = async {
            let conn = TcpStream::connect((host.as_str(), port)).await?;
            if !is_tls {
                return Ok(WsUpstream::Plain(conn));
            }
            // SNI 必须带上（远端内核常挂在共享证书前面）：域名直接参与握手。
            let name = rustls::pki_types::ServerName::try_from(host.clone())
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
            let mut roots = rustls::RootCertStore::empty();
            roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            let cfg = rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth();
            let connector = tokio_rustls::TlsConnector::from(std::sync::Arc::new(cfg));
            // 握手在这里就驱动完：证书不信任、握手被掐，都算连接失败进同一个重试，
            // 而不是留一个半成品 future 给后面的透传突然爆掉。
            Ok::<WsUpstream, std::io::Error>(WsUpstream::Tls(Box::new(
                connector.connect(name, conn).await?,
            )))
        };
        let ws = match attempt_res.await {
            Ok(ws) => ws,
            Err(e) if attempt < CONNECT_RETRIES => {
                attempt += 1;
                tracing::debug!("cli proxy: ws connect failed, retry {attempt}: {e}");
                tokio::time::sleep(CONNECT_RETRY_BACKOFF * attempt).await;
                continue;
            }
            Err(e) => {
                return write_simple(
                    client,
                    502,
                    &format!("kernel unreachable ({upstream}): {e}"),
                )
                .await;
            }
        };
        break ws;
    };

    // Host 按上游重算：逐行、只认字段名顶格的 Host。子串找 "host:" 会撞上
    // `Origin: http://localhost:15777` 这类值 —— 改错行、真正的 Host 还指着代理，
    // 远端内核前面的反代按 Host 分流就会 404。其余头原样、顺序不动；客户端没带
    // Host（RFC 上允许但 CLI 都会带）时等于补一条。
    let head = String::from_utf8_lossy(req.head);
    let mut lines = head.trim_end_matches("\r\n").split("\r\n");
    let mut fwd_head = String::with_capacity(head.len() + 48);
    fwd_head.push_str(lines.next().unwrap_or_default());
    fwd_head.push_str(&format!("\r\nHost: {host}:{port}\r\n"));
    for line in lines {
        let is_host = line
            .get(..5)
            .is_some_and(|name| name.eq_ignore_ascii_case("host:"));
        if is_host {
            continue;
        }
        fwd_head.push_str(line);
        fwd_head.push_str("\r\n");
    }
    fwd_head.push_str("\r\n");
    up.write_all(fwd_head.as_bytes()).await?;
    // 抢跑的客户端字节紧跟在请求头之后补给上游 —— 它们是同一条 client→upstream
    // 字节流的一部分，漏掉就是首帧丢失。
    if !req.leftover.is_empty() {
        up.write_all(req.leftover).await?;
    }
    up.flush().await?;

    // 读上游对握手的回答。101 = 升级成功，进透传；其它 = 把这个回答当普通响应
    // 原样交给 CLI（通常是 401/404，CLI 自己能看懂）。
    let mut rbuf: Vec<u8> = Vec::with_capacity(8 * 1024);
    let mut rtmp = [0u8; 8 * 1024];
    let handshake_deadline = tokio::time::Instant::now() + WS_HANDSHAKE_TIMEOUT;
    let head_end = loop {
        let n = match tokio::time::timeout_at(handshake_deadline, up.read(&mut rtmp)).await {
            Ok(read) => read?,
            Err(_) => {
                return write_simple(
                    client,
                    504,
                    "kernel did not answer the websocket handshake in time",
                )
                .await;
            }
        };
        if n == 0 {
            return write_simple(
                client,
                502,
                "kernel closed the connection during the websocket handshake",
            )
            .await;
        }
        rbuf.extend_from_slice(&rtmp[..n]);
        if let Some(p) = find_header_end(&rbuf) {
            break p;
        }
        if rbuf.len() > 128 * 1024 {
            return write_simple(client, 502, "upstream headers too large").await;
        }
    };
    let status_line = {
        let e = rbuf.windows(2).position(|w| w == b"\r\n").unwrap_or(rbuf.len());
        String::from_utf8_lossy(&rbuf[..e]).to_string()
    };
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(502);
    let started = now_secs();
    let record = |status: u16| ProxyRecord {
        time: started,
        cli: cli.to_string(),
        session_id: session_id.clone(),
        model: None,
        sent_model: None,
        path: path.to_string(),
        status,
        cost: None,
        output_tokens: None,
        fallback_from: None,
        rescued: 0,
    };

    if status != 101 {
        // 非 101：整条当普通响应转给 CLI，然后结束这条连接。头原样转发，保住上游
        // 自己的分帧头，只把 Connection 改成 close —— 这条连接确实会在本条响应后
        // 关掉。body 按 RFC 7230 §3.3.3 分三路补全：只按 Content-Length 补会让
        // chunked 响应挂到上游空闲超时（Go 的 net/http 默认 keep-alive，响应完了
        // 不关连接），也会把两种分帧头都没有的 close-delimited 响应当成已经完整。
        let head_lc = String::from_utf8_lossy(&rbuf[..head_end]).to_ascii_lowercase();
        let framing = response_framing(status, &head_lc);
        client.write_all(&force_connection_close(&rbuf[..head_end])).await?;
        match framing {
            BodyFraming::Empty => {}
            // 有 Content-Length：按声明长度把 body 补全再转，多读到的字节截掉不外泄，
            // 缺的继续读，绝不留半截。
            BodyFraming::Length(total) => {
                let buffered = &rbuf[head_end..];
                let take = buffered.len().min(total);
                client.write_all(&buffered[..take]).await?;
                let mut have = take;
                while have < total {
                    let n = up.read(&mut rtmp).await?;
                    if n == 0 {
                        break; // 上游提前断，客户端会看到截断而不是无限等待
                    }
                    let want = (total - have).min(n);
                    client.write_all(&rtmp[..want]).await?;
                    have += want;
                }
            }
            // chunked：字节原样透传，只扫块边界找终止块；到了就收尾，不等上游关。
            BodyFraming::Chunked => {
                let mut scan = ChunkedScanner::default();
                let mut pending: &[u8] = &rbuf[head_end..];
                loop {
                    let take = scan.feed(pending);
                    client.write_all(&pending[..take]).await?;
                    if scan.is_done() {
                        break;
                    }
                    if scan.is_broken() {
                        // 分块格式坏了就找不到结尾：剩下的当 close-delimited 直通，
                        // 客户端会用同一套规则自己报错。
                        client.write_all(&pending[take..]).await?;
                        tokio::io::copy(&mut up, client).await?;
                        break;
                    }
                    let n = up.read(&mut rtmp).await?;
                    if n == 0 {
                        break;
                    }
                    pending = &rtmp[..n];
                }
            }
            // 两种分帧头都没有：close-delimited，直通到上游关闭；头里已经明示
            // Connection: close，客户端也按「读到关闭为止」理解。
            BodyFraming::UntilClose => {
                client.write_all(&rbuf[head_end..]).await?;
                tokio::io::copy(&mut up, client).await?;
            }
        }
        push_record(records, record(status)).await;
        return client.flush().await;
    }

    // 101：上游的握手响应**原样**转给客户端 —— Sec-WebSocket-Accept 客户端库会校验，
    // 缺了直接判握手失败；Sec-WebSocket-Extensions / -Protocol 是协商结果，丢了
    // 客户端就会拿「没压缩」的预期去解上游的压缩帧。写死一行 101 这些全没了。
    // 读头时连着读进来的上游抢跑首帧（`rbuf[head_end..]`）也在这一笔里一起给出去，
    // 否则这批字节留在缓冲里再也发不出去。
    client.write_all(&rbuf).await?;
    client.flush().await?;
    push_record(records, record(101)).await;
    let res = tokio::io::copy_bidirectional(client, &mut up).await;
    // 透传结束后把连接关掉（101 之后没有 HTTP 语义，留着是死连接）。
    let _ = client.shutdown().await;
    res.map(|_| ())
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
        let rewrites = Arc::new(RwLock::new(ProxyRules::default()));
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
            handle_conn(
                stream,
                Arc::new(ProxyState {
                    target: t,
                    rules: w,
                    records: r,
                    long_cache: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    rescue: Arc::new(RwLock::new(RescueConfig::default())),
                    inject: Arc::new(RwLock::new(InjectConfig::default())),
                    http: test_proxy_http(),
                }),
            )
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
                Arc::new(ProxyState {
                    target,
                    rules: Arc::new(RwLock::new(ProxyRules::default())),
                    records: Arc::new(RwLock::new(Vec::new())),
                    long_cache: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    rescue: Arc::new(RwLock::new(RescueConfig::default())),
                    inject: Arc::new(RwLock::new(InjectConfig::default())),
                    http: test_proxy_http(),
                }),
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
                Arc::new(ProxyState {
                    target,
                    rules: Arc::new(RwLock::new(ProxyRules::default())),
                    records: Arc::new(RwLock::new(Vec::new())),
                    long_cache: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    rescue: Arc::new(RwLock::new(RescueConfig::default())),
                    inject: Arc::new(RwLock::new(InjectConfig::default())),
                    http: test_proxy_http(),
                }),
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
        let rewrites = Arc::new(RwLock::new(ProxyRules::default()));
        let records = Arc::new(RwLock::new(Vec::new()));
        let front = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let front_port = front.local_addr().unwrap().port();
        let (t, w, r) = (Arc::clone(&target), Arc::clone(&rewrites), Arc::clone(&records));
        tokio::spawn(async move {
            let (stream, _) = front.accept().await.unwrap();
            let _ = handle_conn(
                stream,
                Arc::new(ProxyState {
                    target: t,
                    rules: w,
                    records: r,
                    long_cache: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    rescue: Arc::new(RwLock::new(RescueConfig::default())),
                    inject: Arc::new(RwLock::new(InjectConfig::default())),
                    http: test_proxy_http(),
                }),
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
        let rewrites = Arc::new(RwLock::new(ProxyRules::default()));
        let records = Arc::new(RwLock::new(Vec::new()));
        let front = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let front_port = front.local_addr().unwrap().port();
        let (t, w, r) = (Arc::clone(&target), Arc::clone(&rewrites), Arc::clone(&records));
        tokio::spawn(async move {
            let (stream, _) = front.accept().await.unwrap();
            let _ = handle_conn(
                stream,
                Arc::new(ProxyState {
                    target: t,
                    rules: w,
                    records: r,
                    long_cache: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    rescue: Arc::new(RwLock::new(RescueConfig::default())),
                    inject: Arc::new(RwLock::new(InjectConfig::default())),
                    http: test_proxy_http(),
                }),
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

#[cfg(test)]
mod pin_tests {
    use super::*;
    use crate::services::pins::PinRule;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn rules_with_pin(alias: &str, channels: &[i64], fallback: bool) -> ProxyRules {
        let mut rules = ProxyRules::default();
        rules.pins.insert(
            alias_key(alias),
            PinRule {
                base: alias.to_string(),
                channels: channels.to_vec(),
                fallback,
            },
        );
        rules
    }

    /// 序列的形状：没钉住就一个名字（剥完后缀）；钉了就私有别名在前、原名在后；
    /// 不退让时没有原名；显式改写表的结果才是查钉住表的键。
    #[test]
    fn alias_sequence_follows_the_pin_rule() {
        assert_eq!(alias_sequence("claude-opus-5[1m]", &ProxyRules::default()), vec!["claude-opus-5"]);

        let rules = rules_with_pin("grok-4.6", &[21], true);
        assert_eq!(alias_sequence("grok-4.6", &rules), vec!["grok-4.6@ch21", "grok-4.6"]);
        // 请求里带后缀 / 大小写不同：查得到同一条规则，私有别名用钉住表里的大小写
        // （和内核条目一致），退让用的是剥完窗口后缀的原名。
        assert_eq!(alias_sequence("Grok-4.6[1m]", &rules), vec!["grok-4.6@ch21", "Grok-4.6"]);

        let strict = rules_with_pin("grok-4.6", &[21], false);
        assert_eq!(alias_sequence("grok-4.6", &strict), vec!["grok-4.6@ch21"]);

        let mut mapped = rules_with_pin("glm-5.3-flash", &[17], true);
        mapped.rewrites.insert("my-alias".into(), "glm-5.3-flash".into());
        assert_eq!(alias_sequence("my-alias", &mapped), vec!["glm-5.3-flash@ch17", "glm-5.3-flash"]);

        // 内核的 thinking 后缀 `(max)` 不能让钉住失效，而且要跟到私有别名后面去。
        let thinking = rules_with_pin("gpt-5.6", &[9], true);
        assert_eq!(
            alias_sequence("gpt-5.6(max)", &thinking),
            vec!["gpt-5.6@ch9(max)", "gpt-5.6(max)"]
        );
    }

    /// 照抄内核分级：Key 级（401/402/403/429）和渠道级（5xx）才换名字；客户端级不换。
    #[test]
    fn fallback_statuses_mirror_the_kernel_classifier() {
        for s in [401, 402, 403, 429, 500, 502, 503, 504, 520, 524] {
            assert!(is_fallback_status(s), "{s} 应当退让");
        }
        for s in [200, 201, 400, 404, 408, 413, 422] {
            assert!(!is_fallback_status(s), "{s} 不该退让");
        }
    }

    /// 剥离是无条件的：密文拿掉、previous_response_id 拿掉，空摘要的 reasoning
    /// 整条丢掉，有摘要的留下（换了上游之后摘要仍是可读的思维线索）。
    #[test]
    fn stripping_removes_carried_reasoning() {
        let body = serde_json::json!({
            "model": "claude-opus-5",
            "previous_response_id": "resp_abc",
            "input": [
                {"type": "message", "role": "user", "content": "hi"},
                {
                    "type": "reasoning",
                    "id": "rs_msg_011xxx",
                    "summary": [{"type": "summary_text", "text": "prior plan"}],
                    "encrypted_content": "deadbeef"
                },
                {
                    "type": "reasoning",
                    "id": "rs_msg_011yyy",
                    "summary": [{"type": "summary_text", "text": ""}],
                    "encrypted_content": "aa"
                }
            ]
        });
        let out: serde_json::Value =
            serde_json::from_slice(&strip_encrypted_reasoning(&serde_json::to_vec(&body).unwrap()))
                .unwrap();
        assert!(out.get("previous_response_id").is_none());
        let input = out["input"].as_array().unwrap();
        assert_eq!(input.len(), 2, "空摘要的 reasoning 应整条丢掉: {input:?}");
        assert_eq!(input[0]["type"], "message");
        assert_eq!(input[1]["summary"][0]["text"], "prior plan");
        assert!(input[1].get("encrypted_content").is_none());
    }

    /// 两个探测器：没带密文的请求一个字节都不该被碰，上游那句话要认得出来。
    #[test]
    fn detectors_recognise_the_upstream_decrypt_error() {
        assert!(!carries_encrypted_reasoning(br#"{"model":"x","messages":[]}"#));
        assert!(carries_encrypted_reasoning(
            br#"{"input":[{"type":"reasoning","encrypted_content":"aa"}]}"#
        ));

        // 实测原文。
        assert!(is_decrypt_failure(
            br#"{"code":"invalid-argument","error":"Could not decrypt the provided encrypted_content. Ensure the value is the unmodified encrypted_content from a previous response."}"#
        ));
        // 别的 400 不能误判成它 —— 误判会白白剥掉一轮思维链。
        assert!(!is_decrypt_failure(
            br#"{"error":"This model's maximum prompt length is 500000 but the request contains 517306 tokens."}"#
        ));
    }

    /// 端到端：上游说「解不开你带来的密文」时，代理剥掉密文重发一次，CLI 只看到
    /// 最终那个 200。不修的话 Grok CLI 会把这个 400 显示成「会话历史与当前模型
    /// 不兼容，请新开会话」，一条长会话就此报废。
    #[tokio::test]
    async fn an_undecryptable_reasoning_blob_is_stripped_and_resent() {
        let up = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let up_port = up.local_addr().unwrap().port();
        // 每一发有没有带密文，按顺序记下来。
        let carried: Arc<RwLock<Vec<bool>>> = Arc::new(RwLock::new(Vec::new()));
        let seen = Arc::clone(&carried);
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = up.accept().await else { break };
                let seen = Arc::clone(&seen);
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut tmp = [0u8; 8192];
                    let head_end = loop {
                        let n = sock.read(&mut tmp).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                        if let Some(p) = find_header_end(&buf) {
                            break p;
                        }
                    };
                    let head = String::from_utf8_lossy(&buf[..head_end]).to_ascii_lowercase();
                    let len: usize = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .and_then(|v| v.trim().parse().ok())
                        .unwrap_or(0);
                    while buf.len() < head_end + len {
                        let n = sock.read(&mut tmp).await.unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                    }
                    let has = carries_encrypted_reasoning(&buf[head_end..]);
                    let nth = {
                        let mut v = seen.write().await;
                        v.push(has);
                        v.len()
                    };
                    let (status, out) = if nth == 1 {
                        (400, r#"{"code":"invalid-argument","error":"Could not decrypt the provided encrypted_content. Ensure the value is the unmodified encrypted_content from a previous response."}"#.to_string())
                    } else {
                        (200, r#"{"ok":true}"#.to_string())
                    };
                    let resp = format!(
                        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{out}",
                        out.len()
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                    let _ = sock.flush().await;
                });
            }
        });

        let body = br#"{"model":"grok-4.6","input":[{"type":"reasoning","id":"rs_1","summary":[{"type":"summary_text","text":""}],"encrypted_content":"AAAA"},{"type":"message","role":"user","content":"hi"}]}"#;
        let (got, recs) = run_through_proxy(up_port, ProxyRules::default(), body).await;

        assert!(got.starts_with("HTTP/1.1 200"), "CLI 应当只看到重发后的成功：{got}");
        let carried = carried.read().await.clone();
        assert_eq!(carried.len(), 2, "应当正好重发一次：{carried:?}");
        assert!(carried[0], "第一发本来就带着密文");
        assert!(!carried[1], "重发那一次必须已经把密文剥掉了");
        assert_eq!(recs.len(), 1, "记录里只留最终结果");
        assert_eq!(recs[0].status, 200);
    }

    /// 重发只换 model，键序、其余字段一个字节不动。
    #[test]
    fn with_model_only_touches_the_model_field() {
        let body = br#"{"model":"grok-4.6@ch21","max_tokens":8,"messages":[],"system":[{"cache_control":{"type":"ephemeral","ttl":"1h"}}]}"#;
        let out = String::from_utf8(with_model(body, "grok-4.6")).unwrap();
        assert_eq!(
            out,
            r#"{"model":"grok-4.6","max_tokens":8,"messages":[],"system":[{"cache_control":{"type":"ephemeral","ttl":"1h"}}]}"#
        );
        // 没有 model 字段 / 不是 JSON：原样。
        assert_eq!(with_model(b"{\"a\":1}", "x"), b"{\"a\":1}");
        assert_eq!(with_model(b"nope", "x"), b"nope");
    }

    /// 假内核：按收到的 model 决定回什么。每个连接处理一个请求就关，逼着代理每次
    /// 重发都是一次完整的新请求（连接复用与否不影响断言）。
    async fn spawn_kernel(
        decide: fn(&str) -> (u16, String),
    ) -> (u16, Arc<AtomicUsize>, Arc<RwLock<Vec<String>>>) {
        let up = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = up.local_addr().unwrap().port();
        let hits = Arc::new(AtomicUsize::new(0));
        let seen: Arc<RwLock<Vec<String>>> = Arc::new(RwLock::new(Vec::new()));
        let (h, sn) = (Arc::clone(&hits), Arc::clone(&seen));
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = up.accept().await else { break };
                let (h, sn) = (Arc::clone(&h), Arc::clone(&sn));
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut tmp = [0u8; 8192];
                    let body_start = loop {
                        let n = s.read(&mut tmp).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                        if let Some(p) = find_header_end(&buf) {
                            break p;
                        }
                    };
                    let head = String::from_utf8_lossy(&buf[..body_start]).to_ascii_lowercase();
                    let len: usize = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .and_then(|v| v.trim().parse().ok())
                        .unwrap_or(0);
                    while buf.len() < body_start + len {
                        let n = s.read(&mut tmp).await.unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                    }
                    let body: serde_json::Value =
                        serde_json::from_slice(&buf[body_start..]).unwrap_or_default();
                    let model = body["model"].as_str().unwrap_or("").to_string();
                    h.fetch_add(1, Ordering::SeqCst);
                    sn.write().await.push(model.clone());
                    let (status, out) = decide(&model);
                    let resp = format!(
                        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{out}",
                        out.len()
                    );
                    let _ = s.write_all(resp.as_bytes()).await;
                    let _ = s.flush().await;
                });
            }
        });
        (port, hits, seen)
    }

    async fn run_through_proxy(
        kernel_port: u16,
        rules: ProxyRules,
        body: &[u8],
    ) -> (String, Vec<ProxyRecord>) {
        let target = Arc::new(RwLock::new(format!("http://127.0.0.1:{kernel_port}")));
        let rules = Arc::new(RwLock::new(rules));
        let records = Arc::new(RwLock::new(Vec::new()));
        let front = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let front_port = front.local_addr().unwrap().port();
        let (t, w, r) = (Arc::clone(&target), Arc::clone(&rules), Arc::clone(&records));
        tokio::spawn(async move {
            let (stream, _) = front.accept().await.unwrap();
            let _ = handle_conn(
                stream,
                Arc::new(ProxyState {
                    target: t,
                    rules: w,
                    records: r,
                    long_cache: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    rescue: Arc::new(RwLock::new(RescueConfig::default())),
                    inject: Arc::new(RwLock::new(InjectConfig::default())),
                    http: test_proxy_http(),
                }),
            )
            .await;
        });
        let mut c = TcpStream::connect(("127.0.0.1", front_port)).await.unwrap();
        c.write_all(
            format!(
                "POST /v1/messages HTTP/1.1\r\nHost: x\r\nUser-Agent: grok-build/1.0\r\ncontent-length: {}\r\n\r\n",
                body.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
        c.write_all(body).await.unwrap();
        let mut got = Vec::new();
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), c.read_to_end(&mut got)).await;
        let recs = records.read().await.clone();
        (String::from_utf8_lossy(&got).to_string(), recs)
    }

    /// 内核回 408（请求体没读完）时代理自己重发一次，CLI 只看到最终那个 200。
    ///
    /// 这个 408 产生在内核选渠道之前，没有 attempt、没有计费，所以重发是干净的；
    /// 不重发的话上行偶尔卡一下就会把一整轮对话打断。
    #[tokio::test]
    async fn a_body_read_timeout_is_resent_once_and_succeeds() {
        fn decide(_m: &str) -> (u16, String) {
            static N: AtomicUsize = AtomicUsize::new(0);
            if N.fetch_add(1, Ordering::SeqCst) == 0 {
                (408, r#"{"error":"timed out reading the request body"}"#.into())
            } else {
                (200, r#"{"ok":true}"#.into())
            }
        }
        let (port, hits, _seen) = spawn_kernel(decide).await;
        let (got, recs) = run_through_proxy(
            port,
            ProxyRules::default(),
            br#"{"model":"grok-4.6","messages":[]}"#,
        )
        .await;
        assert!(got.starts_with("HTTP/1.1 200"), "CLI 应当只看到重发后的成功：{got}");
        assert_eq!(hits.load(Ordering::SeqCst), 2, "原始一次 + 重发一次");
        assert_eq!(recs.len(), 1, "记录里只留最终结果");
        assert_eq!(recs[0].status, 200);
    }

    /// 但重发是**有上限**的：链路持续拥塞时每次尝试都可能烧掉内核那整段读取超时，
    /// 无限重发会让 CLI 干等到天荒地老，比直接把 408 交出去更糟。
    #[tokio::test]
    async fn a_persistent_body_read_timeout_gives_up_after_one_resend() {
        fn decide(_m: &str) -> (u16, String) {
            (408, r#"{"error":"timed out reading the request body"}"#.into())
        }
        let (port, hits, _seen) = spawn_kernel(decide).await;
        let (got, recs) = run_through_proxy(
            port,
            ProxyRules::default(),
            br#"{"model":"grok-4.6","messages":[]}"#,
        )
        .await;
        assert!(got.starts_with("HTTP/1.1 408"), "最终要把 408 如实交给 CLI：{got}");
        assert_eq!(hits.load(Ordering::SeqCst), 1 + UPLOAD_RETRIES as usize);
        assert_eq!(recs[0].status, 408, "盲区面板要靠这条记录才看得见");
    }

    /// 主线：私有别名被内核 503（那条只有一个渠道，冷却了就没人接），代理用原名重发，
    /// CLI 拿到 200；记录里写明是从哪个名字退下来的。
    #[tokio::test]
    async fn a_cooled_preferred_channel_falls_back_to_the_plain_alias() {
        let (port, hits, seen) = spawn_kernel(|m| {
            if m.contains("@ch") {
                (503, r#"{"error":"no available upstream (all cooled or none)"}"#.into())
            } else {
                (200, format!(r#"{{"model":"{m}"}}"#))
            }
        })
        .await;
        let rules = rules_with_pin("grok-4.6", &[21], true);
        let (got, recs) =
            run_through_proxy(port, rules, br#"{"model":"grok-4.6[1m]","max_tokens":8}"#).await;
        assert!(got.contains("200"), "{got}");
        assert!(got.contains(r#""model":"grok-4.6""#), "{got}");
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        assert_eq!(*seen.read().await, vec!["grok-4.6@ch21", "grok-4.6"]);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].status, 200);
        assert_eq!(recs[0].model.as_deref(), Some("grok-4.6[1m]"));
        assert_eq!(recs[0].sent_model.as_deref(), Some("grok-4.6"));
        assert_eq!(recs[0].fallback_from.as_deref(), Some("grok-4.6@ch21"));
    }

    /// 首选接住了：只发一次，记录里 sent_model 是私有别名，没有退让。
    #[tokio::test]
    async fn a_healthy_preferred_channel_is_used_once() {
        let (port, hits, _) = spawn_kernel(|m| (200, format!(r#"{{"model":"{m}"}}"#))).await;
        let rules = rules_with_pin("grok-4.6", &[21], true);
        let (got, recs) = run_through_proxy(port, rules, br#"{"model":"grok-4.6"}"#).await;
        assert!(got.contains(r#""model":"grok-4.6@ch21""#), "{got}");
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(recs[0].sent_model.as_deref(), Some("grok-4.6@ch21"));
        assert_eq!(recs[0].fallback_from, None);
    }

    /// 不退让：首选没接住就把 503 原样交给 CLI，不用原名再发 —— 用户说了「不可用就
    /// 停」，悄悄落到别家等于把这个开关做成了摆设。
    #[tokio::test]
    async fn without_fallback_the_failure_is_passed_through() {
        let (port, hits, _) = spawn_kernel(|_| (503, r#"{"error":"no available upstream"}"#.into())).await;
        let rules = rules_with_pin("grok-4.6", &[21], false);
        let (got, recs) = run_through_proxy(port, rules, br#"{"model":"grok-4.6"}"#).await;
        assert!(got.contains("503"), "{got}");
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(recs[0].status, 503);
        assert_eq!(recs[0].fallback_from, None);
    }

    /// 客户端级错误（400 too long）绝不重发：换个落点再发一遍只会再收一次 400，还多付
    /// 一次账；而且退让的落点往往窗口更窄，更不可能接住。
    #[tokio::test]
    async fn a_client_error_is_never_retried_on_the_fallback_alias() {
        let (port, hits, _) = spawn_kernel(|_| (400, r#"{"error":"prompt is too long"}"#.into())).await;
        let rules = rules_with_pin("grok-4.6", &[21], true);
        let (got, recs) = run_through_proxy(port, rules, br#"{"model":"grok-4.6"}"#).await;
        assert!(got.contains("400"), "{got}");
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(recs[0].status, 400);
        assert_eq!(recs[0].sent_model.as_deref(), Some("grok-4.6@ch21"));
        assert_eq!(recs[0].fallback_from, None);
    }

    /// 多个钉住落点按序试，全败再退原名；记录里的 fallback_from 是最后一个放弃的。
    #[tokio::test]
    async fn multiple_pinned_targets_are_tried_in_order() {
        let (port, _, seen) = spawn_kernel(|m| {
            if m.contains("@ch") {
                (429, r#"{"error":"rate limited"}"#.into())
            } else {
                (200, "{}".into())
            }
        })
        .await;
        let rules = rules_with_pin("claude-opus-5", &[15, 17], true);
        let (got, recs) = run_through_proxy(port, rules, br#"{"model":"claude-opus-5"}"#).await;
        assert!(got.contains("200"), "{got}");
        assert_eq!(
            *seen.read().await,
            vec!["claude-opus-5@ch15", "claude-opus-5@ch17", "claude-opus-5"]
        );
        assert_eq!(recs[0].fallback_from.as_deref(), Some("claude-opus-5@ch17"));
        // 最终发的和原名一样，不算改写。
        assert_eq!(recs[0].sent_model, None);
    }

    /// 没钉住的别名一切照旧：剥后缀、发一次。
    #[tokio::test]
    async fn unpinned_aliases_are_untouched() {
        let (port, hits, seen) = spawn_kernel(|_| (503, "{}".into())).await;
        let rules = rules_with_pin("grok-4.6", &[21], true);
        let (got, _) = run_through_proxy(port, rules, br#"{"model":"claude-opus-5[1m]"}"#).await;
        assert!(got.contains("503"), "{got}");
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(*seen.read().await, vec!["claude-opus-5"]);
    }
}

#[cfg(test)]
mod rescue_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// 假内核：请求体里带 `marker` 就回 `second`，否则回 `first`。
    /// （ rescue 流程的第一发不带 marker，注入流程的第一发带 —— 分支只看
    /// marker，不看请求序号，两类测试共用。）
    async fn spawn_json_kernel(first: &str, second: &str, marker: &str) -> (u16, Arc<AtomicUsize>) {
        let up = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = up.local_addr().unwrap().port();
        let hits = Arc::new(AtomicUsize::new(0));
        let (first, second, marker) =
            (first.to_string(), second.to_string(), marker.to_string());
        let h = Arc::clone(&hits);
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = up.accept().await else { break };
                let (h, first, second, marker) = (
                    Arc::clone(&h),
                    first.clone(),
                    second.clone(),
                    marker.clone(),
                );
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut tmp = [0u8; 8192];
                    let body_start = loop {
                        let n = s.read(&mut tmp).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                        if let Some(p) = find_header_end(&buf) {
                            break p;
                        }
                    };
                    let head = String::from_utf8_lossy(&buf[..body_start]).to_ascii_lowercase();
                    let len: usize = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .and_then(|v| v.trim().parse().ok())
                        .unwrap_or(0);
                    while buf.len() < body_start + len {
                        let n = s.read(&mut tmp).await.unwrap_or(0);
                        if n == 0 {
                            break;
                        }
                        buf.extend_from_slice(&tmp[..n]);
                    }
                    let body = String::from_utf8_lossy(&buf[body_start..]).to_string();
                    let _ = h.fetch_add(1, Ordering::SeqCst);
                    let out = if body.contains(&marker) {
                        second.clone()
                    } else {
                        first.clone()
                    };
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{out}",
                        out.len()
                    );
                    let _ = s.write_all(resp.as_bytes()).await;
                    let _ = s.flush().await;
                });
            }
        });
        (port, hits)
    }

    async fn run_rescue(
        kernel_port: u16,
        rescue_cfg: RescueConfig,
        body: &[u8],
    ) -> (String, Vec<ProxyRecord>) {
        let target = Arc::new(RwLock::new(format!("http://127.0.0.1:{kernel_port}")));
        let rules = Arc::new(RwLock::new(ProxyRules::default()));
        let records = Arc::new(RwLock::new(Vec::new()));
        let rescue = Arc::new(RwLock::new(rescue_cfg));
        let front = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let front_port = front.local_addr().unwrap().port();
        let (t, w, r, rs) = (
            Arc::clone(&target),
            Arc::clone(&rules),
            Arc::clone(&records),
            Arc::clone(&rescue),
        );
        tokio::spawn(async move {
            let (stream, _) = front.accept().await.unwrap();
            let _ = handle_conn(
                stream,
                Arc::new(ProxyState {
                    target: t,
                    rules: w,
                    records: r,
                    long_cache: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    rescue: rs,
                    inject: Arc::new(RwLock::new(InjectConfig::default())),
                    http: test_proxy_http(),
                }),
            )
            .await;
        });
        let mut c = TcpStream::connect(("127.0.0.1", front_port)).await.unwrap();
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
        let _ =
            tokio::time::timeout(std::time::Duration::from_secs(5), c.read_to_end(&mut got)).await;
        let recs = records.read().await.clone();
        (String::from_utf8_lossy(&got).to_string(), recs)
    }

    /// 主线：拒绝被丢掉（CLI 一个字节都看不到），破甲提示词被追加进重发请求，
    /// 第二次拿到正常回答；记录里写明命中了一次。
    #[tokio::test]
    async fn a_refusal_is_dropped_and_resent_with_the_armor_prompt() {
        let refusal = r#"{"content":[{"type":"text","text":"I cannot assist with that request. As an AI, I must decline."}]}"#;
        let answer = r#"{"content":[{"type":"text","text":"Here is the code you asked for."}]}"#;
        let (port, hits) = spawn_json_kernel(refusal, answer, "ARMOR-MARKER").await;
        let cfg = RescueConfig {
            enabled: true,
            armor_prompt: "ARMOR-MARKER".into(),
            markers: Vec::new(),
            max_retries: 3,
        };
        let (got, recs) = run_rescue(
            port,
            cfg,
            br#"{"model":"m","messages":[{"role":"user","content":"do the task"}]}"#,
        )
        .await;
        assert!(got.contains("Here is the code"), "{got}");
        assert!(
            !got.contains("I cannot assist"),
            "拒绝必须到不了 CLI：{got}"
        );
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].rescued, 1);
        assert_eq!(recs[0].status, 200);
    }

    /// 正常回答不触发：只发一次，记录里 rescued 是 0。
    #[tokio::test]
    async fn a_normal_answer_passes_through_without_resend() {        let answer = r#"{"content":[{"type":"text","text":"Done: the fix is in src/lib.rs."}]}"#;
        let (port, hits) = spawn_json_kernel(answer, answer, "M").await;
        let cfg = RescueConfig {
            enabled: true,
            armor_prompt: "M".into(),
            markers: Vec::new(),
            max_retries: 3,
        };
        let (got, recs) = run_rescue(port, cfg, br#"{"model":"m","messages":[{"role":"user","content":"fix it"}]}"#).await;
        assert!(got.contains("Done: the fix"), "{got}");
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(recs[0].rescued, 0);
    }

    /// 流式（SSE）响应也要判得出拒绝：CLI 实际发的是 stream:true。
    #[tokio::test]
    async fn a_refusal_in_an_sse_stream_is_also_rescued() {
        let refusal = "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"抱歉，我无法提供该信息，这违反安全策略。\"}}\n\n";
        let answer = "data: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"好的，以下是你要的内容。\"}}\n\n";
        let (port, hits) = spawn_json_kernel(refusal, answer, "ARMOR-MARKER").await;
        let cfg = RescueConfig {
            enabled: true,
            armor_prompt: "ARMOR-MARKER".into(),
            markers: Vec::new(),
            max_retries: 3,
        };
        let (got, recs) = run_rescue(
            port,
            cfg,
            br#"{"model":"m","stream":true,"messages":[{"role":"user","content":"task"}]}"#,
        )
        .await;
        assert!(got.contains("好的，以下是"), "{got}");
        assert!(!got.contains("无法提供"), "{got}");
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        assert_eq!(recs[0].rescued, 1);
    }

    /// 动态注入真的进了发往内核的请求体：假内核只在 body 带 marker 时回
    /// second，而 marker 只能来自注入 —— CLI 的原始 body 里没有它。
    #[tokio::test]
    async fn injected_text_rides_the_forwarded_request() {
        let first = r#"{"content":[{"type":"text","text":"first"}]}"#;
        let second = r#"{"content":[{"type":"text","text":"second"}]}"#;
        let (port, hits) = spawn_json_kernel(first, second, "INJECT-MARKER").await;
        let records = Arc::new(RwLock::new(Vec::new()));
        let state = Arc::new(ProxyState {
            target: Arc::new(RwLock::new(format!("http://127.0.0.1:{port}"))),
            rules: Arc::new(RwLock::new(ProxyRules::default())),
            records: Arc::clone(&records),
            long_cache: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            rescue: Arc::new(RwLock::new(RescueConfig::default())),
            inject: Arc::new(RwLock::new(InjectConfig {
                enabled: true,
                rules: vec![crate::services::dynamic_inject::InjectRule {
                    enabled: true,
                    cli: "claude-code".into(),
                    text: "INJECT-MARKER".into(),
                    ..Default::default()
                }],
            })),
            http: test_proxy_http(),
        });
        let front = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let front_port = front.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (stream, _) = front.accept().await.unwrap();
            let _ = handle_conn(stream, state).await;
        });
        // 带 UA：cli 匹配也一起被验证（rule.cli = claude-code）。
        let body = br#"{"model":"m","messages":[{"role":"user","content":"task"}]}"#;
        let mut c = TcpStream::connect(("127.0.0.1", front_port)).await.unwrap();
        c.write_all(
            format!(
                "POST /v1/messages HTTP/1.1\r\nHost: x\r\nUser-Agent: claude-cli/1.0\r\ncontent-length: {}\r\n\r\n",
                body.len()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
        c.write_all(body).await.unwrap();
        let mut got = Vec::new();
        let _ =
            tokio::time::timeout(std::time::Duration::from_secs(5), c.read_to_end(&mut got)).await;
        let got = String::from_utf8_lossy(&got).to_string();
        assert!(got.contains("second"), "注入的 marker 必须进了内核请求体：{got}");
        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }
}

#[cfg(test)]
mod ws_tunnel {
    use super::*;

    /// WebSocket 升级要透传：101 握手原样转，之后裸字节双向通；记录里是 101。
    #[tokio::test]
    async fn a_websocket_upgrade_is_tunneled() {
        // 假上游：回 101，握手后回显第一帧再断开。
        let up = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let up_port = up.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut s, _) = up.accept().await.unwrap();
            let mut buf = vec![0u8; 8192];
            let n = s.read(&mut buf).await.unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            assert!(req.to_lowercase().contains("upgrade: websocket"), "{req}");
            s.write_all(
                b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
                  Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo=\r\n\
                  Sec-WebSocket-Extensions: permessage-deflate\r\n\r\n",
            )
            .await
            .unwrap();
            s.flush().await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let mut f = [0u8; 64];
            let m = s.read(&mut f).await.unwrap();
            let echo = format!("echo:{}", String::from_utf8_lossy(&f[..m]));
            s.write_all(echo.as_bytes()).await.unwrap();
            s.flush().await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        });

        let target = Arc::new(RwLock::new(format!("http://127.0.0.1:{up_port}")));
        let records = Arc::new(RwLock::new(Vec::new()));
        let front = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let front_port = front.local_addr().unwrap().port();
        let (t, r) = (Arc::clone(&target), Arc::clone(&records));
        tokio::spawn(async move {
            let (stream, _) = front.accept().await.unwrap();
            let _ = handle_conn(
                stream,
                Arc::new(ProxyState {
                    target: t,
                    rules: Arc::new(RwLock::new(ProxyRules::default())),
                    records: r,
                    long_cache: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    rescue: Arc::new(RwLock::new(RescueConfig::default())),
                    inject: Arc::new(RwLock::new(InjectConfig::default())),
                    http: test_proxy_http(),
                }),
            )
            .await;
        });

        let mut c = TcpStream::connect(("127.0.0.1", front_port)).await.unwrap();
        c.write_all(
            b"GET /v1/responses HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\n\
             Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
             Sec-WebSocket-Version: 13\r\n\r\n",
        )
        .await
        .unwrap();

        // 读到 101。
        let mut got = Vec::new();
        let mut tmp = [0u8; 8192];
        loop {
            let n = c.read(&mut tmp).await.unwrap();
            if n == 0 {
                break;
            }
            got.extend_from_slice(&tmp[..n]);
            if String::from_utf8_lossy(&got).contains("101 Switching") {
                break;
            }
        }
        let head = String::from_utf8_lossy(&got).to_string();
        assert!(head.starts_with("HTTP/1.1 101"), "{head:?}");
        // 握手响应要原样到客户端：Accept 是客户端库必校验的，Extensions 是协商结果。
        // 以前写死一行 101，这两个头都被吞掉。
        assert!(head.contains("Sec-WebSocket-Accept: s3pPLMBiTxaQ9kYGzzhZRbK+xOo="), "{head:?}");
        assert!(head.contains("Sec-WebSocket-Extensions: permessage-deflate"), "{head:?}");
        assert_eq!(head.matches("HTTP/1.1").count(), 1, "只能有上游那一条状态行：{head:?}");

        // 升级之后裸字节直通。
        c.write_all(b"hello-ws").await.unwrap();
        c.flush().await.unwrap();
        let mut ws = String::new();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !ws.contains("echo:hello-ws") && std::time::Instant::now() < deadline {
            let n = c.read(&mut tmp).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            ws.push_str(&String::from_utf8_lossy(&tmp[..n]));
        }
        assert!(ws.contains("echo:hello-ws"), "{ws:?}");
        // 主动关掉客户端侧，让透传收尾、记录落地。
        let _ = c.shutdown().await;

        let recs = records.read().await;
        assert_eq!(recs.len(), 1);
        assert_eq!(recs[0].status, 101);
        assert_eq!(recs[0].path, "/v1/responses");
    }

    /// 升级请求的固定写法，给下面几条测试共用。
    const UPGRADE_REQ: &[u8] = b"GET /v1/responses HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\n\
        Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
        Sec-WebSocket-Version: 13\r\n\r\n";

    /// 起一个前端监听，把第一条连接交给 handle_conn；返回端口和记录表。
    async fn spawn_front(up_port: u16) -> (u16, Arc<RwLock<Vec<ProxyRecord>>>) {
        let target = Arc::new(RwLock::new(format!("http://127.0.0.1:{up_port}")));
        let records = Arc::new(RwLock::new(Vec::new()));
        let front = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let front_port = front.local_addr().unwrap().port();
        let (t, r) = (Arc::clone(&target), Arc::clone(&records));
        tokio::spawn(async move {
            let (stream, _) = front.accept().await.unwrap();
            let _ = handle_conn(
                stream,
                Arc::new(ProxyState {
                    target: t,
                    rules: Arc::new(RwLock::new(ProxyRules::default())),
                    records: r,
                    long_cache: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    rescue: Arc::new(RwLock::new(RescueConfig::default())),
                    inject: Arc::new(RwLock::new(InjectConfig::default())),
                    http: test_proxy_http(),
                }),
            )
            .await;
        });
        (front_port, records)
    }

    /// 发一个升级请求，把整条响应读到连接关闭；超过 5s 还没关就是代理挂住了。
    async fn upgrade_and_read_to_eof(front_port: u16, why: &str) -> String {
        let mut c = TcpStream::connect(("127.0.0.1", front_port)).await.unwrap();
        c.write_all(UPGRADE_REQ).await.unwrap();
        let mut got = Vec::new();
        tokio::time::timeout(std::time::Duration::from_secs(5), c.read_to_end(&mut got))
            .await
            .expect(why)
            .unwrap();
        String::from_utf8_lossy(&got).into_owned()
    }

    /// chunked 的非 101 响应：按终止块认结尾，然后**主动收尾**。上游（Go 的
    /// net/http 默认 keep-alive）在 chunked 响应之后不关连接，「读到关闭为止」会让
    /// 这条任务和两条连接一直挂到上游空闲超时。
    #[tokio::test]
    async fn a_chunked_non_101_response_ends_at_the_last_chunk() {
        let up = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let up_port = up.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut s, _) = up.accept().await.unwrap();
            let mut tmp = [0u8; 8192];
            let _ = s.read(&mut tmp).await.unwrap();
            // 三次写，边界故意落在块数据中间和终止块中间。
            s.write_all(
                b"HTTP/1.1 429 Too Many Requests\r\ncontent-type: application/json\r\n\
                  transfer-encoding: chunked\r\n\r\n5\r\n{\"e\"",
            )
            .await
            .unwrap();
            s.flush().await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(60)).await;
            s.write_all(b":\r\n4\r\n\"x\"}\r\n0\r\n").await.unwrap();
            s.flush().await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(60)).await;
            s.write_all(b"\r\n").await.unwrap();
            s.flush().await.unwrap();
            // keep-alive：故意不关，代理必须自己认出响应已经结束。
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        });

        let (front_port, records) = spawn_front(up_port).await;
        let got = upgrade_and_read_to_eof(front_port, "终止块到了就该收尾，不能等上游关连接").await;
        let (head, body) = got.split_once("\r\n\r\n").expect("a full header block");
        assert!(head.starts_with("HTTP/1.1 429"), "{head}");
        assert!(head.contains("transfer-encoding: chunked"), "分帧头要原样保留：{head}");
        assert!(head.ends_with("Connection: close"), "{head}");
        assert_eq!(body, "5\r\n{\"e\":\r\n4\r\n\"x\"}\r\n0\r\n\r\n", "chunked 字节要原样透传到终止块");
        assert_eq!(records.read().await[0].status, 429);
    }

    /// 两种分帧头都没有的非 101 响应是 close-delimited：流式转发到上游关闭，并把
    /// Connection 明示成 close（上游写的 keep-alive 在这条专开连接上是假话）。
    #[tokio::test]
    async fn a_close_delimited_non_101_response_streams_until_upstream_closes() {
        let up = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let up_port = up.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut s, _) = up.accept().await.unwrap();
            let mut tmp = [0u8; 8192];
            let _ = s.read(&mut tmp).await.unwrap();
            s.write_all(
                b"HTTP/1.1 502 Bad Gateway\r\ncontent-type: text/plain\r\n\
                  Connection: keep-alive\r\n\r\nupstream ",
            )
            .await
            .unwrap();
            s.flush().await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(60)).await;
            s.write_all(b"exploded").await.unwrap();
            s.flush().await.unwrap();
            // 关闭即结束 —— 这就是 close-delimited 的全部语义。
        });

        let (front_port, records) = spawn_front(up_port).await;
        let got = upgrade_and_read_to_eof(front_port, "上游关了代理就该关").await;
        let (head, body) = got.split_once("\r\n\r\n").expect("a full header block");
        assert!(head.starts_with("HTTP/1.1 502"), "{head}");
        assert!(!head.to_ascii_lowercase().contains("keep-alive"), "{head}");
        assert!(head.ends_with("Connection: close"), "{head}");
        assert_eq!(body, "upstream exploded");
        assert_eq!(records.read().await[0].status, 502);
    }

    /// 扫描器只认边界不看内容：整段喂和逐字节喂必须停在同一个位置，块扩展和
    /// trailer 都要认，终止序列之后的字节不属于这条消息。
    #[test]
    fn chunked_scanner_finds_the_end_regardless_of_read_boundaries() {
        let msg = b"4;ext=1\r\nWiki\r\n5\r\npedia\r\n0\r\nX-Trailer: a\r\n\r\n";
        let stream = [&msg[..], b"NEXT"].concat();

        let mut whole = ChunkedScanner::default();
        assert_eq!(whole.feed(&stream), msg.len());
        assert!(whole.is_done());

        let mut bytewise = ChunkedScanner::default();
        let mut consumed = 0;
        for b in &stream {
            consumed += bytewise.feed(std::slice::from_ref(b));
            if bytewise.is_done() {
                break;
            }
        }
        assert_eq!(consumed, msg.len());
    }

    #[test]
    fn chunked_scanner_gives_up_on_garbage() {
        let mut s = ChunkedScanner::default();
        s.feed(b"zz\r\n");
        assert!(s.is_broken(), "块长度不是十六进制");
        let mut s = ChunkedScanner::default();
        s.feed(b"4\r\nWikiXX\r\n");
        assert!(s.is_broken(), "块数据后面不是 CRLF");
    }

    /// 分帧判定按 RFC 7230 §3.3.3：无 body 的状态码优先，Transfer-Encoding 压过
    /// Content-Length，字段名必须顶格，两者都没有就是读到关闭。
    #[test]
    fn response_framing_follows_rfc_7230() {
        let f = |status, tail: &str| response_framing(status, &format!("http/1.1 {status} x\r\n{tail}\r\n"));
        assert_eq!(f(204, "content-length: 5\r\n"), BodyFraming::Empty);
        assert_eq!(f(304, ""), BodyFraming::Empty);
        assert_eq!(f(401, "transfer-encoding: chunked\r\ncontent-length: 20\r\n"), BodyFraming::Chunked);
        assert_eq!(f(401, "transfer-encoding: gzip, chunked\r\n"), BodyFraming::Chunked);
        assert_eq!(f(401, "transfer-encoding: gzip\r\n"), BodyFraming::UntilClose);
        assert_eq!(f(401, "content-length: 20\r\n"), BodyFraming::Length(20));
        assert_eq!(f(401, "content-length: abc\r\n"), BodyFraming::UntilClose);
        assert_eq!(f(401, "x-content-length: 20\r\n"), BodyFraming::UntilClose);
        assert_eq!(f(502, ""), BodyFraming::UntilClose);
    }

    /// Host 重写只能动 Host 这一行：值里带 "host:" 的头（Origin 的 localhost）不能
    /// 被改，原 Host 要拿掉，新 Host 指向上游，其余头原样。
    #[tokio::test]
    async fn host_rewrite_only_touches_the_host_header() {
        let up = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let up_port = up.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut s, _) = up.accept().await.unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 8192];
            let head_end = loop {
                let n = s.read(&mut tmp).await.unwrap();
                buf.extend_from_slice(&tmp[..n]);
                if let Some(p) = find_header_end(&buf) {
                    break p;
                }
            };
            // 把收到的请求头当 body 回给客户端（close-delimited），测试在客户端侧检查。
            s.write_all(b"HTTP/1.1 426 Upgrade Required\r\n\r\n").await.unwrap();
            s.write_all(&buf[..head_end]).await.unwrap();
            s.flush().await.unwrap();
        });

        let (front_port, _records) = spawn_front(up_port).await;
        let mut c = TcpStream::connect(("127.0.0.1", front_port)).await.unwrap();
        // Origin 故意放在 Host 前面，值里有 "localhost:"。
        c.write_all(
            b"GET /v1/responses HTTP/1.1\r\nOrigin: http://localhost:15777\r\n\
              Host: localhost:15777\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
              Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\n\r\n",
        )
        .await
        .unwrap();
        let mut got = Vec::new();
        tokio::time::timeout(std::time::Duration::from_secs(5), c.read_to_end(&mut got))
            .await
            .expect("upstream closed, so the proxy must close too")
            .unwrap();
        let got = String::from_utf8_lossy(&got);
        let (_, seen_by_upstream) = got.split_once("\r\n\r\n").expect("a header block");
        assert!(seen_by_upstream.starts_with("GET /v1/responses HTTP/1.1\r\n"), "{seen_by_upstream:?}");
        assert!(
            seen_by_upstream.contains("\r\nOrigin: http://localhost:15777\r\n"),
            "Origin 被改了：{seen_by_upstream:?}"
        );
        let hosts: Vec<&str> = seen_by_upstream
            .split("\r\n")
            .filter(|l| l.to_ascii_lowercase().starts_with("host:"))
            .collect();
        assert_eq!(hosts, vec![format!("Host: 127.0.0.1:{up_port}").as_str()], "{seen_by_upstream:?}");
        assert!(seen_by_upstream.contains("Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n"));
    }

    /// 抢跑帧不能丢：CLI 把升级请求头和第一帧放在同一个包里发来，上游必须原样
    /// 收到那一帧。以前只把 `buf[..header_end]` 交给隧道，`header_end` 之后的字节
    /// 被丢，上游永远看不到首帧。
    #[tokio::test]
    async fn a_pipelined_first_frame_is_not_dropped() {
        // 假上游：回 101，然后把它**收到的**全部字节里 header 之后那截回显出来。
        let up = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let up_port = up.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut s, _) = up.accept().await.unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 8192];
            // 读到请求头结束。
            let head_end = loop {
                let n = s.read(&mut tmp).await.unwrap();
                if n == 0 {
                    return;
                }
                buf.extend_from_slice(&tmp[..n]);
                if let Some(p) = find_header_end(&buf) {
                    break p;
                }
            };
            // 抢跑帧可能和头一起到，也可能紧跟着到；补读一次凑齐。
            if buf.len() == head_end {
                let n = s.read(&mut tmp).await.unwrap();
                buf.extend_from_slice(&tmp[..n]);
            }
            let pipelined = String::from_utf8_lossy(&buf[head_end..]).to_string();
            s.write_all(b"HTTP/1.1 101 Switching Protocols\r\n\r\n").await.unwrap();
            s.write_all(format!("saw:{pipelined}").as_bytes()).await.unwrap();
            s.flush().await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        });

        let target = Arc::new(RwLock::new(format!("http://127.0.0.1:{up_port}")));
        let records = Arc::new(RwLock::new(Vec::new()));
        let front = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let front_port = front.local_addr().unwrap().port();
        let (t, r) = (Arc::clone(&target), Arc::clone(&records));
        tokio::spawn(async move {
            let (stream, _) = front.accept().await.unwrap();
            let _ = handle_conn(
                stream,
                Arc::new(ProxyState {
                    target: t,
                    rules: Arc::new(RwLock::new(ProxyRules::default())),
                    records: r,
                    long_cache: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    rescue: Arc::new(RwLock::new(RescueConfig::default())),
                    inject: Arc::new(RwLock::new(InjectConfig::default())),
                    http: test_proxy_http(),
                }),
            )
            .await;
        });

        // 请求头和首帧塞进同一次 write —— 复现管线化到达。
        let mut c = TcpStream::connect(("127.0.0.1", front_port)).await.unwrap();
        c.write_all(
            b"GET /v1/responses HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\n\
             Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
             Sec-WebSocket-Version: 13\r\n\r\nFRAME1",
        )
        .await
        .unwrap();
        c.flush().await.unwrap();

        let mut got = String::new();
        let mut tmp = [0u8; 8192];
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !got.contains("saw:FRAME1") && std::time::Instant::now() < deadline {
            let n = c.read(&mut tmp).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            got.push_str(&String::from_utf8_lossy(&tmp[..n]));
        }
        assert!(got.contains("HTTP/1.1 101"), "{got:?}");
        assert!(got.contains("saw:FRAME1"), "上游没收到抢跑的首帧：{got:?}");
    }

    /// 上游在 101 之前抢跑发来的字节不能丢：读握手响应时和头一起读进缓冲的那截
    /// 帧数据，要在双向透传起步前先补给客户端。
    #[tokio::test]
    async fn upstream_bytes_buffered_with_the_101_reach_the_client() {
        // 假上游：把 101 和第一帧塞进同一次 write，逼代理把帧和头一起读进缓冲。
        let up = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let up_port = up.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut s, _) = up.accept().await.unwrap();
            let mut tmp = [0u8; 8192];
            let _ = s.read(&mut tmp).await.unwrap();
            s.write_all(
                b"HTTP/1.1 101 Switching Protocols\r\n\r\nHELLO-EARLY",
            )
            .await
            .unwrap();
            s.flush().await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        });

        let target = Arc::new(RwLock::new(format!("http://127.0.0.1:{up_port}")));
        let records = Arc::new(RwLock::new(Vec::new()));
        let front = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let front_port = front.local_addr().unwrap().port();
        let (t, r) = (Arc::clone(&target), Arc::clone(&records));
        tokio::spawn(async move {
            let (stream, _) = front.accept().await.unwrap();
            let _ = handle_conn(
                stream,
                Arc::new(ProxyState {
                    target: t,
                    rules: Arc::new(RwLock::new(ProxyRules::default())),
                    records: r,
                    long_cache: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    rescue: Arc::new(RwLock::new(RescueConfig::default())),
                    inject: Arc::new(RwLock::new(InjectConfig::default())),
                    http: test_proxy_http(),
                }),
            )
            .await;
        });

        let mut c = TcpStream::connect(("127.0.0.1", front_port)).await.unwrap();
        c.write_all(
            b"GET /v1/responses HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\n\
             Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
             Sec-WebSocket-Version: 13\r\n\r\n",
        )
        .await
        .unwrap();

        let mut got = String::new();
        let mut tmp = [0u8; 8192];
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !got.contains("HELLO-EARLY") && std::time::Instant::now() < deadline {
            let n = c.read(&mut tmp).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            got.push_str(&String::from_utf8_lossy(&tmp[..n]));
        }
        assert!(got.contains("HTTP/1.1 101"), "{got:?}");
        assert!(got.contains("HELLO-EARLY"), "101 前抢跑的上游字节丢了：{got:?}");
    }

    /// 非 101 的错误响应带 Content-Length 且 body 分多次到达时，代理要把 body
    /// 读全再收尾 —— 只发首轮那截会让客户端按声明长度一直等，界面卡死。
    #[tokio::test]
    async fn a_non_101_response_body_is_forwarded_in_full() {
        let up = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let up_port = up.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut s, _) = up.accept().await.unwrap();
            let mut tmp = [0u8; 8192];
            let _ = s.read(&mut tmp).await.unwrap();
            // 头 + 前半段 body 先发，后半段隔一会再发 —— 逼代理不能只转首轮。
            // body 声明 20 字节，先发 10 再隔一会发 10 —— 逼代理不能只转首轮。
            s.write_all(
                b"HTTP/1.1 401 Unauthorized\r\ncontent-type: application/json\r\ncontent-length: 20\r\n\r\n{\"error\":\"",
            )
            .await
            .unwrap();
            s.flush().await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(80)).await;
            s.write_all(b"denied\"}!!").await.unwrap(); // 10 + 10 = 20
            s.flush().await.unwrap();
        });

        let target = Arc::new(RwLock::new(format!("http://127.0.0.1:{up_port}")));
        let records = Arc::new(RwLock::new(Vec::new()));
        let front = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let front_port = front.local_addr().unwrap().port();
        let (t, r) = (Arc::clone(&target), Arc::clone(&records));
        tokio::spawn(async move {
            let (stream, _) = front.accept().await.unwrap();
            let _ = handle_conn(
                stream,
                Arc::new(ProxyState {
                    target: t,
                    rules: Arc::new(RwLock::new(ProxyRules::default())),
                    records: r,
                    long_cache: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                    rescue: Arc::new(RwLock::new(RescueConfig::default())),
                    inject: Arc::new(RwLock::new(InjectConfig::default())),
                    http: test_proxy_http(),
                }),
            )
            .await;
        });

        let mut c = TcpStream::connect(("127.0.0.1", front_port)).await.unwrap();
        c.write_all(
            b"GET /v1/responses HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\n\
             Connection: Upgrade\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\
             Sec-WebSocket-Version: 13\r\n\r\n",
        )
        .await
        .unwrap();

        // 读到 EOF：body 20 字节全到了连接才关。
        let mut got = Vec::new();
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            c.read_to_end(&mut got),
        )
        .await
        .expect("客户端应当在 body 收全后看到连接关闭，而不是一直等");
        let got = String::from_utf8_lossy(&got);
        assert!(got.contains("401 Unauthorized"), "{got}");
        let body = got.split("\r\n\r\n").nth(1).unwrap_or("");
        assert_eq!(body.len(), 20, "body 没转全：{body:?}");
        assert_eq!(body, "{\"error\":\"denied\"}!!", "{body:?}");

        let recs = records.read().await;
        assert_eq!(recs[0].status, 401);
    }
}
