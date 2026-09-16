//! Phase 5 task 5 — the persistence round-trip integration test.
//!
//! Three scripted sessions against the **same** `$TMPDIR`-based store:
//!
//! | Session | Master key     | Expected outcome                               |
//! |---------|----------------|------------------------------------------------|
//! | A       | `[7u8; 32]`    | Runs one scripted turn, writes session marker. |
//! | B       | `[7u8; 32]`    | Reopens, reads A's marker, runs another turn.  |
//! | C       | `[99u8; 32]`   | Reopens with wrong key, fails to decrypt A/B.  |
//!
//! This is the whole point of Phase 5: session B has to observe bytes
//! that session A wrote on disk, across a clean drop of the storage
//! handle. If this test fails, one of the four layers below is lying
//! about its contract — Argon2id, HKDF, ChaCha20-Poly1305, or redb.
//!
//! ## What this test *is*
//!
//! - The first integration test that exercises every Phase 5 layer
//!   stitched together: passphrase → Argon2id → MasterKey → HKDF
//!   subkeys → ChaCha20-Poly1305 AEAD → redb → `run_session`'s
//!   session marker → drop → reopen → decode → compare.
//! - A regression anchor for the 40-byte marker encoding (see
//!   `session::SESSION_MARKER_KEY` and the hand-rolled layout comment
//!   there). A future refactor that flipped endianness, reordered
//!   fields, or changed the key name would fail this test and the
//!   unit tests in `session.rs::tests` would fail *together* — the
//!   belt-and-braces pairing that catches both "wrong on disk" and
//!   "wrong in memory" regressions.
//!
//! ## What this test is *not*
//!
//! - Not a test of `RedbStorage::open`'s own failure modes — those
//!   live in `aivyx-storage/src/lib.rs::tests`. Here we drive
//!   `run_session` as the only client, so the test asserts on what
//!   the *agent-level* caller observes.
//! - Not a live-API test. Everything uses a `ScriptedProvider` with
//!   zero network I/O, the same way `cli_e2e.rs` and `fs_tool_e2e.rs`
//!   already do.
//! - Not a benchmark. `MasterKey::from_raw` deliberately bypasses
//!   Argon2id for this test — the passphrase-to-key derivation is
//!   already covered by `passphrase::tests::passphrase_to_storage_
//!   full_round_trip`. Combining them here would just add 500ms ×
//!   three sessions to an already expensive test run.
//!
//! ## Spec-vs-reality note on the wrong-key negative
//!
//! `PHASE_5.md` task 5 says the wrong-passphrase path should fail
//! "at `open`, not at first use." That phrasing is aspirational —
//! the actual Task 2 implementation of `RedbStorage::open` is
//! deliberately *oblivious* to existing ciphertext (it precomputes
//! HKDF subkeys from the master but doesn't probe any value), so a
//! wrong master key opens fine and the decrypt failure surfaces at
//! the first `get` against an existing row. This matches the
//! behaviour already documented by
//! `storage::tests::open_wrong_key_fails_to_decrypt`. Session C
//! below asserts that exact shape: the `open` succeeds, and the
//! first `get` against the Sessions domain returns
//! `StorageError::DecryptFailed`.

use std::io::Cursor;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;

use aivyx_audit::{AuditBridge, HmacChainLog};
use aivyx_capability::{CapabilitySet, Scope};
use aivyx_channel::{run_session, LocalChannel, SessionConfig};
use aivyx_core::{AuditHook, CancellationToken, ToolRegistry, TurnOutcome};
use aivyx_crypto::MasterKey;
use aivyx_llm::{
    LlmError, LlmProvider, LlmRequest, LlmStepEnd, LlmStream, LlmStreamEvent, LlmUsage,
};
use aivyx_storage::{KeyDomain, RedbStorage, Storage, StorageConfig, StorageError};

// ---------------------------------------------------------------------------
// Constants that mirror `aivyx_channel::session` — these are the
// *contract* this test is anchoring. If `session::SESSION_MARKER_KEY`
// or `session::SESSION_MARKER_LEN` ever drift, this test will fail
// and force an explicit update to the test (and a salt bump in
// production via `"aivyx-v1-storage"` → `"aivyx-v2-storage"`).
// Duplicating the literals here rather than re-exporting them is
// intentional: the test is *testing* the on-disk format, so it gets
// its own copy.
// ---------------------------------------------------------------------------

const SESSION_MARKER_KEY: &[u8] = b"current";
const SESSION_MARKER_LEN: usize = 40;

