//! Model routing Part 3b — consent-gated cloud escalation.
//!
//! [`EscalationGuard`] is the read side of the per-conversation privacy
//! state the escalation path consults before any call leaves the machine
//! for a `[routing.endpoints.*]` cloud endpoint (G6): the persisted,
//! write-once routing taint and the in-memory, per-conversation consent
//! grant. The daemon implements it with
//! `aivyx_channel::routing_guard::RoutingGuard`; the write side is
//! `aivyx_core::TaintSink`.

use async_trait::async_trait;

/// Per-conversation escalation state, read-only.
#[async_trait]
pub trait EscalationGuard: Send + Sync {
    /// The recorded taint reason for `session`, or `None` if the
    /// conversation is untainted. A tainted conversation never escalates,
    /// in any mode. Implementations fail safe: if the taint state cannot
    /// be read, they report the session as tainted.
    async fn taint(&self, session: &str) -> Option<String>;

    /// Has the operator allowed cloud escalation for `session` in this
    /// process's lifetime? Consent is in-memory only (a restart re-asks)
    /// and never overrides a taint.
    fn consented(&self, session: &str) -> bool;

    /// Has `session` been armed for `on_failure` escalation by a failed
    /// local turn (A16)? Default: never armed — the guard state that sets
    /// armed marks lands in a later task; until then this is a no-op.
    fn armed(&self, _session: &str) -> bool {
        false
    }

    /// Is `session` armed for its *next* turn — i.e. will the next turn
    /// escalate, before it has even started? This is the `armed` set
    /// (`RoutingGuard`'s `ArmState::armed`), never the `active` set
    /// [`Self::armed`] reads. `routing.status` reports this as
    /// `this_conversation.armed`, since that's the question an operator
    /// looking at status actually has ("will my next message escalate?"),
    /// not whether a call is escalating *right now* mid-turn. Default:
    /// never armed.
    fn armed_next(&self, _session: &str) -> bool {
        false
    }

    /// Record that a call in `session` stopped for operator consent
    /// (`ask` mode), with what it would have sent where (`request`), so
    /// the front end can show it. When the trigger is `on_failure` this is
    /// also a pending offer, which `/allow-cloud` arms for the resend and
    /// the next turn otherwise declines. Default: no-op.
    fn note_consent_requested(&self, _session: &str, _request: &ConsentRequest) {}
}

/// Routing visibility B1 — a cloud-consent stop: the cloud model the call
/// would have used, its endpoint, why it wanted the cloud, and roughly how
/// much would have been sent. Never carries content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsentRequest {
    /// The model id (no `@endpoint`).
    pub model: String,
    /// The `[routing.endpoints.*]` name.
    pub endpoint: String,
    /// The [`Trigger::name`] spelling: `no_local_candidate`, `tier` or
    /// `on_failure`.
    pub trigger: String,
    /// The router's estimate of the prompt: the conversation plus the
    /// system prompt.
    pub estimated_tokens: u32,
}

/// The plain-words reason for a trigger (by its [`Trigger::name`]
/// spelling; the config's `tiers` is accepted too), for "because …".
pub fn plain_why(trigger: &str) -> &'static str {
    match trigger {
        "no_local_candidate" => "no local model can handle this request",
        "tier" | "tiers" => "this kind of request is set to use the cloud",
        "on_failure" => "the local model got stuck",
        _ => "this request is set to use the cloud",
    }
}

/// `n` with comma thousands separators (`12578` → `"12,578"`).
pub fn with_thousands(n: u32) -> String {
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

/// The channel-neutral first two sentences of a consent stop: what would
/// go where, and why. `RoutedProvider`'s own error text.
pub fn consent_lead(req: &ConsentRequest) -> String {
    format!(
        "This needs a cloud model: `{}` (your `{}` endpoint), because {}. About {} tokens — \
         this conversation plus the assistant's instructions — would be sent.",
        req.model,
        req.endpoint,
        plain_why(&req.trigger),
        with_thousands(req.estimated_tokens)
    )
}

/// What only the operator can do, for a channel that can't grant consent.
const OPERATOR_ONLY: &str = "Cloud use can only be allowed by the operator — from the terminal \
                             (`/allow-cloud`) or the Studio.";

/// The full consent-stop message for a channel: [`consent_lead`], then
/// how to allow it — `/allow-cloud` where this channel can grant consent
/// (`can_allow_here`: Trusted or Kernel), otherwise who can.
pub fn consent_text(req: &ConsentRequest, can_allow_here: bool) -> String {
    let lead = consent_lead(req);
    if can_allow_here {
        format!(
            "{lead} Send /allow-cloud to allow it for this conversation, then resend your message."
        )
    } else {
        format!("{lead} {OPERATOR_ONLY}")
    }
}

/// The `ask`-mode `on_failure` hint appended to a turn the local model got
/// stuck on, naming the cloud model (`model`, an id) a resend would retry
/// on — per channel, like [`consent_text`].
pub fn stuck_hint(model: &str, can_allow_here: bool) -> String {
    if can_allow_here {
        format!("The local model got stuck; send /allow-cloud and resend to retry on `{model}`.")
    } else {
        format!("The local model got stuck; it could retry on `{model}`. {OPERATOR_ONLY}")
    }
}

/// How cloud escalation is gated. Mirrors `[routing.escalation] mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscalationMode {
    Never,
    Ask,
    Auto,
}

