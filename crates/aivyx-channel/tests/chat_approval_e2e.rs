//! Chat approvals over the real daemon IPC server: the connection is still
//! read while a turn runs, so an `ApprovalRequest` can be answered, a
//! disconnect denies it, `CancelTurn` cancels mid-turn, and any other frame
//! sent during a turn is handled after it.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

use aivyx_capability::CapabilitySet;
use aivyx_channel::LocalChannel;
use aivyx_channel::daemon_ipc::{
    DaemonEnvelope, FrameError, FrontendMessage, StreamEventPayload, decode_frame, encode_frame,
};
use aivyx_channel::daemon_server::run_daemon_compat;
use aivyx_core::{
    Agent, AgentId, Approval, ApprovalRequest, CancellationToken, ChannelContext, Message,
    TurnOutcome,
};

struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("aivyx-approval-e2e-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        ScratchDir { path }
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Asks once, records the answer, and reports it as the final message.
struct AskingAgent {
    id: AgentId,
    caps: CapabilitySet,
    answer: Arc<Mutex<Option<Approval>>>,
}

#[async_trait]
impl Agent for AskingAgent {
    fn id(&self) -> AgentId {
        self.id
    }
    fn capabilities(&self) -> &CapabilitySet {
        &self.caps
    }
    async fn turn(&self, _message: Message, channel: &dyn ChannelContext) -> TurnOutcome {
        let answer = channel
            .request_approval(&ApprovalRequest {
                tool: "fs.delete".into(),
                summary: "fs.delete todo.md".into(),
                input: serde_json::json!({"path": "todo.md"}),
                reason: "deleting can't be undone".into(),
                scope_base: "fs.delete".into(),
                trust_tier: aivyx_capability::TrustTier::Trusted,
            })
            .await;
        *self.answer.lock().unwrap() = Some(answer.clone());
        TurnOutcome::Completed {
            final_message: format!("answer={answer:?}"),
            tool_calls_made: 0,
            duration: Duration::from_millis(1),
        }
    }
}

/// Waits (up to `hold`) for the channel's cancellation, then reports whether
/// it was cancelled.
struct StallingAgent {
    id: AgentId,
    caps: CapabilitySet,
    hold: Duration,
}

#[async_trait]
impl Agent for StallingAgent {
    fn id(&self) -> AgentId {
        self.id
    }
    fn capabilities(&self) -> &CapabilitySet {
        &self.caps
    }
    async fn turn(&self, _message: Message, channel: &dyn ChannelContext) -> TurnOutcome {
        let token = channel.cancellation_token();
        let cancelled = tokio::time::timeout(self.hold, token.cancelled()).await.is_ok();
        TurnOutcome::Completed {
            final_message: if cancelled { "cancelled-mid-turn" } else { "ran-to-end" }.into(),
            tool_calls_made: 0,
            duration: Duration::from_millis(1),
        }
    }
}

struct Client {
    reader: OwnedReadHalf,
    writer: OwnedWriteHalf,
    buf: Vec<u8>,
    session_id: String,
}

impl Client {
    async fn connect(socket: &std::path::Path) -> Client {
        let stream = UnixStream::connect(socket).await.expect("connect");
        let (reader, writer) = stream.into_split();
        let mut c = Client { reader, writer, buf: Vec::new(), session_id: String::new() };
        assert!(matches!(c.next().await, DaemonEnvelope::DaemonReady { .. }));
        c.send(FrontendMessage::StartSession { role: None, frontend_type: None }).await;
        loop {
            if let DaemonEnvelope::SessionStarted { session_id } = c.next().await {
                c.session_id = session_id;
                return c;
            }
        }
    }

    async fn send(&mut self, msg: FrontendMessage) {
        self.writer.write_all(&encode_frame(&msg).unwrap()).await.unwrap();
    }

    async fn submit(&mut self, text: &str) {
        let session_id = self.session_id.clone();
        self.send(FrontendMessage::SubmitInput {
            session_id,
            text: text.into(),
            mission_id: None,
            attachments: Vec::new(),
            headless: false,
        })
        .await;
    }

    async fn next(&mut self) -> DaemonEnvelope {
        loop {
            match decode_frame::<DaemonEnvelope>(&self.buf) {
                Ok((env, consumed)) => {
                    self.buf.drain(..consumed);
                    return env;
                }
                Err(FrameError::IncompleteBuf) => {
                    let mut tmp = [0u8; 4096];
                    let n = self.reader.read(&mut tmp).await.expect("read");
                    assert!(n > 0, "daemon closed the connection");
                    self.buf.extend_from_slice(&tmp[..n]);
                }
                Err(e) => panic!("decode: {e}"),
            }
        }
    }
}

async fn start_daemon(agent: Arc<dyn Agent>) -> (ScratchDir, PathBuf, CancellationToken) {
    let scratch = ScratchDir::new();
    let socket = scratch.path.join("daemon.sock");
    let shutdown = CancellationToken::new();
    let channel = Arc::new(LocalChannel::new("approval-e2e", Vec::<u8>::new()));
    let (s, sd) = (socket.clone(), shutdown.clone());
    tokio::spawn(async move {
        let _ = run_daemon_compat(&s, agent, channel, sd).await;
    });
    for _ in 0..100 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    (scratch, socket, shutdown)
}

fn asking_agent() -> (Arc<dyn Agent>, Arc<Mutex<Option<Approval>>>) {
    let answer = Arc::new(Mutex::new(None));
    let agent = Arc::new(AskingAgent {
        id: AgentId::new(),
        caps: CapabilitySet::empty(),
        answer: Arc::clone(&answer),
    });
    (agent, answer)
}

#[tokio::test]
async fn approval_request_resumes_the_same_turn() {
    let (agent, _) = asking_agent();
    let (_scratch, socket, shutdown) = start_daemon(agent).await;
    let mut c = Client::connect(&socket).await;
    c.send(FrontendMessage::SetApprovals { enabled: true }).await;
    c.submit("delete todo.md").await;
    let outcome = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match c.next().await {
                DaemonEnvelope::StreamEvent {
                    event: StreamEventPayload::ApprovalRequest { request_id, summary, .. },
                    ..
                } => {
                    assert_eq!(summary, "fs.delete todo.md");
                    c.send(FrontendMessage::ResolveApproval { request_id, approved: true }).await;
                }
                DaemonEnvelope::TurnComplete { outcome, .. } => return outcome,
                _ => {}
            }
        }
    })
    .await
    .expect("the turn completes");
    assert!(outcome.contains("answer=Approved"), "{outcome}");
    shutdown.cancel();
}

