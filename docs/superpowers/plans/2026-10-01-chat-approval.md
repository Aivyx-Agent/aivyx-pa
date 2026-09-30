# Chat Approval Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When a chat tool call needs the operator's approval, the turn pauses, the terminal chat / Studio ask Approve or Deny, and on approval the exact call runs and the turn continues.

**Architecture:** A default `ChannelContext::request_approval` hook (returns `Unavailable`) is called by the agent's turn loop whenever a tool call yields `ToolOutcome::RequiresEscalation`. The agent owns the whole confirmation rule (strip model-set `confirmed`, the next-turn ledger fallback); tools only signal "needs approval". The daemon keeps reading a connection during a turn (reader task + `select!`), routes `ResolveApproval` / `CancelTurn` to the running turn, and its bridge implements `request_approval` for connections that opted in with `SetApprovals`.

**Tech Stack:** Rust (tokio, serde, async-trait), Dioxus (Studio, wasm), existing IPC framing (`aivyx-ipc`).

**Spec:** `docs/superpowers/specs/2026-10-01-chat-approval-design.md`

## Global Constraints

- Branch: `feat/chat-approval` in `aivyx-pa`. Commits signed off (`git commit -s`) and ending with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- Zero clippy warnings: `cargo clippy --workspace --all-targets -- -D warnings`; all of `cargo test --workspace` passes after every task.
- `aivyx-web` is outside default-members: check it with `cargo clippy -p aivyx-web --all-targets -- -D warnings`; rebuild its committed bundle with `just build-web` (needs `PATH="$HOME/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$HOME/.cargo/bin:$PATH"`), then `find crates/aivyx-web/dist -name '*.br' -delete`.
- No new `ToolOutcome` / `TurnOutcome` variants (D3 is locked). The approval hook is a **default** trait method (D2 precedent: `session_partition`).
- Unanswered prompt: denied after **600 seconds**; disconnect or turn cancel → denied at once.
- Only connections that sent `SetApprovals { enabled: true }` ever receive an `ApprovalRequest`.
- The fallback ledger is keyed by **session + tool name**.
- Copy (user-facing): prompt header `⚑ Approval needed — {summary}`; answer line `Approve? [y/N] (10 min)`; declined tool result `The operator declined this action.`; timed-out result `No answer within 10 minutes, so this action was not taken.`

---

### Task 1: Core types — approval hook, ledger API, audit variants

**Files:**
- Modify: `crates/aivyx-core/src/confirm.rs` (replace `allows` with `record_refusal` / `take_refusal`)
- Modify: `crates/aivyx-core/src/lib.rs` (types near `ChannelContext` at ~line 320; `AuditTag` at ~line 739)
- Modify: `crates/aivyx-audit/src/lib.rs` (`AuditEvent` enum ~line 70–520; `From<AuditTag>` mapping ~line 1219)
- Modify: `crates/aivyx-channel/src/daemon_server.rs` (event-type label fn ~line 7975)
- Modify: `crates/aivyx-cli/src/bin/aivyx_modules/audit_export.rs` (label match ~line 218)
- Modify: `crates/aivyx-web/src/main.rs` (`audit_event_label` + its test)

**Interfaces:**
- Produces:
  - `aivyx_core::ApprovalRequest { pub tool: String, pub summary: String, pub input: serde_json::Value, pub reason: String }` (`Debug, Clone, PartialEq, Serialize, Deserialize`)
  - `aivyx_core::Approval { Approved, Denied, TimedOut, Unavailable }` (`Debug, Clone, Copy, PartialEq, Eq`)
  - `ChannelContext::request_approval(&self, request: &ApprovalRequest) -> Approval` (async, default `Approval::Unavailable`)
  - `aivyx_core::APPROVAL_TIMEOUT: std::time::Duration = 600 s`
  - `OperatorConfirmations::record_refusal(&self, session: SessionId, turn: TurnId, target: &str)`
  - `OperatorConfirmations::take_refusal(&self, session: SessionId, turn: TurnId, target: &str) -> bool` — true iff a refusal of `target` was recorded in an EARLIER turn of `session`; consumes it
  - `AuditTag::ApprovalRequested { turn_id: TurnId, tool: String, summary: String }`, `AuditTag::ApprovalResolved { turn_id: TurnId, tool: String, outcome: String }` and the same-named `AuditEvent` variants

- [ ] **Step 1: Write the failing ledger test** — replace the body of `crates/aivyx-core/src/confirm.rs`'s module with the new API and add at the end:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_counts_only_in_a_later_turn_of_the_same_session() {
        let ledger = OperatorConfirmations::default();
        let (s, t1, t2) = (SessionId::new(), TurnId::new(), TurnId::new());
        assert!(!ledger.take_refusal(s, t1, "fs.delete"), "nothing recorded yet");
        ledger.record_refusal(s, t1, "fs.delete");
        assert!(!ledger.take_refusal(s, t1, "fs.delete"), "same turn doesn't count");
        assert!(!ledger.take_refusal(SessionId::new(), t2, "fs.delete"), "other session");
        assert!(ledger.take_refusal(s, t2, "fs.delete"), "the operator's next turn");
        assert!(!ledger.take_refusal(s, TurnId::new(), "fs.delete"), "consumed: one run");
    }
}
```

- [ ] **Step 2: Run it — expect a compile failure** (`record_refusal` / `take_refusal` don't exist)

Run: `cargo test -p aivyx-core --lib confirm::`
Expected: FAIL — `no method named record_refusal`.

- [ ] **Step 3: Implement the ledger API** — in `confirm.rs` replace `impl OperatorConfirmations { pub fn allows ... }` with:

```rust
impl OperatorConfirmations {
    /// Remember that `target` was refused in `turn` (the first refusal wins,
    /// so a later refusal in the same turn doesn't move it forward).
    pub fn record_refusal(&self, session: SessionId, turn: TurnId, target: &str) {
        let Ok(mut refused) = self.0.lock() else { return };
        if refused.len() > 1024 {
            refused.clear();
        }
        refused.entry((session, target.to_string())).or_insert(turn);
    }

    /// Whether `target` was refused in an EARLIER turn of `session` — i.e. the
    /// operator has replied since. Consumes the record: one approval, one run.
    pub fn take_refusal(&self, session: SessionId, turn: TurnId, target: &str) -> bool {
        let Ok(mut refused) = self.0.lock() else { return false };
        let key = (session, target.to_string());
        match refused.get(&key) {
            Some(t) if *t != turn => {
                refused.remove(&key);
                true
            }
            _ => false,
        }
    }
}
```

Remove the now-unused `use crate::ToolContext;` import and the old `allows` doc. Keep `ASK_THEN_END_TURN`, `declares_confirmed`, `is_confirmed`, `target_of`.

- [ ] **Step 4: Add the approval types and hook** — in `crates/aivyx-core/src/lib.rs`, directly above `pub trait ChannelContext`:

```rust
/// How long an approval prompt waits before the action is refused.
pub const APPROVAL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(600);

/// A tool call waiting for the operator's yes/no (see
/// [`ChannelContext::request_approval`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApprovalRequest {
    /// The tool's name, e.g. `fs.delete`.
    pub tool: String,
    /// One line: the tool and what it touches, e.g. `fs.delete todo.md`.
    pub summary: String,
    /// The exact arguments that will run if approved.
    pub input: serde_json::Value,
    /// Why it's asking (the tool's escalation reason).
    pub reason: String,
}

/// The operator's answer to an [`ApprovalRequest`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Approval {
    Approved,
    Denied,
    /// No answer within [`APPROVAL_TIMEOUT`].
    TimedOut,
    /// This channel can't ask (a chat app, a script, an unattended run).
    Unavailable,
}
```

and inside `pub trait ChannelContext` (after `cancellation_token`):

```rust
    /// Ask the operator to approve a tool call and wait for the answer.
    /// Default: the channel can't ask, so the turn falls back to ending with
    /// the escalation. A default method, like `session_partition`, so every
    /// existing channel is unchanged (D2).
    async fn request_approval(&self, _request: &ApprovalRequest) -> Approval {
        Approval::Unavailable
    }
```

Re-export check: `ApprovalRequest`, `Approval`, `APPROVAL_TIMEOUT` are defined in `lib.rs`, so they're `aivyx_core::…` directly.

- [ ] **Step 5: Add the audit variants** — in `AuditTag` (lib.rs, after `RateLimited`):

```rust
    /// A tool call paused for the operator's approval in an interactive turn.
    ApprovalRequested {
        turn_id: TurnId,
        tool: String,
        summary: String,
    },
    /// The operator's answer: `approved`, `denied` or `timed_out`.
    ApprovalResolved {
        turn_id: TurnId,
        tool: String,
        outcome: String,
    },
```

In `crates/aivyx-audit/src/lib.rs` add the same two variants to `AuditEvent` (after `RateLimited`, same doc lines) and to the `From<AuditTag>` match:

```rust
            AuditTag::ApprovalRequested { turn_id, tool, summary } => {
                AuditEvent::ApprovalRequested { turn_id, tool, summary }
            }
            AuditTag::ApprovalResolved { turn_id, tool, outcome } => {
                AuditEvent::ApprovalResolved { turn_id, tool, outcome }
            }
