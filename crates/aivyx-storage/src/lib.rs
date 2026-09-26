//! # aivyx-storage
//!
//! Encrypted redb-based storage for Aivyx, with HKDF-derived subkeys
//! per data domain (Sessions, Memory, Audit, Secrets, ChannelState, Missions).
//!
//! See DESIGN.md Deliverable 7 for the storage stack commitment:
//!
//! - **redb** for the embedded ACID KV substrate (single-file, pure
//!   Rust, cross-platform, single-writer enforced by file lock).
//! - **Argon2id + HKDF + ChaCha20-Poly1305** from `aivyx-crypto` for
//!   the passphrase → master → subkey → AEAD chain. This crate never
//!   sees a raw passphrase — the channel adapter (Phase 5 task 3)
//!   derives the master key and hands it to [`RedbStorage::open`].
//! - **One handle per process.** The returned `Arc<dyn Storage>` is
//!   shared across every concurrent turn. `redb::Database` enforces
//!   the single-writer invariant at the file-lock layer.
//!
//! ## API shape
//!
//! - [`Storage`] — the trait agents hold behind `Arc<dyn Storage>`.
//!   Exposes [`Storage::domain`] (cheap — precomputed subkeys) and
//!   [`Storage::flush`] (no-op for redb, present for trait symmetry
//!   with future backends).
//! - [`RedbStorage`] — the one and only concrete impl. `open` is an
//!   inherent associated function rather than a trait method because
//!   a trait method with `Self: Sized` can't be called through
//!   `Arc<dyn Storage>`, which is the only handle type callers see.
//! - [`DomainHandle`] — owned handle to a single [`KeyDomain`]. Holds
//!   the per-domain subkey (cheap to clone) and a `redb::Database`
//!   `Arc`. Lookups and writes go through this type so callers can't
//!   accidentally cross domains (putting a `KeyDomain::Sessions` key
//!   into the `KeyDomain::Memory` table is structurally impossible).
//!
//! ## Key isolation
//!
//! The test suite's `open_wrong_key_fails_to_decrypt` test makes the
//! AEAD isolation explicit: opening a store with the right path but
//! the wrong master key produces a handle whose `get` calls fail with
//! [`StorageError::DecryptFailed`]. The AEAD open error is not
//! distinguished from a tampered-ciphertext error — that's the
//! correct behavior for a ChaCha20-Poly1305 AEAD, and it's what
//! prevents an adversary from learning which of (key, nonce, aad,
//! ciphertext) is "wrong" in a compromise scenario.
//!
//! ## Nonce discipline
//!
//! Every `put` generates a fresh 12-byte nonce via `uuid::Uuid::
//! new_v4()` truncated to 12 bytes and stores `nonce || ciphertext`
//! as the value. The nonce is read back at `get` time. This means
//! the same logical key can be `put` many times across its lifetime
//! without nonce reuse — the AEAD uniqueness requirement is
//! satisfied by write-time randomness, not by key identity.
//!
//! ## Why "async trait" for a sync-backed KV store
//!
//! redb is synchronous. D7 commits to wrapping all redb calls in
//! `tokio::task::spawn_blocking` so the agent's async loop never
//! stalls on a disk read. That's what this crate does internally —
//! every public async method dispatches via `spawn_blocking`. A
//! future non-redb backend (e.g., SQLite-over-tokio-rusqlite) would
//! implement the same trait natively.

#![forbid(unsafe_code)]

use std::ops::Bound;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use redb::{Database, TableDefinition};

use aivyx_crypto::{CryptoError, MasterKey, SubKey, NONCE_LEN};

// --------------------------------------------------------------------
// KeyDomain
// --------------------------------------------------------------------

/// Domain separator for HKDF subkey derivation. D7 line 907 locks
/// this taxonomy at five variants. Adding a sixth is a DESIGN.md
/// amendment.
///
/// Each variant maps to:
/// - a stable `info` byte string for HKDF (via [`KeyDomain::as_bytes`]),
/// - a dedicated redb [`TableDefinition`] (via [`KeyDomain::table_name`]),
/// - its own AEAD `SubKey` cached in the storage handle at open time.
#[derive(Debug, Clone, Copy, Eq, PartialEq, Hash)]
pub enum KeyDomain {
    /// Session metadata and turn history.
    Sessions,
    /// `memory.read` / `memory.write` substrate (Phase 6).
    Memory,
    /// HMAC-chained audit log entries (future phase).
    Audit,
    /// Encrypted config values (API keys, tokens).
    Secrets,
    /// Per-channel persistent state (Matrix sync tokens, etc.).
    ChannelState,
    /// Mission records — long-running work items with approval gates (Phase 21).
    Missions,
    /// Team-mission records — Nonagon (`aivyx-team`) mission runs the daemon
    /// drives, with their checkpoint/resume state for durability across
    /// restarts (Chapter L). One row per mission keyed by mission id. Distinct
    /// from [`KeyDomain::Missions`] (the older single-agent lifecycle).
    TeamMissions,
    /// Schedule records — cron-triggered execution entries (Phase 26).
    Schedules,
    /// Webhook trigger records — HTTP-triggered execution entries (Phase 27).
    Webhooks,
    /// File-watch trigger records — filesystem-change-triggered entries (Phase 27).
    FileWatches,
    /// Persona delta log — HMAC-chained operator-approved identity
    /// deltas per PRODUCT.md P14 (Phase 59). Parallel to
    /// [`KeyDomain::Audit`] per Q2(a) at Phase 59 sign-off. One row
    /// per delta keyed by big-endian u64 sequence number; the chain
    /// is replayed at daemon startup into an in-memory
    /// `EffectivePersona`.
    Persona,
    /// Persona proposal log — HMAC-chained pending Persona deltas
    /// proposed by the reflection auto-loop per PRODUCT.md P14
    /// (Phase 70). Parallel to [`KeyDomain::Persona`] per Q4(a)
    /// at Phase 70 sign-off: proposals are operator-pending
    /// objects, deltas are operator-approved objects, so the
    /// persona chain's invariant (every delta is operator-
    /// approved) stays intact. One row per proposal keyed by
    /// big-endian u64 sequence number; status transitions
    /// (Pending → Approved | Rejected | Superseded) append new
    /// rows rather than mutating in place, so the proposal
    /// history is preserved for audit.
    PersonaProposals,
    /// Memory embedding vectors (Phase 75). One row per
    /// embedded memory entry, keyed by the same `topic\x00seq`
    /// shape the `Memory` domain uses so a vector can be
    /// dropped in lockstep with its entry on forget/evict.
    /// Stored separately from [`KeyDomain::Memory`] so a plain
    /// memory read doesn't drag a 384–1536-float payload, and
    /// so an embedding-model change can invalidate the vector
    /// table without touching the entry table. Loaded into an
    /// in-memory flat cosine index at daemon startup.
    MemoryVectors,
    /// Recall-feedback events (Phase 77). One row per turn that
    /// auto-recall injected memory into, keyed by a big-endian
    /// timestamp + sequence so the reflection loop can read a
    /// time window cheaply. Each row records which `(topic,
    /// seq)` memories were injected and their cosine scores;
    /// the structural correlator pairs these against the audit
    /// chain's `TurnEnded` outcomes to learn which memories
    /// actually help. Kept separate from every other domain so
    /// the learning signal can be GC-clamped independently and
    /// a corrupt row degrades learning, not recall or memory.
    RecallEvents,
    /// Proactive-surfacing dedup log (Phase 80). One row per
    /// item the assistant has surfaced unprompted, keyed by the
    /// item's deterministic id, so the proactive pass never
    /// re-surfaces the same thing across reflection cycles.
    /// Tiny + GC-clamped; isolated so a corrupt row degrades
    /// only proactive dedup, never memory or the recall signal.
    ProactiveLog,
    /// Persistent helpfulness ledger (Phase 82). One row per
    /// memory topic holding the durable, time-decayed EWMA of
    /// "did recalling this topic help" folded from each Phase 77
    /// reflection cycle. Self-pruning; isolated so a corrupt row
    /// degrades only the longitudinal learning view, never
    /// memory, recall, or proactive dedup.
    HelpfulnessLedger,
    /// Persistent co-occurrence ledger (Phase 83). One row per
    /// canonical topic pair holding the durable, time-decayed
    /// EWMA of "these two topics were recalled together in a
    /// turn that helped," folded from each Phase 77 reflection
    /// cycle. Self-pruning; isolated so a corrupt row degrades
    /// only the cross-session pattern view, never memory,
    /// recall, the helpfulness ledger, or proactive dedup.
    CooccurrenceLedger,
    /// Phase 116 — persistent tool/skill relevance ledger.
    /// One row per keyword-set selector, holding per-tool and
    /// per-skill success/failure counts accumulated over time.
    /// The Phase 116 system-prompt assembly reads this ledger
    /// and renders a `## Tools recently used for similar
    /// tasks` section so the agent's selection is informed by
    /// historical outcomes. Isolated so a corrupt row degrades
    /// only the selection-hint signal, never memory, recall,
    /// or any other learning ledger.
    ToolRelevanceLedger,
    /// Persistent correction ledger (Phase 172). One row per
    /// memory topic holding the durable, time-decayed EWMA of
    /// "how often did the operator rework a turn that recalled
    /// this topic," folded from each Phase 77 reflection cycle
    /// (the `completed`-then-rapid-followup correction proxy).
    /// Self-pruning; isolated so a corrupt row degrades only
    /// the self-improvement signal, never memory, recall, the
    /// helpfulness/co-occurrence ledgers, or proactive dedup.
    CorrectionLedger,
    /// Autonomous-loop backlog (Phase 173). One row per signed
    /// chain entry, keyed by big-endian u64 sequence number, so
    /// scan reads return chain-ordered. Holds the operator's
    /// ordered story list (the Ralph `prd.json` analog) with
    /// HMAC-chained `Created` / `Done` / `Skipped` status
    /// transitions. Isolated so a corrupt row degrades only the
    /// loop backlog, never persona, missions, or any learning
    /// ledger.
    LoopBacklog,
    /// One-shot reminders (Phase 183). One row per pending
    /// reminder — `due_unix`, `message`, optional notify targets
    /// — set by `remind.set`, fired + cleared by the reminder
    /// driver. Isolated so a corrupt row degrades only reminders,
    /// never schedules, missions, or any learning ledger.
    Reminders,
    /// Knowledge-wiki pages (Chapter Codex). One row per canonical
    /// topic holding a `WikiPage` — the LLM-consolidated summary of
    /// that topic's memory entries plus its co-occurrence backlinks
    /// and a `source_fingerprint` for incremental regeneration.
    /// Derived from [`KeyDomain::Memory`] + the co-occurrence ledger,
    /// never a second source of truth. Isolated so a corrupt or stale
    /// page degrades only the codex (the Studio Wiki view + the opt-in
    /// recall unit), never memory, recall, or any learning ledger.
    KnowledgeWiki,
    /// Typed knowledge-graph triples (Chapter Lattice). One row per
    /// directed `(subject, predicate, object)` relation extracted from
    /// memory, keyed by the NUL-joined canonical triple. Derived from
    /// [`KeyDomain::Memory`], never a second source of truth. Isolated so
    /// a corrupt or stale triple degrades only the graph (the Studio
    /// graph view + the `graph.query` tool + the opt-in recall source),
    /// never memory, recall, the wiki, or any learning ledger.
    KnowledgeGraph,
    /// Chapter Whetstone — the per-skill effectiveness ledger: a
    /// time-decayed EWMA, keyed by `LearnedSkill` name, of "did invoking
    /// this skill lead to a turn that went well." Folded on the turn
    /// boundary from the `SkillInvocation` audit signal + the turn
    /// outcome. Isolated so a corrupt row degrades only the skill-
    /// refinement signal — never skills, the persona, recall, or any
    /// other ledger.
    SkillHelpfulnessLedger,
    /// Chapter Helm (Opp F) — small persisted loop run-state. Holds the
    /// "a run is active" marker so an opt-in `[loop] resume_on_boot` can
    /// resume an interrupted run after a daemon restart (the in-memory
    /// `SharedLoopState` is lost on restart). A handful of fixed keys, not a
    /// chain — distinct domain so it's HKDF-isolated like every other.
    LoopState,
    /// Chapter Concord — dismissed memory-contradiction ids. One row per
    /// operator-dismissed conflict (keyed by the deterministic
    /// `MemoryConflict` id), so an on-demand detection pass can suppress a
    /// pair the operator marked "keep both / not a contradiction" instead
    /// of re-flagging it every run. Tiny + opaque (the id is an FNV hash,
    /// not a secret); isolated so a corrupt row degrades only conflict
    /// dismissal, never memory or any other signal.
    ConflictDismissals,
    /// Model routing Part 3b — the per-conversation routing taint. One row
    /// per tainted conversation, keyed by the session id, holding the first
    /// recorded reason (a short label, never content). Write-once and never
    /// cleared: a tainted conversation must never escalate to a cloud
    /// endpoint, across restarts and compaction (G6). Isolated so the taint
    /// set is HKDF-separated from the session rows it shadows.
    RoutingTaint,
}

