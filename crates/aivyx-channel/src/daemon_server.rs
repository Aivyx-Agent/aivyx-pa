//! Production daemon server — Phase 17 Task 2, Phase 19 Task 2.
//!
//! Listens on a Unix domain socket, accepts connections, reads IPC
//! frames, dispatches turns through the provided agent, and streams
//! `DaemonMessage` frames back. Supports multi-turn sessions and
//! concurrent connections (Phase 19), with graceful shutdown via a
//! `CancellationToken`.
//!
//! Phase 16 shipped the single-turn PoC; Phase 17 Task 2 extended to
//! multi-turn with graceful shutdown; Phase 19 Task 2 upgrades to
//! multi-connection with per-connection channel construction.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;

use aivyx_audit::{AuditWriter, PersistentAuditLog};
use aivyx_core::{
    Agent, CancellationToken, ChannelContext, GatePolicy, Message, StreamEvent, TurnOutcome,
};

use aivyx_storage::DomainHandle;

use crate::daemon_ipc::{
    AuditEntrySummary, DaemonLifecycleEvent, DaemonMessage, FrameError, FrontendMessage,
    FrontendType, GalleryImage, GateSummary, MissionDetail, MissionSummary,
    NotificationHistoryEntry, PROTOCOL_VERSION, ProfileSummary, QueryPayload, QueryResponsePayload,
    ReminderView, SessionSummary, StreamEventPayload, WireChannelPlatform, decode_frame,
    encode_frame,
};
use crate::mission;

// ---------------------------------------------------------------------------
// DaemonError — typed error enum for the daemon layer (Phase 41 Task 3)
// ---------------------------------------------------------------------------

/// Typed error enum for the daemon server and its subsystems.
///
/// Phase 41 Task 3 replaces the stringly-typed `Result<(), String>`
/// signatures that had accumulated across Phases 16–39. Typed errors
/// are a prerequisite for the Channel SDK (P5) — third-party adapters
/// need matchable variants, not opaque strings.
#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    /// Failed to bind the Unix domain socket.
    #[error("failed to bind daemon socket at {path}: {source}")]
    Bind {
        path: String,
        source: std::io::Error,
    },

    /// Failed to accept an incoming connection.
    #[error("accept error: {0}")]
    Accept(std::io::Error),

    /// IPC frame encoding or decoding failure.
    #[error("frame error: {0}")]
    Frame(#[from] FrameError),

    /// IPC protocol violation (e.g., message before handshake).
    #[error("protocol error: {0}")]
    Protocol(String),

    /// I/O error on the socket connection.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// PID file or state file operation failed.
    #[error("pid/state file error at {path}: {source}")]
    PidFile {
        path: String,
        source: std::io::Error,
    },

    /// Mission store operation failed.
    #[error("mission store error: {0}")]
    MissionStore(String),

    /// Configuration error (missing or invalid config values).
    #[error("config error: {0}")]
    Config(String),

    /// WebSocket or Web UI error.
    #[error("websocket error: {0}")]
    WebSocket(String),

    /// Internal error (catch-all for unexpected conditions).
    #[error("{0}")]
    Internal(String),
}

impl DaemonError {
    /// Convert a `DaemonError` to a `String` for backward compatibility
    /// with callers that still use `Result<_, String>`.
    pub fn to_string_compat(&self) -> String {
        self.to_string()
    }

    /// True when this error is a client hanging up the socket cleanly —
    /// a CLI invocation finishing and dropping its end — rather than a
    /// genuine fault. Backlog #4: the daemon used to log every such
    /// disconnect at error level (`connection handler error: io error:
    /// Broken pipe`), spamming `journalctl` once per CLI query and
    /// masking real errors. These are expected lifecycle events, not
    /// faults, so callers suppress them.
    pub fn is_clean_disconnect(&self) -> bool {
        use std::io::ErrorKind::{BrokenPipe, ConnectionReset, UnexpectedEof};
        match self {
            DaemonError::Io(e) => {
                matches!(e.kind(), BrokenPipe | ConnectionReset | UnexpectedEof)
            }
            _ => false,
        }
    }
}

/// Channel factory: given a `FrontendType`, returns the appropriate
/// `ChannelContext` implementation for that frontend. The binary
/// constructs this closure at startup, capturing the resources each
/// channel type needs (stdout handle for Local, transport for Telegram).
pub type ChannelFactory =
    Arc<dyn Fn(FrontendType) -> Arc<dyn ChannelContext + Send + Sync> + Send + Sync>;

/// Configuration for the daemon server.
///
/// Bundles the parameters that `run_daemon` needs into a single struct.
/// Phase 41 Task 2 extracted these from the 10-parameter function
/// signature that had accreted across Phases 21–39.
pub struct DaemonConfig {
    /// Path to the Unix domain socket the daemon listens on.
    pub socket_path: PathBuf,
    /// The shared agent instance that serves all connections.
    pub agent: Arc<dyn Agent>,
    /// Factory that constructs per-connection `ChannelContext` impls.
    pub channel_factory: ChannelFactory,
    /// Token for triggering graceful shutdown from outside.
    pub shutdown: CancellationToken,
    /// Optional encrypted storage domain for mission state.
    pub mission_store: Option<DomainHandle>,
    /// Phase 63 Task 3 — optional notify dispatcher passed to
    /// `TriggerDispatch::with_notify_dispatcher` so trigger
    /// configs with `notify_target = Some(name)` auto-push the
    /// turn's final response after firing.
    pub notify_dispatcher: Option<Arc<crate::notify_dispatcher::NotifyDispatcher>>,
    /// Chapter Herald — the resolved `[[notify_target]] default = true`
    /// name (possibly an in-memory-synthesized `webui` target — see
    /// `aivyx.rs`), threaded into `TriggerDispatch` and `ReportContext`
    /// so a schedule/mission with no explicit `notify_targets` still
    /// notifies something instead of staying silent.
    pub default_notify_target: Option<String>,
    /// Chapter Herald — the resolved notify-target list (operator's
    /// `[[notify_target]]` entries plus, when applicable, the
    /// daemon's synthesized default "studio" target) for the
    /// read-only `GetNotifyTargets` query. Never mutated from
    /// Studio — targets stay TOML-managed.
    pub notify_targets: Vec<aivyx_config::NotifyTargetConfig>,
    /// Optional encrypted storage domain for cron schedules.
    pub schedule_store: Option<DomainHandle>,
    /// Optional encrypted storage domain for webhook triggers.
    pub webhook_store: Option<DomainHandle>,
    /// Optional encrypted storage domain for file-watch triggers.
    pub file_watch_store: Option<DomainHandle>,
    /// Port for the localhost-only webhook HTTP listener.
    pub webhook_port: Option<u16>,
    /// Port for the web UI server.
    pub web_ui_port: Option<u16>,
    /// Bind host for the web UI server. `None` → `127.0.0.1` (the
    /// localhost-only default). Chapter Harbor: `0.0.0.0` for containers.
    pub web_ui_host: Option<std::net::IpAddr>,
    /// Extra WS Origin allowlist entries beyond the built-in loopback origins.
    /// Empty (default) keeps the localhost-only CSWSH posture. Chapter Harbor:
    /// the hostnames a remotely-exposed Studio is served at.
    pub web_ui_allowed_origins: Vec<String>,
    /// Chapter Postern — shared-secret token gating the web UI's control plane.
    /// `None` (default) → no auth. When set, `/ws` requires the token and static
    /// routes prompt via HTTP Basic.
    pub web_ui_auth_token: Option<String>,
    /// Studio Gallery — base URL of the `comfyui`-named `[[mcp_server]]`'s
    /// backing ComfyUI instance (its `COMFYUI_URL` env entry, defaulting to
    /// `http://localhost:8188` when the entry exists but doesn't set that
    /// key). `None` when no `comfyui` server is configured — the Gallery
    /// query and the `/studio-asset` route both no-op in that case.
    pub comfyui_base_url: Option<String>,
    /// Optional shared memory instance for background GC.
    pub memory: Option<Arc<dyn aivyx_memory::Memory>>,
    /// If set, entries older than this many seconds are expired by a
    /// background 1-hour timer.  Requires `memory` to be `Some`.
    pub memory_ttl_secs: Option<u64>,
    /// Phase 47 — optional handle on the persistent audit log so the
    /// daemon can answer `ListAuditEntries` / `VerifyAuditChain`
    /// inspection queries from the Web UI. When `None`, those queries
    /// return `QueryError { code: "no_audit_log", .. }`.
    pub audit_log: Option<Arc<PersistentAuditLog>>,
    /// Phase 58 — operator-declared identity layer (PRODUCT.md P13).
    /// Read-only at daemon runtime per Q5(a) load-time semantics;
    /// served to the Web UI Profile pane via the `GetProfile`
    /// inspection query. Always populated — the synthesized default
    /// is supplied when `aivyx-pa.toml` has no `[profile]` section.
    pub profile: Arc<aivyx_config::Profile>,
    /// Phase 60 — persistent Persona delta chain (PRODUCT.md P14).
    /// The daemon uses it for both inspection queries
    /// (`ListPersonaDeltas`) and revert operations
    /// (`RevertPersonaDelta` appends to it). `None` is the test-
    /// fixture path (POC daemon / round-trip tests) — both queries
    /// return empty / default responses.
    pub persona_log: Option<Arc<crate::persona::PersistentPersonaLog>>,
    /// Phase 60 — shared runtime effective Persona. The planner
    /// factory reads it per-turn; this handle exists on the daemon
    /// side so `RevertPersonaDelta` and inspection queries can read
    /// the current snapshot. Always present — defaults to an empty
    /// state for test fixtures.
    pub shared_persona: crate::persona::SharedEffectivePersona,
    /// Phase 69 — Web UI desktop-notification broadcaster. When
    /// the Web UI is enabled, the binary constructs one
    /// `WebUiBroadcaster` and Arc-shares it between this field
    /// (so the WS handler can subscribe per browser connection)
    /// and the notify dispatcher (so `kind = "web-ui"` targets
    /// can push frames into it). `None` when the Web UI is
    /// disabled and no `kind = "web-ui"` targets exist.
    pub web_ui_broadcaster: Option<Arc<crate::notify_webui::WebUiBroadcaster>>,
    /// Phase 70 — persistent Persona proposal chain
    /// (KeyDomain::PersonaProposals). Pending proposals from
    /// the reflection auto-loop append rows here; operators
    /// resolve them via `ResolvePersonaProposal`, which
    /// transitions the status to Approved / Rejected and
    /// (on approve) appends a PersonaDelta to `persona_log`.
    /// `None` is the test-fixture path — proposal queries
    /// return empty / not-wired responses.
    pub persona_proposal_log: Option<Arc<crate::persona_proposal::PersistentPersonaProposalLog>>,
    /// Phase 71 — validated `[[reflection_schedule]]` entries
    /// from the config loader. When non-empty AND an audit log
    /// is configured, the daemon spawns
    /// `run_reflection_scheduler` to fire reflection turns on
    /// each entry's cron pattern. When empty, the reflection
    /// scheduler task is not spawned.
    pub reflection_schedules: Vec<aivyx_config::ReflectionScheduleConfig>,
    /// Phase 74 — per-topic-glob retention rules from
    /// `[[memory.retention]]`. Threaded into the memory-GC
    /// timer; first-match wins, unmatched topics fall through
    /// to `memory_ttl_secs`.
    pub memory_retention: Vec<aivyx_config::MemoryRetentionRule>,
    /// Phase 73 — per-target retry + rate-limit policy map.
    /// Built by the binary's startup path from the loaded
    /// `[[notify_target]]` blocks (one entry per target name).
    /// Empty map → every dispatch uses the zero-retry / no-
    /// rate-limit defaults — today's behavior.
    pub target_policies: std::collections::HashMap<String, crate::trigger::TargetPolicy>,
    /// Phase 75 — embedding provider for semantic memory.
    /// `Some` iff `[embedding]` is configured. Drives the
    /// hourly lazy-backfill pass in the memory-GC timer; it is
    /// the same provider the write tool's embedding hook wraps.
    /// `None` = semantic search disabled, no backfill spawned.
    pub embedding_provider: Option<Arc<dyn aivyx_llm::embedding::EmbeddingProvider>>,
    /// Phase 77 — the recall-feedback log. `Some` iff
    /// auto-recall is configured; the reflection scheduler
    /// reads/clamps it on its cadence to close the
    /// recall→learning loop. `None` → the feedback pass is
    /// skipped (pre-Phase-77 behavior).
    pub recall_log: Option<Arc<crate::recall_log::PersistentRecallLog>>,
    /// Phase 82 — the durable helpfulness ledger. `Some` iff
    /// the recall substrate is configured (zero-config, built
    /// alongside the recall log); the reflection recall-feedback
    /// pass folds each window into it. `None` → no fold (a
    /// passive add-on; recall-feedback is unaffected).
    pub helpfulness_ledger: Option<Arc<crate::helpfulness_ledger::PersistentHelpfulnessLedger>>,
    /// Phase 83 — the durable cross-session co-occurrence
    /// ledger. `Some` iff the recall substrate is configured
    /// (zero-config, built alongside the recall log); the
    /// reflection pass folds each window's pairs into it.
    /// `None` → no fold (a passive add-on).
    pub cooccurrence_ledger: Option<Arc<crate::cooccurrence_ledger::PersistentCooccurrenceLedger>>,
    /// Chapter Codex (CX.3) — knowledge-wiki sweep. `Some` when
    /// `[wiki].enabled` + an LLM provider are configured: the daemon
    /// spawns a periodic stale-page sweep on its maintenance cadence.
    /// `None` → no synthesis (the byte-identical default).
    pub wiki_sweep: Option<crate::knowledge_wiki::WikiSweepConfig>,
    /// Chapter Codex (CX.4) — read handle on the knowledge-wiki page
    /// store, for the `ListWikiPages` / `GetWikiPage` read-only IPC.
    /// Built whenever storage is available (independent of `[wiki]`
    /// .enabled — reads return an empty list until a sweep populates it).
    pub wiki_store: Option<Arc<crate::knowledge_wiki::PersistentWikiStore>>,
    /// Chapter Lattice (LT.3) — typed-knowledge-graph sweep. `Some` when
    /// `[graph].enabled` + an LLM provider are configured: the daemon
    /// spawns a periodic triple-extraction sweep on its maintenance
    /// cadence. `None` → no extraction (the byte-identical default).
    pub graph_sweep: Option<crate::knowledge_graph::GraphSweepConfig>,
    /// Chapter Lattice (LT.5) — read handle on the typed-graph store, for
    /// the `GetKnowledgeGraph` read-only IPC (the Studio graph view).
    /// Built whenever storage is available (independent of `[graph]`
    /// .enabled — reads return an empty graph until a sweep populates it).
    pub graph_store: Option<Arc<crate::knowledge_graph::PersistentGraphStore>>,
    /// Chapter Concord — the durable dismissed-conflict set for the
    /// `GetMemoryConflicts` filter + `DismissMemoryConflict` handler. Built
    /// whenever storage is available; `None` ⇒ dismissal is a no-op and
    /// nothing is filtered.
    pub conflict_dismissals: Option<Arc<crate::conflict_dismissals::PersistentConflictDismissals>>,
    /// Phase 172 — the durable correction ledger. `Some` iff
    /// the recall substrate is configured (zero-config, built
    /// alongside the recall log); the reflection recall-feedback
    /// pass folds each window's per-topic correction counts into
    /// it, and the consolidation pass reads it. `None` → no fold
    /// (a passive add-on).
    pub correction_ledger: Option<Arc<crate::correction_ledger::PersistentCorrectionLedger>>,
    /// Phase 79 (Q4a) — shared last-Persona-selection stat the
    /// adaptive refiner writes and `GetLearningInsights` reads.
    /// `None` → adaptive Persona not configured (the surface
    /// reports no selection).
    pub persona_selection_stat: Option<crate::persona_context::SharedPersonaSelectionStat>,
    /// Phase 84 (Q4a) — shared last-turn cluster-recall stat
    /// the recall provider writes and `GetLearningInsights`
    /// reads. `None` → cluster expansion not armed (the
    /// surface reports none).
    pub recall_cluster_stat: Option<crate::memory_recall::SharedRecallClusterStat>,
    /// Phase 80 — `[proactive]` config. `None` (no section) →
    /// proactive surfacing is off; even `Some` no-ops unless
    /// `enabled`.
    pub proactive_config: Option<aivyx_config::ProactiveConfig>,
    /// Phase 80 — proactive dedup log. `Some` iff proactive is
    /// armed; the reflection cron pass uses it for cross-cycle
    /// dedup + the per-window cap.
    pub proactive_log: Option<Arc<crate::proactive_log::PersistentProactiveLog>>,
    /// Phase 80 (Q4a) — shared last-proactive-cycle stat the
    /// pass writes and `GetLearningInsights` reads. `None` →
    /// proactive not armed (the surface reports none).
    pub proactive_stat: Option<crate::proactive_detect::SharedProactiveStat>,
    /// Phase 81 — `[persona_lifecycle]` config. `None` (no
    /// section) → the Persona never self-consolidates or
    /// decays; even `Some` no-ops unless `enabled`.
    pub persona_lifecycle_config: Option<aivyx_config::PersonaLifecycleConfig>,
    /// Phase 81 (Q4a) — shared last-lifecycle-cycle stat the
    /// pass writes and `GetLearningInsights` reads. `None` →
    /// the lifecycle pass is not armed (the surface reports
    /// none).
    pub persona_lifecycle_stat: Option<crate::persona_lifecycle::SharedPersonaLifecycleStat>,
    /// Phase 86 — daemon-scoped, per-session conversation
    /// windows. `Some` iff the recall substrate is configured
    /// (built alongside the recall log at daemon startup, same
    /// life cycle as the shared persona-selection / recall-
    /// cluster stats). The daemon turn loop writes a `(user,
    /// assistant)` pair into the matching session's ring on each
    /// `TurnOutcome::Completed`; both relevance providers read
    /// via `assemble_for` when `recall_window_turns` is greater
    /// than 1. `None` → the Phase 86 window is off (every
    /// recall query is byte-identical to pre-Phase-86).
    pub conversation_windows: Option<crate::conversation_window::SharedConversationWindows>,
    /// Phase 87 — `[persona_consolidation]` config. `None` (no
    /// section) → pattern-driven proposals are off; even
    /// `Some` no-ops unless `enabled`. The reflection pass
    /// reads this alongside the co-occurrence + helpfulness
    /// ledgers + the proposal chain.
    pub persona_consolidation_config: Option<aivyx_config::PersonaConsolidationConfig>,
    /// Phase 87 (Q4a) — shared last-cycle consolidation stat
    /// the pass writes and `GetLearningInsights` reads. `None`
    /// → consolidation not armed (the surface reports none).
    pub persona_consolidation_stat:
        Option<crate::persona_consolidation::SharedPersonaConsolidationStat>,
    /// Phase 87 — production `PairPhraser` for the
    /// LLM-summarized facet phrasing (Q2b). `None` → the pass
    /// has no LLM access and skips the cycle (the actuator
    /// stays best-effort).
    pub persona_consolidation_phraser:
        Option<std::sync::Arc<dyn crate::persona_consolidation::PairPhraser>>,
    /// Phase 172 — `[correction_consolidation]` config. `None`
    /// (no section) → correction-driven proposals are off (the
    /// correction ledger still accumulates passively); `Some`
    /// arms the reflection-cron pass only when `enabled = true`.
    pub correction_consolidation_config: Option<aivyx_config::CorrectionConsolidationConfig>,
    /// Phase 172 — shared last-cycle correction-consolidation
    /// stat the pass writes and `GetLearningInsights` reads.
    /// `None` → not armed (the surface reports none).
    pub correction_consolidation_stat:
        Option<crate::correction_consolidation::SharedCorrectionConsolidationStat>,
    /// Phase 172 — production `TopicPhraser` for the correction
    /// facet phrasing. `None` → the pass has no LLM access and
    /// skips the cycle (the actuator stays best-effort).
    pub correction_consolidation_phraser:
        Option<std::sync::Arc<dyn crate::correction_consolidation::TopicPhraser>>,
    /// Phase 91 — `[recall_judgment]` config. `None` (no
    /// section) → LLM-judged recall is off; `Some` arms the
    /// reflection-cron pass only when `enabled = true`.
    pub recall_judgment_config: Option<aivyx_config::RecallJudgmentConfig>,
    /// Phase 91 (Q4a) — shared last-cycle judgment stat the
    /// pass writes and `GetLearningInsights` reads. `None` →
    /// the pass has not run this daemon lifetime.
    pub recall_judgment_stat: Option<crate::recall_judgment::SharedRecallJudgmentStat>,
    /// Phase 91 — production `RecallJudge` for the
    /// LLM-judged classification (Q2a). `None` → the pass
    /// has no LLM access and skips every cycle (the actuator
    /// stays best-effort).
    pub recall_judge: Option<std::sync::Arc<dyn crate::recall_judgment::RecallJudge>>,
    /// Phase 178 — `[correction_judgment]` config + judge + stat
    /// for the LLM-judged correction fold. All `None` → the
    /// Phase 172 structural correction fold.
    pub correction_judgment_config: Option<aivyx_config::CorrectionJudgmentConfig>,
    pub correction_judge: Option<std::sync::Arc<dyn crate::correction_judgment::CorrectionJudge>>,
    pub correction_judgment_stat: Option<crate::correction_judgment::SharedCorrectionJudgmentStat>,
    /// Phase 179 — `[correction_signal]` config (tool correction
    /// attribution toggle). `None` → topic-only (Phase 172).
    pub correction_signal_config: Option<aivyx_config::CorrectionSignalConfig>,
    /// Phase 93 — `[recall_feedback]` config. `None` (no
    /// section) → `correlate_detailed` runs with the
    /// pre-Phase-93 structural-only behaviour. `Some` with
    /// `use_judgment_signal = true` flips the correlator to
    /// per-hit judgment override (un-judged hits keep the
    /// structural fallback).
    pub recall_feedback_config: Option<aivyx_config::RecallFeedbackConfig>,
    /// Phase 102 — a static snapshot of the registered tool set,
    /// captured from the `ToolRegistry` at daemon construction.
    /// The `GetToolStats` query joins it against the audit chain
    /// so a registered-but-uncalled tool still appears. Empty for
    /// test fixtures / a daemon built without a registry.
    pub tool_descriptors: Vec<ToolDescriptor>,

    /// Phase 112 — Skill Auto-Proposer dependency bundle.
    /// `None` disables the feature entirely; `Some(ctx)` wires
    /// the post-finalize hook so every conversational turn
    /// fires `run_auto_propose_pipeline` in a detached
    /// `tokio::spawn` (Q2b inline-at-turn-boundary). The
    /// pipeline reads the audit log, persona log, persona
    /// proposal log, and shared persona handle that already
    /// live on this struct — only the LLM provider and the
    /// proposer config are bundled here.
    pub skill_auto_proposer: Option<Arc<crate::skill_auto_proposer::SkillAutoProposerContext>>,

    /// Phase 116 — Tool/skill relevance ledger handle.
    /// `None` disables the feature; `Some(handle)` wires the
    /// daemon's post-finalize hook to record per-turn tool
    /// outcomes (Phase 116 Task 4) and the system-prompt
    /// assembly to render the `## Tools recently used for
    /// similar tasks` section (Phase 116 Task 5).
    pub tool_relevance_ledger:
        Option<Arc<crate::tool_relevance_ledger::PersistentToolRelevanceLedger>>,
    /// Chapter Whetstone (WH.3b) — the per-skill effectiveness ledger.
    /// `Some` iff `[skill_refinement]` is configured; the turn loop folds
    /// each turn's `SkillInvocation` outcomes into it for the WH.3
    /// refinement pass to read.
    pub skill_effectiveness_ledger:
        Option<Arc<crate::skill_effectiveness::SkillEffectivenessLedger>>,
    /// Chapter Whetstone (WH.3c) — `[skill_refinement]` config + the
    /// production refinement drafter. Both `Some` (with the ledger +
    /// persona/proposal logs) arm the reflection-cadence refinement pass.
    pub skill_refinement_config: Option<aivyx_config::SkillRefinementConfig>,
    pub skill_refinement_drafter: Option<Arc<dyn crate::skill_refinement::RefinementDrafter>>,
    /// Chapter Praxis (PX.2) — `[skill_authoring]` config + the production
    /// specialization drafter. Both `Some` (with the wiki/graph stores +
    /// proposal/persona logs) arm the reflection-cadence authoring pass.
    pub skill_authoring_config: Option<aivyx_config::SkillAuthoringConfig>,
    pub skill_authoring_drafter: Option<Arc<dyn crate::skill_authoring::SpecializationDrafter>>,
    /// Phase 173 — the autonomous-loop backlog (zero-config,
    /// always built when storage is configured) for the loop
    /// IPC handlers + the driver.
    pub loop_backlog: Option<Arc<crate::loop_backlog::PersistentLoopBacklog>>,
    /// Phase 173 — shared loop run state. `Some` only when the
    /// `[loop]` section is armed; the daemon spawns the loop
    /// driver and the IPC `loop start/stop/status` handlers
    /// flip / read this handle.
    pub loop_state: Option<crate::loop_driver::SharedLoopState>,
    /// Phase 173 — `[loop]` config (priority default +
    /// max-iterations ceiling). `None` when the section is
    /// absent.
    pub loop_config: Option<aivyx_config::LoopConfig>,
    /// Chapter L (L.5) — the daemon's team-mission service (registry + run
    /// deps + team config). `Some` when storage is configured; the
    /// `TeamRun` / `TeamMissionList` / `TeamMissionStatus` / `ResolveTeamGate`
    /// IPC handlers operate on it. `None` disables the team-mission surface.
    pub team_missions: Option<crate::team_mission_driver::TeamMissionService>,
    /// Chapter H — the daemon's default gate policy. `Interactive` (the
    /// default) parks an escalated turn behind an operator gate and waits;
    /// `RejectAndAbort` (headless) records the refusal and finalizes without a
    /// gate. Per-run/per-driver overrides come later (H.4/H.5).
    pub gate_policy: GatePolicy,
    /// Chapter O — proactive journaling cadence. `Some(interval)` (and an
    /// audit log present) spawns the workspace-journal driver, which on each
    /// tick checks for recent activity and, if any, fires a journaling turn
    /// that appends to the agent's workspace journal. `None` ⇒ disabled.
    pub workspace_journaling_interval: Option<std::time::Duration>,
    /// Chapter K (K.4.2) — the priced rate table, built from the
    /// built-in defaults plus any `[pricing.<model>]` overrides.
    /// Threaded into the autonomous-loop driver so the per-run
    /// dollar cap prices overridden models correctly (previously the
    /// driver built `Pricing::new()` internally, ignoring overrides).
    /// Defaults to an empty table for test fixtures that don't price.
    pub pricing: aivyx_cost::Pricing,
    /// Chapter U — path to the `aivyx-pa.toml` the daemon was loaded from, so the
    /// Settings IPC handlers (`GetSettings` / `SetAccessLevel` / `SetBudget`)
    /// can re-read the on-disk values and write sections back via the shared
    /// `aivyx_config::config_write` helper. `None` ⇒ the daemon was launched
    /// without a config file (env-only, or a test fixture); the write handlers
    /// then refuse with a typed "no config file" error rather than guessing a
    /// path. Read-only config (the access level itself) is still load-time —
    /// a write here only updates the file; it takes effect on the next start.
    pub config_toml_path: Option<PathBuf>,
    /// Piece C follow-up — the same value the daemon's own primary
    /// config load resolved its active role from (`LoadOptions::role_override`
    /// at the binary's own startup call). Threaded through so the
    /// re-reads below (`channel_trigger_authz`, Chapter U's
    /// `settings_applied`) resolve against the SAME role, instead of
    /// hardcoding `None` and risking `ConfigError::UnknownRole` for any
    /// operator running a non-default `--role`. `None` is correct for an
    /// env-only launch or a genuinely-default-role deployment — this
    /// field is not itself a resolution mechanism, only a carried value.
    pub role_override: Option<String>,
    /// Chapter Roster (RO.2) — the resolved team-config write target: the
    /// operator's `[team] config_path` (or the conventional `team.toml` beside
    /// `aivyx-pa.toml`), pre-resolved by the binary. `None` ⇒ env-only launch (no
    /// config file); the `SetTeamRoster` handler then refuses, like the other
    /// write handlers. Writes here are load-time — adopted on the next start.
    pub team_config_write_path: Option<PathBuf>,
    /// Chapter X — the model used to **draft** a persona seed from the
    /// operator's description (the Studio's `DraftPersonaSeed` IPC). `None` ⇒ no
    /// model is available for drafting, and the handler returns a typed "no
    /// model" error; live seeding (`SeedPersona`) is LLM-free and unaffected.
    pub seed_draft_llm: Option<SeedDraftLlm>,
    /// Chapter Z — the canonical roots the read-only Documents browser may reach
    /// (`fs` = the access-scoped `fs_root`, `workspace` = the agent's workspace).
    pub document_roots: DocumentRoots,
    /// Phase 186 — the reminder store for the `GetReminders` query (the
    /// TUI Dashboard's reminders panel). `None` ⇒ the `GetReminders`
    /// query returns an empty list rather than erroring — matches the
    /// existing `remind.*` tools' own degrade-gracefully posture.
    pub reminder_store: Option<crate::reminder_tool::SharedReminderStore>,
}

/// Chapter X — the provider + model the daemon uses for one-shot persona-seed
/// drafting. Cloned from the same provider the agent's turns use.
#[derive(Clone)]
pub struct SeedDraftLlm {
    pub provider: Arc<dyn aivyx_llm::LlmProvider>,
    pub model: String,
}

/// Chapter Z — the **pre-canonicalized** roots the Documents browser may list /
/// read. Each is `None` when unavailable (env-only launch / workspace disabled);
/// the handler then returns a typed error. The only reach Documents has.
#[derive(Clone, Default)]
pub struct DocumentRoots {
    /// The operator's `fs_root` (the access level's reach). The `"fs"` root.
    pub fs_root: Option<PathBuf>,
    /// The agent's workspace root. The `"workspace"` root.
    pub workspace_root: Option<PathBuf>,
}

/// Phase 102 — a registered tool's listing fields, snapshotted
/// from the `ToolRegistry` at daemon construction for the
/// `GetToolStats` query. Not a wire type — the daemon joins this
/// with audit stats to produce the wire-format
/// [`crate::daemon_ipc::ToolStat`].
#[derive(Debug, Clone)]
pub struct ToolDescriptor {
    /// Tool name as the planner advertises it (e.g. `fs.read`).
    pub name: String,
    /// One-line tool description.
    pub description: String,
    /// Capability base the tool's audit `ToolCall` events key on
    /// — `required_scope(..).base()`. Equals `name` for most
    /// tools but not all (`web.fetch` keys on `net.fetch`).
    pub scope_base: String,
}

/// Bind a Unix socket at `path` with mode 0600 from the instant it's
/// *visible* at that path — no window where a socket reachable by
/// another local user briefly exists there.
///
/// ## Why this isn't the audit's suggested `libc::umask` bracket
///
/// The originally-specified fix (2026-09-16 audit, Task 7) was to
/// temporarily narrow the process umask to `0o177` around a bind at
/// the final path, then restore it, since `UnixListener::bind` takes
/// no mode parameter. That was implemented first, and reverted after
/// it broke this crate's own test suite: `umask` is per-process, not
/// per-thread, state, so narrowing it around one bind narrows it for
/// *any* file/directory creation happening anywhere in the process
/// for the duration of the bracket — not just this socket.
/// `daemon_roundtrip_e2e.rs` alone runs 20-30+ `#[tokio::test]`
/// functions concurrently in one process, several of which start
/// their own daemon (hitting this function) while others are
/// concurrently creating their own unrelated scratch directories via
/// plain `std::fs::create_dir_all`. One test's narrowed umask landed
/// on another test's directory creation and stripped its execute
/// bit (`0600` has no `x`), which then failed with "Permission
/// denied" the instant that other test tried to create a file inside
/// it (`mission_queries_round_trip_over_ipc`, reproduced
/// 2026-09-16). A `Mutex` serializing only this function's own
/// callers was tried next and confirmed *not* sufficient — it
/// prevents this function's callers from racing each other, but does
/// nothing for the unrelated `create_dir_all` calls that never take
/// the lock, and the flake reproduced again with the lock in place.
///
/// ## What this does instead
///
/// Never touches process-global state. Binds to a staging path — the
/// same file name, in the same directory, plus a short random
/// suffix — chmods it to `0600` while it sits under that
/// not-yet-final name, then atomically `rename`s it onto `path`.
/// Nothing can connect to the socket via its *final* name before the
/// rename, and it is already `0600` the instant it becomes visible
/// *there*. Same end guarantee the umask approach was after, with no
/// process-wide side effect to race.
///
/// Precise, not sloppy, about what that guarantee covers: the staging
/// file itself is briefly at the *ambient umask*'s mode between
/// `bind` and the `set_permissions` two lines below — a real
/// create-then-chmod window, just relocated off the final path rather
/// than eliminated. What actually closes that window is the parent
/// directory (see the precondition below and `create_dir_all_0700`):
/// a `0700` directory means no other local user can `readdir()` or
/// even `stat()` the staging name into existence, so the mode the
/// staging file briefly sits at doesn't matter to anyone but this
/// user's own processes.
///
/// A first version staged inside a freshly created, separately-named
/// subdirectory instead of a same-directory sibling file. Reverted:
/// nesting an extra path component (worse, one whose name embeds a
/// full 36-character UUID for unguessability) routinely blew past
/// `AF_UNIX`'s hard ~108-byte `sun_path` limit — it failed with
/// `path must be shorter than SUN_LEN` in this very test suite,
/// before ever reaching production. The short-suffix, same-directory
/// approach below adds only a handful of bytes.
///
/// Precondition this relies on: `path`'s parent directory is already
/// inaccessible to other local users by the time this is called.
/// Both real call sites guarantee that — each calls
/// `create_dir_all_0700` on the parent immediately before this — so
/// nothing outside this user's own processes can ever `readdir()` the
/// staging name into view, which is what makes a merely
/// collision-avoiding (not cryptographically unguessable) suffix
/// sufficient here: the threat this guards against is a *concurrent
/// same-user process* reusing the identical staging name (this
/// crate's own test suite starts many daemons at once), not an
/// external attacker discovering it.
#[cfg(unix)]
fn bind_unix_socket_0600(path: &Path) -> std::io::Result<UnixListener> {
    use std::os::unix::fs::PermissionsExt;

    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("daemon.sock");
    // Low 32 bits of a fresh v4 UUID — short (8 hex chars) but still
    // effectively random for collision-avoidance purposes: a v4
    // UUID's fixed version/variant bits sit in the middle of its 128
    // bits (bytes 6 and 8 of 16), not in the low 32 we take here.
    // `uuid::Uuid::new_v4()` is already this workspace's standard
    // entropy source for exactly this kind of use (see the `uuid`
    // dep's Cargo.toml comment).
    let suffix = uuid::Uuid::new_v4().as_u128() as u32;
    let staging_path = parent.join(format!(".{file_name}.{suffix:08x}.tmp"));

    // Best-effort cleanup of the staging file on every exit path:
    // `rename` below moves it away on success, making this a no-op;
    // on any early `?` return it still exists under the staging name.
    struct RemoveFileOnDrop<'a>(&'a std::path::Path);
    impl Drop for RemoveFileOnDrop<'_> {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(self.0);
        }
    }
    let _cleanup = RemoveFileOnDrop(&staging_path);

    let listener = UnixListener::bind(&staging_path)?;
    std::fs::set_permissions(&staging_path, std::fs::Permissions::from_mode(0o600))?;
    std::fs::rename(&staging_path, path)?;

    Ok(listener)
}

/// Create `dir`'s parents (if missing, at whatever mode `create_dir_all`
/// gives them — not sensitive) then create `dir` itself atomically at
/// `0700`, or tighten it to `0700` if it already existed.
///
/// Task 7 final review (2026-09-16) — this used to be plain
/// `create_dir_all` (all components, ambient umask) followed by a
/// separate `set_permissions` on just the leaf, the same create-then-
/// chmod shape `bind_unix_socket_0600` above exists to avoid for the
/// socket file. That window matters more than it first looks: the
/// stage-then-rename socket bind is briefly at the *ambient umask*
/// mode while it exists under its staging name (see that function's
/// doc), and this directory being un-traversable by other local users
/// is the *only* thing covering that window — so a create-then-chmod
/// gap here reopens exactly what the socket-bind fix closed. Fixed by
/// creating the leaf via `DirBuilder::mode(0o700)`, whose mode is
/// passed straight to `mkdir(2)`: a umask can only clear bits from a
/// requested mode, never add them, so `0o700` requested this way can
/// never land wider than `0o700` regardless of the ambient umask (the
/// stage-then-rename socket bind above needs the same "no window at a
/// wider mode" property but a temp *file*'s create-mode argument isn't
/// the file's final mode on most platforms the way a directory's is,
/// which is why that fix stages-and-renames rather than relying on
/// `mode()` alone).
#[cfg(unix)]
fn create_dir_all_0700(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            // Pre-existing directory (an older version of this daemon, or
            // simply a prior run) — tighten it the same as before this
            // fix, same as the socket-mode fix's own "chmod out of a
            // pre-existing wider mode" posture.
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        }
        Err(e) => Err(e),
    }
}

/// Run the daemon server.
///
/// Binds the Unix socket at `config.socket_path`, accepts connections
/// in a loop, and spawns a handler task per connection. Each handler
/// reads `FrontendMessage` frames and dispatches turns through the
/// shared `agent`. The `channel_factory` constructs a per-connection
/// `ChannelContext` based on the frontend type sent in `StartSession`.
///
/// The `shutdown` token allows external code (signal handlers, tests)
/// to trigger a graceful shutdown. When cancelled, the daemon stops
/// accepting new connections; in-flight handler tasks complete their
/// current turn and exit.
pub async fn run_daemon(config: DaemonConfig) -> Result<(), DaemonError> {
    let DaemonConfig {
        socket_path,
        agent,
        channel_factory,
        shutdown,
        mission_store,
        notify_dispatcher,
        default_notify_target,
        notify_targets,
        schedule_store,
        webhook_store,
        file_watch_store,
        webhook_port,
        web_ui_port,
        web_ui_host,
        web_ui_allowed_origins,
        web_ui_auth_token,
        comfyui_base_url,
        memory,
        memory_ttl_secs,
        audit_log,
        profile,
        persona_log,
        shared_persona,
        web_ui_broadcaster,
        persona_proposal_log,
        reflection_schedules,
        memory_retention,
        target_policies,
        embedding_provider,
        recall_log,
        helpfulness_ledger,
        cooccurrence_ledger,
        correction_ledger,
        persona_selection_stat,
        recall_cluster_stat,
        proactive_config,
        proactive_log,
        proactive_stat,
        persona_lifecycle_config,
        persona_lifecycle_stat,
        conversation_windows,
        persona_consolidation_config,
        persona_consolidation_stat,
        skill_refinement_config,
        skill_refinement_drafter,
        skill_authoring_config,
        skill_authoring_drafter,
        persona_consolidation_phraser,
        correction_consolidation_config,
        correction_consolidation_stat,
        correction_consolidation_phraser,
        recall_judgment_config,
        recall_judgment_stat,
        recall_judge,
        correction_judgment_config,
        correction_judge,
        correction_judgment_stat,
        correction_signal_config,
        recall_feedback_config,
        tool_descriptors,
        skill_auto_proposer,
        tool_relevance_ledger,
        skill_effectiveness_ledger,
        loop_backlog,
        loop_state,
        loop_config,
        team_missions,
        gate_policy,
        workspace_journaling_interval,
        pricing,
        config_toml_path,
        role_override,
        team_config_write_path,
        seed_draft_llm,
        document_roots,
        reminder_store,
        wiki_sweep,
        wiki_store,
        graph_sweep,
        graph_store,
        conflict_dismissals,
    } = config;
    // Chapter Codex (CX.3) — spawn the knowledge-wiki stale-page sweep on
    // the maintenance cadence when `[wiki].enabled`. Best-effort + shutdown-
    // aware; absent ⇒ no synthesis (byte-identical default).
    let _wiki_sweep_handle = wiki_sweep.map(|w| {
        let sweep_shutdown = shutdown.clone();
        tokio::spawn(crate::knowledge_wiki::run_wiki_sweep_loop(
            w.synthesizer,
            w.interval_secs,
            w.max_pages,
            sweep_shutdown,
        ))
    });
    // Chapter Lattice (LT.3) — spawn the typed-knowledge-graph extraction
    // sweep when `[graph].enabled`. Best-effort + shutdown-aware; absent ⇒
    // no extraction (byte-identical default).
    let _graph_sweep_handle = graph_sweep.map(|g| {
        let sweep_shutdown = shutdown.clone();
        tokio::spawn(crate::knowledge_graph::run_graph_sweep_loop(
            g.extractor,
            g.interval_secs,
            g.max_topics,
            sweep_shutdown,
        ))
    });
    // Phase 102 — shared once into every per-connection
    // `ConnectionContext` so `GetToolStats` can list the tool set.
    let tool_descriptors: Arc<[ToolDescriptor]> = tool_descriptors.into();
    let socket_path = &socket_path;
    let _ = std::fs::remove_file(socket_path);

    if let Some(parent) = socket_path.parent() {
        create_dir_all_0700(parent).map_err(|e| DaemonError::Bind {
            path: parent.display().to_string(),
            source: e,
        })?;
    }

    // Phase 95 — shared per-schedule cadence stats (fired /
    // skipped counts, accumulated across the daemon lifetime).
    // Created once here; cloned into both the reflection-
    // scheduler spawn (writer) and per-connection contexts
    // (reader for `GetLearningInsights`).
    let cadence_stats = crate::reflection_scheduler::shared_recent_reflection_stats();

    let listener = bind_unix_socket_0600(socket_path).map_err(|e| DaemonError::Bind {
        path: socket_path.display().to_string(),
        source: e,
    })?;

    let pid_path = socket_path.with_extension("pid");
    let _pid_guard = PidGuard::write(&pid_path)?;

    // Crash-recovery detection (Phase 41 Task 4).
    let state_path = socket_path.with_extension("state");
    let recovery_notice = detect_crash_recovery(&state_path);
    if let Some(ref stale) = recovery_notice {
        eprintln!(
            "aivyx-pa daemon: detected unclean shutdown (pid {}, started at {}). \
             Lost sessions: {:?}, lost turns: {:?}",
            stale.pid, stale.started_at, stale.sessions, stale.in_flight_turns,
        );
    }
    let _state_guard = StateGuard::write(&state_path)?;
    let daemon_state = _state_guard.shared();

    // Shared trigger dispatch — all trigger subsystems (cron, webhook,
    // file-watch) share the same turn lock and agent/channel references.
    let mut trigger_dispatch =
        crate::trigger::TriggerDispatch::new(Arc::clone(&agent), Arc::clone(&channel_factory));
    if let Some(ref ms) = mission_store {
        trigger_dispatch = trigger_dispatch.with_mission_store(ms.clone());
    }
    // Phase 63 Task 3 — auto-notify on trigger fire if the
    // operator configured `notify_target` on the trigger.
    trigger_dispatch = trigger_dispatch.with_default_notify_target(default_notify_target.clone());
    if let Some(ref nd) = notify_dispatcher {
        trigger_dispatch = trigger_dispatch.with_notify_dispatcher(Arc::clone(nd));
    }
    // Phase 67 — audit auto-notify dispatches into the same
    // persistent chain that records TurnStarted/TurnEnded, so
    // forensic walks see the complete trigger-fire-to-notify
    // story for each schedule fire.
    if let Some(ref al) = audit_log {
        trigger_dispatch = trigger_dispatch.with_audit_log(Arc::clone(al));
    }
    // Phase 73 — per-target retry + rate-limit policy map. Empty
    // map → every dispatch uses the zero-retry / no-rate-limit
    // defaults (today's behavior). Always called even with an
    // empty map so the dispatcher's internal `target_policies`
    // is set authoritatively from config at startup.
    trigger_dispatch = trigger_dispatch.with_target_policies(target_policies);

    // Keep a clone of the schedule store for the read-only `GetSchedules`
    // query (the Command Center routines panel); the original is moved into
    // the scheduler task below.
    let query_schedule_store = schedule_store.clone();
    // Spawn the scheduler loop if a schedule store is provided.
    let _scheduler_handle = schedule_store.map(|store| {
        let sched_dispatch = trigger_dispatch.clone();
        let sched_shutdown = shutdown.clone();
        // Chapter Ledger — the deterministic-digest context, built where the
        // memory + proposal handles live. `report_kind = "digest"` schedules use
        // this instead of an LLM turn (so the digest can't confabulate, #6).
        let report_ctx = memory.clone().map(|mem| {
            let mut builder = crate::digest::WeeklyDigestBuilder::new(mem);
            if let Some(p) = &persona_proposal_log {
                builder = builder.with_proposals(Arc::clone(p));
            }
            crate::daemon_scheduler::ReportContext {
                digest: Arc::new(builder),
                notify: notify_dispatcher.clone(),
                default_notify_target: default_notify_target.clone(),
            }
        });
        // Chapter Muster — the same live TeamMissionService the daemon's IPC
        // handlers and loop-delegation path already share, so a team-mission
        // schedule fires through `TeamMissionService::start_from_goal_for_schedule`
        // instead of an LLM tool-call in the loop.
        let sched_team_missions = team_missions.clone();
        tokio::spawn(async move {
            crate::daemon_scheduler::run_scheduler(
                sched_dispatch,
                store,
                sched_shutdown,
                report_ctx,
                sched_team_missions,
            )
            .await;
        })
    });

    // Spawn the webhook HTTP listener if a webhook store is provided.
    let _webhook_handle = webhook_store.map(|store| {
        let wh_dispatch = trigger_dispatch.clone();
        let wh_shutdown = shutdown.clone();
        let port = webhook_port.unwrap_or(crate::webhook_listener::DEFAULT_WEBHOOK_PORT);
        tokio::spawn(async move {
            if let Err(e) =
                crate::webhook_listener::run_webhook_listener(wh_dispatch, store, port, wh_shutdown)
                    .await
            {
                eprintln!("aivyx-pa webhook listener error: {e}");
            }
        })
    });

    // Spawn the file-watch loop if a file-watch store is provided.
    let _file_watch_handle = file_watch_store.map(|store| {
        let fw_dispatch = trigger_dispatch.clone();
        let fw_shutdown = shutdown.clone();
        tokio::spawn(async move {
            crate::file_watcher::run_file_watcher(fw_dispatch, store, fw_shutdown).await;
        })
    });

    // Phase 173 — spawn the autonomous-loop driver iff the
    // `[loop]` section is armed (loop_state is `Some`) AND the
    // backlog is present. The driver idles (no CPU) until an
    // `aivyx-pa loop start` flips the shared run state; it then
    // fires TriggerSource::Loop turns until the backlog drains
    // or the max-iterations cap is hit.
    let _loop_driver_handle = match (&loop_state, &loop_backlog) {
        (Some(state), Some(backlog)) => {
            let ld_dispatch = trigger_dispatch.clone();
            let ld_backlog = Arc::clone(backlog);
            let ld_state = state.clone();
            let ld_shutdown = shutdown.clone();
            // Phase 174 — build the gate runner from the armed
            // `[loop]` config (gate_command + working_dir +
            // timeout). `None` → no driver-side verification.
            let ld_gate: Option<Arc<dyn crate::loop_gate::GateRunner>> =
                loop_config.as_ref().and_then(|c| {
                    c.gate_command.as_ref().map(|cmd| {
                        Arc::new(crate::loop_gate::ShellGateRunner::new(
                            cmd.clone(),
                            c.working_dir.as_ref().map(std::path::PathBuf::from),
                            std::time::Duration::from_secs(c.gate_timeout_secs),
                        )) as Arc<dyn crate::loop_gate::GateRunner>
                    })
                });
            let ld_max_run_secs = loop_config.as_ref().and_then(|c| c.max_run_secs);
            // Phase 175 — the progress log: the driver reads
            // recent notes from the shared memory handle and
            // injects them into each iteration's prompt.
            let ld_memory = memory.clone();
            let ld_progress_inject = loop_config
                .as_ref()
                .map(|c| c.progress_inject_count)
                .unwrap_or(0);
            eprintln!(
                "aivyx-pa loop: driver armed (max_iterations ceiling={}, \
                 gate={}, max_run_secs={:?}, progress_inject={})",
                loop_config.as_ref().map(|c| c.max_iterations).unwrap_or(0),
                if ld_gate.is_some() { "on" } else { "off" },
                ld_max_run_secs,
                ld_progress_inject,
            );
            // Phase 176 — the token budget: the driver sums
            // TurnEnded usage from the audit chain over the run
            // window. No audit log → no budget enforcement.
            let ld_audit = audit_log.clone();
            let ld_max_run_tokens = loop_config.as_ref().and_then(|c| c.max_run_tokens);
            // Chapter K — the per-run dollar cap.
            let ld_max_run_usd = loop_config.as_ref().and_then(|c| c.max_run_usd);
            // K.4.2 — the override-aware rate table prices the run-window
            // spend, so a `[pricing.<model>]` custom rate advances the cap
            // instead of the under-counting built-in default.
            let ld_pricing = pricing.clone();
            // Chapter Circuit (CI.1) — the cross-iteration stall breaker
            // threshold (0 = disabled). No `[loop]` config → 0 (no driver
            // is spawned in that case anyway).
            let ld_max_idle = loop_config
                .as_ref()
                .map(|c| c.max_idle_iterations)
                .unwrap_or(0);
            // Chapter Foreman — arm deterministic auto-delegation iff
            // `[loop] delegate_above` is set AND a team service exists.
            let ld_delegate = match (
                loop_config.as_ref().and_then(|c| c.delegate_above),
                &team_missions,
            ) {
                (Some(threshold), Some(svc)) => Some((std::sync::Arc::new(svc.clone()), threshold)),
                _ => None,
            };
            // Verdict for delegated stories — when `[loop] verify_completion` is on,
            // give the driver a judge so an auto-delegated mission's result is
            // gated against the story's acceptance criteria (parity with solo
            // `loop.complete`). Built from the team service's own provider/model.
            let ld_judge = match (
                loop_config
                    .as_ref()
                    .map(|c| c.verify_completion)
                    .unwrap_or(false),
                &team_missions,
            ) {
                (true, Some(svc)) => Some(std::sync::Arc::new(svc.completion_judge())),
                _ => None,
            };
            Some(tokio::spawn(async move {
                crate::loop_driver::run_loop_driver(
                    ld_dispatch,
                    ld_backlog,
                    ld_state,
                    ld_gate,
                    ld_max_run_secs,
                    ld_memory,
                    ld_progress_inject,
                    ld_audit,
                    ld_max_run_tokens,
                    ld_max_run_usd,
                    ld_pricing,
                    ld_max_idle,
                    ld_shutdown,
                    ld_delegate,
                    ld_judge,
                )
                .await;
            }))
        }
        _ => None,
    };

    // Chapter Helm (Opp F) — opt-in auto-resume. If `[loop] resume_on_boot`
    // is set, a run was active when the daemon last stopped (the persisted
    // marker — a crash / `systemctl restart`, NOT an explicit `loop stop`),
    // and the backlog still has pending stories, kick off a run so a
    // "runs for days" agent under `Restart=on-failure` keeps working instead
    // of silently stalling. The just-spawned driver picks up `request_start`'s
    // notify. Best-effort: any miss just means the operator runs `loop start`.
    if let (Some(state), Some(backlog), Some(cfg)) = (&loop_state, &loop_backlog, &loop_config) {
        if cfg.resume_on_boot {
            let marker_active = state.persisted_run_active().await;
            let pending = backlog.remaining_count();
            if crate::loop_resume::should_resume_on_boot(cfg.resume_on_boot, marker_active, pending)
            {
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                if state.request_start(cfg.max_iterations, now_ms) {
                    eprintln!(
                        "aivyx-pa loop: resume_on_boot — resuming an interrupted \
                         run ({pending} pending stories)"
                    );
                }
            } else if marker_active {
                eprintln!(
                    "aivyx-pa loop: resume_on_boot set, but the backlog is empty \
                     — nothing to resume"
                );
            }
        }
    }

    // Phase 71 — spawn the reflection scheduler if any
    // `[[reflection_schedule]]` entries are configured AND an
    // audit log is available (the loop reads the chain to
    // build outcome summaries). If either prerequisite is
    // missing the task is simply not spawned; the config block
    // sits idle.
    // Chapter O.5 — proactive journaling. Spawn the re-arming journaling
    // driver when an interval is configured AND an audit log is present (the
    // driver reads the chain to decide whether there was recent activity).
    let _workspace_journal_handle = match (workspace_journaling_interval, audit_log.as_ref()) {
        (Some(interval), Some(al)) => {
            let wj_dispatch = trigger_dispatch.clone();
            let wj_audit = Arc::clone(al);
            let wj_shutdown = shutdown.clone();
            Some(tokio::spawn(async move {
                crate::workspace_journal::run_workspace_journal_driver(
                    wj_dispatch,
                    wj_audit,
                    interval,
                    wj_shutdown,
                )
                .await;
            }))
        }
        _ => None,
    };

    let _reflection_scheduler_handle = match (audit_log.as_ref(), reflection_schedules.is_empty()) {
        (Some(al), false) => {
            let rs_dispatch = trigger_dispatch.clone();
            let rs_shutdown = shutdown.clone();
            let rs_audit = Arc::clone(al);
            let rs_schedules = reflection_schedules.clone();
            let rs_cadence_stats = cadence_stats.clone();
            // Phase 77 — bundle the recall→reflection feedback
            // deps iff the whole substrate is present (recall
            // log + memory + proposal chain). Any missing piece
            // → `None` → the feedback pass is skipped while the
            // reflection turn still fires normally.
            let rs_recall_feedback = match (
                recall_log.clone(),
                memory.clone(),
                persona_proposal_log.clone(),
            ) {
                (Some(rl), Some(mem), Some(pl)) => {
                    Some(crate::reflection_scheduler::RecallFeedbackDeps {
                        recall_log: rl,
                        memory: mem,
                        proposal_log: pl,
                        gc_retain_secs: crate::recall_feedback::RECALL_LOG_RETAIN_SECS,
                        // Phase 82 — fold each window into the
                        // durable ledger when the substrate is
                        // present (zero-config, like the recall
                        // log itself).
                        helpfulness_ledger: helpfulness_ledger.clone(),
                        // Phase 83 — fold each window's pairs
                        // into the durable co-occurrence
                        // ledger (zero-config, same substrate).
                        cooccurrence_ledger: cooccurrence_ledger.clone(),
                        // Phase 172 — fold each window's per-topic
                        // correction counts into the durable
                        // correction ledger (zero-config, same
                        // substrate).
                        correction_ledger: correction_ledger.clone(),
                        // Phase 178 — the correction judge, armed
                        // only when `[correction_judgment]` is
                        // enabled AND a judge was built. `None` →
                        // the Phase 172 structural fold.
                        correction_judge: if correction_judgment_config
                            .as_ref()
                            .map(|c| c.enabled)
                            .unwrap_or(false)
                        {
                            correction_judge.clone()
                        } else {
                            None
                        },
                        correction_judgment_max: correction_judgment_config
                            .as_ref()
                            .map(|c| c.max_corrections_per_cycle)
                            .unwrap_or(0),
                        correction_judgment_stat: correction_judgment_stat.clone(),
                        // Phase 179 — opt-in tool correction
                        // attribution from `[correction_signal]`.
                        attribute_tool_corrections: correction_signal_config
                            .as_ref()
                            .map(|c| c.attribute_tools)
                            .unwrap_or(false),
                        // Phase 93 — per-hit judgment override
                        // when `[recall_feedback].use_judgment_signal
                        // = true`. Absent section → `false`
                        // (byte-identical to pre-Phase-93).
                        use_judgment_signal: recall_feedback_config
                            .as_ref()
                            .map(|c| c.use_judgment_signal)
                            .unwrap_or(false),
                    })
                }
                _ => None,
            };
            // Phase 80 — proactive deps: armed only when the
            // section is enabled AND the substrate is present.
            let rs_proactive = match (
                proactive_config.clone(),
                memory.clone(),
                proactive_log.clone(),
                notify_dispatcher.clone(),
            ) {
                (Some(cfg), Some(mem), Some(plog), Some(nd)) if cfg.enabled => {
                    Some(crate::reflection_scheduler::ProactiveDeps {
                        config: cfg,
                        memory: mem,
                        proactive_log: plog,
                        notify: nd,
                        recall_log: recall_log.clone(),
                        memory_ttl_secs,
                        gc_retain_secs: crate::proactive_log::PROACTIVE_LOG_RETAIN_SECS,
                        stat: proactive_stat.clone(),
                    })
                }
                _ => None,
            };
            // Phase 81 — persona-lifecycle deps: armed only
            // when the section is enabled AND the persona
            // substrate (chain + proposal chain + embedding)
            // is present. Any missing piece → None → the pass
            // is skipped while reflection still fires.
            let rs_persona_lifecycle = match (
                persona_lifecycle_config.clone(),
                persona_log.clone(),
                persona_proposal_log.clone(),
                embedding_provider.clone(),
            ) {
                (Some(cfg), Some(plog), Some(pplog), Some(emb)) if cfg.enabled => {
                    Some(crate::reflection_scheduler::PersonaLifecycleDeps {
                        config: cfg,
                        persona_log: plog,
                        proposal_log: pplog,
                        embedding: emb,
                        // Phase 85 — gate decay by durable
                        // topic helpfulness when available
                        // (already wired for Phase 82).
                        helpfulness_ledger: helpfulness_ledger.clone(),
                        // Phase 88 — gate decay by durable
                        // pair affinity when available
                        // (already wired for Phase 83);
                        // `None` → pure age-only fallback
                        // for `consolidate-pair:` facets.
                        cooccurrence_ledger: cooccurrence_ledger.clone(),
                        stat: persona_lifecycle_stat.clone(),
                    })
                }
                _ => None,
            };
            // Phase 87 — pattern-driven Persona consolidation
            // deps: armed only when the section is enabled AND
            // every substrate is present (co-occurrence ledger
            // + helpfulness ledger + proposal chain + an LLM
            // phraser the binary builds with the existing
            // reflection LLM provider). Any missing piece →
            // None → the pass is skipped while reflection
            // still fires (byte-identical to pre-Phase-87).
            let rs_persona_consolidation = match (
                persona_consolidation_config.clone(),
                cooccurrence_ledger.clone(),
                helpfulness_ledger.clone(),
                persona_proposal_log.clone(),
                persona_consolidation_phraser.clone(),
            ) {
                (Some(cfg), Some(cooc), Some(helps), Some(plog), Some(phraser)) if cfg.enabled => {
                    Some(crate::reflection_scheduler::PersonaConsolidationDeps {
                        config: cfg,
                        cooccurrence_ledger: cooc,
                        helpfulness_ledger: helps,
                        proposal_log: plog,
                        phraser,
                        stat: persona_consolidation_stat.clone(),
                        // Phase 92 — Persona chain handle +
                        // Phase 88 floor. The binary fills
                        // these so the supersession-detection
                        // branch (gated on
                        // `config.enable_supersession`) can
                        // walk applied `consolidate-pair:`
                        // facets. The bin/aivyx wiring uses
                        // the operator's actual
                        // `[persona_lifecycle].decay_pair_
                        // below_affinity` when present;
                        // None / 1.0 here is the default
                        // (the same default as Phase 88).
                        persona_log: persona_log.clone(),
                        pair_below_affinity: persona_lifecycle_config
                            .as_ref()
                            .map(|c| c.decay_pair_below_affinity)
                            .unwrap_or(aivyx_config::DEFAULT_PL_DECAY_PAIR_BELOW_AFFINITY),
                    })
                }
                _ => None,
            };
            // Phase 172 — correction-consolidation deps: armed
            // only when the section is enabled AND the substrate
            // is present (correction ledger + proposal chain + an
            // LLM `TopicPhraser`). Any missing piece → None → the
            // pass is skipped (the correction ledger still
            // accumulates passively; no proposals are filed).
            let rs_correction_consolidation = match (
                correction_consolidation_config.clone(),
                correction_ledger.clone(),
                persona_proposal_log.clone(),
                correction_consolidation_phraser.clone(),
            ) {
                (Some(cfg), Some(ledger), Some(plog), Some(phraser)) if cfg.enabled => {
                    Some(crate::reflection_scheduler::CorrectionConsolidationDeps {
                        config: cfg,
                        correction_ledger: ledger,
                        proposal_log: plog,
                        phraser,
                        stat: correction_consolidation_stat.clone(),
                    })
                }
                _ => None,
            };
            // Phase 91 — LLM-judged recall deps: armed only
            // when the section is enabled AND every substrate
            // is present (recall log + memory + an
            // `LlmRecallJudge` the binary built with the
            // existing reflection LLM provider). Any missing
            // piece → None → the pass is skipped (the Phase 77
            // structural signal remains the only signal,
            // byte-identical to pre-Phase-91).
            let rs_recall_judgment = match (
                recall_judgment_config.clone(),
                recall_log.clone(),
                memory.clone(),
                recall_judge.clone(),
            ) {
                (Some(cfg), Some(rlog), Some(mem), Some(judge)) if cfg.enabled => {
                    Some(crate::reflection_scheduler::RecallJudgmentDeps {
                        config: cfg,
                        recall_log: rlog,
                        memory: mem,
                        judge,
                        stat: recall_judgment_stat.clone(),
                    })
                }
                _ => None,
            };
            // Chapter Whetstone (WH.3c) — skill-refinement deps: armed
            // only when [skill_refinement] is enabled AND every piece is
            // present (the effectiveness ledger + the proposal/persona
            // chains + an LlmRefinementDrafter). Any missing piece → None →
            // the pass is skipped (the ledger still accumulates passively).
            let rs_skill_refinement = match (
                skill_refinement_config.clone(),
                skill_effectiveness_ledger.clone(),
                persona_proposal_log.clone(),
                persona_log.clone(),
                skill_refinement_drafter.clone(),
            ) {
                (Some(cfg), Some(ledger), Some(plog), Some(persona), Some(drafter))
                    if cfg.enabled =>
                {
                    Some(crate::reflection_scheduler::SkillRefinementDeps {
                        config: cfg,
                        ledger,
                        proposal_log: plog,
                        persona_log: persona,
                        drafter,
                        retrofold_watermark: std::sync::Mutex::new(std::collections::HashMap::new()),
                    })
                }
                _ => None,
            };
            // 2026-07-04 dogfood (#6) — topics skill authoring must never
            // draw from: every configured routine's name (reflection +
            // synced cron schedules; store records are "cfg-<name>"). A
            // routine's memory writes are the agent's own journal, not
            // operator-domain knowledge — the first live Praxis pass
            // authored a skill from the nightly-reflection routine's
            // writes. Best-effort: a store read failure just narrows the
            // exclusion to the reflection names.
            let mut sa_excluded_topics: std::collections::HashSet<String> =
                rs_schedules.iter().map(|s| s.name.clone()).collect();
            if let Some(store) = query_schedule_store.as_ref() {
                if let Ok(records) = crate::schedule::list_schedules(store).await {
                    for r in records {
                        let name = r.schedule_id.strip_prefix("cfg-").unwrap_or(&r.schedule_id);
                        sa_excluded_topics.insert(name.to_string());
                    }
                }
            }
            // Chapter Praxis (PX.2) — skill-authoring deps: armed only when
            // [skill_authoring] is enabled AND every piece is present (the
            // wiki + graph stores + the proposal/persona chains + an
            // LlmSpecializationDrafter). Any missing piece → None → skipped.
            let rs_skill_authoring = match (
                skill_authoring_config.clone(),
                wiki_store.clone(),
                graph_store.clone(),
                memory.clone(),
                persona_proposal_log.clone(),
                persona_log.clone(),
                skill_authoring_drafter.clone(),
            ) {
                (
                    Some(cfg),
                    Some(wiki),
                    Some(graph),
                    Some(mem),
                    Some(plog),
                    Some(persona),
                    Some(drafter),
                ) if cfg.enabled => Some(crate::reflection_scheduler::SkillAuthoringDeps {
                    config: cfg,
                    wiki_store: wiki,
                    graph_store: graph,
                    memory: mem,
                    proposal_log: plog,
                    persona_log: persona,
                    drafter,
                    excluded_topics: sa_excluded_topics,
                }),
                _ => None,
            };
            for sched in &rs_schedules {
                eprintln!(
                    "aivyx-pa reflection schedule {:?} registered (cron={:?}, \
                     lookback={}s)",
                    sched.name, sched.cron, sched.lookback_window_secs,
                );
            }
            Some(tokio::spawn(async move {
                crate::reflection_scheduler::run_reflection_scheduler(
                    rs_schedules,
                    rs_dispatch,
                    rs_audit,
                    rs_recall_feedback,
                    rs_proactive,
                    rs_persona_lifecycle,
                    rs_persona_consolidation,
                    rs_correction_consolidation,
                    rs_recall_judgment,
                    rs_skill_refinement,
                    rs_skill_authoring,
                    rs_cadence_stats,
                    rs_shutdown,
                )
                .await;
            }))
        }
        (None, false) => {
            eprintln!(
                "aivyx-pa daemon: {} [[reflection_schedule]] entries configured \
                 but no audit log is available — reflection scheduler not \
                 spawned (outcome summaries require the audit chain)",
                reflection_schedules.len(),
            );
            None
        }
        _ => None,
    };

    // Spawn the web UI server if a port is configured.
    let _web_ui_handle = web_ui_port.map(|port| {
        let web_shutdown = shutdown.clone();
        let web_socket_path = socket_path.to_path_buf();
        let web_broadcaster = web_ui_broadcaster.clone();
        let web_origins = web_ui_allowed_origins.clone();
        let web_auth_token = web_ui_auth_token.clone();
        let web_comfyui_base_url = comfyui_base_url.clone();
        tokio::spawn(async move {
            if let Err(e) = crate::web_ui::run_web_ui_server(
                web_socket_path,
                web_ui_host,
                port,
                web_origins,
                web_auth_token,
                web_comfyui_base_url,
                web_shutdown,
                web_broadcaster,
            )
            .await
            {
                eprintln!("aivyx-pa web ui error: {e}");
            }
        })
    });

    // Spawn the memory-GC timer if a TTL is configured OR if any
    // `[[memory.retention]]` rules are declared (Phase 74). Runs
    // every hour. Path A (retention rules present): walks every
    // entry, finds the first matching rule, applies its policy.
    // Unmatched entries fall through to the global
    // `memory_ttl_secs` cutoff (or are kept if neither matches
    // nor a default TTL exists). Path B (no rules, just TTL):
    // original Phase 42 behavior, every entry checked against
    // the single global cutoff.
    let _memory_gc_handle = {
        let needs_gc = memory_ttl_secs.is_some() || !memory_retention.is_empty();
        // Phase 75 — the same hourly timer also drives the
        // embedding backfill, so it must spawn when a provider
        // is configured even if no TTL/retention GC is.
        let needs_backfill = embedding_provider.is_some();
        let mem_arc = if needs_gc || needs_backfill {
            memory.clone()
        } else {
            None
        };
        if let (true, Some(mem)) = (needs_gc || needs_backfill, mem_arc) {
            let gc_shutdown = shutdown.clone();
            let rules = memory_retention.clone();
            let ttl = memory_ttl_secs;
            let backfill_provider = embedding_provider.clone();
            Some(tokio::spawn(async move {
                let mut interval = tokio::time::interval(std::time::Duration::from_secs(3600));
                // The first tick fires immediately — skip it so the
                // first GC runs after one hour of uptime, not at
                // startup.
                interval.tick().await;
                loop {
                    tokio::select! {
                        _ = interval.tick() => {
                          if needs_gc {
                            let now = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs();
                            // Resolve the default cutoff from the
                            // optional global TTL.
                            let default_cutoff =
                                ttl.map(|t| now.saturating_sub(t));
                            // Build precomputed RetentionMatcher
                            // slice from the rules; cutoff_secs is
                            // None for Forever, Some(now - days*86400)
                            // for ForDays. The closure wraps each
                            // rule's GlobMatcher into the
                            // `&dyn Fn(&str) -> bool` shape the
                            // memory crate's RetentionMatcher
                            // expects.
                            type GlobClosure =
                                Box<dyn Fn(&str) -> bool + Send + Sync>;
                            let closures: Vec<GlobClosure> = rules
                                .iter()
                                .map(|r| {
                                    let m = r.matcher.clone();
                                    Box::new(move |topic: &str| m.is_match(topic))
                                        as Box<
                                            dyn Fn(&str) -> bool + Send + Sync,
                                        >
                                })
                                .collect();
                            let matchers: Vec<aivyx_memory::RetentionMatcher<'_>> =
                                rules.iter().enumerate().map(|(i, r)| {
                                    let cutoff = match r.retention {
                                        aivyx_config::RetentionPolicy::Forever => None,
                                        aivyx_config::RetentionPolicy::ForDays(days) => {
                                            Some(now.saturating_sub(days.saturating_mul(86400)))
                                        }
                                    };
                                    aivyx_memory::RetentionMatcher {
                                        matches: closures[i].as_ref(),
                                        cutoff_secs: cutoff,
                                    }
                                }).collect();
                            let result = if matchers.is_empty() {
                                // No rules → keep the existing
                                // global-TTL path. default_cutoff is
                                // unwrap-able here because !needs_gc
                                // checked above would have skipped
                                // the spawn entirely otherwise.
                                if let Some(cutoff) = default_cutoff {
                                    mem.gc_expired(cutoff).await
                                } else {
                                    Ok(0)
                                }
                            } else {
                                mem.gc_expired_with_rules(
                                    &matchers,
                                    default_cutoff,
                                )
                                .await
                            };
                            match result {
                                Ok(n) if n > 0 => {
                                    eprintln!(
                                        "aivyx-pa memory gc: expired {n} entries \
                                         ({} rule(s) applied)",
                                        rules.len(),
                                    );
                                }
                                Ok(_) => {}
                                Err(e) => {
                                    eprintln!("aivyx-pa memory gc error: {e}");
                                }
                            }
                          }
                          // Phase 75 — lazy embedding backfill on
                          // the same hourly cadence. Bounded per
                          // tick; provider failure is non-fatal
                          // (the pass returns Ok(0) and retries
                          // next hour).
                          if let Some(provider) = &backfill_provider {
                              match crate::memory_embedding::run_backfill_pass(
                                  &mem, provider,
                              )
                              .await
                              {
                                  Ok(n) if n > 0 => {
                                      eprintln!(
                                          "aivyx-pa memory embed: backfilled \
                                           {n} vector(s)"
                                      );
                                  }
                                  Ok(_) => {}
                                  Err(e) => {
                                      eprintln!(
                                          "aivyx-pa memory embed backfill \
                                           error: {e}"
                                      );
                                  }
                              }
                          }
                        }
                        _ = gc_shutdown.cancelled() => break,
                    }
                }
            }))
        } else {
            None
        }
    };

    let mission_store = mission_store.map(Arc::new);
    let query_schedule_store = query_schedule_store.map(Arc::new);
    let notify_targets = Arc::new(notify_targets);
    let pending_recovery: Arc<std::sync::Mutex<Option<DaemonState>>> =
        Arc::new(std::sync::Mutex::new(recovery_notice));
    let mut handles = Vec::new();

    // Piece C (2026-08-23) — build the daemon's own per-channel-type
    // `/team run` authorization once at startup, re-reading the same
    // `aivyx-pa.toml` this process itself loaded (`config_toml_path`) —
    // deliberately not trusting anything the connecting channel-adapter
    // process claims about its own authorization. `None` (env-only
    // launch, no config file) or a failed re-read both fail closed to
    // all-`false` (`ChannelTriggerAuthz::default()`), never fail-open.
    //
    // Formerly a known gap (review finding I2): this re-read used to call
    // `load_settings_config` with `role_override: None` hardcoded, so a
    // daemon started with a non-default `--role` against a config with no
    // role literally named "default" could see this re-read's role
    // resolution diverge from the primary load's and fail with
    // `ConfigError::UnknownRole` — closed by threading `DaemonConfig`'s
    // own `role_override` field through (see its doc comment). Any
    // failure logged below now genuinely indicates something else (a
    // missing/malformed file, bad permissions, or a role renamed between
    // the primary load and this re-read), not a stale-override blind
    // spot.
    let channel_trigger_authz = match config_toml_path.as_deref() {
        Some(p) => match load_settings_config(p, role_override.as_deref()) {
            Ok(cfg) => ChannelTriggerAuthz {
                telegram: cfg
                    .telegram
                    .as_ref()
                    .map(|t| t.team_run_channel)
                    .unwrap_or(false),
                discord: cfg
                    .discord
                    .as_ref()
                    .map(|d| d.team_run_channel)
                    .unwrap_or(false),
                slack: cfg
                    .slack
                    .as_ref()
                    .map(|s| s.team_run_channel)
                    .unwrap_or(false),
            },
            Err(e) => {
                eprintln!(
                    "aivyx-pa daemon: WARNING — failed to re-read {} for /team run channel \
                     authorization: {e}; falling back to all-channels-denied (fail closed)",
                    p.display()
                );
                ChannelTriggerAuthz::default()
            }
        },
        None => ChannelTriggerAuthz::default(),
    };

    // Finding I3(a) — an operator has no other way to confirm what
    // the daemon actually granted; log it once at startup next to
    // the other startup-time daemon state.
    eprintln!(
        "aivyx-pa daemon: /team run channel authorization — telegram: {}, discord: {}, slack: {}",
        channel_trigger_authz.telegram, channel_trigger_authz.discord, channel_trigger_authz.slack
    );

    loop {
        let (stream, _addr) = tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok(conn) => conn,
                    Err(e) => {
                        eprintln!("aivyx-pa daemon: accept error: {e}");
                        continue;
                    }
                }
            }
            _ = shutdown.cancelled() => {
                break;
            }
        };

        let ctx = ConnectionContext {
            stream,
            agent: Arc::clone(&agent),
            channel_factory: Arc::clone(&channel_factory),
            shutdown: shutdown.clone(),
            mission_store: mission_store.clone(),
            schedule_store: query_schedule_store.clone(),
            notify_targets: Arc::clone(&notify_targets),
            pending_recovery: Arc::clone(&pending_recovery),
            daemon_state: Arc::clone(&daemon_state),
            audit_log: audit_log.clone(),
            profile: Arc::clone(&profile),
            persona_log: persona_log.clone(),
            shared_persona: shared_persona.clone(),
            persona_proposal_log: persona_proposal_log.clone(),
            memory: memory.clone(),
            embedding_provider: embedding_provider.clone(),
            recall_log: recall_log.clone(),
            helpfulness_ledger: helpfulness_ledger.clone(),
            cooccurrence_ledger: cooccurrence_ledger.clone(),
            wiki_store: wiki_store.clone(),
            graph_store: graph_store.clone(),
            conflict_dismissals: conflict_dismissals.clone(),
            correction_ledger: correction_ledger.clone(),
            persona_selection_stat: persona_selection_stat.clone(),
            recall_cluster_stat: recall_cluster_stat.clone(),
            proactive_stat: proactive_stat.clone(),
            persona_lifecycle_stat: persona_lifecycle_stat.clone(),
            conversation_windows: conversation_windows.clone(),
            persona_consolidation_stat: persona_consolidation_stat.clone(),
            correction_consolidation_stat: correction_consolidation_stat.clone(),
            correction_judgment_stat: correction_judgment_stat.clone(),
            recall_judgment_stat: recall_judgment_stat.clone(),
            recall_feedback_config: recall_feedback_config.clone(),
            cadence_stats: cadence_stats.clone(),
            tool_descriptors: Arc::clone(&tool_descriptors),
            skill_auto_proposer: skill_auto_proposer.clone(),
            tool_relevance_ledger: tool_relevance_ledger.clone(),
            skill_effectiveness_ledger: skill_effectiveness_ledger.clone(),
            loop_backlog: loop_backlog.clone(),
            loop_state: loop_state.clone(),
            loop_config: loop_config.clone(),
            team_missions: team_missions.clone(),
            gate_policy,
            channel_trigger_authz,
            config_toml_path: config_toml_path.clone(),
            role_override: role_override.clone(),
            team_config_write_path: team_config_write_path.clone(),
            seed_draft_llm: seed_draft_llm.clone(),
            document_roots: document_roots.clone(),
            reminder_store: reminder_store.clone(),
            comfyui_base_url: comfyui_base_url.clone(),
        };

        let handle = tokio::spawn(async move {
            if let Err(e) = handle_connection(ctx).await {
                // Backlog #4: a clean client hang-up (CLI query finishing
                // and dropping the socket) is an expected lifecycle event,
                // not a fault — don't spam it at error level.
                if !e.is_clean_disconnect() {
                    eprintln!("aivyx-pa daemon: connection handler error: {e}");
                }
            }
        });
        handles.push(handle);
    }

    for h in handles {
        let _ = h.await;
    }

    Ok(())
}

/// Piece C (2026-08-23) — the daemon's own, independently-loaded
/// per-channel-type authorization for `/team run <goal>`. Built once
/// at daemon startup from the same `aivyx-pa.toml` every process reads
/// (see the construction site below) — deliberately *not* trusting
/// anything the connecting channel-adapter process claims about its
/// own authorization, since that process is a separate, potentially
/// stale or misconfigured copy of the same config.
#[derive(Debug, Clone, Copy, Default)]
pub struct ChannelTriggerAuthz {
    pub telegram: bool,
    pub discord: bool,
    pub slack: bool,
}

/// Pure: does `authz` grant `platform` the right to start a new team
/// mission via `/team run`? `None` (no `StartSession` yet) or any
/// platform this feature doesn't recognize (Local/Rest/Voice/Email/
/// Matrix) is always denied — fail-closed, never fail-open on an
/// unrecognized or absent identity.
pub fn channel_trigger_authorized(
    authz: &ChannelTriggerAuthz,
    platform: Option<aivyx_core::ChannelPlatform>,
) -> bool {
    match platform {
        Some(aivyx_core::ChannelPlatform::Telegram) => authz.telegram,
        Some(aivyx_core::ChannelPlatform::Discord) => authz.discord,
        Some(aivyx_core::ChannelPlatform::Slack) => authz.slack,
        _ => false,
    }
}

/// Pure: the `triggered_by` tag a channel-started mission's record
/// carries. `register_mission_for_channel_trigger`'s own notify path
/// (`team_mission_driver.rs`) depends on this exact `"channel:"`
/// prefix to disambiguate a channel-triggered mission from a
/// schedule id — do not strip it here.
pub fn channel_trigger_tag(platform: Option<aivyx_core::ChannelPlatform>) -> String {
    match platform {
        Some(aivyx_core::ChannelPlatform::Telegram) => "channel:telegram".to_string(),
        Some(aivyx_core::ChannelPlatform::Discord) => "channel:discord".to_string(),
        Some(aivyx_core::ChannelPlatform::Slack) => "channel:slack".to_string(),
        _ => "channel:unknown".to_string(),
    }
}

/// Pure: the bare platform name for `AuditEvent::TeamMissionChannelTriggered`'s
/// `platform` field — Task 4's documented contract is a bare name
/// (`"telegram"`/`"discord"`/`"slack"`/`"unknown"`), *not* the
/// `"channel:"`-prefixed `channel_trigger_tag` form used for
/// `triggered_by`. Kept as a separate helper (rather than stripping
/// the prefix off `channel_trigger_tag`'s output at the call site) so
/// the two contracts can't accidentally drift back together.
pub fn channel_trigger_audit_platform(platform: Option<aivyx_core::ChannelPlatform>) -> String {
    match platform {
        Some(aivyx_core::ChannelPlatform::Telegram) => "telegram".to_string(),
        Some(aivyx_core::ChannelPlatform::Discord) => "discord".to_string(),
        Some(aivyx_core::ChannelPlatform::Slack) => "slack".to_string(),
        _ => "unknown".to_string(),
    }
}

/// Piece C — the real authorization + start decision for
/// `FrontendMessage::RunTeamMissionChannel`, extracted from the raw
/// wire-protocol read/write glue in `handle_connection` specifically
/// so it's directly testable without a live `UnixStream`/
/// `ConnectionContext` (no precedent for that exists anywhere in this
/// file — see this task's own "Testability note"). Checks
/// authorization *before* service-presence, deliberately: whether a
/// team-mission service even exists is irrelevant to an unauthorized
/// caller, and checking the cheaper, more restrictive gate first keeps
/// both branches independently testable with no service fixture
/// needed for the deny path.
async fn handle_run_team_mission_channel(
    svc: Option<&crate::team_mission_driver::TeamMissionService>,
    authz: &ChannelTriggerAuthz,
    platform: Option<aivyx_core::ChannelPlatform>,
    audit_log: Option<&PersistentAuditLog>,
    goal: String,
) -> DaemonMessage {
    if !channel_trigger_authorized(authz, platform) {
        // Finding I3(b), closed 2026-08-24 — a denied `/team run` used to
        // leave zero forensic trace beyond this eprintln. Now also
        // appended to the persistent audit chain, mirroring the success
        // branch's own TeamMissionChannelTriggered pattern below.
        eprintln!(
            "aivyx-pa daemon: /team run denied for channel {} (not authorized via \
             team_run_channel in aivyx-pa.toml)",
            channel_trigger_audit_platform(platform)
        );
        if let Some(log) = audit_log {
            if let Err(e) = log.append(aivyx_audit::AuditEvent::TeamMissionChannelDenied {
                platform: channel_trigger_audit_platform(platform),
                goal: goal.clone(),
                reason: "channel not authorized via team_run_channel in aivyx-pa.toml".into(),
            }) {
                eprintln!("aivyx-pa daemon: failed to audit denied channel team trigger: {e}");
            }
        }
        return DaemonMessage::Error {
            code: "team_run_channel_denied".into(),
            message: "this channel is not authorized to start team missions (operator \
                      opt-in required via team_run_channel in aivyx-pa.toml)"
                .into(),
        };
    }
    let Some(svc) = svc else {
        return DaemonMessage::Error {
            code: "no_team_missions".into(),
            message: "daemon has no team-mission service configured".into(),
        };
    };
    let tag = channel_trigger_tag(platform);
    match svc.start_from_goal_for_channel_trigger(&goal, None, &tag).await {
        Ok(mission_id) => {
            if let Some(log) = audit_log {
                if let Err(e) = log.append(aivyx_audit::AuditEvent::TeamMissionChannelTriggered {
                    platform: channel_trigger_audit_platform(platform),
                    goal: goal.clone(),
                    mission_id: mission_id.clone(),
                }) {
                    eprintln!("aivyx-pa daemon: failed to audit channel team trigger: {e}");
                }
            }
            DaemonMessage::TeamMissionChannelStarted { mission_id }
        }
        Err(e) => DaemonMessage::Error {
            code: "team_run_channel_failed".into(),
            message: e.to_string(),
        },
    }
}

/// Per-connection state the daemon hands to `handle_connection`.
///
/// Phase 51 Task 3 — lifted from `handle_connection`'s 8-parameter
/// signature into a parameter struct, same pattern Phase 41 Task 2
/// used for `DaemonConfig`. The `#[allow(clippy::too_many_arguments)]`
/// shortcut from Phase 47 Task 4 is gone.
struct ConnectionContext {
    stream: tokio::net::UnixStream,
    agent: Arc<dyn Agent>,
    channel_factory: ChannelFactory,
    shutdown: CancellationToken,
    mission_store: Option<Arc<DomainHandle>>,
    /// Read-only clone of the schedule store for the `GetSchedules` query.
    schedule_store: Option<Arc<DomainHandle>>,
    /// Chapter Herald — shared, read-only notify-target list for the
    /// `GetNotifyTargets` query. `Arc` since every connection clones it.
    notify_targets: Arc<Vec<aivyx_config::NotifyTargetConfig>>,
    pending_recovery: Arc<std::sync::Mutex<Option<DaemonState>>>,
    daemon_state: Arc<std::sync::Mutex<DaemonState>>,
    audit_log: Option<Arc<PersistentAuditLog>>,
    /// Phase 58 — operator-declared Profile snapshot for
    /// `Query::GetProfile`. Cloned-per-connection so the handler
    /// can read it without contending with the daemon's read path.
    profile: Arc<aivyx_config::Profile>,
    /// Phase 60 — persistent Persona log for inspection queries +
    /// revert append. `None` in test fixtures.
    persona_log: Option<Arc<crate::persona::PersistentPersonaLog>>,
    /// Phase 60 — shared effective Persona for inspection +
    /// recompute after revert append.
    shared_persona: crate::persona::SharedEffectivePersona,
    /// Phase 70 — persistent Persona proposal log for
    /// `ListPersonaProposals` / `GetPersonaProposal` queries +
    /// `ResolvePersonaProposal` status transitions. `None` in
    /// test fixtures.
    persona_proposal_log: Option<Arc<crate::persona_proposal::PersistentPersonaProposalLog>>,
    /// Phase 74 — memory substrate handle for the
    /// `ListMemoryTopics` / `GetMemoryTopicEntries` /
    /// `SearchMemory` queries + the `EvictMemoryTopic`
    /// frontend message. `None` in test fixtures.
    memory: Option<Arc<dyn aivyx_memory::Memory>>,
    /// Phase 75 — embedding provider for the `SearchMemory`
    /// semantic path. `None` = `[embedding]` not configured;
    /// a `mode = "semantic"` request transparently falls back
    /// to keyword.
    embedding_provider: Option<Arc<dyn aivyx_llm::embedding::EmbeddingProvider>>,
    /// Phase 78 — recall-feedback log for the read-only
    /// `GetLearningInsights` query. `None` = no auto-recall
    /// configured (the query returns an empty digest).
    recall_log: Option<Arc<crate::recall_log::PersistentRecallLog>>,
    /// Phase 82 — durable helpfulness ledger for the read-only
    /// `GetLearningInsights` longitudinal view. `None` = no
    /// auto-recall configured (no accumulated view).
    helpfulness_ledger: Option<Arc<crate::helpfulness_ledger::PersistentHelpfulnessLedger>>,
    /// Phase 83 — durable co-occurrence ledger for the
    /// read-only `GetLearningInsights` cross-session pattern
    /// view. `None` = no auto-recall configured.
    cooccurrence_ledger: Option<Arc<crate::cooccurrence_ledger::PersistentCooccurrenceLedger>>,
    /// Chapter Codex (CX.4) — read handle on the knowledge-wiki page
    /// store for the `ListWikiPages` / `GetWikiPage` read-only IPC.
    wiki_store: Option<Arc<crate::knowledge_wiki::PersistentWikiStore>>,
    /// Chapter Lattice (LT.5) — read handle on the typed-graph store for
    /// the `GetKnowledgeGraph` read-only IPC.
    graph_store: Option<Arc<crate::knowledge_graph::PersistentGraphStore>>,
    /// Chapter Concord — dismissed-conflict set for the `GetMemoryConflicts`
    /// filter + `DismissMemoryConflict` handler.
    conflict_dismissals: Option<Arc<crate::conflict_dismissals::PersistentConflictDismissals>>,
    /// Phase 172 — durable correction ledger for the read-only
    /// `GetLearningInsights` accumulated-corrections view.
    /// `None` = no auto-recall configured.
    correction_ledger: Option<Arc<crate::correction_ledger::PersistentCorrectionLedger>>,
    /// Phase 79 (Q4a) — last-Persona-selection stat for the
    /// `GetLearningInsights` surface.
    persona_selection_stat: Option<crate::persona_context::SharedPersonaSelectionStat>,
    /// Phase 84 (Q4a) — last-turn cluster-recall stat for the
    /// `GetLearningInsights` surface.
    recall_cluster_stat: Option<crate::memory_recall::SharedRecallClusterStat>,
    /// Phase 80 (Q4a) — last-proactive-cycle stat for the
    /// `GetLearningInsights` surface.
    proactive_stat: Option<crate::proactive_detect::SharedProactiveStat>,
    /// Phase 81 (Q4a) — last-persona-lifecycle-cycle stat for
    /// the `GetLearningInsights` surface.
    persona_lifecycle_stat: Option<crate::persona_lifecycle::SharedPersonaLifecycleStat>,
    /// Phase 86 — per-session conversation windows. `Some` →
    /// the turn loop appends `(user, assistant)` pairs on
    /// `TurnOutcome::Completed` so both relevance providers can
    /// embed a multi-turn query.
    conversation_windows: Option<crate::conversation_window::SharedConversationWindows>,
    /// Phase 87 (Q4a) — last-reflection-cycle pattern-driven
    /// Persona consolidation stat for the
    /// `GetLearningInsights` surface.
    persona_consolidation_stat:
        Option<crate::persona_consolidation::SharedPersonaConsolidationStat>,
    /// Phase 172 (Q4a) — last-reflection-cycle correction-driven
    /// consolidation stat for the `GetLearningInsights` surface.
    correction_consolidation_stat:
        Option<crate::correction_consolidation::SharedCorrectionConsolidationStat>,
    /// Phase 178 — last-cycle correction-judgment stat for the
    /// `GetLearningInsights` surface.
    correction_judgment_stat: Option<crate::correction_judgment::SharedCorrectionJudgmentStat>,
    /// Phase 91 (Q4a) — last-reflection-cycle LLM-judged
    /// recall stat for the `GetLearningInsights` surface.
    recall_judgment_stat: Option<crate::recall_judgment::SharedRecallJudgmentStat>,
    /// Phase 93 — `[recall_feedback]` config for the
    /// `GetLearningInsights` surface so the insights view
    /// reflects the same per-hit judgment override that the
    /// reflection-cron actuator is using.
    recall_feedback_config: Option<aivyx_config::RecallFeedbackConfig>,
    /// Phase 95 — per-schedule cadence stats (fired /
    /// skipped counts) the `GetLearningInsights` surface
    /// reads to render the cadence section.
    cadence_stats: crate::reflection_scheduler::SharedRecentReflectionStats,
    /// Phase 102 — registered-tool snapshot for the `GetToolStats`
    /// query. `Arc`-shared so each per-connection context is a
    /// cheap pointer clone.
    tool_descriptors: Arc<[ToolDescriptor]>,
    /// Phase 112 — Skill Auto-Proposer dependency bundle.
    /// `None` disables the post-turn auto-proposer spawn.
    skill_auto_proposer: Option<Arc<crate::skill_auto_proposer::SkillAutoProposerContext>>,
    /// Phase 116 — tool/skill relevance ledger handle.
    /// `None` disables the recording hook + prompt section.
    tool_relevance_ledger: Option<Arc<crate::tool_relevance_ledger::PersistentToolRelevanceLedger>>,
    /// Chapter Whetstone (WH.3b) — per-skill effectiveness ledger handle.
    /// `None` disables the per-turn fold.
    skill_effectiveness_ledger: Option<Arc<crate::skill_effectiveness::SkillEffectivenessLedger>>,
    /// Phase 173 — the autonomous-loop backlog (always `Some`
    /// when storage is configured) for the `loop add/list/status`
    /// IPC handlers.
    loop_backlog: Option<Arc<crate::loop_backlog::PersistentLoopBacklog>>,
    /// Phase 173 — shared loop run state for `loop start/stop/
    /// status`. `Some` only when the `[loop]` section is armed
    /// (the driver was spawned).
    loop_state: Option<crate::loop_driver::SharedLoopState>,
    /// Phase 173 — the `[loop]` config (default priority +
    /// max-iterations ceiling) for the IPC handlers.
    loop_config: Option<aivyx_config::LoopConfig>,
    /// Chapter L (L.5) — the team-mission service for the `TeamRun` /
    /// `TeamMissionList` / `TeamMissionStatus` / `ResolveTeamGate` handlers.
    team_missions: Option<crate::team_mission_driver::TeamMissionService>,
    gate_policy: GatePolicy,
    /// Piece C — per-channel-type authorization for `/team run`.
    channel_trigger_authz: ChannelTriggerAuthz,
    /// Chapter U — path to the loaded `aivyx-pa.toml` for the Settings IPC
    /// write handlers (`SetAccessLevel` / `SetBudget`) + the `GetSettings`
    /// on-disk re-read. `None` ⇒ env-only launch; the write handlers refuse.
    config_toml_path: Option<PathBuf>,
    /// Piece C follow-up — see `DaemonConfig::role_override`'s own doc
    /// comment; threaded here so `handle_query`'s Settings handlers can
    /// resolve against the daemon's own real active role too.
    role_override: Option<String>,
    /// Chapter Roster (RO.2) — the resolved team-config write target for the
    /// `SetTeamRoster` handler. `None` ⇒ env-only launch; the handler refuses.
    team_config_write_path: Option<PathBuf>,
    /// Chapter X — provider + model for the `DraftPersonaSeed` handler.
    seed_draft_llm: Option<SeedDraftLlm>,
    /// Chapter Z — the canonical roots the Documents browser may reach.
    document_roots: DocumentRoots,
    /// Phase 186 — see `DaemonConfig::reminder_store`'s own doc comment.
    reminder_store: Option<crate::reminder_tool::SharedReminderStore>,
    /// Studio Gallery — base URL of the `comfyui` `[[mcp_server]]`'s
    /// backing ComfyUI instance, for the `GetGallery` query handler.
    comfyui_base_url: Option<String>,
}

async fn handle_connection(ctx: ConnectionContext) -> Result<(), DaemonError> {
    let ConnectionContext {
        stream,
        agent,
        channel_factory,
        shutdown,
        mission_store,
        schedule_store,
        notify_targets,
        pending_recovery,
        daemon_state,
        audit_log,
        profile,
        persona_log,
        shared_persona,
        persona_proposal_log,
        memory,
        embedding_provider,
        recall_log,
        helpfulness_ledger,
        cooccurrence_ledger,
        wiki_store,
        graph_store,
        conflict_dismissals,
        correction_ledger,
        persona_selection_stat,
        recall_cluster_stat,
        proactive_stat,
        persona_lifecycle_stat,
        conversation_windows,
        persona_consolidation_stat,
        correction_consolidation_stat,
        correction_judgment_stat,
        recall_judgment_stat,
        recall_feedback_config,
        cadence_stats,
        tool_descriptors,
        skill_auto_proposer,
        tool_relevance_ledger,
        skill_effectiveness_ledger,
        loop_backlog,
        loop_state,
        loop_config,
        team_missions,
        gate_policy,
        channel_trigger_authz,
        config_toml_path,
        role_override,
        team_config_write_path,
        seed_draft_llm,
        document_roots,
        reminder_store,
        comfyui_base_url,
    } = ctx;
    let (mut reader, mut writer) = stream.into_split();

    let ready = DaemonLifecycleEvent::DaemonReady {
        version: PROTOCOL_VERSION.into(),
    };
    let frame = encode_frame(&ready)?;
    writer.write_all(&frame).await?;

    // Deliver recovery notice to the first connecting frontend (take-once).
    let recovery_frame = {
        let stale = pending_recovery.lock().unwrap().take();
        stale.and_then(|s| {
            let notice = DaemonLifecycleEvent::RecoveryNotice {
                lost_sessions: s.sessions.iter().map(|r| r.session_id.clone()).collect(),
                lost_turns: s.in_flight_turns,
                stale_since: s.started_at,
            };
            encode_frame(&notice).ok()
        })
    };
    if let Some(frame) = recovery_frame {
        let _ = writer.write_all(&frame).await;
    }

    let mut buf = Vec::with_capacity(4096);
    let mut session_id: Option<String> = None;
    let mut channel: Option<Arc<dyn ChannelContext + Send + Sync>> = None;

    loop {
        if shutdown.is_cancelled() {
            send_shutting_down(&mut writer, "shutdown requested").await;
            return Ok(());
        }

        let mut tmp = [0u8; 4096];
        let n = tokio::select! {
            result = reader.read(&mut tmp) => {
                result?
            }
            _ = shutdown.cancelled() => {
                send_shutting_down(&mut writer, "shutdown requested").await;
                return Ok(());
            }
        };
        if n == 0 {
            break; // Frontend disconnected.
        }
        buf.extend_from_slice(&tmp[..n]);

        loop {
            match decode_frame::<FrontendMessage>(&buf) {
                Ok((msg, consumed)) => {
                    buf.drain(..consumed);
                    match msg {
                        FrontendMessage::StartSession {
                            role: _,
                            frontend_type,
                        } => {
                            let ft = frontend_type.unwrap_or(FrontendType::Local);
                            channel = Some(channel_factory(ft));

                            let sid = aivyx_core::SessionId::new().to_string();
                            session_id = Some(sid.clone());

                            // Track session in daemon state — /classic
                            // retirement (Sessions screen): channel and
                            // trust tier are free here (the ChannelContext
                            // was just constructed above); created/
                            // last_active start identical.
                            if let Ok(mut st) = daemon_state.lock() {
                                let now_ms = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_millis() as u64)
                                    .unwrap_or(0);
                                let ch = channel.as_ref().expect("just constructed above");
                                st.sessions.push(SessionRecord {
                                    session_id: sid.clone(),
                                    channel: ch.platform(),
                                    trust_tier: ch.trust_tier(),
                                    created_at_ms: now_ms,
                                    last_active_at_ms: now_ms,
                                });
                            }

                            let resp = DaemonMessage::SessionStarted { session_id: sid };
                            let frame = encode_frame(&resp)?;
                            writer.write_all(&frame).await?;
                        }
                        FrontendMessage::SubmitInput {
                            session_id: sid,
                            text,
                            mission_id: mid,
                            attachments,
                            headless,
                        } => {
                            // /classic retirement (Sessions screen) —
                            // record this session as active on every
                            // submitted turn, not just at StartSession.
                            if let Ok(mut st) = daemon_state.lock() {
                                if let Some(rec) =
                                    st.sessions.iter_mut().find(|r| r.session_id == sid)
                                {
                                    rec.last_active_at_ms = std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .map(|d| d.as_millis() as u64)
                                        .unwrap_or(0);
                                }
                            }

                            // Chapter H — a per-turn `headless: true` opts this
                            // turn into RejectAndAbort, overriding the daemon's
                            // default `gate_policy`; otherwise the daemon default
                            // applies (Interactive unless the daemon is headless).
                            let effective_policy = if headless {
                                GatePolicy::RejectAndAbort
                            } else {
                                gate_policy
                            };
                            let ch = match &channel {
                                Some(c) => Arc::clone(c),
                                None => {
                                    let err = DaemonMessage::Error {
                                        code: "no_session".into(),
                                        message: "SubmitInput before StartSession".into(),
                                    };
                                    let frame = encode_frame(&err).unwrap_or_default();
                                    let _ = writer.write_all(&frame).await;
                                    continue;
                                }
                            };

                            // Phase 45 — construct the right message type
                            // based on whether attachments are present.
                            // Phase 86 — the message's `session_id` must
                            // be stable across turns of the same daemon
                            // session (sid was generated by
                            // `SessionId::new().to_string()` at
                            // StartSession). Parse it back so the
                            // per-session conversation window + the
                            // Phase 77 recall correlation key by the
                            // session the operator actually has, not a
                            // fresh-per-turn surrogate.
                            let session = sid
                                .parse::<uuid::Uuid>()
                                .map(aivyx_core::SessionId)
                                .unwrap_or_else(|_| aivyx_core::SessionId::new());
                            // Phase 86 — keep the user text for the
                            // conversation-window write site below
                            // (Message::text consumes it).
                            let user_text = text.clone();
                            let msg = if let Some(att) = attachments.first() {
                                use base64::Engine;
                                let decoder = base64::engine::general_purpose::STANDARD;
                                match decoder.decode(&att.data_base64) {
                                    Ok(data) if text.is_empty() => {
                                        Message::image(session, &att.media_type, data)
                                    }
                                    Ok(data) => Message::text_with_image(
                                        session,
                                        &text,
                                        &att.media_type,
                                        data,
                                    ),
                                    Err(_) => {
                                        // Bad base64 — fall back to text-only.
                                        Message::text(session, text)
                                    }
                                }
                            } else {
                                Message::text(session, text)
                            };

                            // Track in-flight turn in daemon state.
                            let turn_key = format!("{sid}:turn");
                            if let Ok(mut st) = daemon_state.lock() {
                                st.in_flight_turns.push(turn_key.clone());
                            }

                            let bridge = IpcChannelBridge {
                                inner: ch,
                                writer: Arc::new(tokio::sync::Mutex::new(writer)),
                                session_id: sid.clone(),
                            };

                            // Audit H1 fix — rotate the channel stub's
                            // cancellation token so a previous turn's
                            // timeout or `/cancel` does not pre-cancel
                            // this turn. `CancellationToken` is
                            // monotonic; without this reset, the first
                            // timeout/cancel in a daemon session would
                            // brick every subsequent turn until the
                            // operator restarted the connection.
                            bridge.reset_cancellation();

                            // Phase 116 — capture the audit chain's
                            // pre-turn length so the post-finalize hook
                            // can read the turn's per-tool-call entries
                            // (via `entries_range(pre_len, len -
                            // pre_len)`) without locking.
                            let audit_pre_turn_len = audit_log.as_ref().map(|l| l.len());

                            let outcome = agent.turn(msg, &bridge).await;

                            // Turn completed — remove from in-flight.
                            if let Ok(mut st) = daemon_state.lock() {
                                st.in_flight_turns.retain(|t| t != &turn_key);
                            }

                            // Phase 86 — record the completed turn
                            // (user input → assistant final) into the
                            // per-session conversation window so the
                            // next turn's recall + Persona selection
                            // can embed a multi-turn relevance query.
                            // Only conversational completions are
                            // recorded; trigger-fired synthetic turns
                            // (cron / webhook / file-watch) write
                            // through their own paths and are
                            // intentionally excluded. Best-effort —
                            // a missed write costs one cycle of
                            // signal, never the turn.
                            if let (Some(windows), TurnOutcome::Completed { final_message, .. }) =
                                (&conversation_windows, &outcome)
                            {
                                crate::conversation_window::record_turn(
                                    windows,
                                    session,
                                    &user_text,
                                    final_message,
                                );
                            }

                            // Phase 116 — tool-relevance ledger post-
                            // finalize hook. Fires for every turn (any
                            // TurnOutcome variant) when the ledger is
                            // configured; walks the audit chain from
                            // the pre-turn snapshot to the current head
                            // and records each ToolCall's outcome
                            // against the user input's keyword key.
                            // Detached `tokio::spawn` so it never
                            // blocks the next turn. Failure-isolated.
                            if let (Some(ledger), Some(pre_len), Some(audit)) =
                                (&tool_relevance_ledger, audit_pre_turn_len, &audit_log)
                            {
                                let keyword_key = aivyx_core::relevance::keyword_key(&user_text, 5);
                                if !keyword_key.is_empty() {
                                    let ledger_clone = Arc::clone(ledger);
                                    let audit_clone = Arc::clone(audit);
                                    tokio::spawn(async move {
                                        let head = audit_clone.len();
                                        let limit = head.saturating_sub(pre_len);
                                        if limit == 0 {
                                            return;
                                        }
                                        let entries = match audit_clone
                                            .entries_range(pre_len as u64, limit)
                                        {
                                            Ok(e) => e,
                                            Err(e) => {
                                                eprintln!(
                                                    "aivyx-pa tool-relevance: \
                                                     audit walk failed ({e})"
                                                );
                                                return;
                                            }
                                        };
                                        let now_ms = std::time::SystemTime::now()
                                            .duration_since(std::time::UNIX_EPOCH)
                                            .map(|d| d.as_millis() as u64)
                                            .unwrap_or(0);
                                        crate::tool_relevance_ledger::record_turn_outcomes(
                                            &ledger_clone,
                                            &keyword_key,
                                            &entries,
                                            now_ms,
                                        )
                                        .await;
                                    });
                                }
                            }

                            // Chapter Whetstone (WH.3b) — per-skill
                            // effectiveness fold. Same detached, failure-
                            // isolated shape: walk this turn's audit slice
                            // for SkillInvocation entries and fold each
                            // distinct skill by the turn's grade. Chapter
                            // Strop (ST.1): helpful = Completed WITHOUT a
                            // Candor unfulfilled-claim annotation — a
                            // claimed-but-not-done skill turn now folds
                            // negative instead of drifting every score
                            // positive. Fires only when [skill_refinement]
                            // is configured (the ledger is `Some`).
                            if let (Some(skill_ledger), Some(pre_len), Some(audit)) =
                                (&skill_effectiveness_ledger, audit_pre_turn_len, &audit_log)
                            {
                                let helpful =
                                    crate::skill_effectiveness::turn_folds_helpful(&outcome);
                                let ledger_clone = Arc::clone(skill_ledger);
                                let audit_clone = Arc::clone(audit);
                                tokio::spawn(async move {
                                    let head = audit_clone.len();
                                    let limit = head.saturating_sub(pre_len);
                                    if limit == 0 {
                                        return;
                                    }
                                    let entries =
                                        match audit_clone.entries_range(pre_len as u64, limit) {
                                            Ok(e) => e,
                                            Err(_) => return,
                                        };
                                    let now_secs = std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .map(|d| d.as_secs())
                                        .unwrap_or(0);
                                    crate::skill_effectiveness::record_turn_skills(
                                        &ledger_clone,
                                        &entries,
                                        helpful,
                                        now_secs,
                                    )
                                    .await;
                                });
                            }

                            // Phase 112 + 115 — auto-proposer post-finalize
                            // hook. Q2b inline-at-turn-boundary firing; the
                            // pipeline runs in a detached `tokio::spawn` so
                            // it never blocks the next turn.
                            //
                            // Phase 112-114: fires only on
                            // `TurnOutcome::Completed` (positive-pattern path).
                            // Phase 115: also fires on Failed / Cancelled /
                            // TimedOut / Escalated when the operator has
                            // `from_failed_turns = true` in their TOML and the
                            // specific failure outcome is enabled in
                            // `failure_outcomes`. The negative-feedback
                            // (failure correction) path passes
                            // `ProposalSource::FailedTurn { .. }` to the
                            // pipeline.
                            if let Some(proposer_ctx) = &skill_auto_proposer {
                                use crate::skill_auto_proposer::{
                                    self as sap, FailureKind, ProposalSource,
                                };

                                // Classify the outcome into (signals, source).
                                let dispatch: Option<(sap::TurnSignals, ProposalSource)> =
                                    match &outcome {
                                        TurnOutcome::Completed {
                                            tool_calls_made,
                                            duration,
                                            ..
                                        } => Some((
                                            sap::TurnSignals {
                                                tool_calls_made: *tool_calls_made as u32,
                                                distinct_tool_id_count: (*tool_calls_made as u32)
                                                    .min(4),
                                                duration: *duration,
                                                had_successful_gate_resolve: false,
                                                // Phase 118 — Task 3 ships the
                                                // type substrate; the actual
                                                // ledger/audit-walk sourcing
                                                // for these signals is wired in
                                                // Task 6. Default-zero until
                                                // then.
                                                ..sap::TurnSignals::default()
                                            },
                                            ProposalSource::CompletedTurn,
                                        )),
                                        other => {
                                            // Phase 115 — non-Completed outcomes
                                            // gate on the operator's
                                            // from_failed_turns + failure_outcomes
                                            // config.
                                            let cfg = &proposer_ctx.config;
                                            if !cfg.from_failed_turns {
                                                None
                                            } else {
                                                let kind = match other {
                                                    TurnOutcome::Failed(_)
                                                    | TurnOutcome::MaxStepsExceeded { .. }
                                                    | TurnOutcome::Looping { .. } => {
                                                        FailureKind::Failed
                                                    }
                                                    TurnOutcome::Cancelled { .. } => {
                                                        FailureKind::Cancelled
                                                    }
                                                    TurnOutcome::TimedOut { .. } => {
                                                        FailureKind::TimedOut
                                                    }
                                                    TurnOutcome::Escalated { .. } => {
                                                        FailureKind::Escalated
                                                    }
                                                    TurnOutcome::Completed { .. } => unreachable!(),
                                                };
                                                if !sap::is_failure_candidate(
                                                    kind,
                                                    &cfg.failure_outcomes,
                                                ) {
                                                    None
                                                } else {
                                                    // For failure paths the
                                                    // signals are degenerate; the
                                                    // failure heuristic + judge
                                                    // are the real gates.
                                                    // Phase 118 — degenerate
                                                    // signals for the failure
                                                    // path; the new Profile/Role
                                                    // signals default to zero.
                                                    let signals = sap::TurnSignals::default();
                                                    // Exhaustive on purpose — no
                                                    // `_` arm. `MaxStepsExceeded` /
                                                    // `Looping` reach here too (a
                                                    // local model that runs away
                                                    // and trips the step cap or the
                                                    // cycle breaker), and the prior
                                                    // `_ => unreachable!()` panicked
                                                    // the daemon on exactly that. An
                                                    // exhaustive match makes the
                                                    // compiler force every future
                                                    // outcome to be handled here, so
                                                    // a single turn can never kill a
                                                    // 24/7 daemon.
                                                    let summary = match other {
                                                        TurnOutcome::Failed(e) => {
                                                            format!("planner/agent error: {e}")
                                                        }
                                                        TurnOutcome::MaxStepsExceeded {
                                                            max_steps,
                                                            ..
                                                        } => format!(
                                                            "planner exceeded {max_steps} steps"
                                                        ),
                                                        TurnOutcome::Looping {
                                                            repeat_limit,
                                                            ..
                                                        } => format!(
                                                            "stopped after {repeat_limit} \
                                                         repeated tool calls"
                                                        ),
                                                        TurnOutcome::Cancelled { .. } => {
                                                            "operator cancelled mid-turn".into()
                                                        }
                                                        TurnOutcome::TimedOut {
                                                            elapsed, ..
                                                        } => format!(
                                                            "exceeded turn budget after {}ms",
                                                            elapsed.as_millis()
                                                        ),
                                                        TurnOutcome::Escalated {
                                                            reason, ..
                                                        } => format!("agent escalated: {reason}"),
                                                        // Peeled off by the outer
                                                        // match; a benign string
                                                        // rather than a panic keeps
                                                        // the daemon alive if the
                                                        // invariant ever shifts.
                                                        TurnOutcome::Completed { .. } => {
                                                            "turn completed".into()
                                                        }
                                                    };
                                                    Some((
                                                        signals,
                                                        ProposalSource::FailedTurn {
                                                            kind,
                                                            summary,
                                                        },
                                                    ))
                                                }
                                            }
                                        }
                                    };

                                if let Some((signals, source)) = dispatch {
                                    let summary = sap::build_turn_summary(&user_text, &outcome);
                                    let proposer_ctx = Arc::clone(proposer_ctx);
                                    let audit_clone = audit_log.clone();
                                    let persona_clone = persona_log.clone();
                                    let proposal_clone = persona_proposal_log.clone();
                                    let shared_clone = shared_persona.clone();
                                    let cancel = shutdown.clone();
                                    tokio::spawn(async move {
                                        sap::run_auto_propose_pipeline_with_source(
                                            &proposer_ctx,
                                            audit_clone.as_ref(),
                                            persona_clone.as_ref(),
                                            proposal_clone.as_ref(),
                                            &shared_clone,
                                            session,
                                            signals,
                                            summary,
                                            source,
                                            &cancel,
                                        )
                                        .await;
                                    });
                                }
                            }

                            writer = Arc::try_unwrap(bridge.writer)
                                .map_err(|_| {
                                    DaemonError::Internal("writer arc still shared".into())
                                })?
                                .into_inner();

                            // Chapter H — only an *interactive* run parks an
                            // escalated turn behind an operator gate. A headless
                            // run (RejectAndAbort) never waits: it skips the gate
                            // + ApprovalGate, and the turn finalizes `Escalated`
                            // (the refusal, recorded on the audit chain).
                            if escalation_parks(effective_policy)
                                && let (
                                    TurnOutcome::Escalated { reason, .. },
                                    Some(mission_id),
                                    Some(store),
                                ) = (&outcome, &mid, &mission_store)
                            {
                                let gate_result = async {
                                    let mut record = mission::get_mission(store, mission_id)
                                        .await
                                        .map_err(|e| format!("get mission: {e}"))?
                                        .ok_or_else(|| format!("mission {mission_id} not found"))?;
                                    let gate_id =
                                        format!("gate-{}", uuid::Uuid::new_v4().as_hyphenated());
                                    mission::add_gate(
                                        &mut record,
                                        gate_id.clone(),
                                        reason.clone(),
                                        None,
                                    )
                                    .map_err(|e| e.to_string())?;
                                    mission::update_mission(store, &record)
                                        .await
                                        .map_err(|e| format!("persist mission: {e}"))?;
                                    Ok::<String, String>(gate_id)
                                }
                                .await;

                                match gate_result {
                                    Ok(gate_id) => {
                                        let gate_event = DaemonMessage::StreamEvent {
                                            session_id: sid.clone(),
                                            event: StreamEventPayload::ApprovalGate {
                                                mission_id: mission_id.clone(),
                                                gate_id,
                                                reason: reason.clone(),
                                                scope: None,
                                            },
                                        };
                                        let frame = encode_frame(&gate_event)?;
                                        writer.write_all(&frame).await?;
                                    }
                                    Err(e) => {
                                        let err = DaemonMessage::Error {
                                            code: "gate_create_failed".into(),
                                            message: format!("failed to create gate: {e}"),
                                        };
                                        let frame = encode_frame(&err).unwrap_or_default();
                                        let _ = writer.write_all(&frame).await;
                                    }
                                }
                            }

                            // H.6 — a headless turn that escalated never parked
                            // behind a gate (the block above is skipped when
                            // !escalation_parks); record the refusal on the
                            // audit chain so the unattended path stays as
                            // legible as an operator-resolved gate (Phase 78
                            // autonomous-action-must-stay-legible posture). The
                            // turn still finalizes `Escalated` in `outcome_str`.
                            if !escalation_parks(effective_policy)
                                && let TurnOutcome::Escalated { reason, .. } = &outcome
                            {
                                eprintln!(
                                    "aivyx-pa daemon: escalation refused (headless) on session {sid}: {reason}",
                                );
                                if let Some(al) = &audit_log {
                                    let event = aivyx_audit::AuditEvent::HeadlessRefusal {
                                        run_id: sid.clone(),
                                        surface: aivyx_audit::HeadlessSurfaceSummary::AgentTurn,
                                        reason: reason.clone(),
                                    };
                                    if let Err(e) = al.append(event) {
                                        eprintln!(
                                            "aivyx-pa daemon: failed to audit headless refusal: {e}",
                                        );
                                    }
                                }
                            }

                            let outcome_str = format_outcome(&outcome);

                            let resp = DaemonMessage::TurnComplete {
                                session_id: sid,
                                outcome: outcome_str,
                            };
                            let frame = encode_frame(&resp)?;
                            writer.write_all(&frame).await?;
                        }
                        FrontendMessage::Disconnect => {
                            return Ok(());
                        }
                        FrontendMessage::CancelTurn { session_id: _sid } => {
                            // Audit C1 fix — fire the channel stub's in-flight
                            // cancellation token. Before this fix, the handler
                            // was a no-op (the comment claimed the wiring was
                            // present, but no code actually called `.cancel()`
                            // on anything). The four daemon-side stubs
                            // (Telegram/Discord/Slack/Web) override
                            // `cancel_inflight` to fire their internal token;
                            // any non-daemon channel uses the default no-op.
                            // The turn loop's mid-LLM-step cancellation check
                            // then translates the cancel into
                            // `TurnOutcome::Cancelled`.
                            if let Some(ch) = &channel {
                                ch.cancel_inflight();
                            }
                        }
                        FrontendMessage::ResolveGate {
                            mission_id,
                            gate_id,
                            approved,
                        } => {
                            let Some(store) = &mission_store else {
                                let err = DaemonMessage::Error {
                                    code: "no_mission_store".into(),
                                    message: "ResolveGate received but no mission store configured"
                                        .into(),
                                };
                                let frame = encode_frame(&err).unwrap_or_default();
                                let _ = writer.write_all(&frame).await;
                                continue;
                            };
                            let result = async {
                                let mut record = mission::get_mission(store, &mission_id)
                                    .await
                                    .map_err(|e| format!("get mission: {e}"))?
                                    .ok_or_else(|| format!("mission {mission_id} not found"))?;
                                mission::resolve_gate(&mut record, &gate_id, approved)
                                    .map_err(|e| e.to_string())?;
                                mission::update_mission(store, &record)
                                    .await
                                    .map_err(|e| format!("persist mission: {e}"))?;
                                Ok::<(), String>(())
                            }
                            .await;
                            match result {
                                Ok(()) => {
                                    let resp = DaemonMessage::GateResolved {
                                        mission_id: mission_id.clone(),
                                        gate_id: gate_id.clone(),
                                        approved,
                                    };
                                    let frame = encode_frame(&resp)?;
                                    writer.write_all(&frame).await?;

                                    if approved {
                                        if let Some(ch) = &channel {
                                            let ch = Arc::clone(ch);
                                            let resume_text = format!(
                                                "Gate {gate_id} approved — continue mission {mission_id}"
                                            );
                                            let msg = Message::text(
                                                aivyx_core::SessionId::new(),
                                                resume_text,
                                            );
                                            let sid = session_id.clone().unwrap_or_default();
                                            let bridge = IpcChannelBridge {
                                                inner: ch,
                                                writer: Arc::new(tokio::sync::Mutex::new(writer)),
                                                session_id: sid.clone(),
                                            };

                                            // Audit H1 fix — rotate before
                                            // resuming so a prior cancel does
                                            // not pre-cancel the resume turn.
                                            bridge.reset_cancellation();

                                            let resume_outcome = agent.turn(msg, &bridge).await;

                                            writer = Arc::try_unwrap(bridge.writer)
                                                .map_err(|_| {
                                                    DaemonError::Internal(
                                                        "writer arc still shared".into(),
                                                    )
                                                })?
                                                .into_inner();

                                            let outcome_str = format_outcome(&resume_outcome);
                                            let resp = DaemonMessage::TurnComplete {
                                                session_id: sid,
                                                outcome: outcome_str,
                                            };
                                            let frame = encode_frame(&resp)?;
                                            writer.write_all(&frame).await?;
                                        }
                                    }
                                }
                                Err(e) => {
                                    let err = DaemonMessage::Error {
                                        code: "gate_resolve_failed".into(),
                                        message: format!("failed to resolve gate: {e}"),
                                    };
                                    let frame = encode_frame(&err).unwrap_or_default();
                                    let _ = writer.write_all(&frame).await;
                                }
                            }
                        }
                        FrontendMessage::RunTeamMissionChannel { goal } => {
                            let platform = channel.as_ref().map(|c| c.platform());
                            let resp = handle_run_team_mission_channel(
                                team_missions.as_ref(),
                                &channel_trigger_authz,
                                platform,
                                audit_log.as_deref(),
                                goal,
                            )
                            .await;
                            let frame = encode_frame(&resp)?;
                            writer.write_all(&frame).await?;
                        }
                        FrontendMessage::Shutdown => {
                            send_shutting_down(&mut writer, "operator requested via daemon stop")
                                .await;
                            shutdown.cancel();
                            return Ok(());
                        }
                        FrontendMessage::ProtocolNegotiation { version } => {
                            // v0.1: always accept. Future versions can
                            // check compatibility and respond with
                            // ProtocolRejected if needed.
                            let resp = if version == PROTOCOL_VERSION {
                                DaemonMessage::ProtocolAccepted { version }
                            } else {
                                // For v0.1, accept any version the client
                                // sends — forward compatibility. When v0.2
                                // ships, this branch can reject unknown
                                // versions.
                                DaemonMessage::ProtocolAccepted { version }
                            };
                            let frame = encode_frame(&resp)?;
                            writer.write_all(&frame).await?;
                        }
                        FrontendMessage::Query { id, payload } => {
                            // Phase 47 — inspection queries. Read-only; no
                            // capability check (IPC socket auth is the
                            // authorization boundary, per Q2).
                            let response_payload = handle_query(
                                payload,
                                &daemon_state,
                                mission_store.as_deref(),
                                schedule_store.as_deref(),
                                notify_targets.as_slice(),
                                audit_log.as_deref(),
                                &profile,
                                persona_log.as_deref(),
                                &shared_persona,
                                persona_proposal_log.as_deref(),
                                memory.as_ref(),
                                embedding_provider.as_ref(),
                                recall_log.as_ref(),
                                helpfulness_ledger.as_ref(),
                                cooccurrence_ledger.as_ref(),
                                wiki_store.as_ref(),
                                graph_store.as_ref(),
                                conflict_dismissals.as_ref(),
                                skill_effectiveness_ledger.as_ref(),
                                correction_ledger.as_ref(),
                                persona_selection_stat.as_ref(),
                                recall_cluster_stat.as_ref(),
                                proactive_stat.as_ref(),
                                persona_lifecycle_stat.as_ref(),
                                persona_consolidation_stat.as_ref(),
                                correction_consolidation_stat.as_ref(),
                                correction_judgment_stat.as_ref(),
                                recall_judgment_stat.as_ref(),
                                recall_feedback_config.as_ref(),
                                &cadence_stats,
                                &tool_descriptors,
                                tool_relevance_ledger.as_ref(),
                                loop_backlog.as_ref(),
                                loop_state.as_ref(),
                                loop_config.as_ref(),
                                team_missions.as_ref(),
                                config_toml_path.as_deref(),
                                role_override.as_deref(),
                                team_config_write_path.as_deref(),
                                &document_roots,
                                seed_draft_llm.as_ref(),
                                comfyui_base_url.as_deref(),
                                reminder_store.as_ref(),
                            )
                            .await;
                            let resp = DaemonMessage::QueryResponse {
                                id,
                                payload: response_payload,
                            };
                            let frame = encode_frame(&resp)?;
                            writer.write_all(&frame).await?;
                        }
                        FrontendMessage::RevertPersonaDelta {
                            id,
                            target_delta_id,
                        } => {
                            // Phase 60 — operator-initiated revert
                            // (P14 commit 4). Append a `Revert` op
                            // delta to the persona chain; on
                            // success, recompute the shared state
                            // so the next turn picks it up. Per
                            // Q5(a) at Phase 60 sign-off: no gate
                            // prompt — the operator is the
                            // proposer.
                            let resp = match resolve_persona_revert(
                                persona_log.as_deref(),
                                &shared_persona,
                                &target_delta_id,
                            )
                            .await
                            {
                                Ok(seq) => DaemonMessage::PersonaRevertResolved {
                                    id,
                                    ok: true,
                                    seq: Some(seq),
                                    error: None,
                                },
                                Err(reason) => DaemonMessage::PersonaRevertResolved {
                                    id,
                                    ok: false,
                                    seq: None,
                                    error: Some(reason),
                                },
                            };
                            let frame = encode_frame(&resp)?;
                            writer.write_all(&frame).await?;
                        }
                        FrontendMessage::ResolveSoulConflict {
                            id,
                            category,
                            value,
                        } => {
                            // Chapter Accord — operator removes the losing
                            // facet of a detected contradiction. Appends a
                            // `RemoveList` persona delta (operator-authored,
                            // revertible) + recomputes shared state. No gate:
                            // the operator is the proposer (like RevertPersonaDelta).
                            let resp = match resolve_soul_conflict(
                                persona_log.as_deref(),
                                &shared_persona,
                                &category,
                                &value,
                            )
                            .await
                            {
                                Ok(seq) => DaemonMessage::SoulConflictResolved {
                                    id,
                                    ok: true,
                                    seq: Some(seq),
                                    error: None,
                                },
                                Err(reason) => DaemonMessage::SoulConflictResolved {
                                    id,
                                    ok: false,
                                    seq: None,
                                    error: Some(reason),
                                },
                            };
                            let frame = encode_frame(&resp)?;
                            writer.write_all(&frame).await?;
                        }
                        FrontendMessage::DismissSoulConflict { id, conflict_id } => {
                            // Chapter Accord — "keep both": record the Soul-
                            // conflict id so future detection passes suppress
                            // this pair. Nothing is removed from the Soul.
                            let now = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_secs())
                                .unwrap_or(0);
                            let resp = match conflict_dismissals.as_ref() {
                                None => DaemonMessage::SoulConflictDismissed {
                                    id,
                                    ok: false,
                                    error: Some(
                                        "daemon has no storage configured for \
                                         conflict dismissals"
                                            .into(),
                                    ),
                                },
                                Some(store) => match store.dismiss_soul(&conflict_id, now).await {
                                    Ok(()) => DaemonMessage::SoulConflictDismissed {
                                        id,
                                        ok: true,
                                        error: None,
                                    },
                                    Err(e) => DaemonMessage::SoulConflictDismissed {
                                        id,
                                        ok: false,
                                        error: Some(e.to_string()),
                                    },
                                },
                            };
                            let frame = encode_frame(&resp)?;
                            writer.write_all(&frame).await?;
                        }
                        FrontendMessage::SeedPersona { id, seed } => {
                            // Chapter X — live persona seed (web onboarding).
                            // Plants the seed iff the chain is empty, via the
                            // same signed + audited primitive the boot-seed
                            // uses, then recomputes shared state for next-turn
                            // adoption.
                            let resp = match seed_persona_live(
                                persona_log.as_deref(),
                                &shared_persona,
                                audit_log.as_deref(),
                                seed,
                            )
                            .await
                            {
                                Ok(appended) => DaemonMessage::PersonaSeedResolved {
                                    id,
                                    ok: true,
                                    appended,
                                    error: None,
                                },
                                Err(reason) => DaemonMessage::PersonaSeedResolved {
                                    id,
                                    ok: false,
                                    appended: 0,
                                    error: Some(reason),
                                },
                            };
                            let frame = encode_frame(&resp)?;
                            writer.write_all(&frame).await?;
                        }
                        FrontendMessage::AuthorSkill {
                            id,
                            op,
                            name,
                            trigger,
                            procedure,
                        } => {
                            // Chapter Tutor — operator-initiated skill authoring
                            // on a grown chain: signed + audited append via the
                            // same path the agent skill tools use, but driven by
                            // operator authority (no agent scope), then
                            // recomputed for next-turn adoption.
                            let resp = match author_skill_live(
                                persona_log.as_ref(),
                                &shared_persona,
                                op,
                                &name,
                                trigger.as_deref(),
                                procedure.as_deref(),
                            )
                            .await
                            {
                                Ok(seq) => DaemonMessage::SkillAuthored {
                                    id,
                                    ok: true,
                                    seq: Some(seq),
                                    error: None,
                                },
                                Err(reason) => DaemonMessage::SkillAuthored {
                                    id,
                                    ok: false,
                                    seq: None,
                                    error: Some(reason),
                                },
                            };
                            let frame = encode_frame(&resp)?;
                            writer.write_all(&frame).await?;
                        }
                        FrontendMessage::DraftPersonaSeed { id, description } => {
                            // Chapter X — one-shot LLM draft of a persona seed
                            // from the operator's description. Read-only (drafts
                            // nothing onto the chain); the operator edits +
                            // confirms via SeedPersona. No model ⇒ typed error.
                            let resp = match seed_draft_llm.as_ref() {
                                Some(llm) => {
                                    match crate::persona_seed_draft::draft_persona_seed(
                                        &llm.provider,
                                        &llm.model,
                                        &description,
                                    )
                                    .await
                                    {
                                        Some(seed) => DaemonMessage::PersonaSeedDrafted {
                                            id,
                                            draft: Some(persona_seed_to_wire(seed)),
                                            error: None,
                                        },
                                        None => DaemonMessage::PersonaSeedDrafted {
                                            id,
                                            draft: None,
                                            error: Some(
                                                "the model couldn't draft a seed — \
                                                 fill it in manually instead"
                                                    .to_string(),
                                            ),
                                        },
                                    }
                                }
                                None => DaemonMessage::PersonaSeedDrafted {
                                    id,
                                    draft: None,
                                    error: Some("no model is configured for drafting".to_string()),
                                },
                            };
                            let frame = encode_frame(&resp)?;
                            writer.write_all(&frame).await?;
                        }
                        FrontendMessage::DraftTeamTemplate { id, description } => {
                            // Chapter Nonagon Templates — one-shot LLM draft
                            // of a role-tailored 9-member Nonagon from the
                            // operator's declared Profile. Read-only: the
                            // draft lands in the Studio's existing roster
                            // draft state; SetTeamRoster is what persists.
                            let resp = match seed_draft_llm.as_ref() {
                                Some(llm) => match crate::team_template_draft::draft_team_template(
                                    &llm.provider,
                                    &llm.model,
                                    profile.operator_profile.as_deref(),
                                    &profile.primary_use_cases,
                                    &description,
                                )
                                .await
                                {
                                    Some(draft) => DaemonMessage::TeamTemplateDrafted {
                                        id,
                                        draft: Some(draft),
                                        error: None,
                                    },
                                    None => DaemonMessage::TeamTemplateDrafted {
                                        id,
                                        draft: None,
                                        error: Some(
                                            "the model couldn't draft a team — \
                                                 try the default Nonagon or edit \
                                                 manually instead"
                                                .to_string(),
                                        ),
                                    },
                                },
                                None => DaemonMessage::TeamTemplateDrafted {
                                    id,
                                    draft: None,
                                    error: Some("no model is configured for drafting".to_string()),
                                },
                            };
                            let frame = encode_frame(&resp)?;
                            writer.write_all(&frame).await?;
                        }
                        FrontendMessage::DraftProfile {
                            id,
                            intent,
                            role,
                            tone,
                            never_do,
                        } => {
                            // Chapter Genesis — one-shot LLM draft of the
                            // declared P13 Profile from the operator's
                            // onboarding answers. Read-only (writes nothing);
                            // the operator edits + persists via SetProfile.
                            // Reuses the same provider/model as the persona
                            // seed drafter. No model ⇒ typed error.
                            let answers = crate::profile_draft::IdentityAnswers {
                                intent,
                                role,
                                tone,
                                never_do,
                            };
                            let resp = match seed_draft_llm.as_ref() {
                                Some(llm) => {
                                    match crate::profile_draft::draft_identity(
                                        &llm.provider,
                                        &llm.model,
                                        &answers,
                                    )
                                    .await
                                    {
                                        Some(profile) => DaemonMessage::ProfileDrafted {
                                            id,
                                            draft: Some(drafted_profile_to_wire(profile)),
                                            error: None,
                                        },
                                        None => DaemonMessage::ProfileDrafted {
                                            id,
                                            draft: None,
                                            error: Some(
                                                "the model couldn't draft a profile — \
                                                 fill it in manually instead"
                                                    .to_string(),
                                            ),
                                        },
                                    }
                                }
                                None => DaemonMessage::ProfileDrafted {
                                    id,
                                    draft: None,
                                    error: Some("no model is configured for drafting".to_string()),
                                },
                            };
                            let frame = encode_frame(&resp)?;
                            writer.write_all(&frame).await?;
                        }
                        FrontendMessage::ResolvePersonaProposal {
                            id,
                            proposal_id,
                            resolution,
                        } => {
                            // Phase 70 — operator-initiated proposal
                            // resolution. Approve / ApproveWithEdit
                            // apply a PersonaDelta to the persona log
                            // first, then record the Approved entry on
                            // the proposal chain bound to the delta's
                            // seq. Reject just records the Rejected
                            // entry. The shared persona snapshot is
                            // recomputed on approve so the next turn
                            // sees the new state.
                            //
                            // Chapter Accord (prevent-at-write) — refuse to
                            // approve a facet that would contradict the Soul,
                            // unless the operator dismissed that pair. Keeps the
                            // Soul from ever accreting a contradiction.
                            if let Some(block) = persona_approve_coherence_block(
                                persona_proposal_log.as_deref(),
                                &shared_persona,
                                seed_draft_llm.as_ref(),
                                conflict_dismissals.as_deref(),
                                &proposal_id,
                                &resolution,
                            )
                            .await
                            {
                                let resp = DaemonMessage::PersonaProposalResolved {
                                    id,
                                    ok: false,
                                    success: None,
                                    error: Some(block),
                                };
                                writer.write_all(&encode_frame(&resp)?).await?;
                                continue;
                            }
                            let resp = match resolve_persona_proposal(
                                persona_proposal_log.as_deref(),
                                persona_log.as_deref(),
                                &shared_persona,
                                &id,
                                proposal_id,
                                resolution,
                            )
                            .await
                            {
                                Ok(success) => DaemonMessage::PersonaProposalResolved {
                                    id,
                                    ok: true,
                                    success: Some(success),
                                    error: None,
                                },
                                Err(reason) => DaemonMessage::PersonaProposalResolved {
                                    id,
                                    ok: false,
                                    success: None,
                                    error: Some(reason),
                                },
                            };
                            let frame = encode_frame(&resp)?;
                            writer.write_all(&frame).await?;
                        }
                        FrontendMessage::EvictMemoryTopic { id, topic } => {
                            // Phase 74 — operator-initiated memory
                            // eviction. `Memory::forget` deletes every
                            // entry under the topic and returns the
                            // count.
                            let resp = match memory.as_ref() {
                                None => DaemonMessage::MemoryEvictResolved {
                                    id,
                                    ok: false,
                                    deleted: None,
                                    error: Some(
                                        "daemon has no memory substrate \
                                         configured"
                                            .into(),
                                    ),
                                },
                                Some(mem) => match mem.forget(&topic).await {
                                    Ok(n) => DaemonMessage::MemoryEvictResolved {
                                        id,
                                        ok: true,
                                        deleted: Some(n as u64),
                                        error: None,
                                    },
                                    Err(e) => DaemonMessage::MemoryEvictResolved {
                                        id,
                                        ok: false,
                                        deleted: None,
                                        error: Some(e.to_string()),
                                    },
                                },
                            };
                            let frame = encode_frame(&resp)?;
                            writer.write_all(&frame).await?;
                        }
                        FrontendMessage::ResolveMemoryConflict {
                            id,
                            topic,
                            archive_seq,
                        } => {
                            // Chapter Concord — the operator picked which of
                            // two conflicting facts is true; delete the other
                            // (`archive_seq`) from active memory. Mirrors the
                            // operator-initiated EvictMemoryTopic shape, but
                            // removes one caller-named entry, not the topic.
                            let resp = match memory.as_ref() {
                                None => DaemonMessage::MemoryConflictResolved {
                                    id,
                                    ok: false,
                                    removed: false,
                                    error: Some(
                                        "daemon has no memory substrate \
                                         configured"
                                            .into(),
                                    ),
                                },
                                Some(mem) => match mem.delete_entry(&topic, archive_seq).await {
                                    Ok(removed) => DaemonMessage::MemoryConflictResolved {
                                        id,
                                        ok: true,
                                        removed,
                                        error: None,
                                    },
                                    Err(e) => DaemonMessage::MemoryConflictResolved {
                                        id,
                                        ok: false,
                                        removed: false,
                                        error: Some(e.to_string()),
                                    },
                                },
                            };
                            let frame = encode_frame(&resp)?;
                            writer.write_all(&frame).await?;
                        }
                        FrontendMessage::DismissMemoryConflict { id, conflict_id } => {
                            // Chapter Concord — "keep both": record the
                            // conflict id so future detection passes suppress
                            // this pair. Nothing is deleted.
                            let now = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_secs())
                                .unwrap_or(0);
                            let resp = match conflict_dismissals.as_ref() {
                                None => DaemonMessage::MemoryConflictDismissed {
                                    id,
                                    ok: false,
                                    error: Some(
                                        "daemon has no storage configured for \
                                         conflict dismissals"
                                            .into(),
                                    ),
                                },
                                Some(store) => match store.dismiss(&conflict_id, now).await {
                                    Ok(()) => DaemonMessage::MemoryConflictDismissed {
                                        id,
                                        ok: true,
                                        error: None,
                                    },
                                    Err(e) => DaemonMessage::MemoryConflictDismissed {
                                        id,
                                        ok: false,
                                        error: Some(e.to_string()),
                                    },
                                },
                            };
                            let frame = encode_frame(&resp)?;
                            writer.write_all(&frame).await?;
                        }
                        FrontendMessage::ForgetSkill { id, name } => {
                            // Chapter Repertoire — operator forgets a learned
                            // skill from the Skills screen (appends a
                            // RemoveList persona delta, operator-authoritative).
                            let resp = match persona_log.as_deref() {
                                None => DaemonMessage::SkillForgotten {
                                    id,
                                    ok: false,
                                    removed: false,
                                    name: name.clone(),
                                    error: Some("daemon has no persona log configured".into()),
                                },
                                Some(log) => match crate::skill_edit::operator_forget_skill(
                                    log,
                                    &shared_persona,
                                    &name,
                                )
                                .await
                                {
                                    Ok(removed) => DaemonMessage::SkillForgotten {
                                        id,
                                        ok: true,
                                        removed,
                                        name: name.clone(),
                                        error: None,
                                    },
                                    Err(e) => DaemonMessage::SkillForgotten {
                                        id,
                                        ok: false,
                                        removed: false,
                                        name: name.clone(),
                                        error: Some(e),
                                    },
                                },
                            };
                            let frame = encode_frame(&resp)?;
                            writer.write_all(&frame).await?;
                        }
                        FrontendMessage::CreateSchedule {
                            id,
                            name,
                            cron,
                            prompt,
                            enabled,
                        } => {
                            // Chapter Chime — operator creates a schedule from
                            // the Studio; the running scheduler arms it within
                            // one tick, no restart.
                            let resp = match schedule_store.as_deref() {
                                None => schedule_mutated(
                                    id,
                                    &name,
                                    Err("daemon has no schedule store configured".into()),
                                ),
                                Some(store) => {
                                    let res = crate::schedule::operator_create_schedule(
                                        store, &name, &cron, &prompt, enabled,
                                    )
                                    .await
                                    .map(|_| ());
                                    if res.is_ok() {
                                        audit_schedule_mutation(
                                            audit_log.as_deref(),
                                            "create",
                                            &name,
                                            "operator",
                                        );
                                    }
                                    schedule_mutated(id, &name, res)
                                }
                            };
                            let frame = encode_frame(&resp)?;
                            writer.write_all(&frame).await?;
                        }
                        FrontendMessage::UpdateSchedule {
                            id,
                            schedule_id,
                            enabled,
                            cron,
                            prompt,
                        } => {
                            let resp = match schedule_store.as_deref() {
                                None => schedule_mutated(
                                    id,
                                    &schedule_id,
                                    Err("daemon has no schedule store configured".into()),
                                ),
                                Some(store) => {
                                    let res = crate::schedule::operator_update_schedule(
                                        store,
                                        &schedule_id,
                                        enabled,
                                        cron,
                                        prompt,
                                    )
                                    .await;
                                    if res.is_ok() {
                                        audit_schedule_mutation(
                                            audit_log.as_deref(),
                                            "update",
                                            &schedule_id,
                                            "operator",
                                        );
                                    }
                                    schedule_mutated(id, &schedule_id, res)
                                }
                            };
                            let frame = encode_frame(&resp)?;
                            writer.write_all(&frame).await?;
                        }
                        FrontendMessage::DeleteSchedule { id, schedule_id } => {
                            let resp = match schedule_store.as_deref() {
                                None => schedule_mutated(
                                    id,
                                    &schedule_id,
                                    Err("daemon has no schedule store configured".into()),
                                ),
                                Some(store) => {
                                    let res = crate::schedule::operator_delete_schedule(
                                        store,
                                        &schedule_id,
                                    )
                                    .await;
                                    if res.is_ok() {
                                        audit_schedule_mutation(
                                            audit_log.as_deref(),
                                            "delete",
                                            &schedule_id,
                                            "operator",
                                        );
                                    }
                                    schedule_mutated(id, &schedule_id, res)
                                }
                            };
                            let frame = encode_frame(&resp)?;
                            writer.write_all(&frame).await?;
                        }
                        FrontendMessage::ApplyProfileHint {
                            id,
                            proposal_id,
                            field,
                            applied_value,
                        } => {
                            // Phase 119 — operator's act-on-approval
                            // gesture for a ProfileHint. The CLI has
                            // already mutated aivyx-pa.toml via the Task 3
                            // atomic primitive; this handler's only
                            // job is to record the audit event so
                            // forensic walks can pair the apply with
                            // the upstream proposal.
                            let resp = match audit_log.as_ref() {
                                None => DaemonMessage::ProfileHintApplyAcked {
                                    id,
                                    ok: false,
                                    error: Some("daemon has no audit log configured".into()),
                                },
                                Some(al) => {
                                    let event = aivyx_audit::AuditEvent::ProfileHintApplied {
                                        session_id: aivyx_core::SessionId::new(),
                                        proposal_id,
                                        field,
                                        applied_value,
                                    };
                                    match al.append(event) {
                                        Ok(_) => DaemonMessage::ProfileHintApplyAcked {
                                            id,
                                            ok: true,
                                            error: None,
                                        },
                                        Err(e) => DaemonMessage::ProfileHintApplyAcked {
                                            id,
                                            ok: false,
                                            error: Some(e.to_string()),
                                        },
                                    }
                                }
                            };
                            let frame = encode_frame(&resp)?;
                            writer.write_all(&frame).await?;
                        }
                        FrontendMessage::ImportRoleDraft {
                            id,
                            proposal_id,
                            role_name,
                            parent,
                        } => {
                            // Phase 119 — operator's act-on-approval
                            // gesture for a RoleDefinitionSuggestion.
                            // Same shape as ApplyProfileHint.
                            let resp = match audit_log.as_ref() {
                                None => DaemonMessage::RoleDraftImportAcked {
                                    id,
                                    ok: false,
                                    error: Some("daemon has no audit log configured".into()),
                                },
                                Some(al) => {
                                    let event = aivyx_audit::AuditEvent::RoleDraftImported {
                                        session_id: aivyx_core::SessionId::new(),
                                        proposal_id,
                                        role_name,
                                        parent,
                                    };
                                    match al.append(event) {
                                        Ok(_) => DaemonMessage::RoleDraftImportAcked {
                                            id,
                                            ok: true,
                                            error: None,
                                        },
                                        Err(e) => DaemonMessage::RoleDraftImportAcked {
                                            id,
                                            ok: false,
                                            error: Some(e.to_string()),
                                        },
                                    }
                                }
                            };
                            let frame = encode_frame(&resp)?;
                            writer.write_all(&frame).await?;
                        }
                        FrontendMessage::ImportPersonaChain {
                            id,
                            deltas,
                            effective_at_export: _,
                            force,
                        } => {
                            // Phase 65 Task 3 — replay an exported
                            // chain. Best-effort (Q1(a)): no
                            // transaction wrapping; daemon crash
                            // mid-import leaves chain partial.
                            // Operator re-imports to recover.
                            let resp = match resolve_persona_import(
                                persona_log.as_deref(),
                                &shared_persona,
                                deltas,
                                force,
                            )
                            .await
                            {
                                Ok(success) => DaemonMessage::PersonaImportResolved {
                                    id,
                                    ok: true,
                                    success: Some(success),
                                    error: None,
                                },
                                Err(reason) => DaemonMessage::PersonaImportResolved {
                                    id,
                                    ok: false,
                                    success: None,
                                    error: Some(reason),
                                },
                            };
                            let frame = encode_frame(&resp)?;
                            writer.write_all(&frame).await?;
                        }
                    }
                }
                Err(FrameError::IncompleteBuf) => break,
                Err(e) => {
                    let err_resp = DaemonMessage::Error {
                        code: "invalid_message".into(),
                        message: e.to_string(),
                    };
                    let frame = encode_frame(&err_resp).unwrap_or_default();
                    let _ = writer.write_all(&frame).await;
                    return Err(e.into());
                }
            }
        }
    }

    // Deregister session from daemon state on disconnect.
    if let Some(ref sid) = session_id {
        if let Ok(mut st) = daemon_state.lock() {
            st.sessions.retain(|s| &s.session_id != sid);
        }
    }

    Ok(())
}

/// Backward-compatible single-connection daemon for tests that don't
/// need multi-connection or channel-factory semantics. Accepts one
/// connection, serves it to completion, then returns.
pub async fn run_poc_daemon<C: ChannelContext + Send + Sync + 'static>(
    socket_path: &Path,
    agent: Arc<dyn Agent>,
    channel: Arc<C>,
) -> Result<(), DaemonError> {
    let channel: Arc<dyn ChannelContext + Send + Sync> = channel;
    let factory: ChannelFactory = Arc::new(move |_| Arc::clone(&channel));
    run_single_connection_daemon(socket_path, agent, factory).await
}

/// Accept exactly one connection, serve it to completion, then return.
/// Used by `run_poc_daemon` and tests that need deterministic shutdown.
async fn run_single_connection_daemon(
    socket_path: &Path,
    agent: Arc<dyn Agent>,
    channel_factory: ChannelFactory,
) -> Result<(), DaemonError> {
    let _ = std::fs::remove_file(socket_path);

    if let Some(parent) = socket_path.parent() {
        create_dir_all_0700(parent)?;
    }

    let listener = bind_unix_socket_0600(socket_path).map_err(|source| DaemonError::Bind {
        path: socket_path.display().to_string(),
        source,
    })?;

    let (stream, _addr) = listener.accept().await.map_err(DaemonError::Accept)?;

    let shutdown = CancellationToken::new();
    let no_recovery = Arc::new(std::sync::Mutex::new(None));
    let empty_state = Arc::new(std::sync::Mutex::new(DaemonState {
        pid: std::process::id(),
        started_at: 0,
        sessions: Vec::new(),
        in_flight_turns: Vec::new(),
    }));
    handle_connection(ConnectionContext {
        stream,
        agent,
        channel_factory,
        shutdown,
        mission_store: None,
        schedule_store: None,
        notify_targets: Arc::new(Vec::new()),
        pending_recovery: no_recovery,
        daemon_state: empty_state,
        audit_log: None,
        profile: Arc::new(aivyx_config::Profile::default()),
        persona_log: None,
        shared_persona: crate::persona::shared_effective_persona(
            crate::persona::EffectivePersona::default(),
        ),
        persona_proposal_log: None,
        memory: None,
        embedding_provider: None,
        recall_log: None,
        helpfulness_ledger: None,
        cooccurrence_ledger: None,
        wiki_store: None,
        graph_store: None,
        conflict_dismissals: None,
        correction_ledger: None,
        persona_selection_stat: None,
        recall_cluster_stat: None,
        proactive_stat: None,
        persona_lifecycle_stat: None,
        conversation_windows: None,
        persona_consolidation_stat: None,
        correction_consolidation_stat: None,
        correction_judgment_stat: None,
        recall_judgment_stat: None,
        recall_feedback_config: None,
        cadence_stats: crate::reflection_scheduler::shared_recent_reflection_stats(),
        tool_descriptors: Arc::from(Vec::<ToolDescriptor>::new()),
        skill_auto_proposer: None,
        tool_relevance_ledger: None,
        skill_effectiveness_ledger: None,
        loop_backlog: None,
        loop_state: None,
        loop_config: None,
        team_missions: None,
        gate_policy: GatePolicy::default(),
        channel_trigger_authz: ChannelTriggerAuthz::default(),
        config_toml_path: None,
        role_override: None,
        team_config_write_path: None,
        seed_draft_llm: None,
        document_roots: Default::default(),
        reminder_store: None,
        comfyui_base_url: None,
    })
    .await
}

/// Backward-compatible single-channel daemon with shutdown token.
pub async fn run_daemon_compat<C: ChannelContext + Send + Sync + 'static>(
    socket_path: &Path,
    agent: Arc<dyn Agent>,
    channel: Arc<C>,
    shutdown: CancellationToken,
) -> Result<(), DaemonError> {
    let channel_for_factory: Arc<dyn ChannelContext + Send + Sync> = channel;
    let factory: ChannelFactory = Arc::new(move |_| Arc::clone(&channel_for_factory));
    run_daemon(DaemonConfig {
        socket_path: socket_path.to_path_buf(),
        agent,
        channel_factory: factory,
        shutdown,
        mission_store: None,
        notify_dispatcher: None,
        default_notify_target: None,
        notify_targets: Vec::new(),
        schedule_store: None,
        webhook_store: None,
        file_watch_store: None,
        webhook_port: None,
        web_ui_port: None,
        web_ui_host: None,
        web_ui_allowed_origins: Vec::new(),
        web_ui_auth_token: None,
        comfyui_base_url: None,
        memory: None,
        memory_ttl_secs: None,
        audit_log: None,
        profile: Arc::new(aivyx_config::Profile::default()),
        persona_log: None,
        shared_persona: crate::persona::shared_effective_persona(
            crate::persona::EffectivePersona::default(),
        ),
        web_ui_broadcaster: None,
        persona_proposal_log: None,
        reflection_schedules: Vec::new(),
        workspace_journaling_interval: None,
        target_policies: std::collections::HashMap::new(),
        embedding_provider: None,
        recall_log: None,
        helpfulness_ledger: None,
        cooccurrence_ledger: None,
        wiki_store: None,
        graph_store: None,
        conflict_dismissals: None,
        correction_ledger: None,
        persona_selection_stat: None,
        recall_cluster_stat: None,
        proactive_config: None,
        proactive_log: None,
        proactive_stat: None,
        persona_lifecycle_config: None,
        persona_lifecycle_stat: None,
        memory_retention: Vec::new(),
        conversation_windows: None,
        persona_consolidation_config: None,
        persona_consolidation_stat: None,
        persona_consolidation_phraser: None,
        correction_consolidation_config: None,
        correction_consolidation_stat: None,
        correction_consolidation_phraser: None,
        recall_judgment_config: None,
        recall_judgment_stat: None,
        recall_judge: None,
        correction_judgment_config: None,
        correction_judge: None,
        correction_judgment_stat: None,
        correction_signal_config: None,
        recall_feedback_config: None,
        tool_descriptors: Vec::new(),
        skill_auto_proposer: None,
        tool_relevance_ledger: None,
        skill_effectiveness_ledger: None,
        skill_refinement_config: None,
        skill_refinement_drafter: None,
        skill_authoring_config: None,
        skill_authoring_drafter: None,
        loop_backlog: None,
        loop_state: None,
        loop_config: None,
        team_missions: None,
        gate_policy: GatePolicy::default(),
        pricing: Default::default(),
        config_toml_path: None,
        role_override: None,
        team_config_write_path: None,
        seed_draft_llm: None,
        document_roots: Default::default(),
        reminder_store: None,
        wiki_sweep: None,
        graph_sweep: None,
    })
    .await
}

async fn send_shutting_down(writer: &mut tokio::net::unix::OwnedWriteHalf, reason: &str) {
    let event = DaemonLifecycleEvent::ShuttingDown {
        reason: reason.to_string(),
    };
    if let Ok(frame) = encode_frame(&event) {
        let _ = writer.write_all(&frame).await;
    }
}

fn format_outcome(outcome: &TurnOutcome) -> String {
    match outcome {
        TurnOutcome::Completed { final_message, .. } => {
            format!("completed: {final_message}")
        }
        TurnOutcome::Failed(e) => format!("failed: {e}"),
        TurnOutcome::Cancelled { .. } => "cancelled".into(),
        TurnOutcome::TimedOut { .. } => "timed out".into(),
        TurnOutcome::MaxStepsExceeded { max_steps, .. } => {
            format!("aborted: planner exceeded {max_steps} steps")
        }
        TurnOutcome::Looping { repeat_limit, .. } => {
            format!("stopped: {repeat_limit} repeated identical tool calls")
        }
        TurnOutcome::Escalated { reason, .. } => {
            format!("escalated: {reason}")
        }
    }
}

// ---------------------------------------------------------------------------
// PidGuard — writes PID file on create, removes on drop
// ---------------------------------------------------------------------------

struct PidGuard {
    path: PathBuf,
}

impl PidGuard {
    fn write(path: &Path) -> Result<Self, DaemonError> {
        let pid = std::process::id();
        std::fs::write(path, pid.to_string()).map_err(|source| DaemonError::PidFile {
            path: path.display().to_string(),
            source,
        })?;
        Ok(PidGuard {
            path: path.to_path_buf(),
        })
    }
}

impl Drop for PidGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

// ---------------------------------------------------------------------------
// StateGuard — crash-recovery metadata (Phase 41 Task 4)
// ---------------------------------------------------------------------------

/// One tracked daemon session — channel identity, trust posture, and
/// activity timestamps. Replaces a bare session-id string
/// (`/classic` retirement, `docs/superpowers/specs/2026-08-27-
/// classic-retirement-design.md`) so the Studio's Sessions screen can
/// show more than an opaque id.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SessionRecord {
    pub session_id: String,
    pub channel: aivyx_core::ChannelPlatform,
    pub trust_tier: aivyx_capability::TrustTier,
    pub created_at_ms: u64,
    pub last_active_at_ms: u64,
}

/// Maps the real `aivyx_core::ChannelPlatform` onto `aivyx-ipc`'s
/// wasm32-clean mirror, `aivyx_ipc::WireChannelPlatform`.
///
/// This has to live here rather than as a `From` impl in either crate:
/// `aivyx-ipc` must not depend on `aivyx-core` (see that crate's package
/// description and `WireChannelPlatform`'s doc comment), so it cannot
/// name `aivyx_core::ChannelPlatform`; and a `From<ForeignType> for
/// ForeignType` impl living in this third crate would violate the
/// orphan rule regardless. `aivyx-channel` already depends on both, so
/// a plain function here is the correct home. Deliberately no `_`
/// catch-all: a future new `ChannelPlatform` variant must fail to
/// compile here instead of silently mismapping.
pub fn to_wire_channel_platform(platform: aivyx_core::ChannelPlatform) -> WireChannelPlatform {
    match platform {
        aivyx_core::ChannelPlatform::Local => WireChannelPlatform::Local,
        aivyx_core::ChannelPlatform::Telegram => WireChannelPlatform::Telegram,
        aivyx_core::ChannelPlatform::Discord => WireChannelPlatform::Discord,
        aivyx_core::ChannelPlatform::Slack => WireChannelPlatform::Slack,
        aivyx_core::ChannelPlatform::Matrix => WireChannelPlatform::Matrix,
        aivyx_core::ChannelPlatform::Email => WireChannelPlatform::Email,
        aivyx_core::ChannelPlatform::Rest => WireChannelPlatform::Rest,
        aivyx_core::ChannelPlatform::Voice => WireChannelPlatform::Voice,
    }
}

/// Serializable snapshot of the daemon's active sessions and in-flight
/// turns. Written to `daemon.state` on startup; cleared on clean
/// shutdown. If a stale file is found on next startup, it means the
/// previous daemon crashed — the data inside tells the operator which
/// sessions/turns were lost.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DaemonState {
    pub pid: u32,
    pub started_at: u64,
    pub sessions: Vec<SessionRecord>,
    pub in_flight_turns: Vec<String>,
}

/// RAII guard that writes `daemon.state` on creation and removes it on
/// drop (clean shutdown). Holds a shared handle so `handle_connection`
/// can register/deregister sessions and turns.
struct StateGuard {
    path: PathBuf,
    state: Arc<std::sync::Mutex<DaemonState>>,
}

impl StateGuard {
    fn write(path: &Path) -> Result<Self, DaemonError> {
        let state = DaemonState {
            pid: std::process::id(),
            started_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            sessions: Vec::new(),
            in_flight_turns: Vec::new(),
        };
        Self::persist(path, &state)?;
        Ok(StateGuard {
            path: path.to_path_buf(),
            state: Arc::new(std::sync::Mutex::new(state)),
        })
    }

    fn shared(&self) -> Arc<std::sync::Mutex<DaemonState>> {
        Arc::clone(&self.state)
    }

    fn persist(path: &Path, state: &DaemonState) -> Result<(), DaemonError> {
        let json = serde_json::to_string_pretty(state)
            .map_err(|e| DaemonError::Internal(format!("serialize state: {e}")))?;
        std::fs::write(path, json).map_err(|source| DaemonError::PidFile {
            path: path.display().to_string(),
            source,
        })
    }
}

impl Drop for StateGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Check for a stale `daemon.state` file from a previous crash.
/// Returns `Some(DaemonState)` if a crash is detected, `None` otherwise.
///
/// A clean shutdown removes the state file via `StateGuard::drop`, so
/// any remaining file means the previous daemon exited abnormally.
/// As a safety check, if the recorded PID matches the current process
/// (e.g., test reuse), the file is treated as stale, not a live
/// collision.
fn detect_crash_recovery(state_path: &Path) -> Option<DaemonState> {
    let contents = std::fs::read_to_string(state_path).ok()?;
    let state: DaemonState = serde_json::from_str(&contents).ok()?;
    Some(state)
}

// ---------------------------------------------------------------------------
// Phase 47 — query dispatch
// ---------------------------------------------------------------------------

/// Phase 47 — answer a [`QueryPayload`] from the daemon's in-memory state
/// and persistent stores.
///
/// Read-only by contract. Authorization is enforced at the IPC socket
/// boundary (mode 0600, operator-owned) — see `PRODUCT.md` P6 and
/// `docs/THREAT_MODEL.md` §4.4. Per Q2 of the Phase 47 open doc, no
/// capability check applies at the query layer.
///
/// A poisoned `DaemonState` mutex, a missing mission store, or a
/// storage error are all reported as [`QueryResponsePayload::QueryError`]
/// rather than propagated as a panic. The daemon must stay alive even
/// if one connection's state interaction tripped earlier.
// Eight parameters because the query dispatcher fans out across
// every daemon-side substrate the read-only queries can touch.
// Bundling them into a context struct is a future refactor that
// touches every existing query test fixture; deferred.
#[allow(clippy::too_many_arguments)]
async fn handle_query(
    payload: QueryPayload,
    daemon_state: &Arc<std::sync::Mutex<DaemonState>>,
    mission_store: Option<&DomainHandle>,
    schedule_store: Option<&DomainHandle>,
    notify_targets: &[aivyx_config::NotifyTargetConfig],
    audit_log: Option<&PersistentAuditLog>,
    profile: &aivyx_config::Profile,
    persona_log: Option<&crate::persona::PersistentPersonaLog>,
    shared_persona: &crate::persona::SharedEffectivePersona,
    persona_proposal_log: Option<&crate::persona_proposal::PersistentPersonaProposalLog>,
    memory: Option<&Arc<dyn aivyx_memory::Memory>>,
    embedding_provider: Option<&Arc<dyn aivyx_llm::embedding::EmbeddingProvider>>,
    recall_log: Option<&Arc<crate::recall_log::PersistentRecallLog>>,
    helpfulness_ledger: Option<&Arc<crate::helpfulness_ledger::PersistentHelpfulnessLedger>>,
    cooccurrence_ledger: Option<&Arc<crate::cooccurrence_ledger::PersistentCooccurrenceLedger>>,
    wiki_store: Option<&Arc<crate::knowledge_wiki::PersistentWikiStore>>,
    graph_store: Option<&Arc<crate::knowledge_graph::PersistentGraphStore>>,
    conflict_dismissals: Option<&Arc<crate::conflict_dismissals::PersistentConflictDismissals>>,
    skill_effectiveness_ledger: Option<&Arc<crate::skill_effectiveness::SkillEffectivenessLedger>>,
    correction_ledger: Option<&Arc<crate::correction_ledger::PersistentCorrectionLedger>>,
    persona_selection_stat: Option<&crate::persona_context::SharedPersonaSelectionStat>,
    recall_cluster_stat: Option<&crate::memory_recall::SharedRecallClusterStat>,
    proactive_stat: Option<&crate::proactive_detect::SharedProactiveStat>,
    persona_lifecycle_stat: Option<&crate::persona_lifecycle::SharedPersonaLifecycleStat>,
    persona_consolidation_stat: Option<
        &crate::persona_consolidation::SharedPersonaConsolidationStat,
    >,
    correction_consolidation_stat: Option<
        &crate::correction_consolidation::SharedCorrectionConsolidationStat,
    >,
    correction_judgment_stat: Option<&crate::correction_judgment::SharedCorrectionJudgmentStat>,
    recall_judgment_stat: Option<&crate::recall_judgment::SharedRecallJudgmentStat>,
    recall_feedback_config: Option<&aivyx_config::RecallFeedbackConfig>,
    cadence_stats: &crate::reflection_scheduler::SharedRecentReflectionStats,
    tool_descriptors: &[ToolDescriptor],
    tool_relevance_ledger: Option<
        &Arc<crate::tool_relevance_ledger::PersistentToolRelevanceLedger>,
    >,
    loop_backlog: Option<&Arc<crate::loop_backlog::PersistentLoopBacklog>>,
    loop_state: Option<&crate::loop_driver::SharedLoopState>,
    loop_config: Option<&aivyx_config::LoopConfig>,
    team_missions: Option<&crate::team_mission_driver::TeamMissionService>,
    // Chapter U — the loaded `aivyx-pa.toml` path for the Settings write
    // handlers. `None` ⇒ env-only launch; the write handlers refuse.
    config_toml_path: Option<&Path>,
    // Piece C follow-up — see `DaemonConfig::role_override`'s own doc
    // comment.
    role_override: Option<&str>,
    // Chapter Roster (RO.2) — the resolved team-config write target for the
    // `SetTeamRoster` handler. `None` ⇒ env-only launch; the handler refuses.
    team_config_write_path: Option<&Path>,
    // Chapter Z — the canonical roots for the Documents browser handlers.
    document_roots: &DocumentRoots,
    // Chapter Concord — the daemon's one-shot LLM handle (the same
    // provider + model the agent's turns use) for the on-demand
    // `GetMemoryConflicts` detection pass. `None` ⇒ no provider, so
    // detection returns an empty set (needs an LLM to judge).
    contradiction_llm: Option<&SeedDraftLlm>,
    // Studio Gallery — base URL of the `comfyui` `[[mcp_server]]`'s backing
    // ComfyUI instance. `None` ⇒ no `comfyui` server configured.
    comfyui_base_url: Option<&str>,
    // Phase 186 — see `DaemonConfig::reminder_store`'s own doc comment.
    reminder_store: Option<&crate::reminder_tool::SharedReminderStore>,
) -> QueryResponsePayload {
    /// Phase 47 Q3 — server-side cap on caller-supplied `limit` for
    /// audit queries. Prevents a single query from monopolizing the
    /// daemon on a long chain.
    const AUDIT_QUERY_MAX_LIMIT: u32 = 500;

    match payload {
        QueryPayload::ListSessions => match daemon_state.lock() {
            Ok(st) => {
                let sessions = st
                    .sessions
                    .iter()
                    .map(|s| SessionSummary {
                        session_id: s.session_id.clone(),
                        channel: to_wire_channel_platform(s.channel),
                        trust_tier: s.trust_tier,
                        created_at_ms: s.created_at_ms,
                        last_active_at_ms: s.last_active_at_ms,
                    })
                    .collect();
                QueryResponsePayload::ListSessions { sessions }
            }
            Err(_) => QueryResponsePayload::QueryError {
                code: "state_poisoned".into(),
                message: "daemon state mutex poisoned".into(),
            },
        },
        QueryPayload::ListMissions => {
            let Some(store) = mission_store else {
                return QueryResponsePayload::QueryError {
                    code: "no_mission_store".into(),
                    message: "daemon has no mission store configured".into(),
                };
            };
            match mission::list_missions(store).await {
                Ok(records) => {
                    let missions = records
                        .into_iter()
                        .map(mission_summary_from_record)
                        .collect();
                    QueryResponsePayload::ListMissions { missions }
                }
                Err(e) => QueryResponsePayload::QueryError {
                    code: "list_missions_failed".into(),
                    message: e.to_string(),
                },
            }
        }
        QueryPayload::GetMission { mission_id } => {
            let Some(store) = mission_store else {
                return QueryResponsePayload::QueryError {
                    code: "no_mission_store".into(),
                    message: "daemon has no mission store configured".into(),
                };
            };
            match mission::get_mission(store, &mission_id).await {
                Ok(Some(record)) => QueryResponsePayload::GetMission {
                    mission: Some(mission_detail_from_record(record)),
                },
                Ok(None) => QueryResponsePayload::GetMission { mission: None },
                Err(e) => QueryResponsePayload::QueryError {
                    code: "get_mission_failed".into(),
                    message: e.to_string(),
                },
            }
        }
        QueryPayload::ListAuditEntries { from_seq, limit } => {
            let Some(log) = audit_log else {
                return QueryResponsePayload::QueryError {
                    code: "no_audit_log".into(),
                    message: "daemon has no audit log configured".into(),
                };
            };
            let capped = limit.min(AUDIT_QUERY_MAX_LIMIT) as usize;
            match log.entries_range(from_seq, capped) {
                Ok(rows) => {
                    let entries: Vec<AuditEntrySummary> = rows
                        .into_iter()
                        .map(audit_entry_summary_from_signed)
                        .collect();
                    QueryResponsePayload::ListAuditEntries {
                        entries,
                        total_len: log.len() as u64,
                    }
                }
                Err(e) => QueryResponsePayload::QueryError {
                    code: "list_audit_failed".into(),
                    message: e.to_string(),
                },
            }
        }
        QueryPayload::VerifyAuditChain => {
            let Some(log) = audit_log else {
                return QueryResponsePayload::QueryError {
                    code: "no_audit_log".into(),
                    message: "daemon has no audit log configured".into(),
                };
            };
            let total_len = log.len() as u64;
            match log.verify() {
                Ok(()) => QueryResponsePayload::VerifyAuditChain {
                    ok: true,
                    entries_verified: total_len,
                    error: None,
                },
                Err(e) => QueryResponsePayload::VerifyAuditChain {
                    ok: false,
                    entries_verified: 0,
                    error: Some(e.to_string()),
                },
            }
        }
        QueryPayload::GetToolStats { window_secs } => {
            let Some(log) = audit_log else {
                return QueryResponsePayload::QueryError {
                    code: "no_audit_log".into(),
                    message: "daemon has no audit log configured".into(),
                };
            };
            // `window_secs` → an absolute cutoff; `None` = whole
            // chain. A clock that cannot subtract `secs` (absurdly
            // large window) just yields `None` → whole chain.
            let cutoff = window_secs.and_then(|secs| {
                std::time::SystemTime::now().checked_sub(std::time::Duration::from_secs(secs))
            });
            // The whole chain is loaded — an observability query,
            // not a hot path, and the chain is bounded (Phase 53).
            match log.entries_range(0, log.len()) {
                Ok(rows) => QueryResponsePayload::ToolStats {
                    tools: fold_tool_stats(&rows, cutoff, tool_descriptors),
                },
                Err(e) => QueryResponsePayload::QueryError {
                    code: "tool_stats_failed".into(),
                    message: e.to_string(),
                },
            }
        }
        QueryPayload::GetMcpServerCallStats { window_secs } => {
            let Some(log) = audit_log else {
                return QueryResponsePayload::QueryError {
                    code: "no_audit_log".into(),
                    message: "daemon has no audit log configured".into(),
                };
            };
            let cutoff = window_secs.and_then(|secs| {
                std::time::SystemTime::now().checked_sub(std::time::Duration::from_secs(secs))
            });
            match log.entries_range(0, log.len()) {
                Ok(rows) => QueryResponsePayload::McpServerCallStats {
                    servers: fold_mcp_server_stats(&rows, cutoff),
                },
                Err(e) => QueryResponsePayload::QueryError {
                    code: "mcp_server_call_stats_failed".into(),
                    message: e.to_string(),
                },
            }
        }
        QueryPayload::GetReminders => reminders_query_response(reminder_store).await,
        QueryPayload::DumpToolRelevance { keyword_key_filter } => {
            let Some(ledger) = tool_relevance_ledger else {
                return QueryResponsePayload::QueryError {
                    code: "no_tool_relevance_ledger".into(),
                    message: "daemon has no tool-relevance ledger configured \
                         (enable `[tool_relevance]` in aivyx-pa.toml)"
                        .into(),
                };
            };
            let entries = match ledger.list_all_entries(keyword_key_filter.as_deref()).await {
                Ok(e) => e,
                Err(e) => {
                    return QueryResponsePayload::QueryError {
                        code: "tool_relevance_dump_failed".into(),
                        message: e.to_string(),
                    };
                }
            };
            let mut rows: Vec<crate::daemon_ipc::ToolRelevanceDumpRow> = Vec::new();
            for (keyword_key, entry) in entries {
                for row in entry.outcomes {
                    rows.push(crate::daemon_ipc::ToolRelevanceDumpRow {
                        keyword_key: keyword_key.clone(),
                        surface_kind: row.surface_kind.label().to_string(),
                        identifier: row.identifier,
                        success_count: row.success_count,
                        failure_count: row.failure_count,
                        last_seen_unix_ms: row.last_seen_unix_ms,
                    });
                }
            }
            // Stable column ordering for the operator-facing table.
            rows.sort_by(|a, b| {
                (
                    a.keyword_key.as_str(),
                    a.surface_kind.as_str(),
                    a.identifier.as_str(),
                )
                    .cmp(&(
                        b.keyword_key.as_str(),
                        b.surface_kind.as_str(),
                        b.identifier.as_str(),
                    ))
            });
            QueryResponsePayload::ToolRelevanceDump { rows }
        }
        // ---- Phase 173 — autonomous loop control ---------------
        QueryPayload::LoopAdd {
            title,
            body,
            priority,
        } => {
            let Some(backlog) = loop_backlog else {
                return QueryResponsePayload::QueryError {
                    code: "no_loop_backlog".into(),
                    message: "daemon has no loop backlog configured".into(),
                };
            };
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let priority = priority.unwrap_or_else(|| {
                loop_config
                    .map(|c| c.default_priority)
                    .unwrap_or(aivyx_config::DEFAULT_LOOP_PRIORITY)
            });
            let story_id = format!("ls-{}", uuid::Uuid::new_v4().as_simple());
            match backlog
                .add_story(story_id.clone(), now_ms, priority, title, body)
                .await
            {
                Ok(_) => QueryResponsePayload::LoopStoryAdded { story_id },
                Err(e) => QueryResponsePayload::QueryError {
                    code: "loop_add_failed".into(),
                    message: e.to_string(),
                },
            }
        }
        QueryPayload::LoopList => {
            let Some(backlog) = loop_backlog else {
                return QueryResponsePayload::QueryError {
                    code: "no_loop_backlog".into(),
                    message: "daemon has no loop backlog configured".into(),
                };
            };
            QueryResponsePayload::LoopBacklog {
                stories: backlog.list(crate::loop_backlog::StoryStatusFilter::All),
            }
        }
        QueryPayload::LoopStart { max_iterations } => {
            let (Some(state), Some(cfg)) = (loop_state, loop_config) else {
                return QueryResponsePayload::LoopControl {
                    ok: false,
                    message: "the [loop] section is not armed (set \
                              `[loop] enabled = true` in aivyx-pa.toml and \
                              restart the daemon)"
                        .into(),
                };
            };
            // The configured cap is the ceiling; a per-run request
            // may only lower it.
            let requested = max_iterations
                .unwrap_or(cfg.max_iterations)
                .min(cfg.max_iterations)
                .max(1);
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            if state.request_start(requested, now_ms) {
                // Chapter Helm — persist the active marker so an opt-in
                // `resume_on_boot` resumes this run after a crash/restart.
                // No-op unless resume_on_boot attached a store.
                state.persist_run_marker(true).await;
                QueryResponsePayload::LoopControl {
                    ok: true,
                    message: format!("loop run started (max_iterations={requested})"),
                }
            } else {
                QueryResponsePayload::LoopControl {
                    ok: false,
                    message: "a loop run is already active".into(),
                }
            }
        }
        QueryPayload::LoopStop => {
            let Some(state) = loop_state else {
                return QueryResponsePayload::LoopControl {
                    ok: false,
                    message: "the [loop] section is not armed".into(),
                };
            };
            if state.request_stop() {
                // Chapter Helm — an explicit stop clears the marker, so a
                // later restart does NOT resume (the deliberate stop wins).
                state.persist_run_marker(false).await;
                QueryResponsePayload::LoopControl {
                    ok: true,
                    message: "loop run stopping (after the current \
                              iteration)"
                        .into(),
                }
            } else {
                QueryResponsePayload::LoopControl {
                    ok: false,
                    message: "no loop run is active".into(),
                }
            }
        }
        QueryPayload::LoopStatus => {
            let remaining = loop_backlog.map(|b| b.remaining_count()).unwrap_or(0);
            let state = loop_state.map(|s| s.snapshot()).unwrap_or_default();
            QueryResponsePayload::LoopStatus {
                state,
                remaining,
                armed: loop_state.is_some(),
                gate_enabled: loop_config
                    .map(|c| c.gate_command.is_some())
                    .unwrap_or(false),
                max_run_secs: loop_config.and_then(|c| c.max_run_secs),
                max_run_tokens: loop_config.and_then(|c| c.max_run_tokens),
                max_run_usd: loop_config.and_then(|c| c.max_run_usd),
                max_idle_iterations: loop_config.map(|c| c.max_idle_iterations).unwrap_or(0),
            }
        }
        QueryPayload::LoopLog { limit } => {
            let limit = limit.unwrap_or(50).max(1) as usize;
            let notes = match memory {
                Some(m) => m
                    .get_recent(crate::loop_tool::LOOP_PROGRESS_TOPIC, limit)
                    .await
                    .map(|entries| entries.into_iter().map(|e| e.body).collect())
                    .unwrap_or_default(),
                None => Vec::new(),
            };
            QueryResponsePayload::LoopProgressLog { notes }
        }
        QueryPayload::LoopSkip { story_id } => {
            let Some(backlog) = loop_backlog else {
                return QueryResponsePayload::LoopControl {
                    ok: false,
                    message: "daemon has no loop backlog configured".into(),
                };
            };
            // Guard: only a pending story can be skipped — give a
            // clear reason rather than a chain error.
            match backlog.get(&story_id) {
                None => QueryResponsePayload::LoopControl {
                    ok: false,
                    message: format!("unknown story `{story_id}`"),
                },
                Some(s) if !matches!(s.status, crate::loop_backlog::StoryStatus::Pending) => {
                    QueryResponsePayload::LoopControl {
                        ok: false,
                        message: format!(
                            "story `{story_id}` is not pending \
                             (already resolved)"
                        ),
                    }
                }
                Some(_) => {
                    let now_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as u64)
                        .unwrap_or(0);
                    match backlog
                        .mark_skipped(story_id.clone(), now_ms, Some("operator skip".into()))
                        .await
                    {
                        Ok(_) => QueryResponsePayload::LoopControl {
                            ok: true,
                            message: format!("skipped story `{story_id}`"),
                        },
                        Err(e) => QueryResponsePayload::LoopControl {
                            ok: false,
                            message: format!("skip failed: {e}"),
                        },
                    }
                }
            }
        }
        QueryPayload::TeamRun { plan, config } => {
            let Some(svc) = team_missions else {
                return no_team_missions();
            };
            match svc.start(plan, config).await {
                Ok(mission_id) => QueryResponsePayload::TeamRunStarted { mission_id },
                Err(e) => QueryResponsePayload::QueryError {
                    code: "team_run_failed".into(),
                    message: e.to_string(),
                },
            }
        }
        QueryPayload::TeamRunGoal { goal, config } => {
            let Some(svc) = team_missions else {
                return no_team_missions();
            };
            match svc.start_from_goal(&goal, config).await {
                Ok(mission_id) => QueryResponsePayload::TeamRunStarted { mission_id },
                Err(e) => QueryResponsePayload::QueryError {
                    code: "team_run_goal_failed".into(),
                    message: e.to_string(),
                },
            }
        }
        QueryPayload::TeamMissionList => {
            let Some(svc) = team_missions else {
                return no_team_missions();
            };
            QueryResponsePayload::TeamMissionList {
                missions: svc.list(),
            }
        }
        QueryPayload::TeamMissionStatus { mission_id } => {
            let Some(svc) = team_missions else {
                return no_team_missions();
            };
            QueryResponsePayload::TeamMissionStatus {
                mission: svc.snapshot(&mission_id),
            }
        }
        QueryPayload::GetTeamRoster => {
            // Chapter Y — the active team roster for the Studio's Teams screen.
            let Some(svc) = team_missions else {
                return no_team_missions();
            };
            QueryResponsePayload::GetTeamRoster {
                roster: svc.team_config(),
            }
        }
        QueryPayload::GetMcpStatus => {
            // Chapter Lantern — the Studio MCP screen. Read the daemon's
            // last-start status snapshot (Chapter Conduit CD.3). Absent or
            // unreadable → an empty board (captured_unix 0), never an error.
            let snapshot = crate::mcp_status::read_snapshot().ok().flatten();
            let (captured_unix, servers) = snapshot
                .map(|s| (s.captured_unix, s.servers))
                .unwrap_or((0, Vec::new()));
            QueryResponsePayload::GetMcpStatus {
                captured_unix,
                servers,
            }
        }
        QueryPayload::GetMcpServerConfigs => {
            let path = match config_toml_path {
                Some(p) => p,
                None => return no_config_file_error(),
            };
            match read_mcp_server_configs(path) {
                Ok(servers) => QueryResponsePayload::GetMcpServerConfigs { servers },
                Err(e) => QueryResponsePayload::QueryError {
                    code: "config_reload_failed".into(),
                    message: format!("failed to read mcp server configs: {e}"),
                },
            }
        }
        QueryPayload::GetNotifyTargetConfigs => {
            let path = match config_toml_path {
                Some(p) => p,
                None => return no_config_file_error(),
            };
            match read_notify_target_configs(path) {
                Ok(targets) => QueryResponsePayload::GetNotifyTargetConfigs { targets },
                Err(e) => QueryResponsePayload::QueryError {
                    code: "config_reload_failed".into(),
                    message: format!("failed to read notify target configs: {e}"),
                },
            }
        }
        QueryPayload::GetSchedules => {
            // Command Center — the agent's scheduled background routines.
            // Read-only: list the schedule store, map each record to a wasm-clean
            // view with its next fire computed. Absent store / read error → empty
            // list (the dashboard shows "no routines"), never an error.
            let schedules = match schedule_store {
                Some(store) => match crate::schedule::list_schedules(store).await {
                    Ok(records) => records
                        .iter()
                        .map(|r| aivyx_ipc::protocol::ScheduleView {
                            name: r
                                .schedule_id
                                .strip_prefix("cfg-")
                                .unwrap_or(&r.schedule_id)
                                .to_string(),
                            cron: r.cron_expr.clone(),
                            role: r.role_name.clone(),
                            enabled: r.enabled,
                            last_fired_unix_ms: r.last_fired_at,
                            next_fire_unix_ms: r
                                .next_fire_time()
                                .map(|dt| dt.timestamp_millis() as u64),
                            schedule_id: r.schedule_id.clone(),
                            created_by: r.created_by.as_str().to_string(),
                            prompt: r.prompt.clone(),
                        })
                        .collect(),
                    Err(_) => Vec::new(),
                },
                None => Vec::new(),
            };
            QueryResponsePayload::Schedules { schedules }
        }
        QueryPayload::GetNotifyTargets => {
            // Chapter Herald — read-only view of configured notify targets
            // (including the daemon's synthesized default "studio" one, if
            // any) for the Studio Notifications screen.
            let targets = notify_targets
                .iter()
                .map(|t| aivyx_ipc::protocol::NotifyTargetView {
                    name: t.name.clone(),
                    kind: match &t.kind {
                        aivyx_config::NotifyTargetKind::Telegram { .. } => "telegram",
                        aivyx_config::NotifyTargetKind::Webhook { .. } => "webhook",
                        aivyx_config::NotifyTargetKind::Email { .. } => "email",
                        aivyx_config::NotifyTargetKind::WebUi => "webui",
                    }
                    .to_string(),
                    is_default: t.is_default,
                })
                .collect();
            QueryResponsePayload::GetNotifyTargets { targets }
        }
        QueryPayload::ListDir { root, path } => {
            // Chapter Z — read-only directory listing, scoped + escape-guarded.
            let dir = match resolve_document_root(document_roots, &root) {
                Ok(d) => d,
                Err(resp) => return resp,
            };
            match crate::document_browse::list_dir(dir, &path) {
                Ok(entries) => QueryResponsePayload::ListDir { entries, path },
                Err(e) => map_browse_error(e),
            }
        }
        QueryPayload::ReadFile { root, path } => {
            let dir = match resolve_document_root(document_roots, &root) {
                Ok(d) => d,
                Err(resp) => return resp,
            };
            match crate::document_browse::read_file(dir, &path) {
                Ok(file) => QueryResponsePayload::ReadFile { file },
                Err(e) => map_browse_error(e),
            }
        }
        QueryPayload::WriteFile {
            root,
            path,
            content,
            overwrite,
        } => {
            // Chapter DW — create/save a file (atomic, escape-guarded).
            let dir = match resolve_document_root(document_roots, &root) {
                Ok(d) => d,
                Err(resp) => return resp,
            };
            let res = crate::document_browse::write_file(dir, &path, &content, overwrite);
            if res.is_ok() {
                audit_document_mutation(audit_log, "write", &root, &path);
            }
            fs_mutation_result(res)
        }
        QueryPayload::DeleteFile {
            root,
            path,
            confirm,
        } => {
            // Hard gate: a Documents delete always needs an explicit confirm.
            if !confirm {
                return QueryResponsePayload::FsMutation {
                    ok: false,
                    error: Some("delete requires confirmation".to_string()),
                };
            }
            let dir = match resolve_document_root(document_roots, &root) {
                Ok(d) => d,
                Err(resp) => return resp,
            };
            let res = crate::document_browse::delete_file(dir, &path);
            if res.is_ok() {
                audit_document_mutation(audit_log, "delete", &root, &path);
            }
            fs_mutation_result(res)
        }
        QueryPayload::RenamePath {
            root,
            path,
            new_path,
        } => {
            let dir = match resolve_document_root(document_roots, &root) {
                Ok(d) => d,
                Err(resp) => return resp,
            };
            let res = crate::document_browse::rename_path(dir, &path, &new_path);
            if res.is_ok() {
                audit_document_mutation(
                    audit_log,
                    "rename",
                    &root,
                    &format!("{path} -> {new_path}"),
                );
            }
            fs_mutation_result(res)
        }
        QueryPayload::MakeDir { root, path } => {
            let dir = match resolve_document_root(document_roots, &root) {
                Ok(d) => d,
                Err(resp) => return resp,
            };
            let res = crate::document_browse::make_dir(dir, &path);
            if res.is_ok() {
                audit_document_mutation(audit_log, "mkdir", &root, &path);
            }
            fs_mutation_result(res)
        }
        QueryPayload::ResolveTeamGate {
            mission_id,
            step,
            approve,
        } => {
            let Some(svc) = team_missions else {
                return no_team_missions();
            };
            match svc.resolve(&mission_id, &step, approve).await {
                Ok(phase) => QueryResponsePayload::TeamGateResolved { mission_id, phase },
                Err(e) => QueryResponsePayload::QueryError {
                    code: "resolve_team_gate_failed".into(),
                    message: e.to_string(),
                },
            }
        }
        QueryPayload::AbortTeamMission { mission_id } => {
            let Some(svc) = team_missions else {
                return no_team_missions();
            };
            match svc.abort(&mission_id) {
                Ok(message) => QueryResponsePayload::TeamMissionAborted {
                    mission_id,
                    message,
                },
                Err(e) => QueryResponsePayload::QueryError {
                    code: "abort_team_mission_failed".into(),
                    message: e.to_string(),
                },
            }
        }
        QueryPayload::PauseTeamMission { mission_id } => {
            let Some(svc) = team_missions else {
                return no_team_missions();
            };
            match svc.pause(&mission_id) {
                Ok(message) => QueryResponsePayload::TeamMissionPaused {
                    mission_id,
                    message,
                },
                Err(e) => QueryResponsePayload::QueryError {
                    code: "pause_team_mission_failed".into(),
                    message: e.to_string(),
                },
            }
        }
        QueryPayload::ResumeTeamMission { mission_id } => {
            let Some(svc) = team_missions else {
                return no_team_missions();
            };
            match svc.resume(&mission_id).await {
                Ok(phase) => QueryResponsePayload::TeamMissionResumed {
                    mission_id,
                    phase,
                },
                Err(e) => QueryResponsePayload::QueryError {
                    code: "resume_team_mission_failed".into(),
                    message: e.to_string(),
                },
            }
        }
        QueryPayload::GetProfile { from_disk } => {
            // `from_disk = false` (default): the running snapshot the daemon
            // is using (the Command-Center / status meaning). `true`: re-read
            // the on-disk `[profile]` so the Agents editor seeds from what it
            // writes (they diverge after a SetProfile that hasn't been applied
            // by a restart). Fall back to the running snapshot when there is no
            // config file or the re-read fails.
            let summary = if from_disk {
                match config_toml_path
                    .and_then(|p| load_settings_config(p, role_override).ok())
                {
                    Some(cfg) => profile_summary_from_profile(&cfg.profile),
                    None => profile_summary_from_profile(profile),
                }
            } else {
                profile_summary_from_profile(profile)
            };
            QueryResponsePayload::GetProfile { profile: summary }
        }
        QueryPayload::GetEffectivePersona => {
            let summary = match shared_persona.read() {
                Ok(state) => effective_persona_summary_from_state(&state),
                Err(_) => {
                    return QueryResponsePayload::QueryError {
                        code: "persona_state_poisoned".into(),
                        message: "shared persona state lock poisoned".into(),
                    };
                }
            };
            QueryResponsePayload::GetEffectivePersona { persona: summary }
        }
        QueryPayload::ListPersonaDeltas { from_seq, limit } => {
            const PERSONA_LIST_MAX_LIMIT: u32 = 500;
            let Some(log) = persona_log else {
                return QueryResponsePayload::ListPersonaDeltas {
                    entries: Vec::new(),
                    total_len: 0,
                };
            };
            let entries = log.entries();
            let total_len = entries.len() as u64;
            let start = from_seq as usize;
            let capped = (limit.min(PERSONA_LIST_MAX_LIMIT)) as usize;
            let end = (start + capped).min(entries.len());
            let page: Vec<crate::daemon_ipc::PersonaDeltaSummary> = if start >= entries.len() {
                Vec::new()
            } else {
                entries[start..end]
                    .iter()
                    .map(persona_delta_summary_from_signed)
                    .collect()
            };
            QueryResponsePayload::ListPersonaDeltas {
                entries: page,
                total_len,
            }
        }
        QueryPayload::ExportPersonaChain => {
            // Phase 64 Task 3 — full-fidelity chain dump for the
            // `aivyx-pa identity export` flow. Single-shot response
            // (no pagination) — capped at MAX_EXPORT_CHAIN_ENTRIES.
            // Realistic chain depth is dozens to low-hundreds of
            // approved deltas; the cap exists to prevent a runaway
            // chain from blowing IPC frame size.
            const MAX_EXPORT_CHAIN_ENTRIES: usize = 100_000;
            let Some(log) = persona_log else {
                // No persona log configured — return an empty
                // chain rather than erroring. The CLI treats this
                // as "nothing to export," which is correct.
                return QueryResponsePayload::ExportPersonaChain {
                    deltas: Vec::new(),
                    effective: crate::persona::EffectivePersona::default(),
                };
            };
            let entries = log.entries();
            if entries.len() > MAX_EXPORT_CHAIN_ENTRIES {
                return QueryResponsePayload::QueryError {
                    code: "persona_chain_too_large".into(),
                    message: format!(
                        "persona chain has {} entries; export caps at {} per response. \
                         Contact aivyx-pa maintainers if you legitimately hit this limit.",
                        entries.len(),
                        MAX_EXPORT_CHAIN_ENTRIES,
                    ),
                };
            }
            let deltas: Vec<crate::identity_export::DeltaExport> = entries
                .iter()
                .map(crate::identity_export::DeltaExport::from)
                .collect();
            let effective = match shared_persona.read() {
                Ok(state) => state.clone(),
                Err(_) => {
                    return QueryResponsePayload::QueryError {
                        code: "persona_state_poisoned".into(),
                        message: "shared persona state lock poisoned".into(),
                    };
                }
            };
            QueryResponsePayload::ExportPersonaChain { deltas, effective }
        }
        // Phase 70 — Persona proposal queries.
        QueryPayload::ListPersonaProposals {
            status_filter,
            limit,
        } => {
            let Some(log) = persona_proposal_log else {
                return QueryResponsePayload::QueryError {
                    code: "no_persona_proposal_log".into(),
                    message: "daemon has no persona proposal log configured".into(),
                };
            };
            let filter = parse_proposal_status_filter(&status_filter);
            let all = log.list(filter);
            let total_len = all.len() as u64;
            let capped = (limit as usize).min(all.len());
            let proposals: Vec<crate::daemon_ipc::PersonaProposalSummary> = all
                .into_iter()
                .take(capped)
                .map(proposal_summary_from_view)
                .collect();
            QueryResponsePayload::ListPersonaProposals {
                proposals,
                total_len,
            }
        }
        QueryPayload::GetPersonaProposal { proposal_id } => {
            let Some(log) = persona_proposal_log else {
                return QueryResponsePayload::QueryError {
                    code: "no_persona_proposal_log".into(),
                    message: "daemon has no persona proposal log configured".into(),
                };
            };
            let proposal = log.get(&proposal_id).map(proposal_summary_from_view);
            QueryResponsePayload::GetPersonaProposal { proposal }
        }
        // Phase 73 — notification history. Walks the audit
        // chain for `AutoNotifyDispatched` events, applies the
        // optional `target_filter`, and renders each match into
        // a `NotificationHistoryEntry`. Pagination matches the
        // existing audit-entry handler's pattern (server-side
        // cap of 500 per page).
        QueryPayload::ListNotificationHistory {
            from_seq,
            limit,
            target_filter,
        } => {
            let Some(log) = audit_log else {
                return QueryResponsePayload::QueryError {
                    code: "no_audit_log".into(),
                    message: "daemon has no audit log configured".into(),
                };
            };
            let chain_len = log.len();
            let entries = match log.entries_range(0, chain_len) {
                Ok(e) => e,
                Err(e) => {
                    return QueryResponsePayload::QueryError {
                        code: "audit_read_failed".into(),
                        message: format!("audit chain read failed: {e}"),
                    };
                }
            };
            let target_str = target_filter.as_deref();
            let matches: Vec<NotificationHistoryEntry> = entries
                .iter()
                .filter_map(|entry| {
                    if let aivyx_audit::AuditEvent::AutoNotifyDispatched {
                        session_id,
                        trigger_kind,
                        trigger_id,
                        target_name,
                        outcome,
                        dispatched_at_unix_ms,
                    } = &entry.event
                    {
                        if let Some(filter) = target_str {
                            if target_name != filter {
                                return None;
                            }
                        }
                        let (outcome_kind, outcome_detail) =
                            render_notify_outcome_for_history(outcome);
                        Some(NotificationHistoryEntry {
                            seq: entry.seq,
                            dispatched_at_unix_ms: *dispatched_at_unix_ms,
                            session_id: session_id.to_string(),
                            trigger_kind: format!("{trigger_kind:?}"),
                            trigger_id: trigger_id.clone(),
                            target_name: target_name.clone(),
                            outcome_kind: outcome_kind.into(),
                            outcome_detail,
                        })
                    } else {
                        None
                    }
                })
                .collect();
            let total_len = matches.len() as u64;
            const HISTORY_QUERY_MAX_LIMIT: u32 = 500;
            let capped = (limit.min(HISTORY_QUERY_MAX_LIMIT)) as usize;
            let page: Vec<NotificationHistoryEntry> = matches
                .into_iter()
                .filter(|e| e.seq >= from_seq)
                .take(capped)
                .collect();
            QueryResponsePayload::ListNotificationHistory {
                entries: page,
                total_len,
            }
        }
        // Phase 74 — memory inspection queries.
        QueryPayload::ListMemoryTopics => {
            let Some(mem) = memory else {
                return QueryResponsePayload::QueryError {
                    code: "no_memory".into(),
                    message: "daemon has no memory substrate configured".into(),
                };
            };
            match mem.list_topics().await {
                // #11 — hide internal/machine topics (the per-session
                // `context:pruned:*` archives) from operator-facing listings:
                // this one IPC backs both `aivyx-pa memory list` and the Studio
                // Memory browser. The entries stay reachable by exact
                // `memory show <topic>`; only the cluttered listing is filtered.
                Ok(topics) => QueryResponsePayload::ListMemoryTopics {
                    topics: topics
                        .into_iter()
                        .filter(|t| !crate::prune_sink::is_internal_topic(t))
                        .collect(),
                },
                Err(e) => QueryResponsePayload::QueryError {
                    code: "memory_list_failed".into(),
                    message: e.to_string(),
                },
            }
        }
        QueryPayload::GetMemoryGraph { limit } => {
            // Chapter MG — topic nodes (with entry counts) + weighted
            // co-occurrence edges. Read-only; edges empty when the ledger isn't
            // armed (→ a topic cloud).
            let Some(mem) = memory else {
                return QueryResponsePayload::QueryError {
                    code: "no_memory".into(),
                    message: "daemon has no memory substrate configured".into(),
                };
            };
            match build_memory_graph(mem.as_ref(), cooccurrence_ledger.map(|v| &**v), limit).await {
                Ok((nodes, edges)) => QueryResponsePayload::GetMemoryGraph { nodes, edges },
                Err(e) => QueryResponsePayload::QueryError {
                    code: "memory_list_failed".into(),
                    message: e,
                },
            }
        }
        QueryPayload::ListWikiPages => {
            // Chapter Codex — compact page rows, most-recent first. An
            // absent store (storage not configured) is an empty codex,
            // not an error.
            let Some(store) = wiki_store else {
                return QueryResponsePayload::ListWikiPages { pages: Vec::new() };
            };
            match store.list_summaries().await {
                // #11 — hide internal `context:pruned:*` pages (machine
                // bookkeeping) from operator listings, incl. any synthesized
                // before the sweep learned to skip them.
                Ok(pages) => QueryResponsePayload::ListWikiPages {
                    pages: pages
                        .into_iter()
                        .filter(|p| !crate::prune_sink::is_internal_topic(&p.topic))
                        .collect(),
                },
                Err(e) => QueryResponsePayload::QueryError {
                    code: "wiki_list_failed".into(),
                    message: e.to_string(),
                },
            }
        }
        QueryPayload::GetWikiPage { topic } => {
            // Chapter Codex — one topic's full page (`None` when it has
            // no page yet, or no store is configured).
            let Some(store) = wiki_store else {
                return QueryResponsePayload::GetWikiPage { page: None };
            };
            match store.get_page(&topic).await {
                Ok(page) => QueryResponsePayload::GetWikiPage { page },
                Err(e) => QueryResponsePayload::QueryError {
                    code: "wiki_get_failed".into(),
                    message: e.to_string(),
                },
            }
        }
        QueryPayload::GetKnowledgeGraph { limit } => {
            // Chapter Lattice — entity nodes + the top-`limit` directed
            // typed edges (by mentions). An absent store is an empty graph,
            // not an error.
            let Some(store) = graph_store else {
                return QueryResponsePayload::GetKnowledgeGraph {
                    entities: Vec::new(),
                    edges: Vec::new(),
                };
            };
            const GRAPH_QUERY_MAX_LIMIT: u32 = 500;
            let cap = limit.clamp(1, GRAPH_QUERY_MAX_LIMIT) as usize;
            match (store.entities().await, store.all_triples().await) {
                (Ok(mut entities), Ok(mut edges)) => {
                    // #B — hide conversation-mechanics noise (chat/messages/
                    // tool bookkeeping) the extractor learned to skip only
                    // recently, so triples synthesized before the fix vanish
                    // from the CLI + Studio without a store migration.
                    use crate::knowledge_graph::{is_mechanical_entity, is_mechanical_predicate};
                    edges.retain(|e| {
                        !is_mechanical_entity(&e.subject)
                            && !is_mechanical_entity(&e.object)
                            && !is_mechanical_predicate(&e.predicate)
                    });
                    entities.retain(|e| !is_mechanical_entity(&e.name));
                    // Strongest relations first; cap the edge set.
                    edges.sort_by(|a, b| {
                        b.mentions
                            .cmp(&a.mentions)
                            .then_with(|| a.subject.cmp(&b.subject))
                            .then_with(|| a.predicate.cmp(&b.predicate))
                            .then_with(|| a.object.cmp(&b.object))
                    });
                    edges.truncate(cap);
                    QueryResponsePayload::GetKnowledgeGraph { entities, edges }
                }
                (Err(e), _) | (_, Err(e)) => QueryResponsePayload::QueryError {
                    code: "graph_read_failed".into(),
                    message: e.to_string(),
                },
            }
        }
        QueryPayload::GetMemoryConflicts => {
            // Chapter Concord — on-demand contradiction detection. Needs
            // both a memory substrate and an LLM; either missing ⇒ an
            // empty set (not an error — nothing to resolve).
            let (Some(mem), Some(llm)) = (memory, contradiction_llm) else {
                return QueryResponsePayload::MemoryConflicts {
                    conflicts: Vec::new(),
                };
            };
            let detector = crate::contradiction::ContradictionDetector::new(
                Arc::clone(&llm.provider),
                llm.model.clone(),
            );
            let conflicts = detector.detect(mem.as_ref()).await;
            // Chapter Concord — drop pairs the operator has dismissed
            // ("keep both") so a false positive isn't re-flagged forever.
            let conflicts = match conflict_dismissals {
                Some(d) => d.retain_undismissed(conflicts).await,
                None => conflicts,
            };
            QueryResponsePayload::MemoryConflicts { conflicts }
        }
        QueryPayload::GetSoulConflicts => {
            // Chapter Accord — on-demand Persona contradiction detection over
            // the current effective persona snapshot. Needs an LLM; missing ⇒
            // an empty set (not an error).
            let Some(llm) = contradiction_llm else {
                return QueryResponsePayload::SoulConflicts {
                    conflicts: Vec::new(),
                };
            };
            let snapshot = match shared_persona.read() {
                Ok(p) => p.clone(),
                Err(_) => {
                    return QueryResponsePayload::SoulConflicts {
                        conflicts: Vec::new(),
                    };
                }
            };
            let detector = crate::soul_contradiction::SoulContradictionDetector::new(
                Arc::clone(&llm.provider),
                llm.model.clone(),
            );
            let conflicts = detector.detect(&snapshot).await;
            // Chapter Accord — drop pairs the operator dismissed ("keep both"),
            // so a false positive from the fuzzy detector isn't re-flagged.
            let conflicts = match conflict_dismissals {
                Some(d) => d.retain_undismissed_soul(conflicts).await,
                None => conflicts,
            };
            QueryResponsePayload::SoulConflicts { conflicts }
        }
        QueryPayload::GetSkills => {
            // Chapter Repertoire — the effective persona's learned skills
            // joined with their WH.2 effectiveness, plus the count of
            // pending skill proposals (governed in the Agents screen).
            let raws: Vec<String> = match shared_persona.read() {
                Ok(p) => p.learned_skills.clone(),
                Err(_) => Vec::new(),
            };
            let now_secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            // Chapter Repertoire — all-time per-skill invocation counts from
            // the audit chain's SkillInvocation entries.
            let mut invoke_counts: std::collections::HashMap<String, u32> =
                std::collections::HashMap::new();
            if let Some(audit) = audit_log {
                if let Ok(entries) = audit.entries() {
                    for e in &entries {
                        if let aivyx_audit::AuditEvent::SkillInvocation { skill_name, .. } =
                            &e.event
                        {
                            *invoke_counts.entry(skill_name.clone()).or_insert(0) += 1;
                        }
                    }
                }
            }
            let mut skills = Vec::new();
            for raw in &raws {
                let Some(skill) = crate::persona::LearnedSkill::from_json_value(raw) else {
                    continue;
                };
                let (ewma_score, samples) = match skill_effectiveness_ledger {
                    Some(ledger) => match ledger.skill_score(&skill.name, now_secs).await {
                        Ok(Some(e)) => (e.ewma_score, e.samples),
                        _ => (0.0, 0),
                    },
                    None => (0.0, 0),
                };
                let invocations = invoke_counts.get(&skill.name).copied().unwrap_or(0);
                skills.push(aivyx_ipc::protocol::SkillView {
                    skill,
                    ewma_score,
                    samples,
                    invocations,
                });
            }
            // Pending LearnedSkill-category proposals (Whetstone refinements
            // + Praxis authored skills) → the "review in Agents" pointer.
            let pending_proposals = persona_proposal_log
                .map(|log| {
                    log.list(crate::persona_proposal::ProposalStatusFilter::Pending)
                        .into_iter()
                        .filter(|p| {
                            p.proposed_op.category
                                == crate::persona::PersonaDeltaCategory::LearnedSkill
                        })
                        .count()
                })
                .unwrap_or(0);
            QueryResponsePayload::GetSkills {
                skills,
                pending_proposals,
            }
        }
        QueryPayload::GetToolCatalog => {
            // Chapter Almanac — a pure registry snapshot. Cheap —
            // `tool_descriptors` is captured once at daemon construction.
            QueryResponsePayload::GetToolCatalog {
                tools: build_tool_catalog(tool_descriptors),
            }
        }
        QueryPayload::GetMemoryTopicEntries { topic, limit } => {
            let Some(mem) = memory else {
                return QueryResponsePayload::QueryError {
                    code: "no_memory".into(),
                    message: "daemon has no memory substrate configured".into(),
                };
            };
            // Server-side cap mirrors the audit-entry handler.
            const MEMORY_QUERY_MAX_LIMIT: u32 = 500;
            let capped = limit.clamp(1, MEMORY_QUERY_MAX_LIMIT) as usize;
            match mem.get_recent(&topic, capped).await {
                Ok(entries) => QueryResponsePayload::GetMemoryTopicEntries {
                    entries: entries.into_iter().map(memory_entry_summary).collect(),
                },
                Err(e) => QueryResponsePayload::QueryError {
                    code: "memory_get_failed".into(),
                    message: e.to_string(),
                },
            }
        }
        QueryPayload::SearchMemory {
            query,
            limit,
            semantic,
        } => {
            let Some(mem) = memory else {
                return QueryResponsePayload::QueryError {
                    code: "no_memory".into(),
                    message: "daemon has no memory substrate configured".into(),
                };
            };
            const MEMORY_QUERY_MAX_LIMIT: u32 = 500;
            let capped = limit.clamp(1, MEMORY_QUERY_MAX_LIMIT) as usize;

            // Phase 75 — semantic path with transparent keyword
            // fallback (Q4a). Fall back when: no `[embedding]`
            // provider, the query embed call fails, or the
            // corpus has zero vectors (semantic over an empty
            // index would just return nothing — keyword is
            // strictly better there). The `fell_back_to_keyword`
            // flag lets the operator/agent see it happened.
            if semantic {
                let qvec = match embedding_provider {
                    Some(p) => match p.embed(std::slice::from_ref(&query)).await {
                        Ok(mut v) if !v.is_empty() => Some(v.remove(0)),
                        _ => None,
                    },
                    None => None,
                };
                let has_vectors = mem
                    .load_all_vectors()
                    .await
                    .map(|v| !v.is_empty())
                    .unwrap_or(false);
                if let (Some(qvec), true) = (qvec, has_vectors) {
                    return match mem.semantic_search(&qvec, capped).await {
                        Ok(matches) => QueryResponsePayload::SearchMemory {
                            matches: matches
                                .into_iter()
                                .filter(|m| !crate::prune_sink::is_internal_topic(&m.topic))
                                .map(memory_entry_summary)
                                .collect(),
                            fell_back_to_keyword: false,
                        },
                        Err(e) => QueryResponsePayload::QueryError {
                            code: "memory_search_failed".into(),
                            message: e.to_string(),
                        },
                    };
                }
                // Fallback to keyword, flagged.
                return match mem.search(&query, capped).await {
                    Ok(matches) => QueryResponsePayload::SearchMemory {
                        matches: matches.into_iter().map(memory_entry_summary).collect(),
                        fell_back_to_keyword: true,
                    },
                    Err(e) => QueryResponsePayload::QueryError {
                        code: "memory_search_failed".into(),
                        message: e.to_string(),
                    },
                };
            }

            // Keyword path (default; no behavior change).
            match mem.search(&query, capped).await {
                Ok(matches) => QueryResponsePayload::SearchMemory {
                    matches: matches
                        .into_iter()
                        .filter(|m| !crate::prune_sink::is_internal_topic(&m.topic))
                        .map(memory_entry_summary)
                        .collect(),
                    fell_back_to_keyword: false,
                },
                Err(e) => QueryResponsePayload::QueryError {
                    code: "memory_search_failed".into(),
                    message: e.to_string(),
                },
            }
        }
        QueryPayload::GetLearningInsights { window_secs } => {
            let window = window_secs.unwrap_or(crate::recall_feedback::RECALL_LOG_RETAIN_SECS);
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let now_secs = now_ms / 1000;

            // Phase 79 (Q4a) — last adaptive-Persona selection.
            // Independent of the recall substrate, so resolved
            // once and included in every LearningInsights return.
            let persona_selection =
                persona_selection_stat.and_then(|s| s.read().ok().and_then(|g| g.clone()));
            // Phase 84 (Q4a) — last-turn cluster-recall stat
            // (same shared-handle pattern as persona_selection).
            let cluster_recall =
                recall_cluster_stat.and_then(|s| s.read().ok().and_then(|g| g.clone()));
            let proactive = proactive_stat.and_then(|s| s.read().ok().and_then(|g| g.clone()));
            let persona_lifecycle =
                persona_lifecycle_stat.and_then(|s| s.read().ok().and_then(|g| g.clone()));
            // Phase 87 (Q4a) — last reflection cycle's
            // pattern-driven consolidation outcome (same
            // shared-handle pattern as persona_selection /
            // cluster_recall).
            let persona_consolidation =
                persona_consolidation_stat.and_then(|s| s.read().ok().and_then(|g| g.clone()));
            // Phase 91 (Q4a) — last reflection cycle's
            // LLM-judged recall outcome.
            let recall_judgment =
                recall_judgment_stat.and_then(|s| s.read().ok().and_then(|g| g.clone()));
            // Phase 82 — durable accumulated helpfulness (the
            // longitudinal view). Best-effort: a ledger error
            // collapses to `None`, never breaking the surface;
            // an empty ledger is reported as "none yet."
            let accumulated_helpfulness = match helpfulness_ledger {
                Some(l) => l
                    .accumulated(now_secs, 5)
                    .await
                    .ok()
                    .filter(|a| !a.top_helpful.is_empty() || !a.top_unhelpful.is_empty()),
                None => None,
            };
            // Phase 83 — durable cross-session co-occurrence
            // patterns. Best-effort: ledger error → None,
            // empty → None (never breaks the surface).
            let cooccurrence = match cooccurrence_ledger {
                Some(l) => l
                    .top_affinities(now_secs, 5)
                    .await
                    .ok()
                    .filter(|p| !p.top_pairs.is_empty()),
                None => None,
            };
            // Phase 172 — durable accumulated correction view
            // (the topics the operator most often reworks).
            // Best-effort: ledger error → None, empty → None.
            let accumulated_corrections = match correction_ledger {
                Some(l) => l
                    .accumulated(now_secs, 5)
                    .await
                    .ok()
                    .filter(|a| !a.top_corrected.is_empty()),
                None => None,
            };
            // Phase 172 (Q4a) — last reflection cycle's
            // correction-driven consolidation outcome.
            let correction_consolidation =
                correction_consolidation_stat.and_then(|s| s.read().ok().and_then(|g| g.clone()));
            // Phase 178 — last cycle's correction-judgment stat.
            let correction_judgment =
                correction_judgment_stat.and_then(|s| s.read().ok().and_then(|g| g.clone()));

            // Phase 95 — snapshot per-schedule cadence stats.
            // Sorted by schedule name for stable rendering.
            let cadence: Vec<(String, crate::reflection_scheduler::RecentReflectionStat)> =
                cadence_stats
                    .read()
                    .ok()
                    .map(|g| {
                        let mut v: Vec<_> = g.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                        v.sort_by(|a, b| a.0.cmp(&b.0));
                        v
                    })
                    .unwrap_or_default();

            // No recall substrate → an empty digest is the
            // valid "nothing learned yet" answer, not an error.
            let Some(rlog) = recall_log else {
                return QueryResponsePayload::LearningInsights {
                    digest: crate::recall_insights::build_digest(
                        window,
                        &crate::recall_feedback::HelpfulnessTally::default(),
                        &[],
                        &[],
                        recall_feedback_config.map(|c| c.use_judgment_signal),
                    ),
                    proposals: Vec::new(),
                    persona_selection,
                    proactive,
                    persona_lifecycle,
                    accumulated_helpfulness,
                    cooccurrence,
                    cluster_recall,
                    persona_consolidation,
                    accumulated_corrections,
                    correction_consolidation,
                    correction_judgment,
                    recall_judgment,
                    cadence,
                };
            };

            let since = now_secs.saturating_sub(window);
            let recalls = match rlog.events_since(since).await {
                Ok(r) => r,
                Err(e) => {
                    return QueryResponsePayload::QueryError {
                        code: "recall_log_read_failed".into(),
                        message: e.to_string(),
                    };
                }
            };
            // Outcomes from the audit chain (same builder the
            // reflection loop uses). No audit log → no scored
            // recalls (digest still reports the raw recall
            // count).
            let outcomes = match audit_log {
                Some(al) => {
                    let n = al.len();
                    match al.entries_range(0, n) {
                        Ok(es) => {
                            crate::reflection_scheduler::summarize_recent_outcomes_from_entries(
                                &es, window, now_ms,
                            )
                        }
                        Err(e) => {
                            return QueryResponsePayload::QueryError {
                                code: "audit_read_failed".into(),
                                message: format!("audit chain read failed: {e}"),
                            };
                        }
                    }
                }
                None => Vec::new(),
            };
            let (tally, detail) = crate::recall_feedback::correlate_detailed(
                &recalls,
                &outcomes,
                recall_feedback_config
                    .map(|c| c.use_judgment_signal)
                    .unwrap_or(false),
            );
            let proposals = persona_proposal_log
                .map(|l| l.list(crate::persona_proposal::ProposalStatusFilter::All))
                .unwrap_or_default();
            QueryResponsePayload::LearningInsights {
                digest: crate::recall_insights::build_digest(
                    window,
                    &tally,
                    &detail,
                    &proposals,
                    recall_feedback_config.map(|c| c.use_judgment_signal),
                ),
                proposals: crate::recall_insights::build_provenance(&detail, &proposals),
                persona_selection,
                proactive,
                persona_lifecycle,
                accumulated_helpfulness,
                cooccurrence,
                cluster_recall,
                persona_consolidation,
                accumulated_corrections,
                correction_consolidation,
                correction_judgment,
                recall_judgment,
                cadence,
            }
        }
        // Chapter U — the Settings screen's read + write handlers. Read-only
        // `GetSettings` re-reads the on-disk config (the source of truth the
        // operator edits); the two writers validate, rewrite the section via
        // the shared `aivyx_config::config_write` helper, audit the change, and
        // re-read so the response carries authoritative state.
        QueryPayload::GetSettings => {
            let path = match config_toml_path {
                Some(p) => p,
                None => return no_config_file_error(),
            };
            match load_settings_config(path, role_override) {
                Ok(cfg) => QueryResponsePayload::GetSettings {
                    settings: settings_snapshot(&cfg, embedding_provider.is_some()),
                },
                Err(e) => QueryResponsePayload::QueryError {
                    code: "config_load_failed".into(),
                    message: e,
                },
            }
        }
        QueryPayload::SetAccessLevel {
            level,
            root,
            confirm,
        } => {
            let path = match config_toml_path {
                Some(p) => p,
                None => return no_config_file_error(),
            };
            let lvl = match aivyx_config::AccessLevel::from_wire(&level) {
                Some(l) => l,
                None => {
                    return QueryResponsePayload::QueryError {
                        code: "invalid_level".into(),
                        message: format!(
                            "unknown access level `{level}` \
                             (sandbox | workspace | home | full | custom)"
                        ),
                    };
                }
            };
            // Confirm-first gate (Chapter N) — enforced SERVER-SIDE, not just
            // in the UI. Any expanded (non-sandbox) level needs confirm = true.
            if lvl.is_expanded() && !confirm {
                return QueryResponsePayload::QueryError {
                    code: "confirm_required".into(),
                    message: format!(
                        "granting `{}` access reaches beyond the sandbox; \
                         resend with confirm = true",
                        lvl.as_str()
                    ),
                };
            }
            match aivyx_config::write_access_section(path, lvl, root.as_deref()) {
                Ok(()) => {
                    let summary = match root.as_deref() {
                        Some(r) => format!("access level = {} (root = {r})", lvl.as_str()),
                        None => format!("access level = {}", lvl.as_str()),
                    };
                    audit_config_change(audit_log, "access", &summary);
                    settings_applied(path, embedding_provider.is_some(), role_override)
                }
                Err(e) => map_config_write_error(e),
            }
        }
        QueryPayload::SetMcpServer {
            name,
            transport,
            command,
            args,
            env,
            headers,
            url,
            enabled,
        } => {
            let path = match config_toml_path {
                Some(p) => p,
                None => return no_config_file_error(),
            };
            let entry = aivyx_config::config_write::McpServerEntryWrite {
                name: name.clone(),
                transport,
                command,
                args,
                env,
                headers,
                url,
                enabled,
            };
            match aivyx_config::config_write::write_mcp_server_section(path, &entry) {
                Ok(()) => {
                    audit_config_change(audit_log, "mcp_server", &format!("set {name}"));
                    mcp_servers_applied_after_write(path, "set")
                }
                Err(e) => map_config_write_error(e),
            }
        }
        QueryPayload::DeleteMcpServer { name } => {
            let path = match config_toml_path {
                Some(p) => p,
                None => return no_config_file_error(),
            };
            match aivyx_config::config_write::remove_mcp_server_section(path, &name) {
                Ok(()) => {
                    audit_config_change(audit_log, "mcp_server", &format!("delete {name}"));
                    mcp_servers_applied_after_write(path, "delete")
                }
                Err(e) => map_config_write_error(e),
            }
        }
        QueryPayload::SetNotifyTarget {
            name,
            kind,
            chat_id,
            url,
            to,
            enabled,
            is_default,
            retry_count,
            retry_backoff_ms_start,
            rate_limit_max,
            rate_limit_window_secs,
        } => {
            let path = match config_toml_path {
                Some(p) => p,
                None => return no_config_file_error(),
            };
            let entry = aivyx_config::config_write::NotifyTargetEntryWrite {
                name: name.clone(),
                kind,
                chat_id,
                url,
                to,
                enabled,
                is_default,
                retry_count,
                retry_backoff_ms_start,
                rate_limit_max,
                rate_limit_window_secs,
            };
            match aivyx_config::config_write::write_notify_target_section(path, &entry) {
                Ok(()) => {
                    audit_config_change(audit_log, "notify_target", &format!("set {name}"));
                    notify_targets_applied_after_write(path, "set")
                }
                Err(e) => map_config_write_error(e),
            }
        }
        QueryPayload::DeleteNotifyTarget { name } => {
            let path = match config_toml_path {
                Some(p) => p,
                None => return no_config_file_error(),
            };
            match aivyx_config::config_write::remove_notify_target_section(path, &name) {
                Ok(()) => {
                    audit_config_change(audit_log, "notify_target", &format!("delete {name}"));
                    notify_targets_applied_after_write(path, "delete")
                }
                Err(e) => map_config_write_error(e),
            }
        }
        QueryPayload::GetReflectionScheduleConfigs => {
            let path = match config_toml_path {
                Some(p) => p,
                None => return no_config_file_error(),
            };
            match read_reflection_schedule_configs(path) {
                Ok(schedules) => QueryResponsePayload::GetReflectionScheduleConfigs { schedules },
                Err(e) => QueryResponsePayload::QueryError {
                    code: "config_reload_failed".into(),
                    message: format!("failed to read reflection schedule configs: {e}"),
                },
            }
        }
        QueryPayload::SetReflectionSchedule { name, cron, lookback_window_secs, enabled } => {
            let path = match config_toml_path {
                Some(p) => p,
                None => return no_config_file_error(),
            };
            let entry = aivyx_config::config_write::ReflectionScheduleEntryWrite {
                name: name.clone(),
                cron,
                lookback_window_secs,
                enabled,
            };
            match aivyx_config::config_write::write_reflection_schedule_section(path, &entry) {
                Ok(()) => {
                    audit_config_change(audit_log, "reflection_schedule", &format!("set {name}"));
                    reflection_schedules_applied_after_write(path, "set")
                }
                Err(e) => map_config_write_error(e),
            }
        }
        QueryPayload::DeleteReflectionSchedule { name } => {
            let path = match config_toml_path {
                Some(p) => p,
                None => return no_config_file_error(),
            };
            match aivyx_config::config_write::remove_reflection_schedule_section(path, &name) {
                Ok(()) => {
                    audit_config_change(audit_log, "reflection_schedule", &format!("delete {name}"));
                    reflection_schedules_applied_after_write(path, "delete")
                }
                Err(e) => map_config_write_error(e),
            }
        }
        QueryPayload::GetMemoryProfileConfig => {
            let path = match config_toml_path {
                Some(p) => p,
                None => return no_config_file_error(),
            };
            match aivyx_config::config_write::read_memory_profile(path) {
                Ok(profile) => QueryResponsePayload::GetMemoryProfileConfig {
                    config: aivyx_ipc::protocol::MemoryProfileConfigView {
                        profile: profile.unwrap_or_else(|| "off".to_string()),
                    },
                },
                Err(e) => map_config_write_error(e),
            }
        }
        QueryPayload::SetMemoryProfile { profile } => {
            let path = match config_toml_path {
                Some(p) => p,
                None => return no_config_file_error(),
            };
            match aivyx_config::config_write::write_memory_profile(path, Some(&profile)) {
                Ok(()) => {
                    audit_config_change(audit_log, "memory", &format!("profile = {profile}"));
                    match aivyx_config::config_write::read_memory_profile(path) {
                        Ok(p) => QueryResponsePayload::MemoryProfileConfigApplied {
                            config: aivyx_ipc::protocol::MemoryProfileConfigView {
                                profile: p.unwrap_or_else(|| "off".to_string()),
                            },
                            restart_required: true,
                        },
                        Err(e) => QueryResponsePayload::QueryError {
                            code: "config_reload_failed".into(),
                            message: format!("memory profile saved, but reloading it failed: {e}"),
                        },
                    }
                }
                Err(e) => map_config_write_error(e),
            }
        }
        QueryPayload::GetEmbeddingConfig => {
            let path = match config_toml_path {
                Some(p) => p,
                None => return no_config_file_error(),
            };
            match aivyx_config::config_write::read_embedding_section(path) {
                Ok(e) => QueryResponsePayload::GetEmbeddingConfig { config: embedding_config_view(&e) },
                Err(err) => map_config_write_error(err),
            }
        }
        QueryPayload::SetEmbeddingConfig { base_url, model, api_key } => {
            let path = match config_toml_path {
                Some(p) => p,
                None => return no_config_file_error(),
            };
            let entry = aivyx_config::config_write::EmbeddingEntryWrite { base_url, model, api_key };
            match aivyx_config::config_write::write_embedding_section(path, &entry) {
                Ok(()) => {
                    audit_config_change(audit_log, "embedding", "updated");
                    match aivyx_config::config_write::read_embedding_section(path) {
                        Ok(e) => QueryResponsePayload::EmbeddingConfigApplied {
                            config: embedding_config_view(&e),
                            restart_required: true,
                        },
                        Err(err) => QueryResponsePayload::QueryError {
                            code: "config_reload_failed".into(),
                            message: format!("embedding config saved, but reloading it failed: {err}"),
                        },
                    }
                }
                Err(e) => map_config_write_error(e),
            }
        }
        QueryPayload::GetProactiveConfig => {
            let path = match config_toml_path {
                Some(p) => p,
                None => return no_config_file_error(),
            };
            match aivyx_config::config_write::read_proactive_section(path) {
                Ok(p) => QueryResponsePayload::GetProactiveConfig { config: proactive_config_view(&p) },
                Err(err) => map_config_write_error(err),
            }
        }
        QueryPayload::SetProactiveConfig { enabled, target, max_per_window, window_secs } => {
            let path = match config_toml_path {
                Some(p) => p,
                None => return no_config_file_error(),
            };
            let entry =
                aivyx_config::config_write::ProactiveEntryWrite { enabled, target, max_per_window, window_secs };
            match aivyx_config::config_write::write_proactive_section(path, &entry) {
                Ok(()) => {
                    audit_config_change(audit_log, "proactive", "updated");
                    match aivyx_config::config_write::read_proactive_section(path) {
                        Ok(p) => QueryResponsePayload::ProactiveConfigApplied {
                            config: proactive_config_view(&p),
                            restart_required: true,
                        },
                        Err(err) => QueryResponsePayload::QueryError {
                            code: "config_reload_failed".into(),
                            message: format!("proactive config saved, but reloading it failed: {err}"),
                        },
                    }
                }
                Err(e) => map_config_write_error(e),
            }
        }
        QueryPayload::GetEmailConfig => {
            let path = match config_toml_path {
                Some(p) => p,
                None => return no_config_file_error(),
            };
            match aivyx_config::config_write::read_email_section(path) {
                Ok(e) => QueryResponsePayload::GetEmailConfig { config: email_config_view(&e) },
                Err(err) => map_config_write_error(err),
            }
        }
        QueryPayload::SetEmailConfig { host, port, tls_mode, username, password, from } => {
            let path = match config_toml_path {
                Some(p) => p,
                None => return no_config_file_error(),
            };
            let entry = aivyx_config::config_write::EmailEntryWrite { host, port, tls_mode, username, password, from };
            match aivyx_config::config_write::write_email_section(path, &entry) {
                Ok(()) => {
                    audit_config_change(audit_log, "email", "updated");
                    match aivyx_config::config_write::read_email_section(path) {
                        Ok(e) => QueryResponsePayload::EmailConfigApplied {
                            config: email_config_view(&e),
                            restart_required: true,
                        },
                        Err(err) => QueryResponsePayload::QueryError {
                            code: "config_reload_failed".into(),
                            message: format!("email config saved, but reloading it failed: {err}"),
                        },
                    }
                }
                Err(e) => map_config_write_error(e),
            }
        }
        QueryPayload::GetTelegramConfig => {
            let path = match config_toml_path {
                Some(p) => p,
                None => return no_config_file_error(),
            };
            match aivyx_config::config_write::read_telegram_section(path) {
                Ok(t) => QueryResponsePayload::GetTelegramConfig { config: telegram_config_view(&t) },
                Err(err) => map_config_write_error(err),
            }
        }
        QueryPayload::SetTelegramConfig { token, chat_id, team_run_channel, team_trigger_rate_limit, team_command_allowed_senders } => {
            let path = match config_toml_path {
                Some(p) => p,
                None => return no_config_file_error(),
            };
            let entry = aivyx_config::config_write::TelegramEntryWrite {
                token, chat_id, team_run_channel, team_trigger_rate_limit, team_command_allowed_senders,
            };
            match aivyx_config::config_write::write_telegram_section(path, &entry) {
                Ok(()) => {
                    audit_config_change(audit_log, "telegram", "updated");
                    match aivyx_config::config_write::read_telegram_section(path) {
                        Ok(t) => QueryResponsePayload::TelegramConfigApplied {
                            config: telegram_config_view(&t),
                            restart_required: true,
                        },
                        Err(err) => QueryResponsePayload::QueryError {
                            code: "config_reload_failed".into(),
                            message: format!("telegram config saved, but reloading it failed: {err}"),
                        },
                    }
                }
                Err(e) => map_config_write_error(e),
            }
        }
        QueryPayload::GetDiscordConfig => {
            let path = match config_toml_path {
                Some(p) => p,
                None => return no_config_file_error(),
            };
            match aivyx_config::config_write::read_discord_section(path) {
                Ok(d) => QueryResponsePayload::GetDiscordConfig { config: discord_config_view(&d) },
                Err(err) => map_config_write_error(err),
            }
        }
        QueryPayload::SetDiscordConfig { token, application_id, team_run_channel, team_trigger_rate_limit, team_command_allowed_senders } => {
            let path = match config_toml_path {
                Some(p) => p,
                None => return no_config_file_error(),
            };
            let entry = aivyx_config::config_write::DiscordEntryWrite {
                token, application_id, team_run_channel, team_trigger_rate_limit, team_command_allowed_senders,
            };
            match aivyx_config::config_write::write_discord_section(path, &entry) {
                Ok(()) => {
                    audit_config_change(audit_log, "discord", "updated");
                    match aivyx_config::config_write::read_discord_section(path) {
                        Ok(d) => QueryResponsePayload::DiscordConfigApplied {
                            config: discord_config_view(&d),
                            restart_required: true,
                        },
                        Err(err) => QueryResponsePayload::QueryError {
                            code: "config_reload_failed".into(),
                            message: format!("discord config saved, but reloading it failed: {err}"),
                        },
                    }
                }
                Err(e) => map_config_write_error(e),
            }
        }
        QueryPayload::GetSlackConfig => {
            let path = match config_toml_path {
                Some(p) => p,
                None => return no_config_file_error(),
            };
            match aivyx_config::config_write::read_slack_section(path) {
                Ok(s) => QueryResponsePayload::GetSlackConfig { config: slack_config_view(&s) },
                Err(err) => map_config_write_error(err),
            }
        }
        QueryPayload::SetSlackConfig { bot_token, app_token, team_id, team_run_channel, team_trigger_rate_limit, team_command_allowed_senders } => {
            let path = match config_toml_path {
                Some(p) => p,
                None => return no_config_file_error(),
            };
            let entry = aivyx_config::config_write::SlackEntryWrite {
                bot_token, app_token, team_id, team_run_channel, team_trigger_rate_limit, team_command_allowed_senders,
            };
            match aivyx_config::config_write::write_slack_section(path, &entry) {
                Ok(()) => {
                    audit_config_change(audit_log, "slack", "updated");
                    match aivyx_config::config_write::read_slack_section(path) {
                        Ok(s) => QueryResponsePayload::SlackConfigApplied {
                            config: slack_config_view(&s),
                            restart_required: true,
                        },
                        Err(err) => QueryResponsePayload::QueryError {
                            code: "config_reload_failed".into(),
                            message: format!("slack config saved, but reloading it failed: {err}"),
                        },
                    }
                }
                Err(e) => map_config_write_error(e),
            }
        }
        QueryPayload::TestMcpServerConnection {
            transport,
            command,
            args,
            env,
            headers,
            url,
        } => {
            // Final-review fix #4 — nothing in `aivyx-mcp` times out the
            // connect/handshake calls on its own, and this handler is
            // awaited inline in the per-connection frame loop: a stdio
            // command that spawns but never speaks JSON-RPC, or an
            // HTTP/SSE URL that accepts the TCP connection but never
            // responds, would otherwise hang this entire Studio WebSocket
            // connection forever. `tokio::time::timeout` drops (cancels)
            // the inner future — and everything it owns (the child
            // process, the socket) — the instant it fires, so no explicit
            // cleanup path is needed on the timeout arm below.
            let probe = async move {
                let args_ref: Vec<&str> = args.iter().map(String::as_str).collect();
                let bridge_result: Result<aivyx_mcp::McpServerBridge, String> =
                    match transport.as_str() {
                        "stdio" => {
                            let Some(cmd) = command.as_deref() else {
                                return Err("stdio transport requires `command`".to_string());
                            };
                            aivyx_mcp::McpServerBridge::start_with_sandbox(
                                cmd, &args_ref, &env, None, None, "test-connection",
                            )
                            .await
                        }
                        "sse" | "http" | "streamable-http" => {
                            let Some(u) = url.as_deref() else {
                                return Err("sse/http transport requires `url`".to_string());
                            };
                            let transport_result = if transport == "sse" {
                                aivyx_mcp::SseTransport::connect(u, &headers).await.map(|t| {
                                    std::sync::Arc::new(t) as std::sync::Arc<dyn aivyx_mcp::McpTransport>
                                })
                            } else {
                                aivyx_mcp::StreamableHttpTransport::connect(u, &headers).await.map(
                                    |t| std::sync::Arc::new(t) as std::sync::Arc<dyn aivyx_mcp::McpTransport>,
                                )
                            };
                            match transport_result {
                                Ok(t) => {
                                    aivyx_mcp::McpServerBridge::from_transport(t, "test-connection").await
                                }
                                Err(e) => Err(e),
                            }
                        }
                        other => {
                            return Err(format!("unknown transport {other:?}"));
                        }
                    };
                match bridge_result {
                    Ok(bridge) => {
                        let tool_count = bridge.list_tools().await.map(|t| t.len()).unwrap_or(0);
                        let _ = bridge.shutdown().await;
                        Ok(tool_count)
                    }
                    Err(e) => Err(e),
                }
            };
            match tokio::time::timeout(std::time::Duration::from_secs(15), probe).await {
                Ok(Ok(tool_count)) => QueryResponsePayload::McpServerTestResult {
                    ok: true,
                    tool_count,
                    error: None,
                },
                Ok(Err(e)) => QueryResponsePayload::McpServerTestResult {
                    ok: false,
                    tool_count: 0,
                    error: Some(e),
                },
                Err(_elapsed) => QueryResponsePayload::McpServerTestResult {
                    ok: false,
                    tool_count: 0,
                    error: Some("connection timed out after 15s".to_string()),
                },
            }
        }
        QueryPayload::SetBudget {
            per_run_usd,
            per_day_usd,
            on_exceeded,
            alert_at,
        } => {
            let path = match config_toml_path {
                Some(p) => p,
                None => return no_config_file_error(),
            };
            let action = match on_exceeded.as_deref() {
                None | Some("deny") => aivyx_cost::BudgetAction::Deny,
                Some("alert") => aivyx_cost::BudgetAction::Alert,
                Some(other) => {
                    return QueryResponsePayload::QueryError {
                        code: "invalid_budget".into(),
                        message: format!("unknown on_exceeded `{other}` (expected alert | deny)"),
                    };
                }
            };
            // Chapter Ballast — per-mission caps are not edited from this
            // Studio budget screen; `write_budget_section` only rewrites the
            // run/day/on_exceeded/alert keys, so any `per_mission_*` already in
            // the file survives. None here is therefore non-destructive.
            let budget = aivyx_cost::BudgetConfig {
                per_run_usd,
                per_day_usd,
                per_mission_usd: None,
                per_mission_tokens: None,
                on_exceeded: action,
                alert_at,
            };
            match aivyx_config::write_budget_section(path, &budget) {
                Ok(()) => {
                    let summary = format!(
                        "per_run_usd = {}, per_day_usd = {}, on_exceeded = {}, alert_at = {}",
                        opt_usd(per_run_usd),
                        opt_usd(per_day_usd),
                        budget_action_label(action),
                        opt_frac(alert_at),
                    );
                    audit_config_change(audit_log, "budget", &summary);
                    settings_applied(path, embedding_provider.is_some(), role_override)
                }
                Err(e) => map_config_write_error(e),
            }
        }
        QueryPayload::SetCycleDetection { enabled } => {
            let path = match config_toml_path {
                Some(p) => p,
                None => return no_config_file_error(),
            };
            match aivyx_config::write_agent_cycle_detection(path, enabled) {
                Ok(()) => {
                    audit_config_change(
                        audit_log,
                        "agent",
                        &format!("cycle_detection = {enabled}"),
                    );
                    settings_applied(path, embedding_provider.is_some(), role_override)
                }
                Err(e) => map_config_write_error(e),
            }
        }
        QueryPayload::SetAutonomyLevel { level, confirm } => {
            let path = match config_toml_path {
                Some(p) => p,
                None => return no_config_file_error(),
            };
            let lvl = match aivyx_config::AutonomyLevel::from_wire(&level) {
                Some(l) => l,
                None => {
                    return QueryResponsePayload::QueryError {
                        code: "invalid_level".into(),
                        message: format!(
                            "unknown autonomy level `{level}` (manual | assisted | \
                             supervised | autonomous | unleashed)"
                        ),
                    };
                }
            };
            // Confirm-first gate — enforced SERVER-SIDE, not just in the UI. The
            // autonomy-granting levels (`autonomous` / `unleashed`) let the agent
            // act unattended, so they require an explicit confirm.
            let grants_autonomy = matches!(
                lvl,
                aivyx_config::AutonomyLevel::Autonomous | aivyx_config::AutonomyLevel::Unleashed
            );
            if grants_autonomy && !confirm {
                return QueryResponsePayload::QueryError {
                    code: "confirm_required".into(),
                    message: format!(
                        "autonomy level `{}` lets the agent act unattended; \
                         resend with confirm = true",
                        lvl.as_str()
                    ),
                };
            }
            match aivyx_config::write_autonomy_section(path, lvl) {
                Ok(()) => {
                    audit_config_change(
                        audit_log,
                        "autonomy",
                        &format!("level = {}", lvl.as_str()),
                    );
                    settings_applied(path, embedding_provider.is_some(), role_override)
                }
                Err(e) => map_config_write_error(e),
            }
        }
        QueryPayload::SetProfile {
            assistant_name,
            operator_profile,
            communication_style,
            primary_use_cases,
            behavioral_preferences,
            behavioral_constraints,
        } => {
            let path = match config_toml_path {
                Some(p) => p,
                None => return no_config_file_error(),
            };
            let write = aivyx_config::ProfileWrite {
                assistant_name,
                operator_profile,
                communication_style,
                primary_use_cases,
                behavioral_preferences,
                behavioral_constraints,
            };
            match aivyx_config::write_profile_section(path, &write) {
                Ok(()) => {
                    audit_config_change(audit_log, "profile", &profile_change_summary(&write));
                    profile_applied(path, role_override)
                }
                Err(e) => map_config_write_error(e),
            }
        }
        QueryPayload::GetVoiceSettings => {
            let path = match config_toml_path {
                Some(p) => p,
                None => return no_config_file_error(),
            };
            match load_settings_config(path, role_override) {
                Ok(cfg) => QueryResponsePayload::GetVoiceSettings {
                    settings: voice_snapshot(&cfg),
                },
                Err(e) => QueryResponsePayload::QueryError {
                    code: "config_load_failed".into(),
                    message: e,
                },
            }
        }
        QueryPayload::SetVoice {
            asr_engine,
            tts_engine,
            asr_model_path,
            asr_language,
            asr_beam_size,
            tts_model_dir,
            tts_voice_name,
            tts_speed,
            input_device,
            output_device,
        } => {
            let path = match config_toml_path {
                Some(p) => p,
                None => return no_config_file_error(),
            };
            let write = aivyx_config::VoiceWrite {
                asr_engine,
                tts_engine,
                asr_model_path,
                asr_language,
                asr_beam_size,
                tts_model_dir,
                tts_voice_name,
                tts_speed,
                input_device,
                output_device,
            };
            match aivyx_config::write_voice_section(path, &write) {
                Ok(()) => {
                    audit_config_change(audit_log, "voice", &voice_change_summary(&write));
                    voice_applied(path, role_override)
                }
                Err(e) => map_config_write_error(e),
            }
        }
        QueryPayload::SetTeamRoster { roster } => {
            // Chapter Roster (RO.2) — persist the operator-authored team. The
            // target is pre-resolved by the binary (`[team] config_path` or the
            // conventional `team.toml`); an env-only launch has none → refuse.
            let path = match team_config_write_path {
                Some(p) => p,
                None => return no_config_file_error(),
            };
            // validate → to_toml → 0600. Validation runs first, so an invalid
            // roster is rejected with the validator's message and never written.
            match crate::team_config_write::write_team_config(path, &roster) {
                Ok(()) => {
                    audit_config_change(audit_log, "team", &team_roster_summary(&roster));
                    team_roster_applied(path)
                }
                Err(crate::team_config_write::TeamConfigWriteError::Invalid(message)) => {
                    QueryResponsePayload::QueryError {
                        code: "invalid_roster".into(),
                        message,
                    }
                }
                Err(crate::team_config_write::TeamConfigWriteError::Write(message)) => {
                    QueryResponsePayload::QueryError {
                        code: "team_write_failed".into(),
                        message,
                    }
                }
            }
        }
        QueryPayload::GetGallery => match comfyui_base_url {
            None => QueryResponsePayload::Gallery {
                available: false,
                images: Vec::new(),
            },
            Some(base_url) => fetch_gallery(base_url).await,
        },
    }
}

/// Studio Gallery — read ComfyUI's own `/history` API directly (not the MCP
/// tool surface — see [[comfyui-mcp-integration]]) and shape it into
/// `GalleryImage`s, newest first. Best-effort throughout: a request/parse
/// failure yields an empty (but `available: true`) list rather than a
/// `QueryError` — a ComfyUI hiccup shouldn't break the Studio screen.
async fn fetch_gallery(base_url: &str) -> QueryResponsePayload {
    const GALLERY_MAX_IMAGES: usize = 40;

    let empty = || QueryResponsePayload::Gallery {
        available: true,
        images: Vec::new(),
    };

    let url = format!("{}/history", base_url.trim_end_matches('/'));
    let Ok(resp) = reqwest::get(&url).await else {
        return empty();
    };
    let Ok(body) = resp.text().await else {
        return empty();
    };
    let Ok(history) = serde_json::from_str::<serde_json::Value>(&body) else {
        return empty();
    };
    let Some(entries) = history.as_object() else {
        return empty();
    };

    let mut images: Vec<GalleryImage> = entries
        .iter()
        .filter_map(|(prompt_id, entry)| gallery_image_from_history_entry(prompt_id, entry))
        .collect();
    images.sort_by_key(|img| std::cmp::Reverse(img.created_unix));
    images.truncate(GALLERY_MAX_IMAGES);

    QueryResponsePayload::Gallery {
        available: true,
        images,
    }
}

/// One `/history/{prompt_id}` entry → a `GalleryImage`, or `None` if it has
/// no image output (e.g. an audio/failed generation).
fn gallery_image_from_history_entry(
    prompt_id: &str,
    entry: &serde_json::Value,
) -> Option<GalleryImage> {
    let outputs = entry.get("outputs")?.as_object()?;
    let image = outputs
        .values()
        .find_map(|out| out.get("images")?.as_array()?.first())?;
    let filename = image.get("filename")?.as_str()?.to_string();
    let subfolder = image
        .get("subfolder")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let folder_type = image
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("output")
        .to_string();

    // Prefer the completion timestamp; fall back to the start timestamp for
    // an entry with no `execution_success` message (e.g. a failed run).
    let created_unix = entry
        .get("status")?
        .get("messages")?
        .as_array()?
        .iter()
        .rev()
        .find_map(|msg| {
            let arr = msg.as_array()?;
            match arr.first()?.as_str()? {
                "execution_success" | "execution_start" => arr.get(1)?.get("timestamp")?.as_u64(),
                _ => None,
            }
        })
        .map(|ms| ms / 1000);

    Some(GalleryImage {
        prompt_id: prompt_id.to_string(),
        filename,
        subfolder,
        folder_type,
        created_unix,
        caption: gallery_caption(entry),
    })
}

/// Best-effort caption: the text feeding the `KSampler` node's `positive`
/// input, traced through the submitted node graph. `None` when the graph
/// doesn't have that shape (a custom/non-standard workflow).
fn gallery_caption(entry: &serde_json::Value) -> Option<String> {
    let nodes = entry.get("prompt")?.get(2)?.as_object()?;
    let ksampler = nodes
        .values()
        .find(|n| n.get("class_type").and_then(|c| c.as_str()) == Some("KSampler"))?;
    let positive_link = ksampler.get("inputs")?.get("positive")?.as_array()?;
    let node_id = positive_link.first()?.as_str()?;
    nodes
        .get(node_id)?
        .get("inputs")?
        .get("text")?
        .as_str()
        .map(str::to_string)
}

/// Chapter U — `QueryError` for a Settings write/read when the daemon was
/// launched without an `aivyx-pa.toml` (env-only). The handler refuses rather
/// than fabricate a config path.
fn no_config_file_error() -> QueryResponsePayload {
    QueryResponsePayload::QueryError {
        code: "no_config_file".into(),
        message: "the daemon was launched without an aivyx-pa.toml; settings are \
                  not editable from here"
            .into(),
    }
}

/// Chapter U — load the on-disk config for the Settings snapshot. Inspection
/// posture (no required secrets), same as `aivyx-pa access show`.
fn load_settings_config(
    toml_path: &Path,
    role_override: Option<&str>,
) -> Result<aivyx_config::AivyxConfig, String> {
    let opts = aivyx_config::LoadOptions {
        toml_path: Some(toml_path.to_path_buf()),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: role_override.map(str::to_string),
    };
    aivyx_config::AivyxConfig::load_from_env_and_toml(&opts)
        .map_err(|e| format!("failed to load {}: {e}", toml_path.display()))
}

/// Chapter U — re-read the config from disk and return a `SettingsApplied`
/// response. `restart_required` is always `true`: config is load-time, so a
/// write updates the file but never the running daemon.
fn settings_applied(
    toml_path: &Path,
    embeddings_available: bool,
    role_override: Option<&str>,
) -> QueryResponsePayload {
    match load_settings_config(toml_path, role_override) {
        Ok(cfg) => QueryResponsePayload::SettingsApplied {
            settings: settings_snapshot(&cfg, embeddings_available),
            restart_required: true,
        },
        // The write succeeded but the re-read failed — surface it rather than
        // claim a clean apply.
        Err(e) => QueryResponsePayload::QueryError {
            code: "config_reload_failed".into(),
            message: format!("settings written, but reloading them failed: {e}"),
        },
    }
}

/// Chapter Roster (RO.2) — re-read the written team file and return a
/// `TeamRosterApplied` response. `restart_required` is always `true`: the team
/// service is assembled at boot, so a write updates the file but not the
/// running daemon (mirrors `settings_applied`).
fn team_roster_applied(team_path: &Path) -> QueryResponsePayload {
    match aivyx_team::TeamConfig::load(team_path) {
        Ok(roster) => QueryResponsePayload::TeamRosterApplied {
            roster,
            restart_required: true,
        },
        Err(e) => QueryResponsePayload::QueryError {
            code: "config_reload_failed".into(),
            message: format!("team roster written, but reloading it failed: {e}"),
        },
    }
}

/// Chapter Roster — a compact, forensic-friendly summary of a persisted roster
/// for the `ConfigChanged` audit entry (the shape — team name / lead /
/// specialist count — not the members' souls).
fn team_roster_summary(roster: &aivyx_team::TeamConfig) -> String {
    format!(
        "team = {:?}, lead = {:?}, {} specialist(s)",
        roster.name,
        roster.lead,
        roster.specialists().count(),
    )
}

/// Chapter V — re-read the config from disk and return a `ProfileApplied`
/// response. `restart_required` is always `true`: Profile shapes the system
/// prompt at load time, so a write updates `aivyx-pa.toml` but not the running
/// daemon.
fn profile_applied(toml_path: &Path, role_override: Option<&str>) -> QueryResponsePayload {
    match load_settings_config(toml_path, role_override) {
        Ok(cfg) => QueryResponsePayload::ProfileApplied {
            profile: profile_summary_from_profile(&cfg.profile),
            restart_required: true,
        },
        Err(e) => QueryResponsePayload::QueryError {
            code: "config_reload_failed".into(),
            message: format!("profile written, but reloading it failed: {e}"),
        },
    }
}

/// Chapter V — a compact, forensic-friendly summary of which Profile fields a
/// `SetProfile` write set vs. cleared, for the `ConfigChanged` audit entry.
/// Records the *shape* of the change (set/cleared, list lengths), never the
/// declared values themselves — the audit chain is not the place for the
/// operator's profile prose.
fn profile_change_summary(w: &aivyx_config::ProfileWrite) -> String {
    fn scalar(label: &str, v: &Option<String>) -> String {
        match v.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            Some(_) => format!("{label} = set"),
            None => format!("{label} = cleared"),
        }
    }
    fn list(label: &str, v: &Option<Vec<String>>) -> String {
        match v {
            Some(items) => {
                let n = items.iter().filter(|s| !s.trim().is_empty()).count();
                format!("{label} = {n}")
            }
            None => format!("{label} = cleared"),
        }
    }
    [
        scalar("assistant_name", &w.assistant_name),
        scalar("operator_profile", &w.operator_profile),
        scalar("communication_style", &w.communication_style),
        list("primary_use_cases", &w.primary_use_cases),
        list("behavioral_preferences", &w.behavioral_preferences),
        list("behavioral_constraints", &w.behavioral_constraints),
    ]
    .join(", ")
}

/// Chapter Voice — build the `[voice]` wire snapshot (options + readiness) from
/// a loaded config. Readiness is a `stat`: the Whisper model is a **file**, and
/// the Kokoro TTS prereqs are a `*.onnx` and a `voices-*.bin` inside the model
/// **directory** (Chapter Timbre).
fn voice_snapshot(cfg: &aivyx_config::AivyxConfig) -> aivyx_ipc::protocol::VoiceSettingsSnapshot {
    let v = &cfg.voice_options;
    let as_str = |p: &Option<std::path::PathBuf>| p.as_ref().map(|x| x.display().to_string());
    aivyx_ipc::protocol::VoiceSettingsSnapshot {
        asr_engine: v.asr_engine.clone(),
        tts_engine: v.tts_engine.clone(),
        asr_model_path: as_str(&v.asr_model_path),
        asr_language: v.asr_language.clone(),
        asr_beam_size: v.asr_beam_size.map(|n| n as u32),
        tts_model_dir: as_str(&v.tts_model_dir),
        tts_voice_name: v.tts_voice_name.clone(),
        tts_speed: v.tts_speed,
        input_device: v.input_device.clone(),
        output_device: v.output_device.clone(),
        asr_model_status: path_status(v.asr_model_path.as_deref(), false),
        tts_model_status: model_dir_status(v.tts_model_dir.as_deref(), |n| n.ends_with(".onnx")),
        tts_voices_status: model_dir_status(v.tts_model_dir.as_deref(), |n| {
            n.starts_with("voices") && n.ends_with(".bin")
        }),
    }
}

/// Chapter Timbre — readiness for a file *inside* the Kokoro model directory:
/// `"unset"` (no dir configured), `"present"` (the dir holds an entry matching
/// `pred`), or `"missing"` (no dir, or no matching entry).
fn model_dir_status(dir: Option<&Path>, pred: impl Fn(&str) -> bool) -> String {
    let Some(dir) = dir else {
        return "unset".to_string();
    };
    let found = std::fs::read_dir(dir).is_ok_and(|rd| {
        rd.flatten()
            .any(|e| e.file_name().to_str().is_some_and(&pred))
    });
    if found { "present" } else { "missing" }.to_string()
}

/// Chapter Voice — readiness for one prerequisite path: `"unset"` (no path),
/// `"present"` (exists with the right kind), or `"missing"`.
fn path_status(p: Option<&Path>, want_dir: bool) -> String {
    match p {
        None => "unset",
        Some(path) => {
            let ok = if want_dir {
                path.is_dir()
            } else {
                path.is_file()
            };
            if ok { "present" } else { "missing" }
        }
    }
    .to_string()
}

/// Chapter Voice — re-read the config + return a `VoiceApplied` response.
/// `restart_required` is always `true`: `[voice]` is read when the voice channel
/// starts, so a write never affects a running voice process.
fn voice_applied(toml_path: &Path, role_override: Option<&str>) -> QueryResponsePayload {
    match load_settings_config(toml_path, role_override) {
        Ok(cfg) => QueryResponsePayload::VoiceApplied {
            settings: voice_snapshot(&cfg),
            restart_required: true,
        },
        Err(e) => QueryResponsePayload::QueryError {
            code: "config_reload_failed".into(),
            message: format!("voice settings written, but reloading them failed: {e}"),
        },
    }
}

/// Chapter Voice — a compact set/cleared summary of a `[voice]` write for the
/// `ConfigChanged` audit entry (the shape, not the values — though paths aren't
/// secrets, the audit stays uniform with the profile summary's posture).
fn voice_change_summary(w: &aivyx_config::VoiceWrite) -> String {
    let s = |label: &str, set: bool| format!("{label} = {}", if set { "set" } else { "cleared" });
    fn nonblank(o: &Option<String>) -> bool {
        o.as_deref().map(str::trim).is_some_and(|x| !x.is_empty())
    }
    [
        s("asr_engine", nonblank(&w.asr_engine)),
        s("tts_engine", nonblank(&w.tts_engine)),
        s("asr_model_path", nonblank(&w.asr_model_path)),
        s("asr_language", nonblank(&w.asr_language)),
        s("asr_beam_size", w.asr_beam_size.is_some()),
        s("tts_model_dir", nonblank(&w.tts_model_dir)),
        s("tts_voice_name", nonblank(&w.tts_voice_name)),
        s("tts_speed", w.tts_speed.is_some()),
        s("input_device", nonblank(&w.input_device)),
        s("output_device", nonblank(&w.output_device)),
    ]
    .join(", ")
}

/// Chapter U — build the wire snapshot from a loaded config.
fn settings_snapshot(
    cfg: &aivyx_config::AivyxConfig,
    embeddings_available: bool,
) -> aivyx_ipc::protocol::SettingsSnapshot {
    let b = &cfg.budget;
    aivyx_ipc::protocol::SettingsSnapshot {
        access_level: cfg.access_level.value.as_str().to_string(),
        fs_root: cfg.fs_root.value.display().to_string(),
        confirm_destructive: cfg.confirm_destructive.value,
        provider: provider_label(cfg.provider.value).to_string(),
        model: cfg.model.value.clone(),
        num_ctx: cfg.ollama_options.num_ctx,
        budget: aivyx_ipc::protocol::BudgetSnapshot {
            per_run_usd: b.per_run_usd,
            per_day_usd: b.per_day_usd,
            on_exceeded: budget_action_label(b.on_exceeded).to_string(),
            alert_at: b.alert_at,
        },
        embeddings_available,
        cycle_detection: cfg.cycle_detection.unwrap_or(false),
        autonomy_level: cfg.autonomy_level.value.as_str().to_string(),
    }
}

/// Chapter U — append a `ConfigChanged` audit entry for a settings write.
/// Best-effort: a write with no audit log (test fixture) is silently
/// unaudited; an append failure is logged but does not fail the write (the
/// file change already landed).
fn audit_config_change(audit_log: Option<&PersistentAuditLog>, section: &str, summary: &str) {
    if let Some(log) = audit_log {
        if let Err(e) = log.append(aivyx_audit::AuditEvent::ConfigChanged {
            section: section.to_string(),
            summary: summary.to_string(),
        }) {
            eprintln!("aivyx-pa daemon: failed to audit config change: {e}");
        }
    }
}

/// Chapter U — map a `ConfigWriteError` to a `QueryError` with a stable code.
fn map_config_write_error(e: aivyx_config::ConfigWriteError) -> QueryResponsePayload {
    use aivyx_config::ConfigWriteError as E;
    let code = match &e {
        E::RootRequired { .. } => "root_required",
        E::RootNotAllowed { .. } => "root_not_allowed",
        E::InvalidBudget { .. } => "invalid_budget",
        E::InvalidMcpServer { .. } => "invalid_mcp_server",
        E::InvalidNotifyTarget { .. } => "invalid_notify_target",
        E::InvalidEmailConfig { .. } => "invalid_email_config",
        E::InvalidReflectionSchedule { .. } => "invalid_reflection_schedule",
        E::InvalidMemoryProfile { .. } => "invalid_memory_profile",
        E::InvalidEmbeddingConfig { .. } => "invalid_embedding_config",
        E::InvalidProactiveConfig { .. } => "invalid_proactive_config",
        E::Parse { .. } => "config_parse_failed",
        E::Io { .. } => "config_write_failed",
    };
    QueryResponsePayload::QueryError {
        code: code.into(),
        message: e.to_string(),
    }
}

/// Re-read `[[mcp_server]]` from disk into the wire view type — shared by
/// `GetMcpServerConfigs`/`SetMcpServer`/`DeleteMcpServer`'s handlers so the
/// response always reflects authoritative on-disk state.
///
/// Deliberately does **not** go through `load_settings_config` (the full,
/// fully-resolving `AivyxConfig` loader) the way `settings_applied` does:
/// that loader interpolates `${VAR}` in `env`/`headers` against the
/// daemon's real environment, so a `GetX` response built from it would hand
/// the browser a resolved secret value — and `McpServerForm` seeds its edit
/// form straight from that response, so a save-without-editing would then
/// bake the resolved secret into `aivyx-pa.toml` as a literal, permanently
/// destroying the `${VAR}` placeholder it replaced (final-review fix #1).
/// `aivyx_config::config_write::read_mcp_server_entries` reads the TOML
/// literally instead — no interpolation, and (as a side effect) it also
/// surfaces `enabled = false` entries, which the full loader silently
/// skips.
fn read_mcp_server_configs(
    path: &std::path::Path,
) -> Result<Vec<aivyx_ipc::protocol::McpServerConfigView>, String> {
    let entries =
        aivyx_config::config_write::read_mcp_server_entries(path).map_err(|e| e.to_string())?;
    Ok(entries
        .into_iter()
        .map(|s| aivyx_ipc::protocol::McpServerConfigView {
            name: s.name,
            transport: s.transport,
            command: s.command,
            args: s.args,
            env: s.env,
            headers: s.headers,
            url: s.url,
            enabled: s.enabled,
        })
        .collect())
}

/// Chapter U-mirroring convention (`settings_applied`'s own re-read): after
/// a successful `SetMcpServer`/`DeleteMcpServer` write, re-read the fresh
/// on-disk state for the response. If the re-read itself fails (final-review
/// fix #5 — e.g. the file was concurrently replaced with something that no
/// longer parses as TOML between the write and this read), surface it as a
/// `QueryError` rather than silently claim the operator now has zero
/// configured servers: a bare `McpServersApplied { servers: vec![], .. }`
/// would be indistinguishable from "you deleted everything", masking a
/// daemon that may now fail to boot.
fn mcp_servers_applied_after_write(path: &std::path::Path, verb: &str) -> QueryResponsePayload {
    match read_mcp_server_configs(path) {
        Ok(servers) => QueryResponsePayload::McpServersApplied {
            servers,
            restart_required: true,
        },
        Err(e) => QueryResponsePayload::QueryError {
            code: "config_reload_failed".into(),
            message: format!("mcp server {verb} succeeded, but reloading the list failed: {e}"),
        },
    }
}

/// Re-read `[[notify_target]]` from disk into the wire view type — shared
/// by `GetNotifyTargetConfigs`/`SetNotifyTarget`/`DeleteNotifyTarget`'s
/// handlers, mirroring `read_mcp_server_configs`'s own convention exactly.
fn read_notify_target_configs(
    path: &std::path::Path,
) -> Result<Vec<aivyx_ipc::protocol::NotifyTargetConfigView>, String> {
    let entries = aivyx_config::config_write::read_notify_target_entries(path)
        .map_err(|e| e.to_string())?;
    Ok(entries
        .into_iter()
        .map(|t| aivyx_ipc::protocol::NotifyTargetConfigView {
            name: t.name,
            kind: t.kind,
            chat_id: t.chat_id,
            url: t.url,
            to: t.to,
            enabled: t.enabled,
            is_default: t.is_default,
            retry_count: t.retry_count,
            retry_backoff_ms_start: t.retry_backoff_ms_start,
            rate_limit_max: t.rate_limit_max,
            rate_limit_window_secs: t.rate_limit_window_secs,
        })
        .collect())
}

/// Mirrors `mcp_servers_applied_after_write`'s own convention exactly,
/// including surfacing a re-read failure as a `QueryError` rather than
/// silently claiming zero targets (plan 1's final-review finding #5).
fn notify_targets_applied_after_write(path: &std::path::Path, verb: &str) -> QueryResponsePayload {
    match read_notify_target_configs(path) {
        Ok(targets) => QueryResponsePayload::NotifyTargetsApplied {
            targets,
            restart_required: true,
        },
        Err(e) => QueryResponsePayload::QueryError {
            code: "config_reload_failed".into(),
            message: format!("notify target {verb} succeeded, but reloading the list failed: {e}"),
        },
    }
}

/// Re-read `[[reflection_schedule]]` from disk into the wire view type —
/// shared by `GetReflectionScheduleConfigs`/`SetReflectionSchedule`/
/// `DeleteReflectionSchedule`'s handlers, mirroring
/// `read_notify_target_configs`'s own convention exactly.
fn read_reflection_schedule_configs(
    path: &std::path::Path,
) -> Result<Vec<aivyx_ipc::protocol::ReflectionScheduleConfigView>, String> {
    let entries = aivyx_config::config_write::read_reflection_schedule_entries(path)
        .map_err(|e| e.to_string())?;
    Ok(entries
        .into_iter()
        .map(|e| aivyx_ipc::protocol::ReflectionScheduleConfigView {
            name: e.name,
            cron: e.cron,
            lookback_window_secs: e.lookback_window_secs,
            enabled: e.enabled,
        })
        .collect())
}

/// Mirrors `notify_targets_applied_after_write`'s own convention
/// exactly, including surfacing a re-read failure as a `QueryError`
/// rather than silently claiming zero entries.
fn reflection_schedules_applied_after_write(path: &std::path::Path, verb: &str) -> QueryResponsePayload {
    match read_reflection_schedule_configs(path) {
        Ok(schedules) => QueryResponsePayload::ReflectionScheduleConfigApplied {
            schedules,
            restart_required: true,
        },
        Err(e) => QueryResponsePayload::QueryError {
            code: "config_reload_failed".into(),
            message: format!("reflection schedule {verb} succeeded, but reloading the list failed: {e}"),
        },
    }
}

/// Build a `RedactedSecret` from a raw value read straight off disk (never
/// through the interpolating loader — see `read_mcp_server_entries`'s own
/// doc comment for why that distinction matters). `source` is always
/// `"toml"`: this read path never resolves an env-var fallback, so `"toml"`
/// is the only value it could ever truthfully report.
fn redact(raw: Option<&str>) -> aivyx_ipc::protocol::RedactedSecret {
    aivyx_ipc::protocol::RedactedSecret {
        configured: raw.is_some_and(|s| !s.is_empty()),
        source: "toml".to_string(),
    }
}

fn email_config_view(e: &aivyx_config::config_write::EmailEntryWrite) -> aivyx_ipc::protocol::EmailConfigView {
    aivyx_ipc::protocol::EmailConfigView {
        host: e.host.clone(),
        port: e.port,
        tls_mode: e.tls_mode.clone(),
        username: e.username.clone(),
        password: redact(e.password.as_deref()),
        from: e.from.clone(),
    }
}

fn embedding_config_view(e: &aivyx_config::config_write::EmbeddingEntryWrite) -> aivyx_ipc::protocol::EmbeddingConfigView {
    aivyx_ipc::protocol::EmbeddingConfigView {
        base_url: e.base_url.clone(),
        model: e.model.clone(),
        api_key: redact(e.api_key.as_deref()),
    }
}

fn proactive_config_view(p: &aivyx_config::config_write::ProactiveEntryWrite) -> aivyx_ipc::protocol::ProactiveConfigView {
    aivyx_ipc::protocol::ProactiveConfigView {
        enabled: p.enabled.unwrap_or(false),
        target: p.target.clone(),
        max_per_window: p.max_per_window.unwrap_or(aivyx_config::DEFAULT_PROACTIVE_MAX_PER_WINDOW),
        window_secs: p.window_secs.unwrap_or(aivyx_config::DEFAULT_PROACTIVE_WINDOW_SECS),
    }
}

fn telegram_config_view(t: &aivyx_config::config_write::TelegramEntryWrite) -> aivyx_ipc::protocol::TelegramConfigView {
    aivyx_ipc::protocol::TelegramConfigView {
        token: redact(t.token.as_deref()),
        chat_id: t.chat_id,
        team_run_channel: t.team_run_channel.unwrap_or(false),
        team_trigger_rate_limit: t.team_trigger_rate_limit,
        team_command_allowed_senders: t.team_command_allowed_senders.clone().unwrap_or_default(),
    }
}

fn discord_config_view(d: &aivyx_config::config_write::DiscordEntryWrite) -> aivyx_ipc::protocol::DiscordConfigView {
    aivyx_ipc::protocol::DiscordConfigView {
        token: redact(d.token.as_deref()),
        application_id: d.application_id,
        team_run_channel: d.team_run_channel.unwrap_or(false),
        team_trigger_rate_limit: d.team_trigger_rate_limit,
        team_command_allowed_senders: d.team_command_allowed_senders.clone().unwrap_or_default(),
    }
}

fn slack_config_view(s: &aivyx_config::config_write::SlackEntryWrite) -> aivyx_ipc::protocol::SlackConfigView {
    aivyx_ipc::protocol::SlackConfigView {
        bot_token: redact(s.bot_token.as_deref()),
        app_token: redact(s.app_token.as_deref()),
        team_id: s.team_id.clone(),
        team_run_channel: s.team_run_channel.unwrap_or(false),
        team_trigger_rate_limit: s.team_trigger_rate_limit,
        team_command_allowed_senders: s.team_command_allowed_senders.clone().unwrap_or_default(),
    }
}

/// Chapter U — display label for a provider in the read-only Settings card.
fn provider_label(p: aivyx_config::ProviderKind) -> &'static str {
    use aivyx_config::ProviderKind::*;
    match p {
        Anthropic => "anthropic",
        OpenAi => "openai",
        Ollama => "ollama",
        LlamaCpp => "llamacpp",
        Jan => "jan",
        MistralRs => "mistralrs",
        Broker => "broker",
    }
}

/// Chapter U — stable `[budget] on_exceeded` label (matches the toml repr).
fn budget_action_label(a: aivyx_cost::BudgetAction) -> &'static str {
    match a {
        aivyx_cost::BudgetAction::Alert => "alert",
        aivyx_cost::BudgetAction::Deny => "deny",
    }
}

/// Chapter U — render an optional dollar cap for an audit summary.
fn opt_usd(v: Option<f64>) -> String {
    v.map(|n| format!("{n}"))
        .unwrap_or_else(|| "none".to_string())
}

/// Chapter U — render an optional alert fraction for an audit summary.
fn opt_frac(v: Option<f64>) -> String {
    v.map(|n| format!("{n}"))
        .unwrap_or_else(|| "none".to_string())
}

/// Phase 74 — convert an `aivyx_memory::MemoryEntry` into the
/// flat wire `MemoryEntrySummary`.
fn memory_entry_summary(e: aivyx_memory::MemoryEntry) -> crate::daemon_ipc::MemoryEntrySummary {
    crate::daemon_ipc::MemoryEntrySummary {
        topic: e.topic,
        body: e.body,
        seq: e.seq,
        created_at_secs: e.created_at_secs,
        last_read_at_secs: e.last_read_at_secs,
    }
}

/// Chapter MG — assemble the memory knowledge graph: topic nodes (each counted,
/// capped) + the top-`limit` weighted co-occurrence edges. `Err` only when
/// listing topics fails; an absent ledger yields no edges (a topic cloud).
async fn build_memory_graph(
    mem: &dyn aivyx_memory::Memory,
    cooccurrence_ledger: Option<&crate::cooccurrence_ledger::PersistentCooccurrenceLedger>,
    limit: u32,
) -> Result<
    (
        Vec<aivyx_ipc::protocol::MemoryGraphNode>,
        Vec<aivyx_ipc::PairScore>,
    ),
    String,
> {
    // Node entry-count cap — a node's size is a rough heat, not an exact tally.
    const NODE_COUNT_CAP: usize = 200;
    // #11/#D — drop internal `context:pruned:*` topics so the MG topic
    // cloud shows the user's knowledge, not the daemon's prune bookkeeping
    // (the #11 filter covered `memory list` + the wiki/Lattice graph, but
    // this co-occurrence graph read its own raw `list_topics`).
    let topics: Vec<String> = mem
        .list_topics()
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|t| !crate::prune_sink::is_internal_topic(t))
        .collect();
    let mut nodes = Vec::with_capacity(topics.len());
    for topic in &topics {
        let entry_count = mem
            .get_recent(topic, NODE_COUNT_CAP)
            .await
            .map(|e| e.len() as u32)
            .unwrap_or(0);
        nodes.push(aivyx_ipc::protocol::MemoryGraphNode {
            topic: topic.clone(),
            entry_count,
        });
    }

    let capped = limit.clamp(1, 200) as usize;
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let edges = match cooccurrence_ledger {
        Some(l) => l
            .top_affinities(now_secs, capped)
            .await
            .map(|p| p.top_pairs)
            .unwrap_or_default()
            .into_iter()
            // Drop any edge touching an internal topic on either end.
            .filter(|p| {
                !crate::prune_sink::is_internal_topic(&p.a)
                    && !crate::prune_sink::is_internal_topic(&p.b)
            })
            .collect(),
        None => Vec::new(),
    };
    Ok((nodes, edges))
}

/// Phase 73 — render an `AutoNotifyOutcomeSummary` into
/// (kind, detail) pair for the wire-format history entry. `kind`
/// is the stable lowercase label; `detail` carries variant-
/// specific data.
fn render_notify_outcome_for_history(
    summary: &aivyx_audit::AutoNotifyOutcomeSummary,
) -> (&'static str, String) {
    use aivyx_audit::AutoNotifyOutcomeSummary;
    match summary {
        AutoNotifyOutcomeSummary::Delivered => ("delivered", String::new()),
        AutoNotifyOutcomeSummary::SkippedEmptyResponse => ("skipped_empty_response", String::new()),
        AutoNotifyOutcomeSummary::Failed {
            error_kind,
            error_message,
        } => ("failed", format!("[{error_kind}] {error_message}")),
        AutoNotifyOutcomeSummary::SkippedByCondition { condition } => {
            ("skipped_by_condition", condition.clone())
        }
        AutoNotifyOutcomeSummary::SkippedByRateLimit { limit, window_secs } => {
            ("skipped_by_rate_limit", format!("{limit}/{window_secs}s"))
        }
    }
}

/// Phase 70 — parse the wire-format status filter string into
/// the typed enum. Unknown values fall through to `Pending` per
/// the IPC contract documented at `QueryPayload::ListPersonaProposals`.
fn parse_proposal_status_filter(s: &str) -> crate::persona_proposal::ProposalStatusFilter {
    use crate::persona_proposal::ProposalStatusFilter;
    match s.to_ascii_lowercase().as_str() {
        "all" => ProposalStatusFilter::All,
        "approved" => ProposalStatusFilter::Approved,
        "rejected" => ProposalStatusFilter::Rejected,
        "superseded" => ProposalStatusFilter::Superseded,
        _ => ProposalStatusFilter::Pending,
    }
}

/// Phase 70 — convert an in-memory `PersonaProposal` view into
/// the wire `PersonaProposalSummary` shape.
fn proposal_summary_from_view(
    view: crate::persona_proposal::PersonaProposal,
) -> crate::daemon_ipc::PersonaProposalSummary {
    use crate::persona_proposal::ProposalStatus;
    let category = format!("{:?}", view.proposed_op.category);
    let proposed_reason = view.proposed_op.reason.clone();
    let proposed_op = serde_json::to_value(&view.proposed_op.op).unwrap_or(serde_json::Value::Null);
    // Phase 92 → Phase 94 — lift the linkage onto the
    // summary so the surface grouping helper doesn't need
    // to re-parse `proposed_op` JSON.
    let supersedes_proposal_id = view.proposed_op.supersedes_proposal_id.clone();
    let (status, applied_op, applied_seq, rejected_reason, resolved_at_unix_ms) = match view.status
    {
        ProposalStatus::Pending => ("Pending".to_string(), None, None, None, None),
        ProposalStatus::Approved {
            applied_op,
            applied_seq,
            resolved_at_unix_ms,
        } => (
            "Approved".to_string(),
            Some(serde_json::to_value(&applied_op.op).unwrap_or(serde_json::Value::Null)),
            Some(applied_seq),
            None,
            Some(resolved_at_unix_ms),
        ),
        ProposalStatus::Rejected {
            reason,
            resolved_at_unix_ms,
        } => (
            "Rejected".to_string(),
            None,
            None,
            reason,
            Some(resolved_at_unix_ms),
        ),
        ProposalStatus::Superseded {
            by_proposal_id: _,
            resolved_at_unix_ms,
        } => (
            "Superseded".to_string(),
            None,
            None,
            None,
            Some(resolved_at_unix_ms),
        ),
    };
    crate::daemon_ipc::PersonaProposalSummary {
        id: view.id,
        proposed_at_unix_ms: view.proposed_at_unix_ms,
        source_reflection_session_id: view.source_reflection_session_id,
        status,
        category,
        proposed_op,
        proposed_reason,
        applied_op,
        applied_seq,
        rejected_reason,
        resolved_at_unix_ms,
        supersedes_proposal_id,
    }
}

/// Convert an in-memory effective Persona state into the wire
/// [`EffectivePersonaSummary`]. Phase 60.
fn effective_persona_summary_from_state(
    state: &crate::persona::EffectivePersona,
) -> crate::daemon_ipc::EffectivePersonaSummary {
    crate::daemon_ipc::EffectivePersonaSummary {
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
        is_non_empty: state.is_non_empty(),
    }
}

/// Convert a signed persona chain entry into the wire summary
/// shape. Phase 60.
fn persona_delta_summary_from_signed(
    entry: &crate::persona::SignedPersonaEntry,
) -> crate::daemon_ipc::PersonaDeltaSummary {
    let category_label = format!("{:?}", entry.delta.category);
    let op_value = serde_json::to_value(&entry.delta.op).unwrap_or(serde_json::Value::Null);
    crate::daemon_ipc::PersonaDeltaSummary {
        seq: entry.seq,
        delta_id: entry.delta.delta_id.clone(),
        proposed_at_unix_ms: entry.delta.proposed_at_unix_ms,
        approved_at_unix_ms: entry.delta.approved_at_unix_ms,
        proposal_id: entry.delta.proposal_id.clone(),
        category: category_label,
        op: op_value,
        mac_hex: entry.mac.iter().map(|b| format!("{b:02x}")).collect(),
    }
}

/// Convert an in-memory [`aivyx_config::Profile`] to the
/// [`ProfileSummary`] wire shape. Phase 58 — flattens `Sourced<T>`
/// into plain serializable fields and pre-computes the
/// `injection_enabled` predicate so the Web UI does not need to
/// re-implement the rule.
fn profile_summary_from_profile(profile: &aivyx_config::Profile) -> ProfileSummary {
    ProfileSummary {
        assistant_name: profile.assistant_name.value.clone(),
        assistant_name_source: field_source_label(profile.assistant_name.source).to_string(),
        operator_profile: profile.operator_profile.clone(),
        communication_style: profile.communication_style.clone(),
        primary_use_cases: profile.primary_use_cases.clone(),
        behavioral_preferences: profile.behavioral_preferences.clone(),
        behavioral_constraints: profile.behavioral_constraints.clone(),
        injection_enabled: profile.is_operator_declared(),
    }
}

/// Phase 60 — append an operator-initiated revert delta and
/// recompute the shared runtime state. Returns the new chain seq
/// on success; a human-readable reason on failure.
async fn resolve_persona_revert(
    persona_log: Option<&crate::persona::PersistentPersonaLog>,
    shared_persona: &crate::persona::SharedEffectivePersona,
    target_delta_id: &str,
) -> Result<u64, String> {
    let persona_log =
        persona_log.ok_or_else(|| "daemon has no persona log configured".to_string())?;
    // Validate the target exists in the chain before appending the
    // revert. Forward-pointing targets are rejected at fold time,
    // but rejecting them at append time gives a better operator
    // error message.
    let entries = persona_log.entries();
    let target = entries
        .iter()
        .find(|e| e.delta.delta_id == target_delta_id)
        .ok_or_else(|| format!("no persona delta found with id `{target_delta_id}`"))?;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let revert = crate::persona::PersonaDelta {
        delta_id: format!("pd-revert-{target_delta_id}"),
        proposed_at_unix_ms: now_ms,
        approved_at_unix_ms: now_ms,
        proposal_id: format!("op-revert-{target_delta_id}"),
        category: target.delta.category,
        op: crate::persona::PersonaDeltaOp::Revert {
            target_delta_id: target_delta_id.to_string(),
        },
    };
    let seq = persona_log
        .append(revert)
        .await
        .map_err(|e| format!("persona chain append failed: {e}"))?;
    let entries_after = persona_log.entries();
    if !crate::persona::recompute_shared_from_entries(shared_persona, &entries_after) {
        return Err("shared persona state lock poisoned during recompute".into());
    }
    Ok(seq)
}

/// Chapter Accord — resolve a detected Persona contradiction by removing the
/// losing facet. Maps the wire `category` to one of the five removable
/// soft-list categories (the operator `profile_constraint` and the scalar
/// identity fields are immutable here), appends a `RemoveList` persona delta,
/// and recomputes shared state so the next turn drops the facet. Returns the
/// new chain seq.
async fn resolve_soul_conflict(
    persona_log: Option<&crate::persona::PersistentPersonaLog>,
    shared_persona: &crate::persona::SharedEffectivePersona,
    category: &str,
    value: &str,
) -> Result<u64, String> {
    use aivyx_ipc::persona::PersonaDeltaCategory as Cat;
    let persona_log =
        persona_log.ok_or_else(|| "daemon has no persona log configured".to_string())?;
    if value.trim().is_empty() {
        return Err("no facet value to remove".to_string());
    }
    // Chapter Accord (skill-layer) — a learned-skill side is removed BY NAME
    // (its stored value is JSON, not a plain list string), reusing the same
    // operator-forget primitive the Repertoire screen uses.
    if category == aivyx_ipc::soul_conflict::SoulFacet::LEARNED_SKILL {
        let removed =
            crate::skill_edit::operator_forget_skill(persona_log, shared_persona, value).await?;
        if !removed {
            return Err(format!("no learned skill named `{value}` to remove"));
        }
        // operator_forget_skill already recomputed shared state; report the
        // chain tip as the seq.
        return Ok(persona_log.len().saturating_sub(1) as u64);
    }
    // Only the five accreted soft-list categories are removable. The operator
    // profile_constraint side of a cross-layer conflict is immutable, and the
    // scalar identity fields are not list facets.
    let cat = match category {
        "character_traits" => Cat::CharacterTraits,
        "communication_adaptations" => Cat::CommunicationAdaptations,
        "behavioral_preferences" => Cat::BehavioralPreferences,
        "learned_context" => Cat::LearnedContext,
        "relationship_milestones" => Cat::RelationshipMilestones,
        aivyx_ipc::soul_conflict::SoulFacet::PROFILE_CONSTRAINT => {
            return Err(
                "that side is an operator Profile constraint — it is immutable; \
                 remove the conflicting learned facet instead, or edit the \
                 constraint in your Profile"
                    .to_string(),
            );
        }
        other => {
            return Err(format!(
                "`{other}` is not a removable persona facet category"
            ));
        }
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let delta = crate::persona::PersonaDelta {
        delta_id: format!("pd-accord-{now_ms}"),
        proposed_at_unix_ms: now_ms,
        approved_at_unix_ms: now_ms,
        proposal_id: format!("op-accord-{now_ms}"),
        category: cat,
        op: crate::persona::PersonaDeltaOp::RemoveList {
            value: value.to_string(),
        },
    };
    let seq = persona_log
        .append(delta)
        .await
        .map_err(|e| format!("persona chain append failed: {e}"))?;
    let entries_after = persona_log.entries();
    if !crate::persona::recompute_shared_from_entries(shared_persona, &entries_after) {
        return Err("shared persona state lock poisoned during recompute".into());
    }
    Ok(seq)
}

/// Chapter X — daemon-side **live** persona seed (the web onboarding path).
/// Maps the wire seed to `aivyx_config::PersonaSeed` and plants it via the same
/// `seed_persona_chain_if_empty` primitive the boot-seed uses — so a grown
/// persona is never overwritten and the seed is signed + audited. The chain-
/// empty check is surfaced as a friendly operator error before the primitive.
async fn seed_persona_live(
    persona_log: Option<&crate::persona::PersistentPersonaLog>,
    shared_persona: &crate::persona::SharedEffectivePersona,
    audit: Option<&PersistentAuditLog>,
    wire: aivyx_ipc::protocol::PersonaSeedWire,
) -> Result<u64, String> {
    let log = persona_log.ok_or_else(|| "daemon has no persona log configured".to_string())?;
    if !log.entries().is_empty() {
        return Err(
            "the persona already has content; seeding is only available for a fresh agent".into(),
        );
    }
    let seed = wire_to_persona_seed(wire);
    let appended = crate::persona::seed_persona_chain_if_empty(log, shared_persona, audit, &seed)
        .await
        .map_err(|e| format!("persona seed failed: {e}"))?;
    if appended == 0 {
        return Err("nothing to seed — add at least one trait, note, or skill".into());
    }
    Ok(appended)
}

/// Chapter Tutor — operator-initiated skill authoring on a **grown** chain.
///
/// The operator (via `aivyx-pa skills …` or the Studio) is the authority here, so
/// unlike the agent's scope-gated `skills.teach`/`update`/`forget` tools this
/// runs straight from the `AuthorSkill` IPC with no agent scope. It reuses the
/// exact same op-builders ([`crate::skill_edit`]) + chain-append
/// ([`crate::skill_tool::commit_ops`]) the tools use, so operator- and
/// agent-authored skills land identically (signed, audited, live-recomputed).
/// Returns the chain seq of the last appended delta.
async fn author_skill_live(
    persona_log: Option<&Arc<crate::persona::PersistentPersonaLog>>,
    shared_persona: &crate::persona::SharedEffectivePersona,
    op: aivyx_ipc::protocol::SkillAuthorOp,
    name: &str,
    trigger: Option<&str>,
    procedure: Option<&str>,
) -> Result<u64, String> {
    use aivyx_ipc::protocol::SkillAuthorOp;
    let log = persona_log.ok_or_else(|| "daemon has no persona log configured".to_string())?;
    crate::skill_edit::validate_skill_name(name)?;
    let name = name.trim();
    let skills = crate::skill_tool::current_skills(shared_persona);
    let existing = crate::skill_edit::find_skill_by_name(&skills, name);
    let ops: Vec<crate::persona::PersonaDeltaOp> = match op {
        SkillAuthorOp::Teach => {
            if existing.is_some() {
                return Err(format!(
                    "a skill named {name:?} already exists — use update"
                ));
            }
            let trigger = trigger.unwrap_or("").trim();
            let procedure = procedure.unwrap_or("").trim();
            if trigger.is_empty() || procedure.is_empty() {
                return Err("teach needs a non-empty trigger and procedure".into());
            }
            let skill = crate::persona::LearnedSkill {
                name: name.to_string(),
                trigger: trigger.to_string(),
                procedure: procedure.to_string(),
                ..Default::default()
            };
            vec![crate::skill_edit::teach_op(&skill)]
        }
        SkillAuthorOp::Update => {
            let existing = existing.ok_or_else(|| format!("no skill named {name:?} to update"))?;
            if trigger.is_none() && procedure.is_none() {
                return Err("update needs a new trigger and/or procedure".into());
            }
            let merged = crate::skill_edit::merged_skill(existing, trigger, procedure);
            crate::skill_edit::update_ops(existing, &merged).to_vec()
        }
        SkillAuthorOp::Forget => {
            let existing = existing.ok_or_else(|| format!("no skill named {name:?} to forget"))?;
            vec![crate::skill_edit::forget_op(existing)]
        }
    };
    crate::skill_tool::commit_ops(log, shared_persona, &ops).await
}

/// Chapter X — map the wasm-clean wire seed to the config seed the primitive
/// consumes.
fn wire_to_persona_seed(wire: aivyx_ipc::protocol::PersonaSeedWire) -> aivyx_config::PersonaSeed {
    aivyx_config::PersonaSeed {
        learned_context: wire.learned_context,
        communication_adaptations: wire.communication_adaptations,
        character_traits: wire.character_traits,
        relationship_milestones: wire.relationship_milestones,
        skills: wire
            .skills
            .into_iter()
            .map(|s| aivyx_config::SeedSkill {
                name: s.name,
                trigger: s.trigger,
                procedure: s.procedure,
            })
            .collect(),
    }
}

/// Chapter X — map the config seed (from the LLM drafter) to the wasm-clean
/// wire seed the `DraftPersonaSeed` response carries.
fn persona_seed_to_wire(seed: aivyx_config::PersonaSeed) -> aivyx_ipc::protocol::PersonaSeedWire {
    aivyx_ipc::protocol::PersonaSeedWire {
        learned_context: seed.learned_context,
        communication_adaptations: seed.communication_adaptations,
        character_traits: seed.character_traits,
        relationship_milestones: seed.relationship_milestones,
        skills: seed
            .skills
            .into_iter()
            .map(|s| aivyx_ipc::protocol::SeedSkillWire {
                name: s.name,
                trigger: s.trigger,
                procedure: s.procedure,
            })
            .collect(),
    }
}

/// Chapter Genesis — map the LLM-drafted `DraftedProfile` to the
/// wasm-clean wire the `DraftProfile` response carries.
fn drafted_profile_to_wire(
    profile: crate::profile_draft::DraftedProfile,
) -> aivyx_ipc::protocol::ProfileDraftWire {
    aivyx_ipc::protocol::ProfileDraftWire {
        assistant_name: profile.assistant_name,
        operator_profile: profile.operator_profile,
        communication_style: profile.communication_style,
        primary_use_cases: profile.primary_use_cases,
        behavioral_preferences: profile.behavioral_preferences,
        behavioral_constraints: profile.behavioral_constraints,
    }
}

/// Phase 65 — daemon-side import handler. Validates → conflict
/// checks → optionally wipes → replays → recomputes shared state.
/// Best-effort per Q1(a): no atomic-tx wrapping. Returns
/// `PersonaImportSuccess { deltas_imported, final_chain_seq }`
/// on success.
async fn resolve_persona_import(
    persona_log: Option<&crate::persona::PersistentPersonaLog>,
    shared_persona: &crate::persona::SharedEffectivePersona,
    deltas: Vec<crate::identity_export::DeltaExport>,
    force: bool,
) -> Result<crate::daemon_ipc::PersonaImportSuccess, String> {
    let persona_log =
        persona_log.ok_or_else(|| "daemon has no persona log configured".to_string())?;

    // Re-validate each delta server-side — defense against the
    // CLI sending us a frame that bypassed parse_and_validate
    // (a malicious client, or a CLI bug).
    for (index, d) in deltas.iter().enumerate() {
        d.delta.validate().map_err(|reason| {
            format!(
                "incoming delta at index {index} (seq {seq}) failed validation: {reason}",
                seq = d.seq,
            )
        })?;
    }

    // Conflict check (Q3(a) at sign-off): refuse to overwrite
    // unless --force.
    let existing = persona_log.entries();
    if !existing.is_empty() && !force {
        return Err(format!(
            "persona chain not empty ({} entries); pass --force to overwrite",
            existing.len(),
        ));
    }

    // Force wipe.
    if force && !existing.is_empty() {
        persona_log
            .clear()
            .await
            .map_err(|e| format!("persona chain wipe failed: {e}"))?;
    }

    // Replay. Each append re-signs against the local HMAC key —
    // Phase 60 Q1(a) re-bind made concrete.
    let count = deltas.len() as u64;
    let mut last_seq: u64 = 0;
    for (index, d) in deltas.into_iter().enumerate() {
        let assigned_seq = persona_log.append(d.delta).await.map_err(|e| {
            format!(
                "persona chain append failed at index {index} (expected seq {}): {e}",
                d.seq,
            )
        })?;
        last_seq = assigned_seq;
    }

    // Refresh runtime state (Q3 — refresh: daemon recomputes
    // immediately). The next agent turn sees the imported state.
    let entries_after = persona_log.entries();
    if !crate::persona::recompute_shared_from_entries(shared_persona, &entries_after) {
        return Err("shared persona state lock poisoned during recompute".into());
    }

    Ok(crate::daemon_ipc::PersonaImportSuccess {
        deltas_imported: count,
        final_chain_seq: last_seq,
    })
}

fn field_source_label(src: aivyx_config::FieldSource) -> &'static str {
    match src {
        aivyx_config::FieldSource::Env => "env",
        aivyx_config::FieldSource::Toml => "toml",
        aivyx_config::FieldSource::EncryptedStore => "encrypted-store",
        aivyx_config::FieldSource::Default => "default",
    }
}

fn audit_entry_summary_from_signed(entry: aivyx_audit::SignedEntry) -> AuditEntrySummary {
    let appended_at_unix_ms = entry
        .appended_at
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    let event_type = match &entry.event {
        aivyx_audit::AuditEvent::ToolCall { .. } => "ToolCall",
        aivyx_audit::AuditEvent::ScopeDenied { .. } => "ScopeDenied",
        aivyx_audit::AuditEvent::RateLimited { .. } => "RateLimited",
        aivyx_audit::AuditEvent::TurnStarted { .. } => "TurnStarted",
        aivyx_audit::AuditEvent::TurnEnded { .. } => "TurnEnded",
        aivyx_audit::AuditEvent::LlmCost { .. } => "LlmCost",
        aivyx_audit::AuditEvent::ModelRouted { .. } => "ModelRouted",
        aivyx_audit::AuditEvent::MemoryAccess { .. } => "MemoryAccess",
        aivyx_audit::AuditEvent::AutoNotifyDispatched { .. } => "AutoNotifyDispatched",
        aivyx_audit::AuditEvent::SkillAutoProposal { .. } => "SkillAutoProposal",
        aivyx_audit::AuditEvent::SkillInvocation { .. } => "SkillInvocation",
        aivyx_audit::AuditEvent::ProfileHintApplied { .. } => "ProfileHintApplied",
        aivyx_audit::AuditEvent::RoleDraftImported { .. } => "RoleDraftImported",
        aivyx_audit::AuditEvent::HeadlessRefusal { .. } => "HeadlessRefusal",
        aivyx_audit::AuditEvent::ConfigChanged { .. } => "ConfigChanged",
        aivyx_audit::AuditEvent::PersonaSeeded { .. } => "PersonaSeeded",
        aivyx_audit::AuditEvent::DocumentMutated { .. } => "DocumentMutated",
        aivyx_audit::AuditEvent::ScheduleMutated { .. } => "ScheduleMutated",
        aivyx_audit::AuditEvent::TeamMissionChannelTriggered { .. } => "TeamMissionChannelTriggered",
        aivyx_audit::AuditEvent::TeamMissionChannelDenied { .. } => "TeamMissionChannelDenied",
    }
    .to_string();

    // `event` serializes to JSON unconditionally — the body is `Serialize`.
    let event = serde_json::to_value(&entry.event).unwrap_or(serde_json::Value::Null);

    let mut mac_hex = String::with_capacity(64);
    for b in entry.mac.iter() {
        mac_hex.push_str(&format!("{b:02x}"));
    }

    AuditEntrySummary {
        seq: entry.seq,
        appended_at_unix_ms,
        event_type,
        event,
        mac_hex,
    }
}

/// Phase 102 — fold a slice of audit `SignedEntry`s into per-tool
/// statistics for the `GetToolStats` query, joined against the
/// registered tool set.
///
/// `ToolCall` events are keyed by `scope_used.base()` — the stable,
/// human-meaningful capability base (`fs.read`, `net.fetch`),
/// unlike the per-process `tool_id`. Entries appended before
/// `cutoff` (when set) are skipped. Every registered tool yields a
/// row (zero stats if never called); a base with call history but
/// no currently registered tool yields a `registered: false` row.
/// Rows are ordered by call count descending, then name ascending.
fn fold_tool_stats(
    entries: &[aivyx_audit::SignedEntry],
    cutoff: Option<std::time::SystemTime>,
    tool_descriptors: &[ToolDescriptor],
) -> Vec<crate::daemon_ipc::ToolStat> {
    use std::collections::{BTreeMap, BTreeSet};

    #[derive(Default)]
    struct Acc {
        calls: u64,
        outcomes: BTreeMap<String, u64>,
        total_duration_ms: u64,
    }
    let mut acc: BTreeMap<String, Acc> = BTreeMap::new();

    for entry in entries {
        if let Some(cut) = cutoff {
            if entry.appended_at < cut {
                continue;
            }
        }
        let aivyx_audit::AuditEvent::ToolCall {
            scope_used,
            outcome,
            duration,
            ..
        } = &entry.event
        else {
            continue;
        };
        let a = acc.entry(scope_used.base().to_string()).or_default();
        a.calls += 1;
        a.total_duration_ms += duration.as_millis() as u64;
        let label = match outcome {
            aivyx_core::ToolOutcomeSummary::Completed { .. } => "completed",
            aivyx_core::ToolOutcomeSummary::Denied => "denied",
            aivyx_core::ToolOutcomeSummary::NotInRole => "not_in_role",
            aivyx_core::ToolOutcomeSummary::RateLimited => "rate_limited",
            aivyx_core::ToolOutcomeSummary::RequiresEscalation => "requires_escalation",
            aivyx_core::ToolOutcomeSummary::Failed => "failed",
        };
        *a.outcomes.entry(label.to_string()).or_insert(0) += 1;
    }

    let mut tools: Vec<crate::daemon_ipc::ToolStat> = Vec::new();
    let mut listed: BTreeSet<String> = BTreeSet::new();
    for desc in tool_descriptors {
        listed.insert(desc.scope_base.clone());
        let a = acc.get(&desc.scope_base);
        tools.push(crate::daemon_ipc::ToolStat {
            name: desc.name.clone(),
            description: desc.description.clone(),
            scope_base: desc.scope_base.clone(),
            registered: true,
            calls: a.map_or(0, |x| x.calls),
            outcomes: a.map(|x| x.outcomes.clone()).unwrap_or_default(),
            total_duration_ms: a.map_or(0, |x| x.total_duration_ms),
        });
    }
    // Bases with audit history but no currently registered tool.
    for (base, a) in &acc {
        if listed.contains(base) {
            continue;
        }
        tools.push(crate::daemon_ipc::ToolStat {
            name: base.clone(),
            description: "(no registered tool)".to_string(),
            scope_base: base.clone(),
            registered: false,
            calls: a.calls,
            outcomes: a.outcomes.clone(),
            total_duration_ms: a.total_duration_ms,
        });
    }
    tools.sort_by(|x, y| y.calls.cmp(&x.calls).then_with(|| x.name.cmp(&y.name)));
    tools
}

/// Phase 186 — `aivyx-ipc` cannot depend on `aivyx-channel` (the
/// wasm-clean boundary), so this is a plain field-for-field copy, not a
/// `From` impl (`impl From<Reminder> for ReminderView` would violate the
/// orphan rule: both types are foreign to this crate's own local types).
fn reminder_to_view(r: crate::reminder_store::Reminder) -> ReminderView {
    ReminderView {
        id: r.id,
        due_unix: r.due_unix,
        message: r.message,
        notify_targets: r.notify_targets,
        created_unix: r.created_unix,
    }
}

/// Phase 186 — the `GetReminders` query's actual logic, extracted so it's
/// testable without hand-constructing the giant `handle_query` parameter
/// list (matches `fold_tool_stats`/`fold_mcp_server_stats`'s own
/// extracted-pure-function precedent). `None` (no `[reminders]` /
/// reminder store configured) degrades to an empty list, not an error —
/// matches every other `Option<&...>` arm in `handle_query`.
async fn reminders_query_response(
    reminder_store: Option<&crate::reminder_tool::SharedReminderStore>,
) -> QueryResponsePayload {
    let Some(store) = reminder_store else {
        return QueryResponsePayload::Reminders { reminders: Vec::new() };
    };
    match store.list().await {
        Ok(reminders) => QueryResponsePayload::Reminders {
            reminders: reminders.into_iter().map(reminder_to_view).collect(),
        },
        Err(e) => QueryResponsePayload::QueryError {
            code: "reminders_failed".into(),
            message: e.to_string(),
        },
    }
}

/// POLISH_WAVES.md sub-project 8 item C — fold `mcp.call`-scoped audit
/// `ToolCall` events into per-MCP-server statistics. Shares
/// `fold_tool_stats`'s own audit-walking/cutoff-window shape, but
/// groups by the server name recovered from the scope's qualifier
/// (`mcp.call:<server>:<tool>`) instead of by `scope_used.base()`
/// (which is the literal string `"mcp.call"` for every MCP-bridged
/// tool call, regardless of server — the exact gap this function
/// closes). No registry join (unlike `fold_tool_stats`, which joins
/// against `tool_descriptors`): a server with zero calls in the
/// window simply has no row here, and the caller (the Studio's
/// `McpPanel`) joins this list against its own already-fetched
/// `GetMcpStatus` server list client-side to render a "no recent
/// activity" state for a configured-but-unused server. Accepted
/// limitation: this groups purely by name-as-written against the
/// (immutable, historical) audit chain, so if an operator renames server
/// `A` to `B` and later configures a *new*, different server also named
/// `A` within the same rolling window, that new `A`'s health chip will
/// include the old `A`'s audit history — there is no way to distinguish
/// "the same server renamed" from "a different server reusing an old
/// name" from a rolling audit-derived signal alone.
fn fold_mcp_server_stats(
    entries: &[aivyx_audit::SignedEntry],
    cutoff: Option<std::time::SystemTime>,
) -> Vec<crate::daemon_ipc::McpServerCallStats> {
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct Acc {
        calls: u64,
        outcomes: BTreeMap<String, u64>,
        total_duration_ms: u64,
    }
    let mut acc: BTreeMap<String, Acc> = BTreeMap::new();

    for entry in entries {
        if let Some(cut) = cutoff {
            if entry.appended_at < cut {
                continue;
            }
        }
        let aivyx_audit::AuditEvent::ToolCall {
            scope_used,
            outcome,
            duration,
            ..
        } = &entry.event
        else {
            continue;
        };
        if scope_used.base() != "mcp.call" {
            continue;
        }
        let Some(qualifier) = scope_used.qualifier() else {
            continue;
        };
        // The qualifier is "<server>:<tool>" (proxy.rs's own
        // construction: `format!("mcp.call:{server_name}:{tool_name}")`).
        // Split from the LEFT: the server name comes from the operator's
        // own [[mcp_server]] config (the operator controls it and can
        // avoid a colon), but the tool name is deserialized verbatim
        // from the remote MCP server's own tools/list response with no
        // sanitization anywhere in crates/aivyx-mcp — an untrusted,
        // remote-controlled string that could contain a colon. Splitting
        // from the right would let a misbehaving server hide its own
        // failures behind a garbage server-name split, silently showing
        // "no recent activity" for a server that's actually broken —
        // exactly the failure class this whole aggregation exists to
        // surface.
        let Some((server_name, _tool_name)) = qualifier.split_once(':') else {
            continue;
        };
        let a = acc.entry(server_name.to_string()).or_default();
        a.calls += 1;
        a.total_duration_ms += duration.as_millis() as u64;
        let label = match outcome {
            aivyx_core::ToolOutcomeSummary::Completed { .. } => "completed",
            aivyx_core::ToolOutcomeSummary::Denied => "denied",
            aivyx_core::ToolOutcomeSummary::NotInRole => "not_in_role",
            aivyx_core::ToolOutcomeSummary::RateLimited => "rate_limited",
            aivyx_core::ToolOutcomeSummary::RequiresEscalation => "requires_escalation",
            aivyx_core::ToolOutcomeSummary::Failed => "failed",
        };
        *a.outcomes.entry(label.to_string()).or_insert(0) += 1;
    }

    let mut servers: Vec<crate::daemon_ipc::McpServerCallStats> = acc
        .into_iter()
        .map(|(server_name, a)| crate::daemon_ipc::McpServerCallStats {
            server_name,
            calls: a.calls,
            outcomes: a.outcomes,
            total_duration_ms: a.total_duration_ms,
        })
        .collect();
    servers.sort_by(|x, y| y.calls.cmp(&x.calls).then_with(|| x.server_name.cmp(&y.server_name)));
    servers
}

/// Chapter Almanac — build the `GetToolCatalog` response rows from the
/// registered-tool snapshot: no audit-chain read, unlike `fold_tool_stats`
/// above (this is a pure registry catalog, not observability). `min_tier`
/// is derived from the *bare* form of each tool's scope base — a base
/// that fails to parse (should not happen; `scope_base` always comes from
/// a real `Scope`) falls back to `TrustTier::Kernel`, the safe over-
/// estimate.
fn build_tool_catalog(
    tool_descriptors: &[ToolDescriptor],
) -> Vec<crate::daemon_ipc::ToolCatalogEntry> {
    tool_descriptors
        .iter()
        .map(|d| {
            let min_tier = aivyx_capability::Scope::parse(&d.scope_base)
                .map(|s| aivyx_capability::TrustTier::min_for_scope(&s))
                .unwrap_or(aivyx_capability::TrustTier::Kernel);
            crate::daemon_ipc::ToolCatalogEntry {
                name: d.name.clone(),
                description: d.description.clone(),
                scope_base: d.scope_base.clone(),
                min_tier,
            }
        })
        .collect()
}

fn mission_state_label(state: mission::MissionState) -> &'static str {
    match state {
        mission::MissionState::Created => "Created",
        mission::MissionState::Running => "Running",
        mission::MissionState::GatePending => "GatePending",
        mission::MissionState::Completed => "Completed",
        mission::MissionState::Failed => "Failed",
        mission::MissionState::Cancelled => "Cancelled",
    }
}

fn gate_state_label(state: mission::GateState) -> &'static str {
    match state {
        mission::GateState::Pending => "Pending",
        mission::GateState::Approved => "Approved",
        mission::GateState::Rejected => "Rejected",
    }
}

/// Phase 70 — daemon-side proposal resolution handler. On
/// `Approve` / `ApproveWithEdit` it validates the applied op,
/// appends a `PersonaDelta` to the persona chain, then appends
/// an `Approved` entry to the proposal chain bound to the
/// delta's seq, and recomputes the shared persona snapshot. On
/// `Reject` it just appends a `Rejected` entry.
/// Chapter Accord prevent-at-write — block approving a persona proposal whose
/// NEW facet would contradict the current Soul, unless the operator dismissed
/// that specific pair. Returns `Some(message)` to block, `None` to allow.
/// Best-effort: no LLM, no proposal, not an `AppendList`, or an already-
/// dismissed pair ⇒ `None` (allow). The dismiss set IS the override — reusing
/// Accord's "keep both" so a false positive is never an unescapable lockout.
async fn persona_approve_coherence_block(
    proposal_log: Option<&crate::persona_proposal::PersistentPersonaProposalLog>,
    shared_persona: &crate::persona::SharedEffectivePersona,
    contradiction_llm: Option<&SeedDraftLlm>,
    dismissals: Option<&crate::conflict_dismissals::PersistentConflictDismissals>,
    proposal_id: &str,
    resolution: &crate::daemon_ipc::PersonaProposalResolution,
) -> Option<String> {
    let llm = contradiction_llm?;
    let dismissals = dismissals?;
    // Only Approve / ApproveWithEdit reach the Soul; resolve the op to apply.
    let op = match resolution {
        crate::daemon_ipc::PersonaProposalResolution::ApproveWithEdit { edited_op } => {
            edited_op.clone()
        }
        crate::daemon_ipc::PersonaProposalResolution::Approve => {
            proposal_log?.get(proposal_id)?.proposed_op
        }
        _ => return None, // Reject
    };
    // Only a brand-new list facet can introduce a contradiction.
    let value = match &op.op {
        aivyx_ipc::persona::PersonaDeltaOp::AppendList { value } => value.clone(),
        _ => return None,
    };
    let snapshot = shared_persona.read().ok()?.clone();
    let detector = crate::soul_contradiction::SoulContradictionDetector::new(
        Arc::clone(&llm.provider),
        llm.model.clone(),
    );
    let conflict = detector
        .detect_for_candidate(&snapshot, op.category, &value)
        .await?;
    // Operator already said "keep both" for this pair → allow.
    if dismissals
        .is_soul_dismissed(&conflict.id)
        .await
        .unwrap_or(false)
    {
        return None;
    }
    // Name the EXISTING facet (the side that isn't the candidate).
    let existing = if conflict.a.value == value {
        &conflict.b
    } else {
        &conflict.a
    };
    Some(format!(
        "coherence: approving \"{value}\" would contradict existing {} \"{}\" — {}. \
         Reject it, resolve the existing facet (`aivyx-pa persona resolve {id}`), or \
         accept the tension with `aivyx-pa persona dismiss {id}` then re-approve.",
        existing.category,
        existing.value.trim(),
        conflict.reason.trim(),
        id = conflict.id,
    ))
}

async fn resolve_persona_proposal(
    persona_proposal_log: Option<&crate::persona_proposal::PersistentPersonaProposalLog>,
    persona_log: Option<&crate::persona::PersistentPersonaLog>,
    shared_persona: &crate::persona::SharedEffectivePersona,
    _request_id: &str,
    proposal_id: String,
    resolution: crate::daemon_ipc::PersonaProposalResolution,
) -> Result<crate::daemon_ipc::PersonaProposalResolveSuccess, String> {
    let proposal_log = persona_proposal_log
        .ok_or_else(|| "daemon has no persona proposal log configured".to_string())?;
    let view = proposal_log
        .get(&proposal_id)
        .ok_or_else(|| format!("unknown proposal id `{proposal_id}`"))?;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    match resolution {
        crate::daemon_ipc::PersonaProposalResolution::Reject { reason } => {
            proposal_log
                .append_rejected(proposal_id, now_ms, reason)
                .await
                .map_err(|e| format!("proposal chain append failed: {e}"))?;
            Ok(crate::daemon_ipc::PersonaProposalResolveSuccess {
                proposal_status: "Rejected".into(),
                applied_seq: None,
            })
        }
        crate::daemon_ipc::PersonaProposalResolution::Approve
        | crate::daemon_ipc::PersonaProposalResolution::ApproveWithEdit { .. } => {
            // Resolve the op the operator actually wants applied.
            let applied_op = match &resolution {
                crate::daemon_ipc::PersonaProposalResolution::ApproveWithEdit { edited_op } => {
                    edited_op.clone()
                }
                _ => view.proposed_op.clone(),
            };
            applied_op
                .validate()
                .map_err(|reason| format!("edited op invalid: {reason}"))?;
            // Append to the persona log first; if that fails the
            // proposal stays Pending so the operator can retry.
            let persona_log =
                persona_log.ok_or_else(|| "daemon has no persona log configured".to_string())?;
            let delta_id = format!("pd-approved-{proposal_id}");
            let delta = crate::persona::PersonaDelta {
                delta_id,
                proposed_at_unix_ms: view.proposed_at_unix_ms,
                approved_at_unix_ms: now_ms,
                proposal_id: proposal_id.clone(),
                category: applied_op.category,
                op: applied_op.op.clone(),
            };
            let applied_seq = persona_log
                .append(delta)
                .await
                .map_err(|e| format!("persona chain append failed: {e}"))?;
            // Record the Approved transition on the proposal chain.
            proposal_log
                .append_approved(proposal_id, now_ms, applied_op, applied_seq)
                .await
                .map_err(|e| format!("proposal chain append failed: {e}"))?;
            // Recompute shared persona state so the next turn sees
            // the new effective persona.
            let entries_after = persona_log.entries();
            if !crate::persona::recompute_shared_from_entries(shared_persona, &entries_after) {
                return Err("shared persona state lock poisoned during recompute".into());
            }
            Ok(crate::daemon_ipc::PersonaProposalResolveSuccess {
                proposal_status: "Approved".into(),
                applied_seq: Some(applied_seq),
            })
        }
    }
}

/// Chapter H — whether an escalated turn parks behind an operator gate.
/// Interactive runs park + wait; headless runs (`RejectAndAbort`) never park —
/// the turn finalizes `Escalated` (a recorded refusal) with no gate.
fn escalation_parks(policy: GatePolicy) -> bool {
    !policy.is_headless()
}

/// Chapter L (L.5) — the `QueryError` returned when a team-mission query hits
/// a daemon with no team service configured (storage absent).
fn no_team_missions() -> QueryResponsePayload {
    QueryResponsePayload::QueryError {
        code: "no_team_missions".into(),
        message: "daemon has no team-mission service configured".into(),
    }
}

/// Chapter Z — resolve a Documents `root` string to its canonical path, or a
/// typed `QueryError` (`bad_root` for an unknown name, `no_filesystem` /
/// `no_workspace` when that root is unavailable).
// The `QueryResponsePayload` Err is large, but returning it by value is the
// file-wide convention for handler results — boxing here would be inconsistent.
#[allow(clippy::result_large_err)]
fn resolve_document_root<'a>(
    roots: &'a DocumentRoots,
    root: &str,
) -> Result<&'a Path, QueryResponsePayload> {
    let err = |code: &str, msg: &str| QueryResponsePayload::QueryError {
        code: code.to_string(),
        message: msg.to_string(),
    };
    match root {
        "fs" => roots
            .fs_root
            .as_deref()
            .ok_or_else(|| err("no_filesystem", "filesystem browsing is unavailable")),
        "workspace" => roots
            .workspace_root
            .as_deref()
            .ok_or_else(|| err("no_workspace", "the agent workspace is disabled")),
        other => Err(err("bad_root", &format!("unknown document root `{other}`"))),
    }
}

/// Chapter Z — map a [`crate::document_browse::BrowseError`] to a stable
/// `QueryError` code for the Documents browser.
/// Chapter DW — shared `FsMutation` result for a Documents write op.
fn fs_mutation_result(
    res: Result<(), crate::document_browse::BrowseError>,
) -> QueryResponsePayload {
    match res {
        Ok(()) => QueryResponsePayload::FsMutation {
            ok: true,
            error: None,
        },
        Err(e) => QueryResponsePayload::FsMutation {
            ok: false,
            error: Some(e.to_string()),
        },
    }
}

/// Chapter DW — append a `DocumentMutated` audit entry for a successful Documents
/// write. Best-effort (a test fixture without an audit log is silently
/// unaudited; an append failure is logged, not fatal — the file change landed).
fn audit_document_mutation(
    audit_log: Option<&PersistentAuditLog>,
    op: &str,
    root: &str,
    path: &str,
) {
    if let Some(log) = audit_log {
        use aivyx_audit::AuditWriter;
        if let Err(e) = log.append(aivyx_audit::AuditEvent::DocumentMutated {
            op: op.to_string(),
            root: root.to_string(),
            path: path.to_string(),
        }) {
            eprintln!("aivyx-pa daemon: failed to audit document mutation: {e}");
        }
    }
}

/// Chapter Chime — build the [`DaemonMessage::ScheduleMutated`] ack.
fn schedule_mutated(id: String, schedule_id: &str, res: Result<(), String>) -> DaemonMessage {
    match res {
        Ok(()) => DaemonMessage::ScheduleMutated {
            id,
            ok: true,
            schedule_id: schedule_id.to_string(),
            error: None,
        },
        Err(e) => DaemonMessage::ScheduleMutated {
            id,
            ok: false,
            schedule_id: schedule_id.to_string(),
            error: Some(e),
        },
    }
}

/// Chapter Chime — best-effort audit of a schedule mutation (the
/// Documents-mutation precedent: a failed append logs, never derails).
fn audit_schedule_mutation(
    audit_log: Option<&PersistentAuditLog>,
    op: &str,
    schedule_id: &str,
    actor: &str,
) {
    if let Some(log) = audit_log {
        use aivyx_audit::AuditWriter;
        if let Err(e) = log.append(aivyx_audit::AuditEvent::ScheduleMutated {
            op: op.to_string(),
            schedule_id: schedule_id.to_string(),
            actor: actor.to_string(),
        }) {
            eprintln!("aivyx-pa daemon: failed to audit schedule mutation: {e}");
        }
    }
}

fn map_browse_error(e: crate::document_browse::BrowseError) -> QueryResponsePayload {
    use crate::document_browse::BrowseError as E;
    let (code, message) = match e {
        E::PathEscape => (
            "path_escape",
            "path is outside the allowed root".to_string(),
        ),
        E::NotFound => ("not_found", "no such file or directory".to_string()),
        E::NotADir => ("not_a_dir", "not a directory".to_string()),
        E::NotAFile => ("not_a_file", "not a file".to_string()),
        E::Exists => (
            "exists",
            "a file or directory with that name already exists".to_string(),
        ),
        E::NotEmpty => ("not_empty", "the directory is not empty".to_string()),
        E::Io(s) => ("io_error", s),
    };
    QueryResponsePayload::QueryError {
        code: code.to_string(),
        message,
    }
}

fn mission_summary_from_record(record: mission::MissionRecord) -> MissionSummary {
    let has_pending_gate = record.pending_gate().is_some();
    MissionSummary {
        mission_id: record.mission_id,
        role_name: record.role_name,
        description: record.description,
        state: mission_state_label(record.state).to_string(),
        has_pending_gate,
        created_at: record.created_at,
        updated_at: record.updated_at,
    }
}

fn mission_detail_from_record(record: mission::MissionRecord) -> MissionDetail {
    let gates = record
        .gates
        .into_iter()
        .map(|g| GateSummary {
            gate_id: g.gate_id,
            reason: g.reason,
            scope: g.scope,
            state: gate_state_label(g.state).to_string(),
            created_at: g.created_at,
            resolved_at: g.resolved_at,
        })
        .collect();
    MissionDetail {
        mission_id: record.mission_id,
        role_name: record.role_name,
        description: record.description,
        state: mission_state_label(record.state).to_string(),
        gates,
        created_at: record.created_at,
        updated_at: record.updated_at,
    }
}

// ---------------------------------------------------------------------------
// IpcChannelBridge — forwards StreamEvents over IPC
// ---------------------------------------------------------------------------

struct IpcChannelBridge {
    inner: Arc<dyn ChannelContext + Send + Sync>,
    writer: Arc<tokio::sync::Mutex<tokio::net::unix::OwnedWriteHalf>>,
    session_id: String,
}

#[async_trait::async_trait]
impl ChannelContext for IpcChannelBridge {
    fn channel_name(&self) -> &str {
        self.inner.channel_name()
    }

    fn platform(&self) -> aivyx_core::ChannelPlatform {
        self.inner.platform()
    }

    fn trust_tier(&self) -> aivyx_capability::TrustTier {
        self.inner.trust_tier()
    }

    fn session_id(&self) -> aivyx_core::SessionId {
        // VITRINE.md §6 P3 — this used to delegate to `self.inner.session_id()`,
        // the wrapped daemon-stub channel's OWN stored session, which is set
        // independently of `self.session_id` (this bridge's copy of `sid`, the
        // exact string the caller already parsed into the turn's
        // `Message.session_id` a few lines above where this bridge is built).
        // TurnStarted reads this method; SkillInvocation reads
        // `Message.session_id` directly — so the two audit events could carry
        // two different session ids for the same turn. Parse the same string
        // the same way, so both sides agree; fall back to the inner channel's
        // id (not a fresh `SessionId::new()`) only if that string somehow
        // isn't a valid UUID, so a parse failure still returns *a* stable id
        // rather than a fresh one on every call.
        self.session_id
            .parse::<uuid::Uuid>()
            .map(aivyx_core::SessionId)
            .unwrap_or_else(|_| self.inner.session_id())
    }

    async fn stream_event(&self, event: StreamEvent<'_>) -> Result<(), aivyx_core::ChannelError> {
        let payload = stream_event_to_payload(&event);
        let msg = DaemonMessage::StreamEvent {
            session_id: self.session_id.clone(),
            event: payload,
        };
        let frame = encode_frame(&msg)
            .map_err(|e| aivyx_core::ChannelError::Send(format!("encode StreamEvent: {e}")))?;
        let mut w = self.writer.lock().await;
        w.write_all(&frame)
            .await
            .map_err(|e| aivyx_core::ChannelError::Send(format!("write StreamEvent: {e}")))?;
        Ok(())
    }

    async fn finalize(&self, _outcome: &TurnOutcome) -> Result<(), aivyx_core::ChannelError> {
        Ok(())
    }

    fn cancellation_token(&self) -> aivyx_core::CancellationToken {
        self.inner.cancellation_token()
    }

    fn reset_cancellation(&self) {
        // Audit H1 fix — the daemon calls this between turns
        // to rotate the channel stub's token. Forwards to the
        // underlying daemon stub (Telegram/Discord/Slack/Web)
        // which holds the actual `Mutex<CancellationToken>`.
        self.inner.reset_cancellation();
    }

    fn cancel_inflight(&self) {
        // Audit C1 fix — the daemon calls this from the
        // `FrontendMessage::CancelTurn` handler. Forwards to
        // the underlying daemon stub.
        self.inner.cancel_inflight();
    }
}

fn stream_event_to_payload(event: &StreamEvent<'_>) -> StreamEventPayload {
    match event {
        StreamEvent::Text(text) => StreamEventPayload::Text {
            text: (*text).to_string(),
        },
        StreamEvent::Status(status) => StreamEventPayload::Status {
            status: (*status).to_string(),
        },
        StreamEvent::ToolCallStarted {
            tool,
            tool_name,
            input,
        } => StreamEventPayload::ToolCallStarted {
            tool_id: tool.to_string(),
            tool_name: (*tool_name).to_string(),
            input: (*input).clone(),
        },
        StreamEvent::ToolCallFinished {
            tool,
            tool_name,
            outcome_summary,
        } => StreamEventPayload::ToolCallFinished {
            tool_id: tool.to_string(),
            tool_name: (*tool_name).to_string(),
            outcome_summary: (*outcome_summary).to_string(),
        },
        StreamEvent::ToolOutput {
            tool,
            tool_name,
            chunk,
        } => StreamEventPayload::ToolOutput {
            tool_id: tool.to_string(),
            tool_name: (*tool_name).to_string(),
            chunk: (*chunk).to_string(),
        },
        StreamEvent::Attachment { .. } => StreamEventPayload::Status {
            status: "[attachment not supported over IPC]".to_string(),
        },
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_disconnects_are_classified_for_quiet_logging() {
        // Backlog #4 — a client hanging up the socket is not a fault.
        use std::io::{Error, ErrorKind};
        for kind in [
            ErrorKind::BrokenPipe,
            ErrorKind::ConnectionReset,
            ErrorKind::UnexpectedEof,
        ] {
            let e = DaemonError::Io(Error::new(kind, "peer gone"));
            assert!(
                e.is_clean_disconnect(),
                "{kind:?} should be a clean disconnect"
            );
        }
        // A genuine I/O fault and non-I/O errors must still log loudly.
        assert!(
            !DaemonError::Io(Error::new(ErrorKind::PermissionDenied, "nope")).is_clean_disconnect()
        );
        assert!(!DaemonError::Protocol("bad handshake".into()).is_clean_disconnect());
    }

    // VITRINE.md §6 P3 — TurnStarted (via ChannelContext::session_id) and
    // SkillInvocation (via Message.session_id) used to be able to disagree
    // on the same turn's session id, since IpcChannelBridge::session_id()
    // delegated to the wrapped inner channel's own, separately-tracked
    // session instead of parsing the same `sid` string the Message's own
    // session_id was built from.
    #[tokio::test]
    async fn ipc_channel_bridge_session_id_matches_the_parsed_sid_not_the_inner_channels() {
        struct StubInner(aivyx_core::SessionId);
        #[async_trait::async_trait]
        impl aivyx_core::ChannelContext for StubInner {
            fn channel_name(&self) -> &str {
                "stub"
            }
            fn platform(&self) -> aivyx_core::ChannelPlatform {
                aivyx_core::ChannelPlatform::Local
            }
            fn trust_tier(&self) -> aivyx_capability::TrustTier {
                aivyx_capability::TrustTier::Trusted
            }
            fn session_id(&self) -> aivyx_core::SessionId {
                self.0
            }
            async fn stream_event(
                &self,
                _e: aivyx_core::StreamEvent<'_>,
            ) -> Result<(), aivyx_core::ChannelError> {
                Ok(())
            }
            async fn finalize(
                &self,
                _o: &aivyx_core::TurnOutcome,
            ) -> Result<(), aivyx_core::ChannelError> {
                Ok(())
            }
            fn cancellation_token(&self) -> aivyx_core::CancellationToken {
                aivyx_core::CancellationToken::new()
            }
        }

        let (a, _b) = tokio::net::UnixStream::pair().expect("socketpair");
        let (_read, write) = a.into_split();

        // Deliberately different from `real_sid` — reproduces the bug's
        // precondition: the inner channel's own session diverges from the
        // sid the current connection's turn actually carries.
        let inner_sid = aivyx_core::SessionId::new();
        let real_sid = aivyx_core::SessionId::new();

        let bridge = IpcChannelBridge {
            inner: Arc::new(StubInner(inner_sid)),
            writer: Arc::new(tokio::sync::Mutex::new(write)),
            session_id: real_sid.to_string(),
        };

        assert_eq!(
            bridge.session_id(),
            real_sid,
            "must match the sid this bridge was built with, not the inner channel's own"
        );
        assert_ne!(bridge.session_id(), inner_sid);
    }

    #[test]
    fn interactive_parks_an_escalation_headless_does_not() {
        // Chapter H — the gate at the escalation point is created only for an
        // interactive run; a headless run never parks.
        assert!(escalation_parks(GatePolicy::Interactive));
        assert!(!escalation_parks(GatePolicy::RejectAndAbort));
    }

    // ---- Piece C (2026-08-23) — ChannelTriggerAuthz / handle_run_team_mission_channel ----

    #[test]
    fn channel_trigger_authorized_checks_the_right_platform_flag() {
        let authz = ChannelTriggerAuthz {
            telegram: true,
            discord: false,
            slack: false,
        };
        assert!(channel_trigger_authorized(
            &authz,
            Some(aivyx_core::ChannelPlatform::Telegram)
        ));
        assert!(!channel_trigger_authorized(
            &authz,
            Some(aivyx_core::ChannelPlatform::Discord)
        ));
        assert!(!channel_trigger_authorized(
            &authz,
            Some(aivyx_core::ChannelPlatform::Slack)
        ));
    }

    #[test]
    fn channel_trigger_authorized_denies_unknown_or_absent_platform() {
        let authz = ChannelTriggerAuthz {
            telegram: true,
            discord: true,
            slack: true,
        };
        // No StartSession yet, or a platform this feature was never
        // designed for (Local/Rest/Voice/...) — always denied, never
        // fail-open.
        assert!(!channel_trigger_authorized(&authz, None));
        assert!(!channel_trigger_authorized(
            &authz,
            Some(aivyx_core::ChannelPlatform::Local)
        ));
    }

    #[test]
    fn channel_trigger_tag_names_the_platform() {
        assert_eq!(
            channel_trigger_tag(Some(aivyx_core::ChannelPlatform::Telegram)),
            "channel:telegram"
        );
        assert_eq!(
            channel_trigger_tag(Some(aivyx_core::ChannelPlatform::Discord)),
            "channel:discord"
        );
        assert_eq!(
            channel_trigger_tag(Some(aivyx_core::ChannelPlatform::Slack)),
            "channel:slack"
        );
        assert_eq!(channel_trigger_tag(None), "channel:unknown");
    }

    #[test]
    fn channel_trigger_audit_platform_is_bare_not_channel_prefixed() {
        // Review finding I1 — the `AuditEvent::TeamMissionChannelTriggered`
        // `platform` field's documented contract (Task 4) is a bare
        // platform name, e.g. `"telegram"`, *not* the `"channel:"`-
        // prefixed `channel_trigger_tag` form used for `triggered_by`.
        // `handle_run_team_mission_channel` calls this helper (not
        // `channel_trigger_tag`) to build the audit event's `platform`
        // field — this test pins that contract at the source.
        assert_eq!(
            channel_trigger_audit_platform(Some(aivyx_core::ChannelPlatform::Telegram)),
            "telegram"
        );
        assert_eq!(
            channel_trigger_audit_platform(Some(aivyx_core::ChannelPlatform::Discord)),
            "discord"
        );
        assert_eq!(
            channel_trigger_audit_platform(Some(aivyx_core::ChannelPlatform::Slack)),
            "slack"
        );
        assert_eq!(channel_trigger_audit_platform(None), "unknown");

        // None of these should ever carry the `"channel:"` prefix —
        // that prefix is exclusively `channel_trigger_tag`'s contract.
        for p in [
            Some(aivyx_core::ChannelPlatform::Telegram),
            Some(aivyx_core::ChannelPlatform::Discord),
            Some(aivyx_core::ChannelPlatform::Slack),
            None,
        ] {
            assert!(!channel_trigger_audit_platform(p).starts_with("channel:"));
        }
    }

    #[tokio::test]
    async fn handle_run_team_mission_channel_denies_before_ever_checking_the_service() {
        // Deliberately checks authorization BEFORE service-presence: an
        // unauthorized channel gets denied even if the daemon has no
        // TeamMissionService at all — `svc: None` here proves the
        // authorization branch never touches `svc`, so this test needs no
        // TeamMissionService fixture (there is no existing lightweight one
        // in this file to build from).
        let authz = ChannelTriggerAuthz::default(); // all false
        let resp = handle_run_team_mission_channel(
            None,
            &authz,
            Some(aivyx_core::ChannelPlatform::Telegram),
            None,
            "close the books".to_string(),
        )
        .await;
        match resp {
            DaemonMessage::Error { code, .. } => assert_eq!(code, "team_run_channel_denied"),
            other => panic!("expected Error(team_run_channel_denied), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn handle_run_team_mission_channel_denial_is_audited() {
        use aivyx_crypto::MasterKey;
        use aivyx_storage::{RedbStorage, StorageConfig};

        let dir = std::env::temp_dir()
            .join(format!("aivyx-team-run-denial-audit-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let storage = RedbStorage::open(
            StorageConfig::new(dir.join("store.redb")),
            MasterKey::from_raw([7u8; 32]),
        )
        .await
        .expect("open storage");
        let log = PersistentAuditLog::open(storage, [9u8; 32])
            .await
            .expect("open chain");

        let authz = ChannelTriggerAuthz::default(); // all false
        let resp = handle_run_team_mission_channel(
            None,
            &authz,
            Some(aivyx_core::ChannelPlatform::Telegram),
            Some(&log),
            "close the books".to_string(),
        )
        .await;
        match resp {
            DaemonMessage::Error { code, .. } => assert_eq!(code, "team_run_channel_denied"),
            other => panic!("expected Error(team_run_channel_denied), got {other:?}"),
        }

        let entries = log.entries().expect("read chain");
        assert_eq!(entries.len(), 1, "the denial must append exactly one entry");
        match &entries[0].event {
            aivyx_audit::AuditEvent::TeamMissionChannelDenied { platform, goal, reason } => {
                assert_eq!(platform, "telegram");
                assert_eq!(goal, "close the books");
                assert!(
                    reason.contains("team_run_channel"),
                    "reason should name the config key an operator needs to set: {reason}"
                );
            }
            other => panic!("expected TeamMissionChannelDenied, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn handle_run_team_mission_channel_reports_no_service_when_authorized_but_absent() {
        let authz = ChannelTriggerAuthz {
            telegram: true,
            discord: false,
            slack: false,
        };
        let resp = handle_run_team_mission_channel(
            None,
            &authz,
            Some(aivyx_core::ChannelPlatform::Telegram),
            None,
            "close the books".to_string(),
        )
        .await;
        match resp {
            DaemonMessage::Error { code, .. } => assert_eq!(code, "no_team_missions"),
            other => panic!("expected Error(no_team_missions), got {other:?}"),
        }
    }

    fn test_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join("aivyx-test-state").join(name);
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    #[test]
    fn load_settings_config_resolves_a_non_default_role_when_overridden() {
        let dir = test_dir("role-override-threading");
        let toml_path = dir.join("aivyx-pa.toml");
        std::fs::write(
            &toml_path,
            r#"
[[role]]
name = "custom"
system_prompt = "You are a custom role."
"#,
        )
        .unwrap();

        // Without the override, active-role resolution defaults to
        // "default", which this config doesn't declare -- UnknownRole.
        let without_override = load_settings_config(&toml_path, None);
        assert!(
            without_override.is_err(),
            "a config with only a non-default-named role must fail to \
             load without an override naming it"
        );

        // With the override, the real bug this task fixes: the daemon's
        // own re-read must resolve against the SAME role the primary
        // load used, not silently fall back to the "default" name.
        let with_override = load_settings_config(&toml_path, Some("custom"));
        assert!(
            with_override.is_ok(),
            "load_settings_config must accept a role_override and use it: {:?}",
            with_override.err()
        );
    }

    #[test]
    fn daemon_state_round_trips_through_json() {
        let state = DaemonState {
            pid: 12345,
            started_at: 1713700000,
            sessions: vec![
                SessionRecord {
                    session_id: "ses-abc".into(),
                    channel: aivyx_core::ChannelPlatform::Local,
                    trust_tier: aivyx_capability::TrustTier::Trusted,
                    created_at_ms: 1713700000000,
                    last_active_at_ms: 1713700000000,
                },
                SessionRecord {
                    session_id: "ses-def".into(),
                    channel: aivyx_core::ChannelPlatform::Telegram,
                    trust_tier: aivyx_capability::TrustTier::SemiTrusted,
                    created_at_ms: 1713700001000,
                    last_active_at_ms: 1713700005000,
                },
            ],
            in_flight_turns: vec!["ses-abc:turn".into()],
        };
        let json = serde_json::to_string(&state).unwrap();
        let parsed: DaemonState = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.sessions.len(), 2);
        assert_eq!(parsed.sessions[0].session_id, "ses-abc");
        assert_eq!(parsed.sessions[0].channel, aivyx_core::ChannelPlatform::Local);
        assert_eq!(parsed.sessions[1].trust_tier, aivyx_capability::TrustTier::SemiTrusted);
        assert_eq!(parsed.sessions[1].last_active_at_ms, 1713700005000);
    }

    #[test]
    fn detect_crash_recovery_returns_none_for_missing_file() {
        let dir = test_dir("crash-missing");
        let path = dir.join("daemon.state");
        let _ = std::fs::remove_file(&path);
        assert!(detect_crash_recovery(&path).is_none());
    }

    #[test]
    fn detect_crash_recovery_returns_state_for_stale_file() {
        let dir = test_dir("crash-stale");
        let path = dir.join("daemon.state");
        let state = DaemonState {
            pid: 99999,
            started_at: 1713700000,
            sessions: vec![
                SessionRecord {
                    session_id: "ses-old".into(),
                    channel: aivyx_core::ChannelPlatform::Local,
                    trust_tier: aivyx_capability::TrustTier::Trusted,
                    created_at_ms: 0,
                    last_active_at_ms: 0,
                },
            ],
            in_flight_turns: vec!["ses-old:turn".into()],
        };
        std::fs::write(&path, serde_json::to_string(&state).unwrap()).unwrap();
        let recovered = detect_crash_recovery(&path).unwrap();
        assert_eq!(recovered.pid, 99999);
        assert_eq!(recovered.sessions.len(), 1);
        assert_eq!(recovered.sessions[0].session_id, "ses-old");
        assert_eq!(recovered.in_flight_turns, vec!["ses-old:turn"]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn detect_crash_recovery_returns_none_for_invalid_json() {
        let dir = test_dir("crash-invalid");
        let path = dir.join("daemon.state");
        std::fs::write(&path, "not valid json").unwrap();
        assert!(detect_crash_recovery(&path).is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn state_guard_creates_and_removes_file() {
        let dir = test_dir("guard-lifecycle");
        let path = dir.join("daemon.state");
        {
            let _guard = StateGuard::write(&path).unwrap();
            assert!(path.exists());
            let contents = std::fs::read_to_string(&path).unwrap();
            let state: DaemonState = serde_json::from_str(&contents).unwrap();
            assert_eq!(state.pid, std::process::id());
            assert!(state.sessions.is_empty());
            assert!(state.in_flight_turns.is_empty());
        }
        // Guard dropped — file should be removed.
        assert!(!path.exists());
    }

    #[test]
    fn state_guard_shared_allows_session_tracking() {
        let dir = test_dir("guard-tracking");
        let path = dir.join("daemon.state");
        let guard = StateGuard::write(&path).unwrap();
        let shared = guard.shared();

        // Register a session.
        let rec = SessionRecord {
            session_id: "ses-1".into(),
            channel: aivyx_core::ChannelPlatform::Local,
            trust_tier: aivyx_capability::TrustTier::Trusted,
            created_at_ms: 0,
            last_active_at_ms: 0,
        };
        shared.lock().unwrap().sessions.push(rec.clone());
        assert_eq!(shared.lock().unwrap().sessions, vec![rec]);

        // Register an in-flight turn.
        shared
            .lock()
            .unwrap()
            .in_flight_turns
            .push("ses-1:turn".into());

        // Complete turn.
        shared
            .lock()
            .unwrap()
            .in_flight_turns
            .retain(|t| t != "ses-1:turn");
        assert!(shared.lock().unwrap().in_flight_turns.is_empty());

        // Deregister session.
        shared.lock().unwrap().sessions.retain(|s| s.session_id != "ses-1");
        assert!(shared.lock().unwrap().sessions.is_empty());

        drop(guard);
        assert!(!path.exists());
    }

    // `submit_input_bumps_last_active_but_not_created_at` (final-review
    // finding 1) was removed here: it built its own local `Vec<SessionRecord>`
    // and re-implemented the `SubmitInput` handler's `iter_mut().find()`
    // logic inline rather than calling it, so it could never fail if the
    // real handler regressed. Its coverage — StartSession really captures
    // channel/trust_tier/timestamps, and a real SubmitInput really bumps
    // last_active_at_ms without moving created_at_ms — is now exercised
    // against the actual daemon over real IPC by
    // `list_sessions_query_round_trips_over_ipc` in
    // `tests/daemon_roundtrip_e2e.rs`.

    // -------------------------------------------------------------
    // Phase 58 — Profile inspection query helpers.
    // -------------------------------------------------------------

    #[test]
    fn profile_summary_renders_default_profile_with_injection_disabled() {
        let summary = profile_summary_from_profile(&aivyx_config::Profile::default());
        assert_eq!(summary.assistant_name, "Aivyx PA");
        assert_eq!(summary.assistant_name_source, "default");
        assert!(summary.operator_profile.is_none());
        assert!(summary.communication_style.is_none());
        assert!(summary.primary_use_cases.is_empty());
        assert!(summary.behavioral_preferences.is_empty());
        assert!(summary.behavioral_constraints.is_empty());
        assert!(!summary.injection_enabled);
    }

    #[test]
    fn profile_summary_renders_operator_declared_profile_with_injection_enabled() {
        let profile = aivyx_config::Profile {
            assistant_name: aivyx_config::Sourced::new(
                "Codex".to_string(),
                aivyx_config::FieldSource::Toml,
            ),
            operator_profile: Some("Senior Rust engineer".to_string()),
            communication_style: Some("terse, conclusion-first".to_string()),
            primary_use_cases: vec!["Rust systems".to_string()],
            behavioral_preferences: vec!["prefer integration tests".to_string()],
            behavioral_constraints: vec!["never auto-commit".to_string()],
        };
        let summary = profile_summary_from_profile(&profile);
        assert_eq!(summary.assistant_name, "Codex");
        assert_eq!(summary.assistant_name_source, "toml");
        assert_eq!(
            summary.operator_profile.as_deref(),
            Some("Senior Rust engineer")
        );
        assert_eq!(
            summary.communication_style.as_deref(),
            Some("terse, conclusion-first"),
        );
        assert_eq!(summary.primary_use_cases, vec!["Rust systems".to_string()]);
        assert_eq!(
            summary.behavioral_preferences,
            vec!["prefer integration tests".to_string()],
        );
        assert_eq!(
            summary.behavioral_constraints,
            vec!["never auto-commit".to_string()],
        );
        assert!(summary.injection_enabled);
    }

    // ---- Phase 70 — resolve_persona_proposal end-to-end -----

    /// Helper: open a fresh persona + proposal log pair backed by
    /// real redb storage so the resolve handler's chain
    /// interactions are exercised against the actual substrate.
    async fn open_phase_70_test_logs(
        slug: &str,
    ) -> (
        Arc<crate::persona::PersistentPersonaLog>,
        Arc<crate::persona_proposal::PersistentPersonaProposalLog>,
        crate::persona::SharedEffectivePersona,
    ) {
        use aivyx_crypto::MasterKey;
        use aivyx_storage::{KeyDomain, RedbStorage, Storage, StorageConfig};
        // Per-test slug + a high-res timestamp keeps every test's
        // tempdir distinct under parallel execution. redb refuses
        // two opens of the same file (`Database already open`),
        // so collisions surface as the test panicking on storage
        // open.
        let dir = test_dir(&format!(
            "phase-70-resolve-{slug}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let store: Arc<dyn Storage> = RedbStorage::open(
            StorageConfig::new(dir.join("store.redb")),
            MasterKey::from_raw([70u8; 32]),
        )
        .await
        .expect("storage");
        let persona_log = Arc::new(
            crate::persona::PersistentPersonaLog::open(
                store.domain(KeyDomain::Persona),
                b"persona-key".to_vec(),
            )
            .await
            .expect("persona log"),
        );
        let proposal_log = Arc::new(
            crate::persona_proposal::PersistentPersonaProposalLog::open(
                store.domain(KeyDomain::PersonaProposals),
                b"proposal-key".to_vec(),
            )
            .await
            .expect("proposal log"),
        );
        let shared =
            crate::persona::shared_effective_persona(crate::persona::EffectivePersona::default());
        (persona_log, proposal_log, shared)
    }

    fn pending_op_fixture() -> crate::persona::ProposedPersonaDelta {
        crate::persona::ProposedPersonaDelta {
            category: crate::persona::PersonaDeltaCategory::BehavioralPreferences,
            op: crate::persona::PersonaDeltaOp::AppendList {
                value: "prefer terse".into(),
            },
            reason: Some("operator confirmed".into()),
            supersedes_proposal_id: None,
        }
    }

    #[tokio::test]
    async fn resolve_proposal_approve_appends_to_persona_log_and_records_approved() {
        let (persona_log, proposal_log, shared) = open_phase_70_test_logs("approve").await;
        proposal_log
            .append_pending("pp-1".into(), 1_000, "ses-1".into(), pending_op_fixture())
            .await
            .unwrap();
        let success = resolve_persona_proposal(
            Some(proposal_log.as_ref()),
            Some(persona_log.as_ref()),
            &shared,
            "req-1",
            "pp-1".into(),
            crate::daemon_ipc::PersonaProposalResolution::Approve,
        )
        .await
        .expect("approve ok");
        assert_eq!(success.proposal_status, "Approved");
        assert_eq!(success.applied_seq, Some(0));
        // Persona chain has the applied delta.
        assert_eq!(persona_log.len(), 1);
        // Proposal chain now reports Approved status.
        let view = proposal_log.get("pp-1").expect("present");
        assert!(matches!(
            view.status,
            crate::persona_proposal::ProposalStatus::Approved { applied_seq: 0, .. }
        ));
        // Shared persona state reflects the approved op.
        let snap = shared.read().unwrap();
        assert!(
            snap.behavioral_preferences
                .contains(&"prefer terse".to_string())
        );
    }

    // ---- Chapter Accord prevent-at-write — approve coherence gate ----------

    struct AccordFakeStream(Option<String>);
    #[async_trait::async_trait]
    impl aivyx_llm::LlmStream for AccordFakeStream {
        async fn next_event(
            &mut self,
        ) -> Result<Option<aivyx_llm::LlmStreamEvent>, aivyx_llm::LlmError> {
            Ok(None)
        }
        async fn finish(self: Box<Self>) -> Result<aivyx_llm::LlmStepEnd, aivyx_llm::LlmError> {
            Ok(aivyx_llm::LlmStepEnd::FinalMessage {
                text: self.0.unwrap_or_default(),
                usage: aivyx_llm::LlmUsage::default(),
            })
        }
    }
    struct AccordFakeProvider(&'static str);
    #[async_trait::async_trait]
    impl aivyx_llm::LlmProvider for AccordFakeProvider {
        async fn chat_stream(
            &self,
            _req: aivyx_llm::LlmRequest<'_>,
            _cancel: &aivyx_core::CancellationToken,
        ) -> Result<Box<dyn aivyx_llm::LlmStream>, aivyx_llm::LlmError> {
            Ok(Box::new(AccordFakeStream(Some(self.0.to_string()))))
        }
    }

    #[tokio::test]
    async fn approve_gate_blocks_contradiction_then_dismiss_overrides() {
        let (_persona_log, proposal_log, _shared) = open_phase_70_test_logs("accord-gate").await;
        // Existing Soul facet: "communicate concisely".
        let shared = crate::persona::shared_effective_persona(crate::persona::EffectivePersona {
            character_traits: vec!["communicate concisely".into()],
            ..Default::default()
        });
        // Pending proposal: append a contradicting facet.
        let candidate = "always give long, elaborate explanations";
        proposal_log
            .append_pending(
                "pp-x".into(),
                1_000,
                "ses".into(),
                crate::persona::ProposedPersonaDelta {
                    category: crate::persona::PersonaDeltaCategory::CharacterTraits,
                    op: crate::persona::PersonaDeltaOp::AppendList {
                        value: candidate.into(),
                    },
                    reason: None,
                    supersedes_proposal_id: None,
                },
            )
            .await
            .unwrap();
        // Fake judge: items are [0]="communicate concisely", [1]=candidate.
        let llm = SeedDraftLlm {
            provider: Arc::new(AccordFakeProvider(
                "[{\"a\":0,\"b\":1,\"reason\":\"concise vs elaborate\"}]",
            )),
            model: "test".into(),
        };
        // Dismissals store.
        let dir = std::env::temp_dir().join(format!("aivyx-accord-gate-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let dstore: Arc<dyn aivyx_storage::Storage> = aivyx_storage::RedbStorage::open(
            aivyx_storage::StorageConfig::new(dir.join("d.redb")),
            aivyx_crypto::MasterKey::from_raw([9u8; 32]),
        )
        .await
        .unwrap();
        let dismissals = crate::conflict_dismissals::PersistentConflictDismissals::new(
            dstore.domain(aivyx_storage::KeyDomain::ConflictDismissals),
        );

        // 1) The gate BLOCKS the contradicting approve.
        let block = persona_approve_coherence_block(
            Some(proposal_log.as_ref()),
            &shared,
            Some(&llm),
            Some(&dismissals),
            "pp-x",
            &crate::daemon_ipc::PersonaProposalResolution::Approve,
        )
        .await;
        let msg = block.expect("contradiction must block approval");
        assert!(msg.contains("coherence"), "{msg}");
        assert!(
            msg.contains("communicate concisely"),
            "names the existing facet: {msg}"
        );

        // 2) Dismiss that pair → the gate now ALLOWS (override via keep-both).
        let snap_for_id = { shared.read().unwrap().clone() };
        let conflict = crate::soul_contradiction::SoulContradictionDetector::new(
            Arc::new(AccordFakeProvider("[{\"a\":0,\"b\":1,\"reason\":\"x\"}]")),
            "test",
        )
        .detect_for_candidate(
            &snap_for_id,
            crate::persona::PersonaDeltaCategory::CharacterTraits,
            candidate,
        )
        .await
        .expect("detector finds the candidate conflict");
        dismissals.dismiss_soul(&conflict.id, 1).await.unwrap();
        let after = persona_approve_coherence_block(
            Some(proposal_log.as_ref()),
            &shared,
            Some(&llm),
            Some(&dismissals),
            "pp-x",
            &crate::daemon_ipc::PersonaProposalResolution::Approve,
        )
        .await;
        assert!(
            after.is_none(),
            "a dismissed pair must not block re-approval"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn resolve_proposal_approve_with_edit_records_edited_op() {
        let (persona_log, proposal_log, shared) =
            open_phase_70_test_logs("approve-with-edit").await;
        proposal_log
            .append_pending("pp-1".into(), 1_000, "ses-1".into(), pending_op_fixture())
            .await
            .unwrap();
        let edited = crate::persona::ProposedPersonaDelta {
            category: crate::persona::PersonaDeltaCategory::BehavioralPreferences,
            op: crate::persona::PersonaDeltaOp::AppendList {
                value: "operator-edited preference".into(),
            },
            reason: None,
            supersedes_proposal_id: None,
        };
        resolve_persona_proposal(
            Some(proposal_log.as_ref()),
            Some(persona_log.as_ref()),
            &shared,
            "req-2",
            "pp-1".into(),
            crate::daemon_ipc::PersonaProposalResolution::ApproveWithEdit {
                edited_op: edited.clone(),
            },
        )
        .await
        .expect("approve-with-edit ok");
        // Shared persona reflects the EDITED op, not the original.
        let snap = shared.read().unwrap();
        assert!(
            snap.behavioral_preferences
                .contains(&"operator-edited preference".to_string())
        );
        assert!(
            !snap
                .behavioral_preferences
                .contains(&"prefer terse".to_string())
        );
    }

    #[tokio::test]
    async fn resolve_proposal_reject_records_rejected_no_persona_append() {
        let (persona_log, proposal_log, shared) = open_phase_70_test_logs("reject").await;
        proposal_log
            .append_pending("pp-1".into(), 1_000, "ses-1".into(), pending_op_fixture())
            .await
            .unwrap();
        let success = resolve_persona_proposal(
            Some(proposal_log.as_ref()),
            Some(persona_log.as_ref()),
            &shared,
            "req-3",
            "pp-1".into(),
            crate::daemon_ipc::PersonaProposalResolution::Reject {
                reason: Some("not now".into()),
            },
        )
        .await
        .expect("reject ok");
        assert_eq!(success.proposal_status, "Rejected");
        assert_eq!(success.applied_seq, None);
        // Persona chain UNCHANGED.
        assert!(persona_log.is_empty());
        let view = proposal_log.get("pp-1").unwrap();
        match view.status {
            crate::persona_proposal::ProposalStatus::Rejected { reason, .. } => {
                assert_eq!(reason.as_deref(), Some("not now"));
            }
            other => panic!("expected Rejected, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn resolve_proposal_unknown_id_returns_error() {
        let (persona_log, proposal_log, shared) = open_phase_70_test_logs("unknown-id").await;
        let err = resolve_persona_proposal(
            Some(proposal_log.as_ref()),
            Some(persona_log.as_ref()),
            &shared,
            "req-4",
            "pp-MISSING".into(),
            crate::daemon_ipc::PersonaProposalResolution::Approve,
        )
        .await
        .expect_err("must error");
        assert!(err.contains("pp-MISSING"), "{err}");
    }

    #[test]
    fn parse_proposal_status_filter_handles_known_and_unknown() {
        use crate::persona_proposal::ProposalStatusFilter;
        assert!(matches!(
            parse_proposal_status_filter("all"),
            ProposalStatusFilter::All
        ));
        assert!(matches!(
            parse_proposal_status_filter("Approved"),
            ProposalStatusFilter::Approved
        ));
        assert!(matches!(
            parse_proposal_status_filter("REJECTED"),
            ProposalStatusFilter::Rejected
        ));
        assert!(matches!(
            parse_proposal_status_filter("superseded"),
            ProposalStatusFilter::Superseded
        ));
        // Unknown / empty → Pending per IPC contract.
        assert!(matches!(
            parse_proposal_status_filter("xyz"),
            ProposalStatusFilter::Pending
        ));
        assert!(matches!(
            parse_proposal_status_filter(""),
            ProposalStatusFilter::Pending
        ));
    }

    // ---- Phase 73 — notify-outcome history renderer ----------

    #[test]
    fn history_renderer_delivered_has_empty_detail() {
        let (kind, detail) =
            render_notify_outcome_for_history(&aivyx_audit::AutoNotifyOutcomeSummary::Delivered);
        assert_eq!(kind, "delivered");
        assert!(detail.is_empty());
    }

    #[test]
    fn history_renderer_failed_carries_error_kind_and_message() {
        let (kind, detail) =
            render_notify_outcome_for_history(&aivyx_audit::AutoNotifyOutcomeSummary::Failed {
                error_kind: "transport".into(),
                error_message: "dns lookup failed".into(),
            });
        assert_eq!(kind, "failed");
        assert!(detail.contains("transport"), "{detail}");
        assert!(detail.contains("dns lookup failed"), "{detail}");
    }

    #[test]
    fn history_renderer_skipped_empty_response_has_empty_detail() {
        let (kind, detail) = render_notify_outcome_for_history(
            &aivyx_audit::AutoNotifyOutcomeSummary::SkippedEmptyResponse,
        );
        assert_eq!(kind, "skipped_empty_response");
        assert!(detail.is_empty());
    }

    #[test]
    fn history_renderer_skipped_by_condition_carries_label() {
        let (kind, detail) = render_notify_outcome_for_history(
            &aivyx_audit::AutoNotifyOutcomeSummary::SkippedByCondition {
                condition: "on_failed".into(),
            },
        );
        assert_eq!(kind, "skipped_by_condition");
        assert_eq!(detail, "on_failed");
    }

    #[test]
    fn history_renderer_skipped_by_rate_limit_renders_limit_and_window() {
        let (kind, detail) = render_notify_outcome_for_history(
            &aivyx_audit::AutoNotifyOutcomeSummary::SkippedByRateLimit {
                limit: 10,
                window_secs: 3600,
            },
        );
        assert_eq!(kind, "skipped_by_rate_limit");
        assert_eq!(detail, "10/3600s");
    }

    // ---- Phase 102: fold_tool_stats ---------------------------------

    fn completed_outcome() -> aivyx_core::ToolOutcomeSummary {
        aivyx_core::ToolOutcomeSummary::Completed {
            verified: aivyx_core::VerificationSummary::NotApplicable,
        }
    }

    fn tc_entry(
        seq: u64,
        scope: &str,
        outcome: aivyx_core::ToolOutcomeSummary,
        duration_ms: u64,
        appended_at: std::time::SystemTime,
    ) -> aivyx_audit::SignedEntry {
        aivyx_audit::SignedEntry {
            seq,
            appended_at,
            event: aivyx_audit::AuditEvent::ToolCall {
                turn_id: aivyx_core::TurnId::new(),
                tool_id: aivyx_core::ToolId::new(),
                scope_used: aivyx_capability::Scope::parse(scope).unwrap(),
                input_hash: [0u8; 32],
                outcome,
                duration: std::time::Duration::from_millis(duration_ms),
                auto_corrected_from: None,
                extracted_from_text: None,
            },
            mac: [0u8; 32],
            prev_mac: [0u8; 32],
        }
    }

    fn desc(name: &str, scope_base: &str) -> ToolDescriptor {
        ToolDescriptor {
            name: name.to_string(),
            description: format!("{name} tool"),
            scope_base: scope_base.to_string(),
        }
    }

    #[test]
    fn build_tool_catalog_derives_min_tier_per_scope_base() {
        let descs = vec![
            desc("fs.read", "fs.read"),
            desc("fs.metadata", "fs.metadata"),
            desc("role.switch", "role.switch"),
        ];
        let rows = build_tool_catalog(&descs);
        assert_eq!(rows.len(), 3);
        let tier_of = |name: &str| {
            rows.iter()
                .find(|r| r.name == name)
                .map(|r| r.min_tier)
                .unwrap()
        };
        assert_eq!(tier_of("fs.read"), aivyx_capability::TrustTier::Trusted);
        assert_eq!(
            tier_of("fs.metadata"),
            aivyx_capability::TrustTier::SemiTrusted
        );
        // Chapter Almanac's own finding: role.switch is Trusted-tier per
        // the ceiling code, not Kernel (docs/TOOLS.md corrected to match).
        assert_eq!(tier_of("role.switch"), aivyx_capability::TrustTier::Trusted);
    }

    #[test]
    fn fold_empty_chain_lists_registered_tools_with_zero_stats() {
        let descs = vec![desc("fs.read", "fs.read"), desc("fs.write", "fs.write")];
        let rows = fold_tool_stats(&[], None, &descs);
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.registered && r.calls == 0));
    }

    #[test]
    fn fold_counts_calls_and_outcomes_by_scope_base() {
        let now = std::time::SystemTime::now();
        let entries = vec![
            tc_entry(0, "fs.read:/x/**", completed_outcome(), 10, now),
            tc_entry(1, "fs.read:/y/**", completed_outcome(), 20, now),
            tc_entry(
                2,
                "fs.read:/z/**",
                aivyx_core::ToolOutcomeSummary::Failed,
                6,
                now,
            ),
        ];
        let descs = vec![desc("fs.read", "fs.read")];
        let rows = fold_tool_stats(&entries, None, &descs);
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.calls, 3);
        assert_eq!(r.scope_base, "fs.read");
        assert_eq!(r.outcomes.get("completed"), Some(&2));
        assert_eq!(r.outcomes.get("failed"), Some(&1));
        assert_eq!(r.total_duration_ms, 36);
    }

    #[test]
    fn fold_window_filter_excludes_entries_before_the_cutoff() {
        let now = std::time::SystemTime::now();
        let old = now - std::time::Duration::from_secs(7200);
        let entries = vec![
            tc_entry(0, "fs.read:/a/**", completed_outcome(), 5, old),
            tc_entry(1, "fs.read:/b/**", completed_outcome(), 5, now),
        ];
        let descs = vec![desc("fs.read", "fs.read")];
        // Cutoff one hour ago — the two-hour-old entry is excluded.
        let cutoff = now - std::time::Duration::from_secs(3600);
        let rows = fold_tool_stats(&entries, Some(cutoff), &descs);
        assert_eq!(rows[0].calls, 1, "only the in-window call counts");
    }

    #[test]
    fn fold_called_but_unregistered_base_gets_an_unregistered_row() {
        let now = std::time::SystemTime::now();
        let entries = vec![tc_entry(
            0,
            "shell.exec:cwd:/x/**",
            completed_outcome(),
            9,
            now,
        )];
        // No descriptor for shell.exec — only fs.read is registered.
        let descs = vec![desc("fs.read", "fs.read")];
        let rows = fold_tool_stats(&entries, None, &descs);
        let shell = rows
            .iter()
            .find(|r| r.scope_base == "shell.exec")
            .expect("a called base with no descriptor must still get a row");
        assert!(!shell.registered);
        assert_eq!(shell.calls, 1);
    }

    #[test]
    fn fold_mcp_server_stats_buckets_by_server_not_by_shared_base() {
        let now = std::time::SystemTime::now();
        let entries = vec![
            tc_entry(0, "mcp.call:comfyui:generate_image", completed_outcome(), 10, now),
            tc_entry(
                1,
                "mcp.call:duckduckgo-search:search",
                aivyx_core::ToolOutcomeSummary::Failed,
                10,
                now,
            ),
            tc_entry(
                2,
                "mcp.call:duckduckgo-search:search",
                aivyx_core::ToolOutcomeSummary::Failed,
                10,
                now,
            ),
        ];
        let servers = fold_mcp_server_stats(&entries, None);
        assert_eq!(servers.len(), 2, "two distinct servers, not one shared mcp.call bucket");
        let ddg = servers.iter().find(|s| s.server_name == "duckduckgo-search").unwrap();
        assert_eq!(ddg.calls, 2);
        assert_eq!(ddg.outcomes.get("failed"), Some(&2));
        let comfy = servers.iter().find(|s| s.server_name == "comfyui").unwrap();
        assert_eq!(comfy.calls, 1);
        assert_eq!(comfy.outcomes.get("completed"), Some(&1));
    }

    #[test]
    fn fold_mcp_server_stats_ignores_non_mcp_tool_calls() {
        let now = std::time::SystemTime::now();
        let entries = vec![tc_entry(0, "fs.read", completed_outcome(), 5, now)];
        let servers = fold_mcp_server_stats(&entries, None);
        assert!(servers.is_empty(), "a non-mcp.call entry must not produce a row");
    }

    #[test]
    fn fold_mcp_server_stats_respects_the_cutoff() {
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        let recent = std::time::SystemTime::now();
        let entries = vec![
            tc_entry(0, "mcp.call:comfyui:generate_image", completed_outcome(), 10, old),
            tc_entry(1, "mcp.call:comfyui:generate_image", completed_outcome(), 10, recent),
        ];
        let cutoff = recent - std::time::Duration::from_secs(60);
        let servers = fold_mcp_server_stats(&entries, Some(cutoff));
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].calls, 1, "the entry before cutoff must be excluded");
    }

    #[test]
    fn fold_mcp_server_stats_splits_a_colon_containing_tool_name_from_the_left() {
        let now = std::time::SystemTime::now();
        // The tool name (last segment) is remote-controlled -- an MCP
        // server's own tools/list response, not sanitized anywhere in
        // this codebase -- so it could contain a colon. The server name
        // (first segment) is what this function must recover correctly;
        // splitting from the left does that regardless of what the tool
        // name contains.
        let entries = vec![tc_entry(0, "mcp.call:web-search:ns:search", completed_outcome(), 1, now)];
        let servers = fold_mcp_server_stats(&entries, None);
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].server_name, "web-search");
    }

    // -- Phase 186 GetReminders -------------------------------------------

    async fn open_reminder_store() -> crate::reminder_tool::SharedReminderStore {
        // Mirrors `reminder_store.rs`'s own `open_store()` test fixture.
        let dir = std::env::temp_dir()
            .join(format!("aivyx-dashboard-reminders-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let storage: Arc<dyn aivyx_storage::Storage> = aivyx_storage::RedbStorage::open(
            aivyx_storage::StorageConfig::new(dir.join("store.redb")),
            aivyx_crypto::MasterKey::from_raw([201u8; 32]),
        )
        .await
        .unwrap();
        Arc::new(crate::reminder_store::ReminderStore::new(
            storage.domain(aivyx_storage::KeyDomain::Reminders),
        ))
    }

    #[tokio::test]
    async fn reminders_query_response_lists_pending_soonest_first() {
        let store = open_reminder_store().await;
        store
            .set(&crate::reminder_store::Reminder {
                id: "r1".into(),
                due_unix: 300,
                message: "call mom".into(),
                notify_targets: vec![],
                created_unix: 0,
            })
            .await
            .unwrap();
        store
            .set(&crate::reminder_store::Reminder {
                id: "r2".into(),
                due_unix: 100,
                message: "standup".into(),
                notify_targets: vec!["telegram:123".into()],
                created_unix: 0,
            })
            .await
            .unwrap();

        let resp = reminders_query_response(Some(&store)).await;
        let QueryResponsePayload::Reminders { reminders } = resp else {
            panic!("expected Reminders, got {resp:?}");
        };
        assert_eq!(reminders.len(), 2);
        assert_eq!(reminders[0].id, "r2"); // due 100, soonest first
        assert_eq!(reminders[0].notify_targets, vec!["telegram:123".to_string()]);
        assert_eq!(reminders[1].message, "call mom");
    }

    #[tokio::test]
    async fn reminders_query_response_none_store_is_empty_not_an_error() {
        let resp = reminders_query_response(None).await;
        let QueryResponsePayload::Reminders { reminders } = resp else {
            panic!("expected Reminders, got {resp:?}");
        };
        assert!(reminders.is_empty());
    }

    // -- Chapter U Settings handlers --------------------------------------

    fn settings_toml(name: &str, body: &str) -> PathBuf {
        let dir = test_dir(name);
        let path = dir.join("aivyx-pa.toml");
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn settings_snapshot_reflects_the_loaded_config() {
        // A populated config loads into a faithful snapshot. (Ollama needs no
        // api key, so inspection-mode load succeeds offline.)
        let path = settings_toml(
            "settings-snapshot",
            "[agent]\nprovider = \"ollama\"\nmodel = \"qwen3:8b\"\n\
             [access]\nlevel = \"home\"\n\
             [budget]\nper_run_usd = 5.0\non_exceeded = \"deny\"\nalert_at = 0.8\n\
             [ollama]\nnum_ctx = 16384\n",
        );
        let cfg = load_settings_config(&path, None).expect("load");
        let snap = settings_snapshot(&cfg, true);
        assert_eq!(snap.access_level, "home");
        assert_eq!(snap.provider, "ollama");
        assert_eq!(snap.model, "qwen3:8b");
        assert_eq!(snap.num_ctx, Some(16384));
        assert!(snap.confirm_destructive, "home is an expanded level");
        assert_eq!(snap.budget.per_run_usd, Some(5.0));
        assert_eq!(snap.budget.on_exceeded, "deny");
        assert!(snap.embeddings_available);
    }

    #[test]
    fn voice_snapshot_reflects_config_and_readiness() {
        // A real Whisper model file (present) + a Kokoro model dir that holds a
        // `.onnx` (model present) but no `voices-*.bin` (voices missing).
        let dir = test_dir("voice-snapshot");
        let model = dir.join("whisper.bin");
        std::fs::write(&model, b"x").unwrap();
        let kokoro_dir = dir.join("kokoro");
        std::fs::create_dir_all(&kokoro_dir).unwrap();
        std::fs::write(kokoro_dir.join("kokoro.onnx"), b"x").unwrap();
        let path = settings_toml(
            "voice-snapshot",
            &format!(
                "[agent]\nprovider = \"ollama\"\nmodel = \"qwen3:8b\"\n\
                 [voice]\nasr_engine = \"whisper-rs\"\nasr_model_path = \"{}\"\n\
                 asr_beam_size = 5\ntts_engine = \"kokoro\"\ntts_model_dir = \"{}\"\n",
                model.display(),
                kokoro_dir.display(),
            ),
        );
        let cfg = load_settings_config(&path, None).expect("load");
        let snap = voice_snapshot(&cfg);
        assert_eq!(snap.asr_engine.as_deref(), Some("whisper-rs"));
        assert_eq!(snap.asr_beam_size, Some(5));
        assert_eq!(snap.asr_model_status, "present", "model file exists");
        assert_eq!(
            snap.tts_model_status, "present",
            "kokoro .onnx present in dir"
        );
        assert_eq!(snap.tts_voices_status, "missing", "no voices-*.bin in dir");
    }

    #[tokio::test]
    async fn build_memory_graph_makes_nodes_with_counts_and_no_edges_without_a_ledger() {
        use aivyx_memory::{InMemoryMemory, Memory};
        let mem = InMemoryMemory::new();
        mem.put("rust", "borrow checker note").await.unwrap();
        mem.put("rust", "lifetimes note").await.unwrap();
        mem.put("ops", "deploy runbook").await.unwrap();
        // #D — an internal prune-bookkeeping topic must not appear as a node.
        mem.put("context:pruned:abc-123", "23 messages pruned")
            .await
            .unwrap();

        let (nodes, edges) = build_memory_graph(&mem, None, 40).await.unwrap();
        assert!(edges.is_empty(), "no ledger ⇒ a topic cloud (no edges)");
        let rust = nodes.iter().find(|n| n.topic == "rust").expect("rust node");
        assert_eq!(rust.entry_count, 2);
        let ops = nodes.iter().find(|n| n.topic == "ops").expect("ops node");
        assert_eq!(ops.entry_count, 1);
        assert!(
            !nodes.iter().any(|n| n.topic.starts_with("context:pruned:")),
            "internal prune topics must be filtered from the graph cloud"
        );
    }

    #[test]
    fn voice_change_summary_records_set_cleared_shape() {
        let w = aivyx_config::VoiceWrite {
            asr_engine: Some("whisper-rs".into()),
            asr_model_path: Some("  ".into()), // blank → cleared
            asr_beam_size: Some(5),
            ..Default::default()
        };
        let s = voice_change_summary(&w);
        assert!(s.contains("asr_engine = set"), "{s}");
        assert!(s.contains("asr_model_path = cleared"), "{s}");
        assert!(s.contains("asr_beam_size = set"), "{s}");
        assert!(s.contains("tts_model_dir = cleared"), "{s}");
        assert!(
            !s.contains("whisper-rs"),
            "summary must not carry values: {s}"
        );
    }

    #[test]
    fn config_write_errors_map_to_stable_codes() {
        let cases = [
            (
                aivyx_config::ConfigWriteError::RootRequired {
                    level: aivyx_config::AccessLevel::Workspace,
                },
                "root_required",
            ),
            (
                aivyx_config::ConfigWriteError::InvalidBudget { reason: "x".into() },
                "invalid_budget",
            ),
            (
                aivyx_config::ConfigWriteError::Io { reason: "x".into() },
                "config_write_failed",
            ),
        ];
        for (err, want) in cases {
            match map_config_write_error(err) {
                QueryResponsePayload::QueryError { code, .. } => assert_eq!(code, want),
                other => panic!("expected QueryError, got {other:?}"),
            }
        }
    }

    #[test]
    fn provider_and_budget_labels_match_the_wire_repr() {
        use aivyx_config::ProviderKind;
        assert_eq!(provider_label(ProviderKind::Anthropic), "anthropic");
        assert_eq!(provider_label(ProviderKind::Ollama), "ollama");
        assert_eq!(provider_label(ProviderKind::MistralRs), "mistralrs");
        assert_eq!(provider_label(ProviderKind::Broker), "broker");
        assert_eq!(budget_action_label(aivyx_cost::BudgetAction::Deny), "deny");
        assert_eq!(
            budget_action_label(aivyx_cost::BudgetAction::Alert),
            "alert"
        );
    }

    #[test]
    fn no_config_file_error_is_typed() {
        match no_config_file_error() {
            QueryResponsePayload::QueryError { code, .. } => {
                assert_eq!(code, "no_config_file")
            }
            other => panic!("expected QueryError, got {other:?}"),
        }
    }

    #[test]
    fn opt_renderers_handle_none() {
        assert_eq!(opt_usd(None), "none");
        assert_eq!(opt_usd(Some(5.0)), "5");
        assert_eq!(opt_frac(None), "none");
        assert_eq!(opt_frac(Some(0.8)), "0.8");
    }

    #[test]
    fn profile_change_summary_records_shape_not_values() {
        let w = aivyx_config::ProfileWrite {
            assistant_name: Some("Aria".into()),
            operator_profile: Some("  ".into()), // whitespace → cleared
            communication_style: None,
            primary_use_cases: Some(vec!["coding".into(), "  ".into(), "ops".into()]),
            behavioral_preferences: Some(vec![]),
            behavioral_constraints: None,
        };
        let s = profile_change_summary(&w);
        assert_eq!(
            s,
            "assistant_name = set, operator_profile = cleared, \
             communication_style = cleared, primary_use_cases = 2, \
             behavioral_preferences = 0, behavioral_constraints = cleared"
        );
        // The declared value must never leak into the audit summary.
        assert!(
            !s.contains("Aria"),
            "summary must not carry profile prose: {s}"
        );
        assert!(
            !s.contains("coding"),
            "summary must not carry list values: {s}"
        );
    }

    #[test]
    fn wire_to_persona_seed_maps_fields_and_skills() {
        let wire = aivyx_ipc::protocol::PersonaSeedWire {
            learned_context: vec!["ctx".into()],
            communication_adaptations: vec!["adapt".into()],
            character_traits: vec!["pragmatic".into()],
            relationship_milestones: vec!["genesis".into()],
            skills: vec![aivyx_ipc::protocol::SeedSkillWire {
                name: "rust-review".into(),
                trigger: "t".into(),
                procedure: "p".into(),
            }],
        };
        let seed = wire_to_persona_seed(wire);
        assert_eq!(seed.learned_context, vec!["ctx"]);
        assert_eq!(seed.communication_adaptations, vec!["adapt"]);
        assert_eq!(seed.character_traits, vec!["pragmatic"]);
        assert_eq!(seed.relationship_milestones, vec!["genesis"]);
        assert_eq!(seed.skills.len(), 1);
        assert_eq!(seed.skills[0].name, "rust-review");

        // The inverse mapping (drafter → wire) round-trips.
        let back = persona_seed_to_wire(seed);
        assert_eq!(back.character_traits, vec!["pragmatic"]);
        assert_eq!(back.skills.len(), 1);
        assert_eq!(back.skills[0].name, "rust-review");
    }

    #[tokio::test]
    async fn seed_persona_live_seeds_then_refuses_and_rejects_empty() {
        use aivyx_crypto::MasterKey;
        use aivyx_storage::{KeyDomain, RedbStorage, Storage, StorageConfig};
        let dir = test_dir(&format!(
            "x1-seed-live-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let store: Arc<dyn Storage> = RedbStorage::open(
            StorageConfig::new(dir.join("store.redb")),
            MasterKey::from_raw([9u8; 32]),
        )
        .await
        .expect("storage");
        let log = crate::persona::PersistentPersonaLog::open(
            store.domain(KeyDomain::Persona),
            b"persona-key".to_vec(),
        )
        .await
        .expect("persona log");
        let shared =
            crate::persona::shared_effective_persona(crate::persona::EffectivePersona::default());

        let wire = aivyx_ipc::protocol::PersonaSeedWire {
            character_traits: vec!["pragmatic".into(), "precise".into()],
            ..Default::default()
        };

        // Empty seed on a fresh chain → "nothing to seed".
        let empty = seed_persona_live(
            Some(&log),
            &shared,
            None,
            aivyx_ipc::protocol::PersonaSeedWire::default(),
        )
        .await;
        assert!(empty.is_err(), "empty seed must error");

        // First real seed → plants 2 deltas + adopts.
        let n = seed_persona_live(Some(&log), &shared, None, wire.clone())
            .await
            .expect("seed ok");
        assert_eq!(n, 2);
        assert!(
            shared
                .read()
                .unwrap()
                .character_traits
                .contains(&"precise".to_string())
        );

        // Second seed on the now-non-empty chain → refused.
        let again = seed_persona_live(Some(&log), &shared, None, wire).await;
        assert!(again.is_err(), "must refuse seeding a non-empty chain");
        assert_eq!(log.len(), 2, "chain unchanged after refusal");
    }

    /// Chapter Tutor — operator skill authoring: teach / update / forget on a
    /// chain (no genesis gate), with collision + unknown-skill errors and
    /// live adoption into the shared persona.
    #[tokio::test]
    async fn author_skill_live_teach_update_forget_and_errors() {
        use aivyx_crypto::MasterKey;
        use aivyx_ipc::protocol::SkillAuthorOp;
        use aivyx_storage::{KeyDomain, RedbStorage, Storage, StorageConfig};
        let dir = test_dir(&format!(
            "tu-author-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let store: Arc<dyn Storage> = RedbStorage::open(
            StorageConfig::new(dir.join("store.redb")),
            MasterKey::from_raw([7u8; 32]),
        )
        .await
        .expect("storage");
        let log = Arc::new(
            crate::persona::PersistentPersonaLog::open(
                store.domain(KeyDomain::Persona),
                b"persona-key".to_vec(),
            )
            .await
            .expect("persona log"),
        );
        let shared =
            crate::persona::shared_effective_persona(crate::persona::EffectivePersona::default());

        // No persona log → clear error.
        assert!(
            author_skill_live(
                None,
                &shared,
                SkillAuthorOp::Teach,
                "x",
                Some("t"),
                Some("p")
            )
            .await
            .is_err()
        );

        // Teach a new skill → appended + adopted live.
        author_skill_live(
            Some(&log),
            &shared,
            SkillAuthorOp::Teach,
            "summarize-doc",
            Some("when asked to summarize"),
            Some("read then condense"),
        )
        .await
        .expect("teach ok");
        let skills = crate::skill_tool::current_skills(&shared);
        assert!(skills.iter().any(|s| s.name == "summarize-doc"));

        // Duplicate teach → rejected (use update).
        assert!(
            author_skill_live(
                Some(&log),
                &shared,
                SkillAuthorOp::Teach,
                "summarize-doc",
                Some("t"),
                Some("p")
            )
            .await
            .is_err()
        );

        // Teach with missing trigger/procedure → rejected.
        assert!(
            author_skill_live(
                Some(&log),
                &shared,
                SkillAuthorOp::Teach,
                "incomplete",
                None,
                None
            )
            .await
            .is_err()
        );

        // Update existing: changes trigger, preserves the omitted procedure.
        author_skill_live(
            Some(&log),
            &shared,
            SkillAuthorOp::Update,
            "summarize-doc",
            Some("new trigger"),
            None,
        )
        .await
        .expect("update ok");
        let skills = crate::skill_tool::current_skills(&shared);
        let s = skills.iter().find(|s| s.name == "summarize-doc").unwrap();
        assert_eq!(s.trigger, "new trigger");
        assert_eq!(
            s.procedure, "read then condense",
            "omitted field preserved on update"
        );

        // Update unknown / forget unknown → errors.
        assert!(
            author_skill_live(
                Some(&log),
                &shared,
                SkillAuthorOp::Update,
                "nope",
                Some("t"),
                None
            )
            .await
            .is_err()
        );
        assert!(
            author_skill_live(
                Some(&log),
                &shared,
                SkillAuthorOp::Forget,
                "nope",
                None,
                None
            )
            .await
            .is_err()
        );

        // Forget existing → removed + adopted.
        author_skill_live(
            Some(&log),
            &shared,
            SkillAuthorOp::Forget,
            "summarize-doc",
            None,
            None,
        )
        .await
        .expect("forget ok");
        let skills = crate::skill_tool::current_skills(&shared);
        assert!(
            !skills.iter().any(|s| s.name == "summarize-doc"),
            "skill forgotten"
        );
    }

    #[tokio::test]
    async fn seed_persona_live_without_log_errors() {
        let shared =
            crate::persona::shared_effective_persona(crate::persona::EffectivePersona::default());
        let r = seed_persona_live(
            None,
            &shared,
            None,
            aivyx_ipc::protocol::PersonaSeedWire {
                character_traits: vec!["x".into()],
                ..Default::default()
            },
        )
        .await;
        assert!(r.is_err());
    }

    #[test]
    fn resolve_document_root_picks_the_right_root_or_errors() {
        let roots = DocumentRoots {
            fs_root: Some(PathBuf::from("/srv/work")),
            workspace_root: None,
        };
        assert_eq!(
            resolve_document_root(&roots, "fs").unwrap(),
            Path::new("/srv/work")
        );
        // workspace unavailable → typed no_workspace.
        match resolve_document_root(&roots, "workspace") {
            Err(QueryResponsePayload::QueryError { code, .. }) => assert_eq!(code, "no_workspace"),
            other => panic!("expected no_workspace error, got {other:?}"),
        }
        // unknown root → bad_root.
        match resolve_document_root(&roots, "etc") {
            Err(QueryResponsePayload::QueryError { code, .. }) => assert_eq!(code, "bad_root"),
            other => panic!("expected bad_root error, got {other:?}"),
        }
    }

    #[test]
    fn fs_mutation_result_maps_ok_and_err() {
        use crate::document_browse::BrowseError;
        match fs_mutation_result(Ok(())) {
            QueryResponsePayload::FsMutation { ok, error } => {
                assert!(ok);
                assert!(error.is_none());
            }
            other => panic!("expected FsMutation, got {other:?}"),
        }
        match fs_mutation_result(Err(BrowseError::Exists)) {
            QueryResponsePayload::FsMutation { ok, error } => {
                assert!(!ok);
                assert_eq!(error.as_deref(), Some("already exists"));
            }
            other => panic!("expected FsMutation, got {other:?}"),
        }
    }

    #[test]
    fn map_browse_error_uses_stable_codes() {
        use crate::document_browse::BrowseError as E;
        let cases = [
            (E::PathEscape, "path_escape"),
            (E::NotFound, "not_found"),
            (E::NotADir, "not_a_dir"),
            (E::NotAFile, "not_a_file"),
            (E::Io("x".into()), "io_error"),
        ];
        for (err, want) in cases {
            match map_browse_error(err) {
                QueryResponsePayload::QueryError { code, .. } => assert_eq!(code, want),
                other => panic!("expected QueryError, got {other:?}"),
            }
        }
    }

    // ---- notify-target config views never leak the raw secret (review finding, Task 5) ----
    //
    // Task 2's tests cover read_*_section/write_*_section round-tripping the
    // raw value; Task 4's tests cover RedactedSecret/*ConfigView's JSON
    // *shape* with an already-redacted value hand-built in place. Neither
    // exercises the actual code path a Get/SetEmailConfig-etc. handler runs:
    // write_*_section(path, ...) -> read_*_section(path) -> *_config_view(&e).
    // `handle_query` itself takes ~30 daemon-context parameters (audit log,
    // memory, persona stores, ...) and isn't practically unit-testable in
    // isolation, so these tests reproduce that exact write/read/view chain
    // directly against the private view-builder helpers in this module —
    // the same layer where `redact()` is actually called — with a planted
    // fake secret, and assert the raw value never survives into the
    // serialized wire response.

    const LEAKED_SECRET: &str = "super-secret-do-not-leak";

    fn secret_leak_temp_toml(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "aivyx-daemon-secret-leak-{}-{}-{tag}.toml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        p
    }

    #[test]
    fn email_config_view_never_leaks_the_raw_password() {
        let path = secret_leak_temp_toml("email");
        std::fs::write(&path, "").unwrap();
        aivyx_config::config_write::write_email_section(
            &path,
            &aivyx_config::config_write::EmailEntryWrite {
                host: Some("smtp.example.com".to_string()),
                port: Some(587),
                tls_mode: Some("starttls".to_string()),
                username: Some("bot@example.com".to_string()),
                password: Some(LEAKED_SECRET.to_string()),
                from: Some("bot@example.com".to_string()),
            },
        )
        .unwrap();
        let entry = aivyx_config::config_write::read_email_section(&path).unwrap();
        let view = email_config_view(&entry);
        let json = serde_json::to_string(&view).unwrap();
        std::fs::remove_file(&path).ok();

        assert!(!json.contains(LEAKED_SECRET), "raw password leaked into wire response: {json}");
        assert!(json.contains("\"configured\":true"), "expected configured:true in {json}");
    }

    #[test]
    fn embedding_config_view_never_leaks_the_raw_api_key() {
        let entry = aivyx_config::config_write::EmbeddingEntryWrite {
            base_url: Some("https://api.openai.com".to_string()),
            model: Some("text-embedding-3-small".to_string()),
            api_key: Some("sk-super-secret-value".to_string()),
        };
        let view = embedding_config_view(&entry);
        let json = serde_json::to_string(&view).unwrap();
        assert!(!json.contains("sk-super-secret-value"), "the real key must never reach the wire");
        assert!(view.api_key.configured);
        assert_eq!(view.api_key.source, "toml");
    }

    #[test]
    fn embedding_write_with_none_api_key_leaves_the_existing_secret_on_disk() {
        let path = secret_leak_temp_toml("embedding-rotate");
        std::fs::write(&path, "").unwrap();
        aivyx_config::config_write::write_embedding_section(
            &path,
            &aivyx_config::config_write::EmbeddingEntryWrite {
                base_url: Some("https://api.openai.com".to_string()),
                model: None,
                api_key: Some("sk-original-secret".to_string()),
            },
        )
        .unwrap();
        // A later save rotates only the model, api_key: None.
        aivyx_config::config_write::write_embedding_section(
            &path,
            &aivyx_config::config_write::EmbeddingEntryWrite {
                base_url: None,
                model: Some("text-embedding-3-large".to_string()),
                api_key: None,
            },
        )
        .unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert!(contents.contains("sk-original-secret"), "None must not clear the existing secret");
        assert!(contents.contains("text-embedding-3-large"));
    }

    #[test]
    fn proactive_config_view_defaults_match_the_loader_when_section_absent() {
        let entry = aivyx_config::config_write::ProactiveEntryWrite::default();
        let view = proactive_config_view(&entry);
        assert!(!view.enabled);
        assert_eq!(view.max_per_window, aivyx_config::DEFAULT_PROACTIVE_MAX_PER_WINDOW);
        assert_eq!(view.window_secs, aivyx_config::DEFAULT_PROACTIVE_WINDOW_SECS);
    }

    #[test]
    fn telegram_config_view_never_leaks_the_raw_token() {
        let path = secret_leak_temp_toml("telegram");
        std::fs::write(&path, "").unwrap();
        aivyx_config::config_write::write_telegram_section(
            &path,
            &aivyx_config::config_write::TelegramEntryWrite {
                token: Some(LEAKED_SECRET.to_string()),
                chat_id: Some(123456),
                team_run_channel: Some(true),
                team_trigger_rate_limit: None,
                team_command_allowed_senders: None,
            },
        )
        .unwrap();
        let entry = aivyx_config::config_write::read_telegram_section(&path).unwrap();
        let view = telegram_config_view(&entry);
        let json = serde_json::to_string(&view).unwrap();
        std::fs::remove_file(&path).ok();

        assert!(!json.contains(LEAKED_SECRET), "raw token leaked into wire response: {json}");
        assert!(json.contains("\"configured\":true"), "expected configured:true in {json}");
    }

    #[test]
    fn discord_config_view_never_leaks_the_raw_token() {
        let path = secret_leak_temp_toml("discord");
        std::fs::write(&path, "").unwrap();
        aivyx_config::config_write::write_discord_section(
            &path,
            &aivyx_config::config_write::DiscordEntryWrite {
                token: Some(LEAKED_SECRET.to_string()),
                application_id: Some(42),
                team_run_channel: Some(true),
                team_trigger_rate_limit: None,
                team_command_allowed_senders: None,
            },
        )
        .unwrap();
        let entry = aivyx_config::config_write::read_discord_section(&path).unwrap();
        let view = discord_config_view(&entry);
        let json = serde_json::to_string(&view).unwrap();
        std::fs::remove_file(&path).ok();

        assert!(!json.contains(LEAKED_SECRET), "raw token leaked into wire response: {json}");
        assert!(json.contains("\"configured\":true"), "expected configured:true in {json}");
    }

    #[test]
    fn slack_config_view_never_leaks_the_raw_tokens() {
        let path = secret_leak_temp_toml("slack");
        std::fs::write(&path, "").unwrap();
        aivyx_config::config_write::write_slack_section(
            &path,
            &aivyx_config::config_write::SlackEntryWrite {
                bot_token: Some(LEAKED_SECRET.to_string()),
                app_token: Some(format!("app-{LEAKED_SECRET}")),
                team_id: Some("T123".to_string()),
                team_run_channel: Some(true),
                team_trigger_rate_limit: None,
                team_command_allowed_senders: None,
            },
        )
        .unwrap();
        let entry = aivyx_config::config_write::read_slack_section(&path).unwrap();
        let view = slack_config_view(&entry);
        let json = serde_json::to_string(&view).unwrap();
        std::fs::remove_file(&path).ok();

        assert!(!json.contains(LEAKED_SECRET), "raw token leaked into wire response: {json}");
        // Two RedactedSecret fields (bot_token, app_token) both configured.
        assert_eq!(json.matches("\"configured\":true").count(), 2, "expected both tokens configured:true in {json}");
    }

    #[test]
    fn reflection_schedule_configs_reflect_a_real_write() {
        let path = secret_leak_temp_toml("reflection-schedule");
        std::fs::write(&path, "").unwrap();
        aivyx_config::config_write::write_reflection_schedule_section(
            &path,
            &aivyx_config::config_write::ReflectionScheduleEntryWrite {
                name: "nightly".to_string(),
                cron: "0 0 9 * * * *".to_string(),
                lookback_window_secs: 86_400,
                enabled: true,
            },
        )
        .unwrap();
        let configs = read_reflection_schedule_configs(&path).unwrap();
        std::fs::remove_file(&path).ok();
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].name, "nightly");
        assert_eq!(configs[0].cron, "0 0 9 * * * *");
    }

    // Task 7 (2026-09-16 audit) — the daemon socket used to bind, then
    // chmod(0600) as a separate step, leaving a real window at the
    // process's default umask (often 022) where the socket was
    // world-accessible before the chmod landed. See
    // `bind_unix_socket_0600`'s doc comment for why the fix is a
    // private-staging-directory-then-rename, not a `libc::umask`
    // bracket (the umask approach was tried first and reverted after
    // it broke this crate's own concurrent test suite).
    //
    // This test proves the *final* mode is 0600. It does NOT vary the
    // ambient umask itself (a test setting process umask would
    // reintroduce, inside the test binary, the exact same
    // process-global-state race this whole fix exists to avoid — this
    // crate's own test suite runs many tests concurrently) -- but the
    // new implementation reads or depends on process umask nowhere in
    // its own logic (unlike the original bind-then-chmod code), so the
    // guarantee holds regardless of the ambient umask by construction,
    // not because this test happened to run under one particular
    // value. It does not by itself prove the *old* code was racy —
    // that would need a concurrent connect-during-bind test, higher
    // effort than this fix warrants — but the new implementation is
    // correct by construction regardless: the socket is never visible
    // at `path` before it's already 0600.
    //
    // `#[tokio::test]`, not plain `#[test]`: `bind_unix_socket_0600`
    // wraps `tokio::net::UnixListener::bind` (matching this file's
    // real bind sites), which registers the socket with the Tokio
    // reactor and so panics ("there is no reactor running") outside a
    // Tokio runtime context, even though the call itself is
    // synchronous.
    #[cfg(unix)]
    #[tokio::test]
    async fn socket_is_never_observable_at_a_wider_mode_than_0600() {
        use std::os::unix::fs::PermissionsExt;

        // Hand-rolled temp dir per this file's existing test convention
        // (see e.g. `secret_leak_temp_toml`/team-run-denial tests above)
        // rather than adding the `tempfile` crate — an established
        // workspace convention (see aivyx-storage/aivyx-config
        // Cargo.toml dev-dependency comments).
        let dir =
            std::env::temp_dir().join(format!("aivyx-socket-umask-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket_path = dir.join("test.sock");

        let listener = bind_unix_socket_0600(&socket_path).unwrap();

        let mode = std::fs::metadata(&socket_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "socket mode was {mode:o}, expected 0600");

        // The socket is genuinely connectable at its final path (the
        // stage-then-rename didn't leave it stranded or break the fd).
        let connect = tokio::net::UnixStream::connect(&socket_path).await;
        assert!(connect.is_ok(), "socket should be connectable at its final path");

        // No leftover staging file next to it.
        let leaked_staging_files = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp"))
            .count();
        assert_eq!(leaked_staging_files, 0, "staging file should not leak");

        drop(listener);
        std::fs::remove_file(&socket_path).ok();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn fresh_socket_parent_dir_is_created_at_0700() {
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!(
            "aivyx-socket-dir-umask-test-{}",
            uuid::Uuid::new_v4()
        ));
        let nested = dir.join("aivyx-pa");

        create_dir_all_0700(&nested).unwrap();

        let mode = std::fs::metadata(&nested).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "dir mode was {mode:o}, expected 0700");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn pre_existing_socket_parent_dir_at_a_wider_mode_is_tightened() {
        // Task 7 final review -- create_dir_all_0700 must still close
        // this gap for a directory an older version of this daemon (or
        // any other process) left at a wider mode, not only get the
        // fresh-creation case right.
        use std::os::unix::fs::PermissionsExt;

        let dir = std::env::temp_dir().join(format!(
            "aivyx-socket-dir-tighten-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();

        create_dir_all_0700(&dir).unwrap();

        let mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "pre-existing dir mode was {mode:o}, expected tightened to 0700");

        std::fs::remove_dir_all(&dir).ok();
    }
}
