//! Health-check substrate — registered watchers + current
//! state + recent state-transition ring buffer.
//!
//! Phase 125 Task 5. The tool process's background polling
//! loop (see [`crate::health_polling`]) iterates watchers on
//! their configured intervals and records check results back
//! into this store. Operator tools from Task 6
//! (`health.check.add` / `list` / `recent_changes`) read +
//! write through the same `HealthStore` handle.
//!
//! ## On-disk format
//!
//! Single JSON file at
//! `~/.aivyx-pa/tool-processes/toolkit/health.json` carrying
//! watchers + per-watcher state + the recent-transitions
//! ring buffer. State writes happen at most once per check
//! interval per watcher; for typical operator scale
//! (a handful of URLs at multi-minute intervals) this is
//! O(1 write per few minutes) — negligible I/O.
//!
//! ```json
//! {
//!   "schema_version": 1,
//!   "watchers": [
//!     {"name": "site-x", "url": "https://x.example.com/",
//!      "interval_secs": 300, "expect_status": 200}
//!   ],
//!   "states": {
//!     "site-x": {
//!       "last_check_at": "2026-05-31T...",
//!       "last_status_code": 200,
//!       "last_ok": true
//!     }
//!   },
//!   "recent_transitions": [
//!     {"watcher_name": "site-x", "transitioned_at": "...",
//!      "from_ok": true, "to_ok": false, "status_code": 503}
//!   ]
//! }
//! ```
//!
//! ## Concurrency
//!
//! All store mutations go through a single
//! [`tokio::sync::Mutex<HealthStoreState>`]. The polling loop
//! and the operator-facing tools share the same handle and
//! contend on the same lock; operations are short
//! (microseconds + one fsync per state write).
//!
//! ## Ring buffer cap
//!
//! Recent transitions are capped at 100 across all watchers.
//! Honest scope per Phase 125 sign-off: full alert history is
//! desirable but unbounded growth is risk. A future phase
//! can extend if 100 proves too small.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::fs;
use tokio::sync::Mutex;

pub const CURRENT_SCHEMA_VERSION: u32 = 1;

/// Maximum number of state transitions kept in the ring
/// buffer. Older entries roll off as new ones land.
pub const TRANSITION_RING_BUFFER_CAP: usize = 100;

/// Minimum watcher polling interval. Prevents abuse (polling
/// a URL every second from the operator's IP would look
/// hostile to the target service). Operators who want
/// faster monitoring than 1/min should look at a dedicated
/// monitoring tool, not Aivyx.
pub const MIN_INTERVAL_SECS: u64 = 60;

/// Maximum watcher polling interval. 24 hours — anything
/// longer is effectively "manual check" and the operator
/// can just invoke `web.fetch` themselves.
pub const MAX_INTERVAL_SECS: u64 = 86_400;

