//! Lightweight TODO storage for the `task.*` tools.
//!
//! Phase 125 Task 4. JSON file at
//! `~/.aivyx-pa/tool-processes/toolkit/tasks.json` (0600 perms,
//! atomic write-then-rename — same pattern as Gmail's token
//! file).
//!
//! ## Concurrency model
//!
//! `TaskStore` holds the loaded task list behind a
//! [`tokio::sync::Mutex`]. The harness can dispatch multiple
//! task tools in parallel; each operation takes the lock,
//! mutates in-memory state, persists to disk, drops the
//! lock. Operations are short (microseconds + one fsync) so
//! contention is acceptable.
//!
//! ## On-disk format
//!
//! ```json
//! {
//!   "schema_version": 1,
//!   "tasks": [
//!     {
//!       "id": "uuid-v4",
//!       "title": "...",
//!       "notes": "..." (or null),
//!       "due_date": "2026-06-01T12:00:00Z" (or null),
//!       "status": "open" | "complete",
//!       "created_at": "2026-05-31T06:36:00Z",
//!       "completed_at": "..." (or null),
//!       "completion_note": "..." (or null)
//!     }
//!   ]
//! }
//! ```
//!
//! `schema_version` lets a future Phase 126+ extend the
//! shape without breaking existing operator installs (same
//! pattern as Gmail's token file).

use std::io;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::fs;
use tokio::sync::Mutex;
use uuid::Uuid;

pub const CURRENT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum TaskStoreError {
    #[error("task storage I/O failed at {path:?}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("task storage parse failed at {path:?}: {reason}")]
    Parse { path: PathBuf, reason: String },
    #[error("task storage at {path:?} has schema_version {found} but this build only understands up to {supported}")]
    SchemaTooNew {
        path: PathBuf,
        found: u32,
        supported: u32,
    },
    #[error("task with id {0:?} not found")]
    NotFound(String),
    #[error("`due_date` value {0:?} is not RFC 3339 / ISO 8601 — must look like `2026-06-01T12:00:00Z`")]
    BadDueDate(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    Open,
    Complete,
}

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskStatus::Open => "open",
            TaskStatus::Complete => "complete",
        }
    }
}

/// One TODO entry. Public fields so the tool impls can
/// project specific subsets into their JSON outputs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_date: Option<DateTime<Utc>>,
    pub status: TaskStatus,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_note: Option<String>,
}

/// On-disk wrapper around the task list. Versioned at the
/// file level for forward-compat.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredTasks {
    schema_version: u32,
    #[serde(default)]
    tasks: Vec<Task>,
}

/// The task store. Holds the loaded list under a Mutex; one
/// instance per tool process (constructed once in main and
/// shared via `Arc<TaskStore>` to each `task.*` tool impl).
#[derive(Debug)]
pub struct TaskStore {
    path: PathBuf,
    tasks: Mutex<Vec<Task>>,
}

impl TaskStore {
    /// Open the store at `path`. Loads the file if it exists;
    /// initializes with an empty list if not. Either way the
    /// store is ready for reads and writes.
    pub async fn open(path: PathBuf) -> Result<Self, TaskStoreError> {
        let tasks = match fs::read_to_string(&path).await {
            Ok(body) => {
                let stored: StoredTasks = serde_json::from_str(&body).map_err(|e| {
                    TaskStoreError::Parse {
                        path: path.clone(),
                        reason: e.to_string(),
                    }
                })?;
                if stored.schema_version > CURRENT_SCHEMA_VERSION {
                    return Err(TaskStoreError::SchemaTooNew {
                        path,
                        found: stored.schema_version,
                        supported: CURRENT_SCHEMA_VERSION,
                    });
                }
                stored.tasks
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                return Err(TaskStoreError::Io { path, source: e });
            }
        };
        Ok(Self {
            path,
            tasks: Mutex::new(tasks),
        })
    }

    /// Create a new task with a fresh UUID v4 id and
    /// `status = Open`. Persists to disk before returning.
    pub async fn create(
        &self,
        title: String,
        notes: Option<String>,
        due_date: Option<DateTime<Utc>>,
    ) -> Result<Task, TaskStoreError> {
        let now = Utc::now();
        let task = Task {
            id: Uuid::new_v4().to_string(),
            title,
            notes,
            due_date,
            status: TaskStatus::Open,
            created_at: now,
            completed_at: None,
            completion_note: None,
        };
        let mut guard = self.tasks.lock().await;
        guard.push(task.clone());
        save_to_disk(&self.path, &guard).await?;
        Ok(task)
    }

    /// List tasks filtered by status. `None` filter returns
    /// every task; `Some(status)` returns matching entries
    /// only. `limit` caps the returned slice.
    pub async fn list(
        &self,
        filter: Option<TaskStatus>,
        limit: usize,
    ) -> (Vec<Task>, usize) {
        let guard = self.tasks.lock().await;
        let total_count = guard
            .iter()
            .filter(|t| filter.map(|f| t.status == f).unwrap_or(true))
            .count();
        let filtered: Vec<Task> = guard
            .iter()
            .filter(|t| filter.map(|f| t.status == f).unwrap_or(true))
            .take(limit)
            .cloned()
            .collect();
        (filtered, total_count)
    }

