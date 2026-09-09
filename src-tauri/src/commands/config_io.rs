//! 客户端配置的导出 / 导入。
//!
//! 导出的是**这个客户端自己的**配置：内核连接方式、模型链、强制路由、首选渠道
//! 钉住、出口别名（模型桥接）。渠道和令牌是内核的
//! 数据，内核后台自带 CSV 导入导出，不在这里重复一遍（重复一份就要跟着内核的
//! 字段变化走，迟早对不上）。
//!
//! 密钥默认不导出。管理密码和 client_api_token 拿到就能直接调内核的全部 admin
//! API，而导出文件的去向不受我们控制 —— 用户往往顺手丢进聊天窗口或云盘。要带上
//! 必须显式勾选，并且文件里会写明它含密钥。

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::error::{AppError, AppResult};
use crate::services::cli_io::write_atomic;
use crate::services::bridge::{BridgeEntry, BridgeStore};
use crate::services::fallback::{FallbackChain, FallbackStore};
use crate::services::forced_route::{ForcedRoute, ForcedRouteStore};
use crate::services::kernel::KernelConfig;
use crate::services::pins::{Pin, PinStore};
use crate::state::AppState;

/// 文件格式版本。字段不兼容时靠它给出人话错误，而不是让 serde 抛一串英文。
const FORMAT_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
pub struct ConfigBundle {
    pub format_version: u32,
    /// 导出时壳体打包的内核版本，便于判断链是否来自不同版本的字段约定。
    pub client_kernel_version: String,
    /// true 表示 kernel.admin_password / client_api_token 有真实值。
    pub includes_secrets: bool,
    pub kernel: KernelConfig,
    pub sandbox_cli_writes: bool,
    pub client_api_token: Option<String>,
    pub fallback_chains: Vec<FallbackChain>,
    /// 下面三张表都是**后加的**，所以全带 `#[serde(default)]`：老版本导出的文件
    /// 里没有这些键，不给默认值的话整份文件会读不进来（而它可能是用户唯一一份
    /// 备份）。`format_version` 不动 —— 加可选字段是向后兼容的。
    #[serde(default)]
    pub forced_routes: Vec<ForcedRoute>,
    /// 首选渠道钉住。落点里的 `channel_id` 是**那台内核**的编号，换一台内核导入
    /// 就会指向别的渠道 —— 导入时按 id 原样收下，界面上有「钉住的渠道已不在内核
    /// 里」那条提示兜底。
    #[serde(default)]
    pub pins: Vec<Pin>,
    /// 出口别名（模型桥接）。只带表本身，不带「已经写进哪几个 CLI」——
    /// 那要在目标机器上显式点「写进 CLI」。
    #[serde(default)]
    pub bridge: Vec<BridgeEntry>,
}

/// 导入前的预览：先让用户看清会覆盖什么，再决定要不要写。
#[derive(Debug, Serialize)]
pub struct ImportPreview {
    pub format_version: u32,
    pub client_kernel_version: String,
    pub includes_secrets: bool,
    pub kernel_mode: String,
    pub kernel_endpoint: String,
    pub chain_aliases: Vec<String>,
    /// 会被覆盖掉的本机链（同名的那些）。
    pub overwritten_aliases: Vec<String>,
    /// 这份文件里另外三张表各有多少条。0 = 文件里没有（老格式）或本来就是空的。
    pub forced_route_count: usize,
    pub pin_count: usize,
    pub bridge_count: usize,
}

fn store_path(state: &AppState) -> std::path::PathBuf {
    state.config_dir().join("fallback.json")
}

