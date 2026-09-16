//! `ToolProxy` — implements `aivyx_core::Tool` by delegating to a
//! [`ToolProcessBridge`].
//!
//! One proxy per registered tool. A single bridge may back many
//! proxies (one tool process can register many tools). The proxy
//! is what the daemon registers into `ToolRegistry`; from the turn
//! loop's perspective, it is indistinguishable from an in-tree tool.
//!
//! Phase 49 — Tool Process IPC Foundation (P12). Foundation phase
//! shipped the third-party path.
//!
//! Phase 50 — full-fidelity execute(): `ToolEvent` frames are now
//! relayed to the channel via `invoke_with_events`, and
//! mid-invocation cancellation is targeted via
//! `CancelInvocation { call_id }` using a caller-supplied id.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use aivyx_capability::Scope;
use aivyx_core::{
    AivyxError, StreamEvent, Tool, ToolContext, ToolId, ToolOutcome, Verification,
};

use crate::bridge::{InvocationOutcome, ToolProcessBridge};
use crate::wire::{ToolEventPayload, Verification as WireVerification};

/// Bridges one registered tool (one `ToolDescriptor`) onto the
/// `aivyx_core::Tool` trait.
pub struct ToolProxy {
    id: ToolId,
    /// Public name as the planner sees it — matches the tool
    /// process's `ToolDescriptor.name`.
    name: String,
    description: String,
    input_schema: Value,
    /// Pre-parsed `Scope` used for every invocation. Falls back to
    /// a fresh parse on each call if the daemon decided to honor
    /// an operator-supplied override. The Scope is captured at
    /// registration time so the planner does not have to re-parse
    /// on every dispatch.
    required_scope: Scope,
    /// Shared handle on the underlying bridge. The bridge owns the
    /// child process and the reader task; many proxies may share
    /// one bridge.
    bridge: Arc<ToolProcessBridge>,
}

impl ToolProxy {
    /// Construct a proxy from a descriptor + shared bridge.
    ///
    /// Returns `None` if `required_scope` does not parse — callers
    /// should log + skip the tool rather than crashing.
    pub fn new(
        bridge: Arc<ToolProcessBridge>,
        name: String,
        description: String,
        input_schema: Value,
        required_scope_str: &str,
    ) -> Option<Self> {
        let required_scope = Scope::parse(required_scope_str)?;
        Some(ToolProxy {
            id: ToolId::new(),
            name,
            description,
            input_schema,
            required_scope,
            bridge,
        })
    }

    /// Same as [`Self::new`] but takes an operator-supplied
    /// override scope. The caller is responsible for verifying
    /// the override is `is_granted_by(declared)` — the bridge's
    /// loader does that check.
    pub fn with_override_scope(
        bridge: Arc<ToolProcessBridge>,
        name: String,
        description: String,
        input_schema: Value,
        scope: Scope,
    ) -> Self {
        ToolProxy {
            id: ToolId::new(),
            name,
            description,
            input_schema,
            required_scope: scope,
            bridge,
        }
    }
}

#[async_trait]
impl Tool for ToolProxy {
    fn id(&self) -> ToolId {
        self.id
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        &self.description
    }
    fn input_schema(&self) -> &Value {
        &self.input_schema
    }
    fn required_scope(&self, _input: &Value) -> Scope {
        self.required_scope.clone()
    }

    // Chapter Bulwark — a tool-process tool is a third-party integration
    // (Gmail, Calendar, Contacts, Drive, Obsidian, the web-search toolkit, …).
    // Its output is external content the operator did not author, so it can
    // carry a prompt-injection payload (a hostile email, a poisoned search
    // result). Fence all of it as DATA, not instructions. (Harmless for the
    // handful of pure-compute toolkit tools — the agent still reads the value.)
    fn output_is_untrusted(&self) -> bool {
        true
    }

    // Task 4 (HIGH, 2026-09-16 audit) — a tool-process tool being
    // *configured* (the `[[tool_process]]` entry existing at all) is
    // NOT the same act as the operator reviewing and opting in a
    // specific destructive scope base. `aivyx_core::Tool`'s own trait
    // doc contract is explicit: third-party/OAuth integrations "stay
    // withheld unless a maintainer has explicitly reviewed the base and
    // opted it in" — this impl used to unconditionally return `true`
    // for every tool-process tool, which contradicted that contract for
    // every Gmail/Drive/Notion/Obsidian/N8N/Contacts/Calendar write,
    // send, delete, or archive tool. `scope_overrides` (the daemon's own
    // narrower-than-declared override mechanism) is still folded into
    // `required_scope()` by construction, so the floor grant reflects
    // whatever the operator actually authorized, not the tool's raw
    // declaration — but a withheld base stays withheld from the floor
    // even if an operator override still names it; the floor grant is a
    // *default*, not the only way to obtain the scope — a single-agent
    // role's own `capability_scopes` can still grant it explicitly.
    //
    // Team missions are a real exception, not covered by that escape
    // hatch: `aivyx-pa team run`'s `cli_lead_scopes` and the daemon's
    // `TeamRunDeps.lead_scopes` are both built entirely from this same
    // backcompat floor (`aivyx-cli/src/bin/aivyx.rs`'s
    // `backcompat_floor`), which `bind_lead_scopes` then uses as the
    // clamping ceiling for the whole team. So withholding a base from
    // the floor withholds it from every team mission too, with no
    // config knob to restore it there — a real, known limitation
    // (Task 4 security-audit fix), not an oversight to silently work
    // around by reading this comment as a promise it isn't.
    fn auto_grantable_in_backcompat_floor(&self) -> bool {
        !aivyx_capability::is_withheld_integration_base(self.required_scope.base())
    }

