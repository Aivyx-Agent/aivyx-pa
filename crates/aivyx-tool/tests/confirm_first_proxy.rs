//! Operator confirmation across the tool-process bridge: a bridged
//! confirm-first tool can't be self-confirmed. The child sees a fresh
//! session/turn per call, so `ToolProxy` enforces it — `confirmed: true`
//! only reaches the child after a refusal in an EARLIER turn of the same
//! session.

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
        "confirm-first-proxy-test"
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
async fn a_bridged_confirm_first_tool_needs_a_refusal_in_an_earlier_turn() {
    let cfg = ToolProcessConfig {
        name: "confirm-first-tool-fixture".into(),
        command: env!("CARGO_BIN_EXE_confirm_first_tool_fixture").to_string(),
        args: vec![],
        env: vec![],
        sandbox: None,
        notification_sink: None,
    };
    let bridge = Arc::new(ToolProcessBridge::spawn(cfg).await.expect("fixture spawn"));
    let d = bridge.descriptors().first().expect("one tool").clone();
    let proxy = ToolProxy::new(
        Arc::clone(&bridge),
        d.name.clone(),
        d.description.clone(),
        d.input_schema.clone(),
        &d.required_scope,
    )
    .expect("scope parses");

    let channel = FakeChannel::new();
    let audit = NullAudit;
    let token = channel.cancellation_token();
    let ctx_in = |turn: TurnId| ToolContext {
        agent_id: AgentId::new(),
        session_id: channel.session,
        turn_id: turn,
        channel: &channel,
        audit: &audit,
        cancellation: &token,
        message_origin: aivyx_core::MessageOrigin::Operator,
    };
    let confirmed = serde_json::json!({ "target": "PO-7", "confirmed": true });

    // Self-confirmed on the first call: the child sees it unconfirmed.
    let asked = TurnId::new();
    let first = proxy.execute(confirmed.clone(), &ctx_in(asked)).await;
    assert!(matches!(first, ToolOutcome::RequiresEscalation { .. }), "{first:?}");
    // Again in the same turn: still not the operator's answer.
    let again = proxy.execute(confirmed.clone(), &ctx_in(asked)).await;
    assert!(matches!(again, ToolOutcome::RequiresEscalation { .. }), "{again:?}");
    // The operator's next turn: it runs.
    let later = proxy.execute(confirmed, &ctx_in(TurnId::new())).await;
    assert!(matches!(later, ToolOutcome::Completed { .. }), "{later:?}");
}
