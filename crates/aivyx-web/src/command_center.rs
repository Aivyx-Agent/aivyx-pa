//! The Command Center as a logbook: an instrument strip, a greeting, what
//! needs the operator, what the assistant did since they were last here, and
//! what's coming up — all from the daemon's `Briefing`. The helpers at the
//! top are pure (tested natively); the components below render them.

use aivyx_ipc::briefing::{Briefing, NeedsYouAction, NeedsYouItem};
use aivyx_ipc::protocol::{FrontendMessage, QueryPayload};
use dioxus::prelude::*;

use crate::{ApprovalCard, Dashboard, PendingApproval, Sender, View};

/// The headline for a local hour (0–23).
pub fn greeting(hour: u32) -> &'static str {
    match hour {
        5..=11 => "Good morning.",
        12..=17 => "Good afternoon.",
        _ => "Good evening.",
    }
}

/// "just now", "12 min ago", "9 h ago", "3 days ago".
pub fn ago(then: i64, now: i64) -> String {
    let d = (now - then).max(0);
    match d {
        0..=59 => "just now".to_string(),
        60..=3_599 => format!("{} min ago", d / 60),
        3_600..=86_399 => format!("{} h ago", d / 3_600),
        _ if d < 2 * 86_400 => "1 day ago".to_string(),
        _ => format!("{} days ago", d / 86_400),
    }
}

pub fn hhmm(hour: u32, minute: u32) -> String {
    format!("{hour:02}:{minute:02}")
}

/// A "Coming up" time: today → "19:00", tomorrow → "tmrw 07:00",
/// later → "in 3 d · 07:00".
pub fn when_label(day_offset: i64, hhmm: &str) -> String {
    match day_offset {
        i64::MIN..=0 => hhmm.to_string(),
        1 => format!("tmrw {hhmm}"),
        n => format!("in {n} d · {hhmm}"),
    }
}

pub fn log_heading(b: &Briefing) -> &'static str {
    if b.window_capped {
        "In the last 7 days"
    } else if b.last_active_unix.is_none() {
        "In the last 24 hours"
    } else {
        "Since you were last here"
    }
}

/// The line under the log heading when there's nothing to log — it must
/// agree with [`log_heading`].
pub fn empty_log_line(b: &Briefing) -> &'static str {
    if log_heading(b) == "Since you were last here" {
        "All quiet since you were last here."
    } else {
        "All quiet."
    }
}

/// The spend instrument and whether it should warn (≥ 80 % of the day cap).
pub fn spend_reading(spend: Option<f64>, per_day: Option<f64>) -> (String, bool) {
    match (spend, per_day) {
        (None, _) => ("spend —".to_string(), false),
        (Some(s), Some(cap)) if cap > 0.0 => {
            (format!("${s:.2} of ${cap:.2} · 24 h"), s >= 0.8 * cap)
        }
        (Some(s), _) => (format!("${s:.2} · 24 h"), false),
    }
}

pub fn more_line(n: u32) -> Option<String> {
    (n > 0).then(|| format!("and {n} more → Audit"))
}

/// The latest briefing, when it arrived, and the last failure to show.
#[derive(Clone, Default, PartialEq)]
pub struct BriefingState {
    pub briefing: Option<Briefing>,
    /// `Date.now()` when `briefing` arrived — the "as of" line when offline.
    pub as_of_ms: f64,
    /// The one notice line above the "Needs you" cards.
    pub notice: Option<String>,
    /// `notice` came from a failed `GetBriefing` (not a card action), so the
    /// next good briefing clears it. An action's failure stays until the
    /// next action: the post-action re-fetch must not wipe it.
    pub notice_from_briefing: bool,
    /// A mission gate's Approve / Deny in flight, `(mission_id, gate_id)`.
    /// `ResolveGate` answers with `GateResolved` or a bare
    /// `DaemonEnvelope::Error`, which carries no request id — this marker is
    /// how that error is recognised as the card's.
    pub pending_gate: Option<(String, String)>,
}

