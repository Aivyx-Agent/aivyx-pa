//! Ollama model management tools — Phase 36.
//!
//! Three agent-facing tools for inspecting and managing local
//! Ollama models:
//!
//! - `OllamaListTool` — `GET /api/tags`, returns available models.
//! - `OllamaShowTool` — `POST /api/show`, returns model metadata.
//! - `OllamaPullTool` — `POST /api/pull`, downloads a model.
//!
//! All three are gated to `CEILING_TRUSTED` (and `CEILING_KERNEL`).
//! They use `reqwest::Client` directly — these are Ollama REST API
//! calls, not LLM inference requests, so they don't go through the
//! `LlmProvider` or `HttpTransport` trait.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};

use aivyx_capability::Scope;
use aivyx_core::{AivyxError, Tool, ToolContext, ToolId, ToolOutcome, Verification};

// ---------------------------------------------------------------------------
// Shared constants
// ---------------------------------------------------------------------------

/// Connect timeout for Ollama API calls. Short — Ollama is localhost.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// Request timeout for list/show operations (read-only, fast).
const READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Request timeout for pull operations (model downloads can be large).
const PULL_TIMEOUT: Duration = Duration::from_secs(300);

// ---------------------------------------------------------------------------
// OllamaListTool
// ---------------------------------------------------------------------------

/// Lists locally available Ollama models via `GET /api/tags`.
#[derive(Debug)]
pub struct OllamaListTool {
    id: ToolId,
    schema: Value,
    client: Arc<reqwest::Client>,
    base_url: String,
}

impl OllamaListTool {
    pub fn new(base_url: &str) -> Result<Self, AivyxError> {
        let client = aivyx_llm::transport::client_builder_for(base_url)
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(READ_TIMEOUT)
            .build()
            .map_err(|e| {
                AivyxError::Config(format!(
                    "ollama.list reqwest client build failed: {e}"
                ))
            })?;
        Ok(OllamaListTool {
            id: ToolId::new(),
            schema: json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
            client: Arc::new(client),
            base_url: base_url.trim_end_matches('/').to_string(),
        })
    }

}

#[async_trait]
impl Tool for OllamaListTool {
    fn id(&self) -> ToolId {
        self.id
    }

    fn name(&self) -> &str {
        "ollama.list"
    }

    fn description(&self) -> &str {
        "List locally available Ollama models. No input parameters. \
         Returns a JSON object with a `models` array containing \
         name, size, and modification time for each model."
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn required_scope(&self, _input: &Value) -> Scope {
        Scope::parse("ollama.list").expect("known base")
    }

    // Registered only when the operator's own [provider] kind = "ollama"
    // -- that configuration choice is the opt-in.
    fn auto_grantable_in_backcompat_floor(&self) -> bool {
        true
    }

    async fn execute(&self, _input: Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        let url = format!("{}/api/tags", self.base_url);
        match self.client.get(&url).send().await {
            Ok(resp) => {
                let status = resp.status();
                match resp.text().await {
                    Ok(body) => {
                        if status.is_success() {
                            match serde_json::from_str::<Value>(&body) {
                                Ok(parsed) => ToolOutcome::Completed {
                                    output: parsed,
                                    verified: Verification::NotApplicable,
                                },
                                Err(e) => ToolOutcome::Failed(AivyxError::Tool {
                                    tool: self.id,
                                    detail: format!(
                                        "ollama.list: invalid JSON from {url}: {e}"
                                    ),
                                }),
                            }
                        } else {
                            ToolOutcome::Failed(AivyxError::Tool {
                                tool: self.id,
                                detail: format!(
                                    "ollama.list: HTTP {status} from {url}: {body}"
                                ),
                            })
                        }
                    }
                    Err(e) => ToolOutcome::Failed(AivyxError::Tool {
                        tool: self.id,
                        detail: format!(
                            "ollama.list: failed to read response body from {url}: {e}"
                        ),
                    }),
                }
            }
            Err(e) => ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: format!(
                    "ollama.list: cannot reach {url} — is Ollama running? \
                     Start it with `ollama serve`.\n  (error: {e})"
                ),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// OllamaShowTool
// ---------------------------------------------------------------------------

/// Shows metadata for a specific Ollama model via `POST /api/show`.
#[derive(Debug)]
pub struct OllamaShowTool {
    id: ToolId,
    schema: Value,
    client: Arc<reqwest::Client>,
    base_url: String,
}

impl OllamaShowTool {
    pub fn new(base_url: &str) -> Result<Self, AivyxError> {
        let client = aivyx_llm::transport::client_builder_for(base_url)
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(READ_TIMEOUT)
            .build()
            .map_err(|e| {
                AivyxError::Config(format!(
                    "ollama.show reqwest client build failed: {e}"
                ))
            })?;
        Ok(OllamaShowTool {
            id: ToolId::new(),
            schema: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Model name, e.g. \"llama3.1\" or \"codellama:7b\""
                    }
                },
                "required": ["name"],
                "additionalProperties": false
            }),
            client: Arc::new(client),
            base_url: base_url.trim_end_matches('/').to_string(),
        })
    }

}

