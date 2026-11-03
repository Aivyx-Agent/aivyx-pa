//! Daemon scheduler loop — Phase 26 Task 3.
//!
//! A background task that evaluates schedule entries against the current
//! time and fires agent turns when a schedule is due. Spawned inside
//! `run_daemon` alongside the accept loop, sharing the same `Agent`,
//! `ChannelFactory`, and `CancellationToken`.
//!
//! The tick cadence is adaptive: the loop sleeps until the earliest
//! next-fire-time across all enabled schedules (capped at 60 s so new
//! schedules created mid-run are picked up promptly).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;

use aivyx_core::CancellationToken;
use aivyx_storage::DomainHandle;

use crate::schedule::{self, ScheduleRecord};
use crate::trigger::{TriggerDispatch, TriggerSource};

const MAX_TICK_INTERVAL: Duration = Duration::from_secs(60);

/// How long after the daemon starts a routine waits for the model to answer
/// before it runs anyway (and a failure is reported as usual).
pub const MODEL_WAIT_WINDOW: Duration = Duration::from_secs(10 * 60);
/// How often a held routine asks again whether the model answers.
const MODEL_WAIT_RETRY: Duration = Duration::from_secs(15);

/// Asks the model for a one-token reply; `true` once it answers. Used only
/// while routines are held at start-up (see [`StartupHold`]).
pub type ModelProbe = Arc<dyn Fn() -> Pin<Box<dyn Future<Output = bool> + Send>> + Send + Sync>;

/// UX audit S3 — at daemon start a local model is often still loading, so a
/// routine that is due straight away (a missed run caught up, or a cron that
/// matches the start minute) used to fail and greet the user as "Needs you".
/// Routines that need the model are held until it has answered once. The
/// hold ends for good at the first answer, or [`MODEL_WAIT_WINDOW`] after
/// start, whichever comes first: a model that is still down after that is a
/// real problem and the run fails and shows as before. The probe runs only
/// when a routine is actually due inside the window, so a daemon with
/// nothing due at start never makes the extra call.
pub struct StartupHold {
    probe: Option<ModelProbe>,
    started: Instant,
    window: Duration,
    ready: bool,
    /// Whether the "holding" line has been logged (once per start).
    announced: bool,
}

impl StartupHold {
    pub fn new(probe: Option<ModelProbe>, window: Duration) -> Self {
        Self { probe, started: Instant::now(), window, ready: false, announced: false }
    }

    /// Whether routines that need the model may run now. Asks the model
    /// (once per call) only while the hold is still on.
    pub async fn model_ready(&mut self, now: Instant) -> bool {
        if self.ready {
            return true;
        }
        let Some(probe) = self.probe.as_ref() else {
            self.ready = true;
            return true;
        };
        if now.saturating_duration_since(self.started) >= self.window {
            eprintln!("aivyx-pa scheduler: the model hasn't answered since start; running routines anyway");
            self.ready = true;
            return true;
        }
        if probe().await {
            eprintln!("aivyx-pa scheduler: the model answered; routines run from now on");
            self.ready = true;
        } else if !self.announced {
            eprintln!("aivyx-pa scheduler: the model isn't answering yet; holding due routines until it does");
            self.announced = true;
        }
        self.ready
    }
}

/// A [`ModelProbe`] over the agent's own provider: a tiny request that
/// counts as an answer once the provider returns a reply. Any error (a
/// refused connection, "Loading model", a 503) or a 60 s stall is "not yet".
pub fn llm_probe(provider: Arc<dyn aivyx_llm::LlmProvider>, model: String) -> ModelProbe {
    Arc::new(move || {
        let provider = Arc::clone(&provider);
        let model = model.clone();
        Box::pin(async move {
            let ask = async {
                let messages = vec![aivyx_llm::LlmMessage::user_text("Reply with OK.")];
                let request = aivyx_llm::LlmRequest {
                    model: &model,
                    system: None,
                    messages: &messages,
                    tools: &[],
                    max_tokens: 8,
                    temperature: None,
                    id_slot: None,
                    slot_hint: None,
                    route: None,
                };
                let cancel = CancellationToken::new();
                let Ok(mut stream) = provider.chat_stream(request, &cancel).await else {
                    return false;
                };
                while let Ok(Some(_)) = stream.next_event().await {}
                stream.finish().await.is_ok()
            };
            tokio::time::timeout(Duration::from_secs(60), ask).await.unwrap_or(false)
        })
    })
}

