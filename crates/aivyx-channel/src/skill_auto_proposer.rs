//! Phase 112 Task 4 — Background-task wiring for the skill
//! auto-proposer.
//!
//! Stitches together the two stages shipped in Tasks 2-3:
//!
//! 1. **Heuristic gate** (Phase 112 Task 2,
//!    [`aivyx_core::skill_proposer::heuristic::is_candidate`])
//!    — cheap deterministic filter.
//! 2. **LLM-judge call** (Phase 112 Task 3,
//!    [`aivyx_core::skill_proposer::judge::judge`]) — confirms
//!    the candidate is worth proposing and runs the dedup check
//!    in the same round-trip.
//!
//! Returns a single [`SkillProposerOutcome`] enum so the caller
//! can fan out on the verdict without dealing with a
//! `Result<...>` — the outcome enum encodes both happy paths
//! and every failure mode the auto-proposer needs to be
//! resilient against. **The auto-proposer never propagates an
//! error to the turn loop;** the turn outcome is already
//! committed by the time this fires.
//!
//! ## Q2b implication: background-spawn from post-`finalize`
//!
//! The Phase 112 Q-block (Q2b sign-off) put the auto-proposer
//! on the inline-at-turn-boundary firing path. The
//! implementation guarantee is that the user **never waits
//! for the auto-proposer** — the daemon's turn driver calls
//! [`spawn_auto_proposer_task`] **after** the
//! `TurnOutcome` finalize has already been forwarded to the
//! channel. The spawn is detached; whether the proposer
//! finishes is independent of when the next turn begins.
//!
//! ## Task 4 vs. Task 5 vs. Task 6 split
//!
//! Task 4 (this file) ships the **orchestration and failure-
//! isolation**. The verdict-routing decision logic (threshold-
//! gate auto-accept vs. staged proposal) lands in Task 5; the
//! audit-event emission lands in Task 6. The
//! [`SkillProposerOutcome::Verdict`] variant carries the raw
//! [`JudgeResponse`] so Tasks 5/6 can act on it without
//! re-running the call.

use std::sync::Arc;

// Re-export types the daemon turn-driver constructs at the post-finalize
// hook (Task 7 wiring) so callers can use the path
// `crate::skill_auto_proposer::TurnSignals` without depending on aivyx-core
// directly. Phase 112 keeps the auto-proposer's caller-facing surface
// homed in aivyx-channel.
pub use aivyx_core::skill_proposer::{
    ExistingPersonaSnapshot, ExistingSkillSnapshot, FailureHeuristicConfig,
    FailureKind, HeuristicConfig, JudgeError, JudgeRequest, JudgeResponse,
    ProposalSource, ProposedDraft, SkillDraft, TurnSignals,
    is_failure_candidate,
};
use aivyx_core::skill_proposer;
use aivyx_core::CancellationToken;
use aivyx_llm::LlmProvider;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Runtime configuration for the skill auto-proposer. Task 5
/// promotes this struct to `aivyx-config` and wires the TOML
/// `[skills.auto_propose]` section; Task 4 lands the shape in
/// `aivyx-channel` so the orchestration is testable without
/// the TOML round-trip.
///
/// The operator picked the more autonomous shape at sign-off
/// (Q2b inline + Q3b auto-accept + Q4b LLM dedup), so the
/// defaults below reflect "this should actually fire" rather
/// than "opt-in-only."
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillAutoProposeConfig {
    /// Master switch. Default `true` per Q3b — the operator
    /// opted into the auto-accept trust window deliberately,
    /// so the phase doesn't bury the feature behind a
    /// disabled-by-default flag.
    pub enabled: bool,

    /// Heuristic thresholds for the cheap-gate stage.
    pub heuristic: HeuristicConfig,

    /// LLM-judge model identifier. Same format as the
    /// operator's main `model` field; defaults to the
    /// fast-and-cheap end of the provider's lineup since the
    /// judge is a one-shot structured-output call.
    pub judge_model: String,

    /// Max tokens the judge may emit on one call. Defaults to
    /// 800 — generous for a full SkillDraft + reasoning, short
    /// enough to keep cost bounded.
    pub judge_max_tokens: u32,

    /// Phase 113 — confidence threshold for the auto-accept
    /// path. Used as the fallback when
    /// [`Self::per_category`] is `None` (Phase 113 single-
    /// config posture). Phase 114 — when `per_category` is
    /// `Some`, the per-category threshold takes precedence
    /// for the picked category; this field stays as a
    /// last-resort default.
    pub auto_accept_confidence_threshold: f32,

    /// Fuzzy-title-match cutoff for the cheap dedup pre-filter
    /// (Q4b). A candidate with title fuzzy-match similarity
    /// against any existing skill at or above this threshold
    /// is dropped (LearnedSkill category only).
    pub fuzzy_match_threshold: f32,

    /// Phase 114 — per-`PersonaDeltaCategory` overrides
    /// produced by the `[persona.auto_propose]` TOML section.
    /// `None` when the operator only configured the Phase 113
    /// `[skills.auto_propose]` alias (in which case every
    /// category falls back to `auto_accept_confidence_threshold`
    /// AND the LearnedSkill category is the only one
    /// effectively enabled — Phase 113 behavior). `Some`
    /// when `[persona.auto_propose]` is present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_category: Option<PerCategoryConfigSet>,

    /// Phase 115 — master switch for the negative-feedback
    /// path. `false` (default) preserves Phase 114 behavior
    /// (only Completed turns fire the auto-proposer); `true`
    /// turns on the Phase 115 failed-turn path, gated
    /// further by `failure_outcomes`.
    #[serde(default)]
    pub from_failed_turns: bool,

    /// Phase 115 — per-failure-outcome enable flags. Only
    /// consulted when `from_failed_turns == true`. Default
    /// matches `FailureHeuristicConfig::default()`:
    /// Failed=true, TimedOut=true, Cancelled=false,
    /// Escalated=false.
    #[serde(default)]
    pub failure_outcomes: FailureHeuristicConfig,

    /// Model routing Part 3a — the `TaskKind` the judge's
    /// request is tagged with, or `None` for an untagged call
    /// (the default). Never read from TOML: the daemon wiring
    /// sets it via [`judge_route_task`] — `Some(Judge)` only
    /// when routing is on AND `judge_model` was left unset.
    #[serde(skip)]
    pub judge_route_task: Option<aivyx_route::TaskKind>,
}

/// Model routing Part 3a — the route tag for the skill
/// auto-proposer's judge. Tagged `Judge` only when routing is
/// on and the operator left `judge_model` unset (so it was
/// defaulted to the planner's model); an explicit
/// `judge_model` stays an untagged pin, and with routing off
/// every request is untagged.
pub fn judge_route_task(
    routing_on: bool,
    judge_model_was_empty: bool,
) -> Option<aivyx_route::TaskKind> {
    (routing_on && judge_model_was_empty).then_some(aivyx_route::TaskKind::Judge)
}

impl Default for SkillAutoProposeConfig {
    fn default() -> Self {
        SkillAutoProposeConfig {
            enabled: true,
            heuristic: HeuristicConfig::default(),
            // Empty = follow the planner's configured model
            // (resolved at daemon wiring). A concrete foreign-model
            // default 404s on any non-Anthropic install.
            judge_model: String::new(),
            judge_max_tokens: 800,
            auto_accept_confidence_threshold: 0.85,
            fuzzy_match_threshold: 0.80,
            per_category: None,
            from_failed_turns: false,
            failure_outcomes: FailureHeuristicConfig::default(),
            judge_route_task: None,
        }
    }
}

/// Phase 114 — runtime per-category override set. Mirrors
/// `aivyx_config::PerCategoryConfigSet` field-for-field so
/// the `From` conversion is mechanical.
///
/// Phase 118 — `profile_hint` + `role_definition_suggestion`
/// fields mirror the TOML config. Their
/// `auto_accept_confidence_threshold` is semantically dead at
/// runtime ([`decide_routing`] hard-codes a Staged outcome for
/// these two categories per the always-staged Q2(a) contract);
/// the threshold stays on the struct for type-shape consistency.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PerCategoryConfigSet {
    pub assistant_name: PerCategoryConfig,
    pub operator_profile: PerCategoryConfig,
    pub communication_style: PerCategoryConfig,
    pub primary_use_cases: PerCategoryConfig,
    pub behavioral_preferences: PerCategoryConfig,
    pub behavioral_constraints: PerCategoryConfig,
    pub learned_context: PerCategoryConfig,
    pub communication_adaptations: PerCategoryConfig,
    pub character_traits: PerCategoryConfig,
    pub relationship_milestones: PerCategoryConfig,
    pub learned_skill: PerCategoryConfig,
    /// Phase 118 — `ProfileHint` per-category override.
    /// `auto_accept_confidence_threshold` is ignored at
    /// routing time (always-staged). Default-enabled.
    #[serde(default = "default_phase_118_enabled_list")]
    pub profile_hint: PerCategoryConfig,
    /// Phase 118 — `RoleDefinitionSuggestion` per-category
    /// override. Same notes as `profile_hint`.
    #[serde(default = "default_phase_118_enabled_list")]
    pub role_definition_suggestion: PerCategoryConfig,
}

fn default_phase_118_enabled_list() -> PerCategoryConfig {
    PerCategoryConfig {
        enabled: true,
        auto_accept_confidence_threshold: 0.85,
    }
}

