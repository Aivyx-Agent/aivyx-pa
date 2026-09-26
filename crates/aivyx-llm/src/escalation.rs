//! Model routing Part 3b — consent-gated cloud escalation.
//!
//! [`EscalationGuard`] is the read side of the per-conversation privacy
//! state the escalation path consults before any call leaves the machine
//! for a `[routing.endpoints.*]` cloud endpoint (G6): the persisted,
//! write-once routing taint and the in-memory, per-conversation consent
//! grant. The daemon implements it with
//! `aivyx_channel::routing_guard::RoutingGuard`; the write side is
//! `aivyx_core::TaintSink`.

use async_trait::async_trait;

/// Per-conversation escalation state, read-only.
#[async_trait]
pub trait EscalationGuard: Send + Sync {
    /// The recorded taint reason for `session`, or `None` if the
    /// conversation is untainted. A tainted conversation never escalates,
    /// in any mode. Implementations fail safe: if the taint state cannot
    /// be read, they report the session as tainted.
    async fn taint(&self, session: &str) -> Option<String>;

    /// Has the operator allowed cloud escalation for `session` in this
    /// process's lifetime? Consent is in-memory only (a restart re-asks)
    /// and never overrides a taint.
    fn consented(&self, session: &str) -> bool;
}
