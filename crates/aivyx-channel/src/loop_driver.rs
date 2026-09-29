//! Phase 173 — the autonomous-loop driver (the Aivyx Ralph loop).
//!
//! A background task, sibling of
//! [`crate::reflection_scheduler::run_reflection_scheduler`], that
//! drives the autonomous loop: while a run is active it fires a
//! **fresh-context** `TriggerSource::Loop` agent turn per
//! iteration, re-arming until the backlog is empty or a hard cap
//! is hit. Each turn is one full pass of the Ralph cycle —
//! `loop.next` → implement → run gates (`shell`) → commit
//! (`git`) → `loop.complete` — carried by the canonical
//! [`LOOP_SYSTEM_PROMPT`].
//!
//! ## Control model
//!
//! The driver owns a shared [`LoopRunState`] the daemon IPC
//! handlers (Phase 173 Task 5) flip:
//!
//! - `aivyx-pa loop start` → [`request_start`] sets `active` + the
//!   per-run `max_iterations` and wakes the driver via a
//!   `Notify`.
//! - `aivyx-pa loop stop` → [`request_stop`] clears `active`; the
//!   driver checks between iterations and ends the run.
//! - `aivyx-pa loop status` → [`snapshot`] reads the state.
//!
//! ## Termination (fully autonomous, capped)
//!
//! Per the Phase 173 entry decision, a run is fully autonomous —
//! no per-iteration operator gate — and ends only on one of the
//! [`LoopDecision`] stop conditions: the backlog drains, the
//! per-run `max_iterations` cap is reached, or an operator stop
//! is requested. The cap + capability gating + the audit chain
//! (every iteration is a `TriggerSource::Loop` entry) are the
//! guardrails. Token-budget + wall-clock caps and driver-side
//! gate verification are Phase 174.
//!
//! [`request_start`]: SharedLoopState::request_start
//! [`request_stop`]: SharedLoopState::request_stop
//! [`snapshot`]: SharedLoopState::snapshot

use std::sync::{Arc, RwLock};
use std::time::Duration;

use aivyx_core::CancellationToken;
use tokio::sync::Notify;

use crate::loop_backlog::PersistentLoopBacklog;
use crate::trigger::{TriggerDispatch, TriggerSource};

/// The canonical system-prompt-shaped instruction every loop
/// iteration carries. Deliberately Ralph-faithful: one story,
/// gates before completion, commit, then mark done — and stop
/// cleanly when the backlog is empty.
pub const LOOP_SYSTEM_PROMPT: &str = "\
You are one iteration of an autonomous task loop. A fresh context \
runs this same instruction each iteration; durable state lives in \
the backlog, your memory, your workspace, and — for code work — \
the project's git history, not in this conversation. Stories may \
be code OR everyday work (research, writing, organizing); fit your \
approach to the story in front of you.

Do exactly this, then stop:
1. Call `loop.next` to get the highest-priority pending story. If \
   it reports the backlog is empty, stop immediately and report \
   that the backlog is complete — do not invent work.
2. Do ONLY that one story. Keep the work small and focused; do \
   not start the next story. If the story is genuinely large or \
   spans several specialists (research + code + review, say), you \
   MAY instead delegate it to a durable agent team with `team.run` \
   (pass the story as the goal). The team mission runs in the \
   background and is tracked separately; if you delegate, skip the \
   verification steps below and go straight to step 5 (mark the \
   story complete — it is now the team's). Delegate sparingly: \
   most stories you should just do yourself.
3. Verify your work before claiming it is done — choose the check \
   that fits the task. For a code change in a project, run its \
   quality gates with `shell` (build + tests / typecheck) and do \
   NOT proceed on red. For research or writing, re-read what you \
   produced and confirm it actually answers the story. For a file \
   or note, read it back. If the check fails, fix it or stop; \
   never mark a story done on a failed check.
4. Persist the result so it outlives this context. A code change \
   belongs in a commit (`git`, a focused message naming the story \
   — once the gates are green, and only if committing is available \
   to you). Research and notes belong in your memory; a requested \
   document or file belongs at the path the story asked for. \
   Drafting, not done, is not progress.
5. Only after the work is verified and persisted, call \
   `loop.complete` with the story's id.
6. Record what the next iteration should know by calling \
   `loop.note` with one short line (a gotcha, a convention, a \
   path, a decision). These notes are surfaced back to you under \
   \"Progress so far\" at the top of every future iteration, so \
   future-you can avoid re-learning what you just learned.

Be conservative: it is always correct to stop without completing \
a story if you are unsure or a check failed. The loop will \
re-run and the next fresh context can try again.";

/// Phase 175 — render the progress-log block prepended to the
/// canonical prompt each iteration. `notes` are most-recent-
/// first (as `Memory::get_recent` returns them); the block lists
/// them oldest-first so the agent reads them in the order they
/// were learned. Returns an empty string when there are no notes
/// (the iteration then gets the plain canonical prompt).
pub fn render_progress_block(notes: &[String]) -> String {
    if notes.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "## Progress so far (from earlier iterations — read this first)\n\n",
    );
    for note in notes.iter().rev() {
        let line = note.trim();
        if !line.is_empty() {
            out.push_str("- ");
            out.push_str(line);
            out.push('\n');
        }
    }
    out.push('\n');
    out
}

/// Phase 175 — assemble one iteration's full prompt: the
/// progress block (if any) followed by the canonical loop
/// instruction. Pure so the assembly is unit-testable.
pub fn build_iteration_prompt(notes: &[String]) -> String {
    let block = render_progress_block(notes);
    if block.is_empty() {
        LOOP_SYSTEM_PROMPT.to_string()
    } else {
        format!("{block}{LOOP_SYSTEM_PROMPT}")
    }
}

/// Phase 175 — read the last `count` progress notes from the
/// reserved [`crate::loop_tool::LOOP_PROGRESS_TOPIC`] topic,
/// most-recent-first. `count = 0`, no memory, or a read error
/// all yield an empty vector (injection disabled / degraded).
pub async fn read_progress_notes(
    memory: Option<&Arc<dyn aivyx_memory::Memory>>,
    count: u32,
) -> Vec<String> {
    if count == 0 {
        return Vec::new();
    }
    let Some(memory) = memory else {
        return Vec::new();
    };
    match memory
        .get_recent(crate::loop_tool::LOOP_PROGRESS_TOPIC, count as usize)
        .await
    {
        Ok(entries) => entries.into_iter().map(|e| e.body).collect(),
        Err(_) => Vec::new(),
    }
}

/// Chapter Circuit (CI.1) — read the single most-recent progress
/// note, used as a cheap "did this iteration record a learning?"
/// probe for the stall breaker. `count = 1`; `None`/error → `None`
/// (treated as no note, never breaks the run).
async fn recent_progress_note(
    memory: Option<&Arc<dyn aivyx_memory::Memory>>,
) -> Option<String> {
    read_progress_notes(memory, 1).await.into_iter().next()
}

/// Chapter Foreman — append a one-line note to the loop progress log (the same
/// reserved topic `loop.note` writes), so a delegation decision shows up in the
/// next iteration's injected context + the operator's view. Best-effort.
async fn write_progress_note(
    memory: Option<&Arc<dyn aivyx_memory::Memory>>,
    text: &str,
) {
    if let Some(mem) = memory {
        let _ = mem.put(crate::loop_tool::LOOP_PROGRESS_TOPIC, text).await;
    }
}

/// Chapter Capstone — verify-and-close. When the agent did the work but never
/// called `loop.complete` (small local models routinely forget the closing
/// step, or report it in chat), the story is left pending and the run
/// eventually stalls out on it. If `candidate` is still pending, judge it
/// against its own acceptance criteria — the judge grounds on the memory /
/// workspace artifacts the turn produced — and mark it `Done` iff the judge
/// passes. Genuinely-incomplete work stays pending (the grounded judge rejects
/// it). Returns what happened: [`CloseResult::Closed`] (⇒ the iteration made
/// progress), [`CloseResult::JudgedIncomplete`] (a Verdict FAIL — the signal
/// `on_failure` escalation arms the next iteration on), or
/// [`CloseResult::NotNeeded`] (the story was already resolved).
async fn verify_and_close(
    backlog: &Arc<PersistentLoopBacklog>,
    judge: &crate::completion_judge::CompletionJudge,
    candidate: &aivyx_ipc::backlog::Story,
    memory: Option<&Arc<dyn aivyx_memory::Memory>>,
) -> CloseResult {
    let still_pending = matches!(
        backlog.get(&candidate.id).map(|s| s.status),
        Some(aivyx_ipc::backlog::StoryStatus::Pending)
    );
    if !still_pending {
        // The agent DID close it this turn (or it was skipped) — nothing to do.
        return CloseResult::NotNeeded;
    }
    let v = judge
        .verify(
            &candidate.title,
            &candidate.body,
            "(the agent did not call loop.complete this iteration — judge ONLY \
             from the ground-truth artifacts below; if they satisfy the \
             acceptance criteria, PASS)",
        )
        .await;
    eprintln!(
        "aivyx-pa loop: verify-and-close for '{}' — {}: {}",
        candidate.title,
        if v.passed { "CLOSED" } else { "left pending" },
        v.reason,
    );
    if v.passed {
        let _ = backlog
            .mark_done(candidate.id.clone(), now_unix_ms())
            .await;
        write_progress_note(
            memory,
            &format!(
                "auto-closed '{}' — work verified against the acceptance \
                 criteria (agent omitted loop.complete)",
                candidate.title
            ),
        )
        .await;
        CloseResult::Closed
    } else {
        CloseResult::JudgedIncomplete
    }
}

/// Chapter Capstone — the outcome of [`verify_and_close`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CloseResult {
    /// The judge passed; the story was marked done.
    Closed,
    /// Verdict FAIL — the story was still pending and the judge rejected it.
    JudgedIncomplete,
    /// The story was already resolved; the judge wasn't consulted.
    NotNeeded,
}

/// Model routing Part 3b (A16) — the audit spelling of the loop's
/// `on_failure` signals.
const SIGNAL_VERDICT_FAIL: &str = "loop_verdict_fail";
const SIGNAL_STALL_RESCUE: &str = "loop_stall_rescue";

/// Model routing Part 3b (A16) — decide what an iteration's outcome means
/// for `on_failure` escalation. `close` is the iteration's verify-and-close
/// result, `stalled` whether the stall breaker just tripped, `rescue_used`
/// whether this run already spent its one armed rescue, and `enabled`
/// whether the loop escalates at all (`mode = "auto"` and `on_failure`).
///
/// Returns `(arm the next fire with, stop the run, rescue_used after)`:
/// - disabled ⇒ nothing arms and a stall stops exactly as before;
/// - a stall with the rescue unspent ⇒ don't stop, arm
///   `"loop_stall_rescue"`, spend the rescue;
/// - a stall with the rescue spent ⇒ stop;
/// - otherwise a Verdict FAIL arms `"loop_verdict_fail"`.
fn next_arm(
    close: CloseResult,
    stalled: bool,
    rescue_used: bool,
    enabled: bool,
) -> (Option<&'static str>, bool, bool) {
    if !enabled {
        return (None, stalled, rescue_used);
    }
    if stalled {
        return if rescue_used {
            (None, true, true)
        } else {
            (Some(SIGNAL_STALL_RESCUE), false, true)
        };
    }
    match close {
        CloseResult::JudgedIncomplete => (Some(SIGNAL_VERDICT_FAIL), false, rescue_used),
        CloseResult::Closed | CloseResult::NotNeeded => (None, false, rescue_used),
    }
}

