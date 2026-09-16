//! `LocalChannel` — the first real `ChannelContext` implementation.
//!
//! A CLI channel that writes streamed text + tool markers + finalization
//! lines to a `std::io::Write` sink. The production binary wires it to
//! `io::stdout()`; unit tests wire it to a `Vec<u8>` behind an `Arc<Mutex>`
//! so assertions can inspect exactly what the user would have seen.
//!
//! ## Design notes
//!
//! - `ChannelContext` requires `Send + Sync`. The writer is held behind a
//!   `Mutex<W>` so that the `stream_event` / `finalize` async methods can
//!   take `&self` while still mutating the sink. The mutex is *never*
//!   held across an await — every critical section is a `write_all` call
//!   followed by a `flush`, both synchronous, so there's no deadlock
//!   risk from the cooperative scheduler.
//! - `W: Write + Send + 'static`. `Sync` is explicitly *not* required on
//!   the writer itself — `Mutex<W>` is `Sync` for any `Send` interior, so
//!   adding `Sync` would needlessly exclude common sinks like
//!   `io::Stdout` which is `Send` but technically `Sync` via its own
//!   lock.
//! - `flush()` runs after every text chunk. The whole point of a CLI
//!   channel is that tokens appear *as they arrive*; buffering defeats
//!   the streaming story. This makes each text chunk a syscall, which
//!   is fine at LLM token rates (~100/sec max) but would be a
//!   performance trap if the channel were ever reused for bulk I/O.
//!   Flagged, not fixed — a future batch channel is a different impl.
//! - `StreamEvent::Text` writes the chunk verbatim (no newline). The
//!   LLM is responsible for its own whitespace; adding one would
//!   double-space any model that already includes trailing newlines.
//! - `StreamEvent::ToolCallStarted` / `ToolCallFinished` get one-line
//!   markers prefixed with `  → ` / `  ← ` so they're visually distinct
//!   from the assistant text stream.
//! - `finalize()` prints a trailing newline + a one-line turn-outcome
//!   marker. This is the first phase where `finalize` actually writes
//!   anything — Phase 1/2 tests all no-op'd it — so the format is
//!   deliberately minimal and grep-able.

use std::io::Write;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use aivyx_capability::TrustTier;
use aivyx_core::{
    CancellationToken, ChannelContext, ChannelError, ChannelPlatform, SessionId, StreamEvent,
    TurnOutcome,
};

use crate::render::{render_finalize, render_stream_event, RenderMode};

/// A CLI `ChannelContext` that streams turn output to a synchronous
/// writer.
///
/// Generic over `W: Write + Send` so the same impl covers `io::Stdout`
/// in production and `Vec<u8>` in tests.
pub struct LocalChannel<W: Write + Send + 'static> {
    name: String,
    session: SessionId,
    /// Wrapped in `Mutex` so [`reset_cancellation`](Self::reset_cancellation)
    /// can swap in a fresh token between turns. `tokio_util`'s
    /// `CancellationToken` is monotonic — once cancelled, it stays
    /// cancelled — so a single token across turns would have every
    /// turn after the first ctrl-C see a pre-cancelled token. Rotating
    /// per turn is the minimum fix that keeps the `ChannelContext`
    /// trait surface untouched.
    token: Arc<Mutex<CancellationToken>>,
    writer: Arc<Mutex<W>>,
}

