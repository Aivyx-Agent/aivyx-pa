//! Phase 7 task 7 — the audit chain persistence integration test.
//!
//! Two separate `#[tokio::test]`s, each against its own
//! `$TMPDIR`-based store:
//!
//! | Test | Sessions                                                         |
//! |------|------------------------------------------------------------------|
//! | 1    | A runs one scripted turn through `PersistentAuditLog`, drops.   |
//! |      | B reopens `PersistentAuditLog` and asserts the verified chain   |
//! |      | contains every event A emitted in the right order.              |
//! | 2    | A runs one scripted turn, drops.                                |
//! |      | Test code decrypts row `seq=0`, flips `entry.mac[0]`, writes    |
//! |      | back, then calls `verify_from_disk` and asserts `ChainBroken`.  |
//!
//! This is the whole point of Phase 7 task 1: **the audit chain
//! survives a process restart and tamper is detectable at reopen**.
//! Every Phase 7 task before this one has been building the
//! infrastructure — persistent drain, shared `scan_decode_verify`,
//! binary wire-up — and this test is the smallest observable that
//! stitches them all together end-to-end through `run_session`.
//!
//! ## Scope narrowing vs. the draft
//!
//! The PHASE_7.md draft said "a scripted turn that calls both
//! `memory.write` and `fs.read`." Shipped scope is `memory.write`
//! only — the load-bearing question is "does `PersistentAuditLog`
//! survive a clean drop + reopen," and a single `memory.write` call
//! already produces `TurnStarted / MemoryAccess(Write) /
//! ToolCall(Completed, memory.write) / TurnEnded` on the audit chain,
//! covering three distinct `AuditTag` variants and four
//! `AuditEvent`s. Adding `fs.read` would drag in ~80 lines of
//! `FsReadToolConfig` + sandbox harness for zero additional signal
//! about audit persistence. Recorded as a Task 7 scope narrowing
//! in the shipped PHASE_7.md record.
//!
//! ## What this test is *not*
//!
//! - **Not a unit test of `PersistentAuditLog` internals.** Those
//!   live in `aivyx-audit::persistent::tests` — the ten reopen,
//!   tamper, truncation, and corrupt-value cases shipped with
//!   Task 1, plus the four `verify_from_disk` cases shipped with
//!   Task 2. Here we drive the audit hook *through `run_session`*,
//!   proving the same invariants under the real agent/loop path.
//! - **Not a live-LLM test.** `ScriptedProvider` produces the two
//!   `LlmStepEnd`s the turn needs, zero network I/O. Same pattern
//!   as the other integration tests in this directory.
//! - **Not a test of the binary.** `cli_e2e.rs` already covers the
//!   binary wire-up and the `--verify-only` flag. This test is
//!   strictly `run_session`-level, so a regression in `aivyx.rs`'s
//!   plumbing cannot mask a regression in `PersistentAuditLog`.
//!
//! ## Key layout duplication
//!
//! Like `storage_persistence_e2e.rs` duplicates `SESSION_MARKER_KEY`,
//! this test duplicates the `b"a\0" || seq_be` audit-row key layout
//! rather than re-exporting it from `aivyx-audit`. The test is
//! *testing* that on-disk format; a future key-layout change must
//! fail this test loudly so the change is explicit. If a future
//! layout becomes a public contract, pull the constant up to
//! `aivyx-audit` and delete the duplication here.

use std::io::Cursor;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde_json::json;

use aivyx_audit::{
    AuditError, AuditEvent, MemoryOperation, PersistentAuditLog, SignedEntry,
};
use aivyx_capability::{CapabilitySet, Scope};
use aivyx_channel::{run_session, LocalChannel, SessionConfig};
use aivyx_core::{
    AuditHook, CancellationToken, Tool, ToolOutcomeSummary, ToolRegistry, TurnOutcome,
    TurnOutcomeSummary,
};
use aivyx_crypto::MasterKey;
use aivyx_llm::{
    LlmError, LlmProvider, LlmRequest, LlmStepEnd, LlmStream, LlmStreamEvent, LlmUsage,
    ToolCallEnd,
};
use aivyx_memory::{Memory, MemoryForgetTool, MemoryReadTool, MemoryWriteTool, RedbMemory};
use aivyx_storage::{KeyDomain, RedbStorage, Storage, StorageConfig};

