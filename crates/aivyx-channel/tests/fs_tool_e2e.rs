//! Phase 4 task 5 — LLM-driven filesystem tool round-trip.
//!
//! This test is the whole point of Phase 4: it proves that when a
//! (simulated) LLM asks to call `fs.read` with a concrete path, the
//! request flows through the full stack —
//!
//! ```text
//! ScriptedProvider → LlmPlanner → ConcreteAgent::turn
//!   → ToolRegistry::get
//!     → scope gate at agent.rs (grants `fs.read:<sandbox>/**`)
//!       → FsReadTool::execute (real std::fs::read)
//!         → ToolOutcome::Completed
//!   → LlmPlanner::observe_tool_outcome
//!     → next chat_stream step (with tool_result in history)
//!       → LlmStepEnd::FinalMessage
//!         → TurnOutcome::Completed
//! ```
//!
//! …and the audit chain comes out of the other end with a
//! `ToolCall` entry carrying the `Completed` outcome between the
//! `TurnStarted` / `TurnEnded` pair.
//!
//! The same `ScriptedProvider` shape as [`cli_e2e.rs`] is used
//! (one `chat_stream` call per `ScriptedStep`), but here each user
//! turn is **two scripted steps**: step 1 terminates with
//! [`LlmStepEnd::ToolCall`], step 2 terminates with
//! [`LlmStepEnd::FinalMessage`]. Between the two, the planner
//! dispatches the tool call, the agent scope-checks and executes,
//! and the tool result is appended to the planner's history so the
//! second `chat_stream` sees it as an [`LlmMessage::ToolResult`].
//!
//! ## What this test is *not*
//!
//! - **Not a replacement for `cli_e2e.rs`.** That test is the
//!   chat-only regression for the Phase 3 session loop; this test
//!   is the Phase 4 tool-execution regression. They exercise
//!   disjoint code paths on top of the same `run_session`.
//! - **Not a behavioural test of `FsReadTool` internals.** Those
//!   live next to the tool impl in `aivyx-core::tools::fs::tests`.
//!   Here the tool is a black box — we care that it ran, that it
//!   saw the right sandbox, and that its output round-tripped.
//! - **Not a live-API test.** Everything is scripted. The
//!   `#[ignore]`-gated live test lives elsewhere.

use std::io::{Cursor, Write as _};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde_json::{json, Value};

use aivyx_audit::{AuditBridge, AuditEvent, AuditLog, HmacChainLog};
use aivyx_capability::{CapabilitySet, Scope};
use aivyx_channel::{run_session, LocalChannel, SessionConfig};
use aivyx_core::{
    tools::{FsReadToolConfig, FsWriteToolConfig},
    AuditHook, CancellationToken, Tool, ToolOutcomeSummary, ToolRegistry, TurnOutcome,
    TurnOutcomeSummary,
};
use aivyx_crypto::MasterKey;
use aivyx_llm::{
    LlmError, LlmMessage, LlmProvider, LlmRequest, LlmStepEnd, LlmStream, LlmStreamEvent,
    LlmUsage, ToolCallEnd,
};
use aivyx_storage::{RedbStorage, Storage, StorageConfig};

// ---------------------------------------------------------------------------
// Scripted provider. Identical in shape to the one in cli_e2e.rs, but
// kept local to this test file so the two integration tests stay
// independent — a refactor in one should never force a rebuild of
// the other's fixtures.
// ---------------------------------------------------------------------------

struct ScriptedStep {
    events: Vec<LlmStreamEvent>,
    terminal: LlmStepEnd,
}

struct ScriptedProvider {
    queue: Mutex<std::collections::VecDeque<ScriptedStep>>,
    /// Snapshot of the last `LlmRequest.messages` we were handed. Used
    /// by the assertion at the end of the test to prove the planner
    /// correctly wove a `ToolResult` into the history before the
    /// second `chat_stream` call.
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
// Sandbox — a short-lived temp directory seeded with one file the
// scripted tool call reads. No `tempfile` dep: a process-unique
// subdirectory under `$TMPDIR` is enough for a single integration
// test, and the `Drop` impl cleans up on any exit path.
// ---------------------------------------------------------------------------

struct TestSandbox {
    root: PathBuf,
    parent: PathBuf,
}

impl TestSandbox {
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
        // run — the second `RedbStorage::open` then fails on the redb lock
        // ("scratch storage must open" panic, the flake that failed the
        // v0.7.4 release). A uuid makes the sandbox path unconditionally
        // unique (matches the SharedStoreDir fix in storage_persistence_e2e).
        let uniq = uuid::Uuid::new_v4();
        let parent =
            PathBuf::from(tmp).join(format!("aivyx-fs-e2e-{pid}-{nanos}-{uniq}"));
        let root = parent.join("root");
        std::fs::create_dir_all(&root).expect("test sandbox root must be creatable");
        TestSandbox { root, parent }
    }

