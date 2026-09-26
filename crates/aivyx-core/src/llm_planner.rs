//! LLM-backed [`TurnPlanner`].
//!
//! [`LlmPlanner`] is the Phase 2 counterpart to Phase 1's `VecPlanner`.
//! Instead of walking a pre-recorded script, it holds an
//! `Arc<dyn LlmProvider>` (from `aivyx-llm`) and drives the turn loop
//! by asking the provider for the next step on every call.
//!
//! ## What it does, in order
//!
//! 1. `begin_turn(message)` — seeds the conversation history with a
//!    single `LlmMessage::User` containing the message text.
//! 2. `next_step(...)` — builds an `LlmRequest` from the current
//!    history + system prompt + tool descriptors, calls
//!    `provider.chat_stream(...)`, drains the mid-stream
//!    `LlmStreamEvent::TextChunk` events while relaying each to
//!    `channel.stream_event(StreamEvent::Text(chunk))`, then calls
//!    `stream.finish()` to obtain the terminal `LlmStepEnd`.
//! 3. On `LlmStepEnd::FinalMessage { text, .. }` — appends an
//!    `Assistant { text, tool_calls: [] }` entry to the history and
//!    returns [`NextStep::FinalMessage`].
//! 4. On `LlmStepEnd::ToolCall { .. }` — resolves the tool by name in
//!    the registry, appends an `Assistant { text: text_so_far,
//!    tool_calls: [record] }` entry, remembers the pending `call_id`,
//!    and returns [`NextStep::ToolCall`]. If the tool name is unknown,
//!    the planner synthesizes a `tool_result` error and recursively
//!    asks the provider for another step so the LLM can recover.
//! 5. `observe_tool_outcome(tool_id, outcome)` — consumes the pending
//!    call_id, serializes the outcome into the structured `tool_result`
//!    content (see [`render_tool_result`]), and appends it to history.
//!
//! ## Conversation-history ownership
//!
//! The planner owns the `Vec<LlmMessage>` mutably across the whole turn.
//! One planner instance = one turn — the `ConcreteAgent` factory
//! produces a fresh planner per `Agent::turn` call, so concurrent turns
//! never share history.

use std::collections::VecDeque;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;

use aivyx_kvcache::{CacheKey, CacheMeta, LlamaServerSlotStore};
use aivyx_llm::{
    ContentBlock, KvSlotPool, LlmError, LlmMessage, LlmProvider, LlmRequest, LlmStepEnd, LlmStream,
    LlmStreamEvent, LlmToolCallRecord, LlmToolDescriptor, LlmUsage, SlotHint, ToolCallEnd,
};

use crate::planner::{NextStep, StepObservation, ToolCallRequest, ToolRegistry, TurnPlanner};
use crate::{
    ChannelContext, ContentPart, Message, MessageContent, SessionId, StreamEvent, ToolId,
    ToolOutcome,
};

// ---------------------------------------------------------------------------
// PruneSink — callback for persisting pruned context
// ---------------------------------------------------------------------------

/// Receives a summary of pruned messages when context-window pruning
/// fires. Implementations live in the channel layer (which has access
/// to `Memory`); the core crate defines only the contract.
#[async_trait]
pub trait PruneSink: Send + Sync {
    /// Called once per pruning event with the number of messages
    /// removed and a human-readable summary of their content.
    async fn on_prune(&self, session_id: crate::SessionId, pruned_count: usize, summary: &str);
}

// ---------------------------------------------------------------------------
// ContextProvider — per-turn automatic recall hook (Phase 76)
// ---------------------------------------------------------------------------

/// Read-side sibling of [`PruneSink`]: invoked once per turn with the
/// user's message, returning an already-formatted context block to
/// prepend, or `None` for "nothing relevant — leave the turn
/// untouched."
///
/// The concrete implementation lives in the channel layer (which has
/// access to `Memory` + the embedding provider); the core crate
/// defines only the contract. Mirrors the `PruneSink` pattern.
///
/// Phase 76 deliberately does **not** re-export this trait from
/// `aivyx-core`'s `lib.rs` (unlike the older `PruneSink`): consumers
/// reach it via `aivyx_core::llm_planner::ContextProvider`. Keeping
/// `lib.rs` byte-identical protects the production-core streak; the
/// minor re-export asymmetry is the documented, intentional price.
#[async_trait]
pub trait ContextProvider: Send + Sync {
    /// Return a formatted, injection-safe context block to prepend to
    /// this turn, or `None` to leave the turn unchanged. Must never
    /// panic and must swallow its own errors into `None` (the
    /// universal no-op path — recall is best-effort, never fatal).
    ///
    /// Phase 77 — `session_id` is the turn's conversation id,
    /// passed so an implementation can persist a recall-feedback
    /// event correlated to the turn (the reflection loop pairs it
    /// against the audit chain's per-session `TurnEnded`). It does
    /// not influence what is recalled.
    ///
    /// Vitrine §5 follow-up — `turn_id` is the turn's audit id, so an
    /// implementation that *injects* something audit-worthy (the skill
    /// trigger injector emits `SkillInvocation`) can correlate its
    /// event with the surrounding `TurnStarted`/`TurnEnded` pair the
    /// way a tool-path event would. Pure-recall implementations
    /// ignore it.
    ///
    /// Vitrine §6 follow-up — `origin` is the turn message's
    /// [`crate::MessageOrigin`]. Instruction-bearing injectors (the
    /// skill trigger injector) must return `None` for
    /// `MessageOrigin::System` turns: routine/reflection prompts are
    /// fully engineered and a matched procedure hijacks them.
    /// Data-bearing recall ignores it.
    async fn recall(
        &self,
        user_message: &str,
        session_id: crate::SessionId,
        turn_id: crate::TurnId,
        origin: crate::MessageOrigin,
    ) -> Option<String>;

    /// Model routing Part 3b — whether this provider injects
    /// operator-private data (memory recall, knowledge derived from it).
    /// When a sensitive provider injects a block, a planner built with
    /// [`LlmPlanner::with_taint`] marks the conversation routing-tainted.
    /// Default `false` (e.g. an operator-approved skill procedure).
    fn sensitive(&self) -> bool {
        false
    }

    /// [`Self::recall`] plus whether the returned block carries
    /// sensitive data. The default pairs the block with
    /// [`Self::sensitive`]; a composite provider overrides it to report
    /// only the parts that actually injected something.
    async fn recall_with_sensitivity(
        &self,
        user_message: &str,
        session_id: crate::SessionId,
        turn_id: crate::TurnId,
        origin: crate::MessageOrigin,
    ) -> Option<(String, bool)> {
        let block = self
            .recall(user_message, session_id, turn_id, origin)
            .await?;
        Some((block, self.sensitive()))
    }
}

/// Phase 79 — per-turn system-prompt refiner. Sibling of
/// [`ContextProvider`]: invoked in `begin_turn` with the user's
/// message, it may return a replacement system prompt for *this
/// turn only*, or `None` to leave the planner's base prompt
/// untouched (the universal byte-identical fallback path).
///
/// Used by the adaptive-Persona refiner, which selects the
/// Persona facets relevant to the turn instead of injecting the
/// whole accreted Soul every time. Like every hook in this
/// module it is **not** re-exported from `aivyx-core`'s
/// `lib.rs` — consumers reach it via
/// `aivyx_core::llm_planner::SystemPromptRefiner`. Keeping
/// `lib.rs` byte-identical protects the production-core streak;
/// the minor re-export asymmetry is the documented, intentional
/// price (same rationale as `ContextProvider`).
#[async_trait]
pub trait SystemPromptRefiner: Send + Sync {
    /// Return a replacement system prompt for this turn, or
    /// `None` to keep the planner's base prompt unchanged. Must
    /// never panic and must swallow its own errors into `None`
    /// (best-effort — refinement is never fatal).
    ///
    /// Phase 86 — `session_id` is provided so implementations
    /// that consult a per-session conversational window (the
    /// recall-window-relevance work) can locate the right
    /// recent-turns buffer. Mirrors `ContextProvider::recall`,
    /// which already carries `session_id`.
    ///
    /// Phase 117 — `base_prompt` is the planner's currently
    /// configured base system prompt. Implementations that
    /// produce a refined prompt from scratch (Phase 79's
    /// adaptive Persona refiner) ignore this argument; ones
    /// that EXTEND the base (Phase 117's relevance refiner)
    /// can compose their output as `base_prompt +
    /// addendum`. The default value behaviour for callers
    /// that don't carry the base is the empty string.
    async fn refine(
        &self,
        user_message: &str,
        session_id: crate::SessionId,
        base_prompt: &str,
    ) -> Option<String>;
}

// ---------------------------------------------------------------------------
// ConversationSeeder — prior-turn history replay (Chapter Thread)
// ---------------------------------------------------------------------------

/// One prior message of the session, as recorded by the channel
/// layer's per-session conversation window. `is_user` selects the
/// replayed role; `text` is the message's final text (tool calls and
/// tool results are deliberately NOT replayed — only what the
/// operator and the assistant actually said).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PriorTurn {
    pub is_user: bool,
    pub text: String,
}

/// Chapter Thread — per-turn conversation-history seeding. Sibling of
/// [`ContextProvider`]: invoked once in `begin_turn`, it returns the
/// session's prior messages (oldest first) and the planner seeds them
/// into the turn's history as real `User`/`Assistant` messages before
/// the current user message, so the model sees the conversation the
/// operator sees. An empty vec is the universal no-op path — turns
/// stay byte-identical to the pre-Thread fresh-context behavior.
///
/// Like every hook in this module it is **not** re-exported from
/// `aivyx-core`'s `lib.rs` — consumers reach it via
/// `aivyx_core::llm_planner::ConversationSeeder` (the documented
/// `ContextProvider` precedent).
#[async_trait]
pub trait ConversationSeeder: Send + Sync {
    /// Prior messages of this session, oldest first. Must never
    /// panic and must swallow its own errors into an empty vec
    /// (seeding is best-effort, never fatal).
    async fn prior_turns(&self, session_id: crate::SessionId) -> Vec<PriorTurn>;
}