impl PerCategoryConfigSet {
    /// Lookup the per-category override for a given
    /// `PersonaDeltaCategory` label. Returns `None` for
    /// unknown labels.
    pub fn lookup(&self, category: &str) -> Option<&PerCategoryConfig> {
        match category {
            "AssistantName" => Some(&self.assistant_name),
            "OperatorProfile" => Some(&self.operator_profile),
            "CommunicationStyle" => Some(&self.communication_style),
            "PrimaryUseCases" => Some(&self.primary_use_cases),
            "BehavioralPreferences" => Some(&self.behavioral_preferences),
            "BehavioralConstraints" => Some(&self.behavioral_constraints),
            "LearnedContext" => Some(&self.learned_context),
            "CommunicationAdaptations" => Some(&self.communication_adaptations),
            "CharacterTraits" => Some(&self.character_traits),
            "RelationshipMilestones" => Some(&self.relationship_milestones),
            "LearnedSkill" => Some(&self.learned_skill),
            // Phase 118 — recognized labels so the unknown-
            // category fail-safe in `decide_routing` doesn't
            // fire on these. The threshold is dead-code at
            // routing time; the enable flag is the operator's
            // disable knob.
            "ProfileHint" => Some(&self.profile_hint),
            "RoleDefinitionSuggestion" => Some(&self.role_definition_suggestion),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PerCategoryConfig {
    pub enabled: bool,
    pub auto_accept_confidence_threshold: f32,
}

/// Phase 113 Task 3 — Convert the TOML-loaded
/// `aivyx_config::SkillAutoProposeConfig` into the runtime
/// `aivyx_channel::skill_auto_proposer::SkillAutoProposeConfig`.
/// Maps field-for-field; the two structs intentionally mirror
/// each other so the binary's only job is to call `.into()`.
///
/// Phase 114 — produces `per_category: None`, which the
/// runtime treats as Phase 113 single-config posture
/// (LearnedSkill only).
impl From<aivyx_config::SkillAutoProposeConfig> for SkillAutoProposeConfig {
    fn from(c: aivyx_config::SkillAutoProposeConfig) -> Self {
        SkillAutoProposeConfig {
            enabled: c.enabled,
            heuristic: convert_heuristic(&c.heuristic),
            // `None` (operator didn't pin a model) becomes "" here;
            // the daemon wiring resolves "" to the planner's
            // configured model before the context is built.
            judge_model: c.judge_model.unwrap_or_default(),
            judge_max_tokens: c.judge_max_tokens,
            auto_accept_confidence_threshold: c.auto_accept_confidence_threshold,
            fuzzy_match_threshold: c.fuzzy_match_threshold,
            per_category: None,
            // Phase 115 — Phase 113 alias config has no
            // failure-feedback fields; default to off.
            from_failed_turns: false,
            failure_outcomes: FailureHeuristicConfig::default(),
            judge_route_task: None,
        }
    }
}

/// Phase 114 Task 3 — Convert the TOML-loaded
/// `aivyx_config::PersonaAutoProposeConfig` (per-category)
/// into the runtime `SkillAutoProposeConfig`. The runtime
/// type is shared between Phase 113 (no per_category) and
/// Phase 114 (Some(per_category)).
impl From<aivyx_config::PersonaAutoProposeConfig> for SkillAutoProposeConfig {
    fn from(c: aivyx_config::PersonaAutoProposeConfig) -> Self {
        SkillAutoProposeConfig {
            enabled: c.enabled,
            heuristic: convert_heuristic(&c.heuristic),
            // `None` (operator didn't pin a model) becomes "" here;
            // the daemon wiring resolves "" to the planner's
            // configured model before the context is built.
            judge_model: c.judge_model.unwrap_or_default(),
            judge_max_tokens: c.judge_max_tokens,
            // Phase 114 — `PersonaAutoProposeConfig` has no
            // top-level auto_accept_confidence_threshold; the
            // per-category settings cover the auto-accept
            // policy. This field stays as a sane fallback for
            // categories the operator didn't enumerate (which
            // shouldn't happen — the struct enumerates all 11
            // — but the runtime check defends against future
            // labels too).
            auto_accept_confidence_threshold:
                aivyx_config::DEFAULT_SKILLS_AUTO_PROPOSE_AUTO_ACCEPT_THRESHOLD,
            fuzzy_match_threshold: c.fuzzy_match_threshold,
            per_category: Some(convert_per_category_set(c.per_category)),
            // Phase 115 — failure-feedback fields plumbed
            // from aivyx-config.
            from_failed_turns: c.from_failed_turns,
            failure_outcomes: FailureHeuristicConfig {
                failed: c.failure_outcomes.failed,
                cancelled: c.failure_outcomes.cancelled,
                timed_out: c.failure_outcomes.timed_out,
                escalated: c.failure_outcomes.escalated,
            },
            judge_route_task: None,
        }
    }
}

fn convert_heuristic(
    h: &aivyx_config::SkillsAutoProposeHeuristic,
) -> HeuristicConfig {
    HeuristicConfig {
        tool_call_count_min: h.tool_call_count_min,
        distinct_tool_id_min: h.distinct_tool_id_min,
        duration_ms_min: h.duration_ms_min,
        require_gate_resolve: h.require_gate_resolve,
        mode: match h.mode {
            aivyx_config::SkillsAutoProposeMatchMode::Any =>
                aivyx_core::skill_proposer::MatchMode::Any,
            aivyx_config::SkillsAutoProposeMatchMode::All =>
                aivyx_core::skill_proposer::MatchMode::All,
        },
        // Phase 118 — Task 3 ships the heuristic substrate;
        // the TOML wiring of these two Phase 118 thresholds
        // is Task 6's job. Until then the runtime falls back
        // to the `HeuristicConfig::Default` values.
        ..HeuristicConfig::default()
    }
}

fn convert_per_category_set(
    c: aivyx_config::PerCategoryConfigSet,
) -> PerCategoryConfigSet {
    fn cv(p: aivyx_config::PerCategoryConfig) -> PerCategoryConfig {
        PerCategoryConfig {
            enabled: p.enabled,
            auto_accept_confidence_threshold: p.auto_accept_confidence_threshold,
        }
    }
    PerCategoryConfigSet {
        assistant_name: cv(c.assistant_name),
        operator_profile: cv(c.operator_profile),
        communication_style: cv(c.communication_style),
        primary_use_cases: cv(c.primary_use_cases),
        behavioral_preferences: cv(c.behavioral_preferences),
        behavioral_constraints: cv(c.behavioral_constraints),
        learned_context: cv(c.learned_context),
        communication_adaptations: cv(c.communication_adaptations),
        character_traits: cv(c.character_traits),
        relationship_milestones: cv(c.relationship_milestones),
        learned_skill: cv(c.learned_skill),
        // Phase 118 — pass-through; threshold field carried
        // for type-shape consistency but ignored at routing.
        profile_hint: cv(c.profile_hint),
        role_definition_suggestion: cv(c.role_definition_suggestion),
    }
}

// ---------------------------------------------------------------------------
// Outcome
// ---------------------------------------------------------------------------

/// What happened when the auto-proposer ran. Pure outcome
/// enum — no `Result<>` wrapping because the caller treats
/// every variant as "the proposer is done; move on."
#[derive(Debug, Clone)]
pub enum SkillProposerOutcome {
    /// Master switch is off (`config.enabled == false`).
    Disabled,

    /// Heuristic gate rejected the turn. Most common variant;
    /// chit-chat turns and single-tool quick turns end here.
    HeuristicGated,

    /// Judge call ran successfully. The verdict is in the
    /// `JudgeResponse`. Task 5 reads this variant and routes
    /// to either auto-accept or staged.
    Verdict(JudgeResponse),

    /// Judge call failed (LLM error, parse failure, or
    /// confidence out-of-range). Carries the error message
    /// for the audit log (Task 6) and operator inspection.
    /// **Always logged at WARN level by the spawn wrapper.**
    JudgeError(String),
}

impl SkillProposerOutcome {
    /// Short stable label for the outcome — used by the
    /// audit log (Task 6) and by the spawn wrapper's WARN
    /// log line.
    pub fn label(&self) -> &'static str {
        match self {
            SkillProposerOutcome::Disabled => "disabled",
            SkillProposerOutcome::HeuristicGated => "heuristic-gated",
            SkillProposerOutcome::Verdict(_) => "verdict",
            SkillProposerOutcome::JudgeError(_) => "judge-error",
        }
    }
}

// ---------------------------------------------------------------------------
// Task 5 — Decision routing
// ---------------------------------------------------------------------------

/// The terminal decision the auto-proposer reaches after the
/// LLM judge has returned a [`JudgeResponse`]. One of four
/// outcomes:
///
/// - `AutoAccept`: confidence >= threshold, no LLM-judged dup,
///   no fuzzy-title-clash → land in the LearnedSkill chain
///   directly as an approved entry tagged `auto_accepted: true`.
/// - `Staged`: worth-proposing but below confidence
///   threshold → land in the proposal chain as Pending so the
///   operator can review through `aivyx-pa persona proposals
///   approve` (the same surface manual proposals use).
/// - `DroppedJudgeDup`: the judge declared this candidate a
///   semantic duplicate of an existing skill → nothing
///   written, just logged for audit (Q4b LLM semantic check).
/// - `DroppedFuzzyDup`: the cheap title fuzzy-match pre-
///   filter caught this candidate before the judge even fired
///   → nothing written; cost-efficient dedup (Q4b fuzzy-
///   match pre-filter).
/// - `DroppedNotWorthProposing`: the judge said
///   `is_worth_proposing = false` without naming a dup →
///   nothing written.
#[derive(Debug, Clone, PartialEq)]
pub enum SkillRoutingDecision {
    /// Auto-accept the draft. Phase 114 — `category` and
    /// polymorphic `draft` replace the Phase 113 `SkillDraft`-
    /// only shape.
    AutoAccept {
        category: String,
        draft: ProposedDraft,
        confidence: f32,
    },
    /// Stage the draft for operator approval.
    Staged {
        category: String,
        draft: ProposedDraft,
        confidence: f32,
    },
    DroppedJudgeDup {
        duplicate_of: String,
    },
    DroppedFuzzyDup {
        matched_existing_name: String,
    },
    DroppedNotWorthProposing,
    /// Phase 114 — judge picked a `PersonaDeltaCategory` the
    /// operator disabled in `[persona.auto_propose.<category>]`.
    /// No chain write; logged as a distinct audit outcome so
    /// operators can audit "the auto-proposer wanted to write
    /// X but I'd disabled X."
    DroppedCategoryDisabled {
        category: String,
    },
}

impl SkillRoutingDecision {
    /// Short stable label for the audit log and operator
    /// forensics.
    pub fn label(&self) -> &'static str {
        match self {
            SkillRoutingDecision::AutoAccept { .. } => "auto-accept",
            SkillRoutingDecision::Staged { .. } => "staged",
            SkillRoutingDecision::DroppedJudgeDup { .. } => "dup-dropped-llm",
            SkillRoutingDecision::DroppedFuzzyDup { .. } => "dup-dropped-fuzzy",
            SkillRoutingDecision::DroppedNotWorthProposing => {
                "not-worth-proposing"
            }
            SkillRoutingDecision::DroppedCategoryDisabled { .. } => {
                "category-disabled"
            }
        }
    }
}

/// Apply the threshold-gated routing rules (Q3b) on top of the
/// judge's verdict, with the Q4b fuzzy-match pre-filter
/// running first (cheap dedup catches obvious title-dups
/// before any further work).
///
/// **The pre-filter runs against `verdict.proposed_skill()`'s
/// title, not the original turn summary.** The judge has
/// already drafted a candidate skill at this point; we check
/// if its title fuzzy-matches an existing skill *as a final
/// safety net* (the judge may have missed an obvious dup the
/// fuzzy-match would catch).
///
/// The function is pure — same inputs → same decision. Caller
/// is responsible for actually writing the chain entries on
/// `AutoAccept` and `Staged` outcomes; this just decides which
/// path is right.
pub fn decide_routing(
    verdict: &JudgeResponse,
    existing_skills: &[ExistingSkillSnapshot],
    config: &SkillAutoProposeConfig,
) -> SkillRoutingDecision {
    // Step 1 — Judge said dup, drop immediately. The judge's
    // semantic check beats the fuzzy-match: if the judge saw
    // a dup, we trust it.
    if let Some(name) = &verdict.is_duplicate_of {
        return SkillRoutingDecision::DroppedJudgeDup {
            duplicate_of: name.clone(),
        };
    }

    // Step 2 — Judge said not worth proposing (and not a
    // dup), drop.
    if !verdict.is_worth_proposing {
        return SkillRoutingDecision::DroppedNotWorthProposing;
    }

    // Step 3 — Judge must have committed to a category AND a
    // draft. Phase 114 — these come paired; either missing
    // means the LLM didn't produce a clean verdict, treat as
    // not-worth-proposing rather than ship something broken.
    let Some(category) = verdict.category.as_ref() else {
        return SkillRoutingDecision::DroppedNotWorthProposing;
    };
    let Some(draft) = verdict.proposed_draft.as_ref() else {
        return SkillRoutingDecision::DroppedNotWorthProposing;
    };

    // Phase 114 — per-category enable check. If the operator
    // has the picked category disabled, drop with a distinct
    // outcome so audit forensics can show "the auto-proposer
    // wanted to write category X but the operator disabled
    // X."
    if let Some(pc) = config.per_category.as_ref() {
        match pc.lookup(category) {
            Some(per_cat) if !per_cat.enabled => {
                return SkillRoutingDecision::DroppedCategoryDisabled {
                    category: category.clone(),
                };
            }
            None => {
                // Unknown category label (the judge picked
                // something not in our enumeration). Defensive
                // — treat as disabled.
                return SkillRoutingDecision::DroppedCategoryDisabled {
                    category: category.clone(),
                };
            }
            Some(_) => {} // enabled — fall through
        }
    }

    // Step 4 — Fuzzy-title-match pre-filter for the
    // LearnedSkill category only. List + scalar categories
    // don't use fuzzy match (their values aren't kebab-case
    // titles); the judge's `is_duplicate_of` handled
    // cross-category dedup at step 1.
    if let ProposedDraft::LearnedSkill { name, .. } = draft {
        if let Some(matched) = fuzzy_match_against_existing(
            name,
            existing_skills,
            config.fuzzy_match_threshold,
        ) {
            return SkillRoutingDecision::DroppedFuzzyDup {
                matched_existing_name: matched,
            };
        }
    }

    // Phase 118 — always-staged override for the two
    // operator-staged refinement categories. The P13 (Profile
    // is operator-declared) and P9 (Role config is operator-
    // curated) contracts make auto-accept on these categories
    // a contract violation regardless of judge confidence.
    // Routing forces Staged; the threshold gate below is
    // skipped. The override is hard-coded at the category
    // level (not operator-configurable) — Q2(a) at Phase 118
    // sign-off. Operators who want to disable proposing these
    // categories entirely use the per-category `enabled`
    // flag, which has already fired above.
    if is_always_staged_category(category) {
        return SkillRoutingDecision::Staged {
            category: category.clone(),
            draft: draft.clone(),
            confidence: verdict.confidence,
        };
    }

    // Step 5 — Threshold gate. Use the per-category threshold
    // when configured; fall back to the top-level
    // `auto_accept_confidence_threshold` for Phase 113-alias
    // configs (per_category = None).
    let threshold = config
        .per_category
        .as_ref()
        .and_then(|pc| pc.lookup(category))
        .map(|pc| pc.auto_accept_confidence_threshold)
        .unwrap_or(config.auto_accept_confidence_threshold);

    if verdict.confidence >= threshold {
        SkillRoutingDecision::AutoAccept {
            category: category.clone(),
            draft: draft.clone(),
            confidence: verdict.confidence,
        }
    } else {
        SkillRoutingDecision::Staged {
            category: category.clone(),
            draft: draft.clone(),
            confidence: verdict.confidence,
        }
    }
}

/// Phase 118 — `true` for category labels whose routing is
/// hard-coded to `Staged` regardless of judge confidence.
/// Pure function; the contract is the source of truth (P13 +
/// P9), not operator policy.
///
/// Used by [`decide_routing`] to short-circuit the threshold
/// gate for the two operator-staged refinement categories.
pub fn is_always_staged_category(category: &str) -> bool {
    matches!(category, "ProfileHint" | "RoleDefinitionSuggestion")
}

// ---------------------------------------------------------------------------
// Task 5 — Fuzzy title-match pre-filter (Q4b cheap dedup)
// ---------------------------------------------------------------------------

/// Compute a normalized title similarity in `[0.0, 1.0]`
/// between two skill names.
///
/// Phase 120 Task 2 — lifted into
/// [`aivyx_core::skill_proposer::title_similarity`] so the
/// Phase 120 planner-side tool-name recovery path can share
/// the same primitive. Re-exported here so every existing
/// Phase 112 caller and in-module test keeps the same path.
pub use aivyx_core::skill_proposer::title_similarity;

/// Return the name of the first existing skill whose title
/// similarity against `candidate_title` meets or exceeds
/// `threshold`. Used as the Q4b cheap dedup pre-filter.
pub fn fuzzy_match_against_existing(
    candidate_title: &str,
    existing: &[ExistingSkillSnapshot],
    threshold: f32,
) -> Option<String> {
    existing
        .iter()
        .find(|s| title_similarity(candidate_title, &s.name) >= threshold)
        .map(|s| s.name.clone())
}

// ---------------------------------------------------------------------------
// Task 6 — Audit-event construction helpers
// ---------------------------------------------------------------------------

/// Compute which heuristic signals (Q1b stage 1) crossed
/// their thresholds for a given `TurnSignals` + `HeuristicConfig`
/// pair. The boolean record is what the audit event carries.
///
/// **Always reports the actual signal crossings**, regardless
/// of the `MatchMode`. Forensic queries care about "which
/// signals crossed?", not "did the combined gate fire?" —
/// the gate-firing question is implicit in the outcome
/// summary itself (HeuristicGated vs. anything past the
/// judge).
pub fn signals_matched(
    signals: &TurnSignals,
    config: &HeuristicConfig,
) -> aivyx_audit::HeuristicSignalsMatched {
    aivyx_audit::HeuristicSignalsMatched {
        tool_call_count: signals.tool_calls_made >= config.tool_call_count_min,
        distinct_tool_id_count: signals.distinct_tool_id_count
            >= config.distinct_tool_id_min,
        duration: signals.duration.as_millis() as u64
            >= config.duration_ms_min,
        gate_resolve: signals.had_successful_gate_resolve,
        // Phase 118 — Profile/Role auto-proposer signals.
        // Reported truthfully regardless of MatchMode (audit
        // forensics care about WHICH signals crossed, not
        // which crossings the combined gate consumed).
        profile_pattern_repeated: signals.keyword_key_prior_total_count
            >= config.profile_pattern_recurrence_min,
        role_shape_recurring: signals.recent_scope_denied_count
            >= config.role_shape_scope_denied_min,
    }
}

/// Convert a `SkillProposerOutcome` (+ the routing decision
/// for Verdict outcomes) into the audit-chain summary enum.
/// The Verdict→AutoAccepted/Staged/Dup/NotWorth path requires
/// the routing decision; the other proposer outcomes map 1-1.
///
/// Returns the outcome variant + the (proposed_skill_name,
/// confidence_thousandths) pair that the audit event needs.
/// `judge_latency_ms` is provided by the caller (it's measured
/// at the call site, not here).
pub fn audit_outcome_from(
    proposer_outcome: &SkillProposerOutcome,
    routing: Option<&SkillRoutingDecision>,
) -> (
    aivyx_audit::SkillAutoProposalOutcomeSummary,
    Option<String>,
    Option<u32>,
) {
    use aivyx_audit::SkillAutoProposalOutcomeSummary as S;

    match proposer_outcome {
        SkillProposerOutcome::Disabled => (S::Disabled, None, None),
        SkillProposerOutcome::HeuristicGated => (S::HeuristicGated, None, None),
        SkillProposerOutcome::JudgeError(msg) => (
            S::JudgeError {
                error_message: msg.clone(),
            },
            None,
            None,
        ),
        SkillProposerOutcome::Verdict(verdict) => {
            // Use the routing decision if provided; otherwise
            // fall back to deriving from the verdict alone.
            // (The caller should always supply the routing.)
            let routing_owned;
            let routing = match routing {
                Some(r) => r,
                None => {
                    // Build a default routing decision from the
                    // verdict using empty config defaults. This
                    // shouldn't fire in production paths — Task 7
                    // always passes a routing — but it keeps the
                    // function total.
                    routing_owned = decide_routing(
                        verdict,
                        &[],
                        &SkillAutoProposeConfig::default(),
                    );
                    &routing_owned
                }
            };
            let confidence_thousandths =
                Some((verdict.confidence * 1000.0).round() as u32);
            let draft_display = verdict
                .proposed_draft
                .as_ref()
                .map(|d| d.display_name());
            match routing {
                SkillRoutingDecision::AutoAccept { draft, .. } => (
                    S::AutoAccepted,
                    Some(draft.display_name()),
                    confidence_thousandths,
                ),
                SkillRoutingDecision::Staged { draft, .. } => (
                    S::Staged,
                    Some(draft.display_name()),
                    confidence_thousandths,
                ),
                SkillRoutingDecision::DroppedJudgeDup { duplicate_of } => (
                    S::DuplicateOfExistingLlm {
                        duplicate_of: duplicate_of.clone(),
                    },
                    draft_display,
                    confidence_thousandths,
                ),
                SkillRoutingDecision::DroppedFuzzyDup {
                    matched_existing_name,
                } => (
                    S::DuplicateOfExistingFuzzy {
                        matched_existing_name: matched_existing_name.clone(),
                    },
                    draft_display,
                    confidence_thousandths,
                ),
                SkillRoutingDecision::DroppedNotWorthProposing => (
                    S::NotWorthProposing,
                    draft_display,
                    confidence_thousandths,
                ),
                SkillRoutingDecision::DroppedCategoryDisabled {
                    category,
                } => (
                    // Phase 114 — reuses the
                    // NotWorthProposing outcome with the
                    // category label in the
                    // proposed_skill_name slot so audit
                    // forensics can surface "the auto-
                    // proposer wanted to write category X
                    // but I'd disabled X." A dedicated
                    // audit variant is a Task 5 follow-on
                    // (would require an AuditEvent
                    // extension; Phase 114 keeps the chain
                    // shape conservative).
                    S::NotWorthProposing,
                    Some(format!("(category-disabled: {category})")),
                    confidence_thousandths,
                ),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Run the heuristic gate + LLM judge for one just-finalized
/// turn. Never propagates an error; failure modes are encoded
/// in [`SkillProposerOutcome`].
///
/// The caller assembles `signals` from `TurnOutcome` + the
/// turn's audit-log entries, builds `turn_summary` from the
/// user input + final reply + tool-call narrative, and
/// snapshots `existing_skills` from the Persona chain.
pub async fn auto_propose_for_turn(
    provider: Arc<dyn LlmProvider>,
    config: &SkillAutoProposeConfig,
    signals: TurnSignals,
    turn_summary: String,
    existing_persona: ExistingPersonaSnapshot,
    cancellation: &CancellationToken,
) -> SkillProposerOutcome {
    // Phase 114-compat entry: defaults to CompletedTurn
    // source. Phase 115 callers use
    // `auto_propose_for_turn_with_source` to pass an
    // explicit `ProposalSource::FailedTurn { .. }`.
    auto_propose_for_turn_with_source(
        provider,
        config,
        signals,
        turn_summary,
        existing_persona,
        ProposalSource::CompletedTurn,
        cancellation,
    )
    .await
}

/// Phase 115 — same as `auto_propose_for_turn` but takes an
/// explicit `ProposalSource` so the failure-feedback path
/// can pass `FailedTurn { .. }`. The Phase 114-compat
/// wrapper above hard-codes `CompletedTurn` for existing
/// callers.
#[allow(clippy::too_many_arguments)]
pub async fn auto_propose_for_turn_with_source(
    provider: Arc<dyn LlmProvider>,
    config: &SkillAutoProposeConfig,
    signals: TurnSignals,
    turn_summary: String,
    existing_persona: ExistingPersonaSnapshot,
    source: ProposalSource,
    cancellation: &CancellationToken,
) -> SkillProposerOutcome {
    if !config.enabled {
        return SkillProposerOutcome::Disabled;
    }

    // Heuristic gate: Phase 114 path uses TurnSignals + the
    // four-signal gate; Phase 115 failed-turn path skips
    // the signals gate (the daemon side already checked
    // `is_failure_candidate`).
    let pass_heuristic = match source {
        ProposalSource::CompletedTurn => {
            skill_proposer::is_candidate(&signals, &config.heuristic)
        }
        ProposalSource::FailedTurn { .. } => true,
    };
    if !pass_heuristic {
        return SkillProposerOutcome::HeuristicGated;
    }

    let request = JudgeRequest {
        turn_summary: &turn_summary,
        existing_persona: &existing_persona,
        model: &config.judge_model,
        max_tokens: config.judge_max_tokens,
        source,
        route_task: config.judge_route_task.clone(),
    };

    match skill_proposer::judge(provider, request, cancellation).await {
        Ok(response) => SkillProposerOutcome::Verdict(response),
        Err(JudgeError::Provider(e)) => {
            SkillProposerOutcome::JudgeError(format!("provider: {e}"))
        }
        Err(JudgeError::ParseFailure { raw }) => {
            SkillProposerOutcome::JudgeError(format!(
                "parse: {} chars unparseable",
                raw.len()
            ))
        }
        Err(JudgeError::ConfidenceOutOfRange(c)) => {
            SkillProposerOutcome::JudgeError(format!(
                "confidence out of range: {c}"
            ))
        }
    }
}

/// Detached-spawn wrapper for the post-`finalize` hook in the
/// daemon turn-driver. Calls [`auto_propose_for_turn`] inside
/// a `tokio::spawn`, logs the outcome label at the
/// appropriate level, and drops. The turn driver does NOT
/// await the returned `JoinHandle` — the auto-proposer runs
/// independently of subsequent turns.
///
/// The `tokio::JoinHandle` is returned so callers (test code,
/// shutdown-drain code) can `.await` if they want to know when
/// the proposer is done. The daemon turn-driver discards it.
pub fn spawn_auto_proposer_task(
    provider: Arc<dyn LlmProvider>,
    config: SkillAutoProposeConfig,
    signals: TurnSignals,
    turn_summary: String,
    existing_persona: ExistingPersonaSnapshot,
    cancellation: CancellationToken,
) -> tokio::task::JoinHandle<SkillProposerOutcome> {
    tokio::spawn(async move {
        let outcome = auto_propose_for_turn(
            provider,
            &config,
            signals,
            turn_summary,
            existing_persona,
            &cancellation,
        )
        .await;
        // Q2b failure-isolation contract: WARN on the
        // failure label so the operator notices a sustained
        // streak of judge errors, but never panic / never
        // propagate. The audit-event emission in Task 6 will
        // give the operator the full forensic surface.
        match &outcome {
            SkillProposerOutcome::JudgeError(msg) => {
                eprintln!("aivyx-pa skill-auto-proposer: judge-error ({msg})");
            }
            SkillProposerOutcome::Disabled
            | SkillProposerOutcome::HeuristicGated
            | SkillProposerOutcome::Verdict(_) => {}
        }
        outcome
    })
}

// ---------------------------------------------------------------------------
// Task 7 — Unified pipeline (heuristic → judge → routing → chain writes →
// audit event), suitable for spawn-from-daemon at the post-finalize hook.
// ---------------------------------------------------------------------------

/// Dependency bundle the daemon hands the auto-proposer at startup.
/// Daemon-side wires `Some(...)` into `DaemonConfig::skill_auto_proposer`
/// when the operator has the feature enabled.
pub struct SkillAutoProposerContext {
    pub config: SkillAutoProposeConfig,
    pub llm_provider: Arc<dyn aivyx_llm::LlmProvider>,
}

impl std::fmt::Debug for SkillAutoProposerContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SkillAutoProposerContext")
            .field("config", &self.config)
            .field("llm_provider", &"<dyn LlmProvider>")
            .finish()
    }
}

/// Phase 114 — snapshot the full effective Persona state
/// into the form the judge needs for cross-category dedup.
/// Reads under the read lock and copies every category so
/// the judge call doesn't hold the lock.
///
/// Malformed `LearnedSkill` JSON entries are skipped (same
/// posture the renderer takes — Phase 110 Q3c precedent).
pub fn snapshot_existing_persona(
    shared: &crate::persona::SharedEffectivePersona,
) -> ExistingPersonaSnapshot {
    let Ok(state) = shared.read() else {
        return ExistingPersonaSnapshot::default();
    };
    let learned_skills = state
        .learned_skills
        .iter()
        .filter_map(|s| crate::persona::LearnedSkill::from_json_value(s))
        .map(|sk| {
            let summary: String = sk.procedure.chars().take(200).collect();
            ExistingSkillSnapshot {
                name: sk.name,
                trigger: sk.trigger,
                procedure_summary: summary,
            }
        })
        .collect();
    ExistingPersonaSnapshot {
        assistant_name: state.assistant_name.clone(),
        operator_profile: state.operator_profile.clone(),
        communication_style: state.communication_style.clone(),
        primary_use_cases: state.primary_use_cases.clone(),
        behavioral_preferences: state.behavioral_preferences.clone(),
        behavioral_constraints: state.behavioral_constraints.clone(),
        learned_context: state.learned_context.clone(),
        communication_adaptations: state.communication_adaptations.clone(),
        character_traits: state.character_traits.clone(),
        relationship_milestones: state.relationship_milestones.clone(),
        learned_skills,
        // Phase 118 — JSON-serialized payloads pass through
        // verbatim; the judge prompt parses them at render
        // time for the cross-category dedup summary.
        profile_hints: state.profile_hints.clone(),
        role_drafts: state.role_drafts.clone(),
    }
}

/// Phase 113 backwards-compat alias. Returns just the
/// `learned_skills` portion of the full Persona snapshot —
/// callers that only need the skill list (e.g. `decide_routing`'s
/// fuzzy-match input) can use this without changing their code
/// after the Phase 114 generalization. Internally delegates to
/// [`snapshot_existing_persona`].
pub fn snapshot_existing_skills(
    shared: &crate::persona::SharedEffectivePersona,
) -> Vec<ExistingSkillSnapshot> {
    snapshot_existing_persona(shared).learned_skills
}

/// Compose a turn summary string from the user input and the
/// agent's final reply (or an outcome-shaped placeholder for
/// non-Completed terminals). Keep it short enough that the
/// judge prompt stays operator-budget-shaped (~500 tokens
/// budget; this contributes ~half).
pub fn build_turn_summary(
    user_text: &str,
    outcome: &aivyx_core::TurnOutcome,
) -> String {
    use aivyx_core::TurnOutcome;
    let mut s = String::new();
    s.push_str("User said: ");
    let user_excerpt: String = user_text.chars().take(400).collect();
    s.push_str(&user_excerpt);
    if user_text.chars().count() > 400 {
        s.push_str(" […]");
    }
    s.push_str("\n\nAgent ");
    match outcome {
        TurnOutcome::Completed {
            final_message,
            tool_calls_made,
            duration,
        } => {
            s.push_str(&format!(
                "completed in {}ms with {} tool call(s).\nReply: ",
                duration.as_millis(),
                tool_calls_made
            ));
            let reply_excerpt: String =
                final_message.chars().take(400).collect();
            s.push_str(&reply_excerpt);
            if final_message.chars().count() > 400 {
                s.push_str(" […]");
            }
        }
        TurnOutcome::Escalated { reason, .. } => {
            s.push_str(&format!("escalated: {reason}"));
        }
        TurnOutcome::TimedOut { elapsed, .. } => {
            s.push_str(&format!(
                "timed out after {}ms",
                elapsed.as_millis()
            ));
        }
        TurnOutcome::Cancelled { .. } => {
            s.push_str("was cancelled");
        }
        TurnOutcome::MaxStepsExceeded { max_steps, .. } => {
            s.push_str(&format!("hit max_steps={max_steps} (runaway planner)"));
        }
        TurnOutcome::Looping { repeat_limit, .. } => {
            s.push_str(&format!(
                "stopped after repeat_limit={repeat_limit} identical tool calls (runaway loop)"
            ));
        }
        TurnOutcome::Failed(_) => {
            s.push_str("failed");
        }
    }
    s
}

/// Full auto-propose pipeline. Spawn this from the daemon's
/// post-finalize hook. Runs heuristic gate → judge call →
/// routing decision → chain writes (for AutoAccept/Staged) →
/// audit-event emission, in order. Each step is failure-
/// isolated; any error path collapses to a `JudgeError` audit
/// event and returns cleanly.
///
/// The pipeline is **inline-at-turn-boundary** per Q2b but
/// runs **after the user has already received the turn
/// reply** so its latency cost is invisible. Critical-path
/// independence is the caller's responsibility (`tokio::spawn`
/// from after the finalize event has been forwarded).
#[allow(clippy::too_many_arguments)]
pub async fn run_auto_propose_pipeline(
    proposer_ctx: &SkillAutoProposerContext,
    audit_log: Option<&Arc<aivyx_audit::PersistentAuditLog>>,
    persona_log: Option<&Arc<crate::persona::PersistentPersonaLog>>,
    persona_proposal_log: Option<
        &Arc<crate::persona_proposal::PersistentPersonaProposalLog>,
    >,
    shared_persona: &crate::persona::SharedEffectivePersona,
    session_id: aivyx_core::SessionId,
    signals: TurnSignals,
    turn_summary: String,
    cancellation: &CancellationToken,
) {
    // Phase 114-compat: defaults to CompletedTurn source.
    run_auto_propose_pipeline_with_source(
        proposer_ctx,
        audit_log,
        persona_log,
        persona_proposal_log,
        shared_persona,
        session_id,
        signals,
        turn_summary,
        ProposalSource::CompletedTurn,
        cancellation,
    )
    .await
}

/// Phase 115 — same as `run_auto_propose_pipeline` but takes
/// an explicit `ProposalSource`. The daemon's broadened
/// post-finalize hook uses this when firing the failure-
/// feedback path; existing Phase 114 callers stay on the
/// `_with_source = CompletedTurn` default through the
/// backward-compat wrapper above.
#[allow(clippy::too_many_arguments)]
pub async fn run_auto_propose_pipeline_with_source(
    proposer_ctx: &SkillAutoProposerContext,
    audit_log: Option<&Arc<aivyx_audit::PersistentAuditLog>>,
    persona_log: Option<&Arc<crate::persona::PersistentPersonaLog>>,
    persona_proposal_log: Option<
        &Arc<crate::persona_proposal::PersistentPersonaProposalLog>,
    >,
    shared_persona: &crate::persona::SharedEffectivePersona,
    session_id: aivyx_core::SessionId,
    signals: TurnSignals,
    turn_summary: String,
    source: ProposalSource,
    cancellation: &CancellationToken,
) {
    let signals_record =
        signals_matched(&signals, &proposer_ctx.config.heuristic);
    // Phase 114 — full Persona snapshot. The auto-proposer
    // sees every category; the routing decide-fn extracts
    // just the skill list for fuzzy-match.
    let existing_persona = snapshot_existing_persona(shared_persona);

    // Keep a copy of the source for the audit event below;
    // the call into auto_propose_for_turn_with_source moves
    // its `source` arg into the JudgeRequest.
    let source_for_audit = source.clone();
    // Time the judge call so the audit event carries latency.
    let judge_started = std::time::Instant::now();
    let proposer_outcome = auto_propose_for_turn_with_source(
        Arc::clone(&proposer_ctx.llm_provider),
        &proposer_ctx.config,
        signals,
        turn_summary,
        existing_persona.clone(),
        source,
        cancellation,
    )
    .await;
    let judge_latency_ms = match &proposer_outcome {
        // Only timing is meaningful when the judge actually
        // fired. Disabled / HeuristicGated short-circuit before
        // any LLM work — report None.
        SkillProposerOutcome::Disabled
        | SkillProposerOutcome::HeuristicGated => None,
        _ => Some(judge_started.elapsed().as_millis() as u64),
    };

    // Compute the routing decision (and perform chain writes
    // for AutoAccept/Staged outcomes). `decide_routing`'s
    // fuzzy-match input scoped to the LearnedSkill list per
    // Phase 113; cross-category dedup is handled by the
    // judge's `is_duplicate_of` field directly.
    let routing = match &proposer_outcome {
        SkillProposerOutcome::Verdict(verdict) => Some(decide_routing(
            verdict,
            &existing_persona.learned_skills,
            &proposer_ctx.config,
        )),
        _ => None,
    };

    // Chain writes — best-effort. A failure here is logged but
    // does NOT bubble up; the audit event below still captures
    // the routing decision.
    if let Some(decision) = &routing {
        match decision {
            SkillRoutingDecision::AutoAccept { category, draft, .. } => {
                if let (Some(plog), Some(pp_log)) =
                    (persona_log, persona_proposal_log)
                {
                    let _ = write_auto_accepted_delta(
                        plog,
                        pp_log,
                        shared_persona,
                        category,
                        draft,
                        &session_id,
                    )
                    .await;
                }
            }
            SkillRoutingDecision::Staged { category, draft, .. } => {
                if let Some(pp_log) = persona_proposal_log {
                    let _ = write_staged_delta(
                        pp_log,
                        category,
                        draft,
                        &session_id,
                    )
                    .await;
                }
            }
            _ => {}
        }
    }

    // Emit the audit event. Best-effort.
    let (outcome_summary, proposed_skill_name, confidence_thousandths) =
        audit_outcome_from(&proposer_outcome, routing.as_ref());
    // Phase 114 — the category label the judge picked. For
    // AutoAccept/Staged it comes from the routing decision;
    // for the other variants the routing doesn't carry the
    // category, but the verdict does (when present).
    let category_label = match &routing {
        Some(SkillRoutingDecision::AutoAccept { category, .. })
        | Some(SkillRoutingDecision::Staged { category, .. })
        | Some(SkillRoutingDecision::DroppedCategoryDisabled {
            category, ..
        }) => Some(category.clone()),
        _ => match &proposer_outcome {
            SkillProposerOutcome::Verdict(v) => v.category.clone(),
            _ => None,
        },
    };
    // Phase 115 — convert the runtime ProposalSource into the
    // audit-event ProposalSourceSummary. None for the
    // pre-Phase-115 backward-compat default; Some for explicit
    // FailedTurn entries so audit-export filters can pick them
    // out.
    let source_summary = match &source_for_audit {
        ProposalSource::CompletedTurn => None,
        ProposalSource::FailedTurn { kind, .. } => {
            Some(aivyx_audit::ProposalSourceSummary::FailedTurn {
                failure_kind: kind.label().to_string(),
            })
        }
    };
    if let Some(alog) = audit_log {
        let event = aivyx_audit::AuditEvent::SkillAutoProposal {
            session_id,
            outcome: outcome_summary,
            confidence_thousandths,
            proposed_skill_name,
            judge_latency_ms,
            heuristic_signals_matched: signals_record,
            category: category_label,
            source: source_summary,
        };
        use aivyx_audit::AuditWriter as _;
        if let Err(e) = alog.append(event) {
            eprintln!(
                "aivyx-pa skill-auto-proposer: audit append failed ({e})"
            );
        }
    }
}

/// Phase 114 — chain-write helper for the AutoAccept path,
/// generalized from skill-only to all 11 PersonaDeltaCategory
/// variants. Dispatches on `category` + `draft` to produce
/// the right `PersonaDeltaOp` shape:
///
/// - `LearnedSkill` (list of JSON-serialized skill objects):
///   wraps the draft in a `LearnedSkill` struct, JSON-
///   serializes, emits as `AppendList { value: <json> }`.
/// - List categories (BehavioralPreferences, LearnedContext,
///   etc.): emits `AppendList { value: <plain string> }`.
/// - Scalar categories (AssistantName, OperatorProfile,
///   CommunicationStyle): emits `SetScalar { value:
///   Some(<plain string>) }`.
///
/// Writes the same three-step sequence as the operator-side
/// `aivyx-pa persona proposals approve` flow: Pending append →
/// PersonaDelta append → Approved transition → shared
/// persona recompute.
async fn write_auto_accepted_delta(
    persona_log: &Arc<crate::persona::PersistentPersonaLog>,
    persona_proposal_log: &Arc<
        crate::persona_proposal::PersistentPersonaProposalLog,
    >,
    shared_persona: &crate::persona::SharedEffectivePersona,
    category_label: &str,
    draft: &ProposedDraft,
    session_id: &aivyx_core::SessionId,
) -> Result<(), String> {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    let proposed_op = build_proposed_op(
        category_label,
        draft,
        format!(
            "auto-accepted by persona-auto-proposer (Phase 114) from session {}",
            session_id.0
        ),
    )?;
    let proposal_id = format!("auto-{}-{}", session_id.0, now_ms);

    // Step 1 — pending proposal
    persona_proposal_log
        .append_pending(
            proposal_id.clone(),
            now_ms,
            session_id.0.to_string(),
            proposed_op.clone(),
        )
        .await
        .map_err(|e| format!("pending append: {e}"))?;

    // Step 2 — append the persona delta
    let delta_id = format!("pd-auto-{proposal_id}");
    let delta = crate::persona::PersonaDelta {
        delta_id,
        proposed_at_unix_ms: now_ms,
        approved_at_unix_ms: now_ms,
        proposal_id: proposal_id.clone(),
        category: proposed_op.category,
        op: proposed_op.op.clone(),
    };
    let applied_seq = persona_log
        .append(delta)
        .await
        .map_err(|e| format!("persona append: {e}"))?;

    // Step 3 — approved proposal transition
    persona_proposal_log
        .append_approved(proposal_id, now_ms, proposed_op, applied_seq)
        .await
        .map_err(|e| format!("approved append: {e}"))?;

    // Step 4 — refresh shared effective persona
    let entries_after = persona_log.entries();
    if !crate::persona::recompute_shared_from_entries(
        shared_persona,
        &entries_after,
    ) {
        return Err("shared persona lock poisoned".into());
    }
    Ok(())
}

/// Phase 114 — chain-write helper for the Staged path,
/// generalized over all PersonaDeltaCategory variants. Writes
/// a Pending proposal entry only; the operator resolves
/// through `aivyx-pa persona proposals approve` / `reject`.
async fn write_staged_delta(
    persona_proposal_log: &Arc<
        crate::persona_proposal::PersistentPersonaProposalLog,
    >,
    category_label: &str,
    draft: &ProposedDraft,
    session_id: &aivyx_core::SessionId,
) -> Result<(), String> {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    let proposed_op = build_proposed_op(
        category_label,
        draft,
        format!(
            "staged by persona-auto-proposer (Phase 114) from session {}",
            session_id.0
        ),
    )?;
    let proposal_id = format!("auto-{}-{}", session_id.0, now_ms);

    persona_proposal_log
        .append_pending(proposal_id, now_ms, session_id.0.to_string(), proposed_op)
        .await
        .map_err(|e| format!("pending append: {e}"))?;

    Ok(())
}

/// Phase 114 — map a `(category_label, ProposedDraft)` pair to
/// the right `ProposedPersonaDelta`. Returns `Err` for
/// (category, draft variant) pairs the runtime considers
/// incompatible (e.g. `BehavioralPreferences` + LearnedSkill
/// draft).
fn build_proposed_op(
    category_label: &str,
    draft: &ProposedDraft,
    reason: String,
) -> Result<crate::persona::ProposedPersonaDelta, String> {
    let category = parse_persona_delta_category(category_label)?;
    let op = match (category, draft) {
        // LearnedSkill must come with a LearnedSkill-shaped draft.
        (
            crate::persona::PersonaDeltaCategory::LearnedSkill,
            ProposedDraft::LearnedSkill {
                name,
                trigger,
                procedure,
            },
        ) => {
            let learned = crate::persona::LearnedSkill {
                name: name.clone(),
                trigger: trigger.clone(),
                procedure: procedure.clone(),
                ..Default::default()
            };
            crate::persona::PersonaDeltaOp::AppendList {
                value: learned.to_json_value(),
            }
        }
        // List categories take a plain ListAppend.
        (
            crate::persona::PersonaDeltaCategory::PrimaryUseCases
            | crate::persona::PersonaDeltaCategory::BehavioralPreferences
            | crate::persona::PersonaDeltaCategory::BehavioralConstraints
            | crate::persona::PersonaDeltaCategory::LearnedContext
            | crate::persona::PersonaDeltaCategory::CommunicationAdaptations
            | crate::persona::PersonaDeltaCategory::CharacterTraits
            | crate::persona::PersonaDeltaCategory::RelationshipMilestones,
            ProposedDraft::ListAppend { value },
        ) => crate::persona::PersonaDeltaOp::AppendList {
            value: value.clone(),
        },
        // Scalar categories take a ScalarSet.
        (
            crate::persona::PersonaDeltaCategory::AssistantName
            | crate::persona::PersonaDeltaCategory::OperatorProfile
            | crate::persona::PersonaDeltaCategory::CommunicationStyle,
            ProposedDraft::ScalarSet { value },
        ) => crate::persona::PersonaDeltaOp::SetScalar {
            value: Some(value.clone()),
        },
        // Phase 118 — ProfileHint must come with the matching
        // ProfileHint-shaped draft. The payload is JSON-
        // serialized so it rides inside the list-shaped
        // category's `AppendList { value }` op (same pattern
        // as Phase 110 LearnedSkill).
        (
            crate::persona::PersonaDeltaCategory::ProfileHint,
            ProposedDraft::ProfileHint {
                field,
                suggested_value,
                rationale,
            },
        ) => {
            let payload = aivyx_core::skill_proposer::ProfileFieldHint {
                field: *field,
                suggested_value: suggested_value.clone(),
                rationale: rationale.clone(),
            };
            crate::persona::PersonaDeltaOp::AppendList {
                value: serde_json::to_string(&payload).map_err(|e| {
                    format!("ProfileHint payload encode: {e}")
                })?,
            }
        }
        // Phase 118 — RoleDefinitionSuggestion must come with
        // the matching RoleDefinitionSuggestion-shaped draft.
        (
            crate::persona::PersonaDeltaCategory::RoleDefinitionSuggestion,
            ProposedDraft::RoleDefinitionSuggestion {
                name,
                parent,
                system_prompt_addendum,
                tool_allowlist_additions,
                rationale,
            },
        ) => {
            let payload = aivyx_core::skill_proposer::RoleDraft {
                name: name.clone(),
                parent: parent.clone(),
                system_prompt_addendum: system_prompt_addendum.clone(),
                tool_allowlist_additions: tool_allowlist_additions.clone(),
                rationale: rationale.clone(),
            };
            crate::persona::PersonaDeltaOp::AppendList {
                value: serde_json::to_string(&payload).map_err(|e| {
                    format!("RoleDraft payload encode: {e}")
                })?,
            }
        }
        // Any other (category, draft) combination is an
        // incompatibility — the judge produced a category
        // that doesn't match its draft shape. Refuse to write.
        (cat, draft) => {
            return Err(format!(
                "category {cat:?} incompatible with draft kind {}",
                draft.kind_label()
            ));
        }
    };
    Ok(crate::persona::ProposedPersonaDelta {
        category,
        op,
        reason: Some(reason),
        supersedes_proposal_id: None,
    })
}

/// Phase 114 — parse the judge's category-label string into
/// the runtime `PersonaDeltaCategory` enum.
fn parse_persona_delta_category(
    label: &str,
) -> Result<crate::persona::PersonaDeltaCategory, String> {
    use crate::persona::PersonaDeltaCategory as C;
    let cat = match label {
        "AssistantName" => C::AssistantName,
        "OperatorProfile" => C::OperatorProfile,
        "CommunicationStyle" => C::CommunicationStyle,
        "PrimaryUseCases" => C::PrimaryUseCases,
        "BehavioralPreferences" => C::BehavioralPreferences,
        "BehavioralConstraints" => C::BehavioralConstraints,
        "LearnedContext" => C::LearnedContext,
        "CommunicationAdaptations" => C::CommunicationAdaptations,
        "CharacterTraits" => C::CharacterTraits,
        "RelationshipMilestones" => C::RelationshipMilestones,
        "LearnedSkill" => C::LearnedSkill,
        // Phase 118 — the two operator-staged refinement
        // categories. Always-staged routing in
        // `decide_routing` makes the chain write path the
        // operator-approval flow's job, but the proposer's
        // `build_proposed_op` still has to construct the
        // pending PersonaDelta.
        "ProfileHint" => C::ProfileHint,
        "RoleDefinitionSuggestion" => C::RoleDefinitionSuggestion,
        other => {
            return Err(format!("unknown persona delta category: {other}"))
        }
    };
    Ok(cat)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_core::skill_proposer::{MatchMode, SkillDraft};
    use aivyx_llm::{
        ContentBlock, LlmError, LlmMessage, LlmProvider, LlmRequest, LlmStepEnd,
        LlmStream, LlmStreamEvent, LlmUsage,
    };
    use async_trait::async_trait;
    use std::collections::VecDeque;
    use std::sync::Mutex;
    use std::time::Duration;

    // ----- A minimal scripted provider -----

    enum ScriptedStep {
        FinalText(String),
        Error(LlmError),
    }

    struct ScriptedProvider {
        steps: Mutex<VecDeque<ScriptedStep>>,
        /// The `route` of every request seen, in call order.
        routes_seen: Mutex<Vec<Option<aivyx_llm::RouteHint>>>,
    }

    impl ScriptedProvider {
        fn new(steps: Vec<ScriptedStep>) -> Arc<Self> {
            Arc::new(ScriptedProvider {
                steps: Mutex::new(steps.into()),
                routes_seen: Mutex::new(Vec::new()),
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
            self.routes_seen.lock().unwrap().push(request.route.clone());
            let step = self.steps.lock().unwrap().pop_front().ok_or_else(|| {
                LlmError::Config("ScriptedProvider exhausted".to_string())
            })?;
            match step {
                ScriptedStep::FinalText(text) => Ok(Box::new(ScriptedStream {
                    text: Some(text),
                })),
                ScriptedStep::Error(e) => Err(e),
            }
        }
    }

    struct ScriptedStream {
        text: Option<String>,
    }

    #[async_trait]
    impl LlmStream for ScriptedStream {
        async fn next_event(
            &mut self,
        ) -> Result<Option<LlmStreamEvent>, LlmError> {
            Ok(None)
        }
        async fn finish(self: Box<Self>) -> Result<LlmStepEnd, LlmError> {
            Ok(LlmStepEnd::FinalMessage {
                text: self.text.unwrap_or_default(),
                usage: LlmUsage::default(),
            })
        }
    }

    // Avoid an "unused" warning on the imports above we'd
    // need only if a future test wants to construct an
    // LlmMessage::User directly. `_unused_smoke` documents the
    // intent for readers.
    #[allow(dead_code)]
    fn _unused_smoke() {
        let _: LlmMessage = LlmMessage::User {
            content: vec![ContentBlock::Text { text: "x".into() }],
        };
    }

    fn fire_threshold_signals() -> TurnSignals {
        TurnSignals {
            tool_calls_made: 5,
            distinct_tool_id_count: 3,
            duration: Duration::from_millis(8_000),
            had_successful_gate_resolve: false,
            ..TurnSignals::default()
        }
    }

    fn below_threshold_signals() -> TurnSignals {
        TurnSignals {
            tool_calls_made: 1,
            distinct_tool_id_count: 1,
            duration: Duration::from_millis(800),
            had_successful_gate_resolve: false,
            ..TurnSignals::default()
        }
    }

    // ----- Model routing Part 3a -----

    #[test]
    fn judge_route_task_tags_only_a_defaulted_judge_under_routing() {
        use aivyx_route::TaskKind;
        assert_eq!(judge_route_task(true, true), Some(TaskKind::Judge));
        // Explicit `judge_model` ⇒ untagged pin.
        assert_eq!(judge_route_task(true, false), None);
        // Routing off ⇒ always untagged.
        assert_eq!(judge_route_task(false, true), None);
        assert_eq!(judge_route_task(false, false), None);
    }

    #[test]
    fn judge_route_task_defaults_to_none_and_is_not_serialized() {
        let config = SkillAutoProposeConfig::default();
        assert_eq!(config.judge_route_task, None);
        let tagged = SkillAutoProposeConfig {
            judge_route_task: Some(aivyx_route::TaskKind::Judge),
            ..SkillAutoProposeConfig::default()
        };
        let json = serde_json::to_string(&tagged).unwrap();
        assert!(!json.contains("judge_route_task"), "{json}");
    }

    const WORTHLESS_VERDICT: &str = r#"{"is_worth_proposing":false,"confidence":0.1,
        "category":null,"proposed_draft":null,"is_duplicate_of":null}"#;

    #[tokio::test]
    async fn configured_judge_route_task_reaches_the_judge_request() {
        let provider = ScriptedProvider::new(vec![ScriptedStep::FinalText(
            WORTHLESS_VERDICT.into(),
        )]);
        let config = SkillAutoProposeConfig {
            judge_model: "m".into(),
            judge_route_task: Some(aivyx_route::TaskKind::Judge),
            ..SkillAutoProposeConfig::default()
        };
        let cancel = CancellationToken::new();
        auto_propose_for_turn(
            provider.clone(),
            &config,
            fire_threshold_signals(),
            "summary".into(),
            ExistingPersonaSnapshot::default(),
            &cancel,
        )
        .await;
        let seen = provider.routes_seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 1);
        let hint = seen[0].as_ref().expect("judge request tagged");
        assert_eq!(hint.task, aivyx_route::TaskKind::Judge);
        assert_eq!(hint.session, None);
        assert!(hint.estimated_prompt_tokens > 0);
    }

    #[tokio::test]
    async fn default_config_leaves_the_judge_request_untagged() {
        let provider = ScriptedProvider::new(vec![ScriptedStep::FinalText(
            WORTHLESS_VERDICT.into(),
        )]);
        let config = SkillAutoProposeConfig {
            judge_model: "m".into(),
            ..SkillAutoProposeConfig::default()
        };
        let cancel = CancellationToken::new();
        auto_propose_for_turn(
            provider.clone(),
            &config,
            fire_threshold_signals(),
            "summary".into(),
            ExistingPersonaSnapshot::default(),
            &cancel,
        )
        .await;
        assert_eq!(*provider.routes_seen.lock().unwrap(), vec![None]);
    }

    // ----- Outcome branches -----

    #[tokio::test]
    async fn disabled_config_returns_disabled_outcome_without_calling_llm() {
        // Empty scripted provider → if the LLM is called, we'd
        // get "exhausted" error. Master switch off should
        // skip the call entirely.
        let provider = ScriptedProvider::new(vec![]);
        let config = SkillAutoProposeConfig {
            enabled: false,
            ..SkillAutoProposeConfig::default()
        };
        let cancel = CancellationToken::new();
        let outcome = auto_propose_for_turn(
            provider,
            &config,
            fire_threshold_signals(),
            "summary".into(),
            ExistingPersonaSnapshot::default(),
            &cancel,
        )
        .await;
        assert!(matches!(outcome, SkillProposerOutcome::Disabled));
        assert_eq!(outcome.label(), "disabled");
    }

    #[tokio::test]
    async fn heuristic_gated_turns_skip_the_llm_call() {
        // Empty scripted provider — heuristic gate must
        // short-circuit before we'd hit the "exhausted"
        // error.
        let provider = ScriptedProvider::new(vec![]);
        let config = SkillAutoProposeConfig::default();
        let cancel = CancellationToken::new();
        let outcome = auto_propose_for_turn(
            provider,
            &config,
            below_threshold_signals(),
            "summary".into(),
            ExistingPersonaSnapshot::default(),
            &cancel,
        )
        .await;
        assert!(matches!(outcome, SkillProposerOutcome::HeuristicGated));
        assert_eq!(outcome.label(), "heuristic-gated");
    }

    #[tokio::test]
    async fn candidate_turn_with_worth_proposing_verdict_returns_verdict() {
        let provider = ScriptedProvider::new(vec![ScriptedStep::FinalText(
            r#"{"is_worth_proposing":true,"confidence":0.91,
            "category":"LearnedSkill",
            "proposed_draft":{"kind":"LearnedSkill",
            "name":"research-topic","trigger":"research X",
            "procedure":"1. ...\n2. ..."},"is_duplicate_of":null,
            "reasoning":"recurring"}"#
                .into(),
        )]);
        let config = SkillAutoProposeConfig::default();
        let cancel = CancellationToken::new();
        let outcome = auto_propose_for_turn(
            provider,
            &config,
            fire_threshold_signals(),
            "summary".into(),
            ExistingPersonaSnapshot::default(),
            &cancel,
        )
        .await;
        match outcome {
            SkillProposerOutcome::Verdict(r) => {
                assert!(r.is_worth_proposing);
                assert!((r.confidence - 0.91).abs() < 1e-6);
                let draft = r.proposed_skill().unwrap();
                assert_eq!(draft.name, "research-topic");
                // Ensure SkillDraft re-export is wired
                let _ = SkillDraft::clone(&draft);
            }
            _ => panic!("expected Verdict; got {:?}", outcome),
        }
    }

    #[tokio::test]
    async fn judge_provider_error_becomes_judge_error_outcome() {
        let provider = ScriptedProvider::new(vec![ScriptedStep::Error(
            LlmError::Config("simulated outage".into()),
        )]);
        let config = SkillAutoProposeConfig::default();
        let cancel = CancellationToken::new();
        let outcome = auto_propose_for_turn(
            provider,
            &config,
            fire_threshold_signals(),
            "summary".into(),
            ExistingPersonaSnapshot::default(),
            &cancel,
        )
        .await;
        match outcome {
            SkillProposerOutcome::JudgeError(msg) => {
                assert!(msg.contains("provider"));
                assert!(msg.contains("simulated outage"));
            }
            _ => panic!("expected JudgeError; got {:?}", outcome),
        }
    }

    #[tokio::test]
    async fn judge_parse_failure_becomes_judge_error_outcome() {
        let provider = ScriptedProvider::new(vec![ScriptedStep::FinalText(
            "the llm misbehaved and didn't return json".into(),
        )]);
        let config = SkillAutoProposeConfig::default();
        let cancel = CancellationToken::new();
        let outcome = auto_propose_for_turn(
            provider,
            &config,
            fire_threshold_signals(),
            "summary".into(),
            ExistingPersonaSnapshot::default(),
            &cancel,
        )
        .await;
        match outcome {
            SkillProposerOutcome::JudgeError(msg) => {
                assert!(msg.contains("parse"));
            }
            _ => panic!("expected JudgeError; got {:?}", outcome),
        }
    }

    #[tokio::test]
    async fn judge_confidence_out_of_range_becomes_judge_error() {
        let provider = ScriptedProvider::new(vec![ScriptedStep::FinalText(
            r#"{"is_worth_proposing":true,"confidence":2.0,
            "proposed_skill":null,"is_duplicate_of":null}"#
                .into(),
        )]);
        let config = SkillAutoProposeConfig::default();
        let cancel = CancellationToken::new();
        let outcome = auto_propose_for_turn(
            provider,
            &config,
            fire_threshold_signals(),
            "summary".into(),
            ExistingPersonaSnapshot::default(),
            &cancel,
        )
        .await;
        match outcome {
            SkillProposerOutcome::JudgeError(msg) => {
                assert!(msg.contains("confidence"));
            }
            _ => panic!("expected JudgeError; got {:?}", outcome),
        }
    }

    // ----- Failure isolation: spawn wrapper -----

    #[tokio::test]
    async fn spawn_returns_a_join_handle_that_yields_the_outcome() {
        let provider = ScriptedProvider::new(vec![ScriptedStep::FinalText(
            r#"{"is_worth_proposing":false,"confidence":0.2,
            "proposed_skill":null,"is_duplicate_of":null}"#
                .into(),
        )]);
        let config = SkillAutoProposeConfig::default();
        let cancel = CancellationToken::new();
        let handle = spawn_auto_proposer_task(
            provider,
            config,
            fire_threshold_signals(),
            "summary".into(),
            ExistingPersonaSnapshot::default(),
            cancel,
        );
        let outcome = handle.await.expect("task must not panic");
        match outcome {
            SkillProposerOutcome::Verdict(r) => {
                assert!(!r.is_worth_proposing);
            }
            _ => panic!("expected Verdict; got {:?}", outcome),
        }
    }

