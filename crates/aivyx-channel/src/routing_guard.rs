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
pub struct RoutingGuard {
    storage: DomainHandle,
    /// Every taint this process knows of: persisted rows it has read or
    /// written, and marks whose write failed (the fail-safe path).
    cache: Mutex<HashMap<String, String>>,
    /// Serializes `mark`'s read-then-write so the first reason wins even
    /// under concurrent marks of the same session.
    mark_lock: tokio::sync::Mutex<()>,
    consent: Mutex<HashSet<String>>,
}

impl RoutingGuard {
    pub fn new(storage: Arc<dyn Storage>) -> Self {
        Self {
            storage: storage.domain(KeyDomain::RoutingTaint),
            cache: Mutex::new(HashMap::new()),
            mark_lock: tokio::sync::Mutex::new(()),
            consent: Mutex::new(HashSet::new()),
        }
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

/// Lock a std mutex, recovering from poisoning: both guarded sets only
/// ever grow, so a panicked holder can't leave them inconsistent.
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
}

#[async_trait]
impl aivyx_core::TaintSink for RoutingGuard {
    async fn mark(&self, session: &str, reason: &str) -> bool {
        RoutingGuard::mark(self, session, reason).await
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
}