// ---------------------------------------------------------------------------
// Audit row key layout (local copy — see module header for rationale)
// ---------------------------------------------------------------------------

/// Prefix for every audit row under `KeyDomain::Audit`. The `\0`
/// sentinel prevents any future namespace in the same domain from
/// colliding with this one. Must match `aivyx_audit::persistent`'s
/// private `AUDIT_KEY_PREFIX` byte-for-byte.
const AUDIT_KEY_PREFIX: &[u8] = b"a\0";

/// Rebuild the `b"a\0" || seq_be` row key for a given sequence
/// number. Mirror of `aivyx_audit::persistent::audit_key`, which is
/// not pub.
fn audit_row_key(seq: u64) -> Vec<u8> {
    let mut key = Vec::with_capacity(AUDIT_KEY_PREFIX.len() + 8);
    key.extend_from_slice(AUDIT_KEY_PREFIX);
    key.extend_from_slice(&seq.to_be_bytes());
    key
}

// ---------------------------------------------------------------------------
// Scratch dir — RAII tempdir shared across one test's sessions
// ---------------------------------------------------------------------------

struct SharedStoreDir {
    parent: PathBuf,
    store: PathBuf,
}

impl SharedStoreDir {
    fn new(tag: &str) -> Self {
        let tmp = std::env::var("TMPDIR")
            .or_else(|_| std::env::var("TEMP"))
            .unwrap_or_else(|_| "/tmp".to_string());
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let pid = std::process::id();
        // `pid-nanos` alone collides when two tests in this binary call
        // `new()` within the same coarse-clock tick under a loaded parallel
        // run, and the second `RedbStorage::open` fails on the redb lock; a
        // uuid makes the path unconditionally unique (see storage_persistence_e2e).
        let uniq = uuid::Uuid::new_v4();
        let parent = PathBuf::from(tmp)
            .join(format!("aivyx-audit-e2e-{tag}-{pid}-{nanos}-{uniq}"));
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

/// The 32-byte master used by every session in this file. Same
/// rationale as `memory_tool_e2e.rs`: we deliberately bypass Argon2id
/// because `passphrase::tests::passphrase_to_storage_full_round_trip`
/// already covers the passphrase→key path, and combining them would
/// burn ~500ms per session on a test whose point is audit round-trip.
const TEST_MASTER: [u8; 32] = [7u8; 32];

/// Deterministic HMAC key for the persistent audit chain. Same
/// stability-for-assertions reason the other e2e tests pin their
/// audit keys.
const TEST_AUDIT_KEY: [u8; 32] = [0x42u8; 32];

async fn open_store(dir: &SharedStoreDir) -> Arc<dyn Storage> {
    RedbStorage::open(
        StorageConfig::new(dir.store.clone()),
        MasterKey::from_raw(TEST_MASTER),
    )
    .await
    .expect("scratch storage must open")
}

// ---------------------------------------------------------------------------
// Drain fence — wait for the persistent audit log's background drain
// task to flush at least `expected` rows to disk before the caller
// drops the log. Local copy of the same helper in
// `persistent::tests::wait_for_disk`, because the background drain
// goes through a bounded mpsc and we cannot observe flush-completion
// synchronously from the `AuditHook::on_event` path.
// ---------------------------------------------------------------------------

async fn wait_for_audit_rows(storage: &Arc<dyn Storage>, expected: usize) {
    let handle = storage.domain(KeyDomain::Audit);
    for _ in 0..2000 {
        let rows = handle
            .scan_prefix(AUDIT_KEY_PREFIX)
            .await
            .expect("audit scan_prefix must succeed");
        if rows.len() == expected {
            return;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    panic!("persistent audit drain never flushed {expected} rows to disk");
}

// ---------------------------------------------------------------------------
// Scripted provider — local copy, same shape as the other e2e tests.
// Two `ScriptedStep`s per turn: one `ToolCall` terminal driving the
// planner to dispatch `memory.write`, one `FinalMessage` terminal
// closing the turn.
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

fn zero_usage() -> LlmUsage {
    LlmUsage::default()
}

/// Build the fixed two-step script that drives one `memory.write`
/// call under the topic `notes` with a known body, followed by a
/// final message.
fn memory_write_script(topic: &str, body: &str) -> Vec<ScriptedStep> {
    vec![
        ScriptedStep {
            events: vec![],
            terminal: LlmStepEnd::ToolCalls {
                calls: vec![ToolCallEnd {
                    call_id: "toolu_write_01".to_string(),
                    tool_name: "memory.write".to_string(),
                    input: json!({ "topic": topic, "body": body }),
                    name_resolution: aivyx_llm::NameResolution::Known,
                }],
                text_so_far: String::new(),
                usage: zero_usage(),
            },
        },
        ScriptedStep {
            events: vec![LlmStreamEvent::TextChunk("stored it".into())],
            terminal: LlmStepEnd::FinalMessage {
                text: "stored it".to_string(),
                usage: zero_usage(),
            },
        },
    ]
}

// ---------------------------------------------------------------------------
// Memory tool harness — three `memory.*` tools wired to a fresh
// `RedbMemory` over the given storage. Same shape as
// `memory_tool_e2e.rs`'s harness, trimmed to just what this test
// needs.
// ---------------------------------------------------------------------------

struct MemoryHarness {
    tools: Arc<ToolRegistry>,
    capabilities: CapabilitySet,
}

async fn build_memory_harness(storage: Arc<dyn Storage>) -> MemoryHarness {
    let memory: Arc<dyn Memory> = RedbMemory::open(Arc::clone(&storage))
        .await
        .expect("RedbMemory::open must succeed over scratch storage");

    let memory_read = MemoryReadTool::new(Arc::clone(&memory));
    let memory_write = MemoryWriteTool::new(Arc::clone(&memory));
    let memory_forget = MemoryForgetTool::new(Arc::clone(&memory));

    let tools: Arc<ToolRegistry> = Arc::new(ToolRegistry::new(vec![
        Arc::new(memory_read) as Arc<dyn Tool>,
        Arc::new(memory_write) as Arc<dyn Tool>,
        Arc::new(memory_forget) as Arc<dyn Tool>,
    ]));

    let capabilities = CapabilitySet::from_scopes([
        Scope::parse("memory.read").unwrap(),
        Scope::parse("memory.write").unwrap(),
        Scope::parse("memory.forget").unwrap(),
    ]);

    MemoryHarness {
        tools,
        capabilities,
    }
}

fn base_session_config(harness: &MemoryHarness, storage: Arc<dyn Storage>) -> SessionConfig {
    SessionConfig {
        model: "claude-haiku-4-5-20251001".to_string(),
        system_prompt: "test".to_string(),
        max_tokens: 256,
        capabilities: harness.capabilities.clone(),
        tools: Arc::clone(&harness.tools),
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
    }
}

// ---------------------------------------------------------------------------
// Test 1 — Positive round-trip: A writes, B reopens and the chain
// replays cleanly with the exact 4-event shape.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn audit_chain_survives_clean_close_and_second_session_verifies_it() {
    let dir = SharedStoreDir::new("positive");

    // ====== Session A ================================================
    //
    // Run one scripted `memory.write` turn behind a live
    // `PersistentAuditLog`. Emit:
    //   seq 0: TurnStarted
    //   seq 1: MemoryAccess { op = Write, scope = memory.write:topic:notes }
    //   seq 2: ToolCall     { Completed, scope = memory.write:topic:notes }
    //   seq 3: TurnEnded    { Completed, tool_calls_made = 1 }
    //   seq 4: LlmCost      { model, usage } — Chapter K, the LLM-backed
    //                         planner reports a model so the turn epilogue
    //                         emits a priced cost event after TurnEnded.
    //
    // Drop both the log and the storage handle at block exit so
    // session B can reopen the file — redb enforces single-writer.

    {
        let storage = open_store(&dir).await;
        let harness = build_memory_harness(Arc::clone(&storage)).await;

        let persistent_audit =
            PersistentAuditLog::open(Arc::clone(&storage), TEST_AUDIT_KEY)
                .await
                .expect("session A persistent audit must open");
        assert_eq!(
            persistent_audit.len(),
            0,
            "session A must start with an empty chain"
        );

        let provider = ScriptedProvider::new(memory_write_script(
            "notes",
            "the user's favorite color is purple",
        ));

        // Two live references to the same `Arc`: one typed (for the
        // drain fence below), one as `dyn AuditHook` (for run_session).
        let audit_typed = Arc::new(persistent_audit);
        let audit_hook: Arc<dyn AuditHook> = audit_typed.clone();

        let stdin_script = b"remember my favorite color\n";
        let reader = Cursor::new(&stdin_script[..]);
        let channel = LocalChannel::<Vec<u8>>::new("audit-e2e-a", Vec::new());

        let config = base_session_config(&harness, Arc::clone(&storage));

        let report = run_session(
            Arc::clone(&provider) as Arc<dyn LlmProvider>,
            audit_hook,
            None,
            config,
            channel,
            reader,
        )
        .await
        .expect("session A must complete cleanly");

        assert_eq!(report.turns_run, 1);
        match report.last_outcome {
            Some(TurnOutcome::Completed {
                ref final_message,
                tool_calls_made,
                ..
            }) => {
                assert_eq!(final_message, "stored it");
                assert_eq!(tool_calls_made, 1);
            }
            other => panic!("session A: expected Completed(1), got {other:?}"),
        }

        // The in-memory chain has 4 entries synchronously, but the
        // background drain task is what lands them on disk. Fence on
        // the on-disk row count before dropping the log so session B
        // observes a fully-drained chain.
        assert_eq!(
            audit_typed.len(),
            5,
            "session A in-memory chain must hold 5 events after one turn \
             (TurnStarted / MemoryAccess / ToolCall / TurnEnded / LlmCost)"
        );
        wait_for_audit_rows(&storage, 5).await;

        // Drop order matters: every `Arc<dyn Storage>` / `DomainHandle` /
        // drain-task clone of the backing `Arc<Database>` must be released
        // before session B can acquire redb's single-writer file lock.
        //
        // - `harness` owns `RedbMemory` → `DomainHandle` → `Arc<Database>`
        //   via the three memory tools, so dropping it here is mandatory.
        // - `audit_typed` owns the drain task; its `Drop` calls
        //   `JoinHandle::abort()`, which is *non-blocking* — the task
        //   drops its captured `Arc<dyn Storage>` only when the runtime
        //   next polls it. We yield below so that polling happens.
        // - `provider` is a `ScriptedProvider` with no storage clone.
        drop(harness);
        drop(audit_typed);
        drop(storage);
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
    }

    // ====== Session B ================================================
    //
    // Reopen `PersistentAuditLog` against the same path. `open` runs
    // the shared `scan_decode_verify` pipeline on the way in — if
    // anything went wrong with the AEAD seal, the per-row nonce, the
    // HMAC chain replay, or the seq ordering, `open` returns
    // `Err(AuditError::...)` and this test fails at the unwrap.

    let storage = open_store(&dir).await;

    // Also run `verify_from_disk` as an independent cold-path check,
    // exactly the same way `aivyx-pa --verify-only` does it. This is
    // the strongest statement we can make about the on-disk chain
    // without spawning a drain task.
    let report = PersistentAuditLog::verify_from_disk(Arc::clone(&storage), TEST_AUDIT_KEY)
        .await
        .expect("verify_from_disk must succeed on a clean chain");
    assert_eq!(
        report.entries_verified, 5,
        "verify_from_disk must report 5 entries \
         (TurnStarted / MemoryAccess / ToolCall / TurnEnded / LlmCost)"
    );
    assert_eq!(
        report.head_seq,
        Some(4),
        "verify_from_disk head_seq must be 4 (= entries_verified - 1)"
    );

    let log = PersistentAuditLog::open(Arc::clone(&storage), TEST_AUDIT_KEY)
        .await
        .expect("session B PersistentAuditLog::open must succeed on a clean chain");
    assert_eq!(
        log.len(),
        5,
        "session B in-memory chain must be seeded with 5 verified entries"
    );

    // ---- The Level-3 assertion: every event's shape survived the
    //      AEAD seal → redb row → reopen scan → decode → HMAC
    //      replay pipeline. Any bit-rot anywhere in that stack
    //      would desync either the count or one of the four
    //      pattern matches below.

    let entries = log
        .entries()
        .expect("session B must be able to read the recovered chain");
    assert_eq!(entries.len(), 5, "chain must be exactly 5 entries");

    // seq 0 — TurnStarted (from the session loop's per-turn audit).
    assert_eq!(entries[0].seq, 0);
    assert!(
        matches!(&entries[0].event, AuditEvent::TurnStarted { .. }),
        "seq 0 must be TurnStarted, got {:?}",
        entries[0].event
    );

    // seq 1 — MemoryAccess, emitted by MemoryWriteTool before the
    // substrate put. This is the R1 payoff: the audit chain carries
    // the *narrowed* scope `memory.write:topic:notes`, not the broad
    // unqualified capability the caller holds.
    match &entries[1].event {
        AuditEvent::MemoryAccess {
            operation,
            scope,
            query_or_key,
            ..
        } => {
            assert_eq!(*operation, MemoryOperation::Write);
            assert_eq!(scope.base(), "memory.write");
            assert_eq!(scope.qualifier(), Some("topic:notes"));
            assert_eq!(query_or_key, "notes");
        }
        other => panic!("seq 1 must be MemoryAccess(Write), got {other:?}"),
    }
    assert_eq!(entries[1].seq, 1);

    // seq 2 — ToolCall wrapping the same tool, emitted by the
    // session loop. Scope narrowed the same way.
    match &entries[2].event {
        AuditEvent::ToolCall {
            outcome,
            scope_used,
            ..
        } => {
            assert!(
                matches!(outcome, ToolOutcomeSummary::Completed { .. }),
                "seq 2 ToolCall must be Completed, got {outcome:?}"
            );
            assert_eq!(scope_used.base(), "memory.write");
            assert_eq!(scope_used.qualifier(), Some("topic:notes"));
        }
        other => panic!("seq 2 must be ToolCall, got {other:?}"),
    }
    assert_eq!(entries[2].seq, 2);

    // seq 3 — TurnEnded, from the session loop's turn epilogue.
    match &entries[3].event {
        AuditEvent::TurnEnded {
            outcome,
            tool_calls_made,
            ..
        } => {
            assert_eq!(*outcome, TurnOutcomeSummary::Completed);
            assert_eq!(*tool_calls_made, 1);
        }
        other => panic!("seq 3 must be TurnEnded, got {other:?}"),
    }
    assert_eq!(entries[3].seq, 3);

    // seq 4 — LlmCost, Chapter K's per-turn priced-spend event. The
    // LLM-backed planner reports a model, so the turn epilogue emits
    // this after TurnEnded, carrying the model + token usage.
    match &entries[4].event {
        AuditEvent::LlmCost { model, .. } => {
            assert!(!model.is_empty(), "LlmCost must carry the planner's model");
        }
        other => panic!("seq 4 must be LlmCost, got {other:?}"),
    }
    assert_eq!(entries[4].seq, 4);

    // Chain-internal invariant: each entry's prev_mac chains back to
    // the previous entry's mac. `PersistentAuditLog::open` already
    // verified this during reopen (that's the whole point of Task 2),
    // but asserting it here in the test, in the test's own words,
    // makes the failure message crystal clear if the internal check
    // ever regresses silently.
    for i in 1..entries.len() {
        assert_eq!(
            entries[i].prev_mac, entries[i - 1].mac,
            "seq {i} prev_mac must chain back to seq {}", i - 1
        );
    }

    // Teardown: drop the live log before the storage so the drain
    // task can abort cleanly before the redb file lock is released.
    drop(log);
    drop(storage);
}

// ---------------------------------------------------------------------------
// Test 2 — Negative round-trip: session A writes, test code tampers
// seq 0's MAC, `verify_from_disk` reports `ChainBroken { seq: 0 }`.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tampered_audit_row_fails_verification_with_chain_broken() {
    let dir = SharedStoreDir::new("negative");

    // Session A — identical to test 1's session A. The turn emits
    // 5 events that land on disk under `KeyDomain::Audit`
    // (TurnStarted / MemoryAccess / ToolCall / TurnEnded / LlmCost).
    {
        let storage = open_store(&dir).await;
        let harness = build_memory_harness(Arc::clone(&storage)).await;

        let persistent_audit =
            PersistentAuditLog::open(Arc::clone(&storage), TEST_AUDIT_KEY)
                .await
                .expect("session A persistent audit must open");

        let provider = ScriptedProvider::new(memory_write_script(
            "notes",
            "tamper test body",
        ));

        let audit_typed = Arc::new(persistent_audit);
        let audit_hook: Arc<dyn AuditHook> = audit_typed.clone();

        let stdin_script = b"remember a thing\n";
        let reader = Cursor::new(&stdin_script[..]);
        let channel = LocalChannel::<Vec<u8>>::new("audit-e2e-tamper", Vec::new());

        let config = base_session_config(&harness, Arc::clone(&storage));

        run_session(
            Arc::clone(&provider) as Arc<dyn LlmProvider>,
            audit_hook,
            None,
            config,
            channel,
            reader,
        )
        .await
        .expect("tamper-test session A must complete cleanly");

        wait_for_audit_rows(&storage, 5).await;
        drop(harness);
        drop(audit_typed);
        drop(storage);
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
    }

    // Tamper step — reopen storage (NOT the audit log, so no drain
    // task is running), read `seq 0` through the `KeyDomain::Audit`
    // handle (which AEAD-decrypts it), decode the `SignedEntry`,
    // flip one byte of its `mac`, re-serialize, and put it back.
    // The AEAD handle will re-encrypt under the same per-row key,
    // so from the filesystem's perspective the ciphertext is
    // *valid* — only the HMAC-chain replay will notice.
    //
    // This is exactly the same tamper recipe as
    // `persistent::tests::chain_break_detected_after_byte_flip_on_reopen`,
    // promoted from a unit-test-private storage handle to the real
    // `RedbStorage::open` path.
    {
        let storage = open_store(&dir).await;
        let handle = storage.domain(KeyDomain::Audit);

        let seq0_key = audit_row_key(0);
        let seq0_value = handle
            .get(&seq0_key)
            .await
            .expect("scratch storage must answer get()")
            .expect("seq 0 row must exist on disk after session A");

        let mut entry: SignedEntry = serde_json::from_slice(&seq0_value)
            .expect("seq 0 row must decode as SignedEntry");
        assert_eq!(entry.seq, 0, "sanity: the row under seq0_key must be seq 0");

        // Flip the high bit of the first MAC byte. Any single-bit
        // perturbation is enough — HMAC-SHA256's MAC-recomputation
        // check compares the full tag.
        let original_byte = entry.mac[0];
        entry.mac[0] ^= 0xff;
        assert_ne!(
            entry.mac[0], original_byte,
            "MAC tamper must actually flip a bit"
        );

        let tampered_bytes = serde_json::to_vec(&entry).expect("re-serialize");
        handle
            .put(&seq0_key, &tampered_bytes)
            .await
            .expect("write-back of tampered row must succeed");

        drop(storage);
    }

    // The moment of truth — `verify_from_disk` runs the full HMAC
    // chain replay and must fail at seq 0, because seq 1's
    // `prev_mac` will no longer match the tampered seq 0's (new)
    // mac. Actually seq 0 itself is detected first: recomputing the
    // MAC over seq 0's canonical bytes with the chain key yields
    // the pre-tamper mac, not the stored-after-tamper one. Either
    // way the first break the walker sees is at seq 0.
    let storage = open_store(&dir).await;
    let err = PersistentAuditLog::verify_from_disk(Arc::clone(&storage), TEST_AUDIT_KEY)
        .await
        .expect_err("tampered chain must fail verification");

    match err {
        AuditError::ChainBroken { seq, reason } => {
            assert_eq!(
                seq, 0,
                "tamper on seq 0 must surface as ChainBroken {{ seq: 0 }}, got seq {seq} \
                 (reason: {reason})"
            );
        }
        other => panic!(
            "tampered chain must surface as AuditError::ChainBroken, got {other:?}"
        ),
    }

    // Also prove that a live `open` fails the same way — the two
    // share `scan_decode_verify` per Task 2, but making the
    // assertion explicit catches the hypothetical future regression
    // where a refactor splits them back apart and one path stops
    // verifying on reopen.
    let live_err = PersistentAuditLog::open(Arc::clone(&storage), TEST_AUDIT_KEY)
        .await
        .expect_err("tampered chain must also fail PersistentAuditLog::open");
    assert!(
        matches!(live_err, AuditError::ChainBroken { seq: 0, .. }),
        "live open must also report ChainBroken at seq 0, got {live_err:?}"
    );

    drop(storage);
}
