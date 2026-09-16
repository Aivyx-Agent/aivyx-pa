//! `run_session` — the reusable REPL loop shared by the `aivyx-pa` binary
//! and Phase 3 task 5's end-to-end integration test.
//!
//! ## Why this lives in the library
//!
//! Phase 3 task 5 needs a hermetic test that drives the full CLI stack
//! (planner → provider → audit → channel) via scripted I/O, without
//! touching the network. The binary's `main.rs` hardcodes
//! `AnthropicProvider::new(...)` + `io::stdin()` + `io::stdout()`, none
//! of which a test can intercept. Two options exist:
//!
//! 1. Add a test-only transport backdoor to the binary.
//! 2. Extract the REPL itself into a library function parameterized by
//!    `Arc<dyn LlmProvider>` + `impl BufRead` + `impl Write`, so tests
//!    call the library directly while `main.rs` stays a thin wiring
//!    layer.
//!
//! Option 2 is cleaner: it separates *composition* (what `main` decides
//! at process start — where secrets come from, which provider backs the
//! planner, which sinks I/O talks to) from *execution* (what a turn
//! actually does). The test gets to swap composition without ever
//! calling `main`.
//!
//! ## What this module owns
//!
//! - [`SessionConfig`] — the per-session knobs: model id, system prompt,
//!   max output tokens, capability set. Produced from the binary's
//!   env-var parsing or from a test fixture.
//! - [`run_session`] — the REPL loop itself. Reads user input from the
//!   `reader` one line at a time, rotates the channel's cancellation
//!   token, drives a turn through the provided agent, and keeps going
//!   until EOF. Returns a [`SessionReport`] the test can assert on.
//! - [`SessionReport`] — how many turns ran and the final outcome of
//!   the last turn. Minimal by design; audit verification goes through
//!   the `AuditBridge::writer()` handle the caller already holds.
//!
//! ## What this module deliberately does **not** own
//!
//! - **Signal handling.** The signal task in `main.rs` spawns a
//!   `tokio::signal::ctrl_c` listener that reads the channel's token
//!   slot. Unit tests don't send Unix signals, and wiring a signal
//!   listener into an integration test would be flaky. The channel's
//!   `reset_cancellation()` call on every iteration is the only loop-
//!   level piece of ctrl-C machinery, and that's here.
//! - **Secret handling.** `SessionConfig` takes plain `String`s for
//!   `model` and `system_prompt`. The API key lives inside whichever
//!   `LlmProvider` the caller supplies — `AnthropicProvider` holds a
//!   `SecretString` internally, and tests use a `FakeLlmProvider` with
//!   no secret at all. Keeping the session layer secret-free means the
//!   test path never has to mint a fake key.

use std::io::{BufRead, Write};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use aivyx_capability::CapabilitySet;
// `LlmPlanner` + `LlmPlannerConfig` + `Agent` were used by the
// inline agent-stack construction Phase 137 lifted into
// `build_agent_stack`; they stay imported there (qualified
// imports inside the helper) and no longer need to be visible
// in `run_session`'s scope. `ConcreteAgent` + `AgentId` remain
// imported because `build_agent_stack` is in the same module.
use aivyx_core::{
    agent::ConcreteAgent, planner::ToolRegistry, AgentId, AuditHook, ChannelContext,
    Message, TurnOutcome,
};
use aivyx_llm::LlmProvider;
use aivyx_storage::{KeyDomain, Storage};

use crate::LocalChannel;

/// Fixed redb key used for the single-row "current session" marker.
///
/// Phase 5 task 4 persistence scope (Q1 option 1): session metadata only.
/// A second process start reads this key under `KeyDomain::Sessions` to
/// detect "I've been here before." The value layout is 40 bytes:
///
/// ```text
///   [0..16]   session_id  (Uuid bytes — *current* process's session)
///   [16..24]  opened_at_secs   u64 big-endian, seconds since UNIX_EPOCH
///   [24..32]  last_turn_index  u64 big-endian, REPL turn counter (0 at open)
///   [32..40]  last_turn_at_secs u64 big-endian, 0 before the first turn
/// ```
///
/// Not serde: this record has exactly four fields and will never grow
/// within Phase 5 (Q4: schema bumps happen by HKDF salt rotation, not by
/// in-place migration), so a hand-rolled fixed layout avoids pulling
/// `serde_json` into the channel crate's prod deps.
const SESSION_MARKER_KEY: &[u8] = b"current";
const SESSION_MARKER_LEN: usize = 40;