    #[tokio::test]
    async fn spawn_wrapper_does_not_panic_on_provider_error() {
        // The crux of the Q2b failure-isolation contract:
        // even a provider that simulates an outage cannot
        // bring down the daemon. The spawn wrapper logs WARN
        // and yields a JudgeError outcome; the JoinHandle
        // completes cleanly.
        let provider = ScriptedProvider::new(vec![ScriptedStep::Error(
            LlmError::Config("kapow".into()),
        )]);
        let config = SkillAutoProposeConfig::default();
        let cancel = CancellationToken::new();
        let handle = spawn_auto_proposer_task(
            provider,
            config,
            fire_threshold_signals(),
            "summary".into(),
            ExistingPersonaSnapshot::default(),
            cancel,
        );
        let outcome = handle.await.expect("spawn must not panic");
        assert_eq!(outcome.label(), "judge-error");
    }

    // ----- Config -----

    #[test]
    fn default_config_is_enabled_with_sane_thresholds() {
        let c = SkillAutoProposeConfig::default();
        assert!(c.enabled);
        assert_eq!(c.heuristic.mode, MatchMode::Any);
        assert_eq!(c.judge_max_tokens, 800);
        assert!((c.auto_accept_confidence_threshold - 0.85).abs() < 1e-6);
        assert!((c.fuzzy_match_threshold - 0.80).abs() < 1e-6);
    }

