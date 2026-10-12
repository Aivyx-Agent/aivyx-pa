//! [`SignalChannel`] — the `ChannelContext` for one Signal turn. The
//! reply is sent once complete: streamed text is buffered and, at
//! `finalize`, kept for the session loop, which sends it (split into
//! Signal-sized parts).

use std::fmt::Write as _;
use std::sync::Mutex;

use async_trait::async_trait;

use aivyx_capability::TrustTier;
use aivyx_core::{
    CancellationToken, ChannelContext, ChannelError, ChannelPlatform, SessionId, StreamEvent,
    TurnOutcome,
};

pub struct SignalChannel {
    session: SessionId,
    partition: String,
    token: Mutex<CancellationToken>,
    buffer: Mutex<String>,
    reply: Mutex<Option<String>>,
}

impl SignalChannel {
    /// `session` is the sender's conversation; `partition` its memory
    /// namespace (`session::partition`).
    pub(crate) fn new(session: SessionId, partition: String) -> Self {
        SignalChannel {
            session,
            partition,
            token: Mutex::new(CancellationToken::new()),
            buffer: Mutex::new(String::new()),
            reply: Mutex::new(None),
        }
    }

    /// The finished reply, once `finalize` ran.
    pub(crate) fn take_reply(&self) -> Option<String> {
        self.reply.lock().expect("reply mutex poisoned").take()
    }
}

fn append_event(buffer: &mut String, event: &StreamEvent<'_>) {
    let newline = |b: &mut String| {
        if !b.is_empty() && !b.ends_with('\n') {
            b.push('\n');
        }
    };
    match event {
        StreamEvent::Text(chunk) => buffer.push_str(chunk),
        StreamEvent::Status(s) => {
            newline(buffer);
            let _ = writeln!(buffer, "… {s}");
        }
        StreamEvent::ToolCallStarted { tool_name, .. } => {
            newline(buffer);
            let _ = writeln!(buffer, "→ {tool_name}");
        }
        StreamEvent::ToolCallFinished {
            tool_name,
            outcome_summary,
            ..
        } => {
            newline(buffer);
            let _ = writeln!(buffer, "← {tool_name} {outcome_summary}");
        }
        StreamEvent::Attachment { .. } | StreamEvent::ToolOutput { .. } => {}
    }
}

fn finalize_footer(outcome: &TurnOutcome) -> String {
    match outcome {
        TurnOutcome::Completed { .. } => String::new(),
        TurnOutcome::Escalated { reason, .. } => format!("\n⏸ escalation: {reason}"),
        TurnOutcome::TimedOut { elapsed, .. } => format!("\n⏱ timed out after {elapsed:?}"),
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

#[async_trait]
impl ChannelContext for SignalChannel {
    fn channel_name(&self) -> &str {
        "aivyx-signal"
    }

    fn platform(&self) -> ChannelPlatform {
        ChannelPlatform::Signal
    }

    fn trust_tier(&self) -> TrustTier {
        // Only messages `policy::decide` approved reach a channel: an
        // allowed sender whose safety number the operator verified.
        TrustTier::SemiTrusted
    }

    fn session_id(&self) -> SessionId {
        self.session
    }

    fn session_partition(&self) -> Option<String> {
        Some(self.partition.clone())
    }

    async fn stream_event(&self, event: StreamEvent<'_>) -> Result<(), ChannelError> {
        let mut buf = self
            .buffer
            .lock()
            .map_err(|e| ChannelError::Send(format!("SignalChannel buffer poisoned: {e}")))?;
        append_event(&mut buf, &event);
        Ok(())
    }

    async fn finalize(&self, outcome: &TurnOutcome) -> Result<(), ChannelError> {
        let mut out = {
            let mut buf = self
                .buffer
                .lock()
                .map_err(|e| ChannelError::Send(format!("SignalChannel buffer poisoned: {e}")))?;
            std::mem::take(&mut *buf)
        };
        out.push_str(&finalize_footer(outcome));
        let text = if out.trim().is_empty() {
            "(no reply)".to_string()
        } else {
            out
        };
        *self.reply.lock().expect("reply mutex poisoned") = Some(text);
        Ok(())
    }

    fn cancellation_token(&self) -> CancellationToken {
        self.token.lock().expect("token mutex poisoned").clone()
    }

    fn reset_cancellation(&self) {
        *self.token.lock().expect("token mutex poisoned") = CancellationToken::new();
    }
}

#[cfg(test)]
mod channel_tests {
    use super::*;

    fn channel() -> SignalChannel {
        SignalChannel::new(SessionId::new(), "signal-abc".into())
    }

    #[test]
    fn identity_is_signal_semitrusted_partitioned_by_sender() {
        let c = channel();
        assert_eq!(c.platform(), ChannelPlatform::Signal);
        assert_eq!(c.trust_tier(), TrustTier::SemiTrusted);
        assert_eq!(c.session_partition().as_deref(), Some("signal-abc"));
        assert_eq!(c.channel_name(), "aivyx-signal");
    }

    #[tokio::test]
    async fn finalize_keeps_the_whole_reply_once() {
        let c = channel();
        assert_eq!(c.take_reply(), None);
        c.stream_event(StreamEvent::Text("the answer is ")).await.unwrap();
        c.stream_event(StreamEvent::Text("42")).await.unwrap();
        c.finalize(&TurnOutcome::Completed {
            final_message: "the answer is 42".into(),
            tool_calls_made: 0,
            duration: std::time::Duration::ZERO,
        })
        .await
        .unwrap();
        assert_eq!(c.take_reply().as_deref(), Some("the answer is 42"));
        assert_eq!(c.take_reply(), None);
    }

    #[tokio::test]
    async fn an_empty_turn_still_says_something() {
        let c = channel();
        c.finalize(&TurnOutcome::Cancelled { tool_calls_made: 0 }).await.unwrap();
        assert_eq!(c.take_reply().as_deref(), Some("\n✕ cancelled"));
    }

    #[test]
    fn reset_cancellation_yields_a_fresh_token() {
        let c = channel();
        c.cancellation_token().cancel();
        c.reset_cancellation();
        assert!(!c.cancellation_token().is_cancelled());
    }
}
