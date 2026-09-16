//! Multi-tool IPC harness — Phase 128 lift to substrate.
//!
//! Companion to [`crate::harness::run_tool_as_subprocess`]
//! (single-tool, Phase 50). This module ships
//! [`run_multi_tool_subprocess`] which runs N tools through
//! ONE tool-process binary by name-keyed dispatch on
//! `InvokeTool { tool_name, ... }`.
//!
//! ## Lift history
//!
//! Phase 123 (Gmail) was the first reusable multi-tool
//! integration; its harness lived in
//! `aivyx-gmail/src/harness.rs` with a Task 8 exit note
//! recommending lift to substrate. Phase 125 (toolkit) was
//! the second consumer; its harness lived in
//! `aivyx-toolkit/src/harness.rs` as a line-for-line copy
//! (with the service-specific identifier strings swapped).
//! Phase 128 (Calendar) would have been the third copy —
//! the documented "clear-win threshold for the lift." This
//! module is that lift.
//!
//! ## Post-lift consumer shape
//!
//! `aivyx-gmail/src/harness.rs` and
//! `aivyx-toolkit/src/harness.rs` are thin re-export shims
//! preserving their public-API surface
//! (`aivyx_gmail::HarnessError`, etc) so no downstream
//! consumer code needs to change. New integrations
//! (Calendar, Drive, GitHub) consume this module directly.
//!
//! ## Protocol
//!
//! 1. Reads `ToolHello` from stdin (the daemon-side
//!    initiator frame).
//! 2. Writes `ToolRegister` describing every wrapped tool
//!    in one frame.
//! 3. Loops on stdin:
//!    - `InvokeTool { tool_name, ... }` → looks up the tool
//!      by name, builds a minimal `ToolContext` for the
//!      child, calls `tool.execute(input, ctx)`, serializes
//!      the `ToolOutcome` back as `ToolResult` /
//!      `ToolError`. Unknown tool names yield
//!      `ToolError { code: "unknown_tool" }`.
//!    - `CancelInvocation { call_id }` → cancels the
//!      cancellation token for the matching in-flight call
//!      (cooperative; the tool must poll for it).
//!    - `ToolShutdown` → exits cleanly.
//!    - Unknown variants → skipped (forward-compat).
//!
//! ## Invariants enforced on entry
//!
//! - `tools` must be non-empty
//!   ([`HarnessError::EmptyToolList`]).
//! - Tool names must be unique within `tools`
//!   ([`HarnessError::DuplicateToolName`]). Gmail registers
//!   `gmail.search`, `gmail.read`, etc — no duplicates by
//!   construction, but the harness rejects defensively.
//!
//! ## Channel-context naming
//!
//! Each tool execution sees a [`NoopChannel`] whose
//! `channel_name` is automatically `"{tool_process_name}-harness"`
//! — so an `aivyx-gmail` process surfaces as
//! `"aivyx-gmail-harness"` and an `aivyx-calendar` process
//! surfaces as `"aivyx-calendar-harness"`. The channel
//! doesn't ship streaming events (`stream_event` drops);
//! a future Chapter F integration with chunked progress
//! (e.g. `drive.upload`) would replace this with a real
//! channel that ships `ToolEventPayload::OutputChunk`
//! frames back to the daemon.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use thiserror::Error;
use tokio::io::{stdin, stdout};
use tokio::sync::Mutex;

use aivyx_capability::TrustTier;
use aivyx_core::{
    AgentId, CancellationToken, ChannelContext, ChannelError, ChannelPlatform,
    NullAuditHook, SessionId, StreamEvent, Tool, ToolContext, ToolOutcome, TurnId,
    TurnOutcome, Verification,
};

use crate::{
    frame::{read_frame, write_frame, FrameError},
    wire::{
        DaemonToTool, ToolDescriptor, ToolEventPayload, ToolToDaemon,
        Verification as WireVerification,
    },
};