/// Whether a routine's run goes through the model. A deterministic digest
/// (with its builder wired) doesn't.
fn needs_model(sched: &ScheduleRecord, has_report_ctx: bool) -> bool {
    !(sched.team_mission.is_none() && sched.report_kind.as_deref() == Some("digest") && has_report_ctx)
}

/// Merge TOML `[[schedule]]` entries into the schedule store. Entries
/// whose `name` already exists in storage are skipped (storage is
/// authoritative after first sync). Called once at daemon startup.
pub async fn sync_config_schedules(
    store: &DomainHandle,
    config_schedules: &[crate::schedule::ScheduleRecord],
) -> Result<usize, String> {
    let mut synced = 0;
    for record in config_schedules {
        let existing = schedule::get_schedule(store, &record.schedule_id)
            .await
            .map_err(|e| format!("sync_config_schedules get: {e}"))?;
        if existing.is_none() {
            schedule::create_schedule(store, record)
                .await
                .map_err(|e| format!("sync_config_schedules create: {e}"))?;
            synced += 1;
        }
    }
    Ok(synced)
}

/// Convert config-layer `ScheduleConfig` entries into storage-layer
/// `ScheduleRecord` values suitable for `sync_config_schedules`.
pub fn config_to_records(
    configs: &[aivyx_config::ScheduleConfig],
) -> Result<Vec<ScheduleRecord>, String> {
    configs
        .iter()
        .map(|c| {
            let mut built = match &c.team_mission {
                Some(tm) => ScheduleRecord::new_team_mission(
                    format!("cfg-{}", c.name),
                    c.cron.clone(),
                    tm.goal.clone(),
                    tm.pack_config.clone(),
                ),
                None => ScheduleRecord::new(
                    format!("cfg-{}", c.name),
                    c.cron.clone(),
                    c.role.clone(),
                    c.prompt.clone(),
                ),
            }?;
            built.enabled = c.enabled;
            built.wrap_mission = c.wrap_mission;
            built.notify_target = c.notify_target.clone();
            built.notify_targets = c.notify_targets.clone();
            built.notify_when = c.notify_when;
            built.report_kind = c.report_kind.clone();
            Ok(built)
        })
        .collect()
}

/// Chapter Ledger — context the scheduler uses to run a `report_kind = "digest"`
/// routine **deterministically** (no LLM). `None` ⇒ digest routines fall back
/// to firing their prompt as a normal LLM turn (so the daemon still works if
/// the builder wasn't wired).
#[derive(Clone)]
pub struct ReportContext {
    pub digest: std::sync::Arc<crate::digest::WeeklyDigestBuilder>,
    pub notify:
        Option<std::sync::Arc<crate::notify_dispatcher::NotifyDispatcher>>,
    /// Chapter Herald — see `TriggerDispatch::default_notify_target`;
    /// same live fallback, for the deterministic digest path which
    /// bypasses `TriggerDispatch::fire` entirely.
    pub default_notify_target: Option<String>,
}

/// Run the scheduler loop. This future never returns normally — it
/// runs until `shutdown` is cancelled.
///
/// On each tick the scheduler:
/// 1. Loads all enabled schedules from `store`.
/// 2. For each schedule whose next fire time is ≤ now and that hasn't
///    already fired in this cron window, fires a turn through the agent.
/// 3. Updates `last_fired_at` on fired schedules.
/// 4. Sleeps until the earliest next-fire-time (capped at 60 s).
pub async fn run_scheduler(
    dispatch: TriggerDispatch,
    store: DomainHandle,
    shutdown: CancellationToken,
    report_ctx: Option<ReportContext>,
    team_missions: Option<crate::team_mission_driver::TeamMissionService>,
    model_probe: Option<ModelProbe>,
) {
    let mut hold = StartupHold::new(model_probe, MODEL_WAIT_WINDOW);
    loop {
        if shutdown.is_cancelled() {
            return;
        }

        let sleep_dur = match tick(
            &dispatch,
            &store,
            report_ctx.as_ref(),
            team_missions.as_ref(),
            &mut hold,
        )
        .await
        {
            Ok(dur) => dur,
            Err(e) => {
                eprintln!("aivyx-pa scheduler: tick error: {e}");
                MAX_TICK_INTERVAL
            }
        };

        tokio::select! {
            _ = tokio::time::sleep(sleep_dur) => {}
            _ = shutdown.cancelled() => return,
        }
    }
}

