//! Phase 3 task 5 — end-to-end CLI integration test.
//!
//! This test drives the same `run_session` function the `aivyx-pa` binary
//! calls, but with every non-deterministic seam replaced by a
//! test-owned fake:
//!
//! | Seam              | Binary                      | Test                  |
//! |-------------------|-----------------------------|-----------------------|
//! | `LlmProvider`     | `AnthropicProvider` (HTTPS) | `ScriptedProvider`    |
//! | `reader`          | `io::stdin().lock()`        | `Cursor<&[u8]>`       |
//! | `writer`          | `io::stdout()`              | `Arc<Mutex<Vec<u8>>>` |
//! | audit             | key from `/dev/urandom`     | fixed key             |
//! | signal handler    | `tokio::signal::ctrl_c`     | (skipped)             |
//!
//! The scripted provider returns a short run of `TextChunk` events
//! followed by a `FinalMessage` terminal for each call. The test asserts:
//!
//! 1. **User-visible output.** Every scripted text chunk appears in
//!    stdout, in order, followed by the `[turn completed]` marker. The
//!    prompt (`> `) appears once before each turn.
//! 2. **Audit chain.** The in-memory `HmacChainLog` contains exactly
//!    the events the loop should have emitted (`TurnStarted`,
//!    `TurnEnded`) for each turn, in order, and `verify()` passes.
//! 3. **Clean EOF.** When the scripted reader hits EOF, `run_session`
//!    returns `Ok` with the expected `turns_run` count.
//!
//! What this test deliberately does *not* do: assert on byte-exact
//! rendering of the finalize marker (the renderer has its own tests),
//! verify the SSE parser (the Anthropic provider has its own tests),
//! or replay any Anthropic wire bytes (that would just be re-testing
//! the provider-to-planner conversion path).

use std::io::Cursor;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;

use aivyx_audit::{AuditBridge, AuditEvent, AuditLog, HmacChainLog};
use aivyx_capability::{CapabilitySet, Scope};
use aivyx_channel::{run_session, LocalChannel, SessionConfig};
use aivyx_core::{AuditHook, CancellationToken, ToolRegistry, TurnOutcome, TurnOutcomeSummary};
use aivyx_crypto::MasterKey;
use aivyx_llm::{
    LlmError, LlmMessage, LlmProvider, LlmRequest, LlmStepEnd, LlmStream, LlmStreamEvent, LlmUsage,
};
use aivyx_storage::{RedbStorage, Storage, StorageConfig};

// ---------------------------------------------------------------------------
// Scratch storage — Phase 5 task 4 added `SessionConfig.storage`, so every
// integration test now needs a real `Arc<dyn Storage>` to feed the field.
// We hand-roll a `$TMPDIR`-based directory (same convention aivyx-core's
// fs.rs and fs_tool_e2e.rs both use — "avoid adding tempfile as a dep for
// 50 lines of test hygiene"), seed it with a deterministic master key
// (`[7u8; 32]` here, same shape aivyx-audit uses for its fixture keys),
// and drop it on test exit.
// ---------------------------------------------------------------------------

struct ScratchStoreDir {
    parent: PathBuf,
    store: PathBuf,
}

impl ScratchStoreDir {
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
            PathBuf::from(tmp).join(format!("aivyx-cli-e2e-store-{pid}-{nanos}-{uniq}"));
        std::fs::create_dir_all(&parent).expect("scratch store parent must be creatable");
        let store = parent.join("store.redb");
        ScratchStoreDir { parent, store }
    }
}

impl Drop for ScratchStoreDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.parent);
    }
}

async fn open_scratch_storage(dir: &ScratchStoreDir) -> Arc<dyn Storage> {
    // `MasterKey::from_raw` bypasses Argon2id and takes a literal 32
    // bytes — exactly what we want in a test, since Argon2id at
    // `d7_default()` takes ~500ms per call and this test is going
    // through `run_session`, not through the passphrase flow itself.
    let master = MasterKey::from_raw([7u8; 32]);
    RedbStorage::open(StorageConfig::new(dir.store.clone()), master)
        .await
        .expect("scratch storage must open")
}

