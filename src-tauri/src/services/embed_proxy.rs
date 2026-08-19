//! Loopback proxy that strips frame-blocking headers so ccLoad's admin UI can
//! be embedded in an iframe inside the main window.
//!
//! Why a proxy at all: the kernel hard-codes `X-Frame-Options: DENY`
//! (internal/app/server.go:1327) with no config switch, and patching the
//! kernel would fork every future upgrade. This proxy is deliberately dumb —
//! forward bytes, remove headers, know nothing about the API. It only serves
//! the iframe in our own window, so:
//!   * binds 127.0.0.1 only (never reachable from the network),
//!   * mints a random per-launch URL prefix that the iframe must present,
//!     so other pages on the machine cannot embed the kernel through us.
//!     The kernel's pages reference CSS/JS/APIs with root-absolute paths
//!     (`/web/assets/...`), which lose the prefix once resolved against
//!     the proxy origin — those still pass, but only when their Referer
//!     carries the same unguessable token (see handle_conn).
//!
//! SSE/NDJSON: /web/logs.html streams chunks. The response body is forwarded
//! chunk-by-chunk (never buffered), so live log tailing works unchanged.

use std::sync::Arc;

use futures_util::StreamExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::RwLock;

use crate::error::AppError;

/// Headers removed from kernel responses before they reach the iframe.
/// Everything else (Set-Cookie, Content-Type, cache headers) passes through.
const STRIP: &[&str] = &["x-frame-options", "content-security-policy"];

/// Target origin, refreshable when the kernel settings change (mode switch,
/// port change) without rebinding the listener.
type SharedTarget = Arc<RwLock<String>>;

pub struct EmbedProxy {
    target: SharedTarget,
    /// Random per-launch path prefix; the iframe uses `/embed-<token>/...`.
    token: String,
    handle: tokio::task::JoinHandle<()>,
    port: u16,
}

impl EmbedProxy {
    /// Bind a loopback listener and start serving.
    pub async fn start(target_base_url: &str) -> Result<Arc<Self>, AppError> {
        let token = random_token();

        let mut listener = None;
        for port in (15731..=15760).rev() {
            match TcpListener::bind(("127.0.0.1", port)).await {
                Ok(l) => {
                    listener = Some((l, port));
                    break;
                }
                Err(_) => continue,
            }
        }
        let (listener, port) =
            listener.ok_or_else(|| AppError::Config("no free port for embed proxy".into()))?;

        let target: SharedTarget = Arc::new(RwLock::new(target_base_url.to_string()));
        let target_for_task = Arc::clone(&target);
        let token_arc = Arc::new(token.clone());
        let handle = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let target = Arc::clone(&target_for_task);
                let token = Arc::clone(&token_arc);
                tokio::spawn(async move {
                    let _ = handle_conn(stream, target, token).await;
                });
            }
        });

        Ok(Arc::new(Self {
            target,
            token,
            handle,
            port,
        }))
    }

    /// Point the proxy at a different kernel origin (settings changed).
    pub async fn retarget(&self, base_url: &str) {
        *self.target.write().await = base_url.to_string();
    }

    /// Base URL for the iframe: `http://127.0.0.1:<port>/embed-<token>`.
    pub fn embed_url(&self, path: &str) -> String {
        format!(
            "http://127.0.0.1:{}/embed-{}{}",
            self.port, self.token, path
        )
    }

    pub fn port(&self) -> u16 {
        self.port
    }
}

impl Drop for EmbedProxy {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

fn random_token() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    (0..24)
        .map(|_| {
            let i = rng.gen_range(0..36);
            if i < 10 {
                (b'0' + i) as char
            } else {
                (b'a' + i - 10) as char
            }
        })
        .collect()
}

