//! Unified trigger dispatch — Phase 27 Task 2.
//!
//! All daemon trigger types (cron schedules, webhooks, file watchers)
//! converge on the same turn-dispatch path: construct a `Message`,
//! acquire the turn lock, run `agent.turn()`, and log the outcome.
//! This module owns the shared dispatch logic so that each trigger
//! source only needs to decide *when* to fire and *what* prompt to
//! send.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Mutex;

use aivyx_audit::{
    AuditEvent, AuditWriter, AutoNotifyOutcomeSummary, HeadlessSurfaceSummary, PersistentAuditLog,
    TriggerKindSummary,
};
use aivyx_core::{Agent, GatePolicy, Message, SessionId, TurnOutcome};

use aivyx_storage::DomainHandle;

use crate::daemon_ipc::FrontendType;
use crate::daemon_server::ChannelFactory;
use crate::mission;
use crate::notify_dispatcher::{NotifyDispatcher, NotifyError};

// ---------------------------------------------------------------------------
// Trigger source tag — carried through dispatch for logging / audit.
// ---------------------------------------------------------------------------

/// Identifies the origin of a triggered turn. Carried through the
/// dispatch path for logging; will also inform audit attribution in
/// future phases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerSource {
    Cron,
    Webhook,
    FileWatch,
    /// Phase 71 — reflection-scheduler fire. Distinct from `Cron`
    /// so audit forensics + log lines can tell self-learning
    /// reflection turns apart from operator-declared cron jobs.
    Reflection,
    /// Phase 173 — autonomous-loop fire (the Aivyx Ralph loop).
    /// Distinct from `Cron` / `Reflection` so audit forensics +
    /// log lines can isolate the autonomous loop's per-iteration
    /// turns.
    Loop,
    /// Chapter Herald — a team mission reached a terminal phase
    /// (Done/Rejected/Halted). Distinct from the LLM-turn trigger
    /// sources above: a mission notification fires once per
    /// mission, not per turn, and never wraps a fresh turn itself
    /// (the mission already ran).
    Mission,
}

impl std::fmt::Display for TriggerSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TriggerSource::Cron => write!(f, "cron"),
            TriggerSource::Webhook => write!(f, "webhook"),
            TriggerSource::FileWatch => write!(f, "file-watch"),
            TriggerSource::Reflection => write!(f, "reflection"),
            TriggerSource::Loop => write!(f, "loop"),
            TriggerSource::Mission => write!(f, "mission"),
        }
    }
}

impl From<TriggerSource> for TriggerKindSummary {
    /// Phase 67 — runtime trigger kind → audit summary kind.
    /// One-way conversion used by the audit-emission path in
    /// [`TriggerDispatch::fire`].
    fn from(src: TriggerSource) -> Self {
        match src {
            TriggerSource::Cron => TriggerKindSummary::Cron,
            TriggerSource::Webhook => TriggerKindSummary::Webhook,
            TriggerSource::FileWatch => TriggerKindSummary::FileWatch,
            TriggerSource::Reflection => TriggerKindSummary::Reflection,
            TriggerSource::Loop => TriggerKindSummary::Loop,
            TriggerSource::Mission => TriggerKindSummary::Mission,
        }
    }
}

/// Phase 67 — runtime [`NotifyError`] → audit [`AutoNotifyOutcomeSummary::Failed`].
/// Same `error_kind` labels the `notify.send` tool uses (Phase
/// 62 Q4(a)), so forensic searches can grep across both
/// agent-initiated and daemon-initiated notify failures
/// uniformly.
pub fn outcome_from_notify_error(e: &NotifyError) -> AutoNotifyOutcomeSummary {
    let (error_kind, error_message) = match e {
        NotifyError::Transport(s) => ("transport", s.clone()),
        NotifyError::Auth(s) => ("auth", s.clone()),
        NotifyError::Rejected(status) => ("rejected", format!("HTTP {status}")),
        NotifyError::Timeout => ("timeout", "operation timed out".to_string()),
        NotifyError::UnknownTarget(name) => {
            ("unknown_target", format!("no notify_target named `{name}`"))
        }
    };
    AutoNotifyOutcomeSummary::Failed {
        error_kind: error_kind.to_string(),
        error_message,
    }
}

// ---------------------------------------------------------------------------
// Shared turn-dispatch context.
// ---------------------------------------------------------------------------

/// Shared state for trigger dispatch. Created once at daemon startup
/// and cloned into each trigger subsystem (scheduler, webhook listener,
/// file watcher).
#[derive(Clone)]
pub struct TriggerDispatch {
    agent: Arc<dyn Agent>,
    channel_factory: ChannelFactory,
    /// Serializes triggered turns so concurrent fires don't interleave.
    turn_lock: Arc<Mutex<()>>,
    /// Optional mission store for automatic mission wrapping.
    mission_store: Option<DomainHandle>,
    /// Optional notify dispatcher for Phase 63 auto-notify sugar.
    /// When set, a trigger with `notify_target = Some(name)` fires
    /// the named target's backend after the turn completes.
    notify_dispatcher: Option<Arc<NotifyDispatcher>>,
    /// Phase 67 — optional audit log handle. When set, every
    /// auto-notify fire (delivered, skipped-empty, or failed)
    /// emits an `AuditEvent::AutoNotifyDispatched` entry. When
    /// `None`, the auto-notify path runs as before (eprintln
    /// only) — same shape as the existing audit hook
    /// integration in tool calls.
    audit_log: Option<Arc<PersistentAuditLog>>,
    /// Phase 73 — per-target policy map (retry + rate-limit).
    /// Populated at daemon startup from the loaded
    /// `[[notify_target]]` blocks. Empty map → every dispatch
    /// uses the zero-retry / no-rate-limit defaults (today's
    /// behavior).
    target_policies: Arc<std::collections::HashMap<String, TargetPolicy>>,
    /// Chapter Herald — the operator's `[[notify_target]] default =
    /// true` name, resolved ONCE at daemon startup from the same
    /// target list `build_notify_dispatcher` used (which may include
    /// an in-memory-only synthesized `webui` target — see
    /// `aivyx.rs`'s daemon startup). A trigger whose caller passed an
    /// EMPTY `notify_targets` falls back to this live default instead
    /// of staying silent — covers Studio-created and agent-created
    /// schedules, which (unlike `[[schedule]]` TOML entries) never go
    /// through the config-load-time default-baking step.
    default_notify_target: Option<String>,
    /// Phase 73 — in-memory rate-limit registry per Q3(a).
    /// `Arc<...>` because the dispatcher is `Clone` and the
    /// registry state needs to be shared across clones (the
    /// scheduler / webhook listener / file watcher all hold
    /// their own clone of the dispatcher).
    rate_limit_registry: Arc<RateLimitRegistry>,
    /// Chapter H — the gate posture for triggered turns. Every `TriggerSource`
    /// (Cron / Webhook / FileWatch / Reflection / Loop) is **operator-absent**,
    /// so this defaults to `RejectAndAbort` (headless): an escalation can't park
    /// behind a gate no one will answer — it's recorded as a refusal and the
    /// mission ends.
    gate_policy: GatePolicy,
}

