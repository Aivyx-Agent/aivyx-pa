//! [`DiscordChannel`] — the `ChannelContext` impl proper.
//!
//! Mirrors `aivyx-telegram::TelegramChannel` very closely.
//! Discord and Telegram have the same conversational shape from
//! the agent's point of view: bot account, authenticated user on
//! the other side, whole-message delivery (not partial-token
//! streaming), and a stable per-conversation id (Discord
//! `channel_id`, Telegram `chat_id`) that the multi-tenant
//! session partitioning hangs off.
//!
//! ## Streaming model
//!
//! Discord's REST `create_message` is whole-message, like
//! Telegram's `sendMessage`. We accumulate every
//! `stream_event(Text)` chunk plus rendered tool-call markers
//! into an owned `String` buffer, then `finalize(outcome)`
//! drains the buffer and issues a single
//! `transport.send_message(...)` call. One turn = one Discord
//! message.
//!
//! ## Per-turn cancellation
//!
//! Same `Arc<Mutex<CancellationToken>>` slot Telegram uses. The
//! session listen loop rotates the token between turns
//! ([`Self::reset_cancellation`]) so a `/cancel` mid-turn N does
//! not poison turn N+1.
//!
//! ## Buffer mutation under `&self`
//!
//! `ChannelContext::stream_event` takes `&self`, so the text
//! buffer lives behind a `std::sync::Mutex<String>` — not the
//! tokio variant, because every critical section is a synchronous
//! `push_str` never held across an await. Matches the
//! `TelegramChannel::buffer` choice exactly.

use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use aivyx_capability::TrustTier;
use aivyx_core::{
    CancellationToken, ChannelContext, ChannelError, ChannelPlatform, SessionId, StreamEvent,
    TurnOutcome,
};

use crate::transport::{DiscordTransport, OutgoingMessage, TransportError};

/// A Discord `ChannelContext`. Generic over the transport so
/// tests can inject `ScriptedTransport` without standing up a
/// gateway. Production type is `DiscordChannel<TwilightTransport>`.
pub struct DiscordChannel<T: DiscordTransport + 'static> {
    name: String,
    session: SessionId,
    /// The Discord channel this `DiscordChannel` is bound to.
    /// Discord uses one `channel_id` for both DMs and guild
    /// text channels — the snowflake is the stable identity
    /// for memory partitioning, the same way `chat_id` is
    /// for Telegram.
    channel_id: u64,
    /// Security-audit fix (Task 10, 2026-09-16) — the operator's
    /// configured `channel_filter` (see
    /// `aivyx_config::DiscordConfig::channel_filter`), mirroring
    /// Telegram's `chat_filter`. `None` = no channel allowlisted,
    /// which per `THREAT_MODEL.md` means `Untrusted`, not
    /// `SemiTrusted` — see `trust_tier()` below.
    channel_filter: Option<u64>,
    /// Per-turn cancellation slot. Rotated between turns.
    token: Arc<Mutex<CancellationToken>>,
    /// Accumulated turn output. Drained on each `finalize()`.
    buffer: Mutex<String>,
    /// The swappable transport. `Arc` so the session loop can
    /// share the same instance the channel writes through.
    transport: Arc<T>,
}