#[tokio::test]
async fn without_set_approvals_the_channel_cannot_ask() {
    let (agent, _) = asking_agent();
    let (_scratch, socket, shutdown) = start_daemon(agent).await;
    let mut c = Client::connect(&socket).await;
    c.submit("delete todo.md").await;
    let outcome = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match c.next().await {
                DaemonEnvelope::StreamEvent { event: StreamEventPayload::ApprovalRequest { .. }, .. } => {
                    panic!("a connection that didn't opt in must never be asked")
                }
                DaemonEnvelope::TurnComplete { outcome, .. } => return outcome,
                _ => {}
            }
        }
    })
    .await
    .expect("the turn completes");
    assert!(outcome.contains("answer=Unavailable"), "{outcome}");
    shutdown.cancel();
}

#[tokio::test]
async fn closing_the_connection_during_an_approval_denies_it() {
    let (agent, answer) = asking_agent();
    let (_scratch, socket, shutdown) = start_daemon(agent).await;
    let mut c = Client::connect(&socket).await;
    c.send(FrontendMessage::SetApprovals { enabled: true }).await;
    c.submit("delete todo.md").await;
    loop {
        if let DaemonEnvelope::StreamEvent { event: StreamEventPayload::ApprovalRequest { .. }, .. } =
            c.next().await
        {
            break;
        }
    }
    drop(c);
    let got = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(a) = answer.lock().unwrap().clone() {
                return a;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the prompt is resolved once the frontend is gone");
    assert_eq!(got, Approval::Denied);
    shutdown.cancel();
}

#[tokio::test]
async fn cancel_turn_mid_turn_is_read_during_the_turn() {
    let agent = Arc::new(StallingAgent {
        id: AgentId::new(),
        caps: CapabilitySet::empty(),
        hold: Duration::from_secs(8),
    });
    let (_scratch, socket, shutdown) = start_daemon(agent).await;
    let mut c = Client::connect(&socket).await;
    c.submit("take your time").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let session_id = c.session_id.clone();
    c.send(FrontendMessage::CancelTurn { session_id }).await;
    let outcome = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let DaemonEnvelope::TurnComplete { outcome, .. } = c.next().await {
                return outcome;
            }
        }
    })
    .await
    .expect("the cancel is read while the turn runs, not after it");
    assert!(outcome.contains("cancelled-mid-turn"), "{outcome}");
    shutdown.cancel();
}

#[tokio::test]
async fn a_frame_sent_during_a_turn_is_handled_after_it() {
    let agent = Arc::new(StallingAgent {
        id: AgentId::new(),
        caps: CapabilitySet::empty(),
        hold: Duration::from_millis(400),
    });
    let (_scratch, socket, shutdown) = start_daemon(agent).await;
    let mut c = Client::connect(&socket).await;
    c.submit("hold on").await;
    c.send(FrontendMessage::ProtocolNegotiation { version: "0.1".into() }).await;
    let mut order = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        while order.len() < 2 {
            match c.next().await {
                DaemonEnvelope::TurnComplete { .. } => order.push("turn"),
                DaemonEnvelope::ProtocolAccepted { .. } => order.push("negotiation"),
                _ => {}
            }
        }
    })
    .await
    .expect("both arrive");
    assert_eq!(order, vec!["turn", "negotiation"]);
    shutdown.cancel();
}

/// The terminal client (`DaemonSession`) answers through its approver hook
/// mid-turn — the prompt it shows is its own business.
#[tokio::test]
async fn terminal_client_answers_an_approval_through_its_approver() {
    for (says_yes, expected) in [(true, "answer=Approved"), (false, "answer=Denied")] {
        let (agent, _) = asking_agent();
        let (_scratch, socket, shutdown) = start_daemon(agent).await;
        let mut session = aivyx_channel::daemon_client::DaemonSession::connect(&socket, None, None)
            .await
            .expect("connect");
        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let seen_by_approver = Arc::clone(&seen);
        session
            .enable_approvals(Arc::new(move |summary: &str, _reason: &str, _input: &serde_json::Value| {
                seen_by_approver.lock().unwrap().push(summary.to_string());
                says_yes
            }))
            .await
            .expect("opt in");
        let (_events, outcome) = tokio::time::timeout(
            Duration::from_secs(10),
            session.submit_input("delete todo.md".into()),
        )
        .await
        .expect("the turn completes")
        .expect("no protocol error");
        assert!(outcome.contains(expected), "{outcome}");
        assert_eq!(*seen.lock().unwrap(), vec!["fs.delete todo.md".to_string()]);
        shutdown.cancel();
    }
}
