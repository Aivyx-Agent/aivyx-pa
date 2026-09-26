//! Turn planners — the seam between the loop and whatever drives tool calls.
//!
//! Phase 1 uses a deterministic `VecPlanner` that walks a fixed list of
//! steps. Phase 2 will add an LLM-backed planner that streams tokens and
//! parses tool calls as it goes. The turn loop doesn't care which it has;
//! it just asks `next_step` and dispatches.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::{ChannelContext, Message, TokenUsage, Tool, ToolId, ToolOutcome, ToolOutcomeSummary};

/// What the loop should do next. Mirrors the shapes a real LLM step can
/// produce — a tool call, a final message, or a stop — but with none of
/// the streaming machinery.
#[derive(Debug, Clone)]
pub enum NextStep {
    /// Call this tool with this input. The loop will scope-check it and
    /// either execute or deny.
    ///
    /// Phase 120 — `auto_corrected_from` carries the verbatim name the
    /// LLM originally emitted when the planner's fuzzy-match recovery
    /// path landed on a different `tool_id` than the model said. `None`
    /// for the dominant case (model emitted a registered name verbatim).
    /// Threaded through to the agent's `AuditTag::ToolCall` emission so
    /// forensic walks see the correction.
    ///
    /// Phase 126 — `extracted_from_text` carries the wrapper-tag
    /// identifier when the planner extracted this call from response
    /// TEXT (e.g. `<tool_code>` blocks some LLMs emit instead of using
    /// the protocol channel). `None` for the dominant case. Composes
    /// with `auto_corrected_from` — both can be `Some` when the
    /// extracted call carried a hallucinated tool name fuzzy-recovered
    /// on the way to dispatch.
    ToolCall {
        tool_id: ToolId,
        input: Value,
        #[doc(hidden)]
        auto_corrected_from: Option<String>,
        #[doc(hidden)]
        extracted_from_text: Option<String>,
    },

    /// Execute multiple tool calls concurrently. The loop dispatches all
    /// of them via `join_all`, observes every outcome, then asks the
    /// planner for the next step. Phase 40.
    ToolCalls(Vec<ToolCallRequest>),

    /// The planner has a final assistant message for the channel. Loop
    /// terminates with `TurnOutcome::Completed`.
    FinalMessage(String),

    /// No more steps — terminates with `TurnOutcome::Completed` and an
    /// empty final message. Used by planners that finish without a
    /// natural "final message" signal.
    Stop,
}

/// A single tool call within a [`NextStep::ToolCalls`] batch. Carries the
/// resolved `ToolId` (not the string name — resolution happens in the
/// planner before the batch reaches the turn loop).
#[derive(Debug, Clone)]
pub struct ToolCallRequest {
    pub tool_id: ToolId,
    pub input: Value,
    /// Phase 120 — verbatim name the LLM emitted before the planner's
    /// fuzzy-match recovery resolved to this `tool_id`. `None` in the
    /// dominant case. Threaded to the per-call `AuditTag::ToolCall`
    /// emission.
    pub auto_corrected_from: Option<String>,
    /// Phase 126 — wrapper-tag identifier (`"tool_code"` or
    /// `"tool_call"`) when the planner extracted this call from
    /// response TEXT rather than the protocol channel. `None` in
    /// the dominant case. Threaded to the per-call audit emission;
    /// composes with `auto_corrected_from` when both fire.
    pub extracted_from_text: Option<String>,
}

/// What the planner observes after each executed step. Carries only the
/// outcome summary (not the full `ToolOutcome`) so the planner can't peek
/// at the underlying `serde_json::Value` payload and make decisions based
/// on secret-y data — audit stays authoritative.
#[derive(Debug, Clone)]
pub struct StepObservation {
    pub tool_id: ToolId,
    pub summary: ToolOutcomeSummary,
}