/// Knobs the REPL needs to construct one session's planner + agent.
pub struct SessionConfig {
    pub model: String,
    pub system_prompt: String,
    pub max_tokens: u32,
    /// The agent's capability set. Defaults in the binary are broad
    /// (`memory.read`, `memory.write`) because the local CLI is the
    /// most-trusted channel on the box; tests may pick their own.
    pub capabilities: CapabilitySet,
    /// The tool registry for this session. Phase 4 task 4 moved this
    /// out of `run_session` (where it was hardcoded to an empty
    /// registry) so the binary can register real tools at startup
    /// while the chat-only integration test keeps passing an empty
    /// one. Shared as `Arc` because the planner factory closure
    /// clones it per-turn and `ConcreteAgent` holds its own handle.
    pub tools: Arc<ToolRegistry>,
    /// The encrypted storage handle for this session. Phase 5 task 4
    /// added the field; `run_session` writes a small session-metadata
    /// record under `KeyDomain::Sessions` at open and after each turn
    /// (see [`SESSION_MARKER_KEY`]). Shared as `Arc<dyn Storage>` to
    /// match the `AuditHook` pattern from Phase 2 — the binary owns
    /// the one-per-process `RedbStorage` handle, tests inject a
    /// throwaway `RedbStorage` against a tempdir, and both flow
    /// through the same trait object.
    pub storage: Arc<dyn Storage>,
    /// Prompt string written before each `read_line`. The binary
    /// passes `"> "`; tests usually pass `""` so captured output is
    /// easier to assert on.
    pub prompt: String,
    /// Banner line printed once at session start, before the first
    /// prompt. `None` means "no banner" — the test path uses this to
    /// keep stdout output deterministic.
    pub banner: Option<String>,
    /// Phase 11 Task 4 — role-derived tool allowlist. `None` means
    /// "allow every registered tool" (legacy Phase 6–10 behavior).
    /// `Some(set)` filters the advertised catalog at the planner
    /// layer and the dispatch gate at the agent layer — see
    /// `LlmPlannerConfig::tool_allowlist` and
    /// `ConcreteAgent::with_tool_allowlist` for the two enforcement
    /// points.
    pub tool_allowlist: Option<std::collections::BTreeSet<String>>,
    /// Phase 11 Task 4 — role-derived memory-topic prefix. `None`
    /// means "no prefix" (legacy behavior). A `Some` value is
    /// prepended by the dispatch layer to every `memory.*` tool
    /// call's `topic` input before the tool sees it. Invisible to
    /// the model by design.
    pub memory_topic_prefix: Option<String>,
    /// Phase 30 — runtime role overrides. When `Some`, the planner
    /// factory reads this on each turn construction to pick up
    /// prompt appendix and allowlist mutations set by `role.update`.
    pub role_overrides: Option<crate::role_overrides::SharedRoleOverrides>,
    /// Phase 43 — context window size in tokens for pruning. When
    /// `Some`, the planner prunes old history when estimated tokens
    /// exceed 80% of this value. `None` disables pruning.
    pub context_window_tokens: Option<usize>,
    /// Phase 43 Task 4 — optional sink for persisting pruned context
    /// summaries. When `Some`, the planner calls it whenever messages
    /// are dropped during context-window pruning. `None` means pruned
    /// messages are silently discarded.
    pub prune_sink: Option<Arc<dyn aivyx_core::llm_planner::PruneSink>>,
    /// Phase 76 — optional automatic-recall hook. When `Some`,
    /// the planner embeds each user message and prepends the
    /// top relevant memories to that turn. `None` means no
    /// auto-recall (pre-Phase-76 behavior). Mirrors
    /// `prune_sink` — a planner hook carried by-Arc through the
    /// per-turn config clone.
    pub context_provider:
        Option<Arc<dyn aivyx_core::llm_planner::ContextProvider>>,
    /// Phase 79 — optional per-turn system-prompt refiner
    /// (adaptive Persona). Carried by-Arc through the per-turn
    /// config clone, applied in `begin_turn` *after* the
    /// Phase 60 refresher sets the base prompt, so the selected
    /// Persona wins. `None` = pre-Phase-79 behavior.
    pub system_prompt_refiner:
        Option<Arc<dyn aivyx_core::llm_planner::SystemPromptRefiner>>,
    /// Phase 60 — per-turn system-prompt refresh closure. When
    /// `Some`, the planner factory invokes this on each turn to
    /// rebuild the `system_prompt` from the current state of
    /// Profile, Persona, and the active role. This is the
    /// hot-reload hook for reflection-approved Persona deltas
    /// (P14 commit 3) and closes the Phase 59 Q5(a) deferral.
    /// `None` means "use the static `system_prompt` baked at
    /// session-build time" — backwards-compatible with pre-Phase-60
    /// callers.
    pub prompt_refresher: Option<Arc<dyn Fn() -> String + Send + Sync>>,
    /// Per-turn safety knobs (deadline + small-cycle breaker), built once from
    /// the operator's `[agent]` config. `TurnSafety::default()` keeps the loop
    /// byte-identical. Lifted into the `AgentStackSpec` and applied in
    /// `build_agent_stack`.
    pub turn_safety: aivyx_core::TurnSafety,
    /// Task 4 fix round 1 — the operator's `[access] confirm_destructive`
    /// posture, threaded into `ConcreteAgent::with_confirm_destructive` by
    /// `build_agent_stack` (via `AgentStackSpec`) so the agent-level
    /// confirm-destructive gate in `run_tool_call` (D1) actually fires for
    /// REPL/Local sessions built through `run_session`. Mirrors the same
    /// flag already wired into the tool-level `fs.write`/`fs.delete`/
    /// `git.commit` configs at the binary's construction sites. `false`
    /// keeps the loop byte-identical to pre-Task-4 behavior.
    pub confirm_destructive: bool,
}

