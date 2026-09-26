//! # aivyx-llm
//!
//! The `LlmProvider` trait and supporting types. This is the protocol
//! crate that sits between `aivyx-core`'s turn loop and any concrete
//! model backend (Anthropic, Ollama, etc.).
//!
//! `aivyx-llm` depends on no other Aivyx crate — it is at the bottom of
//! the dependency graph, and `aivyx-core` depends on *it* to declare
//! `AivyxError::Llm(#[from] LlmError)`. Providers implement the trait
//! in their own crates (or, for the reference Anthropic impl, later in
//! this same crate).
//!
//! See DESIGN.md D1 (the north-star paragraph mentions "its LlmProvider"
//! and `llm.chat_stream(...)`) and D6 (`AivyxError::Llm`).
//!
//! ## The shape in one paragraph
//!
//! A consumer builds an [`LlmRequest`] — model name, system prompt,
//! conversation history as `&[LlmMessage]`, available tools as
//! `&[LlmToolDescriptor]`, and generation knobs. It calls
//! [`LlmProvider::chat_stream`] with a `CancellationToken`, which yields
//! a boxed [`LlmStream`]. The consumer pulls mid-stream events
//! ([`LlmStreamEvent::TextChunk`] / [`LlmStreamEvent::Usage`]) via
//! [`LlmStream::next_event`] until it returns `None`, then calls
//! [`LlmStream::finish`] to obtain the terminal [`LlmStepEnd`] — either
//! `FinalMessage` (the turn ends) or `ToolCall` (the turn loop dispatches
//! the tool and loops back with a new `LlmMessage::ToolResult` appended
//! to the history).
//!
//! ## Why a two-method LlmStream instead of a `futures::Stream`
//!
//! Because the stream yields small event values (`String` text chunks,
//! `LlmUsage` deltas) but terminates with a *different* larger value
//! ([`LlmStepEnd`]), and because `LlmProvider` has to be dyn-compatible
//! (held behind `Arc<dyn LlmProvider>` by the turn planner), the
//! idiomatic `impl Stream<Item = ...>` shape does not fit. The
//! two-method pattern — `next_event` until `None`, then `finish` once —
//! separates the mid-stream and terminal concerns cleanly and stays
//! boxable.

#![allow(dead_code)]

use std::fmt;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

#[cfg(any(
    feature = "provider-anthropic",
    feature = "provider-openai",
    feature = "provider-ollama"
))]
pub mod transport;

#[cfg(feature = "provider-anthropic")]
pub mod anthropic;

#[cfg(feature = "provider-openai")]
pub mod openai;

#[cfg(feature = "provider-ollama")]
pub mod ollama;

// Phase 134 — embedded Rust-native inference. Gated by
// `provider-mistral-rs`; the backend-specific Cargo features
// (`provider-mistral-rs-cuda`, etc.) all imply this baseline so
// `cfg(feature = "provider-mistral-rs")` is sufficient here.
#[cfg(feature = "provider-mistral-rs")]
pub mod mistral_rs;

/// Chapter Stencil (ST.1) — tool-call grammar generation for
/// grammar-constrained decoding. Pure and provider-agnostic (no
/// engine), so it is ungated: the MistralRs provider consumes it
/// today and a future `llama-server` `/completion` GBNF path can
/// reuse it unchanged.
pub mod tool_grammar;

/// kvcache adoption — KV-cache slot checkout/release pool. A pure
/// numeric slot-id allocator with no I/O or knowledge of kvcache
/// persistence.
mod kv_slot_pool;
pub use kv_slot_pool::KvSlotPool;

/// kvcache adoption — Fetches `total_slots` + `build_info` from a real
/// llama-server `/props` response. Used at startup to size the
/// `KvSlotPool` and detect server upgrades via `build_info`.
mod kvcache_probe;
pub use kvcache_probe::{KVCACHE_PROBE_TIMEOUT, LlamaSlotsInfo, parse_llama_slots_info};

/// fetch_llama_slots_info requires the provider-openai feature (for reqwest).
#[cfg(feature = "provider-openai")]
pub use kvcache_probe::fetch_llama_slots_info;

/// Phase 75 — embedding provider for semantic memory search.
/// Reuses the shared HTTP transport; an OpenAI-compatible
/// `/v1/embeddings` client whose `base_url` can point at the
/// cloud API or a local server (ollama / llama.cpp /
/// text-embeddings-inference).
#[cfg(any(
    feature = "provider-anthropic",
    feature = "provider-openai",
    feature = "provider-ollama"
))]
pub mod embedding;

/// Phase 104 — `aivyx-pa init` provider credential verification.
/// Issues `GET /v1/models` against Anthropic / OpenAI to confirm
/// `(api_key, model)` is valid before the wizard writes
/// `aivyx-pa.toml`. See [`verify::verify_provider_credentials`].
#[cfg(any(
    feature = "provider-anthropic",
    feature = "provider-openai",
    feature = "provider-ollama"
))]
pub mod verify;

/// Model routing Part 3a — an `LlmProvider` that picks a model per
/// tagged request with `aivyx-route`'s shared `Router`. Ungated: it
/// dispatches through providers the caller builds.
pub mod routed;
pub use routed::{ProfileRefresher, ProviderFactory, RouteObserver, RoutedProvider, is_routable};

// ---------------------------------------------------------------------------
// Conversation messages
// ---------------------------------------------------------------------------