impl<W: Write + Send + 'static> LocalChannel<W> {
    /// Construct a `LocalChannel` with a caller-supplied writer. The
    /// writer is wrapped in an `Arc<Mutex>` internally; callers that
    /// need to inspect the sink after a turn can obtain a shared
    /// handle via [`writer_handle`](Self::writer_handle).
    pub fn new(name: impl Into<String>, writer: W) -> Self {
        LocalChannel {
            name: name.into(),
            session: SessionId::new(),
            token: Arc::new(Mutex::new(CancellationToken::new())),
            writer: Arc::new(Mutex::new(writer)),
        }
    }

    /// Obtain a second handle to the underlying writer. Tests use this
    /// to read back what was written after a turn has completed.
    pub fn writer_handle(&self) -> Arc<Mutex<W>> {
        Arc::clone(&self.writer)
    }

    /// Obtain a shared handle to the token slot itself. The binary's
    /// signal handler uses this so it can re-read the *current* token
    /// on every ctrl-C (rather than capturing a single long-lived
    /// clone before any rotation happens).
    pub fn token_slot(&self) -> Arc<Mutex<CancellationToken>> {
        Arc::clone(&self.token)
    }

    /// Snapshot the current cancellation token. Non-rotating callers
    /// (the turn loop inside `agent.rs`) see this value for the life
    /// of a single turn.
    pub fn cancel_handle(&self) -> CancellationToken {
        self.token.lock().expect("token mutex poisoned").clone()
    }

    /// Rotate the cancellation token. Called by the CLI binary between
    /// turns: once `reset_cancellation()` returns, the next
    /// `cancellation_token()` call yields a fresh token that has never
    /// been cancelled. Any previously-cloned handle is orphaned and
    /// ignored by the new turn.
    pub fn reset_cancellation(&self) {
        let mut slot = self.token.lock().expect("token mutex poisoned");
        *slot = CancellationToken::new();
    }
}

#[async_trait]
impl<W: Write + Send + 'static> ChannelContext for LocalChannel<W> {
    fn channel_name(&self) -> &str {
        &self.name
    }

    fn platform(&self) -> ChannelPlatform {
        ChannelPlatform::Local
    }

    fn trust_tier(&self) -> TrustTier {
        // The local CLI is the single most trusted channel on the box —
        // it runs as the user, in the user's shell. Remote channels
        // will drop to `SemiTrusted` or `Untrusted` as they're added.
        TrustTier::Trusted
    }

    fn session_id(&self) -> SessionId {
        self.session
    }

    async fn stream_event(&self, event: StreamEvent<'_>) -> Result<(), ChannelError> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|e| ChannelError::Send(format!("LocalChannel writer poisoned: {e}")))?;

        render_stream_event(RenderMode::Human, &mut *writer, &event)
            .map_err(|e| ChannelError::Send(format!("LocalChannel write failed: {e}")))?;
        writer
            .flush()
            .map_err(|e| ChannelError::Send(format!("LocalChannel flush failed: {e}")))?;

        Ok(())
    }

    async fn finalize(&self, outcome: &TurnOutcome) -> Result<(), ChannelError> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|e| ChannelError::Send(format!("LocalChannel writer poisoned: {e}")))?;

        render_finalize(RenderMode::Human, &mut *writer, outcome)
            .map_err(|e| ChannelError::Send(format!("LocalChannel finalize failed: {e}")))?;
        writer
            .flush()
            .map_err(|e| ChannelError::Send(format!("LocalChannel flush failed: {e}")))?;

        Ok(())
    }

    fn cancellation_token(&self) -> CancellationToken {
        self.token.lock().expect("token mutex poisoned").clone()
    }
}

// ---------------------------------------------------------------------------
// TierOverride — trust-tier-overriding delegation wrapper.
// ---------------------------------------------------------------------------

/// Wraps any `ChannelContext` and forces `trust_tier()` to a fixed value,
/// delegating every other method to the inner channel unchanged. Used to
/// downgrade a trigger source (webhooks) that would otherwise construct
/// a full-trust channel via the shared `channel_factory`, without
/// touching that channel type's own trust semantics for its normal
/// callers.
///
/// Stores the inner channel as a concrete
/// `Arc<dyn ChannelContext + Send + Sync>` rather than a generic
/// `C: ChannelContext` — there is exactly one call site
/// (`trigger.rs::fire()`'s webhook branch), and `ChannelFactory` already
/// returns that exact trait-object type, so a generic parameter would
/// add no flexibility, only an extra type parameter to thread through
/// call sites.
pub struct TierOverride {
    inner: Arc<dyn ChannelContext + Send + Sync>,
    tier: TrustTier,
}