#[async_trait]
impl Tool for OllamaShowTool {
    fn id(&self) -> ToolId {
        self.id
    }

    fn name(&self) -> &str {
        "ollama.show"
    }

    fn description(&self) -> &str {
        "Show metadata for an Ollama model. Input: JSON object with \
         a `name` field (e.g. \"llama3.1\"). Returns modelfile, \
         parameters, template, and license information."
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn required_scope(&self, _input: &Value) -> Scope {
        Scope::parse("ollama.show").expect("known base")
    }

    // Registered only when the operator's own [provider] kind = "ollama"
    // -- that configuration choice is the opt-in.
    fn auto_grantable_in_backcompat_floor(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        let name = match input.get("name").and_then(|v| v.as_str()) {
            Some(n) if !n.is_empty() => n,
            _ => {
                return ToolOutcome::Failed(AivyxError::Tool {
                    tool: self.id,
                    detail: "ollama.show: `name` field is required and must \
                             be a non-empty string"
                        .to_string(),
                });
            }
        };

        let url = format!("{}/api/show", self.base_url);
        let body = json!({ "name": name });

        match self.client.post(&url).json(&body).send().await {
            Ok(resp) => {
                let status = resp.status();
                match resp.text().await {
                    Ok(text) => {
                        if status.is_success() {
                            match serde_json::from_str::<Value>(&text) {
                                Ok(parsed) => ToolOutcome::Completed {
                                    output: parsed,
                                    verified: Verification::NotApplicable,
                                },
                                Err(e) => ToolOutcome::Failed(AivyxError::Tool {
                                    tool: self.id,
                                    detail: format!(
                                        "ollama.show: invalid JSON from {url}: {e}"
                                    ),
                                }),
                            }
                        } else {
                            ToolOutcome::Failed(AivyxError::Tool {
                                tool: self.id,
                                detail: format!(
                                    "ollama.show: HTTP {status} for model \
                                     \"{name}\": {text}"
                                ),
                            })
                        }
                    }
                    Err(e) => ToolOutcome::Failed(AivyxError::Tool {
                        tool: self.id,
                        detail: format!(
                            "ollama.show: failed to read response body: {e}"
                        ),
                    }),
                }
            }
            Err(e) => ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: format!(
                    "ollama.show: cannot reach {url} — is Ollama running? \
                     Start it with `ollama serve`.\n  (error: {e})"
                ),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// OllamaPullTool
// ---------------------------------------------------------------------------

/// Pulls (downloads) a model from the Ollama registry via
/// `POST /api/pull` with `stream: false`.
#[derive(Debug)]
pub struct OllamaPullTool {
    id: ToolId,
    schema: Value,
    client: Arc<reqwest::Client>,
    base_url: String,
}

impl OllamaPullTool {
    pub fn new(base_url: &str) -> Result<Self, AivyxError> {
        let client = aivyx_llm::transport::client_builder_for(base_url)
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(PULL_TIMEOUT)
            .build()
            .map_err(|e| {
                AivyxError::Config(format!(
                    "ollama.pull reqwest client build failed: {e}"
                ))
            })?;
        Ok(OllamaPullTool {
            id: ToolId::new(),
            schema: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Model name to pull, e.g. \"llama3.1\" or \"codellama:7b\""
                    }
                },
                "required": ["name"],
                "additionalProperties": false
            }),
            client: Arc::new(client),
            base_url: base_url.trim_end_matches('/').to_string(),
        })
    }

}

