//! Phase 75 — embedding provider for semantic memory search.
//!
//! An OpenAI-compatible `/v1/embeddings` HTTP client built on
//! the shared [`crate::transport::HttpTransport`]. The
//! `base_url` is operator-configured and, since 2.0, required and
//! local: a server on this machine or the operator's own network
//! (ollama, llama.cpp, text-embeddings-inference). It is checked
//! when the provider is built and before every request.
//!
//! Zero new workspace deps: this reuses the existing
//! `reqwest`/rustls transport the chat providers use.

use async_trait::async_trait;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::transport::{HttpTransport, ReqwestTransport};
use crate::LlmError;

/// Failure modes for an embedding call. Mirrors the
/// notify-error taxonomy so callers can classify
/// retryable-vs-not uniformly.
#[derive(Debug, Clone, thiserror::Error)]
pub enum EmbeddingError {
    /// Connection / protocol failure (DNS, TLS, socket close).
    /// Retryable.
    #[error("embedding transport error: {0}")]
    Transport(String),
    /// The endpoint rejected the credentials. Not retryable
    /// without operator intervention.
    #[error("embedding auth error: {0}")]
    Auth(String),
    /// The endpoint accepted the connection but refused the
    /// request (4xx / 5xx). Carries the HTTP status.
    #[error("embedding rejected (status {0})")]
    Rejected(u16),
    /// Operation timed out / was cancelled.
    #[error("embedding timeout")]
    Timeout,
    /// The response wasn't the shape we expected (missing
    /// `data[].embedding`, wrong dimension count, non-JSON).
    #[error("embedding response malformed: {0}")]
    Malformed(String),
}

/// Abstraction over an embedding backend so the daemon's
/// write/backfill/search paths can be unit-tested without a
/// real HTTP server. Production: [`OpenAiEmbeddingProvider`].
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    /// Embed a batch of texts. Returns one vector per input,
    /// in input order. An empty input slice returns an empty
    /// vec (no network call).
    async fn embed(
        &self,
        texts: &[String],
    ) -> Result<Vec<Vec<f32>>, EmbeddingError>;

    /// The model identifier (for diagnostics + the
    /// dimension-mismatch detection in the vector store).
    fn model(&self) -> &str;

    /// Expected output dimensionality. The vector store uses
    /// this to detect a model swap (old vectors with a
    /// different length are treated as unembedded).
    fn dimensions(&self) -> usize;
}

/// OpenAI-compatible `/v1/embeddings` provider.
pub struct OpenAiEmbeddingProvider {
    base_url: String,
    model: String,
    api_key: Option<SecretString>,
    dimensions: usize,
    transport: Box<dyn HttpTransport>,
}

impl OpenAiEmbeddingProvider {
    /// `base_url` and `model` are required (2.0: no cloud default) and must be local
    /// (`require_local`; blocking DNS for a name that isn't local by
    /// spelling).
    ///
    /// Blocking DNS: never call from async code (daemon startup builds it).
    pub fn new(
        base_url: Option<String>,
        model: String,
        api_key: Option<SecretString>,
        dimensions: usize,
    ) -> Result<Self, LlmError> {
        let base_url = base_url.ok_or_else(|| {
            LlmError::Config(
                "the embedding provider needs a base_url (a server on this machine or your own network)"
                    .to_string(),
            )
        })?;
        crate::local_only::check_url(&base_url)?;
        Ok(Self {
            base_url,
            model,
            api_key,
            dimensions,
            transport: Box::new(ReqwestTransport::new()?),
        })
    }

    /// Test seam — inject a fake transport.
    pub fn with_transport(
        base_url: String,
        model: String,
        api_key: Option<SecretString>,
        dimensions: usize,
        transport: Box<dyn HttpTransport>,
    ) -> Self {
        Self {
            base_url,
            model,
            api_key,
            dimensions,
            transport,
        }
    }

    fn endpoint(&self) -> String {
        format!("{}/v1/embeddings", self.base_url.trim_end_matches('/'))
    }
}

#[derive(Deserialize)]
struct EmbeddingResponse {
    data: Vec<EmbeddingDatum>,
}

#[derive(Deserialize)]
struct EmbeddingDatum {
    embedding: Vec<f32>,
}

/// Map an `LlmError` from the transport into the embedding
/// taxonomy. The transport raises `Api { status, .. }` for
/// non-2xx; classify 401/403 as Auth, everything else as
/// Rejected.
fn map_transport_error(e: LlmError) -> EmbeddingError {
    match e {
        LlmError::Cancelled => EmbeddingError::Timeout,
        LlmError::Transport(s) => EmbeddingError::Transport(s),
        LlmError::Api { status, message } => {
            if status == 401 || status == 403 {
                EmbeddingError::Auth(message)
            } else {
                EmbeddingError::Rejected(status)
            }
        }
        other => EmbeddingError::Transport(other.to_string()),
    }
}

#[async_trait]
impl EmbeddingProvider for OpenAiEmbeddingProvider {
    async fn embed(
        &self,
        texts: &[String],
    ) -> Result<Vec<Vec<f32>>, EmbeddingError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let body = json!({
            "model": self.model,
            "input": texts,
        });
        let body_bytes = serde_json::to_vec(&body).map_err(|e| {
            EmbeddingError::Malformed(format!("request encode: {e}"))
        })?;

