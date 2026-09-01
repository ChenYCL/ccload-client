//! Manual e2e harness for the embed proxy against a live kernel.
//!
//! Usage: `cargo run --example embed_e2e -- [kernel-base-url]`
//!
//! Prints one proxied document URL, then idles until Ctrl-C so curl can
//! exercise it:
//!   curl <printed-url>                                # document, 200
//!   curl -H "Referer: <origin of printed-url>/x" http://127.0.0.1:<port>/web/assets/css/styles.css
//!   curl http://127.0.0.1:<port>/web/assets/css/styles.css   # no Referer, 403

use ccload_client_lib::services::embed_proxy::EmbedProxy;
use ccload_client_lib::services::kernel::{KernelConfig, KernelMode};

#[tokio::main]
async fn main() {
    let base = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "http://127.0.0.1:15722".into());
    let cfg = KernelConfig {
        mode: KernelMode::Remote,
        port: 15722,
        remote_url: Some(base),
        admin_password: String::new(),
        data_dir: None,
        outbound_proxy: None,
    };
    let proxy = EmbedProxy::start(&cfg).await.expect("start proxy");
    println!("DOC={}", proxy.embed_url("/web/channels.html"));
    println!("ORIGIN=http://127.0.0.1:{}", proxy.port());
    // Idle so curl can poke it; the harness is killed when done.
    std::thread::sleep(std::time::Duration::from_secs(600));
}
