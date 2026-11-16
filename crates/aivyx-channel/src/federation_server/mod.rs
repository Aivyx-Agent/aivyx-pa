//! A19 (FED.3) — the inbound federation listener.
//!
//! A paired peer (another of the operator's own instances) connects over a
//! Noise `XX` [`Link`], sends one signed [`RequestFrame`] and gets one
//! [`ResponseFrame`]. Each request is checked in this order: the transport
//! key belongs to a configured peer, the Ed25519 signature verifies for
//! *this* instance, the signer is that peer, the frame is not a replay, and
//! the verb is one the operator allowed for the peer. Then:
//!
//! - **chat** runs one turn with an empty capability ceiling (no tools);
//! - **search** runs no turn: it returns the matching learned skills'
//!   `name`, `trigger` and `procedure`, nothing else;
//! - **task** needs the operator's up-front approval (a *waiting* parked
//!   step), then runs one turn under the peer's effective authority, where
//!   the turn's own gates park further waiting steps.
//!
//! Every turn is a Peer-origin message (no recall, persona or skill
//! injection) on a fresh session, so no history is shared between requests
//! or with the operator. Every request, reply and refusal writes one
//! [`AuditEvent::FederationCrossing`] with a fixed, payload-free reason; a
//! ping (an empty frame) writes none.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use aivyx_audit::{AuditEvent, AuditWriter, PersistentAuditLog};
use aivyx_capability::{CapabilitySet, Scope, TrustTier};
use aivyx_config::{AutonomyLevel, FederationConfig, FederationVerb};
use aivyx_core::{
    Agent, Approval, ApprovalRequest, CancellationToken, ChannelContext, ChannelError, ChannelPlatform,
    Message, SessionId, StreamEvent, TurnOutcome,
};
use aivyx_federation::consent::{ConsentRequirement, consent_for};
use aivyx_federation::identity::{Identity, ReplayGuard};
use aivyx_federation::received::{PeerContent, Provenance};
use aivyx_federation::relay::{Crossing, CrossingDirection, CrossingOutcome};
use aivyx_federation::trust::{TrustPolicy, effective_authority};
use aivyx_federation_net::{Link, NetError, PeerCard, RequestFrame, ResponseFrame, TransportKey};
use aivyx_ipc::federation::{RelayRequest, RelayResponse};
use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;

use crate::parked_steps::{StepParker, TIMED_OUT_RESULT};
use crate::persona::SharedEffectivePersona;

/// At most this many connections are served at once; more are dropped.
pub const MAX_CONNECTIONS: usize = 16;
/// At most this many of them from one peer.
pub const MAX_CONNECTIONS_PER_PEER: usize = 4;
/// How long a connected peer has to send its request frame.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a turn gets to stop after its deadline cancels it.
const CANCEL_GRACE: Duration = Duration::from_secs(5);
/// Most skills one search returns.
const MAX_SEARCH_RESULTS: usize = 20;
/// An unknown transport key's crossing is written once per this window per
/// key, so a stranger retrying in a loop doesn't fill the audit chain.
const UNKNOWN_KEY_QUIET: Duration = Duration::from_secs(600);
/// At most this many unknown keys are remembered; past it, expired entries
/// are dropped and, if none had expired, every crossing is written again.
const MAX_UNKNOWN_KEYS: usize = 1024;
/// Most encoded bytes of search results in one reply, well under `MAX_FRAME`.
const MAX_SEARCH_BYTES: usize = 512 * 1024;
/// Characters of a task's goal shown in its approval summary.
const SUMMARY_GOAL_CHARS: usize = 200;

