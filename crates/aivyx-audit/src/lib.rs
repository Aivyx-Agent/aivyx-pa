//! # aivyx-audit
//!
//! HMAC-chained append-only audit log for Aivyx agents.
//!
//! Every tool call, scope check, and turn outcome is appended to this log
//! synchronously as it happens — not batched at turn end. If the process
//! crashes mid-turn, the audit log still tells the truth about what got
//! executed.
//!
//! See DESIGN.md Deliverable 1 (audit is synchronous, inline, HMAC-chained)
//! and Deliverable 4 (the 5-variant `AuditEvent` enum — per-tool for
//! grants, per-scope for denials, with a dedicated `MemoryAccess` view).
//!
//! ## The chain property
//!
//! Each entry's MAC is computed over `prev_mac || canonical_bytes(event)`.
//! Tampering with any entry invalidates every subsequent MAC, so the chain
//! itself is the integrity proof — no per-entry signature required.
//!
//! The canonical bytes come from `serde_jcs` (RFC 8785 JSON
//! Canonicalization Scheme), so byte-identical output is guaranteed across
//! runs regardless of struct field declaration order.
//!
//! ## Phase 1 scope
//!
//! In-memory `HmacChainLog` only. Disk persistence will come when
//! `aivyx-storage`'s `KeyDomain::Audit` is wired in a later phase.

use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use thiserror::Error;

use aivyx_capability::{CapabilitySet, Scope};
use aivyx_core::{
    ChannelPlatform, SessionId, TokenUsage, ToolId, ToolOutcomeSummary, TurnId,
    TurnOutcomeSummary,
};

type HmacSha256 = Hmac<Sha256>;

/// Versioned seed for the genesis (pre-entry-0) MAC. Mirrors D7's versioned
/// HKDF salt — bumping to `"aivyx-audit-v2-genesis"` produces a different
/// chain lineage, enabling clean format rotation without in-place migration.
const GENESIS_SEED: &[u8] = b"aivyx-audit-v1-genesis";

// ---------------------------------------------------------------------------
// AuditEvent — the 5 variants from D4
// ---------------------------------------------------------------------------

/// The set of events appended to the audit log. Per D4:
///
/// - `ToolCall` — primary key `tool_id`; covers every tool execution
/// - `ScopeDenied` — primary key `scope`; every denied capability check
/// - `TurnStarted` / `TurnEnded` — paired via `turn_id` for correlation
/// - `MemoryAccess` — redundant with `ToolCall` but indexed for fast
///   memory-specific queries (D4's one deliberate deviation from strict
///   mixed naming)
///
/// Every variant is *self-contained* — readable without cross-referencing
/// other entries — so a single entry can be displayed in a UI or printed to
/// a log without joining against siblings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum AuditEvent {
    /// A tool executed.
    ToolCall {
        turn_id: TurnId,
        tool_id: ToolId,
        scope_used: Scope,
        /// SHA-256 of the raw input. D4: "hash, not raw input — secrets safety."
        input_hash: [u8; 32],
        outcome: ToolOutcomeSummary,
        duration: Duration,
        /// Phase 120 — verbatim name the LLM originally emitted
        /// when the planner auto-corrected via fuzzy match.
        /// `None` for the dominant case (model emitted a
        /// registered name verbatim). `#[serde(default,
        /// skip_serializing_if = "Option::is_none")]` preserves
        /// HMAC-chain byte-identical canonical JSON for
        /// pre-Phase-120 entries (Phase 92 / Phase 117 /
        /// Phase 118 wire-compat precedent).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        auto_corrected_from: Option<String>,
        /// Phase 126 — wrapper-tag identifier (`"tool_code"` or
        /// `"tool_call"`) when the planner extracted this call
        /// from response TEXT. `None` for the dominant case
        /// (call came through the LLM provider's protocol
        /// channel). Same `#[serde(default, skip_serializing_if)]`
        /// pattern as `auto_corrected_from` preserves HMAC-
        /// chain byte-identical canonical JSON for pre-
        /// Phase-126 entries.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        extracted_from_text: Option<String>,
    },

    /// A scope check denied a tool call.
    ScopeDenied {
        turn_id: TurnId,
        tool_attempted: ToolId,
        scope_requested: Scope,
        /// Snapshot of capabilities at denial time — not a reference, so the
        /// set is preserved even if the agent's caps change later.
        held_capabilities: CapabilitySet,
    },

    /// Chapter Throttle (TH.3) — a tool call blocked by a `[rate_limit]` cap.
    /// Distinct from `ScopeDenied` (capability / role) so a forensic walk
    /// separates throttled from unauthorized; `reason` names the breached limit
    /// and window. The same call's `ToolCall` entry carries a `RateLimited`
    /// outcome summary.
    RateLimited {
        turn_id: TurnId,
        tool_attempted: ToolId,
        tool: String,
        reason: String,
    },

    /// Turn started. Correlates with `TurnEnded` via `turn_id`.
    TurnStarted {
        turn_id: TurnId,
        session_id: SessionId,
        channel: ChannelPlatform,
        trust_tier: TrustTierSummary,
        /// The `agent_caps.intersect(tier_ceiling)` snapshot — authoritative
        /// for the whole turn, per D5.
        effective_capabilities: CapabilitySet,
    },

    /// Turn ended. Paired with `TurnStarted`.
    TurnEnded {
        turn_id: TurnId,
        outcome: TurnOutcomeSummary,
        tool_calls_made: usize,
        duration: Duration,
        usage: TokenUsage,
    },

    /// LLM spend for a turn (Chapter K — cost governance). A dedicated,
    /// **additive** view of an LLM-backed turn's token usage *plus the model*
    /// it ran on — the model `TurnEnded` deliberately doesn't carry, so the
    /// cost report can price each turn precisely. Emitted only for LLM-backed
    /// turns (deterministic planners report no model). The priced dollar
    /// figure is derived at report time from a `Pricing` table, so a later
    /// rate correction re-prices history without rewriting the chain.
    LlmCost {
        turn_id: TurnId,
        /// The model the turn ran on (e.g. `claude-opus-4-8`, `llama3.1`).
        model: String,
        usage: TokenUsage,
    },

    /// Model routing — the router picked `model` (`id@endpoint`) for a
    /// routing-tagged LLM call (task kind `task`, e.g. `chat`), with the
    /// router's human-readable `reason` (including any fallback note).
    /// Additive: an internally tagged variant, so existing entries
    /// canonicalize unchanged. `session_id` is omitted when `None` (the
    /// router's record carries no session today).
    ModelRouted {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        model: String,
        task: String,
        reason: String,
    },

    /// Model routing Part 3b — conversation `session_id` was first marked
    /// routing-tainted: it touched sensitive data (a sensitive tool's
    /// output, operator-private memory recall, a sensitive channel), so it
    /// never escalates to a cloud endpoint. `reason` is a short label,
    /// never content. Written once per session, on the first mark.
    /// Additive: an internally tagged variant, so existing entries
    /// canonicalize unchanged.
    ConversationTainted { session_id: String, reason: String },

    /// Dedicated view of a memory operation. Redundant with `ToolCall`
    /// (every memory op *is* also a tool call), but indexed for fast
    /// memory-specific queries. D4 justifies this as the one deviation
    /// from strict mixed-model naming.
    MemoryAccess {
        turn_id: TurnId,
        operation: MemoryOperation,
        scope: Scope,
        /// Free-form filter or key — for a recall this is the query string,
        /// for a write it's the storage key. Not hashed: memory queries /
        /// keys are already audit-safe (no raw secrets pass through them
        /// by convention).
        query_or_key: String,
    },

    /// Phase 67 — daemon-initiated auto-notify on a trigger fire.
    /// Distinct from `ToolCall` (which records agent-initiated
    /// `notify.send` calls) so forensic searches can tell apart
    /// "the agent decided to notify" from "the daemon's
    /// trigger-config sugar decided to notify."
    ///
    /// Correlation: `session_id` is recorded on the corresponding
    /// `TurnStarted` audit event from the same trigger fire — walk
    /// backward through the chain to find the matching turn.
    /// `turn_id` correlation is a Phase 67 deferral
    /// (`TurnOutcome` doesn't carry `turn_id` today; lifting it
    /// touches 120 match sites).
    /// Phase 117 — `skills.invoke` successful invocation.
    /// Emitted alongside the regular `ToolCall` audit entry
    /// for the same call so the skill name lands in cleartext
    /// without exposing the rest of the tool input. Phase 116's
    /// `record_turn_outcomes` reads this variant to populate
    /// per-skill ledger rows; the operator-side `aivyx-pa audit
    /// export --event-type SkillInvocation` filter accepts the
    /// label.
    SkillInvocation {
        /// The turn that fired the invocation. Pair with the
        /// surrounding `TurnStarted` / `TurnEnded` via turn_id.
        turn_id: TurnId,
        /// The session whose turn fired it. Matches the
        /// surrounding `TurnStarted`.
        session_id: SessionId,
        /// The skill's stable kebab-case identifier from
        /// `LearnedSkill::name`.
        skill_name: String,
    },
    AutoNotifyDispatched {
        /// Session id minted by `TriggerDispatch::fire` for this
        /// trigger fire. Matches the `TurnStarted` /
        /// `TurnEnded` events from the same fire.
        session_id: SessionId,
        /// Which trigger kind fired (cron / webhook / file-watch).
        trigger_kind: TriggerKindSummary,
        /// Operator-declared id of the trigger that fired (e.g.
        /// `"morning-summary"`).
        trigger_id: String,
        /// Target name from `[[notify_target]]` that the auto-
        /// notify dispatched (or attempted to dispatch) to.
        target_name: String,
        /// What happened. Three forms: delivered, skipped because
        /// the agent's turn produced an empty response, failed
        /// during dispatch.
        outcome: AutoNotifyOutcomeSummary,
        /// Wall-clock timestamp of the dispatch attempt, ms since
        /// the Unix epoch. The chain's per-entry `appended_at` is
        /// the canonical audit timestamp; this is the moment the
        /// dispatcher was called, included for operator readability.
        dispatched_at_unix_ms: u64,
    },

    /// Phase 112 Task 6 — Skill Auto-Proposer fire record.
    ///
    /// Emitted once per turn where the auto-proposer ran past
    /// the heuristic gate. The outcome carries which terminal
    /// routing decision the proposer reached (or which failure
    /// mode it hit). Pair with the surrounding `TurnEnded`
    /// event via session_id to reconstruct what the agent
    /// learned (or didn't) from that turn.
    ///
    /// Confidence is stored as `confidence_thousandths` (a u32
    /// in `0..=1000`) rather than `f32` so the variant can stay
    /// `Eq` like the rest of `AuditEvent`. Read as `f32` via
    /// `confidence_thousandths as f32 / 1000.0`.
    SkillAutoProposal {
        /// The session whose turn fired the proposer. Matches
        /// the surrounding `TurnStarted` / `TurnEnded`.
        session_id: SessionId,
        /// Terminal routing outcome — what the proposer decided
        /// to do (or what error it hit).
        outcome: SkillAutoProposalOutcomeSummary,
        /// Judge confidence × 1000. Stored as integer to keep
        /// `AuditEvent: Eq` per the chain's invariant. None
        /// for outcomes that didn't reach the judge call
        /// (heuristic-gated, disabled, fuzzy-dropped pre-judge
        /// — note: Phase 112 runs fuzzy post-judge, so fuzzy
        /// dups DO have a confidence).
        confidence_thousandths: Option<u32>,
        /// Operator-readable name of the proposed draft. For
        /// `LearnedSkill` this is the kebab-case slug; for
        /// list/scalar categories (Phase 114) this is a
        /// truncated value. None for outcomes that didn't
        /// produce a draft.
        proposed_skill_name: Option<String>,
        /// Wall-clock duration of the judge call (Q1b stage 2).
        /// None for outcomes that didn't reach the judge.
        judge_latency_ms: Option<u64>,
        /// Which heuristic signals (Q1b stage 1) crossed
        /// during the candidate-gate evaluation. Lets forensic
        /// walks answer "what kind of turns are firing the
        /// proposer the most?" by tallying signal patterns.
        heuristic_signals_matched: HeuristicSignalsMatched,
        /// Phase 114 — the `PersonaDeltaCategory` label the
        /// judge picked. `None` for pre-Phase-114 entries
        /// (the field uses `#[serde(default,
        /// skip_serializing_if = "Option::is_none")]` so old
        /// chain entries round-trip byte-identically — Phase
        /// 92's `supersedes_proposal_id` precedent for wire-
        /// compatible audit-chain extensions).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        category: Option<String>,
        /// Phase 115 — what triggered this auto-proposer
        /// fire: a Phase 114 positive-pattern (Completed
        /// turn) or a Phase 115 negative-feedback path
        /// (failed turn). `None` for pre-Phase-115 entries;
        /// the absence of the field is semantically the
        /// CompletedTurn default. Same wire-compat pattern
        /// as the Phase 114 `category` field.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<ProposalSourceSummary>,
    },

    /// Phase 119 — operator-side application of an approved
    /// `ProfileHint` proposal. Distinct from
    /// `PersonaProposalResolved` (which records the *approve*
    /// gesture that lands the chain entry); this event records
    /// the *act-on-approval* gesture that mutates
    /// `aivyx-pa.toml`'s `[profile]` section.
    ///
    /// Forensic walks can answer "the operator approved this
    /// hint AND acted on it" definitively by pairing this
    /// event with the source proposal via `proposal_id`.
    ///
    /// Wire shape mirrors Phase 117 `SkillInvocation` and the
    /// Phase 112-114 `SkillAutoProposal` patterns: every
    /// field is owned + Eq + Serialize, so chain HMAC is
    /// computed over canonical JSON without surprises.
    ProfileHintApplied {
        /// The session whose CLI invocation fired the apply.
        /// Pair with the surrounding `TurnStarted` /
        /// `TurnEnded` via session_id for full operator-
        /// gesture context.
        session_id: SessionId,
        /// The proposal id this apply acted on. Links the
        /// apply event to the source `PersonaProposalResolved`
        /// and the upstream `SkillAutoProposal` for the
        /// proposer fire that drafted it.
        proposal_id: String,
        /// Which declared `[profile]` field the apply mutated.
        /// Matches `ProfileField::label()` from
        /// `aivyx_core::skill_proposer` (one of
        /// `"assistant_name"`, `"operator_profile"`,
        /// `"communication_style"`, `"primary_use_cases"`,
        /// `"behavioral_preferences"`,
        /// `"behavioral_constraints"`).
        field: String,
        /// The value the operator approved + the apply wrote
        /// into `aivyx-pa.toml`. For scalar fields this is the
        /// new scalar value; for list fields this is the
        /// appended entry (apply does not delete; it adds).
        applied_value: String,
    },

    /// Phase 119 — operator-side import of an approved
    /// `RoleDefinitionSuggestion` proposal. Mirrors
    /// `ProfileHintApplied` for the second Phase 118
    /// category; records the act-on-approval gesture that
    /// adds a `[roles.<name>]` section to `aivyx-pa.toml`.
    RoleDraftImported {
        /// The session whose CLI invocation fired the import.
        session_id: SessionId,
        /// Source proposal id for forensic linkage.
        proposal_id: String,
        /// The kebab-case role name the import wrote. Matches
        /// `RoleDraft::name`.
        role_name: String,
        /// The parent role the import declared
        /// `inherits_from = "<parent>"` for, if any. `None`
        /// indicates a top-level role (no parent chain).
        /// `#[serde(default, skip_serializing_if = ...)]`
        /// preserves wire-compat: an absent field in a
        /// future read decodes as `None`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<String>,
    },

    /// Chapter H — a human-approval gate was *refused* because the run is
    /// headless (no operator present to answer it). The audit-chain twin of
    /// the in-memory refusal the run already records (the single-agent turn
    /// finalizes `Escalated`, the team step → `Rejected`, the trigger
    /// mission → cancelled). Headless v1 only ever refuses at a gate — it
    /// never auto-approves — so this event is the canonical, queryable
    /// "what the operator would have been asked, and we declined on their
    /// behalf" record. Extends Phase 78's autonomous-action-must-stay-legible
    /// posture to the unattended path: an operator reviewing the chain later
    /// sees exactly what was declined and why.
    ///
    /// Every field is owned + `Eq` + `Serialize`, so the chain HMAC is
    /// computed over canonical JSON without surprises (the Phase 117 /
    /// Phase 119 precedent).
    HeadlessRefusal {
        /// Correlation key for the refused run. For an agent turn or a
        /// trigger fire this is the `SessionId` (string form) recorded on
        /// the surrounding `TurnStarted` / `TurnEnded`; for a team mission
        /// it is the mission id. A `String` so the one variant spans all
        /// three surfaces.
        run_id: String,
        /// Which run path hit the gate — the "what kind of run" axis.
        surface: HeadlessSurfaceSummary,
        /// The escalation's reason, verbatim — the "why-refused" an operator
        /// would have been shown to approve.
        reason: String,
    },

    /// Chapter U — an operator changed a config section via the Settings IPC
    /// (`SetAccessLevel` / `SetBudget`) — the daemon's first config-**write**
    /// path. The audit-chain record of a settings mutation: which `aivyx-pa.toml`
    /// section was rewritten and a human-readable summary of the new value(s).
    ///
    /// Self-contained per D4 (readable without joining siblings). The change is
    /// written to disk but is load-time — it takes effect on the next daemon
    /// start, so this records *what was set*, not a runtime state transition.
    /// Every field is owned + `Eq` + `Serialize`, so the chain HMAC is computed
    /// over canonical JSON without surprises (the HeadlessRefusal precedent).
    ConfigChanged {
        /// The `aivyx-pa.toml` section rewritten: `"access"` or `"budget"`.
        section: String,
        /// Human-readable summary of the new value(s), e.g.
        /// `"access level = home"` or
        /// `"per_run_usd = 5, per_day_usd = none, on_exceeded = deny"`.
        summary: String,
    },

    /// Chapter W — the operator's onboarding Persona/Skills seed
    /// (`[persona_seed]`) was planted on the persona chain at first boot. The
    /// audit-chain record that the agent's *learned* identity started from an
    /// operator-authored seed rather than an empty chain — fires once (the
    /// seed only applies to an empty chain). Records the shape, not the seeded
    /// content (the deltas themselves are on the persona chain).
    ///
    /// Self-contained per D4; every field is owned + `Eq` + `Serialize`, so the
    /// chain HMAC is computed over canonical JSON without surprises.
    PersonaSeeded {
        /// Number of seed deltas appended to the persona chain.
        entries: u64,
        /// Comma-joined category labels seeded, e.g.
        /// `"learned_context, character_traits, skill"`.
        categories: String,
    },

    /// Chapter DW — an operator-initiated filesystem mutation from the Studio's
    /// Documents editor (the second web write surface). Records *what* changed so
    /// the Command-Center feed + forensic walks see web-initiated file writes
    /// alongside the agent's own `fs.*` tool calls.
    ///
    /// Self-contained per D4; owned + `Eq` + `Serialize` fields, so the chain
    /// HMAC is computed over canonical JSON without surprises.
    DocumentMutated {
        /// `"write" | "delete" | "rename" | "mkdir"`.
        op: String,
        /// `"workspace"` or `"fs"` — which document root.
        root: String,
        /// The relative path affected (for rename: `"old -> new"`).
        path: String,
    },

    /// Chapter Chime — a schedule (cron routine) was created, updated,
    /// or deleted outside the config file: by the operator from the
    /// Studio's Schedules screen, or by the agent through the
    /// `schedule.create` / `schedule.cancel` tools (whose `ToolCall`
    /// events this complements with the *which schedule* detail).
    ScheduleMutated {
        /// `"create" | "update" | "delete"`.
        op: String,
        /// The storage id (`agt-`-prefixed when agent-created).
        schedule_id: String,
        /// `"operator"` or `"agent"`.
        actor: String,
    },

    /// Piece C (2026-08-23) — a channel's native `/team run <goal>`
    /// command successfully started a new team mission. Distinct from
    /// `Trigger` (that variant is specifically for *refused* headless
    /// trigger runs) and from `TeamMission` (that variant is a gate
    /// event on an already-running mission) — this is the one-time
    /// "a channel started a brand-new mission" audit record.
    TeamMissionChannelTriggered {
        /// The originating channel platform (`"telegram"` /
        /// `"discord"` / `"slack"`), lower-case, matching
        /// `daemon_server.rs`'s own platform-tag convention.
        platform: String,
        goal: String,
        mission_id: String,
    },

    /// Piece C follow-up (2026-08-24) — a channel's native `/team run
    /// <goal>` command was refused (the channel isn't opted into
    /// `team_run_channel`). Sibling of `TeamMissionChannelTriggered` —
    /// same shape minus `mission_id` (nothing was created), plus
    /// `reason` for the refusal.
    TeamMissionChannelDenied {
        /// Same convention as `TeamMissionChannelTriggered::platform`.
        platform: String,
        goal: String,
        reason: String,
    },
}

