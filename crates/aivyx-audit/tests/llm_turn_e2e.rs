//! Phase 2 task 5 — full end-to-end integration test.
//!
//! This is the "it actually works" gate for Phase 2. It drives a real
//! `ConcreteAgent` turn through:
//!
//! ```text
//! ConcreteAgent
//!   ↓ begin_turn + next_step + observe_tool_outcome
//! LlmPlanner
//!   ↓ chat_stream
//! OpenAiProvider (OpenAI-compatible, local server)
//!   ↓ HttpTransport::post_sse
//! FakeTransport (canned SSE bytes)
//!   ↓ bytes_stream
//! SSE line parser (inside OpenAiStream)
//!   ↓ state machine
//! LlmStepEnd → NextStep → ToolRegistry lookup → Tool::execute
//!   ↓
//! ToolOutcome → LlmMessage::ToolResult (appended to history)
//!   ↓ second chat_stream call, FinalMessage
//!   ↓
//! AuditBridge
//!   ↓ append
//! HmacChainLog
//! ```
//!
//! If this test passes, every seam in Phase 2 lines up. If any one
//! piece is even slightly misshapen — a wrong field name, a flipped
//! enum variant, a missed `begin_turn` wiring, a broken SSE frame
//! terminator — this test won't compile or won't pass.
//!
//! The test is an integration test (not a unit test) because it
//! deliberately exercises every crate *only* through its public API.
//! If it ever needs a `#[cfg(test)]`-only item, a public surface is
//! broken and that is the signal.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::stream;
use serde_json::{json, Value};
use tokio_util::sync::CancellationToken;

use aivyx_audit::{AuditBridge, AuditEvent, AuditLog, HmacChainLog};
use aivyx_capability::{CapabilitySet, Scope, TrustTier};
use aivyx_core::{
    agent::ConcreteAgent,
    llm_planner::{LlmPlanner, LlmPlannerConfig},
    planner::ToolRegistry,
    Agent, AgentId, ChannelContext, ChannelError, ChannelPlatform, Message, SessionId,
    StreamEvent, Tool, ToolContext, ToolId, ToolOutcome, TurnOutcome, TurnOutcomeSummary,
    Verification,
};
use aivyx_llm::openai::{OpenAiConfig, OpenAiProvider};
use aivyx_llm::transport::{ByteStream, HttpTransport};
use aivyx_llm::{LlmError, LlmProvider};

// ---------------------------------------------------------------------------
// FakeTransport — replays canned SSE bytes in order, one response per
// chat_stream call. Identical in shape to the FakeTransport inside the
// provider's own test module, rebuilt here so this test exercises only
// the public HttpTransport trait.
// ---------------------------------------------------------------------------

struct FakeTransport {
    responses: Mutex<std::collections::VecDeque<Vec<&'static str>>>,
}

impl FakeTransport {
    fn new(responses: Vec<Vec<&'static str>>) -> Self {
        FakeTransport {
            responses: Mutex::new(responses.into()),
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
        let chunks = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| LlmError::Config("FakeTransport exhausted".to_string()))?;
        let iter = chunks
            .into_iter()
            .map(|s| Ok::<Bytes, LlmError>(Bytes::from_static(s.as_bytes())));
        Ok(Box::pin(stream::iter(iter)))
    }
}

// ---------------------------------------------------------------------------
// FakeChannel — records every StreamEvent::Text call so we can assert
// the planner relayed tokens as they arrived. Also holds a
// CancellationToken so `channel.cancellation_token()` returns something
// sensible.
// ---------------------------------------------------------------------------

struct FakeChannel {
    session: SessionId,
    token: CancellationToken,
    streamed_text: Mutex<Vec<String>>,
}