/// The seam between the loop and its step source.
///
/// Phase 2 added three methods to this trait — `begin_turn`,
/// `observe_tool_outcome`, and the `channel` parameter on `next_step` —
/// so the LLM-backed planner can see the user message, stream text to
/// the channel as tokens arrive, and read the full `ToolOutcome` after
/// each dispatched call. All three additions have defaults where
/// possible so pre-Phase-2 planners (like [`VecPlanner`]) require
/// minimal updates.
#[async_trait]
pub trait TurnPlanner: Send + Sync {
    /// Called once, at the start of a turn, with the triggering user
    /// message and the turn's audit id. Deterministic planners can
    /// ignore both; LLM-backed planners seed their conversation
    /// history here, and the per-turn context hooks use `turn_id` to
    /// correlate audit events (e.g. an injected skill's
    /// `SkillInvocation`) with the surrounding `TurnStarted` /
    /// `TurnEnded` pair.
    async fn begin_turn(&mut self, _message: &Message, _turn_id: crate::TurnId) {}

    /// Model routing Part 3b — the conversation this turn belongs to,
    /// called just before [`Self::begin_turn`] with the channel's session
    /// id. That is the key taint is written under, so a planner that
    /// routes (and so checks taint and consent) must key by it rather
    /// than by `Message::session_id`, which trigger and gate-resume turns
    /// mint fresh. Planners that don't route ignore it.
    fn set_conversation(&mut self, _session: crate::SessionId) {}

    /// Return the next step given everything observed so far. The
    /// `channel` handle is available for planners that want to relay
    /// mid-step output (LLM token streaming); planners that don't
    /// stream just ignore it.
    async fn next_step(
        &mut self,
        observed: &[StepObservation],
        channel: &dyn ChannelContext,
    ) -> NextStep;

    /// Called by the turn loop immediately after a [`NextStep::ToolCall`]
    /// has been dispatched and its outcome is known, *before* the loop
    /// asks for the next step. LLM planners use this to append a
    /// `tool_result` message to their conversation history; other
    /// planners default to ignoring it.
    async fn observe_tool_outcome(&mut self, _tool_id: ToolId, _outcome: &ToolOutcome) {}

    /// POLISH_WAVES.md sub-project 4, item E — the rendered tool-result
    /// text this turn's planner has accumulated. The turn loop's own
    /// `observed: Vec<StepObservation>` deliberately carries only a
    /// summary, not tool output text (see `StepObservation`'s own doc
    /// comment) — this is the seam the turn loop uses instead, after
    /// the step loop exits, to build the identifier-fidelity check's
    /// source pool. Deterministic planners return empty (the default)
    /// — they have no LLM history to draw from.
    fn tool_result_texts(&self) -> Vec<String> {
        Vec::new()
    }

    /// Cumulative token usage across all LLM steps in this turn.
    /// The turn loop reads this after the step loop exits and passes
    /// it into `AuditTag::TurnEnded`. Deterministic planners return
    /// zero (the default).
    fn turn_usage(&self) -> TokenUsage {
        TokenUsage::default()
    }

    /// The model this planner runs on (e.g. `claude-opus-4-8`). The turn loop
    /// pairs it with [`turn_usage`](Self::turn_usage) into the Chapter-K
    /// `AuditTag::LlmCost` event so spend can be priced per turn.
    /// **Deterministic planners return `""`** — the loop then emits no
    /// `LlmCost` (there is no LLM spend to price).
    fn model(&self) -> &str {
        ""
    }

    /// The turn's LLM spend split by the model that actually served it —
    /// one Chapter-K `AuditTag::LlmCost` event per entry. A routed planner
    /// can use several models in one turn; the default is the single
    /// [`model`](Self::model) + [`turn_usage`](Self::turn_usage) pair, or
    /// nothing for a deterministic planner.
    fn turn_costs(&self) -> Vec<(String, TokenUsage)> {
        if self.model().is_empty() {
            vec![]
        } else {
            vec![(self.model().to_string(), self.turn_usage())]
        }
    }
}

/// Deterministic planner that walks a fixed script of steps. Used for
/// Phase 1 tests — every step is pre-recorded, no branching on
/// observations. Phase 2's LLM planner will be a different impl of the
/// same trait.
pub struct VecPlanner {
    steps: std::collections::VecDeque<NextStep>,
}

