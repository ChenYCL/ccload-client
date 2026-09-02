//! 托管用户自己的 Node 轻量服务。
//!
//! 三类活它都能干，但只有第三类是真的缺：
//!
//! * **代理** —— 数据面已经有 `cli_proxy` 了（Rust，扛 SSE 和长连接）。
//!   这里托管的服务如果也想插在链路上，让它自己转发到内核即可，不与代理抢。
//! * **MCP** —— 五家 CLI 的 MCP 配置由 `cli_extensions` 写。实测本机 27 个 MCP
//!   里 17 个 stdio、6 个 http、**0 个 sse**：stdio 由 CLI 自己拉起进程，
//!   根本不需要托管；http/sse 型才需要一个常驻端口，那正是这里的用途。
//! * **常驻 HTTP / SSE 服务** —— 需要一个活着的进程和一个稳定端口，进程死了
//!   要能看出来。这是本模块存在的理由。
//!
//! 和内核的托管方式对齐（`kernel.rs`）：自己的进程组、`kill_on_drop`、起完轮询
//! 健康检查再算「就绪」——端口 listen 了不代表服务能用。
//!
//! 端口由**用户指定**而不是我们随机分配：MCP 配置、CLI 配置里都要写死这个地址，
//! 随机端口等于每次重启都要重写一遍配置。

use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

use crate::error::AppError;

/// 起完之后等健康检查的上限。Node 冷启动（require 一堆包）比内核慢，给足。
const READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
const READY_POLL: std::time::Duration = std::time::Duration::from_millis(250);

/// 一个受管服务的定义。存在 `~/.ccload-client/node-services.json`。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeService {
    /// 稳定标识，也是 UI 上的名字。
    pub id: String,
    /// 入口脚本的绝对路径（`node <entry>`），或一条自定义命令的首个参数。
    pub entry: String,
    /// 额外参数。留空就是 `node <entry>`。
    #[serde(default)]
    pub args: Vec<String>,
    /// 工作目录。留空则取 entry 所在目录 —— 相对路径的 require 才找得到。
    #[serde(default)]
    pub cwd: Option<String>,
    /// 服务监听的端口。写进 MCP / CLI 配置的就是它，所以必须由用户定死。
    pub port: u16,
    /// 健康检查路径，默认 `/health`。空字符串表示不做检查，起了就算成功。
    #[serde(default)]
    pub health_path: Option<String>,
    /// 注入的环境变量。代理地址、token 这类东西从这里给。
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    /// 关掉的服务不随客户端启动。
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}

impl NodeService {
    fn health_url(&self) -> Option<String> {
        let p = self.health_path.as_deref().unwrap_or("/health");
        if p.is_empty() {
            return None;
        }
        let p = if p.starts_with('/') {
            p.to_string()
        } else {
            format!("/{p}")
        };
        Some(format!("http://127.0.0.1:{}{p}", self.port))
    }

    fn workdir(&self) -> Option<PathBuf> {
        if let Some(d) = self.cwd.as_ref().filter(|s| !s.trim().is_empty()) {
            return Some(PathBuf::from(d));
        }
        PathBuf::from(&self.entry).parent().map(PathBuf::from)
    }