/// Execute one scheduler tick. Returns the duration to sleep before the
/// next tick.
async fn tick(
    dispatch: &TriggerDispatch,
    store: &DomainHandle,
    report_ctx: Option<&ReportContext>,
    team_missions: Option<&crate::team_mission_driver::TeamMissionService>,
    hold: &mut StartupHold,
) -> Result<Duration, String> {
    let schedules = schedule::list_schedules(store)
        .await
        .map_err(|e| format!("list schedules: {e}"))?;

    let now = Utc::now();
    let mut earliest_next: Option<Duration> = None;
    // Asked at most once per tick, and only when a routine is due.
    let mut model_ready: Option<bool> = None;
    let mut held = false;

    for sched in &schedules {
        if !sched.enabled {
            continue;
        }

        let Some(next_fire) = sched.next_fire_time_after(
            last_fired_or_epoch(sched),
        ) else {
            continue;
        };

        if next_fire <= now {
            if !already_fired_in_window(sched, next_fire) {
                if needs_model(sched, report_ctx.is_some()) {
                    let ready = match model_ready {
                        Some(r) => r,
                        None => {
                            let r = hold.model_ready(Instant::now()).await;
                            model_ready = Some(r);
                            r
                        }
                    };
                    if !ready {
                        // Not stamped as fired: the next tick tries again.
                        held = true;
                        continue;
                    }
                }
                fire_schedule(dispatch, store, sched, report_ctx, team_missions).await;
            }
            // Recompute next fire after this one.
            if let Some(after_now) = sched.next_fire_time_after(now) {
                update_earliest(&mut earliest_next, after_now, now);
            }
        } else {
            update_earliest(&mut earliest_next, next_fire, now);
        }
    }

    let sleep = earliest_next.unwrap_or(MAX_TICK_INTERVAL).min(MAX_TICK_INTERVAL);
    Ok(if held { sleep.min(MODEL_WAIT_RETRY) } else { sleep })
}

/// Compute the anchor for next-fire-time: if the schedule has fired
/// before, use that timestamp; otherwise anchor at **creation time**.
/// The old epoch anchor made every never-fired schedule instantly
/// "overdue", so a schedule created at 15:18 for a 16:00 cron fired a
/// spurious catch-up within seconds (Chime dogfood, 2026-07-06) — the
/// reflection scheduler's boot-fire bug, same family. A `created_at`
/// of 0 (hand-crafted records) still degrades to epoch harmlessly.
fn last_fired_or_epoch(sched: &ScheduleRecord) -> chrono::DateTime<Utc> {
    match sched.last_fired_at {
        Some(ms) => {
            chrono::DateTime::from_timestamp_millis(ms as i64)
                .unwrap_or(chrono::DateTime::UNIX_EPOCH)
        }
        None => chrono::DateTime::from_timestamp_millis(sched.created_at as i64)
            .unwrap_or(chrono::DateTime::UNIX_EPOCH),
    }
}

/// Deduplication: a schedule has "already fired in this window" if
/// `last_fired_at` ≥ the fire time we're considering.
fn already_fired_in_window(sched: &ScheduleRecord, fire_time: chrono::DateTime<Utc>) -> bool {
    match sched.last_fired_at {
        Some(ms) => {
            let last = chrono::DateTime::from_timestamp_millis(ms as i64)
                .unwrap_or(chrono::DateTime::UNIX_EPOCH);
            last >= fire_time
        }
        None => false,
    }
}

fn update_earliest(
    earliest: &mut Option<Duration>,
    fire_time: chrono::DateTime<Utc>,
    now: chrono::DateTime<Utc>,
) {
    let delta = (fire_time - now).to_std().unwrap_or(Duration::ZERO);
    match earliest {
        Some(current) if delta < *current => *earliest = Some(delta),
        None => *earliest = Some(delta),
        _ => {}
    }
}

