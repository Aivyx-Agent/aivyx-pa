//! Model routing Part 3b — the per-conversation routing taint and cloud
//! consent (G6).
//!
//! [`RoutingGuard`] owns two pieces of per-conversation state the cloud
//! escalation path consults:
//!
//! - **Taint** — persisted in [`aivyx_storage::KeyDomain::RoutingTaint`],
//!   keyed by the session id, holding the first recorded reason (a short
//!   label such as the sensitive tool's name, never content). Write-once
//!   and never cleared, so it survives restarts and compaction. A tainted
//!   conversation never escalates, in any mode.
//! - **Consent** — an in-memory set of sessions the operator allowed to
//!   escalate. Deliberately not persisted: a restart re-asks.
//!
//! Taint fails SAFE. Every known taint is also held in an in-memory cache
//! consulted before storage, so a failed write still treats the session as
//! tainted for this process's lifetime; a taint row that can't be read is
//! reported as tainted rather than clean.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use aivyx_llm::escalation::{ConsentRequest, Trigger, stuck_hint};
use aivyx_storage::{DomainHandle, KeyDomain, Storage};
use async_trait::async_trait;

/// Reason reported when a session's taint row exists but can't be read.
const UNREADABLE_REASON: &str = "routing taint unreadable";

/// Model routing Part 3b (A16) — the three per-session sets behind
/// `on_failure` arming, held under one lock so every transition (`arm`,
/// `begin_armed_turn`, `end_armed_turn`, `note_consent_requested`, the
/// offer half of [`RoutingGuard::allow`], and the `armed` read) is a
/// single critical section. Splitting these across separate mutexes (the
/// original shape) let a consent note that arrived after its turn's
/// `end_armed_turn` had already run leave a stale entry that a later,
/// unrelated turn would pick up — see task-3-review.md's concurrency
/// finding.
///
/// `ask`-mode consent stops leave a *pending offer*, never a re-arm
/// (operator decision, 2026-09-27): `/allow-cloud` turns the offer into
/// an armed mark, so the resend escalates exactly once; the next turn
/// starting without it declines the offer, runs locally, and no further
/// consent stops come from the old failure.
#[derive(Default)]
struct ArmState {
    /// Sessions armed for their *next* turn. A turn's start moves its
    /// session out of here into `active`; see
    /// [`RoutingGuard::begin_armed_turn`].
    armed: HashSet<String>,
    /// Sessions armed for *this* (currently running) turn.
    /// `EscalationGuard::armed` reads this set, never `armed` above —
    /// that's the whole one-shot, next-turn-only design.
    active: HashSet<String>,
    /// Sessions whose active-turn arming stopped for operator consent
    /// (`ask` mode, `NeedsConsent`): a pending offer.
    /// [`RoutingGuard::allow`] turns it into an armed mark;
    /// [`RoutingGuard::begin_armed_turn`] drops it (the offer was
    /// declined). Only ever set for a session that's currently in
    /// `active` — see [`RoutingGuard::note_consent_requested`].
    pending_offer: HashSet<String>,
}

/// Persisted routing taint + in-memory cloud consent, plus the in-memory
/// `on_failure` arming state (A16, [`ArmState`]). Implements
/// [`aivyx_llm::escalation::EscalationGuard`] (read side),
/// [`aivyx_core::TaintSink`] (write side) and
/// [`aivyx_core::EscalationArming`].
///
/// In `ask` mode a consent stop on an armed turn leaves a pending offer:
/// [`RoutingGuard::allow`] (`/allow-cloud`, from chat, IPC or the CLI)
/// arms it for the resend, and a new turn without it declines it.
///
/// **Build exactly one per process and share it** (`Arc<RoutingGuard>`)
/// with everything that marks or reads taint — the agent, its planners,
/// the escalation path. Write-once, first-reason-wins and the
/// "newly tainted" result of [`RoutingGuard::mark`] (which gates the
/// once-per-session `ConversationTainted` audit entry) all hold per
/// instance: two guards over the same storage would each report a
/// session's first mark as new, and consent granted on one would be
/// invisible to the other.
pub struct RoutingGuard {
    storage: DomainHandle,
    /// Every taint this process knows of: persisted rows it has read or
    /// written, and marks whose write failed (the fail-safe path).
    cache: Mutex<HashMap<String, String>>,
    /// Serializes `mark`'s read-then-write so the first reason wins even
    /// under concurrent marks of the same session.
    mark_lock: tokio::sync::Mutex<()>,
    consent: Mutex<HashSet<String>>,
    /// Model routing Part 3b (A16) — the armed/active/pending-offer
    /// state; see [`ArmState`].
    arm_state: Mutex<ArmState>,
    /// Model routing Part 3b (A16) — the cloud model id the `ask`-mode
    /// hint from [`RoutingGuard::arm`] names for an untainted session.
    /// `None` when there is nothing to say.
    on_failure_model: Mutex<Option<String>>,
    /// Routing visibility B1 — each session's latest cloud-consent stop,
    /// until [`RoutingGuard::take_consent_request`] hands it over or the
    /// session's next turn begins.
    consent_requests: Mutex<HashMap<String, ConsentRequest>>,
}

