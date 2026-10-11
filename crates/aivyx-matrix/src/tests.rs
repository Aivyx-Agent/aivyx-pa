//! Session-level tests: the scripted transport drives the real session
//! loop through a `ConcreteAgent` wired to a scripted `LlmProvider`.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;

use aivyx_audit::{AuditBridge, HmacChainLog};
use aivyx_capability::{CapabilitySet, Scope};
use aivyx_core::{AuditHook, CancellationToken, ToolRegistry};
use aivyx_crypto::MasterKey;
use aivyx_llm::{
    LlmError, LlmMessage, LlmProvider, LlmRequest, LlmStepEnd, LlmStream, LlmStreamEvent, LlmUsage,
};
use aivyx_storage::{RedbStorage, Storage, StorageConfig};

// ---------------------------------------------------------------------------
// Scripted LLM provider — identical shape to the Telegram precedent.
// ---------------------------------------------------------------------------

struct ScriptedStep {
    events: Vec<LlmStreamEvent>,
    terminal: LlmStepEnd,
}

/// Lets a test hold a turn "in flight": each `chat_stream` call signals
/// `started`, then waits for `release` (or for the turn's cancellation).
#[derive(Default)]
struct Gate {
    started: tokio::sync::Notify,
    release: tokio::sync::Notify,
}

struct ScriptedProvider {
    queue: StdMutex<VecDeque<ScriptedStep>>,
    gate: Option<Arc<Gate>>,
    seen: Arc<StdMutex<Vec<String>>>,
}

#[async_trait]
impl LlmProvider for ScriptedProvider {
    async fn chat_stream(
        &self,
        request: LlmRequest<'_>,
        cancellation: &CancellationToken,
    ) -> Result<Box<dyn LlmStream>, LlmError> {
        self.seen
            .lock()
            .unwrap()
            .push(format!("{:?}", request.messages));
        if let Some(gate) = &self.gate {
            gate.started.notify_one();
            tokio::select! {
                _ = gate.release.notified() => {}
                _ = cancellation.cancelled() => return Err(LlmError::Cancelled),
            }
        }
        assert!(
            !request.messages.is_empty(),
            "planner must always send non-empty history"
        );
        assert!(
            matches!(request.messages[0], LlmMessage::User { .. }),
            "history[0] should be a User message for a turn with no tools"
        );
        let step = self
            .queue
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| LlmError::Config("ScriptedProvider exhausted".into()))?;
        Ok(Box::new(ScriptedStream {
            events: step.events.into_iter(),
            terminal: Some(step.terminal),
        }))
    }
}

struct ScriptedStream {
    events: std::vec::IntoIter<LlmStreamEvent>,
    terminal: Option<LlmStepEnd>,
}

#[async_trait]
impl LlmStream for ScriptedStream {
    async fn next_event(&mut self) -> Result<Option<LlmStreamEvent>, LlmError> {
        Ok(self.events.next())
    }
    async fn finish(self: Box<Self>) -> Result<LlmStepEnd, LlmError> {
        self.terminal
            .ok_or_else(|| LlmError::StreamEnded("ScriptedStream::finish double-called".into()))
    }
}

fn final_step(chunks: &[&str], text: &str) -> ScriptedStep {
    ScriptedStep {
        events: chunks
            .iter()
            .map(|c| LlmStreamEvent::TextChunk((*c).to_string()))
            .collect(),
        terminal: LlmStepEnd::FinalMessage {
            text: text.to_string(),
            usage: LlmUsage::default(),
        },
    }
}
// ---------------------------------------------------------------------------
// Scratch storage helper — a fresh tmp dir per test so tests run in
// parallel without stepping on each other's redb files.
// ---------------------------------------------------------------------------

async fn scratch_storage(suffix: &str) -> (Arc<dyn Storage>, PathBuf) {
    let tmp = std::env::var("TMPDIR")
        .or_else(|_| std::env::var("TEMP"))
        .unwrap_or_else(|_| "/tmp".to_string());
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let pid = std::process::id();
    let parent = PathBuf::from(tmp).join(format!("aivyx-matrix-{suffix}-{pid}-{nanos}"));
    std::fs::create_dir_all(&parent).expect("scratch store parent must be creatable");
    let store_path = parent.join("store.redb");
    let storage: Arc<dyn Storage> = RedbStorage::open(
        StorageConfig::new(store_path),
        MasterKey::from_raw([7u8; 32]),
    )
    .await
    .expect("scratch storage must open");
    (storage, parent)
}

use crate::policy::{MatrixPolicy, ASK_TO_VERIFY, CANT_DECRYPT};
use crate::session::{run_matrix_session_with_transport, MatrixSessionConfig};
use crate::transport::{Inbound, IncomingMessage, ScriptedTransport, TransportError};

