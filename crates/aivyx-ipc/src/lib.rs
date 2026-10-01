//! The daemon ↔ client **wire protocol** (Chapter M) — wasm-clean shared types.
//!
//! Carved out of `aivyx-channel` (the daemon, which can't target `wasm32`) so
//! the browser Mission-Control app (a Dioxus `wasm32` client) and the daemon
//! serialize from **one source of wire truth** — adding a variant updates both
//! sides at once, with no hand-maintained JSON mirror to drift.
//!
//! M.2a seeds the crate with the **team-mission** types (the headline of
//! Chapter M's Mission Control). The full IPC envelope (`FrontendMessage` /
//! `QueryPayload` / `QueryResponsePayload` …) and the other embedded data
//! types land in later M.2 slices, after each is made wasm-clean.
//!
//! Deps: `aivyx-team-types` (itself wasm-clean) + `serde` only.

pub mod backlog;
pub mod briefing;
pub mod insights;
pub mod ledgers;
pub mod loop_state;
pub mod persona;
/// The daemon ↔ client IPC envelope + frame codec (Chapter M.2f): the
/// `FrontendMessage` / `DaemonMessage` / `QueryPayload` / `QueryResponsePayload`
/// protocol, the `*Summary` wire structs, and `encode_frame` / `decode_frame`.
pub mod protocol;
pub mod team_mission;
/// Chapter Codex — knowledge-wiki page DTOs (`WikiPage` / `WikiBacklink` /
/// `WikiPageSummary`) shared by the daemon, IPC, and the Studio Wiki view.
pub mod wiki;
/// Chapter Lattice — typed knowledge-graph DTOs (`GraphTriple` /
/// `GraphEntity` / `GraphPath`) shared by the daemon, IPC, the
/// `graph.query` tool, and the Studio graph view.
pub mod graph;
/// Chapter Concord — memory-conflict DTOs (`MemoryConflict` /
/// `ConflictSide`) shared by the daemon, IPC, and the CLI/Studio for
/// surfacing contradictory stored facts to the operator for resolution.
pub mod conflict;
/// Chapter Accord — the wasm-clean Soul-contradiction result type
/// (`SoulConflict` / `SoulFacet`) shared by the daemon, IPC, and CLI/Studio
/// for surfacing self-contradictory or profile-drifting persona facets.
pub mod soul_conflict;
/// Chapter Passport — the federation relay protocol verbs (`RelayRequest` /
/// `RelayResponse`), the wasm-clean wire shape both the local Nonagon bus and a
/// future cross-operator relay carry (docs/FEDERATION.md §5). Crypto + the
/// signing envelope live in `aivyx-federation`.
pub mod federation;

pub use backlog::{Story, StoryStatus};
pub use insights::{
    ContributingTurn, CorrectionConsolidationStat, CorrectionJudgmentStat, DeltaExport,
    LearningDigest, PersonaConsolidationStat, PersonaLifecycleProposed, PersonaLifecycleStat,
    PersonaSelectionStat, ProactiveKind, ProactiveStat, ProactiveSurfaced, ProposalProvenance,
    RecallClusterStat, RecallJudgment, RecallJudgmentStat, RecentReflectionStat, SoftCategory,
};
pub use ledgers::{
    AccumulatedCorrections, AccumulatedHelpfulness, CooccurrencePatterns, PairScore,
    TopicCorrections, TopicScore,
};
pub use federation::{RelayRequest, RelayResponse};
pub use loop_state::LoopRunState;
pub use persona::{
    EffectivePersona, LearnedSkill, PersonaDelta, PersonaDeltaCategory, PersonaDeltaOp,
    ProposedPersonaDelta,
};
pub use team_mission::{
    TeamMissionPhase, TeamMissionRecord, TeamMissionView, TeamStepState, TeamStepView,
};
// Chapter Y — the team roster types, for the Studio's Teams screen.
pub use aivyx_team_types::{TeamConfig, TeamMember, TrustTier};
