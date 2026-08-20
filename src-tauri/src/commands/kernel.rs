//! Kernel lifecycle commands. The renderer never talks to the process
//! directly — start / stop / status is the entire surface.

use std::path::PathBuf;

use tauri::{AppHandle, Manager, State};

use crate::error::AppResult;
use crate::services::kernel::{KernelConfig, KernelStatus};
use crate::state::AppState;

/// Resolve the bundled `ccload` binary. Dev builds look next to Cargo.toml
/// (`src-tauri/binaries/ccload`); packaged builds use the resource dir.
fn resolve_binary(app: &AppHandle) -> Option<PathBuf> {
    if let Ok(res) = app.path().resource_dir() {
        let packaged = res.join("binaries").join(binary_name());
        if packaged.exists() {
            return Some(packaged);
        }
    }
    // `CARGO_MANIFEST_DIR` is set at compile time to src-tauri/.
    let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("binaries")
        .join(binary_name());
    dev.exists().then_some(dev)
}

fn binary_name() -> &'static str {
    if cfg!(windows) {
        "ccload.exe"
    } else {
        "ccload"
    }
}

#[tauri::command]
pub async fn kernel_status(state: State<'_, AppState>) -> AppResult<KernelStatus> {
    Ok(state.kernel.status().await)
}

/// 打进这个壳体的 ccLoad 版本。
///
/// 由 `scripts/build-kernel.mjs` 从 `vendor/ccLoad` 的 `git describe` 写进
/// `kernel-version.txt`，同一个值也通过 `-ldflags -X` 编进了二进制自己的 version
/// 包 —— 两边同源，不会再出现界面上写死一个版本号、实际打包的是另一版的情况。
/// 这里不去启动内核问它：设置页开机就要显示，而远端模式下本机内核根本不会跑。
#[tauri::command]
pub fn kernel_bundled_version() -> &'static str {
    include_str!("../../kernel-version.txt").trim()
}

#[tauri::command]
pub async fn kernel_start(app: AppHandle, state: State<'_, AppState>) -> AppResult<KernelStatus> {
    let cfg = state.settings.read().await.kernel.clone();
    let bin = match cfg.mode {
        crate::services::kernel::KernelMode::Managed => resolve_binary(&app),
        crate::services::kernel::KernelMode::Remote => None,
    };
    state.kernel.start(&cfg, bin).await?;
    // A fresh process means a fresh admin session.
    state.admin.invalidate().await;
    ensure_client_token(&state).await?;
    // (Re)target the iframe proxy at the now-live kernel origin.
    ensure_embed_proxy(&state).await?;
    Ok(state.kernel.status().await)
}

/// Start the embed proxy once, or retarget it if settings changed.
pub async fn ensure_embed_proxy(state: &AppState) -> Result<(), crate::error::AppError> {
    let base = state.settings.read().await.kernel.base_url();
    let guard = state.embed_proxy.read().await;
    if let Some(proxy) = guard.as_ref() {
        proxy.retarget(&base).await;
    } else {
        drop(guard);
        let proxy = crate::services::embed_proxy::EmbedProxy::start(&base).await?;
        *state.embed_proxy.write().await = Some(proxy);
    }
    Ok(())
}

/// Iframe base URL for the renderer, or None while the proxy is not up.
#[tauri::command]
pub async fn embed_proxy_url(state: State<'_, AppState>) -> AppResult<Option<String>> {
    Ok(state
        .embed_proxy
        .read()
        .await
        .as_ref()
        .map(|p| p.embed_url("")))
}

/// Open (or focus) the standalone admin window on the given page
/// ("channels.html", "logs.html", ...). A top-level window is not affected
/// by the kernel's X-Frame-Options: DENY, which only blocks framing — so
/// this needs no proxy and renders with the kernel's own engine-agnostic
/// assets. (The in-app iframe route was abandoned: WKWebView renders
/// cross-origin sandboxed iframes unreliably on macOS.)
#[tauri::command]
pub async fn open_admin_window(
    app: AppHandle,
    state: State<'_, AppState>,
    page: Option<String>,
) -> AppResult<()> {
    let base = state.settings.read().await.kernel.base_url();
    let file = page.unwrap_or_else(|| "channels.html".to_string());
    let url = format!("{base}/web/{file}");
    // `base` 来自设置页里那个自由文本框，不是壳体生成的。解析一次，顺便挡掉
    // 非 http(s) 的 scheme —— 下面导航要用的也正是这个解析结果。
    let parsed = tauri::Url::parse(&url)
        .map_err(|e| crate::error::AppError::Config(format!("内核地址不是合法 URL：{e}")))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(crate::error::AppError::Config(format!(
            "内核地址的协议必须是 http 或 https，当前是 {}",
            parsed.scheme()
        ))
        .into());
    }

    let terr = |e: tauri::Error| crate::error::AppError::Config(e.to_string());
    if let Some(w) = app.get_webview_window("admin") {
        // Same target page? Keep it (preserves login/session state);
        // otherwise navigate, then raise the window.
        if w.url().map(|u| u.to_string() != url).unwrap_or(true) {
            // 用 navigate 而不是 eval("location.replace('…')")：那条路会把用户
            // 输入的地址插进一段单引号 JS 字面量里，地址里有个 `'` 就能在这个
            // **已登录的管理窗口**里执行任意脚本，而管理会话的 token 能直接调
            // 内核的全部 admin API。navigate 走的是原生导航，没有字符串拼接。
            w.navigate(parsed).map_err(terr)?;
        }
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
        return Ok(());
    }

    // 只在真要新建窗口时才登 —— 复用那条路上窗口里已经有会话了，白登一次没意义。
    let session = admin_web_session(&state).await;

    tauri::WebviewWindowBuilder::new(
        &app,
        "admin",
        tauri::WebviewUrl::External(url.parse().map_err(|e| {
            crate::error::AppError::Config(format!("bad admin url: {e}"))
        })?),
    )
    .title("ccLoad 管理后台")
    .inner_size(1180.0, 800.0)
    .min_inner_size(880.0, 560.0)
    .initialization_script(admin_session_script(session.as_ref()))
    .build()
    .map_err(terr)?;
    Ok(())
}

