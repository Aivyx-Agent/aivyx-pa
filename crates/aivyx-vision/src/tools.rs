//! `vision.generate_svg` -- the one tool this milestone adds.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use aivyx_capability::Scope;
use aivyx_core::{AivyxError, Tool, ToolContext, ToolId, ToolOutcome, Verification};
use aivyx_vision_svg::TextCompleter;

pub struct GenerateSvgTool {
    id: ToolId,
    schema: Value,
    completer: Arc<dyn TextCompleter>,
}

impl GenerateSvgTool {
    pub fn new(completer: Arc<dyn TextCompleter>) -> Self {
        Self {
            id: ToolId::new(),
            schema: input_schema(),
            completer,
        }
    }
}

#[async_trait]
impl Tool for GenerateSvgTool {
    fn id(&self) -> ToolId {
        self.id
    }
    fn name(&self) -> &str {
        "vision.generate_svg"
    }
    fn description(&self) -> &str {
        "Generate a sanitized SVG image from a text prompt. Input: \
         `{prompt: string (required)}`. Returns `{svg: string}` -- the \
         sanitized SVG markup; the caller decides whether/where to save \
         it. Scope: `vision.generate`."
    }
    fn input_schema(&self) -> &Value {
        &self.schema
    }
    fn required_scope(&self, _input: &Value) -> Scope {
        Scope::parse("vision.generate")
            .expect("vision.generate must parse -- it is in aivyx-capability's KNOWN_BASES")
    }
    async fn execute(&self, input: Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        let prompt = match required_string(&input, "prompt") {
            Ok(s) => s,
            Err(e) => return failed(self.id, format!("vision.generate_svg: {e}")),
        };
        match aivyx_vision_svg::generate_svg(self.completer.as_ref(), &prompt).await {
            Ok(svg) => ToolOutcome::Completed {
                output: json!({ "svg": svg }),
                // Generation + sanitization already happened; there is
                // nothing further to verify against an external source
                // of truth, same reasoning as calc.eval's own pure
                // computation.
                verified: Verification::NotApplicable,
            },
            Err(e) => failed(self.id, format!("vision.generate_svg: {e}")),
        }
    }
}

fn input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "prompt": {
                "type": "string",
                "minLength": 1,
                "description": "Description of the SVG image to generate, \
                                e.g. \"a small red circle icon\"."
            }
        },
        "required": ["prompt"],
        "additionalProperties": false
    })
}

fn required_string(input: &Value, field: &str) -> Result<String, String> {
    let s = input
        .get(field)
        .and_then(|v| v.as_str())
        .ok_or_else(|| format!("input must include a `{field}` string field"))?;
    if s.trim().is_empty() {
        return Err(format!("`{field}` must not be empty"));
    }
    Ok(s.to_string())
}

