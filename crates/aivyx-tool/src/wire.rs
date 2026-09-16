//! Wire-level message types for the Aivyx tool process protocol.
//!
//! This module is the authoritative source for the schema documented
//! in [`docs/TOOL_SDK.md`](../../../docs/TOOL_SDK.md). Frames are
//! length-prefixed JSON over the child's stdin (daemon-to-tool) and
//! stdout (tool-to-daemon), with the same framing as
//! `aivyx-channel::daemon_ipc` — see `crate::frame` for the I/O.
//!
//! Phase 49 — added in the foundation phase; v0 stability per
//! `PRODUCT.md` P11. New variants land additively; treat unknown
//! tags as ignore.

use serde::{Deserialize, Serialize};

/// Protocol version the daemon advertises in `ToolHello`. v0.1.
pub const TOOL_PROTOCOL_VERSION: &str = "0.1";

// ---------------------------------------------------------------------------
// Daemon → tool (sent on the child's stdin)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DaemonToTool {
    /// First frame after spawn. The tool replies with `ToolRegister`.
    ToolHello {
        protocol_version: String,
    },
    /// Invoke a registered tool. Tool replies with zero or more
    /// `ToolEvent`s followed by a terminal `ToolResult` or
    /// `ToolError`.
    InvokeTool {
        call_id: String,
        tool_name: String,
        input: serde_json::Value,
        turn_id: String,
    },
    /// Operator cancelled a turn — please wind down `call_id` and
    /// reply with `ToolError { code: "cancelled" }` promptly.
    CancelInvocation {
        call_id: String,
    },
    /// Daemon is shutting down. Exit cleanly within a few seconds
    /// or the daemon SIGKILLs the process group.
    ToolShutdown,
}

// ---------------------------------------------------------------------------
// Tool → daemon (sent on the child's stdout)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ToolToDaemon {
    /// Response to `ToolHello`. Lists every tool this process
    /// provides.
    ToolRegister {
        tool_process_name: String,
        tools: Vec<ToolDescriptor>,
    },
    /// Streaming progress mid-invocation. The daemon forwards
    /// these into the channel's `StreamEvent` surface so the
    /// operator sees progress.
    ToolEvent {
        call_id: String,
        event: ToolEventPayload,
    },
    /// Terminal — success. Lands directly in
    /// `ToolOutcome::Completed { verified, output }`.
    ToolResult {
        call_id: String,
        verified: Verification,
        output: serde_json::Value,
    },
    /// Terminal — failure.
    ToolError {
        call_id: String,
        code: String,
        message: String,
    },
    /// Terminal — Task 4 (HIGH, 2026-09-16 audit). The tool needs
    /// operator escalation before it can proceed (mirrors
    /// `aivyx_core::ToolOutcome::RequiresEscalation`). Previously
    /// flattened into `ToolError { code: "requires_escalation", .. }`,
    /// which meant an out-of-process tool's escalation request reached
    /// the daemon as a generic failure instead of the turn loop's real
    /// `TurnOutcome::Escalated` handling. `scope` is deliberately not
    /// carried on the wire: the daemon-side turn loop always overwrites
    /// it with the authoritative `required_scope` it just checked (see
    /// `ToolOutcome::RequiresEscalation`'s own doc — RN.3), so a tool
    /// process's opinion of its own scope would be discarded anyway.
    RequiresEscalation {
        call_id: String,
        reason: String,
    },
    /// Phase 191 — a tool process pushes a notification with no
    /// preceding `InvokeTool` (no `call_id`: this isn't a response
    /// to anything, it fires from the tool process's own background
    /// task, e.g. a health-check watcher's polling loop noticing a
    /// state change). The daemon capability-checks the sending tool
    /// process before honoring this — see `aivyx-tool`'s
    /// `NotificationSink` trait (Task 2) for the injection point;
    /// `aivyx-tool` itself has no opinion on scopes.
    DispatchNotification {
        target: String,
        message: String,
        subject: Option<String>,
    },
}