/// 开窗前替用户登一次，把内核 Web UI 的会话预置好。
///
/// 失败不算错：拿不到会话就照常开窗，用户手动登录即可（设置页和「内核后台」页都
/// 摆着可复制的管理密码）。为了省一次登录把整个管理后台打不开，不划算。
async fn admin_web_session(state: &AppState) -> Option<crate::services::admin::WebSession> {
    let (base_url, password) = {
        let s = state.settings.read().await;
        (s.kernel.base_url(), s.kernel.admin_password.clone())
    };
    if password.is_empty() {
        return None;
    }
    match state.admin.web_session(&base_url, &password).await {
        Ok(s) if s.expires_in_secs > 0 => Some(s),
        Ok(_) => {
            tracing::warn!("kernel /login returned no expiresIn; leaving the admin window to log in manually");
            None
        }
        Err(e) => {
            tracing::warn!("admin auto-login failed, falling back to the login page: {e}");
            None
        }
    }
}

/// 把会话写进管理窗口的 localStorage，键名与内核 Web UI 的 `storeWebSession`
/// 一致（`vendor/ccLoad/web/assets/js/web-auth.js`）。
///
/// 两个刻意的选择：
///
/// 1. **值一律用 `serde_json` 编码再拼进脚本**，不做字符串插值。这个窗口持有活的
///    管理会话，能调内核全部 admin API —— 往它的 JS 字面量里拼东西正是下面
///    `navigate` 那段注释修掉的注入口子，别在这里重新开一个。
/// 2. **一个窗口只预置一次**（sessionStorage 上打标记）。`initialization_script`
///    每次导航都会跑；不打标记的话，用户在后台点了「退出登录」，下一次导航又被我们
///    悄悄登回去，等于用户的退出按钮失灵。
fn admin_session_script(session: Option<&crate::services::admin::WebSession>) -> String {
    let Some(s) = session else {
        return String::new();
    };
    let token = serde_json::Value::String(s.token.clone());
    let role = serde_json::Value::String(s.role.clone());
    format!(
        r#"(function () {{
  try {{
    if (sessionStorage.getItem('ccload_desktop_seeded')) return;
    sessionStorage.setItem('ccload_desktop_seeded', '1');
    localStorage.setItem('ccload_token', {token});
    localStorage.setItem('ccload_token_expiry', String(Date.now() + {expires} * 1000));
    localStorage.setItem('ccload_web_role', {role});
  }} catch (e) {{}}
}})();"#,
        token = token,
        role = role,
        expires = s.expires_in_secs,
    )
}

/// Mint a dedicated API token the first time the kernel comes up, so CLI
/// takeover has something to write besides the admin password. A stored
/// token is re-validated against the live kernel first — tokens are bound
/// to one kernel instance, and a stale one (kernel switched, token revoked
/// on the kernel side) would 401 every CLI it was written into.
async fn ensure_client_token(state: &AppState) -> Result<(), crate::error::AppError> {
    let (base_url, password, existing) = {
        let s = state.settings.read().await;
        (
            s.kernel.base_url(),
            s.kernel.admin_password.clone(),
            s.client_api_token.clone(),
        )
    };
    if let Some(tok) = &existing {
        if api_token_works(state, &base_url, tok).await {
            return Ok(());
        }
        // Stale: drop it and mint a fresh one below.
        state.settings.write().await.client_api_token = None;
    }
    let created = state
        .admin
        .request(
            &base_url,
            &password,
            "POST",
            "auth-tokens",
            None,
            Some(serde_json::json!({
                "description": "desktop-client",
                "is_active": true
            })),
        )
        .await?;
    let token = created
        .pointer("/data/token")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            crate::error::AppError::Config("kernel created a token but returned no plaintext".into())
        })?;
    state.settings.write().await.client_api_token = Some(token.to_string());
    state.persist().await?;
    Ok(())
}