#[tauri::command]
pub async fn config_export(
    state: State<'_, AppState>,
    path: String,
    include_secrets: bool,
) -> AppResult<String> {
    let s = state.settings.read().await;
    let mut kernel = s.kernel.clone();
    let mut token = s.client_api_token.clone();
    if !include_secrets {
        kernel.admin_password = String::new();
        token = None;
        // 出口代理地址可能带 user:pass@（ssh -D / 付费 SOCKS 常见），
        // 说好不含密钥的文件不能把它原样带出去。
        kernel.outbound_proxy = kernel.outbound_proxy.map(|p| strip_userinfo(&p));
    }
    let bundle = ConfigBundle {
        format_version: FORMAT_VERSION,
        client_kernel_version: crate::commands::kernel::kernel_bundled_version().to_string(),
        includes_secrets: include_secrets,
        kernel,
        sandbox_cli_writes: s.sandbox_cli_writes,
        client_api_token: token,
        fallback_chains: FallbackStore::load(&store_path(&state))?.chains,
        forced_routes: ForcedRouteStore::load(&crate::commands::forced_route::store_path(&state))
            .map(|s| s.routes)
            .unwrap_or_default(),
        pins: PinStore::load(&crate::commands::pins::store_path(&state))
            .map(|s| s.pins)
            .unwrap_or_default(),
        bridge: BridgeStore::load(&crate::commands::bridge::store_path(&state))
            .map(|s| s.entries)
            .unwrap_or_default(),
    };
    drop(s);

    let body = serde_json::to_string_pretty(&bundle)
        .map_err(|e| AppError::Config(format!("导出序列化失败：{e}")))?;
    // 走 write_atomic：带密钥时必须是 0600，不能按 umask 落成人人可读。
    write_atomic(std::path::Path::new(&path), &format!("{body}\n"))?;
    Ok(path)
}

fn read_bundle(path: &str) -> Result<ConfigBundle, AppError> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| AppError::Config(format!("读不到 {path}：{e}")))?;
    let bundle: ConfigBundle = serde_json::from_str(&raw)
        .map_err(|e| AppError::Config(format!("不是有效的 ccLoad 客户端配置文件：{e}")))?;
    if bundle.format_version > FORMAT_VERSION {
        return Err(AppError::Config(format!(
            "该文件是更高版本的格式（v{}），当前客户端只认到 v{FORMAT_VERSION}，请先升级客户端",
            bundle.format_version
        )));
    }
    Ok(bundle)
}

#[tauri::command]
pub async fn config_import_preview(
    state: State<'_, AppState>,
    path: String,
) -> AppResult<ImportPreview> {
    let bundle = read_bundle(&path)?;
    let existing = FallbackStore::load(&store_path(&state))?.chains;
    let incoming: Vec<String> = bundle
        .fallback_chains
        .iter()
        .map(|c| c.alias.clone())
        .collect();
    let overwritten = existing
        .iter()
        .filter(|c| incoming.contains(&c.alias))
        .map(|c| c.alias.clone())
        .collect();
    Ok(ImportPreview {
        format_version: bundle.format_version,
        client_kernel_version: bundle.client_kernel_version,
        includes_secrets: bundle.includes_secrets,
        kernel_mode: format!("{:?}", bundle.kernel.mode).to_lowercase(),
        kernel_endpoint: bundle.kernel.base_url(),
        chain_aliases: incoming,
        overwritten_aliases: overwritten,
        forced_route_count: bundle.forced_routes.len(),
        pin_count: bundle.pins.len(),
        bridge_count: bundle.bridge.len(),
    })
}