impl VecPlanner {
    pub fn new(steps: impl IntoIterator<Item = NextStep>) -> Self {
        VecPlanner {
            steps: steps.into_iter().collect(),
        }
    }
}

#[async_trait]
impl TurnPlanner for VecPlanner {
    async fn next_step(
        &mut self,
        _observed: &[StepObservation],
        _channel: &dyn ChannelContext,
    ) -> NextStep {
        self.steps.pop_front().unwrap_or(NextStep::Stop)
    }
}

/// A tool registry — how the loop looks up a `Tool` by its `ToolId`. A
/// linearly-scanned `Vec<Arc<dyn Tool>>` behind an `RwLock` so the set
/// can be **hot-swapped** at runtime (e.g. when an MCP server signals
/// `tools/list_changed`) while the agent shares it via `Arc`. Lookups
/// take a brief read lock and return owned clones — never a guard — so
/// no lock is ever held across an `.await`.
pub struct ToolRegistry {
    tools: std::sync::RwLock<Vec<Arc<dyn Tool>>>,
}

impl ToolRegistry {
    pub fn new(tools: Vec<Arc<dyn Tool>>) -> Self {
        ToolRegistry {
            tools: std::sync::RwLock::new(tools),
        }
    }

    pub fn get(&self, id: ToolId) -> Option<Arc<dyn Tool>> {
        self.tools
            .read()
            .unwrap()
            .iter()
            .find(|t| t.id() == id)
            .cloned()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.read().unwrap().is_empty()
    }

    /// An owned snapshot of every registered tool. Used by the LLM
    /// planner to build the `LlmToolDescriptor` list. Returns owned
    /// `Arc`s so the caller holds no lock.
    pub fn snapshot(&self) -> Vec<Arc<dyn Tool>> {
        self.tools.read().unwrap().clone()
    }

    /// Look up a tool by its human name. Linear scan — the registry
    /// holds at most a few dozen tools in realistic use, and the LLM
    /// planner only calls this once per LLM step.
    pub fn find_by_name(&self, name: &str) -> Option<ToolId> {
        self.tools
            .read()
            .unwrap()
            .iter()
            .find(|t| t.name() == name)
            .map(|t| t.id())
    }

    /// Hot-swap: remove the tools with the given ids and append
    /// `additions`, atomically under the write lock. Returns
    /// `(removed, added)`. Used to apply an MCP server's
    /// `*/list_changed` refresh without restarting the daemon —
    /// capability-safe because the replacements carry the same
    /// `required_scope` family the role already granted.
    pub fn replace_tools(
        &self,
        remove_ids: &[ToolId],
        additions: Vec<Arc<dyn Tool>>,
    ) -> (usize, usize) {
        let mut guard = self.tools.write().unwrap();
        let before = guard.len();
        guard.retain(|t| !remove_ids.contains(&t.id()));
        let removed = before - guard.len();
        let added = additions.len();
        guard.extend(additions);
        (removed, added)
    }
}