// Fixed crossing reasons and reply texts. None may ever carry payload text.
const UNKNOWN_KEY: &str = "unknown transport key";
const BAD_FRAME: &str = "malformed request frame";
const BAD_SIGNATURE: &str = "the request signature did not verify or the request expired";
const WRONG_SIGNER: &str = "the request was signed under another instance id";
const REPLAYED: &str = "replayed request";
const BAD_REQUEST: &str = "malformed relay request";
const VERB_NOT_ALLOWED: &str = "verb not allowed for this peer";
const FIELD_TOO_LONG: &str = "a request field is too long";
const BUSY: &str = "busy";
const REFUSED: &str = "refused by the operator";
const NO_SCOPES: &str = "no scopes are granted to this peer for tasks";
const NOT_QUEUED: &str = "the approval could not be queued";
const OVER_TIME: &str = "the request ran past its time limit";
const NO_READABLE_REQUEST: &str = "no readable request";
const REPLY_NOT_SENT: &str = "reply could not be sent";
const TOO_MANY_CONNECTIONS: &str = "too many connections from this peer";

/// Every reason this listener ever replies with. The outbound side records
/// a peer's reason only when it is one of these, so a peer can never put
/// text of its own into the requester's audit chain.
pub(crate) const REPLY_REASONS: &[&str] = &[
    BAD_FRAME,
    BAD_SIGNATURE,
    WRONG_SIGNER,
    REPLAYED,
    BAD_REQUEST,
    VERB_NOT_ALLOWED,
    FIELD_TOO_LONG,
    BUSY,
    REFUSED,
    NO_SCOPES,
    NOT_QUEUED,
    OVER_TIME,
    TIMED_OUT_RESULT,
];

/// A configured peer whose card verified.
#[derive(Debug, Clone)]
pub struct FederationPeer {
    pub id: String,
    pub card: PeerCard,
    /// The X25519 transport key the card's identity vouches for.
    pub x25519: [u8; 32],
    pub address: String,
    pub verbs: Vec<FederationVerb>,
    /// `allowed_scopes` + the autonomy ceiling, as the operator wrote them.
    pub policy: TrustPolicy,
    pub task_timeout: Duration,
}

