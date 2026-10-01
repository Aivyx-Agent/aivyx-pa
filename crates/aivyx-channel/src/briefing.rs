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
    pub proposals: Vec<ProposalFact>,
    pub due_reminders: Vec<ReminderFact>,
    pub upcoming: Vec<UpcomingFact>,
    /// Goals of work in progress.
    pub in_progress: Vec<String>,
    pub spend_24h_usd: Option<f64>,
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

/// Pure: facts in, the briefing out.
pub fn compose(f: &Facts, _now: i64) -> Briefing {
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
    // Proposals.
    for p in &f.proposals {
        let sentence = if p.is_skill {
            "I'd like to add or refine a skill.".to_string()
        } else {
            format!("I'd like to update my {}.", p.category)
        };
        needs.push(item(format!("proposal:{}", p.id), sentence, p.reason.clone(), NeedsYouAction::Review, "agents"));
    }
    // Went wrong.
    for r in f.routine_runs.iter().filter(|r| r.failed == Some(true)) {
        needs.push(item(
            format!("failed:{}:{}", r.what, r.at_unix),
            format!("The {} failed.", r.what),
            None,
            NeedsYouAction::Look,
            "missions",
        ));
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
    // Due reminders.
    for r in &f.due_reminders {
        needs.push(item(
            format!("reminder:{}", r.id),
            format!("Reminder: {}", r.message),
            None,
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
    coming_up.extend(f.in_progress.iter().map(|goal| UpcomingItem {
        at_unix: None,
        sentence: format!("Working on \u{201c}{goal}\u{201d}."),
        link: "mission-control".into(),
    }));

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
    pub reminders: Option<&'a crate::reminder_tool::SharedReminderStore>,
    pub memory: Option<&'a Arc<dyn aivyx_memory::Memory>>,
    pub pricing: &'a aivyx_cost::Pricing,
}

/// What the audit chain contributes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AuditFacts {
    pub notifications: Vec<NotifyFact>,
    pub changes: Vec<(String, i64)>,
    pub memories_saved: Vec<i64>,
    pub spend_24h_usd: f64,
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
            a.spend_24h_usd += pricing.cost_of(model, &counts).usd;
        }
        if t < window_start {
            continue;
        }
        match &e.event {
            AuditEvent::ToolCall { scope_used, outcome: aivyx_core::ToolOutcomeSummary::Completed { .. }, .. }
                // Memory writes get their own line ("I saved N memories").
                if !aivyx_capability::is_read_only_base(scope_used.base())
                    && !scope_used.base().starts_with("memory.") =>
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
    const PAGE: u64 = 512;
    let mut end = log.len() as u64;
    let mut out: Vec<SignedEntry> = Vec::new();
    while end > 0 {
        let from = end.saturating_sub(PAGE);
        let page = log.entries_range(from, (end - from) as usize).map_err(|e| e.to_string())?;
        let reached = page.first().is_some_and(|e| unix_of(e.appended_at) < since);
        out.splice(0..0, page);
        if reached {
            break;
        }
        end = from;
    }
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
                    if (!r.wrap_mission || r.report_kind.as_deref() == Some("digest"))
                        && let Some(ms) = r.last_fired_at
                        && (ms / 1000) as i64 >= window_start
                    {
                        f.routine_runs.push(RoutineRun {
                            what: format!("routine {name}"),
                            at_unix: (ms / 1000) as i64,
                            failed: None,
                        });
                    }
                }
            }
            Err(_) => f.source_errors.push("routines".into()),
        }
    }

    if let Some(log) = src.audit_log {
        match read_since(log, window_start.min(now - DAY)) {
            Ok(entries) => {
                let a = audit_facts(&entries, window_start, now, src.pricing);
                f.notifications = a.notifications;
                f.changes = a.changes;
                f.memories_saved = a.memories_saved;
                f.spend_24h_usd = Some(a.spend_24h_usd);
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

    if let Some(store) = src.reminders {
        match store.due(now).await {
            Ok(due) => {
                f.due_reminders = due
                    .into_iter()
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
    fn needs_you_is_approvals_then_proposals_then_problems_then_reminders() {
        let mut f = base();
        f.due_reminders = vec![ReminderFact { id: "r1".into(), message: "call mom".into(), due_unix: NOW - 5 }];
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
        assert_eq!(b.needs_you[2].link, "agents");
        assert_eq!(b.needs_you[4].link, "missions");
        assert_eq!(b.needs_you[5].link, "notifications");
        assert_eq!(b.needs_you[7].action, NeedsYouAction::Reminder { id: "r1".into() });
        assert_eq!(b.needs_you[7].link, "reminders");
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
    fn instruments_and_window_pass_through() {
        let mut f = base();
        f.spend_24h_usd = Some(0.42);
        f.memory_topics = Some(12);
        let b = compose(&f, NOW);
        assert_eq!(b.spend_24h_usd, Some(0.42));
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

    const NOW: i64 = 2_000_000_000;

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
            entry(4, start + 35, tool("memory.write", completed)),
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
    }

    #[tokio::test]
    async fn gather_reads_missions_schedules_and_reminders() {
        use aivyx_crypto::MasterKey;
        use aivyx_storage::{KeyDomain, RedbStorage, Storage, StorageConfig};
        let dir = std::env::temp_dir().join(format!("aivyx-briefing-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let storage: Arc<dyn Storage> =
            RedbStorage::open(StorageConfig::new(dir.join("store.redb")), MasterKey::from_raw([3u8; 32]))
                .await
                .unwrap();
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
        reminders
            .set(&crate::reminder_store::Reminder {
                id: "later".into(),
                due_unix: now + 9_999,
                message: "later".into(),
                notify_targets: vec![],
                created_unix: 0,
            })
            .await
            .unwrap();

        let pricing = aivyx_cost::Pricing::new();
        let src = BriefingSources {
            mission_store: Some(&missions),
            schedule_store: None,
            audit_log: None,
            persona_proposals: None,
            team_missions: None,
            reminders: Some(&reminders),
            memory: None,
            pricing: &pricing,
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
        assert_eq!(
            f.due_reminders,
            vec![ReminderFact { id: "due".into(), message: "call mom".into(), due_unix: now - 10 }]
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
        use aivyx_crypto::MasterKey;
        use aivyx_storage::{KeyDomain, RedbStorage, Storage, StorageConfig};
        let dir = std::env::temp_dir().join(format!("aivyx-briefing-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let storage: Arc<dyn Storage> =
            RedbStorage::open(StorageConfig::new(dir.join("store.redb")), MasterKey::from_raw([3u8; 32]))
                .await
                .unwrap();
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
            reminders: None,
            memory: None,
            pricing: &pricing,
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
    async fn a_fired_digest_schedule_is_counted_even_when_wrap_mission_is_set() {
        use aivyx_crypto::MasterKey;
        use aivyx_storage::{KeyDomain, RedbStorage, Storage, StorageConfig};
        let dir = std::env::temp_dir().join(format!("aivyx-briefing-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let storage: Arc<dyn Storage> =
            RedbStorage::open(StorageConfig::new(dir.join("store.redb")), MasterKey::from_raw([3u8; 32]))
                .await
                .unwrap();
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

        let pricing = aivyx_cost::Pricing::new();
        let src = BriefingSources {
            mission_store: None,
            schedule_store: Some(&schedules),
            audit_log: None,
            persona_proposals: None,
            team_missions: None,
            reminders: None,
            memory: None,
            pricing: &pricing,
        };
        let f = gather(&src, Some(now - 3_600), now).await;
        assert_eq!(
            f.routine_runs,
            vec![RoutineRun { what: "routine digest".into(), at_unix: now - 100, failed: None }]
        );
        // Both schedules are enabled, so both get an upcoming entry from
        // their own future cron fire; the point under test is that the
        // plain (non-digest) schedule produces one too.
        assert!(f.upcoming.iter().any(|u| u.name == "standup"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