impl BriefingState {
    /// A card action is about to be sent: the last failure no longer applies.
    pub fn begin_action(&mut self) {
        self.notice = None;
        self.notice_from_briefing = false;
    }

    /// A mission gate's Approve / Deny is about to be sent.
    pub fn begin_gate(&mut self, mission_id: String, gate_id: String) {
        self.begin_action();
        self.pending_gate = Some((mission_id, gate_id));
    }

    /// This mission gate's Approve / Deny is in flight (its buttons are
    /// disabled until it's answered, so a double press can't send twice).
    pub fn gate_pending(&self, mission_id: &str, gate_id: &str) -> bool {
        self.pending_gate.as_ref().is_some_and(|(m, g)| m == mission_id && g == gate_id)
    }

    pub fn on_briefing(&mut self, briefing: Briefing, now_ms: f64) {
        self.briefing = Some(briefing);
        self.as_of_ms = now_ms;
        if self.notice_from_briefing {
            self.notice = None;
            self.notice_from_briefing = false;
        }
    }

    pub fn on_briefing_error(&mut self, message: String) {
        self.notice = Some(message);
        self.notice_from_briefing = true;
    }

    pub fn on_action_error(&mut self, message: String) {
        self.notice = Some(message);
        self.notice_from_briefing = false;
    }

    pub fn on_gate_resolved(&mut self, mission_id: &str, gate_id: &str) {
        if self
            .pending_gate
            .as_ref()
            .is_some_and(|(m, g)| m == mission_id && g == gate_id)
        {
            self.pending_gate = None;
        }
    }

    /// A `DaemonEnvelope::Error` arrived. If a gate action is in flight it is
    /// that action's answer: show it and return `true`.
    pub fn on_daemon_error(&mut self, message: String) -> bool {
        if self.pending_gate.take().is_some() {
            self.on_action_error(message);
            true
        } else {
            false
        }
    }
}

pub fn briefing_query() -> FrontendMessage {
    FrontendMessage::Query { id: "cc-briefing".to_string(), payload: QueryPayload::GetBriefing }
}

/// Local (hour, minute, day number) for a unix time — wasm only.
fn local_parts(unix: i64) -> (u32, u32, i64) {
    let d = js_sys::Date::new(&wasm_bindgen::JsValue::from_f64(unix as f64 * 1000.0));
    let offset_ms = d.get_timezone_offset() * 60_000.0;
    let day = ((unix as f64 * 1000.0 - offset_ms) / 86_400_000.0).floor() as i64;
    (d.get_hours(), d.get_minutes(), day)
}

/// Today as "Wednesday 1 October" — wasm only.
fn today_line() -> String {
    let options = js_sys::Object::new();
    for (k, v) in [("weekday", "long"), ("day", "numeric"), ("month", "long")] {
        let _ = js_sys::Reflect::set(&options, &k.into(), &v.into());
    }
    String::from(js_sys::Date::new_0().to_locale_date_string("en-GB", &options))
}

fn now_unix() -> i64 {
    (js_sys::Date::now() / 1000.0) as i64
}

fn go(mut view: Signal<View>, slug: &str) {
    if let Some(v) = View::from_slug(slug) {
        view.set(v);
    }
}

/// Send an action, then re-ask for the briefing on the same connection, so
/// the answer already reflects it (frames are handled in order). A new
/// action clears the last failure's notice; a mission gate is also marked
/// in flight so its error (which carries no id) can be recognised.
fn act(ws: Sender, mut state: Signal<BriefingState>, msg: FrontendMessage) {
    match &msg {
        FrontendMessage::ResolveGate { mission_id, gate_id, .. } => {
            state.write().begin_gate(mission_id.clone(), gate_id.clone())
        }
        _ => state.write().begin_action(),
    }
    ws.send(msg);
    ws.send(briefing_query());
}

fn action_query(payload: QueryPayload) -> FrontendMessage {
    FrontendMessage::Query { id: "cc-action".to_string(), payload }
}