impl TierOverride {
    pub fn new(inner: Arc<dyn ChannelContext + Send + Sync>, tier: TrustTier) -> Self {
        TierOverride { inner, tier }
    }
}

#[async_trait]
impl ChannelContext for TierOverride {
    fn channel_name(&self) -> &str {
        self.inner.channel_name()
    }

    fn platform(&self) -> ChannelPlatform {
        self.inner.platform()
    }

    fn trust_tier(&self) -> TrustTier {
        self.tier
    }

    fn session_id(&self) -> SessionId {
        self.inner.session_id()
    }

    async fn stream_event(&self, event: StreamEvent<'_>) -> Result<(), ChannelError> {
        self.inner.stream_event(event).await
    }

    async fn finalize(&self, outcome: &TurnOutcome) -> Result<(), ChannelError> {
        self.inner.finalize(outcome).await
    }

    fn cancellation_token(&self) -> CancellationToken {
        self.inner.cancellation_token()
    }

    // Default-body methods on `ChannelContext` — delegated explicitly so
    // this wrapper stays transparent rather than silently reverting to
    // the trait's own default (which would ignore whatever the inner
    // channel actually does).
    fn session_partition(&self) -> Option<String> {
        self.inner.session_partition()
    }

    fn reset_cancellation(&self) {
        self.inner.reset_cancellation();
    }

