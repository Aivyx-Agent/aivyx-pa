//! Daemon-mode Matrix frontend: this process holds the Matrix client and
//! applies `aivyx_matrix::policy`; only policy-approved turns are
//! submitted to the daemon over IPC. Mirrors `discord_daemon_frontend`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use aivyx_core::{
    CancellationToken, ChannelContext, ChannelError, ChannelPlatform, SessionId, StreamEvent,
    TurnOutcome,
};
use aivyx_matrix::policy::{
    ASK_TO_VERIFY, Action, CANT_DECRYPT, MatrixPolicy, OncePerRoom, decide,
};
use aivyx_matrix::transport::{MatrixTransport, OutgoingMessage, TransportError};

use crate::daemon_client::{self, DaemonSession};
use crate::daemon_ipc::FrontendType;
use crate::daemon_server::DaemonError;
use crate::gate_command;
use crate::team_command::{self, TeamCommand, sender_allowed};
use crate::team_dispatch;
use crate::team_trigger_state::{
    ConfirmReply, PendingTrigger, check_and_record_trigger, parse_confirm_reply,
};

pub struct MatrixDaemonChannel {
    session: SessionId,
    token: Mutex<CancellationToken>,
}

impl MatrixDaemonChannel {
    pub fn new() -> Self {
        MatrixDaemonChannel {
            session: SessionId::new(),
            token: Mutex::new(CancellationToken::new()),
        }
    }
}