impl KeyDomain {
    /// Stable byte string for HKDF's `info` parameter. These values
    /// are the schema-versioning contract: changing any of them is a
    /// cold-start operation (subkeys change, existing ciphertexts
    /// are unreadable). They're lowercase-ASCII so a future
    /// `HKDF_SALT` rotation is the *only* way to invalidate the
    /// whole keyspace.
    pub const fn as_bytes(self) -> &'static [u8] {
        match self {
            KeyDomain::Sessions => b"sessions",
            KeyDomain::Memory => b"memory",
            KeyDomain::Audit => b"audit",
            KeyDomain::Secrets => b"secrets",
            KeyDomain::ChannelState => b"channel-state",
            KeyDomain::Missions => b"missions",
            KeyDomain::Schedules => b"schedules",
            KeyDomain::Webhooks => b"webhooks",
            KeyDomain::FileWatches => b"file-watches",
            KeyDomain::Persona => b"persona",
            KeyDomain::PersonaProposals => b"persona-proposals",
            KeyDomain::MemoryVectors => b"memory-vectors",
            KeyDomain::RecallEvents => b"recall-events",
            KeyDomain::ProactiveLog => b"proactive-log",
            KeyDomain::HelpfulnessLedger => b"helpfulness-ledger",
            KeyDomain::CooccurrenceLedger => b"cooccurrence-ledger",
            KeyDomain::ToolRelevanceLedger => b"tool-relevance-ledger",
            KeyDomain::CorrectionLedger => b"correction-ledger",
            KeyDomain::LoopBacklog => b"loop-backlog",
            KeyDomain::Reminders => b"reminders",
            KeyDomain::TeamMissions => b"team-missions",
            KeyDomain::KnowledgeWiki => b"knowledge-wiki",
            KeyDomain::KnowledgeGraph => b"knowledge-graph",
            KeyDomain::SkillHelpfulnessLedger => b"skill-helpfulness-ledger",
            KeyDomain::LoopState => b"loop-state",
            KeyDomain::ConflictDismissals => b"conflict-dismissals",
            KeyDomain::RoutingTaint => b"routing-taint",
        }
    }

    /// redb table name for this domain. The `_v1` suffix pairs with
    /// the `aivyx-v1-storage` HKDF salt in `aivyx-crypto`: a future
    /// `v2` salt rotation means a `v2` table name, and the old table
    /// is left in place (dead but intact) to simplify incident
    /// recovery.
    pub const fn table_name(self) -> &'static str {
        match self {
            KeyDomain::Sessions => "aivyx_sessions_v1",
            KeyDomain::Memory => "aivyx_memory_v1",
            KeyDomain::Audit => "aivyx_audit_v1",
            KeyDomain::Secrets => "aivyx_secrets_v1",
            KeyDomain::ChannelState => "aivyx_channel_state_v1",
            KeyDomain::Missions => "aivyx_missions_v1",
            KeyDomain::Schedules => "aivyx_schedules_v1",
            KeyDomain::Webhooks => "aivyx_webhooks_v1",
            KeyDomain::FileWatches => "aivyx_file_watches_v1",
            KeyDomain::Persona => "aivyx_persona_v1",
            KeyDomain::PersonaProposals => "aivyx_persona_proposals_v1",
            KeyDomain::MemoryVectors => "aivyx_memory_vectors_v1",
            KeyDomain::RecallEvents => "aivyx_recall_events_v1",
            KeyDomain::ProactiveLog => "aivyx_proactive_log_v1",
            KeyDomain::HelpfulnessLedger => {
                "aivyx_helpfulness_ledger_v1"
            }
            KeyDomain::CooccurrenceLedger => {
                "aivyx_cooccurrence_ledger_v1"
            }
            KeyDomain::ToolRelevanceLedger => {
                "aivyx_tool_relevance_ledger_v1"
            }
            KeyDomain::CorrectionLedger => {
                "aivyx_correction_ledger_v1"
            }
            KeyDomain::LoopBacklog => "aivyx_loop_backlog_v1",
            KeyDomain::Reminders => "aivyx_reminders_v1",
            KeyDomain::TeamMissions => "aivyx_team_missions_v1",
            KeyDomain::KnowledgeWiki => "aivyx_knowledge_wiki_v1",
            KeyDomain::KnowledgeGraph => "aivyx_knowledge_graph_v1",
            KeyDomain::SkillHelpfulnessLedger => {
                "aivyx_skill_helpfulness_ledger_v1"
            }
            KeyDomain::LoopState => "aivyx_loop_state_v1",
            KeyDomain::ConflictDismissals => {
                "aivyx_conflict_dismissals_v1"
            }
            KeyDomain::RoutingTaint => "aivyx_routing_taint_v1",
        }
    }

    /// All variants, iteration order stable. Used at `open` time to
    /// precompute every subkey and to create the redb tables.
    pub const ALL: [KeyDomain; 27] = [
        KeyDomain::Sessions,
        KeyDomain::Memory,
        KeyDomain::Audit,
        KeyDomain::Secrets,
        KeyDomain::ChannelState,
        KeyDomain::Missions,
        KeyDomain::Schedules,
        KeyDomain::Webhooks,
        KeyDomain::FileWatches,
        KeyDomain::Persona,
        KeyDomain::PersonaProposals,
        KeyDomain::MemoryVectors,
        KeyDomain::RecallEvents,
        KeyDomain::ProactiveLog,
        KeyDomain::HelpfulnessLedger,
        KeyDomain::CooccurrenceLedger,
        KeyDomain::ToolRelevanceLedger,
        KeyDomain::CorrectionLedger,
        KeyDomain::LoopBacklog,
        KeyDomain::Reminders,
        KeyDomain::TeamMissions,
        KeyDomain::KnowledgeWiki,
        KeyDomain::KnowledgeGraph,
        KeyDomain::SkillHelpfulnessLedger,
        KeyDomain::LoopState,
        KeyDomain::ConflictDismissals,
        KeyDomain::RoutingTaint,
    ];
}

