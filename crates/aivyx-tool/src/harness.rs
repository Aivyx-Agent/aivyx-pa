//! `run_tool_as_subprocess` — generic tool-process harness.
//!
//! Wraps any [`aivyx_core::Tool`] implementation as a tool-process
//! binary that speaks the protocol documented in
//! [`docs/TOOL_SDK.md`](../../../docs/TOOL_SDK.md). The load-bearing
//! deliverable for the **PRODUCT.md P12 "extractable without
//! rewriting"** property — any first-party `Tool` impl can be served
//! out-of-process by passing it to this function and shipping the
//! resulting binary.
//!
//! Phase 50 — Tool Process IPC Foundation closeout.
//!
//! ## What the harness does
//!
//! 1. Reads `ToolHello` from stdin.
//! 2. Writes `ToolRegister` describing the wrapped tool.
//! 3. Loops on stdin:
//!    - `InvokeTool` → builds a minimal `ToolContext` for the
//!      child, calls `tool.execute(input, ctx)`, serializes the
//!      `ToolOutcome` back as `ToolResult` / `ToolError`.
//!    - `CancelInvocation` → cancels the cancellation token for
//!      the matching call (best-effort; cooperative).
//!    - `ToolShutdown` → exits cleanly.
//!    - Unknown variants → skipped (forward-compat).
//!
//! ## What's synthesized in the child's `ToolContext`
//!
//! - `channel`: a [`HarnessChannel`] that buffers `StreamEvent`s
//!   the tool emits and translates them into `ToolEventPayload`
//!   frames sent on stdout.
//! - `audit`: a no-op [`NullAuditHook`]. The *parent* writes the
//!   `AuditEvent::ToolCall` row when it sees the terminal
//!   `ToolResult` / `ToolError`.
//! - `cancellation`: a fresh per-call `CancellationToken`.
//! - `agent_id`, `session_id`, `turn_id`: fresh per-call IDs.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::io::{stdin, stdout};
use tokio::sync::Mutex;

use aivyx_core::{
    AgentId, CancellationToken, ChannelContext, ChannelError, ChannelPlatform,
    NullAuditHook, SessionId, StreamEvent, Tool, ToolContext, ToolOutcome, TurnId,
    TurnOutcome, Verification,
};

use crate::frame::{read_frame, write_frame};
use crate::wire::{
    DaemonToTool, ToolDescriptor, ToolEventPayload, ToolToDaemon,
    Verification as WireVerification,
};

/// Synchronous `ChannelContext` that buffers events from the
/// in-child tool and ships them as `ToolEvent` frames on the
/// process's stdout. One instance per invocation.
struct HarnessChannel {
    session: SessionId,
    cancellation: CancellationToken,
    call_id: String,
    stdout_sink: Arc<Mutex<tokio::io::Stdout>>,
}

#[async_trait]
impl ChannelContext for HarnessChannel {
    fn channel_name(&self) -> &str {
        "aivyx-tool-harness"
    }
    fn platform(&self) -> ChannelPlatform {
        ChannelPlatform::Local
    }
    fn trust_tier(&self) -> aivyx_capability::TrustTier {
        // The harness inherits the parent daemon's effective tier;
        // returning Trusted is a placeholder — the child does not
        // enforce capability decisions. Scope checks happened on
        // the parent before InvokeTool was sent.
        aivyx_capability::TrustTier::Trusted
    }
    fn session_id(&self) -> SessionId {
        self.session
    }

    async fn stream_event(&self, event: StreamEvent<'_>) -> Result<(), ChannelError> {
        let payload = match event {
            StreamEvent::Status(status) => ToolEventPayload::Status {
                status: status.to_string(),
            },
            StreamEvent::ToolOutput { chunk, .. } => ToolEventPayload::OutputChunk {
                chunk: chunk.to_string(),
            },
            // Other StreamEvent variants are dropped silently.
            _ => return Ok(()),
        };
        let frame = ToolToDaemon::ToolEvent {
            call_id: self.call_id.clone(),
            event: payload,
        };
        let mut guard = self.stdout_sink.lock().await;
        let _ = write_frame(&mut *guard, &frame).await;
        Ok(())
    }

    async fn finalize(&self, _outcome: &TurnOutcome) -> Result<(), ChannelError> {
        Ok(())
    }

    fn cancellation_token(&self) -> CancellationToken {
        self.cancellation.clone()
    }
}

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
        // Task 4 (HIGH, 2026-09-16 audit) — see `multi_harness.rs`'s
        // identical `outcome_to_wire` for the full rationale: this used
        // to flatten into a generic `ToolError`, so the daemon-side
        // `ToolProxy::execute` could never see a real escalation.
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