    /// Mark a task complete. Records `completed_at` (now) and
    /// the optional `completion_note`. Returns the updated
    /// task. Errors with `NotFound` if no task has the given
    /// id.
    pub async fn complete(
        &self,
        id: &str,
        completion_note: Option<String>,
    ) -> Result<Task, TaskStoreError> {
        let now = Utc::now();
        let mut guard = self.tasks.lock().await;
        let pos = guard
            .iter()
            .position(|t| t.id == id)
            .ok_or_else(|| TaskStoreError::NotFound(id.to_string()))?;
        let task = &mut guard[pos];
        task.status = TaskStatus::Complete;
        task.completed_at = Some(now);
        task.completion_note = completion_note;
        let updated = task.clone();
        save_to_disk(&self.path, &guard).await?;
        Ok(updated)
    }

    /// Remove a task from the store. Returns the removed
    /// task's id. Errors with `NotFound` if no task matches.
    pub async fn delete(&self, id: &str) -> Result<String, TaskStoreError> {
        let mut guard = self.tasks.lock().await;
        let pos = guard
            .iter()
            .position(|t| t.id == id)
            .ok_or_else(|| TaskStoreError::NotFound(id.to_string()))?;
        guard.remove(pos);
        save_to_disk(&self.path, &guard).await?;
        Ok(id.to_string())
    }
}

/// Parse an operator-supplied due-date string as RFC 3339.
/// Public helper so the tool layer can validate inputs at the
/// parse boundary instead of inside execute().
pub fn parse_due_date(raw: &str) -> Result<DateTime<Utc>, TaskStoreError> {
    DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.with_timezone(&Utc))
        .map_err(|_| TaskStoreError::BadDueDate(raw.to_string()))
}

// Phase 144 — the OS-level primitives moved to
// `crate::secure_io`. This `save_to_disk` wraps
// the shared helpers with `StoredTasks` payload
// serialization + store-specific error context.

