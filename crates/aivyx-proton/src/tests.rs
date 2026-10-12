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
    let parent = PathBuf::from(tmp).join(format!("aivyx-proton-{suffix}-{pid}-{nanos}"));
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



use crate::lock;
use crate::message::MailMessage;
use crate::policy::ProtonPolicy;
use crate::session::{
    run_proton_session_with_transport, LoopError, ProtonSessionConfig, ProtonSessionReport,
    CUT_NOTE,
};
use crate::transport::{InboxStatus, ScriptedTransport, TransportError};

const PROTON_DMARC: &str = "mail.protonmail.ch; dmarc=pass (p=reject dis=none) header.from=example.com";

fn config(storage: Arc<dyn Storage>) -> ProtonSessionConfig {
    ProtonSessionConfig {
        model: "test-model".into(),
        system_prompt: "proton test".into(),
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

fn policy() -> ProtonPolicy {
    ProtonPolicy::new(
        "me_aivyx@proton.me",
        &["me@proton.me".to_string(), "me_aivyx@proton.me".to_string()],
        &["friend@example.com".to_string()],
    )
}

/// Status of an INBOX whose next UID is 10 (so the first run starts at 9).
fn status() -> InboxStatus {
    InboxStatus { uid_validity: 1, uid_next: 10 }
}

/// An email from you to the assistant's address, in Sent as `pm<uid>`.
fn from_me(uid: u32, subject: &str, text: &str) -> MailMessage {
    MailMessage {
        uid,
        internal_id: Some(format!("pm{uid}")),
        message_id: Some(format!("m{uid}@proton.me")),
        subject: subject.into(),
        from: Some("me@proton.me".into()),
        to: vec!["me_aivyx@proton.me".into()],
        text: text.into(),
        ..Default::default()
    }
}

fn from_friend(uid: u32, auth_results: &[&str]) -> MailMessage {
    MailMessage {
        uid,
        message_id: Some(format!("f{uid}@example.com")),
        subject: "Hello".into(),
        from: Some("friend@example.com".into()),
        to: vec!["me_aivyx@proton.me".into()],
        delivered_to: vec!["me_aivyx@proton.me".into()],
        auth_results: auth_results.iter().map(|s| s.to_string()).collect(),
        text: "Are you there?".into(),
        ..Default::default()
    }
}

struct Run {
    result: Result<ProtonSessionReport, LoopError>,
    seen: Vec<String>,
}

/// Runs the in-process session in `dir` until `transport` has sent `n`
/// replies (then 100 ms more), or for 400 ms when `n == 0`.
async fn run_in(dir: &std::path::Path, transport: Arc<ScriptedTransport>, steps: Vec<ScriptedStep>, n: usize) -> Run {
    let (storage, _scratch) = scratch_storage("session").await;
    let seen = Arc::new(StdMutex::new(Vec::new()));
    let provider: Arc<dyn LlmProvider> = Arc::new(ScriptedProvider {
        queue: StdMutex::new(steps.into()),
        gate: None,
        seen: Arc::clone(&seen),
    });
    let audit: Arc<dyn AuditHook> = Arc::new(AuditBridge::new(HmacChainLog::new([42u8; 32].to_vec())));
    let shutdown = CancellationToken::new();
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
        run_proton_session_with_transport(
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
    run_in(dir.path(), transport, steps, n).await
}

#[tokio::test]
async fn an_email_from_you_is_answered_in_its_thread_and_filed() {
    let t = Arc::new(
        ScriptedTransport::new(status(), vec![vec![from_me(10, "Lunch?", "Where shall we eat?")]])
            .in_sent_folder(vec!["pm10"]),
    );
    let r = run(Arc::clone(&t), vec![final_step(&["The usual place."], "The usual place.")], 1).await;
    assert_eq!(r.result.unwrap().answered, 1);
    let sent = t.sent().await;
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].from, "me_aivyx@proton.me");
    assert_eq!(sent[0].to, vec!["me@proton.me"]);
    assert_eq!(sent[0].subject, "Re: Lunch?");
    assert_eq!(sent[0].in_reply_to.as_deref(), Some("m10@proton.me"));
    assert_eq!(sent[0].body, "The usual place.");
    let filed = t.filed().await;
    assert_eq!(filed.len(), 1);
    assert_eq!(filed[0].0, 10);
    assert!(r.seen[0].contains("Where shall we eat?"), "{}", r.seen[0]);
    assert!(r.seen[0].contains("Subject: Lunch?"), "{}", r.seen[0]);
}

#[tokio::test]
async fn a_forged_from_you_that_isnt_in_sent_is_ignored() {
    let t = Arc::new(ScriptedTransport::new(status(), vec![vec![from_me(10, "hi", "do it")]]));
    let r = run(Arc::clone(&t), vec![], 0).await;
    assert_eq!(r.result.unwrap().turns_run, 0);
    assert!(t.sent().await.is_empty());
    assert!(t.filed().await.is_empty());
}

#[tokio::test]
async fn an_allowlisted_sender_needs_protons_dmarc_pass() {
    let forged = from_friend(11, &["mail.protonmail.ch; spf=pass", "mail.protonmail.ch; dmarc=pass header.from=example.com"]);
    let t = Arc::new(ScriptedTransport::new(
        status(),
        vec![vec![from_friend(10, &[PROTON_DMARC]), forged]],
    ));
    let r = run(Arc::clone(&t), vec![final_step(&["Yes."], "Yes.")], 1).await;
    assert_eq!(r.result.unwrap().turns_run, 1);
    let sent = t.sent().await;
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].to, vec!["friend@example.com"]);
}