#[component]
fn InstrumentStrip(
    dashboard: Dashboard,
    connected: bool,
    briefing: Option<Briefing>,
    view: Signal<View>,
) -> Element {
    let model = match &dashboard.settings {
        Some(s) => {
            let ctx = s
                .num_ctx
                .map(|n| format!("{}k ctx", n / 1024))
                .unwrap_or_else(|| "default ctx".into());
            format!("{} · {ctx}", s.model)
        }
        None => "model —".to_string(),
    };
    let per_day = dashboard.settings.as_ref().and_then(|s| s.budget.per_day_usd);
    let (spend, spend_warn) =
        spend_reading(briefing.as_ref().and_then(|b| b.spend_24h_usd), per_day);
    let memory = briefing
        .as_ref()
        .and_then(|b| b.memory_topics)
        .map(|n| format!("{n} topics"))
        .unwrap_or_else(|| "memory —".into());
    let (chain, chain_bad) = match dashboard.chain_ok {
        Some(true) => ("◆ chain sealed", false),
        Some(false) => ("◆ chain broken", true),
        None => ("◆ chain …", false),
    };
    rsx! {
        div { class: "cc-strip",
            button {
                class: if connected { "cc-reading" } else { "cc-reading bad" },
                onclick: move |_| go(view, "sessions"),
                if connected { "● daemon" } else { "● offline" }
            }
            button { class: "cc-reading", onclick: move |_| go(view, "models"), "{model}" }
            button {
                class: if spend_warn { "cc-reading warn" } else { "cc-reading" },
                onclick: move |_| go(view, "settings"),
                "{spend}"
            }
            button { class: "cc-reading", onclick: move |_| go(view, "memory"), "{memory}" }
            button {
                class: if chain_bad { "cc-reading bad" } else { "cc-reading" },
                onclick: move |_| go(view, "audit"),
                "{chain}"
            }
        }
    }
}

#[component]
fn NeedsYouCard(item: NeedsYouItem, view: Signal<View>) -> Element {
    let ws = use_context::<Sender>();
    let state = use_context::<Signal<BriefingState>>();
    let link = item.link.clone();
    let buttons = match item.action.clone() {
        NeedsYouAction::MissionGate { mission_id, gate_id } => {
            let sending = state.read().gate_pending(&mission_id, &gate_id);
            let (m2, g2) = (mission_id.clone(), gate_id.clone());
            rsx! {
                if sending {
                    span { class: "label-tech", "Sending…" }
                }
                button {
                    class: "btn",
                    disabled: sending,
                    onclick: move |_| act(ws, state, FrontendMessage::ResolveGate {
                        mission_id: mission_id.clone(),
                        gate_id: gate_id.clone(),
                        approved: true,
                    }),
                    "Approve"
                }
                button {
                    class: "btn ghost",
                    disabled: sending,
                    onclick: move |_| act(ws, state, FrontendMessage::ResolveGate {
                        mission_id: m2.clone(),
                        gate_id: g2.clone(),
                        approved: false,
                    }),
                    "Deny"
                }
            }
        }
        NeedsYouAction::TeamGate { mission_id, step } => {
            let (m2, s2) = (mission_id.clone(), step.clone());
            rsx! {
                button {
                    class: "btn",
                    onclick: move |_| act(ws, state, action_query(QueryPayload::ResolveTeamGate {
                        mission_id: mission_id.clone(),
                        step: step.clone(),
                        approve: true,
                    })),
                    "Approve"
                }
                button {
                    class: "btn ghost",
                    onclick: move |_| act(ws, state, action_query(QueryPayload::ResolveTeamGate {
                        mission_id: m2.clone(),
                        step: s2.clone(),
                        approve: false,
                    })),
                    "Deny"
                }
            }
        }
        NeedsYouAction::Reminder { id } => {
            let id2 = id.clone();
            rsx! {
                button {
                    class: "btn",
                    onclick: move |_| act(ws, state, action_query(QueryPayload::CompleteReminder { id: id.clone() })),
                    "Done"
                }
                button {
                    class: "btn ghost",
                    onclick: move |_| act(ws, state, action_query(QueryPayload::SnoozeReminder { id: id2.clone(), secs: 3_600 })),
                    "Snooze 1 h"
                }
            }
        }
        NeedsYouAction::Review => rsx! {
            button { class: "btn", onclick: move |_| go(view, &link), "Review" }
        },
        NeedsYouAction::Look => rsx! {
            button { class: "btn ghost", onclick: move |_| go(view, &link), "Open" }
        },
    };
    rsx! {
        div { class: "cc-card",
            p { class: "cc-sentence", "{item.sentence}" }
            if let Some(d) = &item.detail {
                p { class: "muted", "{d}" }
            }
            div { class: "cc-actions", {buttons} }
        }
    }
}

