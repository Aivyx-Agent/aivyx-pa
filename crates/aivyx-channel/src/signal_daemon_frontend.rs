//! Daemon-mode Signal frontend: this process talks to signal-cli and
//! applies `aivyx_signal::policy` (via `aivyx_signal::session::route`);
//! only approved messages are submitted to the daemon, over one IPC session
//! per sender. `/team …` and gate commands are gated as on Matrix.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use aivyx_core::{
    CancellationToken, ChannelContext, ChannelError, ChannelPlatform, SessionId, StreamEvent,
    TurnOutcome,
};
use aivyx_signal::policy::{OncePerSender, SignalPolicy};
use aivyx_signal::session::{route, send_reply, turn_text, Routed};
use aivyx_signal::transport::{SignalTransport, TransportError};

use crate::daemon_client::DaemonSession;
use crate::daemon_ipc::FrontendType;
use crate::daemon_server::DaemonError;
use crate::gate_command;
use crate::team_trigger_state::PendingTrigger;

/// The daemon-side channel for `FrontendType::Signal` connections.
pub struct SignalDaemonChannel {
    session: SessionId,
    token: Mutex<CancellationToken>,
}

impl SignalDaemonChannel {
    pub fn new() -> Self {
        SignalDaemonChannel {
            session: SessionId::new(),
            token: Mutex::new(CancellationToken::new()),
        }
    }
}

