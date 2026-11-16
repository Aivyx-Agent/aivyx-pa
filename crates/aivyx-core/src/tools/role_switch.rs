//! `RoleSwitchTool` — Phase 14 sub-agent role-switching primitive
//! (**PRODUCT.md P1**).
//!
//! ## What it does (Task 3 shape)
//!
//! Task 2 shipped the **scope plumbing, tool registration, and
//! dispatch gate**. Task 3 replaces the placeholder `execute` body
//! with a real inline sub-session: the tool owns an optional
//! `child_factory` closure, captured at session-construction time,
//! that knows how to build a fully-wired child `ConcreteAgent` for
//! any declared role. When `execute` fires (the scope gate has
//! already passed by then), the tool calls the factory with the
//! `target` role name, runs `child.turn(Message::text(session_id,
//! task), ctx.channel).await` synchronously, and returns the child's
//! final message as the `ToolOutcome::Completed` output JSON.
//!
//! ## Why the child reuses the parent's channel
//!
//! The child agent runs on `ctx.channel` — the same
//! `&dyn ChannelContext` the parent is using. This is intentional:
//!
//! - **Writer sharing**: every child stream event lands in the same
//!   terminal the parent's events lands in, in event-emission order.
//!   The user sees a seamless transcript with no "child output
//!   started / stopped" framing.
//! - **Cancellation sharing**: a ctrl-C at the user's terminal
//!   cancels the channel's token, and both the parent's deadline
//!   task (waiting to fire) and the child's in-flight turn observe
//!   it. The child aborts, the parent's observation of the tool
//!   outcome is `Cancelled`, and the parent's turn loop then
//!   short-circuits at the next top-of-loop cancellation check.
//!   The parent doesn't need to synthesize a cancellation path of
//!   its own.
//! - **Trust tier stability**: `channel.trust_tier()` is constant
//!   for the channel's lifetime, so the child's turn-start
//!   intersection with `tier.default_ceiling()` produces the same
//!   tier ceiling the parent is operating under. Combined with the
//!   child's declared envelope (which was already attenuated against
//!   the parent's chain by `assemble_role_envelope`), this gives the
//!   PRODUCT.md P1.3 "structurally impossible escalation" guarantee:
//!   the child cannot hold any scope the parent did not transitively
//!   grant, because (a) `assemble_role_envelope` walks leaf-to-root
//!   and intersects, and (b) the tier ceiling is identical.
//!
//! ## Why the dispatch gate is free
//!
//! `Tool::required_scope(&input)` is documented as pure and is
//! called by the turn loop **before** `execute` runs. For
//! `role.switch`, the function just reads `input["target"]` and
//! returns `Scope::parse(&format!("role.switch:{target}"))`. The
//! loop's existing scope gate then intersects the derived scope
//! with the agent's effective capability set and short-circuits
//! to `ToolOutcome::Denied { scope, held }` if the intersection
//! is empty. **No explicit denial path is written inside
//! `execute`** — it falls out of the existing architecture.
//!
//! ## Why the factory is `Option`al
//!
//! The unit tests in this module build a bare `RoleSwitchTool` with
//! no factory and pin the `required_scope` contract (the dispatch
//! gate's input, essentially) without a live session layer. In real
//! use, the session layer constructs the tool with
//! `::with_child_factory(...)` before pushing it into the
//! `ToolRegistry`. A factory-less tool that actually reaches
//! `execute` returns a `Failed` outcome — "role.switch was invoked
//! but no session is wired for sub-sessions" — rather than
//! panicking, so a misconfigured test path surfaces cleanly.
//!
//! ## Input shape
//!
//! ```json
//! { "target": "researcher", "task": "read file X and summarize" }
//! ```
//!
//! - `target`: the role name to switch into. Must name a role the
//!   `child_factory` knows about; unknown targets surface as a
//!   `Failed` outcome with a human-readable detail.
//! - `task`: the initial user message the child's planner sees.
//!   Q2's "clean-slate child conversation history" decision means
//!   nothing else crosses the boundary.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use serde_json::{Value, json};