    fn root(&self) -> &std::path::Path {
        &self.root
    }

    /// Phase 5 task 4: give each sandbox a sibling `store.redb` path
    /// so the scratch `RedbStorage` lives inside the same `parent`
    /// tempdir and gets cleaned up by the same `Drop` impl.
    fn store_path(&self) -> PathBuf {
        self.parent.join("store.redb")
    }

    fn write(&self, rel: &str, contents: &[u8]) {
        let path = self.root.join(rel);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("mkdir -p parent");
        }
        let mut f = std::fs::File::create(&path).expect("create test file");
        f.write_all(contents).expect("write test file");
    }
}

impl Drop for TestSandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.parent);
    }
}

// ---------------------------------------------------------------------------
// Build a full SessionConfig + registry + audit stack. Shared by the
// two tests below so the fixture wiring stays in one place.
// ---------------------------------------------------------------------------

struct Harness {
    /// The canonicalized sandbox path the scope capabilities were
    /// anchored on — also the sandbox the tools were built with.
    /// The tests use this to construct input paths.
    canonical_root: PathBuf,
    tools: Arc<ToolRegistry>,
    capabilities: CapabilitySet,
}

fn build_harness(sandbox: &TestSandbox) -> Harness {
    // Build both tools at the raw sandbox root; their `build()` calls
    // canonicalize internally. We then pull the canonical root back
    // out of the read tool and anchor the capability scopes on it so
    // a symlinked `$TMPDIR` can't silently widen or shift the grant.
    let fs_read = FsReadToolConfig::new(sandbox.root().to_path_buf())
        .build()
        .expect("fs.read tool must build against a live sandbox");
    let fs_write = FsWriteToolConfig::new(sandbox.root().to_path_buf())
        .build()
        .expect("fs.write tool must build against a live sandbox");

    let canonical_root = fs_read.sandbox_root().to_path_buf();
    let root_display = canonical_root.display();
    let fs_read_scope = Scope::parse(&format!("fs.read:{root_display}/**"))
        .expect("canonical fs.read scope must parse");
    let fs_write_scope = Scope::parse(&format!("fs.write:{root_display}/**"))
        .expect("canonical fs.write scope must parse");

    let tools: Arc<ToolRegistry> = Arc::new(ToolRegistry::new(vec![
        Arc::new(fs_read) as Arc<dyn Tool>,
        Arc::new(fs_write) as Arc<dyn Tool>,
    ]));

    let capabilities = CapabilitySet::from_scopes([fs_read_scope, fs_write_scope]);

    Harness {
        canonical_root,
        tools,
        capabilities,
    }
}

/// Open a throwaway `RedbStorage` at the sandbox's `store.redb` path.
///
/// Phase 5 task 4 made `SessionConfig.storage` a required field, so
/// every integration test that builds a config via `base_session_config`
/// now needs a real `Arc<dyn Storage>` threaded in alongside. The
/// deterministic `[7u8; 32]` master key matches the one the chat-only
/// cli_e2e test uses so future debugging can cross-reference them.
async fn open_scratch_storage(sandbox: &TestSandbox) -> Arc<dyn Storage> {
    let master = MasterKey::from_raw([7u8; 32]);
    RedbStorage::open(StorageConfig::new(sandbox.store_path()), master)
        .await
        .expect("scratch storage must open")
}

