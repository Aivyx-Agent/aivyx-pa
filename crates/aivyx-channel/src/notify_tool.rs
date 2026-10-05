//! Phase 62 Task 7 — `NotifySendTool` (agent-facing notify.send).
//!
//! The infrastructure tool the agent calls to push a message to
//! an operator-configured `[[notify_target]]`. Follows the
//! `OnceLock`-factory pattern from `MissionCreateTool` (Phase 21)
//! and `RoleSwitchTool` (Phase 14): the tool registers in the
//! `ToolRegistry` with an empty dispatcher slot, and the
//! binary's startup path fills the slot via `set_dispatcher`
//! after constructing the `NotifyDispatcher` (Phase 62 Task 4)
//! and registering per-target backends (Tasks 5, 6).
//!
//! ## Input schema (Q3(b) at sign-off)
//!
//! ```json
//! {
//!   "target":  "phone",
//!   "message": "Build failed — see logs",
//!   "subject": "CI alert"
//! }
//! ```
//!
//! `subject` is optional. The dispatcher routes by `target`
//! (string lookup); the named backend renders the
//! subject/message according to its own conventions (Telegram
//! prepends bold; webhook serializes both fields explicitly).
//!
//! ## Output shape (Q4(a) at sign-off)
//!
//! **Success:**
//! ```json
//! { "success": true, "target": "phone",
//!   "delivered_at": "2026-05-13T14:30:00Z" }
//! ```
//!
//! **Delivery failure:**
//! ```json
//! { "success": false, "target": "phone",
//!   "error_kind": "rejected", "error_message": "HTTP 429" }
//! ```
//!
//! In both cases the [`ToolOutcome`] is `Completed` — the tool
//! *call* succeeded; the agent reads the structured output and
//! decides retry/escalate/give-up. `ToolOutcome::Failed` is
//! reserved for cases where the tool itself is unusable: the
//! dispatcher slot is unset (programming error) or the input
//! lacks a required field (malformed call).

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use chrono::Utc;
use serde_json::{json, Value};

use aivyx_capability::Scope;
use aivyx_core::{AivyxError, Tool, ToolContext, ToolId, ToolOutcome, Verification};

use crate::notify_dispatcher::{NotifyDispatcher, NotifyError};

pub struct NotifySendTool {
    id: ToolId,
    schema: Value,
    dispatcher: OnceLock<Arc<NotifyDispatcher>>,
}

impl std::fmt::Debug for NotifySendTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NotifySendTool")
            .field("id", &self.id)
            .field("has_dispatcher", &self.dispatcher.get().is_some())
            .finish()
    }
}

impl Default for NotifySendTool {
    fn default() -> Self {
        Self::new()
    }
}

impl NotifySendTool {
    pub fn new() -> Self {
        Self {
            id: ToolId::new(),
            schema: notify_send_input_schema(),
            dispatcher: OnceLock::new(),
        }
    }

    /// Fill the dispatcher slot. Called once at daemon startup
    /// after the `NotifyDispatcher` is built and all per-target
    /// backends are registered. Subsequent calls are no-ops
    /// (per `OnceLock::set` semantics) and return the unused
    /// dispatcher to the caller — the daemon should treat that
    /// as a programming error.
    pub fn set_dispatcher(
        &self,
        dispatcher: Arc<NotifyDispatcher>,
    ) -> Result<(), Arc<NotifyDispatcher>> {
        self.dispatcher.set(dispatcher)
    }
}

#[async_trait]
impl Tool for NotifySendTool {
    fn id(&self) -> ToolId {
        self.id
    }

    fn name(&self) -> &str {
        "notify.send"
    }