impl RoutingGuard {
    pub fn new(storage: Arc<dyn Storage>) -> Self {
        Self {
            storage: storage.domain(KeyDomain::RoutingTaint),
            cache: Mutex::new(HashMap::new()),
            mark_lock: tokio::sync::Mutex::new(()),
            consent: Mutex::new(HashSet::new()),
            arm_state: Mutex::new(ArmState::default()),
            on_failure_model: Mutex::new(None),
            consent_requests: Mutex::new(HashMap::new()),
        }
    }

    /// Set (or clear) the cloud model id the `ask`-mode hint from
    /// [`RoutingGuard::arm`] names for an untainted session; the hint's
    /// wording is [`aivyx_llm::escalation::stuck_hint`], per channel. In
    /// memory only.
    pub fn set_on_failure_model(&self, model: Option<String>) {
        *lock(&self.on_failure_model) = model;
    }

    /// Routing visibility B1 — hand over `session`'s latest cloud-consent
    /// stop, once: the daemon calls this after a turn to show the request
    /// per channel. `None` when the session's calls didn't stop for
    /// consent since its last turn began (or it was already taken).
    pub fn take_consent_request(&self, session: &str) -> Option<ConsentRequest> {
        lock(&self.consent_requests).remove(session)
    }

    fn cached(&self, session: &str) -> Option<String> {
        lock(&self.cache).get(session).cloned()
    }

    fn remember(&self, session: &str, reason: &str) {
        lock(&self.cache)
            .entry(session.to_owned())
            .or_insert_with(|| reason.to_owned());
    }

    /// Taint `session` with `reason`. Write-once: if the session is
    /// already tainted the first reason is kept and this is a no-op.
    /// Returns `true` only when this call newly tainted the session.
    pub async fn mark(&self, session: &str, reason: &str) -> bool {
        let _serial = self.mark_lock.lock().await;
        if self.cached(session).is_some() {
            return false;
        }
        match self.storage.get(session.as_bytes()).await {
            Ok(Some(existing)) => {
                self.remember(session, &String::from_utf8_lossy(&existing));
                false
            }
            Ok(None) => {
                // Cache first: whatever happens to the write, this
                // process treats the session as tainted.
                self.remember(session, reason);
                if let Err(e) = self.storage.put(session.as_bytes(), reason.as_bytes()).await {
                    eprintln!(
                        "aivyx-pa: routing taint for session {session} not persisted \
                         ({e}); treating it as tainted for this process's lifetime"
                    );
                }
                true
            }
            Err(e) => {
                // Can't tell whether a first reason is already on disk, so
                // don't overwrite it; hold the taint in memory instead.
                eprintln!(
                    "aivyx-pa: routing taint for session {session} unreadable \
                     ({e}); treating it as tainted for this process's lifetime"
                );
                self.remember(session, reason);
                true
            }
        }
    }

    /// The recorded taint reason for `session`, or `None` if untainted.
    /// Consults the in-memory cache first, then storage; an unreadable
    /// row reports the session as tainted.
    pub async fn taint(&self, session: &str) -> Option<String> {
        if let Some(reason) = self.cached(session) {
            return Some(reason);
        }
        match self.storage.get(session.as_bytes()).await {
            Ok(Some(reason)) => {
                let reason = String::from_utf8_lossy(&reason).into_owned();
                self.remember(session, &reason);
                Some(reason)
            }
            Ok(None) => None,
            Err(e) => {
                eprintln!(
                    "aivyx-pa: routing taint for session {session} unreadable \
                     ({e}); treating it as tainted"
                );
                Some(UNREADABLE_REASON.to_owned())
            }
        }
    }