/// Chapter H — which headless run path produced a [`AuditEvent::HeadlessRefusal`].
/// Lets a forensic walk tell apart "an interactive turn opted into headless"
/// from "a team mission step hit a human gate" from "an operator-absent
/// trigger fired", each of which refuses for a structurally different reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum HeadlessSurfaceSummary {
    /// The interactive single-agent turn path, run headless — either
    /// `SubmitInput { headless: true }` (per-run opt-in) or the daemon-wide
    /// `RejectAndAbort` policy.
    AgentTurn,
    /// A Chapter L team mission whose step reached a `GateMode::Human` gate
    /// with no operator to resolve it.
    TeamMission {
        /// The step id that requested the approval gate.
        step: String,
    },
    /// An operator-absent trigger dispatch (loop / cron / webhook /
    /// file-watch / reflection) — all of which default to headless.
    Trigger {
        /// Which trigger kind fired the refused run.
        trigger_kind: TriggerKindSummary,
    },
}

/// Phase 115 — discriminator for what triggered an auto-
/// proposer fire. Mirrors
/// `aivyx_core::skill_proposer::ProposalSource` but lives
/// in `aivyx-audit` so the chain shape stays independent of
/// `aivyx-core`'s skill-proposer evolution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ProposalSourceSummary {
    /// Phase 114 positive-pattern path: `TurnOutcome::
    /// Completed`. Default for entries that omit the source
    /// field on read.
    CompletedTurn,
    /// Phase 115 negative-feedback path: a non-Completed
    /// `TurnOutcome`. The `failure_kind` field carries the
    /// specific variant (`"failed"` / `"cancelled"` /
    /// `"timed_out"` / `"escalated"`).
    FailedTurn { failure_kind: String },
}

/// Phase 112 — Skill Auto-Proposer outcome discriminator.
/// Mirrors `aivyx_channel::skill_auto_proposer::SkillProposerOutcome`
/// fused with the routing-decision label space (Task 5's
/// `SkillRoutingDecision::label()`). Lives here in `aivyx-audit`
/// so the chain shape stays independent of `aivyx-channel`'s
/// orchestration layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum SkillAutoProposalOutcomeSummary {
    /// Master switch was off; the proposer was bypassed.
    Disabled,
    /// Heuristic gate rejected the turn — no LLM call fired.
    HeuristicGated,
    /// Judge fired and judged the candidate worth proposing,
    /// confidence reached the auto-accept threshold, no dup
    /// detected on either the LLM-semantic or the fuzzy-title
    /// pre-filter. Landed in the LearnedSkill chain as an
    /// approved entry.
    AutoAccepted,
    /// Judge fired and judged the candidate worth proposing,
    /// but confidence was below the auto-accept threshold.
    /// Landed in the proposal chain as Pending — the
    /// operator will resolve via `aivyx-pa persona proposals`.
    Staged,
    /// Judge declared the candidate a semantic duplicate of an
    /// existing skill. Nothing written.
    DuplicateOfExistingLlm { duplicate_of: String },
    /// Title fuzzy-match against existing skills caught a
    /// paraphrase / token-reorder the judge missed. Nothing
    /// written.
    DuplicateOfExistingFuzzy { matched_existing_name: String },
    /// Judge said `is_worth_proposing == false` (and not a
    /// dup). Nothing written.
    NotWorthProposing,
    /// Judge call failed (provider error, parse failure, or
    /// confidence out of range). Carries the error message
    /// for operator forensics.
    JudgeError { error_message: String },
}

/// Phase 112 — Bitmap-style record of which heuristic signals
/// (Q1b stage 1) crossed their thresholds during candidate
/// gating. All fields are booleans, but we use a struct
/// rather than a `Vec<String>` so the audit chain stays
/// schema-stable and forensic queries can be exact-match
/// rather than substring.
///
/// Phase 118 — extended with two operator-staged refinement
/// signals for the Profile/Role auto-proposer paths. Both are
/// `#[serde(default)]` so old chain entries (Phase 112-117)
/// round-trip unchanged: absent field decodes as `false`,
/// matching the pre-Phase-118 behavior where these signals
/// did not exist. Phase 92 `supersedes_proposal_id`
/// wire-compat precedent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeuristicSignalsMatched {
    pub tool_call_count: bool,
    pub distinct_tool_id_count: bool,
    pub duration: bool,
    pub gate_resolve: bool,
    /// Phase 118 — `true` when the current turn's keyword_key
    /// (Phase 116) has been observed in the relevance ledger
    /// with a prior cumulative outcome count at or above the
    /// `profile_pattern_recurrence_min` threshold. Signals
    /// that the operator's request shape repeats — the
    /// Profile/Role auto-proposer treats this as a candidate
    /// for a `ProfileHint` proposal.
    #[serde(default, skip_serializing_if = "is_false")]
    pub profile_pattern_repeated: bool,
    /// Phase 118 — `true` when the recent session window
    /// (turns visible in the audit log) contains
    /// `ScopeDenied` events at or above the
    /// `role_shape_scope_denied_min` threshold. Signals
    /// that the current role's tool_allowlist /
    /// system_prompt envelope doesn't fit the operator's
    /// request shape — the Profile/Role auto-proposer
    /// treats this as a candidate for a
    /// `RoleDefinitionSuggestion` proposal.
    #[serde(default, skip_serializing_if = "is_false")]
    pub role_shape_recurring: bool,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(b: &bool) -> bool {
    !*b
}