// ---------------------------------------------------------------------------
// Phase 4 task 1 — Tool trait surface audit
//
// The purpose of this test module is *not* to exercise `ToolRegistry`
// behavior — that's covered indirectly through `agent.rs`'s happy-path
// and scope-denial tests. It exists to prove, ahead of Phase 4 task 2's
// `FsReadTool`, that a concrete `Tool` impl with the shape a real
// filesystem tool needs:
//
//   - holds its own state (here, a scope prefix; there, a sandbox root)
//   - derives an input-specific `Scope` from the input JSON via R1
//   - is reachable through every registry lookup path (`get`,
//     `find_by_name`, `iter_tools`)
//   - surfaces a JSON input schema the LLM planner can stringify
//
// ...compiles and works against the existing Phase 0–3 trait surface
// with *no* amendment to the `Tool` trait, the `ToolContext` struct,
// the `ToolOutcome` enum, or the `ToolRegistry` API. If this test ever
// stops compiling without changes to the test itself, the contract has
// drifted and task 2's real filesystem tool is at risk.
//
// See `docs/PHASE_4.md` task 1 for the design decision.
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tool_surface_audit {
    use super::*;

    use std::sync::OnceLock;

    use async_trait::async_trait;
    use serde_json::json;

    use aivyx_capability::{CapabilitySet, Scope, TrustTier};

    use crate::{Tool, ToolContext, ToolOutcome, Verification};

    /// A skeleton tool shaped like the upcoming `FsReadTool`: holds a
    /// scope-prefix string ("sandbox root"), derives a path-qualified
    /// scope from the input's `"path"` field, and returns a
    /// `Completed/NotApplicable` result. No real filesystem access —
    /// this is a type-level audit, not a behavioral test.
    struct FsReadSkeleton {
        id: ToolId,
        sandbox_root: String,
    }

    impl FsReadSkeleton {
        fn new(sandbox_root: impl Into<String>) -> Self {
            FsReadSkeleton {
                id: ToolId::new(),
                sandbox_root: sandbox_root.into(),
            }
        }
    }

    #[async_trait]
    impl Tool for FsReadSkeleton {
        fn id(&self) -> ToolId {
            self.id
        }
        fn name(&self) -> &str {
            "fs.read"
        }
        fn description(&self) -> &str {
            "Read a UTF-8 file under the agent's sandbox root."
        }
        fn input_schema(&self) -> &serde_json::Value {
            static SCHEMA: OnceLock<serde_json::Value> = OnceLock::new();
            SCHEMA.get_or_init(|| {
                json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Relative path under the sandbox root."
                        }
                    },
                    "required": ["path"]
                })
            })
        }

        fn required_scope(&self, input: &serde_json::Value) -> Scope {
            // R1: the scope is derived from the *input*, not hard-coded.
            // Phase 4 task 2's real tool will canonicalize the path and
            // compare the canonicalized form against the sandbox prefix
            // (Q4 in PHASE_4.md). The skeleton here keeps the shape —
            // a qualified `fs.read:<path>` — without doing I/O.
            let path = input
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("/dev/null");
            Scope::parse(&format!("fs.read:{}/{path}", self.sandbox_root))
                .expect("sandbox path forms a legal scope qualifier")
        }

        async fn execute(&self, _input: serde_json::Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
            // The task-1 skeleton never actually runs — the audit is
            // structural, not behavioral. Task 2 provides the real impl.
            ToolOutcome::Completed {
                output: json!({"bytes": 0}),
                verified: Verification::NotApplicable,
            }
        }
    }

    #[test]
    fn concrete_tool_is_reachable_through_every_registry_lookup_path() {
        let tool = Arc::new(FsReadSkeleton::new("/home/user/aivyx-sandbox"));
        let id = tool.id();
        let registry = ToolRegistry::new(vec![tool as Arc<dyn Tool>]);

        assert!(!registry.is_empty());

        // Path 1: LLM planner round-trips `name → id → Tool` at tool-call
        // dispatch time. Both halves must succeed for a real tool call
        // to reach execute.
        let found_id = registry
            .find_by_name("fs.read")
            .expect("fs.read must be findable by name");
        assert_eq!(
            found_id, id,
            "name lookup must return the same id the tool reports"
        );

        let found_tool = registry
            .get(found_id)
            .expect("id lookup must return the tool");
        assert_eq!(found_tool.name(), "fs.read");

        // Path 2: LLM planner walks the registry snapshot once at
        // construction to build the descriptor list it sends to the model.
        let snapshot = registry.snapshot();
        let names: Vec<&str> = snapshot.iter().map(|t| t.name()).collect();
        assert_eq!(names, vec!["fs.read"]);

        // Path 3: `input_schema()` returns a stable `&serde_json::Value`
        // — the descriptor-building path at the planner needs this to
        // serialize into the Anthropic `tool_input_schema` field.
        let schema = found_tool.input_schema();
        assert_eq!(schema["type"], json!("object"));
        assert_eq!(schema["required"], json!(["path"]));
    }

    #[test]
    fn replace_tools_hot_swaps_by_id() {
        let keep = Arc::new(FsReadSkeleton::new("/sandbox"));
        let drop_me = Arc::new(FsReadSkeleton::new("/sandbox"));
        let keep_id = keep.id();
        let drop_id = drop_me.id();
        let registry = ToolRegistry::new(vec![keep as Arc<dyn Tool>, drop_me as Arc<dyn Tool>]);
        assert_eq!(registry.snapshot().len(), 2);

        // Swap out `drop_me`, add a fresh tool — atomically.
        let added = Arc::new(FsReadSkeleton::new("/sandbox"));
        let added_id = added.id();
        let (removed, added_n) = registry.replace_tools(&[drop_id], vec![added as Arc<dyn Tool>]);
        assert_eq!((removed, added_n), (1, 1));

        // The kept + added tools resolve; the dropped one is gone.
        assert!(registry.get(keep_id).is_some());
        assert!(registry.get(added_id).is_some());
        assert!(registry.get(drop_id).is_none());
        assert_eq!(registry.snapshot().len(), 2);
    }

    #[test]
    fn r1_scope_derivation_uses_the_input_path() {
        let tool = FsReadSkeleton::new("/home/user/aivyx-sandbox");

        // A read under the sandbox derives a path-qualified scope.
        let scope = tool.required_scope(&json!({"path": "notes/today.md"}));
        assert_eq!(scope.base(), "fs.read");
        assert_eq!(
            scope.qualifier(),
            Some("/home/user/aivyx-sandbox/notes/today.md")
        );

        // A missing-path input falls back to /dev/null. This is
        // deliberately a *legal* scope — task 2's real tool will instead
        // fail the scope-derivation step by returning an error path, and
        // the loop's scope check at `agent.rs:314` will deny the call.
        // Here we just prove the trait method is *pure* (no panic on
        // missing field, no I/O).
        let fallback = tool.required_scope(&json!({}));
        assert_eq!(fallback.base(), "fs.read");
    }

    #[test]
    fn broad_capability_grants_derived_narrow_scope() {
        // Reprises the `r1_narrow_scope_is_granted_by_broad_capability`
        // check from lib.rs, but against the filesystem shape. An agent
        // with `fs.read:/home/user/aivyx-sandbox/**` must be able to
        // cover a tool call whose R1 derives
        // `fs.read:/home/user/aivyx-sandbox/notes/today.md`. This is the
        // scope system's core promise and the reason Phase 4 picked a
        // filesystem tool to stress-test it.
        let held =
            CapabilitySet::from_scopes([
                Scope::parse("fs.read:/home/user/aivyx-sandbox/**").unwrap()
            ]);
        let effective = held.intersect(TrustTier::Trusted.default_ceiling());

        let tool = FsReadSkeleton::new("/home/user/aivyx-sandbox");
        let needed = tool.required_scope(&json!({"path": "notes/today.md"}));

        assert!(
            effective.grants(&needed),
            "broad sandbox scope must cover a path under the sandbox root"
        );
    }

    #[test]
    fn out_of_sandbox_scope_is_not_granted_by_sandbox_capability() {
        // The negative — the *whole point* of prefix-attenuated scopes.
        // An agent holding `fs.read:/home/user/aivyx-sandbox/**` must
        // NOT cover `fs.read:/etc/passwd`. Task 2's real tool will reach
        // this by canonicalizing an evil input like `"../etc/passwd"`;
        // the skeleton here simulates by passing an absolute path
        // through the template directly.
        let held =
            CapabilitySet::from_scopes([
                Scope::parse("fs.read:/home/user/aivyx-sandbox/**").unwrap()
            ]);
        let effective = held.intersect(TrustTier::Trusted.default_ceiling());

        let attacker = Scope::parse("fs.read:/etc/passwd").unwrap();
        assert!(
            !effective.grants(&attacker),
            "sandbox scope must not grant reads outside the sandbox prefix"
        );
    }
}