fn failed(id: ToolId, detail: String) -> ToolOutcome {
    ToolOutcome::Failed(AivyxError::Tool { tool: id, detail })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use aivyx_vision_svg::TextCompleterError;

    /// A fake `TextCompleter` returning a canned response, mirroring
    /// `aivyx-vision-svg`'s own test-double shape.
    struct FakeCompleter {
        response: Mutex<Option<Result<String, TextCompleterError>>>,
    }

    impl FakeCompleter {
        fn returning(response: Result<String, TextCompleterError>) -> Self {
            Self {
                response: Mutex::new(Some(response)),
            }
        }
    }

    #[async_trait::async_trait]
    impl aivyx_vision_svg::TextCompleter for FakeCompleter {
        async fn complete(&self, _prompt: &str) -> Result<String, TextCompleterError> {
            self.response
                .lock()
                .unwrap()
                .take()
                .expect("FakeCompleter.complete called more than once")
        }
    }

    /// A minimal, real `ToolContext` for unit-testing a single tool's
    /// `execute()` outside the IPC harness. `GenerateSvgTool::execute`
    /// ignores `_ctx` entirely (same as `calc.eval`'s), so a no-op
    /// `ChannelContext`/`NullAuditHook` fixture is legitimate here --
    /// this is the same `stub_ctx` shape already established by
    /// `aivyx-obsidian`'s own tool tests (e.g.
    /// `crates/aivyx-obsidian/src/tools/get_note.rs`), copied verbatim
    /// rather than invented fresh. `calc.eval`'s own tests never call
    /// `execute()` at all (they test `evaluate()` + metadata
    /// separately), so that crate offered no precedent either way.
    fn dummy_context<'a>() -> ToolContext<'a> {
        use aivyx_core::{AgentId, CancellationToken, NullAuditHook, SessionId, TurnId};

        struct NoopChannel;

        #[async_trait::async_trait]
        impl aivyx_core::ChannelContext for NoopChannel {
            fn channel_name(&self) -> &str {
                "test"
            }
            fn platform(&self) -> aivyx_core::ChannelPlatform {
                aivyx_core::ChannelPlatform::Local
            }
            fn trust_tier(&self) -> aivyx_capability::TrustTier {
                aivyx_capability::TrustTier::Trusted
            }
            fn session_id(&self) -> SessionId {
                SessionId::new()
            }
            async fn stream_event(
                &self,
                _e: aivyx_core::StreamEvent<'_>,
            ) -> Result<(), aivyx_core::ChannelError> {
                Ok(())
            }
            async fn finalize(
                &self,
                _o: &aivyx_core::TurnOutcome,
            ) -> Result<(), aivyx_core::ChannelError> {
                Ok(())
            }
            fn cancellation_token(&self) -> CancellationToken {
                CancellationToken::new()
            }
        }

        let channel: &'static dyn aivyx_core::ChannelContext = Box::leak(Box::new(NoopChannel));
        let audit: &'static NullAuditHook = Box::leak(Box::new(NullAuditHook));
        let cancel: &'static CancellationToken = Box::leak(Box::new(CancellationToken::new()));
        ToolContext {
            agent_id: AgentId::new(),
            session_id: SessionId::new(),
            turn_id: TurnId::new(),
            channel,
            audit,
            cancellation: cancel,
            message_origin: aivyx_core::MessageOrigin::Operator,
        }
    }

    #[tokio::test]
    async fn tool_metadata_is_sound() {
        let tool = GenerateSvgTool::new(Arc::new(FakeCompleter::returning(Ok(String::new()))));
        assert_eq!(tool.name(), "vision.generate_svg");
        assert!(!tool.description().is_empty());
        assert_eq!(tool.input_schema()["type"], "object");
        assert_eq!(
            tool.required_scope(&json!({"prompt": "anything"})),
            Scope::parse("vision.generate").unwrap()
        );
    }

    #[tokio::test]
    async fn execute_returns_the_sanitized_svg_on_success() {
        let completer = FakeCompleter::returning(Ok(
            "<svg xmlns=\"http://www.w3.org/2000/svg\"><circle r=\"5\"/></svg>".to_string(),
        ));
        let tool = GenerateSvgTool::new(Arc::new(completer));
        let outcome = tool
            .execute(json!({"prompt": "a small circle"}), &dummy_context())
            .await;
        match outcome {
            ToolOutcome::Completed { output, .. } => {
                let svg = output["svg"]
                    .as_str()
                    .expect("output must have a svg string field");
                assert!(svg.contains("<circle"));
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn execute_fails_clearly_on_a_missing_prompt_field() {
        let tool = GenerateSvgTool::new(Arc::new(FakeCompleter::returning(Ok(String::new()))));
        let outcome = tool.execute(json!({}), &dummy_context()).await;
        assert!(matches!(outcome, ToolOutcome::Failed(_)));
    }

    #[tokio::test]
    async fn execute_fails_clearly_when_generation_errors() {
        let completer = FakeCompleter::returning(Err(TextCompleterError("backend down".into())));
        let tool = GenerateSvgTool::new(Arc::new(completer));
        let outcome = tool
            .execute(json!({"prompt": "anything"}), &dummy_context())
            .await;
        assert!(matches!(outcome, ToolOutcome::Failed(_)));
    }
}