/// Chapter Foreman follow-up — how many times a single story may fail
/// auto-delegation in a run before it is skipped. After this, re-running a full
/// (slow, costly) team mission every iteration is clearly not productive, so the
/// story is marked `Skipped` and the loop moves on to other work.
const MAX_DELEGATION_ATTEMPTS: u32 = 2;

/// Record a failed auto-delegation of `story_id`: bump its per-run attempt
/// count, and once it reaches [`MAX_DELEGATION_ATTEMPTS`] mark the story
/// `Skipped` (a resolution → progress) instead of leaving it to be re-delegated.
/// Returns whether the iteration made progress (a skip resolves the story; an
/// under-cap failure does not, so repeated failures still feed the stall
/// breaker as a backstop).
async fn record_failed_delegation(
    backlog: &PersistentLoopBacklog,
    memory: Option<&Arc<dyn aivyx_memory::Memory>>,
    attempts: &mut std::collections::HashMap<String, u32>,
    story_id: &str,
    story_title: &str,
    detail: &str,
) -> bool {
    let n = {
        let c = attempts.entry(story_id.to_string()).or_insert(0);
        *c += 1;
        *c
    };
    if n >= MAX_DELEGATION_ATTEMPTS {
        let _ = backlog
            .mark_skipped(
                story_id.to_string(),
                now_unix_ms(),
                Some(format!(
                    "auto-delegation {detail}; skipped after {n} failed attempt(s)"
                )),
            )
            .await;
        write_progress_note(
            memory,
            &format!(
                "skipped story '{story_title}' — delegation {detail} ({n} attempts)"
            ),
        )
        .await;
        attempts.remove(story_id);
        true
    } else {
        write_progress_note(
            memory,
            &format!(
                "delegation of '{story_title}' {detail}; left pending \
                 (attempt {n}/{MAX_DELEGATION_ATTEMPTS})"
            ),
        )
        .await;
        false
    }
}

/// Chapter Circuit (CI.1) — the cross-iteration stall breaker.
///
/// The driver fires a fresh-context iteration and — unlike Bridle's
/// *within-turn* repeat-call breaker — cannot see the turn's tool
/// calls. It judges progress by observable state instead: an
/// iteration "made progress" iff it completed/delegated a story
/// (the backlog shrank) **or** recorded a fresh progress note. N
/// consecutive iterations with neither is a stall — the loop is
/// spinning on something it can't get past (the v0.7.4 bug, where
/// every iteration was denied at `loop.next`, recorded nothing, and
/// the backlog never moved, is the canonical case). Stopping then
/// saves the rest of the iteration/token budget the run would
/// otherwise burn re-failing identically.
///
/// `max_idle == 0` disables the breaker (the caps become the only
/// stop). Pure + tiny so the counting logic is unit-tested without
/// a driver harness.
#[derive(Debug)]
struct StallTracker {
    max_idle: u32,
    consecutive_idle: u32,
}

impl StallTracker {
    fn new(max_idle: u32) -> Self {
        Self { max_idle, consecutive_idle: 0 }
    }

    /// Record one iteration's progress. Returns `true` when the run
    /// should stop (the consecutive-idle count reached `max_idle`).
    fn record(&mut self, made_progress: bool) -> bool {
        if self.max_idle == 0 {
            return false;
        }
        if made_progress {
            self.consecutive_idle = 0;
            false
        } else {
            self.consecutive_idle += 1;
            self.consecutive_idle >= self.max_idle
        }
    }

    /// Model routing Part 3b (A16) — after a stall, leave the streak one
    /// idle iteration short of the threshold: exactly one more iteration
    /// runs, and if it too makes no progress the breaker trips again.
    /// (Progress still clears the streak as usual.)
    fn grant_one_more(&mut self) {
        self.consecutive_idle = self.max_idle.saturating_sub(1);
    }
}

/// Phase 176 — sum `input_tokens + output_tokens` over every
/// `TurnEnded` event in a slice of audit entries. Pure so the
/// budget accounting is unit-testable without a daemon. The
/// driver passes the run-window slice (entries since the run's
/// start-seq) so the total is "tokens spent during this run."
pub fn sum_turn_usage(entries: &[aivyx_audit::SignedEntry]) -> u64 {
    entries
        .iter()
        .filter_map(|e| match &e.event {
            aivyx_audit::AuditEvent::TurnEnded { usage, .. } => Some(
                usage.input_tokens as u64 + usage.output_tokens as u64,
            ),
            _ => None,
        })
        .sum()
}

/// Phase 176 — best-effort read of the run-window token total:
/// the sum of `TurnEnded` usage for audit entries appended since
/// `start_seq`. `None` audit log or any read error → `0` (the
/// budget simply isn't enforced, never breaks the run).
fn read_run_tokens(
    audit_log: Option<&Arc<aivyx_audit::PersistentAuditLog>>,
    start_seq: usize,
) -> u64 {
    let Some(al) = audit_log else {
        return 0;
    };
    let len = al.len();
    if len <= start_seq {
        return 0;
    }
    // entries_range(from_seq, limit): read the `len - start_seq`
    // entries appended since the run began.
    match al.entries_range(start_seq as u64, len - start_seq) {
        Ok(entries) => sum_turn_usage(&entries),
        Err(_) => 0,
    }
}

/// Chapter K — sum the **priced** spend (USD) over every `LlmCost` event in a
/// slice of audit entries, using `pricing`. Pure so the dollar accounting is
/// unit-testable without a daemon. Local models price at $0, so a local run
/// never advances the dollar cap.
pub fn sum_turn_cost(entries: &[aivyx_audit::SignedEntry], pricing: &aivyx_cost::Pricing) -> f64 {
    entries
        .iter()
        .filter_map(|e| match &e.event {
            aivyx_audit::AuditEvent::LlmCost { model, usage, .. } => {
                let counts = aivyx_cost::TokenCounts {
                    input: usage.input_tokens as u64,
                    output: usage.output_tokens as u64,
                    cache_read: usage.cache_read_input_tokens as u64,
                    cache_write: usage.cache_creation_input_tokens as u64,
                };
                Some(pricing.cost_of(model, &counts).usd)
            }
            _ => None,
        })
        .sum()
}

/// Chapter K — best-effort read of the run-window priced spend (the dollar
/// analogue of [`read_run_tokens`]). `None` audit log or any read error → `0.0`
/// (the dollar cap simply isn't enforced; never breaks the run).
fn read_run_cost(
    audit_log: Option<&Arc<aivyx_audit::PersistentAuditLog>>,
    start_seq: usize,
    pricing: &aivyx_cost::Pricing,
) -> f64 {
    let Some(al) = audit_log else {
        return 0.0;
    };
    let len = al.len();
    if len <= start_seq {
        return 0.0;
    }
    match al.entries_range(start_seq as u64, len - start_seq) {
        Ok(entries) => sum_turn_cost(&entries, pricing),
        Err(_) => 0.0,
    }
}

// `LoopRunState` moved to the wasm-clean `aivyx-ipc` crate (Chapter M.2b) so
// the browser app shares it; re-exported here so the driver + IPC are unchanged.
pub use aivyx_ipc::loop_state::LoopRunState;

/// What the driver should do at the top of an iteration. Pure
/// over the run state + the backlog's remaining count so the
/// termination logic is unit-testable without an agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopDecision {
    /// Fire one more iteration.
    Continue,
    /// Stop — the backlog drained.
    StopBacklogEmpty,
    /// Stop — the per-run `max_iterations` cap was reached.
    StopMaxIterations,
    /// Stop — an operator `loop stop` cleared `active`.
    StopRequested,
    /// Phase 174 — stop, the wall-clock `max_run_secs` cap was
    /// reached.
    StopWallClock,
    /// Phase 176 — stop, the `max_run_tokens` budget was reached.
    /// `tokens` is the run-window total at the stop.
    StopBudget { tokens: u64 },
    /// Chapter K — stop, the `max_run_usd` dollar budget was reached.
    /// `cents` is the run-window priced spend at the stop (USD×100, so the
    /// decision stays `Eq`).
    StopBudgetUsd { cents: u64 },
}

impl LoopDecision {
    /// The operator-readable reason recorded in
    /// `LoopRunState::last_stop_reason`. `Continue` has none.
    pub fn stop_reason(&self) -> Option<&'static str> {
        match self {
            LoopDecision::Continue => None,
            LoopDecision::StopBacklogEmpty => Some("backlog complete"),
            LoopDecision::StopMaxIterations => {
                Some("reached max_iterations cap")
            }
            LoopDecision::StopRequested => Some("operator stop"),
            LoopDecision::StopWallClock => {
                Some("reached max_run_secs wall-clock cap")
            }
            LoopDecision::StopBudget { .. } => {
                Some("reached max_run_tokens budget cap")
            }
            LoopDecision::StopBudgetUsd { .. } => {
                Some("reached max_run_usd dollar-budget cap")
            }
        }
    }
}

/// Pure termination decision. Checked at the top of every
/// iteration. Order matters: an operator stop wins, then the
/// wall-clock cap, then the iteration cap, then backlog drain
/// (so a stop mid-run is honoured even if the backlog still has
/// work).
///
/// Phase 174 — `elapsed_secs` + `max_run_secs` add the
/// wall-clock cap; `max_run_secs = None` disables it.
///
/// Phase 176 — `tokens_used` + `max_run_tokens` add the token
/// budget, checked after the wall-clock cap; `max_run_tokens =
/// None` disables it.
#[allow(clippy::too_many_arguments)]
pub fn decide(
    active: bool,
    iteration: u32,
    max_iterations: u32,
    remaining_stories: usize,
    elapsed_secs: u64,
    max_run_secs: Option<u64>,
    tokens_used: u64,
    max_run_tokens: Option<u64>,
    spent_usd: f64,
    max_run_usd: Option<f64>,
) -> LoopDecision {
    if !active {
        return LoopDecision::StopRequested;
    }
    if let Some(cap) = max_run_secs {
        if elapsed_secs >= cap {
            return LoopDecision::StopWallClock;
        }
    }
    if let Some(cap) = max_run_tokens {
        if tokens_used >= cap {
            return LoopDecision::StopBudget { tokens: tokens_used };
        }
    }
    if let Some(cap) = max_run_usd {
        if spent_usd >= cap {
            return LoopDecision::StopBudgetUsd {
                cents: (spent_usd * 100.0).round() as u64,
            };
        }
    }
    if iteration >= max_iterations {
        return LoopDecision::StopMaxIterations;
    }
    if remaining_stories == 0 {
        return LoopDecision::StopBacklogEmpty;
    }
    LoopDecision::Continue
}