/// A content block within a user message. Matches the content-block
/// array format used by both Anthropic and OpenAI APIs.
///
/// Phase 163 — amendment A13 adds `DocumentBase64` so PDFs route to
/// provider-specific document blocks rather than masquerading as
/// images.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    /// Plain text content.
    Text { text: String },
    /// Base64-encoded image with MIME type (e.g. `image/png`).
    ImageBase64 { media_type: String, data: String },
    /// Phase 163 — base64-encoded document with
    /// MIME type (e.g. `application/pdf`).
    /// Routes to provider-specific document
    /// content blocks (Anthropic) or skip-and-
    /// warn on providers without native
    /// document support (OpenAI, Ollama,
    /// mistral_rs). See amendment A13.
    DocumentBase64 { media_type: String, data: String },
}

impl ContentBlock {
    /// Convenience: create a text content block.
    pub fn text(s: impl Into<String>) -> Self {
        ContentBlock::Text { text: s.into() }
    }

    /// Convenience: create an image content block from raw bytes.
    /// The caller provides the MIME type; the bytes are base64-encoded
    /// internally.
    pub fn image_from_bytes(media_type: impl Into<String>, bytes: &[u8]) -> Self {
        use base64::Engine;
        ContentBlock::ImageBase64 {
            media_type: media_type.into(),
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
        }
    }

    /// Phase 163 — convenience: create a
    /// document content block from raw bytes.
    /// Same base64 encoding shape as
    /// [`image_from_bytes`]; provider mapping
    /// downstream determines whether the block
    /// reaches the model or gets skipped with a
    /// warning.
    pub fn document_from_bytes(
        media_type: impl Into<String>,
        bytes: &[u8],
    ) -> Self {
        use base64::Engine;
        ContentBlock::DocumentBase64 {
            media_type: media_type.into(),
            data: base64::engine::general_purpose::STANDARD.encode(bytes),
        }
    }

    /// Returns `true` if this block is an image.
    pub fn is_image(&self) -> bool {
        matches!(self, ContentBlock::ImageBase64 { .. })
    }

    /// Phase 163 — Returns `true` if this block
    /// is a document.
    pub fn is_document(&self) -> bool {
        matches!(self, ContentBlock::DocumentBase64 { .. })
    }
}

/// A single entry in a conversation passed to [`LlmProvider::chat_stream`].
///
/// The caller owns the conversation as a `Vec<LlmMessage>` and replays
/// the whole history on every step. Providers are stateless from the
/// trait's perspective — a provider that wants to cache prompt prefixes
/// (e.g. Anthropic prompt caching) does so internally by hashing the
/// request, not by holding a conversation handle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum LlmMessage {
    /// A user turn. Carries one or more content blocks — text, images,
    /// or a mix. Phase 45 extended this from a plain `String` to
    /// `Vec<ContentBlock>` for multimodal support.
    User { content: Vec<ContentBlock> },

    /// A prior assistant response. Carries both the text the model
    /// emitted and any tool calls it made, so the history can be replayed
    /// to the provider faithfully. `text` may be empty if the assistant's
    /// only output was a tool call.
    Assistant {
        text: String,
        tool_calls: Vec<LlmToolCallRecord>,
    },

    /// The result of a tool call the agent executed in response to an
    /// `Assistant { tool_calls: .. }` message. Referenced by `call_id`
    /// so providers (like Anthropic) that correlate on opaque IDs can
    /// resume cleanly.
    ToolResult {
        call_id: String,
        /// Serialized tool output. The planner turns a `ToolOutcome` into
        /// this string — probably the output JSON for `Completed`, a
        /// short error message for `Denied` / `Failed` / `TimedOut`.
        content: String,
        /// If the tool failed or was denied, set so the provider can
        /// render the result as an error (Anthropic's `is_error: true`).
        is_error: bool,
    },
}

impl LlmMessage {
    /// Convenience: create a user message with a single text block.
    pub fn user_text(s: impl Into<String>) -> Self {
        LlmMessage::User {
            content: vec![ContentBlock::text(s)],
        }
    }
}

/// A record of a tool call the LLM emitted on a prior step, stored on
/// [`LlmMessage::Assistant`] so the history round-trips exactly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmToolCallRecord {
    /// Opaque ID assigned by the provider — needed to correlate an
    /// [`LlmMessage::ToolResult`] back to the call that produced it.
    pub call_id: String,
    pub tool_name: String,
    pub input: Value,
}

/// A tool the LLM is allowed to call this turn. Pre-built by the LLM
/// planner from the agent's tool registry; `aivyx-llm` never sees
/// `aivyx-core::Tool` directly, which keeps the dependency direction
/// clean (llm sits below core).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmToolDescriptor {
    pub name: String,
    pub description: String,
    /// JSON Schema for the tool's input. Anthropic and OpenAI both
    /// accept raw JSON Schema here; providers that need a different
    /// shape translate internally.
    pub input_schema: Value,
}

// ---------------------------------------------------------------------------
// Token estimation (Phase 43 Task 2)
// ---------------------------------------------------------------------------

/// Estimate the token count for a slice of conversation messages.
///
/// Uses a simple `chars / 4` heuristic — deliberately conservative
/// (overestimates for ASCII, underestimates for CJK, roughly
/// accurate for mixed English text). This is a planning heuristic,
/// not a billing counter; the goal is to trigger pruning before the
/// provider rejects the request, not to match the provider's exact
/// tokenizer. Zero new dependencies.
///
/// The estimate covers message text content, tool call inputs
/// (serialized as JSON), and tool result content. It does not
/// include per-message framing overhead (role markers, JSON
/// structure) — those are small relative to content and the 80%
/// budget threshold absorbs the error.
/// Conservative flat token estimate for an image content block.
/// Based on Anthropic's formula `(width * height) / 750` for a
/// typical 1024x1024 image. We don't decode images to get
/// dimensions — that would require an image decoder dependency.
const IMAGE_TOKEN_ESTIMATE: usize = 1600;