    fn cancel_inflight(&self) {
        self.inner.cancel_inflight();
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    //! Tests here focus on what `LocalChannel` *specifically owns* —
    //! metadata, session stability, cancellation, and the flush-per-
    //! chunk guarantee. Per-variant render format is covered by the
    //! `render` module's own tests (`render::tests`). One end-to-end
    //! delegation smoke test proves the channel actually calls the
    //! renderer.

    use super::*;

    /// Build a `LocalChannel` wrapped around an in-memory byte sink,
    /// plus the shared handle the test uses to read back what was
    /// written.
    fn mem_channel() -> (LocalChannel<Vec<u8>>, Arc<Mutex<Vec<u8>>>) {
        let channel = LocalChannel::new("test", Vec::<u8>::new());
        let handle = channel.writer_handle();
        (channel, handle)
    }

    fn read_output(handle: &Arc<Mutex<Vec<u8>>>) -> String {
        String::from_utf8(handle.lock().unwrap().clone()).expect("output is utf-8")
    }

    #[test]
    fn platform_and_trust_tier_are_local_and_trusted() {
        let (channel, _) = mem_channel();
        assert_eq!(channel.platform(), ChannelPlatform::Local);
        assert_eq!(channel.trust_tier(), TrustTier::Trusted);
        assert_eq!(channel.channel_name(), "test");
    }

    #[test]
    fn session_id_is_stable_across_reads() {
        let (channel, _) = mem_channel();
        let s1 = channel.session_id();
        let s2 = channel.session_id();
        assert_eq!(s1, s2, "session_id must be stable across calls");
    }

    #[tokio::test]
    async fn stream_event_delegates_to_renderer() {
        // One integration smoke test proving the channel's
        // stream_event + finalize actually call through the render
        // module. Detailed per-variant format assertions live in
        // `render::tests`.
        let (channel, handle) = mem_channel();
        channel
            .stream_event(StreamEvent::Text("hello "))
            .await
            .unwrap();
        channel
            .stream_event(StreamEvent::Text("world"))
            .await
            .unwrap();
        channel
            .finalize(&TurnOutcome::Completed {
                final_message: "hello world".into(),
                tool_calls_made: 0,
                duration: std::time::Duration::from_millis(1),
            })
            .await
            .unwrap();

        let out = read_output(&handle);
        assert!(out.starts_with("hello world"), "text was relayed: {out:?}");
        assert!(
            out.ends_with("[turn completed]\n"),
            "finalize marker was relayed: {out:?}"
        );
    }

    #[tokio::test]
    async fn cancellation_token_is_shared_between_channel_and_handle() {
        let (channel, _) = mem_channel();
        let cancel = channel.cancel_handle();
        assert!(!channel.cancellation_token().is_cancelled());
        cancel.cancel();
        assert!(
            channel.cancellation_token().is_cancelled(),
            "cancelling via cancel_handle should be visible through cancellation_token()"
        );
    }

    #[tokio::test]
    async fn reset_cancellation_swaps_in_a_fresh_token() {
        // After cancelling and then calling reset_cancellation, the
        // channel's cancellation_token() must report *not* cancelled.
        // This is the fix for the Phase 3 task 3 monotonic-token bug:
        // a ctrl-C during turn N must not poison turn N+1.
        let (channel, _) = mem_channel();
        let old = channel.cancel_handle();
        old.cancel();
        assert!(channel.cancellation_token().is_cancelled());

        channel.reset_cancellation();
        assert!(
            !channel.cancellation_token().is_cancelled(),
            "after reset_cancellation the new token must be un-cancelled"
        );
        // The previously-captured clone still reports cancelled — it's
        // the *old* token, which is monotonic and stays cancelled.
        // Orphaned handles being stuck in the cancelled state is the
        // whole reason we rotate instead of reset-in-place.
        assert!(
            old.is_cancelled(),
            "orphaned handle retains its old cancelled state"
        );
    }

    #[tokio::test]
    async fn token_slot_sees_rotation() {
        // The signal handler in the binary reads the *current* token
        // through `token_slot()` on every ctrl-C, not through a single
        // clone. Verify the slot handle actually observes rotation.
        let (channel, _) = mem_channel();
        let slot = channel.token_slot();

        let before = slot.lock().unwrap().clone();
        before.cancel();
        channel.reset_cancellation();

        let after = slot.lock().unwrap().clone();
        assert!(
            !after.is_cancelled(),
            "slot should expose the post-rotation token, not the cancelled one"
        );
    }

    #[tokio::test]
    async fn each_text_chunk_is_flushed_so_streaming_is_visible() {
        // Proxy for "we called flush after every write": wrap the sink
        // in a type that counts flushes, and assert one flush per write.
        struct CountingSink {
            buf: Vec<u8>,
            flushes: usize,
        }
        impl Write for CountingSink {
            fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
                self.buf.extend_from_slice(data);
                Ok(data.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                self.flushes += 1;
                Ok(())
            }
        }

        let sink = CountingSink {
            buf: Vec::new(),
            flushes: 0,
        };
        let channel = LocalChannel::new("counting", sink);
        let handle = channel.writer_handle();

        channel.stream_event(StreamEvent::Text("a")).await.unwrap();
        channel.stream_event(StreamEvent::Text("b")).await.unwrap();
        channel.stream_event(StreamEvent::Text("c")).await.unwrap();

        let guard = handle.lock().unwrap();
        assert_eq!(guard.buf, b"abc");
        assert_eq!(
            guard.flushes, 3,
            "each text chunk should trigger one flush so the user sees tokens as they arrive"
        );
    }

    // -----------------------------------------------------------------
    // TierOverride
    // -----------------------------------------------------------------

    #[test]
    fn tier_override_replaces_trust_tier_only() {
        let (channel, _) = mem_channel();
        let inner: Arc<dyn ChannelContext + Send + Sync> = Arc::new(channel);
        let wrapped = TierOverride::new(Arc::clone(&inner), TrustTier::Untrusted);

        assert_eq!(wrapped.trust_tier(), TrustTier::Untrusted);
        // Everything else is untouched — delegated straight through.
        assert_eq!(wrapped.channel_name(), inner.channel_name());
        assert_eq!(wrapped.platform(), inner.platform());
        assert_eq!(wrapped.session_id(), inner.session_id());
        assert_eq!(wrapped.session_partition(), inner.session_partition());
    }

    #[tokio::test]
    async fn tier_override_delegates_stream_event_and_finalize() {
        let (channel, handle) = mem_channel();
        let inner: Arc<dyn ChannelContext + Send + Sync> = Arc::new(channel);
        let wrapped = TierOverride::new(inner, TrustTier::Untrusted);

        wrapped
            .stream_event(StreamEvent::Text("hi"))
            .await
            .unwrap();
        wrapped
            .finalize(&TurnOutcome::Completed {
                final_message: "hi".into(),
                tool_calls_made: 0,
                duration: std::time::Duration::from_millis(1),
            })
            .await
            .unwrap();

        let out = read_output(&handle);
        assert!(out.starts_with("hi"), "text was relayed: {out:?}");
        assert!(
            out.ends_with("[turn completed]\n"),
            "finalize marker was relayed: {out:?}"
        );
    }

    #[tokio::test]
    async fn tier_override_delegates_cancellation_token() {
        let (channel, _) = mem_channel();
        let cancel = channel.cancel_handle();
        let inner: Arc<dyn ChannelContext + Send + Sync> = Arc::new(channel);
        let wrapped = TierOverride::new(inner, TrustTier::Untrusted);

        assert!(!wrapped.cancellation_token().is_cancelled());
        cancel.cancel();
        assert!(
            wrapped.cancellation_token().is_cancelled(),
            "cancellation must be visible through the wrapper"
        );
    }

    /// A minimal fake `ChannelContext` used solely to prove `TierOverride`
    /// calls through to the inner channel's `reset_cancellation` /
    /// `cancel_inflight` rather than silently falling back to the
    /// trait's own no-op defaults. `LocalChannel` itself doesn't override
    /// those two trait methods (it exposes non-trait inherent methods of
    /// the same name instead), so it can't distinguish "delegated" from
    /// "used the trait default" — this fake can.
    struct RecordingChannel {
        reset_calls: Arc<Mutex<u32>>,
        cancel_calls: Arc<Mutex<u32>>,
    }

    #[async_trait]
    impl ChannelContext for RecordingChannel {
        fn channel_name(&self) -> &str {
            "recording"
        }
        fn platform(&self) -> ChannelPlatform {
            ChannelPlatform::Local
        }
        fn trust_tier(&self) -> TrustTier {
            TrustTier::Trusted
        }
        fn session_id(&self) -> SessionId {
            SessionId::new()
        }
        async fn stream_event(&self, _event: StreamEvent<'_>) -> Result<(), ChannelError> {
            Ok(())
        }
        async fn finalize(&self, _outcome: &TurnOutcome) -> Result<(), ChannelError> {
            Ok(())
        }
        fn cancellation_token(&self) -> CancellationToken {
            CancellationToken::new()
        }
        fn reset_cancellation(&self) {
            *self.reset_calls.lock().unwrap() += 1;
        }
        fn cancel_inflight(&self) {
            *self.cancel_calls.lock().unwrap() += 1;
        }
    }

    #[test]
    fn tier_override_delegates_reset_cancellation_and_cancel_inflight() {
        let reset_calls = Arc::new(Mutex::new(0));
        let cancel_calls = Arc::new(Mutex::new(0));
        let inner: Arc<dyn ChannelContext + Send + Sync> = Arc::new(RecordingChannel {
            reset_calls: Arc::clone(&reset_calls),
            cancel_calls: Arc::clone(&cancel_calls),
        });
        let wrapped = TierOverride::new(inner, TrustTier::Untrusted);

        wrapped.reset_cancellation();
        wrapped.cancel_inflight();

        assert_eq!(
            *reset_calls.lock().unwrap(),
            1,
            "reset_cancellation must delegate to the inner channel, not the trait default"
        );
        assert_eq!(
            *cancel_calls.lock().unwrap(),
            1,
            "cancel_inflight must delegate to the inner channel, not the trait default"
        );
    }
}
