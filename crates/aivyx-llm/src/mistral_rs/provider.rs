//! `MistralRsProvider` — Aivyx's `LlmProvider` impl
//! over an in-process `mistralrs` model.
//!
//! ## Phase 134 MVP scope
//!
//! - GGUF model loading from a local path.
//! - Text-only chat (image / audio multimodal stays
//!   Phase 135+).
//! - **Non-streaming** `chat_stream` impl — issues
//!   `send_chat_request` (which returns the full
//!   response in one shot) and surfaces the text as
//!   a single `LlmStreamEvent::TextChunk` followed by
//!   `LlmStepEnd::FinalMessage` or
//!   `LlmStepEnd::ToolCalls`.
//!
//! ### Why non-streaming for Phase 134?
//!
//! mistralrs 0.8.1's [`Stream<'a>`] borrows from the
//! Model with a lifetime, while Aivyx's `LlmStream`
//! contract requires `Send + 'static`. Bridging
//! that gap cleanly requires either a self-referential
//! struct (`ouroboros` crate or similar — a new
//! dependency) or a spawned task forwarding through
//! `tokio::sync::mpsc` (architectural work that
//! deserves its own phase).
//!
//! Phase 134 ships the simpler `send_chat_request`
//! path because:
//! 1. The bridge code is correct end-to-end and
//!    operators can actually run local-LLM turns.
//! 2. Local LLM inference latency dominates network
//!    round-trips; streaming UX value is lower on a
//!    100ms-per-token local model than on a
//!    1-second-network-roundtrip cloud model.
//! 3. Streaming is a focused Phase 135 task once
//!    operators have validated the broader
//!    integration.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use mistralrs::{GgufModelBuilder, Model};
use tokio_util::sync::CancellationToken;

use crate::mistral_rs::convert::{append_message_to_builder, apply_tools};
use crate::tool_grammar::{
    parse_constrained_output, system_message_for, tool_call_grammar, ConstrainedOutput,
};
use crate::{
    LlmError, LlmProvider, LlmRequest, LlmStepEnd, LlmStream, LlmStreamEvent, LlmUsage,
    NameResolution, ToolCallEnd,
};

/// Operator-supplied config for the embedded provider.
///
/// `model_path` points at either:
/// - A directory containing one or more GGUF files
///   (e.g. `gguf_models/qwen3-4b/`), in which case
///   `model_file` names the specific file inside, OR
/// - A single GGUF file directly.
#[derive(Debug, Clone)]
pub struct MistralRsConfig {
    pub model_path: PathBuf,
    pub model_file: Option<String>,
    pub chat_template_path: Option<PathBuf>,
    pub max_seq_len: Option<usize>,
    /// Chapter Stencil (ST.3) — when `true`, tool-carrying turns
    /// constrain decoding to the [`tool_call_grammar`] JSON Schema
    /// so a small model emits a valid, real-named call (or the
    /// `respond` text escape) by construction. Default `false`.
    pub constrain_tool_calls: bool,
}

impl MistralRsConfig {
    pub fn new(model_path: impl Into<PathBuf>) -> Self {
        MistralRsConfig {
            model_path: model_path.into(),
            model_file: None,
            chat_template_path: None,
            max_seq_len: None,
            constrain_tool_calls: false,
        }
    }

    pub fn with_model_file(mut self, file: impl Into<String>) -> Self {
        self.model_file = Some(file.into());
        self
    }

    pub fn with_chat_template(mut self, path: impl Into<PathBuf>) -> Self {
        self.chat_template_path = Some(path.into());
        self
    }

    pub fn with_max_seq_len(mut self, len: usize) -> Self {
        self.max_seq_len = Some(len);
        self
    }

    pub fn with_constrain_tool_calls(mut self, on: bool) -> Self {
        self.constrain_tool_calls = on;
        self
    }
}

/// In-process LLM provider backed by mistralrs.
///
/// The model is loaded once at construction time and
/// reused across every `chat_stream` call.
pub struct MistralRsProvider {
    model: Arc<Model>,
    /// Chapter Stencil (ST.3) — grammar-constrain tool-carrying
    /// turns. Copied from [`MistralRsConfig::constrain_tool_calls`]
    /// at construction.
    constrain_tool_calls: bool,
}