```

Add labels: in `daemon_server.rs`'s event-type match add
`aivyx_audit::AuditEvent::ApprovalRequested { .. } => "ApprovalRequested",` and `aivyx_audit::AuditEvent::ApprovalResolved { .. } => "ApprovalResolved",`; in `audit_export.rs` the same two arms with `AuditEvent::`; in `crates/aivyx-web/src/main.rs` `audit_event_label` add `"ApprovalRequested" => "Approval asked",` and `"ApprovalResolved" => "Approval answered",`, and add both names to the `every_audit_event_variant_has_a_readable_name` list.

- [ ] **Step 6: Build everything and run tests**

Run: `cargo test -p aivyx-core --lib confirm:: && cargo test -p aivyx-audit && cargo clippy --workspace --all-targets -- -D warnings`
Expected: the ledger test passes. Clippy will FAIL where `fs.rs`, `git.rs`, `aivyx-dataread` and `aivyx-tool` still call `allows(...)`: in each, replace

```rust
!self.confirms.allows(ctx, &TARGET, CONFIRMED)
```

temporarily with

```rust
!(CONFIRMED && self.confirms.take_refusal(ctx.session_id, ctx.turn_id, &TARGET))
```

followed on the refusal path by `self.confirms.record_refusal(ctx.session_id, ctx.turn_id, &TARGET);` (exact same semantics as before). Task 3 deletes these ledgers; this keeps the tree green in between. In `agent.rs`, the integration gate `!self.integration_confirms.allows(&ctx, tool_name, true)` becomes `!self.integration_confirms.take_refusal(ctx.session_id, turn_id, tool_name)` with `self.integration_confirms.record_refusal(ctx.session_id, turn_id, tool_name)` inside the `if needs_destructive_confirmation` branch. Then `cargo clippy -p aivyx-web --all-targets -- -D warnings` and `cargo test -p aivyx-web`.
Expected: all green.

- [ ] **Step 7: Commit**

```bash
git add -A && git commit -s -m "feat(core): approval hook, explicit ledger API, approval audit events"
```

---

### Task 2: The agent asks, re-runs, or reports "declined"

**Files:**
- Modify: `crates/aivyx-core/src/planner.rs:65` (`ToolCallRequest` gains `pub operator_approved: bool`)
- Modify: `crates/aivyx-core/src/agent.rs` (`ConcreteAgent` fields; `turn_inner` deadline; single-call and batch paths ~line 740–890; `run_tool_call` ~line 1359–1705)
- Test: `crates/aivyx-core/src/agent.rs` `mod tests`

**Interfaces:**
- Consumes: Task 1's `Approval`, `ApprovalRequest`, `APPROVAL_TIMEOUT`, `record_refusal`, `take_refusal`, `AuditTag::ApprovalRequested/ApprovalResolved`.
- Produces:
  - `ToolCallRequest.operator_approved: bool` (every construction site sets `false`; only the agent's re-run sets `true`)
  - `fn approval_summary(tool_name: &str, input: &serde_json::Value) -> String` (pub(crate), in agent.rs)
  - Agent field `confirms: crate::confirm::OperatorConfirmations` replacing `integration_confirms`
  - Behaviour: a `RequiresEscalation` from `run_tool_call` in an interactive turn is resolved through `channel.request_approval`.

- [ ] **Step 1: Write the failing tests** (in `agent.rs` `mod tests`, next to `an_escalated_integration_write_runs_once_in_the_operators_next_turn`). Add a scripted channel:

```rust
    /// A `FakeChannel` whose `request_approval` returns scripted answers
    /// and records what it was asked.
    struct ApprovingChannel {
        inner: FakeChannel,
        answers: std::sync::Mutex<std::collections::VecDeque<Approval>>,
        asked: std::sync::Mutex<Vec<ApprovalRequest>>,
    }
    impl ApprovingChannel {
        fn new(answers: &[Approval]) -> Self {
            ApprovingChannel {
                inner: FakeChannel::new(ChannelPlatform::Local, TrustTier::Trusted),
                answers: std::sync::Mutex::new(answers.iter().copied().collect()),
                asked: std::sync::Mutex::new(Vec::new()),
            }
        }
    }
    #[async_trait::async_trait]
    impl ChannelContext for ApprovingChannel {
        fn channel_name(&self) -> &str { self.inner.channel_name() }
        fn platform(&self) -> ChannelPlatform { self.inner.platform() }
        fn trust_tier(&self) -> TrustTier { self.inner.trust_tier() }
        fn session_id(&self) -> SessionId { self.inner.session_id() }
        async fn stream_event(&self, e: StreamEvent<'_>) -> Result<(), ChannelError> {
            self.inner.stream_event(e).await
        }
        async fn finalize(&self, o: &TurnOutcome) -> Result<(), ChannelError> {
            self.inner.finalize(o).await
        }
        fn cancellation_token(&self) -> CancellationToken { self.inner.cancellation_token() }
        async fn request_approval(&self, r: &ApprovalRequest) -> Approval {
            self.asked.lock().unwrap().push(r.clone());
            self.answers.lock().unwrap().pop_front().unwrap_or(Approval::Unavailable)
        }
    }
```

A confirm-first fake tool that records the inputs it ran with:

```rust
    /// Confirm-first: escalates unless `confirmed: true`; records every input.
    struct ConfirmFirstTool {
        id: ToolId,
        schema: serde_json::Value,
        ran_with: std::sync::Mutex<Vec<serde_json::Value>>,
    }
    impl ConfirmFirstTool {
        fn new() -> Arc<Self> {
            Arc::new(ConfirmFirstTool {
                id: ToolId::new(),
                schema: json!({"type": "object", "properties": {
                    "path": {"type": "string"}, "confirmed": {"type": "boolean"}}}),
                ran_with: std::sync::Mutex::new(Vec::new()),
            })
        }
    }
    #[async_trait::async_trait]
    impl Tool for ConfirmFirstTool {
        fn id(&self) -> ToolId { self.id }
        fn name(&self) -> &str { "fs.delete" }
        fn description(&self) -> &str { "test" }
        fn input_schema(&self) -> &serde_json::Value { &self.schema }
        fn required_scope(&self, _: &serde_json::Value) -> Scope { Scope::parse("fs.read").unwrap() }
        async fn execute(&self, input: serde_json::Value, _: &ToolContext<'_>) -> ToolOutcome {
            self.ran_with.lock().unwrap().push(input.clone());
            if crate::confirm::is_confirmed(&input) {
                ToolOutcome::Completed { output: json!({"deleted": true}), verified: Verification::Verified }
            } else {
                ToolOutcome::RequiresEscalation { reason: "deleting can't be undone".into(), scope: None }
            }
        }
    }
```

(Use whatever capability scope `make_agent`'s callers grant in neighbouring tests — `fs.read` with `CapabilitySet::from_scopes([Scope::parse("fs.read").unwrap()])` — the tool's `required_scope` must be held.)

The tests:

```rust
    fn delete_plan(tool_id: ToolId, input: serde_json::Value) -> Vec<NextStep> {
        vec![
            NextStep::ToolCall { tool_id, input, auto_corrected_from: None, extracted_from_text: None },
            NextStep::FinalMessage("done".to_string()),
        ]
    }

    #[tokio::test]
    async fn an_approved_call_reruns_the_exact_input_with_confirmed() {
        let tool = ConfirmFirstTool::new();
        let caps = CapabilitySet::from_scopes([Scope::parse("fs.read").unwrap()]);
        let agent = make_agent(caps, vec![tool.clone()], RecordingAudit::new(),
            delete_plan(tool.id, json!({"path": "todo.md"})));
        let channel = ApprovingChannel::new(&[Approval::Approved]);
        let out = agent.turn(Message::text(channel.session_id(), "delete todo.md"), &channel).await;
        assert!(matches!(out, TurnOutcome::Completed { .. }), "{out:?}");
        let ran = tool.ran_with.lock().unwrap().clone();
        assert_eq!(ran, vec![json!({"path": "todo.md", "confirmed": false}),
                             json!({"path": "todo.md", "confirmed": true})]);
        let asked = channel.asked.lock().unwrap().clone();
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].summary, "fs.delete todo.md");
        assert_eq!(asked[0].input, json!({"path": "todo.md"}));
    }

    #[tokio::test]
    async fn a_model_supplied_confirmed_is_stripped_before_asking() {
        let tool = ConfirmFirstTool::new();
        let caps = CapabilitySet::from_scopes([Scope::parse("fs.read").unwrap()]);
        let agent = make_agent(caps, vec![tool.clone()], RecordingAudit::new(),
            delete_plan(tool.id, json!({"path": "todo.md", "confirmed": true})));
        let channel = ApprovingChannel::new(&[Approval::Denied]);
        let out = agent.turn(Message::text(channel.session_id(), "delete"), &channel).await;
        assert!(matches!(out, TurnOutcome::Completed { .. }), "declined continues the turn: {out:?}");
        let ran = tool.ran_with.lock().unwrap().clone();
        assert_eq!(ran, vec![json!({"path": "todo.md", "confirmed": false})], "never ran confirmed");
    }

    #[tokio::test]
    async fn a_timed_out_approval_reports_no_answer_and_continues() {
        let tool = ConfirmFirstTool::new();
        let caps = CapabilitySet::from_scopes([Scope::parse("fs.read").unwrap()]);
        let audit = RecordingAudit::new();
        let agent = make_agent(caps, vec![tool.clone()], audit.clone(),
            delete_plan(tool.id, json!({"path": "todo.md"})));
        let channel = ApprovingChannel::new(&[Approval::TimedOut]);
        let out = agent.turn(Message::text(channel.session_id(), "delete"), &channel).await;
        assert!(matches!(out, TurnOutcome::Completed { .. }), "{out:?}");
        let events = audit.snapshot();
        assert!(events.iter().any(|e| matches!(e, AuditTag::ApprovalRequested { .. })));
        assert!(events.iter().any(|e| matches!(e,
            AuditTag::ApprovalResolved { outcome, .. } if outcome == "timed_out")));
    }

    #[tokio::test]
    async fn unavailable_escalates_and_the_next_turn_may_confirm() {
        let tool = ConfirmFirstTool::new();
        let caps = CapabilitySet::from_scopes([Scope::parse("fs.read").unwrap()]);
        let agent = make_agent(caps, vec![tool.clone()], RecordingAudit::new(),
            delete_plan(tool.id, json!({"path": "todo.md", "confirmed": true})));
        let channel = FakeChannel::new(ChannelPlatform::Local, TrustTier::Trusted);
        let first = agent.turn(Message::text(channel.session, "delete"), &channel).await;
        assert!(matches!(first, TurnOutcome::Escalated { .. }), "{first:?}");
        let second = agent.turn(Message::text(channel.session, "yes"), &channel).await;
        assert!(matches!(second, TurnOutcome::Completed { .. }), "{second:?}");
        assert_eq!(tool.ran_with.lock().unwrap().last().unwrap()["confirmed"], json!(true));
    }
