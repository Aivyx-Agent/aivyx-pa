//! `PersistentAuditLog` — Phase 7 task 1.
//!
//! A durable `AuditHook` whose HMAC chain survives process restarts. Sits
//! as a sibling to `AuditBridge`, not a replacement: callers that want
//! in-memory audit (tests, short-lived tools) keep using `AuditBridge`;
//! callers that want durability (the `aivyx-pa` binary) wrap their
//! `Arc<dyn Storage>` in `PersistentAuditLog::open` instead.
//!
//! ## Why this file, and not an amendment to `AuditHook`
//!
//! `aivyx_core::AuditHook::on_event(&self, tag)` is **sync** and the turn
//! loop calls it inline. `Storage::put` is **async**. Phase 7 opened with
//! Q4 on the table: amend the trait to `async fn on_event(...)` so the
//! persist write can be awaited directly? The answer — held over from
//! the six-phase DESIGN.md empty-diff streak — was no. The trait stays
//! sync, and the async impedance gets solved in this one module by a
//! bounded channel plus a background drain task.
//!
//! The **five invariants** that design has to preserve (without which
//! "audit survives a crash" is a lie):
//!
//! 1. **Chain computation is sync and on the hot path.** The HMAC tag for
//!    entry `N` is computed inside `on_event` before it returns, so a
//!    crash between `on_event` and the drain task persisting entry `N`
//!    loses the tail of the log but never produces a log with a broken
//!    chain. The in-memory `HmacChainLog` is the authority; disk is a
//!    replica that catches up.
//! 2. **Append order equals disk order.** The drain task consumes the
//!    channel in FIFO order (`mpsc` is per-receiver FIFO) and writes
//!    sequentially — no concurrent `put`s racing on the same domain.
//! 3. **Persist failures are observed at most one event later.** If the
//!    drain task's `put` fails at seq `N`, the health flag flips before
//!    `on_event` is called for seq `N+1`, so the next `on_event` call
//!    returns without appending and routes the error through the
//!    handler. Worst case: one extra in-memory entry gets chained on
//!    top of an un-persisted tail, but that entry is never acknowledged
//!    upstream as audited — see invariant 4.
//! 4. **A chain-rejected event is never acked as audited.** If
//!    `HmacChainLog::append` itself fails (e.g. `Serialize`,
//!    `LockPoisoned`), the handler runs *before* anything is pushed to
//!    the channel, so the drain task never sees a tombstone.
//! 5. **Drain task lifetime is bounded by `PersistentAuditLog`.** The
//!    `JoinHandle` is stored on the struct and `abort()`ed in `Drop`,
//!    so a dropped `PersistentAuditLog` cannot leave a drain task alive
//!    against a closed storage handle.
//! 6. **The persisted chain anchor never claims a seq the disk doesn't
//!    actually have yet.** `PersistentAuditLog::open`'s reopen-path
//!    scan re-derives sequence numbers purely from scan position —
//!    which means a *tail* truncation (deleting the last N on-disk
//!    rows, leaving a shorter but otherwise perfectly self-consistent
//!    chain) was structurally undetectable: "the log stopped growing
//!    here" and "the log's tail was deleted" produce byte-identical
//!    on-disk state. Closing that gap needs something outside the
//!    scan itself to compare against — a small anchor record (last
//!    known `seq` + `mac`) stored under a reserved key in the same
//!    `KeyDomain::Audit` domain (see `CHAIN_ANCHOR_KEY`). The subtle
//!    part is *when* that anchor gets written: it is updated from
//!    inside the drain task, immediately **after** `handle.put` for
//!    the corresponding entry has itself returned `Ok` (i.e. after
//!    that entry is durably committed) — never synchronously inside
//!    `append()`, where the entry is only chained in memory and not
//!    yet even queued to the drain task's persistence path. Writing
//!    the anchor any earlier would let a crash between "chain the
//!    entry" and "the drain task persists it" leave an anchor that
//!    claims a seq the disk never actually received, which is a false
//!    positive on the very next open — exactly the kind of new
//!    failure mode this mechanism must not introduce into the chain
//!    it's meant to protect. The consequence of this ordering is that
//!    the anchor can only ever *lag* the true on-disk tail (benign —
//!    treated as "no news," not tamper evidence), never precede it;
//!    `check_tail_anchor` only rejects an anchor that is *ahead of*,
//!    or disagrees with, what a fresh scan actually finds. See
//!    `PHASE_7.md` Q2 for the historical context: this was flagged at
//!    design time as a known, narrower-than-feared gap ("an attacker
//!    who deletes the most recent session entirely") and explicitly
//!    deferred rather than solved by a new `ChainLinked` event
//!    variant; this anchor closes it without touching the
//!    `AuditEvent` schema.
//!
//! ## On-disk shape
//!
//! Key: `b"a\0" || seq.to_be_bytes()` (9 bytes). Big-endian means the
//! natural lex order of the `scan_prefix(b"a\0")` result equals the
//! natural numeric order of `seq`, so the reopen path consumes the scan
//! in one pass without sorting.
//!
//! Value: `serde_json::to_vec(&SignedEntry)`. The chain's integrity
//! comes from `serde_jcs`-canonical bytes over the `event` field during
//! MAC computation — the on-disk envelope just needs to round-trip
//! `SignedEntry` faithfully, which plain `serde_json` already does for
//! this struct shape.

use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use aivyx_storage::{DomainHandle, KeyDomain, ScanRow, Storage};

use crate::{
    AuditError, AuditEvent, AuditLog, AuditWriter, HmacChainLog, SignedEntry,
};

/// Durable audit hook. See module docs for the invariant list.
pub struct PersistentAuditLog {
    chain: Arc<HmacChainLog>,
    sender: mpsc::Sender<SignedEntry>,
    health: Arc<AtomicBool>,
    first_error: Arc<StdMutex<Option<String>>>,
    on_error: Arc<dyn Fn(AuditError) + Send + Sync>,
    /// Held so `Drop` can abort the drain task. Optioned only to allow
    /// `take()` inside `Drop`; always `Some` after construction.
    drain_handle: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for PersistentAuditLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PersistentAuditLog")
            .field("len", &self.chain.len())
            .field("healthy", &self.health.load(Ordering::SeqCst))
            .finish()
    }
}

/// Channel capacity. Large enough to absorb a burst from a turn (turns
/// emit ~4-10 events) without blocking the sync caller, small enough
/// that sustained backpressure surfaces instead of growing unbounded.
const CHANNEL_CAPACITY: usize = 1024;