// --------------------------------------------------------------------
// Type aliases
// --------------------------------------------------------------------

/// One row returned by [`DomainHandle::scan_prefix`] — an owned
/// `(key, plaintext)` pair. Named so the `scan_prefix` signature
/// stays readable and downstream crates can refer to it without
/// repeating the inner tuple shape.
pub type ScanRow = (Vec<u8>, Vec<u8>);

// --------------------------------------------------------------------
// next_lex — exclusive upper bound for a lexicographic prefix scan
// --------------------------------------------------------------------

/// Compute the smallest byte string strictly greater than every byte
/// string that begins with `prefix`. Used as the exclusive upper
/// bound of a lexicographic prefix-scan range.
///
/// Algorithm: find the **last byte** of `prefix` that is not `0xFF`,
/// increment it, and truncate everything after. If no such byte
/// exists (empty prefix, or all `0xFF`s), return `None` — the caller
/// should translate that into `Bound::Unbounded`, meaning "scan to
/// end of domain."
///
/// Examples:
///
/// - `next_lex(b"notes\0")` → `Some(b"notes\x01")`
/// - `next_lex(b"a")` → `Some(b"b")`
/// - `next_lex(b"")` → `None` (empty prefix matches everything)
/// - `next_lex(&[0xFF, 0xFF])` → `None` (no valid successor)
/// - `next_lex(&[0x01, 0xFF])` → `Some(&[0x02])` (carry-and-truncate)
fn next_lex(prefix: &[u8]) -> Option<Vec<u8>> {
    for i in (0..prefix.len()).rev() {
        if prefix[i] != 0xFF {
            let mut out = prefix[..=i].to_vec();
            out[i] += 1;
            return Some(out);
        }
    }
    None
}

// --------------------------------------------------------------------
// Errors
// --------------------------------------------------------------------

/// Errors this crate produces. All collapse to `AivyxError::Storage`
/// at the `aivyx-core` boundary — the top-level `AivyxError` enum
/// already reserves a `Storage(String)` slot for exactly this
/// purpose (D6 12-variant cap).
#[derive(Debug, Clone, thiserror::Error)]
pub enum StorageError {
    /// The redb backend refused to open, create, or commit. Wraps
    /// the upstream string because redb's error enum is larger than
    /// any caller cares to match against.
    #[error("redb error: {0}")]
    Redb(String),

    /// Failed to derive a domain subkey via HKDF. Surfaced unchanged
    /// from `aivyx-crypto::CryptoError`.
    #[error("crypto error: {0}")]
    Crypto(#[from] CryptoError),

    /// Decryption failed for a value read from disk. Either the
    /// master key is wrong (cold open against a store written with
    /// a different passphrase), the file has been tampered with, or
    /// the table was written by a future storage schema this binary
    /// doesn't understand. The three cases are indistinguishable by
    /// construction — that's the AEAD guarantee.
    #[error("decrypt failed for {domain:?} key (wrong master key or tampered data)")]
    DecryptFailed { domain: KeyDomain },

    /// A value on disk is shorter than the nonce length. This means
    /// the table was corrupted or written by a non-Aivyx writer.
    #[error("corrupt value for {domain:?}: length {len} < nonce length {NONCE_LEN}")]
    CorruptValue { domain: KeyDomain, len: usize },

    /// `tokio::task::spawn_blocking` panicked or was cancelled. Should
    /// not happen in practice — redb calls do not panic for any
    /// reason the storage layer would handle differently from any
    /// other runtime failure.
    #[error("blocking task join failed: {0}")]
    JoinFailed(String),
}

// Small adapters to lift upstream error kinds into `StorageError::Redb`.
// Each `impl From<…> for StorageError` is one line, so the storage code
// below can use `?` freely without a tower of `.map_err(…)` calls.

impl From<redb::Error> for StorageError {
    fn from(e: redb::Error) -> Self {
        Self::Redb(e.to_string())
    }
}
impl From<redb::DatabaseError> for StorageError {
    fn from(e: redb::DatabaseError) -> Self {
        Self::Redb(e.to_string())
    }
}
impl From<redb::TransactionError> for StorageError {
    fn from(e: redb::TransactionError) -> Self {
        Self::Redb(e.to_string())
    }
}
impl From<redb::TableError> for StorageError {
    fn from(e: redb::TableError) -> Self {
        Self::Redb(e.to_string())
    }
}
impl From<redb::StorageError> for StorageError {
    fn from(e: redb::StorageError) -> Self {
        Self::Redb(e.to_string())
    }
}
impl From<redb::CommitError> for StorageError {
    fn from(e: redb::CommitError) -> Self {
        Self::Redb(e.to_string())
    }
}

// --------------------------------------------------------------------
// StorageConfig — inputs to RedbStorage::open
// --------------------------------------------------------------------

/// Open-time configuration. Mirrors the Phase 4 `FsReadToolConfig`
/// pattern: a plain struct whose fallible `open` produces an
/// `Arc`-wrapped handle, so the binary owns composition and
/// surfaces startup errors at the boundary where the user can see
/// them.
#[derive(Debug, Clone)]
pub struct StorageConfig {
    /// Absolute path to the redb file. The parent directory must
    /// exist; `open` will not create parents. Pick a path under
    /// `$XDG_DATA_HOME/aivyx-pa/` or similar.
    pub path: PathBuf,
}

impl StorageConfig {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

// --------------------------------------------------------------------
// Storage trait
// --------------------------------------------------------------------

/// Trait agents hold behind `Arc<dyn Storage>`. D7 locks this as the
/// single storage surface in the workspace; adding a second method
/// requires a contract update.
///
/// Note that `open` is not a trait method — see [`RedbStorage::open`].
/// A trait method returning `Self: Sized` would not be callable
/// through `Arc<dyn Storage>`, which is the only handle type the
/// session layer accepts.
#[async_trait]
pub trait Storage: Send + Sync {
    /// Return an owned handle to the named domain. Cheap: the subkey
    /// is precomputed at open time and cloned into the handle (32
    /// bytes); the underlying `redb::Database` is an `Arc`.
    fn domain(&self, domain: KeyDomain) -> DomainHandle;

    /// Flush pending writes. No-op on redb (commits are explicit and
    /// durable at transaction close), present on the trait for
    /// symmetry with future backends that buffer.
    async fn flush(&self) -> Result<(), StorageError>;
}

// --------------------------------------------------------------------
// RedbStorage — the one concrete impl
// --------------------------------------------------------------------

/// Single redb-backed [`Storage`] implementation. Holds one
/// `Arc<Database>` handle, the master key (zeroize-on-drop), and
/// one precomputed [`SubKey`] per [`KeyDomain`] variant.
///
/// The master key is kept around so that a future `rotate_passphrase`
/// operation can re-seal every value without requiring a reopen.
/// No such method exists in Phase 5 — kept deliberately simple.
#[derive(Debug)]
pub struct RedbStorage {
    db: Arc<Database>,
    subkeys: [SubKey; 27],
    // _master held to make the zeroize-on-drop behavior load-bearing:
    // as long as RedbStorage is alive, the master is alive; when the
    // last Arc drops, so does the master.
    _master: MasterKey,
}

impl RedbStorage {
    /// Open (or create) an encrypted store at the given path using
    /// the provided master key. All tables are created on first open
    /// and all nine [`KeyDomain`] subkeys are derived up front, so
    /// the hot path never calls HKDF.
    ///
    /// # Errors
    ///
    /// Returns a `StorageError::Redb` if the file cannot be opened
    /// or created, or a `StorageError::Crypto` if HKDF refuses one
    /// of the subkey derivations (extremely unlikely — see
    /// `aivyx-crypto::CryptoError::HkdfExpandFailed`).
    pub async fn open(
        config: StorageConfig,
        master: MasterKey,
    ) -> Result<Arc<dyn Storage>, StorageError> {
        // Derive every subkey first. This is pure-memory and fast
        // enough that doing it on the async side (no spawn_blocking)
        // is fine — HKDF is a couple of microseconds.
        let subkeys = Self::derive_all_subkeys(&master)?;

        // Open the redb file on a blocking worker so the reactor
        // thread doesn't stall on disk I/O. `redb::Database::create`
        // opens an existing file or creates a new one; a failed open
        // produces a `DatabaseError` which flows through our
        // `From<DatabaseError>` impl.
        //
        // Phase 7 task 6 — on cold-start (file did not exist before
        // `Database::create`), we follow up with `chmod 0600` on the
        // freshly-created file. The `try_exists` check runs *before*
        // `Database::create` so a pre-existing file (possibly
        // deliberately re-permed by the operator) is not silently
        // forced back to 0600 on every reopen. Failure to chmod is
        // a hard error: a 0600 assertion is load-bearing for D5's
        // local-trust model, and silently continuing with an
        // inherited-umask file would hand the operator a false
        // sense of security.
        let path = config.path.clone();
        let db = tokio::task::spawn_blocking(move || -> Result<Database, StorageError> {
            let was_cold = !path
                .try_exists()
                .map_err(|e| StorageError::Redb(format!("failed to probe store path {path:?}: {e}")))?;
            let db = Database::create(&path)?;
            // Create every domain's table during the first write
            // transaction so a cold store has the full schema
            // before any read hits it.
            let write = db.begin_write()?;
            for domain in KeyDomain::ALL {
                let table_def: TableDefinition<&[u8], &[u8]> =
                    TableDefinition::new(domain.table_name());
                let _ = write.open_table(table_def)?;
            }
            write.commit()?;
            if was_cold {
                chmod_user_only(&path).map_err(|e| {
                    StorageError::Redb(format!("failed to chmod 0600 on {path:?}: {e}"))
                })?;
            }
            Ok(db)
        })
        .await
        .map_err(|e| StorageError::JoinFailed(e.to_string()))??;

        Ok(Arc::new(Self {
            db: Arc::new(db),
            subkeys,
            _master: master,
        }))
    }