```

Also update `an_escalated_integration_write_runs_once_in_the_operators_next_turn` only if it no longer compiles (it uses `FakeChannel`, whose default `request_approval` is `Unavailable`, so its behaviour is unchanged).

- [ ] **Step 2: Run them — expect failures**

Run: `cargo test -p aivyx-core --lib agent::tests::an_approved_call_reruns_the_exact_input_with_confirmed agent::tests::a_model_supplied`
Expected: FAIL (first test: turn ends `Escalated`; no approval asked).

- [ ] **Step 3: `ToolCallRequest.operator_approved`** — in `planner.rs` add to the struct:

```rust
    /// Set only by the agent when re-running a call the operator approved
    /// through `ChannelContext::request_approval`. Planners always set `false`.
    pub operator_approved: bool,
```

Add `operator_approved: false,` to every `ToolCallRequest { … }` literal (`grep -rn "ToolCallRequest {" crates`).

- [ ] **Step 4: One ledger in the agent; strip + confirm in `run_tool_call`** — rename the field `integration_confirms` → `confirms` (struct, `new()`, doc: "session + tool name: refused confirm-first or integration calls, for channels that can't ask"). In `run_tool_call`, destructure `operator_approved` from `req`. After `let ctx = ToolContext { … };` and before `input_bytes`, insert:

```rust
        // The operator, not the model, confirms. For a confirm-first tool
        // (its schema declares `confirmed`) the flag is ours to set: true
        // only for an operator-approved re-run, or — where the channel
        // can't ask — when the operator replied after an earlier refusal.
        let confirm_first = crate::confirm::declares_confirmed(tool.input_schema());
        let operator_confirmed = operator_approved
            || (crate::confirm::is_confirmed(&input)
                && self.confirms.take_refusal(ctx.session_id, turn_id, tool.name()));
        let mut input = input;
        if confirm_first {
            if let Some(obj) = input.as_object_mut() {
                obj.insert("confirmed".into(), serde_json::Value::Bool(operator_confirmed));
            }
        }
```

Replace the integration gate with:

```rust
        let needs_destructive_confirmation = self.confirm_destructive
            && aivyx_capability::is_withheld_integration_base(needed.base())
            && !operator_approved
            && !self.confirms.take_refusal(ctx.session_id, turn_id, tool_name);
```

and after `let step_duration = step_start.elapsed();` insert:

```rust
        // Remember a refusal so that, on a channel that can't ask, the
        // operator's next message can approve it (see the turn loop).
        if matches!(outcome, ToolOutcome::RequiresEscalation { .. }) && !operator_approved {
            self.confirms.record_refusal(ctx.session_id, turn_id, tool_name);
        }
```

(`tool_name` is `tool.name()`, already bound above the gate.) Update the gate's reason text to: `"{tool_name} needs the operator's approval before it runs (`{}` is a third-party action Aivyx PA never takes unasked)."`

- [ ] **Step 5: `approval_summary`** — add near `call_signature` in agent.rs:

```rust
/// One line for an approval prompt: the tool and what it touches.
pub(crate) fn approval_summary(tool_name: &str, input: &serde_json::Value) -> String {
    const KEYS: [&str; 6] = ["path", "repo", "to", "recipient", "purchase_order_id", "name"];
    let what = KEYS
        .iter()
        .find_map(|k| input.get(*k).and_then(|v| v.as_str()))
        .map(str::to_string)
        .or_else(|| {
            input.as_object().and_then(|o| {
                o.iter()
                    .filter(|(k, _)| k.as_str() != "confirmed")
                    .find_map(|(_, v)| v.as_str().map(str::to_string))
            })
        });
    match what {
        Some(w) => format!("{tool_name} {}", w.chars().take(80).collect::<String>()),
        None => tool_name.to_string(),
    }
}
```

Test (add to `mod tests`):

```rust
    #[test]
    fn approval_summary_names_the_target() {
        assert_eq!(approval_summary("fs.delete", &json!({"path": "todo.md", "confirmed": true})),
                   "fs.delete todo.md");
        assert_eq!(approval_summary("kitchen.order.send", &json!({"purchase_order_id": "PO-7"})),
                   "kitchen.order.send PO-7");
        assert_eq!(approval_summary("x.y", &json!({"count": 3})), "x.y");
    }