/// Phase 174 — format the stop reason for a red gate after
/// iteration N (pre-flight uses iteration `0`). Pure so the
/// label is testable without running the driver.
pub fn gate_stop_reason(
    outcome: &crate::loop_gate::GateOutcome,
    iteration: u32,
) -> String {
    if iteration == 0 {
        format!("pre-flight {}", outcome.label())
    } else {
        format!("{} after iteration {iteration}", outcome.label())
    }
}

/// Shared run-state handle plus the start-notify. Cloned into the
/// driver task and every IPC handler.
///
/// ## Durability note (Chapter Circuit, CI.4)
///
/// This run state is **in-memory only** — a fresh
/// [`LoopRunState::default`] (`active = false`) is created each
/// daemon boot and is never persisted or reloaded (unlike team
/// missions, which `reload()` paused state). The backlog *stories*
/// are durable (the HMAC-chained `PersistentLoopBacklog`); the
/// *run* is not. So a daemon crash/restart mid-run does **not**
/// auto-resume: the stories remain pending and the operator (or an
/// autostart hook) must re-issue `aivyx-pa loop start`. This is the
/// conservative default — auto-resuming an autonomous, possibly
/// code-committing loop on every boot is a deliberate safety
/// decision, tracked as an opt-in follow-up rather than assumed.
#[derive(Clone)]
pub struct SharedLoopState {
    state: Arc<RwLock<LoopRunState>>,
    /// Wakes the idle driver when a run is requested.
    notify: Arc<Notify>,
    /// Chapter Helm (Opp F) — optional persisted run marker (a
    /// `KeyDomain::LoopState` handle). When set, an operator `loop start`
    /// persists "active" and `loop stop` persists "idle", so an opt-in
    /// `[loop] resume_on_boot` can resume a crash-interrupted run while
    /// respecting a deliberate stop. `None` (tests, no store) ⇒ no
    /// persistence, byte-identical to before.
    resume_store: Option<aivyx_storage::DomainHandle>,
}

impl Default for SharedLoopState {
    fn default() -> Self {
        Self::new()
    }
}

impl SharedLoopState {
    pub fn new() -> Self {
        SharedLoopState {
            state: Arc::new(RwLock::new(LoopRunState::default())),
            notify: Arc::new(Notify::new()),
            resume_store: None,
        }
    }

    /// Chapter Helm — attach the persisted run marker store
    /// (`storage.domain(KeyDomain::LoopState)`). Builder; the daemon calls it
    /// when `[loop] resume_on_boot` is set.
    pub fn with_resume_store(
        mut self,
        store: aivyx_storage::DomainHandle,
    ) -> Self {
        self.resume_store = Some(store);
        self
    }

    /// Chapter Helm — persist the run marker (operator intent). Awaited so a
    /// `loop stop` is durable before the daemon could exit. The operator-driven
    /// IPC handlers call this right after `request_start` (true) /
    /// `request_stop` (false). Best-effort: a write failure is logged, never
    /// fatal. No-op when no store is attached (`resume_on_boot` off).
    pub async fn persist_run_marker(&self, active: bool) {
        if let Some(store) = &self.resume_store {
            if let Err(e) = crate::loop_resume::set_run_active(store, active).await
            {
                eprintln!("aivyx-pa loop: failed to persist run marker: {e}");
            }
        }
    }

    /// Chapter Helm — read the persisted marker for the boot-resume decision.
    /// `false` when no store is attached or the marker is absent/unreadable.
    pub async fn persisted_run_active(&self) -> bool {
        match &self.resume_store {
            Some(store) => crate::loop_resume::run_was_active(store).await,
            None => false,
        }
    }

    /// Read-only snapshot for `aivyx-pa loop status`.
    pub fn snapshot(&self) -> LoopRunState {
        self.state.read().expect("loop state lock").clone()
    }

    /// Request a run start with the given cap. No-op (returns
    /// `false`) if a run is already active. Wakes the driver.
    pub fn request_start(
        &self,
        max_iterations: u32,
        now_unix_ms: u64,
    ) -> bool {
        {
            let mut s = self.state.write().expect("loop state lock");
            if s.active {
                return false;
            }
            s.active = true;
            s.iteration = 0;
            s.max_iterations = max_iterations;
            s.started_at_unix_ms = now_unix_ms;
            s.last_stop_reason = None;
            s.tokens_used = 0;
            s.spent_cents = 0;
            s.consecutive_idle = 0;
        }
        self.notify.notify_one();
        true
    }

    /// Request the active run to stop. Returns `false` if no run
    /// is active. The driver ends the run between iterations.
    pub fn request_stop(&self) -> bool {
        let mut s = self.state.write().expect("loop state lock");
        if !s.active {
            return false;
        }
        s.active = false;
        true
    }

    fn is_active(&self) -> bool {
        self.state.read().expect("loop state lock").active
    }

    fn iteration(&self) -> u32 {
        self.state.read().expect("loop state lock").iteration
    }

    fn max_iterations(&self) -> u32 {
        self.state.read().expect("loop state lock").max_iterations
    }

    fn started_at_unix_ms(&self) -> u64 {
        self.state.read().expect("loop state lock").started_at_unix_ms
    }

    fn record_iteration(&self) {
        let mut s = self.state.write().expect("loop state lock");
        s.iteration = s.iteration.saturating_add(1);
    }

    /// Phase 177 — record the run-window token total for the
    /// operator-facing `aivyx-pa loop status` surface.
    fn record_tokens(&self, tokens: u64) {
        let mut s = self.state.write().expect("loop state lock");
        s.tokens_used = tokens;
    }

    /// Chapter K — record the run-window priced spend (USD → cents) for the
    /// `aivyx-pa loop status` surface.
    fn record_cost(&self, usd: f64) {
        let mut s = self.state.write().expect("loop state lock");
        s.spent_cents = (usd * 100.0).round() as u64;
    }

    /// Chapter Circuit (CI.5) — record the live consecutive-idle count
    /// (the CI.1 stall-breaker streak) for the `aivyx-pa loop status` surface.
    fn record_idle(&self, consecutive_idle: u32) {
        let mut s = self.state.write().expect("loop state lock");
        s.consecutive_idle = consecutive_idle;
    }

    fn finish_run(&self, reason: &str) {
        let mut s = self.state.write().expect("loop state lock");
        s.active = false;
        s.last_stop_reason = Some(reason.to_string());
    }
}

/// Chapter Circuit (CI.4) — clears a run's `active` flag if the
/// driver task unwinds (panics) mid-run.
///
/// Every *clean* stop path calls [`SharedLoopState::finish_run`] (or
/// `request_stop`), which clears `active`. But if an iteration
/// panics, the driver task dies with `active` still `true` and no
/// driver behind it — and because [`SharedLoopState::request_start`]
/// no-ops while `active`, every future `aivyx-pa loop start` would
/// silently refuse ("already running"), wedging the loop until a
/// daemon restart. This guard, scoped to a single run, runs on
/// unwind and clears the flag so the next start can proceed. The
/// driver `disarm()`s it on a clean run end (the stop path already
/// recorded the precise reason); only an abnormal exit triggers the
/// fallback clear.
struct RunActiveGuard {
    shared: SharedLoopState,
    armed: bool,
}

impl RunActiveGuard {
    fn new(shared: SharedLoopState) -> Self {
        RunActiveGuard { shared, armed: true }
    }