fn config(storage: Arc<dyn Storage>) -> MatrixSessionConfig {
    MatrixSessionConfig {
        model: "test-model".into(),
        system_prompt: "matrix test".into(),
        max_tokens: 128,
        capabilities: CapabilitySet::from_scopes([Scope::parse("memory.read").unwrap()]),
        tools: Arc::new(ToolRegistry::new(Vec::new())),
        storage,
        tool_allowlist: None,
        memory_topic_prefix: None,
        turn_timeout_secs: None,
        cycle_detection: None,
        injection_scan_enabled: true,
        injection_scan_exempt: std::collections::BTreeSet::new(),
        confirm_integration_writes: false,
        budget_gate: None,
        rate_gate: None,
    }
}

fn policy() -> MatrixPolicy {
    MatrixPolicy {
        room_filter: "!home:x".into(),
        allowed_senders: vec!["@me:x".into()],
    }
}

fn msg(room: &str, sender: &str, text: &str, verified: bool) -> Inbound {
    Inbound::Message(IncomingMessage {
        room_id: room.into(),
        sender: sender.into(),
        text: text.into(),
        verified,
    })
}

/// Runs the session until `transport` has sent `n` messages (or 5 s).
async fn run_until_sent(
    transport: Arc<ScriptedTransport>,
    provider: Vec<ScriptedStep>,
    n: usize,
) -> Result<usize, String> {
    let (storage, _dir) = scratch_storage("session").await;
    let provider: Arc<dyn LlmProvider> = Arc::new(ScriptedProvider {
        queue: StdMutex::new(provider.into()),
        gate: None,
        seen: Arc::default(),
    });
    let audit: Arc<dyn AuditHook> =
        Arc::new(AuditBridge::new(HmacChainLog::new([42u8; 32].to_vec())));
    let shutdown = CancellationToken::new();
    let (watch_t, watch_s) = (Arc::clone(&transport), shutdown.clone());
    tokio::spawn(async move {
        while watch_t.sent().await.len() < n {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        watch_s.cancel();
    });
    tokio::time::timeout(
        Duration::from_secs(5),
        run_matrix_session_with_transport(
            transport,
            policy(),
            config(storage),
            provider,
            audit,
            None,
            shutdown,
        ),
    )
    .await
    .map_err(|_| "session did not finish".to_string())?
    .map(|r| r.turns_run)
}

#[tokio::test]
async fn the_verified_operator_gets_a_turn_and_strangers_get_nothing() {
    let transport = Arc::new(
        ScriptedTransport::with_queue(vec![
            msg("!home:x", "@stranger:x", "hello?", true),
            msg("!other:x", "@me:x", "wrong room", true),
            msg("!home:x", "@me:x", "hi", true),
        ])
        .encrypted_rooms(vec!["!home:x"]),
    );
    let turns = run_until_sent(
        Arc::clone(&transport),
        vec![final_step(&["hello!"], "hello!")],
        1,
    )
    .await
    .unwrap();
    assert_eq!(turns, 1);
    let sent = transport.sent().await;
    assert_eq!(sent.len(), 1);
    assert_eq!(
        (sent[0].room_id.as_str(), sent[0].text.as_str()),
        ("!home:x", "hello!")
    );
}

#[tokio::test]
async fn an_unverified_device_is_asked_to_verify_once() {
    let transport = Arc::new(
        ScriptedTransport::with_queue(vec![
            msg("!home:x", "@me:x", "one", false),
            msg("!home:x", "@me:x", "two", false),
            Inbound::Undecryptable {
                room_id: "!home:x".into(),
                sender: "@me:x".into(),
            },
            Inbound::Undecryptable {
                room_id: "!home:x".into(),
                sender: "@me:x".into(),
            },
            msg("!home:x", "@me:x", "verified now", true),
        ])
        .encrypted_rooms(vec!["!home:x"]),
    );
    run_until_sent(Arc::clone(&transport), vec![final_step(&["ok"], "ok")], 3)
        .await
        .unwrap();
    let texts: Vec<String> = transport.sent().await.into_iter().map(|m| m.text).collect();
    assert_eq!(
        texts,
        vec![
            ASK_TO_VERIFY.to_string(),
            CANT_DECRYPT.to_string(),
            "ok".to_string()
        ]
    );
}

#[tokio::test]
async fn only_the_operators_invite_to_the_home_room_is_joined() {
    let transport = Arc::new(
        ScriptedTransport::with_queue(vec![
            Inbound::Invite {
                room_id: "!home:x".into(),
                inviter: "@me:x".into(),
            },
            Inbound::Invite {
                room_id: "!a:x".into(),
                inviter: "@me:x".into(),
            },
            Inbound::Invite {
                room_id: "!b:x".into(),
                inviter: "@stranger:x".into(),
            },
            msg("!home:x", "@me:x", "hi", true),
        ])
        .encrypted_rooms(vec!["!home:x"]),
    );
    run_until_sent(Arc::clone(&transport), vec![final_step(&["hi"], "hi")], 1)
        .await
        .unwrap();
    assert_eq!(transport.joined().await, vec!["!home:x".to_string()]);
    assert_eq!(transport.declined().await, vec!["!a:x".to_string(), "!b:x".to_string()]);
}

#[tokio::test]
async fn an_unencrypted_home_room_refuses_to_start() {
    let transport = Arc::new(ScriptedTransport::with_queue(vec![])); // nothing encrypted
    let err = run_until_sent(transport, vec![], 1).await.unwrap_err();
    assert!(err.contains("isn't encrypted"), "{err}");
}

#[tokio::test]
async fn a_logout_between_turns_ends_the_session_with_an_error() {
    let transport = Arc::new(
        ScriptedTransport::with_queue(vec![])
            .encrypted_rooms(vec!["!home:x"])
            .then_fail(|| TransportError::Unauthorized("M_UNKNOWN_TOKEN".into())),
    );
    let err = run_until_sent(transport, vec![], 1).await.unwrap_err();
    assert!(err.contains("connect matrix"), "{err}");
}

#[tokio::test]
async fn a_closed_inbound_stream_ends_the_session_instead_of_backing_off_forever() {
    let transport = Arc::new(
        ScriptedTransport::with_queue(vec![])
            .encrypted_rooms(vec!["!home:x"])
            .then_fail(|| TransportError::Closed),
    );
    let err = tokio::time::timeout(Duration::from_secs(5), run_until_sent(transport, vec![], 1))
        .await
        .expect("must end, not back off")
        .unwrap_err();
    assert!(err.contains("sync stopped"), "{err}");
}

// ---------------------------------------------------------------------------
// Gated-provider tests: the turn is genuinely in flight while the in-turn
// `select!` arms run.
// ---------------------------------------------------------------------------

type SessionHandle = tokio::task::JoinHandle<Result<crate::session::MatrixSessionReport, String>>;

struct Running {
    handle: SessionHandle,
    shutdown: CancellationToken,
    gate: Arc<Gate>,
    seen: Arc<StdMutex<Vec<String>>>,
}

async fn start_gated(transport: Arc<ScriptedTransport>, steps: Vec<ScriptedStep>) -> Running {
    let (storage, _dir) = scratch_storage("gated").await;
    let gate = Arc::new(Gate::default());
    let seen: Arc<StdMutex<Vec<String>>> = Arc::default();
    let provider: Arc<dyn LlmProvider> = Arc::new(ScriptedProvider {
        queue: StdMutex::new(steps.into()),
        gate: Some(Arc::clone(&gate)),
        seen: Arc::clone(&seen),
    });
    let audit: Arc<dyn AuditHook> =
        Arc::new(AuditBridge::new(HmacChainLog::new([42u8; 32].to_vec())));
    let shutdown = CancellationToken::new();
    let handle = tokio::spawn(run_matrix_session_with_transport(
        transport,
        policy(),
        config(storage),
        provider,
        audit,
        None,
        shutdown.clone(),
    ));
    Running {
        handle,
        shutdown,
        gate,
        seen,
    }
}

impl Running {
    async fn turn_started(&self) {
        tokio::time::timeout(Duration::from_secs(5), self.gate.started.notified())
            .await
            .expect("turn never reached the provider");
    }

    async fn finish(self) -> Result<crate::session::MatrixSessionReport, String> {
        tokio::time::timeout(Duration::from_secs(5), self.handle)
            .await
            .expect("session did not finish")
            .expect("session task panicked")
    }
}

async fn wait_sent(t: &ScriptedTransport, n: usize) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while t.sent().await.len() < n {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("expected messages were never sent");
}

fn operator_transport(queue: Vec<Inbound>) -> Arc<ScriptedTransport> {
    Arc::new(ScriptedTransport::with_queue(queue).encrypted_rooms(vec!["!home:x"]))
}

#[tokio::test]
async fn cancel_during_a_turn_cancels_it_and_the_session_continues() {
    let transport = operator_transport(vec![msg("!home:x", "@me:x", "slow one", true)]);
    let run = start_gated(
        Arc::clone(&transport),
        vec![final_step(&["second"], "second")],
    )
    .await;
    run.turn_started().await;
    transport
        .push_inbound(msg("!home:x", "@me:x", "/cancel", true))
        .await;
    wait_sent(&transport, 1).await;
    assert!(transport.sent().await[0].text.contains("cancelled"));

    transport
        .push_inbound(msg("!home:x", "@me:x", "again", true))
        .await;
    run.turn_started().await;
    run.gate.release.notify_one();
    wait_sent(&transport, 2).await;
    run.shutdown.cancel();
    let seen = Arc::clone(&run.seen);
    let report = run.finish().await.unwrap();
    assert_eq!(report.turns_run, 2);
    let sent = transport.sent().await;
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[1].text, "second");
    // `/cancel` was never a turn: the provider saw only the two real messages.
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert!(seen.iter().all(|s| !s.contains("/cancel")));
}