#[derive(Debug, Error)]
pub enum HealthStoreError {
    #[error("health storage I/O failed at {path:?}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("health storage parse failed at {path:?}: {reason}")]
    Parse { path: PathBuf, reason: String },
    #[error("health storage at {path:?} has schema_version {found} but this build only understands up to {supported}")]
    SchemaTooNew {
        path: PathBuf,
        found: u32,
        supported: u32,
    },
    #[error("watcher name {0:?} already registered")]
    DuplicateName(String),
    #[error("watcher name {0:?} not found")]
    NotFound(String),
    #[error("`interval_secs` ({0}) out of range [{MIN_INTERVAL_SECS}, {MAX_INTERVAL_SECS}]")]
    BadInterval(u64),
    #[error("`url` {0:?} must start with `http://` or `https://`")]
    BadUrl(String),
    #[error("`name` must be a non-empty identifier with no whitespace or `/`")]
    BadName,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Watcher {
    pub name: String,
    pub url: String,
    pub interval_secs: u64,
    pub expect_status: u16,
}

/// Phase 147 — outcome of a
/// [`HealthStore::remove_watcher`] call.
/// Idempotent shape: same as
/// `calendar.delete_event` and `budget.delete`
/// so the agent can paraphrase
/// "already removed" vs "removed just now"
/// uniformly across delete-style tools.
#[derive(Debug, Clone)]
pub struct RemoveOutcome {
    pub name: String,
    pub was_already_removed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct WatcherState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_check_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_status_code: Option<u16>,
    #[serde(default)]
    pub last_ok: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transition {
    pub watcher_name: String,
    pub transitioned_at: DateTime<Utc>,
    pub from_ok: bool,
    pub to_ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_code: Option<u16>,
}

/// One probe attempt outcome — what the polling loop hands
/// to `record_check`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeOutcome {
    pub status_code: Option<u16>,
    pub ok: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct StoredHealth {
    schema_version: u32,
    #[serde(default)]
    watchers: Vec<Watcher>,
    #[serde(default)]
    states: HashMap<String, WatcherState>,
    #[serde(default)]
    recent_transitions: Vec<Transition>,
}

#[derive(Debug, Default)]
struct HealthStoreState {
    watchers: Vec<Watcher>,
    states: HashMap<String, WatcherState>,
    recent_transitions: Vec<Transition>,
}

#[derive(Debug)]
pub struct HealthStore {
    path: PathBuf,
    inner: Mutex<HealthStoreState>,
}

impl HealthStore {
    /// Open the store at `path`. Loads existing watchers +
    /// states if the file exists; otherwise initializes an
    /// empty store ready for `add_watcher`.
    pub async fn open(path: PathBuf) -> Result<Self, HealthStoreError> {
        let state = match fs::read_to_string(&path).await {
            Ok(body) => {
                let stored: StoredHealth =
                    serde_json::from_str(&body).map_err(|e| HealthStoreError::Parse {
                        path: path.clone(),
                        reason: e.to_string(),
                    })?;
                if stored.schema_version > CURRENT_SCHEMA_VERSION {
                    return Err(HealthStoreError::SchemaTooNew {
                        path,
                        found: stored.schema_version,
                        supported: CURRENT_SCHEMA_VERSION,
                    });
                }
                HealthStoreState {
                    watchers: stored.watchers,
                    states: stored.states,
                    recent_transitions: stored.recent_transitions,
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => HealthStoreState::default(),
            Err(e) => return Err(HealthStoreError::Io { path, source: e }),
        };
        Ok(Self {
            path,
            inner: Mutex::new(state),
        })
    }

    /// Register a new watcher. Validates name + url +
    /// interval at the boundary; errors with `DuplicateName`
    /// if another watcher already uses the name. Persists on
    /// success. Returns the registered watcher.
    pub async fn add_watcher(
        &self,
        name: String,
        url: String,
        interval_secs: u64,
        expect_status: u16,
    ) -> Result<Watcher, HealthStoreError> {
        validate_name(&name)?;
        validate_url(&url)?;
        validate_interval(interval_secs)?;
        let mut guard = self.inner.lock().await;
        if guard.watchers.iter().any(|w| w.name == name) {
            return Err(HealthStoreError::DuplicateName(name));
        }
        let watcher = Watcher {
            name: name.clone(),
            url,
            interval_secs,
            expect_status,
        };
        guard.watchers.push(watcher.clone());
        guard.states.insert(name, WatcherState::default());
        save_to_disk(&self.path, &guard).await?;
        Ok(watcher)
    }

    /// Phase 147 — remove a watcher by name.
    /// Idempotent: returns
    /// `was_already_removed: true` when no
    /// watcher matched, matching
    /// `calendar.delete_event` +
    /// `budget.delete` posture so the agent
    /// doesn't have to special-case missing
    /// names. Removes both the watcher entry
    /// and its state-map entry to keep the
    /// two parallel structures in sync.
    /// Persists to disk only on actual change.
    pub async fn remove_watcher(
        &self,
        name: &str,
    ) -> Result<RemoveOutcome, HealthStoreError> {
        let mut guard = self.inner.lock().await;
        let pos = guard.watchers.iter().position(|w| w.name == name);
        match pos {
            Some(idx) => {
                guard.watchers.remove(idx);
                guard.states.remove(name);
                save_to_disk(&self.path, &guard).await?;
                Ok(RemoveOutcome {
                    name: name.to_string(),
                    was_already_removed: false,
                })
            }
            None => Ok(RemoveOutcome {
                name: name.to_string(),
                was_already_removed: true,
            }),
        }
    }

    /// Snapshot of every registered watcher + its current
    /// state. Used by `health.check.list` (Task 6). Returned
    /// as parallel slices so callers can zip them.
    pub async fn list_watchers(&self) -> Vec<(Watcher, WatcherState)> {
        let guard = self.inner.lock().await;
        guard
            .watchers
            .iter()
            .map(|w| {
                let s = guard
                    .states
                    .get(&w.name)
                    .cloned()
                    .unwrap_or_default();
                (w.clone(), s)
            })
            .collect()
    }

    /// Return transitions whose `transitioned_at` is within
    /// the last `window` from `now`. Used by
    /// `health.check.recent_changes` (Task 6).
    pub async fn recent_transitions_within(
        &self,
        now: DateTime<Utc>,
        window: Duration,
    ) -> Vec<Transition> {
        let cutoff = now - chrono::Duration::from_std(window).unwrap_or(chrono::Duration::zero());
        let guard = self.inner.lock().await;
        guard
            .recent_transitions
            .iter()
            .filter(|t| t.transitioned_at >= cutoff)
            .cloned()
            .collect()
    }

    /// Find watchers whose next check is due relative to
    /// `now`. A watcher is due when
    /// `last_check_at + interval_secs <= now`, OR when
    /// `last_check_at` is `None` (never checked). Used by the
    /// polling loop in [`crate::health_polling`].
    pub async fn due_watchers(&self, now: DateTime<Utc>) -> Vec<Watcher> {
        let guard = self.inner.lock().await;
        guard
            .watchers
            .iter()
            .filter(|w| {
                let state = guard.states.get(&w.name);
                match state.and_then(|s| s.last_check_at) {
                    None => true, // never checked
                    Some(last) => {
                        let next = last
                            + chrono::Duration::try_seconds(w.interval_secs as i64)
                                .unwrap_or(chrono::Duration::zero());
                        next <= now
                    }
                }
            })
            .cloned()
            .collect()
    }

    /// Return the duration until the next watcher is due
    /// relative to `now`. Returns `None` if no watchers are
    /// registered. The polling loop uses this to sleep
    /// efficiently between batches.
    pub async fn next_check_in(&self, now: DateTime<Utc>) -> Option<Duration> {
        let guard = self.inner.lock().await;
        if guard.watchers.is_empty() {
            return None;
        }
        let mut soonest: Option<chrono::Duration> = None;
        for w in &guard.watchers {
            let state = guard.states.get(&w.name);
            let next = match state.and_then(|s| s.last_check_at) {
                None => chrono::Duration::zero(), // already due
                Some(last) => {
                    let interval = chrono::Duration::try_seconds(w.interval_secs as i64)
                        .unwrap_or(chrono::Duration::zero());
                    (last + interval) - now
                }
            };
            let wait = if next < chrono::Duration::zero() {
                chrono::Duration::zero()
            } else {
                next
            };
            soonest = Some(match soonest {
                None => wait,
                Some(prev) => prev.min(wait),
            });
        }
        soonest.map(|d| d.to_std().unwrap_or(Duration::ZERO))
    }

    /// Record a check outcome against a watcher. If
    /// `outcome.ok` differs from the previous `last_ok`,
    /// appends a `Transition` to the ring buffer (capped at
    /// [`TRANSITION_RING_BUFFER_CAP`]) and returns `Some(transition)`.
    /// Returns `None` if no state transition occurred (including
    /// the first-ever poll, which has no prior state to transition from).
    pub async fn record_check(
        &self,
        watcher_name: &str,
        outcome: ProbeOutcome,
        now: DateTime<Utc>,
    ) -> Result<Option<Transition>, HealthStoreError> {
        let mut guard = self.inner.lock().await;
        let prev_state = guard.states.get(watcher_name).cloned();
        let prev_ok = prev_state.as_ref().map(|s| s.last_ok);
        let had_prior_check = prev_state
            .as_ref()
            .map(|s| s.last_check_at.is_some())
            .unwrap_or(false);

        let new_state = WatcherState {
            last_check_at: Some(now),
            last_status_code: outcome.status_code,
            last_ok: outcome.ok,
        };
        guard.states.insert(watcher_name.to_string(), new_state);

        // Record a transition iff the watcher had a prior
        // check (so we don't fire "transition" on the first-
        // ever poll) and the ok-flag flipped.
        let mut fired: Option<Transition> = None;
        if had_prior_check {
            if let Some(prev) = prev_ok {
                if prev != outcome.ok {
                    let t = Transition {
                        watcher_name: watcher_name.to_string(),
                        transitioned_at: now,
                        from_ok: prev,
                        to_ok: outcome.ok,
                        status_code: outcome.status_code,
                    };
                    push_transition_ring(&mut guard.recent_transitions, t.clone());
                    fired = Some(t);
                }
            }
        }

        save_to_disk(&self.path, &guard).await?;
        Ok(fired)
    }
}

/// Append + trim the front so the ring buffer stays under
/// the cap. O(1) amortized at the cap because `Vec::remove(0)`
/// fires at most once per push when over capacity.
fn push_transition_ring(ring: &mut Vec<Transition>, t: Transition) {
    ring.push(t);
    while ring.len() > TRANSITION_RING_BUFFER_CAP {
        ring.remove(0);
    }
}

fn validate_name(name: &str) -> Result<(), HealthStoreError> {
    if name.is_empty() {
        return Err(HealthStoreError::BadName);
    }
    if name.chars().any(|c| c.is_whitespace() || c == '/' || c == '\\') {
        return Err(HealthStoreError::BadName);
    }
    Ok(())
}

fn validate_url(url: &str) -> Result<(), HealthStoreError> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(HealthStoreError::BadUrl(url.to_string()));
    }
    Ok(())
}

fn validate_interval(secs: u64) -> Result<(), HealthStoreError> {
    if !(MIN_INTERVAL_SECS..=MAX_INTERVAL_SECS).contains(&secs) {
        return Err(HealthStoreError::BadInterval(secs));
    }
    Ok(())
}

async fn save_to_disk(
    path: &Path,
    state: &HealthStoreState,
) -> Result<(), HealthStoreError> {
    let stored = StoredHealth {
        schema_version: CURRENT_SCHEMA_VERSION,
        watchers: state.watchers.clone(),
        states: state.states.clone(),
        recent_transitions: state.recent_transitions.clone(),
    };
    let body = serde_json::to_string_pretty(&stored).map_err(|e| HealthStoreError::Parse {
        path: path.to_path_buf(),
        reason: format!("serialize: {e}"),
    })?;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            create_dir_all_secure(parent)
                .await
                .map_err(|source| HealthStoreError::Io {
                    path: parent.to_path_buf(),
                    source,
                })?;
        }
    }
    let tmp_path = with_tmp_suffix(path);
    write_secure(&tmp_path, body.as_bytes())
        .await
        .map_err(|source| HealthStoreError::Io {
            path: tmp_path.clone(),
            source,
        })?;
    fs::rename(&tmp_path, path)
        .await
        .map_err(|source| HealthStoreError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    Ok(())
}

fn with_tmp_suffix(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".tmp");
    PathBuf::from(s)
}

