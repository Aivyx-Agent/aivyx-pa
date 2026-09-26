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
}

impl Trigger {
    /// The audit spelling.
    pub fn name(self) -> &'static str {
        match self {
            Trigger::NoLocalCandidate => "no_local_candidate",
            Trigger::Tier => "tier",
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
    /// `"allowed"`, `"consent_requested"`, `"blocked_taint"` or `"disabled"`.
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

    #[test]
    fn modes_and_triggers_have_audit_names() {
        assert_eq!(EscalationMode::Ask.name(), "ask");
        assert_eq!(EscalationMode::Auto.name(), "auto");
        assert_eq!(EscalationMode::Never.name(), "never");
        assert_eq!(Trigger::NoLocalCandidate.name(), "no_local_candidate");
        assert_eq!(Trigger::Tier.name(), "tier");
    }
}
