//! Model routing Part 3a — `routing.status` / `routing.explain` tools.
//! Read-only views of the daemon's `RoutedProvider`: the router's
//! candidate list, and the last routing decision for a session.
//! Infrastructure-tier (see `crates/aivyx-capability/src/lib.rs`'s
//! `KNOWN_BASES` doc comment). Registered only when `[routing] enabled`.

use std::sync::Arc;

use aivyx_capability::Scope;
use aivyx_llm::RoutedProvider;
use async_trait::async_trait;
use serde_json::{Value, json};

use crate::{Tool, ToolContext, ToolId, ToolOutcome, Verification};

// ---------------------------------------------------------------------------
// routing.status
// ---------------------------------------------------------------------------

/// `routing.status` — the router's default model and every candidate.
pub struct RoutingStatusTool {
    id: ToolId,
    schema: Value,
    routed: Arc<RoutedProvider>,
}

impl std::fmt::Debug for RoutingStatusTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoutingStatusTool")
            .field("id", &self.id)
            .finish()
    }
}

impl RoutingStatusTool {
    pub fn new(routed: Arc<RoutedProvider>) -> Self {
        RoutingStatusTool {
            id: ToolId::new(),
            schema: status_input_schema(),
            routed,
        }
    }
}

#[async_trait]
impl Tool for RoutingStatusTool {
    fn id(&self) -> ToolId {
        self.id
    }

    fn name(&self) -> &str {
        "routing.status"
    }

    fn description(&self) -> &str {
        "Show the model router's candidates. Input is a JSON object (no \
         fields required). Returns a JSON object with `default` (the \
         configured model, as `id@endpoint`) and a `candidates` array — \
         each entry has `model` (`id@endpoint`), `tier`, `capabilities` \
         (known), `unknown_capabilities` (assumed but unconfirmed), \
         `context_window` (tokens, or null when unknown) and `availability` \
         (`available`, `unverified` or `unavailable`)."
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn required_scope(&self, _input: &Value) -> Scope {
        Scope::parse("routing.status").expect("routing.status must parse — it is in KNOWN_BASES")
    }

    // Read-only over the daemon's own router state, no model-supplied
    // path (the same risk profile as skill_defaults.list, already
    // floored) — included in the zero-config default role's capability
    // floor. See compute_backcompat_floor's own doc comment in aivyx.rs.
    fn auto_grantable_in_backcompat_floor(&self) -> bool {
        true
    }

    // Chapter Bulwark — model ids come from endpoint discovery (a local
    // server's `/api/tags` or `/v1/models`), i.e. external content.
    // Picket/Bulwark cover every call's output automatically.
    fn output_is_untrusted(&self) -> bool {
        true
    }

    async fn execute(&self, _input: Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        let candidates: Vec<Value> = self
            .routed
            .router()
            .profiles()
            .iter()
            .map(|p| {
                json!({
                    "model": p.key().to_string(),
                    "tier": p.tier.to_string(),
                    "capabilities": p
                        .capabilities
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>(),
                    "unknown_capabilities": p
                        .unknown_capabilities
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>(),
                    "context_window": p.context_window,
                    "availability": p.availability,
                })
            })
            .collect();
        ToolOutcome::Completed {
            output: json!({
                "default": self.routed.default_key().to_string(),
                "candidates": candidates,
            }),
            verified: Verification::Verified,
        }
    }
}

// ---------------------------------------------------------------------------
// routing.explain
// ---------------------------------------------------------------------------

/// `routing.explain` — the router's last decision for a session (the
/// calling turn's own session when `session` is omitted).
pub struct RoutingExplainTool {
    id: ToolId,
    schema: Value,
    routed: Arc<RoutedProvider>,
}

impl std::fmt::Debug for RoutingExplainTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoutingExplainTool")
            .field("id", &self.id)
            .finish()
    }
}

impl RoutingExplainTool {
    pub fn new(routed: Arc<RoutedProvider>) -> Self {
        RoutingExplainTool {
            id: ToolId::new(),
            schema: explain_input_schema(),
            routed,
        }
    }
}

#[async_trait]
impl Tool for RoutingExplainTool {
    fn id(&self) -> ToolId {
        self.id
    }

    fn name(&self) -> &str {
        "routing.explain"
    }