/// Chapter Muster — the deterministic team-mission half of `fire_schedule`,
/// extracted into its own function so it's testable without constructing a
/// `TriggerDispatch` (which nothing in this codebase does today — see this
/// function's own test for why). Loads `sched.team_mission`'s pack config
/// (if any) fresh at fire time, then starts the mission via
/// `TeamMissionService::start_from_goal_for_schedule`, tagging it with
/// `sched.schedule_id`. Errors are logged, never propagated — matches
/// `fire_schedule`'s own existing fire-and-forget error handling.
async fn fire_team_mission_schedule(
    sched: &ScheduleRecord,
    tm: &crate::schedule::ScheduledTeamMission,
    team_missions: &crate::team_mission_driver::TeamMissionService,
    report_ctx: Option<&ReportContext>,
) {
    let config = match &tm.pack_config {
        Some(path) => match aivyx_team::TeamConfig::load(path) {
            Ok(c) => Some(c),
            Err(e) => {
                let msg = format!(
                    "schedule {:?}'s pack_config {path:?} failed to load: {e}",
                    sched.schedule_id
                );
                eprintln!("aivyx-pa scheduler: {msg}");
                notify_team_mission_failure(sched, &msg, report_ctx).await;
                return;
            }
        },
        None => None,
    };
    match team_missions
        .start_from_goal_for_schedule(&tm.goal, config, &sched.schedule_id)
        .await
    {
        Ok(mission_id) => {
            eprintln!(
                "aivyx-pa scheduler: schedule {:?} started team mission {mission_id}",
                sched.schedule_id
            );
        }
        Err(e) => {
            let msg = format!(
                "schedule {:?} failed to start a team mission: {e}",
                sched.schedule_id
            );
            eprintln!("aivyx-pa scheduler: {msg}");
            notify_team_mission_failure(sched, &msg, report_ctx).await;
        }
    }
}

/// Chapter Muster — a scheduled team mission failed to start at all (bad
/// `pack_config` path, or the goal failed to decompose into a plan).
/// Unlike a mission that starts and later fails/gets rejected/halts
/// (which already notifies via `notify_mission_result`'s own
/// `AwaitingApproval`/terminal-phase path), a mission that never starts
/// has no `TeamMissionRecord` for that mechanism to key off of -- so this
/// is a separate, narrower notify path, mirroring `run_digest_report`'s
/// own dispatch pattern exactly (same `resolve_notify_targets` +
/// `ctx.notify` shape). `notify_when` is not consulted: a start failure
/// is always worth surfacing, unconditionally, since it's the one
/// outcome an operator has no other way to learn about (there's no
/// mission at all to look up in Mission Control).
async fn notify_team_mission_failure(
    sched: &ScheduleRecord,
    message: &str,
    report_ctx: Option<&ReportContext>,
) {
    let Some(ctx) = report_ctx else { return };
    let Some(notify) = &ctx.notify else { return };
    let targets = crate::trigger::resolve_notify_targets(
        &sched.notify_targets,
        ctx.default_notify_target.as_deref(),
    );
    if targets.is_empty() {
        return;
    }
    let subject = format!("cron: {}", sched.schedule_id);
    for target in &targets {
        if let Err(e) = notify.dispatch(target, message, Some(&subject)).await {
            eprintln!("aivyx-pa scheduler: team-mission-failure notify to {target:?} failed: {e}");
        }
    }
}

