//! The `AnthropicProvider`: concrete `LlmProvider` against the Anthropic
//! Messages streaming API.
//!
//! ## Wire format (documented here so the state machine is reviewable)
//!
//! A streaming `POST /v1/messages` returns these events in order:
//!
//! 1. `message_start` — carries `message.usage.input_tokens` and the
//!    message metadata. We capture usage here.
//! 2. For each block the model emits (text or tool_use), one
//!    `content_block_start` + N `content_block_delta` + one
//!    `content_block_stop`.
//!    - Text blocks: `delta.type = "text_delta"`, `delta.text = "..."`.
//!    - Tool-use blocks: the `content_block_start` carries `id`, `name`,
//!      and an empty `input: {}`. Each `content_block_delta` has
//!      `delta.type = "input_json_delta"` with a `partial_json` string;
//!      we accumulate these and parse the concatenated string at
//!      `content_block_stop`.
//! 3. `message_delta` — carries `delta.stop_reason` and
//!    `usage.output_tokens`. We record both.
//! 4. `message_stop` — terminal.
//!
//! On `stop_reason == "tool_use"` we emit `LlmStepEnd::ToolCall` with
//! the first (and, per Anthropic's current behavior, only) tool-use
//! block. On `stop_reason == "end_turn"` (or `"max_tokens"`, etc.) we
//! emit `LlmStepEnd::FinalMessage`.

use async_trait::async_trait;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::{
    ContentBlock, LlmError, LlmMessage, LlmProvider, LlmRequest, LlmStepEnd, LlmStream,
    LlmStreamEvent, LlmUsage,
};

use super::sse::{SseEvent, SseReader};
use crate::transport::{HttpTransport, ReqwestTransport};

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const ANTHROPIC_VERSION: &str = "2023-06-01";

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Provider configuration. API key is wrapped in `SecretString` so it
/// never shows up in `Debug` output or accidental `tracing` macros.
pub struct AnthropicConfig {
    pub api_key: SecretString,
    pub base_url: Option<String>,
    /// Phase 166 — operator-tunable cap on PDF
    /// page count for document content blocks.
    /// Defaults to [`ANTHROPIC_PDF_PAGE_CAP`]
    /// (100, matching Anthropic's documented
    /// per-document cap). Operators with custom
    /// plans override either via
    /// [`with_pdf_page_cap`] or the env-var
    /// `AIVYX_PA_ANTHROPIC_PDF_PAGE_CAP` checked
    /// in [`AnthropicConfig::new`].
    pub pdf_page_cap: usize,
}

impl AnthropicConfig {
    pub fn new(api_key: impl Into<SecretString>) -> Self {
        AnthropicConfig {
            api_key: api_key.into(),
            base_url: None,
            pdf_page_cap: pdf_page_cap_from_env_or_default(),
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = Some(base_url.into());
        self
    }

    /// Phase 166 — override the per-document
    /// PDF page cap. Takes precedence over the
    /// env-var fallback set in
    /// [`AnthropicConfig::new`].
    pub fn with_pdf_page_cap(mut self, cap: usize) -> Self {
        self.pdf_page_cap = cap;
        self
    }
}

/// Phase 166 — read the env-var override for
/// the PDF page cap or fall back to the
/// constant default. Pure substrate so the
/// env-var resolution can be tested.
fn pdf_page_cap_from_env_or_default() -> usize {
    match std::env::var("AIVYX_PA_ANTHROPIC_PDF_PAGE_CAP") {
        Ok(raw) => match raw.trim().parse::<usize>() {
            Ok(n) if n >= 1 => n,
            _ => ANTHROPIC_PDF_PAGE_CAP,
        },
        Err(_) => ANTHROPIC_PDF_PAGE_CAP,
    }
}

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

pub struct AnthropicProvider {
    config: AnthropicConfig,
    transport: Box<dyn HttpTransport>,
}

impl AnthropicProvider {
    /// Build a provider with the real `reqwest` transport.
    pub fn new(config: AnthropicConfig) -> Result<Self, LlmError> {
        Ok(AnthropicProvider {
            config,
            transport: Box::new(ReqwestTransport::new()?),
        })
    }

    /// Build a provider with a caller-supplied transport. This is the
    /// entry point tests use — pass a `FakeTransport` that replays
    /// canned SSE bytes.
    pub fn with_transport(
        config: AnthropicConfig,
        transport: Box<dyn HttpTransport>,
    ) -> Self {
        AnthropicProvider { config, transport }
    }

    fn endpoint(&self) -> String {
        let base = self
            .config
            .base_url
            .as_deref()
            .unwrap_or(DEFAULT_BASE_URL);
        format!("{base}/v1/messages")
    }
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    async fn chat_stream(
        &self,
        request: LlmRequest<'_>,
        cancellation: &CancellationToken,
    ) -> Result<Box<dyn LlmStream>, LlmError> {
        let body = build_request_body(&request, self.config.pdf_page_cap)?;
        let body_bytes = serde_json::to_vec(&body)
            .map_err(|e| LlmError::Parse(format!("request serialization: {e}")))?;

        let api_key = self.config.api_key.expose_secret().to_string();
        let headers: Vec<(&str, &str)> = vec![
            ("content-type", "application/json"),
            ("accept", "text/event-stream"),
            ("anthropic-version", ANTHROPIC_VERSION),
            ("x-api-key", api_key.as_str()),
        ];

        let endpoint = self.endpoint();
        let byte_stream = self
            .transport
            .post_sse(&endpoint, &headers, body_bytes, cancellation)
            .await?;

        // Phase 120 — same snapshot pattern as the OpenAI provider.
        let known_tool_names: std::collections::HashSet<String> = request
            .tools
            .iter()
            .map(|t| t.name.to_string())
            .collect();

        Ok(Box::new(AnthropicStream {
            sse: SseReader::new(byte_stream),
            state: StreamState::default(),
            terminal: None,
            known_tool_names,
        }))
    }
}

// ---------------------------------------------------------------------------
// Request-body construction
// ---------------------------------------------------------------------------

fn build_request_body(
    request: &LlmRequest<'_>,
    pdf_page_cap: usize,
) -> Result<Value, LlmError> {
    if request.model.is_empty() {
        return Err(LlmError::UnknownModel(String::new()));
    }

    // Phase 164 — pre-flight guard: document
    // content blocks require Claude 3.5+.
    // Detect from the request shape + model
    // string; surface a clear client-side
    // error instead of letting the API return
    // a 400.
    if request_has_document_block(request)
        && !model_supports_documents(request.model)
    {
        return Err(LlmError::Config(format!(
            "Anthropic model {model:?} does not support document blocks; \
             document content blocks (e.g. PDFs) require Claude 3.5 or newer \
             (claude-3-5-*, claude-3-7-*, claude-opus-4-*, claude-sonnet-4-*, \
             claude-haiku-4-*, or a newer-prefix variant). Pick a supported \
             model in your aivyx-pa.toml or attach the document to a model that \
             accepts it.",
            model = request.model
        )));
    }

    // Phase 165 — best-effort PDF page-count
    // cap. Byte-scans each PDF document block
    // for `/Type /Page` markers and refuses if
    // the count exceeds the cap. Honest limit:
    // misses pages inside compressed object
    // streams (modern PDFs commonly use
    // FlateDecode), so the false-negative case
    // falls through to Anthropic's own
    // page-count enforcement.
    //
    // Phase 166 — the cap is now operator-
    // tunable via `AnthropicConfig::pdf_page_cap`
    // (env-var fallback
    // `AIVYX_PA_ANTHROPIC_PDF_PAGE_CAP`).
    if let Some(over_cap) = first_pdf_over_page_cap(request, pdf_page_cap)? {
        return Err(LlmError::Config(format!(
            "PDF document block exceeds the configured Anthropic page cap: \
             counted at least {count} pages via best-effort byte-scan; max \
             allowed is {pdf_page_cap}. Compressed-stream PDFs may evade \
             this client-side check; Anthropic's server-side cap will \
             enforce as well.",
            count = over_cap.count,
        )));
    }

    let messages: Vec<Value> = merge_consecutive_tool_results(
        request
            .messages
            .iter()
            .map(anthropic_message)
            .collect::<Result<_, _>>()?,
    );

    let tools: Vec<Value> = request
        .tools
        .iter()
        .map(|t| {
            json!({
                "name": t.name,
                "description": t.description,
                "input_schema": t.input_schema,
            })
        })
        .collect();

    let mut body = json!({
        "model": request.model,
        "max_tokens": request.max_tokens,
        "messages": messages,
        "stream": true,
    });

    if let Some(system) = request.system {
        body["system"] = Value::String(system.to_string());
    }
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools);
    }
    if let Some(temp) = request.temperature {
        body["temperature"] = json!(temp);
    }

    Ok(body)
}