use aivyx_capability::Scope;

use crate::{
    Agent, AivyxError, Message, Tool, ToolContext, ToolId, ToolOutcome, TurnOutcome, Verification,
};

/// The type of the closure that builds a child agent for a given
/// target role name. Returns `Ok(agent)` if the target resolves and
/// a child agent can be built; returns `Err(detail)` with a
/// human-readable explanation otherwise (unknown role, attenuation
/// failure, construction failure, etc.).
///
/// The factory is called *after* the dispatch gate has already
/// verified that the caller's capability set grants
/// `role.switch:<target>`, so the factory does not need to re-check
/// the capability layer — it just needs to perform the mechanical
/// step of "build a `ConcreteAgent` for this role with an envelope
/// computed by `assemble_role_envelope`, wired to the same provider,
/// audit, and tool registry the parent uses."
///
/// The closure is `Send + Sync` so it can live behind an `Arc` and
/// be cloned across turn boundaries safely.
pub type ChildAgentFactory = dyn Fn(&str) -> Result<Box<dyn Agent>, String> + Send + Sync;

/// The Phase 14 sub-agent role-switching tool. See the module docs
/// for the full picture.
///
/// ## Why the factory lives behind `OnceLock`
///
/// `RoleSwitchTool` is held in the `ToolRegistry` as `Arc<dyn Tool>`,
/// which means the session layer has no mutable path to the tool
/// after the registry is built. The factory closure, however, needs
/// to be wired **after** the registry exists, because the child
/// agent it produces must reuse the same tool registry the parent
/// is running against (sub-sessions reuse the tool set by
/// construction — see PRODUCT.md P1's "same process, single
/// execution pointer" line).
///
/// The circular dependency — factory needs the registry; registry
/// needs the tool — is broken by `OnceLock<Arc<ChildAgentFactory>>`:
///
/// 1. Session layer creates `Arc::new(RoleSwitchTool::new())` —
///    factory slot is empty.
/// 2. Session layer pushes a clone of that `Arc` into `tool_list`
///    (upcast to `Arc<dyn Tool>`) and builds `ToolRegistry`.
/// 3. Session layer builds the factory closure, capturing
///    `Arc::clone(&tools)` and all the other collaborators.
/// 4. Session layer calls `role_switch_tool.set_child_factory(...)`
///    on the *outer* `Arc<RoleSwitchTool>` handle. Because the
///    registry's copy and the outer handle are the same `Arc`, the
///    `OnceLock::set` is visible through both.
///
/// `OnceLock` is the right primitive here (not `Mutex`, not `RwLock`):
/// the factory is written exactly once at startup and read many
/// times from the tool's `execute`, so set-once semantics are exact
/// and the read path avoids any locking overhead.
pub struct RoleSwitchTool {
    id: ToolId,
    schema: Value,
    child_factory: OnceLock<Arc<ChildAgentFactory>>,
}

impl std::fmt::Debug for RoleSwitchTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoleSwitchTool")
            .field("id", &self.id)
            .field("has_child_factory", &self.child_factory.get().is_some())
            .finish()
    }
}

impl Default for RoleSwitchTool {
    fn default() -> Self {
        Self::new()
    }
}

impl RoleSwitchTool {
    pub fn new() -> Self {
        RoleSwitchTool {
            id: ToolId::new(),
            schema: role_switch_input_schema(),
            child_factory: OnceLock::new(),
        }
    }

