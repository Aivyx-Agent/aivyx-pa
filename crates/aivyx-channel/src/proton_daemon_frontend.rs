//! Daemon-mode Proton Mail frontend: this process holds the Bridge
//! connection and runs `aivyx_proton::run_mail_loop` (lock, cursor, trust
//! policy, reply, filing); only approved mail is submitted to the daemon,
//! over one IPC session per email thread so each thread keeps its own
//! conversation. `/team …` and gate commands are read from the email's
//! first line, gated exactly as on Matrix.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use aivyx_core::{
    CancellationToken, ChannelContext, ChannelError, ChannelPlatform, SessionId, StreamEvent,
    TurnOutcome,
};
use aivyx_proton::policy::ProtonPolicy;
use aivyx_proton::session::Turn;
use aivyx_proton::transport::MailTransport;
use aivyx_proton::{LoopError, TurnRunner};

use crate::daemon_client::DaemonSession;
use crate::daemon_ipc::FrontendType;
use crate::daemon_server::DaemonError;
use crate::gate_command;
use crate::team_trigger_state::PendingTrigger;

/// The daemon-side channel for `FrontendType::Proton` connections.
pub struct ProtonDaemonChannel {
    session: SessionId,
    token: Mutex<CancellationToken>,
}

impl ProtonDaemonChannel {
    pub fn new() -> Self {
        ProtonDaemonChannel {
            session: SessionId::new(),
            token: Mutex::new(CancellationToken::new()),
        }
    }
}