/// Prefix for every audit entry key in `KeyDomain::Audit`. The
/// trailing `\0` protects against collision with any future namespace
/// the audit domain might grow.
const AUDIT_KEY_PREFIX: &[u8] = b"a\0";

fn audit_key(seq: u64) -> Vec<u8> {
    let mut key = Vec::with_capacity(AUDIT_KEY_PREFIX.len() + 8);
    key.extend_from_slice(AUDIT_KEY_PREFIX);
    key.extend_from_slice(&seq.to_be_bytes());
    key
}

// ---------------------------------------------------------------------------
// Chain anchor — tail-truncation detection (Task 11, 2026-09-16 security
// audit). See module docs, invariant 6, for the ordering rationale.
// ---------------------------------------------------------------------------

/// Reserved key for the persisted chain anchor. Deliberately **not**
/// `AUDIT_KEY_PREFIX`-shaped: it starts with `z`, which sorts well
/// outside `scan_prefix(AUDIT_KEY_PREFIX)`'s `[a\0, a\1)` range (see
/// `next_lex`), so it can never be swept up by the entry scan or
/// mistaken for an entry by `seq_from_key_bytes`.
///
/// Lives inside `KeyDomain::Audit` (via ordinary `DomainHandle::get`/
/// `put`) rather than as a separate OS-level file: `PersistentAuditLog`
/// only ever holds an opaque `Arc<dyn Storage>` — no filesystem path is
/// available to it, and `Storage` is documented (D7) as the workspace's
/// single storage surface, so extending its trait signature for one
/// caller's sidecar file was rejected as disproportionate to a MEDIUM
/// finding. Storing it as another key in the same domain keeps the
/// change contained to this module and gets 0600-equivalent protection
/// and encryption at rest for free from the domain it already lives in.
const CHAIN_ANCHOR_KEY: &[u8] = b"z\0chain-anchor";

/// The last known tail of the chain: the highest `seq` durably
/// persisted, and its `mac`. Updated after every successful append
/// (see `spawn_drain_task`), checked against the real on-disk tail on
/// every `open`/`verify_from_disk` (see `check_tail_anchor`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct ChainAnchor {
    last_seq: u64,
    last_mac: [u8; 32],
}

/// Persist `seq`/`mac` as the new chain anchor. Called only after the
/// corresponding entry's own `handle.put` has already returned `Ok` —
/// see invariant 6 in the module docs for why that ordering is load-
/// bearing.
async fn write_anchor(
    handle: &DomainHandle,
    seq: u64,
    mac: [u8; 32],
) -> Result<(), aivyx_storage::StorageError> {
    let anchor = ChainAnchor { last_seq: seq, last_mac: mac };
    let bytes = serde_json::to_vec(&anchor).expect("ChainAnchor always serializes");
    handle.put(CHAIN_ANCHOR_KEY, &bytes).await
}

/// Read the persisted chain anchor, if one exists. `None` means either
/// a brand-new store, or a store created before this fix landed — both
/// are "no historical anchor to compare against," not tamper evidence.
/// Any decode failure is likewise treated as "no anchor" rather than
/// an error: the anchor is a best-effort accelerant for tail-
/// truncation detection, not itself part of the chain's integrity
/// proof, so a corrupt anchor record must never be the reason a
/// legitimately-intact chain refuses to open.
async fn read_anchor(handle: &DomainHandle) -> Option<ChainAnchor> {
    let bytes = handle.get(CHAIN_ANCHOR_KEY).await.ok().flatten()?;
    serde_json::from_slice(&bytes).ok()
}

/// Compare the persisted anchor (if any) against the real, freshly
/// re-verified tail of the chain. An anchor that is *behind* the real
/// tail (`last.seq > anchor.last_seq`) is expected and benign — see
/// invariant 6. Only an anchor that is *ahead* of the real tail (fewer
/// entries on disk than the anchor claims) or that disagrees with the
/// real tail's MAC at the same seq is tamper/truncation evidence.
async fn check_tail_anchor(
    handle: &DomainHandle,
    entries: &[SignedEntry],
) -> Result<(), AuditError> {
    let Some(anchor) = read_anchor(handle).await else {
        return Ok(());
    };
    match entries.last() {
        None => Err(AuditError::TailTruncated {
            anchor_seq: anchor.last_seq,
            disk_seq: None,
        }),
        Some(last) if last.seq < anchor.last_seq => Err(AuditError::TailTruncated {
            anchor_seq: anchor.last_seq,
            disk_seq: Some(last.seq),
        }),
        Some(last) if last.seq == anchor.last_seq && last.mac != anchor.last_mac => {
            Err(AuditError::TailTruncated {
                anchor_seq: anchor.last_seq,
                disk_seq: Some(last.seq),
            })
        }
        Some(_) => Ok(()),
    }
}

impl PersistentAuditLog {
    /// Open a persistent audit log over `storage` using `audit_key` as
    /// the HMAC chain key. The caller is expected to have extracted
    /// `audit_key` from `KeyDomain::Audit`'s `SubKey` via
    /// `SubKey::as_bytes()` at the binary wire-up site — this keeps
    /// `aivyx-audit` free of any direct dependency on `aivyx-crypto`.
    ///
    /// On open, the full contents of `KeyDomain::Audit` are scanned,
    /// decoded as `SignedEntry` records, the chain is verified
    /// externally against `audit_key`, and the in-memory
    /// `HmacChainLog` is pre-populated via
    /// [`HmacChainLog::from_verified_entries`]. Any tamper, truncation,
    /// or decode failure surfaces as an `AuditError` — the agent must
    /// not continue running against a broken chain.
    ///
    /// The default error handler panics (matching `AuditBridge::new`).
    /// Use [`Self::with_error_handler`] to override.
    pub async fn open(
        storage: Arc<dyn Storage>,
        audit_key: [u8; 32],
    ) -> Result<Self, AuditError> {
        Self::open_with_error_handler(
            storage,
            audit_key,
            Box::new(|e| panic!("persistent audit: drain failed: {e}")),
        )
        .await
    }

    /// Same as [`Self::open`] but routes drain-task errors through
    /// the supplied handler instead of panicking. The handler runs on
    /// the thread that observes the failure — typically the drain
    /// task, occasionally a subsequent `on_event` caller if the health
    /// flag is inspected first. Callers that want tests or metrics
    /// instead of a process kill use this entry point.
    pub async fn with_error_handler(
        storage: Arc<dyn Storage>,
        audit_key: [u8; 32],
        on_error: impl Fn(AuditError) + Send + Sync + 'static,
    ) -> Result<Self, AuditError> {
        Self::open_with_error_handler(storage, audit_key, Box::new(on_error)).await
    }