    /// Attach a child agent factory. Called by the session layer at
    /// startup, **after** the `ToolRegistry` has been built from a
    /// clone of `Arc<RoleSwitchTool>`, so the factory closure can
    /// capture the registry handle for child-agent construction.
    /// See the struct-level doc for the full initialization dance.
    ///
    /// Takes `&self` (not `&mut self`) because the tool is already
    /// inside an `Arc` at the call site; `OnceLock::set` handles
    /// the thread-safety. Returns `Err(factory)` if a factory was
    /// already set, giving the caller the option to observe
    /// misconfiguration without panicking. The session layer in
    /// practice treats this as a hard error (a bug in its own
    /// initialization ordering), but the type reflects that it's
    /// recoverable in principle.
    pub fn set_child_factory(
        &self,
        factory: Arc<ChildAgentFactory>,
    ) -> Result<(), Arc<ChildAgentFactory>> {
        self.child_factory.set(factory)
    }
}

#[async_trait]
impl Tool for RoleSwitchTool {
    fn id(&self) -> ToolId {
        self.id
    }

    fn name(&self) -> &str {
        "role.switch"
    }

    fn description(&self) -> &str {
        "Switch the agent into a child role for a bounded sub-session. \
         Input is a JSON object with two string fields: `target` (the \
         role name to switch into) and `task` (the initial user message \
         the child's planner sees). The child runs under the target \
         role's attenuated capability envelope per PRODUCT.md P1; when \
         the sub-task completes, control returns to the parent role \
         and the child's final message is returned as the tool output. \
         The child's capability envelope is computed by \
         `assemble_role_envelope` from the parent's chain, so a child \
         cannot hold any scope the parent did not transitively grant it."
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn required_scope(&self, input: &Value) -> Scope {
        // Pull the target role name from input. If it's missing or
        // not a string, return a deny scope no role can hold so the
        // loop's scope gate produces a clean `Denied` with the
        // malformed-input base distinguishable from a policy
        // denial. Same pattern as fs.rs's `deny_scope_for`.
        let Some(target) = input.get("target").and_then(|v| v.as_str()) else {
            return role_switch_deny_scope();
        };

        // A target that contains `:` or other characters that
        // would confuse scope parsing collapses to the deny scope.
        // Role names in Aivyx configs are bare identifiers; a
        // dispatched call with a `:` in the target is either a
        // malformed planner output or an injection attempt, and
        // the right response is "deny, don't parse-pun."
        if target.contains(':') || target.is_empty() {
            return role_switch_deny_scope();
        }

        Scope::parse(&format!("role.switch:{target}")).unwrap_or_else(role_switch_deny_scope)
    }

    async fn execute(&self, input: Value, ctx: &ToolContext<'_>) -> ToolOutcome {
        // The scope gate has already passed by the time we get
        // here — Task 2 pinned that contract. Extract the target
        // and task strings.
        let target = input
            .get("target")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let task = input
            .get("task")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        // A factory-less tool that actually executes is a
        // misconfigured session layer. Return `Failed` rather than
        // panicking so the surrounding test / binary surfaces the
        // problem as a tool-level error the planner observes and
        // can report. This path is reachable only in test setups
        // that forgot to call `set_child_factory`.
        let Some(factory) = self.child_factory.get() else {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: "role.switch invoked without a child-agent factory; \
                         the session layer must call \
                         RoleSwitchTool::set_child_factory(...) after \
                         registering the tool"
                    .to_string(),
            });
        };

        // Build the child agent via the factory. An `Err` here means
        // the factory couldn't resolve the target — unknown role,
        // capability assembly failure, etc. — and the right surface
        // is `Failed` with the factory's human-readable detail.
        let child = match factory(&target) {
            Ok(child) => child,
            Err(detail) => {
                return ToolOutcome::Failed(AivyxError::Tool {
                    tool: self.id,
                    detail: format!("role.switch target={target:?}: {detail}"),
                });
            }
        };

