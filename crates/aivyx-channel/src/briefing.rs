//! The Command Center briefing: [`gather`] reads what the record already
//! holds into [`Facts`]; [`compose`] (pure) writes every sentence. No model
//! is involved — the wording is fixed templates, in the assistant's voice.

use aivyx_ipc::briefing::{Briefing, LogEntry, NeedsYouAction, NeedsYouItem, UpcomingItem};

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