    async fn open_with_error_handler(
        storage: Arc<dyn Storage>,
        audit_key: [u8; 32],
        on_error: Box<dyn Fn(AuditError) + Send + Sync>,
    ) -> Result<Self, AuditError> {
        // Shared with `verify_from_disk`: one scan+decode+verify
        // pipeline, so reopen and standalone-verify cannot drift.
        let entries = scan_decode_verify(&storage, &audit_key).await?;

        let chain = Arc::new(HmacChainLog::from_verified_entries(
            audit_key.to_vec(),
            entries,
        ));

        // Double-check: the constructor bypasses MAC recomputation,
        // so run the chain's own verify() over the loaded state as a
        // belt-and-braces check before handing the log to the agent.
        chain.verify()?;

        let (sender, receiver) = mpsc::channel::<SignedEntry>(CHANNEL_CAPACITY);
        let health = Arc::new(AtomicBool::new(true));
        let first_error: Arc<StdMutex<Option<String>>> =
            Arc::new(StdMutex::new(None));
        let on_error: Arc<dyn Fn(AuditError) + Send + Sync> = Arc::from(on_error);

        let drain_handle = spawn_drain_task(
            Arc::clone(&storage),
            receiver,
            Arc::clone(&health),
            Arc::clone(&first_error),
            Arc::clone(&on_error),
        );

        Ok(PersistentAuditLog {
            chain,
            sender,
            health,
            first_error,
            on_error,
            drain_handle: Some(drain_handle),
        })
    }

    /// `true` while the drain task has successfully persisted every
    /// entry it has seen so far. Flips to `false` on the first drain
    /// failure and stays there — there is no automatic recovery path.
    pub fn is_healthy(&self) -> bool {
        self.health.load(Ordering::SeqCst)
    }

    /// Current in-memory chain length. Matches the number of entries
    /// that have been chained; may briefly exceed the number persisted
    /// to disk while the drain task is catching up.
    pub fn len(&self) -> usize {
        self.chain.len()
    }

    /// `true` when no entries have been appended yet. Paired with
    /// [`Self::len`] to satisfy clippy's `len_without_is_empty`.
    pub fn is_empty(&self) -> bool {
        self.chain.len() == 0
    }

    /// Verify the in-memory chain end-to-end. Delegates to
    /// [`HmacChainLog::verify`]; on the reopen path this is redundant
    /// with `open`'s verify, but it is the single entry point callers
    /// reach for when they want a fresh integrity check.
    pub fn verify(&self) -> Result<(), AuditError> {
        self.chain.verify()
    }

    /// Snapshot of all entries, for tests and admin tools.
    pub fn entries(&self) -> Result<Vec<SignedEntry>, AuditError> {
        self.chain.entries()
    }

    /// Ranged snapshot — delegates to [`HmacChainLog::entries_range`].
    /// Phase 47 — used by the daemon's `ListAuditEntries` query to
    /// paginate the audit chain into the Web UI viewer.
    pub fn entries_range(
        &self,
        from_seq: u64,
        limit: usize,
    ) -> Result<Vec<SignedEntry>, AuditError> {
        self.chain.entries_range(from_seq, limit)
    }

    /// Cold-start chain verification over `KeyDomain::Audit`.
    ///
    /// Runs the same scan + decode + HMAC-replay pipeline that
    /// [`Self::open`] runs internally, but **without** building a
    /// live `PersistentAuditLog` — no in-memory chain held open, no
    /// drain task spawned. Intended for:
    ///
    /// - **Startup banners.** The binary in Task 3 calls this before
    ///   constructing the live log so it can surface "audit:
    ///   persistent (N events verified from disk)" to the operator.
    /// - **Integration tests.** Task 7's `audit_persistence_e2e.rs`
    ///   drives a two-session flow and asserts the verified chain
    ///   matches session A's emitted events.
    /// - **A future `--verify-only` CLI mode** (Q6, deferred to
    ///   Task 3) where the binary verifies then exits without
    ///   opening a session at all.
    ///
    /// ## Q5 resolution — fail-closed at the boundary
    ///
    /// Tamper, truncation, or decode failure returns
    /// `Err(AuditError::ChainBroken { .. })` /
    /// `Err(AuditError::CorruptStoredEntry { .. })` unchanged. The
    /// caller decides whether to exit non-zero, restore from
    /// backup, or (in a future dev tool) display the break and
    /// continue. No policy is baked in here — which keeps
    /// `aivyx-audit` out of any `AuditEvent::ChainBreakDetected`
    /// schema-extension territory that would amend D4. See
    /// `PHASE_7.md` Q5 for the decision trail.
    pub async fn verify_from_disk(
        storage: Arc<dyn Storage>,
        audit_key: [u8; 32],
    ) -> Result<VerifyReport, AuditError> {
        let entries = scan_decode_verify(&storage, &audit_key).await?;
        Ok(VerifyReport::from_entries(&entries))
    }
}

/// Result of a cold-start `verify_from_disk` run.
///
/// Intentionally small — fields are added only when a concrete
/// caller needs them. Today's callers are:
///
/// - Task 3 startup banner → `entries_verified`
/// - Task 7 integration test → `head_seq`, `head_mac`
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyReport {
    /// Number of entries that passed the full chain replay. Equals
    /// the number of rows under `KeyDomain::Audit` — verify is
    /// all-or-nothing (we return `Err` at the first break), so this
    /// also equals "rows scanned."
    pub entries_verified: usize,
    /// Sequence number of the last verified entry, or `None` on an
    /// empty store. Equivalent to `entries_verified - 1` when
    /// `entries_verified > 0`, exposed separately so callers can
    /// pattern-match on `Option` without subtracting from a
    /// `usize`.
    pub head_seq: Option<u64>,
    /// HMAC tag of the last verified entry, or the genesis seed on
    /// an empty store. This is the value that a subsequent
    /// [`PersistentAuditLog::open`] will chain its first new
    /// append onto — useful for integration tests that want to
    /// prove session B picked up exactly where session A left off.
    pub head_mac: [u8; 32],
}