        // Run the child's turn on the parent's channel. The child's
        // `turn()` emits its own `TurnStarted` / `TurnEnded` audit
        // events against its own `TurnId`, so the audit chain shows
        // the parent→child→parent transition with distinct turn
        // identifiers and distinct `effective_capabilities`
        // snapshots — this is the PRODUCT.md P1.4 "each turn tagged
        // by role active at turn-start" guarantee.
        //
        // The child's `Message::text` uses the same `session_id` as
        // the parent (Q2: clean-slate *conversation history*, not
        // clean-slate session). The session_id is the outer chat's
        // identity; the clean slate is the child's planner starting
        // with no prior observed steps, which is what
        // `ConcreteAgent::turn` already does on every invocation.
        //
        // A19 — a federation peer's request stays a peer's request in the
        // child turn: no operator profile, recall, skill injection or
        // refiner, and no `team.run`. Operator and System parents are
        // unchanged (the child is Operator-origin, as before).
        let mut child_message = Message::text(ctx.session_id, task.clone());
        if ctx.message_origin == crate::MessageOrigin::Peer {
            child_message = child_message.from_peer();
        }
        let child_outcome = child.turn(child_message, ctx.channel).await;

        // Translate the child's `TurnOutcome` into a parent-facing
        // `ToolOutcome`. A `Completed` child turn becomes a
        // `Completed` tool call carrying the child's final message;
        // any other termination (cancelled, timed out, failed) maps
        // to a structured tool output the parent's planner can read
        // and react to, without surfacing as a parent-level
        // `Failed` outcome (which would bubble out of the parent
        // turn entirely).
        match child_outcome {
            TurnOutcome::Completed {
                final_message,
                tool_calls_made,
                duration,
            } => ToolOutcome::Completed {
                output: json!({
                    "status": "completed",
                    "target": target,
                    "final_message": final_message,
                    "tool_calls_made": tool_calls_made,
                    "duration_ms": duration.as_millis() as u64,
                }),
                // The child's sub-session is a bounded unit of
                // work with its own tool calls, each of which
                // carries its own `Verification` at emission time.
                // The composite "did the role.switch call itself
                // verify" question is not meaningful — the tool
                // didn't touch a verifiable substrate, it ran a
                // conversation. `NotApplicable` matches the Task 2
                // placeholder rationale.
                verified: Verification::NotApplicable,
            },
            TurnOutcome::Cancelled { tool_calls_made } => ToolOutcome::Completed {
                output: json!({
                    "status": "cancelled",
                    "target": target,
                    "tool_calls_made": tool_calls_made,
                }),
                verified: Verification::NotApplicable,
            },
            TurnOutcome::TimedOut {
                tool_calls_made,
                elapsed,
            } => ToolOutcome::Completed {
                output: json!({
                    "status": "timed_out",
                    "target": target,
                    "tool_calls_made": tool_calls_made,
                    "elapsed_ms": elapsed.as_millis() as u64,
                }),
                verified: Verification::NotApplicable,
            },
            TurnOutcome::Escalated {
                reason,
                tool_calls_made,
                ..
            } => ToolOutcome::Completed {
                output: json!({
                    "status": "escalated",
                    "target": target,
                    "reason": reason,
                    "tool_calls_made": tool_calls_made,
                }),
                verified: Verification::NotApplicable,
            },
            TurnOutcome::MaxStepsExceeded {
                tool_calls_made,
                duration,
                max_steps,
            } => ToolOutcome::Completed {
                output: json!({
                    "status": "max_steps_exceeded",
                    "target": target,
                    "tool_calls_made": tool_calls_made,
                    "duration_ms": duration.as_millis() as u64,
                    "max_steps": max_steps,
                }),
                verified: Verification::NotApplicable,
            },
            // Chapter Bridle — child sub-session stopped on a repeated
            // identical tool call. Surfaced to the parent as a bounded
            // completion (like the other non-Failed terminals) with the
            // synthesized final message.
            TurnOutcome::Looping {
                final_message,
                tool_calls_made,
                duration,
                repeat_limit,
            } => ToolOutcome::Completed {
                output: json!({
                    "status": "looping",
                    "target": target,
                    "final_message": final_message,
                    "tool_calls_made": tool_calls_made,
                    "duration_ms": duration.as_millis() as u64,
                    "repeat_limit": repeat_limit,
                }),
                verified: Verification::NotApplicable,
            },
            TurnOutcome::Failed(err) => ToolOutcome::Completed {
                output: json!({
                    "status": "failed",
                    "target": target,
                    "error": err.to_string(),
                }),
                verified: Verification::NotApplicable,
            },
        }
    }
}