/// Phase 137 — agent-stack construction inputs.
///
/// The subset of [`SessionConfig`] fields that
/// `build_agent_stack` reads, lifted out of the
/// REPL-specific surface so non-REPL channel
/// adapters (Phase 137's voice loop;
/// Phase 138+ web / REST) can construct the same
/// agent stack without going through `run_session`.
///
/// Field-for-field a subset of `SessionConfig`. The
/// REPL extras (`prompt`, `banner`, `storage`) live
/// on `SessionConfig` only because they're
/// REPL-specific; agent construction doesn't need
/// them.
pub struct AgentStackSpec {
    pub model: String,
    pub system_prompt: String,
    pub max_tokens: u32,
    pub capabilities: CapabilitySet,
    pub tools: Arc<ToolRegistry>,
    pub tool_allowlist: Option<std::collections::BTreeSet<String>>,
    pub memory_topic_prefix: Option<String>,
    pub role_overrides: Option<crate::role_overrides::SharedRoleOverrides>,
    pub context_window_tokens: Option<usize>,
    pub prune_sink: Option<Arc<dyn aivyx_core::llm_planner::PruneSink>>,
    pub context_provider:
        Option<Arc<dyn aivyx_core::llm_planner::ContextProvider>>,
    pub system_prompt_refiner:
        Option<Arc<dyn aivyx_core::llm_planner::SystemPromptRefiner>>,
    pub prompt_refresher: Option<Arc<dyn Fn() -> String + Send + Sync>>,
    /// Chapter K (K.4.2) — optional pre-call dollar gate, attached to the
    /// built agent. `None` (the default) leaves turns ungated. Non-daemon
    /// LLM-backed channels (voice) set this from the shared
    /// `ChannelBudgetGate` built at startup.
    pub budget_gate: Option<Arc<dyn aivyx_core::BudgetGate>>,
    /// Chapter Throttle (TH.3) — optional per-tool-call rate-limit gate,
    /// attached to the built agent. `None` (the default) leaves tool calls
    /// unthrottled. The daemon builds this from `[rate_limit]` at startup.
    pub rate_gate: Option<Arc<dyn aivyx_core::RateGate>>,
    /// Per-turn safety knobs (deadline + small-cycle breaker), applied to the
    /// built agent via `TurnSafety::apply`. `TurnSafety::default()` leaves the
    /// loop byte-identical.
    pub turn_safety: aivyx_core::TurnSafety,
    /// `aivyx-checkpoint` — attached to the built agent so fs_root-mutating
    /// tool calls get a git-ref snapshot before they run. `None` (the
    /// default from `from_session_config`) leaves the loop byte-identical;
    /// non-REPL channels (voice) that want checkpointing set this directly
    /// on the spec, same pattern as `budget_gate`/`rate_gate`.
    pub checkpointer: Option<std::sync::Arc<aivyx_core::GitCheckpointer>>,
    /// Task 4 fix round 1 — the operator's `[access] confirm_destructive`
    /// posture, applied to the built agent via
    /// `ConcreteAgent::with_confirm_destructive` in `build_agent_stack`.
    /// `from_session_config` copies this from `SessionConfig`; the voice
    /// arm (the other `AgentStackSpec` construction site) sets it directly,
    /// same pattern as `turn_safety`. `false` leaves the loop
    /// byte-identical to pre-Task-4 behavior.
    pub confirm_destructive: bool,
}