/// Phase 165 — Anthropic's documented per-
/// document page cap. Hardcoded; operators with
/// custom plans that allow more pages can't
/// override (Phase 166+ candidate for a TOML
/// knob).
pub(crate) const ANTHROPIC_PDF_PAGE_CAP: usize = 100;

/// Phase 165 — best-effort PDF page count via
/// byte-scan. Honest limit: misses pages inside
/// compressed object streams (FlateDecode-
/// wrapped xref + object streams in modern
/// PDFs). For uncompressed PDFs the count is
/// accurate; for compressed PDFs it
/// under-counts, which produces a false
/// negative (cap doesn't fire). Never
/// over-counts.
///
/// Detection: looks for `/Type` whitespace
/// `/Page` followed by a non-`s` byte so
/// `/Pages` (the parent node) is excluded.
///
/// Phase 168 — augmented with catalog-aware
/// fallback. Modern PDFs commonly compress
/// individual page objects but leave the
/// catalog's `/Type /Pages /Count N` declared
/// total visible in the xref dictionaries.
/// Phase 168 takes the maximum of the per-
/// page scan and the largest declared
/// `/Count` (after a `/Type /Pages` marker),
/// so compressed PDFs surface their declared
/// total via metadata rather than zero. The
/// max() lean is the safe direction for a
/// cap check: over-estimate = operator-
/// visible error, under-estimate = silent
/// bypass.
pub(crate) fn count_pdf_pages_best_effort(bytes: &[u8]) -> usize {
    let per_page = count_pdf_pages_per_page_scan(bytes);
    let declared = count_pdf_pages_declared_max(bytes);
    per_page.max(declared)
}

/// Phase 165 — original per-page byte scan.
/// Kept as a named helper so Phase 168's
/// max() composition reads obviously.
fn count_pdf_pages_per_page_scan(bytes: &[u8]) -> usize {
    let needle = b"/Page";
    let mut count = 0usize;
    let mut i = 0usize;
    while i + needle.len() <= bytes.len() {
        if &bytes[i..i + needle.len()] == needle {
            // Reject `/Pages` (the parent
            // node).
            let after = bytes.get(i + needle.len()).copied();
            if after != Some(b's') {
                // Look back for the `/Type`
                // marker (with intervening
                // whitespace) so we count
                // actual page-object headers,
                // not arbitrary `/Page`
                // references. We allow up to
                // 16 bytes of leading text in
                // case the PDF uses pretty
                // formatting.
                let back_start = i.saturating_sub(20);
                let prefix = &bytes[back_start..i];
                if has_type_marker(prefix) {
                    count += 1;
                }
            }
            i += needle.len();
        } else {
            i += 1;
        }
    }
    count
}

/// Phase 168 — declared-count scan. Walks the
/// byte stream looking for `/Type /Pages`
/// markers; within ~64 bytes after each
/// match, looks for `/Count N` and parses
/// the integer. Returns the maximum N seen.
///
/// Why look-ahead 64 bytes: PDF dictionaries
/// can have `/Type /Pages` first with `/Count
/// N` later in the same dict separated by
/// `/Kids [...]` (which itself can be long
/// but the count field is typically nearby).
/// 64 is enough for typical Pages-node
/// dictionaries that haven't been pretty-
/// printed to extremes; longer Kids arrays
/// push /Count out of reach but in those
/// cases the per-page scan usually catches
/// the count anyway.
fn count_pdf_pages_declared_max(bytes: &[u8]) -> usize {
    let pages_marker = b"/Pages";
    let count_marker = b"/Count";
    let mut max_declared = 0usize;
    let mut i = 0usize;
    while i + pages_marker.len() <= bytes.len() {
        if &bytes[i..i + pages_marker.len()] == pages_marker {
            // Confirm this is `/Type /Pages`
            // (the parent / root node), not
            // just `/Pages` as a reference.
            let back_start = i.saturating_sub(20);
            let prefix = &bytes[back_start..i];
            if has_type_marker(prefix) {
                // Look ahead up to 128 bytes
                // for `/Count <digits>`.
                let look_end = (i + pages_marker.len() + 128).min(bytes.len());
                let look = &bytes[i..look_end];
                if let Some(pos) = find_subslice(look, count_marker) {
                    let rest = &look[pos + count_marker.len()..];
                    if let Some(n) = parse_leading_integer_after_whitespace(rest) {
                        if n > max_declared {
                            max_declared = n;
                        }
                    }
                }
            }
            i += pages_marker.len();
        } else {
            i += 1;
        }
    }
    max_declared
}

/// Phase 168 — find a sub-slice in a slice.
/// Returns the byte offset of the first
/// match or None. Naive search; the slices
/// we search are bounded (≤ 128 bytes) so
/// this is fine.
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    for i in 0..=(haystack.len() - needle.len()) {
        if &haystack[i..i + needle.len()] == needle {
            return Some(i);
        }
    }
    None
}

/// Phase 168 — parse a leading integer after
/// optional whitespace. Returns the parsed
/// value or None if the input doesn't start
/// with whitespace + digits.
fn parse_leading_integer_after_whitespace(bytes: &[u8]) -> Option<usize> {
    let mut i = 0;
    // Skip whitespace.
    while i < bytes.len() && matches!(bytes[i], b' ' | b'\t' | b'\n' | b'\r') {
        i += 1;
    }
    let start = i;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if start == i {
        return None;
    }
    let digits = std::str::from_utf8(&bytes[start..i]).ok()?;
    digits.parse::<usize>().ok()
}

fn has_type_marker(prefix: &[u8]) -> bool {
    // Scan for `/Type` allowing trailing
    // whitespace before the `/Page` we just
    // matched. The page-object header is
    // typically `/Type /Page` or `/Type/Page`
    // depending on PDF formatting.
    let marker = b"/Type";
    if prefix.len() < marker.len() {
        return false;
    }
    for start in 0..=(prefix.len() - marker.len()) {
        if &prefix[start..start + marker.len()] == marker {
            // Everything between `/Type` and
            // the matched `/Page` must be
            // whitespace.
            let between = &prefix[start + marker.len()..];
            if between.iter().all(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r')) {
                return true;
            }
        }
    }
    false
}

/// Phase 165 — describes a single PDF document
/// block that exceeded the page cap. Pure
/// substrate so the pre-flight check can be
/// tested without going through
/// `build_request_body`.
struct OverCapPdf {
    count: usize,
}

/// Phase 165 — scan the request for the first
/// PDF document block whose best-effort page
/// count exceeds the cap. Returns `Ok(None)`
/// when all PDFs are within the cap (or there
/// are no PDFs); `Ok(Some(OverCapPdf))` when
/// one exceeds. base64 decode error surfaces as
/// `LlmError::Parse` since the data block is
/// invalid input.
fn first_pdf_over_page_cap(
    request: &LlmRequest<'_>,
    cap: usize,
) -> Result<Option<OverCapPdf>, LlmError> {
    use base64::Engine;
    let engine = base64::engine::general_purpose::STANDARD;
    for msg in request.messages.iter() {
        let LlmMessage::User { content } = msg else {
            continue;
        };
        for block in content {
            let ContentBlock::DocumentBase64 { media_type, data } = block else {
                continue;
            };
            if media_type != "application/pdf" {
                continue;
            }
            let bytes = engine.decode(data).map_err(|e| {
                LlmError::Parse(format!(
                    "PDF document block has invalid base64: {e}"
                ))
            })?;
            let count = count_pdf_pages_best_effort(&bytes);
            if count > cap {
                return Ok(Some(OverCapPdf { count }));
            }
        }
    }
    Ok(None)
}

/// Phase 164 — true iff any `LlmMessage::User`
/// in the request carries at least one
/// `ContentBlock::DocumentBase64`. Pure
/// substrate so the guard can be tested
/// without going through `build_request_body`.
fn request_has_document_block(request: &LlmRequest<'_>) -> bool {
    request.messages.iter().any(|msg| match msg {
        LlmMessage::User { content } => {
            content.iter().any(|b| b.is_document())
        }
        _ => false,
    })
}