        let auth_header;
        let mut headers: Vec<(&str, &str)> =
            vec![("content-type", "application/json")];
        if let Some(key) = &self.api_key {
            auth_header = format!("Bearer {}", key.expose_secret());
            headers.push(("authorization", auth_header.as_str()));
        }

        let cancellation = CancellationToken::new();
        let raw = self
            .transport
            .post_json(
                &self.endpoint(),
                &headers,
                body_bytes,
                &cancellation,
            )
            .await
            .map_err(map_transport_error)?;

        let parsed: EmbeddingResponse = serde_json::from_slice(&raw)
            .map_err(|e| {
                EmbeddingError::Malformed(format!("response decode: {e}"))
            })?;
        if parsed.data.len() != texts.len() {
            return Err(EmbeddingError::Malformed(format!(
                "expected {} embeddings, got {}",
                texts.len(),
                parsed.data.len()
            )));
        }
        Ok(parsed.data.into_iter().map(|d| d.embedding).collect())
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn dimensions(&self) -> usize {
        self.dimensions
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::ByteStream;

    /// Fake transport returning a canned JSON body for
    /// post_json. post_sse is unused here.
    struct FakeTransport {
        response: Result<Vec<u8>, LlmError>,
    }

    #[async_trait]
    impl HttpTransport for FakeTransport {
        async fn post_sse(
            &self,
            _url: &str,
            _headers: &[(&str, &str)],
            _body: Vec<u8>,
            _cancellation: &CancellationToken,
        ) -> Result<ByteStream, LlmError> {
            Err(LlmError::Transport("unused".into()))
        }

        async fn post_json(
            &self,
            _url: &str,
            _headers: &[(&str, &str)],
            _body: Vec<u8>,
            _cancellation: &CancellationToken,
        ) -> Result<Vec<u8>, LlmError> {
            self.response.clone()
        }
    }

    fn provider(resp: Result<Vec<u8>, LlmError>) -> OpenAiEmbeddingProvider {
        OpenAiEmbeddingProvider::with_transport(
            "http://localhost:1234".into(),
            "test-model".into(),
            None,
            3,
            Box::new(FakeTransport { response: resp }),
        )
    }

    #[tokio::test]
    async fn embed_empty_input_skips_network() {
        // The fake would error on a call; an empty input must
        // not reach it.
        let p = provider(Err(LlmError::Transport("should not fire".into())));
        let out = p.embed(&[]).await.unwrap();
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn embed_parses_data_embeddings_in_order() {
        let body = serde_json::to_vec(&json!({
            "data": [
                { "embedding": [0.1, 0.2, 0.3] },
                { "embedding": [0.4, 0.5, 0.6] }
            ]
        }))
        .unwrap();
        let p = provider(Ok(body));
        let out = p
            .embed(&["a".to_string(), "b".to_string()])
            .await
            .unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0], vec![0.1, 0.2, 0.3]);
        assert_eq!(out[1], vec![0.4, 0.5, 0.6]);
    }

    #[tokio::test]
    async fn embed_count_mismatch_is_malformed() {
        let body = serde_json::to_vec(&json!({
            "data": [ { "embedding": [0.1, 0.2, 0.3] } ]
        }))
        .unwrap();
        let p = provider(Ok(body));
        // Asked for 2, got 1.
        let err = p
            .embed(&["a".to_string(), "b".to_string()])
            .await
            .expect_err("must error");
        assert!(matches!(err, EmbeddingError::Malformed(_)));
    }

    #[tokio::test]
    async fn embed_non_json_is_malformed() {
        let p = provider(Ok(b"not json at all".to_vec()));
        let err = p
            .embed(&["a".to_string()])
            .await
            .expect_err("must error");
        assert!(matches!(err, EmbeddingError::Malformed(_)));
    }

    #[tokio::test]
    async fn embed_401_maps_to_auth() {
        let p = provider(Err(LlmError::Api {
            status: 401,
            message: "bad key".into(),
        }));
        let err = p
            .embed(&["a".to_string()])
            .await
            .expect_err("must error");
        assert!(matches!(err, EmbeddingError::Auth(_)));
    }

    #[tokio::test]
    async fn embed_500_maps_to_rejected() {
        let p = provider(Err(LlmError::Api {
            status: 503,
            message: "overloaded".into(),
        }));
        let err = p
            .embed(&["a".to_string()])
            .await
            .expect_err("must error");
        assert!(matches!(err, EmbeddingError::Rejected(503)));
    }

    #[tokio::test]
    async fn embed_cancelled_maps_to_timeout() {
        let p = provider(Err(LlmError::Cancelled));
        let err = p
            .embed(&["a".to_string()])
            .await
            .expect_err("must error");
        assert!(matches!(err, EmbeddingError::Timeout));
    }

    #[test]
    fn endpoint_strips_trailing_slash() {
        let p = OpenAiEmbeddingProvider::with_transport(
            "http://localhost:1234/".into(),
            "m".into(),
            None,
            3,
            Box::new(FakeTransport {
                response: Ok(vec![]),
            }),
        );
        assert_eq!(p.endpoint(), "http://localhost:1234/v1/embeddings");
    }

    #[test]
    fn model_and_dimensions_accessors() {
        let p = provider(Ok(vec![]));
        assert_eq!(p.model(), "test-model");
        assert_eq!(p.dimensions(), 3);
    }
}