pub fn estimate_tokens(messages: &[LlmMessage]) -> usize {
    let mut chars: usize = 0;
    let mut images: usize = 0;
    for msg in messages {
        match msg {
            LlmMessage::User { content } => {
                for block in content {
                    match block {
                        ContentBlock::Text { text } => chars += text.len(),
                        ContentBlock::ImageBase64 { .. } => images += 1,
                        // Phase 163 — document blocks count
                        // as a single "image-equivalent" for
                        // the cost-estimate heuristic; the
                        // provider-side billing varies, but
                        // a PDF is closer to an image in
                        // tokens than to chars.
                        ContentBlock::DocumentBase64 { .. } => images += 1,
                    }
                }
            }
            LlmMessage::Assistant { text, tool_calls } => {
                chars += text.len();
                for tc in tool_calls {
                    chars += tc.tool_name.len();
                    // Serialize input to get its character count.
                    chars += tc.input.to_string().len();
                }
            }
            LlmMessage::ToolResult { content, call_id, .. } => {
                chars += content.len();
                chars += call_id.len();
            }
        }
    }
    chars.div_ceil(4) + images * IMAGE_TOKEN_ESTIMATE
}

/// Estimate the token count of a system prompt string.
pub fn estimate_system_tokens(system: Option<&str>) -> usize {
    match system {
        Some(s) => s.len().div_ceil(4),
        None => 0,
    }
}

// ---------------------------------------------------------------------------
// Request
// ---------------------------------------------------------------------------

/// One step's worth of input to [`LlmProvider::chat_stream`].
///
/// Borrowed from the caller — the caller owns the conversation history
/// and tool list for the whole turn and lends them for each step. This
/// avoids O(n²) cloning as the conversation grows.
#[derive(Debug, Clone)]
pub struct LlmRequest<'a> {
    /// Provider-specific model identifier, e.g.
    /// `"claude-3-5-sonnet-20241022"`. Providers validate against their
    /// own known-model lists and return [`LlmError::UnknownModel`] if
    /// they don't recognize the string.
    pub model: &'a str,

    /// System prompt / agent persona. `None` is valid for providers
    /// that default to their own empty system prompt.
    pub system: Option<&'a str>,

    /// Conversation so far, oldest first. The provider sees the whole
    /// history on every step; the caller is responsible for appending
    /// `ToolResult` entries between steps.
    pub messages: &'a [LlmMessage],

    /// Tools the LLM may call this step. Empty slice means "text-only,
    /// no tool use."
    pub tools: &'a [LlmToolDescriptor],

    /// Maximum tokens to generate. Required — providers differ on
    /// defaults, so the caller always declares one.
    pub max_tokens: u32,

    /// Optional sampling temperature. `None` means "provider default."
    pub temperature: Option<f32>,

    /// llama-server-only: pins this request to a specific `/slots` id (an
    /// extension beyond the OpenAI spec, but honored by llama-server on
    /// `/v1/chat/completions` -- verified empirically against a real
    /// server during `aivyx-coder`'s own kvcache adoption, not documented
    /// in llama-server's own API reference). Only ever set when
    /// `[agent] provider = "llama_cpp"` and a slot has been checked out
    /// (see `LlmPlanner`'s kvcache fields); `None` for every other
    /// provider and every llama-server request before checkout.
    pub id_slot: Option<u32>,

    /// `aivyx-broker`-only: an additive `aivyx_slot_hint` JSON field
    /// (`{"prefix_hash": ..., "preferred_slot": ...}`) the broker uses to
    /// make its own cache-locality-aware slot admission decision. Only
    /// ever set when `[agent] provider = "broker"` (see
    /// `LlmPlanner::with_broker_slot_hint`); `None` for every other
    /// provider. Mutually exclusive with `id_slot` in practice -- broker
    /// mode never checks out a local `KvSlotPool` slot (the broker owns
    /// that lifecycle itself), so `id_slot` stays `None` whenever this is
    /// `Some`, and vice versa. A backend that doesn't recognize the
    /// `aivyx_slot_hint` field (real OpenAI, Anthropic, Ollama, a bare
    /// llama-server) is unaffected: only the OpenAI-compat provider's
    /// request-body builder serializes it, and only when `Some`.
    pub slot_hint: Option<SlotHint>,

    /// Model-routing metadata for `RoutedProvider` (`routed.rs`); every
    /// other provider ignores it. `None` (an untagged call site) means a
    /// `RoutedProvider` forwards the request to the configured provider
    /// unchanged, `model` included.
    pub route: Option<RouteHint>,
}

/// See [`LlmRequest::route`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteHint {
    pub task: aivyx_route::TaskKind,
    /// Stickiness key (the conversation's session id). `None` for side calls.
    pub session: Option<String>,
    /// The caller's prompt-size estimate; becomes the minimum context
    /// window. `0` = no requirement.
    pub estimated_prompt_tokens: u32,
}