/// Normalize raw prior turns into a provider-safe message prefix:
///
/// 1. Blank entries are dropped.
/// 2. Leading assistant entries are dropped — providers require the
///    first message to be a user turn.
/// 3. Consecutive same-role entries coalesce into one message
///    (newline-joined) so strict-alternation providers never see
///    `user, user` or `assistant, assistant`.
/// 4. A trailing user entry (a prior turn whose completion was empty
///    — the window records the operator's message but skips an empty
///    assistant final) gets an honest `(no response was produced that
///    turn)` assistant filler, both to preserve alternation against
///    the current user message that follows and so the model can SEE
///    that it never answered.
fn seeded_history_messages(prior: Vec<PriorTurn>) -> Vec<LlmMessage> {
    // Steps 1–3: drop blanks + leading assistants, coalesce runs.
    let mut coalesced: Vec<PriorTurn> = Vec::with_capacity(prior.len());
    for turn in prior {
        if turn.text.trim().is_empty() {
            continue;
        }
        if coalesced.is_empty() && !turn.is_user {
            continue; // leading assistant — drop
        }
        match coalesced.last_mut() {
            Some(last) if last.is_user == turn.is_user => {
                last.text.push('\n');
                last.text.push_str(&turn.text);
            }
            _ => coalesced.push(turn),
        }
    }
    // Step 4: trailing user → alternation filler.
    if coalesced.last().is_some_and(|t| t.is_user) {
        coalesced.push(PriorTurn {
            is_user: false,
            text: "(no response was produced that turn)".to_string(),
        });
    }
    coalesced
        .into_iter()
        .map(|t| {
            if t.is_user {
                LlmMessage::User {
                    content: vec![ContentBlock::text(t.text)],
                }
            } else {
                LlmMessage::Assistant {
                    text: t.text,
                    tool_calls: Vec::new(),
                }
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// All the knobs the LLM planner needs at construction time. Separated
/// from [`LlmPlanner::new`] so callers can build one at config-parse
/// time and reuse it, and so new fields can land without churning the
/// constructor signature.
#[derive(Clone)]
pub struct LlmPlannerConfig {
    /// Provider-specific model id, e.g. `"claude-haiku-4-5-20251001"`.
    pub model: String,
    /// Optional system prompt. `None` means the provider's default
    /// (usually empty) is used.
    pub system_prompt: Option<String>,
    /// Max output tokens per step.
    pub max_tokens: u32,
    /// Optional sampling temperature; `None` means provider default.
    pub temperature: Option<f32>,
    /// Phase 11 Task 4 — role-derived tool allowlist. When `Some`,
    /// `LlmPlanner::new` filters the registry's tool list through
    /// this set before sending the catalog to the provider. The
    /// filtered-out tools are never advertised to the model, so
    /// the model never tries to call them — **this** is the
    /// primary enforcement. The dispatch-layer check in
    /// `ConcreteAgent::run_tool_call` is belt-and-suspenders for
    /// tool calls that bypass advertisement (stale tool_use
    /// blocks on resumed conversations, non-LLM planners, etc.).
    ///
    /// `None` means "no filter — advertise every registered
    /// tool," preserving Phase 6–10 behavior for planners built
    /// without a role.
    pub tool_allowlist: Option<std::collections::BTreeSet<String>>,
    /// Phase 43 Task 2 — context window size in tokens. Used by the
    /// pruning layer to decide when to drop old history messages.
    /// Defaults per provider: 200_000 (Anthropic), 128_000 (OpenAI).
    /// `None` disables pruning entirely.
    pub context_window_tokens: Option<usize>,
    /// Phase 43 Task 4 — optional callback invoked when messages are
    /// pruned. The channel layer provides an implementation backed by
    /// `Memory::put()` to persist pruned context for later reflection.
    /// `None` means pruned messages are silently discarded.
    pub prune_sink: Option<Arc<dyn PruneSink>>,
    /// Phase 76 — optional automatic-recall hook. When `Some`,
    /// `begin_turn` calls it with the user's message and prepends
    /// any returned block to that turn's context. `None` means no
    /// auto-recall (pre-Phase-76 behavior exactly).
    pub context_provider: Option<Arc<dyn ContextProvider>>,
    /// Phase 79 — optional per-turn system-prompt refiner. When
    /// `Some`, `begin_turn` calls it with the user's message and
    /// (on `Some`) swaps the system prompt for that turn. `None`
    /// means the base prompt is used unchanged (pre-Phase-79
    /// behavior exactly).
    pub system_prompt_refiner: Option<Arc<dyn SystemPromptRefiner>>,
    /// Chapter Thread — optional conversation-history seeder. When
    /// `Some`, `begin_turn` seeds the turn's history with the
    /// session's prior user/assistant messages (normalized via
    /// [`seeded_history_messages`]) before the current user message,
    /// so conversational channels see real multi-turn context. `None`
    /// means fresh-context turns (pre-Thread behavior exactly).
    pub conversation_seeder: Option<Arc<dyn ConversationSeeder>>,
    /// Phase 120 — threshold for the planner's tool-name fuzzy-
    /// match recovery. Float in `[0.0, 1.0]`. Defaults to
    /// [`FUZZY_TOOL_NAME_THRESHOLD`] (0.80, matches Phase 112's
    /// fuzzy-match default).
    ///
    /// Operators set this via `[providers]
    /// tool_name_auto_correct_threshold = ...` in `aivyx-pa.toml`;
    /// the binary plumbs it through to this field at planner-
    /// construction time.
    ///
    /// `0.0` → every Unknown name matches (the planner picks the
    /// first registered tool — effectively garbage out).
    /// `1.0` → only exact-token-set matches (preserves the
    /// pre-Phase-120 unknown-tool error path).
    pub tool_name_auto_correct_threshold: f32,
}

impl std::fmt::Debug for LlmPlannerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmPlannerConfig")
            .field("model", &self.model)
            .field("system_prompt", &self.system_prompt)
            .field("max_tokens", &self.max_tokens)
            .field("temperature", &self.temperature)
            .field("tool_allowlist", &self.tool_allowlist)
            .field("context_window_tokens", &self.context_window_tokens)
            .field("prune_sink", &self.prune_sink.as_ref().map(|_| ".."))
            .field(
                "context_provider",
                &self.context_provider.as_ref().map(|_| ".."),
            )
            .field(
                "system_prompt_refiner",
                &self.system_prompt_refiner.as_ref().map(|_| ".."),
            )
            .field(
                "conversation_seeder",
                &self.conversation_seeder.as_ref().map(|_| ".."),
            )
            .finish()
    }
}

impl LlmPlannerConfig {
    pub fn new(model: impl Into<String>) -> Self {
        LlmPlannerConfig {
            model: model.into(),
            system_prompt: None,
            max_tokens: 1024,
            temperature: None,
            tool_allowlist: None,
            context_window_tokens: None,
            prune_sink: None,
            context_provider: None,
            system_prompt_refiner: None,
            conversation_seeder: None,
            // Phase 120 — same default as the FUZZY_TOOL_NAME_THRESHOLD
            // const used at Task 4. Operators override via TOML.
            tool_name_auto_correct_threshold: FUZZY_TOOL_NAME_THRESHOLD,
        }
    }

    /// Phase 120 — override the tool-name fuzzy-match threshold.
    /// Caller is responsible for clamping into `[0.0, 1.0]` —
    /// the config layer (`aivyx-config`) rejects out-of-range
    /// values at TOML-parse time so the planner never sees a
    /// malformed value in practice.
    pub fn with_tool_name_auto_correct_threshold(mut self, threshold: f32) -> Self {
        self.tool_name_auto_correct_threshold = threshold;
        self
    }

    pub fn with_system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(prompt.into());
        self
    }

    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    pub fn with_temperature(mut self, temperature: f32) -> Self {
        self.temperature = Some(temperature);
        self
    }

    /// Set the context window size in tokens. When set, the planner
    /// prunes old history messages before each LLM call if the
    /// estimated token count exceeds 80% of this value.
    pub fn with_context_window(mut self, tokens: usize) -> Self {
        self.context_window_tokens = Some(tokens);
        self
    }

    /// Attach a prune sink that receives summaries of pruned messages
    /// for persistence (e.g. to memory). See [`PruneSink`].
    pub fn with_prune_sink(mut self, sink: Arc<dyn PruneSink>) -> Self {
        self.prune_sink = Some(sink);
        self
    }

    /// Attach a [`ContextProvider`] for per-turn automatic recall.
    /// `None` (the default) preserves pre-Phase-76 behavior. Mirrors
    /// [`Self::with_prune_sink`].
    pub fn with_context_provider(mut self, provider: Arc<dyn ContextProvider>) -> Self {
        self.context_provider = Some(provider);
        self
    }

    /// Phase 79 — attach a per-turn [`SystemPromptRefiner`].
    /// Mirrors [`Self::with_context_provider`]; `None` (the
    /// default) preserves pre-Phase-79 behavior.
    pub fn with_system_prompt_refiner(mut self, refiner: Arc<dyn SystemPromptRefiner>) -> Self {
        self.system_prompt_refiner = Some(refiner);
        self
    }

    /// Chapter Thread — attach a [`ConversationSeeder`] for prior-turn
    /// history replay. Mirrors [`Self::with_context_provider`]; `None`
    /// (the default) preserves fresh-context turns exactly.
    pub fn with_conversation_seeder(mut self, seeder: Arc<dyn ConversationSeeder>) -> Self {
        self.conversation_seeder = Some(seeder);
        self
    }

    /// Attach a role-derived tool allowlist. See
    /// [`Self::tool_allowlist`] for semantics. `None` preserves
    /// legacy behavior (allow all registered tools).
    pub fn with_tool_allowlist(
        mut self,
        allowlist: Option<std::collections::BTreeSet<String>>,
    ) -> Self {
        self.tool_allowlist = allowlist;
        self
    }
}

// ---------------------------------------------------------------------------
// Planner
// ---------------------------------------------------------------------------

/// LLM-backed [`TurnPlanner`]. Built once per turn — the planner factory
/// on [`crate::ConcreteAgent`] constructs a fresh instance whose
/// conversation history starts empty.
pub struct LlmPlanner {
    provider: Arc<dyn LlmProvider>,
    registry: Arc<ToolRegistry>,
    config: LlmPlannerConfig,
    tools: Vec<LlmToolDescriptor>,
    history: Vec<LlmMessage>,
    pending_call_ids: VecDeque<String>,
    /// POLISH_WAVES.md sub-project 4, item A — `(tool_id, count)` of the
    /// current CONSECUTIVE-failure streak for one tool. `None` when the
    /// last observed outcome was a success, or no outcome has been
    /// observed yet. A different tool's failure resets the streak to
    /// that tool rather than accumulating across tools.
    consecutive_tool_failures: Option<(ToolId, usize)>,
    /// Cumulative token usage across all LLM steps in this turn.
    accumulated_usage: crate::TokenUsage,
    /// Running count of messages pruned during this turn for context
    /// window management (Phase 43).
    pruned_message_count: usize,
    /// Index into `history` of the current turn's task message — the
    /// user message `begin_turn` pushed. Pruning drops oldest-first
    /// and the task is the oldest turn-local message, so a fat tool
    /// turn used to discard its own question (live rig 2026-07-05:
    /// the model, left with seven tool results and no task, reset to
    /// a greeter reply and made off-task persona-flavored tool
    /// calls). The pruner now pins this message: if it falls inside
    /// the pruned prefix it is re-inserted right after the sentinel.
    task_message_index: Option<usize>,
    /// `None` unless `with_kv_cache` was called (only ever true when
    /// `[agent] provider = "llama_cpp"`) -- every other code path this
    /// task adds is a complete no-op when this is `None`.
    kv_cache: Option<KvCacheConfig>,
    /// The slot id checked out from `kv_cache`'s pool, set as soon as
    /// `checkout()` succeeds in `begin_turn` (before any `.await` point),
    /// not only on a fully successful warm-up/restore -- so `Drop` can
    /// always release it even if the rest of `begin_turn`'s async work
    /// never completes (e.g. the turn future is dropped mid-warm-up).
    kv_slot_id: Option<u32>,
    /// `true` only when `with_broker_slot_hint` was called (only ever
    /// true when `[agent] provider = "broker"`). When set, every
    /// outgoing `LlmRequest` carries `slot_hint: Some(SlotHint { .. })`
    /// instead of a raw `id_slot` pin -- `aivyx-broker` owns the full
    /// checkout/restore/warm/save lifecycle server-side in this mode, so
    /// this planner never calls `with_kv_cache` alongside it, and
    /// `ensure_kv_slot_checked_out` (gated on `kv_cache.is_some()`)
    /// stays a complete no-op the whole turn.
    broker_slot_hint: bool,
    /// `None` unless `with_routing` was called (only for the daemon's
    /// conversational planners, and only when `[routing]` is configured).
    /// When set, the main request carries a `RouteHint` with this task
    /// kind, and each step's usage is priced against the model the
    /// router actually used.
    routing: Option<(Arc<aivyx_llm::RoutedProvider>, aivyx_route::TaskKind)>,
    /// The router's stickiness, taint and consent key for this turn — the
    /// conversation from `set_conversation` (the channel's session, the
    /// key the agent taints under), else the turn message's session id;
    /// set in `begin_turn`.
    route_session: Option<String>,
    /// The conversation from `TurnPlanner::set_conversation`, if the turn
    /// loop supplied one.
    conversation: Option<SessionId>,
    /// Per-model token usage for this turn, one entry per model that
    /// served a step (see `TurnPlanner::turn_costs`).
    turn_costs: Vec<(String, crate::TokenUsage)>,
    /// Model routing Part 3b — `None` unless `with_taint` was called (the
    /// daemon's conversational planners, only when cloud escalation is
    /// active). When set, a sensitive `ContextProvider`'s injection marks
    /// the turn's session routing-tainted.
    taint: Option<Arc<dyn crate::TaintSink>>,
}

struct KvCacheConfig {
    pool: Arc<KvSlotPool>,
    store: Arc<LlamaServerSlotStore>,
    backend_id: String,
    model_id: String,
    build_hash: String,
}

/// Bounds `ensure_kv_slot_checked_out`'s three I/O calls: restore,
/// warm-up (`chat_stream` + its full drain), and save.
/// Deliberately not `aivyx_llm::KVCACHE_PROBE_TIMEOUT` (3s) -- that
/// constant budgets a `/props` HTTP metadata fetch, not a real LLM
/// generation call; a slow-but-healthy local model warming a fresh
/// slot can legitimately take longer than 3s. Without *some* bound
/// here, a wedged llama-server stalls the whole turn indefinitely:
/// this runs inside `begin_turn`, before `ConcreteAgent::turn()`'s own
/// wall-clock deadline task is even spawned, and the production
/// transport has no HTTP timeout by design.
const KVCACHE_WARM_UP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// POLISH_WAVES.md sub-project 4, item A — after this many CONSECUTIVE
/// failures of the same tool, `observe_tool_outcome` appends a one-shot
/// advisory to the tool result telling the model to stop retrying.
/// Bridle's own breaker (`aivyx-core/src/agent.rs`) only catches
/// consecutive IDENTICAL calls (same tool_id + same input); a model that
/// varies its arguments each retry never trips it, so this is a
/// deliberately separate, differently-keyed mechanism.
const TOOL_FAILURE_NUDGE_THRESHOLD: usize = 3;

/// The literal marker `observe_tool_outcome` appends the tool-failure
/// nudge after. Shared with `tool_result_texts`, which strips
/// everything from this marker onward before returning tool-result
/// text as a "source of truth" pool — the nudge is Aivyx's own
/// injected scaffolding, not tool-provided data (final-review fix,
/// POLISH_WAVES.md sub-project 4).
const TOOL_FAILURE_NUDGE_MARKER: &str = "\n\n[SYSTEM NOTE:";

// ---------------------------------------------------------------------------
// One-shot-per-failure-class warning latches (final-review Fix 2).
// ---------------------------------------------------------------------------
//
// Without these, a persistently misconfigured backend (e.g. `llama-server`
// started without `--slot-save-path`) reprints the identical
// `ensure_kv_slot_checked_out` warning on every single turn for the life
// of the process -- exactly the "warning spam loop" the kvcache design
// doc called out as something to avoid ("not a warning spam loop per
// turn (rate-limited or one-shot ... )"). Each function below guards its
// own `eprintln!` with a private `OnceLock<()>`, so it prints the first
// time this failure class is hit in this process's lifetime and stays
// silent after that. Deliberately coarse: one latch per distinct failure
// *class*, not truly "once ever" across every possible message variant
// (a later call with a different underlying error still prints only the
// first one observed) -- matches the design doc's own "one-shot"
// framing. Mirrors `tools/git.rs`'s `should_warn_once` latch shape
// (a `OnceLock`-backed guard rather than printing every time),
// simplified here since these classes are a small fixed set rather than
// a per-repo key.

fn warn_pool_full() {
    static WARNED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    WARNED.get_or_init(|| {
        eprintln!("aivyx-pa: kvcache: no free slot in the pool; this turn runs unpinned");
    });
}

fn warn_restore_failed(err: &dyn std::fmt::Display) {
    static WARNED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    WARNED.get_or_init(|| {
        eprintln!("aivyx-pa: kvcache: restore_into_slot failed: {err}");
    });
}

fn warn_restore_timed_out() {
    static WARNED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    WARNED.get_or_init(|| {
        eprintln!("aivyx-pa: kvcache: restore_into_slot timed out; treating as a miss");
    });
}

fn warn_warm_up_request_failed(err: &dyn std::fmt::Display) {
    static WARNED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    WARNED.get_or_init(|| {
        eprintln!("aivyx-pa: kvcache: warm-up request failed: {err}");
    });
}

fn warn_warm_up_timed_out() {
    static WARNED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    WARNED.get_or_init(|| {
        eprintln!(
            "aivyx-pa: kvcache: warm-up timed out after {KVCACHE_WARM_UP_TIMEOUT:?}; \
             skipping save so a partial/corrupt slot is never recorded as a \
             valid cache entry"
        );
    });
}

fn warn_warm_up_stream_errored() {
    static WARNED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    WARNED.get_or_init(|| {
        eprintln!(
            "aivyx-pa: kvcache: warm-up stream errored mid-response; \
             skipping save so a partial/corrupt slot is never \
             recorded as a valid cache entry"
        );
    });
}

fn warn_save_failed(err: &dyn std::fmt::Display) {
    static WARNED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    WARNED.get_or_init(|| {
        eprintln!("aivyx-pa: kvcache: save_from_slot failed: {err}");
    });
}

fn warn_save_timed_out() {
    static WARNED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    WARNED.get_or_init(|| {
        eprintln!("aivyx-pa: kvcache: save_from_slot timed out");
    });
}

impl LlmPlanner {
    pub fn new(
        provider: Arc<dyn LlmProvider>,
        registry: Arc<ToolRegistry>,
        config: LlmPlannerConfig,
    ) -> Self {
        // Phase 11 Task 4 — role-allowlist filter on the advertised
        // tool catalog. When `config.tool_allowlist` is `Some`,
        // tools whose name is not in the set are not collected
        // into the descriptor list, so the provider request
        // (`request.tools`) never mentions them and the model
        // therefore never emits a tool_use block against them.
        // This is the primary enforcement point for the role
        // allowlist; see the dispatch-layer check in
        // `agent.rs::run_tool_call` for the belt-and-suspenders
        // safety net.
        let tools = registry
            .snapshot()
            .into_iter()
            .filter(|tool| {
                config
                    .tool_allowlist
                    .as_ref()
                    .is_none_or(|set| set.contains(tool.name()))
            })
            .map(|tool| LlmToolDescriptor {
                name: tool.name().to_string(),
                description: tool.description().to_string(),
                input_schema: tool.input_schema().clone(),
            })
            .collect();

        LlmPlanner {
            provider,
            registry,
            config,
            tools,
            history: Vec::new(),
            pending_call_ids: VecDeque::new(),
            consecutive_tool_failures: None,
            accumulated_usage: crate::TokenUsage::default(),
            pruned_message_count: 0,
            task_message_index: None,
            kv_cache: None,
            kv_slot_id: None,
            broker_slot_hint: false,
            routing: None,
            route_session: None,
            conversation: None,
            turn_costs: Vec::new(),
            taint: None,
        }
    }

    /// Opts this `LlmPlanner` into KV-cache persistence against a
    /// llama-server backend. Only ever called by whoever builds this
    /// planner's factory closure when `[agent] provider = "llama_cpp"`
    /// -- every other provider never calls this, and every code path
    /// this enables is a complete no-op otherwise.
    pub fn with_kv_cache(
        mut self,
        pool: Arc<KvSlotPool>,
        store: Arc<LlamaServerSlotStore>,
        backend_id: String,
        model_id: String,
        build_hash: String,
    ) -> Self {
        // `with_kv_cache` and `with_broker_slot_hint` are mutually
        // exclusive by construction (llama-server-local-kvcache mode and
        // aivyx-broker mode never coexist for the same planner) -- this
        // is a cheap invariant check, not a behavior change, to catch a
        // future caller that ever combines the two.
        debug_assert!(
            !self.broker_slot_hint,
            "with_kv_cache called on a planner already in broker_slot_hint mode"
        );
        self.kv_cache = Some(KvCacheConfig {
            pool,
            store,
            backend_id,
            model_id,
            build_hash,
        });
        self
    }

    /// Opts this `LlmPlanner` into `aivyx-broker` slot-hint mode. Only
    /// ever called by whoever builds this planner's factory closure when
    /// `[agent] provider = "broker"` -- mutually exclusive with
    /// `with_kv_cache` in practice (the two are never called on the same
    /// planner: the broker owns slot admission and the restore/warm/save
    /// lifecycle itself, so this planner must never also run its own
    /// local `KvSlotPool` checkout for the same turn). Every outgoing
    /// `LlmRequest` this planner builds after this call carries
    /// `slot_hint: Some(SlotHint { prefix_hash, preferred_slot })`
    /// instead of a raw `id_slot` pin.
    pub fn with_broker_slot_hint(mut self) -> Self {
        // Symmetric invariant check to the one in `with_kv_cache` above.
        debug_assert!(
            self.kv_cache.is_none(),
            "with_broker_slot_hint called on a planner that already has kv_cache set"
        );
        self.broker_slot_hint = true;
        self
    }

    /// Tags this planner's main LLM request for model routing: every step
    /// carries a `RouteHint` for `task`, sticky per conversation (the turn
    /// message's session id). `router` must be the `RoutedProvider` this
    /// planner's provider dispatches through — its last decision for the
    /// session names the model each step's usage is priced against. Only
    /// the daemon's conversational planners call this; the KV warm-up
    /// request stays untagged.
    pub fn with_routing(
        mut self,
        router: Arc<aivyx_llm::RoutedProvider>,
        task: aivyx_route::TaskKind,
    ) -> Self {
        self.routing = Some((router, task));
        self
    }

    /// Model routing Part 3b — mark the turn's session routing-tainted
    /// (reason `"memory recall"`) when a [`ContextProvider`] whose
    /// [`ContextProvider::sensitive`] is `true` injects a non-empty block
    /// in `begin_turn`. Not attaching one (the default) runs no taint
    /// machinery.
    pub fn with_taint(mut self, sink: Arc<dyn crate::TaintSink>) -> Self {
        self.taint = Some(sink);
        self
    }

    /// Checks out a slot and either restores a previously-saved matching
    /// prefix into it, or warms it fresh with exactly this turn's stable
    /// prefix (system prompt + tool defs -- never `self.history`, which
    /// may hold real prior conversation seeded by a `ConversationSeeder`)
    /// and saves it for future turns. Every failure mode past the
    /// pool-checkout itself is fail-open: logged, the turn simply runs
    /// with an un-warmed (but still correctly pool-owned) slot. Called
    /// once, from `begin_turn`, before anything touches `self.history`.
    ///
    /// Idempotent: a no-op if a slot was already checked out earlier
    /// this turn (guards against a second `begin_turn` call on the same
    /// planner leaking the first slot -- `next_step`'s own defensive
    /// "history is empty" branch shows `begin_turn` isn't guaranteed
    /// exactly-once by every caller).
    ///
    /// Skips kvcache entirely (no checkout at all) when
    /// `config.system_prompt_refiner` is set: the refiner rewrites
    /// `self.config.system_prompt` *after* this method returns (see
    /// `begin_turn`'s later Phase 79 step), so warming/keying here would
    /// use the pre-refined prompt and every future restore would
    /// mismatch and fall back to cold, forever, silently defeating the
    /// feature. Running this method after the refiner instead isn't
    /// safe either -- the refiner's output derives from the user's real
    /// message, so persisting a warm-up built from it would leak real
    /// per-session content to disk. The two features are fundamentally
    /// in tension; a refiner-enabled planner just never gets kvcache
    /// rather than getting a silently-broken version of it.
    async fn ensure_kv_slot_checked_out(&mut self) {
        if self.kv_slot_id.is_some() {
            return; // already checked out earlier this turn
        }
        let Some(kv) = &self.kv_cache else {
            return; // kvcache not configured for this planner
        };
        if self.config.system_prompt_refiner.is_some() {
            return; // see this method's doc comment -- refiner + kvcache are in tension
        }
        let Some(slot_id) = kv.pool.checkout() else {
            // No `tracing` dependency in this crate (see `tools/git.rs`'s
            // `confiner_for` doc comment) -- `eprintln!` matches the rest
            // of `aivyx-pa`'s operator-facing warning convention. Rate-limited
            // (see the one-shot warning latches above `impl LlmPlanner`)
            // so a persistently-full pool doesn't spam this line every
            // single turn forever.
            warn_pool_full();
            return;
        };
        self.kv_slot_id = Some(slot_id);

        let key = CacheKey {
            backend_id: kv.backend_id.clone(),
            model_id: kv.model_id.clone(),
            build_hash: kv.build_hash.clone(),
            prefix_hash: compute_prefix_hash(self.config.system_prompt.as_deref(), &self.tools),
        };

        // This same process may have already loaded exactly this prefix
        // into this exact slot -- e.g. `aivyx-pa`'s daemon builds a fresh
        // `LlmPlanner` every turn, but `KvSlotPool` itself is long-lived
        // for the process, so a later turn pinned back onto the same
        // slot id can find its own earlier work still physically live in
        // llama-server's GPU memory. Restoring here would silently
        // overwrite that live conversation KV state with the frozen
        // prefix-only snapshot saved at warm-up time -- a correctness
        // regression, not just wasted work. Skip straight to pinning
        // `id_slot` for the real turn in that case.
        if kv.pool.last_loaded_prefix(slot_id).as_deref() == Some(key.prefix_hash.as_str()) {
            return;
        }

        let restored = match tokio::time::timeout(
            KVCACHE_WARM_UP_TIMEOUT,
            kv.store.restore_into_slot(&key, slot_id),
        )
        .await
        {
            Ok(Ok(true)) => true,
            Ok(Ok(false)) => false,
            Ok(Err(err)) => {
                warn_restore_failed(&err);
                false
            }
            Err(_elapsed) => {
                warn_restore_timed_out();
                false
            }
        };

        if restored {
            // Record so a later checkout of this same slot for this same
            // prefix (still live from this restore) can skip the redundant
            // restore above.
            kv.pool
                .record_loaded_prefix(slot_id, key.prefix_hash.clone());
        }

        if !restored {
            // Cold (or a stale/rejected restore -- Manifest::insert is an
            // upsert, so this cleanly repairs a stuck-cold key too):
            // warm the slot with exactly the stable prefix, save it, then
            // proceed. The warm-up goes through the *same* provider the
            // real turn uses (not a raw HTTP call) so its tokenization
            // matches exactly -- a mismatch here silently defeats
            // automatic reuse. The trailing empty User message is
            // required, not decorative: confirmed live against a real
            // llama-server (Qwen3.5's chat template) that a
            // system-message-only request is REJECTED outright (400, "No
            // user query found in messages"). `model` is
            // `self.config.model`, not `kv.model_id` -- the latter is
            // reserved for `CacheKey` only, so the two can never
            // silently diverge from whatever the real turn's own
            // request sends.
            let warm_up_messages: Vec<LlmMessage> = vec![LlmMessage::user_text("")];
            let warm_up_request = LlmRequest {
                model: self.config.model.as_str(),
                system: self.config.system_prompt.as_deref(),
                messages: &warm_up_messages,
                tools: &self.tools,
                max_tokens: 1,
                temperature: None,
                id_slot: Some(slot_id),
                // This warm-up path only ever runs when `kv_cache` is
                // configured, which is never true alongside
                // `broker_slot_hint` (see that field's doc comment) --
                // always `None` here.
                slot_hint: None,
                route: None,
            };
            let cancellation = crate::CancellationToken::new();
            let provider = self.provider.clone();
            // The *entire* round trip -- chat_stream's own return AND
            // fully draining the resulting stream -- is bounded by one
            // timeout, not just the initial call: a wedged llama-server
            // can stall indefinitely either before responding at all or
            // mid-stream, and this runs before `ConcreteAgent::turn()`'s
            // own wall-clock deadline task is even spawned.
            let warm_up_ok = tokio::time::timeout(KVCACHE_WARM_UP_TIMEOUT, async {
                match provider.chat_stream(warm_up_request, &cancellation).await {
                    Ok(mut stream) => loop {
                        match stream.next_event().await {
                            Ok(Some(_)) => {}
                            Ok(None) => break true,
                            Err(_) => {
                                warn_warm_up_stream_errored();
                                break false;
                            }
                        }
                    },
                    Err(err) => {
                        warn_warm_up_request_failed(&err);
                        false
                    }
                }
            })
            .await;

            match warm_up_ok {
                Ok(true) => {
                    // Record as soon as the warm-up itself succeeds, not
                    // conditioned on the save below: the warm-up is what
                    // actually loaded this prefix into the slot's live
                    // GPU state on llama-server, so a later checkout of
                    // this same slot for this same prefix can skip a
                    // redundant restore/warm-up regardless of whether
                    // the on-disk save (purely for persistence across
                    // process restarts) succeeds. Recording this only
                    // inside the save's success arm would mean a failed
                    // or timed-out save leaves nothing recorded, so the
                    // very next checkout re-runs the warm-up -- sending
                    // a system-only prompt that TRUNCATES the slot's KV
                    // back to just the prefix and destroying whatever
                    // the intervening turns had built up.
                    kv.pool
                        .record_loaded_prefix(slot_id, key.prefix_hash.clone());

                    let meta = CacheMeta {
                        size_bytes: 1,
                        token_count: 1,
                    };
                    // Bounded like restore_into_slot above and the
                    // chat_stream+drain round trip: this POSTs to
                    // llama-server (serializing a full KV slot to disk)
                    // on a client with no HTTP timeout, before
                    // ConcreteAgent::turn()'s own deadline is armed --
                    // the most likely of the three calls to wedge.
                    match tokio::time::timeout(
                        KVCACHE_WARM_UP_TIMEOUT,
                        kv.store.save_from_slot(&key, slot_id, meta),
                    )
                    .await
                    {
                        Ok(Ok(())) => {}
                        Ok(Err(err)) => {
                            warn_save_failed(&err);
                        }
                        Err(_elapsed) => {
                            warn_save_timed_out();
                        }
                    }
                }
                Ok(false) => {} // already logged inside the timed block above
                Err(_elapsed) => {
                    warn_warm_up_timed_out();
                }
            }
        }
    }

    /// Inspect the conversation history. Test-only: the planner owns
    /// the history internally, but tests need to assert on its contents
    /// after tool observations.
    pub fn history(&self) -> &[LlmMessage] {
        &self.history
    }

    /// Number of messages pruned from conversation history during this
    /// turn to stay within the context window budget (Phase 43).
    pub fn pruned_message_count(&self) -> usize {
        self.pruned_message_count
    }

    /// Names of tools actually advertised to the provider — i.e. the
    /// post-filter catalog after `config.tool_allowlist` is applied.
    /// Tests assert on this to confirm the planner-layer allowlist
    /// filter is the *primary* enforcement point for Phase 11 roles
    /// (the dispatch-layer gate in `ConcreteAgent::run_tool_call` is
    /// the belt-and-suspenders). Returns names in registry iteration
    /// order.
    pub fn advertised_tool_names(&self) -> Vec<&str> {
        self.tools.iter().map(|t| t.name.as_str()).collect()
    }

    /// The model that served the latest step: the router's last decision
    /// for this session when the step was routed, else the configured
    /// model. A step over a history `RoutedProvider` won't route (a PDF)
    /// went to the configured model even though the request was tagged —
    /// and `last_decision` still names an earlier turn's model.
    fn served_model(&self) -> String {
        self.routing
            .as_ref()
            .filter(|_| aivyx_llm::is_routable(&self.history))
            // `last_served`, not the local router's last decision: it also
            // sees calls escalated to a cloud model (Part 3b).
            .and_then(|(r, _)| r.last_served(self.route_session.as_deref()?))
            .map(|key| key.id)
            .unwrap_or_else(|| self.config.model.clone())
    }

    /// Add a step's usage to the running total, and to the per-model
    /// total of the model that served the step — the routed provider's
    /// last-served model for this session when routed, else the
    /// configured model.
    fn accumulate(&mut self, usage: LlmUsage) {
        self.accumulated_usage.input_tokens += usage.input_tokens;
        self.accumulated_usage.output_tokens += usage.output_tokens;
        self.accumulated_usage.cache_creation_input_tokens += usage.cache_creation_input_tokens;
        self.accumulated_usage.cache_read_input_tokens += usage.cache_read_input_tokens;

        let served = self.served_model();
        let step = crate::TokenUsage::from(usage);
        match self
            .turn_costs
            .iter_mut()
            .find(|(model, _)| *model == served)
        {
            Some((_, total)) => {
                total.input_tokens += step.input_tokens;
                total.output_tokens += step.output_tokens;
                total.cache_creation_input_tokens += step.cache_creation_input_tokens;
                total.cache_read_input_tokens += step.cache_read_input_tokens;
            }
            None => self.turn_costs.push((served, step)),
        }
    }

    /// The context a routed request needs, in tokens (chars / 4): the
    /// history, the system prompt, the tool catalog as sent, and room
    /// for the reply.
    fn route_estimate(&self) -> u32 {
        let system = self.config.system_prompt.as_deref().map_or(0, str::len);
        let tools = serde_json::to_string(&self.tools).map_or(0, |json| json.len());
        let prompt = aivyx_llm::estimate_tokens(&self.history) + (system + tools) / 4;
        (prompt as u32).saturating_add(self.config.max_tokens)
    }

    /// Build one `LlmRequest` from the current history + config and
    /// drain the provider's stream, returning the terminal value.
    /// Relays every `TextChunk` to the channel as a `StreamEvent::Text`.
    async fn one_step(&self, channel: &dyn ChannelContext) -> Result<LlmStepEnd, LlmError> {
        // Broker mode (`broker_slot_hint`) attaches `slot_hint` instead
        // of pinning `id_slot` directly -- `aivyx-broker` picks the
        // physical slot itself, server-side, using this as a
        // cache-locality hint. `self.kv_slot_id` stays `None` the whole
        // turn in broker mode (this planner never calls `with_kv_cache`
        // alongside `with_broker_slot_hint`, so `ensure_kv_slot_checked_out`
        // never runs), so `id_slot` below is always `None` here too.
        let slot_hint = self.broker_slot_hint.then(|| SlotHint {
            prefix_hash: compute_prefix_hash(self.config.system_prompt.as_deref(), &self.tools),
            preferred_slot: self.kv_slot_id,
        });
        let request = LlmRequest {
            model: self.config.model.as_str(),
            system: self.config.system_prompt.as_deref(),
            messages: &self.history,
            tools: &self.tools,
            max_tokens: self.config.max_tokens,
            temperature: self.config.temperature,
            id_slot: self.kv_slot_id,
            slot_hint,
            // Tagged only when `with_routing` was called (the daemon's
            // conversational planners); otherwise untagged, per the
            // "untagged ⇒ unchanged" compatibility invariant.
            route: self.routing.as_ref().map(|(_, task)| aivyx_llm::RouteHint {
                task: task.clone(),
                session: self.route_session.clone(),
                estimated_prompt_tokens: self.route_estimate(),
            }),
        };

        let cancellation = channel.cancellation_token();
        let mut stream: Box<dyn LlmStream> =
            self.provider.chat_stream(request, &cancellation).await?;

        // Race the stream's next event against cancellation. When the
        // cancel future wins, we drop the stream immediately (dropping
        // a Box<dyn LlmStream> propagates through the provider's
        // internal body stream and aborts the underlying connection)
        // and return `LlmError::Cancelled`. The turn loop's own
        // post-next_step cancellation check then takes over and emits
        // `LoopOutcome::Cancelled` / `LoopOutcome::TimedOut` as
        // appropriate. Phase 3 task 4 added this path so wall-clock
        // timeouts actually interrupt a completion mid-token.
        loop {
            let next = tokio::select! {
                biased;
                _ = cancellation.cancelled() => {
                    drop(stream);
                    return Err(LlmError::Cancelled);
                }
                event = stream.next_event() => event?,
            };

            let Some(event) = next else { break };

            if let LlmStreamEvent::TextChunk(ref chunk) = event {
                // Best-effort relay: if the channel rejects the event,
                // we log it in the sense of "drop it on the floor" —
                // the LLM stream still has to be drained or we leak the
                // outbound connection. Channel errors are terminal for
                // the turn loop, not for the stream.
                let _ = channel.stream_event(StreamEvent::Text(chunk)).await;
            }
            // `Usage` events are currently ignored — usage is carried
            // on the terminal value. This matches the provider's
            // documented contract.
        }

        stream.finish().await
    }

    /// Phase 126 — per-call dispatch helper extracted from the
    /// Phase 120 / 101 inline loop. Resolves the tool name
    /// (with fuzzy-match recovery if unknown), validates the
    /// input schema (if repair budget remains), and either
    /// returns a `ToolCallRequest` to batch or surfaces an
    /// error result into history.
    ///
    /// `extracted_from_text` threads into the returned request's
    /// audit-trail field: `None` for protocol-channel calls;
    /// `Some(wrapper_tag)` for Phase 126 text-extracted calls.
    /// The bool in the return tuple is `true` when this call
    /// emitted an `invalid_input` repair result (the caller
    /// uses it to advance the repair-rounds counter).
    fn process_one_call(
        &mut self,
        call: ToolCallEnd,
        extracted_from_text: Option<String>,
        validate_enabled: bool,
    ) -> (Option<ToolCallRequest>, bool) {
        let resolution = self.registry.find_by_name(&call.tool_name);
        let (tool_id, auto_corrected_from) = match resolution {
            Some(id) => (id, None),
            None => match fuzzy_recover_tool_name(
                &self.registry,
                &call.tool_name,
                self.config.tool_name_auto_correct_threshold,
            ) {
                Some(matched_id) => (matched_id, Some(call.tool_name.clone())),
                None => {
                    let suggestions = top_n_similar_tools(&self.registry, &call.tool_name, 3);
                    let message = build_unknown_tool_message(&call.tool_name, &suggestions);
                    let mut body = json!({
                        "error": "unknown_tool",
                        "message": message,
                    });
                    if !suggestions.is_empty() {
                        body["did_you_mean"] = json!(
                            suggestions
                                .iter()
                                .map(|(n, _)| n.clone())
                                .collect::<Vec<_>>()
                        );
                    }
                    self.history.push(LlmMessage::ToolResult {
                        call_id: call.call_id,
                        content: body.to_string(),
                        is_error: true,
                    });
                    return (None, false);
                }
            },
        };

        if validate_enabled {
            if let Some(tool) = self.registry.get(tool_id) {
                if let Err(summary) = validate_tool_input(tool.input_schema(), &call.input) {
                    let schema = tool.input_schema().clone();
                    self.history.push(LlmMessage::ToolResult {
                        call_id: call.call_id,
                        content: json!({
                            "error": "invalid_input",
                            "message": summary,
                            "expected_schema": schema,
                        })
                        .to_string(),
                        is_error: true,
                    });
                    return (None, true);
                }
            }
        }
        self.pending_call_ids.push_back(call.call_id);
        (
            Some(ToolCallRequest {
                tool_id,
                input: call.input,
                auto_corrected_from,
                extracted_from_text,
            }),
            false,
        )
    }
}

#[async_trait]
impl TurnPlanner for LlmPlanner {
    fn set_conversation(&mut self, session: SessionId) {
        self.conversation = Some(session);
    }

    async fn begin_turn(&mut self, message: &Message, turn_id: crate::TurnId) {
        let conversation = self.conversation.unwrap_or(message.session_id);
        self.route_session = Some(conversation.to_string());
        self.ensure_kv_slot_checked_out().await;
        let mut content = match &message.content {
            MessageContent::Text(text) => vec![ContentBlock::text(text)],
            MessageContent::Image { media_type, data } => {
                vec![ContentBlock::image_from_bytes(media_type, data)]
            }
            MessageContent::Document { media_type, data } => {
                vec![ContentBlock::document_from_bytes(media_type, data)]
            }
            MessageContent::Mixed(parts) => parts
                .iter()
                .map(|part| match part {
                    ContentPart::Text(text) => ContentBlock::text(text),
                    ContentPart::Image { media_type, data } => {
                        ContentBlock::image_from_bytes(media_type, data)
                    }
                    ContentPart::Document { media_type, data } => {
                        ContentBlock::document_from_bytes(media_type, data)
                    }
                })
                .collect(),
        };

        // Phase 76 — automatic recall. Embed-and-retrieve is driven
        // by the user's *text* (Q2a: latest user message only). The
        // returned block is prepended as a distinct leading text
        // block *inside the same user message* rather than as its
        // own message: a separate message would risk provider
        // role-alternation rules, and folding it into the static
        // system prompt would make per-turn recall look like a
        // standing instruction. The provider's `recall` is
        // best-effort — a `None` (no provider, embed failure, empty
        // index, all-below-floor) leaves the turn byte-identical to
        // pre-Phase-76 behavior.
        // Compute the user's text once — both the Phase 76
        // recall hook and the Phase 79 system-prompt refiner key
        // off it, and they are independently configured.
        let query_text = match &message.content {
            MessageContent::Text(text) => text.clone(),
            MessageContent::Image { .. } => String::new(),
            MessageContent::Document { .. } => String::new(),
            MessageContent::Mixed(parts) => {
                let joined: Vec<&str> = parts
                    .iter()
                    .filter_map(|p| match p {
                        ContentPart::Text(t) => Some(t.as_str()),
                        ContentPart::Image { .. } => None,
                        ContentPart::Document { .. } => None,
                    })
                    .collect();
                joined.join(" ")
            }
        };
        let has_query = !query_text.trim().is_empty();

        if let Some(provider) = &self.config.context_provider {
            if has_query {
                if let Some((block, sensitive)) = provider
                    .recall_with_sensitivity(
                        &query_text,
                        message.session_id,
                        turn_id,
                        message.origin,
                    )
                    .await
                {
                    // Model routing Part 3b — operator-private recall taints
                    // the conversation before any model call sees it.
                    if sensitive && !block.trim().is_empty() {
                        if let Some(sink) = &self.taint {
                            sink.mark(&conversation.to_string(), "memory recall").await;
                        }
                    }
                    content.insert(0, ContentBlock::text(block));
                }
            }
        }

        // Phase 79 — adaptive Persona. The refiner may replace
        // this turn's system prompt with one carrying only the
        // contextually-relevant Persona facets. The planner is
        // built fresh per turn (the factory constructs a new
        // instance each turn), so mutating `config.system_prompt`
        // here is naturally turn-scoped. Clone the `Arc` out
        // first to release the `&self.config` borrow before the
        // `&mut self.config` assignment. `None` (no refiner,
        // blank message, fallback) leaves the base prompt
        // byte-identical to pre-Phase-79.
        let refiner = self.config.system_prompt_refiner.clone();
        if let Some(refiner) = refiner {
            if has_query {
                // Phase 117 — pass the planner's current base
                // prompt so extending refiners (relevance
                // section) can compose without rebuilding the
                // base from scratch. Refiners that ignore the
                // arg (Phase 79 adaptive Persona) behave
                // identically to pre-Phase-117.
                let base_prompt = self.config.system_prompt.clone().unwrap_or_default();
                if let Some(refined) = refiner
                    .refine(&query_text, message.session_id, &base_prompt)
                    .await
                {
                    self.config.system_prompt = Some(refined);
                }
            }
        }

        // Chapter Thread — seed the session's prior conversation as
        // real User/Assistant messages ahead of the current one, so
        // the model sees the conversation the operator sees ("did you
        // find it?" can resolve "it"). Runs only on a fresh history
        // (one planner = one turn, but stay defensive), and only for
        // planners built with a seeder — every other caller keeps
        // fresh-context turns byte-identical. Seeded messages carry
        // no tool_calls, so pending_call_ids stays consistent.
        if let Some(seeder) = &self.config.conversation_seeder {
            if self.history.is_empty() {
                let prior = seeder.prior_turns(message.session_id).await;
                if !prior.is_empty() {
                    self.history.extend(seeded_history_messages(prior));
                }
            }
        }

        self.history.push(LlmMessage::User { content });
        // Pin the task message through pruning (see the field doc).
        self.task_message_index = Some(self.history.len() - 1);
        self.pending_call_ids.clear();
    }

    async fn next_step(
        &mut self,
        _observed: &[StepObservation],
        channel: &dyn ChannelContext,
    ) -> NextStep {
        // Defensive: if `begin_turn` was never called (a non-
        // ConcreteAgent caller drove us manually), seed with an empty
        // user message rather than sending a tool-less message list —
        // Anthropic rejects zero-message requests.
        if self.history.is_empty() {
            self.history.push(LlmMessage::user_text(""));
        }

        // Phase 43 Task 3 — context window pruning. If the estimated
        // token count exceeds 80% of the configured context window,
        // drop the oldest messages (preserving the most-recent tail)
        // and insert a sentinel so the model knows context was lost.
        if let Some(window) = self.config.context_window_tokens {
            let budget = window * 4 / 5; // 80% threshold
            let system_tokens =
                aivyx_llm::estimate_system_tokens(self.config.system_prompt.as_deref());
            let history_tokens = aivyx_llm::estimate_tokens(&self.history);
            let total = system_tokens + history_tokens;
            if total > budget && self.history.len() > 1 {
                // Record pre-pruning token count.
                self.accumulated_usage.context_tokens_before_pruning = total as u32;

                // Keep at least the last message (the most recent user
                // turn or tool result). Prune from the front until we
                // fit, or until only one message remains.
                let target = budget.saturating_sub(system_tokens);
                let mut keep_from = self.history.len() - 1;
                let mut tail_tokens = aivyx_llm::estimate_tokens(&self.history[keep_from..]);
                // Grow the tail backwards while it still fits.
                while keep_from > 0 {
                    let candidate = keep_from - 1;
                    let candidate_tokens =
                        aivyx_llm::estimate_tokens(&self.history[candidate..candidate + 1]);
                    if tail_tokens + candidate_tokens > target {
                        break;
                    }
                    tail_tokens += candidate_tokens;
                    keep_from = candidate;
                }
                let pruned_count = keep_from;
                if pruned_count > 0 {
                    // Pin the turn's task message: if it sits inside
                    // the prefix about to be dropped, clone it out so
                    // it can be re-inserted after the sentinel — a
                    // turn must never lose its own question (see the
                    // `task_message_index` field doc).
                    let rescued_task = self
                        .task_message_index
                        .filter(|&idx| idx < pruned_count)
                        .map(|idx| self.history[idx].clone());
                    // Build a summary before draining, for the prune sink.
                    if let Some(ref sink) = self.config.prune_sink {
                        let summary = summarise_pruned(&self.history[..pruned_count]);
                        let sid = channel.session_id();
                        sink.on_prune(sid, pruned_count, &summary).await;
                    }
                    self.history.drain(..pruned_count);
                    self.history.insert(
                        0,
                        LlmMessage::user_text(format!(
                            "[Earlier context pruned: {pruned_count} messages removed \
                             to fit context window]"
                        )),
                    );
                    if let Some(task) = rescued_task {
                        self.history.insert(1, task);
                        self.task_message_index = Some(1);
                    } else if let Some(idx) = self.task_message_index {
                        // Survived the drain — shift for the removed
                        // prefix plus the inserted sentinel.
                        self.task_message_index = Some(idx - pruned_count + 1);
                    }
                    self.pruned_message_count += pruned_count;
                }

                // Record post-pruning token count.
                let after = system_tokens + aivyx_llm::estimate_tokens(&self.history);
                self.accumulated_usage.context_tokens_after_pruning = after as u32;
            }
        }

        // Loop so we can synthesize a recovery step if the LLM picks a
        // tool name we don't recognize, or (Phase 101) emits a known
        // tool with input that fails its schema. `repair_rounds`
        // bounds the latter: after two `invalid_input` repair results
        // the call dispatches as-is (PHASE_101.md Q3).
        let mut repair_rounds = 0usize;
        loop {
            let terminal = match self.one_step(channel).await {
                Ok(t) => t,
                Err(LlmError::Cancelled) => {
                    // Mid-stream cancellation (either an external
                    // signal or a wall-clock timeout firing on the
                    // channel's token). Return `NextStep::Stop` so the
                    // turn loop's own post-next_step cancellation
                    // re-check takes over and translates to
                    // `LoopOutcome::Cancelled` / `TimedOut`. Returning
                    // a FinalMessage here would misleadingly show up
                    // as a completed turn.
                    return NextStep::Stop;
                }
                Err(e) => {
                    // Any other provider error terminates the turn
                    // cleanly from the loop's perspective. We surface
                    // it as a FinalMessage carrying the error text so
                    // audit still sees a Completed turn. A future
                    // enhancement could plumb `AivyxError::Llm`
                    // through a new NextStep variant, but that's a
                    // bigger change.
                    return NextStep::FinalMessage(format!("LLM error: {e}"));
                }
            };

            match terminal {
                LlmStepEnd::FinalMessage { text, usage } => {
                    self.accumulate(usage);

                    // Phase 126 — before treating this as a final
                    // message, try to extract tool calls from the
                    // text. Some LLM providers (qwen3 via Ollama
                    // observed in Phase 124) emit `<tool_code>` /
                    // `<tool_call>` JSON in response text rather
                    // than the protocol `tool_calls` array.
                    // Extraction yields synthesized ToolCallEnds
                    // that flow through the same Phase 120/101
                    // dispatch helper as protocol-channel calls;
                    // the wrapper-tag is threaded into the per-call
                    // `extracted_from_text` audit field for
                    // forensic visibility.
                    //
                    // Phase 127 Task 7 — the extractor receives a
                    // family-hint from the provider (Ollama queries
                    // `/api/show` once per model; other providers
                    // return None via the trait default). The hint
                    // biases inner-shape priority — qwen-family
                    // models prefer Qwen3-Coder XML over JSON
                    // inside `<tool_call>`. Failure to determine
                    // the family is silent — the substrate falls
                    // back to the default permissive scan.
                    // Routed: ask about the model that actually served
                    // the step, not the configured one.
                    let served = self.served_model();
                    let family_hint = self.provider.tool_call_family_hint(&served).await;
                    let extracted = crate::textual_tool_call::extract_tool_calls_with_hint(
                        &text,
                        family_hint.as_deref(),
                    );
                    if !extracted.is_empty() {
                        // Synthesize ToolCallEnds with UUID call IDs
                        // (the protocol channel didn't issue any).
                        // Each call carries the wrapper-tag through
                        // to its eventual audit entry.
                        let synthesized: Vec<(ToolCallEnd, String)> = extracted
                            .into_iter()
                            .map(|ext| {
                                let wrapper = ext.wrapper_tag.clone();
                                let call = ToolCallEnd {
                                    call_id: format!("extracted-{}", uuid::Uuid::new_v4()),
                                    tool_name: ext.tool_name,
                                    input: ext.arguments,
                                    name_resolution: aivyx_llm::NameResolution::Known,
                                };
                                (call, wrapper)
                            })
                            .collect();

                        // Push the assistant message AS THE MODEL
                        // SENT IT — text contains the `<tool_code>`
                        // blocks; the synthesized records mirror the
                        // protocol-channel shape so re-feeding history
                        // on the next round (after tool execution)
                        // works the same as a normal protocol-channel
                        // tool-call turn.
                        let records: Vec<LlmToolCallRecord> = synthesized
                            .iter()
                            .map(|(c, _)| LlmToolCallRecord {
                                call_id: c.call_id.clone(),
                                tool_name: c.tool_name.clone(),
                                input: c.input.clone(),
                            })
                            .collect();
                        self.history.push(LlmMessage::Assistant {
                            text: text.clone(),
                            tool_calls: records,
                        });

                        // Process each extracted call through the
                        // same dispatch helper as protocol calls.
                        // wrapper_tag flows into the per-request
                        // `extracted_from_text` field.
                        let validate_enabled = repair_rounds < 2;
                        let mut batch: Vec<ToolCallRequest> = Vec::new();
                        let mut had_invalid_input = false;
                        for (call, wrapper_tag) in synthesized {
                            let (req_opt, invalid) =
                                self.process_one_call(call, Some(wrapper_tag), validate_enabled);
                            if invalid {
                                had_invalid_input = true;
                            }
                            if let Some(req) = req_opt {
                                batch.push(req);
                            }
                        }
                        if had_invalid_input {
                            repair_rounds += 1;
                        }
                        if batch.is_empty() {
                            // Every extracted call failed (unknown or
                            // invalid). Loop to retry LLM with error
                            // results in history.
                            continue;
                        }
                        if batch.len() == 1 {
                            let req = batch.into_iter().next().unwrap();
                            return NextStep::ToolCall {
                                tool_id: req.tool_id,
                                input: req.input,
                                auto_corrected_from: req.auto_corrected_from,
                                extracted_from_text: req.extracted_from_text,
                            };
                        }
                        return NextStep::ToolCalls(batch);
                    }

                    // No extractable calls — original FinalMessage path.
                    self.history.push(LlmMessage::Assistant {
                        text: text.clone(),
                        tool_calls: Vec::new(),
                    });
                    return NextStep::FinalMessage(text);
                }
                LlmStepEnd::ToolCalls {
                    calls,
                    text_so_far,
                    usage,
                } => {
                    self.accumulate(usage);

                    // Build assistant message with all tool call records.
                    let records: Vec<LlmToolCallRecord> = calls
                        .iter()
                        .map(|c| LlmToolCallRecord {
                            call_id: c.call_id.clone(),
                            tool_name: c.tool_name.clone(),
                            input: c.input.clone(),
                        })
                        .collect();
                    self.history.push(LlmMessage::Assistant {
                        text: text_so_far,
                        tool_calls: records,
                    });

                    // Partition calls into known (dispatchable) and
                    // unknown (immediate error). Known calls get queued
                    // for execution; unknown ones get synthetic
                    // tool_result errors appended to history now.
                    let mut batch: Vec<ToolCallRequest> = Vec::new();
                    // Phase 101 — tracks whether this round emitted an
                    // `invalid_input` repair result, so the repair cap
                    // advances only on a genuine validation failure.
                    let mut had_invalid_input = false;
                    // Once two repair rounds are spent, validation is
                    // skipped: a known call dispatches as-is and the
                    // tool's own `execute` validation is the floor
                    // (PHASE_101.md Q3).
                    let validate_enabled = repair_rounds < 2;

                    for call in calls {
                        // Phase 120 fuzzy-recovery + Phase 101
                        // validation, refactored into a helper
                        // at Phase 126 Task 4 so the new
                        // FinalMessage extraction branch can share
                        // the same dispatch path. Protocol-channel
                        // calls always carry `extracted_from_text:
                        // None`; the helper does not synthesize a
                        // wrapper-tag for these.
                        let (req_opt, invalid) =
                            self.process_one_call(call, None, validate_enabled);
                        if invalid {
                            had_invalid_input = true;
                        }
                        if let Some(req) = req_opt {
                            batch.push(req);
                        }
                    }

                    // Phase 101 — a round that emitted an `invalid_input`
                    // result spends one of the two repair attempts.
                    if had_invalid_input {
                        repair_rounds += 1;
                    }

                    if batch.is_empty() {
                        // Every call was unknown or failed validation —
                        // loop to retry the LLM with the error results
                        // in history.
                        continue;
                    }

                    if batch.len() == 1 {
                        // Single known tool — use the singular path.
                        // Phase 120 — preserve the auto-correction flag
                        // from the per-call ToolCallRequest.
                        // Phase 126 — preserve the extraction flag too;
                        // both compose forensically in the audit chain.
                        let req = batch.into_iter().next().unwrap();
                        return NextStep::ToolCall {
                            tool_id: req.tool_id,
                            input: req.input,
                            auto_corrected_from: req.auto_corrected_from,
                            extracted_from_text: req.extracted_from_text,
                        };
                    }

                    // Multiple known tools — batch dispatch.
                    return NextStep::ToolCalls(batch);
                }
            }
        }
    }

    async fn observe_tool_outcome(&mut self, tool_id: ToolId, outcome: &ToolOutcome) {
        // `pending_call_ids` is populated by the most recent ToolCall(s)
        // return; if empty, either `begin_turn` wasn't called or the turn
        // loop invoked us out of order. Synthesize a stable id so the
        // history stays well-formed.
        let call_id = self
            .pending_call_ids
            .pop_front()
            .unwrap_or_else(|| "unknown-call".to_string());

        let (content, is_error) = render_tool_result(outcome);
        // Vitrine chat testing (2026-07-05) — cap giant tool results
        // BEFORE they enter history. The pruner can only drop whole
        // messages and must keep the tail, so a single raw-HTML
        // web.fetch (~30k tokens) was un-prunable and pushed the
        // request past the real context window — the provider then
        // truncated server-side, silently, from the front, where the
        // system prompt lives.
        let mut content = cap_tool_result_content(content, self.config.context_window_tokens);

        // POLISH_WAVES.md sub-project 4, item A — tool-failure thrash
        // nudge. Live repro: web_search down, the model pivoted once
        // reasonably then degenerated into 6 differing failed calls and
        // a raw web.fetch of the search engine's own homepage, never
        // reporting the outage. Track consecutive failures of the SAME
        // tool regardless of input, and nudge once when the streak
        // reaches the threshold.
        self.consecutive_tool_failures = if is_error {
            Some(match self.consecutive_tool_failures {
                Some((id, count)) if id == tool_id => (id, count + 1),
                _ => (tool_id, 1),
            })
        } else {
            None
        };
        if let Some((id, count)) = self.consecutive_tool_failures
            && id == tool_id
            && count == TOOL_FAILURE_NUDGE_THRESHOLD
        {
            let tool_name = self
                .registry
                .get(tool_id)
                .map(|t| t.name().to_string())
                .unwrap_or_else(|| "the tool".to_string());
            content.push_str(&format!(
                "{TOOL_FAILURE_NUDGE_MARKER} {tool_name} has failed {count} times in \
                 a row. Stop retrying it — report the outage to the \
                 operator instead of trying an unrelated approach.]"
            ));
        }

        self.history.push(LlmMessage::ToolResult {
            call_id,
            content,
            is_error,
        });
    }

    fn turn_usage(&self) -> crate::TokenUsage {
        self.accumulated_usage
    }

    fn model(&self) -> &str {
        &self.config.model
    }

    /// Per-model spend. Without routing every step is the configured
    /// model, so this is exactly `[(model, turn_usage)]`. The turn-level
    /// pruning estimates ride on the first entry, as they do on
    /// `turn_usage`.
    fn turn_costs(&self) -> Vec<(String, crate::TokenUsage)> {
        if self.turn_costs.is_empty() {
            return if self.config.model.is_empty() {
                vec![]
            } else {
                vec![(self.config.model.clone(), self.accumulated_usage)]
            };
        }
        let mut costs = self.turn_costs.clone();
        costs[0].1.context_tokens_before_pruning =
            self.accumulated_usage.context_tokens_before_pruning;
        costs[0].1.context_tokens_after_pruning =
            self.accumulated_usage.context_tokens_after_pruning;
        costs
    }

    fn tool_result_texts(&self) -> Vec<String> {
        self.history
            .iter()
            .filter_map(|m| match m {
                LlmMessage::ToolResult { content, .. } => {
                    let text = content
                        .split(TOOL_FAILURE_NUDGE_MARKER)
                        .next()
                        .unwrap_or(content.as_str());
                    Some(text.to_string())
                }
                _ => None,
            })
            .collect()
    }
}

impl Drop for LlmPlanner {
    /// Releases this turn's checked-out kvcache slot, if any. Pure,
    /// synchronous, infallible -- `KvSlotPool::release` does no I/O, so
    /// this is safe to run from `Drop` (which cannot be async). Fires
    /// naturally when this planner (a local variable inside
    /// `ConcreteAgent::turn()`, freshly constructed every turn) goes out
    /// of scope at the end of the turn -- no separate release call site
    /// needed anywhere.
    fn drop(&mut self) {
        if let (Some(slot_id), Some(kv)) = (self.kv_slot_id, &self.kv_cache) {
            kv.pool.release(slot_id);
        }
    }
}

// ---------------------------------------------------------------------------
// Tool-result rendering
// ---------------------------------------------------------------------------

/// Serialize a [`ToolOutcome`] into the `(content, is_error)` pair that
/// goes into an [`LlmMessage::ToolResult`].
///
/// Successful outcomes emit the tool's `output` verbatim as compact
/// JSON — whatever the tool produced, the LLM sees. Failure outcomes
/// use a stable structured envelope so future additions don't break
/// existing agents:
///
/// ```json
/// { "error": "<kind>", "message": "<detail>" }
/// ```
///
/// The `error` field is one of: `denied`, `not_in_role`, `rate_limited`,
/// `failed`, `timed_out`, `requires_escalation`. It is stable across versions;
/// new kinds land as new strings, never as renames.
/// Floor on the tool-result cap so a small configured window can never
/// cripple tool output entirely (~1k tokens of result is always
/// allowed through).
const TOOL_RESULT_CAP_FLOOR_CHARS: usize = 4_000;

/// Cap a rendered tool-result string to roughly **half the context
/// window**: budget = `window_tokens * 2` chars (at the pruner's ~4
/// chars/token estimate, that's `window/2` tokens), floored at
/// [`TOOL_RESULT_CAP_FLOOR_CHARS`]. `None` (pruning disabled) keeps
/// the content untouched — byte-identical to the pre-cap behavior.
///
/// Truncation keeps the head (where structured output and page
/// content start) and appends an explicit marker so the model knows
/// it saw a partial result rather than a complete one.
fn cap_tool_result_content(content: String, window_tokens: Option<usize>) -> String {
    let Some(window) = window_tokens else {
        return content;
    };
    let max_chars = (window * 2).max(TOOL_RESULT_CAP_FLOOR_CHARS);
    let total = content.chars().count();
    if total <= max_chars {
        return content;
    }
    let mut capped: String = content.chars().take(max_chars).collect();
    capped.push_str(&format!(
        "\n…[tool output truncated: showing {max_chars} of {total} \
         chars — the result was too large for the model's context; \
         request a narrower read if the missing part matters]"
    ));
    capped
}

fn render_tool_result(outcome: &ToolOutcome) -> (String, bool) {
    match outcome {
        ToolOutcome::Completed { output, .. } => {
            // Emit the output as-is. If the tool's output happens to be
            // `{"error": ...}` we leave that alone — that's the tool's
            // responsibility. Verification state is not propagated
            // because an unverified success is still a successful
            // return per D1, and audit is authoritative for verify.
            let content = serde_json::to_string(output)
                .unwrap_or_else(|_| "<unserializable output>".to_string());
            (content, false)
        }
        ToolOutcome::Denied { scope, .. } => {
            let envelope = json!({
                "error": "denied",
                "message": format!("scope {scope} not granted"),
            });
            (envelope.to_string(), true)
        }
        ToolOutcome::NotInRole { tool_name } => {
            let envelope = json!({
                "error": "not_in_role",
                "message": format!("tool {tool_name} is not in the active role's allowlist"),
            });
            (envelope.to_string(), true)
        }
        ToolOutcome::RateLimited { tool_name, reason } => {
            let envelope = json!({
                "error": "rate_limited",
                "message": format!("tool {tool_name} throttled: {reason}"),
            });
            (envelope.to_string(), true)
        }
        ToolOutcome::RequiresEscalation { reason, .. } => {
            let envelope = json!({
                "error": "requires_escalation",
                "message": reason,
            });
            (envelope.to_string(), true)
        }
        ToolOutcome::Failed(err) => {
            let envelope = json!({
                "error": "failed",
                "message": err.to_string(),
            });
            (envelope.to_string(), true)
        }
    }
}

// ---------------------------------------------------------------------------
// Tool-call input validation — Phase 101
// ---------------------------------------------------------------------------

/// Validate a tool call's `input` against the tool's declared
/// `input_schema()` (JSON Schema). Returns `Ok(())` when the input
/// satisfies the schema, or `Err(summary)` — a human-readable
/// digest of the first few violations — which the planner turns
/// into an `invalid_input` repair result.
///
/// **Fails open.** If the schema itself does not compile as valid
/// JSON Schema, the input is treated as valid. Tool schemas are
/// authored in-tree and a malformed one should never reach here;
/// failing open guarantees a quirky future schema can never brick
/// its own tool's dispatch — the worst case degrades to
/// pre-Phase-101 behavior (the tool's own `execute` validation is
/// still the floor).
/// Phase 120 Task 4 — fuzzy-match threshold for the tool-name
/// recovery path. Default `0.80` matches Phase 112's fuzzy-match
/// default (the substrate's load-bearing threshold for tokenized
/// Jaccard similarity decisions). Task 5 will make this operator-
/// configurable via `[providers] tool_name_auto_correct_threshold`.
const FUZZY_TOOL_NAME_THRESHOLD: f32 = 0.80;

/// Phase 120 Task 4 — fuzzy-match recovery for hallucinated tool
/// names. Walks `registry`'s tools, computes
/// `aivyx_core::skill_proposer::title_similarity(emitted_name,
/// tool_name)`, and returns the `ToolId` of the best match if and
/// only if its score meets or exceeds `threshold`.
///
/// Returns `None` when:
/// - The registry is empty (no tools registered for this turn).
/// - No tool's name scores at or above `threshold`.
///
/// Ties broken by registration order (the first tool to reach the
/// max score wins). In practice ties are rare since the threshold
/// gates on a meaningful similarity ceiling.
///
/// Pure function modulo the registry iteration; planner-internal.
fn fuzzy_recover_tool_name(
    registry: &crate::ToolRegistry,
    emitted_name: &str,
    threshold: f32,
) -> Option<crate::ToolId> {
    let mut best: Option<(crate::ToolId, f32)> = None;
    let snapshot = registry.snapshot();
    for tool in &snapshot {
        let score = crate::skill_proposer::title_similarity(emitted_name, tool.name());
        if score >= threshold {
            // Find the id for this tool by name (cheap — the
            // registry's `find_by_name` is the canonical lookup).
            let Some(id) = registry.find_by_name(tool.name()) else {
                continue;
            };
            match best {
                None => best = Some((id, score)),
                Some((_, b)) if score > b => best = Some((id, score)),
                _ => {} // existing best wins on tie
            }
        }
    }
    best.map(|(id, _)| id)
}

/// Phase 120 Task 6 — top-N tools ranked by Jaccard title similarity
/// against the model's emitted name. Used by the synthetic
/// `unknown_tool` error path so the model sees `available_suggestions`
/// and can retry with the right name.
///
/// Returns `Vec<(tool_name, score)>` sorted descending by score; ties
/// broken by registration order (stable). At most `n` entries; empty
/// when the registry is empty (the synthetic message falls back to
/// a neutral "no tools available" form — see [`build_unknown_tool_message`]).
///
/// Pure function modulo the registry iteration.
fn top_n_similar_tools(
    registry: &crate::ToolRegistry,
    emitted_name: &str,
    n: usize,
) -> Vec<(String, f32)> {
    let mut scored: Vec<(String, f32)> = registry
        .snapshot()
        .into_iter()
        .map(|tool| {
            let score = crate::skill_proposer::title_similarity(emitted_name, tool.name());
            (tool.name().to_string(), score)
        })
        .collect();
    // Stable sort by score descending; ties keep iteration order
    // (which matches registration order on `ToolRegistry::iter_tools`).
    scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    scored.truncate(n);
    scored
}

/// Phase 120 Task 6 — operator-style synthetic message for the
/// model when the planner's fuzzy-match recovery couldn't resolve
/// the emitted name above threshold.
///
/// Wording is deliberately unambiguous about WHICH tools to retry
/// with so the model picks the right name on the next turn:
/// - With suggestions: `"tool 'X' is not registered. Did you mean
///   'Y', 'Z', 'W'?"`
/// - Empty registry: `"tool 'X' is not registered. (no tools
///   available in this role)"` — neutral fallback rather than a
///   misleading "did you mean?" with no suggestions.
fn build_unknown_tool_message(emitted_name: &str, suggestions: &[(String, f32)]) -> String {
    if suggestions.is_empty() {
        return format!(
            "tool '{}' is not registered. (no tools available in this role)",
            emitted_name
        );
    }
    let names: Vec<String> = suggestions.iter().map(|(n, _)| format!("'{n}'")).collect();
    format!(
        "tool '{}' is not registered. Did you mean {}?",
        emitted_name,
        names.join(", "),
    )
}

fn validate_tool_input(
    schema: &serde_json::Value,
    input: &serde_json::Value,
) -> Result<(), String> {
    let validator = match jsonschema::validator_for(schema) {
        Ok(v) => v,
        Err(_) => return Ok(()), // fail open — see doc comment
    };
    // Cap the digest at the first five violations: enough for the
    // model to repair the call, short enough to keep the result
    // message compact.
    let violations: Vec<String> = validator
        .iter_errors(input)
        .take(5)
        .map(|e| e.to_string())
        .collect();
    if violations.is_empty() {
        Ok(())
    } else {
        Err(violations.join("; "))
    }
}

/// Build a compact summary of pruned messages for the prune sink.
/// Truncates each message to avoid storing massive tool results
/// verbatim in memory — the point is orientation, not replay.
fn summarise_pruned(messages: &[LlmMessage]) -> String {
    use std::fmt::Write;
    let mut buf = String::new();
    for (i, msg) in messages.iter().enumerate() {
        if i > 0 {
            buf.push('\n');
        }
        match msg {
            LlmMessage::User { content } => {
                let parts: Vec<&str> = content
                    .iter()
                    .map(|b| match b {
                        ContentBlock::Text { text } => text.as_str(),
                        ContentBlock::ImageBase64 { media_type, .. } => media_type.as_str(),
                        ContentBlock::DocumentBase64 { media_type, .. } => media_type.as_str(),
                    })
                    .collect();
                let summary = parts.join(", ");
                let _ = write!(buf, "[user] {}", truncate(&summary, 200));
            }
            LlmMessage::Assistant { text, tool_calls } => {
                let _ = write!(buf, "[assistant] {}", truncate(text, 200));
                for tc in tool_calls {
                    let _ = write!(buf, "\n  tool_call: {}", tc.tool_name);
                }
            }
            LlmMessage::ToolResult {
                call_id, content, ..
            } => {
                let _ = write!(
                    buf,
                    "[tool_result call_id={call_id}] {}",
                    truncate(content, 200)
                );
            }
        }
    }
    buf
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        // Find a char boundary at or before `max`.
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
    }
}