    /// CLI / MCP 配置里该写的地址。
    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ServiceState {
    Stopped,
    Running,
    /// 进程还在，但健康检查没过 —— 端口 listen 了不等于服务能用。
    Unhealthy,
    /// 进程自己退了。和 Stopped 分开：这个是意外，UI 要能看出来。
    Exited,
}

#[derive(Debug, Serialize)]
pub struct ServiceStatus {
    pub id: String,
    pub state: ServiceState,
    pub port: u16,
    pub base_url: String,
    /// 最近一次失败的原因，成功时为空。
    pub message: Option<String>,
}

struct Running {
    child: Child,
    spec: NodeService,
}

#[derive(Default)]
pub struct NodeServices {
    running: Mutex<std::collections::HashMap<String, Running>>,
    http: reqwest::Client,
}

impl NodeServices {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            running: Mutex::new(Default::default()),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(3))
                .build()
                .expect("static client"),
        })
    }

    /// 起一个服务。已经在跑就先停掉再起 —— 幂等，重复点不会留下孤儿进程。
    ///
    /// `platform_env` 是平台注入的约定环境变量（网关地址、token 之类），
    /// 由调用方按当前设置算出来；用户在服务里填的同名变量优先。
    pub async fn start(
        &self,
        spec: &NodeService,
        platform_env: impl IntoIterator<Item = (String, String)>,
    ) -> Result<ServiceStatus, AppError> {
        self.stop(&spec.id).await?;

        let entry = PathBuf::from(&spec.entry);
        if !entry.exists() {
            return Err(AppError::Config(format!(
                "入口脚本不存在：{}",
                entry.display()
            )));
        }

        let mut cmd = Command::new("node");
        cmd.arg(&spec.entry).args(&spec.args);
        if let Some(dir) = spec.workdir() {
            cmd.current_dir(dir);
        }
        // PORT 是约定：Node 服务读它决定监听端口，免得两处各写一遍再对不上。
        cmd.env("PORT", spec.port.to_string());
        // 平台约定环境变量。托管服务最常见的用法就是「定时干点活 → 问一下
        // 模型 → 把结果发给谁」；这一类脚本都需要网关地址和凭据。由平台注入
        // 而不是让每份脚本硬编码：地址随内核模式/代理开关变，token 会轮换，
        // 硬编码的脚本会在某个看不见的时刻集体失效。同名用户 env 覆盖平台值。
        for (k, v) in platform_env {
            cmd.env(k, v);
        }
        for (k, v) in &spec.env {
            cmd.env(k, v);
        }
        cmd.kill_on_drop(true);
        // 自己的进程组：停的时候能连它拉起的子进程一起收掉。
        #[cfg(unix)]
        cmd.process_group(0);

        let child = cmd
            .spawn()
            .map_err(|e| AppError::Io(format!("起不来 {}：{e}（node 装了吗？）", spec.id)))?;

        self.running.lock().await.insert(
            spec.id.clone(),
            Running {
                child,
                spec: spec.clone(),
            },
        );

        match self.await_ready(spec).await {
            Ok(()) => Ok(ServiceStatus {
                id: spec.id.clone(),
                state: ServiceState::Running,
                port: spec.port,
                base_url: spec.base_url(),
                message: None,
            }),
            Err(e) => {
                // 起失败就别把半死的进程留着占端口。
                let _ = self.stop(&spec.id).await;
                Err(e)
            }
        }
    }

    async fn await_ready(&self, spec: &NodeService) -> Result<(), AppError> {
        let Some(url) = spec.health_url() else {
            return Ok(());
        };
        let deadline = tokio::time::Instant::now() + READY_TIMEOUT;
        loop {
            // 已经退了的进程不可能变健康，别干等到超时。
            if let Some(r) = self.running.lock().await.get_mut(&spec.id) {
                if let Ok(Some(exit)) = r.child.try_wait() {
                    return Err(AppError::Config(format!(
                        "{} 启动即退出（{exit}）—— 看看脚本自己报了什么错",
                        spec.id
                    )));
                }
            }
            if let Ok(resp) = self.http.get(&url).send().await {
                if resp.status().is_success() {
                    return Ok(());
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(AppError::Config(format!(
                    "{} 的 {url} 在 {}s 内没应答",
                    spec.id,
                    READY_TIMEOUT.as_secs()
                )));
            }
            tokio::time::sleep(READY_POLL).await;
        }
    }

    pub async fn stop(&self, id: &str) -> Result<(), AppError> {
        if let Some(mut r) = self.running.lock().await.remove(id) {
            kill_process_tree(&mut r.child).await;
        }
        Ok(())
    }

    /// 当前状态。会顺手回收已经退掉的进程，免得 UI 一直显示「运行中」。
    pub async fn status(&self, id: &str) -> ServiceStatus {
        let mut guard = self.running.lock().await;
        let Some(r) = guard.get_mut(id) else {
            return ServiceStatus {
                id: id.to_string(),
                state: ServiceState::Stopped,
                port: 0,
                base_url: String::new(),
                message: None,
            };
        };
        let spec = r.spec.clone();
        if let Ok(Some(exit)) = r.child.try_wait() {
            let msg = format!("进程已退出（{exit}）");
            guard.remove(id);
            return ServiceStatus {
                id: id.to_string(),
                state: ServiceState::Exited,
                port: spec.port,
                base_url: spec.base_url(),
                message: Some(msg),
            };
        }
        drop(guard);

        let healthy = match spec.health_url() {
            None => true,
            Some(url) => self
                .http
                .get(&url)
                .send()
                .await
                .map(|r| r.status().is_success())
                .unwrap_or(false),
        };
        ServiceStatus {
            id: id.to_string(),
            state: if healthy {
                ServiceState::Running
            } else {
                ServiceState::Unhealthy
            },
            port: spec.port,
            base_url: spec.base_url(),
            message: (!healthy).then(|| "进程在跑，但健康检查没过".to_string()),
        }
    }

    pub async fn stop_all(&self) {
        let ids: Vec<String> = self.running.lock().await.keys().cloned().collect();
        for id in ids {
            let _ = self.stop(&id).await;
        }
    }
}