impl<T: DiscordTransport + 'static> DiscordChannel<T> {
    /// Construct a `DiscordChannel` bound to a single channel id.
    ///
    /// `channel_filter` is the operator's configured allowlist value
    /// (`aivyx_config::DiscordConfig::channel_filter`), not
    /// necessarily equal to `channel_id` — see `trust_tier()`.
    ///
    /// Task 3 calls this from the transport's tests; Task 5
    /// binary wiring will call it for real per-channel-id
    /// partition. The dead-code allow is scoped narrowly because
    /// Task 4 ships the impl ahead of the consumer.
    #[allow(dead_code)]
    pub(crate) fn new(
        name: impl Into<String>,
        channel_id: u64,
        channel_filter: Option<u64>,
        transport: Arc<T>,
    ) -> Self {
        DiscordChannel {
            name: name.into(),
            session: SessionId::new(),
            channel_id,
            channel_filter,
            token: Arc::new(Mutex::new(CancellationToken::new())),
            buffer: Mutex::new(String::new()),
            transport,
        }
    }

    /// The Discord channel id this channel is bound to. Exposed
    /// for the listen loop (which routes inbound `MessageCreate`
    /// events by `channel_id`) and for tests asserting the channel
    /// remembers what it was given.
    #[allow(dead_code)]
    pub(crate) fn channel_id(&self) -> u64 {
        self.channel_id
    }

    /// Shared handle to the transport, so the listen loop can
    /// call `next_message` and `send_message` on the same
    /// instance the channel writes through. `pub(crate)` because
    /// downstream crates do not see the private transport
    /// trait.
    #[allow(dead_code)]
    pub(crate) fn transport(&self) -> Arc<T> {
        Arc::clone(&self.transport)
    }

    /// Rotate the per-turn cancellation token. Call between
    /// turns; matches `LocalChannel::reset_cancellation` and
    /// `TelegramChannel::reset_cancellation`.
    #[allow(dead_code)]
    pub(crate) fn reset_cancellation(&self) {
        let mut slot = self.token.lock().expect("token mutex poisoned");
        *slot = CancellationToken::new();
    }

    /// Snapshot the buffer contents without clearing. Test-only;
    /// the production path is `finalize()`, which drains-and-
    /// sends atomically.
    #[cfg(test)]
    pub(crate) fn buffer_snapshot(&self) -> String {
        self.buffer.lock().expect("buffer mutex poisoned").clone()
    }
}

/// Render one `StreamEvent` into the turn's text buffer.
/// Extracted from the trait impl so tests can assert the exact
/// rendering without going through the full `ChannelContext`
/// surface. The rendering matches `TelegramChannel`'s
/// `append_event` byte-for-byte — Discord users see the same
/// `→ tool_name` / `← tool_name outcome` markers Telegram users
/// see, so an operator switching channels gets a consistent
/// in-message UX.
fn append_event(buffer: &mut String, event: &StreamEvent<'_>) {
    match event {
        StreamEvent::Text(chunk) => {
            buffer.push_str(chunk);
        }
        StreamEvent::Status(s) => {
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
            // Phase 107 Q2 full-parity defers Discord attachment
            // upload to a follow-on phase. Silently drop
            // attachment events at the channel boundary — same
            // posture Telegram took at Phase 8.
        }
        StreamEvent::ToolOutput { .. } => {
            // SemiTrusted adapters render tool output at
            // ToolCallFinished, not per-chunk. Matches the
            // Phase 12 Task 1 trust-tier asymmetry the Telegram
            // adapter encoded.
        }
    }
}