    fn derive_all_subkeys(master: &MasterKey) -> Result<[SubKey; 27], StorageError> {
        // `KeyDomain::ALL` is indexed in declaration order; we rely
        // on that to slot each derived subkey into a fixed-size
        // array so `domain()` is an O(1) index-by-discriminant.
        Ok([
            master.derive_subkey(KeyDomain::Sessions.as_bytes())?,
            master.derive_subkey(KeyDomain::Memory.as_bytes())?,
            master.derive_subkey(KeyDomain::Audit.as_bytes())?,
            master.derive_subkey(KeyDomain::Secrets.as_bytes())?,
            master.derive_subkey(KeyDomain::ChannelState.as_bytes())?,
            master.derive_subkey(KeyDomain::Missions.as_bytes())?,
            master.derive_subkey(KeyDomain::Schedules.as_bytes())?,
            master.derive_subkey(KeyDomain::Webhooks.as_bytes())?,
            master.derive_subkey(KeyDomain::FileWatches.as_bytes())?,
            master.derive_subkey(KeyDomain::Persona.as_bytes())?,
            master.derive_subkey(KeyDomain::PersonaProposals.as_bytes())?,
            master.derive_subkey(KeyDomain::MemoryVectors.as_bytes())?,
            master.derive_subkey(KeyDomain::RecallEvents.as_bytes())?,
            master.derive_subkey(KeyDomain::ProactiveLog.as_bytes())?,
            master.derive_subkey(
                KeyDomain::HelpfulnessLedger.as_bytes(),
            )?,
            master.derive_subkey(
                KeyDomain::CooccurrenceLedger.as_bytes(),
            )?,
            master.derive_subkey(
                KeyDomain::ToolRelevanceLedger.as_bytes(),
            )?,
            master.derive_subkey(
                KeyDomain::CorrectionLedger.as_bytes(),
            )?,
            master.derive_subkey(KeyDomain::LoopBacklog.as_bytes())?,
            master.derive_subkey(KeyDomain::Reminders.as_bytes())?,
            master.derive_subkey(KeyDomain::TeamMissions.as_bytes())?,
            master.derive_subkey(KeyDomain::KnowledgeWiki.as_bytes())?,
            master.derive_subkey(KeyDomain::KnowledgeGraph.as_bytes())?,
            master.derive_subkey(
                KeyDomain::SkillHelpfulnessLedger.as_bytes(),
            )?,
            master.derive_subkey(KeyDomain::LoopState.as_bytes())?,
            master.derive_subkey(
                KeyDomain::ConflictDismissals.as_bytes(),
            )?,
            master.derive_subkey(KeyDomain::RoutingTaint.as_bytes())?,
        ])
    }

