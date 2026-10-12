//! Which emails become turns. Pure, so every case is a unit test.
//!
//! A message is a turn only when it is addressed to `channel_address` and
//! is either **from you** — From is one of `own_addresses` and the same
//! Proton message is in your Sent folder — or **allowlisted** — From is in
//! `allowed_external_senders` and Proton's own DMARC result passes for its
//! domain. Everything else is ignored silently (no auto-reply: that would
//! confirm the address and send backscatter to forged senders). See
//! `docs/superpowers/specs/2026-10-12-proton-mail-design.md` § Trust.

use crate::message::MailMessage;

/// The authserv-id Proton's receiving server stamps on its own
/// `Authentication-Results` values.
pub const PROTON_AUTHSERV_ID: &str = "mail.protonmail.ch";

/// All addresses lowercased (by [`ProtonPolicy::new`]); `MailMessage`
/// addresses are lowercased by the parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtonPolicy {
    pub channel_address: String,
    pub own_addresses: Vec<String>,
    pub allowed_external_senders: Vec<String>,
}

impl ProtonPolicy {
    pub fn new(channel_address: &str, own: &[String], external: &[String]) -> Self {
        let lower = |v: &[String]| v.iter().map(|a| a.trim().to_ascii_lowercase()).collect();
        ProtonPolicy {
            channel_address: channel_address.trim().to_ascii_lowercase(),
            own_addresses: lower(own),
            allowed_external_senders: lower(external),
        }
    }

    /// From is one of your addresses: the caller must look the message up
    /// in Sent (by `X-Pm-Internal-Id`) before calling [`decide`].
    pub fn claims_operator(&self, m: &MailMessage) -> bool {
        m.from
            .as_deref()
            .is_some_and(|f| f != self.channel_address && self.own_addresses.iter().any(|a| a == f))
    }
}

/// Who a turn came from. Both run at `SemiTrusted`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sender {
    Operator,
    External,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Turn(Sender),
    /// Why, for the debug log only — nothing is ever sent back.
    Ignore(&'static str),
}

/// `in_sent`: the message's `X-Pm-Internal-Id` is in the Sent folder
/// (only consulted when From is one of `own_addresses`).
pub fn decide(p: &ProtonPolicy, m: &MailMessage, in_sent: bool) -> Decision {
    let Some(from) = m.from.as_deref() else {
        return Decision::Ignore("no From address");
    };
    if from == p.channel_address {
        return Decision::Ignore("from the channel address");
    }
    if is_automatic(m) {
        return Decision::Ignore("automatic mail");
    }
    let delivered = m.delivered_to.iter().any(|a| a == &p.channel_address);
    if p.own_addresses.iter().any(|a| a == from) {
        if !in_sent {
            return Decision::Ignore("From is one of yours, but this account didn't send it");
        }
        // Proven yours by Sent membership, so the To/Cc you wrote count:
        // mail between two addresses of one account may carry no
        // Delivered-To.
        let addressed = delivered || m.to.iter().chain(&m.cc).any(|a| a == &p.channel_address);
        return if addressed {
            Decision::Turn(Sender::Operator)
        } else {
            Decision::Ignore("not addressed to the channel")
        };
    }
    if !delivered {
        return Decision::Ignore("not delivered to the channel address");
    }
    if !p.allowed_external_senders.iter().any(|a| a == from) {
        return Decision::Ignore("sender not allowed");
    }
    let domain = from.rsplit_once('@').map(|(_, d)| d).unwrap_or("");
    if proton_dmarc_passes(&m.auth_results, domain) {
        Decision::Turn(Sender::External)
    } else {
        Decision::Ignore("no Proton DMARC pass for the From domain")
    }
}

/// `Auto-Submitted` other than `no`, or `Precedence: bulk/list/junk`.
fn is_automatic(m: &MailMessage) -> bool {
    m.auto_submitted
        .as_deref()
        .is_some_and(|v| !v.trim().eq_ignore_ascii_case("no"))
        || m.precedence.as_deref().is_some_and(|v| {
            matches!(v.trim().to_ascii_lowercase().as_str(), "bulk" | "list" | "junk")
        })
}

