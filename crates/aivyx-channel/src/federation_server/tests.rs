//! Tests for the inbound federation listener. The helpers are
//! `pub(crate)` so Task 8's loopback tests can reuse them.

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
use std::collections::HashMap;
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
    /// `FederationServe::assisted_as_manual` (A19 final review I2).
    pub assisted_as_manual: bool,
}

impl Default for PeerSetup {
    fn default() -> Self {
        PeerSetup {
            verbs: vec![FederationVerb::Chat, FederationVerb::Task, FederationVerb::Search],
            allowed: vec![],
            ceiling: AutonomyLevel::Manual,
            task_timeout: Duration::from_secs(30),
            assisted_as_manual: false,
        }
    }
}

/// A running listener for the instance `desktop`, paired with the peer
/// `laptop` (whose keys the test holds).
pub(crate) struct Fixture {
    _scratch: Scratch,
    pub addr: String,
    pub server_card: PeerCard,
    pub client_key: Arc<TransportKey>,
    pub client_id: Arc<Identity>,
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
    let assisted_as_manual = setup.assisted_as_manual;
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
        assisted_as_manual,
    };
    let shutdown = CancellationToken::new();
    tokio::spawn(serve_listener(listener, serve, shutdown.clone()));
    Fixture {
        _scratch: Scratch(dir),
        addr: listen.to_string(),
        server_card,
        client_key: Arc::new(client_key),
        client_id: Arc::new(client_id),
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

/// A19 final review I2 — an `assisted` peer runs under this instance's own
/// gates; when those skip a question somewhere (`unleashed`, or
/// `confirm_destructive = false`), the daemon sets `assisted_as_manual` and
/// the peer's task runs confirm-all.
#[tokio::test]
async fn an_assisted_peer_runs_as_manual_when_this_instance_is_looser() {
    for (flag, want) in [(false, false), (true, true)] {
        let agent = Arc::new(RecordingAgent::new("ok"));
        let setup = PeerSetup {
            allowed: vec!["fs.read"],
            ceiling: AutonomyLevel::Assisted,
            assisted_as_manual: flag,
            ..Default::default()
        };
        let f = Arc::new(start(agent.clone(), setup, no_persona()).await);
        let sender = {
            let f = Arc::clone(&f);
            tokio::spawn(async move { send(&f, task("list the notes")).await })
        };
        let step = waiting_step(&f.parker, "federation.task").await;
        decide(&f.parker, &step.id, true).await;
        assert!(task_reply(sender.await.unwrap()).0);
        let seen = agent.seen.lock().unwrap().clone();
        assert_eq!(seen[0].confirm_all, want, "assisted_as_manual = {flag}");
    }
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

pub(crate) fn test_context(parker: &Arc<StepParker>) -> FederationContext {
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

#[tokio::test]
async fn a_peer_that_sends_no_readable_request_is_audited() {
    let f = start(Arc::new(RecordingAgent::new("x")), PeerSetup::default(), no_persona()).await;
    let link = Link::connect(&f.addr, &f.client_key, f.server_card.verify().unwrap()).await.unwrap();
    drop(link); // closed before any frame
    let c = wait_crossings(&f.audit, 1).await;
    assert_eq!(c, vec![["laptop", "inbound", "-", "denied", "no readable request"].map(String::from)]);
}

#[tokio::test]
async fn a_search_reply_stays_well_under_the_frame_cap() {
    let mut persona = crate::persona::EffectivePersona::default();
    for i in 0..20 {
        let procedure = "x".repeat(100 * 1024);
        persona
            .learned_skills
            .push(serde_json::json!({"name": format!("big-{i}"), "trigger": "t", "procedure": procedure}).to_string());
    }
    let f = start(Arc::new(RecordingAgent::new("x")), PeerSetup::default(), crate::persona::shared_effective_persona(persona))
        .await;
    let results = match send(&f, RelayRequest::Search { query: "big".into() }).await {
        ResponseFrame::Ok { response: RelayResponse::Search { results } } => results,
        other => panic!("expected search results, got {other:?}"),
    };
    assert!(!results.is_empty() && results.len() < 20, "{} results", results.len());
    let encoded = serde_json::to_vec(&results).unwrap().len();
    assert!(encoded <= 512 * 1024, "{encoded} bytes");
    assert_eq!(crossings(&f.audit)[0][..4], ["laptop", "inbound", "search", "relayed"].map(String::from));
}

#[tokio::test]
async fn a_fifth_connection_from_one_peer_is_refused() {
    let f = start(Arc::new(RecordingAgent::new("x")), PeerSetup::default(), no_persona()).await;
    let server_x = f.server_card.verify().unwrap();
    let mut open = Vec::new();
    for _ in 0..MAX_CONNECTIONS_PER_PEER {
        open.push(Link::connect(&f.addr, &f.client_key, server_x).await.unwrap());
    }
    // Let the server take the four slots before the fifth arrives.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(crossings(&f.audit).is_empty(), "idle links are not crossings yet");
    let mut fifth = Link::connect(&f.addr, &f.client_key, server_x).await.unwrap();
    let c = wait_crossings(&f.audit, 1).await;
    assert_eq!(c, vec![["laptop", "inbound", "-", "denied", "too many connections from this peer"].map(String::from)]);
    let _ = fifth.send(&[]).await;
    assert!(fifth.recv().await.is_err(), "the fifth link is closed");
    // Closing one frees a slot: a ping gets through again.
    drop(open.pop());
    tokio::time::sleep(Duration::from_millis(100)).await;
    ping(&f.addr, &f.client_key, server_x).await.unwrap();
}

fn peer_cfg(id: &str, card: &PeerCard) -> FederationPeerConfig {
    FederationPeerConfig {
        id: id.into(),
        card: card.encode(),
        address: "127.0.0.1:1".into(),
        verbs: vec![FederationVerb::Chat],
        allowed_scopes: vec![],
        ceiling: AutonomyLevel::Manual,
        task_timeout: Duration::from_secs(900),
    }
}

#[tokio::test]
async fn load_peers_refuses_two_peers_on_one_key() {
    // One transport key bound by two identities.
    let shared = TransportKey::generate().unwrap();
    let a = PeerCard::issue(&Identity::generate("laptop".into()).unwrap(), &shared).await.unwrap();
    let b = PeerCard::issue(&Identity::generate("phone".into()).unwrap(), &shared).await.unwrap();
    let cfg = FederationConfig { id: "desktop".into(), listen: None, peers: vec![peer_cfg("laptop", &a), peer_cfg("phone", &b)] };
    let err = load_peers(&cfg).unwrap_err();
    assert!(err.contains("laptop") && err.contains("phone") && err.contains("transport key"), "{err}");

    // One identity key under two instance ids, with distinct transport keys.
    let dir = std::env::temp_dir().join(format!("aivyx-fed-dup-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let _scratch = Scratch(dir.clone());
    let master = MasterKey::from_raw([4u8; 32]);
    let key_path = dir.join("identity.key");
    let first = Identity::load_or_generate("laptop".into(), &key_path, &master).unwrap();
    let second = Identity::load_or_generate("phone".into(), &key_path, &master).unwrap();
    assert_eq!(first.public_key_base64(), second.public_key_base64());
    let a = PeerCard::issue(&first, &TransportKey::generate().unwrap()).await.unwrap();
    let b = PeerCard::issue(&second, &TransportKey::generate().unwrap()).await.unwrap();
    let cfg = FederationConfig { id: "desktop".into(), listen: None, peers: vec![peer_cfg("laptop", &a), peer_cfg("phone", &b)] };
    let err = load_peers(&cfg).unwrap_err();
    assert!(err.contains("laptop") && err.contains("phone") && err.contains("identity key"), "{err}");
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

/// Drift guard: every reason the listener sends a peer (each `denied(..)` /
/// `task_refused(..)` argument, and every reason `verify` returns) is one of
/// `REPLY_REASONS`, the list the requester accepts. A new reply reason
/// that is not added there fails here.
#[test]
fn every_reason_sent_to_a_peer_is_in_reply_reasons() {
    let src = include_str!("mod.rs");
    let mut consts: HashMap<&str, &str> = HashMap::new();
    for line in src.lines() {
        let Some(rest) = line.trim().strip_prefix("const ") else { continue };
        let Some((name, value)) = rest.split_once(": &str = \"") else { continue };
        consts.insert(name, value.trim_end_matches("\";"));
    }
    consts.insert("TIMED_OUT_RESULT", TIMED_OUT_RESULT);
    let check = |name: &str, site: &str| {
        let value = consts.get(name).unwrap_or_else(|| panic!("`{site}` sends `{name}`, not a fixed reason const"));
        assert!(REPLY_REASONS.contains(value), "`{name}` ({value:?}) is sent but missing from REPLY_REASONS");
    };
    let mut sent = 0;
    for call in ["denied(", "task_refused("] {
        for (at, _) in src.match_indices(call) {
            let before = &src[..at];
            if before.ends_with("fn ") || before.ends_with("inbound_") {
                continue;
            }
            let args = &src[at + call.len()..];
            let args = &args[..args.find(')').unwrap()];
            let last = args.rsplit(',').next().unwrap().trim();
            if last == "reason" {
                continue; // `verify`'s reasons, checked below
            }
            check(last, call);
            sent += 1;
        }
    }
    let body = &src[src.find("fn verify(").unwrap()..];
    let body = &body[..body.find("\n}\n").unwrap()];
    let mut from_verify = 0;
    for word in body.split(|c: char| !(c.is_ascii_uppercase() || c == '_')) {
        if word.len() > 2 && consts.contains_key(word) {
            check(word, "verify");
            from_verify += 1;
        }
    }
    assert!(sent >= 9 && from_verify >= 4, "scan found too few sites ({sent}, {from_verify})");
}