/// Produce a deny scope for malformed `role.switch` input. The
/// qualifier is a reserved sentinel no real role name can collide
/// with (contains a `/` which role names forbid), so any
/// CapabilitySet that does not contain this exact sentinel denies.
/// Mirrors `fs.rs::deny_scope_for`'s pattern.
fn role_switch_deny_scope() -> Scope {
    // The deny scope parses cleanly (base `role.switch` is in
    // KNOWN_BASES and the qualifier isn't the forbidden `*`), and
    // the sentinel is long + contains `/` so no operator config
    // would ever name a role this. The `/` also pushes dispatch
    // through `QualifierKind::PathGlob`, which means an operator
    // who somehow wanted to grant it would need an explicit
    // glob match — deny-by-default is preserved.
    Scope::parse("role.switch:__aivyx_deny__/invalid-input")
        .expect("role.switch deny scope must parse")
}

fn role_switch_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "target": {
                "type": "string",
                "description": "Role name to switch into. Must be a role \
                    declared in the loaded config; must not contain a \
                    colon. The child runs under the target role's \
                    attenuated capability envelope per PRODUCT.md P1."
            },
            "task": {
                "type": "string",
                "description": "Initial user message the child's planner \
                    sees. Q2's clean-slate decision means the parent's \
                    conversation history is NOT threaded through."
            }
        },
        "required": ["target", "task"]
    })
}

#[cfg(test)]
mod tests {
    //! Pure-fn tests for `RoleSwitchTool`. These pin the
    //! `required_scope` contract — the dispatch gate's input,
    //! essentially — without requiring a full turn-loop setup. A
    //! separate session-layer integration test (Task 3) pins the
    //! end-to-end allowed/denied behavior against a real child
    //! agent flow.

    use super::*;
    use aivyx_capability::{CapabilitySet, Scope};

    fn scope(s: &str) -> Scope {
        Scope::parse(s).expect("test scope must parse")
    }

    #[test]
    fn tool_name_and_id_are_stable() {
        let t = RoleSwitchTool::new();
        assert_eq!(t.name(), "role.switch");
        assert_ne!(t.id().0, uuid::Uuid::nil());
    }

    #[test]
    fn required_scope_reads_target_and_produces_qualified_scope() {
        let t = RoleSwitchTool::new();
        let input = json!({ "target": "researcher", "task": "summarize" });
        let s = t.required_scope(&input);
        assert_eq!(s.base(), "role.switch");
        assert_eq!(s.qualifier(), Some("researcher"));
    }

    #[test]
    fn required_scope_denies_missing_target() {
        let t = RoleSwitchTool::new();
        let input = json!({ "task": "no target here" });
        let s = t.required_scope(&input);
        assert_eq!(
            s.qualifier(),
            Some("__aivyx_deny__/invalid-input"),
            "malformed input must produce the reserved deny sentinel"
        );
    }

    #[test]
    fn required_scope_denies_colon_in_target() {
        // A `:` in the target would pun against scope parsing:
        // `role.switch:evil:researcher` would parse as qualifier
        // `evil:researcher`, which SimpleGlob would then treat as
        // an arbitrary string. Closing that hole at required_scope
        // time is cheaper than teaching the dispatch gate about
        // composite qualifiers.
        let t = RoleSwitchTool::new();
        let input = json!({ "target": "evil:researcher", "task": "x" });
        let s = t.required_scope(&input);
        assert_eq!(s.qualifier(), Some("__aivyx_deny__/invalid-input"));
    }