/// 服务清单存哪儿。和 settings.json 同目录。
pub fn services_path(config_dir: &std::path::Path) -> PathBuf {
    config_dir.join("node-services.json")
}

pub fn load_services(config_dir: &std::path::Path) -> Vec<NodeService> {
    let path = services_path(config_dir);
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    serde_json::from_str(&raw).unwrap_or_default()
}

pub fn save_services(
    config_dir: &std::path::Path,
    list: &[NodeService],
) -> Result<(), AppError> {
    let body =
        serde_json::to_string_pretty(list).map_err(|e| AppError::Config(e.to_string()))?;
    crate::services::cli_io::write_atomic(&services_path(config_dir), &body)
}

/// 收掉整棵进程树，而不是只杀 node 自己。
///
/// `process_group(0)` 让服务当自己的组长，但 **tokio 的 `Child::kill` 只对 pid 发
/// SIGKILL** —— 组里被 node 拉起的 headless CLI（一次跑上十分钟的 `claude -p`）
/// 会被 reparent 给 launchd 继续跑完，token 照烧。所以先给 **-pgid** 发 SIGTERM
/// 给整组一个退出的机会，等几秒，还活着就 SIGKILL 整组。
async fn kill_process_tree(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pgid) = child.id() {
        let pgid: i32 = pgid as i32;
        // 组长进程自己也是组成员，-pgid 一并覆盖。
        unsafe { libc::kill(-pgid, libc::SIGTERM) };
        for _ in 0..50 {
            // try_wait 轮询的是 node 自己；它退了，整组里其余成员也已失去
            // 家长 —— 但真正的判据仍是它，组里别的进程没有句柄可等。
            if matches!(child.try_wait(), Ok(Some(_))) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        unsafe { libc::kill(-pgid, libc::SIGKILL) };
    }
    let _ = child.kill().await;
    let _ = child.wait().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(id: &str, entry: &str, port: u16) -> NodeService {
        NodeService {
            id: id.into(),
            entry: entry.into(),
            args: vec![],
            cwd: None,
            port,
            health_path: None,
            env: Default::default(),
            enabled: true,
        }
    }

    #[test]
    fn health_path_defaults_and_can_be_disabled() {
        let mut s = spec("a", "/tmp/x.js", 4000);
        assert_eq!(s.health_url().as_deref(), Some("http://127.0.0.1:4000/health"));
        s.health_path = Some("ready".into());
        assert_eq!(s.health_url().as_deref(), Some("http://127.0.0.1:4000/ready"));
        s.health_path = Some(String::new());
        assert!(s.health_url().is_none(), "空字符串 = 不做健康检查");
    }

    /// cwd 留空时取入口所在目录 —— 否则脚本里的相对 require 找不到文件。
    #[test]
    fn workdir_falls_back_to_the_entry_directory() {
        let s = spec("a", "/srv/app/index.js", 4000);
        assert_eq!(s.workdir().unwrap(), PathBuf::from("/srv/app"));
    }

    #[tokio::test]
    async fn missing_entry_is_rejected_before_spawning() {
        let svc = NodeServices::new();
        let err = svc
            .start(&spec("ghost", "/definitely/not/here.js", 4123), [])
            .await
            .unwrap_err();
        assert!(format!("{err:?}").contains("入口脚本不存在"));
    }

    #[tokio::test]
    async fn status_of_an_unknown_service_is_stopped() {
        let svc = NodeServices::new();
        assert_eq!(svc.status("nope").await.state, ServiceState::Stopped);
    }

    /// 一个立刻退出的脚本不能被报成「运行中」。
    #[tokio::test]
    async fn a_script_that_exits_immediately_is_reported_as_failed() {
        let dir = tempfile::tempdir().unwrap();
        let entry = dir.path().join("boom.js");
        std::fs::write(&entry, "process.exit(3);").unwrap();
        let svc = NodeServices::new();
        let mut s = spec("boom", entry.to_str().unwrap(), 4124);
        s.health_path = Some("/health".into());
        let err = svc.start(&s, []).await.unwrap_err();
        let text = format!("{err:?}");
        assert!(
            text.contains("启动即退出") || text.contains("没应答"),
            "退出的脚本必须报错，实际：{text}"
        );
    }

    /// 起一个真的 HTTP 服务，走完 spawn → 健康检查 → 停 的整条路。
    #[tokio::test]
    async fn starts_a_real_node_service_and_stops_it() {
        if std::process::Command::new("node")
            .arg("-v")
            .output()
            .is_err()
        {
            eprintln!("跳过：本机没有 node");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let entry = dir.path().join("srv.js");
        // 读 PORT 环境变量 —— 这就是我们和脚本之间的约定。
        std::fs::write(
            &entry,
            r#"require('http').createServer((q,s)=>{s.writeHead(200);s.end('ok')})
                 .listen(Number(process.env.PORT));"#,
        )
        .unwrap();

        let svc = NodeServices::new();
        let s = spec("real", entry.to_str().unwrap(), 4125);
        let st = svc.start(&s, []).await.expect("应当起得来");
        assert_eq!(st.state, ServiceState::Running);
        assert_eq!(st.base_url, "http://127.0.0.1:4125");
        assert_eq!(svc.status("real").await.state, ServiceState::Running);

        svc.stop("real").await.unwrap();
        assert_eq!(svc.status("real").await.state, ServiceState::Stopped);
    }
}

#[cfg(test)]
mod sse_service {
    use super::*;
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpStream;

    /// 端到端：托管一个真的 SSE 服务，确认 spawn → 健康检查 → 流式读 → 停
    /// 整条路都通。这是「MCP over sse」和「变量交互」两个场景的地基。
    #[tokio::test]
    async fn hosts_a_real_sse_service_and_streams_from_it() {
        if std::process::Command::new("node").arg("-v").output().is_err() {
            eprintln!("跳过：本机没有 node");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let entry = dir.path().join("sse.js");
        std::fs::write(
            &entry,
            r#"const http=require('http');
http.createServer((q,s)=>{
  if(q.url==='/health'){s.writeHead(200);s.end('ok');return}
  s.writeHead(200,{'content-type':'text/event-stream','cache-control':'no-cache'});
  let i=0;
  const t=setInterval(()=>{
    s.write(`data: tick-${i}\n\n`);
    if(++i>2){clearInterval(t);s.end()}
  },120);
}).listen(Number(process.env.PORT));"#,
        )
        .unwrap();

        let svc = NodeServices::new();
        let spec = NodeService {
            id: "sse".into(),
            entry: entry.to_str().unwrap().into(),
            args: vec![],
            cwd: None,
            port: 4126,
            health_path: Some("/health".into()),
            env: Default::default(),
            enabled: true,
        };
        let st = svc.start(&spec, []).await.expect("SSE 服务应当起得来");
        assert_eq!(st.state, ServiceState::Running);

        // 真去读一段流，确认拿到的是 SSE 而不是一次性 body。
        let mut stream = TcpStream::connect(("127.0.0.1", 4126)).await.unwrap();
        use tokio::io::AsyncWriteExt;
        stream
            .write_all(b"GET /events HTTP/1.1\r\nHost: x\r\nAccept: text/event-stream\r\n\r\n")
            .await
            .unwrap();
        let mut seen = String::new();
        let mut buf = [0u8; 2048];
        for _ in 0..12 {
            let n = tokio::time::timeout(
                std::time::Duration::from_secs(3),
                stream.read(&mut buf),
            )
            .await
            .expect("读超时")
            .unwrap();
            if n == 0 {
                break;
            }
            seen.push_str(&String::from_utf8_lossy(&buf[..n]));
            if seen.contains("tick-2") {
                break;
            }
        }
        assert!(seen.contains("text/event-stream"), "响应头要是 SSE：{seen}");
        assert!(seen.contains("tick-0") && seen.contains("tick-2"), "要收到多条事件：{seen}");

        svc.stop("sse").await.unwrap();
        assert_eq!(svc.status("sse").await.state, ServiceState::Stopped);
    }
}

#[cfg(test)]
mod platform_env_tests {
    use super::*;

    /// 平台 env 必须真的进到子进程，且**用户同名 env 覆盖平台值** ——
    /// 覆盖顺序反了，用户想指向别的内核时会莫名连回默认地址。
    #[tokio::test]
    async fn platform_env_reaches_the_child_and_user_env_wins() {
        if std::process::Command::new("node").arg("-v").output().is_err() {
            eprintln!("跳过：本机没有 node");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let entry = dir.path().join("echo_env.js");
        // 把两个变量回显出来：一个只有平台给，一个两边都给。
        std::fs::write(
            &entry,
            r#"require('http').createServer((q,s)=>{
              s.writeHead(200,{'content-type':'application/json'});
              s.end(JSON.stringify({p:process.env.CCLOAD_TEST_PLATFORM||null,
                                    u:process.env.CCLOAD_TEST_DUAL||null}));
            }).listen(Number(process.env.PORT));"#,
        )
        .unwrap();

        let svc = NodeServices::new();
        let mut spec = NodeService {
            id: "envtest".into(),
            entry: entry.to_str().unwrap().into(),
            args: vec![],
            cwd: None,
            port: 4127,
            health_path: Some("/health".into()),
            env: Default::default(),
            enabled: true,
        };
        spec.env.insert("CCLOAD_TEST_DUAL".into(), "from-user".into());

        let platform = vec![
            ("CCLOAD_TEST_PLATFORM".to_string(), "from-platform".to_string()),
            ("CCLOAD_TEST_DUAL".to_string(), "from-platform".to_string()),
        ];
        svc.start(&spec, platform).await.expect("应当起得来");

        let body: String = reqwest::get("http://127.0.0.1:4127/").await.unwrap().text().await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["p"], "from-platform", "平台独有变量要进到子进程");
        assert_eq!(v["u"], "from-user", "用户同名 env 必须覆盖平台值");
        svc.stop("envtest").await.unwrap();
    }
}