fn base_session_config(harness: &Harness, storage: Arc<dyn Storage>) -> SessionConfig {
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
// Test 1 — happy path. An LLM asks for fs.read on a sandboxed file,
// the tool runs, the second LLM step echoes a summary as the final
// message, and the audit chain bears witness to all of it.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scripted_fs_read_tool_call_round_trips_through_full_stack() {
    let sandbox = TestSandbox::new();
    sandbox.write("notes/today.md", b"buy milk\n");

    let harness = build_harness(&sandbox);

    // The LLM will ask for an absolute canonical path — the same
    // path a real provider would receive back from Anthropic after
    // an LLM decided to call the tool. Constructing it ourselves
    // here keeps the test independent of any path-rewriting that
    // might land later.
    let target_path = harness.canonical_root.join("notes/today.md");
    let target_path_str = target_path.to_str().unwrap().to_string();

    let provider = ScriptedProvider::new(vec![
        // Step 1: LLM calls fs.read. The planner dispatches the call.
        ScriptedStep {
            events: vec![],
            terminal: LlmStepEnd::ToolCalls {
                calls: vec![ToolCallEnd {
                    call_id: "toolu_read_01".to_string(),
                    tool_name: "fs.read".to_string(),
                    input: json!({ "path": target_path_str }),
                    name_resolution: aivyx_llm::NameResolution::Known,
                }],
                text_so_far: String::new(),
                usage: zero_usage(),
            },
        },
        // Step 2: LLM sees the tool result in its history and replies
        // with a final message. The streamed chunk is a single piece
        // of text that names the file contents — easy to assert on.
        ScriptedStep {
            events: vec![LlmStreamEvent::TextChunk("the note says: buy milk".into())],
            terminal: LlmStepEnd::FinalMessage {
                text: "the note says: buy milk".to_string(),
                usage: zero_usage(),
            },
        },
    ]);

    let audit_log = HmacChainLog::new([7u8; 32].to_vec());
    let audit_bridge = Arc::new(AuditBridge::new(audit_log));
    let audit_hook: Arc<dyn AuditHook> = audit_bridge.clone();

    let stdin_script = b"please read my note\n";
    let reader = Cursor::new(&stdin_script[..]);
    let channel = LocalChannel::<Vec<u8>>::new("fs-e2e", Vec::new());
    let sink = channel.writer_handle();

    let storage = open_scratch_storage(&sandbox).await;
    let config = base_session_config(&harness, storage);

    let report = run_session(
        Arc::clone(&provider) as Arc<dyn LlmProvider>,
        audit_hook,
        None,
        config,
        channel,
        reader,
    )
    .await
    .expect("run_session must complete cleanly");

    // ---- Assertion 1: turn completed with exactly one tool call.
    assert_eq!(report.turns_run, 1);
    match report.last_outcome {
        Some(TurnOutcome::Completed {
            ref final_message,
            tool_calls_made,
            ..
        }) => {
            assert_eq!(final_message, "the note says: buy milk");
            assert_eq!(tool_calls_made, 1);
        }
        other => panic!("expected Completed(1 tool call), got {other:?}"),
    }

    // ---- Assertion 2: the final message reached the user.
    let output = String::from_utf8(sink.lock().unwrap().clone()).expect("utf-8 output");
    assert!(
        output.contains("the note says: buy milk"),
        "final message chunk must be streamed to the channel: {output:?}"
    );
    assert!(
        output.contains("[turn completed]"),
        "finalize marker must render after the turn: {output:?}"
    );

    // ---- Assertion 3: the planner fed a ToolResult into step 2's
    //      history. This proves the tool-result weaving path in
    //      `LlmPlanner::observe_tool_outcome` actually ran, so an
    //      eventual regression there would surface here and not
    //      only in the core crate's unit tests.
    let last_messages = provider.last_messages.lock().unwrap().clone();
    let saw_tool_result = last_messages.iter().any(|m| {
        matches!(
            m,
            LlmMessage::ToolResult {
                call_id,
                is_error,
                ..
            } if call_id == "toolu_read_01" && !is_error
        )
    });
    assert!(
        saw_tool_result,
        "step 2 must have seen the tool result in history: {last_messages:?}"
    );

    // ---- Assertion 4: the audit chain recorded the whole shape —
    //      TurnStarted → ToolCall(Completed) → TurnEnded(Completed) —
    //      and verifies cleanly.
    let log = audit_bridge.writer();
    log.verify().expect("audit chain must verify");

    let entries = log.entries().expect("can read entries");
    let events: Vec<&AuditEvent> = entries.iter().map(|e| &e.event).collect();

    // Exactly four entries: TurnStarted, ToolCall, TurnEnded, and
    // (Chapter K) LlmCost — the LLM-backed planner reports a model so
    // the turn epilogue prices the turn.
    assert_eq!(
        entries.len(),
        4,
        "expected TurnStarted + ToolCall + TurnEnded + LlmCost, got {} entries: {:#?}",
        entries.len(),
        events
    );

    assert!(matches!(events[0], AuditEvent::TurnStarted { .. }));
    match events[1] {
        AuditEvent::ToolCall {
            outcome,
            scope_used,
            ..
        } => {
            // `ToolOutcomeSummary::Completed` is a struct variant with
            // a `verified` field — match on it rather than equating so
            // future additions to that variant don't force this test
            // to be rewritten.
            assert!(
                matches!(outcome, ToolOutcomeSummary::Completed { .. }),
                "expected Completed outcome in audit, got {outcome:?}"
            );
            // The scope stored in the audit entry is the one the tool
            // asked for (the R1-derived narrow scope), not the broad
            // capability the agent held. For fs.read this is a
            // path-qualified scope pointing at the file the tool was
            // asked to read.
            assert_eq!(scope_used.base(), "fs.read");
            assert!(
                scope_used
                    .qualifier()
                    .map(|q| q.contains("notes/today.md"))
                    .unwrap_or(false),
                "audit scope must carry the requested path, got {scope_used:?}"
            );
        }
        other => panic!("expected AuditEvent::ToolCall at index 1, got {other:?}"),
    }
    match events[2] {
        AuditEvent::TurnEnded {
            outcome,
            tool_calls_made,
            ..
        } => {
            assert_eq!(*outcome, TurnOutcomeSummary::Completed);
            assert_eq!(*tool_calls_made, 1);
        }
        other => panic!("expected AuditEvent::TurnEnded at index 2, got {other:?}"),
    }
    assert!(
        matches!(events[3], AuditEvent::LlmCost { .. }),
        "expected AuditEvent::LlmCost at index 3, got {:?}",
        events[3]
    );

    // Turn id correlation: the ToolCall's turn_id must match the
    // TurnStarted / TurnEnded pair wrapping it. This is the D5
    // guarantee that audit correlates within a turn.
    let (started_turn, started_session) = match events[0] {
        AuditEvent::TurnStarted {
            turn_id,
            session_id,
            ..
        } => (*turn_id, *session_id),
        _ => unreachable!(),
    };
    let tool_turn = match events[1] {
        AuditEvent::ToolCall { turn_id, .. } => *turn_id,
        _ => unreachable!(),
    };
    let ended_turn = match events[2] {
        AuditEvent::TurnEnded { turn_id, .. } => *turn_id,
        _ => unreachable!(),
    };
    assert_eq!(started_turn, tool_turn, "tool call must share the turn id");
    assert_eq!(
        started_turn, ended_turn,
        "turn-end must share the turn id"
    );
    // Session id is just surfaced so a regression that forgot to
    // propagate it would fail loudly. No further assertion needed.
    let _ = started_session;
}

