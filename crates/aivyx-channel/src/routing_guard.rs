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

use aivyx_storage::{DomainHandle, KeyDomain, Storage};
use async_trait::async_trait;

/// Reason reported when a session's taint row exists but can't be read.
const UNREADABLE_REASON: &str = "routing taint unreadable";

/// Persisted routing taint + in-memory cloud consent. Implements
/// [`aivyx_llm::escalation::EscalationGuard`] (read side) and
/// [`aivyx_core::TaintSink`] (write side).
///
/// **Build exactly one per process and share it** (`Arc<RoutingGuard>`)
/// with everything that marks or reads taint — the agent, its planners,
/// the escalation path. Write-once, first-reason-wins and the
/// "newly tainted" result of [`RoutingGuard::mark`] (which gates the
/// once-per-session `ConversationTainted` audit entry) all hold per
/// instance: two guards over the same storage would each report a
/// session's first mark as new, and consent granted on one would be
/// invisible to the other.
/// Model routing Part 3b (A16) — the three per-session sets behind
/// `on_failure` arming, held under one lock so every transition (`arm`,
/// `begin_armed_turn`, `end_armed_turn`, `note_consent_requested`, and the
/// `armed` read) is a single critical section. Splitting these across
/// separate mutexes (the original shape) let a consent note that arrived
/// after its turn's `end_armed_turn` had already run leave a stale
/// `consent_requested` entry that a later, unrelated turn's
/// `end_armed_turn` would pick up and use to spuriously re-arm — see
/// task-3-review.md's concurrency finding.
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
    /// (`ask` mode, `NeedsConsent`), so [`RoutingGuard::end_armed_turn`]
    /// knows to re-arm rather than clear. Only ever set for a session
    /// that's currently in `active` — see
    /// [`RoutingGuard::note_consent_requested`].
    consent_requested: HashSet<String>,
}

pub struct RoutingGuard {
    storage: DomainHandle,
    /// Every taint this process knows of: persisted rows it has read or
    /// written, and marks whose write failed (the fail-safe path).
    cache: Mutex<HashMap<String, String>>,
    /// Serializes `mark`'s read-then-write so the first reason wins even
    /// under concurrent marks of the same session.
    mark_lock: tokio::sync::Mutex<()>,
    consent: Mutex<HashSet<String>>,
    /// Model routing Part 3b (A16) — the armed/active/consent-requested
    /// state; see [`ArmState`].
    arm_state: Mutex<ArmState>,
    /// Model routing Part 3b (A16) — the `ask`-mode hint line handed back
    /// from [`RoutingGuard::arm`] to an untainted session. `None` when
    /// there is nothing to say.
    arm_hint: Mutex<Option<String>>,
}

impl RoutingGuard {
    pub fn new(storage: Arc<dyn Storage>) -> Self {
        Self {
            storage: storage.domain(KeyDomain::RoutingTaint),
            cache: Mutex::new(HashMap::new()),
            mark_lock: tokio::sync::Mutex::new(()),
            consent: Mutex::new(HashSet::new()),
            arm_state: Mutex::new(ArmState::default()),
            arm_hint: Mutex::new(None),
        }
    }

    /// Set (or clear) the `ask`-mode hint line [`RoutingGuard::arm`] hands
    /// back for an untainted session. In memory only.
    pub fn set_arm_hint(&self, hint: Option<String>) {
        *lock(&self.arm_hint) = hint;
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
    pub fn allow(&self, session: &str) {
        lock(&self.consent).insert(session.to_owned());
    }

    /// Has the operator allowed cloud escalation for `session`?
    pub fn consented(&self, session: &str) -> bool {
        lock(&self.consent).contains(session)
    }
}

/// Model routing Part 3b — the chat command that allows cloud escalation
/// for the conversation it's sent in. Only the whole message counts.
pub const ALLOW_CLOUD_COMMAND: &str = "/allow-cloud";

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

