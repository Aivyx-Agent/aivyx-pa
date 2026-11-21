//! 2.0 (local-only): every model provider refuses a non-local URL when it
//! is built, re-checks before each request, never follows a redirect, and
//! only connects to local addresses. No network: IP literals, known cloud
//! hosts (refused without a lookup) and a stub resolver.
#![cfg(all(feature = "provider-openai", feature = "provider-ollama"))]

use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use aivyx_llm::embedding::OpenAiEmbeddingProvider;
use aivyx_llm::ollama::{OllamaConfig, OllamaProvider};
use aivyx_llm::openai::{OpenAiConfig, OpenAiProvider};
use aivyx_llm::transport::{HttpTransport, ReqwestTransport};
use aivyx_llm::{LlmError, LlmMessage, LlmProvider, LlmRequest};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

const TAIL: &str =
    " — aivyx is local-only; point it at a model server on this machine or your own network";

fn not_local(err: LlmError) -> String {
    match err {
        LlmError::NotLocal(text) => text,
        other => panic!("expected NotLocal, got {other:?}"),
    }
}

#[test]
fn every_constructor_refuses_a_cloud_or_public_url() {
    let openai = |url: &str| {
        OpenAiProvider::new(OpenAiConfig::without_api_key().with_base_url(url)).err().unwrap()
    };
    assert_eq!(
        not_local(openai("https://api.openai.com")),
        format!("https://api.openai.com/ is not local (cloud host){TAIL}")
    );
    assert_eq!(
        not_local(openai("https://user:pw@api.anthropic.com/v1")),
        format!("https://api.anthropic.com/v1 is not local (cloud host){TAIL}")
    );
    assert!(not_local(openai("http://203.0.113.7:8080")).contains("public address 203.0.113.7"));
    assert!(not_local(openai("ftp://127.0.0.1")).contains("not an http(s) URL"));

    let ollama = OllamaProvider::new(
        OllamaConfig::default_local().with_base_url("https://api.openai.com".to_string()),
    )
    .err()
    .unwrap();
    assert!(not_local(ollama).contains("(cloud host)"));
    let ollama = OllamaProvider::new(
        OllamaConfig::default_local().with_base_url("http://8.8.8.8:11434".to_string()),
    )
    .err()
    .unwrap();
    assert!(not_local(ollama).contains("public address 8.8.8.8"));

    let embed = OpenAiEmbeddingProvider::new(
        Some("https://api.openai.com".to_string()),
        None,
        None,
        1536,
    )
    .err()
    .unwrap();
    assert!(not_local(embed).contains("(cloud host)"));
}

#[test]
fn constructors_need_a_url_and_accept_local_ones() {
    let err = OpenAiProvider::new(OpenAiConfig::without_api_key()).err().unwrap();
    assert!(matches!(err, LlmError::Config(ref m) if m.contains("needs a base_url")), "{err:?}");
    let err = OpenAiEmbeddingProvider::new(None, None, None, 768).err().unwrap();
    assert!(matches!(err, LlmError::Config(ref m) if m.contains("needs a base_url")), "{err:?}");

    for url in ["http://127.0.0.1:8080", "http://192.168.1.20:8080", "http://[::1]:8080"] {
        OpenAiProvider::new(OpenAiConfig::without_api_key().with_base_url(url)).unwrap();
        OpenAiEmbeddingProvider::new(Some(url.to_string()), None, None, 768).unwrap();
        OllamaProvider::new(OllamaConfig::default_local().with_base_url(url.to_string())).unwrap();
    }
    // The default Ollama address (localhost) is local by spelling.
    OllamaProvider::new(OllamaConfig::default_local()).unwrap();
}

/// A stub resolver whose answer flips from local to public.
struct Flip {
    public: AtomicBool,
}

