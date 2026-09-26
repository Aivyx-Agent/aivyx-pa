//! The native `OllamaProvider`: concrete `LlmProvider` against
//! Ollama's `/api/chat` streaming endpoint.
//!
//! Phase 121 Task 2 — ships the skeleton: provider struct,
//! config types, request-body builder, health check. The JSONL
//! streaming line reader (Task 3), stream state machine (Task
//! 4), and `LlmProvider::chat_stream` impl (Task 5) land in
//! follow-on tasks.
//!
//! ## Wire format (target)
//!
//! Ollama `/api/chat` accepts a JSON request body:
//!
//! ```json
//! {
//!   "model": "qwen3.6:27b",
//!   "messages": [{ "role": "user", "content": "..." }],
//!   "tools": [{ "type": "function", "function": { ... } }],
//!   "stream": true,
//!   "options": { "num_ctx": 8192, "num_predict": 1024, ... }
//! }
//! ```
//!
//! The response streams as newline-delimited JSON (one object
//! per line):
//!
//! ```json
//! { "message": { "role": "assistant", "content": "..." }, "done": false }
//! { "message": { "role": "assistant", "content": "..." }, "done": false }
//! { "message": { "role": "assistant", "tool_calls": [...] }, "done": true,
//!   "prompt_eval_count": 42, "eval_count": 31 }
//! ```
//!
//! Tool calls typically arrive in the final `done: true` chunk
//! as a complete `tool_calls` array (not deltas — simpler than
//! OpenAI's incremental reassembly).
//!
//! Phase 121 Task 4 parses this stream shape.

use std::collections::HashMap;
use std::sync::Mutex;

use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{ContentBlock, LlmError, LlmMessage, LlmRequest};

use crate::transport::{HttpTransport, ReqwestTransport};

/// Default Ollama base URL — standard port for `ollama serve`.
/// Mirrors `crate::openai::DEFAULT_OLLAMA_BASE_URL` (the OpenAI-
/// compat path that pre-dates this native adapter); the constant
/// is duplicated rather than re-exported so the two paths stay
/// independently auditable.
pub const DEFAULT_OLLAMA_BASE_URL: &str = "http://localhost:11434";

/// Chapter P — cap on auto-detected `num_ctx`. The agent's prompt (system +
/// tool defs + few-shot examples) runs ~4–11k tokens, which starves Ollama's
/// default `num_ctx` of 4096 down to a single generated token. We auto-set
/// `num_ctx = min(native_context, this cap)` when the operator hasn't: 16k
/// clears the prompt with comfortable generation headroom without allocating
/// the model's full (often 128k+) native window, which would waste VRAM.
pub const AUTO_NUM_CTX_CAP: u32 = 16_384;

/// Chapter P — the recommended local model for a fresh first-run. A
/// **tool-capable** qwen3 model in the ~8B tier: small enough to download on a
/// normal laptop (~5 GB), big enough to drive the agent's tool-calling, and in
/// the family verified live against the thinking + non-terminal-tool-call
/// fixes and auto-`num_ctx` (qwen3 at 9B and 27B). The wizard defaults to it
/// and offers to pull it; `aivyx-pa doctor` checks for a usable model against it.
pub const RECOMMENDED_LOCAL_MODEL: &str = "qwen3:8b";

/// #17c — how many times to resample after Ollama rejects the model's
/// malformed tool-call JSON with an HTTP 500. One retry recovers the common
/// transient case without turning a persistent bad-output loop into a spend
/// sink.
const MAX_TOOL_PARSE_RETRIES: usize = 1;

/// Whether an Ollama error body is its "the model emitted unparseable
/// tool-call JSON" failure (`error parsing tool call: raw='…'`), as opposed
/// to a genuine server fault we must surface. Matching Ollama's message text
/// is the only signal available (it returns a plain 500). Pure + tested.
fn is_ollama_tool_parse_error(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    m.contains("parsing tool call") || m.contains("parse tool call")
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Ollama-specific generation options that ride in the
/// `options: {...}` block of the `/api/chat` request body.
/// Operators set these via `[ollama]` in `aivyx-pa.toml` (Phase 121
/// Task 6); `None` values are omitted from the wire form so
/// Ollama's own defaults apply.
///
/// The set covers the operator-relevant subset of Ollama's
/// modelfile options. Future Phase 121 follow-ups can add more
/// fields additively without breaking the wire shape (Ollama
/// ignores unknown options).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OllamaOptions {
    /// Context window size in tokens. Override Ollama's
    /// per-model default; useful for models with large native
    /// contexts running on operators with sufficient VRAM.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub num_ctx: Option<u32>,
    /// Maximum tokens to generate. Override `max_tokens` from
    /// the request when set; otherwise the request value applies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub num_predict: Option<u32>,
    /// Number of threads the runtime may use. Operator-tunable
    /// for shared-host setups.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub num_thread: Option<u32>,
    /// Mirostat sampling mode (0 = disabled, 1 = Mirostat,
    /// 2 = Mirostat 2.0). Operator-conservative default: `None`
    /// → Ollama's per-model default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mirostat: Option<u8>,
    /// Top-k sampling. `None` → Ollama default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_k: Option<u32>,
    /// Top-p (nucleus) sampling. `None` → Ollama default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    /// Repeat penalty. `None` → Ollama default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat_penalty: Option<f32>,
    /// Number of previous tokens to consider for `repeat_penalty`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat_last_n: Option<i32>,
    /// Random seed for reproducibility.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,
}

impl OllamaOptions {
    /// `true` when every field is `None`. Used by the request-
    /// body builder to decide whether to emit the `options`
    /// block at all (cleaner wire form when the operator
    /// hasn't overridden anything).
    pub fn is_empty(&self) -> bool {
        self.num_ctx.is_none()
            && self.num_predict.is_none()
            && self.num_thread.is_none()
            && self.mirostat.is_none()
            && self.top_k.is_none()
            && self.top_p.is_none()
            && self.repeat_penalty.is_none()
            && self.repeat_last_n.is_none()
            && self.seed.is_none()
    }
}

/// Provider config. The OpenAI provider needed an `api_key`
/// because cloud OpenAI authenticates via Bearer; Ollama does
/// not require auth, so the field is `Option<SecretString>` for
/// non-default deployments (e.g. operator-protected Ollama
/// behind a proxy). The default `None` matches the
/// `ollama serve` posture.
pub struct OllamaConfig {
    /// Base URL. `None` → [`DEFAULT_OLLAMA_BASE_URL`].
    pub base_url: Option<String>,
    /// Optional API key for protected Ollama deployments. Most
    /// operators leave this `None`.
    pub api_key: Option<SecretString>,
    /// Operator-configured generation options. Defaults all-
    /// `None`; the request-body builder omits the `options`
    /// block when this is empty.
    pub options: OllamaOptions,
}

impl OllamaConfig {
    /// Build a default config: localhost, no auth, no option
    /// overrides. Matches a vanilla `ollama serve` setup.
    pub fn default_local() -> Self {
        OllamaConfig {
            base_url: None,
            api_key: None,
            options: OllamaOptions::default(),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = Some(base_url.into());
        self
    }

    pub fn with_api_key(mut self, key: impl Into<SecretString>) -> Self {
        self.api_key = Some(key.into());
        self
    }

    pub fn with_options(mut self, options: OllamaOptions) -> Self {
        self.options = options;
        self
    }
}

impl Default for OllamaConfig {
    fn default() -> Self {
        Self::default_local()
    }
}

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

/// Native Ollama `LlmProvider`. Phase 121 Task 2 ships the
/// skeleton; the `LlmProvider::chat_stream` impl lands at
/// Task 5 after Tasks 3-4 deliver the JSONL streaming
/// substrate.
pub struct OllamaProvider {
    pub(crate) config: OllamaConfig,
    pub(crate) transport: Box<dyn HttpTransport>,
    /// Phase 127 Task 6 — per-model family-hint cache.
    /// `Option<String>` slot: `Some("qwen35")` when
    /// `/api/show` returned a family; `None` when the
    /// query failed or returned no family field. Caching
    /// the `None` outcome avoids re-querying on every
    /// turn when the model isn't introspectable.
    family_cache: Mutex<HashMap<String, Option<String>>>,
    /// Per-model `thinking`-capability cache. For a "thinking" model
    /// (qwen3, …) we send `think: true` so its reasoning is routed into
    /// the separate `thinking` field — which the agent discards — leaving
    /// the answer (or tool_calls) in `content`. (`think: false` is NOT a
    /// reliable suppressor on current Ollama: some hybrid models ignore
    /// it and emit reasoning into `content`.) `bool` value: `true` =
    /// supports thinking; caching both outcomes avoids re-querying
    /// `/api/show`.
    thinking_cache: Mutex<HashMap<String, bool>>,
    /// Chapter P — per-model auto-`num_ctx` cache. The agent's prompt
    /// (system + tools + few-shot) runs ~4–11k tokens, which starves the
    /// Ollama default `num_ctx` of 4096 down to a single generated token.
    /// When the operator hasn't set `num_ctx`, we read the model's native
    /// context length from `/api/show` and default to `min(native, cap)`.
    /// `Some(n)` = use `n`; `None` = `/api/show` gave nothing, fall back to
    /// Ollama's own default. Cached to avoid re-querying.
    num_ctx_cache: Mutex<HashMap<String, Option<u32>>>,
}

impl OllamaProvider {
    pub fn new(config: OllamaConfig) -> Result<Self, LlmError> {
        Ok(OllamaProvider {
            config,
            transport: Box::new(ReqwestTransport::new()?),
            family_cache: Mutex::new(HashMap::new()),
            thinking_cache: Mutex::new(HashMap::new()),
            num_ctx_cache: Mutex::new(HashMap::new()),
        })
    }

