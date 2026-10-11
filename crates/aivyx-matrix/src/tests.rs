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

struct ScriptedProvider {
    queue: StdMutex<VecDeque<ScriptedStep>>,
}

#[async_trait]
impl LlmProvider for ScriptedProvider {
    async fn chat_stream(
        &self,
        request: LlmRequest<'_>,
        _cancellation: &CancellationToken,
    ) -> Result<Box<dyn LlmStream>, LlmError> {
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
async fn invites_are_joined_or_declined_by_inviter() {
    let transport = Arc::new(
        ScriptedTransport::with_queue(vec![
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
    assert_eq!(transport.joined().await, vec!["!a:x".to_string()]);
    assert_eq!(transport.declined().await, vec!["!b:x".to_string()]);
}

#[tokio::test]
async fn an_unencrypted_home_room_refuses_to_start() {
    let transport = Arc::new(ScriptedTransport::with_queue(vec![])); // nothing encrypted
    let err = run_until_sent(transport, vec![], 1).await.unwrap_err();
    assert!(err.contains("isn't encrypted"), "{err}");
}

#[tokio::test]
async fn a_transport_error_mid_turn_still_completes_the_turn() {
    let transport = Arc::new(
        ScriptedTransport::with_queue(vec![msg("!home:x", "@me:x", "hi", true)])
            .encrypted_rooms(vec!["!home:x"])
            .then_fail(|| TransportError::Platform("sync task ended".into())),
    );
    let turns = run_until_sent(
        Arc::clone(&transport),
        vec![final_step(&["done"], "done")],
        1,
    )
    .await
    .unwrap();
    assert_eq!(turns, 1);
    assert_eq!(transport.sent().await[0].text, "done");
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
async fn a_logout_mid_turn_finishes_the_turn_then_ends_with_an_error() {
    let transport = Arc::new(
        ScriptedTransport::with_queue(vec![msg("!home:x", "@me:x", "hi", true)])
            .encrypted_rooms(vec!["!home:x"])
            .then_fail(|| TransportError::Unauthorized("M_UNKNOWN_TOKEN".into())),
    );
    let err = run_until_sent(
        Arc::clone(&transport),
        vec![final_step(&["done"], "done")],
        1,
    )
    .await
    .unwrap_err();
    assert!(err.contains("connect matrix"), "{err}");
    assert_eq!(transport.sent().await[0].text, "done");
}
