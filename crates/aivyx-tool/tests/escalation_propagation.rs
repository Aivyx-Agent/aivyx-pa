//! Task 4 (HIGH, 2026-09-16 audit) — end-to-end proof that
//! `ToolOutcome::RequiresEscalation` from an out-of-process tool
//! genuinely reaches the daemon as an escalation, not a generic
//! failure.
//!
//! Drives the fix through every hop it previously got flattened at:
//!
//! 1. **Tool-process side** (`escalating_tool_fixture` binary, running
//!    the real `run_multi_tool_subprocess` dispatch loop): the tool's
//!    `ToolOutcome::RequiresEscalation` must serialize as
//!    `ToolToDaemon::RequiresEscalation`, not `ToolError`.
//! 2. **Wire + bridge** (`ToolProcessBridge`'s real reader loop): the
//!    frame must decode into `InvocationOutcome::RequiresEscalation`,
//!    not `InvocationOutcome::ToolError`.
//! 3. **Daemon side** (`ToolProxy::execute`): the outcome must map to
//!    `aivyx_core::ToolOutcome::RequiresEscalation`, not
//!    `ToolOutcome::Failed`.
//!
//! Same shape as `p12_equivalence.rs` (spawn a real compiled fixture
//! binary via `CARGO_BIN_EXE_*`, drive it through the real bridge +
//! proxy), applied to the escalation path instead of the happy path.

use std::sync::Arc;

use async_trait::async_trait;
use aivyx_capability::TrustTier;
use aivyx_core::{
    AgentId, AuditHook, AuditTag, CancellationToken, ChannelContext, ChannelError,
    ChannelPlatform, SessionId, StreamEvent, Tool, ToolContext, ToolOutcome, TurnId,
    TurnOutcome,
};
use aivyx_tool::{ToolProcessBridge, ToolProcessConfig, ToolProxy};

struct FakeChannel {
    session: SessionId,
    token: CancellationToken,
}

impl FakeChannel {
    fn new() -> Self {
        FakeChannel {
            session: SessionId::new(),
            token: CancellationToken::new(),
        }
    }
}

#[async_trait]
impl ChannelContext for FakeChannel {
    fn channel_name(&self) -> &str {
        "escalation-propagation-test"
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

struct NullAudit;
impl AuditHook for NullAudit {
    fn on_event(&self, _tag: AuditTag) {}
}

#[tokio::test]
async fn requires_escalation_from_a_tool_process_reaches_the_daemon_as_escalation_not_a_generic_failure(
) {
    let fixture_path = env!("CARGO_BIN_EXE_escalating_tool_fixture");

    let cfg = ToolProcessConfig {
        name: "escalating-tool-fixture".into(),
        command: fixture_path.to_string(),
        args: vec![],
        env: vec![],
        sandbox: None,
        notification_sink: None,
    };
    let bridge = ToolProcessBridge::spawn(cfg)
        .await
        .expect("fixture spawn");
    let bridge = Arc::new(bridge);

    let descriptor = bridge
        .descriptors()
        .first()
        .expect("fixture must register one tool")
        .clone();
    assert_eq!(descriptor.name, "escalating.fixture");

    let proxy = ToolProxy::new(
        Arc::clone(&bridge),
        descriptor.name.clone(),
        descriptor.description.clone(),
        descriptor.input_schema.clone(),
        &descriptor.required_scope,
    )
    .expect("descriptor.required_scope must parse");

    let channel = FakeChannel::new();
    let audit = NullAudit;
    let token = channel.cancellation_token();
    let ctx = ToolContext {
        agent_id: AgentId::new(),
        session_id: channel.session,
        turn_id: TurnId::new(),
        channel: &channel,
        audit: &audit,
        cancellation: &token,
        message_origin: aivyx_core::MessageOrigin::Operator,
    };

    let outcome = proxy.execute(serde_json::json!({}), &ctx).await;

    match outcome {
        ToolOutcome::RequiresEscalation { reason, scope } => {
            assert_eq!(reason, "fixture always escalates");
            // The daemon-side mapping deliberately does not fabricate a
            // scope — the turn loop (agent.rs, RN.3) is the sole
            // authoritative stamper, and this test drives `ToolProxy`
            // directly, below the turn loop.
            assert_eq!(scope, None);
        }
        other => panic!(
            "expected ToolOutcome::RequiresEscalation (the fix's whole point — \
             an out-of-process tool's escalation must survive the wire round \
             trip instead of flattening into a generic failure); got {other:?}"
        ),
    }
}