/// Phase 67 — auto-notify outcome discriminator.
///
/// Mirrors `aivyx_channel::trigger`'s three post-turn dispatch
/// paths: dispatch succeeded, dispatch deliberately skipped
/// because the turn produced an empty body, dispatch failed.
/// Carried as the `outcome` field of [`AuditEvent::AutoNotifyDispatched`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum AutoNotifyOutcomeSummary {
    /// The dispatcher backend returned `Ok(())`. The notification
    /// has been delivered to the target (or at least handed off
    /// to it — for webhooks, "delivered" means the endpoint
    /// returned 2xx).
    Delivered,
    /// The turn outcome's body was empty so the dispatcher was
    /// deliberately not called (Phase 63 Q2(a) at sign-off).
    /// Recording this in the chain means operators can answer
    /// "why didn't my notification arrive?" definitively.
    SkippedEmptyResponse,
    /// The dispatcher backend returned an error. `error_kind`
    /// mirrors the `notify.send` tool's classification:
    /// `"transport"`, `"auth"`, `"rejected"`, `"timeout"`,
    /// `"unknown_target"`.
    Failed {
        error_kind: String,
        error_message: String,
    },
    /// Phase 72 — the trigger's `notify_when` condition gate
    /// evaluated to false against the turn outcome, so the
    /// dispatcher was deliberately skipped. `condition` carries
    /// the stable string label (`"on_failed"`,
    /// `"on_completed_non_empty"`) so forensic searches can
    /// answer "why didn't this fire?" definitively.
    SkippedByCondition { condition: String },
    /// Phase 73 — the target's in-memory rate-limit token bucket
    /// was exhausted when this dispatch was attempted, so the
    /// backend call was deliberately skipped. `limit` and
    /// `window_secs` carry the effective policy at the time of
    /// the skip so audit forensics can answer "what was the
    /// rate limit when this was skipped?" without needing the
    /// live config.
    SkippedByRateLimit { limit: u32, window_secs: u64 },
}

/// Phase 67 — trigger kind label for the audit chain. Mirrors
/// `aivyx_channel::trigger::TriggerSource`; duplicated in this
/// crate to avoid a dep edge from `aivyx-audit` to
/// `aivyx-channel` (the audit chain shape is independent of the
/// channel adapter substrate).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum TriggerKindSummary {
    Cron,
    Webhook,
    FileWatch,
    /// Phase 71 — reflection-scheduler fire. Distinct from
    /// `Cron` so forensic searches can tell "the agent
    /// reflected on its own behavior" apart from "an operator-
    /// declared cron job ran." Reflection turns carry the
    /// canonical reflection prompt + outcome-summary input.
    Reflection,
    /// Phase 173 — autonomous-loop fire (the Aivyx Ralph loop).
    /// Distinct from `Cron` / `Reflection` so forensic searches
    /// can isolate the autonomous, code-committing loop's
    /// iterations: every `TriggerSource::Loop` turn is one
    /// fresh-context pass over the backlog.
    Loop,
    /// Chapter Herald — a team mission reached a terminal phase.
    /// See `aivyx_channel::trigger::TriggerSource::Mission`.
    Mission,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryOperation {
    Read,
    Write,
    Forget,
}

/// Mirror of `aivyx_capability::TrustTier` — duplicated here to avoid a
/// dependency from audit on capability's concrete enum, and because the
/// audit record only needs the tier *name*, not its behavior. The conversion
/// is one-way: `TrustTierSummary::from(tier)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrustTierSummary {
    Kernel,
    Trusted,
    SemiTrusted,
    Untrusted,
}

impl From<aivyx_capability::TrustTier> for TrustTierSummary {
    fn from(t: aivyx_capability::TrustTier) -> Self {
        use aivyx_capability::TrustTier;
        match t {
            TrustTier::Kernel => TrustTierSummary::Kernel,
            TrustTier::Trusted => TrustTierSummary::Trusted,
            TrustTier::SemiTrusted => TrustTierSummary::SemiTrusted,
            TrustTier::Untrusted => TrustTierSummary::Untrusted,
        }
    }
}

// ---------------------------------------------------------------------------
// Signed entries and the chain
// ---------------------------------------------------------------------------

/// One signed entry in the chain. The MAC binds the entry to everything
/// preceding it via `prev_mac`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedEntry {
    /// Monotonic sequence number, 0-based. Not derived from the MAC — kept
    /// explicit so tools can reference entries by seq without computing the
    /// full chain.
    pub seq: u64,
    /// Wall-clock time of append, for human display. NOT part of the MAC
    /// input — clock skew must not break integrity.
    pub appended_at: SystemTime,
    pub event: AuditEvent,
    /// 32-byte HMAC-SHA256 tag over `prev_mac || canonical_bytes(event)`.
    pub mac: [u8; 32],
    /// The previous entry's MAC (or the genesis seed for entry 0).
    pub prev_mac: [u8; 32],
}

// ---------------------------------------------------------------------------
// AuditError
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum AuditError {
    #[error("canonical serialization failed: {0}")]
    Serialize(String),

    #[error("chain verification failed at seq {seq}: {reason}")]
    ChainBroken { seq: u64, reason: String },

    #[error("lock poisoned")]
    LockPoisoned,

    /// A put/get/scan call into `aivyx-storage` failed while reading or
    /// writing `KeyDomain::Audit`. Phase 7 task 1 wraps the upstream
    /// `StorageError` as a string so `aivyx-audit`'s public error
    /// surface does not leak storage internals to dependents.
    #[error("storage error: {0}")]
    Storage(String),

    /// An on-disk record could not be decoded, or its key/seq fields
    /// disagreed with its position in the scan. Surfaces at
    /// `PersistentAuditLog::open` only — once reopened, the in-memory
    /// chain is the source of truth.
    #[error("corrupt stored entry at seq {seq}: {reason}")]
    CorruptStoredEntry { seq: u64, reason: String },

    /// The persisted chain anchor (last known `seq` + `mac`, updated
    /// after every durably-persisted append — see `persistent.rs`'s
    /// module docs, invariant 6) disagrees with the real, freshly
    /// re-verified tail found on disk: either fewer entries exist than
    /// the anchor claims (`disk_seq` lower than `anchor_seq`, or
    /// `None` on a fully-emptied store), or the same `anchor_seq` is
    /// present but its MAC no longer matches. Distinct from
    /// `ChainBroken`/`CorruptStoredEntry`, which are both derived
    /// purely from *internal* consistency of whatever currently
    /// happens to be on disk — a scan-position enumeration with the
    /// tail cut off is, by construction, indistinguishable from "the
    /// log never grew past that point" without this external anchor
    /// to compare against.
    #[error(
        "audit chain tail truncation detected: anchor claims last seq {anchor_seq} \
         but the real on-disk tail is {disk_seq:?}"
    )]
    TailTruncated {
        anchor_seq: u64,
        disk_seq: Option<u64>,
    },
}

// ---------------------------------------------------------------------------
// AuditWriter / AuditLog traits
// ---------------------------------------------------------------------------

/// Minimal append surface — what a `ToolContext` will hold a reference to.
/// Returned by `HmacChainLog` and by `NullAuditLog`.
pub trait AuditWriter: Send + Sync {
    /// Append an event. Synchronous per D1: "blocking, microseconds per
    /// call." Returns the sequence number assigned, or an error on failure.
    fn append(&self, event: AuditEvent) -> Result<u64, AuditError>;
}

/// Full audit surface — extends `AuditWriter` with read/verify. The turn
/// loop holds an `AuditWriter` reference; test code and admin tools hold
/// an `AuditLog` reference for inspection.
pub trait AuditLog: AuditWriter {
    /// Read the entry at `seq`, or `None` if past the end.
    fn get(&self, seq: u64) -> Option<SignedEntry>;

    /// Number of entries so far.
    fn len(&self) -> usize;

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Walk the chain from entry 0 and recompute every MAC. Returns
    /// `Ok(())` if all MACs match and every `prev_mac` refers to the
    /// previous entry's MAC; `Err(ChainBroken)` at the first discrepancy.
    fn verify(&self) -> Result<(), AuditError>;
}

// ---------------------------------------------------------------------------
// HmacChainLog — the concrete in-memory HMAC-chained implementation.
// ---------------------------------------------------------------------------

/// In-memory HMAC-chained audit log.
///
/// The secret key is held by value; in a real deployment it's derived via
/// HKDF from the master key (D7 `KeyDomain::Audit`). For Phase 1, callers
/// pass in a key directly — storage wiring comes later.
pub struct HmacChainLog {
    key: Vec<u8>,
    inner: Mutex<Inner>,
}

struct Inner {
    entries: Vec<SignedEntry>,
}

impl HmacChainLog {
    pub fn new(key: impl Into<Vec<u8>>) -> Self {
        HmacChainLog {
            key: key.into(),
            inner: Mutex::new(Inner {
                entries: Vec::new(),
            }),
        }
    }

    /// Construct a log pre-populated with entries recovered from durable
    /// storage.
    ///
    /// **Caller must have already verified the chain** (`AuditLog::verify`
    /// semantics) over `entries` against `key` before calling this. The
    /// constructor inserts them *as-is* into the in-memory entry vec
    /// without recomputing MACs — which is the only way to honour the
    /// invariant that in-memory entries are byte-identical to what the
    /// reopen path blessed on disk. Recomputing here would hide any
    /// tamper the caller's verify missed.
    ///
    /// Intended exclusively for `PersistentAuditLog::open`'s reopen path.
    /// Subsequent `append` calls chain off the last entry's `mac` as
    /// usual, so seq numbering continues monotonically from
    /// `entries.len()`.
    pub fn from_verified_entries(
        key: impl Into<Vec<u8>>,
        entries: Vec<SignedEntry>,
    ) -> Self {
        HmacChainLog {
            key: key.into(),
            inner: Mutex::new(Inner { entries }),
        }
    }

    /// Snapshot of all entries — cloned. Intended for tests and admin
    /// tools, not for hot paths.
    pub fn entries(&self) -> Result<Vec<SignedEntry>, AuditError> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| AuditError::LockPoisoned)?
            .entries
            .clone())
    }

    /// Ranged snapshot — clone at most `limit` entries starting at
    /// `from_seq`. Returns an empty vec if `from_seq` is past the end.
    ///
    /// Phase 47 — used by the daemon's `ListAuditEntries` query to
    /// satisfy the Web UI audit viewer without ever materializing the
    /// full chain into a single response. Caller-supplied `limit` is
    /// capped by the daemon at 500 (Phase 47 Q3); this method itself
    /// imposes no upper bound — short reads are returned verbatim.
    pub fn entries_range(
        &self,
        from_seq: u64,
        limit: usize,
    ) -> Result<Vec<SignedEntry>, AuditError> {
        let inner = self.inner.lock().map_err(|_| AuditError::LockPoisoned)?;
        let start = from_seq as usize;
        if start >= inner.entries.len() {
            return Ok(Vec::new());
        }
        let end = (start + limit).min(inner.entries.len());
        Ok(inner.entries[start..end].to_vec())
    }

    fn compute_mac(&self, prev_mac: &[u8; 32], event_bytes: &[u8]) -> [u8; 32] {
        let mut mac = <HmacSha256 as KeyInit>::new_from_slice(&self.key)
            .expect("HMAC accepts any key length");
        mac.update(prev_mac);
        mac.update(event_bytes);
        let out = mac.finalize().into_bytes();
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&out);
        arr
    }
}

impl AuditWriter for HmacChainLog {
    fn append(&self, event: AuditEvent) -> Result<u64, AuditError> {
        let event_bytes =
            serde_jcs::to_vec(&event).map_err(|e| AuditError::Serialize(e.to_string()))?;

        let mut inner = self.inner.lock().map_err(|_| AuditError::LockPoisoned)?;

        let seq = inner.entries.len() as u64;
        let prev_mac = match inner.entries.last() {
            Some(prev) => prev.mac,
            None => {
                let mut seed = [0u8; 32];
                let src = GENESIS_SEED;
                // Left-pad: copy the seed into the *end* of the array; leading
                // zeros fill the rest. Deterministic and future-proof against
                // lengthening the seed string.
                let start = seed.len() - src.len();
                seed[start..].copy_from_slice(src);
                seed
            }
        };
        let mac = self.compute_mac(&prev_mac, &event_bytes);

        let entry = SignedEntry {
            seq,
            appended_at: SystemTime::now(),
            event,
            mac,
            prev_mac,
        };
        inner.entries.push(entry);
        Ok(seq)
    }
}

impl AuditLog for HmacChainLog {
    fn get(&self, seq: u64) -> Option<SignedEntry> {
        self.inner.lock().ok()?.entries.get(seq as usize).cloned()
    }

    fn len(&self) -> usize {
        self.inner
            .lock()
            .map(|i| i.entries.len())
            .unwrap_or(0)
    }

