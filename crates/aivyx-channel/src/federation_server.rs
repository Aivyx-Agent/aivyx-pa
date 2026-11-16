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

use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
/// How long a connected peer has to send its request frame.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a turn gets to stop after its deadline cancels it.
const CANCEL_GRACE: Duration = Duration::from_secs(5);
/// Most skills one search returns.
const MAX_SEARCH_RESULTS: usize = 20;
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
    cfg.peers
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
        .collect()
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
}

/// The audit event for a crossing. Carries ids, verb and a fixed reason only.
pub fn crossing_event(c: &Crossing) -> AuditEvent {
    let (outcome, reason) = match &c.outcome {
        CrossingOutcome::Relayed => ("relayed", String::new()),
        CrossingOutcome::Denied { reason } => ("denied", reason.clone()),
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

/// The peers with a task running; one task per peer at a time.
#[derive(Default)]
pub(crate) struct Busy(Arc<Mutex<HashSet<String>>>);

/// Holds a peer's task slot; releases it when dropped, on every path.
pub(crate) struct BusyGuard {
    set: Arc<Mutex<HashSet<String>>>,
    peer: String,
}

impl Busy {
    pub(crate) fn try_take(&self, peer: &str) -> Option<BusyGuard> {
        let mut set = self.0.lock().unwrap_or_else(|e| e.into_inner());
        set.insert(peer.to_string()).then(|| BusyGuard { set: Arc::clone(&self.0), peer: peer.to_string() })
    }
}

impl Drop for BusyGuard {
    fn drop(&mut self) {
        self.set.lock().unwrap_or_else(|e| e.into_inner()).remove(&self.peer);
    }
}

/// State one listener shares across its connections.
pub(crate) struct ListenerState {
    replay: ReplayGuard,
    busy: Busy,
    /// Fires on daemon shutdown; every request's cancel token is a child.
    shutdown: CancellationToken,
}

impl ListenerState {
    pub(crate) fn new(shutdown: CancellationToken) -> Self {
        ListenerState { replay: ReplayGuard::new(), busy: Busy::default(), shutdown }
    }
}

/// Bind `serve.listen` and serve until `shutdown` fires.
pub async fn run_federation_listener(serve: FederationServe, shutdown: CancellationToken) -> Result<(), String> {
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

/// Serve one connection: handshake, one request, one reply. Every request
/// and refusal is audited before the reply goes out.
pub(crate) async fn handle_connection(serve: &FederationServe, stream: TcpStream, state: &ListenerState) {
    let peers = Arc::clone(&serve.peers);
    let mut link = match Link::accept(stream, &serve.key, |k| peers.iter().any(|p| &p.x25519 == k)).await {
        Ok(link) => link,
        Err(NetError::UnknownPeer { fingerprint }) => {
            record(&serve.audit, &Crossing::inbound_denied(format!("unknown:{fingerprint}"), "-", UNKNOWN_KEY));
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
    let raw = match tokio::time::timeout(REQUEST_TIMEOUT, link.recv()).await {
        Ok(Ok(raw)) => raw,
        Ok(Err(e)) => {
            eprintln!("aivyx-pa federation: peer `{}` sent no request: {e}", peer.id);
            return;
        }
        Err(_) => {
            eprintln!("aivyx-pa federation: peer `{}` sent no request in time", peer.id);
            return;
        }
    };
    if raw.is_empty() {
        // A ping: proves connect, handshake and key match. Not a crossing.
        let _ = link.send(&[]).await;
        return;
    }
    let (verb, frame, denied) = serve_request(serve, state, peer, &raw).await;
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
    let bytes = match serde_json::to_vec(&frame) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("aivyx-pa federation: couldn't encode a reply to `{}`: {e}", peer.id);
            return;
        }
    };
    if let Err(e) = link.send(&bytes).await {
        eprintln!("aivyx-pa federation: couldn't reply to `{}`: {e}", peer.id);
    }
}

fn denied(verb: &'static str, reason: &'static str) -> (&'static str, ResponseFrame, Option<&'static str>) {
    (verb, ResponseFrame::Denied { reason: reason.into() }, Some(reason))
}

fn task_refused(reason: &'static str) -> (&'static str, ResponseFrame, Option<&'static str>) {
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
/// procedure cross, never other persona fields or skill metadata.
fn search_skills(persona: &SharedEffectivePersona, query: &str) -> Vec<String> {
    let q = query.to_lowercase();
    crate::skill_tool::current_skills(persona)
        .into_iter()
        .filter(|s| s.name.to_lowercase().contains(&q) || s.trigger.to_lowercase().contains(&q))
        .take(MAX_SEARCH_RESULTS)
        .filter_map(|s| {
            serde_json::to_string(&serde_json::json!({
                "name": s.name,
                "trigger": s.trigger,
                "procedure": s.procedure,
            }))
            .ok()
        })
        .collect()
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
        .map(|c| if c.is_control() { ' ' } else { c })
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
    /// The `manual` posture: the peer's ceiling is `manual`.
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
            confirm_all: content.authority().autonomy == AutonomyLevel::Manual,
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
pub(crate) mod tests {
    use super::*;
    use aivyx_capability::Scope;
    use aivyx_config::{FederationConfig, FederationPeerConfig};
    use aivyx_core::llm_planner::ContextProvider;
    use aivyx_core::{
        AgentId, ConcreteAgent, LlmPlanner, LlmPlannerConfig, NextStep, NullAuditHook, Tool,
        ToolContext, ToolId, ToolOutcome, ToolRegistry, Verification, VecPlanner,
    };
    use aivyx_crypto::MasterKey;
    use aivyx_federation_net::{fingerprint, ping, request};
    use aivyx_ipc::federation::RelayResponse;
    use aivyx_llm::{LlmError, LlmProvider, LlmRequest, LlmStepEnd, LlmStream, LlmStreamEvent, LlmUsage};
    use aivyx_storage::{KeyDomain, RedbStorage, Storage, StorageConfig};
    use std::path::PathBuf;
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::net::TcpListener;

    struct Scratch(PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// What one turn saw of its message and channel.
    #[derive(Debug, Clone)]
    pub(crate) struct Seen {
        pub origin: aivyx_core::MessageOrigin,
        pub ceiling: Option<CapabilitySet>,
        pub confirm_all: bool,
        pub session: SessionId,
        pub platform: ChannelPlatform,
    }

    /// Records each turn and replies with a fixed text. Reusable by Task 8's
    /// loopback tests.
    pub(crate) struct RecordingAgent {
        id: AgentId,
        caps: CapabilitySet,
        pub seen: Arc<StdMutex<Vec<Seen>>>,
        reply: String,
    }

    impl RecordingAgent {
        pub(crate) fn new(reply: &str) -> Self {
            RecordingAgent {
                id: AgentId::new(),
                caps: CapabilitySet::empty(),
                seen: Arc::new(StdMutex::new(Vec::new())),
                reply: reply.into(),
            }
        }
    }

    #[async_trait]
    impl Agent for RecordingAgent {
        fn id(&self) -> AgentId {
            self.id
        }
        fn capabilities(&self) -> &CapabilitySet {
            &self.caps
        }
        async fn turn(&self, message: Message, channel: &dyn ChannelContext) -> TurnOutcome {
            self.seen.lock().unwrap().push(Seen {
                origin: message.origin,
                ceiling: channel.capability_ceiling(),
                confirm_all: channel.confirm_every_action(),
                session: channel.session_id(),
                platform: channel.platform(),
            });
            TurnOutcome::Completed {
                final_message: self.reply.clone(),
                tool_calls_made: 0,
                duration: Duration::from_millis(0),
            }
        }
    }

    /// A tool named `name` needing scope `name` that counts its runs.
    struct CountingTool {
        id: ToolId,
        name: &'static str,
        schema: serde_json::Value,
        runs: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Tool for CountingTool {
        fn id(&self) -> ToolId {
            self.id
        }
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            "counting test tool"
        }
        fn input_schema(&self) -> &serde_json::Value {
            &self.schema
        }
        fn required_scope(&self, _input: &serde_json::Value) -> Scope {
            Scope::parse(self.name).unwrap()
        }
        async fn execute(&self, _input: serde_json::Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
            self.runs.fetch_add(1, Ordering::SeqCst);
            ToolOutcome::Completed { output: serde_json::json!({"ok": true}), verified: Verification::NotApplicable }
        }
    }

    /// A real turn loop whose planner calls `tool` once, then says "done".
    fn one_call_agent(caps: &[&str], tool: &'static str) -> (Arc<dyn Agent>, Arc<AtomicUsize>) {
        let runs = Arc::new(AtomicUsize::new(0));
        let t = Arc::new(CountingTool {
            id: ToolId::new(),
            name: tool,
            schema: serde_json::json!({"type": "object"}),
            runs: Arc::clone(&runs),
        });
        let tool_id = t.id;
        let caps = CapabilitySet::from_scopes(caps.iter().map(|s| Scope::parse(s).unwrap()));
        let registry = Arc::new(ToolRegistry::new(vec![t as Arc<dyn Tool>]));
        let agent = ConcreteAgent::new(AgentId::new(), caps, registry, Arc::new(NullAuditHook), move || {
            Box::new(VecPlanner::new([
                NextStep::ToolCall {
                    tool_id,
                    input: serde_json::json!({}),
                    auto_corrected_from: None,
                    extracted_from_text: None,
                },
                NextStep::FinalMessage("done".into()),
            ]))
        });
        (Arc::new(agent), runs)
    }

    pub(crate) struct PeerSetup {
        pub verbs: Vec<FederationVerb>,
        pub allowed: Vec<&'static str>,
        pub ceiling: AutonomyLevel,
        pub task_timeout: Duration,
    }

    impl Default for PeerSetup {
        fn default() -> Self {
            PeerSetup {
                verbs: vec![FederationVerb::Chat, FederationVerb::Task, FederationVerb::Search],
                allowed: vec![],
                ceiling: AutonomyLevel::Manual,
                task_timeout: Duration::from_secs(30),
            }
        }
    }

    /// A running listener for the instance `desktop`, paired with the peer
    /// `laptop` (whose keys the test holds).
    pub(crate) struct Fixture {
        _scratch: Scratch,
        pub addr: String,
        pub server_card: PeerCard,
        pub client_key: TransportKey,
        pub client_id: Identity,
        pub parker: Arc<StepParker>,
        pub audit: Arc<PersistentAuditLog>,
        pub shutdown: CancellationToken,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            self.shutdown.cancel();
        }
    }

    pub(crate) async fn start(agent: Arc<dyn Agent>, setup: PeerSetup, persona: SharedEffectivePersona) -> Fixture {
        let dir = std::env::temp_dir().join(format!("aivyx-fed-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let storage: Arc<dyn Storage> =
            RedbStorage::open(StorageConfig::new(dir.join("s.redb")), MasterKey::from_raw([9u8; 32]))
                .await
                .unwrap();
        let audit = Arc::new(PersistentAuditLog::open(Arc::clone(&storage), [7u8; 32]).await.unwrap());
        let parker = Arc::new(StepParker::new(
            storage.domain(KeyDomain::ParkedSteps),
            aivyx_core::AreaFlags::everywhere(false),
            7,
        ));
        let server_id = Identity::generate("desktop".into()).unwrap();
        let server_key = TransportKey::generate().unwrap();
        let client_id = Identity::generate("laptop".into()).unwrap();
        let client_key = TransportKey::generate().unwrap();
        let client_card = PeerCard::issue(&client_id, &client_key).await.unwrap();
        let server_card = PeerCard::issue(&server_id, &server_key).await.unwrap();
        let cfg = FederationConfig {
            id: "desktop".into(),
            listen: None,
            peers: vec![FederationPeerConfig {
                id: "laptop".into(),
                card: client_card.encode(),
                address: "127.0.0.1:1".into(),
                verbs: setup.verbs,
                allowed_scopes: setup.allowed.iter().map(|s| Scope::parse(s).unwrap()).collect(),
                ceiling: setup.ceiling,
                task_timeout: setup.task_timeout,
            }],
        };
        let peers = load_peers(&cfg).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let listen = listener.local_addr().unwrap();
        let serve = FederationServe {
            listen,
            identity: Arc::new(server_id),
            key: Arc::new(server_key),
            peers: Arc::new(peers),
            agent,
            parker: Arc::clone(&parker),
            persona,
            audit: Arc::clone(&audit),
        };
        let shutdown = CancellationToken::new();
        tokio::spawn(serve_listener(listener, serve, shutdown.clone()));
        Fixture {
            _scratch: Scratch(dir),
            addr: listen.to_string(),
            server_card,
            client_key,
            client_id,
            parker,
            audit,
            shutdown,
        }
    }

    fn no_persona() -> SharedEffectivePersona {
        crate::persona::shared_effective_persona(crate::persona::EffectivePersona::default())
    }

    pub(crate) async fn send(f: &Fixture, req: RelayRequest) -> ResponseFrame {
        request(&f.addr, &f.client_key, &f.server_card, &f.client_id, &req, Duration::from_secs(20))
            .await
            .unwrap()
    }

    /// Send pre-built frame bytes on a fresh link and decode the reply.
    async fn send_raw(f: &Fixture, bytes: &[u8]) -> ResponseFrame {
        let mut link = Link::connect(&f.addr, &f.client_key, f.server_card.verify().unwrap()).await.unwrap();
        link.send(bytes).await.unwrap();
        serde_json::from_slice(&link.recv().await.unwrap()).unwrap()
    }

    async fn signed_frame(f: &Fixture, recipient: &str, req: &RelayRequest) -> Vec<u8> {
        let (header, body) = f.client_id.sign_relay(recipient, req).await.unwrap();
        serde_json::to_vec(&RequestFrame { header, body: STANDARD.encode(body) }).unwrap()
    }

    /// `(peer_id, direction, verb, outcome, reason)` for every crossing.
    pub(crate) fn crossings(audit: &PersistentAuditLog) -> Vec<[String; 5]> {
        audit
            .entries()
            .unwrap()
            .into_iter()
            .filter_map(|e| match e.event {
                AuditEvent::FederationCrossing { peer_id, direction, verb, outcome, reason } => {
                    Some([peer_id, direction, verb, outcome, reason])
                }
                _ => None,
            })
            .collect()
    }

    async fn wait_crossings(audit: &PersistentAuditLog, n: usize) -> Vec<[String; 5]> {
        for _ in 0..200 {
            let got = crossings(audit);
            if got.len() >= n {
                return got;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("expected {n} crossings, got {:?}", crossings(audit));
    }

    /// The pending waiting step for `tool`, once it appears.
    pub(crate) async fn waiting_step(p: &StepParker, tool: &str) -> crate::parked_steps::ParkedStep {
        for _ in 0..200 {
            if let Some(s) = p.list().await.unwrap().into_iter().find(|s| {
                s.waiting && s.tool == tool && s.state == crate::parked_steps::ParkedState::Pending
            }) {
                return s;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("no waiting step for {tool}");
    }

    async fn decide(p: &StepParker, id: &str, approve: bool) {
        p.resolve(id, approve, |_| async { panic!("a waiting step never runs on resolve") }).await.unwrap();
    }

    fn task(goal: &str) -> RelayRequest {
        RelayRequest::Task { goal: goal.into() }
    }

    fn task_reply(frame: ResponseFrame) -> (bool, String) {
        match frame {
            ResponseFrame::Ok { response: RelayResponse::Task { accepted, detail } } => (accepted, detail),
            other => panic!("expected a task reply, got {other:?}"),
        }
    }

    fn assert_denied(frame: ResponseFrame, want: &str) {
        match frame {
            ResponseFrame::Denied { reason } => assert!(reason.contains(want), "{reason}"),
            other => panic!("expected Denied, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn chat_from_a_paired_peer_gets_a_reply_and_two_crossings() {
        let agent = Arc::new(RecordingAgent::new("hello back"));
        let f = start(agent.clone(), PeerSetup::default(), no_persona()).await;
        let reply = send(&f, RelayRequest::Chat { message: "secret words".into() }).await;
        assert_eq!(reply, ResponseFrame::Ok { response: RelayResponse::Chat { reply: "hello back".into() } });
        let c = crossings(&f.audit);
        assert_eq!(c, vec![["laptop", "inbound", "chat", "relayed", ""].map(String::from)]);
        let seen = agent.seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].origin, aivyx_core::MessageOrigin::Peer);
        assert_eq!(seen[0].ceiling, Some(CapabilitySet::empty()), "chat runs no tools");
        assert_eq!(seen[0].platform, ChannelPlatform::Federation);
        // One session per request: a second chat gets a new one.
        send(&f, RelayRequest::Chat { message: "again".into() }).await;
        let seen = agent.seen.lock().unwrap().clone();
        assert_ne!(seen[0].session, seen[1].session);
    }

    #[tokio::test]
    async fn a_verb_not_in_the_peers_list_is_denied() {
        let agent = Arc::new(RecordingAgent::new("x"));
        let setup = PeerSetup { verbs: vec![FederationVerb::Chat], ..Default::default() };
        let f = start(agent.clone(), setup, no_persona()).await;
        assert_denied(send(&f, task("delete everything")).await, "verb not allowed for this peer");
        assert!(agent.seen.lock().unwrap().is_empty());
        assert!(f.parker.list().await.unwrap().is_empty(), "nothing was queued");
        let c = crossings(&f.audit);
        assert_eq!(c, vec![["laptop", "inbound", "task", "denied", "verb not allowed for this peer"].map(String::from)]);
    }

    #[tokio::test]
    async fn an_unknown_key_is_closed_and_audited_with_its_fingerprint() {
        let f = start(Arc::new(RecordingAgent::new("x")), PeerSetup::default(), no_persona()).await;
        let stranger = TransportKey::generate().unwrap();
        if let Ok(mut link) = Link::connect(&f.addr, &stranger, f.server_card.verify().unwrap()).await {
            let _ = link.send(b"{}").await;
            assert!(link.recv().await.is_err(), "the server closes an unknown key's link");
        }
        let c = wait_crossings(&f.audit, 1).await;
        let id = format!("unknown:{}", fingerprint(&stranger.public()));
        assert_eq!(c, vec![[id.as_str(), "inbound", "-", "denied", "unknown transport key"].map(String::from)]);
    }

    #[tokio::test]
    async fn a_replayed_frame_is_denied() {
        let f = start(Arc::new(RecordingAgent::new("hi")), PeerSetup::default(), no_persona()).await;
        let frame = signed_frame(&f, "desktop", &RelayRequest::Chat { message: "once".into() }).await;
        assert!(matches!(send_raw(&f, &frame).await, ResponseFrame::Ok { .. }));
        assert_denied(send_raw(&f, &frame).await, "replayed request");
        let c = crossings(&f.audit);
        assert_eq!(c.len(), 2);
        assert_eq!(c[1][3], "denied");
    }

    #[tokio::test]
    async fn a_frame_signed_for_another_instance_is_denied() {
        let agent = Arc::new(RecordingAgent::new("hi"));
        let f = start(agent.clone(), PeerSetup::default(), no_persona()).await;
        let frame = signed_frame(&f, "elsewhere", &RelayRequest::Chat { message: "hi".into() }).await;
        assert_denied(send_raw(&f, &frame).await, "did not verify");
        assert!(agent.seen.lock().unwrap().is_empty());
        let c = crossings(&f.audit);
        assert_eq!(c[0][..4], ["laptop", "inbound", "-", "denied"].map(String::from));
    }

    #[tokio::test]
    async fn a_task_waits_for_the_up_front_approval_then_runs() {
        let agent = Arc::new(RecordingAgent::new("tidied"));
        let setup = PeerSetup { allowed: vec!["fs.read"], ..Default::default() };
        let f = Arc::new(start(agent.clone(), setup, no_persona()).await);
        let sender = {
            let f = Arc::clone(&f);
            tokio::spawn(async move { send(&f, task("tidy the notes\nnow")).await })
        };
        let step = waiting_step(&f.parker, "federation.task").await;
        assert_eq!(step.origin, "peer laptop");
        assert_eq!(step.summary, "Run a task from peer laptop: tidy the notes now");
        assert!(agent.seen.lock().unwrap().is_empty(), "nothing runs before the approval");
        decide(&f.parker, &step.id, true).await;
        assert_eq!(task_reply(sender.await.unwrap()), (true, "tidied".into()));
        let seen = agent.seen.lock().unwrap().clone();
        assert_eq!(seen[0].origin, aivyx_core::MessageOrigin::Peer);
        let ceiling = seen[0].ceiling.clone().unwrap();
        assert!(ceiling.grants(&Scope::parse("fs.read").unwrap()));
        assert!(!ceiling.grants(&Scope::parse("fs.write").unwrap()));
        assert!(seen[0].confirm_all, "the default ceiling is manual");
        assert_eq!(crossings(&f.audit), vec![["laptop", "inbound", "task", "relayed", ""].map(String::from)]);
    }

    #[tokio::test]
    async fn a_refused_task_is_not_run() {
        let agent = Arc::new(RecordingAgent::new("x"));
        let setup = PeerSetup { allowed: vec!["fs.read"], ..Default::default() };
        let f = Arc::new(start(agent.clone(), setup, no_persona()).await);
        let sender = {
            let f = Arc::clone(&f);
            tokio::spawn(async move { send(&f, task("read the diary")).await })
        };
        let step = waiting_step(&f.parker, "federation.task").await;
        decide(&f.parker, &step.id, false).await;
        assert_eq!(task_reply(sender.await.unwrap()), (false, "refused by the operator".into()));
        assert!(agent.seen.lock().unwrap().is_empty());
        let c = crossings(&f.audit);
        assert_eq!(c, vec![["laptop", "inbound", "task", "denied", "refused by the operator"].map(String::from)]);
        assert!(!c[0].iter().any(|v| v.contains("diary")), "no payload text in the audit");
    }

    #[tokio::test]
    async fn a_task_times_out_waiting_for_approval() {
        let agent = Arc::new(RecordingAgent::new("x"));
        let setup =
            PeerSetup { allowed: vec!["fs.read"], task_timeout: Duration::from_millis(300), ..Default::default() };
        let f = start(agent.clone(), setup, no_persona()).await;
        assert_eq!(task_reply(send(&f, task("wait")).await), (false, "timed out waiting for approval".into()));
        assert!(agent.seen.lock().unwrap().is_empty());
        let steps = f.parker.list().await.unwrap();
        assert_eq!(steps[0].state, crate::parked_steps::ParkedState::Lapsed);
    }

    #[tokio::test]
    async fn a_second_task_while_one_runs_is_busy() {
        let setup = PeerSetup { allowed: vec!["fs.read"], ..Default::default() };
        let f = Arc::new(start(Arc::new(RecordingAgent::new("x")), setup, no_persona()).await);
        let first = {
            let f = Arc::clone(&f);
            tokio::spawn(async move { send(&f, task("first")).await })
        };
        let step = waiting_step(&f.parker, "federation.task").await;
        assert_eq!(task_reply(send(&f, task("second")).await), (false, "busy".into()));
        decide(&f.parker, &step.id, false).await;
        assert!(!task_reply(first.await.unwrap()).0);
        // Released: a third task is queued, not busy.
        let third = {
            let f = Arc::clone(&f);
            tokio::spawn(async move { send(&f, task("third")).await })
        };
        let step = waiting_step(&f.parker, "federation.task").await;
        decide(&f.parker, &step.id, false).await;
        assert_eq!(task_reply(third.await.unwrap()), (false, "refused by the operator".into()));
    }

    #[tokio::test]
    async fn a_task_runs_only_under_the_peers_scopes() {
        let (agent, runs) = one_call_agent(&["fs.read", "shell.exec"], "shell.exec");
        let setup = PeerSetup { allowed: vec!["fs.read"], ceiling: AutonomyLevel::Assisted, ..Default::default() };
        let f = Arc::new(start(agent, setup, no_persona()).await);
        let sender = {
            let f = Arc::clone(&f);
            tokio::spawn(async move { send(&f, task("run ls")).await })
        };
        let step = waiting_step(&f.parker, "federation.task").await;
        decide(&f.parker, &step.id, true).await;
        let (accepted, _) = task_reply(sender.await.unwrap());
        assert!(accepted);
        assert_eq!(runs.load(Ordering::SeqCst), 0, "shell.exec is outside the peer's grant");
    }

    #[tokio::test]
    async fn manual_ceiling_asks_before_every_change() {
        let (agent, runs) = one_call_agent(&["fs.write"], "fs.write");
        let setup = PeerSetup { allowed: vec!["fs.write"], ..Default::default() };
        let f = Arc::new(start(agent, setup, no_persona()).await);
        let sender = {
            let f = Arc::clone(&f);
            tokio::spawn(async move { send(&f, task("write a note")).await })
        };
        let up_front = waiting_step(&f.parker, "federation.task").await;
        decide(&f.parker, &up_front.id, true).await;
        let write = waiting_step(&f.parker, "fs.write").await;
        assert_eq!(write.origin, "peer laptop");
        decide(&f.parker, &write.id, false).await;
        let (accepted, _) = task_reply(sender.await.unwrap());
        assert!(accepted);
        assert_eq!(runs.load(Ordering::SeqCst), 0, "the operator refused the write");
    }

    #[tokio::test]
    async fn search_returns_procedure_fields_only() {
        let mut persona = crate::persona::EffectivePersona {
            operator_profile: Some("private profile".into()),
            ..Default::default()
        };
        for (name, trigger) in [("deploy-checklist", "when deploying"), ("garden", "watering plants")] {
            persona.learned_skills.push(
                serde_json::json!({"name": name, "trigger": trigger, "procedure": "1. run tests", "domain": "ops"})
                    .to_string(),
            );
        }
        let agent = Arc::new(RecordingAgent::new("x"));
        let f = start(agent.clone(), PeerSetup::default(), crate::persona::shared_effective_persona(persona)).await;
        let results = match send(&f, RelayRequest::Search { query: "DEPLOY".into() }).await {
            ResponseFrame::Ok { response: RelayResponse::Search { results } } => results,
            other => panic!("expected search results, got {other:?}"),
        };
        assert_eq!(results.len(), 1);
        let v: serde_json::Value = serde_json::from_str(&results[0]).unwrap();
        let mut keys: Vec<&String> = v.as_object().unwrap().keys().collect();
        keys.sort();
        assert_eq!(keys, ["name", "procedure", "trigger"]);
        assert_eq!(v["name"], "deploy-checklist");
        assert!(agent.seen.lock().unwrap().is_empty(), "search runs no turn");
        assert_eq!(crossings(&f.audit)[0][..4], ["laptop", "inbound", "search", "relayed"].map(String::from));
    }

    struct CountingRecall(Arc<AtomicUsize>);
    #[async_trait]
    impl ContextProvider for CountingRecall {
        async fn recall(
            &self,
            _m: &str,
            _s: SessionId,
            _t: aivyx_core::TurnId,
            _o: aivyx_core::MessageOrigin,
        ) -> Option<String> {
            self.0.fetch_add(1, Ordering::SeqCst);
            None
        }
    }

    struct SaysHi;
    struct HiStream;
    #[async_trait]
    impl LlmProvider for SaysHi {
        async fn chat_stream(
            &self,
            _r: LlmRequest<'_>,
            _c: &CancellationToken,
        ) -> Result<Box<dyn LlmStream>, LlmError> {
            Ok(Box::new(HiStream))
        }
    }
    #[async_trait]
    impl LlmStream for HiStream {
        async fn next_event(&mut self) -> Result<Option<LlmStreamEvent>, LlmError> {
            Ok(None)
        }
        async fn finish(self: Box<Self>) -> Result<LlmStepEnd, LlmError> {
            Ok(LlmStepEnd::FinalMessage { text: "hi".into(), usage: LlmUsage::default() })
        }
    }

    #[tokio::test]
    async fn a_peer_turn_gets_no_recall() {
        let count = Arc::new(AtomicUsize::new(0));
        let registry = Arc::new(ToolRegistry::new(vec![]));
        let factory = {
            let (count, registry) = (Arc::clone(&count), Arc::clone(&registry));
            move || -> Box<dyn aivyx_core::TurnPlanner> {
                let config =
                    LlmPlannerConfig::new("m").with_context_provider(Arc::new(CountingRecall(Arc::clone(&count))));
                Box::new(LlmPlanner::new(Arc::new(SaysHi), Arc::clone(&registry), config))
            }
        };
        let agent: Arc<dyn Agent> = Arc::new(ConcreteAgent::new(
            AgentId::new(),
            CapabilitySet::empty(),
            registry,
            Arc::new(NullAuditHook),
            factory,
        ));
        let f = start(Arc::clone(&agent), PeerSetup::default(), no_persona()).await;
        let reply = send(&f, RelayRequest::Chat { message: "what do you remember?".into() }).await;
        assert_eq!(reply, ResponseFrame::Ok { response: RelayResponse::Chat { reply: "hi".into() } });
        assert_eq!(count.load(Ordering::SeqCst), 0, "no recall for a peer");
        // The provider is wired: an operator turn does recall.
        let ctx = test_context(&f.parker);
        agent.turn(Message::text(ctx.session, "hello"), &ctx).await;
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    fn test_context(parker: &Arc<StepParker>) -> FederationContext {
        FederationContext {
            peer_id: "laptop".into(),
            session: SessionId::new(),
            ceiling: CapabilitySet::empty(),
            confirm_all: true,
            cancel: CancellationToken::new(),
            deadline: tokio::time::Instant::now() + Duration::from_secs(5),
            parker: Arc::clone(parker),
        }
    }

    #[tokio::test]
    async fn ping_gets_an_empty_frame_and_writes_no_crossing() {
        let f = start(Arc::new(RecordingAgent::new("x")), PeerSetup::default(), no_persona()).await;
        ping(&f.addr, &f.client_key, f.server_card.verify().unwrap()).await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(crossings(&f.audit).is_empty());
    }

    #[tokio::test]
    async fn load_peers_refuses_a_card_for_another_id() {
        let id = Identity::generate("laptop".into()).unwrap();
        let card = PeerCard::issue(&id, &TransportKey::generate().unwrap()).await.unwrap();
        let cfg = FederationConfig {
            id: "desktop".into(),
            listen: None,
            peers: vec![FederationPeerConfig {
                id: "phone".into(),
                card: card.encode(),
                address: "127.0.0.1:1".into(),
                verbs: vec![FederationVerb::Chat],
                allowed_scopes: vec![],
                ceiling: AutonomyLevel::Manual,
                task_timeout: Duration::from_secs(900),
            }],
        };
        let err = load_peers(&cfg).unwrap_err();
        assert!(err.contains("phone") && err.contains("laptop"), "{err}");
    }

    // ---- binding notes ----

    #[test]
    fn a_stored_approval_never_beats_a_cancelled_turn() {
        let cancel = CancellationToken::new();
        assert_eq!(settle(Approval::Approved, &cancel), Approval::Approved);
        cancel.cancel();
        assert_eq!(settle(Approval::Approved, &cancel), Approval::Denied);
        assert_eq!(settle(Approval::TimedOut, &cancel), Approval::TimedOut);
    }

    #[tokio::test]
    async fn a_cancelled_context_never_asks() {
        let f = start(Arc::new(RecordingAgent::new("x")), PeerSetup::default(), no_persona()).await;
        let ctx = test_context(&f.parker);
        ctx.cancel_inflight();
        let req = ApprovalRequest {
            tool: "fs.write".into(),
            summary: "fs.write a".into(),
            input: serde_json::json!({}),
            reason: "r".into(),
            scope_base: "fs.write".into(),
            trust_tier: TrustTier::Trusted,
        };
        assert_eq!(ctx.request_approval(&req).await, Approval::Denied);
    }

    #[test]
    fn crossing_events_carry_only_metadata() {
        let c = Crossing::inbound_denied("laptop", "task", "busy");
        assert_eq!(
            crossing_event(&c),
            AuditEvent::FederationCrossing {
                peer_id: "laptop".into(),
                direction: "inbound".into(),
                verb: "task".into(),
                outcome: "denied".into(),
                reason: "busy".into(),
            }
        );
    }
}
