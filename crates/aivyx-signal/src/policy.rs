//! Who may drive a turn. Pure, so every case is a unit test.
//!
//! A message is a turn only from an `allowed_senders` entry whose identity
//! key the operator has verified (`TRUSTED_VERIFIED`). An allowed sender
//! with an unverified or changed key gets one explanation per run;
//! everyone else is ignored silently.

use std::collections::HashSet;

use crate::envelope::{Inbound, TextMessage};
use crate::transport::TrustLevel;

/// Told once per sender per run when their key isn't verified.
pub const ASK_TO_VERIFY: &str = "I only answer a verified safety number. Run \
    `aivyx-pa signal verify` and compare the number it shows with your phone \
    (Signal → this chat → View safety number).";

/// Entries are E.164 numbers or ACIs (UUIDs, lowercased).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignalPolicy {
    allowed_senders: Vec<String>,
}

impl SignalPolicy {
    pub fn new(allowed_senders: &[String]) -> Self {
        SignalPolicy {
            allowed_senders: allowed_senders
                .iter()
                .map(|s| s.trim().to_ascii_lowercase())
                .collect(),
        }
    }

    pub fn allowed_senders(&self) -> &[String] {
        &self.allowed_senders
    }

    /// The sender is listed, by number or by ACI.
    pub fn is_allowed(&self, m: &TextMessage) -> bool {
        [m.sender_number.as_deref(), m.sender_uuid.as_deref()]
            .into_iter()
            .flatten()
            .any(|id| self.allowed_senders.iter().any(|a| a == &id.to_ascii_lowercase()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Turn(TextMessage),
    /// Reply [`ASK_TO_VERIFY`] to `to` (once per run).
    AskToVerify { to: String },
    Ignore,
}

/// `trust`: signal-cli's trust level for the sender's key (looked up only
/// for allowed senders; `None` when it knows no key).
pub fn decide(p: &SignalPolicy, inbound: Inbound, trust: Option<TrustLevel>) -> Action {
    let Inbound::Text(m) = inbound else {
        return Action::Ignore;
    };
    if !p.is_allowed(&m) {
        return Action::Ignore;
    }
    if trust == Some(TrustLevel::TrustedVerified) {
        Action::Turn(m)
    } else {
        Action::AskToVerify {
            to: m.sender().to_string(),
        }
    }
}

/// Remembers who already got [`ASK_TO_VERIFY`] this run.
#[derive(Debug, Default)]
pub struct OncePerSender(HashSet<String>);

impl OncePerSender {
    /// `true` the first time `who` is seen.
    pub fn first(&mut self, who: &str) -> bool {
        self.0.insert(who.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> SignalPolicy {
        SignalPolicy::new(&["+15550001111".to_string(), "AAAA-UUID".to_string()])
    }

    fn msg(number: Option<&str>, uuid: Option<&str>) -> Inbound {
        Inbound::Text(TextMessage {
            sender_number: number.map(str::to_string),
            sender_uuid: uuid.map(str::to_string),
            timestamp: 1,
            text: "hi".into(),
            attachments: 0,
        })
    }

    #[test]
    fn only_a_verified_allowed_sender_is_a_turn() {
        let p = policy();
        let me = || msg(Some("+15550001111"), Some("x"));
        assert!(matches!(decide(&p, me(), Some(TrustLevel::TrustedVerified)), Action::Turn(_)));
        for t in [None, Some(TrustLevel::TrustedUnverified), Some(TrustLevel::Untrusted)] {
            assert_eq!(
                decide(&p, me(), t),
                Action::AskToVerify { to: "+15550001111".into() },
                "{t:?}"
            );
        }
    }

    #[test]
    fn strangers_and_non_messages_are_ignored_even_when_verified() {
        let p = policy();
        let v = Some(TrustLevel::TrustedVerified);
        assert_eq!(decide(&p, msg(Some("+15559999999"), Some("y")), v), Action::Ignore);
        assert_eq!(decide(&p, Inbound::Other, v), Action::Ignore);
    }

    #[test]
    fn a_sender_may_be_listed_by_aci() {
        let p = policy();
        let m = msg(None, Some("aaaa-uuid"));
        assert_eq!(
            decide(&p, m, Some(TrustLevel::TrustedUnverified)),
            Action::AskToVerify { to: "aaaa-uuid".into() }
        );
    }

    #[test]
    fn the_verify_reply_goes_once_per_sender() {
        let mut once = OncePerSender::default();
        assert!(once.first("+1"));
        assert!(!once.first("+1"));
        assert!(once.first("+2"));
    }
}