/// One rendered line of the log or of "Coming up".
#[derive(Clone, PartialEq)]
struct Row {
    time: String,
    sentence: String,
    warn: bool,
    link: String,
}

/// The whole home screen.
#[component]
pub fn Logbook(
    dashboard: Dashboard,
    connected: bool,
    view: Signal<View>,
    first_visit: bool,
    name: String,
) -> Element {
    let state = use_context::<Signal<BriefingState>>();
    let pending = use_context::<Signal<Option<PendingApproval>>>();
    let s = state();
    let now = now_unix();
    let (hour, _, today) = local_parts(now);
    let date_line = today_line();
    let last_here = s
        .briefing
        .as_ref()
        .and_then(|b| b.last_active_unix)
        .map(|t| format!(" · last here {}", ago(t, now)));
    let as_of = (!connected && s.as_of_ms > 0.0).then(|| {
        let (h, m, _) = local_parts((s.as_of_ms / 1000.0) as i64);
        format!(" · as of {}", hhmm(h, m))
    });

    rsx! {
        div { class: if connected { "cc-page" } else { "cc-page stale" },
            InstrumentStrip { dashboard: dashboard.clone(), connected, briefing: s.briefing.clone(), view }
            if first_visit {
                div { class: "glass-card welcome-card",
                    h3 { "Start here: say hi to {name}" }
                    p { class: "muted",
                        "Chat is where you ask for things — try “What can you help me with?” "
                        "This page fills in as it works: what needs you, what it did, and what's next."
                    }
                    div { class: "welcome-actions",
                        button { class: "btn", onclick: move |_| go(view, "chat"), "Open Chat" }
                        button { class: "btn ghost", onclick: move |_| go(view, "guide"), "Read the guide" }
                    }
                }
            }
            h1 { class: "cc-greeting", "{greeting(hour)}" }
            p { class: "label-tech cc-dateline",
                "{date_line}"
                if let Some(l) = last_here {
                    "{l}"
                }
                if let Some(a) = as_of {
                    "{a}"
                }
            }
            match &s.briefing {
                None => rsx! {
                    if let Some(n) = s.notice.clone() {
                        p { class: "cc-notice", "{n}" }
                    }
                    if let Some(p) = pending() {
                        ApprovalCard { p }
                    }
                    p { class: "label-tech", "Reading the record…" }
                },
                Some(b) => rsx! {
                    Sections { briefing: b.clone(), notice: s.notice.clone(), pending: pending(), view, today }
                },
            }
        }
    }
}

