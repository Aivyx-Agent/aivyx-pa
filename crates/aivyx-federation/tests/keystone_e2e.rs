//! Chapter Passport PP.6 — the keystone integration proof.
//!
//! Two operator-owned identities, in process, exercising the **whole** receive
//! pipeline end to end: a peer signs a relay request → I verify it (PP.1) →
//! guard against replay (PP.1) → resolve per-peer trust + attenuate (PP.2) →
//! bind it as untrusted, provenance-tagged content (PP.4) → classify operator
//! consent (PP.5) → shape the crossing for audit (PP.3).
//!
//! No transport and no second machine — that is the deferred Nexus layer. This
//! proves the *substrate* composes: a peer is structurally bounded by the
//! intersection of both operators' authority, forged/replayed requests are
//! rejected, deny-by-default holds, revocation narrows reach immediately, and
//! peer-initiated effect is gated.

use aivyx_capability::{CapabilitySet, Scope};
use aivyx_config::AutonomyLevel;
use aivyx_federation::consent::{ConsentRequirement, consent_for};
use aivyx_federation::identity::{Identity, ReplayGuard, SignedHeader};
use aivyx_federation::received::{PeerContent, Provenance};
use aivyx_federation::relay::{Crossing, CrossingOutcome};
use aivyx_federation::trust::{TrustPolicy, effective_authority};
use aivyx_ipc::RelayRequest;

fn scope(s: &str) -> Scope {
    Scope::parse(s).expect("known base")
}
fn set(scopes: &[&str]) -> CapabilitySet {
    CapabilitySet::from_scopes(scopes.iter().map(|s| scope(s)))
}

/// This host's instance id: what peers sign their requests for.
const HOST_ID: &str = "host";

/// What handling a crossing's verb would need on the host — host-derived, never
/// peer-supplied (a peer asks for *work*, the host decides what scopes that work
/// touches; this is itself part of no-self-escalation).
fn host_scopes_for(req: &RelayRequest) -> Vec<Scope> {
    match req {
        RelayRequest::Chat { .. } => vec![],                       // pure dialogue
        RelayRequest::Search { .. } => vec![scope("memory.read")], // read shareable knowledge
        RelayRequest::Task { .. } => vec![scope("memory.read"), scope("memory.write")],
    }
}

/// The host's full receive pipeline. `Ok` = accepted (with its consent
/// requirement); `Err` = a refused [`Crossing`] (bad sig / replay / invalid).
#[allow(clippy::too_many_arguments)]
fn receive(
    peer_id: &str,
    peer_pubkey: &str,
    header: &SignedHeader,
    body: &[u8],
    req: RelayRequest,
    guard: &ReplayGuard,
    policy: Option<&TrustPolicy>,
    host_ceiling: &CapabilitySet,
    autonomy_cap: AutonomyLevel,
) -> Result<(PeerContent, ConsentRequirement), Crossing> {
    let verb = req.verb();
    // PP.1 — identity: verify the signature against the peer's known key.
    Identity::verify_request(peer_pubkey, HOST_ID, header, body).map_err(|e| {
        Crossing::inbound_denied(peer_id, verb, format!("signature: {e}"))
    })?;
    // PP.1 — replay guard.
    guard
        .check_and_record(header)
        .map_err(|e| Crossing::inbound_denied(peer_id, verb, format!("replay: {e}")))?;
    // PP.2 — resolve per-peer trust + attenuate (host decides scope needs).
    let asked = host_scopes_for(&req);
    let authority = effective_authority(&asked, policy, host_ceiling, autonomy_cap);
    // PP.4 — bind as untrusted, provenance-tagged content (validates payload).
    let provenance = Provenance {
        peer_id: peer_id.to_string(),
        peer_public_key: peer_pubkey.to_string(),
    };
    let content = PeerContent::accept(provenance, authority, req)
        .map_err(|e| Crossing::inbound_denied(peer_id, verb, format!("invalid: {e}")))?;
    // PP.5 — classify operator consent.
    let consent = consent_for(&content);
    Ok((content, consent))
}