impl AgentStackSpec {
    /// Lift the agent-relevant fields out of a
    /// `SessionConfig`. The Local-channel REPL calls
    /// this internally; non-REPL channels (voice,
    /// future web/REST) build the spec directly.
    pub fn from_session_config(c: &SessionConfig) -> Self {
        AgentStackSpec {
            model: c.model.clone(),
            system_prompt: c.system_prompt.clone(),
            max_tokens: c.max_tokens,
            capabilities: c.capabilities.clone(),
            tools: Arc::clone(&c.tools),
            tool_allowlist: c.tool_allowlist.clone(),
            memory_topic_prefix: c.memory_topic_prefix.clone(),
            role_overrides: c.role_overrides.clone(),
            context_window_tokens: c.context_window_tokens,
            prune_sink: c.prune_sink.clone(),
            context_provider: c.context_provider.clone(),
            system_prompt_refiner: c.system_prompt_refiner.clone(),
            prompt_refresher: c.prompt_refresher.clone(),
            // The REPL/local path is ungated for now; daemon + voice attach
            // the shared gate at their own build sites.
            budget_gate: None,
            rate_gate: None,
            turn_safety: c.turn_safety.clone(),
            checkpointer: None,
            confirm_destructive: c.confirm_destructive,
        }
    }
}

/// Phase 137 — build the agent stack from a provider,
/// audit hook, and [`AgentStackSpec`].
///
/// Returns an `Arc<dyn Agent>` ready for any channel
/// adapter to drive — `run_session` for the REPL,
/// `aivyx_voice::run_push_to_talk_loop` for voice,
/// future web/REST adapters likewise.
///
/// The planner factory closure captures the
/// provider, registry, prompt refresher, and role
/// overrides by `Arc`; each turn the closure
/// clones the planner config, applies the optional
/// per-turn mutations (system prompt refresh, role
/// override allowlist mutations), and constructs a
/// fresh `LlmPlanner`. Per-turn cost is one config
/// clone and one Arc clone of each captured value.
pub fn build_agent_stack(
    provider: Arc<dyn aivyx_llm::LlmProvider>,
    audit: Arc<dyn aivyx_core::AuditHook>,
    spec: AgentStackSpec,
) -> Arc<dyn aivyx_core::Agent> {
    use aivyx_core::llm_planner::{LlmPlanner, LlmPlannerConfig};

    let AgentStackSpec {
        model,
        system_prompt,
        max_tokens,
        capabilities,
        tools,
        tool_allowlist,
        memory_topic_prefix,
        role_overrides,
        context_window_tokens,
        prune_sink,
        context_provider,
        system_prompt_refiner,
        prompt_refresher,
        budget_gate,
        rate_gate,
        turn_safety,
        checkpointer,
        confirm_destructive,
    } = spec;

    let provider_for_factory = Arc::clone(&provider);
    let registry_for_factory = Arc::clone(&tools);
    let mut planner_config = LlmPlannerConfig::new(model)
        .with_system_prompt(system_prompt)
        .with_max_tokens(max_tokens)
        .with_tool_allowlist(tool_allowlist.clone());
    if let Some(cw) = context_window_tokens {
        planner_config = planner_config.with_context_window(cw);
    }
    if let Some(sink) = prune_sink {
        planner_config = planner_config.with_prune_sink(sink);
    }
    if let Some(provider) = context_provider {
        planner_config = planner_config.with_context_provider(provider);
    }
    if let Some(refiner) = system_prompt_refiner {
        planner_config = planner_config.with_system_prompt_refiner(refiner);
    }
    let role_overrides_for_factory = role_overrides;
    let prompt_refresher_for_factory = prompt_refresher;

    let agent = ConcreteAgent::new(
        AgentId::new(),
        capabilities,
        tools,
        audit,
        move || {
            let mut cfg = planner_config.clone();
            if let Some(ref refresher) = prompt_refresher_for_factory {
                cfg.system_prompt = Some(refresher());
            }
            if let Some(ref shared) = role_overrides_for_factory
                && let Ok(overrides) = shared.read()
                && !overrides.is_empty()
            {
                crate::role_overrides::apply_to_planner_config(&overrides, &mut cfg);
            }
            Box::new(LlmPlanner::new(
                Arc::clone(&provider_for_factory),
                Arc::clone(&registry_for_factory),
                cfg,
            ))
        },
    )
    .with_tool_allowlist(tool_allowlist)
    .with_memory_topic_prefix(memory_topic_prefix)
    .with_budget_gate(budget_gate)
    .with_rate_gate(rate_gate)
    .with_checkpointer(checkpointer)
    // Task 4 fix round 1 — same `[access] confirm_destructive` posture as
    // the tool-level fs.write/fs.delete/git.commit gate; without this the
    // agent-level confirm-destructive gate in `run_tool_call` (D1) never
    // fires for the REPL/Local and voice agent stacks built here.
    .with_confirm_destructive(confirm_destructive);

    // Apply the per-turn safety knobs (deadline + small-cycle breaker) through
    // the one shared choke point, so this path can't drift from the others.
    let agent = turn_safety.apply(agent);

    Arc::new(agent)
}

