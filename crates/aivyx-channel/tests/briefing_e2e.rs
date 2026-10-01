//! The Command Center briefing over the real daemon IPC server.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use aivyx_capability::CapabilitySet;
use aivyx_channel::LocalChannel;
use aivyx_channel::daemon_ipc::{
    DaemonEnvelope, FrameError, FrontendMessage, QueryPayload, QueryResponsePayload, decode_frame, encode_frame,
};
use aivyx_channel::daemon_server::run_daemon_compat;
use aivyx_core::{Agent, AgentId, CancellationToken, ChannelContext, Message, TurnOutcome};

struct QuietAgent {
    id: AgentId,
    caps: CapabilitySet,
}

#[async_trait]
impl Agent for QuietAgent {
    fn id(&self) -> AgentId {
        self.id
    }
    fn capabilities(&self) -> &CapabilitySet {
        &self.caps
    }
    async fn turn(&self, _m: Message, _c: &dyn ChannelContext) -> TurnOutcome {
        TurnOutcome::Completed { final_message: "ok".into(), tool_calls_made: 0, duration: Duration::from_millis(1) }
    }
}

async fn query(stream: &mut UnixStream, buf: &mut Vec<u8>, id: &str, payload: QueryPayload) -> QueryResponsePayload {
    let msg = FrontendMessage::Query { id: id.into(), payload };
    stream.write_all(&encode_frame(&msg).unwrap()).await.unwrap();
    loop {
        match decode_frame::<DaemonEnvelope>(buf) {
            Ok((env, n)) => {
                buf.drain(..n);
                if let DaemonEnvelope::QueryResponse { id: got, payload } = env
                    && got == id
                {
                    return payload;
                }
            }
            Err(FrameError::IncompleteBuf) => {
                let mut tmp = [0u8; 4096];
                let n = stream.read(&mut tmp).await.unwrap();
                assert!(n > 0, "daemon closed the connection");
                buf.extend_from_slice(&tmp[..n]);
            }
            Err(e) => panic!("decode: {e}"),
        }
    }
}

#[tokio::test]
async fn get_briefing_answers_over_ipc() {
    let dir: PathBuf = std::env::temp_dir().join(format!("aivyx-briefing-e2e-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("daemon.sock");
    let shutdown = CancellationToken::new();
    let agent: Arc<dyn Agent> = Arc::new(QuietAgent { id: AgentId::new(), caps: CapabilitySet::empty() });
    let channel = Arc::new(LocalChannel::new("briefing-e2e", Vec::<u8>::new()));
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
    let mut stream = UnixStream::connect(&socket).await.unwrap();
    let mut buf = Vec::new();

    let QueryResponsePayload::Briefing { briefing } = query(&mut stream, &mut buf, "b1", QueryPayload::GetBriefing).await
    else {
        panic!("expected a Briefing");
    };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    assert_eq!(briefing.last_active_unix, None);
    assert!((briefing.window_start_unix - (now - 24 * 3600)).abs() < 60);
    assert!(!briefing.window_capped);

    // No reminder store in the compat daemon → a clean "not ok", not an error.
    let resp = query(&mut stream, &mut buf, "r1", QueryPayload::CompleteReminder { id: "x".into() }).await;
    assert_eq!(resp, QueryResponsePayload::ReminderUpdated { id: "x".into(), ok: false, due_unix: None });

    shutdown.cancel();
    let _ = std::fs::remove_dir_all(&dir);
}