impl Default for MatrixDaemonChannel {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl ChannelContext for MatrixDaemonChannel {
    fn channel_name(&self) -> &str {
        "aivyx-matrix-daemon"
    }
    fn platform(&self) -> ChannelPlatform {
        ChannelPlatform::Matrix
    }
    fn trust_tier(&self) -> aivyx_capability::TrustTier {
        // The frontend submits only `Action::Turn` messages: the operator,
        // in the home room, from a verified device.
        aivyx_capability::TrustTier::SemiTrusted
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
        self.token.lock().expect("token mutex poisoned").clone()
    }
    fn reset_cancellation(&self) {
        *self.token.lock().expect("token mutex poisoned") = CancellationToken::new();
    }
    fn cancel_inflight(&self) {
        self.token.lock().expect("token mutex poisoned").cancel();
    }
}

/// The `/team` precedence chain for one message. `Some(reply)` when it
/// was a team command or a pending confirmation; `None` to carry on.
#[allow(clippy::too_many_arguments)]
async fn handle_matrix_team_command(
    text: &str,
    socket_path: &Path,
    pending: &mut Option<PendingTrigger<String>>,
    history: &mut Vec<Instant>,
    team_run_channel: bool,
    team_trigger_rate_limit: Option<u32>,
    sender: String,
    allowed_senders: &[String],
) -> Option<String> {
    if team_command::parse(text).is_some() && !sender_allowed(allowed_senders, &sender) {
        return Some("✗ you are not authorized to issue /team commands.".to_string());
    }
    if let Some(p) = pending.take() {
        let now = Instant::now();
        match parse_confirm_reply(text) {
            Some(ConfirmReply::Yes) | Some(ConfirmReply::No) if p.sender_id != sender => {
                if !p.is_expired(now) {
                    *pending = Some(p);
                }
            }
            Some(ConfirmReply::Yes) if p.is_expired(now) => {
                return Some("✗ that request expired, ask again.".to_string());
            }
            Some(ConfirmReply::Yes) => {
                return Some(
                    match daemon_client::run_team_mission_channel(
                        socket_path,
                        FrontendType::Matrix,
                        p.goal.clone(),
                    )
                    .await
                    {
                        Ok(id) => format!("✓ Started mission {id}."),
                        Err(e) => format!("✗ Could not start the mission: {e}"),
                    },
                );
            }
            Some(ConfirmReply::No) => return Some("Cancelled.".to_string()),
            None => {
                if !p.is_expired(now) {
                    *pending = Some(p);
                }
            }
        }
    }
    if let Some(TeamCommand::Run { goal }) = team_command::parse(text) {
        if !team_run_channel {
            return Some("✗ this channel is not authorized to start team missions.".to_string());
        }
        if let Some(limit) = team_trigger_rate_limit
            && !check_and_record_trigger(history, limit, Instant::now())
        {
            return Some(format!(
                "✗ too many mission-start requests (max {limit} per hour), try again later."
            ));
        }
        *pending = Some(PendingTrigger::new(goal.clone(), sender));
        return Some(format!("Start '{goal}' on the default team? Reply yes/no."));
    }
    if let Some(cmd) = team_command::parse(text) {
        return Some(team_dispatch::dispatch(socket_path, cmd).await);
    }
    None
}

/// Apply the failure rule to a Matrix call: Unauthorized ends the session;
/// anything else is logged and the turn's outcome stands.
async fn settle(
    session: DaemonSession,
    what: &str,
    result: Result<(), TransportError>,
) -> Result<DaemonSession, DaemonError> {
    match result {
        Ok(()) => Ok(session),
        Err(e @ TransportError::Unauthorized(_)) => {
            let _ = session.disconnect().await;
            Err(DaemonError::Internal(e.to_string()))
        }
        Err(e) => {
            eprintln!("aivyx-matrix(daemon): {what} failed: {e}");
            Ok(session)
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn run_matrix_daemon_session<T: MatrixTransport + 'static>(
    transport: Arc<T>,
    policy: MatrixPolicy,
    socket_path: PathBuf,
    role: Option<String>,
    shutdown: CancellationToken,
    team_run_channel: bool,
    team_trigger_rate_limit: Option<u32>,
    team_command_allowed_senders: Vec<String>,
) -> Result<(), DaemonError> {
    if !transport
        .room_is_encrypted(&policy.room_filter)
        .await
        .map_err(|e| DaemonError::Internal(e.to_string()))?
    {
        return Err(DaemonError::Internal(format!(
            "the Matrix room {} isn't encrypted — the assistant only talks in end-to-end-encrypted rooms",
            policy.room_filter
        )));
    }
    let mut session =
        DaemonSession::connect(&socket_path, role, Some(FrontendType::Matrix)).await?;
    let (mut asked, mut told) = (OncePerRoom::new(), OncePerRoom::new());
    let mut pending: Option<PendingTrigger<String>> = None;
    let mut history = Vec::new();
    let mut backoff = std::time::Duration::from_secs(1);
    let send = |room_id: String, text: String| {
        let t = Arc::clone(&transport);
        async move { t.send_message(OutgoingMessage { room_id, text }).await }
    };

    loop {
        let inbound = tokio::select! {
            biased;
            _ = shutdown.cancelled() => break,
            res = transport.next_inbound() => match res {
                Ok(i) => {
                    backoff = std::time::Duration::from_secs(1);
                    i
                }
                Err(TransportError::Unauthorized(e)) => {
                    let _ = session.disconnect().await;
                    return Err(DaemonError::Internal(TransportError::Unauthorized(e).to_string()));
                }
                Err(e) => {
                    eprintln!("aivyx-matrix(daemon): {e}; retrying in {}s", backoff.as_secs());
                    tokio::select! {
                        _ = shutdown.cancelled() => break,
                        _ = tokio::time::sleep(backoff) => {}
                    }
                    backoff = (backoff * 2).min(std::time::Duration::from_secs(60));
                    continue;
                }
            }
        };
        let m = match decide(&policy, inbound) {
            Action::Turn(m) => m,
            Action::AskToVerify { room_id } => {
                if asked.first(&room_id) {
                    let r = send(room_id, ASK_TO_VERIFY.into()).await;
                    session = settle(session, "send", r).await?;
                }
                continue;
            }
            Action::CantDecrypt { room_id } => {
                if told.first(&room_id) {
                    let r = send(room_id, CANT_DECRYPT.into()).await;
                    session = settle(session, "send", r).await?;
                }
                continue;
            }
            Action::Join { room_id } => {
                let r = transport.join_room(&room_id).await;
                session = settle(session, "join", r).await?;
                continue;
            }
            Action::Decline { room_id } => {
                let r = transport.decline_invite(&room_id).await;
                session = settle(session, "decline", r).await?;
                continue;
            }
            Action::Ignore => continue,
        };
        let text = m.text.trim().to_string();
        if text == "/cancel" {
            let _ = session.cancel_turn().await;
            continue;
        }
        if let Some(reply) = handle_matrix_team_command(
            &text,
            &socket_path,
            &mut pending,
            &mut history,
            team_run_channel,
            team_trigger_rate_limit,
            m.sender.clone(),
            &team_command_allowed_senders,
        )
        .await
        {
            let r = send(m.room_id.clone(), reply).await;
            session = settle(session, "send", r).await?;
            continue;
        }
        if let Some(gate) = gate_command::parse(&text) {
            let reply = match session
                .resolve_gate(gate.mission_id, gate.gate_id, gate.approved)
                .await
            {
                Ok(()) => format!(
                    "✓ Gate {}.",
                    if gate.approved {
                        "approved"
                    } else {
                        "rejected"
                    }
                ),
                Err(e) => format!("✗ Gate resolve failed: {e}"),
            };
            let r = send(m.room_id.clone(), reply).await;
            session = settle(session, "send", r).await?;
            continue;
        }
        let (events, outcome) = session.submit_input(m.text).await?;
        let reply = crate::daemon_ipc::chat_reply(
            crate::discord_daemon_frontend::render_events_for_discord(&events),
            &events,
            &outcome,
        );
        let r = send(m.room_id, reply).await;
        session = settle(session, "send", r).await?;
    }
    let _ = session.disconnect().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_channel_is_matrix_semitrusted() {
        let c = MatrixDaemonChannel::new();
        assert_eq!(c.platform(), ChannelPlatform::Matrix);
        assert_eq!(c.trust_tier(), aivyx_capability::TrustTier::SemiTrusted);
        assert_eq!(c.channel_name(), "aivyx-matrix-daemon");
    }

    #[tokio::test]
    async fn a_team_command_from_a_sender_not_allowed_is_refused() {
        let mut pending = None;
        let mut history = Vec::new();
        let out = handle_matrix_team_command(
            "/team status",
            std::path::Path::new("/nonexistent.sock"),
            &mut pending,
            &mut history,
            false,
            None,
            "@stranger:x".to_string(),
            &["@me:x".to_string()],
        )
        .await;
        assert_eq!(
            out,
            Some("✗ you are not authorized to issue /team commands.".to_string())
        );
    }

    #[tokio::test]
    async fn ordinary_text_is_not_a_team_command() {
        let mut pending = None;
        let mut history = Vec::new();
        let out = handle_matrix_team_command(
            "hello",
            std::path::Path::new("/nonexistent.sock"),
            &mut pending,
            &mut history,
            false,
            None,
            "@me:x".to_string(),
            &["@me:x".to_string()],
        )
        .await;
        assert_eq!(out, None);
    }

    use aivyx_matrix::transport::{Inbound, ScriptedTransport};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixListener;

    /// Fake daemon: does the handshake, then swallows whatever follows.
    async fn fake_daemon() -> (PathBuf, tokio::task::JoinHandle<()>) {
        use crate::daemon_ipc::{DaemonEnvelope, encode_frame};
        let sock =
            std::env::temp_dir().join(format!("aivyx-matrix-daemon-{}.sock", uuid::Uuid::new_v4()));
        let listener = UnixListener::bind(&sock).expect("bind");
        let h = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.expect("accept");
            let ready = encode_frame(&DaemonEnvelope::DaemonReady {
                version: "0.1".into(),
            })
            .unwrap();
            s.write_all(&ready).await.unwrap();
            let mut tmp = [0u8; 2048];
            let _ = s.read(&mut tmp).await;
            let started = encode_frame(&DaemonEnvelope::SessionStarted {
                session_id: "s".into(),
            })
            .unwrap();
            s.write_all(&started).await.unwrap();
            while matches!(s.read(&mut tmp).await, Ok(n) if n > 0) {}
        });
        (sock, h)
    }

    fn policy() -> MatrixPolicy {
        MatrixPolicy {
            room_filter: "!home:x".into(),
            allowed_senders: vec!["@me:x".into()],
        }
    }

    #[tokio::test]
    async fn platform_error_then_shutdown_returns_promptly() {
        let (sock, _srv) = fake_daemon().await;
        let t = Arc::new(
            ScriptedTransport::with_queue(vec![])
                .encrypted_rooms(vec!["!home:x"])
                .then_fail(|| TransportError::Platform("boom".into())),
        );
        let shutdown = CancellationToken::new();
        let s2 = shutdown.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            s2.cancel();
        });
        let r = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            run_matrix_daemon_session(t, policy(), sock, None, shutdown, false, None, vec![]),
        )
        .await
        .expect("must not sleep out the backoff");
        assert!(r.is_ok());
    }

    #[tokio::test]
    async fn unauthorized_from_next_inbound_ends_the_session() {
        let (sock, _srv) = fake_daemon().await;
        let t = Arc::new(
            ScriptedTransport::with_queue(vec![])
                .encrypted_rooms(vec!["!home:x"])
                .then_fail(|| TransportError::Unauthorized("M_UNKNOWN_TOKEN".into())),
        );
        let r = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            run_matrix_daemon_session(
                t,
                policy(),
                sock,
                None,
                CancellationToken::new(),
                false,
                None,
                vec![],
            ),
        )
        .await
        .expect("returns");
        let e = r.expect_err("must be Err").to_string();
        assert!(e.contains("connect matrix"), "{e}");
    }

    #[tokio::test]
    async fn an_invite_is_joined_through_the_session() {
        let (sock, _srv) = fake_daemon().await;
        let t = Arc::new(
            ScriptedTransport::with_queue(vec![Inbound::Invite {
                room_id: "!home:x".into(),
                inviter: "@me:x".into(),
            }])
            .encrypted_rooms(vec!["!home:x"]),
        );
        let shutdown = CancellationToken::new();
        let (t2, s2) = (Arc::clone(&t), shutdown.clone());
        let run = tokio::spawn(run_matrix_daemon_session(
            t2,
            policy(),
            sock,
            None,
            s2,
            false,
            None,
            vec![],
        ));
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        shutdown.cancel();
        run.await.unwrap().unwrap();
        assert_eq!(t.joined().await, vec!["!home:x".to_string()]);
    }
}