/// Render a `TurnOutcome` into a short footer line. Matches the
/// Telegram precedent byte-for-byte so the in-message UX is
/// consistent across the SemiTrusted adapters.
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
impl<T: DiscordTransport + 'static> ChannelContext for DiscordChannel<T> {
    fn channel_name(&self) -> &str {
        &self.name
    }

    fn platform(&self) -> ChannelPlatform {
        ChannelPlatform::Discord
    }

    fn trust_tier(&self) -> TrustTier {
        // Security-audit fix (Task 10, 2026-09-16). Discord
        // previously had no filter mechanism at all and always
        // returned `SemiTrusted` regardless of which channel a
        // message came from — the finding this task fixes.
        // `THREAT_MODEL.md` §2 defines `SemiTrusted` as requiring an
        // allowlisted chat/channel; `Untrusted` is the tier for
        // everyone else, `None` (no filter configured) included. See
        // `TelegramChannel::trust_tier()` for the identical reasoning
        // mirrored here at Discord's own `channel_id` granularity.
        match self.channel_filter {
            Some(allowed) if allowed == self.channel_id => TrustTier::SemiTrusted,
            _ => TrustTier::Untrusted,
        }
    }

    fn session_id(&self) -> SessionId {
        self.session
    }

    fn session_partition(&self) -> Option<String> {
        // One Discord channel = one memory partition. The
        // stringified `channel_id` is the stable, Discord-
        // assigned identity the turn loop uses to namespace
        // session-scoped tool state. Two `DiscordChannel`
        // instances sharing the same memory but different
        // `channel_id`s cannot see each other's memory —
        // proven at Task 6 by the parallel of Telegram's
        // `two_chats_isolated` test.
        Some(self.channel_id.to_string())
    }

    async fn stream_event(&self, event: StreamEvent<'_>) -> Result<(), ChannelError> {
        let mut buf = self
            .buffer
            .lock()
            .map_err(|e| ChannelError::Send(format!("DiscordChannel buffer poisoned: {e}")))?;
        append_event(&mut buf, &event);
        Ok(())
    }

    async fn finalize(&self, outcome: &TurnOutcome) -> Result<(), ChannelError> {
        // Drain the buffer under the lock, release the lock
        // *before* the network call. Holding `std::sync::Mutex`
        // across an await would be a correctness bug — tokio's
        // scheduler does not understand std mutexes.
        let payload = {
            let mut buf = self
                .buffer
                .lock()
                .map_err(|e| ChannelError::Send(format!("DiscordChannel buffer poisoned: {e}")))?;
            let mut out = std::mem::take(&mut *buf);
            out.push_str(&finalize_footer(outcome));
            out
        };

        // A turn with zero streamed text and a Completed outcome
        // (e.g. a tool-only turn that the LLM finished without
        // speaking) would produce an empty payload. Discord's
        // `create_message` REST endpoint rejects empty content,
        // so we replace it with a minimal `(no reply)` rather
        // than silently dropping or erroring. Matches Telegram.
        let payload = if payload.trim().is_empty() {
            "(no reply)".to_string()
        } else {
            payload
        };

        self.transport
            .send_message(OutgoingMessage {
                channel_id: self.channel_id,
                text: payload,
            })
            .await?;

        Ok(())
    }

    fn cancellation_token(&self) -> CancellationToken {
        self.token.lock().expect("token mutex poisoned").clone()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod channel_tests {
    use super::*;
    use crate::transport::{IncomingMessage, ScriptedTransport};

    fn channel_with_empty_transport() -> DiscordChannel<ScriptedTransport> {
        let transport = Arc::new(ScriptedTransport::with_queue(vec![]));
        // `channel_filter: Some(12345)` matches `channel_id` below, so
        // every existing test built on this helper keeps seeing
        // `SemiTrusted`, exactly as it did before Task 10's
        // trust_tier() fix.
        DiscordChannel::new("aivyx-discord-test", 12345, Some(12345), transport)
    }

    // -- Identity surface ------------------------------------------------

    #[test]
    fn channel_reports_discord_platform() {
        let c = channel_with_empty_transport();
        assert_eq!(c.platform(), ChannelPlatform::Discord);
    }

    #[test]
    fn channel_reports_semitrusted_tier() {
        let c = channel_with_empty_transport();
        assert_eq!(c.trust_tier(), TrustTier::SemiTrusted);
    }

    // Security-audit fix (Task 10, 2026-09-16). `THREAT_MODEL.md`
    // defines `SemiTrusted` as requiring an allowlisted channel and
    // `Untrusted` as the tier for anyone else — these three tests pin
    // that distinction down at the `DiscordChannel::trust_tier()`
    // level. Prior to this fix Discord had no filter mechanism at
    // all and always returned `SemiTrusted`.

    #[test]
    fn allowlisted_channel_is_semitrusted() {
        let transport = Arc::new(ScriptedTransport::with_queue(vec![]));
        let c = DiscordChannel::new("allow", 12345, Some(12345), transport);
        assert_eq!(c.trust_tier(), TrustTier::SemiTrusted);
    }

    #[test]
    fn non_allowlisted_channel_is_untrusted() {
        let transport = Arc::new(ScriptedTransport::with_queue(vec![]));
        let c = DiscordChannel::new("deny", 99999, Some(12345), transport);
        assert_eq!(c.trust_tier(), TrustTier::Untrusted);
    }

    #[test]
    fn no_filter_configured_is_untrusted_by_default() {
        // The important behavior-change assertion: per
        // THREAT_MODEL.md, an unallowlisted-by-default channel (no
        // filter set at all) is Untrusted, not SemiTrusted — closing
        // the gap where "no config" silently meant "trust everyone
        // as SemiTrusted."
        let transport = Arc::new(ScriptedTransport::with_queue(vec![]));
        let c = DiscordChannel::new("nofilter", 55555, None, transport);
        assert_eq!(c.trust_tier(), TrustTier::Untrusted);
    }

    #[test]
    fn channel_reports_name() {
        let c = channel_with_empty_transport();
        assert_eq!(c.channel_name(), "aivyx-discord-test");
    }

    #[test]
    fn channel_partitions_by_channel_id() {
        let c = channel_with_empty_transport();
        assert_eq!(c.session_partition().as_deref(), Some("12345"));
    }

    #[test]
    fn channel_id_accessor_round_trips() {
        let c = channel_with_empty_transport();
        assert_eq!(c.channel_id(), 12345);
    }

    // -- append_event rendering ------------------------------------------

    #[test]
    fn append_event_text_concatenates() {
        let mut buf = String::new();
        append_event(&mut buf, &StreamEvent::Text("hello "));
        append_event(&mut buf, &StreamEvent::Text("world"));
        assert_eq!(buf, "hello world");
    }

    #[test]
    fn append_event_status_lands_on_its_own_line() {
        let mut buf = String::from("partial line");
        append_event(&mut buf, &StreamEvent::Status("thinking"));
        assert!(buf.contains("partial line\n… thinking\n"));
    }

    #[test]
    fn append_event_tool_call_started_renders_arrow_prefix() {
        let mut buf = String::new();
        let input = serde_json::Value::Null;
        append_event(
            &mut buf,
            &StreamEvent::ToolCallStarted {
                tool_name: "memory.read",
                tool: aivyx_core::ToolId::new(),
                input: &input,
            },
        );
        assert!(buf.contains("→ memory.read"));
    }

    #[test]
    fn append_event_tool_call_finished_renders_back_arrow() {
        let mut buf = String::new();
        append_event(
            &mut buf,
            &StreamEvent::ToolCallFinished {
                tool_name: "memory.read",
                outcome_summary: "ok (3 entries)",
                tool: aivyx_core::ToolId::new(),
            },
        );
        assert!(buf.contains("← memory.read ok (3 entries)"));
    }

    #[test]
    fn append_event_attachment_is_silently_dropped() {
        let mut buf = String::from("baseline");
        append_event(
            &mut buf,
            &StreamEvent::Attachment {
                kind: aivyx_core::AttachmentKind::Image { mime: "image/png" },
                data: &[0xDE, 0xAD],
                filename: None,
            },
        );
        // Buffer unchanged — attachment is dropped, not
        // rendered, per the Phase 107 Q2 attachments-deferred
        // posture.
        assert_eq!(buf, "baseline");
    }

    // -- finalize_footer rendering ---------------------------------------

    fn completed_outcome() -> TurnOutcome {
        TurnOutcome::Completed {
            final_message: "done".to_string(),
            tool_calls_made: 0,
            duration: std::time::Duration::from_secs(0),
        }
    }

    #[test]
    fn finalize_footer_completed_is_empty() {
        assert_eq!(finalize_footer(&completed_outcome()), "");
    }

    #[test]
    fn finalize_footer_failed_carries_reason() {
        let out = TurnOutcome::Failed(aivyx_core::AivyxError::Channel(
            "network blew up".to_string(),
        ));
        assert!(finalize_footer(&out).contains("network blew up"));
    }

    #[test]
    fn finalize_footer_cancelled_uses_cross_marker() {
        let out = TurnOutcome::Cancelled {
            tool_calls_made: 0,
        };
        let footer = finalize_footer(&out);
        assert!(footer.contains("✕"));
        assert!(footer.contains("cancelled"));
    }

    // -- finalize end-to-end through the scripted transport --------------

    #[tokio::test]
    async fn finalize_drains_buffer_and_sends_one_message() {
        let transport = Arc::new(ScriptedTransport::with_queue(vec![]));
        let c: DiscordChannel<ScriptedTransport> =
            DiscordChannel::new("smoke", 4242, Some(4242), Arc::clone(&transport));

        c.stream_event(StreamEvent::Text("the answer is "))
            .await
            .unwrap();
        c.stream_event(StreamEvent::Text("42"))
            .await
            .unwrap();
        c.finalize(&TurnOutcome::Completed {
            final_message: "the answer is 42".to_string(),
            tool_calls_made: 0,
            duration: std::time::Duration::from_secs(0),
        })
        .await
        .unwrap();

        let sent = transport.sent().await;
        assert_eq!(sent.len(), 1, "exactly one outbound message per turn");
        assert_eq!(sent[0].channel_id, 4242);
        assert_eq!(sent[0].text, "the answer is 42");
        assert_eq!(c.buffer_snapshot(), "", "buffer drained by finalize");
    }

    #[tokio::test]
    async fn finalize_with_empty_payload_sends_placeholder() {
        let transport = Arc::new(ScriptedTransport::with_queue(vec![]));
        let c: DiscordChannel<ScriptedTransport> =
            DiscordChannel::new("smoke", 4242, Some(4242), Arc::clone(&transport));

        // No stream_event calls at all — agent completed without
        // speaking. finalize must still emit something Discord
        // accepts.
        c.finalize(&TurnOutcome::Completed {
            final_message: String::new(),
            tool_calls_made: 0,
            duration: std::time::Duration::from_secs(0),
        })
        .await
        .unwrap();

        let sent = transport.sent().await;
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].text, "(no reply)");
    }

    #[tokio::test]
    async fn finalize_appends_outcome_footer_to_payload() {
        let transport = Arc::new(ScriptedTransport::with_queue(vec![]));
        let c: DiscordChannel<ScriptedTransport> =
            DiscordChannel::new("smoke", 4242, Some(4242), Arc::clone(&transport));

        c.stream_event(StreamEvent::Text("partial"))
            .await
            .unwrap();
        c.finalize(&TurnOutcome::Escalated {
            reason: "needs operator approval".to_string(),
            pending_tool: aivyx_core::ToolId::new(),
            scope: None,
            tool_calls_made: 0,
        })
        .await
        .unwrap();

        let sent = transport.sent().await;
        assert_eq!(sent.len(), 1);
        assert!(sent[0].text.contains("partial"));
        assert!(sent[0].text.contains("⏸ escalation: needs operator approval"));
    }

    // -- Cancellation token rotation -------------------------------------

    #[test]
    fn reset_cancellation_yields_a_fresh_token() {
        let c = channel_with_empty_transport();
        let before = c.cancellation_token();
        before.cancel();
        assert!(before.is_cancelled());

        // After reset, the channel's own token slot must be
        // a fresh one that is not cancelled.
        c.reset_cancellation();
        let after = c.cancellation_token();
        assert!(!after.is_cancelled(), "fresh token must not be cancelled");
    }

    #[test]
    fn unused_message_struct_compiles() {
        // Sanity: IncomingMessage round-trips through the
        // channel-tests module. The test exists so a future
        // refactor that drops the public field set trips here
        // before it trips at Task 6's scripted-e2e suite.
        let m = IncomingMessage {
            message_id: 1,
            channel_id: 2,
            author_id: 3,
            text: "hi".to_string(),
        };
        assert_eq!(m.text, "hi");
    }
}
