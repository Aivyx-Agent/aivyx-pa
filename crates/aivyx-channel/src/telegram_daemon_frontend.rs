//! Daemon-mode Telegram multi-chat pump — Phase 19 Task 3/4.
//!
//! Drives a multi-chat Telegram frontend over the daemon IPC channel.
//! The outer `get_updates` loop retains the same structure as the
//! in-process `run_telegram_multi_session`: one cursor, per-chat
//! routing, lazy spawn of inner tasks. Each inner task submits turns
//! through a `DaemonSession` instead of constructing an agent.
//!
//! Extracted from the binary in Task 4 to keep `aivyx.rs` under
//! the 2400-line threshold.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use aivyx_core::{
    CancellationToken, ChannelContext, ChannelError, ChannelPlatform, SessionId, StreamEvent,
    TurnOutcome,
};
use aivyx_telegram::transport::{IncomingMessage, OutgoingMessage, ReqwestTransport, TelegramTransport};

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

const LONG_POLL_TIMEOUT_SECS: u32 = 25;

// ---------------------------------------------------------------------------
// TelegramDaemonChannel — identity stub for the daemon's ChannelFactory
// ---------------------------------------------------------------------------

/// Lightweight `ChannelContext` stub that reports `Telegram` platform
/// and a trust tier derived from whether the operator has an allowlist
/// (`[telegram] chat_filter`) configured at all. Used by the daemon's
/// `ChannelFactory` when a `FrontendType::Telegram` connection arrives.
/// The stub's `stream_event` and `finalize` are no-ops — the
/// `IpcChannelBridge` handles forwarding those over IPC.
///
/// Daemon-first-path fix (2026-09-16, Task 10 fix round 2) — this used
/// to unconditionally return `SemiTrusted` regardless of configuration,
/// completely bypassing the per-sender authorization fix landed in
/// `aivyx-telegram::TelegramChannel::trust_tier()` for the in-process
/// path. Every daemon-routed Telegram session — the default runtime
/// mode with no flags — got the operator's full `SemiTrusted` ceiling
/// no matter what. See `run_telegram_daemon_multi_session`'s
/// `chat_filter` routing-drop-filter (the other half of this fix): a
/// message only ever reaches a session (and thus this stub) at all
/// when either no filter is configured (routing accepts everyone,
/// mirroring the pre-existing "Phase 8 compatibility knob" behavior)
/// or the filter matched. `allowlist_configured` distinguishes those
/// two cases for `trust_tier()`'s purposes: no allowlist configured
/// means every reachable sender is untrusted; an allowlist configured
/// means every message that reached this point already passed it.
pub struct TelegramDaemonChannel {
    session: SessionId,
    /// Rotated per turn by [`reset_cancellation`] and fired by
    /// [`cancel_inflight`]. Wrapped in `Mutex` so the daemon can
    /// install a fresh token between turns (C1+H1 fix from the
    /// Agent Loop audit — `CancellationToken` is monotonic, so a
    /// single timeout would otherwise brick every subsequent
    /// turn in this session).
    token: Mutex<CancellationToken>,
    /// Whether the operator has `[telegram] chat_filter` configured at
    /// all (computed once in `aivyx.rs` from
    /// `TelegramConfig::chat_filter.is_some()` and threaded through the
    /// `ChannelFactory` closure). Drives `trust_tier()` — see the
    /// struct doc comment above.
    allowlist_configured: bool,
}

impl TelegramDaemonChannel {
    pub fn new(allowlist_configured: bool) -> Self {
        TelegramDaemonChannel {
            session: SessionId::new(),
            token: Mutex::new(CancellationToken::new()),
            allowlist_configured,
        }
    }
}

impl Default for TelegramDaemonChannel {
    fn default() -> Self {
        // Safe default: no allowlist configured ⇒ Untrusted. Real
        // construction always goes through `new()` via the
        // `ChannelFactory` closure in `aivyx.rs`, which passes the
        // operator's actual configuration.
        Self::new(false)
    }
}

#[async_trait::async_trait]
impl ChannelContext for TelegramDaemonChannel {
    fn channel_name(&self) -> &str {
        "aivyx-telegram-daemon"
    }