// ---------------------------------------------------------------------------
// Test 2 — scope denial. The LLM asks for a path outside the sandbox
// prefix. The scope gate denies the tool call at the loop, the
// planner sees a `{"error":"denied"}` tool result, and then the LLM
// gives up with a final message. The turn still lands in
// `TurnOutcome::Completed` (D1: denial is a normal loop outcome,
// not a loop-level failure), and the audit chain has both a
// `ScopeDenied` entry and a `ToolCall` entry carrying
// `ToolOutcomeSummary::Denied`.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scripted_fs_read_out_of_sandbox_path_routes_through_denial_recovery() {
    let sandbox = TestSandbox::new();
    // Don't seed the sandbox with anything — the attack path points
    // at a filename the sandbox doesn't even contain, and the test
    // is going to assert the scope check rejects it before the tool
    // ever reaches the filesystem.
    let harness = build_harness(&sandbox);

    // The LLM asks to read `/etc/passwd`. The scope the tool derives
    // from the input will be `fs.read:/etc/passwd`, which is not
    // covered by the held `fs.read:<canonical_root>/**` capability,
    // so the loop's scope gate (agent.rs:~314) denies the call.
    let provider = ScriptedProvider::new(vec![
        ScriptedStep {
            events: vec![],
            terminal: LlmStepEnd::ToolCalls {
                calls: vec![ToolCallEnd {
                    call_id: "toolu_bad_01".to_string(),
                    tool_name: "fs.read".to_string(),
                    input: json!({ "path": "/etc/passwd" }),
                    name_resolution: aivyx_llm::NameResolution::Known,
                }],
                text_so_far: String::new(),
                usage: zero_usage(),
            },
        },
        ScriptedStep {
            events: vec![],
            terminal: LlmStepEnd::FinalMessage {
                text: "I can't read that path.".to_string(),
                usage: zero_usage(),
            },
        },
    ]);

    let audit_log = HmacChainLog::new([13u8; 32].to_vec());
    let audit_bridge = Arc::new(AuditBridge::new(audit_log));
    let audit_hook: Arc<dyn AuditHook> = audit_bridge.clone();

    let stdin_script = b"read /etc/passwd\n";
    let reader = Cursor::new(&stdin_script[..]);
    let channel = LocalChannel::<Vec<u8>>::new("fs-e2e-deny", Vec::new());

    let storage = open_scratch_storage(&sandbox).await;
    let config = base_session_config(&harness, storage);

    let report = run_session(
        Arc::clone(&provider) as Arc<dyn LlmProvider>,
        audit_hook,
        None,
        config,
        channel,
        reader,
    )
    .await
    .expect("run_session must complete cleanly even on scope denial");

    // ---- Assertion 1: the turn lands as Completed with the planner's
    //      recovery final message. A denied tool call is NOT a loop-
    //      level failure — the planner gets to see the denial and
    //      decide what to do.
    assert_eq!(report.turns_run, 1);
    match report.last_outcome {
        Some(TurnOutcome::Completed {
            ref final_message,
            tool_calls_made,
            ..
        }) => {
            assert_eq!(final_message, "I can't read that path.");
            // `tool_calls_made` counts attempted tool calls, including
            // denied ones — the loop bumps the counter before the
            // scope gate runs. This matches the existing
            // `tool_calls_made` coverage in agent.rs:631-635.
            assert_eq!(tool_calls_made, 1);
        }
        other => panic!("expected Completed(1 tool call) on denial, got {other:?}"),
    }

    // ---- Assertion 2: the planner's step-2 history carries a
    //      ToolResult with is_error=true and an "error":"denied"
    //      envelope. This is the exact shape `render_tool_result`
    //      emits for a `Denied` outcome.
    let last_messages = provider.last_messages.lock().unwrap().clone();
    let denial_result = last_messages.iter().find_map(|m| match m {
        LlmMessage::ToolResult {
            call_id,
            content,
            is_error,
        } if call_id == "toolu_bad_01" => Some((content.clone(), *is_error)),
        _ => None,
    });
    let (content, is_error) =
        denial_result.expect("step 2 must see a ToolResult for the denied call");
    assert!(is_error, "denied tool result must be flagged as an error");
    let parsed: Value = serde_json::from_str(&content).expect("denial envelope is JSON");
    assert_eq!(parsed["error"], "denied");
    assert!(
        parsed["message"]
            .as_str()
            .map(|s| s.contains("fs.read"))
            .unwrap_or(false),
        "denial message must name the denied scope, got {parsed:?}"
    );

    // ---- Assertion 3: the audit chain has the shape
    //      [TurnStarted, ScopeDenied, TurnEnded(Completed), LlmCost].
    //
    //      Note: the loop emits `ScopeDenied` *instead of* `ToolCall`
    //      — the early return at `agent.rs:316-338` means a denied
    //      call never reaches the `ToolCall` append site. So the
    //      runtime-auditor view (`ToolCall`) and the policy-auditor
    //      view (`ScopeDenied`) are mutually exclusive for a given
    //      call, not redundant. The invariant is "exactly one of the
    //      two per tool call attempt."
    let log = audit_bridge.writer();
    log.verify().expect("audit chain must verify on denial");
    let entries = log.entries().expect("can read entries");
    let events: Vec<&AuditEvent> = entries.iter().map(|e| &e.event).collect();

    assert_eq!(
        entries.len(),
        4,
        "expected TurnStarted + ScopeDenied + TurnEnded + LlmCost, got {} entries: {:#?}",
        entries.len(),
        events
    );

    assert!(matches!(events[0], AuditEvent::TurnStarted { .. }));
    match events[1] {
        AuditEvent::ScopeDenied {
            scope_requested, ..
        } => {
            // When `FsReadTool::required_scope` sees an absolute input
            // path that escapes the sandbox, `lexical_resolve` returns
            // `None` and the tool falls back to a deny sentinel
            // (`fs.read:/aivyx/__deny__/invalid-input`). That is the
            // scope the agent compares against the held capability
            // set — not `fs.read:/etc/passwd`. Asserting the sentinel
            // proves the lexical-escape path ran, which is the whole
            // point of this test.
            assert_eq!(scope_requested.base(), "fs.read");
            assert_eq!(
                scope_requested.qualifier(),
                Some("/aivyx/__deny__/invalid-input"),
                "denied scope must be the tool's lexical-escape sentinel, got {scope_requested:?}"
            );
        }
        other => panic!("expected ScopeDenied at index 1, got {other:?}"),
    }
    match events[2] {
        AuditEvent::TurnEnded {
            outcome,
            tool_calls_made,
            ..
        } => {
            // D1 invariant: a denied tool call is still "one attempted
            // tool call" from the loop's perspective, and the turn
            // itself lands as Completed (not Failed) because the
            // planner got to recover with a final message.
            assert_eq!(*outcome, TurnOutcomeSummary::Completed);
            assert_eq!(*tool_calls_made, 1);
        }
        other => panic!("expected TurnEnded at index 2, got {other:?}"),
    }
    assert!(
        matches!(events[3], AuditEvent::LlmCost { .. }),
        "expected LlmCost at index 3, got {:?}",
        events[3]
    );

    // Confirm there is NO `ToolCall` entry — the early return after
    // `ScopeDenied` is the reason, and a regression that accidentally
    // started emitting both should be caught here.
    let has_any_tool_call = events
        .iter()
        .any(|e| matches!(e, AuditEvent::ToolCall { .. }));
    assert!(
        !has_any_tool_call,
        "scope denial must NOT emit a ToolCall entry (loop returns early): {events:#?}"
    );
}

