//! Phase 121 Task 4 — Stream state machine + chunk parser for
//! Ollama's `/api/chat` JSONL streaming protocol.
//!
//! Ollama's stream shape is meaningfully simpler than OpenAI's:
//!
//! ```text
//! { "message": { "role": "assistant", "content": "Hello" }, "done": false }
//! { "message": { "role": "assistant", "content": " world" }, "done": false }
//! { "message": { "role": "assistant", "content": "" },
//!   "done": true,
//!   "prompt_eval_count": 42,
//!   "eval_count": 31 }
//! ```
//!
//! For tool-call responses:
//!
//! ```text
//! { "message": { "role": "assistant", "content": "" }, "done": false }
//! { "message": { "role": "assistant", "content": "",
//!                "tool_calls": [
//!                  { "function": { "name": "fs.read",
//!                                  "arguments": { "path": "foo" } } }
//!                ] },
//!   "done": true,
//!   "prompt_eval_count": 65,
//!   "eval_count": 12 }
//! ```
//!
//! Two key Ollama-vs-OpenAI differences:
//!
//! 1. **Tool calls are NOT delta-streamed.** They arrive complete
//!    inside the final `done: true` chunk's `message.tool_calls`
//!    array (not incremental `function.arguments` strings as
//!    OpenAI emits). The state machine accumulates text deltas
//!    in chronological order; tool calls land in one shot.
//!
//! 2. **Usage on the final chunk**. Ollama reports
//!    `prompt_eval_count` (input tokens) and `eval_count` (output
//!    tokens) on the `done: true` chunk. There's no
//!    `stream_options.include_usage` knob to toggle this — the
//!    fields are always present in the terminal chunk.
//!
//! Phase 120 Task 3's `NameResolution` validation runs at
//! terminal-build time, same as the OpenAI and Anthropic
//! providers. The Phase 120 substrate (Phase 121's load-bearing
//! local-model rehab story) flows uniformly across providers.

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;

use crate::{
    LlmError, LlmStepEnd, LlmStream, LlmStreamEvent, LlmUsage, NameResolution,
    ToolCallEnd,
};

use super::jsonl::JsonlReader;

/// Internal state accumulated across chunks. Mirrors the
/// OpenAI provider's `StreamState` shape minus the
/// finish_reason handling (Ollama uses `done: true` as the
/// terminal signal, not a string field).
#[derive(Default)]
struct StreamState {
    accumulated_text: String,
    usage: LlmUsage,
    /// Phase 121 — Ollama delivers tool_calls all-at-once on the
    /// `done: true` chunk, so this is `Some` only after the
    /// terminal chunk has been parsed. Distinct from OpenAI's
    /// per-index PendingToolCall accumulation.
    pending_tool_calls: Option<Vec<ToolCallEnd>>,
    /// Set to `true` when a chunk with `done: true` lands. The
    /// state machine returns `LlmStreamEvent::Usage` for any
    /// chunks that arrive after this (defensive; should not
    /// happen with Ollama's protocol but the JSONL reader
    /// doesn't enforce the terminal contract).
    terminal_seen: bool,
    /// The terminal chunk's `done_reason`, when it sent one.
    done_reason: Option<String>,
}

/// The Ollama `LlmStream` impl. Wraps a [`JsonlReader`] and
/// classifies each chunk into `LlmStreamEvent::TextChunk` /
/// `LlmStreamEvent::Usage` / terminal accumulation.
pub struct OllamaStream {
    reader: JsonlReader,
    state: StreamState,
    terminal: Option<LlmStepEnd>,
    /// Phase 120 Task 3 — canonical tool-name set the planner
    /// advertised for this request. Same posture as the OpenAI
    /// and Anthropic providers: the stream-builder classifies
    /// each emitted tool_name as `NameResolution::Known` or
    /// `NameResolution::Unknown` at terminal-build time. Empty
    /// set when the request advertised no tools.
    known_tool_names: std::collections::HashSet<String>,
}