/// `aivyx-broker` slot hint carried on [`LlmRequest::slot_hint`].
/// Serialized as the `aivyx_slot_hint` JSON key by the OpenAI-compat
/// provider (`openai::provider::build_request_body`) — an additive field
/// on top of the plain OpenAI-compatible wire shape; a request without it
/// behaves exactly like a normal OpenAI-compatible call.
#[derive(Debug, Clone, PartialEq)]
pub struct SlotHint {
    /// Stable hash of (system prompt + tool definitions) — the same
    /// computation `aivyx-core`'s `compute_prefix_hash` uses for its own
    /// local `CacheKey`. The broker uses this to prefer routing requests
    /// that share a prefix onto the same physical `llama-server` slot.
    pub prefix_hash: String,
    /// Optional slot the caller would prefer, if it has one in mind.
    /// `None` lets the broker pick freely. `aivyx-pa`'s own broker-mode
    /// planner never has a preference (it never checks out a local
    /// slot), so this is always `None` from that call site today —
    /// typed as `Option` because the broker's own wire contract accepts
    /// one, not because `aivyx-pa` currently sends one.
    pub preferred_slot: Option<u32>,
}

// ---------------------------------------------------------------------------
// Stream events + terminal value
// ---------------------------------------------------------------------------

/// A mid-stream event yielded by [`LlmStream::next_event`]. The stream
/// yields zero or more of these, then returns `None`, then the caller
/// calls [`LlmStream::finish`] to obtain the terminal [`LlmStepEnd`].
#[derive(Debug, Clone, PartialEq)]
pub enum LlmStreamEvent {
    /// A chunk of assistant text. The planner relays this straight
    /// through `channel.stream_event(StreamEvent::Text(chunk))` so the
    /// user sees output as it's generated.
    TextChunk(String),

    /// Optional mid-stream usage delta. Providers that emit incremental
    /// token counts (Anthropic's `message_delta` events) surface them
    /// here; providers that only emit usage at the end just put it on
    /// the [`LlmStepEnd`] instead.
    Usage(LlmUsage),
}

/// Terminal value of an [`LlmStream`]. Returned by [`LlmStream::finish`]
/// after the event stream has been fully drained. Exactly one variant
/// is produced per step.
#[derive(Debug, Clone)]
pub enum LlmStepEnd {
    /// The LLM finished with a plain assistant message — no tool call.
    /// The planner returns `NextStep::FinalMessage(text)` to the turn
    /// loop and the turn terminates with `TurnOutcome::Completed`.
    FinalMessage { text: String, usage: LlmUsage },

    /// The LLM wants to invoke one or more tools. The planner looks up
    /// each tool by name in its registry and returns a
    /// `NextStep::ToolCalls` batch to the turn loop. On the next step
    /// the planner appends an [`LlmMessage::ToolResult`] for each
    /// `call_id` to the history.
    ///
    /// Tool-call argument deltas are reassembled by the provider before
    /// this variant is produced — the planner sees the complete `input`
    /// once per tool, not a stream of argument chunks. Providers that
    /// stream `input_json_delta` events (Anthropic) buffer them
    /// internally.
    ///
    /// Phase 40: changed from singular `ToolCall` to plural
    /// `ToolCalls` to support parallel tool execution.
    ToolCalls {
        calls: Vec<ToolCallEnd>,
        /// Any text the LLM emitted in the same step *before* the tool
        /// calls. Often empty, but models can narrate their reasoning
        /// before calling tools. The planner should still relay it to
        /// the channel so the user sees it.
        text_so_far: String,
        usage: LlmUsage,
    },
}

/// A single tool call end-state within an [`LlmStepEnd::ToolCalls`]
/// batch. Phase 40.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallEnd {
    /// Opaque ID assigned by the provider — needed to correlate an
    /// [`LlmMessage::ToolResult`] back to the call that produced it.
    pub call_id: String,
    pub tool_name: String,
    pub input: Value,
    /// Phase 120 — provider-side validation of `tool_name` against
    /// the canonical tool set the request advertised. The Phase 120
    /// planner branches on this to decide whether to dispatch
    /// directly (`Known`) or to run fuzzy-match recovery
    /// (`Unknown { original }`).
    ///
    /// The OpenAI / Ollama / Anthropic providers populate this at
    /// stream-build time from a `HashSet<&str>` over
    /// `request.tools[].name`. The substrate stays pure-function;
    /// no auto-correct happens here (Q2(c) at Phase 120 sign-off —
    /// the planner owns recovery semantics).
    ///
    /// Default `NameResolution::Known` preserves the pre-Phase-120
    /// behavior for any direct caller that constructs
    /// `ToolCallEnd` literally (e.g. test fixtures); production
    /// providers always populate truthfully.
    pub name_resolution: NameResolution,
}

/// Phase 120 — provider-side classification of a tool name against
/// the canonical tool set the request advertised. See
/// [`ToolCallEnd::name_resolution`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum NameResolution {
    /// The provider matched `tool_name` against an entry in the
    /// request's `tools[].name` set. The planner dispatches
    /// directly; no recovery needed.
    #[default]
    Known,
    /// The provider could not match `tool_name` against the
    /// advertised set. The planner's Phase 120 recovery path takes
    /// over: fuzzy-match against the registered tool names; above
    /// threshold → auto-correct + audit; below threshold →
    /// structured "did you mean?" error to the model.
    ///
    /// `original` is the verbatim name the model emitted — useful
    /// for the audit event and for the "did you mean?" message
    /// (operator forensics: did the model say `fs_read`, `FsRead`,
    /// or `fs read`?).
    Unknown { original: String },
}