impl EscalationMode {
    /// The config / audit spelling.
    pub fn name(self) -> &'static str {
        match self {
            EscalationMode::Never => "never",
            EscalationMode::Ask => "ask",
            EscalationMode::Auto => "auto",
        }
    }
}

/// Why a call wants a cloud model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// No local model meets the call's hard needs.
    NoLocalCandidate,
    /// The task kind is listed in `[routing.escalation] tiers`.
    Tier,
    /// The conversation's previous turn failed locally (A16).
    OnFailure,
}

impl Trigger {
    /// The audit spelling.
    pub fn name(self) -> &'static str {
        match self {
            Trigger::NoLocalCandidate => "no_local_candidate",
            Trigger::Tier => "tier",
            Trigger::OnFailure => "on_failure",
        }
    }
}

/// What to do with a call a trigger wants to escalate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EscalationVerdict {
    /// Send it to a cloud model.
    Proceed,
    /// `ask` mode without a grant: stop the turn and ask.
    NeedsConsent,
    /// The conversation is tainted; the reason names the source.
    Blocked(String),
    /// Escalation is off, or impossible for this call (no session).
    Disabled,
}

/// Pure: mode × session × consent × taint → verdict. A call without a
/// session never escalates (its taint can't be known); taint wins over
/// every mode and over consent.
pub fn decide_escalation(
    mode: EscalationMode,
    session: Option<&str>,
    consented: bool,
    taint: Option<&str>,
) -> EscalationVerdict {
    if mode == EscalationMode::Never || session.is_none() {
        return EscalationVerdict::Disabled;
    }
    if let Some(reason) = taint {
        return EscalationVerdict::Blocked(reason.to_string());
    }
    match mode {
        EscalationMode::Auto => EscalationVerdict::Proceed,
        EscalationMode::Ask if consented => EscalationVerdict::Proceed,
        EscalationMode::Ask => EscalationVerdict::NeedsConsent,
        EscalationMode::Never => unreachable!("handled above"),
    }
}

/// One escalation decision, for the audit chain. Never carries content —
/// only a hash of the would-be outbound request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EscalationRecord {
    pub session_id: Option<String>,
    /// The cloud model, where one was chosen.
    pub model: Option<String>,
    pub trigger: Trigger,
    pub mode: EscalationMode,
    /// `"allowed"`, `"allowed_failed"` (allowed, but the call failed —
    /// possibly after the cloud received it), `"consent_requested"`,
    /// `"no_cloud_model"`, `"blocked_taint"` or `"disabled"`.
    pub outcome: &'static str,
    /// Hex SHA-256 of the would-be outbound request (system + messages).
    pub payload_hash: String,
}

/// Receives every escalation decision (the daemon audits through it).
pub type EscalationObserver = std::sync::Arc<dyn Fn(&EscalationRecord) + Send + Sync>;