#[tauri::command]
pub async fn config_import(
    state: State<'_, AppState>,
    path: String,
    apply_kernel: bool,
) -> AppResult<Vec<String>> {
    let bundle = read_bundle(&path)?;
    let mut done = Vec::new();

    // 链是合并（同名覆盖），不是整表替换：导入别人的一组链不该把本机自己的删掉。
    let path = store_path(&state);
    let mut chains = FallbackStore::load(&path)?.chains;
    for c in bundle.fallback_chains {
        match chains.iter().position(|x| x.alias == c.alias) {
            Some(i) => chains[i] = c,
            None => chains.push(c),
        }
    }
    let n = chains.len();
    FallbackStore { chains }.save(&path)?;
    done.push(format!("模型链已合并，现共 {n} 条"));

    // 另外三张表同样是**合并**（同名覆盖），不是整表替换：导入别人的一份配置
    // 不该把本机自己攒的那些删掉。空的就整段跳过，免得在日志里留一堆「现共 0 条」。
    if !bundle.forced_routes.is_empty() {
        let p = crate::commands::forced_route::store_path(&state);
        let mut routes = ForcedRouteStore::load(&p)?.routes;
        for r in bundle.forced_routes {
            match routes.iter().position(|x| x.from == r.from) {
                Some(i) => routes[i] = r,
                None => routes.push(r),
            }
        }
        let n = routes.len();
        ForcedRouteStore { routes }.save(&p)?;
        done.push(format!("强制路由已合并，现共 {n} 条"));
    }

    if !bundle.pins.is_empty() {
        let p = crate::commands::pins::store_path(&state);
        let mut store = PinStore::load(&p)?;
        for pin in bundle.pins {
            store.upsert(pin);
        }
        let n = store.pins.len();
        store.save(&p)?;
        // 落点里的 channel_id 是**导出那台内核**的编号。换一台内核导入就会指向
        // 别的渠道，所以不在这里替用户写内核 —— 让他去「模型路由」页确认，
        // 那儿有「钉住的渠道已不在内核里」和「写回内核」两条兜底。
        done.push(format!(
            "首选渠道钉住已合并，现共 {n} 条（渠道编号跟着导出那台内核走，请到「模型路由」页确认一遍）"
        ));
    }

    if !bundle.bridge.is_empty() {
        let p = crate::commands::bridge::store_path(&state);
        let mut store = BridgeStore::load(&p)?;
        for e in bundle.bridge {
            let key = crate::services::context_floor::alias_key(&e.alias);
            match store
                .entries
                .iter()
                .position(|x| crate::services::context_floor::alias_key(&x.alias) == key)
            {
                Some(i) => store.entries[i] = e,
                None => store.entries.push(e),
            }
        }
        let n = store.entries.len();
        store.save(&p)?;
        // 只落盘 + 刷代理改写表。**不写 CLI 配置** —— 那会动用户 home 下的真实
        // 文件，必须是他自己点「写进 CLI」的那一下。
        crate::commands::bridge::refresh_proxy_rewrites(&state).await;
        done.push(format!(
            "出口别名已合并，现共 {n} 条（改写表已生效；要写进各 CLI 请到「模型桥接」页点「写进 CLI」）"
        ));
    }

    if apply_kernel {
        let mut kernel = bundle.kernel;
        {
            let s = state.settings.read().await;
            // 没带密钥的文件里密码是空串，别用它把本机能用的密码盖掉。
            if kernel.admin_password.is_empty() {
                kernel.admin_password = s.kernel.admin_password.clone();
            }
            // data_dir 是本机路径，跟着别人的机器走必然是错的。
            kernel.data_dir = s.kernel.data_dir.clone();
        }
        // 走和设置页同一条路：校验、失效令牌、重建客户端、重定向代理。
        // 之前这里直接赋值，切换内核身份后旧 token 留着，每个 CLI 都 401。
        crate::commands::settings::apply_kernel_config(&state, kernel).await?;
        // 文件里的 token 是给**那台内核**的，只在身份没换（apply 没清掉它）时
        // 才有意义；apply 清过就以清过为准。sandbox 是本机偏好，不从文件带。
        if let Some(tok) = bundle.client_api_token {
            let mut s = state.settings.write().await;
            if s.client_api_token.is_none() {
                s.client_api_token = Some(tok);
            }
            drop(s);
            state.persist().await?;
        }
        done.push("内核连接设置已应用（需重启内核生效）".into());
    }
    Ok(done)
}

/// 原生「保存到哪」对话框。
///
/// 之前走 JS 侧的 plugin-dialog `save()`，在这台机器上点击后连对话框都不弹、
/// promise 也不落（用户侧表现为「点了没反应」）。Rust 侧的对话框挂在 app 上，
/// 是 app-modal，行为可靠；顺手把权限依赖也从渲染端拿掉了。
#[tauri::command]
pub async fn pick_save_path(app: tauri::AppHandle, default_name: String) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    let picked = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .add_filter("JSON", &["json"])
            .set_file_name(default_name)
            .blocking_save_file()
    })
    .await
    .ok()
    .flatten();
    picked
        .and_then(|p| p.into_path().ok())
        .map(|p| p.display().to_string())
}

/// 原生「选一个目录」对话框。会话预设要的是 cwd，不是某个文件。
#[tauri::command]
pub async fn pick_folder(app: tauri::AppHandle) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    let picked =
        tauri::async_runtime::spawn_blocking(move || app.dialog().file().blocking_pick_folder())
            .await
            .ok()
            .flatten();
    picked
        .and_then(|p| p.into_path().ok())
        .map(|p| p.display().to_string())
}