impl Default for SignalDaemonChannel {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl ChannelContext for SignalDaemonChannel {
    fn channel_name(&self) -> &str {
        "aivyx-signal-daemon"
    }
    fn platform(&self) -> ChannelPlatform {
        ChannelPlatform::Signal
    }
    fn trust_tier(&self) -> aivyx_capability::TrustTier {
        // The frontend submits only messages from an allowed sender whose
        // safety number the operator verified.
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

/// Daemon sessions kept open at once (one per recent sender).
const MAX_SENDER_SESSIONS: usize = 8;

struct Sessions {
    socket_path: PathBuf,
    role: Option<String>,
    open: VecDeque<(String, DaemonSession)>,
}

impl Sessions {
    async fn for_sender(&mut self, sender: &str) -> Result<&mut DaemonSession, DaemonError> {
        if let Some(i) = self.open.iter().position(|(s, _)| s == sender) {
            let entry = self.open.remove(i).expect("index from position");
            self.open.push_back(entry);
        } else {
            if self.open.len() >= MAX_SENDER_SESSIONS
                && let Some((_, old)) = self.open.pop_front()
            {
                let _ = old.disconnect().await;
            }
            let s = DaemonSession::connect(&self.socket_path, self.role.clone(), Some(FrontendType::Signal))
                .await?;
            self.open.push_back((sender.to_string(), s));
        }
        Ok(&mut self.open.back_mut().expect("just pushed").1)
    }

    async fn close_all(&mut self) {
        for (_, s) in self.open.drain(..) {
            let _ = s.disconnect().await;
        }
    }
}

/// A Signal call's error: fatal ends the session, anything else is logged.
fn settle(what: &str, r: Result<(), TransportError>) -> Result<(), DaemonError> {
    match r {
        Ok(()) => Ok(()),
        Err(e) if e.is_fatal() => Err(DaemonError::ChannelFatal(e.to_string())),
        Err(e) => {
            eprintln!("aivyx-signal(daemon): {what} failed: {e}");
            Ok(())
        }
    }
}

/// Runs the Signal channel against a running daemon. `state_dir` is the
/// instance's `signal_dir()` (it holds the lock). An unregistered account
/// or a second running channel is `DaemonError::ChannelFatal` (no
/// in-process fallback can help); a daemon failure is another error, so
/// the binary falls back.
#[allow(clippy::too_many_arguments)]
pub async fn run_signal_daemon_session<T: SignalTransport + ?Sized>(
    transport: &T,
    policy: &SignalPolicy,
    state_dir: &Path,
    socket_path: PathBuf,
    role: Option<String>,
    shutdown: CancellationToken,
    team_run_channel: bool,
    team_trigger_rate_limit: Option<u32>,
    team_command_allowed_senders: Vec<String>,
) -> Result<(), DaemonError> {
    let _lock = aivyx_signal::lock::acquire(state_dir).map_err(DaemonError::ChannelFatal)?;
    let mut sessions = Sessions {
        socket_path: socket_path.clone(),
        role,
        open: VecDeque::new(),
    };
    let result = pump(
        transport,
        policy,
        &socket_path,
        &mut sessions,
        shutdown,
        team_run_channel,
        team_trigger_rate_limit,
        &team_command_allowed_senders,
    )
    .await;
    sessions.close_all().await;
    result
}

#[allow(clippy::too_many_arguments)]
async fn pump<T: SignalTransport + ?Sized>(
    transport: &T,
    policy: &SignalPolicy,
    socket_path: &Path,
    sessions: &mut Sessions,
    shutdown: CancellationToken,
    team_run_channel: bool,
    team_trigger_rate_limit: Option<u32>,
    team_command_allowed_senders: &[String],
) -> Result<(), DaemonError> {
    let mut asked = OncePerSender::default();
    let mut pending: Option<PendingTrigger<String>> = None;
    let mut history: Vec<Instant> = Vec::new();
    let mut backoff = Duration::from_secs(1);
    loop {
        let event = tokio::select! {
            biased;
            _ = shutdown.cancelled() => return Ok(()),
            r = transport.next_event() => r,
        };
        let event = match event {
            Ok(e) => {
                backoff = Duration::from_secs(1);
                e
            }
            Err(e) if e.is_fatal() => return Err(DaemonError::ChannelFatal(e.to_string())),
            Err(e) => {
                eprintln!("aivyx-signal(daemon): {e}; retrying in {}s", backoff.as_secs());
                tokio::select! {
                    biased;
                    _ = shutdown.cancelled() => return Ok(()),
                    _ = tokio::time::sleep(backoff) => {}
                }
                backoff = (backoff * 2).min(Duration::from_secs(60));
                continue;
            }
        };
        let m = match route(transport, policy, &mut asked, event).await {
            Ok(Routed::Turn(m)) => m,
            Ok(Routed::Handled) => continue,
            Err(e) => {
                settle("route", Err(e))?;
                continue;
            }
        };
        let sender = m.sender().to_string();
        let first_line = m.text.trim();
        // Read between turns, so it can't stop one already running (as on
        // Matrix and Discord in daemon mode).
        if first_line == "/cancel" {
            continue;
        }
        if let Some(reply) = crate::channel_team_chain::handle_team_command(
            FrontendType::Signal,
            first_line,
            socket_path,
            &mut pending,
            &mut history,
            team_run_channel,
            team_trigger_rate_limit,
            sender.clone(),
            team_command_allowed_senders,
        )
        .await
        {
            settle("send", send_reply(transport, &sender, &reply, &[]).await)?;
            continue;
        }
        if let Some(gate) = gate_command::parse(first_line) {
            let session = sessions.for_sender(&sender).await?;
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
            settle("send", send_reply(transport, &sender, &reply, &[]).await)?;
            continue;
        }
        let (text, notes) = turn_text(&m);
        let session = sessions.for_sender(&sender).await?;
        settle("typing", transport.typing(&sender, false).await)?;
        let submitted = session.submit_input(text).await;
        settle("typing", transport.typing(&sender, true).await)?;
        let (events, outcome) = submitted?;
        let reply = crate::daemon_ipc::chat_reply(
            crate::discord_daemon_frontend::render_events_for_discord(&events),
            &events,
            &outcome,
        );
        settle("send", send_reply(transport, &sender, &reply, &notes).await)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_signal::envelope::{Inbound, TextMessage};
    use aivyx_signal::transport::{ScriptedTransport, TrustLevel};

    const ME: &str = "+15550001111";

    fn policy() -> SignalPolicy {
        SignalPolicy::new(&[ME.to_string()])
    }

    #[test]
    fn daemon_channel_is_signal_semitrusted() {
        let c = SignalDaemonChannel::new();
        assert_eq!(c.platform(), ChannelPlatform::Signal);
        assert_eq!(c.trust_tier(), aivyx_capability::TrustTier::SemiTrusted);
        assert_eq!(c.channel_name(), "aivyx-signal-daemon");
    }

    #[tokio::test]
    async fn an_unregistered_account_is_channel_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let t = ScriptedTransport::new(vec![])
            .then_fail(|| TransportError::Unregistered("Authorization failed".into()));
        let r = tokio::time::timeout(
            Duration::from_secs(5),
            run_signal_daemon_session(
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
        assert!(e.to_string().contains("connect signal"), "{e}");
    }

    #[tokio::test]
    async fn a_missing_daemon_is_not_channel_fatal_so_the_binary_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        let t = ScriptedTransport::new(vec![Inbound::Text(TextMessage {
            sender_number: Some(ME.into()),
            sender_uuid: None,
            timestamp: 1,
            text: "hello".into(),
            attachments: 0,
        })])
        .with_identity(ME, TrustLevel::TrustedVerified, "1");
        let r = run_signal_daemon_session(
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
        assert!(matches!(r, Err(ref e) if !matches!(e, DaemonError::ChannelFatal(_))), "{r:?}");
        // No typing indicator is left running.
        assert!(t.typing_events().await.last().is_none_or(|(_, stop)| *stop));
    }

    #[tokio::test]
    async fn an_unverified_sender_gets_the_verify_reply_without_a_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let t = ScriptedTransport::new(vec![Inbound::Text(TextMessage {
            sender_number: Some(ME.into()),
            sender_uuid: None,
            timestamp: 1,
            text: "hello".into(),
            attachments: 0,
        })])
        .with_identity(ME, TrustLevel::TrustedUnverified, "1");
        let shutdown = CancellationToken::new();
        let s2 = shutdown.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            s2.cancel();
        });
        run_signal_daemon_session(
            &t,
            &policy(),
            dir.path(),
            PathBuf::from("/nonexistent.sock"),
            None,
            shutdown,
            false,
            None,
            vec![],
        )
        .await
        .unwrap();
        assert_eq!(t.sent().await.len(), 1);
        assert_eq!(t.sent().await[0].1, aivyx_signal::policy::ASK_TO_VERIFY);
    }
}
