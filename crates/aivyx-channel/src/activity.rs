//! The operator's last activity, for the Command Center's "since you were
//! last here". Only things the operator *does* count (see
//! [`is_operator_action`]); looking doesn't. Actions less than
//! [`VISIT_GAP_SECS`] apart are one visit, and "last here" is the end of the
//! previous visit — so acting on the Command Center never empties its own
//! log. Persisted in the encrypted `ChannelState` domain.

use std::sync::Mutex;

use aivyx_storage::DomainHandle;
use serde::{Deserialize, Serialize};

use crate::daemon_ipc::{FrontendMessage, QueryPayload};

/// Actions closer together than this are one visit.
pub const VISIT_GAP_SECS: i64 = 30 * 60;

const KEY: &[u8] = b"operator.activity";

/// How far `last_action` moves before it is written again. A restart
/// loses at most this much precision; a new visit is always written.
pub const PERSIST_EVERY_SECS: i64 = 60;

/// The persisted pair: the newest action, and the end of the visit before
/// the current one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Activity {
    pub last_action: Option<i64>,
    pub anchor: Option<i64>,
}

impl Activity {
    /// Record an action at `now`. A gap longer than a visit starts a new
    /// visit, whose anchor is the previous visit's last action.
    pub fn touched(self, now: i64) -> Activity {
        match self.last_action {
            Some(t) if now - t <= VISIT_GAP_SECS => Activity { last_action: Some(now), anchor: self.anchor },
            previous => Activity { last_action: Some(now), anchor: previous },
        }
    }

    /// When "since you were last here" starts, seen at `now`: once the
    /// newest action is a visit-gap old, it is the end of the last visit.
    pub fn last_here(self, now: i64) -> Option<i64> {
        match self.last_action {
            Some(t) if now - t > VISIT_GAP_SECS => Some(t),
            _ => self.anchor,
        }
    }
}

/// The daemon's one clock, shared by every connection.
pub struct ActivityClock {
    /// (current, last persisted).
    state: Mutex<(Activity, Activity)>,
    store: Option<DomainHandle>,
}

/// Whether moving from the persisted value to `next` is worth a write.
fn worth_persisting(persisted: Activity, next: Activity) -> bool {
    next.anchor != persisted.anchor
        || match (persisted.last_action, next.last_action) {
            (Some(p), Some(n)) => (n - p).abs() >= PERSIST_EVERY_SECS,
            (p, n) => p != n,
        }
}

impl ActivityClock {
    /// Read the persisted state (unreadable or absent → fresh).
    pub async fn load(store: Option<DomainHandle>) -> ActivityClock {
        let state = match &store {
            Some(s) => match s.get(KEY).await {
                Ok(Some(bytes)) => serde_json::from_slice(&bytes).unwrap_or_default(),
                _ => Activity::default(),
            },
            None => Activity::default(),
        };
        ActivityClock { state: Mutex::new((state, state)), store }
    }

    pub fn snapshot(&self) -> Activity {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).0
    }

    /// Record an operator action. The in-memory value always moves; the
    /// store is written only when a new visit starts or `last_action` has
    /// moved [`PERSIST_EVERY_SECS`] since the last write. A failed write is
    /// logged, not fatal: the in-memory value still serves this run.
    pub async fn touch(&self, now: i64) {
        let next = {
            let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            s.0 = s.0.touched(now);
            if self.store.is_none() || !worth_persisting(s.1, s.0) {
                return;
            }
            s.1 = s.0;
            s.0
        };
        if let Some(store) = &self.store
            && let Ok(bytes) = serde_json::to_vec(&next)
            && let Err(e) = store.put(KEY, &bytes).await
        {
            eprintln!("aivyx-pa daemon: failed to persist operator activity: {e}");
        }
    }
}

/// Frames that are the operator doing something (any daemon-socket front
/// end: CLI chat, TUI, Studio). Reads — `GetBriefing` included — don't count,
/// and neither does a headless (automated) `SubmitInput`.
pub fn is_operator_action(msg: &FrontendMessage) -> bool {
    match msg {
        FrontendMessage::SubmitInput { headless: false, .. }
        | FrontendMessage::ResolveApproval { .. }
        | FrontendMessage::ResolveGate { .. }
        | FrontendMessage::ResolvePersonaProposal { .. } => true,
        FrontendMessage::Query { payload, .. } => matches!(
            payload,
            QueryPayload::ResolveTeamGate { .. }
                | QueryPayload::CompleteReminder { .. }
                | QueryPayload::SnoozeReminder { .. }
        ),
        _ => false,
    }
}