/// Summary of what the session did, returned after EOF.
#[derive(Debug, Clone)]
pub struct SessionReport {
    /// Number of non-empty lines the user fed in that actually ran a
    /// turn. Empty lines and whitespace-only lines are skipped and do
    /// not count.
    pub turns_run: usize,
    /// Outcome of the last turn, if any. `None` means the session
    /// never saw a non-empty input line.
    pub last_outcome: Option<TurnOutcome>,
}

/// Drive a single CLI session to completion.
///
/// The loop reads lines from `reader` (usually `io::stdin().lock()` in
/// production or a `Cursor` in tests), dispatches each non-empty line
/// as a `Message::text` through the agent, and streams the resulting
/// events to the `LocalChannel` wrapped around `writer`. Returns when
/// `reader` signals EOF (`read_line` returns `Ok(0)`).
///
/// The `provider` is an `Arc<dyn LlmProvider>` so the binary can pass
/// a live `AnthropicProvider` and the integration test can pass a
/// `FakeLlmProvider`. Both flow through the same `LlmPlanner` +
/// `ConcreteAgent` stack.
///
/// The `audit` hook is passed in rather than constructed here because
/// the test wants to inspect the chain afterwards via its own
/// `AuditBridge::writer()` handle, and the binary wants to use
/// `/dev/urandom` for the key while the test wants a deterministic one.
pub async fn run_session<R, W>(
    provider: Arc<dyn LlmProvider>,
    audit: Arc<dyn AuditHook>,
    checkpointer: Option<std::sync::Arc<aivyx_core::GitCheckpointer>>,
    config: SessionConfig,
    channel: LocalChannel<W>,
    mut reader: R,
) -> Result<SessionReport, String>
where
    // `R: BufRead` is intentionally *not* `Send`: the binary's
    // `io::StdinLock` is not `Send`, and `run_session` is always
    // driven from a single task (there's no internal `spawn` that
    // crosses threads with the reader), so a `Send` bound would be
    // a phantom requirement that just breaks the real caller.
    R: BufRead,
    W: Write + Send + 'static,
{
    // ---- Agent stack --------------------------------------------------
    //
    // Phase 137 — the construction logic that was previously inline
    // here lives in `build_agent_stack` so non-REPL channels (voice,
    // future web/REST) can reuse it. The REPL-specific extras
    // (storage handle for the session marker, banner, prompt
    // string) stay below.
    let storage = Arc::clone(&config.storage);
    let agent_spec = AgentStackSpec::from_session_config(&config);
    let agent_spec = AgentStackSpec { checkpointer, ..agent_spec };
    let agent = build_agent_stack(provider, Arc::clone(&audit), agent_spec);

    // ---- Session marker (Phase 5 task 4) -----------------------------
    //
    // Ask the store whether a previous process already wrote a marker
    // under `KeyDomain::Sessions` / `SESSION_MARKER_KEY`. If one exists,
    // surface a one-line "resuming" message to stderr — the point of
    // the phase is proving the round-trip works across process
    // boundaries, and this is the smallest observable that demonstrates
    // it without touching the planner's conversation state (which is
    // Phase 6 memory territory).
    //
    // Storage errors are *not* fatal: if the store rejects the read or
    // the value decodes funny, we log and keep going. The REPL is the
    // user's primary surface; a degraded persistence layer should
    // never cost them the ability to talk to the agent.
    let session_id = channel.session_id();
    let sessions = storage.domain(KeyDomain::Sessions);
    match sessions.get(SESSION_MARKER_KEY).await {
        Ok(Some(bytes)) => match decode_session_marker(&bytes) {
            Some(prior) => {
                eprintln!(
                    "aivyx-pa: resuming — prior session {} opened {}s ago, last turn index {}",
                    prior.session_uuid_hex(),
                    now_secs().saturating_sub(prior.opened_at_secs),
                    prior.last_turn_index,
                );
            }
            None => {
                eprintln!(
                    "aivyx-pa: session marker present but unparseable ({} bytes); starting fresh",
                    bytes.len()
                );
            }
        },
        Ok(None) => {
            // First run against this store. Silent — the banner is
            // enough UX for "new session starting."
        }
        Err(e) => {
            eprintln!("aivyx-pa: session marker read failed ({e}); starting fresh");
        }
    }

    let opened_at_secs = now_secs();
    write_session_marker(
        &sessions,
        &SessionMarker {
            session_uuid: *session_id.0.as_bytes(),
            opened_at_secs,
            last_turn_index: 0,
            last_turn_at_secs: 0,
        },
    )
    .await;

    // ---- Banner ------------------------------------------------------
    //
    // Printed to the same writer the channel will stream through. We
    // reach into `writer_handle()` rather than adding a separate
    // `Banner` event because the channel's `StreamEvent` vocabulary is
    // locked by D3 and the banner is not a turn event.
    if let Some(banner) = config.banner.as_deref() {
        let writer = channel.writer_handle();
        let mut guard = writer
            .lock()
            .map_err(|e| format!("writer mutex poisoned: {e}"))?;
        writeln!(&mut *guard, "{banner}")
            .map_err(|e| format!("banner write failed: {e}"))?;
        guard
            .flush()
            .map_err(|e| format!("banner flush failed: {e}"))?;
    }

    // ---- REPL --------------------------------------------------------
    let mut turns_run: usize = 0;
    let mut last_outcome: Option<TurnOutcome> = None;
    let mut line = String::new();

    loop {
        // Prompt is written directly to the channel's writer so the
        // test's captured output reflects exactly what the user would
        // have seen. In the binary case (`io::Stdout`), this is the
        // same file descriptor a bare `print!` would reach.
        if !config.prompt.is_empty() {
            let writer = channel.writer_handle();
            let mut guard = writer
                .lock()
                .map_err(|e| format!("writer mutex poisoned: {e}"))?;
            write!(&mut *guard, "{}", config.prompt)
                .map_err(|e| format!("prompt write failed: {e}"))?;
            guard
                .flush()
                .map_err(|e| format!("prompt flush failed: {e}"))?;
        }

        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => {
                return Ok(SessionReport {
                    turns_run,
                    last_outcome,
                });
            }
            Ok(_) => {}
            Err(e) => return Err(format!("failed to read from input: {e}")),
        }

        let input = line.trim();
        if input.is_empty() {
            continue;
        }

        // Rotate the channel's cancellation token so a turn-N cancel
        // does not pre-cancel turn N+1. Same reasoning as the binary:
        // `tokio_util::CancellationToken` is monotonic, so we swap in
        // a fresh one per turn.
        channel.reset_cancellation();

        // Phase 45 — `/image <path> [caption]` command. Reads a local
        // file, detects media type from extension, and constructs an
        // image or text+image message for the agent.
        let message = if let Some(rest) = input.strip_prefix("/image ") {
            match parse_image_command(rest) {
                Ok((path, caption)) => match read_image_file(&path) {
                    Ok((media_type, data)) => {
                        if let Some(text) = caption {
                            Message::text_with_image(
                                channel.session_id(),
                                text,
                                media_type,
                                data,
                            )
                        } else {
                            Message::image(channel.session_id(), media_type, data)
                        }
                    }
                    Err(e) => {
                        let writer = channel.writer_handle();
                        if let Ok(mut guard) = writer.lock() {
                            let _ = writeln!(&mut *guard, "error: {e}");
                            let _ = guard.flush();
                        }
                        continue;
                    }
                },
                Err(e) => {
                    let writer = channel.writer_handle();
                    if let Ok(mut guard) = writer.lock() {
                        let _ = writeln!(&mut *guard, "error: {e}");
                        let _ = guard.flush();
                    }
                    continue;
                }
            }
        } else {
            Message::text(channel.session_id(), input)
        };
        let outcome = agent.turn(message, &channel).await;
        turns_run += 1;
        last_outcome = Some(outcome);

        // Update the session marker with the fresh turn count and the
        // current wall-clock timestamp. Same non-fatal-on-error shape
        // as the open-time write above: a storage hiccup should not
        // take down the REPL between the user's turns.
        write_session_marker(
            &sessions,
            &SessionMarker {
                session_uuid: *session_id.0.as_bytes(),
                opened_at_secs,
                last_turn_index: turns_run as u64,
                last_turn_at_secs: now_secs(),
            },
        )
        .await;
    }
}