impl Default for ProtonDaemonChannel {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl ChannelContext for ProtonDaemonChannel {
    fn channel_name(&self) -> &str {
        "aivyx-proton-daemon"
    }
    fn platform(&self) -> ChannelPlatform {
        ChannelPlatform::Email
    }
    fn trust_tier(&self) -> aivyx_capability::TrustTier {
        // The frontend submits only mail `aivyx_proton::policy::decide`
        // approved: from you (in Sent) or allowlisted with a DMARC pass.
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

/// Daemon sessions kept open at once (one per recent thread); the oldest
/// is closed past this.
const MAX_THREAD_SESSIONS: usize = 8;

struct DaemonRunner {
    socket_path: PathBuf,
    role: Option<String>,
    threads: VecDeque<(String, DaemonSession)>,
    pending: Option<PendingTrigger<String>>,
    history: Vec<Instant>,
    team_run_channel: bool,
    team_trigger_rate_limit: Option<u32>,
    team_command_allowed_senders: Vec<String>,
}

impl DaemonRunner {
    /// The thread's daemon session, connecting (and evicting the oldest)
    /// when needed.
    async fn session_for(&mut self, root: &str) -> Result<&mut DaemonSession, DaemonError> {
        if let Some(i) = self.threads.iter().position(|(r, _)| r == root) {
            let entry = self.threads.remove(i).expect("index from position");
            self.threads.push_back(entry);
        } else {
            if self.threads.len() >= MAX_THREAD_SESSIONS
                && let Some((_, old)) = self.threads.pop_front()
            {
                let _ = old.disconnect().await;
            }
            let session = DaemonSession::connect(
                &self.socket_path,
                self.role.clone(),
                Some(FrontendType::Proton),
            )
            .await?;
            self.threads.push_back((root.to_string(), session));
        }
        Ok(&mut self.threads.back_mut().expect("just pushed").1)
    }

    async fn close_all(&mut self) {
        for (_, s) in self.threads.drain(..) {
            let _ = s.disconnect().await;
        }
    }
}

#[async_trait::async_trait]
impl TurnRunner for DaemonRunner {
    async fn run(&mut self, turn: &Turn) -> Result<Option<String>, String> {
        let sender = turn.message.from.clone().unwrap_or_default();
        if let Some(reply) = crate::channel_team_chain::handle_team_command(
            FrontendType::Proton,
            &turn.first_line,
            &self.socket_path,
            &mut self.pending,
            &mut self.history,
            self.team_run_channel,
            self.team_trigger_rate_limit,
            sender,
            &self.team_command_allowed_senders,
        )
        .await
        {
            return Ok(Some(reply));
        }
        let root = turn.thread();
        if let Some(gate) = gate_command::parse(&turn.first_line) {
            let session = self.session_for(&root).await.map_err(|e| e.to_string())?;
            let reply = match session
                .resolve_gate(gate.mission_id, gate.gate_id, gate.approved)
                .await
            {
                Ok(()) => format!(
                    "✓ Gate {}.",
                    if gate.approved { "approved" } else { "rejected" }
                ),
                Err(e) => format!("✗ Gate resolve failed: {e}"),
            };
            return Ok(Some(reply));
        }
        let session = self.session_for(&root).await.map_err(|e| e.to_string())?;
        let (events, outcome) = session
            .submit_input(turn.text.clone())
            .await
            .map_err(|e| e.to_string())?;
        Ok(Some(crate::daemon_ipc::chat_reply(
            crate::discord_daemon_frontend::render_events_for_discord(&events),
            &events,
            &outcome,
        )))
    }
}

/// Runs the Proton channel against a running daemon. `state_dir` is the
/// instance's `proton_dir()`. A rejected Bridge login, a changed
/// certificate or a second running channel is
/// `DaemonError::ChannelFatal` (no in-process fallback can help); a daemon
/// failure is `DaemonError::Internal`, so the binary falls back.
#[allow(clippy::too_many_arguments)]
pub async fn run_proton_daemon_session<T: MailTransport + ?Sized>(
    transport: &T,
    policy: &ProtonPolicy,
    state_dir: &Path,
    socket_path: PathBuf,
    role: Option<String>,
    shutdown: CancellationToken,
    team_run_channel: bool,
    team_trigger_rate_limit: Option<u32>,
    team_command_allowed_senders: Vec<String>,
) -> Result<(), DaemonError> {
    let mut runner = DaemonRunner {
        socket_path,
        role,
        threads: VecDeque::new(),
        pending: None,
        history: Vec::new(),
        team_run_channel,
        team_trigger_rate_limit,
        team_command_allowed_senders,
    };
    let result =
        aivyx_proton::run_mail_loop(transport, policy, state_dir, &mut runner, shutdown).await;
    runner.close_all().await;
    match result {
        Ok(_) => Ok(()),
        Err(LoopError::Fatal(e)) => Err(DaemonError::ChannelFatal(e)),
        Err(LoopError::Runner(e)) => Err(DaemonError::Internal(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_proton::transport::{InboxStatus, ScriptedTransport, TransportError};

    fn policy() -> ProtonPolicy {
        ProtonPolicy::new("bot@proton.me", &["me@proton.me".into(), "bot@proton.me".into()], &[])
    }

    #[test]
    fn daemon_channel_is_email_semitrusted() {
        let c = ProtonDaemonChannel::new();
        assert_eq!(c.platform(), ChannelPlatform::Email);
        assert_eq!(c.trust_tier(), aivyx_capability::TrustTier::SemiTrusted);
        assert_eq!(c.channel_name(), "aivyx-proton-daemon");
    }

    #[tokio::test]
    async fn a_rejected_bridge_login_is_channel_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let t = ScriptedTransport::new(InboxStatus { uid_validity: 1, uid_next: 1 }, vec![])
            .then_fail(|| TransportError::Unauthorized("NO".into()));
        let r = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            run_proton_daemon_session(
                &t,
                &policy(),
                dir.path(),
                PathBuf::from("/nonexistent.sock"),
                None,
                CancellationToken::new(),
                false,
                None,
                vec![],
            ),
        )
        .await
        .expect("returns");
        let e = r.expect_err("must be Err");
        assert!(matches!(e, DaemonError::ChannelFatal(_)), "{e:?}");
        assert!(e.to_string().contains("connect proton"), "{e}");
    }

    #[tokio::test]
    async fn a_missing_daemon_is_internal_so_the_binary_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        let mail = aivyx_proton::message::MailMessage {
            uid: 5,
            internal_id: Some("pm5".into()),
            from: Some("me@proton.me".into()),
            to: vec!["bot@proton.me".into()],
            text: "hello".into(),
            ..Default::default()
        };
        let t = ScriptedTransport::new(InboxStatus { uid_validity: 1, uid_next: 5 }, vec![vec![mail]])
            .in_sent_folder(vec!["pm5"]);
        let r = run_proton_daemon_session(
            &t,
            &policy(),
            dir.path(),
            PathBuf::from("/nonexistent.sock"),
            None,
            CancellationToken::new(),
            false,
            None,
            vec![],
        )
        .await;
        assert!(matches!(r, Err(DaemonError::Internal(_))), "{r:?}");
        assert!(t.sent().await.is_empty());
    }
}