/// Phase 73 — per-target retry + rate-limit policy snapshot.
/// One entry per `[[notify_target]]` block.
#[derive(Debug, Clone, Default)]
pub struct TargetPolicy {
    pub retry_count: u32,
    pub retry_backoff_ms_start: u64,
    pub rate_limit_max: Option<u32>,
    pub rate_limit_window_secs: Option<u64>,
}

impl TargetPolicy {
    /// Build the dispatcher's `target_policies` map from the
    /// loaded `[[notify_target]]` config.
    pub fn map_from_targets(
        targets: &[aivyx_config::NotifyTargetConfig],
    ) -> std::collections::HashMap<String, TargetPolicy> {
        targets
            .iter()
            .map(|t| {
                (
                    t.name.clone(),
                    TargetPolicy {
                        retry_count: t.retry_count,
                        retry_backoff_ms_start: t.retry_backoff_ms_start,
                        rate_limit_max: t.rate_limit_max,
                        rate_limit_window_secs: t.rate_limit_window_secs,
                    },
                )
            })
            .collect()
    }
}

/// Phase 73 — per-target sliding-window rate-limit registry.
/// Each target gets its own `VecDeque<u64>` of recent dispatch
/// timestamps (epoch ms); `check_and_record` evicts timestamps
/// outside the window before deciding admit / reject.
///
/// State lives in memory for the daemon's lifetime per Q3(a).
/// Daemon restart resets the bucket — acceptable for v1 since
/// the audit chain remains the canonical record of what
/// actually dispatched.
/// The bucket for a target is already at its configured `max` for the
/// current window — `check_and_record` left the bucket unchanged.
#[derive(thiserror::Error, Debug, Clone, Copy, PartialEq, Eq)]
#[error("rate limit exceeded for this target's configured window")]
pub struct RateLimitExceeded;

#[derive(Debug, Default)]
pub struct RateLimitRegistry {
    state: tokio::sync::Mutex<
        std::collections::HashMap<String, std::collections::VecDeque<u64>>,
    >,
}