    fn description(&self) -> &str {
        "Explain which model the router last chose for a conversation, and \
         why. Input is a JSON object with an optional `session` field (a \
         session id; defaults to the current conversation). Returns a JSON \
         object with `session`, `model` (`id@endpoint`), `task` (the kind \
         of call that was routed) and `reason`, or `decision: null` when \
         nothing has been routed for that session yet."
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn required_scope(&self, _input: &Value) -> Scope {
        Scope::parse("routing.read").expect("routing.read must parse — it is in KNOWN_BASES")
    }

    // Read-only, same rationale as RoutingStatusTool.
    fn auto_grantable_in_backcompat_floor(&self) -> bool {
        true
    }

    // Chapter Bulwark — the model id and the reason (which names models)
    // come from endpoint discovery, i.e. external content.
    fn output_is_untrusted(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, ctx: &ToolContext<'_>) -> ToolOutcome {
        let session = match input.get("session") {
            None | Some(Value::Null) => ctx.session_id.to_string(),
            Some(Value::String(s)) if !s.is_empty() => s.clone(),
            Some(_) => {
                return ToolOutcome::Failed(crate::AivyxError::Tool {
                    tool: self.id,
                    detail: "routing.explain: `session` must be a non-empty string".to_string(),
                });
            }
        };
        let output = match self.routed.router().last_decision(&session) {
            Some(record) => json!({
                "session": session,
                "model": record.model.to_string(),
                "task": record.task.name(),
                "reason": record.reason,
            }),
            None => json!({ "session": session, "decision": null }),
        };
        ToolOutcome::Completed {
            output,
            verified: Verification::Verified,
        }
    }
}

// ---------------------------------------------------------------------------
// Schemas
// ---------------------------------------------------------------------------

fn status_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {},
        "required": []
    })
}

