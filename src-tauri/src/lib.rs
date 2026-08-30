//! Library crate so commands stay unit-testable without launching a window.

mod commands;
pub mod error;
pub mod platform;
pub mod services;
pub mod state;

use tauri::tray::{MouseButton, MouseButtonState, TrayIconEvent};
use tauri::Manager;

use crate::state::AppState;

/// 关窗会把窗口和 Dock 图标一起藏起来，菜单栏那颗图标是唯一回来的路。
/// 左键 / 双击托盘、菜单「显示窗口」都走这里，免得两处各写一遍漏掉恢复 Dock。
fn show_main_window(app: &tauri::AppHandle) {
    #[cfg(target_os = "macos")]
    let _ = app.set_activation_policy(tauri::ActivationPolicy::Regular);
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // The client binary doubles as the vision MCP server: CLIs spawn
    // `<this binary> vision-mcp` for image description on non-multimodal
    // models. Handle it before Tauri starts — it must run headless.
    if std::env::args().nth(1).as_deref() == Some("vision-mcp") {
        std::process::exit(services::vision_mcp::serve_stdio());
    }

    // 同一个二进制也是生图 MCP 服务器（`<this binary> image-mcp`），
    // 让五家 CLI 都能生图 / 改图。同样必须在 Tauri 起来之前处理。
    if std::env::args().nth(1).as_deref() == Some("image-mcp") {
        std::process::exit(services::image_mcp::serve_stdio());
    }

    // 必须在 WebView 建出来之前：这些键是启动时读一次的。
    platform::disable_automatic_text_substitutions();

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let state = AppState::load().expect("load app state");
            app.manage(state);

            // The iframe proxy comes up with the app so the embedded admin UI
            // works even when the kernel auto-started outside kernel_start().
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let state = handle.state::<AppState>();
                if let Err(e) = crate::commands::kernel::ensure_embed_proxy(&state).await {
                    tracing::warn!("embed proxy: {e}");
                }
                // 数据面代理也跟着起来：CLI 的接管配置指向的就是它，晚起一刻
                // 就有请求打到没人监听的端口上。
                if let Err(e) = crate::commands::cli_proxy::ensure_cli_proxy(&state).await {
                    tracing::warn!("cli proxy: {e}");
                }
                // 用户的 Node 服务跟着一起起来：MCP over http/sse 型的服务
                // 得先有个活着的端口，CLI 才连得上。
                crate::commands::node_services::autostart(&state).await;
                // 代理起来之后把被 CLI 冲掉的接管写回去 —— 顺序不能反：
                // reconcile 要把 CLI 指向代理的地址，代理没起来时那个地址还
                // 不存在。只碰有过快照的目标，没接管过的不动。
                match crate::commands::cli::cli_reconcile(state.clone()).await {
                    Ok(healed) if !healed.is_empty() => {
                        tracing::info!("接管已重新写回：{}", healed.join("、"));
                    }
                    Err(_) => tracing::warn!("接管重写失败，CLI 配置维持原样"),
                    _ => {}
                }
            });

            // Close hides the window AND the Dock icon — the managed kernel
            // and any in-flight CLI sessions must keep running, and an app
            // parked in the Dock with no window looks dead. The tray icon in
            // the menu bar is the way back in; quit lives there too.
            let window = app.get_webview_window("main").expect("main window");
            // 只有 macOS 分支用得到它（切 Dock 显示策略），其他平台留着就是
            // 一个未使用变量，clippy -D warnings 会红。
            #[cfg(target_os = "macos")]
            let app_handle = app.handle().clone();
            let win = window.clone();
            window.on_window_event(move |event| {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = win.hide();
                    // Accessory = no Dock icon, no menu bar of our own. The
                    // tray icon keeps running either way.
                    // Dock 是 macOS 独有的概念，`set_activation_policy` 也只在
                    // macOS 的 tauri 上存在 —— 不加守卫的话 Linux/Windows 直接
                    // 编不过（cannot find `ActivationPolicy` in `tauri`）。
                    #[cfg(target_os = "macos")]
                    let _ = app_handle.set_activation_policy(tauri::ActivationPolicy::Accessory);
                }
            });

            // Tray: the only way to fully quit after close-hides-window.
            let quit =
                tauri::menu::MenuItem::with_id(app, "quit", "退出 ccLoad", true, None::<&str>)?;
            let show = tauri::menu::MenuItem::with_id(app, "show", "显示窗口", true, None::<&str>)?;
            let menu = tauri::menu::Menu::with_items(app, &[&show, &quit])?;
            let _tray = tauri::tray::TrayIconBuilder::new()
                .icon(app.default_window_icon().expect("icon").clone())
                .menu(&menu)
                // 左键唤起窗口。Linux 不发 tray click 事件（Tauri 标明 Unsupported），
                // 只能靠左键弹出菜单里的「显示窗口」。
                .show_menu_on_left_click(cfg!(target_os = "linux"))
                .on_tray_icon_event(|tray, event| {
                    let show = matches!(
                        event,
                        TrayIconEvent::Click {
                            button: MouseButton::Left,
                            button_state: MouseButtonState::Up,
                            ..
                        } | TrayIconEvent::DoubleClick {
                            button: MouseButton::Left,
                            ..
                        }
                    );
                    if show {
                        show_main_window(tray.app_handle());
                    }
                })
                .on_menu_event(|app, event| match event.id.as_ref() {
                    // 必须先停内核再退出。`app.exit` 最终走的是
                    // `std::process::exit`，它不跑析构 —— `Child` 的
                    // `kill_on_drop(true)` 永远不会触发，而进程又被
                    // `process_group(0)` 从父进程的信号组里摘了出去。结果就是
                    // 退出后 ccload 还占着端口和 ccload.db：下次启动要么
                    // "address in use"，要么那个杀不掉的孤儿先应答 /health，
                    // 界面显示「运行中」而我们手里的句柄早已失效。
                    "quit" => {
                        let app = app.clone();
                        tauri::async_runtime::spawn(async move {
                            if let Some(state) = app.try_state::<AppState>() {
                                // 托管的 Node 服务必须一起收：它们持有固定端口，
                                // 留成孤儿的话下次启动端口被占、起不来，而用户
                                // 看到的还是「未运行」，无从下手。
                                state.node_services.stop_all().await;
                                let _ = state.kernel.stop().await;
                            }
                            app.exit(0);
                        });
                    }
                    "show" => show_main_window(app),
                    _ => {}
                })
                .build(app)?;

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::kernel::kernel_status,
            commands::kernel::kernel_bundled_version,
            commands::update::check_client_update,
            commands::config_io::config_export,
            commands::config_io::config_import_preview,
            commands::config_io::config_import,
            commands::config_io::pick_save_path,
            commands::config_io::pick_open_path,
            commands::config_io::pick_folder,
            commands::graph::graph_list,
            commands::graph::graph_save,
            commands::graph::graph_validate,
            commands::graph::graph_preview,
            commands::graph::graph_apply,
            commands::kernel::kernel_start,
            commands::kernel::kernel_stop,
            commands::kernel::kernel_config,
            commands::kernel::embed_proxy_url,
            commands::cli_proxy::cli_proxy_url,
            commands::cli_proxy::cli_proxy_records,
            commands::cli_proxy::cli_proxy_session,
            commands::cli_proxy::cli_proxy_long_cache,
            commands::cli_proxy::cli_proxy_set_long_cache,
            commands::cli_proxy::cli_proxy_usage,
            commands::node_services::node_service_list,
            commands::node_services::node_service_save,
            commands::node_services::node_service_delete,
            commands::node_services::node_service_start,
            commands::node_services::node_service_stop,
            commands::node_services::node_service_status,
            commands::node_services::node_service_write_script,
            commands::kernel::open_admin_window,
            commands::admin::admin_request,
            commands::admin::admin_ping,
            commands::cli::cli_preview,
            commands::cli::cli_preview_all,
            commands::cli::cli_apply,
            commands::cli::cli_reconcile,
            commands::cli::cli_set_proxy_routing,
            commands::cli::cli_backups,
            commands::cli::cli_backup_diff,
            commands::cli::cli_restore,
            commands::cli::cli_read_files,
            commands::cli::cli_write_file,
            commands::cli::cli_env_keys,
            commands::extensions::extensions_list,
            commands::extensions::extensions_support,
            commands::extensions::extension_install,
            commands::extensions::extension_remove,
            commands::extensions::extension_read,
            commands::extensions::extension_sync,
            commands::fallback::fallback_list,
            commands::fallback::fallback_save,
            commands::fallback::fallback_delete,
            commands::fallback::fallback_apply,
            commands::forced_route::forced_route_list,
            commands::forced_route::forced_route_save,
            commands::forced_route::forced_route_delete,
            commands::forced_route::forced_route_apply,
            commands::models::model_import,
            commands::models::vision_mcp_set,
            commands::models::vision_mcp_state,
            commands::models::image_mcp_set,
            commands::models::image_mcp_state,
            commands::models::mcp_usage_stats,
            commands::models::mcp_usage_clear,
            commands::inject::inject_state,
            commands::inject::inject_preview,
            commands::inject::inject_apply,
            commands::channel_usage::channel_usage_probe,
            commands::session::session_list,
            commands::session::session_slim,
            commands::session::session_compact,
            commands::session::session_delete,
            commands::preset::preset_list,
            commands::preset::preset_prefs,
            commands::preset::preset_set_hide_builtins,
            commands::preset::preset_save,
            commands::preset::preset_delete,
            commands::preset::preset_spawn,
            commands::settings::settings_get,
            commands::settings::settings_set_kernel,
            commands::settings::settings_set_sandbox,
            commands::settings::settings_set_client_token,
        ])
        .run(tauri::generate_context!())
        .expect("error while running ccload-client");
}