    #[test]
    fn config_round_trips_through_serde_json() {
        let original = SkillAutoProposeConfig::default();
        let s = serde_json::to_string(&original).unwrap();
        let back: SkillAutoProposeConfig = serde_json::from_str(&s).unwrap();
        assert_eq!(back, original);
    }

    // ----- Task 5 — Decision routing -----

    fn worth_proposing_verdict(confidence: f32) -> JudgeResponse {
        JudgeResponse {
            is_worth_proposing: true,
            confidence,
            category: Some("LearnedSkill".into()),
            proposed_draft: Some(ProposedDraft::LearnedSkill {
                name: "research-topic".into(),
                trigger: "user asks to research X".into(),
                procedure: "1. fs.read\n2. web.fetch".into(),
            }),
            is_duplicate_of: None,
            reasoning: None,
        }
    }

    /// Helper for tests that need a verdict with a specific
    /// LearnedSkill name. Mirrors `worth_proposing_verdict` but
    /// lets the caller override the skill name (used by
    /// fuzzy-match dedup tests).
    fn worth_proposing_verdict_with_name(
        confidence: f32,
        name: &str,
    ) -> JudgeResponse {
        JudgeResponse {
            is_worth_proposing: true,
            confidence,
            category: Some("LearnedSkill".into()),
            proposed_draft: Some(ProposedDraft::LearnedSkill {
                name: name.into(),
                trigger: "user asks to research X".into(),
                procedure: "1. fs.read\n2. web.fetch".into(),
            }),
            is_duplicate_of: None,
            reasoning: None,
        }
    }

