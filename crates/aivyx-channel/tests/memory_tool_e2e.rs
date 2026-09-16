//! Phase 6 task 5 — the memory tool round-trip integration test.
//!
//! Two scripted sessions against the **same** `$TMPDIR`-based store,
//! the same master key, and the same on-disk bytes:
//!
//! | Session | What runs                                                   |
//! |---------|-------------------------------------------------------------|
//! | A       | Planner calls `memory.write`, substrate persists one entry. |
//! | B       | Planner calls `memory.read`, substrate returns A's entry.   |
//!
//! Between the two sessions, the `Arc<dyn Storage>` from A is dropped,
//! the `Arc<dyn Memory>` from A is dropped, the `RedbStorage`'s
//! underlying redb `Database` is released, and a **fresh**
//! `RedbStorage::open` + `RedbMemory::open` pair is constructed over
//! the same path. If any part of the Phase 6 Task 2 persistence story
//! is wrong — the AEAD seal, the per-row nonce, the counter seed at
//! reopen, the domain-keyed prefix scan — session B will fail to see
//! session A's entry and this test will light up.
//!
//! This is the whole point of Phase 6: **memory is a tool the agent
//! chose to call, and that choice survives a process restart**. A
//! prompt hook silently dumping the last N turns into the system
//! message at turn start could also produce "the agent remembered
//! your favorite color," but it would not produce an audit chain
//! with `memory.write:topic:notes` and `memory.read:topic:notes`
//! entries — and it would not survive session B reopening the store
//! from cold bytes. This test is the smallest observable that tells
//! the two implementations apart, and D1's central commitment rests
//! on that distinction.
//!
//! ## Shape
//!
//! Same `ScriptedProvider` pattern as `cli_e2e.rs`, `fs_tool_e2e.rs`,
//! and `storage_persistence_e2e.rs`: each user turn is driven by a
//! fresh `ScriptedProvider` with two steps — step 1 terminates with
//! [`LlmStepEnd::ToolCall`] so the planner dispatches a memory tool
//! call, step 2 terminates with [`LlmStepEnd::FinalMessage`] after
//! the planner has observed the tool outcome and woven the
//! [`LlmMessage::ToolResult`] into history. The `ScriptedProvider`
//! is duplicated here rather than shared with the other test files
//! on the same "refactor isolation" principle those files note.
//!
//! ## What this test is *not*
//!
//! - **Not a live-LLM test.** No network I/O. The scripted provider
//!   produces exactly the two `LlmStepEnd`s each turn needs, same
//!   as every other integration test in this directory.
//! - **Not a behavioural test of `RedbMemory` internals.** Those
//!   live next to the impl in `aivyx-memory::redb::tests`. Here the
//!   substrate is a black box — we care that it persisted the write
//!   and served the read, not how it laid out keys.
//! - **Not a scope-denial test.** `fs_tool_e2e.rs` already carries
//!   the canonical denial-recovery assertion for the denial path,
//!   and the memory tools use exactly the same deny-scope +
//!   early-return plumbing via `agent.rs`. Covering it again here
//!   would re-assert the same planner/loop path with different tool
//!   names, which is noise per Phase 4's "don't write tests that
//!   re-run other tests with a cosmetic difference" discipline.

use std::io::Cursor;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde_json::{json, Value};

use aivyx_audit::{AuditBridge, AuditEvent, AuditLog, HmacChainLog, MemoryOperation};
use aivyx_capability::{CapabilitySet, Scope};
use aivyx_channel::{run_session, LocalChannel, SessionConfig};
use aivyx_core::{
    AuditHook, CancellationToken, Tool, ToolOutcomeSummary, ToolRegistry, TurnOutcome,
    TurnOutcomeSummary,
};
use aivyx_crypto::MasterKey;
use aivyx_llm::{
    LlmError, LlmMessage, LlmProvider, LlmRequest, LlmStepEnd, LlmStream, LlmStreamEvent,
    LlmUsage, ToolCallEnd,
};
use aivyx_memory::{Memory, MemoryForgetTool, MemoryReadTool, MemoryWriteTool, RedbMemory};
use aivyx_storage::{KeyDomain, RedbStorage, Storage, StorageConfig};