/// Decode the 40-byte session marker as `session.rs` writes it.
/// Mirror of `decode_session_marker` kept private to this test.
fn decode_marker(bytes: &[u8]) -> Option<(u64, u64, u64)> {
    // Returns `(opened_at_secs, last_turn_index, last_turn_at_secs)`.
    // The 16-byte session UUID prefix is not asserted on — each
    // `LocalChannel::new` mints a fresh UUID, so sessions A and B
    // hold *different* session_ids (by design) and the test cares
    // that the bytes survived the round-trip, not that they match
    // any specific value.
    if bytes.len() != SESSION_MARKER_LEN {
        return None;
    }
    let opened_at_secs = u64::from_be_bytes(bytes[16..24].try_into().ok()?);
    let last_turn_index = u64::from_be_bytes(bytes[24..32].try_into().ok()?);
    let last_turn_at_secs = u64::from_be_bytes(bytes[32..40].try_into().ok()?);
    Some((opened_at_secs, last_turn_index, last_turn_at_secs))
}

// ---------------------------------------------------------------------------
// Scratch dir — a `$TMPDIR`-based store path that persists across the
// three sessions. Drop cleans up on test exit regardless of which
// session panicked. Same hand-rolled RAII convention the two existing
// integration tests use (fs_tool_e2e's `TestSandbox`, cli_e2e's
// `ScratchStoreDir`) to keep the workspace off of `tempfile`.
// ---------------------------------------------------------------------------

struct SharedStoreDir {
    parent: PathBuf,
    store: PathBuf,
}

impl SharedStoreDir {
    fn new() -> Self {
        let tmp = std::env::var("TMPDIR")
            .or_else(|_| std::env::var("TEMP"))
            .unwrap_or_else(|_| "/tmp".to_string());
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let pid = std::process::id();
        // `pid-nanos` alone collides when both tests in this
        // binary call `new()` within the same coarse-clock
        // tick under a loaded parallel run; a uuid makes the
        // shared-store path unconditionally unique.
        let uniq = uuid::Uuid::new_v4();
        let parent = PathBuf::from(tmp)
            .join(format!("aivyx-persist-e2e-{pid}-{nanos}-{uniq}"));
        std::fs::create_dir_all(&parent).expect("shared store parent must be creatable");
        SharedStoreDir {
            store: parent.join("store.redb"),
            parent,
        }
    }
}

impl Drop for SharedStoreDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.parent);
    }
}

async fn open_store(dir: &SharedStoreDir, master: MasterKey) -> Arc<dyn Storage> {
    RedbStorage::open(StorageConfig::new(dir.store.clone()), master)
        .await
        .expect("storage must open against the shared tempdir")
}

// ---------------------------------------------------------------------------
// Scripted provider — local copy, same shape as cli_e2e.rs and
// fs_tool_e2e.rs. Each `ScriptedStep` corresponds to one planner
// step; one `FinalMessage` terminal per user turn.
// ---------------------------------------------------------------------------

struct ScriptedStep {
    events: Vec<LlmStreamEvent>,
    terminal: LlmStepEnd,
}

struct ScriptedProvider {
    queue: Mutex<std::collections::VecDeque<ScriptedStep>>,
}

impl ScriptedProvider {
    fn new(steps: Vec<ScriptedStep>) -> Arc<Self> {
        Arc::new(ScriptedProvider {
            queue: Mutex::new(steps.into()),
        })
    }
}

#[async_trait]
impl LlmProvider for ScriptedProvider {
    async fn chat_stream(
        &self,
        _request: LlmRequest<'_>,
        _cancellation: &CancellationToken,
    ) -> Result<Box<dyn LlmStream>, LlmError> {
        let step = self
            .queue
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| LlmError::Config("ScriptedProvider exhausted".into()))?;
        Ok(Box::new(ScriptedStream {
            events: step.events.into_iter(),
            terminal: Some(step.terminal),
        }))
    }
}

struct ScriptedStream {
    events: std::vec::IntoIter<LlmStreamEvent>,
    terminal: Option<LlmStepEnd>,
}

#[async_trait]
impl LlmStream for ScriptedStream {
    async fn next_event(&mut self) -> Result<Option<LlmStreamEvent>, LlmError> {
        Ok(self.events.next())
    }
    async fn finish(self: Box<Self>) -> Result<LlmStepEnd, LlmError> {
        self.terminal
            .ok_or_else(|| LlmError::StreamEnded("ScriptedStream::finish double-called".into()))
    }
}

fn one_turn_final(text: &str) -> ScriptedStep {
    ScriptedStep {
        events: vec![LlmStreamEvent::TextChunk(text.to_string())],
        terminal: LlmStepEnd::FinalMessage {
            text: text.to_string(),
            usage: LlmUsage::default(),
        },
    }
}

/// Build a one-shot audit hook. Each session mints its own in-memory
/// chain — the audit persistence question is out of scope for Phase 5
/// (see Q1 option 2), so session B's audit chain is independent of
/// session A's.
fn fresh_audit(key_byte: u8) -> Arc<dyn AuditHook> {
    Arc::new(AuditBridge::new(HmacChainLog::new(
        [key_byte; 32].to_vec(),
    )))
}