#[tokio::test]
async fn a_message_during_a_turn_is_queued_and_runs_next_in_order() {
    let transport = operator_transport(vec![msg("!home:x", "@me:x", "first", true)]);
    let run = start_gated(
        Arc::clone(&transport),
        vec![final_step(&["r1"], "r1"), final_step(&["r2"], "r2")],
    )
    .await;
    run.turn_started().await;
    transport
        .push_inbound(msg("!home:x", "@me:x", "second", true))
        .await;
    tokio::time::sleep(Duration::from_millis(100)).await; // let the arm queue it
    run.gate.release.notify_one();
    run.turn_started().await;
    run.gate.release.notify_one();
    wait_sent(&transport, 2).await;
    run.shutdown.cancel();
    let seen = Arc::clone(&run.seen);
    let report = run.finish().await.unwrap();
    assert_eq!(report.turns_run, 2);
    let texts: Vec<String> = transport.sent().await.into_iter().map(|m| m.text).collect();
    assert_eq!(texts, vec!["r1".to_string(), "r2".to_string()]);
    let seen = seen.lock().unwrap();
    assert!(seen[0].contains("first") && !seen[0].contains("second"));
    assert!(seen[1].contains("second"));
}

#[tokio::test]
async fn shutdown_during_a_turn_does_not_start_queued_turns() {
    let transport = operator_transport(vec![msg("!home:x", "@me:x", "first", true)]);
    let run = start_gated(
        Arc::clone(&transport),
        vec![final_step(&["r1"], "r1"), final_step(&["r2"], "r2")],
    )
    .await;
    run.turn_started().await;
    transport
        .push_inbound(msg("!home:x", "@me:x", "queued", true))
        .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    run.shutdown.cancel();
    let seen = Arc::clone(&run.seen);
    let report = run.finish().await.unwrap();
    assert_eq!(report.turns_run, 1);
    assert_eq!(transport.sent().await.len(), 1);
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_transport_error_mid_turn_still_completes_the_turn() {
    let transport = Arc::new(
        ScriptedTransport::with_queue(vec![msg("!home:x", "@me:x", "hi", true)])
            .encrypted_rooms(vec!["!home:x"])
            .then_fail(|| TransportError::Platform("sync task ended".into())),
    );
    let run = start_gated(Arc::clone(&transport), vec![final_step(&["done"], "done")]).await;
    run.turn_started().await;
    // The error has been (or is being) hit while the turn is in flight.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(transport.sent().await.is_empty());
    run.gate.release.notify_one();
    wait_sent(&transport, 1).await;
    run.shutdown.cancel(); // also interrupts the reconnect backoff
    let report = run.finish().await.unwrap();
    assert_eq!(report.turns_run, 1);
    assert_eq!(transport.sent().await[0].text, "done");
}

#[tokio::test]
async fn a_closed_stream_mid_turn_finishes_the_turn_then_ends_with_an_error() {
    let transport = Arc::new(
        ScriptedTransport::with_queue(vec![msg("!home:x", "@me:x", "hi", true)])
            .encrypted_rooms(vec!["!home:x"])
            .then_fail(|| TransportError::Closed),
    );
    let run = start_gated(Arc::clone(&transport), vec![final_step(&["done"], "done")]).await;
    run.turn_started().await;
    run.gate.release.notify_one();
    let err = run.finish().await.unwrap_err();
    assert!(err.contains("sync stopped"), "{err}");
    assert_eq!(transport.sent().await[0].text, "done");
}

#[tokio::test]
async fn a_logout_mid_turn_finishes_the_turn_then_ends_with_an_error() {
    let transport = Arc::new(
        ScriptedTransport::with_queue(vec![msg("!home:x", "@me:x", "hi", true)])
            .encrypted_rooms(vec!["!home:x"])
            .then_fail(|| TransportError::Unauthorized("M_UNKNOWN_TOKEN".into())),
    );
    let run = start_gated(Arc::clone(&transport), vec![final_step(&["done"], "done")]).await;
    run.turn_started().await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(transport.sent().await.is_empty());
    run.gate.release.notify_one();
    let err = run.finish().await.unwrap_err();
    assert!(err.contains("connect matrix"), "{err}");
    assert_eq!(transport.sent().await[0].text, "done");
}