    fn existing_skills_fixture() -> Vec<ExistingSkillSnapshot> {
        vec![
            ExistingSkillSnapshot {
                name: "summarize-pdf".into(),
                trigger: "user shares a PDF".into(),
                procedure_summary: "fs.read PDF, extract sections".into(),
            },
            ExistingSkillSnapshot {
                name: "deploy-to-staging".into(),
                trigger: "user requests staging deploy".into(),
                procedure_summary: "git.status, shell.exec deploy script".into(),
            },
        ]
    }

    #[test]
    fn routing_auto_accepts_at_threshold() {
        let verdict = worth_proposing_verdict(0.85);
        let config = SkillAutoProposeConfig::default();
        let d = decide_routing(&verdict, &[], &config);
        match &d {
            SkillRoutingDecision::AutoAccept {
                confidence,
                draft,
                category,
            } => {
                assert!((confidence - 0.85).abs() < 1e-6);
                assert_eq!(category, "LearnedSkill");
                assert_eq!(
                    draft.as_skill_draft().unwrap().name,
                    "research-topic"
                );
            }
            _ => panic!("expected AutoAccept; got {:?}", d),
        }
        assert_eq!(d.label(), "auto-accept");
    }

    #[test]
    fn routing_auto_accepts_above_threshold() {
        let verdict = worth_proposing_verdict(0.95);
        let config = SkillAutoProposeConfig::default();
        let d = decide_routing(&verdict, &[], &config);
        assert!(matches!(d, SkillRoutingDecision::AutoAccept { .. }));
    }