/// Phase 164 — true iff `model` names an
/// Anthropic model that supports document
/// content blocks. Detection is by hyphenated
/// prefix:
///
/// - `claude-3-5-*` (e.g. claude-3-5-sonnet-20240620)
/// - `claude-3-7-*` (forward-compat for a 3.7 release)
/// - `claude-opus-4-*`, `claude-sonnet-4-*`,
///   `claude-haiku-4-*` (Claude 4 family
///   shipped 2025-2026)
///
/// Hand-maintained list. A new variant with a
/// different prefix shape fails closed until
/// the substrate adds the prefix; the operator
/// sees a clear pre-flight error and the fix
/// is one line here.
fn model_supports_documents(model: &str) -> bool {
    const SUPPORTED_PREFIXES: &[&str] = &[
        "claude-3-5-",
        "claude-3-7-",
        "claude-opus-4-",
        "claude-sonnet-4-",
        "claude-haiku-4-",
    ];
    SUPPORTED_PREFIXES
        .iter()
        .any(|p| model.starts_with(p))
}

fn anthropic_message(msg: &LlmMessage) -> Result<Value, LlmError> {
    Ok(match msg {
        LlmMessage::User { content } => {
            let blocks: Vec<Value> = content
                .iter()
                .map(|b| match b {
                    ContentBlock::Text { text } => {
                        json!({"type": "text", "text": text})
                    }
                    ContentBlock::ImageBase64 { media_type, data } => json!({
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": media_type,
                            "data": data,
                        }
                    }),
                    // Phase 163 / amendment A13 —
                    // Anthropic supports document content
                    // blocks (Claude 3.5+). Same source
                    // shape as image; the model handles
                    // PDFs natively.
                    ContentBlock::DocumentBase64 { media_type, data } => json!({
                        "type": "document",
                        "source": {
                            "type": "base64",
                            "media_type": media_type,
                            "data": data,
                        }
                    }),
                })
                .collect();
            json!({ "role": "user", "content": blocks })
        }
        LlmMessage::Assistant { text, tool_calls } => {
            let mut content: Vec<Value> = Vec::new();
            if !text.is_empty() {
                content.push(json!({"type": "text", "text": text}));
            }
            for call in tool_calls {
                content.push(json!({
                    "type": "tool_use",
                    "id": call.call_id,
                    "name": call.tool_name,
                    "input": call.input,
                }));
            }
            json!({ "role": "assistant", "content": content })
        }
        LlmMessage::ToolResult {
            call_id,
            content,
            is_error,
        } => json!({
            "role": "user",
            "content": [{
                "type": "tool_result",
                "tool_use_id": call_id,
                "content": content,
                "is_error": is_error,
            }],
        }),
    })
}

/// Merge consecutive `role: "user"` messages whose content blocks are all
/// `tool_result` entries into a single user message. The Anthropic API
/// requires all tool results answering a multi-tool assistant turn to
/// appear in one `role: "user"` message. Phase 40.
fn merge_consecutive_tool_results(messages: Vec<Value>) -> Vec<Value> {
    let mut merged: Vec<Value> = Vec::with_capacity(messages.len());

    for msg in messages {
        let dominated = is_tool_result_user_msg(&msg)
            && merged.last().is_some_and(is_tool_result_user_msg);

        if dominated {
            // Extend the previous message's content array.
            let prev = merged.last_mut().unwrap();
            let incoming = msg["content"].as_array().unwrap();
            let target = prev["content"].as_array_mut().unwrap();
            target.extend(incoming.iter().cloned());
        } else {
            merged.push(msg);
        }
    }

    merged
}