// ---------------------------------------------------------------------------
// ScriptedProvider — yields one scripted step per `chat_stream` call.
// One `chat_stream` invocation corresponds to one planner `one_step`,
// and one `FinalMessage` terminal corresponds to one user turn
// (Phase 3 has no tools, so there's no tool-call → tool-result loop
// within a single turn).
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
        request: LlmRequest<'_>,
        _cancellation: &CancellationToken,
    ) -> Result<Box<dyn LlmStream>, LlmError> {
        // Sanity: the planner must always send a non-empty message list
        // or Anthropic would reject the request. We check it here so a
        // regression in `LlmPlanner::begin_turn` would fail this test
        // loudly instead of silently emptying the history.
        assert!(
            !request.messages.is_empty(),
            "planner sent an empty message list to the provider"
        );
        // And the planner must seed the history with a user message
        // first. (Tools don't run in Phase 3, so every request starts
        // with User.)
        assert!(
            matches!(request.messages[0], LlmMessage::User { .. }),
            "expected history[0] to be a User message"
        );

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

fn usage() -> LlmUsage {
    LlmUsage::default()
}

fn final_step(chunks: &[&str], text: &str) -> ScriptedStep {
    ScriptedStep {
        events: chunks
            .iter()
            .map(|c| LlmStreamEvent::TextChunk((*c).to_string()))
            .collect(),
        terminal: LlmStepEnd::FinalMessage {
            text: text.to_string(),
            usage: usage(),
        },
    }
}

// ---------------------------------------------------------------------------
// The test proper.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scripted_session_drives_two_turns_end_to_end() {
    // -- Scripted LLM responses. Two user turns → two scripted steps.
    let provider = ScriptedProvider::new(vec![
        final_step(&["Hello", ", ", "world"], "Hello, world"),
        final_step(&["Goodbye"], "Goodbye"),
    ]);

    // -- Audit log with a deterministic key so the chain is reproducible
    //    across runs. The binary uses /dev/urandom; tests use a fixed
    //    32-byte key because we assert on `entries().len()` and the
    //    actual MAC is whatever the key produces.
    let audit_log = HmacChainLog::new([42u8; 32].to_vec());
    let audit_bridge = Arc::new(AuditBridge::new(audit_log));
    let audit_hook: Arc<dyn AuditHook> = audit_bridge.clone();

    // -- Scripted stdin: two non-empty lines + a blank line + EOF.
    //    The blank line proves the loop skips whitespace-only input.
    let stdin_script = b"what time is it\n\ngoodbye\n";
    let reader = Cursor::new(&stdin_script[..]);

    // -- Capture stdout into an in-memory sink via LocalChannel.
    //    The channel keeps a second Arc<Mutex<Vec<u8>>> handle that
    //    we inspect after the session returns.
    let channel = LocalChannel::<Vec<u8>>::new("cli-e2e", Vec::new());
    let sink = channel.writer_handle();

    // -- Scratch storage. Phase 5 task 4 made `SessionConfig.storage`
    //    a required field; `run_session` writes a session-metadata
    //    record to `KeyDomain::Sessions` at open + after each turn.
    //    Nothing in the test asserts on storage contents (that's the
    //    job of Phase 5 task 5's `storage_persistence_e2e.rs`); the
    //    handle exists just so the chat-only regression compiles and
    //    exercises the write path once per turn.
    let scratch_store = ScratchStoreDir::new();
    let storage = open_scratch_storage(&scratch_store).await;

    // -- Session config. Empty prompt keeps captured output easy to
    //    assert on; no banner for the same reason. Empty tool registry:
    //    this test is the Phase 3 chat-only regression. Phase 4 task 5
    //    will add a separate `fs_tool_e2e.rs` that drives real tools.
    let config = SessionConfig {
        model: "claude-haiku-4-5-20251001".to_string(),
        system_prompt: "test".to_string(),
        max_tokens: 256,
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

    // -- Drive the session.
    let report = run_session(provider, audit_hook, None, config, channel, reader)
        .await
        .expect("run_session must complete cleanly on scripted EOF");

    // ---- Assertion 1: the session ran two turns and finished cleanly.
    assert_eq!(
        report.turns_run, 2,
        "scripted stdin has two non-empty lines; blank line should be skipped"
    );
    match report.last_outcome {
        Some(TurnOutcome::Completed {
            ref final_message,
            tool_calls_made,
            ..
        }) => {
            assert_eq!(final_message, "Goodbye");
            assert_eq!(tool_calls_made, 0);
        }
        other => panic!("expected last turn to be Completed(Goodbye), got {other:?}"),
    }

    // ---- Assertion 2: captured stdout contains every streamed chunk
    //      in the right order, plus the finalize marker after each turn.
    let output = String::from_utf8(sink.lock().unwrap().clone()).expect("utf-8 output");
    // Turn 1 chunks in order:
    let hello_at = output.find("Hello").expect("Hello chunk present");
    let comma_at = output.find(", ").expect(", chunk present");
    let world_at = output.find("world").expect("world chunk present");
    assert!(
        hello_at < comma_at && comma_at < world_at,
        "turn 1 chunks out of order in captured output: {output:?}"
    );
    // Finalize marker for turn 1 must appear before turn 2's chunks.
    let first_finalize = output
        .find("[turn completed]")
        .expect("turn 1 finalize marker present");
    let goodbye_at = output.find("Goodbye").expect("Goodbye chunk present");
    assert!(
        first_finalize < goodbye_at,
        "turn 1 must finalize before turn 2 starts streaming"
    );
    // And a second finalize marker for turn 2.
    let second_finalize = output[first_finalize + 1..]
        .find("[turn completed]")
        .expect("turn 2 finalize marker present");
    assert!(
        second_finalize > 0,
        "turn 2 finalize must appear after turn 1 finalize"
    );

    // ---- Assertion 3: the audit chain has exactly the shape we expect
    //      and verifies cleanly.
    let log = audit_bridge.writer();
    log.verify().expect("audit chain must verify");

    let entries = log.entries().expect("can read entries");
    // Two turns, each emitting TurnStarted, TurnEnded, and (Chapter K) a
    // LlmCost event — the LLM-backed planner reports a model, so the turn
    // epilogue prices the turn. No tool calls in between since Phase 3 has
    // no concrete tools registered.
    assert_eq!(
        entries.len(),
        6,
        "expected 6 audit entries (2 turns × TurnStarted+TurnEnded+LlmCost), got {}",
        entries.len()
    );

    // The chain stores typed `AuditEvent` values directly (not raw
    // bytes — the MAC covers the canonical JSON, but the entry keeps
    // the structured event for inspection). Pattern-match the sequence
    // shape directly.
    let events: Vec<&AuditEvent> = entries.iter().map(|e| &e.event).collect();

    // Turn 1: TurnStarted(0), TurnEnded(1), LlmCost(2).
    assert!(matches!(events[0], AuditEvent::TurnStarted { .. }));
    assert!(matches!(
        events[1],
        AuditEvent::TurnEnded {
            outcome: TurnOutcomeSummary::Completed,
            tool_calls_made: 0,
            ..
        }
    ));
    assert!(matches!(events[2], AuditEvent::LlmCost { .. }));
    // Turn 2: TurnStarted(3), TurnEnded(4), LlmCost(5).
    assert!(matches!(events[3], AuditEvent::TurnStarted { .. }));
    assert!(matches!(
        events[4],
        AuditEvent::TurnEnded {
            outcome: TurnOutcomeSummary::Completed,
            tool_calls_made: 0,
            ..
        }
    ));
    assert!(matches!(events[5], AuditEvent::LlmCost { .. }));

    // The two `TurnStarted` entries must carry the *same* session id
    // — both turns run on the one `LocalChannel`, so they share a
    // session. Different turn ids, same session.
    match (&events[0], &events[3]) {
        (
            AuditEvent::TurnStarted {
                turn_id: t1,
                session_id: s1,
                ..
            },
            AuditEvent::TurnStarted {
                turn_id: t2,
                session_id: s2,
                ..
            },
        ) => {
            assert_eq!(s1, s2, "both turns must share the channel's session id");
            assert_ne!(t1, t2, "each turn must get a distinct turn id");
        }
        _ => unreachable!(),
    }

    // Seq numbers are monotonic 0..6 (HmacChainLog invariant, but
    // worth asserting here so a regression in the chain would be
    // caught by the E2E test too, not just the audit-crate tests).
    for (i, entry) in entries.iter().enumerate() {
        assert_eq!(entry.seq, i as u64);
    }
}