/// A stable-within-one-process-run hash of the stable prefix (system
/// prompt + tool definitions) -- used as `CacheKey.prefix_hash`.
/// Deliberately NOT guaranteed stable across Rust versions/compilations:
/// a rebuild changing the hash algorithm just means old kvcache entries
/// silently miss instead of hit (fail-open, matching every other
/// kvcache operation), never a correctness problem. `None` and `Some("")`
/// hash differently (a leading discriminant byte precedes the content)
/// so a planner with no system prompt at all never collides with one
/// whose prompt happens to be the empty string.
fn compute_prefix_hash(system_prompt: Option<&str>, tools: &[LlmToolDescriptor]) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    match system_prompt {
        Some(s) => {
            true.hash(&mut hasher);
            s.hash(&mut hasher);
        }
        None => false.hash(&mut hasher),
    }
    for tool in tools {
        tool.name.hash(&mut hasher);
        tool.description.hash(&mut hasher);
        tool.input_schema.to_string().hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_llm::ToolCallEnd;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use serde_json::{Value, json};

    use aivyx_capability::{Scope, TrustTier};
    use aivyx_llm::LlmUsage;

    use crate::planner::NextStep;
    use crate::{
        AivyxError, ChannelError, ChannelPlatform, SessionId, Tool, ToolContext, ToolId,
        ToolOutcome, TurnId, TurnOutcome, Verification,
    };

    // -----------------------------------------------------------------------
    // A minimal FakeLlmProvider built from a script of (events, terminal)
    // pairs — one per expected `chat_stream` call. Deliberately rebuilt
    // here rather than imported from `aivyx-llm`'s test module.
    // -----------------------------------------------------------------------

    struct FakeLlmProvider {
        script: Mutex<std::collections::VecDeque<FakeStep>>,
        // GPU-slot broker coordination — records `(id_slot, slot_hint)` off the most recent
        // `chat_stream` call, so tests can assert on what the planner
        // actually sent without a real HTTP layer to inspect.
        last_request: Mutex<Option<(Option<u32>, Option<SlotHint>)>>,
        // Model routing — `(model, route)` off every `chat_stream` call.
        routes: Mutex<Vec<(String, Option<aivyx_llm::RouteHint>)>>,
        // Model routing — the model of every `tool_call_family_hint` call.
        hints: Mutex<Vec<String>>,
    }

    struct FakeStep {
        events: Vec<LlmStreamEvent>,
        terminal: LlmStepEnd,
    }

    impl FakeLlmProvider {
        fn new(steps: Vec<FakeStep>) -> Arc<Self> {
            Arc::new(FakeLlmProvider {
                script: Mutex::new(steps.into()),
                last_request: Mutex::new(None),
                routes: Mutex::new(Vec::new()),
                hints: Mutex::new(Vec::new()),
            })
        }
    }

    #[async_trait]
    impl LlmProvider for FakeLlmProvider {
        async fn chat_stream(
            &self,
            request: LlmRequest<'_>,
            _cancellation: &crate::CancellationToken,
        ) -> Result<Box<dyn LlmStream>, LlmError> {
            *self.last_request.lock().unwrap() = Some((request.id_slot, request.slot_hint.clone()));
            self.routes
                .lock()
                .unwrap()
                .push((request.model.to_string(), request.route.clone()));
            let step = self
                .script
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| LlmError::Config("FakeLlmProvider exhausted".to_string()))?;
            Ok(Box::new(FakeStream {
                events: step.events.into_iter(),
                terminal: Some(step.terminal),
            }))
        }

        async fn tool_call_family_hint(&self, model: &str) -> Option<String> {
            self.hints.lock().unwrap().push(model.to_string());
            None
        }
    }

    struct FakeStream {
        events: std::vec::IntoIter<LlmStreamEvent>,
        terminal: Option<LlmStepEnd>,
    }

    #[async_trait]
    impl LlmStream for FakeStream {
        async fn next_event(&mut self) -> Result<Option<LlmStreamEvent>, LlmError> {
            Ok(self.events.next())
        }
        async fn finish(self: Box<Self>) -> Result<LlmStepEnd, LlmError> {
            self.terminal
                .ok_or_else(|| LlmError::StreamEnded("double finish".to_string()))
        }
    }

    // -----------------------------------------------------------------------
    // FakeChannel records streamed text so we can assert the planner
    // relayed tokens as they arrived.
    // -----------------------------------------------------------------------

    struct RecChannel {
        session: SessionId,
        token: crate::CancellationToken,
        streamed: Mutex<Vec<String>>,
    }

    impl RecChannel {
        fn new() -> Self {
            RecChannel {
                session: SessionId::new(),
                token: crate::CancellationToken::new(),
                streamed: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl ChannelContext for RecChannel {
        fn channel_name(&self) -> &str {
            "rec"
        }
        fn platform(&self) -> ChannelPlatform {
            ChannelPlatform::Local
        }
        fn trust_tier(&self) -> TrustTier {
            TrustTier::Trusted
        }
        fn session_id(&self) -> SessionId {
            self.session
        }
        async fn stream_event(&self, event: StreamEvent<'_>) -> Result<(), ChannelError> {
            if let StreamEvent::Text(s) = event {
                self.streamed.lock().unwrap().push(s.to_string());
            }
            Ok(())
        }
        async fn finalize(&self, _outcome: &TurnOutcome) -> Result<(), ChannelError> {
            Ok(())
        }
        fn cancellation_token(&self) -> crate::CancellationToken {
            self.token.clone()
        }
    }

    // -----------------------------------------------------------------------
    // FakeTool (reused shape from agent.rs tests, trimmed).
    // -----------------------------------------------------------------------

    struct FakeTool {
        id: ToolId,
        name: &'static str,
        schema: Value,
    }

    impl FakeTool {
        fn new(name: &'static str) -> Self {
            FakeTool {
                id: ToolId::new(),
                name,
                schema: json!({"type": "object"}),
            }
        }

        /// Phase 101 — a `FakeTool` carrying a real JSON Schema, for
        /// the planner validate-before-dispatch tests.
        fn with_schema(name: &'static str, schema: Value) -> Self {
            FakeTool {
                id: ToolId::new(),
                name,
                schema,
            }
        }
    }

    #[async_trait]
    impl Tool for FakeTool {
        fn id(&self) -> ToolId {
            self.id
        }
        fn name(&self) -> &str {
            self.name
        }
        fn description(&self) -> &str {
            "fake"
        }
        fn input_schema(&self) -> &Value {
            &self.schema
        }
        fn required_scope(&self, _input: &Value) -> Scope {
            Scope::parse("memory.read").unwrap()
        }
        async fn execute(&self, _input: Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
            ToolOutcome::Completed {
                output: json!({"ok": true}),
                verified: Verification::NotApplicable,
            }
        }
    }

    fn zero_usage() -> LlmUsage {
        LlmUsage::default()
    }

    // -----------------------------------------------------------------------
    // Tests proper
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn final_message_path_returns_next_step_and_appends_history() {
        let script = vec![FakeStep {
            events: vec![
                LlmStreamEvent::TextChunk("he".to_string()),
                LlmStreamEvent::TextChunk("llo".to_string()),
            ],
            terminal: LlmStepEnd::FinalMessage {
                text: "hello".to_string(),
                usage: zero_usage(),
            },
        }];
        let provider = FakeLlmProvider::new(script);
        let registry = Arc::new(ToolRegistry::new(vec![]));
        let mut planner = LlmPlanner::new(
            provider,
            registry,
            LlmPlannerConfig::new("claude-haiku-4-5-20251001"),
        );

        let channel = RecChannel::new();
        planner
            .begin_turn(&Message::text(channel.session, "hi"), TurnId::new())
            .await;
        let step = planner.next_step(&[], &channel).await;
        assert!(matches!(step, NextStep::FinalMessage(ref m) if m == "hello"));

        // Streamed chunks relayed to the channel in order.
        let streamed = channel.streamed.lock().unwrap().clone();
        assert_eq!(streamed, vec!["he".to_string(), "llo".to_string()]);

        // History: User("hi") → Assistant("hello", no tool calls).
        let hist = planner.history();
        assert_eq!(hist.len(), 2);
        assert!(matches!(
            hist[0],
            LlmMessage::User { ref content } if content == &[ContentBlock::text("hi")]
        ));
        match &hist[1] {
            LlmMessage::Assistant { text, tool_calls } => {
                assert_eq!(text, "hello");
                assert!(tool_calls.is_empty());
            }
            other => panic!("expected Assistant, got {other:?}"),
        }
    }

    // ---- Phase 76 — ContextProvider auto-recall hook -----------

    struct FakeContextProvider {
        block: Option<String>,
        seen: std::sync::Mutex<Vec<String>>,
        seen_sessions: std::sync::Mutex<Vec<SessionId>>,
    }

    impl FakeContextProvider {
        fn new(block: Option<&str>) -> Arc<Self> {
            Arc::new(Self {
                block: block.map(str::to_string),
                seen: std::sync::Mutex::new(Vec::new()),
                seen_sessions: std::sync::Mutex::new(Vec::new()),
            })
        }
    }

    #[async_trait]
    impl ContextProvider for FakeContextProvider {
        async fn recall(
            &self,
            user_message: &str,
            session_id: SessionId,
            _turn_id: TurnId,
            _origin: crate::MessageOrigin,
        ) -> Option<String> {
            self.seen.lock().unwrap().push(user_message.to_string());
            self.seen_sessions.lock().unwrap().push(session_id);
            self.block.clone()
        }
    }

    // ---- Model routing Part 3b — recall taint -------------------

    /// A provider with a fixed block and a fixed sensitivity.
    struct SensitivityProvider {
        block: Option<String>,
        sensitive: bool,
    }

    #[async_trait]
    impl ContextProvider for SensitivityProvider {
        async fn recall(
            &self,
            _user_message: &str,
            _session_id: SessionId,
            _turn_id: TurnId,
            _origin: crate::MessageOrigin,
        ) -> Option<String> {
            self.block.clone()
        }
        fn sensitive(&self) -> bool {
            self.sensitive
        }
    }

    #[derive(Default)]
    struct RecordingTaint(std::sync::Mutex<Vec<(String, String)>>);

    #[async_trait]
    impl crate::TaintSink for RecordingTaint {
        async fn mark(&self, session: &str, reason: &str) -> bool {
            self.0
                .lock()
                .unwrap()
                .push((session.to_owned(), reason.to_owned()));
            true
        }
    }

    /// Runs `begin_turn` with a taint sink and `provider`; returns the
    /// recorded marks and the message's session.
    async fn recall_marks(
        provider: Arc<dyn ContextProvider>,
    ) -> (Vec<(String, String)>, SessionId) {
        let sink = Arc::new(RecordingTaint::default());
        let mut planner = bare_planner(LlmPlannerConfig::new("m").with_context_provider(provider))
            .with_taint(sink.clone());
        let session = SessionId::new();
        planner
            .begin_turn(&Message::text(session, "what's my color?"), TurnId::new())
            .await;
        let marks = sink.0.lock().unwrap().clone();
        (marks, session)
    }

    #[tokio::test]
    async fn a_sensitive_providers_injection_marks_the_session() {
        let (marks, session) = recall_marks(Arc::new(SensitivityProvider {
            block: Some("- [notes] purple".to_string()),
            sensitive: true,
        }))
        .await;
        assert_eq!(
            marks,
            vec![(session.to_string(), "memory recall".to_string())]
        );
    }

    #[tokio::test]
    async fn a_non_sensitive_providers_injection_does_not_mark() {
        let (marks, _) = recall_marks(Arc::new(SensitivityProvider {
            block: Some("## Relevant skill".to_string()),
            sensitive: false,
        }))
        .await;
        assert!(marks.is_empty(), "got {marks:?}");
    }

    #[tokio::test]
    async fn an_empty_injection_does_not_mark() {
        for block in [None, Some(String::new()), Some("  \n".to_string())] {
            let (marks, _) = recall_marks(Arc::new(SensitivityProvider {
                block: block.clone(),
                sensitive: true,
            }))
            .await;
            assert!(marks.is_empty(), "{block:?}: got {marks:?}");
        }
    }

    fn bare_planner(config: LlmPlannerConfig) -> LlmPlanner {
        LlmPlanner::new(
            FakeLlmProvider::new(vec![]),
            Arc::new(ToolRegistry::new(vec![])),
            config,
        )
    }

    #[tokio::test]
    async fn context_provider_prepends_recalled_block() {
        let provider = FakeContextProvider::new(Some(
            "## Relevant context (auto-recalled)\n- [notes] purple",
        ));
        let mut planner =
            bare_planner(LlmPlannerConfig::new("m").with_context_provider(provider.clone()));
        let channel = RecChannel::new();
        planner
            .begin_turn(
                &Message::text(channel.session, "what's my color?"),
                TurnId::new(),
            )
            .await;

        // The query handed to recall is the raw user text.
        assert_eq!(
            provider.seen.lock().unwrap().clone(),
            vec!["what's my color?".to_string()]
        );
        // Phase 77 — begin_turn threads the message's session id
        // through so the impl can correlate a recall-feedback
        // event to this turn.
        assert_eq!(
            provider.seen_sessions.lock().unwrap().clone(),
            vec![channel.session]
        );
        // History user message: recalled block FIRST, then the
        // user's own text — one message, two content blocks.
        match &planner.history()[0] {
            LlmMessage::User { content } => {
                assert_eq!(content.len(), 2);
                assert_eq!(
                    content[0],
                    ContentBlock::text(
                        "## Relevant context (auto-recalled)\n\
                         - [notes] purple"
                    )
                );
                assert_eq!(content[1], ContentBlock::text("what's my color?"));
            }
            other => panic!("expected User, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn context_provider_none_leaves_turn_unchanged() {
        let provider = FakeContextProvider::new(None);
        let mut planner =
            bare_planner(LlmPlannerConfig::new("m").with_context_provider(provider.clone()));
        let channel = RecChannel::new();
        planner
            .begin_turn(&Message::text(channel.session, "hi there"), TurnId::new())
            .await;
        // recall consulted, returned None → turn byte-identical.
        assert_eq!(provider.seen.lock().unwrap().len(), 1);
        assert!(matches!(
            planner.history()[0],
            LlmMessage::User { ref content }
                if content == &[ContentBlock::text("hi there")]
        ));
    }

    #[tokio::test]
    async fn no_context_provider_is_unchanged() {
        // Regression guard: the default config path must not change.
        let mut planner = bare_planner(LlmPlannerConfig::new("m"));
        let channel = RecChannel::new();
        planner
            .begin_turn(&Message::text(channel.session, "hello"), TurnId::new())
            .await;
        assert!(matches!(
            planner.history()[0],
            LlmMessage::User { ref content }
                if content == &[ContentBlock::text("hello")]
        ));
    }

    #[tokio::test]
    async fn context_provider_skipped_for_blank_query() {
        // Whitespace-only text must not consult the provider (no
        // point embedding empty input) and must not inject a block
        // even if the provider would return one.
        let provider = FakeContextProvider::new(Some("SHOULD-NOT-APPEAR"));
        let mut planner =
            bare_planner(LlmPlannerConfig::new("m").with_context_provider(provider.clone()));
        let channel = RecChannel::new();
        planner
            .begin_turn(&Message::text(channel.session, "   "), TurnId::new())
            .await;
        assert!(provider.seen.lock().unwrap().is_empty());
        assert!(matches!(
            planner.history()[0],
            LlmMessage::User { ref content }
                if content == &[ContentBlock::text("   ")]
        ));
    }

    // ---- Tool-result cap (Vitrine chat testing 2026-07-05) -----

    #[test]
    fn tool_result_cap_none_window_is_untouched() {
        let big = "x".repeat(1_000_000);
        assert_eq!(cap_tool_result_content(big.clone(), None), big);
    }

    #[test]
    fn tool_result_cap_under_budget_is_untouched() {
        let s = "small output".to_string();
        assert_eq!(cap_tool_result_content(s.clone(), Some(16_384)), s);
    }

    #[test]
    fn tool_result_cap_truncates_with_marker() {
        // window 16384 → budget 32768 chars.
        let big = "y".repeat(100_000);
        let capped = cap_tool_result_content(big, Some(16_384));
        assert!(capped.starts_with("yyy"));
        assert!(capped.contains("tool output truncated"));
        assert!(capped.contains("100000"));
        // Budget + marker, nowhere near the original size.
        assert!(capped.chars().count() < 33_000);
    }

    #[test]
    fn tool_result_cap_floor_protects_tiny_windows() {
        // window 100 → raw budget 200, floored to 4000.
        let content = "z".repeat(3_000);
        assert_eq!(cap_tool_result_content(content.clone(), Some(100)), content);
    }

    // ---- Chapter Thread — ConversationSeeder hook --------------

    fn u(text: &str) -> PriorTurn {
        PriorTurn {
            is_user: true,
            text: text.to_string(),
        }
    }
    fn a(text: &str) -> PriorTurn {
        PriorTurn {
            is_user: false,
            text: text.to_string(),
        }
    }

    #[test]
    fn seeded_messages_basic_pair_replays_in_order() {
        let msgs = seeded_history_messages(vec![u("hi"), a("hello!")]);
        assert_eq!(msgs.len(), 2);
        assert!(matches!(
            &msgs[0],
            LlmMessage::User { content } if content == &[ContentBlock::text("hi")]
        ));
        assert!(matches!(
            &msgs[1],
            LlmMessage::Assistant { text, tool_calls }
                if text == "hello!" && tool_calls.is_empty()
        ));
    }

    #[test]
    fn seeded_messages_drops_leading_assistant() {
        // Providers require the first message to be a user turn.
        let msgs = seeded_history_messages(vec![a("orphan"), u("q"), a("r")]);
        assert_eq!(msgs.len(), 2);
        assert!(matches!(
            &msgs[0],
            LlmMessage::User { content } if content == &[ContentBlock::text("q")]
        ));
    }

    #[test]
    fn seeded_messages_coalesces_consecutive_same_role() {
        // Strict-alternation providers must never see user,user.
        let msgs = seeded_history_messages(vec![u("part one"), u("part two"), a("answer")]);
        assert_eq!(msgs.len(), 2);
        assert!(matches!(
            &msgs[0],
            LlmMessage::User { content }
                if content == &[ContentBlock::text("part one\npart two")]
        ));
    }

    #[test]
    fn seeded_messages_trailing_user_gets_honest_filler() {
        // A prior turn whose completion was empty leaves a trailing
        // user entry; the filler keeps alternation against the
        // current user message AND shows the model it never answered.
        let msgs = seeded_history_messages(vec![u("icao for jandakot?")]);
        assert_eq!(msgs.len(), 2);
        assert!(matches!(
            &msgs[1],
            LlmMessage::Assistant { text, .. }
                if text == "(no response was produced that turn)"
        ));
    }

    #[test]
    fn seeded_messages_blank_entries_drop_to_empty() {
        assert!(seeded_history_messages(vec![u("  "), a("")]).is_empty());
        assert!(seeded_history_messages(vec![]).is_empty());
    }

    struct FakeSeeder {
        prior: Vec<PriorTurn>,
        seen_sessions: std::sync::Mutex<Vec<SessionId>>,
    }

    impl FakeSeeder {
        fn new(prior: Vec<PriorTurn>) -> Arc<Self> {
            Arc::new(Self {
                prior,
                seen_sessions: std::sync::Mutex::new(Vec::new()),
            })
        }
    }

    #[async_trait]
    impl ConversationSeeder for FakeSeeder {
        async fn prior_turns(&self, session_id: crate::SessionId) -> Vec<PriorTurn> {
            self.seen_sessions.lock().unwrap().push(session_id);
            self.prior.clone()
        }
    }

    #[tokio::test]
    async fn conversation_seeder_replays_prior_before_current() {
        let seeder = FakeSeeder::new(vec![u("first question"), a("first answer")]);
        let mut planner =
            bare_planner(LlmPlannerConfig::new("m").with_conversation_seeder(seeder.clone()));
        let channel = RecChannel::new();
        planner
            .begin_turn(&Message::text(channel.session, "follow-up?"), TurnId::new())
            .await;
        // Session id threaded through so the impl can find the window.
        assert_eq!(
            seeder.seen_sessions.lock().unwrap().clone(),
            vec![channel.session]
        );
        // History: prior user, prior assistant, THEN the current turn.
        let history = planner.history();
        assert_eq!(history.len(), 3);
        assert!(matches!(
            &history[0],
            LlmMessage::User { content }
                if content == &[ContentBlock::text("first question")]
        ));
        assert!(matches!(
            &history[1],
            LlmMessage::Assistant { text, .. } if text == "first answer"
        ));
        assert!(matches!(
            &history[2],
            LlmMessage::User { content }
                if content == &[ContentBlock::text("follow-up?")]
        ));
    }

    #[tokio::test]
    async fn conversation_seeder_empty_is_byte_identical() {
        let seeder = FakeSeeder::new(vec![]);
        let mut planner =
            bare_planner(LlmPlannerConfig::new("m").with_conversation_seeder(seeder.clone()));
        let channel = RecChannel::new();
        planner
            .begin_turn(&Message::text(channel.session, "hello"), TurnId::new())
            .await;
        let history = planner.history();
        assert_eq!(history.len(), 1);
        assert!(matches!(
            &history[0],
            LlmMessage::User { content }
                if content == &[ContentBlock::text("hello")]
        ));
    }

    #[tokio::test]
    async fn conversation_seeder_composes_with_recall_block() {
        // Seeded prior messages land as separate history entries; the
        // recall block still folds into the CURRENT user message.
        let seeder = FakeSeeder::new(vec![u("prior q"), a("prior a")]);
        let provider = FakeContextProvider::new(Some("RECALL-BLOCK"));
        let mut planner = bare_planner(
            LlmPlannerConfig::new("m")
                .with_conversation_seeder(seeder)
                .with_context_provider(provider),
        );
        let channel = RecChannel::new();
        planner
            .begin_turn(&Message::text(channel.session, "now?"), TurnId::new())
            .await;
        let history = planner.history();
        assert_eq!(history.len(), 3);
        assert!(matches!(
            &history[2],
            LlmMessage::User { content }
                if content == &[
                    ContentBlock::text("RECALL-BLOCK"),
                    ContentBlock::text("now?"),
                ]
        ));
    }

    // ---- Phase 79 — SystemPromptRefiner hook -------------------

    struct FakeRefiner {
        refined: Option<String>,
        seen: std::sync::Mutex<Vec<String>>,
    }

    impl FakeRefiner {
        fn new(refined: Option<&str>) -> Arc<Self> {
            Arc::new(Self {
                refined: refined.map(str::to_string),
                seen: std::sync::Mutex::new(Vec::new()),
            })
        }
    }

    #[async_trait]
    impl SystemPromptRefiner for FakeRefiner {
        async fn refine(
            &self,
            user_message: &str,
            _session_id: crate::SessionId,
            _base_prompt: &str,
        ) -> Option<String> {
            self.seen.lock().unwrap().push(user_message.to_string());
            self.refined.clone()
        }
    }

    #[tokio::test]
    async fn refiner_some_swaps_system_prompt_for_the_turn() {
        let refiner = FakeRefiner::new(Some("REFINED PERSONA PROMPT"));
        let mut planner = bare_planner(
            LlmPlannerConfig::new("m")
                .with_system_prompt("BASE PROMPT")
                .with_system_prompt_refiner(refiner.clone()),
        );
        let channel = RecChannel::new();
        planner
            .begin_turn(
                &Message::text(channel.session, "help me ship"),
                TurnId::new(),
            )
            .await;
        assert_eq!(
            refiner.seen.lock().unwrap().clone(),
            vec!["help me ship".to_string()]
        );
        assert_eq!(
            planner.config.system_prompt.as_deref(),
            Some("REFINED PERSONA PROMPT")
        );
    }

    #[tokio::test]
    async fn refiner_none_keeps_base_prompt() {
        let refiner = FakeRefiner::new(None);
        let mut planner = bare_planner(
            LlmPlannerConfig::new("m")
                .with_system_prompt("BASE PROMPT")
                .with_system_prompt_refiner(refiner.clone()),
        );
        let channel = RecChannel::new();
        planner
            .begin_turn(&Message::text(channel.session, "hi"), TurnId::new())
            .await;
        // Consulted, returned None → base byte-identical.
        assert_eq!(refiner.seen.lock().unwrap().len(), 1);
        assert_eq!(planner.config.system_prompt.as_deref(), Some("BASE PROMPT"));
    }

    #[tokio::test]
    async fn no_refiner_is_unchanged() {
        // Regression guard: the default path must not change.
        let mut planner = bare_planner(LlmPlannerConfig::new("m").with_system_prompt("BASE"));
        let channel = RecChannel::new();
        planner
            .begin_turn(&Message::text(channel.session, "hello"), TurnId::new())
            .await;
        assert_eq!(planner.config.system_prompt.as_deref(), Some("BASE"));
    }

    #[tokio::test]
    async fn refiner_skipped_for_blank_query() {
        // Whitespace-only message must not consult the refiner
        // and must leave the base prompt untouched.
        let refiner = FakeRefiner::new(Some("SHOULD-NOT-APPEAR"));
        let mut planner = bare_planner(
            LlmPlannerConfig::new("m")
                .with_system_prompt("BASE")
                .with_system_prompt_refiner(refiner.clone()),
        );
        let channel = RecChannel::new();
        planner
            .begin_turn(&Message::text(channel.session, "   "), TurnId::new())
            .await;
        assert!(refiner.seen.lock().unwrap().is_empty());
        assert_eq!(planner.config.system_prompt.as_deref(), Some("BASE"));
    }

    #[tokio::test]
    async fn tool_call_path_resolves_name_via_registry_and_appends_history() {
        let tool = Arc::new(FakeTool::new("memory.read"));
        let tool_id = tool.id();

        let script = vec![FakeStep {
            events: vec![],
            terminal: LlmStepEnd::ToolCalls {
                calls: vec![ToolCallEnd {
                    call_id: "toolu_01".to_string(),
                    tool_name: "memory.read".to_string(),
                    input: json!({"query": "yesterday"}),
                    name_resolution: aivyx_llm::NameResolution::Known,
                }],
                text_so_far: String::new(),
                usage: zero_usage(),
            },
        }];
        let provider = FakeLlmProvider::new(script);
        let registry = Arc::new(ToolRegistry::new(vec![tool]));
        let mut planner = LlmPlanner::new(
            provider,
            registry,
            LlmPlannerConfig::new("claude-haiku-4-5-20251001"),
        );

        let channel = RecChannel::new();
        planner
            .begin_turn(&Message::text(channel.session, "recall"), TurnId::new())
            .await;
        let step = planner.next_step(&[], &channel).await;
        match step {
            NextStep::ToolCall {
                tool_id: returned,
                input,
                auto_corrected_from: None,
                extracted_from_text: None,
            } => {
                assert_eq!(returned, tool_id);
                assert_eq!(input, json!({"query": "yesterday"}));
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }

        // History has the assistant tool_use message recorded.
        let hist = planner.history();
        match &hist[1] {
            LlmMessage::Assistant { tool_calls, .. } => {
                assert_eq!(tool_calls.len(), 1);
                assert_eq!(tool_calls[0].call_id, "toolu_01");
                assert_eq!(tool_calls[0].tool_name, "memory.read");
            }
            other => panic!("expected Assistant at index 1, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn observe_tool_outcome_appends_success_result_to_history() {
        let tool = Arc::new(FakeTool::new("memory.read"));
        let tool_id = tool.id();

        let script = vec![FakeStep {
            events: vec![],
            terminal: LlmStepEnd::ToolCalls {
                calls: vec![ToolCallEnd {
                    call_id: "toolu_42".to_string(),
                    tool_name: "memory.read".to_string(),
                    input: json!({}),
                    name_resolution: aivyx_llm::NameResolution::Known,
                }],
                text_so_far: String::new(),
                usage: zero_usage(),
            },
        }];
        let provider = FakeLlmProvider::new(script);
        let registry = Arc::new(ToolRegistry::new(vec![tool]));
        let mut planner = LlmPlanner::new(
            provider,
            registry,
            LlmPlannerConfig::new("claude-haiku-4-5-20251001"),
        );

        let channel = RecChannel::new();
        planner
            .begin_turn(&Message::text(channel.session, "go"), TurnId::new())
            .await;
        let _ = planner.next_step(&[], &channel).await;

        let outcome = ToolOutcome::Completed {
            output: json!({"found": 3, "items": ["a", "b", "c"]}),
            verified: Verification::NotApplicable,
        };
        planner.observe_tool_outcome(tool_id, &outcome).await;

        let last = planner.history().last().unwrap();
        match last {
            LlmMessage::ToolResult {
                call_id,
                content,
                is_error,
            } => {
                assert_eq!(call_id, "toolu_42");
                assert!(!is_error);
                // The content should round-trip back to the original output.
                let parsed: Value = serde_json::from_str(content).unwrap();
                assert_eq!(parsed, json!({"found": 3, "items": ["a", "b", "c"]}));
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn tool_result_texts_collects_only_tool_result_content() {
        let tool = Arc::new(FakeTool::new("memory.read"));
        let tool_id = tool.id();
        let registry = Arc::new(ToolRegistry::new(vec![tool]));
        let mut planner = LlmPlanner::new(
            FakeLlmProvider::new(vec![]),
            registry,
            LlmPlannerConfig::new("claude-haiku-4-5-20251001"),
        );
        let outcome = ToolOutcome::Completed {
            output: json!({"registration": "VH-EZT"}),
            verified: Verification::NotApplicable,
        };
        planner.observe_tool_outcome(tool_id, &outcome).await;

        let texts = planner.tool_result_texts();
        assert_eq!(texts.len(), 1);
        assert!(texts[0].contains("VH-EZT"), "{texts:?}");
    }

    #[tokio::test]
    async fn observe_tool_outcome_nudges_after_three_consecutive_failures() {
        let tool = Arc::new(FakeTool::new("web_search"));
        let tool_id = tool.id();
        let registry = Arc::new(ToolRegistry::new(vec![tool]));
        let mut planner = LlmPlanner::new(
            FakeLlmProvider::new(vec![]),
            registry,
            LlmPlannerConfig::new("claude-haiku-4-5-20251001"),
        );
        let failure = ToolOutcome::Failed(AivyxError::Internal("search backend down".to_string()));

        planner.observe_tool_outcome(tool_id, &failure).await;
        planner.observe_tool_outcome(tool_id, &failure).await;
        planner.observe_tool_outcome(tool_id, &failure).await;

        let last = planner.history().last().unwrap();
        match last {
            LlmMessage::ToolResult {
                content, is_error, ..
            } => {
                assert!(*is_error);
                assert!(
                    content.contains("failed 3 times in a row"),
                    "3rd consecutive failure must carry the nudge: {content}"
                );
                assert!(
                    content.contains("web_search"),
                    "nudge names the tool: {content}"
                );
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn tool_result_texts_excludes_the_failure_nudge_text() {
        let tool = Arc::new(FakeTool::new("web_search"));
        let tool_id = tool.id();
        let registry = Arc::new(ToolRegistry::new(vec![tool]));
        let mut planner = LlmPlanner::new(
            FakeLlmProvider::new(vec![]),
            registry,
            LlmPlannerConfig::new("claude-haiku-4-5-20251001"),
        );
        let failure = ToolOutcome::Failed(AivyxError::Internal("down".to_string()));
        planner.observe_tool_outcome(tool_id, &failure).await;
        planner.observe_tool_outcome(tool_id, &failure).await;
        planner.observe_tool_outcome(tool_id, &failure).await; // 3rd — nudge appended

        let texts = planner.tool_result_texts();
        assert_eq!(texts.len(), 3);
        assert!(
            !texts[2].contains("SYSTEM NOTE"),
            "tool_result_texts must strip the nudge, got: {:?}",
            texts[2]
        );
    }

    #[tokio::test]
    async fn observe_tool_outcome_nudge_fires_once_not_on_every_later_failure() {
        let tool = Arc::new(FakeTool::new("web_search"));
        let tool_id = tool.id();
        let registry = Arc::new(ToolRegistry::new(vec![tool]));
        let mut planner = LlmPlanner::new(
            FakeLlmProvider::new(vec![]),
            registry,
            LlmPlannerConfig::new("claude-haiku-4-5-20251001"),
        );
        let failure = ToolOutcome::Failed(AivyxError::Internal("down".to_string()));
        for _ in 0..4 {
            planner.observe_tool_outcome(tool_id, &failure).await;
        }
        let last = planner.history().last().unwrap();
        match last {
            LlmMessage::ToolResult { content, .. } => {
                assert!(
                    !content.contains("failed 3 times in a row"),
                    "the 4th consecutive failure must not repeat the nudge: {content}"
                );
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn observe_tool_outcome_failure_streak_resets_on_different_tool() {
        let tool_a = Arc::new(FakeTool::new("web_search"));
        let tool_b = Arc::new(FakeTool::new("web.fetch"));
        let (id_a, id_b) = (tool_a.id(), tool_b.id());
        let registry = Arc::new(ToolRegistry::new(vec![tool_a, tool_b]));
        let mut planner = LlmPlanner::new(
            FakeLlmProvider::new(vec![]),
            registry,
            LlmPlannerConfig::new("claude-haiku-4-5-20251001"),
        );
        let failure = ToolOutcome::Failed(AivyxError::Internal("down".to_string()));
        planner.observe_tool_outcome(id_a, &failure).await;
        planner.observe_tool_outcome(id_a, &failure).await;
        planner.observe_tool_outcome(id_b, &failure).await; // different tool — resets id_a's streak
        planner.observe_tool_outcome(id_a, &failure).await;
        let last = planner.history().last().unwrap();
        match last {
            LlmMessage::ToolResult { content, .. } => {
                assert!(
                    !content.contains("failed 3 times in a row"),
                    "a different tool's failure must reset the streak: {content}"
                );
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn observe_tool_outcome_failure_streak_resets_on_success() {
        let tool = Arc::new(FakeTool::new("web_search"));
        let tool_id = tool.id();
        let registry = Arc::new(ToolRegistry::new(vec![tool]));
        let mut planner = LlmPlanner::new(
            FakeLlmProvider::new(vec![]),
            registry,
            LlmPlannerConfig::new("claude-haiku-4-5-20251001"),
        );
        let failure = ToolOutcome::Failed(AivyxError::Internal("down".to_string()));
        let success = ToolOutcome::Completed {
            output: json!({}),
            verified: Verification::NotApplicable,
        };
        planner.observe_tool_outcome(tool_id, &failure).await;
        planner.observe_tool_outcome(tool_id, &failure).await;
        planner.observe_tool_outcome(tool_id, &success).await; // resets
        planner.observe_tool_outcome(tool_id, &failure).await;
        planner.observe_tool_outcome(tool_id, &failure).await;
        let last = planner.history().last().unwrap();
        match last {
            LlmMessage::ToolResult { content, .. } => {
                assert!(
                    !content.contains("failed 3 times in a row"),
                    "a success must reset the streak: {content}"
                );
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn observe_tool_outcome_serializes_denied_as_structured_error() {
        let tool = Arc::new(FakeTool::new("shell.exec"));
        let tool_id = tool.id();

        let script = vec![FakeStep {
            events: vec![],
            terminal: LlmStepEnd::ToolCalls {
                calls: vec![ToolCallEnd {
                    call_id: "toolu_denied".to_string(),
                    tool_name: "shell.exec".to_string(),
                    input: json!({}),
                    name_resolution: aivyx_llm::NameResolution::Known,
                }],
                text_so_far: String::new(),
                usage: zero_usage(),
            },
        }];
        let provider = FakeLlmProvider::new(script);
        let registry = Arc::new(ToolRegistry::new(vec![tool]));
        let mut planner = LlmPlanner::new(
            provider,
            registry,
            LlmPlannerConfig::new("claude-haiku-4-5-20251001"),
        );

        let channel = RecChannel::new();
        planner
            .begin_turn(&Message::text(channel.session, "run stuff"), TurnId::new())
            .await;
        let _ = planner.next_step(&[], &channel).await;

        let outcome = ToolOutcome::Denied {
            scope: Scope::parse("shell.exec:rm").unwrap(),
            held: aivyx_capability::CapabilitySet::empty(),
        };
        planner.observe_tool_outcome(tool_id, &outcome).await;

        let last = planner.history().last().unwrap();
        match last {
            LlmMessage::ToolResult {
                content, is_error, ..
            } => {
                assert!(*is_error);
                let parsed: Value = serde_json::from_str(content).unwrap();
                assert_eq!(parsed["error"], "denied");
                assert!(
                    parsed["message"]
                        .as_str()
                        .unwrap()
                        .contains("shell.exec:rm")
                );
            }
            other => panic!("expected ToolResult, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unknown_tool_name_synthesizes_error_and_retries() {
        // Script: first chat_stream returns ToolCall with an unknown
        // name; second chat_stream returns a FinalMessage. Planner
        // should NOT surface an error — it should append the synthetic
        // error tool_result and loop internally.
        let known = Arc::new(FakeTool::new("memory.read"));
        let script = vec![
            FakeStep {
                events: vec![],
                terminal: LlmStepEnd::ToolCalls {
                    calls: vec![ToolCallEnd {
                        call_id: "toolu_bad".to_string(),
                        tool_name: "does.not.exist".to_string(),
                        input: json!({}),
                        name_resolution: aivyx_llm::NameResolution::Known,
                    }],
                    text_so_far: String::new(),
                    usage: zero_usage(),
                },
            },
            FakeStep {
                events: vec![],
                terminal: LlmStepEnd::FinalMessage {
                    text: "giving up".to_string(),
                    usage: zero_usage(),
                },
            },
        ];
        let provider = FakeLlmProvider::new(script);
        let registry = Arc::new(ToolRegistry::new(vec![known]));
        let mut planner = LlmPlanner::new(
            provider,
            registry,
            LlmPlannerConfig::new("claude-haiku-4-5-20251001"),
        );

        let channel = RecChannel::new();
        planner
            .begin_turn(&Message::text(channel.session, "help"), TurnId::new())
            .await;
        let step = planner.next_step(&[], &channel).await;
        assert!(matches!(step, NextStep::FinalMessage(ref m) if m == "giving up"));

        // The history should contain the synthetic error tool_result.
        let has_unknown = planner.history().iter().any(|m| match m {
            LlmMessage::ToolResult {
                content, is_error, ..
            } => *is_error && content.contains("unknown_tool"),
            _ => false,
        });
        assert!(has_unknown, "expected a synthetic unknown_tool entry");
    }

    // ----- Phase 120 — Tool-name fuzzy recovery + auto-correction audit -----

    #[tokio::test]
    async fn phase_120_fuzzy_recovery_dispatches_close_match() {
        // qwen3.6:27b emits `fs_read`; the registered tool is `fs.read`.
        // title_similarity("fs_read", "fs.read") = 1.0 (same tokens
        // after separator normalization) → above the 0.80 threshold
        // → the planner dispatches `fs.read` and records the verbatim
        // `fs_read` as auto_corrected_from.
        let fs_read = Arc::new(FakeTool::new("fs.read"));
        let fs_read_id = fs_read.id();
        let script = vec![FakeStep {
            events: vec![],
            terminal: LlmStepEnd::ToolCalls {
                calls: vec![ToolCallEnd {
                    call_id: "c1".into(),
                    tool_name: "fs_read".into(),
                    input: json!({}),
                    // Provider flagged Unknown; planner takes over.
                    name_resolution: aivyx_llm::NameResolution::Unknown {
                        original: "fs_read".into(),
                    },
                }],
                text_so_far: String::new(),
                usage: zero_usage(),
            },
        }];
        let provider = FakeLlmProvider::new(script);
        let registry = Arc::new(ToolRegistry::new(vec![fs_read]));
        let mut planner = LlmPlanner::new(provider, registry, LlmPlannerConfig::new("local-qwen"));
        let channel = RecChannel::new();
        planner
            .begin_turn(
                &Message::text(channel.session, "read a file"),
                TurnId::new(),
            )
            .await;
        let step = planner.next_step(&[], &channel).await;
        match step {
            NextStep::ToolCall {
                tool_id,
                auto_corrected_from,
                ..
            } => {
                assert_eq!(tool_id, fs_read_id);
                assert_eq!(auto_corrected_from.as_deref(), Some("fs_read"));
            }
            other => panic!("expected ToolCall with auto-correction, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn phase_120_below_threshold_synthesizes_unknown_tool_error() {
        // `do_the_thing` vs registered `memory.read` has too few
        // shared tokens to clear 0.80 (1/4 Jaccard). Fall-through
        // path: synthetic unknown_tool error message; loop continues
        // so the model can retry.
        let memory_read = Arc::new(FakeTool::new("memory.read"));
        let script = vec![
            FakeStep {
                events: vec![],
                terminal: LlmStepEnd::ToolCalls {
                    calls: vec![ToolCallEnd {
                        call_id: "c1".into(),
                        tool_name: "do_the_thing".into(),
                        input: json!({}),
                        name_resolution: aivyx_llm::NameResolution::Unknown {
                            original: "do_the_thing".into(),
                        },
                    }],
                    text_so_far: String::new(),
                    usage: zero_usage(),
                },
            },
            FakeStep {
                events: vec![],
                terminal: LlmStepEnd::FinalMessage {
                    text: "giving up".into(),
                    usage: zero_usage(),
                },
            },
        ];
        let provider = FakeLlmProvider::new(script);
        let registry = Arc::new(ToolRegistry::new(vec![memory_read]));
        let mut planner = LlmPlanner::new(provider, registry, LlmPlannerConfig::new("local-qwen"));
        let channel = RecChannel::new();
        planner
            .begin_turn(&Message::text(channel.session, "do it"), TurnId::new())
            .await;
        let step = planner.next_step(&[], &channel).await;
        // Loop continued past the unknown call and reached
        // FinalMessage on the next chat_stream.
        assert!(matches!(step, NextStep::FinalMessage(ref m) if m == "giving up"));
        // History carries the synthetic error.
        let has_unknown = planner.history().iter().any(|m| match m {
            LlmMessage::ToolResult {
                content, is_error, ..
            } => *is_error && content.contains("unknown_tool"),
            _ => false,
        });
        assert!(
            has_unknown,
            "below-threshold path must synthesize unknown_tool"
        );
    }

    #[tokio::test]
    async fn phase_120_known_name_dispatches_with_no_auto_correction() {
        // The dominant case: model emits a registered name verbatim;
        // no recovery needed; auto_corrected_from is None.
        let fs_read = Arc::new(FakeTool::new("fs.read"));
        let fs_read_id = fs_read.id();
        let script = vec![FakeStep {
            events: vec![],
            terminal: LlmStepEnd::ToolCalls {
                calls: vec![ToolCallEnd {
                    call_id: "c1".into(),
                    tool_name: "fs.read".into(),
                    input: json!({}),
                    name_resolution: aivyx_llm::NameResolution::Known,
                }],
                text_so_far: String::new(),
                usage: zero_usage(),
            },
        }];
        let provider = FakeLlmProvider::new(script);
        let registry = Arc::new(ToolRegistry::new(vec![fs_read]));
        let mut planner = LlmPlanner::new(provider, registry, LlmPlannerConfig::new("m"));
        let channel = RecChannel::new();
        planner
            .begin_turn(&Message::text(channel.session, "read"), TurnId::new())
            .await;
        let step = planner.next_step(&[], &channel).await;
        match step {
            NextStep::ToolCall {
                tool_id,
                auto_corrected_from,
                ..
            } => {
                assert_eq!(tool_id, fs_read_id);
                assert!(
                    auto_corrected_from.is_none(),
                    "verbatim Known dispatch must NOT report an auto-correction"
                );
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn phase_120_fuzzy_recover_picks_best_match() {
        // Threshold-pinning unit test for the pure helper. With both
        // fs.read and web.fetch registered, an emitted `fs_read`
        // resolves to fs.read (not web.fetch).
        let fs_read = Arc::new(FakeTool::new("fs.read")) as Arc<dyn Tool>;
        let web_fetch = Arc::new(FakeTool::new("web.fetch")) as Arc<dyn Tool>;
        let fs_id = fs_read.id();
        let registry = ToolRegistry::new(vec![fs_read, web_fetch]);
        let resolved = fuzzy_recover_tool_name(&registry, "fs_read", 0.80);
        assert_eq!(resolved, Some(fs_id));
    }

    #[test]
    fn phase_120_fuzzy_recover_returns_none_when_no_match_clears_threshold() {
        let memory_read = Arc::new(FakeTool::new("memory.read")) as Arc<dyn Tool>;
        let registry = ToolRegistry::new(vec![memory_read]);
        // do_the_thing vs memory.read → Jaccard 0/5 = 0 < 0.80.
        let resolved = fuzzy_recover_tool_name(&registry, "do_the_thing", 0.80);
        assert!(resolved.is_none());
    }

    #[test]
    fn phase_120_fuzzy_recover_returns_none_for_empty_registry() {
        let registry = ToolRegistry::new(vec![]);
        let resolved = fuzzy_recover_tool_name(&registry, "fs_read", 0.80);
        assert!(resolved.is_none());
    }

    #[test]
    fn phase_120_planner_config_default_threshold_matches_const() {
        // The LlmPlannerConfig::new default plumbs through the
        // FUZZY_TOOL_NAME_THRESHOLD const; the config layer's
        // DEFAULT_TOOL_NAME_AUTO_CORRECT_THRESHOLD is the same value.
        let config = LlmPlannerConfig::new("m");
        assert!((config.tool_name_auto_correct_threshold - FUZZY_TOOL_NAME_THRESHOLD).abs() < 1e-6);
    }

    #[test]
    fn phase_120_with_tool_name_auto_correct_threshold_overrides() {
        let config = LlmPlannerConfig::new("m").with_tool_name_auto_correct_threshold(0.55);
        assert!((config.tool_name_auto_correct_threshold - 0.55).abs() < 1e-6);
    }

    // ----- Phase 120 Task 6 — "Did you mean?" suggestions -----

    #[test]
    fn phase_120_top_n_orders_by_similarity_descending() {
        // `fs_read` against {fs.read, fs.write, web.fetch}:
        //   tokens(fs_read) = {fs, read}
        //   - fs.read  → {fs, read} → 1.0 (perfect)
        //   - fs.write → {fs, write} → 1/3
        //   - web.fetch → {web, fetch} → 0/4
        // Top-3 must rank fs.read, fs.write, web.fetch in that order.
        let fs_read = Arc::new(FakeTool::new("fs.read")) as Arc<dyn Tool>;
        let fs_write = Arc::new(FakeTool::new("fs.write")) as Arc<dyn Tool>;
        let web_fetch = Arc::new(FakeTool::new("web.fetch")) as Arc<dyn Tool>;
        let registry = ToolRegistry::new(vec![fs_read, fs_write, web_fetch]);
        let top = top_n_similar_tools(&registry, "fs_read", 3);
        assert_eq!(top.len(), 3);
        assert_eq!(top[0].0, "fs.read");
        assert_eq!(top[1].0, "fs.write");
        assert_eq!(top[2].0, "web.fetch");
        // Scores descending.
        assert!(top[0].1 > top[1].1);
        assert!(top[1].1 > top[2].1);
    }

    #[test]
    fn phase_120_top_n_caps_at_n() {
        let tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(FakeTool::new("fs.read")),
            Arc::new(FakeTool::new("fs.write")),
            Arc::new(FakeTool::new("web.fetch")),
            Arc::new(FakeTool::new("memory.read")),
            Arc::new(FakeTool::new("memory.write")),
        ];
        let registry = ToolRegistry::new(tools);
        let top = top_n_similar_tools(&registry, "fs_read", 3);
        assert_eq!(top.len(), 3, "top-3 cap must hold under 5-tool registry");
    }

    #[test]
    fn phase_120_top_n_returns_empty_for_empty_registry() {
        let registry = ToolRegistry::new(vec![]);
        let top = top_n_similar_tools(&registry, "fs_read", 3);
        assert!(top.is_empty());
    }

    #[test]
    fn phase_120_build_unknown_message_includes_suggestions() {
        let suggestions = vec![
            ("fs.read".to_string(), 1.0),
            ("fs.write".to_string(), 0.5),
            ("memory.read".to_string(), 0.33),
        ];
        let msg = build_unknown_tool_message("fs_read", &suggestions);
        // The emitted name appears.
        assert!(msg.contains("'fs_read'"));
        // "Did you mean?" prefix appears.
        assert!(msg.contains("Did you mean"));
        // All three suggestions appear in order.
        let pos_a = msg.find("'fs.read'").unwrap();
        let pos_b = msg.find("'fs.write'").unwrap();
        let pos_c = msg.find("'memory.read'").unwrap();
        assert!(pos_a < pos_b);
        assert!(pos_b < pos_c);
    }

    #[test]
    fn phase_120_build_unknown_message_falls_back_for_empty_registry() {
        // Empty suggestions → neutral fallback. The model should NOT
        // see a misleading "Did you mean ?" form.
        let msg = build_unknown_tool_message("fs_read", &[]);
        assert!(msg.contains("'fs_read'"));
        assert!(msg.contains("no tools available"));
        // Critical: no misleading "Did you mean?" with empty list.
        assert!(!msg.contains("Did you mean"));
    }

    #[tokio::test]
    async fn phase_120_below_threshold_includes_did_you_mean_in_history() {
        // End-to-end: model emits below-threshold name; planner
        // synthesizes unknown_tool ToolResult; the JSON body
        // includes a `did_you_mean` field with the ranked
        // suggestions. The model can parse that field on its next
        // turn and retry with the right name.
        let fs_read = Arc::new(FakeTool::new("fs.read"));
        let web_fetch = Arc::new(FakeTool::new("web.fetch"));
        let script = vec![
            FakeStep {
                events: vec![],
                terminal: LlmStepEnd::ToolCalls {
                    calls: vec![ToolCallEnd {
                        call_id: "c1".into(),
                        // do_the_thing is below threshold against
                        // either fs.read or web.fetch (1/4 max).
                        tool_name: "do_the_thing".into(),
                        input: json!({}),
                        name_resolution: aivyx_llm::NameResolution::Unknown {
                            original: "do_the_thing".into(),
                        },
                    }],
                    text_so_far: String::new(),
                    usage: zero_usage(),
                },
            },
            FakeStep {
                events: vec![],
                terminal: LlmStepEnd::FinalMessage {
                    text: "ok".into(),
                    usage: zero_usage(),
                },
            },
        ];
        let provider = FakeLlmProvider::new(script);
        let registry = Arc::new(ToolRegistry::new(vec![fs_read, web_fetch]));
        let mut planner = LlmPlanner::new(provider, registry, LlmPlannerConfig::new("m"));
        let channel = RecChannel::new();
        planner
            .begin_turn(&Message::text(channel.session, "do it"), TurnId::new())
            .await;
        let _ = planner.next_step(&[], &channel).await;
        // History carries an unknown_tool ToolResult whose JSON
        // body includes a did_you_mean field.
        let body = planner
            .history()
            .iter()
            .find_map(|m| match m {
                LlmMessage::ToolResult {
                    content, is_error, ..
                } if *is_error && content.contains("unknown_tool") => Some(content.clone()),
                _ => None,
            })
            .expect("expected synthetic unknown_tool entry");
        let parsed: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(parsed["error"], "unknown_tool");
        // did_you_mean array carries the top-3 (or fewer) names.
        let suggestions = parsed["did_you_mean"]
            .as_array()
            .expect("did_you_mean must be an array");
        assert!(!suggestions.is_empty());
        // The "Did you mean" phrasing is part of the human-readable
        // message too.
        assert!(
            parsed["message"].as_str().unwrap().contains("Did you mean"),
            "human message must include 'Did you mean': {body}"
        );
    }

    #[tokio::test]
    async fn phase_120_zero_threshold_disables_fuzzy_recovery() {
        // Operator sets the threshold to 0.0 — wait, 0.0 means
        // "every match clears", which would auto-correct EVERYTHING
        // (including unrelated names). The semantically conservative
        // disable is threshold = 1.0 (exact-match only). Test that
        // posture: with threshold = 1.0, an emitted `fs_read` (Jaccard
        // 1.0 vs `fs.read`) STILL clears (1.0 >= 1.0); but `fs_rea`
        // (Jaccard 0.5) does NOT. Pin the inclusive-bound semantics.
        let fs_read = Arc::new(FakeTool::new("fs.read"));
        let script = vec![
            FakeStep {
                events: vec![],
                terminal: LlmStepEnd::ToolCalls {
                    calls: vec![ToolCallEnd {
                        call_id: "c1".into(),
                        tool_name: "fs_rea".into(), // partial — Jaccard 0.5
                        input: json!({}),
                        name_resolution: aivyx_llm::NameResolution::Unknown {
                            original: "fs_rea".into(),
                        },
                    }],
                    text_so_far: String::new(),
                    usage: zero_usage(),
                },
            },
            FakeStep {
                events: vec![],
                terminal: LlmStepEnd::FinalMessage {
                    text: "giving up".into(),
                    usage: zero_usage(),
                },
            },
        ];
        let provider = FakeLlmProvider::new(script);
        let registry = Arc::new(ToolRegistry::new(vec![fs_read]));
        let mut planner = LlmPlanner::new(
            provider,
            registry,
            LlmPlannerConfig::new("m").with_tool_name_auto_correct_threshold(1.0),
        );
        let channel = RecChannel::new();
        planner
            .begin_turn(&Message::text(channel.session, "read"), TurnId::new())
            .await;
        let step = planner.next_step(&[], &channel).await;
        // Partial match doesn't clear threshold 1.0 → unknown_tool.
        assert!(matches!(step, NextStep::FinalMessage(ref m) if m == "giving up"));
        let has_unknown = planner.history().iter().any(|m| match m {
            LlmMessage::ToolResult {
                content, is_error, ..
            } => *is_error && content.contains("unknown_tool"),
            _ => false,
        });
        assert!(
            has_unknown,
            "threshold 1.0 must fail-through for partial matches"
        );
    }

    #[tokio::test]
    async fn provider_error_surfaces_as_final_message() {
        let script: Vec<FakeStep> = vec![]; // immediately exhausted
        let provider = FakeLlmProvider::new(script);
        let registry = Arc::new(ToolRegistry::new(vec![]));
        let mut planner = LlmPlanner::new(
            provider,
            registry,
            LlmPlannerConfig::new("claude-haiku-4-5-20251001"),
        );

        let channel = RecChannel::new();
        planner
            .begin_turn(&Message::text(channel.session, "hi"), TurnId::new())
            .await;
        let step = planner.next_step(&[], &channel).await;
        match step {
            NextStep::FinalMessage(m) => assert!(m.starts_with("LLM error:")),
            other => panic!("expected FinalMessage, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Mid-stream cancellation — Phase 3 task 4. The planner's one_step
    // loop races stream events against `cancellation.cancelled()`. When
    // the channel's token flips to cancelled while the stream is still
    // yielding, the planner must drop the stream, surface
    // `LlmError::Cancelled`, and `next_step` must translate that into
    // `NextStep::Stop` (not a FinalMessage — doing so would misleadingly
    // complete the turn).
    // -----------------------------------------------------------------------

    /// Provider whose stream blocks forever on `next_event`. The only
    /// way a turn that uses it can terminate is via cancellation of the
    /// channel's token.
    struct BlockingProvider;

    #[async_trait]
    impl LlmProvider for BlockingProvider {
        async fn chat_stream(
            &self,
            _request: LlmRequest<'_>,
            _cancellation: &crate::CancellationToken,
        ) -> Result<Box<dyn LlmStream>, LlmError> {
            Ok(Box::new(BlockingStream))
        }
    }

    struct BlockingStream;

    #[async_trait]
    impl LlmStream for BlockingStream {
        async fn next_event(&mut self) -> Result<Option<LlmStreamEvent>, LlmError> {
            // Never resolves. The `tokio::select!` in `one_step` must
            // always pick the cancellation branch to let the caller
            // make progress.
            std::future::pending::<()>().await;
            unreachable!()
        }
        async fn finish(self: Box<Self>) -> Result<LlmStepEnd, LlmError> {
            // finish() shouldn't be reached on the cancel path, but if
            // it is, report it loudly so the test catches the misroute.
            Err(LlmError::StreamEnded(
                "BlockingStream::finish reached".into(),
            ))
        }
    }

    #[tokio::test]
    async fn mid_stream_cancel_returns_next_step_stop() {
        let provider = Arc::new(BlockingProvider);
        let registry = Arc::new(ToolRegistry::new(vec![]));
        let mut planner = LlmPlanner::new(
            provider,
            registry,
            LlmPlannerConfig::new("claude-haiku-4-5-20251001"),
        );

        let channel = RecChannel::new();
        // Spawn a task that cancels the channel's token shortly after
        // the planner starts draining the stream. Yielding once
        // guarantees we enter `one_step` before the cancel fires.
        let token = channel.token.clone();
        tokio::spawn(async move {
            tokio::task::yield_now().await;
            token.cancel();
        });

        planner
            .begin_turn(&Message::text(channel.session, "hi"), TurnId::new())
            .await;
        let step = planner.next_step(&[], &channel).await;

        assert!(
            matches!(step, NextStep::Stop),
            "mid-stream cancel must surface as NextStep::Stop, got {step:?}"
        );
    }

    #[tokio::test]
    async fn failed_outcome_produces_failed_envelope() {
        // Direct unit test of render_tool_result — no planner needed.
        let outcome = ToolOutcome::Failed(AivyxError::Internal("boom".to_string()));
        let (content, is_error) = render_tool_result(&outcome);
        assert!(is_error);
        let parsed: Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["error"], "failed");
        assert!(parsed["message"].as_str().unwrap().contains("boom"));
    }

    #[tokio::test]
    async fn rate_limited_outcome_produces_rate_limited_envelope() {
        // TH.2 — a throttled call renders a distinct `rate_limited` error the
        // model can adapt to, carrying the breached-limit reason.
        let outcome = ToolOutcome::RateLimited {
            tool_name: "web.fetch".to_string(),
            reason: "per-turn cap for `web.fetch` reached: 6 of 6".to_string(),
        };
        let (content, is_error) = render_tool_result(&outcome);
        assert!(is_error);
        let parsed: Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["error"], "rate_limited");
        assert!(parsed["message"].as_str().unwrap().contains("web.fetch"));
        assert!(parsed["message"].as_str().unwrap().contains("per-turn cap"));
        // Forensically distinct from capability / role denials.
        let summary = crate::ToolOutcomeSummary::from(&outcome);
        assert_eq!(summary, crate::ToolOutcomeSummary::RateLimited);
        assert_ne!(summary, crate::ToolOutcomeSummary::Denied);
        assert_ne!(summary, crate::ToolOutcomeSummary::NotInRole);
    }

    // -----------------------------------------------------------------------
    // Phase 40 — multi-tool ToolCalls produces NextStep::ToolCalls batch
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn multi_tool_calls_returns_next_step_tool_calls_batch() {
        let tool_a = Arc::new(FakeTool::new("fs.read"));
        let tool_b = Arc::new(FakeTool::new("memory.read"));
        let tool_a_id = tool_a.id();
        let tool_b_id = tool_b.id();

        let script = vec![FakeStep {
            events: vec![],
            terminal: LlmStepEnd::ToolCalls {
                calls: vec![
                    ToolCallEnd {
                        call_id: "toolu_a".to_string(),
                        tool_name: "fs.read".to_string(),
                        input: json!({"path": "/x"}),
                        name_resolution: aivyx_llm::NameResolution::Known,
                    },
                    ToolCallEnd {
                        call_id: "toolu_b".to_string(),
                        tool_name: "memory.read".to_string(),
                        input: json!({"topic": "notes"}),
                        name_resolution: aivyx_llm::NameResolution::Known,
                    },
                ],
                text_so_far: String::new(),
                usage: zero_usage(),
            },
        }];
        let provider = FakeLlmProvider::new(script);
        let registry = Arc::new(ToolRegistry::new(vec![tool_a, tool_b]));
        let mut planner = LlmPlanner::new(
            provider,
            registry,
            LlmPlannerConfig::new("claude-haiku-4-5-20251001"),
        );

        let channel = RecChannel::new();
        planner
            .begin_turn(&Message::text(channel.session, "do both"), TurnId::new())
            .await;
        let step = planner.next_step(&[], &channel).await;

        match step {
            NextStep::ToolCalls(batch) => {
                assert_eq!(batch.len(), 2);
                assert_eq!(batch[0].tool_id, tool_a_id);
                assert_eq!(batch[0].input, json!({"path": "/x"}));
                assert_eq!(batch[1].tool_id, tool_b_id);
                assert_eq!(batch[1].input, json!({"topic": "notes"}));
            }
            other => panic!("expected ToolCalls batch, got {other:?}"),
        }

        // Verify history: assistant has 2 tool_calls.
        let hist = planner.history();
        match &hist[1] {
            LlmMessage::Assistant { tool_calls, .. } => {
                assert_eq!(tool_calls.len(), 2);
            }
            other => panic!("expected Assistant, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn multi_tool_with_unknown_executes_known_and_errors_unknown() {
        // 3 calls: 2 known, 1 unknown. Should return ToolCalls with the
        // 2 known tools and append a synthetic error ToolResult for the unknown.
        let tool_a = Arc::new(FakeTool::new("fs.read"));
        let tool_b = Arc::new(FakeTool::new("memory.read"));
        let tool_a_id = tool_a.id();
        let tool_b_id = tool_b.id();

        let script = vec![FakeStep {
            events: vec![],
            terminal: LlmStepEnd::ToolCalls {
                calls: vec![
                    ToolCallEnd {
                        call_id: "toolu_a".to_string(),
                        tool_name: "fs.read".to_string(),
                        input: json!({}),
                        name_resolution: aivyx_llm::NameResolution::Known,
                    },
                    ToolCallEnd {
                        call_id: "toolu_bad".to_string(),
                        tool_name: "does.not.exist".to_string(),
                        input: json!({}),
                        name_resolution: aivyx_llm::NameResolution::Known,
                    },
                    ToolCallEnd {
                        call_id: "toolu_b".to_string(),
                        tool_name: "memory.read".to_string(),
                        input: json!({}),
                        name_resolution: aivyx_llm::NameResolution::Known,
                    },
                ],
                text_so_far: String::new(),
                usage: zero_usage(),
            },
        }];
        let provider = FakeLlmProvider::new(script);
        let registry = Arc::new(ToolRegistry::new(vec![tool_a, tool_b]));
        let mut planner = LlmPlanner::new(
            provider,
            registry,
            LlmPlannerConfig::new("claude-haiku-4-5-20251001"),
        );

        let channel = RecChannel::new();
        planner
            .begin_turn(&Message::text(channel.session, "go"), TurnId::new())
            .await;
        let step = planner.next_step(&[], &channel).await;

        match step {
            NextStep::ToolCalls(batch) => {
                assert_eq!(batch.len(), 2);
                assert_eq!(batch[0].tool_id, tool_a_id);
                assert_eq!(batch[1].tool_id, tool_b_id);
            }
            other => panic!("expected ToolCalls batch, got {other:?}"),
        }

        // The unknown tool's error result is already in history.
        let hist = planner.history();
        let tool_result = &hist[2]; // index 0 = user, 1 = assistant, 2 = tool_result
        match tool_result {
            LlmMessage::ToolResult {
                call_id, is_error, ..
            } => {
                assert_eq!(call_id, "toolu_bad");
                assert!(is_error);
            }
            other => panic!("expected ToolResult for unknown tool, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Phase 43 Task 2 — context window config
    // -----------------------------------------------------------------------

    #[test]
    fn context_window_defaults_to_none() {
        let config = LlmPlannerConfig::new("test-model");
        assert_eq!(config.context_window_tokens, None);
    }

    #[test]
    fn context_window_builder() {
        let config = LlmPlannerConfig::new("test-model").with_context_window(200_000);
        assert_eq!(config.context_window_tokens, Some(200_000));
    }

    // -----------------------------------------------------------------------
    // Phase 43 Task 3 — context window pruning
    // -----------------------------------------------------------------------

    /// Helper: build a planner with a tiny context window, pre-seed
    /// history with known messages, then call `next_step` so the
    /// pruning logic runs.
    fn make_pruning_planner(
        window_tokens: usize,
        messages: Vec<LlmMessage>,
        reply: &str,
    ) -> (LlmPlanner, RecChannel) {
        let script = vec![FakeStep {
            events: vec![],
            terminal: LlmStepEnd::FinalMessage {
                text: reply.to_string(),
                usage: zero_usage(),
            },
        }];
        let provider = FakeLlmProvider::new(script);
        let registry = Arc::new(ToolRegistry::new(vec![]));
        let config = LlmPlannerConfig::new("test").with_context_window(window_tokens);
        let mut planner = LlmPlanner::new(provider, registry, config);
        planner.history = messages;
        (planner, RecChannel::new())
    }

    #[tokio::test]
    async fn pruning_skipped_when_under_budget() {
        // 5 short messages, generous context window — no pruning.
        let msgs: Vec<LlmMessage> = (0..5)
            .map(|i| LlmMessage::user_text(format!("msg{i}")))
            .collect();
        let (mut planner, ch) = make_pruning_planner(200_000, msgs, "ok");
        planner.next_step(&[], &ch).await;
        assert_eq!(planner.pruned_message_count(), 0);
        // 5 original + 1 assistant reply (no sentinel inserted).
        assert_eq!(planner.history().len(), 6);
    }

    #[tokio::test]
    async fn pruning_drops_oldest_when_over_budget() {
        // Each "x".repeat(100) message ≈ 25 tokens.
        // 10 messages ≈ 250 tokens. Set window to 200 → budget = 160.
        // Pruning should drop some messages.
        let msgs: Vec<LlmMessage> = (0..10)
            .map(|i| LlmMessage::user_text(format!("message-{i}-{}", "x".repeat(100))))
            .collect();
        let (mut planner, ch) = make_pruning_planner(200, msgs, "ok");
        planner.next_step(&[], &ch).await;
        assert!(planner.pruned_message_count() > 0);
        // First message in history should be the sentinel.
        match &planner.history()[0] {
            LlmMessage::User { content } => {
                let text = match &content[0] {
                    ContentBlock::Text { text } => text,
                    other => panic!("expected Text block, got {other:?}"),
                };
                assert!(text.contains("[Earlier context pruned:"));
            }
            other => panic!("expected User sentinel, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn pruning_preserves_at_least_last_message() {
        // Extremely small window (10 tokens = 40 chars). Even a
        // single message exceeds budget, but we never prune the last
        // message. Two messages in: we should prune one and keep one
        // (plus sentinel).
        let msgs = vec![
            LlmMessage::user_text("a]".repeat(50)), // ~25 tokens
            LlmMessage::user_text("b".repeat(200)), // ~50 tokens
        ];
        let (mut planner, ch) = make_pruning_planner(10, msgs, "ok");
        planner.next_step(&[], &ch).await;
        assert_eq!(planner.pruned_message_count(), 1);
        // History: sentinel + last-original + assistant-reply = 3.
        assert_eq!(planner.history().len(), 3);
    }

    #[tokio::test]
    async fn pruning_pins_the_turns_task_message() {
        // Live-rig failure shape (2026-07-05): the task question is
        // the OLDEST turn-local message and big tool results follow,
        // so naive oldest-first pruning discarded the turn's own
        // question and the model reset to a greeter reply. The pin
        // re-inserts the task right after the sentinel.
        let mut msgs = vec![LlmMessage::user_text("what is the cruise speed?")];
        for i in 0..6 {
            msgs.push(LlmMessage::ToolResult {
                call_id: format!("c{i}"),
                content: "h".repeat(400), // ~100 tokens each
                is_error: false,
            });
        }
        let (mut planner, ch) = make_pruning_planner(150, msgs, "ok");
        planner.task_message_index = Some(0);
        planner.next_step(&[], &ch).await;
        assert!(planner.pruned_message_count() > 0);
        // Sentinel first, the rescued task right after it.
        match &planner.history()[1] {
            LlmMessage::User { content } => {
                assert_eq!(content[0], ContentBlock::text("what is the cruise speed?"));
            }
            other => panic!("expected pinned task, got {other:?}"),
        }
        assert_eq!(planner.task_message_index, Some(1));
    }

    #[tokio::test]
    async fn pruning_shifts_a_surviving_task_index() {
        // Task near the tail survives the drain — its index must
        // shift by (pruned prefix - inserted sentinel).
        let mut msgs: Vec<LlmMessage> = (0..5)
            .map(|_| LlmMessage::user_text("x".repeat(400)))
            .collect();
        msgs.push(LlmMessage::user_text("the task"));
        msgs.push(LlmMessage::ToolResult {
            call_id: "c0".into(),
            content: "y".repeat(200),
            is_error: false,
        });
        let (mut planner, ch) = make_pruning_planner(200, msgs, "ok");
        planner.task_message_index = Some(5);
        planner.next_step(&[], &ch).await;
        let idx = planner.task_message_index.unwrap();
        match &planner.history()[idx] {
            LlmMessage::User { content } => {
                assert_eq!(content[0], ContentBlock::text("the task"));
            }
            other => panic!("expected task at shifted index, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn pruning_skipped_when_no_context_window() {
        // No context window configured → pruning never triggers.
        let msgs: Vec<LlmMessage> = (0..20)
            .map(|_| LlmMessage::user_text("x".repeat(1000)))
            .collect();
        let script = vec![FakeStep {
            events: vec![],
            terminal: LlmStepEnd::FinalMessage {
                text: "ok".to_string(),
                usage: zero_usage(),
            },
        }];
        let provider = FakeLlmProvider::new(script);
        let registry = Arc::new(ToolRegistry::new(vec![]));
        let config = LlmPlannerConfig::new("test"); // no .with_context_window()
        let mut planner = LlmPlanner::new(provider, registry, config);
        planner.history = msgs;
        let ch = RecChannel::new();
        planner.next_step(&[], &ch).await;
        assert_eq!(planner.pruned_message_count(), 0);
        // 20 original + 1 reply
        assert_eq!(planner.history().len(), 21);
    }

    #[tokio::test]
    async fn pruning_accumulates_across_steps() {
        // Two LLM calls in one turn (tool call → final). Each call
        // prunes. We verify the counter accumulates.
        let tool = Arc::new(FakeTool::new("echo"));
        let tool_id = tool.id();
        let script = vec![
            FakeStep {
                events: vec![],
                terminal: LlmStepEnd::ToolCalls {
                    calls: vec![ToolCallEnd {
                        call_id: "c1".to_string(),
                        tool_name: "echo".to_string(),
                        input: json!({}),
                        name_resolution: aivyx_llm::NameResolution::Known,
                    }],
                    text_so_far: String::new(),
                    usage: zero_usage(),
                },
            },
            FakeStep {
                events: vec![],
                terminal: LlmStepEnd::FinalMessage {
                    text: "done".to_string(),
                    usage: zero_usage(),
                },
            },
        ];
        let provider = FakeLlmProvider::new(script);
        let registry = Arc::new(ToolRegistry::new(vec![tool]));
        // Tiny window ensures pruning fires on both calls.
        let config = LlmPlannerConfig::new("test").with_context_window(60);
        let mut planner = LlmPlanner::new(provider, registry, config);
        // Seed with enough bulk to trigger pruning.
        for i in 0..8 {
            planner.history.push(LlmMessage::user_text(format!(
                "bulk-{i}-{}",
                "y".repeat(80)
            )));
        }
        let ch = RecChannel::new();
        // First call — tool call.
        let step = planner.next_step(&[], &ch).await;
        assert!(matches!(step, NextStep::ToolCall { .. }));
        let first_pruned = planner.pruned_message_count();
        assert!(first_pruned > 0, "should have pruned on first step");
        // Observe tool result, adding more content.
        planner
            .observe_tool_outcome(
                tool_id,
                &ToolOutcome::Completed {
                    output: json!({"data": "x".repeat(100)}),
                    verified: Verification::NotApplicable,
                },
            )
            .await;
        // Second call — final message.
        let step = planner.next_step(&[], &ch).await;
        assert!(matches!(step, NextStep::FinalMessage(_)));
        assert!(
            planner.pruned_message_count() >= first_pruned,
            "counter should accumulate"
        );
    }

    // -----------------------------------------------------------------------
    // Phase 43 Task 5 — TokenUsage pruning fields
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn turn_usage_reports_pruning_tokens() {
        // Over-budget history triggers pruning and populates the
        // before/after token fields in TokenUsage.
        let msgs: Vec<LlmMessage> = (0..10)
            .map(|i| LlmMessage::user_text(format!("msg-{i}-{}", "x".repeat(100))))
            .collect();
        let (mut planner, ch) = make_pruning_planner(200, msgs, "ok");
        planner.next_step(&[], &ch).await;

        let usage = planner.turn_usage();
        assert!(
            usage.context_tokens_before_pruning > 0,
            "before should be populated when pruning fires"
        );
        assert!(
            usage.context_tokens_after_pruning > 0,
            "after should be populated when pruning fires"
        );
        assert!(
            usage.context_tokens_after_pruning < usage.context_tokens_before_pruning,
            "after < before when messages were pruned"
        );
    }

    #[tokio::test]
    async fn turn_usage_zeroes_when_no_pruning() {
        // Under-budget — pruning doesn't fire, fields stay zero.
        let msgs = vec![LlmMessage::user_text("short")];
        let (mut planner, ch) = make_pruning_planner(200_000, msgs, "ok");
        planner.next_step(&[], &ch).await;

        let usage = planner.turn_usage();
        assert_eq!(usage.context_tokens_before_pruning, 0);
        assert_eq!(usage.context_tokens_after_pruning, 0);
    }

    // -----------------------------------------------------------------------
    // Phase 45 Task 3 — begin_turn with multimodal MessageContent
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn begin_turn_image_to_content_block() {
        let script = vec![FakeStep {
            events: vec![],
            terminal: LlmStepEnd::FinalMessage {
                text: "I see an image".to_string(),
                usage: zero_usage(),
            },
        }];
        let provider = FakeLlmProvider::new(script);
        let registry = Arc::new(ToolRegistry::new(vec![]));
        let config = LlmPlannerConfig::new("test");
        let mut planner = LlmPlanner::new(provider, registry, config);

        let session = crate::SessionId::new();
        let msg = Message::image(session, "image/png", vec![0x89, 0x50]);
        planner.begin_turn(&msg, TurnId::new()).await;

        let hist = planner.history();
        assert_eq!(hist.len(), 1);
        match &hist[0] {
            LlmMessage::User { content } => {
                assert_eq!(content.len(), 1);
                assert!(content[0].is_image());
            }
            other => panic!("expected User, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn begin_turn_mixed_content() {
        let script = vec![FakeStep {
            events: vec![],
            terminal: LlmStepEnd::FinalMessage {
                text: "ok".to_string(),
                usage: zero_usage(),
            },
        }];
        let provider = FakeLlmProvider::new(script);
        let registry = Arc::new(ToolRegistry::new(vec![]));
        let config = LlmPlannerConfig::new("test");
        let mut planner = LlmPlanner::new(provider, registry, config);

        let session = crate::SessionId::new();
        let msg = Message::text_with_image(session, "describe this", "image/jpeg", vec![0xFF]);
        planner.begin_turn(&msg, TurnId::new()).await;

        let hist = planner.history();
        assert_eq!(hist.len(), 1);
        match &hist[0] {
            LlmMessage::User { content } => {
                assert_eq!(content.len(), 2);
                assert!(
                    matches!(&content[0], ContentBlock::Text { text } if text == "describe this")
                );
                assert!(content[1].is_image());
            }
            other => panic!("expected User, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Phase 101 — tool-call input validation & repair.
    // -----------------------------------------------------------------------

    fn req_path_schema() -> Value {
        json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"]
        })
    }

    #[test]
    fn validate_accepts_well_formed_input() {
        assert!(validate_tool_input(&req_path_schema(), &json!({"path": "notes.txt"}),).is_ok());
    }

    #[test]
    fn validate_rejects_missing_required_field() {
        let err = validate_tool_input(&req_path_schema(), &json!({}))
            .expect_err("missing required `path` must fail validation");
        assert!(
            err.contains("path"),
            "the summary should name the missing field: {err}"
        );
    }

    #[test]
    fn validate_rejects_wrong_typed_field() {
        let err = validate_tool_input(&req_path_schema(), &json!({"path": 123}))
            .expect_err("a non-string `path` must fail validation");
        assert!(!err.is_empty(), "the summary must not be empty");
    }

    #[test]
    fn validate_tolerates_extra_unschemad_field() {
        // Tool schemas do not set `additionalProperties: false`, so an
        // extra field the model invented is tolerated, not rejected.
        assert!(
            validate_tool_input(
                &req_path_schema(),
                &json!({"path": "x", "hallucinated": true}),
            )
            .is_ok()
        );
    }

    #[test]
    fn validate_fails_open_on_a_malformed_schema() {
        // A value that is not itself a valid JSON Schema must never
        // brick dispatch — `validate_tool_input` treats it as valid.
        assert!(validate_tool_input(&json!(42), &json!({"anything": true})).is_ok());
    }

    #[tokio::test]
    async fn well_formed_call_dispatches_without_a_repair_round() {
        let tool = Arc::new(FakeTool::with_schema("fs.read", req_path_schema()));
        let tool_id = tool.id();
        let script = vec![FakeStep {
            events: vec![],
            terminal: LlmStepEnd::ToolCalls {
                calls: vec![ToolCallEnd {
                    call_id: "c1".to_string(),
                    tool_name: "fs.read".to_string(),
                    input: json!({"path": "ok.txt"}),
                    name_resolution: aivyx_llm::NameResolution::Known,
                }],
                text_so_far: String::new(),
                usage: zero_usage(),
            },
        }];
        let provider = FakeLlmProvider::new(script);
        let registry = Arc::new(ToolRegistry::new(vec![tool]));
        let mut planner = LlmPlanner::new(provider, registry, LlmPlannerConfig::new("m"));
        let channel = RecChannel::new();
        planner
            .begin_turn(&Message::text(channel.session, "go"), TurnId::new())
            .await;
        match planner.next_step(&[], &channel).await {
            NextStep::ToolCall { tool_id: got, .. } => assert_eq!(got, tool_id),
            other => panic!("expected ToolCall, got {other:?}"),
        }
        assert!(
            !planner.history().iter().any(|m| matches!(
                m,
                LlmMessage::ToolResult { content, .. } if content.contains("invalid_input")
            )),
            "a well-formed call must not produce a repair result"
        );
    }

    #[tokio::test]
    async fn malformed_call_is_repaired_then_dispatched() {
        let tool = Arc::new(FakeTool::with_schema("fs.read", req_path_schema()));
        let tool_id = tool.id();
        let script = vec![
            // Round 1 — missing the required `path` field.
            FakeStep {
                events: vec![],
                terminal: LlmStepEnd::ToolCalls {
                    calls: vec![ToolCallEnd {
                        call_id: "c1".to_string(),
                        tool_name: "fs.read".to_string(),
                        input: json!({}),
                        name_resolution: aivyx_llm::NameResolution::Known,
                    }],
                    text_so_far: String::new(),
                    usage: zero_usage(),
                },
            },
            // Round 2 — the model repairs the call.
            FakeStep {
                events: vec![],
                terminal: LlmStepEnd::ToolCalls {
                    calls: vec![ToolCallEnd {
                        call_id: "c2".to_string(),
                        tool_name: "fs.read".to_string(),
                        input: json!({"path": "fixed.txt"}),
                        name_resolution: aivyx_llm::NameResolution::Known,
                    }],
                    text_so_far: String::new(),
                    usage: zero_usage(),
                },
            },
        ];
        let provider = FakeLlmProvider::new(script);
        let registry = Arc::new(ToolRegistry::new(vec![tool]));
        let mut planner = LlmPlanner::new(provider, registry, LlmPlannerConfig::new("m"));
        let channel = RecChannel::new();
        planner
            .begin_turn(&Message::text(channel.session, "go"), TurnId::new())
            .await;
        match planner.next_step(&[], &channel).await {
            NextStep::ToolCall {
                tool_id: got,
                input,
                ..
            } => {
                assert_eq!(got, tool_id);
                assert_eq!(input, json!({"path": "fixed.txt"}));
            }
            other => panic!("expected the repaired ToolCall, got {other:?}"),
        }
        let repairs = planner
            .history()
            .iter()
            .filter(|m| {
                matches!(
                    m,
                    LlmMessage::ToolResult { content, .. } if content.contains("invalid_input")
                )
            })
            .count();
        assert_eq!(repairs, 1, "exactly one repair round expected");
    }

    #[tokio::test]
    async fn two_repair_rounds_then_dispatch_as_is() {
        let tool = Arc::new(FakeTool::with_schema("fs.read", req_path_schema()));
        // Three rounds, all missing `path`. The third dispatches the
        // still-invalid call as-is — the two-repair cap disabled
        // validation (PHASE_101.md Q3).
        let bad = || LlmStepEnd::ToolCalls {
            calls: vec![ToolCallEnd {
                call_id: "c".to_string(),
                tool_name: "fs.read".to_string(),
                input: json!({}),
                name_resolution: aivyx_llm::NameResolution::Known,
            }],
            text_so_far: String::new(),
            usage: zero_usage(),
        };
        let script = vec![
            FakeStep {
                events: vec![],
                terminal: bad(),
            },
            FakeStep {
                events: vec![],
                terminal: bad(),
            },
            FakeStep {
                events: vec![],
                terminal: bad(),
            },
        ];
        let provider = FakeLlmProvider::new(script);
        let registry = Arc::new(ToolRegistry::new(vec![tool]));
        let mut planner = LlmPlanner::new(provider, registry, LlmPlannerConfig::new("m"));
        let channel = RecChannel::new();
        planner
            .begin_turn(&Message::text(channel.session, "go"), TurnId::new())
            .await;
        let step = planner.next_step(&[], &channel).await;
        assert!(
            matches!(step, NextStep::ToolCall { .. }),
            "after the two-repair cap the call dispatches as-is, got {step:?}"
        );
        let repairs = planner
            .history()
            .iter()
            .filter(|m| {
                matches!(
                    m,
                    LlmMessage::ToolResult { content, .. } if content.contains("invalid_input")
                )
            })
            .count();
        assert_eq!(repairs, 2, "repair attempts are capped at two");
    }

    #[tokio::test]
    async fn mixed_batch_dispatches_valid_call_and_errors_invalid_one() {
        let good = Arc::new(FakeTool::with_schema("fs.read", req_path_schema()));
        let bad_tool = Arc::new(FakeTool::with_schema("fs.write", req_path_schema()));
        let good_id = good.id();
        let script = vec![FakeStep {
            events: vec![],
            terminal: LlmStepEnd::ToolCalls {
                calls: vec![
                    ToolCallEnd {
                        call_id: "ok".to_string(),
                        tool_name: "fs.read".to_string(),
                        input: json!({"path": "ok.txt"}),
                        name_resolution: aivyx_llm::NameResolution::Known,
                    },
                    ToolCallEnd {
                        call_id: "bad".to_string(),
                        tool_name: "fs.write".to_string(),
                        input: json!({}),
                        name_resolution: aivyx_llm::NameResolution::Known,
                    },
                ],
                text_so_far: String::new(),
                usage: zero_usage(),
            },
        }];
        let provider = FakeLlmProvider::new(script);
        let registry = Arc::new(ToolRegistry::new(vec![good, bad_tool]));
        let mut planner = LlmPlanner::new(provider, registry, LlmPlannerConfig::new("m"));
        let channel = RecChannel::new();
        planner
            .begin_turn(&Message::text(channel.session, "go"), TurnId::new())
            .await;
        // Only the valid call dispatches — the invalid one is errored,
        // leaving a single-tool batch.
        match planner.next_step(&[], &channel).await {
            NextStep::ToolCall { tool_id: got, .. } => assert_eq!(got, good_id),
            other => panic!("expected the valid ToolCall, got {other:?}"),
        }
        let repairs = planner
            .history()
            .iter()
            .filter(|m| {
                matches!(
                    m,
                    LlmMessage::ToolResult { content, .. } if content.contains("invalid_input")
                )
            })
            .count();
        assert_eq!(
            repairs, 1,
            "the one invalid call produced one repair result"
        );
    }

    // -----------------------------------------------------------------------
    // Phase 126 — textual-tool-call extraction from FinalMessage text
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn phase_126_tool_code_extraction_dispatches_known_call() {
        // Model returns text with a `<tool_code>` block containing a
        // real registered tool. The planner extracts, synthesizes
        // a ToolCallEnd, dispatches, and the AuditTag::ToolCall
        // carries extracted_from_text: Some("tool_code").
        let fs_read = FakeTool::new("fs.read");
        let fs_read_id = fs_read.id;
        let script = vec![FakeStep {
            events: vec![LlmStreamEvent::TextChunk(
                "I'll read the file.\n\n\
                 <tool_code>\n  \
                 {\"name\": \"fs.read\", \"arguments\": {\"path\": \"x.txt\"}}\n\
                 </tool_code>"
                    .to_string(),
            )],
            terminal: LlmStepEnd::FinalMessage {
                text: "I'll read the file.\n\n\
                       <tool_code>\n  \
                       {\"name\": \"fs.read\", \"arguments\": {\"path\": \"x.txt\"}}\n\
                       </tool_code>"
                    .to_string(),
                usage: zero_usage(),
            },
        }];
        let provider = FakeLlmProvider::new(script);
        let registry = Arc::new(ToolRegistry::new(vec![Arc::new(fs_read)]));
        let mut planner = LlmPlanner::new(provider, registry, LlmPlannerConfig::new("test-model"));

        let channel = RecChannel::new();
        planner
            .begin_turn(&Message::text(channel.session, "read x.txt"), TurnId::new())
            .await;
        let step = planner.next_step(&[], &channel).await;
        match step {
            NextStep::ToolCall {
                tool_id,
                input,
                auto_corrected_from,
                extracted_from_text,
            } => {
                assert_eq!(tool_id, fs_read_id, "extraction resolved to fs.read");
                assert_eq!(input["path"], "x.txt");
                assert!(
                    auto_corrected_from.is_none(),
                    "tool name was exact; no fuzzy-recovery should fire"
                );
                assert_eq!(
                    extracted_from_text.as_deref(),
                    Some("tool_code"),
                    "wrapper-tag identifier threaded through to the audit field"
                );
            }
            other => panic!("expected NextStep::ToolCall from extraction; got {other:?}"),
        }
    }

    #[tokio::test]
    async fn phase_126_tool_call_extraction_with_tool_parameters_shape() {
        // gemma4's observed shape: `<tool_call>` wrapper with
        // `tool`/`parameters` JSON. Extraction handles both shapes
        // identically and routes through the same dispatcher.
        let memory_read = FakeTool::new("memory.read");
        let memory_read_id = memory_read.id;
        let script = vec![FakeStep {
            events: vec![],
            terminal: LlmStepEnd::FinalMessage {
                text: "<tool_call>\
                       {\"tool\": \"memory.read\", \"parameters\": {\"topic\": \"x\"}}\
                       </tool_call>"
                    .to_string(),
                usage: zero_usage(),
            },
        }];
        let provider = FakeLlmProvider::new(script);
        let registry = Arc::new(ToolRegistry::new(vec![Arc::new(memory_read)]));
        let mut planner = LlmPlanner::new(provider, registry, LlmPlannerConfig::new("test-model"));

        let channel = RecChannel::new();
        planner
            .begin_turn(
                &Message::text(channel.session, "read memory"),
                TurnId::new(),
            )
            .await;
        let step = planner.next_step(&[], &channel).await;
        match step {
            NextStep::ToolCall {
                tool_id,
                extracted_from_text,
                ..
            } => {
                assert_eq!(tool_id, memory_read_id);
                assert_eq!(
                    extracted_from_text.as_deref(),
                    Some("tool_call"),
                    "wrapper-tag for <tool_call> shape correctly identified"
                );
            }
            other => panic!("expected ToolCall; got {other:?}"),
        }
    }

    #[tokio::test]
    async fn phase_126_extraction_falls_through_to_final_message_when_no_blocks() {
        // Plain-text response (no `<tool_code>` blocks). Extraction
        // returns empty; existing FinalMessage path runs unchanged.
        let script = vec![FakeStep {
            events: vec![],
            terminal: LlmStepEnd::FinalMessage {
                text: "Just regular prose without any tool-call markers.".to_string(),
                usage: zero_usage(),
            },
        }];
        let provider = FakeLlmProvider::new(script);
        let registry = Arc::new(ToolRegistry::new(vec![]));
        let mut planner = LlmPlanner::new(provider, registry, LlmPlannerConfig::new("test-model"));

        let channel = RecChannel::new();
        planner
            .begin_turn(&Message::text(channel.session, "say hi"), TurnId::new())
            .await;
        let step = planner.next_step(&[], &channel).await;
        assert!(matches!(
            step,
            NextStep::FinalMessage(ref m) if m.starts_with("Just regular prose")
        ));
    }

    #[tokio::test]
    async fn phase_126_extraction_composes_with_phase_120_fuzzy_recovery() {
        // gemma4 observed emitting `fs.write_file` (a hallucinated
        // alternative). With the operator's tool_name_auto_correct_
        // threshold lowered to 0.5, Phase 120 fuzzy-recovers
        // `fs.write_file` → `fs.write`. The audit entry carries
        // BOTH extracted_from_text AND auto_corrected_from.
        let fs_write = FakeTool::new("fs.write");
        let fs_write_id = fs_write.id;
        let script = vec![FakeStep {
            events: vec![],
            terminal: LlmStepEnd::FinalMessage {
                text: "<tool_call>\
                       {\"tool\": \"fs.write_file\", \"parameters\": \
                        {\"path\": \"x\", \"content\": \"y\"}}\
                       </tool_call>"
                    .to_string(),
                usage: zero_usage(),
            },
        }];
        let provider = FakeLlmProvider::new(script);
        let registry = Arc::new(ToolRegistry::new(vec![Arc::new(fs_write)]));
        let mut planner = LlmPlanner::new(
            provider,
            registry,
            LlmPlannerConfig::new("test-model").with_tool_name_auto_correct_threshold(0.5),
        );

        let channel = RecChannel::new();
        planner
            .begin_turn(&Message::text(channel.session, "save it"), TurnId::new())
            .await;
        let step = planner.next_step(&[], &channel).await;
        match step {
            NextStep::ToolCall {
                tool_id,
                auto_corrected_from,
                extracted_from_text,
                ..
            } => {
                assert_eq!(tool_id, fs_write_id, "fuzzy-recovered to fs.write");
                assert_eq!(
                    auto_corrected_from.as_deref(),
                    Some("fs.write_file"),
                    "Phase 120 records the hallucinated original"
                );
                assert_eq!(
                    extracted_from_text.as_deref(),
                    Some("tool_call"),
                    "Phase 126 records the extraction wrapper-tag"
                );
            }
            other => panic!("expected ToolCall composed; got {other:?}"),
        }
    }

    #[tokio::test]
    async fn phase_126_unknown_tool_in_extracted_call_below_threshold_loops_with_error() {
        // Extracted tool name not registered AND no fuzzy match
        // (threshold too high). The planner records an unknown_tool
        // error in history and loops; the next-step result is
        // whatever the LLM produces on the second round. Mock
        // returns a clean final message on round 2 so the test
        // can assert the loop's outcome.
        let script = vec![
            FakeStep {
                events: vec![],
                terminal: LlmStepEnd::FinalMessage {
                    text: "<tool_code>\
                           {\"name\": \"nonexistent.tool\", \"arguments\": {}}\
                           </tool_code>"
                        .to_string(),
                    usage: zero_usage(),
                },
            },
            FakeStep {
                events: vec![],
                terminal: LlmStepEnd::FinalMessage {
                    text: "sorry, retrying without tool".to_string(),
                    usage: zero_usage(),
                },
            },
        ];
        let provider = FakeLlmProvider::new(script);
        let registry = Arc::new(ToolRegistry::new(vec![]));
        let mut planner = LlmPlanner::new(provider, registry, LlmPlannerConfig::new("test-model"));

        let channel = RecChannel::new();
        planner
            .begin_turn(&Message::text(channel.session, "do thing"), TurnId::new())
            .await;
        let step = planner.next_step(&[], &channel).await;
        // Round 1 produced an unknown_tool error in history; the
        // planner looped to round 2 which returned a clean
        // FinalMessage. The error result must be in history.
        assert!(matches!(step, NextStep::FinalMessage(ref m) if m.contains("sorry")));
        let hist = planner.history();
        let unknown_tool_errors = hist
            .iter()
            .filter(|m| {
                matches!(
                    m,
                    LlmMessage::ToolResult { content, .. } if content.contains("unknown_tool")
                )
            })
            .count();
        assert_eq!(
            unknown_tool_errors, 1,
            "extracted call with unknown tool surfaces an unknown_tool error in history"
        );
    }

    #[tokio::test]
    async fn phase_126_multiple_extracted_calls_dispatch_as_batch() {
        // Text contains multiple `<tool_code>` blocks. Planner
        // extracts all of them and returns ToolCalls(batch) when
        // more than one is dispatchable.
        let fs_read = FakeTool::new("fs.read");
        let memory_read = FakeTool::new("memory.read");
        let fs_read_id = fs_read.id;
        let memory_read_id = memory_read.id;
        let script = vec![FakeStep {
            events: vec![],
            terminal: LlmStepEnd::FinalMessage {
                text: "Doing two things:\n\
                       <tool_code>{\"name\": \"fs.read\", \"arguments\": {\"path\": \"x\"}}</tool_code>\n\
                       <tool_code>{\"name\": \"memory.read\", \"arguments\": {\"topic\": \"y\"}}</tool_code>"
                    .to_string(),
                usage: zero_usage(),
            },
        }];
        let provider = FakeLlmProvider::new(script);
        let registry = Arc::new(ToolRegistry::new(vec![
            Arc::new(fs_read),
            Arc::new(memory_read),
        ]));
        let mut planner = LlmPlanner::new(provider, registry, LlmPlannerConfig::new("test-model"));

        let channel = RecChannel::new();
        planner
            .begin_turn(&Message::text(channel.session, "two tasks"), TurnId::new())
            .await;
        let step = planner.next_step(&[], &channel).await;
        match step {
            NextStep::ToolCalls(batch) => {
                assert_eq!(batch.len(), 2);
                // Source-order preserved.
                assert_eq!(batch[0].tool_id, fs_read_id);
                assert_eq!(batch[1].tool_id, memory_read_id);
                // Both carry the extraction marker.
                assert_eq!(batch[0].extracted_from_text.as_deref(), Some("tool_code"));
                assert_eq!(batch[1].extracted_from_text.as_deref(), Some("tool_code"));
            }
            other => panic!("expected ToolCalls(batch); got {other:?}"),
        }
    }

    #[tokio::test]
    async fn phase_126_malformed_extracted_block_drops_silently() {
        // `<tool_code>` block with malformed JSON. Extractor drops
        // it silently; planner sees zero extracted calls; falls
        // through to FinalMessage.
        let script = vec![FakeStep {
            events: vec![],
            terminal: LlmStepEnd::FinalMessage {
                text: "<tool_code>not valid json</tool_code>".to_string(),
                usage: zero_usage(),
            },
        }];
        let provider = FakeLlmProvider::new(script);
        let registry = Arc::new(ToolRegistry::new(vec![]));
        let mut planner = LlmPlanner::new(provider, registry, LlmPlannerConfig::new("test-model"));

        let channel = RecChannel::new();
        planner
            .begin_turn(&Message::text(channel.session, "x"), TurnId::new())
            .await;
        let step = planner.next_step(&[], &channel).await;
        // Malformed → no extraction → falls through to FinalMessage
        // with the original raw text.
        assert!(matches!(
            step,
            NextStep::FinalMessage(ref m) if m.contains("not valid json")
        ));
    }

    #[tokio::test]
    async fn phase_126_history_preserves_raw_text_with_tool_code_block() {
        // After extraction, the assistant message pushed to history
        // contains the raw text (including the `<tool_code>` block)
        // alongside the synthesized records. This preserves
        // context for re-feeding the model on the next round.
        let fs_read = FakeTool::new("fs.read");
        let raw_text = "<tool_code>\
                        {\"name\": \"fs.read\", \"arguments\": {\"path\": \"x\"}}\
                        </tool_code>";
        let script = vec![FakeStep {
            events: vec![],
            terminal: LlmStepEnd::FinalMessage {
                text: raw_text.to_string(),
                usage: zero_usage(),
            },
        }];
        let provider = FakeLlmProvider::new(script);
        let registry = Arc::new(ToolRegistry::new(vec![Arc::new(fs_read)]));
        let mut planner = LlmPlanner::new(provider, registry, LlmPlannerConfig::new("test-model"));

        let channel = RecChannel::new();
        planner
            .begin_turn(&Message::text(channel.session, "x"), TurnId::new())
            .await;
        let _ = planner.next_step(&[], &channel).await;

        let hist = planner.history();
        // Expect: User → Assistant { text: raw_text, tool_calls: 1 record }
        let assistant_msg = hist
            .iter()
            .find_map(|m| match m {
                LlmMessage::Assistant { text, tool_calls } => Some((text, tool_calls)),
                _ => None,
            })
            .expect("history must include synthesized Assistant message");
        assert_eq!(
            assistant_msg.0, raw_text,
            "raw text including <tool_code> block preserved for context"
        );
        assert_eq!(
            assistant_msg.1.len(),
            1,
            "one synthesized tool_call record matching the extracted block"
        );
        assert_eq!(assistant_msg.1[0].tool_name, "fs.read");
        // Synthesized call_id prefix.
        assert!(
            assistant_msg.1[0].call_id.starts_with("extracted-"),
            "synthesized call_id carries the extraction prefix; got {:?}",
            assistant_msg.1[0].call_id
        );
    }

    // ====================================================
    // Phase 127 Task 7 — planner composition with the new
    // parser families. Each test confirms the end-to-end
    // path works: model emits text in format X, planner
    // extracts via the substrate, synthesizes a ToolCallEnd,
    // dispatches to the registered tool. The `extracted_
    // from_text` audit field carries the wrapper-tag through
    // to the audit chain.
    //
    // The FakeLlmProvider used here does NOT override
    // `tool_call_family_hint`, so the family hint is None
    // and the substrate uses default priority order. The
    // parsers don't ambiguously match on these inputs, so
    // hint-less extraction produces correct results.
    // ====================================================

    #[tokio::test]
    async fn phase_127_planner_extracts_qwen3_coder_xml() {
        // Qwen3.5/3.6 emits XML inside `<tool_call>` per
        // Ollama issue #14745. Phase 127 Task 2's parser
        // catches it; the planner dispatches via the same
        // Phase 120/101 helper as protocol tool calls.
        let fs_write = FakeTool::new("fs.write");
        let fs_write_id = fs_write.id;
        let script = vec![FakeStep {
            events: vec![],
            terminal: LlmStepEnd::FinalMessage {
                text: "<tool_call>\
                       <function=fs.write>\
                       <parameter=path>test.txt</parameter>\
                       <parameter=content>phase 127</parameter>\
                       </function>\
                       </tool_call>"
                    .to_string(),
                usage: zero_usage(),
            },
        }];
        let provider = FakeLlmProvider::new(script);
        let registry = Arc::new(ToolRegistry::new(vec![Arc::new(fs_write)]));
        let mut planner = LlmPlanner::new(provider, registry, LlmPlannerConfig::new("test-model"));

        let channel = RecChannel::new();
        planner
            .begin_turn(
                &Message::text(channel.session, "write a file"),
                TurnId::new(),
            )
            .await;
        let step = planner.next_step(&[], &channel).await;
        match step {
            NextStep::ToolCall {
                tool_id,
                extracted_from_text,
                input,
                ..
            } => {
                assert_eq!(tool_id, fs_write_id);
                assert_eq!(
                    extracted_from_text.as_deref(),
                    Some("tool_call"),
                    "wrapper-tag for Qwen3-Coder XML extracted call"
                );
                assert_eq!(input["path"], "test.txt");
                assert_eq!(input["content"], "phase 127");
            }
            other => panic!("expected ToolCall; got {other:?}"),
        }
    }

    #[tokio::test]
    async fn phase_127_planner_extracts_phi4_mini_list() {
        // Phi-4-mini emits a JSON array inside the
        // `<|tool_call|>` special-token wrapper. Even a
        // single-element list goes through the JSON-list
        // path. wrapper_tag is the literal `"|tool_call|"`
        // (with bars).
        let fs_write = FakeTool::new("fs.write");
        let fs_write_id = fs_write.id;
        let script = vec![FakeStep {
            events: vec![],
            terminal: LlmStepEnd::FinalMessage {
                text: r#"<|tool_call|>[{"name": "fs.write", "arguments": {"path": "phi.txt"}}]<|/tool_call|>"#
                    .to_string(),
                usage: zero_usage(),
            },
        }];
        let provider = FakeLlmProvider::new(script);
        let registry = Arc::new(ToolRegistry::new(vec![Arc::new(fs_write)]));
        let mut planner = LlmPlanner::new(provider, registry, LlmPlannerConfig::new("test-model"));

        let channel = RecChannel::new();
        planner
            .begin_turn(&Message::text(channel.session, "x"), TurnId::new())
            .await;
        let step = planner.next_step(&[], &channel).await;
        match step {
            NextStep::ToolCall {
                tool_id,
                extracted_from_text,
                input,
                ..
            } => {
                assert_eq!(tool_id, fs_write_id);
                assert_eq!(
                    extracted_from_text.as_deref(),
                    Some("|tool_call|"),
                    "wrapper-tag carries the bars verbatim for grep-distinctness"
                );
                assert_eq!(input["path"], "phi.txt");
            }
            other => panic!("expected ToolCall; got {other:?}"),
        }
    }

    #[tokio::test]
    async fn phase_127_planner_extracts_gemma3_python_fence() {
        // Gemma 3 emits Python-call syntax inside a
        // ```tool_code` markdown fence. Phase 127 Task 4's
        // hand-written recursive-descent parser translates
        // Python kwargs into JSON arguments.
        let fs_write = FakeTool::new("fs.write");
        let fs_write_id = fs_write.id;
        let script = vec![FakeStep {
            events: vec![],
            terminal: LlmStepEnd::FinalMessage {
                text: "```tool_code\nfs.write(path='gemma.txt', content='hi')\n```".to_string(),
                usage: zero_usage(),
            },
        }];
        let provider = FakeLlmProvider::new(script);
        let registry = Arc::new(ToolRegistry::new(vec![Arc::new(fs_write)]));
        let mut planner = LlmPlanner::new(provider, registry, LlmPlannerConfig::new("test-model"));

        let channel = RecChannel::new();
        planner
            .begin_turn(&Message::text(channel.session, "x"), TurnId::new())
            .await;
        let step = planner.next_step(&[], &channel).await;
        match step {
            NextStep::ToolCall {
                tool_id,
                extracted_from_text,
                input,
                ..
            } => {
                assert_eq!(tool_id, fs_write_id);
                assert_eq!(
                    extracted_from_text.as_deref(),
                    Some("tool_code_fence"),
                    "wrapper-tag distinguishes the markdown fence from bare <tool_code>"
                );
                assert_eq!(input["path"], "gemma.txt");
                assert_eq!(input["content"], "hi");
            }
            other => panic!("expected ToolCall; got {other:?}"),
        }
    }

    #[tokio::test]
    async fn phase_127_planner_extracts_bare_json() {
        // Some Ollama models (qwen3:32b per issue #11662)
        // emit raw JSON with no wrapper at all. Phase 127
        // Task 5's bare-JSON fallback catches it when the
        // entire response is exactly one tool-call-shaped
        // JSON object. wrapper_tag is `"(bare)"` to
        // communicate "no wrapper detected".
        let fs_read = FakeTool::new("fs.read");
        let fs_read_id = fs_read.id;
        let script = vec![FakeStep {
            events: vec![],
            terminal: LlmStepEnd::FinalMessage {
                text: r#"{"name": "fs.read", "arguments": {"path": "x"}}"#.to_string(),
                usage: zero_usage(),
            },
        }];
        let provider = FakeLlmProvider::new(script);
        let registry = Arc::new(ToolRegistry::new(vec![Arc::new(fs_read)]));
        let mut planner = LlmPlanner::new(provider, registry, LlmPlannerConfig::new("test-model"));

        let channel = RecChannel::new();
        planner
            .begin_turn(&Message::text(channel.session, "x"), TurnId::new())
            .await;
        let step = planner.next_step(&[], &channel).await;
        match step {
            NextStep::ToolCall {
                tool_id,
                extracted_from_text,
                input,
                ..
            } => {
                assert_eq!(tool_id, fs_read_id);
                assert_eq!(
                    extracted_from_text.as_deref(),
                    Some("(bare)"),
                    "wrapper-tag for bare-JSON is `(bare)`"
                );
                assert_eq!(input["path"], "x");
            }
            other => panic!("expected ToolCall; got {other:?}"),
        }
    }

    #[test]
    fn compute_prefix_hash_is_stable_for_identical_inputs() {
        let tools = vec![LlmToolDescriptor {
            name: "read_file".to_string(),
            description: "reads a file".to_string(),
            input_schema: serde_json::json!({"type": "object"}),
        }];
        let h1 = compute_prefix_hash(Some("system prompt text"), &tools);
        let h2 = compute_prefix_hash(Some("system prompt text"), &tools);
        assert_eq!(h1, h2);
    }

    #[test]
    fn compute_prefix_hash_differs_when_system_text_differs() {
        let tools: Vec<LlmToolDescriptor> = vec![];
        let h1 = compute_prefix_hash(Some("prompt A"), &tools);
        let h2 = compute_prefix_hash(Some("prompt B"), &tools);
        assert_ne!(h1, h2);
    }

    #[test]
    fn compute_prefix_hash_differs_when_tools_differ() {
        let system = Some("same system text");
        let tools_a = vec![LlmToolDescriptor {
            name: "read_file".to_string(),
            description: "reads".to_string(),
            input_schema: serde_json::json!({}),
        }];
        let tools_b = vec![LlmToolDescriptor {
            name: "write_file".to_string(),
            description: "writes".to_string(),
            input_schema: serde_json::json!({}),
        }];
        assert_ne!(
            compute_prefix_hash(system, &tools_a),
            compute_prefix_hash(system, &tools_b)
        );
    }

    #[test]
    fn compute_prefix_hash_treats_none_system_distinctly_from_empty_string() {
        let tools: Vec<LlmToolDescriptor> = vec![];
        assert_ne!(
            compute_prefix_hash(None, &tools),
            compute_prefix_hash(Some(""), &tools)
        );
    }

    // ---- kvcache Fix 1 / Fix 2 regression tests -----------------
    //
    // Both tests below return from `ensure_kv_slot_checked_out` before
    // it ever performs I/O (the refiner-skip and idempotency guards are
    // both checked before `pool.checkout()`), so a `LlamaServerSlotStore`
    // opened against a local tempdir with a never-dialed `base_url` is
    // sufficient -- `LlamaServerSlotStore::open` itself does no network
    // I/O (only opens a local sqlite manifest + creates a local slots
    // dir), and neither test exercises a code path that would ever
    // reach the store's own HTTP calls. No mock server needed.

    fn kv_cache_config_for_test() -> (KvCacheConfig, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = LlamaServerSlotStore::open(dir.path(), "http://127.0.0.1:1", 1_000_000)
            .expect("open a local-only slot store");
        let config = KvCacheConfig {
            pool: Arc::new(KvSlotPool::new(2)),
            store: Arc::new(store),
            backend_id: "test-backend".to_string(),
            model_id: "test-model".to_string(),
            build_hash: "test-build".to_string(),
        };
        // Return the TempDir guard alongside -- the caller holds it for
        // the test's duration so the directory isn't removed out from
        // under `store` before the test finishes.
        (config, dir)
    }

    #[tokio::test]
    async fn ensure_kv_slot_checked_out_skips_entirely_when_a_refiner_is_configured() {
        // Fix 1 regression test: a refiner-enabled planner must never
        // check out a slot at all (see `ensure_kv_slot_checked_out`'s
        // doc comment for why warming pre-refined content would
        // silently defeat the cache forever).
        let refiner = FakeRefiner::new(Some("REFINED"));
        let mut planner =
            bare_planner(LlmPlannerConfig::new("m").with_system_prompt_refiner(refiner));
        let (kv, _dir) = kv_cache_config_for_test();
        let pool = kv.pool.clone();
        planner.kv_cache = Some(kv);

        planner.ensure_kv_slot_checked_out().await;

        assert_eq!(
            planner.kv_slot_id, None,
            "a refiner-enabled planner must never check out a kvcache slot"
        );
        // The pool itself must be untouched -- both slots still free.
        assert_eq!(pool.checkout(), Some(0));
        assert_eq!(pool.checkout(), Some(1));
    }

    #[tokio::test]
    async fn ensure_kv_slot_checked_out_is_idempotent_within_a_turn() {
        // Fix 2 regression test: a slot already recorded this turn must
        // never be replaced by a second checkout (which would leak the
        // first slot forever -- `Drop` only ever releases the last one
        // recorded).
        let mut planner = bare_planner(LlmPlannerConfig::new("m"));
        let (kv, _dir) = kv_cache_config_for_test();
        let pool = kv.pool.clone();
        planner.kv_cache = Some(kv);
        // Simulate "already checked out earlier this turn" directly,
        // rather than driving a full successful warm-up round trip
        // through a fake provider -- the guard fires purely off
        // `kv_slot_id.is_some()`, before any I/O, so this is a faithful
        // (and hermetic) way to exercise it.
        planner.kv_slot_id = Some(0);
        pool.checkout(); // matches: slot 0 is "already checked out"

        planner.ensure_kv_slot_checked_out().await;

        assert_eq!(
            planner.kv_slot_id,
            Some(0),
            "a slot already recorded this turn must not be replaced"
        );
        // The pool must be untouched by this call: slot 1 is still the
        // next free one, not slot 0 re-taken or slot 1 already consumed.
        assert_eq!(pool.checkout(), Some(1));
    }

    #[tokio::test]
    async fn ensure_kv_slot_checked_out_skips_restore_when_the_pool_already_has_this_prefix() {
        // Final-review Fix 1 regression test: a second `begin_turn` (a
        // fresh `LlmPlanner`, matching how `aivyx-pa` builds one per turn)
        // that checks out a slot the pool already recorded as holding
        // this exact prefix must NOT re-run `restore_into_slot` -- doing
        // so would overwrite this same process's own live conversation
        // KV state with the frozen prefix-only snapshot. `restore_into_slot`
        // itself isn't directly instrumentable (`LlamaServerSlotStore` is
        // a concrete type from `aivyx-kvcache`, not a trait), so this
        // discriminates via the *next* step instead: the code under test
        // only ever reaches `provider.chat_stream` (the warm-up call) if
        // `restore_into_slot` was attempted and returned a miss/failure.
        // A `FakeLlmProvider` seeded with exactly one scripted step whose
        // script remains unconsumed after the call is therefore proof
        // the whole restore-or-warm-up block -- restore included -- was
        // skipped, not just that warm-up specifically didn't run.
        let script = vec![FakeStep {
            events: vec![],
            terminal: LlmStepEnd::FinalMessage {
                text: String::new(),
                usage: zero_usage(),
            },
        }];
        let provider = FakeLlmProvider::new(script);
        let mut planner = LlmPlanner::new(
            provider.clone(),
            Arc::new(ToolRegistry::new(vec![])),
            LlmPlannerConfig::new("m"),
        );
        let (kv, _dir) = kv_cache_config_for_test();
        let pool = kv.pool.clone();
        // Same prefix `ensure_kv_slot_checked_out` will compute for this
        // planner: no system prompt, no tools (matches `bare_planner`'s
        // shape, reproduced by hand here since we need the concrete
        // `FakeLlmProvider` handle `bare_planner` doesn't expose).
        let prefix_hash = compute_prefix_hash(None, &[]);
        pool.record_loaded_prefix(0, prefix_hash);
        planner.kv_cache = Some(kv);

        planner.ensure_kv_slot_checked_out().await;

        assert_eq!(
            planner.kv_slot_id,
            Some(0),
            "the slot must still be checked out and pinned for the real turn"
        );
        assert_eq!(
            provider.script.lock().unwrap().len(),
            1,
            "chat_stream (the warm-up call, only reachable after a restore miss/failure) \
             must never be invoked when the pool already holds this exact prefix for this \
             slot -- a consumed script would mean restore_into_slot was redundantly \
             (and destructively) attempted"
        );
    }

    #[tokio::test]
    async fn ensure_kv_slot_checked_out_does_not_skip_when_the_pool_has_a_different_prefix() {
        // Mirror of the test above, inverted: proves the skip check is
        // genuinely conditioned on the prefix comparison
        // (`last_loaded_prefix(slot_id) == Some(key.prefix_hash)`), not
        // just "always skip once anything is recorded for this slot" --
        // a real coverage gap, since nothing else in the suite would
        // fail if that comparison were made unconditional and the whole
        // restore/warm-up feature silently disabled. Seed the pool with
        // a prefix hash that is NOT what this planner's real `CacheKey`
        // computes, then assert the restore-or-warm-up path DOES run:
        // the scripted `chat_stream` step (the warm-up call) is consumed
        // (script length ends at 0), same discriminator as above but
        // inverted.
        let script = vec![FakeStep {
            events: vec![],
            terminal: LlmStepEnd::FinalMessage {
                text: String::new(),
                usage: zero_usage(),
            },
        }];
        let provider = FakeLlmProvider::new(script);
        let mut planner = LlmPlanner::new(
            provider.clone(),
            Arc::new(ToolRegistry::new(vec![])),
            LlmPlannerConfig::new("m"),
        );
        let (kv, _dir) = kv_cache_config_for_test();
        let pool = kv.pool.clone();
        // Deliberately NOT the prefix hash this planner's config will
        // compute (no system prompt, no tools) -- a different, wrong
        // prefix recorded for this same slot.
        pool.record_loaded_prefix(0, "some-other-prefix-entirely".to_string());
        planner.kv_cache = Some(kv);

        planner.ensure_kv_slot_checked_out().await;

        assert_eq!(
            planner.kv_slot_id,
            Some(0),
            "the slot must still be checked out and pinned for the real turn"
        );
        assert_eq!(
            provider.script.lock().unwrap().len(),
            0,
            "chat_stream (the warm-up call) must be invoked when the pool's recorded prefix \
             for this slot does not match this planner's own prefix -- if this stayed at 1, \
             the skip check would be firing unconditionally instead of comparing prefixes"
        );
    }

    // ---- GPU-slot broker coordination — ProviderKind::Broker slot-hint mode ----

    #[tokio::test]
    async fn broker_slot_hint_mode_skips_local_checkout_and_attaches_slot_hint() {
        // Regression test: a broker-mode planner
        // (`with_broker_slot_hint`, no `with_kv_cache`) must never touch a
        // local `KvSlotPool` -- `aivyx-broker` owns admission and the
        // restore/warm/save lifecycle itself -- and every outgoing
        // request must carry `slot_hint` instead of a raw `id_slot` pin.
        let script = vec![FakeStep {
            events: vec![],
            terminal: LlmStepEnd::FinalMessage {
                text: "ok".to_string(),
                usage: zero_usage(),
            },
        }];
        let provider = FakeLlmProvider::new(script);
        let mut planner = LlmPlanner::new(
            provider.clone(),
            Arc::new(ToolRegistry::new(vec![])),
            LlmPlannerConfig::new("m"),
        )
        .with_broker_slot_hint();

        // A pool that exists but is deliberately never wired into this
        // planner (no `with_kv_cache` call) -- proof the broker-mode
        // planner has no way to touch it at all, not merely that it
        // chooses not to.
        let unused_pool = Arc::new(KvSlotPool::new(1));

        let channel = RecChannel::new();
        planner
            .begin_turn(&Message::text(channel.session, "hi"), TurnId::new())
            .await;
        let _ = planner.next_step(&[], &channel).await;

        assert!(
            planner.kv_cache.is_none(),
            "broker mode must never configure a local kvcache pool"
        );
        assert_eq!(
            planner.kv_slot_id, None,
            "broker mode must never check out a local slot id"
        );
        assert_eq!(
            unused_pool.checkout(),
            Some(0),
            "the unrelated pool must be completely untouched by a broker-mode turn"
        );

        let (id_slot, slot_hint) = provider
            .last_request
            .lock()
            .unwrap()
            .clone()
            .expect("chat_stream must have been called");
        assert_eq!(
            id_slot, None,
            "broker mode must send id_slot: None, not a raw local pin"
        );
        let hint = slot_hint.expect("broker mode must attach a slot_hint");
        assert_eq!(
            hint.prefix_hash,
            compute_prefix_hash(None, &[]),
            "prefix_hash must match this planner's own config (no system prompt, no tools)"
        );
        assert_eq!(
            hint.preferred_slot, None,
            "no local slot was ever checked out, so preferred_slot must be None"
        );
    }

    #[tokio::test]
    async fn non_broker_planner_never_attaches_a_slot_hint() {
        // Inverse of the test above: an ordinary planner (no
        // `with_broker_slot_hint` call -- every existing provider's
        // shape today) must never send `slot_hint`, byte-identical to
        // behavior before broker-mode slot hinting existed.
        let script = vec![FakeStep {
            events: vec![],
            terminal: LlmStepEnd::FinalMessage {
                text: "ok".to_string(),
                usage: zero_usage(),
            },
        }];
        let provider = FakeLlmProvider::new(script);
        let mut planner = LlmPlanner::new(
            provider.clone(),
            Arc::new(ToolRegistry::new(vec![])),
            LlmPlannerConfig::new("m"),
        );

        let channel = RecChannel::new();
        planner
            .begin_turn(&Message::text(channel.session, "hi"), TurnId::new())
            .await;
        let _ = planner.next_step(&[], &channel).await;

        let (id_slot, slot_hint) = provider
            .last_request
            .lock()
            .unwrap()
            .clone()
            .expect("chat_stream must have been called");
        assert_eq!(id_slot, None);
        assert_eq!(
            slot_hint, None,
            "a planner that never opted into broker mode must never attach a slot_hint"
        );
    }
    // ---- Model routing — turn tagging and per-model cost ----

    fn routing_profile(
        endpoint: &str,
        id: &str,
        caps: &[aivyx_route::Capability],
    ) -> aivyx_route::ModelProfile {
        let mut p = aivyx_route::ModelProfile::new(id, aivyx_route::EndpointRef::new(endpoint));
        p.tier = aivyx_route::Tier::Medium;
        p.capabilities.insert(aivyx_route::Capability::Completion);
        p.capabilities.extend(caps.iter().copied());
        p
    }

    /// A real `RoutedProvider` over `default@default` (no tool calling)
    /// and `big@gpu` (tool calling), so any request advertising a tool
    /// routes to `big`.
    fn routed_over(
        default: Arc<FakeLlmProvider>,
        gpu: Arc<FakeLlmProvider>,
    ) -> Arc<aivyx_llm::RoutedProvider> {
        let router = aivyx_route::Router::new(
            vec![
                routing_profile("default", "default", &[]),
                routing_profile("gpu", "big", &[aivyx_route::Capability::Tools]),
            ],
            aivyx_route::TaskOverrides::default(),
        );
        let factory: aivyx_llm::ProviderFactory =
            Box::new(move |_endpoint: &aivyx_route::EndpointRef| {
                Ok(Arc::clone(&gpu) as Arc<dyn LlmProvider>)
            });
        Arc::new(aivyx_llm::RoutedProvider::new(
            aivyx_route::ModelKey {
                endpoint: aivyx_route::EndpointRef::new("default"),
                id: "default".into(),
            },
            default as Arc<dyn LlmProvider>,
            router,
            factory,
        ))
    }

    fn final_step(input_tokens: u32, output_tokens: u32) -> FakeStep {
        FakeStep {
            events: vec![],
            terminal: LlmStepEnd::FinalMessage {
                text: "ok".to_string(),
                usage: LlmUsage {
                    input_tokens,
                    output_tokens,
                    ..LlmUsage::default()
                },
            },
        }
    }

    #[tokio::test]
    async fn routed_planner_tags_its_request_with_the_turn_session() {
        let provider = FakeLlmProvider::new(vec![final_step(1, 1)]);
        let routed = routed_over(FakeLlmProvider::new(vec![]), FakeLlmProvider::new(vec![]));
        let mut planner = LlmPlanner::new(
            provider.clone(),
            Arc::new(ToolRegistry::new(vec![])),
            LlmPlannerConfig::new("m"),
        )
        .with_routing(routed, aivyx_route::TaskKind::Chat);

        let channel = RecChannel::new();
        let message = Message::text(channel.session, "hello there");
        planner.begin_turn(&message, TurnId::new()).await;
        let _ = planner.next_step(&[], &channel).await;

        let routes = provider.routes.lock().unwrap().clone();
        assert_eq!(routes.len(), 1, "one main request");
        let hint = routes[0]
            .1
            .clone()
            .expect("a routed planner must tag its request");
        assert_eq!(hint.task, aivyx_route::TaskKind::Chat);
        assert_eq!(hint.session, Some(message.session_id.to_string()));
        assert!(hint.estimated_prompt_tokens > 0, "got {hint:?}");
    }

    #[tokio::test]
    async fn unrouted_planner_sends_no_route_hint() {
        let provider = FakeLlmProvider::new(vec![final_step(1, 1)]);
        let mut planner = LlmPlanner::new(
            provider.clone(),
            Arc::new(ToolRegistry::new(vec![])),
            LlmPlannerConfig::new("m"),
        );

        let channel = RecChannel::new();
        planner
            .begin_turn(&Message::text(channel.session, "hi"), TurnId::new())
            .await;
        let _ = planner.next_step(&[], &channel).await;

        let routes = provider.routes.lock().unwrap().clone();
        assert_eq!(routes, vec![("m".to_string(), None)]);
    }

    #[tokio::test]
    async fn turn_costs_price_the_model_the_router_used() {
        let default = FakeLlmProvider::new(vec![]);
        let gpu = FakeLlmProvider::new(vec![final_step(10, 5)]);
        let routed = routed_over(default.clone(), gpu.clone());
        let registry = Arc::new(ToolRegistry::new(vec![
            Arc::new(FakeTool::new("echo")) as Arc<dyn Tool>
        ]));
        let mut planner = LlmPlanner::new(
            Arc::clone(&routed) as Arc<dyn LlmProvider>,
            registry,
            LlmPlannerConfig::new("default"),
        )
        .with_routing(routed, aivyx_route::TaskKind::Chat);

        let channel = RecChannel::new();
        planner
            .begin_turn(&Message::text(channel.session, "hi"), TurnId::new())
            .await;
        let _ = planner.next_step(&[], &channel).await;

        assert!(default.routes.lock().unwrap().is_empty());
        assert_eq!(gpu.routes.lock().unwrap()[0].0, "big");
        let usage = planner.turn_usage();
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(planner.turn_costs(), vec![("big".to_string(), usage)]);
    }

    /// The `estimated_prompt_tokens` a routed planner sends for one
    /// "hello there" turn under `config`, with `tools` registered.
    async fn routed_estimate(config: LlmPlannerConfig, tools: Vec<Arc<dyn Tool>>) -> u32 {
        let provider = FakeLlmProvider::new(vec![final_step(1, 1)]);
        let routed = routed_over(FakeLlmProvider::new(vec![]), FakeLlmProvider::new(vec![]));
        let mut planner =
            LlmPlanner::new(provider.clone(), Arc::new(ToolRegistry::new(tools)), config)
                .with_routing(routed, aivyx_route::TaskKind::Chat);

        let channel = RecChannel::new();
        planner
            .begin_turn(
                &Message::text(channel.session, "hello there"),
                TurnId::new(),
            )
            .await;
        let _ = planner.next_step(&[], &channel).await;

        let routes = provider.routes.lock().unwrap().clone();
        routes[0].1.clone().expect("tagged").estimated_prompt_tokens
    }

    fn estimate_config(system_prompt: Option<&str>, max_tokens: u32) -> LlmPlannerConfig {
        let mut config = LlmPlannerConfig::new("m");
        config.system_prompt = system_prompt.map(str::to_string);
        config.max_tokens = max_tokens;
        config
    }

    #[tokio::test]
    async fn the_route_estimate_counts_system_prompt_tools_and_max_tokens() {
        let base = routed_estimate(estimate_config(None, 0), vec![]).await;

        let system = "x".repeat(4_000);
        let with_system = routed_estimate(estimate_config(Some(&system), 0), vec![]).await;
        assert!(
            with_system >= base + 1_000,
            "system prompt: {base} -> {with_system}"
        );

        let with_tools = routed_estimate(
            estimate_config(None, 0),
            vec![Arc::new(FakeTool::new("echo")) as Arc<dyn Tool>],
        )
        .await;
        assert!(with_tools > base, "tools: {base} -> {with_tools}");

        let with_output = routed_estimate(estimate_config(None, 2_000), vec![]).await;
        assert_eq!(with_output, base + 2_000, "max_tokens");
    }

    #[tokio::test]
    async fn the_family_hint_asks_about_the_model_the_router_used() {
        let default = FakeLlmProvider::new(vec![]);
        let gpu = FakeLlmProvider::new(vec![final_step(1, 1)]);
        let routed = routed_over(default.clone(), gpu.clone());
        let registry = Arc::new(ToolRegistry::new(vec![
            Arc::new(FakeTool::new("echo")) as Arc<dyn Tool>
        ]));
        let mut planner = LlmPlanner::new(
            Arc::clone(&routed) as Arc<dyn LlmProvider>,
            registry,
            LlmPlannerConfig::new("default"),
        )
        .with_routing(routed, aivyx_route::TaskKind::Chat);

        let channel = RecChannel::new();
        planner
            .begin_turn(&Message::text(channel.session, "hi"), TurnId::new())
            .await;
        let _ = planner.next_step(&[], &channel).await;

        assert_eq!(gpu.hints.lock().unwrap().clone(), vec!["big".to_string()]);
        assert!(default.hints.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_pdf_turn_after_a_routed_one_is_billed_and_hinted_as_the_configured_model() {
        let default = FakeLlmProvider::new(vec![final_step(20, 2)]);
        let gpu = FakeLlmProvider::new(vec![final_step(10, 5)]);
        let routed = routed_over(default.clone(), gpu.clone());
        let session = SessionId::new();
        let planner_for = || {
            let registry = Arc::new(ToolRegistry::new(vec![
                Arc::new(FakeTool::new("echo")) as Arc<dyn Tool>
            ]));
            LlmPlanner::new(
                Arc::clone(&routed) as Arc<dyn LlmProvider>,
                registry,
                LlmPlannerConfig::new("default"),
            )
            .with_routing(Arc::clone(&routed), aivyx_route::TaskKind::Chat)
        };
        let channel = RecChannel::new();

        // Turn 1: routed to `big`.
        let mut first = planner_for();
        first
            .begin_turn(&Message::text(session, "hi"), TurnId::new())
            .await;
        let _ = first.next_step(&[], &channel).await;
        assert_eq!(first.turn_costs()[0].0, "big");

        // Turn 2, same session, carrying a PDF: served by the default,
        // though the router's last decision for the session is `big`.
        let mut second = planner_for();
        second
            .begin_turn(
                &Message::document(session, "application/pdf", b"%PDF-".to_vec()),
                TurnId::new(),
            )
            .await;
        let _ = second.next_step(&[], &channel).await;

        assert_eq!(default.routes.lock().unwrap()[0].0, "default");
        let usage = second.turn_usage();
        assert_eq!(usage.input_tokens, 20);
        assert_eq!(second.turn_costs(), vec![("default".to_string(), usage)]);
        assert_eq!(gpu.hints.lock().unwrap().clone(), vec!["big".to_string()]);
        assert_eq!(
            default.hints.lock().unwrap().clone(),
            vec!["default".to_string()]
        );
    }

    /// Never tainted, never consented — `auto` mode doesn't need consent.
    struct CleanGuard;

    #[async_trait]
    impl aivyx_llm::EscalationGuard for CleanGuard {
        async fn taint(&self, _session: &str) -> Option<String> {
            None
        }
        fn consented(&self, _session: &str) -> bool {
            false
        }
    }

    #[tokio::test]
    async fn an_escalated_turn_is_billed_and_hinted_as_the_cloud_model() {
        let default = FakeLlmProvider::new(vec![]);
        let cloud = FakeLlmProvider::new(vec![final_step(30, 4)]);
        // Local: only `default`, which can't call tools. Cloud: `claude`,
        // which can — so a tool-advertising turn escalates in `auto`.
        let local = aivyx_route::Router::new(
            vec![routing_profile("default", "default", &[])],
            aivyx_route::TaskOverrides::default(),
        );
        let mut claude = routing_profile("cloud", "claude", &[aivyx_route::Capability::Tools]);
        claude.locality = aivyx_route::Locality::Cloud;
        let cloud_for_factory = Arc::clone(&cloud);
        let factory: aivyx_llm::ProviderFactory =
            Box::new(move |_endpoint: &aivyx_route::EndpointRef| {
                Ok(Arc::clone(&cloud_for_factory) as Arc<dyn LlmProvider>)
            });
        let routed = Arc::new(
            aivyx_llm::RoutedProvider::new(
                aivyx_route::ModelKey {
                    endpoint: aivyx_route::EndpointRef::new("default"),
                    id: "default".into(),
                },
                Arc::clone(&default) as Arc<dyn LlmProvider>,
                local,
                factory,
            )
            .with_escalation(aivyx_llm::EscalationSetup {
                router: aivyx_route::Router::new(vec![claude], aivyx_route::TaskOverrides::default())
                    .with_allow_cloud(true),
                mode: aivyx_llm::EscalationMode::Auto,
                no_local_candidate: true,
                tiers: vec![],
                guard: Arc::new(CleanGuard),
                observer: Arc::new(|_: &aivyx_llm::EscalationRecord| {}),
            }),
        );
        let registry = Arc::new(ToolRegistry::new(vec![
            Arc::new(FakeTool::new("echo")) as Arc<dyn Tool>
        ]));
        let mut planner = LlmPlanner::new(
            Arc::clone(&routed) as Arc<dyn LlmProvider>,
            registry,
            LlmPlannerConfig::new("default"),
        )
        .with_routing(Arc::clone(&routed), aivyx_route::TaskKind::Chat);
        let channel = RecChannel::new();
        planner
            .begin_turn(&Message::text(channel.session, "hi"), TurnId::new())
            .await;
        let _ = planner.next_step(&[], &channel).await;

        assert_eq!(cloud.routes.lock().unwrap()[0].0, "claude");
        let usage = planner.turn_usage();
        assert_eq!(usage.input_tokens, 30);
        assert_eq!(planner.turn_costs(), vec![("claude".to_string(), usage)]);
        assert_eq!(cloud.hints.lock().unwrap().clone(), vec!["claude".to_string()]);
    }

    /// One store for both sides, as the daemon's `RoutingGuard` is:
    /// the agent marks through `TaintSink`, the router reads through
    /// `EscalationGuard`.
    #[derive(Default)]
    struct SharedTaint(Mutex<std::collections::HashMap<String, String>>);

    #[async_trait]
    impl crate::TaintSink for SharedTaint {
        async fn mark(&self, session: &str, reason: &str) -> bool {
            let mut m = self.0.lock().unwrap();
            if m.contains_key(session) {
                return false;
            }
            m.insert(session.to_string(), reason.to_string());
            true
        }
    }

    #[async_trait]
    impl aivyx_llm::EscalationGuard for SharedTaint {
        async fn taint(&self, session: &str) -> Option<String> {
            self.0.lock().unwrap().get(session).cloned()
        }
        fn consented(&self, _session: &str) -> bool {
            false
        }
    }

    /// Final-review C1 — triggers and gate resumes send a message whose
    /// session differs from the channel's. The agent taints the channel's
    /// session; the router must check that same conversation, so a
    /// sensitive tool's output never follows an escalated first step to
    /// the cloud, even under `auto`.
    #[tokio::test]
    async fn sensitive_output_blocks_escalation_when_the_message_session_differs() {
        use crate::Agent;
        let cloud = FakeLlmProvider::new(vec![
            FakeStep {
                events: vec![],
                terminal: LlmStepEnd::ToolCalls {
                    calls: vec![ToolCallEnd {
                        call_id: "toolu_01".to_string(),
                        tool_name: "gmail.search".to_string(),
                        input: json!({}),
                        name_resolution: aivyx_llm::NameResolution::Known,
                    }],
                    text_so_far: String::new(),
                    usage: zero_usage(),
                },
            },
            final_step(1, 1),
        ]);
        let guard = Arc::new(SharedTaint::default());
        let local = aivyx_route::Router::new(
            vec![routing_profile("default", "default", &[])],
            aivyx_route::TaskOverrides::default(),
        );
        let mut claude = routing_profile("cloud", "claude", &[aivyx_route::Capability::Tools]);
        claude.locality = aivyx_route::Locality::Cloud;
        let cloud_for_factory = Arc::clone(&cloud);
        let factory: aivyx_llm::ProviderFactory =
            Box::new(move |_endpoint: &aivyx_route::EndpointRef| {
                Ok(Arc::clone(&cloud_for_factory) as Arc<dyn LlmProvider>)
            });
        let routed = Arc::new(
            aivyx_llm::RoutedProvider::new(
                aivyx_route::ModelKey {
                    endpoint: aivyx_route::EndpointRef::new("default"),
                    id: "default".into(),
                },
                FakeLlmProvider::new(vec![]) as Arc<dyn LlmProvider>,
                local,
                factory,
            )
            .with_escalation(aivyx_llm::EscalationSetup {
                router: aivyx_route::Router::new(vec![claude], aivyx_route::TaskOverrides::default())
                    .with_allow_cloud(true),
                mode: aivyx_llm::EscalationMode::Auto,
                no_local_candidate: true,
                tiers: vec![],
                guard: Arc::clone(&guard) as Arc<dyn aivyx_llm::EscalationGuard>,
                observer: Arc::new(|_: &aivyx_llm::EscalationRecord| {}),
            }),
        );
        let registry = Arc::new(ToolRegistry::new(vec![
            Arc::new(FakeTool::new("gmail.search")) as Arc<dyn Tool>
        ]));
        let planner_registry = Arc::clone(&registry);
        let agent = crate::ConcreteAgent::new(
            crate::AgentId::new(),
            aivyx_capability::CapabilitySet::from_scopes([Scope::parse("memory.read").unwrap()]),
            registry,
            Arc::new(crate::NullAuditHook),
            move || {
                Box::new(
                    LlmPlanner::new(
                        Arc::clone(&routed) as Arc<dyn LlmProvider>,
                        Arc::clone(&planner_registry),
                        LlmPlannerConfig::new("default"),
                    )
                    .with_routing(Arc::clone(&routed), aivyx_route::TaskKind::Chat),
                ) as Box<dyn TurnPlanner>
            },
        )
        .with_taint(
            Arc::clone(&guard) as Arc<dyn crate::TaintSink>,
            vec!["gmail.".to_string()],
        );

        let channel = RecChannel::new();
        let _ = agent
            .turn(Message::text(SessionId::new(), "summarize my email"), &channel)
            .await;

        assert!(
            guard.0.lock().unwrap().contains_key(&channel.session.to_string()),
            "the conversation is tainted"
        );
        assert_eq!(
            cloud.routes.lock().unwrap().len(),
            1,
            "only the untainted first step may reach the cloud"
        );
    }

    #[tokio::test]
    async fn unrouted_turn_costs_are_the_configured_model_and_turn_usage() {
        let provider = FakeLlmProvider::new(vec![final_step(7, 3)]);
        let mut planner = LlmPlanner::new(
            provider,
            Arc::new(ToolRegistry::new(vec![])),
            LlmPlannerConfig::new("m"),
        );

        let channel = RecChannel::new();
        planner
            .begin_turn(&Message::text(channel.session, "hi"), TurnId::new())
            .await;
        let _ = planner.next_step(&[], &channel).await;

        let usage = planner.turn_usage();
        assert_eq!(usage.output_tokens, 3);
        assert_eq!(planner.turn_costs(), vec![("m".to_string(), usage)]);
    }
}
