//! The Command Center briefing: [`gather`] reads what the record already
//! holds into [`Facts`]; [`compose`] (pure) writes every sentence. No model
//! is involved — the wording is fixed templates, in the assistant's voice.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use aivyx_audit::{AuditEvent, AutoNotifyOutcomeSummary, PersistentAuditLog, SignedEntry};
use aivyx_ipc::briefing::{Briefing, LogEntry, NeedsYouAction, NeedsYouItem, UpcomingItem};
use aivyx_storage::DomainHandle;

/// How many log lines the page shows.
pub const LOG_CAP: usize = 12;
const DAY: i64 = 24 * 3600;
const WEEK: i64 = 7 * DAY;
/// How far ahead "Needs you" looks for reminders. The reminder driver fires
/// (and removes) a reminder within ~30 s of it falling due, so the page
/// shows the ones coming up, not only the already-due.
pub const REMINDER_LOOKAHEAD_SECS: i64 = 2 * 3600;
/// A schedule's `last_fired_at` within this of a `trg-` mission run of the
/// same routine is that run, not another one.
const DUPLICATE_RUN_SECS: i64 = 120;
/// How many pieces of work in progress "Coming up" names.
const IN_PROGRESS_CAP: usize = 3;
/// Tool bases whose completed calls aren't "changes" for the log, beside
/// read-only bases and `memory.*` (which gets its own line).
/// - `workspace`: the assistant's own notebook; every op (read or write)
///   shares the one base.
/// - `mcp.call`: every MCP tool shares the one base, and whether a call
///   read or wrote isn't recorded.
const NOT_CHANGES: &[&str] = &["workspace", "mcp.call"];

