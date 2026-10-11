//! Phase 77 — the recall-feedback signal log.
//!
//! One row per turn that automatic recall (Phase 76) injected
//! memory into. The reflection loop reads a time window of
//! these and pairs them against the audit chain's `TurnEnded`
//! outcomes to learn which memories actually help (Q1a — a
//! structural, non-LLM signal).
//!
//! Backed by [`aivyx_storage::KeyDomain::RecallEvents`], a
//! dedicated encrypted domain so the signal can be GC-clamped
//! independently and a corrupt row degrades learning, never
//! recall or memory. This is **best-effort** infrastructure:
//! it is not a tamper-evident chain like the persona-proposal
//! log — a lost or skipped row just means the loop misses one
//! turn's worth of signal.

use serde::{Deserialize, Serialize};

// moved to the wasm-clean aivyx-ipc crate (Chapter M.2d-2); re-exported here.
pub use aivyx_ipc::insights::{RecallJudgment};

use aivyx_core::SessionId;
use aivyx_storage::DomainHandle;


/// One memory that auto-recall injected into a turn, with the
/// cosine score it was ranked at.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallHit {
    pub topic: String,
    pub seq: u64,
    pub score: f32,
    /// Phase 84 — `true` iff this hit was injected by
    /// cluster-aware co-recall (a Phase 83 affined sibling the
    /// literal query missed), `false` for a primary
    /// keyword/semantic hit. `#[serde(default)]` keeps the
    /// recall-log + IPC round-trip back-compatible (older rows
    /// decode as `false`). Phase 77 `correlate_detailed` reads
    /// only `(topic, seq)` so it is unaffected; the Phase 83
    /// co-occurrence fold uses this to **exclude** cluster
    /// hits (the self-policing — the ledger never learns from
    /// its own expansion).
    #[serde(default)]
    pub cluster: bool,
    /// Phase 91 — the LLM-judged classification, recorded by
    /// the reflection-cron `run_recall_judgment_pass` when
    /// `[recall_judgment]` is enabled. `None` for un-judged
    /// hits (the pass is off, the cron hasn't run yet, or the
    /// per-cycle cap rolled this hit to a later cycle).
    /// `#[serde(default, skip_serializing_if = "Option::is_none")]`
    /// keeps the recall-log + IPC round-trip back-compatible —
    /// the established wire-compat shape Phase 84 introduced
    /// with `cluster: bool`. No existing accumulator (Phase
    /// 82 / 83 / 85 / 87 / 88) consumes this field in v1
    /// (Q3a augment, not replace).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub judgment: Option<RecallJudgment>,
}

/// The recall that happened on one turn. `ts_secs` is wall
/// clock at injection time; `session_id` ties it to the
/// conversation so the correlator can apply the
/// "operator-corrected-next-turn" heuristic per session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallEvent {
    pub ts_secs: u64,
    pub session_id: SessionId,
    pub hits: Vec<RecallHit>,
    /// Phase 178 — the (truncated) operator message that drove
    /// this turn's recall. Captured so the correction-judgment
    /// pass can read a corrected turn's follow-up message and
    /// classify it (Rework / Praise / Unrelated). `#[serde(default)]`
    /// so pre-Phase-178 rows decode with an empty string (and
    /// simply aren't judgeable). The recall log is HKDF-domain-
    /// encrypted at rest; the text is truncated at capture.
    #[serde(default)]
    pub query_text: String,
}

/// Phase 178 — max characters of the operator query stored on a
/// `RecallEvent`. Enough for the judge to classify a follow-up;
/// bounded so the recall log doesn't bloat or hold long pastes.
pub const QUERY_TEXT_MAX_CHARS: usize = 400;

/// Phase 178 — truncate an operator message to
/// [`QUERY_TEXT_MAX_CHARS`] on a char boundary for capture.
pub fn truncate_query_text(msg: &str) -> String {
    let trimmed = msg.trim();
    if trimmed.chars().count() <= QUERY_TEXT_MAX_CHARS {
        return trimmed.to_string();
    }
    trimmed.chars().take(QUERY_TEXT_MAX_CHARS).collect()
}

#[derive(Debug, thiserror::Error)]
pub enum RecallLogError {
    #[error("recall log storage error: {0}")]
    Storage(String),
    #[error("recall log encode error: {0}")]
    Encode(String),
}

