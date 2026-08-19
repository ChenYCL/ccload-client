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
    .build()
    .map_err(terr)?;
    Ok(())
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
}