// ---------------------------------------------------------------------------
// Session marker — hand-rolled fixed-layout encode/decode for the
// 40-byte metadata record at `KeyDomain::Sessions` / `SESSION_MARKER_KEY`.
//
// Kept private to this module because the encoding is an internal
// detail of how `run_session` uses storage, not part of the public API
// of the channel crate. A future phase (probably Phase 6 memory) will
// replace this with a richer schema keyed on `SessionId` bytes; at that
// point we'll bump the HKDF salt and start the "aivyx-v2-storage" era
// rather than try to migrate in place.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
struct SessionMarker {
    session_uuid: [u8; 16],
    opened_at_secs: u64,
    last_turn_index: u64,
    last_turn_at_secs: u64,
}

impl SessionMarker {
    fn encode(&self) -> [u8; SESSION_MARKER_LEN] {
        let mut out = [0u8; SESSION_MARKER_LEN];
        out[0..16].copy_from_slice(&self.session_uuid);
        out[16..24].copy_from_slice(&self.opened_at_secs.to_be_bytes());
        out[24..32].copy_from_slice(&self.last_turn_index.to_be_bytes());
        out[32..40].copy_from_slice(&self.last_turn_at_secs.to_be_bytes());
        out
    }

    fn session_uuid_hex(&self) -> String {
        let mut s = String::with_capacity(32);
        for byte in self.session_uuid.iter() {
            s.push_str(&format!("{byte:02x}"));
        }
        s
    }
}