/// Run an `aivyx_core::Tool` as a tool process.
///
/// Reads `ToolHello`, sends `ToolRegister` for the tool, then
/// loops on stdin until `ToolShutdown` or EOF.
///
/// This is the canonical proof of the P12 "extractable without
/// rewriting" property: take any `Tool` impl, wrap it in this
/// function, ship as a binary, and the parent daemon's
/// `ToolProcessBridge` + `ToolProxy` route invocations through it
/// identically to in-process execution.
pub async fn run_tool_as_subprocess<T>(
    tool: T,
    tool_process_name: impl Into<String>,
) -> Result<(), HarnessError>
where
    T: Tool + 'static,
{
    let mut stdin = stdin();
    let stdout_sink = Arc::new(Mutex::new(stdout()));

    // Handshake.
    let body = read_frame(&mut stdin)
        .await
        .map_err(HarnessError::Frame)?
        .ok_or(HarnessError::UnexpectedEof("during handshake"))?;
    let hello: DaemonToTool =
        serde_json::from_str(&body).map_err(|e| HarnessError::Decode(e.to_string()))?;
    if !matches!(hello, DaemonToTool::ToolHello { .. }) {
        return Err(HarnessError::HandshakeUnexpected(Box::new(hello)));
    }

    let descriptor = ToolDescriptor {
        name: tool.name().to_string(),
        description: tool.description().to_string(),
        input_schema: tool.input_schema().clone(),
        // R1: required_scope takes input; at handshake time we
        // don't have one. Use an empty object as the
        // representative input. Operators tighten via
        // [tool_process.scope_overrides] on the parent side.
        required_scope: tool.required_scope(&serde_json::json!({})).to_string(),
    };

    let register = ToolToDaemon::ToolRegister {
        tool_process_name: tool_process_name.into(),
        tools: vec![descriptor],
    };
    {
        let mut guard = stdout_sink.lock().await;
        write_frame(&mut *guard, &register)
            .await
            .map_err(HarnessError::Frame)?;
    }

    let tool = Arc::new(tool);
    let pending_cancels: Arc<Mutex<HashMap<String, CancellationToken>>> =
        Arc::new(Mutex::new(HashMap::new()));

    loop {
        let body = match read_frame(&mut stdin).await {
            Ok(Some(b)) => b,
            Ok(None) => return Ok(()),
            Err(e) => return Err(HarnessError::Frame(e)),
        };
        let msg: DaemonToTool = match serde_json::from_str(&body) {
            Ok(m) => m,
            Err(_) => continue,
        };

        match msg {
            DaemonToTool::ToolHello { .. } => {
                // Spurious — handshake already consumed it.
            }
            DaemonToTool::InvokeTool {
                call_id,
                tool_name: _,
                input,
                turn_id: _,
            } => {
                let tool = Arc::clone(&tool);
                let cancellation = CancellationToken::new();
                {
                    let mut guard = pending_cancels.lock().await;
                    guard.insert(call_id.clone(), cancellation.clone());
                }
                let pending_cancels = Arc::clone(&pending_cancels);
                let stdout_sink = Arc::clone(&stdout_sink);
                let call_id_for_task = call_id.clone();
                tokio::spawn(async move {
                    let channel = HarnessChannel {
                        session: SessionId::new(),
                        cancellation: cancellation.clone(),
                        call_id: call_id_for_task.clone(),
                        stdout_sink: Arc::clone(&stdout_sink),
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
                    let reply = outcome_to_wire(call_id_for_task.clone(), outcome);
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

/// Errors the harness surfaces.
#[derive(Debug, thiserror::Error)]
pub enum HarnessError {
    #[error("framing error: {0}")]
    Frame(#[from] crate::frame::FrameError),
    #[error("expected ToolHello, got {0:?}")]
    HandshakeUnexpected(Box<DaemonToTool>),
    #[error("unexpected EOF {0}")]
    UnexpectedEof(&'static str),
    #[error("decode error: {0}")]
    Decode(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_capability::{CapabilitySet, Scope};

    #[test]
    fn verification_round_trips() {
        for v in [
            Verification::Verified,
            Verification::Unverified,
            Verification::NotApplicable,
        ] {
            let wire = verification_to_wire(v);
            let json = serde_json::to_string(&wire).unwrap();
            assert!(!json.is_empty());
        }
    }

    #[test]
    fn outcome_completed_maps_to_tool_result() {
        let outcome = ToolOutcome::Completed {
            output: serde_json::json!({"ok": true}),
            verified: Verification::Verified,
        };
        let wire = outcome_to_wire("c-1".into(), outcome);
        match wire {
            ToolToDaemon::ToolResult { call_id, verified, output } => {
                assert_eq!(call_id, "c-1");
                assert!(matches!(verified, WireVerification::Verified));
                assert_eq!(output["ok"], true);
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[test]
    fn outcome_failed_maps_to_tool_error() {
        let outcome = ToolOutcome::Failed(aivyx_core::AivyxError::Cancelled);
        let wire = outcome_to_wire("c-2".into(), outcome);
        match wire {
            ToolToDaemon::ToolError { call_id, code, message } => {
                assert_eq!(call_id, "c-2");
                assert_eq!(code, "tool_failed");
                assert!(message.contains("cancel"));
            }
            other => panic!("expected ToolError, got {other:?}"),
        }
    }

    #[test]
    fn outcome_denied_maps_to_scope_denied_error() {
        let scope = Scope::parse("memory.read").unwrap();
        let outcome = ToolOutcome::Denied {
            scope,
            held: CapabilitySet::empty(),
        };
        let wire = outcome_to_wire("c-3".into(), outcome);
        match wire {
            ToolToDaemon::ToolError { code, .. } => {
                assert_eq!(code, "scope_denied");
            }
            other => panic!("expected ToolError, got {other:?}"),
        }
    }
}
