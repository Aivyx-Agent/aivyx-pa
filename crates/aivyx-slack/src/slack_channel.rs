//! [`SlackChannel`] — the `ChannelContext` impl proper.
//!
//! Mirrors `aivyx-discord::DiscordChannel` byte-for-byte at
//! the rendering layer so an operator switching between
//! Slack, Discord, and Telegram sees the same in-message UX:
//! same `→ tool_name` / `← tool_name outcome` markers, same
//! `… status` prefix, same outcome footers, same
//! `(no reply)` placeholder for empty turns.
//!
//! ## What's different from `DiscordChannel`
//!
//! Two things:
//!
//! - `channel_id` is `String` (Slack's `C0123456789` /
//!   `D0123456789`), not `u64` (Discord's snowflake).
//! - `session_partition()` returns
//!   `Some(format!("{team_id}:{channel_id}"))` per Phase 108
//!   Q3a — the four-data-point confirmation that
//!   `Option<String>` is the right partition return type.
//!   `team_id` is needed because a Slack bot can be in
//!   multiple workspaces; `channel_id` alone would alias
//!   `C0123456789` across two different teams.
//!
//! Everything else — the per-turn cancellation slot, the
//! interior buffer behind `std::sync::Mutex<String>`, the
//! `append_event` rendering, the `finalize_footer` outcome
//! rendering, the empty-payload-guard, the
//! transport-error-to-channel-error From impl — is identical
//! to `DiscordChannel`.

use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use aivyx_capability::TrustTier;
use aivyx_core::{
    CancellationToken, ChannelContext, ChannelError, ChannelPlatform, SessionId, StreamEvent,
    TurnOutcome,
};

use crate::transport::{OutgoingMessage, SlackTransport, TransportError};

/// A Slack `ChannelContext`. Generic over the transport so
/// tests can inject `ScriptedTransport` without standing up a
/// Socket Mode connection. Production type is
/// `SlackChannel<SlackMorphismTransport>`.
pub struct SlackChannel<T: SlackTransport + 'static> {
    name: String,
    session: SessionId,
    /// Slack `team_id` (`T0123456789`). Combined with
    /// `channel_id` to form the partition key.
    team_id: String,
    /// Slack `channel_id` (`C0123456789` for channel,
    /// `D0123456789` for DM). String, not u64, because
    /// Slack IDs are alphanumeric strings natively.
    channel_id: String,
    /// Security-audit fix (Task 10, 2026-09-16) — the operator's
    /// configured workspace constraint
    /// (`aivyx_config::SlackConfig::team_id`). `None` = no workspace
    /// constraint; `Some(t)` = this bot is only meant to treat
    /// workspace `t` as trusted. Consulted by `trust_tier()` alongside
    /// `channel_filter` below.
    team_filter: Option<String>,
    /// Security-audit fix (Task 10, 2026-09-16) — the operator's
    /// configured `channel_filter` (see
    /// `aivyx_config::SlackConfig::channel_filter`), mirroring
    /// Telegram's `chat_filter` at Slack's own `channel_id`
    /// granularity. `None` = no channel allowlisted, which per
    /// `THREAT_MODEL.md` means `Untrusted`, not `SemiTrusted` — see
    /// `trust_tier()` below.
    channel_filter: Option<String>,
    /// Per-turn cancellation slot. Rotated between turns.
    token: Arc<Mutex<CancellationToken>>,
    /// Accumulated turn output. Drained on each `finalize()`.
    buffer: Mutex<String>,
    /// The swappable transport.
    transport: Arc<T>,
}

impl<T: SlackTransport + 'static> SlackChannel<T> {
    /// Construct a `SlackChannel` bound to a single
    /// `(team_id, channel_id)` partition.
    ///
    /// `team_filter` / `channel_filter` are the operator's configured
    /// allowlist values (`aivyx_config::SlackConfig::team_id` /
    /// `channel_filter`), not necessarily equal to `team_id` /
    /// `channel_id` — see `trust_tier()`.
    #[allow(dead_code, clippy::too_many_arguments)]
    pub(crate) fn new(
        name: impl Into<String>,
        team_id: impl Into<String>,
        channel_id: impl Into<String>,
        team_filter: Option<String>,
        channel_filter: Option<String>,
        transport: Arc<T>,
    ) -> Self {
        SlackChannel {
            name: name.into(),
            session: SessionId::new(),
            team_id: team_id.into(),
            channel_id: channel_id.into(),
            team_filter,
            channel_filter,
            token: Arc::new(Mutex::new(CancellationToken::new())),
            buffer: Mutex::new(String::new()),
            transport,
        }
    }

    /// The Slack channel_id this channel is bound to.
    #[allow(dead_code)]
    pub(crate) fn channel_id(&self) -> &str {
        &self.channel_id
    }

    /// The Slack team_id this channel is bound to.
    #[allow(dead_code)]
    pub(crate) fn team_id(&self) -> &str {
        &self.team_id
    }

    /// The Q3a partition key — `"{team_id}:{channel_id}"`.
    /// Exposed for the session driver's per-channel mailbox
    /// routing.
    #[allow(dead_code)]
    pub(crate) fn partition_key(&self) -> String {
        format!("{}:{}", self.team_id, self.channel_id)
    }

    /// Shared handle to the transport.
    #[allow(dead_code)]
    pub(crate) fn transport(&self) -> Arc<T> {
        Arc::clone(&self.transport)
    }

    /// Rotate the per-turn cancellation token. Call between
    /// turns; matches the Local / Telegram / Discord pattern.
    #[allow(dead_code)]
    pub(crate) fn reset_cancellation(&self) {
        let mut slot = self.token.lock().expect("token mutex poisoned");
        *slot = CancellationToken::new();
    }

    /// Test-only snapshot of the buffer.
    #[cfg(test)]
    pub(crate) fn buffer_snapshot(&self) -> String {
        self.buffer.lock().expect("buffer mutex poisoned").clone()
    }
}