    fn verify(&self) -> Result<(), AuditError> {
        let entries = self
            .inner
            .lock()
            .map_err(|_| AuditError::LockPoisoned)?
            .entries
            .clone();

        let mut expected_prev = {
            let mut seed = [0u8; 32];
            let src = GENESIS_SEED;
            let start = seed.len() - src.len();
            seed[start..].copy_from_slice(src);
            seed
        };

        for (idx, entry) in entries.iter().enumerate() {
            if entry.seq != idx as u64 {
                return Err(AuditError::ChainBroken {
                    seq: idx as u64,
                    reason: format!("seq field = {}, expected {}", entry.seq, idx),
                });
            }
            if entry.prev_mac != expected_prev {
                return Err(AuditError::ChainBroken {
                    seq: entry.seq,
                    reason: "prev_mac does not match previous entry's mac".into(),
                });
            }
            let bytes = serde_jcs::to_vec(&entry.event)
                .map_err(|e| AuditError::Serialize(e.to_string()))?;
            let expected_mac = self.compute_mac(&expected_prev, &bytes);
            if expected_mac != entry.mac {
                return Err(AuditError::ChainBroken {
                    seq: entry.seq,
                    reason: "MAC does not match recomputation over canonical event bytes".into(),
                });
            }
            expected_prev = entry.mac;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// NullAuditLog — no-op writer for tests that don't need integrity.
// ---------------------------------------------------------------------------

/// Appends are accepted and dropped. `len()` always reports 0; `verify()`
/// always succeeds. Intended for fake `ToolContext` wiring in Phase 1
/// task 4 where the test cares about control flow, not audit.
pub struct NullAuditLog;

impl AuditWriter for NullAuditLog {
    fn append(&self, _event: AuditEvent) -> Result<u64, AuditError> {
        Ok(0)
    }
}

impl AuditLog for NullAuditLog {
    fn get(&self, _seq: u64) -> Option<SignedEntry> {
        None
    }

    fn len(&self) -> usize {
        0
    }

    fn verify(&self) -> Result<(), AuditError> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Bridge: aivyx_core::AuditHook → aivyx_audit::AuditWriter
// ---------------------------------------------------------------------------
//
// `aivyx-core` declares a forward `AuditHook` trait with an `AuditTag` enum
// so the turn loop can emit audit events without depending on this crate.
// The bridge below closes the loop: any `AuditWriter` (e.g. `HmacChainLog`)
// can be wrapped in an `AuditBridge` and handed to `ConcreteAgent::new` as
// an `Arc<dyn AuditHook>`.
//
// Why an explicit adapter and not a blanket `impl<W: AuditWriter> AuditHook
// for W`? D4 commits to audit failures being *visible* — not swallowed. A
// blanket impl has nowhere to route an `AuditError`, so the bridge forces
// callers to declare their error-handling strategy at construction time.
// The default (`AuditBridge::new`) panics, matching D4's "audit must
// complete before the call returns" spirit: a broken chain is a
// configuration bug, not an operational condition.

/// Translates `aivyx_core::MemoryOperation` into the audit-owned copy. The
/// two enums are kept separate so core does not depend on audit's serde
/// machinery; this impl is the single translation point.
impl From<aivyx_core::MemoryOperation> for MemoryOperation {
    fn from(op: aivyx_core::MemoryOperation) -> Self {
        match op {
            aivyx_core::MemoryOperation::Read => MemoryOperation::Read,
            aivyx_core::MemoryOperation::Write => MemoryOperation::Write,
            aivyx_core::MemoryOperation::Forget => MemoryOperation::Forget,
        }
    }
}

/// Translates a forward-declared `AuditTag` from the turn loop into the
/// `AuditEvent` shape the HMAC chain appends. The translation is mostly
/// field-for-field — only `trust_tier` and `operation` need conversion
/// through their respective `From` impls.
impl From<aivyx_core::AuditTag> for AuditEvent {
    fn from(tag: aivyx_core::AuditTag) -> Self {
        use aivyx_core::AuditTag;
        match tag {
            AuditTag::TurnStarted {
                turn_id,
                session_id,
                channel,
                trust_tier,
                effective_capabilities,
            } => AuditEvent::TurnStarted {
                turn_id,
                session_id,
                channel,
                trust_tier: trust_tier.into(),
                effective_capabilities,
            },
            AuditTag::TurnEnded {
                turn_id,
                outcome,
                tool_calls_made,
                duration,
                usage,
            } => AuditEvent::TurnEnded {
                turn_id,
                outcome,
                tool_calls_made,
                duration,
                usage,
            },
            AuditTag::LlmCost {
                turn_id,
                model,
                usage,
            } => AuditEvent::LlmCost {
                turn_id,
                model,
                usage,
            },
            AuditTag::ModelRouted {
                session_id,
                model,
                task,
                reason,
            } => AuditEvent::ModelRouted {
                session_id,
                model,
                task,
                reason,
            },
            AuditTag::ConversationTainted { session_id, reason } => {
                AuditEvent::ConversationTainted { session_id, reason }
            }
            AuditTag::ToolCall {
                turn_id,
                tool_id,
                scope_used,
                input_hash,
                outcome,
                duration,
                auto_corrected_from,
                extracted_from_text,
            } => AuditEvent::ToolCall {
                turn_id,
                tool_id,
                scope_used,
                input_hash,
                outcome,
                duration,
                auto_corrected_from,
                extracted_from_text,
            },
            AuditTag::ScopeDenied {
                turn_id,
                tool_attempted,
                scope_requested,
                held_capabilities,
            } => AuditEvent::ScopeDenied {
                turn_id,
                tool_attempted,
                scope_requested,
                held_capabilities,
            },
            AuditTag::RateLimited {
                turn_id,
                tool_attempted,
                tool,
                reason,
            } => AuditEvent::RateLimited {
                turn_id,
                tool_attempted,
                tool,
                reason,
            },
            AuditTag::MemoryAccess {
                turn_id,
                operation,
                scope,
                query_or_key,
            } => AuditEvent::MemoryAccess {
                turn_id,
                operation: operation.into(),
                scope,
                query_or_key,
            },
            AuditTag::SkillInvocation {
                turn_id,
                session_id,
                skill_name,
            } => AuditEvent::SkillInvocation {
                turn_id,
                session_id,
                skill_name,
            },
            AuditTag::HeadlessRefusal {
                run_id,
                step,
                reason,
            } => AuditEvent::HeadlessRefusal {
                run_id,
                surface: HeadlessSurfaceSummary::TeamMission { step },
                reason,
            },
        }
    }
}

/// Adapter that lets any `AuditWriter` satisfy `aivyx_core::AuditHook`.
///
/// Construct with [`AuditBridge::new`] for the D4-aligned panic-on-error
/// default, or with [`AuditBridge::with_error_handler`] to supply a custom
/// strategy (log, metric, soft-fail, etc.).
pub struct AuditBridge<W: AuditWriter> {
    writer: W,
    on_error: Box<dyn Fn(AuditError) + Send + Sync>,
    /// Audit L3 fix — count of `writer.append` failures the bridge
    /// has seen, regardless of the configured `on_error` strategy.
    /// Operators running a `with_error_handler` soft-fail policy
    /// (log-and-continue) can read this counter via
    /// [`failed_append_count`] to spot a degraded chain that the
    /// custom handler would otherwise hide. Atomic so the read
    /// path stays lock-free; `Relaxed` is sufficient — we're
    /// counting events, not synchronising on them.
    failed_appends: std::sync::atomic::AtomicU64,
}

impl<W: AuditWriter> AuditBridge<W> {
    /// Default bridge: panics on any `AuditError`. This matches D4's
    /// commitment that audit failures must be visible — a broken chain in
    /// a running agent is a misconfiguration, not something to log away.
    pub fn new(writer: W) -> Self {
        AuditBridge {
            writer,
            on_error: Box::new(|e| panic!("audit bridge: append failed: {e}")),
            failed_appends: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Escape hatch for callers that need a non-panic strategy (e.g. an
    /// ops dashboard where logging the error and keeping the process up is
    /// preferable to crashing mid-turn). Use sparingly — every error that
    /// this handler swallows is an invariant from D4 that no longer holds.
    pub fn with_error_handler(
        writer: W,
        on_error: impl Fn(AuditError) + Send + Sync + 'static,
    ) -> Self {
        AuditBridge {
            writer,
            on_error: Box::new(on_error),
            failed_appends: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Access the wrapped writer for verification / read-only queries.
    /// Used by tests that want to assert chain length or replay entries
    /// after a turn has run through the bridge.
    pub fn writer(&self) -> &W {
        &self.writer
    }

    /// Total `writer.append` failures the bridge has observed since
    /// construction. The bridge increments this *before* calling the
    /// configured `on_error` handler, so the counter reflects every
    /// failure — including ones swallowed by a soft-fail custom
    /// handler.
    ///
    /// Audit L3 fix — surfaces silent audit-write failures so
    /// operators running a `with_error_handler` log-and-continue
    /// strategy can spot a degraded chain. A non-zero value here
    /// means the HMAC chain is no longer a faithful record of the
    /// turn's tool calls and the operator should investigate
    /// (disk full, permissions changed, writer lock contention,
    /// etc.).
    pub fn failed_append_count(&self) -> u64 {
        self.failed_appends.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl<W: AuditWriter + 'static> aivyx_core::AuditHook for AuditBridge<W> {
    fn on_event(&self, tag: aivyx_core::AuditTag) {
        let event: AuditEvent = tag.into();
        if let Err(e) = self.writer.append(event) {
            // Audit L3 fix — increment *before* dispatching to the
            // configured handler. The default `new` constructor's
            // handler is `panic!`, so the counter only matters for
            // soft-fail custom handlers; incrementing before the
            // panic costs one atomic store and keeps a counter
            // truthful in the (rare) case someone inspects the
            // bridge inside a catch-unwind harness.
            self.failed_appends
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            (self.on_error)(e);
        }
    }
}

// ---------------------------------------------------------------------------
// PersistentAuditLog — Phase 7 task 1 durable wrapper
// ---------------------------------------------------------------------------

mod persistent;
pub use persistent::PersistentAuditLog;

// ---------------------------------------------------------------------------
// Utility: input_hash helper for ToolCall events.
// ---------------------------------------------------------------------------

/// Hash a raw tool input (as bytes) into the 32-byte digest stored in
/// `AuditEvent::ToolCall::input_hash`. Provided here so every call site
/// uses the same hash family; the output is SHA-256.
pub fn hash_tool_input(bytes: &[u8]) -> [u8; 32] {
    use sha2::Digest;
    let mut h = Sha256::new();
    h.update(bytes);
    let out = h.finalize();
    let mut arr = [0u8; 32];
    arr.copy_from_slice(&out);
    arr
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_capability::{CapabilitySet, Scope, TrustTier};

    fn test_key() -> Vec<u8> {
        b"phase1-test-key-do-not-ship".to_vec()
    }

    fn sample_scope() -> Scope {
        Scope::parse("memory.read:session:abc").unwrap()
    }

    fn sample_capset() -> CapabilitySet {
        CapabilitySet::from_scopes([
            Scope::parse("memory.read").unwrap(),
            Scope::parse("llm.call").unwrap(),
        ])
    }

    fn sample_turn_started() -> AuditEvent {
        AuditEvent::TurnStarted {
            turn_id: TurnId::new(),
            session_id: SessionId::new(),
            channel: ChannelPlatform::Local,
            trust_tier: TrustTierSummary::from(TrustTier::Trusted),
            effective_capabilities: sample_capset(),
        }
    }

    fn sample_tool_call() -> AuditEvent {
        AuditEvent::ToolCall {
            turn_id: TurnId::new(),
            tool_id: ToolId::new(),
            scope_used: sample_scope(),
            input_hash: hash_tool_input(b"{\"query\":\"yesterday\"}"),
            outcome: ToolOutcomeSummary::Completed {
                verified: aivyx_core::VerificationSummary::NotApplicable,
            },
            duration: Duration::from_millis(37),
            auto_corrected_from: None,
            extracted_from_text: None,
        }
    }

    // ---- Chain basics ----

    #[test]
    fn empty_chain_verifies() {
        let log = HmacChainLog::new(test_key());
        assert!(log.verify().is_ok());
        assert_eq!(AuditLog::len(&log), 0);
    }

    #[test]
    fn single_append_produces_seq_zero() {
        let log = HmacChainLog::new(test_key());
        let seq = log.append(sample_turn_started()).unwrap();
        assert_eq!(seq, 0);
        assert_eq!(AuditLog::len(&log), 1);
        log.verify().unwrap();
    }

    #[test]
    fn multi_append_produces_valid_chain() {
        let log = HmacChainLog::new(test_key());
        log.append(sample_turn_started()).unwrap();
        log.append(sample_tool_call()).unwrap();
        log.append(AuditEvent::TurnEnded {
            turn_id: TurnId::new(),
            outcome: TurnOutcomeSummary::Completed,
            tool_calls_made: 1,
            duration: Duration::from_secs(2),
            usage: TokenUsage::default(),
        })
        .unwrap();
        assert_eq!(AuditLog::len(&log), 3);
        log.verify().unwrap();
    }

    // ---- Phase 47 — entries_range ----

    #[test]
    fn entries_range_returns_requested_window() {
        let log = HmacChainLog::new(test_key());
        for _ in 0..5 {
            log.append(sample_tool_call()).unwrap();
        }
        // First two entries.
        let window = log.entries_range(0, 2).unwrap();
        assert_eq!(window.len(), 2);
        assert_eq!(window[0].seq, 0);
        assert_eq!(window[1].seq, 1);

        // Middle slice.
        let window = log.entries_range(2, 2).unwrap();
        assert_eq!(window.len(), 2);
        assert_eq!(window[0].seq, 2);
        assert_eq!(window[1].seq, 3);
    }

    #[test]
    fn entries_range_short_read_when_limit_exceeds_chain() {
        let log = HmacChainLog::new(test_key());
        log.append(sample_tool_call()).unwrap();
        log.append(sample_tool_call()).unwrap();
        // Asking for 10 from seq 1 should yield only 1.
        let window = log.entries_range(1, 10).unwrap();
        assert_eq!(window.len(), 1);
        assert_eq!(window[0].seq, 1);
    }

    #[test]
    fn entries_range_returns_empty_when_from_seq_past_end() {
        let log = HmacChainLog::new(test_key());
        log.append(sample_tool_call()).unwrap();
        let window = log.entries_range(5, 10).unwrap();
        assert!(window.is_empty());
    }

    #[test]
    fn entries_range_zero_limit_returns_empty() {
        let log = HmacChainLog::new(test_key());
        log.append(sample_tool_call()).unwrap();
        let window = log.entries_range(0, 0).unwrap();
        assert!(window.is_empty());
    }

    #[test]
    fn prev_mac_of_entry_n_matches_mac_of_entry_n_minus_1() {
        let log = HmacChainLog::new(test_key());
        log.append(sample_turn_started()).unwrap();
        log.append(sample_tool_call()).unwrap();
        let entries = log.entries().unwrap();
        assert_eq!(entries[1].prev_mac, entries[0].mac);
    }

    // ---- Tamper detection ----

    #[test]
    fn tampering_with_an_entry_breaks_chain() {
        let log = HmacChainLog::new(test_key());
        log.append(sample_turn_started()).unwrap();
        log.append(sample_tool_call()).unwrap();
        log.append(AuditEvent::TurnEnded {
            turn_id: TurnId::new(),
            outcome: TurnOutcomeSummary::Completed,
            tool_calls_made: 1,
            duration: Duration::from_secs(2),
            usage: TokenUsage::default(),
        })
        .unwrap();
        log.verify().unwrap();

        // Mutate entry 1's event in place. We reach into the Mutex for this
        // test only — real callers cannot do this because `entries()`
        // returns a clone.
        {
            let mut inner = log.inner.lock().unwrap();
            if let AuditEvent::ToolCall {
                ref mut duration, ..
            } = inner.entries[1].event
            {
                *duration = Duration::from_secs(999);
            } else {
                panic!("expected ToolCall at index 1");
            }
        }

        let err = log.verify().unwrap_err();
        match err {
            AuditError::ChainBroken { seq, .. } => {
                assert_eq!(seq, 1, "tamper on entry 1 must be detected at seq 1");
            }
            _ => panic!("expected ChainBroken, got {err:?}"),
        }
    }

    #[test]
    fn tampering_with_prev_mac_breaks_chain() {
        let log = HmacChainLog::new(test_key());
        log.append(sample_turn_started()).unwrap();
        log.append(sample_tool_call()).unwrap();

        {
            let mut inner = log.inner.lock().unwrap();
            inner.entries[1].prev_mac[0] ^= 0xFF;
        }

        let err = log.verify().unwrap_err();
        match err {
            AuditError::ChainBroken { seq, .. } => assert_eq!(seq, 1),
            _ => panic!("expected ChainBroken"),
        }
    }

    // ---- Canonical-bytes determinism across logs with the same key ----

    #[test]
    fn two_logs_same_key_same_events_produce_identical_macs() {
        // The "determinism that makes HMAC-chained audit useful" test:
        // build two separate logs from the same key, append the same
        // sequence of logically-equal events, and confirm the MAC chain
        // is identical byte-for-byte.
        let event_a = sample_turn_started();
        let event_b = match &event_a {
            AuditEvent::TurnStarted {
                turn_id,
                session_id,
                channel,
                trust_tier,
                effective_capabilities,
            } => AuditEvent::TurnStarted {
                turn_id: *turn_id,
                session_id: *session_id,
                channel: *channel,
                trust_tier: *trust_tier,
                effective_capabilities: effective_capabilities.clone(),
            },
            _ => unreachable!(),
        };

        let log1 = HmacChainLog::new(test_key());
        let log2 = HmacChainLog::new(test_key());
        log1.append(event_a).unwrap();
        log2.append(event_b).unwrap();

        let e1 = log1.entries().unwrap();
        let e2 = log2.entries().unwrap();
        assert_eq!(e1[0].mac, e2[0].mac, "same event + same key → same MAC");
        assert_eq!(e1[0].prev_mac, e2[0].prev_mac);
    }

    #[test]
    fn different_keys_produce_different_macs() {
        let log1 = HmacChainLog::new(b"key-one".to_vec());
        let log2 = HmacChainLog::new(b"key-two".to_vec());
        let ev = sample_turn_started();
        let ev2 = match &ev {
            AuditEvent::TurnStarted {
                turn_id,
                session_id,
                channel,
                trust_tier,
                effective_capabilities,
            } => AuditEvent::TurnStarted {
                turn_id: *turn_id,
                session_id: *session_id,
                channel: *channel,
                trust_tier: *trust_tier,
                effective_capabilities: effective_capabilities.clone(),
            },
            _ => unreachable!(),
        };
        log1.append(ev).unwrap();
        log2.append(ev2).unwrap();
        assert_ne!(
            log1.entries().unwrap()[0].mac,
            log2.entries().unwrap()[0].mac
        );
    }

    // ---- Round-trip all 5 variants ----

    #[test]
    fn all_five_variants_round_trip_through_canonical_json() {
        let turn_id = TurnId::new();

        let events = vec![
            sample_tool_call(),
            AuditEvent::ScopeDenied {
                turn_id,
                tool_attempted: ToolId::new(),
                scope_requested: Scope::parse("shell.exec:rm").unwrap(),
                held_capabilities: sample_capset(),
            },
            sample_turn_started(),
            AuditEvent::TurnEnded {
                turn_id,
                outcome: TurnOutcomeSummary::Cancelled,
                tool_calls_made: 2,
                duration: Duration::from_millis(500),
                usage: TokenUsage::default(),
            },
            AuditEvent::MemoryAccess {
                turn_id,
                operation: MemoryOperation::Read,
                scope: Scope::parse("memory.read:session:abc").unwrap(),
                query_or_key: "yesterday".to_string(),
            },
        ];

        for ev in events {
            let bytes = serde_jcs::to_vec(&ev).expect("jcs must accept");
            let back: AuditEvent = serde_json::from_slice(&bytes).expect("round trip");
            assert_eq!(ev, back);
        }
    }

    // ---- Phase 67 — AutoNotifyDispatched variant ----

    #[test]
    fn auto_notify_dispatched_round_trips_through_canonical_json() {
        let cases = vec![
            AuditEvent::AutoNotifyDispatched {
                session_id: SessionId::new(),
                trigger_kind: TriggerKindSummary::Cron,
                trigger_id: "morning-briefing".into(),
                target_name: "phone".into(),
                outcome: AutoNotifyOutcomeSummary::Delivered,
                dispatched_at_unix_ms: 1_715_000_000_000,
            },
            AuditEvent::AutoNotifyDispatched {
                session_id: SessionId::new(),
                trigger_kind: TriggerKindSummary::Webhook,
                trigger_id: "ci-events".into(),
                target_name: "ops-alerts".into(),
                outcome: AutoNotifyOutcomeSummary::SkippedEmptyResponse,
                dispatched_at_unix_ms: 1_715_000_000_001,
            },
            AuditEvent::AutoNotifyDispatched {
                session_id: SessionId::new(),
                trigger_kind: TriggerKindSummary::FileWatch,
                trigger_id: "notes-dir".into(),
                target_name: "phone".into(),
                outcome: AutoNotifyOutcomeSummary::Failed {
                    error_kind: "rejected".into(),
                    error_message: "HTTP 429".into(),
                },
                dispatched_at_unix_ms: 1_715_000_000_002,
            },
        ];

        for ev in cases {
            let bytes = serde_jcs::to_vec(&ev).expect("jcs serializes");
            let back: AuditEvent = serde_json::from_slice(&bytes).expect("round trip");
            assert_eq!(ev, back);
        }
    }

    #[test]
    fn auto_notify_outcome_summary_serializes_with_kind_tag() {
        // Sanity check: the #[serde(tag = "kind")] makes the
        // wire shape `{"kind": "Delivered"}` etc., which the
        // existing Web UI chain reader handles cleanly.
        let delivered = AutoNotifyOutcomeSummary::Delivered;
        let json = serde_json::to_value(&delivered).unwrap();
        assert_eq!(json["kind"], "Delivered");

        let failed = AutoNotifyOutcomeSummary::Failed {
            error_kind: "auth".into(),
            error_message: "HTTP 401".into(),
        };
        let json = serde_json::to_value(&failed).unwrap();
        assert_eq!(json["kind"], "Failed");
        assert_eq!(json["error_kind"], "auth");
        assert_eq!(json["error_message"], "HTTP 401");
    }

    #[test]
    fn trigger_kind_summary_serializes_with_kind_tag() {
        let cron = TriggerKindSummary::Cron;
        let json = serde_json::to_value(cron).unwrap();
        assert_eq!(json["kind"], "Cron");

        let webhook: TriggerKindSummary = serde_json::from_value(
            serde_json::json!({"kind": "Webhook"}),
        )
        .expect("Webhook variant parses");
        assert_eq!(webhook, TriggerKindSummary::Webhook);
    }

    // ---- Phase 117 — SkillInvocation variant ----

    #[test]
    fn skill_invocation_round_trips_through_canonical_json() {
        let ev = AuditEvent::SkillInvocation {
            turn_id: TurnId::new(),
            session_id: SessionId::new(),
            skill_name: "research-multi-source".into(),
        };
        let bytes = serde_jcs::to_vec(&ev).expect("jcs serializes");
        let back: AuditEvent =
            serde_json::from_slice(&bytes).expect("round trip");
        assert_eq!(ev, back);
    }

    #[test]
    fn skill_invocation_serializes_with_kind_tag() {
        let ev = AuditEvent::SkillInvocation {
            turn_id: TurnId::new(),
            session_id: SessionId::new(),
            skill_name: "x".into(),
        };
        let json = serde_json::to_value(&ev).unwrap();
        assert_eq!(json["kind"], "SkillInvocation");
        assert_eq!(json["skill_name"], "x");
    }

    #[test]
    fn skill_invocation_can_be_hmac_chained() {
        let log = HmacChainLog::new(test_key());
        log.append(sample_tool_call()).unwrap();
        log.append(AuditEvent::SkillInvocation {
            turn_id: TurnId::new(),
            session_id: SessionId::new(),
            skill_name: "research-topic".into(),
        })
        .unwrap();
        log.verify().unwrap();
        assert_eq!(AuditLog::len(&log), 2);
    }

    // ---- Phase 112 — SkillAutoProposal variant ----

    fn no_signals() -> HeuristicSignalsMatched {
        // Phase 118 — `Default` derive lets us write the
        // all-false fixture in one line. The two new Phase 118
        // bool fields default to `false` (matching the
        // pre-Phase-118 baseline).
        HeuristicSignalsMatched::default()
    }

    fn all_signals() -> HeuristicSignalsMatched {
        HeuristicSignalsMatched {
            tool_call_count: true,
            distinct_tool_id_count: true,
            duration: true,
            gate_resolve: true,
            profile_pattern_repeated: true,
            role_shape_recurring: true,
        }
    }

    #[test]
    fn skill_auto_proposal_round_trips_for_every_outcome() {
        let cases = vec![
            AuditEvent::SkillAutoProposal {
                session_id: SessionId::new(),
                outcome: SkillAutoProposalOutcomeSummary::Disabled,
                confidence_thousandths: None,
                proposed_skill_name: None,
                judge_latency_ms: None,
                heuristic_signals_matched: no_signals(),
                category: None,
                source: None,
            },
            AuditEvent::SkillAutoProposal {
                session_id: SessionId::new(),
                outcome: SkillAutoProposalOutcomeSummary::HeuristicGated,
                confidence_thousandths: None,
                proposed_skill_name: None,
                judge_latency_ms: None,
                heuristic_signals_matched: no_signals(),
                category: None,
                source: None,
            },
            AuditEvent::SkillAutoProposal {
                session_id: SessionId::new(),
                outcome: SkillAutoProposalOutcomeSummary::AutoAccepted,
                confidence_thousandths: Some(910),
                proposed_skill_name: Some("research-topic".into()),
                judge_latency_ms: Some(1450),
                heuristic_signals_matched: all_signals(),
                category: None,
                source: None,
            },
            AuditEvent::SkillAutoProposal {
                session_id: SessionId::new(),
                outcome: SkillAutoProposalOutcomeSummary::Staged,
                confidence_thousandths: Some(720),
                proposed_skill_name: Some("research-topic".into()),
                judge_latency_ms: Some(1320),
                heuristic_signals_matched: HeuristicSignalsMatched {
                    tool_call_count: true,
                    distinct_tool_id_count: true,
                    duration: false,
                    gate_resolve: false,
                    ..HeuristicSignalsMatched::default()
                },
                category: None,
                source: None,
            },
            AuditEvent::SkillAutoProposal {
                session_id: SessionId::new(),
                outcome:
                    SkillAutoProposalOutcomeSummary::DuplicateOfExistingLlm {
                        duplicate_of: "summarize-pdf".into(),
                    },
                confidence_thousandths: Some(960),
                proposed_skill_name: None,
                judge_latency_ms: Some(1100),
                heuristic_signals_matched: all_signals(),
                category: None,
                source: None,
            },
            AuditEvent::SkillAutoProposal {
                session_id: SessionId::new(),
                outcome:
                    SkillAutoProposalOutcomeSummary::DuplicateOfExistingFuzzy {
                        matched_existing_name: "summarize-doc".into(),
                    },
                confidence_thousandths: Some(880),
                proposed_skill_name: Some("summarize-pdf".into()),
                judge_latency_ms: Some(1200),
                heuristic_signals_matched: all_signals(),
                category: None,
                source: None,
            },
            AuditEvent::SkillAutoProposal {
                session_id: SessionId::new(),
                outcome: SkillAutoProposalOutcomeSummary::NotWorthProposing,
                confidence_thousandths: Some(300),
                proposed_skill_name: None,
                judge_latency_ms: Some(900),
                heuristic_signals_matched: all_signals(),
                category: None,
                source: None,
            },
            AuditEvent::SkillAutoProposal {
                session_id: SessionId::new(),
                outcome: SkillAutoProposalOutcomeSummary::JudgeError {
                    error_message: "provider: HTTP 429".into(),
                },
                confidence_thousandths: None,
                proposed_skill_name: None,
                judge_latency_ms: Some(420),
                heuristic_signals_matched: all_signals(),
                category: None,
                source: None,
            },
        ];

        for ev in cases {
            let bytes = serde_jcs::to_vec(&ev).expect("jcs serializes");
            let back: AuditEvent =
                serde_json::from_slice(&bytes).expect("round trip");
            assert_eq!(ev, back);
        }
    }

    #[test]
    fn skill_auto_proposal_outcome_summary_serializes_with_kind_tag() {
        let auto = SkillAutoProposalOutcomeSummary::AutoAccepted;
        let json = serde_json::to_value(&auto).unwrap();
        assert_eq!(json["kind"], "AutoAccepted");

        let dup = SkillAutoProposalOutcomeSummary::DuplicateOfExistingFuzzy {
            matched_existing_name: "x".into(),
        };
        let json = serde_json::to_value(&dup).unwrap();
        assert_eq!(json["kind"], "DuplicateOfExistingFuzzy");
        assert_eq!(json["matched_existing_name"], "x");

        let err = SkillAutoProposalOutcomeSummary::JudgeError {
            error_message: "boom".into(),
        };
        let json = serde_json::to_value(&err).unwrap();
        assert_eq!(json["kind"], "JudgeError");
        assert_eq!(json["error_message"], "boom");
    }

    // ---- Phase 114 — backward-compatible `category` field ----

    #[test]
    fn skill_auto_proposal_with_category_round_trips() {
        // Phase 114: a SkillAutoProposal with a populated
        // category field round-trips through canonical JSON.
        let ev = AuditEvent::SkillAutoProposal {
            session_id: SessionId::new(),
            outcome: SkillAutoProposalOutcomeSummary::AutoAccepted,
            confidence_thousandths: Some(910),
            proposed_skill_name: Some("prefer terse replies".into()),
            judge_latency_ms: Some(1450),
            heuristic_signals_matched: all_signals(),
            category: Some("BehavioralPreferences".into()),
            source: None,
        };
        let bytes = serde_jcs::to_vec(&ev).expect("jcs serializes");
        let back: AuditEvent =
            serde_json::from_slice(&bytes).expect("round trip");
        assert_eq!(ev, back);
    }

    #[test]
    fn skill_auto_proposal_without_category_round_trips_byte_identically() {
        // Phase 114 backward-compatibility: a SkillAutoProposal
        // with category=None serializes to canonical JSON that
        // OMITS the field entirely (per `skip_serializing_if`),
        // so a pre-Phase-114 entry decoded into the new struct
        // and re-serialized produces the same bytes.
        let ev = AuditEvent::SkillAutoProposal {
            session_id: SessionId::new(),
            outcome: SkillAutoProposalOutcomeSummary::AutoAccepted,
            confidence_thousandths: Some(910),
            proposed_skill_name: Some("research-topic".into()),
            judge_latency_ms: Some(1450),
            heuristic_signals_matched: all_signals(),
            category: None,
            source: None,
        };
        let bytes = serde_jcs::to_vec(&ev).expect("jcs serializes");
        // The JSON output should NOT contain "category" when None.
        let s = std::str::from_utf8(&bytes).unwrap();
        assert!(
            !s.contains("\"category\""),
            "category=None must serialize as absent field: {s}"
        );
        let back: AuditEvent =
            serde_json::from_slice(&bytes).expect("round trip");
        assert_eq!(ev, back);
    }

    #[test]
    fn skill_auto_proposal_decodes_pre_phase_114_entry_with_no_category() {
        // Simulates a Phase 112-113 chain entry: the JSON has
        // no "category" field. Decoding into the Phase 114
        // struct must succeed with category=None.
        let raw_pre_114 = serde_json::json!({
            "kind": "SkillAutoProposal",
            "session_id": SessionId::new(),
            "outcome": {"kind": "AutoAccepted"},
            "confidence_thousandths": 910u32,
            "proposed_skill_name": "research-topic",
            "judge_latency_ms": 1450u64,
            "heuristic_signals_matched": {
                "tool_call_count": true,
                "distinct_tool_id_count": true,
                "duration": true,
                "gate_resolve": true,
            },
        });
        let decoded: AuditEvent =
            serde_json::from_value(raw_pre_114).expect("decode");
        match decoded {
            AuditEvent::SkillAutoProposal { category, .. } => {
                assert!(category.is_none());
            }
            _ => panic!("expected SkillAutoProposal"),
        }
    }

    // ---- Phase 115 — `source` field backward-compat ----

    #[test]
    fn skill_auto_proposal_with_failed_turn_source_round_trips() {
        let ev = AuditEvent::SkillAutoProposal {
            session_id: SessionId::new(),
            outcome: SkillAutoProposalOutcomeSummary::AutoAccepted,
            confidence_thousandths: Some(910),
            proposed_skill_name: Some("never run rm -rf".into()),
            judge_latency_ms: Some(1450),
            heuristic_signals_matched: all_signals(),
            category: Some("BehavioralConstraints".into()),
            source: Some(ProposalSourceSummary::FailedTurn {
                failure_kind: "failed".into(),
            }),
        };
        let bytes = serde_jcs::to_vec(&ev).expect("jcs serializes");
        let back: AuditEvent =
            serde_json::from_slice(&bytes).expect("round trip");
        assert_eq!(ev, back);
    }

    #[test]
    fn skill_auto_proposal_without_source_round_trips_byte_identically() {
        // Phase 115 backward-compatibility: source=None
        // serializes as ABSENT field, so a pre-Phase-115
        // entry decoded into the new struct and re-encoded
        // produces the same canonical bytes.
        let ev = AuditEvent::SkillAutoProposal {
            session_id: SessionId::new(),
            outcome: SkillAutoProposalOutcomeSummary::AutoAccepted,
            confidence_thousandths: Some(910),
            proposed_skill_name: Some("research-topic".into()),
            judge_latency_ms: Some(1450),
            heuristic_signals_matched: all_signals(),
            category: None,
            source: None,
        };
        let bytes = serde_jcs::to_vec(&ev).expect("jcs serializes");
        let s = std::str::from_utf8(&bytes).unwrap();
        assert!(
            !s.contains("\"source\""),
            "source=None must serialize as absent field: {s}"
        );
        let back: AuditEvent =
            serde_json::from_slice(&bytes).expect("round trip");
        assert_eq!(ev, back);
    }

    #[test]
    fn skill_auto_proposal_decodes_pre_phase_115_entry_with_no_source() {
        // Simulates a Phase 112-114 chain entry: the JSON
        // has no "source" field. Decoding into the Phase
        // 115 struct must succeed with source=None.
        let raw_pre_115 = serde_json::json!({
            "kind": "SkillAutoProposal",
            "session_id": SessionId::new(),
            "outcome": {"kind": "AutoAccepted"},
            "confidence_thousandths": 910u32,
            "proposed_skill_name": "research-topic",
            "judge_latency_ms": 1450u64,
            "heuristic_signals_matched": {
                "tool_call_count": true,
                "distinct_tool_id_count": true,
                "duration": true,
                "gate_resolve": true,
            },
            "category": "LearnedSkill",
        });
        let decoded: AuditEvent =
            serde_json::from_value(raw_pre_115).expect("decode");
        match decoded {
            AuditEvent::SkillAutoProposal { source, .. } => {
                assert!(source.is_none());
            }
            _ => panic!("expected SkillAutoProposal"),
        }
    }

    // ----- Phase 118 — HeuristicSignalsMatched wire-compat -----

    #[test]
    fn pre_phase_118_heuristic_signals_decode_with_new_fields_false() {
        // Phase 112-117 chain entries have a 4-field
        // heuristic_signals_matched block. Decoding into the
        // Phase 118 struct must succeed with the two new
        // fields false (the `#[serde(default)]` attribute
        // supplies false for absent bools).
        let pre_118 = serde_json::json!({
            "tool_call_count": true,
            "distinct_tool_id_count": false,
            "duration": true,
            "gate_resolve": false,
        });
        let decoded: HeuristicSignalsMatched =
            serde_json::from_value(pre_118).expect("decode");
        assert!(decoded.tool_call_count);
        assert!(!decoded.distinct_tool_id_count);
        assert!(decoded.duration);
        assert!(!decoded.gate_resolve);
        // Phase 118 fields default to false.
        assert!(!decoded.profile_pattern_repeated);
        assert!(!decoded.role_shape_recurring);
    }

    #[test]
    fn phase_118_heuristic_signals_round_trip_via_json() {
        let original = HeuristicSignalsMatched {
            tool_call_count: true,
            distinct_tool_id_count: true,
            duration: false,
            gate_resolve: false,
            profile_pattern_repeated: true,
            role_shape_recurring: true,
        };
        let json = serde_json::to_value(original).unwrap();
        let parsed: HeuristicSignalsMatched =
            serde_json::from_value(json).unwrap();
        assert_eq!(parsed, original);
    }

    #[test]
    fn phase_118_heuristic_signals_skip_serialize_when_false() {
        // `#[serde(default, skip_serializing_if = "is_false")]`
        // keeps the wire form byte-identical to pre-Phase-118
        // chain entries when the new signals are both false.
        // Critical for HMAC-chain backward compatibility: a
        // mid-chain Phase 118 read of a Phase 117-written
        // entry must canonicalize identically.
        let all_false_pre_118_shape = HeuristicSignalsMatched {
            tool_call_count: true,
            distinct_tool_id_count: false,
            duration: false,
            gate_resolve: false,
            profile_pattern_repeated: false,
            role_shape_recurring: false,
        };
        let json = serde_json::to_value(all_false_pre_118_shape).unwrap();
        let obj = json.as_object().expect("object");
        // The two Phase 118 fields are NOT in the wire form.
        assert!(!obj.contains_key("profile_pattern_repeated"));
        assert!(!obj.contains_key("role_shape_recurring"));
        // The pre-Phase-118 four fields ARE present.
        assert!(obj.contains_key("tool_call_count"));
        assert!(obj.contains_key("distinct_tool_id_count"));
        assert!(obj.contains_key("duration"));
        assert!(obj.contains_key("gate_resolve"));
    }

    #[test]
    fn phase_118_heuristic_signals_emit_field_when_true() {
        // The skip_serializing_if only fires for false. When
        // either Phase 118 signal is true, the field appears
        // in the wire form so audit forensics can answer
        // "which signal crossed?" by reading the raw JSON.
        let only_profile = HeuristicSignalsMatched {
            profile_pattern_repeated: true,
            ..HeuristicSignalsMatched::default()
        };
        let json = serde_json::to_value(only_profile).unwrap();
        let obj = json.as_object().expect("object");
        assert!(obj.contains_key("profile_pattern_repeated"));
        assert!(!obj.contains_key("role_shape_recurring"));
    }

    #[test]
    fn proposal_source_summary_serializes_with_kind_tag() {
        let completed = ProposalSourceSummary::CompletedTurn;
        let json = serde_json::to_value(&completed).unwrap();
        assert_eq!(json["kind"], "CompletedTurn");

        let failed = ProposalSourceSummary::FailedTurn {
            failure_kind: "timed_out".into(),
        };
        let json = serde_json::to_value(&failed).unwrap();
        assert_eq!(json["kind"], "FailedTurn");
        assert_eq!(json["failure_kind"], "timed_out");
    }

    #[test]
    fn skill_auto_proposal_can_be_hmac_chained() {
        // Same proof-of-life test the other variants have: an
        // entry of the new variant lands in the HmacChainLog
        // without breaking the chain verification.
        let log = HmacChainLog::new(test_key());
        log.append(sample_tool_call()).unwrap();
        log.append(AuditEvent::SkillAutoProposal {
            session_id: SessionId::new(),
            outcome: SkillAutoProposalOutcomeSummary::AutoAccepted,
            confidence_thousandths: Some(910),
            proposed_skill_name: Some("research-topic".into()),
            judge_latency_ms: Some(1450),
            heuristic_signals_matched: all_signals(),
            category: None,
            source: None,
        })
        .unwrap();
        log.verify().unwrap();
        assert_eq!(AuditLog::len(&log), 2);
    }

    // ----- Phase 119 — ProfileHintApplied + RoleDraftImported -----

    #[test]
    fn profile_hint_applied_round_trips_via_serde_jcs() {
        // Wire-shape stability: every field round-trips
        // through canonical JSON so the HMAC chain hashes
        // bind to the same bytes pre- and post-serialize.
        let event = AuditEvent::ProfileHintApplied {
            session_id: SessionId::new(),
            proposal_id: "pp-phase118-hint".into(),
            field: "communication_style".into(),
            applied_value: "terse and bullet-formatted".into(),
        };
        let json = serde_json::to_value(&event).unwrap();
        let parsed: AuditEvent =
            serde_json::from_value(json).expect("decode");
        assert_eq!(parsed, event);
    }

    #[test]
    fn profile_hint_applied_serializes_with_kind_tag() {
        let event = AuditEvent::ProfileHintApplied {
            session_id: SessionId::new(),
            proposal_id: "pp-x".into(),
            field: "assistant_name".into(),
            applied_value: "Aivyx PA".into(),
        };
        let json = serde_json::to_value(&event).unwrap();
        // The `#[serde(tag = "kind")]` discriminator picks the
        // PascalCase variant name as the wire tag — operator-
        // readable forensic walks rely on this.
        assert_eq!(json["kind"], "ProfileHintApplied");
        assert_eq!(json["field"], "assistant_name");
        assert_eq!(json["applied_value"], "Aivyx PA");
        assert_eq!(json["proposal_id"], "pp-x");
    }

    #[test]
    fn role_draft_imported_round_trips_via_serde_jcs() {
        let event = AuditEvent::RoleDraftImported {
            session_id: SessionId::new(),
            proposal_id: "pp-phase118-role".into(),
            role_name: "research-deploy".into(),
            parent: Some("research".into()),
        };
        let json = serde_json::to_value(&event).unwrap();
        let parsed: AuditEvent =
            serde_json::from_value(json).expect("decode");
        assert_eq!(parsed, event);
    }

    #[test]
    fn role_draft_imported_with_no_parent_round_trips() {
        let event = AuditEvent::RoleDraftImported {
            session_id: SessionId::new(),
            proposal_id: "pp-y".into(),
            role_name: "operator-mode".into(),
            parent: None,
        };
        let json = serde_json::to_value(&event).unwrap();
        // `#[serde(skip_serializing_if = "Option::is_none")]`
        // keeps the wire form compact when no parent —
        // forensic readers don't see a redundant `null`.
        assert!(json.as_object().unwrap().get("parent").is_none());
        let parsed: AuditEvent =
            serde_json::from_value(json).expect("decode");
        assert_eq!(parsed, event);
    }

    #[test]
    fn role_draft_imported_serializes_with_kind_tag() {
        let event = AuditEvent::RoleDraftImported {
            session_id: SessionId::new(),
            proposal_id: "pp-z".into(),
            role_name: "n".into(),
            parent: None,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["kind"], "RoleDraftImported");
    }

    #[test]
    fn profile_hint_applied_can_be_hmac_chained() {
        // Proof-of-life: the variant lands in the chain
        // and chain verification still passes. Pair with
        // the surrounding ToolCall sample_tool_call to
        // confirm chain hashing is stable across mixed
        // variant types.
        let log = HmacChainLog::new(test_key());
        log.append(sample_tool_call()).unwrap();
        log.append(AuditEvent::ProfileHintApplied {
            session_id: SessionId::new(),
            proposal_id: "pp-1".into(),
            field: "communication_style".into(),
            applied_value: "terse".into(),
        })
        .unwrap();
        log.verify().unwrap();
        assert_eq!(AuditLog::len(&log), 2);
    }

    #[test]
    fn role_draft_imported_can_be_hmac_chained() {
        let log = HmacChainLog::new(test_key());
        log.append(sample_tool_call()).unwrap();
        log.append(AuditEvent::RoleDraftImported {
            session_id: SessionId::new(),
            proposal_id: "pp-2".into(),
            role_name: "x".into(),
            parent: Some("y".into()),
        })
        .unwrap();
        log.verify().unwrap();
        assert_eq!(AuditLog::len(&log), 2);
    }

    // ----- Phase 120 — AuditEvent::ToolCall.auto_corrected_from wire-compat -----

    #[test]
    fn pre_phase_120_tool_call_decodes_with_auto_corrected_from_none() {
        // Phase 119-and-earlier chain entries have no
        // auto_corrected_from field. The #[serde(default,
        // skip_serializing_if = ...)] attribute means an event
        // with None already serializes WITHOUT the field — that
        // wire form IS the pre-Phase-120 shape. Round-trip
        // through serialize-then-deserialize confirms the
        // #[serde(default)] supplies None for the absent field.
        let pre_120_shape = sample_tool_call();
        let json = serde_json::to_value(&pre_120_shape).unwrap();
        // The serialized form must NOT carry auto_corrected_from.
        let obj = json.as_object().expect("object");
        assert!(
            !obj.contains_key("auto_corrected_from"),
            "sample_tool_call() with None must serialize without the field"
        );
        // Decoding that same wire form into the Phase 120 struct
        // must succeed with None.
        let decoded: AuditEvent = serde_json::from_value(json).expect("decode");
        match decoded {
            AuditEvent::ToolCall {
                auto_corrected_from,
                ..
            } => {
                assert!(auto_corrected_from.is_none());
            }
            _ => panic!("expected ToolCall"),
        }
    }

    #[test]
    fn phase_120_tool_call_with_auto_correction_round_trips() {
        // Phase 120 hallucination case: model emitted `fs_read`,
        // planner auto-corrected to fs.read. The audit chain records
        // the verbatim original. Wire-shape stability: round-trip
        // through canonical JSON and back without loss.
        let event = AuditEvent::ToolCall {
            turn_id: TurnId::new(),
            tool_id: ToolId::new(),
            scope_used: sample_scope(),
            input_hash: hash_tool_input(b"{}"),
            outcome: ToolOutcomeSummary::Completed {
                verified: aivyx_core::VerificationSummary::NotApplicable,
            },
            duration: Duration::from_millis(15),
            auto_corrected_from: Some("fs_read".into()),
            extracted_from_text: Some("fs_read".into()),
        };
        let json = serde_json::to_value(&event).unwrap();
        // The Phase 120 field appears in the wire form.
        assert_eq!(json["auto_corrected_from"], "fs_read");
        let parsed: AuditEvent = serde_json::from_value(json).expect("decode");
        assert_eq!(parsed, event);
    }

    #[test]
    fn phase_120_tool_call_none_skips_serialize_for_chain_compat() {
        // `#[serde(default, skip_serializing_if = "Option::is_none")]`
        // keeps the wire form byte-identical to pre-Phase-120
        // entries when no auto-correction happened. Critical for
        // HMAC-chain backward compatibility: a mid-chain Phase 120
        // read of a Phase 119-written entry must canonicalize
        // identically.
        let event = AuditEvent::ToolCall {
            turn_id: TurnId::new(),
            tool_id: ToolId::new(),
            scope_used: sample_scope(),
            input_hash: hash_tool_input(b"{}"),
            outcome: ToolOutcomeSummary::Completed {
                verified: aivyx_core::VerificationSummary::NotApplicable,
            },
            duration: Duration::from_millis(15),
            auto_corrected_from: None,
            extracted_from_text: None,
        };
        let json = serde_json::to_value(&event).unwrap();
        let obj = json.as_object().expect("object");
        // Critical wire-compat assertion: the field is NOT in the
        // canonical-JSON form when None.
        assert!(
            !obj.contains_key("auto_corrected_from"),
            "Phase 120 None case must omit the field for HMAC-chain compat"
        );
    }

    #[test]
    fn phase_120_tool_call_with_auto_correction_can_be_hmac_chained() {
        // Proof-of-life: a ToolCall variant with the Phase 120 field
        // populated lands in the HmacChainLog and chain verification
        // still passes. The chain hashes over canonical JSON, so the
        // new field is part of the MAC computation when populated.
        let log = HmacChainLog::new(test_key());
        log.append(sample_tool_call()).unwrap();
        log.append(AuditEvent::ToolCall {
            turn_id: TurnId::new(),
            tool_id: ToolId::new(),
            scope_used: sample_scope(),
            input_hash: hash_tool_input(b"{}"),
            outcome: ToolOutcomeSummary::Completed {
                verified: aivyx_core::VerificationSummary::NotApplicable,
            },
            duration: Duration::from_millis(15),
            auto_corrected_from: Some("fs_read".into()),
            extracted_from_text: Some("fs_read".into()),
        })
        .unwrap();
        log.verify().unwrap();
        assert_eq!(AuditLog::len(&log), 2);
    }

    // ----- Phase 126 — AuditEvent::ToolCall.extracted_from_text wire-compat -----

    #[test]
    fn phase_126_tool_call_extracted_round_trips() {
        let event = AuditEvent::ToolCall {
            turn_id: TurnId::new(),
            tool_id: ToolId::new(),
            scope_used: sample_scope(),
            input_hash: hash_tool_input(b"{}"),
            outcome: ToolOutcomeSummary::Completed {
                verified: aivyx_core::VerificationSummary::NotApplicable,
            },
            duration: Duration::from_millis(8),
            auto_corrected_from: None,
            extracted_from_text: Some("tool_code".to_string()),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["extracted_from_text"], "tool_code");
        let decoded: AuditEvent = serde_json::from_value(json).unwrap();
        match decoded {
            AuditEvent::ToolCall {
                extracted_from_text,
                ..
            } => {
                assert_eq!(extracted_from_text.as_deref(), Some("tool_code"));
            }
            other => panic!("expected ToolCall; got {other:?}"),
        }
    }

    #[test]
    fn chapter_h_headless_refusal_round_trips_each_surface() {
        // H.6 — a headless gate refusal must serialize + decode
        // byte-stably across all three surfaces (the chain HMAC is
        // computed over canonical JSON). The `surface` is `#[serde(tag
        // = "kind")]` like the other summary enums.
        for surface in [
            HeadlessSurfaceSummary::AgentTurn,
            HeadlessSurfaceSummary::TeamMission {
                step: "draft".into(),
            },
            HeadlessSurfaceSummary::Trigger {
                trigger_kind: TriggerKindSummary::Loop,
            },
        ] {
            let event = AuditEvent::HeadlessRefusal {
                run_id: "sess-42".into(),
                surface: surface.clone(),
                reason: "kitchen.order.send would spend money (no operator)".into(),
            };
            let json = serde_json::to_value(&event).unwrap();
            assert_eq!(json["kind"], "HeadlessRefusal");
            let decoded: AuditEvent = serde_json::from_value(json).expect("decode");
            assert_eq!(decoded, event, "round-trip must be lossless for {surface:?}");
        }
    }

    #[test]
    fn model_routed_round_trips_through_canonical_json() {
        // Model routing — the router's decision lands on the chain, so it
        // must serialize + decode byte-stably like every other variant.
        for session_id in [None, Some("sess-1".to_string())] {
            let event = AuditEvent::ModelRouted {
                session_id: session_id.clone(),
                model: "big@gpu".into(),
                task: "chat".into(),
                reason: "chat prefers a medium model".into(),
            };
            let bytes = serde_jcs::to_vec(&event).expect("jcs must accept");
            let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(json["kind"], "ModelRouted");
            assert_eq!(json.get("session_id").is_some(), session_id.is_some());
            let decoded: AuditEvent = serde_json::from_slice(&bytes).expect("round trip");
            assert_eq!(decoded, event);
        }
    }

    #[test]
    fn model_routed_audit_tag_bridges_field_for_field() {
        let event: AuditEvent = aivyx_core::AuditTag::ModelRouted {
            session_id: None,
            model: "big@gpu".into(),
            task: "chat".into(),
            reason: "only candidate".into(),
        }
        .into();
        assert_eq!(
            event,
            AuditEvent::ModelRouted {
                session_id: None,
                model: "big@gpu".into(),
                task: "chat".into(),
                reason: "only candidate".into(),
            }
        );
    }

    #[test]
    fn conversation_tainted_round_trips_through_canonical_json() {
        let event = AuditEvent::ConversationTainted {
            session_id: "sess-1".into(),
            reason: "gmail.search output".into(),
        };
        let bytes = serde_jcs::to_vec(&event).expect("jcs must accept");
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["kind"], "ConversationTainted");
        assert_eq!(json["session_id"], "sess-1");
        assert_eq!(json["reason"], "gmail.search output");
        let decoded: AuditEvent = serde_json::from_slice(&bytes).expect("round trip");
        assert_eq!(decoded, event);
    }

    #[test]
    fn conversation_tainted_audit_tag_bridges_field_for_field() {
        let event: AuditEvent = aivyx_core::AuditTag::ConversationTainted {
            session_id: "sess-1".into(),
            reason: "memory recall".into(),
        }
        .into();
        assert_eq!(
            event,
            AuditEvent::ConversationTainted {
                session_id: "sess-1".into(),
                reason: "memory recall".into(),
            }
        );
    }

    #[test]
    fn chapter_h_team_audit_tag_bridges_to_team_mission_surface() {
        // The team driver only holds an `Arc<dyn AuditHook>`, so it
        // emits `AuditTag::HeadlessRefusal`; the bridge must land a
        // `TeamMission { step }` surface on the chain.
        let event: AuditEvent = aivyx_core::AuditTag::HeadlessRefusal {
            run_id: "mission-7".into(),
            step: "review".into(),
            reason: "human-approval gate at step 'review' (no operator)".into(),
        }
        .into();
        match event {
            AuditEvent::HeadlessRefusal {
                run_id, surface, ..
            } => {
                assert_eq!(run_id, "mission-7");
                assert_eq!(
                    surface,
                    HeadlessSurfaceSummary::TeamMission {
                        step: "review".into()
                    }
                );
            }
            other => panic!("expected HeadlessRefusal, got {other:?}"),
        }
    }

    #[test]
    fn phase_126_tool_call_none_skips_serialize_for_chain_compat() {
        // Same wire-compat invariant as Phase 120: when the field
        // is None, the canonical-JSON form omits it entirely so
        // pre-Phase-126 chain entries continue to verify against
        // the Phase 126 read path.
        let event = AuditEvent::ToolCall {
            turn_id: TurnId::new(),
            tool_id: ToolId::new(),
            scope_used: sample_scope(),
            input_hash: hash_tool_input(b"{}"),
            outcome: ToolOutcomeSummary::Completed {
                verified: aivyx_core::VerificationSummary::NotApplicable,
            },
            duration: Duration::from_millis(8),
            auto_corrected_from: None,
            extracted_from_text: None,
        };
        let json = serde_json::to_value(&event).unwrap();
        let obj = json.as_object().expect("object");
        assert!(
            !obj.contains_key("extracted_from_text"),
            "Phase 126 None case must omit the field for HMAC-chain compat"
        );
    }

    #[test]
    fn phase_126_tool_call_with_extraction_and_correction_compose() {
        // Both fields populated — extracted from text AND
        // fuzzy-corrected (gemma4 emitting `<tool_call>` with
        // hallucinated `fs.write_file` that Phase 120 fuzzy-
        // recovered to `fs.write`). The audit chain records
        // both forensically.
        let event = AuditEvent::ToolCall {
            turn_id: TurnId::new(),
            tool_id: ToolId::new(),
            scope_used: sample_scope(),
            input_hash: hash_tool_input(b"{}"),
            outcome: ToolOutcomeSummary::Completed {
                verified: aivyx_core::VerificationSummary::NotApplicable,
            },
            duration: Duration::from_millis(8),
            auto_corrected_from: Some("fs.write_file".to_string()),
            extracted_from_text: Some("tool_call".to_string()),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["auto_corrected_from"], "fs.write_file");
        assert_eq!(json["extracted_from_text"], "tool_call");

        // HMAC-chain proof: serialize into the chain and verify.
        let log = HmacChainLog::new(test_key());
        log.append(event).unwrap();
        log.verify().unwrap();
        assert_eq!(AuditLog::len(&log), 1);
    }

    #[test]
    fn pre_phase_126_tool_call_decodes_with_extracted_from_text_none() {
        // Same pattern as the Phase 120 wire-compat test: a
        // ToolCall with extracted_from_text == None serializes
        // WITHOUT the field (#[serde(default,
        // skip_serializing_if = "Option::is_none")] preserves
        // pre-Phase-126 chain compatibility). Round-trip
        // through serialize-then-deserialize confirms the
        // #[serde(default)] supplies None on the absent field.
        let pre_126_shape = sample_tool_call();
        let json = serde_json::to_value(&pre_126_shape).unwrap();
        let obj = json.as_object().expect("object");
        assert!(
            !obj.contains_key("extracted_from_text"),
            "sample_tool_call() with None must serialize without the field"
        );
        let decoded: AuditEvent = serde_json::from_value(json).expect("decode");
        match decoded {
            AuditEvent::ToolCall {
                extracted_from_text,
                auto_corrected_from,
                ..
            } => {
                assert!(extracted_from_text.is_none());
                assert!(auto_corrected_from.is_none());
            }
            other => panic!("expected ToolCall; got {other:?}"),
        }
    }

    // ---- NullAuditLog ----

    #[test]
    fn null_audit_log_accepts_and_verifies() {
        let null = NullAuditLog;
        null.append(sample_tool_call()).unwrap();
        assert_eq!(AuditLog::len(&null), 0);
        null.verify().unwrap();
    }

    // ---- D1 Scenario 3 audit trail: rm -rf denied on Tier 2 ----

    #[test]
    fn d1_scenario3_produces_denied_audit_trail() {
        // Walks the audit trail a denied Telegram rm -rf would emit:
        // TurnStarted → ScopeDenied → TurnEnded, all MAC-chained.
        let log = HmacChainLog::new(test_key());
        let turn_id = TurnId::new();
        let session_id = SessionId::new();

        let agent = CapabilitySet::from_scopes([Scope::parse("shell.exec").unwrap()]);
        let effective = agent.intersect(TrustTier::SemiTrusted.default_ceiling());

        log.append(AuditEvent::TurnStarted {
            turn_id,
            session_id,
            channel: ChannelPlatform::Telegram,
            trust_tier: TrustTierSummary::SemiTrusted,
            effective_capabilities: effective.clone(),
        })
        .unwrap();

        log.append(AuditEvent::ScopeDenied {
            turn_id,
            tool_attempted: ToolId::new(),
            scope_requested: Scope::parse("shell.exec:rm").unwrap(),
            held_capabilities: effective,
        })
        .unwrap();

        log.append(AuditEvent::TurnEnded {
            turn_id,
            outcome: TurnOutcomeSummary::Completed,
            tool_calls_made: 0,
            duration: Duration::from_millis(42),
            usage: TokenUsage::default(),
        })
        .unwrap();

        log.verify().unwrap();
        assert_eq!(AuditLog::len(&log), 3);

        // Spot-check the denial was captured with the right scope.
        match log.get(1).unwrap().event {
            AuditEvent::ScopeDenied {
                scope_requested, ..
            } => {
                assert_eq!(scope_requested.base(), "shell.exec");
                assert_eq!(scope_requested.qualifier(), Some("rm"));
            }
            other => panic!("expected ScopeDenied, got {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Bridge tests: aivyx_core::AuditHook <-> aivyx_audit::AuditWriter
    // -----------------------------------------------------------------------

    /// An `AuditWriter` that always fails. Used by the with_error_handler
    /// test to verify the handler path runs instead of panicking.
    struct AlwaysBroken;

    impl AuditWriter for AlwaysBroken {
        fn append(&self, _event: AuditEvent) -> Result<u64, AuditError> {
            Err(AuditError::ChainBroken {
                seq: 0,
                reason: "synthetic test failure".to_string(),
            })
        }
    }

    #[test]
    fn bridge_writes_tool_call_to_chain() {
        use aivyx_core::{AuditHook, AuditTag, ToolId};
        use std::time::Duration;

        let log = HmacChainLog::new(test_key());
        let bridge = AuditBridge::new(log);

        // Feed one ToolCall through the AuditHook surface.
        bridge.on_event(AuditTag::ToolCall {
            turn_id: TurnId::new(),
            tool_id: ToolId::new(),
            scope_used: sample_scope(),
            input_hash: [7u8; 32],
            outcome: ToolOutcomeSummary::Completed {
                verified: aivyx_core::VerificationSummary::NotApplicable,
            },
            duration: Duration::from_millis(3),
            auto_corrected_from: None,
            extracted_from_text: None,
        });

        // Chain length went up, verification still holds.
        assert_eq!(bridge.writer().len(), 1);
        bridge.writer().verify().unwrap();

        // And the entry is actually a ToolCall with the right input_hash.
        match bridge.writer().get(0).unwrap().event {
            AuditEvent::ToolCall { input_hash, .. } => {
                assert_eq!(input_hash, [7u8; 32]);
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    #[test]
    fn bridge_translates_all_five_variants() {
        use aivyx_core::{AuditHook, AuditTag, TokenUsage, ToolId};
        use std::time::Duration;

        let bridge = AuditBridge::new(HmacChainLog::new(test_key()));
        let turn_id = TurnId::new();
        let session_id = SessionId::new();

        // One of each D4 variant — the bridge must translate every shape.
        bridge.on_event(AuditTag::TurnStarted {
            turn_id,
            session_id,
            channel: ChannelPlatform::Local,
            trust_tier: TrustTier::Trusted,
            effective_capabilities: sample_capset(),
        });
        bridge.on_event(AuditTag::ToolCall {
            turn_id,
            tool_id: ToolId::new(),
            scope_used: sample_scope(),
            input_hash: [1u8; 32],
            outcome: ToolOutcomeSummary::Completed {
                verified: aivyx_core::VerificationSummary::NotApplicable,
            },
            duration: Duration::from_millis(1),
            auto_corrected_from: None,
            extracted_from_text: None,
        });
        bridge.on_event(AuditTag::ScopeDenied {
            turn_id,
            tool_attempted: ToolId::new(),
            scope_requested: Scope::parse("shell.exec:rm").unwrap(),
            held_capabilities: sample_capset(),
        });
        bridge.on_event(AuditTag::MemoryAccess {
            turn_id,
            operation: aivyx_core::MemoryOperation::Read,
            scope: sample_scope(),
            query_or_key: "yesterday".to_string(),
        });
        bridge.on_event(AuditTag::TurnEnded {
            turn_id,
            outcome: TurnOutcomeSummary::Completed,
            tool_calls_made: 1,
            duration: Duration::from_millis(5),
            usage: TokenUsage::default(),
        });

        // All five entries present, chain still verifies.
        assert_eq!(bridge.writer().len(), 5);
        bridge.writer().verify().unwrap();

        // Spot-check the TurnStarted translation picked the right tier
        // and the MemoryAccess translation picked the right op kind.
        match bridge.writer().get(0).unwrap().event {
            AuditEvent::TurnStarted { trust_tier, .. } => {
                assert_eq!(trust_tier, TrustTierSummary::Trusted);
            }
            other => panic!("expected TurnStarted at seq 0, got {other:?}"),
        }
        match bridge.writer().get(3).unwrap().event {
            AuditEvent::MemoryAccess { operation, .. } => {
                assert_eq!(operation, MemoryOperation::Read);
            }
            other => panic!("expected MemoryAccess at seq 3, got {other:?}"),
        }
    }

    #[test]
    fn bridge_with_handler_captures_error_instead_of_panicking() {
        use aivyx_core::{AuditHook, AuditTag, ToolId};
        use std::sync::{Arc, Mutex};
        use std::time::Duration;

        let captured: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let captured_clone = Arc::clone(&captured);

        let bridge = AuditBridge::with_error_handler(AlwaysBroken, move |e| {
            captured_clone.lock().unwrap().push(e.to_string());
        });

        // This would panic under the default bridge — with a handler it
        // routes to the closure instead.
        bridge.on_event(AuditTag::ToolCall {
            turn_id: TurnId::new(),
            tool_id: ToolId::new(),
            scope_used: sample_scope(),
            input_hash: [0u8; 32],
            outcome: ToolOutcomeSummary::Completed {
                verified: aivyx_core::VerificationSummary::NotApplicable,
            },
            duration: Duration::from_millis(1),
            auto_corrected_from: None,
            extracted_from_text: None,
        });

        let errors = captured.lock().unwrap();
        assert_eq!(errors.len(), 1);
        assert!(
            errors[0].contains("chain verification failed"),
            "expected AuditError::ChainBroken message, got {:?}",
            errors[0]
        );
    }

    // Audit L3 regression — the bridge increments
    // `failed_append_count` on every writer failure, even when
    // a soft-fail `with_error_handler` swallows the error. This
    // gives operators visibility into degraded audit chains
    // that a custom handler would otherwise hide.
    #[test]
    fn audit_l3_failed_append_count_tracks_writer_failures() {
        use aivyx_core::{AuditHook, AuditTag, ToolId};
        use std::time::Duration;

        let bridge = AuditBridge::with_error_handler(AlwaysBroken, |_e| {
            // intentionally swallow — the counter must still tick
        });

        assert_eq!(bridge.failed_append_count(), 0, "fresh bridge has no failures");

        let sample = || AuditTag::ToolCall {
            turn_id: TurnId::new(),
            tool_id: ToolId::new(),
            scope_used: sample_scope(),
            input_hash: [0u8; 32],
            outcome: ToolOutcomeSummary::Completed {
                verified: aivyx_core::VerificationSummary::NotApplicable,
            },
            duration: Duration::from_millis(1),
            auto_corrected_from: None,
            extracted_from_text: None,
        };

        bridge.on_event(sample());
        bridge.on_event(sample());
        bridge.on_event(sample());

        assert_eq!(
            bridge.failed_append_count(),
            3,
            "every failure must increment the counter even though the \
             custom handler swallowed the AuditError"
        );
    }

    // ---- Piece C (2026-08-23) — TeamMissionChannelTriggered variant ----

    #[test]
    fn team_mission_channel_triggered_round_trips() {
        let event = AuditEvent::TeamMissionChannelTriggered {
            platform: "telegram".to_string(),
            goal: "close the books".to_string(),
            mission_id: "m-1".to_string(),
        };
        let json = serde_json::to_string(&event).expect("serialize");
        let back: AuditEvent = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(event, back);
    }

    #[test]
    fn team_mission_channel_denied_round_trips() {
        let event = AuditEvent::TeamMissionChannelDenied {
            platform: "telegram".to_string(),
            goal: "close the books".to_string(),
            reason: "channel not authorized via team_run_channel in aivyx-pa.toml".to_string(),
        };
        let json = serde_json::to_string(&event).expect("serialize");
        let back: AuditEvent = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(event, back);
    }
}