/// The **first** `Authentication-Results` value must be stamped
/// `mail.protonmail.ch` and be `dmarc=pass` with `header.from` equal to
/// `from_domain`. Proton adds its own values, DMARC first, ahead of
/// anything the sender wrote, so a forged value is never first.
pub fn proton_dmarc_passes(auth_results: &[String], from_domain: &str) -> bool {
    let Some(first) = auth_results.first() else {
        return false;
    };
    let mut parts = first.split(';');
    let authserv = parts.next().unwrap_or("");
    // An authserv-id may be followed by a version number.
    if !authserv
        .split_whitespace()
        .next()
        .is_some_and(|id| id.eq_ignore_ascii_case(PROTON_AUTHSERV_ID))
    {
        return false;
    }
    let Some(result) = parts.next() else {
        return false;
    };
    let result = result.trim().to_ascii_lowercase();
    let Some(rest) = result.strip_prefix("dmarc=") else {
        return false;
    };
    let verdict = rest
        .split(|c: char| c.is_whitespace() || c == '(')
        .next()
        .unwrap_or("");
    verdict == "pass"
        && result
            .split_whitespace()
            .find_map(|t| t.strip_prefix("header.from="))
            .is_some_and(|d| !from_domain.is_empty() && d == from_domain.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROTON_DMARC: &str =
        "mail.protonmail.ch; dmarc=pass (p=reject dis=none) header.from=example.com";

    fn policy() -> ProtonPolicy {
        ProtonPolicy::new(
            "Me_Aivyx@proton.me",
            &["me@proton.me".to_string(), "me_aivyx@proton.me".to_string()],
            &["Friend@Example.com".to_string()],
        )
    }

    fn mail(from: &str) -> MailMessage {
        MailMessage {
            uid: 1,
            from: Some(from.into()),
            delivered_to: vec!["me_aivyx@proton.me".into()],
            ..Default::default()
        }
    }

    fn external(auth_results: &[&str]) -> MailMessage {
        MailMessage {
            auth_results: auth_results.iter().map(|s| s.to_string()).collect(),
            ..mail("friend@example.com")
        }
    }

    #[test]
    fn mail_from_you_is_a_turn_only_when_this_account_sent_it() {
        let m = mail("me@proton.me");
        assert!(policy().claims_operator(&m));
        assert_eq!(decide(&policy(), &m, true), Decision::Turn(Sender::Operator));
        // A forged "From: you" from outside is never in your Sent folder.
        assert!(matches!(decide(&policy(), &m, false), Decision::Ignore(_)));
    }

    #[test]
    fn mail_from_you_may_be_addressed_by_to_or_cc() {
        let mut m = MailMessage {
            delivered_to: vec![],
            to: vec!["me_aivyx@proton.me".into()],
            ..mail("me@proton.me")
        };
        assert_eq!(decide(&policy(), &m, true), Decision::Turn(Sender::Operator));
        m.to = vec!["someone@else.com".into()];
        assert!(matches!(decide(&policy(), &m, true), Decision::Ignore(_)));
        m.cc = vec!["me_aivyx@proton.me".into()];
        assert_eq!(decide(&policy(), &m, true), Decision::Turn(Sender::Operator));
    }

    #[test]
    fn an_allowlisted_sender_needs_protons_own_dmarc_pass() {
        let p = policy();
        assert!(!p.claims_operator(&external(&[])));
        assert_eq!(
            decide(&p, &external(&[PROTON_DMARC, "mail.protonmail.ch; spf=pass"]), false),
            Decision::Turn(Sender::External)
        );
        // No Proton result at all.
        assert!(matches!(decide(&p, &external(&[]), false), Decision::Ignore(_)));
        // DMARC fail, or for another domain.
        assert!(matches!(
            decide(&p, &external(&["mail.protonmail.ch; dmarc=fail header.from=example.com"]), false),
            Decision::Ignore(_)
        ));
        assert!(matches!(
            decide(&p, &external(&["mail.protonmail.ch; dmarc=pass header.from=evil.com"]), false),
            Decision::Ignore(_)
        ));
        // A forged pass below Proton's own results is never first.
        assert!(matches!(
            decide(
                &p,
                &external(&[
                    "mail.protonmail.ch; spf=pass smtp.mailfrom=example.com",
                    "mail.protonmail.ch; dmarc=pass header.from=example.com",
                ]),
                false
            ),
            Decision::Ignore(_)
        ));
        // Another server's stamp.
        assert!(matches!(
            decide(&p, &external(&["mx.evil.com; dmarc=pass header.from=example.com"]), false),
            Decision::Ignore(_)
        ));
    }

    #[test]
    fn external_mail_must_be_delivered_to_the_channel_address() {
        let mut m = external(&[PROTON_DMARC]);
        m.delivered_to = vec!["me@proton.me".into()];
        m.to = vec!["me_aivyx@proton.me".into()];
        assert!(matches!(decide(&policy(), &m, false), Decision::Ignore(_)));
    }

    #[test]
    fn strangers_loops_and_automatic_mail_are_ignored() {
        let p = policy();
        assert!(matches!(decide(&p, &mail("stranger@x.com"), false), Decision::Ignore(_)));
        // The assistant's own replies (from the channel address).
        let own = mail("me_aivyx@proton.me");
        assert!(!p.claims_operator(&own));
        assert!(matches!(decide(&p, &own, true), Decision::Ignore(_)));
        let mut m = mail("me@proton.me");
        m.auto_submitted = Some("auto-replied".into());
        assert!(matches!(decide(&p, &m, true), Decision::Ignore(_)));
        m.auto_submitted = Some("no".into());
        assert_eq!(decide(&p, &m, true), Decision::Turn(Sender::Operator));
        m.precedence = Some("List".into());
        assert!(matches!(decide(&p, &m, true), Decision::Ignore(_)));
        let no_from = MailMessage { from: None, ..mail("x@y") };
        assert!(matches!(decide(&p, &no_from, true), Decision::Ignore(_)));
    }

    #[test]
    fn the_recorded_proton_headers_pass_for_their_domain() {
        let raw = include_bytes!("../tests/fixtures/proton-external-dmarc-pass.headers");
        let mut full = raw.to_vec();
        full.extend_from_slice(b"From: x@newsletter.example\r\n\r\nhi\r\n");
        let m = crate::message::parse(1, &full).unwrap();
        assert!(proton_dmarc_passes(&m.auth_results, "newsletter.example"));
        assert!(proton_dmarc_passes(&m.auth_results, "NewsLetter.example"));
        assert!(!proton_dmarc_passes(&m.auth_results, "bounce.newsletter.example"));
    }
}