#[async_trait]
impl Tool for OllamaPullTool {
    fn id(&self) -> ToolId {
        self.id
    }

    fn name(&self) -> &str {
        "ollama.pull"
    }

    fn description(&self) -> &str {
        "Pull (download) a model from the Ollama registry. Input: \
         JSON object with a `name` field (e.g. \"llama3.1\"). This \
         operation may take several minutes for large models. Returns \
         a status object on success or failure."
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn required_scope(&self, _input: &Value) -> Scope {
        Scope::parse("ollama.pull").expect("known base")
    }

    // Registered only when the operator's own [provider] kind = "ollama"
    // -- that configuration choice is the opt-in.
    fn auto_grantable_in_backcompat_floor(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        let name = match input.get("name").and_then(|v| v.as_str()) {
            Some(n) if !n.is_empty() => n,
            _ => {
                return ToolOutcome::Failed(AivyxError::Tool {
                    tool: self.id,
                    detail: "ollama.pull: `name` field is required and must \
                             be a non-empty string"
                        .to_string(),
                });
            }
        };

        let url = format!("{}/api/pull", self.base_url);
        let body = json!({ "name": name, "stream": false });

        match self.client.post(&url).json(&body).send().await {
            Ok(resp) => {
                let status = resp.status();
                match resp.text().await {
                    Ok(text) => {
                        if status.is_success() {
                            match serde_json::from_str::<Value>(&text) {
                                Ok(parsed) => ToolOutcome::Completed {
                                    output: json!({
                                        "status": "success",
                                        "model": name,
                                        "details": parsed
                                    }),
                                    verified: Verification::Unverified,
                                },
                                Err(e) => ToolOutcome::Failed(AivyxError::Tool {
                                    tool: self.id,
                                    detail: format!(
                                        "ollama.pull: invalid JSON from {url}: {e}"
                                    ),
                                }),
                            }
                        } else {
                            ToolOutcome::Failed(AivyxError::Tool {
                                tool: self.id,
                                detail: format!(
                                    "ollama.pull: HTTP {status} pulling model \
                                     \"{name}\": {text}"
                                ),
                            })
                        }
                    }
                    Err(e) => ToolOutcome::Failed(AivyxError::Tool {
                        tool: self.id,
                        detail: format!(
                            "ollama.pull: failed to read response body: {e}"
                        ),
                    }),
                }
            }
            Err(e) => ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: format!(
                    "ollama.pull: cannot reach {url} — is Ollama running? \
                     Start it with `ollama serve`.\n  (error: {e})"
                ),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_capability::TrustTier;
    use aivyx_core::{
        AgentId, CancellationToken, ChannelContext, ChannelError, ChannelPlatform,
        NullAuditHook, SessionId, StreamEvent, ToolContext, TurnOutcome,
    };

    // -- Minimal fake channel for ToolContext construction --

    struct StubChannel {
        session: SessionId,
        token: CancellationToken,
    }

    impl StubChannel {
        fn new() -> Self {
            StubChannel {
                session: SessionId::new(),
                token: CancellationToken::new(),
            }
        }
    }

    #[async_trait]
    impl ChannelContext for StubChannel {
        fn channel_name(&self) -> &str {
            "stub"
        }
        fn platform(&self) -> ChannelPlatform {
            ChannelPlatform::Local
        }
        fn trust_tier(&self) -> TrustTier {
            TrustTier::Trusted
        }
        fn session_id(&self) -> SessionId {
            self.session
        }
        async fn stream_event(&self, _event: StreamEvent<'_>) -> Result<(), ChannelError> {
            Ok(())
        }
        async fn finalize(&self, _outcome: &TurnOutcome) -> Result<(), ChannelError> {
            Ok(())
        }
        fn cancellation_token(&self) -> CancellationToken {
            self.token.clone()
        }
    }

    static AUDIT: NullAuditHook = NullAuditHook;
    static TOKEN: std::sync::LazyLock<CancellationToken> =
        std::sync::LazyLock::new(CancellationToken::new);

    fn test_ctx(channel: &dyn ChannelContext) -> ToolContext<'_> {
        ToolContext {
            agent_id: AgentId::new(),
            session_id: channel.session_id(),
            turn_id: aivyx_core::TurnId::new(),
            channel,
            audit: &AUDIT,
            cancellation: &TOKEN,
            message_origin: aivyx_core::MessageOrigin::Operator,
        }
    }

    /// Starts a minimal mock HTTP server that responds to Ollama API endpoints.
    async fn mock_ollama_server() -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap();
        let addr = listener.local_addr().unwrap();
        let base_url = format!("http://{addr}");

        let handle = tokio::spawn(async move {
            // Accept up to 10 connections for the test suite
            for _ in 0..10 {
                let Ok((mut stream, _)) = listener.accept().await else {
                    break;
                };
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let mut buf = vec![0u8; 4096];
                    let n = stream.read(&mut buf).await.unwrap_or(0);
                    let request = String::from_utf8_lossy(&buf[..n]);

                    let (status, body) = if request.starts_with("GET /api/tags") {
                        ("200 OK", json!({
                            "models": [{
                                "name": "llama3.1:latest",
                                "size": 4_000_000_000_u64,
                                "modified_at": "2026-01-15T10:00:00Z"
                            }]
                        }).to_string())
                    } else if request.starts_with("POST /api/show") {
                        if request.contains("\"nonexistent\"") {
                            ("404 Not Found", json!({
                                "error": "model 'nonexistent' not found"
                            }).to_string())
                        } else {
                            ("200 OK", json!({
                                "modelfile": "FROM llama3.1",
                                "parameters": "temperature 0.7",
                                "template": "{{ .Prompt }}",
                                "license": "Meta Llama 3.1 License"
                            }).to_string())
                        }
                    } else if request.starts_with("POST /api/pull") {
                        ("200 OK", json!({
                            "status": "success"
                        }).to_string())
                    } else {
                        ("404 Not Found", "not found".to_string())
                    };

                    let response = format!(
                        "HTTP/1.1 {status}\r\n\
                         Content-Type: application/json\r\n\
                         Content-Length: {}\r\n\
                         Connection: close\r\n\
                         \r\n\
                         {body}",
                        body.len()
                    );
                    let _ = stream.write_all(response.as_bytes()).await;
                });
            }
        });

