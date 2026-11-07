//! Lightweight budget-entry storage for the
//! `budget.*` tools.
//!
//! Phase 143. JSON file at
//! `~/.aivyx-pa/tool-processes/toolkit/budget.json`
//! (0600 perms, atomic write-then-rename — same
//! pattern as Phase 125's task_store).
//!
//! ## Concurrency model
//!
//! `BudgetStore` holds the loaded entry list
//! behind a [`tokio::sync::Mutex`]. The harness
//! can dispatch multiple `budget.*` tools in
//! parallel; each operation takes the lock,
//! mutates in-memory state, persists to disk,
//! drops the lock. Operations are short
//! (microseconds + one fsync) so contention is
//! acceptable.
//!
//! ## On-disk format
//!
//! ```json
//! {
//!   "schema_version": 1,
//!   "entries": [
//!     {
//!       "id": "uuid-v4",
//!       "amount": 12.50,
//!       "category": "food",
//!       "note": "lunch" (or null),
//!       "recorded_at": "2026-06-03T12:30:00Z"
//!     }
//!   ]
//! }
//! ```
//!
//! `schema_version` lets a future phase extend
//! the shape without breaking existing operator
//! installs (same posture as task_store and
//! Gmail's token file).

use std::collections::{BTreeSet, HashMap};
use std::io;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Datelike, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::fs;
use tokio::sync::Mutex;
use uuid::Uuid;

pub const CURRENT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum BudgetStoreError {
    #[error("budget storage I/O failed at {path:?}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("budget storage parse failed at {path:?}: {reason}")]
    Parse { path: PathBuf, reason: String },
    #[error("budget storage at {path:?} has schema_version {found} but this build only understands up to {supported}")]
    SchemaTooNew {
        path: PathBuf,
        found: u32,
        supported: u32,
    },
    #[error("`amount` value {0} is not finite — must be a regular number")]
    BadAmount(f64),
    #[error("`category` must not be empty")]
    EmptyCategory,
    #[error("budget entry with id {0:?} not found")]
    NotFound(String),
}

/// Outcome of a [`BudgetStore::delete`] call.
/// Surfaced verbatim through the
/// `budget.delete` tool's output so the agent
/// can paraphrase "already removed" vs "removed
/// just now" if useful.
#[derive(Debug, Clone)]
pub struct DeleteOutcome {
    pub id: String,
    pub was_already_deleted: bool,
}

/// A single budget entry. Operator records an
/// amount (positive = expense, negative = income/
/// refund), a category (free-text), and an
/// optional note. The id + recorded_at are
/// assigned by the store at record time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetEntry {
    pub id: String,
    pub amount: f64,
    pub category: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub recorded_at: DateTime<Utc>,
}

/// On-disk wrapper around the entry list,
/// versioned at the file level for forward-compat
/// (same shape as task_store::StoredTasks).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredEntries {
    schema_version: u32,
    #[serde(default)]
    entries: Vec<BudgetEntry>,
}

/// The budget store. Holds the loaded entry list
/// under a Mutex; one instance per tool process
/// (constructed once in main and shared via
/// `Arc<BudgetStore>` to both `budget.*` tool
/// impls).
#[derive(Debug)]
pub struct BudgetStore {
    path: PathBuf,
    entries: Mutex<Vec<BudgetEntry>>,
}

// =====================================================================
// Phase 150 — category canonicalization + suggestion
// =====================================================================

/// Phase 150 — canonical form for a budget
/// category string: lowercase + trim. Pure
/// substrate.
///
/// `"Food"` → `"food"`. `" FOOD "` → `"food"`.
/// `""` → `""` (caller decides whether empty is
/// valid — `record` already rejects empty
/// categories with `BudgetStoreError::EmptyCategory`).
pub fn normalize_category(input: &str) -> String {
    input.trim().to_lowercase()
}

/// Phase 150 — fuzzy-match a category input
/// against a slice of known categories using
/// Levenshtein distance. Returns the closest
/// known category when:
/// - The distance is `>= 1` (exact match yields
///   `None` — there's nothing to suggest).
/// - The distance is `<= 2` (the documented
///   "close-typo" window).
///
/// Comparison happens in normalized form so
/// "Food" and "fod" yield "food" if "food"
/// is known. Pure substrate.
pub fn suggest_category(input: &str, known: &[String]) -> Option<String> {
    let normalized_input = normalize_category(input);
    if normalized_input.is_empty() {
        return None;
    }
    let mut best: Option<(usize, &String)> = None;
    for candidate in known {
        let normalized_candidate = normalize_category(candidate);
        if normalized_candidate.is_empty() {
            continue;
        }
        let d = levenshtein(&normalized_input, &normalized_candidate);
        if d == 0 {
            // Exact match — nothing to suggest.
            return None;
        }
        if d <= 2 {
            match best {
                None => best = Some((d, candidate)),
                Some((bd, _)) if d < bd => best = Some((d, candidate)),
                _ => {}
            }
        }
    }
    best.map(|(_, c)| c.clone())
}