// ---------------------------------------------------------------------------
// Test 3 — Task 4 fix round 1. Proves `SessionConfig.confirm_destructive`
// actually threads all the way through `run_session` →
// `AgentStackSpec::from_session_config` → `build_agent_stack` →
// `ConcreteAgent::with_confirm_destructive` into the D1 dispatch gate in
// `aivyx-core::agent::run_tool_call` — not just that the field exists and
// compiles. `aivyx-core::agent::tests` already proves the gate itself
// works on a directly-constructed `ConcreteAgent`
// (`granted_email_send_scope_still_requires_confirmation_when_confirm_destructive_is_on`);
// this test is the end-to-end proof for the *other* production
// construction path (`build_agent_stack`, reached here via `run_session`
// exactly like the Local/REPL arm in the `aivyx-pa` binary).
// ---------------------------------------------------------------------------

/// A fake `email.send`-scoped tool. `email.send` is one of the withheld
/// integration bases (`aivyx_capability::is_withheld_integration_base`)
/// the Task 4 agent-level confirm gate targets — unlike `fs.write`/
/// `fs.delete`, which have their own, separate, tool-level
/// `confirm_destructive` check (`FsWriteToolConfig`/`FsDeleteToolConfig`)
/// unrelated to the agent-level gate under test here. A fake tool avoids
/// pulling `aivyx-gmail`'s OAuth-backed crate into this integration test
/// just to prove config threading.
struct EmailSendFakeTool {
    id: aivyx_core::ToolId,
    schema: Value,
}