/// Persistent recall-event log over
/// [`aivyx_storage::KeyDomain::RecallEvents`].
///
/// Key layout: `ts_secs_be(8) || uuid(16)`. The big-endian
/// timestamp prefix makes a `scan_prefix(&[])` return rows in
/// time order; the UUID suffix makes every append
/// collision-free without a persisted counter (two recalls in
/// the same second still get distinct keys). Reads filter the
/// full scan by timestamp — the same bounded-scan discipline
/// `RedbMemory` uses, kept honest by the independent GC clamp.
pub struct PersistentRecallLog {
    storage: DomainHandle,
}

impl PersistentRecallLog {
    pub fn new(storage: DomainHandle) -> Self {
        Self { storage }
    }

    fn event_key(ts_secs: u64) -> Vec<u8> {
        let mut k = Vec::with_capacity(24);
        k.extend_from_slice(&ts_secs.to_be_bytes());
        k.extend_from_slice(uuid::Uuid::new_v4().as_bytes());
        k
    }

    /// Append one recall event. Best-effort: callers treat a
    /// failure as "this turn's signal is lost," never fatal.
    pub async fn append(
        &self,
        event: &RecallEvent,
    ) -> Result<(), RecallLogError> {
        let key = Self::event_key(event.ts_secs);
        let value = serde_json::to_vec(event)
            .map_err(|e| RecallLogError::Encode(e.to_string()))?;
        self.storage
            .put(&key, &value)
            .await
            .map_err(|e| RecallLogError::Storage(e.to_string()))
    }

    /// Every event with `ts_secs >= since_secs`, ascending by
    /// timestamp. The reflection loop's lookback input.
    pub async fn events_since(
        &self,
        since_secs: u64,
    ) -> Result<Vec<RecallEvent>, RecallLogError> {
        let rows = self
            .storage
            .scan_prefix(&[])
            .await
            .map_err(|e| RecallLogError::Storage(e.to_string()))?;
        let mut out = Vec::new();
        for (_k, v) in &rows {
            let ev: RecallEvent = serde_json::from_slice(v)
                .map_err(|e| RecallLogError::Encode(e.to_string()))?;
            if ev.ts_secs >= since_secs {
                out.push(ev);
            }
        }
        out.sort_by_key(|e| e.ts_secs);
        Ok(out)
    }

    /// Delete every event older than `cutoff_secs`. Bounded-
    /// growth clamp, run on the same reflection cadence so the
    /// signal log can't grow without limit. Uses the key's
    /// timestamp prefix — no value decode needed. Returns the
    /// number of rows removed.
    pub async fn gc_older_than(
        &self,
        cutoff_secs: u64,
    ) -> Result<usize, RecallLogError> {
        let rows = self
            .storage
            .scan_prefix(&[])
            .await
            .map_err(|e| RecallLogError::Storage(e.to_string()))?;
        let mut deleted = 0usize;
        for (k, _v) in &rows {
            if k.len() < 8 {
                continue;
            }
            let mut buf = [0u8; 8];
            buf.copy_from_slice(&k[..8]);
            if u64::from_be_bytes(buf) < cutoff_secs {
                self.storage
                    .delete(k)
                    .await
                    .map_err(|e| RecallLogError::Storage(e.to_string()))?;
                deleted += 1;
            }
        }
        Ok(deleted)
    }

    /// Phase 91 — variant of [`Self::events_since`] that also
    /// returns each row's storage key, so the caller can call
    /// [`Self::update_event`] to write back an updated event
    /// (e.g. with `judgment` field patched). Same `ts_secs >=
    /// since_secs` filter; same ascending order.
    pub async fn events_with_keys_since(
        &self,
        since_secs: u64,
    ) -> Result<Vec<(Vec<u8>, RecallEvent)>, RecallLogError> {
        let rows = self
            .storage
            .scan_prefix(&[])
            .await
            .map_err(|e| RecallLogError::Storage(e.to_string()))?;
        let mut out = Vec::new();
        for (k, v) in &rows {
            let ev: RecallEvent = serde_json::from_slice(v)
                .map_err(|e| RecallLogError::Encode(e.to_string()))?;
            if ev.ts_secs >= since_secs {
                out.push((k.clone(), ev));
            }
        }
        out.sort_by_key(|(_, e)| e.ts_secs);
        Ok(out)
    }

