//! Phase 111 Task 5 — Daemon-mode Slack multi-channel pump.
//!
//! Mirrors the Phase 19 `telegram_daemon_frontend.rs` and the
//! Phase 111 Task 3 `discord_daemon_frontend.rs` patterns.
//! Drives a multi-channel Slack frontend over the daemon IPC
//! channel: one outer `next_message` loop pumps the Socket Mode
//! WebSocket (via the now-live `SlackMorphismTransport` from
//! Task 4), per-partition routing fans out to inner mailbox
//! tasks, each inner task submits turns through a
//! `DaemonSession` instead of constructing an agent.
//!
//! ## Three data points for the daemon-frontend shape
//!
//! Telegram (Phase 19), Discord (Phase 111 Task 3), and Slack
//! (Phase 111 Task 5) now all run the same daemon-frontend
//! shape. The Q-block at Phase 111 Task 2 sign-off (Q2a —
//! mirror Phase 19 exactly) anticipated this: if Slack
//! converges on the same shape, the **shared substrate
//! question** raised in the open doc gets answered
//! affirmatively at three data points. A future small
//! refactor could lift the per-route routing logic into a
//! shared helper; Phase 111 ships three siblings per the
//! adapter-pattern doc's "extract only when forced" rule.
//!
//! ## Partition key — `(team_id, channel_id)` vs. `u64` /
//! `i64`
//!
//! Slack's partition key is a string (`"{team_id}:{channel_id}"`
//! per Phase 108 Q3a) where Discord and Telegram used `u64`
//! and `i64` respectively. The routing `HashMap` is keyed
//! on `String` here; the shape is otherwise identical.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use aivyx_core::{
    CancellationToken, ChannelContext, ChannelError, ChannelPlatform, SessionId, StreamEvent,
    TurnOutcome,
};
use aivyx_slack::transport::{
    IncomingMessage, OutgoingMessage, SlackMorphismTransport, SlackTransport,
};

use crate::daemon_client::{self, DaemonSession};
use crate::daemon_ipc::{FrontendType, StreamEventPayload};
use crate::daemon_server::DaemonError;
use crate::gate_command;
use crate::team_command::{self, sender_allowed, TeamCommand};
use crate::team_dispatch;
use crate::team_trigger_state::{
    check_and_record_trigger, parse_confirm_reply, ConfirmReply, PendingTrigger,
};
use std::time::Instant;

// ---------------------------------------------------------------------------
// SlackDaemonChannel — identity stub for the daemon's ChannelFactory
// ---------------------------------------------------------------------------

/// Lightweight `ChannelContext` stub that reports `Slack` platform
/// and a trust tier derived from whether the operator has an allowlist
/// (`[slack] channel_filter`) configured at all. Used by the daemon's
/// `ChannelFactory` when a `FrontendType::Slack` connection arrives.
/// Same shape as `TelegramDaemonChannel` and `DiscordDaemonChannel`;
/// `stream_event` and `finalize` are no-ops because the daemon-side
/// `IpcChannelBridge` handles forwarding events over IPC.
///
/// Daemon-first-path fix (2026-09-16, Task 10 fix round 2) — see
/// `TelegramDaemonChannel`'s identical doc comment for the full
/// rationale. This stub used to unconditionally return `SemiTrusted`,
/// and Slack additionally had zero routing-level filtering of any kind
/// in its daemon frontend before this fix (see
/// `run_slack_daemon_multi_session`'s new `channel_filter` parameter).
pub struct SlackDaemonChannel {
    session: SessionId,
    /// Rotated per turn by [`reset_cancellation`] and fired by
    /// [`cancel_inflight`]. See `TelegramDaemonChannel::token` —
    /// same C1+H1 audit fix.
    token: Mutex<CancellationToken>,
    /// Whether the operator has `[slack] channel_filter` configured at
    /// all. See `TelegramDaemonChannel::allowlist_configured`.
    allowlist_configured: bool,
}

impl SlackDaemonChannel {
    pub fn new(allowlist_configured: bool) -> Self {
        SlackDaemonChannel {
            session: SessionId::new(),
            token: Mutex::new(CancellationToken::new()),
            allowlist_configured,
        }
    }
}

impl Default for SlackDaemonChannel {
    fn default() -> Self {
        // Safe default: no allowlist configured ⇒ Untrusted. Real
        // construction always goes through `new()` via the
        // `ChannelFactory` closure in `aivyx.rs`.
        Self::new(false)
    }
}

#[async_trait::async_trait]
impl ChannelContext for SlackDaemonChannel {
    fn channel_name(&self) -> &str {
        "aivyx-slack-daemon"
    }

    fn platform(&self) -> ChannelPlatform {
        ChannelPlatform::Slack
    }

    fn trust_tier(&self) -> aivyx_capability::TrustTier {
        if self.allowlist_configured {
            aivyx_capability::TrustTier::SemiTrusted
        } else {
            aivyx_capability::TrustTier::Untrusted
        }
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
        let mut slot = self.token.lock().expect("token mutex poisoned");
        *slot = CancellationToken::new();
    }

    fn cancel_inflight(&self) {
        self.token.lock().expect("token mutex poisoned").cancel();
    }
}

// ---------------------------------------------------------------------------
// Multi-channel pump
// ---------------------------------------------------------------------------

struct PartitionRoute {
    sender: tokio::sync::mpsc::Sender<IncomingMessage>,
    handle: tokio::task::JoinHandle<Result<(), DaemonError>>,
}