    /// The run ended (or is ending) through a path that already
    /// owns the `active` flag — don't fire the fallback clear.
    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for RunActiveGuard {
    fn drop(&mut self) {
        if self.armed {
            self.shared.finish_run("driver aborted unexpectedly");
        }
    }
}

/// How long the idle driver waits for a start signal before
/// re-checking the shutdown token. Bounded so daemon shutdown is
/// responsive even when no run is active.
const IDLE_POLL: Duration = Duration::from_secs(30);

fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Run the autonomous-loop driver. Never returns normally — runs
/// until `shutdown` is cancelled. Idle (no CPU) until a run is
/// requested via [`SharedLoopState::request_start`]; then fires
/// `TriggerSource::Loop` iterations until [`decide`] says stop.
#[allow(clippy::too_many_arguments)]
pub async fn run_loop_driver(
    dispatch: TriggerDispatch,
    backlog: Arc<PersistentLoopBacklog>,
    shared: SharedLoopState,
    gate: Option<Arc<dyn crate::loop_gate::GateRunner>>,
    max_run_secs: Option<u64>,
    memory: Option<Arc<dyn aivyx_memory::Memory>>,
    progress_inject_count: u32,
    audit_log: Option<Arc<aivyx_audit::PersistentAuditLog>>,
    max_run_tokens: Option<u64>,
    max_run_usd: Option<f64>,
    pricing: aivyx_cost::Pricing,
    max_idle_iterations: u32,
    shutdown: CancellationToken,
    // Chapter Foreman — opt-in deterministic auto-delegation. `Some((svc,
    // threshold))` ⇒ before each solo turn the driver scores the next pending
    // story; one scoring `>= threshold` is handed to the team (headless) instead
    // of the model. `None` ⇒ off (byte-identical to pre-Foreman).
    delegate: Option<(Arc<crate::team_mission_driver::TeamMissionService>, u32)>,
    // Verdict judge — `Some` when `[loop] verify_completion` is on. Serves TWO
    // paths: (a) a delegated mission's *result* is judged against the story's
    // acceptance criteria before it's marked done (parity with solo
    // `loop.complete`); (b) Chapter Capstone — the solo verify-and-close below,
    // which grounds on the artifact when the agent did the work but forgot to
    // call `loop.complete`. `None` ⇒ neither runs (accept-on-Done / no close-out).
    completion_judge: Option<Arc<crate::completion_judge::CompletionJudge>>,
    // Model routing Part 3b (A16) — `on_failure` escalation in the loop. The
    // daemon passes `true` only when `[routing.escalation] mode = "auto"`,
    // `on_failure = true` and the dispatch carries an arming. Then a Verdict
    // FAIL arms the next iteration, and a stall gets ONE armed rescue
    // iteration per run. `false` ⇒ byte-identical to before.
    escalate_on_failure: bool,
) {
    // Chapter K — the dollar cap prices LlmCost events with the rate table the
    // daemon built (built-in defaults + any `[pricing.<model>]` overrides, K.5).
    loop {
        if shutdown.is_cancelled() {
            return;
        }
        // Idle until a run is requested (or shutdown / poll).
        if !shared.is_active() {
            tokio::select! {
                _ = shared.notify.notified() => {}
                _ = tokio::time::sleep(IDLE_POLL) => { continue; }
                _ = shutdown.cancelled() => return,
            }
        }

        // A run is active — drive iterations.
        eprintln!(
            "aivyx-pa loop: run started (max_iterations={}, gate={}, \
             max_run_secs={:?}, max_run_tokens={:?}, max_run_usd={:?}, \
             stall_breaker={})",
            shared.max_iterations(),
            if gate.is_some() { "on" } else { "off" },
            max_run_secs,
            max_run_tokens,
            max_run_usd,
            if max_idle_iterations == 0 {
                "off".to_string()
            } else {
                format!("{max_idle_iterations} idle")
            },
        );

        // Phase 176 — snapshot the audit chain length so the
        // token budget counts only turns that complete during
        // THIS run. No audit log → the budget can't be enforced
        // (degrades to no cap, like the gate when unset).
        let budget_start_seq =
            audit_log.as_ref().map(|al| al.len()).unwrap_or(0);

        // Phase 174 — pre-flight gate: refuse to start on a red
        // tree. (iteration 0 → "pre-flight" in the reason.)
        if let Some(g) = &gate {
            let outcome = g.run().await;
            if !outcome.is_green() {
                let reason = gate_stop_reason(&outcome, 0);
                shared.finish_run(&reason);
                eprintln!("aivyx-pa loop: run not started — {reason}");
                continue;
            }
        }

        // Chapter Circuit (CI.1) — the per-run stall breaker. Reset
        // each run so a fresh `loop start` always gets a full budget
        // of idle slack.
        let mut stall = StallTracker::new(max_idle_iterations);

        // Chapter Foreman follow-up — per-run count of *failed* auto-delegations
        // per story. After `MAX_DELEGATION_ATTEMPTS`, a story is skipped (it
        // clearly isn't team-decomposable) instead of re-running a full mission
        // every iteration. Reset per run, like the stall breaker.
        let mut delegation_attempts: std::collections::HashMap<String, u32> =
            std::collections::HashMap::new();

        // Chapter Circuit (CI.4) — arm the wedge guard for this run.
        // On a clean end we disarm it (the stop path owns `active`);
        // on a panic it clears `active` so the loop isn't wedged.
        let mut active_guard = RunActiveGuard::new(shared.clone());

        // Model routing Part 3b (A16) — per run: the signal to arm the next
        // fired iteration with, and whether the one stall rescue is spent.
        let mut arm_next: Option<&'static str> = None;
        let mut rescue_used = false;

        loop {
            if shutdown.is_cancelled() {
                // Leave the run flagged active so it can resume on
                // restart-via-start; just stop driving. (Disarm so the
                // guard doesn't overwrite that intent on the way out.)
                active_guard.disarm();
                return;
            }
            let remaining = backlog.remaining_count();
            let elapsed_secs = now_unix_ms()
                .saturating_sub(shared.started_at_unix_ms())
                / 1000;
            let tokens_used = read_run_tokens(
                audit_log.as_ref(),
                budget_start_seq,
            );
            // Phase 177 — surface the live run-window spend so
            // `aivyx-pa loop status` can show it approaching the cap.
            shared.record_tokens(tokens_used);
            // Chapter K — the priced run-window spend for the dollar cap.
            let spent_usd = read_run_cost(audit_log.as_ref(), budget_start_seq, &pricing);
            shared.record_cost(spent_usd);
            let decision = decide(
                shared.is_active(),
                shared.iteration(),
                shared.max_iterations(),
                remaining,
                elapsed_secs,
                max_run_secs,
                tokens_used,
                max_run_tokens,
                spent_usd,
                max_run_usd,
            );
            if let Some(reason) = decision.stop_reason() {
                shared.finish_run(reason);
                eprintln!(
                    "aivyx-pa loop: run ended — {reason} (after {} \
                     iteration(s))",
                    shared.iteration(),
                );
                break;
            }

            // Chapter Foreman — deterministic auto-delegation. Peek the next
            // pending story; if it scores complex enough, hand it to the team
            // (headless, inline) instead of firing a solo turn — this does NOT
            // depend on the model choosing `team.run`. A completed mission marks
            // the story done; any other outcome leaves it pending with a note.
            //
            // A16 — a pending stall rescue always fires solo: a delegation
            // runs no agent turn, so it can't carry the arm, and the run's
            // one rescue must never be spent without an armed turn.
            if let Some((svc, threshold)) = &delegate
                && arm_next != Some(SIGNAL_STALL_RESCUE)
            {
                if let Some(story) = backlog.next_pending() {
                    let assessment =
                        crate::task_complexity::assess(&story.title, &story.body);
                    if assessment.should_delegate(*threshold) {
                        let iter = shared.iteration() + 1;
                        shared.record_iteration();
                        eprintln!(
                            "aivyx-pa loop: iteration {iter} — delegating story {} to \
                             the team ({})",
                            story.id,
                            assessment.explain(),
                        );
                        let goal = if story.body.trim().is_empty() {
                            story.title.clone()
                        } else {
                            format!("{}\n\n{}", story.title, story.body)
                        };
                        let made_progress = match svc
                            .run_goal_blocking(
                                &goal,
                                None,
                                aivyx_core::GatePolicy::RejectAndAbort,
                            )
                            .await
                        {
                            Ok((mid, crate::team_mission::TeamMissionPhase::Done)) => {
                                // Verdict for delegated stories — judge the
                                // mission's result against the story's acceptance
                                // criteria before accepting it (the same gate solo
                                // `loop.complete` gets). A rejection is treated as
                                // a failed delegation (retry/skip), not a done.
                                let verdict = if let Some(j) = &completion_judge {
                                    let result = svc
                                        .snapshot(&mid)
                                        .map(|r| {
                                            r.outputs
                                                .values()
                                                .cloned()
                                                .collect::<Vec<_>>()
                                                .join("\n\n")
                                        })
                                        .unwrap_or_default();
                                    let v = j.verify(&story.title, &story.body, &result).await;
                                    eprintln!(
                                        "aivyx-pa loop: delegated completion verdict for '{}' — {}: {}",
                                        story.title,
                                        if v.passed { "ACCEPTED" } else { "REJECTED" },
                                        v.reason,
                                    );
                                    v.passed
                                } else {
                                    true
                                };
                                if verdict {
                                    let _ = backlog
                                        .mark_done(story.id.clone(), now_unix_ms())
                                        .await;
                                    write_progress_note(
                                        memory.as_ref(),
                                        &format!(
                                            "delegated story '{}' to team mission {mid} → done",
                                            story.title
                                        ),
                                    )
                                    .await;
                                    delegation_attempts.remove(&story.id);
                                    true
                                } else {
                                    record_failed_delegation(
                                        &backlog,
                                        memory.as_ref(),
                                        &mut delegation_attempts,
                                        &story.id,
                                        &story.title,
                                        &format!(
                                            "mission {mid} completed but its result did \
                                             not meet the story's acceptance criteria"
                                        ),
                                    )
                                    .await
                                }
                            }
                            // A non-Done mission or an error is a failed
                            // delegation: count it, and after MAX_DELEGATION_-
                            // ATTEMPTS skip the story (terminal) rather than
                            // re-running another full, costly mission each
                            // iteration until the stall breaker trips.
                            Ok((mid, phase)) => {
                                record_failed_delegation(
                                    &backlog,
                                    memory.as_ref(),
                                    &mut delegation_attempts,
                                    &story.id,
                                    &story.title,
                                    &format!("mission {mid} ended {phase:?}"),
                                )
                                .await
                            }
                            Err(e) => {
                                record_failed_delegation(
                                    &backlog,
                                    memory.as_ref(),
                                    &mut delegation_attempts,
                                    &story.id,
                                    &story.title,
                                    &format!("failed: {e}"),
                                )
                                .await
                            }
                        };
                        // Stall accounting mirrors the solo path: a completed
                        // delegation is progress; a failed/halted one is not, so
                        // repeated failures trip the breaker instead of spinning.
                        let stalled = stall.record(made_progress);
                        // A16 — the same `next_arm` rule as a solo iteration,
                        // with no verdict of its own (a delegated result's
                        // verdict decides done-or-retry above, not arming).
                        // Assigning `arm_next` drops any Verdict FAIL arm
                        // pending from an earlier solo iteration: it must not
                        // reach a later, non-adjacent solo fire. A stall here
                        // gets the run's one rescue, armed on the next
                        // iteration, which the guard above keeps solo.
                        let rescue_was_used = rescue_used;
                        let (arm, should_stop, now_used) = next_arm(
                            CloseResult::NotNeeded,
                            stalled,
                            rescue_used,
                            escalate_on_failure,
                        );
                        arm_next = arm;
                        rescue_used = now_used;
                        if stalled && !should_stop {
                            stall.grant_one_more();
                            eprintln!(
                                "aivyx-pa loop: stalled — one escalated rescue iteration \
                                 (on_failure)"
                            );
                        }
                        shared.record_idle(stall.consecutive_idle);
                        if should_stop {
                            let mut reason = format!(
                                "no progress for {max_idle_iterations} consecutive \
                                 iteration(s) (stall breaker)"
                            );
                            if rescue_was_used {
                                reason.push_str(" (after one on_failure rescue)");
                            }
                            shared.finish_run(&reason);
                            eprintln!("aivyx-pa loop: run ended — {reason}");
                            break;
                        }
                        continue;
                    }
                }
            }

            let iter = shared.iteration() + 1;
            let trigger_id = format!("loop-iter-{iter}");

            // Phase 175 — read the recent progress notes and
            // prepend them to the canonical prompt so this fresh
            // context opens with what earlier iterations learned.
            // Best-effort: a memory read error degrades to no
            // injection, never breaking the run.
            let notes = read_progress_notes(
                memory.as_ref(),
                progress_inject_count,
            )
            .await;
            let prompt = build_iteration_prompt(&notes);

            eprintln!(
                "aivyx-pa loop: iteration {iter} firing ({remaining} \
                 stor{} remaining, {} progress note(s) injected)",
                if remaining == 1 { "y" } else { "ies" },
                notes.len(),
            );
            // Chapter Circuit (CI.1) — snapshot the progress signal
            // BEFORE the iteration so we can tell afterwards whether
            // it advanced: the backlog count and the latest note.
            let note_before = recent_progress_note(memory.as_ref()).await;
            // Chapter Capstone — snapshot the story `loop.next` will hand out
            // (highest-priority pending) so that, after the turn, we can tell
            // whether the agent closed it. If it's left pending but the work is
            // actually done, the verify-and-close pass below finishes it.
            let candidate_before = backlog.next_pending();

            // Fire a fresh-context loop turn. `wrap_mission =
            // false`: the loop's own backlog is the work tracker,
            // not a per-iteration mission. No notify target.
            // A16 — `arm_next` is `Some` only with `escalate_on_failure`.
            let _ = dispatch
                .fire_armed(
                    TriggerSource::Loop,
                    &trigger_id,
                    &prompt,
                    false,
                    &[],
                    aivyx_config::NotifyWhen::Always,
                    arm_next.take(),
                )
                .await;
            shared.record_iteration();

            // Phase 174 — post-iteration gate: verify the tree is
            // still green. A red gate stops the run immediately
            // (non-destructive — the commit is preserved for the
            // operator; the loop does not pile more changes onto
            // a broken tree).
            if let Some(g) = &gate {
                let outcome = g.run().await;
                if !outcome.is_green() {
                    let reason = gate_stop_reason(&outcome, iter);
                    shared.finish_run(&reason);
                    eprintln!("aivyx-pa loop: run ended — {reason}");
                    break;
                }
            }

            // Chapter Capstone — verify-and-close. A small local model often
            // DOES the work but forgets to call `loop.complete` (or reports it
            // in chat instead of the tool), leaving a genuinely-finished story
            // pending until the stall breaker kills the run. If the story
            // `loop.next` handed out this iteration is STILL pending, judge it
            // against its own acceptance criteria — grounded on the memory /
            // workspace artifacts the turn actually produced — and mark it done
            // iff the judge passes. Genuinely-incomplete work stays pending (the
            // grounded judge rejects it, e.g. "only 1 of 2 requested items").
            // Only runs when `[loop] verify_completion` is on (judge is Some).
            let close = if let (Some(judge), Some(candidate)) =
                (&completion_judge, &candidate_before)
            {
                verify_and_close(&backlog, judge, candidate, memory.as_ref()).await
            } else {
                CloseResult::NotNeeded
            };

            // Chapter Circuit (CI.1) — stall breaker. The iteration
            // "made progress" iff a story completed/delegated (the
            // backlog shrank) or a fresh progress note was recorded.
            // N consecutive idle iterations stops the run before it
            // burns the rest of its iteration/token budget spinning
            // on something it can't get past.
            let remaining_after = backlog.remaining_count();
            let note_after = recent_progress_note(memory.as_ref()).await;
            let made_progress = remaining_after < remaining
                || (note_after.is_some() && note_after != note_before);
            let stalled = stall.record(made_progress);
            // A16 — a Verdict FAIL arms the next iteration; the first stall
            // of a run gets one armed rescue iteration instead of stopping.
            let rescue_was_used = rescue_used;
            let (arm, should_stop, now_used) =
                next_arm(close, stalled, rescue_used, escalate_on_failure);
            arm_next = arm;
            rescue_used = now_used;
            if stalled && !should_stop {
                stall.grant_one_more();
                eprintln!(
                    "aivyx-pa loop: stalled — one escalated rescue iteration (on_failure)"
                );
            }
            // CI.5 — surface the live idle streak for `aivyx-pa loop status`.
            shared.record_idle(stall.consecutive_idle);
            if should_stop {
                let mut reason = format!(
                    "no progress for {max_idle_iterations} consecutive \
                     iteration(s) (stall breaker)"
                );
                if rescue_was_used {
                    reason.push_str(" (after one on_failure rescue)");
                }
                shared.finish_run(&reason);
                eprintln!(
                    "aivyx-pa loop: run ended — {reason} (after {} iteration(s))",
                    shared.iteration(),
                );
                break;
            }
        }

        // Chapter Circuit (CI.4) — the run ended cleanly (every `break`
        // above ran `finish_run`, which cleared `active` + recorded the
        // reason). Disarm the wedge guard so it doesn't overwrite that.
        active_guard.disarm();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Chapter Capstone — verify-and-close test scaffolding ----------

    /// A judge provider that returns a fixed verdict line, so `verify_and_close`
    /// can be tested deterministically without a live model.
    struct FixedVerdict(&'static str);
    struct OneShot(Option<String>);
    #[async_trait::async_trait]
    impl aivyx_llm::LlmStream for OneShot {
        async fn next_event(
            &mut self,
        ) -> Result<Option<aivyx_llm::LlmStreamEvent>, aivyx_llm::LlmError> {
            Ok(None)
        }
        async fn finish(
            self: Box<Self>,
        ) -> Result<aivyx_llm::LlmStepEnd, aivyx_llm::LlmError> {
            Ok(aivyx_llm::LlmStepEnd::FinalMessage {
                text: self.0.unwrap_or_default(),
                usage: aivyx_llm::LlmUsage::default(),
            })
        }
    }
    #[async_trait::async_trait]
    impl aivyx_llm::LlmProvider for FixedVerdict {
        async fn chat_stream(
            &self,
            _request: aivyx_llm::LlmRequest<'_>,
            _cancel: &CancellationToken,
        ) -> Result<Box<dyn aivyx_llm::LlmStream>, aivyx_llm::LlmError> {
            Ok(Box::new(OneShot(Some(self.0.to_string()))))
        }
    }

    fn judge_returning(
        verdict: &'static str,
    ) -> crate::completion_judge::CompletionJudge {
        crate::completion_judge::CompletionJudge::new(Arc::new(FixedVerdict(verdict)), "test")
    }

    /// Guard that removes the scratch dir on drop (no `tempfile` dep here).
    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    async fn backlog_with_one_pending() -> (Arc<PersistentLoopBacklog>, String, Scratch) {
        use aivyx_crypto::MasterKey;
        use aivyx_storage::{KeyDomain, RedbStorage, Storage, StorageConfig};
        let dir = std::env::temp_dir()
            .join(format!("aivyx-capstone-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store: Arc<dyn Storage> = RedbStorage::open(
            StorageConfig::new(dir.join("s.redb")),
            MasterKey::from_raw([7u8; 32]),
        )
        .await
        .unwrap();
        let bl = PersistentLoopBacklog::open(
            store.domain(KeyDomain::LoopBacklog),
            b"k".to_vec(),
        )
        .await
        .unwrap();
        bl.add_story(
            "s1".into(),
            5,
            1,
            "Write brew-guide.md".into(),
            "Acceptance: file exists with 3 steps.".into(),
        )
        .await
        .unwrap();
        (Arc::new(bl), "s1".to_string(), Scratch(dir))
    }

    #[tokio::test]
    async fn verify_and_close_marks_done_when_judge_passes() {
        let (bl, id, _dir) = backlog_with_one_pending().await;
        let candidate = bl.next_pending().expect("one pending");
        let closed = verify_and_close(
            &bl,
            &judge_returning("PASS — brew-guide.md exists with 3 steps."),
            &candidate,
            None,
        )
        .await;
        assert_eq!(closed, CloseResult::Closed, "a PASS verdict closes the story");
        assert!(matches!(
            bl.get(&id).unwrap().status,
            aivyx_ipc::backlog::StoryStatus::Done { .. }
        ));
        assert_eq!(bl.remaining_count(), 0);
    }

    #[tokio::test]
    async fn verify_and_close_leaves_pending_when_judge_fails() {
        // The focus-tips failure mode: work done partially → judge rejects →
        // the story stays pending (NOT falsely closed).
        let (bl, id, _dir) = backlog_with_one_pending().await;
        let candidate = bl.next_pending().unwrap();
        let closed = verify_and_close(
            &bl,
            &judge_returning("FAIL — only 1 of 2 required items present."),
            &candidate,
            None,
        )
        .await;
        assert_eq!(
            closed,
            CloseResult::JudgedIncomplete,
            "a FAIL verdict must not close the story",
        );
        assert!(matches!(
            bl.get(&id).unwrap().status,
            aivyx_ipc::backlog::StoryStatus::Pending
        ));
        assert_eq!(bl.remaining_count(), 1);
    }

    #[tokio::test]
    async fn verify_and_close_is_noop_when_already_resolved() {
        // The agent DID call loop.complete → the story is already Done → the
        // close-out must not double-judge or error.
        let (bl, id, _dir) = backlog_with_one_pending().await;
        let candidate = bl.next_pending().unwrap();
        bl.mark_done(id.clone(), now_unix_ms()).await.unwrap();
        let closed = verify_and_close(
            &bl,
            &judge_returning("PASS — should not even be consulted."),
            &candidate,
            None,
        )
        .await;
        assert_eq!(
            closed,
            CloseResult::NotNeeded,
            "already-resolved story is a no-op for close-out",
        );
        assert!(matches!(
            bl.get(&id).unwrap().status,
            aivyx_ipc::backlog::StoryStatus::Done { .. }
        ));
    }

    #[test]
    fn decide_continue_when_active_under_cap_with_work() {
        assert_eq!(
            decide(true, 0, 5, 3, 0, None, 0, None, 0.0, None),
            LoopDecision::Continue
        );
        assert_eq!(
            decide(true, 4, 5, 1, 10, Some(3600), 0, None, 0.0, None),
            LoopDecision::Continue
        );
    }

    #[test]
    fn decide_stops_at_cap() {
        assert_eq!(
            decide(true, 5, 5, 3, 0, None, 0, None, 0.0, None),
            LoopDecision::StopMaxIterations
        );
        assert_eq!(
            decide(true, 6, 5, 3, 0, None, 0, None, 0.0, None),
            LoopDecision::StopMaxIterations
        );
    }

    #[test]
    fn decide_stops_on_empty_backlog() {
        assert_eq!(
            decide(true, 1, 5, 0, 0, None, 0, None, 0.0, None),
            LoopDecision::StopBacklogEmpty
        );
    }

    #[test]
    fn decide_stop_request_wins_over_remaining_work() {
        // Inactive (operator stopped) beats everything else.
        assert_eq!(
            decide(false, 1, 5, 3, 0, None, 0, None, 0.0, None),
            LoopDecision::StopRequested
        );
    }

    #[test]
    fn decide_cap_wins_over_backlog_empty() {
        // At the cap with an empty backlog, the cap reason is
        // reported (checked first) — both are valid stops.
        assert_eq!(
            decide(true, 5, 5, 0, 0, None, 0, None, 0.0, None),
            LoopDecision::StopMaxIterations
        );
    }

    #[test]
    fn decide_wall_clock_cap() {
        // None → never fires, even at huge elapsed.
        assert_eq!(
            decide(true, 1, 5, 3, 1_000_000, None, 0, None, 0.0, None),
            LoopDecision::Continue
        );
        // Under the cap → continue.
        assert_eq!(
            decide(true, 1, 5, 3, 59, Some(60), 0, None, 0.0, None),
            LoopDecision::Continue
        );
        // At/over the cap → stop.
        assert_eq!(
            decide(true, 1, 5, 3, 60, Some(60), 0, None, 0.0, None),
            LoopDecision::StopWallClock
        );
        assert_eq!(
            decide(true, 1, 5, 3, 61, Some(60), 0, None, 0.0, None),
            LoopDecision::StopWallClock
        );
    }

    #[test]
    fn decide_wall_clock_beats_iteration_cap_and_backlog() {
        // Wall-clock is checked before the iteration cap + drain.
        assert_eq!(
            decide(true, 99, 5, 0, 100, Some(60), 0, None, 0.0, None),
            LoopDecision::StopWallClock
        );
    }

    #[test]
    fn decide_stop_request_beats_wall_clock() {
        assert_eq!(
            decide(false, 1, 5, 3, 100, Some(60), 0, None, 0.0, None),
            LoopDecision::StopRequested
        );
    }

    #[test]
    fn decide_token_budget_cap() {
        // None → never fires, even at huge usage.
        assert_eq!(
            decide(true, 1, 5, 3, 0, None, 1_000_000_000, None, 0.0, None),
            LoopDecision::Continue
        );
        // Under the cap → continue.
        assert_eq!(
            decide(true, 1, 5, 3, 0, None, 999, Some(1000), 0.0, None),
            LoopDecision::Continue
        );
        // At/over the cap → stop, carrying the total.
        assert_eq!(
            decide(true, 1, 5, 3, 0, None, 1000, Some(1000), 0.0, None),
            LoopDecision::StopBudget { tokens: 1000 }
        );
        assert_eq!(
            decide(true, 1, 5, 3, 0, None, 1500, Some(1000), 0.0, None),
            LoopDecision::StopBudget { tokens: 1500 }
        );
    }

    #[test]
    fn decide_budget_after_wall_clock_before_iteration_cap() {
        // Wall-clock wins over budget.
        assert_eq!(
            decide(true, 1, 5, 3, 100, Some(60), 9999, Some(1000), 0.0, None),
            LoopDecision::StopWallClock
        );
        // Budget wins over the iteration cap + backlog drain.
        assert_eq!(
            decide(true, 99, 5, 0, 0, None, 9999, Some(1000), 0.0, None),
            LoopDecision::StopBudget { tokens: 9999 }
        );
        // Operator stop still beats budget.
        assert_eq!(
            decide(false, 1, 5, 3, 0, None, 9999, Some(1000), 0.0, None),
            LoopDecision::StopRequested
        );
    }

    #[test]
    fn gate_stop_reason_preflight_vs_iteration() {
        use crate::loop_gate::GateOutcome;
        let failed = GateOutcome::Failed { code: Some(1) };
        assert!(gate_stop_reason(&failed, 0).starts_with("pre-flight"));
        let r = gate_stop_reason(&failed, 3);
        assert!(r.contains("after iteration 3"));
        assert!(r.contains("exit 1"));
    }

    #[test]
    fn stop_reasons_are_labeled() {
        assert!(LoopDecision::Continue.stop_reason().is_none());
        assert_eq!(
            LoopDecision::StopBacklogEmpty.stop_reason(),
            Some("backlog complete")
        );
        assert_eq!(
            LoopDecision::StopMaxIterations.stop_reason(),
            Some("reached max_iterations cap")
        );
        assert_eq!(
            LoopDecision::StopRequested.stop_reason(),
            Some("operator stop")
        );
        assert_eq!(
            LoopDecision::StopWallClock.stop_reason(),
            Some("reached max_run_secs wall-clock cap")
        );
        assert_eq!(
            LoopDecision::StopBudget { tokens: 5 }.stop_reason(),
            Some("reached max_run_tokens budget cap")
        );
        assert_eq!(
            LoopDecision::StopBudgetUsd { cents: 500 }.stop_reason(),
            Some("reached max_run_usd dollar-budget cap")
        );
    }

    #[test]
    fn decide_dollar_budget_cap() {
        // None → never fires, even at high spend.
        assert_eq!(
            decide(true, 1, 5, 3, 0, None, 0, None, 999.0, None),
            LoopDecision::Continue
        );
        // Under the cap → continue.
        assert_eq!(
            decide(true, 1, 5, 3, 0, None, 0, None, 4.99, Some(5.0)),
            LoopDecision::Continue
        );
        // At/over the cap → stop, carrying the spend as cents.
        assert_eq!(
            decide(true, 1, 5, 3, 0, None, 0, None, 5.0, Some(5.0)),
            LoopDecision::StopBudgetUsd { cents: 500 }
        );
        assert_eq!(
            decide(true, 1, 5, 3, 0, None, 0, None, 12.34, Some(5.0)),
            LoopDecision::StopBudgetUsd { cents: 1234 }
        );
        // The token cap is checked before the dollar cap.
        assert_eq!(
            decide(true, 1, 5, 3, 0, None, 1000, Some(1000), 99.0, Some(5.0)),
            LoopDecision::StopBudget { tokens: 1000 }
        );
        // The dollar cap beats the iteration cap + backlog drain.
        assert_eq!(
            decide(true, 99, 5, 0, 0, None, 0, None, 9.0, Some(5.0)),
            LoopDecision::StopBudgetUsd { cents: 900 }
        );
    }

    #[test]
    fn sum_turn_cost_prices_llm_cost_events() {
        use aivyx_audit::{AuditEvent, SignedEntry};
        use aivyx_core::{TokenUsage, TurnId};

        fn entry(seq: u64, event: AuditEvent) -> SignedEntry {
            SignedEntry {
                seq,
                appended_at: std::time::SystemTime::UNIX_EPOCH,
                prev_mac: [0u8; 32],
                mac: [0u8; 32],
                event,
            }
        }
        fn cost(model: &str, input: u32, output: u32) -> AuditEvent {
            AuditEvent::LlmCost {
                turn_id: TurnId::new(),
                model: model.to_string(),
                usage: TokenUsage {
                    input_tokens: input,
                    output_tokens: output,
                    ..Default::default()
                },
            }
        }

        let pricing = aivyx_cost::Pricing::new();
        let entries = vec![
            entry(0, cost("claude-sonnet-4-6", 1_000_000, 1_000_000)), // $18
            entry(1, cost("llama3.1", 9_000_000, 9_000_000)),          // $0 local (free)
        ];
        let total = sum_turn_cost(&entries, &pricing);
        assert!((total - 18.0).abs() < 1e-9, "got {total} — local adds $0");
    }

    #[test]
    fn request_start_sets_state_and_is_idempotent() {
        let s = SharedLoopState::new();
        assert!(!s.snapshot().active);
        assert!(s.request_start(10, 1_000));
        let snap = s.snapshot();
        assert!(snap.active);
        assert_eq!(snap.max_iterations, 10);
        assert_eq!(snap.iteration, 0);
        assert_eq!(snap.started_at_unix_ms, 1_000);
        // Second start while active is a no-op.
        assert!(!s.request_start(99, 2_000));
        assert_eq!(s.snapshot().max_iterations, 10);
    }

    #[test]
    fn request_stop_only_when_active() {
        let s = SharedLoopState::new();
        assert!(!s.request_stop()); // not active
        s.request_start(5, 0);
        assert!(s.request_stop());
        assert!(!s.snapshot().active);
    }

    #[test]
    fn record_iteration_and_finish_run() {
        let s = SharedLoopState::new();
        s.request_start(5, 0);
        s.record_iteration();
        s.record_iteration();
        assert_eq!(s.snapshot().iteration, 2);
        s.finish_run("backlog complete");
        let snap = s.snapshot();
        assert!(!snap.active);
        assert_eq!(
            snap.last_stop_reason.as_deref(),
            Some("backlog complete")
        );
    }

    #[test]
    fn restart_after_finish_resets_counters() {
        let s = SharedLoopState::new();
        s.request_start(3, 0);
        s.record_iteration();
        s.finish_run("operator stop");
        s.record_tokens(5_000);
        // A fresh run zeroes iteration + tokens + clears the stop
        // reason.
        assert!(s.request_start(7, 5_000));
        let snap = s.snapshot();
        assert_eq!(snap.iteration, 0);
        assert_eq!(snap.max_iterations, 7);
        assert!(snap.last_stop_reason.is_none());
        assert_eq!(snap.tokens_used, 0);
    }

    #[test]
    fn record_tokens_surfaces_in_snapshot() {
        let s = SharedLoopState::new();
        s.request_start(5, 0);
        s.record_tokens(42_000);
        assert_eq!(s.snapshot().tokens_used, 42_000);
        // Latest write wins (the driver records the running total).
        s.record_tokens(55_000);
        assert_eq!(s.snapshot().tokens_used, 55_000);
    }

    #[test]
    fn prompt_mentions_the_core_steps() {
        // Guard the canonical prompt's load-bearing instructions.
        assert!(LOOP_SYSTEM_PROMPT.contains("loop.next"));
        assert!(LOOP_SYSTEM_PROMPT.contains("loop.complete"));
        assert!(LOOP_SYSTEM_PROMPT.contains("loop.note"));
        assert!(LOOP_SYSTEM_PROMPT.contains("gates"));
        assert!(LOOP_SYSTEM_PROMPT.contains("commit"));
        assert!(LOOP_SYSTEM_PROMPT.contains("empty"));
        assert!(LOOP_SYSTEM_PROMPT.contains("Progress so far"));
    }

    /// Chapter Circuit (CI.2) — the iteration prompt must not assume a
    /// software-dev backlog. The build/tests/commit flow it once hardcoded
    /// stranded everyday-PA stories (research, writing): the model had to
    /// improvise past "run the quality gates" and "commit with git" for work
    /// that has no project and no granted `git.write`. The verification (step 3)
    /// and persistence (step 4) steps must offer a non-code path, and the
    /// commit must be conditional on committing being available.
    #[test]
    fn iteration_prompt_is_task_agnostic() {
        let p = LOOP_SYSTEM_PROMPT;
        // Both kinds of work are named.
        assert!(p.contains("research"), "lost the non-code work path");
        assert!(
            p.contains("code change"),
            "lost the code work path framing",
        );
        // Verification offers a non-gate check, and persistence offers a
        // non-commit home (memory / a requested file).
        assert!(
            p.contains("re-read what you produced"),
            "verification step no longer covers non-code work",
        );
        assert!(
            p.contains("belong in your memory"),
            "persistence step no longer covers non-code work",
        );
        // The commit step is conditional, not mandatory.
        assert!(
            p.contains("only if committing is available"),
            "the git commit step must be conditional, not unconditional",
        );
    }

    // ---- Phase 175 — progress-log rendering -------------------

    #[test]
    fn empty_notes_render_empty_block_and_plain_prompt() {
        assert_eq!(render_progress_block(&[]), "");
        assert_eq!(build_iteration_prompt(&[]), LOOP_SYSTEM_PROMPT);
    }

    #[test]
    fn notes_render_oldest_first_with_header() {
        // get_recent returns newest-first; the block lists them
        // oldest-first so the agent reads them in learned order.
        let notes = vec![
            "newest learning".to_string(),
            "middle learning".to_string(),
            "oldest learning".to_string(),
        ];
        let block = render_progress_block(&notes);
        assert!(block.contains("## Progress so far"));
        let oldest = block.find("oldest learning").unwrap();
        let newest = block.find("newest learning").unwrap();
        assert!(oldest < newest, "oldest note should render first");
    }

    #[test]
    fn blank_notes_are_skipped() {
        let notes =
            vec!["real".to_string(), "  ".to_string(), "".to_string()];
        let block = render_progress_block(&notes);
        assert!(block.contains("- real\n"));
        // Only one bullet.
        assert_eq!(block.matches("\n- ").count(), 1);
    }

    #[test]
    fn build_iteration_prompt_prepends_block() {
        let notes = vec!["a learning".to_string()];
        let prompt = build_iteration_prompt(&notes);
        assert!(prompt.starts_with("## Progress so far"));
        assert!(prompt.contains("a learning"));
        assert!(prompt.ends_with(LOOP_SYSTEM_PROMPT));
    }

    /// Chapter Circuit (CI.0) — prompt↔floor contract drift guard.
    ///
    /// The canonical iteration prompt instructs the loop agent to call a
    /// specific set of tools. Each such tool MUST be reachable by the
    /// zero-config default role, or the instruction is dead on arrival — the
    /// exact failure mode that left `loop.*` (v0.7.4) and `team.run` (CI.0)
    /// denied for every iteration. The grants live in the daemon binary's
    /// backcompat floor (`aivyx-cli/src/bin/aivyx.rs`, gated on the loop being
    /// armed). This test can't reach that inline floor, so it pins the *prompt*
    /// side: if someone edits the instruction to add/rename a tool, this guard
    /// fails and points them at the floor grant they must keep in lockstep.
    ///
    /// Disposition of each named tool:
    /// - `loop.next` / `loop.complete` / `loop.note` — floor-granted when armed.
    /// - `team.run` — floor-granted when armed (the delegation branch).
    /// - `shell` (`shell.exec`) — already in the floor (Local channel).
    /// - `git` (`git.write`) — intentionally NOT floor-granted (Forge: operator
    ///   opts in per-repo). The unconditional commit step is a known task-fit
    ///   mismatch tracked by CI.2; it is listed here so the coupling is explicit.
    #[test]
    fn iteration_prompt_only_names_reachable_tools() {
        let p = LOOP_SYSTEM_PROMPT;
        for tool in ["loop.next", "loop.complete", "loop.note", "team.run"] {
            assert!(
                p.contains(tool),
                "iteration prompt no longer names `{tool}` — if you removed it, \
                 drop the matching floor grant in aivyx.rs (loop-armed block); \
                 if you renamed it, update the grant to match.",
            );
        }
        // The quality-gate + commit steps reference `shell` and `git`.
        assert!(p.contains("`shell`"), "lost the shell quality-gate step");
        assert!(p.contains("`git`"), "lost the git commit step");
    }

    #[test]
    fn run_active_guard_clears_active_on_abnormal_drop() {
        // Simulates a driver task that unwinds mid-run: the guard
        // drops while still armed and must clear `active` so the next
        // `loop start` isn't wedged.
        let s = SharedLoopState::new();
        assert!(s.request_start(10, 0));
        assert!(s.snapshot().active);
        {
            let _guard = RunActiveGuard::new(s.clone());
            // dropped here without disarm (the "panic" case)
        }
        let snap = s.snapshot();
        assert!(!snap.active, "an armed drop must clear active");
        assert_eq!(
            snap.last_stop_reason.as_deref(),
            Some("driver aborted unexpectedly"),
        );
        // The wedge is gone — a fresh start succeeds.
        assert!(s.request_start(10, 1));
    }

    #[test]
    fn run_active_guard_disarm_leaves_active_untouched() {
        // The clean-end path: the run owns `active` (e.g. left active
        // on shutdown for restart-via-start), so a disarmed guard must
        // not touch it.
        let s = SharedLoopState::new();
        assert!(s.request_start(10, 0));
        {
            let mut guard = RunActiveGuard::new(s.clone());
            guard.disarm();
        }
        assert!(
            s.snapshot().active,
            "a disarmed guard must leave active as the run left it",
        );
    }

    #[test]
    fn stall_tracker_disabled_never_stops() {
        let mut s = StallTracker::new(0);
        for _ in 0..100 {
            assert!(!s.record(false), "max_idle=0 must never stop the run");
        }
    }

    #[test]
    fn stall_tracker_stops_after_consecutive_idle_threshold() {
        let mut s = StallTracker::new(3);
        assert!(!s.record(false), "1 idle < 3");
        assert!(!s.record(false), "2 idle < 3");
        assert!(s.record(false), "3 consecutive idle reaches the threshold");
    }

    #[test]
    fn stall_tracker_progress_resets_the_counter() {
        let mut s = StallTracker::new(3);
        assert!(!s.record(false)); // idle 1
        assert!(!s.record(false)); // idle 2
        assert!(!s.record(true), "progress clears the streak");
        // Back to zero — it takes a fresh 3 to trip.
        assert!(!s.record(false)); // idle 1
        assert!(!s.record(false)); // idle 2
        assert!(s.record(false), "idle 3 after the reset trips");
    }

    // ---- Model routing Part 3b (A16) — on_failure in the loop ----------

    #[test]
    fn stall_tracker_grant_one_more_allows_exactly_one_idle_iteration() {
        let mut s = StallTracker::new(3);
        assert!(!s.record(false));
        assert!(!s.record(false));
        assert!(s.record(false), "stalled");
        s.grant_one_more();
        assert!(s.record(false), "the very next idle iteration stalls again");
    }

    #[test]
    fn next_arm_verdict_fail_arms_the_next_iteration() {
        assert_eq!(
            next_arm(CloseResult::JudgedIncomplete, false, false, true),
            (Some("loop_verdict_fail"), false, false)
        );
        // Closed / not needed → nothing to arm.
        assert_eq!(next_arm(CloseResult::Closed, false, false, true), (None, false, false));
        assert_eq!(next_arm(CloseResult::NotNeeded, false, false, true), (None, false, false));
        // A spent rescue doesn't stop verdict arming.
        assert_eq!(
            next_arm(CloseResult::JudgedIncomplete, false, true, true),
            (Some("loop_verdict_fail"), false, true)
        );
    }

    #[test]
    fn next_arm_first_stall_gets_one_rescue_second_stops() {
        assert_eq!(
            next_arm(CloseResult::NotNeeded, true, false, true),
            (Some("loop_stall_rescue"), false, true),
            "the first stall is rescued, not stopped"
        );
        // The rescue signal wins over a same-iteration verdict FAIL.
        assert_eq!(
            next_arm(CloseResult::JudgedIncomplete, true, false, true),
            (Some("loop_stall_rescue"), false, true)
        );
        assert_eq!(
            next_arm(CloseResult::NotNeeded, true, true, true),
            (None, true, true),
            "a second stall ends the run"
        );
    }

    #[test]
    fn next_arm_disabled_never_arms_and_stalls_stop_as_before() {
        for close in [CloseResult::Closed, CloseResult::JudgedIncomplete, CloseResult::NotNeeded] {
            assert_eq!(next_arm(close, false, false, false), (None, false, false));
            assert_eq!(next_arm(close, true, false, false), (None, true, false));
        }
    }

    // Driver-level: the real `run_loop_driver` over a recording agent and a
    // recording arming, both behind a real `TriggerDispatch`.

    mod driver_arming {
        use super::*;
        use aivyx_capability::{CapabilitySet, TrustTier};
        use aivyx_core::{
            Agent, AgentId, CancellationToken as CoreCancellationToken, ChannelContext,
            ChannelError, ChannelPlatform, EscalationArming, Message, SessionId, StreamEvent,
            TurnOutcome,
        };
        use std::sync::Mutex as StdMutex;

        type Log = Arc<StdMutex<Vec<String>>>;

        /// Does nothing — every iteration is idle (no note, no completion).
        /// With `add_on_first_turn`, its first turn adds that story to the
        /// backlog (to steer the next iteration onto a different story).
        struct IdleAgent {
            id: AgentId,
            caps: CapabilitySet,
            log: Log,
            add_on_first_turn: StdMutex<Option<(Arc<PersistentLoopBacklog>, u32, String)>>,
        }

        #[async_trait::async_trait]
        impl Agent for IdleAgent {
            fn id(&self) -> AgentId {
                self.id
            }
            fn capabilities(&self) -> &CapabilitySet {
                &self.caps
            }
            async fn turn(&self, _message: Message, _channel: &dyn ChannelContext) -> TurnOutcome {
                self.log.lock().unwrap().push("turn".to_string());
                let add = self.add_on_first_turn.lock().unwrap().take();
                if let Some((bl, priority, body)) = add {
                    bl.add_story("added".into(), 6, priority, "Added story".into(), body)
                        .await
                        .unwrap();
                }
                TurnOutcome::Completed {
                    final_message: "ok".to_string(),
                    tool_calls_made: 0,
                    duration: Duration::from_millis(0),
                }
            }
        }

        struct RecordingArming {
            log: Log,
        }

        #[async_trait::async_trait]
        impl EscalationArming for RecordingArming {
            async fn arm(
                &self,
                _session: &str,
                signal: &str,
                _can_allow_here: bool,
            ) -> (bool, Option<String>) {
                self.log.lock().unwrap().push(format!("arm:{signal}"));
                (true, None)
            }
            fn begin_armed_turn(&self, _session: &str) {}
            fn end_armed_turn(&self, _session: &str) {}
        }

        struct TestChannel(SessionId);

        #[async_trait::async_trait]
        impl ChannelContext for TestChannel {
            fn channel_name(&self) -> &str {
                "test-loop"
            }
            fn platform(&self) -> ChannelPlatform {
                ChannelPlatform::Local
            }
            fn trust_tier(&self) -> TrustTier {
                TrustTier::Trusted
            }
            fn session_id(&self) -> SessionId {
                self.0
            }
            async fn stream_event(&self, _event: StreamEvent<'_>) -> Result<(), ChannelError> {
                Ok(())
            }
            async fn finalize(&self, _outcome: &TurnOutcome) -> Result<(), ChannelError> {
                Ok(())
            }
            fn cancellation_token(&self) -> CoreCancellationToken {
                CoreCancellationToken::new()
            }
        }

        /// Run one loop to completion; returns the event log and the
        /// recorded stop reason.
        async fn drive(
            max_iterations: u32,
            max_idle: u32,
            judge: Option<Arc<crate::completion_judge::CompletionJudge>>,
            escalate_on_failure: bool,
        ) -> (Vec<String>, String) {
            drive_with(max_iterations, max_idle, judge, escalate_on_failure, None, None).await
        }

        /// A Foreman delegation service whose every mission fails to plan
        /// (the fake provider answers with non-JSON), so each delegation
        /// is a failed, no-progress iteration.
        async fn failing_delegate() -> Arc<crate::team_mission_driver::TeamMissionService> {
            use crate::team_mission_driver::{tests as tm, SharedMissionState, TeamMissionService};
            Arc::new(TeamMissionService::new(
                SharedMissionState::new(tm::team_domain().await),
                tm::deps("not a plan"),
                aivyx_team::default_nonagon(),
                aivyx_core::GatePolicy::RejectAndAbort,
            ))
        }

        /// A story body enumerated enough to score over any small threshold.
        fn complex_body() -> String {
            (1..=30).map(|i| format!("- part {i}\n")).collect()
        }

        /// [`drive`], plus Foreman delegation at `delegate_threshold` (over
        /// [`failing_delegate`]) and a story the agent's first turn adds
        /// (`(priority, body)`).
        async fn drive_with(
            max_iterations: u32,
            max_idle: u32,
            judge: Option<Arc<crate::completion_judge::CompletionJudge>>,
            escalate_on_failure: bool,
            delegate_threshold: Option<u32>,
            add_on_first_turn: Option<(u32, String)>,
        ) -> (Vec<String>, String) {
            let log: Log = Arc::new(StdMutex::new(Vec::new()));
            let (bl, _id, _dir) = backlog_with_one_pending().await;
            let delegate = match delegate_threshold {
                Some(t) => Some((failing_delegate().await, t)),
                None => None,
            };
            let factory: crate::daemon_server::ChannelFactory =
                Arc::new(|_ft: crate::daemon_ipc::FrontendType| {
                    Arc::new(TestChannel(SessionId::new()))
                        as Arc<dyn ChannelContext + Send + Sync>
                });
            let dispatch = TriggerDispatch::new(
                Arc::new(IdleAgent {
                    id: AgentId::new(),
                    caps: CapabilitySet::empty(),
                    log: Arc::clone(&log),
                    add_on_first_turn: StdMutex::new(
                        add_on_first_turn.map(|(prio, body)| (Arc::clone(&bl), prio, body)),
                    ),
                }),
                factory,
            )
            .with_escalation_arming(Arc::new(RecordingArming { log: Arc::clone(&log) }));
            let shared = SharedLoopState::new();
            assert!(shared.request_start(max_iterations, now_unix_ms()));
            // `request_start` stores a wake-up permit for a parked driver. The
            // driver below starts already active and never parks first, so
            // consume the permit here — otherwise, once the run ends, the
            // idle driver would wake on it and record a spurious second
            // (immediately stopped) run over the reason under test.
            shared.notify.notified().await;
            let shutdown = CancellationToken::new();
            let driver = tokio::spawn(run_loop_driver(
                dispatch,
                bl,
                shared.clone(),
                None,
                None,
                None,
                0,
                None,
                None,
                None,
                aivyx_cost::Pricing::default(),
                max_idle,
                shutdown.clone(),
                delegate,
                judge,
                escalate_on_failure,
            ));
            let deadline = std::time::Instant::now() + Duration::from_secs(20);
            while shared.snapshot().active {
                assert!(std::time::Instant::now() < deadline, "loop run never ended");
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            shutdown.cancel();
            driver.await.unwrap();
            let reason = shared.snapshot().last_stop_reason.unwrap_or_default();
            let events = log.lock().unwrap().clone();
            (events, reason)
        }

        #[tokio::test]
        async fn a_judged_incomplete_iteration_arms_the_next_fire() {
            let judge = Arc::new(judge_returning("FAIL — nothing was written."));
            let (events, _) = drive(2, 0, Some(judge), true).await;
            assert_eq!(events, vec!["turn", "arm:loop_verdict_fail", "turn"]);
        }

        #[tokio::test]
        async fn a_stall_gets_exactly_one_rescue_iteration_then_stops() {
            let (events, reason) = drive(10, 2, None, true).await;
            // Two idle iterations stall; ONE rescued iteration (armed) runs;
            // its idleness stalls again and ends the run.
            assert_eq!(events, vec!["turn", "turn", "arm:loop_stall_rescue", "turn"]);
            assert!(
                reason.ends_with("(after one on_failure rescue)"),
                "stop reason names the spent rescue: {reason}"
            );
            assert!(reason.contains("stall breaker"), "{reason}");
        }

        #[tokio::test]
        async fn a_delegation_stall_gets_the_rescue_as_an_armed_solo_iteration() {
            // Final review (f) — a delegation iteration can't carry an arm
            // (it fires no agent turn), so a stall there must not spend
            // the run's rescue without an armed turn. It follows the same
            // rule as a solo stall: the rescue arms the next iteration,
            // which runs solo (not delegated again) so the arm is used.
            let (events, reason) = drive_with(10, 1, None, true, Some(0), None).await;
            assert_eq!(events, vec!["arm:loop_stall_rescue", "turn"]);
            assert!(
                reason.ends_with("(after one on_failure rescue)"),
                "stop reason names the spent rescue: {reason}"
            );
        }

        #[tokio::test]
        async fn a_delegation_stall_with_escalation_off_ends_the_run_as_before() {
            let (events, reason) = drive_with(10, 1, None, false, Some(0), None).await;
            assert!(events.is_empty(), "{events:?}");
            assert_eq!(
                reason,
                "no progress for 1 consecutive iteration(s) (stall breaker)"
            );
        }

        #[tokio::test]
        async fn a_verdict_fail_arm_does_not_survive_a_delegation_iteration() {
            // Final review (f) — iteration 1 (solo, s1) is judged FAIL and
            // arms the next fire; its turn adds a higher-priority complex
            // story, so iterations 2–3 delegate it (both fail; the second
            // skips it). Iteration 4 is solo again on s1 but not adjacent
            // to the FAIL: it must not be armed.
            assert!(!crate::task_complexity::assess(
                "Write brew-guide.md",
                "Acceptance: file exists with 3 steps."
            )
            .should_delegate(20));
            assert!(crate::task_complexity::assess("Added story", &complex_body()).should_delegate(20));
            let judge = Arc::new(judge_returning("FAIL — nothing was written."));
            let (events, _) =
                drive_with(4, 10, Some(judge), true, Some(20), Some((0, complex_body()))).await;
            assert_eq!(events, vec!["turn", "turn"], "no stale verdict arm");
        }

        #[tokio::test]
        async fn disabled_never_arms_and_a_stall_ends_the_run_as_before() {
            let judge = Arc::new(judge_returning("FAIL — nothing was written."));
            let (events, reason) = drive(10, 2, Some(judge), false).await;
            assert_eq!(events, vec!["turn", "turn"]);
            assert_eq!(
                reason,
                "no progress for 2 consecutive iteration(s) (stall breaker)"
            );
        }
    }

    #[tokio::test]
    async fn read_progress_notes_disabled_or_absent_is_empty() {
        use aivyx_memory::{InMemoryMemory, Memory};
        let mem: Arc<dyn Memory> = Arc::new(InMemoryMemory::new());
        mem.put(crate::loop_tool::LOOP_PROGRESS_TOPIC, "x")
            .await
            .unwrap();
        // count = 0 → empty even with notes present.
        assert!(read_progress_notes(Some(&mem), 0).await.is_empty());
        // No memory → empty.
        assert!(read_progress_notes(None, 5).await.is_empty());
    }

    #[tokio::test]
    async fn read_progress_notes_returns_recent_newest_first_capped() {
        use aivyx_memory::{InMemoryMemory, Memory};
        let mem: Arc<dyn Memory> = Arc::new(InMemoryMemory::new());
        for i in 0..5 {
            mem.put(
                crate::loop_tool::LOOP_PROGRESS_TOPIC,
                &format!("note {i}"),
            )
            .await
            .unwrap();
        }
        // Only reads the reserved topic, capped at `count`,
        // newest-first.
        let notes = read_progress_notes(Some(&mem), 3).await;
        assert_eq!(notes.len(), 3);
        assert_eq!(notes[0], "note 4");
        assert_eq!(notes[2], "note 2");

        // build_iteration_prompt then renders them oldest-first.
        let prompt = build_iteration_prompt(&notes);
        let two = prompt.find("note 2").unwrap();
        let four = prompt.find("note 4").unwrap();
        assert!(two < four, "oldest of the window renders first");
    }

    // ---- Phase 176 — token-budget accounting ------------------

    fn turn_ended_entry(seq: u64, input: u32, output: u32) -> aivyx_audit::SignedEntry {
        aivyx_audit::SignedEntry {
            seq,
            appended_at: std::time::UNIX_EPOCH,
            event: aivyx_audit::AuditEvent::TurnEnded {
                turn_id: aivyx_core::TurnId::new(),
                outcome: aivyx_core::TurnOutcomeSummary::Completed,
                tool_calls_made: 0,
                duration: Duration::from_millis(1),
                usage: aivyx_core::TokenUsage {
                    input_tokens: input,
                    output_tokens: output,
                    ..Default::default()
                },
            },
            prev_mac: [0u8; 32],
            mac: [0u8; 32],
        }
    }

    fn non_turn_entry(seq: u64) -> aivyx_audit::SignedEntry {
        aivyx_audit::SignedEntry {
            seq,
            appended_at: std::time::UNIX_EPOCH,
            event: aivyx_audit::AuditEvent::MemoryAccess {
                turn_id: aivyx_core::TurnId::new(),
                operation: aivyx_audit::MemoryOperation::Write,
                scope: aivyx_capability::Scope::parse("memory.write")
                    .expect("known base"),
                query_or_key: "k".into(),
            },
            prev_mac: [0u8; 32],
            mac: [0u8; 32],
        }
    }

    #[test]
    fn sum_turn_usage_sums_input_plus_output_over_turn_ended_only() {
        let entries = vec![
            turn_ended_entry(0, 100, 50),
            non_turn_entry(1), // ignored
            turn_ended_entry(2, 200, 25),
        ];
        // (100+50) + (200+25) = 375.
        assert_eq!(sum_turn_usage(&entries), 375);
    }

    #[test]
    fn sum_turn_usage_empty_is_zero() {
        assert_eq!(sum_turn_usage(&[]), 0);
        assert_eq!(sum_turn_usage(&[non_turn_entry(0)]), 0);
    }

    // ---- Chapter Helm: persisted run marker ----

    #[tokio::test]
    async fn run_marker_persists_start_and_clears_on_stop() {
        use aivyx_crypto::MasterKey;
        use aivyx_storage::{KeyDomain, RedbStorage, Storage, StorageConfig};

        let base = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into());
        let dir = std::path::PathBuf::from(base)
            .join(format!("aivyx-helm-marker-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store: Arc<dyn Storage> = RedbStorage::open(
            StorageConfig::new(dir.join("store.redb")),
            MasterKey::from_raw([9u8; 32]),
        )
        .await
        .unwrap();
        let state = SharedLoopState::new()
            .with_resume_store(store.domain(KeyDomain::LoopState));

        // No marker yet → not active.
        assert!(!state.persisted_run_active().await);

        // Operator start → marker active (the IPC handler order).
        assert!(state.request_start(5, 0));
        state.persist_run_marker(true).await;
        assert!(
            state.persisted_run_active().await,
            "start persists the active marker"
        );

        // Operator stop → marker cleared (a deliberate stop wins on restart).
        assert!(state.request_stop());
        state.persist_run_marker(false).await;
        assert!(
            !state.persisted_run_active().await,
            "explicit stop clears the marker"
        );
    }

    #[tokio::test]
    async fn run_marker_is_noop_without_a_store() {
        // No store attached (resume_on_boot off) → persistence is a no-op and
        // the read is always false. Byte-identical to pre-Helm.
        let state = SharedLoopState::new();
        state.persist_run_marker(true).await;
        assert!(!state.persisted_run_active().await);
    }

    /// Chapter Foreman follow-up — a story that fails delegation repeatedly is
    /// skipped after `MAX_DELEGATION_ATTEMPTS`, not re-delegated forever.
    #[tokio::test]
    async fn foreman_skips_a_story_after_repeated_failed_delegations() {
        use crate::loop_backlog::{PersistentLoopBacklog, StoryStatus};
        use aivyx_crypto::MasterKey;
        use aivyx_storage::{KeyDomain, RedbStorage, Storage, StorageConfig};

        let dir = std::env::temp_dir()
            .join(format!("aivyx-foreman-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store: Arc<dyn Storage> = RedbStorage::open(
            StorageConfig::new(dir.join("s.redb")),
            MasterKey::from_raw([4u8; 32]),
        )
        .await
        .unwrap();
        let bl = PersistentLoopBacklog::open(
            store.domain(KeyDomain::LoopBacklog),
            b"k".to_vec(),
        )
        .await
        .unwrap();
        bl.add_story("s".into(), 1, 1, "complex".into(), "body".into())
            .await
            .unwrap();

        let mut attempts = std::collections::HashMap::new();
        // 1st failure (under the cap of 2): not progress; story stays pending.
        let p1 = record_failed_delegation(
            &bl, None, &mut attempts, "s", "complex", "mission ended Rejected",
        )
        .await;
        assert!(!p1, "an under-cap failure is not progress");
        assert!(matches!(bl.get("s").unwrap().status, StoryStatus::Pending));
        assert_eq!(bl.remaining_count(), 1);

        // 2nd failure hits the cap → skipped (a resolution → progress).
        let p2 = record_failed_delegation(
            &bl, None, &mut attempts, "s", "complex", "mission ended Rejected",
        )
        .await;
        assert!(p2, "skipping resolves the story → progress");
        assert!(matches!(bl.get("s").unwrap().status, StoryStatus::Skipped { .. }));
        assert_eq!(bl.remaining_count(), 0, "skipped story is no longer pending");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