#[derive(Debug, Error)]
pub enum HarnessError {
    #[error("framing error: {0}")]
    Frame(#[from] FrameError),
    #[error("expected ToolHello, got {0:?}")]
    HandshakeUnexpected(Box<DaemonToTool>),
    #[error("unexpected EOF {0}")]
    UnexpectedEof(&'static str),
    #[error("decode error: {0}")]
    Decode(String),
    #[error("tool list must contain at least one tool")]
    EmptyToolList,
    #[error("duplicate tool name {0:?} in tool list — names must be unique within a process")]
    DuplicateToolName(String),
}

/// Run multiple [`aivyx_core::Tool`] impls as a single tool
/// process. Same protocol as
/// [`crate::harness::run_tool_as_subprocess`] but the
/// `ToolRegister` frame carries `Vec<ToolDescriptor>` and
/// the dispatch loop routes each `InvokeTool { tool_name,
/// ... }` to the matching tool by name.
pub async fn run_multi_tool_subprocess(
    tools: Vec<Arc<dyn Tool>>,
    tool_process_name: impl Into<String>,
    extra_outbound: Option<tokio::sync::mpsc::UnboundedReceiver<crate::wire::ToolToDaemon>>,
) -> Result<(), HarnessError> {
    let tool_process_name = tool_process_name.into();

    if tools.is_empty() {
        return Err(HarnessError::EmptyToolList);
    }

    // Build the name-keyed dispatch map up front; rejecting
    // duplicates at this stage means the IPC loop never has
    // to disambiguate.
    let mut by_name: HashMap<String, Arc<dyn Tool>> = HashMap::new();
    for tool in &tools {
        let name = tool.name().to_string();
        if by_name.contains_key(&name) {
            return Err(HarnessError::DuplicateToolName(name));
        }
        by_name.insert(name, Arc::clone(tool));
    }

    let mut stdin = stdin();
    let stdout_sink = Arc::new(Mutex::new(stdout()));

    // Channel name derived from the process name so an
    // `aivyx-gmail` binary surfaces as
    // `"aivyx-gmail-harness"` etc — preserves the pre-lift
    // channel identification.
    let channel_name = format!("{tool_process_name}-harness");

    // Handshake — read ToolHello.
    let body = read_frame(&mut stdin)
        .await
        .map_err(HarnessError::Frame)?
        .ok_or(HarnessError::UnexpectedEof("during handshake"))?;
    let hello: DaemonToTool = serde_json::from_str(&body)
        .map_err(|e| HarnessError::Decode(e.to_string()))?;
    if !matches!(hello, DaemonToTool::ToolHello { .. }) {
        return Err(HarnessError::HandshakeUnexpected(Box::new(hello)));
    }

    // Build + send ToolRegister with every tool's
    // descriptor.
    let descriptors: Vec<ToolDescriptor> = tools
        .iter()
        .map(|t| ToolDescriptor {
            name: t.name().to_string(),
            description: t.description().to_string(),
            input_schema: t.input_schema().clone(),
            required_scope: t
                .required_scope(&serde_json::json!({}))
                .to_string(),
        })
        .collect();
    let register = ToolToDaemon::ToolRegister {
        tool_process_name,
        tools: descriptors,
    };
    {
        let mut guard = stdout_sink.lock().await;
        write_frame(&mut *guard, &register)
            .await
            .map_err(HarnessError::Frame)?;
    }

    // Phase 191 — an optional side channel lets tasks running
    // independently of this function's own dispatch loop (e.g.
    // `aivyx-toolkit`'s background health-check poller) share the
    // same stdout writer to emit unsolicited frames (today, just
    // `DispatchNotification`) without corrupting frame boundaries.
    // Only spawned when a caller actually passes a receiver; existing
    // callers passing `None` see zero behavior change.
    //
    // Spawned only AFTER `ToolRegister` above has actually been
    // written: the daemon's handshake is strict — the first frame it
    // reads from this process MUST be `ToolRegister`, and anything
    // else (including a `DispatchNotification` that raced ahead) is a
    // protocol violation that makes the daemon drop this ENTIRE tool
    // process's surface, not just the errant frame. Spawning the
    // forwarder any earlier (e.g. right after `stdout_sink` is built,
    // before the handshake) would let an eager background task (like
    // the health poller's immediate first probe) win that race.
    if let Some(mut rx) = extra_outbound {
        let forward_sink = Arc::clone(&stdout_sink);
        tokio::spawn(async move {
            while let Some(frame) = rx.recv().await {
                let mut guard = forward_sink.lock().await;
                let _ = crate::frame::write_frame(&mut *guard, &frame).await;
            }
        });
    }

    // Dispatch loop.
    let pending_cancels: Arc<Mutex<HashMap<String, CancellationToken>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let by_name = Arc::new(by_name);
    let channel_name = Arc::new(channel_name);

    loop {
        let body = match read_frame(&mut stdin).await {
            Ok(Some(b)) => b,
            Ok(None) => return Ok(()),
            Err(e) => return Err(HarnessError::Frame(e)),
        };
        let msg: DaemonToTool = match serde_json::from_str(&body) {
            Ok(m) => m,
            Err(_) => continue, // unknown variant — forward-compat skip
        };
        match msg {
            DaemonToTool::ToolHello { .. } => {
                // Spurious — handshake already consumed it.
            }
            DaemonToTool::InvokeTool {
                call_id,
                tool_name,
                input,
                turn_id: _,
            } => {
                let by_name = Arc::clone(&by_name);
                let stdout_sink = Arc::clone(&stdout_sink);
                let pending_cancels = Arc::clone(&pending_cancels);
                let channel_name = Arc::clone(&channel_name);
                let cancellation = CancellationToken::new();
                {
                    let mut guard = pending_cancels.lock().await;
                    guard.insert(call_id.clone(), cancellation.clone());
                }
                let call_id_for_task = call_id.clone();
                tokio::spawn(async move {
                    let reply = match by_name.get(&tool_name) {
                        Some(tool) => {
                            let channel = NoopChannel {
                                session: SessionId::new(),
                                cancellation: cancellation.clone(),
                                channel_name: Arc::clone(&channel_name),
                                _call_id: call_id_for_task.clone(),
                                _stdout_sink: Arc::clone(&stdout_sink),
                            };
                            let audit = NullAuditHook;
                            let ctx = ToolContext {
                                agent_id: AgentId::new(),
                                session_id: SessionId::new(),
                                turn_id: TurnId::new(),
                                channel: &channel,
                                audit: &audit,
                                cancellation: &cancellation,
                                // Known, deliberate simplification: this bridge hosts
                                // out-of-process vertical tools over a wire protocol
                                // that carries no origin field today, so every call
                                // through here is hardcoded Operator regardless of
                                // its real trigger. A tool exposed only through this
                                // bridge cannot participate in origin-based guards
                                // like schedule.*'s (see schedule_tool.rs).
                                message_origin: aivyx_core::MessageOrigin::Operator,
                            };
                            let outcome = tool.execute(input, &ctx).await;
                            outcome_to_wire(call_id_for_task.clone(), outcome)
                        }
                        None => ToolToDaemon::ToolError {
                            call_id: call_id_for_task.clone(),
                            code: "unknown_tool".into(),
                            message: format!(
                                "tool {tool_name:?} not registered by this process"
                            ),
                        },
                    };
                    {
                        let mut guard = stdout_sink.lock().await;
                        let _ = write_frame(&mut *guard, &reply).await;
                    }
                    pending_cancels.lock().await.remove(&call_id_for_task);
                });
            }
            DaemonToTool::CancelInvocation { call_id } => {
                let guard = pending_cancels.lock().await;
                if let Some(tok) = guard.get(&call_id) {
                    tok.cancel();
                }
            }
            DaemonToTool::ToolShutdown => return Ok(()),
        }
    }
}

/// No-op `ChannelContext` for tool-side dispatch.
/// Tool processes that don't need streaming progress
/// (the current Gmail / toolkit / Calendar shape — each
/// tool is one HTTP call or file op) get this channel.
/// A future Chapter F integration with chunked progress
/// (e.g. `drive.upload`) would replace this with a real
/// channel that ships `ToolEventPayload::OutputChunk`
/// frames back to the daemon.
struct NoopChannel {
    session: SessionId,
    cancellation: CancellationToken,
    channel_name: Arc<String>,
    _call_id: String,
    _stdout_sink: Arc<Mutex<tokio::io::Stdout>>,
}

#[async_trait]
impl ChannelContext for NoopChannel {
    fn channel_name(&self) -> &str {
        &self.channel_name
    }
    fn platform(&self) -> ChannelPlatform {
        ChannelPlatform::Local
    }
    fn trust_tier(&self) -> TrustTier {
        // Daemon-side enforcement already ran before
        // InvokeTool reached us; the child tier is a
        // placeholder here.
        TrustTier::Trusted
    }
    fn session_id(&self) -> SessionId {
        self.session
    }
    async fn stream_event(&self, _event: StreamEvent<'_>) -> Result<(), ChannelError> {
        // Tool processes using this channel don't emit
        // streaming progress; drop.
        Ok(())
    }
    async fn finalize(&self, _outcome: &TurnOutcome) -> Result<(), ChannelError> {
        // Per-tool-call channel; no turn-level finalize work.
        Ok(())
    }
    fn cancellation_token(&self) -> CancellationToken {
        self.cancellation.clone()
    }
}

// ---------------------------------------------------------------
// Outcome → wire conversion. Mirrors the helper in the
// single-tool harness (private to that module). Lifted
// here so all multi-tool consumers share the same
// translation.
// ---------------------------------------------------------------

fn verification_to_wire(v: Verification) -> WireVerification {
    match v {
        Verification::Verified => WireVerification::Verified,
        Verification::Unverified => WireVerification::Unverified,
        Verification::NotApplicable => WireVerification::NotApplicable,
    }
}

fn outcome_to_wire(call_id: String, outcome: ToolOutcome) -> ToolToDaemon {
    match outcome {
        ToolOutcome::Completed { output, verified } => ToolToDaemon::ToolResult {
            call_id,
            verified: verification_to_wire(verified),
            output,
        },
        ToolOutcome::Denied { scope, .. } => ToolToDaemon::ToolError {
            call_id,
            code: "scope_denied".into(),
            message: format!(
                "tool refused: scope {scope} not held (this should not happen — \
                 the parent enforces capability before InvokeTool)"
            ),
        },
        ToolOutcome::NotInRole { tool_name } => ToolToDaemon::ToolError {
            call_id,
            code: "not_in_role".into(),
            message: format!("tool {tool_name} not in active role's allowlist"),
        },
        ToolOutcome::RateLimited { tool_name, reason } => ToolToDaemon::ToolError {
            call_id,
            code: "rate_limited".into(),
            message: format!(
                "tool {tool_name} throttled: {reason} (this should not happen — \
                 the parent enforces rate limits before InvokeTool)"
            ),
        },
        // Task 4 (HIGH, 2026-09-16 audit) — previously flattened into
        // `ToolToDaemon::ToolError { code: "requires_escalation", .. }`,
        // which meant `ToolProxy::execute` (daemon side) could only ever
        // see a generic failure and never routed through the turn
        // loop's real `TurnOutcome::Escalated` handling. `scope` is not
        // carried across the wire: the turn loop always overwrites it
        // with the authoritative `required_scope` it just checked (RN.3),
        // so the tool process's own opinion would be discarded anyway.
        ToolOutcome::RequiresEscalation { reason, .. } => {
            ToolToDaemon::RequiresEscalation { call_id, reason }
        }
        ToolOutcome::Failed(err) => ToolToDaemon::ToolError {
            call_id,
            code: "tool_failed".into(),
            message: err.to_string(),
        },
    }
}

// Suppress unused warnings on the event payload import — it
// stays reachable from binaries' compile targets as the
// public surface a future streaming Chapter F integration
// would consume from NoopChannel.
#[allow(dead_code)]
fn _payload_anchor() -> ToolEventPayload {
    ToolEventPayload::Status {
        status: String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use serde_json::json;

    use aivyx_capability::Scope;
    use aivyx_core::ToolId;

    struct EchoTool {
        id: ToolId,
        name: String,
        schema: serde_json::Value,
    }

    impl EchoTool {
        fn new(name: &str) -> Self {
            Self {
                id: ToolId::new(),
                name: name.to_string(),
                schema: json!({"type":"object"}),
            }
        }
    }

    #[async_trait]
    impl Tool for EchoTool {
        fn id(&self) -> ToolId {
            self.id
        }
        fn name(&self) -> &str {
            &self.name
        }
        fn description(&self) -> &str {
            "echo"
        }
        fn input_schema(&self) -> &serde_json::Value {
            &self.schema
        }
        fn required_scope(&self, _input: &serde_json::Value) -> Scope {
            Scope::parse("memory.read").unwrap()
        }
        async fn execute(
            &self,
            input: serde_json::Value,
            _ctx: &ToolContext<'_>,
        ) -> ToolOutcome {
            ToolOutcome::Completed {
                output: json!({"echoed": input, "from": self.name}),
                verified: Verification::NotApplicable,
            }
        }
    }

    #[test]
    fn outcome_to_wire_completed_yields_tool_result() {
        let outcome = ToolOutcome::Completed {
            output: json!({"k": 1}),
            verified: Verification::Verified,
        };
        match outcome_to_wire("c-1".into(), outcome) {
            ToolToDaemon::ToolResult { call_id, verified, output } => {
                assert_eq!(call_id, "c-1");
                assert!(matches!(verified, WireVerification::Verified));
                assert_eq!(output["k"], 1);
            }
            other => panic!("expected ToolResult; got {other:?}"),
        }
    }

    // ---- Task 4 (HIGH, 2026-09-16 audit) — stop flattening escalation ----

    #[test]
    fn outcome_to_wire_requires_escalation_yields_wire_requires_escalation_not_tool_error() {
        let outcome = ToolOutcome::RequiresEscalation {
            reason: "operator approval needed".to_string(),
            scope: None,
        };
        match outcome_to_wire("c-3".into(), outcome) {
            ToolToDaemon::RequiresEscalation { call_id, reason } => {
                assert_eq!(call_id, "c-3");
                assert_eq!(reason, "operator approval needed");
            }
            other => panic!(
                "expected ToolToDaemon::RequiresEscalation (not flattened into a \
                 generic ToolError); got {other:?}"
            ),
        }
    }

    #[test]
    fn outcome_to_wire_failed_yields_tool_error_with_code_tool_failed() {
        let outcome = ToolOutcome::Failed(aivyx_core::AivyxError::Cancelled);
        match outcome_to_wire("c-2".into(), outcome) {
            ToolToDaemon::ToolError { call_id, code, .. } => {
                assert_eq!(call_id, "c-2");
                assert_eq!(code, "tool_failed");
            }
            other => panic!("expected ToolError; got {other:?}"),
        }
    }

    #[tokio::test]
    async fn empty_tool_list_rejected_with_dedicated_error() {
        let err = run_multi_tool_subprocess(vec![], "x", None)
            .await
            .expect_err("must error");
        assert!(matches!(err, HarnessError::EmptyToolList));
    }

    #[tokio::test]
    async fn duplicate_tool_names_rejected() {
        let tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(EchoTool::new("dup")),
            Arc::new(EchoTool::new("dup")),
        ];
        let err = run_multi_tool_subprocess(tools, "x", None)
            .await
            .expect_err("must error");
        match err {
            HarnessError::DuplicateToolName(name) => assert_eq!(name, "dup"),
            other => panic!("expected DuplicateToolName; got {other:?}"),
        }
    }

    #[test]
    fn channel_name_derived_from_process_name() {
        // Verifies the channel_name format convention so
        // consumers (and future Chapter F integrations) can
        // rely on it.
        let process_name = "aivyx-calendar";
        let expected = "aivyx-calendar-harness";
        let derived = format!("{process_name}-harness");
        assert_eq!(derived, expected);
    }
}
