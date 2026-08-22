//! 壳体自更新检查的 IPC 边界。
//!
//! 只读一次网络，不下载、不替换、不落盘 —— 界面拿到结果只是亮一个按钮，点开
//! 浏览器由用户自己决定。为什么不用 tauri-plugin-updater，见
//! [`crate::services::update`] 的模块注释。

use crate::services::update::{check, UpdateInfo};

/// 查一次「有没有新版壳体」。
///
/// `current` 由前端传：真值是 `getVersion()` 读到的 `tauri.conf.json` 版本，
/// 而 beta 流水线会把完整 tag 戳进去（`0.1.0-beta.20260822.4`）。Rust 侧的
/// `CARGO_PKG_VERSION` 是编译期的**基座**版本（`0.1.0`），拿它去比会让所有 beta
/// 用户永远看到「有新版」—— 那个数字和包上的版本不是一回事。
///
/// 失败返回 Err(String)，前端当成「这次没查到」静默处理即可：这是锦上添花的
/// 功能，不该因为断网或 GitHub 限流在侧栏弹错误。
#[tauri::command]
pub async fn check_client_update(current: String) -> Result<UpdateInfo, String> {
    check(&current).await
}