impl aivyx_route::Resolve for Flip {
    fn resolve(&self, _host: &str, _port: u16) -> std::io::Result<Vec<IpAddr>> {
        Ok(vec![if self.public.load(Ordering::SeqCst) {
            "203.0.113.7".parse().unwrap()
        } else {
            "127.0.0.1".parse().unwrap()
        }])
    }
}

/// A one-request-at-a-time HTTP server on 127.0.0.1 answering `response`,
/// counting the requests it reads.
async fn server(response: String) -> (u16, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hits);
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut buf = Vec::new();
            let mut chunk = [0u8; 4096];
            // Read the head, then the body by content-length.
            loop {
                let n = socket.read(&mut chunk).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&chunk[..n]);
                let text = String::from_utf8_lossy(&buf).to_string();
                if let Some(end) = text.find("\r\n\r\n") {
                    let len = text[..end]
                        .lines()
                        .find_map(|l| {
                            let l = l.to_ascii_lowercase();
                            l.strip_prefix("content-length:").map(|v| v.trim().parse().unwrap_or(0))
                        })
                        .unwrap_or(0usize);
                    if buf.len() >= end + 4 + len {
                        break;
                    }
                }
            }
            counter.fetch_add(1, Ordering::SeqCst);
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.shutdown().await;
        }
    });
    (port, hits)
}

fn request<'a>(messages: &'a [LlmMessage]) -> LlmRequest<'a> {
    LlmRequest {
        model: "m",
        system: None,
        messages,
        tools: &[],
        max_tokens: 16,
        temperature: None,
        id_slot: None,
        slot_hint: None,
        route: None,
        no_thinking: false,
    }
}

const SSE_OK: &str = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n\
     data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"},\"finish_reason\":\"stop\"}]}\n\n\
     data: [DONE]\n\n";

#[tokio::test]
async fn every_request_rechecks_the_url() {
    let (port, hits) = server(SSE_OK.to_string()).await;
    let flip = Arc::new(Flip {
        public: AtomicBool::new(false),
    });
    let transport = ReqwestTransport::with_resolver(flip.clone()).unwrap();
    // A domain that resolves (stub) to 127.0.0.1, then to a public address.
    let provider = OpenAiProvider::with_transport(
        OpenAiConfig::without_api_key().with_base_url(format!("http://models.example.com:{port}")),
        Box::new(transport),
    );
    let messages = vec![LlmMessage::user_text("hello")];
    let cancel = CancellationToken::new();

    let stream = provider.chat_stream(request(&messages), &cancel).await;
    assert!(stream.is_ok(), "local while it resolves locally: {:?}", stream.err());
    assert_eq!(hits.load(Ordering::SeqCst), 1);

    flip.public.store(true, Ordering::SeqCst);
    let err = provider.chat_stream(request(&messages), &cancel).await.err().unwrap();
    assert!(
        not_local(err).starts_with(&format!(
            "http://models.example.com:{port}/v1/chat/completions is not local (resolves to a public address 203.0.113.7)"
        ))
    );
    assert_eq!(hits.load(Ordering::SeqCst), 1, "nothing was sent after the flip");
}

#[tokio::test]
async fn an_ip_literal_url_built_unchecked_is_still_refused_per_request() {
    // `with_transport` skips the constructor check; the transport doesn't.
    let provider = OpenAiProvider::with_transport(
        OpenAiConfig::without_api_key().with_base_url("http://203.0.113.7:8080"),
        Box::new(ReqwestTransport::new().unwrap()),
    );
    let messages = vec![LlmMessage::user_text("hello")];
    let err = provider
        .chat_stream(request(&messages), &CancellationToken::new())
        .await
        .err()
        .unwrap();
    assert!(not_local(err).contains("public address 203.0.113.7"));
    // An empty base URL (no address) fails closed too: a relative URL
    // reqwest won't send.
    let provider = OpenAiProvider::with_transport(
        OpenAiConfig::without_api_key(),
        Box::new(ReqwestTransport::new().unwrap()),
    );
    let err = provider
        .chat_stream(request(&messages), &CancellationToken::new())
        .await
        .err()
        .unwrap();
    assert!(matches!(err, LlmError::Transport(_) | LlmError::NotLocal(_)), "{err:?}");
}