impl RateLimitRegistry {
    pub fn new() -> Self {
        Self {
            state: tokio::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Test whether a dispatch at `now_ms` fits within the budget
    /// for `target`. On Ok the timestamp is recorded; on Err the
    /// bucket is unchanged. Caller passes the policy values
    /// directly so the registry stays config-agnostic.
    pub async fn check_and_record(
        &self,
        target: &str,
        max: u32,
        window_secs: u64,
        now_ms: u64,
    ) -> Result<(), RateLimitExceeded> {
        let window_ms = window_secs.saturating_mul(1000);
        let cutoff = now_ms.saturating_sub(window_ms);
        let mut state = self.state.lock().await;
        let bucket = state.entry(target.to_string()).or_default();
        // Evict entries older than the window.
        while bucket.front().is_some_and(|&t| t < cutoff) {
            bucket.pop_front();
        }
        if (bucket.len() as u32) >= max {
            return Err(RateLimitExceeded);
        }
        bucket.push_back(now_ms);
        Ok(())
    }

    /// Test-only: read the bucket length for inspection.
    #[cfg(test)]
    pub async fn len_for(&self, target: &str) -> usize {
        self.state
            .lock()
            .await
            .get(target)
            .map(|b| b.len())
            .unwrap_or(0)
    }
}

impl TriggerDispatch {
    pub fn new(agent: Arc<dyn Agent>, channel_factory: ChannelFactory) -> Self {
        Self {
            agent,
            channel_factory,
            turn_lock: Arc::new(Mutex::new(())),
            mission_store: None,
            notify_dispatcher: None,
            audit_log: None,
            target_policies: Arc::new(std::collections::HashMap::new()),
            default_notify_target: None,
            rate_limit_registry: Arc::new(RateLimitRegistry::new()),
            // Trigger fires are operator-absent → headless by default.
            gate_policy: GatePolicy::RejectAndAbort,
        }
    }

    /// Chapter H — override the gate posture (e.g. an operator-watched webhook
    /// could be `Interactive`). Defaults to `RejectAndAbort`, since every
    /// trigger source is operator-absent.
    pub fn with_gate_policy(mut self, policy: GatePolicy) -> Self {
        self.gate_policy = policy;
        self
    }

    /// Phase 73 — attach the per-target retry + rate-limit
    /// policy map built from the loaded `[[notify_target]]`
    /// config. Replaces any previously-set map. Without this
    /// call, every target dispatches with the default
    /// (zero retries, no rate limit) — today's behavior.
    pub fn with_target_policies(
        mut self,
        policies: std::collections::HashMap<String, TargetPolicy>,
    ) -> Self {
        self.target_policies = Arc::new(policies);
        self
    }

    /// Attach a mission store so that triggers with `wrap_mission = true`
    /// can create missions automatically.
    pub fn with_mission_store(mut self, store: DomainHandle) -> Self {
        self.mission_store = Some(store);
        self
    }

    /// Phase 63 Task 3 — attach a `NotifyDispatcher` so triggers
    /// with `notify_target = Some(name)` can auto-dispatch their
    /// turn's final response after completion.
    /// Chapter Herald — set the live default-target fallback name.
    pub fn with_default_notify_target(mut self, name: Option<String>) -> Self {
        self.default_notify_target = name;
        self
    }

    pub fn with_notify_dispatcher(mut self, dispatcher: Arc<NotifyDispatcher>) -> Self {
        self.notify_dispatcher = Some(dispatcher);
        self
    }

    /// Phase 67 — attach a persistent audit log so trigger-fired
    /// auto-notify events land in the chain alongside
    /// `TurnStarted` / `TurnEnded`. Optional: when the daemon's
    /// startup didn't open an audit log (e.g. PoC harness path),
    /// auto-notify falls back to eprintln-only behavior.
    pub fn with_audit_log(mut self, audit_log: Arc<PersistentAuditLog>) -> Self {
        self.audit_log = Some(audit_log);
        self
    }

    /// Fire a single triggered turn through the agent.
    ///
    /// Acquires the turn lock, constructs a `Message` from the prompt,
    /// dispatches through `agent.turn()`, and returns the wall-clock
    /// duration of the turn (for logging / metrics).
    ///
    /// When `wrap_mission` is true and a mission store is configured,
    /// a `MissionRecord` is created before the turn (state Created →
    /// Running) and completed or failed after the turn finishes.
    ///
    /// Phase 63 Task 3: when `notify_target` is `Some` and a notify
    /// dispatcher is configured, after the turn completes the
    /// agent's final response is auto-pushed to the named target.
    /// Q2(a): empty agent response skips the dispatch.
    /// Q3(a): failed turns dispatch a synthesized body.
    /// Q4(a): subject is `<kind>: <trigger-id>`.
    pub async fn fire(
        &self,
        source: TriggerSource,
        trigger_id: &str,
        prompt: &str,
        wrap_mission: bool,
        notify_targets: &[String],
        notify_when: aivyx_config::NotifyWhen,
    ) -> Duration {
        eprintln!(
            "aivyx-pa trigger: firing {source} {trigger_id:?} (prompt={prompt:?}, mission={wrap_mission})",
        );

        // Chapter H (Task 2) — THREAT_MODEL.md is explicit that webhook
        // requests run `Untrusted` by default. Every trigger source
        // shares the same `channel_factory` (there's no per-source
        // frontend distinction below `FrontendType::Local`), so without
        // this branch a webhook fire got `LocalChannel`'s hardcoded
        // `Trusted` tier — the highest capability ceiling — for a
        // request that, even post-auth (Task 1), originates outside the
        // operator's own shell. Scoped to `Webhook` only: every other
        // trigger source keeps going through the unmodified
        // `channel_factory` path unchanged.
        let channel: Arc<dyn aivyx_core::ChannelContext + Send + Sync> =
            if source == TriggerSource::Webhook {
                Arc::new(crate::local::TierOverride::new(
                    (self.channel_factory)(FrontendType::Local),
                    aivyx_capability::TrustTier::Untrusted,
                ))
            } else {
                (self.channel_factory)(FrontendType::Local)
            };
        // Phase 67 — keep the session_id around so the audit
        // event can carry it; the same id is recorded on the
        // `TurnStarted` audit entry emitted from agent.turn().
        let session_id = SessionId::new();
        // System-originated: routine/reflection prompts are already fully
        // engineered — instruction-bearing context injection (skill
        // procedures) must not compete with them.
        let msg = Message::text(session_id, prompt.to_owned()).system_originated();

        // Create mission if requested.
        let mission_id = if wrap_mission {
            if let Some(store) = &self.mission_store {
                let mid = format!("trg-{}", uuid::Uuid::new_v4().as_simple());
                let description = format!(
                    "{source} trigger {trigger_id}: {prompt}",
                );
                let mut record = mission::MissionRecord::new(
                    mid.clone(),
                    "default".into(),
                    description,
                );
                if let Err(e) = mission::create_mission(store, &record).await {
                    eprintln!("aivyx-pa trigger: failed to create mission for {trigger_id}: {e}");
                    None
                } else {
                    // Transition to Running immediately.
                    let _ = mission::transition_to_running(&mut record);
                    if let Err(e) = mission::update_mission(store, &record).await {
                        eprintln!("aivyx-pa trigger: failed to start mission {mid}: {e}");
                    }
                    Some(mid)
                }
            } else {
                eprintln!(
                    "aivyx-pa trigger: wrap_mission requested for {trigger_id} but no mission store configured"
                );
                None
            }
        } else {
            None
        };

        let start = std::time::Instant::now();
        let _guard = self.turn_lock.lock().await;
        let outcome = self.agent.turn(msg, channel.as_ref()).await;
        drop(_guard);
        let elapsed = start.elapsed();

        eprintln!(
            "aivyx-pa trigger: {source} {trigger_id:?} turn outcome: {outcome:?} ({elapsed:.1?})",
        );

        // Complete or fail the mission based on turn outcome.
        if let (Some(mid), Some(store)) = (&mission_id, &self.mission_store) {
            let result = async {
                let mut record = mission::get_mission(store, mid)
                    .await
                    .map_err(|e| format!("get mission: {e}"))?
                    .ok_or_else(|| format!("mission {mid} not found"))?;

                match &outcome {
                    TurnOutcome::Completed { .. } => {
                        mission::complete_mission(&mut record)
                            .map_err(|e| format!("complete mission: {e}"))?;
                    }
                    TurnOutcome::Failed(_)
                    | TurnOutcome::Cancelled { .. }
                    | TurnOutcome::TimedOut { .. }
                    | TurnOutcome::MaxStepsExceeded { .. }
                    | TurnOutcome::Looping { .. } => {
                        // Cancel rather than fail — the mission itself didn't
                        // hit a gate rejection, the turn just didn't succeed.
                        // `MaxStepsExceeded` joins the other non-success
                        // terminations here per L1/R3 audit fix; Chapter
                        // Bridle's `Looping` joins them for the same reason.
                        mission::cancel_mission(&mut record)
                            .map_err(|e| format!("cancel mission: {e}"))?;
                    }
                    TurnOutcome::Escalated { reason, .. } if self.gate_policy.is_headless() => {
                        // Chapter H — a triggered (operator-absent) run can't
                        // park behind a gate no one will answer. Record the
                        // refusal and end the mission, like the non-success arm.
                        eprintln!(
                            "aivyx-pa trigger: escalation refused (headless) on mission {mid}: {reason}",
                        );
                        // H.6 — land the refusal on the audit chain (best-
                        // effort; a failed append must not derail the mission
                        // lifecycle) so the unattended trigger path is as
                        // legible as an operator-resolved gate would be.
                        if let Some(al) = &self.audit_log {
                            let event = AuditEvent::HeadlessRefusal {
                                run_id: session_id.to_string(),
                                surface: HeadlessSurfaceSummary::Trigger {
                                    trigger_kind: TriggerKindSummary::from(source),
                                },
                                reason: reason.clone(),
                            };
                            if let Err(e) = al.append(event) {
                                eprintln!(
                                    "aivyx-pa trigger: failed to audit headless refusal for {mid}: {e}",
                                );
                            }
                        }
                        mission::cancel_mission(&mut record)
                            .map_err(|e| format!("cancel mission: {e}"))?;
                    }
                    TurnOutcome::Escalated { reason, .. } => {
                        // Phase 35: create a gate on the mission so the
                        // operator can approve/reject and resume the turn.
                        // (Interactive override — a watched trigger.)
                        let gate_id = format!(
                            "gate-{}",
                            uuid::Uuid::new_v4().as_hyphenated()
                        );
                        mission::add_gate(
                            &mut record,
                            gate_id.clone(),
                            reason.clone(),
                            None,
                        )
                        .map_err(|e| format!("add gate: {e}"))?;
                        eprintln!(
                            "aivyx-pa trigger: escalation gate {gate_id} created on mission {mid}",
                        );
                        // Mission stays in GatePending (set by add_gate).
                    }
                }

                mission::update_mission(store, &record)
                    .await
                    .map_err(|e| format!("update mission: {e}"))?;
                Ok::<(), String>(())
            }
            .await;

            if let Err(e) = result {
                eprintln!("aivyx-pa trigger: mission lifecycle error for {mid}: {e}");
            }
        }

        // ---- Phase 63 Task 3: auto-notify ------------------------
        // If the trigger declared a notify_target AND a dispatcher
        // is configured, push the turn's outcome to the named
        // target. Failure surfaces as eprintln; one attempt, no
        // retry (Phase 63 sign-off).
        //
        // Phase 67 closes the deferral: every fire (delivered,
        // skipped-empty, or failed) emits an
        // `AuditEvent::AutoNotifyDispatched` entry when an
        // audit log is configured. eprintln remains for live
        // debug visibility.
        //
        // Chapter Herald — an empty caller-supplied list falls back to
        // the live default target (covers Studio/agent-created
        // schedules and any trigger source that never had a chance to
        // bake a default in at config-load time).
        let notify_targets: Vec<String> =
            resolve_notify_targets(notify_targets, self.default_notify_target.as_deref());
        if !notify_targets.is_empty() {
            if let Some(dispatcher) = self.notify_dispatcher.clone() {
                let body = render_notify_body(&outcome);
                let subject = format!("{source}: {trigger_id}");
                // Phase 72 — evaluate the conditional gate (Q3(a)).
                // A gate that returns false skips the dispatch
                // for every target and records SkippedByCondition
                // per-target so forensic searches can grep across
                // targets uniformly.
                let gate_passes =
                    condition_gate_passes(notify_when, &outcome, &body);
                if !gate_passes {
                    let condition = notify_when.condition_label().to_string();
                    eprintln!(
                        "aivyx-pa trigger: auto-notify skipped (notify_when = \
                         {condition}) for {source} {trigger_id:?} → targets \
                         {notify_targets:?}",
                    );
                    for target in &notify_targets {
                        self.emit_auto_notify_audit(
                            session_id,
                            source,
                            trigger_id,
                            target,
                            AutoNotifyOutcomeSummary::SkippedByCondition {
                                condition: condition.clone(),
                            },
                        )
                        .await;
                    }
                } else if body.is_empty() {
                    // Q2(a) at Phase 63 — empty response skips
                    // every target. Audit per target.
                    eprintln!(
                        "aivyx-pa trigger: auto-notify skipped (empty response) \
                         for {source} {trigger_id:?} → targets {notify_targets:?}",
                    );
                    for target in &notify_targets {
                        self.emit_auto_notify_audit(
                            session_id,
                            source,
                            trigger_id,
                            target,
                            AutoNotifyOutcomeSummary::SkippedEmptyResponse,
                        )
                        .await;
                    }
                } else {
                    // Phase 72 Q4(a) — concurrent fan-out via
                    // join_all. Each per-target outcome is audited
                    // independently; one target's transport
                    // failure doesn't block the others. Latency-
                    // bounded by the slowest backend.
                    //
                    // Phase 73 — each per-target task additionally:
                    //   1. Checks the rate-limit bucket (Q3(a))
                    //      before any dispatch attempt; exhausted
                    //      bucket → audit SkippedByRateLimit + skip.
                    //   2. Wraps the dispatch in a retry loop
                    //      (Q1(b) + Q2(b)) that retries on
                    //      Transport, Timeout, or Rejected with
                    //      HTTP status ≥ 500. Auth, UnknownTarget,
                    //      and Rejected 4xx never retry.
                    let policies = Arc::clone(&self.target_policies);
                    let rate_limit_registry =
                        Arc::clone(&self.rate_limit_registry);
                    let futures = notify_targets.iter().map(|target| {
                        let dispatcher = Arc::clone(&dispatcher);
                        let body = body.clone();
                        let subject = subject.clone();
                        let target = target.clone();
                        let policy = policies
                            .get(&target)
                            .cloned()
                            .unwrap_or_default();
                        let rl_registry = Arc::clone(&rate_limit_registry);
                        async move {
                            // Rate-limit gate. Exhausted bucket
                            // records SkippedByRateLimit and
                            // returns the per-target outcome
                            // distinct from any backend Err.
                            if let (Some(max), Some(window_secs)) =
                                (policy.rate_limit_max, policy.rate_limit_window_secs)
                            {
                                let now_ms = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_millis() as u64)
                                    .unwrap_or(0);
                                if rl_registry
                                    .check_and_record(&target, max, window_secs, now_ms)
                                    .await
                                    .is_err()
                                {
                                    return (
                                        target,
                                        DispatchOutcome::SkippedByRateLimit {
                                            max,
                                            window_secs,
                                        },
                                    );
                                }
                            }
                            // Retry-aware dispatch.
                            let result = dispatch_with_retry(
                                &dispatcher,
                                &target,
                                &body,
                                &subject,
                                policy.retry_count,
                                policy.retry_backoff_ms_start,
                            )
                            .await;
                            (target, DispatchOutcome::Backend(result))
                        }
                    });
                    let results = futures_util::future::join_all(futures).await;
                    for (target, outcome) in results {
                        let audit_outcome = match outcome {
                            DispatchOutcome::SkippedByRateLimit {
                                max,
                                window_secs,
                            } => {
                                eprintln!(
                                    "aivyx-pa trigger: auto-notify skipped \
                                     (rate-limit exhausted: {max}/{window_secs}s) \
                                     for {source} {trigger_id:?} → target \
                                     `{target}`",
                                );
                                AutoNotifyOutcomeSummary::SkippedByRateLimit {
                                    limit: max,
                                    window_secs,
                                }
                            }
                            DispatchOutcome::Backend(Ok(())) => {
                                eprintln!(
                                    "aivyx-pa trigger: auto-notify dispatched for \
                                     {source} {trigger_id:?} → target `{target}`",
                                );
                                AutoNotifyOutcomeSummary::Delivered
                            }
                            DispatchOutcome::Backend(Err(e)) => {
                                eprintln!(
                                    "aivyx-pa trigger: auto-notify FAILED for \
                                     {source} {trigger_id:?} → target \
                                     `{target}`: {e}",
                                );
                                outcome_from_notify_error(&e)
                            }
                        };
                        self.emit_auto_notify_audit(
                            session_id,
                            source,
                            trigger_id,
                            &target,
                            audit_outcome,
                        )
                        .await;
                    }
                }
            }
        }

        elapsed
    }

    /// Phase 67 — emit an `AuditEvent::AutoNotifyDispatched`
    /// entry for the fire. When `audit_log` is `None` this is a
    /// no-op (matching the PoC harness path). Append failures
    /// are eprintln-logged and silently swallowed per Q3(a) —
    /// the notification's already happened or didn't; failing
    /// the trigger because the audit chain couldn't record it
    /// would conflate two concerns.
    async fn emit_auto_notify_audit(
        &self,
        session_id: SessionId,
        trigger_source: TriggerSource,
        trigger_id: &str,
        target_name: &str,
        outcome: AutoNotifyOutcomeSummary,
    ) {
        emit_auto_notify_audit(
            self.audit_log.as_deref(),
            session_id,
            trigger_source,
            trigger_id,
            target_name,
            outcome,
        );
    }
}

/// Chapter Herald — free-function core of the `AutoNotifyDispatched`
/// audit emission, extracted so a caller without a full
/// `TriggerDispatch` (the team-mission driver) can still land the
/// SAME audit event schedules do — the Notifications screen's
/// history table has one source, not two.
pub fn emit_auto_notify_audit(
    audit_log: Option<&PersistentAuditLog>,
    session_id: SessionId,
    trigger_source: TriggerSource,
    trigger_id: &str,
    target_name: &str,
    outcome: AutoNotifyOutcomeSummary,
) {
    let Some(log) = audit_log else {
        return;
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let event = AuditEvent::AutoNotifyDispatched {
        session_id,
        trigger_kind: TriggerKindSummary::from(trigger_source),
        trigger_id: trigger_id.to_string(),
        target_name: target_name.to_string(),
        outcome,
        dispatched_at_unix_ms: now_ms,
    };
    // PersistentAuditLog's AuditWriter::append is sync — the
    // on-disk write is fire-and-forget via the persistent log's
    // internal drain task.
    if let Err(e) = log.append(event) {
        eprintln!(
            "aivyx-pa trigger: audit log append failed for AutoNotifyDispatched \
             ({trigger_source} {trigger_id:?} → {target_name}): {e}"
        );
    }
}

/// Phase 73 — internal per-target outcome shape distinguishing
/// "the backend ran (and succeeded or failed)" from "the
/// rate-limit gate refused the call." Folded into
/// `AutoNotifyOutcomeSummary` at audit-emission time.
enum DispatchOutcome {
    Backend(Result<(), NotifyError>),
    SkippedByRateLimit { max: u32, window_secs: u64 },
}

/// Phase 73 — retry-aware dispatch wrapper. Calls the backend
/// once, then retries on transient failures up to
/// `retry_count` additional times with exponential backoff
/// (`backoff_ms_start * 2^attempt`). Auth, UnknownTarget, and
/// Rejected with HTTP status < 500 are NOT retried per Q2(b).
///
/// Returns the final `Result` after the last attempt — every
/// retry that bounces is silently swallowed; only the final
/// outcome is exposed to the caller / audit chain. eprintln
/// logs each retry for live debug visibility.
async fn dispatch_with_retry(
    dispatcher: &NotifyDispatcher,
    target: &str,
    body: &str,
    subject: &str,
    retry_count: u32,
    backoff_ms_start: u64,
) -> Result<(), NotifyError> {
    let total_attempts = retry_count.saturating_add(1);
    for attempt in 0..total_attempts {
        let result = dispatcher.dispatch(target, body, Some(subject)).await;
        match result {
            Ok(()) => return Ok(()),
            Err(e) if attempt + 1 < total_attempts && is_transient_failure(&e) => {
                let backoff_ms = backoff_ms_start.saturating_mul(1u64 << attempt);
                eprintln!(
                    "aivyx-pa trigger: auto-notify attempt {} of {} for target \
                     `{target}` failed ({e}); retrying in {backoff_ms}ms",
                    attempt + 1,
                    total_attempts,
                );
                tokio::time::sleep(std::time::Duration::from_millis(backoff_ms))
                    .await;
            }
            Err(e) => return Err(e),
        }
    }
    // Unreachable: the loop body always returns Ok or Err on the
    // last attempt. Defensive fallback for the type checker.
    Err(NotifyError::Transport(
        "dispatch_with_retry exhausted attempts without producing a result".into(),
    ))
}

/// Phase 73 — Q2(b): does this `NotifyError` warrant a retry?
/// Retries on Transport, Timeout, and Rejected with HTTP status
/// ≥ 500. Auth, UnknownTarget, and Rejected 4xx return false.
pub fn is_transient_failure(e: &NotifyError) -> bool {
    match e {
        NotifyError::Transport(_) | NotifyError::Timeout => true,
        NotifyError::Rejected(status) => *status >= 500,
        NotifyError::Auth(_) | NotifyError::UnknownTarget(_) => false,
    }
}

/// Phase 72 — evaluate the conditional dispatch gate against a
/// turn outcome. Public for testing.
///
/// - `Always` → always passes.
/// - `OnFailed` → passes only for `Failed | TimedOut`.
/// - `OnCompletedNonEmpty` → passes only when the turn
///   completed AND the rendered body is non-whitespace.
pub fn condition_gate_passes(
    notify_when: aivyx_config::NotifyWhen,
    outcome: &TurnOutcome,
    rendered_body: &str,
) -> bool {
    use aivyx_config::NotifyWhen;
    match notify_when {
        NotifyWhen::Always => true,
        NotifyWhen::OnFailed => matches!(
            outcome,
            TurnOutcome::Failed(_) | TurnOutcome::TimedOut { .. }
        ),
        NotifyWhen::OnCompletedNonEmpty => {
            matches!(outcome, TurnOutcome::Completed { .. })
                && !rendered_body.trim().is_empty()
        }
        // Chapter Ledger — grounding gate: completed + non-empty + did real
        // work (≥1 tool call). A generative aggregation routine that produced
        // prose with no tool calls invented it; suppress the broadcast.
        NotifyWhen::OnCompletedGrounded => {
            !rendered_body.trim().is_empty()
                && matches!(
                    outcome,
                    TurnOutcome::Completed { tool_calls_made, .. }
                        if *tool_calls_made >= 1
                )
        }
    }
}

/// Render the body of an auto-notify message from a `TurnOutcome`.
/// Public for testing.
///
/// - `Completed` → the agent's `final_message` text.
/// - `Escalated` → "Turn escalated: <reason>" so the operator
///   sees the agent needed approval.
/// - `Failed` → "Turn failed: <error>" per Q3(a) at sign-off.
/// - `TimedOut` → "Turn timed out after <duration>".
/// - `Cancelled` → "Turn cancelled".
///
/// Empty string → caller should skip the dispatch (Q2(a)).
/// Chapter Herald — resolve the effective notify-target list: the
/// caller's explicit list if non-empty, else the live default (if
/// any). Pure + independently testable; both `TriggerDispatch::fire`
/// and the scheduler's deterministic-digest path share it so a
/// schedule/mission with no explicit `notify_targets` still notifies
/// something instead of staying silent — covers Studio-created and
/// agent-created schedules, which never go through the config-load-
/// time default-baking step `[[schedule]]` TOML entries get.
pub fn resolve_notify_targets(explicit: &[String], default: Option<&str>) -> Vec<String> {
    if !explicit.is_empty() {
        return explicit.to_vec();
    }
    default.map(|d| vec![d.to_string()]).unwrap_or_default()
}

pub fn render_notify_body(outcome: &TurnOutcome) -> String {
    match outcome {
        TurnOutcome::Completed { final_message, .. } => final_message.clone(),
        TurnOutcome::Escalated { reason, .. } => {
            format!("Turn escalated: {reason}")
        }
        TurnOutcome::Failed(e) => format!("Turn failed: {e}"),
        TurnOutcome::TimedOut { elapsed, .. } => {
            format!("Turn timed out after {elapsed:.1?}")
        }
        TurnOutcome::Cancelled { .. } => "Turn cancelled".to_string(),
        TurnOutcome::MaxStepsExceeded { max_steps, .. } => {
            format!("Turn aborted: planner exceeded {max_steps} steps")
        }
        TurnOutcome::Looping {
            final_message,
            repeat_limit,
            ..
        } => {
            // Surface the synthesized message; note the cause.
            if final_message.is_empty() {
                format!("Turn stopped after {repeat_limit} repeated identical tool calls")
            } else {
                final_message.clone()
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_core::{AivyxError, ToolId};
    use std::time::Duration;

    // ---- Task 2 — webhook-triggered turns run at Untrusted --------
    //
    // `fire()` always requested `FrontendType::Local` regardless of
    // `source`, and `LocalChannel::trust_tier()` is hardcoded `Trusted`
    // — so a webhook-fired turn ran at the highest capability ceiling.
    // These tests prove the fix is scoped to `TriggerSource::Webhook`
    // only: webhook now observes `Untrusted`, while another
    // operator-configured source (`Cron`) is unaffected and still
    // observes `Trusted`.

    mod tier_override_tests {
        use super::*;
        use aivyx_capability::{CapabilitySet, TrustTier};
        use aivyx_core::{
            AgentId, CancellationToken as CoreCancellationToken, ChannelContext, ChannelError,
            ChannelPlatform, StreamEvent,
        };
        use std::sync::Mutex as StdMutex;

        /// Fake `Agent` that records the trust tier of whatever channel
        /// `fire()` actually constructed and passed to `turn()` — the
        /// only reliable way to observe what tier a triggered turn ran
        /// at, since `TriggerDispatch::fire` doesn't return the channel
        /// itself.
        struct TierCapturingAgent {
            id: AgentId,
            caps: CapabilitySet,
            captured_tier: Arc<StdMutex<Option<TrustTier>>>,
        }

        #[async_trait::async_trait]
        impl Agent for TierCapturingAgent {
            fn id(&self) -> AgentId {
                self.id
            }

            fn capabilities(&self) -> &CapabilitySet {
                &self.caps
            }

            async fn turn(&self, _message: Message, channel: &dyn ChannelContext) -> TurnOutcome {
                *self.captured_tier.lock().unwrap() = Some(channel.trust_tier());
                TurnOutcome::Completed {
                    final_message: "ok".to_string(),
                    tool_calls_made: 0,
                    duration: Duration::from_millis(0),
                }
            }
        }

        /// Minimal `ChannelContext` used as the `channel_factory`'s
        /// `FrontendType::Local` return value. Always reports `Trusted`
        /// — mirrors `LocalChannel::trust_tier()`'s real hardcoded
        /// value, so a passing `cron_trigger_still_runs_at_trusted_tier`
        /// proves the untouched path rather than an artifact of the fake.
        struct AlwaysTrustedChannel;

        #[async_trait::async_trait]
        impl ChannelContext for AlwaysTrustedChannel {
            fn channel_name(&self) -> &str {
                "test-local"
            }
            fn platform(&self) -> ChannelPlatform {
                ChannelPlatform::Local
            }
            fn trust_tier(&self) -> TrustTier {
                TrustTier::Trusted
            }
            fn session_id(&self) -> SessionId {
                SessionId::new()
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

        fn tier_test_dispatch(captured_tier: Arc<StdMutex<Option<TrustTier>>>) -> TriggerDispatch {
            let channel_factory: ChannelFactory = Arc::new(|_ft: FrontendType| {
                Arc::new(AlwaysTrustedChannel) as Arc<dyn ChannelContext + Send + Sync>
            });
            TriggerDispatch::new(
                Arc::new(TierCapturingAgent {
                    id: AgentId::new(),
                    caps: CapabilitySet::empty(),
                    captured_tier,
                }),
                channel_factory,
            )
        }

        #[tokio::test]
        async fn webhook_trigger_runs_at_untrusted_tier() {
            let captured_tier = Arc::new(StdMutex::new(None));
            let dispatch = tier_test_dispatch(Arc::clone(&captured_tier));

            dispatch
                .fire(
                    TriggerSource::Webhook,
                    "wh1",
                    "do it",
                    false,
                    &[],
                    aivyx_config::NotifyWhen::Always,
                )
                .await;

            assert_eq!(
                *captured_tier.lock().unwrap(),
                Some(TrustTier::Untrusted),
                "a webhook-fired turn must run at Untrusted, per THREAT_MODEL.md"
            );
        }

        #[tokio::test]
        async fn cron_trigger_still_runs_at_trusted_tier() {
            let captured_tier = Arc::new(StdMutex::new(None));
            let dispatch = tier_test_dispatch(Arc::clone(&captured_tier));

            dispatch
                .fire(
                    TriggerSource::Cron,
                    "cron1",
                    "do it",
                    false,
                    &[],
                    aivyx_config::NotifyWhen::Always,
                )
                .await;

            assert_eq!(
                *captured_tier.lock().unwrap(),
                Some(TrustTier::Trusted),
                "non-webhook trigger sources must be unaffected by the webhook downgrade"
            );
        }
    }

    #[test]
    fn resolve_notify_targets_prefers_explicit_list() {
        let explicit = vec!["a".to_string(), "b".to_string()];
        assert_eq!(
            resolve_notify_targets(&explicit, Some("studio")),
            explicit,
            "an explicit list is never overridden by the default"
        );
    }

    #[test]
    fn resolve_notify_targets_falls_back_to_default_when_empty() {
        assert_eq!(
            resolve_notify_targets(&[], Some("studio")),
            vec!["studio".to_string()]
        );
    }

    #[test]
    fn resolve_notify_targets_empty_with_no_default_stays_silent() {
        assert!(resolve_notify_targets(&[], None).is_empty());
    }

    #[test]
    fn render_completed_returns_final_message_verbatim() {
        let outcome = TurnOutcome::Completed {
            final_message: "Daily summary: 3 commits, 2 PRs reviewed.".into(),
            tool_calls_made: 0,
            duration: Duration::from_secs(2),
        };
        assert_eq!(
            render_notify_body(&outcome),
            "Daily summary: 3 commits, 2 PRs reviewed."
        );
    }

    #[test]
    fn render_completed_empty_message_returns_empty_string() {
        // Caller (TriggerDispatch::fire) checks for empty body
        // and skips the dispatch per Q2(a).
        let outcome = TurnOutcome::Completed {
            final_message: String::new(),
            tool_calls_made: 0,
            duration: Duration::from_secs(1),
        };
        assert!(render_notify_body(&outcome).is_empty());
    }

    #[test]
    fn grounding_gate_suppresses_a_no_tool_aggregation_turn() {
        use aivyx_config::NotifyWhen::OnCompletedGrounded as Grounded;
        let nonempty = |tools: usize| TurnOutcome::Completed {
            final_message: "Trend: retirees diversifying income.".into(),
            tool_calls_made: tools,
            duration: Duration::from_secs(2),
        };
        let body = "Trend: retirees diversifying income.";
        // 0 tool calls → fabricated (no real search) → gate fails (suppress).
        assert!(!condition_gate_passes(Grounded, &nonempty(0), body));
        // ≥1 tool call → grounded → gate passes (deliver).
        assert!(condition_gate_passes(Grounded, &nonempty(1), body));
        assert!(condition_gate_passes(Grounded, &nonempty(3), body));
        // Empty body never passes, even with tools.
        assert!(!condition_gate_passes(Grounded, &nonempty(2), "  "));
        // A failed turn never passes the grounded gate.
        assert!(!condition_gate_passes(
            Grounded,
            &TurnOutcome::Failed(AivyxError::Channel("x".into())),
            "irrelevant",
        ));
    }

    #[test]
    fn render_failed_returns_turn_failed_prefix() {
        let outcome = TurnOutcome::Failed(AivyxError::Channel(
            "provider unreachable".into(),
        ));
        let body = render_notify_body(&outcome);
        assert!(body.starts_with("Turn failed:"), "body: {body}");
        assert!(body.contains("provider unreachable"), "body: {body}");
    }

    #[test]
    fn render_escalated_returns_escalation_summary() {
        let outcome = TurnOutcome::Escalated {
            reason: "destructive shell command refused".into(),
            pending_tool: ToolId::new(),
            scope: None,
            tool_calls_made: 1,
        };
        let body = render_notify_body(&outcome);
        assert!(body.starts_with("Turn escalated:"), "body: {body}");
        assert!(body.contains("destructive"), "body: {body}");
    }

    #[test]
    fn render_timed_out_includes_elapsed() {
        let outcome = TurnOutcome::TimedOut {
            tool_calls_made: 5,
            elapsed: Duration::from_secs(120),
        };
        let body = render_notify_body(&outcome);
        assert!(body.starts_with("Turn timed out"), "body: {body}");
    }

    #[test]
    fn render_cancelled_returns_cancelled_marker() {
        let outcome = TurnOutcome::Cancelled { tool_calls_made: 2 };
        assert_eq!(render_notify_body(&outcome), "Turn cancelled");
    }

    #[test]
    fn trigger_source_display() {
        assert_eq!(TriggerSource::Cron.to_string(), "cron");
        assert_eq!(TriggerSource::Webhook.to_string(), "webhook");
        assert_eq!(TriggerSource::FileWatch.to_string(), "file-watch");
    }

    #[test]
    fn trigger_source_eq() {
        assert_eq!(TriggerSource::Cron, TriggerSource::Cron);
        assert_ne!(TriggerSource::Cron, TriggerSource::Webhook);
        assert_ne!(TriggerSource::Webhook, TriggerSource::FileWatch);
    }

    // ---- Phase 67 — TriggerSource → TriggerKindSummary conversion ----

    #[test]
    fn trigger_source_converts_to_audit_kind() {
        assert_eq!(
            TriggerKindSummary::from(TriggerSource::Cron),
            TriggerKindSummary::Cron,
        );
        assert_eq!(
            TriggerKindSummary::from(TriggerSource::Webhook),
            TriggerKindSummary::Webhook,
        );
        assert_eq!(
            TriggerKindSummary::from(TriggerSource::FileWatch),
            TriggerKindSummary::FileWatch,
        );
    }

    // ---- Phase 67 — NotifyError → AutoNotifyOutcomeSummary mapping ----

    #[test]
    fn notify_error_transport_maps_to_failed_transport() {
        let err = NotifyError::Transport("dns failure".into());
        match outcome_from_notify_error(&err) {
            AutoNotifyOutcomeSummary::Failed {
                error_kind,
                error_message,
            } => {
                assert_eq!(error_kind, "transport");
                assert_eq!(error_message, "dns failure");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn notify_error_auth_maps_to_failed_auth() {
        let err = NotifyError::Auth("HTTP 401".into());
        match outcome_from_notify_error(&err) {
            AutoNotifyOutcomeSummary::Failed {
                error_kind,
                error_message,
            } => {
                assert_eq!(error_kind, "auth");
                assert_eq!(error_message, "HTTP 401");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn notify_error_rejected_maps_to_failed_with_status_in_message() {
        let err = NotifyError::Rejected(429);
        match outcome_from_notify_error(&err) {
            AutoNotifyOutcomeSummary::Failed {
                error_kind,
                error_message,
            } => {
                assert_eq!(error_kind, "rejected");
                assert!(error_message.contains("429"), "msg: {error_message}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn notify_error_timeout_maps_to_failed_timeout() {
        let err = NotifyError::Timeout;
        match outcome_from_notify_error(&err) {
            AutoNotifyOutcomeSummary::Failed {
                error_kind,
                error_message,
            } => {
                assert_eq!(error_kind, "timeout");
                assert!(error_message.contains("timed out"), "msg: {error_message}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn notify_error_unknown_target_maps_to_failed_unknown_target() {
        let err = NotifyError::UnknownTarget("phone".into());
        match outcome_from_notify_error(&err) {
            AutoNotifyOutcomeSummary::Failed {
                error_kind,
                error_message,
            } => {
                assert_eq!(error_kind, "unknown_target");
                assert!(error_message.contains("phone"), "msg: {error_message}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    // ---- Phase 72 — conditional dispatch gate ----------------

    use aivyx_config::NotifyWhen;

    fn completed_with_body(body: &str) -> TurnOutcome {
        TurnOutcome::Completed {
            final_message: body.to_string(),
            tool_calls_made: 0,
            duration: Duration::from_secs(1),
        }
    }

    fn failed_outcome() -> TurnOutcome {
        TurnOutcome::Failed(AivyxError::Tool {
            tool: ToolId::new(),
            detail: "boom".into(),
        })
    }

    #[test]
    fn condition_always_always_passes() {
        let body = "anything";
        assert!(condition_gate_passes(
            NotifyWhen::Always,
            &completed_with_body(body),
            body,
        ));
        assert!(condition_gate_passes(
            NotifyWhen::Always,
            &failed_outcome(),
            "Turn failed: boom",
        ));
    }

    #[test]
    fn condition_on_failed_only_passes_for_failed_and_timed_out() {
        assert!(condition_gate_passes(
            NotifyWhen::OnFailed,
            &failed_outcome(),
            "Turn failed: boom",
        ));
        assert!(condition_gate_passes(
            NotifyWhen::OnFailed,
            &TurnOutcome::TimedOut {
                elapsed: Duration::from_secs(30),
                tool_calls_made: 0,
            },
            "Turn timed out after 30s",
        ));
        assert!(!condition_gate_passes(
            NotifyWhen::OnFailed,
            &completed_with_body("daily summary"),
            "daily summary",
        ));
    }

    #[test]
    fn condition_on_completed_non_empty_passes_only_for_completed_with_body() {
        assert!(condition_gate_passes(
            NotifyWhen::OnCompletedNonEmpty,
            &completed_with_body("daily summary"),
            "daily summary",
        ));
        // Empty body → false.
        assert!(!condition_gate_passes(
            NotifyWhen::OnCompletedNonEmpty,
            &completed_with_body(""),
            "",
        ));
        // Whitespace-only body → false.
        assert!(!condition_gate_passes(
            NotifyWhen::OnCompletedNonEmpty,
            &completed_with_body("   \n"),
            "   \n",
        ));
        // Failed turn → false even if body is non-empty.
        assert!(!condition_gate_passes(
            NotifyWhen::OnCompletedNonEmpty,
            &failed_outcome(),
            "Turn failed: boom",
        ));
    }

    #[test]
    fn condition_label_returns_stable_strings() {
        assert_eq!(NotifyWhen::Always.condition_label(), "always");
        assert_eq!(NotifyWhen::OnFailed.condition_label(), "on_failed");
        assert_eq!(
            NotifyWhen::OnCompletedNonEmpty.condition_label(),
            "on_completed_non_empty"
        );
    }

    // ---- Phase 73 — retry classifier ------------------------

    #[test]
    fn transient_failure_includes_transport_timeout_5xx() {
        assert!(is_transient_failure(&NotifyError::Transport(
            "dns".into()
        )));
        assert!(is_transient_failure(&NotifyError::Timeout));
        assert!(is_transient_failure(&NotifyError::Rejected(500)));
        assert!(is_transient_failure(&NotifyError::Rejected(502)));
        assert!(is_transient_failure(&NotifyError::Rejected(599)));
    }

    #[test]
    fn transient_failure_excludes_auth_4xx_unknown() {
        assert!(!is_transient_failure(&NotifyError::Auth("401".into())));
        assert!(!is_transient_failure(&NotifyError::Rejected(400)));
        assert!(!is_transient_failure(&NotifyError::Rejected(401)));
        assert!(!is_transient_failure(&NotifyError::Rejected(403)));
        assert!(!is_transient_failure(&NotifyError::Rejected(404)));
        assert!(!is_transient_failure(&NotifyError::Rejected(499)));
        assert!(!is_transient_failure(&NotifyError::UnknownTarget(
            "phone".into()
        )));
    }

    // ---- Phase 73 — rate-limit registry ---------------------

    #[tokio::test]
    async fn rate_bucket_admits_up_to_limit_then_rejects() {
        let reg = RateLimitRegistry::new();
        let now = 1_000_000u64;
        // 3 within 60s window — all admit.
        for _ in 0..3 {
            reg.check_and_record("phone", 3, 60, now).await.unwrap();
        }
        assert_eq!(reg.len_for("phone").await, 3);
        // 4th within same window — rejected, bucket unchanged.
        let err = reg.check_and_record("phone", 3, 60, now).await;
        assert!(err.is_err());
        assert_eq!(reg.len_for("phone").await, 3);
    }

    #[tokio::test]
    async fn rate_bucket_evicts_expired_timestamps() {
        let reg = RateLimitRegistry::new();
        let t0 = 1_000_000u64;
        // Saturate the bucket at t0.
        for _ in 0..3 {
            reg.check_and_record("phone", 3, 60, t0).await.unwrap();
        }
        // Try again 61 seconds later — window has slid past all
        // three entries; bucket evicts them and admits the new
        // dispatch.
        let later = t0 + 61_000;
        reg.check_and_record("phone", 3, 60, later).await.unwrap();
        assert_eq!(reg.len_for("phone").await, 1);
    }

    #[tokio::test]
    async fn rate_bucket_isolates_per_target() {
        let reg = RateLimitRegistry::new();
        let now = 1_000_000u64;
        // Saturate `phone` but not `desktop`.
        for _ in 0..2 {
            reg.check_and_record("phone", 2, 60, now).await.unwrap();
        }
        // phone is full; desktop is fresh.
        assert!(reg.check_and_record("phone", 2, 60, now).await.is_err());
        reg.check_and_record("desktop", 2, 60, now).await.unwrap();
        assert_eq!(reg.len_for("phone").await, 2);
        assert_eq!(reg.len_for("desktop").await, 1);
    }

    #[tokio::test]
    async fn target_policy_map_from_targets_round_trips() {
        let targets = vec![aivyx_config::NotifyTargetConfig {
            name: "phone".into(),
            kind: aivyx_config::NotifyTargetKind::Webhook {
                url: "https://example.com/x".into(),
            },
            enabled: true,
            is_default: false,
            retry_count: 3,
            retry_backoff_ms_start: 200,
            rate_limit_max: Some(5),
            rate_limit_window_secs: Some(60),
        }];
        let map = TargetPolicy::map_from_targets(&targets);
        let p = map.get("phone").expect("present");
        assert_eq!(p.retry_count, 3);
        assert_eq!(p.retry_backoff_ms_start, 200);
        assert_eq!(p.rate_limit_max, Some(5));
        assert_eq!(p.rate_limit_window_secs, Some(60));
    }
}