```

- [ ] **Step 6: A pausable deadline** — in `turn_inner` replace the `deadline_fired` / `deadline_task` block with a small struct defined above `impl ConcreteAgent`:

```rust
/// The turn's wall-clock deadline, pausable while an approval prompt waits
/// (a slow human answer must not time the turn out).
struct TurnDeadline {
    fired: Arc<AtomicBool>,
    token: CancellationToken,
    remaining: Duration,
    started: Instant,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl TurnDeadline {
    fn start(timeout: Duration, token: CancellationToken) -> Self {
        let mut d = TurnDeadline {
            fired: Arc::new(AtomicBool::new(false)),
            token,
            remaining: timeout,
            started: Instant::now(),
            task: None,
        };
        d.resume();
        d
    }
    fn resume(&mut self) {
        let (fired, token, remaining) = (Arc::clone(&self.fired), self.token.clone(), self.remaining);
        self.started = Instant::now();
        self.task = Some(tokio::spawn(async move {
            tokio::time::sleep(remaining).await;
            fired.store(true, Ordering::SeqCst);
            token.cancel();
        }));
    }
    fn pause(&mut self) {
        if let Some(t) = self.task.take() {
            t.abort();
            self.remaining = self.remaining.saturating_sub(self.started.elapsed());
        }
    }
    fn stop(&mut self) {
        if let Some(t) = self.task.take() {
            t.abort();
        }
    }
}
```

In `turn_inner`: `let mut deadline = TurnDeadline::start(self.turn_timeout, cancellation.clone());`, replace every `&deadline_fired` with `&deadline.fired`, and `deadline_task.abort();` with `deadline.stop();`.

- [ ] **Step 7: Resolve escalations through the channel** — add a method on `ConcreteAgent`:

```rust
    /// A call came back `RequiresEscalation`: ask the operator through the
    /// channel. `Some(result)` replaces the escalation (approved re-run, or a
    /// "declined" / "no answer" tool result the model sees); `None` keeps it
    /// (the channel can't ask — the turn ends and the ledger lets the
    /// operator's next message approve).
    async fn seek_approval(
        &self,
        env: &TurnCallEnv<'_>,
        deadline: &mut TurnDeadline,
        tool_id: ToolId,
        input: serde_json::Value,
        reason: &str,
    ) -> Option<(StepObservation, ToolOutcome, Option<String>)> {
        let tool_name = self.tools.get(tool_id)?.name().to_string();
        let mut shown = input.clone();
        if let Some(obj) = shown.as_object_mut() {
            obj.remove("confirmed");
        }
        let request = ApprovalRequest {
            tool: tool_name.clone(),
            summary: approval_summary(&tool_name, &shown),
            input: shown.clone(),
            reason: reason.to_string(),
        };
        self.audit.on_event(AuditTag::ApprovalRequested {
            turn_id: env.turn_id,
            tool: tool_name.clone(),
            summary: request.summary.clone(),
        });
        deadline.pause();
        let answer = env.channel.request_approval(&request).await;
        deadline.resume();
        let outcome_label = match answer {
            Approval::Approved => "approved",
            Approval::Denied => "denied",
            Approval::TimedOut => "timed_out",
            Approval::Unavailable => return None,
        };
        self.audit.on_event(AuditTag::ApprovalResolved {
            turn_id: env.turn_id,
            tool: tool_name,
            outcome: outcome_label.to_string(),
        });
        match answer {
            Approval::Approved => Some(
                self.run_tool_call(
                    env,
                    crate::planner::ToolCallRequest {
                        tool_id,
                        input: shown,
                        auto_corrected_from: None,
                        extracted_from_text: None,
                        operator_approved: true,
                    },
                )
                .await,
            ),
            _ => {
                let detail = if answer == Approval::TimedOut {
                    "No answer within 10 minutes, so this action was not taken."
                } else {
                    "The operator declined this action."
                };
                Some((
                    StepObservation { tool_id, summary: ToolOutcomeSummary::Failed },
                    ToolOutcome::Failed(AivyxError::Tool { tool: tool_id, detail: detail.to_string() }),
                    None,
                ))
            }
        }
    }
```

In the single-call path, keep a copy of the input before dispatch (`let input_for_approval = input.clone();` before building `req`) and change

```rust
                    let (observation, outcome, injection_reason) =
                        self.run_tool_call(&env, req).await;
```

to

```rust
                    let (mut observation, mut outcome, mut injection_reason) =
                        self.run_tool_call(&env, req).await;
                    if let ToolOutcome::RequiresEscalation { reason, .. } = &outcome
                        && injection_reason.is_none()
                    {
                        let reason = reason.clone();
                        if let Some(resolved) = self
                            .seek_approval(&env, &mut deadline, tool_id, input_for_approval, &reason)
                            .await
                        {
                            (observation, outcome, injection_reason) = resolved;
                        }
                    }
```

In the batch path, keep `let inputs: Vec<(ToolId, serde_json::Value)> = batch.iter().map(|r| (r.tool_id, r.input.clone())).collect();` before the `futures` map, iterate `results.into_iter().zip(inputs)`, and inside the loop, before the `escalated.is_none()` check:

```rust
                        let (mut observation, mut outcome, mut injection_reason) = (observation, outcome, injection_reason);
                        if let ToolOutcome::RequiresEscalation { reason, .. } = &outcome
                            && injection_reason.is_none()
                        {
                            let reason = reason.clone();
                            if let Some(resolved) =
                                self.seek_approval(&env, &mut deadline, call_id, call_input, &reason).await
                            {
                                (observation, outcome, injection_reason) = resolved;
                            }
                        }
```

(with the tuple destructured as `((observation, outcome, injection_reason), (call_id, call_input))`, and the rest of the loop body unchanged, using `observation.tool_id` as before).

- [ ] **Step 8: Run the agent tests**

Run: `cargo test -p aivyx-core --lib agent::`
Expected: PASS, including the four new tests, `approval_summary_names_the_target`, and the existing integration-gate tests.

- [ ] **Step 9: Workspace check and commit**

Run: `cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: green (fix any other `ToolCallRequest` literal the compiler names by adding `operator_approved: false`).

```bash
git add -A && git commit -s -m "feat(agent): ask the operator on escalation; approved calls re-run exactly, declined ones continue"
```

---

### Task 3: Tools only say "needs approval"; the per-tool ledgers go

**Files:**
- Modify: `crates/aivyx-core/src/tools/fs.rs` (FsWriteTool / FsDeleteTool: drop `confirms` fields; refusals → `RequiresEscalation`)
- Modify: `crates/aivyx-core/src/tools/git.rs` (GitCommitTool: same)
- Modify: `crates/aivyx-dataread/src/sandbox.rs`, `xlsx_writer.rs`, `pdf_writer.rs` (drop `overwrite_confirms`; replacing needs `overwrite` + `confirmed`, refusal → `RequiresEscalation`; schemas gain `confirmed`)
- Modify: `crates/aivyx-tool/src/proxy.rs` (drop `confirms` and the forwarding rewrite — the agent owns it)
- Delete: `crates/aivyx-tool/tests/confirm_first_proxy.rs`, `crates/aivyx-tool/src/bin/confirm_first_tool_fixture.rs` (the rule now lives in the agent and is tested there)

**Interfaces:**
- Consumes: Task 2 (the agent strips/sets `confirmed` for any schema declaring it).
- Produces: `fs.delete`, `fs.write` (overwrite), `git.commit`, `data.xlsx.write`/`data.pdf.write` (replace) return `ToolOutcome::RequiresEscalation { reason, scope: None }` when confirm-first applies and `confirmed` is not true; with `confirmed: true` they run.

- [ ] **Step 1: Update the tool tests first** — in `fs.rs` tests replace `delete_confirm_first_needs_a_later_turn` and `overwrite_confirm_first_needs_a_later_turn` with:

```rust
    #[test]
    fn delete_confirm_first_asks_then_runs_when_confirmed() {
        let sandbox = SandboxDir::new();
        sandbox.write_file("doomed.txt", b"bye");
        let tool = FsDeleteToolConfig::new(sandbox.root.clone())
            .with_confirm_destructive(true)
            .build()
            .unwrap();
        let asked = run_execute(&tool, json!({"path": "doomed.txt"}));
        assert!(matches!(asked, ToolOutcome::RequiresEscalation { .. }), "{asked:?}");
        assert!(sandbox.root.join("doomed.txt").exists());
        let ran = run_execute(&tool, json!({"path": "doomed.txt", "confirmed": true}));
        assert!(matches!(ran, ToolOutcome::Completed { .. }), "{ran:?}");
        assert!(!sandbox.root.join("doomed.txt").exists());
    }

    #[test]
    fn overwrite_confirm_first_asks_then_runs_when_confirmed() {
        let sandbox = SandboxDir::new();
        sandbox.write_file("notes.txt", b"original");
        let tool = FsWriteToolConfig::new(sandbox.root.clone())
            .with_confirm_destructive(true)
            .build()
            .unwrap();
        let asked = run_execute(&tool, json!({"path": "notes.txt", "content": "new"}));
        assert!(matches!(asked, ToolOutcome::RequiresEscalation { .. }), "{asked:?}");
        let ran = run_execute(&tool, json!({"path": "notes.txt", "content": "new", "confirmed": true}));
        assert!(matches!(ran, ToolOutcome::Completed { .. }), "{ran:?}");
    }
```

Update `delete_confirm_first_refuses_without_confirmed` and `write_confirm_first_refuses_overwrite_without_confirmed` to assert `RequiresEscalation` instead of `Failed`. Remove `run_execute_in` if no test uses it. In `git.rs` change `commit_confirm_first_refuses_without_confirmation` back to: unconfirmed → `RequiresEscalation`, `confirmed: true` in the same ctx → `Completed`. In `pdf_writer.rs` replace `replacing_a_file_under_confirm_first_needs_a_later_turn` with: `{"overwrite": true}` → `RequiresEscalation`, `{"overwrite": true, "confirmed": true}` → `Completed`.

- [ ] **Step 2: Run them — expect failures**

Run: `cargo test -p aivyx-core --lib tools:: ; cargo test -p aivyx-dataread`
Expected: FAIL (tools still return `Failed` / use the ledger).

- [ ] **Step 3: Implement** —
  - `fs.rs`: delete the `confirms` fields and their `Default::default()` initialisers. The overwrite check becomes `if self.confirm_destructive && lexical_abs.exists() && !is_confirmed(&input) { return ToolOutcome::RequiresEscalation { reason: format!("overwriting {path_str:?} can't be undone. {DESTRUCTIVE_CONFIRM_HINT}"), scope: None }; }`; the delete check `if self.confirm_destructive && !is_confirmed(&input) { return ToolOutcome::RequiresEscalation { reason: format!("deleting {path_str:?} can't be undone. {DESTRUCTIVE_CONFIRM_HINT}"), scope: None }; }` (keep it where it is, after the lexical resolve). Set `DESTRUCTIVE_CONFIRM_HINT` to `"The operator must approve it first."`.
  - `git.rs`: remove the `confirms` field; the commit check becomes `if self.confirm_destructive && !git_is_confirmed(&input) { return ToolOutcome::RequiresEscalation { reason: format!("git.commit writes history to {}: the operator must approve it first.", repo.display()), scope: None }; }`.
  - `aivyx-dataread/src/sandbox.rs`: replace `overwrite_confirms: Option<Arc<OperatorConfirmations>>` with `confirm_destructive: bool` (`with_confirm_destructive(on)` sets it); `resolve_write_target` drops its `ctx` parameter again and, for an existing target, returns `Err(fail(..))` when `!overwrite`, and `Err(ToolOutcome::RequiresEscalation { reason: format!("{path_str:?} already exists and replacing it can't be undone: the operator must approve it first."), scope: None })` when `self.confirm_destructive && !is_confirmed(input)` (`aivyx_core::confirm::is_confirmed`). Writers call `resolve_write_target(&input, self.id)`; their schemas add `"confirmed": { "type": "boolean", "description": "Set only by Aivyx PA after the operator approves replacing an existing file." }`.
  - `aivyx-tool/src/proxy.rs`: remove the `confirms` field, its two initialisers and the `if let Some(confirms) …` block in `execute` (restore `let turn_id = …` as the first line after the cancellation check). Delete the fixture binary and its test.

- [ ] **Step 4: Run tests**

Run: `cargo test -p aivyx-core --lib tools:: && cargo test -p aivyx-dataread && cargo test -p aivyx-tool`
Expected: PASS.

- [ ] **Step 5: Workspace check and commit**

Run: `cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
Expected: green. (e2e tests in `crates/aivyx-channel/tests/fs_tool_e2e.rs` that expected a `Failed` for an unconfirmed delete now see the turn escalate; update their assertion to `TurnOutcome::Escalated` — the test channel's `request_approval` is the default `Unavailable`.)

```bash
git add -A && git commit -s -m "refactor(tools): confirm-first tools just ask; the agent owns operator confirmation"
```

---

### Task 4: IPC messages

**Files:**
- Modify: `crates/aivyx-ipc/src/protocol.rs` (`FrontendMessage` ~line 2260; `StreamEventPayload` ~line 3039; `render_for_cli` ~line 3092; tests ~line 3540)
- Modify: `docs/DAEMON_IPC.md`

**Interfaces:**
- Produces:
  - `FrontendMessage::SetApprovals { enabled: bool }`
  - `FrontendMessage::ResolveApproval { request_id: String, approved: bool }`
  - `StreamEventPayload::ApprovalRequest { request_id: String, tool: String, summary: String, input: serde_json::Value, reason: String, expires_in_secs: u64 }`

- [ ] **Step 1: Failing round-trip test** — in protocol.rs tests:

```rust
    #[test]
    fn approval_messages_round_trip() {
        for msg in [
            FrontendMessage::SetApprovals { enabled: true },
            FrontendMessage::ResolveApproval { request_id: "a-1".into(), approved: false },
        ] {
            let json = serde_json::to_string(&msg).unwrap();
            assert_eq!(serde_json::from_str::<FrontendMessage>(&json).unwrap(), msg);
        }
        let ev = StreamEventPayload::ApprovalRequest {
            request_id: "a-1".into(),
            tool: "fs.delete".into(),
            summary: "fs.delete todo.md".into(),
            input: serde_json::json!({"path": "todo.md"}),
            reason: "deleting can't be undone".into(),
            expires_in_secs: 600,
        };
        let json = serde_json::to_string(&ev).unwrap();
        assert_eq!(serde_json::from_str::<StreamEventPayload>(&json).unwrap(), ev);
        assert!(ev.render_for_cli().contains("Approval needed — fs.delete todo.md"));
    }
```

- [ ] **Step 2: Run — expect compile failure** (`cargo test -p aivyx-ipc approval_messages_round_trip`).

- [ ] **Step 3: Add the variants** — to `FrontendMessage` (after `ResolveGate`):

```rust
    /// This connection can show approval prompts: tool calls that need the
    /// operator's approval pause and send `ApprovalRequest` instead of ending
    /// the turn. Off unless sent.
    SetApprovals { enabled: bool },
    /// The operator's answer to a `StreamEventPayload::ApprovalRequest`.
    ResolveApproval { request_id: String, approved: bool },
```

to `StreamEventPayload` (after `ApprovalGate`):

```rust
    /// A chat tool call is paused for the operator's approval; answer with
    /// `FrontendMessage::ResolveApproval`. Denied after `expires_in_secs`.
    ApprovalRequest {
        request_id: String,
        tool: String,
        summary: String,
        input: serde_json::Value,
        reason: String,
        expires_in_secs: u64,
    },
```

and a `render_for_cli` arm:

```rust
            StreamEventPayload::ApprovalRequest { summary, reason, .. } => {
                format!("\n  ⚑ Approval needed — {summary}\n    why: {reason}\n")
            }
```

Fix any other exhaustive matches on `StreamEventPayload` / `FrontendMessage` the compiler names (the daemon's frame match gets real arms in Task 5; until then add `FrontendMessage::SetApprovals { .. } | FrontendMessage::ResolveApproval { .. } => {}` there; the Studio's event match gets its real arm in Task 8 — add `StreamEventPayload::ApprovalRequest { .. } => {}` for now).

- [ ] **Step 4: Document** — in `docs/DAEMON_IPC.md` add a section "Chat approvals" describing the three messages, the opt-in, the 600 s expiry, and that a disconnect denies.

- [ ] **Step 5: Test, check, commit**

Run: `cargo test -p aivyx-ipc && cargo clippy --workspace --all-targets -- -D warnings && cargo clippy -p aivyx-web --all-targets -- -D warnings`
Expected: green.

```bash
git add -A && git commit -s -m "feat(ipc): SetApprovals, ApprovalRequest, ResolveApproval"
```

---

### Task 5: The daemon listens during a turn and asks through the bridge

**Files:**
- Create: `crates/aivyx-channel/src/approval_desk.rs`
- Modify: `crates/aivyx-channel/src/lib.rs` (`pub mod approval_desk;`)
- Modify: `crates/aivyx-channel/src/daemon_server.rs` (`handle_connection` read loop ~2146–3860; SubmitInput turn ~2478; ResolveGate resume turn ~2979; `IpcChannelBridge` ~8617)
- Test: `crates/aivyx-channel/tests/daemon_roundtrip_e2e.rs`

**Interfaces:**
- Consumes: Task 4 messages; Task 1 `ApprovalRequest`/`Approval`/`APPROVAL_TIMEOUT`.
- Produces:
  - `approval_desk::ApprovalDesk` — `new() -> Arc<Self>`, `async fn ask(&self, writer: &Arc<tokio::sync::Mutex<OwnedWriteHalf>>, session_id: &str, request: &ApprovalRequest, cancel: &CancellationToken) -> Approval`, `fn resolve(&self, request_id: &str, approved: bool)`, `fn deny_all(&self)`
  - `IpcChannelBridge.approvals: Option<Arc<ApprovalDesk>>` (Some only when the connection sent `SetApprovals { enabled: true }` and the submit isn't headless)

- [ ] **Step 1: Write `approval_desk.rs` with its unit test**

```rust
//! The daemon's side of chat approvals: sends an `ApprovalRequest` to the
//! connection's frontend and waits for the matching `ResolveApproval`
//! (routed here by the connection loop), a timeout, a cancel or a
//! disconnect.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use aivyx_core::{Approval, ApprovalRequest, CancellationToken, APPROVAL_TIMEOUT};
use tokio::io::AsyncWriteExt;
use tokio::sync::oneshot;

use crate::daemon_ipc::{encode_frame, DaemonMessage, StreamEventPayload};

#[derive(Default)]
pub struct ApprovalDesk {
    waiting: Mutex<HashMap<String, oneshot::Sender<bool>>>,
}

impl ApprovalDesk {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Ask and wait. A write failure, a closed desk, or `cancel` → `Denied`;
    /// no answer in `APPROVAL_TIMEOUT` → `TimedOut`.
    pub async fn ask<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        writer: &tokio::sync::Mutex<W>,
        session_id: &str,
        request: &ApprovalRequest,
        cancel: &CancellationToken,
    ) -> Approval {
        self.ask_with_timeout(writer, session_id, request, cancel, APPROVAL_TIMEOUT).await
    }