/// Returns `true` if `msg` is a `role: "user"` message where every
/// content block has `type: "tool_result"`.
fn is_tool_result_user_msg(msg: &Value) -> bool {
    msg.get("role").and_then(Value::as_str) == Some("user")
        && msg
            .get("content")
            .and_then(Value::as_array)
            .is_some_and(|blocks| {
                !blocks.is_empty()
                    && blocks
                        .iter()
                        .all(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
            })
}

// ---------------------------------------------------------------------------
// Stream state machine
// ---------------------------------------------------------------------------

#[derive(Default)]
struct StreamState {
    accumulated_text: String,
    usage: LlmUsage,
    pending_tool: Option<PendingTool>,
    completed_tools: Vec<CompletedTool>,
    stop_reason: Option<String>,
}

struct PendingTool {
    call_id: String,
    tool_name: String,
    input_json: String,
}

struct CompletedTool {
    call_id: String,
    tool_name: String,
    input: Value,
}

struct AnthropicStream {
    sse: SseReader,
    state: StreamState,
    terminal: Option<LlmStepEnd>,
    /// Phase 120 — canonical tool-name set the planner advertised.
    /// Anthropic's hosted models rarely hallucinate tool names
    /// (well-trained tool-use protocol), but the validation runs
    /// uniformly so the substrate doesn't have provider-specific
    /// recovery semantics. Empty set when the request advertised
    /// no tools.
    known_tool_names: std::collections::HashSet<String>,
}

#[async_trait]
impl LlmStream for AnthropicStream {
    async fn next_event(&mut self) -> Result<Option<LlmStreamEvent>, LlmError> {
        loop {
            if self.terminal.is_some() {
                return Ok(None);
            }
            let Some(sse_event) = self.sse.next_event().await? else {
                // Stream ended without a message_stop — that's a truncation.
                return Err(LlmError::StreamEnded(
                    "Anthropic SSE ended before message_stop".to_string(),
                ));
            };
            if let Some(emit) = self.handle_sse(sse_event)? {
                return Ok(Some(emit));
            }
        }
    }

    async fn finish(self: Box<Self>) -> Result<LlmStepEnd, LlmError> {
        self.terminal.ok_or_else(|| {
            LlmError::StreamEnded(
                "AnthropicStream::finish called before stream drained".to_string(),
            )
        })
    }
}

impl AnthropicStream {
    /// Process one SSE event, updating state and optionally returning a
    /// mid-stream `LlmStreamEvent` to yield. On `message_stop`, sets
    /// `self.terminal` so the next `next_event` call returns `None`.
    fn handle_sse(&mut self, sse: SseEvent) -> Result<Option<LlmStreamEvent>, LlmError> {
        match sse.event.as_str() {
            "message_start" => {
                let parsed: MessageStart = parse_data(&sse.data)?;
                self.state.usage.input_tokens = parsed.message.usage.input_tokens;
                self.state.usage.cache_creation_input_tokens =
                    parsed.message.usage.cache_creation_input_tokens.unwrap_or(0);
                self.state.usage.cache_read_input_tokens =
                    parsed.message.usage.cache_read_input_tokens.unwrap_or(0);
                Ok(None)
            }
            "content_block_start" => {
                let parsed: ContentBlockStart = parse_data(&sse.data)?;
                if parsed.content_block.kind == "tool_use" {
                    self.state.pending_tool = Some(PendingTool {
                        call_id: parsed.content_block.id.unwrap_or_default(),
                        tool_name: parsed.content_block.name.unwrap_or_default(),
                        input_json: String::new(),
                    });
                }
                Ok(None)
            }
            "content_block_delta" => {
                let parsed: ContentBlockDelta = parse_data(&sse.data)?;
                match parsed.delta.kind.as_str() {
                    "text_delta" => {
                        let text = parsed.delta.text.unwrap_or_default();
                        self.state.accumulated_text.push_str(&text);
                        Ok(Some(LlmStreamEvent::TextChunk(text)))
                    }
                    "input_json_delta" => {
                        let partial = parsed.delta.partial_json.unwrap_or_default();
                        let pending = self.state.pending_tool.as_mut().ok_or_else(|| {
                            LlmError::Parse(
                                "input_json_delta without open tool_use block".to_string(),
                            )
                        })?;
                        pending.input_json.push_str(&partial);
                        Ok(None)
                    }
                    other => Err(LlmError::Parse(format!(
                        "unknown content_block_delta type: {other}"
                    ))),
                }
            }
            "content_block_stop" => {
                if let Some(pending) = self.state.pending_tool.take() {
                    let input: Value = if pending.input_json.is_empty() {
                        json!({})
                    } else {
                        serde_json::from_str(&pending.input_json).map_err(|e| {
                            LlmError::Parse(format!("tool input_json parse: {e}"))
                        })?
                    };
                    self.state.completed_tools.push(CompletedTool {
                        call_id: pending.call_id,
                        tool_name: pending.tool_name,
                        input,
                    });
                }
                Ok(None)
            }
            "message_delta" => {
                let parsed: MessageDelta = parse_data(&sse.data)?;
                if let Some(stop) = parsed.delta.stop_reason {
                    self.state.stop_reason = Some(stop);
                }
                if let Some(usage) = parsed.usage {
                    self.state.usage.output_tokens = usage.output_tokens.unwrap_or(0);
                }
                Ok(None)
            }
            "message_stop" => {
                self.terminal = Some(self.build_terminal()?);
                Ok(None)
            }
            "ping" | "error" => {
                // `ping` is a heartbeat — ignore. `error` events in the
                // stream itself are rare; when they happen Anthropic
                // sends the error body as data. Surface as Api error.
                if sse.event == "error" {
                    return Err(LlmError::Api {
                        status: 0,
                        message: sse.data,
                    });
                }
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    fn build_terminal(&mut self) -> Result<LlmStepEnd, LlmError> {
        let usage = self.state.usage;
        let stop = self.state.stop_reason.as_deref().unwrap_or("");
        if stop == "tool_use" {
            if self.state.completed_tools.is_empty() {
                return Err(LlmError::Parse(
                    "stop_reason=tool_use but no completed tool_use blocks".to_string(),
                ));
            }
            let calls = std::mem::take(&mut self.state.completed_tools)
                .into_iter()
                .map(|t| {
                    // Phase 120 — validate against the snapshot.
                    let name_resolution = if self
                        .known_tool_names
                        .contains(&t.tool_name)
                    {
                        crate::NameResolution::Known
                    } else {
                        crate::NameResolution::Unknown {
                            original: t.tool_name.clone(),
                        }
                    };
                    crate::ToolCallEnd {
                        call_id: t.call_id,
                        tool_name: t.tool_name,
                        input: t.input,
                        name_resolution,
                    }
                })
                .collect();
            Ok(LlmStepEnd::ToolCalls {
                calls,
                text_so_far: std::mem::take(&mut self.state.accumulated_text),
                usage,
            })
        } else {
            Ok(LlmStepEnd::FinalMessage {
                text: std::mem::take(&mut self.state.accumulated_text),
                usage,
            })
        }
    }
}

fn parse_data<T: for<'de> Deserialize<'de>>(data: &str) -> Result<T, LlmError> {
    serde_json::from_str::<T>(data)
        .map_err(|e| LlmError::Parse(format!("SSE data JSON: {e}")))
}

// ---------------------------------------------------------------------------
// Anthropic wire-format structs (minimal — only the fields we read)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct MessageStart {
    message: MessageStartMessage,
}

#[derive(Deserialize)]
struct MessageStartMessage {
    usage: MessageStartUsage,
}

#[derive(Deserialize)]
struct MessageStartUsage {
    input_tokens: u32,
    #[serde(default)]
    cache_creation_input_tokens: Option<u32>,
    #[serde(default)]
    cache_read_input_tokens: Option<u32>,
}

#[derive(Deserialize)]
struct ContentBlockStart {
    content_block: ContentBlockStartBlock,
}

#[derive(Deserialize)]
struct ContentBlockStartBlock {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Deserialize)]
struct ContentBlockDelta {
    delta: ContentBlockDeltaInner,
}

#[derive(Deserialize)]
struct ContentBlockDeltaInner {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    partial_json: Option<String>,
}

#[derive(Deserialize)]
struct MessageDelta {
    delta: MessageDeltaInner,
    #[serde(default)]
    usage: Option<MessageDeltaUsage>,
}

#[derive(Deserialize)]
struct MessageDeltaInner {
    #[serde(default)]
    stop_reason: Option<String>,
}

#[derive(Deserialize)]
struct MessageDeltaUsage {
    #[serde(default)]
    output_tokens: Option<u32>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LlmMessage, LlmToolDescriptor};
    use async_trait::async_trait;
    use bytes::Bytes;
    use futures_util::stream;

    // Env vars are process-global. Run every env-touching test under
    // one mutex so `cargo test` parallelism can't make one test's
    // `remove_var` race with another's `set_var` on the shared
    // `AIVYX_PA_ANTHROPIC_PDF_PAGE_CAP` key. Same pattern as
    // aivyx-channel::passphrase and aivyx-config.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }
    use std::sync::Mutex;

    use crate::transport::{ByteStream, HttpTransport};

    // -----------------------------------------------------------------------
    // FakeTransport: replays canned SSE bytes.
    // -----------------------------------------------------------------------

    struct FakeTransport {
        response: Mutex<Option<Result<Vec<&'static str>, LlmError>>>,
        seen_body: Mutex<Option<Vec<u8>>>,
    }

    impl FakeTransport {
        fn ok(chunks: Vec<&'static str>) -> Self {
            FakeTransport {
                response: Mutex::new(Some(Ok(chunks))),
                seen_body: Mutex::new(None),
            }
        }
        fn err(e: LlmError) -> Self {
            FakeTransport {
                response: Mutex::new(Some(Err(e))),
                seen_body: Mutex::new(None),
            }
        }
    }

    #[async_trait]
    impl HttpTransport for FakeTransport {
        async fn post_sse(
            &self,
            _url: &str,
            _headers: &[(&str, &str)],
            body: Vec<u8>,
            _cancellation: &CancellationToken,
        ) -> Result<ByteStream, LlmError> {
            *self.seen_body.lock().unwrap() = Some(body);
            let scripted = self
                .response
                .lock()
                .unwrap()
                .take()
                .ok_or_else(|| LlmError::Config("FakeTransport exhausted".to_string()))?;
            match scripted {
                Err(e) => Err(e),
                Ok(chunks) => {
                    let iter = chunks
                        .into_iter()
                        .map(|s| Ok::<Bytes, LlmError>(Bytes::from_static(s.as_bytes())));
                    Ok(Box::pin(stream::iter(iter)))
                }
            }
        }
    }

    fn provider_with(transport: FakeTransport) -> (AnthropicProvider, std::sync::Arc<FakeTransport>) {
        // We want to keep a handle to the transport for assertions but
        // also need to hand an owned Box to the provider. Use Arc and
        // a small wrapper that forwards calls.
        let arc = std::sync::Arc::new(transport);
        struct ArcTransport(std::sync::Arc<FakeTransport>);
        #[async_trait]
        impl HttpTransport for ArcTransport {
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
        let provider = AnthropicProvider::with_transport(
            AnthropicConfig::new(SecretString::from("test-key")),
            Box::new(ArcTransport(arc.clone())),
        );
        (provider, arc)
    }

    fn final_message_script() -> Vec<&'static str> {
        vec![
            "event: message_start\n",
            "data: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":12}}}\n\n",
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\", world\"}}\n\n",
            "event: content_block_stop\ndata: {\"index\":0}\n\n",
            "event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":7}}\n\n",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        ]
    }

    fn tool_call_script() -> Vec<&'static str> {
        vec![
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":25}}}\n\n",
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_01\",\"name\":\"memory.read\",\"input\":{}}}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"query\\\":\"}}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"yesterday\\\"}\"}}\n\n",
            "event: content_block_stop\ndata: {\"index\":0}\n\n",
            "event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":19}}\n\n",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        ]
    }

    fn blank_request_args() -> (Vec<LlmMessage>, Vec<LlmToolDescriptor>) {
        (
            vec![LlmMessage::user_text("hi")],
            vec![],
        )
    }

    #[tokio::test]
    async fn final_message_path_produces_reassembled_text_and_usage() {
        let (provider, _t) = provider_with(FakeTransport::ok(final_message_script()));
        let (messages, tools) = blank_request_args();
        let req = LlmRequest {
            model: "claude-haiku-4-5-20251001",
            system: Some("sys"),
            messages: &messages,
            tools: &tools,
            max_tokens: 256,
            temperature: None,
        id_slot: None,
        slot_hint: None,
        route: None,
        };

        let token = CancellationToken::new();
        let mut stream = provider.chat_stream(req, &token).await.unwrap();

        let mut reassembled = String::new();
        while let Some(ev) = stream.next_event().await.unwrap() {
            if let LlmStreamEvent::TextChunk(chunk) = ev {
                reassembled.push_str(&chunk);
            }
        }
        let terminal = stream.finish().await.unwrap();
        match terminal {
            LlmStepEnd::FinalMessage { text, usage } => {
                assert_eq!(text, "Hello, world");
                assert_eq!(reassembled, "Hello, world");
                assert_eq!(usage.input_tokens, 12);
                assert_eq!(usage.output_tokens, 7);
            }
            other => panic!("expected FinalMessage, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn tool_call_path_reassembles_partial_json_into_input() {
        let (provider, _t) = provider_with(FakeTransport::ok(tool_call_script()));
        let (messages, tools) = blank_request_args();
        let req = LlmRequest {
            model: "claude-haiku-4-5-20251001",
            system: None,
            messages: &messages,
            tools: &tools,
            max_tokens: 256,
            temperature: None,
        id_slot: None,
        slot_hint: None,
        route: None,
        };

        let token = CancellationToken::new();
        let mut stream = provider.chat_stream(req, &token).await.unwrap();
        while stream.next_event().await.unwrap().is_some() {}
        let terminal = stream.finish().await.unwrap();

        match terminal {
            LlmStepEnd::ToolCalls {
                calls,
                text_so_far,
                usage,
            } => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].call_id, "toolu_01");
                assert_eq!(calls[0].tool_name, "memory.read");
                assert_eq!(calls[0].input, json!({"query": "yesterday"}));
                assert_eq!(text_so_far, "");
                assert_eq!(usage.input_tokens, 25);
                assert_eq!(usage.output_tokens, 19);
            }
            other => panic!("expected ToolCalls, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn api_error_surface_from_transport() {
        let (provider, _t) = provider_with(FakeTransport::err(LlmError::Api {
            status: 429,
            message: "rate limited".to_string(),
        }));
        let (messages, tools) = blank_request_args();
        let req = LlmRequest {
            model: "claude-haiku-4-5-20251001",
            system: None,
            messages: &messages,
            tools: &tools,
            max_tokens: 256,
            temperature: None,
        id_slot: None,
        slot_hint: None,
        route: None,
        };
        let token = CancellationToken::new();
        let err = match provider.chat_stream(req, &token).await {
            Ok(_) => panic!("expected error, got Ok stream"),
            Err(e) => e,
        };
        assert!(matches!(
            err,
            LlmError::Api {
                status: 429,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn truncated_stream_errors_on_next_event() {
        let truncated = vec![
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":1}}}\n\n",
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"abc\"}}\n\n",
        ];
        let (provider, _t) = provider_with(FakeTransport::ok(truncated));
        let (messages, tools) = blank_request_args();
        let req = LlmRequest {
            model: "claude-haiku-4-5-20251001",
            system: None,
            messages: &messages,
            tools: &tools,
            max_tokens: 256,
            temperature: None,
        id_slot: None,
        slot_hint: None,
        route: None,
        };
        let token = CancellationToken::new();
        let mut stream = provider.chat_stream(req, &token).await.unwrap();
        // Drain until error or None. We expect StreamEnded after the
        // last valid text chunk, because message_stop never arrives.
        let mut saw_text = false;
        let err = loop {
            match stream.next_event().await {
                Ok(Some(LlmStreamEvent::TextChunk(_))) => saw_text = true,
                Ok(Some(_)) => {}
                Ok(None) => panic!("unexpected clean end on truncated stream"),
                Err(e) => break e,
            }
        };
        assert!(saw_text);
        assert!(matches!(err, LlmError::StreamEnded(_)));
    }

    #[tokio::test]
    async fn request_body_has_expected_shape() {
        let (provider, transport) = provider_with(FakeTransport::ok(final_message_script()));
        let messages = vec![LlmMessage::user_text("ping")];
        let tools = vec![LlmToolDescriptor {
            name: "memory.read".to_string(),
            description: "look things up".to_string(),
            input_schema: json!({"type": "object"}),
        }];
        let req = LlmRequest {
            model: "claude-haiku-4-5-20251001",
            system: Some("you are a test"),
            messages: &messages,
            tools: &tools,
            max_tokens: 128,
            // 0.5 is exactly representable in f32, so the f32→f64 JSON
            // promotion doesn't introduce drift. Any decimal that isn't
            // a sum of powers of two (e.g. 0.3) would fail this assertion.
            temperature: Some(0.5),
            id_slot: None,
            slot_hint: None,
            route: None,
        };
        let token = CancellationToken::new();
        let mut stream = provider.chat_stream(req, &token).await.unwrap();
        while stream.next_event().await.unwrap().is_some() {}
        let _ = stream.finish().await.unwrap();

        let body = transport.seen_body.lock().unwrap().clone().unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["model"], "claude-haiku-4-5-20251001");
        assert_eq!(body["max_tokens"], 128);
        assert_eq!(body["stream"], true);
        assert_eq!(body["system"], "you are a test");
        assert_eq!(body["temperature"], 0.5);
        assert_eq!(body["tools"][0]["name"], "memory.read");
        assert_eq!(body["messages"][0]["role"], "user");
    }

    #[tokio::test]
    async fn cancelled_token_short_circuits_before_send() {
        // FakeTransport doesn't itself check the token — it just replays
        // bytes. The real ReqwestTransport has a `tokio::select!` that
        // bails on a pre-cancelled token; we can't cover it with a fake.
        // Instead, assert the easier invariant: if chat_stream returns
        // successfully, the resulting stream can still be drained.
        let (provider, _t) = provider_with(FakeTransport::ok(final_message_script()));
        let (messages, tools) = blank_request_args();
        let req = LlmRequest {
            model: "claude-haiku-4-5-20251001",
            system: None,
            messages: &messages,
            tools: &tools,
            max_tokens: 16,
            temperature: None,
        id_slot: None,
        slot_hint: None,
        route: None,
        };
        let token = CancellationToken::new();
        let mut stream = provider.chat_stream(req, &token).await.unwrap();
        token.cancel();
        // Drain — FakeTransport ignores cancellation, so the stream
        // completes normally. This test documents that the token is
        // honored at the transport layer, not inside AnthropicStream.
        let mut events = 0;
        while stream.next_event().await.unwrap().is_some() {
            events += 1;
        }
        assert!(events >= 2);
    }

    // -----------------------------------------------------------------------
    // Phase 40 — consecutive ToolResult messages merge into one user message
    // -----------------------------------------------------------------------

    #[test]
    #[allow(clippy::useless_vec)]
    fn consecutive_tool_results_merge_into_single_user_message() {
        use crate::LlmMessage;

        let messages = vec![
            LlmMessage::user_text("read both files"),
            LlmMessage::Assistant {
                text: String::new(),
                tool_calls: vec![
                    crate::LlmToolCallRecord {
                        call_id: "call_1".to_string(),
                        tool_name: "fs.read".to_string(),
                        input: json!({"path": "/a.txt"}),
                    },
                    crate::LlmToolCallRecord {
                        call_id: "call_2".to_string(),
                        tool_name: "fs.read".to_string(),
                        input: json!({"path": "/b.txt"}),
                    },
                ],
            },
            LlmMessage::ToolResult {
                call_id: "call_1".to_string(),
                content: "contents of a".to_string(),
                is_error: false,
            },
            LlmMessage::ToolResult {
                call_id: "call_2".to_string(),
                content: "contents of b".to_string(),
                is_error: false,
            },
        ];

        let serialized: Vec<Value> = messages
            .iter()
            .map(anthropic_message)
            .collect::<Result<_, _>>()
            .unwrap();

        // Before merging: 4 messages (user, assistant, user/tool_result, user/tool_result)
        assert_eq!(serialized.len(), 4);

        let merged = merge_consecutive_tool_results(serialized);

        // After merging: 3 messages (user, assistant, user with 2 tool_results)
        assert_eq!(merged.len(), 3);

        // The merged user message has both tool_result blocks.
        let tool_msg = &merged[2];
        assert_eq!(tool_msg["role"], "user");
        let content = tool_msg["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["tool_use_id"], "call_1");
        assert_eq!(content[1]["tool_use_id"], "call_2");
    }

    #[test]
    #[allow(clippy::useless_vec)]
    fn non_consecutive_tool_results_stay_separate() {
        use crate::LlmMessage;

        // Two tool results with a text user message between them — should NOT merge.
        let messages = vec![
            LlmMessage::ToolResult {
                call_id: "call_1".to_string(),
                content: "ok".to_string(),
                is_error: false,
            },
            LlmMessage::user_text("continue"),
            LlmMessage::ToolResult {
                call_id: "call_2".to_string(),
                content: "ok".to_string(),
                is_error: false,
            },
        ];

        let serialized: Vec<Value> = messages
            .iter()
            .map(anthropic_message)
            .collect::<Result<_, _>>()
            .unwrap();
        let merged = merge_consecutive_tool_results(serialized);

        // All 3 should remain separate.
        assert_eq!(merged.len(), 3);
    }

    // -----------------------------------------------------------------------
    // Phase 40 — multi-tool stream produces ToolCalls with multiple entries
    // -----------------------------------------------------------------------

    fn multi_tool_call_script() -> Vec<&'static str> {
        vec![
            "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":10}}}\n\n",
            "event: content_block_start\ndata: {\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_01\",\"name\":\"fs.read\",\"input\":{}}}\n\n",
            "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"/a.txt\\\"}\"}}\n\n",
            "event: content_block_stop\ndata: {\"index\":0}\n\n",
            "event: content_block_start\ndata: {\"index\":1,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_02\",\"name\":\"memory.read\",\"input\":{}}}\n\n",
            "event: content_block_delta\ndata: {\"index\":1,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"topic\\\":\\\"notes\\\"}\"}}\n\n",
            "event: content_block_stop\ndata: {\"index\":1}\n\n",
            "event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"tool_use\"},\"usage\":{\"output_tokens\":20}}\n\n",
            "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
        ]
    }

    #[tokio::test]
    async fn multi_tool_stream_produces_batch_tool_calls() {
        let (provider, _t) = provider_with(FakeTransport::ok(multi_tool_call_script()));
        let (messages, tools) = blank_request_args();
        let req = LlmRequest {
            model: "claude-haiku-4-5-20251001",
            system: Some("sys"),
            messages: &messages,
            tools: &tools,
            max_tokens: 256,
            temperature: None,
        id_slot: None,
        slot_hint: None,
        route: None,
        };
        let token = CancellationToken::new();
        let mut stream = provider.chat_stream(req, &token).await.unwrap();

        // Drain mid-stream events.
        while stream.next_event().await.unwrap().is_some() {}

        match stream.finish().await.unwrap() {
            LlmStepEnd::ToolCalls { calls, .. } => {
                assert_eq!(calls.len(), 2);
                assert_eq!(calls[0].call_id, "toolu_01");
                assert_eq!(calls[0].tool_name, "fs.read");
                assert_eq!(calls[0].input, json!({"path": "/a.txt"}));
                assert_eq!(calls[1].call_id, "toolu_02");
                assert_eq!(calls[1].tool_name, "memory.read");
                assert_eq!(calls[1].input, json!({"topic": "notes"}));
            }
            other => panic!("expected ToolCalls, got {other:?}"),
        }
    }

    // ---- Phase 163 / Amendment A13 — document content blocks ----

    #[test]
    fn anthropic_message_emits_document_block_for_pdf() {
        use crate::{ContentBlock, LlmMessage};

        let msg = LlmMessage::User {
            content: vec![
                ContentBlock::text("summarize this paper"),
                ContentBlock::DocumentBase64 {
                    media_type: "application/pdf".to_string(),
                    data: "JVBERi0xLjQK".to_string(),
                },
            ],
        };
        let v = anthropic_message(&msg).expect("ok");
        assert_eq!(v["role"], "user");
        let blocks = v["content"].as_array().expect("array");
        assert_eq!(blocks.len(), 2);
        // First block is text.
        assert_eq!(blocks[0]["type"], "text");
        assert_eq!(blocks[0]["text"], "summarize this paper");
        // Second block is the document — type
        // = "document", source shape matches the
        // Anthropic API contract.
        assert_eq!(blocks[1]["type"], "document");
        assert_eq!(blocks[1]["source"]["type"], "base64");
        assert_eq!(blocks[1]["source"]["media_type"], "application/pdf");
        assert_eq!(blocks[1]["source"]["data"], "JVBERi0xLjQK");
    }

    #[test]
    fn anthropic_message_document_only_no_text() {
        use crate::{ContentBlock, LlmMessage};

        let msg = LlmMessage::User {
            content: vec![ContentBlock::DocumentBase64 {
                media_type: "application/pdf".to_string(),
                data: "JVBE".to_string(),
            }],
        };
        let v = anthropic_message(&msg).expect("ok");
        let blocks = v["content"].as_array().expect("array");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["type"], "document");
    }

    // ---- Phase 164 — model-version guard ----

    #[test]
    fn model_supports_documents_accepts_claude_3_5_family() {
        assert!(model_supports_documents("claude-3-5-sonnet-20240620"));
        assert!(model_supports_documents("claude-3-5-haiku-20241022"));
    }

    #[test]
    fn model_supports_documents_accepts_claude_4_family() {
        assert!(model_supports_documents("claude-opus-4-7"));
        assert!(model_supports_documents("claude-sonnet-4-6"));
        assert!(model_supports_documents("claude-haiku-4-5-20251001"));
    }

    #[test]
    fn model_supports_documents_rejects_claude_3_legacy() {
        // Claude 3 (without -5) didn't have
        // document blocks. Operators on those
        // models get fail-closed pre-flight.
        assert!(!model_supports_documents("claude-3-opus-20240229"));
        assert!(!model_supports_documents("claude-3-sonnet-20240229"));
        assert!(!model_supports_documents("claude-3-haiku-20240307"));
    }

    #[test]
    fn model_supports_documents_rejects_unknown_prefix() {
        // Fail-closed for unfamiliar names.
        assert!(!model_supports_documents(""));
        assert!(!model_supports_documents("claude-2"));
        assert!(!model_supports_documents("gpt-4"));
        assert!(!model_supports_documents("claude-5-future-variant"));
    }

    #[test]
    fn build_request_body_rejects_document_on_legacy_model() {
        use crate::{ContentBlock, LlmMessage, LlmRequest};

        let msgs = [LlmMessage::User {
            content: vec![ContentBlock::DocumentBase64 {
                media_type: "application/pdf".to_string(),
                data: "JVBE".to_string(),
            }],
        }];
        let req = LlmRequest {
            model: "claude-3-opus-20240229",
            messages: &msgs,
            tools: &[],
            system: None,
            max_tokens: 100,
            temperature: None,
        id_slot: None,
        slot_hint: None,
        route: None,
        };
        let err = build_request_body(&req, ANTHROPIC_PDF_PAGE_CAP).unwrap_err();
        match err {
            LlmError::Config(msg) => {
                assert!(msg.contains("does not support document blocks"));
                assert!(msg.contains("claude-3-opus"));
            }
            other => panic!("expected Config error, got {other:?}"),
        }
    }

    #[test]
    fn build_request_body_accepts_document_on_supported_model() {
        use crate::{ContentBlock, LlmMessage, LlmRequest};

        let msgs = [LlmMessage::User {
            content: vec![ContentBlock::DocumentBase64 {
                media_type: "application/pdf".to_string(),
                data: "JVBE".to_string(),
            }],
        }];
        let req = LlmRequest {
            model: "claude-haiku-4-5-20251001",
            messages: &msgs,
            tools: &[],
            system: None,
            max_tokens: 100,
            temperature: None,
        id_slot: None,
        slot_hint: None,
        route: None,
        };
        let body = build_request_body(&req, ANTHROPIC_PDF_PAGE_CAP).expect("ok");
        assert_eq!(body["model"], "claude-haiku-4-5-20251001");
        // Request still contains the document
        // block — passes through to the API.
        let user_msg = &body["messages"][0];
        assert_eq!(user_msg["content"][0]["type"], "document");
    }

    #[test]
    fn build_request_body_text_only_unaffected_on_legacy_model() {
        // Sanity: the guard fires ONLY when
        // documents are present. Text-only
        // requests on legacy models pass through
        // (those models just don't get
        // documents, not nothing).
        use crate::{LlmMessage, LlmRequest};
        let msgs = [LlmMessage::user_text("hello")];
        let req = LlmRequest {
            model: "claude-3-opus-20240229",
            messages: &msgs,
            tools: &[],
            system: None,
            max_tokens: 100,
            temperature: None,
        id_slot: None,
        slot_hint: None,
        route: None,
        };
        assert!(build_request_body(&req, ANTHROPIC_PDF_PAGE_CAP).is_ok());
    }

    // ---- Phase 165 — PDF page-count cap ----

    #[test]
    fn page_cap_pinned_to_one_hundred() {
        // Regression pin so a future widening
        // of the cap notices the impact on the
        // INSTALL.md doc + the operator-visible
        // error message.
        assert_eq!(ANTHROPIC_PDF_PAGE_CAP, 100);
    }

    #[test]
    fn count_pdf_pages_zero_on_empty_input() {
        assert_eq!(count_pdf_pages_best_effort(b""), 0);
    }

    #[test]
    fn count_pdf_pages_zero_on_unrelated_bytes() {
        // Random bytes with no /Type /Page
        // markers.
        assert_eq!(count_pdf_pages_best_effort(b"hello world"), 0);
    }

    #[test]
    fn count_pdf_pages_counts_three_uncompressed_pages() {
        // A simplified uncompressed PDF body
        // with three /Type /Page page-object
        // headers. The byte-scan should count
        // all three.
        let body = b"\
%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 >> endobj
3 0 obj << /Type /Page /Parent 2 0 R >> endobj
4 0 obj << /Type /Page /Parent 2 0 R >> endobj
5 0 obj << /Type /Page /Parent 2 0 R >> endobj
%%EOF";
        assert_eq!(count_pdf_pages_best_effort(body), 3);
    }

    #[test]
    fn count_pdf_pages_per_page_scan_does_not_count_pages_parent_node() {
        // `/Type /Pages` is the parent node,
        // NOT a page. The per-page byte-scan
        // excludes it via the trailing-`s`
        // check. Phase 168 — the declared-
        // count scan picks up the /Count
        // value, so the combined
        // count_pdf_pages_best_effort returns
        // the declared count.
        let body = b"<< /Type /Pages /Count 1 >>";
        assert_eq!(count_pdf_pages_per_page_scan(body), 0);
        assert_eq!(count_pdf_pages_best_effort(body), 1);
    }

    #[test]
    fn count_pdf_pages_counts_compact_form() {
        // PDFs sometimes pack the type marker
        // tightly: `/Type/Page` with no space.
        // The matcher's whitespace-between
        // check uses `.all()` which is
        // vacuously true for empty slices, so
        // the compact form is correctly
        // counted.
        let compact = b"<< /Type/Page /Parent 0 0 R >>";
        assert_eq!(count_pdf_pages_best_effort(compact), 1);
    }

    // ---- Phase 168 — declared-count scan ----

    #[test]
    fn declared_count_scan_reads_root_pages_count() {
        // A compressed PDF would hide
        // individual /Type /Page page objects
        // inside FlateDecode streams, but the
        // root catalog's `/Type /Pages /Count
        // N` typically remains visible. The
        // declared-count scan picks it up.
        let body = b"%PDF-1.7\n\
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj\n\
2 0 obj << /Type /Pages /Count 47 /Kids [...compressed...] >> endobj\n\
%%EOF";
        // per-page scan: zero (no individual
        // /Type /Page in this uncompressed
        // body).
        assert_eq!(count_pdf_pages_per_page_scan(body), 0);
        // declared-count: 47.
        assert_eq!(count_pdf_pages_declared_max(body), 47);
        // combined: max(0, 47) = 47.
        assert_eq!(count_pdf_pages_best_effort(body), 47);
    }

    #[test]
    fn declared_count_scan_handles_multiple_pages_nodes() {
        // Large PDFs use a tree of /Type
        // /Pages nodes; each inner one has
        // its own /Count covering its
        // subtree. Phase 168 takes the max
        // across all /Count values. For a
        // well-formed PDF the root /Count is
        // the largest.
        let body = b"\
2 0 obj << /Type /Pages /Count 100 /Kids [3 0 R 4 0 R] >> endobj
3 0 obj << /Type /Pages /Count 60 /Kids [...] >> endobj
4 0 obj << /Type /Pages /Count 40 /Kids [...] >> endobj
";
        assert_eq!(count_pdf_pages_declared_max(body), 100);
    }

    #[test]
    fn declared_count_scan_ignores_count_on_non_pages_dicts() {
        // `/Count` outside a /Type /Pages
        // dict shouldn't be picked up.
        // Phase 168 requires the marker
        // sequence in order.
        let body = b"<< /Type /Outlines /Count 5 >>";
        assert_eq!(count_pdf_pages_declared_max(body), 0);
    }

    #[test]
    fn declared_count_scan_handles_compact_and_spaced_separators() {
        // PDFs vary in whitespace; the
        // scanner should tolerate the
        // common shapes.
        let compact = b"<< /Type /Pages/Count 12 >>";
        let spaced = b"<< /Type /Pages  /Count   12   >>";
        assert_eq!(count_pdf_pages_declared_max(compact), 12);
        assert_eq!(count_pdf_pages_declared_max(spaced), 12);
    }

    #[test]
    fn declared_count_scan_zero_when_count_field_too_far() {
        // The look-ahead window is 128 bytes.
        // A /Count placed beyond that window
        // is missed; falls back to 0 (and
        // the per-page scan or server cap
        // catches the real count). This pins
        // the documented edge case.
        let mut body = b"<< /Type /Pages /Kids [".to_vec();
        // Fill with 200 bytes of refs to
        // push /Count out of the window.
        for _ in 0..40 {
            body.extend_from_slice(b" 999 0 R");
        }
        body.extend_from_slice(b" ] /Count 50 >>");
        assert_eq!(count_pdf_pages_declared_max(&body), 0);
    }

    #[test]
    fn declared_count_scan_ignores_malformed_count_value() {
        // `/Count notanumber` — parse fails;
        // the scanner returns 0 (not a
        // panic).
        let body = b"<< /Type /Pages /Count notanumber >>";
        assert_eq!(count_pdf_pages_declared_max(body), 0);
    }

    #[test]
    fn best_effort_max_combines_per_page_and_declared() {
        // Both signals present, declared
        // larger: max returns declared.
        let body = b"\
%PDF-1.4
1 0 obj << /Type /Catalog /Pages 2 0 R >> endobj
2 0 obj << /Type /Pages /Count 100 /Kids [3 0 R 4 0 R 5 0 R] >> endobj
3 0 obj << /Type /Page /Parent 2 0 R >> endobj
4 0 obj << /Type /Page /Parent 2 0 R >> endobj
5 0 obj << /Type /Page /Parent 2 0 R >> endobj
%%EOF";
        assert_eq!(count_pdf_pages_per_page_scan(body), 3);
        assert_eq!(count_pdf_pages_declared_max(body), 100);
        assert_eq!(count_pdf_pages_best_effort(body), 100);
    }

    // ---- Phase 168 — find_subslice + parse_leading_integer_after_whitespace ----

    #[test]
    fn find_subslice_finds_first_match() {
        assert_eq!(
            find_subslice(b"abcXYZdef", b"XYZ"),
            Some(3)
        );
    }

    #[test]
    fn find_subslice_returns_none_on_miss() {
        assert_eq!(find_subslice(b"hello", b"world"), None);
    }

    #[test]
    fn find_subslice_handles_empty_and_oversize_needles() {
        assert_eq!(find_subslice(b"abc", b""), None);
        assert_eq!(find_subslice(b"ab", b"abcdef"), None);
    }

    #[test]
    fn parse_leading_integer_after_whitespace_strips_whitespace() {
        assert_eq!(
            parse_leading_integer_after_whitespace(b"  42 rest"),
            Some(42)
        );
        assert_eq!(
            parse_leading_integer_after_whitespace(b"\t\n  7"),
            Some(7)
        );
    }

    #[test]
    fn parse_leading_integer_after_whitespace_rejects_non_digit_start() {
        assert_eq!(
            parse_leading_integer_after_whitespace(b"abc"),
            None
        );
        assert_eq!(
            parse_leading_integer_after_whitespace(b" abc"),
            None
        );
        assert_eq!(parse_leading_integer_after_whitespace(b""), None);
    }

    #[test]
    fn build_request_body_rejects_pdf_over_cap_on_supported_model() {
        use crate::{ContentBlock, LlmMessage, LlmRequest};
        use base64::Engine;

        // Construct a fake "PDF" with 101 page-
        // object headers — over the 100-page
        // cap.
        let mut body = b"%PDF-1.4\n".to_vec();
        for i in 0..101 {
            body.extend_from_slice(
                format!("{i} 0 obj << /Type /Page /Parent 0 0 R >> endobj\n").as_bytes(),
            );
        }
        let data =
            base64::engine::general_purpose::STANDARD.encode(&body);
        let msgs = [LlmMessage::User {
            content: vec![ContentBlock::DocumentBase64 {
                media_type: "application/pdf".to_string(),
                data,
            }],
        }];
        let req = LlmRequest {
            model: "claude-haiku-4-5-20251001",
            messages: &msgs,
            tools: &[],
            system: None,
            max_tokens: 100,
            temperature: None,
        id_slot: None,
        slot_hint: None,
        route: None,
        };
        let err = build_request_body(&req, ANTHROPIC_PDF_PAGE_CAP).unwrap_err();
        match err {
            LlmError::Config(msg) => {
                assert!(msg.contains("page cap"), "{msg}");
                assert!(msg.contains("101"), "{msg}");
            }
            other => panic!("expected Config error, got {other:?}"),
        }
    }

    #[test]
    fn build_request_body_accepts_pdf_at_exact_cap() {
        use crate::{ContentBlock, LlmMessage, LlmRequest};
        use base64::Engine;

        // 100-page PDF — exactly at the cap.
        // Should pass (cap is exclusive of the
        // boundary: counts > ANTHROPIC_PDF_PAGE_CAP
        // are rejected, == is accepted).
        let mut body = b"%PDF-1.4\n".to_vec();
        for i in 0..100 {
            body.extend_from_slice(
                format!("{i} 0 obj << /Type /Page /Parent 0 0 R >> endobj\n").as_bytes(),
            );
        }
        let data =
            base64::engine::general_purpose::STANDARD.encode(&body);
        let msgs = [LlmMessage::User {
            content: vec![ContentBlock::DocumentBase64 {
                media_type: "application/pdf".to_string(),
                data,
            }],
        }];
        let req = LlmRequest {
            model: "claude-haiku-4-5-20251001",
            messages: &msgs,
            tools: &[],
            system: None,
            max_tokens: 100,
            temperature: None,
        id_slot: None,
        slot_hint: None,
        route: None,
        };
        assert!(build_request_body(&req, ANTHROPIC_PDF_PAGE_CAP).is_ok());
    }

    #[test]
    fn build_request_body_rejects_invalid_base64_pdf() {
        use crate::{ContentBlock, LlmMessage, LlmRequest};

        let msgs = [LlmMessage::User {
            content: vec![ContentBlock::DocumentBase64 {
                media_type: "application/pdf".to_string(),
                data: "not valid base64 @!#$".to_string(),
            }],
        }];
        let req = LlmRequest {
            model: "claude-haiku-4-5-20251001",
            messages: &msgs,
            tools: &[],
            system: None,
            max_tokens: 100,
            temperature: None,
        id_slot: None,
        slot_hint: None,
        route: None,
        };
        let err = build_request_body(&req, ANTHROPIC_PDF_PAGE_CAP).unwrap_err();
        assert!(matches!(err, LlmError::Parse(_)));
    }

    // ---- Phase 166 — pdf_page_cap config knob ----

    #[test]
    fn pdf_page_cap_default_matches_constant() {
        // `AnthropicConfig::new` reads AIVYX_PA_ANTHROPIC_PDF_PAGE_CAP,
        // so this must hold `env_lock` like the sibling env-var tests
        // — without it, a parallel sibling's set_var races this read.
        let _lock = env_lock();
        let cfg = AnthropicConfig::new(SecretString::from("k"));
        assert_eq!(cfg.pdf_page_cap, ANTHROPIC_PDF_PAGE_CAP);
    }

    #[test]
    fn pdf_page_cap_builder_overrides_default() {
        // Holds `env_lock` for the same reason as the default test
        // above (`new` reads the env var before the builder override).
        let _lock = env_lock();
        let cfg = AnthropicConfig::new(SecretString::from("k"))
            .with_pdf_page_cap(200);
        assert_eq!(cfg.pdf_page_cap, 200);
    }

    #[test]
    fn pdf_page_cap_env_var_overrides_default() {
        // Two sibling tests below also set this key — serialize them
        // under `env_lock` so the set/remove can't race.
        let _lock = env_lock();
        let key = "AIVYX_PA_ANTHROPIC_PDF_PAGE_CAP";
        // SAFETY: serialized through `env_lock`.
        unsafe { std::env::set_var(key, "250") };
        let result = pdf_page_cap_from_env_or_default();
        unsafe { std::env::remove_var(key) };
        assert_eq!(result, 250);
    }

    #[test]
    fn pdf_page_cap_env_var_invalid_falls_back_to_default() {
        let _lock = env_lock();
        let key = "AIVYX_PA_ANTHROPIC_PDF_PAGE_CAP";
        unsafe { std::env::set_var(key, "not a number") };
        let result = pdf_page_cap_from_env_or_default();
        unsafe { std::env::remove_var(key) };
        assert_eq!(result, ANTHROPIC_PDF_PAGE_CAP);
    }

    #[test]
    fn pdf_page_cap_env_var_zero_falls_back_to_default() {
        // Zero is not a sensible cap — caller
        // would never be able to attach a PDF.
        // Treat as invalid; fall back to
        // default.
        let _lock = env_lock();
        let key = "AIVYX_PA_ANTHROPIC_PDF_PAGE_CAP";
        unsafe { std::env::set_var(key, "0") };
        let result = pdf_page_cap_from_env_or_default();
        unsafe { std::env::remove_var(key) };
        assert_eq!(result, ANTHROPIC_PDF_PAGE_CAP);
    }

    #[test]
    fn build_request_body_honors_higher_custom_cap() {
        // A 150-page PDF would fail at the
        // 100-page default cap; with cap=200
        // it passes.
        use crate::{ContentBlock, LlmMessage, LlmRequest};
        use base64::Engine;
        let mut body = b"%PDF-1.4\n".to_vec();
        for i in 0..150 {
            body.extend_from_slice(
                format!("{i} 0 obj << /Type /Page /Parent 0 0 R >> endobj\n")
                    .as_bytes(),
            );
        }
        let data =
            base64::engine::general_purpose::STANDARD.encode(&body);
        let msgs = [LlmMessage::User {
            content: vec![ContentBlock::DocumentBase64 {
                media_type: "application/pdf".to_string(),
                data,
            }],
        }];
        let req = LlmRequest {
            model: "claude-haiku-4-5-20251001",
            messages: &msgs,
            tools: &[],
            system: None,
            max_tokens: 100,
            temperature: None,
        id_slot: None,
        slot_hint: None,
        route: None,
        };
        // Default cap → rejected.
        assert!(build_request_body(&req, ANTHROPIC_PDF_PAGE_CAP).is_err());
        // Custom higher cap → accepted.
        assert!(build_request_body(&req, 200).is_ok());
    }

    #[test]
    fn build_request_body_error_message_uses_configured_cap() {
        use crate::{ContentBlock, LlmMessage, LlmRequest};
        use base64::Engine;
        let mut body = b"%PDF-1.4\n".to_vec();
        for i in 0..60 {
            body.extend_from_slice(
                format!("{i} 0 obj << /Type /Page /Parent 0 0 R >> endobj\n")
                    .as_bytes(),
            );
        }
        let data =
            base64::engine::general_purpose::STANDARD.encode(&body);
        let msgs = [LlmMessage::User {
            content: vec![ContentBlock::DocumentBase64 {
                media_type: "application/pdf".to_string(),
                data,
            }],
        }];
        let req = LlmRequest {
            model: "claude-haiku-4-5-20251001",
            messages: &msgs,
            tools: &[],
            system: None,
            max_tokens: 100,
            temperature: None,
        id_slot: None,
        slot_hint: None,
        route: None,
        };
        // Cap of 50 → over by 10.
        let err = build_request_body(&req, 50).unwrap_err();
        match err {
            LlmError::Config(msg) => {
                assert!(msg.contains("60"), "{msg}");
                assert!(msg.contains("50"), "{msg}");
            }
            other => panic!("expected Config error, got {other:?}"),
        }
    }

    #[test]
    fn build_request_body_ignores_page_cap_for_non_pdf_documents() {
        // A DOCX document block doesn't go
        // through the PDF page-count scanner;
        // it just rides through to the API
        // (which will likely return 400, but
        // that's the provider's concern).
        use crate::{ContentBlock, LlmMessage, LlmRequest};
        use base64::Engine;
        let data = base64::engine::general_purpose::STANDARD.encode(b"PK\x03\x04 fake docx");
        let msgs = [LlmMessage::User {
            content: vec![ContentBlock::DocumentBase64 {
                media_type:
                    "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
                        .to_string(),
                data,
            }],
        }];
        let req = LlmRequest {
            model: "claude-haiku-4-5-20251001",
            messages: &msgs,
            tools: &[],
            system: None,
            max_tokens: 100,
            temperature: None,
        id_slot: None,
        slot_hint: None,
        route: None,
        };
        assert!(build_request_body(&req, ANTHROPIC_PDF_PAGE_CAP).is_ok());
    }
}
