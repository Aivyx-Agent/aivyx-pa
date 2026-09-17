//! [`TelegramChannel`] — the `ChannelContext` impl proper.
//!
//! ## Streaming model
//!
//! Unlike `LocalChannel`, which flushes every text chunk as a syscall
//! so tokens appear in the user's terminal as they arrive, Telegram
//! has no "partial message" UX: the Bot API sends whole messages, and
//! trying to stream via `editMessageText` costs a rate-limited API
//! call per chunk and looks jittery on clients. So the strategy is:
//!
//! - `stream_event(Text)` — **append to an owned `String` buffer.**
//!   The incoming `&str` is borrowed from the turn-loop's per-chunk
//!   allocation; we copy it because the borrow dies the moment this
//!   async call returns.
//! - `stream_event(ToolCallStarted / ToolCallFinished / Status)` —
//!   append a rendered marker line to the same buffer. The user sees
//!   the tool call as in-message text rather than as a separate
//!   Telegram message, which avoids the "one tool = one chat bubble"
//!   spam.
//! - `finalize(outcome)` — drain the buffer, append the finalize
//!   marker, and call `send_message` **once** with the whole thing.
//!   One turn = one Telegram message.
//!
//! ## Per-turn cancellation
//!
//! Matches `LocalChannel`'s pattern exactly: an `Arc<Mutex<CancellationToken>>`
//! slot so the listen loop can rotate the token between turns. The
//! Phase 3 "monotonic token poisons turn N+1" bug applies equally to
//! a network channel — if a user sends `/cancel` during turn N (the
//! Phase 8 task 5 open question), the next turn must see a fresh
//! token, not the already-cancelled one.
//!
//! ## Buffer mutation under `&self`
//!
//! `ChannelContext::stream_event` takes `&self`, which means the text
//! buffer must live behind a mutex. We use `std::sync::Mutex<String>`
//! (not `tokio::sync::Mutex`) because every critical section is a
//! synchronous `push_str` — never held across an await — and the
//! `std` mutex is cheaper for contention-free workloads. This matches
//! the `LocalChannel::writer` pattern exactly.

use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use aivyx_capability::TrustTier;
use aivyx_core::{
    CancellationToken, ChannelContext, ChannelError, ChannelPlatform, SessionId, StreamEvent,
    TurnOutcome,
};

use crate::transport::{OutgoingMessage, TelegramTransport, TransportError};

/// A Telegram `ChannelContext`. Generic over the transport so tests
/// can inject a scripted double without running HTTP. The concrete
/// production type is `TelegramChannel<ReqwestTransport>`.
pub struct TelegramChannel<T: TelegramTransport + 'static> {
    name: String,
    session: SessionId,
    /// The Telegram chat this channel is bound to. Phase 8 Task 1
    /// ships a **one-chat-per-channel** model — the multi-chat story
    /// (one store, many `TelegramChannel` instances keyed by chat_id)
    /// is Task 2's problem. Fixing one chat per channel here keeps
    /// Task 1 a pure adapter-pattern exercise.
    chat_id: i64,
    /// Security-audit fix (Task 10, 2026-09-16) — the operator's
    /// configured `chat_filter` (see `aivyx_config::TelegramConfig`),
    /// carried onto the channel so `trust_tier()` can tell an actually
    /// allowlisted chat apart from one that merely happened to reach
    /// this constructor. `None` = no chat allowlisted (per
    /// `THREAT_MODEL.md`, that means `Untrusted`, not `SemiTrusted` —
    /// see the `trust_tier()` doc comment below for why).
    chat_filter: Option<i64>,
    /// Per-turn cancellation slot. See the module doc for why we
    /// rotate this between turns.
    token: Arc<Mutex<CancellationToken>>,
    /// Accumulated turn output. Cleared on each `finalize()`.
    buffer: Mutex<String>,
    /// The swappable transport — either `ReqwestTransport` in prod or
    /// a scripted double in tests.
    transport: Arc<T>,
}