/// Levenshtein distance between two strings —
/// classic two-row dynamic programming. Pure
/// function; no allocations beyond the two
/// O(min(len_a, len_b)) row buffers.
fn levenshtein(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let n = a_chars.len();
    let m = b_chars.len();
    if n == 0 {
        return m;
    }
    if m == 0 {
        return n;
    }
    let mut prev: Vec<usize> = (0..=m).collect();
    let mut curr: Vec<usize> = vec![0; m + 1];
    for i in 1..=n {
        curr[0] = i;
        for j in 1..=m {
            let cost = if a_chars[i - 1] == b_chars[j - 1] {
                0
            } else {
                1
            };
            curr[j] = (prev[j] + 1) // deletion
                .min(curr[j - 1] + 1) // insertion
                .min(prev[j - 1] + cost); // substitution
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[m]
}

impl BudgetStore {
    /// Open the store at `path`. Loads the file
    /// if it exists; initializes with an empty
    /// list if not. Either way the store is
    /// ready for reads and writes.
    pub async fn open(path: PathBuf) -> Result<Self, BudgetStoreError> {
        let entries = match fs::read_to_string(&path).await {
            Ok(body) => {
                let stored: StoredEntries = serde_json::from_str(&body).map_err(|e| {
                    BudgetStoreError::Parse {
                        path: path.clone(),
                        reason: e.to_string(),
                    }
                })?;
                if stored.schema_version > CURRENT_SCHEMA_VERSION {
                    return Err(BudgetStoreError::SchemaTooNew {
                        path,
                        found: stored.schema_version,
                        supported: CURRENT_SCHEMA_VERSION,
                    });
                }
                stored.entries
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                return Err(BudgetStoreError::Io { path, source: e });
            }
        };
        Ok(Self {
            path,
            entries: Mutex::new(entries),
        })
    }

    /// Append a new entry with a fresh UUID v4
    /// id and `recorded_at` = now. Persists to
    /// disk before returning. Validates that
    /// `amount` is finite (NaN / infinity
    /// rejected) and `category` is non-empty
    /// after trimming.
    pub async fn record(
        &self,
        amount: f64,
        category: String,
        note: Option<String>,
    ) -> Result<BudgetEntry, BudgetStoreError> {
        if !amount.is_finite() {
            return Err(BudgetStoreError::BadAmount(amount));
        }
        // Phase 150 — silent case-fold + trim.
        // "Food" → "food"; "  FOOD  " → "food".
        // Legacy entries (recorded pre-Phase 150)
        // stay as-recorded; only NEW entries are
        // normalized.
        let category = normalize_category(&category);
        if category.is_empty() {
            return Err(BudgetStoreError::EmptyCategory);
        }
        let entry = BudgetEntry {
            id: Uuid::new_v4().to_string(),
            amount,
            category,
            note: note.map(|n| n.trim().to_string()).filter(|n| !n.is_empty()),
            recorded_at: Utc::now(),
        };
        let mut guard = self.entries.lock().await;
        guard.push(entry.clone());
        save_to_disk(&self.path, &guard).await?;
        Ok(entry)
    }

    /// Phase 144 — partial update. Each `Option`
    /// arg either replaces the corresponding field
    /// or leaves it unchanged. The double-Option
    /// on `note` distinguishes "explicitly clear"
    /// (`Some(None)`) from "leave alone" (`None`).
    /// Returns the updated entry, or
    /// [`BudgetStoreError::NotFound`] when no
    /// entry matches the id.
    pub async fn update(
        &self,
        id: &str,
        amount: Option<f64>,
        category: Option<String>,
        note: Option<Option<String>>,
    ) -> Result<BudgetEntry, BudgetStoreError> {
        if let Some(a) = amount {
            if !a.is_finite() {
                return Err(BudgetStoreError::BadAmount(a));
            }
        }
        let normalized_category = match category {
            None => None,
            Some(c) => {
                // Phase 150 — silent case-fold +
                // trim. Same posture as `record`:
                // updated entries get the canonical
                // form even if the operator typed
                // "Food".
                let normalized = normalize_category(&c);
                if normalized.is_empty() {
                    return Err(BudgetStoreError::EmptyCategory);
                }
                Some(normalized)
            }
        };
        let mut guard = self.entries.lock().await;
        let pos = guard
            .iter()
            .position(|e| e.id == id)
            .ok_or_else(|| BudgetStoreError::NotFound(id.to_string()))?;
        if let Some(a) = amount {
            guard[pos].amount = a;
        }
        if let Some(c) = normalized_category {
            guard[pos].category = c;
        }
        if let Some(new_note) = note {
            guard[pos].note =
                new_note.map(|n| n.trim().to_string()).filter(|n| !n.is_empty());
        }
        let updated = guard[pos].clone();
        save_to_disk(&self.path, &guard).await?;
        Ok(updated)
    }

    /// Phase 144 — idempotent delete. Returns
    /// `was_already_deleted: true` when no entry
    /// matched the id; matches
    /// calendar.delete_event's posture so the
    /// agent doesn't have to special-case
    /// already-removed entries.
    pub async fn delete(&self, id: &str) -> Result<DeleteOutcome, BudgetStoreError> {
        let mut guard = self.entries.lock().await;
        let pos = guard.iter().position(|e| e.id == id);
        match pos {
            Some(idx) => {
                guard.remove(idx);
                save_to_disk(&self.path, &guard).await?;
                Ok(DeleteOutcome {
                    id: id.to_string(),
                    was_already_deleted: false,
                })
            }
            None => Ok(DeleteOutcome {
                id: id.to_string(),
                was_already_deleted: true,
            }),
        }
    }

    /// Aggregate entries within `[since, until)`
    /// (inclusive lower, exclusive upper) into a
    /// `BudgetSummary`. Pure read; the lock is
    /// released after the snapshot copy so
    /// concurrent writes aren't blocked during
    /// aggregation.
    pub async fn summary(
        &self,
        since: DateTime<Utc>,
        until: DateTime<Utc>,
    ) -> BudgetSummary {
        let snapshot: Vec<BudgetEntry> = {
            let guard = self.entries.lock().await;
            guard
                .iter()
                .filter(|e| e.recorded_at >= since && e.recorded_at < until)
                .cloned()
                .collect()
        };
        aggregate(&snapshot, since, until)
    }

    /// Phase 149 — month-over-month trend.
    /// Bucketizes entries into calendar months
    /// going back `months_back` from `now` (so the
    /// returned `months` slice has length
    /// `months_back`, oldest first). `now`'s
    /// month is the most-recent bucket. Optional
    /// `category` filter applies before
    /// bucketing.
    ///
    /// `now` parameterized so tests can pin a
    /// deterministic month boundary.
    pub async fn trend(
        &self,
        now: DateTime<Utc>,
        months_back: u32,
        category: Option<&str>,
    ) -> BudgetTrend {
        // Snapshot under lock.
        let snapshot: Vec<BudgetEntry> = {
            let guard = self.entries.lock().await;
            guard.clone()
        };
        aggregate_trend(&snapshot, now, months_back, category)
    }

    /// Phase 150 — unique sorted list of every
    /// category present in the store. Legacy
    /// entries (pre-Phase 150) may surface
    /// non-normalized variants here ("Food" vs
    /// "food"); new entries are always
    /// pre-normalized via `record` / `update`.
    /// Sorted ascending for stable LLM
    /// consumption.
    pub async fn known_categories(&self) -> Vec<String> {
        let guard = self.entries.lock().await;
        let set: BTreeSet<String> =
            guard.iter().map(|e| e.category.clone()).collect();
        set.into_iter().collect()
    }
}

/// Aggregated view of a slice of entries within
/// `[since, until)`. Pure substrate so the
/// aggregation itself can be tested without
/// touching the store.
#[derive(Debug, Clone, Serialize)]
pub struct BudgetSummary {
    pub since: DateTime<Utc>,
    pub until: DateTime<Utc>,
    pub total: f64,
    pub entry_count: usize,
    /// One bucket per category present in the
    /// window. Sorted descending by `total`; ties
    /// broken alphabetically on `category`.
    pub by_category: Vec<CategoryTotal>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CategoryTotal {
    pub category: String,
    pub total: f64,
    pub count: usize,
}

/// Phase 149 — per-month aggregation with
/// month-over-month delta + percentage change.
/// One bucket per calendar month in the window,
/// ordered chronologically (oldest first). The
/// first bucket's `delta_vs_prior` and
/// `pct_change_vs_prior` are `None`; subsequent
/// buckets compare against the immediately
/// preceding month.
#[derive(Debug, Clone, Serialize)]
pub struct BudgetTrend {
    pub months: Vec<MonthBucket>,
    /// If the caller filtered by category, the
    /// category name surfaces here. `None` means
    /// "all categories aggregated."
    pub category: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MonthBucket {
    /// `"YYYY-MM"` — operator-readable and
    /// stable-sort-friendly.
    pub month: String,
    pub total: f64,
    pub entry_count: usize,
    /// `total - prior.total`. `None` for the
    /// first bucket.
    pub delta_vs_prior: Option<f64>,
    /// `(total - prior.total) / prior.total *
    /// 100.0`. `None` for the first bucket OR
    /// when the prior month's total was zero
    /// (clean for the agent — no infinity
    /// special-case).
    pub pct_change_vs_prior: Option<f64>,
}

fn aggregate(
    entries: &[BudgetEntry],
    since: DateTime<Utc>,
    until: DateTime<Utc>,
) -> BudgetSummary {
    let mut by_cat: HashMap<String, (f64, usize)> = HashMap::new();
    let mut total: f64 = 0.0;
    for e in entries {
        total += e.amount;
        let bucket = by_cat.entry(e.category.clone()).or_insert((0.0, 0));
        bucket.0 += e.amount;
        bucket.1 += 1;
    }
    let mut by_category: Vec<CategoryTotal> = by_cat
        .into_iter()
        .map(|(category, (total, count))| CategoryTotal {
            category,
            total,
            count,
        })
        .collect();
    by_category.sort_by(|a, b| {
        b.total
            .partial_cmp(&a.total)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.category.cmp(&b.category))
    });
    BudgetSummary {
        since,
        until,
        total,
        entry_count: entries.len(),
        by_category,
    }
}

/// Phase 149 — pure-substrate month-over-month
/// aggregation. Pure so the bucket math + delta
/// math can be tested without touching the
/// store. `months_back` defines the number of
/// calendar-month buckets returned; the first
/// bucket is `months_back - 1` months before
/// the month of `now`, the last bucket is the
/// month of `now`.
fn aggregate_trend(
    entries: &[BudgetEntry],
    now: DateTime<Utc>,
    months_back: u32,
    category: Option<&str>,
) -> BudgetTrend {
    if months_back == 0 {
        return BudgetTrend {
            months: Vec::new(),
            category: category.map(str::to_string),
        };
    }

    // Build the chronologically-ordered list of
    // month boundaries we'll bucket into. Index 0
    // is the oldest month start; index N-1 is the
    // current month start. We need a final "next
    // month start" to upper-bound the last bucket.
    let mut boundaries: Vec<DateTime<Utc>> =
        Vec::with_capacity(months_back as usize + 1);
    // Walk back months_back-1 months from now to
    // find the oldest bucket's first day.
    let (mut year, mut month) = (now.year(), now.month());
    for _ in 0..(months_back - 1) {
        let (py, pm) = prev_month(year, month);
        year = py;
        month = pm;
    }
    // Push boundaries: oldest start → ... → next
    // month's start (upper bound for the current
    // month bucket).
    for _ in 0..=months_back {
        boundaries.push(month_start_utc(year, month));
        let (ny, nm) = next_month(year, month);
        year = ny;
        month = nm;
    }

    // Bucket the (optionally-filtered) entries.
    let mut buckets: Vec<(f64, usize)> = vec![(0.0, 0); months_back as usize];
    for e in entries {
        if let Some(cat) = category {
            if e.category != cat {
                continue;
            }
        }
        // Find the bucket whose [start, next_start)
        // window contains this entry's
        // recorded_at. Linear scan is fine —
        // months_back is capped at 36 elsewhere.
        for (idx, window) in boundaries.windows(2).enumerate() {
            let lo = window[0];
            let hi = window[1];
            if e.recorded_at >= lo && e.recorded_at < hi {
                buckets[idx].0 += e.amount;
                buckets[idx].1 += 1;
                break;
            }
        }
    }

    // Assemble MonthBuckets with delta + pct
    // computed against the prior bucket.
    let mut months: Vec<MonthBucket> = Vec::with_capacity(months_back as usize);
    for (idx, (total, count)) in buckets.iter().enumerate() {
        let start = boundaries[idx];
        let month_label = format!("{:04}-{:02}", start.year(), start.month());
        let (delta, pct) = if idx == 0 {
            (None, None)
        } else {
            let prior_total = buckets[idx - 1].0;
            let d = *total - prior_total;
            let p = if prior_total == 0.0 {
                None
            } else {
                Some((d / prior_total) * 100.0)
            };
            (Some(d), p)
        };
        months.push(MonthBucket {
            month: month_label,
            total: *total,
            entry_count: *count,
            delta_vs_prior: delta,
            pct_change_vs_prior: pct,
        });
    }

    BudgetTrend {
        months,
        category: category.map(str::to_string),
    }
}

fn month_start_utc(year: i32, month: u32) -> DateTime<Utc> {
    Utc.with_ymd_and_hms(year, month, 1, 0, 0, 0)
        .single()
        .expect("month start always exists for a valid year+month")
}

fn next_month(year: i32, month: u32) -> (i32, u32) {
    if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    }
}

fn prev_month(year: i32, month: u32) -> (i32, u32) {
    if month == 1 {
        (year - 1, 12)
    } else {
        (year, month - 1)
    }
}

// ---------- on-disk plumbing -----------
//
// Phase 144 — the OS-level primitives
// (create_dir_all_secure, write_secure,
// with_tmp_suffix) moved to
// `crate::secure_io` so the same helpers cover
// every JSON store. This wrapper handles the
// `StoredEntries` payload serialization +
// surfaces store-specific errors with the right
// path context.

async fn save_to_disk(
    path: &Path,
    entries: &[BudgetEntry],
) -> Result<(), BudgetStoreError> {
    let stored = StoredEntries {
        schema_version: CURRENT_SCHEMA_VERSION,
        entries: entries.to_vec(),
    };
    let body =
        serde_json::to_string_pretty(&stored).map_err(|e| BudgetStoreError::Parse {
            path: path.to_path_buf(),
            reason: format!("serialize: {e}"),
        })?;
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            crate::secure_io::create_dir_all_secure(parent)
                .await
                .map_err(|source| BudgetStoreError::Io {
                    path: parent.to_path_buf(),
                    source,
                })?;
        }
    }
    let tmp_path = crate::secure_io::with_tmp_suffix(path);
    crate::secure_io::write_secure(&tmp_path, body.as_bytes())
        .await
        .map_err(|source| BudgetStoreError::Io {
            path: tmp_path.clone(),
            source,
        })?;
    fs::rename(&tmp_path, path)
        .await
        .map_err(|source| BudgetStoreError::Io {
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
            "aivyx-toolkit-budget-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn ts(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn entry(amount: f64, category: &str, recorded_at: &str) -> BudgetEntry {
        BudgetEntry {
            id: Uuid::new_v4().to_string(),
            amount,
            category: category.to_string(),
            note: None,
            recorded_at: ts(recorded_at),
        }
    }

    #[test]
    fn aggregate_sums_totals_and_counts_by_category() {
        let entries = vec![
            entry(12.50, "food", "2026-06-01T08:00:00Z"),
            entry(4.75, "food", "2026-06-01T12:00:00Z"),
            entry(20.00, "transport", "2026-06-01T18:00:00Z"),
        ];
        let s = aggregate(
            &entries,
            ts("2026-06-01T00:00:00Z"),
            ts("2026-06-02T00:00:00Z"),
        );
        assert_eq!(s.entry_count, 3);
        assert!((s.total - 37.25).abs() < 1e-9);
        assert_eq!(s.by_category.len(), 2);
        // Sorted desc by total: transport (20.00),
        // food (17.25).
        assert_eq!(s.by_category[0].category, "transport");
        assert!((s.by_category[0].total - 20.00).abs() < 1e-9);
        assert_eq!(s.by_category[0].count, 1);
        assert_eq!(s.by_category[1].category, "food");
        assert!((s.by_category[1].total - 17.25).abs() < 1e-9);
        assert_eq!(s.by_category[1].count, 2);
    }

    #[test]
    fn aggregate_ties_broken_alphabetically() {
        let entries = vec![
            entry(10.0, "transport", "2026-06-01T00:00:00Z"),
            entry(10.0, "food", "2026-06-01T00:00:00Z"),
        ];
        let s = aggregate(
            &entries,
            ts("2026-06-01T00:00:00Z"),
            ts("2026-06-02T00:00:00Z"),
        );
        assert_eq!(s.by_category[0].category, "food");
        assert_eq!(s.by_category[1].category, "transport");
    }

    #[test]
    fn aggregate_empty_input_yields_empty_summary() {
        let s = aggregate(
            &[],
            ts("2026-06-01T00:00:00Z"),
            ts("2026-06-02T00:00:00Z"),
        );
        assert_eq!(s.entry_count, 0);
        assert_eq!(s.total, 0.0);
        assert!(s.by_category.is_empty());
    }

    // ---- Phase 149 — aggregate_trend ----

    #[test]
    fn aggregate_trend_zero_months_back_yields_empty_months() {
        let t = aggregate_trend(&[], ts("2026-06-15T12:00:00Z"), 0, None);
        assert!(t.months.is_empty());
        assert!(t.category.is_none());
    }

    #[test]
    fn aggregate_trend_single_month_no_delta() {
        // months_back = 1 → only the current
        // month, no prior comparison.
        let entries = vec![
            entry(10.00, "food", "2026-06-01T08:00:00Z"),
            entry(5.00, "food", "2026-06-15T08:00:00Z"),
        ];
        let t = aggregate_trend(&entries, ts("2026-06-15T12:00:00Z"), 1, None);
        assert_eq!(t.months.len(), 1);
        assert_eq!(t.months[0].month, "2026-06");
        assert!((t.months[0].total - 15.00).abs() < 1e-9);
        assert_eq!(t.months[0].entry_count, 2);
        assert!(t.months[0].delta_vs_prior.is_none());
        assert!(t.months[0].pct_change_vs_prior.is_none());
    }

    #[test]
    fn aggregate_trend_three_months_includes_deltas() {
        let entries = vec![
            // April
            entry(100.0, "food", "2026-04-15T08:00:00Z"),
            // May
            entry(120.0, "food", "2026-05-10T08:00:00Z"),
            entry(30.0, "food", "2026-05-20T08:00:00Z"), // May total 150
            // June
            entry(75.0, "food", "2026-06-01T08:00:00Z"),
        ];
        let t = aggregate_trend(&entries, ts("2026-06-15T12:00:00Z"), 3, None);
        assert_eq!(t.months.len(), 3);
        assert_eq!(t.months[0].month, "2026-04");
        assert!((t.months[0].total - 100.0).abs() < 1e-9);
        assert!(t.months[0].delta_vs_prior.is_none()); // first
        assert_eq!(t.months[1].month, "2026-05");
        assert!((t.months[1].total - 150.0).abs() < 1e-9);
        assert!((t.months[1].delta_vs_prior.unwrap() - 50.0).abs() < 1e-9);
        assert!((t.months[1].pct_change_vs_prior.unwrap() - 50.0).abs() < 1e-9); // +50%
        assert_eq!(t.months[2].month, "2026-06");
        assert!((t.months[2].total - 75.0).abs() < 1e-9);
        assert!((t.months[2].delta_vs_prior.unwrap() + 75.0).abs() < 1e-9); // -75
        assert!((t.months[2].pct_change_vs_prior.unwrap() + 50.0).abs() < 1e-9); // -50%
    }

    #[test]
    fn aggregate_trend_zero_prior_yields_null_pct() {
        // Prior month had no entries (or no
        // category-matching entries). The agent
        // should see null rather than infinity
        // for pct_change.
        let entries = vec![
            // Nothing in May.
            entry(40.0, "food", "2026-06-01T08:00:00Z"),
        ];
        let t = aggregate_trend(&entries, ts("2026-06-15T12:00:00Z"), 2, None);
        assert_eq!(t.months.len(), 2);
        // May bucket — empty.
        assert_eq!(t.months[0].month, "2026-05");
        assert_eq!(t.months[0].total, 0.0);
        // June bucket — delta vs zero-prior.
        assert!((t.months[1].delta_vs_prior.unwrap() - 40.0).abs() < 1e-9);
        assert!(
            t.months[1].pct_change_vs_prior.is_none(),
            "zero-prior must yield null pct, not infinity"
        );
    }

    #[test]
    fn aggregate_trend_category_filter_isolates_one_category() {
        let entries = vec![
            entry(100.0, "food", "2026-05-01T00:00:00Z"),
            entry(50.0, "transport", "2026-05-01T00:00:00Z"),
            entry(60.0, "food", "2026-06-01T00:00:00Z"),
        ];
        let t = aggregate_trend(
            &entries,
            ts("2026-06-15T12:00:00Z"),
            2,
            Some("food"),
        );
        assert_eq!(t.category.as_deref(), Some("food"));
        assert_eq!(t.months[0].month, "2026-05");
        assert!((t.months[0].total - 100.0).abs() < 1e-9); // food only
        assert_eq!(t.months[1].month, "2026-06");
        assert!((t.months[1].total - 60.0).abs() < 1e-9);
        // transport row dropped from totals.
    }

    #[test]
    fn aggregate_trend_handles_year_boundary() {
        // months_back spanning Dec 2025 → Jan 2026.
        let entries = vec![
            entry(200.0, "rent", "2025-12-01T00:00:00Z"),
            entry(220.0, "rent", "2026-01-01T00:00:00Z"),
        ];
        let t = aggregate_trend(&entries, ts("2026-01-15T12:00:00Z"), 2, None);
        assert_eq!(t.months.len(), 2);
        assert_eq!(t.months[0].month, "2025-12");
        assert_eq!(t.months[1].month, "2026-01");
        assert!((t.months[1].delta_vs_prior.unwrap() - 20.0).abs() < 1e-9);
    }

    #[test]
    fn aggregate_trend_entries_outside_window_ignored() {
        // months_back = 3 from June 2026 → window
        // is April–June. April entry counts; March
        // does not.
        let entries = vec![
            entry(500.0, "food", "2026-03-15T00:00:00Z"), // before window
            entry(100.0, "food", "2026-04-15T00:00:00Z"),
        ];
        let t = aggregate_trend(&entries, ts("2026-06-15T12:00:00Z"), 3, None);
        assert_eq!(t.months.len(), 3);
        assert!((t.months[0].total - 100.0).abs() < 1e-9); // April
        assert_eq!(t.months[1].total, 0.0); // May
        assert_eq!(t.months[2].total, 0.0); // June
    }

    #[tokio::test]
    async fn store_trend_round_trip_through_disk() {
        let dir = scratch_dir();
        let path = dir.join("budget.json");
        let store = BudgetStore::open(path.clone()).await.unwrap();
        // Record across two months using
        // recorded_at via direct entry push so the
        // test isn't time-dependent on Utc::now().
        // Use record() then manually mutate
        // recorded_at via re-open: simpler to seed
        // the file directly. Use record() and
        // verify the trend over current month.
        store
            .record(20.00, "food".to_string(), None)
            .await
            .unwrap();
        let now = Utc::now();
        let t = store.trend(now, 1, None).await;
        assert_eq!(t.months.len(), 1);
        assert!((t.months[0].total - 20.00).abs() < 1e-9);
    }

    // ---- Phase 150 — normalize / Levenshtein / suggest ----

    #[test]
    fn normalize_category_lowercases_and_trims() {
        assert_eq!(normalize_category("Food"), "food");
        assert_eq!(normalize_category("FOOD"), "food");
        assert_eq!(normalize_category("  food  "), "food");
        assert_eq!(normalize_category("  Eating Out  "), "eating out");
        assert_eq!(normalize_category(""), "");
        assert_eq!(normalize_category("   "), "");
    }

    #[test]
    fn levenshtein_handles_empty_inputs() {
        assert_eq!(levenshtein("", ""), 0);
        assert_eq!(levenshtein("food", ""), 4);
        assert_eq!(levenshtein("", "food"), 4);
    }

    #[test]
    fn levenshtein_identical_strings_yield_zero() {
        assert_eq!(levenshtein("food", "food"), 0);
        assert_eq!(levenshtein("a", "a"), 0);
    }

    #[test]
    fn levenshtein_known_edit_distances() {
        // One substitution.
        assert_eq!(levenshtein("food", "good"), 1);
        // One deletion.
        assert_eq!(levenshtein("food", "fod"), 1);
        // One insertion.
        assert_eq!(levenshtein("foo", "food"), 1);
        // Two edits (delete + substitute).
        assert_eq!(levenshtein("food", "f"), 3);
        // Completely different short strings.
        assert_eq!(levenshtein("foo", "bar"), 3);
    }

    #[test]
    fn suggest_category_returns_none_for_exact_match() {
        let known = vec!["food".to_string(), "transport".to_string()];
        assert!(suggest_category("food", &known).is_none());
        // Exact match after normalization.
        assert!(suggest_category("Food", &known).is_none());
    }

    #[test]
    fn suggest_category_returns_closest_within_distance_two() {
        let known = vec!["food".to_string(), "transport".to_string()];
        // "fod" → "food" (distance 1).
        assert_eq!(
            suggest_category("fod", &known),
            Some("food".to_string())
        );
        // "transprt" → "transport" (distance 1).
        assert_eq!(
            suggest_category("transprt", &known),
            Some("transport".to_string())
        );
        // "foof" → "food" (distance 1).
        assert_eq!(
            suggest_category("foof", &known),
            Some("food".to_string())
        );
    }

    #[test]
    fn suggest_category_returns_none_for_too_distant() {
        let known = vec!["food".to_string(), "transport".to_string()];
        // "groceries" is far from both (>2).
        assert!(suggest_category("groceries", &known).is_none());
    }

    #[test]
    fn suggest_category_returns_closest_when_multiple_within_threshold() {
        // "food" distance 1, "good" distance 2 → pick
        // food (closer).
        let known = vec!["food".to_string(), "good".to_string()];
        assert_eq!(
            suggest_category("fod", &known),
            Some("food".to_string())
        );
    }

    #[test]
    fn suggest_category_returns_none_for_empty_input() {
        let known = vec!["food".to_string()];
        assert!(suggest_category("", &known).is_none());
        assert!(suggest_category("   ", &known).is_none());
    }

    #[tokio::test]
    async fn record_normalizes_category_to_lowercase() {
        let dir = scratch_dir();
        let path = dir.join("budget.json");
        let store = BudgetStore::open(path.clone()).await.unwrap();
        let entry = store
            .record(10.00, "Food".to_string(), None)
            .await
            .unwrap();
        assert_eq!(entry.category, "food", "category should be normalized");
    }

    #[tokio::test]
    async fn update_normalizes_category_to_lowercase() {
        let dir = scratch_dir();
        let path = dir.join("budget.json");
        let store = BudgetStore::open(path.clone()).await.unwrap();
        let entry = store
            .record(10.00, "food".to_string(), None)
            .await
            .unwrap();
        let updated = store
            .update(&entry.id, None, Some("Transport".to_string()), None)
            .await
            .unwrap();
        assert_eq!(updated.category, "transport");
    }

    #[tokio::test]
    async fn known_categories_dedupes_and_sorts() {
        let dir = scratch_dir();
        let path = dir.join("budget.json");
        let store = BudgetStore::open(path.clone()).await.unwrap();
        store
            .record(10.00, "Transport".to_string(), None)
            .await
            .unwrap();
        store
            .record(15.00, "food".to_string(), None)
            .await
            .unwrap();
        store
            .record(20.00, "Food".to_string(), None) // normalizes to "food"
            .await
            .unwrap();
        let got = store.known_categories().await;
        // Three records but two unique
        // post-normalization categories,
        // ascending.
        assert_eq!(got, vec!["food".to_string(), "transport".to_string()]);
    }

    #[tokio::test]
    async fn record_then_summary_round_trip() {
        let dir = scratch_dir();
        let path = dir.join("budget.json");
        let store = BudgetStore::open(path.clone()).await.unwrap();
        let now_before = Utc::now();
        let e1 = store
            .record(12.50, "food".to_string(), Some("lunch".to_string()))
            .await
            .unwrap();
        let e2 = store
            .record(20.00, "transport".to_string(), None)
            .await
            .unwrap();
        let now_after = Utc::now();

        let s = store
            .summary(
                now_before - chrono::Duration::seconds(1),
                now_after + chrono::Duration::seconds(1),
            )
            .await;
        assert_eq!(s.entry_count, 2);
        assert!((s.total - 32.50).abs() < 1e-9);
        // Both entries persisted with ids.
        assert!(!e1.id.is_empty());
        assert!(!e2.id.is_empty());
        assert_eq!(e1.note, Some("lunch".to_string()));
        assert_eq!(e2.note, None);
    }

    #[tokio::test]
    async fn record_persists_across_reopen() {
        let dir = scratch_dir();
        let path = dir.join("budget.json");
        {
            let store = BudgetStore::open(path.clone()).await.unwrap();
            store
                .record(5.00, "snacks".to_string(), None)
                .await
                .unwrap();
        }
        // Re-open and verify.
        let store = BudgetStore::open(path.clone()).await.unwrap();
        let s = store
            .summary(ts("2020-01-01T00:00:00Z"), ts("2030-01-01T00:00:00Z"))
            .await;
        assert_eq!(s.entry_count, 1);
        assert!((s.total - 5.00).abs() < 1e-9);
    }

    #[tokio::test]
    async fn record_rejects_non_finite_amount() {
        let dir = scratch_dir();
        let path = dir.join("budget.json");
        let store = BudgetStore::open(path).await.unwrap();
        let err = store
            .record(f64::NAN, "food".to_string(), None)
            .await
            .unwrap_err();
        assert!(matches!(err, BudgetStoreError::BadAmount(_)));
    }

    #[tokio::test]
    async fn record_rejects_blank_category() {
        let dir = scratch_dir();
        let path = dir.join("budget.json");
        let store = BudgetStore::open(path).await.unwrap();
        let err = store
            .record(5.00, "   ".to_string(), None)
            .await
            .unwrap_err();
        assert!(matches!(err, BudgetStoreError::EmptyCategory));
    }

    #[tokio::test]
    async fn summary_window_excludes_entries_outside_range() {
        let dir = scratch_dir();
        let path = dir.join("budget.json");
        let store = BudgetStore::open(path).await.unwrap();
        store
            .record(10.00, "food".to_string(), None)
            .await
            .unwrap();
        // since/until = empty window before now → no
        // entries in range.
        let s = store
            .summary(ts("2020-01-01T00:00:00Z"), ts("2020-01-02T00:00:00Z"))
            .await;
        assert_eq!(s.entry_count, 0);
        assert_eq!(s.total, 0.0);
    }

    // ---- Phase 144 — update + delete ----

    #[tokio::test]
    async fn update_partial_amount_only_leaves_category_and_note_unchanged() {
        let dir = scratch_dir();
        let path = dir.join("budget.json");
        let store = BudgetStore::open(path.clone()).await.unwrap();
        let e = store
            .record(10.00, "food".to_string(), Some("lunch".to_string()))
            .await
            .unwrap();
        let updated = store
            .update(&e.id, Some(12.50), None, None)
            .await
            .unwrap();
        assert!((updated.amount - 12.50).abs() < 1e-9);
        assert_eq!(updated.category, "food");
        assert_eq!(updated.note, Some("lunch".to_string()));
    }

    #[tokio::test]
    async fn update_explicit_none_note_clears_note() {
        let dir = scratch_dir();
        let path = dir.join("budget.json");
        let store = BudgetStore::open(path.clone()).await.unwrap();
        let e = store
            .record(5.00, "snacks".to_string(), Some("desk grazing".to_string()))
            .await
            .unwrap();
        // Phase 144 — `Some(None)` is the
        // "explicitly clear" signal at the
        // substrate layer.
        let updated = store
            .update(&e.id, None, None, Some(None))
            .await
            .unwrap();
        assert_eq!(updated.note, None);
    }

    #[tokio::test]
    async fn update_some_note_replaces_existing() {
        let dir = scratch_dir();
        let path = dir.join("budget.json");
        let store = BudgetStore::open(path.clone()).await.unwrap();
        let e = store
            .record(5.00, "snacks".to_string(), Some("old".to_string()))
            .await
            .unwrap();
        let updated = store
            .update(&e.id, None, None, Some(Some("new".to_string())))
            .await
            .unwrap();
        assert_eq!(updated.note, Some("new".to_string()));
    }

    #[tokio::test]
    async fn update_missing_id_returns_not_found() {
        let dir = scratch_dir();
        let path = dir.join("budget.json");
        let store = BudgetStore::open(path.clone()).await.unwrap();
        let err = store
            .update("does-not-exist", Some(1.0), None, None)
            .await
            .unwrap_err();
        match err {
            BudgetStoreError::NotFound(id) => assert_eq!(id, "does-not-exist"),
            other => panic!("expected NotFound; got {other:?}"),
        }
    }

    #[tokio::test]
    async fn update_rejects_non_finite_amount() {
        let dir = scratch_dir();
        let path = dir.join("budget.json");
        let store = BudgetStore::open(path.clone()).await.unwrap();
        let e = store
            .record(5.00, "snacks".to_string(), None)
            .await
            .unwrap();
        let err = store
            .update(&e.id, Some(f64::INFINITY), None, None)
            .await
            .unwrap_err();
        assert!(matches!(err, BudgetStoreError::BadAmount(_)));
    }

    #[tokio::test]
    async fn update_rejects_blank_category() {
        let dir = scratch_dir();
        let path = dir.join("budget.json");
        let store = BudgetStore::open(path.clone()).await.unwrap();
        let e = store
            .record(5.00, "snacks".to_string(), None)
            .await
            .unwrap();
        let err = store
            .update(&e.id, None, Some("   ".to_string()), None)
            .await
            .unwrap_err();
        assert!(matches!(err, BudgetStoreError::EmptyCategory));
    }

    #[tokio::test]
    async fn delete_present_entry_removes_it() {
        let dir = scratch_dir();
        let path = dir.join("budget.json");
        let store = BudgetStore::open(path.clone()).await.unwrap();
        let e = store
            .record(5.00, "snacks".to_string(), None)
            .await
            .unwrap();
        let outcome = store.delete(&e.id).await.unwrap();
        assert_eq!(outcome.id, e.id);
        assert!(!outcome.was_already_deleted);
        // Verify it's gone via summary.
        let s = store
            .summary(ts("2020-01-01T00:00:00Z"), ts("2030-01-01T00:00:00Z"))
            .await;
        assert_eq!(s.entry_count, 0);
    }

    #[tokio::test]
    async fn delete_missing_entry_is_idempotent() {
        let dir = scratch_dir();
        let path = dir.join("budget.json");
        let store = BudgetStore::open(path.clone()).await.unwrap();
        let outcome = store.delete("never-existed").await.unwrap();
        assert_eq!(outcome.id, "never-existed");
        assert!(
            outcome.was_already_deleted,
            "missing id must surface as was_already_deleted = true"
        );
    }

    #[tokio::test]
    async fn update_then_delete_persists_across_reopen() {
        let dir = scratch_dir();
        let path = dir.join("budget.json");
        let id_alpha: String;
        let id_beta: String;
        {
            let store = BudgetStore::open(path.clone()).await.unwrap();
            let alpha = store
                .record(10.00, "alpha".to_string(), None)
                .await
                .unwrap();
            let beta = store
                .record(20.00, "beta".to_string(), None)
                .await
                .unwrap();
            id_alpha = alpha.id;
            id_beta = beta.id;
            // Update alpha; delete beta.
            store
                .update(&id_alpha, Some(15.00), None, None)
                .await
                .unwrap();
            store.delete(&id_beta).await.unwrap();
        }
        // Re-open and verify both changes survived.
        let store = BudgetStore::open(path.clone()).await.unwrap();
        let s = store
            .summary(ts("2020-01-01T00:00:00Z"), ts("2030-01-01T00:00:00Z"))
            .await;
        assert_eq!(s.entry_count, 1);
        assert!((s.total - 15.00).abs() < 1e-9);
        assert_eq!(s.by_category[0].category, "alpha");
        // Beta is gone; second delete still
        // idempotent.
        let outcome = store.delete(&id_beta).await.unwrap();
        assert!(outcome.was_already_deleted);
    }

    #[tokio::test]
    async fn open_rejects_future_schema_version() {
        let dir = scratch_dir();
        let path = dir.join("budget.json");
        fs::write(
            &path,
            r#"{"schema_version": 99, "entries": []}"#,
        )
        .await
        .unwrap();
        let err = BudgetStore::open(path).await.unwrap_err();
        assert!(matches!(err, BudgetStoreError::SchemaTooNew { .. }));
    }
}