    pub fn with_transport(
        config: OllamaConfig,
        transport: Box<dyn HttpTransport>,
    ) -> Self {
        OllamaProvider {
            config,
            transport,
            family_cache: Mutex::new(HashMap::new()),
            thinking_cache: Mutex::new(HashMap::new()),
            num_ctx_cache: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn base_url(&self) -> &str {
        self.config
            .base_url
            .as_deref()
            .unwrap_or(DEFAULT_OLLAMA_BASE_URL)
    }

    /// Endpoint for the chat-streaming POST. Different from the
    /// OpenAI-compat path's `/v1/chat/completions`: Ollama's
    /// native endpoint is `/api/chat`.
    pub(crate) fn endpoint(&self) -> String {
        format!("{}/api/chat", self.base_url())
    }

    /// Phase 127 Task 6 — query Ollama `/api/show` for
    /// `details.family`. Returns the family string on
    /// success, `None` on any failure (network, model not
    /// pulled, parse error, missing family field).
    /// Failure is silent because the family-hint is a
    /// best-effort optimization — the substrate falls
    /// back to permissive scan when the hint is absent.
    async fn query_model_family(&self, model: &str) -> Option<String> {
        let url = format!("{}/api/show", self.base_url());
        let body = serde_json::to_vec(&json!({ "name": model })).ok()?;
        let cancellation = tokio_util::sync::CancellationToken::new();
        let bytes = self
            .transport
            .post_json(&url, &[("content-type", "application/json")], body, &cancellation)
            .await
            .ok()?;
        let value: Value = serde_json::from_slice(&bytes).ok()?;
        let family = value
            .get("details")?
            .get("family")?
            .as_str()?
            .trim()
            .to_string();
        if family.is_empty() {
            return None;
        }
        Some(family)
    }

    /// Query Ollama `/api/show` for the model's top-level
    /// `capabilities` array (e.g. `["completion","tools","thinking"]`)
    /// and report whether it advertises `"thinking"`. Best-effort:
    /// any failure (network, model not pulled, parse error, missing
    /// field) reports `false` so we fall back to the model's default
    /// behavior rather than sending a `think` flag it may reject.
    async fn query_model_supports_thinking(&self, model: &str) -> bool {
        let url = format!("{}/api/show", self.base_url());
        let Ok(body) = serde_json::to_vec(&json!({ "name": model })) else {
            return false;
        };
        let cancellation = tokio_util::sync::CancellationToken::new();
        let Ok(bytes) = self
            .transport
            .post_json(&url, &[("content-type", "application/json")], body, &cancellation)
            .await
        else {
            return false;
        };
        let Ok(value) = serde_json::from_slice::<Value>(&bytes) else {
            return false;
        };
        value
            .get("capabilities")
            .and_then(|c| c.as_array())
            .map(|caps| caps.iter().any(|c| c.as_str() == Some("thinking")))
            .unwrap_or(false)
    }

    /// Cached lookup of whether `model` is a thinking model. When it is,
    /// we send `think: true` so the reasoning is routed into the separate
    /// `thinking` field (discarded) and `content` stays clean. Queries
    /// `/api/show` once per model, then serves from cache.
    async fn is_thinking_model(&self, model: &str) -> bool {
        {
            if let Ok(cache) = self.thinking_cache.lock() {
                if let Some(cached) = cache.get(model) {
                    return *cached;
                }
            }
        }
        let supports = self.query_model_supports_thinking(model).await;
        if let Ok(mut cache) = self.thinking_cache.lock() {
            cache.insert(model.to_string(), supports);
        }
        supports
    }

    /// Chapter P — query `/api/show` for the model's native context length
    /// (`model_info["<arch>.context_length"]`, e.g. `qwen35.context_length`).
    /// Best-effort: any failure returns `None`, and the caller falls back to
    /// Ollama's own `num_ctx` default.
    async fn query_model_context_length(&self, model: &str) -> Option<u32> {
        let url = format!("{}/api/show", self.base_url());
        let body = serde_json::to_vec(&json!({ "name": model })).ok()?;
        let cancellation = tokio_util::sync::CancellationToken::new();
        let bytes = self
            .transport
            .post_json(&url, &[("content-type", "application/json")], body, &cancellation)
            .await
            .ok()?;
        let value: Value = serde_json::from_slice(&bytes).ok()?;
        let model_info = value.get("model_info")?.as_object()?;
        // The key is architecture-prefixed (`llama.context_length`,
        // `qwen35.context_length`, …); match by suffix.
        model_info
            .iter()
            .find(|(k, _)| k.ends_with(".context_length"))
            .and_then(|(_, v)| v.as_u64())
            .and_then(|n| u32::try_from(n).ok())
    }

    /// Cached auto-`num_ctx`: the model's native context length capped at
    /// [`AUTO_NUM_CTX_CAP`]. `None` when `/api/show` yields nothing (→ Ollama's
    /// own default applies). Queried once per model, then served from cache.
    async fn auto_num_ctx_for(&self, model: &str) -> Option<u32> {
        {
            if let Ok(cache) = self.num_ctx_cache.lock() {
                if let Some(cached) = cache.get(model) {
                    return *cached;
                }
            }
        }
        let resolved = self
            .query_model_context_length(model)
            .await
            .map(|native| native.min(AUTO_NUM_CTX_CAP));
        if let Ok(mut cache) = self.num_ctx_cache.lock() {
            cache.insert(model.to_string(), resolved);
        }
        resolved
    }

    /// Lightweight health check against the Ollama base URL.
    /// Same shape as `OpenAiProvider::health_check`; Ollama
    /// answers GET / with the plain-text body
    /// `"Ollama is running"`.
    pub async fn health_check(&self) -> Result<(), String> {
        let url = self.base_url();
        match self.transport.get_text(url).await {
            Ok(_body) => Ok(()),
            Err(LlmError::Transport(e)) => Err(format!(
                "cannot reach {url} — is Ollama running? \
                 Start it with `ollama serve`.\n  \
                 (transport error: {e})"
            )),
            Err(LlmError::Api { status, message }) => {
                Err(format!("{url} returned HTTP {status}: {message}"))
            }
            Err(other) => {
                Err(format!("health check against {url} failed: {other}"))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// LlmProvider impl (Phase 121 Task 5)
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
impl crate::LlmProvider for OllamaProvider {
    async fn chat_stream(
        &self,
        request: crate::LlmRequest<'_>,
        cancellation: &tokio_util::sync::CancellationToken,
    ) -> Result<Box<dyn crate::LlmStream>, LlmError> {
        // Thinking models (qwen3, …) otherwise dump their answer into
        // a `thinking` field — which the agent discards — and leave
        // `content` empty when tools are present. Detect the capability
        // (cached `/api/show`) and turn thinking off so the answer
        // lands in `content`.
        let thinking_capable = self.is_thinking_model(request.model).await;
        // Chapter P — auto-`num_ctx`: only when the operator hasn't set one,
        // size the context window to the model's native length (capped) so the
        // agent prompt doesn't starve generation. An explicit `num_ctx` wins.
        let auto_num_ctx = if self.config.options.num_ctx.is_none() {
            self.auto_num_ctx_for(request.model).await
        } else {
            None
        };
        let body = build_request_body(
            &request,
            &self.config.options,
            thinking_capable,
            auto_num_ctx,
        )?;
        let body_bytes = serde_json::to_vec(&body).map_err(|e| {
            LlmError::Parse(format!("request serialization: {e}"))
        })?;

        let mut headers: Vec<(&str, &str)> =
            vec![("content-type", "application/json")];
        // Ollama doesn't require auth by default; the API key is
        // for operator-protected deployments behind a proxy.
        // Following the OpenAI provider's posture: only emit
        // Authorization when an api_key is configured so we
        // don't send `Bearer ` with an empty secret to vanilla
        // Ollama.
        let auth_header;
        if let Some(ref key) = self.config.api_key {
            use secrecy::ExposeSecret;
            auth_header = format!("Bearer {}", key.expose_secret());
            headers.push(("authorization", auth_header.as_str()));
        }

        let endpoint = self.endpoint();
        let mut attempt = 0usize;
        // #17c — Ollama returns HTTP 500 "error parsing tool call" when the
        // model emits malformed tool-call JSON (a transient small-model output
        // glitch, not a server fault). It hard-fails the whole turn; but a fresh
        // sample almost always parses, so retry once before giving up. A plain
        // re-POST resamples (generation has no server-side side effects, and the
        // cancellation token is honored each attempt).
        let byte_stream = loop {
            match self
                .transport
                .post_sse(&endpoint, &headers, body_bytes.clone(), cancellation)
                .await
            {
                Ok(s) => break s,
                Err(LlmError::Api { status: 500, message })
                    if attempt < MAX_TOOL_PARSE_RETRIES
                        && is_ollama_tool_parse_error(&message) =>
                {
                    attempt += 1;
                    eprintln!(
                        "aivyx-pa ollama: model emitted a malformed tool call; \
                         resampling (retry {attempt}/{MAX_TOOL_PARSE_RETRIES})"
                    );
                    continue;
                }
                Err(e) => return Err(e),
            }
        };

        // Phase 120 Task 3 — snapshot the canonical tool-name
        // set so the stream's terminal-build step can flag any
        // emitted tool_name that doesn't match. Same posture as
        // the OpenAI and Anthropic providers.
        let known_tool_names: std::collections::HashSet<String> = request
            .tools
            .iter()
            .map(|t| t.name.to_string())
            .collect();

        let reader = super::jsonl::JsonlReader::new(byte_stream);
        Ok(Box::new(super::stream::OllamaStream::new(
            reader,
            known_tool_names,
        )))
    }

    async fn tool_call_family_hint(&self, model: &str) -> Option<String> {
        // Check cache first; release the guard before any
        // await. `Option<String>` in the value slot: `Some`
        // means "we tried and got a family"; `None` cached
        // means "we tried and got nothing — don't re-query."
        {
            let cache = self.family_cache.lock().ok()?;
            if let Some(cached) = cache.get(model) {
                return cached.clone();
            }
        }
        let family = self.query_model_family(model).await;
        if let Ok(mut cache) = self.family_cache.lock() {
            cache.insert(model.to_string(), family.clone());
        }
        family
    }
}

// ---------------------------------------------------------------------------
// Request-body construction (Phase 121 Task 2)
// ---------------------------------------------------------------------------

/// Build the JSON body for Ollama's `/api/chat`. Pure function
/// over the request + operator options. Returns
/// `LlmError::UnknownModel("")` for empty model strings (same
/// guard as the OpenAI provider).
///
/// Wire shape:
/// ```json
/// {
///   "model": "<model>",
///   "messages": [...],
///   "tools": [...],         // present iff request.tools non-empty
///   "stream": true,
///   "options": {...}        // present iff operator overrode anything
/// }
/// ```
///
/// `system` (the per-turn system prompt) lands as a leading
/// `{"role": "system", "content": "..."}` message — same shape
/// the OpenAI provider uses, and what Ollama's `/api/chat`
/// expects.
///
/// `temperature` lands inside the `options` block as
/// `options.temperature` rather than at top-level (Ollama's
/// convention).
pub fn build_request_body(
    request: &LlmRequest<'_>,
    options: &OllamaOptions,
    thinking_capable: bool,
    auto_num_ctx: Option<u32>,
) -> Result<Value, LlmError> {
    if request.model.is_empty() {
        return Err(LlmError::UnknownModel(String::new()));
    }

    let mut messages: Vec<Value> = Vec::new();
    if let Some(system) = request.system {
        messages.push(json!({"role": "system", "content": system}));
    }
    for msg in request.messages {
        messages.push(ollama_message(msg)?);
    }

    let mut body = json!({
        "model": request.model,
        "messages": messages,
        "stream": true,
    });

    // Route a thinking model's reasoning into the separate `thinking`
    // field (which the agent discards) so it stays out of `content`.
    // Counter-intuitively this means `think: true`, NOT false: on
    // current Ollama, `think: false` does not reliably suppress a
    // hybrid-reasoning model — qwen3:30b-a3b ignores it and dumps its
    // chain-of-thought straight into `content` (verbose, off-persona),
    // while `think: true` cleanly splits reasoning → `thinking` and the
    // answer (or tool_calls) → `content`. Verified live on Ollama 0.30
    // for qwen3:8b and qwen3:30b-a3b, with and without tools. Only
    // emitted for models that advertise the `thinking` capability, so
    // non-thinking models never see a `think` flag.
    if thinking_capable {
        body["think"] = json!(true);
    }

    if !request.tools.is_empty() {
        let tools: Vec<Value> = request
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.input_schema,
                    }
                })
            })
            .collect();
        body["tools"] = Value::Array(tools);
    }

    // Phase 121 — operator-configured options + request-side
    // temperature merge into the same `options` block per Ollama's
    // convention. We compute the merged value into a Value rather
    // than mutating `options` (which is shared via &) so the call
    // site stays pure.
    let mut options_value =
        serde_json::to_value(options).map_err(|e| {
            LlmError::Parse(format!("ollama options serialize: {e}"))
        })?;
    if let Some(temp) = request.temperature {
        if let Some(obj) = options_value.as_object_mut() {
            obj.insert("temperature".into(), json!(temp));
        }
    }
    // Chapter P — inject the auto-detected `num_ctx` only when the operator
    // didn't configure one (the serialized `options` omits `num_ctx` then).
    if options.num_ctx.is_none() {
        if let Some(n) = auto_num_ctx {
            if let Some(obj) = options_value.as_object_mut() {
                obj.insert("num_ctx".into(), json!(n));
            }
        }
    }
    let emit_options = match &options_value {
        Value::Object(obj) => !obj.is_empty(),
        _ => false,
    };
    if emit_options {
        body["options"] = options_value;
    }

    Ok(body)
}

/// Translate one `LlmMessage` into Ollama's wire format. Same
/// shape as the OpenAI provider for User/Assistant/ToolResult,
/// with two minor protocol differences:
/// - Ollama's tool-result role is `"tool"` (matches OpenAI;
///   distinct from Anthropic's `"user"` + content block).
/// - Ollama's `tool_calls` array on the Assistant message stores
///   `function.arguments` as a JSON OBJECT (not a JSON string —
///   Ollama parses the function-args natively, unlike OpenAI's
///   string-JSON-in-JSON convention).
fn ollama_message(msg: &LlmMessage) -> Result<Value, LlmError> {
    Ok(match msg {
        LlmMessage::User { content } => {
            let has_images = content.iter().any(ContentBlock::is_image);
            let dropped_documents =
                content.iter().filter(|b| b.is_document()).count();
            if dropped_documents > 0 {
                eprintln!(
                    "ollama provider: dropping {dropped_documents} document \
                     block(s); Ollama's chat API has no document content \
                     block — Phase 163/A13 skip-and-warn"
                );
            }
            if has_images {
                // Ollama's vision-model path uses an `images`
                // array of base64 strings alongside text content
                // (different from OpenAI's content-block array).
                // Phase 121 ships the operator-common text+image
                // shape; richer multimodal scaffolds can land
                // additively as Ollama's vision support
                // stabilizes.
                let text = content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");
                let images: Vec<Value> = content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::ImageBase64 { data, .. } => {
                            Some(json!(data))
                        }
                        _ => None,
                    })
                    .collect();
                let mut msg = json!({
                    "role": "user",
                    "content": text,
                });
                if !images.is_empty() {
                    msg["images"] = Value::Array(images);
                }
                msg
            } else {
                let text = content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");
                json!({ "role": "user", "content": text })
            }
        }
        LlmMessage::Assistant { text, tool_calls } => {
            let mut msg = json!({"role": "assistant"});
            // Ollama's chat protocol expects `content` even when
            // it's empty (some Ollama models reject Assistant
            // messages with no content field).
            msg["content"] = Value::String(text.clone());
            if !tool_calls.is_empty() {
                let calls: Vec<Value> = tool_calls
                    .iter()
                    .map(|c| {
                        json!({
                            "id": c.call_id,
                            "function": {
                                "name": c.tool_name,
                                // Ollama's `arguments` is a JSON
                                // object, NOT a JSON-encoded
                                // string. Different from OpenAI.
                                "arguments": c.input.clone(),
                            }
                        })
                    })
                    .collect();
                msg["tool_calls"] = Value::Array(calls);
            }
            msg
        }
        LlmMessage::ToolResult {
            call_id,
            content,
            ..
        } => json!({
            "role": "tool",
            "tool_call_id": call_id,
            "content": content,
        }),
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LlmMessage, LlmRequest, LlmToolDescriptor};