// ---------------------------------------------------------------------------
// Scratch dir shared by both sessions. RAII cleanup on drop: whichever
// session panics last, the parent directory comes down with it. Same
// shape as `SharedStoreDir` in `storage_persistence_e2e.rs` — kept
// local so a refactor in that file doesn't ripple.
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
        // `pid-nanos` alone collides when two tests in this binary call
        // `new()` within the same coarse-clock tick under a loaded parallel
        // run, and the second `RedbStorage::open` fails on the redb lock; a
        // uuid makes the path unconditionally unique (see storage_persistence_e2e).
        let uniq = uuid::Uuid::new_v4();
        let parent =
            PathBuf::from(tmp).join(format!("aivyx-memory-e2e-{pid}-{nanos}-{uniq}"));
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

// The raw 32-byte master used for both session A and session B. Same
// byte for the same reason `storage_persistence_e2e.rs` does it: we
// deliberately bypass Argon2id here because `passphrase::tests` already
// covers the passphrase→key path, and combining them would burn
// ~500ms × two sessions on a test whose point is the redb round-trip.
const TEST_MASTER: [u8; 32] = [7u8; 32];

async fn open_store(dir: &SharedStoreDir) -> Arc<dyn Storage> {
    RedbStorage::open(
        StorageConfig::new(dir.store.clone()),
        MasterKey::from_raw(TEST_MASTER),
    )
    .await
    .expect("scratch storage must open")
}

// ---------------------------------------------------------------------------
// Scripted provider — same shape as the other integration tests. Each
// turn is exactly two `ScriptedStep`s: one `ToolCall` terminal and one
// `FinalMessage` terminal, run back-to-back by the planner.
// ---------------------------------------------------------------------------

struct ScriptedStep {
    events: Vec<LlmStreamEvent>,
    terminal: LlmStepEnd,
}

struct ScriptedProvider {
    queue: Mutex<std::collections::VecDeque<ScriptedStep>>,
    /// Snapshot of the most recent `LlmRequest.messages` the planner
    /// handed to `chat_stream`. The read-side assertion at the end
    /// of session B uses this to prove the planner wove the tool
    /// result into its history before the second step — i.e., the
    /// substrate's bytes made it through `MemoryReadTool::execute`,
    /// through `render_tool_result`, through
    /// `LlmPlanner::observe_tool_outcome`, and out the other side
    /// as an `LlmMessage::ToolResult`.
    last_messages: Mutex<Vec<LlmMessage>>,
}