impl<T: TelegramTransport + 'static> TelegramChannel<T> {
    /// Construct a `TelegramChannel` bound to a single chat.
    ///
    /// `chat_filter` is the operator's configured allowlist value
    /// (`aivyx_config::TelegramConfig::chat_filter` / the
    /// `chat_filter` parameter threaded through
    /// `run_telegram_multi_session`), not necessarily equal to
    /// `chat_id` — see `trust_tier()`.
    // Task 1 only exercises this from the tests module; Task 4's
    // binary wiring will call it for real. The allow is scoped to
    // the constructor so the rest of the impl still gets dead-code
    // checking on any accidentally-orphaned helpers.
    #[allow(dead_code)]
    pub(crate) fn new(
        name: impl Into<String>,
        chat_id: i64,
        chat_filter: Option<i64>,
        transport: Arc<T>,
    ) -> Self {
        TelegramChannel {
            name: name.into(),
            session: SessionId::new(),
            chat_id,
            chat_filter,
            token: Arc::new(Mutex::new(CancellationToken::new())),
            buffer: Mutex::new(String::new()),
            transport,
        }
    }

    /// The chat this channel is bound to. Exposed for the listen loop
    /// (which reads it to route inbound updates) and for tests (which
    /// assert the channel remembers what it was given).
    #[allow(dead_code)] // consumed by `listen()` in task 4 wiring
    pub(crate) fn chat_id(&self) -> i64 {
        self.chat_id
    }

    /// Shared handle to the transport, so the listen loop can call
    /// `get_updates` with the same object the channel uses for
    /// `send_message`. Pub(crate) because downstream crates should
    /// not see the private transport trait.
    #[allow(dead_code)]
    pub(crate) fn transport(&self) -> Arc<T> {
        Arc::clone(&self.transport)
    }

    /// Rotate the per-turn cancellation token. Call between turns;
    /// see `LocalChannel::reset_cancellation` for the same pattern.
    #[allow(dead_code)] // consumed by task 4 binary wiring + exercised in tests
    pub(crate) fn reset_cancellation(&self) {
        let mut slot = self.token.lock().expect("token mutex poisoned");
        *slot = CancellationToken::new();
    }

    /// Snapshot the current buffer contents without clearing. Test-
    /// only — the production path is `finalize()`, which drains-and-
    /// sends atomically.
    #[cfg(test)]
    pub(crate) fn buffer_snapshot(&self) -> String {
        self.buffer.lock().expect("buffer mutex poisoned").clone()
    }
}

/// Render one `StreamEvent` into the turn's text buffer. Extracted
/// from the trait impl so tests can assert the exact rendering
/// without having to go through the full `ChannelContext` surface.
fn append_event(buffer: &mut String, event: &StreamEvent<'_>) {
    match event {
        StreamEvent::Text(chunk) => {
            buffer.push_str(chunk);
        }
        StreamEvent::Status(s) => {
            // Status markers go on their own line so they don't
            // concatenate into adjacent LLM text. The `…` prefix
            // matches the "in progress" visual convention.
            if !buffer.is_empty() && !buffer.ends_with('\n') {
                buffer.push('\n');
            }
            buffer.push_str("… ");
            buffer.push_str(s);
            buffer.push('\n');
        }
        StreamEvent::ToolCallStarted { tool_name, .. } => {
            if !buffer.is_empty() && !buffer.ends_with('\n') {
                buffer.push('\n');
            }
            // Phase 10 task 3: render the human tool name instead
            // of a truncated UUID. The `tool` ToolId is still on
            // the event for audit use, but a chat user reading
            // `→ memory.read` understands it immediately where
            // `→ tool[a1b2c3d4]` meant nothing.
            let _ = writeln!(buffer, "→ {tool_name}");
        }
        StreamEvent::ToolCallFinished {
            tool_name,
            outcome_summary,
            ..
        } => {
            if !buffer.is_empty() && !buffer.ends_with('\n') {
                buffer.push('\n');
            }
            let _ = writeln!(buffer, "← {tool_name} {outcome_summary}");
        }
        StreamEvent::Attachment { .. } => {
            // Phase 8's non-goals list defers rich media. Silently
            // drop attachment events at the channel boundary rather
            // than erroring — an agent that emits an attachment into
            // a Telegram channel should degrade gracefully, not
            // crash the turn.
        }
        StreamEvent::ToolOutput { .. } => {
            // Phase 12 task 1 trust-tier asymmetry: `SemiTrusted`
            // adapters get the same finish-time summary they had
            // before Phase 12. Per-chunk rendering would need a
            // per-tool-call accumulator on the channel, which is
            // more surface than the Phase 11 asymmetry pattern
            // justifies — Local gets the richer streamed UX, and
            // Telegram sees `ToolOutput` as a no-op and renders
            // the aggregated result at `ToolCallFinished`. Users
            // on Telegram see exactly what they saw in Phase 11.
        }
    }
}