    #[test]
    fn routing_stages_below_threshold() {
        let verdict = worth_proposing_verdict(0.84);
        let config = SkillAutoProposeConfig::default();
        let d = decide_routing(&verdict, &[], &config);
        match &d {
            SkillRoutingDecision::Staged {
                confidence,
                draft,
                category,
            } => {
                assert!((confidence - 0.84).abs() < 1e-6);
                assert_eq!(category, "LearnedSkill");
                assert_eq!(
                    draft.as_skill_draft().unwrap().name,
                    "research-topic"
                );
            }
            _ => panic!("expected Staged; got {:?}", d),
        }
        assert_eq!(d.label(), "staged");
    }

    #[test]
    fn routing_drops_when_judge_declares_duplicate() {
        let mut verdict = worth_proposing_verdict(0.99);
        verdict.is_worth_proposing = false;
        verdict.is_duplicate_of = Some("summarize-pdf".into());
        let config = SkillAutoProposeConfig::default();
        let d = decide_routing(&verdict, &existing_skills_fixture(), &config);
        match &d {
            SkillRoutingDecision::DroppedJudgeDup { duplicate_of } => {
                assert_eq!(duplicate_of, "summarize-pdf");
            }
            _ => panic!("expected DroppedJudgeDup; got {:?}", d),
        }
        assert_eq!(d.label(), "dup-dropped-llm");
    }

    #[test]
    fn routing_drops_when_not_worth_proposing() {
        let verdict = JudgeResponse {
            is_worth_proposing: false,
            confidence: 0.5,
            category: None,
            proposed_draft: None,
            is_duplicate_of: None,
            reasoning: Some("one-off chat".into()),
        };
        let config = SkillAutoProposeConfig::default();
        let d = decide_routing(&verdict, &[], &config);
        assert!(matches!(d, SkillRoutingDecision::DroppedNotWorthProposing));
        assert_eq!(d.label(), "not-worth-proposing");
    }

    #[test]
    fn routing_drops_when_judge_says_worth_but_omits_draft() {
        // LLM misbehavior — treat as not-worth-proposing.
        let verdict = JudgeResponse {
            is_worth_proposing: true,
            confidence: 0.9,
            category: Some("LearnedSkill".into()),
            proposed_draft: None,
            is_duplicate_of: None,
            reasoning: None,
        };
        let config = SkillAutoProposeConfig::default();
        let d = decide_routing(&verdict, &[], &config);
        assert!(matches!(d, SkillRoutingDecision::DroppedNotWorthProposing));
    }

    #[test]
    fn routing_fuzzy_match_drops_obvious_title_dup() {
        // Existing: "summarize-pdf"; candidate: "summarize-pdf" → identical
        // title → fuzzy match fires.
        let verdict = worth_proposing_verdict_with_name(0.95, "summarize-pdf");
        let config = SkillAutoProposeConfig::default();
        let d = decide_routing(&verdict, &existing_skills_fixture(), &config);
        match &d {
            SkillRoutingDecision::DroppedFuzzyDup {
                matched_existing_name,
            } => {
                assert_eq!(matched_existing_name, "summarize-pdf");
            }
            _ => panic!("expected DroppedFuzzyDup; got {:?}", d),
        }
        assert_eq!(d.label(), "dup-dropped-fuzzy");
    }

    #[test]
    fn routing_fuzzy_match_drops_underscored_vs_dotted_variant() {
        // Existing: "summarize-pdf"; candidate: "summarize_pdf" — same
        // tokens after normalization.
        let verdict = worth_proposing_verdict_with_name(0.95, "summarize_pdf");
        let config = SkillAutoProposeConfig::default();
        let d = decide_routing(&verdict, &existing_skills_fixture(), &config);
        assert!(matches!(d, SkillRoutingDecision::DroppedFuzzyDup { .. }));
    }

    #[test]
    fn routing_fuzzy_match_drops_reordered_tokens() {
        // Existing: "deploy-to-staging"; candidate: "staging-to-deploy" —
        // same token set after normalization → Jaccard 1.0.
        let verdict = worth_proposing_verdict_with_name(0.95, "staging-to-deploy");
        let config = SkillAutoProposeConfig::default();
        let d = decide_routing(&verdict, &existing_skills_fixture(), &config);
        assert!(matches!(d, SkillRoutingDecision::DroppedFuzzyDup { .. }));
    }

    #[test]
    fn routing_does_not_fuzzy_drop_distinct_titles() {
        let verdict = worth_proposing_verdict(0.95);
        // "research-topic" has zero tokens in common with the two
        // existing skill names.
        let config = SkillAutoProposeConfig::default();
        let d = decide_routing(&verdict, &existing_skills_fixture(), &config);
        assert!(matches!(d, SkillRoutingDecision::AutoAccept { .. }));
    }

    #[test]
    fn routing_respects_higher_fuzzy_threshold() {
        // With a high threshold (0.99), a partial overlap shouldn't
        // count as a dup. Existing "summarize-pdf"; candidate
        // "summarize-doc" — overlap is {summarize} of {summarize, pdf,
        // doc} → 1/3 → below 0.99.
        let verdict = worth_proposing_verdict_with_name(0.95, "summarize-doc");
        let config = SkillAutoProposeConfig {
            fuzzy_match_threshold: 0.99,
            ..SkillAutoProposeConfig::default()
        };
        let d = decide_routing(&verdict, &existing_skills_fixture(), &config);
        assert!(matches!(d, SkillRoutingDecision::AutoAccept { .. }));
    }

    // ----- Task 5 — Title similarity primitive -----

    #[test]
    fn title_similarity_identical_titles_are_one() {
        assert_eq!(title_similarity("research-topic", "research-topic"), 1.0);
    }

    #[test]
    fn title_similarity_normalizes_separators() {
        assert_eq!(title_similarity("memory.gc", "memory_gc"), 1.0);
        assert_eq!(title_similarity("memory.gc", "memory-gc"), 1.0);
    }

    #[test]
    fn title_similarity_is_case_insensitive() {
        assert_eq!(title_similarity("Memory.GC", "memory.gc"), 1.0);
    }

    #[test]
    fn title_similarity_jaccard_for_partial_overlap() {
        // "summarize-pdf" vs "summarize-doc" — tokens {summarize, pdf}
        // vs {summarize, doc} → intersection {summarize}, union
        // {summarize, pdf, doc} → 1/3 ≈ 0.333.
        let sim = title_similarity("summarize-pdf", "summarize-doc");
        assert!((sim - 1.0 / 3.0).abs() < 1e-5);
    }

    #[test]
    fn title_similarity_disjoint_tokens_are_zero() {
        assert_eq!(title_similarity("alpha", "beta"), 0.0);
    }

    #[test]
    fn title_similarity_empty_inputs() {
        assert_eq!(title_similarity("", ""), 1.0);
        assert_eq!(title_similarity("alpha", ""), 0.0);
        assert_eq!(title_similarity("", "alpha"), 0.0);
    }

    #[test]
    fn fuzzy_match_returns_first_match_above_threshold() {
        let existing = existing_skills_fixture();
        let m = fuzzy_match_against_existing("summarize-pdf", &existing, 0.80);
        assert_eq!(m.as_deref(), Some("summarize-pdf"));
    }

    #[test]
    fn fuzzy_match_returns_none_when_no_existing_match() {
        let existing = existing_skills_fixture();
        let m = fuzzy_match_against_existing("totally-novel-skill", &existing, 0.80);
        assert!(m.is_none());
    }

    // ----- Phase 114 Task 4 — Per-category routing -----

    fn config_with_per_category_defaults() -> SkillAutoProposeConfig {
        SkillAutoProposeConfig {
            per_category: Some(PerCategoryConfigSet {
                assistant_name: PerCategoryConfig {
                    enabled: false,
                    auto_accept_confidence_threshold: 0.99,
                },
                operator_profile: PerCategoryConfig {
                    enabled: false,
                    auto_accept_confidence_threshold: 0.99,
                },
                communication_style: PerCategoryConfig {
                    enabled: false,
                    auto_accept_confidence_threshold: 0.99,
                },
                primary_use_cases: PerCategoryConfig {
                    enabled: true,
                    auto_accept_confidence_threshold: 0.85,
                },
                behavioral_preferences: PerCategoryConfig {
                    enabled: true,
                    auto_accept_confidence_threshold: 0.85,
                },
                behavioral_constraints: PerCategoryConfig {
                    enabled: true,
                    auto_accept_confidence_threshold: 0.85,
                },
                learned_context: PerCategoryConfig {
                    enabled: true,
                    auto_accept_confidence_threshold: 0.85,
                },
                communication_adaptations: PerCategoryConfig {
                    enabled: true,
                    auto_accept_confidence_threshold: 0.85,
                },
                character_traits: PerCategoryConfig {
                    enabled: true,
                    auto_accept_confidence_threshold: 0.85,
                },
                relationship_milestones: PerCategoryConfig {
                    enabled: true,
                    auto_accept_confidence_threshold: 0.85,
                },
                learned_skill: PerCategoryConfig {
                    enabled: true,
                    auto_accept_confidence_threshold: 0.85,
                },
                // Phase 118 — enabled by default in test
                // fixture; threshold value is dead code at
                // routing time but carried for shape
                // consistency.
                profile_hint: PerCategoryConfig {
                    enabled: true,
                    auto_accept_confidence_threshold: 0.85,
                },
                role_definition_suggestion: PerCategoryConfig {
                    enabled: true,
                    auto_accept_confidence_threshold: 0.85,
                },
            }),
            ..SkillAutoProposeConfig::default()
        }
    }

    fn list_append_verdict(
        category: &str,
        confidence: f32,
        value: &str,
    ) -> JudgeResponse {
        JudgeResponse {
            is_worth_proposing: true,
            confidence,
            category: Some(category.into()),
            proposed_draft: Some(ProposedDraft::ListAppend {
                value: value.into(),
            }),
            is_duplicate_of: None,
            reasoning: None,
        }
    }

    fn scalar_set_verdict(
        category: &str,
        confidence: f32,
        value: &str,
    ) -> JudgeResponse {
        JudgeResponse {
            is_worth_proposing: true,
            confidence,
            category: Some(category.into()),
            proposed_draft: Some(ProposedDraft::ScalarSet {
                value: value.into(),
            }),
            is_duplicate_of: None,
            reasoning: None,
        }
    }

    #[test]
    fn routing_per_category_disabled_drops_to_category_disabled_outcome() {
        // Scalar default: AssistantName disabled.
        let verdict =
            scalar_set_verdict("AssistantName", 0.999, "Aivyx PA");
        let config = config_with_per_category_defaults();
        let d = decide_routing(&verdict, &[], &config);
        match &d {
            SkillRoutingDecision::DroppedCategoryDisabled { category } => {
                assert_eq!(category, "AssistantName");
            }
            _ => panic!("expected DroppedCategoryDisabled; got {:?}", d),
        }
        assert_eq!(d.label(), "category-disabled");
    }