    pub(crate) async fn ask_with_timeout<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        writer: &tokio::sync::Mutex<W>,
        session_id: &str,
        request: &ApprovalRequest,
        cancel: &CancellationToken,
        timeout: std::time::Duration,
    ) -> Approval {
        let request_id = format!("approval-{}", uuid::Uuid::new_v4());
        let (tx, rx) = oneshot::channel();
        if let Ok(mut w) = self.waiting.lock() {
            w.insert(request_id.clone(), tx);
        }
        let msg = DaemonMessage::StreamEvent {
            session_id: session_id.to_string(),
            event: StreamEventPayload::ApprovalRequest {
                request_id: request_id.clone(),
                tool: request.tool.clone(),
                summary: request.summary.clone(),
                input: request.input.clone(),
                reason: request.reason.clone(),
                expires_in_secs: timeout.as_secs(),
            },
        };
        let sent = match encode_frame(&msg) {
            Ok(frame) => writer.lock().await.write_all(&frame).await.is_ok(),
            Err(_) => false,
        };
        let answer = if !sent {
            Approval::Denied
        } else {
            tokio::select! {
                r = rx => match r { Ok(true) => Approval::Approved, _ => Approval::Denied },
                _ = tokio::time::sleep(timeout) => Approval::TimedOut,
                _ = cancel.cancelled() => Approval::Denied,
            }
        };
        if let Ok(mut w) = self.waiting.lock() {
            w.remove(&request_id);
        }
        answer
    }

    /// Route a `ResolveApproval`. Unknown ids (already timed out) are ignored.
    pub fn resolve(&self, request_id: &str, approved: bool) {
        if let Some(tx) = self.waiting.lock().ok().and_then(|mut w| w.remove(request_id)) {
            let _ = tx.send(approved);
        }
    }

    /// The frontend went away: every open prompt is denied.
    pub fn deny_all(&self) {
        if let Ok(mut w) = self.waiting.lock() {
            for (_, tx) in w.drain() {
                let _ = tx.send(false);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon_ipc::decode_frame;

    fn request() -> ApprovalRequest {
        ApprovalRequest {
            tool: "fs.delete".into(),
            summary: "fs.delete todo.md".into(),
            input: serde_json::json!({"path": "todo.md"}),
            reason: "can't be undone".into(),
        }
    }

    async fn sent_request_id(buf: &tokio::sync::Mutex<Vec<u8>>) -> String {
        loop {
            let bytes = buf.lock().await.clone();
            if let Ok((DaemonMessage::StreamEvent { event: StreamEventPayload::ApprovalRequest { request_id, .. }, .. }, _)) =
                decode_frame::<DaemonMessage>(&bytes)
            {
                return request_id;
            }
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn an_answer_a_timeout_and_a_disconnect() {
        let desk = ApprovalDesk::new();
        let writer = Arc::new(tokio::sync::Mutex::new(Vec::<u8>::new()));
        let cancel = CancellationToken::new();

        let (d, w, c) = (Arc::clone(&desk), Arc::clone(&writer), cancel.clone());
        let asking = tokio::spawn(async move { d.ask(&w, "s", &request(), &c).await });
        let id = sent_request_id(&writer).await;
        desk.resolve(&id, true);
        assert_eq!(asking.await.unwrap(), Approval::Approved);

        let quick = desk
            .ask_with_timeout(&tokio::sync::Mutex::new(Vec::<u8>::new()), "s", &request(), &cancel,
                std::time::Duration::from_millis(10))
            .await;
        assert_eq!(quick, Approval::TimedOut);

        let writer2 = Arc::new(tokio::sync::Mutex::new(Vec::<u8>::new()));
        let (d, w, c) = (Arc::clone(&desk), Arc::clone(&writer2), cancel.clone());
        let asking = tokio::spawn(async move { d.ask(&w, "s", &request(), &c).await });
        sent_request_id(&writer2).await;
        desk.deny_all();
        assert_eq!(asking.await.unwrap(), Approval::Denied);
    }
}
```

(`encode_frame`/`decode_frame`/`DaemonMessage` are re-exported by `crate::daemon_ipc`; check the existing `use` lines at the top of `daemon_server.rs` and use the same paths.)

Run: `cargo test -p aivyx-channel --lib approval_desk`
Expected: PASS.

- [ ] **Step 2: Write the failing daemon e2e tests** — in `crates/aivyx-channel/tests/daemon_roundtrip_e2e.rs`, following the file's existing single-connection helper (the tests around line 553 build a `DaemonSessionConfig` and drive a raw socket), add:
  1. `approval_request_resumes_the_same_turn`: a scripted provider whose first step calls a confirm-first test tool (schema declares `confirmed`, escalates unless confirmed, returns `{"ran": true}` when confirmed) and whose second step is a final message. The frontend sends `StartSession`, `SetApprovals { enabled: true }`, `SubmitInput`; on receiving `StreamEvent { ApprovalRequest { request_id, .. } }` it sends `ResolveApproval { request_id, approved: true }`; it then expects `TurnComplete` whose outcome is completed, and the tool must have run exactly once with `confirmed: true`.
  2. `closing_the_connection_during_an_approval_denies_it`: same setup; on the `ApprovalRequest` the frontend drops the socket; the daemon side (the test holds the tool) must see no confirmed run within 2 s.
  3. `cancel_turn_mid_turn_is_read_during_the_turn`: a provider whose stream stalls until cancelled (await the cancellation token in `next_event`); the frontend sends `SubmitInput` then, 200 ms later, `CancelTurn`; expect `TurnComplete` within 5 s with a cancelled outcome.
  4. `a_frame_sent_during_a_turn_is_handled_after_it`: during a stalled-then-finishing turn the frontend sends a `Query { ListSessions }` (or the file's simplest query); the response must arrive after `TurnComplete`.

Run: `cargo test -p aivyx-channel --test daemon_roundtrip_e2e approval cancel_turn_mid frame_sent_during`
Expected: FAIL / hang-then-fail (bounded with `tokio::time::timeout(Duration::from_secs(10), …)` in each test).

- [ ] **Step 3: Reader task + deferred queue** — in `handle_connection`, replace the byte-read `loop { … read … buf … loop { match decode_frame(&buf) { Ok((msg, consumed)) => { buf.drain(..consumed); match msg { BODY } } Err(IncompleteBuf) => break, Err(e) => { ERR } } } }` scaffolding with:

```rust
    // Frames are decoded by a reader task so a running turn can still see
    // `CancelTurn` / `ResolveApproval` (see `drive_turn`).
    let (frame_tx, mut frames) =
        tokio::sync::mpsc::unbounded_channel::<Result<FrontendMessage, FrameError>>();
    let reader_task = tokio::spawn(async move {
        let mut buf = Vec::with_capacity(4096);
        let mut tmp = [0u8; 4096];
        loop {
            match reader.read(&mut tmp).await {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&tmp[..n]),
            }
            loop {
                match decode_frame::<FrontendMessage>(&buf) {
                    Ok((msg, consumed)) => {
                        buf.drain(..consumed);
                        if frame_tx.send(Ok(msg)).is_err() {
                            return;
                        }
                    }
                    Err(FrameError::IncompleteBuf) => break,
                    Err(e) => {
                        let _ = frame_tx.send(Err(e));
                        return;
                    }
                }
            }
        }
    });
    let mut deferred: std::collections::VecDeque<FrontendMessage> = Default::default();
    let mut approvals_enabled = false;

    loop {
        let next = match deferred.pop_front() {
            Some(msg) => Ok(msg),
            None => tokio::select! {
                f = frames.recv() => match f { Some(f) => f, None => break },
                _ = shutdown.cancelled() => {
                    send_shutting_down(&mut writer, "shutdown requested").await;
                    reader_task.abort();
                    return Ok(());
                }
            },
        };
        let msg = match next {
            Ok(msg) => msg,
            Err(e) => {
                /* ERR — the existing `Err(e) => { … return Err(e.into()); }` body, unchanged */
            }
        };
        match msg {
            /* BODY — the existing arms, unchanged, plus: */
            FrontendMessage::SetApprovals { enabled } => {
                approvals_enabled = enabled;
            }
            FrontendMessage::ResolveApproval { .. } => {
                // Only meaningful during a turn (handled in `drive_turn`); a
                // late answer to a timed-out prompt is ignored.
            }
        }
    }
    reader_task.abort();
```

Delete the old `buf`/`tmp` locals and the `shutdown.is_cancelled()` pre-check at the loop top (the `select!` covers it). The BODY moves out one nesting level — re-indent it (`rustfmt` is not enforced repo-wide; keep the edit a pure re-indent). The `buf.drain(..consumed);` line inside the old `Ok` arm is deleted.

- [ ] **Step 4: `drive_turn`** — add a free function in `daemon_server.rs`:

```rust
/// Run a turn while still serving the connection: `CancelTurn` cancels it,
/// `ResolveApproval` answers its prompt, a closed connection denies any open
/// prompt; every other frame is kept, in order, for after the turn.
async fn drive_turn<F: std::future::Future<Output = TurnOutcome>>(
    turn: F,
    frames: &mut tokio::sync::mpsc::UnboundedReceiver<Result<FrontendMessage, FrameError>>,
    deferred: &mut std::collections::VecDeque<FrontendMessage>,
    channel: Option<&Arc<dyn ChannelContext + Send + Sync>>,
    desk: Option<&crate::approval_desk::ApprovalDesk>,
) -> TurnOutcome {
    tokio::pin!(turn);
    let mut open = true;
    loop {
        tokio::select! {
            outcome = &mut turn => return outcome,
            f = frames.recv(), if open => match f {
                Some(Ok(FrontendMessage::CancelTurn { .. })) => {
                    if let Some(ch) = channel { ch.cancel_inflight(); }
                }
                Some(Ok(FrontendMessage::ResolveApproval { request_id, approved })) => {
                    if let Some(d) = desk { d.resolve(&request_id, approved); }
                }
                Some(Ok(other)) => deferred.push_back(other),
                Some(Err(_)) | None => {
                    open = false;
                    if let Some(d) = desk { d.deny_all(); }
                    if let Some(ch) = channel { ch.cancel_inflight(); }
                }
            },
        }
    }
}
```

(`cancel_inflight` is the method the existing `CancelTurn` arm calls on `ch`; use the same call.) At the SubmitInput site replace `let outcome = agent.turn(msg, &bridge).await;` with

```rust
let desk = bridge.approvals.clone();
let outcome = drive_turn(agent.turn(msg, &bridge), &mut frames, &mut deferred, channel.as_ref(), desk.as_deref()).await;
```

and do the same for the ResolveGate resume turn (`resume_outcome`).

- [ ] **Step 5: The bridge asks** — add `approvals: Option<Arc<crate::approval_desk::ApprovalDesk>>` to `IpcChannelBridge`; at both construction sites set it to `(approvals_enabled && !headless).then(crate::approval_desk::ApprovalDesk::new)` (the ResolveGate site has no `headless`; use `approvals_enabled.then(…)`). Implement in `impl ChannelContext for IpcChannelBridge`:

```rust
    async fn request_approval(&self, request: &aivyx_core::ApprovalRequest) -> aivyx_core::Approval {
        match &self.approvals {
            Some(desk) => {
                desk.ask(&self.writer, &self.session_id, request, &self.inner.cancellation_token()).await
            }
            None => aivyx_core::Approval::Unavailable,
        }
    }
```

- [ ] **Step 6: Run the e2e tests**

Run: `cargo test -p aivyx-channel --test daemon_roundtrip_e2e`
Expected: PASS (all old tests plus the four new ones).

- [ ] **Step 7: Workspace check and commit**

Run: `cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`

```bash
git add -A && git commit -s -m "feat(daemon): keep reading a connection during a turn; ask approvals through the bridge"
```

---

### Task 6: Terminal chat over the daemon asks

**Files:**
- Modify: `crates/aivyx-channel/src/daemon_client.rs` (`DaemonSession` ~line 42; `send_and_collect` ~line 216)
- Create: `crates/aivyx-channel/src/approval_prompt.rs` (terminal prompt text + answer parsing, shared with Task 7)
- Modify: `crates/aivyx-channel/src/lib.rs` (`pub mod approval_prompt;`)
- Modify: `crates/aivyx-cli/src/bin/aivyx_modules/repl_daemon.rs` (`run_connected`)

**Interfaces:**
- Consumes: Task 4 messages; Task 5 daemon behaviour.
- Produces:
  - `approval_prompt::render(summary: &str, reason: &str, input: &serde_json::Value) -> String`
  - `approval_prompt::is_yes(answer: &str) -> bool`
  - `approval_prompt::ask_tty(prompt_text: &str) -> Option<bool>` (reads one line from `/dev/tty`; `None` if the terminal can't be opened)
  - `type Approver = Arc<dyn Fn(&str, &str, &serde_json::Value) -> bool + Send + Sync>` in daemon_client.rs; `DaemonSession::enable_approvals(&mut self, approver: Approver) -> Result<(), DaemonError>` (stores it, sends `SetApprovals { enabled: true }`)

- [ ] **Step 1: `approval_prompt.rs` with tests**

```rust
//! The terminal's approval prompt — shared by the daemon-mode and
//! in-process chats.

use std::io::{BufRead, Write};

/// The block shown before asking.
pub fn render(summary: &str, reason: &str, input: &serde_json::Value) -> String {
    let args = input.to_string();
    let args = if args.chars().count() > 200 {
        format!("{}…", args.chars().take(200).collect::<String>())
    } else {
        args
    };
    format!(
        "\n⚑ Approval needed — {summary}\n  why: {reason}\n  args: {args}\n  Approve? [y/N] (10 min) "
    )
}

/// `y` / `yes` (any case) approves; anything else — Enter included — denies.
pub fn is_yes(answer: &str) -> bool {
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// Show `prompt_text` on the controlling terminal and read one line from it
/// (the chat loop owns stdin). `None` when there's no terminal to ask.
pub fn ask_tty(prompt_text: &str) -> Option<bool> {
    let mut tty = std::fs::OpenOptions::new().read(true).write(true).open("/dev/tty").ok()?;
    tty.write_all(prompt_text.as_bytes()).ok()?;
    tty.flush().ok()?;
    let mut line = String::new();
    std::io::BufReader::new(tty).read_line(&mut line).ok()?;
    Some(is_yes(&line))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_yes_approves() {
        for a in ["y", "Y", "yes", " YES \n"] {
            assert!(is_yes(a), "{a:?}");
        }
        for a in ["", "\n", "n", "no", "yep", "sure"] {
            assert!(!is_yes(a), "{a:?}");
        }
    }

    #[test]
    fn the_prompt_names_the_action_and_trims_long_args() {
        let long = serde_json::json!({"content": "x".repeat(500)});
        let text = render("fs.write notes.txt", "overwriting can't be undone", &long);
        assert!(text.contains("⚑ Approval needed — fs.write notes.txt"));
        assert!(text.contains("why: overwriting can't be undone"));
        assert!(text.contains("…"));
        assert!(text.trim_end().ends_with("Approve? [y/N] (10 min)"));
    }
}
```

Run: `cargo test -p aivyx-channel --lib approval_prompt` → PASS.

- [ ] **Step 2: Client hook — failing test** — in `crates/aivyx-channel/tests/daemon_roundtrip_e2e.rs` add `terminal_client_answers_an_approval_through_its_approver`: the Task 5 daemon setup, but the frontend is a real `DaemonSession`; call `session.enable_approvals(Arc::new(|_, _, _| true)).await`, then `session.submit_input("delete it".into()).await`; expect `Ok` with a completed outcome and the tool run once with `confirmed: true`. A second session with `Arc::new(|_, _, _| false)` must get a completed turn and no confirmed run.

- [ ] **Step 3: Implement** — in `DaemonSession` add `approver: Option<Approver>` (initialised `None` in `connect`) and:

```rust
    /// Show approval prompts: tool calls that need the operator's yes pause
    /// and `approver(summary, reason, input)` answers them.
    pub async fn enable_approvals(&mut self, approver: Approver) -> Result<(), DaemonError> {
        self.approver = Some(approver);
        let frame = encode_frame(&FrontendMessage::SetApprovals { enabled: true })?;
        self.writer.lock().await.write_all(&frame).await?;
        Ok(())
    }
```

In `send_and_collect`'s `StreamEvent` arm:

```rust
                Ok((DaemonEnvelope::StreamEvent { event, .. }, consumed)) => {
                    self.buf.drain(..consumed);
                    if let (StreamEventPayload::ApprovalRequest { request_id, summary, reason, input, .. }, Some(approver)) =
                        (&event, self.approver.clone())
                    {
                        let (s, r, i) = (summary.clone(), reason.clone(), input.clone());
                        let approved = tokio::task::spawn_blocking(move || approver(&s, &r, &i))
                            .await
                            .unwrap_or(false);
                        let answer = FrontendMessage::ResolveApproval { request_id: request_id.clone(), approved };
                        let frame = encode_frame(&answer)?;
                        self.writer.lock().await.write_all(&frame).await?;
                    }
                    events.push(event);
                }
```

In `repl_daemon.rs::run_connected`, make `session` mutable and, before building `daemon_config`, when stdin is a terminal:

```rust
    if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        let approver: aivyx_channel::daemon_client::Approver = std::sync::Arc::new(|summary, reason, input| {
            let text = aivyx_channel::approval_prompt::render(summary, reason, input);
            aivyx_channel::approval_prompt::ask_tty(&text).unwrap_or(false)
        });
        session.enable_approvals(approver).await.map_err(|e| e.to_string())?;
    }
```

- [ ] **Step 4: Run tests** — `cargo test -p aivyx-channel --test daemon_roundtrip_e2e terminal_client && cargo test -p aivyx-channel --lib approval_prompt` → PASS.

- [ ] **Step 5: Check and commit** — `cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`

```bash
git add -A && git commit -s -m "feat(repl): the terminal chat answers approval prompts over the daemon"
```

---

### Task 7: In-process terminal chat asks

**Files:**
- Modify: `crates/aivyx-channel/src/local.rs` (`LocalChannel` ~line 56; its `ChannelContext` impl)
- Modify: `crates/aivyx-cli/src/bin/aivyx.rs` (Local arm near `let session_config = SessionConfig {` ~line 10898 where `channel` is built)
- Test: `crates/aivyx-channel/tests/cli_e2e.rs`

**Interfaces:**
- Consumes: `approval_prompt::render`, `approval_prompt::ask_tty` (Task 6); Task 1 hook.
- Produces: `LocalChannel::with_approver(self, approver: Arc<dyn Fn(&ApprovalRequest) -> Approval + Send + Sync>) -> Self`

- [ ] **Step 1: Failing test** — in `cli_e2e.rs` add `in_process_chat_asks_and_runs_the_approved_call`: `run_session` with a `LocalChannel` built `.with_approver(Arc::new(|_| Approval::Approved))`, `empty_session_config` whose `tools` registry holds a confirm-first test tool (schema declares `confirmed`; escalates unless confirmed; records inputs) and whose `capabilities` include its scope; a `RecordingProvider` scripted with a tool call to it then a final message (reuse the file's `ScriptedProvider`/`final_step` helpers; add a tool-call step helper mirroring the file's existing one). Assert the tool ran with `confirmed: true` and `report.turns_run == 1`.

- [ ] **Step 2: Run — expect failure** (`with_approver` missing).

- [ ] **Step 3: Implement** — `LocalChannel` gains `approver: Option<Arc<dyn Fn(&aivyx_core::ApprovalRequest) -> aivyx_core::Approval + Send + Sync>>` (`None` in `new`), the builder above, and in its `ChannelContext` impl:

```rust
    async fn request_approval(&self, request: &aivyx_core::ApprovalRequest) -> aivyx_core::Approval {
        let Some(approver) = self.approver.clone() else {
            return aivyx_core::Approval::Unavailable;
        };
        let request = request.clone();
        let ask = tokio::task::spawn_blocking(move || approver(&request));
        match tokio::time::timeout(aivyx_core::APPROVAL_TIMEOUT, ask).await {
            Ok(Ok(answer)) => answer,
            Ok(Err(_)) => aivyx_core::Approval::Denied,
            Err(_) => aivyx_core::Approval::TimedOut,
        }
    }
```

In `aivyx.rs`, where the Local arm builds its `LocalChannel`, add (only when stdin is a terminal):

```rust
            let channel = if io::stdin().is_terminal() {
                channel.with_approver(Arc::new(|r: &aivyx_core::ApprovalRequest| {
                    let text = aivyx_channel::approval_prompt::render(&r.summary, &r.reason, &r.input);
                    match aivyx_channel::approval_prompt::ask_tty(&text) {
                        Some(true) => aivyx_core::Approval::Approved,
                        Some(false) => aivyx_core::Approval::Denied,
                        None => aivyx_core::Approval::Unavailable,
                    }
                }))
            } else {
                channel
            };
```

- [ ] **Step 4: Run tests** — `cargo test -p aivyx-channel --test cli_e2e` → PASS.

- [ ] **Step 5: Check and commit** — workspace clippy + tests green.

```bash
git add -A && git commit -s -m "feat(repl): in-process chat asks approval prompts on the terminal"
```

---

### Task 8: Studio approval card

**Files:**
- Modify: `crates/aivyx-channel/src/web_ui.rs` (~line 905: after sending `StartSession`, send `SetApprovals { enabled: true }`)
- Modify: `crates/aivyx-web/src/main.rs` (context signal; `ws_task` event arm ~line 11311; `ChatPanel` ~line 4365; new `ApprovalCard` component)
- Modify: `crates/aivyx-web/assets/stitch.css`
- Rebuild: `crates/aivyx-web/dist/`

**Interfaces:**
- Consumes: Task 4 messages; Task 5 daemon.
- Produces: `struct PendingApproval { request_id: String, summary: String, reason: String, input: serde_json::Value, expires_at_ms: f64 }` (`Clone, PartialEq`), a `Signal<Option<PendingApproval>>` context, `#[component] fn ApprovalCard(p: PendingApproval) -> Element`, `fn approval_args_preview(input: &serde_json::Value) -> String` (pure, tested).

- [ ] **Step 1: Failing pure test** (in a new `#[cfg(test)] mod approval_card_tests` in main.rs):

```rust
    #[test]
    fn args_preview_is_pretty_and_bounded() {
        let small = super::approval_args_preview(&serde_json::json!({"path": "todo.md"}));
        assert!(small.contains("\"path\": \"todo.md\""), "{small}");
        let big = super::approval_args_preview(&serde_json::json!({"content": "x".repeat(5000)}));
        assert!(big.chars().count() <= 2001 && big.ends_with('…'), "{}", big.len());
    }
```

Run: `cargo test -p aivyx-web approval_card_tests` → FAIL (fn missing).

- [ ] **Step 2: Implement** —

```rust
/// A chat tool call waiting for Approve / Deny.
#[derive(Clone, PartialEq)]
struct PendingApproval {
    request_id: String,
    summary: String,
    reason: String,
    input: serde_json::Value,
    expires_at_ms: f64,
}

fn approval_args_preview(input: &serde_json::Value) -> String {
    let pretty = serde_json::to_string_pretty(input).unwrap_or_default();
    if pretty.chars().count() > 2000 {
        format!("{}…", pretty.chars().take(2000).collect::<String>())
    } else {
        pretty
    }
}

#[component]
fn ApprovalCard(p: PendingApproval) -> Element {
    let ws = use_context::<Sender>();
    let mut pending = use_context::<Signal<Option<PendingApproval>>>();
    let mut transcript = use_context::<Signal<Vec<ChatLine>>>();
    let answer = move |approved: bool| {
        let p = p.clone();
        move |_| {
            ws.send(FrontendMessage::ResolveApproval { request_id: p.request_id.clone(), approved });
            let mark = if approved { "✓ approved" } else { "✗ declined" };
            transcript.write().push(ChatLine::system(format!("{mark}: {}", p.summary)));
            pending.set(None);
        }
    };
    let minutes_left = ((p.expires_at_ms - js_sys::Date::now()) / 60_000.0).ceil().max(0.0);
    rsx! {
        div { class: "glass-card approval-card",
            h4 { "⚑ Approval needed — {p.summary}" }
            p { class: "muted", "{p.reason}" }
            details { summary { "Arguments" } pre { class: "approval-args", "{approval_args_preview(&p.input)}" } }
            div { class: "approval-actions",
                button { class: "btn", onclick: answer(true), "Approve" }
                button { class: "btn ghost", onclick: answer(false), "Deny" }
                span { class: "label-tech", "answer within {minutes_left} min" }
            }
        }
    }
}
```

Provide the context next to the existing `gate` signal in `App` (`let approval = use_signal(|| None::<PendingApproval>); use_context_provider(|| approval);`), pass it into `ws_task` like `gate`, and add the event arm:

```rust
                    StreamEventPayload::ApprovalRequest { request_id, summary, reason, input, expires_in_secs, .. } => {
                        approval.set(Some(PendingApproval {
                            request_id, summary, reason, input,
                            expires_at_ms: js_sys::Date::now() + (expires_in_secs as f64) * 1000.0,
                        }));
                    }
```

On `TurnComplete` for the Studio's session, if an approval is still pending, clear it and push `ChatLine::system(format!("✗ not approved (no answer): {}", p.summary))`. In `ChatPanel`, render `if let Some(p) = use_context::<Signal<Option<PendingApproval>>>()() { ApprovalCard { p } }` directly after the transcript `div`. CSS:

```css
.approval-card { border-color: var(--color-warn, #d97706); margin: 8px 0; }
.approval-args { max-height: 240px; overflow: auto; white-space: pre-wrap; }
.approval-actions { display: flex; gap: 10px; align-items: center; flex-wrap: wrap; margin-top: 10px; }
```

In `web_ui.rs`, right after the `StartSession` frame is written (~line 905), write `encode_frame(&FrontendMessage::SetApprovals { enabled: true })` the same way.

- [ ] **Step 3: Tests and bundle** — `cargo test -p aivyx-web && cargo clippy -p aivyx-web --all-targets -- -D warnings`, then rebuild the bundle (Global Constraints) and delete `.br` files.

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -s -m "feat(studio): an approval card in Chat — Approve or Deny, and the turn goes on"
```

---

### Task 9: Docs, live verification, changelog

**Files:**
- Modify: `docs/guide/04-chat-and-missions.md` (a "When it asks first" section: the prompt in the terminal and the Studio card, 10 minutes, deny by default, chat apps reply "yes")
- Modify: `docs/guide/08-access-and-settings.md` (safety-net box: "it pauses and asks you"; chat apps: "reply to approve")
- Modify: `docs/SECURITY_POSTURE.md` (§3: the agent owns confirmation; prompts; the ledger fallback)
- Modify: `CHANGELOG.md` (`[Unreleased]` → Added: chat approval prompts; Fixed: Ctrl-C mid-turn over the daemon)
- Rebuild: Studio bundle (guide pages are compiled in)

- [ ] **Step 1: Write the docs** (plain words, same style as the surrounding pages).

- [ ] **Step 2: Live terminal check** — in an isolated home with Lemonade running (see the scratchpad harness from the 2026-09-30 audits: `env.sh`, `drive2.py`): put `todo.md` in the sandbox, run `aivyx-pa` (daemon mode), ask "Please delete todo.md". Expect the `⚑ Approval needed — fs.delete …` block; answer `y` → the file is gone and the reply continues. Repeat, answer Enter → the file stays and the reply says it wasn't deleted. Repeat with `--no-daemon`.

- [ ] **Step 3: Live Studio check** — headless Chrome via `cdp.mjs`: ask to delete a file in Chat, wait for `.approval-card`, click **Approve** → the file is gone and "✓ approved" appears; repeat with **Deny**.

- [ ] **Step 4: Live Ctrl-C check** — daemon mode, ask for a long answer, Ctrl-C after 2 s: the prompt returns within a few seconds with the turn cancelled.

- [ ] **Step 5: Full sweep and commit**

Run: `cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace && cargo clippy -p aivyx-web --all-targets -- -D warnings && cargo test -p aivyx-web`

```bash
git add -A && git commit -s -m "docs: chat approval prompts; changelog"
```