fn explain_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "session": {
                "type": "string",
                "description": "Session id to explain; omit for the current conversation."
            }
        },
        "required": []
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod routing_tool_tests {
    use super::*;
    use crate::{AgentId, CancellationToken, MessageOrigin, NullAuditHook, SessionId, TurnId};
    use aivyx_llm::{
        LlmError, LlmMessage, LlmProvider, LlmRequest, LlmStepEnd, LlmStream, LlmStreamEvent,
        LlmUsage, ProviderFactory, RouteHint,
    };
    use aivyx_route::{
        Availability, Capability, EndpointRef, ModelKey, ModelProfile, Router, TaskKind,
        TaskOverrides, Tier,
    };

    struct NoopChannel {
        session: SessionId,
        token: CancellationToken,
    }

    #[async_trait]
    impl crate::ChannelContext for NoopChannel {
        fn channel_name(&self) -> &str {
            "routing-test"
        }
        fn platform(&self) -> crate::ChannelPlatform {
            crate::ChannelPlatform::Local
        }
        fn trust_tier(&self) -> aivyx_capability::TrustTier {
            aivyx_capability::TrustTier::Trusted
        }
        fn session_id(&self) -> SessionId {
            self.session
        }
        async fn stream_event(
            &self,
            _event: crate::StreamEvent<'_>,
        ) -> Result<(), crate::ChannelError> {
            Ok(())
        }
        async fn finalize(&self, _outcome: &crate::TurnOutcome) -> Result<(), crate::ChannelError> {
            Ok(())
        }
        fn cancellation_token(&self) -> CancellationToken {
            self.token.clone()
        }
    }

    async fn run_execute(tool: &dyn Tool, session: SessionId, input: Value) -> ToolOutcome {
        let channel = NoopChannel {
            session,
            token: CancellationToken::new(),
        };
        let audit = NullAuditHook;
        let ctx = ToolContext {
            agent_id: AgentId::new(),
            session_id: channel.session,
            turn_id: TurnId::new(),
            channel: &channel,
            audit: &audit,
            cancellation: &channel.token,
            message_origin: MessageOrigin::Operator,
        };
        tool.execute(input, &ctx).await
    }

    fn completed(outcome: ToolOutcome) -> Value {
        match outcome {
            ToolOutcome::Completed { output, .. } => output,
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    /// Answers every call with an empty successful stream.
    struct Ok_;

    #[async_trait]
    impl LlmProvider for Ok_ {
        async fn chat_stream(
            &self,
            _request: LlmRequest<'_>,
            _cancellation: &CancellationToken,
        ) -> Result<Box<dyn LlmStream>, LlmError> {
            Ok(Box::new(EmptyStream))
        }
    }

    struct EmptyStream;

    #[async_trait]
    impl LlmStream for EmptyStream {
        async fn next_event(&mut self) -> Result<Option<LlmStreamEvent>, LlmError> {
            Ok(None)
        }
        async fn finish(self: Box<Self>) -> Result<LlmStepEnd, LlmError> {
            Ok(LlmStepEnd::FinalMessage {
                text: String::new(),
                usage: LlmUsage::default(),
            })
        }
    }

    /// `small@default` (the configured model) plus `big@gpu`, a large
    /// tool-calling model with a known context window.
    fn routed() -> Arc<RoutedProvider> {
        let mut small = ModelProfile::new("small", EndpointRef::new("default"));
        small.tier = Tier::Small;
        small.capabilities.insert(Capability::Completion);
        small.unknown_capabilities.insert(Capability::Tools);
        let mut big = ModelProfile::new("big", EndpointRef::new("gpu"));
        big.tier = Tier::Large;
        big.capabilities
            .extend([Capability::Completion, Capability::Tools]);
        big.context_window = Some(32_768);
        big.availability = Availability::Available;
        let factory: ProviderFactory =
            Box::new(|_: &EndpointRef| Ok(Arc::new(Ok_) as Arc<dyn LlmProvider>));
        Arc::new(RoutedProvider::new(
            ModelKey {
                endpoint: EndpointRef::new("default"),
                id: "small".into(),
            },
            Arc::new(Ok_),
            Router::new(vec![small, big], TaskOverrides::default()),
            factory,
        ))
    }

    /// One routed `Plan` call tagged with `session`.
    async fn route_once(routed: &RoutedProvider, session: &str) {
        let messages = [LlmMessage::user_text("hi")];
        let request = LlmRequest {
            model: "small",
            system: None,
            messages: &messages,
            tools: &[],
            max_tokens: 8,
            temperature: None,
            id_slot: None,
            slot_hint: None,
            route: Some(RouteHint {
                task: TaskKind::Plan,
                session: Some(session.to_string()),
                estimated_prompt_tokens: 0,
            }),
        };
        let stream = routed
            .chat_stream(request, &CancellationToken::new())
            .await
            .unwrap();
        stream.finish().await.unwrap();
    }

    #[tokio::test]
    async fn status_lists_the_default_and_every_candidate() {
        let tool = RoutingStatusTool::new(routed());
        let output = completed(run_execute(&tool, SessionId::new(), json!({})).await);
        assert_eq!(output["default"], "small@default");
        let candidates = output["candidates"].as_array().unwrap();
        assert_eq!(candidates.len(), 2);
        let big = candidates
            .iter()
            .find(|c| c["model"] == "big@gpu")
            .expect("big@gpu listed");
        assert_eq!(big["tier"], "large");
        assert_eq!(big["capabilities"], json!(["completion", "tools"]));
        assert_eq!(big["unknown_capabilities"], json!([]));
        assert_eq!(big["context_window"], 32_768);
        assert_eq!(big["availability"], "available");
        let small = candidates
            .iter()
            .find(|c| c["model"] == "small@default")
            .expect("small@default listed");
        assert_eq!(small["unknown_capabilities"], json!(["tools"]));
        assert_eq!(small["context_window"], Value::Null);
    }

    #[tokio::test]
    async fn explain_is_null_before_any_routed_call() {
        let tool = RoutingExplainTool::new(routed());
        let session = SessionId::new();
        let output = completed(run_execute(&tool, session, json!({})).await);
        assert_eq!(output["decision"], Value::Null);
        assert!(output.get("decision").is_some());
        assert_eq!(output["session"], session.to_string());
    }

    #[tokio::test]
    async fn explain_defaults_to_the_calling_turns_session() {
        let routed = routed();
        let session = SessionId::new();
        route_once(&routed, &session.to_string()).await;
        let tool = RoutingExplainTool::new(Arc::clone(&routed));
        let output = completed(run_execute(&tool, session, json!({})).await);
        assert_eq!(output["model"], "big@gpu");
        assert_eq!(output["task"], "plan");
        assert!(!output["reason"].as_str().unwrap().is_empty());
        assert!(output.get("decision").is_none());

        // Another session has no decision of its own.
        let other = completed(run_execute(&tool, SessionId::new(), json!({})).await);
        assert_eq!(other["decision"], Value::Null);
    }

    #[tokio::test]
    async fn explain_honours_an_explicit_session() {
        let routed = routed();
        route_once(&routed, "sess-1").await;
        let tool = RoutingExplainTool::new(Arc::clone(&routed));
        let output =
            completed(run_execute(&tool, SessionId::new(), json!({ "session": "sess-1" })).await);
        assert_eq!(output["session"], "sess-1");
        assert_eq!(output["model"], "big@gpu");
    }

    #[tokio::test]
    async fn explain_rejects_a_non_string_session() {
        let tool = RoutingExplainTool::new(routed());
        match run_execute(&tool, SessionId::new(), json!({ "session": 7 })).await {
            ToolOutcome::Failed(crate::AivyxError::Tool { detail, .. }) => {
                assert!(detail.contains("session"), "{detail}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn scopes_floor_trust_and_metadata() {
        let status = RoutingStatusTool::new(routed());
        let explain = RoutingExplainTool::new(routed());
        assert_eq!(status.required_scope(&json!({})).as_str(), "routing.status");
        assert_eq!(explain.required_scope(&json!({})).as_str(), "routing.read");
        for tool in [&status as &dyn Tool, &explain] {
            assert!(tool.auto_grantable_in_backcompat_floor(), "{}", tool.name());
            assert!(tool.output_is_untrusted(), "{}", tool.name());
            assert_eq!(super::super::check_tool_quality(tool), Vec::<String>::new());
        }
    }
}
