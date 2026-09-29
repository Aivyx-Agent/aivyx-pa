//! Routing visibility B4 — the Studio's pure routing helpers: the plain
//! words the Models screen, the status bar and the cloud-consent card show
//! for the router's state. No Dioxus, no IPC — every function here is a
//! pure mapping so it can be tested natively (`cargo test -p aivyx-web`).

use aivyx_ipc::protocol::{RoutingStatusView, turn_outcome_correction};

/// A candidate's capabilities in plain words: the known ones joined with
/// " · ", then each assumed-but-unconfirmed one as "`name`: unknown" —
/// `tools · vision · thinking: unknown`. `completion` is left out (every
/// candidate completes text); a candidate with nothing else is "text only".
pub fn plain_capabilities(known: &[String], unknown: &[String]) -> String {
    let parts: Vec<String> = known
        .iter()
        .filter(|c| c.as_str() != "completion")
        .cloned()
        .chain(
            unknown
                .iter()
                .filter(|c| c.as_str() != "completion")
                .map(|c| format!("{c}: unknown")),
        )
        .collect();
    if parts.is_empty() {
        "text only".to_string()
    } else {
        parts.join(" · ")
    }
}

/// The residency label for a candidate: `loaded`, `needs load`, `may not
/// fit`, or "—" when residency has no opinion (or an unknown value).
pub fn residency_label(residency: Option<&str>) -> &'static str {
    match residency {
        Some("loaded") => "loaded",
        Some("needs_load") => "needs load",
        Some("wont_fit") => "may not fit",
        _ => "—",
    }
}

/// The chip tone for a residency value (a `stitch.css` `.chip` modifier).
pub fn residency_chip(residency: Option<&str>) -> &'static str {
    match residency {
        Some("loaded") => "chip success",
        Some("needs_load") => "chip",
        Some("wont_fit") => "chip warning",
        _ => "chip muted",
    }
}

/// The chip tone for a candidate's availability.
pub fn availability_chip(availability: &str) -> &'static str {
    match availability {
        "available" => "chip success",
        "unavailable" => "chip error",
        _ => "chip muted",
    }
}

/// `n` with comma thousands separators (`12578` → `"12,578"`), matching
/// `aivyx-llm`'s `with_thousands`.
pub fn with_thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A context window in tokens, or "unknown".
pub fn context_label(tokens: Option<u32>) -> String {
    match tokens {
        Some(t) => with_thousands(u64::from(t)),
        None => "unknown".to_string(),
    }
}

/// VRAM in plain words: "21.5 GiB free of 24.0 GiB", or that no local
/// server has reported it yet.
pub fn vram_label(total: Option<u64>, available: Option<u64>) -> String {
    let gib = |b: u64| b as f64 / (1024.0 * 1024.0 * 1024.0);
    match (total, available) {
        (Some(t), Some(a)) => format!("{:.1} GiB free of {:.1} GiB", gib(a), gib(t)),
        (Some(t), None) => format!("{:.1} GiB total", gib(t)),
        (None, Some(a)) => format!("{:.1} GiB free", gib(a)),
        (None, None) => "not reported yet".to_string(),
    }
}

/// The escalation mode in plain words.
pub fn escalation_label(mode: &str) -> String {
    match mode {
        "off" => "off — no cloud endpoint is configured".to_string(),
        "ask" => "ask — cloud use needs your consent per conversation".to_string(),
        "auto" => "auto — cloud use without asking".to_string(),
        other => other.to_string(),
    }
}

/// The pin selector's "Auto" value. Not empty: an `<option value="">`
/// loses its value attribute and reads back as its label ("Auto"), which
/// would then be sent as a model name. `*` is never a model id.
pub const AUTO_PIN: &str = "*";

/// The pin selector's options, `(value, label)`: "Auto" ([`AUTO_PIN`],
/// which unpins) first, then each candidate by its `id@endpoint`.
pub fn pin_options(status: &RoutingStatusView) -> Vec<(String, String)> {
    std::iter::once((AUTO_PIN.to_string(), "Auto".to_string()))
        .chain(status.candidates.iter().map(|c| (c.model.clone(), c.model.clone())))
        .collect()
}

/// The pin selector's current value: this conversation's pin, else
/// [`AUTO_PIN`].
pub fn pin_selected(status: &RoutingStatusView) -> String {
    status
        .session
        .as_ref()
        .and_then(|s| s.pinned.clone())
        .unwrap_or_else(|| AUTO_PIN.to_string())
}

/// The `SetRoutingPin` model for a selected option: `None` (unpin) for
/// "Auto", else the candidate's `id@endpoint`.
pub fn pin_request(value: &str) -> Option<String> {
    (value != AUTO_PIN).then(|| value.to_string())
}