impl EmailSendFakeTool {
    fn new() -> Self {
        EmailSendFakeTool {
            id: aivyx_core::ToolId::new(),
            schema: json!({}),
        }
    }
}

#[async_trait]
impl Tool for EmailSendFakeTool {
    fn id(&self) -> aivyx_core::ToolId {
        self.id
    }
    fn name(&self) -> &str {
        "gmail.send"
    }
    fn description(&self) -> &str {
        "fake gmail.send — Task 4 fix round 1 confirm_destructive threading test"
    }
    fn input_schema(&self) -> &Value {
        &self.schema
    }
    fn required_scope(&self, _input: &Value) -> Scope {
        Scope::parse("email.send").expect("email.send is a known base")
    }
    async fn execute(
        &self,
        _input: Value,
        _ctx: &aivyx_core::ToolContext<'_>,
    ) -> aivyx_core::ToolOutcome {
        // Would happily complete if it ever ran — proves the *gate* stops
        // the call before dispatch, not the tool refusing itself.
        aivyx_core::ToolOutcome::Completed {
            output: json!({"ok": true}),
            verified: aivyx_core::Verification::NotApplicable,
        }
    }
}

#[tokio::test]
async fn scripted_withheld_scope_escalates_when_confirm_destructive_threads_from_session_config()
{
    // Storage only — this test doesn't exercise the fs sandbox at all.
    let sandbox = TestSandbox::new();
    let storage = open_scratch_storage(&sandbox).await;

    let tool = Arc::new(EmailSendFakeTool::new());
    let tool_id = tool.id();
    let tools: Arc<ToolRegistry> = Arc::new(ToolRegistry::new(vec![tool as Arc<dyn Tool>]));
    // Role explicitly holds `email.send` — bypasses the floor-grant
    // question entirely, same as the aivyx-core unit test this extends.
    let capabilities = CapabilitySet::from_scopes([Scope::parse("email.send").unwrap()]);

    let provider = ScriptedProvider::new(vec![ScriptedStep {
        events: vec![],
        terminal: LlmStepEnd::ToolCalls {
            calls: vec![ToolCallEnd {
                call_id: "toolu_send_01".to_string(),
                tool_name: "gmail.send".to_string(),
                input: json!({}),
                name_resolution: aivyx_llm::NameResolution::Known,
            }],
            text_so_far: String::new(),
            usage: zero_usage(),
        },
    }]);
    // No second scripted step: if the confirm gate fails to short-circuit
    // and the loop tries to dispatch a second `chat_stream` call, the
    // scripted provider is exhausted and `run_session` fails loudly
    // instead of silently completing — the escalation path never reaches
    // a second planner turn.

    let audit_log = HmacChainLog::new([21u8; 32].to_vec());
    let audit_bridge = Arc::new(AuditBridge::new(audit_log));
    let audit_hook: Arc<dyn AuditHook> = audit_bridge.clone();

    let stdin_script = b"send the email\n";
    let reader = Cursor::new(&stdin_script[..]);
    let channel = LocalChannel::<Vec<u8>>::new("confirm-destructive-e2e", Vec::new());

    let config = SessionConfig {
        model: "claude-haiku-4-5-20251001".to_string(),
        system_prompt: "test".to_string(),
        max_tokens: 256,
        capabilities,
        tools,
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
        // The field under test.
        confirm_destructive: true,
    };

    let report = run_session(
        Arc::clone(&provider) as Arc<dyn LlmProvider>,
        audit_hook,
        None,
        config,
        channel,
        reader,
    )
    .await
    .expect("run_session must complete cleanly on an escalated turn");

    assert_eq!(report.turns_run, 1);
    match report.last_outcome {
        Some(TurnOutcome::Escalated {
            pending_tool,
            ref scope,
            tool_calls_made,
            ..
        }) => {
            assert_eq!(pending_tool, tool_id);
            assert_eq!(tool_calls_made, 1);
            assert_eq!(
                scope.as_ref().map(|s| s.base()),
                Some("email.send"),
                "escalation must carry the withheld scope"
            );
        }
        other => panic!(
            "expected TurnOutcome::Escalated — proves SessionConfig.confirm_destructive \
             actually reached the built ConcreteAgent's D1 gate through \
             build_agent_stack, not just that the field compiles; got {other:?}"
        ),
    }

    // The audit's ToolCall entry (still emitted — D1: no action, attempted
    // or not, goes unaudited) must record RequiresEscalation, confirming
    // the *gate*, not the fake tool, stopped the call.
    let log = audit_bridge.writer();
    log.verify().expect("audit chain must verify");
    let entries = log.entries().expect("can read entries");
    let tool_call_outcome = entries.iter().find_map(|e| match &e.event {
        AuditEvent::ToolCall { outcome, .. } => Some(outcome.clone()),
        _ => None,
    });
    assert_eq!(
        tool_call_outcome,
        Some(ToolOutcomeSummary::RequiresEscalation),
        "ToolCall audit entry must record RequiresEscalation"
    );
}