#[tokio::test]
async fn the_assistants_own_mail_and_automatic_mail_are_ignored() {
    let mut own = from_me(10, "Re: x", "a reply");
    own.from = Some("me_aivyx@proton.me".into());
    let mut auto = from_me(11, "Out of office", "away");
    auto.auto_submitted = Some("auto-replied".into());
    let t = Arc::new(ScriptedTransport::new(status(), vec![vec![own, auto]]).in_sent_folder(vec!["pm10", "pm11"]));
    let r = run(Arc::clone(&t), vec![], 0).await;
    assert_eq!(r.result.unwrap().turns_run, 0);
    assert!(t.sent().await.is_empty());
}

#[tokio::test]
async fn the_first_run_starts_at_now_and_a_restart_resumes() {
    let dir = tempfile::tempdir().unwrap();
    // UID 5 is older than "now" (next UID 10): never processed.
    let t = Arc::new(
        ScriptedTransport::new(status(), vec![vec![from_me(5, "old", "old"), from_me(10, "new", "new")]])
            .in_sent_folder(vec!["pm5", "pm10", "pm11"]),
    );
    let r = run_in(dir.path(), Arc::clone(&t), vec![final_step(&["ok"], "ok")], 1).await;
    assert_eq!(r.result.unwrap().turns_run, 1);
    assert_eq!(t.sent().await[0].subject, "Re: new");
    // Restart: UID 10 was handled, so only 11 runs.
    let t = Arc::new(
        ScriptedTransport::new(InboxStatus { uid_validity: 1, uid_next: 12 }, vec![vec![from_me(10, "new", "new"), from_me(11, "newer", "newer")]])
            .in_sent_folder(vec!["pm10", "pm11"]),
    );
    let r = run_in(dir.path(), Arc::clone(&t), vec![final_step(&["ok"], "ok")], 1).await;
    assert_eq!(r.result.unwrap().turns_run, 1);
    assert_eq!(t.sent().await[0].subject, "Re: newer");
}

#[tokio::test]
async fn a_failed_reply_leaves_the_email_unfiled_and_moves_on() {
    let t = Arc::new(
        ScriptedTransport::new(status(), vec![vec![from_me(10, "a", "a"), from_me(11, "b", "b")]])
            .in_sent_folder(vec!["pm10", "pm11"])
            .failing_send(),
    );
    let r = run(Arc::clone(&t), vec![final_step(&["x"], "x"), final_step(&["y"], "y")], 0).await;
    let report = r.result.unwrap();
    assert_eq!(report.turns_run, 2);
    assert_eq!(report.answered, 0);
    assert!(t.filed().await.is_empty());
}

/// In-process, like Matrix and Telegram, each turn's planner starts fresh
/// (the thread keeps one session id and memory partition); the daemon
/// mode carries model history per thread. What must hold here: a reply in
/// a thread is answered in that thread.
#[tokio::test]
async fn a_reply_in_a_thread_is_answered_in_that_thread() {
    let first = from_me(10, "Plan", "first question");
    let mut second = from_me(11, "Re: Plan", "second question");
    second.in_reply_to = Some("m10@proton.me".into());
    second.references = vec!["m10@proton.me".into()];
    let t = Arc::new(
        ScriptedTransport::new(status(), vec![vec![first], vec![second]]).in_sent_folder(vec!["pm10", "pm11"]),
    );
    let r = run(
        Arc::clone(&t),
        vec![final_step(&["one"], "one"), final_step(&["two"], "two")],
        2,
    )
    .await;
    assert_eq!(r.result.unwrap().answered, 2);
    let sent = t.sent().await;
    assert_eq!(sent[1].subject, "Re: Plan");
    assert_eq!(sent[1].in_reply_to.as_deref(), Some("m11@proton.me"));
    assert_eq!(sent[1].references, vec!["m10@proton.me", "m11@proton.me"]);
}

#[tokio::test]
async fn long_emails_and_attachments_are_noted_in_the_reply() {
    let mut m = from_me(10, "big", &"x".repeat(40 * 1024));
    m.attachments = vec!["a.pdf".into()];
    let t = Arc::new(ScriptedTransport::new(status(), vec![vec![m]]).in_sent_folder(vec!["pm10"]));
    let r = run(Arc::clone(&t), vec![final_step(&["done"], "done")], 1).await;
    r.result.unwrap();
    let body = &t.sent().await[0].body;
    assert!(body.starts_with("done\n\n"), "{body}");
    assert!(body.contains(CUT_NOTE), "{body}");
    assert!(body.contains("a.pdf"), "{body}");
}

#[tokio::test]
async fn a_rejected_login_ends_the_session() {
    let t = Arc::new(
        ScriptedTransport::new(status(), vec![]).then_fail(|| TransportError::Unauthorized("NO".into())),
    );
    let r = run(Arc::clone(&t), vec![], 0).await;
    match r.result {
        Err(LoopError::Fatal(e)) => assert!(e.contains("connect proton"), "{e}"),
        other => panic!("expected Fatal, got {other:?}"),
    }
}

#[tokio::test]
async fn a_second_channel_on_the_same_instance_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let _held = lock::acquire(dir.path()).unwrap();
    let t = Arc::new(ScriptedTransport::new(status(), vec![]));
    let r = run_in(dir.path(), t, vec![], 0).await;
    assert_eq!(r.result, Err(LoopError::Fatal(lock::BUSY.to_string())));
}