/// The consent card's sentence — the same channel-neutral two sentences
/// as `aivyx-llm`'s `consent_lead`, built from the event's fields.
pub fn consent_lead(model: &str, endpoint: &str, why: &str, estimated_tokens: u32) -> String {
    format!(
        "This needs a cloud model: `{model}` (your `{endpoint}` endpoint), because {why}. About {} \
         tokens — this conversation plus the assistant's instructions — would be sent.",
        with_thousands(u64::from(estimated_tokens))
    )
}

/// The outcome line to add after a turn: none when a consent card was
/// shown this turn (the outcome text is that same request, worded for the
/// channel), else [`turn_outcome_correction`].
pub fn outcome_note(displayed: &str, outcome: &str, consent_shown: bool) -> Option<String> {
    if consent_shown {
        return None;
    }
    turn_outcome_correction(displayed, outcome)
}

/// What the status bar's model segment shows.
#[derive(Debug, Clone, PartialEq)]
pub enum StatusModel {
    /// The latest `ModelRouted` for the Studio's conversation.
    Routed { model: String, reason: String },
    /// Routing is on but this conversation has no routed call yet.
    NoneYet,
    /// Routing is off (or its state isn't known yet).
    Off,
}

impl StatusModel {
    /// The segment's state from the latest `ModelRouted` (`model`,
    /// `reason`) and whether routing is enabled (`None` until the first
    /// `RoutingStatus` answer).
    pub fn from_state(routed: Option<&(String, String)>, enabled: Option<bool>) -> Self {
        match (routed, enabled) {
            (Some((model, reason)), _) => StatusModel::Routed { model: model.clone(), reason: reason.clone() },
            (None, Some(true)) => StatusModel::NoneYet,
            (None, _) => StatusModel::Off,
        }
    }