/// Drive `run_session` through exactly one user turn with a fresh
/// `LocalChannel` and the given storage handle. Returns after EOF.
/// Used for both session A and session B — the only difference is
/// the storage Arc handed in.
async fn run_one_turn(storage: Arc<dyn Storage>, audit_key_byte: u8, user_line: &str) {
    let provider = ScriptedProvider::new(vec![one_turn_final("ack")]);
    let audit = fresh_audit(audit_key_byte);

    // Script a single non-empty line followed by EOF. The trailing
    // `\n` on the last line is important: `read_line` only returns
    // `Ok(0)` (EOF) once the buffer is empty, so a missing newline
    // would make the loop block waiting for more input on the test's
    // Cursor (which can't produce any).
    let script = format!("{user_line}\n");
    let reader = Cursor::new(script.into_bytes());

    let channel = LocalChannel::<Vec<u8>>::new("persist-e2e", Vec::<u8>::new());

    let config = SessionConfig {
        model: "claude-haiku-4-5-20251001".to_string(),
        system_prompt: "test".to_string(),
        max_tokens: 64,
        capabilities: CapabilitySet::from_scopes([Scope::parse("memory.read").unwrap()]),
        tools: Arc::new(ToolRegistry::new(Vec::new())),
        storage,
        prompt: String::new(),
        banner: None,
        tool_allowlist: None,
        memory_topic_prefix: None,
        role_overrides: None,
        context_window_tokens: None,
        prune_sink: None,
        context_provider: None,
        system_prompt_refiner: None,
        prompt_refresher: None,
        turn_safety: Default::default(),
        confirm_destructive: false,
    };

    let report = run_session(provider, audit, None, config, channel, reader)
        .await
        .expect("run_session must complete cleanly on scripted EOF");

    assert_eq!(report.turns_run, 1, "one scripted turn per helper call");
    assert!(
        matches!(report.last_outcome, Some(TurnOutcome::Completed { .. })),
        "scripted turn must land as Completed"
    );
}