/// A routine (or other trigger) run inside the window.
#[derive(Debug, Clone, PartialEq)]
pub struct RoutineRun {
    /// "routine digest", "webhook trigger deploy".
    pub what: String,
    pub at_unix: i64,
    /// `Some(true)` failed, `Some(false)` succeeded, `None` unknown (the run
    /// wasn't wrapped in a mission, so only "it ran" is on record).
    pub failed: Option<bool>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NotifyFact {
    pub target: String,
    pub at_unix: i64,
    /// `None` = delivered; `Some(kind)` = failed with that error kind.
    pub error_kind: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GateFact {
    pub mission_id: String,
    pub gate_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TeamGateFact {
    pub mission_id: String,
    pub step: String,
    pub goal: String,
}

/// A step an unattended run parked for review (supervised batching).
#[derive(Debug, Clone, PartialEq)]
pub struct ParkedFact {
    pub id: String,
    pub summary: String,
    /// Which run parked it ("routine digest").
    pub origin: String,
    pub parked_at: i64,
    /// What the step touches as it is now.
    pub preview: Option<String>,
}

/// A parked step resolved inside the window.
#[derive(Debug, Clone, PartialEq)]
pub struct ParkedResolvedFact {
    pub summary: String,
    pub state: aivyx_ipc::parked::ParkedState,
    pub at_unix: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProposalFact {
    pub id: String,
    /// The category in words ("communication style").
    pub category: String,
    pub is_skill: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReminderFact {
    pub id: String,
    pub message: String,
    pub due_unix: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UpcomingFact {
    pub name: String,
    pub at_unix: i64,
}

/// Everything the composer needs, already read.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Facts {
    pub last_here: Option<i64>,
    pub window_start: i64,
    pub window_capped: bool,
    pub routine_runs: Vec<RoutineRun>,
    pub notifications: Vec<NotifyFact>,
    /// Completed non-read-only tool calls in the window: (tool base, at).
    pub changes: Vec<(String, i64)>,
    /// Memory writes in the window (their times).
    pub memories_saved: Vec<i64>,
    pub mission_gates: Vec<GateFact>,
    pub team_gates: Vec<TeamGateFact>,
    /// Steps parked for review, still pending.
    pub parked_pending: Vec<ParkedFact>,
    /// Parked steps resolved in the window.
    pub parked_resolved: Vec<ParkedResolvedFact>,
    pub proposals: Vec<ProposalFact>,
    /// Reminders due within [`REMINDER_LOOKAHEAD_SECS`] (and any already
    /// due the driver hasn't fired yet).
    pub reminders: Vec<ReminderFact>,
    pub upcoming: Vec<UpcomingFact>,
    /// Goals of work in progress.
    pub in_progress: Vec<String>,
    pub spend_24h_usd: Option<f64>,
    /// `true` when any `LlmCost` in the 24 h window had no known price, so
    /// `spend_24h_usd` is a lower bound, not the full spend.
    pub spend_untracked: bool,
    pub memory_topics: Option<u64>,
    /// Sources that couldn't be read ("reminders", "the audit trail", …).
    pub source_errors: Vec<String>,
}

/// Where the log starts: the end of the last visit, else the last 24 h,
/// never more than 7 days back. Returns `(start, capped)`.
pub fn window(last_here: Option<i64>, now: i64) -> (i64, bool) {
    match last_here {
        None => (now - DAY, false),
        Some(t) if t < now - WEEK => (now - WEEK, true),
        Some(t) => (t, false),
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    if n == 1 { format!("1 {one}") } else { format!("{n} {many}") }
}

fn item(key: String, sentence: String, detail: Option<String>, action: NeedsYouAction, link: &str) -> NeedsYouItem {
    NeedsYouItem { key, sentence, detail, action, link: link.to_string() }
}

/// How long ago: "5 min", "3 h", "2 days" (at least "1 min").
fn ago(secs: i64) -> String {
    let secs = secs.max(0);
    if secs < 3600 {
        format!("{} min", (secs / 60).max(1))
    } else if secs < DAY {
        format!("{} h", secs / 3600)
    } else {
        let d = secs / DAY;
        format!("{d} {}", if d == 1 { "day" } else { "days" })
    }
}

/// `due_unix` relative to `now`: "due now", "due in 25 min", "due in 1 h
/// 10 min". Minutes round up, so "due in 0 min" never shows.
fn due_in(due_unix: i64, now: i64) -> String {
    if due_unix <= now {
        return "due now".into();
    }
    let mins = (due_unix - now + 59) / 60;
    match (mins / 60, mins % 60) {
        (0, m) => format!("due in {m} min"),
        (h, 0) => format!("due in {h} h"),
        (h, m) => format!("due in {h} h {m} min"),
    }
}

/// Pure: facts in, the briefing out.
pub fn compose(f: &Facts, now: i64) -> Briefing {
    let mut needs = Vec::new();

    // Approvals.
    for g in &f.mission_gates {
        needs.push(item(
            format!("gate:{}:{}", g.mission_id, g.gate_id),
            format!("A mission is waiting for your go-ahead: {}.", g.reason.trim_end_matches('.')),
            None,
            NeedsYouAction::MissionGate { mission_id: g.mission_id.clone(), gate_id: g.gate_id.clone() },
            "missions",
        ));
    }
    for g in &f.team_gates {
        needs.push(item(
            format!("team-gate:{}:{}", g.mission_id, g.step),
            format!("The team is waiting for your go-ahead on \u{201c}{}\u{201d} (step {}).", g.goal, g.step),
            None,
            NeedsYouAction::TeamGate { mission_id: g.mission_id.clone(), step: g.step.clone() },
            "mission-control",
        ));
    }
    // Steps unattended runs parked for review (supervised batching).
    for p in &f.parked_pending {
        needs.push(item(
            format!("parked:{}", p.id),
            format!("The {} is waiting for your go-ahead: {}.", p.origin, p.summary.trim_end_matches('.')),
            Some(format!("Parked {} ago.", ago(now - p.parked_at))),
            NeedsYouAction::ParkedStep { id: p.id.clone(), preview: p.preview.clone() },
            "command",
        ));
    }
    // Proposals.
    for p in &f.proposals {
        let sentence = if p.is_skill {
            "I'd like to add or refine a skill.".to_string()
        } else {
            format!("I'd like to update my {}.", p.category)
        };
        let link = if p.is_skill { "skills" } else { "agents" };
        needs.push(item(format!("proposal:{}", p.id), sentence, p.reason.clone(), NeedsYouAction::Review, link));
    }
    // Went wrong: one card per failing routine, in first-failure order.
    let mut failures: Vec<(&str, usize)> = Vec::new();
    for r in f.routine_runs.iter().filter(|r| r.failed == Some(true)) {
        match failures.iter_mut().find(|(w, _)| *w == r.what) {
            Some(g) => g.1 += 1,
            None => failures.push((&r.what, 1)),
        }
    }
    for (what, count) in failures {
        let sentence =
            if count == 1 { format!("The {what} failed.") } else { format!("The {what} failed {count} times.") };
        needs.push(item(format!("failed:{what}"), sentence, None, NeedsYouAction::Look, "missions"));
    }
    // Failed notifications, grouped by (target, kind) in first-seen order.
    let mut groups: Vec<(&str, &str, usize)> = Vec::new();
    for n in &f.notifications {
        if let Some(kind) = &n.error_kind {
            match groups.iter_mut().find(|(t, k, _)| *t == n.target && *k == kind) {
                Some(g) => g.2 += 1,
                None => groups.push((&n.target, kind, 1)),
            }
        }
    }
    for (target, kind, count) in groups {
        let what = if count == 1 { "a notification".to_string() } else { format!("{count} notifications") };
        needs.push(item(
            format!("notify-failed:{target}:{kind}"),
            format!("I couldn't send {what} to {target} ({kind})."),
            None,
            NeedsYouAction::Look,
            "notifications",
        ));
    }
    for s in &f.source_errors {
        needs.push(item(
            format!("source:{s}"),
            format!("I couldn't read {s} just now."),
            None,
            NeedsYouAction::Look,
            "command",
        ));
    }
    // Reminders coming up, soonest first.
    let mut reminders: Vec<&ReminderFact> = f.reminders.iter().collect();
    reminders.sort_by(|a, b| a.due_unix.cmp(&b.due_unix).then(a.id.cmp(&b.id)));
    for r in reminders {
        needs.push(item(
            format!("reminder:{}", r.id),
            format!("Reminder: {}", r.message),
            Some(due_in(r.due_unix, now)),
            NeedsYouAction::Reminder { id: r.id.clone() },
            "reminders",
        ));
    }

    // The log.
    let mut log: Vec<LogEntry> = Vec::new();
    for r in &f.routine_runs {
        let (sentence, warn) = match r.failed {
            Some(true) => (format!("The {} failed.", r.what), true),
            _ => (format!("I ran the {}.", r.what), false),
        };
        log.push(LogEntry { at_unix: r.at_unix, sentence, warn, link: "schedules".into() });
    }
    for n in &f.notifications {
        let (sentence, warn) = match &n.error_kind {
            None => (format!("I sent a notification to {}.", n.target), false),
            Some(kind) => (format!("A notification to {} failed ({kind}).", n.target), true),
        };
        log.push(LogEntry { at_unix: n.at_unix, sentence, warn, link: "notifications".into() });
    }
    if let Some(last) = f.changes.iter().map(|(_, t)| *t).max() {
        let mut counts: Vec<(&str, usize)> = Vec::new();
        for (tool, _) in &f.changes {
            match counts.iter_mut().find(|(t, _)| *t == tool) {
                Some(c) => c.1 += 1,
                None => counts.push((tool, 1)),
            }
        }
        counts.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        let mut named: Vec<String> = counts.iter().take(3).map(|(t, n)| format!("{t} \u{d7}{n}")).collect();
        if counts.len() > 3 {
            named.push("and others".into());
        }
        log.push(LogEntry {
            at_unix: last,
            sentence: format!("I made {}: {}.", plural(f.changes.len(), "change", "changes"), named.join(", ")),
            warn: false,
            link: "audit".into(),
        });
    }
    for p in &f.parked_resolved {
        use aivyx_ipc::parked::ParkedState as S;
        let (sentence, warn) = match p.state {
            S::Approved => (format!("You approved a parked step: {}.", p.summary), false),
            S::Denied => (format!("You turned down a parked step: {}.", p.summary), false),
            S::Lapsed => (format!("A parked step lapsed unreviewed: {}.", p.summary), true),
            S::Failed => (format!("A parked step you approved failed: {}.", p.summary), true),
            S::Pending => continue,
        };
        log.push(LogEntry { at_unix: p.at_unix, sentence, warn, link: "command".into() });
    }
    if let Some(last) = f.memories_saved.iter().copied().max() {
        log.push(LogEntry {
            at_unix: last,
            sentence: format!("I saved {}.", plural(f.memories_saved.len(), "memory", "memories")),
            warn: false,
            link: "memory".into(),
        });
    }
    log.sort_by_key(|l| l.at_unix);
    let log_more = log.len().saturating_sub(LOG_CAP);
    let log: Vec<LogEntry> = log.into_iter().skip(log_more).collect();

    // Coming up.
    let mut upcoming: Vec<&UpcomingFact> = f.upcoming.iter().collect();
    upcoming.sort_by_key(|u| u.at_unix);
    let mut coming_up: Vec<UpcomingItem> = upcoming
        .into_iter()
        .take(3)
        .map(|u| UpcomingItem { at_unix: Some(u.at_unix), sentence: u.name.clone(), link: "schedules".into() })
        .collect();
    coming_up.extend(f.in_progress.iter().take(IN_PROGRESS_CAP).map(|goal| UpcomingItem {
        at_unix: None,
        sentence: format!("Working on \u{201c}{goal}\u{201d}."),
        link: "mission-control".into(),
    }));
    if f.in_progress.len() > IN_PROGRESS_CAP {
        coming_up.push(UpcomingItem {
            at_unix: None,
            sentence: format!("and {} more in progress.", f.in_progress.len() - IN_PROGRESS_CAP),
            link: "mission-control".into(),
        });
    }

    Briefing {
        last_active_unix: f.last_here,
        window_start_unix: f.window_start,
        window_capped: f.window_capped,
        spend_24h_usd: f.spend_24h_usd,
        memory_topics: f.memory_topics,
        needs_you: needs,
        log,
        log_more: log_more as u32,
        coming_up,
        spend_untracked: f.spend_untracked,
    }
}

/// Read handles for [`gather`]. Every source is optional; a missing one is
/// simply absent from the briefing, an unreadable one is named in
/// `Facts::source_errors`.
pub struct BriefingSources<'a> {
    pub mission_store: Option<&'a DomainHandle>,
    pub schedule_store: Option<&'a DomainHandle>,
    pub audit_log: Option<&'a PersistentAuditLog>,
    pub persona_proposals: Option<&'a crate::persona_proposal::PersistentPersonaProposalLog>,
    pub team_missions: Option<&'a crate::team_mission_driver::TeamMissionService>,
    /// Supervised batching — steps parked for review; `None` when no area is
    /// supervised.
    pub parked: Option<&'a crate::parked_steps::StepParker>,
    pub reminders: Option<&'a crate::reminder_tool::SharedReminderStore>,
    pub memory: Option<&'a Arc<dyn aivyx_memory::Memory>>,
    pub pricing: &'a aivyx_cost::Pricing,
    /// The daemon's audit-walk cache; `None` walks the chain every time.
    pub audit_cache: Option<&'a AuditFactsCache>,
}

/// What an audit walk depends on: the chain's length, the log window's
/// start, and the minute (so the rolling 24 h spend boundary moves at most
/// a minute late).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuditKey {
    pub chain_len: u64,
    pub window_start: i64,
    pub minute: i64,
}

/// The key for a walk at `now`. The walk runs at the start of `now`'s
/// minute, so a window that rolls with the clock (no last visit, or the
/// 7-day cap) stays put across the minute's polls.
pub fn audit_key(chain_len: u64, last_here: Option<i64>, now: i64) -> AuditKey {
    let minute = now.div_euclid(60);
    AuditKey { chain_len, window_start: window(last_here, minute * 60).0, minute }
}

/// The last audit walk, kept so repeated briefing polls with nothing new
/// on the chain skip the walk. One per daemon.
#[derive(Debug, Default)]
pub struct AuditFactsCache {
    last: std::sync::Mutex<Option<(AuditKey, AuditFacts)>>,
}

impl AuditFactsCache {
    /// The cached facts for `key`, else `compute()`'s (cached on success).
    /// The lock is held across `compute`, so concurrent polls walk once.
    pub fn get_or_compute<E>(
        &self,
        key: AuditKey,
        compute: impl FnOnce() -> Result<AuditFacts, E>,
    ) -> Result<AuditFacts, E> {
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((k, facts)) = last.as_ref()
            && *k == key
        {
            return Ok(facts.clone());
        }
        let facts = compute()?;
        *last = Some((key, facts.clone()));
        Ok(facts)
    }
}

/// What the audit chain contributes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AuditFacts {
    pub notifications: Vec<NotifyFact>,
    pub changes: Vec<(String, i64)>,
    pub memories_saved: Vec<i64>,
    pub spend_24h_usd: f64,
    /// `true` when any `LlmCost` in the 24 h window had no known price, so
    /// `spend_24h_usd` is a lower bound, not the full spend.
    pub spend_untracked: bool,
}

fn unix_of(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Pure fold over recent chain entries: window events for the log, and the
/// last 24 h of `LlmCost` for the spend instrument.
pub fn audit_facts(
    entries: &[SignedEntry],
    window_start: i64,
    now: i64,
    pricing: &aivyx_cost::Pricing,
) -> AuditFacts {
    let mut a = AuditFacts::default();
    for e in entries {
        let t = unix_of(e.appended_at);
        if let AuditEvent::LlmCost { model, usage, .. } = &e.event
            && t >= now - DAY
        {
            let counts = aivyx_cost::TokenCounts {
                input: usage.input_tokens as u64,
                output: usage.output_tokens as u64,
                cache_read: usage.cache_read_input_tokens as u64,
                cache_write: usage.cache_creation_input_tokens as u64,
            };
            let cost = pricing.cost_of(model, &counts);
            a.spend_24h_usd += cost.usd;
            if !cost.priced {
                a.spend_untracked = true;
            }
        }
        if t < window_start {
            continue;
        }
        match &e.event {
            AuditEvent::ToolCall { scope_used, outcome: aivyx_core::ToolOutcomeSummary::Completed { .. }, .. }
                // Memory writes get their own line ("I saved N memories").
                if !aivyx_capability::is_read_only_base(scope_used.base())
                    && !scope_used.base().starts_with("memory.")
                    && !NOT_CHANGES.contains(&scope_used.base()) =>
            {
                a.changes.push((scope_used.base().to_string(), t));
            }
            AuditEvent::MemoryAccess { operation: aivyx_audit::MemoryOperation::Write, .. } => {
                a.memories_saved.push(t);
            }
            AuditEvent::AutoNotifyDispatched { target_name, outcome, .. } => match outcome {
                AutoNotifyOutcomeSummary::Delivered => a.notifications.push(NotifyFact {
                    target: target_name.clone(),
                    at_unix: t,
                    error_kind: None,
                }),
                AutoNotifyOutcomeSummary::Failed { error_kind, .. } => a.notifications.push(NotifyFact {
                    target: target_name.clone(),
                    at_unix: t,
                    error_kind: Some(error_kind.clone()),
                }),
                _ => {}
            },
            _ => {}
        }
    }
    a
}

/// `"cron trigger cfg-digest: …"` → `"routine digest"`; other trigger
/// sources keep their name (`"webhook trigger deploy"`). Not a trigger
/// mission → `None`.
pub fn trigger_label(description: &str) -> Option<String> {
    let (head, _) = description.split_once(": ")?;
    let (source, id) = head.split_once(" trigger ")?;
    let id = id.strip_prefix("cfg-").unwrap_or(id);
    Some(if source == "cron" { format!("routine {id}") } else { format!("{source} trigger {id}") })
}

/// The newest entries back to `since` (inclusive), oldest first. Pages
/// backwards so a long chain isn't read in full.
fn read_since(log: &PersistentAuditLog, since: i64) -> Result<Vec<SignedEntry>, String> {
    page_back(log.len() as u64, |from, n| log.entries_range(from, n).map_err(|e| e.to_string()), since)
}

/// [`read_since`] over any `fetch(from, n)` of a `len`-entry chain: reads
/// pages newest-first until one starts before `since`, then concatenates
/// them oldest-first once.
fn page_back(
    len: u64,
    mut fetch: impl FnMut(u64, usize) -> Result<Vec<SignedEntry>, String>,
    since: i64,
) -> Result<Vec<SignedEntry>, String> {
    const PAGE: u64 = 512;
    let mut end = len;
    let mut pages: Vec<Vec<SignedEntry>> = Vec::new();
    while end > 0 {
        let from = end.saturating_sub(PAGE);
        let page = fetch(from, (end - from) as usize)?;
        let reached = page.first().is_some_and(|e| unix_of(e.appended_at) < since);
        pages.push(page);
        if reached {
            break;
        }
        end = from;
    }
    let mut out: Vec<SignedEntry> = pages.into_iter().rev().flatten().collect();
    out.retain(|e| unix_of(e.appended_at) >= since);
    Ok(out)
}

/// Category in words: `CommunicationStyle` → `communication style`.
fn category_words(debug_name: &str) -> String {
    let mut out = String::new();
    for (i, ch) in debug_name.chars().enumerate() {
        if ch.is_uppercase() && i > 0 {
            out.push(' ');
        }
        out.extend(ch.to_lowercase());
    }
    out
}

/// Read every source into [`Facts`].
pub async fn gather(src: &BriefingSources<'_>, last_here: Option<i64>, now: i64) -> Facts {
    let (window_start, window_capped) = window(last_here, now);
    let mut f = Facts { last_here, window_start, window_capped, ..Default::default() };

    if let Some(store) = src.mission_store {
        match crate::mission::list_missions(store).await {
            Ok(records) => {
                for m in records {
                    let updated = (m.updated_at / 1000) as i64;
                    // A mission only counts as a trigger/routine run when its
                    // id carries the `trg-` prefix the trigger path actually
                    // mints — a user mission whose description happens to
                    // parse like a trigger label must not be mistaken for one.
                    let label =
                        if m.mission_id.starts_with("trg-") { trigger_label(&m.description) } else { None };
                    let gate = m.pending_gate();
                    if let Some(g) = gate {
                        f.mission_gates.push(GateFact {
                            mission_id: m.mission_id.clone(),
                            gate_id: g.gate_id.clone(),
                            reason: g.reason.clone(),
                        });
                    }
                    match (&label, m.state) {
                        (
                            Some(what),
                            crate::mission::MissionState::Completed
                            | crate::mission::MissionState::Failed
                            | crate::mission::MissionState::Cancelled,
                        ) if updated >= window_start =>
                        {
                            // The trigger path ends an unsuccessful turn with
                            // `cancel_mission` (→ `Cancelled`), not `Failed`;
                            // a gate rejection is the other way a trigger
                            // mission ends up `Failed`. Either reads as failed.
                            f.routine_runs.push(RoutineRun {
                                what: what.clone(),
                                at_unix: updated,
                                failed: Some(m.state != crate::mission::MissionState::Completed),
                            });
                        }
                        // A mission with a pending gate is surfaced via
                        // `mission_gates` above, not also here — it belongs
                        // only under "Needs you", not "Coming up" too.
                        (None, s)
                            if gate.is_none()
                                && !m.is_terminal()
                                && s != crate::mission::MissionState::Created =>
                        {
                            f.in_progress.push(m.description.clone());
                        }
                        _ => {}
                    }
                }
            }
            Err(_) => f.source_errors.push("missions".into()),
        }
    }

    // Every run so far came from a `trg-` mission.
    let mission_runs = f.routine_runs.len();
    if let Some(store) = src.schedule_store {
        match crate::schedule::list_schedules(store).await {
            Ok(records) => {
                for r in records.iter().filter(|r| r.enabled) {
                    let name = r.schedule_id.strip_prefix("cfg-").unwrap_or(&r.schedule_id).to_string();
                    if let Some(next) = r.next_fire_time() {
                        f.upcoming.push(UpcomingFact { name: name.clone(), at_unix: next.timestamp() });
                    }
                    // Wrapped runs are already counted from their mission,
                    // except digest routines: `run_digest_report` runs them
                    // without ever creating a mission, even when
                    // `wrap_mission` is set, so they'd otherwise vanish.
                    // Team-mission routines likewise run as a team mission,
                    // never a `trg-` mission record.
                    if (!r.wrap_mission
                        || r.report_kind.as_deref() == Some("digest")
                        || r.team_mission.is_some())
                        && let Some(ms) = r.last_fired_at
                        && (ms / 1000) as i64 >= window_start
                    {
                        let what = format!("routine {name}");
                        let at_unix = (ms / 1000) as i64;
                        // Without a digest builder, a wrapped digest routine
                        // falls back to a `trg-` mission — already counted
                        // above, so don't count the firing twice.
                        let counted = f.routine_runs[..mission_runs]
                            .iter()
                            .any(|m| m.what == what && (m.at_unix - at_unix).abs() <= DUPLICATE_RUN_SECS);
                        if !counted {
                            f.routine_runs.push(RoutineRun { what, at_unix, failed: None });
                        }
                    }
                }
            }
            Err(_) => f.source_errors.push("routines".into()),
        }
    }

    if let Some(log) = src.audit_log {
        // The walk runs at the start of the minute (see `audit_key`), so a
        // cached walk and a fresh one agree; entries appended since then
        // change the chain length and so the key.
        let key = audit_key(log.len() as u64, last_here, now);
        let walk_now = key.minute * 60;
        let walk = || {
            read_since(log, key.window_start.min(walk_now - DAY))
                .map(|entries| audit_facts(&entries, key.window_start, walk_now, src.pricing))
        };
        let walked = match src.audit_cache {
            Some(cache) => cache.get_or_compute(key, walk),
            None => walk(),
        };
        match walked {
            Ok(a) => {
                f.notifications = a.notifications;
                f.changes = a.changes;
                f.memories_saved = a.memories_saved;
                f.spend_24h_usd = Some(a.spend_24h_usd);
                f.spend_untracked = a.spend_untracked;
            }
            Err(_) => f.source_errors.push("the audit trail".into()),
        }
    }

    if let Some(log) = src.persona_proposals {
        for p in log.list(crate::persona_proposal::ProposalStatusFilter::Pending) {
            let is_skill = p.proposed_op.category == crate::persona::PersonaDeltaCategory::LearnedSkill;
            f.proposals.push(ProposalFact {
                id: p.id.clone(),
                category: category_words(&format!("{:?}", p.proposed_op.category)),
                is_skill,
                reason: p.proposed_op.reason.clone(),
            });
        }
    }

    if let Some(svc) = src.team_missions {
        for m in svc.list() {
            if m.phase == aivyx_ipc::team_mission::TeamMissionPhase::AwaitingApproval
                && let Some(step) = &m.pending_gate
            {
                f.team_gates.push(TeamGateFact { mission_id: m.id.clone(), step: step.clone(), goal: m.goal.clone() });
            } else if !m.phase.is_terminal() {
                f.in_progress.push(m.goal.clone());
            }
        }
    }

    if let Some(parker) = src.parked {
        match parker.list().await {
            Ok(steps) => {
                for s in steps {
                    if s.state == aivyx_ipc::parked::ParkedState::Pending {
                        f.parked_pending.push(ParkedFact {
                            id: s.id,
                            summary: s.summary,
                            origin: s.origin,
                            parked_at: s.parked_at,
                            preview: s.preview,
                        });
                    } else if let Some(at) = s.resolved_at.filter(|t| *t >= window_start) {
                        f.parked_resolved.push(ParkedResolvedFact { summary: s.summary, state: s.state, at_unix: at });
                    }
                }
            }
            Err(_) => f.source_errors.push("parked steps".into()),
        }
    }

    if let Some(store) = src.reminders {
        match store.list().await {
            Ok(all) => {
                f.reminders = all
                    .into_iter()
                    .filter(|r| r.due_unix <= now + REMINDER_LOOKAHEAD_SECS)
                    .map(|r| ReminderFact { id: r.id, message: r.message, due_unix: r.due_unix })
                    .collect();
            }
            Err(_) => f.source_errors.push("reminders".into()),
        }
    }

    if let Some(mem) = src.memory {
        match mem.list_topics().await {
            Ok(topics) => {
                f.memory_topics =
                    Some(topics.iter().filter(|t| !crate::prune_sink::is_internal_topic(t)).count() as u64);
            }
            Err(_) => f.source_errors.push("memory".into()),
        }
    }

    f
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_000_000;

    fn base() -> Facts {
        let (window_start, window_capped) = window(Some(NOW - 3_600), NOW);
        Facts { last_here: Some(NOW - 3_600), window_start, window_capped, ..Default::default() }
    }

    #[test]
    fn the_window_falls_back_to_a_day_and_caps_at_a_week() {
        assert_eq!(window(None, NOW), (NOW - 24 * 3600, false));
        assert_eq!(window(Some(NOW - 60), NOW), (NOW - 60, false));
        assert_eq!(window(Some(NOW - 30 * 24 * 3600), NOW), (NOW - 7 * 24 * 3600, true));
    }

    #[test]
    fn an_empty_record_composes_an_empty_briefing() {
        let b = compose(&base(), NOW);
        assert!(b.needs_you.is_empty());
        assert!(b.log.is_empty());
        assert!(b.coming_up.is_empty());
        assert_eq!(b.log_more, 0);
        assert_eq!(b.last_active_unix, Some(NOW - 3_600));
    }

    #[test]
    fn pending_parked_steps_come_after_gates_with_a_preview() {
        let mut f = base();
        f.team_gates = vec![TeamGateFact { mission_id: "t1".into(), step: "deploy".into(), goal: "ship".into() }];
        f.proposals = vec![ProposalFact { id: "p2".into(), category: "communication style".into(), is_skill: false, reason: None }];
        f.parked_pending = vec![ParkedFact {
            id: "ab12cd34".into(),
            summary: "fs.delete old.txt".into(),
            origin: "routine tidy".into(),
            parked_at: NOW - 2 * 3600,
            preview: Some("Now: old.txt (5 bytes)\nhello".into()),
        }];
        let b = compose(&f, NOW);
        let card = &b.needs_you[1];
        assert_eq!(card.sentence, "The routine tidy is waiting for your go-ahead: fs.delete old.txt.");
        assert_eq!(card.detail.as_deref(), Some("Parked 2 h ago."));
        assert_eq!(card.key, "parked:ab12cd34");
        assert_eq!(
            card.action,
            NeedsYouAction::ParkedStep { id: "ab12cd34".into(), preview: Some("Now: old.txt (5 bytes)\nhello".into()) }
        );
        assert_eq!(b.needs_you[2].sentence, "I'd like to update my communication style.");
    }

    #[test]
    fn resolved_parked_steps_are_logged() {
        use aivyx_ipc::parked::ParkedState as S;
        let mut f = base();
        f.parked_resolved = [S::Approved, S::Denied, S::Lapsed, S::Failed]
            .into_iter()
            .enumerate()
            .map(|(i, state)| ParkedResolvedFact { summary: "fs.delete a.txt".into(), state, at_unix: NOW - 100 + i as i64 })
            .collect();
        let b = compose(&f, NOW);
        let lines: Vec<(&str, bool)> = b.log.iter().map(|l| (l.sentence.as_str(), l.warn)).collect();
        assert_eq!(
            lines,
            vec![
                ("You approved a parked step: fs.delete a.txt.", false),
                ("You turned down a parked step: fs.delete a.txt.", false),
                ("A parked step lapsed unreviewed: fs.delete a.txt.", true),
                ("A parked step you approved failed: fs.delete a.txt.", true),
            ]
        );
    }

    #[test]
    fn needs_you_is_approvals_then_proposals_then_problems_then_reminders() {
        let mut f = base();
        f.reminders = vec![ReminderFact { id: "r1".into(), message: "call mom".into(), due_unix: NOW - 5 }];
        f.notifications = vec![
            NotifyFact { target: "telegram".into(), at_unix: NOW - 50, error_kind: Some("auth".into()) },
            NotifyFact { target: "telegram".into(), at_unix: NOW - 40, error_kind: Some("auth".into()) },
        ];
        f.routine_runs = vec![RoutineRun { what: "routine trend-scan".into(), at_unix: NOW - 30, failed: Some(true) }];
        f.proposals = vec![
            ProposalFact { id: "p1".into(), category: "learned skill".into(), is_skill: true, reason: Some("you ask for this often".into()) },
            ProposalFact { id: "p2".into(), category: "communication style".into(), is_skill: false, reason: None },
        ];
        f.team_gates = vec![TeamGateFact { mission_id: "t1".into(), step: "deploy".into(), goal: "ship the note".into() }];
        f.mission_gates = vec![GateFact { mission_id: "m1".into(), gate_id: "g1".into(), reason: "send the weekly digest".into() }];
        f.source_errors = vec!["reminders".into()];

        let b = compose(&f, NOW);
        let sentences: Vec<&str> = b.needs_you.iter().map(|n| n.sentence.as_str()).collect();
        assert_eq!(
            sentences,
            vec![
                "A mission is waiting for your go-ahead: send the weekly digest.",
                "The team is waiting for your go-ahead on \u{201c}ship the note\u{201d} (step deploy).",
                "I'd like to add or refine a skill.",
                "I'd like to update my communication style.",
                "The routine trend-scan failed.",
                "I couldn't send 2 notifications to telegram (auth).",
                "I couldn't read reminders just now.",
                "Reminder: call mom",
            ]
        );
        assert_eq!(b.needs_you[0].action, NeedsYouAction::MissionGate { mission_id: "m1".into(), gate_id: "g1".into() });
        assert_eq!(b.needs_you[0].link, "missions");
        assert_eq!(b.needs_you[1].action, NeedsYouAction::TeamGate { mission_id: "t1".into(), step: "deploy".into() });
        assert_eq!(b.needs_you[1].link, "mission-control");
        assert_eq!(b.needs_you[2].action, NeedsYouAction::Review);
        assert_eq!(b.needs_you[2].detail.as_deref(), Some("you ask for this often"));
        assert_eq!(b.needs_you[2].link, "skills");
        assert_eq!(b.needs_you[3].link, "agents");
        assert_eq!(b.needs_you[4].link, "missions");
        assert_eq!(b.needs_you[5].link, "notifications");
        assert_eq!(b.needs_you[7].action, NeedsYouAction::Reminder { id: "r1".into() });
        assert_eq!(b.needs_you[7].link, "reminders");
        assert_eq!(b.needs_you[7].detail.as_deref(), Some("due now"));
        // Keys are unique.
        let mut keys: Vec<&str> = b.needs_you.iter().map(|n| n.key.as_str()).collect();
        keys.sort();
        keys.dedup();
        assert_eq!(keys.len(), b.needs_you.len());
    }

    #[test]
    fn a_single_failed_notification_reads_naturally() {
        let mut f = base();
        f.notifications = vec![NotifyFact { target: "email".into(), at_unix: NOW - 5, error_kind: Some("transport".into()) }];
        let b = compose(&f, NOW);
        assert_eq!(b.needs_you[0].sentence, "I couldn't send a notification to email (transport).");
    }

    #[test]
    fn the_log_reads_in_the_first_person_oldest_first() {
        let mut f = base();
        f.routine_runs = vec![
            RoutineRun { what: "routine digest".into(), at_unix: NOW - 300, failed: Some(false) },
            RoutineRun { what: "routine trend-scan".into(), at_unix: NOW - 100, failed: Some(true) },
            RoutineRun { what: "routine tidy".into(), at_unix: NOW - 200, failed: None },
        ];
        f.notifications = vec![
            NotifyFact { target: "telegram".into(), at_unix: NOW - 250, error_kind: None },
            NotifyFact { target: "email".into(), at_unix: NOW - 90, error_kind: Some("auth".into()) },
        ];
        f.changes = vec![
            ("fs.write".into(), NOW - 280),
            ("calendar.create".into(), NOW - 150),
            ("fs.write".into(), NOW - 80),
        ];
        f.memories_saved = vec![NOW - 260, NOW - 70];

        let b = compose(&f, NOW);
        let lines: Vec<(i64, &str, bool)> =
            b.log.iter().map(|l| (NOW - l.at_unix, l.sentence.as_str(), l.warn)).collect();
        assert_eq!(
            lines,
            vec![
                (300, "I ran the routine digest.", false),
                (250, "I sent a notification to telegram.", false),
                (200, "I ran the routine tidy.", false),
                (100, "The routine trend-scan failed.", true),
                (90, "A notification to email failed (auth).", true),
                (80, "I made 3 changes: fs.write \u{d7}2, calendar.create \u{d7}1.", false),
                (70, "I saved 2 memories.", false),
            ]
        );
        assert_eq!(b.log[0].link, "schedules");
        assert_eq!(b.log[1].link, "notifications");
        assert_eq!(b.log[5].link, "audit");
        assert_eq!(b.log[6].link, "memory");
    }

    #[test]
    fn singular_and_many_tools_read_naturally() {
        let mut f = base();
        f.changes = vec![
            ("a.x".into(), NOW - 9),
            ("b.x".into(), NOW - 8),
            ("c.x".into(), NOW - 7),
            ("d.x".into(), NOW - 6),
        ];
        f.memories_saved = vec![NOW - 5];
        let b = compose(&f, NOW);
        assert_eq!(b.log[0].sentence, "I made 4 changes: a.x \u{d7}1, b.x \u{d7}1, c.x \u{d7}1, and others.");
        assert_eq!(b.log[1].sentence, "I saved 1 memory.");

        let mut f = base();
        f.changes = vec![("fs.write".into(), NOW - 9)];
        assert_eq!(compose(&f, NOW).log[0].sentence, "I made 1 change: fs.write \u{d7}1.");
    }

    #[test]
    fn the_log_keeps_the_newest_twelve() {
        let mut f = base();
        f.routine_runs = (0..15)
            .map(|i| RoutineRun { what: format!("routine r{i}"), at_unix: NOW - 1_000 + i, failed: Some(false) })
            .collect();
        let b = compose(&f, NOW);
        assert_eq!(b.log.len(), LOG_CAP);
        assert_eq!(b.log_more, 3);
        assert_eq!(b.log[0].sentence, "I ran the routine r3.");
        assert_eq!(b.log[11].sentence, "I ran the routine r14.");
    }

    #[test]
    fn coming_up_is_the_next_three_routines_then_work_in_progress() {
        let mut f = base();
        f.upcoming = vec![
            UpcomingFact { name: "c".into(), at_unix: NOW + 300 },
            UpcomingFact { name: "a".into(), at_unix: NOW + 100 },
            UpcomingFact { name: "d".into(), at_unix: NOW + 400 },
            UpcomingFact { name: "b".into(), at_unix: NOW + 200 },
        ];
        f.in_progress = vec!["ship the note".into()];
        let b = compose(&f, NOW);
        let s: Vec<(Option<i64>, &str)> =
            b.coming_up.iter().map(|u| (u.at_unix.map(|t| t - NOW), u.sentence.as_str())).collect();
        assert_eq!(
            s,
            vec![
                (Some(100), "a"),
                (Some(200), "b"),
                (Some(300), "c"),
                (None, "Working on \u{201c}ship the note\u{201d}."),
            ]
        );
        assert_eq!(b.coming_up[0].link, "schedules");
        assert_eq!(b.coming_up[3].link, "mission-control");
    }

    #[test]
    fn reminders_coming_up_read_soonest_first_with_a_relative_time() {
        let mut f = base();
        f.reminders = vec![
            ReminderFact { id: "b".into(), message: "water plants".into(), due_unix: NOW + 70 * 60 },
            ReminderFact { id: "c".into(), message: "standup".into(), due_unix: NOW + 3_600 },
            ReminderFact { id: "a".into(), message: "call mom".into(), due_unix: NOW + 25 * 60 },
            ReminderFact { id: "d".into(), message: "kettle".into(), due_unix: NOW + 30 },
            ReminderFact { id: "e".into(), message: "late".into(), due_unix: NOW - 90 },
            ReminderFact { id: "f".into(), message: "on the dot".into(), due_unix: NOW },
        ];
        let b = compose(&f, NOW);
        let got: Vec<(&str, Option<&str>)> =
            b.needs_you.iter().map(|n| (n.sentence.as_str(), n.detail.as_deref())).collect();
        assert_eq!(
            got,
            vec![
                ("Reminder: late", Some("due now")),
                ("Reminder: on the dot", Some("due now")),
                ("Reminder: kettle", Some("due in 1 min")),
                ("Reminder: call mom", Some("due in 25 min")),
                ("Reminder: standup", Some("due in 1 h")),
                ("Reminder: water plants", Some("due in 1 h 10 min")),
            ]
        );
        assert_eq!(b.needs_you[3].action, NeedsYouAction::Reminder { id: "a".into() });
        assert_eq!(b.needs_you[3].key, "reminder:a");
    }

    #[test]
    fn repeated_failures_of_one_routine_are_one_card() {
        let mut f = base();
        f.routine_runs = vec![
            RoutineRun { what: "routine scan".into(), at_unix: NOW - 500, failed: Some(true) },
            RoutineRun { what: "routine digest".into(), at_unix: NOW - 400, failed: Some(true) },
            RoutineRun { what: "routine scan".into(), at_unix: NOW - 300, failed: Some(true) },
            RoutineRun { what: "routine scan".into(), at_unix: NOW - 250, failed: Some(false) },
            RoutineRun { what: "routine scan".into(), at_unix: NOW - 200, failed: Some(true) },
            RoutineRun { what: "routine scan".into(), at_unix: NOW - 150, failed: Some(true) },
            RoutineRun { what: "routine scan".into(), at_unix: NOW - 100, failed: Some(true) },
        ];
        let b = compose(&f, NOW);
        let got: Vec<(&str, &str)> = b.needs_you.iter().map(|n| (n.key.as_str(), n.sentence.as_str())).collect();
        assert_eq!(
            got,
            vec![
                ("failed:routine scan", "The routine scan failed 5 times."),
                ("failed:routine digest", "The routine digest failed."),
            ]
        );
        assert!(b.needs_you.iter().all(|n| n.action == NeedsYouAction::Look && n.link == "missions"));
        // The log keeps every run on its own line.
        assert_eq!(b.log.len(), 7);
        assert_eq!(b.log.iter().filter(|l| l.warn).count(), 6);
    }

    #[test]
    fn coming_up_shows_at_most_three_pieces_of_work() {
        let mut f = base();
        f.in_progress = (1..=5).map(|i| format!("job {i}")).collect();
        let b = compose(&f, NOW);
        let s: Vec<(Option<i64>, &str, &str)> =
            b.coming_up.iter().map(|u| (u.at_unix, u.sentence.as_str(), u.link.as_str())).collect();
        assert_eq!(
            s,
            vec![
                (None, "Working on \u{201c}job 1\u{201d}.", "mission-control"),
                (None, "Working on \u{201c}job 2\u{201d}.", "mission-control"),
                (None, "Working on \u{201c}job 3\u{201d}.", "mission-control"),
                (None, "and 2 more in progress.", "mission-control"),
            ]
        );

        let mut f = base();
        f.in_progress = (1..=3).map(|i| format!("job {i}")).collect();
        assert_eq!(compose(&f, NOW).coming_up.len(), 3);
    }

    #[test]
    fn instruments_and_window_pass_through() {
        let mut f = base();
        f.spend_24h_usd = Some(0.42);
        f.spend_untracked = true;
        f.memory_topics = Some(12);
        let b = compose(&f, NOW);
        assert_eq!(b.spend_24h_usd, Some(0.42));
        assert!(b.spend_untracked);
        assert_eq!(b.memory_topics, Some(12));
        assert_eq!(b.window_start_unix, NOW - 3_600);
        assert!(!b.window_capped);
    }
}

#[cfg(test)]
mod gather_tests {
    use super::*;
    use std::sync::Arc;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use aivyx_audit::{AuditEvent, AutoNotifyOutcomeSummary, SignedEntry, TriggerKindSummary};
    use aivyx_audit::AuditWriter;
    use aivyx_storage::{KeyDomain, Storage};

    const NOW: i64 = 2_000_000_000;

    /// A fresh encrypted store in its own temp dir (the caller removes it).
    async fn temp_storage() -> (std::path::PathBuf, Arc<dyn Storage>) {
        use aivyx_crypto::MasterKey;
        use aivyx_storage::{RedbStorage, StorageConfig};
        let dir = std::env::temp_dir().join(format!("aivyx-briefing-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let storage: Arc<dyn Storage> =
            RedbStorage::open(StorageConfig::new(dir.join("store.redb")), MasterKey::from_raw([3u8; 32]))
                .await
                .unwrap();
        (dir, storage)
    }

    fn at(unix: i64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(unix as u64)
    }

    fn entry(seq: u64, unix: i64, event: AuditEvent) -> SignedEntry {
        SignedEntry { seq, appended_at: at(unix), event, mac: [0u8; 32], prev_mac: [0u8; 32] }
    }

    fn tool(scope: &str, outcome: aivyx_core::ToolOutcomeSummary) -> AuditEvent {
        AuditEvent::ToolCall {
            turn_id: aivyx_core::TurnId::new(),
            tool_id: aivyx_core::ToolId::new(),
            scope_used: aivyx_capability::Scope::parse(scope).unwrap(),
            input_hash: [0u8; 32],
            outcome,
            duration: Duration::from_millis(1),
            auto_corrected_from: None,
            extracted_from_text: None,
        }
    }

    fn notify(target: &str, outcome: AutoNotifyOutcomeSummary, unix: i64) -> AuditEvent {
        AuditEvent::AutoNotifyDispatched {
            session_id: aivyx_core::SessionId::new(),
            trigger_kind: TriggerKindSummary::Cron,
            trigger_id: "cfg-digest".into(),
            target_name: target.into(),
            outcome,
            dispatched_at_unix_ms: (unix * 1000) as u64,
        }
    }

    fn chain(n: u64, first_unix: i64) -> Vec<SignedEntry> {
        (0..n)
            .map(|i| {
                entry(i, first_unix + i as i64, AuditEvent::MemoryAccess {
                    turn_id: aivyx_core::TurnId::new(),
                    operation: aivyx_audit::MemoryOperation::Write,
                    scope: aivyx_capability::Scope::parse("memory.write").unwrap(),
                    query_or_key: "k".into(),
                })
            })
            .collect()
    }

    #[test]
    fn paging_back_reads_only_what_it_needs_oldest_first() {
        let all = chain(1_300, 10_000);
        let mut fetches = Vec::new();
        let got = page_back(
            all.len() as u64,
            |from, n| {
                fetches.push((from, n));
                Ok(all[from as usize..from as usize + n].to_vec())
            },
            10_000 + 200,
        )
        .unwrap();
        let seqs: Vec<u64> = got.iter().map(|e| e.seq).collect();
        assert_eq!(seqs, (200..1_300).collect::<Vec<u64>>());
        // Three pages back from the end: 788..1300, 276..788, 0..276.
        assert_eq!(fetches, vec![(788, 512), (276, 512), (0, 276)]);

        // A window inside the newest page stops after one fetch.
        let mut fetches = 0;
        let got = page_back(
            all.len() as u64,
            |from, n| {
                fetches += 1;
                Ok(all[from as usize..from as usize + n].to_vec())
            },
            10_000 + 1_290,
        )
        .unwrap();
        assert_eq!(got.len(), 10);
        assert_eq!(fetches, 1);

        // An empty chain never fetches.
        let got = page_back(0, |_, _| panic!("no fetch on an empty chain"), 0).unwrap();
        assert!(got.is_empty());

        // A fetch error surfaces.
        assert_eq!(page_back(5, |_, _| Err("boom".into()), 0).unwrap_err(), "boom");
    }

    #[test]
    fn the_audit_cache_computes_once_per_key() {
        let cache = AuditFactsCache::default();
        let mut calls = 0;
        let mut get = |key: AuditKey, n: i64| {
            cache
                .get_or_compute(key, || -> Result<AuditFacts, String> {
                    calls += 1;
                    Ok(AuditFacts { memories_saved: vec![n], ..Default::default() })
                })
                .unwrap()
        };
        let k = AuditKey { chain_len: 10, window_start: 100, minute: 7 };
        assert_eq!(get(k, 1).memories_saved, vec![1]);
        // Same key: served from the cache (the compute isn't called again).
        assert_eq!(get(k, 2).memories_saved, vec![1]);
        // Any key part changing recomputes.
        assert_eq!(get(AuditKey { chain_len: 11, ..k }, 3).memories_saved, vec![3]);
        assert_eq!(get(AuditKey { chain_len: 11, window_start: 90, ..k }, 4).memories_saved, vec![4]);
        assert_eq!(get(AuditKey { chain_len: 11, window_start: 90, minute: 8 }, 5).memories_saved, vec![5]);
        assert_eq!(get(AuditKey { chain_len: 11, window_start: 90, minute: 8 }, 6).memories_saved, vec![5]);
        assert_eq!(calls, 4);

        // A failed compute isn't cached.
        let cache = AuditFactsCache::default();
        assert!(cache.get_or_compute(k, || Err::<AuditFacts, _>("boom")).is_err());
        assert_eq!(cache.get_or_compute(k, || Ok::<_, &str>(AuditFacts::default())).unwrap(), AuditFacts::default());
    }

    /// Within one minute, a briefing with no last visit keeps the same
    /// audit window — so repeated polls hit the cache.
    #[test]
    fn the_audit_key_is_stable_within_a_minute() {
        let base = 60 * 1_000_000;
        assert_eq!(audit_key(5, None, base + 1), audit_key(5, None, base + 59));
        assert_ne!(audit_key(5, None, base + 59), audit_key(5, None, base + 60));
        assert_eq!(audit_key(5, Some(base - 600), base + 1), audit_key(5, Some(base - 600), base + 30));
        assert_eq!(audit_key(5, Some(base - 600), base).window_start, base - 600);
        assert_ne!(audit_key(5, None, base), audit_key(6, None, base));
    }

    /// `gather` reads the audit facts through the cache when it has one.
    #[tokio::test]
    async fn gather_serves_audit_facts_from_the_cache() {
        let (dir, storage) = temp_storage().await;
        let log = PersistentAuditLog::open(storage, [9u8; 32]).await.unwrap();
        log.append(AuditEvent::MemoryAccess {
            turn_id: aivyx_core::TurnId::new(),
            operation: aivyx_audit::MemoryOperation::Write,
            scope: aivyx_capability::Scope::parse("memory.write").unwrap(),
            query_or_key: "k".into(),
        })
        .unwrap();
        let now = crate::activity::now_unix();
        let pricing = aivyx_cost::Pricing::new();
        let cache = AuditFactsCache::default();
        // Seed the key this gather will use with a marker value.
        let marker = AuditFacts { memories_saved: vec![42], ..Default::default() };
        cache.get_or_compute(audit_key(log.len() as u64, None, now), || Ok::<_, String>(marker)).unwrap();
        let mut src = BriefingSources {
            mission_store: None,
            schedule_store: None,
            audit_log: Some(&log),
            persona_proposals: None,
            team_missions: None,
            parked: None,
            reminders: None,
            memory: None,
            pricing: &pricing,
            audit_cache: Some(&cache),
        };
        let f = gather(&src, None, now).await;
        assert_eq!(f.memories_saved, vec![42]);
        // Without the cache, the chain is read.
        src.audit_cache = None;
        let f = gather(&src, None, now).await;
        assert_eq!(f.memories_saved.len(), 1);
        assert_ne!(f.memories_saved, vec![42]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn trigger_missions_name_their_routine() {
        assert_eq!(trigger_label("cron trigger cfg-digest: summarise"), Some("routine digest".into()));
        assert_eq!(trigger_label("webhook trigger deploy: go"), Some("webhook trigger deploy".into()));
        assert_eq!(trigger_label("write the report"), None);
    }

    #[test]
    fn audit_facts_keep_the_window_and_skip_reads() {
        let completed = aivyx_core::ToolOutcomeSummary::Completed {
            verified: aivyx_core::VerificationSummary::Verified,
        };
        let start = NOW - 3_600;
        let entries = vec![
            // Before the window: ignored for the log, but inside 24 h → priced.
            entry(0, start - 10, tool("fs.write", completed.clone())),
            entry(1, start - 10, AuditEvent::LlmCost {
                turn_id: aivyx_core::TurnId::new(),
                model: "claude-sonnet-5".into(),
                usage: aivyx_core::TokenUsage { input_tokens: 1_000_000, ..Default::default() },
            }),
            entry(2, start + 10, tool("fs.write", completed.clone())),
            entry(3, start + 20, tool("fs.read", completed.clone())),
            entry(4, start + 30, tool("fs.delete", aivyx_core::ToolOutcomeSummary::Denied)),
            entry(4, start + 35, tool("memory.write", completed.clone())),
            // The assistant's own notebook and MCP calls aren't changes.
            entry(4, start + 36, tool("workspace", completed.clone())),
            entry(4, start + 37, tool("mcp.call:srv:tool", completed)),
            entry(5, start + 40, AuditEvent::MemoryAccess {
                turn_id: aivyx_core::TurnId::new(),
                operation: aivyx_audit::MemoryOperation::Write,
                scope: aivyx_capability::Scope::parse("memory.write").unwrap(),
                query_or_key: "k".into(),
            }),
            entry(6, start + 50, notify("telegram", AutoNotifyOutcomeSummary::Delivered, start + 50)),
            entry(7, start + 60, notify(
                "email",
                AutoNotifyOutcomeSummary::Failed { error_kind: "auth".into(), error_message: "bad".into() },
                start + 60,
            )),
        ];
        let pricing = aivyx_cost::Pricing::new();
        let a = audit_facts(&entries, start, NOW, &pricing);
        assert_eq!(a.changes, vec![("fs.write".to_string(), start + 10)]);
        assert_eq!(a.memories_saved, vec![start + 40]);
        assert_eq!(
            a.notifications,
            vec![
                NotifyFact { target: "telegram".into(), at_unix: start + 50, error_kind: None },
                NotifyFact { target: "email".into(), at_unix: start + 60, error_kind: Some("auth".into()) },
            ]
        );
        let expected = pricing
            .cost_of("claude-sonnet-5", &aivyx_cost::TokenCounts { input: 1_000_000, ..Default::default() })
            .usd;
        assert!((a.spend_24h_usd - expected).abs() < 1e-9);
        assert!(!a.spend_untracked, "every model in this fixture is priced");
    }

    /// An `LlmCost` entry for a model with no known rate flags the spend as
    /// a lower bound.
    #[test]
    fn audit_facts_flags_unpriced_models_as_untracked() {
        let pricing = aivyx_cost::Pricing::new();
        let entries = vec![entry(0, NOW - 10, AuditEvent::LlmCost {
            turn_id: aivyx_core::TurnId::new(),
            model: "some-mystery-model".into(),
            usage: aivyx_core::TokenUsage { input_tokens: 1_000, ..Default::default() },
        })];
        let a = audit_facts(&entries, NOW - 3_600, NOW, &pricing);
        assert_eq!(a.spend_24h_usd, 0.0);
        assert!(a.spend_untracked);
    }

    /// A genuinely free local model is `priced = true, usd = 0` — it must
    /// NOT set `spend_untracked` (it isn't a lower bound, it's exact).
    #[test]
    fn audit_facts_does_not_flag_a_free_local_model() {
        assert!(aivyx_cost::is_local_model("llama3.1"));
        let pricing = aivyx_cost::Pricing::new();
        let entries = vec![entry(0, NOW - 10, AuditEvent::LlmCost {
            turn_id: aivyx_core::TurnId::new(),
            model: "llama3.1".into(),
            usage: aivyx_core::TokenUsage { input_tokens: 1_000, ..Default::default() },
        })];
        let a = audit_facts(&entries, NOW - 3_600, NOW, &pricing);
        assert_eq!(a.spend_24h_usd, 0.0);
        assert!(!a.spend_untracked);
    }

    #[tokio::test]
    async fn gather_reads_missions_schedules_and_reminders() {
        let (dir, storage) = temp_storage().await;
        let now = crate::activity::now_unix();
        let missions = storage.domain(KeyDomain::Missions);

        // The real trigger path ends an unsuccessful turn with
        // `mission::cancel_mission` → `Cancelled`, never `Failed` directly;
        // a gate rejection is the other way a trigger mission ends up
        // `Failed`. Cover both.
        let mut cancelled = crate::mission::MissionRecord::new(
            "trg-1".into(),
            "default".into(),
            "cron trigger cfg-trend-scan: scan".into(),
        );
        cancelled.state = crate::mission::MissionState::Cancelled;
        crate::mission::create_mission(&missions, &cancelled).await.unwrap();

        let mut failed = crate::mission::MissionRecord::new(
            "trg-2".into(),
            "default".into(),
            "cron trigger cfg-nightly: run".into(),
        );
        failed.state = crate::mission::MissionState::Failed;
        crate::mission::create_mission(&missions, &failed).await.unwrap();

        let mut gated = crate::mission::MissionRecord::new("m-2".into(), "default".into(), "write the report".into());
        gated.state = crate::mission::MissionState::GatePending;
        gated.gates.push(crate::mission::GateRecord {
            gate_id: "g1".into(),
            reason: "send the weekly digest".into(),
            scope: None,
            state: crate::mission::GateState::Pending,
            created_at: 0,
            resolved_at: None,
        });
        crate::mission::create_mission(&missions, &gated).await.unwrap();

        // A plain running mission: not a trigger, not gated — must still
        // show up as in-progress work.
        let mut running = crate::mission::MissionRecord::new(
            "m-3".into(),
            "default".into(),
            "update the household budget".into(),
        );
        running.state = crate::mission::MissionState::Running;
        crate::mission::create_mission(&missions, &running).await.unwrap();

        let reminders: crate::reminder_tool::SharedReminderStore =
            Arc::new(crate::reminder_store::ReminderStore::new(storage.domain(KeyDomain::Reminders)));
        reminders
            .set(&crate::reminder_store::Reminder {
                id: "due".into(),
                due_unix: now - 10,
                message: "call mom".into(),
                notify_targets: vec![],
                created_unix: 0,
            })
            .await
            .unwrap();
        for (id, due_unix) in [("soon", now + 25 * 60), ("edge", now + 2 * 3_600), ("later", now + 2 * 3_600 + 1)] {
            reminders
                .set(&crate::reminder_store::Reminder {
                    id: id.into(),
                    due_unix,
                    message: id.into(),
                    notify_targets: vec![],
                    created_unix: 0,
                })
                .await
                .unwrap();
        }

        let pricing = aivyx_cost::Pricing::new();
        let src = BriefingSources {
            mission_store: Some(&missions),
            schedule_store: None,
            audit_log: None,
            persona_proposals: None,
            team_missions: None,
            parked: None,
            reminders: Some(&reminders),
            memory: None,
            pricing: &pricing,
            audit_cache: None,
        };
        let f = gather(&src, Some(now - 3_600), now).await;
        assert_eq!(f.routine_runs.len(), 2);
        let mut runs = f.routine_runs.clone();
        runs.sort_by(|a, b| a.what.cmp(&b.what));
        assert_eq!(runs[0].what, "routine nightly");
        assert_eq!(runs[0].failed, Some(true));
        assert_eq!(runs[1].what, "routine trend-scan");
        assert_eq!(runs[1].failed, Some(true));
        assert_eq!(
            f.mission_gates,
            vec![GateFact { mission_id: "m-2".into(), gate_id: "g1".into(), reason: "send the weekly digest".into() }]
        );
        // The gated mission ("write the report") must not also appear as
        // in-progress work — only the plain running one does.
        assert_eq!(f.in_progress, vec!["update the household budget".to_string()]);
        // Reminders coming up in the next 2 hours (plus any already due
        // the driver hasn't fired yet), soonest first.
        assert_eq!(
            f.reminders,
            vec![
                ReminderFact { id: "due".into(), message: "call mom".into(), due_unix: now - 10 },
                ReminderFact { id: "soon".into(), message: "soon".into(), due_unix: now + 25 * 60 },
                ReminderFact { id: "edge".into(), message: "edge".into(), due_unix: now + 2 * 3_600 },
            ]
        );
        assert!(f.source_errors.is_empty());
        assert_eq!(f.spend_24h_usd, None);
        assert_eq!(f.memory_topics, None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A mission is only ever treated as a trigger/routine run when its id
    /// has the `trg-` prefix the trigger path actually mints — not merely
    /// because its (user-supplied) description happens to parse like a
    /// trigger label.
    #[tokio::test]
    async fn a_user_mission_that_merely_mentions_trigger_is_not_mistaken_for_a_routine() {
        let (dir, storage) = temp_storage().await;
        let now = crate::activity::now_unix();
        let missions = storage.domain(KeyDomain::Missions);

        let mut running = crate::mission::MissionRecord::new(
            "m-1".into(),
            "default".into(),
            "fix the webhook trigger handler: urgent".into(),
        );
        running.state = crate::mission::MissionState::Running;
        crate::mission::create_mission(&missions, &running).await.unwrap();

        let pricing = aivyx_cost::Pricing::new();
        let src = BriefingSources {
            mission_store: Some(&missions),
            schedule_store: None,
            audit_log: None,
            persona_proposals: None,
            team_missions: None,
            parked: None,
            reminders: None,
            memory: None,
            pricing: &pricing,
            audit_cache: None,
        };
        let f = gather(&src, Some(now - 3_600), now).await;
        assert!(f.routine_runs.is_empty());
        assert_eq!(f.in_progress, vec!["fix the webhook trigger handler: urgent".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A `report_kind = "digest"` routine runs via `run_digest_report`
    /// without ever creating a mission, even when `wrap_mission` is true —
    /// so its firing must be counted straight from the schedule record, not
    /// skipped on the assumption a mission will cover it.
    #[tokio::test]
    async fn fired_digest_and_team_schedules_are_counted_even_when_wrap_mission_is_set() {
        let (dir, storage) = temp_storage().await;
        let now = crate::activity::now_unix();
        let schedules = storage.domain(KeyDomain::Schedules);

        let mut digest = crate::schedule::ScheduleRecord::new(
            "cfg-digest".into(),
            "0 0 3 * * *".into(),
            "default".into(),
            "send digest".into(),
        )
        .unwrap();
        digest.wrap_mission = true;
        digest.report_kind = Some("digest".into());
        digest.last_fired_at = Some(((now - 100) * 1000) as u64);
        crate::schedule::create_schedule(&schedules, &digest).await.unwrap();

        let mut standup = crate::schedule::ScheduleRecord::new(
            "cfg-standup".into(),
            "0 0 9 * * *".into(),
            "default".into(),
            "standup".into(),
        )
        .unwrap();
        standup.last_fired_at = None;
        crate::schedule::create_schedule(&schedules, &standup).await.unwrap();

        // A team-mission routine runs as a team mission, never a `trg-`
        // mission record, so it too is counted from the schedule — even
        // with `wrap_mission` set.
        let mut team = crate::schedule::ScheduleRecord::new_team_mission(
            "cfg-prep".into(),
            "0 0 6 * * *".into(),
            "prep the line".into(),
            None,
        )
        .unwrap();
        team.wrap_mission = true;
        team.last_fired_at = Some(((now - 50) * 1000) as u64);
        crate::schedule::create_schedule(&schedules, &team).await.unwrap();

        // An ordinary wrapped routine is counted from its mission, not here.
        let mut wrapped = crate::schedule::ScheduleRecord::new(
            "cfg-wrapped".into(),
            "0 0 7 * * *".into(),
            "default".into(),
            "wrapped".into(),
        )
        .unwrap();
        wrapped.wrap_mission = true;
        wrapped.last_fired_at = Some(((now - 60) * 1000) as u64);
        crate::schedule::create_schedule(&schedules, &wrapped).await.unwrap();

        let pricing = aivyx_cost::Pricing::new();
        let src = BriefingSources {
            mission_store: None,
            schedule_store: Some(&schedules),
            audit_log: None,
            persona_proposals: None,
            team_missions: None,
            parked: None,
            reminders: None,
            memory: None,
            pricing: &pricing,
            audit_cache: None,
        };
        let f = gather(&src, Some(now - 3_600), now).await;
        let mut runs = f.routine_runs.clone();
        runs.sort_by(|a, b| a.what.cmp(&b.what));
        assert_eq!(
            runs,
            vec![
                RoutineRun { what: "routine digest".into(), at_unix: now - 100, failed: None },
                RoutineRun { what: "routine prep".into(), at_unix: now - 50, failed: None },
            ]
        );
        // Both schedules are enabled, so both get an upcoming entry from
        // their own future cron fire; the point under test is that the
        // plain (non-digest) schedule produces one too.
        assert!(f.upcoming.iter().any(|u| u.name == "standup"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Without a digest builder, a wrapped digest routine falls back to a
    /// `trg-` mission — which is counted from the mission, so the schedule's
    /// `last_fired_at` must not count it a second time. A digest run with
    /// no matching mission nearby is still counted from the schedule.
    #[tokio::test]
    async fn a_digest_run_that_fell_back_to_a_mission_is_counted_once() {
        async fn runs(mission_at: i64, fired_at: i64, now: i64) -> Vec<RoutineRun> {
            let (dir, storage) = temp_storage().await;
            let missions = storage.domain(KeyDomain::Missions);
            let schedules = storage.domain(KeyDomain::Schedules);
            let mut m = crate::mission::MissionRecord::new(
                "trg-9".into(),
                "default".into(),
                "cron trigger cfg-digest: send digest".into(),
            );
            m.state = crate::mission::MissionState::Completed;
            m.updated_at = (mission_at * 1000) as u64;
            crate::mission::create_mission(&missions, &m).await.unwrap();
            let mut digest = crate::schedule::ScheduleRecord::new(
                "cfg-digest".into(),
                "0 0 3 * * *".into(),
                "default".into(),
                "send digest".into(),
            )
            .unwrap();
            digest.wrap_mission = true;
            digest.report_kind = Some("digest".into());
            digest.last_fired_at = Some((fired_at * 1000) as u64);
            crate::schedule::create_schedule(&schedules, &digest).await.unwrap();
            let pricing = aivyx_cost::Pricing::new();
            let src = BriefingSources {
                mission_store: Some(&missions),
                schedule_store: Some(&schedules),
                audit_log: None,
                persona_proposals: None,
                team_missions: None,
                parked: None,
                reminders: None,
                memory: None,
                pricing: &pricing,
                audit_cache: None,
            };
            let f = gather(&src, Some(now - 3_600), now).await;
            let _ = std::fs::remove_dir_all(&dir);
            f.routine_runs
        }
        let now = crate::activity::now_unix();
        // The mission completes 90 s after the schedule fired: one run.
        assert_eq!(
            runs(now - 100, now - 190, now).await,
            vec![RoutineRun { what: "routine digest".into(), at_unix: now - 100, failed: Some(false) }]
        );
        // A mission from an earlier, unrelated firing doesn't hide this one.
        let got = runs(now - 1_000, now - 190, now).await;
        assert_eq!(
            got,
            vec![
                RoutineRun { what: "routine digest".into(), at_unix: now - 1_000, failed: Some(false) },
                RoutineRun { what: "routine digest".into(), at_unix: now - 190, failed: None },
            ]
        );
    }
}