    /// Allow cloud escalation for `session` for this process's lifetime.
    /// In-memory only; never overrides a taint.
    ///
    /// Also (A16) turns a pending `on_failure` offer — left by an `ask`
    /// turn that stopped for consent — into an armed mark, so the resend
    /// escalates exactly once. Without a pending offer it arms nothing.
    ///
    /// Lock order: the consent lock is taken and released before the
    /// `ArmState` lock; the two are never held together, here or anywhere
    /// else in this type, so there is no ordering to invert.
    pub fn allow(&self, session: &str) {
        lock(&self.consent).insert(session.to_owned());
        let mut state = lock(&self.arm_state);
        if state.pending_offer.remove(session) {
            state.armed.insert(session.to_owned());
        }
    }

    /// Has the operator allowed cloud escalation for `session`?
    pub fn consented(&self, session: &str) -> bool {
        lock(&self.consent).contains(session)
    }
}

/// Model routing Part 3b — the chat command that allows cloud escalation
/// for the conversation it's sent in. Only the whole message counts.
pub const ALLOW_CLOUD_COMMAND: &str = "/allow-cloud";

/// First-run coherence A1 (F14) — the reply an in-process session (no
/// daemon) gives a whole-message `/allow-cloud` instead of sending it to the
/// model: escalation consent lives in the daemon.
pub const IN_PROCESS_ALLOW_CLOUD_REPLY: &str =
    "Cloud escalation needs the daemon — start it with `aivyx-pa daemon run` (or store your \
     passphrase so aivyx-pa starts it), then allow it there.";

/// Is `text` exactly the `/allow-cloud` command (surrounding whitespace
/// aside)? Anything else is a normal turn.
pub fn is_allow_cloud_command(text: &str) -> bool {
    text.trim() == ALLOW_CLOUD_COMMAND
}

/// Grants cloud-escalation consent for `session` when escalation is
/// configured (`guard` is `Some`) and the request comes from the operator
/// (`trusted`: a Trusted-tier channel, the local IPC socket or the CLI),
/// and returns whether it was recorded and the reply. Consent is in-memory
/// and never clears a taint.
pub fn allow_cloud_reply(
    guard: Option<&RoutingGuard>,
    session: &str,
    trusted: bool,
) -> (bool, &'static str) {
    match guard {
        Some(_) if !trusted => (
            false,
            "Cloud escalation can only be allowed by the operator — from a trusted frontend, or \
             `aivyx-pa routing allow-cloud <session>`.",
        ),
        Some(guard) => {
            guard.allow(session);
            (
                true,
                "Cloud escalation allowed for this conversation (until the daemon restarts). \
                 Resend your message.",
            )
        }
        None => (
            false,
            "Cloud escalation is not enabled — it needs a cloud [routing.endpoints] entry and \
             [routing.escalation] mode \"ask\" or \"auto\".",
        ),
    }
}

/// Lock a std mutex, recovering from poisoning. Every mutation this module
/// makes to a guarded set (or `ArmState`) is a single, non-interruptible
/// call under the lock, so a panicked holder can't leave one half-updated.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[async_trait]
impl aivyx_llm::escalation::EscalationGuard for RoutingGuard {
    async fn taint(&self, session: &str) -> Option<String> {
        RoutingGuard::taint(self, session).await
    }

    fn consented(&self, session: &str) -> bool {
        RoutingGuard::consented(self, session)
    }

    fn armed(&self, session: &str) -> bool {
        // The *active* set (this turn), never `armed` (next turn) — see
        // the doc on `ArmState::active`.
        lock(&self.arm_state).active.contains(session)
    }

    fn armed_next(&self, session: &str) -> bool {
        // The *armed* set (next turn), never `active` (this turn) — the
        // other half of the split `armed` reads; see `ArmState::armed`.
        lock(&self.arm_state).armed.contains(session)
    }

    fn note_consent_requested(&self, session: &str, request: &ConsentRequest) {
        // Routing visibility B1 — keep the request for the front end (the
        // latest wins). Its own lock, released before the `ArmState` one:
        // the two are never held together.
        lock(&self.consent_requests).insert(session.to_owned(), request.clone());
        if request.trigger != Trigger::OnFailure.name() {
            // Only an `on_failure` stop is an offer `/allow-cloud` arms.
            return;
        }
        // Records a pending offer, only while the session is still active
        // this turn — an OnFailure consent request can only come from an
        // active-turn call. This is what keeps a late-arriving note (one
        // that lands after `end_armed_turn` already ran for this turn)
        // from leaving an offer a later `/allow-cloud` would arm.
        let mut state = lock(&self.arm_state);
        if state.active.contains(session) {
            state.pending_offer.insert(session.to_owned());
        }
    }
}

#[async_trait]
impl aivyx_core::TaintSink for RoutingGuard {
    async fn mark(&self, session: &str, reason: &str) -> bool {
        RoutingGuard::mark(self, session, reason).await
    }
}