    fn description(&self) -> &str {
        "Push a notification message to an operator-configured \
         target (Telegram chat, generic webhook, or email). Input is a \
         JSON object with `target` (the name of a configured \
         notify_target), `message` (the body to send), and an \
         optional `subject`. Returns a JSON result with `success` \
         (bool) and on failure `error_kind` + `error_message` so \
         the agent can decide whether to retry. Use this when you \
         have something to surface to the operator outside the \
         current session — scheduled summaries, completed \
         long-running tasks, escalations."
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn required_scope(&self, input: &Value) -> Scope {
        // Build the qualified scope `notify.send:<target>` when
        // the input names a target, falling back to unqualified
        // `notify.send` when the input is malformed. The
        // capability layer's Rule 2 grants the qualified form to
        // a holder of the unqualified form, so an agent with
        // `notify.send` in its envelope can call any target;
        // narrow envelopes can use `notify.send:phone` to gate
        // by name.
        let target = input.get("target").and_then(|v| v.as_str()).unwrap_or("");
        let raw = if target.is_empty() {
            "notify.send".to_string()
        } else {
            format!("notify.send:{target}")
        };
        // Fall back to bare `notify.send` if the target string
        // contains a glob/qualifier metacharacter that
        // Scope::parse would reject. The execute path catches
        // bad target names regardless.
        Scope::parse(&raw).unwrap_or_else(|| {
            Scope::parse("notify.send").expect("notify.send must be a known base")
        })
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        let Some(dispatcher) = self.dispatcher.get() else {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: "notify.send invoked without a dispatcher; \
                         the session layer must call \
                         NotifySendTool::set_dispatcher(...) after \
                         constructing the NotifyDispatcher"
                    .to_string(),
            });
        };

        // Required: target + message. Subject is optional.
        let target = match input.get("target").and_then(|v| v.as_str()) {
            Some(t) if !t.is_empty() => t.to_string(),
            _ => {
                return ToolOutcome::Failed(AivyxError::Tool {
                    tool: self.id,
                    detail: "notify.send requires a non-empty `target` field"
                        .to_string(),
                });
            }
        };
        let message = match input.get("message").and_then(|v| v.as_str()) {
            Some(m) if !m.is_empty() => m.to_string(),
            _ => {
                return ToolOutcome::Failed(AivyxError::Tool {
                    tool: self.id,
                    detail: "notify.send requires a non-empty `message` field"
                        .to_string(),
                });
            }
        };
        let subject = input
            .get("subject")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());

        let result = dispatcher
            .dispatch(&target, &message, subject.as_deref())
            .await;

        let output = match result {
            Ok(()) => json!({
                "success": true,
                "target": target,
                "delivered_at": Utc::now().to_rfc3339(),
            }),
            Err(e) => {
                let (kind, msg) = classify_for_output(&e);
                json!({
                    "success": false,
                    "target": target,
                    "error_kind": kind,
                    "error_message": msg,
                })
            }
        };

        ToolOutcome::Completed {
            output,
            verified: Verification::NotApplicable,
        }
    }
}

/// Map a [`NotifyError`] to its `(kind, message)` pair for the
/// tool's output JSON. Pure; testable in isolation.
fn classify_for_output(e: &NotifyError) -> (&'static str, String) {
    match e {
        NotifyError::Transport(s) => ("transport", s.clone()),
        NotifyError::Auth(s) => ("auth", s.clone()),
        NotifyError::Rejected(status) => ("rejected", format!("HTTP {status}")),
        NotifyError::Timeout => ("timeout", "operation timed out".to_string()),
        NotifyError::UnknownTarget(name) => {
            ("unknown_target", format!("no notify_target named `{name}`"))
        }
    }
}