/// Happy path: a trusted peer's `search` verifies, attenuates to its policy, and
/// runs read-only without a gate.
#[tokio::test]
async fn trusted_search_flows_end_to_end() {
    let peer = Identity::generate("peer".into()).unwrap();
    let host_ceiling = set(&["memory.read", "memory.write"]);
    let policy = TrustPolicy::new([scope("memory.read")]);
    let guard = ReplayGuard::new();

    let req = RelayRequest::Search { query: "pour-over bloom".into() };
    let (header, body) = peer.sign_relay(HOST_ID, &req).await.unwrap();

    let (content, consent) = receive(
        "peer",
        &peer.public_key_base64(),
        &header,
        &body,
        req,
        &guard,
        Some(&policy),
        &host_ceiling,
        AutonomyLevel::Assisted,
    )
    .expect("a faithful, trusted crossing is accepted");

    assert!(!content.is_denied());
    assert!(content.may(&scope("memory.read")));
    assert!(!content.may(&scope("memory.write")), "search never reaches write");
    assert_eq!(consent, ConsentRequirement::NotRequired);
}

/// A peer's `task` is effect — accepted, attenuated, but gated on operator
/// consent. And the peer can never borrow the host's broader caps.
#[tokio::test]
async fn task_is_attenuated_and_gated() {
    let peer = Identity::generate("peer".into()).unwrap();
    // host could do fs.write; the peer's policy does not include it.
    let host_ceiling = set(&["memory.read", "memory.write", "fs.write"]);
    let policy = TrustPolicy::new([scope("memory.read")]);
    let guard = ReplayGuard::new();

    let req = RelayRequest::Task { goal: "update my notes".into() };
    let (header, body) = peer.sign_relay(HOST_ID, &req).await.unwrap();

    let (content, consent) = receive(
        "peer",
        &peer.public_key_base64(),
        &header,
        &body,
        req,
        &guard,
        Some(&policy),
        &host_ceiling,
        AutonomyLevel::Autonomous, // even a permissive host cap...
    )
    .expect("accepted as untrusted content");

    // attenuation: memory.read granted; memory.write asked-but-not-in-policy;
    // fs.write never asked nor in policy — the peer can't borrow the host's caps.
    assert!(content.may(&scope("memory.read")));
    assert!(!content.may(&scope("memory.write")));
    assert!(!content.may(&scope("fs.write")));
    // ...a peer task still requires the operator's gate (no self-escalation).
    assert!(matches!(consent, ConsentRequirement::OperatorConsent { .. }));
}

/// Deny-by-default: a peer with no `TrustPolicy` is verified but granted
/// nothing, and its request is refused at consent.
#[tokio::test]
async fn unknown_peer_is_denied_by_default() {
    let peer = Identity::generate("stranger".into()).unwrap();
    let host_ceiling = set(&["memory.read", "memory.write"]);
    let guard = ReplayGuard::new();

    let req = RelayRequest::Task { goal: "do something".into() };
    let (header, body) = peer.sign_relay(HOST_ID, &req).await.unwrap();

    let (content, consent) = receive(
        "stranger",
        &peer.public_key_base64(),
        &header,
        &body,
        req,
        &guard,
        None, // no TrustPolicy
        &host_ceiling,
        AutonomyLevel::Assisted,
    )
    .expect("a valid signature is accepted; trust is what denies");

    assert!(content.is_denied());
    assert!(!content.may(&scope("memory.read")));
    assert_eq!(consent, ConsentRequirement::Denied);
}

/// A forged body under a valid header is rejected at signature verification —
/// the crossing is refused, never reaching trust resolution.
#[tokio::test]
async fn forged_body_is_rejected() {
    let peer = Identity::generate("peer".into()).unwrap();
    let host_ceiling = set(&["memory.read"]);
    let policy = TrustPolicy::new([scope("memory.read")]);
    let guard = ReplayGuard::new();

    let (header, _body) = peer
        .sign_relay(HOST_ID, &RelayRequest::Chat { message: "hello".into() })
        .await
        .unwrap();
    // attacker swaps the body for a Task under the same (valid) header
    let forged_req = RelayRequest::Task { goal: "exfiltrate".into() };
    let forged_body = aivyx_federation::relay::relay_body_bytes(&forged_req).unwrap();

    let refused = receive(
        "peer",
        &peer.public_key_base64(),
        &header,
        &forged_body,
        forged_req,
        &guard,
        Some(&policy),
        &host_ceiling,
        AutonomyLevel::Assisted,
    )
    .expect_err("a forged body must be refused");
    assert!(matches!(refused.outcome, CrossingOutcome::Denied { .. }));
}

