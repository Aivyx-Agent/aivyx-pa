//! HTTP transport seam for the model providers.
//!
//! The provider does not own a `reqwest::Client` directly — it owns a
//! `Box<dyn HttpTransport>`. The real implementation (`ReqwestTransport`)
//! wraps `reqwest`; a test fake (`FakeTransport`, in the test module of
//! `provider.rs`) replays canned bytes.
//!
//! This split is what lets the SSE parser and the `LlmProvider` impl be
//! tested exhaustively with zero network. The OpenAI-compatible and
//! Ollama providers and the embedding client share it.

use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::Stream;
use tokio_util::sync::CancellationToken;

use aivyx_route::{Resolve, SystemResolver};

use crate::LlmError;
use crate::local_only::{check_url_async, model_client_builder_with};

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
/// audit 2026-11-11, X1). For a **model** URL use
/// [`model_client_builder`](crate::local_only::model_client_builder),
/// which also refuses redirects and non-local addresses.
pub fn direct_client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder().no_proxy()
}

/// Production transport: one `reqwest::Client` (rustls) built with
/// [`model_client_builder_with`]: no proxy (every model is local since
/// 2.0), no redirects, and a connect-time resolver that only returns local
/// addresses. Every request first re-checks its URL with `require_local`
/// (in `spawn_blocking`), because reqwest skips the resolver for an
/// IP-literal URL and a provider's URL may have been built unchecked.
pub struct ReqwestTransport {
    client: reqwest::Client,
    resolver: Arc<dyn Resolve>,
}

impl ReqwestTransport {
    pub fn new() -> Result<Self, LlmError> {
        Self::with_resolver(Arc::new(SystemResolver))
    }

    /// [`Self::new`] over `resolver`, for both the per-request URL check
    /// and the connect-time resolver (a test seam).
    pub fn with_resolver(resolver: Arc<dyn Resolve>) -> Result<Self, LlmError> {
        let client = model_client_builder_with(Arc::clone(&resolver))
            .user_agent(concat!("aivyx-llm/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| LlmError::Config(format!("reqwest client build failed: {e}")))?;
        Ok(ReqwestTransport { client, resolver })
    }

    /// Uses `client` for every request. The per-request local-only URL
    /// check still applies; the caller owns the client's redirect and
    /// resolver settings.
    pub fn from_client(client: reqwest::Client) -> Self {
        ReqwestTransport {
            client,
            resolver: Arc::new(SystemResolver),
        }
    }

    /// `require_local` on `url`, off the async runtime.
    async fn check(&self, url: &str) -> Result<(), LlmError> {
        check_url_async(url, Arc::clone(&self.resolver)).await
    }
}

/// A send error, with its source chain (the local-only resolver's refusal
/// is a source, not the top-level message).
fn send_error(e: reqwest::Error) -> LlmError {
    let mut text = e.to_string();
    let mut source = std::error::Error::source(&e);
    while let Some(s) = source {
        text.push_str(": ");
        text.push_str(&s.to_string());
        source = s.source();
    }
    LlmError::Transport(text)
}

/// A redirect is refused, not followed (the policy is `none`, so reqwest
/// hands it back as the response): following it would re-send the request
/// to whatever host the server names.
fn refuse_redirect(response: &reqwest::Response) -> Result<(), LlmError> {
    let status = response.status();
    if status.is_redirection() {
        let to = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("<none>");
        return Err(LlmError::Api {
            status: status.as_u16(),
            message: format!("the model server redirected to {to}; aivyx doesn't follow redirects"),
        });
    }
    Ok(())
}

#[async_trait]
impl HttpTransport for ReqwestTransport {
    async fn get_text(&self, url: &str) -> Result<String, LlmError> {
        self.check(url).await?;
        let response = self.client.get(url).send().await.map_err(send_error)?;
        refuse_redirect(&response)?;

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
        // Before each stream (re)connect: every chat stream is a new POST.
        self.check(url).await?;

        let mut request = self.client.post(url).body(body);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }

        let response = request.send().await.map_err(send_error)?;
        refuse_redirect(&response)?;

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
        self.check(url).await?;
        let mut request = self.client.post(url).body(body);
        for (name, value) in headers {
            request = request.header(*name, *value);
        }
        let response = request.send().await.map_err(send_error)?;
        refuse_redirect(&response)?;
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