/// "Needs you", the log, and "Coming up" for a loaded briefing.
#[component]
fn Sections(
    briefing: Briefing,
    notice: Option<String>,
    pending: Option<PendingApproval>,
    view: Signal<View>,
    today: i64,
) -> Element {
    let b = &briefing;
    let count = b.needs_you.len() + usize::from(pending.is_some());
    let log: Vec<Row> = b
        .log
        .iter()
        .map(|l| {
            let (h, m, _) = local_parts(l.at_unix);
            Row { time: hhmm(h, m), sentence: l.sentence.clone(), warn: l.warn, link: l.link.clone() }
        })
        .collect();
    let upcoming: Vec<Row> = b
        .coming_up
        .iter()
        .map(|u| {
            let time = match u.at_unix {
                Some(t) => {
                    let (h, m, day) = local_parts(t);
                    when_label(day - today, &hhmm(h, m))
                }
                None => "now".to_string(),
            };
            Row { time, sentence: u.sentence.clone(), warn: false, link: u.link.clone() }
        })
        .collect();
    let heading = log_heading(b);
    let quiet = empty_log_line(b);
    let more = more_line(b.log_more);

    rsx! {
        h2 { class: "label-tech cc-needs", "Needs you · {count}" }
        if let Some(n) = notice {
            p { class: "cc-notice", "{n}" }
        }
        if let Some(p) = pending {
            ApprovalCard { p }
        }
        if count == 0 {
            p { class: "cc-quiet", "Nothing needs you." }
        }
        for item in b.needs_you.iter() {
            NeedsYouCard { key: "{item.key}", item: item.clone(), view }
        }

        h2 { class: "label-tech cc-section", "{heading}" }
        if log.is_empty() {
            p { class: "cc-quiet", "{quiet}" }
        }
        for (i, row) in log.into_iter().enumerate() {
            EntryRow { key: "{i}", row, view }
        }
        if let Some(more) = more {
            button { class: "cc-entry cc-more", onclick: move |_| go(view, "audit"), "{more}" }
        }

        h2 { class: "label-tech cc-section", "Coming up" }
        if upcoming.is_empty() {
            button { class: "cc-entry cc-quiet", onclick: move |_| go(view, "schedules"), "Nothing scheduled." }
        }
        for (i, row) in upcoming.into_iter().enumerate() {
            EntryRow { key: "{i}", row, view }
        }
    }
}