fn decode_session_marker(bytes: &[u8]) -> Option<SessionMarker> {
    if bytes.len() != SESSION_MARKER_LEN {
        return None;
    }
    let mut session_uuid = [0u8; 16];
    session_uuid.copy_from_slice(&bytes[0..16]);
    let opened_at_secs = u64::from_be_bytes(bytes[16..24].try_into().ok()?);
    let last_turn_index = u64::from_be_bytes(bytes[24..32].try_into().ok()?);
    let last_turn_at_secs = u64::from_be_bytes(bytes[32..40].try_into().ok()?);
    Some(SessionMarker {
        session_uuid,
        opened_at_secs,
        last_turn_index,
        last_turn_at_secs,
    })
}

async fn write_session_marker(sessions: &aivyx_storage::DomainHandle, marker: &SessionMarker) {
    let encoded = marker.encode();
    if let Err(e) = sessions.put(SESSION_MARKER_KEY, &encoded).await {
        eprintln!("aivyx-pa: session marker write failed ({e}); continuing");
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Phase 45 — `/image` command helpers
// ---------------------------------------------------------------------------

/// Detect MIME type from a file extension. Returns an error for
/// unsupported extensions so the user gets clear feedback.
fn detect_media_type(path: &std::path::Path) -> Result<&'static str, String> {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("png") => Ok("image/png"),
        Some("jpg" | "jpeg") => Ok("image/jpeg"),
        Some("gif") => Ok("image/gif"),
        Some("webp") => Ok("image/webp"),
        Some(other) => Err(format!(
            "unsupported image extension '.{other}' — expected png, jpg, jpeg, gif, or webp"
        )),
        None => Err(format!(
            "cannot detect image type: '{}' has no file extension",
            path.display()
        )),
    }
}

/// Parse `/image <path> [caption]`. The path is the first whitespace-
/// delimited token; everything after it (if any) is the caption text.
fn parse_image_command(rest: &str) -> Result<(String, Option<String>), String> {
    let rest = rest.trim();
    if rest.is_empty() {
        return Err("usage: /image <path> [caption text]".to_string());
    }
    // Split on first whitespace: path + optional caption.
    let (path, caption) = match rest.split_once(char::is_whitespace) {
        Some((p, c)) => {
            let c = c.trim();
            if c.is_empty() {
                (p.to_string(), None)
            } else {
                (p.to_string(), Some(c.to_string()))
            }
        }
        None => (rest.to_string(), None),
    };
    Ok((path, caption))
}