async fn save_to_disk(path: &Path, tasks: &[Task]) -> Result<(), TaskStoreError> {
    let stored = StoredTasks {
        schema_version: CURRENT_SCHEMA_VERSION,
        tasks: tasks.to_vec(),
    };
    let body = serde_json::to_string_pretty(&stored).map_err(|e| TaskStoreError::Parse {
        path: path.to_path_buf(),
        reason: format!("serialize: {e}"),
    })?;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            crate::secure_io::create_dir_all_secure(parent)
                .await
                .map_err(|source| TaskStoreError::Io {
                    path: parent.to_path_buf(),
                    source,
                })?;
        }
    }
    let tmp_path = crate::secure_io::with_tmp_suffix(path);
    crate::secure_io::write_secure(&tmp_path, body.as_bytes())
        .await
        .map_err(|source| TaskStoreError::Io {
            path: tmp_path.clone(),
            source,
        })?;
    fs::rename(&tmp_path, path)
        .await
        .map_err(|source| TaskStoreError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    Ok(())
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
            "aivyx-toolkit-tasks-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn open_creates_empty_store_when_file_absent() {
        let dir = scratch_dir();
        let path = dir.join("tasks.json");
        let store = TaskStore::open(path).await.expect("open");
        let (tasks, total) = store.list(None, 100).await;
        assert!(tasks.is_empty());
        assert_eq!(total, 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn create_persists_task_to_disk() {
        let dir = scratch_dir();
        let path = dir.join("tasks.json");
        let store = TaskStore::open(path.clone()).await.expect("open");
        let task = store
            .create("buy milk".to_string(), None, None)
            .await
            .expect("create");
        assert_eq!(task.title, "buy milk");
        assert_eq!(task.status, TaskStatus::Open);
        assert!(task.completed_at.is_none());

        // Reopen — task should still be there.
        let reopened = TaskStore::open(path).await.expect("reopen");
        let (tasks, total) = reopened.list(None, 100).await;
        assert_eq!(total, 1);
        assert_eq!(tasks[0].title, "buy milk");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn create_with_due_date_persists_it() {
        let dir = scratch_dir();
        let path = dir.join("tasks.json");
        let store = TaskStore::open(path.clone()).await.expect("open");
        let due = parse_due_date("2026-06-01T12:00:00Z").expect("parse");
        let task = store
            .create("call dentist".to_string(), Some("at 12pm".to_string()), Some(due))
            .await
            .expect("create");
        assert_eq!(task.notes.as_deref(), Some("at 12pm"));
        assert_eq!(task.due_date, Some(due));

        let reopened = TaskStore::open(path).await.expect("reopen");
        let (tasks, _) = reopened.list(None, 100).await;
        assert_eq!(tasks[0].due_date, Some(due));
        assert_eq!(tasks[0].notes.as_deref(), Some("at 12pm"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn list_filter_by_status_returns_only_matching() {
        let dir = scratch_dir();
        let path = dir.join("tasks.json");
        let store = TaskStore::open(path).await.expect("open");
        let t1 = store.create("a".to_string(), None, None).await.unwrap();
        let _t2 = store.create("b".to_string(), None, None).await.unwrap();
        let t3 = store.create("c".to_string(), None, None).await.unwrap();
        store.complete(&t1.id, None).await.unwrap();
        store.complete(&t3.id, None).await.unwrap();

        let (open, open_total) = store.list(Some(TaskStatus::Open), 100).await;
        assert_eq!(open_total, 1);
        assert_eq!(open[0].title, "b");

        let (_done, done_total) = store.list(Some(TaskStatus::Complete), 100).await;
        assert_eq!(done_total, 2);

        let (all, all_total) = store.list(None, 100).await;
        assert_eq!(all_total, 3);
        assert_eq!(all.len(), 3);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn list_limit_caps_returned_but_total_is_full() {
        let dir = scratch_dir();
        let path = dir.join("tasks.json");
        let store = TaskStore::open(path).await.expect("open");
        for i in 0..10 {
            store.create(format!("task {i}"), None, None).await.unwrap();
        }
        let (tasks, total) = store.list(None, 3).await;
        assert_eq!(total, 10, "total_count is unaffected by limit");
        assert_eq!(tasks.len(), 3, "returned slice respects limit");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn complete_marks_task_with_timestamp_and_note() {
        let dir = scratch_dir();
        let path = dir.join("tasks.json");
        let store = TaskStore::open(path).await.expect("open");
        let t = store
            .create("write tests".to_string(), None, None)
            .await
            .unwrap();
        let updated = store
            .complete(&t.id, Some("done well".to_string()))
            .await
            .expect("complete");
        assert_eq!(updated.status, TaskStatus::Complete);
        assert!(updated.completed_at.is_some());
        assert_eq!(updated.completion_note.as_deref(), Some("done well"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn complete_unknown_id_returns_not_found() {
        let dir = scratch_dir();
        let path = dir.join("tasks.json");
        let store = TaskStore::open(path).await.expect("open");
        match store.complete("does-not-exist", None).await {
            Err(TaskStoreError::NotFound(id)) => assert_eq!(id, "does-not-exist"),
            other => panic!("expected NotFound; got {other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn delete_removes_task_and_persists() {
        let dir = scratch_dir();
        let path = dir.join("tasks.json");
        let store = TaskStore::open(path.clone()).await.expect("open");
        let t = store.create("temp".to_string(), None, None).await.unwrap();
        let removed = store.delete(&t.id).await.expect("delete");
        assert_eq!(removed, t.id);

        let reopened = TaskStore::open(path).await.expect("reopen");
        let (tasks, total) = reopened.list(None, 100).await;
        assert_eq!(total, 0);
        assert!(tasks.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn delete_unknown_id_returns_not_found() {
        let dir = scratch_dir();
        let path = dir.join("tasks.json");
        let store = TaskStore::open(path).await.expect("open");
        match store.delete("ghost").await {
            Err(TaskStoreError::NotFound(id)) => assert_eq!(id, "ghost"),
            other => panic!("expected NotFound; got {other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn schema_too_new_surfaces_with_path() {
        let dir = scratch_dir();
        let path = dir.join("tasks.json");
        std::fs::write(
            &path,
            r#"{"schema_version": 99, "tasks": []}"#,
        )
        .unwrap();
        match TaskStore::open(path.clone()).await {
            Err(TaskStoreError::SchemaTooNew { found, supported, .. }) => {
                assert_eq!(found, 99);
                assert_eq!(supported, CURRENT_SCHEMA_VERSION);
            }
            other => panic!("expected SchemaTooNew; got {other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn parse_due_date_accepts_rfc3339_z() {
        let dt = parse_due_date("2026-06-01T12:00:00Z").expect("parse");
        assert_eq!(dt.to_rfc3339(), "2026-06-01T12:00:00+00:00");
    }

    #[tokio::test]
    async fn parse_due_date_accepts_offset_form() {
        let dt = parse_due_date("2026-06-01T12:00:00-05:00").expect("parse");
        // Normalized to UTC by `with_timezone`.
        assert_eq!(dt.to_rfc3339(), "2026-06-01T17:00:00+00:00");
    }

    #[tokio::test]
    async fn parse_due_date_rejects_garbage() {
        match parse_due_date("not a date") {
            Err(TaskStoreError::BadDueDate(raw)) => assert_eq!(raw, "not a date"),
            other => panic!("expected BadDueDate; got {other:?}"),
        }
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn save_writes_with_0600_perms() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch_dir();
        let path = dir.join("tasks.json");
        let store = TaskStore::open(path.clone()).await.expect("open");
        store.create("perm test".to_string(), None, None).await.unwrap();
        let meta = std::fs::metadata(&path).expect("stat");
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "got {mode:o}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn save_is_atomic_via_tmp_then_rename() {
        let dir = scratch_dir();
        let path = dir.join("tasks.json");
        let store = TaskStore::open(path.clone()).await.expect("open");
        store.create("atomic".to_string(), None, None).await.unwrap();
        let tmp = crate::secure_io::with_tmp_suffix(&path);
        assert!(!tmp.exists(), ".tmp must be renamed away");
        assert!(path.exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}
