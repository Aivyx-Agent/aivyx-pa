//! The `OpenAiProvider`: concrete `LlmProvider` against the OpenAI
//! Chat Completions streaming API (`/v1/chat/completions`).
//!
//! ## Wire format
//!
//! A streaming POST returns SSE frames as `data: {json}` lines:
//!
//! 1. Each chunk has `choices[0].delta` containing either
//!    `content` (text) or `tool_calls` (function call deltas).
//! 2. `tool_calls` stream as incremental `function.arguments`
//!    strings that must be concatenated.
//! 3. `finish_reason` appears on the final choice: `"stop"` for
//!    text, `"tool_calls"` for tool invocation.
//! 4. `data: [DONE]` terminates the stream.
//! 5. Usage appears in the final chunk when
//!    `stream_options.include_usage` is set.

use async_trait::async_trait;
use futures_util::StreamExt;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use crate::{
    ContentBlock, LlmError, LlmMessage, LlmProvider, LlmRequest, LlmStepEnd, LlmStream,
    LlmStreamEvent, LlmUsage,
};

use crate::transport::{ByteStream, HttpTransport, ReqwestTransport};

const DEFAULT_BASE_URL: &str = "https://api.openai.com";
/// Default Ollama base URL — standard port for `ollama serve`.
pub const DEFAULT_OLLAMA_BASE_URL: &str = "http://localhost:11434";

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

pub struct OpenAiConfig {
    pub api_key: Option<SecretString>,
    pub base_url: Option<String>,
    /// When `true`, the `stream_options.include_usage` field is
    /// included in request bodies. Cloud OpenAI supports this;
    /// some Ollama versions may reject unknown fields. Default:
    /// `true`.
    pub include_stream_usage: bool,
    /// Chapter Emboss (EB.2) — grammar-constrained tool-calling for
    /// **llama.cpp-family** servers (`llama-server`, Jan). When `true`,
    /// tool-carrying turns constrain decoding to the
    /// [`tool_grammar::tool_call_grammar`] JSON-Schema (injected as the
    /// `json_schema` extension on the chat-completions body, which
    /// llama.cpp compiles to a GBNF grammar), so a small local GGUF
    /// emits a valid, real-named call by construction. Default `false`
    /// → the unchanged OpenAI-compat passthrough. Set only for the
    /// llama.cpp-backed providers; cloud OpenAI does not support the
    /// extension.
    ///
    /// [`tool_grammar::tool_call_grammar`]: crate::tool_grammar::tool_call_grammar
    pub constrain_tool_calls: bool,
}

impl OpenAiConfig {
    pub fn new(api_key: impl Into<SecretString>) -> Self {
        OpenAiConfig {
            api_key: Some(api_key.into()),
            base_url: None,
            include_stream_usage: true,
            constrain_tool_calls: false,
        }
    }

    /// Build a config with no API key. Used for local providers
    /// like Ollama that don't require authentication.
    pub fn without_api_key() -> Self {
        OpenAiConfig {
            api_key: None,
            base_url: None,
            include_stream_usage: false,
            constrain_tool_calls: false,
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = Some(base_url.into());
        self
    }

    pub fn with_include_stream_usage(mut self, include: bool) -> Self {
        self.include_stream_usage = include;
        self
    }

    /// Chapter Emboss (EB.2) — enable grammar-constrained tool-calling
    /// (llama.cpp-family servers only). Default off.
    pub fn with_constrain_tool_calls(mut self, on: bool) -> Self {
        self.constrain_tool_calls = on;
        self
    }
}

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

pub struct OpenAiProvider {
    config: OpenAiConfig,
    transport: Box<dyn HttpTransport>,
}

impl OpenAiProvider {
    pub fn new(config: OpenAiConfig) -> Result<Self, LlmError> {
        Ok(OpenAiProvider {
            config,
            transport: Box::new(ReqwestTransport::new()?),
        })
    }

    pub fn with_transport(
        config: OpenAiConfig,
        transport: Box<dyn HttpTransport>,
    ) -> Self {
        OpenAiProvider { config, transport }
    }

    fn endpoint(&self) -> String {
        let base = self
            .config
            .base_url
            .as_deref()
            .unwrap_or(DEFAULT_BASE_URL);
        format!("{base}/v1/chat/completions")
    }

    fn base_url(&self) -> &str {
        self.config
            .base_url
            .as_deref()
            .unwrap_or(DEFAULT_BASE_URL)
    }