/// One entry in a `ToolRegister`. The daemon validates the
/// declared `required_scope` against the active role's envelope
/// at handshake time; rejected tools are excluded from the
/// registry (the tool process stays alive in case the rejection
/// was a typo).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDescriptor {
    pub name: String,
    pub description: String,
    /// JSON Schema for the input. The daemon validates input
    /// against this before dispatching — invalid input never
    /// reaches the tool process.
    pub input_schema: serde_json::Value,
    /// Capability scope this tool needs. Must parse via
    /// `aivyx_capability::Scope::parse`. New scope bases must
    /// be declared in `aivyx-capability` before they can be
    /// used here.
    pub required_scope: String,
}

/// Verification semantics — same enum shape as
/// `aivyx_core::Verification`, but flat (no `aivyx-core`
/// dependency on the wire). The bridge translates between
/// the two.
///
/// `Verified`      — tool queried the system and confirmed effect.
/// `Unverified`    — tool returned Ok but did not check.
/// `NotApplicable` — verification is not meaningful (read-only query).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verification {
    Verified,
    Unverified,
    NotApplicable,
}

/// Streaming progress payload. New `kind` values land additively;
/// adapters skip unknown kinds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ToolEventPayload {
    /// Status line for the operator render layer.
    Status { status: String },
    /// Partial output (for tools that stream their result body).
    OutputChunk { chunk: String },
    /// Free-form log entry. The daemon's log forwarder may surface
    /// these in operator logs.
    Log { level: String, message: String },
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip<T>(value: &T) -> T
    where
        T: Serialize + for<'a> Deserialize<'a> + PartialEq + std::fmt::Debug,
    {
        let json = serde_json::to_string(value).expect("serialize");
        let back: T = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(&back, value, "round-trip mismatch");
        back
    }

    #[test]
    fn daemon_to_tool_round_trips() {
        roundtrip(&DaemonToTool::ToolHello {
            protocol_version: "0.1".into(),
        });
        roundtrip(&DaemonToTool::InvokeTool {
            call_id: "c-1".into(),
            tool_name: "wordcount".into(),
            input: serde_json::json!({"text": "hello world"}),
            turn_id: "t-1".into(),
        });
        roundtrip(&DaemonToTool::CancelInvocation {
            call_id: "c-1".into(),
        });
        roundtrip(&DaemonToTool::ToolShutdown);
    }

    #[test]
    fn tool_to_daemon_round_trips() {
        roundtrip(&ToolToDaemon::ToolRegister {
            tool_process_name: "wordcount-server".into(),
            tools: vec![ToolDescriptor {
                name: "wordcount".into(),
                description: "Count words.".into(),
                input_schema: serde_json::json!({"type": "object"}),
                required_scope: "tool.wordcount".into(),
            }],
        });
        roundtrip(&ToolToDaemon::ToolEvent {
            call_id: "c-1".into(),
            event: ToolEventPayload::Status {
                status: "thinking".into(),
            },
        });
        roundtrip(&ToolToDaemon::ToolResult {
            call_id: "c-1".into(),
            verified: Verification::NotApplicable,
            output: serde_json::json!({"words": 2}),
        });
        roundtrip(&ToolToDaemon::ToolError {
            call_id: "c-1".into(),
            code: "internal".into(),
            message: "boom".into(),
        });
        // Task 4 (HIGH, 2026-09-16 audit).
        roundtrip(&ToolToDaemon::RequiresEscalation {
            call_id: "c-1".into(),
            reason: "operator approval needed".into(),
        });
    }

    #[test]
    fn dispatch_notification_round_trips() {
        roundtrip(&ToolToDaemon::DispatchNotification {
            target: "phone".into(),
            message: "watcher x went down".into(),
            subject: Some("Health alert".into()),
        });
        roundtrip(&ToolToDaemon::DispatchNotification {
            target: "phone".into(),
            message: "watcher x recovered".into(),
            subject: None,
        });
    }

    #[test]
    fn unknown_variants_fail_deserialize_as_expected() {
        // Unknown variants WILL fail to deserialize at the typed
        // layer — bridge code must use a serde_json::Value-first
        // decoder and skip unknown tags. This test pins the
        // expected behavior.
        let unknown =
            r#"{"type":"FutureUnknownVariant","weird":true}"#;
        let parsed: Result<DaemonToTool, _> = serde_json::from_str(unknown);
        assert!(parsed.is_err());
    }

    #[test]
    fn verification_round_trips() {
        for v in [
            Verification::Verified,
            Verification::Unverified,
            Verification::NotApplicable,
        ] {
            roundtrip(&v);
        }
    }
}