#[tokio::test]
async fn a_redirect_is_never_followed() {
    let (target_port, target_hits) = server(SSE_OK.to_string()).await;
    let redirect = format!(
        "HTTP/1.1 307 Temporary Redirect\r\nlocation: http://127.0.0.1:{target_port}/v1/chat/completions\r\n\
         content-length: 0\r\nconnection: close\r\n\r\n"
    );
    let (port, hits) = server(redirect.clone()).await;
    let transport = ReqwestTransport::new().unwrap();
    let url = format!("http://127.0.0.1:{port}/v1/chat/completions");
    let cancel = CancellationToken::new();

    let err = transport.post_sse(&url, &[], b"{\"prompt\":\"secret\"}".to_vec(), &cancel).await.err().unwrap();
    assert!(matches!(err, LlmError::Api { status: 307, .. }), "{err:?}");
    let err = transport.post_json(&url, &[], b"{}".to_vec(), &cancel).await.err().unwrap();
    assert!(matches!(err, LlmError::Api { status: 307, .. }), "{err:?}");
    let err = transport.get_text(&url).await.err().unwrap();
    assert!(matches!(err, LlmError::Api { status: 307, .. }), "{err:?}");
    assert_eq!(hits.load(Ordering::SeqCst), 3);
    assert_eq!(target_hits.load(Ordering::SeqCst), 0, "the redirect target was contacted");

    // 308 through a provider too.
    let redirect308 = redirect.replace("307 Temporary Redirect", "308 Permanent Redirect");
    let (port, _) = server(redirect308).await;
    let provider = OpenAiProvider::new(
        OpenAiConfig::without_api_key().with_base_url(format!("http://127.0.0.1:{port}")),
    )
    .unwrap();
    let messages = vec![LlmMessage::user_text("hello")];
    let err = provider.chat_stream(request(&messages), &cancel).await.err().unwrap();
    assert!(matches!(err, LlmError::Api { status: 308, .. }), "{err:?}");
    assert_eq!(target_hits.load(Ordering::SeqCst), 0, "the redirect target was contacted");
}

/// A stub resolver answering a public address for every name.
struct Public;

impl aivyx_route::Resolve for Public {
    fn resolve(&self, _host: &str, _port: u16) -> std::io::Result<Vec<IpAddr>> {
        Ok(vec!["203.0.113.7".parse().unwrap()])
    }
}

/// A `.lan` name is local by spelling, so the URL check passes it with no
/// lookup. The connect-time resolver still resolves it, and refuses the
/// public answer: nothing is sent, and the error is `NotLocal`.
#[tokio::test]
async fn the_connect_time_resolver_catches_a_local_spelling_that_resolves_public() {
    let (port, hits) = server(SSE_OK.to_string()).await;
    let url = format!("http://gpu.lan:{port}");
    aivyx_route::require_local_with(&url, &Public).expect("local by spelling, no lookup");
    let provider = OpenAiProvider::with_transport(
        OpenAiConfig::without_api_key().with_base_url(url),
        Box::new(ReqwestTransport::with_resolver(Arc::new(Public)).unwrap()),
    );
    let messages = vec![LlmMessage::user_text("hello")];
    let err = provider
        .chat_stream(request(&messages), &CancellationToken::new())
        .await
        .err()
        .unwrap();
    let text = not_local(err);
    assert!(text.contains("gpu.lan"), "{text}");
    assert!(text.contains("resolves to a public address 203.0.113.7"), "{text}");
    assert!(text.ends_with(TAIL), "{text}");
    assert_eq!(hits.load(Ordering::SeqCst), 0, "nothing reached the server");
}