impl OllamaStream {
    /// Construct from a JsonlReader + the request's tool-name
    /// snapshot. Phase 121 Task 5 wires this from
    /// `OllamaProvider::chat_stream`; tests construct directly.
    pub fn new(
        reader: JsonlReader,
        known_tool_names: std::collections::HashSet<String>,
    ) -> Self {
        OllamaStream {
            reader,
            state: StreamState::default(),
            terminal: None,
            known_tool_names,
        }
    }
}

#[async_trait]
impl LlmStream for OllamaStream {
    async fn next_event(&mut self) -> Result<Option<LlmStreamEvent>, LlmError> {
        loop {
            if self.terminal.is_some() {
                return Ok(None);
            }
            match self.reader.next_line().await? {
                Some(line) => {
                    if let Some(emit) = self.handle_chunk(&line)? {
                        return Ok(Some(emit));
                    }
                    // If `done: true` was just seen, build the
                    // terminal and return None so the caller
                    // calls finish().
                    if self.state.terminal_seen {
                        self.terminal = Some(self.build_terminal()?);
                        let reason = self.state.done_reason.as_deref();
                        return Ok(crate::finish_event("ollama", reason, reason == Some("length")));
                    }
                }
                None => {
                    // Stream EOF with no `done: true` chunk —
                    // defensive: build the terminal from
                    // whatever we accumulated so the caller's
                    // finish() returns a well-shaped value
                    // rather than erroring.
                    self.terminal = Some(self.build_terminal()?);
                    return Ok(None);
                }
            }
        }
    }

    async fn finish(self: Box<Self>) -> Result<LlmStepEnd, LlmError> {
        self.terminal.ok_or_else(|| {
            LlmError::StreamEnded(
                "OllamaStream::finish called before stream drained"
                    .to_string(),
            )
        })
    }
}

// ---------------------------------------------------------------------------
// Chunk handling
// ---------------------------------------------------------------------------

impl OllamaStream {
    fn handle_chunk(
        &mut self,
        data: &str,
    ) -> Result<Option<LlmStreamEvent>, LlmError> {
        let chunk: OllamaChunk = serde_json::from_str(data)
            .map_err(|e| LlmError::Parse(format!("Ollama chunk JSON: {e}")))?;

        // Terminal signal — Ollama sets done: true on the last
        // chunk. Usage + tool_calls arrive on this chunk too;
        // accumulate them before returning.
        if chunk.done {
            self.state.terminal_seen = true;
            self.state.done_reason = chunk.done_reason.clone();
            if let Some(prompt) = chunk.prompt_eval_count {
                self.state.usage.input_tokens = prompt;
            }
            if let Some(eval) = chunk.eval_count {
                self.state.usage.output_tokens = eval;
            }
            if let Some(msg) = &chunk.message {
                if let Some(tcs) = &msg.tool_calls {
                    self.state.pending_tool_calls =
                        Some(parse_tool_calls(tcs, &self.known_tool_names)?);
                }
                // The terminal chunk's message.content is
                // usually empty (text streamed in earlier
                // chunks); but defensively append if present
                // so a non-streaming model still works.
                if let Some(content) = &msg.content {
                    if !content.is_empty() {
                        self.state.accumulated_text.push_str(content);
                    }
                }
            }
            return Ok(None);
        }

        // Non-terminal chunk — text delta, AND (newer Ollama /
        // qwen3) the tool_calls array. Early Ollama only emitted
        // tool_calls on the `done: true` chunk; current versions
        // (observed: Ollama 0.30.5 + qwen3.6) deliver the complete
        // tool_calls on a `done: false` chunk and leave the terminal
        // chunk's `tool_calls` null. Capture them here so the call
        // isn't silently dropped — otherwise a tool-using turn ends
        // with no tool execution and no text (the model spent its
        // whole response on a call we ignored). The terminal handler
        // only overwrites `pending_tool_calls` when its own chunk
        // carries a non-null array, so a later null `done` chunk
        // can't clear what we set here.
        if let Some(msg) = chunk.message {
            if let Some(tcs) = &msg.tool_calls {
                if !tcs.is_empty() {
                    self.state.pending_tool_calls =
                        Some(parse_tool_calls(tcs, &self.known_tool_names)?);
                }
            }
            if let Some(content) = msg.content {
                if !content.is_empty() {
                    self.state.accumulated_text.push_str(&content);
                    return Ok(Some(LlmStreamEvent::TextChunk(content)));
                }
            }
        }

        Ok(None)
    }

