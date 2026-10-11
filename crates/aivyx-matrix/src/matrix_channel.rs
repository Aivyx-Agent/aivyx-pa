//! [`MatrixChannel`] — the `ChannelContext` for one Matrix room. Whole-
//! message delivery: streamed text is buffered and sent once at
//! `finalize`, as on Telegram and Discord.

use std::fmt::Write as _;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use aivyx_capability::TrustTier;
use aivyx_core::{
    CancellationToken, ChannelContext, ChannelError, ChannelPlatform, SessionId, StreamEvent,
    TurnOutcome,
};

use crate::transport::{MatrixTransport, OutgoingMessage, TransportError};

pub struct MatrixChannel<T: MatrixTransport + 'static> {
    name: String,
    session: SessionId,
    room_id: String,
    token: Arc<Mutex<CancellationToken>>,
    buffer: Mutex<String>,
    transport: Arc<T>,
}

impl<T: MatrixTransport + 'static> MatrixChannel<T> {
    pub(crate) fn new(name: impl Into<String>, room_id: String, transport: Arc<T>) -> Self {
        MatrixChannel {
            name: name.into(),
            session: SessionId::new(),
            room_id,
            token: Arc::new(Mutex::new(CancellationToken::new())),
            buffer: Mutex::new(String::new()),
            transport,
        }
    }

    pub(crate) fn reset_cancellation(&self) {
        *self.token.lock().expect("token mutex poisoned") = CancellationToken::new();
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

impl From<TransportError> for ChannelError {
    fn from(e: TransportError) -> Self {
        ChannelError::Platform(e.to_string())
    }
}

#[async_trait]
impl<T: MatrixTransport + 'static> ChannelContext for MatrixChannel<T> {
    fn channel_name(&self) -> &str {
        &self.name
    }

    fn platform(&self) -> ChannelPlatform {
        ChannelPlatform::Matrix
    }

    fn trust_tier(&self) -> TrustTier {
        // Only messages `policy::decide` turned into turns ever reach a
        // channel: the operator, in the home room, from a verified device.
        TrustTier::SemiTrusted
    }

    fn session_id(&self) -> SessionId {
        self.session
    }

    fn session_partition(&self) -> Option<String> {
        Some(self.room_id.clone())
    }

    async fn stream_event(&self, event: StreamEvent<'_>) -> Result<(), ChannelError> {
        let mut buf = self
            .buffer
            .lock()
            .map_err(|e| ChannelError::Send(format!("MatrixChannel buffer poisoned: {e}")))?;
        append_event(&mut buf, &event);
        Ok(())
    }

    async fn finalize(&self, outcome: &TurnOutcome) -> Result<(), ChannelError> {
        let payload = {
            let mut buf = self
                .buffer
                .lock()
                .map_err(|e| ChannelError::Send(format!("MatrixChannel buffer poisoned: {e}")))?;
            let mut out = std::mem::take(&mut *buf);
            out.push_str(&finalize_footer(outcome));
            out
        };
        let text = if payload.trim().is_empty() {
            "(no reply)".to_string()
        } else {
            payload
        };
        self.transport
            .send_message(OutgoingMessage {
                room_id: self.room_id.clone(),
                text,
            })
            .await?;
        Ok(())
    }

    fn cancellation_token(&self) -> CancellationToken {
        self.token.lock().expect("token mutex poisoned").clone()
    }
}

#[cfg(test)]
mod channel_tests {
    use super::*;
    use crate::transport::ScriptedTransport;

    fn channel() -> (MatrixChannel<ScriptedTransport>, Arc<ScriptedTransport>) {
        let t = Arc::new(ScriptedTransport::with_queue(vec![]));
        (
            MatrixChannel::new("aivyx-matrix-test", "!home:x".into(), Arc::clone(&t)),
            t,
        )
    }

    #[test]
    fn identity_is_matrix_semitrusted_partitioned_by_room() {
        let (c, _) = channel();
        assert_eq!(c.platform(), ChannelPlatform::Matrix);
        assert_eq!(c.trust_tier(), TrustTier::SemiTrusted);
        assert_eq!(c.session_partition().as_deref(), Some("!home:x"));
        assert_eq!(c.channel_name(), "aivyx-matrix-test");
    }

    #[tokio::test]
    async fn finalize_sends_one_message_with_the_streamed_text() {
        let (c, t) = channel();
        c.stream_event(StreamEvent::Text("the answer is "))
            .await
            .unwrap();
        c.stream_event(StreamEvent::Text("42")).await.unwrap();
        c.finalize(&TurnOutcome::Completed {
            final_message: "the answer is 42".into(),
            tool_calls_made: 0,
            duration: std::time::Duration::ZERO,
        })
        .await
        .unwrap();
        assert_eq!(
            t.sent().await,
            vec![OutgoingMessage {
                room_id: "!home:x".into(),
                text: "the answer is 42".into()
            }]
        );
    }

    #[tokio::test]
    async fn an_empty_turn_still_sends_something() {
        let (c, t) = channel();
        c.finalize(&TurnOutcome::Cancelled { tool_calls_made: 0 })
            .await
            .unwrap();
        assert_eq!(t.sent().await[0].text, "\n✕ cancelled");
    }

    #[test]
    fn reset_cancellation_yields_a_fresh_token() {
        let (c, _) = channel();
        c.cancellation_token().cancel();
        c.reset_cancellation();
        assert!(!c.cancellation_token().is_cancelled());
    }
}