    /// Lightweight health check against the provider's base URL.
    ///
    /// For Ollama, a GET to `http://localhost:11434` returns the
    /// plain-text body `"Ollama is running"`. This method checks
    /// reachability and returns a human-readable diagnostic:
    ///
    /// - `Ok(())` — the server responded (any 2xx).
    /// - `Err(msg)` — actionable error string suitable for display
    ///   to the user.
    pub async fn health_check(&self) -> Result<(), String> {
        let url = self.base_url();
        match self.transport.get_text(url).await {
            Ok(_body) => Ok(()),
            Err(LlmError::Transport(e)) => {
                // Connection refused, DNS failure, timeout, etc.
                Err(format!(
                    "cannot reach {url} — is Ollama running? \
                     Start it with `ollama serve`.\n  \
                     (transport error: {e})"
                ))
            }
            Err(LlmError::Api { status, message }) => {
                Err(format!(
                    "{url} returned HTTP {status}: {message}"
                ))
            }
            Err(other) => {
                Err(format!(
                    "health check against {url} failed: {other}"
                ))
            }
        }
    }
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    async fn chat_stream(
        &self,
        request: LlmRequest<'_>,
        cancellation: &CancellationToken,
    ) -> Result<Box<dyn LlmStream>, LlmError> {
        // Chapter Emboss (EB.3) — constrain only on tool-carrying turns
        // and only when the operator enabled it (llama.cpp-family).
        let constrain = self.config.constrain_tool_calls && !request.tools.is_empty();
        let body =
            build_request_body(&request, self.config.include_stream_usage, constrain)?;
        let body_bytes = serde_json::to_vec(&body)
            .map_err(|e| LlmError::Parse(format!("request serialization: {e}")))?;

        let mut headers: Vec<(&str, &str)> = vec![
            ("content-type", "application/json"),
        ];
        // Only add the Authorization header if an API key is
        // configured. Ollama ignores it, but omitting it avoids
        // sending "Bearer " with an empty secret.
        let auth_header;
        if let Some(ref api_key) = self.config.api_key {
            auth_header = format!("Bearer {}", api_key.expose_secret());
            headers.push(("authorization", auth_header.as_str()));
        }

        let endpoint = self.endpoint();
        let byte_stream = self
            .transport
            .post_sse(&endpoint, &headers, body_bytes, cancellation)
            .await?;

        // Phase 120 — snapshot the canonical tool-name set so the
        // terminal-build step can flag any pending tool-call whose
        // name doesn't match. Pure-function check; no auto-correct
        // here (the planner owns recovery semantics per Q2(c)).
        let known_tool_names: std::collections::HashSet<String> = request
            .tools
            .iter()
            .map(|t| t.name.to_string())
            .collect();

        Ok(Box::new(OpenAiStream {
            stream: byte_stream,
            buf: Vec::with_capacity(4096),
            exhausted: false,
            state: StreamState::default(),
            terminal: None,
            known_tool_names,
            constrain,
        }))
    }
}

// ---------------------------------------------------------------------------
// Request-body construction
// ---------------------------------------------------------------------------

fn build_request_body(
    request: &LlmRequest<'_>,
    include_stream_usage: bool,
    constrain: bool,
) -> Result<Value, LlmError> {
    if request.model.is_empty() {
        return Err(LlmError::UnknownModel(String::new()));
    }

    let mut messages: Vec<Value> = Vec::new();
    // Chapter Emboss (EB.3) — under grammar-constrained decoding, append
    // the `respond` preamble to the system message so the model knows the
    // sentinel is how it answers in plain text and ends the turn. Off →
    // the operator system prompt passes through untouched.
    let system = crate::tool_grammar::system_message_for(request.system, constrain);
    if let Some(system) = system {
        messages.push(json!({"role": "system", "content": system}));
    }
    for msg in request.messages {
        messages.push(openai_message(msg)?);
    }

    let mut body = json!({
        "model": request.model,
        "max_tokens": request.max_tokens,
        "messages": messages,
        "stream": true,
    });

    if include_stream_usage {
        body["stream_options"] = json!({"include_usage": true});
    }

    if constrain {
        // Chapter Emboss (EB.3) — constrain decoding to the tool-call
        // grammar via llama.cpp's `json_schema` extension (it compiles
        // the schema to a GBNF grammar server-side). The constrained
        // reply lands in message `content`, parsed at terminal — so we
        // deliberately do NOT send the native `tools` array (which would
        // make llama.cpp build its own competing grammar + route to
        // `tool_calls`). The grammar *is* the tool definition.
        body["json_schema"] = crate::tool_grammar::tool_call_grammar(request.tools);
    } else if !request.tools.is_empty() {
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

    if let Some(temp) = request.temperature {
        body["temperature"] = json!(temp);
    }

    if let Some(slot_id) = request.id_slot {
        body["id_slot"] = json!(slot_id);
    }

    if let Some(hint) = &request.slot_hint {
        body["aivyx_slot_hint"] = json!({
            "prefix_hash": hint.prefix_hash,
            "preferred_slot": hint.preferred_slot,
        });
    }

    Ok(body)
}

fn openai_message(msg: &LlmMessage) -> Result<Value, LlmError> {
    Ok(match msg {
        LlmMessage::User { content } => {
            let has_images = content.iter().any(ContentBlock::is_image);
            let has_documents = content.iter().any(ContentBlock::is_document);
            if has_documents {
                eprintln!(
                    "openai provider: dropping {} document block(s); \
                     OpenAI Chat Completions has no native document content \
                     block — use the Files API + assistants flow for PDFs",
                    content.iter().filter(|b| b.is_document()).count(),
                );
            }
            if has_images {
                // OpenAI multimodal: content array with text + image_url blocks.
                let blocks: Vec<Value> = content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text } => {
                            Some(json!({"type": "text", "text": text}))
                        }
                        ContentBlock::ImageBase64 { media_type, data } => {
                            Some(json!({
                                "type": "image_url",
                                "image_url": {
                                    "url": format!("data:{media_type};base64,{data}")
                                }
                            }))
                        }
                        // Phase 163 — skip document
                        // blocks for OpenAI (warned
                        // above).
                        ContentBlock::DocumentBase64 { .. } => None,
                    })
                    .collect();
                json!({ "role": "user", "content": blocks })
            } else {
                // Text-only: plain string for backwards compat.
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
            if !text.is_empty() {
                msg["content"] = Value::String(text.clone());
            }
            if !tool_calls.is_empty() {
                let calls: Vec<Value> = tool_calls
                    .iter()
                    .map(|c| {
                        json!({
                            "id": c.call_id,
                            "type": "function",
                            "function": {
                                "name": c.tool_name,
                                "arguments": c.input.to_string(),
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
// Stream state machine
// ---------------------------------------------------------------------------

#[derive(Default)]
struct StreamState {
    accumulated_text: String,
    usage: LlmUsage,
    pending_tools: Vec<PendingToolCall>,
    finish_reason: Option<String>,
}

#[derive(Default)]
struct PendingToolCall {
    call_id: String,
    tool_name: String,
    arguments: String,
}

struct OpenAiStream {
    stream: ByteStream,
    buf: Vec<u8>,
    exhausted: bool,
    state: StreamState,
    terminal: Option<LlmStepEnd>,
    /// Phase 120 — canonical tool-name set the planner advertised
    /// for this request. Stream-builder validates each pending
    /// `tool_name` against this at terminal-build time. Empty when
    /// the request advertised no tools (turn body was a plain chat).
    known_tool_names: std::collections::HashSet<String>,
    /// Chapter Emboss (EB.3) — grammar-constrained turn. When `true`,
    /// the reply is the constrained `{"name","arguments"}` JSON streamed
    /// as `content`; content deltas are buffered silently (not emitted
    /// as `TextChunk`, so the raw JSON never reaches the channel) and
    /// the accumulated text is parsed at terminal into a real tool call
    /// or an unwrapped `respond` reply.
    constrain: bool,
}

#[async_trait]
impl LlmStream for OpenAiStream {
    async fn next_event(&mut self) -> Result<Option<LlmStreamEvent>, LlmError> {
        loop {
            if self.terminal.is_some() {
                return Ok(None);
            }
            match self.next_data_line().await? {
                Some(line) if line == "[DONE]" => {
                    self.terminal = Some(self.build_terminal()?);
                    return Ok(None);
                }
                Some(line) => {
                    if let Some(emit) = self.handle_chunk(&line)? {
                        return Ok(Some(emit));
                    }
                }
                None => {
                    self.terminal = Some(self.build_terminal()?);
                    return Ok(None);
                }
            }
        }
    }

    async fn finish(self: Box<Self>) -> Result<LlmStepEnd, LlmError> {
        self.terminal.ok_or_else(|| {
            LlmError::StreamEnded(
                "OpenAiStream::finish called before stream drained".to_string(),
            )
        })
    }
}

impl OpenAiStream {
    async fn next_data_line(&mut self) -> Result<Option<String>, LlmError> {
        loop {
            if let Some(line) = try_extract_data_line(&mut self.buf) {
                return Ok(Some(line));
            }
            if self.exhausted {
                return Ok(None);
            }
            match self.stream.next().await {
                Some(Ok(chunk)) => self.buf.extend_from_slice(&chunk),
                Some(Err(e)) => return Err(e),
                None => self.exhausted = true,
            }
        }
    }

    fn handle_chunk(&mut self, data: &str) -> Result<Option<LlmStreamEvent>, LlmError> {
        let chunk: ChatChunk = serde_json::from_str(data)
            .map_err(|e| LlmError::Parse(format!("OpenAI chunk JSON: {e}")))?;

        if let Some(usage) = chunk.usage {
            self.state.usage.input_tokens = usage.prompt_tokens.unwrap_or(0);
            self.state.usage.output_tokens = usage.completion_tokens.unwrap_or(0);
        }

        let Some(choice) = chunk.choices.into_iter().next() else {
            return Ok(None);
        };

        if let Some(reason) = choice.finish_reason {
            self.state.finish_reason = Some(reason);
        }

        if let Some(content) = choice.delta.content {
            self.state.accumulated_text.push_str(&content);
            // Chapter Emboss (EB.3) — under constraint the content is the
            // tool-call JSON; buffer it silently and reclassify at
            // terminal rather than leaking raw JSON to the channel.
            if self.constrain {
                return Ok(None);
            }
            return Ok(Some(LlmStreamEvent::TextChunk(content)));
        }

        if let Some(tool_calls) = choice.delta.tool_calls {
            for tc in tool_calls {
                let idx = tc.index.unwrap_or(0) as usize;
                // Grow the Vec to accommodate this index.
                while self.state.pending_tools.len() <= idx {
                    self.state.pending_tools.push(PendingToolCall::default());
                }
                let pending = &mut self.state.pending_tools[idx];
                if let Some(id) = tc.id {
                    pending.call_id = id;
                }
                if let Some(func) = tc.function {
                    if let Some(name) = func.name {
                        pending.tool_name = name;
                    }
                    if let Some(args) = func.arguments {
                        pending.arguments.push_str(&args);
                    }
                }
            }
        }

        Ok(None)
    }

    fn build_terminal(&mut self) -> Result<LlmStepEnd, LlmError> {
        let usage = self.state.usage;
        let reason = self.state.finish_reason.as_deref().unwrap_or("stop");

        // Chapter Emboss (EB.3) — grammar-constrained turn: the reply is
        // the constrained JSON in `accumulated_text` (we omitted the
        // native `tools`, so `finish_reason` is never `tool_calls`).
        // Parse it: a real name → one tool call; the `respond` sentinel
        // → a plain-text final message. A parse miss (the grammar makes
        // it well-formed, so this is defensive) falls through to the
        // normal extraction below with the raw text.
        if self.constrain {
            let text = std::mem::take(&mut self.state.accumulated_text);
            match crate::tool_grammar::parse_constrained_output(&text) {
                Some(crate::tool_grammar::ConstrainedOutput::ToolCall {
                    tool_name,
                    input,
                }) => {
                    let name_resolution = if self.known_tool_names.contains(&tool_name) {
                        crate::NameResolution::Known
                    } else {
                        crate::NameResolution::Unknown {
                            original: tool_name.clone(),
                        }
                    };
                    return Ok(LlmStepEnd::ToolCalls {
                        calls: vec![crate::ToolCallEnd {
                            call_id: "llamacpp-constrained-call".to_string(),
                            tool_name,
                            input,
                            name_resolution,
                        }],
                        text_so_far: String::new(),
                        usage,
                    });
                }
                Some(crate::tool_grammar::ConstrainedOutput::Text(t)) => {
                    return Ok(LlmStepEnd::FinalMessage { text: t, usage });
                }
                None => {
                    return Ok(LlmStepEnd::FinalMessage { text, usage });
                }
            }
        }

        if reason == "tool_calls" {
            let mut calls = Vec::with_capacity(self.state.pending_tools.len());
            for pending in std::mem::take(&mut self.state.pending_tools) {
                let input: Value = if pending.arguments.is_empty() {
                    json!({})
                } else {
                    serde_json::from_str(&pending.arguments).map_err(|e| {
                        LlmError::Parse(format!("tool arguments JSON: {e}"))
                    })?
                };
                // Phase 120 — validate the model's emitted name
                // against the canonical set the request advertised.
                // No auto-correct here: the planner owns recovery.
                let name_resolution = if self
                    .known_tool_names
                    .contains(&pending.tool_name)
                {
                    crate::NameResolution::Known
                } else {
                    crate::NameResolution::Unknown {
                        original: pending.tool_name.clone(),
                    }
                };
                calls.push(crate::ToolCallEnd {
                    call_id: pending.call_id,
                    tool_name: pending.tool_name,
                    input,
                    name_resolution,
                });
            }
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

fn try_extract_data_line(buf: &mut Vec<u8>) -> Option<String> {
    let s = std::str::from_utf8(buf).ok()?;
    for (i, line) in s.split('\n').enumerate() {
        let trimmed = line.trim();
        if let Some(data) = trimmed.strip_prefix("data:") {
            let data = data.strip_prefix(' ').unwrap_or(data);
            let result = data.to_string();
            let consumed = s.split('\n')
                .take(i + 1)
                .map(|l| l.len() + 1)
                .sum::<usize>();
            buf.drain(..consumed.min(buf.len()));
            return Some(result);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// OpenAI wire-format structs (minimal — only the fields we read)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct ChatChunk {
    #[serde(default)]
    choices: Vec<ChunkChoice>,
    #[serde(default)]
    usage: Option<ChunkUsage>,
}

#[derive(Deserialize)]
struct ChunkChoice {
    delta: ChunkDelta,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct ChunkDelta {
    content: Option<String>,
    tool_calls: Option<Vec<ChunkToolCall>>,
}

#[derive(Deserialize)]
struct ChunkToolCall {
    index: Option<u32>,
    id: Option<String>,
    function: Option<ChunkFunction>,
}

#[derive(Deserialize)]
struct ChunkFunction {
    name: Option<String>,
    arguments: Option<String>,
}

#[derive(Deserialize)]
struct ChunkUsage {
    prompt_tokens: Option<u32>,
    completion_tokens: Option<u32>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use crate::transport::ByteStream;
    use crate::{LlmToolCallRecord, LlmToolDescriptor};
    use futures_util::stream;

    fn stream_from(chunks: Vec<&'static str>) -> ByteStream {
        let iter = chunks
            .into_iter()
            .map(|s| Ok::<Bytes, LlmError>(Bytes::from_static(s.as_bytes())));
        Box::pin(stream::iter(iter))
    }

    struct FakeTransport {
        sse_bytes: Vec<u8>,
    }

    impl FakeTransport {
        fn new(sse: &str) -> Self {
            FakeTransport {
                sse_bytes: sse.as_bytes().to_vec(),
            }
        }
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
            let chunk = Bytes::from(self.sse_bytes.clone());
            Ok(Box::pin(stream::once(async move { Ok(chunk) })))
        }
    }

    fn test_provider(sse: &str) -> OpenAiProvider {
        let config = OpenAiConfig::new("sk-test");
        OpenAiProvider::with_transport(config, Box::new(FakeTransport::new(sse)))
    }

    fn simple_request() -> (Vec<LlmMessage>, Vec<LlmToolDescriptor>) {
        (
            vec![LlmMessage::user_text("hello")],
            vec![],
        )
    }

    #[tokio::test]
    async fn text_response_streams_and_finishes() {
        let sse = "\
data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\" world\"},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":2}}\n\n\
data: [DONE]\n\n";

        let provider = test_provider(sse);
        let (msgs, tools) = simple_request();
        let req = LlmRequest {
            model: "gpt-4",
            system: None,
            messages: &msgs,
            tools: &tools,
            max_tokens: 1000,
            temperature: None,
            id_slot: None,
            slot_hint: None,
            route: None,
        };
        let cancel = CancellationToken::new();
        let mut stream = provider.chat_stream(req, &cancel).await.unwrap();

        let ev1 = stream.next_event().await.unwrap().unwrap();
        assert!(matches!(ev1, LlmStreamEvent::TextChunk(ref t) if t == "Hello"));

        let ev2 = stream.next_event().await.unwrap().unwrap();
        assert!(matches!(ev2, LlmStreamEvent::TextChunk(ref t) if t == " world"));

        assert!(stream.next_event().await.unwrap().is_none());

        let end = stream.finish().await.unwrap();
        match end {
            LlmStepEnd::FinalMessage { text, usage } => {
                assert_eq!(text, "Hello world");
                assert_eq!(usage.input_tokens, 10);
                assert_eq!(usage.output_tokens, 2);
            }
            _ => panic!("expected FinalMessage"),
        }
    }

    #[tokio::test]
    async fn tool_call_response_assembles_arguments() {
        let sse = "\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"id\":\"call_abc\",\"function\":{\"name\":\"get_weather\",\"arguments\":\"\"}}]},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"function\":{\"arguments\":\"{\\\"loc\"}}]},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"function\":{\"arguments\":\"ation\\\": \\\"SF\\\"}\"}}]},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":8}}\n\n\
data: [DONE]\n\n";

        let provider = test_provider(sse);
        let (msgs, tools) = simple_request();
        let req = LlmRequest {
            model: "gpt-4",
            system: None,
            messages: &msgs,
            tools: &tools,
            max_tokens: 1000,
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
            LlmStepEnd::ToolCalls {
                calls,
                usage,
                ..
            } => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].call_id, "call_abc");
                assert_eq!(calls[0].tool_name, "get_weather");
                assert_eq!(calls[0].input["location"], "SF");
                assert_eq!(usage.input_tokens, 5);
                assert_eq!(usage.output_tokens, 8);
                // Phase 120 — the request advertises NO tools (simple_request
                // returns an empty tools Vec), so the model's emitted name is
                // flagged Unknown. The planner's Phase 120 recovery path
                // takes over from here.
                match &calls[0].name_resolution {
                    crate::NameResolution::Unknown { original } => {
                        assert_eq!(original, "get_weather");
                    }
                    other => panic!(
                        "expected Unknown (no tools advertised), got {other:?}"
                    ),
                }
            }
            _ => panic!("expected ToolCalls"),
        }
    }

    // ----- Phase 120 — Provider-side validation of tool names -----

    fn request_with_tools<'a>(
        msgs: &'a [LlmMessage],
        tools: &'a [LlmToolDescriptor],
    ) -> LlmRequest<'a> {
        // Helper for the Phase 120 validation tests so each test
        // doesn't replicate the LlmRequest scaffolding.
        LlmRequest {
            model: "gpt-4",
            system: None,
            messages: msgs,
            tools,
            max_tokens: 1000,
            temperature: None,
            id_slot: None,
            slot_hint: None,
            route: None,
        }
    }

    fn tool_descriptor(name: &str) -> LlmToolDescriptor {
        LlmToolDescriptor {
            name: name.into(),
            description: format!("{name} description"),
            input_schema: json!({"type": "object"}),
        }
    }

    #[tokio::test]
    async fn name_resolution_known_when_request_advertises_the_tool() {
        // Model emits `fs.read` AND the request advertised `fs.read`
        // in tools[] → NameResolution::Known. Planner dispatches
        // directly, no recovery needed.
        let sse = "\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"id\":\"c1\",\"function\":{\"name\":\"fs.read\",\"arguments\":\"{}\"}}]},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n\
data: [DONE]\n\n";
        let provider = test_provider(sse);
        let msgs = vec![LlmMessage::user_text("read a file")];
        let tools = vec![tool_descriptor("fs.read")];
        let req = request_with_tools(&msgs, &tools);
        let cancel = CancellationToken::new();
        let mut stream = provider.chat_stream(req, &cancel).await.unwrap();
        while stream.next_event().await.unwrap().is_some() {}
        let end = stream.finish().await.unwrap();
        match end {
            LlmStepEnd::ToolCalls { calls, .. } => {
                assert_eq!(calls[0].tool_name, "fs.read");
                assert!(matches!(
                    calls[0].name_resolution,
                    crate::NameResolution::Known
                ));
            }
            _ => panic!("expected ToolCalls"),
        }
    }

    #[tokio::test]
    async fn name_resolution_unknown_when_model_hallucinates_separator() {
        // Phase 120's load-bearing case: local model (qwen3.6:27b
        // pattern) emits `fs_read` when the registered tool is
        // `fs.read`. Provider doesn't know about the underscore-vs-
        // dot equivalence — that's the planner's fuzzy-match job
        // at Task 4. Provider just flags Unknown with the verbatim
        // original name.
        let sse = "\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"id\":\"c1\",\"function\":{\"name\":\"fs_read\",\"arguments\":\"{}\"}}]},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n\
data: [DONE]\n\n";
        let provider = test_provider(sse);
        let msgs = vec![LlmMessage::user_text("read a file")];
        let tools = vec![tool_descriptor("fs.read")];
        let req = request_with_tools(&msgs, &tools);
        let cancel = CancellationToken::new();
        let mut stream = provider.chat_stream(req, &cancel).await.unwrap();
        while stream.next_event().await.unwrap().is_some() {}
        let end = stream.finish().await.unwrap();
        match end {
            LlmStepEnd::ToolCalls { calls, .. } => {
                assert_eq!(calls[0].tool_name, "fs_read");
                match &calls[0].name_resolution {
                    crate::NameResolution::Unknown { original } => {
                        assert_eq!(
                            original, "fs_read",
                            "original must be the verbatim emitted name"
                        );
                    }
                    other => panic!("expected Unknown, got {other:?}"),
                }
            }
            _ => panic!("expected ToolCalls"),
        }
    }

    #[tokio::test]
    async fn name_resolution_classifies_per_call_in_a_batch() {
        // A parallel-tool batch: one Known + one Unknown. Phase
        // 120 validation runs per-pending-call; the planner sees
        // both classifications and recovers only the Unknown one.
        let sse = "\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"fs.read\",\"arguments\":\"{}\"}}]},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":1,\"id\":\"c2\",\"function\":{\"name\":\"web_fetch\",\"arguments\":\"{}\"}}]},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n\
data: [DONE]\n\n";
        let provider = test_provider(sse);
        let msgs = vec![LlmMessage::user_text("read then fetch")];
        let tools = vec![
            tool_descriptor("fs.read"),
            tool_descriptor("web.fetch"),
        ];
        let req = request_with_tools(&msgs, &tools);
        let cancel = CancellationToken::new();
        let mut stream = provider.chat_stream(req, &cancel).await.unwrap();
        while stream.next_event().await.unwrap().is_some() {}
        let end = stream.finish().await.unwrap();
        match end {
            LlmStepEnd::ToolCalls { calls, .. } => {
                assert_eq!(calls.len(), 2);
                // First call: known.
                assert_eq!(calls[0].tool_name, "fs.read");
                assert!(matches!(
                    calls[0].name_resolution,
                    crate::NameResolution::Known
                ));
                // Second call: unknown (web_fetch vs registered web.fetch).
                assert_eq!(calls[1].tool_name, "web_fetch");
                assert!(matches!(
                    calls[1].name_resolution,
                    crate::NameResolution::Unknown { .. }
                ));
            }
            _ => panic!("expected ToolCalls"),
        }
    }

    #[tokio::test]
    async fn request_body_includes_system_as_message() {
        let (msgs, _) = simple_request();
        let req = LlmRequest {
            model: "gpt-4",
            system: Some("you are helpful"),
            messages: &msgs,
            tools: &[],
            max_tokens: 100,
            temperature: None,
            id_slot: None,
            slot_hint: None,
            route: None,
        };
        let body = build_request_body(&req, true, false).unwrap();
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "you are helpful");
        assert_eq!(messages[1]["role"], "user");
    }

    #[tokio::test]
    async fn request_body_includes_tools_as_functions() {
        let (msgs, _) = simple_request();
        let tools = vec![LlmToolDescriptor {
            name: "read_file".into(),
            description: "Read a file".into(),
            input_schema: json!({"type": "object", "properties": {"path": {"type": "string"}}}),
        }];
        let req = LlmRequest {
            model: "gpt-4",
            system: None,
            messages: &msgs,
            tools: &tools,
            max_tokens: 100,
            temperature: None,
            id_slot: None,
            slot_hint: None,
            route: None,
        };
        let body = build_request_body(&req, true, false).unwrap();
        let tool_arr = body["tools"].as_array().unwrap();
        assert_eq!(tool_arr.len(), 1);
        assert_eq!(tool_arr[0]["type"], "function");
        assert_eq!(tool_arr[0]["function"]["name"], "read_file");
    }

    // ---- Chapter Emboss (EB.3): grammar-constrained tool-calling ----

    /// A constrained body carries the `json_schema` grammar, omits the
    /// native `tools` array, and prepends the `respond` preamble to the
    /// system message.
    #[tokio::test]
    async fn constrained_body_uses_json_schema_and_omits_tools() {
        let (msgs, _) = simple_request();
        let tools = vec![LlmToolDescriptor {
            name: "fs.read".into(),
            description: "Read a file".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"],
            }),
        }];
        let req = LlmRequest {
            model: "qwen3",
            system: Some("You are Aivyx."),
            messages: &msgs,
            tools: &tools,
            max_tokens: 100,
            temperature: None,
            id_slot: None,
            slot_hint: None,
            route: None,
        };
        let body = build_request_body(&req, false, true).unwrap();

        // The grammar is present as `json_schema` (a oneOf union)…
        assert!(body.get("json_schema").is_some(), "json_schema injected");
        assert!(body["json_schema"].get("oneOf").is_some(), "grammar is the union");
        // …and the native `tools` array is omitted (the grammar IS the
        // tool definition).
        assert!(body.get("tools").is_none(), "native tools omitted under constraint");
        // The system message carries the operator prompt + the preamble.
        let sys = body["messages"][0]["content"].as_str().unwrap();
        assert!(sys.starts_with("You are Aivyx."));
        assert!(sys.contains(crate::tool_grammar::RESPOND_SENTINEL));
    }

    fn constrained_provider(sse: &str) -> OpenAiProvider {
        let config = OpenAiConfig::without_api_key().with_constrain_tool_calls(true);
        OpenAiProvider::with_transport(config, Box::new(FakeTransport::new(sse)))
    }

    fn fs_read_request_tools() -> Vec<LlmToolDescriptor> {
        vec![LlmToolDescriptor {
            name: "fs.read".into(),
            description: "Read a file".into(),
            input_schema: json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"],
            }),
        }]
    }

    /// Under constraint, the model emits the tool-call JSON as `content`
    /// (finish_reason "stop"); content deltas are suppressed (no leaked
    /// JSON) and the terminal reclassifies it into a real `fs.read` call.
    #[tokio::test]
    async fn constrained_stream_parses_content_json_into_tool_call() {
        // The constrained JSON, split across two content deltas to
        // exercise accumulation.
        let sse = "\
data: {\"choices\":[{\"delta\":{\"content\":\"{\\\"name\\\": \\\"fs.read\\\", \\\"argum\"},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"delta\":{\"content\":\"ents\\\": {\\\"path\\\": \\\"probe.txt\\\"}}\"},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":7,\"completion_tokens\":12}}\n\n\
data: [DONE]\n\n";
        let provider = constrained_provider(sse);
        let tools = fs_read_request_tools();
        let (msgs, _) = simple_request();
        let req = LlmRequest {
            model: "qwen3",
            system: None,
            messages: &msgs,
            tools: &tools,
            max_tokens: 1000,
            temperature: None,
            id_slot: None,
            slot_hint: None,
            route: None,
        };
        let cancel = CancellationToken::new();
        let mut stream = provider.chat_stream(req, &cancel).await.unwrap();

        // No TextChunk should leak — the constrained JSON is buffered
        // silently and reclassified at terminal.
        let mut leaked = 0;
        while stream.next_event().await.unwrap().is_some() {
            leaked += 1;
        }
        assert_eq!(leaked, 0, "constrained stream must not leak content events");
        let end = stream.finish().await.unwrap();
        match end {
            LlmStepEnd::ToolCalls { calls, usage, text_so_far } => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].tool_name, "fs.read");
                assert_eq!(calls[0].input["path"], "probe.txt");
                // fs.read was advertised → Known (no fuzzy recovery needed).
                assert!(matches!(calls[0].name_resolution, crate::NameResolution::Known));
                assert!(text_so_far.is_empty(), "no text leaks alongside the call");
                assert_eq!(usage.output_tokens, 12);
            }
            other => panic!("expected ToolCalls, got {other:?}"),
        }
    }

    /// Under constraint, a `respond` sentinel unwraps to a plain-text
    /// final message (the model's way of declining a tool).
    #[tokio::test]
    async fn constrained_stream_unwraps_respond_to_final_text() {
        let sse = "\
data: {\"choices\":[{\"delta\":{\"content\":\"{\\\"name\\\": \\\"respond\\\", \\\"arguments\\\": {\\\"text\\\": \\\"All done.\\\"}}\"},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
data: [DONE]\n\n";
        let provider = constrained_provider(sse);
        let tools = fs_read_request_tools();
        let (msgs, _) = simple_request();
        let req = LlmRequest {
            model: "qwen3",
            system: None,
            messages: &msgs,
            tools: &tools,
            max_tokens: 1000,
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
            LlmStepEnd::FinalMessage { text, .. } => assert_eq!(text, "All done."),
            other => panic!("expected FinalMessage, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn tool_result_message_maps_correctly() {
        let msg = LlmMessage::ToolResult {
            call_id: "call_xyz".into(),
            content: "file contents here".into(),
            is_error: false,
        };
        let val = openai_message(&msg).unwrap();
        assert_eq!(val["role"], "tool");
        assert_eq!(val["tool_call_id"], "call_xyz");
        assert_eq!(val["content"], "file contents here");
    }

    #[tokio::test]
    async fn assistant_with_tool_calls_round_trips() {
        let msg = LlmMessage::Assistant {
            text: "Let me check".into(),
            tool_calls: vec![LlmToolCallRecord {
                call_id: "call_1".into(),
                tool_name: "search".into(),
                input: json!({"q": "test"}),
            }],
        };
        let val = openai_message(&msg).unwrap();
        assert_eq!(val["role"], "assistant");
        assert_eq!(val["content"], "Let me check");
        let tcs = val["tool_calls"].as_array().unwrap();
        assert_eq!(tcs[0]["id"], "call_1");
        assert_eq!(tcs[0]["function"]["name"], "search");
    }

    // ---- Ollama / optional API key tests --------------------------------

    #[test]
    fn without_api_key_config_has_none_key_and_no_stream_usage() {
        let cfg = OpenAiConfig::without_api_key();
        assert!(cfg.api_key.is_none());
        assert!(!cfg.include_stream_usage);
    }

    #[test]
    fn constrain_tool_calls_defaults_off_and_builder_sets_it() {
        // Chapter Emboss (EB.2) — both constructors default the flag
        // off (byte-identical passthrough); the builder flips it.
        assert!(!OpenAiConfig::new("sk-test").constrain_tool_calls);
        assert!(!OpenAiConfig::without_api_key().constrain_tool_calls);
        assert!(
            OpenAiConfig::without_api_key()
                .with_constrain_tool_calls(true)
                .constrain_tool_calls
        );
    }

    #[test]
    fn with_api_key_config_has_some_key_and_stream_usage() {
        let cfg = OpenAiConfig::new("sk-test");
        assert!(cfg.api_key.is_some());
        assert!(cfg.include_stream_usage);
    }

    #[tokio::test]
    async fn request_body_omits_stream_options_when_disabled() {
        let (msgs, _) = simple_request();
        let req = LlmRequest {
            model: "llama3.1",
            system: None,
            messages: &msgs,
            tools: &[],
            max_tokens: 2048,
            temperature: None,
            id_slot: None,
            slot_hint: None,
            route: None,
        };
        let body = build_request_body(&req, false, false).unwrap();
        assert!(
            body.get("stream_options").is_none(),
            "stream_options must be absent when include_stream_usage is false: {body}"
        );
    }

    #[tokio::test]
    async fn request_body_includes_stream_options_when_enabled() {
        let (msgs, _) = simple_request();
        let req = LlmRequest {
            model: "gpt-4",
            system: None,
            messages: &msgs,
            tools: &[],
            max_tokens: 1000,
            temperature: None,
            id_slot: None,
            slot_hint: None,
            route: None,
        };
        let body = build_request_body(&req, true, false).unwrap();
        assert!(
            body.get("stream_options").is_some(),
            "stream_options must be present when include_stream_usage is true: {body}"
        );
        assert_eq!(body["stream_options"]["include_usage"], true);
    }

    #[test]
    fn default_ollama_base_url_is_localhost_11434() {
        assert_eq!(DEFAULT_OLLAMA_BASE_URL, "http://localhost:11434");
    }

    // ---- Health-check tests -----------------------------------------------

    struct HealthyTransport;

    #[async_trait]
    impl HttpTransport for HealthyTransport {
        async fn post_sse(
            &self,
            _url: &str,
            _headers: &[(&str, &str)],
            _body: Vec<u8>,
            _cancellation: &CancellationToken,
        ) -> Result<ByteStream, LlmError> {
            unreachable!("post_sse should not be called during health check");
        }

        async fn get_text(&self, _url: &str) -> Result<String, LlmError> {
            Ok("Ollama is running".to_string())
        }
    }

    struct UnreachableTransport;

    #[async_trait]
    impl HttpTransport for UnreachableTransport {
        async fn post_sse(
            &self,
            _url: &str,
            _headers: &[(&str, &str)],
            _body: Vec<u8>,
            _cancellation: &CancellationToken,
        ) -> Result<ByteStream, LlmError> {
            unreachable!();
        }

        async fn get_text(&self, _url: &str) -> Result<String, LlmError> {
            Err(LlmError::Transport(
                "connection refused".to_string(),
            ))
        }
    }

    #[tokio::test]
    async fn health_check_ok_when_server_responds() {
        let cfg = OpenAiConfig::without_api_key()
            .with_base_url("http://localhost:11434");
        let provider = OpenAiProvider::with_transport(
            cfg,
            Box::new(HealthyTransport),
        );
        assert!(provider.health_check().await.is_ok());
    }

    #[tokio::test]
    async fn health_check_returns_actionable_error_on_connection_refused() {
        let cfg = OpenAiConfig::without_api_key()
            .with_base_url("http://localhost:11434");
        let provider = OpenAiProvider::with_transport(
            cfg,
            Box::new(UnreachableTransport),
        );
        let err = provider.health_check().await.unwrap_err();
        assert!(
            err.contains("ollama serve"),
            "error should mention `ollama serve`: {err}"
        );
        assert!(
            err.contains("localhost:11434"),
            "error should mention the URL: {err}"
        );
    }

    #[test]
    fn ollama_config_endpoint_uses_ollama_base_url() {
        let cfg = OpenAiConfig::without_api_key()
            .with_base_url(DEFAULT_OLLAMA_BASE_URL);
        let provider = OpenAiProvider::with_transport(
            cfg,
            Box::new(FakeTransport::new("")),
        );
        assert_eq!(
            provider.endpoint(),
            "http://localhost:11434/v1/chat/completions"
        );
    }

    #[tokio::test]
    async fn ollama_style_text_response_without_usage() {
        // Ollama often omits usage fields entirely and may not
        // send stream_options. This test verifies the provider
        // handles a response that has no usage block gracefully.
        let sse = "\
data: {\"choices\":[{\"delta\":{\"content\":\"Hi\"},\"finish_reason\":null}]}\n\n\
data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
data: [DONE]\n\n";

        let cfg = OpenAiConfig::without_api_key()
            .with_base_url("http://localhost:11434");
        let provider = OpenAiProvider::with_transport(
            cfg,
            Box::new(FakeTransport::new(sse)),
        );
        let (msgs, tools) = simple_request();
        let req = LlmRequest {
            model: "llama3.1",
            system: None,
            messages: &msgs,
            tools: &tools,
            max_tokens: 2048,
            temperature: None,
            id_slot: None,
            slot_hint: None,
            route: None,
        };
        let cancel = CancellationToken::new();
        let mut stream = provider.chat_stream(req, &cancel).await.unwrap();

        let ev = stream.next_event().await.unwrap().unwrap();
        assert!(matches!(ev, LlmStreamEvent::TextChunk(ref t) if t == "Hi"));
        assert!(stream.next_event().await.unwrap().is_none());

        let end = stream.finish().await.unwrap();
        match end {
            LlmStepEnd::FinalMessage { text, usage } => {
                assert_eq!(text, "Hi");
                // Usage is zero when not provided — not an error.
                assert_eq!(usage.input_tokens, 0);
                assert_eq!(usage.output_tokens, 0);
            }
            _ => panic!("expected FinalMessage"),
        }
    }

    #[test]
    fn build_request_body_omits_id_slot_when_none() {
        let messages: Vec<LlmMessage> = vec![];
        let tools: Vec<LlmToolDescriptor> = vec![];
        let request = LlmRequest {
            model: "test-model",
            system: None,
            messages: &messages,
            tools: &tools,
            max_tokens: 100,
            temperature: None,
            id_slot: None,
            slot_hint: None,
            route: None,
        };
        let body = build_request_body(&request, true, false).unwrap();
        assert!(body.get("id_slot").is_none(), "id_slot must be omitted entirely when None");
    }

    #[test]
    fn build_request_body_includes_id_slot_when_set() {
        let messages: Vec<LlmMessage> = vec![];
        let tools: Vec<LlmToolDescriptor> = vec![];
        let request = LlmRequest {
            model: "test-model",
            system: None,
            messages: &messages,
            tools: &tools,
            max_tokens: 100,
            temperature: None,
            id_slot: Some(2),
            slot_hint: None,
            route: None,
        };
        let body = build_request_body(&request, true, false).unwrap();
        assert_eq!(body["id_slot"], serde_json::json!(2));
    }

    #[test]
    fn build_request_body_omits_aivyx_slot_hint_when_none() {
        let messages: Vec<LlmMessage> = vec![];
        let tools: Vec<LlmToolDescriptor> = vec![];
        let request = LlmRequest {
            model: "test-model",
            system: None,
            messages: &messages,
            tools: &tools,
            max_tokens: 100,
            temperature: None,
            id_slot: None,
            slot_hint: None,
            route: None,
        };
        let body = build_request_body(&request, true, false).unwrap();
        assert!(
            body.get("aivyx_slot_hint").is_none(),
            "aivyx_slot_hint must be omitted entirely when None -- a request without it \
             must behave exactly like a plain OpenAI-compatible call"
        );
    }

    #[test]
    fn build_request_body_includes_aivyx_slot_hint_when_set() {
        let messages: Vec<LlmMessage> = vec![];
        let tools: Vec<LlmToolDescriptor> = vec![];
        let request = LlmRequest {
            model: "test-model",
            system: None,
            messages: &messages,
            tools: &tools,
            max_tokens: 100,
            temperature: None,
            // Broker mode never sets `id_slot` directly (no local
            // `KvSlotPool` checkout) -- `slot_hint` carries the
            // equivalent information instead.
            id_slot: None,
            slot_hint: Some(crate::SlotHint {
                prefix_hash: "abc123".to_string(),
                preferred_slot: Some(2),
            }),
            route: None,
        };
        let body = build_request_body(&request, true, false).unwrap();
        assert_eq!(
            body["aivyx_slot_hint"],
            serde_json::json!({"prefix_hash": "abc123", "preferred_slot": 2}),
        );
        assert!(
            body.get("id_slot").is_none(),
            "id_slot must stay absent when only slot_hint is set"
        );
    }
}