/// Routing-level drop-filter: `true` iff `channel_id` may be routed to
/// a session. `filter: None` (no `[slack] channel_filter` configured)
/// accepts every channel. Mirrors
/// `telegram_daemon_frontend::telegram_chat_is_allowed`; extended in
/// Task 10 fix round 3 (2026-09-16) to also check `team_id`, closing
/// a gap round 2 left open — extracted so it's directly unit-testable
/// without a live transport/socket. Matches
/// `SlackChannel::trust_tier()`'s own in-process semantics: a message
/// is routable when its `channel_id` matches the configured
/// `channel_filter` (or no `channel_filter` is configured at all) AND
/// its `team_id` matches the configured `team_filter` (or no
/// `team_filter` is configured). Round 2 checked `channel_filter`
/// only; an operator who configured both a `channel_filter` and a
/// `team_filter` (defense in depth against a channel-id collision
/// across two workspaces the bot is installed in) got a real gap in
/// daemon mode — a colliding channel id from the wrong workspace
/// passed this routing filter and reached `SemiTrusted`, where the
/// in-process path would have correctly rejected it as `Untrusted`.
fn slack_channel_is_allowed(
    channel_id: &str,
    channel_filter: Option<&str>,
    team_id: &str,
    team_filter: Option<&str>,
) -> bool {
    let channel_ok = match channel_filter {
        Some(allowed) => channel_id == allowed,
        None => true,
    };
    let team_ok = match team_filter {
        Some(allowed) => team_id == allowed,
        None => true,
    };
    channel_ok && team_ok
}