/// Wall-clock seconds.
pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn actions_within_a_visit_keep_the_previous_visit_as_last_here() {
        let a = Activity::default();
        assert_eq!(a.last_here(0), None);

        // First ever action: nothing before it.
        let a = a.touched(1_000);
        assert_eq!(a.last_here(1_010), None);

        // Same visit: still nothing before it.
        let a = a.touched(1_500);
        assert_eq!(a.last_here(1_600), None);

        // Away for over 30 min: the last action becomes "last here" at once.
        assert_eq!(a.last_here(1_500 + VISIT_GAP_SECS + 1), Some(1_500));

        // Acting in the new visit keeps showing the old visit's end.
        let later = 1_500 + VISIT_GAP_SECS + 100;
        let a = a.touched(later);
        assert_eq!(a.last_here(later + 5), Some(1_500));
        let a = a.touched(later + 60);
        assert_eq!(a.last_here(later + 70), Some(1_500));
    }

    #[test]
    fn operator_actions_are_the_things_you_do() {
        let yes = [
            FrontendMessage::SubmitInput {
                session_id: "s".into(),
                text: "hi".into(),
                mission_id: None,
                attachments: Vec::new(),
                headless: false,
            },
            FrontendMessage::ResolveApproval { request_id: "a".into(), approved: true },
            FrontendMessage::ResolveGate { mission_id: "m".into(), gate_id: "g".into(), approved: false },
            FrontendMessage::Query { id: "x".into(), payload: QueryPayload::CompleteReminder { id: "r".into() } },
            FrontendMessage::Query { id: "x".into(), payload: QueryPayload::SnoozeReminder { id: "r".into(), secs: 60 } },
            FrontendMessage::Query {
                id: "x".into(),
                payload: QueryPayload::ResolveTeamGate { mission_id: "m".into(), step: "s".into(), approve: true },
            },
        ];
        for m in &yes {
            assert!(is_operator_action(m), "{m:?}");
        }
        let no = [
            // A headless (automated) submit isn't the operator acting.
            FrontendMessage::SubmitInput {
                session_id: "s".into(),
                text: "scheduled".into(),
                mission_id: None,
                attachments: Vec::new(),
                headless: true,
            },
            FrontendMessage::Query { id: "x".into(), payload: QueryPayload::GetBriefing },
            FrontendMessage::Query { id: "x".into(), payload: QueryPayload::GetReminders },
            FrontendMessage::SetApprovals { enabled: true },
        ];
        for m in &no {
            assert!(!is_operator_action(m), "{m:?}");
        }
    }

    #[tokio::test]
    async fn the_clock_survives_a_reopen() {
        use aivyx_crypto::MasterKey;
        use aivyx_storage::{KeyDomain, RedbStorage, Storage, StorageConfig};
        let dir = std::env::temp_dir().join(format!("aivyx-activity-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("store.redb");
        {
            let store: Arc<dyn Storage> =
                RedbStorage::open(StorageConfig::new(path.clone()), MasterKey::from_raw([5u8; 32]))
                    .await
                    .unwrap();
            let clock = ActivityClock::load(Some(store.domain(KeyDomain::ChannelState))).await;
            clock.touch(1_000).await;
            clock.touch(1_000 + VISIT_GAP_SECS + 10).await;
        }
        let store: Arc<dyn Storage> =
            RedbStorage::open(StorageConfig::new(path), MasterKey::from_raw([5u8; 32])).await.unwrap();
        let clock = ActivityClock::load(Some(store.domain(KeyDomain::ChannelState))).await;
        assert_eq!(
            clock.snapshot(),
            Activity { last_action: Some(1_000 + VISIT_GAP_SECS + 10), anchor: Some(1_000) }
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Touch a fresh store's clock at each of `touches`, then reopen it and
    /// return what was persisted (alongside the in-memory value before).
    async fn persisted_after(touches: &[i64]) -> (Activity, Activity) {
        use aivyx_crypto::MasterKey;
        use aivyx_storage::{KeyDomain, RedbStorage, Storage, StorageConfig};
        let dir = std::env::temp_dir().join(format!("aivyx-activity-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("store.redb");
        let in_memory = {
            let store: Arc<dyn Storage> =
                RedbStorage::open(StorageConfig::new(path.clone()), MasterKey::from_raw([5u8; 32]))
                    .await
                    .unwrap();
            let clock = ActivityClock::load(Some(store.domain(KeyDomain::ChannelState))).await;
            for t in touches {
                clock.touch(*t).await;
            }
            clock.snapshot()
        };
        let store: Arc<dyn Storage> =
            RedbStorage::open(StorageConfig::new(path), MasterKey::from_raw([5u8; 32])).await.unwrap();
        let persisted = ActivityClock::load(Some(store.domain(KeyDomain::ChannelState))).await.snapshot();
        let _ = std::fs::remove_dir_all(&dir);
        (in_memory, persisted)
    }

    #[tokio::test]
    async fn the_clock_persists_only_when_it_matters() {
        // Two touches 10 s apart: memory moves, the store keeps the first.
        let (mem, disk) = persisted_after(&[1_000, 1_010]).await;
        assert_eq!(mem, Activity { last_action: Some(1_010), anchor: None });
        assert_eq!(disk, Activity { last_action: Some(1_000), anchor: None });

        // A minute past the last persisted value is persisted.
        let (_, disk) = persisted_after(&[1_000, 1_010, 1_059]).await;
        assert_eq!(disk.last_action, Some(1_000));
        let (_, disk) = persisted_after(&[1_000, 1_010, 1_060]).await;
        assert_eq!(disk.last_action, Some(1_060));

        // A new visit (anchor change) is persisted.
        let later = 1_010 + VISIT_GAP_SECS + 1;
        let (mem, disk) = persisted_after(&[1_000, 1_010, later]).await;
        assert_eq!(disk, mem);
        assert_eq!(disk, Activity { last_action: Some(later), anchor: Some(1_010) });
    }

    #[tokio::test]
    async fn without_a_store_the_clock_still_works_in_memory() {
        let clock = ActivityClock::load(None).await;
        clock.touch(5).await;
        assert_eq!(clock.snapshot().last_action, Some(5));
    }
}