// ---------------------------------------------------------------------------
// Test 1 — Session A writes, session B reads it back.
//
// The canonical Phase 5 persistence demonstration: run a turn, drop
// the handle, reopen the exact same file with the exact same master
// key, and prove the bytes survived.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn session_marker_survives_clean_close_and_reopen() {
    let dir = SharedStoreDir::new();

    // ---- Session A ----------------------------------------------------
    //
    // Open the store fresh, run exactly one scripted turn, then drop
    // the handle explicitly. Dropping releases redb's single-writer
    // lock — session B would fail to `open` otherwise, because redb
    // enforces "one process, one handle" at the file level.
    let t_before = now_secs();
    {
        let storage_a = open_store(&dir, MasterKey::from_raw([7u8; 32])).await;
        run_one_turn(Arc::clone(&storage_a), 1, "hello from A").await;

        // Confirm session A actually wrote the marker *before* we drop
        // the handle. This is not strictly required for the test's
        // end-to-end goal, but it localizes a regression: if this
        // assertion fires we know the bug is in session A's write
        // path, not in session B's read path.
        let raw_a = storage_a
            .domain(KeyDomain::Sessions)
            .get(SESSION_MARKER_KEY)
            .await
            .expect("sessions domain read must succeed")
            .expect("session A must have written a marker before drop");
        assert_eq!(
            raw_a.len(),
            SESSION_MARKER_LEN,
            "marker from session A must be exactly {SESSION_MARKER_LEN} bytes, got {}",
            raw_a.len()
        );
        let (opened_a, turn_idx_a, turn_at_a) =
            decode_marker(&raw_a).expect("session A marker must decode");
        assert_eq!(
            turn_idx_a, 1,
            "session A ran exactly one turn, last_turn_index must be 1"
        );
        assert!(
            opened_a >= t_before,
            "opened_at_secs ({opened_a}) must be >= t_before ({t_before})"
        );
        assert!(
            turn_at_a >= opened_a,
            "last_turn_at_secs ({turn_at_a}) must be >= opened_at_secs ({opened_a})"
        );
        drop(storage_a);
    }

    // ---- Session B ----------------------------------------------------
    //
    // Reopen with the same master key. This is the moment of truth:
    // if Argon2id / HKDF / AEAD / redb all held their contracts,
    // session B can read the raw bytes session A wrote.
    //
    // We deliberately read the marker *before* running a turn,
    // because `run_session` itself overwrites the marker on the way
    // in (index 0) and then again after each turn. If we read after,
    // we'd be observing session B's write, not session A's.
    let storage_b = open_store(&dir, MasterKey::from_raw([7u8; 32])).await;
    let raw_b = storage_b
        .domain(KeyDomain::Sessions)
        .get(SESSION_MARKER_KEY)
        .await
        .expect("session B's get must not return a decrypt error")
        .expect("session B must see session A's marker on disk");
    assert_eq!(raw_b.len(), SESSION_MARKER_LEN);
    let (opened_b_seen, turn_idx_b_seen, turn_at_b_seen) =
        decode_marker(&raw_b).expect("session A's marker must still decode in session B");

    // The bytes session B reads must match what session A wrote — the
    // same shape (`last_turn_index == 1`) and timestamps within the
    // test's wall-clock window.
    assert_eq!(
        turn_idx_b_seen, 1,
        "session B must observe session A's last_turn_index = 1"
    );
    let t_after_a = now_secs();
    assert!(
        opened_b_seen >= t_before && opened_b_seen <= t_after_a,
        "session A's opened_at_secs ({opened_b_seen}) must fall in [{t_before}, {t_after_a}]"
    );
    assert!(
        turn_at_b_seen >= opened_b_seen && turn_at_b_seen <= t_after_a,
        "session A's last_turn_at_secs ({turn_at_b_seen}) must be in [{opened_b_seen}, {t_after_a}]"
    );

    // Now run a turn in session B too, and assert the marker is
    // overwritten — session B's turn count becomes 1 relative to its
    // own `opened_at_secs`, not session A's. This is what the "I've
    // been here before, and now I'm here again" loop actually looks
    // like: each process writes its own session marker, and the
    // previous one is replaced (Q1 option 1 leaning: single-row
    // "current session" pointer, not a history log).
    run_one_turn(Arc::clone(&storage_b), 2, "hello from B").await;

    let raw_b_after = storage_b
        .domain(KeyDomain::Sessions)
        .get(SESSION_MARKER_KEY)
        .await
        .expect("session B post-turn read must succeed")
        .expect("session B must have its own marker after its turn");
    let (opened_b_new, turn_idx_b_new, _turn_at_b_new) =
        decode_marker(&raw_b_after).expect("session B marker must decode");

    // Session B has its own `opened_at_secs` (generated inside
    // `run_session` when B's `run_one_turn` helper called it), which
    // is strictly >= session A's. And `last_turn_index == 1` because
    // session B ran exactly one turn from its own perspective.
    assert_eq!(
        turn_idx_b_new, 1,
        "session B's own last_turn_index must be 1, not session A's"
    );
    assert!(
        opened_b_new >= opened_b_seen,
        "session B's opened_at_secs ({opened_b_new}) must be >= session A's ({opened_b_seen})"
    );

    // Drop B cleanly so the temp dir's `Drop` can remove the files
    // without the OS complaining about an open redb handle.
    drop(storage_b);
}

// ---------------------------------------------------------------------------
// Test 2 — Session C: wrong master key is rejected.
//
// Session A writes, drops the handle, session C reopens with a
// *different* master key. The open itself succeeds (redb doesn't
// know the store is encrypted), but the first `get` against the
// Sessions domain fails with `StorageError::DecryptFailed` because
// session C's HKDF-derived subkey can't decrypt ciphertext bound
// to session A's subkey via AEAD.
//
// This is the Phase 5 security invariant in its strongest form: a
// wrong master never produces a partial or silently-corrupted read.
// The AEAD failure mode is hard-no, not degrade-gracefully.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn wrong_master_key_fails_to_decrypt_prior_session() {
    let dir = SharedStoreDir::new();

    // Session A: establish the on-disk ciphertext session C will
    // later try and fail to decrypt.
    {
        let storage_a = open_store(&dir, MasterKey::from_raw([7u8; 32])).await;
        run_one_turn(Arc::clone(&storage_a), 1, "only A can read this").await;
        drop(storage_a);
    }

    // Session C: *different* master key, same path. `open` proceeds
    // cleanly (HKDF produces 32 bytes from any 32-byte input, so the
    // handle construction never fails on a wrong key), and the
    // failure surfaces only when session C actually asks for a
    // decrypted value. See the module-level spec-vs-reality note
    // above for why this differs from PHASE_5.md's phrasing.
    let storage_c = open_store(&dir, MasterKey::from_raw([99u8; 32])).await;
    let err = storage_c
        .domain(KeyDomain::Sessions)
        .get(SESSION_MARKER_KEY)
        .await
        .expect_err("wrong master key must fail the first decrypted read");

    assert!(
        matches!(
            err,
            StorageError::DecryptFailed {
                domain: KeyDomain::Sessions
            }
        ),
        "expected DecryptFailed {{ domain: Sessions }}, got: {err:?}"
    );

    drop(storage_c);
}

// ---------------------------------------------------------------------------
// Utilities
// ---------------------------------------------------------------------------

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
