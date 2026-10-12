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
    let parent = PathBuf::from(tmp).join(format!("aivyx-signal-{suffix}-{pid}-{nanos}"));
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

use crate::envelope::{Inbound, TextMessage};
use crate::lock;
use crate::policy::{SignalPolicy, ASK_TO_VERIFY};
use crate::session::{
    run_signal_session_with_transport, SignalSessionConfig, SignalSessionReport, ATTACHMENTS_NOTE,
};
use crate::transport::{ScriptedTransport, TransportError, TrustLevel};

const ME: &str = "+15550001111";

fn config(storage: Arc<dyn Storage>) -> SignalSessionConfig {
    SignalSessionConfig {
        model: "test-model".into(),
        system_prompt: "signal test".into(),
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

fn policy() -> SignalPolicy {
    SignalPolicy::new(&[ME.to_string()])
}

fn text(from: &str, body: &str, ts: u64) -> Inbound {
    Inbound::Text(TextMessage {
        sender_number: Some(from.into()),
        sender_uuid: Some(format!("uuid-{from}")),
        timestamp: ts,
        text: body.into(),
        attachments: 0,
    })
}

/// The operator, with a verified key.
fn verified(events: Vec<Inbound>) -> ScriptedTransport {
    ScriptedTransport::new(events).with_identity(ME, TrustLevel::TrustedVerified, "11111 22222")
}

struct Run {
    result: Result<SignalSessionReport, String>,
    seen: Vec<String>,
}

/// Runs the session until `transport` has sent `n` messages (then 100 ms
/// more), or for 400 ms when `n == 0`. `gate` holds turns in flight.
async fn run_with(
    dir: &std::path::Path,
    transport: Arc<ScriptedTransport>,
    steps: Vec<ScriptedStep>,
    gate: Option<Arc<Gate>>,
    n: usize,
    shutdown: CancellationToken,
) -> Run {
    let (storage, _scratch) = scratch_storage("session").await;
    let seen = Arc::new(StdMutex::new(Vec::new()));
    let provider: Arc<dyn LlmProvider> = Arc::new(ScriptedProvider {
        queue: StdMutex::new(steps.into()),
        gate,
        seen: Arc::clone(&seen),
    });
    let audit: Arc<dyn AuditHook> = Arc::new(AuditBridge::new(HmacChainLog::new([42u8; 32].to_vec())));
    let (watch_t, watch_s) = (Arc::clone(&transport), shutdown.clone());
    tokio::spawn(async move {
        if n == 0 {
            tokio::time::sleep(Duration::from_millis(400)).await;
        } else {
            while watch_t.sent().await.len() < n {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        watch_s.cancel();
    });
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        run_signal_session_with_transport(
            transport.as_ref(),
            &policy(),
            dir,
            config(storage),
            provider,
            audit,
            None,
            shutdown,
        ),
    )
    .await
    .expect("session did not finish");
    let seen = seen.lock().unwrap().clone();
    Run { result, seen }
}

async fn run(transport: Arc<ScriptedTransport>, steps: Vec<ScriptedStep>, n: usize) -> Run {
    let dir = tempfile::tempdir().unwrap();
    run_with(dir.path(), transport, steps, None, n, CancellationToken::new()).await
}

#[tokio::test]
async fn the_verified_operator_gets_an_answer_with_typing_and_a_read_receipt() {
    let t = Arc::new(verified(vec![text(ME, "What's on tomorrow?", 7)]));
    let r = run(Arc::clone(&t), vec![final_step(&["Dentist at 9."], "Dentist at 9.")], 1).await;
    assert_eq!(r.result.unwrap().turns_run, 1);
    assert_eq!(t.sent().await, vec![(ME.to_string(), "Dentist at 9.".to_string())]);
    assert_eq!(t.receipts().await, vec![(ME.to_string(), 7)]);
    assert_eq!(
        t.typing_events().await,
        vec![(ME.to_string(), false), (ME.to_string(), true)]
    );
    assert!(r.seen[0].contains("What's on tomorrow?"), "{}", r.seen[0]);
}

#[tokio::test]
async fn an_unverified_key_gets_one_explanation_and_no_turn() {
    let t = Arc::new(
        ScriptedTransport::new(vec![text(ME, "hi", 1), text(ME, "hello?", 2)])
            .with_identity(ME, TrustLevel::TrustedUnverified, "11111 22222"),
    );
    let r = run(Arc::clone(&t), vec![], 0).await;
    assert_eq!(r.result.unwrap().turns_run, 0);
    assert_eq!(t.sent().await, vec![(ME.to_string(), ASK_TO_VERIFY.to_string())]);
    assert!(t.receipts().await.is_empty());
}

#[tokio::test]
async fn a_changed_key_is_refused_like_an_unverified_one() {
    let t = Arc::new(
        ScriptedTransport::new(vec![text(ME, "it's me, new phone", 1)])
            .with_identity(ME, TrustLevel::Untrusted, "99999 00000"),
    );
    let r = run(Arc::clone(&t), vec![], 0).await;
    assert_eq!(r.result.unwrap().turns_run, 0);
    assert_eq!(t.sent().await, vec![(ME.to_string(), ASK_TO_VERIFY.to_string())]);
}

#[tokio::test]
async fn strangers_and_non_messages_get_nothing() {
    let stranger = "+15559999999";
    let t = Arc::new(
        verified(vec![text(stranger, "hello", 1), Inbound::Other])
            .with_identity(stranger, TrustLevel::TrustedVerified, "x"),
    );
    let r = run(Arc::clone(&t), vec![], 0).await;
    assert_eq!(r.result.unwrap().turns_run, 0);
    assert!(t.sent().await.is_empty());
}

#[tokio::test]
async fn a_long_reply_is_split_and_attachments_are_noted() {
    let long = format!("{}\n{}", "a".repeat(1500), "b".repeat(1500));
    let mut m = text(ME, "write a lot", 1);
    if let Inbound::Text(t) = &mut m {
        t.attachments = 1;
    }
    let t = Arc::new(verified(vec![m]));
    let r = run(Arc::clone(&t), vec![final_step(&[&long], &long)], 2).await;
    r.result.unwrap();
    let sent = t.sent().await;
    assert_eq!(sent.len(), 2);
    assert_eq!(sent[0].1, "a".repeat(1500));
    assert!(sent[1].1.starts_with(&"b".repeat(1500)));
    assert!(sent[1].1.ends_with(ATTACHMENTS_NOTE), "{}", sent[1].1);
}

#[tokio::test]
async fn cancel_stops_the_running_turn_and_a_queued_message_runs_after() {
    let gate = Arc::new(Gate::default());
    let t = Arc::new(verified(vec![text(ME, "slow question", 1)]));
    let dir = tempfile::tempdir().unwrap();
    let (t2, g2) = (Arc::clone(&t), Arc::clone(&gate));
    tokio::spawn(async move {
        g2.started.notified().await;
        t2.push(text(ME, "and another thing", 2)).await;
        t2.push(text(ME, "/cancel", 3)).await;
        // The queued message's turn waits on the gate too: let it through.
        g2.started.notified().await;
        g2.release.notify_one();
    });
    let r = run_with(
        dir.path(),
        Arc::clone(&t),
        vec![final_step(&["second answer"], "second answer")],
        Some(Arc::clone(&gate)),
        2,
        CancellationToken::new(),
    )
    .await;
    let report = r.result.unwrap();
    assert_eq!(report.turns_run, 2);
    let sent = t.sent().await;
    assert!(sent[0].1.contains("cancelled"), "{sent:?}");
    assert_eq!(sent[1].1, "second answer");
}

#[tokio::test]
async fn stopping_mid_turn_sends_nothing() {
    let gate = Arc::new(Gate::default());
    let t = Arc::new(verified(vec![text(ME, "slow", 1)]));
    let dir = tempfile::tempdir().unwrap();
    let shutdown = CancellationToken::new();
    let (g2, s2) = (Arc::clone(&gate), shutdown.clone());
    tokio::spawn(async move {
        g2.started.notified().await;
        s2.cancel();
    });
    let r = run_with(dir.path(), Arc::clone(&t), vec![], Some(gate), 99, shutdown).await;
    assert_eq!(r.result.unwrap().turns_run, 1);
    assert!(t.sent().await.is_empty());
}

#[tokio::test]
async fn an_unregistered_account_ends_the_session() {
    let t = Arc::new(
        ScriptedTransport::new(vec![]).then_fail(|| TransportError::Unregistered("Authorization failed".into())),
    );
    let r = run(Arc::clone(&t), vec![], 0).await;
    assert!(r.result.unwrap_err().contains("connect signal"));
}

#[tokio::test]
async fn signal_cli_being_down_is_retried_not_fatal() {
    let t = Arc::new(verified(vec![]));
    t.push_error(TransportError::Unreachable("no socket".into())).await;
    t.push(text(ME, "still there?", 1)).await;
    // n == 1: answered after the (1 s) backoff.
    let r = run(Arc::clone(&t), vec![final_step(&["Yes."], "Yes.")], 1).await;
    assert_eq!(r.result.unwrap().turns_run, 1);
    assert_eq!(t.sent().await, vec![(ME.to_string(), "Yes.".to_string())]);
}

#[tokio::test]
async fn a_transient_error_mid_turn_still_lets_shutdown_through() {
    let gate = Arc::new(Gate::default());
    let t = Arc::new(verified(vec![text(ME, "slow", 1)]));
    let dir = tempfile::tempdir().unwrap();
    let shutdown = CancellationToken::new();
    let cancelled_at = Arc::new(StdMutex::new(None));
    let (t2, g2, s2, at2) = (Arc::clone(&t), Arc::clone(&gate), shutdown.clone(), Arc::clone(&cancelled_at));
    tokio::spawn(async move {
        g2.started.notified().await;
        // The session polls next_event during the turn: hand it one error.
        t2.push_error(TransportError::Unreachable("connection closed".into())).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        *at2.lock().unwrap() = Some(std::time::Instant::now());
        s2.cancel();
    });
    let r = run_with(dir.path(), Arc::clone(&t), vec![], Some(gate), 99, shutdown).await;
    let at = cancelled_at.lock().unwrap().expect("shutdown was cancelled");
    assert!(at.elapsed() < Duration::from_secs(1), "took {:?}", at.elapsed());
    assert_eq!(r.result.unwrap().turns_run, 1);
    assert!(t.sent().await.is_empty());
}

#[tokio::test]
async fn shutdown_mid_turn_drops_the_queued_message() {
    let gate = Arc::new(Gate::default());
    let t = Arc::new(verified(vec![text(ME, "first", 1), text(ME, "second", 2)]));
    let dir = tempfile::tempdir().unwrap();
    let shutdown = CancellationToken::new();
    let (g2, s2) = (Arc::clone(&gate), shutdown.clone());
    tokio::spawn(async move {
        g2.started.notified().await;
        // Let the session queue "second" while "first" is held.
        tokio::time::sleep(Duration::from_millis(100)).await;
        s2.cancel();
    });
    let r = run_with(
        dir.path(),
        Arc::clone(&t),
        vec![final_step(&["x"], "x"), final_step(&["y"], "y")],
        Some(gate),
        99,
        shutdown,
    )
    .await;
    assert_eq!(r.result.unwrap().turns_run, 1);
    assert_eq!(r.seen.len(), 1, "{:?}", r.seen);
    assert!(t.sent().await.is_empty());
}

#[tokio::test]
async fn a_second_channel_on_the_same_instance_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let _held = lock::acquire(dir.path()).unwrap();
    let t = Arc::new(ScriptedTransport::new(vec![]));
    let r = run_with(dir.path(), t, vec![], None, 0, CancellationToken::new()).await;
    assert_eq!(r.result.unwrap_err(), lock::BUSY);
}