/// Render one `StreamEvent` into the turn's text buffer.
/// Identical byte-for-byte to
/// `aivyx-discord::discord_channel::append_event` — Slack,
/// Discord, and Telegram all render the same SemiTrusted
/// in-message UX so an operator switching adapters sees no
/// surprise UI deltas.
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
            // Phase 108 Q4a foundation-scope defers Slack
            // attachment upload to a follow-on phase. Same
            // drop-at-boundary posture as Discord (Q2 full-
            // parity) and Telegram (Phase 8 non-goal).
        }
        StreamEvent::ToolOutput { .. } => {
            // SemiTrusted adapters render at ToolCallFinished,
            // not per-chunk. Phase 12 Task 1 trust-tier
            // asymmetry — same as Discord + Telegram.
        }
    }
}

/// Render a `TurnOutcome` into a short footer line. Identical
/// to Telegram and Discord byte-for-byte for cross-adapter
/// UX consistency.
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
impl<T: SlackTransport + 'static> ChannelContext for SlackChannel<T> {
    fn channel_name(&self) -> &str {
        &self.name
    }

    fn platform(&self) -> ChannelPlatform {
        ChannelPlatform::Slack
    }

    fn trust_tier(&self) -> TrustTier {
        // Security-audit fix (Task 10, 2026-09-16). Prior to this
        // fix, Slack only optionally constrained by workspace
        // (`team_id`) — and that constraint was never actually
        // consulted anywhere, so it was dead config — while
        // `trust_tier()` itself always returned `SemiTrusted`
        // regardless. `THREAT_MODEL.md` §2 defines `SemiTrusted` as
        // requiring an allowlisted chat/channel; `Untrusted` is the
        // tier for everyone else. `SemiTrusted` now requires BOTH:
        // this channel matches the operator's configured
        // `channel_filter` (mirroring Telegram's `chat_filter`/
        // Discord's `channel_filter`), AND, if a `team_filter`
        // (workspace constraint) is also configured, this message's
        // `team_id` matches it too (defense in depth — a channel_id
        // could theoretically collide across two different
        // workspaces the bot is installed in). No `channel_filter`
        // configured at all (`None`) is `Untrusted`, full stop — a
        // `team_filter` alone is not fine-grained enough to satisfy
        // THREAT_MODEL.md's "allowlisted chat" requirement.
        let channel_allowed =
            matches!(&self.channel_filter, Some(id) if id == &self.channel_id);
        let team_ok = match &self.team_filter {
            Some(t) => t == &self.team_id,
            None => true,
        };
        if channel_allowed && team_ok {
            TrustTier::SemiTrusted
        } else {
            TrustTier::Untrusted
        }
    }

    fn session_id(&self) -> SessionId {
        self.session
    }

    fn session_partition(&self) -> Option<String> {
        // Phase 108 Q3a — stringify (team_id, channel_id) as
        // the partition key. Two SlackChannel instances
        // sharing one memory but with different
        // (team_id, channel_id) pairs cannot see each
        // other's memory. The colon-joined string fits
        // `Option<String>` and confirms the three-data-point
        // adapter pattern at four data points; punting the
        // Phase 9 Q7 richer-type question to a future
        // Matrix-shaped adapter.
        Some(self.partition_key())
    }

    async fn stream_event(&self, event: StreamEvent<'_>) -> Result<(), ChannelError> {
        let mut buf = self
            .buffer
            .lock()
            .map_err(|e| ChannelError::Send(format!("SlackChannel buffer poisoned: {e}")))?;
        append_event(&mut buf, &event);
        Ok(())
    }

    async fn finalize(&self, outcome: &TurnOutcome) -> Result<(), ChannelError> {
        // Same drain-under-lock-then-release-before-await
        // pattern as Discord. Holding std::sync::Mutex
        // across an await would be a correctness bug; tokio
        // does not understand std mutexes.
        let payload = {
            let mut buf = self
                .buffer
                .lock()
                .map_err(|e| ChannelError::Send(format!("SlackChannel buffer poisoned: {e}")))?;
            let mut out = std::mem::take(&mut *buf);
            out.push_str(&finalize_footer(outcome));
            out
        };

        // Slack's chat.postMessage rejects empty content
        // (or — in some channel-type combinations — silently
        // discards it). Substitute (no reply) so the
        // operator sees something deterministic. Matches
        // Discord + Telegram.
        let payload = if payload.trim().is_empty() {
            "(no reply)".to_string()
        } else {
            payload
        };

        self.transport
            .send_message(OutgoingMessage {
                channel_id: self.channel_id.clone(),
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

    fn channel_with_empty_transport() -> SlackChannel<ScriptedTransport> {
        let transport = Arc::new(ScriptedTransport::with_queue(vec![]));
        // `channel_filter: Some("C123")` matches `channel_id` below,
        // so every existing test built on this helper keeps seeing
        // `SemiTrusted`, exactly as it did before Task 10's
        // trust_tier() fix.
        SlackChannel::new(
            "aivyx-slack-test",
            "T01",
            "C123",
            None,
            Some("C123".to_string()),
            transport,
        )
    }

    // -- Identity surface ------------------------------------------------

    #[test]
    fn channel_reports_slack_platform() {
        let c = channel_with_empty_transport();
        assert_eq!(c.platform(), ChannelPlatform::Slack);
    }

    #[test]
    fn channel_reports_semitrusted_tier() {
        let c = channel_with_empty_transport();
        assert_eq!(c.trust_tier(), TrustTier::SemiTrusted);
    }

    // Security-audit fix (Task 10, 2026-09-16). `THREAT_MODEL.md`
    // defines `SemiTrusted` as requiring an allowlisted chat/channel
    // and `Untrusted` as the tier for anyone else — these tests pin
    // that distinction down at the `SlackChannel::trust_tier()`
    // level. Prior to this fix Slack's `team_id` constraint was
    // never actually consulted, and `trust_tier()` always returned
    // `SemiTrusted` regardless.

    #[test]
    fn allowlisted_channel_is_semitrusted() {
        let transport = Arc::new(ScriptedTransport::with_queue(vec![]));
        let c = SlackChannel::new(
            "allow",
            "T01",
            "C123",
            None,
            Some("C123".to_string()),
            transport,
        );
        assert_eq!(c.trust_tier(), TrustTier::SemiTrusted);
    }

    #[test]
    fn non_allowlisted_channel_is_untrusted() {
        let transport = Arc::new(ScriptedTransport::with_queue(vec![]));
        let c = SlackChannel::new(
            "deny",
            "T01",
            "C999",
            None,
            Some("C123".to_string()),
            transport,
        );
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
        let c = SlackChannel::new("nofilter", "T01", "C555", None, None, transport);
        assert_eq!(c.trust_tier(), TrustTier::Untrusted);
    }

    #[test]
    fn team_filter_mismatch_denies_even_with_matching_channel_filter() {
        // Defense in depth: a configured team_filter that doesn't
        // match this message's team_id denies SemiTrusted even when
        // channel_filter matches — a channel_id could collide across
        // two different workspaces the bot is installed in.
        let transport = Arc::new(ScriptedTransport::with_queue(vec![]));
        let c = SlackChannel::new(
            "team-mismatch",
            "T_OTHER",
            "C123",
            Some("T01".to_string()),
            Some("C123".to_string()),
            transport,
        );
        assert_eq!(c.trust_tier(), TrustTier::Untrusted);
    }

    #[test]
    fn channel_reports_name() {
        let c = channel_with_empty_transport();
        assert_eq!(c.channel_name(), "aivyx-slack-test");
    }

    #[test]
    fn channel_partitions_by_team_colon_channel() {
        let c = channel_with_empty_transport();
        assert_eq!(c.session_partition().as_deref(), Some("T01:C123"));
    }

    #[test]
    fn channel_id_and_team_id_accessors_round_trip() {
        let c = channel_with_empty_transport();
        assert_eq!(c.channel_id(), "C123");
        assert_eq!(c.team_id(), "T01");
        assert_eq!(c.partition_key(), "T01:C123");
    }

    // -- Partition shape with multi-workspace ----------------------------

    #[test]
    fn two_channels_with_same_channel_id_but_different_team_id_partition_distinctly() {
        // The load-bearing four-data-point assertion: a Slack
        // bot in two workspaces that happen to allocate the
        // same channel_id (rare but possible) must partition
        // into two distinct buckets. This is the property
        // Q3a's team_id-prefixed partition key buys us.
        let transport = Arc::new(ScriptedTransport::with_queue(vec![]));
        let c_a: SlackChannel<ScriptedTransport> = SlackChannel::new(
            "slack",
            "TEAM_A",
            "CSHARED",
            None,
            None,
            Arc::clone(&transport),
        );
        let c_b: SlackChannel<ScriptedTransport> = SlackChannel::new(
            "slack",
            "TEAM_B",
            "CSHARED",
            None,
            None,
            Arc::clone(&transport),
        );
        assert_ne!(
            c_a.session_partition(),
            c_b.session_partition(),
            "same channel_id in different teams must partition distinctly",
        );
        assert_eq!(c_a.session_partition().as_deref(), Some("TEAM_A:CSHARED"));
        assert_eq!(c_b.session_partition().as_deref(), Some("TEAM_B:CSHARED"));
    }

    // -- append_event rendering ------------------------------------------

    #[test]
    fn append_event_text_concatenates() {
        let mut buf = String::new();
        append_event(&mut buf, &StreamEvent::Text("hello "));
        append_event(&mut buf, &StreamEvent::Text("slack"));
        assert_eq!(buf, "hello slack");
    }

    #[test]
    fn append_event_status_lands_on_its_own_line() {
        let mut buf = String::from("partial line");
        append_event(&mut buf, &StreamEvent::Status("thinking"));
        assert!(buf.contains("partial line\n… thinking\n"));
    }

    #[test]
    fn append_event_tool_call_renders_arrow_prefix() {
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
        assert_eq!(buf, "baseline");
    }

    // -- finalize end-to-end through the scripted transport --------------

    fn completed_outcome() -> TurnOutcome {
        TurnOutcome::Completed {
            final_message: "done".to_string(),
            tool_calls_made: 0,
            duration: std::time::Duration::from_secs(0),
        }
    }

    #[tokio::test]
    async fn finalize_drains_buffer_and_sends_one_message() {
        let transport = Arc::new(ScriptedTransport::with_queue(vec![]));
        let c: SlackChannel<ScriptedTransport> = SlackChannel::new(
            "smoke",
            "T01",
            "C42",
            None,
            None,
            Arc::clone(&transport),
        );

        c.stream_event(StreamEvent::Text("the answer is "))
            .await
            .unwrap();
        c.stream_event(StreamEvent::Text("42"))
            .await
            .unwrap();
        c.finalize(&completed_outcome()).await.unwrap();

        let sent = transport.sent().await;
        assert_eq!(sent.len(), 1, "exactly one outbound message per turn");
        assert_eq!(sent[0].channel_id, "C42");
        assert_eq!(sent[0].text, "the answer is 42");
        assert_eq!(c.buffer_snapshot(), "", "buffer drained by finalize");
    }

    #[tokio::test]
    async fn finalize_with_empty_payload_sends_placeholder() {
        let transport = Arc::new(ScriptedTransport::with_queue(vec![]));
        let c: SlackChannel<ScriptedTransport> = SlackChannel::new(
            "smoke",
            "T01",
            "C42",
            None,
            None,
            Arc::clone(&transport),
        );
        c.finalize(&completed_outcome()).await.unwrap();

        let sent = transport.sent().await;
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].text, "(no reply)");
    }

    #[tokio::test]
    async fn finalize_appends_outcome_footer_to_payload() {
        let transport = Arc::new(ScriptedTransport::with_queue(vec![]));
        let c: SlackChannel<ScriptedTransport> = SlackChannel::new(
            "smoke",
            "T01",
            "C42",
            None,
            None,
            Arc::clone(&transport),
        );

        c.stream_event(StreamEvent::Text("partial")).await.unwrap();
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

    // -- Cancellation rotation -------------------------------------------

    #[test]
    fn reset_cancellation_yields_a_fresh_token() {
        let c = channel_with_empty_transport();
        let before = c.cancellation_token();
        before.cancel();
        assert!(before.is_cancelled());

        c.reset_cancellation();
        let after = c.cancellation_token();
        assert!(!after.is_cancelled(), "fresh token must not be cancelled");
    }

    #[test]
    fn unused_incoming_message_compiles() {
        // Sanity-pin: the IncomingMessage public field set
        // must stay compatible with the channel tests so a
        // future refactor that drops fields trips here.
        let m = IncomingMessage {
            team_id: "T01".to_string(),
            channel_id: "C42".to_string(),
            user_id: "U001".to_string(),
            text: "hi".to_string(),
            message_ts: "1700000000.000100".to_string(),
        };
        assert_eq!(m.partition_key(), "T01:C42");
        assert_eq!(m.text, "hi");
    }
}