    async fn execute(&self, input: Value, context: &ToolContext<'_>) -> ToolOutcome {
        // The turn loop has already enforced the capability check
        // and the role allowlist before we get here. Our job is to
        // round-trip the invocation through the bridge and map the
        // result back into a ToolOutcome.

        // Phase 50 — short-circuit cancellation before we even
        // generate a call_id.
        if context.cancellation.is_cancelled() {
            return ToolOutcome::Failed(AivyxError::Cancelled);
        }

        let turn_id = context.turn_id.to_string();
        let bridge = Arc::clone(&self.bridge);
        let tool_name = self.name.clone();
        let tool_id = self.id;

        // Phase 50 — caller-supplied call_id so a targeted
        // CancelInvocation can be sent if the cancellation token
        // fires mid-invocation.
        let call_id = uuid::Uuid::new_v4().to_string();

        // Phase 50 — event relay shape.
        //
        // `ChannelContext::stream_event` is async; the bridge's
        // `on_event` callback is sync. We use a tokio mpsc as the
        // sync-to-async bridge, spawn the bridge invocation as
        // its own task (so the bridge can advance independently
        // of the proxy's drain loop), and drain events on the
        // proxy side.
        //
        // Spawning the invocation also cleanly decouples the
        // drain order from the bridge's progress: even if the
        // bridge has already produced the terminal outcome, any
        // events still buffered in the mpsc are drained before
        // the proxy returns. That's the property the conformance
        // test exercises.
        let (ev_tx, mut ev_rx) =
            tokio::sync::mpsc::unbounded_channel::<ToolEventPayload>();
        let on_event = move |ev: ToolEventPayload| {
            let _ = ev_tx.send(ev);
        };

        let bridge_for_invoke = Arc::clone(&bridge);
        let tool_name_for_invoke = tool_name.clone();
        let call_id_for_invoke = call_id.clone();
        let invoke_handle = tokio::spawn(async move {
            bridge_for_invoke
                .invoke_with_events(
                    &call_id_for_invoke,
                    &tool_name_for_invoke,
                    input,
                    &turn_id,
                    on_event,
                )
                .await
        });

        let channel = context.channel;
        let cancellation = context.cancellation;

        // Drain events and watch cancellation. Exit the loop when
        // `ev_rx.recv()` returns None (event sender dropped, which
        // happens when the spawned task completes and drops the
        // closure that owns ev_tx).
        let mut cancelled_flag = false;
        loop {
            tokio::select! {
                _ = cancellation.cancelled(), if !cancelled_flag => {
                    cancelled_flag = true;
                    let _ = bridge.cancel(&call_id).await;
                    // Don't return yet — keep draining so the tool's
                    // ToolError{code:"cancelled"} response gets through
                    // and the spawned task completes cleanly.
                }
                ev_opt = ev_rx.recv() => {
                    match ev_opt {
                        Some(ToolEventPayload::Status { status }) => {
                            let _ = channel
                                .stream_event(StreamEvent::Status(&status))
                                .await;
                        }
                        Some(ToolEventPayload::OutputChunk { chunk }) => {
                            let _ = channel
                                .stream_event(StreamEvent::ToolOutput {
                                    tool: tool_id,
                                    tool_name: &tool_name,
                                    chunk: &chunk,
                                })
                                .await;
                        }
                        Some(ToolEventPayload::Log { level, message }) => {
                            eprintln!(
                                "aivyx-pa tool {tool_name:?} [{level}]: {message}"
                            );
                        }
                        None => break,
                    }
                }
            }
        }

        if cancelled_flag {
            // Ensure the spawned task completes (it will, because
            // we sent CancelInvocation and the tool is contracted
            // to respond promptly). Drop the join handle's result.
            let _ = invoke_handle.await;
            return ToolOutcome::Failed(AivyxError::Cancelled);
        }

        let outcome = match invoke_handle.await {
            Ok(r) => r,
            Err(_) => {
                return ToolOutcome::Failed(AivyxError::Internal(format!(
                    "tool bridge task panicked for `{tool_name}`"
                )));
            }
        };

        match outcome {
            Ok(InvocationOutcome::Completed { verified, output }) => {
                ToolOutcome::Completed {
                    output,
                    verified: map_verification(verified),
                }
            }
            Ok(InvocationOutcome::ToolError { code, message }) => {
                ToolOutcome::Failed(AivyxError::Internal(format!(
                    "tool `{tool_name}` returned error [{code}]: {message}"
                )))
            }
            // Task 4 (HIGH, 2026-09-16 audit) — the other half of the
            // same flattening fix, on the daemon side: this used to be
            // unreachable (the tool-process side only ever emitted
            // `InvocationOutcome::ToolError { code: "requires_escalation",
            // .. }`). `scope: None` is correct, not a placeholder — the
            // turn loop (`aivyx-core/src/agent.rs`, RN.3) always
            // overwrites it with the authoritative `required_scope` it
            // just checked before dispatch, discarding any scope a tool
            // itself would have supplied.
            Ok(InvocationOutcome::RequiresEscalation { reason }) => {
                ToolOutcome::RequiresEscalation {
                    reason,
                    scope: None,
                }
            }
            Err(e) => ToolOutcome::Failed(AivyxError::Internal(format!(
                "tool bridge error for `{tool_name}`: {e}"
            ))),
        }
    }
}