/// Probe GET /v1/models with the token. Only a real `401` proves the kernel
/// rejects it — every other outcome (200, an odd version's 404, a timeout, a
/// refused connection) leaves the token presumed good.
///
/// Two things this must get right, both learned from minting duplicates:
///   * use the kernel's shared client, not a fresh one — that client carries
///     `.no_proxy()` and the 30s timeout. A bare client with a 5s deadline
///     goes through a system HTTP_PROXY and times out against a remote kernel,
///     which looked exactly like "token rejected".
///   * a transport failure must NOT authorize a re-mint. Re-minting needs a
///     working admin login anyway, so on a genuinely dead network the mint
///     fails too; but on a *slow* one the strict probe fails while the lenient
///     mint succeeds, leaving an orphan token behind on every start.
async fn api_token_works(state: &AppState, base_url: &str, token: &str) -> bool {
    let resp = state
        .kernel
        .http()
        .get(format!("{base_url}/v1/models"))
        .bearer_auth(token)
        .send()
        .await;
    token_verdict(resp.as_ref().map(|r| r.status().as_u16()).ok())
}

/// The decision itself, separated from the transport so it can be tested
/// without standing up an AppState. `None` = the request never completed.
fn token_verdict(status: Option<u16>) -> bool {
    match status {
        Some(401) => false,
        Some(_) => true,
        // Unreachable/slow kernel says nothing about the token's validity.
        None => true,
    }
}

#[tauri::command]
pub async fn kernel_stop(state: State<'_, AppState>) -> AppResult<KernelStatus> {
    state.kernel.stop().await?;
    state.admin.invalidate().await;
    Ok(state.kernel.status().await)
}

#[tauri::command]
pub async fn kernel_config(state: State<'_, AppState>) -> AppResult<KernelConfig> {
    Ok(state.settings.read().await.kernel.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only a real 401 means "the kernel rejects this token". Everything else
    /// leaves it presumed good.
    ///
    /// The transport-failure case is the one that actually bit us: the probe
    /// used to run on a bare 5s client (no `.no_proxy()`), so a slow remote
    /// kernel timed out, got read as "rejected", and a *new* token was minted
    /// on the lenient 30s client — a fresh orphan `desktop-client` token in
    /// the kernel on every start.
    #[test]
    fn transport_failure_never_reads_as_a_rejected_token() {
        assert!(!token_verdict(Some(401)), "401 is the only rejection");
        assert!(token_verdict(Some(200)));
        // Odd kernel versions may not serve /v1/models at all; auth still passed.
        assert!(token_verdict(Some(404)));
        assert!(token_verdict(Some(500)));
        // Timeout / refused connection / DNS failure — says nothing about the
        // token, and must not trigger a re-mint.
        assert!(token_verdict(None), "unreachable kernel must not re-mint");
    }

    fn session(token: &str) -> crate::services::admin::WebSession {
        crate::services::admin::WebSession {
            token: token.to_string(),
            expires_in_secs: 86_400,
            role: "admin".into(),
        }
    }

    /// 这个窗口持有活的管理会话，token 能调内核全部 admin API。token 里带引号/
    /// 反斜杠时必须还是一个字符串字面量，不能闭合它跑出去执行 —— 跟 `navigate`
    /// 那条注释讲的是同一个口子。
    #[test]
    fn a_quote_in_the_token_cannot_break_out_of_the_script() {
        let nasty = r#"a'b"c\d");alert(1);//"#;
        let script = admin_session_script(Some(&session(nasty)));

        // 子串匹配分不清「转义后的 \");」和「真的闭合了字面量的 ");」，所以判据
        // 不能是「有没有出现某段文本」—— payload 本来就会作为**数据**待在字面量
        // 里。把实参原样抠出来反解析：能还原成原文，就说明它自始至终是一个完整
        // 的字符串字面量，没有多出任何一个字符跑到代码位置上去。
        let line = script
            .lines()
            .find(|l| l.contains("'ccload_token'"))
            .expect("token line missing");
        let arg = line
            .trim_end()
            .strip_suffix(");")
            .and_then(|l| l.split_once("'ccload_token', "))
            .map(|(_, arg)| arg)
            .expect("could not isolate the setItem argument");

        assert_eq!(
            serde_json::from_str::<String>(arg).expect("argument is not one JSON string"),
            nasty,
            "argument did not round-trip — something escaped the literal:\n{script}"
        );
    }

    /// 没会话就别产出脚本 —— 用户手动登录，别往窗口里塞半截东西。
    #[test]
    fn no_session_means_no_script() {
        assert!(admin_session_script(None).is_empty());
    }

    /// `initialization_script` 每次导航都跑。不打标记的话，用户在后台点「退出
    /// 登录」，下一次导航又被我们悄悄登回去，退出按钮等于失灵。
    #[test]
    fn the_session_is_seeded_once_per_window_not_per_navigation() {
        let script = admin_session_script(Some(&session("t")));
        assert!(script.contains("ccload_desktop_seeded"), "{script}");
        // 三个键名必须和内核 web-auth.js 的 storeWebSession 对得上。
        for key in ["ccload_token", "ccload_token_expiry", "ccload_web_role"] {
            assert!(script.contains(key), "missing {key}:\n{script}");
        }
    }
}