impl VerifyReport {
    fn from_entries(entries: &[SignedEntry]) -> Self {
        let genesis = {
            const GENESIS_SEED: &[u8] = b"aivyx-audit-v1-genesis";
            let mut seed = [0u8; 32];
            let start = seed.len() - GENESIS_SEED.len();
            seed[start..].copy_from_slice(GENESIS_SEED);
            seed
        };
        match entries.last() {
            Some(last) => VerifyReport {
                entries_verified: entries.len(),
                head_seq: Some(last.seq),
                head_mac: last.mac,
            },
            None => VerifyReport {
                entries_verified: 0,
                head_seq: None,
                head_mac: genesis,
            },
        }
    }
}

impl AuditWriter for PersistentAuditLog {
    fn append(&self, event: AuditEvent) -> Result<u64, AuditError> {
        // Invariant 1: chain computation happens synchronously here.
        let seq = self.chain.append(event)?;

        // Invariant 3/4: if the drain task has already marked us
        // unhealthy, route through the handler and stop propagating
        // to disk. The event is still in the in-memory chain — that's
        // intentional, since the chain is the authority and future
        // verify() calls must still see a consistent tail.
        if !self.health.load(Ordering::SeqCst) {
            let msg = self
                .first_error
                .lock()
                .ok()
                .and_then(|g| g.clone())
                .unwrap_or_else(|| "drain task previously failed".to_string());
            (self.on_error)(AuditError::Storage(msg));
            return Ok(seq);
        }

        // Invariant 2: push the freshly-chained entry to the drain
        // task. `get(seq)` returns a clone — small, bounded, and
        // simpler than re-deriving the SignedEntry shape at the call
        // site. The `Option` is `Some` because we just appended it.
        let Some(entry) = self.chain.get(seq) else {
            // Can't happen short of a simultaneous external truncate,
            // but we surface instead of panicking.
            (self.on_error)(AuditError::Storage(format!(
                "just-appended entry seq={seq} missing from chain"
            )));
            return Ok(seq);
        };

        // `try_send` is deliberate: if the buffer is full, backpressure
        // becomes visible as a storage error instead of blocking the
        // turn loop. A full channel at 1024 capacity means the drain
        // is wedged — which is exactly the condition the health flag
        // was designed to surface, via this same handler.
        if let Err(e) = self.sender.try_send(entry) {
            (self.on_error)(AuditError::Storage(format!(
                "audit drain channel send failed: {e}"
            )));
            // Still flip the health flag so subsequent appends
            // short-circuit instead of retrying a wedged channel.
            self.health.store(false, Ordering::SeqCst);
            if let Ok(mut slot) = self.first_error.lock() {
                if slot.is_none() {
                    *slot = Some(format!("channel send failed: {e}"));
                }
            }
        }

        Ok(seq)
    }
}

impl AuditLog for PersistentAuditLog {
    fn get(&self, seq: u64) -> Option<SignedEntry> {
        self.chain.get(seq)
    }

    fn len(&self) -> usize {
        self.chain.len()
    }

    fn verify(&self) -> Result<(), AuditError> {
        self.chain.verify()
    }
}

impl aivyx_core::AuditHook for PersistentAuditLog {
    fn on_event(&self, tag: aivyx_core::AuditTag) {
        let event: AuditEvent = tag.into();
        if let Err(e) = self.append(event) {
            (self.on_error)(e);
        }
    }
}

impl Drop for PersistentAuditLog {
    fn drop(&mut self) {
        // Invariant 5: bound the drain task's lifetime.
        //
        // Dropping the sender *alone* isn't enough: the drain task's
        // select-receiver would cleanly exit, but only after it has
        // finished the in-flight put — and if that put is blocked on
        // a frozen filesystem, we'd stall here. `abort()` is the
        // correct escape hatch for a drop path that must not block.
        if let Some(handle) = self.drain_handle.take() {
            handle.abort();
        }
    }
}

// ---------------------------------------------------------------------------
// Reopen path: scan + decode + verify SignedEntry rows from disk.
//
// Used by *both* `PersistentAuditLog::open` (which then constructs an
// in-memory chain from the result) and `PersistentAuditLog::verify_from_disk`
// (which discards the result after building a `VerifyReport`). Sharing
// one pipeline guarantees the two entry points cannot drift in their
// definition of "valid on-disk audit state."
// ---------------------------------------------------------------------------

async fn scan_decode_verify(
    storage: &Arc<dyn Storage>,
    audit_key: &[u8; 32],
) -> Result<Vec<SignedEntry>, AuditError> {
    let handle = storage.domain(KeyDomain::Audit);
    let rows: Vec<ScanRow> = handle
        .scan_prefix(AUDIT_KEY_PREFIX)
        .await
        .map_err(|e| AuditError::Storage(e.to_string()))?;
    let entries = decode_and_validate_rows(audit_key, rows)?;
    check_tail_anchor(&handle, &entries).await?;
    Ok(entries)
}

fn decode_and_validate_rows(
    audit_key: &[u8; 32],
    rows: Vec<ScanRow>,
) -> Result<Vec<SignedEntry>, AuditError> {
    let mut entries: Vec<SignedEntry> = Vec::with_capacity(rows.len());
    for (expected_seq, (key, value)) in rows.into_iter().enumerate() {
        let expected_seq = expected_seq as u64;

        // The key layout commits to `b"a\0" || seq_be`. A row whose
        // key disagrees with its scan-ordering position means either
        // an out-of-band write or a gap — both fatal.
        let seq_from_key = seq_from_key_bytes(&key).ok_or_else(|| {
            AuditError::CorruptStoredEntry {
                seq: expected_seq,
                reason: format!(
                    "key layout wrong: expected {AUDIT_KEY_PREFIX:?} || seq_be, got {key:?}"
                ),
            }
        })?;
        if seq_from_key != expected_seq {
            return Err(AuditError::CorruptStoredEntry {
                seq: expected_seq,
                reason: format!(
                    "seq gap: key encodes seq {seq_from_key}, scan position {expected_seq}"
                ),
            });
        }

        let entry: SignedEntry = serde_json::from_slice(&value).map_err(|e| {
            AuditError::CorruptStoredEntry {
                seq: expected_seq,
                reason: format!("serde_json decode: {e}"),
            }
        })?;
        if entry.seq != expected_seq {
            return Err(AuditError::CorruptStoredEntry {
                seq: expected_seq,
                reason: format!(
                    "entry.seq field = {}, key/position = {}",
                    entry.seq, expected_seq
                ),
            });
        }
        entries.push(entry);
    }

    verify_entries_external(audit_key, &entries)?;
    Ok(entries)
}