impl FakeChannel {
    fn new() -> Self {
        FakeChannel {
            session: SessionId::new(),
            token: CancellationToken::new(),
            streamed_text: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl ChannelContext for FakeChannel {
    fn channel_name(&self) -> &str {
        "e2e"
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
    async fn stream_event(&self, event: StreamEvent<'_>) -> Result<(), ChannelError> {
        if let StreamEvent::Text(chunk) = event {
            self.streamed_text.lock().unwrap().push(chunk.to_string());
        }
        Ok(())
    }
    async fn finalize(&self, _outcome: &TurnOutcome) -> Result<(), ChannelError> {
        Ok(())
    }
    fn cancellation_token(&self) -> CancellationToken {
        self.token.clone()
    }
}

// ---------------------------------------------------------------------------
// MemoryReadTool — a fake tool the LLM is allowed to call.
//
// On execute() it returns a deterministic JSON payload. The LLM's
// second SSE response is scripted to reference the payload in its final
// message, which proves the planner correctly serialized the tool
// output back into the conversation history.
// ---------------------------------------------------------------------------

struct MemoryReadTool {
    id: ToolId,
    schema: Value,
}

impl MemoryReadTool {
    fn new() -> Self {
        MemoryReadTool {
            id: ToolId::new(),
            schema: json!({"type": "object"}),
        }
    }
}

#[async_trait]
impl Tool for MemoryReadTool {
    fn id(&self) -> ToolId {
        self.id
    }
    fn name(&self) -> &str {
        "memory.read"
    }
    fn description(&self) -> &str {
        "read from memory"
    }
    fn input_schema(&self) -> &Value {
        &self.schema
    }
    fn required_scope(&self, _input: &Value) -> Scope {
        Scope::parse("memory.read").unwrap()
    }
    async fn execute(&self, _input: Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        ToolOutcome::Completed {
            output: json!({
                "items": ["buy milk", "call dentist"],
                "count": 2,
            }),
            verified: Verification::NotApplicable,
        }
    }
}

// ---------------------------------------------------------------------------
// SSE scripts: step 1 = tool-call, step 2 = final message.
//
// Both scripts are hand-rolled OpenAI-compatible Chat Completions
// streams (what llama-server, Jan, Lemonade and the broker speak). They
// split the tool-call arguments and the text across chunks so the test
// exercises the real parser + state machine, not a simplified happy
// path. (2.0: this test used the removed Anthropic provider.)
// ---------------------------------------------------------------------------

fn step1_tool_call_script() -> Vec<&'static str> {
    vec![
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_mem_01\",\"type\":\"function\",\"function\":{\"name\":\"memory.read\",\"arguments\":\"\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"{\\\"topic\\\":\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"todos\\\"}\"}}]},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}],\"usage\":{\"prompt_tokens\":42,\"completion_tokens\":14}}\n\n",
        "data: [DONE]\n\n",
    ]
}

fn step2_final_message_script() -> Vec<&'static str> {
    vec![
        "data: {\"choices\":[{\"delta\":{\"content\":\"You have 2 items: \"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"delta\":{\"content\":\"buy milk and call dentist.\"},\"finish_reason\":null}]}\n\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":80,\"completion_tokens\":11}}\n\n",
        "data: [DONE]\n\n",
    ]
}

// ---------------------------------------------------------------------------
// The test: two-step LLM turn end-to-end.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn llm_driven_turn_e2e_tool_call_then_final_message() {
    // Audit chain, wrapped in the real bridge (panic-on-error default).
    let chain = HmacChainLog::new(b"phase2-task5-e2e-key".to_vec());
    let bridge: Arc<AuditBridge<HmacChainLog>> = Arc::new(AuditBridge::new(chain));

    // Real OpenAI-compatible provider backed by a fake transport that replays
    // two scripted responses in order: tool-call, then final message.
    let transport = FakeTransport::new(vec![
        step1_tool_call_script(),
        step2_final_message_script(),
    ]);
    let provider: Arc<dyn LlmProvider> = Arc::new(OpenAiProvider::with_transport(
        OpenAiConfig::without_api_key().with_base_url("http://127.0.0.1:8080"),
        Box::new(transport),
    ));

    // Single tool the agent is allowed to call. The LLM's step-1
    // response references it by name ("memory.read").
    let tool = Arc::new(MemoryReadTool::new());
    let tool_id = tool.id();
    let registry = Arc::new(ToolRegistry::new(vec![tool as Arc<dyn Tool>]));

    // The agent holds the required capability; the trusted channel's
    // ceiling grants it through, so the tool call is executed (not
    // denied).
    let caps = CapabilitySet::from_scopes([Scope::parse("memory.read").unwrap()]);

    // Planner factory: one fresh LlmPlanner per turn, capturing a
    // clone of the provider Arc and the registry.
    let provider_for_factory = Arc::clone(&provider);
    let registry_for_factory = Arc::clone(&registry);
    let bridge_hook = Arc::clone(&bridge);

    let agent = ConcreteAgent::new(
        AgentId::new(),
        caps,
        registry,
        bridge_hook,
        move || {
            Box::new(LlmPlanner::new(
                Arc::clone(&provider_for_factory),
                Arc::clone(&registry_for_factory),
                LlmPlannerConfig::new("local-model")
                    .with_system_prompt("You are a terse assistant.")
                    .with_max_tokens(128),
            ))
        },
    );

    // Drive a turn.
    let channel = FakeChannel::new();
    let outcome = agent
        .turn(
            Message::text(channel.session, "what's on my todo list?"),
            &channel,
        )
        .await;

    // ---- Assertion 1: the turn completed with the LLM's final text.
    match outcome {
        TurnOutcome::Completed {
            ref final_message,
            tool_calls_made,
            ..
        } => {
            assert_eq!(tool_calls_made, 1, "exactly one tool call expected");
            assert_eq!(
                final_message, "You have 2 items: buy milk and call dentist.",
                "final message should be the reassembled step-2 text"
            );
        }
        other => panic!("expected Completed, got {other:?}"),
    }

    // ---- Assertion 2: the channel received the streamed text chunks
    //       from step 2 — proving the planner relayed TextChunk events
    //       as they arrived, not just the terminal value.
    let streamed = channel.streamed_text.lock().unwrap().clone();
    assert_eq!(
        streamed,
        vec![
            "You have 2 items: ".to_string(),
            "buy milk and call dentist.".to_string()
        ],
        "channel should have seen the two content chunks in order"
    );

    // ---- Assertion 3: the HMAC audit chain is intact and carries
    //       exactly the four events this LLM-backed turn should produce:
    //       TurnStarted → ToolCall → TurnEnded → LlmCost (Chapter K).
    let log = bridge.writer();
    assert_eq!(
        log.len(),
        4,
        "audit chain should have TurnStarted + ToolCall + TurnEnded + LlmCost"
    );
    log.verify()
        .expect("HMAC chain should verify end-to-end after a real LLM turn");

    match log.get(0).unwrap().event {
        AuditEvent::TurnStarted {
            channel: ChannelPlatform::Local,
            ..
        } => {}
        ref other => panic!("seq 0: expected TurnStarted on Local, got {other:?}"),
    }
    match log.get(1).unwrap().event {
        AuditEvent::ToolCall {
            tool_id: ref recorded_tool_id,
            ..
        } => {
            assert_eq!(
                *recorded_tool_id, tool_id,
                "audited tool_id should match the registry entry the LLM resolved"
            );
        }
        ref other => panic!("seq 1: expected ToolCall, got {other:?}"),
    }
    match log.get(2).unwrap().event {
        AuditEvent::TurnEnded {
            outcome: TurnOutcomeSummary::Completed,
            tool_calls_made: 1,
            ..
        } => {}
        ref other => panic!("seq 2: expected TurnEnded(Completed, 1), got {other:?}"),
    }
    // Chapter K — the LLM-backed turn also emits a priced-able LlmCost event
    // carrying the model the turn ran on.
    match log.get(3).unwrap().event {
        AuditEvent::LlmCost { ref model, .. } => {
            assert!(!model.is_empty(), "LlmCost must record the model");
        }
        ref other => panic!("seq 3: expected LlmCost, got {other:?}"),
    }
}