    fn build_terminal(&mut self) -> Result<LlmStepEnd, LlmError> {
        let usage = self.state.usage;
        if let Some(calls) = std::mem::take(&mut self.state.pending_tool_calls) {
            return Ok(LlmStepEnd::ToolCalls {
                calls,
                text_so_far: std::mem::take(&mut self.state.accumulated_text),
                usage,
            });
        }
        Ok(LlmStepEnd::FinalMessage {
            text: std::mem::take(&mut self.state.accumulated_text),
            usage,
        })
    }
}

/// Phase 121 Task 4 — translate Ollama's terminal-chunk
/// `tool_calls` array into the protocol's `Vec<ToolCallEnd>`.
/// Per-call validation against the request's `known_tool_names`
/// matches Phase 120 Task 3's posture across providers.
///
/// Ollama's tool-call shape:
///
/// ```json
/// { "function": { "name": "fs.read",
///                 "arguments": { "path": "foo" } } }
/// ```
///
/// There is no `id` field in Ollama's tool-call protocol —
/// Ollama's chat history correlates by position, not by ID. We
/// synthesize a stable call_id per call to keep the
/// `LlmMessage::ToolResult { call_id }` round-trip working;
/// `tc-{index}-{tool_name}` is unique within one
/// `LlmStepEnd::ToolCalls` batch.
fn parse_tool_calls(
    raw: &[OllamaToolCallRecord],
    known: &std::collections::HashSet<String>,
) -> Result<Vec<ToolCallEnd>, LlmError> {
    let mut out = Vec::with_capacity(raw.len());
    for (idx, tc) in raw.iter().enumerate() {
        let function = tc.function.as_ref().ok_or_else(|| {
            LlmError::Parse(
                "Ollama tool_call missing function field".to_string(),
            )
        })?;
        let tool_name = function.name.clone();
        // Ollama's arguments is already a JSON object (not a
        // string). Default to {} when absent so the planner sees
        // a well-formed input.
        let input = function.arguments.clone().unwrap_or(Value::Object(
            serde_json::Map::new(),
        ));
        let name_resolution = if known.contains(&tool_name) {
            NameResolution::Known
        } else {
            NameResolution::Unknown {
                original: tool_name.clone(),
            }
        };
        // Synthesize a stable id (Ollama doesn't supply one). The
        // call_id is operator-visible in the audit chain via
        // `LlmMessage::ToolResult { call_id }`; the format here
        // is `tc-<idx>-<name>` so a forensic walk can correlate
        // even without a server-supplied id.
        let call_id = tc
            .id
            .clone()
            .unwrap_or_else(|| format!("tc-{idx}-{tool_name}"));
        out.push(ToolCallEnd {
            call_id,
            tool_name,
            input,
            name_resolution,
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Wire-format types (deserialize-only)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct OllamaChunk {
    #[serde(default)]
    message: Option<OllamaMessage>,
    #[serde(default)]
    done: bool,
    #[serde(default)]
    prompt_eval_count: Option<u32>,
    #[serde(default)]
    eval_count: Option<u32>,
    /// On the `done: true` chunk: `"stop"`, `"length"` (the reply hit
    /// `num_predict` or the context window), `"load"`, ...
    #[serde(default)]
    done_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OllamaMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<OllamaToolCallRecord>>,
}

#[derive(Debug, Deserialize)]
struct OllamaToolCallRecord {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<OllamaToolCallFunction>,
}

#[derive(Debug, Deserialize)]
struct OllamaToolCallFunction {
    name: String,
    #[serde(default)]
    arguments: Option<Value>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::ByteStream;
    use bytes::Bytes;
    use futures_util::stream;
    use serde_json::json;
    use std::pin::Pin;

    fn make_reader(chunks: Vec<&[u8]>) -> JsonlReader {
        let items: Vec<Result<Bytes, LlmError>> = chunks
            .into_iter()
            .map(|c| Ok::<_, LlmError>(Bytes::copy_from_slice(c)))
            .collect();
        let stream: ByteStream = Pin::from(Box::new(stream::iter(items)))
            as Pin<Box<dyn futures_util::Stream<Item = _> + Send>>;
        JsonlReader::new(stream)
    }

    fn empty_known() -> std::collections::HashSet<String> {
        std::collections::HashSet::new()
    }

    fn known(names: &[&str]) -> std::collections::HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    // ----- Text-only streaming -----

    /// Task 5c item 11: `done_reason: "length"` on the terminal chunk (the
    /// reply hit `num_predict` or the context) yields `Truncated` before the
    /// stream ends; `"stop"` does not.
    #[tokio::test]
    async fn length_done_reason_yields_truncated() {
        let chunks: Vec<&[u8]> = vec![
            b"{\"message\":{\"role\":\"assistant\",\"content\":\"Half\"},\"done\":false}\n",
            b"{\"message\":{\"role\":\"assistant\",\"content\":\"\"},\"done\":true,\"done_reason\":\"length\",\"eval_count\":128}\n",
        ];
        let mut stream = OllamaStream::new(make_reader(chunks), empty_known());
        assert!(matches!(stream.next_event().await.unwrap(), Some(LlmStreamEvent::TextChunk(_))));
        assert_eq!(stream.next_event().await.unwrap(), Some(LlmStreamEvent::Truncated));
        assert!(stream.next_event().await.unwrap().is_none());
        match Box::new(stream).finish().await.unwrap() {
            LlmStepEnd::FinalMessage { text, .. } => assert_eq!(text, "Half"),
            other => panic!("expected FinalMessage, got {other:?}"),
        }

        let chunks: Vec<&[u8]> = vec![
            b"{\"message\":{\"role\":\"assistant\",\"content\":\"Done.\"},\"done\":true,\"done_reason\":\"stop\"}\n",
        ];
        let mut stream = OllamaStream::new(make_reader(chunks), empty_known());
        assert!(stream.next_event().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn streams_text_chunks_and_terminates_on_done() {
        // Three chunks: two text deltas + final done=true with
        // usage. The state machine emits two TextChunks then
        // None, and finish() returns FinalMessage with the
        // accumulated text and usage.
        let chunks: Vec<&[u8]> = vec![
            b"{\"message\":{\"role\":\"assistant\",\"content\":\"Hello\"},\"done\":false}\n",
            b"{\"message\":{\"role\":\"assistant\",\"content\":\" world\"},\"done\":false}\n",
            b"{\"message\":{\"role\":\"assistant\",\"content\":\"\"},\"done\":true,\"prompt_eval_count\":42,\"eval_count\":31}\n",
        ];
        let reader = make_reader(chunks);
        let mut stream = OllamaStream::new(reader, empty_known());

        match stream.next_event().await.unwrap() {
            Some(LlmStreamEvent::TextChunk(s)) => assert_eq!(s, "Hello"),
            other => panic!("expected TextChunk(Hello), got {other:?}"),
        }
        match stream.next_event().await.unwrap() {
            Some(LlmStreamEvent::TextChunk(s)) => assert_eq!(s, " world"),
            other => panic!("expected TextChunk( world), got {other:?}"),
        }
        // Terminal chunk drained internally; next_event yields None.
        assert!(stream.next_event().await.unwrap().is_none());

        let end = Box::new(stream).finish().await.unwrap();
        match end {
            LlmStepEnd::FinalMessage { text, usage } => {
                assert_eq!(text, "Hello world");
                assert_eq!(usage.input_tokens, 42);
                assert_eq!(usage.output_tokens, 31);
            }
            other => panic!("expected FinalMessage, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn handles_stream_eof_without_done_chunk_defensively() {
        // The JsonlReader's EOF without a `done: true` chunk is a
        // protocol violation in practice but we synthesize a
        // terminal rather than erroring (operator-conservative).
        let chunks: Vec<&[u8]> = vec![
            b"{\"message\":{\"role\":\"assistant\",\"content\":\"partial\"},\"done\":false}\n",
        ];
        let reader = make_reader(chunks);
        let mut stream = OllamaStream::new(reader, empty_known());
        let _ = stream.next_event().await.unwrap(); // TextChunk
        assert!(stream.next_event().await.unwrap().is_none());
        let end = Box::new(stream).finish().await.unwrap();
        match end {
            LlmStepEnd::FinalMessage { text, .. } => {
                assert_eq!(text, "partial");
            }
            other => panic!("expected FinalMessage, got {other:?}"),
        }
    }

    // ----- Tool-call streaming -----

    #[tokio::test]
    async fn tool_call_arrives_on_done_chunk_with_complete_arguments() {
        // Ollama delivers tool_calls all-at-once on done:true.
        // No deltas to reassemble (unlike OpenAI). Text-so-far
        // is empty for pure tool-call turns.
        let body = json!({
            "message": {
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "function": {
                        "name": "fs.read",
                        "arguments": { "path": "foo.txt" }
                    }
                }]
            },
            "done": true,
            "prompt_eval_count": 65,
            "eval_count": 12
        });
        let line = format!("{body}\n");
        let chunks: Vec<&[u8]> = vec![line.as_bytes()];
        let reader = make_reader(chunks);
        let mut stream = OllamaStream::new(reader, known(&["fs.read"]));

        // The single chunk is the terminal; next_event yields
        // None directly.
        assert!(stream.next_event().await.unwrap().is_none());
        let end = Box::new(stream).finish().await.unwrap();
        match end {
            LlmStepEnd::ToolCalls {
                calls,
                text_so_far,
                usage,
            } => {
                assert_eq!(text_so_far, "");
                assert_eq!(usage.input_tokens, 65);
                assert_eq!(usage.output_tokens, 12);
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].tool_name, "fs.read");
                assert_eq!(calls[0].input["path"], "foo.txt");
                // Phase 120 — Known because fs.read is in the
                // request's advertised set.
                assert!(matches!(
                    calls[0].name_resolution,
                    NameResolution::Known
                ));
                // The synthesized call_id is stable (tc-0-fs.read).
                assert_eq!(calls[0].call_id, "tc-0-fs.read");
            }
            other => panic!("expected ToolCalls, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn tool_call_arrives_on_non_terminal_chunk_then_null_on_done() {
        // Observed with Ollama 0.30.5 + qwen3.6: the complete
        // tool_calls array lands on a `done: false` chunk and the
        // terminal `done: true` chunk carries `tool_calls: null`.
        // The reader must capture the non-terminal call rather than
        // ignore it (the old "tool_calls only on done" assumption
        // silently dropped the call → a tool-using turn produced no
        // execution and no text).
        let call_chunk = json!({
            "message": {
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "function": {
                        "name": "fs.read",
                        "arguments": { "path": "foo.txt" }
                    }
                }]
            },
            "done": false
        });
        let done_chunk = json!({
            "message": { "role": "assistant", "content": "", "tool_calls": null },
            "done": true,
            "prompt_eval_count": 65,
            "eval_count": 12
        });
        let lines = format!("{call_chunk}\n{done_chunk}\n");
        let reader = make_reader(vec![lines.as_bytes()]);
        let mut stream = OllamaStream::new(reader, known(&["fs.read"]));

        // No text events; the call rides on a non-terminal chunk and
        // the stream then terminates.
        while stream.next_event().await.unwrap().is_some() {}
        let end = Box::new(stream).finish().await.unwrap();
        match end {
            LlmStepEnd::ToolCalls { calls, usage, .. } => {
                assert_eq!(calls.len(), 1, "the non-terminal tool call must survive");
                assert_eq!(calls[0].tool_name, "fs.read");
                assert_eq!(calls[0].input["path"], "foo.txt");
                // The null tool_calls on the done chunk must not clear it.
                assert_eq!(usage.output_tokens, 12);
            }
            other => panic!("expected ToolCalls, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn text_then_tool_calls_carries_text_so_far_through() {
        // Mixed turn: model narrates, then emits tool calls on
        // the terminal chunk. text_so_far must carry the
        // narration.
        let chunks: Vec<&[u8]> = vec![
            b"{\"message\":{\"role\":\"assistant\",\"content\":\"Let me check.\"},\"done\":false}\n",
            br#"{"message":{"role":"assistant","content":"","tool_calls":[{"function":{"name":"fs.read","arguments":{"path":"a"}}}]},"done":true,"prompt_eval_count":10,"eval_count":5}"#,
            b"\n",
        ];
        let reader = make_reader(chunks);
        let mut stream = OllamaStream::new(reader, known(&["fs.read"]));

        // First: the text delta.
        match stream.next_event().await.unwrap() {
            Some(LlmStreamEvent::TextChunk(s)) => {
                assert_eq!(s, "Let me check.")
            }
            other => panic!("expected TextChunk, got {other:?}"),
        }
        // Then the terminal lands; next_event yields None.
        assert!(stream.next_event().await.unwrap().is_none());

        let end = Box::new(stream).finish().await.unwrap();
        match end {
            LlmStepEnd::ToolCalls {
                calls,
                text_so_far,
                ..
            } => {
                assert_eq!(text_so_far, "Let me check.");
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].tool_name, "fs.read");
            }
            other => panic!("expected ToolCalls, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn hallucinated_tool_name_flagged_unknown() {
        // Phase 120 Task 3 — provider classifies emitted tool
        // names against the advertised set. Ollama's load-bearing
        // case for this phase: model emits `fs_read` when the
        // registered tool is `fs.read`. Provider flags Unknown;
        // the planner's Phase 120 Task 4 fuzzy recovery kicks
        // in downstream.
        let body = json!({
            "message": {
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "function": {
                        "name": "fs_read",
                        "arguments": { "path": "x" }
                    }
                }]
            },
            "done": true,
            "prompt_eval_count": 1,
            "eval_count": 1
        });
        let line = format!("{body}\n");
        let chunks: Vec<&[u8]> = vec![line.as_bytes()];
        let reader = make_reader(chunks);
        // The request advertised fs.read; the model emitted
        // fs_read.
        let mut stream = OllamaStream::new(reader, known(&["fs.read"]));
        assert!(stream.next_event().await.unwrap().is_none());
        let end = Box::new(stream).finish().await.unwrap();
        match end {
            LlmStepEnd::ToolCalls { calls, .. } => {
                assert_eq!(calls[0].tool_name, "fs_read");
                match &calls[0].name_resolution {
                    NameResolution::Unknown { original } => {
                        assert_eq!(original, "fs_read");
                    }
                    other => panic!("expected Unknown, got {other:?}"),
                }
            }
            other => panic!("expected ToolCalls, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn multiple_tool_calls_in_one_terminal_chunk() {
        // Ollama can emit multiple tool calls in one batch — the
        // state machine returns all of them in LlmStepEnd::ToolCalls.
        let body = json!({
            "message": {
                "role": "assistant",
                "content": "",
                "tool_calls": [
                    { "function": { "name": "fs.read", "arguments": { "path": "a" } } },
                    { "function": { "name": "web.fetch", "arguments": { "url": "b" } } }
                ]
            },
            "done": true,
            "prompt_eval_count": 1,
            "eval_count": 1
        });
        let line = format!("{body}\n");
        let chunks: Vec<&[u8]> = vec![line.as_bytes()];
        let reader = make_reader(chunks);
        let mut stream =
            OllamaStream::new(reader, known(&["fs.read", "web.fetch"]));
        assert!(stream.next_event().await.unwrap().is_none());
        let end = Box::new(stream).finish().await.unwrap();
        match end {
            LlmStepEnd::ToolCalls { calls, .. } => {
                assert_eq!(calls.len(), 2);
                assert_eq!(calls[0].tool_name, "fs.read");
                assert_eq!(calls[1].tool_name, "web.fetch");
                // Synthesized call_ids are stable + distinct.
                assert_eq!(calls[0].call_id, "tc-0-fs.read");
                assert_eq!(calls[1].call_id, "tc-1-web.fetch");
            }
            other => panic!("expected ToolCalls, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn malformed_chunk_json_surfaces_parse_error() {
        // A line that's not valid JSON should surface as Parse,
        // not silently skipped.
        let chunks: Vec<&[u8]> = vec![b"{this is not json}\n"];
        let reader = make_reader(chunks);
        let mut stream = OllamaStream::new(reader, empty_known());
        let err = stream.next_event().await.unwrap_err();
        assert!(matches!(err, LlmError::Parse(_)));
    }

    #[tokio::test]
    async fn tool_call_missing_function_surfaces_parse_error() {
        // Defensive: a tool_calls entry without a `function`
        // field violates Ollama's protocol contract; surface as
        // Parse rather than producing a garbage ToolCallEnd.
        let body = json!({
            "message": {
                "role": "assistant",
                "content": "",
                "tool_calls": [ { "id": "tc-bare" } ]
            },
            "done": true
        });
        let line = format!("{body}\n");
        let chunks: Vec<&[u8]> = vec![line.as_bytes()];
        let reader = make_reader(chunks);
        let mut stream = OllamaStream::new(reader, empty_known());
        let err = stream.next_event().await.unwrap_err();
        assert!(matches!(err, LlmError::Parse(_)));
    }

    #[tokio::test]
    async fn tool_call_with_missing_arguments_defaults_to_empty_object() {
        let body = json!({
            "message": {
                "role": "assistant",
                "content": "",
                "tool_calls": [
                    { "function": { "name": "fs.read" } }
                ]
            },
            "done": true
        });
        let line = format!("{body}\n");
        let chunks: Vec<&[u8]> = vec![line.as_bytes()];
        let reader = make_reader(chunks);
        let mut stream = OllamaStream::new(reader, known(&["fs.read"]));
        assert!(stream.next_event().await.unwrap().is_none());
        let end = Box::new(stream).finish().await.unwrap();
        match end {
            LlmStepEnd::ToolCalls { calls, .. } => {
                assert_eq!(calls.len(), 1);
                assert!(calls[0].input.is_object());
                assert_eq!(calls[0].input.as_object().unwrap().len(), 0);
            }
            other => panic!("expected ToolCalls, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn empty_content_chunks_emit_nothing() {
        // Defensive: some Ollama models emit a non-terminal
        // chunk with empty content before the first real token.
        // The state machine should NOT emit an empty TextChunk
        // (that would surprise the caller).
        let chunks: Vec<&[u8]> = vec![
            b"{\"message\":{\"role\":\"assistant\",\"content\":\"\"},\"done\":false}\n",
            b"{\"message\":{\"role\":\"assistant\",\"content\":\"hello\"},\"done\":false}\n",
            b"{\"message\":{\"role\":\"assistant\",\"content\":\"\"},\"done\":true,\"prompt_eval_count\":1,\"eval_count\":1}\n",
        ];
        let reader = make_reader(chunks);
        let mut stream = OllamaStream::new(reader, empty_known());
        // First chunk had empty content → no event.
        match stream.next_event().await.unwrap() {
            Some(LlmStreamEvent::TextChunk(s)) => assert_eq!(s, "hello"),
            other => panic!("expected TextChunk(hello), got {other:?}"),
        }
        assert!(stream.next_event().await.unwrap().is_none());
        let end = Box::new(stream).finish().await.unwrap();
        match end {
            LlmStepEnd::FinalMessage { text, .. } => {
                assert_eq!(text, "hello");
            }
            other => panic!("expected FinalMessage, got {other:?}"),
        }
    }
}
