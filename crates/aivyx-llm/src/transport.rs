//! HTTP transport seam for the Anthropic provider.
//!
//! The provider does not own a `reqwest::Client` directly — it owns a
//! `Box<dyn HttpTransport>`. The real implementation (`ReqwestTransport`)
//! wraps `reqwest`; a test fake (`FakeTransport`, in the test module of
//! `provider.rs`) replays canned bytes.
//!
//! This split is what lets the SSE parser and the `LlmProvider` impl be
//! tested exhaustively with zero network. It also makes a second vendor
//! (OpenAI, Ollama, ...) trivially reuse the same transport.

use std::pin::Pin;

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::Stream;
use tokio_util::sync::CancellationToken;

use crate::LlmError;

/// A byte stream returned by [`HttpTransport::post_sse`]. Wraps
/// `reqwest::Response::bytes_stream()` in the real path; in tests it wraps
/// an `IntoIter` over pre-canned `Bytes` chunks.
///
/// The SSE parser reads from this stream and splits it into events — it
/// does *not* assume each `Bytes` chunk contains one complete event.
pub type ByteStream =
    Pin<Box<dyn Stream<Item = Result<Bytes, LlmError>> + Send + 'static>>;

/// The transport seam. Two methods:
///
/// - `post_sse` — POST a JSON body, read back an SSE byte stream.
/// - `get_text` — GET a URL and return the response body as a string.
///   Used for lightweight health checks (e.g. Ollama's root endpoint
///   returns `"Ollama is running"`).
#[async_trait]
pub trait HttpTransport: Send + Sync {
    async fn post_sse(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        body: Vec<u8>,
        cancellation: &CancellationToken,
    ) -> Result<ByteStream, LlmError>;

    /// Simple GET returning the response body as a `String`.
    /// Default implementation returns `LlmError::Transport` so
    /// test fakes that don't need it can skip the method.
    async fn get_text(&self, _url: &str) -> Result<String, LlmError> {
        Err(LlmError::Transport(
            "get_text not implemented on this transport".to_string(),
        ))
    }

    /// Phase 75 — non-streaming POST returning the full
    /// response body as bytes. Used by the embedding provider
    /// (`POST /v1/embeddings` is request/response, not SSE).
    /// Default impl errors so streaming-only test fakes can
    /// skip it.
    async fn post_json(
        &self,
        _url: &str,
        _headers: &[(&str, &str)],
        _body: Vec<u8>,
        _cancellation: &CancellationToken,
    ) -> Result<Vec<u8>, LlmError> {
        Err(LlmError::Transport(
            "post_json not implemented on this transport".to_string(),
        ))
    }
}

// ---------------------------------------------------------------------------
// Real implementation: reqwest
// ---------------------------------------------------------------------------

/// A client builder that never uses a proxy, for services that are always
/// on this machine or network (setup and doctor probes, discovery).
///
/// reqwest otherwise sends even loopback requests -- prompts, keys -- to
/// `HTTP_PROXY`/`HTTPS_PROXY`/`ALL_PROXY` when one is set (aivyx-vision
/// audit 2026-11-11, X1).
pub fn direct_client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder().no_proxy()
}

/// A client builder for `url`: direct when `url` is a local address
/// (aivyx-route's `url_locality`, the rule routing uses), otherwise
/// honouring a configured proxy, which people behind a corporate proxy
/// need for cloud APIs.
pub fn client_builder_for(url: &str) -> reqwest::ClientBuilder {
    if is_local_url(url) {
        direct_client_builder()
    } else {
        reqwest::Client::builder()
    }
}

fn is_local_url(url: &str) -> bool {
    aivyx_route::url_locality(url) == aivyx_route::Locality::Local
}

/// Production transport: a `reqwest::Client` with rustls. It holds two
/// clients and picks one per request: `direct` (no proxy) for a local
/// address, `client` (honouring a configured proxy) for anything else --
/// see [`client_builder_for`].
pub struct ReqwestTransport {
    client: reqwest::Client,
    direct: reqwest::Client,
}

impl ReqwestTransport {
    pub fn new() -> Result<Self, LlmError> {
        let build = |builder: reqwest::ClientBuilder| {
            builder
                .user_agent(concat!("aivyx-llm/", env!("CARGO_PKG_VERSION")))
                .build()
                .map_err(|e| LlmError::Config(format!("reqwest client build failed: {e}")))
        };
        Ok(ReqwestTransport {
            client: build(reqwest::Client::builder())?,
            direct: build(direct_client_builder())?,
        })
    }

    /// Uses `client` for every request, local or not.
    pub fn from_client(client: reqwest::Client) -> Self {
        ReqwestTransport {
            direct: client.clone(),
            client,
        }
    }

    fn client_for(&self, url: &str) -> &reqwest::Client {
        if is_local_url(url) {
            &self.direct
        } else {
            &self.client
        }
    }
}

#[async_trait]
impl HttpTransport for ReqwestTransport {
    async fn get_text(&self, url: &str) -> Result<String, LlmError> {
        let response = self
            .client_for(url)
            .get(url)
            .send()
            .await
            .map_err(|e| LlmError::Transport(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "<no body>".to_string());
            return Err(LlmError::Api {
                status: status.as_u16(),
                message: body,
            });
        }

        response
            .text()
            .await
            .map_err(|e| LlmError::Transport(e.to_string()))
    }

    async fn post_sse(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        body: Vec<u8>,
        cancellation: &CancellationToken,
    ) -> Result<ByteStream, LlmError> {
        use futures_util::StreamExt;

        if cancellation.is_cancelled() {
            return Err(LlmError::Cancelled);
        }

        let mut request = self.client_for(url).post(url).body(body);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }

        let response = request
            .send()
            .await
            .map_err(|e| LlmError::Transport(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let message: String = response
                .text()
                .await
                .unwrap_or_else(|_| "<no body>".to_string());
            return Err(LlmError::Api {
                status: status.as_u16(),
                message,
            });
        }

        // `bytes_stream` yields `Result<Bytes, reqwest::Error>`; map the
        // error into our `LlmError::Transport` variant.
        let stream = response.bytes_stream().map(
            |res: Result<Bytes, reqwest::Error>| -> Result<Bytes, LlmError> {
                res.map_err(|e| LlmError::Transport(e.to_string()))
            },
        );

        Ok(Box::pin(stream))
    }

    async fn post_json(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        body: Vec<u8>,
        cancellation: &CancellationToken,
    ) -> Result<Vec<u8>, LlmError> {
        if cancellation.is_cancelled() {
            return Err(LlmError::Cancelled);
        }
        let mut request = self.client_for(url).post(url).body(body);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = request
            .send()
            .await
            .map_err(|e| LlmError::Transport(e.to_string()))?;
        let status = response.status();
        if !status.is_success() {
            let message: String = response
                .text()
                .await
                .unwrap_or_else(|_| "<no body>".to_string());
            return Err(LlmError::Api {
                status: status.as_u16(),
                message,
            });
        }
        let bytes = response
            .bytes()
            .await
            .map_err(|e| LlmError::Transport(e.to_string()))?;
        Ok(bytes.to_vec())
    }
}