async fn create_dir_all_secure(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o700);
        fs::set_permissions(dir, perms).await?;
    }
    Ok(())
}

async fn write_secure(path: &Path, body: &[u8]) -> io::Result<()> {
    #[cfg(unix)]
    {
        use tokio::io::AsyncWriteExt;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .await?;
        file.write_all(body).await?;
        file.sync_all().await?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        fs::write(path, body).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn scratch_dir() -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let tmp = std::env::var("TMPDIR")
            .or_else(|_| std::env::var("TEMP"))
            .unwrap_or_else(|_| "/tmp".to_string());
        let dir = PathBuf::from(tmp).join(format!(
            "aivyx-toolkit-health-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn t0() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-05-31T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    // ---- validators ---------------------------------------

    #[test]
    fn validate_name_rejects_empty_and_whitespace() {
        assert!(validate_name("").is_err());
        assert!(validate_name(" ").is_err());
        assert!(validate_name("has space").is_err());
        assert!(validate_name("has/slash").is_err());
        assert!(validate_name("good-name").is_ok());
    }

    #[test]
    fn validate_url_requires_http_scheme() {
        assert!(validate_url("https://x.example.com/").is_ok());
        assert!(validate_url("http://x.example.com/").is_ok());
        assert!(validate_url("ftp://x").is_err());
        assert!(validate_url("x.example.com").is_err());
    }

    #[test]
    fn validate_interval_bounds() {
        assert!(validate_interval(60).is_ok());
        assert!(validate_interval(86_400).is_ok());
        assert!(validate_interval(59).is_err());
        assert!(validate_interval(86_401).is_err());
    }

    // ---- open / add ---------------------------------------

    #[tokio::test]
    async fn open_creates_empty_store_when_absent() {
        let dir = scratch_dir();
        let store = HealthStore::open(dir.join("health.json"))
            .await
            .expect("open");
        let watchers = store.list_watchers().await;
        assert!(watchers.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn add_watcher_persists_and_initializes_state() {
        let dir = scratch_dir();
        let path = dir.join("health.json");
        let store = HealthStore::open(path.clone()).await.expect("open");
        let w = store
            .add_watcher(
                "site-x".to_string(),
                "https://x.example.com/".to_string(),
                300,
                200,
            )
            .await
            .expect("add");
        assert_eq!(w.name, "site-x");
        assert_eq!(w.interval_secs, 300);
        // State persisted on reopen.
        let reopened = HealthStore::open(path).await.expect("reopen");
        let watchers = reopened.list_watchers().await;
        assert_eq!(watchers.len(), 1);
        assert_eq!(watchers[0].0.name, "site-x");
        // Initial state: never checked.
        assert!(watchers[0].1.last_check_at.is_none());
        assert!(!watchers[0].1.last_ok);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn add_watcher_rejects_duplicate_name() {
        let dir = scratch_dir();
        let store = HealthStore::open(dir.join("health.json")).await.unwrap();
        store
            .add_watcher("dup".to_string(), "https://x/".to_string(), 60, 200)
            .await
            .unwrap();
        match store
            .add_watcher("dup".to_string(), "https://y/".to_string(), 60, 200)
            .await
        {
            Err(HealthStoreError::DuplicateName(name)) => assert_eq!(name, "dup"),
            other => panic!("expected DuplicateName; got {other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn add_watcher_validates_at_boundary() {
        let dir = scratch_dir();
        let store = HealthStore::open(dir.join("health.json")).await.unwrap();
        // Bad name.
        assert!(matches!(
            store.add_watcher("bad name".to_string(), "https://x/".to_string(), 60, 200).await,
            Err(HealthStoreError::BadName)
        ));
        // Bad url.
        assert!(matches!(
            store.add_watcher("good".to_string(), "ftp://x".to_string(), 60, 200).await,
            Err(HealthStoreError::BadUrl(_))
        ));
        // Bad interval.
        assert!(matches!(
            store.add_watcher("good".to_string(), "https://x/".to_string(), 30, 200).await,
            Err(HealthStoreError::BadInterval(30))
        ));
        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- due_watchers / next_check_in ---------------------

    #[tokio::test]
    async fn due_watchers_returns_unchecked_immediately() {
        let dir = scratch_dir();
        let store = HealthStore::open(dir.join("health.json")).await.unwrap();
        store
            .add_watcher("never-checked".to_string(), "https://x/".to_string(), 300, 200)
            .await
            .unwrap();
        let due = store.due_watchers(t0()).await;
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].name, "never-checked");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn due_watchers_skips_recently_checked() {
        let dir = scratch_dir();
        let store = HealthStore::open(dir.join("health.json")).await.unwrap();
        store
            .add_watcher("x".to_string(), "https://x/".to_string(), 300, 200)
            .await
            .unwrap();
        store
            .record_check("x", ProbeOutcome { status_code: Some(200), ok: true }, t0())
            .await
            .unwrap();
        // 1 second later — not yet due (300s interval).
        let due = store.due_watchers(t0() + chrono::Duration::seconds(1)).await;
        assert!(due.is_empty());
        // 300 seconds later — due.
        let due = store.due_watchers(t0() + chrono::Duration::seconds(300)).await;
        assert_eq!(due.len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn next_check_in_zero_for_unchecked_watcher() {
        let dir = scratch_dir();
        let store = HealthStore::open(dir.join("health.json")).await.unwrap();
        store
            .add_watcher("x".to_string(), "https://x/".to_string(), 60, 200)
            .await
            .unwrap();
        let wait = store.next_check_in(t0()).await;
        assert_eq!(wait, Some(Duration::ZERO));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn next_check_in_none_when_no_watchers() {
        let dir = scratch_dir();
        let store = HealthStore::open(dir.join("health.json")).await.unwrap();
        assert!(store.next_check_in(t0()).await.is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn next_check_in_min_across_watchers() {
        let dir = scratch_dir();
        let store = HealthStore::open(dir.join("health.json")).await.unwrap();
        store.add_watcher("a".to_string(), "https://a/".to_string(), 60, 200).await.unwrap();
        store.add_watcher("b".to_string(), "https://b/".to_string(), 300, 200).await.unwrap();
        store.record_check("a", ProbeOutcome { status_code: Some(200), ok: true }, t0()).await.unwrap();
        store.record_check("b", ProbeOutcome { status_code: Some(200), ok: true }, t0()).await.unwrap();
        // At t0 + 10s: `a` due in 50s; `b` due in 290s. Min is 50s.
        let wait = store
            .next_check_in(t0() + chrono::Duration::seconds(10))
            .await
            .unwrap();
        assert_eq!(wait, Duration::from_secs(50));
        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- record_check / transitions -----------------------

    #[tokio::test]
    async fn record_check_first_poll_no_transition() {
        let dir = scratch_dir();
        let store = HealthStore::open(dir.join("health.json")).await.unwrap();
        store.add_watcher("x".to_string(), "https://x/".to_string(), 60, 200).await.unwrap();
        store
            .record_check("x", ProbeOutcome { status_code: Some(503), ok: false }, t0())
            .await
            .unwrap();
        let transitions = store
            .recent_transitions_within(t0() + chrono::Duration::seconds(10), Duration::from_secs(3600))
            .await;
        assert!(
            transitions.is_empty(),
            "first poll has no prior state to transition FROM"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn record_check_state_flip_records_transition() {
        let dir = scratch_dir();
        let store = HealthStore::open(dir.join("health.json")).await.unwrap();
        store.add_watcher("x".to_string(), "https://x/".to_string(), 60, 200).await.unwrap();
        // First poll: ok.
        store.record_check("x", ProbeOutcome { status_code: Some(200), ok: true }, t0()).await.unwrap();
        // Second poll: down.
        store
            .record_check(
                "x",
                ProbeOutcome { status_code: Some(503), ok: false },
                t0() + chrono::Duration::seconds(60),
            )
            .await
            .unwrap();
        let transitions = store
            .recent_transitions_within(
                t0() + chrono::Duration::seconds(120),
                Duration::from_secs(3600),
            )
            .await;
        assert_eq!(transitions.len(), 1);
        assert_eq!(transitions[0].watcher_name, "x");
        assert!(transitions[0].from_ok);
        assert!(!transitions[0].to_ok);
        assert_eq!(transitions[0].status_code, Some(503));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn record_check_no_state_change_no_transition() {
        let dir = scratch_dir();
        let store = HealthStore::open(dir.join("health.json")).await.unwrap();
        store.add_watcher("x".to_string(), "https://x/".to_string(), 60, 200).await.unwrap();
        store.record_check("x", ProbeOutcome { status_code: Some(200), ok: true }, t0()).await.unwrap();
        store
            .record_check(
                "x",
                ProbeOutcome { status_code: Some(200), ok: true },
                t0() + chrono::Duration::seconds(60),
            )
            .await
            .unwrap();
        let transitions = store
            .recent_transitions_within(
                t0() + chrono::Duration::seconds(120),
                Duration::from_secs(3600),
            )
            .await;
        assert!(transitions.is_empty(), "ok-to-ok is not a transition");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn record_check_returns_the_transition_on_a_real_flip() {
        let dir = scratch_dir();
        let store = HealthStore::open(dir.join("health.json")).await.unwrap();
        store.add_watcher("x".to_string(), "https://x/".to_string(), 60, 200).await.unwrap();
        // First poll: establishes a baseline, no transition.
        let first = store
            .record_check("x", ProbeOutcome { status_code: Some(200), ok: true }, t0())
            .await
            .unwrap();
        assert!(first.is_none(), "first-ever poll must not report a transition");

        // Second poll: real flip.
        let second = store
            .record_check(
                "x",
                ProbeOutcome { status_code: Some(503), ok: false },
                t0() + chrono::Duration::seconds(60),
            )
            .await
            .unwrap();
        assert!(second.is_some(), "an ok-flag flip must report the transition");
        let t = second.unwrap();
        assert_eq!(t.watcher_name, "x");
        assert!(t.from_ok);
        assert!(!t.to_ok);
        assert_eq!(t.status_code, Some(503));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn record_check_returns_none_when_state_unchanged() {
        let dir = scratch_dir();
        let store = HealthStore::open(dir.join("health.json")).await.unwrap();
        store.add_watcher("x".to_string(), "https://x/".to_string(), 60, 200).await.unwrap();
        store.record_check("x", ProbeOutcome { status_code: Some(200), ok: true }, t0()).await.unwrap();
        let repeat = store
            .record_check(
                "x",
                ProbeOutcome { status_code: Some(200), ok: true },
                t0() + chrono::Duration::seconds(60),
            )
            .await
            .unwrap();
        assert!(repeat.is_none(), "no state change must not report a transition");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn recent_transitions_window_filters_old_entries() {
        let dir = scratch_dir();
        let store = HealthStore::open(dir.join("health.json")).await.unwrap();
        store.add_watcher("x".to_string(), "https://x/".to_string(), 60, 200).await.unwrap();
        // Build a transition at t0.
        store.record_check("x", ProbeOutcome { status_code: Some(200), ok: true }, t0()).await.unwrap();
        store
            .record_check(
                "x",
                ProbeOutcome { status_code: Some(503), ok: false },
                t0() + chrono::Duration::seconds(60),
            )
            .await
            .unwrap();
        // Window of 30s from t0+90s — transition at t0+60s NOT included (30s window only covers t0+60s..t0+90s ... wait, +60s IS within +60..+90).
        // Use a tighter window: 10s from t0+120s → covers t0+110s..t0+120s; transition at t0+60s excluded.
        let recent = store
            .recent_transitions_within(
                t0() + chrono::Duration::seconds(120),
                Duration::from_secs(10),
            )
            .await;
        assert!(recent.is_empty());
        // Wide window: 1hr from t0+120s — transition included.
        let recent = store
            .recent_transitions_within(
                t0() + chrono::Duration::seconds(120),
                Duration::from_secs(3600),
            )
            .await;
        assert_eq!(recent.len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn transition_ring_buffer_caps_at_100() {
        let dir = scratch_dir();
        let store = HealthStore::open(dir.join("health.json")).await.unwrap();
        store.add_watcher("x".to_string(), "https://x/".to_string(), 60, 200).await.unwrap();
        // Alternate ok/down 200 times → 199 transitions.
        let mut ok = true;
        store.record_check("x", ProbeOutcome { status_code: Some(200), ok }, t0()).await.unwrap();
        for i in 0..200 {
            ok = !ok;
            store
                .record_check(
                    "x",
                    ProbeOutcome { status_code: Some(if ok { 200 } else { 503 }), ok },
                    t0() + chrono::Duration::seconds(60 * (i + 1) as i64),
                )
                .await
                .unwrap();
        }
        let all = store
            .recent_transitions_within(
                t0() + chrono::Duration::seconds(60 * 250),
                Duration::from_secs(60 * 60 * 24),
            )
            .await;
        assert_eq!(all.len(), TRANSITION_RING_BUFFER_CAP);
        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- persistence --------------------------------------

    #[tokio::test]
    async fn store_round_trips_through_disk() {
        let dir = scratch_dir();
        let path = dir.join("health.json");
        let store = HealthStore::open(path.clone()).await.unwrap();
        store.add_watcher("x".to_string(), "https://x/".to_string(), 60, 200).await.unwrap();
        store
            .record_check("x", ProbeOutcome { status_code: Some(200), ok: true }, t0())
            .await
            .unwrap();
        // Reopen — watcher + last-check state survive.
        let reopened = HealthStore::open(path).await.unwrap();
        let watchers = reopened.list_watchers().await;
        assert_eq!(watchers.len(), 1);
        assert_eq!(watchers[0].1.last_status_code, Some(200));
        assert!(watchers[0].1.last_ok);
        assert!(watchers[0].1.last_check_at.is_some());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn schema_too_new_surfaces_with_path() {
        let dir = scratch_dir();
        let path = dir.join("health.json");
        std::fs::write(
            &path,
            r#"{"schema_version": 99, "watchers": [], "states": {}, "recent_transitions": []}"#,
        )
        .unwrap();
        match HealthStore::open(path.clone()).await {
            Err(HealthStoreError::SchemaTooNew { found, .. }) => assert_eq!(found, 99),
            other => panic!("expected SchemaTooNew; got {other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn save_writes_with_0600_perms() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch_dir();
        let path = dir.join("health.json");
        let store = HealthStore::open(path.clone()).await.unwrap();
        store.add_watcher("x".to_string(), "https://x/".to_string(), 60, 200).await.unwrap();
        let meta = std::fs::metadata(&path).unwrap();
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "got {mode:o}");
        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- push_transition_ring -----------------------------

    #[test]
    fn push_transition_ring_evicts_oldest_at_cap() {
        let mut ring: Vec<Transition> = Vec::new();
        for i in 0..TRANSITION_RING_BUFFER_CAP + 5 {
            push_transition_ring(
                &mut ring,
                Transition {
                    watcher_name: format!("w-{i}"),
                    transitioned_at: t0(),
                    from_ok: true,
                    to_ok: false,
                    status_code: Some(503),
                },
            );
        }
        assert_eq!(ring.len(), TRANSITION_RING_BUFFER_CAP);
        // First five should have rolled off; the surviving
        // front is w-5.
        assert_eq!(ring[0].watcher_name, "w-5");
        assert_eq!(
            ring[TRANSITION_RING_BUFFER_CAP - 1].watcher_name,
            format!("w-{}", TRANSITION_RING_BUFFER_CAP + 4),
        );
    }

    // ---- Phase 147 — remove_watcher ----------------------

    #[tokio::test]
    async fn remove_watcher_present_returns_was_already_removed_false() {
        let dir = scratch_dir();
        let path = dir.join("health.json");
        let store = HealthStore::open(path.clone()).await.expect("open");
        store
            .add_watcher(
                "site-x".to_string(),
                "https://x.example.com/".to_string(),
                300,
                200,
            )
            .await
            .expect("add");
        let outcome = store.remove_watcher("site-x").await.expect("remove");
        assert_eq!(outcome.name, "site-x");
        assert!(!outcome.was_already_removed);
        // Verify it's gone via list.
        let watchers = store.list_watchers().await;
        assert!(watchers.is_empty(), "watcher must be gone after remove");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn remove_watcher_missing_is_idempotent() {
        let dir = scratch_dir();
        let store = HealthStore::open(dir.join("health.json"))
            .await
            .expect("open");
        let outcome = store
            .remove_watcher("never-existed")
            .await
            .expect("remove");
        assert_eq!(outcome.name, "never-existed");
        assert!(
            outcome.was_already_removed,
            "missing name must surface as was_already_removed = true"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn remove_watcher_persists_across_reopen() {
        let dir = scratch_dir();
        let path = dir.join("health.json");
        {
            let store = HealthStore::open(path.clone()).await.expect("open");
            store
                .add_watcher(
                    "alpha".to_string(),
                    "https://a.example.com/".to_string(),
                    300,
                    200,
                )
                .await
                .expect("add alpha");
            store
                .add_watcher(
                    "beta".to_string(),
                    "https://b.example.com/".to_string(),
                    300,
                    200,
                )
                .await
                .expect("add beta");
            store.remove_watcher("alpha").await.expect("remove alpha");
        }
        // Re-open and verify only beta survives.
        let reopened = HealthStore::open(path.clone()).await.expect("reopen");
        let watchers = reopened.list_watchers().await;
        assert_eq!(watchers.len(), 1);
        assert_eq!(watchers[0].0.name, "beta");
        // Second remove of alpha — idempotent.
        let outcome = reopened
            .remove_watcher("alpha")
            .await
            .expect("idempotent remove");
        assert!(outcome.was_already_removed);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn remove_watcher_cleans_state_map_in_sync() {
        // Phase 147 regression boundary — the
        // watchers Vec and states HashMap are
        // parallel structures. Remove must clean
        // BOTH; an orphaned state entry would
        // leak memory + survive re-add as
        // recovered state.
        let dir = scratch_dir();
        let path = dir.join("health.json");
        let store = HealthStore::open(path.clone()).await.expect("open");
        store
            .add_watcher(
                "site-y".to_string(),
                "https://y.example.com/".to_string(),
                300,
                200,
            )
            .await
            .expect("add");
        store.remove_watcher("site-y").await.expect("remove");
        // Re-add the same name; it should start
        // fresh (not pick up a stale state).
        let re_added = store
            .add_watcher(
                "site-y".to_string(),
                "https://y.example.com/".to_string(),
                300,
                200,
            )
            .await
            .expect("re-add");
        assert_eq!(re_added.name, "site-y");
        let watchers = store.list_watchers().await;
        assert_eq!(watchers.len(), 1);
        // Fresh state — never checked.
        assert!(watchers[0].1.last_check_at.is_none());
        assert!(!watchers[0].1.last_ok);
        std::fs::remove_dir_all(&dir).ok();
    }
}