/// One HTTP/1.1 conversation. Keep-alive is not implemented — every response
/// carries `Connection: close` and the connection is dropped, which iframes
/// and browsers handle fine at admin-page volumes.
async fn handle_conn(
    mut client: TcpStream,
    target: SharedTarget,
    token: Arc<String>,
) -> std::io::Result<()> {
    // Read until end of headers, then any Content-Length body.
    let mut buf: Vec<u8> = Vec::with_capacity(8 * 1024);
    let mut tmp = [0u8; 8 * 1024];
    let header_end = loop {
        let n = client.read(&mut tmp).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = find_header_end(&buf) {
            break pos;
        }
        if buf.len() > 64 * 1024 {
            return write_simple(&mut client, 431, "headers too large").await;
        }
    };

    let headers_text = String::from_utf8_lossy(&buf[..header_end]).to_string();
    let mut lines = headers_text.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("").to_string();
    let raw_path = parts.next().unwrap_or("").to_string();

    // Parse body length and collect passthrough headers. Referer is kept
    // aside — it is the second factor for the token check below.
    let mut content_length = 0usize;
    let mut referer = String::new();
    let mut sec_fetch_dest = String::new();
    let mut accept = String::new();
    let mut fwd_headers: Vec<(String, String)> = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        let lower = name.trim().to_ascii_lowercase();
        if lower == "host" || lower == "connection" {
            continue;
        }
        if lower == "content-length" {
            content_length = value.trim().parse().unwrap_or(0);
            continue;
        }
        if lower == "referer" {
            referer = value.trim().to_string();
        }
        if lower == "sec-fetch-dest" {
            sec_fetch_dest = value.trim().to_string();
        }
        if lower == "accept" {
            accept = value.trim().to_string();
        }
        fwd_headers.push((name.trim().to_string(), value.trim().to_string()));
    }

    // Two ways in. Either the path itself carries the per-launch token —
    // the proof the requester knows the secret — or the request comes from
    // a document we served under that token. The second case is the normal
    // one for iframe innards: absolute-path assets resolve against the
    // proxy origin and drop the token from the URL, but the browser still
    // sends the embedding document's URL as Referer, so the token rides
    // along there. Anything else is some other page on the machine trying
    // to use us as a frame-bypass. Refuse.
    let prefix = format!("/embed-{}/", token);
    let from_embed = referer.starts_with("http://127.0.0.1") && referer.contains(&prefix);
    let path = match raw_path.strip_prefix(&prefix) {
        // Root (path == prefix minus trailing slash) maps to "/".
        Some("") => "/".to_string(),
        Some(suffix) => format!("/{suffix}"),
        None if from_embed => {
            // The kernel's JS does `location.href = '/web/login.html'` when
            // the session expires. Serving that navigation here would land
            // the iframe on a token-less URL, and every sub-resource after
            // it would lose the Referer proof. Bounce document navigations
            // back under the token prefix instead, so the document URL —
            // and the Referer of everything it loads — keeps the token.
            let is_document = sec_fetch_dest == "document"
                || (sec_fetch_dest.is_empty() && accept.contains("text/html"));
            if is_document && method == "GET" {
                let location = format!("/embed-{token}{raw_path}");
                return write_redirect(&mut client, &location).await;
            }
            raw_path.clone()
        }
        None => {
            return write_simple(&mut client, 403, "bad embed token").await;
        }
    };

    // Consume exactly content-length body bytes.
    let body_bytes: Vec<u8> = if content_length > 0 {
        let mut body = buf[header_end..].to_vec();
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

    // Forward with reqwest: TLS, chunked requests, and response streaming.
    let upstream_target = target.read().await.clone();
    let url = format!("{upstream_target}{path}");
    let Ok(method_parsed) = method.parse::<reqwest::Method>() else {
        return write_simple(&mut client, 400, "bad method").await;
    };
    let mut req = kernel_http().request(method_parsed, &url);
    for (name, value) in &fwd_headers {
        if let Ok(hv) = reqwest::header::HeaderValue::from_str(value) {
            req = req.header(name, hv);
        }
    }
    if !body_bytes.is_empty() {
        req = req.body(body_bytes);
    }

    let resp = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            return write_simple(&mut client, 502, &format!("kernel unreachable: {e}")).await;
        }
    };

    // Status line + forwarded headers, minus the frame blockers. The upstream
    // content-length (when present) passes through as-is; bodies without a
    // known length (SSE, streaming) are re-framed as chunked below.
    let status = resp.status();
    let has_content_length = resp
        .headers()
        .get(reqwest::header::CONTENT_LENGTH)
        .is_some();
    let mut head = format!("HTTP/1.1 {status}\r\n");
    for (name, value) in resp.headers() {
        let lower = name.as_str().to_ascii_lowercase();
        if STRIP.contains(&lower.as_str()) || lower == "transfer-encoding" {
            continue;
        }
        if let Ok(vs) = value.to_str() {
            head.push_str(&format!("{name}: {vs}\r\n"));
        }
    }
    if !has_content_length {
        // Length unknown until the stream is consumed → chunked to the
        // browser. Never combined with content-length (RFC 9110 forbids it).
        head.push_str("Transfer-Encoding: chunked\r\n");
    }
    head.push_str("Connection: close\r\n\r\n");
    client.write_all(head.as_bytes()).await?;

    // Forward the body chunk-for-chunk so SSE works. With content-length the
    // chunks are raw; otherwise they carry the chunked framing.
    let mut stream = resp.bytes_stream();
    while let Some(item) = stream.next().await {
        let Ok(bytes) = item else { break };
        if !has_content_length {
            client
                .write_all(format!("{:x}\r\n", bytes.len()).as_bytes())
                .await?;
        }
        client.write_all(&bytes).await?;
        if !has_content_length {
            client.write_all(b"\r\n").await?;
        }
    }
    if !has_content_length {
        client.write_all(b"0\r\n\r\n").await.ok();
    }
    client.flush().await.ok();
    Ok(())
}