#[async_trait]
impl aivyx_core::EscalationArming for RoutingGuard {
    /// Arm `session`'s next turn. `newly` reports whether this call
    /// inserted a fresh mark (it wasn't already armed); the hint is
    /// returned only when the session is untainted (a tainted session
    /// never escalates, so there is nothing to ask about), worded for
    /// whether the channel `can_allow_here`.
    async fn arm(
        &self,
        session: &str,
        _signal: &str,
        can_allow_here: bool,
    ) -> (bool, Option<String>) {
        // Taint check first, outside the lock — it's the only `.await` in
        // this method and a std mutex must never be held across one.
        let untainted = RoutingGuard::taint(self, session).await.is_none();
        let newly = lock(&self.arm_state).armed.insert(session.to_owned());
        let hint = if untainted {
            lock(&self.on_failure_model)
                .as_deref()
                .map(|model| stuck_hint(model, can_allow_here))
        } else {
            None
        };
        (newly, hint)
    }

    /// Turn start: an armed mark becomes this turn's active mark. A
    /// session armed mid-turn (i.e. not present in `armed` at this call)
    /// is left alone — it will be picked up by the *next* `begin`. Any
    /// pending consent offer is dropped: a new turn without `/allow-cloud`
    /// first means the operator declined it.
    fn begin_armed_turn(&self, session: &str) {
        // Routing visibility B1 — an untaken consent request belongs to an
        // earlier turn; it must never answer for this one.
        lock(&self.consent_requests).remove(session);
        let mut state = lock(&self.arm_state);
        state.pending_offer.remove(session);
        if state.armed.remove(session) {
            state.active.insert(session.to_owned());
        }
    }