/// Hex SHA-256 over the system prompt and the JSON-serialized messages —
/// what would leave the machine, without keeping any of it.
pub fn payload_hash(system: Option<&str>, messages: &[crate::LlmMessage]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    match system {
        Some(s) => {
            hasher.update(b"system\0");
            hasher.update(s.as_bytes());
        }
        None => hasher.update(b"nosystem\0"),
    }
    hasher.update(b"\0messages\0");
    hasher.update(serde_json::to_vec(messages).unwrap_or_default());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decide_escalation_truth_table() {
        use EscalationMode::*;
        use EscalationVerdict::*;
        let blocked = || Blocked("gmail.search output".to_string());
        // (mode, session, consented, taint) -> verdict
        type Case<'a> = (EscalationMode, Option<&'a str>, bool, Option<&'a str>, EscalationVerdict);
        let cases: Vec<Case<'_>> = vec![
            // never: always disabled, whatever else holds
            (Never, Some("s"), false, None, Disabled),
            (Never, Some("s"), true, None, Disabled),
            (Never, Some("s"), true, Some("gmail.search output"), Disabled),
            (Never, None, true, None, Disabled),
            // no session: never escalates, in any mode
            (Ask, None, true, None, Disabled),
            (Auto, None, false, None, Disabled),
            (Auto, None, false, Some("gmail.search output"), Disabled),
            // taint wins over every mode and over consent
            (Auto, Some("s"), false, Some("gmail.search output"), blocked()),
            (Auto, Some("s"), true, Some("gmail.search output"), blocked()),
            (Ask, Some("s"), true, Some("gmail.search output"), blocked()),
            (Ask, Some("s"), false, Some("gmail.search output"), blocked()),
            // untainted
            (Auto, Some("s"), false, None, Proceed),
            (Auto, Some("s"), true, None, Proceed),
            (Ask, Some("s"), true, None, Proceed),
            (Ask, Some("s"), false, None, NeedsConsent),
        ];
        for (mode, session, consented, taint, expected) in cases {
            assert_eq!(
                decide_escalation(mode, session, consented, taint),
                expected,
                "mode={mode:?} session={session:?} consented={consented} taint={taint:?}"
            );
        }
    }

    #[test]
    fn the_payload_hash_is_hex_stable_and_content_free() {
        let messages = vec![crate::LlmMessage::user_text("my secret email")];
        let a = payload_hash(Some("system"), &messages);
        let b = payload_hash(Some("system"), &messages);
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert!(!a.contains("secret"));
        assert_ne!(a, payload_hash(None, &messages));
        assert_ne!(
            a,
            payload_hash(Some("system"), &[crate::LlmMessage::user_text("other")])
        );
    }

    fn tier_request(tokens: u32) -> ConsentRequest {
        ConsentRequest {
            model: "claude-sonnet-5".to_string(),
            endpoint: "claude".to_string(),
            trigger: Trigger::Tier.name().to_string(),
            estimated_tokens: tokens,
        }
    }

    #[test]
    fn plain_why_covers_every_trigger() {
        assert_eq!(
            plain_why(Trigger::NoLocalCandidate.name()),
            "no local model can handle this request"
        );
        assert_eq!(plain_why(Trigger::Tier.name()), "this kind of request is set to use the cloud");
        assert_eq!(plain_why("tiers"), "this kind of request is set to use the cloud");
        assert_eq!(plain_why(Trigger::OnFailure.name()), "the local model got stuck");
    }

    #[test]
    fn consent_lead_is_the_neutral_two_sentences() {
        assert_eq!(
            consent_lead(&tier_request(12578)),
            "This needs a cloud model: `claude-sonnet-5` (your `claude` endpoint), because this \
             kind of request is set to use the cloud. About 12,578 tokens — this conversation \
             plus the assistant's instructions — would be sent."
        );
    }

    #[test]
    fn consent_text_where_consent_can_be_given() {
        assert_eq!(
            consent_text(&tier_request(12578), true),
            "This needs a cloud model: `claude-sonnet-5` (your `claude` endpoint), because this \
             kind of request is set to use the cloud. About 12,578 tokens — this conversation \
             plus the assistant's instructions — would be sent. Send /allow-cloud to allow it \
             for this conversation, then resend your message."
        );
    }

    #[test]
    fn consent_text_where_only_the_operator_can_give_it() {
        let text = consent_text(&tier_request(1_234_567), false);
        assert_eq!(
            text,
            "This needs a cloud model: `claude-sonnet-5` (your `claude` endpoint), because this \
             kind of request is set to use the cloud. About 1,234,567 tokens — this conversation \
             plus the assistant's instructions — would be sent. Cloud use can only be allowed by \
             the operator — from the terminal (`/allow-cloud`) or the Studio."
        );
    }

    #[test]
    fn thousands_separators() {
        assert_eq!(with_thousands(0), "0");
        assert_eq!(with_thousands(999), "999");
        assert_eq!(with_thousands(1000), "1,000");
        assert_eq!(with_thousands(12578), "12,578");
        assert_eq!(with_thousands(u32::MAX), "4,294,967,295");
    }

    #[test]
    fn the_stuck_hint_is_per_channel() {
        assert_eq!(
            stuck_hint("claude-sonnet-5", true),
            "The local model got stuck; send /allow-cloud and resend to retry on `claude-sonnet-5`."
        );
        assert_eq!(
            stuck_hint("claude-sonnet-5", false),
            "The local model got stuck; it could retry on `claude-sonnet-5`. Cloud use can only \
             be allowed by the operator — from the terminal (`/allow-cloud`) or the Studio."
        );
    }

    #[test]
    fn modes_and_triggers_have_audit_names() {
        assert_eq!(EscalationMode::Ask.name(), "ask");
        assert_eq!(EscalationMode::Auto.name(), "auto");
        assert_eq!(EscalationMode::Never.name(), "never");
        assert_eq!(Trigger::NoLocalCandidate.name(), "no_local_candidate");
        assert_eq!(Trigger::Tier.name(), "tier");
        assert_eq!(Trigger::OnFailure.name(), "on_failure");
    }
}
