//! Daemon IPC round-trip end-to-end tests.
//!
//! Phase 16 Task 3 proved the IPC protocol carries one turn.
//! Phase 17 Task 2 extends to multi-turn sessions, graceful
//! shutdown, and frontend disconnect.
//!
//! | Seam              | Production                   | Test                  |
//! |-------------------|------------------------------|-----------------------|
//! | Agent             | `ConcreteAgent` + LLM        | `FakeStreamingAgent`  |
//! | Socket path       | `$XDG_RUNTIME_DIR/aivyx-pa/...` | `$TMPDIR/<unique>`    |
//! | ChannelContext     | `LocalChannel<Stdout>`       | `LocalChannel<Vec>`   |

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use aivyx_capability::CapabilitySet;
use aivyx_channel::daemon_client::{DaemonSession, daemon_is_running, run_poc_client};
use aivyx_channel::daemon_ipc::FrontendType;
use aivyx_channel::daemon_ipc::{
    DaemonEnvelope, FrameError, FrontendMessage, MissionDetail, QueryPayload, QueryResponsePayload,
    StreamEventPayload, decode_frame, encode_frame,
};
use aivyx_channel::daemon_server::{
    ChannelFactory, DaemonConfig, run_daemon, run_daemon_compat, run_poc_daemon,
};
use aivyx_channel::{DaemonSessionConfig, run_daemon_session, run_daemon_session_connected};
// Phase 58 — `DaemonConfig.profile` field for Profile inspection
// query support. Tests construct daemons with the synthesized
// default Profile, except the dedicated Phase 58 Profile-query test.
use aivyx_channel::LocalChannel;
use aivyx_config::Profile;
use aivyx_core::{
    Agent, AgentId, CancellationToken, ChannelContext, Message, StreamEvent, TurnOutcome,
};

// ---------------------------------------------------------------------------
// Fake agent that streams two text chunks and completes.
// ---------------------------------------------------------------------------

struct FakeStreamingAgent {
    id: AgentId,
    caps: CapabilitySet,
}

#[async_trait]
impl Agent for FakeStreamingAgent {
    fn id(&self) -> AgentId {
        self.id
    }

    fn capabilities(&self) -> &CapabilitySet {
        &self.caps
    }

    async fn turn(&self, _message: Message, channel: &dyn ChannelContext) -> TurnOutcome {
        // Stream two text chunks so the test can verify ordering.
        let _ = channel.stream_event(StreamEvent::Text("Hello ")).await;
        let _ = channel
            .stream_event(StreamEvent::Text("from daemon!"))
            .await;

        TurnOutcome::Completed {
            final_message: "Hello from daemon!".into(),
            tool_calls_made: 0,
            duration: Duration::from_millis(1),
        }
    }
}

// ---------------------------------------------------------------------------
// Scratch directory for the Unix socket.
// ---------------------------------------------------------------------------

struct ScratchDir {
    path: PathBuf,
}