/// Token usage accounting for one step. All fields are optional in
/// practice — a provider that doesn't expose breakdowns just returns
/// zeros for the fields it doesn't track.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    /// Anthropic prompt-caching write count. Zero for providers that
    /// don't cache — cheap enough to always carry, expensive enough
    /// (in $) to want visible in the audit trail.
    pub cache_creation_input_tokens: u32,
    /// Anthropic prompt-caching read count (the cost saver).
    pub cache_read_input_tokens: u32,
}

// ---------------------------------------------------------------------------
// LlmProvider + LlmStream traits
// ---------------------------------------------------------------------------

/// The protocol boundary between Aivyx's turn loop and a concrete LLM
/// backend. Dyn-compatible — the turn planner holds an
/// `Arc<dyn LlmProvider>` and the implementation is swapped at
/// construction time.
///
/// D1 commitment: every step of the tool-calling loop goes through this
/// trait's `chat_stream` method. There is no non-streaming `chat` call.
/// A provider that only supports non-streaming responses can still
/// implement this trait by buffering the full response and yielding it
/// as a single `TextChunk` before the terminal — Phase 2's focus on
/// streaming is about the *shape*, not a requirement that every
/// provider transport be genuinely incremental.
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// Begin one LLM step. Returns a boxed [`LlmStream`] the caller
    /// drains event-by-event, then calls `finish` on.
    ///
    /// The `CancellationToken` is borrowed for the lifetime of the
    /// request; providers are expected to poll it between HTTP reads
    /// (or to race their I/O against `token.cancelled()`) so a cancelled
    /// turn doesn't wait for the LLM to finish. On cancellation the
    /// returned stream is allowed to error with [`LlmError::Cancelled`]
    /// on the next `next_event` call, or to end cleanly — both are
    /// valid; the planner handles either.
    async fn chat_stream(
        &self,
        request: LlmRequest<'_>,
        cancellation: &CancellationToken,
    ) -> Result<Box<dyn LlmStream>, LlmError>;

    /// Phase 127 — model-family hint for the textual
    /// tool-call extractor's parser-priority bias. Returns
    /// the training-family identifier for `model`
    /// (Ollama's `details.family` per `/api/show` is the
    /// canonical source) so the extractor can reorder its
    /// inner-shape priority — for example, qwen-family
    /// models prefer Qwen3-Coder XML over JSON inside a
    /// `<tool_call>` wrapper.
    ///
    /// The default impl returns `None` (no hint), which
    /// the extractor treats as "default priority order."
    /// Providers that don't have a notion of model family
    /// (cloud providers serving a single fixed model, or
    /// providers that can't introspect their model
    /// metadata) can rely on the default. Ollama overrides
    /// to query `/api/show` once per model and cache the
    /// result.
    ///
    /// Failure to determine the family — network error,
    /// model not pulled, response unparseable — returns
    /// `None`. The substrate falls back to the default
    /// permissive scan; the hint is purely a priority
    /// optimization.
    async fn tool_call_family_hint(&self, _model: &str) -> Option<String> {
        None
    }
}

/// A live LLM response stream. Yields [`LlmStreamEvent`]s via
/// `next_event` until it returns `None`, then produces one terminal
/// [`LlmStepEnd`] via `finish`.
///
/// Not `Sync` — a stream is owned by one driver (the planner task) for
/// its whole lifetime. `Send` is required so providers can use async
/// runtimes that move futures across threads.
#[async_trait]
pub trait LlmStream: Send {
    /// Pull the next mid-stream event. Returns `None` when no more
    /// events will arrive — at that point the caller must call
    /// [`LlmStream::finish`] to obtain the terminal value.
    ///
    /// Calling `next_event` after it has already returned `None` is
    /// a logic bug; implementations may return `None` again or may
    /// return [`LlmError::StreamEnded`] — callers should not rely on
    /// either.
    async fn next_event(&mut self) -> Result<Option<LlmStreamEvent>, LlmError>;

    /// Consume the stream and return its terminal value. Must be
    /// called exactly once, after `next_event` has returned `None`.
    /// Consumes `Box<Self>` (not `&mut self`) so the implementation
    /// can move owned state out of the boxed stream cleanly.
    async fn finish(self: Box<Self>) -> Result<LlmStepEnd, LlmError>;
}

// ---------------------------------------------------------------------------
// LlmError
// ---------------------------------------------------------------------------

/// Errors producible by an [`LlmProvider`]. Wrapped by `AivyxError::Llm`
/// in `aivyx-core`, so each variant corresponds to a kind of failure a
/// caller might handle differently:
///
/// - `Transport` — retryable network failure
/// - `Api` — provider returned an error response (rate limit, invalid
///   request, etc.); inspect `status` to decide what to do
/// - `Parse` — response didn't match the expected schema; always a bug
/// - `Cancelled` — request aborted via the `CancellationToken`
/// - `StreamEnded` — stream terminated before a [`LlmStepEnd`] could be
///   produced (truncated response, disconnected mid-stream)
/// - `UnknownModel` — the `model` field in the request isn't recognized
/// - `Config` — provider misconfiguration (missing API key, bad URL)
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum LlmError {
    #[error("http transport error: {0}")]
    Transport(String),

    #[error("provider API error (status {status}): {message}")]
    Api { status: u16, message: String },

    #[error("response parsing failed: {0}")]
    Parse(String),

    #[error("request cancelled")]
    Cancelled,

    #[error("stream ended unexpectedly: {0}")]
    StreamEnded(String),

    #[error("unknown model: {0}")]
    UnknownModel(String),

    #[error("configuration error: {0}")]
    Config(String),

    #[error("model routing: {0}")]
    Routing(String),
}