/// Read a file from disk and detect its media type from the extension.
fn read_image_file(path: &str) -> Result<(String, Vec<u8>), String> {
    let p = std::path::Path::new(path);
    let media_type = detect_media_type(p)?;
    let data = std::fs::read(p).map_err(|e| format!("cannot read '{}': {e}", p.display()))?;
    if data.is_empty() {
        return Err(format!("file '{}' is empty", p.display()));
    }
    Ok((media_type.to_string(), data))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_marker() -> SessionMarker {
        SessionMarker {
            session_uuid: [
                0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0xfe, 0xdc, 0xba, 0x98, 0x76, 0x54,
                0x32, 0x10,
            ],
            opened_at_secs: 0x1122_3344_5566_7788,
            last_turn_index: 42,
            last_turn_at_secs: 0x0011_2233_4455_6677,
        }
    }

    #[test]
    fn session_marker_round_trips_through_encode_decode() {
        let marker = sample_marker();
        let bytes = marker.encode();
        assert_eq!(bytes.len(), SESSION_MARKER_LEN);

        let decoded = decode_session_marker(&bytes).expect("well-formed bytes must decode");
        assert_eq!(decoded.session_uuid, marker.session_uuid);
        assert_eq!(decoded.opened_at_secs, marker.opened_at_secs);
        assert_eq!(decoded.last_turn_index, marker.last_turn_index);
        assert_eq!(decoded.last_turn_at_secs, marker.last_turn_at_secs);
    }

    #[test]
    fn session_marker_encode_uses_big_endian_layout() {
        // Pin the exact byte layout: a future refactor that flips
        // endian-ness would silently corrupt existing stores on disk
        // if this test didn't anchor the layout explicitly.
        let marker = sample_marker();
        let bytes = marker.encode();

        assert_eq!(&bytes[0..16], &marker.session_uuid);
        assert_eq!(&bytes[16..24], &marker.opened_at_secs.to_be_bytes());
        assert_eq!(&bytes[24..32], &marker.last_turn_index.to_be_bytes());
        assert_eq!(&bytes[32..40], &marker.last_turn_at_secs.to_be_bytes());
    }

    #[test]
    fn decode_session_marker_rejects_wrong_length() {
        assert!(decode_session_marker(&[]).is_none());
        assert!(decode_session_marker(&[0u8; SESSION_MARKER_LEN - 1]).is_none());
        assert!(decode_session_marker(&[0u8; SESSION_MARKER_LEN + 1]).is_none());
    }

    #[test]
    fn session_uuid_hex_is_lowercase_zero_padded_32_chars() {
        let marker = sample_marker();
        let hex = marker.session_uuid_hex();
        assert_eq!(hex.len(), 32);
        assert_eq!(hex, "0123456789abcdeffedcba9876543210");
    }

    // ---- Phase 45 — /image command helpers ----

    #[test]
    fn detect_media_type_from_extension() {
        use std::path::Path;
        assert_eq!(detect_media_type(Path::new("photo.png")).unwrap(), "image/png");
        assert_eq!(detect_media_type(Path::new("photo.jpg")).unwrap(), "image/jpeg");
        assert_eq!(detect_media_type(Path::new("photo.jpeg")).unwrap(), "image/jpeg");
        assert_eq!(detect_media_type(Path::new("photo.JPG")).unwrap(), "image/jpeg");
        assert_eq!(detect_media_type(Path::new("photo.gif")).unwrap(), "image/gif");
        assert_eq!(detect_media_type(Path::new("photo.webp")).unwrap(), "image/webp");
        assert!(detect_media_type(Path::new("photo.bmp")).is_err());
        assert!(detect_media_type(Path::new("noext")).is_err());
    }

    #[test]
    fn parse_image_command_path_only() {
        let (path, caption) = parse_image_command("/tmp/shot.png").unwrap();
        assert_eq!(path, "/tmp/shot.png");
        assert_eq!(caption, None);
    }

    #[test]
    fn parse_image_command_with_caption() {
        let (path, caption) = parse_image_command("/tmp/shot.png What is this?").unwrap();
        assert_eq!(path, "/tmp/shot.png");
        assert_eq!(caption, Some("What is this?".to_string()));
    }

    #[test]
    fn parse_image_command_empty_is_error() {
        assert!(parse_image_command("").is_err());
        assert!(parse_image_command("   ").is_err());
    }
}