    fn platform(&self) -> ChannelPlatform {
        ChannelPlatform::Telegram
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
// Multi-chat pump
// ---------------------------------------------------------------------------

struct ChatRoute {
    sender: tokio::sync::mpsc::Sender<IncomingMessage>,
    handle: tokio::task::JoinHandle<Result<(), DaemonError>>,
}

/// Routing-level drop-filter: `true` iff `chat_id` may be routed to a
/// session. `filter: None` (no `[telegram] chat_filter` configured)
/// accepts every chat — the pre-existing "Phase 8 compatibility knob"
/// behavior. Extracted from the loop body in `run_telegram_daemon_multi_session`
/// so it's directly unit-testable without a live transport/socket.
fn telegram_chat_is_allowed(chat_id: i64, filter: Option<i64>) -> bool {
    match filter {
        Some(allowed) => chat_id == allowed,
        None => true,
    }
}

/// Drive a multi-chat Telegram frontend over the daemon IPC channel.
///
/// Streamed `StreamEventPayload` events are accumulated per turn and
/// sent as a single Telegram message at `TurnComplete` (Q3→(b)).
/// `/cancel` is forwarded as `CancelTurn` over IPC.
#[allow(clippy::too_many_arguments)]
pub async fn run_telegram_daemon_multi_session(
    transport: Arc<ReqwestTransport>,
    chat_filter: Option<i64>,
    socket_path: PathBuf,
    role: Option<String>,
    shutdown: CancellationToken,
    team_run_channel: bool,
    team_trigger_rate_limit: Option<u32>,
    team_command_allowed_senders: Vec<i64>,
) -> Result<(), DaemonError> {
    let mut routes: HashMap<i64, ChatRoute> = HashMap::new();
    let mut offset: i64 = 0;

    loop {
        if shutdown.is_cancelled() {
            break;
        }

        let updates = tokio::select! {
            biased;
            _ = shutdown.cancelled() => break,
            res = transport.get_updates(offset, LONG_POLL_TIMEOUT_SECS) => match res {
                Ok(batch) => batch,
                Err(e) => {
                    eprintln!("aivyx-telegram(daemon): get_updates failed ({e}); backing off 1s");
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    continue;
                }
            }
        };

        if updates.is_empty() {
            continue;
        }

        for msg in updates {
            offset = offset.max(msg.update_id + 1);

            if !telegram_chat_is_allowed(msg.chat_id, chat_filter) {
                continue;
            }

            let chat_id = msg.chat_id;

            let route = routes.entry(chat_id).or_insert_with(|| {
                let (tx, rx) = tokio::sync::mpsc::channel(32);
                let transport_clone = Arc::clone(&transport);
                let sp = socket_path.clone();
                let role_clone = role.clone();
                let shutdown_clone = shutdown.clone();
                let team_command_allowed_senders = team_command_allowed_senders.clone();
                let handle = tokio::spawn(async move {
                    run_telegram_daemon_chat_task(
                        transport_clone,
                        chat_id,
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
                ChatRoute { sender: tx, handle }
            });

            if let Err(e) = route.sender.send(msg).await {
                eprintln!(
                    "aivyx-telegram(daemon): chat {chat_id} mailbox send failed ({e}); dropping route"
                );
                routes.remove(&chat_id);
            }
        }
    }

    let drained: Vec<(i64, ChatRoute)> = routes.drain().collect();
    for (_chat_id, ChatRoute { sender, handle }) in drained {
        drop(sender);
        match handle.await {
            Ok(Err(e)) => eprintln!("aivyx-telegram(daemon): inner task error: {e}"),
            Err(e) => eprintln!("aivyx-telegram(daemon): inner task join failed: {e}"),
            Ok(Ok(())) => {}
        }
    }

    Ok(())
}

/// Outcome of [`handle_telegram_team_run_message`] — whether the message
/// was fully handled by the confirm-first/`\/team run` logic (in which
/// case the caller must reply and move on to the next message, never
/// falling through to `gate_command`/`team_command` or a chat turn), or
/// left unhandled (in which case the caller falls through unchanged).
#[derive(Debug, PartialEq, Eq)]
enum TelegramChatOutcome {
    /// Reply immediately with this text; do not forward to the LLM turn
    /// path or the generic `gate_command`/`team_command` dispatch below.
    Reply(String),
    /// Nothing here matched (not a pending confirm resolution, not a
    /// fresh `/team run`) — the caller should fall through to its own
    /// existing `gate_command`/`team_command` dispatch and, ultimately,
    /// `session.submit_input`.
    NotHandled,
}

/// Extracted from `run_telegram_daemon_chat_task` specifically so the
/// ordering invariant (the pending-confirm check and `/team run`
/// recognition MUST be checked before the generic `team_command`
/// dispatch, or `/team run` becomes permanently unreachable dead code —
/// a real bug this exact branch shipped once and had to fix in all
/// three channels, see the final-review report) is directly testable
/// without a live transport or socket. Preserves the original inline
/// control flow exactly: the pending-confirm check runs first and may
/// or may not resolve/return, then (unconditionally, whether or not a
/// pending trigger was just restored) a fresh `/team run` is checked —
/// so a `/team run <new goal>` sent while an old confirm is still
/// pending overwrites it, matching pre-extraction behavior.
#[allow(clippy::too_many_arguments)]
async fn handle_telegram_team_run_message(
    text: &str,
    socket_path: &Path,
    pending_trigger: &mut Option<PendingTrigger<i64>>,
    trigger_history: &mut Vec<Instant>,
    team_run_channel: bool,
    team_trigger_rate_limit: Option<u32>,
    sender_id: i64,
) -> TelegramChatOutcome {
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
                // sender-allowlist check in handle_telegram_incoming_command
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
                return TelegramChatOutcome::Reply(
                    "✗ that request expired, ask again.".to_string(),
                );
            }
            Some(ConfirmReply::Yes) => {
                let reply = match daemon_client::run_team_mission_channel(
                    socket_path,
                    FrontendType::Telegram,
                    pending.goal.clone(),
                )
                .await
                {
                    Ok(mission_id) => format!("✓ Started mission {mission_id}."),
                    Err(e) => format!("✗ Could not start the mission: {e}"),
                };
                return TelegramChatOutcome::Reply(reply);
            }
            Some(ConfirmReply::No) => {
                return TelegramChatOutcome::Reply("Cancelled.".to_string());
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
            return TelegramChatOutcome::Reply(
                "✗ this channel is not authorized to start team missions.".to_string(),
            );
        }
        let allowed = match team_trigger_rate_limit {
            Some(limit) => check_and_record_trigger(trigger_history, limit, Instant::now()),
            None => true,
        };
        if !allowed {
            let limit = team_trigger_rate_limit.unwrap_or(0);
            return TelegramChatOutcome::Reply(format!(
                "✗ too many mission-start requests (max {limit} per hour), \
                 try again later."
            ));
        }
        *pending_trigger = Some(PendingTrigger::new(goal.clone(), sender_id));
        return TelegramChatOutcome::Reply(format!(
            "Start '{goal}' on the default team? Reply yes/no."
        ));
    }

    TelegramChatOutcome::NotHandled
}

/// Outcome of [`handle_telegram_incoming_command`] — whether the loop
/// should reply immediately (native command matched) or forward the
/// message on to the normal chat-turn path.
#[derive(Debug, PartialEq, Eq)]
enum TelegramIncomingOutcome {
    /// Reply with this text; do not forward to the LLM turn path.
    Reply(String),
    /// Nothing matched any native command — forward to the normal
    /// chat-turn path (after the caller's own `gate_command::parse`
    /// check, which never collides with anything handled here).
    ForwardToChatTurn,
}

/// Owns the full `/team run` vs. generic `/team ...` precedence chain for
/// one inbound Telegram message, in the order it must run:
/// [`handle_telegram_team_run_message`] (pending-confirm resolution and
/// fresh `/team run` recognition) FIRST, then `team_command::parse` +
/// `team_dispatch::dispatch`.
///
/// Extracted one level further than `handle_telegram_team_run_message`
/// itself specifically so this ORDERING — not just each step's own
/// internal correctness — is what a test exercises. A prior fix wave
/// extracted `handle_telegram_team_run_message` and added tests against
/// it directly, which genuinely tests its own logic but does NOT prove
/// the call-site order in the real loop: a re-review confirmed (by
/// physically reordering the loop's calls in a throwaway worktree) that
/// those tests kept passing even after reintroducing the original bug,
/// because they never exercise a function that contains both this call
/// and the generic dispatch it must precede. This function is that
/// missing piece — see `team_run_is_recognized_before_the_generic_team_command_dispatch`
/// below, which fails if the two calls inside this function are swapped.
///
/// `gate_command::parse` is deliberately NOT folded in here: it matches
/// bare `/approve`/`/reject`, which can never collide with `/team run`'s
/// `/team`-prefixed syntax or with `team_command::parse`'s own domain, so
/// its position relative to this function is not part of the invariant
/// under test. It stays inline in the loop, checked after this function
/// returns `ForwardToChatTurn`.
#[allow(clippy::too_many_arguments)]
async fn handle_telegram_incoming_command(
    text: &str,
    socket_path: &Path,
    pending_trigger: &mut Option<PendingTrigger<i64>>,
    trigger_history: &mut Vec<Instant>,
    team_run_channel: bool,
    team_trigger_rate_limit: Option<u32>,
    sender_id: i64,
    allowed_senders: &[i64],
) -> TelegramIncomingOutcome {
    // Team-Command Sender Allowlist (2026-08-23) — must run before
    // BOTH handle_telegram_team_run_message (so an unauthorized /team
    // run is denied before it can even stage a confirm-first prompt)
    // and the generic team_command::parse dispatch below. Checked only
    // when the text actually parses as a /team command at all --
    // ordinary chat text from an unauthorized sender is completely
    // unaffected.
    //
    // Note this check alone does NOT protect the later "yes"/"no"
    // confirm-reply step -- a bare "yes" never parses as a /team
    // command, so it never reaches this check at all. That step is
    // separately protected by binding each PendingTrigger to the
    // sender_id that staged it (final-review fix, 2026-08-24; see
    // `handle_telegram_team_run_message`): only the sender who was
    // asked "Start '<goal>' on the default team?" can answer it, so
    // another allowlisted sender in the same chat can't hijack or
    // cancel someone else's staged mission.
    if team_command::parse(text).is_some() && !sender_allowed(allowed_senders, &sender_id) {
        return TelegramIncomingOutcome::Reply(
            "✗ you are not authorized to issue /team commands.".to_string(),
        );
    }

    match handle_telegram_team_run_message(
        text,
        socket_path,
        pending_trigger,
        trigger_history,
        team_run_channel,
        team_trigger_rate_limit,
        sender_id,
    )
    .await
    {
        TelegramChatOutcome::Reply(reply) => return TelegramIncomingOutcome::Reply(reply),
        TelegramChatOutcome::NotHandled => {}
    }

    if let Some(team_cmd) = team_command::parse(text) {
        let reply = team_dispatch::dispatch(socket_path, team_cmd).await;
        return TelegramIncomingOutcome::Reply(reply);
    }

    TelegramIncomingOutcome::ForwardToChatTurn
}

/// Per-chat inner task: connect a `DaemonSession`, submit turns,
/// accumulate streamed events, send one Telegram message per turn.
#[allow(clippy::too_many_arguments)]
async fn run_telegram_daemon_chat_task(
    transport: Arc<ReqwestTransport>,
    chat_id: i64,
    socket_path: PathBuf,
    role: Option<String>,
    mut mailbox: tokio::sync::mpsc::Receiver<IncomingMessage>,
    shutdown: CancellationToken,
    team_run_channel: bool,
    team_trigger_rate_limit: Option<u32>,
    team_command_allowed_senders: Vec<i64>,
) -> Result<(), DaemonError> {
    let mut session = DaemonSession::connect(
        &socket_path,
        role,
        Some(FrontendType::Telegram),
    )
    .await?;

    let mut pending_trigger: Option<PendingTrigger<i64>> = None;
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
        // chain lives in `handle_telegram_incoming_command`, extracted
        // specifically so this ordering (both MUST run in the right order,
        // or `/team run` becomes permanently unreachable dead code) is
        // itself directly testable, not just each piece's own internal
        // correctness. See that function's own doc comment.
        match handle_telegram_incoming_command(
            msg.text.trim(),
            &socket_path,
            &mut pending_trigger,
            &mut trigger_history,
            team_run_channel,
            team_trigger_rate_limit,
            msg.user_id,
            &team_command_allowed_senders,
        )
        .await
        {
            TelegramIncomingOutcome::Reply(text) => {
                transport
                    .send_message(OutgoingMessage { chat_id, text })
                    .await
                    .map_err(|e| {
                        DaemonError::Internal(format!("send_message to chat {chat_id}: {e}"))
                    })?;
                continue;
            }
            TelegramIncomingOutcome::ForwardToChatTurn => {}
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
                .send_message(OutgoingMessage { chat_id, text: reply })
                .await
                .map_err(|e| DaemonError::Internal(format!(
                    "send_message to chat {chat_id}: {e}"
                )))?;
            continue;
        }

        // Note: the generic `/team ...` dispatch is now folded into
        // `handle_telegram_incoming_command` above (it must run after
        // `/team run` recognition within that single function for the
        // ordering invariant to be testable) — nothing else to do here.

        // Phase 45 — forward image data through IPC when present.
        let (events, outcome) = if let Some(ref img) = msg.image {
            use base64::Engine;
            let encoder = base64::engine::general_purpose::STANDARD;
            let att = crate::daemon_ipc::IpcAttachment {
                media_type: img.media_type.clone(),
                data_base64: encoder.encode(&img.data),
                filename: None,
            };
            session
                .submit_input_with_attachments(msg.text, vec![att])
                .await?
        } else {
            session.submit_input(msg.text).await?
        };

        let buf = build_telegram_reply(&events, &outcome);

        transport
            .send_message(OutgoingMessage {
                chat_id,
                text: buf,
            })
            .await
            .map_err(|e| DaemonError::Internal(format!(
                "send_message to chat {chat_id}: {e}"
            )))?;
    }

    let _ = session.disconnect().await;
    Ok(())
}

/// Render accumulated `StreamEventPayload` events into a single
/// Telegram message. Matches `TelegramChannel`'s `append_event`
/// rendering style (tool arrows, status markers) rather than the
/// CLI's `render_for_cli`.
fn render_events_for_telegram(events: &[StreamEventPayload]) -> String {
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

/// Build the outbound Telegram reply text: the rendered event journal
/// (tool-call/status lines + streamed text), plus a correction line
/// when the turn's own outcome diverges from what the events alone
/// would show (a reply floor, a Candor/identifier-fidelity annotation,
/// or a non-completed outcome's reason). Skips the correction only
/// when it would exactly duplicate `render_events_for_telegram`'s own
/// empty-events "(no reply)" fallback.
fn build_telegram_reply(events: &[StreamEventPayload], outcome: &str) -> String {
    let displayed = crate::daemon_ipc::concat_text_events(events);
    let mut buf = render_events_for_telegram(events);
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

// `parse_gate_command` lived here through Phases 19–110. Phase 111
// extracted it into `crate::gate_command` so the Discord and Slack
// daemon-frontends could share the parser. Three-data-point
// reuse — see Phase 111 PHASE_111.md Q-block, Q2a sign-off.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon_ipc::StreamEventPayload;

    #[test]
    fn approval_gate_renders_with_reply_hint() {
        let events = vec![StreamEventPayload::ApprovalGate {
            mission_id: "m-001".into(),
            gate_id: "g-abc".into(),
            reason: "deploy?".into(),
            scope: None,
        }];
        let rendered = render_events_for_telegram(&events);
        assert!(rendered.contains("APPROVAL GATE"));
        assert!(rendered.contains("/approve m-001 g-abc"));
        assert!(rendered.contains("/reject  m-001 g-abc"));
    }

    #[test]
    fn build_telegram_reply_appends_correction_when_outcome_differs() {
        let events = vec![StreamEventPayload::Text {
            text: "{\"path\": \"airports.csv\"}".into(),
        }];
        let out = build_telegram_reply(
            &events,
            "completed: I wasn't able to produce a usable reply this turn — please try again.",
        );
        assert!(out.contains("{\"path\": \"airports.csv\"}"), "{out}");
        assert!(out.contains("corrected"), "{out}");
    }

    #[test]
    fn build_telegram_reply_no_correction_when_outcome_matches() {
        let events = vec![StreamEventPayload::Text {
            text: "an answer".into(),
        }];
        let out = build_telegram_reply(&events, "completed: an answer");
        assert_eq!(out, "an answer");
    }

    #[test]
    fn build_telegram_reply_does_not_double_up_no_reply() {
        let out = build_telegram_reply(&[], "completed: ");
        assert_eq!(out, "(no reply)", "must not print (no reply) twice: {out}");
    }

    #[test]
    fn build_telegram_reply_surfaces_non_completed_outcome() {
        let out = build_telegram_reply(&[], "timed out");
        assert!(out.contains("timed out"), "{out}");
    }

    // Audit C1+H1 regression — see DESIGN.md D1 turn-loop
    // contract and the Agent Loop audit notes. The stub
    // must (a) fire its token on `cancel_inflight` so the
    // daemon's CancelTurn handler actually cancels the
    // in-flight turn, and (b) rotate its token on
    // `reset_cancellation` so a prior cancel does not
    // pre-cancel turn N+1.

    #[test]
    fn cancel_inflight_cancels_the_current_token() {
        let ch = TelegramDaemonChannel::new(false);
        let token = ch.cancellation_token();
        assert!(!token.is_cancelled(), "fresh token must not be cancelled");
        ch.cancel_inflight();
        assert!(
            token.is_cancelled(),
            "after cancel_inflight, the snapshot token must report cancelled"
        );
    }

    #[test]
    fn reset_cancellation_installs_a_fresh_token() {
        let ch = TelegramDaemonChannel::new(false);
        ch.cancel_inflight();
        let stale = ch.cancellation_token();
        assert!(stale.is_cancelled(), "post-cancel token is cancelled");
        ch.reset_cancellation();
        let fresh = ch.cancellation_token();
        assert!(
            !fresh.is_cancelled(),
            "after reset_cancellation the new token must be un-cancelled"
        );
        // The old snapshot stays cancelled (monotonic), confirming
        // that reset swapped in a separate token rather than
        // un-cancelling the existing one.
        assert!(
            stale.is_cancelled(),
            "the pre-reset token must remain cancelled"
        );
    }

    // --- Task 10 fix round 2 (2026-09-16) — the daemon-first-path gap.
    // The stub used to hardcode `SemiTrusted` unconditionally, bypassing
    // the per-sender authorization fix entirely for every daemon-routed
    // session (the default runtime mode). These tests lock in that the
    // stub's own trust tier now actually depends on whether an
    // allowlist is configured, mirroring the in-process
    // `TelegramChannel::trust_tier()` semantics for messages that
    // reach a session at all (see `telegram_chat_is_allowed` below for
    // the routing-level half of this fix).

    #[test]
    fn stub_reports_untrusted_when_no_allowlist_configured() {
        let ch = TelegramDaemonChannel::new(false);
        assert_eq!(
            ch.trust_tier(),
            aivyx_capability::TrustTier::Untrusted,
            "no chat_filter configured must mean every reachable sender is Untrusted"
        );
    }

    #[test]
    fn stub_reports_semitrusted_when_allowlist_configured() {
        let ch = TelegramDaemonChannel::new(true);
        assert_eq!(
            ch.trust_tier(),
            aivyx_capability::TrustTier::SemiTrusted,
            "a configured chat_filter means messages reaching this stub already matched it"
        );
    }

    // --- Routing-level drop-filter (the other half of Task 10 fix
    // round 2). `telegram_chat_is_allowed` is the exact predicate used
    // in `run_telegram_daemon_multi_session`'s loop before a message is
    // ever routed to a per-chat session — proving a non-matching
    // sender's message never reaches a session at all when a filter is
    // configured, without needing a live transport/socket harness.

    #[test]
    fn chat_filter_none_allows_any_chat() {
        assert!(telegram_chat_is_allowed(111, None));
        assert!(telegram_chat_is_allowed(999, None));
    }

    #[test]
    fn chat_filter_some_allows_only_the_matching_chat() {
        assert!(telegram_chat_is_allowed(111, Some(111)));
        assert!(
            !telegram_chat_is_allowed(999, Some(111)),
            "a non-matching chat must be dropped before reaching a session"
        );
    }

    // --- I5 (final-review) — the ordering invariant that keeps
    // `/team run` from being permanently unreachable, locked in by
    // testing `handle_telegram_team_run_message` directly.

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixListener;

    /// A fake daemon that does the `StartSession` handshake then
    /// replies `TeamMissionChannelStarted` — mirrors
    /// `daemon_client.rs`'s own
    /// `run_team_mission_channel_does_the_start_session_handshake_then_sends_the_request`
    /// fixture, since `handle_telegram_team_run_message`'s "yes" path
    /// calls the real `daemon_client::run_team_mission_channel`.
    async fn fake_daemon_starting_mission(
        mission_id: &str,
    ) -> (PathBuf, tokio::task::JoinHandle<()>) {
        use crate::daemon_ipc::{encode_frame, DaemonEnvelope};

        let sock = std::env::temp_dir().join(format!(
            "aivyx-telegram-teamrun-{}.sock",
            uuid::Uuid::new_v4()
        ));
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
        let mut pending: Option<PendingTrigger<i64>> = None;
        let mut history: Vec<Instant> = Vec::new();

        let outcome = handle_telegram_team_run_message(
            "/team run close the books",
            Path::new("/nonexistent/unused.sock"),
            &mut pending,
            &mut history,
            true,
            None,
            123,
        )
        .await;

        match outcome {
            TelegramChatOutcome::Reply(text) => {
                assert!(
                    !text.contains("should never be dispatched directly"),
                    "must not fall through to the generic team_dispatch stub: {text}"
                );
                assert!(
                    text.contains("Reply yes/no"),
                    "must prompt for confirmation: {text}"
                );
            }
            TelegramChatOutcome::NotHandled => panic!("expected a Reply, got NotHandled"),
        }
        assert!(pending.is_some(), "a pending trigger must now be recorded");
    }

    #[tokio::test]
    async fn team_run_denied_when_channel_not_opted_in() {
        let mut pending: Option<PendingTrigger<i64>> = None;
        let mut history: Vec<Instant> = Vec::new();

        let outcome = handle_telegram_team_run_message(
            "/team run close the books",
            Path::new("/nonexistent/unused.sock"),
            &mut pending,
            &mut history,
            false,
            None,
            123,
        )
        .await;

        match outcome {
            TelegramChatOutcome::Reply(text) => {
                assert!(text.contains("not authorized"), "got: {text}");
            }
            TelegramChatOutcome::NotHandled => panic!("expected a Reply, got NotHandled"),
        }
        assert!(pending.is_none(), "an unauthorized attempt must not arm a pending trigger");
    }

    #[tokio::test]
    async fn ordinary_text_with_no_pending_trigger_is_not_handled() {
        let mut pending: Option<PendingTrigger<i64>> = None;
        let mut history: Vec<Instant> = Vec::new();

        let outcome = handle_telegram_team_run_message(
            "just chatting, nothing special",
            Path::new("/nonexistent/unused.sock"),
            &mut pending,
            &mut history,
            true,
            None,
            123,
        )
        .await;

        assert_eq!(outcome, TelegramChatOutcome::NotHandled);
        assert!(pending.is_none());
    }

    #[tokio::test]
    async fn pending_trigger_plus_yes_starts_the_mission_via_the_real_daemon_client_call() {
        let (sock, server) = fake_daemon_starting_mission("m-42").await;

        let mut pending = Some(PendingTrigger::new("close the books", 123));
        let mut history: Vec<Instant> = Vec::new();

        let outcome = handle_telegram_team_run_message(
            "yes", &sock, &mut pending, &mut history, true, None, 123,
        )
        .await;

        match outcome {
            TelegramChatOutcome::Reply(text) => {
                assert!(text.contains("Started mission"), "got: {text}");
                assert!(text.contains("m-42"));
            }
            TelegramChatOutcome::NotHandled => panic!("expected a Reply, got NotHandled"),
        }
        assert!(pending.is_none(), "the pending trigger is consumed on yes");

        let _ = server.await;
        let _ = std::fs::remove_file(&sock);
    }

    #[tokio::test]
    async fn pending_trigger_plus_no_cancels() {
        let mut pending = Some(PendingTrigger::new("close the books", 123));
        let mut history: Vec<Instant> = Vec::new();

        let outcome = handle_telegram_team_run_message(
            "no",
            Path::new("/nonexistent/unused.sock"),
            &mut pending,
            &mut history,
            true,
            None,
            123,
        )
        .await;

        match outcome {
            TelegramChatOutcome::Reply(text) => {
                assert!(text.contains("Cancelled."), "got: {text}");
            }
            TelegramChatOutcome::NotHandled => panic!("expected a Reply, got NotHandled"),
        }
        assert!(pending.is_none(), "a 'no' reply clears the pending trigger");
    }

    // --- Re-review fix — the genuine ordering-lock test. The 15 tests
    // above (and their Discord/Slack siblings) only ever call
    // `handle_telegram_team_run_message` directly, so they lock in that
    // function's own internal correctness but never the real loop's
    // call-site order relative to `team_command::parse`. Proven
    // empirically: in a throwaway worktree, moving
    // `handle_telegram_team_run_message`'s call site in
    // `run_telegram_daemon_chat_task` to AFTER the generic
    // `team_command::parse` dispatch (reintroducing the original bug)
    // left all 1270 tests passing. This test calls
    // `handle_telegram_incoming_command` — the function that now owns
    // BOTH steps in one place — so a regression in their relative order
    // fails here directly. See the mutation-proof in the final-review
    // fix report for this exact test failing/passing before/after a
    // real reorder of this function's own body.

    #[tokio::test]
    async fn team_run_is_recognized_before_the_generic_team_command_dispatch() {
        // This is the test that actually locks in the ordering bug this
        // whole thing exists to guard against. If /team run's own
        // recognition ever moves to AFTER the generic team_command::parse
        // dispatch again, this test must fail, not just the tests on the
        // extracted piece in isolation.
        let mut pending_trigger = None;
        let mut trigger_history = Vec::new();
        let outcome = handle_telegram_incoming_command(
            "/team run close the books",
            std::path::Path::new("/nonexistent/unused.sock"),
            &mut pending_trigger,
            &mut trigger_history,
            true,  // team_run_channel
            None,  // team_trigger_rate_limit
            123,   // sender_id -- IS in the allowlist
            &[123i64, 456],
        )
        .await;
        match outcome {
            TelegramIncomingOutcome::Reply(text) => {
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
            TelegramIncomingOutcome::ForwardToChatTurn => {
                panic!("/team run was not recognized at all")
            }
        }
        assert!(pending_trigger.is_some(), "a pending trigger should now be set");
    }

    // --- Sender Allowlist Task 3 --------------------------------------

    #[tokio::test]
    async fn unauthorized_sender_is_denied_before_team_run_recognition() {
        // This is the test that proves the ordering: the sender check must
        // run BEFORE handle_telegram_team_run_message's own call, or an
        // unauthorized sender's /team run would still reach the confirm-
        // first flow (team_run_channel: true below would otherwise let it
        // succeed). Piece C's own analogous ordering test needed two review
        // rounds because an earlier version only proved a narrower helper's
        // internals, never the real call-site order — this test is written
        // to avoid that exact failure mode by asserting on the SPECIFIC
        // confirm-prompt text that would appear if the check were bypassed.
        let mut pending_trigger = None;
        let mut trigger_history = Vec::new();
        let outcome = handle_telegram_incoming_command(
            "/team run close the books",
            std::path::Path::new("/nonexistent/unused.sock"),
            &mut pending_trigger,
            &mut trigger_history,
            true, // team_run_channel -- would otherwise let this succeed
            None, // team_trigger_rate_limit
            999,  // sender_id -- NOT in the allowlist
            &[123i64, 456],
        )
        .await;
        match outcome {
            TelegramIncomingOutcome::Reply(text) => {
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
            TelegramIncomingOutcome::ForwardToChatTurn => {
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
        // Confirms the check gates the WHOLE /team surface, not just Run --
        // /team status is a Piece B command with no team_run_channel
        // involvement at all.
        let mut pending_trigger = None;
        let mut trigger_history = Vec::new();
        let outcome = handle_telegram_incoming_command(
            "/team status",
            std::path::Path::new("/nonexistent/unused.sock"),
            &mut pending_trigger,
            &mut trigger_history,
            false,
            None,
            999,
            &[123i64, 456],
        )
        .await;
        match outcome {
            TelegramIncomingOutcome::Reply(text) => {
                assert!(text.contains("not authorized to issue /team commands"));
            }
            TelegramIncomingOutcome::ForwardToChatTurn => {
                panic!("expected a denial reply, not a forward to chat turn")
            }
        }
    }

    #[tokio::test]
    async fn authorized_sender_reaches_dispatch_not_the_denial() {
        // Confirms the check doesn't false-positive-deny a legitimate
        // sender. socket_path points nowhere, so team_dispatch::dispatch
        // itself will fail (daemon unreachable) -- the point is the reply
        // is THAT failure, not the sender-denial message, proving the
        // authorized sender got past this check and reached real dispatch.
        let mut pending_trigger = None;
        let mut trigger_history = Vec::new();
        let outcome = handle_telegram_incoming_command(
            "/team status",
            std::path::Path::new("/nonexistent/unused.sock"),
            &mut pending_trigger,
            &mut trigger_history,
            false,
            None,
            123, // sender_id -- IS in the allowlist
            &[123i64, 456],
        )
        .await;
        match outcome {
            TelegramIncomingOutcome::Reply(text) => {
                assert!(!text.contains("not authorized to issue /team commands"));
            }
            TelegramIncomingOutcome::ForwardToChatTurn => {
                panic!("expected a Reply (dispatch attempted), not ForwardToChatTurn")
            }
        }
    }

    #[tokio::test]
    async fn non_team_text_is_unaffected_regardless_of_sender() {
        let mut pending_trigger = None;
        let mut trigger_history = Vec::new();
        let outcome = handle_telegram_incoming_command(
            "hello, just chatting",
            std::path::Path::new("/nonexistent/unused.sock"),
            &mut pending_trigger,
            &mut trigger_history,
            false,
            None,
            999, // unauthorized sender -- must not matter here
            &[123i64, 456],
        )
        .await;
        assert_eq!(outcome, TelegramIncomingOutcome::ForwardToChatTurn);
    }

    // --- Final-review Finding 1 (2026-08-24) — the confirm-reply
    // sender-binding fix. Before this fix, `PendingTrigger` carried no
    // sender identity, so ANY allowlisted sender in the same chat could
    // resolve (start or cancel) a trigger staged by someone else --
    // because a bare "yes"/"no" never parses as a /team command, the
    // main sender-allowlist check above never even sees it. These tests
    // go through `handle_telegram_incoming_command`, the real entry
    // point, so they exercise the exact call path a live message takes.

    #[tokio::test]
    async fn wrong_sender_yes_does_not_resolve_someone_elses_pending_trigger() {
        // Sender 111 staged the trigger; sender 222 is ALSO allowlisted
        // (so the main allowlist check would never deny them) but did
        // NOT stage this particular trigger.
        let mut pending_trigger = Some(PendingTrigger::new("close the books", 111i64));
        let mut trigger_history = Vec::new();

        let outcome = handle_telegram_incoming_command(
            "yes",
            std::path::Path::new("/nonexistent/unused.sock"),
            &mut pending_trigger,
            &mut trigger_history,
            true,
            None,
            222i64, // sender B -- allowlisted, but not the staging sender
            &[111i64, 222i64],
        )
        .await;

        assert_eq!(
            outcome,
            TelegramIncomingOutcome::ForwardToChatTurn,
            "a wrong-sender 'yes' must fall through like ordinary chat text, \
             not resolve the trigger and not produce a special denial reply"
        );
        assert!(
            pending_trigger.is_some(),
            "sender A's pending trigger must survive an unrelated sender's 'yes'"
        );
        assert_eq!(
            pending_trigger.as_ref().unwrap().sender_id,
            111i64,
            "the original staging sender's trigger must be untouched"
        );
    }

    #[tokio::test]
    async fn correct_sender_yes_resolves_their_own_pending_trigger() {
        let (sock, server) = fake_daemon_starting_mission("m-77").await;

        let mut pending_trigger = Some(PendingTrigger::new("close the books", 111i64));
        let mut trigger_history = Vec::new();

        let outcome = handle_telegram_incoming_command(
            "yes",
            &sock,
            &mut pending_trigger,
            &mut trigger_history,
            true,
            None,
            111i64, // sender A -- the one who staged it
            &[111i64, 222i64],
        )
        .await;

        match outcome {
            TelegramIncomingOutcome::Reply(text) => {
                assert!(text.contains("Started mission"), "got: {text}");
                assert!(text.contains("m-77"));
            }
            TelegramIncomingOutcome::ForwardToChatTurn => {
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