impl MistralRsProvider {
    /// Build a provider by loading the configured GGUF
    /// model. Async because mistralrs's builder runs
    /// the model load (mmap + tokenizer init + chat
    /// template parse) on a worker; this is the only
    /// `chat_stream`-blocking work.
    pub async fn new(config: MistralRsConfig) -> Result<Self, LlmError> {
        let constrain_tool_calls = config.constrain_tool_calls;
        let (dir, files) = match &config.model_file {
            Some(f) => (
                config.model_path.to_string_lossy().to_string(),
                vec![f.clone()],
            ),
            None => {
                // The operator pointed at a single GGUF file
                // directly. Split into (parent_dir, filename).
                let parent = config
                    .model_path
                    .parent()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_else(|| ".".to_string());
                let filename = config
                    .model_path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .ok_or_else(|| {
                        LlmError::Config(format!(
                            "mistralrs: cannot extract filename from {:?}",
                            config.model_path,
                        ))
                    })?;
                (parent, vec![filename])
            }
        };

        let mut builder = GgufModelBuilder::new(dir, files);
        if let Some(template) = &config.chat_template_path {
            builder = builder.with_chat_template(template.to_string_lossy().to_string());
        }
        let model = builder
            .build()
            .await
            .map_err(|e| LlmError::Config(format!("mistralrs: model load failed: {e}")))?;
        Ok(MistralRsProvider {
            model: Arc::new(model),
            constrain_tool_calls,
        })
    }
}

#[async_trait]
impl LlmProvider for MistralRsProvider {
    async fn chat_stream(
        &self,
        request: LlmRequest<'_>,
        _cancellation: &CancellationToken,
    ) -> Result<Box<dyn LlmStream>, LlmError> {
        // Chapter Stencil (ST.3) — grammar-constrained tool-calling.
        // On tool-carrying turns, constrain decoding to a JSON-Schema
        // grammar so the model can only emit a valid, real-named call
        // (or the `respond` text escape). `JsonSchema` constrains the
        // raw output *tokens*, not mistralrs's native tool-call
        // channel — so the constrained JSON arrives as message
        // `content`, which we parse below. Off (the default) or on a
        // tool-less turn: the unchanged, unconstrained path.
        let constrain = self.constrain_tool_calls && !request.tools.is_empty();

        // Build the request.
        let mut builder = mistralrs::RequestBuilder::new();
        // Chapter Bridle (BR.3) — when constrained, augment the system
        // message with the `respond` preamble so the model knows the
        // sentinel is how it replies in plain text and ends the turn.
        // The grammar (Stencil) *admits* `respond`; without this note a
        // small model never *chooses* it and loops (ST.4 finding). Off
        // when unconstrained → the system prompt is unchanged.
        let system_text = system_message_for(request.system, constrain);
        if let Some(sys) = system_text {
            builder = builder.add_message(mistralrs::TextMessageRole::System, sys);
        }
        for msg in request.messages {
            builder = append_message_to_builder(builder, msg);
        }
        builder = apply_tools(builder, request.tools)
            .map_err(|e| LlmError::Config(format!("mistralrs tool conversion: {e}")))?;

        if constrain {
            let grammar = tool_call_grammar(request.tools);
            builder = builder.set_constraint(mistralrs::Constraint::JsonSchema(grammar));
        }

        builder = builder.set_sampler_max_len(request.max_tokens as usize);
        if let Some(temp) = request.temperature {
            builder = builder.set_sampler_temperature(temp.into());
        }

        // Issue the request. Phase 134 uses the non-streaming
        // `send_chat_request` to avoid mistralrs's borrowed
        // `Stream<'a>` lifetime — see module-level docs.
        let response = self
            .model
            .send_chat_request(builder)
            .await
            .map_err(|e| LlmError::Config(format!("mistralrs send_chat_request: {e}")))?;

        // Extract the parts of the response we care about.
        let choice = response.choices.into_iter().next().ok_or_else(|| {
            LlmError::Parse("mistralrs: response has no choices".to_string())
        })?;
        // `"length"`: the reply hit `max_tokens` (the sampler's max length).
        let truncated = choice.finish_reason == "length";
        let finish_reason = choice.finish_reason.clone();
        let native_text = choice.message.content.unwrap_or_default();
        let native_tool_calls: Vec<ToolCallEnd> = choice
            .message
            .tool_calls
            .unwrap_or_default()
            .into_iter()
            .map(|tc| {
                let input = serde_json::from_str(&tc.function.arguments)
                    .unwrap_or(serde_json::json!({}));
                ToolCallEnd {
                    call_id: tc.id,
                    tool_name: tc.function.name,
                    input,
                    name_resolution: NameResolution::Known,
                }
            })
            .collect();

        // When constrained, the grammar-shaped JSON is in `content`,
        // not the native tool-call channel. Parse it: a `respond`
        // sentinel unwraps to plain text; any other name is a real
        // tool call. If parsing somehow fails (the grammar makes it
        // well-formed, so this is defensive), fall through to the
        // native extraction.
        let (text, tool_calls) = match (constrain, parse_constrained_output(&native_text)) {
            (true, Some(ConstrainedOutput::Text(t))) => (t, Vec::new()),
            (true, Some(ConstrainedOutput::ToolCall { tool_name, input })) => (
                String::new(),
                vec![ToolCallEnd {
                    call_id: "mistralrs-constrained-call".to_string(),
                    tool_name,
                    input,
                    name_resolution: NameResolution::Known,
                }],
            ),
            _ => (native_text, native_tool_calls),
        };

        let usage = LlmUsage {
            input_tokens: response.usage.prompt_tokens as u32,
            output_tokens: response.usage.completion_tokens as u32,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        };

        // Wrap into a single-shot stream that emits the text
        // and then ends with the appropriate StepEnd variant.
        Ok(Box::new(SingleShotStream {
            text_emitted: false,
            text: text.clone(),
            tool_calls,
            usage,
            finish_reason,
            truncated,
            ended: false,
        }))
    }