/// 原生「选一个文件」对话框，同上。
#[tauri::command]
pub async fn pick_open_path(app: tauri::AppHandle) -> Option<String> {
    use tauri_plugin_dialog::DialogExt;
    let picked = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .add_filter("JSON", &["json"])
            .blocking_pick_file()
    })
    .await
    .ok()
    .flatten();
    picked
        .and_then(|p| p.into_path().ok())
        .map(|p| p.display().to_string())
}

/// `socks5://user:pass@host:1080` → `socks5://host:1080`。解析不了就原样返回
/// —— 那种写法本来也过不了 parse_outbound_proxy 的校验。
fn strip_userinfo(url: &str) -> String {
    match reqwest::Url::parse(url) {
        Ok(mut u) if !u.username().is_empty() || u.password().is_some() => {
            let _ = u.set_username("");
            let _ = u.set_password(None);
            u.to_string()
        }
        _ => url.to_string(),
    }
}

#[cfg(test)]
mod redaction_tests {
    use super::*;

    /// 说好不含密钥的导出文件，出口代理里的 user:pass 也得摘干净。
    #[test]
    fn export_without_secrets_strips_proxy_credentials() {
        assert_eq!(
            strip_userinfo("socks5://alice:s3cret@127.0.0.1:1080"),
            "socks5://127.0.0.1:1080"
        );
        assert_eq!(strip_userinfo("http://127.0.0.1:7890"), "http://127.0.0.1:7890");
        assert_eq!(strip_userinfo("not a url"), "not a url");
    }

    /// 老版本导出的文件里没有后加的那三张表 —— 必须能读进来，而且读成空。
    ///
    /// 这是最要紧的一条：那份文件可能是用户唯一一份备份。缺 `#[serde(default)]`
    /// 的话整份文件会因为「缺字段」直接解析失败，而错误信息只会说某个键不存在。
    #[test]
    fn a_v1_bundle_without_the_new_tables_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("old.json");
        std::fs::write(
            &p,
            r#"{"format_version":1,"client_kernel_version":"v4.10.0",
                "includes_secrets":false,
                "kernel":{"mode":"remote","port":15722,"remote_url":"https://x:8992",
                          "admin_password":"","data_dir":null},
                "sandbox_cli_writes":false,"client_api_token":null,
                "fallback_chains":[]}"#,
        )
        .unwrap();
        let b = read_bundle(p.to_str().unwrap()).unwrap();
        assert!(b.forced_routes.is_empty());
        assert!(b.pins.is_empty());
        assert!(b.bridge.is_empty());
    }

    /// 新格式来回一趟，三张表都要原样回来。
    #[test]
    fn the_new_tables_survive_a_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("new.json");
        let bundle = ConfigBundle {
            format_version: FORMAT_VERSION,
            client_kernel_version: "v4.10.4".into(),
            includes_secrets: false,
            kernel: KernelConfig::default(),
            sandbox_cli_writes: false,
            client_api_token: None,
            fallback_chains: Vec::new(),
            forced_routes: Vec::new(),
            pins: vec![Pin {
                alias: "claude-opus-5".into(),
                targets: vec![crate::services::pins::PinTarget {
                    channel_id: 15,
                    channel_name: "Anthropic".into(),
                    upstream: "claude-opus-5".into(),
                }],
                fallback: true,
            }],
            bridge: vec![BridgeEntry {
                alias: "ccload-fast".into(),
                target: "grok-4.6".into(),
                context_window: 0,
                compact_percent: 0,
                targets: Default::default(),
                tier: None,
            }],
        };
        std::fs::write(&p, serde_json::to_string(&bundle).unwrap()).unwrap();
        let back = read_bundle(p.to_str().unwrap()).unwrap();
        assert_eq!(back.pins.len(), 1);
        assert_eq!(back.pins[0].targets[0].channel_id, 15);
        assert_eq!(back.bridge.len(), 1);
        assert_eq!(back.bridge[0].target, "grok-4.6");
    }
}
