# Supervised Batching Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** At the `supervised` autonomy level (per area), an unattended run parks an exact needs-approval call for later review instead of refusing it; the operator approves or denies parked steps in the Studio or the terminal, and an approved step runs exactly as parked.

**Architecture:** The turn loop's existing approval hook (`ChannelContext::request_approval`) gains a `Parked { id }` answer. A new `ParkingChannel` wrapper (aivyx-channel) sits around the channel of every unattended run (trigger engine: routines, webhooks, file watches, reflection, loop iterations; team missions: lead + specialists) and, for a call whose area is `supervised`, writes it to a new encrypted `ParkedSteps` store via a shared `StepParker`. The daemon answers new IPC queries from that store and runs an approved step once through a new `Agent::run_approved_call` (no model involved).

**Tech Stack:** Rust workspace (tokio, async-trait, serde, redb via aivyx-storage), Dioxus wasm Studio (aivyx-web).

Spec: `docs/superpowers/specs/2026-10-07-supervised-batching-design.md`.

## Global Constraints

- No `supervised` area anywhere (global level and every override) ⇒ no parker is built ⇒ behaviour byte-identical to today.
- Nothing new becomes gated: only calls that already come back `RequiresEscalation` can park.
- Attended conversations are unchanged.
- The model's tool result for a parked call is exactly: ``This step needs the operator's approval, so it was parked for review (id `<id>`) and not taken. Don't rely on it having happened.``
- Parked input never contains a model-supplied `confirmed` key.
- An approved step runs **once**; resolving a non-pending id is an error.
- A failed parking write falls back to refusal (today's behaviour) and is audited — never a silent drop or an unparked run.
- `[autonomy] review_expiry_days`, default 7, must be ≥ 1.
- Resolved entries kept: the newest 200.
- Verification for every task: `cargo test -p <crate>` for touched crates; at the end the full sweep: `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, the Rust 1.99 clippy (`touch crates/aivyx-config/src/lib.rs; CARGO_TARGET_DIR=target/rust199 ~/.cargo/bin/cargo +1.99.0 clippy --workspace --all-targets -- -D warnings`), `PATH=$HOME/.cargo/bin:$PATH just check-web`, `cargo check -p aivyx-cli --features channel-voice,yubikey`.
- The workspace is not rustfmt-clean; match surrounding style, never reformat files.
- Commits: `git commit -s`, message ending `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. CHANGELOG must not gain `Authorization: Bearer` text. BUSL-1.1 is "source-available", never "open source".

---

### Task 1: Core — `Parked` answer, scope in the request, `run_approved_call`

**Files:**
- Modify: `crates/aivyx-core/src/lib.rs` (`ApprovalRequest`, `Approval`, `Agent` trait, re-export)
- Modify: `crates/aivyx-core/src/agent.rs` (`ConfirmAllAreas` → `AreaFlags`, `seek_approval`, `run_approved_call`)
- Modify: `crates/aivyx-channel/src/approval_desk.rs` (test constructor gains the new fields)
- Test: `crates/aivyx-core/src/agent.rs` tests module

**Interfaces:**
- Produces:
  - `pub struct AreaFlags { pub default: bool, pub areas: BTreeMap<String, bool> }` with `everywhere(bool)`, `for_base(&str) -> bool`, `any() -> bool`; `pub type ConfirmAllAreas = AreaFlags;` (existing call sites keep compiling).
  - `ApprovalRequest { tool, summary, input, reason, scope_base: String, trust_tier: TrustTier }`.
  - `enum Approval { Approved, Denied, TimedOut, Unavailable, Parked { id: String } }` — no longer `Copy`.
  - `pub fn parked_message(id: &str) -> String` (aivyx-core, the exact model-facing text).
  - `Agent::run_approved_call(&self, tool: &str, input: serde_json::Value, channel: &dyn ChannelContext) -> Result<String, String>` — default `Err("this agent can't run approved calls")`; `ConcreteAgent` overrides.

- [ ] **Step 1: Write the failing tests** (agent.rs tests module, beside the existing chat-approval tests at ~6404 that use a `request_approval`-overriding test channel)

```rust
#[tokio::test]
async fn parked_answer_becomes_a_parked_tool_result_and_the_turn_continues() {
    // A channel that parks every request, recording what it was asked.
    // Reuse the existing escalating test tool + scripted planner used by
    // `approved_call_reruns_with_operator_approved` (same module): first
    // step calls the escalating tool, second step finishes.
    let (agent, channel) = approval_fixture(crate::Approval::Parked { id: "ab12cd34".into() });
    let outcome = agent.turn(test_message(), &channel).await;
    assert!(matches!(outcome, TurnOutcome::Completed { .. }), "{outcome:?}");
    let seen = channel.requests();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].scope_base, "fs.delete");
    assert_eq!(seen[0].trust_tier, TrustTier::Trusted);
    assert!(seen[0].input.get("confirmed").is_none());
    let observed = channel.tool_results();
    assert!(observed.iter().any(|r| r.contains(&crate::parked_message("ab12cd34"))));
}

#[tokio::test]
async fn run_approved_call_runs_once_with_operator_approval() {
    let (agent, tool_runs) = approved_call_fixture(/* grants fs.delete */ true);
    let out = agent
        .run_approved_call("fs.delete", serde_json::json!({"path": "a.txt"}), &silent_channel())
        .await;
    assert!(out.is_ok(), "{out:?}");
    assert_eq!(tool_runs.load(Ordering::SeqCst), 1);
    assert!(tool_runs_saw_operator_approved());
}

#[tokio::test]
async fn run_approved_call_refuses_a_missing_capability() {
    let (agent, tool_runs) = approved_call_fixture(/* grants fs.delete */ false);
    let err = agent
        .run_approved_call("fs.delete", serde_json::json!({"path": "a.txt"}), &silent_channel())
        .await
        .unwrap_err();
    assert!(err.contains("not granted"), "{err}");
    assert_eq!(tool_runs.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn run_approved_call_refuses_an_unknown_tool() {
    let (agent, _) = approved_call_fixture(true);
    let err = agent.run_approved_call("nope.tool", serde_json::json!({}), &silent_channel()).await.unwrap_err();
    assert!(err.contains("no longer exists"), "{err}");
}

#[test]
fn area_flags_any() {
    assert!(!AreaFlags::everywhere(false).any());
    assert!(AreaFlags::everywhere(true).any());
    let mut f = AreaFlags::everywhere(false);
    f.areas.insert("fs".into(), true);
    assert!(f.any() && f.for_base("fs.delete") && !f.for_base("email.send"));
}
```

`approval_fixture`, `approved_call_fixture`, `silent_channel` and `tool_runs_saw_operator_approved` are small helpers built from the module's existing test tools/planners (the escalating tool returns `RequiresEscalation` unless `ctx.operator_approved`, and counts executions).

- [ ] **Step 2: Run to verify failure** — `cargo test -p aivyx-core parked_ run_approved_call area_flags` → compile errors (`Parked`, `scope_base`, `run_approved_call`, `AreaFlags` missing).

- [ ] **Step 3: Implement**

lib.rs:

```rust
pub struct ApprovalRequest {
    // …existing fields…
    /// The capability base the call needs (`fs.delete`) — its first word is
    /// the autonomy area.
    pub scope_base: String,
    /// The trust tier of the channel the call ran on; an approved parked
    /// call runs at this tier again.
    pub trust_tier: TrustTier,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Approval {
    Approved,
    Denied,
    TimedOut,
    Unavailable,
    /// Not taken now: stored for the operator's later review under `id`.
    Parked { id: String },
}

/// What the model is told about a parked call.
pub fn parked_message(id: &str) -> String {
    format!(
        "This step needs the operator's approval, so it was parked for review (id `{id}`) and not taken. Don't rely on it having happened."
    )
}

pub trait Agent: Send + Sync {
    // …
    /// Run one tool call the operator has approved (a parked step), with no
    /// model involved: capability check → audit → execute → audit, as a
    /// normal call, with `operator_approved` set. `Ok` carries a short
    /// result; `Err` says why nothing (or a failure) happened.
    async fn run_approved_call(
        &self,
        _tool: &str,
        _input: serde_json::Value,
        _channel: &dyn ChannelContext,
    ) -> Result<String, String> {
        Err("this agent can't run approved calls".into())
    }
}
```

agent.rs — rename `ConfirmAllAreas` to `AreaFlags` (doc: "An on/off setting per autonomy area…"), add `pub type ConfirmAllAreas = AreaFlags;` and:

```rust
    /// Whether any area is on.
    pub fn any(&self) -> bool {
        self.default || self.areas.values().any(|v| *v)
    }
```

Re-export `AreaFlags` next to `ConfirmAllAreas` in lib.rs.

`seek_approval`: build the request with

```rust
        let tool = self.tools.get(tool_id)?;
        let tool_name = tool.name().to_string();
        // …strip `confirmed` as today…
        let scope_base = tool.required_scope(&shown).base().to_string();
        let request = crate::ApprovalRequest {
            tool: tool_name.clone(),
            summary: approval_summary(&tool_name, &shown),
            input: shown.clone(),
            reason: reason.to_string(),
            scope_base,
            trust_tier: env.channel.trust_tier(),
        };
```

match the answer by reference (`match &answer`), label `Parked { .. } => "parked"`, and in the non-approved arm:

```rust
                let detail = match &answer {
                    crate::Approval::TimedOut => "No answer within 10 minutes, so this action was not taken.".to_string(),
                    crate::Approval::Parked { id } => crate::parked_message(id),
                    _ => "The operator declined this action.".to_string(),
                };
```

`ConcreteAgent::run_approved_call` (in `impl Agent for ConcreteAgent`):

```rust
    async fn run_approved_call(
        &self,
        tool: &str,
        input: serde_json::Value,
        channel: &dyn ChannelContext,
    ) -> Result<String, String> {
        let Some(tool_id) = self.tools.find_by_name(tool) else {
            return Err(format!("the tool `{tool}` no longer exists"));
        };
        let effective = self.capabilities.intersect(channel.trust_tier().default_ceiling());
        let cancellation = channel.cancellation_token();
        let env = TurnCallEnv {
            turn_id: TurnId::new(),
            channel,
            cancellation: &cancellation,
            effective: &effective,
            message_origin: MessageOrigin::Operator,
        };
        let (_, outcome, _) = self
            .run_tool_call(
                &env,
                crate::planner::ToolCallRequest {
                    tool_id,
                    input,
                    auto_corrected_from: None,
                    extracted_from_text: None,
                    operator_approved: true,
                },
            )
            .await;
        match outcome {
            ToolOutcome::Completed { output, .. } => {
                let text = match output {
                    serde_json::Value::String(s) => s,
                    other => other.to_string(),
                };
                Ok(text.chars().take(500).collect())
            }
            ToolOutcome::Denied { scope, .. } => Err(format!("the capability `{scope}` is not granted any more")),
            ToolOutcome::NotInRole { tool_name } => Err(format!("the active role doesn't allow `{tool_name}`")),
            ToolOutcome::RateLimited { reason, .. } => Err(reason),
            ToolOutcome::RequiresEscalation { reason, .. } => Err(reason),
            ToolOutcome::Failed(e) => Err(e.to_string()),
        }
    }
```

(Match the real `ToolOutcome` variant list; if more variants exist, map each to `Err` with its reason.)

Fix every compile error from `Approval` losing `Copy` (use `.clone()` or match by reference) and from the two new `ApprovalRequest` fields (`approval_desk.rs` test: `scope_base: "fs.delete".into(), trust_tier: TrustTier::Trusted`).

- [ ] **Step 4: Run** — `cargo test -p aivyx-core` and `cargo test -p aivyx-channel approval` → PASS.

- [ ] **Step 5: Commit** — `feat(core): parked approvals and run_approved_call`.

---

### Task 2: Shared types — storage domain, IPC types, audit events

**Files:**
- Modify: `crates/aivyx-storage/src/lib.rs` (new `KeyDomain::ParkedSteps`; `ALL` 27→28; subkeys array 27→28; drift test 28; exhaustive matches)
- Modify: `README.md` (both "27" storage-domain figures → 28)
- Create: `crates/aivyx-ipc/src/parked.rs`; Modify: `crates/aivyx-ipc/src/lib.rs` (`pub mod parked;`)
- Modify: `crates/aivyx-ipc/src/protocol.rs` (`QueryPayload::{GetParkedSteps, ResolveParkedStep}`, `QueryResponsePayload::{ParkedSteps, ParkedStepResolved}`)
- Modify: `crates/aivyx-ipc/src/briefing.rs` (`NeedsYouAction::ParkedStep { id, preview }`)
- Modify: `crates/aivyx-audit/src/lib.rs` (`StepParked`, `StepParkFailed`, `ParkedStepResolved`)
- Modify: `crates/aivyx-channel/src/daemon_server.rs:~8124` and `crates/aivyx-cli/src/bin/aivyx_modules/audit_export.rs:~235` (kind names)

**Interfaces:**
- Produces:

```rust
// aivyx_ipc::parked
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParkedState { Pending, Approved, Denied, Lapsed, Failed }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParkedStep {
    pub id: String,
    pub tool: String,
    pub input: serde_json::Value,
    pub summary: String,
    pub reason: String,
    pub area: String,
    /// Which run parked it: "routine digest", "webhook trigger deploy", "team mission tm-…".
    pub origin: String,
    pub trust_tier: aivyx_capability::TrustTier,
    pub parked_at: i64,
    pub state: ParkedState,
    pub resolved_at: Option<i64>,
    /// A short result: the tool's output or error, or why it lapsed/failed.
    pub result: Option<String>,
    /// The current state of what the step touches — filled when listed,
    /// never stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}
```

  - `QueryPayload::GetParkedSteps` → `QueryResponsePayload::ParkedSteps { steps: Vec<ParkedStep> }` (newest first, all states).
  - `QueryPayload::ResolveParkedStep { id: String, approve: bool }` → `QueryResponsePayload::ParkedStepResolved { step: ParkedStep }` or `QueryError { code: "resolve_parked_step_failed", .. }`.
  - `NeedsYouAction::ParkedStep { id: String, preview: Option<String> }`.
  - Audit: `StepParked { id, tool, summary, area, origin }`, `StepParkFailed { tool, origin, reason }`, `ParkedStepResolved { id, tool, outcome }` (outcome: `approved` | `denied` | `lapsed` | `failed`).

- [ ] **Step 1: Failing tests**

aivyx-storage (beside the RoutingTaint tests):

```rust
#[test]
fn parked_steps_domain_has_stable_metadata() {
    assert_eq!(KeyDomain::ParkedSteps.as_bytes(), b"parked-steps");
    assert_eq!(KeyDomain::ParkedSteps.table_name(), "aivyx_parked_steps_v1");
    assert!(KeyDomain::ALL.contains(&KeyDomain::ParkedSteps));
}
```

and change `key_domain_count_matches_docs` to expect 28.

aivyx-ipc `parked.rs`:

```rust
#[test]
fn parked_queries_round_trip() {
    let step = sample();
    for p in [QueryPayload::GetParkedSteps, QueryPayload::ResolveParkedStep { id: "ab12cd34".into(), approve: true }] {
        let back: QueryPayload = serde_json::from_str(&serde_json::to_string(&p).unwrap()).unwrap();
        assert_eq!(back, p);
    }
    let r = QueryResponsePayload::ParkedSteps { steps: vec![step.clone()] };
    let back: QueryResponsePayload = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
    assert_eq!(back, r);
}

#[test]
fn preview_is_optional_on_the_wire() {
    let mut v = serde_json::to_value(sample()).unwrap();
    v.as_object_mut().unwrap().remove("preview");
    let back: ParkedStep = serde_json::from_value(v).unwrap();
    assert_eq!(back.preview, None);
}
```

(If `QueryPayload`/`QueryResponsePayload` don't derive `PartialEq`, compare re-serialised JSON strings instead.)

aivyx-audit: a round-trip test that each new event serialises and appends to a scratch `PersistentAuditLog` and verifies (copy the existing `HeadlessRefusal` append test's setup).

- [ ] **Step 2: Run** — `cargo test -p aivyx-storage -p aivyx-ipc -p aivyx-audit` → compile failures.

- [ ] **Step 3: Implement** — storage: add the variant after `RoutingTaint` with a doc comment ("Supervised batching — tool calls parked by unattended runs for the operator's review; one row per step keyed by its id"), `b"parked-steps"`, `"aivyx_parked_steps_v1"`, append to `ALL`, add `derive_subkey` + `subkeys[27]`, widen `[SubKey; 27]`/`[KeyDomain; 27]` to 28, add to the exhaustive `| KeyDomain::RoutingTaint => {}` arm. README lines 38 and 298: 27 → 28. IPC and audit: the types above, documented like their neighbours. Kind names: `"StepParked"`, `"StepParkFailed"`, `"ParkedStepResolved"`.

- [ ] **Step 4: Run** — same command → PASS; `cargo check --workspace` (catches every exhaustive match on `QueryPayload` / `NeedsYouAction` / `AuditEvent` — add a minimal arm where one is required; the Studio arm is filled in Task 6, use `NeedsYouAction::ParkedStep { .. } => rsx! {}` for now if `check-web` needs it).

- [ ] **Step 5: Commit** — `feat: parked-steps storage domain, IPC types and audit events`.

---

### Task 3: The parked-step store, `StepParker` and `ParkingChannel`

**Files:**
- Create: `crates/aivyx-channel/src/parked_steps.rs`
- Modify: `crates/aivyx-channel/src/lib.rs` (`pub mod parked_steps;`)

**Interfaces:**
- Consumes: Task 1 (`ApprovalRequest`, `Approval::Parked`, `AreaFlags`, `Agent::run_approved_call`), Task 2 (types, domain, audit events).
- Produces:

```rust
pub const KEEP_RESOLVED: usize = 200;

#[derive(Debug, thiserror::Error)]
pub enum ParkError {
    #[error("parked-step storage error: {0}")] Storage(String),
    #[error("no parked step with id `{0}`")] NotFound(String),
    #[error("parked step `{id}` is already {state}")] NotPending { id: String, state: String },
}

pub struct StepParker { /* store, supervised: AreaFlags, expiry_secs: i64,
    audit_log, notify: Option<(Arc<NotifyDispatcher>, String)>,
    preview_root: Option<PathBuf>, resolve_lock: tokio::sync::Mutex<()> */ }

impl StepParker {
    pub fn new(storage: DomainHandle, supervised: AreaFlags, expiry_days: u32) -> Self;
    pub fn with_audit_log(self, log: Arc<PersistentAuditLog>) -> Self;
    pub fn with_notify(self, dispatcher: Arc<NotifyDispatcher>, target: String) -> Self;
    pub fn with_preview_root(self, root: PathBuf) -> Self;
    pub fn parks(&self, scope_base: &str) -> bool;
    /// Park `req` (or return the id of an identical pending step). Storage
    /// failure → audited `StepParkFailed`, returns `Approval::Unavailable`.
    pub async fn park(&self, req: &ApprovalRequest, origin: &str) -> Approval;
    /// Every step, newest first, after lapsing expired ones; pending steps carry a preview.
    pub async fn list(&self) -> Result<Vec<ParkedStep>, ParkError>;
    /// Lapse expired pending steps; returns how many.
    pub async fn lapse_expired(&self) -> Result<usize, ParkError>;
    /// Resolve one pending step. On approve, `run` executes it once; its
    /// `Ok`/`Err` becomes `approved`/`failed` with the result text.
    pub async fn resolve<F, Fut>(&self, id: &str, approve: bool, run: F) -> Result<ParkedStep, ParkError>
    where F: FnOnce(ParkedStep) -> Fut, Fut: Future<Output = Result<String, String>>;
}

pub struct ParkingChannel { inner: Arc<dyn ChannelContext + Send + Sync>, parker: Arc<StepParker>, origin: String }
impl ParkingChannel { pub fn new(inner, parker, origin: impl Into<String>) -> Self; }
// ChannelContext: delegates everything to `inner` (like `TierOverride`), except
// `request_approval`: `if parker.parks(&req.scope_base) { parker.park(req, &origin).await } else { inner.request_approval(req).await }`.

/// Daemon IPC: answer `GetParkedSteps` / `ResolveParkedStep`; `None` for any other query.
pub async fn handle_parked_query(
    payload: &QueryPayload,
    parker: Option<&StepParker>,
    agent: &Arc<dyn Agent>,
    channel_factory: &ChannelFactory,
) -> Option<QueryResponsePayload>;

/// The current state of what a step touches.
pub fn preview_for(step: &ParkedStep, root: Option<&Path>) -> Option<String>;
```

Details:
- id: first 8 hex chars of a v4 UUID (`uuid::Uuid::new_v4().simple().to_string()[..8]`), re-drawn if taken. Key = id bytes; value = `serde_json::to_vec(&step)` with `preview: None`.
- Dedupe: if a pending step has the same `tool` and `input`, return `Parked { id: existing }` without a new row or a new notification.
- Notification (when set): `dispatcher.dispatch(&target, &format!("The {origin} parked a step for your review: {summary} (aivyx-pa review)."), None)`; a dispatch error is logged with `eprintln!`, never fails the park.
- Expiry: `pending && now - parked_at > expiry_secs` → `Lapsed`, `resolved_at = now`, `result = "Not reviewed within N days."`, audited `ParkedStepResolved { outcome: "lapsed" }`. Run by `list`, `resolve` (before checking state) and `lapse_expired`.
- Retention after every resolve/lapse: delete resolved rows beyond the newest `KEEP_RESOLVED` by `resolved_at`.
- `resolve` holds `resolve_lock` for its whole body so two concurrent approvals can't both run.
- Preview: for `tool` starting `fs.` with a string `input.path` (relative → joined to `root`): existing file → `"Now: {path} ({n} bytes)\n{first 1500 chars, lossy}"`; directory → `"Now: {path} is a folder"`; missing → `"Now: {path} doesn't exist."`; otherwise `serde_json::to_string_pretty(&input)`.
- `handle_parked_query` approve runner: `let ch = TierOverride::new(channel_factory(FrontendType::Local), step.trust_tier); agent.run_approved_call(&step.tool, step.input.clone(), &ch).await`. `parker == None` → `QueryError { code: "parked_steps_unavailable", message: "No area is supervised, so nothing is parked." }`.

- [ ] **Step 1: Failing tests** (in `parked_steps.rs`; scratch store set up like `conflict_dismissals.rs` tests)

```rust
fn req(tool: &str, base: &str, path: &str) -> ApprovalRequest {
    ApprovalRequest {
        tool: tool.into(),
        summary: format!("{tool} {path}"),
        input: serde_json::json!({ "path": path }),
        reason: "deletes a file".into(),
        scope_base: base.into(),
        trust_tier: TrustTier::Trusted,
    }
}
fn supervised_fs() -> AreaFlags { let mut f = AreaFlags::everywhere(false); f.areas.insert("fs".into(), true); f }

#[tokio::test] async fn park_round_trips_and_lists_pending() {
    let s = Scratch::new(); let p = parker(&s, supervised_fs(), 7).await;
    let Approval::Parked { id } = p.park(&req("fs.delete", "fs.delete", "a.txt"), "routine tidy").await else { panic!() };
    let steps = p.list().await.unwrap();
    assert_eq!(steps.len(), 1);
    assert_eq!((steps[0].id.as_str(), steps[0].state, steps[0].area.as_str(), steps[0].origin.as_str()), (id.as_str(), ParkedState::Pending, "fs", "routine tidy"));
}
#[tokio::test] async fn identical_pending_step_is_not_parked_twice() { /* park same req twice → same id, list len 1 */ }
#[tokio::test] async fn approve_runs_once_and_records_the_result() {
    // resolve(id, true, runner counting calls returning Ok("deleted")) → state Approved, result "deleted", count 1;
    // second resolve → Err(NotPending), count still 1.
}
#[tokio::test] async fn deny_never_runs() { /* runner panics if called; state Denied */ }
#[tokio::test] async fn failed_run_is_recorded_as_failed() { /* runner Err("capability gone") → Failed, result contains it */ }
#[tokio::test] async fn unknown_id_is_an_error() { /* NotFound */ }
#[tokio::test] async fn expired_pending_step_lapses_and_cannot_be_approved() {
    // park, then rewrite the row with parked_at = now - 8 days; list → Lapsed; resolve → NotPending.
}
#[tokio::test] async fn retention_keeps_the_newest_resolved() {
    // write KEEP_RESOLVED + 5 resolved rows directly, resolve one more → KEEP_RESOLVED resolved rows remain, newest kept.
}
#[tokio::test] async fn parking_channel_parks_only_supervised_areas() {
    // ParkingChannel over a channel whose request_approval returns Unavailable:
    // fs.delete → Parked; email.send → Unavailable; nothing stored for the email call.
}
#[tokio::test] async fn storage_failure_falls_back_to_refusal() {
    // A StepParker over a store opened with one master key, then reopened with another
    // (decrypt failure on scan) — or a DomainHandle on a dropped/closed DB, whichever the
    // storage tests already use for a failing handle → park returns Unavailable.
}
#[test] fn preview_shows_the_file_as_it_is_now() { /* temp dir: existing, missing, folder, non-fs tool */ }
```

- [ ] **Step 2: Run** — `cargo test -p aivyx-channel parked_steps` → fails (module missing).
- [ ] **Step 3: Implement** the module as specified.
- [ ] **Step 4: Run** — `cargo test -p aivyx-channel parked_steps` → PASS.
- [ ] **Step 5: Commit** — `feat(channel): parked-step store, StepParker and ParkingChannel`.

---

### Task 4: Wire parking into unattended runs and the daemon

**Files:**
- Modify: `crates/aivyx-channel/src/trigger.rs` (`with_step_parker`, wrap the channel in `fire`, `origin_label`)
- Modify: `crates/aivyx-channel/src/team_mission_driver.rs` (`TeamMissionService::with_step_parker`, wrap the lead channel when headless)
- Modify: `crates/aivyx-team/src/pool.rs` (specialist approvals go to the lead channel)
- Modify: `crates/aivyx-channel/src/daemon_server.rs` (`DaemonConfig.step_parker`, `ConnectionContext.step_parker`, query intercept, hourly lapse task, briefing source)
- Modify: `crates/aivyx-channel/src/activity.rs` (`ResolveParkedStep` is an operator action)
- Modify: `crates/aivyx-channel/src/daemon_client.rs` (`get_parked_steps`, `resolve_parked_step`)
- Modify: every `DaemonConfig { .. }` literal (`step_parker: None` in fixtures)
- Test: `crates/aivyx-channel/src/trigger.rs` tests; `crates/aivyx-team/src/pool.rs` tests; `crates/aivyx-channel/tests/daemon_roundtrip_e2e.rs`

**Interfaces:**
- Consumes: Task 3 (`StepParker`, `ParkingChannel`, `handle_parked_query`).
- Produces: `TriggerDispatch::with_step_parker(Arc<StepParker>) -> Self`; `TeamMissionService::with_step_parker(self, Arc<StepParker>) -> Self`; `DaemonConfig.step_parker: Option<Arc<StepParker>>`; `daemon_client::get_parked_steps(&Path) -> Result<Vec<ParkedStep>, DaemonError>`; `daemon_client::resolve_parked_step(&Path, String, bool) -> Result<ParkedStep, DaemonError>`.

Details:
- trigger.rs `fire`: after the existing `channel` binding,

```rust
        // Supervised batching — an unattended run parks a needs-approval
        // call in a `supervised` area for the operator's review instead of
        // refusing it.
        let channel: Arc<dyn aivyx_core::ChannelContext + Send + Sync> =
            match &self.step_parker {
                Some(parker) if self.gate_policy.is_headless() => Arc::new(
                    crate::parked_steps::ParkingChannel::new(channel, Arc::clone(parker), origin_label(source, trigger_id)),
                ),
                _ => channel,
            };
```

  with `fn origin_label(source: TriggerSource, id: &str) -> String`: Cron → `routine {id}` (strip `cfg-`), Webhook → `webhook {id}`, FileWatch → `file watch {id}`, Reflection → `reflection`, Loop → `autonomous loop`, Mission → `mission {id}`.
- team_mission_driver: `TeamMissionService` gains `step_parker: Option<Arc<StepParker>>`; where `MissionLeadChannel::new()` is built for a run (~1767), build `Arc<dyn ChannelContext + Send + Sync>` = `ParkingChannel::new(Arc::new(MissionLeadChannel::new()), parker, format!("team mission {id}"))` when the run's gate policy is headless and a parker is set, else the bare lead channel; pass `&*channel` to `run_until_pause`. Thread the parker through to that function the same way `gate_policy` reaches it (~1467).
- pool.rs `run`: wrap the specialist channel so approvals go to the lead:

```rust
/// A specialist's channel whose approval requests go to the lead's channel
/// (which, for an unattended mission, parks them).
struct LeadApprovals<'a> { inner: SpecialistChannel, lead: &'a dyn ChannelContext }
// ChannelContext for LeadApprovals: delegate every method to `inner`; `request_approval` → `self.lead.request_approval(r).await`.
```

  Interactive missions are unchanged (`MissionLeadChannel` answers `Unavailable`, the default).
- daemon_server: `DaemonConfig.step_parker`, cloned into `ConnectionContext`; in the `FrontendMessage::Query` arm, before `handle_query`:

```rust
                            } else if let Some(r) = crate::parked_steps::handle_parked_query(
                                &payload, step_parker.as_deref(), &agent, &channel_factory,
                            ).await {
                                r
                            } else {
```

  Pass the parker to `trigger_dispatch.with_step_parker(..)` where the dispatch is built (~915). Spawn an hourly task (first tick skipped, `tokio::select!` on `shutdown`) calling `parker.lapse_expired()`, logging errors. Add `parked: Option<&StepParker>` to `BriefingSources` (Task 6 consumes it in `gather`; here pass `step_parker.as_deref()`).
- activity.rs: add `| QueryPayload::ResolveParkedStep { .. }`.

- [ ] **Step 1: Failing tests**
  - trigger.rs: `headless_fire_parks_a_supervised_call` — a stub agent whose `turn` calls `channel.request_approval(&req("fs.delete"))` and records the answer; with a parker supervising `fs`, the answer is `Parked`; without a parker it is `Unavailable`; with an `Interactive` gate policy it is `Unavailable`.
  - pool.rs: `specialist_approval_requests_go_to_the_lead` — a lead channel that answers `Parked { id: "x" }`; a stub specialist agent that asks; the answer seen is `Parked`.
  - activity.rs: extend the existing table test with `ResolveParkedStep` → true.
  - e2e (`daemon_roundtrip_e2e.rs`, using the existing daemon fixture): `GetParkedSteps` with no parker → `QueryError` code `parked_steps_unavailable`; with a parker seeded with one pending step → `ParkedSteps` of length 1; `ResolveParkedStep { approve: false }` → `ParkedStepResolved` with state `Denied`; resolving again → `QueryError`.
  - End to end (spec): `routine_at_supervised_parks_fs_delete_then_approve_deletes` — in `crates/aivyx-channel/tests/` with a real `ConcreteAgent` holding the real `fs.delete` tool (confirm_destructive on) and a scripted planner that calls `fs.delete` on a temp file then finishes: fire through `TriggerDispatch` (headless, parker supervising `fs`) → the file still exists and one step is pending; `handle_parked_query(ResolveParkedStep { approve: true })` → the file is gone, state `Approved`. A second test denies → the file still exists. (Look for an existing integration test that builds `ConcreteAgent` with `FsDeleteTool` and a scripted planner, and reuse its setup.)
- [ ] **Step 2: Run** — `cargo test -p aivyx-channel -p aivyx-team` → failures.
- [ ] **Step 3: Implement** as above.
- [ ] **Step 4: Run** — `cargo test -p aivyx-channel -p aivyx-team` → PASS.
- [ ] **Step 5: Commit** — `feat: park supervised calls from unattended runs; review queries`.

---

### Task 5: Config, daemon start-up, and `aivyx-pa review`

**Files:**
- Modify: `crates/aivyx-config/src/lib.rs` (`RawAutonomy.review_expiry_days`, `AivyxConfig.autonomy_review_expiry_days: u32`, validation)
- Test: `crates/aivyx-config/src/tests.rs`
- Modify: `scripts/gen-config-reference.py` (meaning) → regenerate `docs/manual/reference/02-configuration.md`
- Modify: `crates/aivyx-cli/src/bin/aivyx.rs` (destructure the new field; `area_flags` helper; build the parker; `CliMode::Review`; parse; dispatch)
- Create: `crates/aivyx-cli/src/bin/aivyx_modules/review.rs`
- Modify: `crates/aivyx-cli/src/bin/aivyx_modules/help.rs` (entry), `docs/manual/reference/01-cli.md` (heading `### \`aivyx-pa review\``)

**Interfaces:**
- Consumes: Tasks 3–4.
- Produces: `fn area_flags(level, overrides, on: impl Fn(AutonomyPosture) -> bool) -> aivyx_core::AreaFlags`; `confirm_all_areas` becomes `area_flags(level, overrides, |p| matches!(p.gate, GatePosture::ConfirmAll))`; `supervised_areas(level, overrides) = area_flags(.., |p| matches!(p.gate, GatePosture::BatchIrreversible))`.

Details:
- Config: absent → 7 (`FieldSource::Default` not needed — plain `u32`); `0` → `ConfigError::Invalid { field: "autonomy.review_expiry_days", reason: "must be at least 1 (days)" }`.
- Gen script meaning: `"autonomy.review_expiry_days": "How many days a step parked for your review (areas at `supervised`) waits before it lapses unreviewed. Default 7."`
- Daemon start-up (where `notify_dispatcher`, `default_notify_target_name`, `fs_root` and `storage` exist, before `DaemonConfig`):

```rust
    // Supervised batching — only built when some area is `supervised`; with
    // none, unattended runs refuse needs-approval calls exactly as before.
    let supervised = supervised_areas(autonomy_level.value, &autonomy_overrides);
    let step_parker = supervised.any().then(|| {
        let mut p = aivyx_channel::parked_steps::StepParker::new(
            storage.domain(KeyDomain::ParkedSteps), supervised, autonomy_review_expiry_days,
        )
        .with_audit_log(Arc::clone(&persistent_audit_for_query))
        .with_preview_root(fs_root.clone());
        if let Some(t) = &default_notify_target_name {
            p = p.with_notify(Arc::clone(&notify_dispatcher), t.clone());
        }
        Arc::new(p)
    });
```

  Pass `step_parker.clone()` to `DaemonConfig` and `.with_step_parker(..)` on the team-mission service when `Some`. (Use the variable names actually in scope; check `cargo check -p aivyx-cli --features channel-voice,yubikey` too.)
- `review.rs`:

```rust
//! `aivyx-pa review` — steps that unattended runs parked for your approval
//! (areas at the `supervised` autonomy level).

pub async fn run_review(cmd: ReviewCommand) -> Result<(), String>;
pub enum ReviewCommand { List, Approve { id: String, yes: bool }, Deny { id: String } }
pub fn render_list(steps: &[ParkedStep], now: i64) -> String; // pure, unit-tested
```

  `List`: pending steps as `"{id}  {summary}\n      from {origin}, {ago} ago — {reason}"`, then a line "N resolved recently (newest first):" with up to 5 `"{id}  {state}  {summary}"`; none pending → `"Nothing is waiting for your review."`. `Approve`: find the step (unknown/non-pending → error naming its state), print summary + preview, ask `Run this step now? [y/N] ` on stdin unless `yes`; then `resolve_parked_step(.., true)` and print `"Approved — {result}"` or `"Failed — {result}"`. `Deny`: resolve false, print `"Denied."`. Daemon not running → same message pattern as `learning.rs`.
- Parse (beside `learning`): `review`, `review list`, `review approve <id> [--yes]`, `review deny <id>`; anything else → error ``unrecognized argument to `aivyx-pa review`: `…` ``.
- help.rs entry (group `Agent`, after `loop`): name `review`, summary `"Review steps parked for your approval"`, usage the three forms.
- 01-cli.md, in "Your agent" after `loop`: `### \`aivyx-pa review\`` with the forms and two sentences on what a parked step is and that approve runs the exact call once.

- [ ] **Step 1: Failing tests** — config: `review_expiry_days_defaults_to_7`, `review_expiry_days_reads_toml`, `review_expiry_days_zero_is_rejected`. aivyx.rs: `supervised_areas_from_overrides` (global assisted + `fs = supervised` → `for_base("fs.delete")` true, `email.send` false, `any()` true; no overrides at assisted → `any()` false; global supervised → `default` true); parse tests for each `review` form and a bad one. review.rs: `render_list` for empty, pending + resolved. help drift test passes once the heading exists.
- [ ] **Step 2: Run** — `cargo test -p aivyx-config review_expiry; cargo test -p aivyx-cli review supervised_areas` → fail.
- [ ] **Step 3: Implement**; run `python3 scripts/gen-config-reference.py` to regenerate.
- [ ] **Step 4: Run** — `cargo test -p aivyx-config -p aivyx-cli` → PASS.
- [ ] **Step 5: Commit** — `feat(cli): aivyx-pa review and [autonomy] review_expiry_days`.

---

### Task 6: Studio card, briefing, docs, CHANGELOG

**Files:**
- Modify: `crates/aivyx-channel/src/briefing.rs` (`ParkedFact`, `Facts.parked_pending`, `Facts.parked_resolved`, `gather`, `compose`)
- Modify: `crates/aivyx-web/src/command_center.rs` (`NeedsYouAction::ParkedStep` buttons + preview)
- Modify: `docs/guide/08-access-and-settings.md` (drop "planned but not built yet"), `docs/guide/15-autonomy-and-routines.md` (new section "Reviewing parked steps"), `docs/AUTONOMY.md` (closeout row + open question resolved), `docs/DAEMON_IPC.md` (the two queries), `CHANGELOG.md` (Unreleased → Added)

**Interfaces:**
- Consumes: `BriefingSources.parked` (Task 4), `StepParker::list` (Task 3), `NeedsYouAction::ParkedStep` (Task 2).

Details:
- `ParkedFact { id, summary, origin, parked_at, preview: Option<String> }`; `gather`: `parker.list()` → pending into `parked_pending`; resolved with `resolved_at >= window_start` into `parked_resolved: Vec<(String /*summary*/, ParkedState, i64)>`; an error pushes `"parked steps"` to `source_errors`.
- `compose`, right after team gates: one card per pending step — key `parked:{id}`, sentence ``A {origin} is waiting for your go-ahead: {summary}.``, detail `Parked {ago} ago.`, action `ParkedStep { id, preview }`, link `"command-center"` (use the slug the Command Center view actually has). Log lines for resolved: approved → `You approved a parked step: {summary}.`; denied → `You turned down a parked step: {summary}.`; lapsed → `A parked step lapsed unreviewed: {summary}.` (warn); failed → `A parked step you approved failed: {summary}.` (warn).
- Studio: `NeedsYouAction::ParkedStep { id, preview }` → optional `pre { class: "preview", "{preview}" }` and Approve / Deny buttons sending `action_query(QueryPayload::ResolveParkedStep { id, approve })`, same shape as `TeamGate`.
- Guide 15 section: what parks (deletes/overwrites, integration writes, git commits, anything that would ask) when the area is `supervised` and nobody's there; where to review (Command Center "Needs you", `aivyx-pa review`); approve runs the exact call once; deny drops it; `review_expiry_days` (default 7); the notification; a config example:

```toml
[autonomy]
level = "assisted"
review_expiry_days = 7

[[autonomy.override]]
domain = "fs"
level = "supervised"
```

- CHANGELOG `### Added`: "Supervised batching: at the `supervised` autonomy level (globally or per area), an unattended run (routine, webhook, file watch, the loop, a team mission) parks a step that needs your approval instead of refusing it. Review parked steps in the Command Center or with `aivyx-pa review`; an approved step runs exactly as parked. Unreviewed steps lapse after `[autonomy] review_expiry_days` (default 7)."

- [ ] **Step 1: Failing tests** — briefing: `compose_lists_pending_parked_steps_with_preview`, `compose_logs_resolved_parked_steps` (each state's sentence and warn flag).
- [ ] **Step 2: Run** — `cargo test -p aivyx-channel briefing` → fail.
- [ ] **Step 3: Implement** briefing + Studio + docs.
- [ ] **Step 4: Run** — `cargo test -p aivyx-channel briefing`; `PATH=$HOME/.cargo/bin:$PATH just check-web` → PASS.
- [ ] **Step 5: Full sweep** (Global Constraints list) → all green.
- [ ] **Step 6: Commit** — `feat(studio): parked steps in Needs you; docs`.