fn map_verification(v: WireVerification) -> Verification {
    match v {
        WireVerification::Verified => Verification::Verified,
        WireVerification::Unverified => Verification::Unverified,
        WireVerification::NotApplicable => Verification::NotApplicable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::ToolProcessConfig;

    #[test]
    fn map_verification_covers_all_variants() {
        assert!(matches!(
            map_verification(WireVerification::Verified),
            Verification::Verified
        ));
        assert!(matches!(
            map_verification(WireVerification::Unverified),
            Verification::Unverified
        ));
        assert!(matches!(
            map_verification(WireVerification::NotApplicable),
            Verification::NotApplicable
        ));
    }

    // ---- Task 4 (HIGH, 2026-09-16 audit) — floor-grant scope gate ----

    /// Minimal inline Python tool process: completes the `ToolHello` →
    /// `ToolRegister` handshake (registering one throwaway `noop` tool
    /// with an irrelevant scope) and then blocks on its next read.
    /// `auto_grantable_in_backcompat_floor` never touches the bridge —
    /// only `ToolProxy::new`'s own `required_scope_str` argument — but
    /// building a `ToolProxy` at all requires a real, handshaked
    /// `ToolProcessBridge`, so a live subprocess is unavoidable here
    /// (same pattern `bridge.rs`'s own tests already use).
    const HANDSHAKE_ONLY_SCRIPT: &str = r#"
import sys, json, struct

def read_frame():
    hdr = sys.stdin.buffer.read(4)
    if not hdr or len(hdr) < 4:
        return None
    (n,) = struct.unpack(">I", hdr)
    return json.loads(sys.stdin.buffer.read(n).decode("utf-8"))

def write_frame(msg):
    body = json.dumps(msg).encode("utf-8")
    sys.stdout.buffer.write(struct.pack(">I", len(body)) + body)
    sys.stdout.buffer.flush()

hello = read_frame()
assert hello["type"] == "ToolHello"
write_frame({
    "type": "ToolRegister",
    "tool_process_name": "test-tool",
    "tools": [{
        "name": "noop",
        "description": "noop",
        "input_schema": {"type": "object"},
        "required_scope": "memory.read"
    }]
})
# Block until the parent drops the pipe (test end), then exit quietly.
read_frame()
"#;

    /// Build a `ToolProxy` whose `required_scope` is `scope_str`,
    /// backed by a live handshake-only tool process. Returns `None`
    /// (test should skip, not fail) when `python3` is unavailable —
    /// mirrors `bridge.rs`'s own conformance tests.
    async fn test_proxy_with_scope(scope_str: &str) -> Option<ToolProxy> {
        let config = ToolProcessConfig {
            name: "test".into(),
            command: "python3".into(),
            args: vec!["-c".into(), HANDSHAKE_ONLY_SCRIPT.into()],
            env: vec![],
            sandbox: None,
            notification_sink: None,
        };
        let bridge = match crate::bridge::ToolProcessBridge::spawn(config).await {
            Ok(b) => b,
            Err(e) => {
                eprintln!("skipping: python3 unavailable: {e}");
                return None;
            }
        };
        Some(
            ToolProxy::new(
                Arc::new(bridge),
                "test-tool".into(),
                "a tool for testing".into(),
                serde_json::json!({"type": "object"}),
                scope_str,
            )
            .expect("scope_str must parse"),
        )
    }

    #[tokio::test]
    async fn write_scoped_tool_proxy_is_not_auto_grantable_in_backcompat_floor() {
        let Some(proxy) = test_proxy_with_scope("email.send").await else {
            return;
        };
        assert!(!proxy.auto_grantable_in_backcompat_floor());
    }

    #[tokio::test]
    async fn read_scoped_tool_proxy_is_still_auto_grantable_in_backcompat_floor() {
        let Some(proxy) = test_proxy_with_scope("email.read").await else {
            return;
        };
        assert!(proxy.auto_grantable_in_backcompat_floor());
    }
}