    fn note_consent_requested(&self, session: &str) {
        // Only recorded while the session is still active this turn — an
        // OnFailure consent request can only come from an active-turn
        // call. This is what keeps a late-arriving note (one that lands
        // after `end_armed_turn` already ran for this turn) from sticking
        // around to misdirect a later, unrelated turn's exit.
        let mut state = lock(&self.arm_state);
        if state.active.contains(session) {
            state.consent_requested.insert(session.to_owned());
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
    /// never escalates, so there is nothing to ask about).
    async fn arm(&self, session: &str, _signal: &str) -> (bool, Option<String>) {
        // Taint check first, outside the lock — it's the only `.await` in
        // this method and a std mutex must never be held across one.
        let untainted = RoutingGuard::taint(self, session).await.is_none();
        let newly = lock(&self.arm_state).armed.insert(session.to_owned());
        let hint = if untainted {
            lock(&self.arm_hint).clone()
        } else {
            None
        };
        (newly, hint)
    }

    /// Turn start: an armed mark becomes this turn's active mark. A
    /// session armed mid-turn (i.e. not present in `armed` at this call)
    /// is left alone — it will be picked up by the *next* `begin`.
    fn begin_armed_turn(&self, session: &str) {
        let mut state = lock(&self.arm_state);
        if state.armed.remove(session) {
            state.active.insert(session.to_owned());
        }
    }

    /// Turn end (every exit): clear the active mark and the consent note,
    /// unconditionally. If the turn stopped for operator consent, re-arm
    /// instead of just clearing, so the next turn picks the mark back up.
    fn end_armed_turn(&self, session: &str) {
        let mut state = lock(&self.arm_state);
        let was_consent_requested = state.consent_requested.remove(session);
        state.active.remove(session);
        if was_consent_requested {
            state.armed.insert(session.to_owned());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_core::TaintSink;
    use aivyx_crypto::MasterKey;
    use aivyx_llm::escalation::EscalationGuard;
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

        assert!(guard.arm("s1", "looping").await.0);
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
    async fn consent_requested_re_arms_at_turn_end() {
        let scratch = Scratch::new();
        let guard = RoutingGuard::new(open_storage(&scratch, 7).await);

        assert!(guard.arm("s1", "looping").await.0);
        guard.begin_armed_turn("s1");
        assert!(guard.armed("s1"));

        guard.note_consent_requested("s1");
        guard.end_armed_turn("s1");
        assert!(!guard.armed("s1"), "not active between turns");

        guard.begin_armed_turn("s1");
        assert!(guard.armed("s1"), "re-armed by the consent-requested exit");
    }

    #[tokio::test]
    async fn arm_reports_newly_false_on_a_second_arm() {
        let scratch = Scratch::new();
        let guard = RoutingGuard::new(open_storage(&scratch, 7).await);

        assert!(guard.arm("s1", "looping").await.0, "first arm is new");
        assert!(
            !guard.arm("s1", "loop_verdict_fail").await.0,
            "second arm before it's consumed is not new"
        );
    }

    #[tokio::test]
    async fn arm_hint_is_returned_only_when_untainted() {
        let scratch = Scratch::new();
        let guard = RoutingGuard::new(open_storage(&scratch, 7).await);
        guard.set_arm_hint(Some("The local model got stuck; send /allow-cloud".to_string()));

        let (_, hint) = guard.arm("clean", "looping").await;
        assert_eq!(hint.as_deref(), Some("The local model got stuck; send /allow-cloud"));

        guard.mark("tainted", "gmail.search output").await;
        let (_, hint) = guard.arm("tainted", "looping").await;
        assert_eq!(hint, None, "a tainted session gets no hint");
    }

    #[tokio::test]
    async fn arming_during_an_active_turn_survives_to_the_next_begin() {
        let scratch = Scratch::new();
        let guard = RoutingGuard::new(open_storage(&scratch, 7).await);

        assert!(guard.arm("s1", "looping").await.0);
        guard.begin_armed_turn("s1");
        assert!(guard.armed("s1"), "current turn is active");

        // Arm again while this turn is still active — it must not touch
        // the current turn's active mark.
        assert!(
            guard.arm("s1", "tool_call_repair_exhausted").await.0,
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
    async fn note_consent_requested_on_an_inactive_session_is_a_no_op() {
        let scratch = Scratch::new();
        let guard = RoutingGuard::new(open_storage(&scratch, 7).await);

        // "s1" was never armed or begun — it's not active.
        guard.note_consent_requested("s1");
        guard.end_armed_turn("s1");
        guard.begin_armed_turn("s1");
        assert!(
            !guard.armed("s1"),
            "a consent note for a session that isn't active must not arm anything"
        );
    }

    #[tokio::test]
    async fn a_consent_note_that_arrives_after_turn_end_does_not_arm_a_later_turn() {
        // Regression test for the reviewer's interleaving
        // (task-3-review.md): `end_armed_turn` and `note_consent_requested`
        // used to touch `consent_requested`/`active`/`armed` under
        // separate locks, so a consent note that lands *after* its turn
        // already ended could leave a stale `consent_requested` entry
        // that a later, unrelated turn's `end_armed_turn` picked up and
        // used to spuriously re-arm. With every transition serialized
        // under one lock and `note_consent_requested` only recording
        // while the session is still active, a late note is a no-op.
        let scratch = Scratch::new();
        let guard = RoutingGuard::new(open_storage(&scratch, 7).await);

        // Turn A: armed, active, ends normally (no consent involved).
        assert!(guard.arm("s1", "looping").await.0);
        guard.begin_armed_turn("s1");
        guard.end_armed_turn("s1");

        // A consent note for turn A arrives late, after turn A already
        // ended — the race the reviewer described. It must not stick:
        // the session is no longer active.
        guard.note_consent_requested("s1");

        // Turn B: an unrelated, later exit for the same session, with no
        // arming of its own. It must not be spuriously re-armed by the
        // stale note.
        guard.end_armed_turn("s1");
        guard.begin_armed_turn("s1");
        assert!(
            !guard.armed("s1"),
            "a late consent note for an already-ended turn must not re-arm a later turn"
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