impl ScratchDir {
    fn new() -> Self {
        let tmp = std::env::var("TMPDIR")
            .or_else(|_| std::env::var("TEMP"))
            .unwrap_or_else(|_| "/tmp".to_string());
        let path = PathBuf::from(tmp).join(format!("aivyx-daemon-e2e-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("scratch dir must be creatable");
        ScratchDir { path }
    }

    fn socket_path(&self) -> PathBuf {
        self.path.join("daemon.sock")
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

// ---------------------------------------------------------------------------
// The test
// ---------------------------------------------------------------------------

#[tokio::test]
async fn one_turn_round_trips_over_ipc() {
    let scratch = ScratchDir::new();
    let socket_path = scratch.socket_path();

    let agent: Arc<dyn Agent> = Arc::new(FakeStreamingAgent {
        id: AgentId::new(),
        caps: CapabilitySet::empty(),
    });

    let channel = Arc::new(LocalChannel::new("daemon-test", Vec::<u8>::new()));

    // Spawn the daemon server on a background task.
    let daemon_socket = socket_path.clone();
    let daemon_agent = Arc::clone(&agent);
    let daemon_channel = Arc::clone(&channel);
    let daemon_handle = tokio::spawn(async move {
        run_poc_daemon(&daemon_socket, daemon_agent, daemon_channel)
            .await
            .expect("daemon must complete successfully");
    });

    // Give the daemon a moment to bind the socket.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Run the client.
    let result = run_poc_client(&socket_path, None, "hello".to_string())
        .await
        .expect("client must complete successfully");

    // Assert the daemon sent DaemonReady with version "0.1".
    assert_eq!(
        result.daemon_version.as_deref(),
        Some("0.1"),
        "daemon must send version 0.1"
    );

    // Assert session_id is non-empty.
    assert!(
        !result.session_id.is_empty(),
        "session_id must be non-empty"
    );

    // Assert we received exactly two Text stream events in order.
    assert_eq!(
        result.events.len(),
        2,
        "expected 2 stream events, got {}: {:?}",
        result.events.len(),
        result.events,
    );
    assert_eq!(
        result.events[0],
        StreamEventPayload::Text {
            text: "Hello ".into()
        },
    );
    assert_eq!(
        result.events[1],
        StreamEventPayload::Text {
            text: "from daemon!".into()
        },
    );

    // Assert the outcome message.
    assert!(
        result.outcome.contains("Hello from daemon!"),
        "outcome must contain the final message, got: {}",
        result.outcome,
    );

    // Wait for the daemon task to finish cleanly.
    tokio::time::timeout(Duration::from_secs(5), daemon_handle)
        .await
        .expect("daemon must finish within 5s")
        .expect("daemon task must not panic");
}

// ---------------------------------------------------------------------------
// Phase 17 Task 2 — multi-turn session test
// ---------------------------------------------------------------------------

#[tokio::test]
async fn multi_turn_session_streams_both_turns() {
    let scratch = ScratchDir::new();
    let socket_path = scratch.socket_path();

    let agent: Arc<dyn Agent> = Arc::new(FakeStreamingAgent {
        id: AgentId::new(),
        caps: CapabilitySet::empty(),
    });

    let channel = Arc::new(LocalChannel::new("daemon-test", Vec::<u8>::new()));
    let shutdown = CancellationToken::new();

    let daemon_socket = socket_path.clone();
    let daemon_agent = Arc::clone(&agent);
    let daemon_channel = Arc::clone(&channel);
    let daemon_shutdown = shutdown.clone();
    let daemon_handle = tokio::spawn(async move {
        run_daemon_compat(
            &daemon_socket,
            daemon_agent,
            daemon_channel,
            daemon_shutdown,
        )
        .await
        .expect("daemon must complete successfully");
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Connect and handshake manually for multi-turn control.
    let stream = UnixStream::connect(&socket_path)
        .await
        .expect("connect to daemon");
    let (mut reader, mut writer) = stream.into_split();
    let mut buf = Vec::with_capacity(4096);

    // Read DaemonReady.
    read_more(&mut reader, &mut buf).await;
    let (envelope, consumed): (DaemonEnvelope, _) = decode_frame(&buf).expect("decode DaemonReady");
    buf.drain(..consumed);
    assert!(matches!(envelope, DaemonEnvelope::DaemonReady { .. }));

    // StartSession.
    let frame = encode_frame(&FrontendMessage::StartSession {
        role: None,
        frontend_type: None,
    })
    .unwrap();
    writer.write_all(&frame).await.unwrap();

    let sid: String = loop {
        match decode_frame::<DaemonEnvelope>(&buf) {
            Ok((DaemonEnvelope::SessionStarted { session_id }, consumed)) => {
                buf.drain(..consumed);
                break session_id;
            }
            Err(FrameError::IncompleteBuf) => read_more(&mut reader, &mut buf).await,
            other => panic!("expected SessionStarted, got {other:?}"),
        }
    };

    // --- Turn 1 ---
    let frame = encode_frame(&FrontendMessage::SubmitInput {
        session_id: sid.clone(),
        text: "turn one".into(),
        mission_id: None,
        attachments: vec![],
        headless: false,
    })
    .unwrap();
    writer.write_all(&frame).await.unwrap();

    let (events_1, outcome_1) = collect_turn_events(&mut reader, &mut buf).await;
    assert_eq!(events_1.len(), 2, "turn 1 events: {events_1:?}");
    assert!(
        outcome_1.contains("Hello from daemon!"),
        "turn 1 outcome: {outcome_1}"
    );

    // --- Turn 2 ---
    let frame = encode_frame(&FrontendMessage::SubmitInput {
        session_id: sid.clone(),
        text: "turn two".into(),
        mission_id: None,
        attachments: vec![],
        headless: false,
    })
    .unwrap();
    writer.write_all(&frame).await.unwrap();

    let (events_2, outcome_2) = collect_turn_events(&mut reader, &mut buf).await;
    assert_eq!(events_2.len(), 2, "turn 2 events: {events_2:?}");
    assert!(
        outcome_2.contains("Hello from daemon!"),
        "turn 2 outcome: {outcome_2}"
    );

    // Disconnect cleanly.
    let frame = encode_frame(&FrontendMessage::Disconnect).unwrap();
    writer.write_all(&frame).await.unwrap();

    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(5), daemon_handle)
        .await
        .expect("daemon must finish within 5s")
        .expect("daemon task must not panic");
}

// ---------------------------------------------------------------------------
// Phase 17 Task 2 — graceful shutdown test
// ---------------------------------------------------------------------------

#[tokio::test]
async fn graceful_shutdown_sends_shutting_down() {
    let scratch = ScratchDir::new();
    let socket_path = scratch.socket_path();

    let agent: Arc<dyn Agent> = Arc::new(FakeStreamingAgent {
        id: AgentId::new(),
        caps: CapabilitySet::empty(),
    });

    let channel = Arc::new(LocalChannel::new("daemon-test", Vec::<u8>::new()));
    let shutdown = CancellationToken::new();

    let daemon_socket = socket_path.clone();
    let daemon_agent = Arc::clone(&agent);
    let daemon_channel = Arc::clone(&channel);
    let daemon_shutdown = shutdown.clone();
    let daemon_handle = tokio::spawn(async move {
        run_daemon_compat(
            &daemon_socket,
            daemon_agent,
            daemon_channel,
            daemon_shutdown,
        )
        .await
        .expect("daemon must complete successfully");
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let stream = UnixStream::connect(&socket_path)
        .await
        .expect("connect to daemon");
    let (mut reader, mut _writer) = stream.into_split();
    let mut buf = Vec::with_capacity(4096);

    // Read DaemonReady.
    read_more(&mut reader, &mut buf).await;
    let (_envelope, consumed): (DaemonEnvelope, _) =
        decode_frame(&buf).expect("decode DaemonReady");
    buf.drain(..consumed);

    // Trigger shutdown from outside.
    shutdown.cancel();

    // The daemon should send ShuttingDown before closing.
    let shutting_down = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match decode_frame::<DaemonEnvelope>(&buf) {
                Ok((DaemonEnvelope::ShuttingDown { reason }, _consumed)) => {
                    return reason;
                }
                Err(FrameError::IncompleteBuf) => {
                    let mut tmp = [0u8; 4096];
                    match reader.read(&mut tmp).await {
                        Ok(0) => return "connection closed without ShuttingDown".into(),
                        Ok(n) => buf.extend_from_slice(&tmp[..n]),
                        Err(e) => return format!("read error: {e}"),
                    }
                }
                Ok((other, consumed)) => {
                    buf.drain(..consumed);
                    panic!("unexpected message after shutdown: {other:?}");
                }
                Err(e) => panic!("decode error: {e}"),
            }
        }
    })
    .await
    .expect("must receive ShuttingDown within 5s");

    assert!(
        shutting_down.contains("shutdown"),
        "reason must mention shutdown, got: {shutting_down}"
    );

    tokio::time::timeout(Duration::from_secs(5), daemon_handle)
        .await
        .expect("daemon must finish within 5s")
        .expect("daemon task must not panic");
}

// ---------------------------------------------------------------------------
// Phase 17 Task 2 — frontend disconnect test
// ---------------------------------------------------------------------------

#[tokio::test]
async fn frontend_disconnect_stops_daemon_cleanly() {
    let scratch = ScratchDir::new();
    let socket_path = scratch.socket_path();

    let agent: Arc<dyn Agent> = Arc::new(FakeStreamingAgent {
        id: AgentId::new(),
        caps: CapabilitySet::empty(),
    });

    let channel = Arc::new(LocalChannel::new("daemon-test", Vec::<u8>::new()));
    let shutdown = CancellationToken::new();

    let daemon_socket = socket_path.clone();
    let daemon_agent = Arc::clone(&agent);
    let daemon_channel = Arc::clone(&channel);
    let daemon_shutdown = shutdown.clone();
    let daemon_handle = tokio::spawn(async move {
        run_daemon_compat(
            &daemon_socket,
            daemon_agent,
            daemon_channel,
            daemon_shutdown,
        )
        .await
        .expect("daemon must complete successfully");
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Connect, read DaemonReady, then immediately drop the connection.
    {
        let stream = UnixStream::connect(&socket_path)
            .await
            .expect("connect to daemon");
        let (mut reader, _writer) = stream.into_split();
        let mut buf = Vec::with_capacity(4096);
        read_more(&mut reader, &mut buf).await;
        // Connection drops here when `stream` (via reader/_writer) goes out of scope.
    }

    // The handler task exits on disconnect; cancel the daemon's
    // accept loop so the daemon itself shuts down.
    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(5), daemon_handle)
        .await
        .expect("daemon must finish within 5s after shutdown")
        .expect("daemon task must not panic");
}

// ---------------------------------------------------------------------------
// Phase 17 Task 4 — DaemonSession multi-turn test
// ---------------------------------------------------------------------------

#[tokio::test]
async fn daemon_session_multi_turn_via_client_library() {
    let scratch = ScratchDir::new();
    let socket_path = scratch.socket_path();

    let agent: Arc<dyn Agent> = Arc::new(FakeStreamingAgent {
        id: AgentId::new(),
        caps: CapabilitySet::empty(),
    });

    let channel = Arc::new(LocalChannel::new("daemon-test", Vec::<u8>::new()));
    let shutdown = CancellationToken::new();

    let daemon_socket = socket_path.clone();
    let daemon_agent = Arc::clone(&agent);
    let daemon_channel = Arc::clone(&channel);
    let daemon_shutdown = shutdown.clone();
    let daemon_handle = tokio::spawn(async move {
        run_daemon_compat(
            &daemon_socket,
            daemon_agent,
            daemon_channel,
            daemon_shutdown,
        )
        .await
        .expect("daemon must complete successfully");
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut session = DaemonSession::connect(&socket_path, None, None)
        .await
        .expect("DaemonSession::connect must succeed");

    assert_eq!(session.daemon_version.as_deref(), Some("0.1"));
    assert!(!session.session_id.is_empty());

    // Turn 1.
    let (events_1, outcome_1) = session
        .submit_input("first turn".into())
        .await
        .expect("turn 1 must succeed");
    assert_eq!(events_1.len(), 2, "turn 1 events: {events_1:?}");
    assert!(outcome_1.contains("Hello from daemon!"));

    // Turn 2.
    let (events_2, outcome_2) = session
        .submit_input("second turn".into())
        .await
        .expect("turn 2 must succeed");
    assert_eq!(events_2.len(), 2, "turn 2 events: {events_2:?}");
    assert!(outcome_2.contains("Hello from daemon!"));

    session.disconnect().await.expect("disconnect must succeed");

    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(5), daemon_handle)
        .await
        .expect("daemon must finish within 5s")
        .expect("daemon task must not panic");
}

// ---------------------------------------------------------------------------
// Phase 17 Task 4 — daemon_is_running utility test
// ---------------------------------------------------------------------------

#[tokio::test]
async fn daemon_is_running_returns_false_for_absent_socket() {
    let scratch = ScratchDir::new();
    let socket_path = scratch.socket_path();
    assert!(
        !daemon_is_running(&socket_path).await,
        "daemon_is_running must return false when no daemon is listening"
    );
}

// ---------------------------------------------------------------------------
// Phase 18 Task 2 — run_daemon_session REPL integration test
// ---------------------------------------------------------------------------

#[tokio::test]
async fn run_daemon_session_renders_two_turns() {
    let scratch = ScratchDir::new();
    let socket_path = scratch.socket_path();

    let agent: Arc<dyn Agent> = Arc::new(FakeStreamingAgent {
        id: AgentId::new(),
        caps: CapabilitySet::empty(),
    });

    let channel = Arc::new(LocalChannel::new("daemon-test", Vec::<u8>::new()));
    let shutdown = CancellationToken::new();

    let daemon_socket = socket_path.clone();
    let daemon_agent = Arc::clone(&agent);
    let daemon_channel = Arc::clone(&channel);
    let daemon_shutdown = shutdown.clone();
    let daemon_handle = tokio::spawn(async move {
        run_daemon_compat(
            &daemon_socket,
            daemon_agent,
            daemon_channel,
            daemon_shutdown,
        )
        .await
        .expect("daemon must complete successfully");
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let config = DaemonSessionConfig {
        socket_path: socket_path.clone(),
        role: None,
        prompt: "> ".into(),
        banner: Some("test banner".into()),
        cancel_flag: None,
        frontend_type: None,
    };

    // Two input lines, then EOF.
    let input = std::io::Cursor::new(b"first turn\nsecond turn\n");
    let mut output = Vec::<u8>::new();

    let report = run_daemon_session(config, input, &mut output)
        .await
        .expect("run_daemon_session must succeed");

    assert_eq!(report.turns_run, 2, "must run exactly 2 turns");
    assert!(report.last_outcome.is_some(), "must have a last outcome");

    let output_str = String::from_utf8(output).expect("output must be valid UTF-8");
    assert!(
        output_str.contains("test banner"),
        "output must contain banner, got: {output_str}"
    );
    assert!(
        output_str.contains("Hello "),
        "output must contain streamed text, got: {output_str}"
    );
    assert!(
        output_str.contains("from daemon!"),
        "output must contain streamed text, got: {output_str}"
    );
    // Three prompts: before turn 1, before turn 2, before the EOF read.
    assert_eq!(
        output_str.matches("> ").count(),
        3,
        "must have exactly 3 prompts in output, got: {output_str}"
    );

    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(5), daemon_handle)
        .await
        .expect("daemon must finish within 5s")
        .expect("daemon task must not panic");
}

// ---------------------------------------------------------------------------
// Phase 18 Task 2 — run_daemon_session banner-only test (empty input)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn run_daemon_session_with_no_input_prints_banner_only() {
    let scratch = ScratchDir::new();
    let socket_path = scratch.socket_path();

    let agent: Arc<dyn Agent> = Arc::new(FakeStreamingAgent {
        id: AgentId::new(),
        caps: CapabilitySet::empty(),
    });

    let channel = Arc::new(LocalChannel::new("daemon-test", Vec::<u8>::new()));
    let shutdown = CancellationToken::new();

    let daemon_socket = socket_path.clone();
    let daemon_agent = Arc::clone(&agent);
    let daemon_channel = Arc::clone(&channel);
    let daemon_shutdown = shutdown.clone();
    let daemon_handle = tokio::spawn(async move {
        run_daemon_compat(
            &daemon_socket,
            daemon_agent,
            daemon_channel,
            daemon_shutdown,
        )
        .await
        .expect("daemon must complete successfully");
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let config = DaemonSessionConfig {
        socket_path: socket_path.clone(),
        role: None,
        prompt: "> ".into(),
        banner: Some("daemon-mode banner".into()),
        cancel_flag: None,
        frontend_type: None,
    };

    // Empty input — immediate EOF.
    let input = std::io::Cursor::new(b"");
    let mut output = Vec::<u8>::new();

    let report = run_daemon_session(config, input, &mut output)
        .await
        .expect("run_daemon_session must succeed");

    assert_eq!(report.turns_run, 0, "no turns should run on empty input");
    assert!(report.last_outcome.is_none(), "no outcome on empty input");

    let output_str = String::from_utf8(output).expect("output must be valid UTF-8");
    assert!(
        output_str.contains("daemon-mode banner"),
        "output must contain banner, got: {output_str}"
    );

    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(5), daemon_handle)
        .await
        .expect("daemon must finish within 5s")
        .expect("daemon task must not panic");
}

// ---------------------------------------------------------------------------
// Phase 18 Task 3 — run_daemon_session_connected (pre-connected) test
// ---------------------------------------------------------------------------

#[tokio::test]
async fn run_daemon_session_connected_with_cancel_handle() {
    let scratch = ScratchDir::new();
    let socket_path = scratch.socket_path();

    let agent: Arc<dyn Agent> = Arc::new(FakeStreamingAgent {
        id: AgentId::new(),
        caps: CapabilitySet::empty(),
    });

    let channel = Arc::new(LocalChannel::new("daemon-test", Vec::<u8>::new()));
    let shutdown = CancellationToken::new();

    let daemon_socket = socket_path.clone();
    let daemon_agent = Arc::clone(&agent);
    let daemon_channel = Arc::clone(&channel);
    let daemon_shutdown = shutdown.clone();
    let daemon_handle = tokio::spawn(async move {
        run_daemon_compat(
            &daemon_socket,
            daemon_agent,
            daemon_channel,
            daemon_shutdown,
        )
        .await
        .expect("daemon must complete successfully");
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Pre-connect (as the binary does in daemon mode).
    let session = DaemonSession::connect(&socket_path, None, None)
        .await
        .expect("connect must succeed");

    let cancel_handle = session.cancel_handle();
    assert!(!session.session_id.is_empty());

    // Verify the cancel handle is cloneable and has the right session ID.
    let _handle2 = cancel_handle.clone();

    let config = DaemonSessionConfig {
        socket_path: socket_path.clone(),
        role: None,
        prompt: "> ".into(),
        banner: Some("pre-connected test".into()),
        cancel_flag: None,
        frontend_type: None,
    };

    let input = std::io::Cursor::new(b"hello\n");
    let mut output = Vec::<u8>::new();

    let report = run_daemon_session_connected(session, config, input, &mut output)
        .await
        .expect("run_daemon_session_connected must succeed");

    assert_eq!(report.turns_run, 1);

    let output_str = String::from_utf8(output).expect("valid UTF-8");
    assert!(output_str.contains("pre-connected test"));
    assert!(output_str.contains("Hello "));
    assert!(output_str.contains("from daemon!"));

    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(5), daemon_handle)
        .await
        .expect("daemon must finish within 5s")
        .expect("daemon task must not panic");
}

#[tokio::test]
async fn cancel_flag_resets_between_turns() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let scratch = ScratchDir::new();
    let socket_path = scratch.socket_path();

    let agent: Arc<dyn Agent> = Arc::new(FakeStreamingAgent {
        id: AgentId::new(),
        caps: CapabilitySet::empty(),
    });

    let channel = Arc::new(LocalChannel::new("daemon-test", Vec::<u8>::new()));
    let shutdown = CancellationToken::new();

    let daemon_socket = socket_path.clone();
    let daemon_agent = Arc::clone(&agent);
    let daemon_channel = Arc::clone(&channel);
    let daemon_shutdown = shutdown.clone();
    let daemon_handle = tokio::spawn(async move {
        run_daemon_compat(
            &daemon_socket,
            daemon_agent,
            daemon_channel,
            daemon_shutdown,
        )
        .await
        .expect("daemon must complete successfully");
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let session = DaemonSession::connect(&socket_path, None, None)
        .await
        .expect("connect must succeed");

    // Simulate a prior cancel: flag starts true.
    let cancel_flag = Arc::new(AtomicBool::new(true));

    let config = DaemonSessionConfig {
        socket_path: socket_path.clone(),
        role: None,
        prompt: "> ".into(),
        banner: Some("flag-reset test".into()),
        cancel_flag: Some(Arc::clone(&cancel_flag)),
        frontend_type: None,
    };

    let input = std::io::Cursor::new(b"turn1\nturn2\n");
    let mut output = Vec::<u8>::new();

    let report = run_daemon_session_connected(session, config, input, &mut output)
        .await
        .expect("session must succeed");

    assert_eq!(report.turns_run, 2);
    // After the last turn completes, the flag should still be false
    // (the REPL resets it before each submit_input).
    assert!(!cancel_flag.load(Ordering::Relaxed));

    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(5), daemon_handle)
        .await
        .expect("daemon must finish within 5s")
        .expect("daemon task must not panic");
}

// ---------------------------------------------------------------------------
// Phase 19 Task 2 — two concurrent connections
// ---------------------------------------------------------------------------

#[tokio::test]
async fn two_concurrent_connections() {
    use aivyx_channel::daemon_server::{ChannelFactory, run_daemon};

    let scratch = ScratchDir::new();
    let socket_path = scratch.socket_path();

    let agent: Arc<dyn Agent> = Arc::new(FakeStreamingAgent {
        id: AgentId::new(),
        caps: CapabilitySet::empty(),
    });

    let channel = Arc::new(LocalChannel::new("daemon-test", Vec::<u8>::new()));
    let shutdown = CancellationToken::new();

    let channel_for_factory: Arc<dyn aivyx_core::ChannelContext + Send + Sync> = channel;
    let factory: ChannelFactory = Arc::new(move |_| Arc::clone(&channel_for_factory));

    let daemon_socket = socket_path.clone();
    let daemon_agent = Arc::clone(&agent);
    let daemon_factory = Arc::clone(&factory);
    let daemon_shutdown = shutdown.clone();
    let daemon_handle = tokio::spawn(async move {
        run_daemon(DaemonConfig {
            socket_path: daemon_socket,
            agent: daemon_agent,
            channel_factory: daemon_factory,
            shutdown: daemon_shutdown,
            mission_store: None,
            notify_dispatcher: None,
            default_notify_target: None,
            notify_targets: Vec::new(),
            schedule_store: None,
            webhook_store: None,
            file_watch_store: None,
            webhook_port: None,
            web_ui_port: None,
            web_ui_host: None,
            web_ui_allowed_origins: Vec::new(),
            web_ui_auth_token: None,
            comfyui_base_url: None,
            memory: None,
            memory_ttl_secs: None,
            audit_log: None,
            profile: Arc::new(Profile::default()),
            persona_log: None,
            shared_persona: aivyx_channel::persona::shared_effective_persona(
                aivyx_channel::persona::EffectivePersona::default(),
            ),
            web_ui_broadcaster: None,
            persona_proposal_log: None,
            reflection_schedules: Vec::new(),
            target_policies: std::collections::HashMap::new(),
            embedding_provider: None,
            recall_log: None,
            helpfulness_ledger: None,
            cooccurrence_ledger: None,
            correction_ledger: None,
            persona_selection_stat: None,
            recall_cluster_stat: None,
            proactive_config: None,
            proactive_log: None,
            proactive_stat: None,
            persona_lifecycle_config: None,
            persona_lifecycle_stat: None,
            memory_retention: Vec::new(),
            conversation_windows: None,
            persona_consolidation_config: None,
            persona_consolidation_stat: None,
            persona_consolidation_phraser: None,
            correction_consolidation_config: None,
            correction_consolidation_stat: None,
            correction_consolidation_phraser: None,
            loop_backlog: None,
            loop_state: None,
            loop_config: None,
            team_missions: None,
            gate_policy: aivyx_core::GatePolicy::default(),
            workspace_journaling_interval: None,
            pricing: Default::default(),
            config_toml_path: None,
            role_override: None,
            team_config_write_path: None,
            seed_draft_llm: None,
            document_roots: Default::default(),
            reminder_store: None,
            wiki_sweep: None,
            wiki_store: None,
            graph_sweep: None,
            graph_store: None,
            conflict_dismissals: None,
            recall_judgment_config: None,
            recall_judgment_stat: None,
            recall_judge: None,
            correction_judgment_config: None,
            correction_judge: None,
            correction_judgment_stat: None,
            correction_signal_config: None,
            recall_feedback_config: None,
            tool_descriptors: Vec::new(),
            skill_auto_proposer: None,
            tool_relevance_ledger: None,
            skill_effectiveness_ledger: None,
            skill_refinement_config: None,
            skill_refinement_drafter: None,
            skill_authoring_config: None,
            skill_authoring_drafter: None,
        })
        .await
        .expect("daemon must complete successfully");
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Connect two clients concurrently.
    let mut session_a = DaemonSession::connect(&socket_path, None, None)
        .await
        .expect("connection A must succeed");
    let mut session_b = DaemonSession::connect(&socket_path, None, None)
        .await
        .expect("connection B must succeed");

    // Both sessions should have different session IDs.
    assert_ne!(session_a.session_id, session_b.session_id);

    // Submit turns on both connections.
    let (events_a, outcome_a) = session_a
        .submit_input("from A".into())
        .await
        .expect("turn A must succeed");
    let (events_b, outcome_b) = session_b
        .submit_input("from B".into())
        .await
        .expect("turn B must succeed");

    assert_eq!(events_a.len(), 2);
    assert_eq!(events_b.len(), 2);
    assert!(outcome_a.contains("Hello from daemon!"));
    assert!(outcome_b.contains("Hello from daemon!"));

    let _ = session_a.disconnect().await;
    let _ = session_b.disconnect().await;

    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(5), daemon_handle)
        .await
        .expect("daemon must finish within 5s")
        .expect("daemon task must not panic");
}

// ---------------------------------------------------------------------------
// Phase 19 Task 2 — connection after disconnect
// ---------------------------------------------------------------------------

#[tokio::test]
async fn connection_after_disconnect() {
    let scratch = ScratchDir::new();
    let socket_path = scratch.socket_path();

    let agent: Arc<dyn Agent> = Arc::new(FakeStreamingAgent {
        id: AgentId::new(),
        caps: CapabilitySet::empty(),
    });

    let channel = Arc::new(LocalChannel::new("daemon-test", Vec::<u8>::new()));
    let shutdown = CancellationToken::new();

    let daemon_socket = socket_path.clone();
    let daemon_agent = Arc::clone(&agent);
    let daemon_channel = Arc::clone(&channel);
    let daemon_shutdown = shutdown.clone();
    let daemon_handle = tokio::spawn(async move {
        run_daemon_compat(
            &daemon_socket,
            daemon_agent,
            daemon_channel,
            daemon_shutdown,
        )
        .await
        .expect("daemon must complete successfully");
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    // First connection: one turn, then disconnect.
    let mut session_1 = DaemonSession::connect(&socket_path, None, None)
        .await
        .expect("connection 1 must succeed");
    let (events_1, _) = session_1
        .submit_input("first".into())
        .await
        .expect("turn 1 must succeed");
    assert_eq!(events_1.len(), 2);
    let _ = session_1.disconnect().await;

    // Small delay to let the handler task finish.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Second connection: the daemon should still be accepting.
    let mut session_2 = DaemonSession::connect(&socket_path, None, None)
        .await
        .expect("connection 2 must succeed after first disconnected");
    let (events_2, _) = session_2
        .submit_input("second".into())
        .await
        .expect("turn 2 must succeed");
    assert_eq!(events_2.len(), 2);
    let _ = session_2.disconnect().await;

    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(5), daemon_handle)
        .await
        .expect("daemon must finish within 5s")
        .expect("daemon task must not panic");
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn read_more(reader: &mut tokio::net::unix::OwnedReadHalf, buf: &mut Vec<u8>) {
    let mut tmp = [0u8; 4096];
    let n = reader.read(&mut tmp).await.expect("read_more");
    assert!(n > 0, "unexpected EOF in read_more");
    buf.extend_from_slice(&tmp[..n]);
}

async fn collect_turn_events(
    reader: &mut tokio::net::unix::OwnedReadHalf,
    buf: &mut Vec<u8>,
) -> (Vec<StreamEventPayload>, String) {
    let mut events = Vec::new();
    loop {
        match decode_frame::<DaemonEnvelope>(buf) {
            Ok((DaemonEnvelope::StreamEvent { event, .. }, consumed)) => {
                buf.drain(..consumed);
                events.push(event);
            }
            Ok((DaemonEnvelope::TurnComplete { outcome, .. }, consumed)) => {
                buf.drain(..consumed);
                return (events, outcome);
            }
            Ok((DaemonEnvelope::Error { code, message }, _)) => {
                panic!("daemon error ({code}): {message}");
            }
            Err(FrameError::IncompleteBuf) => {
                read_more(reader, buf).await;
            }
            Ok((other, consumed)) => {
                buf.drain(..consumed);
                panic!("unexpected message during turn: {other:?}");
            }
            Err(e) => panic!("decode error: {e}"),
        }
    }
}

// ---------------------------------------------------------------------------
// PlatformEchoAgent — echoes the channel's platform in the turn outcome.
// ---------------------------------------------------------------------------

struct PlatformEchoAgent {
    id: AgentId,
    caps: CapabilitySet,
}

#[async_trait]
impl Agent for PlatformEchoAgent {
    fn id(&self) -> AgentId {
        self.id
    }

    fn capabilities(&self) -> &CapabilitySet {
        &self.caps
    }

    async fn turn(&self, _message: Message, channel: &dyn ChannelContext) -> TurnOutcome {
        let platform = format!("{:?}", channel.platform());
        let tier = format!("{:?}", channel.trust_tier());
        let text = format!("platform={platform} tier={tier}");
        let _ = channel.stream_event(StreamEvent::Text(&text)).await;

        TurnOutcome::Completed {
            final_message: text,
            tool_calls_made: 0,
            duration: Duration::from_millis(1),
        }
    }
}

// ---------------------------------------------------------------------------
// TelegramDaemonChannel — identity stub for Telegram frontend type tests.
// ---------------------------------------------------------------------------

struct TestTelegramChannel {
    session: aivyx_core::SessionId,
    token: CancellationToken,
}

impl TestTelegramChannel {
    fn new() -> Self {
        TestTelegramChannel {
            session: aivyx_core::SessionId::new(),
            token: CancellationToken::new(),
        }
    }
}

#[async_trait]
impl ChannelContext for TestTelegramChannel {
    fn channel_name(&self) -> &str {
        "test-telegram-daemon"
    }

    fn platform(&self) -> aivyx_core::ChannelPlatform {
        aivyx_core::ChannelPlatform::Telegram
    }

    fn trust_tier(&self) -> aivyx_capability::TrustTier {
        aivyx_capability::TrustTier::SemiTrusted
    }

    fn session_id(&self) -> aivyx_core::SessionId {
        self.session
    }

    async fn stream_event(&self, _event: StreamEvent<'_>) -> Result<(), aivyx_core::ChannelError> {
        Ok(())
    }

    async fn finalize(&self, _outcome: &TurnOutcome) -> Result<(), aivyx_core::ChannelError> {
        Ok(())
    }

    fn cancellation_token(&self) -> CancellationToken {
        self.token.clone()
    }
}

// ---------------------------------------------------------------------------
// Phase 19 Task 3 — Telegram frontend type dispatches through ChannelFactory.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn telegram_frontend_type_gets_telegram_channel() {
    let scratch = ScratchDir::new();
    let socket_path = scratch.socket_path();

    let agent: Arc<dyn Agent> = Arc::new(PlatformEchoAgent {
        id: AgentId::new(),
        caps: CapabilitySet::empty(),
    });

    let factory: ChannelFactory = Arc::new(|ft| match ft {
        FrontendType::Telegram => Arc::new(TestTelegramChannel::new()),
        FrontendType::Local | FrontendType::Web | FrontendType::Discord | FrontendType::Slack => {
            Arc::new(LocalChannel::new("test-local", Vec::<u8>::new()))
        }
    });

    let shutdown = CancellationToken::new();
    let daemon_socket = socket_path.clone();
    let daemon_agent = Arc::clone(&agent);
    let daemon_shutdown = shutdown.clone();
    let daemon_handle = tokio::spawn(async move {
        run_daemon(DaemonConfig {
            socket_path: daemon_socket,
            agent: daemon_agent,
            channel_factory: factory,
            shutdown: daemon_shutdown,
            mission_store: None,
            notify_dispatcher: None,
            default_notify_target: None,
            notify_targets: Vec::new(),
            schedule_store: None,
            webhook_store: None,
            file_watch_store: None,
            webhook_port: None,
            web_ui_port: None,
            web_ui_host: None,
            web_ui_allowed_origins: Vec::new(),
            web_ui_auth_token: None,
            comfyui_base_url: None,
            memory: None,
            memory_ttl_secs: None,
            audit_log: None,
            profile: Arc::new(Profile::default()),
            persona_log: None,
            shared_persona: aivyx_channel::persona::shared_effective_persona(
                aivyx_channel::persona::EffectivePersona::default(),
            ),
            web_ui_broadcaster: None,
            persona_proposal_log: None,
            reflection_schedules: Vec::new(),
            target_policies: std::collections::HashMap::new(),
            embedding_provider: None,
            recall_log: None,
            helpfulness_ledger: None,
            cooccurrence_ledger: None,
            correction_ledger: None,
            persona_selection_stat: None,
            recall_cluster_stat: None,
            proactive_config: None,
            proactive_log: None,
            proactive_stat: None,
            persona_lifecycle_config: None,
            persona_lifecycle_stat: None,
            memory_retention: Vec::new(),
            conversation_windows: None,
            persona_consolidation_config: None,
            persona_consolidation_stat: None,
            persona_consolidation_phraser: None,
            correction_consolidation_config: None,
            correction_consolidation_stat: None,
            correction_consolidation_phraser: None,
            loop_backlog: None,
            loop_state: None,
            loop_config: None,
            team_missions: None,
            gate_policy: aivyx_core::GatePolicy::default(),
            workspace_journaling_interval: None,
            pricing: Default::default(),
            config_toml_path: None,
            role_override: None,
            team_config_write_path: None,
            seed_draft_llm: None,
            document_roots: Default::default(),
            reminder_store: None,
            wiki_sweep: None,
            wiki_store: None,
            graph_sweep: None,
            graph_store: None,
            conflict_dismissals: None,
            recall_judgment_config: None,
            recall_judgment_stat: None,
            recall_judge: None,
            correction_judgment_config: None,
            correction_judge: None,
            correction_judgment_stat: None,
            correction_signal_config: None,
            recall_feedback_config: None,
            tool_descriptors: Vec::new(),
            skill_auto_proposer: None,
            tool_relevance_ledger: None,
            skill_effectiveness_ledger: None,
            skill_refinement_config: None,
            skill_refinement_drafter: None,
            skill_authoring_config: None,
            skill_authoring_drafter: None,
        })
        .await
        .expect("daemon must complete successfully");
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut session = DaemonSession::connect(&socket_path, None, Some(FrontendType::Telegram))
        .await
        .expect("connect must succeed");

    let (events, outcome) = session
        .submit_input("hello".to_string())
        .await
        .expect("submit must succeed");

    assert!(
        outcome.contains("Telegram"),
        "outcome must report Telegram platform: {outcome}"
    );
    assert!(
        outcome.contains("SemiTrusted"),
        "outcome must report SemiTrusted tier: {outcome}"
    );
    assert!(!events.is_empty(), "must receive at least one stream event");

    let _ = session.disconnect().await;

    shutdown.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(5), daemon_handle).await;
}

#[tokio::test]
async fn mixed_local_and_telegram_frontends_on_same_daemon() {
    let scratch = ScratchDir::new();
    let socket_path = scratch.socket_path();

    let agent: Arc<dyn Agent> = Arc::new(PlatformEchoAgent {
        id: AgentId::new(),
        caps: CapabilitySet::empty(),
    });

    let factory: ChannelFactory = Arc::new(|ft| match ft {
        FrontendType::Telegram => Arc::new(TestTelegramChannel::new()),
        FrontendType::Local | FrontendType::Web | FrontendType::Discord | FrontendType::Slack => {
            Arc::new(LocalChannel::new("test-local", Vec::<u8>::new()))
        }
    });

    let shutdown = CancellationToken::new();
    let daemon_socket = socket_path.clone();
    let daemon_agent = Arc::clone(&agent);
    let daemon_shutdown = shutdown.clone();
    let daemon_handle = tokio::spawn(async move {
        run_daemon(DaemonConfig {
            socket_path: daemon_socket,
            agent: daemon_agent,
            channel_factory: factory,
            shutdown: daemon_shutdown,
            mission_store: None,
            notify_dispatcher: None,
            default_notify_target: None,
            notify_targets: Vec::new(),
            schedule_store: None,
            webhook_store: None,
            file_watch_store: None,
            webhook_port: None,
            web_ui_port: None,
            web_ui_host: None,
            web_ui_allowed_origins: Vec::new(),
            web_ui_auth_token: None,
            comfyui_base_url: None,
            memory: None,
            memory_ttl_secs: None,
            audit_log: None,
            profile: Arc::new(Profile::default()),
            persona_log: None,
            shared_persona: aivyx_channel::persona::shared_effective_persona(
                aivyx_channel::persona::EffectivePersona::default(),
            ),
            web_ui_broadcaster: None,
            persona_proposal_log: None,
            reflection_schedules: Vec::new(),
            target_policies: std::collections::HashMap::new(),
            embedding_provider: None,
            recall_log: None,
            helpfulness_ledger: None,
            cooccurrence_ledger: None,
            correction_ledger: None,
            persona_selection_stat: None,
            recall_cluster_stat: None,
            proactive_config: None,
            proactive_log: None,
            proactive_stat: None,
            persona_lifecycle_config: None,
            persona_lifecycle_stat: None,
            memory_retention: Vec::new(),
            conversation_windows: None,
            persona_consolidation_config: None,
            persona_consolidation_stat: None,
            persona_consolidation_phraser: None,
            correction_consolidation_config: None,
            correction_consolidation_stat: None,
            correction_consolidation_phraser: None,
            loop_backlog: None,
            loop_state: None,
            loop_config: None,
            team_missions: None,
            gate_policy: aivyx_core::GatePolicy::default(),
            workspace_journaling_interval: None,
            pricing: Default::default(),
            config_toml_path: None,
            role_override: None,
            team_config_write_path: None,
            seed_draft_llm: None,
            document_roots: Default::default(),
            reminder_store: None,
            wiki_sweep: None,
            wiki_store: None,
            graph_sweep: None,
            graph_store: None,
            conflict_dismissals: None,
            recall_judgment_config: None,
            recall_judgment_stat: None,
            recall_judge: None,
            correction_judgment_config: None,
            correction_judge: None,
            correction_judgment_stat: None,
            correction_signal_config: None,
            recall_feedback_config: None,
            tool_descriptors: Vec::new(),
            skill_auto_proposer: None,
            tool_relevance_ledger: None,
            skill_effectiveness_ledger: None,
            skill_refinement_config: None,
            skill_refinement_drafter: None,
            skill_authoring_config: None,
            skill_authoring_drafter: None,
        })
        .await
        .expect("daemon must complete successfully");
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Connect a Local frontend.
    let mut local_session = DaemonSession::connect(&socket_path, None, Some(FrontendType::Local))
        .await
        .expect("local connect must succeed");

    // Connect a Telegram frontend.
    let mut tg_session = DaemonSession::connect(&socket_path, None, Some(FrontendType::Telegram))
        .await
        .expect("telegram connect must succeed");

    // Submit turns on both.
    let (_local_events, local_outcome) = local_session
        .submit_input("hi".to_string())
        .await
        .expect("local submit must succeed");

    let (_tg_events, tg_outcome) = tg_session
        .submit_input("hi".to_string())
        .await
        .expect("telegram submit must succeed");

    // Local should report Local platform + Trusted tier.
    assert!(
        local_outcome.contains("Local"),
        "local outcome must report Local platform: {local_outcome}"
    );
    assert!(
        local_outcome.contains("Trusted"),
        "local outcome must report Trusted tier: {local_outcome}"
    );

    // Telegram should report Telegram platform + SemiTrusted tier.
    assert!(
        tg_outcome.contains("Telegram"),
        "telegram outcome must report Telegram platform: {tg_outcome}"
    );
    assert!(
        tg_outcome.contains("SemiTrusted"),
        "telegram outcome must report SemiTrusted tier: {tg_outcome}"
    );

    // Different session IDs.
    assert_ne!(
        local_session.session_id, tg_session.session_id,
        "different frontends must get different session IDs"
    );

    let _ = local_session.disconnect().await;
    let _ = tg_session.disconnect().await;

    shutdown.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(5), daemon_handle).await;
}

// ---------------------------------------------------------------------------
// Phase 20 Task 2 — daemon_stop triggers graceful shutdown
// ---------------------------------------------------------------------------

#[tokio::test]
async fn daemon_stop_triggers_graceful_shutdown() {
    let scratch = ScratchDir::new();
    let socket_path = scratch.socket_path();

    let agent: Arc<dyn Agent> = Arc::new(FakeStreamingAgent {
        id: AgentId::new(),
        caps: CapabilitySet::empty(),
    });

    let channel = Arc::new(LocalChannel::new("daemon-stop-test", Vec::<u8>::new()));
    let shutdown = CancellationToken::new();

    let daemon_socket = socket_path.clone();
    let daemon_agent = Arc::clone(&agent);
    let daemon_channel = Arc::clone(&channel);
    let daemon_shutdown = shutdown.clone();
    let daemon_handle = tokio::spawn(async move {
        run_daemon_compat(
            &daemon_socket,
            daemon_agent,
            daemon_channel,
            daemon_shutdown,
        )
        .await
        .expect("daemon must complete successfully");
    });

    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        daemon_is_running(&socket_path).await,
        "daemon must be running before stop"
    );

    let reason = aivyx_channel::daemon_client::daemon_stop(&socket_path)
        .await
        .expect("daemon_stop must succeed");
    assert!(
        reason.contains("operator requested"),
        "shutdown reason must mention operator: {reason}"
    );

    tokio::time::timeout(Duration::from_secs(5), daemon_handle)
        .await
        .expect("daemon must exit within 5s after stop")
        .expect("daemon task must not panic");
}

#[tokio::test]
async fn daemon_status_reports_running_daemon() {
    let scratch = ScratchDir::new();
    let socket_path = scratch.socket_path();

    let agent: Arc<dyn Agent> = Arc::new(FakeStreamingAgent {
        id: AgentId::new(),
        caps: CapabilitySet::empty(),
    });

    let channel = Arc::new(LocalChannel::new("daemon-status-test", Vec::<u8>::new()));
    let shutdown = CancellationToken::new();

    let daemon_socket = socket_path.clone();
    let daemon_agent = Arc::clone(&agent);
    let daemon_channel = Arc::clone(&channel);
    let daemon_shutdown = shutdown.clone();
    let daemon_handle = tokio::spawn(async move {
        run_daemon_compat(
            &daemon_socket,
            daemon_agent,
            daemon_channel,
            daemon_shutdown,
        )
        .await
        .expect("daemon must complete successfully");
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let info = aivyx_channel::daemon_client::daemon_status(&socket_path).await;
    assert!(info.running, "daemon must report as running");
    assert_eq!(
        info.version.as_deref(),
        Some("0.1"),
        "daemon must report protocol version 0.1"
    );

    shutdown.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(5), daemon_handle).await;
}

#[tokio::test]
async fn daemon_status_reports_not_running_for_absent_socket() {
    let scratch = ScratchDir::new();
    let socket_path = scratch.socket_path();

    let info = aivyx_channel::daemon_client::daemon_status(&socket_path).await;
    assert!(!info.running, "daemon must report as not running");
    assert!(
        info.version.is_none(),
        "version must be None when not running"
    );
}

// ---------------------------------------------------------------------------
// Phase 20 Task 3 — PID file lifecycle
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pid_file_appears_on_daemon_start_and_disappears_on_stop() {
    let scratch = ScratchDir::new();
    let socket_path = scratch.socket_path();
    let pid_path = socket_path.with_extension("pid");

    assert!(
        !pid_path.exists(),
        "PID file must not exist before daemon start"
    );

    let agent: Arc<dyn Agent> = Arc::new(FakeStreamingAgent {
        id: AgentId::new(),
        caps: CapabilitySet::empty(),
    });

    let channel = Arc::new(LocalChannel::new("pid-test", Vec::<u8>::new()));
    let shutdown = CancellationToken::new();

    let daemon_socket = socket_path.clone();
    let daemon_agent = Arc::clone(&agent);
    let daemon_channel = Arc::clone(&channel);
    let daemon_shutdown = shutdown.clone();
    let daemon_handle = tokio::spawn(async move {
        run_daemon_compat(
            &daemon_socket,
            daemon_agent,
            daemon_channel,
            daemon_shutdown,
        )
        .await
        .expect("daemon must complete successfully");
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    assert!(pid_path.exists(), "PID file must exist while daemon runs");
    let pid_content = std::fs::read_to_string(&pid_path).expect("PID file must be readable");
    let pid: u32 = pid_content
        .trim()
        .parse()
        .expect("PID file must contain a valid u32");
    assert!(pid > 0, "PID must be positive");

    shutdown.cancel();
    tokio::time::timeout(Duration::from_secs(5), daemon_handle)
        .await
        .expect("daemon must exit within 5s")
        .expect("daemon task must not panic");

    assert!(
        !pid_path.exists(),
        "PID file must be removed after daemon shutdown"
    );
}

#[tokio::test]
async fn daemon_status_includes_pid_from_pid_file() {
    let scratch = ScratchDir::new();
    let socket_path = scratch.socket_path();

    let agent: Arc<dyn Agent> = Arc::new(FakeStreamingAgent {
        id: AgentId::new(),
        caps: CapabilitySet::empty(),
    });

    let channel = Arc::new(LocalChannel::new("pid-status-test", Vec::<u8>::new()));
    let shutdown = CancellationToken::new();

    let daemon_socket = socket_path.clone();
    let daemon_agent = Arc::clone(&agent);
    let daemon_channel = Arc::clone(&channel);
    let daemon_shutdown = shutdown.clone();
    let daemon_handle = tokio::spawn(async move {
        run_daemon_compat(
            &daemon_socket,
            daemon_agent,
            daemon_channel,
            daemon_shutdown,
        )
        .await
        .expect("daemon must complete successfully");
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let info = aivyx_channel::daemon_client::daemon_status(&socket_path).await;
    assert!(info.running, "daemon must report as running");
    assert!(info.pid.is_some(), "daemon status must include PID");
    assert!(info.pid.unwrap() > 0, "PID must be positive");

    shutdown.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(5), daemon_handle).await;
}

#[test]
fn read_pid_file_returns_none_for_missing_file() {
    let result = aivyx_channel::daemon_client::read_pid_file(std::path::Path::new(
        "/nonexistent/daemon.pid",
    ));
    assert!(result.is_none());
}

#[test]
fn read_pid_file_returns_none_for_non_numeric_content() {
    let dir = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string());
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos();
    let path = std::path::PathBuf::from(dir).join(format!("aivyx-pid-test-{pid}-{nanos}.pid"));
    std::fs::write(&path, "not-a-number").expect("write test PID file");
    let result = aivyx_channel::daemon_client::read_pid_file(&path);
    let _ = std::fs::remove_file(&path);
    assert!(result.is_none());
}

// ---------------------------------------------------------------------------
// FakeEscalatingAgent — returns TurnOutcome::Escalated on the first turn,
// then Completed on subsequent turns (simulating the resume after gate
// approval).
// ---------------------------------------------------------------------------

struct FakeEscalatingAgent {
    id: AgentId,
    caps: CapabilitySet,
    turn_count: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl Agent for FakeEscalatingAgent {
    fn id(&self) -> AgentId {
        self.id
    }

    fn capabilities(&self) -> &CapabilitySet {
        &self.caps
    }

    async fn turn(&self, _message: Message, channel: &dyn ChannelContext) -> TurnOutcome {
        let n = self
            .turn_count
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if n == 0 {
            let _ = channel
                .stream_event(StreamEvent::Text("escalating..."))
                .await;
            TurnOutcome::Escalated {
                reason: "requires approval".into(),
                pending_tool: aivyx_core::ToolId::new(),
                scope: None,
                tool_calls_made: 1,
            }
        } else {
            let _ = channel
                .stream_event(StreamEvent::Text("resumed after approval"))
                .await;
            TurnOutcome::Completed {
                final_message: "mission continued".into(),
                tool_calls_made: 0,
                duration: Duration::from_millis(1),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Escalation→gate turn-loop wiring integration test (Phase 23 Task 2).
//
// Verifies: submit a turn with mission_id → agent escalates →
// daemon creates gate + emits ApprovalGate → resolve gate approved →
// daemon resumes turn → TurnComplete.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn escalation_gate_wiring_approve_resumes_turn() {
    use aivyx_channel::mission::{self, MissionRecord};
    use aivyx_crypto::MasterKey;
    use aivyx_storage::{KeyDomain, RedbStorage, Storage, StorageConfig};

    let scratch = ScratchDir::new();
    let socket_path = scratch.socket_path();
    let daemon_socket = socket_path.clone();

    let store_path = scratch.path.join("test.redb");
    let storage: Arc<dyn Storage> = RedbStorage::open(
        StorageConfig::new(store_path),
        MasterKey::from_raw([7u8; 32]),
    )
    .await
    .expect("storage must open");
    let mission_handle = storage.domain(KeyDomain::Missions);

    let mission_id = format!("m-{}", uuid::Uuid::new_v4());
    let record = MissionRecord::new(mission_id.clone(), "default".into(), "test mission".into());
    mission::create_mission(&mission_handle, &record)
        .await
        .expect("create mission must succeed");

    let mut record = mission::get_mission(&mission_handle, &mission_id)
        .await
        .expect("get mission")
        .expect("mission must exist");
    mission::transition_to_running(&mut record).expect("start mission");
    mission::update_mission(&mission_handle, &record)
        .await
        .expect("persist started mission");

    let verify_handle = storage.domain(KeyDomain::Missions);

    let agent: Arc<dyn Agent + Send + Sync> = Arc::new(FakeEscalatingAgent {
        id: AgentId::new(),
        caps: CapabilitySet::empty(),
        turn_count: std::sync::atomic::AtomicUsize::new(0),
    });
    let daemon_agent = Arc::clone(&agent);
    let shutdown = CancellationToken::new();
    let daemon_shutdown = shutdown.clone();

    let factory: ChannelFactory = Arc::new(move |_ft| {
        let ch: Arc<dyn ChannelContext + Send + Sync> =
            Arc::new(LocalChannel::new("gate-test", Vec::<u8>::new()));
        ch
    });

    let daemon_handle = tokio::spawn(async move {
        run_daemon(DaemonConfig {
            socket_path: daemon_socket,
            agent: daemon_agent,
            channel_factory: factory,
            shutdown: daemon_shutdown,
            notify_dispatcher: None,
            default_notify_target: None,
            notify_targets: Vec::new(),
            mission_store: Some(mission_handle),
            schedule_store: None,
            webhook_store: None,
            file_watch_store: None,
            webhook_port: None,
            web_ui_port: None,
            web_ui_host: None,
            web_ui_allowed_origins: Vec::new(),
            web_ui_auth_token: None,
            comfyui_base_url: None,
            memory: None,
            memory_ttl_secs: None,
            audit_log: None,
            profile: Arc::new(Profile::default()),
            persona_log: None,
            shared_persona: aivyx_channel::persona::shared_effective_persona(
                aivyx_channel::persona::EffectivePersona::default(),
            ),
            web_ui_broadcaster: None,
            persona_proposal_log: None,
            reflection_schedules: Vec::new(),
            target_policies: std::collections::HashMap::new(),
            embedding_provider: None,
            recall_log: None,
            helpfulness_ledger: None,
            cooccurrence_ledger: None,
            correction_ledger: None,
            persona_selection_stat: None,
            recall_cluster_stat: None,
            proactive_config: None,
            proactive_log: None,
            proactive_stat: None,
            persona_lifecycle_config: None,
            persona_lifecycle_stat: None,
            memory_retention: Vec::new(),
            conversation_windows: None,
            persona_consolidation_config: None,
            persona_consolidation_stat: None,
            persona_consolidation_phraser: None,
            correction_consolidation_config: None,
            correction_consolidation_stat: None,
            correction_consolidation_phraser: None,
            loop_backlog: None,
            loop_state: None,
            loop_config: None,
            team_missions: None,
            gate_policy: aivyx_core::GatePolicy::default(),
            workspace_journaling_interval: None,
            pricing: Default::default(),
            config_toml_path: None,
            role_override: None,
            team_config_write_path: None,
            seed_draft_llm: None,
            document_roots: Default::default(),
            reminder_store: None,
            wiki_sweep: None,
            wiki_store: None,
            graph_sweep: None,
            graph_store: None,
            conflict_dismissals: None,
            recall_judgment_config: None,
            recall_judgment_stat: None,
            recall_judge: None,
            correction_judgment_config: None,
            correction_judge: None,
            correction_judgment_stat: None,
            correction_signal_config: None,
            recall_feedback_config: None,
            tool_descriptors: Vec::new(),
            skill_auto_proposer: None,
            tool_relevance_ledger: None,
            skill_effectiveness_ledger: None,
            skill_refinement_config: None,
            skill_refinement_drafter: None,
            skill_authoring_config: None,
            skill_authoring_drafter: None,
        })
        .await
        .expect("daemon must complete successfully");
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let stream = UnixStream::connect(&socket_path)
        .await
        .expect("connect to daemon");
    let (mut reader, mut writer) = stream.into_split();

    let mut buf = Vec::new();

    // --- Handshake ---
    read_more(&mut reader, &mut buf).await;
    match decode_frame::<DaemonEnvelope>(&buf) {
        Ok((DaemonEnvelope::DaemonReady { .. }, consumed)) => {
            buf.drain(..consumed);
        }
        other => panic!("expected DaemonReady, got {other:?}"),
    }

    let start = FrontendMessage::StartSession {
        role: None,
        frontend_type: Some(FrontendType::Local),
    };
    let frame = encode_frame(&start).unwrap();
    writer.write_all(&frame).await.unwrap();

    loop {
        read_more(&mut reader, &mut buf).await;
        match decode_frame::<DaemonEnvelope>(&buf) {
            Ok((DaemonEnvelope::SessionStarted { session_id, .. }, consumed)) => {
                buf.drain(..consumed);
                let _ = session_id;
                break;
            }
            Err(FrameError::IncompleteBuf) => continue,
            other => panic!("expected SessionStarted, got {other:?}"),
        }
    }

    // --- Turn 1: submit with mission_id → expect escalation + gate ---
    let submit = FrontendMessage::SubmitInput {
        session_id: "s1".into(),
        text: "do something risky".into(),
        mission_id: Some(mission_id.clone()),
        attachments: vec![],
        headless: false,
    };
    let frame = encode_frame(&submit).unwrap();
    writer.write_all(&frame).await.unwrap();

    let mut gate_event: Option<(String, String)> = None;
    #[allow(unused_assignments)] // Initial false is the default; overwritten in loop body
    let mut saw_escalated_outcome = false;
    let mut events = Vec::new();

    loop {
        match decode_frame::<DaemonEnvelope>(&buf) {
            Ok((DaemonEnvelope::StreamEvent { event, .. }, consumed)) => {
                buf.drain(..consumed);
                if let StreamEventPayload::ApprovalGate {
                    ref mission_id,
                    ref gate_id,
                    ..
                } = event
                {
                    gate_event = Some((mission_id.clone(), gate_id.clone()));
                }
                events.push(event);
            }
            Ok((DaemonEnvelope::TurnComplete { outcome, .. }, consumed)) => {
                buf.drain(..consumed);
                assert!(
                    outcome.contains("escalated"),
                    "expected escalated outcome, got: {outcome}"
                );
                saw_escalated_outcome = true;
                break;
            }
            Ok((DaemonEnvelope::Error { code, message }, _)) => {
                panic!("daemon error ({code}): {message}");
            }
            Err(FrameError::IncompleteBuf) => {
                read_more(&mut reader, &mut buf).await;
            }
            Ok((other, consumed)) => {
                buf.drain(..consumed);
                panic!("unexpected message: {other:?}");
            }
            Err(e) => panic!("decode error: {e}"),
        }
    }

    assert!(saw_escalated_outcome, "must see escalated TurnComplete");
    let (gate_mid, gate_gid) = gate_event.expect("must receive ApprovalGate stream event");
    assert_eq!(gate_mid, mission_id, "gate mission_id must match");

    // Verify mission is now GatePending in storage.
    let stored = mission::get_mission(&verify_handle, &mission_id)
        .await
        .expect("get mission")
        .expect("mission must exist");
    assert_eq!(
        stored.state,
        aivyx_channel::mission::MissionState::GatePending,
        "mission must be GatePending after escalation"
    );
    assert_eq!(stored.gates.len(), 1, "must have exactly one gate");

    // --- Resolve gate (approved) → expect resume turn ---
    let resolve = FrontendMessage::ResolveGate {
        mission_id: mission_id.clone(),
        gate_id: gate_gid.clone(),
        approved: true,
    };
    let frame = encode_frame(&resolve).unwrap();
    writer.write_all(&frame).await.unwrap();

    let mut saw_gate_resolved = false;
    #[allow(unused_assignments)]
    let mut saw_resume_complete = false;

    loop {
        match decode_frame::<DaemonEnvelope>(&buf) {
            Ok((DaemonEnvelope::GateResolved { approved, .. }, consumed)) => {
                buf.drain(..consumed);
                assert!(approved, "gate must be approved");
                saw_gate_resolved = true;
            }
            Ok((DaemonEnvelope::StreamEvent { event, .. }, consumed)) => {
                buf.drain(..consumed);
                events.push(event);
            }
            Ok((DaemonEnvelope::TurnComplete { outcome, .. }, consumed)) => {
                buf.drain(..consumed);
                assert!(
                    outcome.contains("mission continued"),
                    "resume outcome: {outcome}"
                );
                saw_resume_complete = true;
                break;
            }
            Ok((DaemonEnvelope::Error { code, message }, _)) => {
                panic!("daemon error during resume ({code}): {message}");
            }
            Err(FrameError::IncompleteBuf) => {
                read_more(&mut reader, &mut buf).await;
            }
            Ok((other, consumed)) => {
                buf.drain(..consumed);
                panic!("unexpected message during resume: {other:?}");
            }
            Err(e) => panic!("decode error during resume: {e}"),
        }
    }

    assert!(saw_gate_resolved, "must see GateResolved");
    assert!(saw_resume_complete, "must see resume TurnComplete");

    // Verify mission is back to Running after gate approval.
    let stored = mission::get_mission(&verify_handle, &mission_id)
        .await
        .expect("get mission")
        .expect("mission must exist");
    assert_eq!(
        stored.state,
        aivyx_channel::mission::MissionState::Running,
        "mission must be Running after approved gate"
    );

    // Clean up.
    let disconnect = FrontendMessage::Disconnect;
    let frame = encode_frame(&disconnect).unwrap();
    let _ = writer.write_all(&frame).await;

    shutdown.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(5), daemon_handle).await;
}

#[tokio::test]
async fn escalation_gate_wiring_reject_fails_mission() {
    use aivyx_channel::mission::{self, MissionRecord};
    use aivyx_crypto::MasterKey;
    use aivyx_storage::{KeyDomain, RedbStorage, Storage, StorageConfig};

    let scratch = ScratchDir::new();
    let socket_path = scratch.socket_path();
    let daemon_socket = socket_path.clone();

    let store_path = scratch.path.join("test.redb");
    let storage: Arc<dyn Storage> = RedbStorage::open(
        StorageConfig::new(store_path),
        MasterKey::from_raw([7u8; 32]),
    )
    .await
    .expect("storage must open");
    let mission_handle = storage.domain(KeyDomain::Missions);

    let mission_id = format!("m-{}", uuid::Uuid::new_v4());
    let record = MissionRecord::new(mission_id.clone(), "default".into(), "test mission".into());
    mission::create_mission(&mission_handle, &record)
        .await
        .expect("create mission");

    let mut record = mission::get_mission(&mission_handle, &mission_id)
        .await
        .expect("get")
        .expect("exists");
    mission::transition_to_running(&mut record).expect("start");
    mission::update_mission(&mission_handle, &record)
        .await
        .expect("persist");

    let verify_handle = storage.domain(KeyDomain::Missions);

    let agent: Arc<dyn Agent + Send + Sync> = Arc::new(FakeEscalatingAgent {
        id: AgentId::new(),
        caps: CapabilitySet::empty(),
        turn_count: std::sync::atomic::AtomicUsize::new(0),
    });
    let daemon_agent = Arc::clone(&agent);
    let shutdown = CancellationToken::new();
    let daemon_shutdown = shutdown.clone();

    let factory: ChannelFactory = Arc::new(move |_ft| {
        let ch: Arc<dyn ChannelContext + Send + Sync> =
            Arc::new(LocalChannel::new("gate-test", Vec::<u8>::new()));
        ch
    });

    let daemon_handle = tokio::spawn(async move {
        run_daemon(DaemonConfig {
            socket_path: daemon_socket,
            agent: daemon_agent,
            channel_factory: factory,
            shutdown: daemon_shutdown,
            notify_dispatcher: None,
            default_notify_target: None,
            notify_targets: Vec::new(),
            mission_store: Some(mission_handle),
            schedule_store: None,
            webhook_store: None,
            file_watch_store: None,
            webhook_port: None,
            web_ui_port: None,
            web_ui_host: None,
            web_ui_allowed_origins: Vec::new(),
            web_ui_auth_token: None,
            comfyui_base_url: None,
            memory: None,
            memory_ttl_secs: None,
            audit_log: None,
            profile: Arc::new(Profile::default()),
            persona_log: None,
            shared_persona: aivyx_channel::persona::shared_effective_persona(
                aivyx_channel::persona::EffectivePersona::default(),
            ),
            web_ui_broadcaster: None,
            persona_proposal_log: None,
            reflection_schedules: Vec::new(),
            target_policies: std::collections::HashMap::new(),
            embedding_provider: None,
            recall_log: None,
            helpfulness_ledger: None,
            cooccurrence_ledger: None,
            correction_ledger: None,
            persona_selection_stat: None,
            recall_cluster_stat: None,
            proactive_config: None,
            proactive_log: None,
            proactive_stat: None,
            persona_lifecycle_config: None,
            persona_lifecycle_stat: None,
            memory_retention: Vec::new(),
            conversation_windows: None,
            persona_consolidation_config: None,
            persona_consolidation_stat: None,
            persona_consolidation_phraser: None,
            correction_consolidation_config: None,
            correction_consolidation_stat: None,
            correction_consolidation_phraser: None,
            loop_backlog: None,
            loop_state: None,
            loop_config: None,
            team_missions: None,
            gate_policy: aivyx_core::GatePolicy::default(),
            workspace_journaling_interval: None,
            pricing: Default::default(),
            config_toml_path: None,
            role_override: None,
            team_config_write_path: None,
            seed_draft_llm: None,
            document_roots: Default::default(),
            reminder_store: None,
            wiki_sweep: None,
            wiki_store: None,
            graph_sweep: None,
            graph_store: None,
            conflict_dismissals: None,
            recall_judgment_config: None,
            recall_judgment_stat: None,
            recall_judge: None,
            correction_judgment_config: None,
            correction_judge: None,
            correction_judgment_stat: None,
            correction_signal_config: None,
            recall_feedback_config: None,
            tool_descriptors: Vec::new(),
            skill_auto_proposer: None,
            tool_relevance_ledger: None,
            skill_effectiveness_ledger: None,
            skill_refinement_config: None,
            skill_refinement_drafter: None,
            skill_authoring_config: None,
            skill_authoring_drafter: None,
        })
        .await
        .expect("daemon must complete");
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let stream = UnixStream::connect(&socket_path).await.expect("connect");
    let (mut reader, mut writer) = stream.into_split();
    let mut buf = Vec::new();

    // Handshake.
    read_more(&mut reader, &mut buf).await;
    match decode_frame::<DaemonEnvelope>(&buf) {
        Ok((DaemonEnvelope::DaemonReady { .. }, consumed)) => buf.drain(..consumed),
        other => panic!("expected DaemonReady: {other:?}"),
    };

    let start = FrontendMessage::StartSession {
        role: None,
        frontend_type: Some(FrontendType::Local),
    };
    writer
        .write_all(&encode_frame(&start).unwrap())
        .await
        .unwrap();
    loop {
        read_more(&mut reader, &mut buf).await;
        match decode_frame::<DaemonEnvelope>(&buf) {
            Ok((DaemonEnvelope::SessionStarted { .. }, consumed)) => {
                buf.drain(..consumed);
                break;
            }
            Err(FrameError::IncompleteBuf) => continue,
            other => panic!("expected SessionStarted: {other:?}"),
        }
    }

    // Submit with mission → triggers escalation.
    let submit = FrontendMessage::SubmitInput {
        session_id: "s1".into(),
        text: "do something".into(),
        mission_id: Some(mission_id.clone()),
        attachments: vec![],
        headless: false,
    };
    writer
        .write_all(&encode_frame(&submit).unwrap())
        .await
        .unwrap();

    let mut gate_gid = String::new();
    loop {
        match decode_frame::<DaemonEnvelope>(&buf) {
            Ok((DaemonEnvelope::StreamEvent { event, .. }, consumed)) => {
                buf.drain(..consumed);
                if let StreamEventPayload::ApprovalGate { gate_id, .. } = &event {
                    gate_gid = gate_id.clone();
                }
            }
            Ok((DaemonEnvelope::TurnComplete { .. }, consumed)) => {
                buf.drain(..consumed);
                break;
            }
            Err(FrameError::IncompleteBuf) => read_more(&mut reader, &mut buf).await,
            Ok((other, consumed)) => {
                buf.drain(..consumed);
                panic!("unexpected: {other:?}");
            }
            Err(e) => panic!("decode: {e}"),
        }
    }
    assert!(!gate_gid.is_empty(), "must have gate_id");

    // Reject the gate.
    let resolve = FrontendMessage::ResolveGate {
        mission_id: mission_id.clone(),
        gate_id: gate_gid,
        approved: false,
    };
    writer
        .write_all(&encode_frame(&resolve).unwrap())
        .await
        .unwrap();

    loop {
        match decode_frame::<DaemonEnvelope>(&buf) {
            Ok((DaemonEnvelope::GateResolved { approved, .. }, consumed)) => {
                buf.drain(..consumed);
                assert!(!approved, "gate must be rejected");
                break;
            }
            Err(FrameError::IncompleteBuf) => read_more(&mut reader, &mut buf).await,
            Ok((other, consumed)) => {
                buf.drain(..consumed);
                panic!("unexpected during reject: {other:?}");
            }
            Err(e) => panic!("decode: {e}"),
        }
    }

    // Verify mission is Failed after rejection.
    let stored = mission::get_mission(&verify_handle, &mission_id)
        .await
        .expect("get mission")
        .expect("must exist");
    assert_eq!(
        stored.state,
        aivyx_channel::mission::MissionState::Failed,
        "mission must be Failed after gate rejection"
    );

    let disconnect = FrontendMessage::Disconnect;
    let _ = writer.write_all(&encode_frame(&disconnect).unwrap()).await;
    shutdown.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(5), daemon_handle).await;
}

// ---------------------------------------------------------------------------
// Protocol negotiation (Phase 41 Task 5)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn protocol_negotiation_accepted() {
    let scratch = ScratchDir::new();
    let socket_path = scratch.socket_path();

    let agent: Arc<dyn Agent> = Arc::new(FakeStreamingAgent {
        id: AgentId::new(),
        caps: CapabilitySet::empty(),
    });

    let channel = Arc::new(LocalChannel::new("daemon-test", Vec::<u8>::new()));
    let shutdown = CancellationToken::new();

    let daemon_socket = socket_path.clone();
    let daemon_agent = Arc::clone(&agent);
    let daemon_channel = Arc::clone(&channel);
    let daemon_shutdown = shutdown.clone();
    let daemon_handle = tokio::spawn(async move {
        run_daemon_compat(
            &daemon_socket,
            daemon_agent,
            daemon_channel,
            daemon_shutdown,
        )
        .await
        .expect("daemon must complete successfully");
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let stream = UnixStream::connect(&socket_path)
        .await
        .expect("connect to daemon");
    let (mut reader, mut writer) = stream.into_split();
    let mut buf = Vec::with_capacity(4096);

    // Read DaemonReady.
    read_more(&mut reader, &mut buf).await;
    let (envelope, consumed): (DaemonEnvelope, _) = decode_frame(&buf).expect("decode DaemonReady");
    buf.drain(..consumed);
    assert!(matches!(envelope, DaemonEnvelope::DaemonReady { .. }));

    // Send ProtocolNegotiation.
    let negotiate = FrontendMessage::ProtocolNegotiation {
        version: "0.1".into(),
    };
    let frame = encode_frame(&negotiate).unwrap();
    writer.write_all(&frame).await.unwrap();

    // Read ProtocolAccepted.
    loop {
        match decode_frame::<DaemonEnvelope>(&buf) {
            Ok((DaemonEnvelope::ProtocolAccepted { version }, consumed)) => {
                buf.drain(..consumed);
                assert_eq!(version, "0.1");
                break;
            }
            Err(FrameError::IncompleteBuf) => read_more(&mut reader, &mut buf).await,
            other => panic!("expected ProtocolAccepted, got {other:?}"),
        }
    }

    // After negotiation, normal session flow works.
    let frame = encode_frame(&FrontendMessage::StartSession {
        role: None,
        frontend_type: None,
    })
    .unwrap();
    writer.write_all(&frame).await.unwrap();

    loop {
        match decode_frame::<DaemonEnvelope>(&buf) {
            Ok((DaemonEnvelope::SessionStarted { .. }, consumed)) => {
                buf.drain(..consumed);
                break;
            }
            Err(FrameError::IncompleteBuf) => read_more(&mut reader, &mut buf).await,
            other => panic!("expected SessionStarted, got {other:?}"),
        }
    }

    let disconnect = FrontendMessage::Disconnect;
    let _ = writer.write_all(&encode_frame(&disconnect).unwrap()).await;
    shutdown.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(5), daemon_handle).await;
}

// ---------------------------------------------------------------------------
// Phase 47 Task 2 — Query/QueryResponse round trip (ListSessions)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn list_sessions_query_round_trips_over_ipc() {
    let scratch = ScratchDir::new();
    let socket_path = scratch.socket_path();

    let agent: Arc<dyn Agent> = Arc::new(FakeStreamingAgent {
        id: AgentId::new(),
        caps: CapabilitySet::empty(),
    });
    let channel = Arc::new(LocalChannel::new("daemon-test", Vec::<u8>::new()));
    let shutdown = CancellationToken::new();

    let daemon_socket = socket_path.clone();
    let daemon_agent = Arc::clone(&agent);
    let daemon_channel = Arc::clone(&channel);
    let daemon_shutdown = shutdown.clone();
    let daemon_handle = tokio::spawn(async move {
        run_daemon_compat(
            &daemon_socket,
            daemon_agent,
            daemon_channel,
            daemon_shutdown,
        )
        .await
        .expect("daemon must complete successfully");
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Open a connection and start a session so the daemon has at least
    // one entry in DaemonState.sessions.
    let stream = UnixStream::connect(&socket_path).await.expect("connect");
    let (mut reader, mut writer) = stream.into_split();
    let mut buf: Vec<u8> = Vec::new();

    // Consume DaemonReady.
    loop {
        match decode_frame::<DaemonEnvelope>(&buf) {
            Ok((DaemonEnvelope::DaemonReady { .. }, consumed)) => {
                buf.drain(..consumed);
                break;
            }
            Err(FrameError::IncompleteBuf) => read_more(&mut reader, &mut buf).await,
            other => panic!("expected DaemonReady, got {other:?}"),
        }
    }

    // Start a session so DaemonState.sessions has one entry.
    let frame = encode_frame(&FrontendMessage::StartSession {
        role: None,
        frontend_type: None,
    })
    .unwrap();
    writer.write_all(&frame).await.unwrap();
    let started_session_id = loop {
        match decode_frame::<DaemonEnvelope>(&buf) {
            Ok((DaemonEnvelope::SessionStarted { session_id }, consumed)) => {
                buf.drain(..consumed);
                break session_id;
            }
            Err(FrameError::IncompleteBuf) => read_more(&mut reader, &mut buf).await,
            other => panic!("expected SessionStarted, got {other:?}"),
        }
    };

    // Send a Query{ListSessions}.
    let query = FrontendMessage::Query {
        id: "q-test-001".into(),
        payload: QueryPayload::ListSessions,
    };
    writer
        .write_all(&encode_frame(&query).unwrap())
        .await
        .unwrap();

    // Expect QueryResponse with matching id and the started session listed.
    let (rid, sessions) = loop {
        match decode_frame::<DaemonEnvelope>(&buf) {
            Ok((DaemonEnvelope::QueryResponse { id, payload }, consumed)) => {
                buf.drain(..consumed);
                match payload {
                    QueryResponsePayload::ListSessions { sessions } => break (id, sessions),
                    QueryResponsePayload::QueryError { code, message } => {
                        panic!("unexpected QueryError ({code}): {message}");
                    }
                    other => panic!("expected ListSessions, got {other:?}"),
                }
            }
            Err(FrameError::IncompleteBuf) => read_more(&mut reader, &mut buf).await,
            other => panic!("expected QueryResponse, got {other:?}"),
        }
    };

    assert_eq!(rid, "q-test-001", "correlation id must echo");
    assert_eq!(
        sessions.len(),
        1,
        "expected the started session to be listed, got {sessions:?}"
    );
    assert_eq!(
        sessions[0].session_id, started_session_id,
        "session_id in response must match the started session"
    );
    // `/classic` retirement final-review fix — StartSession must really
    // capture channel/trust_tier/timestamps, not just an id. This is a
    // `LocalChannel`, so the daemon's real channel/trust_tier lookup
    // must resolve to `Local`/`Trusted`; a `created_at_ms` of 0 would
    // mean the timestamp capture regressed to a default.
    assert_eq!(
        sessions[0].channel,
        aivyx_channel::daemon_ipc::WireChannelPlatform::Local,
        "StartSession must capture the real channel platform"
    );
    assert_eq!(
        sessions[0].trust_tier,
        aivyx_capability::TrustTier::Trusted,
        "StartSession must capture the real trust tier"
    );
    assert!(
        sessions[0].created_at_ms > 0,
        "StartSession must capture a real created_at_ms timestamp"
    );
    let created_at_ms = sessions[0].created_at_ms;
    let first_last_active_ms = sessions[0].last_active_at_ms;
    assert!(
        first_last_active_ms > 0,
        "StartSession must capture a real last_active_at_ms timestamp"
    );

    // `/classic` retirement final-review fix — drive a real SubmitInput
    // through the daemon and confirm last_active_at_ms actually bumps
    // (created_at_ms must not move), rather than only asserting the
    // struct has the field.
    tokio::time::sleep(Duration::from_millis(20)).await;
    let submit = FrontendMessage::SubmitInput {
        session_id: started_session_id.clone(),
        text: "bump last_active".into(),
        mission_id: None,
        attachments: vec![],
        headless: false,
    };
    writer
        .write_all(&encode_frame(&submit).unwrap())
        .await
        .unwrap();
    let (_events, _outcome) = collect_turn_events(&mut reader, &mut buf).await;

    let query2 = FrontendMessage::Query {
        id: "q-test-002".into(),
        payload: QueryPayload::ListSessions,
    };
    writer
        .write_all(&encode_frame(&query2).unwrap())
        .await
        .unwrap();

    let (rid2, sessions2) = loop {
        match decode_frame::<DaemonEnvelope>(&buf) {
            Ok((DaemonEnvelope::QueryResponse { id, payload }, consumed)) => {
                buf.drain(..consumed);
                match payload {
                    QueryResponsePayload::ListSessions { sessions } => break (id, sessions),
                    QueryResponsePayload::QueryError { code, message } => {
                        panic!("unexpected QueryError ({code}): {message}");
                    }
                    other => panic!("expected ListSessions, got {other:?}"),
                }
            }
            Err(FrameError::IncompleteBuf) => read_more(&mut reader, &mut buf).await,
            other => panic!("expected QueryResponse, got {other:?}"),
        }
    };

    assert_eq!(rid2, "q-test-002", "correlation id must echo");
    assert_eq!(sessions2.len(), 1);
    assert_eq!(sessions2[0].created_at_ms, created_at_ms, "created_at_ms must not move on SubmitInput");
    assert!(
        sessions2[0].last_active_at_ms > first_last_active_ms,
        "SubmitInput must bump last_active_at_ms (before: {first_last_active_ms}, after: {})",
        sessions2[0].last_active_at_ms
    );

    let _ = writer
        .write_all(&encode_frame(&FrontendMessage::Disconnect).unwrap())
        .await;
    shutdown.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(5), daemon_handle).await;
}

// ---------------------------------------------------------------------------
// Phase 47 Task 3 — Mission queries (ListMissions, GetMission)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn mission_queries_round_trip_over_ipc() {
    use aivyx_channel::daemon_server::{ChannelFactory, DaemonConfig, run_daemon};
    use aivyx_channel::mission::{self, MissionRecord};
    use aivyx_crypto::MasterKey;
    use aivyx_storage::{KeyDomain, RedbStorage, Storage, StorageConfig};

    let scratch = ScratchDir::new();
    let socket_path = scratch.socket_path();
    let daemon_socket = socket_path.clone();

    // Seed a mission in a real RedbStorage.
    let store_path = scratch.path.join("missions.redb");
    let storage: Arc<dyn Storage> = RedbStorage::open(
        StorageConfig::new(store_path),
        MasterKey::from_raw([9u8; 32]),
    )
    .await
    .expect("storage must open");
    let mission_handle = storage.domain(KeyDomain::Missions);

    let mission_id = format!("m-{}", uuid::Uuid::new_v4());
    let record = MissionRecord::new(
        mission_id.clone(),
        "default".into(),
        "phase-47 query test".into(),
    );
    mission::create_mission(&mission_handle, &record)
        .await
        .expect("create mission");

    // Spin up the daemon with the seeded mission store.
    let agent: Arc<dyn Agent> = Arc::new(FakeStreamingAgent {
        id: AgentId::new(),
        caps: CapabilitySet::empty(),
    });
    let daemon_agent = Arc::clone(&agent);
    let shutdown = CancellationToken::new();
    let daemon_shutdown = shutdown.clone();

    let factory: ChannelFactory = Arc::new(move |_| {
        let ch: Arc<dyn ChannelContext + Send + Sync> =
            Arc::new(LocalChannel::new("mq-test", Vec::<u8>::new()));
        ch
    });

    let daemon_handle = tokio::spawn(async move {
        run_daemon(DaemonConfig {
            socket_path: daemon_socket,
            agent: daemon_agent,
            channel_factory: factory,
            shutdown: daemon_shutdown,
            notify_dispatcher: None,
            default_notify_target: None,
            notify_targets: Vec::new(),
            mission_store: Some(mission_handle),
            schedule_store: None,
            webhook_store: None,
            file_watch_store: None,
            webhook_port: None,
            web_ui_port: None,
            web_ui_host: None,
            web_ui_allowed_origins: Vec::new(),
            web_ui_auth_token: None,
            comfyui_base_url: None,
            memory: None,
            memory_ttl_secs: None,
            audit_log: None,
            profile: Arc::new(Profile::default()),
            persona_log: None,
            shared_persona: aivyx_channel::persona::shared_effective_persona(
                aivyx_channel::persona::EffectivePersona::default(),
            ),
            web_ui_broadcaster: None,
            persona_proposal_log: None,
            reflection_schedules: Vec::new(),
            target_policies: std::collections::HashMap::new(),
            embedding_provider: None,
            recall_log: None,
            helpfulness_ledger: None,
            cooccurrence_ledger: None,
            correction_ledger: None,
            persona_selection_stat: None,
            recall_cluster_stat: None,
            proactive_config: None,
            proactive_log: None,
            proactive_stat: None,
            persona_lifecycle_config: None,
            persona_lifecycle_stat: None,
            memory_retention: Vec::new(),
            conversation_windows: None,
            persona_consolidation_config: None,
            persona_consolidation_stat: None,
            persona_consolidation_phraser: None,
            correction_consolidation_config: None,
            correction_consolidation_stat: None,
            correction_consolidation_phraser: None,
            loop_backlog: None,
            loop_state: None,
            loop_config: None,
            team_missions: None,
            gate_policy: aivyx_core::GatePolicy::default(),
            workspace_journaling_interval: None,
            pricing: Default::default(),
            config_toml_path: None,
            role_override: None,
            team_config_write_path: None,
            seed_draft_llm: None,
            document_roots: Default::default(),
            reminder_store: None,
            wiki_sweep: None,
            wiki_store: None,
            graph_sweep: None,
            graph_store: None,
            conflict_dismissals: None,
            recall_judgment_config: None,
            recall_judgment_stat: None,
            recall_judge: None,
            correction_judgment_config: None,
            correction_judge: None,
            correction_judgment_stat: None,
            correction_signal_config: None,
            recall_feedback_config: None,
            tool_descriptors: Vec::new(),
            skill_auto_proposer: None,
            tool_relevance_ledger: None,
            skill_effectiveness_ledger: None,
            skill_refinement_config: None,
            skill_refinement_drafter: None,
            skill_authoring_config: None,
            skill_authoring_drafter: None,
        })
        .await
        .expect("daemon must complete successfully");
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let stream = UnixStream::connect(&socket_path).await.expect("connect");

    // Task 7 final review — lock in the precondition the whole
    // stage-then-rename socket-bind fix depends on: `run_daemon` really
    // does call `create_dir_all_0700` on the socket's parent before
    // binding, not just in a unit test of that helper in isolation.
    // `ScratchDir::new()` creates this directory itself at the ambient
    // umask (see its own doc/history — that's the exact directory whose
    // pre-fix 0755 mode this crate's own concurrent test suite once
    // corrupted via a racy process-global umask bracket), so seeing
    // `0700` here after a real daemon has started proves the daemon's
    // own startup path tightens it, not merely that the helper can.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&scratch.path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o700,
            "daemon startup must tighten its socket's parent dir to 0700"
        );
    }

    let (mut reader, mut writer) = stream.into_split();
    let mut buf: Vec<u8> = Vec::new();

    // Consume DaemonReady.
    loop {
        match decode_frame::<DaemonEnvelope>(&buf) {
            Ok((DaemonEnvelope::DaemonReady { .. }, consumed)) => {
                buf.drain(..consumed);
                break;
            }
            Err(FrameError::IncompleteBuf) => read_more(&mut reader, &mut buf).await,
            other => panic!("expected DaemonReady, got {other:?}"),
        }
    }

    // --- ListMissions ---
    let q = FrontendMessage::Query {
        id: "list-1".into(),
        payload: QueryPayload::ListMissions,
    };
    writer.write_all(&encode_frame(&q).unwrap()).await.unwrap();

    let (rid, missions) = loop {
        match decode_frame::<DaemonEnvelope>(&buf) {
            Ok((DaemonEnvelope::QueryResponse { id, payload }, consumed)) => {
                buf.drain(..consumed);
                match payload {
                    QueryResponsePayload::ListMissions { missions } => break (id, missions),
                    QueryResponsePayload::QueryError { code, message } => {
                        panic!("unexpected QueryError ({code}): {message}");
                    }
                    other => panic!("expected ListMissions, got {other:?}"),
                }
            }
            Err(FrameError::IncompleteBuf) => read_more(&mut reader, &mut buf).await,
            other => panic!("expected QueryResponse, got {other:?}"),
        }
    };

    assert_eq!(rid, "list-1");
    assert_eq!(missions.len(), 1, "expected exactly one mission");
    let summary = &missions[0];
    assert_eq!(summary.mission_id, mission_id);
    assert_eq!(summary.role_name, "default");
    assert_eq!(summary.description, "phase-47 query test");
    assert_eq!(summary.state, "Created");
    assert!(!summary.has_pending_gate);

    // --- GetMission (existing) ---
    let q = FrontendMessage::Query {
        id: "get-1".into(),
        payload: QueryPayload::GetMission {
            mission_id: mission_id.clone(),
        },
    };
    writer.write_all(&encode_frame(&q).unwrap()).await.unwrap();

    let (rid, detail): (String, Option<MissionDetail>) = loop {
        match decode_frame::<DaemonEnvelope>(&buf) {
            Ok((DaemonEnvelope::QueryResponse { id, payload }, consumed)) => {
                buf.drain(..consumed);
                match payload {
                    QueryResponsePayload::GetMission { mission } => break (id, mission),
                    QueryResponsePayload::QueryError { code, message } => {
                        panic!("unexpected QueryError ({code}): {message}");
                    }
                    other => panic!("expected GetMission, got {other:?}"),
                }
            }
            Err(FrameError::IncompleteBuf) => read_more(&mut reader, &mut buf).await,
            other => panic!("expected QueryResponse, got {other:?}"),
        }
    };

    assert_eq!(rid, "get-1");
    let detail = detail.expect("mission must be Some(_)");
    assert_eq!(detail.mission_id, mission_id);
    assert_eq!(detail.state, "Created");
    assert!(detail.gates.is_empty());

    // --- GetMission (missing) ---
    let q = FrontendMessage::Query {
        id: "get-2".into(),
        payload: QueryPayload::GetMission {
            mission_id: "does-not-exist".into(),
        },
    };
    writer.write_all(&encode_frame(&q).unwrap()).await.unwrap();

    let (rid, detail): (String, Option<MissionDetail>) = loop {
        match decode_frame::<DaemonEnvelope>(&buf) {
            Ok((DaemonEnvelope::QueryResponse { id, payload }, consumed)) => {
                buf.drain(..consumed);
                match payload {
                    QueryResponsePayload::GetMission { mission } => break (id, mission),
                    other => panic!("expected GetMission, got {other:?}"),
                }
            }
            Err(FrameError::IncompleteBuf) => read_more(&mut reader, &mut buf).await,
            other => panic!("expected QueryResponse, got {other:?}"),
        }
    };
    assert_eq!(rid, "get-2");
    assert!(detail.is_none(), "missing mission must be None, not error");

    let _ = writer
        .write_all(&encode_frame(&FrontendMessage::Disconnect).unwrap())
        .await;
    shutdown.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(5), daemon_handle).await;
}

#[tokio::test]
async fn mission_queries_without_store_return_query_error() {
    // No mission_store configured — daemon must respond with QueryError,
    // not crash, not hang.
    let scratch = ScratchDir::new();
    let socket_path = scratch.socket_path();

    let agent: Arc<dyn Agent> = Arc::new(FakeStreamingAgent {
        id: AgentId::new(),
        caps: CapabilitySet::empty(),
    });
    let channel = Arc::new(LocalChannel::new("mq-test", Vec::<u8>::new()));
    let shutdown = CancellationToken::new();

    let daemon_socket = socket_path.clone();
    let daemon_agent = Arc::clone(&agent);
    let daemon_channel = Arc::clone(&channel);
    let daemon_shutdown = shutdown.clone();
    let daemon_handle = tokio::spawn(async move {
        run_daemon_compat(
            &daemon_socket,
            daemon_agent,
            daemon_channel,
            daemon_shutdown,
        )
        .await
        .expect("daemon must complete successfully");
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let stream = UnixStream::connect(&socket_path).await.expect("connect");
    let (mut reader, mut writer) = stream.into_split();
    let mut buf: Vec<u8> = Vec::new();

    loop {
        match decode_frame::<DaemonEnvelope>(&buf) {
            Ok((DaemonEnvelope::DaemonReady { .. }, consumed)) => {
                buf.drain(..consumed);
                break;
            }
            Err(FrameError::IncompleteBuf) => read_more(&mut reader, &mut buf).await,
            other => panic!("expected DaemonReady, got {other:?}"),
        }
    }

    let q = FrontendMessage::Query {
        id: "no-store".into(),
        payload: QueryPayload::ListMissions,
    };
    writer.write_all(&encode_frame(&q).unwrap()).await.unwrap();

    let code = loop {
        match decode_frame::<DaemonEnvelope>(&buf) {
            Ok((DaemonEnvelope::QueryResponse { payload, .. }, consumed)) => {
                buf.drain(..consumed);
                match payload {
                    QueryResponsePayload::QueryError { code, .. } => break code,
                    other => panic!("expected QueryError, got {other:?}"),
                }
            }
            Err(FrameError::IncompleteBuf) => read_more(&mut reader, &mut buf).await,
            other => panic!("expected QueryResponse, got {other:?}"),
        }
    };
    assert_eq!(code, "no_mission_store");

    let _ = writer
        .write_all(&encode_frame(&FrontendMessage::Disconnect).unwrap())
        .await;
    shutdown.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(5), daemon_handle).await;
}

/// Chapter L (L.5) — a daemon with no team-mission service answers the team
/// queries with `QueryError { code: "no_team_missions" }` over the wire (the
/// new IPC variants encode/dispatch; no crash, no hang).
#[tokio::test]
async fn team_queries_without_service_return_query_error() {
    let scratch = ScratchDir::new();
    let socket_path = scratch.socket_path();

    let agent: Arc<dyn Agent> = Arc::new(FakeStreamingAgent {
        id: AgentId::new(),
        caps: CapabilitySet::empty(),
    });
    let channel = Arc::new(LocalChannel::new("tm-test", Vec::<u8>::new()));
    let shutdown = CancellationToken::new();

    let daemon_socket = socket_path.clone();
    let daemon_agent = Arc::clone(&agent);
    let daemon_channel = Arc::clone(&channel);
    let daemon_shutdown = shutdown.clone();
    let daemon_handle = tokio::spawn(async move {
        run_daemon_compat(
            &daemon_socket,
            daemon_agent,
            daemon_channel,
            daemon_shutdown,
        )
        .await
        .expect("daemon must complete successfully");
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let stream = UnixStream::connect(&socket_path).await.expect("connect");
    let (mut reader, mut writer) = stream.into_split();
    let mut buf: Vec<u8> = Vec::new();

    loop {
        match decode_frame::<DaemonEnvelope>(&buf) {
            Ok((DaemonEnvelope::DaemonReady { .. }, consumed)) => {
                buf.drain(..consumed);
                break;
            }
            Err(FrameError::IncompleteBuf) => read_more(&mut reader, &mut buf).await,
            other => panic!("expected DaemonReady, got {other:?}"),
        }
    }

    // Each of the four team queries must come back as the same QueryError.
    for (id, payload) in [
        ("tm-list", QueryPayload::TeamMissionList),
        (
            "tm-goal",
            QueryPayload::TeamRunGoal {
                goal: "close the kitchen".into(),
                config: None,
            },
        ),
        (
            "tm-status",
            QueryPayload::TeamMissionStatus {
                mission_id: "x".into(),
            },
        ),
        (
            "tm-resolve",
            QueryPayload::ResolveTeamGate {
                mission_id: "x".into(),
                step: "g".into(),
                approve: true,
            },
        ),
    ] {
        let q = FrontendMessage::Query {
            id: id.into(),
            payload,
        };
        writer.write_all(&encode_frame(&q).unwrap()).await.unwrap();

        let code = loop {
            match decode_frame::<DaemonEnvelope>(&buf) {
                Ok((DaemonEnvelope::QueryResponse { payload, .. }, consumed)) => {
                    buf.drain(..consumed);
                    match payload {
                        QueryResponsePayload::QueryError { code, .. } => break code,
                        other => panic!("expected QueryError, got {other:?}"),
                    }
                }
                Err(FrameError::IncompleteBuf) => read_more(&mut reader, &mut buf).await,
                other => panic!("expected QueryResponse, got {other:?}"),
            }
        };
        assert_eq!(code, "no_team_missions", "query {id}");
    }

    let _ = writer
        .write_all(&encode_frame(&FrontendMessage::Disconnect).unwrap())
        .await;
    shutdown.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(5), daemon_handle).await;
}

// ---------------------------------------------------------------------------
// Phase 47 Task 4 — Audit queries (ListAuditEntries, VerifyAuditChain)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn audit_queries_round_trip_over_ipc() {
    use aivyx_audit::{AuditEvent, AuditWriter, PersistentAuditLog, TrustTierSummary};
    use aivyx_capability::TrustTier;
    use aivyx_channel::daemon_server::{ChannelFactory, DaemonConfig, run_daemon};
    use aivyx_core::{ChannelPlatform, SessionId, TurnId};
    use aivyx_crypto::MasterKey;
    use aivyx_storage::{RedbStorage, Storage, StorageConfig};

    let scratch = ScratchDir::new();
    let socket_path = scratch.socket_path();
    let daemon_socket = socket_path.clone();

    // Open storage + persistent audit log; seed two events.
    let store_path = scratch.path.join("audit.redb");
    let storage: Arc<dyn Storage> = RedbStorage::open(
        StorageConfig::new(store_path),
        MasterKey::from_raw([5u8; 32]),
    )
    .await
    .expect("storage open");
    let audit_key: [u8; 32] = [42u8; 32];
    let audit_log = PersistentAuditLog::open(Arc::clone(&storage), audit_key)
        .await
        .expect("audit log open");
    let audit_log = Arc::new(audit_log);

    audit_log
        .append(AuditEvent::TurnStarted {
            turn_id: TurnId::new(),
            session_id: SessionId::new(),
            channel: ChannelPlatform::Local,
            trust_tier: TrustTierSummary::from(TrustTier::Trusted),
            effective_capabilities: CapabilitySet::empty(),
        })
        .expect("append TurnStarted");
    audit_log
        .append(AuditEvent::TurnEnded {
            turn_id: TurnId::new(),
            outcome: aivyx_core::TurnOutcomeSummary::Completed,
            tool_calls_made: 0,
            duration: Duration::from_millis(10),
            usage: aivyx_core::TokenUsage::default(),
        })
        .expect("append TurnEnded");

    // Spin up the daemon with audit_log threaded in.
    let agent: Arc<dyn Agent> = Arc::new(FakeStreamingAgent {
        id: AgentId::new(),
        caps: CapabilitySet::empty(),
    });
    let daemon_agent = Arc::clone(&agent);
    let shutdown = CancellationToken::new();
    let daemon_shutdown = shutdown.clone();

    let factory: ChannelFactory = Arc::new(move |_| {
        let ch: Arc<dyn ChannelContext + Send + Sync> =
            Arc::new(LocalChannel::new("audit-test", Vec::<u8>::new()));
        ch
    });

    let daemon_audit = Arc::clone(&audit_log);
    let daemon_handle = tokio::spawn(async move {
        run_daemon(DaemonConfig {
            socket_path: daemon_socket,
            agent: daemon_agent,
            channel_factory: factory,
            shutdown: daemon_shutdown,
            mission_store: None,
            notify_dispatcher: None,
            default_notify_target: None,
            notify_targets: Vec::new(),
            schedule_store: None,
            webhook_store: None,
            file_watch_store: None,
            webhook_port: None,
            web_ui_port: None,
            web_ui_host: None,
            web_ui_allowed_origins: Vec::new(),
            web_ui_auth_token: None,
            comfyui_base_url: None,
            memory: None,
            memory_ttl_secs: None,
            audit_log: Some(daemon_audit),
            profile: Arc::new(Profile::default()),
            persona_log: None,
            shared_persona: aivyx_channel::persona::shared_effective_persona(
                aivyx_channel::persona::EffectivePersona::default(),
            ),
            web_ui_broadcaster: None,
            persona_proposal_log: None,
            reflection_schedules: Vec::new(),
            target_policies: std::collections::HashMap::new(),
            embedding_provider: None,
            recall_log: None,
            helpfulness_ledger: None,
            cooccurrence_ledger: None,
            correction_ledger: None,
            persona_selection_stat: None,
            recall_cluster_stat: None,
            proactive_config: None,
            proactive_log: None,
            proactive_stat: None,
            persona_lifecycle_config: None,
            persona_lifecycle_stat: None,
            memory_retention: Vec::new(),
            conversation_windows: None,
            persona_consolidation_config: None,
            persona_consolidation_stat: None,
            persona_consolidation_phraser: None,
            correction_consolidation_config: None,
            correction_consolidation_stat: None,
            correction_consolidation_phraser: None,
            loop_backlog: None,
            loop_state: None,
            loop_config: None,
            team_missions: None,
            gate_policy: aivyx_core::GatePolicy::default(),
            workspace_journaling_interval: None,
            pricing: Default::default(),
            config_toml_path: None,
            role_override: None,
            team_config_write_path: None,
            seed_draft_llm: None,
            document_roots: Default::default(),
            reminder_store: None,
            wiki_sweep: None,
            wiki_store: None,
            graph_sweep: None,
            graph_store: None,
            conflict_dismissals: None,
            recall_judgment_config: None,
            recall_judgment_stat: None,
            recall_judge: None,
            correction_judgment_config: None,
            correction_judge: None,
            correction_judgment_stat: None,
            correction_signal_config: None,
            recall_feedback_config: None,
            tool_descriptors: Vec::new(),
            skill_auto_proposer: None,
            tool_relevance_ledger: None,
            skill_effectiveness_ledger: None,
            skill_refinement_config: None,
            skill_refinement_drafter: None,
            skill_authoring_config: None,
            skill_authoring_drafter: None,
        })
        .await
        .expect("daemon must complete successfully");
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let stream = UnixStream::connect(&socket_path).await.expect("connect");
    let (mut reader, mut writer) = stream.into_split();
    let mut buf: Vec<u8> = Vec::new();

    // Consume DaemonReady.
    loop {
        match decode_frame::<DaemonEnvelope>(&buf) {
            Ok((DaemonEnvelope::DaemonReady { .. }, consumed)) => {
                buf.drain(..consumed);
                break;
            }
            Err(FrameError::IncompleteBuf) => read_more(&mut reader, &mut buf).await,
            other => panic!("expected DaemonReady, got {other:?}"),
        }
    }

    // --- ListAuditEntries (from_seq=0, limit=100) ---
    let q = FrontendMessage::Query {
        id: "audit-list".into(),
        payload: QueryPayload::ListAuditEntries {
            from_seq: 0,
            limit: 100,
        },
    };
    writer.write_all(&encode_frame(&q).unwrap()).await.unwrap();

    let (rid, entries, total_len) = loop {
        match decode_frame::<DaemonEnvelope>(&buf) {
            Ok((DaemonEnvelope::QueryResponse { id, payload }, consumed)) => {
                buf.drain(..consumed);
                match payload {
                    QueryResponsePayload::ListAuditEntries { entries, total_len } => {
                        break (id, entries, total_len);
                    }
                    QueryResponsePayload::QueryError { code, message } => {
                        panic!("unexpected QueryError ({code}): {message}");
                    }
                    other => panic!("expected ListAuditEntries, got {other:?}"),
                }
            }
            Err(FrameError::IncompleteBuf) => read_more(&mut reader, &mut buf).await,
            other => panic!("expected QueryResponse, got {other:?}"),
        }
    };
    assert_eq!(rid, "audit-list");
    assert_eq!(total_len, 2);
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].seq, 0);
    assert_eq!(entries[0].event_type, "TurnStarted");
    assert_eq!(entries[1].seq, 1);
    assert_eq!(entries[1].event_type, "TurnEnded");
    assert_eq!(entries[0].mac_hex.len(), 64, "mac must be 32 bytes hex");

    // --- ListAuditEntries (from_seq=1, limit=10) — short read ---
    let q = FrontendMessage::Query {
        id: "audit-page2".into(),
        payload: QueryPayload::ListAuditEntries {
            from_seq: 1,
            limit: 10,
        },
    };
    writer.write_all(&encode_frame(&q).unwrap()).await.unwrap();

    let entries = loop {
        match decode_frame::<DaemonEnvelope>(&buf) {
            Ok((DaemonEnvelope::QueryResponse { payload, .. }, consumed)) => {
                buf.drain(..consumed);
                match payload {
                    QueryResponsePayload::ListAuditEntries { entries, .. } => break entries,
                    other => panic!("expected ListAuditEntries, got {other:?}"),
                }
            }
            Err(FrameError::IncompleteBuf) => read_more(&mut reader, &mut buf).await,
            other => panic!("expected QueryResponse, got {other:?}"),
        }
    };
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].seq, 1);

    // --- VerifyAuditChain — must report ok=true, 2 entries ---
    let q = FrontendMessage::Query {
        id: "audit-verify".into(),
        payload: QueryPayload::VerifyAuditChain,
    };
    writer.write_all(&encode_frame(&q).unwrap()).await.unwrap();

    let (ok, entries_verified, error) = loop {
        match decode_frame::<DaemonEnvelope>(&buf) {
            Ok((DaemonEnvelope::QueryResponse { payload, .. }, consumed)) => {
                buf.drain(..consumed);
                match payload {
                    QueryResponsePayload::VerifyAuditChain {
                        ok,
                        entries_verified,
                        error,
                    } => {
                        break (ok, entries_verified, error);
                    }
                    other => panic!("expected VerifyAuditChain, got {other:?}"),
                }
            }
            Err(FrameError::IncompleteBuf) => read_more(&mut reader, &mut buf).await,
            other => panic!("expected QueryResponse, got {other:?}"),
        }
    };
    assert!(ok, "chain must verify");
    assert_eq!(entries_verified, 2);
    assert!(error.is_none());

    let _ = writer
        .write_all(&encode_frame(&FrontendMessage::Disconnect).unwrap())
        .await;
    shutdown.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(5), daemon_handle).await;
}

#[tokio::test]
async fn audit_queries_without_log_return_query_error() {
    let scratch = ScratchDir::new();
    let socket_path = scratch.socket_path();

    let agent: Arc<dyn Agent> = Arc::new(FakeStreamingAgent {
        id: AgentId::new(),
        caps: CapabilitySet::empty(),
    });
    let channel = Arc::new(LocalChannel::new("audit-test", Vec::<u8>::new()));
    let shutdown = CancellationToken::new();

    let daemon_socket = socket_path.clone();
    let daemon_agent = Arc::clone(&agent);
    let daemon_channel = Arc::clone(&channel);
    let daemon_shutdown = shutdown.clone();
    let daemon_handle = tokio::spawn(async move {
        run_daemon_compat(
            &daemon_socket,
            daemon_agent,
            daemon_channel,
            daemon_shutdown,
        )
        .await
        .expect("daemon must complete successfully");
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let stream = UnixStream::connect(&socket_path).await.expect("connect");
    let (mut reader, mut writer) = stream.into_split();
    let mut buf: Vec<u8> = Vec::new();

    loop {
        match decode_frame::<DaemonEnvelope>(&buf) {
            Ok((DaemonEnvelope::DaemonReady { .. }, consumed)) => {
                buf.drain(..consumed);
                break;
            }
            Err(FrameError::IncompleteBuf) => read_more(&mut reader, &mut buf).await,
            other => panic!("expected DaemonReady, got {other:?}"),
        }
    }

    let q = FrontendMessage::Query {
        id: "no-audit".into(),
        payload: QueryPayload::ListAuditEntries {
            from_seq: 0,
            limit: 50,
        },
    };
    writer.write_all(&encode_frame(&q).unwrap()).await.unwrap();

    let code = loop {
        match decode_frame::<DaemonEnvelope>(&buf) {
            Ok((DaemonEnvelope::QueryResponse { payload, .. }, consumed)) => {
                buf.drain(..consumed);
                match payload {
                    QueryResponsePayload::QueryError { code, .. } => break code,
                    other => panic!("expected QueryError, got {other:?}"),
                }
            }
            Err(FrameError::IncompleteBuf) => read_more(&mut reader, &mut buf).await,
            other => panic!("expected QueryResponse, got {other:?}"),
        }
    };
    assert_eq!(code, "no_audit_log");

    let _ = writer
        .write_all(&encode_frame(&FrontendMessage::Disconnect).unwrap())
        .await;
    shutdown.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(5), daemon_handle).await;
}