    fn subkey_for(&self, domain: KeyDomain) -> &SubKey {
        // Hard-coded match keyed off the discriminant. If a future
        // amendment adds a variant, the compiler forces this match
        // to update — safer than `as usize` indexing.
        match domain {
            KeyDomain::Sessions => &self.subkeys[0],
            KeyDomain::Memory => &self.subkeys[1],
            KeyDomain::Audit => &self.subkeys[2],
            KeyDomain::Secrets => &self.subkeys[3],
            KeyDomain::ChannelState => &self.subkeys[4],
            KeyDomain::Missions => &self.subkeys[5],
            KeyDomain::Schedules => &self.subkeys[6],
            KeyDomain::Webhooks => &self.subkeys[7],
            KeyDomain::FileWatches => &self.subkeys[8],
            KeyDomain::Persona => &self.subkeys[9],
            KeyDomain::PersonaProposals => &self.subkeys[10],
            KeyDomain::MemoryVectors => &self.subkeys[11],
            KeyDomain::RecallEvents => &self.subkeys[12],
            KeyDomain::ProactiveLog => &self.subkeys[13],
            KeyDomain::HelpfulnessLedger => &self.subkeys[14],
            KeyDomain::CooccurrenceLedger => &self.subkeys[15],
            KeyDomain::ToolRelevanceLedger => &self.subkeys[16],
            KeyDomain::CorrectionLedger => &self.subkeys[17],
            KeyDomain::LoopBacklog => &self.subkeys[18],
            KeyDomain::Reminders => &self.subkeys[19],
            KeyDomain::TeamMissions => &self.subkeys[20],
            KeyDomain::KnowledgeWiki => &self.subkeys[21],
            KeyDomain::KnowledgeGraph => &self.subkeys[22],
            KeyDomain::SkillHelpfulnessLedger => &self.subkeys[23],
            KeyDomain::LoopState => &self.subkeys[24],
            KeyDomain::ConflictDismissals => &self.subkeys[25],
            KeyDomain::RoutingTaint => &self.subkeys[26],
        }
    }
}

/// Phase 7 task 6 — `chmod 0600` the given path on Unix (owner
/// read+write, nothing else). No-op on non-Unix: D5 declares Linux as
/// the supported platform, but keeping the non-unix arm a clean no-op
/// lets macOS dev machines `cargo check` the workspace without a
/// platform cfg elsewhere. Returns any `io::Error` from
/// `fs::set_permissions` so callers can wrap it in their own error
/// type — this helper does not pick a policy, it just flips the bits.
///
/// Intentionally duplicated between this crate and
/// `aivyx-channel::passphrase`: two ~5-line helpers are cheaper than
/// a new `aivyx-core` public surface for a Unix-perms utility, and
/// the logic is stable enough that divergence between copies is a
/// non-problem.
#[cfg(unix)]
fn chmod_user_only(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn chmod_user_only(_path: &std::path::Path) -> std::io::Result<()> {
    Ok(())
}

#[async_trait]
impl Storage for RedbStorage {
    fn domain(&self, domain: KeyDomain) -> DomainHandle {
        DomainHandle {
            db: Arc::clone(&self.db),
            subkey: self.subkey_for(domain).clone(),
            domain,
        }
    }

    async fn flush(&self) -> Result<(), StorageError> {
        // redb commits are explicit (every `put`/`delete` ends with
        // `commit()`), so there is nothing to flush. The trait
        // method exists so a future buffered backend can override.
        Ok(())
    }
}

// --------------------------------------------------------------------
// DomainHandle — per-domain KV API
// --------------------------------------------------------------------

/// Owned handle to a single storage domain. Returned by
/// [`Storage::domain`]. Caller holds as many of these per session as
/// it likes — each is cheap (one `Arc::clone` + one `SubKey::clone`,
/// which is 32 bytes).
#[derive(Debug, Clone)]
pub struct DomainHandle {
    db: Arc<Database>,
    subkey: SubKey,
    domain: KeyDomain,
}

impl DomainHandle {
    /// Which domain this handle speaks to. Useful for error reporting
    /// at the caller so "storage error for Sessions" distinguishes
    /// from "storage error for Memory."
    pub fn domain(&self) -> KeyDomain {
        self.domain
    }

    /// Fetch `key`, returning `Ok(None)` if not present or
    /// `Ok(Some(plaintext))` on a successful AEAD open.
    ///
    /// Decrypt failures surface as
    /// [`StorageError::DecryptFailed`] — which means the wrong
    /// master key was used to open this store, the file was
    /// tampered with, or the table was written by a schema this
    /// binary cannot read. All three are indistinguishable and all
    /// three are fatal to the operation.
    pub async fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError> {
        let db = Arc::clone(&self.db);
        let domain = self.domain;
        let subkey = self.subkey.clone();
        let key = key.to_vec();
        let aad = self.aad(&key);

        tokio::task::spawn_blocking(move || -> Result<Option<Vec<u8>>, StorageError> {
            let read = db.begin_read()?;
            let table_def: TableDefinition<&[u8], &[u8]> =
                TableDefinition::new(domain.table_name());
            let table = match read.open_table(table_def) {
                Ok(t) => t,
                // `TableDoesNotExist` is indistinguishable from
                // "no key present" for our purposes — the table is
                // always created at `open` time but the bound
                // variant gives us a future-proof no-op.
                Err(redb::TableError::TableDoesNotExist(_)) => return Ok(None),
                Err(e) => return Err(e.into()),
            };
            let Some(stored) = table.get(key.as_slice())? else {
                return Ok(None);
            };
            let value_bytes = stored.value().to_vec();
            if value_bytes.len() < NONCE_LEN {
                return Err(StorageError::CorruptValue {
                    domain,
                    len: value_bytes.len(),
                });
            }
            let (nonce, ciphertext) = value_bytes.split_at(NONCE_LEN);
            let plaintext = subkey
                .open(nonce, &aad, ciphertext)
                .map_err(|_| StorageError::DecryptFailed { domain })?;
            Ok(Some(plaintext))
        })
        .await
        .map_err(|e| StorageError::JoinFailed(e.to_string()))?
    }

    /// Write `value` under `key`, sealing with the domain subkey and
    /// a fresh 12-byte nonce. Overwrites any previous value.
    pub async fn put(&self, key: &[u8], value: &[u8]) -> Result<(), StorageError> {
        // Fresh random nonce per write. `Uuid::new_v4` is uniform-
        // random via getrandom; we truncate to 12 bytes. The same
        // logical key can be `put` many times without nonce reuse
        // because the nonce is resampled on every call — the AEAD
        // uniqueness requirement is satisfied by write-time
        // randomness, not by key identity.
        let nonce_bytes: [u8; 16] = *uuid::Uuid::new_v4().as_bytes();
        let mut nonce = [0u8; NONCE_LEN];
        nonce.copy_from_slice(&nonce_bytes[..NONCE_LEN]);

        let aad = self.aad(key);
        let ciphertext = self.subkey.seal(&nonce, &aad, value)?;

        // Prepend the nonce to the ciphertext for on-disk storage.
        // The `get` path splits it back out. This is the standard
        // "nonce || ct" wire format for unauthenticated-nonce
        // AEAD schemes.
        let mut stored = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        stored.extend_from_slice(&nonce);
        stored.extend_from_slice(&ciphertext);

        let db = Arc::clone(&self.db);
        let domain = self.domain;
        let key = key.to_vec();

        tokio::task::spawn_blocking(move || -> Result<(), StorageError> {
            let write = db.begin_write()?;
            {
                let table_def: TableDefinition<&[u8], &[u8]> =
                    TableDefinition::new(domain.table_name());
                let mut table = write.open_table(table_def)?;
                table.insert(key.as_slice(), stored.as_slice())?;
            }
            write.commit()?;
            Ok(())
        })
        .await
        .map_err(|e| StorageError::JoinFailed(e.to_string()))?
    }

    /// Scan every key in this domain whose byte representation begins
    /// with `prefix`, returning `(key, plaintext)` pairs **sorted by
    /// key ascending**. Each value goes through the same AEAD-open
    /// path as [`get`], so a wrong master key or tampered ciphertext
    /// surfaces as [`StorageError::DecryptFailed`] on the first bad
    /// value and aborts the scan.
    ///
    /// ## Semantics
    ///
    /// - An **empty prefix** matches every key in the domain. Useful
    ///   for admin/debug scans; Phase 6's `RedbMemory` uses it exactly
    ///   once at construction time to find the max existing sequence
    ///   number across all topics.
    /// - A **missing table** (possible in theory; the open path
    ///   creates all five at startup) returns an empty `Vec`, not an
    ///   error — matches `get`'s handling of the same condition.
    /// - **Lexicographic ordering** is byte-ordering, not UTF-8
    ///   collation. Callers that want newest-first get it by building
    ///   their keys as `prefix || seq_be` and reversing the returned
    ///   iterator, which is what `RedbMemory::get_recent` does.
    /// - The full result set is materialized into a `Vec` before the
    ///   blocking task returns. That's fine for memory-domain values
    ///   (small entries, bounded by agent write rate) but would need
    ///   revisiting if a future domain stores large blobs and wants
    ///   streaming — at which point we'd add a second method rather
    ///   than retrofit this one.
    ///
    /// ## Prefix-scan mechanics
    ///
    /// redb's range API takes a Rust `RangeBounds`. The lexicographic
    /// "all keys starting with `prefix`" idiom is
    /// `[prefix, next_lex(prefix))` — inclusive on the lower bound,
    /// exclusive on the first key that sorts strictly above every
    /// valid continuation. [`next_lex`] computes that upper bound by
    /// incrementing the last non-`0xFF` byte of the prefix and
    /// truncating everything after; if the prefix is all `0xFF`s
    /// (or empty), there is no upper bound and we use `Bound::Unbounded`.
    pub async fn scan_prefix(
        &self,
        prefix: &[u8],
    ) -> Result<Vec<ScanRow>, StorageError> {
        let db = Arc::clone(&self.db);
        let domain = self.domain;
        let subkey = self.subkey.clone();
        let prefix = prefix.to_vec();
        let domain_bytes = domain.as_bytes();
        // Capture `domain_bytes` now; AAD builds inside the blocking
        // closure walk the same bytes per row.
        let domain_bytes_owned: Vec<u8> = domain_bytes.to_vec();

        tokio::task::spawn_blocking(
            move || -> Result<Vec<ScanRow>, StorageError> {
                let read = db.begin_read()?;
                let table_def: TableDefinition<&[u8], &[u8]> =
                    TableDefinition::new(domain.table_name());
                let table = match read.open_table(table_def) {
                    Ok(t) => t,
                    Err(redb::TableError::TableDoesNotExist(_)) => return Ok(Vec::new()),
                    Err(e) => return Err(e.into()),
                };

                let upper = next_lex(&prefix);
                let lower_bound: Bound<&[u8]> = Bound::Included(prefix.as_slice());
                let upper_bound: Bound<&[u8]> = match upper.as_deref() {
                    Some(u) => Bound::Excluded(u),
                    None => Bound::Unbounded,
                };

                let iter = table.range::<&[u8]>((lower_bound, upper_bound))?;

                let mut out: Vec<ScanRow> = Vec::new();
                for row in iter {
                    let (k, v) = row?;
                    let key_bytes = k.value().to_vec();
                    let stored = v.value().to_vec();
                    if stored.len() < NONCE_LEN {
                        return Err(StorageError::CorruptValue {
                            domain,
                            len: stored.len(),
                        });
                    }
                    let (nonce, ciphertext) = stored.split_at(NONCE_LEN);
                    // Rebuild AAD per-row because AAD is
                    // `b"aivyx-v1" || domain_bytes || 0x00 || user_key`
                    // and the user_key portion is this row's key.
                    let mut aad =
                        Vec::with_capacity(8 + domain_bytes_owned.len() + 1 + key_bytes.len());
                    aad.extend_from_slice(b"aivyx-v1");
                    aad.extend_from_slice(&domain_bytes_owned);
                    aad.push(0x00);
                    aad.extend_from_slice(&key_bytes);
                    let plaintext = subkey
                        .open(nonce, &aad, ciphertext)
                        .map_err(|_| StorageError::DecryptFailed { domain })?;
                    out.push((key_bytes, plaintext));
                }

                Ok(out)
            },
        )
        .await
        .map_err(|e| StorageError::JoinFailed(e.to_string()))?
    }

    /// Delete `key`. Returns `Ok(())` whether or not the key was
    /// present — the KV semantics are set-like, not reference-counted.
    pub async fn delete(&self, key: &[u8]) -> Result<(), StorageError> {
        let db = Arc::clone(&self.db);
        let domain = self.domain;
        let key = key.to_vec();

        tokio::task::spawn_blocking(move || -> Result<(), StorageError> {
            let write = db.begin_write()?;
            {
                let table_def: TableDefinition<&[u8], &[u8]> =
                    TableDefinition::new(domain.table_name());
                let mut table = write.open_table(table_def)?;
                let _ = table.remove(key.as_slice())?;
            }
            write.commit()?;
            Ok(())
        })
        .await
        .map_err(|e| StorageError::JoinFailed(e.to_string()))?
    }

    /// Build the AEAD associated-data for a `(domain, user_key)`
    /// pair. Binding the AAD to both prevents a value from being
    /// copy-pasted across domains or across keys within the same
    /// domain — a renamed key will fail to decrypt, as will a value
    /// moved from `Sessions` into `Memory`.
    ///
    /// The per-row AAD rebuild inside `scan_prefix` is intentionally
    /// not delegated to this method — `scan_prefix` runs inside a
    /// `spawn_blocking` closure that cannot hold a `&self` borrow
    /// across the `.await`, so it clones `domain.as_bytes()` into an
    /// owned `Vec<u8>` before entering the blocking context and
    /// rebuilds AAD manually. If AAD format ever changes, both this
    /// function and the scan_prefix row loop must stay in sync.
    ///
    /// Format: `b"aivyx-v1" || domain_bytes || 0x00 || user_key`
    /// The `0x00` separator prevents ambiguity between domains
    /// whose names could otherwise form a prefix (none today, but
    /// a future addition like `"session"` vs `"sessions"` would
    /// otherwise collide).
    fn aad(&self, user_key: &[u8]) -> Vec<u8> {
        let domain_bytes = self.domain.as_bytes();
        let mut aad = Vec::with_capacity(8 + domain_bytes.len() + 1 + user_key.len());
        aad.extend_from_slice(b"aivyx-v1");
        aad.extend_from_slice(domain_bytes);
        aad.push(0x00);
        aad.extend_from_slice(user_key);
        aad
    }
}

// --------------------------------------------------------------------
// Tests
// --------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// RAII temp directory — creates `$TMPDIR/aivyx-storage-test-<uuid>`
    /// on construction, removes the whole tree on drop. Rolled here
    /// to match `aivyx-core::tools::fs`'s `SandboxDir` convention and
    /// avoid a `tempfile` dep for ~20 lines of hygiene.
    struct StoreDir {
        dir: PathBuf,
    }

    impl StoreDir {
        fn new() -> Self {
            let tmp = std::env::var("TMPDIR")
                .or_else(|_| std::env::var("TEMP"))
                .unwrap_or_else(|_| "/tmp".to_string());
            let dir = PathBuf::from(tmp)
                .join(format!("aivyx-storage-test-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&dir).expect("test store dir must be creatable");
            StoreDir { dir }
        }

        fn path(&self) -> PathBuf {
            self.dir.join("store.redb")
        }
    }

    impl Drop for StoreDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn test_master(seed: u8) -> MasterKey {
        MasterKey::from_raw([seed; 32])
    }

    async fn open_store(dir: &StoreDir, master: MasterKey) -> Arc<dyn Storage> {
        RedbStorage::open(StorageConfig::new(dir.path()), master)
            .await
            .expect("open store")
    }

    // ---- KeyDomain --------------------------------------------------

    #[test]
    fn key_domain_count_matches_docs() {
        // F7 drift-guard (Chapter Throttle TH.4): pin the number of encrypted
        // storage domains so the figure in the code and the docs cannot diverge
        // silently. `as_bytes()` / `table_name()` are exhaustive matches, so a
        // new variant forces a compile error there; this test plus `ALL` catch
        // the count. **If this number changes, update `KeyDomain::ALL`, the
        // "Encrypted storage domains" row + the `aivyx-storage` line in
        // `README.md`, and the storage-domain figure in
        // `docs/BACKEND_AUDIT_*.md`.**
        assert_eq!(KeyDomain::ALL.len(), 27, "encrypted storage domain count");
    }

    #[test]
    fn key_domain_info_strings_are_distinct() {
        // Regression lock for D7 — any change here is a subkey
        // rotation for the affected domain and must be deliberate.
        let all: Vec<_> = KeyDomain::ALL.iter().map(|d| d.as_bytes()).collect();
        let mut uniq = all.clone();
        uniq.sort();
        uniq.dedup();
        assert_eq!(uniq.len(), all.len(), "info strings must be unique");
    }

    #[test]
    fn key_domain_table_names_are_distinct_and_versioned() {
        let all: Vec<_> = KeyDomain::ALL.iter().map(|d| d.table_name()).collect();
        let mut uniq = all.clone();
        uniq.sort();
        uniq.dedup();
        assert_eq!(uniq.len(), all.len(), "table names must be unique");
        for name in &all {
            assert!(
                name.ends_with("_v1"),
                "table {name} must be schema-versioned"
            );
            assert!(
                name.starts_with("aivyx_"),
                "table {name} must be aivyx-namespaced"
            );
        }
    }

    #[test]
    fn key_domain_all_covers_every_variant() {
        // If a future phase adds a seventeenth `KeyDomain`
        // variant, this test fails because `ALL` is a fixed-size
        // array and the match below forces an update. Tripwire
        // for "adding a variant without updating ALL."
        for d in KeyDomain::ALL {
            match d {
                KeyDomain::Sessions
                | KeyDomain::Memory
                | KeyDomain::Audit
                | KeyDomain::Secrets
                | KeyDomain::ChannelState
                | KeyDomain::Missions
                | KeyDomain::Schedules
                | KeyDomain::Webhooks
                | KeyDomain::FileWatches
                | KeyDomain::Persona
                | KeyDomain::PersonaProposals
                | KeyDomain::MemoryVectors
                | KeyDomain::RecallEvents
                | KeyDomain::ProactiveLog
                | KeyDomain::HelpfulnessLedger
                | KeyDomain::CooccurrenceLedger
                | KeyDomain::ToolRelevanceLedger
                | KeyDomain::CorrectionLedger
                | KeyDomain::LoopBacklog
                | KeyDomain::Reminders
                | KeyDomain::TeamMissions
                | KeyDomain::KnowledgeWiki
                | KeyDomain::KnowledgeGraph
                | KeyDomain::SkillHelpfulnessLedger
                | KeyDomain::LoopState
                | KeyDomain::ConflictDismissals
                | KeyDomain::RoutingTaint => {}
            }
        }
    }

    // ---- Phase 70 — PersonaProposals domain ------------------------

    #[test]
    fn persona_proposals_domain_has_stable_metadata() {
        assert_eq!(
            KeyDomain::PersonaProposals.as_bytes(),
            b"persona-proposals"
        );
        assert_eq!(
            KeyDomain::PersonaProposals.table_name(),
            "aivyx_persona_proposals_v1"
        );
        assert!(KeyDomain::ALL.contains(&KeyDomain::PersonaProposals));
    }

    #[tokio::test]
    async fn persona_proposals_domain_isolates_from_persona_domain() {
        // Same key in two different domains must not collide —
        // proposals are operator-pending objects, deltas are
        // operator-approved objects. Phase 70 Q4(a).
        let dir = StoreDir::new();
        let store = open_store(&dir, test_master(70)).await;

        let persona = store.domain(KeyDomain::Persona);
        let proposals = store.domain(KeyDomain::PersonaProposals);

        let key = b"seq-0";
        persona.put(key, b"approved-delta").await.unwrap();
        proposals.put(key, b"pending-proposal").await.unwrap();

        assert_eq!(
            persona.get(key).await.unwrap(),
            Some(b"approved-delta".to_vec()),
            "Persona domain returned the proposal's value"
        );
        assert_eq!(
            proposals.get(key).await.unwrap(),
            Some(b"pending-proposal".to_vec()),
            "PersonaProposals domain returned the persona's value"
        );
    }

    #[test]
    fn memory_vectors_domain_has_stable_metadata() {
        assert_eq!(KeyDomain::MemoryVectors.as_bytes(), b"memory-vectors");
        assert_eq!(
            KeyDomain::MemoryVectors.table_name(),
            "aivyx_memory_vectors_v1"
        );
        assert!(KeyDomain::ALL.contains(&KeyDomain::MemoryVectors));
    }

    #[tokio::test]
    async fn memory_vectors_domain_isolates_from_memory_domain() {
        // Same key in the Memory vs MemoryVectors domains must
        // not collide — entries and their embedding vectors are
        // stored separately so a plain memory read doesn't drag
        // the float payload (Phase 75 Q3(a)).
        let dir = StoreDir::new();
        let store = open_store(&dir, test_master(75)).await;

        let entries = store.domain(KeyDomain::Memory);
        let vectors = store.domain(KeyDomain::MemoryVectors);

        let key = b"notes\x00\x00\x00\x00\x00\x00\x00\x00";
        entries.put(key, b"the body text").await.unwrap();
        vectors.put(key, b"\x01\x02\x03\x04").await.unwrap();

        assert_eq!(
            entries.get(key).await.unwrap(),
            Some(b"the body text".to_vec()),
            "Memory domain returned the vector's value"
        );
        assert_eq!(
            vectors.get(key).await.unwrap(),
            Some(vec![1u8, 2, 3, 4]),
            "MemoryVectors domain returned the entry's value"
        );
    }

    // ---- Phase 77 — RecallEvents domain ----------------------------

    #[test]
    fn recall_events_domain_has_stable_metadata() {
        assert_eq!(KeyDomain::RecallEvents.as_bytes(), b"recall-events");
        assert_eq!(
            KeyDomain::RecallEvents.table_name(),
            "aivyx_recall_events_v1"
        );
        assert!(KeyDomain::ALL.contains(&KeyDomain::RecallEvents));
    }

    #[tokio::test]
    async fn recall_events_domain_isolates_from_memory_domain() {
        // The recall-feedback signal is a distinct learning
        // artifact: it must not collide with memory entries (a
        // corrupt/GC'd signal degrades learning, never recall or
        // the memory itself). Phase 77 Q2(a).
        let dir = StoreDir::new();
        let store = open_store(&dir, test_master(77)).await;

        let entries = store.domain(KeyDomain::Memory);
        let recall = store.domain(KeyDomain::RecallEvents);

        let key = b"shared-key";
        entries.put(key, b"a memory body").await.unwrap();
        recall.put(key, b"a recall event").await.unwrap();

        assert_eq!(
            entries.get(key).await.unwrap(),
            Some(b"a memory body".to_vec()),
            "Memory domain returned the recall event's value"
        );
        assert_eq!(
            recall.get(key).await.unwrap(),
            Some(b"a recall event".to_vec()),
            "RecallEvents domain returned the memory's value"
        );
    }

    // ---- Phase 80 — ProactiveLog domain ----------------------------

    #[test]
    fn proactive_log_domain_has_stable_metadata() {
        assert_eq!(
            KeyDomain::ProactiveLog.as_bytes(),
            b"proactive-log"
        );
        assert_eq!(
            KeyDomain::ProactiveLog.table_name(),
            "aivyx_proactive_log_v1"
        );
        assert!(KeyDomain::ALL.contains(&KeyDomain::ProactiveLog));
    }

    #[tokio::test]
    async fn proactive_log_domain_isolates_from_recall_events() {
        // The proactive dedup log is distinct from the
        // recall-feedback signal: a corrupt/GC'd proactive row
        // must degrade only proactive dedup, never the learning
        // signal. Phase 80.
        let dir = StoreDir::new();
        let store = open_store(&dir, test_master(80)).await;

        let recall = store.domain(KeyDomain::RecallEvents);
        let proactive = store.domain(KeyDomain::ProactiveLog);

        let key = b"shared-key";
        recall.put(key, b"a recall event").await.unwrap();
        proactive.put(key, b"a surfaced item").await.unwrap();

        assert_eq!(
            recall.get(key).await.unwrap(),
            Some(b"a recall event".to_vec()),
            "RecallEvents domain returned the proactive value"
        );
        assert_eq!(
            proactive.get(key).await.unwrap(),
            Some(b"a surfaced item".to_vec()),
            "ProactiveLog domain returned the recall value"
        );
    }

    // ---- Phase 82 — HelpfulnessLedger domain -----------------------

    #[test]
    fn helpfulness_ledger_domain_has_stable_metadata() {
        assert_eq!(
            KeyDomain::HelpfulnessLedger.as_bytes(),
            b"helpfulness-ledger"
        );
        assert_eq!(
            KeyDomain::HelpfulnessLedger.table_name(),
            "aivyx_helpfulness_ledger_v1"
        );
        assert!(
            KeyDomain::ALL.contains(&KeyDomain::HelpfulnessLedger)
        );
    }

    #[tokio::test]
    async fn helpfulness_ledger_domain_isolates_from_recall_events()
    {
        // The durable helpfulness ledger is distinct from the
        // ephemeral recall-feedback signal: a corrupt/pruned
        // ledger row must degrade only the longitudinal
        // learning view, never the per-cycle recall signal.
        // Phase 82.
        let dir = StoreDir::new();
        let store = open_store(&dir, test_master(82)).await;

        let recall = store.domain(KeyDomain::RecallEvents);
        let ledger = store.domain(KeyDomain::HelpfulnessLedger);

        let key = b"shared-key";
        recall.put(key, b"a recall event").await.unwrap();
        ledger.put(key, b"a ledger row").await.unwrap();

        assert_eq!(
            recall.get(key).await.unwrap(),
            Some(b"a recall event".to_vec()),
            "RecallEvents domain returned the ledger value"
        );
        assert_eq!(
            ledger.get(key).await.unwrap(),
            Some(b"a ledger row".to_vec()),
            "HelpfulnessLedger domain returned the recall value"
        );
    }

    // ---- Phase 83 — CooccurrenceLedger domain ----------------------

    #[test]
    fn cooccurrence_ledger_domain_has_stable_metadata() {
        assert_eq!(
            KeyDomain::CooccurrenceLedger.as_bytes(),
            b"cooccurrence-ledger"
        );
        assert_eq!(
            KeyDomain::CooccurrenceLedger.table_name(),
            "aivyx_cooccurrence_ledger_v1"
        );
        assert!(
            KeyDomain::ALL
                .contains(&KeyDomain::CooccurrenceLedger)
        );
    }

    #[tokio::test]
    async fn cooccurrence_ledger_domain_isolates_from_helpfulness()
    {
        // The pair co-occurrence ledger is distinct from the
        // per-topic helpfulness ledger: a corrupt/pruned pair
        // row must degrade only the cross-session pattern
        // view, never the per-topic longitudinal signal.
        // Phase 83.
        let dir = StoreDir::new();
        let store = open_store(&dir, test_master(83)).await;

        let helpful =
            store.domain(KeyDomain::HelpfulnessLedger);
        let cooc =
            store.domain(KeyDomain::CooccurrenceLedger);

        let key = b"shared-key";
        helpful.put(key, b"a topic row").await.unwrap();
        cooc.put(key, b"a pair row").await.unwrap();

        assert_eq!(
            helpful.get(key).await.unwrap(),
            Some(b"a topic row".to_vec()),
            "HelpfulnessLedger returned the cooccurrence value"
        );
        assert_eq!(
            cooc.get(key).await.unwrap(),
            Some(b"a pair row".to_vec()),
            "CooccurrenceLedger returned the helpfulness value"
        );
    }

    // ---- Model routing Part 3b — RoutingTaint domain ---------------

    #[test]
    fn routing_taint_domain_has_stable_metadata() {
        assert_eq!(KeyDomain::RoutingTaint.as_bytes(), b"routing-taint");
        assert_eq!(
            KeyDomain::RoutingTaint.table_name(),
            "aivyx_routing_taint_v1"
        );
        assert!(KeyDomain::ALL.contains(&KeyDomain::RoutingTaint));
    }

    #[tokio::test]
    async fn routing_taint_domain_isolates_from_sessions() {
        // A session's taint row shares its key (the session id) with
        // the session's own row; the two must never alias.
        let dir = StoreDir::new();
        let store = open_store(&dir, test_master(84)).await;

        let sessions = store.domain(KeyDomain::Sessions);
        let taint = store.domain(KeyDomain::RoutingTaint);

        let key = b"session-1";
        sessions.put(key, b"a session row").await.unwrap();
        taint.put(key, b"a taint row").await.unwrap();

        assert_eq!(
            sessions.get(key).await.unwrap(),
            Some(b"a session row".to_vec()),
            "Sessions returned the taint value"
        );
        assert_eq!(
            taint.get(key).await.unwrap(),
            Some(b"a taint row".to_vec()),
            "RoutingTaint returned the session value"
        );
    }

    // ---- Happy path -------------------------------------------------

    #[tokio::test]
    async fn put_then_get_round_trips() {
        let dir = StoreDir::new();
        let store = open_store(&dir, test_master(1)).await;

        let handle = store.domain(KeyDomain::Sessions);
        handle.put(b"session-1", b"turn-17").await.unwrap();

        let got = handle.get(b"session-1").await.unwrap();
        assert_eq!(got, Some(b"turn-17".to_vec()));
    }

    #[tokio::test]
    async fn get_missing_key_returns_none() {
        let dir = StoreDir::new();
        let store = open_store(&dir, test_master(1)).await;
        let got = store.domain(KeyDomain::Memory).get(b"never-written").await.unwrap();
        assert_eq!(got, None);
    }

    #[tokio::test]
    async fn put_overwrites_previous_value() {
        let dir = StoreDir::new();
        let store = open_store(&dir, test_master(2)).await;
        let handle = store.domain(KeyDomain::Secrets);

        handle.put(b"api-key", b"sk-v1").await.unwrap();
        handle.put(b"api-key", b"sk-v2").await.unwrap();

        let got = handle.get(b"api-key").await.unwrap();
        assert_eq!(got, Some(b"sk-v2".to_vec()));
    }

    #[tokio::test]
    async fn delete_removes_key() {
        let dir = StoreDir::new();
        let store = open_store(&dir, test_master(3)).await;
        let handle = store.domain(KeyDomain::Audit);

        handle.put(b"entry-1", b"hmac-chain").await.unwrap();
        handle.delete(b"entry-1").await.unwrap();
        assert_eq!(handle.get(b"entry-1").await.unwrap(), None);
    }

    #[tokio::test]
    async fn delete_is_idempotent() {
        let dir = StoreDir::new();
        let store = open_store(&dir, test_master(3)).await;
        let handle = store.domain(KeyDomain::Audit);

        // Delete a never-written key — should not error.
        handle.delete(b"never-existed").await.unwrap();
        handle.delete(b"never-existed").await.unwrap();
    }

    #[tokio::test]
    async fn flush_is_a_no_op_but_succeeds() {
        let dir = StoreDir::new();
        let store = open_store(&dir, test_master(4)).await;
        store.flush().await.unwrap();
    }

    // ---- Domain isolation --------------------------------------------

    #[tokio::test]
    async fn put_in_one_domain_does_not_leak_to_another() {
        let dir = StoreDir::new();
        let store = open_store(&dir, test_master(5)).await;

        store
            .domain(KeyDomain::Sessions)
            .put(b"key", b"sessions-value")
            .await
            .unwrap();

        // Same key in a different domain must be a miss, even though
        // the underlying redb database is the same file — the tables
        // are named per domain so this is structurally impossible to
        // confuse.
        let memory_miss = store.domain(KeyDomain::Memory).get(b"key").await.unwrap();
        assert_eq!(memory_miss, None);
    }

    #[tokio::test]
    async fn every_domain_round_trips_independently() {
        let dir = StoreDir::new();
        let store = open_store(&dir, test_master(6)).await;

        for domain in KeyDomain::ALL {
            let handle = store.domain(domain);
            let key = format!("k-{:?}", domain).into_bytes();
            let value = format!("v-{:?}", domain).into_bytes();
            handle.put(&key, &value).await.unwrap();
            let got = handle.get(&key).await.unwrap();
            assert_eq!(got, Some(value));
        }
    }

    // ---- Persistence across reopen -----------------------------------

    #[tokio::test]
    async fn values_persist_across_reopen_with_same_master() {
        let dir = StoreDir::new();

        // Session A: write, drop the handle.
        {
            let store = open_store(&dir, test_master(7)).await;
            store
                .domain(KeyDomain::Sessions)
                .put(b"resume-me", b"turn-42")
                .await
                .unwrap();
            // Explicit drop so the redb file lock is released
            // before session B opens the same path.
            drop(store);
        }

        // Session B: reopen same path + same master, read back.
        let store = open_store(&dir, test_master(7)).await;
        let got = store.domain(KeyDomain::Sessions).get(b"resume-me").await.unwrap();
        assert_eq!(got, Some(b"turn-42".to_vec()));
    }

    // ---- Wrong-key negatives -----------------------------------------

    #[tokio::test]
    async fn open_wrong_key_fails_to_decrypt() {
        let dir = StoreDir::new();

        // Session A: write with master(1).
        {
            let store = open_store(&dir, test_master(1)).await;
            store
                .domain(KeyDomain::Secrets)
                .put(b"api-key", b"sk-plaintext")
                .await
                .unwrap();
            drop(store);
        }

        // Session B: reopen with master(2). The file opens fine
        // (redb doesn't know anything about our encryption layer);
        // the failure surfaces at `get` time when AEAD open fails.
        let store = open_store(&dir, test_master(2)).await;
        let err = store
            .domain(KeyDomain::Secrets)
            .get(b"api-key")
            .await
            .unwrap_err();
        assert!(
            matches!(err, StorageError::DecryptFailed { domain: KeyDomain::Secrets }),
            "unexpected error: {err:?}"
        );
    }

    #[tokio::test]
    async fn aad_binding_detects_key_rename() {
        // Write under key "a", then try to rewrite the stored bytes
        // under key "b" and read them back. The "read b" path will
        // build AAD containing "b", AEAD-open against ciphertext
        // bound to "a"'s AAD, and fail. We can't easily inject
        // forged bytes through the public API, so this test verifies
        // the AAD *function* itself encodes the key — a hand-compute.
        let dir = StoreDir::new();
        let store = open_store(&dir, test_master(8)).await;
        let handle = store.domain(KeyDomain::Sessions);

        let aad_a = handle.aad(b"key-a");
        let aad_b = handle.aad(b"key-b");
        assert_ne!(aad_a, aad_b);
        assert!(aad_a.starts_with(b"aivyx-v1"));
        assert!(aad_a.ends_with(b"key-a"));
    }

    // ---- Smoke: large-ish values go through -------------------------

    #[tokio::test]
    async fn large_value_round_trips() {
        let dir = StoreDir::new();
        let store = open_store(&dir, test_master(9)).await;
        let handle = store.domain(KeyDomain::Memory);

        // 64 KiB — big enough to exercise the "ChaCha20 streams many
        // blocks" path, small enough to keep the test fast.
        let big = vec![0xAB; 64 * 1024];
        handle.put(b"big", &big).await.unwrap();
        let got = handle.get(b"big").await.unwrap();
        assert_eq!(got, Some(big));
    }

    // ---- next_lex — pure helper --------------------------------------

    #[test]
    fn next_lex_increments_last_non_ff_byte() {
        assert_eq!(next_lex(b"notes\0").as_deref(), Some(&b"notes\x01"[..]));
        assert_eq!(next_lex(b"a").as_deref(), Some(&b"b"[..]));
        // Truncate: the carry-and-truncate case. Input [0x01, 0xFF]
        // → output [0x02] (the 0xFF is dropped because we increment
        // the first non-FF byte and drop everything after).
        assert_eq!(next_lex(&[0x01, 0xFF]).as_deref(), Some(&[0x02][..]));
    }

    #[test]
    fn next_lex_returns_none_for_unbounded_cases() {
        // Empty prefix: matches everything, no finite upper bound.
        assert_eq!(next_lex(b""), None);
        // All 0xFFs: no byte can be incremented without overflowing,
        // so there's no valid successor within the same length.
        assert_eq!(next_lex(&[0xFF, 0xFF, 0xFF]), None);
    }

    // ---- scan_prefix --------------------------------------------------

    #[tokio::test]
    async fn scan_prefix_returns_matching_keys_sorted_ascending() {
        let dir = StoreDir::new();
        let store = open_store(&dir, test_master(10)).await;
        let handle = store.domain(KeyDomain::Memory);

        // Put three keys under the "notes\0" prefix (Phase 6 Memory
        // layout: topic || 0x00 || seq_be). The scan must return all
        // three, sorted by key ascending — which corresponds to seq
        // ascending because of big-endian encoding.
        for seq in 0u64..3 {
            let mut key = b"notes\0".to_vec();
            key.extend_from_slice(&seq.to_be_bytes());
            handle
                .put(&key, format!("body-{seq}").as_bytes())
                .await
                .unwrap();
        }

        let rows = handle.scan_prefix(b"notes\0").await.unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].1, b"body-0");
        assert_eq!(rows[1].1, b"body-1");
        assert_eq!(rows[2].1, b"body-2");
        // Verify ordering is by key ascending by checking the final
        // 8 bytes of each returned key are monotonically increasing.
        let seqs: Vec<u64> = rows
            .iter()
            .map(|(k, _)| {
                let tail = &k[k.len() - 8..];
                let mut buf = [0u8; 8];
                buf.copy_from_slice(tail);
                u64::from_be_bytes(buf)
            })
            .collect();
        assert_eq!(seqs, vec![0, 1, 2]);
    }

    #[tokio::test]
    async fn scan_prefix_isolates_sibling_topics() {
        // Regression lock: prefix "notes\0" must not pick up keys
        // under "notesfoo\0" even though "notesfoo" starts with
        // "notes". The 0x00 separator is what makes these disjoint.
        let dir = StoreDir::new();
        let store = open_store(&dir, test_master(11)).await;
        let handle = store.domain(KeyDomain::Memory);

        let mut notes_key = b"notes\0".to_vec();
        notes_key.extend_from_slice(&0u64.to_be_bytes());
        handle.put(&notes_key, b"real-notes").await.unwrap();

        let mut notesfoo_key = b"notesfoo\0".to_vec();
        notesfoo_key.extend_from_slice(&0u64.to_be_bytes());
        handle.put(&notesfoo_key, b"sibling").await.unwrap();

        let notes_rows = handle.scan_prefix(b"notes\0").await.unwrap();
        assert_eq!(notes_rows.len(), 1);
        assert_eq!(notes_rows[0].1, b"real-notes");

        let notesfoo_rows = handle.scan_prefix(b"notesfoo\0").await.unwrap();
        assert_eq!(notesfoo_rows.len(), 1);
        assert_eq!(notesfoo_rows[0].1, b"sibling");
    }

    #[tokio::test]
    async fn scan_prefix_empty_prefix_returns_every_key_in_domain() {
        let dir = StoreDir::new();
        let store = open_store(&dir, test_master(12)).await;
        let handle = store.domain(KeyDomain::Memory);

        handle.put(b"a", b"av").await.unwrap();
        handle.put(b"b", b"bv").await.unwrap();
        handle.put(b"c", b"cv").await.unwrap();

        let rows = handle.scan_prefix(b"").await.unwrap();
        assert_eq!(rows.len(), 3);
        // Confirm every row decrypted successfully and the domain is
        // exhaustively covered.
        let vals: Vec<Vec<u8>> = rows.into_iter().map(|(_, v)| v).collect();
        assert!(vals.contains(&b"av".to_vec()));
        assert!(vals.contains(&b"bv".to_vec()));
        assert!(vals.contains(&b"cv".to_vec()));
    }

    #[tokio::test]
    async fn scan_prefix_nonexistent_prefix_returns_empty_vec() {
        let dir = StoreDir::new();
        let store = open_store(&dir, test_master(13)).await;
        let handle = store.domain(KeyDomain::Memory);
        handle.put(b"real-key", b"real-value").await.unwrap();

        let rows = handle.scan_prefix(b"ghost-prefix").await.unwrap();
        assert!(rows.is_empty());
    }

    // ---- Phase 7 task 6: filesystem permission hardening ----------

    #[cfg(unix)]
    #[tokio::test]
    async fn cold_open_chmods_store_file_to_0600() {
        use std::os::unix::fs::PermissionsExt;

        // Cold-start path: the store file does not exist before
        // `open`, so the `was_cold` probe returns true and the chmod
        // step runs. Asserting on `& 0o777` masks off the file-type
        // bits — on stat-back the `mode()` bits include S_IFREG, and
        // we only care about the permission-bit portion.
        let dir = StoreDir::new();
        assert!(
            !dir.path().exists(),
            "precondition: store path must not exist before open"
        );

        let _store = open_store(&dir, test_master(30)).await;

        let meta = fs::metadata(dir.path()).expect("store file must exist after open");
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "cold-opened store file must be chmod 0600, got 0o{mode:o}"
        );

        // A second open against the *same* path is a warm reopen.
        // `was_cold` returns false because the file now exists, so
        // the chmod path is skipped. We assert this by pre-mutating
        // the perms to 0o640 and confirming a reopen does NOT fight
        // the operator back to 0o600 — the cold-only policy from
        // Q6d in action.
        std::fs::set_permissions(
            dir.path(),
            std::fs::Permissions::from_mode(0o640),
        )
        .expect("chmod pre-mutation must succeed");

        // The existing store is still open; drop it to release the
        // file lock before reopening.
        drop(_store);
        let _store2 = open_store(&dir, test_master(30)).await;

        let mode2 = fs::metadata(dir.path()).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode2, 0o640,
            "warm reopen must not force perms back to 0600, got 0o{mode2:o}"
        );
    }

    #[tokio::test]
    async fn scan_prefix_on_wrong_master_fails_on_first_row() {
        // Same shape as `open_wrong_key_fails_to_decrypt` but for the
        // scan path: write with master(20), reopen with master(21),
        // first row the scan tries to decrypt fails with
        // DecryptFailed, and the scan aborts with that error (doesn't
        // skip the bad row and return the rest).
        let dir = StoreDir::new();
        {
            let store = open_store(&dir, test_master(20)).await;
            store
                .domain(KeyDomain::Memory)
                .put(b"topic\0\x00\x00\x00\x00\x00\x00\x00\x00", b"hello")
                .await
                .unwrap();
            drop(store);
        }

        let store = open_store(&dir, test_master(21)).await;
        let err = store
            .domain(KeyDomain::Memory)
            .scan_prefix(b"topic\0")
            .await
            .unwrap_err();
        assert!(
            matches!(err, StorageError::DecryptFailed { domain: KeyDomain::Memory }),
            "unexpected error: {err:?}"
        );
    }
}