    #[test]
    fn scope_gate_denies_when_role_lacks_target_scope() {
        // A `coder` role holding `role.switch:researcher` asking
        // to switch into `scribe` (which it did not declare). The
        // derived needed scope is `role.switch:scribe`, and the
        // held set's `role.switch:researcher` does NOT grant it
        // (SimpleGlob equality mismatch, Rule 3). The CapabilitySet
        // intersection therefore reports no grant.
        let held = CapabilitySet::from_scopes([scope("role.switch:researcher")]);
        let t = RoleSwitchTool::new();
        let input = json!({ "target": "scribe", "task": "x" });
        let needed = t.required_scope(&input);
        assert!(
            !held.grants(&needed),
            "role holding role.switch:researcher must not be granted \
             role.switch:scribe via the scope gate"
        );
    }

    #[test]
    fn scope_gate_allows_when_role_holds_matching_qualified_scope() {
        let held = CapabilitySet::from_scopes([scope("role.switch:researcher")]);
        let t = RoleSwitchTool::new();
        let input = json!({ "target": "researcher", "task": "x" });
        let needed = t.required_scope(&input);
        assert!(held.grants(&needed));
    }

    #[test]
    fn scope_gate_allows_when_role_holds_unqualified_role_switch() {
        // Rule 2: held unqualified grants qualified needed. The
        // `default` root role declaring unqualified `role.switch`
        // lets descendants declare qualified forms and have them
        // inherit cleanly under the assemble_role_envelope walk.
        let held = CapabilitySet::from_scopes([scope("role.switch")]);
        let t = RoleSwitchTool::new();
        let input = json!({ "target": "anything", "task": "x" });
        let needed = t.required_scope(&input);
        assert!(held.grants(&needed));
    }

    #[test]
    fn set_child_factory_attaches_the_closure_via_once_lock() {
        // Pin the `set_child_factory` path: a newly-constructed
        // tool has no factory, and calling `set_child_factory`
        // succeeds and flips the `has_child_factory` Debug field.
        let t = RoleSwitchTool::new();
        let dbg_before = format!("{:?}", t);
        assert!(
            dbg_before.contains("has_child_factory: false"),
            "fresh tool must report has_child_factory: false, got {dbg_before}"
        );

        let factory: Arc<ChildAgentFactory> =
            Arc::new(|_target: &str| Err("test: no factory wired".to_string()));
        assert!(
            t.set_child_factory(factory).is_ok(),
            "first set_child_factory call must succeed"
        );

        let dbg_after = format!("{:?}", t);
        assert!(
            dbg_after.contains("has_child_factory: true"),
            "after set_child_factory, Debug must report has_child_factory: true, got {dbg_after}"
        );
        assert_eq!(t.name(), "role.switch");
    }

    #[test]
    fn set_child_factory_rejects_a_second_call() {
        // OnceLock semantics: the second set returns Err with the
        // factory we tried to pass, preserving the first one. This
        // is the "session layer initialization ordering bug"
        // diagnostic path — the caller can observe the double-set
        // and surface it as a configuration error rather than
        // silently overwriting the first factory.
        let t = RoleSwitchTool::new();
        let factory_a: Arc<ChildAgentFactory> = Arc::new(|_target: &str| Err("a".to_string()));
        let factory_b: Arc<ChildAgentFactory> = Arc::new(|_target: &str| Err("b".to_string()));

        assert!(t.set_child_factory(factory_a).is_ok(), "first set ok");
        match t.set_child_factory(factory_b) {
            Ok(()) => panic!("second set must return Err, got Ok"),
            Err(returned) => {
                // The returned Err carries the second factory (the
                // one that wasn't installed), not the first. We
                // can't compare closures, but we can at least
                // verify it's a valid Arc that still has its
                // original strong count of 1 from the Err path.
                assert!(Arc::strong_count(&returned) >= 1);
            }
        }
    }
}