/// Fire a single scheduled turn through the shared trigger dispatch.
async fn fire_schedule(
    dispatch: &TriggerDispatch,
    store: &DomainHandle,
    sched: &ScheduleRecord,
    report_ctx: Option<&ReportContext>,
    team_missions: Option<&crate::team_mission_driver::TeamMissionService>,
) {
    if let Some(tm) = &sched.team_mission {
        match team_missions {
            Some(svc) => fire_team_mission_schedule(sched, tm, svc, report_ctx).await,
            None => eprintln!(
                "aivyx-pa scheduler: schedule {:?} targets a team mission but no \
                 TeamMissionService is wired — skipping",
                sched.schedule_id
            ),
        }
        update_last_fired(store, sched).await;
        return;
    }

    // Chapter Ledger — a `report_kind = "digest"` routine runs a deterministic
    // daemon-assembled digest instead of an LLM turn, so it cannot confabulate
    // (#6). Falls through to the normal LLM path if no builder is wired.
    if sched.report_kind.as_deref() == Some("digest") {
        if let Some(ctx) = report_ctx {
            run_digest_report(ctx, sched).await;
            update_last_fired(store, sched).await;
            return;
        }
        eprintln!(
            "aivyx-pa scheduler: schedule {:?} is report_kind=digest but no digest \
             builder is wired — falling back to the LLM prompt",
            sched.schedule_id
        );
    }

    dispatch
        .fire(
            TriggerSource::Cron,
            &sched.schedule_id,
            &sched.prompt,
            sched.wrap_mission,
            &sched.notify_targets,
            sched.notify_when,
        )
        .await;

    update_last_fired(store, sched).await;
}

/// Chapter Ledger — build the deterministic digest (since the last fire) +
/// persist it + push it to any notify targets. No LLM in the content path.
async fn run_digest_report(ctx: &ReportContext, sched: &ScheduleRecord) {
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    // Cover everything since the last fire (ms→s); first run → since 0.
    let since_secs = sched.last_fired_at.map(|ms| ms / 1000).unwrap_or(0);

    let text = ctx.digest.run(since_secs, now_secs).await;
    eprintln!(
        "aivyx-pa scheduler: deterministic digest written ({} chars) for {:?}",
        text.len(),
        sched.schedule_id
    );

    // Deliver to notify targets (the digest text is always a real, non-empty,
    // successful completion). `OnFailed` opts out (a digest never fails);
    // `Always` / `OnCompletedNonEmpty` deliver the briefing.
    let targets: Vec<String> = crate::trigger::resolve_notify_targets(
        &sched.notify_targets,
        ctx.default_notify_target.as_deref(),
    );
    if !targets.is_empty() && sched.notify_when != aivyx_config::NotifyWhen::OnFailed {
        if let Some(notify) = &ctx.notify {
            let subject = format!("cron: {}", sched.schedule_id);
            for target in &targets {
                if let Err(e) = notify.dispatch(target, &text, Some(&subject)).await {
                    eprintln!(
                        "aivyx-pa scheduler: digest notify to {target:?} failed: {e}"
                    );
                }
            }
        }
    }
}

