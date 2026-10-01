//! The Command Center's briefing — what needs the operator, what the
//! assistant did since they were last here, and what's coming up. Composed
//! by the daemon from the record (never by a model); the Studio only
//! renders it. Wasm-clean: plain data, no I/O.

use serde::{Deserialize, Serialize};

/// Answer to [`crate::protocol::QueryPayload::GetBriefing`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Briefing {
    /// The end of the operator's previous visit — what "last here" shows.
    /// `None` until the operator has acted at least once before this visit.
    pub last_active_unix: Option<i64>,
    /// Where the log window starts.
    pub window_start_unix: i64,
    /// `true` when the window was cut to 7 days.
    pub window_capped: bool,
    /// Priced `LlmCost` spend over the last 24 h; `None` if the audit chain
    /// couldn't be read.
    pub spend_24h_usd: Option<f64>,
    /// Operator-visible memory topics; `None` without a memory substrate.
    pub memory_topics: Option<u64>,
    pub needs_you: Vec<NeedsYouItem>,
    /// Oldest first, at most 12.
    pub log: Vec<LogEntry>,
    /// Older log lines left out of `log`.
    pub log_more: u32,
    pub coming_up: Vec<UpcomingItem>,
    /// `true` when any `LlmCost` in the 24 h window had no known price, so
    /// `spend_24h_usd` is a lower bound, not the full spend.
    #[serde(default)]
    pub spend_untracked: bool,
}

/// One card under "Needs you".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NeedsYouItem {
    /// Stable per item (the Studio's list key).
    pub key: String,
    pub sentence: String,
    /// A second, quieter line (a proposal's reason, a gate's scope).
    pub detail: Option<String>,
    pub action: NeedsYouAction,
    /// The Studio view slug that owns this item (`View::from_slug`).
    pub link: String,
}

/// What the card's buttons do.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum NeedsYouAction {
    /// Approve / Deny → `FrontendMessage::ResolveGate`.
    MissionGate { mission_id: String, gate_id: String },
    /// Approve / Deny → `QueryPayload::ResolveTeamGate`.
    TeamGate { mission_id: String, step: String },
    /// Review → open `link`.
    Review,
    /// Done / Snooze 1 h → `CompleteReminder` / `SnoozeReminder`.
    Reminder { id: String },
    /// Nothing to press here; the link explains.
    Look,
}

/// One line under "Since you were last here".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogEntry {
    pub at_unix: i64,
    pub sentence: String,
    /// Something went wrong (the time is shown in the warn colour).
    pub warn: bool,
    pub link: String,
}

/// One line under "Coming up".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpcomingItem {
    /// `None` for work already in progress.
    pub at_unix: Option<i64>,
    pub sentence: String,
    pub link: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{QueryPayload, QueryResponsePayload};

    #[test]
    fn briefing_and_reminder_messages_round_trip() {
        let b = Briefing {
            last_active_unix: Some(10),
            window_start_unix: 10,
            window_capped: false,
            spend_24h_usd: Some(0.25),
            memory_topics: Some(3),
            needs_you: vec![NeedsYouItem {
                key: "reminder:r1".into(),
                sentence: "Reminder: call mom".into(),
                detail: None,
                action: NeedsYouAction::Reminder { id: "r1".into() },
                link: "reminders".into(),
            }],
            log: vec![LogEntry { at_unix: 20, sentence: "I ran the routine digest.".into(), warn: false, link: "schedules".into() }],
            log_more: 0,
            coming_up: vec![UpcomingItem { at_unix: None, sentence: "Working on “ship it”.".into(), link: "mission-control".into() }],
            spend_untracked: true,
        };
        let resp = QueryResponsePayload::Briefing { briefing: b };
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(serde_json::from_str::<QueryResponsePayload>(&json).unwrap(), resp);

        for q in [
            QueryPayload::GetBriefing,
            QueryPayload::CompleteReminder { id: "r1".into() },
            QueryPayload::SnoozeReminder { id: "r1".into(), secs: 3600 },
        ] {
            let json = serde_json::to_string(&q).unwrap();
            assert_eq!(serde_json::from_str::<QueryPayload>(&json).unwrap(), q);
        }
        let upd = QueryResponsePayload::ReminderUpdated { id: "r1".into(), ok: true, due_unix: Some(99) };
        let json = serde_json::to_string(&upd).unwrap();
        assert_eq!(serde_json::from_str::<QueryResponsePayload>(&json).unwrap(), upd);
    }

    /// Old JSON (written before `spend_untracked` existed) still
    /// deserializes, defaulting the new field to `false`.
    #[test]
    fn briefing_without_spend_untracked_field_deserializes() {
        let json = r#"{
            "last_active_unix": null,
            "window_start_unix": 10,
            "window_capped": false,
            "spend_24h_usd": 0.25,
            "memory_topics": null,
            "needs_you": [],
            "log": [],
            "log_more": 0,
            "coming_up": []
        }"#;
        let b: Briefing = serde_json::from_str(json).unwrap();
        assert!(!b.spend_untracked);
    }
}