fn notify_send_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "target": {
                "type": "string",
                "description": "Name of a configured notify_target. \
                                Must match one of the [[notify_target]] \
                                entries in the operator's config. The \
                                system prompt lists reachable targets."
            },
            "message": {
                "type": "string",
                "description": "The notification body. Backends render \
                                it according to their conventions; \
                                Telegram sends it as a plain message, \
                                webhook serializes it into a JSON field."
            },
            "subject": {
                "type": "string",
                "description": "Optional short title or subject line. \
                                Used by backends that support titles \
                                (webhook payload `subject` field, \
                                Telegram prepends as bold)."
            }
        },
        "required": ["target", "message"]
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify_dispatcher::NotifyBackend;
    use aivyx_capability::TrustTier;
    use aivyx_core::{
        AgentId, CancellationToken, ChannelContext, ChannelError, ChannelPlatform,
        NullAuditHook, SessionId, StreamEvent, ToolContext, TurnOutcome,
    };
    use std::sync::Mutex;

    // --- Stub channel for ToolContext construction (mirrors the
    //     pattern from ollama_tools.rs tests) -------------------

    struct StubChannel {
        session: SessionId,
        token: CancellationToken,
    }

    impl StubChannel {
        fn new() -> Self {
            Self {
                session: SessionId::new(),
                token: CancellationToken::new(),
            }
        }
    }

    #[async_trait]
    impl ChannelContext for StubChannel {
        fn channel_name(&self) -> &str {
            "stub"
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

    static AUDIT: NullAuditHook = NullAuditHook;
    static TOKEN: std::sync::LazyLock<CancellationToken> =
        std::sync::LazyLock::new(CancellationToken::new);

    fn test_ctx(channel: &dyn ChannelContext) -> ToolContext<'_> {
        ToolContext {
            agent_id: AgentId::new(),
            session_id: channel.session_id(),
            turn_id: aivyx_core::TurnId::new(),
            channel,
            audit: &AUDIT,
            cancellation: &TOKEN,
            message_origin: aivyx_core::MessageOrigin::Operator,
        }
    }

    /// Minimal mock backend for tool-level integration tests.
    /// Records each call and returns a caller-configurable
    /// outcome.
    struct MockBackend {
        outcome: Mutex<Result<(), NotifyError>>,
        calls: Mutex<Vec<(String, Option<String>)>>,
    }

    impl MockBackend {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                outcome: Mutex::new(Ok(())),
                calls: Mutex::new(Vec::new()),
            })
        }

        fn with_outcome(outcome: Result<(), NotifyError>) -> Arc<Self> {
            Arc::new(Self {
                outcome: Mutex::new(outcome),
                calls: Mutex::new(Vec::new()),
            })
        }
    }

    #[async_trait]
    impl NotifyBackend for MockBackend {
        async fn send(
            &self,
            message: &str,
            subject: Option<&str>,
        ) -> Result<(), NotifyError> {
            self.calls
                .lock()
                .unwrap()
                .push((message.to_string(), subject.map(|s| s.to_string())));
            self.outcome.lock().unwrap().clone()
        }

        fn kind(&self) -> &'static str {
            "mock"
        }
    }

    fn dispatcher_with_single_backend(
        name: &str,
        backend: Arc<dyn NotifyBackend>,
    ) -> Arc<NotifyDispatcher> {
        let mut d = NotifyDispatcher::new();
        d.register(name, backend);
        Arc::new(d)
    }

    async fn invoke_execute(tool: &NotifySendTool, input: Value) -> ToolOutcome {
        let channel = StubChannel::new();
        let ctx = test_ctx(&channel);
        tool.execute(input, &ctx).await
    }

    #[tokio::test]
    async fn execute_success_returns_success_true_with_target_and_timestamp() {
        let backend = MockBackend::new();
        let dispatcher = dispatcher_with_single_backend("phone", backend.clone());
        let tool = NotifySendTool::new();
        tool.set_dispatcher(dispatcher).expect("set_dispatcher ok");

        let out = invoke_execute(
            &tool,
            json!({"target": "phone", "message": "hello"}),
        )
        .await;

        match out {
            ToolOutcome::Completed { output, .. } => {
                assert_eq!(output["success"], true);
                assert_eq!(output["target"], "phone");
                assert!(output["delivered_at"].as_str().unwrap().contains('T'));
            }
            other => panic!("expected Completed, got {other:?}"),
        }

        // The backend received the call.
        let calls = backend.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "hello");
        assert_eq!(calls[0].1, None);
    }

    #[tokio::test]
    async fn execute_passes_subject_to_backend() {
        let backend = MockBackend::new();
        let dispatcher = dispatcher_with_single_backend("phone", backend.clone());
        let tool = NotifySendTool::new();
        tool.set_dispatcher(dispatcher).unwrap();

        invoke_execute(
            &tool,
            json!({"target": "phone", "message": "body", "subject": "title"}),
        )
        .await;

        let calls = backend.calls.lock().unwrap();
        assert_eq!(calls[0].1, Some("title".to_string()));
    }

    #[tokio::test]
    async fn execute_empty_subject_is_treated_as_absent() {
        let backend = MockBackend::new();
        let dispatcher = dispatcher_with_single_backend("phone", backend.clone());
        let tool = NotifySendTool::new();
        tool.set_dispatcher(dispatcher).unwrap();

        invoke_execute(
            &tool,
            json!({"target": "phone", "message": "body", "subject": ""}),
        )
        .await;

        let calls = backend.calls.lock().unwrap();
        assert_eq!(calls[0].1, None);
    }

    #[tokio::test]
    async fn execute_unknown_target_returns_success_false_with_kind() {
        let backend = MockBackend::new();
        let dispatcher = dispatcher_with_single_backend("phone", backend);
        let tool = NotifySendTool::new();
        tool.set_dispatcher(dispatcher).unwrap();

        let out = invoke_execute(
            &tool,
            json!({"target": "laptop", "message": "x"}),
        )
        .await;

        match out {
            ToolOutcome::Completed { output, .. } => {
                assert_eq!(output["success"], false);
                assert_eq!(output["target"], "laptop");
                assert_eq!(output["error_kind"], "unknown_target");
                assert!(output["error_message"]
                    .as_str()
                    .unwrap()
                    .contains("laptop"));
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn execute_delivery_failure_returns_completed_with_classified_kind() {
        let cases = vec![
            (NotifyError::Auth("401".into()), "auth"),
            (NotifyError::Rejected(429), "rejected"),
            (NotifyError::Timeout, "timeout"),
            (NotifyError::Transport("dns failure".into()), "transport"),
        ];
        for (err, expected_kind) in cases {
            let backend = MockBackend::with_outcome(Err(err));
            let dispatcher = dispatcher_with_single_backend("phone", backend);
            let tool = NotifySendTool::new();
            tool.set_dispatcher(dispatcher).unwrap();

            let out = invoke_execute(
                &tool,
                json!({"target": "phone", "message": "x"}),
            )
            .await;

            match out {
                ToolOutcome::Completed { output, .. } => {
                    assert_eq!(output["success"], false);
                    assert_eq!(
                        output["error_kind"], expected_kind,
                        "kind mismatch for {expected_kind}"
                    );
                }
                other => panic!("expected Completed for {expected_kind}, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn execute_missing_target_returns_failed() {
        let backend = MockBackend::new();
        let dispatcher = dispatcher_with_single_backend("phone", backend);
        let tool = NotifySendTool::new();
        tool.set_dispatcher(dispatcher).unwrap();

        let out = invoke_execute(&tool, json!({"message": "x"})).await;
        assert!(
            matches!(out, ToolOutcome::Failed(_)),
            "expected Failed (malformed input), got {out:?}"
        );
    }

    #[tokio::test]
    async fn execute_missing_message_returns_failed() {
        let backend = MockBackend::new();
        let dispatcher = dispatcher_with_single_backend("phone", backend);
        let tool = NotifySendTool::new();
        tool.set_dispatcher(dispatcher).unwrap();

        let out = invoke_execute(&tool, json!({"target": "phone"})).await;
        assert!(matches!(out, ToolOutcome::Failed(_)));
    }

    #[tokio::test]
    async fn execute_without_dispatcher_set_returns_failed() {
        let tool = NotifySendTool::new();
        let out = invoke_execute(
            &tool,
            json!({"target": "phone", "message": "x"}),
        )
        .await;
        assert!(matches!(out, ToolOutcome::Failed(_)));
    }

    #[test]
    fn required_scope_with_target_returns_qualified_form() {
        let tool = NotifySendTool::new();
        let scope = tool.required_scope(&json!({"target": "phone", "message": "x"}));
        assert_eq!(scope.base(), "notify.send");
        assert_eq!(scope.qualifier(), Some("phone"));
    }

    #[test]
    fn required_scope_without_target_falls_back_to_unqualified() {
        let tool = NotifySendTool::new();
        let scope = tool.required_scope(&json!({"message": "x"}));
        assert_eq!(scope.base(), "notify.send");
        assert_eq!(scope.qualifier(), None);
    }

    #[test]
    fn classify_for_output_distinguishes_each_variant() {
        assert_eq!(
            classify_for_output(&NotifyError::Transport("x".into())).0,
            "transport"
        );
        assert_eq!(
            classify_for_output(&NotifyError::Auth("x".into())).0,
            "auth"
        );
        assert_eq!(
            classify_for_output(&NotifyError::Rejected(404)).0,
            "rejected"
        );
        assert_eq!(classify_for_output(&NotifyError::Timeout).0, "timeout");
        assert_eq!(
            classify_for_output(&NotifyError::UnknownTarget("phone".into())).0,
            "unknown_target"
        );
    }
}