// ---------------------------------------------------------------------------
// Debug helper — LlmRequest's `messages` and `tools` are slices, so the
// derived Debug is fine, but we want a compact Display for logging.
// ---------------------------------------------------------------------------

impl fmt::Display for LlmRequest<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "LlmRequest(model={}, msgs={}, tools={}, max_tokens={})",
            self.model,
            self.messages.len(),
            self.tools.len(),
            self.max_tokens,
        )
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    // -----------------------------------------------------------------------
    // Fake provider — used by every dyn-compat test in this module.
    //
    // Scripts both the mid-stream events and the terminal value at
    // construction time. Mirrors `VecPlanner`'s "walk a pre-recorded
    // script" shape so the test fixture is obvious.
    // -----------------------------------------------------------------------

    struct FakeProvider {
        script: Mutex<Option<FakeScript>>,
    }

    struct FakeScript {
        events: Vec<LlmStreamEvent>,
        terminal: LlmStepEnd,
    }

    impl FakeProvider {
        fn new(events: Vec<LlmStreamEvent>, terminal: LlmStepEnd) -> Self {
            FakeProvider {
                script: Mutex::new(Some(FakeScript { events, terminal })),
            }
        }
    }

    #[async_trait]
    impl LlmProvider for FakeProvider {
        async fn chat_stream(
            &self,
            _request: LlmRequest<'_>,
            _cancellation: &CancellationToken,
        ) -> Result<Box<dyn LlmStream>, LlmError> {
            let script = self
                .script
                .lock()
                .unwrap()
                .take()
                .ok_or_else(|| LlmError::Config("FakeProvider exhausted".to_string()))?;
            Ok(Box::new(FakeStream {
                events: script.events.into_iter(),
                terminal: Some(script.terminal),
            }))
        }
    }

    struct FakeStream {
        events: std::vec::IntoIter<LlmStreamEvent>,
        terminal: Option<LlmStepEnd>,
    }

    #[async_trait]
    impl LlmStream for FakeStream {
        async fn next_event(&mut self) -> Result<Option<LlmStreamEvent>, LlmError> {
            Ok(self.events.next())
        }
        async fn finish(self: Box<Self>) -> Result<LlmStepEnd, LlmError> {
            self.terminal
                .ok_or_else(|| LlmError::StreamEnded("finish called twice".to_string()))
        }
    }

    fn sample_request<'a>(
        messages: &'a [LlmMessage],
        tools: &'a [LlmToolDescriptor],
    ) -> LlmRequest<'a> {
        LlmRequest {
            model: "claude-3-5-sonnet-20241022",
            system: Some("You are a test fixture."),
            messages,
            tools,
            max_tokens: 256,
            temperature: Some(0.2),
            id_slot: None,
            slot_hint: None,
            route: None,
        }
    }

    // -----------------------------------------------------------------------
    // Test 1 — dyn compatibility proof.
    //
    // If any method on LlmProvider or LlmStream accidentally used
    // `impl Trait` or a generic parameter, this test wouldn't compile.
    // That's the whole point — the ability to construct `Box<dyn _>` is
    // the trait's most important structural property.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn llm_provider_is_dyn_compatible() {
        let provider: Box<dyn LlmProvider> = Box::new(FakeProvider::new(
            vec![],
            LlmStepEnd::FinalMessage {
                text: "ok".to_string(),
                usage: LlmUsage::default(),
            },
        ));

        let messages: Vec<LlmMessage> = vec![LlmMessage::user_text("hi")];
        let tools: Vec<LlmToolDescriptor> = vec![];
        let token = CancellationToken::new();

        let stream = provider
            .chat_stream(sample_request(&messages, &tools), &token)
            .await
            .expect("fake provider must accept a request");

        // stream is Box<dyn LlmStream> — the dyn compatibility we care
        // about. Drain and finish.
        let mut stream: Box<dyn LlmStream> = stream;
        assert!(stream.next_event().await.unwrap().is_none());
        let end = stream.finish().await.unwrap();
        match end {
            LlmStepEnd::FinalMessage { text, .. } => assert_eq!(text, "ok"),
            other => panic!("expected FinalMessage, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Test 2 — text chunks reassemble into a full message.
    //
    // The most common shape: the provider streams three text deltas and
    // terminates with a FinalMessage. The caller concatenates the chunks
    // it saw and cross-checks against the terminal's `text` field.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn text_chunks_reassemble_into_final_message() {
        let provider = FakeProvider::new(
            vec![
                LlmStreamEvent::TextChunk("Hello, ".to_string()),
                LlmStreamEvent::TextChunk("how can I ".to_string()),
                LlmStreamEvent::TextChunk("help?".to_string()),
            ],
            LlmStepEnd::FinalMessage {
                text: "Hello, how can I help?".to_string(),
                usage: LlmUsage {
                    input_tokens: 10,
                    output_tokens: 7,
                    ..LlmUsage::default()
                },
            },
        );

        let messages = vec![LlmMessage::user_text("hi")];
        let tools: Vec<LlmToolDescriptor> = vec![];
        let token = CancellationToken::new();

        let mut stream = provider
            .chat_stream(sample_request(&messages, &tools), &token)
            .await
            .unwrap();

        let mut reassembled = String::new();
        while let Some(event) = stream.next_event().await.unwrap() {
            match event {
                LlmStreamEvent::TextChunk(chunk) => reassembled.push_str(&chunk),
                LlmStreamEvent::Usage(_) => {}
            }
        }

        let end = stream.finish().await.unwrap();
        match end {
            LlmStepEnd::FinalMessage { text, usage } => {
                assert_eq!(reassembled, text);
                assert_eq!(reassembled, "Hello, how can I help?");
                assert_eq!(usage.output_tokens, 7);
            }
            other => panic!("expected FinalMessage, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Test 3 — terminal ToolCall carries reassembled input.
    //
    // The provider emits zero text and terminates with a ToolCall. The
    // planner's job on this path is to look up the tool by name and
    // dispatch it; this test just verifies the shape arrives intact.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn terminal_tool_call_carries_full_input() {
        let input = json!({"query": "yesterday", "limit": 10});
        let provider = FakeProvider::new(
            vec![],
            LlmStepEnd::ToolCalls {
                calls: vec![ToolCallEnd {
                    call_id: "toolu_01".to_string(),
                    tool_name: "memory.read".to_string(),
                    input: input.clone(),
                    name_resolution: crate::NameResolution::Known,
                }],
                text_so_far: String::new(),
                usage: LlmUsage::default(),
            },
        );

        let messages = vec![LlmMessage::user_text("what did I work on yesterday?")];
        let tools = vec![LlmToolDescriptor {
            name: "memory.read".to_string(),
            description: "recall prior sessions".to_string(),
            input_schema: json!({"type": "object"}),
        }];
        let token = CancellationToken::new();

        let mut stream = provider
            .chat_stream(sample_request(&messages, &tools), &token)
            .await
            .unwrap();

        // Zero mid-stream events — the first next_event returns None.
        assert!(stream.next_event().await.unwrap().is_none());

        match stream.finish().await.unwrap() {
            LlmStepEnd::ToolCalls {
                calls,
                text_so_far,
                ..
            } => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].call_id, "toolu_01");
                assert_eq!(calls[0].tool_name, "memory.read");
                assert_eq!(calls[0].input, input);
                assert!(text_so_far.is_empty());
            }
            other => panic!("expected ToolCalls, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Test 4 — LlmMessage round-trips through serde_json.
    //
    // Task 3's Anthropic impl will serialize `&[LlmMessage]` into the
    // provider's request body, so the serde shape has to be stable now.
    // Covers all three variants and LlmToolCallRecord / LlmUsage.
    // -----------------------------------------------------------------------

    #[test]
    fn llm_message_round_trips_through_serde() {
        let original = vec![
            LlmMessage::user_text("hi"),
            LlmMessage::Assistant {
                text: "Looking that up.".to_string(),
                tool_calls: vec![LlmToolCallRecord {
                    call_id: "toolu_42".to_string(),
                    tool_name: "memory.read".to_string(),
                    input: json!({"query": "yesterday"}),
                }],
            },
            LlmMessage::ToolResult {
                call_id: "toolu_42".to_string(),
                content: r#"{"results":["wrote DESIGN.md"]}"#.to_string(),
                is_error: false,
            },
        ];

        let json = serde_json::to_string(&original).expect("serialize");
        let back: Vec<LlmMessage> = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(original, back);

        // Tag discriminator was chosen deliberately: the JSON must have a
        // `role` field so Anthropic's API shape (which also uses `role`)
        // maps cleanly. Don't rely on exact output — just verify the tag.
        assert!(json.contains(r#""role":"user""#));
        assert!(json.contains(r#""role":"assistant""#));
        assert!(json.contains(r#""role":"tool_result""#));
    }

    // -----------------------------------------------------------------------
    // Test 5 — LlmError Display strings match the declared format.
    //
    // AivyxError::Llm(#[from] LlmError) in core will use these as its
    // display output, so pinning them here lets core's tests rely on
    // stable strings.
    // -----------------------------------------------------------------------

    #[test]
    fn llm_error_display_strings_are_stable() {
        assert_eq!(
            LlmError::Transport("tls handshake".to_string()).to_string(),
            "http transport error: tls handshake"
        );
        assert_eq!(
            LlmError::Api {
                status: 429,
                message: "rate limited".to_string()
            }
            .to_string(),
            "provider API error (status 429): rate limited"
        );
        assert_eq!(LlmError::Cancelled.to_string(), "request cancelled");
        assert_eq!(
            LlmError::UnknownModel("gpt-9".to_string()).to_string(),
            "unknown model: gpt-9"
        );
    }

    // Suppress the "Arc is imported but unused" lint if we ever drop
    // Arc-using test helpers — this is here only so the import survives
    // a refactor that briefly removes then restores it. It's a no-op.
    #[allow(dead_code)]
    fn _keep_arc_import_alive(_: Arc<u8>) {}

    // -----------------------------------------------------------------------
    // Token estimation tests (Phase 43 Task 2)
    // -----------------------------------------------------------------------

    #[test]
    fn estimate_tokens_empty() {
        assert_eq!(estimate_tokens(&[]), 0);
    }

    #[test]
    fn estimate_tokens_user_message() {
        // 12 chars -> 3 tokens
        let msgs = vec![LlmMessage::user_text("hello world!")];
        assert_eq!(estimate_tokens(&msgs), 3);
    }

    #[test]
    fn estimate_tokens_assistant_with_tool_call() {
        let msgs = vec![LlmMessage::Assistant {
            text: "Let me check.".to_string(), // 14 chars
            tool_calls: vec![LlmToolCallRecord {
                call_id: "c1".to_string(),
                tool_name: "fs.read".to_string(), // 7 chars
                input: json!({ "path": "/tmp" }),  // ~16 chars serialized
            }],
        }];
        let tokens = estimate_tokens(&msgs);
        // 14 + 7 + ~16 = ~37 chars -> ~10 tokens
        assert!(tokens > 5 && tokens < 20, "got {tokens}");
    }

    #[test]
    fn estimate_tokens_tool_result() {
        let msgs = vec![LlmMessage::ToolResult {
            call_id: "c1".to_string(), // 2 chars
            content: "file contents here".to_string(), // 18 chars
            is_error: false,
        }];
        // 2 + 18 = 20 chars -> 5 tokens
        assert_eq!(estimate_tokens(&msgs), 5);
    }

    #[test]
    fn estimate_tokens_multi_message() {
        let msgs = vec![
            LlmMessage::user_text("abcd"), // 4 chars
            LlmMessage::Assistant { text: "efgh".to_string(), tool_calls: vec![] }, // 4 chars
            LlmMessage::user_text("ijkl"), // 4 chars
        ];
        // 12 chars -> 3 tokens
        assert_eq!(estimate_tokens(&msgs), 3);
    }

    #[test]
    fn estimate_tokens_with_image() {
        let msgs = vec![LlmMessage::User {
            content: vec![
                ContentBlock::text("describe this"),
                ContentBlock::ImageBase64 {
                    media_type: "image/png".to_string(),
                    data: "iVBOR...".to_string(),
                },
            ],
        }];
        let tokens = estimate_tokens(&msgs);
        // 13 chars / 4 = 4 tokens + 1600 image tokens = 1604
        assert_eq!(tokens, 1604);
    }

    // ---- Phase 163 / Amendment A13 — DocumentBase64 ----

    #[test]
    fn document_from_bytes_encodes_base64() {
        let block = ContentBlock::document_from_bytes(
            "application/pdf",
            b"%PDF-1.4 fake data",
        );
        match block {
            ContentBlock::DocumentBase64 { media_type, data } => {
                assert_eq!(media_type, "application/pdf");
                // base64 of "%PDF-1.4 fake data"
                assert_eq!(data, "JVBERi0xLjQgZmFrZSBkYXRh");
            }
            other => panic!("expected DocumentBase64, got {other:?}"),
        }
    }

    #[test]
    fn is_document_returns_true_only_for_document_variant() {
        assert!(ContentBlock::document_from_bytes("application/pdf", b"x")
            .is_document());
        assert!(!ContentBlock::text("hello").is_document());
        assert!(!ContentBlock::image_from_bytes("image/png", b"x")
            .is_document());
    }

    #[test]
    fn is_image_stays_false_for_document_variant() {
        // Regression pin so a future refactor
        // doesn't accidentally widen is_image to
        // include documents.
        let doc = ContentBlock::document_from_bytes("application/pdf", b"x");
        assert!(!doc.is_image());
    }

    #[test]
    fn estimate_tokens_with_document_counts_as_image_equivalent() {
        // Phase 163 — documents count as the
        // same per-block budget as images for
        // the cost-estimate heuristic.
        let msgs = vec![LlmMessage::User {
            content: vec![
                ContentBlock::text("summarize this paper"),
                ContentBlock::DocumentBase64 {
                    media_type: "application/pdf".to_string(),
                    data: "JVBER...".to_string(),
                },
            ],
        }];
        let tokens = estimate_tokens(&msgs);
        // 20 chars / 4 = 5 tokens + 1600 doc-as-image tokens = 1605
        assert_eq!(tokens, 1605);
    }

    #[test]
    fn estimate_system_tokens_none() {
        assert_eq!(estimate_system_tokens(None), 0);
    }

    #[test]
    fn estimate_system_tokens_some() {
        // 20 chars -> 5 tokens
        assert_eq!(estimate_system_tokens(Some("You are a helpful AI")), 5);
    }

    #[test]
    fn content_block_serde_roundtrip() {
        let blocks = vec![
            ContentBlock::text("hello"),
            ContentBlock::ImageBase64 {
                media_type: "image/jpeg".to_string(),
                data: "base64data".to_string(),
            },
        ];
        let json = serde_json::to_string(&blocks).unwrap();
        let back: Vec<ContentBlock> = serde_json::from_str(&json).unwrap();
        assert_eq!(blocks, back);
        assert!(json.contains(r#""type":"text""#));
        assert!(json.contains(r#""type":"image_base64""#));
    }

    #[test]
    fn user_text_convenience() {
        let msg = LlmMessage::user_text("hi");
        match &msg {
            LlmMessage::User { content } => {
                assert_eq!(content.len(), 1);
                assert_eq!(content[0], ContentBlock::text("hi"));
            }
            _ => panic!("expected User"),
        }
    }

    #[test]
    fn routing_errors_explain_themselves() {
        assert_eq!(
            LlmError::Routing("no model has vision".into()).to_string(),
            "model routing: no model has vision"
        );
    }

    #[test]
    fn a_route_hint_carries_task_session_and_estimate() {
        let hint = RouteHint {
            task: aivyx_route::TaskKind::Chat,
            session: Some("s".into()),
            estimated_prompt_tokens: 12,
        };
        assert_eq!(hint.clone(), hint);
    }
}