/// Stamp `last_fired_at = now` (shared by both fire paths).
async fn update_last_fired(store: &DomainHandle, sched: &ScheduleRecord) {
    // Update last_fired_at regardless of outcome.
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    let mut updated = sched.clone();
    updated.last_fired_at = Some(now_ms);
    if let Err(e) = schedule::update_schedule(store, &updated).await {
        eprintln!(
            "aivyx-pa scheduler: failed to update last_fired_at for {:?}: {e}",
            sched.schedule_id,
        );
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::DateTime;

    fn make_schedule(cron_expr: &str, last_fired: Option<u64>) -> ScheduleRecord {
        let mut r = ScheduleRecord::new(
            "test-sched".into(),
            cron_expr.into(),
            "default".into(),
            "do something".into(),
        )
        .unwrap();
        r.last_fired_at = last_fired;
        r
    }

    #[test]
    fn never_fired_schedule_anchors_at_creation_not_epoch() {
        // A schedule created NOW for a later tick must not be "overdue
        // since 1970" — the epoch anchor fired a 16:00 schedule seconds
        // after its 15:18 creation (Chime dogfood, 2026-07-06).
        let s = make_schedule("0 * * * * * *", None);
        let anchor = last_fired_or_epoch(&s);
        assert_eq!(anchor.timestamp_millis(), s.created_at as i64);
        let next = s.next_fire_time_after(anchor).expect("has a next fire");
        assert!(
            next > anchor,
            "first fire must be the next tick after creation"
        );
    }

    #[test]
    fn last_fired_or_epoch_returns_timestamp() {
        let ts: u64 = 1_700_000_000_000; // 2023-11-14
        let s = make_schedule("0 * * * * * *", Some(ts));
        let dt = last_fired_or_epoch(&s);
        assert_eq!(dt.timestamp_millis(), ts as i64);
    }

    #[test]
    fn already_fired_in_window_prevents_double_fire() {
        let fire_time = DateTime::parse_from_rfc3339("2026-06-01T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        // last_fired_at is AT the fire time — should be considered fired.
        let s = make_schedule(
            "0 0 9 * * * *",
            Some(fire_time.timestamp_millis() as u64),
        );
        assert!(already_fired_in_window(&s, fire_time));
    }

    #[test]
    fn not_fired_when_last_fired_is_before_window() {
        let fire_time = DateTime::parse_from_rfc3339("2026-06-01T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        // last_fired is one hour before — not in this window.
        let earlier = fire_time - chrono::Duration::hours(1);
        let s = make_schedule(
            "0 0 9 * * * *",
            Some(earlier.timestamp_millis() as u64),
        );
        assert!(!already_fired_in_window(&s, fire_time));
    }

    #[test]
    fn not_fired_when_no_last_fired() {
        let fire_time = DateTime::parse_from_rfc3339("2026-06-01T09:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let s = make_schedule("0 0 9 * * * *", None);
        assert!(!already_fired_in_window(&s, fire_time));
    }

    fn probe_answering(answers: Arc<std::sync::atomic::AtomicBool>, calls: Arc<std::sync::atomic::AtomicUsize>) -> ModelProbe {
        Arc::new(move || {
            let answers = Arc::clone(&answers);
            let calls = Arc::clone(&calls);
            Box::pin(async move {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                answers.load(std::sync::atomic::Ordering::SeqCst)
            })
        })
    }

    #[tokio::test]
    async fn routines_wait_until_the_model_answers_once() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        let answers = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(AtomicUsize::new(0));
        let mut hold = StartupHold::new(
            Some(probe_answering(Arc::clone(&answers), Arc::clone(&calls))),
            Duration::from_secs(600),
        );
        let start = Instant::now();
        // Model still loading: held.
        assert!(!hold.model_ready(start).await);
        assert!(!hold.model_ready(start + Duration::from_secs(15)).await);
        // It answers: routines run, and the hold never asks again.
        answers.store(true, Ordering::SeqCst);
        assert!(hold.model_ready(start + Duration::from_secs(30)).await);
        answers.store(false, Ordering::SeqCst);
        assert!(hold.model_ready(start + Duration::from_secs(45)).await);
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn routines_run_anyway_once_the_start_up_window_has_passed() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        let calls = Arc::new(AtomicUsize::new(0));
        let mut hold = StartupHold::new(
            Some(probe_answering(Arc::new(AtomicBool::new(false)), Arc::clone(&calls))),
            Duration::from_secs(600),
        );
        let later = Instant::now() + Duration::from_secs(601);
        assert!(hold.model_ready(later).await, "a model still down after the window is a real failure");
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn without_a_probe_nothing_is_held() {
        let mut hold = StartupHold::new(None, Duration::from_secs(600));
        assert!(hold.model_ready(Instant::now()).await);
    }

    #[test]
    fn only_a_wired_digest_runs_without_the_model() {
        let mut digest = make_schedule("0 0 7 * * * *", None);
        digest.report_kind = Some("digest".into());
        assert!(!needs_model(&digest, true));
        assert!(needs_model(&digest, false), "without a builder the digest falls back to the model");
        assert!(needs_model(&make_schedule("0 0 7 * * * *", None), true));
    }

    #[test]
    fn update_earliest_picks_minimum() {
        let now = Utc::now();
        let mut earliest: Option<Duration> = None;

        let t1 = now + chrono::Duration::seconds(120);
        update_earliest(&mut earliest, t1, now);
        assert!(earliest.is_some());
        let first = earliest.unwrap();

        let t2 = now + chrono::Duration::seconds(30);
        update_earliest(&mut earliest, t2, now);
        assert!(earliest.unwrap() < first);

        // A later time shouldn't replace the earlier one.
        let t3 = now + chrono::Duration::seconds(300);
        update_earliest(&mut earliest, t3, now);
        assert!(earliest.unwrap() < Duration::from_secs(60));
    }

    #[test]
    fn config_to_records_converts_correctly() {
        let configs = vec![aivyx_config::ScheduleConfig {
            name: "daily-check".into(),
            cron: "0 0 9 * * * *".into(),
            role: "ops".into(),
            prompt: "check system health".into(),
            enabled: true,
            wrap_mission: false,
            notify_target: None,
            notify_targets: Vec::new(),
            notify_when: aivyx_config::NotifyWhen::Always,
            report_kind: None,
            team_mission: None,
        }];
        let records = config_to_records(&configs).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].schedule_id, "cfg-daily-check");
        assert_eq!(records[0].role_name, "ops");
        assert!(records[0].enabled);
    }

    #[test]
    fn config_to_records_rejects_invalid_cron() {
        let configs = vec![aivyx_config::ScheduleConfig {
            name: "bad".into(),
            cron: "not valid".into(),
            role: "default".into(),
            prompt: "test".into(),
            enabled: true,
            wrap_mission: false,
            notify_target: None,
            notify_targets: Vec::new(),
            notify_when: aivyx_config::NotifyWhen::Always,
            report_kind: None,
            team_mission: None,
        }];
        assert!(config_to_records(&configs).is_err());
    }

    #[test]
    fn disabled_config_produces_disabled_record() {
        let configs = vec![aivyx_config::ScheduleConfig {
            name: "off".into(),
            cron: "0 0 9 * * * *".into(),
            role: "default".into(),
            prompt: "test".into(),
            enabled: false,
            wrap_mission: false,
            notify_target: None,
            notify_targets: Vec::new(),
            notify_when: aivyx_config::NotifyWhen::Always,
            report_kind: None,
            team_mission: None,
        }];
        let records = config_to_records(&configs).unwrap();
        assert!(!records[0].enabled);
    }

    #[test]
    fn config_to_records_propagates_wrap_mission() {
        let configs = vec![aivyx_config::ScheduleConfig {
            name: "wrapped".into(),
            cron: "0 0 9 * * * *".into(),
            role: "default".into(),
            prompt: "test".into(),
            enabled: true,
            wrap_mission: true,
            notify_target: None,
            notify_targets: Vec::new(),
            notify_when: aivyx_config::NotifyWhen::Always,
            report_kind: None,
            team_mission: None,
        }];
        let records = config_to_records(&configs).unwrap();
        assert!(records[0].wrap_mission);
    }

    #[tokio::test]
    async fn fire_team_mission_schedule_starts_a_tagged_mission() {
        let sched = ScheduleRecord::new_team_mission(
            "cfg-nightly-boh-close".to_string(),
            "0 0 2 * * * *".to_string(),
            "run the overnight close".to_string(),
            None,
        )
        .expect("valid");
        let svc = crate::team_mission_driver::TeamMissionService::new(
            crate::team_mission_driver::SharedMissionState::new(
                crate::team_mission_driver::tests::team_domain().await,
            ),
            crate::team_mission_driver::tests::deps(crate::team_mission_driver::tests::TOOL_PLAN_JSON),
            aivyx_team::default_nonagon(),
            aivyx_core::GatePolicy::Interactive,
        );
        let tm = sched.team_mission.as_ref().expect("set");
        fire_team_mission_schedule(&sched, tm, &svc, None).await;
        let missions = svc.list();
        assert_eq!(missions.len(), 1);
        assert_eq!(missions[0].triggered_by.as_deref(), Some("cfg-nightly-boh-close"));
    }

    /// Build the smallest valid `ReportContext` for a test -- deviation
    /// from the brief's illustrative `WeeklyDigestBuilder::new_for_test()`
    /// (no such constructor exists; the real one is
    /// `WeeklyDigestBuilder::new(memory: Arc<dyn Memory>)`, confirmed
    /// against `digest.rs`, which its own tests satisfy with
    /// `aivyx_memory::InMemoryMemory`).
    fn test_report_ctx(
        dispatcher: crate::notify_dispatcher::NotifyDispatcher,
    ) -> ReportContext {
        let mem: std::sync::Arc<dyn aivyx_memory::Memory> =
            std::sync::Arc::new(aivyx_memory::InMemoryMemory::new());
        ReportContext {
            digest: std::sync::Arc::new(crate::digest::WeeklyDigestBuilder::new(mem)),
            notify: Some(std::sync::Arc::new(dispatcher)),
            default_notify_target: None,
        }
    }

    #[tokio::test]
    async fn fire_team_mission_schedule_notifies_on_a_bad_pack_config_path() {
        let backend = crate::team_mission_driver::tests::RecordingNotifyBackend::new();
        let mut dispatcher = crate::notify_dispatcher::NotifyDispatcher::new();
        dispatcher.register("ops-channel", backend.clone());
        let mut sched = ScheduleRecord::new_team_mission(
            "cfg-bad-pack".to_string(),
            "0 0 2 * * * *".to_string(),
            "run the overnight close".to_string(),
            Some("/nonexistent/path/does-not-exist.toml".to_string()),
        )
        .expect("valid");
        sched.notify_targets = vec!["ops-channel".to_string()];
        let svc = crate::team_mission_driver::TeamMissionService::new(
            crate::team_mission_driver::SharedMissionState::new(
                crate::team_mission_driver::tests::team_domain().await,
            ),
            crate::team_mission_driver::tests::deps(crate::team_mission_driver::tests::TOOL_PLAN_JSON),
            aivyx_team::default_nonagon(),
            aivyx_core::GatePolicy::Interactive,
        );
        let tm = sched.team_mission.as_ref().expect("set");
        let ctx = test_report_ctx(dispatcher);
        fire_team_mission_schedule(&sched, tm, &svc, Some(&ctx)).await;
        assert_eq!(svc.list().len(), 0, "a bad pack_config never registers a mission");
        let calls = backend.calls.lock().unwrap();
        assert_eq!(calls.len(), 1, "the load failure notifies the schedule's own target");
        assert!(calls[0].0.contains("pack_config"), "message names what failed");
    }

    #[tokio::test]
    async fn fire_team_mission_schedule_loads_a_real_pack_config_successfully() {
        // The success half of Some(pack_config) -- previously entirely untested.
        let dir = std::env::temp_dir().join(format!(
            "aivyx-schedule-pack-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let pack_path = dir.join("pack.toml");
        let config = aivyx_team::default_nonagon();
        std::fs::write(&pack_path, config.to_toml().expect("serialize")).expect("write pack file");

        let sched = ScheduleRecord::new_team_mission(
            "cfg-real-pack".to_string(),
            "0 0 2 * * * *".to_string(),
            "run the overnight close".to_string(),
            Some(pack_path.to_string_lossy().to_string()),
        )
        .expect("valid");
        let svc = crate::team_mission_driver::TeamMissionService::new(
            crate::team_mission_driver::SharedMissionState::new(
                crate::team_mission_driver::tests::team_domain().await,
            ),
            crate::team_mission_driver::tests::deps(crate::team_mission_driver::tests::TOOL_PLAN_JSON),
            aivyx_team::default_nonagon(),
            aivyx_core::GatePolicy::Interactive,
        );
        let tm = sched.team_mission.as_ref().expect("set");
        fire_team_mission_schedule(&sched, tm, &svc, None).await;
        let missions = svc.list();
        assert_eq!(missions.len(), 1, "a valid pack_config path loads and starts a mission");
    }

    #[test]
    fn config_to_records_builds_a_team_mission_record() {
        let cfg = aivyx_config::ScheduleConfig {
            name: "nightly-boh-close".to_string(),
            cron: "0 0 2 * * * *".to_string(),
            role: "default".to_string(),
            prompt: String::new(),
            enabled: true,
            wrap_mission: false,
            notify_target: None,
            notify_targets: vec![],
            notify_when: aivyx_config::NotifyWhen::Always,
            report_kind: None,
            team_mission: Some(aivyx_config::ScheduledTeamMissionConfig {
                goal: "run the overnight close".to_string(),
                pack_config: None,
            }),
        };
        let records = config_to_records(&[cfg]).expect("valid");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].schedule_id, "cfg-nightly-boh-close");
        assert_eq!(records[0].role_name, "");
        assert_eq!(records[0].prompt, "");
        let tm = records[0].team_mission.as_ref().expect("team_mission set");
        assert_eq!(tm.goal, "run the overnight close");
    }
}