    async fn tool_call_family_hint(&self, _model: &str) -> Option<String> {
        // Phase 134 — no family probe. Phase 135+ adds a
        // config knob (`[mistralrs] family_hint = "qwen3"`)
        // for operators who want to bias the textual
        // extractor.
        None
    }
}

/// LlmStream impl that surfaces a pre-computed response
/// as one TextChunk + one StepEnd. Used by the
/// non-streaming Phase 134 path.
struct SingleShotStream {
    text_emitted: bool,
    text: String,
    tool_calls: Vec<ToolCallEnd>,
    usage: LlmUsage,
    /// The response's `finish_reason`, and whether it means the reply was
    /// cut off (`"length"`).
    finish_reason: String,
    truncated: bool,
    /// Set once the finish reason has been reported.
    ended: bool,
}

#[async_trait]
impl LlmStream for SingleShotStream {
    async fn next_event(&mut self) -> Result<Option<LlmStreamEvent>, LlmError> {
        if !self.text_emitted && !self.text.is_empty() {
            self.text_emitted = true;
            return Ok(Some(LlmStreamEvent::TextChunk(self.text.clone())));
        }
        if !self.ended {
            self.ended = true;
            return Ok(crate::finish_event(
                "mistralrs",
                Some(&self.finish_reason),
                self.truncated,
            ));
        }
        Ok(None)
    }