fn find_header_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4)
}

fn kernel_http() -> reqwest::Client {
    // 5s connect timeout; body streaming is unbounded.
    reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(5))
        .build()
        .expect("static client")
}

async fn write_simple(client: &mut TcpStream, code: u16, text: &str) -> std::io::Result<()> {
    let body = format!("{{\"error\":\"{text}\"}}");
    let head = format!(
        "HTTP/1.1 {code} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        http_reason(code),
        body.len()
    );
    client.write_all(head.as_bytes()).await?;
    client.write_all(body.as_bytes()).await?;
    client.flush().await
}

/// Send the iframe back under the token prefix for the same resource.
async fn write_redirect(client: &mut TcpStream, location: &str) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    );
    client.write_all(head.as_bytes()).await?;
    client.flush().await
}

fn http_reason(code: u16) -> &'static str {
    match code {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        431 => "Request Header Fields Too Large",
        502 => "Bad Gateway",
        _ => "Error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn strips_frame_headers_and_forwards_body() {
        // In-process fake kernel: one request, one response with the hostile
        // headers, then check what the proxy sent back.
        let fake = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let fake_port = fake.local_addr().unwrap().port();
        let fake_task = tokio::spawn(async move {
            let (mut sock, _) = fake.accept().await.unwrap();
            // Read until end of headers — reqwest may split the request
            // across multiple TCP segments.
            let mut buf = Vec::new();
            let mut tmp = [0u8; 4096];
            while find_header_end(&buf).is_none() {
                let n = sock.read(&mut tmp).await.unwrap();
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
            }
            let req = String::from_utf8_lossy(&buf).to_string();
            let resp = "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nX-Frame-Options: DENY\r\nContent-Security-Policy: frame-ancestors 'none'\r\nContent-Length: 5\r\n\r\nhello";
            sock.write_all(resp.as_bytes()).await.unwrap();
            req
        });

        // The proxy target is a shared string; fake kernel directly.
        let target: SharedTarget = Arc::new(RwLock::new(format!("http://127.0.0.1:{fake_port}")));
        let token = Arc::new("testtoken".to_string());

        // Client side of the proxied conversation.
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_port = proxy_listener.local_addr().unwrap().port();
        let proxy_task = tokio::spawn(async move {
            let (sock, _) = proxy_listener.accept().await.unwrap();
            let token = Arc::clone(&token);
            handle_conn(sock, target, token).await.unwrap();
        });

        let mut c = TcpStream::connect(("127.0.0.1", proxy_port)).await.unwrap();
        c.write_all(
            b"GET /embed-testtoken/web/channels.html HTTP/1.1\r\nHost: x\r\nCookie: a=b\r\n\r\n",
        )
        .await
        .unwrap();
        let mut got = Vec::new();
        c.read_to_end(&mut got).await.unwrap();
        let text = String::from_utf8_lossy(&got).to_string();

        let upstream_req = fake_task.await.unwrap();
        assert!(upstream_req.contains("GET /web/channels.html HTTP/1.1"), "{upstream_req}");
        // reqwest normalizes header names to lowercase on the wire.
        let upstream_lower = upstream_req.to_lowercase();
        assert!(upstream_lower.contains("cookie: a=b"), "cookies pass through: {upstream_req}");
        assert!(!text.to_lowercase().contains("x-frame-options"), "{text}");
        assert!(!text.to_lowercase().contains("content-security-policy"), "{text}");
        // Header names are forwarded in whatever case reqwest emits (lowercase).
        assert!(
            text.to_lowercase().contains("content-type: text/html"),
            "{text}"
        );
        assert!(text.ends_with("hello"), "body intact: {text}");
        proxy_task.await.unwrap();
    }

    #[tokio::test]
    async fn forwards_post_body_and_length() {
        // POST with a JSON body must arrive intact: path rewrite, headers,
        // and exactly content-length bytes. A regression here drops the
        // login request inside the iframe.
        let fake = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let fake_port = fake.local_addr().unwrap().port();
        let body = br#"{"mode":"admin","password":"secret"}"#;
        let body_len = body.len();
        let fake_task = tokio::spawn(async move {
            let (mut sock, _) = fake.accept().await.unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 4096];
            while buf.len() < body_len || find_header_end(&buf).is_none() {
                let n = sock.read(&mut tmp).await.unwrap();
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
                // Enough once headers + full body are both in.
                if let Some(h) = find_header_end(&buf) {
                    if buf.len() >= h + body_len {
                        break;
                    }
                }
            }
            let req = String::from_utf8_lossy(&buf).to_string();
            sock.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}",
            )
            .await
            .unwrap();
            req
        });

        let target: SharedTarget = Arc::new(RwLock::new(format!("http://127.0.0.1:{fake_port}")));
        let token = Arc::new("tok".to_string());
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_port = proxy_listener.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            let (sock, _) = proxy_listener.accept().await.unwrap();
            handle_conn(sock, target, token).await.unwrap();
        });

        let req = format!(
            "POST /embed-tok/login HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: {body_len}\r\n\r\n"
        );
        let mut c = TcpStream::connect(("127.0.0.1", proxy_port)).await.unwrap();
        c.write_all(req.as_bytes()).await.unwrap();
        c.write_all(body).await.unwrap();
        let mut got = Vec::new();
        c.read_to_end(&mut got).await.unwrap();

        let upstream = fake_task.await.unwrap();
        assert!(upstream.contains("POST /login HTTP/1.1"), "{upstream}");
        assert!(
            upstream.ends_with(r#""password":"secret"}"#),
            "body forwarded intact: {upstream}"
        );
        task.await.unwrap();
    }

    #[tokio::test]
    async fn rejects_wrong_token() {
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_port = proxy_listener.local_addr().unwrap().port();
        let target: SharedTarget = Arc::new(RwLock::new("http://127.0.0.1:1".to_string()));
        let token = Arc::new("right".to_string());
        let task = tokio::spawn(async move {
            let (sock, _) = proxy_listener.accept().await.unwrap();
            handle_conn(sock, target, token).await.unwrap();
        });
        let mut c = TcpStream::connect(("127.0.0.1", proxy_port)).await.unwrap();
        c.write_all(b"GET /embed-wrong/x HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        let mut got = Vec::new();
        c.read_to_end(&mut got).await.unwrap();
        let text = String::from_utf8_lossy(&got).to_string();
        assert!(text.starts_with("HTTP/1.1 403"), "{text}");
        task.await.unwrap();
    }

    #[tokio::test]
    async fn absolute_subresources_pass_with_embed_referer() {
        // The kernel's pages load assets by root-absolute path, so the
        // browser asks for /web/assets/... with no token in the URL. With
        // the embed document as Referer it must be forwarded as-is; with a
        // missing or foreign Referer it must stay refused.
        let fake = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let fake_port = fake.local_addr().unwrap().port();
        let fake_task = tokio::spawn(async move {
            let (mut sock, _) = fake.accept().await.unwrap();
            let mut buf = Vec::new();
            let mut tmp = [0u8; 4096];
            while find_header_end(&buf).is_none() {
                let n = sock.read(&mut tmp).await.unwrap();
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
            }
            let req = String::from_utf8_lossy(&buf).to_string();
            sock.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/css\r\nContent-Length: 3\r\n\r\nok!",
            )
            .await
            .unwrap();
            req
        });

        let target: SharedTarget = Arc::new(RwLock::new(format!("http://127.0.0.1:{fake_port}")));
        let token = Arc::new("tok".to_string());
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_port = proxy_listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            // Two connections: the legitimate sub-resource, then the
            // token-less probe with no Referer.
            for _ in 0..2 {
                let Ok((sock, _)) = proxy_listener.accept().await else {
                    break;
                };
                let target = Arc::clone(&target);
                let token = Arc::clone(&token);
                tokio::spawn(async move {
                    let _ = handle_conn(sock, target, token).await;
                });
            }
        });

        // 1) Sub-resource with the embed document as Referer → forwarded.
        let mut c = TcpStream::connect(("127.0.0.1", proxy_port)).await.unwrap();
        c.write_all(
            b"GET /web/assets/css/styles.css?v=dev HTTP/1.1\r\nHost: x\r\nReferer: http://127.0.0.1:1/embed-tok/web/channels.html\r\n\r\n",
        )
        .await
        .unwrap();
        let mut got = Vec::new();
        c.read_to_end(&mut got).await.unwrap();
        let ok = String::from_utf8_lossy(&got).to_string();
        assert!(ok.starts_with("HTTP/1.1 200"), "{ok}");
        assert!(ok.ends_with("ok!"), "css body intact: {ok}");

        // 2) Same path, no Referer → refused, fake kernel never sees it.
        let mut c = TcpStream::connect(("127.0.0.1", proxy_port)).await.unwrap();
        c.write_all(b"GET /web/assets/css/styles.css HTTP/1.1\r\nHost: x\r\n\r\n")
            .await
            .unwrap();
        let mut got = Vec::new();
        c.read_to_end(&mut got).await.unwrap();
        let denied = String::from_utf8_lossy(&got).to_string();
        assert!(denied.starts_with("HTTP/1.1 403"), "{denied}");

        let upstream = fake_task.await.unwrap();
        assert!(
            upstream.contains("GET /web/assets/css/styles.css?v=dev HTTP/1.1"),
            "absolute path forwarded unchanged: {upstream}"
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn foreign_referer_does_not_open_the_gate() {
        // A Referer mentioning the token but hosted elsewhere (or a token
        // guess) must not pass: the prefix must appear in a loopback URL.
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_port = proxy_listener.local_addr().unwrap().port();
        let target: SharedTarget = Arc::new(RwLock::new("http://127.0.0.1:1".to_string()));
        let token = Arc::new("right".to_string());
        let task = tokio::spawn(async move {
            let (sock, _) = proxy_listener.accept().await.unwrap();
            handle_conn(sock, target, token).await.unwrap();
        });
        let mut c = TcpStream::connect(("127.0.0.1", proxy_port)).await.unwrap();
        c.write_all(
            b"GET /x HTTP/1.1\r\nHost: x\r\nReferer: http://127.0.0.1:5/embed-wrong/x\r\n\r\n",
        )
        .await
        .unwrap();
        let mut got = Vec::new();
        c.read_to_end(&mut got).await.unwrap();
        let text = String::from_utf8_lossy(&got).to_string();
        assert!(text.starts_with("HTTP/1.1 403"), "{text}");
        task.await.unwrap();
    }

    #[tokio::test]
    async fn token_less_navigations_bounce_back_under_the_token() {
        // location.href = '/web/login.html' from inside the iframe: the
        // navigation carries the embed Referer, so instead of serving a
        // token-less document (whose sub-resources would then fail the
        // gate), redirect to the same path under /embed-<token>/.
        let proxy_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_port = proxy_listener.local_addr().unwrap().port();
        let target: SharedTarget = Arc::new(RwLock::new("http://127.0.0.1:1".to_string()));
        let token = Arc::new("tok".to_string());
        let task = tokio::spawn(async move {
            let (sock, _) = proxy_listener.accept().await.unwrap();
            handle_conn(sock, target, token).await.unwrap();
        });
        let mut c = TcpStream::connect(("127.0.0.1", proxy_port)).await.unwrap();
        c.write_all(
            b"GET /web/login.html HTTP/1.1\r\nHost: x\r\nSec-Fetch-Dest: document\r\nAccept: text/html\r\nReferer: http://127.0.0.1:1/embed-tok/web/channels.html\r\n\r\n",
        )
        .await
        .unwrap();
        let mut got = Vec::new();
        c.read_to_end(&mut got).await.unwrap();
        let text = String::from_utf8_lossy(&got).to_string();
        assert!(text.starts_with("HTTP/1.1 302"), "{text}");
        assert!(
            text.contains("Location: /embed-tok/web/login.html"),
            "{text}"
        );
        task.await.unwrap();
    }
}