/// Render a `TurnOutcome` into a short footer line. Kept minimal so
/// the user's reply isn't dominated by status noise.
fn finalize_footer(outcome: &TurnOutcome) -> String {
    match outcome {
        TurnOutcome::Completed { .. } => String::new(),
        TurnOutcome::Escalated { reason, .. } => format!("\n⏸ escalation: {reason}"),
        TurnOutcome::TimedOut { elapsed, .. } => {
            format!("\n⏱ timed out after {elapsed:?}")
        }
        TurnOutcome::Cancelled { .. } => "\n✕ cancelled".to_string(),
        TurnOutcome::MaxStepsExceeded { max_steps, .. } => {
            format!("\n✕ planner exceeded {max_steps} steps per turn")
        }
        TurnOutcome::Looping { repeat_limit, .. } => {
            format!("\n✕ stopped after {repeat_limit} repeated identical tool calls")
        }
        TurnOutcome::Failed(e) => format!("\n✕ failed: {e}"),
    }
}

impl From<TransportError> for ChannelError {
    fn from(e: TransportError) -> Self {
        match e {
            TransportError::Platform(msg) => ChannelError::Platform(msg),
        }
    }
}

#[async_trait]
impl<T: TelegramTransport + 'static> ChannelContext for TelegramChannel<T> {
    fn channel_name(&self) -> &str {
        &self.name
    }

    fn platform(&self) -> ChannelPlatform {
        ChannelPlatform::Telegram
    }

    fn trust_tier(&self) -> TrustTier {
        // Security-audit fix (Task 10, 2026-09-16). `THREAT_MODEL.md`
        // §2 defines `SemiTrusted` as "the operator over a remote,
        // authenticated channel ... their own Telegram bot, with
        // chat-id allowlisted" and `Untrusted` as "anyone the operator
        // has not authenticated ... unallowlisted senders." Prior to
        // this fix this method ignored `chat_filter` entirely and
        // always returned `SemiTrusted` — including when the operator
        // had configured no filter at all (`chat_filter: None`, the
        // out-of-the-box default), which silently trusted *any* chat
        // that found the bot. Now: `SemiTrusted` only when the
        // operator's configured `chat_filter` names this exact chat;
        // `Untrusted` otherwise, `None` included. This is a real
        // behavior change for operators with no filter configured —
        // see `docs/INSTALL.md`'s `chat_filter` note.
        match self.chat_filter {
            Some(allowed) if allowed == self.chat_id => TrustTier::SemiTrusted,
            _ => TrustTier::Untrusted,
        }
    }

    fn session_id(&self) -> SessionId {
        self.session
    }

    fn session_partition(&self) -> Option<String> {
        // Phase 8 Task 2 — one Telegram chat = one memory partition.
        // The stringified `chat_id` is the stable, Telegram-assigned
        // identity the turn loop uses to namespace session-scoped
        // tool state. Two `TelegramChannel` instances sharing the
        // same `RedbMemory` and different `chat_id`s cannot see each
        // other's memory; see `tests/two_chats_isolated.rs`.
        Some(self.chat_id.to_string())
    }

    async fn stream_event(&self, event: StreamEvent<'_>) -> Result<(), ChannelError> {
        let mut buf = self
            .buffer
            .lock()
            .map_err(|e| ChannelError::Send(format!("TelegramChannel buffer poisoned: {e}")))?;
        append_event(&mut buf, &event);
        Ok(())
    }

    async fn finalize(&self, outcome: &TurnOutcome) -> Result<(), ChannelError> {
        // Drain the buffer under the lock, release the lock *before*
        // the network call. Holding `std::sync::Mutex` across an
        // await would be a correctness bug (not just style) because
        // tokio's scheduler does not understand std mutexes.
        let payload = {
            let mut buf = self
                .buffer
                .lock()
                .map_err(|e| ChannelError::Send(format!("TelegramChannel buffer poisoned: {e}")))?;
            let mut out = std::mem::take(&mut *buf);
            out.push_str(&finalize_footer(outcome));
            out
        };

        // A turn with zero streamed text + a Completed outcome (e.g.
        // a tool-only turn that the LLM finished without speaking)
        // would produce an empty payload. Telegram rejects empty
        // `sendMessage`, so we replace it with a minimal "(no reply)"
        // rather than silently dropping or erroring.
        let payload = if payload.trim().is_empty() {
            "(no reply)".to_string()
        } else {
            payload
        };

        self.transport
            .send_message(OutgoingMessage {
                chat_id: self.chat_id,
                text: payload,
            })
            .await?;

        Ok(())
    }

    fn cancellation_token(&self) -> CancellationToken {
        self.token.lock().expect("token mutex poisoned").clone()
    }
}
