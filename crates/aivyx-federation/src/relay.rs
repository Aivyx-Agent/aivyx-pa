//! Relay composition + the auditable crossing shape (FED.3 / Chapter Passport PP.3).
//!
//! The wire **verbs** live in the wasm-clean [`aivyx_ipc::federation`]
//! ([`RelayRequest`] / `RelayResponse`); this module composes them with the
//! [`Identity`] signing envelope (PP.1) into a signed frame, and defines the
//! forensic **shape** of a crossing for the audit chain (FED.0 §6).
//!
//! What is **not** here (deferred): a transport / relay server, and the live
//! `AuditEvent::FederationCrossing` emission. The audit chain has established
//! forward-compat patterns (serde-default fields), so its *emission* can land
//! when a crossing actually executes (with transport); the irreversible part —
//! the wire protocol — is locked now. [`Crossing`] pins the audit *fields* so
//! that emission is a mechanical wiring step, not a redesign.

use aivyx_ipc::RelayRequest;

use crate::FederationError;
use crate::identity::{Identity, SignedHeader};

/// Which way a crossing went, from *my* instance's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrossingDirection {
    /// A peer's request arriving at my instance.
    Inbound,
    /// My instance's request leaving for a peer.
    Outbound,
}

/// What happened to a crossing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CrossingOutcome {
    /// Relayed under the FED.2-computed authority (the intersection).
    Relayed,
    /// Refused — deny-by-default, outside the peer's `TrustPolicy`, or a failed
    /// signature / replay check. `reason` is operator-readable.
    Denied { reason: String },
}

/// The forensic shape of one cross-boundary crossing — the fields a future
/// `AuditEvent::FederationCrossing` will carry (FED.0 §6: *"every relayed
/// request, response, grant, and refusal is an `AuditEvent` carrying the peer's
/// identity"*). Defined now so the audit **shape** is locked with the substrate;
/// the emission onto the HMAC chain is wired when crossings execute. Carries the
/// peer id + verb + outcome — **never key material or private data**.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crossing {
    /// The peer's `instance_id` (its public-key identity is the trust anchor).
    pub peer_id: String,
    /// Inbound (peer → me) or outbound (me → peer).
    pub direction: CrossingDirection,
    /// The relay verb (`"chat"` / `"task"` / `"search"`).
    pub verb: &'static str,
    /// Relayed or denied (with reason).
    pub outcome: CrossingOutcome,
}

impl Crossing {
    /// An inbound crossing that was refused — the deny-by-default / out-of-policy
    /// / bad-signature case.
    pub fn inbound_denied(peer_id: impl Into<String>, verb: &'static str, reason: impl Into<String>) -> Self {
        Self {
            peer_id: peer_id.into(),
            direction: CrossingDirection::Inbound,
            verb,
            outcome: CrossingOutcome::Denied { reason: reason.into() },
        }
    }
}

/// The canonical bytes a [`RelayRequest`] is signed over. The **same bytes**
/// travel with the [`SignedHeader`] and are re-hashed by the receiver, so
/// verification never depends on re-serializing identically.
pub fn relay_body_bytes(req: &RelayRequest) -> Result<Vec<u8>, FederationError> {
    serde_json::to_vec(req)
        .map_err(|e| FederationError::Validation(format!("serialize relay request: {e}")))
}

impl Identity {
    /// Sign a relay request, returning the [`SignedHeader`] **and the exact body
    /// bytes that were signed**. Send both to the peer; it verifies the header
    /// against these bytes with [`Identity::verify_request`]. Pair the receive
    /// side with a [`ReplayGuard`](crate::identity::ReplayGuard).
    ///
    /// `recipient` is the receiving peer's instance id (signed in, so the frame
    /// verifies only at that peer).
    ///
    /// Async because [`Identity::sign_request`] is (the hardware-backed path
    /// can block for seconds waiting on a physical touch).
    pub async fn sign_relay(
        &self,
        recipient: &str,
        req: &RelayRequest,
    ) -> Result<(SignedHeader, Vec<u8>), FederationError> {
        let body = relay_body_bytes(req)?;
        let header = self.sign_request(recipient, &body).await?;
        Ok((header, body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn signed_relay_verifies_at_the_peer() {
        let me = Identity::generate("me".into()).unwrap();
        let req = RelayRequest::Task { goal: "summarize the docs".into() };
        let (header, body) = me.sign_relay("peer", &req).await.unwrap();
        // the peer knows my public key and verifies the frame
        Identity::verify_request(&me.public_key_base64(), "peer", &header, &body)
            .expect("a faithfully relayed frame verifies");
    }

    #[tokio::test]
    async fn tampered_relay_body_fails_verification() {
        let me = Identity::generate("me".into()).unwrap();
        let (header, _body) =
            me.sign_relay("peer", &RelayRequest::Chat { message: "hi".into() }).await.unwrap();
        // an attacker swaps the body for a different verb under the same header
        let forged = relay_body_bytes(&RelayRequest::Task { goal: "exfiltrate".into() }).unwrap();
        assert!(Identity::verify_request(&me.public_key_base64(), "peer", &header, &forged).is_err());
    }

    #[test]
    fn crossing_records_a_denial_with_reason() {
        let c = Crossing::inbound_denied("stranger", "task", "no TrustPolicy (deny-by-default)");
        assert_eq!(c.direction, CrossingDirection::Inbound);
        assert_eq!(c.verb, "task");
        match c.outcome {
            CrossingOutcome::Denied { reason } => assert!(reason.contains("deny-by-default")),
            _ => panic!("expected a denial"),
        }
    }
}