#[cfg(test)]
mod orphan_tests {
    use super::*;

    fn spec(id: &str, entry: &str, port: u16) -> NodeService {
        NodeService {
            id: id.into(),
            entry: entry.into(),
            args: vec![],
            cwd: None,
            port,
            health_path: None,
            env: Default::default(),
            enabled: true,
        }
    }

    /// stop() 必须收掉 node 拉起的**孙进程**。tokio 的 kill 只对 pid 发信号，
    /// 曾经的实现在杀掉 node 后留下 `claude -p` 这类长跑子进程继续烧 token ——
    /// 这里用 sh 循环模拟：stop 之后日志必须不再增长。
    #[tokio::test]
    async fn stop_kills_the_whole_process_group_not_just_node() {
        let dir = tempfile::tempdir().unwrap();
        let entry = dir.path().join("srv.js");
        std::fs::write(
            &entry,
            r#"const { spawn } = require('child_process');
spawn('sh', ['-c', 'while true; do echo tick >> /tmp/ccload-orphan-test.log; sleep 1; done'], { stdio: 'ignore' });
require('http').createServer((q,s)=>{s.writeHead(200);s.end('ok')}).listen(Number(process.env.PORT));"#,
        )
        .unwrap();
        let log = "/tmp/ccload-orphan-test.log";
        let _ = std::fs::remove_file(log);

        let svc = NodeServices::new();
        let s = spec("orphan", entry.to_str().unwrap(), 4133);
        svc.start(&s, []).await.expect("起不来");
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        assert!(std::path::Path::new(log).exists(), "孙进程应当已经在写日志");

        svc.stop("orphan").await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
        let before = std::fs::read_to_string(log).unwrap().lines().count();
        tokio::time::sleep(std::time::Duration::from_millis(2500)).await;
        let after = std::fs::read_to_string(log).unwrap().lines().count();
        assert_eq!(before, after, "stop 之后孙进程还在跑（{before} → {after} ticks）—— 进程组没被收掉");
    }
}