    /// Phase 91 — overwrite the row at `key` with `event`. The
    /// caller obtained `key` from
    /// [`Self::events_with_keys_since`]; mutating only the
    /// `judgment` field on existing hits is the v1 use case.
    /// The key is preserved verbatim (no re-keying) so a
    /// concurrent `gc_older_than` still works.
    pub async fn update_event(
        &self,
        key: &[u8],
        event: &RecallEvent,
    ) -> Result<(), RecallLogError> {
        let value = serde_json::to_vec(event)
            .map_err(|e| RecallLogError::Encode(e.to_string()))?;
        self.storage
            .put(key, &value)
            .await
            .map_err(|e| RecallLogError::Storage(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_crypto::MasterKey;
    use aivyx_storage::{KeyDomain, RedbStorage, Storage, StorageConfig};
    use std::path::PathBuf;

    #[test]
    fn truncate_query_text_trims_and_caps() {
        assert_eq!(truncate_query_text("  hi there  "), "hi there");
        let long: String = "x".repeat(QUERY_TEXT_MAX_CHARS + 50);
        let t = truncate_query_text(&long);
        assert_eq!(t.chars().count(), QUERY_TEXT_MAX_CHARS);
        // Multibyte safe (no panic on a char-boundary cut).
        let emoji: String = "🦀".repeat(QUERY_TEXT_MAX_CHARS + 10);
        assert_eq!(
            truncate_query_text(&emoji).chars().count(),
            QUERY_TEXT_MAX_CHARS
        );
    }

    #[test]
    fn recall_event_query_text_round_trips_and_defaults() {
        // New field serializes; an old row without it decodes to "".
        let ev = RecallEvent {
            ts_secs: 5,
            session_id: SessionId::new(),
            hits: vec![],
            query_text: "fix the auth bug".into(),
        };
        let json = serde_json::to_string(&ev).unwrap();
        let back: RecallEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(back.query_text, "fix the auth bug");
        // Pre-178 shape (no query_text) → default "".
        let old: String = r#"{"ts_secs":5,"session_id":""#.to_string()
            + ev.session_id.to_string().as_str()
            + r#"","hits":[]}"#;
        let decoded: RecallEvent = serde_json::from_str(&old).unwrap();
        assert_eq!(decoded.query_text, "");
    }
    use std::sync::Arc;

    struct Scratch {
        dir: PathBuf,
    }
    impl Scratch {
        fn new() -> Self {
            let base = std::env::var("TMPDIR")
                .unwrap_or_else(|_| "/tmp".to_string());
            let dir = PathBuf::from(base).join(format!(
                "aivyx-recall-log-test-{}",
                uuid::Uuid::new_v4()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Scratch { dir }
        }
        fn store_path(&self) -> PathBuf {
            self.dir.join("store.redb")
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    async fn open_log(scratch: &Scratch, mb: u8) -> PersistentRecallLog {
        let master = MasterKey::from_raw([mb; 32]);
        let storage: Arc<dyn Storage> = RedbStorage::open(
            StorageConfig::new(scratch.store_path()),
            master,
        )
        .await
        .expect("open store");
        PersistentRecallLog::new(storage.domain(KeyDomain::RecallEvents))
    }

    fn ev(ts: u64, topic: &str, seq: u64, score: f32) -> RecallEvent {
        RecallEvent {
            ts_secs: ts,
            session_id: SessionId::new(),
            query_text: String::new(),
            hits: vec![RecallHit {
                topic: topic.into(),
                seq,
                score,
                cluster: false,
                judgment: None,
            }],
        }
    }

    #[tokio::test]
    async fn append_then_events_since_round_trips_and_filters() {
        let scratch = Scratch::new();
        let log = open_log(&scratch, 1).await;
        log.append(&ev(100, "a", 0, 0.9)).await.unwrap();
        log.append(&ev(200, "b", 1, 0.8)).await.unwrap();
        log.append(&ev(300, "c", 2, 0.7)).await.unwrap();

        // Window cuts off everything before 200.
        let got = log.events_since(200).await.unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].ts_secs, 200);
        assert_eq!(got[1].ts_secs, 300);
        assert_eq!(got[0].hits[0].topic, "b");
        assert!((got[1].hits[0].score - 0.7).abs() < 1e-6);

        // since=0 returns all, ascending.
        let all = log.events_since(0).await.unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].ts_secs, 100);
    }

    #[tokio::test]
    async fn same_second_appends_do_not_collide() {
        let scratch = Scratch::new();
        let log = open_log(&scratch, 2).await;
        // Three recalls in the very same wall-clock second.
        log.append(&ev(500, "x", 0, 0.5)).await.unwrap();
        log.append(&ev(500, "y", 1, 0.6)).await.unwrap();
        log.append(&ev(500, "z", 2, 0.7)).await.unwrap();
        let all = log.events_since(0).await.unwrap();
        assert_eq!(all.len(), 3, "uuid suffix must keep keys distinct");
    }

    #[tokio::test]
    async fn gc_older_than_clamps_and_counts() {
        let scratch = Scratch::new();
        let log = open_log(&scratch, 3).await;
        log.append(&ev(100, "old", 0, 0.1)).await.unwrap();
        log.append(&ev(150, "old2", 1, 0.1)).await.unwrap();
        log.append(&ev(900, "fresh", 2, 0.9)).await.unwrap();

        let removed = log.gc_older_than(200).await.unwrap();
        assert_eq!(removed, 2);
        let left = log.events_since(0).await.unwrap();
        assert_eq!(left.len(), 1);
        assert_eq!(left[0].hits[0].topic, "fresh");
    }

    #[tokio::test]
    async fn empty_log_reads_empty_not_error() {
        let scratch = Scratch::new();
        let log = open_log(&scratch, 4).await;
        assert!(log.events_since(0).await.unwrap().is_empty());
        assert_eq!(log.gc_older_than(999).await.unwrap(), 0);
    }

    /// Phase 84 — the `cluster` marker round-trips, and a row
    /// written before Phase 84 (no `cluster` key) decodes as
    /// `false` via `#[serde(default)]` (back-compat).
    #[test]
    fn cluster_marker_round_trips_and_defaults_false() {
        let hit = RecallHit {
            topic: "deploy".into(),
            seq: 7,
            score: 0.9,
            cluster: true,
            judgment: None,
        };
        let j = serde_json::to_string(&hit).unwrap();
        let back: RecallHit =
            serde_json::from_str(&j).unwrap();
        assert_eq!(back, hit);
        assert!(back.cluster);

        // A pre-Phase-84 row has no `cluster` field at all.
        let legacy = r#"{"topic":"t","seq":1,"score":0.5}"#;
        let decoded: RecallHit =
            serde_json::from_str(legacy).unwrap();
        assert!(
            !decoded.cluster,
            "missing `cluster` must default to false"
        );
    }

    /// Phase 91 — `judgment: Option<RecallJudgment>` wire-
    /// compat. An old `RecallHit` row (Phase 84-style, no
    /// `judgment` field) decodes as `judgment: None`; a
    /// fresh row with `judgment: None` serializes WITHOUT
    /// the field thanks to
    /// `#[serde(skip_serializing_if = "Option::is_none")]`,
    /// so an old reader still parses the new JSON.
    #[test]
    fn judgment_field_is_back_compat_with_phase_84_rows() {
        // Pre-Phase-91 row (Phase 84 shape with `cluster`).
        let legacy = r#"
            {"topic":"deploy","seq":7,"score":0.9,
             "cluster":false}
        "#;
        let decoded: RecallHit =
            serde_json::from_str(legacy).unwrap();
        assert!(
            decoded.judgment.is_none(),
            "missing `judgment` must default to None"
        );

        // A fresh row with `judgment: None` serializes
        // without the field (skip_serializing_if). Old
        // readers parse this as a pre-Phase-91 row.
        let fresh = RecallHit {
            topic: "deploy".into(),
            seq: 8,
            score: 0.8,
            cluster: false,
            judgment: None,
        };
        let json = serde_json::to_string(&fresh).unwrap();
        assert!(
            !json.contains("judgment"),
            "judgment: None must be omitted from JSON: {json}"
        );

        // A fresh row with `judgment: Some(Used)` serializes
        // the field with the snake_case label.
        let judged = RecallHit {
            topic: "deploy".into(),
            seq: 9,
            score: 0.7,
            cluster: false,
            judgment: Some(RecallJudgment::Used),
        };
        let json = serde_json::to_string(&judged).unwrap();
        assert!(
            json.contains("\"judgment\":\"used\""),
            "snake_case judgment label expected: {json}"
        );
        let back: RecallHit =
            serde_json::from_str(&json).unwrap();
        assert_eq!(back.judgment, Some(RecallJudgment::Used));
    }
}