fn seq_from_key_bytes(key: &[u8]) -> Option<u64> {
    if key.len() != AUDIT_KEY_PREFIX.len() + 8 {
        return None;
    }
    if !key.starts_with(AUDIT_KEY_PREFIX) {
        return None;
    }
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&key[AUDIT_KEY_PREFIX.len()..]);
    Some(u64::from_be_bytes(buf))
}

/// Verify the chain over a vec of pre-parsed entries, **without** the
/// `HmacChainLog` being populated yet. Mirrors
/// `HmacChainLog::verify`'s recompute strategy but uses a short-lived
/// `HmacChainLog::new(key)` purely to borrow its `compute_mac` via
/// `append` semantics is overkill — we just rebuild the MAC inline.
fn verify_entries_external(
    audit_key: &[u8; 32],
    entries: &[SignedEntry],
) -> Result<(), AuditError> {
    use hmac::{Hmac, KeyInit, Mac};
    use sha2::Sha256;
    type HmacSha256 = Hmac<Sha256>;

    // Duplicated from lib.rs: same GENESIS_SEED as HmacChainLog. Kept
    // as a local const rather than pub-exporting the constant, so the
    // audit module stays the single owner of the seed value.
    const GENESIS_SEED: &[u8] = b"aivyx-audit-v1-genesis";
    let mut expected_prev = {
        let mut seed = [0u8; 32];
        let start = seed.len() - GENESIS_SEED.len();
        seed[start..].copy_from_slice(GENESIS_SEED);
        seed
    };

    for (idx, entry) in entries.iter().enumerate() {
        if entry.seq != idx as u64 {
            return Err(AuditError::ChainBroken {
                seq: idx as u64,
                reason: format!("seq field = {}, expected {}", entry.seq, idx),
            });
        }
        if entry.prev_mac != expected_prev {
            return Err(AuditError::ChainBroken {
                seq: entry.seq,
                reason: "prev_mac does not match previous entry's mac".into(),
            });
        }

        let event_bytes = serde_jcs::to_vec(&entry.event)
            .map_err(|e| AuditError::Serialize(e.to_string()))?;

        let mut mac = <HmacSha256 as KeyInit>::new_from_slice(audit_key)
            .expect("HMAC accepts any key length");
        mac.update(&expected_prev);
        mac.update(&event_bytes);
        let out = mac.finalize().into_bytes();
        let mut expected_mac = [0u8; 32];
        expected_mac.copy_from_slice(&out);

        if expected_mac != entry.mac {
            return Err(AuditError::ChainBroken {
                seq: entry.seq,
                reason: "MAC does not match recomputation over canonical event bytes"
                    .into(),
            });
        }
        expected_prev = entry.mac;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Drain task.
// ---------------------------------------------------------------------------

fn spawn_drain_task(
    storage: Arc<dyn Storage>,
    mut receiver: mpsc::Receiver<SignedEntry>,
    health: Arc<AtomicBool>,
    first_error: Arc<StdMutex<Option<String>>>,
    on_error: Arc<dyn Fn(AuditError) + Send + Sync>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let handle = storage.domain(KeyDomain::Audit);
        while let Some(entry) = receiver.recv().await {
            // Once unhealthy, drop the rest on the floor — the on_event
            // path is already short-circuiting new events, so any
            // stragglers here were already in flight before the flip.
            if !health.load(Ordering::SeqCst) {
                continue;
            }

            let key = audit_key(entry.seq);
            let value = match serde_json::to_vec(&entry) {
                Ok(bytes) => bytes,
                Err(e) => {
                    let msg = format!("serde_json encode seq={}: {e}", entry.seq);
                    mark_unhealthy(&health, &first_error, &msg);
                    (on_error)(AuditError::CorruptStoredEntry {
                        seq: entry.seq,
                        reason: msg,
                    });
                    continue;
                }
            };

            if let Err(e) = handle.put(&key, &value).await {
                let msg = format!("storage.put seq={}: {e}", entry.seq);
                mark_unhealthy(&health, &first_error, &msg);
                (on_error)(AuditError::Storage(msg));
                continue;
            }

            // Invariant 6: only now — after the entry itself is
            // durably persisted — record it as the new chain anchor.
            // A failure here is deliberately non-fatal to health: the
            // entry is already safely on disk, and a stale/behind
            // anchor can only ever under-detect a future truncation
            // (see `check_tail_anchor`), never produce a false
            // positive. Still surfaced through the error handler so
            // it isn't silently swallowed.
            if let Err(e) = write_anchor(&handle, entry.seq, entry.mac).await {
                (on_error)(AuditError::Storage(format!(
                    "chain anchor put seq={}: {e}",
                    entry.seq
                )));
            }
        }
    })
}