    /// Turn end (every exit): clear the active mark, unconditionally.
    /// Never re-arms: a consent stop leaves a pending offer (see
    /// [`RoutingGuard::allow`]), which only `/allow-cloud` turns back into
    /// an armed mark.
    fn end_armed_turn(&self, session: &str) {
        lock(&self.arm_state).active.remove(session);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_core::TaintSink;
    use aivyx_crypto::MasterKey;
    use aivyx_llm::escalation::{ConsentRequest, EscalationGuard};
    use aivyx_storage::{RedbStorage, StorageConfig};
    use std::path::PathBuf;

    struct Scratch {
        dir: PathBuf,
    }
    impl Scratch {
        fn new() -> Self {
            let tmp = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into());
            let dir = PathBuf::from(tmp)
                .join(format!("aivyx-routing-guard-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).unwrap();
            Scratch { dir }
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    async fn open_storage(scratch: &Scratch, key: u8) -> Arc<dyn Storage> {
        RedbStorage::open(
            StorageConfig::new(scratch.dir.join("s.redb")),
            MasterKey::from_raw([key; 32]),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn unmarked_session_is_untainted() {
        let scratch = Scratch::new();
        let guard = RoutingGuard::new(open_storage(&scratch, 7).await);
        assert_eq!(guard.taint("s1").await, None);
    }

    #[tokio::test]
    async fn mark_records_the_reason() {
        let scratch = Scratch::new();
        let guard = RoutingGuard::new(open_storage(&scratch, 7).await);
        assert!(guard.mark("s1", "tool:gmail.read").await, "first mark is new");
        assert_eq!(guard.taint("s1").await.as_deref(), Some("tool:gmail.read"));
        assert_eq!(guard.taint("s2").await, None, "taint is per session");
    }

    #[tokio::test]
    async fn second_mark_keeps_the_first_reason() {
        let scratch = Scratch::new();
        let guard = RoutingGuard::new(open_storage(&scratch, 7).await);
        assert!(guard.mark("s1", "first").await);
        assert!(!guard.mark("s1", "second").await, "second mark is a no-op");
        assert_eq!(guard.taint("s1").await.as_deref(), Some("first"));
    }

    #[tokio::test]
    async fn taint_survives_a_new_guard_over_the_same_storage() {
        let scratch = Scratch::new();
        let storage = open_storage(&scratch, 7).await;
        let guard = RoutingGuard::new(storage.clone());
        guard.mark("s1", "first").await;
        drop(guard);

        let guard = RoutingGuard::new(storage);
        assert_eq!(guard.taint("s1").await.as_deref(), Some("first"));
        assert!(
            !guard.mark("s1", "second").await,
            "a persisted taint is not re-marked after a restart"
        );
        assert_eq!(guard.taint("s1").await.as_deref(), Some("first"));
    }

    #[tokio::test]
    async fn taint_survives_a_store_reopen() {
        let scratch = Scratch::new();
        {
            let guard = RoutingGuard::new(open_storage(&scratch, 7).await);
            guard.mark("s1", "first").await;
        }
        let guard = RoutingGuard::new(open_storage(&scratch, 7).await);
        assert_eq!(guard.taint("s1").await.as_deref(), Some("first"));
    }

    #[tokio::test]
    async fn consent_is_per_session_and_not_persisted() {
        let scratch = Scratch::new();
        let storage = open_storage(&scratch, 7).await;
        let guard = RoutingGuard::new(storage.clone());
        assert!(!guard.consented("s1"));
        guard.allow("s1");
        assert!(guard.consented("s1"));
        assert!(!guard.consented("s2"), "consent is per session");

        let guard = RoutingGuard::new(storage);
        assert!(!guard.consented("s1"), "a restart re-asks");
    }

    #[tokio::test]
    async fn consent_does_not_clear_taint() {
        let scratch = Scratch::new();
        let guard = RoutingGuard::new(open_storage(&scratch, 7).await);
        guard.mark("s1", "first").await;
        guard.allow("s1");
        assert_eq!(guard.taint("s1").await.as_deref(), Some("first"));
    }

    #[tokio::test]
    async fn unreadable_taint_fails_safe_and_keeps_the_first_reason() {
        // A row sealed under one key is unreadable (DecryptFailed) under
        // another — the practical way to make storage fail here.
        let scratch = Scratch::new();
        {
            let guard = RoutingGuard::new(open_storage(&scratch, 7).await);
            guard.mark("s1", "first").await;
        }
        {
            let guard = RoutingGuard::new(open_storage(&scratch, 9).await);
            assert_eq!(
                guard.taint("s1").await.as_deref(),
                Some(UNREADABLE_REASON),
                "an unreadable row reads as tainted, never clean"
            );
            // Marking over an unreadable row holds the taint in memory
            // but must not overwrite the first reason on disk.
            assert!(guard.mark("s1", "second").await);
            assert_eq!(guard.taint("s1").await.as_deref(), Some("second"));
        }
        let guard = RoutingGuard::new(open_storage(&scratch, 7).await);
        assert_eq!(guard.taint("s1").await.as_deref(), Some("first"));
    }

    #[tokio::test]
    async fn usable_through_both_traits() {
        let scratch = Scratch::new();
        let guard = Arc::new(RoutingGuard::new(open_storage(&scratch, 7).await));
        let sink: Arc<dyn TaintSink> = guard.clone();
        let read: Arc<dyn EscalationGuard> = guard.clone();
        assert!(sink.mark("s1", "first").await);
        assert!(!sink.mark("s1", "second").await);
        assert_eq!(read.taint("s1").await.as_deref(), Some("first"));
        guard.allow("s1");
        assert!(read.consented("s1"));
    }

    #[test]
    fn allow_cloud_is_the_whole_message_only() {
        assert!(is_allow_cloud_command("/allow-cloud"));
        assert!(is_allow_cloud_command("  /allow-cloud\n"));
        assert!(!is_allow_cloud_command("/allow-cloud please"));
        assert!(!is_allow_cloud_command("please /allow-cloud"));
        assert!(!is_allow_cloud_command("/ALLOW-CLOUD"));
        assert!(!is_allow_cloud_command("allow-cloud"));
    }

    // ---- Model routing Part 3b (A16) — EscalationArming state ----

    use aivyx_core::EscalationArming;

    #[tokio::test]
    async fn arm_begin_end_is_one_shot() {
        let scratch = Scratch::new();
        let guard = RoutingGuard::new(open_storage(&scratch, 7).await);

        assert!(guard.arm("s1", "looping", true).await.0);
        guard.begin_armed_turn("s1");
        assert!(guard.armed("s1"), "active this turn");

        guard.end_armed_turn("s1");
        assert!(!guard.armed("s1"), "cleared at turn end");

        guard.begin_armed_turn("s1");
        assert!(
            !guard.armed("s1"),
            "one-shot: a begin with nothing armed doesn't activate"
        );
    }

    #[tokio::test]
    async fn armed_next_reads_the_armed_set_not_the_active_one() {
        let scratch = Scratch::new();
        let guard = RoutingGuard::new(open_storage(&scratch, 7).await);

        assert!(!guard.armed_next("s1"), "unarmed session");
        assert!(guard.arm("s1", "looping", true).await.0);
        assert!(guard.armed_next("s1"), "armed for its next turn");
        assert!(!guard.armed("s1"), "not active yet — the turn hasn't begun");

        guard.begin_armed_turn("s1");
        assert!(guard.armed("s1"), "now active this turn");
        assert!(
            !guard.armed_next("s1"),
            "moved out of the armed set into active — one-shot"
        );

        guard.end_armed_turn("s1");
        assert!(!guard.armed("s1"));
        assert!(!guard.armed_next("s1"), "cleared, not re-armed");
    }

    // ---- I3 (operator decision 2026-09-27): an ignored `ask` offer
    // lapses at the next turn ----

    /// A failing turn armed `s1`, and the following turn (sent without
    /// `/allow-cloud`) stopped for consent. Leaves `s1` with a pending
    /// offer and nothing armed.
    async fn fail_then_consent_stop(guard: &RoutingGuard) {
        assert!(guard.arm("s1", "looping", true).await.0);
        guard.begin_armed_turn("s1");
        assert!(guard.armed("s1"), "the turn after the failure is escalated");
        guard.note_consent_requested("s1", &on_failure_request());
        guard.end_armed_turn("s1");
    }

    #[tokio::test]
    async fn a_consent_stop_leaves_a_pending_offer_not_an_armed_mark() {
        let scratch = Scratch::new();
        let guard = RoutingGuard::new(open_storage(&scratch, 7).await);
        fail_then_consent_stop(&guard).await;
        assert!(!guard.armed("s1"), "not active between turns");
        assert!(
            !guard.armed_next("s1"),
            "a consent stop no longer re-arms the next turn"
        );
    }

    #[tokio::test]
    async fn allow_after_a_consent_stop_escalates_the_resend_exactly_once() {
        let scratch = Scratch::new();
        let guard = RoutingGuard::new(open_storage(&scratch, 7).await);
        fail_then_consent_stop(&guard).await;

        guard.allow("s1");
        assert!(guard.consented("s1"));
        assert!(guard.armed_next("s1"), "allow turns the pending offer into an armed mark");

        // The resend.
        guard.begin_armed_turn("s1");
        assert!(guard.armed("s1"), "the resend escalates");
        guard.end_armed_turn("s1");

        // The turn after it.
        guard.begin_armed_turn("s1");
        assert!(!guard.armed("s1"), "the turn after the resend is local");
        guard.end_armed_turn("s1");
        assert!(!guard.armed_next("s1"));
    }

    #[tokio::test]
    async fn an_ordinary_turn_after_a_consent_stop_declines_the_offer() {
        let scratch = Scratch::new();
        let guard = RoutingGuard::new(open_storage(&scratch, 7).await);
        fail_then_consent_stop(&guard).await;

        // The operator ignores the offer and sends a normal message.
        guard.begin_armed_turn("s1");
        assert!(!guard.armed("s1"), "not escalated, so no consent stop either");
        // A consent note can't arrive for an inactive session, but if one
        // did it must not revive the offer.
        guard.note_consent_requested("s1", &on_failure_request());
        guard.end_armed_turn("s1");
        assert!(!guard.armed_next("s1"));

        // The offer is gone: a later allow only records consent.
        guard.allow("s1");
        assert!(guard.consented("s1"));
        assert!(!guard.armed_next("s1"), "the declined offer can't be revived");
        guard.begin_armed_turn("s1");
        assert!(!guard.armed("s1"));
    }

    #[tokio::test]
    async fn allow_without_a_pending_offer_only_records_consent() {
        let scratch = Scratch::new();
        let guard = RoutingGuard::new(open_storage(&scratch, 7).await);
        guard.allow("s1");
        assert!(guard.consented("s1"));
        assert!(!guard.armed_next("s1"), "nothing to arm");
        guard.begin_armed_turn("s1");
        assert!(!guard.armed("s1"));
    }

    #[tokio::test]
    async fn allow_after_a_failure_keeps_the_existing_mark_for_the_resend() {
        // The happy path the hint describes: fail, `/allow-cloud`, resend.
        let scratch = Scratch::new();
        let guard = RoutingGuard::new(open_storage(&scratch, 7).await);
        assert!(guard.arm("s1", "looping", true).await.0);
        guard.allow("s1");
        assert!(guard.armed_next("s1"));
        guard.begin_armed_turn("s1");
        assert!(guard.armed("s1"), "the resend escalates");
        guard.end_armed_turn("s1");
        guard.begin_armed_turn("s1");
        assert!(!guard.armed("s1"), "once");
    }

    #[tokio::test]
    async fn allow_through_the_reply_helper_arms_a_pending_offer() {
        // `/allow-cloud` (chat intercept and IPC) reaches the guard via
        // `allow_cloud_reply`.
        let scratch = Scratch::new();
        let guard = RoutingGuard::new(open_storage(&scratch, 7).await);
        fail_then_consent_stop(&guard).await;
        assert!(allow_cloud_reply(Some(&guard), "s1", true).0);
        assert!(guard.armed_next("s1"));
    }

    #[tokio::test]
    async fn an_untrusted_allow_does_not_arm_a_pending_offer() {
        let scratch = Scratch::new();
        let guard = RoutingGuard::new(open_storage(&scratch, 7).await);
        fail_then_consent_stop(&guard).await;
        assert!(!allow_cloud_reply(Some(&guard), "s1", false).0);
        assert!(!guard.armed_next("s1"));
        assert!(!guard.consented("s1"));
    }

    #[tokio::test]
    async fn arm_reports_newly_false_on_a_second_arm() {
        let scratch = Scratch::new();
        let guard = RoutingGuard::new(open_storage(&scratch, 7).await);

        assert!(guard.arm("s1", "looping", true).await.0, "first arm is new");
        assert!(
            !guard.arm("s1", "loop_verdict_fail", true).await.0,
            "second arm before it's consumed is not new"
        );
    }

    #[tokio::test]
    async fn arm_hint_is_returned_only_when_untainted() {
        let scratch = Scratch::new();
        let guard = RoutingGuard::new(open_storage(&scratch, 7).await);
        guard.set_on_failure_model(Some("cloud-m".to_string()));

        let (_, hint) = guard.arm("clean", "looping", true).await;
        assert_eq!(
            hint.as_deref(),
            Some("The local model got stuck; send /allow-cloud and resend to retry on `cloud-m`.")
        );

        guard.mark("tainted", "gmail.search output").await;
        let (_, hint) = guard.arm("tainted", "looping", true).await;
        assert_eq!(hint, None, "a tainted session gets no hint");
    }

    #[tokio::test]
    async fn the_arm_hint_is_per_channel() {
        // Routing visibility B1 — a channel that can't grant consent is
        // told who can, not invited to send `/allow-cloud`.
        let scratch = Scratch::new();
        let guard = RoutingGuard::new(open_storage(&scratch, 7).await);
        guard.set_on_failure_model(Some("cloud-m".to_string()));

        let (_, hint) = guard.arm("bot", "looping", false).await;
        assert_eq!(
            hint.as_deref(),
            Some(
                "The local model got stuck; it could retry on `cloud-m`. Cloud use can only be \
                 allowed by the operator — from the terminal (`/allow-cloud`) or the Studio."
            )
        );

        guard.set_on_failure_model(None);
        assert_eq!(guard.arm("other", "looping", true).await.1, None, "no cloud model, no hint");
    }

    // ---- Routing visibility B1 — the consent request ----

    fn on_failure_request() -> ConsentRequest {
        ConsentRequest {
            model: "cloud-m".to_string(),
            endpoint: "claude".to_string(),
            trigger: "on_failure".to_string(),
            estimated_tokens: 42,
        }
    }

    fn tier_request(tokens: u32) -> ConsentRequest {
        ConsentRequest {
            model: "cloud-m".to_string(),
            endpoint: "claude".to_string(),
            trigger: "tier".to_string(),
            estimated_tokens: tokens,
        }
    }

    #[tokio::test]
    async fn the_guard_keeps_the_latest_consent_request_and_hands_it_over_once() {
        let scratch = Scratch::new();
        let guard = RoutingGuard::new(open_storage(&scratch, 7).await);
        assert_eq!(guard.take_consent_request("s1"), None);

        guard.note_consent_requested("s1", &tier_request(10));
        guard.note_consent_requested("s1", &tier_request(20));
        guard.note_consent_requested("s2", &tier_request(30));

        assert_eq!(guard.take_consent_request("s1"), Some(tier_request(20)), "the latest");
        assert_eq!(guard.take_consent_request("s1"), None, "handed over once");
        assert_eq!(guard.take_consent_request("s2"), Some(tier_request(30)), "per session");
    }

    #[tokio::test]
    async fn a_non_on_failure_consent_stop_is_no_pending_offer() {
        // Only `on_failure`'s stop is an offer `/allow-cloud` arms; a tier
        // stop on an armed turn records the request and nothing else.
        let scratch = Scratch::new();
        let guard = RoutingGuard::new(open_storage(&scratch, 7).await);
        assert!(guard.arm("s1", "looping", true).await.0);
        guard.begin_armed_turn("s1");
        guard.note_consent_requested("s1", &tier_request(10));
        guard.end_armed_turn("s1");
        guard.allow("s1");
        assert!(!guard.armed_next("s1"), "no offer to arm");
        assert_eq!(guard.take_consent_request("s1"), Some(tier_request(10)));
    }

    #[tokio::test]
    async fn an_on_failure_consent_stop_still_leaves_a_pending_offer() {
        let scratch = Scratch::new();
        let guard = RoutingGuard::new(open_storage(&scratch, 7).await);
        fail_then_consent_stop(&guard).await;
        assert_eq!(guard.take_consent_request("s1"), Some(on_failure_request()));
        guard.allow("s1");
        assert!(guard.armed_next("s1"), "taking the request leaves the offer alone");
    }

    #[tokio::test]
    async fn a_new_turn_drops_an_untaken_consent_request() {
        let scratch = Scratch::new();
        let guard = RoutingGuard::new(open_storage(&scratch, 7).await);
        guard.note_consent_requested("s1", &tier_request(10));
        guard.begin_armed_turn("s1");
        assert_eq!(
            guard.take_consent_request("s1"),
            None,
            "a stale request never answers for a later turn"
        );
    }

    #[tokio::test]
    async fn arming_during_an_active_turn_survives_to_the_next_begin() {
        let scratch = Scratch::new();
        let guard = RoutingGuard::new(open_storage(&scratch, 7).await);

        assert!(guard.arm("s1", "looping", true).await.0);
        guard.begin_armed_turn("s1");
        assert!(guard.armed("s1"), "current turn is active");

        // Arm again while this turn is still active — it must not touch
        // the current turn's active mark.
        assert!(
            guard.arm("s1", "tool_call_repair_exhausted", true).await.0,
            "a fresh mark while active is still newly armed"
        );
        assert!(guard.armed("s1"), "current turn stays active, not doubled");

        guard.end_armed_turn("s1");
        assert!(!guard.armed("s1"), "cleared between turns");

        guard.begin_armed_turn("s1");
        assert!(
            guard.armed("s1"),
            "the mark made during the active turn survives to the next begin"
        );
    }

    #[tokio::test]
    async fn note_consent_requested_on_an_inactive_session_leaves_no_offer() {
        let scratch = Scratch::new();
        let guard = RoutingGuard::new(open_storage(&scratch, 7).await);

        // "s1" was never armed or begun — it's not active.
        guard.note_consent_requested("s1", &on_failure_request());
        guard.end_armed_turn("s1");
        guard.allow("s1");
        guard.begin_armed_turn("s1");
        assert!(
            !guard.armed("s1"),
            "a consent note for a session that isn't active must not leave an offer to arm"
        );
    }

    #[tokio::test]
    async fn a_consent_note_that_arrives_after_turn_end_does_not_arm_a_later_turn() {
        // Regression test for the reviewer's interleaving
        // (task-3-review.md): `end_armed_turn` and `note_consent_requested`
        // used to touch their sets under separate locks, so a consent
        // note that lands *after* its turn already ended could leave a
        // stale entry for a later turn to act on. With every transition
        // serialized under one lock and `note_consent_requested` only
        // recording a pending offer while the session is still active, a
        // late note is a no-op: not even `/allow-cloud` arms from it.
        let scratch = Scratch::new();
        let guard = RoutingGuard::new(open_storage(&scratch, 7).await);

        // Turn A: armed, active, ends normally (no consent involved).
        assert!(guard.arm("s1", "looping", true).await.0);
        guard.begin_armed_turn("s1");
        guard.end_armed_turn("s1");

        // A consent note for turn A arrives late, after turn A already
        // ended — the race the reviewer described. It must not stick:
        // the session is no longer active.
        guard.note_consent_requested("s1", &on_failure_request());

        // Turn B: an unrelated, later exit for the same session, with no
        // arming of its own. It must not be spuriously re-armed by the
        // stale note.
        guard.end_armed_turn("s1");
        guard.allow("s1");
        guard.begin_armed_turn("s1");
        assert!(
            !guard.armed("s1"),
            "a late consent note for an already-ended turn must not leave an offer to arm"
        );
    }

    #[tokio::test]
    async fn allow_cloud_reply_records_consent_only_when_enabled() {
        let scratch = Scratch::new();
        let guard = RoutingGuard::new(open_storage(&scratch, 1).await);
        let (granted, reply) = allow_cloud_reply(Some(&guard), "s", true);
        assert!(granted);
        assert!(guard.consented("s"));
        assert!(reply.contains("allowed"), "{reply}");
        assert!(reply.contains("Resend"), "{reply}");

        let (granted, reply) = allow_cloud_reply(None, "s", true);
        assert!(!granted);
        assert!(reply.contains("not enabled"), "{reply}");
    }
}