    fn simple_request<'a>(
        msgs: &'a [LlmMessage],
        tools: &'a [LlmToolDescriptor],
    ) -> LlmRequest<'a> {
        LlmRequest {
            model: "qwen3.6:27b",
            system: None,
            messages: msgs,
            tools,
            max_tokens: 1024,
            temperature: None,
            id_slot: None,
            slot_hint: None,
            route: None,
        }
    }

    // ----- Config -----

    #[test]
    fn config_default_local_uses_default_base_url() {
        let cfg = OllamaConfig::default_local();
        assert!(cfg.base_url.is_none());
        assert!(cfg.api_key.is_none());
        assert!(cfg.options.is_empty());
    }

    #[test]
    fn options_is_empty_when_all_none() {
        let options = OllamaOptions::default();
        assert!(options.is_empty());
    }

    #[test]
    fn options_is_not_empty_when_any_field_set() {
        let options = OllamaOptions {
            num_ctx: Some(8192),
            ..OllamaOptions::default()
        };
        assert!(!options.is_empty());
    }

    #[test]
    fn options_round_trips_through_serde() {
        let original = OllamaOptions {
            num_ctx: Some(8192),
            num_predict: Some(1024),
            mirostat: Some(2),
            seed: Some(42),
            ..OllamaOptions::default()
        };
        let json = serde_json::to_value(&original).unwrap();
        let parsed: OllamaOptions = serde_json::from_value(json).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn options_skip_serialize_when_none() {
        let options = OllamaOptions {
            num_ctx: Some(8192),
            ..OllamaOptions::default()
        };
        let json = serde_json::to_value(&options).unwrap();
        let obj = json.as_object().unwrap();
        assert!(obj.contains_key("num_ctx"));
        // Every other field is None → omitted from wire form.
        assert_eq!(obj.len(), 1);
    }

    // ----- Endpoint -----

    #[test]
    fn endpoint_uses_default_base_when_unset() {
        let provider = OllamaProvider::new(OllamaConfig::default_local())
            .expect("build provider");
        assert_eq!(provider.endpoint(), "http://localhost:11434/api/chat");
    }

    #[test]
    fn endpoint_honors_operator_base_url_override() {
        let provider = OllamaProvider::new(
            OllamaConfig::default_local()
                .with_base_url("http://gpu-host.local:11434"),
        )
        .expect("build provider");
        assert_eq!(
            provider.endpoint(),
            "http://gpu-host.local:11434/api/chat"
        );
    }

    // ----- Request body -----

    #[test]
    fn request_body_minimal_no_tools_no_options() {
        let msgs = vec![LlmMessage::user_text("hello")];
        let req = simple_request(&msgs, &[]);
        let options = OllamaOptions::default();
        let body = build_request_body(&req, &options, false, None).unwrap();
        assert_eq!(body["model"], "qwen3.6:27b");
        assert_eq!(body["stream"], true);
        // No options block when both operator and request
        // contribute nothing.
        assert!(body.get("options").is_none());
        // No tools block when request advertises nothing.
        assert!(body.get("tools").is_none());
        // Messages array contains the one user message.
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[0]["content"], "hello");
    }

    #[test]
    fn request_body_omits_think_by_default() {
        // Non-thinking models must never see a `think` flag (some
        // Ollama models reject it).
        let msgs = vec![LlmMessage::user_text("hello")];
        let req = simple_request(&msgs, &[]);
        let body =
            build_request_body(&req, &OllamaOptions::default(), false, None).unwrap();
        assert!(body.get("think").is_none());
    }

    #[test]
    fn request_body_enables_think_for_thinking_models() {
        // A thinking model (detected via /api/show capabilities) gets
        // `think: true` so its reasoning is routed into the separate
        // `thinking` field (discarded) and the answer / tool_calls land
        // in `content`. `think: false` is not a reliable suppressor on
        // current Ollama — qwen3:30b-a3b ignores it and leaks reasoning
        // into `content`.
        let msgs = vec![LlmMessage::user_text("hello")];
        let req = simple_request(&msgs, &[]);
        let body =
            build_request_body(&req, &OllamaOptions::default(), true, None).unwrap();
        assert_eq!(body["think"], true);
    }

    #[test]
    fn request_body_injects_auto_num_ctx_when_unset() {
        // Chapter P — with no operator `num_ctx`, the auto-detected value
        // lands in the options block so generation isn't starved.
        let msgs = vec![LlmMessage::user_text("hello")];
        let req = simple_request(&msgs, &[]);
        let body =
            build_request_body(&req, &OllamaOptions::default(), false, Some(16_384)).unwrap();
        assert_eq!(body["options"]["num_ctx"], 16_384);
    }

    #[test]
    fn request_body_explicit_num_ctx_wins_over_auto() {
        // An operator-set `num_ctx` is never overridden by auto-detection.
        let msgs = vec![LlmMessage::user_text("hello")];
        let req = simple_request(&msgs, &[]);
        let options = OllamaOptions { num_ctx: Some(8_192), ..Default::default() };
        let body = build_request_body(&req, &options, false, Some(16_384)).unwrap();
        assert_eq!(body["options"]["num_ctx"], 8_192);
    }

    #[test]
    fn request_body_no_num_ctx_when_auto_unavailable() {
        // `/api/show` gave nothing (auto = None) and no operator value →
        // omit `num_ctx` entirely so Ollama's own default applies.
        let msgs = vec![LlmMessage::user_text("hello")];
        let req = simple_request(&msgs, &[]);
        let body =
            build_request_body(&req, &OllamaOptions::default(), false, None).unwrap();
        assert!(body.get("options").is_none() || body["options"].get("num_ctx").is_none());
    }

    #[test]
    fn request_body_includes_system_as_leading_message() {
        let msgs = vec![LlmMessage::user_text("hello")];
        let req = LlmRequest {
            model: "qwen3.6:27b",
            system: Some("you are helpful"),
            messages: &msgs,
            tools: &[],
            max_tokens: 1024,
            temperature: None,
        id_slot: None,
        slot_hint: None,
        route: None,
        };
        let body = build_request_body(&req, &OllamaOptions::default(), false, None).unwrap();
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "you are helpful");
        assert_eq!(messages[1]["role"], "user");
    }

    #[test]
    fn request_body_includes_tools_as_function_descriptors() {
        let msgs = vec![LlmMessage::user_text("read a file")];
        let tools = vec![LlmToolDescriptor {
            name: "fs.read".into(),
            description: "Read a file from sandbox.".into(),
            input_schema: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
        }];
        let req = simple_request(&msgs, &tools);
        let body = build_request_body(&req, &OllamaOptions::default(), false, None).unwrap();
        let tools_array = body["tools"].as_array().unwrap();
        assert_eq!(tools_array.len(), 1);
        assert_eq!(tools_array[0]["type"], "function");
        assert_eq!(tools_array[0]["function"]["name"], "fs.read");
        assert_eq!(
            tools_array[0]["function"]["description"],
            "Read a file from sandbox."
        );
    }

    #[test]
    fn request_body_includes_options_block_when_operator_set_anything() {
        let msgs = vec![LlmMessage::user_text("hi")];
        let req = simple_request(&msgs, &[]);
        let options = OllamaOptions {
            num_ctx: Some(16384),
            mirostat: Some(2),
            ..OllamaOptions::default()
        };
        let body = build_request_body(&req, &options, false, None).unwrap();
        let opts = body["options"].as_object().unwrap();
        assert_eq!(opts["num_ctx"], 16384);
        assert_eq!(opts["mirostat"], 2);
        // Unset options stay out of the wire form.
        assert!(!opts.contains_key("top_p"));
        assert!(!opts.contains_key("seed"));
    }

    #[test]
    fn request_body_request_temperature_lands_inside_options_block() {
        // Ollama's convention: temperature is a sampling option,
        // not a top-level field. The request's temperature merges
        // into the options block.
        let msgs = vec![LlmMessage::user_text("hi")];
        let req = LlmRequest {
            model: "qwen3.6:27b",
            system: None,
            messages: &msgs,
            tools: &[],
            max_tokens: 1024,
            temperature: Some(0.7),
            id_slot: None,
            slot_hint: None,
            route: None,
        };
        let body = build_request_body(&req, &OllamaOptions::default(), false, None).unwrap();
        let opts = body["options"].as_object().unwrap();
        // The request's temperature carried through.
        assert!((opts["temperature"].as_f64().unwrap() - 0.7).abs() < 1e-6);
        // Temperature alone causes options block to appear.
        assert!(body["options"].is_object());
    }

    #[test]
    fn request_body_temperature_merges_with_operator_options() {
        let msgs = vec![LlmMessage::user_text("hi")];
        let req = LlmRequest {
            model: "qwen3.6:27b",
            system: None,
            messages: &msgs,
            tools: &[],
            max_tokens: 1024,
            temperature: Some(0.2),
            id_slot: None,
            slot_hint: None,
            route: None,
        };
        let options = OllamaOptions {
            num_ctx: Some(8192),
            ..OllamaOptions::default()
        };
        let body = build_request_body(&req, &options, false, None).unwrap();
        let opts = body["options"].as_object().unwrap();
        // Both fields carried through.
        assert_eq!(opts["num_ctx"], 8192);
        assert!((opts["temperature"].as_f64().unwrap() - 0.2).abs() < 1e-6);
    }

    #[test]
    fn request_body_empty_model_errors() {
        let msgs = vec![LlmMessage::user_text("hi")];
        let req = LlmRequest {
            model: "",
            system: None,
            messages: &msgs,
            tools: &[],
            max_tokens: 1024,
            temperature: None,
        id_slot: None,
        slot_hint: None,
        route: None,
        };
        let err =
            build_request_body(&req, &OllamaOptions::default(), false, None).unwrap_err();
        assert!(matches!(err, LlmError::UnknownModel(_)));
    }

    // ----- Message translation -----

    #[test]
    fn assistant_message_with_tool_calls_uses_object_args_not_string() {
        // Ollama's wire shape stores tool-call arguments as a
        // JSON object, NOT a JSON-encoded string (different from
        // OpenAI). The agent's history must round-trip with the
        // right shape.
        let msgs = vec![LlmMessage::Assistant {
            text: "calling fs.read".into(),
            tool_calls: vec![crate::LlmToolCallRecord {
                call_id: "c1".into(),
                tool_name: "fs.read".into(),
                input: json!({"path": "foo.txt"}),
            }],
        }];
        let req = simple_request(&msgs, &[]);
        let body = build_request_body(&req, &OllamaOptions::default(), false, None).unwrap();
        let messages = body["messages"].as_array().unwrap();
        let asst = &messages[0];
        assert_eq!(asst["role"], "assistant");
        assert_eq!(asst["content"], "calling fs.read");
        let calls = asst["tool_calls"].as_array().unwrap();
        // The arguments field is an OBJECT, not a string.
        assert!(calls[0]["function"]["arguments"].is_object());
        assert_eq!(
            calls[0]["function"]["arguments"]["path"],
            "foo.txt"
        );
    }

    #[test]
    fn tool_result_message_uses_tool_role_with_call_id() {
        let msgs = vec![LlmMessage::ToolResult {
            call_id: "c1".into(),
            content: "{\"result\":\"ok\"}".into(),
            is_error: false,
        }];
        let req = simple_request(&msgs, &[]);
        let body = build_request_body(&req, &OllamaOptions::default(), false, None).unwrap();
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages[0]["role"], "tool");
        assert_eq!(messages[0]["tool_call_id"], "c1");
        assert_eq!(messages[0]["content"], "{\"result\":\"ok\"}");
    }

    #[test]
    fn user_message_with_image_uses_ollama_images_array() {
        // Ollama's vision protocol carries base64 images as a
        // separate `images: []` field, not in the content array
        // (different from OpenAI's content-block format).
        let msgs = vec![LlmMessage::User {
            content: vec![
                ContentBlock::text("describe this"),
                ContentBlock::ImageBase64 {
                    media_type: "image/png".into(),
                    data: "AAA".into(),
                },
            ],
        }];
        let req = simple_request(&msgs, &[]);
        let body = build_request_body(&req, &OllamaOptions::default(), false, None).unwrap();
        let user = &body["messages"].as_array().unwrap()[0];
        assert_eq!(user["role"], "user");
        assert_eq!(user["content"], "describe this");
        let images = user["images"].as_array().unwrap();
        assert_eq!(images.len(), 1);
        assert_eq!(images[0], "AAA");
    }

    // ----- Phase 121 Task 5 — chat_stream integration -----

    use crate::transport::{ByteStream, HttpTransport};
    use crate::{LlmProvider, LlmStepEnd, LlmStreamEvent};
    use async_trait::async_trait;
    use bytes::Bytes;
    use futures_util::stream;
    use std::pin::Pin;
    use tokio_util::sync::CancellationToken;

    /// Fake HttpTransport that returns canned bytes for any
    /// post_sse. Records the request body + URL so tests can
    /// assert on what the provider sent.
    struct FakeOllamaTransport {
        canned_jsonl: Vec<u8>,
        captured: std::sync::Mutex<Vec<CapturedRequest>>,
    }

    #[derive(Debug, Clone)]
    struct CapturedRequest {
        url: String,
        body: Vec<u8>,
        had_auth: bool,
    }

    impl FakeOllamaTransport {
        fn new(jsonl: &str) -> Self {
            FakeOllamaTransport {
                canned_jsonl: jsonl.as_bytes().to_vec(),
                captured: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl HttpTransport for FakeOllamaTransport {
        async fn post_sse(
            &self,
            url: &str,
            headers: &[(&str, &str)],
            body: Vec<u8>,
            _cancellation: &CancellationToken,
        ) -> Result<ByteStream, LlmError> {
            let had_auth =
                headers.iter().any(|(k, _)| k.eq_ignore_ascii_case("authorization"));
            self.captured.lock().unwrap().push(CapturedRequest {
                url: url.to_string(),
                body,
                had_auth,
            });
            let chunk = Bytes::from(self.canned_jsonl.clone());
            Ok(Pin::from(Box::new(stream::once(async move { Ok(chunk) })))
                as Pin<Box<dyn futures_util::Stream<Item = _> + Send>>)
        }
    }

    #[tokio::test]
    async fn chat_stream_posts_to_api_chat_endpoint() {
        // The load-bearing wiring assertion: chat_stream routes
        // to /api/chat (not the OpenAI-compat /v1/chat/completions).
        let jsonl = "{\"message\":{\"role\":\"assistant\",\"content\":\"hi\"},\"done\":true,\"prompt_eval_count\":1,\"eval_count\":1}\n";
        let transport = FakeOllamaTransport::new(jsonl);
        let captured_handle = std::sync::Arc::new(std::sync::Mutex::new(
            Vec::<CapturedRequest>::new(),
        ));
        // Move the transport's mutex pointer into the provider;
        // we'll snapshot below.
        let provider = OllamaProvider::with_transport(
            OllamaConfig::default_local(),
            Box::new(transport),
        );
        let msgs = vec![LlmMessage::user_text("hi")];
        let req = LlmRequest {
            model: "qwen3.6:27b",
            system: None,
            messages: &msgs,
            tools: &[],
            max_tokens: 1024,
            temperature: None,
        id_slot: None,
        slot_hint: None,
        route: None,
        };
        let cancel = CancellationToken::new();
        let mut stream = provider.chat_stream(req, &cancel).await.unwrap();
        // Drain and verify terminal.
        while stream.next_event().await.unwrap().is_some() {}
        let end = stream.finish().await.unwrap();
        assert!(matches!(end, LlmStepEnd::FinalMessage { .. }));
        // captured_handle is the Arc above, but we can't peek
        // into the transport from here (it was moved). Use a
        // separate test below for the URL assertion via a
        // capturing transport variant.
        let _ = captured_handle;
    }

    #[test]
    fn detects_ollama_tool_parse_error_body() {
        assert!(is_ollama_tool_parse_error(
            "error parsing tool call: raw='{\"story...\"}', err=invalid character"
        ));
        assert!(is_ollama_tool_parse_error("Error Parsing Tool Call")); // case-insensitive
        // A genuine server fault must NOT be treated as retryable tool noise.
        assert!(!is_ollama_tool_parse_error("out of memory"));
        assert!(!is_ollama_tool_parse_error("model not found"));
    }

    /// #17c — fails the first `post_sse` with Ollama's tool-parse 500, then
    /// succeeds, so we can assert `chat_stream` retries and recovers.
    struct FailOnceOllamaTransport {
        calls: std::sync::atomic::AtomicUsize,
        canned_jsonl: Vec<u8>,
    }
    #[async_trait]
    impl HttpTransport for FailOnceOllamaTransport {
        async fn post_sse(
            &self,
            _url: &str,
            _headers: &[(&str, &str)],
            _body: Vec<u8>,
            _cancellation: &CancellationToken,
        ) -> Result<ByteStream, LlmError> {
            let n = self
                .calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n == 0 {
                return Err(LlmError::Api {
                    status: 500,
                    message: "error parsing tool call: raw='{\"story...\"}', \
                              err=invalid character '}' after object key"
                        .into(),
                });
            }
            let chunk = Bytes::from(self.canned_jsonl.clone());
            Ok(Pin::from(Box::new(stream::once(async move { Ok(chunk) })))
                as Pin<Box<dyn futures_util::Stream<Item = _> + Send>>)
        }
    }

    #[tokio::test]
    async fn chat_stream_retries_once_on_malformed_tool_call_500() {
        let jsonl = "{\"message\":{\"role\":\"assistant\",\"content\":\"ok\"},\"done\":true,\"prompt_eval_count\":1,\"eval_count\":1}\n";
        let transport = std::sync::Arc::new(FailOnceOllamaTransport {
            calls: std::sync::atomic::AtomicUsize::new(0),
            canned_jsonl: jsonl.as_bytes().to_vec(),
        });
        let provider = OllamaProvider::with_transport(
            OllamaConfig::default_local(),
            Box::new(ArcTransport(std::sync::Arc::clone(&transport))),
        );
        let msgs = vec![LlmMessage::user_text("hi")];
        let req = LlmRequest {
            model: "qwen3.6:27b",
            system: None,
            messages: &msgs,
            tools: &[],
            max_tokens: 1024,
            temperature: None,
        id_slot: None,
        slot_hint: None,
        route: None,
        };
        let cancel = CancellationToken::new();
        // Would be Err without the retry; the second attempt succeeds.
        let mut stream = provider
            .chat_stream(req, &cancel)
            .await
            .expect("retry recovers the malformed-tool-call 500");
        while stream.next_event().await.unwrap().is_some() {}
        assert!(matches!(
            stream.finish().await.unwrap(),
            LlmStepEnd::FinalMessage { .. }
        ));
        assert_eq!(
            transport.calls.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "post_sse called twice: initial failure + one retry"
        );
    }

    /// Adapts an `Arc<T: HttpTransport>` into a `Box<dyn HttpTransport>` so a
    /// test can hold a handle to the transport after moving it into a provider.
    struct ArcTransport<T: HttpTransport>(std::sync::Arc<T>);
    #[async_trait]
    impl<T: HttpTransport> HttpTransport for ArcTransport<T> {
        async fn post_sse(
            &self,
            url: &str,
            headers: &[(&str, &str)],
            body: Vec<u8>,
            cancellation: &CancellationToken,
        ) -> Result<ByteStream, LlmError> {
            self.0.post_sse(url, headers, body, cancellation).await
        }
    }

    /// Helper: build a provider with a transport whose captured
    /// state we can inspect after `chat_stream`. Returns the
    /// provider AND a clone of the inner Arc<Mutex<...>> so tests
    /// can read what the provider sent.
    fn provider_with_capturing_transport(
        jsonl: &str,
        config: OllamaConfig,
    ) -> (
        OllamaProvider,
        std::sync::Arc<std::sync::Mutex<Option<CapturedRequest>>>,
    ) {
        struct CapturingTransport {
            canned: Vec<u8>,
            captured:
                std::sync::Arc<std::sync::Mutex<Option<CapturedRequest>>>,
        }
        #[async_trait]
        impl HttpTransport for CapturingTransport {
            async fn post_sse(
                &self,
                url: &str,
                headers: &[(&str, &str)],
                body: Vec<u8>,
                _cancellation: &CancellationToken,
            ) -> Result<ByteStream, LlmError> {
                let had_auth = headers.iter().any(|(k, _)| {
                    k.eq_ignore_ascii_case("authorization")
                });
                *self.captured.lock().unwrap() = Some(CapturedRequest {
                    url: url.to_string(),
                    body: body.clone(),
                    had_auth,
                });
                let chunk = Bytes::from(self.canned.clone());
                Ok(Pin::from(Box::new(stream::once(
                    async move { Ok(chunk) },
                )))
                    as Pin<
                        Box<dyn futures_util::Stream<Item = _> + Send>,
                    >)
            }
        }
        let captured = std::sync::Arc::new(std::sync::Mutex::new(None));
        let transport = CapturingTransport {
            canned: jsonl.as_bytes().to_vec(),
            captured: std::sync::Arc::clone(&captured),
        };
        let provider =
            OllamaProvider::with_transport(config, Box::new(transport));
        (provider, captured)
    }

    #[tokio::test]
    async fn chat_stream_targets_native_api_chat_endpoint_not_openai_compat() {
        let jsonl = "{\"message\":{\"role\":\"assistant\",\"content\":\"hi\"},\"done\":true,\"prompt_eval_count\":1,\"eval_count\":1}\n";
        let (provider, captured) = provider_with_capturing_transport(
            jsonl,
            OllamaConfig::default_local(),
        );
        let msgs = vec![LlmMessage::user_text("hi")];
        let req = LlmRequest {
            model: "qwen3.6:27b",
            system: None,
            messages: &msgs,
            tools: &[],
            max_tokens: 1024,
            temperature: None,
        id_slot: None,
        slot_hint: None,
        route: None,
        };
        let cancel = CancellationToken::new();
        let mut stream = provider.chat_stream(req, &cancel).await.unwrap();
        while stream.next_event().await.unwrap().is_some() {}
        let _ = stream.finish().await.unwrap();
        let cap = captured.lock().unwrap().clone().expect("captured");
        // The Phase 121 load-bearing wiring assertion: native
        // /api/chat endpoint, NOT the OpenAI-compat
        // /v1/chat/completions.
        assert!(
            cap.url.ends_with("/api/chat"),
            "expected /api/chat endpoint, got {}",
            cap.url
        );
        assert!(
            !cap.url.contains("/v1/chat/completions"),
            "must NOT route through OpenAI-compat path; got {}",
            cap.url
        );
    }

    #[tokio::test]
    async fn chat_stream_default_config_omits_authorization_header() {
        // Vanilla `ollama serve` doesn't authenticate; the
        // provider must NOT emit `Authorization: Bearer ` with
        // an empty secret. Matches the OpenAI provider's
        // empty-key posture.
        let jsonl = "{\"message\":{\"role\":\"assistant\",\"content\":\"hi\"},\"done\":true,\"prompt_eval_count\":1,\"eval_count\":1}\n";
        let (provider, captured) = provider_with_capturing_transport(
            jsonl,
            OllamaConfig::default_local(),
        );
        let msgs = vec![LlmMessage::user_text("hi")];
        let req = LlmRequest {
            model: "qwen3.6:27b",
            system: None,
            messages: &msgs,
            tools: &[],
            max_tokens: 1024,
            temperature: None,
        id_slot: None,
        slot_hint: None,
        route: None,
        };
        let cancel = CancellationToken::new();
        let _ = provider.chat_stream(req, &cancel).await.unwrap();
        let cap = captured.lock().unwrap().clone().expect("captured");
        assert!(
            !cap.had_auth,
            "no Authorization header expected for default-config Ollama"
        );
    }

    #[tokio::test]
    async fn chat_stream_emits_authorization_when_api_key_set() {
        // Operator-protected Ollama behind a proxy: the api_key
        // is propagated as Bearer token.
        let jsonl = "{\"message\":{\"role\":\"assistant\",\"content\":\"hi\"},\"done\":true,\"prompt_eval_count\":1,\"eval_count\":1}\n";
        let config = OllamaConfig::default_local()
            .with_api_key(secrecy::SecretString::new("opaque-token".into()));
        let (provider, captured) =
            provider_with_capturing_transport(jsonl, config);
        let msgs = vec![LlmMessage::user_text("hi")];
        let req = LlmRequest {
            model: "qwen3.6:27b",
            system: None,
            messages: &msgs,
            tools: &[],
            max_tokens: 1024,
            temperature: None,
        id_slot: None,
        slot_hint: None,
        route: None,
        };
        let cancel = CancellationToken::new();
        let _ = provider.chat_stream(req, &cancel).await.unwrap();
        let cap = captured.lock().unwrap().clone().expect("captured");
        assert!(cap.had_auth, "Authorization header expected when api_key set");
    }

    #[tokio::test]
    async fn chat_stream_text_only_turn_produces_final_message() {
        // End-to-end: three-chunk JSONL response → FinalMessage
        // with accumulated text + usage. Phase 121's most-common
        // turn shape (chat-only response).
        let jsonl = concat!(
            "{\"message\":{\"role\":\"assistant\",\"content\":\"Hello\"},\"done\":false}\n",
            "{\"message\":{\"role\":\"assistant\",\"content\":\" world\"},\"done\":false}\n",
            "{\"message\":{\"role\":\"assistant\",\"content\":\"\"},\"done\":true,\"prompt_eval_count\":7,\"eval_count\":3}\n",
        );
        let (provider, _) = provider_with_capturing_transport(
            jsonl,
            OllamaConfig::default_local(),
        );
        let msgs = vec![LlmMessage::user_text("say hi")];
        let req = LlmRequest {
            model: "qwen3.6:27b",
            system: None,
            messages: &msgs,
            tools: &[],
            max_tokens: 1024,
            temperature: None,
        id_slot: None,
        slot_hint: None,
        route: None,
        };
        let cancel = CancellationToken::new();
        let mut stream = provider.chat_stream(req, &cancel).await.unwrap();
        let mut text = String::new();
        while let Some(ev) = stream.next_event().await.unwrap() {
            if let LlmStreamEvent::TextChunk(s) = ev {
                text.push_str(&s);
            }
        }
        assert_eq!(text, "Hello world");
        let end = stream.finish().await.unwrap();
        match end {
            LlmStepEnd::FinalMessage { text, usage } => {
                assert_eq!(text, "Hello world");
                assert_eq!(usage.input_tokens, 7);
                assert_eq!(usage.output_tokens, 3);
            }
            other => panic!("expected FinalMessage, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn chat_stream_tool_call_turn_with_known_name() {
        // End-to-end: request advertises fs.read; model emits a
        // tool call with the verbatim name; provider classifies
        // Known and dispatches the ToolCallEnd through.
        let jsonl = "{\"message\":{\"role\":\"assistant\",\"content\":\"\",\"tool_calls\":[{\"function\":{\"name\":\"fs.read\",\"arguments\":{\"path\":\"a\"}}}]},\"done\":true,\"prompt_eval_count\":5,\"eval_count\":2}\n";
        let (provider, _) = provider_with_capturing_transport(
            jsonl,
            OllamaConfig::default_local(),
        );
        let msgs = vec![LlmMessage::user_text("read a file")];
        let tools = vec![LlmToolDescriptor {
            name: "fs.read".into(),
            description: "Read a file".into(),
            input_schema: json!({"type": "object"}),
        }];
        let req = LlmRequest {
            model: "qwen3.6:27b",
            system: None,
            messages: &msgs,
            tools: &tools,
            max_tokens: 1024,
            temperature: None,
        id_slot: None,
        slot_hint: None,
        route: None,
        };
        let cancel = CancellationToken::new();
        let mut stream = provider.chat_stream(req, &cancel).await.unwrap();
        while stream.next_event().await.unwrap().is_some() {}
        let end = stream.finish().await.unwrap();
        match end {
            LlmStepEnd::ToolCalls { calls, usage, .. } => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].tool_name, "fs.read");
                assert_eq!(calls[0].input["path"], "a");
                assert!(matches!(
                    calls[0].name_resolution,
                    crate::NameResolution::Known
                ));
                assert_eq!(usage.input_tokens, 5);
                assert_eq!(usage.output_tokens, 2);
            }
            other => panic!("expected ToolCalls, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn chat_stream_hallucinated_name_flagged_unknown_through_provider() {
        // End-to-end Phase 121 load-bearing case: provider
        // wires Phase 120 Task 3 NameResolution into the stream
        // through chat_stream. Request advertised fs.read; model
        // emitted fs_read. Provider flags Unknown all the way to
        // the planner's downstream Phase 120 recovery.
        let jsonl = "{\"message\":{\"role\":\"assistant\",\"content\":\"\",\"tool_calls\":[{\"function\":{\"name\":\"fs_read\",\"arguments\":{\"path\":\"a\"}}}]},\"done\":true,\"prompt_eval_count\":1,\"eval_count\":1}\n";
        let (provider, _) = provider_with_capturing_transport(
            jsonl,
            OllamaConfig::default_local(),
        );
        let msgs = vec![LlmMessage::user_text("read")];
        let tools = vec![LlmToolDescriptor {
            name: "fs.read".into(),
            description: "Read a file".into(),
            input_schema: json!({"type": "object"}),
        }];
        let req = LlmRequest {
            model: "qwen3.6:27b",
            system: None,
            messages: &msgs,
            tools: &tools,
            max_tokens: 1024,
            temperature: None,
        id_slot: None,
        slot_hint: None,
        route: None,
        };
        let cancel = CancellationToken::new();
        let mut stream = provider.chat_stream(req, &cancel).await.unwrap();
        while stream.next_event().await.unwrap().is_some() {}
        let end = stream.finish().await.unwrap();
        match end {
            LlmStepEnd::ToolCalls { calls, .. } => {
                assert_eq!(calls[0].tool_name, "fs_read");
                match &calls[0].name_resolution {
                    crate::NameResolution::Unknown { original } => {
                        assert_eq!(original, "fs_read");
                    }
                    other => panic!("expected Unknown, got {other:?}"),
                }
            }
            other => panic!("expected ToolCalls, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn chat_stream_serializes_request_body_with_native_shape() {
        // Verify the request body the transport receives matches
        // Ollama's /api/chat wire shape (not OpenAI's).
        let jsonl = "{\"message\":{\"role\":\"assistant\",\"content\":\"\"},\"done\":true,\"prompt_eval_count\":1,\"eval_count\":1}\n";
        let config = OllamaConfig::default_local().with_options(
            OllamaOptions {
                num_ctx: Some(8192),
                ..OllamaOptions::default()
            },
        );
        let (provider, captured) =
            provider_with_capturing_transport(jsonl, config);
        let msgs = vec![LlmMessage::user_text("hi")];
        let req = LlmRequest {
            model: "qwen3.6:27b",
            system: None,
            messages: &msgs,
            tools: &[],
            max_tokens: 1024,
            temperature: Some(0.7),
            id_slot: None,
            slot_hint: None,
            route: None,
        };
        let cancel = CancellationToken::new();
        let _ = provider.chat_stream(req, &cancel).await.unwrap();
        let cap = captured.lock().unwrap().clone().expect("captured");
        let body: Value = serde_json::from_slice(&cap.body).unwrap();
        // The wire shape: model + messages + stream:true +
        // options block with operator's num_ctx AND the request's
        // temperature merged in.
        assert_eq!(body["model"], "qwen3.6:27b");
        assert_eq!(body["stream"], true);
        assert_eq!(body["options"]["num_ctx"], 8192);
        assert!(
            (body["options"]["temperature"].as_f64().unwrap() - 0.7).abs()
                < 1e-6
        );
    }

    // ----- Phase 121 Task 7 — Split-chunk JSONL through full pipeline -----

    /// Transport that returns the canned JSONL bytes as MULTIPLE
    /// chunks, simulating a real network stream where a single
    /// JSON object spans two TCP reads. Exercises the
    /// JsonlReader's buffer state machine through the full
    /// OllamaProvider → JsonlReader → OllamaStream chain.
    struct ChunkedFakeTransport {
        chunks: Vec<Vec<u8>>,
    }

    #[async_trait]
    impl HttpTransport for ChunkedFakeTransport {
        async fn post_sse(
            &self,
            _url: &str,
            _headers: &[(&str, &str)],
            _body: Vec<u8>,
            _cancellation: &CancellationToken,
        ) -> Result<ByteStream, LlmError> {
            let items: Vec<Result<Bytes, LlmError>> = self
                .chunks
                .iter()
                .map(|c| Ok::<_, LlmError>(Bytes::from(c.clone())))
                .collect();
            Ok(Pin::from(Box::new(stream::iter(items)))
                as Pin<Box<dyn futures_util::Stream<Item = _> + Send>>)
        }
    }

    #[tokio::test]
    async fn chat_stream_reassembles_jsonl_chunks_split_across_reads() {
        // Phase 121 Task 7 e2e: a real-network simulation where
        // one of the Ollama JSON objects arrives split across
        // two ByteStream chunks. The JsonlReader's buffer state
        // machine (Task 3) reassembles it; the OllamaStream
        // (Task 4) consumes one logical line per parse; the
        // OllamaProvider chat_stream (Task 5) ties the chain
        // together end-to-end.
        let chunks: Vec<Vec<u8>> = vec![
            // First chunk: complete first object + start of
            // second.
            b"{\"message\":{\"role\":\"assistant\",\"content\":\"Hello\"},\"done\":false}\n{\"message\":{\"role\":\"assistant\",\"content\":\" wor".to_vec(),
            // Second chunk: rest of second object + complete
            // terminal chunk.
            b"ld\"},\"done\":false}\n{\"message\":{\"role\":\"assistant\",\"content\":\"\"},\"done\":true,\"prompt_eval_count\":7,\"eval_count\":3}\n".to_vec(),
        ];
        let transport = ChunkedFakeTransport { chunks };
        let provider = OllamaProvider::with_transport(
            OllamaConfig::default_local(),
            Box::new(transport),
        );
        let msgs = vec![LlmMessage::user_text("say hi")];
        let req = LlmRequest {
            model: "qwen3.6:27b",
            system: None,
            messages: &msgs,
            tools: &[],
            max_tokens: 1024,
            temperature: None,
        id_slot: None,
        slot_hint: None,
        route: None,
        };
        let cancel = CancellationToken::new();
        let mut stream = provider.chat_stream(req, &cancel).await.unwrap();
        let mut text = String::new();
        while let Some(ev) = stream.next_event().await.unwrap() {
            if let LlmStreamEvent::TextChunk(s) = ev {
                text.push_str(&s);
            }
        }
        // Both text deltas reassembled correctly across the
        // ByteStream-chunk split.
        assert_eq!(text, "Hello world");
        let end = stream.finish().await.unwrap();
        match end {
            LlmStepEnd::FinalMessage { text, usage } => {
                assert_eq!(text, "Hello world");
                assert_eq!(usage.input_tokens, 7);
                assert_eq!(usage.output_tokens, 3);
            }
            other => panic!("expected FinalMessage, got {other:?}"),
        }
    }

    // ----- Phase 127 Task 6 — family-hint cache + /api/show -----

    use std::sync::Arc;

    /// Fake transport for `/api/show` testing. The shared
    /// `state` lets the test inspect call count + URLs
    /// after the provider has taken ownership of the
    /// transport via its `Box<dyn HttpTransport>` slot.
    /// `post_sse` errors — these tests don't exercise it.
    #[derive(Clone)]
    struct FakeShowTransport {
        state: Arc<FakeShowState>,
    }

    struct FakeShowState {
        canned_json: Vec<u8>,
        // None ⇒ post_json returns the canned JSON;
        // Some ⇒ post_json errors with the given Api error.
        force_error: Option<(u16, String)>,
        post_json_calls: std::sync::Mutex<Vec<String>>,
    }

    impl FakeShowTransport {
        fn ok(canned_json: &str) -> Self {
            Self {
                state: Arc::new(FakeShowState {
                    canned_json: canned_json.as_bytes().to_vec(),
                    force_error: None,
                    post_json_calls: std::sync::Mutex::new(Vec::new()),
                }),
            }
        }
        fn failing() -> Self {
            Self {
                state: Arc::new(FakeShowState {
                    canned_json: Vec::new(),
                    force_error: Some((404, "model not found".into())),
                    post_json_calls: std::sync::Mutex::new(Vec::new()),
                }),
            }
        }
        fn call_count(&self) -> usize {
            self.state.post_json_calls.lock().unwrap().len()
        }
    }

    #[async_trait]
    impl HttpTransport for FakeShowTransport {
        async fn post_sse(
            &self,
            _url: &str,
            _headers: &[(&str, &str)],
            _body: Vec<u8>,
            _cancellation: &CancellationToken,
        ) -> Result<ByteStream, LlmError> {
            Err(LlmError::Transport("post_sse not used in show test".into()))
        }
        async fn post_json(
            &self,
            url: &str,
            _headers: &[(&str, &str)],
            _body: Vec<u8>,
            _cancellation: &CancellationToken,
        ) -> Result<Vec<u8>, LlmError> {
            self.state
                .post_json_calls
                .lock()
                .unwrap()
                .push(url.to_string());
            if let Some((status, ref msg)) = self.state.force_error {
                return Err(LlmError::Api {
                    status,
                    message: msg.clone(),
                });
            }
            Ok(self.state.canned_json.clone())
        }
    }

    fn provider_with_transport(
        transport: Box<dyn HttpTransport>,
    ) -> OllamaProvider {
        OllamaProvider::with_transport(OllamaConfig::default_local(), transport)
    }

    #[tokio::test]
    async fn phase_127_family_hint_returns_qwen_family_from_show() {
        let canned =
            r#"{"details": {"family": "qwen35", "parameter_size": "27.8B"}}"#;
        let transport = FakeShowTransport::ok(canned);
        let provider = provider_with_transport(Box::new(transport.clone()));
        let family = provider.tool_call_family_hint("qwen3.6:27b").await;
        assert_eq!(family.as_deref(), Some("qwen35"));
        assert_eq!(transport.call_count(), 1);
    }

    #[tokio::test]
    async fn phase_127_family_hint_caches_per_model() {
        // Two calls for the same model should hit /api/show
        // exactly ONCE.
        let transport = FakeShowTransport::ok(r#"{"details": {"family": "qwen35"}}"#);
        let provider = provider_with_transport(Box::new(transport.clone()));
        provider.tool_call_family_hint("qwen3.6:27b").await;
        provider.tool_call_family_hint("qwen3.6:27b").await;
        assert_eq!(
            transport.call_count(),
            1,
            "two queries for the same model should result in exactly one /api/show call"
        );
    }

    #[tokio::test]
    async fn phase_127_family_hint_distinct_models_cached_separately() {
        let transport = FakeShowTransport::ok(r#"{"details": {"family": "qwen35"}}"#);
        let provider = provider_with_transport(Box::new(transport.clone()));
        provider.tool_call_family_hint("qwen3.6:27b").await;
        provider.tool_call_family_hint("gemma4:31b").await;
        assert_eq!(
            transport.call_count(),
            2,
            "distinct model names should each trigger their own /api/show call"
        );
    }

    #[tokio::test]
    async fn phase_127_family_hint_caches_failures() {
        // The failure path is also cached: a model that
        // failed once shouldn't trigger repeat queries.
        let transport = FakeShowTransport::failing();
        let provider = provider_with_transport(Box::new(transport.clone()));
        let first = provider.tool_call_family_hint("unknown:model").await;
        let second = provider.tool_call_family_hint("unknown:model").await;
        assert!(first.is_none());
        assert!(second.is_none());
        assert_eq!(
            transport.call_count(),
            1,
            "failed lookups cache the failure; second call doesn't retry"
        );
    }

    #[tokio::test]
    async fn phase_127_family_hint_missing_details_returns_none() {
        let transport = FakeShowTransport::ok(r#"{"modelfile": "FROM whatever"}"#);
        let provider = provider_with_transport(Box::new(transport));
        let family = provider.tool_call_family_hint("any:model").await;
        assert!(
            family.is_none(),
            "missing details.family returns None even on HTTP success"
        );
    }

    #[tokio::test]
    async fn phase_127_family_hint_empty_family_returns_none() {
        // Edge case: details.family is an empty string;
        // treat as no hint (string is non-discriminating).
        let transport = FakeShowTransport::ok(r#"{"details": {"family": "  "}}"#);
        let provider = provider_with_transport(Box::new(transport));
        let family = provider.tool_call_family_hint("any:model").await;
        assert!(family.is_none());
    }
}