impl ScriptedProvider {
    fn new(steps: Vec<ScriptedStep>) -> Arc<Self> {
        Arc::new(ScriptedProvider {
            queue: Mutex::new(steps.into()),
            last_messages: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl LlmProvider for ScriptedProvider {
    async fn chat_stream(
        &self,
        request: LlmRequest<'_>,
        _cancellation: &CancellationToken,
    ) -> Result<Box<dyn LlmStream>, LlmError> {
        *self.last_messages.lock().unwrap() = request.messages.to_vec();
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

// ---------------------------------------------------------------------------
// Registry builder. Produces a fresh `ToolRegistry` containing exactly
// the three memory tools wrapping a `RedbMemory::open` over the given
// storage handle. Shared by both sessions so they go through the same
// wiring — the only thing that differs between them is the
// `Arc<dyn Storage>` handle they're anchored on (one per session, same
// path underneath).
// ---------------------------------------------------------------------------

struct MemoryHarness {
    tools: Arc<ToolRegistry>,
    capabilities: CapabilitySet,
    /// Retained so assertions can reach into the substrate *outside*
    /// the tool path — useful for the "second session's counter seeds
    /// off the first session's entries" assertion below.
    memory: Arc<dyn Memory>,
}

async fn build_memory_harness(storage: Arc<dyn Storage>) -> MemoryHarness {
    // Open a fresh RedbMemory against the given storage. `open`
    // scans entries under `KeyDomain::Memory` to seed the monotonic
    // seq counter, so session B's handle will start its counter
    // strictly greater than session A's highest entry — the exact
    // PHASE_6.md task 2 invariant the reopen test is trying to
    // exercise at the integration level.
    let memory: Arc<dyn Memory> = RedbMemory::open(Arc::clone(&storage))
        .await
        .expect("RedbMemory::open must succeed over live scratch storage");

    let memory_read = MemoryReadTool::new(Arc::clone(&memory));
    let memory_write = MemoryWriteTool::new(Arc::clone(&memory));
    let memory_forget = MemoryForgetTool::new(Arc::clone(&memory));

    let tools: Arc<ToolRegistry> = Arc::new(ToolRegistry::new(vec![
        Arc::new(memory_read) as Arc<dyn Tool>,
        Arc::new(memory_write) as Arc<dyn Tool>,
        Arc::new(memory_forget) as Arc<dyn Tool>,
    ]));

    // Broad unqualified grants. D4 Rule 2: an unqualified held scope
    // grants any qualified needed scope with the same base, which
    // covers the `memory.<op>:topic:<topic>` scopes each tool
    // derives from its input. Same policy the CLI binary uses for
    // the Trusted local channel — see `bin/aivyx.rs`'s capability
    // block for the canonical version.
    let capabilities = CapabilitySet::from_scopes([
        Scope::parse("memory.read").unwrap(),
        Scope::parse("memory.write").unwrap(),
        Scope::parse("memory.forget").unwrap(),
    ]);

    MemoryHarness {
        tools,
        capabilities,
        memory,
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
// The test.
//
// Single function on purpose: the two sessions have to run
// back-to-back against the same `SharedStoreDir` for the reopen
// assertion to mean anything, and splitting them into separate
// `#[tokio::test]`s would force shared global state. Sequential
// in-function is the straightforward shape.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn memory_survives_a_clean_close_and_second_session_recalls_it() {
    let dir = SharedStoreDir::new();

    // ====== Session A — write and persist ============================
    //
    // Planner is a single-turn script: step 1 calls memory.write,
    // step 2 emits "stored it" as the final message. Audit log is a
    // fresh HmacChainLog with a deterministic key so the test can
    // assert on exact entry contents.

    let session_a_report;
    let session_a_entries;
    {
        let storage = open_store(&dir).await;
        let harness = build_memory_harness(Arc::clone(&storage)).await;

        let provider = ScriptedProvider::new(vec![
            ScriptedStep {
                events: vec![],
                terminal: LlmStepEnd::ToolCalls {
                    calls: vec![ToolCallEnd {
                        call_id: "toolu_write_01".to_string(),
                        tool_name: "memory.write".to_string(),
                        input: json!({
                            "topic": "notes",
                            "body": "the user's favorite color is purple",
                        }),
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
        ]);

        let audit_log = HmacChainLog::new([7u8; 32].to_vec());
        let audit_bridge = Arc::new(AuditBridge::new(audit_log));
        let audit_hook: Arc<dyn AuditHook> = audit_bridge.clone();

        let stdin_script = b"remember my favorite color\n";
        let reader = Cursor::new(&stdin_script[..]);
        let channel = LocalChannel::<Vec<u8>>::new("memory-e2e-a", Vec::new());

        let config = base_session_config(&harness, Arc::clone(&storage));

        session_a_report = run_session(
            Arc::clone(&provider) as Arc<dyn LlmProvider>,
            audit_hook,
            None,
            config,
            channel,
            reader,
        )
        .await
        .expect("session A must complete cleanly");

        // Pull the audit entries out before the bridge is dropped.
        let log = audit_bridge.writer();
        log.verify().expect("session A audit chain must verify");
        session_a_entries = log
            .entries()
            .expect("can read session A entries")
            .into_iter()
            .map(|e| e.event)
            .collect::<Vec<AuditEvent>>();

        // ---- Also sanity-check that the substrate itself reports
        //      the write went in, via the `memory` handle the harness
        //      kept around. This is the "talk to the substrate
        //      directly, bypassing the tool surface" probe: if the
        //      tool thought it wrote something but the substrate
        //      disagrees, the bug is at the tool/substrate seam and
        //      this assertion localizes it.
        let entries = harness
            .memory
            .get_recent("notes", 10)
            .await
            .expect("memory.get_recent on live session A");
        assert_eq!(
            entries.len(),
            1,
            "session A's memory handle must see one entry under 'notes'"
        );
        assert_eq!(entries[0].body, "the user's favorite color is purple");
        assert_eq!(
            entries[0].seq, 0,
            "first-ever write must land at seq 0 — Task 1 invariant"
        );

        // Explicitly drop the `Arc<dyn Storage>` + harness + memory
        // handle at block exit. Everything goes out of scope
        // simultaneously; the underlying `redb::Database` closes when
        // the last `Arc<DomainHandle>` drops.
    }

    // ---- Session A assertions ---------------------------------------

    assert_eq!(session_a_report.turns_run, 1);
    match session_a_report.last_outcome {
        Some(TurnOutcome::Completed {
            ref final_message,
            tool_calls_made,
            ..
        }) => {
            assert_eq!(final_message, "stored it");
            assert_eq!(tool_calls_made, 1);
        }
        other => panic!("session A: expected Completed(1 tool call), got {other:?}"),
    }

    // Audit shape: TurnStarted / MemoryAccess(Write) / ToolCall(Completed,
    // memory.write) / TurnEnded / LlmCost. The MemoryAccess tag comes from
    // MemoryWriteTool itself — Phase 6 D1: memory ops carry their own
    // semantic audit tag on top of the generic ToolCall the session
    // loop records. The trailing LlmCost is Chapter K's per-turn priced
    // cost event (the LLM-backed planner reports a model).
    assert_eq!(
        session_a_entries.len(),
        5,
        "session A audit: expected 5 entries, got {}: {:#?}",
        session_a_entries.len(),
        session_a_entries
    );
    assert!(matches!(
        &session_a_entries[0],
        AuditEvent::TurnStarted { .. }
    ));
    match &session_a_entries[1] {
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
        other => panic!("session A: expected MemoryAccess at index 1, got {other:?}"),
    }
    match &session_a_entries[2] {
        AuditEvent::ToolCall {
            outcome,
            scope_used,
            ..
        } => {
            assert!(
                matches!(outcome, ToolOutcomeSummary::Completed { .. }),
                "session A ToolCall must be Completed, got {outcome:?}"
            );
            // The scope the audit records is the one the tool derived
            // from its input: `memory.write:topic:notes`. This is the
            // Phase 6 task 3 R1 payoff — the audit chain carries the
            // *narrowed* scope, not the broad held capability.
            assert_eq!(scope_used.base(), "memory.write");
            assert_eq!(scope_used.qualifier(), Some("topic:notes"));
        }
        other => panic!("session A: expected ToolCall at index 2, got {other:?}"),
    }
    match &session_a_entries[3] {
        AuditEvent::TurnEnded {
            outcome,
            tool_calls_made,
            ..
        } => {
            assert_eq!(*outcome, TurnOutcomeSummary::Completed);
            assert_eq!(*tool_calls_made, 1);
        }
        other => panic!("session A: expected TurnEnded at index 3, got {other:?}"),
    }
    assert!(
        matches!(&session_a_entries[4], AuditEvent::LlmCost { .. }),
        "session A: expected LlmCost at index 4, got {:?}",
        session_a_entries[4]
    );

    // ====== Between sessions — raw-bytes persistence probe ===========
    //
    // Reopen the store in isolation (not through run_session) and
    // confirm there is in fact ciphertext under KeyDomain::Memory. If
    // session A's write never reached disk, session B would fail for
    // the same reason but the diagnosis would be harder. Catching it
    // here isolates "storage didn't commit" from "memory tool didn't
    // read it back."
    {
        let probe_storage = open_store(&dir).await;
        let probe_memory = RedbMemory::open(Arc::clone(&probe_storage))
            .await
            .expect("probe RedbMemory::open must succeed");
        let probe_entries = probe_memory
            .get_recent("notes", 10)
            .await
            .expect("probe memory.get_recent against reopened store");
        assert_eq!(
            probe_entries.len(),
            1,
            "between-session probe must see session A's entry on disk"
        );
        assert_eq!(probe_entries[0].body, "the user's favorite color is purple");
        assert_eq!(probe_entries[0].seq, 0);
        // probe drops at block exit, again releasing redb.
    }

    // ====== Session B — read and recall ==============================
    //
    // Fresh run_session on the same SharedStoreDir with the same
    // master key. The planner's script is: step 1 calls memory.read
    // with the same topic session A wrote under; step 2 observes the
    // tool result in history and produces a final message that
    // echoes the remembered body. The critical assertion is that the
    // tool result surfaced in step 2's history — *that* is what
    // proves the recall crossed the process boundary.

    let session_b_report;
    let session_b_entries;
    let session_b_last_messages;
    {
        let storage = open_store(&dir).await;
        let harness = build_memory_harness(Arc::clone(&storage)).await;

        // The harness above opens a fresh `RedbMemory` which calls
        // `seed_counter_from_storage` on the reopened domain. Probe
        // that seed value directly: a follow-on write should get
        // `seq = 1`, not `seq = 0`. This is the Task 2 crash-
        // recovery invariant surfaced at the integration level.
        // We do it *before* session B runs so the probe write is
        // idempotent relative to the recall assertions below — and
        // then we forget the probe topic so it can't leak into the
        // recall path.
        let probe_seq = harness
            .memory
            .put("seq-probe", "x")
            .await
            .expect("probe put must succeed");
        assert_eq!(
            probe_seq, 1,
            "reopened RedbMemory must seed its counter strictly greater than \
             the max on-disk seq: expected 1 (since session A used seq 0), got {probe_seq}"
        );
        harness
            .memory
            .forget("seq-probe")
            .await
            .expect("probe forget must succeed");

        let provider = ScriptedProvider::new(vec![
            ScriptedStep {
                events: vec![],
                terminal: LlmStepEnd::ToolCalls {
                    calls: vec![ToolCallEnd {
                        call_id: "toolu_read_01".to_string(),
                        tool_name: "memory.read".to_string(),
                        input: json!({ "topic": "notes" }),
                        name_resolution: aivyx_llm::NameResolution::Known,
                    }],
                    text_so_far: String::new(),
                    usage: zero_usage(),
                },
            },
            ScriptedStep {
                events: vec![LlmStreamEvent::TextChunk(
                    "your favorite color is purple".into(),
                )],
                terminal: LlmStepEnd::FinalMessage {
                    text: "your favorite color is purple".to_string(),
                    usage: zero_usage(),
                },
            },
        ]);

        let audit_log = HmacChainLog::new([11u8; 32].to_vec());
        let audit_bridge = Arc::new(AuditBridge::new(audit_log));
        let audit_hook: Arc<dyn AuditHook> = audit_bridge.clone();

        let stdin_script = b"what is my favorite color?\n";
        let reader = Cursor::new(&stdin_script[..]);
        let channel = LocalChannel::<Vec<u8>>::new("memory-e2e-b", Vec::new());

        let config = base_session_config(&harness, Arc::clone(&storage));

        session_b_report = run_session(
            Arc::clone(&provider) as Arc<dyn LlmProvider>,
            audit_hook,
            None,
            config,
            channel,
            reader,
        )
        .await
        .expect("session B must complete cleanly");

        let log = audit_bridge.writer();
        log.verify().expect("session B audit chain must verify");
        session_b_entries = log
            .entries()
            .expect("can read session B entries")
            .into_iter()
            .map(|e| e.event)
            .collect::<Vec<AuditEvent>>();

        session_b_last_messages = provider.last_messages.lock().unwrap().clone();
    }

    // ---- Session B assertions ---------------------------------------

    assert_eq!(session_b_report.turns_run, 1);
    match session_b_report.last_outcome {
        Some(TurnOutcome::Completed {
            ref final_message,
            tool_calls_made,
            ..
        }) => {
            assert_eq!(final_message, "your favorite color is purple");
            assert_eq!(tool_calls_made, 1);
        }
        other => panic!("session B: expected Completed(1 tool call), got {other:?}"),
    }

    // Audit shape same as session A: TurnStarted / MemoryAccess(Read) /
    // ToolCall(Completed, memory.read) / TurnEnded / LlmCost.
    assert_eq!(
        session_b_entries.len(),
        5,
        "session B audit: expected 5 entries, got {}: {:#?}",
        session_b_entries.len(),
        session_b_entries
    );
    assert!(
        matches!(&session_b_entries[4], AuditEvent::LlmCost { .. }),
        "session B: expected LlmCost at index 4, got {:?}",
        session_b_entries[4]
    );
    match &session_b_entries[1] {
        AuditEvent::MemoryAccess {
            operation,
            scope,
            query_or_key,
            ..
        } => {
            assert_eq!(*operation, MemoryOperation::Read);
            assert_eq!(scope.base(), "memory.read");
            assert_eq!(scope.qualifier(), Some("topic:notes"));
            assert_eq!(query_or_key, "notes");
        }
        other => panic!("session B: expected MemoryAccess at index 1, got {other:?}"),
    }
    match &session_b_entries[2] {
        AuditEvent::ToolCall {
            outcome,
            scope_used,
            ..
        } => {
            assert!(
                matches!(outcome, ToolOutcomeSummary::Completed { .. }),
                "session B ToolCall must be Completed, got {outcome:?}"
            );
            assert_eq!(scope_used.base(), "memory.read");
            assert_eq!(scope_used.qualifier(), Some("topic:notes"));
        }
        other => panic!("session B: expected ToolCall at index 2, got {other:?}"),
    }

    // ---- The load-bearing assertion: the planner's step-2 history
    //      carries a ToolResult for `toolu_read_01` whose content
    //      includes session A's body. This is the proof the recall
    //      actually crossed the process boundary: the substrate
    //      decrypted the on-disk row, `MemoryReadTool::execute`
    //      turned it into JSON, `render_tool_result` packed it into
    //      the planner's next step, and the body landed in what the
    //      LLM would see as a real tool result. If any of those
    //      layers lies, this assertion fails.
    let read_result = session_b_last_messages.iter().find_map(|m| match m {
        LlmMessage::ToolResult {
            call_id,
            content,
            is_error,
        } if call_id == "toolu_read_01" => Some((content.clone(), *is_error)),
        _ => None,
    });
    let (content, is_error) = read_result
        .expect("session B step 2 must see a ToolResult for the memory.read call");
    assert!(
        !is_error,
        "memory.read tool result must not be flagged as an error"
    );
    let parsed: Value =
        serde_json::from_str(&content).expect("memory.read tool result must be JSON");
    // The tool's output shape from Task 3 is {"topic", "entries", "count"}.
    assert_eq!(parsed["topic"], "notes");
    assert_eq!(parsed["count"], 1);
    let entries = parsed["entries"]
        .as_array()
        .expect("entries must be a JSON array");
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0]["body"], "the user's favorite color is purple",
        "session B must recall the exact body session A wrote"
    );
    assert_eq!(
        entries[0]["seq"], 0,
        "recalled entry must preserve session A's seq"
    );
}

// ---------------------------------------------------------------------------
// Bonus assertion in a second test function: `memory.forget` through
// the tool surface actually drops the row from disk, so a third hypo-
// thetical session would see an empty topic. This is the smallest
// forget-round-trip proof that fits the "tool → substrate → disk"
// shape the other assertions already built up, and pairs the
// read/write persistence case with a forget persistence case so the
// integration story covers the full D4 memory.* family.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn memory_forget_persists_across_reopen() {
    let dir = SharedStoreDir::new();

    // ---- Pre-session — seed two entries via the substrate directly.
    //      We skip the planner here because the scripted provider
    //      dance is orthogonal to what this test is proving: the
    //      load-bearing assertion is the *forget-then-reopen* shape,
    //      not the tool-call dispatch path (which the round-trip
    //      test above already covers).
    {
        let storage = open_store(&dir).await;
        let memory = RedbMemory::open(Arc::clone(&storage))
            .await
            .expect("seed memory open");
        memory.put("notes", "entry 1").await.expect("seed put 1");
        memory.put("notes", "entry 2").await.expect("seed put 2");
        // Bytes are on disk after this block's Drop releases redb.
    }

    // ---- Session — one scripted turn calling memory.forget.
    {
        let storage = open_store(&dir).await;
        let harness = build_memory_harness(Arc::clone(&storage)).await;

        let provider = ScriptedProvider::new(vec![
            ScriptedStep {
                events: vec![],
                terminal: LlmStepEnd::ToolCalls {
                    calls: vec![ToolCallEnd {
                        call_id: "toolu_forget_01".to_string(),
                        tool_name: "memory.forget".to_string(),
                        input: json!({ "topic": "notes" }),
                        name_resolution: aivyx_llm::NameResolution::Known,
                    }],
                    text_so_far: String::new(),
                    usage: zero_usage(),
                },
            },
            ScriptedStep {
                events: vec![LlmStreamEvent::TextChunk("forgotten".into())],
                terminal: LlmStepEnd::FinalMessage {
                    text: "forgotten".to_string(),
                    usage: zero_usage(),
                },
            },
        ]);

        let audit_log = HmacChainLog::new([17u8; 32].to_vec());
        let audit_bridge = Arc::new(AuditBridge::new(audit_log));
        let audit_hook: Arc<dyn AuditHook> = audit_bridge.clone();

        let stdin_script = b"forget my notes\n";
        let reader = Cursor::new(&stdin_script[..]);
        let channel = LocalChannel::<Vec<u8>>::new("memory-e2e-forget", Vec::new());
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
        .expect("forget session must complete cleanly");

        assert_eq!(report.turns_run, 1);
        assert!(matches!(
            report.last_outcome,
            Some(TurnOutcome::Completed { .. })
        ));
    }

    // ---- Post-session — reopen and confirm the topic is gone from disk.
    {
        let storage = open_store(&dir).await;
        let memory = RedbMemory::open(Arc::clone(&storage))
            .await
            .expect("post-forget memory open");
        let entries = memory
            .get_recent("notes", 10)
            .await
            .expect("post-forget get_recent");
        assert!(
            entries.is_empty(),
            "memory.forget must survive reopen — topic 'notes' still had entries: {entries:?}"
        );
    }

    // Sanity-check the KeyDomain::Memory prefix at the raw-storage
    // level: after forget, a scan of the entry prefix should return
    // zero rows. This is the "did the bytes actually leave disk"
    // probe — a forget that just unlinked an in-memory index but
    // left ciphertext behind would fail this assertion.
    //
    // Opening the domain directly is fine here; the test is
    // deliberately reaching under the Memory tool surface to check
    // the substrate's own promise.
    {
        let storage = open_store(&dir).await;
        let domain = storage.domain(KeyDomain::Memory);
        // The RedbMemory entry key layout (`e\x00<topic>\x00<seq_be>`)
        // is an internal detail of aivyx-memory, but we can still
        // assert at a weaker granularity: a scan under the broad
        // entry discriminator `b"e\x00"` should be empty, because no
        // other topic survives. This is the contract the forget
        // tool's integration story owes the persistence layer.
        let rows = domain
            .scan_prefix(b"e\x00")
            .await
            .expect("raw scan must succeed");
        assert!(
            rows.is_empty(),
            "raw KeyDomain::Memory entry prefix must be empty after forget: {} rows left",
            rows.len()
        );
    }
}