    /// The segment's tooltip.
    pub fn tooltip(&self) -> String {
        match self {
            StatusModel::Routed { reason, .. } => reason.clone(),
            StatusModel::NoneYet => "No routed call in this conversation yet.".to_string(),
            StatusModel::Off => "Model routing is off — every turn uses the configured model.".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_ipc::protocol::{RoutingCandidateView, RoutingSessionView};

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    fn candidate(model: &str) -> RoutingCandidateView {
        RoutingCandidateView {
            model: model.to_string(),
            tier: "medium".to_string(),
            capabilities: s(&["completion", "tools"]),
            unknown_capabilities: Vec::new(),
            context_window: Some(32768),
            availability: "available".to_string(),
            residency: None,
        }
    }

    fn status(pinned: Option<&str>) -> RoutingStatusView {
        RoutingStatusView {
            enabled: true,
            default_model: Some("small@default".to_string()),
            candidates: vec![candidate("small@default"), candidate("big@gpu")],
            vram_total_bytes: None,
            vram_available_bytes: None,
            escalation_mode: "off".to_string(),
            classifier_enabled: false,
            session: Some(RoutingSessionView {
                session_id: "s".to_string(),
                current_model: None,
                pinned: pinned.map(str::to_string),
                last_model: None,
                last_reason: None,
                tainted: None,
                cloud_allowed: false,
            }),
        }
    }

    #[test]
    fn capabilities_read_as_plain_words_with_unknowns_marked() {
        assert_eq!(
            plain_capabilities(&s(&["completion", "tools", "vision"]), &s(&["thinking"])),
            "tools · vision · thinking: unknown"
        );
    }

    #[test]
    fn a_completion_only_candidate_is_text_only() {
        assert_eq!(plain_capabilities(&s(&["completion"]), &[]), "text only");
        assert_eq!(plain_capabilities(&[], &[]), "text only");
    }

    #[test]
    fn only_unknown_capabilities_still_read_plainly() {
        assert_eq!(
            plain_capabilities(&s(&["completion"]), &s(&["tools", "vision"])),
            "tools: unknown · vision: unknown"
        );
    }

    #[test]
    fn residency_labels_are_plain_words() {
        assert_eq!(residency_label(Some("loaded")), "loaded");
        assert_eq!(residency_label(Some("needs_load")), "needs load");
        assert_eq!(residency_label(Some("wont_fit")), "may not fit");
        assert_eq!(residency_label(None), "—");
        assert_eq!(residency_label(Some("something_new")), "—");
    }

    #[test]
    fn residency_and_availability_chips_use_existing_tones() {
        assert_eq!(residency_chip(Some("loaded")), "chip success");
        assert_eq!(residency_chip(Some("needs_load")), "chip");
        assert_eq!(residency_chip(Some("wont_fit")), "chip warning");
        assert_eq!(residency_chip(None), "chip muted");
        assert_eq!(availability_chip("available"), "chip success");
        assert_eq!(availability_chip("unverified"), "chip muted");
        assert_eq!(availability_chip("unavailable"), "chip error");
    }

    #[test]
    fn thousands_separators() {
        assert_eq!(with_thousands(0), "0");
        assert_eq!(with_thousands(999), "999");
        assert_eq!(with_thousands(1000), "1,000");
        assert_eq!(with_thousands(12578), "12,578");
        assert_eq!(with_thousands(1_234_567), "1,234,567");
    }

    #[test]
    fn context_windows_use_thousands_or_unknown() {
        assert_eq!(context_label(Some(32768)), "32,768");
        assert_eq!(context_label(None), "unknown");
    }

    #[test]
    fn vram_reads_in_gib_or_says_it_is_unreported() {
        let gib = 1024 * 1024 * 1024;
        assert_eq!(vram_label(Some(24 * gib), Some(20 * gib + gib / 2)), "20.5 GiB free of 24.0 GiB");
        assert_eq!(vram_label(Some(24 * gib), None), "24.0 GiB total");
        assert_eq!(vram_label(None, None), "not reported yet");
    }

    #[test]
    fn escalation_modes_read_plainly() {
        assert_eq!(escalation_label("off"), "off — no cloud endpoint is configured");
        assert_eq!(escalation_label("ask"), "ask — cloud use needs your consent per conversation");
        assert_eq!(escalation_label("auto"), "auto — cloud use without asking");
        assert_eq!(escalation_label("never"), "never");
    }

    #[test]
    fn pin_options_are_auto_then_each_candidate() {
        assert_eq!(
            pin_options(&status(None)),
            vec![
                (AUTO_PIN.to_string(), "Auto".to_string()),
                ("small@default".to_string(), "small@default".to_string()),
                ("big@gpu".to_string(), "big@gpu".to_string()),
            ]
        );
    }

    #[test]
    fn pin_options_with_routing_off_are_just_auto() {
        assert_eq!(pin_options(&RoutingStatusView::disabled()), vec![(AUTO_PIN.to_string(), "Auto".to_string())]);
    }

    #[test]
    fn the_selected_pin_is_the_conversations_pin_or_auto() {
        assert_eq!(pin_selected(&status(Some("big@gpu"))), "big@gpu");
        assert_eq!(pin_selected(&status(None)), AUTO_PIN);
        assert_eq!(pin_selected(&RoutingStatusView::disabled()), AUTO_PIN);
    }

    #[test]
    fn the_auto_option_unpins_and_a_model_option_pins_it() {
        // Found in the real run: an `<option value="">` loses its value
        // attribute and reads back as its label, so "Auto" would have been
        // sent as a model name. The sentinel is never a model id.
        assert_eq!(pin_request(AUTO_PIN), None);
        assert_eq!(pin_request("big@gpu"), Some("big@gpu".to_string()));
        assert!(pin_options(&status(None)).iter().all(|(v, _)| !v.is_empty()));
    }

    #[test]
    fn the_consent_lead_matches_the_daemons_wording() {
        assert_eq!(
            consent_lead("claude-sonnet-5", "claude", "this kind of request is set to use the cloud", 12578),
            "This needs a cloud model: `claude-sonnet-5` (your `claude` endpoint), because this \
             kind of request is set to use the cloud. About 12,578 tokens — this conversation \
             plus the assistant's instructions — would be sent."
        );
    }

    #[test]
    fn the_outcome_line_is_suppressed_when_a_consent_card_was_shown() {
        let outcome = "completed: This needs a cloud model: … Send /allow-cloud to allow it.";
        assert_eq!(outcome_note("", outcome, true), None);
        assert!(outcome_note("", outcome, false).is_some());
        // A normal turn is untouched either way.
        assert_eq!(outcome_note("hi", "completed: hi", false), None);
    }

    #[test]
    fn the_status_model_shows_the_latest_routed_model() {
        let routed = ("big@gpu".to_string(), "needs tools".to_string());
        let m = StatusModel::from_state(Some(&routed), Some(true));
        assert_eq!(m, StatusModel::Routed { model: "big@gpu".into(), reason: "needs tools".into() });
        assert_eq!(m.tooltip(), "needs tools");
    }

    #[test]
    fn the_status_model_without_a_decision_depends_on_routing() {
        assert_eq!(StatusModel::from_state(None, Some(true)), StatusModel::NoneYet);
        assert_eq!(StatusModel::from_state(None, Some(false)), StatusModel::Off);
        assert_eq!(StatusModel::from_state(None, None), StatusModel::Off);
        assert_eq!(StatusModel::NoneYet.tooltip(), "No routed call in this conversation yet.");
        assert_eq!(
            StatusModel::Off.tooltip(),
            "Model routing is off — every turn uses the configured model."
        );
    }
}