    async fn finish(self: Box<Self>) -> Result<LlmStepEnd, LlmError> {
        if self.tool_calls.is_empty() {
            Ok(LlmStepEnd::FinalMessage {
                text: self.text,
                usage: self.usage,
            })
        } else {
            Ok(LlmStepEnd::ToolCalls {
                calls: self.tool_calls,
                text_so_far: self.text,
                usage: self.usage,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mistralrs_config_builder_chain() {
        let cfg = MistralRsConfig::new("/models/qwen3")
            .with_model_file("qwen3-4b-q4_k_m.gguf")
            .with_chat_template("/templates/qwen3.json")
            .with_max_seq_len(8192);
        assert_eq!(cfg.model_path.to_string_lossy(), "/models/qwen3");
        assert_eq!(cfg.model_file.as_deref(), Some("qwen3-4b-q4_k_m.gguf"));
        assert_eq!(
            cfg.chat_template_path.as_ref().map(|p| p.to_string_lossy().to_string()),
            Some("/templates/qwen3.json".to_string()),
        );
        assert_eq!(cfg.max_seq_len, Some(8192));
    }

    #[test]
    fn mistralrs_config_minimal_construction() {
        let cfg = MistralRsConfig::new("/models/single-file.gguf");
        assert!(cfg.model_file.is_none());
        assert!(cfg.chat_template_path.is_none());
        assert!(cfg.max_seq_len.is_none());
        // Chapter Stencil (ST.3) — constraint defaults off.
        assert!(!cfg.constrain_tool_calls);
    }

    #[test]
    fn mistralrs_config_constrain_tool_calls_builder() {
        let cfg = MistralRsConfig::new("/models/x.gguf").with_constrain_tool_calls(true);
        assert!(cfg.constrain_tool_calls);
    }

    // The constrained-output parser + `respond` preamble tests moved to
    // `tool_grammar.rs` with their functions (Chapter Emboss EB.1).

    #[tokio::test]
    async fn single_shot_stream_emits_text_then_final_message() {
        let stream = SingleShotStream {
            text_emitted: false,
            text: "hello".to_string(),
            tool_calls: Vec::new(),
            usage: LlmUsage::default(),
            finish_reason: "stop".to_string(),
            truncated: false,
            ended: false,
        };
        let mut boxed: Box<dyn LlmStream> = Box::new(stream);
        let ev = boxed.next_event().await.unwrap().expect("text chunk");
        match ev {
            LlmStreamEvent::TextChunk(s) => assert_eq!(s, "hello"),
            other => panic!("expected TextChunk, got {other:?}"),
        }
        let next = boxed.next_event().await.unwrap();
        assert!(next.is_none(), "stream ends after one chunk");
        let end = boxed.finish().await.unwrap();
        match end {
            LlmStepEnd::FinalMessage { text, .. } => assert_eq!(text, "hello"),
            other => panic!("expected FinalMessage, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn single_shot_stream_with_tool_calls_finishes_as_tool_calls() {
        let stream = SingleShotStream {
            text_emitted: true,
            text: String::new(),
            tool_calls: vec![ToolCallEnd {
                call_id: "abc".to_string(),
                tool_name: "fs.read".to_string(),
                input: serde_json::json!({"path": "/etc/hosts"}),
                name_resolution: NameResolution::Known,
            }],
            usage: LlmUsage::default(),
            finish_reason: "tool_calls".to_string(),
            truncated: false,
            ended: false,
        };
        let mut boxed: Box<dyn LlmStream> = Box::new(stream);
        assert!(boxed.next_event().await.unwrap().is_none());
        let end = boxed.finish().await.unwrap();
        match end {
            LlmStepEnd::ToolCalls { calls, .. } => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].tool_name, "fs.read");
            }
            other => panic!("expected ToolCalls, got {other:?}"),
        }
    }

    /// Task 5c item 11: `finish_reason: "length"` yields `Truncated` after
    /// the text, then the stream ends with the partial `FinalMessage`.
    #[tokio::test]
    async fn single_shot_stream_reports_a_length_cut_off() {
        let stream = SingleShotStream {
            text_emitted: false,
            text: "half a sent".to_string(),
            tool_calls: Vec::new(),
            usage: LlmUsage::default(),
            finish_reason: "length".to_string(),
            truncated: true,
            ended: false,
        };
        let mut boxed: Box<dyn LlmStream> = Box::new(stream);
        assert!(matches!(boxed.next_event().await.unwrap(), Some(LlmStreamEvent::TextChunk(_))));
        assert_eq!(boxed.next_event().await.unwrap(), Some(LlmStreamEvent::Truncated));
        assert!(boxed.next_event().await.unwrap().is_none());
        assert!(matches!(boxed.finish().await.unwrap(), LlmStepEnd::FinalMessage { .. }));
    }
}