    #[test]
    fn routing_per_category_uses_per_category_threshold_not_top_level() {
        // Top-level threshold default 0.85. Per-category set
        // CommunicationAdaptations threshold to 0.95; a verdict
        // at 0.90 should stage, not auto-accept.
        let verdict = list_append_verdict(
            "CommunicationAdaptations",
            0.90,
            "the operator likes concise code reviews",
        );
        let mut config = config_with_per_category_defaults();
        config
            .per_category
            .as_mut()
            .unwrap()
            .communication_adaptations
            .auto_accept_confidence_threshold = 0.95;
        let d = decide_routing(&verdict, &[], &config);
        assert!(matches!(d, SkillRoutingDecision::Staged { .. }));
    }

    #[test]
    fn routing_per_category_auto_accepts_when_above_per_category_threshold() {
        let verdict = list_append_verdict(
            "BehavioralPreferences",
            0.90,
            "prefer terse replies",
        );
        let config = config_with_per_category_defaults();
        let d = decide_routing(&verdict, &[], &config);
        match &d {
            SkillRoutingDecision::AutoAccept {
                category, draft, ..
            } => {
                assert_eq!(category, "BehavioralPreferences");
                assert!(matches!(draft, ProposedDraft::ListAppend { .. }));
            }
            _ => panic!("expected AutoAccept; got {:?}", d),
        }
    }

    #[test]
    fn routing_phase_113_alias_path_uses_top_level_threshold() {
        // Phase 113 config: per_category = None. The routing
        // falls back to auto_accept_confidence_threshold for
        // all categories.
        let verdict = list_append_verdict(
            "BehavioralPreferences",
            0.90,
            "prefer terse",
        );
        let config = SkillAutoProposeConfig {
            auto_accept_confidence_threshold: 0.85,
            per_category: None,
            ..SkillAutoProposeConfig::default()
        };
        let d = decide_routing(&verdict, &[], &config);
        assert!(matches!(d, SkillRoutingDecision::AutoAccept { .. }));
    }

    #[test]
    fn routing_unknown_category_drops_to_category_disabled() {
        // The judge picked a label not in the runtime
        // enumeration. Defensive — treat as disabled.
        let verdict = list_append_verdict("NotARealCategory", 0.95, "x");
        let config = config_with_per_category_defaults();
        let d = decide_routing(&verdict, &[], &config);
        assert!(matches!(
            d,
            SkillRoutingDecision::DroppedCategoryDisabled { .. }
        ));
    }

    // ----- Phase 118 — Always-staged routing override -----

    fn profile_hint_verdict(confidence: f32) -> JudgeResponse {
        JudgeResponse {
            is_worth_proposing: true,
            confidence,
            category: Some("ProfileHint".into()),
            proposed_draft: Some(ProposedDraft::ProfileHint {
                field: aivyx_core::skill_proposer::ProfileField::CommunicationStyle,
                suggested_value: "terse".into(),
                rationale: "operator consistently uses brevity".into(),
            }),
            is_duplicate_of: None,
            reasoning: None,
        }
    }

    fn role_def_suggestion_verdict(confidence: f32) -> JudgeResponse {
        JudgeResponse {
            is_worth_proposing: true,
            confidence,
            category: Some("RoleDefinitionSuggestion".into()),
            proposed_draft: Some(ProposedDraft::RoleDefinitionSuggestion {
                name: "research-deploy".into(),
                parent: Some("research".into()),
                system_prompt_addendum: "...".into(),
                tool_allowlist_additions: vec!["git.commit".into()],
                rationale: "operator's research-then-deploy shape repeats".into(),
            }),
            is_duplicate_of: None,
            reasoning: None,
        }
    }

    #[test]
    fn is_always_staged_category_recognizes_phase_118_labels() {
        // Pure-function pin so the contract is checkable
        // without setting up a full routing fixture. The two
        // labels here are the contract; everything else
        // returns false.
        assert!(is_always_staged_category("ProfileHint"));
        assert!(is_always_staged_category("RoleDefinitionSuggestion"));
        assert!(!is_always_staged_category("LearnedSkill"));
        assert!(!is_always_staged_category("BehavioralPreferences"));
        assert!(!is_always_staged_category("CommunicationStyle"));
        assert!(!is_always_staged_category(""));
    }

    #[test]
    fn routing_profile_hint_stages_even_at_max_confidence() {
        // The P13 Profile-operator-owned contract makes
        // auto-accept a contract violation regardless of how
        // confident the judge is. Confidence at 1.0 still
        // routes to Staged.
        let verdict = profile_hint_verdict(1.0);
        let config = config_with_per_category_defaults();
        let d = decide_routing(&verdict, &[], &config);
        match d {
            SkillRoutingDecision::Staged {
                category,
                confidence,
                ..
            } => {
                assert_eq!(category, "ProfileHint");
                assert!((confidence - 1.0).abs() < 1e-6);
            }
            other => panic!("expected Staged, got {other:?}"),
        }
    }

    #[test]
    fn routing_role_definition_suggestion_stages_even_at_max_confidence() {
        // P9 Role-config operator-curated contract preserved
        // by hard-coded Staged routing.
        let verdict = role_def_suggestion_verdict(1.0);
        let config = config_with_per_category_defaults();
        let d = decide_routing(&verdict, &[], &config);
        match d {
            SkillRoutingDecision::Staged {
                category,
                confidence,
                ..
            } => {
                assert_eq!(category, "RoleDefinitionSuggestion");
                assert!((confidence - 1.0).abs() < 1e-6);
            }
            other => panic!("expected Staged, got {other:?}"),
        }
    }

    #[test]
    fn routing_phase_118_categories_under_phase_113_alias_config_still_stage() {
        // Phase 113 alias config has `per_category = None`
        // (single top-level threshold). The always-staged
        // override must still fire — it's a category-name
        // check, not a per_category-shape check.
        let verdict = profile_hint_verdict(0.99);
        let config = SkillAutoProposeConfig::default(); // per_category = None
        let d = decide_routing(&verdict, &[], &config);
        assert!(
            matches!(d, SkillRoutingDecision::Staged { ref category, .. } if category == "ProfileHint"),
            "expected Staged for ProfileHint under per_category=None config, got {d:?}",
        );
    }

    #[test]
    fn routing_profile_hint_respects_operator_disable() {
        // Operator can still disable proposing Phase 118
        // categories entirely via the per-category enable
        // flag. The always-staged override is for the
        // confidence axis; the enable axis stays operator-
        // controlled.
        let verdict = profile_hint_verdict(0.95);
        let mut config = config_with_per_category_defaults();
        if let Some(pc) = config.per_category.as_mut() {
            pc.profile_hint.enabled = false;
        }
        let d = decide_routing(&verdict, &[], &config);
        match d {
            SkillRoutingDecision::DroppedCategoryDisabled { category } => {
                assert_eq!(category, "ProfileHint");
            }
            other => panic!("expected DroppedCategoryDisabled, got {other:?}"),
        }
    }

    #[test]
    fn routing_role_definition_suggestion_respects_operator_disable() {
        let verdict = role_def_suggestion_verdict(0.95);
        let mut config = config_with_per_category_defaults();
        if let Some(pc) = config.per_category.as_mut() {
            pc.role_definition_suggestion.enabled = false;
        }
        let d = decide_routing(&verdict, &[], &config);
        match d {
            SkillRoutingDecision::DroppedCategoryDisabled { category } => {
                assert_eq!(category, "RoleDefinitionSuggestion");
            }
            other => panic!("expected DroppedCategoryDisabled, got {other:?}"),
        }
    }

    #[test]
    fn routing_profile_hint_still_drops_when_judge_says_not_worth_proposing() {
        // The always-staged override only fires when the
        // proposal makes it to the threshold gate. A judge
        // verdict of `is_worth_proposing = false` drops
        // upstream regardless of category.
        let verdict = JudgeResponse {
            is_worth_proposing: false,
            ..profile_hint_verdict(0.0)
        };
        let config = config_with_per_category_defaults();
        let d = decide_routing(&verdict, &[], &config);
        assert!(matches!(
            d,
            SkillRoutingDecision::DroppedNotWorthProposing
        ));
    }

    #[test]
    fn routing_profile_hint_judge_dup_still_drops() {
        // Cross-category dedup from the judge still drops
        // Phase 118 candidates — same contract as every other
        // category.
        let verdict = JudgeResponse {
            is_duplicate_of: Some("existing CommunicationStyle hint".into()),
            ..profile_hint_verdict(0.95)
        };
        let config = config_with_per_category_defaults();
        let d = decide_routing(&verdict, &[], &config);
        assert!(matches!(d, SkillRoutingDecision::DroppedJudgeDup { .. }));
    }

    // ----- Phase 114 Task 4 — Category-op dispatch -----

    #[test]
    fn build_proposed_op_dispatches_learned_skill() {
        let draft = ProposedDraft::LearnedSkill {
            name: "research".into(),
            trigger: "research X".into(),
            procedure: "1. ...".into(),
        };
        let op = build_proposed_op("LearnedSkill", &draft, "r".into()).unwrap();
        assert_eq!(
            op.category,
            crate::persona::PersonaDeltaCategory::LearnedSkill
        );
        match op.op {
            crate::persona::PersonaDeltaOp::AppendList { value } => {
                let parsed =
                    crate::persona::LearnedSkill::from_json_value(&value)
                        .expect("learned-skill JSON parses");
                assert_eq!(parsed.name, "research");
                assert_eq!(parsed.procedure, "1. ...");
            }
            other => panic!("expected AppendList, got {other:?}"),
        }
    }

    #[test]
    fn build_proposed_op_dispatches_list_append_for_behavioral_preferences() {
        let draft = ProposedDraft::ListAppend {
            value: "prefer terse".into(),
        };
        let op = build_proposed_op(
            "BehavioralPreferences",
            &draft,
            "r".into(),
        )
        .unwrap();
        assert_eq!(
            op.category,
            crate::persona::PersonaDeltaCategory::BehavioralPreferences
        );
        match op.op {
            crate::persona::PersonaDeltaOp::AppendList { value } => {
                assert_eq!(value, "prefer terse");
            }
            other => panic!("expected AppendList, got {other:?}"),
        }
    }

    #[test]
    fn build_proposed_op_dispatches_scalar_set_for_assistant_name() {
        let draft = ProposedDraft::ScalarSet {
            value: "Aivyx PA".into(),
        };
        let op = build_proposed_op("AssistantName", &draft, "r".into()).unwrap();
        assert_eq!(
            op.category,
            crate::persona::PersonaDeltaCategory::AssistantName
        );
        match op.op {
            crate::persona::PersonaDeltaOp::SetScalar { value } => {
                assert_eq!(value.as_deref(), Some("Aivyx PA"));
            }
            other => panic!("expected SetScalar, got {other:?}"),
        }
    }

    #[test]
    fn build_proposed_op_rejects_incompatible_pairs() {
        // List category + LearnedSkill draft → incompatible.
        let bad = ProposedDraft::LearnedSkill {
            name: "x".into(),
            trigger: "y".into(),
            procedure: "z".into(),
        };
        let err = build_proposed_op(
            "BehavioralPreferences",
            &bad,
            "r".into(),
        )
        .unwrap_err();
        assert!(err.contains("incompatible"), "{err}");

        // Scalar category + ListAppend draft → incompatible.
        let bad = ProposedDraft::ListAppend { value: "x".into() };
        let err =
            build_proposed_op("AssistantName", &bad, "r".into()).unwrap_err();
        assert!(err.contains("incompatible"), "{err}");
    }

    #[test]
    fn parse_persona_delta_category_accepts_all_thirteen_labels() {
        for label in [
            "AssistantName",
            "OperatorProfile",
            "CommunicationStyle",
            "PrimaryUseCases",
            "BehavioralPreferences",
            "BehavioralConstraints",
            "LearnedContext",
            "CommunicationAdaptations",
            "CharacterTraits",
            "RelationshipMilestones",
            "LearnedSkill",
            // Phase 118 additions.
            "ProfileHint",
            "RoleDefinitionSuggestion",
        ] {
            parse_persona_delta_category(label)
                .unwrap_or_else(|e| panic!("label {label} failed: {e}"));
        }
        assert!(parse_persona_delta_category("NotARealCategory").is_err());
    }

    // ----- Phase 118 — build_proposed_op dispatch -----

    #[test]
    fn build_proposed_op_dispatches_profile_hint() {
        let draft = ProposedDraft::ProfileHint {
            field: aivyx_core::skill_proposer::ProfileField::CommunicationStyle,
            suggested_value: "terse and bullet-formatted".into(),
            rationale: "operator consistently uses bullets".into(),
        };
        let op = build_proposed_op(
            "ProfileHint",
            &draft,
            "reason text".into(),
        )
        .expect("compatible pair");
        assert_eq!(op.category, crate::persona::PersonaDeltaCategory::ProfileHint);
        assert_eq!(op.reason.as_deref(), Some("reason text"));
        match &op.op {
            crate::persona::PersonaDeltaOp::AppendList { value } => {
                // The chain value is a JSON-serialized
                // ProfileFieldHint payload. Round-trip-decode
                // it to verify the contents.
                let parsed: aivyx_core::skill_proposer::ProfileFieldHint =
                    serde_json::from_str(value).expect("round-trip");
                assert_eq!(
                    parsed.field,
                    aivyx_core::skill_proposer::ProfileField::CommunicationStyle
                );
                assert!(parsed.suggested_value.contains("bullet-formatted"));
                assert!(parsed.rationale.contains("bullets"));
            }
            other => panic!("expected AppendList, got {other:?}"),
        }
    }

    #[test]
    fn build_proposed_op_dispatches_role_definition_suggestion() {
        let draft = ProposedDraft::RoleDefinitionSuggestion {
            name: "research-deploy".into(),
            parent: Some("research".into()),
            system_prompt_addendum: "After research, summarize the diff.".into(),
            tool_allowlist_additions: vec!["git.commit".into()],
            rationale: "operator's research-then-deploy shape repeats".into(),
        };
        let op = build_proposed_op(
            "RoleDefinitionSuggestion",
            &draft,
            "r".into(),
        )
        .expect("compatible pair");
        assert_eq!(
            op.category,
            crate::persona::PersonaDeltaCategory::RoleDefinitionSuggestion
        );
        match &op.op {
            crate::persona::PersonaDeltaOp::AppendList { value } => {
                let parsed: aivyx_core::skill_proposer::RoleDraft =
                    serde_json::from_str(value).expect("round-trip");
                assert_eq!(parsed.name, "research-deploy");
                assert_eq!(parsed.parent.as_deref(), Some("research"));
                assert_eq!(parsed.tool_allowlist_additions.len(), 1);
                assert!(parsed.rationale.contains("repeats"));
            }
            other => panic!("expected AppendList, got {other:?}"),
        }
    }