/// Drive a multi-channel Slack frontend over the daemon IPC
/// channel. Mirrors `run_discord_daemon_multi_session` (Phase
/// 111 Task 3) and `run_telegram_daemon_multi_session` (Phase
/// 19); the only platform-specific surface is the partition
/// key — Slack uses `String` for `(team_id, channel_id)`
/// rather than Discord's `u64` or Telegram's `i64`.
#[allow(clippy::too_many_arguments)]
pub async fn run_slack_daemon_multi_session(
    transport: Arc<SlackMorphismTransport>,
    channel_filter: Option<String>,
    // Optional workspace (`team_id`) constraint, mirroring
    // `SlackChannel::team_filter` — see `slack_channel_is_allowed`.
    // Task 10 fix round 3 (2026-09-16).
    team_filter: Option<String>,
    socket_path: PathBuf,
    role: Option<String>,
    shutdown: CancellationToken,
    team_run_channel: bool,
    team_trigger_rate_limit: Option<u32>,
    team_command_allowed_senders: Vec<String>,
) -> Result<(), DaemonError> {
    let mut routes: HashMap<String, PartitionRoute> = HashMap::new();

    loop {
        if shutdown.is_cancelled() {
            break;
        }

        let msg = tokio::select! {
            biased;
            _ = shutdown.cancelled() => break,
            res = transport.next_message() => match res {
                Ok(m) => m,
                Err(e) => {
                    eprintln!(
                        "aivyx-slack(daemon): next_message failed ({e}); backing off 1s"
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    continue;
                }
            }
        };

        if !slack_channel_is_allowed(
            &msg.channel_id,
            channel_filter.as_deref(),
            &msg.team_id,
            team_filter.as_deref(),
        ) {
            continue;
        }

        let partition = msg.partition_key();

        let route = routes.entry(partition.clone()).or_insert_with(|| {
            let (tx, rx) = tokio::sync::mpsc::channel(32);
            let transport_clone = Arc::clone(&transport);
            let sp = socket_path.clone();
            let role_clone = role.clone();
            let shutdown_clone = shutdown.clone();
            let partition_for_task = partition.clone();
            let team_command_allowed_senders = team_command_allowed_senders.clone();
            let handle = tokio::spawn(async move {
                run_slack_daemon_partition_task(
                    transport_clone,
                    partition_for_task,
                    sp,
                    role_clone,
                    rx,
                    shutdown_clone,
                    team_run_channel,
                    team_trigger_rate_limit,
                    team_command_allowed_senders,
                )
                .await
            });
            PartitionRoute { sender: tx, handle }
        });

        if let Err(e) = route.sender.send(msg).await {
            eprintln!(
                "aivyx-slack(daemon): partition {partition} mailbox send failed ({e}); dropping route"
            );
            routes.remove(&partition);
        }
    }

    let drained: Vec<(String, PartitionRoute)> = routes.drain().collect();
    for (_partition, PartitionRoute { sender, handle }) in drained {
        drop(sender);
        match handle.await {
            Ok(Err(e)) => eprintln!("aivyx-slack(daemon): inner task error: {e}"),
            Err(e) => eprintln!("aivyx-slack(daemon): inner task join failed: {e}"),
            Ok(Ok(())) => {}
        }
    }

    Ok(())
}

/// Outcome of [`handle_slack_team_run_message`] — mirrors
/// `telegram_daemon_frontend::TelegramChatOutcome` /
/// `discord_daemon_frontend::DiscordChatOutcome` exactly.
#[derive(Debug, PartialEq, Eq)]
enum SlackChatOutcome {
    /// Reply immediately with this text; do not forward to the LLM turn
    /// path or the generic `gate_command`/`team_command` dispatch below.
    Reply(String),
    /// Nothing here matched (not a pending confirm resolution, not a
    /// fresh `/team run`) — the caller should fall through to its own
    /// existing `gate_command`/`team_command` dispatch and, ultimately,
    /// `session.submit_input`.
    NotHandled,
}

/// Extracted from `run_slack_daemon_partition_task` specifically so the
/// ordering invariant (the pending-confirm check and `/team run`
/// recognition MUST be checked before the generic `team_command`
/// dispatch, or `/team run` becomes permanently unreachable dead code —
/// a real bug this exact branch shipped once and had to fix in all
/// three channels, see the final-review report) is directly testable
/// without a live transport or socket. Mirrors
/// `telegram_daemon_frontend::handle_telegram_team_run_message` exactly
/// (same control flow, same fall-through-overwrite semantics) — see that
/// function's own doc comment for the detailed rationale. Takes only
/// `text`, not the partition key or a live `SlackMorphismTransport`: the
/// per-partition `channel_id`/reply-sending shape stays entirely in the
/// thin caller wrapper.
#[allow(clippy::too_many_arguments)]
async fn handle_slack_team_run_message(
    text: &str,
    socket_path: &Path,
    pending_trigger: &mut Option<PendingTrigger<String>>,
    trigger_history: &mut Vec<Instant>,
    team_run_channel: bool,
    team_trigger_rate_limit: Option<u32>,
    sender_id: &str,
) -> SlackChatOutcome {
    // Piece C — a pending confirm-first prompt takes priority over
    // everything else (including a stray gate_command/team_command
    // match, though "yes"/"no" never collide with either's own
    // `/`-prefixed syntax). Must run before both the gate_command
    // check below and Piece B's own generic `team_command::parse`
    // dispatch — the latter matches every `TeamCommand` variant
    // including `Run` and would otherwise route a fresh `/team run`
    // straight into `team_dispatch::dispatch`'s deliberate "should
    // never be dispatched directly" stub reply.
    if let Some(pending) = pending_trigger.take() {
        let now = Instant::now();
        match parse_confirm_reply(text) {
            Some(ConfirmReply::Yes) | Some(ConfirmReply::No)
                if pending.sender_id != sender_id =>
            {
                // Sender Allowlist final-review fix (2026-08-24) — a
                // different sender than the one who staged this trigger
                // replied yes/no. Only the staging sender may confirm or
                // cancel their own request (a bare "yes"/"no" never
                // parses as a /team command, so it never reaches the
                // sender-allowlist check in handle_slack_incoming_command
                // at all). Put the trigger back (unless it just expired)
                // and fall through to normal handling, exactly like an
                // unparseable reply — deliberately no denial reply, since
                // revealing that a pending trigger exists to an
                // uninvolved sender would leak information.
                if !pending.is_expired(now) {
                    *pending_trigger = Some(pending);
                }
            }
            Some(ConfirmReply::Yes) if pending.is_expired(now) => {
                return SlackChatOutcome::Reply("✗ that request expired, ask again.".to_string());
            }
            Some(ConfirmReply::Yes) => {
                let reply = match daemon_client::run_team_mission_channel(
                    socket_path,
                    FrontendType::Slack,
                    pending.goal.clone(),
                )
                .await
                {
                    Ok(mission_id) => format!("✓ Started mission {mission_id}."),
                    Err(e) => format!("✗ Could not start the mission: {e}"),
                };
                return SlackChatOutcome::Reply(reply);
            }
            Some(ConfirmReply::No) => {
                return SlackChatOutcome::Reply("Cancelled.".to_string());
            }
            None => {
                // Not a yes/no reply — put the pending trigger back
                // (unless it just expired) and fall through to the
                // normal command/chat-turn handling below.
                if !pending.is_expired(now) {
                    *pending_trigger = Some(pending);
                }
            }
        }
    }

    // Piece C — `/team run <goal>` itself. Must also run before
    // Piece B's generic `team_command::parse` block below, for the
    // same reason as the pending-trigger check above.
    if let Some(TeamCommand::Run { goal }) = team_command::parse(text) {
        if !team_run_channel {
            return SlackChatOutcome::Reply(
                "✗ this channel is not authorized to start team missions.".to_string(),
            );
        }
        let allowed = match team_trigger_rate_limit {
            Some(limit) => check_and_record_trigger(trigger_history, limit, Instant::now()),
            None => true,
        };
        if !allowed {
            let limit = team_trigger_rate_limit.unwrap_or(0);
            return SlackChatOutcome::Reply(format!(
                "✗ too many mission-start requests (max {limit} per hour), \
                 try again later."
            ));
        }
        *pending_trigger = Some(PendingTrigger::new(goal.clone(), sender_id.to_string()));
        return SlackChatOutcome::Reply(format!(
            "Start '{goal}' on the default team? Reply yes/no."
        ));
    }

    SlackChatOutcome::NotHandled
}

/// Outcome of [`handle_slack_incoming_command`] — mirrors
/// `telegram_daemon_frontend::TelegramIncomingOutcome` /
/// `discord_daemon_frontend::DiscordIncomingOutcome` exactly.
#[derive(Debug, PartialEq, Eq)]
enum SlackIncomingOutcome {
    /// Reply with this text; do not forward to the LLM turn path.
    Reply(String),
    /// Nothing matched any native command — forward to the normal
    /// chat-turn path (after the caller's own `gate_command::parse`
    /// check, which never collides with anything handled here).
    ForwardToChatTurn,
}

/// Owns the full `/team run` vs. generic `/team ...` precedence chain for
/// one inbound Slack message. Mirrors
/// `telegram_daemon_frontend::handle_telegram_incoming_command` exactly —
/// see that function's own doc comment for the detailed rationale (the
/// re-review that found the prior fix wave's tests, on
/// `handle_slack_team_run_message` alone, didn't prove the real loop's
/// call-site order).
///
/// `gate_command::parse` is deliberately NOT folded in here — see the
/// Telegram sibling's doc comment for why it's safe to leave inline in
/// the loop.
#[allow(clippy::too_many_arguments)]
async fn handle_slack_incoming_command(
    text: &str,
    socket_path: &Path,
    pending_trigger: &mut Option<PendingTrigger<String>>,
    trigger_history: &mut Vec<Instant>,
    team_run_channel: bool,
    team_trigger_rate_limit: Option<u32>,
    sender_id: String,
    allowed_senders: &[String],
) -> SlackIncomingOutcome {
    // Team-Command Sender Allowlist (2026-08-23) — must run before
    // BOTH handle_slack_team_run_message (so an unauthorized /team run
    // is denied before it can even stage a confirm-first prompt) and
    // the generic team_command::parse dispatch below. Checked only
    // when the text actually parses as a /team command at all --
    // ordinary chat text from an unauthorized sender is completely
    // unaffected.
    //
    // Note this check alone does NOT protect the later "yes"/"no"
    // confirm-reply step -- a bare "yes" never parses as a /team
    // command, so it never reaches this check at all. That step is
    // separately protected by binding each PendingTrigger to the
    // sender_id that staged it (final-review fix, 2026-08-24; see
    // `handle_slack_team_run_message`): only the sender who was asked
    // "Start '<goal>' on the default team?" can answer it, so another
    // allowlisted sender in the same chat can't hijack or cancel
    // someone else's staged mission.
    if team_command::parse(text).is_some() && !sender_allowed(allowed_senders, &sender_id) {
        return SlackIncomingOutcome::Reply(
            "✗ you are not authorized to issue /team commands.".to_string(),
        );
    }

    match handle_slack_team_run_message(
        text,
        socket_path,
        pending_trigger,
        trigger_history,
        team_run_channel,
        team_trigger_rate_limit,
        &sender_id,
    )
    .await
    {
        SlackChatOutcome::Reply(reply) => return SlackIncomingOutcome::Reply(reply),
        SlackChatOutcome::NotHandled => {}
    }

    if let Some(team_cmd) = team_command::parse(text) {
        let reply = team_dispatch::dispatch(socket_path, team_cmd).await;
        return SlackIncomingOutcome::Reply(reply);
    }

    SlackIncomingOutcome::ForwardToChatTurn
}

/// Per-partition inner task. Mirrors
/// `run_discord_daemon_channel_task` (Phase 111 Task 3) and
/// `run_telegram_daemon_chat_task` (Phase 19) — the only
/// per-adapter difference is the channel-id type used to
/// route outbound `OutgoingMessage`s.
#[allow(clippy::too_many_arguments)]
async fn run_slack_daemon_partition_task(
    transport: Arc<SlackMorphismTransport>,
    partition: String,
    socket_path: PathBuf,
    role: Option<String>,
    mut mailbox: tokio::sync::mpsc::Receiver<IncomingMessage>,
    shutdown: CancellationToken,
    team_run_channel: bool,
    team_trigger_rate_limit: Option<u32>,
    team_command_allowed_senders: Vec<String>,
) -> Result<(), DaemonError> {
    let mut session = DaemonSession::connect(
        &socket_path,
        role,
        Some(FrontendType::Slack),
    )
    .await?;

    let mut pending_trigger: Option<PendingTrigger<String>> = None;
    let mut trigger_history: Vec<Instant> = Vec::new();

    loop {
        let msg = tokio::select! {
            biased;
            _ = shutdown.cancelled() => break,
            maybe_msg = mailbox.recv() => match maybe_msg {
                Some(m) => m,
                None => break,
            }
        };

        if msg.text.trim() == "/cancel" {
            let _ = session.cancel_turn().await;
            continue;
        }

        // Piece C — the full `/team run` vs. generic `/team ...` precedence
        // chain lives in `handle_slack_incoming_command`, extracted
        // specifically so this ordering (both MUST run in the right order,
        // or `/team run` becomes permanently unreachable dead code) is
        // itself directly testable, not just each piece's own internal
        // correctness. See that function's own doc comment.
        match handle_slack_incoming_command(
            msg.text.trim(),
            &socket_path,
            &mut pending_trigger,
            &mut trigger_history,
            team_run_channel,
            team_trigger_rate_limit,
            msg.user_id.clone(),
            &team_command_allowed_senders,
        )
        .await
        {
            SlackIncomingOutcome::Reply(text) => {
                transport
                    .send_message(OutgoingMessage {
                        channel_id: msg.channel_id.clone(),
                        text,
                    })
                    .await
                    .map_err(|e| {
                        DaemonError::Internal(format!(
                            "send_message to partition {partition}: {e}"
                        ))
                    })?;
                continue;
            }
            SlackIncomingOutcome::ForwardToChatTurn => {}
        }

        if let Some(gate_cmd) = gate_command::parse(msg.text.trim()) {
            let result = session
                .resolve_gate(gate_cmd.mission_id, gate_cmd.gate_id, gate_cmd.approved)
                .await;
            let reply = match result {
                Ok(()) => {
                    let status = if gate_cmd.approved { "approved" } else { "rejected" };
                    format!("✓ Gate {status}.")
                }
                Err(e) => format!("✗ Gate resolve failed: {e}"),
            };
            transport
                .send_message(OutgoingMessage {
                    channel_id: msg.channel_id.clone(),
                    text: reply,
                })
                .await
                .map_err(|e| {
                    DaemonError::Internal(format!(
                        "send_message to partition {partition}: {e}"
                    ))
                })?;
            continue;
        }

        // Note: the generic `/team ...` dispatch is now folded into
        // `handle_slack_incoming_command` above (it must run after
        // `/team run` recognition within that single function for the
        // ordering invariant to be testable) — nothing else to do here.

        let (events, outcome) = session.submit_input(msg.text).await?;
        let buf = build_slack_reply(&events, &outcome);

        transport
            .send_message(OutgoingMessage {
                channel_id: msg.channel_id.clone(),
                text: buf,
            })
            .await
            .map_err(|e| {
                DaemonError::Internal(format!(
                    "send_message to partition {partition}: {e}"
                ))
            })?;
    }

    let _ = session.disconnect().await;
    Ok(())
}

/// Render accumulated `StreamEventPayload` events into a
/// single Slack message. Byte-identical to
/// `render_events_for_discord` (Phase 111 Task 3) and
/// structurally identical to `render_events_for_telegram`
/// (Phase 19) — the three SemiTrusted adapters all produce
/// the same in-message UX deliberately, so an operator
/// switching between them sees no surprise UI deltas.
pub(crate) fn render_events_for_slack(events: &[StreamEventPayload]) -> String {
    let mut buf = String::new();
    for event in events {
        match event {
            StreamEventPayload::Text { text } => {
                buf.push_str(text);
            }
            StreamEventPayload::Status { status } => {
                if !buf.is_empty() && !buf.ends_with('\n') {
                    buf.push('\n');
                }
                buf.push_str("… ");
                buf.push_str(status);
                buf.push('\n');
            }
            StreamEventPayload::ToolCallStarted { tool_name, .. } => {
                if !buf.is_empty() && !buf.ends_with('\n') {
                    buf.push('\n');
                }
                buf.push_str("→ ");
                buf.push_str(tool_name);
                buf.push('\n');
            }
            StreamEventPayload::ToolCallFinished {
                tool_name,
                outcome_summary,
                ..
            } => {
                if !buf.is_empty() && !buf.ends_with('\n') {
                    buf.push('\n');
                }
                buf.push_str("← ");
                buf.push_str(tool_name);
                buf.push(' ');
                buf.push_str(outcome_summary);
                buf.push('\n');
            }
            StreamEventPayload::ToolOutput { .. } => {}
            StreamEventPayload::ApprovalGate {
                mission_id,
                gate_id,
                reason,
                ..
            } => {
                if !buf.is_empty() && !buf.ends_with('\n') {
                    buf.push('\n');
                }
                buf.push_str(&format!(
                    "⚑ APPROVAL GATE [{mission_id}/{gate_id}]: {reason}\n\
                     Reply /approve {mission_id} {gate_id}\n\
                     or    /reject  {mission_id} {gate_id}\n"
                ));
            }
        }
    }

    if buf.trim().is_empty() {
        "(no reply)".to_string()
    } else {
        buf
    }
}

/// Build the outbound Slack reply text: the rendered event journal
/// (tool-call/status lines + streamed text), plus a correction line
/// when the turn's own outcome diverges from what the events alone
/// would show (a reply floor, a Candor/identifier-fidelity annotation,
/// or a non-completed outcome's reason). Skips the correction only
/// when it would exactly duplicate `render_events_for_slack`'s own
/// empty-events "(no reply)" fallback.
fn build_slack_reply(events: &[StreamEventPayload], outcome: &str) -> String {
    let displayed = crate::daemon_ipc::concat_text_events(events);
    let mut buf = render_events_for_slack(events);
    if let Some(note) = crate::daemon_ipc::turn_outcome_correction(&displayed, outcome) {
        if !(note == "(no reply)" && buf.trim() == "(no reply)") {
            if !buf.is_empty() && !buf.ends_with('\n') {
                buf.push('\n');
            }
            buf.push_str(&note);
        }
    }
    buf
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon_ipc::StreamEventPayload;

    #[test]
    fn slack_daemon_channel_reports_correct_identity() {
        let c = SlackDaemonChannel::new(true);
        assert_eq!(c.channel_name(), "aivyx-slack-daemon");
        assert_eq!(c.platform(), ChannelPlatform::Slack);
        assert_eq!(c.trust_tier(), aivyx_capability::TrustTier::SemiTrusted);
    }

    // --- Task 10 fix round 2 (2026-09-16) — the daemon-first-path gap.
    // See `telegram_daemon_frontend`'s identical tests for the full
    // rationale.

    #[test]
    fn stub_reports_untrusted_when_no_allowlist_configured() {
        let c = SlackDaemonChannel::new(false);
        assert_eq!(
            c.trust_tier(),
            aivyx_capability::TrustTier::Untrusted,
            "no channel_filter configured must mean every reachable sender is Untrusted"
        );
    }

    #[test]
    fn stub_reports_semitrusted_when_allowlist_configured() {
        let c = SlackDaemonChannel::new(true);
        assert_eq!(
            c.trust_tier(),
            aivyx_capability::TrustTier::SemiTrusted,
            "a configured channel_filter means messages reaching this stub already matched it"
        );
    }

    // --- Routing-level drop-filter (the other half of Task 10 fix
    // round 2). Slack had zero filtering of any kind before this.

    #[test]
    fn channel_filter_none_allows_any_channel() {
        assert!(slack_channel_is_allowed("C111", None, "T01", None));
        assert!(slack_channel_is_allowed("C999", None, "T01", None));
    }

    #[test]
    fn channel_filter_some_allows_only_the_matching_channel() {
        assert!(slack_channel_is_allowed("C111", Some("C111"), "T01", None));
        assert!(
            !slack_channel_is_allowed("C999", Some("C111"), "T01", None),
            "a non-matching channel must be dropped before reaching a session"
        );
    }

    // Task 10 fix round 3 (2026-09-16) — `team_filter` parity with
    // the in-process `SlackChannel::trust_tier()`. Round 2 only
    // checked `channel_filter`, leaving a gap for a channel-id
    // collision across two different workspaces.

    #[test]
    fn team_filter_none_allows_any_workspace() {
        assert!(slack_channel_is_allowed("C111", Some("C111"), "T01", None));
        assert!(slack_channel_is_allowed("C111", Some("C111"), "T99", None));
    }

    #[test]
    fn team_filter_mismatch_denies_even_with_matching_channel_filter() {
        assert!(
            !slack_channel_is_allowed("C111", Some("C111"), "T99", Some("T01")),
            "a colliding channel id from the wrong workspace must be dropped, \
             not routed to a session"
        );
    }

    #[test]
    fn team_filter_and_channel_filter_both_matching_allows() {
        assert!(slack_channel_is_allowed(
            "C111",
            Some("C111"),
            "T01",
            Some("T01")
        ));
    }

    // Audit C1+H1 regression — same coverage as the
    // Telegram + Discord stubs.
    #[test]
    fn cancel_inflight_then_reset_yields_fresh_token() {
        let c = SlackDaemonChannel::new(false);
        let stale = c.cancellation_token();
        assert!(!stale.is_cancelled());
        c.cancel_inflight();
        assert!(stale.is_cancelled(), "cancel_inflight fires the live token");
        c.reset_cancellation();
        assert!(
            !c.cancellation_token().is_cancelled(),
            "reset_cancellation installs a fresh token"
        );
        assert!(stale.is_cancelled(), "the pre-reset token stays cancelled");
    }

    #[test]
    fn render_events_text_concatenates() {
        let events = vec![
            StreamEventPayload::Text { text: "hello ".into() },
            StreamEventPayload::Text { text: "slack".into() },
        ];
        assert_eq!(render_events_for_slack(&events), "hello slack");
    }

    #[test]
    fn render_events_tool_call_arrows_render_consistently() {
        let events = vec![
            StreamEventPayload::ToolCallStarted {
                tool_id: "tid".into(),
                tool_name: "memory.read".into(),
                input: serde_json::Value::Null,
            },
            StreamEventPayload::ToolCallFinished {
                tool_id: "tid".into(),
                tool_name: "memory.read".into(),
                outcome_summary: "ok".into(),
            },
        ];
        let out = render_events_for_slack(&events);
        assert!(out.contains("→ memory.read"));
        assert!(out.contains("← memory.read ok"));
    }

    #[test]
    fn render_events_empty_payload_returns_no_reply_placeholder() {
        assert_eq!(render_events_for_slack(&[]), "(no reply)");
    }

    #[test]
    fn build_slack_reply_appends_correction_when_outcome_differs() {
        let events = vec![StreamEventPayload::Text {
            text: "{\"path\": \"airports.csv\"}".into(),
        }];
        let out = build_slack_reply(
            &events,
            "completed: I wasn't able to produce a usable reply this turn — please try again.",
        );
        assert!(out.contains("{\"path\": \"airports.csv\"}"), "{out}");
        assert!(out.contains("corrected"), "{out}");
    }

    #[test]
    fn build_slack_reply_no_correction_when_outcome_matches() {
        let events = vec![StreamEventPayload::Text {
            text: "an answer".into(),
        }];
        let out = build_slack_reply(&events, "completed: an answer");
        assert_eq!(out, "an answer");
    }

    #[test]
    fn build_slack_reply_does_not_double_up_no_reply() {
        let out = build_slack_reply(&[], "completed: ");
        assert_eq!(out, "(no reply)", "must not print (no reply) twice: {out}");
    }

    #[test]
    fn build_slack_reply_surfaces_non_completed_outcome() {
        let out = build_slack_reply(&[], "timed out");
        assert!(out.contains("timed out"), "{out}");
    }

    #[test]
    fn render_events_byte_identical_to_discord_renderer() {
        // Three-data-point sanity check: the Slack and Discord
        // renderers must produce byte-identical output for the
        // same StreamEventPayload sequence. If a future divergence
        // ships (e.g. Slack-specific markdown), this test breaks
        // and forces the divergence to be documented.
        let events = vec![
            StreamEventPayload::Text { text: "result: ".into() },
            StreamEventPayload::ToolCallStarted {
                tool_id: "tid".into(),
                tool_name: "fs.read".into(),
                input: serde_json::Value::Null,
            },
            StreamEventPayload::Status { status: "thinking".into() },
            StreamEventPayload::ApprovalGate {
                mission_id: "m-001".into(),
                gate_id: "g-abc".into(),
                scope: Some("fs.write".into()),
                reason: "write a file".into(),
            },
        ];
        let slack_out = render_events_for_slack(&events);
        let discord_out =
            crate::discord_daemon_frontend::render_events_for_discord(&events);
        assert_eq!(slack_out, discord_out);
    }

    // --- I5 (final-review) — the ordering invariant that keeps
    // `/team run` from being permanently unreachable, locked in by
    // testing `handle_slack_team_run_message` directly.

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixListener;

    /// A fake daemon that does the `StartSession` handshake then
    /// replies `TeamMissionChannelStarted` — mirrors
    /// `daemon_client.rs`'s own
    /// `run_team_mission_channel_does_the_start_session_handshake_then_sends_the_request`
    /// fixture, since `handle_slack_team_run_message`'s "yes" path calls
    /// the real `daemon_client::run_team_mission_channel`.
    async fn fake_daemon_starting_mission(
        mission_id: &str,
    ) -> (PathBuf, tokio::task::JoinHandle<()>) {
        use crate::daemon_ipc::{encode_frame, DaemonEnvelope};

        let sock = std::env::temp_dir()
            .join(format!("aivyx-slack-teamrun-{}.sock", uuid::Uuid::new_v4()));
        let listener = UnixListener::bind(&sock).expect("bind fake daemon");
        let sock_clone = sock.clone();
        let mission_id = mission_id.to_string();

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let ready = encode_frame(&DaemonEnvelope::DaemonReady {
                version: "0.1".into(),
            })
            .expect("encode ready");
            stream.write_all(&ready).await.expect("write ready");

            let mut tmp = [0u8; 2048];
            let _ = stream.read(&mut tmp).await; // client's StartSession
            let started = encode_frame(&DaemonEnvelope::SessionStarted {
                session_id: "sess-1".into(),
            })
            .expect("encode started");
            stream.write_all(&started).await.expect("write started");

            let _ = stream.read(&mut tmp).await; // client's RunTeamMissionChannel
            let resp = encode_frame(&DaemonEnvelope::TeamMissionChannelStarted { mission_id })
                .expect("encode resp");
            stream.write_all(&resp).await.expect("write resp");
            let _ = stream.read(&mut tmp).await;
        });

        (sock_clone, server)
    }

    #[tokio::test]
    async fn team_run_authorized_and_under_limit_prompts_for_confirmation() {
        // This is the test that would have caught the original ordering
        // bug: if the generic `team_command`/`team_dispatch` dispatch
        // ran first, `/team run` would hit `team_dispatch::dispatch`'s
        // "should never be dispatched directly" stub instead of this
        // confirm prompt.
        let mut pending: Option<PendingTrigger<String>> = None;
        let mut history: Vec<Instant> = Vec::new();

        let outcome = handle_slack_team_run_message(
            "/team run close the books",
            Path::new("/nonexistent/unused.sock"),
            &mut pending,
            &mut history,
            true,
            None,
            "U123ALLOWED",
        )
        .await;

        match outcome {
            SlackChatOutcome::Reply(text) => {
                assert!(
                    !text.contains("should never be dispatched directly"),
                    "must not fall through to the generic team_dispatch stub: {text}"
                );
                assert!(
                    text.contains("Reply yes/no"),
                    "must prompt for confirmation: {text}"
                );
            }
            SlackChatOutcome::NotHandled => panic!("expected a Reply, got NotHandled"),
        }
        assert!(pending.is_some(), "a pending trigger must now be recorded");
    }

    #[tokio::test]
    async fn team_run_denied_when_channel_not_opted_in() {
        let mut pending: Option<PendingTrigger<String>> = None;
        let mut history: Vec<Instant> = Vec::new();

        let outcome = handle_slack_team_run_message(
            "/team run close the books",
            Path::new("/nonexistent/unused.sock"),
            &mut pending,
            &mut history,
            false,
            None,
            "U123ALLOWED",
        )
        .await;

        match outcome {
            SlackChatOutcome::Reply(text) => {
                assert!(text.contains("not authorized"), "got: {text}");
            }
            SlackChatOutcome::NotHandled => panic!("expected a Reply, got NotHandled"),
        }
        assert!(pending.is_none(), "an unauthorized attempt must not arm a pending trigger");
    }

    #[tokio::test]
    async fn ordinary_text_with_no_pending_trigger_is_not_handled() {
        let mut pending: Option<PendingTrigger<String>> = None;
        let mut history: Vec<Instant> = Vec::new();

        let outcome = handle_slack_team_run_message(
            "just chatting, nothing special",
            Path::new("/nonexistent/unused.sock"),
            &mut pending,
            &mut history,
            true,
            None,
            "U123ALLOWED",
        )
        .await;

        assert_eq!(outcome, SlackChatOutcome::NotHandled);
        assert!(pending.is_none());
    }

    #[tokio::test]
    async fn pending_trigger_plus_yes_starts_the_mission_via_the_real_daemon_client_call() {
        let (sock, server) = fake_daemon_starting_mission("m-42").await;

        let mut pending = Some(PendingTrigger::new("close the books", "U123ALLOWED".to_string()));
        let mut history: Vec<Instant> = Vec::new();

        let outcome = handle_slack_team_run_message(
            "yes", &sock, &mut pending, &mut history, true, None, "U123ALLOWED",
        )
        .await;

        match outcome {
            SlackChatOutcome::Reply(text) => {
                assert!(text.contains("Started mission"), "got: {text}");
                assert!(text.contains("m-42"));
            }
            SlackChatOutcome::NotHandled => panic!("expected a Reply, got NotHandled"),
        }
        assert!(pending.is_none(), "the pending trigger is consumed on yes");

        let _ = server.await;
        let _ = std::fs::remove_file(&sock);
    }

    #[tokio::test]
    async fn pending_trigger_plus_no_cancels() {
        let mut pending = Some(PendingTrigger::new("close the books", "U123ALLOWED".to_string()));
        let mut history: Vec<Instant> = Vec::new();

        let outcome = handle_slack_team_run_message(
            "no",
            Path::new("/nonexistent/unused.sock"),
            &mut pending,
            &mut history,
            true,
            None,
            "U123ALLOWED",
        )
        .await;

        match outcome {
            SlackChatOutcome::Reply(text) => {
                assert!(text.contains("Cancelled."), "got: {text}");
            }
            SlackChatOutcome::NotHandled => panic!("expected a Reply, got NotHandled"),
        }
        assert!(pending.is_none(), "a 'no' reply clears the pending trigger");
    }

    // --- Re-review fix — the genuine ordering-lock test. See
    // `telegram_daemon_frontend`'s identical test for the full rationale:
    // the 15 tests above only ever call `handle_slack_team_run_message`
    // directly, so they lock in that function's own internal correctness
    // but never the real loop's call-site order relative to
    // `team_command::parse`. This test calls
    // `handle_slack_incoming_command` — the function that now owns BOTH
    // steps in one place — so a regression in their relative order fails
    // here directly.

    #[tokio::test]
    async fn team_run_is_recognized_before_the_generic_team_command_dispatch() {
        let mut pending_trigger = None;
        let mut trigger_history = Vec::new();
        let outcome = handle_slack_incoming_command(
            "/team run close the books",
            std::path::Path::new("/nonexistent/unused.sock"),
            &mut pending_trigger,
            &mut trigger_history,
            true,  // team_run_channel
            None,  // team_trigger_rate_limit
            "U123ALLOWED".to_string(),
            &["U123ALLOWED".to_string(), "U456ALLOWED".to_string()],
        )
        .await;
        match outcome {
            SlackIncomingOutcome::Reply(text) => {
                assert!(
                    text.contains("Reply yes/no"),
                    "expected the confirm prompt, got: {text}"
                );
                assert!(
                    !text.contains("should never be dispatched directly"),
                    "got the generic-dispatch stub reply instead of the confirm prompt \
                     -- this means /team run is being swallowed by team_command::parse's \
                     dispatch again, the exact bug this test exists to catch: {text}"
                );
            }
            SlackIncomingOutcome::ForwardToChatTurn => {
                panic!("/team run was not recognized at all")
            }
        }
        assert!(pending_trigger.is_some(), "a pending trigger should now be set");
    }

    // --- Sender Allowlist Task 5 — the sender-allowlist check must run
    // before /team run recognition, or an unauthorized sender's /team run
    // still reaches the confirm-first flow. Mirrors Task 3/4's own tests.

    #[tokio::test]
    async fn unauthorized_sender_is_denied_before_team_run_recognition() {
        let mut pending_trigger = None;
        let mut trigger_history = Vec::new();
        let outcome = handle_slack_incoming_command(
            "/team run close the books",
            std::path::Path::new("/nonexistent/unused.sock"),
            &mut pending_trigger,
            &mut trigger_history,
            true,
            None,
            "U999NOTALLOWED".to_string(),
            &["U123ALLOWED".to_string(), "U456ALLOWED".to_string()],
        )
        .await;
        match outcome {
            SlackIncomingOutcome::Reply(text) => {
                assert!(
                    text.contains("not authorized to issue /team commands"),
                    "expected the sender-denial reply, got: {text}"
                );
                assert!(
                    !text.contains("Reply yes/no"),
                    "got the confirm-first prompt instead of the sender-denial \
                     reply -- this means the sender-allowlist check is being \
                     bypassed by /team run's own recognition, the exact bug \
                     this test exists to catch: {text}"
                );
            }
            SlackIncomingOutcome::ForwardToChatTurn => {
                panic!("expected a denial reply, not a forward to chat turn")
            }
        }
        assert!(
            pending_trigger.is_none(),
            "an unauthorized /team run must not set a pending trigger"
        );
    }

    #[tokio::test]
    async fn unauthorized_sender_is_denied_for_the_generic_team_surface_too() {
        let mut pending_trigger = None;
        let mut trigger_history = Vec::new();
        let outcome = handle_slack_incoming_command(
            "/team status",
            std::path::Path::new("/nonexistent/unused.sock"),
            &mut pending_trigger,
            &mut trigger_history,
            false,
            None,
            "U999NOTALLOWED".to_string(),
            &["U123ALLOWED".to_string(), "U456ALLOWED".to_string()],
        )
        .await;
        match outcome {
            SlackIncomingOutcome::Reply(text) => {
                assert!(text.contains("not authorized to issue /team commands"));
            }
            SlackIncomingOutcome::ForwardToChatTurn => {
                panic!("expected a denial reply, not a forward to chat turn")
            }
        }
    }

    #[tokio::test]
    async fn authorized_sender_reaches_dispatch_not_the_denial() {
        let mut pending_trigger = None;
        let mut trigger_history = Vec::new();
        let outcome = handle_slack_incoming_command(
            "/team status",
            std::path::Path::new("/nonexistent/unused.sock"),
            &mut pending_trigger,
            &mut trigger_history,
            false,
            None,
            "U123ALLOWED".to_string(),
            &["U123ALLOWED".to_string(), "U456ALLOWED".to_string()],
        )
        .await;
        match outcome {
            SlackIncomingOutcome::Reply(text) => {
                assert!(!text.contains("not authorized to issue /team commands"));
            }
            SlackIncomingOutcome::ForwardToChatTurn => {
                panic!("expected a Reply (dispatch attempted), not ForwardToChatTurn")
            }
        }
    }

    #[tokio::test]
    async fn non_team_text_is_unaffected_regardless_of_sender() {
        let mut pending_trigger = None;
        let mut trigger_history = Vec::new();
        let outcome = handle_slack_incoming_command(
            "hello, just chatting",
            std::path::Path::new("/nonexistent/unused.sock"),
            &mut pending_trigger,
            &mut trigger_history,
            false,
            None,
            "U999NOTALLOWED".to_string(),
            &["U123ALLOWED".to_string(), "U456ALLOWED".to_string()],
        )
        .await;
        assert_eq!(outcome, SlackIncomingOutcome::ForwardToChatTurn);
    }

    // --- Final-review Finding 1 (2026-08-24) — the confirm-reply
    // sender-binding fix. See the identical Telegram tests for the full
    // rationale: before this fix, ANY allowlisted sender in the same
    // partition could resolve a trigger staged by someone else, because
    // a bare "yes"/"no" never parses as a /team command and so never
    // reaches the sender-allowlist check above.

    #[tokio::test]
    async fn wrong_sender_yes_does_not_resolve_someone_elses_pending_trigger() {
        let mut pending_trigger =
            Some(PendingTrigger::new("close the books", "U111STAGED".to_string()));
        let mut trigger_history = Vec::new();

        let outcome = handle_slack_incoming_command(
            "yes",
            std::path::Path::new("/nonexistent/unused.sock"),
            &mut pending_trigger,
            &mut trigger_history,
            true,
            None,
            "U222OTHER".to_string(), // allowlisted, but not the staging sender
            &["U111STAGED".to_string(), "U222OTHER".to_string()],
        )
        .await;

        assert_eq!(
            outcome,
            SlackIncomingOutcome::ForwardToChatTurn,
            "a wrong-sender 'yes' must fall through like ordinary chat text, \
             not resolve the trigger and not produce a special denial reply"
        );
        assert!(
            pending_trigger.is_some(),
            "sender A's pending trigger must survive an unrelated sender's 'yes'"
        );
        assert_eq!(pending_trigger.as_ref().unwrap().sender_id, "U111STAGED");
    }

    #[tokio::test]
    async fn correct_sender_yes_resolves_their_own_pending_trigger() {
        let (sock, server) = fake_daemon_starting_mission("m-77").await;

        let mut pending_trigger =
            Some(PendingTrigger::new("close the books", "U111STAGED".to_string()));
        let mut trigger_history = Vec::new();

        let outcome = handle_slack_incoming_command(
            "yes",
            &sock,
            &mut pending_trigger,
            &mut trigger_history,
            true,
            None,
            "U111STAGED".to_string(), // the sender who staged it
            &["U111STAGED".to_string(), "U222OTHER".to_string()],
        )
        .await;

        match outcome {
            SlackIncomingOutcome::Reply(text) => {
                assert!(text.contains("Started mission"), "got: {text}");
                assert!(text.contains("m-77"));
            }
            SlackIncomingOutcome::ForwardToChatTurn => {
                panic!("expected a Reply, got ForwardToChatTurn")
            }
        }
        assert!(
            pending_trigger.is_none(),
            "the pending trigger is consumed once the staging sender confirms"
        );

        let _ = server.await;
        let _ = std::fs::remove_file(&sock);
    }
}