fn mark_unhealthy(
    health: &AtomicBool,
    first_error: &StdMutex<Option<String>>,
    msg: &str,
) {
    health.store(false, Ordering::SeqCst);
    if let Ok(mut slot) = first_error.lock() {
        if slot.is_none() {
            *slot = Some(msg.to_string());
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AuditEvent, MemoryOperation as AuditMemoryOp, TrustTierSummary};
    use aivyx_capability::{CapabilitySet, Scope, TrustTier};
    use aivyx_core::{
        ChannelPlatform, SessionId, TokenUsage, ToolId, ToolOutcomeSummary, TurnId,
        TurnOutcomeSummary, VerificationSummary,
    };
    use aivyx_crypto::MasterKey;
    use aivyx_storage::{RedbStorage, StorageConfig};
    use std::path::PathBuf;
    use std::time::Duration;

    fn sample_scope() -> Scope {
        Scope::parse("memory.read:topic:notes").unwrap()
    }

    fn sample_capset() -> CapabilitySet {
        CapabilitySet::from_scopes([Scope::parse("memory.read").unwrap()])
    }

    fn sample_turn_started() -> AuditEvent {
        AuditEvent::TurnStarted {
            turn_id: TurnId::new(),
            session_id: SessionId::new(),
            channel: ChannelPlatform::Local,
            trust_tier: TrustTierSummary::from(TrustTier::Trusted),
            effective_capabilities: sample_capset(),
        }
    }

    fn sample_tool_call() -> AuditEvent {
        AuditEvent::ToolCall {
            turn_id: TurnId::new(),
            tool_id: ToolId::new(),
            scope_used: sample_scope(),
            input_hash: [0u8; 32],
            outcome: ToolOutcomeSummary::Completed {
                verified: VerificationSummary::NotApplicable,
            },
            duration: Duration::from_millis(1),
            auto_corrected_from: None,
            extracted_from_text: None,
        }
    }

    fn sample_memory_access() -> AuditEvent {
        AuditEvent::MemoryAccess {
            turn_id: TurnId::new(),
            operation: AuditMemoryOp::Read,
            scope: sample_scope(),
            query_or_key: "notes/yesterday".into(),
        }
    }

    fn sample_turn_ended() -> AuditEvent {
        AuditEvent::TurnEnded {
            turn_id: TurnId::new(),
            outcome: TurnOutcomeSummary::Completed,
            tool_calls_made: 1,
            duration: Duration::from_millis(2),
            usage: TokenUsage::default(),
        }
    }

    /// Mirror of `aivyx-storage`'s `StoreDir` test helper. We duplicate
    /// instead of importing because `StoreDir` is private to the
    /// storage crate's test module — and adding a `tempfile` dep for
    /// ~50 lines of test hygiene was deliberately rejected there too.
    struct StoreDir {
        dir: PathBuf,
    }

    impl StoreDir {
        fn new() -> Self {
            let tmp = std::env::var("TMPDIR")
                .or_else(|_| std::env::var("TEMP"))
                .unwrap_or_else(|_| "/tmp".to_string());
            let dir = PathBuf::from(tmp)
                .join(format!("aivyx-audit-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).expect("test store dir must be creatable");
            StoreDir { dir }
        }

        fn path(&self) -> PathBuf {
            self.dir.join("store.redb")
        }
    }

    impl Drop for StoreDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    /// Derive the `KeyDomain::Audit` subkey's raw bytes — exactly the
    /// flow the binary uses at wire-up time. `MasterKey::from_raw` is
    /// the test path for a known master; in production the master
    /// comes from `derive_master_key` over a passphrase.
    fn fresh_chain_key(seed: u8) -> [u8; 32] {
        let master = MasterKey::from_raw([seed; 32]);
        let subkey = master.derive_subkey(b"audit").unwrap();
        let mut out = [0u8; 32];
        out.copy_from_slice(subkey.as_bytes());
        out
    }

    async fn fresh_storage(seed: u8) -> (StoreDir, Arc<dyn Storage>, [u8; 32]) {
        let dir = StoreDir::new();
        let chain_key = fresh_chain_key(seed);
        let master = MasterKey::from_raw([seed; 32]);
        let storage = RedbStorage::open(StorageConfig::new(dir.path()), master)
            .await
            .unwrap();
        (dir, storage, chain_key)
    }

    async fn wait_for_disk(storage: &Arc<dyn Storage>, expected: usize) {
        // Drain is async; we don't want to sleep, so we poll the
        // KeyDomain::Audit scan every millisecond up to 2 seconds.
        let handle = storage.domain(KeyDomain::Audit);
        for _ in 0..2000 {
            let rows = handle.scan_prefix(AUDIT_KEY_PREFIX).await.unwrap();
            if rows.len() == expected {
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        panic!("drain never persisted {expected} rows");
    }

    /// Like `wait_for_disk`, but for the chain anchor specifically.
    /// Needed because the anchor write happens in the drain task
    /// *after* the corresponding entry's own `put` (invariant 6) — so
    /// `wait_for_disk` returning (which only checks entry rows) can
    /// race ahead of the anchor write for the same entry. Tests that
    /// need the anchor to have caught up to a specific seq (not just
    /// "behind or equal," which is always safe per `check_tail_anchor`)
    /// must poll for it explicitly rather than relying on
    /// `wait_for_disk` alone.
    async fn wait_for_anchor(storage: &Arc<dyn Storage>, expected_seq: u64) {
        let handle = storage.domain(KeyDomain::Audit);
        for _ in 0..2000 {
            if let Some(anchor) = read_anchor(&handle).await {
                if anchor.last_seq == expected_seq {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        panic!("chain anchor never reached seq {expected_seq}");
    }

    #[tokio::test]
    async fn open_on_empty_domain_yields_empty_chain() {
        let (_dir, storage, chain_key) = fresh_storage(1).await;
        let log = PersistentAuditLog::open(storage, chain_key).await.unwrap();
        assert_eq!(log.len(), 0);
        assert!(log.is_healthy());
        log.verify().unwrap();
    }

    #[tokio::test]
    async fn append_round_trips_through_disk() {
        let (_dir, storage, chain_key) = fresh_storage(2).await;
        let log = PersistentAuditLog::open(Arc::clone(&storage), chain_key)
            .await
            .unwrap();

        log.append(sample_turn_started()).unwrap();
        log.append(sample_tool_call()).unwrap();
        log.append(sample_memory_access()).unwrap();
        log.append(sample_turn_ended()).unwrap();

        assert_eq!(log.len(), 4);
        wait_for_disk(&storage, 4).await;
        log.verify().unwrap();
        assert!(log.is_healthy());
    }

    #[tokio::test]
    async fn reopen_recovers_chain_and_continues_seq() {
        let (_dir, storage, chain_key) = fresh_storage(3).await;

        {
            let log = PersistentAuditLog::open(Arc::clone(&storage), chain_key)
                .await
                .unwrap();
            log.append(sample_turn_started()).unwrap();
            log.append(sample_tool_call()).unwrap();
            wait_for_disk(&storage, 2).await;
        }

        let log2 = PersistentAuditLog::open(Arc::clone(&storage), chain_key)
            .await
            .unwrap();
        assert_eq!(log2.len(), 2);
        log2.verify().unwrap();

        let seq = log2.append(sample_turn_ended()).unwrap();
        assert_eq!(seq, 2);
        wait_for_disk(&storage, 3).await;
        drop(log2);

        let log3 = PersistentAuditLog::open(storage, chain_key).await.unwrap();
        assert_eq!(log3.len(), 3);
        log3.verify().unwrap();
    }

    #[tokio::test]
    async fn reopen_with_wrong_key_fails_chain_verify() {
        let (_dir, storage, chain_key) = fresh_storage(4).await;
        {
            let log = PersistentAuditLog::open(Arc::clone(&storage), chain_key)
                .await
                .unwrap();
            log.append(sample_turn_started()).unwrap();
            wait_for_disk(&storage, 1).await;
        }

        let wrong = [0u8; 32];
        let err = PersistentAuditLog::open(storage, wrong).await.unwrap_err();
        assert!(matches!(err, AuditError::ChainBroken { .. }));
    }

    #[tokio::test]
    async fn tampered_entry_is_rejected_on_reopen() {
        let (_dir, storage, chain_key) = fresh_storage(5).await;
        {
            let log = PersistentAuditLog::open(Arc::clone(&storage), chain_key)
                .await
                .unwrap();
            log.append(sample_turn_started()).unwrap();
            log.append(sample_tool_call()).unwrap();
            wait_for_disk(&storage, 2).await;
        }

        // Overwrite seq=0 with a re-serialized but modified version.
        let handle = storage.domain(KeyDomain::Audit);
        let rows = handle.scan_prefix(AUDIT_KEY_PREFIX).await.unwrap();
        let (key0, val0) = rows[0].clone();
        let mut entry0: SignedEntry = serde_json::from_slice(&val0).unwrap();
        // Flip one byte of the mac: guaranteed chain break.
        entry0.mac[0] ^= 0xff;
        let tampered = serde_json::to_vec(&entry0).unwrap();
        handle.put(&key0, &tampered).await.unwrap();

        let err = PersistentAuditLog::open(storage, chain_key)
            .await
            .unwrap_err();
        assert!(matches!(err, AuditError::ChainBroken { .. }));
    }

    // Renamed from `truncated_tail_is_detected_as_seq_gap` (Task 11,
    // 2026-09-16 security audit): this test always deleted the
    // *middle* entry (seq=1 of 0..2), never the tail — the name
    // claimed tail-truncation coverage this test never provided. The
    // body is unchanged; it was always a correct test of the
    // mid-sequence gap case, just mis-named. Genuine tail-truncation
    // coverage is `genuine_tail_truncation_is_detected` below.
    #[tokio::test]
    async fn middle_entry_deletion_is_detected_as_seq_gap() {
        let (_dir, storage, chain_key) = fresh_storage(6).await;
        {
            let log = PersistentAuditLog::open(Arc::clone(&storage), chain_key)
                .await
                .unwrap();
            log.append(sample_turn_started()).unwrap();
            log.append(sample_tool_call()).unwrap();
            log.append(sample_turn_ended()).unwrap();
            wait_for_disk(&storage, 3).await;
        }

        // Delete the middle entry (seq=1) — this creates a gap the
        // reopen path must catch.
        let handle = storage.domain(KeyDomain::Audit);
        let middle_key = audit_key(1);
        handle.delete(&middle_key).await.unwrap();

        let err = PersistentAuditLog::open(storage, chain_key)
            .await
            .unwrap_err();
        assert!(matches!(
            err,
            AuditError::CorruptStoredEntry { .. } | AuditError::ChainBroken { .. }
        ));
    }

    /// The real tail-truncation case the old (mis-named) test above
    /// never covered: delete the LAST row and nothing else. The
    /// remaining seq=0,1 rows are internally perfectly self-consistent
    /// (no gap, chain replays cleanly from genesis) — before Task 11
    /// this was silently accepted as a valid, shorter chain. The
    /// persisted chain anchor (updated to seq=2 by the drain task
    /// after the third append durably lands) is what makes the
    /// missing seq=2 row observable on reopen.
    #[tokio::test]
    async fn genuine_tail_truncation_is_detected() {
        let (_dir, storage, chain_key) = fresh_storage(24).await;
        {
            let log = PersistentAuditLog::open(Arc::clone(&storage), chain_key)
                .await
                .unwrap();
            log.append(sample_turn_started()).unwrap();
            log.append(sample_tool_call()).unwrap();
            log.append(sample_turn_ended()).unwrap();
            wait_for_disk(&storage, 3).await;
            // Not redundant with the line above: `wait_for_disk` only
            // observes the three entry rows; the anchor write for the
            // third entry happens in a subsequent await inside the
            // same drain-task iteration (invariant 6) and can lag
            // slightly behind. Without this, the test would flake
            // depending on exactly when the drain task is aborted by
            // the `drop(log)`-equivalent end of this block.
            wait_for_anchor(&storage, 2).await;
        }

        // Delete the LAST row (seq=2) — genuine tail truncation, not
        // the middle-entry-deletion case above.
        let handle = storage.domain(KeyDomain::Audit);
        handle.delete(&audit_key(2)).await.unwrap();

        let err = PersistentAuditLog::open(storage, chain_key)
            .await
            .unwrap_err();
        assert!(
            matches!(err, AuditError::TailTruncated { .. }),
            "expected TailTruncated, got {err:?}"
        );
    }

    /// A store created before this fix landed has entries but no
    /// chain-anchor key. `read_anchor` returning `None` must be
    /// treated as "no historical anchor to check against," never as
    /// truncation — otherwise every pre-existing store would fail to
    /// reopen the moment the binary upgrades to this fix.
    #[tokio::test]
    async fn missing_anchor_is_not_treated_as_truncation() {
        let (_dir, storage, chain_key) = fresh_storage(25).await;
        {
            let log = PersistentAuditLog::open(Arc::clone(&storage), chain_key)
                .await
                .unwrap();
            log.append(sample_turn_started()).unwrap();
            wait_for_disk(&storage, 1).await;
        }

        // Simulate "written before this fix existed": remove the
        // anchor key entirely, leaving only the entry itself.
        let handle = storage.domain(KeyDomain::Audit);
        handle.delete(CHAIN_ANCHOR_KEY).await.unwrap();

        let log = PersistentAuditLog::open(storage, chain_key)
            .await
            .expect("missing anchor must not block reopen of an intact chain");
        assert_eq!(log.len(), 1);
    }

    #[tokio::test]
    async fn corrupt_value_surfaces_as_corrupt_entry() {
        let (_dir, storage, chain_key) = fresh_storage(7).await;
        {
            let log = PersistentAuditLog::open(Arc::clone(&storage), chain_key)
                .await
                .unwrap();
            log.append(sample_turn_started()).unwrap();
            wait_for_disk(&storage, 1).await;
        }

        let handle = storage.domain(KeyDomain::Audit);
        handle.put(&audit_key(0), b"not-valid-json").await.unwrap();

        let err = PersistentAuditLog::open(storage, chain_key)
            .await
            .unwrap_err();
        assert!(matches!(err, AuditError::CorruptStoredEntry { .. }));
    }

    #[tokio::test]
    async fn seq_monotonic_across_three_reopens() {
        let (_dir, storage, chain_key) = fresh_storage(8).await;

        for _ in 0..3 {
            let log = PersistentAuditLog::open(Arc::clone(&storage), chain_key)
                .await
                .unwrap();
            log.append(sample_tool_call()).unwrap();
            wait_for_disk(&storage, log.len()).await;
            drop(log);
        }

        let log = PersistentAuditLog::open(storage, chain_key).await.unwrap();
        assert_eq!(log.len(), 3);
        let entries = log.entries().unwrap();
        assert_eq!(entries[0].seq, 0);
        assert_eq!(entries[1].seq, 1);
        assert_eq!(entries[2].seq, 2);
        log.verify().unwrap();
    }

    #[tokio::test]
    async fn key_layout_is_big_endian_seq() {
        assert_eq!(audit_key(0), b"a\0\0\0\0\0\0\0\0\0".to_vec());
        assert_eq!(
            audit_key(0x0102_0304_0506_0708),
            [b'a', 0, 1, 2, 3, 4, 5, 6, 7, 8].to_vec()
        );
        let parsed = seq_from_key_bytes(&audit_key(42)).unwrap();
        assert_eq!(parsed, 42);
    }

    // ---- Task 2: verify_from_disk standalone entry point --------------
    //
    // These four tests exercise the cold-start verification path that
    // does not construct a live `PersistentAuditLog`. They share the
    // `fresh_storage` + `wait_for_disk` fixtures above and use seeds
    // 20–23 to stay out of the 1–9 range Task 1's tests already own.

    #[tokio::test]
    async fn verify_from_disk_on_empty_store_reports_zero() {
        let (_dir, storage, chain_key) = fresh_storage(20).await;
        let report = PersistentAuditLog::verify_from_disk(storage, chain_key)
            .await
            .unwrap();
        assert_eq!(report.entries_verified, 0);
        assert_eq!(report.head_seq, None);
        // Genesis seed is left-padded into [u8; 32].
        let mut expected = [0u8; 32];
        let seed = b"aivyx-audit-v1-genesis";
        expected[32 - seed.len()..].copy_from_slice(seed);
        assert_eq!(report.head_mac, expected);
    }

    #[tokio::test]
    async fn verify_from_disk_reports_entries_verified_count() {
        let (_dir, storage, chain_key) = fresh_storage(21).await;
        {
            let log = PersistentAuditLog::open(Arc::clone(&storage), chain_key)
                .await
                .unwrap();
            log.append(sample_turn_started()).unwrap();
            log.append(sample_tool_call()).unwrap();
            log.append(sample_memory_access()).unwrap();
            log.append(sample_tool_call()).unwrap();
            log.append(sample_turn_ended()).unwrap();
            wait_for_disk(&storage, 5).await;

            // Capture the last entry's mac so we can assert
            // `verify_from_disk` reports the same head.
            let entries = log.entries().unwrap();
            assert_eq!(entries.len(), 5);
            let head_mac = entries[4].mac;

            drop(log);

            let report =
                PersistentAuditLog::verify_from_disk(Arc::clone(&storage), chain_key)
                    .await
                    .unwrap();
            assert_eq!(report.entries_verified, 5);
            assert_eq!(report.head_seq, Some(4));
            assert_eq!(report.head_mac, head_mac);
        }
    }

    #[tokio::test]
    async fn verify_from_disk_returns_chain_broken_with_known_at_seq() {
        let (_dir, storage, chain_key) = fresh_storage(22).await;
        {
            let log = PersistentAuditLog::open(Arc::clone(&storage), chain_key)
                .await
                .unwrap();
            log.append(sample_turn_started()).unwrap();
            log.append(sample_tool_call()).unwrap();
            log.append(sample_turn_ended()).unwrap();
            wait_for_disk(&storage, 3).await;
        }

        // Tamper with seq=1 — a non-boundary row — so we can assert
        // the reported `seq` is not trivially the genesis edge.
        let handle = storage.domain(KeyDomain::Audit);
        let key1 = audit_key(1);
        let val1 = handle.get(&key1).await.unwrap().unwrap();
        let mut entry1: SignedEntry = serde_json::from_slice(&val1).unwrap();
        entry1.mac[7] ^= 0xff;
        let tampered = serde_json::to_vec(&entry1).unwrap();
        handle.put(&key1, &tampered).await.unwrap();

        let err = PersistentAuditLog::verify_from_disk(storage, chain_key)
            .await
            .unwrap_err();
        match err {
            AuditError::ChainBroken { seq, .. } => assert_eq!(seq, 1),
            other => panic!("expected ChainBroken {{ seq: 1 }}, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn verify_from_disk_does_not_hold_storage_lock() {
        // Regression: `scan_decode_verify` must not hold any lock on
        // the storage across its await points, or back-to-back verifies
        // followed by a live `open` would deadlock under redb.
        let (_dir, storage, chain_key) = fresh_storage(23).await;
        {
            let log = PersistentAuditLog::open(Arc::clone(&storage), chain_key)
                .await
                .unwrap();
            log.append(sample_turn_started()).unwrap();
            log.append(sample_tool_call()).unwrap();
            wait_for_disk(&storage, 2).await;
        }

        let r1 = PersistentAuditLog::verify_from_disk(Arc::clone(&storage), chain_key)
            .await
            .unwrap();
        let r2 = PersistentAuditLog::verify_from_disk(Arc::clone(&storage), chain_key)
            .await
            .unwrap();
        assert_eq!(r1, r2);
        assert_eq!(r1.entries_verified, 2);

        // The live open must still succeed after two verify passes.
        let log = PersistentAuditLog::open(storage, chain_key).await.unwrap();
        assert_eq!(log.len(), 2);
        log.verify().unwrap();
    }

    #[tokio::test]
    async fn error_handler_wires_up_cleanly() {
        // Inducing a real drain failure against RedbStorage is
        // awkward, so this test confirms the closure shape and the
        // happy-path invariant: on a healthy storage, the handler
        // closure is never invoked for a single-append workload.
        let (_dir, storage, chain_key) = fresh_storage(9).await;
        let captured: Arc<StdMutex<Vec<String>>> = Arc::new(StdMutex::new(Vec::new()));
        let captured_clone = Arc::clone(&captured);
        let log = PersistentAuditLog::with_error_handler(
            Arc::clone(&storage),
            chain_key,
            move |e| {
                captured_clone.lock().unwrap().push(e.to_string());
            },
        )
        .await
        .unwrap();
        assert!(log.is_healthy());
        log.append(sample_turn_started()).unwrap();
        wait_for_disk(&storage, 1).await;
        assert!(captured.lock().unwrap().is_empty());
    }
}