/// A replayed request (same signed header re-sent) is rejected by the guard on
/// the second arrival.
#[tokio::test]
async fn replayed_request_is_rejected() {
    let peer = Identity::generate("peer".into()).unwrap();
    let host_ceiling = set(&["memory.read"]);
    let policy = TrustPolicy::new([scope("memory.read")]);
    let guard = ReplayGuard::new();

    let req = RelayRequest::Search { query: "x".into() };
    let (header, body) = peer.sign_relay(HOST_ID, &req).await.unwrap();
    let pk = peer.public_key_base64();

    // first arrival: accepted
    receive("peer", &pk, &header, &body, req.clone(), &guard, Some(&policy), &host_ceiling, AutonomyLevel::Assisted)
        .expect("first arrival is accepted");
    // second arrival of the SAME header: rejected as a replay
    let refused = receive("peer", &pk, &header, &body, req, &guard, Some(&policy), &host_ceiling, AutonomyLevel::Assisted)
        .expect_err("a replay must be refused");
    match refused.outcome {
        CrossingOutcome::Denied { reason } => assert!(reason.contains("replay")),
        _ => panic!("expected a replay denial"),
    }
}

/// Verifying a peer's request against the *wrong* peer's key fails — identity is
/// bound to the key, not the claimed instance id.
#[tokio::test]
async fn wrong_peer_key_is_rejected() {
    let alice = Identity::generate("alice".into()).unwrap();
    let mallory = Identity::generate("mallory".into()).unwrap();
    let host_ceiling = set(&["memory.read"]);
    let policy = TrustPolicy::new([scope("memory.read")]);
    let guard = ReplayGuard::new();

    let req = RelayRequest::Search { query: "x".into() };
    let (header, body) = alice.sign_relay(HOST_ID, &req).await.unwrap();

    // host looks up "alice" but is given mallory's key → verification fails
    let refused = receive(
        "alice",
        &mallory.public_key_base64(),
        &header,
        &body,
        req,
        &guard,
        Some(&policy),
        &host_ceiling,
        AutonomyLevel::Assisted,
    )
    .expect_err("a mismatched key must be refused");
    assert!(matches!(refused.outcome, CrossingOutcome::Denied { .. }));
}

/// Revocation narrows reach immediately: a broad policy grants write; tightening
/// it drops write on the very next crossing.
#[tokio::test]
async fn revocation_narrows_reach_on_the_next_crossing() {
    let peer = Identity::generate("peer".into()).unwrap();
    let host_ceiling = set(&["memory.read", "memory.write"]);

    async fn make(peer: &Identity) -> (RelayRequest, SignedHeader, Vec<u8>) {
        let req = RelayRequest::Task { goal: "write notes".into() };
        let (header, body) = peer.sign_relay(HOST_ID, &req).await.unwrap();
        (req, header, body)
    }

    // broad policy: write is in reach (a Task asks memory.read + memory.write)
    let broad = TrustPolicy::new([scope("memory.read"), scope("memory.write")]);
    let (req, header, body) = make(&peer).await;
    let guard1 = ReplayGuard::new();
    let (before, _) = receive("peer", &peer.public_key_base64(), &header, &body, req, &guard1, Some(&broad), &host_ceiling, AutonomyLevel::Assisted).unwrap();
    assert!(before.may(&scope("memory.write")));

    // tighten the policy → write gone on the next request
    let tightened = TrustPolicy::new([scope("memory.read")]);
    let (req2, header2, body2) = make(&peer).await;
    let guard2 = ReplayGuard::new();
    let (after, _) = receive("peer", &peer.public_key_base64(), &header2, &body2, req2, &guard2, Some(&tightened), &host_ceiling, AutonomyLevel::Assisted).unwrap();
    assert!(after.may(&scope("memory.read")));
    assert!(!after.may(&scope("memory.write")), "revocation took effect immediately");
}