        (base_url, handle)
    }

    // ---- OllamaListTool ----

    #[tokio::test]
    async fn list_returns_models() {
        let (base_url, _server) = mock_ollama_server().await;
        let tool = OllamaListTool::new(&base_url).unwrap();
        let ch = StubChannel::new();
        let ctx = test_ctx(&ch);

        let outcome = tool.execute(json!({}), &ctx).await;
        match outcome {
            ToolOutcome::Completed { output, .. } => {
                let models = output.get("models").unwrap().as_array().unwrap();
                assert_eq!(models.len(), 1);
                assert_eq!(models[0]["name"], "llama3.1:latest");
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn list_unreachable_fails() {
        let tool = OllamaListTool::new("http://127.0.0.1:1").unwrap();
        let ch = StubChannel::new();
        let ctx = test_ctx(&ch);

        let outcome = tool.execute(json!({}), &ctx).await;
        match outcome {
            ToolOutcome::Failed(AivyxError::Tool { detail, .. }) => {
                assert!(
                    detail.contains("cannot reach"),
                    "expected connection error, got: {detail}"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    // ---- OllamaShowTool ----

    #[tokio::test]
    async fn show_returns_metadata() {
        let (base_url, _server) = mock_ollama_server().await;
        let tool = OllamaShowTool::new(&base_url).unwrap();
        let ch = StubChannel::new();
        let ctx = test_ctx(&ch);

        let outcome = tool.execute(json!({"name": "llama3.1"}), &ctx).await;
        match outcome {
            ToolOutcome::Completed { output, .. } => {
                assert!(output.get("modelfile").is_some());
                assert!(output.get("parameters").is_some());
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn show_missing_name_fails() {
        let (base_url, _server) = mock_ollama_server().await;
        let tool = OllamaShowTool::new(&base_url).unwrap();
        let ch = StubChannel::new();
        let ctx = test_ctx(&ch);

        let outcome = tool.execute(json!({}), &ctx).await;
        match outcome {
            ToolOutcome::Failed(AivyxError::Tool { detail, .. }) => {
                assert!(detail.contains("name"), "expected name error: {detail}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn show_nonexistent_model_fails() {
        let (base_url, _server) = mock_ollama_server().await;
        let tool = OllamaShowTool::new(&base_url).unwrap();
        let ch = StubChannel::new();
        let ctx = test_ctx(&ch);

        let outcome = tool
            .execute(json!({"name": "nonexistent"}), &ctx)
            .await;
        match outcome {
            ToolOutcome::Failed(AivyxError::Tool { detail, .. }) => {
                assert!(
                    detail.contains("404"),
                    "expected 404 error: {detail}"
                );
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    // ---- OllamaPullTool ----

    #[tokio::test]
    async fn pull_succeeds() {
        let (base_url, _server) = mock_ollama_server().await;
        let tool = OllamaPullTool::new(&base_url).unwrap();
        let ch = StubChannel::new();
        let ctx = test_ctx(&ch);

        let outcome = tool
            .execute(json!({"name": "llama3.1"}), &ctx)
            .await;
        match outcome {
            ToolOutcome::Completed { output, .. } => {
                assert_eq!(output["status"], "success");
                assert_eq!(output["model"], "llama3.1");
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn pull_missing_name_fails() {
        let (base_url, _server) = mock_ollama_server().await;
        let tool = OllamaPullTool::new(&base_url).unwrap();
        let ch = StubChannel::new();
        let ctx = test_ctx(&ch);

        let outcome = tool.execute(json!({}), &ctx).await;
        match outcome {
            ToolOutcome::Failed(AivyxError::Tool { detail, .. }) => {
                assert!(detail.contains("name"), "expected name error: {detail}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    // ---- Scope checks ----

    #[test]
    fn list_scope_is_ollama_list() {
        let tool = OllamaListTool::new("http://localhost:11434").unwrap();
        let scope = tool.required_scope(&json!({}));
        assert_eq!(scope.as_str(), "ollama.list");
    }

    #[test]
    fn show_scope_is_ollama_show() {
        let tool = OllamaShowTool::new("http://localhost:11434").unwrap();
        let scope = tool.required_scope(&json!({"name": "llama3.1"}));
        assert_eq!(scope.as_str(), "ollama.show");
    }

    #[test]
    fn pull_scope_is_ollama_pull() {
        let tool = OllamaPullTool::new("http://localhost:11434").unwrap();
        let scope = tool.required_scope(&json!({"name": "llama3.1"}));
        assert_eq!(scope.as_str(), "ollama.pull");
    }
}