#[component]
fn EntryRow(row: Row, view: Signal<View>) -> Element {
    let slug = row.link.clone();
    rsx! {
        button { class: "cc-entry", onclick: move |_| go(view, &slug),
            span { class: if row.warn { "cc-time warn" } else { "cc-time" }, "{row.time}" }
            span { "{row.sentence}" }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn greeting_turns_at_five_noon_and_six() {
        assert_eq!(greeting(4), "Good evening.");
        assert_eq!(greeting(5), "Good morning.");
        assert_eq!(greeting(11), "Good morning.");
        assert_eq!(greeting(12), "Good afternoon.");
        assert_eq!(greeting(17), "Good afternoon.");
        assert_eq!(greeting(18), "Good evening.");
        assert_eq!(greeting(23), "Good evening.");
    }

    #[test]
    fn ago_reads_in_one_unit() {
        assert_eq!(ago(100, 130), "just now");
        assert_eq!(ago(0, 720), "12 min ago");
        assert_eq!(ago(0, 9 * 3_600 + 5), "9 h ago");
        assert_eq!(ago(0, 86_400 + 5), "1 day ago");
        assert_eq!(ago(0, 3 * 86_400), "3 days ago");
        assert_eq!(ago(500, 100), "just now");
    }

    #[test]
    fn coming_up_times() {
        assert_eq!(hhmm(7, 0), "07:00");
        assert_eq!(when_label(0, "19:00"), "19:00");
        assert_eq!(when_label(1, "07:00"), "tmrw 07:00");
        assert_eq!(when_label(3, "07:00"), "in 3 d · 07:00");
    }

    #[test]
    fn the_log_heading_follows_the_window() {
        let mut b = Briefing { last_active_unix: Some(1), ..Default::default() };
        assert_eq!(log_heading(&b), "Since you were last here");
        b.window_capped = true;
        assert_eq!(log_heading(&b), "In the last 7 days");
        let b = Briefing::default();
        assert_eq!(log_heading(&b), "In the last 24 hours");
    }

    #[test]
    fn the_empty_log_line_matches_the_heading() {
        let mut b = Briefing { last_active_unix: Some(1), ..Default::default() };
        assert_eq!(empty_log_line(&b), "All quiet since you were last here.");
        b.window_capped = true;
        assert_eq!(empty_log_line(&b), "All quiet.");
        assert_eq!(empty_log_line(&Briefing::default()), "All quiet.");
    }

    #[test]
    fn only_the_gate_in_flight_reads_as_pending() {
        let mut s = BriefingState::default();
        assert!(!s.gate_pending("m1", "g1"));
        s.begin_gate("m1".into(), "g1".into());
        assert!(s.gate_pending("m1", "g1"));
        assert!(!s.gate_pending("m1", "g2"));
        assert!(!s.gate_pending("m2", "g1"));
        s.on_gate_resolved("m1", "g1");
        assert!(!s.gate_pending("m1", "g1"));
    }

    #[test]
    fn spend_warns_at_eighty_percent_of_the_cap() {
        assert_eq!(spend_reading(None, Some(2.0)), ("spend —".to_string(), false));
        assert_eq!(spend_reading(Some(0.42), None), ("$0.42 · 24 h".to_string(), false));
        assert_eq!(
            spend_reading(Some(0.42), Some(2.0)),
            ("$0.42 of $2.00 · 24 h".to_string(), false)
        );
        assert!(spend_reading(Some(1.6), Some(2.0)).1);
    }

    #[test]
    fn more_line_only_when_something_was_left_out() {
        assert_eq!(more_line(0), None);
        assert_eq!(more_line(3).as_deref(), Some("and 3 more → Audit"));
    }

    fn some_briefing() -> Briefing {
        Briefing { last_active_unix: Some(1), ..Default::default() }
    }

    #[test]
    fn a_failed_mission_gate_lands_in_the_notice() {
        let mut s = BriefingState::default();
        s.begin_gate("m1".into(), "g1".into());
        assert!(s.on_daemon_error("mission m1 not found".into()));
        assert_eq!(s.notice.as_deref(), Some("mission m1 not found"));
        assert_eq!(s.pending_gate, None);
        // Without a gate in flight an error is not the Command Center's.
        assert!(!s.on_daemon_error("some chat error".into()));
        assert_eq!(s.notice.as_deref(), Some("mission m1 not found"));
    }

    #[test]
    fn a_resolved_gate_clears_the_marker() {
        let mut s = BriefingState::default();
        s.begin_gate("m1".into(), "g1".into());
        s.on_gate_resolved("m1", "g1");
        assert_eq!(s.pending_gate, None);
        assert!(!s.on_daemon_error("unrelated".into()));
        assert_eq!(s.notice, None);
    }

    #[test]
    fn a_new_action_clears_the_old_notice() {
        let mut s = BriefingState::default();
        s.on_action_error("That reminder couldn't be updated.".into());
        s.begin_action();
        assert_eq!(s.notice, None);
    }

    #[test]
    fn the_refetch_keeps_an_action_failure_but_clears_a_briefing_failure() {
        let mut s = BriefingState::default();
        s.on_action_error("gate already resolved".into());
        s.on_briefing(some_briefing(), 5.0);
        assert_eq!(s.notice.as_deref(), Some("gate already resolved"));
        assert_eq!(s.as_of_ms, 5.0);

        let mut s = BriefingState::default();
        s.on_briefing_error("audit chain unreadable".into());
        assert_eq!(s.notice.as_deref(), Some("audit chain unreadable"));
        s.on_briefing(some_briefing(), 6.0);
        assert_eq!(s.notice, None);
        assert!(s.briefing.is_some());
    }

    #[test]
    fn an_action_failure_replaces_a_briefing_failure_and_stays() {
        let mut s = BriefingState::default();
        s.on_briefing_error("x".into());
        s.on_action_error("y".into());
        s.on_briefing(some_briefing(), 1.0);
        assert_eq!(s.notice.as_deref(), Some("y"));
    }

    #[test]
    fn the_briefing_query_asks_for_the_briefing() {
        match briefing_query() {
            FrontendMessage::Query { id, payload: QueryPayload::GetBriefing } => {
                assert_eq!(id, "cc-briefing")
            }
            _ => panic!("not a GetBriefing query"),
        }
    }
}
