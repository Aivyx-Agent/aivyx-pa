//! pa's LLM transport sends a request for a local address directly and
//! everything else through a configured proxy. reqwest on its own would
//! send even loopback requests -- prompts, keys -- to the proxy
//! (aivyx-vision audit 2026-11-11, X1).
//!
//! Its own test binary: it sets process-wide proxy variables.
#![cfg(feature = "provider-openai")]

use std::time::Duration;

use aivyx_llm::transport::{HttpTransport, ReqwestTransport};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn local_requests_go_direct_and_cloud_requests_use_the_proxy() {
    let proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_url = format!("http://{}", proxy.local_addr().unwrap());
    // SAFETY: this binary runs this one test; nothing else reads the
    // environment concurrently.
    unsafe {
        for var in [
            "HTTP_PROXY",
            "http_proxy",
            "HTTPS_PROXY",
            "https_proxy",
            "ALL_PROXY",
            "all_proxy",
        ] {
            std::env::set_var(var, &proxy_url);
        }
        std::env::remove_var("NO_PROXY");
        std::env::remove_var("no_proxy");
    }
    let (seen_tx, mut seen_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = proxy.accept().await {
            let mut buf = [0u8; 512];
            let n = socket.read(&mut buf).await.unwrap_or(0);
            let line = String::from_utf8_lossy(&buf[..n]).lines().next().unwrap_or("").to_string();
            let _ = seen_tx.send(line);
            let _ = socket
                .write_all(b"HTTP/1.1 502 Bad Gateway\r\ncontent-length: 0\r\n\r\n")
                .await;
        }
    });

    let transport = ReqwestTransport::new().unwrap();
    let cancel = CancellationToken::new();

    // Local: nothing listens on port 9, so the direct request fails to
    // connect and the proxy never sees it.
    let local = transport
        .post_json("http://127.0.0.1:9/v1/chat/completions", &[], b"{}".to_vec(), &cancel)
        .await;
    assert!(local.is_err());
    let leaked = tokio::time::timeout(Duration::from_millis(300), seen_rx.recv()).await;
    assert!(leaked.is_err(), "a local request went through the proxy: {leaked:?}");

    // Cloud: still goes through the configured proxy (people behind a
    // corporate proxy need it).
    let cloud = transport
        .post_json("http://api.example.com/v1/messages", &[], b"{}".to_vec(), &cancel)
        .await;
    assert!(cloud.is_err(), "the fake proxy answers 502");
    let seen = tokio::time::timeout(Duration::from_secs(5), seen_rx.recv())
        .await
        .expect("a cloud request must go through the proxy")
        .unwrap();
    assert!(seen.contains("api.example.com"), "proxy saw: {seen}");
}