/// Verify every configured peer's card. A card that does not verify, or
/// whose instance id is not the peer's configured id, is an error.
pub fn load_peers(cfg: &FederationConfig) -> Result<Vec<FederationPeer>, String> {
    let peers = cfg
        .peers
        .iter()
        .map(|p| {
            let card = PeerCard::decode(&p.card).map_err(|e| format!("federation peer `{}`: {e}", p.id))?;
            let x25519 = card.verify().map_err(|e| format!("federation peer `{}`: {e}", p.id))?;
            if card.instance_id != p.id {
                return Err(format!(
                    "federation peer `{}`: the card is for the instance `{}`",
                    p.id, card.instance_id
                ));
            }
            Ok(FederationPeer {
                id: p.id.clone(),
                card,
                x25519,
                address: p.address.clone(),
                verbs: p.verbs.clone(),
                policy: TrustPolicy::new(p.allowed_scopes.iter().cloned()).with_ceiling(p.ceiling),
                task_timeout: p.task_timeout,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    // Two peers on one key would make the key-to-peer lookup ambiguous.
    for (i, a) in peers.iter().enumerate() {
        for b in &peers[i + 1..] {
            if a.x25519 == b.x25519 {
                return Err(format!("federation peers `{}` and `{}` share a transport key", a.id, b.id));
            }
            if a.card.ed25519_public == b.card.ed25519_public {
                return Err(format!("federation peers `{}` and `{}` share an identity key", a.id, b.id));
            }
        }
    }
    Ok(peers)
}

/// Everything the listener needs.
pub struct FederationServe {
    pub listen: SocketAddr,
    pub identity: Arc<Identity>,
    pub key: Arc<TransportKey>,
    pub peers: Arc<Vec<FederationPeer>>,
    pub agent: Arc<dyn Agent>,
    pub parker: Arc<StepParker>,
    pub persona: SharedEffectivePersona,
    pub audit: Arc<PersistentAuditLog>,
    /// A19 final review I2 — this instance doesn't ask before deletes and
    /// overwrites in some area (an `unleashed` level or override, or
    /// `[access] confirm_destructive = false`). A peer's `assisted` ceiling
    /// defers to those gates, so with this set every peer task runs as
    /// `manual`: every action asks.
    pub assisted_as_manual: bool,
    /// Tests only: a listener already bound to `listen`, served instead of
    /// binding again (binding a released port again races other tests).
    #[cfg(test)]
    pub(crate) prebound: Option<TcpListener>,
}

/// The audit event for a crossing. Carries ids, verb and a fixed reason only.
pub fn crossing_event(c: &Crossing) -> AuditEvent {
    let (outcome, reason) = match &c.outcome {
        CrossingOutcome::Relayed => ("relayed", String::new()),
        CrossingOutcome::Denied { reason } => ("denied", reason.clone()),
        CrossingOutcome::Abandoned { reason } => ("abandoned", reason.clone()),
    };
    AuditEvent::FederationCrossing {
        peer_id: c.peer_id.clone(),
        direction: match c.direction {
            CrossingDirection::Inbound => "inbound",
            CrossingDirection::Outbound => "outbound",
        }
        .into(),
        verb: c.verb.into(),
        outcome: outcome.into(),
        reason,
    }
}

fn record(audit: &PersistentAuditLog, crossing: &Crossing) {
    if let Err(e) = audit.append(crossing_event(crossing)) {
        eprintln!("aivyx-pa federation: couldn't audit a crossing: {e}");
    }
}

/// Counted per-peer slots: at most `limit` held per peer at once.
pub(crate) struct PeerSlots {
    held: Arc<Mutex<HashMap<String, usize>>>,
    limit: usize,
}

/// Holds one of a peer's slots; releases it when dropped, on every path.
pub(crate) struct SlotGuard {
    held: Arc<Mutex<HashMap<String, usize>>>,
    peer: String,
}

impl PeerSlots {
    pub(crate) fn new(limit: usize) -> Self {
        PeerSlots { held: Arc::default(), limit }
    }

    pub(crate) fn try_take(&self, peer: &str) -> Option<SlotGuard> {
        let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        let n = held.entry(peer.to_string()).or_insert(0);
        if *n >= self.limit {
            return None;
        }
        *n += 1;
        Some(SlotGuard { held: Arc::clone(&self.held), peer: peer.to_string() })
    }
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        let mut held = self.held.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = held.get_mut(&self.peer) {
            *n -= 1;
            if *n == 0 {
                held.remove(&self.peer);
            }
        }
    }
}

/// State one listener shares across its connections.
pub(crate) struct ListenerState {
    replay: ReplayGuard,
    /// One running task per peer.
    busy: PeerSlots,
    /// At most [`MAX_CONNECTIONS_PER_PEER`] open connections per peer.
    connections: PeerSlots,
    /// Fires on daemon shutdown; every request's cancel token is a child.
    shutdown: CancellationToken,
    /// When each unknown key's crossing was last written.
    unknown_seen: Mutex<HashMap<String, Instant>>,
}

impl ListenerState {
    pub(crate) fn new(shutdown: CancellationToken) -> Self {
        ListenerState {
            replay: ReplayGuard::new(),
            busy: PeerSlots::new(1),
            connections: PeerSlots::new(MAX_CONNECTIONS_PER_PEER),
            shutdown,
            unknown_seen: Mutex::new(HashMap::new()),
        }
    }

    /// Whether to write a crossing for this unknown key now: the first time,
    /// then once per [`UNKNOWN_KEY_QUIET`].
    fn should_record_unknown(&self, fingerprint: &str) -> bool {
        let now = Instant::now();
        let mut seen = self.unknown_seen.lock().unwrap_or_else(|e| e.into_inner());
        if seen.get(fingerprint).is_some_and(|at| now.duration_since(*at) < UNKNOWN_KEY_QUIET) {
            return false;
        }
        if seen.len() >= MAX_UNKNOWN_KEYS {
            seen.retain(|_, at| now.duration_since(*at) < UNKNOWN_KEY_QUIET);
        }
        if seen.len() < MAX_UNKNOWN_KEYS {
            seen.insert(fingerprint.to_string(), now);
        }
        true
    }
}

/// Bind `serve.listen` and serve until `shutdown` fires.
pub async fn run_federation_listener(serve: FederationServe, shutdown: CancellationToken) -> Result<(), String> {
    #[cfg(test)]
    let mut serve = serve;
    #[cfg(test)]
    if let Some(listener) = serve.prebound.take() {
        return serve_listener(listener, serve, shutdown).await;
    }
    let listener = TcpListener::bind(serve.listen)
        .await
        .map_err(|e| format!("couldn't listen on {}: {e}", serve.listen))?;
    eprintln!("aivyx-pa federation: listening on {}", serve.listen);
    serve_listener(listener, serve, shutdown).await
}

/// Serve an already-bound listener (tests bind port 0).
pub(crate) async fn serve_listener(
    listener: TcpListener,
    serve: FederationServe,
    shutdown: CancellationToken,
) -> Result<(), String> {
    let serve = Arc::new(serve);
    let state = Arc::new(ListenerState::new(shutdown.clone()));
    let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    loop {
        tokio::select! {
            _ = shutdown.cancelled() => return Ok(()),
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => match Arc::clone(&slots).try_acquire_owned() {
                    Ok(permit) => {
                        let (serve, state) = (Arc::clone(&serve), Arc::clone(&state));
                        tokio::spawn(async move {
                            handle_connection(&serve, stream, &state).await;
                            drop(permit);
                        });
                    }
                    Err(_) => drop(stream),
                },
                Err(e) => {
                    eprintln!("aivyx-pa federation: accept failed: {e}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            },
        }
    }
}

/// Serve one connection: handshake, one request, one reply. Every request,
/// reply and refusal is audited; `relayed` only once the reply was sent.
pub(crate) async fn handle_connection(serve: &FederationServe, stream: TcpStream, state: &ListenerState) {
    let peers = Arc::clone(&serve.peers);
    let mut link = match Link::accept(stream, &serve.key, |k| peers.iter().any(|p| &p.x25519 == k)).await {
        Ok(link) => link,
        Err(NetError::UnknownPeer { fingerprint }) => {
            if state.should_record_unknown(&fingerprint) {
                record(&serve.audit, &Crossing::inbound_denied(format!("unknown:{fingerprint}"), "-", UNKNOWN_KEY));
            }
            return;
        }
        Err(e) => {
            eprintln!("aivyx-pa federation: handshake failed: {e}");
            return;
        }
    };
    let remote = link.remote_static();
    let Some(peer) = serve.peers.iter().find(|p| p.x25519 == remote) else {
        return;
    };
    let Some(_connection) = state.connections.try_take(&peer.id) else {
        record(&serve.audit, &Crossing::inbound_denied(peer.id.clone(), "-", TOO_MANY_CONNECTIONS));
        return;
    };
    let raw = match tokio::time::timeout(REQUEST_TIMEOUT, link.recv()).await {
        Ok(Ok(raw)) => raw,
        Ok(Err(e)) => {
            eprintln!("aivyx-pa federation: peer `{}` sent no readable request: {e}", peer.id);
            record(&serve.audit, &Crossing::inbound_denied(peer.id.clone(), "-", NO_READABLE_REQUEST));
            return;
        }
        Err(_) => {
            eprintln!("aivyx-pa federation: peer `{}` sent no request in time", peer.id);
            record(&serve.audit, &Crossing::inbound_denied(peer.id.clone(), "-", NO_READABLE_REQUEST));
            return;
        }
    };
    if raw.is_empty() {
        // A ping: proves connect, handshake and key match. Not a crossing.
        let _ = link.send(&[]).await;
        return;
    }
    let (verb, frame, mut denied) = serve_request(serve, state, peer, &raw).await;
    let sent = match serde_json::to_vec(&frame) {
        Ok(bytes) => link.send(&bytes).await.map_err(|e| e.to_string()),
        Err(e) => Err(e.to_string()),
    };
    if let Err(e) = sent {
        eprintln!("aivyx-pa federation: couldn't reply to `{}`: {e}", peer.id);
        // A refusal keeps its own reason; a reply that never left is not relayed.
        denied = denied.or(Some(REPLY_NOT_SENT));
    }
    let crossing = Crossing {
        peer_id: peer.id.clone(),
        direction: CrossingDirection::Inbound,
        verb,
        outcome: match denied {
            Some(reason) => CrossingOutcome::Denied { reason: reason.into() },
            None => CrossingOutcome::Relayed,
        },
    };
    record(&serve.audit, &crossing);
}

/// Every reason a peer is sent goes through [`denied`] or [`task_refused`],
/// and must be one of [`REPLY_REASONS`] (the requester records only those).
fn denied(verb: &'static str, reason: &'static str) -> (&'static str, ResponseFrame, Option<&'static str>) {
    debug_assert!(REPLY_REASONS.contains(&reason), "unlisted reply reason: {reason}");
    (verb, ResponseFrame::Denied { reason: reason.into() }, Some(reason))
}

fn task_refused(reason: &'static str) -> (&'static str, ResponseFrame, Option<&'static str>) {
    debug_assert!(REPLY_REASONS.contains(&reason), "unlisted reply reason: {reason}");
    let response = RelayResponse::Task { accepted: false, detail: reason.into() };
    ("task", ResponseFrame::Ok { response }, Some(reason))
}

/// Verify the frame, then run its verb. Returns `(verb, reply, denial reason)`.
async fn serve_request(
    serve: &FederationServe,
    state: &ListenerState,
    peer: &FederationPeer,
    raw: &[u8],
) -> (&'static str, ResponseFrame, Option<&'static str>) {
    let req = match verify(serve, state, peer, raw) {
        Ok(req) => req,
        Err(reason) => return denied("-", reason),
    };
    let verb = req.verb();
    let wanted = match &req {
        RelayRequest::Chat { .. } => FederationVerb::Chat,
        RelayRequest::Task { .. } => FederationVerb::Task,
        RelayRequest::Search { .. } => FederationVerb::Search,
    };
    if !peer.verbs.contains(&wanted) {
        return denied(verb, VERB_NOT_ALLOWED);
    }
    // Chat and search run no tools: they ask for nothing. A task asks for
    // everything the operator granted the peer; the intersection with the
    // Trusted ceiling and the peer's autonomy ceiling is the real limit.
    let asked: &[Scope] = if wanted == FederationVerb::Task { &peer.policy.allowed_scopes } else { &[] };
    let authority = effective_authority(
        asked,
        Some(&peer.policy),
        TrustTier::Trusted.default_ceiling(),
        peer.policy.peer_ceiling,
    );
    let provenance = Provenance { peer_id: peer.id.clone(), peer_public_key: peer.card.ed25519_public.clone() };
    let content = match PeerContent::accept(provenance, authority, req) {
        Ok(content) => content,
        Err(_) => return denied(verb, FIELD_TOO_LONG),
    };
    match content.request() {
        RelayRequest::Chat { message } => {
            let ctx = FederationContext::for_request(serve, state, peer, &content);
            match run_turn(&serve.agent, &ctx, message).await {
                Some(outcome) => {
                    let response = RelayResponse::Chat { reply: crate::trigger::render_notify_body(&outcome) };
                    (verb, ResponseFrame::Ok { response }, None)
                }
                None => denied(verb, OVER_TIME),
            }
        }
        RelayRequest::Search { query } => {
            let response = RelayResponse::Search { results: search_skills(&serve.persona, query) };
            (verb, ResponseFrame::Ok { response }, None)
        }
        RelayRequest::Task { goal } => run_task(serve, state, peer, &content, goal).await,
    }
}

/// The transport authenticated the peer; now the request itself must be
/// signed by that peer, for this instance, and new.
fn verify(
    serve: &FederationServe,
    state: &ListenerState,
    peer: &FederationPeer,
    raw: &[u8],
) -> Result<RelayRequest, &'static str> {
    let frame: RequestFrame = serde_json::from_slice(raw).map_err(|_| BAD_FRAME)?;
    let body = STANDARD.decode(&frame.body).map_err(|_| BAD_FRAME)?;
    Identity::verify_request(&peer.card.ed25519_public, serve.identity.instance_id(), &frame.header, &body)
        .map_err(|_| BAD_SIGNATURE)?;
    if frame.header.instance_id != peer.id {
        return Err(WRONG_SIGNER);
    }
    state.replay.check_and_record(&frame.header).map_err(|_| REPLAYED)?;
    serde_json::from_slice(&body).map_err(|_| BAD_REQUEST)
}

/// The privacy line for search: only a matching skill's name, trigger and
/// procedure cross, never other persona fields or skill metadata. Results
/// stop before their encoded total passes [`MAX_SEARCH_BYTES`].
fn search_skills(persona: &SharedEffectivePersona, query: &str) -> Vec<String> {
    let q = query.to_lowercase();
    let mut results = Vec::new();
    let mut total = 0;
    let matches = crate::skill_tool::current_skills(persona)
        .into_iter()
        .filter(|s| s.name.to_lowercase().contains(&q) || s.trigger.to_lowercase().contains(&q))
        .take(MAX_SEARCH_RESULTS);
    for s in matches {
        let json = serde_json::json!({ "name": s.name, "trigger": s.trigger, "procedure": s.procedure });
        let Ok(result) = serde_json::to_string(&json) else { continue };
        // Encoded again inside the reply's JSON array: quoted, escaped, comma.
        let size = serde_json::to_string(&result).map(|e| e.len() + 1).unwrap_or(usize::MAX);
        if total + size > MAX_SEARCH_BYTES {
            break;
        }
        total += size;
        results.push(result);
    }
    results
}

/// A task: one up-front approval, then one turn under the peer's authority.
/// One deadline (`task_timeout`) covers the approval and the turn.
async fn run_task(
    serve: &FederationServe,
    state: &ListenerState,
    peer: &FederationPeer,
    content: &PeerContent,
    goal: &str,
) -> (&'static str, ResponseFrame, Option<&'static str>) {
    let Some(_slot) = state.busy.try_take(&peer.id) else {
        return task_refused(BUSY);
    };
    // `consent_for` classifies every task as needing consent; it says
    // `Denied` only when the operator granted the peer no scopes at all.
    let reason = match consent_for(content) {
        ConsentRequirement::Denied => return task_refused(NO_SCOPES),
        ConsentRequirement::OperatorConsent { reason } => reason,
        ConsentRequirement::NotRequired => format!("peer '{}' requested a task on your instance", peer.id),
    };
    let ctx = FederationContext::for_request(serve, state, peer, content);
    let shown: String = goal
        .chars()
        .take(SUMMARY_GOAL_CHARS)
        .map(|c| if crate::parked_steps::is_hidden_char(c) { ' ' } else { c })
        .collect();
    let up_front = ApprovalRequest {
        tool: "federation.task".into(),
        summary: format!("Run a task from peer {}: {shown}", peer.id),
        input: serde_json::json!({ "peer": peer.id, "goal": goal }),
        reason,
        scope_base: "federation.relay".into(),
        trust_tier: TrustTier::Trusted,
    };
    match ctx.request_approval(&up_front).await {
        Approval::Approved => {}
        Approval::TimedOut => return task_refused(TIMED_OUT_RESULT),
        Approval::Unavailable => return task_refused(NOT_QUEUED),
        Approval::Denied | Approval::Parked { .. } => return task_refused(REFUSED),
    }
    match run_turn(&serve.agent, &ctx, goal).await {
        Some(outcome) => {
            let response =
                RelayResponse::Task { accepted: true, detail: crate::trigger::render_notify_body(&outcome) };
            ("task", ResponseFrame::Ok { response }, None)
        }
        None => task_refused(OVER_TIME),
    }
}

/// Run one Peer-origin turn; past the context's deadline it is cancelled
/// and `None` is returned.
async fn run_turn(agent: &Arc<dyn Agent>, ctx: &FederationContext, text: &str) -> Option<TurnOutcome> {
    let turn = agent.turn(Message::text(ctx.session, text).from_peer(), ctx);
    tokio::pin!(turn);
    tokio::select! {
        outcome = &mut turn => Some(outcome),
        _ = tokio::time::sleep_until(ctx.deadline) => {
            ctx.cancel.cancel();
            let _ = tokio::time::timeout(CANCEL_GRACE, turn).await;
            None
        }
    }
}

/// A decision the store holds never beats a cancelled turn: once `cancel`
/// fired, an approval is a denial, so a cancelled or timed-out turn never
/// runs an approved call.
fn settle(answer: Approval, cancel: &CancellationToken) -> Approval {
    if cancel.is_cancelled() && answer == Approval::Approved { Approval::Denied } else { answer }
}

/// The channel a peer's request runs on.
pub(crate) struct FederationContext {
    peer_id: String,
    /// Fresh per request: no history shared between requests or with the
    /// operator, and no refusal ledger carried across.
    session: SessionId,
    /// The peer's effective authority (empty for chat).
    ceiling: CapabilitySet,
    /// The `manual` posture: the peer's ceiling is `manual`, or it is
    /// `assisted` on an instance whose own gates skip some questions
    /// ([`FederationServe::assisted_as_manual`]).
    confirm_all: bool,
    cancel: CancellationToken,
    /// The request's single deadline; approvals get what is left of it.
    deadline: tokio::time::Instant,
    parker: Arc<StepParker>,
}

impl FederationContext {
    fn for_request(
        serve: &FederationServe,
        state: &ListenerState,
        peer: &FederationPeer,
        content: &PeerContent,
    ) -> Self {
        FederationContext {
            peer_id: peer.id.clone(),
            session: SessionId::new(),
            ceiling: content.authority().scopes.clone(),
            confirm_all: content.authority().autonomy == AutonomyLevel::Manual
                || serve.assisted_as_manual,
            cancel: state.shutdown.child_token(),
            deadline: tokio::time::Instant::now() + peer.task_timeout,
            parker: Arc::clone(&serve.parker),
        }
    }
}

#[async_trait]
impl ChannelContext for FederationContext {
    fn channel_name(&self) -> &str {
        "aivyx-federation"
    }

    fn platform(&self) -> ChannelPlatform {
        ChannelPlatform::Federation
    }

    fn trust_tier(&self) -> TrustTier {
        TrustTier::Trusted
    }

    fn session_id(&self) -> SessionId {
        self.session
    }

    async fn stream_event(&self, _event: StreamEvent<'_>) -> Result<(), ChannelError> {
        Ok(())
    }

    async fn finalize(&self, _outcome: &TurnOutcome) -> Result<(), ChannelError> {
        Ok(())
    }

    fn cancellation_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// Park a waiting step in the operator's review queue and wait for the
    /// decision, for at most what is left of the request's deadline.
    async fn request_approval(&self, request: &ApprovalRequest) -> Approval {
        if self.cancel.is_cancelled() {
            return Approval::Denied;
        }
        let remaining = self.deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Approval::TimedOut;
        }
        let origin = format!("peer {}", self.peer_id);
        let Ok(id) = self.parker.park_waiting(request, &origin).await else {
            return Approval::Unavailable;
        };
        let answer = self.parker.await_decision(&id, remaining, &self.cancel).await;
        settle(answer, &self.cancel)
    }

    fn capability_ceiling(&self) -> Option<CapabilitySet> {
        Some(self.ceiling.clone())
    }

    fn confirm_every_action(&self) -> bool {
        self.confirm_all
    }

    fn cancel_inflight(&self) {
        self.cancel.cancel();
    }
}


#[cfg(test)]
pub(crate) mod tests;

#[cfg(test)]
mod e2e_tests;