    #[test]
    fn build_proposed_op_rejects_profile_hint_with_wrong_draft_shape() {
        // Category ProfileHint + ListAppend draft → incompatible.
        let bad = ProposedDraft::ListAppend { value: "x".into() };
        let err =
            build_proposed_op("ProfileHint", &bad, "r".into()).unwrap_err();
        assert!(err.contains("incompatible"), "{err}");
    }

    #[test]
    fn build_proposed_op_rejects_role_definition_with_wrong_draft_shape() {
        // Category RoleDefinitionSuggestion + LearnedSkill draft → incompatible.
        let bad = ProposedDraft::LearnedSkill {
            name: "x".into(),
            trigger: "y".into(),
            procedure: "z".into(),
        };
        let err =
            build_proposed_op("RoleDefinitionSuggestion", &bad, "r".into())
                .unwrap_err();
        assert!(err.contains("incompatible"), "{err}");
    }

    // ----- Task 6 — Audit-event construction helpers -----

    #[test]
    fn signals_matched_reports_each_axis_independently() {
        let signals = TurnSignals {
            tool_calls_made: 3,
            distinct_tool_id_count: 1, // below min=2
            duration: Duration::from_millis(10_000),
            had_successful_gate_resolve: true,
            ..TurnSignals::default()
        };
        let config = HeuristicConfig::default();
        let m = signals_matched(&signals, &config);
        assert!(m.tool_call_count);
        assert!(!m.distinct_tool_id_count);
        assert!(m.duration);
        assert!(m.gate_resolve);
    }

    #[test]
    fn signals_matched_reports_all_below_threshold_as_all_false() {
        let signals = TurnSignals {
            tool_calls_made: 0,
            distinct_tool_id_count: 0,
            duration: Duration::from_millis(0),
            had_successful_gate_resolve: false,
            ..TurnSignals::default()
        };
        let config = HeuristicConfig::default();
        let m = signals_matched(&signals, &config);
        assert!(!m.tool_call_count);
        assert!(!m.distinct_tool_id_count);
        assert!(!m.duration);
        assert!(!m.gate_resolve);
        assert!(!m.profile_pattern_repeated);
        assert!(!m.role_shape_recurring);
    }

    #[test]
    fn signals_matched_reports_profile_pattern_repeated_when_threshold_crossed() {
        // Phase 118 — `profile_pattern_repeated` fires only
        // when keyword_key_prior_total_count crosses the
        // configured threshold. Reports truthfully regardless
        // of whether the combined gate also fires.
        let signals = TurnSignals {
            keyword_key_prior_total_count: 8,
            ..TurnSignals::default()
        };
        let config = HeuristicConfig::default(); // default min=5
        let m = signals_matched(&signals, &config);
        assert!(m.profile_pattern_repeated);
        assert!(!m.role_shape_recurring);
        // The legacy axes stay false.
        assert!(!m.tool_call_count);
    }

    #[test]
    fn signals_matched_reports_role_shape_recurring_when_threshold_crossed() {
        let signals = TurnSignals {
            recent_scope_denied_count: 3,
            ..TurnSignals::default()
        };
        let config = HeuristicConfig::default(); // default min=2
        let m = signals_matched(&signals, &config);
        assert!(m.role_shape_recurring);
        assert!(!m.profile_pattern_repeated);
    }

    #[test]
    fn signals_matched_phase_118_signals_below_threshold_report_false() {
        let signals = TurnSignals {
            keyword_key_prior_total_count: 4, // one short
            recent_scope_denied_count: 1,     // one short
            ..TurnSignals::default()
        };
        let config = HeuristicConfig::default();
        let m = signals_matched(&signals, &config);
        assert!(!m.profile_pattern_repeated);
        assert!(!m.role_shape_recurring);
    }

    #[test]
    fn audit_outcome_disabled_maps_cleanly() {
        let (outcome, name, conf) =
            audit_outcome_from(&SkillProposerOutcome::Disabled, None);
        assert!(matches!(
            outcome,
            aivyx_audit::SkillAutoProposalOutcomeSummary::Disabled
        ));
        assert!(name.is_none());
        assert!(conf.is_none());
    }

    #[test]
    fn audit_outcome_heuristic_gated_maps_cleanly() {
        let (outcome, name, conf) =
            audit_outcome_from(&SkillProposerOutcome::HeuristicGated, None);
        assert!(matches!(
            outcome,
            aivyx_audit::SkillAutoProposalOutcomeSummary::HeuristicGated
        ));
        assert!(name.is_none());
        assert!(conf.is_none());
    }

    #[test]
    fn audit_outcome_judge_error_carries_message() {
        let (outcome, name, conf) = audit_outcome_from(
            &SkillProposerOutcome::JudgeError("provider: HTTP 429".into()),
            None,
        );
        match outcome {
            aivyx_audit::SkillAutoProposalOutcomeSummary::JudgeError {
                error_message,
            } => {
                assert_eq!(error_message, "provider: HTTP 429");
            }
            _ => panic!("expected JudgeError"),
        }
        assert!(name.is_none());
        assert!(conf.is_none());
    }

    #[test]
    fn audit_outcome_auto_accept_carries_name_and_confidence() {
        let verdict = worth_proposing_verdict(0.91);
        let routing = decide_routing(
            &verdict,
            &[],
            &SkillAutoProposeConfig::default(),
        );
        let (outcome, name, conf) = audit_outcome_from(
            &SkillProposerOutcome::Verdict(verdict.clone()),
            Some(&routing),
        );
        assert!(matches!(
            outcome,
            aivyx_audit::SkillAutoProposalOutcomeSummary::AutoAccepted
        ));
        assert_eq!(name.as_deref(), Some("research-topic"));
        assert_eq!(conf, Some(910));
    }

    #[test]
    fn audit_outcome_staged_carries_name_and_confidence() {
        let verdict = worth_proposing_verdict(0.72);
        let routing = decide_routing(
            &verdict,
            &[],
            &SkillAutoProposeConfig::default(),
        );
        let (outcome, name, conf) = audit_outcome_from(
            &SkillProposerOutcome::Verdict(verdict.clone()),
            Some(&routing),
        );
        assert!(matches!(
            outcome,
            aivyx_audit::SkillAutoProposalOutcomeSummary::Staged
        ));
        assert_eq!(name.as_deref(), Some("research-topic"));
        assert_eq!(conf, Some(720));
    }

    #[test]
    fn audit_outcome_dup_llm_carries_dup_name() {
        let mut verdict = worth_proposing_verdict(0.95);
        verdict.is_worth_proposing = false;
        verdict.is_duplicate_of = Some("summarize-pdf".into());
        let routing = decide_routing(
            &verdict,
            &existing_skills_fixture(),
            &SkillAutoProposeConfig::default(),
        );
        let (outcome, _name, _conf) = audit_outcome_from(
            &SkillProposerOutcome::Verdict(verdict),
            Some(&routing),
        );
        match outcome {
            aivyx_audit::SkillAutoProposalOutcomeSummary::DuplicateOfExistingLlm {
                duplicate_of,
            } => {
                assert_eq!(duplicate_of, "summarize-pdf");
            }
            _ => panic!("expected DuplicateOfExistingLlm"),
        }
    }

    #[test]
    fn audit_outcome_dup_fuzzy_carries_matched_name() {
        let verdict = worth_proposing_verdict_with_name(0.95, "summarize-pdf");
        let routing = decide_routing(
            &verdict,
            &existing_skills_fixture(),
            &SkillAutoProposeConfig::default(),
        );
        let (outcome, _name, _conf) = audit_outcome_from(
            &SkillProposerOutcome::Verdict(verdict),
            Some(&routing),
        );
        match outcome {
            aivyx_audit::SkillAutoProposalOutcomeSummary::DuplicateOfExistingFuzzy {
                matched_existing_name,
            } => {
                assert_eq!(matched_existing_name, "summarize-pdf");
            }
            _ => panic!("expected DuplicateOfExistingFuzzy"),
        }
    }

    // ----- Phase 113 Task 3 — From<aivyx_config::SkillAutoProposeConfig> -----

    #[test]
    fn from_aivyx_config_maps_every_field() {
        let config_side = aivyx_config::SkillAutoProposeConfig {
            enabled: true,
            heuristic: aivyx_config::SkillsAutoProposeHeuristic {
                tool_call_count_min: 5,
                distinct_tool_id_min: 4,
                duration_ms_min: 9000,
                require_gate_resolve: true,
                mode: aivyx_config::SkillsAutoProposeMatchMode::All,
            },
            judge_model: Some("claude-opus-4-7".into()),
            judge_max_tokens: 1200,
            auto_accept_confidence_threshold: 0.91,
            fuzzy_match_threshold: 0.65,
        };
        let runtime: SkillAutoProposeConfig = config_side.into();
        assert!(runtime.enabled);
        assert_eq!(runtime.judge_model, "claude-opus-4-7");
        assert_eq!(runtime.judge_max_tokens, 1200);
        assert!((runtime.auto_accept_confidence_threshold - 0.91).abs() < 1e-6);
        assert!((runtime.fuzzy_match_threshold - 0.65).abs() < 1e-6);
        assert_eq!(runtime.heuristic.tool_call_count_min, 5);
        assert_eq!(runtime.heuristic.distinct_tool_id_min, 4);
        assert_eq!(runtime.heuristic.duration_ms_min, 9000);
        assert!(runtime.heuristic.require_gate_resolve);
        assert_eq!(runtime.heuristic.mode, MatchMode::All);
    }

    #[test]
    fn from_aivyx_config_persona_auto_propose_populates_per_category() {
        let cfg = aivyx_config::PersonaAutoProposeConfig {
            enabled: true,
            heuristic: aivyx_config::SkillsAutoProposeHeuristic {
                tool_call_count_min: 3,
                distinct_tool_id_min: 2,
                duration_ms_min: 5000,
                require_gate_resolve: false,
                mode: aivyx_config::SkillsAutoProposeMatchMode::Any,
            },
            judge_model: Some("m".into()),
            judge_max_tokens: 800,
            fuzzy_match_threshold: 0.80,
            per_category: aivyx_config::PerCategoryConfigSet::defaults(),
            from_failed_turns: false,
            failure_outcomes: aivyx_config::FailureOutcomesConfig::default(),
        };
        let runtime: SkillAutoProposeConfig = cfg.into();
        let pc = runtime.per_category.expect("per_category populated");
        // Scalar defaults — off
        assert!(!pc.assistant_name.enabled);
        assert!(!pc.operator_profile.enabled);
        assert!(!pc.communication_style.enabled);
        // List defaults — on
        assert!(pc.behavioral_preferences.enabled);
        assert!(pc.learned_skill.enabled);
        assert!(pc.character_traits.enabled);
        // Lookup helper works
        assert!(pc.lookup("LearnedSkill").is_some());
        assert!(pc.lookup("AssistantName").is_some());
        assert!(pc.lookup("NotARealCategory").is_none());
        // Phase 118 — recognized labels with the defaults
        // policy (enabled=true; threshold honored at parse
        // but ignored at routing).
        assert!(pc.profile_hint.enabled);
        assert!(pc.role_definition_suggestion.enabled);
        assert!(pc.lookup("ProfileHint").is_some());
        assert!(pc.lookup("RoleDefinitionSuggestion").is_some());
    }

    #[test]
    fn from_aivyx_config_skill_auto_propose_leaves_per_category_none() {
        // Phase 113 alias path: SkillAutoProposeConfig (skill-only)
        // → SkillAutoProposeConfig with `per_category: None`. The
        // runtime treats None as Phase 113 single-config posture.
        let cfg = aivyx_config::SkillAutoProposeConfig {
            enabled: true,
            heuristic: aivyx_config::SkillsAutoProposeHeuristic {
                tool_call_count_min: 3,
                distinct_tool_id_min: 2,
                duration_ms_min: 5000,
                require_gate_resolve: false,
                mode: aivyx_config::SkillsAutoProposeMatchMode::Any,
            },
            judge_model: Some("m".into()),
            judge_max_tokens: 800,
            auto_accept_confidence_threshold: 0.90,
            fuzzy_match_threshold: 0.80,
        };
        let runtime: SkillAutoProposeConfig = cfg.into();
        assert!(runtime.per_category.is_none());
        assert!((runtime.auto_accept_confidence_threshold - 0.90).abs() < 1e-6);
    }

    #[test]
    fn from_aivyx_config_maps_any_mode() {
        let config_side = aivyx_config::SkillAutoProposeConfig {
            enabled: false,
            heuristic: aivyx_config::SkillsAutoProposeHeuristic {
                tool_call_count_min: 1,
                distinct_tool_id_min: 1,
                duration_ms_min: 1,
                require_gate_resolve: false,
                mode: aivyx_config::SkillsAutoProposeMatchMode::Any,
            },
            judge_model: Some("x".into()),
            judge_max_tokens: 1,
            auto_accept_confidence_threshold: 0.0,
            fuzzy_match_threshold: 0.0,
        };
        let runtime: SkillAutoProposeConfig = config_side.into();
        assert!(!runtime.enabled);
        assert_eq!(runtime.heuristic.mode, MatchMode::Any);
    }

    #[test]
    fn audit_outcome_not_worth_proposing_maps_cleanly() {
        let verdict = JudgeResponse {
            is_worth_proposing: false,
            confidence: 0.30,
            category: None,
            proposed_draft: None,
            is_duplicate_of: None,
            reasoning: None,
        };
        let routing = decide_routing(
            &verdict,
            &[],
            &SkillAutoProposeConfig::default(),
        );
        let (outcome, name, conf) = audit_outcome_from(
            &SkillProposerOutcome::Verdict(verdict),
            Some(&routing),
        );
        assert!(matches!(
            outcome,
            aivyx_audit::SkillAutoProposalOutcomeSummary::NotWorthProposing
        ));
        assert!(name.is_none());
        assert_eq!(conf, Some(300));
    }
}
