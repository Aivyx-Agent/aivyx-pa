//! Daemon IPC protocol types and framing.
//!
//! Implements the wire format specified in `docs/DAEMON_IPC.md`:
//! length-prefixed JSON frames over a Unix domain socket. Three
//! top-level message envelopes (`FrontendMessage`, `DaemonMessage`,
//! `DaemonLifecycleEvent`) are serde-serializable and round-trip
//! through the `encode_frame` / `decode_frame` helpers.
//!
//! Phase 16 Task 2 — this module is the parsing substrate the PoC
//! daemon (Task 3) builds on. It deliberately owns no I/O; the
//! async read/write loops live in the daemon and frontend dispatch
//! paths.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Protocol version sent in `DaemonReady`. Phase 16 defines `"0.1"`.
pub const PROTOCOL_VERSION: &str = "0.1";

/// 16 MiB — per `docs/DAEMON_IPC.md`. A frame whose length prefix
/// exceeds this is a protocol error.
pub const MAX_PAYLOAD_SIZE: u32 = 16 * 1024 * 1024;

/// Length of the frame header (4-byte big-endian payload length).
pub const FRAME_HEADER_LEN: usize = 4;

/// Resolve the daemon socket path per `docs/DAEMON_IPC.md`:
///
/// 1. `$XDG_RUNTIME_DIR/aivyx-pa/daemon.sock` (preferred)
/// 2. `$HOME/.local/share/aivyx-pa/daemon.sock` (fallback)
///
/// Returns `Err` only if neither `XDG_RUNTIME_DIR` nor `HOME` is set.
pub fn default_socket_path() -> Result<PathBuf, String> {
    if let Ok(xdg) = std::env::var("XDG_RUNTIME_DIR") {
        return Ok(PathBuf::from(xdg).join("aivyx-pa").join("daemon.sock"));
    }
    if let Ok(home) = std::env::var("HOME") {
        return Ok(PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("aivyx-pa")
            .join("daemon.sock"));
    }
    Err("neither XDG_RUNTIME_DIR nor HOME is set; cannot determine daemon socket path".into())
}

/// Resolve the daemon PID file path — sibling of the socket file.
///
/// `$XDG_RUNTIME_DIR/aivyx-pa/daemon.pid` (preferred) or
/// `$HOME/.local/share/aivyx-pa/daemon.pid` (fallback).
pub fn default_pid_path() -> Result<PathBuf, String> {
    default_socket_path().map(|p| p.with_extension("pid"))
}

// ---------------------------------------------------------------------------
// Frontend type — identifies the connecting adapter.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FrontendType {
    Local,
    Telegram,
    Web,
    /// Phase 111 — Discord adapter daemon-frontend. Mirrors the
    /// Phase 19 Telegram-over-daemon pattern; the daemon-side
    /// `discord_daemon_frontend.rs` builds an `IpcChannelBridge`
    /// when a `FrontendType::Discord` connection arrives.
    Discord,
    /// Phase 111 — Slack adapter daemon-frontend. Same shape as
    /// Discord; the daemon-side `slack_daemon_frontend.rs` builds
    /// an `IpcChannelBridge` when a `FrontendType::Slack`
    /// connection arrives.
    Slack,
}

// ---------------------------------------------------------------------------
// Phase 45 — IPC attachment for multimodal input
// ---------------------------------------------------------------------------

/// A base64-encoded file attachment sent with `SubmitInput`. The daemon
/// decodes the base64 data and constructs the appropriate `Message`
/// variant (image, text+image, or text-only if no attachments).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IpcAttachment {
    pub media_type: String,
    pub data_base64: String,
    #[serde(default)]
    pub filename: Option<String>,
}

// ---------------------------------------------------------------------------
// Phase 47 — Query/QueryResponse envelope (Web UI Phase 2)
// ---------------------------------------------------------------------------

/// Inspection-side queries the frontend sends to the daemon. Carried
/// inside [`FrontendMessage::Query`] with a correlation `id` the daemon
/// echoes back in [`DaemonMessage::QueryResponse`].
///
/// All queries are read-only by contract — mutating operations stay on
/// the existing turn-loop / gate-resolution paths.
///
/// **Authorization:** none at the query layer. The daemon IPC socket
/// is `mode 0600` owned by the operator's UID (`PRODUCT.md` P6 /
/// `docs/THREAT_MODEL.md` §4.4). Anyone who can `read(2)` the socket
/// *is* the operator, so per-query capability gating would only check
/// the operator's own role envelope against their own inspection —
/// which is not the threat model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum QueryPayload {
    /// List active session IDs tracked by the daemon.
    ListSessions,
    /// List all missions persisted under `KeyDomain::Missions`.
    ListMissions,
    /// Fetch a single mission by id, including all its gates.
    GetMission {
        mission_id: String,
    },
    /// Paginated read of the persistent audit chain. Returns at most
    /// `limit` entries starting at `from_seq`. `limit` is capped
    /// server-side at 500 (Phase 47 Q3). Read-only.
    ListAuditEntries {
        from_seq: u64,
        limit: u32,
    },
    /// Cold-verify the in-memory audit chain. Returns whether the chain
    /// hashes match, the number of entries verified, and the first
    /// error encountered if any.
    VerifyAuditChain,
    /// Phase 58 — fetch the daemon's loaded `Profile`
    /// (PRODUCT.md P13). Read-only inspection. Returns a
    /// [`ProfileSummary`].
    ///
    /// `from_disk` (Chapter V) selects which Profile:
    /// - `false` (default) — the **running** snapshot the daemon is
    ///   actually using for system-prompt assembly (the boot-time
    ///   `Arc<Profile>`). This is the Command-Center / status meaning.
    /// - `true` — re-read the **on-disk** `[profile]` from `aivyx-pa.toml`
    ///   (the same source the Agents editor *writes*, and what the next
    ///   restart will load). The two diverge after a `SetProfile` write
    ///   that hasn't been applied by a restart yet; the editor seeds from
    ///   `from_disk = true` so what you load equals what you edit.
    ///   Falls back to the running snapshot when the daemon was launched
    ///   without an `aivyx-pa.toml`.
    ///
    /// `#[serde(default)]` keeps the field absent on the wire for
    /// pre-Chapter-V clients (`{"kind":"GetProfile"}` decodes to
    /// `from_disk = false`), so the running-state meaning is unchanged.
    GetProfile {
        #[serde(default)]
        from_disk: bool,
    },
    /// Phase 60 — fetch the daemon's current effective Persona
    /// (PRODUCT.md P14). Read-only inspection. Returns the
    /// folded state — same values the assemble_session_prompt
    /// helper uses for the "## How I have learned to communicate"
    /// section.
    GetEffectivePersona,
    /// Phase 60 — fetch the persona delta chain with pagination.
    /// Mirrors `ListAuditEntries`. The daemon caps `limit`
    /// server-side at 500 entries per response.
    ListPersonaDeltas {
        from_seq: u64,
        limit: u32,
    },
    /// Phase 64 — fetch the full Persona chain in a single response
    /// for export. Unlike `ListPersonaDeltas` (paginated summaries
    /// for the Web UI), this returns full-fidelity `DeltaExport`
    /// values that preserve every PersonaDelta field. Operator-
    /// driven; intended to feed `aivyx-pa identity export <path>`.
    /// The daemon returns up to `MAX_EXPORT_CHAIN_ENTRIES` entries
    /// in one shot (current cap: 100,000 — enough for years of
    /// reflection-approved deltas at realistic rates).
    ExportPersonaChain,
    /// Phase 70 — list pending and resolved Persona proposals.
    /// `status_filter` is one of `"all" | "pending" | "approved"
    /// | "rejected" | "superseded"`; unknown values default to
    /// `"pending"` server-side. The daemon caps the page at
    /// `limit` entries.
    ListPersonaProposals {
        status_filter: String,
        limit: u32,
    },
    /// Phase 70 — fetch a single Persona proposal by id. Returns
    /// the proposal's current status-derived view.
    GetPersonaProposal {
        proposal_id: String,
    },
    /// Phase 73 — paginated walk of the audit chain for
    /// `AutoNotifyDispatched` events. The daemon filters by
    /// `target_filter` when set and renders each match into a
    /// `NotificationHistoryEntry`. `limit` is server-side
    /// capped (same as audit-entry queries: 500 max per page).
    ListNotificationHistory {
        from_seq: u64,
        limit: u32,
        target_filter: Option<String>,
    },
    /// Chapter Herald — read-only list of configured notify targets
    /// (name/kind/default), for the Studio Notifications screen.
    /// Targets remain TOML-managed; this is a view, not a CRUD
    /// surface — creating/editing a target still means editing
    /// `aivyx-pa.toml`.
    GetNotifyTargets,
    /// POLISH_WAVES.md sub-project 7 plan 2 — the editable notify-target
    /// list (distinct from `GetNotifyTargets`'s read-only status view).
    /// Responds with [`QueryResponsePayload::GetNotifyTargetConfigs`].
    GetNotifyTargetConfigs,
    /// Add or replace (by `name`) one `[[notify_target]]` entry. Takes
    /// effect on the next daemon start. Responds with
    /// [`QueryResponsePayload::NotifyTargetsApplied`] (or `QueryError`).
    SetNotifyTarget {
        name: String,
        // `kind` alone collides with this enum's own `#[serde(tag =
        // "kind")]` internal tag (same reason `trigger_kind`/
        // `outcome_kind`/`surface_kind` elsewhere in this file avoid the
        // bare word) — keep the Rust field name `kind` for parity with
        // `NotifyTargetEntryWrite`/`NotifyTargetConfigView`, but rename
        // its wire key so it doesn't shadow the tag.
        #[serde(rename = "target_kind")]
        kind: String,
        #[serde(default)]
        chat_id: Option<String>,
        #[serde(default)]
        url: Option<String>,
        #[serde(default)]
        to: Option<String>,
        enabled: bool,
        is_default: bool,
        #[serde(default)]
        retry_count: u32,
        #[serde(default)]
        retry_backoff_ms_start: u64,
        #[serde(default)]
        rate_limit_max: Option<u32>,
        #[serde(default)]
        rate_limit_window_secs: Option<u64>,
    },
    /// Remove one `[[notify_target]]` entry by name (a no-op if absent).
    /// Responds with [`QueryResponsePayload::NotifyTargetsApplied`].
    DeleteNotifyTarget { name: String },
    /// Phase 74 — list every distinct memory topic. Drives the
    /// Web UI Memory pane's left-column topic list + the
    /// `aivyx-pa memory list` CLI render.
    ListMemoryTopics,
    /// Phase 74 — fetch up to `limit` entries for a single
    /// memory topic, newest first. Mirrors `Memory::get_recent`'s
    /// shape over the wire.
    GetMemoryTopicEntries {
        topic: String,
        limit: u32,
    },
    /// Phase 74 — substring search across topics + bodies.
    /// Empty `query` returns the newest entries across every
    /// topic.
    SearchMemory {
        query: String,
        limit: u32,
        /// Phase 75 — request the embedding-ranked path.
        /// `#[serde(default)]` (= `false`, keyword) so
        /// pre-Phase-75 clients and stored frames round-trip
        /// unchanged. Falls back to keyword transparently when
        /// embedding is unavailable.
        #[serde(default)]
        semantic: bool,
    },
    /// Chapter MG — fetch the memory **knowledge graph**: topic nodes (each with
    /// its entry count) + the top-`limit` weighted co-occurrence edges (which
    /// topics get recalled together helpfully). Read-only. `edges` is empty when
    /// the co-occurrence ledger isn't armed (→ a topic cloud). Responds with
    /// [`QueryResponsePayload::GetMemoryGraph`].
    GetMemoryGraph {
        limit: u32,
    },
    /// Chapter Codex — list synthesized knowledge-wiki pages (compact
    /// rows: topic + snippet + entry count + updated-at), most-recent
    /// first. Read-only. Empty when no pages have been synthesized.
    /// Responds with [`QueryResponsePayload::ListWikiPages`].
    ListWikiPages,
    /// Chapter Codex — fetch one topic's full knowledge-wiki page
    /// (summary + backlinks + source-entry seqs). Read-only. Responds
    /// with [`QueryResponsePayload::GetWikiPage`] (`page: None` when the
    /// topic has no page yet).
    GetWikiPage {
        topic: String,
    },
    /// Chapter Lattice — fetch the **typed knowledge graph**: entity nodes
    /// (with degree) + the top-`limit` directed `(subject)-[predicate]->
    /// (object)` relations (by `mentions`). Read-only. Empty until the
    /// extraction sweep has run. Responds with
    /// [`QueryResponsePayload::GetKnowledgeGraph`].
    GetKnowledgeGraph {
        limit: u32,
    },
    /// Chapter Concord — detect contradictions in stored memory: an
    /// on-demand LLM pass that returns pairs of entries under one topic
    /// asserting incompatible facts, for the operator to resolve. Read-
    /// only (detection performs no mutation). Responds with
    /// [`QueryResponsePayload::MemoryConflicts`].
    GetMemoryConflicts,
    /// Chapter Accord — on-demand detection of self-contradicting Persona
    /// facets (and learned facets that drift against operator Profile
    /// constraints), for the operator to resolve. Read-only. Responds with
    /// [`QueryResponsePayload::SoulConflicts`].
    GetSoulConflicts,
    /// Chapter Repertoire — the Studio Skills library: every `LearnedSkill`
    /// in the effective persona, joined with its WH.2 effectiveness
    /// (decayed EWMA + samples), plus the count of pending skill proposals.
    /// Read-only. Responds with [`QueryResponsePayload::GetSkills`].
    GetSkills,
    /// Chapter Almanac — the Studio Tools screen: the daemon's full
    /// registered tool catalog (name, description, capability base,
    /// minimum trust tier), independent of audit history — a pure
    /// registry snapshot, not observability. Read-only. Responds with
    /// [`QueryResponsePayload::GetToolCatalog`].
    GetToolCatalog,
    /// Chapter Lantern — the Studio MCP screen: each configured MCP
    /// server's last-start health (connected + tool count, or failed +
    /// reason + captured stderr), read from the daemon's status
    /// snapshot. Read-only. Responds with
    /// [`QueryResponsePayload::GetMcpStatus`].
    GetMcpStatus,
    /// Command Center — list the agent's scheduled background routines for the
    /// dashboard (name, cadence, enabled, last/next fire). Read-only. Responds
    /// with [`QueryResponsePayload::Schedules`].
    GetSchedules,
    /// Phase 78 — read-only learning-observability query.
    /// `window_secs = None` → the handler's default lookback.
    /// `#[serde(default)]` so older clients/frames decode.
    GetLearningInsights {
        #[serde(default)]
        window_secs: Option<u64>,
    },
    /// Phase 102 — read-only tool-observability query. Returns the
    /// daemon's registered tool set joined with audit-derived
    /// call statistics. `window_secs = None` → the whole audit
    /// chain; `Some(n)` → only `ToolCall` events from the last `n`
    /// seconds. `#[serde(default)]` so older clients/frames decode.
    GetToolStats {
        #[serde(default)]
        window_secs: Option<u64>,
    },
    /// Phase 119 Task 6 — operator-inspection dump of the
    /// Phase 116 `KeyDomain::ToolRelevanceLedger`. Returns every
    /// per-keyword-key outcome row optionally filtered to a
    /// single keyword key. `#[serde(default)]` so the filter is
    /// absent in pre-Phase-119 frames (which won't send this
    /// query at all, but the wire-compat pattern stays uniform).
    DumpToolRelevance {
        #[serde(default)]
        keyword_key_filter: Option<String>,
    },
    /// Phase 173 — add a story to the autonomous-loop backlog.
    /// `priority = None` → the `[loop].default_priority` (or the
    /// built-in default). Returns the new story's id.
    LoopAdd {
        title: String,
        #[serde(default)]
        body: String,
        #[serde(default)]
        priority: Option<u32>,
    },
    /// Phase 173 — list every backlog story (all statuses).
    LoopList,
    /// Phase 173 — start an autonomous-loop run. `max_iterations
    /// = None` → the `[loop].max_iterations` default. Fails if a
    /// run is already active or the `[loop]` section is not armed.
    LoopStart {
        #[serde(default)]
        max_iterations: Option<u32>,
    },
    /// Phase 173 — request the active run to stop (between
    /// iterations). Fails if no run is active.
    LoopStop,
    /// Phase 173 — read the loop run state + remaining backlog.
    LoopStatus,
    /// Phase 175 — read the recent loop progress-log notes
    /// (operator parity with what the driver injects). `limit =
    /// None` → a default window.
    LoopLog {
        #[serde(default)]
        limit: Option<u32>,
    },
    /// Phase 177 — mark a pending backlog story `Skipped` (the
    /// operator prunes a stuck / no-longer-wanted story). Reuses
    /// the `LoopControl` response.
    LoopSkip {
        story_id: String,
    },
    /// Chapter L (L.5) — start a daemon-run team mission from an explicit
    /// [`MissionPlan`]. `config` pins a vertical-pack team (`None` ⇒ the daemon
    /// default Nonagon). Fails if no team service is configured. Responds with
    /// [`QueryResponsePayload::TeamRunStarted`].
    TeamRun {
        plan: aivyx_team_types::MissionPlan,
        #[serde(default)]
        config: Option<aivyx_team_types::TeamConfig>,
    },
    /// Chapter L — start a mission from a free-text goal: the daemon decomposes
    /// it into a plan (one LLM planning call over the chosen team's roster) and
    /// runs it. `config` pins a vertical-pack team (`None` ⇒ the default
    /// Nonagon). Responds with [`QueryResponsePayload::TeamRunStarted`].
    TeamRunGoal {
        goal: String,
        #[serde(default)]
        config: Option<aivyx_team_types::TeamConfig>,
    },
    /// Chapter L (L.5) — every team mission's snapshot (the poll feed the TUI
    /// Missions panel ticks). Responds with
    /// [`QueryResponsePayload::TeamMissionList`].
    TeamMissionList,
    /// Chapter L (L.5) — one team mission's snapshot. Responds with
    /// [`QueryResponsePayload::TeamMissionStatus`] (`None` if unknown).
    TeamMissionStatus {
        mission_id: String,
    },
    /// Chapter Y — fetch the daemon's active team roster (the Nonagon
    /// `TeamConfig`: lead + specialists, each with role / trust / scopes /
    /// tools / soul). Read-only; the Studio's Teams screen renders it. Responds
    /// with [`QueryResponsePayload::GetTeamRoster`] (or `QueryError` `no_team`
    /// when the daemon has no team service).
    GetTeamRoster,
    /// Chapter Z — list a directory for the Documents browser. `root` is
    /// `"workspace"` (the agent's `~/.aivyx-pa/workspace`) or `"fs"` (the operator's
    /// access-scoped `fs_root`); `path` is relative to that root. Read-only;
    /// `..`/symlink escapes are rejected daemon-side. Responds with
    /// [`QueryResponsePayload::ListDir`] (or `QueryError` `bad_root` /
    /// `no_workspace` / `path_escape` / `not_found` / `not_a_dir` / `io_error`).
    ListDir {
        root: String,
        #[serde(default)]
        path: String,
    },
    /// Chapter Z — read a file for the Documents viewer. Same `root` / `path`
    /// rules as [`ListDir`]; binary or over-cap files come back with
    /// `content = None`. Responds with [`QueryResponsePayload::ReadFile`] (or the
    /// same `QueryError` codes as `ListDir`, plus `not_a_file`).
    ReadFile {
        root: String,
        path: String,
    },
    /// Chapter DW — write (create or, with `overwrite`, replace) a text file.
    /// Same `root`/`path` rules + guard as the reads. `overwrite = false` refuses
    /// an existing path (`exists`); the editor's save sends `true`. Atomic
    /// temp+rename, audited. Responds with [`QueryResponsePayload::FsMutation`].
    WriteFile {
        root: String,
        path: String,
        content: String,
        #[serde(default)]
        overwrite: bool,
    },
    /// Chapter DW — delete a **file** or an **empty directory** (never
    /// recursive). **Requires** `confirm = true` — the daemon refuses without it
    /// (`confirm_required`). Audited. Responds with `FsMutation`.
    DeleteFile {
        root: String,
        path: String,
        #[serde(default)]
        confirm: bool,
    },
    /// Chapter DW — rename `path` → `new_path` (both under `root`). Never
    /// clobbers (`new_path` must not exist). Audited. Responds with `FsMutation`.
    RenamePath {
        root: String,
        path: String,
        new_path: String,
    },
    /// Chapter DW — create a directory at `path`. Errors if it exists. Audited.
    /// Responds with `FsMutation`.
    MakeDir {
        root: String,
        path: String,
    },
    /// Chapter L (L.5) — approve or reject a mission paused at a human-approval
    /// gate. Responds with [`QueryResponsePayload::TeamGateResolved`].
    ResolveTeamGate {
        mission_id: String,
        step: String,
        approve: bool,
    },
    /// Chapter Belay — request that a running mission halt at its next wave
    /// boundary. Responds with [`QueryResponsePayload::TeamMissionAborted`].
    AbortTeamMission {
        mission_id: String,
    },
    /// Chapter Mission Control — request that a running mission pause at
    /// its next wave boundary (resumable, unlike abort). Responds with
    /// [`QueryResponsePayload::TeamMissionPaused`].
    PauseTeamMission {
        mission_id: String,
    },
    /// Chapter Mission Control — resume a paused mission; the resume
    /// drives in the background. Responds with
    /// [`QueryResponsePayload::TeamMissionResumed`].
    ResumeTeamMission {
        mission_id: String,
    },
    /// Chapter U — read the daemon's effective config snapshot for the
    /// Settings screen: access level + resolved `fs_root` + confirm posture,
    /// provider / model / `num_ctx`, the `[budget]` caps, and whether an
    /// embedding provider is available. Read-only; none of this was queryable
    /// before. Responds with [`QueryResponsePayload::GetSettings`].
    GetSettings,
    /// Chapter U — rewrite the `[access]` section of `aivyx-pa.toml`. `level` is
    /// one of `sandbox | workspace | home | full | custom`; `root` is required
    /// for `workspace`/`custom` and rejected for the auto-derived levels.
    /// `confirm` MUST be `true` for any expanded (non-sandbox) level — the
    /// Chapter N confirm-first gate, enforced **server-side**, not just in the
    /// UI. The change is written to disk but takes effect on the next daemon
    /// start (access level is load-time). Responds with
    /// [`QueryResponsePayload::SettingsApplied`] (or `QueryError` on a
    /// validation failure / missing config file).
    SetAccessLevel {
        level: String,
        #[serde(default)]
        root: Option<String>,
        #[serde(default)]
        confirm: bool,
    },
    /// Chapter U — rewrite the `[budget]` section of `aivyx-pa.toml`. A `None`
    /// cap clears that dimension (uncapped). `on_exceeded` is `alert | deny`
    /// (absent ⇒ the `deny` default); `alert_at` is the early-warning fraction
    /// in `[0.0, 1.0]` (absent ⇒ no early-warning tier). Takes effect on the
    /// next daemon start. Responds with
    /// [`QueryResponsePayload::SettingsApplied`] (or `QueryError`).
    SetBudget {
        #[serde(default)]
        per_run_usd: Option<f64>,
        #[serde(default)]
        per_day_usd: Option<f64>,
        #[serde(default)]
        on_exceeded: Option<String>,
        #[serde(default)]
        alert_at: Option<f64>,
    },
    /// Rewrite `[agent] cycle_detection` — arm/disarm the interactive agent's
    /// small-cycle breaker. Takes effect on the next daemon start. Responds with
    /// [`QueryResponsePayload::SettingsApplied`] (or `QueryError`).
    SetCycleDetection { enabled: bool },
    /// Model routing Part 3b (A15) — allow cloud escalation for one
    /// conversation, in this daemon process only (a restart re-asks). The
    /// same effect as sending `/allow-cloud` in that conversation. Never
    /// overrides a routing taint. Responds with
    /// [`QueryResponsePayload::CloudEscalationAllowed`], or
    /// [`QueryResponsePayload::CloudEscalationNotEnabled`] when no cloud
    /// escalation is configured.
    AllowCloudEscalation { session_id: String },
    /// Chapter Reins — rewrite `[autonomy] level`. `level` is one of `manual |
    /// assisted | supervised | autonomous | unleashed`. `confirm` MUST be `true`
    /// for the autonomy-granting levels (`autonomous` / `unleashed`) — the
    /// confirm-first gate, enforced **server-side**, not just in the UI. Only
    /// the `level` is rewritten (per-domain overrides + the allowlist are
    /// preserved). Takes effect on the next daemon start. Responds with
    /// [`QueryResponsePayload::SettingsApplied`] (or `QueryError`).
    SetAutonomyLevel {
        level: String,
        #[serde(default)]
        confirm: bool,
    },
    /// Chapter V — rewrite the `[profile]` section of `aivyx-pa.toml` (the
    /// operator-declared identity layer, PRODUCT.md P13). Every field carries
    /// **clear-on-`None`** semantics matching `aivyx_config::ProfileWrite`: an
    /// absent field removes that key (the loader's default then applies — e.g.
    /// `assistant_name` falls back to `"Aivyx PA"`), a present one writes it. An
    /// explicit empty list (`Some([])`) is "declared but empty", distinct from
    /// absent. The Settings screen's confirm-first gate does **not** apply here
    /// — Profile is free-form declaration, not an access-expansion. Takes
    /// effect on the next daemon start (Profile shapes `assemble_session_prompt`
    /// at load time). Responds with [`QueryResponsePayload::ProfileApplied`]
    /// (or `QueryError` on a malformed config file / write failure).
    ///
    /// The agent's *self-learned* Persona is **not** writable here — it is
    /// governed only through the existing proposal/revert IPC
    /// ([`FrontendMessage::ResolvePersonaProposal`] /
    /// [`FrontendMessage::RevertPersonaDelta`]).
    SetProfile {
        #[serde(default)]
        assistant_name: Option<String>,
        #[serde(default)]
        operator_profile: Option<String>,
        #[serde(default)]
        communication_style: Option<String>,
        #[serde(default)]
        primary_use_cases: Option<Vec<String>>,
        #[serde(default)]
        behavioral_preferences: Option<Vec<String>>,
        #[serde(default)]
        behavioral_constraints: Option<Vec<String>>,
    },
    /// Chapter Voice — read the daemon's `[voice]` config snapshot for the
    /// Voice screen: the options + a **readiness** check (the daemon stats the
    /// Whisper model and scans the Kokoro model directory for the `.onnx` model
    /// and `voices-*.bin`). Read-only.
    /// Responds with [`QueryResponsePayload::GetVoiceSettings`].
    GetVoiceSettings,
    /// Chapter Voice — rewrite the `[voice]` section of `aivyx-pa.toml`. All fields
    /// are `#[serde(default)]` with **clear-on-`None`** (matching
    /// `aivyx_config::VoiceWrite`). Load-time — takes effect when the voice
    /// channel (`aivyx-pa --channel voice`) next starts. Responds with
    /// [`QueryResponsePayload::VoiceApplied`] (or `QueryError`).
    SetVoice {
        #[serde(default)]
        asr_engine: Option<String>,
        #[serde(default)]
        tts_engine: Option<String>,
        #[serde(default)]
        asr_model_path: Option<String>,
        #[serde(default)]
        asr_language: Option<String>,
        #[serde(default)]
        asr_beam_size: Option<u32>,
        #[serde(default)]
        tts_model_dir: Option<String>,
        #[serde(default)]
        tts_voice_name: Option<String>,
        #[serde(default)]
        tts_speed: Option<f32>,
        #[serde(default)]
        input_device: Option<String>,
        #[serde(default)]
        output_device: Option<String>,
    },
    /// Chapter Roster — persist the operator-authored team. Writes the whole
    /// `[team]`-rooted config file the daemon loads at startup (Chapter Roster
    /// RO.1: `[team] config_path`, else the conventional `team.toml` beside
    /// `aivyx-pa.toml`). The daemon **validates** the roster server-side
    /// (`TeamConfig::validate` — names, scopes parse to a known base,
    /// lead-is-a-member, ≤9 specialists) **before** it touches disk; an invalid
    /// roster is rejected with `QueryError` `invalid_roster` and nothing is
    /// written. NT-02 is unchanged: declaring a member scope the lead lacks is
    /// valid but inert (`attenuate_for_member` still floors specialists at
    /// spawn). Takes effect on the **next daemon start** (the team service is
    /// boot-assembled). Responds with [`QueryResponsePayload::TeamRosterApplied`]
    /// (or `QueryError` — `no_config_file` for an env-only launch).
    SetTeamRoster {
        roster: aivyx_team_types::TeamConfig,
    },
    /// POLISH_WAVES.md sub-project 7, item B — the editable MCP server
    /// list (distinct from `GetMcpStatus`'s live connection status).
    /// Responds with [`QueryResponsePayload::GetMcpServerConfigs`].
    GetMcpServerConfigs,
    /// Add or replace (by `name`) one `[[mcp_server]]` entry. Takes effect
    /// on the next daemon start (MCP servers are boot-constructed).
    /// Responds with [`QueryResponsePayload::McpServersApplied`] (or
    /// `QueryError` on a structural validation failure).
    SetMcpServer {
        name: String,
        transport: String,
        #[serde(default)]
        command: Option<String>,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: Vec<(String, String)>,
        #[serde(default)]
        headers: Vec<(String, String)>,
        #[serde(default)]
        url: Option<String>,
        enabled: bool,
    },
    /// Remove one `[[mcp_server]]` entry by name (a no-op, not an error, if
    /// no entry with that name exists). Takes effect on the next daemon
    /// start. Responds with [`QueryResponsePayload::McpServersApplied`].
    DeleteMcpServer { name: String },
    /// POLISH_WAVES.md sub-project 7, item B — attempt a real connection
    /// (the same logic the daemon uses at boot) against in-progress form
    /// values, before the operator saves. Never joins the live server
    /// list — a one-shot probe, torn down after. Responds with
    /// [`QueryResponsePayload::McpServerTestResult`].
    TestMcpServerConnection {
        transport: String,
        #[serde(default)]
        command: Option<String>,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: Vec<(String, String)>,
        #[serde(default)]
        headers: Vec<(String, String)>,
        #[serde(default)]
        url: Option<String>,
    },
    /// Studio Gallery — recent images generated via the configured
    /// `comfyui` `[[mcp_server]]`, read directly from ComfyUI's own
    /// `/history` HTTP API (not the MCP tool surface). Read-only.
    /// Responds with [`QueryResponsePayload::Gallery`].
    GetGallery,
    /// Response: [`QueryResponsePayload::GetEmailConfig`].
    GetEmailConfig,
    /// `password: None` means leave the existing SMTP password untouched.
    /// Responds with [`QueryResponsePayload::EmailConfigApplied`].
    SetEmailConfig {
        #[serde(default)]
        host: Option<String>,
        #[serde(default)]
        port: Option<u16>,
        #[serde(default)]
        tls_mode: Option<String>,
        #[serde(default)]
        username: Option<String>,
        #[serde(default)]
        password: Option<String>,
        #[serde(default)]
        from: Option<String>,
    },
    /// Response: [`QueryResponsePayload::GetTelegramConfig`].
    GetTelegramConfig,
    /// `token: None` means leave the existing bot token untouched.
    /// Responds with [`QueryResponsePayload::TelegramConfigApplied`].
    SetTelegramConfig {
        #[serde(default)]
        token: Option<String>,
        #[serde(default)]
        chat_id: Option<i64>,
        #[serde(default)]
        team_run_channel: Option<bool>,
        #[serde(default)]
        team_trigger_rate_limit: Option<u32>,
        #[serde(default)]
        team_command_allowed_senders: Option<Vec<i64>>,
    },
    /// Response: [`QueryResponsePayload::GetDiscordConfig`].
    GetDiscordConfig,
    /// `token: None` means leave the existing bot token untouched.
    /// Responds with [`QueryResponsePayload::DiscordConfigApplied`].
    SetDiscordConfig {
        #[serde(default)]
        token: Option<String>,
        #[serde(default)]
        application_id: Option<u64>,
        #[serde(default)]
        team_run_channel: Option<bool>,
        #[serde(default)]
        team_trigger_rate_limit: Option<u32>,
        #[serde(default)]
        team_command_allowed_senders: Option<Vec<u64>>,
    },
    /// Response: [`QueryResponsePayload::GetSlackConfig`].
    GetSlackConfig,
    /// `bot_token`/`app_token`: `None` means leave that token untouched
    /// (each rotatable independently). Responds with
    /// [`QueryResponsePayload::SlackConfigApplied`].
    SetSlackConfig {
        #[serde(default)]
        bot_token: Option<String>,
        #[serde(default)]
        app_token: Option<String>,
        #[serde(default)]
        team_id: Option<String>,
        #[serde(default)]
        team_run_channel: Option<bool>,
        #[serde(default)]
        team_trigger_rate_limit: Option<u32>,
        #[serde(default)]
        team_command_allowed_senders: Option<Vec<String>>,
    },
    /// POLISH_WAVES.md sub-project 7 plan 3 — the editable
    /// `[[reflection_schedule]]` list. Responds with
    /// [`QueryResponsePayload::GetReflectionScheduleConfigs`].
    GetReflectionScheduleConfigs,
    /// Add or replace (by `name`) one `[[reflection_schedule]]` entry.
    /// Takes effect on the next daemon start. Responds with
    /// [`QueryResponsePayload::ReflectionScheduleConfigApplied`] (or
    /// `QueryError`).
    SetReflectionSchedule {
        name: String,
        cron: String,
        lookback_window_secs: u64,
        enabled: bool,
    },
    /// Remove one `[[reflection_schedule]]` entry by name (a no-op if
    /// absent). Responds with
    /// [`QueryResponsePayload::ReflectionScheduleConfigApplied`].
    DeleteReflectionSchedule { name: String },
    /// Response: [`QueryResponsePayload::GetMemoryProfileConfig`].
    GetMemoryProfileConfig,
    /// `profile`: `"off" | "lite" | "smart"`. Always sent — the Studio
    /// picker has no blank state (unlike every other `SetX` in this
    /// sub-project, this field is a plain `String`, not `Option`).
    /// Responds with [`QueryResponsePayload::MemoryProfileConfigApplied`].
    SetMemoryProfile { profile: String },
    /// Response: [`QueryResponsePayload::GetEmbeddingConfig`].
    GetEmbeddingConfig,
    /// `api_key: None` means leave the existing key untouched. Responds
    /// with [`QueryResponsePayload::EmbeddingConfigApplied`].
    SetEmbeddingConfig {
        #[serde(default)]
        base_url: Option<String>,
        #[serde(default)]
        model: Option<String>,
        #[serde(default)]
        api_key: Option<String>,
    },
    /// Response: [`QueryResponsePayload::GetProactiveConfig`].
    GetProactiveConfig,
    /// Responds with [`QueryResponsePayload::ProactiveConfigApplied`].
    SetProactiveConfig {
        #[serde(default)]
        enabled: Option<bool>,
        #[serde(default)]
        target: Option<String>,
        #[serde(default)]
        max_per_window: Option<u32>,
        #[serde(default)]
        window_secs: Option<u64>,
    },
    /// POLISH_WAVES.md sub-project 8 item C — read-only per-MCP-server
    /// observability query, closing the gap [`QueryPayload::
    /// GetToolStats`] leaves for MCP-bridged tools (which all share
    /// the single `"mcp.call"` scope base). `window_secs = None`
    /// scopes the answer to the whole audit chain, matching
    /// `GetToolStats`'s own convention. Distinct from [`QueryPayload::
    /// GetMcpStatus`], which is a boot-time file snapshot, not live
    /// audit-derived data — do not merge these two queries.
    GetMcpServerCallStats {
        #[serde(default)]
        window_secs: Option<u64>,
    },
    /// Phase 186 — read-only reminders query for the TUI Dashboard's
    /// reminders panel. No frontend surface existed for `remind.*`
    /// before this: the feature was agent-tool-only
    /// (`crate::reminder_tool::RemindListTool`). Always "all pending" —
    /// no parameters, matching `ReminderStore::list`'s own shape.
    GetReminders,
}

/// Chapter Repertoire — one row in the Studio Skills library: a
/// `LearnedSkill` joined with its WH.2 effectiveness. `ewma_score`/
/// `samples` are `0` for a skill the effectiveness ledger hasn't seen yet
/// ("not yet measured"). Wasm-clean (the Studio renders it directly).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillView {
    /// The skill itself (name, trigger, procedure, version, provenance,
    /// refined_from, domain).
    pub skill: crate::persona::LearnedSkill,
    /// Decayed effectiveness EWMA (WH.2 ledger), decayed to "now". `0.0`
    /// when unmeasured.
    pub ewma_score: f32,
    /// Folded windows behind `ewma_score` — a confidence proxy. `0` when
    /// unmeasured.
    pub samples: u32,
    /// Chapter Repertoire — how many times this skill has been invoked
    /// (`SkillInvocation` audit entries), all-time. `0` if never used.
    #[serde(default)]
    pub invocations: u32,
}

/// Chapter Almanac — one row in the Studio Tools library: a registered
/// tool's name, description, and the capability it gates on. `min_tier`
/// is the least-trusted `TrustTier` whose default ceiling grants
/// `scope_base` unqualified (see `TrustTier::min_for_scope`) — the
/// tier a channel needs before this tool becomes reachable at all.
/// Wasm-clean (the Studio renders it directly).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCatalogEntry {
    pub name: String,
    pub description: String,
    pub scope_base: String,
    pub min_tier: aivyx_capability::TrustTier,
}

/// Chapter Tutor — which operator-authoring action [`FrontendMessage::AuthorSkill`]
/// performs against the persona chain. This is the **operator** channel
/// (CLI / Studio), distinct from the agent's scope-gated `skills.teach` tool:
/// it writes operator-authored skills directly via the daemon's chain-append,
/// so it needs no agent `skills.write` scope and works on a grown chain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SkillAuthorOp {
    /// Add a new skill. Daemon rejects a duplicate `name` (use `Update`).
    Teach,
    /// Change an existing skill's `trigger` and/or `procedure` (supersession).
    Update,
    /// Remove an existing skill by `name`.
    Forget,
}

/// Chapter Lantern — one MCP server's last-start health for the Studio
/// MCP screen. Mirrors the daemon's status snapshot (Chapter Conduit
/// CD.3): `connected` with a `tool_count`, or failed with an `error`
/// and the last lines of captured `stderr`. Wasm-clean (the Studio
/// renders it directly).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpServerStatusView {
    pub name: String,
    /// `"stdio"`, `"sse"`, or `"http"`.
    pub transport: String,
    pub connected: bool,
    /// Tools the server registered when connected; `0` if it failed.
    pub tool_count: usize,
    /// Failure reason (`None` when connected).
    #[serde(default)]
    pub error: Option<String>,
    /// Last captured stderr lines (stdio servers; empty otherwise).
    #[serde(default)]
    pub stderr_tail: Vec<String>,
}

impl McpServerStatusView {
    /// A server that connected and registered `tool_count` tools.
    pub fn connected(name: &str, transport: &str, tool_count: usize) -> Self {
        Self {
            name: name.to_string(),
            transport: transport.to_string(),
            connected: true,
            tool_count,
            error: None,
            stderr_tail: Vec::new(),
        }
    }

    /// A server that failed to start or discover, with the reason and
    /// any captured stderr.
    pub fn failed(name: &str, transport: &str, error: String, stderr_tail: Vec<String>) -> Self {
        Self {
            name: name.to_string(),
            transport: transport.to_string(),
            connected: false,
            tool_count: 0,
            error: Some(error),
            stderr_tail,
        }
    }
}

/// The **editable configuration** of one `[[mcp_server]]` entry — distinct
/// from `McpServerStatusView` (the connection's live runtime status).
/// `env`/`headers` are NOT secrets on this wire type — see
/// `crates/aivyx-config/src/lib.rs`'s own doc comment on `McpServerConfig`:
/// the intended operator practice is a `${VAR}` placeholder, resolved from
/// the daemon's own environment at load time, not a literal secret value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpServerConfigView {
    pub name: String,
    /// `"stdio"`, `"sse"`, or `"http"`.
    pub transport: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub headers: Vec<(String, String)>,
    pub url: Option<String>,
    pub enabled: bool,
}

/// Command Center — one scheduled background routine for the dashboard.
/// Wasm-clean (the Studio renders it directly). The agent's `[[schedule]]`
/// entries (e.g. the default starter routines) become these views: the cron
/// cadence plus when each last fired and next fires, so the dashboard shows a
/// live agent working on its own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScheduleView {
    /// Display name (the schedule id with any `cfg-` config prefix stripped).
    pub name: String,
    /// The raw 6-field (seconds-first) cron expression, e.g. `0 0 7 * * *`.
    pub cron: String,
    /// Role the routine runs as.
    pub role: String,
    pub enabled: bool,
    /// Last actual fire (unix ms); `None` if it has never fired.
    #[serde(default)]
    pub last_fired_unix_ms: Option<u64>,
    /// Next scheduled fire (unix ms); `None` if the cron yields no future time.
    #[serde(default)]
    pub next_fire_unix_ms: Option<u64>,
    /// Chapter Chime — the full storage id (`cfg-`-prefixed for config
    /// routines); mutation messages address schedules by this id.
    #[serde(default)]
    pub schedule_id: String,
    /// Chapter Chime — creation provenance: `"config" | "operator" | "agent"`.
    #[serde(default)]
    pub created_by: String,
    /// Chapter Chime — the prompt the routine fires (empty for
    /// deterministic report routines).
    #[serde(default)]
    pub prompt: String,
}

/// Response payload mirroring [`QueryPayload`]. Wrapped in
/// [`DaemonMessage::QueryResponse`] with the same correlation `id`
/// the query was sent with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
// Phase 95 — `LearningInsights` accumulates ~14 optional
// stat fields across phases 78-93 plus the Phase 95 cadence
// vec. Boxing each Option<Stat> would churn serde wire
// formats for marginal benefit (the variant is heap-
// allocated in practice — most fields are `None` or short
// `Vec`s). The size disparity is an artifact of the
// wire-compat-via-additive-fields pattern the project uses.
#[allow(clippy::large_enum_variant)]
pub enum QueryResponsePayload {
    /// Response to [`QueryPayload::ListSessions`].
    ListSessions {
        sessions: Vec<SessionSummary>,
    },
    /// Response to [`QueryPayload::ListMissions`].
    ListMissions {
        missions: Vec<MissionSummary>,
    },
    /// Response to [`QueryPayload::GetMission`]. `mission` is `None`
    /// when the mission id does not exist (not an error).
    GetMission {
        mission: Option<MissionDetail>,
    },
    /// Response to [`QueryPayload::ListAuditEntries`]. `entries` is the
    /// page of summaries; `total_len` is the full chain length so the
    /// frontend can show "showing N..M of T" and know when to stop
    /// paginating.
    ListAuditEntries {
        entries: Vec<AuditEntrySummary>,
        total_len: u64,
    },
    /// Response to [`QueryPayload::VerifyAuditChain`].
    VerifyAuditChain {
        ok: bool,
        entries_verified: u64,
        /// Human-readable failure description on `ok == false`.
        error: Option<String>,
    },
    /// The daemon could not answer the query. `code` is a stable
    /// machine-readable label; `message` is human-readable.
    QueryError {
        code: String,
        message: String,
    },
    /// Response to [`QueryPayload::GetProfile`]. Phase 58 — the
    /// daemon's currently-loaded Profile snapshot. The Web UI
    /// renders this into the Profile pane mirroring `aivyx-pa profile
    /// show`. Always populated — even on a daemon with no `[profile]`
    /// section in TOML, the synthesized default is returned (the
    /// snapshot includes `injection_enabled = false` in that case).
    GetProfile {
        profile: ProfileSummary,
    },
    /// Response to [`QueryPayload::GetEffectivePersona`]. Phase 60
    /// — the current folded Persona state. Always populated; an
    /// empty Persona returns an [`EffectivePersonaSummary`] with all
    /// fields empty / `None`.
    GetEffectivePersona {
        persona: EffectivePersonaSummary,
    },
    /// Response to [`QueryPayload::ExportPersonaChain`]. Phase 64.
    /// Full-fidelity chain in a single response for the
    /// `aivyx-pa identity export` flow. `deltas` is the chain in
    /// order; `effective` is the folded state at export time
    /// (the export bundle embeds this as `effective_at_export`
    /// per Q5(a)).
    ExportPersonaChain {
        deltas: Vec<crate::DeltaExport>,
        effective: crate::EffectivePersona,
    },
    /// Response to [`QueryPayload::ListPersonaDeltas`]. Phase 60
    /// — paginated page of approved deltas. `total_len` is the full
    /// chain length so the frontend knows when to stop paginating.
    ListPersonaDeltas {
        entries: Vec<PersonaDeltaSummary>,
        total_len: u64,
    },
    /// Phase 70 — response to [`QueryPayload::ListPersonaProposals`].
    /// `proposals` is the filtered page; `total_len` is the total
    /// number of proposals matching the filter (not capped by
    /// `limit`).
    ListPersonaProposals {
        proposals: Vec<PersonaProposalSummary>,
        total_len: u64,
    },
    /// Phase 70 — response to [`QueryPayload::GetPersonaProposal`].
    /// `proposal` is `None` when the id does not exist.
    GetPersonaProposal {
        proposal: Option<PersonaProposalSummary>,
    },
    /// Phase 73 — response to [`QueryPayload::ListNotificationHistory`].
    /// `entries` is the filtered page; `total_len` is the total
    /// number of `AutoNotifyDispatched` audit events matching
    /// the filter (uncapped by `limit`).
    ListNotificationHistory {
        entries: Vec<NotificationHistoryEntry>,
        total_len: u64,
    },
    /// Chapter Herald — response to [`QueryPayload::GetNotifyTargets`].
    GetNotifyTargets {
        targets: Vec<NotifyTargetView>,
    },
    /// Response to [`QueryPayload::GetNotifyTargetConfigs`].
    GetNotifyTargetConfigs { targets: Vec<NotifyTargetConfigView> },
    /// Response to [`QueryPayload::SetNotifyTarget`] / [`QueryPayload::
    /// DeleteNotifyTarget`]. Carries the fresh list and `restart_required`
    /// (always `true` — notify targets are boot-constructed).
    NotifyTargetsApplied {
        targets: Vec<NotifyTargetConfigView>,
        restart_required: bool,
    },
    /// Phase 74 — response to [`QueryPayload::ListMemoryTopics`].
    /// Distinct topic names sorted ascending.
    ListMemoryTopics {
        topics: Vec<String>,
    },
    /// Chapter MG — response to [`QueryPayload::GetMemoryGraph`]. `nodes` are the
    /// topics (with entry counts); `edges` are the weighted co-occurrence pairs
    /// (`crate::PairScore`, reused as-is) — empty when co-occurrence is unarmed.
    GetMemoryGraph {
        nodes: Vec<MemoryGraphNode>,
        edges: Vec<crate::PairScore>,
    },
    /// Chapter Codex — response to [`QueryPayload::ListWikiPages`]:
    /// compact page rows, most-recent first.
    ListWikiPages {
        pages: Vec<crate::wiki::WikiPageSummary>,
    },
    /// Chapter Codex — response to [`QueryPayload::GetWikiPage`]: the
    /// full page, or `None` when the topic has no page yet.
    GetWikiPage {
        page: Option<crate::wiki::WikiPage>,
    },
    /// Chapter Lattice — response to [`QueryPayload::GetKnowledgeGraph`]:
    /// `entities` are the nodes (with degree); `edges` are the directed
    /// typed relations. Both empty until the extraction sweep has run.
    GetKnowledgeGraph {
        entities: Vec<crate::graph::GraphEntity>,
        edges: Vec<crate::graph::GraphTriple>,
    },
    /// Chapter Concord — response to [`QueryPayload::GetMemoryConflicts`]:
    /// the detected contradictions (each a `(topic, older, newer, reason)`
    /// tuple), empty when none are found. Transient — recomputed each call.
    MemoryConflicts {
        conflicts: Vec<crate::conflict::MemoryConflict>,
    },
    /// Chapter Accord — response to [`QueryPayload::GetSoulConflicts`]: the
    /// detected Persona contradictions, empty when none. Transient —
    /// recomputed each call.
    SoulConflicts {
        conflicts: Vec<crate::soul_conflict::SoulConflict>,
    },
    /// Chapter Repertoire — response to [`QueryPayload::GetSkills`]: the
    /// skill inventory (each `LearnedSkill` + its effectiveness) and the
    /// count of pending skill proposals (governed in the Agents screen).
    GetSkills {
        skills: Vec<SkillView>,
        pending_proposals: usize,
    },
    /// Chapter Almanac — response to [`QueryPayload::GetToolCatalog`]:
    /// every tool in the daemon's live registry, for the Studio's
    /// read-only browse/search screen. Unordered — the Studio sorts
    /// client-side.
    GetToolCatalog {
        tools: Vec<ToolCatalogEntry>,
    },
    /// Chapter Lantern — response to [`QueryPayload::GetMcpStatus`]: each
    /// configured MCP server's last-start health, plus the unix time the
    /// snapshot was captured (`0` when no snapshot exists yet — the
    /// daemon hasn't started with any `[[mcp_server]]` configured).
    GetMcpStatus {
        captured_unix: u64,
        servers: Vec<McpServerStatusView>,
    },
    /// Response to [`QueryPayload::GetMcpServerConfigs`].
    GetMcpServerConfigs { servers: Vec<McpServerConfigView> },
    /// Response to [`QueryPayload::SetMcpServer`] / [`QueryPayload::
    /// DeleteMcpServer`]. Carries the **fresh** list (re-read from disk)
    /// and `restart_required` (always `true` — MCP servers are
    /// boot-constructed).
    McpServersApplied {
        servers: Vec<McpServerConfigView>,
        restart_required: bool,
    },
    /// Response to [`QueryPayload::TestMcpServerConnection`]. `error` is
    /// `None` iff `ok` — the raw connection error string otherwise (not a
    /// `QueryError`, since a failed test-connection is an expected,
    /// non-exceptional outcome the form should just display inline).
    McpServerTestResult {
        ok: bool,
        tool_count: usize,
        #[serde(default)]
        error: Option<String>,
    },
    /// Command Center — response to [`QueryPayload::GetSchedules`]: the agent's
    /// scheduled background routines for the dashboard.
    Schedules {
        schedules: Vec<ScheduleView>,
    },
    /// Phase 74 — response to [`QueryPayload::GetMemoryTopicEntries`].
    /// Newest-first paginated entries for one topic.
    GetMemoryTopicEntries {
        entries: Vec<MemoryEntrySummary>,
    },
    /// Phase 74 — response to [`QueryPayload::SearchMemory`].
    /// Matching entries newest-first.
    SearchMemory {
        matches: Vec<MemoryEntrySummary>,
        /// Phase 75 — `true` when a `semantic` request was
        /// transparently served by the keyword path (no
        /// `[embedding]` config, provider call failed, or the
        /// corpus has zero vectors). `#[serde(default)]` so
        /// older frames decode as `false`.
        #[serde(default)]
        fell_back_to_keyword: bool,
    },
    /// Phase 78 — response to
    /// [`QueryPayload::GetLearningInsights`]. The digest is the
    /// per-window operational picture; `proposals` is the
    /// reconstructed provenance for each recall-driven Persona
    /// proposal. An empty digest (zero recalls) is a valid
    /// "nothing learned yet" answer, not an error.
    LearningInsights {
        digest: crate::LearningDigest,
        proposals: Vec<crate::ProposalProvenance>,
        /// Phase 79 (Q4a) — the last turn's adaptive-Persona
        /// selection (selected/total facets), or `None` if no
        /// adaptive selection has run (no `[embedding]`, small
        /// Soul, or pre-Phase-79). `#[serde(default)]` so older
        /// frames decode.
        #[serde(default)]
        persona_selection:
            Option<crate::PersonaSelectionStat>,
        /// Phase 80 (Q4a) — the last proactive cycle's outcome
        /// (what was surfaced + why, deduped/capped counts), or
        /// `None` if proactive has not run (off / no schedule /
        /// pre-Phase-80). `#[serde(default)]` so older frames
        /// decode.
        #[serde(default)]
        proactive:
            Option<crate::ProactiveStat>,
        /// Phase 81 (Q4a) — the last persona-lifecycle cycle's
        /// outcome (what was proposed for consolidation/decay +
        /// why, deduped count), or `None` if the lifecycle pass
        /// has not run (off / no schedule / pre-Phase-81).
        /// `#[serde(default)]` so older frames decode.
        #[serde(default)]
        persona_lifecycle: Option<
            crate::PersonaLifecycleStat,
        >,
        /// Phase 82 — the durable, decayed accumulated
        /// per-topic helpfulness (the longitudinal view Phase
        /// 78 deferred, distinct from the windowed
        /// `digest.top_helpful`). `None` if the ledger is
        /// absent / empty (no auto-recall, or pre-Phase-82).
        /// `#[serde(default)]` so older frames decode.
        #[serde(default)]
        accumulated_helpfulness: Option<
            crate::AccumulatedHelpfulness,
        >,
        /// Phase 83 — the durable cross-session co-occurrence
        /// patterns (topics that consistently help together).
        /// `None` if the ledger is absent / empty (no
        /// auto-recall, or pre-Phase-83). `#[serde(default)]`
        /// so older frames decode.
        #[serde(default)]
        cooccurrence: Option<
            crate::CooccurrencePatterns,
        >,
        /// Phase 84 — the last turn's cluster-aware co-recall
        /// outcome (driver→sibling pairs injected). `None` if
        /// cluster expansion is off / has not run this daemon
        /// lifetime. `#[serde(default)]` so older frames
        /// decode.
        #[serde(default)]
        cluster_recall: Option<
            crate::RecallClusterStat,
        >,
        /// Phase 87 — the last reflection cycle's pattern-
        /// driven Persona consolidation outcome (filed pairs +
        /// the LLM-availability flag). `None` if
        /// `[persona_consolidation]` is off, no cycle has
        /// fired this daemon lifetime, or the substrate is
        /// missing. `#[serde(default)]` so older frames decode.
        #[serde(default)]
        persona_consolidation: Option<
            crate::PersonaConsolidationStat,
        >,
        /// Phase 172 — durable accumulated correction view
        /// (the topics the operator most often reworks). `None`
        /// if the correction ledger is absent / empty (no
        /// auto-recall, or pre-Phase-172). `#[serde(default)]`
        /// so older frames decode.
        #[serde(default)]
        accumulated_corrections: Option<
            crate::AccumulatedCorrections,
        >,
        /// Phase 172 — the last reflection cycle's correction-
        /// driven Persona consolidation outcome (filed topics +
        /// the LLM-availability flag). `None` if
        /// `[correction_consolidation]` is off, no cycle has
        /// fired this daemon lifetime, or the substrate is
        /// missing. `#[serde(default)]` so older frames decode.
        #[serde(default)]
        correction_consolidation: Option<
            crate::CorrectionConsolidationStat,
        >,
        /// Phase 178 — last reflection cycle's correction-
        /// judgment outcome (judged / rework / praise /
        /// unrelated / structural-fallback counts). `None` when
        /// `[correction_judgment]` is off / the fold hasn't run.
        /// `#[serde(default)]` so older frames decode.
        #[serde(default)]
        correction_judgment: Option<
            crate::CorrectionJudgmentStat,
        >,
        /// Phase 91 — last reflection cycle's LLM-judged
        /// recall outcome (per-classification counts +
        /// `(topic, judgment)` pairs + the
        /// `llm_unavailable` flag). `None` when the
        /// `[recall_judgment]` section is off / the pass
        /// has never run. `#[serde(default)]` so older
        /// frames decode unchanged.
        #[serde(default)]
        recall_judgment: Option<
            crate::RecallJudgmentStat,
        >,
        /// Phase 95 — per-schedule accumulating cadence
        /// stats. Each entry is `(schedule_name,
        /// RecentReflectionStat { fired, skipped })`.
        /// Empty `Vec` when no schedule has ever made a
        /// cadence decision (the in-memory map starts
        /// empty; first cycle decisions populate it).
        /// `#[serde(default)]` so older frames decode
        /// unchanged.
        #[serde(default)]
        cadence: Vec<(
            String,
            crate::RecentReflectionStat,
        )>,
    },
    /// Phase 102 — response to [`QueryPayload::GetToolStats`]. One
    /// [`ToolStat`] per tool, ordered by call count descending then
    /// name ascending. An empty `Vec` is a valid "no tools, no
    /// calls" answer, not an error.
    ToolStats {
        tools: Vec<ToolStat>,
    },
    /// Phase 119 Task 6 — response to
    /// [`QueryPayload::DumpToolRelevance`]. Flat per-row table
    /// rather than per-keyword-key nested entries: the operator's
    /// CLI renders one table row per `(keyword_key, surface_kind,
    /// identifier)` triple, so the wire shape pre-flattens.
    /// An empty `Vec` is a valid "no entries" answer, not an
    /// error. Rows are ordered ascending by
    /// `(keyword_key, surface_kind, identifier)` so the operator's
    /// table renders in a stable column order.
    ToolRelevanceDump {
        rows: Vec<ToolRelevanceDumpRow>,
    },
    /// Phase 173 — response to [`QueryPayload::LoopAdd`]. Carries
    /// the new story's id.
    LoopStoryAdded {
        story_id: String,
    },
    /// Phase 173 — response to [`QueryPayload::LoopList`]. Every
    /// backlog story, insertion order.
    LoopBacklog {
        stories: Vec<crate::Story>,
    },
    /// Phase 173 — response to [`QueryPayload::LoopStart`] /
    /// [`QueryPayload::LoopStop`]. `ok` is whether the control
    /// action took effect; `message` is the operator-readable
    /// result either way.
    LoopControl {
        ok: bool,
        message: String,
    },
    /// Phase 173 — response to [`QueryPayload::LoopStatus`]. The
    /// run state plus the live remaining-pending count.
    LoopStatus {
        state: crate::LoopRunState,
        remaining: usize,
        /// Whether the `[loop]` section is armed (the driver was
        /// spawned). When `false`, `loop start` will fail.
        armed: bool,
        /// Phase 174 — whether driver-side gate verification is
        /// configured (`[loop].gate_command` set). `#[serde(default)]`
        /// so pre-174 frames decode.
        #[serde(default)]
        gate_enabled: bool,
        /// Phase 174 — the wall-clock cap in seconds, if any.
        /// `#[serde(default)]` so pre-174 frames decode.
        #[serde(default)]
        max_run_secs: Option<u64>,
        /// Phase 176 — the per-run token budget, if any.
        /// `#[serde(default)]` so pre-176 frames decode.
        #[serde(default)]
        max_run_tokens: Option<u64>,
        /// Chapter K (K.4.2) — the per-run dollar cap, if any. The
        /// live spend rides `state.spent_cents`; this carries the cap
        /// value so `aivyx-pa loop status` can show "spend / cap".
        /// `#[serde(default)]` so pre-K.4.2 frames decode.
        #[serde(default)]
        max_run_usd: Option<f64>,
        /// Chapter Circuit (CI.5) — the cross-iteration stall-breaker
        /// threshold (`[loop] max_idle_iterations`; `0` = disabled).
        /// The live consecutive-idle count rides
        /// `state.consecutive_idle`; this carries the configured
        /// threshold so `aivyx-pa loop status` can show "idle / threshold".
        /// `#[serde(default)]` so pre-Circuit frames decode.
        #[serde(default)]
        max_idle_iterations: u32,
    },
    /// Phase 175 — response to [`QueryPayload::LoopLog`]. Recent
    /// progress notes, most-recent-first.
    LoopProgressLog {
        notes: Vec<String>,
    },
    /// Chapter L (L.5) — response to [`QueryPayload::TeamRun`]. The new
    /// mission's id; the drive runs in the background (poll `TeamMissionStatus`).
    TeamRunStarted {
        mission_id: String,
    },
    /// Chapter L (L.5) — response to [`QueryPayload::TeamMissionList`]. Every
    /// known mission's full record (plan + checkpoint + phase), as the loop's
    /// `LoopBacklog` carries `Story`s. The TUI maps these → `MissionRow`s (L.6).
    TeamMissionList {
        missions: Vec<crate::TeamMissionRecord>,
    },
    /// Chapter L (L.5) — response to [`QueryPayload::TeamMissionStatus`].
    /// `None` when the id is unknown.
    TeamMissionStatus {
        mission: Option<crate::TeamMissionRecord>,
    },
    /// Chapter L (L.5) — response to [`QueryPayload::ResolveTeamGate`]. The
    /// phase the decision moved the mission to (`Executing` on approve — the
    /// resume drives in the background — or `Rejected`).
    TeamGateResolved {
        mission_id: String,
        phase: crate::TeamMissionPhase,
    },
    /// Chapter Belay — response to [`QueryPayload::AbortTeamMission`]. A short
    /// human-readable status (the mission will halt at its next wave boundary).
    TeamMissionAborted {
        mission_id: String,
        message: String,
    },
    /// Chapter Mission Control — response to
    /// [`QueryPayload::PauseTeamMission`]. A short human-readable status
    /// (the mission will pause at its next wave boundary).
    TeamMissionPaused {
        mission_id: String,
        message: String,
    },
    /// Chapter Mission Control — response to
    /// [`QueryPayload::ResumeTeamMission`]. The phase the resume moved the
    /// mission to (always `Executing` — the resume drives in the
    /// background).
    TeamMissionResumed {
        mission_id: String,
        phase: crate::TeamMissionPhase,
    },
    /// Chapter Y — response to [`QueryPayload::GetTeamRoster`]. The daemon's
    /// active team configuration, rendered as-is by the Studio's Teams screen.
    GetTeamRoster {
        roster: aivyx_team_types::TeamConfig,
    },
    /// Chapter Z — response to [`QueryPayload::ListDir`]. `entries` is the
    /// directory's contents (dirs first); `path` echoes the listed relative path
    /// so the browser can confirm/render the breadcrumb.
    ListDir {
        entries: Vec<DocEntry>,
        path: String,
    },
    /// Chapter Z — response to [`QueryPayload::ReadFile`].
    ReadFile {
        file: DocFile,
    },
    /// Chapter DW — shared response to a Documents mutation (`WriteFile` /
    /// `DeleteFile` / `RenamePath` / `MakeDir`). `ok = false` carries a
    /// human-readable `error` (the web re-`ListDir`s the directory on success).
    FsMutation {
        ok: bool,
        error: Option<String>,
    },
    /// Response to [`QueryPayload::GetSettings`]. Chapter U — the daemon's
    /// effective config snapshot for the Settings screen.
    GetSettings {
        settings: SettingsSnapshot,
    },
    /// Response to [`QueryPayload::SetAccessLevel`] / [`QueryPayload::SetBudget`].
    /// Chapter U — carries the **fresh** snapshot (re-read from disk so the UI
    /// re-renders from authoritative state) and `restart_required`: `true`
    /// whenever the change is load-time and won't apply to the running daemon
    /// until it restarts (always the case today).
    SettingsApplied {
        settings: SettingsSnapshot,
        restart_required: bool,
    },
    /// Response to [`QueryPayload::AllowCloudEscalation`]: consent recorded
    /// for `session_id` (in-memory, this daemon process only).
    CloudEscalationAllowed { session_id: String },
    /// Response to [`QueryPayload::AllowCloudEscalation`] when the daemon
    /// has no cloud escalation configured (nothing was recorded).
    CloudEscalationNotEnabled,
    /// Response to [`QueryPayload::SetProfile`]. Chapter V — carries the
    /// **fresh** `ProfileSummary` (re-read from disk so the editor re-renders
    /// from authoritative state) and `restart_required` (always `true` today —
    /// Profile is load-time, like the Settings writes).
    ProfileApplied {
        profile: ProfileSummary,
        restart_required: bool,
    },
    /// Response to [`QueryPayload::GetVoiceSettings`]. Chapter Voice.
    GetVoiceSettings {
        settings: VoiceSettingsSnapshot,
    },
    /// Response to [`QueryPayload::SetVoice`]. Chapter Voice — the **fresh**
    /// snapshot (re-read from disk, readiness re-stat'd) + `restart_required`
    /// (always `true`; `[voice]` is load-time).
    VoiceApplied {
        settings: VoiceSettingsSnapshot,
        restart_required: bool,
    },
    /// Response to [`QueryPayload::SetTeamRoster`]. Chapter Roster — the
    /// **validated, persisted** roster (re-read from disk so the Teams screen
    /// re-renders from authoritative state) + `restart_required` (always `true`
    /// today — the team service is boot-assembled, like the Settings writes).
    TeamRosterApplied {
        roster: aivyx_team_types::TeamConfig,
        restart_required: bool,
    },
    /// Response to [`QueryPayload::GetGallery`]. `available = false` when no
    /// `comfyui`-named `[[mcp_server]]` is configured (no network call is
    /// made in that case) — the Studio Gallery screen renders an empty
    /// state rather than an error.
    Gallery {
        available: bool,
        images: Vec<GalleryImage>,
    },
    GetEmailConfig { config: EmailConfigView },
    EmailConfigApplied { config: EmailConfigView, restart_required: bool },
    GetTelegramConfig { config: TelegramConfigView },
    TelegramConfigApplied { config: TelegramConfigView, restart_required: bool },
    GetDiscordConfig { config: DiscordConfigView },
    DiscordConfigApplied { config: DiscordConfigView, restart_required: bool },
    GetSlackConfig { config: SlackConfigView },
    SlackConfigApplied { config: SlackConfigView, restart_required: bool },
    /// Response to [`QueryPayload::GetReflectionScheduleConfigs`].
    GetReflectionScheduleConfigs { schedules: Vec<ReflectionScheduleConfigView> },
    /// Response to [`QueryPayload::SetReflectionSchedule`] /
    /// [`QueryPayload::DeleteReflectionSchedule`]. Carries the fresh
    /// list and `restart_required` (config is load-time), mirroring
    /// `NotifyTargetsApplied`'s own shape.
    ReflectionScheduleConfigApplied { schedules: Vec<ReflectionScheduleConfigView>, restart_required: bool },
    GetMemoryProfileConfig { config: MemoryProfileConfigView },
    MemoryProfileConfigApplied { config: MemoryProfileConfigView, restart_required: bool },
    GetEmbeddingConfig { config: EmbeddingConfigView },
    EmbeddingConfigApplied { config: EmbeddingConfigView, restart_required: bool },
    GetProactiveConfig { config: ProactiveConfigView },
    ProactiveConfigApplied { config: ProactiveConfigView, restart_required: bool },
    /// Response to [`QueryPayload::GetMcpServerCallStats`].
    McpServerCallStats { servers: Vec<McpServerCallStats> },
    /// Response to [`QueryPayload::GetReminders`].
    Reminders { reminders: Vec<ReminderView> },
}

/// Studio Gallery — one ComfyUI generation, read from `/history`. Wasm-clean
/// (the Studio renders it directly); `caption` and `created_unix` are
/// best-effort (`None` when the node graph/status shape doesn't match what
/// we know how to parse).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GalleryImage {
    pub prompt_id: String,
    pub filename: String,
    pub subfolder: String,
    /// ComfyUI's own `type` field for `/view` — `output`/`input`/`temp`.
    pub folder_type: String,
    pub created_unix: Option<u64>,
    pub caption: Option<String>,
}

/// Chapter Voice — the daemon's `[voice]` config + readiness snapshot for the
/// Voice screen. Wasm-clean plain-field mirror of `aivyx_config::VoiceOptions`
/// (paths as strings) plus readiness flags the daemon computes by `stat`ing the
/// model prerequisites. (Chapter Timbre: the TTS prereqs are the Kokoro model
/// directory's `.onnx` + `voices-*.bin`, not the old Piper voice/espeak paths.)
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct VoiceSettingsSnapshot {
    pub asr_engine: Option<String>,
    pub tts_engine: Option<String>,
    pub asr_model_path: Option<String>,
    pub asr_language: Option<String>,
    pub asr_beam_size: Option<u32>,
    pub tts_model_dir: Option<String>,
    pub tts_voice_name: Option<String>,
    pub tts_speed: Option<f32>,
    pub input_device: Option<String>,
    pub output_device: Option<String>,
    /// `"present" | "missing" | "unset"` for the Whisper `.bin` model.
    pub asr_model_status: String,
    /// `"present" | "missing" | "unset"` for a `*.onnx` in the Kokoro model dir.
    pub tts_model_status: String,
    /// `"present" | "missing" | "unset"` for a `voices-*.bin` in the model dir.
    pub tts_voices_status: String,
}

/// Chapter U — the daemon's effective config snapshot for the Settings screen.
/// Wasm-clean plain-field mirror (no `aivyx-config` / `aivyx-cost` dep): the
/// access half mirrors `aivyx-pa access show`, the budget half mirrors the
/// `[budget]` caps, and the provider half is read-only context (changed via
/// `aivyx-pa init`, not this screen — see `docs/FRONTEND.md` §8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettingsSnapshot {
    /// `sandbox | workspace | home | full | custom` (matches `[access] level`).
    pub access_level: String,
    /// The resolved filesystem reach the level derives (display string).
    pub fs_root: String,
    /// Whether irreversible fs ops confirm first (the expanded-level posture).
    pub confirm_destructive: bool,
    /// Provider label — `anthropic | openai | ollama | …` (read-only here).
    pub provider: String,
    /// Active model id (read-only here).
    pub model: String,
    /// Context window in tokens when known (`num_ctx`), else `None`.
    pub num_ctx: Option<u32>,
    /// The `[budget]` dollar caps + alert posture.
    pub budget: BudgetSnapshot,
    /// Whether an embedding provider is configured (drives the Memory
    /// semantic-search availability the operator sees elsewhere).
    pub embeddings_available: bool,
    /// `[agent] cycle_detection` — whether the small-cycle breaker is armed for
    /// the interactive agent (`false` ⇒ off, the default). Autonomous team
    /// agents always have it on regardless; this knob is the interactive toggle.
    pub cycle_detection: bool,
    /// Chapter Reins — `[autonomy] level` (`manual | assisted | supervised |
    /// autonomous | unleashed`; matches the dial). `assisted` is the default.
    pub autonomy_level: String,
}

/// Chapter U — wire mirror of the `[budget]` caps (a plain-field copy of
/// `aivyx_cost::BudgetConfig` so `aivyx-ipc` stays wasm-clean). A `None` cap
/// is uncapped; `on_exceeded` is `alert | deny`; `alert_at` is the
/// early-warning fraction (`None` ⇒ no early-warning tier).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BudgetSnapshot {
    pub per_run_usd: Option<f64>,
    pub per_day_usd: Option<f64>,
    pub on_exceeded: String,
    pub alert_at: Option<f64>,
}

/// A secret field's wire representation on the READ side, for every
/// config-write consumer in POLISH_WAVES.md sub-project 7 (MCP env/header
/// values are NOT secrets by this codebase's convention — see
/// `McpServerConfigView`'s own doc comment — so this type's first real use
/// is the Notify-target/channel-adapter and Settings-coverage plans, not
/// this one). Never carries the real value: `configured` says whether a
/// value exists at all, `source` says where it came from (`"toml"`,
/// `"env"`, ...) — mirrors `aivyx_config::SourcedSecret`'s own `Debug`
/// redaction and its `FieldSource` provenance, on the wire.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RedactedSecret {
    pub configured: bool,
    pub source: String,
}

/// The **editable configuration** of the shared `[email]` SMTP block.
/// `password` never carries the real value (see [`RedactedSecret`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmailConfigView {
    pub host: Option<String>,
    pub port: Option<u16>,
    pub tls_mode: Option<String>,
    pub username: Option<String>,
    pub password: RedactedSecret,
    pub from: Option<String>,
}

/// The **editable configuration** of the `[telegram]` channel adapter.
/// `token` never carries the real value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TelegramConfigView {
    pub token: RedactedSecret,
    pub chat_id: Option<i64>,
    pub team_run_channel: bool,
    pub team_trigger_rate_limit: Option<u32>,
    pub team_command_allowed_senders: Vec<i64>,
}

/// The **editable configuration** of the `[discord]` channel adapter.
/// `token` never carries the real value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DiscordConfigView {
    pub token: RedactedSecret,
    pub application_id: Option<u64>,
    pub team_run_channel: bool,
    pub team_trigger_rate_limit: Option<u32>,
    pub team_command_allowed_senders: Vec<u64>,
}

/// The **editable configuration** of the `[slack]` channel adapter. Two
/// independent secrets, neither ever carrying its real value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SlackConfigView {
    pub bot_token: RedactedSecret,
    pub app_token: RedactedSecret,
    pub team_id: Option<String>,
    pub team_run_channel: bool,
    pub team_trigger_rate_limit: Option<u32>,
    pub team_command_allowed_senders: Vec<String>,
}

/// Phase 119 Task 6 — wire-format per-row dump entry for
/// [`QueryResponsePayload::ToolRelevanceDump`]. Flattens the
/// `(keyword_key, OutcomeRow)` pair so the CLI renders one
/// table row per entry without nested decode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolRelevanceDumpRow {
    /// The Phase 116 keyword key the outcome was recorded under.
    pub keyword_key: String,
    /// `"tool"` or `"skill"` (matches `RelevanceSurfaceKind::label()`).
    pub surface_kind: String,
    /// The tool or skill identifier (e.g. `fs.read`,
    /// `summarize-pdf`).
    pub identifier: String,
    pub success_count: u32,
    pub failure_count: u32,
    pub last_seen_unix_ms: u64,
}

/// Phase 102 — wire-format per-tool observability row for
/// [`QueryResponsePayload::ToolStats`]. One row per tool: the
/// registry-listing fields (`name`, `description`, `scope_base`,
/// `registered`) joined with the audit-derived call statistics.
/// The `aivyx-pa tools` CLI renders one table row per `ToolStat`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolStat {
    /// Tool name as the planner advertises it (e.g. `fs.read`).
    pub name: String,
    /// One-line tool description.
    pub description: String,
    /// Capability base the tool's calls key on in the audit chain
    /// (`AuditEvent::ToolCall`'s `scope_used.base()`).
    pub scope_base: String,
    /// `true` when the tool is in the daemon's live registry. A
    /// `false` row is a base with audit history but no currently
    /// registered tool (a channel/role change, or a removed tool).
    pub registered: bool,
    /// Total `AuditEvent::ToolCall` events for this base within
    /// the requested window.
    pub calls: u64,
    /// Per-outcome counts, keyed by the stable outcome label
    /// (`completed`, `failed`, `denied`, `not_in_role`,
    /// `requires_escalation`). A key is absent when its count is
    /// zero; the present values sum to `calls`.
    pub outcomes: std::collections::BTreeMap<String, u64>,
    /// Total wall-clock duration across all `calls`, in
    /// milliseconds. The average is `total_duration_ms / calls`,
    /// derived client-side.
    pub total_duration_ms: u64,
}

/// POLISH_WAVES.md sub-project 8 item C — per-MCP-server call
/// statistics, derived from the audit chain the same way [`ToolStat`]
/// is, but grouped by MCP server name (recovered from the `mcp.call`
/// scope's qualifier, `<server>:<tool>`) instead of by capability
/// base. Closes the gap where `ToolStat`'s aggregation — keyed on
/// `Scope::base()` — collapses every configured `[[mcp_server]]`'s
/// tool calls into one shared `"mcp.call"` row, making it impossible
/// to tell which server is actually failing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServerCallStats {
    pub server_name: String,
    /// Total `AuditEvent::ToolCall` events for this server within the
    /// requested window.
    pub calls: u64,
    /// Per-outcome counts, keyed by the same stable outcome label
    /// strings `ToolStat::outcomes` uses -- but unlike `ToolStat`
    /// (which covers all tools and so can carry any of those labels),
    /// only `completed` and `failed` are actually reachable here:
    /// every row is folded from `mcp.call`-scoped `AuditEvent::ToolCall`
    /// events, `ScopeDenied`/`NotInRole`/`RateLimited` are separate
    /// `AuditTag` variants that return early and never produce a
    /// `ToolCall` event at all, and `McpToolProxy::execute`
    /// (`crates/aivyx-mcp/src/proxy.rs`) only ever resolves to
    /// `Completed` or `Failed`. A key is absent when its count is zero.
    pub outcomes: std::collections::BTreeMap<String, u64>,
    /// Total wall-clock duration across all `calls`, in milliseconds.
    pub total_duration_ms: u64,
}

/// Phase 186 — a wasm-clean mirror of `aivyx_channel::reminder_store::
/// Reminder`, carried on the wire by [`QueryResponsePayload::Reminders`].
/// `aivyx-ipc` cannot depend on `aivyx-channel` (the wasm-clean
/// boundary), so this is a plain field-for-field copy, not a shared type
/// — `daemon_server.rs` converts between them with a free function.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReminderView {
    pub id: String,
    /// When the reminder fires, unix seconds.
    pub due_unix: i64,
    pub message: String,
    #[serde(default)]
    pub notify_targets: Vec<String>,
    pub created_unix: i64,
}

/// Phase 74 — wire-format view of one memory entry. Flat shape
/// mirroring the Phase 47 audit / Phase 70 proposal summary
/// patterns; the Web UI Memory pane + `aivyx-pa memory show` CLI
/// render against this type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryEntrySummary {
    pub topic: String,
    pub body: String,
    pub seq: u64,
    pub created_at_secs: u64,
    /// Phase 74 — `last_read_at_secs` so operators can sort the
    /// Memory pane by LRU heat. `0` means "never read since
    /// Phase 74 landed."
    pub last_read_at_secs: u64,
}

/// Chapter MG — one node in the memory knowledge graph: a topic + how many
/// entries it holds (the Studio sizes the node by this).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryGraphNode {
    pub topic: String,
    pub entry_count: u32,
}

/// Phase 73 — flat wire view of one `AuditEvent::AutoNotifyDispatched`
/// entry. Mirrors the shape of `AuditEntrySummary` (Phase 47) but
/// projects the dispatch-specific fields into top-level keys so the
/// Web UI / CLI don't have to dig into a nested JSON.
///
/// `outcome_kind` is the stable string label of
/// `AutoNotifyOutcomeSummary` (`"delivered"`, `"failed"`,
/// `"skipped_empty_response"`, `"skipped_by_condition"`,
/// `"skipped_by_rate_limit"`). `outcome_detail` carries
/// variant-specific data — error message for `failed`, condition
/// label for `skipped_by_condition`, `"<limit>/<window_secs>s"`
/// for `skipped_by_rate_limit`, empty for `delivered` and
/// `skipped_empty_response`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotificationHistoryEntry {
    pub seq: u64,
    pub dispatched_at_unix_ms: u64,
    pub session_id: String,
    pub trigger_kind: String,
    pub trigger_id: String,
    pub target_name: String,
    pub outcome_kind: String,
    pub outcome_detail: String,
}

/// Chapter Herald — read-only wire view of one configured
/// `[[notify_target]]` (or the daemon's synthesized default "studio"
/// target). `kind` is `"telegram" | "webhook" | "email" | "webui"`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyTargetView {
    pub name: String,
    pub kind: String,
    pub is_default: bool,
}

/// The **editable configuration** of one `[[notify_target]]` entry —
/// distinct from `NotifyTargetView` (the read-only status type shown in
/// the Notifications screen's "Targets" rail). No secret fields — the
/// actual credentials for an email-kind target live in the separate
/// `[email]` section (see `EmailConfigView`), and telegram/webhook/email
/// targets here only carry routing data (`chat_id`/`url`/`to`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NotifyTargetConfigView {
    pub name: String,
    pub kind: String,
    pub chat_id: Option<String>,
    pub url: Option<String>,
    pub to: Option<String>,
    pub enabled: bool,
    pub is_default: bool,
    pub retry_count: u32,
    pub retry_backoff_ms_start: u64,
    pub rate_limit_max: Option<u32>,
    pub rate_limit_window_secs: Option<u64>,
}

/// The **editable configuration** of one `[[reflection_schedule]]`
/// entry. `role_override`/`skip_when_idle`/`min_audit_entries_to_fire`
/// stay TOML-only — not represented here; a write preserves them via
/// the `KNOWN_KEYS` mechanism in `write_reflection_schedule_section`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReflectionScheduleConfigView {
    pub name: String,
    pub cron: String,
    pub lookback_window_secs: u64,
    pub enabled: bool,
}

/// The **editable configuration** of `[memory] profile` — a 3-way
/// switch (`"off" | "lite" | "smart"`), not the 2-way lite/smart
/// picker an earlier design pass assumed (`MemoryProfile` has 3
/// variants — see `aivyx-config/src/lib.rs:2706`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryProfileConfigView {
    pub profile: String,
}

/// The **editable configuration** of the `[embedding]` section's
/// primary fields. `api_key` never carries the real value (see
/// [`RedactedSecret`]). `dimensions`/`rag_top_k`/`rag_min_similarity`/
/// `recall_window_turns`/`recall_gate_min_chars` and the Chapter Loom
/// recall-fusion tuning fields stay TOML-only — not represented here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmbeddingConfigView {
    pub base_url: Option<String>,
    pub model: Option<String>,
    pub api_key: RedactedSecret,
}

/// The **editable configuration** of the `[proactive]` section's
/// primary fields. `signals` (the 3 `signal_*` toggles) stays
/// TOML-only — not represented here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProactiveConfigView {
    pub enabled: bool,
    pub target: Option<String>,
    pub max_per_window: u32,
    pub window_secs: u64,
}

/// Wire-safe mirror of `aivyx_core::ChannelPlatform` (Chapter Postern).
///
/// `aivyx-ipc` is documented wasm32-clean (see this crate's package
/// description) and must never depend on `aivyx-core`, which pulls in
/// `aivyx-storage` (redb/filesystem) and `tokio`'s `process` feature —
/// neither wasm32-compatible. Rather than embed the real enum, this
/// crate keeps its own mirror, following the same precedent as
/// `AuditEntrySummary::event_type` above (which mirrors `AuditEvent` as
/// a `String` label for the same reason). `ChannelPlatform` is a small
/// closed enum rather than an open-ended type, so a mirror *enum* is the
/// right shape here — the daemon side (which does depend on
/// `aivyx-core`) is responsible for converting into this type; see
/// `aivyx_channel::daemon_server::to_wire_channel_platform`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum WireChannelPlatform {
    Local,
    Telegram,
    Discord,
    Slack,
    Matrix,
    Email,
    Rest,
    Voice,
}

/// Minimal per-session metadata returned by
/// Chapter Postern/`/classic` retirement — one session as shown on the
/// Studio's Sessions screen: identity, channel, trust posture, and
/// activity timestamps (Unix millis).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionSummary {
    pub session_id: String,
    pub channel: WireChannelPlatform,
    pub trust_tier: aivyx_capability::TrustTier,
    pub created_at_ms: u64,
    pub last_active_at_ms: u64,
}

/// Compact mission view for the dashboard list pane. Mirrors the
/// fields needed for a row in a table; the full record (including
/// gates) is fetched on demand via
/// [`QueryPayload::GetMission`].
///
/// `state` is the rendered string form of `MissionState`
/// (`"Created" | "Running" | "GatePending" | "Completed" |
/// "Failed" | "Cancelled"`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MissionSummary {
    pub mission_id: String,
    pub role_name: String,
    pub description: String,
    pub state: String,
    pub has_pending_gate: bool,
    pub created_at: u64,
    pub updated_at: u64,
}

/// Full mission view including gate history. Returned by
/// [`QueryResponsePayload::GetMission`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MissionDetail {
    pub mission_id: String,
    pub role_name: String,
    pub description: String,
    pub state: String,
    pub gates: Vec<GateSummary>,
    pub created_at: u64,
    pub updated_at: u64,
}

/// One gate within a `MissionDetail`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GateSummary {
    pub gate_id: String,
    pub reason: String,
    pub scope: Option<String>,
    /// `"Pending" | "Approved" | "Rejected"`.
    pub state: String,
    pub created_at: u64,
    pub resolved_at: Option<u64>,
}

/// One row of the audit log as exposed over IPC. Projected from
/// `aivyx_audit::SignedEntry` — the wire shape intentionally keeps
/// the event body as `serde_json::Value` so additions to the
/// `AuditEvent` enum do not break the IPC schema.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditEntrySummary {
    pub seq: u64,
    /// Unix millis. `SystemTime` is converted at the daemon boundary
    /// so the wire format does not depend on platform clock encoding.
    pub appended_at_unix_ms: u64,
    /// String label of the `AuditEvent` variant — `"ToolCall"`,
    /// `"ScopeDenied"`, `"TurnStarted"`, `"TurnEnded"`,
    /// `"MemoryAccess"`. Stable; new variants append new labels.
    pub event_type: String,
    /// Full event payload as JSON. Schema follows `AuditEvent`'s
    /// serde repr.
    pub event: serde_json::Value,
    /// HMAC tag, hex-encoded for display.
    pub mac_hex: String,
}

/// Operator-declared Profile snapshot returned by
/// [`QueryResponsePayload::GetProfile`]. Phase 58 (PRODUCT.md P13).
///
/// Wire-shaped mirror of `aivyx_config::Profile` — flattens
/// `Sourced<T>` into plain serializable fields and pre-computes the
/// `injection_enabled` predicate (the runtime
/// `Profile::is_operator_declared()` result) so the Web UI does not
/// need to re-implement the rule.
///
/// `assistant_name_source` is the stringified [`aivyx_config::FieldSource`]:
/// `"toml"`, `"default"`, `"env"`, or `"encrypted-store"`. Other
/// fields do not carry per-field provenance — Profile fields other
/// than `assistant_name` are either declared in `[profile]` or
/// absent, never sourced from env or the encrypted store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileSummary {
    pub assistant_name: String,
    pub assistant_name_source: String,
    pub operator_profile: Option<String>,
    pub communication_style: Option<String>,
    pub primary_use_cases: Vec<String>,
    pub behavioral_preferences: Vec<String>,
    pub behavioral_constraints: Vec<String>,
    /// `true` when the daemon's `Profile::is_operator_declared()`
    /// returned `true` — i.e. Profile is shaping every turn's system
    /// prompt via `assemble_session_prompt`. `false` means the
    /// substrate is at its passthrough default.
    pub injection_enabled: bool,
}

/// One signed persona delta as it appears over the wire. Mirrors
/// the in-memory `aivyx_channel::persona::SignedPersonaEntry` but
/// flattens the HMAC arrays to hex strings (so the JSON wire format
/// stays uniform with other Summary types) and stringifies the
/// `category` / `op` fields for stable cross-version compatibility.
///
/// Phase 60 — used by the `ListPersonaDeltas` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonaDeltaSummary {
    pub seq: u64,
    pub delta_id: String,
    pub proposed_at_unix_ms: u64,
    pub approved_at_unix_ms: u64,
    pub proposal_id: String,
    /// Stable string label of `PersonaDeltaCategory` — e.g.
    /// `"BehavioralPreferences"`, `"LearnedContext"`.
    pub category: String,
    /// Op as JSON object: `{ "kind": "SetScalar", "value": ... }`,
    /// `{ "kind": "AppendList", "value": "..." }`, etc. The wire
    /// shape mirrors `PersonaDeltaOp`'s serde repr.
    pub op: serde_json::Value,
    pub mac_hex: String,
}

/// Wire-format view of a Persona proposal. Phase 70 — used by
/// `ListPersonaProposals` + `GetPersonaProposal`.
///
/// `proposed_op` is the agent's original proposal (always present);
/// `applied_op` and `applied_seq` are populated only when the
/// proposal's current status is `Approved` (and may differ from
/// `proposed_op` if the operator edited before approving — Q3(a)).
/// `reason` is populated only when the current status is `Rejected`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonaProposalSummary {
    pub id: String,
    pub proposed_at_unix_ms: u64,
    pub source_reflection_session_id: String,
    /// Stable string label, one of `"Pending" | "Approved" |
    /// "Rejected" | "Superseded"`.
    pub status: String,
    /// Stable string label of the proposal's category.
    pub category: String,
    /// Agent's original op as JSON; same shape as
    /// [`PersonaDeltaSummary::op`].
    pub proposed_op: serde_json::Value,
    /// Optional operator-supplied reason the agent gave for
    /// proposing this delta.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed_reason: Option<String>,
    /// On `Approved` proposals only: the op that was actually
    /// applied (may differ from `proposed_op` per Q3(a) edit-on-
    /// approve flow).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_op: Option<serde_json::Value>,
    /// On `Approved` proposals only: seq of the resulting
    /// PersonaDelta in the persona chain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub applied_seq: Option<u64>,
    /// On `Rejected` proposals only: operator-supplied reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejected_reason: Option<String>,
    /// Resolved-at timestamp for `Approved | Rejected |
    /// Superseded` proposals; `None` for `Pending`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at_unix_ms: Option<u64>,
    /// Phase 92 — when this proposal is one half of a
    /// linked supersession pair, the id of the other half.
    /// `None` for proposals that are not part of a
    /// supersession (the common case). Pulled up from the
    /// inner `ProposedPersonaDelta` so the Phase 94
    /// surface-side grouping helper can read it without
    /// re-parsing the embedded `proposed_op` JSON.
    /// `#[serde(default, skip_serializing_if =
    /// "Option::is_none")]` preserves IPC wire-compat — the
    /// established Phase 84 / 91 / 92 pattern.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supersedes_proposal_id: Option<String>,
}

/// Folded effective Persona snapshot. Phase 60 — returned by
/// `GetEffectivePersona`. Mirrors `aivyx_channel::persona::EffectivePersona`
/// shape directly; serializable so the Web UI can render it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EffectivePersonaSummary {
    pub assistant_name: Option<String>,
    pub operator_profile: Option<String>,
    pub communication_style: Option<String>,
    pub primary_use_cases: Vec<String>,
    pub behavioral_preferences: Vec<String>,
    pub behavioral_constraints: Vec<String>,
    pub learned_context: Vec<String>,
    pub communication_adaptations: Vec<String>,
    pub character_traits: Vec<String>,
    pub relationship_milestones: Vec<String>,
    /// Pre-computed flag — `true` when any field is non-empty.
    pub is_non_empty: bool,
}

/// Chapter X — wire mirror of `aivyx_config::PersonaSeed`: the operator's
/// onboarding Persona/Skills seed, carried by `SeedPersona` (the live web seed)
/// and returned by `DraftPersonaSeed` (the LLM draft). Plain fields only — no
/// `aivyx-config` / `aivyx-llm` dep — so the crate stays wasm-clean. Seeds only
/// the *learned* persona categories; the Profile-mirror scalars stay declared
/// in `[profile]`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PersonaSeedWire {
    #[serde(default)]
    pub learned_context: Vec<String>,
    #[serde(default)]
    pub communication_adaptations: Vec<String>,
    #[serde(default)]
    pub character_traits: Vec<String>,
    #[serde(default)]
    pub relationship_milestones: Vec<String>,
    #[serde(default)]
    pub skills: Vec<SeedSkillWire>,
}

/// Chapter X — one starter skill in a [`PersonaSeedWire`]. Mirrors
/// `aivyx_config::SeedSkill`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SeedSkillWire {
    pub name: String,
    pub trigger: String,
    pub procedure: String,
}

/// Chapter Genesis — wire mirror of `aivyx_cli`/`aivyx_channel`'s
/// `DraftedProfile`: the six declared P13 Profile fields the LLM drafts
/// from the operator's onboarding answers. Returned by `DraftProfile`;
/// the operator edits the fields and persists them via the existing
/// `SetProfile` query. Plain fields only — no `aivyx-config` /
/// `aivyx-llm` dep — so the crate stays wasm-clean. Every field is
/// best-effort: a scalar the model omits is `None`, a list it omits is
/// empty, and the operator is always the author of record.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ProfileDraftWire {
    #[serde(default)]
    pub assistant_name: Option<String>,
    #[serde(default)]
    pub operator_profile: Option<String>,
    #[serde(default)]
    pub communication_style: Option<String>,
    #[serde(default)]
    pub primary_use_cases: Vec<String>,
    #[serde(default)]
    pub behavioral_preferences: Vec<String>,
    #[serde(default)]
    pub behavioral_constraints: Vec<String>,
}

/// Chapter Z — one entry in a `ListDir` response. Read-only directory listing
/// for the Studio's Documents browser.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocEntry {
    /// File / directory name (no path — relative to the listed directory).
    pub name: String,
    /// `"dir" | "file" | "symlink" | "other"`.
    pub kind: String,
    /// Size in bytes (`0` for directories).
    pub size_bytes: u64,
}

/// Chapter Z — a file's contents for the Documents viewer. `content` is `None`
/// when the file is binary or larger than the read cap (the UI then shows a
/// "not shown" note from `size_bytes` / `binary`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocFile {
    /// The requested relative path (echoed for the viewer header).
    pub path: String,
    /// Full size on disk in bytes.
    pub size_bytes: u64,
    /// UTF-8 text contents, capped at the read limit; `None` when binary or
    /// over the cap.
    pub content: Option<String>,
    /// `true` when `content` holds only the first `cap` bytes of a larger file.
    pub truncated: bool,
    /// `true` when the file looked binary (NUL byte / invalid UTF-8) — `content`
    /// is then `None`.
    pub binary: bool,
}

// ---------------------------------------------------------------------------
// Frontend → Daemon
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum FrontendMessage {
    StartSession {
        role: Option<String>,
        #[serde(default)]
        frontend_type: Option<FrontendType>,
    },
    SubmitInput {
        session_id: String,
        text: String,
        #[serde(default)]
        mission_id: Option<String>,
        /// Phase 45 — optional image/file attachments. `#[serde(default)]`
        /// ensures old clients that omit this field still deserialize.
        #[serde(default)]
        attachments: Vec<IpcAttachment>,
        /// Chapter H — request an **unattended** turn: at an approval gate the
        /// daemon refuses (records the reason) rather than parking for an
        /// operator. `#[serde(default)]` (false ⇒ interactive) keeps old
        /// clients decoding. The daemon maps this to `GatePolicy::RejectAndAbort`
        /// (a `bool` because `aivyx-ipc` is wasm-clean and can't name the
        /// `aivyx-core` enum).
        #[serde(default)]
        headless: bool,
    },
    CancelTurn {
        session_id: String,
    },
    ResolveGate {
        mission_id: String,
        gate_id: String,
        approved: bool,
    },
    Disconnect,
    Shutdown,
    /// Protocol version negotiation (Phase 41 Task 5).
    /// Sent by the frontend after receiving `DaemonReady`.
    /// For v0.1, the daemon always accepts.
    ProtocolNegotiation {
        version: String,
    },
    /// Phase 47 — read-only inspection query. The daemon answers with
    /// [`DaemonMessage::QueryResponse`] carrying the same `id`.
    Query {
        id: String,
        payload: QueryPayload,
    },
    /// Piece C (2026-08-23) — start a new Nonagon team mission from a
    /// channel's native `/team run <goal>` command. Unlike `Query`,
    /// this is only ever sent over a connection that has already sent
    /// `StartSession` with a real `frontend_type` — the daemon's own
    /// authorization check (Chapter — `ChannelTriggerAuthz` in
    /// `daemon_server.rs`) depends on knowing which channel is asking,
    /// which the anonymous one-shot `Query` path cannot provide.
    /// Responds with [`DaemonMessage::TeamMissionChannelStarted`] or
    /// `Error` (capability-denied, no team-mission service configured,
    /// or a decomposition/start failure).
    RunTeamMissionChannel {
        goal: String,
    },
    /// Phase 60 — operator-initiated Persona revert (PRODUCT.md P14
    /// commit 4). The daemon appends a `Revert` op delta to the
    /// persona chain referencing `target_delta_id` and recomputes
    /// the shared runtime state so the next turn picks it up.
    /// Per Q5(a) at Phase 60 sign-off: reverts are operator-only;
    /// no gate prompt since the operator initiated.
    ///
    /// Reply: [`DaemonMessage::PersonaRevertResolved`] with the
    /// same `id`.
    RevertPersonaDelta {
        id: String,
        target_delta_id: String,
    },
    /// Chapter X — operator-initiated **live** persona seed (the web onboarding
    /// path). Plants the `seed` on the persona chain as operator-authored
    /// approved deltas, **iff the chain is empty** (a fresh agent) — the same
    /// `seed_persona_chain_if_empty` primitive the boot-seed (Chapter W) uses,
    /// so a grown persona is never overwritten. Adopted next-turn (the daemon
    /// recomputes the shared state). LLM-free.
    ///
    /// Reply: [`DaemonMessage::PersonaSeedResolved`] with the same `id`.
    SeedPersona {
        id: String,
        seed: PersonaSeedWire,
    },
    /// Chapter Tutor — operator-initiated skill authoring on a **grown** chain.
    /// The operator (via `aivyx-pa skills teach|update|forget` or the Studio Skills
    /// screen) authors a skill directly; the daemon appends a signed,
    /// operator-authored `LearnedSkill` delta via the same `skill_edit` helpers
    /// the agent tool uses, then recomputes the shared persona (adopted
    /// next-turn). This is the human Kernel-tier authoring path — it needs **no**
    /// agent `skills.write` scope, and the agent's `skills.teach` tool is
    /// unchanged. `trigger`/`procedure` are required for `Teach`, optional for
    /// `Update` (omit to keep the existing value), and ignored for `Forget`.
    ///
    /// Reply: [`DaemonMessage::SkillAuthored`] with the same `id`.
    AuthorSkill {
        id: String,
        op: SkillAuthorOp,
        name: String,
        trigger: Option<String>,
        procedure: Option<String>,
    },
    /// Chapter X — ask the daemon to **draft** a persona seed from the
    /// operator's free-text `description` using the configured model. Read-only
    /// (drafts nothing onto the chain) — it only pre-fills the editable seed
    /// form; the operator confirms via [`SeedPersona`]. LLM-assisted, so it can
    /// fail (no model configured / model error) — the UI falls back to manual
    /// entry.
    ///
    /// Reply: [`DaemonMessage::PersonaSeedDrafted`] with the same `id`.
    DraftPersonaSeed {
        id: String,
        description: String,
    },
    /// Chapter Nonagon Templates — ask the daemon to **draft** a full
    /// 9-member Nonagon (a coordinator lead + 8 specialists) tailored to
    /// the operator's declared role/use-cases (+ optional free-text
    /// `description`), using the configured model. Read-only: the draft
    /// lands in the Studio's existing roster-edit-and-approve draft state
    /// (the same one manual edits use) and is only persisted when the
    /// operator sends [`FrontendMessage::SetTeamRoster`]. LLM-assisted, so
    /// it can fail — the UI falls back to the stock default / manual
    /// editing.
    ///
    /// Reply: [`DaemonMessage::TeamTemplateDrafted`] with the same `id`.
    DraftTeamTemplate {
        id: String,
        description: String,
    },
    /// Chapter Genesis — ask the daemon to **draft** the declared P13
    /// Profile from the operator's short onboarding answers (what they
    /// want the assistant to be, the role it plays, how it should talk,
    /// what it must never do) using the configured model. Read-only — it
    /// drafts nothing; the operator edits the returned fields and
    /// persists them via the `SetProfile` query. LLM-assisted, so it can
    /// fail (no model / model error) — the UI falls back to manual entry.
    ///
    /// Reply: [`DaemonMessage::ProfileDrafted`] with the same `id`.
    DraftProfile {
        id: String,
        /// "What do you want this assistant to be for you?"
        intent: String,
        /// The role it should play (collaborator / coach / assistant / …).
        role: String,
        /// How it should talk — tone & warmth.
        tone: String,
        /// The hard lines — what it must never do.
        never_do: String,
    },
    /// Phase 65 — operator-driven Persona chain import (Phase 60
    /// identity-deferral closer). Replays a parsed export bundle
    /// onto the local chain. Without `force` the daemon refuses if
    /// the local chain is non-empty. With `force` the daemon wipes
    /// the chain before replaying. Each delta is re-signed against
    /// the local HMAC key during replay (Phase 60 Q1(a)).
    ///
    /// Daemon-side flow (best-effort, no atomic-tx wrapping per
    /// Phase 65 Q1(a)): re-validate → conflict check → optional
    /// wipe → per-delta append → recompute shared runtime state.
    ///
    /// Reply: [`DaemonMessage::PersonaImportResolved`] with the
    /// same `id`.
    ImportPersonaChain {
        id: String,
        /// Deltas to replay in order. Each is appended via the
        /// existing `PersistentPersonaLog::append` path so the new
        /// chain's MACs bind to the target host's key.
        deltas: Vec<crate::DeltaExport>,
        /// Expected effective state after replay; the daemon
        /// echoes this back in the response for the CLI to verify.
        /// Already validated against the deltas at parse time by
        /// the CLI, but carried to the daemon for completeness.
        ///
        /// Boxed at Phase 118 — the two new operator-staged
        /// list fields on `EffectivePersona` (`profile_hints`,
        /// `role_drafts`) pushed the struct past the
        /// `clippy::large_enum_variant` threshold for this
        /// variant. Boxing keeps the rest of the
        /// `FrontendMessage` enum compact; the indirection is
        /// invisible to the daemon-side handler.
        effective_at_export: Box<crate::EffectivePersona>,
        /// If `false` and the local chain is non-empty, refuse.
        /// If `true`, wipe and replace.
        force: bool,
    },
    /// Phase 70 — operator-initiated resolution of a pending
    /// Persona proposal (P14 self-learning closure). Per Q3(a)
    /// at Phase 70 sign-off the operator can approve as-is,
    /// approve-with-edit (the daemon applies the edited op
    /// instead of the original), or reject with an optional
    /// reason.
    ///
    /// Daemon-side flow on `Approve` / `ApproveWithEdit`:
    /// validate the applied op → append a `PersonaDelta` to
    /// the persona chain → append an `Approved` entry to the
    /// proposal chain referencing the delta's seq → recompute
    /// shared runtime state. On `Reject`: append a `Rejected`
    /// entry only.
    ///
    /// Reply: [`DaemonMessage::PersonaProposalResolved`] with
    /// the same `id`.
    ResolvePersonaProposal {
        id: String,
        proposal_id: String,
        resolution: PersonaProposalResolution,
    },
    /// Phase 74 — operator-initiated memory topic eviction.
    /// Deletes every entry under `topic`; replies with
    /// [`DaemonMessage::MemoryEvictResolved`] carrying the
    /// number of entries deleted on success.
    EvictMemoryTopic {
        id: String,
        topic: String,
    },
    /// Chapter Concord — operator resolves a detected contradiction by
    /// choosing which fact is true: the *other* entry (`archive_seq`
    /// under `topic`) is deleted from active memory. Replies with
    /// [`DaemonMessage::MemoryConflictResolved`] carrying whether an
    /// entry was removed.
    ResolveMemoryConflict {
        id: String,
        topic: String,
        archive_seq: u64,
    },
    /// Chapter Concord — operator dismisses a detected contradiction as a
    /// false positive ("keep both"): `conflict_id` is recorded so future
    /// detection passes suppress that pair (nothing is deleted). Replies
    /// with [`DaemonMessage::MemoryConflictDismissed`].
    DismissMemoryConflict {
        id: String,
        conflict_id: String,
    },
    /// Chapter Accord — operator resolves a detected Persona contradiction by
    /// removing the losing facet: a `RemoveList` persona delta for
    /// `(category, value)` is appended to the signed chain (operator-
    /// authoritative, revertible). `profile_constraint` is immutable and
    /// rejected. Replies with [`DaemonMessage::SoulConflictResolved`].
    ResolveSoulConflict {
        id: String,
        category: String,
        value: String,
    },
    /// Chapter Accord — operator dismisses a detected Soul contradiction as a
    /// false positive ("keep both"): `conflict_id` is recorded so future
    /// detection passes suppress that pair (nothing is removed). Replies with
    /// [`DaemonMessage::SoulConflictDismissed`].
    DismissSoulConflict {
        id: String,
        conflict_id: String,
    },
    /// Chapter Repertoire — operator forgets a learned skill by name from
    /// the Studio Skills screen. Appends a `RemoveList` persona delta
    /// (operator-authoritative); responds with [`DaemonMessage::SkillForgotten`].
    ForgetSkill {
        id: String,
        name: String,
    },
    /// Chapter Chime — operator creates a schedule from the Studio.
    /// The daemon validates the cron expression, rejects id
    /// collisions, stamps `Operator` provenance, and writes the
    /// record; the running scheduler arms it within one tick
    /// (≤60 s) — no restart. Acked by
    /// [`DaemonEnvelope::ScheduleMutated`].
    CreateSchedule {
        id: String,
        name: String,
        cron: String,
        prompt: String,
        enabled: bool,
    },
    /// Chapter Chime — operator updates a schedule (enable/disable
    /// toggle, cron, or prompt; `None` fields stay unchanged).
    UpdateSchedule {
        id: String,
        schedule_id: String,
        #[serde(default)]
        enabled: Option<bool>,
        #[serde(default)]
        cron: Option<String>,
        #[serde(default)]
        prompt: Option<String>,
    },
    /// Chapter Chime — operator deletes a schedule (the Studio
    /// confirms first, like the Documents delete).
    DeleteSchedule {
        id: String,
        schedule_id: String,
    },
    /// Phase 119 — operator's act-on-approval gesture for a
    /// Phase 118 `ProfileHint` proposal. Carries the values
    /// the CLI already wrote to `aivyx-pa.toml` via the Task 3
    /// atomic primitive; the daemon's job is to record the
    /// `AuditEvent::ProfileHintApplied` entry so forensic
    /// walks see the apply alongside the upstream
    /// `SkillAutoProposal` + `PersonaProposalResolved`.
    ///
    /// Reply: [`DaemonMessage::ProfileHintApplyAcked`].
    ApplyProfileHint {
        id: String,
        /// Source proposal id from the operator-approved
        /// `ProfileHint` chain entry.
        proposal_id: String,
        /// The declared Profile-config field the apply
        /// mutated (matches `ProfileField::label()`).
        field: String,
        /// The value written to aivyx-pa.toml — new scalar for
        /// scalar fields, appended entry for list fields.
        applied_value: String,
    },
    /// Phase 119 — operator's act-on-approval gesture for a
    /// Phase 118 `RoleDefinitionSuggestion` proposal.
    /// Mirrors `ApplyProfileHint` for the second category.
    ///
    /// Reply: [`DaemonMessage::RoleDraftImportAcked`].
    ImportRoleDraft {
        id: String,
        /// Source proposal id from the operator-approved
        /// `RoleDefinitionSuggestion` chain entry.
        proposal_id: String,
        /// The kebab-case role name written.
        role_name: String,
        /// The parent role for inheritance (or `None` for
        /// top-level).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<String>,
    },
}

/// Phase 70 — operator resolution variants for
/// [`FrontendMessage::ResolvePersonaProposal`]. Tagged so
/// future variants (e.g. `Defer`, `RejectWithSuggestion`) can be
/// added without breaking older daemons / frontends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum PersonaProposalResolution {
    /// Approve verbatim — apply the agent's `proposed_op`
    /// unchanged.
    Approve,
    /// Approve with operator edits. The daemon validates and
    /// applies `edited_op` instead of the original
    /// `proposed_op`. Both are preserved in the proposal chain
    /// for audit.
    ApproveWithEdit {
        edited_op: crate::ProposedPersonaDelta,
    },
    /// Reject the proposal. `reason` is optional and carried in
    /// the audit trail.
    Reject {
        reason: Option<String>,
    },
}

// ---------------------------------------------------------------------------
// Daemon → Frontend (turn-loop traffic)
// ---------------------------------------------------------------------------

// `QueryResponse`'s `LearningInsights` payload legitimately
// accretes one read-only surface field per learning phase
// (79/80/81/82/83…); boxing every protocol field for a
// non-hot-path control message would harm readability for a
// marginal stack-size win that the next phase reintroduces.
// The large variant *is* the common case here.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DaemonMessage {
    SessionStarted {
        session_id: String,
    },
    StreamEvent {
        session_id: String,
        event: StreamEventPayload,
    },
    TurnComplete {
        session_id: String,
        outcome: String,
    },
    Error {
        code: String,
        message: String,
    },
    MissionCreated {
        mission_id: String,
    },
    MissionStateChanged {
        mission_id: String,
        state: String,
    },
    GateResolved {
        mission_id: String,
        gate_id: String,
        approved: bool,
    },
    /// Piece C — response to [`FrontendMessage::RunTeamMissionChannel`]
    /// on success. The new mission's id; the drive runs in the
    /// background (poll via the existing `QueryPayload::
    /// TeamMissionStatus`, same as every other team-mission start
    /// path).
    TeamMissionChannelStarted {
        mission_id: String,
    },
    /// Protocol version accepted (Phase 41 Task 5).
    ProtocolAccepted {
        version: String,
    },
    /// Protocol version rejected — client should disconnect or retry
    /// with a supported version (Phase 41 Task 5).
    ProtocolRejected {
        supported: Vec<String>,
    },
    /// Phase 47 — response to a [`FrontendMessage::Query`]. The `id`
    /// echoes the query's correlation id so the frontend can match
    /// async responses without bookkeeping.
    QueryResponse {
        id: String,
        payload: QueryResponsePayload,
    },
    /// Phase 60 — response to [`FrontendMessage::RevertPersonaDelta`].
    /// `ok = true` on a successful append + shared-state recompute;
    /// `ok = false` with `error` populated on failure (unknown
    /// target_delta_id, storage error, lock poisoning).
    PersonaRevertResolved {
        id: String,
        ok: bool,
        /// Sequence number of the appended revert delta on success;
        /// `None` on failure.
        seq: Option<u64>,
        error: Option<String>,
    },
    /// Chapter X — response to [`FrontendMessage::SeedPersona`]. `ok = true`
    /// with `appended` (the number of seed deltas planted) on success;
    /// `ok = false` with `error` when the chain is non-empty (already seeded /
    /// grown), the seed is empty, or a storage error occurred.
    PersonaSeedResolved {
        id: String,
        ok: bool,
        appended: u64,
        error: Option<String>,
    },
    /// Chapter Tutor — response to [`FrontendMessage::AuthorSkill`]. `ok = true`
    /// with `seq` (the chain seq of the appended delta) on success; `ok = false`
    /// with `error` on a validation failure, a `Teach` name collision, an
    /// `Update`/`Forget` of an unknown skill, or a storage error.
    SkillAuthored {
        id: String,
        ok: bool,
        seq: Option<u64>,
        error: Option<String>,
    },
    /// Chapter X — response to [`FrontendMessage::DraftPersonaSeed`]. `draft` is
    /// `Some` with the LLM-drafted seed (the operator edits + confirms); `None`
    /// with `error` when no model is configured or the draft failed.
    PersonaSeedDrafted {
        id: String,
        draft: Option<PersonaSeedWire>,
        error: Option<String>,
    },
    /// Chapter Nonagon Templates — response to
    /// [`FrontendMessage::DraftTeamTemplate`]. `draft` is `Some` with the
    /// LLM-drafted, already-clamped-and-validated
    /// [`aivyx_team_types::TeamConfig`] (exactly 9 members: a lead + 8
    /// specialists, each specialist's capabilities assigned in code from a
    /// fixed archetype — never LLM-authored scope strings); `None` with
    /// `error` when no model is configured or the draft failed.
    TeamTemplateDrafted {
        id: String,
        draft: Option<aivyx_team_types::TeamConfig>,
        error: Option<String>,
    },
    /// Chapter Genesis — response to [`FrontendMessage::DraftProfile`].
    /// `draft` is `Some` with the LLM-drafted Profile (which the operator
    /// edits, then persists via `SetProfile`); `None` with `error` when no
    /// model is configured or the draft failed.
    ProfileDrafted {
        id: String,
        draft: Option<ProfileDraftWire>,
        error: Option<String>,
    },
    /// Phase 65 — response to [`FrontendMessage::ImportPersonaChain`].
    /// On success carries `deltas_imported` (count from the
    /// request, useful for the CLI's tally output) and
    /// `final_chain_seq` (the last seq in the new chain).
    /// On failure carries `error` describing what went wrong
    /// (conflict without force, validation failure, append
    /// error mid-stream).
    PersonaImportResolved {
        id: String,
        ok: bool,
        /// On success: `{deltas_imported, final_chain_seq}` per
        /// Phase 65 Q4(a). `None` on failure.
        success: Option<PersonaImportSuccess>,
        error: Option<String>,
    },
    /// Phase 70 — response to
    /// [`FrontendMessage::ResolvePersonaProposal`]. `ok = true`
    /// on success; `success` carries `{ proposal_status,
    /// applied_seq }` on Approve / ApproveWithEdit (where
    /// `applied_seq` is the persona-chain seq of the appended
    /// delta) or `{ proposal_status: "Rejected", applied_seq:
    /// None }` on Reject. `error` is populated on failure
    /// (unknown proposal id, validation failure of edited op,
    /// invalid status transition, storage error).
    PersonaProposalResolved {
        id: String,
        ok: bool,
        success: Option<PersonaProposalResolveSuccess>,
        error: Option<String>,
    },
    /// Phase 74 — response to
    /// [`FrontendMessage::EvictMemoryTopic`]. `ok = true` with
    /// `deleted` set on success; `ok = false` with `error`
    /// populated when the substrate rejects (empty topic, etc.).
    MemoryEvictResolved {
        id: String,
        ok: bool,
        deleted: Option<u64>,
        error: Option<String>,
    },
    /// Chapter Concord — ack for [`FrontendMessage::ResolveMemoryConflict`].
    /// `ok = true` with `removed` = whether the archived entry existed
    /// (idempotent no-op → `false`); `ok = false` + `error` on substrate
    /// rejection (empty topic, storage error).
    MemoryConflictResolved {
        id: String,
        ok: bool,
        removed: bool,
        error: Option<String>,
    },
    /// Chapter Accord — ack for [`FrontendMessage::ResolveSoulConflict`].
    /// `ok = true` with the new chain `seq` when the losing facet was removed;
    /// `ok = false` + `error` when the facet is immutable, unknown, or the
    /// chain append fails.
    SoulConflictResolved {
        id: String,
        ok: bool,
        seq: Option<u64>,
        error: Option<String>,
    },
    /// Chapter Accord — ack for [`FrontendMessage::DismissSoulConflict`].
    /// `ok = true` on success; `ok = false` + `error` on storage failure.
    SoulConflictDismissed {
        id: String,
        ok: bool,
        error: Option<String>,
    },
    /// Chapter Concord — ack for [`FrontendMessage::DismissMemoryConflict`].
    /// `ok = true` on success; `ok = false` + `error` on storage failure.
    MemoryConflictDismissed {
        id: String,
        ok: bool,
        error: Option<String>,
    },
    /// Chapter Repertoire — ack for [`FrontendMessage::ForgetSkill`].
    /// `ok = true` + `removed` whether a skill by that name existed; `name`
    /// echoes the request so the UI can drop the row locally.
    SkillForgotten {
        id: String,
        ok: bool,
        removed: bool,
        name: String,
        error: Option<String>,
    },
    /// Chapter Chime — ack for the schedule mutations
    /// ([`FrontendMessage::CreateSchedule`] / `UpdateSchedule` /
    /// `DeleteSchedule`). `ok = false` carries the reason
    /// (invalid cron, id collision, unknown id).
    ScheduleMutated {
        id: String,
        ok: bool,
        schedule_id: String,
        error: Option<String>,
    },

    /// Phase 119 — ack for [`FrontendMessage::ApplyProfileHint`].
    /// `ok = true` means the daemon recorded the
    /// `AuditEvent::ProfileHintApplied` entry; `ok = false`
    /// with `error` populated means the audit-log append
    /// failed (the operator's `aivyx-pa.toml` mutation already
    /// landed CLI-side before the IPC fired).
    ProfileHintApplyAcked {
        id: String,
        ok: bool,
        error: Option<String>,
    },
    /// Phase 119 — ack for [`FrontendMessage::ImportRoleDraft`].
    /// Same shape as `ProfileHintApplyAcked`.
    RoleDraftImportAcked {
        id: String,
        ok: bool,
        error: Option<String>,
    },
    /// Phase 69 — broadcast-style Web UI desktop notification.
    /// Fired by `NotifyWebUiBackend` and
    /// relayed onto every connected Web UI WebSocket. Distinct
    /// from `StreamEvent` (which is per-session); these are
    /// per-daemon notifications without a session correlation.
    DesktopNotification {
        title: String,
        body: String,
    },
    /// Chapter Mission Control — broadcast-style team-mission live update.
    /// Fired by the daemon's `RegistryObserver` whenever a step starts,
    /// finishes, or the mission's phase transitions, and relayed onto
    /// every connected Web UI WebSocket. Same broadcast shape as
    /// `DesktopNotification` (no session correlation) — carries the
    /// already-projected view rather than the raw record, since only the
    /// daemon has the live running-step signal in scope.
    TeamMissionUpdated {
        view: crate::TeamMissionView,
    },
}

/// Phase 70 — success payload for
/// [`DaemonMessage::PersonaProposalResolved`]. `proposal_status`
/// is the new stable label after resolution (`"Approved" |
/// "Rejected"`); `applied_seq` is the persona-chain seq of the
/// appended PersonaDelta on Approve / ApproveWithEdit, or `None`
/// on Reject.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonaProposalResolveSuccess {
    pub proposal_status: String,
    pub applied_seq: Option<u64>,
}

/// Phase 65 — success payload for [`DaemonMessage::PersonaImportResolved`].
/// Mirrors the operator-feedback shape requested at Q4(a) sign-off.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonaImportSuccess {
    /// How many deltas the daemon appended. Equals the length
    /// of the request's `deltas` array on a full import.
    pub deltas_imported: u64,
    /// The seq of the final appended delta. After a successful
    /// import, `final_chain_seq + 1` is the chain's current
    /// length (since seqs are zero-indexed).
    pub final_chain_seq: u64,
}

// ---------------------------------------------------------------------------
// Daemon → Frontend (lifecycle, separate from DaemonMessage per Q4)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DaemonLifecycleEvent {
    DaemonReady { version: String },
    ShuttingDown { reason: String },
    RecoveryNotice {
        lost_sessions: Vec<String>,
        lost_turns: Vec<String>,
        stale_since: u64,
    },
}

// ---------------------------------------------------------------------------
// StreamEventPayload — owned, serializable mirror of core::StreamEvent<'a>
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum StreamEventPayload {
    Text {
        text: String,
    },
    Status {
        status: String,
    },
    ToolCallStarted {
        tool_id: String,
        tool_name: String,
        input: serde_json::Value,
    },
    ToolCallFinished {
        tool_id: String,
        tool_name: String,
        outcome_summary: String,
    },
    ToolOutput {
        tool_id: String,
        tool_name: String,
        chunk: String,
    },
    ApprovalGate {
        mission_id: String,
        gate_id: String,
        reason: String,
        scope: Option<String>,
    },
}

impl StreamEventPayload {
    /// Render this payload to a human-readable CLI string, matching the
    /// format that `render_stream_event(RenderMode::Human, ..)` produces
    /// for the in-process path. This lets a daemon-mode frontend pipe IPC
    /// events through the same rendering code path without converting back
    /// to the borrowed `StreamEvent<'a>` type.
    pub fn render_for_cli(&self) -> String {
        match self {
            StreamEventPayload::Text { text } => text.clone(),
            StreamEventPayload::Status { status } => format!("  ⋯ {status}\n"),
            StreamEventPayload::ToolCallStarted {
                tool_name, input, ..
            } => {
                let input_oneline = serde_json::to_string(input).unwrap_or_default();
                format!("  → {tool_name} {input_oneline}\n")
            }
            StreamEventPayload::ToolCallFinished {
                tool_name,
                outcome_summary,
                ..
            } => format!("  ← {tool_name} {outcome_summary}\n"),
            StreamEventPayload::ToolOutput { chunk, .. } => chunk.clone(),
            StreamEventPayload::ApprovalGate {
                mission_id,
                gate_id,
                reason,
                scope,
            } => {
                let scope_str = scope
                    .as_deref()
                    .map(|s| format!(" (scope: {s})"))
                    .unwrap_or_default();
                format!(
                    "\n  ⚑ APPROVAL GATE [{mission_id}/{gate_id}]: \
                     {reason}{scope_str}\n"
                )
            }
        }
    }
}

/// Concatenate just the `Text` chunks from a turn's events, in order —
/// the text a surface has displayed (or would display) as the
/// assistant's answer. Ignores `Status`/`ToolCall*`/`ApprovalGate`
/// events. Pure.
pub fn concat_text_events(events: &[StreamEventPayload]) -> String {
    let mut out = String::new();
    for event in events {
        if let StreamEventPayload::Text { text } = event {
            out.push_str(text);
        }
    }
    out
}

/// Compare what a surface already displayed/reconstructed for a turn
/// (`displayed`, from [`concat_text_events`] or an equivalent
/// live-accumulated buffer) against the turn's own authoritative
/// `outcome` string (as sent on `TurnComplete`/`Msg::TurnFinished` —
/// `"completed: {final_message}"` for a normal completion, a fixed
/// reason string for every other `TurnOutcome` variant — see
/// `format_outcome` in `aivyx-channel`'s `daemon_server.rs`). Returns
/// `Some(line)` to show when they diverge in a way the operator should
/// see; `None` when nothing needs correcting. The turn loop's own
/// post-processing (a final-message floor, Candor's claim-check,
/// an identifier-fidelity check) only ever touches `outcome`'s
/// `final_message` — never the raw streamed text — so this is the seam
/// a surface uses to catch up. Pure.
pub fn turn_outcome_correction(displayed: &str, outcome: &str) -> Option<String> {
    let displayed = displayed.trim();
    match outcome.strip_prefix("completed: ") {
        Some(final_message) => {
            let final_message = final_message.trim();
            if final_message.is_empty() {
                return if displayed.is_empty() {
                    Some("(no reply)".to_string())
                } else {
                    // Something streamed even though the final step's
                    // own text ended up empty (e.g. a tool-call-only
                    // final step after real narration) — nothing
                    // authoritative to add.
                    None
                };
            }
            if displayed.is_empty() {
                return Some(final_message.to_string());
            }
            // Multi-step turns stream EVERY step's text (including
            // narration before a tool call — LlmPlanner::one_step
            // relays every TextChunk as it arrives), but final_message
            // is only ever the LAST step's text. A healthy multi-step
            // turn's displayed text therefore legitimately contains
            // MORE than final_message; the real answer still matches
            // its tail, so there is nothing to correct.
            if displayed.ends_with(final_message) {
                return None;
            }
            // The turn loop's post-processing (Candor's claim-check,
            // the identifier-fidelity check — see
            // `append_turn_note` in `aivyx-core`'s `agent.rs`, the
            // sole owner of this "\n\n⚠ {note}" format; aivyx-ipc
            // can't depend on aivyx-core to share a constant, so
            // this string is duplicated by convention, not by
            // reference — keep the two in sync by hand if either
            // changes) appends "\n\n⚠ {note}" annotations to
            // final_message. When the pre-annotation portion matches
            // what already displayed, show ONLY the new annotation(s)
            // — not the whole final_message, which would duplicate
            // the already-correct answer behind a misleading
            // "corrected" framing. Re-review fix: use the FIRST
            // marker whose preceding text actually matches displayed
            // (not just the first marker in the string) — the
            // model's own organic text can legitimately contain a
            // bare "⚠ " paragraph before a real annotation is ever
            // appended, and the first `find` alone would land on that
            // organic marker instead of the real annotation boundary.
            for (idx, _) in final_message.match_indices("\n\n⚠ ") {
                let answer_part = final_message[..idx].trim_end();
                if displayed.ends_with(answer_part) {
                    return Some(final_message[idx..].trim_start().to_string());
                }
            }
            Some(format!("⚠ corrected: {final_message}"))
        }
        None => Some(outcome.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Framing: encode / decode
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum FrameError {
    PayloadTooLarge(u32),
    IncompleteBuf,
    Utf8(String),
    Json(String),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::PayloadTooLarge(n) => {
                write!(f, "payload size {n} exceeds max {MAX_PAYLOAD_SIZE}")
            }
            FrameError::IncompleteBuf => write!(f, "buffer too short for a complete frame"),
            FrameError::Utf8(e) => write!(f, "payload is not valid UTF-8: {e}"),
            FrameError::Json(e) => write!(f, "JSON parse error: {e}"),
        }
    }
}

impl std::error::Error for FrameError {}

/// Encode a serializable message into a length-prefixed frame.
pub fn encode_frame<T: Serialize>(msg: &T) -> Result<Vec<u8>, FrameError> {
    let json = serde_json::to_vec(msg).map_err(|e| FrameError::Json(e.to_string()))?;
    let len: u32 = json
        .len()
        .try_into()
        .map_err(|_| FrameError::PayloadTooLarge(u32::MAX))?;
    if len > MAX_PAYLOAD_SIZE {
        return Err(FrameError::PayloadTooLarge(len));
    }
    let mut buf = Vec::with_capacity(FRAME_HEADER_LEN + json.len());
    buf.extend_from_slice(&len.to_be_bytes());
    buf.extend_from_slice(&json);
    Ok(buf)
}

/// Try to decode one frame from the front of `buf`. On success returns
/// the deserialized message and the number of bytes consumed (header +
/// payload). Returns `Err(IncompleteBuf)` if `buf` does not yet contain
/// a full frame — the caller should read more bytes and retry.
pub fn decode_frame<T: for<'de> Deserialize<'de>>(buf: &[u8]) -> Result<(T, usize), FrameError> {
    if buf.len() < FRAME_HEADER_LEN {
        return Err(FrameError::IncompleteBuf);
    }
    let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]);
    if len > MAX_PAYLOAD_SIZE {
        return Err(FrameError::PayloadTooLarge(len));
    }
    let total = FRAME_HEADER_LEN + len as usize;
    if buf.len() < total {
        return Err(FrameError::IncompleteBuf);
    }
    let payload = &buf[FRAME_HEADER_LEN..total];
    let text = std::str::from_utf8(payload).map_err(|e| FrameError::Utf8(e.to_string()))?;
    let msg: T = serde_json::from_str(text).map_err(|e| FrameError::Json(e.to_string()))?;
    Ok((msg, total))
}

/// Convenience: decode a frame where the message type is one of the
/// three IPC envelopes. Wraps `decode_frame` with the union type.
///
/// The daemon's receive loop calls `decode_frame::<FrontendMessage>`.
/// The frontend's receive loop needs to demux `DaemonMessage` vs.
/// `DaemonLifecycleEvent` — this enum carries both.
// See `DaemonMessage` — same accreting-`LearningInsights`
// rationale; this enum mirrors its variants.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DaemonEnvelope {
    // DaemonMessage variants (flattened for serde tag dispatch)
    SessionStarted {
        session_id: String,
    },
    /// POLISH_WAVES.md sub-project 6, item B. Sent once per WebSocket
    /// connection by the web-UI bridge (`aivyx-channel/src/web_ui.rs`)
    /// immediately after `SessionStarted` — never by the daemon core
    /// itself, so there is no `DaemonMessage` counterpart. `boot_id` is a
    /// random id minted once when the bridge's `run_web_ui_server` starts,
    /// stable for that process's lifetime and different after any
    /// restart. Studio compares it across reconnects to detect "the
    /// daemon I'm now talking to isn't the one I started against" — a
    /// direct proxy for "a redeployed dist bundle restarted the daemon" —
    /// and hints that a reload will pick up the newer bundle.
    ServerInfo {
        boot_id: String,
    },
    StreamEvent {
        session_id: String,
        event: StreamEventPayload,
    },
    TurnComplete {
        session_id: String,
        outcome: String,
    },
    Error {
        code: String,
        message: String,
    },
    // Mission variants (Phase 21)
    MissionCreated {
        mission_id: String,
    },
    MissionStateChanged {
        mission_id: String,
        state: String,
    },
    GateResolved {
        mission_id: String,
        gate_id: String,
        approved: bool,
    },
    /// Piece C — mirrors `DaemonMessage::TeamMissionChannelStarted`.
    TeamMissionChannelStarted {
        mission_id: String,
    },
    // DaemonLifecycleEvent variants
    DaemonReady {
        version: String,
    },
    ShuttingDown {
        reason: String,
    },
    RecoveryNotice {
        lost_sessions: Vec<String>,
        lost_turns: Vec<String>,
        stale_since: u64,
    },
    // Protocol negotiation variants (Phase 41 Task 5)
    ProtocolAccepted {
        version: String,
    },
    ProtocolRejected {
        supported: Vec<String>,
    },
    // Phase 47 — inspection query response.
    QueryResponse {
        id: String,
        payload: QueryResponsePayload,
    },
    // Phase 60 — Persona revert resolution.
    PersonaRevertResolved {
        id: String,
        ok: bool,
        seq: Option<u64>,
        error: Option<String>,
    },
    // Chapter X — live persona seed resolution.
    PersonaSeedResolved {
        id: String,
        ok: bool,
        appended: u64,
        error: Option<String>,
    },
    // Chapter Tutor — operator-authored skill result.
    SkillAuthored {
        id: String,
        ok: bool,
        seq: Option<u64>,
        error: Option<String>,
    },
    // Chapter X — LLM-drafted persona seed.
    PersonaSeedDrafted {
        id: String,
        draft: Option<PersonaSeedWire>,
        error: Option<String>,
    },
    /// Chapter Nonagon Templates — response to
    /// [`FrontendMessage::DraftTeamTemplate`]. `draft` is `Some` with the
    /// LLM-drafted, already-clamped-and-validated
    /// [`aivyx_team_types::TeamConfig`] (exactly 9 members: a lead + 8
    /// specialists, each specialist's capabilities assigned in code from a
    /// fixed archetype — never LLM-authored scope strings); `None` with
    /// `error` when no model is configured or the draft failed.
    TeamTemplateDrafted {
        id: String,
        draft: Option<aivyx_team_types::TeamConfig>,
        error: Option<String>,
    },
    // Chapter Genesis — LLM-drafted declared Profile.
    ProfileDrafted {
        id: String,
        draft: Option<ProfileDraftWire>,
        error: Option<String>,
    },
    // Phase 65 — Persona import resolution.
    PersonaImportResolved {
        id: String,
        ok: bool,
        success: Option<PersonaImportSuccess>,
        error: Option<String>,
    },
    // Phase 69 — Web UI desktop notification (broadcast).
    DesktopNotification {
        title: String,
        body: String,
    },
    // Chapter Mission Control — team-mission live update (broadcast).
    TeamMissionUpdated {
        view: crate::TeamMissionView,
    },
    // Phase 70 — Persona proposal resolution result.
    PersonaProposalResolved {
        id: String,
        ok: bool,
        success: Option<PersonaProposalResolveSuccess>,
        error: Option<String>,
    },
    // Phase 74 — memory eviction resolution result.
    MemoryEvictResolved {
        id: String,
        ok: bool,
        deleted: Option<u64>,
        error: Option<String>,
    },
    /// Chapter Concord — memory-conflict resolution result.
    MemoryConflictResolved {
        id: String,
        ok: bool,
        removed: bool,
        error: Option<String>,
    },
    /// Chapter Accord — Soul-conflict resolution result.
    SoulConflictResolved {
        id: String,
        ok: bool,
        seq: Option<u64>,
        error: Option<String>,
    },
    /// Chapter Accord — Soul-conflict dismissal result.
    SoulConflictDismissed {
        id: String,
        ok: bool,
        error: Option<String>,
    },
    /// Chapter Concord — memory-conflict dismissal result.
    MemoryConflictDismissed {
        id: String,
        ok: bool,
        error: Option<String>,
    },
    /// Chapter Repertoire — ack for `ForgetSkill`.
    SkillForgotten {
        id: String,
        ok: bool,
        removed: bool,
        name: String,
        error: Option<String>,
    },
    /// Chapter Chime — ack for the schedule mutations
    /// ([`FrontendMessage::CreateSchedule`] / `UpdateSchedule` /
    /// `DeleteSchedule`). `ok = false` carries the reason
    /// (invalid cron, id collision, unknown id).
    ScheduleMutated {
        id: String,
        ok: bool,
        schedule_id: String,
        error: Option<String>,
    },

    // Phase 119 — ProfileHint apply ack.
    ProfileHintApplyAcked {
        id: String,
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
    // Phase 119 — RoleDraft import ack.
    RoleDraftImportAcked {
        id: String,
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---- FrontendMessage round-trip ----

    #[test]
    fn frontend_message_round_trips() {
        let cases = vec![
            FrontendMessage::StartSession {
                role: Some("coder".into()),
                frontend_type: Some(FrontendType::Local),
            },
            FrontendMessage::StartSession { role: None, frontend_type: None },
            FrontendMessage::StartSession {
                role: None,
                frontend_type: Some(FrontendType::Telegram),
            },
            FrontendMessage::StartSession {
                role: None,
                frontend_type: Some(FrontendType::Web),
            },
            FrontendMessage::SubmitInput {
                session_id: "abc-123".into(),
                text: "hello world".into(),
                mission_id: None,
                attachments: vec![],
                headless: false,
            },
            FrontendMessage::CancelTurn {
                session_id: "abc-123".into(),
            },
            FrontendMessage::ResolveGate {
                mission_id: "m-001".into(),
                gate_id: "g-001".into(),
                approved: true,
            },
            FrontendMessage::ResolveGate {
                mission_id: "m-001".into(),
                gate_id: "g-002".into(),
                approved: false,
            },
            FrontendMessage::Disconnect,
            FrontendMessage::Shutdown,
            FrontendMessage::ProtocolNegotiation {
                version: "0.1".into(),
            },
            // Phase 47 — Query variants.
            FrontendMessage::Query {
                id: "q-001".into(),
                payload: QueryPayload::ListSessions,
            },
            FrontendMessage::Query {
                id: "q-002".into(),
                payload: QueryPayload::ListMissions,
            },
            FrontendMessage::Query {
                id: "q-003".into(),
                payload: QueryPayload::GetMission {
                    mission_id: "m-abc".into(),
                },
            },
            FrontendMessage::Query {
                id: "q-004".into(),
                payload: QueryPayload::ListAuditEntries {
                    from_seq: 0,
                    limit: 100,
                },
            },
            FrontendMessage::Query {
                id: "q-005".into(),
                payload: QueryPayload::VerifyAuditChain,
            },
            // Phase 58 — Profile inspection query.
            FrontendMessage::Query {
                id: "q-006".into(),
                payload: QueryPayload::GetProfile { from_disk: false },
            },
            // Phase 60 — Persona inspection queries.
            FrontendMessage::Query {
                id: "q-007".into(),
                payload: QueryPayload::GetEffectivePersona,
            },
            FrontendMessage::Query {
                id: "q-008".into(),
                payload: QueryPayload::ListPersonaDeltas {
                    from_seq: 0,
                    limit: 50,
                },
            },
            // Phase 60 — operator-initiated Persona revert.
            FrontendMessage::RevertPersonaDelta {
                id: "rv-1".into(),
                target_delta_id: "pd-abc123".into(),
            },
            // Phase 70 — Persona proposal queries.
            FrontendMessage::Query {
                id: "q-100".into(),
                payload: QueryPayload::ListPersonaProposals {
                    status_filter: "pending".into(),
                    limit: 50,
                },
            },
            FrontendMessage::Query {
                id: "q-101".into(),
                payload: QueryPayload::GetPersonaProposal {
                    proposal_id: "pp-001".into(),
                },
            },
            // Phase 73 — notification history queries.
            FrontendMessage::Query {
                id: "q-200".into(),
                payload: QueryPayload::ListNotificationHistory {
                    from_seq: 0,
                    limit: 100,
                    target_filter: None,
                },
            },
            FrontendMessage::Query {
                id: "q-201".into(),
                payload: QueryPayload::ListNotificationHistory {
                    from_seq: 50,
                    limit: 25,
                    target_filter: Some("phone".into()),
                },
            },
            // Phase 74 — memory inspection queries.
            FrontendMessage::Query {
                id: "q-300".into(),
                payload: QueryPayload::ListMemoryTopics,
            },
            FrontendMessage::Query {
                id: "q-301".into(),
                payload: QueryPayload::GetMemoryTopicEntries {
                    topic: "notes".into(),
                    limit: 16,
                },
            },
            FrontendMessage::Query {
                id: "q-302".into(),
                payload: QueryPayload::SearchMemory {
                    query: "foo".into(),
                    limit: 20,
                    semantic: true,
                },
            },
            FrontendMessage::Query {
                id: "q-303".into(),
                payload: QueryPayload::GetLearningInsights {
                    window_secs: Some(86_400),
                },
            },
            // Phase 74 — operator-initiated memory eviction.
            FrontendMessage::EvictMemoryTopic {
                id: "ev-1".into(),
                topic: "stale-notes".into(),
            },
            // Phase 70 — Persona proposal resolutions.
            FrontendMessage::ResolvePersonaProposal {
                id: "rs-1".into(),
                proposal_id: "pp-001".into(),
                resolution: PersonaProposalResolution::Approve,
            },
            FrontendMessage::ResolvePersonaProposal {
                id: "rs-2".into(),
                proposal_id: "pp-002".into(),
                resolution: PersonaProposalResolution::ApproveWithEdit {
                    edited_op: crate::ProposedPersonaDelta {
                        category: crate::PersonaDeltaCategory::BehavioralPreferences,
                        op: crate::PersonaDeltaOp::AppendList {
                            value: "operator-edited preference".into(),
                        },
                        reason: None,
                        supersedes_proposal_id: None,
                    },
                },
            },
            FrontendMessage::ResolvePersonaProposal {
                id: "rs-3".into(),
                proposal_id: "pp-003".into(),
                resolution: PersonaProposalResolution::Reject {
                    reason: Some("not safe".into()),
                },
            },
            FrontendMessage::ResolvePersonaProposal {
                id: "rs-4".into(),
                proposal_id: "pp-004".into(),
                resolution: PersonaProposalResolution::Reject { reason: None },
            },
        ];
        for msg in cases {
            let frame = encode_frame(&msg).expect("encode");
            let (decoded, consumed): (FrontendMessage, _) =
                decode_frame(&frame).expect("decode");
            assert_eq!(decoded, msg);
            assert_eq!(consumed, frame.len());
        }
    }

    // ---- DaemonMessage round-trip ----

    #[test]
    fn daemon_message_round_trips() {
        let cases = vec![
            DaemonMessage::SessionStarted {
                session_id: "s1".into(),
            },
            DaemonMessage::StreamEvent {
                session_id: "s1".into(),
                event: StreamEventPayload::Text {
                    text: "hello".into(),
                },
            },
            DaemonMessage::StreamEvent {
                session_id: "s1".into(),
                event: StreamEventPayload::ToolCallStarted {
                    tool_id: "t1".into(),
                    tool_name: "fs.read".into(),
                    input: serde_json::json!({"path": "/tmp/test"}),
                },
            },
            DaemonMessage::TurnComplete {
                session_id: "s1".into(),
                outcome: "completed".into(),
            },
            DaemonMessage::Error {
                code: "internal".into(),
                message: "something broke".into(),
            },
            DaemonMessage::MissionCreated {
                mission_id: "m-001".into(),
            },
            DaemonMessage::MissionStateChanged {
                mission_id: "m-001".into(),
                state: "Running".into(),
            },
            DaemonMessage::GateResolved {
                mission_id: "m-001".into(),
                gate_id: "g-001".into(),
                approved: true,
            },
            DaemonMessage::ProtocolAccepted {
                version: "0.1".into(),
            },
            DaemonMessage::ProtocolRejected {
                supported: vec!["0.1".into(), "0.2".into()],
            },
            // Phase 47 — QueryResponse variants.
            DaemonMessage::QueryResponse {
                id: "q-001".into(),
                payload: QueryResponsePayload::ListSessions {
                    sessions: vec![
                        SessionSummary {
                            session_id: "s-1".into(),
                            channel: WireChannelPlatform::Local,
                            trust_tier: aivyx_capability::TrustTier::Trusted,
                            created_at_ms: 0,
                            last_active_at_ms: 0,
                        },
                        SessionSummary {
                            session_id: "s-2".into(),
                            channel: WireChannelPlatform::Telegram,
                            trust_tier: aivyx_capability::TrustTier::SemiTrusted,
                            created_at_ms: 0,
                            last_active_at_ms: 0,
                        },
                    ],
                },
            },
            DaemonMessage::QueryResponse {
                id: "q-002".into(),
                payload: QueryResponsePayload::ListSessions { sessions: vec![] },
            },
            DaemonMessage::QueryResponse {
                id: "q-003".into(),
                payload: QueryResponsePayload::QueryError {
                    code: "internal".into(),
                    message: "store unavailable".into(),
                },
            },
            DaemonMessage::QueryResponse {
                id: "q-004".into(),
                payload: QueryResponsePayload::ListMissions {
                    missions: vec![MissionSummary {
                        mission_id: "m-1".into(),
                        role_name: "default".into(),
                        description: "test".into(),
                        state: "Running".into(),
                        has_pending_gate: false,
                        created_at: 1,
                        updated_at: 2,
                    }],
                },
            },
            DaemonMessage::QueryResponse {
                id: "q-005".into(),
                payload: QueryResponsePayload::GetMission { mission: None },
            },
            DaemonMessage::QueryResponse {
                id: "q-007".into(),
                payload: QueryResponsePayload::ListAuditEntries {
                    entries: vec![AuditEntrySummary {
                        seq: 0,
                        appended_at_unix_ms: 1_700_000_000_000,
                        event_type: "TurnStarted".into(),
                        event: serde_json::json!({"type": "TurnStarted"}),
                        mac_hex: "deadbeef".repeat(8),
                    }],
                    total_len: 1,
                },
            },
            DaemonMessage::QueryResponse {
                id: "q-008".into(),
                payload: QueryResponsePayload::VerifyAuditChain {
                    ok: true,
                    entries_verified: 42,
                    error: None,
                },
            },
            DaemonMessage::QueryResponse {
                id: "q-009".into(),
                payload: QueryResponsePayload::VerifyAuditChain {
                    ok: false,
                    entries_verified: 0,
                    error: Some("chain broken at seq 3".into()),
                },
            },
            // Phase 58 — Profile inspection response (default snapshot,
            // injection disabled).
            DaemonMessage::QueryResponse {
                id: "q-010".into(),
                payload: QueryResponsePayload::GetProfile {
                    profile: ProfileSummary {
                        assistant_name: "Aivyx PA".into(),
                        assistant_name_source: "default".into(),
                        operator_profile: None,
                        communication_style: None,
                        primary_use_cases: vec![],
                        behavioral_preferences: vec![],
                        behavioral_constraints: vec![],
                        injection_enabled: false,
                    },
                },
            },
            // Phase 58 — Profile inspection response with operator-
            // declared content (injection enabled).
            DaemonMessage::QueryResponse {
                id: "q-011".into(),
                payload: QueryResponsePayload::GetProfile {
                    profile: ProfileSummary {
                        assistant_name: "Codex".into(),
                        assistant_name_source: "toml".into(),
                        operator_profile: Some("Senior Rust engineer".into()),
                        communication_style: Some("terse".into()),
                        primary_use_cases: vec!["Rust systems".into()],
                        behavioral_preferences: vec!["prefer integration tests".into()],
                        behavioral_constraints: vec!["never auto-commit".into()],
                        injection_enabled: true,
                    },
                },
            },
            // Phase 60 — Persona inspection responses.
            DaemonMessage::QueryResponse {
                id: "q-012".into(),
                payload: QueryResponsePayload::GetEffectivePersona {
                    persona: EffectivePersonaSummary::default(),
                },
            },
            DaemonMessage::QueryResponse {
                id: "q-013".into(),
                payload: QueryResponsePayload::GetEffectivePersona {
                    persona: EffectivePersonaSummary {
                        behavioral_preferences: vec!["always cite sources".into()],
                        learned_context: vec!["operator uses Vim".into()],
                        is_non_empty: true,
                        ..Default::default()
                    },
                },
            },
            DaemonMessage::QueryResponse {
                id: "q-014".into(),
                payload: QueryResponsePayload::ListPersonaDeltas {
                    entries: vec![],
                    total_len: 0,
                },
            },
            DaemonMessage::QueryResponse {
                id: "q-015".into(),
                payload: QueryResponsePayload::ListPersonaDeltas {
                    entries: vec![PersonaDeltaSummary {
                        seq: 0,
                        delta_id: "pd-abc".into(),
                        proposed_at_unix_ms: 1_715_000_000_000,
                        approved_at_unix_ms: 1_715_000_060_000,
                        proposal_id: "rp-1".into(),
                        category: "BehavioralPreferences".into(),
                        op: serde_json::json!({
                            "kind": "AppendList",
                            "value": "prefer terse"
                        }),
                        mac_hex: "0".repeat(64),
                    }],
                    total_len: 1,
                },
            },
            // Phase 60 — revert resolution responses.
            DaemonMessage::PersonaRevertResolved {
                id: "rv-1".into(),
                ok: true,
                seq: Some(2),
                error: None,
            },
            DaemonMessage::PersonaRevertResolved {
                id: "rv-2".into(),
                ok: false,
                seq: None,
                error: Some("no persona delta found with id `pd-missing`".into()),
            },
            // Phase 70 — proposal resolution responses.
            DaemonMessage::PersonaProposalResolved {
                id: "rs-1".into(),
                ok: true,
                success: Some(PersonaProposalResolveSuccess {
                    proposal_status: "Approved".into(),
                    applied_seq: Some(42),
                }),
                error: None,
            },
            DaemonMessage::PersonaProposalResolved {
                id: "rs-2".into(),
                ok: true,
                success: Some(PersonaProposalResolveSuccess {
                    proposal_status: "Rejected".into(),
                    applied_seq: None,
                }),
                error: None,
            },
            DaemonMessage::PersonaProposalResolved {
                id: "rs-3".into(),
                ok: false,
                success: None,
                error: Some("unknown proposal id `pp-missing`".into()),
            },
            // Phase 74 — memory eviction resolution.
            DaemonMessage::MemoryEvictResolved {
                id: "ev-1".into(),
                ok: true,
                deleted: Some(7),
                error: None,
            },
            DaemonMessage::MemoryEvictResolved {
                id: "ev-2".into(),
                ok: false,
                deleted: None,
                error: Some("memory topic must be non-empty".into()),
            },
            // Phase 74 — memory query responses.
            DaemonMessage::QueryResponse {
                id: "q-300".into(),
                payload: QueryResponsePayload::ListMemoryTopics {
                    topics: vec!["notes".into(), "project/x".into()],
                },
            },
            DaemonMessage::QueryResponse {
                id: "q-301".into(),
                payload: QueryResponsePayload::GetMemoryTopicEntries {
                    entries: vec![MemoryEntrySummary {
                        topic: "notes".into(),
                        body: "remember the milk".into(),
                        seq: 3,
                        created_at_secs: 1_715_000_000,
                        last_read_at_secs: 1_715_000_500,
                    }],
                },
            },
            DaemonMessage::QueryResponse {
                id: "q-302".into(),
                payload: QueryResponsePayload::SearchMemory {
                    matches: vec![MemoryEntrySummary {
                        topic: "project/x".into(),
                        body: "the foo subsystem".into(),
                        seq: 9,
                        created_at_secs: 1_715_001_000,
                        last_read_at_secs: 0,
                    }],
                    fell_back_to_keyword: true,
                },
            },
            DaemonMessage::QueryResponse {
                id: "q-303".into(),
                payload: QueryResponsePayload::LearningInsights {
                    digest: crate::LearningDigest {
                        window_secs: 86_400,
                        recalls_total: 12,
                        recalls_scored: 9,
                        promoted: 4,
                        not_promoted: 2,
                        top_helpful: vec![("project/x".into(), 5.0)],
                        top_unhelpful: vec![("scratch".into(), -3.0)],
                        proposals_in_window: 1,
                        judgment_signal: None,
                    },
                    proposals: vec![
                        crate::ProposalProvenance {
                            proposal_id: "recall-fb:project/x".into(),
                            topic: "project/x".into(),
                            status: "pending".into(),
                            net_score: 5.0,
                            reason: Some("net +5".into()),
                            contributing: vec![
                                crate::ContributingTurn {
                                    ts_secs: 1_715_000_000,
                                    outcome_kind: Some(
                                        "completed".into(),
                                    ),
                                    signal: Some(1.0),
                                    seqs: vec![9],
                                },
                            ],
                        },
                    ],
                    persona_selection: Some(
                        crate::PersonaSelectionStat {
                            ts_secs: 1_715_002_000,
                            selected: 6,
                            total: 20,
                        },
                    ),
                    proactive: Some(
                        crate::ProactiveStat {
                            ts_secs: 1_715_003_000,
                            surfaced: vec![
                                crate::ProactiveSurfaced {
                                    kind: crate::ProactiveKind::DueReminder,
                                    topic: "rem".into(),
                                    reason: "reminder in 'rem' was due 2h ago".into(),
                                },
                            ],
                            deduped: 1,
                            capped: 0,
                        },
                    ),
                    persona_lifecycle: Some(
                        crate::PersonaLifecycleStat {
                            ts_secs: 1_715_004_000,
                            proposed: vec![
                                crate::PersonaLifecycleProposed {
                                    kind: "consolidate".into(),
                                    category: crate::SoftCategory::LearnedContext,
                                    value: "dup a".into(),
                                    reason: "2 near-duplicate learned_context facets (cosine > 0.92)".into(),
                                },
                            ],
                            deduped: 1,
                        },
                    ),
                    accumulated_helpfulness: Some(
                        crate::AccumulatedHelpfulness {
                            top_helpful: vec![
                                crate::TopicScore {
                                    topic: "project/x".into(),
                                    score: 12.5,
                                    samples: 7,
                                },
                            ],
                            top_unhelpful: vec![
                                crate::TopicScore {
                                    topic: "scratch".into(),
                                    score: -4.0,
                                    samples: 3,
                                },
                            ],
                        },
                    ),
                    cooccurrence: Some(
                        crate::CooccurrencePatterns {
                            top_pairs: vec![
                                crate::PairScore {
                                    a: "deploy".into(),
                                    b: "rollback".into(),
                                    score: 8.0,
                                    samples: 5,
                                },
                            ],
                        },
                    ),
                    cluster_recall: Some(
                        crate::RecallClusterStat {
                            ts_secs: 1_715_005_000,
                            injected: 1,
                            pairs: vec![(
                                "deploy".into(),
                                "rollback".into(),
                            )],
                        },
                    ),
                    persona_consolidation: Some(
                        crate::PersonaConsolidationStat {
                            ts_secs: 1_715_005_500,
                            filed: 1,
                            deduped: 0,
                            skipped_unhelpful: 0,
                            llm_unavailable: false,
                            pairs: vec![(
                                "deploy".into(),
                                "rollback".into(),
                            )],
                            superseded: 0,
                        },
                    ),
                    accumulated_corrections: Some(
                        crate::AccumulatedCorrections {
                            top_corrected: vec![
                                crate::TopicCorrections {
                                    topic: "deploy".into(),
                                    count: 3.0,
                                    samples: 2,
                                },
                            ],
                        },
                    ),
                    correction_consolidation: Some(
                        crate::CorrectionConsolidationStat {
                            ts_secs: 1_715_005_550,
                            filed: 1,
                            llm_unavailable: false,
                            topics: vec!["deploy".into()],
                        },
                    ),
                    correction_judgment: Some(
                        crate::CorrectionJudgmentStat {
                            ts_secs: 1_715_005_560,
                            judged: 4,
                            rework: 2,
                            praise: 1,
                            unrelated: 1,
                            structural_fallback: 1,
                            llm_unavailable: false,
                        },
                    ),
                    recall_judgment: Some(
                        crate::RecallJudgmentStat {
                            ts_secs: 1_715_005_600,
                            judged: 3,
                            used: 1,
                            irrelevant: 1,
                            hurt: 1,
                            skipped: 0,
                            llm_unavailable: false,
                            pairs: vec![
                                ("deploy".into(),
                                 crate::RecallJudgment::Used),
                                ("rollback".into(),
                                 crate::RecallJudgment::Irrelevant),
                                ("auth".into(),
                                 crate::RecallJudgment::Hurt),
                            ],
                        },
                    ),
                    cadence: vec![
                        (
                            "nightly".into(),
                            crate::RecentReflectionStat {
                                fired: 6,
                                skipped: 1,
                            },
                        ),
                    ],
                },
            },
            // Phase 70 — proposal query responses.
            DaemonMessage::QueryResponse {
                id: "q-100".into(),
                payload: QueryResponsePayload::ListPersonaProposals {
                    proposals: vec![PersonaProposalSummary {
                        id: "pp-001".into(),
                        proposed_at_unix_ms: 1_715_000_000_000,
                        source_reflection_session_id: "ses-abc".into(),
                        status: "Pending".into(),
                        category: "BehavioralPreferences".into(),
                        proposed_op: serde_json::json!({
                            "kind": "AppendList",
                            "value": "prefer terse",
                        }),
                        proposed_reason: Some(
                            "operator confirmed 3 turns".into(),
                        ),
                        applied_op: None,
                        applied_seq: None,
                        rejected_reason: None,
                        resolved_at_unix_ms: None,
                        supersedes_proposal_id: None,
                    }],
                    total_len: 1,
                },
            },
            DaemonMessage::QueryResponse {
                id: "q-101".into(),
                payload: QueryResponsePayload::GetPersonaProposal {
                    proposal: None,
                },
            },
            // Phase 73 — notification history response.
            DaemonMessage::QueryResponse {
                id: "q-200".into(),
                payload: QueryResponsePayload::ListNotificationHistory {
                    entries: vec![NotificationHistoryEntry {
                        seq: 42,
                        dispatched_at_unix_ms: 1_715_000_000_000,
                        session_id: "ses-abc".into(),
                        trigger_kind: "Cron".into(),
                        trigger_id: "morning-summary".into(),
                        target_name: "phone".into(),
                        outcome_kind: "delivered".into(),
                        outcome_detail: String::new(),
                    }],
                    total_len: 1,
                },
            },
            DaemonMessage::QueryResponse {
                id: "q-006".into(),
                payload: QueryResponsePayload::GetMission {
                    mission: Some(MissionDetail {
                        mission_id: "m-1".into(),
                        role_name: "default".into(),
                        description: "test".into(),
                        state: "GatePending".into(),
                        gates: vec![GateSummary {
                            gate_id: "g-1".into(),
                            reason: "approve please".into(),
                            scope: Some("shell.exec".into()),
                            state: "Pending".into(),
                            created_at: 10,
                            resolved_at: None,
                        }],
                        created_at: 1,
                        updated_at: 5,
                    }),
                },
            },
            // Phase 69 — Web UI desktop notification broadcast.
            DaemonMessage::DesktopNotification {
                title: "Build complete".into(),
                body: "aivyx-core: 1232 tests passed.".into(),
            },
            DaemonMessage::DesktopNotification {
                title: "Trigger fired".into(),
                body: String::new(),
            },
            DaemonMessage::TeamMissionUpdated {
                view: crate::TeamMissionView {
                    id: "m1".into(),
                    goal: "test goal".into(),
                    lead: "coordinator".into(),
                    phase: crate::TeamMissionPhase::Executing,
                    pending_gate: None,
                    halt_reason: None,
                    verify_attempts: 0,
                    progress: 42,
                    steps: vec![],
                },
            },
        ];
        for msg in cases {
            let frame = encode_frame(&msg).expect("encode");
            let (decoded, consumed): (DaemonMessage, _) =
                decode_frame(&frame).expect("decode");
            assert_eq!(decoded, msg);
            assert_eq!(consumed, frame.len());
        }
    }

    // ---- DaemonEnvelope must decode QueryResponse from a DaemonMessage frame ----

    #[test]
    fn daemon_envelope_decodes_query_response() {
        let msg = DaemonMessage::QueryResponse {
            id: "q-1".into(),
            payload: QueryResponsePayload::ListSessions {
                sessions: vec![SessionSummary {
                    session_id: "abc".into(),
                    channel: WireChannelPlatform::Local,
                    trust_tier: aivyx_capability::TrustTier::Trusted,
                    created_at_ms: 0,
                    last_active_at_ms: 0,
                }],
            },
        };
        let frame = encode_frame(&msg).expect("encode");
        let (envelope, consumed): (DaemonEnvelope, _) =
            decode_frame(&frame).expect("decode");
        assert_eq!(consumed, frame.len());
        match envelope {
            DaemonEnvelope::QueryResponse { id, payload } => {
                assert_eq!(id, "q-1");
                match payload {
                    QueryResponsePayload::ListSessions { sessions } => {
                        assert_eq!(sessions.len(), 1);
                        assert_eq!(sessions[0].session_id, "abc");
                    }
                    other => panic!("expected ListSessions, got {other:?}"),
                }
            }
            other => panic!("expected QueryResponse, got {other:?}"),
        }
    }

    // ---- Chapter Tutor — AuthorSkill / SkillAuthored IPC round-trip ----

    #[test]
    fn author_skill_request_round_trips() {
        let msg = FrontendMessage::AuthorSkill {
            id: "skill-cli".into(),
            op: SkillAuthorOp::Update,
            name: "summarize-doc".into(),
            trigger: Some("when asked to summarize".into()),
            procedure: None,
        };
        let frame = encode_frame(&msg).expect("encode");
        let (decoded, consumed): (FrontendMessage, _) =
            decode_frame(&frame).expect("decode");
        assert_eq!(consumed, frame.len());
        match decoded {
            FrontendMessage::AuthorSkill {
                op, name, trigger, procedure, ..
            } => {
                assert_eq!(op, SkillAuthorOp::Update);
                assert_eq!(name, "summarize-doc");
                assert_eq!(trigger.as_deref(), Some("when asked to summarize"));
                assert_eq!(procedure, None);
            }
            other => panic!("expected AuthorSkill, got {other:?}"),
        }
    }

    /// The daemon replies with `DaemonMessage::SkillAuthored`; the client
    /// decodes `DaemonEnvelope`. This guards the cross-enum compatibility the
    /// CLI/Studio rely on.
    #[test]
    fn skill_authored_daemon_message_decodes_as_envelope() {
        let msg = DaemonMessage::SkillAuthored {
            id: "skill-cli".into(),
            ok: true,
            seq: Some(42),
            error: None,
        };
        let frame = encode_frame(&msg).expect("encode");
        let (envelope, consumed): (DaemonEnvelope, _) =
            decode_frame(&frame).expect("decode");
        assert_eq!(consumed, frame.len());
        match envelope {
            DaemonEnvelope::SkillAuthored { ok, seq, error, .. } => {
                assert!(ok);
                assert_eq!(seq, Some(42));
                assert_eq!(error, None);
            }
            other => panic!("expected SkillAuthored, got {other:?}"),
        }
    }

    // ---- Chapter L (L.5) team-mission IPC round-trip ----

    #[test]
    fn team_mission_queries_round_trip() {
        use aivyx_team_types::{MissionPlan, Step};

        let plan = MissionPlan::new(
            "ship",
            vec![
                Step::delegate("a", "researcher", "go"),
                Step::human_gate("g", "reviewer", "ok?").after(["a"]),
            ],
        );
        // The request carrying a full MissionPlan survives the frame.
        let req = FrontendMessage::Query {
            id: "tr".into(),
            payload: QueryPayload::TeamRun { plan: plan.clone(), config: None },
        };
        let frame = encode_frame(&req).expect("encode");
        let (decoded, _): (FrontendMessage, _) = decode_frame(&frame).expect("decode");
        assert_eq!(decoded, req, "TeamRun round-trips with its plan");

        // The list response carrying a full record survives the frame.
        let mut record =
            crate::TeamMissionRecord::new("m1", "ship", plan);
        record.phase = crate::TeamMissionPhase::AwaitingApproval;
        record.pending_gate = Some("g".into());
        let resp = DaemonMessage::QueryResponse {
            id: "tr".into(),
            payload: QueryResponsePayload::TeamMissionList {
                missions: vec![record.clone()],
            },
        };
        let frame = encode_frame(&resp).expect("encode");
        let (env, _): (DaemonEnvelope, _) = decode_frame(&frame).expect("decode");
        match env {
            DaemonEnvelope::QueryResponse { payload, .. } => match payload {
                QueryResponsePayload::TeamMissionList { missions } => {
                    assert_eq!(missions, vec![record]);
                }
                other => panic!("expected TeamMissionList, got {other:?}"),
            },
            other => panic!("expected QueryResponse, got {other:?}"),
        }

        // The resolve response carries the resulting phase.
        let resolved = QueryResponsePayload::TeamGateResolved {
            mission_id: "m1".into(),
            phase: crate::TeamMissionPhase::Rejected,
        };
        let frame = encode_frame(&resolved).expect("encode");
        let (back, _): (QueryResponsePayload, _) = decode_frame(&frame).expect("decode");
        assert_eq!(back, resolved);

        // Chapter Mission Control — pause request/response, a short status
        // message like abort.
        let pause_req = QueryPayload::PauseTeamMission { mission_id: "m1".into() };
        let frame = encode_frame(&pause_req).expect("encode");
        let (back, _): (QueryPayload, _) = decode_frame(&frame).expect("decode");
        assert_eq!(back, pause_req);

        let paused = QueryResponsePayload::TeamMissionPaused {
            mission_id: "m1".into(),
            message: "mission will pause at its next wave boundary".into(),
        };
        let frame = encode_frame(&paused).expect("encode");
        let (back, _): (QueryResponsePayload, _) = decode_frame(&frame).expect("decode");
        assert_eq!(back, paused);

        // Chapter Mission Control — resume request/response, a phase
        // response like resolve.
        let resume_req = QueryPayload::ResumeTeamMission { mission_id: "m1".into() };
        let frame = encode_frame(&resume_req).expect("encode");
        let (back, _): (QueryPayload, _) = decode_frame(&frame).expect("decode");
        assert_eq!(back, resume_req);

        let resumed = QueryResponsePayload::TeamMissionResumed {
            mission_id: "m1".into(),
            phase: crate::TeamMissionPhase::Executing,
        };
        let frame = encode_frame(&resumed).expect("encode");
        let (back, _): (QueryResponsePayload, _) = decode_frame(&frame).expect("decode");
        assert_eq!(back, resumed);
    }

    #[test]
    fn document_mutation_queries_and_response_round_trip() {
        // `WriteFile.overwrite` + `DeleteFile.confirm` default false.
        let w: QueryPayload =
            serde_json::from_str(r#"{"kind":"WriteFile","root":"fs","path":"a.txt","content":"x"}"#)
                .unwrap();
        assert_eq!(w, QueryPayload::WriteFile { root: "fs".into(), path: "a.txt".into(), content: "x".into(), overwrite: false });
        let d: QueryPayload =
            serde_json::from_str(r#"{"kind":"DeleteFile","root":"fs","path":"a.txt"}"#).unwrap();
        assert_eq!(d, QueryPayload::DeleteFile { root: "fs".into(), path: "a.txt".into(), confirm: false });

        for req in [
            QueryPayload::WriteFile { root: "workspace".into(), path: "n.md".into(), content: "# hi".into(), overwrite: true },
            QueryPayload::RenamePath { root: "fs".into(), path: "a.txt".into(), new_path: "b.txt".into() },
            QueryPayload::MakeDir { root: "fs".into(), path: "newdir".into() },
        ] {
            let msg = FrontendMessage::Query { id: "dm".into(), payload: req.clone() };
            let frame = encode_frame(&msg).expect("encode");
            let (back, _): (FrontendMessage, _) = decode_frame(&frame).expect("decode");
            assert_eq!(back, msg);
        }

        for resp in [
            QueryResponsePayload::FsMutation { ok: true, error: None },
            QueryResponsePayload::FsMutation { ok: false, error: Some("already exists".into()) },
        ] {
            let frame = encode_frame(&resp).expect("encode");
            let (back, _): (QueryResponsePayload, _) = decode_frame(&frame).expect("decode");
            assert_eq!(back, resp);
        }
    }

    #[test]
    fn memory_graph_query_and_response_round_trip() {
        let req = FrontendMessage::Query {
            id: "mg".into(),
            payload: QueryPayload::GetMemoryGraph { limit: 40 },
        };
        let frame = encode_frame(&req).expect("encode");
        let (back, _): (FrontendMessage, _) = decode_frame(&frame).expect("decode");
        assert_eq!(back, req);

        let resp = QueryResponsePayload::GetMemoryGraph {
            nodes: vec![
                MemoryGraphNode { topic: "rust".into(), entry_count: 12 },
                MemoryGraphNode { topic: "ops".into(), entry_count: 3 },
            ],
            edges: vec![crate::PairScore {
                a: "rust".into(),
                b: "ops".into(),
                score: 2.5,
                samples: 7,
            }],
        };
        let frame = encode_frame(&resp).expect("encode");
        let (back, _): (QueryResponsePayload, _) = decode_frame(&frame).expect("decode");
        assert_eq!(back, resp, "nodes + reused PairScore edges survive the frame");
    }

    #[test]
    fn wiki_queries_and_responses_round_trip() {
        // Requests.
        for req in [
            QueryPayload::ListWikiPages,
            QueryPayload::GetWikiPage { topic: "deploy".into() },
        ] {
            let msg = FrontendMessage::Query { id: "wk".into(), payload: req.clone() };
            let frame = encode_frame(&msg).expect("encode");
            let (back, _): (FrontendMessage, _) = decode_frame(&frame).expect("decode");
            assert_eq!(back, msg);
        }

        // List response.
        let list = QueryResponsePayload::ListWikiPages {
            pages: vec![crate::wiki::WikiPageSummary {
                topic: "deploy".into(),
                snippet: "ships via ci…".into(),
                entry_count: 4,
                updated_at: 1000,
            }],
        };
        let frame = encode_frame(&list).expect("encode");
        let (back, _): (QueryResponsePayload, _) = decode_frame(&frame).expect("decode");
        assert_eq!(back, list);

        // Full-page response (Some + None).
        let page = crate::wiki::WikiPage {
            topic: "deploy".into(),
            summary: "Deploy ships via CI; rollback by image tag.".into(),
            source_seqs: vec![1, 2, 3],
            entry_count: 3,
            backlinks: vec![crate::wiki::WikiBacklink {
                topic: "ci".into(),
                affinity: 0.8,
                hops: 1,
            }],
            updated_at: 2000,
            source_fingerprint: crate::wiki::WikiPage::fingerprint(&[1, 2, 3]),
        };
        for resp in [
            QueryResponsePayload::GetWikiPage { page: Some(page) },
            QueryResponsePayload::GetWikiPage { page: None },
        ] {
            let frame = encode_frame(&resp).expect("encode");
            let (back, _): (QueryResponsePayload, _) = decode_frame(&frame).expect("decode");
            assert_eq!(back, resp);
        }
    }

    #[test]
    fn knowledge_graph_query_and_response_round_trip() {
        let req = FrontendMessage::Query {
            id: "kg".into(),
            payload: QueryPayload::GetKnowledgeGraph { limit: 80 },
        };
        let frame = encode_frame(&req).expect("encode");
        let (back, _): (FrontendMessage, _) = decode_frame(&frame).expect("decode");
        assert_eq!(back, req);

        let resp = QueryResponsePayload::GetKnowledgeGraph {
            entities: vec![
                crate::graph::GraphEntity { name: "deploy".into(), degree: 2, kind: String::new() },
                crate::graph::GraphEntity { name: "ci".into(), degree: 1, kind: "system".into() },
            ],
            edges: vec![crate::graph::GraphTriple {
                subject: "deploy".into(),
                predicate: "depends-on".into(),
                object: "ci".into(),
                source_seqs: vec![1, 2],
                mentions: 3,
                updated_at: 100,
            }],
        };
        let frame = encode_frame(&resp).expect("encode");
        let (back, _): (QueryResponsePayload, _) = decode_frame(&frame).expect("decode");
        assert_eq!(back, resp, "entities + directed typed edges survive the frame");
    }

    #[test]
    fn get_skills_query_and_response_round_trip() {
        let req = FrontendMessage::Query {
            id: "sk".into(),
            payload: QueryPayload::GetSkills,
        };
        let frame = encode_frame(&req).expect("encode");
        let (back, _): (FrontendMessage, _) = decode_frame(&frame).expect("decode");
        assert_eq!(back, req);

        let resp = QueryResponsePayload::GetSkills {
            skills: vec![SkillView {
                skill: crate::persona::LearnedSkill {
                    name: "deploy".into(),
                    trigger: "when shipping".into(),
                    procedure: "run ci then ship".into(),
                    version: 2,
                    provenance: crate::persona::SkillProvenance {
                        author: crate::persona::SkillAuthor::Agent,
                        reason: Some("authored from knowledge".into()),
                    },
                    refined_from: Some("deploy".into()),
                    domain: Some("deploy".into()),
                },
                ewma_score: 1.5,
                samples: 4,
                invocations: 7,
            }],
            pending_proposals: 2,
        };
        let frame = encode_frame(&resp).expect("encode");
        let (back, _): (QueryResponsePayload, _) = decode_frame(&frame).expect("decode");
        assert_eq!(back, resp, "skill view + effectiveness survive the frame");
    }

    #[test]
    fn get_tool_catalog_query_and_response_round_trip() {
        let req = FrontendMessage::Query {
            id: "tc".into(),
            payload: QueryPayload::GetToolCatalog,
        };
        let frame = encode_frame(&req).expect("encode");
        let (back, _): (FrontendMessage, _) = decode_frame(&frame).expect("decode");
        assert_eq!(back, req);

        let resp = QueryResponsePayload::GetToolCatalog {
            tools: vec![ToolCatalogEntry {
                name: "fs.read".into(),
                description: "Read a file under the sandbox root.".into(),
                scope_base: "fs.read".into(),
                min_tier: aivyx_capability::TrustTier::Trusted,
            }],
        };
        let frame = encode_frame(&resp).expect("encode");
        let (back, _): (QueryResponsePayload, _) = decode_frame(&frame).expect("decode");
        assert_eq!(back, resp, "tool catalog entries survive the frame");
    }

    #[test]
    fn voice_settings_queries_and_responses_round_trip() {
        // SetVoice with all fields absent (the "clear everything" shape).
        let empty: QueryPayload = serde_json::from_str(r#"{"kind":"SetVoice"}"#).unwrap();
        assert!(matches!(empty, QueryPayload::SetVoice { asr_engine: None, .. }));

        let reqs = vec![
            QueryPayload::GetVoiceSettings,
            QueryPayload::SetVoice {
                asr_engine: Some("whisper-rs".into()),
                tts_engine: Some("kokoro".into()),
                asr_model_path: Some("/m/whisper.bin".into()),
                asr_language: Some("en".into()),
                asr_beam_size: Some(5),
                tts_model_dir: Some("/m/kokoro".into()),
                tts_voice_name: Some("af_heart".into()),
                tts_speed: Some(1.0),
                input_device: None,
                output_device: None,
            },
        ];
        for payload in reqs {
            let msg = FrontendMessage::Query { id: "v".into(), payload: payload.clone() };
            let frame = encode_frame(&msg).expect("encode");
            let (back, _): (FrontendMessage, _) = decode_frame(&frame).expect("decode");
            assert_eq!(back, msg);
        }

        let snap = VoiceSettingsSnapshot {
            asr_model_path: Some("/m/whisper.bin".into()),
            asr_beam_size: Some(5),
            tts_model_dir: Some("/m/kokoro".into()),
            tts_voice_name: Some("af_heart".into()),
            tts_speed: Some(1.0),
            asr_model_status: "present".into(),
            tts_model_status: "present".into(),
            tts_voices_status: "missing".into(),
            ..Default::default()
        };
        for resp in [
            QueryResponsePayload::GetVoiceSettings { settings: snap.clone() },
            QueryResponsePayload::VoiceApplied { settings: snap.clone(), restart_required: true },
        ] {
            let frame = encode_frame(&resp).expect("encode");
            let (back, _): (QueryResponsePayload, _) = decode_frame(&frame).expect("decode");
            assert_eq!(back, resp);
        }
    }

    #[test]
    fn get_team_roster_round_trips_the_full_config() {
        use aivyx_team_types::{DialogueConfig, TeamConfig, TeamMember, TrustTier};

        let roster = TeamConfig {
            name: "Nonagon".into(),
            description: "the default nine".into(),
            lead: "orchestrator".into(),
            members: vec![
                TeamMember {
                    name: "orchestrator".into(),
                    role: "Lead / Orchestrator".into(),
                    soul: "You coordinate the team.".into(),
                    tool_allowlist: vec!["team.message".into()],
                    capability_scopes: vec!["fs.read".into()],
                    trust_ceiling: TrustTier::Trusted,
                    model: None,
                    base_url: None,
                },
                TeamMember {
                    name: "researcher".into(),
                    role: "Research".into(),
                    soul: "You gather facts.".into(),
                    tool_allowlist: vec![],
                    capability_scopes: vec![],
                    trust_ceiling: TrustTier::SemiTrusted,
                    model: None,
                    base_url: None,
                },
            ],
            dialogue: DialogueConfig::default(),
        };
        let req = FrontendMessage::Query {
            id: "mc-teams".into(),
            payload: QueryPayload::GetTeamRoster,
        };
        let frame = encode_frame(&req).expect("encode");
        let (back, _): (FrontendMessage, _) = decode_frame(&frame).expect("decode");
        assert_eq!(back, req);

        let resp = QueryResponsePayload::GetTeamRoster {
            roster: roster.clone(),
        };
        let frame = encode_frame(&resp).expect("encode");
        let (back, _): (QueryResponsePayload, _) = decode_frame(&frame).expect("decode");
        assert_eq!(back, resp, "the full TeamConfig survives the frame");
    }

    #[test]
    fn set_team_roster_round_trips_the_request_and_response() {
        use aivyx_team_types::{DialogueConfig, TeamConfig, TeamMember, TrustTier};

        let roster = TeamConfig {
            name: "edited-team".into(),
            description: "operator-authored".into(),
            lead: "boss".into(),
            members: vec![
                TeamMember {
                    name: "boss".into(),
                    role: "Lead".into(),
                    soul: "You lead.".into(),
                    tool_allowlist: vec!["team.message".into()],
                    capability_scopes: vec!["fs.read".into()],
                    trust_ceiling: TrustTier::Trusted,
                    model: None,
                    base_url: None,
                },
                TeamMember {
                    name: "helper".into(),
                    role: "Helper".into(),
                    soul: "You help.".into(),
                    tool_allowlist: vec![],
                    capability_scopes: vec![],
                    trust_ceiling: TrustTier::SemiTrusted,
                    model: None,
                    base_url: None,
                },
            ],
            dialogue: DialogueConfig::default(),
        };
        // The write request carries the whole roster.
        let req = FrontendMessage::Query {
            id: "mc-roster-set".into(),
            payload: QueryPayload::SetTeamRoster { roster: roster.clone() },
        };
        let frame = encode_frame(&req).expect("encode");
        let (back, _): (FrontendMessage, _) = decode_frame(&frame).expect("decode");
        assert_eq!(back, req);

        // The response echoes the validated roster + restart_required.
        let resp = QueryResponsePayload::TeamRosterApplied { roster, restart_required: true };
        let frame = encode_frame(&resp).expect("encode");
        let (back, _): (QueryResponsePayload, _) = decode_frame(&frame).expect("decode");
        assert_eq!(back, resp);
    }

    #[test]
    fn document_browse_queries_and_responses_round_trip() {
        // Requests — `ListDir.path` defaults absent (root listing).
        let absent: QueryPayload = serde_json::from_str(r#"{"kind":"ListDir","root":"fs"}"#).unwrap();
        assert_eq!(absent, QueryPayload::ListDir { root: "fs".into(), path: String::new() });
        for req in [
            QueryPayload::ListDir { root: "workspace".into(), path: "projects".into() },
            QueryPayload::ReadFile { root: "fs".into(), path: "notes/todo.md".into() },
        ] {
            let msg = FrontendMessage::Query { id: "d".into(), payload: req.clone() };
            let frame = encode_frame(&msg).expect("encode");
            let (back, _): (FrontendMessage, _) = decode_frame(&frame).expect("decode");
            assert_eq!(back, msg);
        }

        // Responses.
        let list = QueryResponsePayload::ListDir {
            entries: vec![
                DocEntry { name: "sub".into(), kind: "dir".into(), size_bytes: 0 },
                DocEntry { name: "a.txt".into(), kind: "file".into(), size_bytes: 12 },
            ],
            path: "projects".into(),
        };
        let read = QueryResponsePayload::ReadFile {
            file: DocFile {
                path: "a.txt".into(),
                size_bytes: 12,
                content: Some("hello".into()),
                truncated: false,
                binary: false,
            },
        };
        for resp in [list, read] {
            let frame = encode_frame(&resp).expect("encode");
            let (back, _): (QueryResponsePayload, _) = decode_frame(&frame).expect("decode");
            assert_eq!(back, resp);
        }
    }

    // ---- Chapter U Settings IPC round-trip ----

    fn sample_snapshot() -> SettingsSnapshot {
        SettingsSnapshot {
            access_level: "home".into(),
            fs_root: "/home/op".into(),
            confirm_destructive: true,
            provider: "ollama".into(),
            model: "qwen3:8b".into(),
            num_ctx: Some(16384),
            budget: BudgetSnapshot {
                per_run_usd: Some(5.0),
                per_day_usd: None,
                on_exceeded: "deny".into(),
                alert_at: Some(0.8),
            },
            embeddings_available: false,
            cycle_detection: true,
            autonomy_level: "supervised".into(),
        }
    }

    #[test]
    fn settings_queries_round_trip() {
        // Each request variant survives a frame round-trip (incl. the
        // confirm flag and the per-dimension budget caps).
        let reqs = vec![
            QueryPayload::GetSettings,
            QueryPayload::SetAccessLevel {
                level: "full".into(),
                root: None,
                confirm: true,
            },
            QueryPayload::SetAccessLevel {
                level: "custom".into(),
                root: Some("/srv/agent".into()),
                confirm: true,
            },
            QueryPayload::SetBudget {
                per_run_usd: Some(2.5),
                per_day_usd: Some(20.0),
                on_exceeded: Some("alert".into()),
                alert_at: Some(0.9),
            },
            QueryPayload::SetCycleDetection { enabled: true },
            QueryPayload::SetAutonomyLevel {
                level: "autonomous".into(),
                confirm: true,
            },
        ];
        for payload in reqs {
            let msg = FrontendMessage::Query {
                id: "s".into(),
                payload: payload.clone(),
            };
            let frame = encode_frame(&msg).expect("encode");
            let (decoded, _): (FrontendMessage, _) = decode_frame(&frame).expect("decode");
            assert_eq!(decoded, msg, "settings query round-trips");
        }
    }

    #[test]
    fn allow_cloud_escalation_round_trips() {
        let msg = FrontendMessage::Query {
            id: "q".into(),
            payload: QueryPayload::AllowCloudEscalation {
                session_id: "0b5c7d2e-0000-4000-8000-000000000000".into(),
            },
        };
        let frame = encode_frame(&msg).expect("encode");
        let (decoded, _): (FrontendMessage, _) = decode_frame(&frame).expect("decode");
        assert_eq!(decoded, msg);

        for payload in [
            QueryResponsePayload::CloudEscalationAllowed {
                session_id: "s".into(),
            },
            QueryResponsePayload::CloudEscalationNotEnabled,
        ] {
            let frame = encode_frame(&payload).expect("encode");
            let (back, _): (QueryResponsePayload, _) = decode_frame(&frame).expect("decode");
            assert_eq!(back, payload);
        }
    }

    #[test]
    fn settings_responses_round_trip() {
        let snap = sample_snapshot();
        let responses = vec![
            QueryResponsePayload::GetSettings {
                settings: snap.clone(),
            },
            QueryResponsePayload::SettingsApplied {
                settings: snap.clone(),
                restart_required: true,
            },
        ];
        for payload in responses {
            let frame = encode_frame(&payload).expect("encode");
            let (back, _): (QueryResponsePayload, _) = decode_frame(&frame).expect("decode");
            assert_eq!(back, payload, "settings response round-trips with its snapshot");
        }
    }

    #[test]
    fn set_access_level_defaults_root_and_confirm() {
        // A pre-Chapter-U-shaped or minimal client may omit `root`/`confirm`;
        // serde defaults must decode them (root: None, confirm: false) so an
        // expanded-level request without an explicit confirm is *not* silently
        // treated as confirmed.
        let json = r#"{"kind":"SetAccessLevel","level":"home"}"#;
        let decoded: QueryPayload = serde_json::from_str(json).expect("decode");
        assert_eq!(
            decoded,
            QueryPayload::SetAccessLevel {
                level: "home".into(),
                root: None,
                confirm: false,
            }
        );
    }

    #[test]
    fn set_budget_defaults_all_fields_absent() {
        // An empty SetBudget clears every cap (all None) and leaves
        // on_exceeded to the daemon's default — no field is required on the
        // wire.
        let json = r#"{"kind":"SetBudget"}"#;
        let decoded: QueryPayload = serde_json::from_str(json).expect("decode");
        assert_eq!(
            decoded,
            QueryPayload::SetBudget {
                per_run_usd: None,
                per_day_usd: None,
                on_exceeded: None,
                alert_at: None,
            }
        );
    }

    #[test]
    fn seed_persona_request_round_trips() {
        let seed = PersonaSeedWire {
            learned_context: vec!["operator builds Aivyx".into()],
            communication_adaptations: vec![],
            character_traits: vec!["pragmatic".into(), "precise".into()],
            relationship_milestones: vec!["genesis: first launch".into()],
            skills: vec![SeedSkillWire {
                name: "rust-review".into(),
                trigger: "when reviewing Rust".into(),
                procedure: "check unwraps".into(),
            }],
        };
        let msg = FrontendMessage::SeedPersona {
            id: "mc-agents-seed".into(),
            seed,
        };
        let frame = encode_frame(&msg).expect("encode");
        let (back, _): (FrontendMessage, _) = decode_frame(&frame).expect("decode");
        assert_eq!(back, msg);
    }

    #[test]
    fn persona_seed_resolved_round_trips() {
        for (ok, appended, error) in [
            (true, 5u64, None),
            (false, 0u64, Some("the persona already has content".to_string())),
        ] {
            let env = DaemonEnvelope::PersonaSeedResolved {
                id: "mc-agents-seed".into(),
                ok,
                appended,
                error,
            };
            let frame = encode_frame(&env).expect("encode");
            let (back, _): (DaemonEnvelope, _) = decode_frame(&frame).expect("decode");
            assert_eq!(back, env);
        }
    }

    #[test]
    fn server_info_round_trips() {
        let msg = DaemonEnvelope::ServerInfo {
            boot_id: "test-boot-id".to_string(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"type\":\"ServerInfo\""));
        let back: DaemonEnvelope = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn draft_persona_seed_request_and_response_round_trip() {
        let req = FrontendMessage::DraftPersonaSeed {
            id: "mc-agents-draft".into(),
            description: "a witty, terse pair-programmer".into(),
        };
        let frame = encode_frame(&req).expect("encode");
        let (back, _): (FrontendMessage, _) = decode_frame(&frame).expect("decode");
        assert_eq!(back, req);

        for resp in [
            DaemonEnvelope::PersonaSeedDrafted {
                id: "mc-agents-draft".into(),
                draft: Some(PersonaSeedWire {
                    character_traits: vec!["witty".into()],
                    ..Default::default()
                }),
                error: None,
            },
            DaemonEnvelope::PersonaSeedDrafted {
                id: "mc-agents-draft".into(),
                draft: None,
                error: Some("no model configured".into()),
            },
        ] {
            let frame = encode_frame(&resp).expect("encode");
            let (back, _): (DaemonEnvelope, _) = decode_frame(&frame).expect("decode");
            assert_eq!(back, resp);
        }
    }

    #[test]
    fn persona_seed_wire_defaults_absent_lists() {
        // A minimal seed (only traits) decodes with the other lists empty.
        let json = r#"{"character_traits":["witty"]}"#;
        let seed: PersonaSeedWire = serde_json::from_str(json).expect("decode");
        assert_eq!(seed.character_traits, vec!["witty"]);
        assert!(seed.learned_context.is_empty());
        assert!(seed.skills.is_empty());
    }

    #[test]
    fn draft_profile_request_and_response_round_trip() {
        // Chapter Genesis — the onboarding answers go out, the drafted
        // Profile (or a typed error) comes back, both surviving a frame.
        let req = FrontendMessage::DraftProfile {
            id: "mc-onboard-draft".into(),
            intent: "a calm thinking partner for my writing".into(),
            role: "collaborator".into(),
            tone: "warm but concise".into(),
            never_do: "never flatter; never pad answers".into(),
        };
        let frame = encode_frame(&req).expect("encode");
        let (back, _): (FrontendMessage, _) = decode_frame(&frame).expect("decode");
        assert_eq!(back, req);

        for resp in [
            DaemonEnvelope::ProfileDrafted {
                id: "mc-onboard-draft".into(),
                draft: Some(ProfileDraftWire {
                    assistant_name: Some("Quill".into()),
                    primary_use_cases: vec!["writing".into()],
                    behavioral_constraints: vec!["never flatter".into()],
                    ..Default::default()
                }),
                error: None,
            },
            DaemonEnvelope::ProfileDrafted {
                id: "mc-onboard-draft".into(),
                draft: None,
                error: Some("no model configured".into()),
            },
        ] {
            let frame = encode_frame(&resp).expect("encode");
            let (back, _): (DaemonEnvelope, _) = decode_frame(&frame).expect("decode");
            assert_eq!(back, resp);
        }
    }

    #[test]
    fn profile_draft_wire_defaults_absent_fields() {
        // A minimal draft (only a name) decodes with scalars None + lists empty.
        let json = r#"{"assistant_name":"Quill"}"#;
        let d: ProfileDraftWire = serde_json::from_str(json).expect("decode");
        assert_eq!(d.assistant_name.as_deref(), Some("Quill"));
        assert!(d.operator_profile.is_none());
        assert!(d.primary_use_cases.is_empty());
        assert!(d.behavioral_constraints.is_empty());
    }

    #[test]
    fn set_profile_round_trips_full_and_empty() {
        // Chapter V — every Profile field survives a frame round-trip, both
        // fully-populated and fully-absent (the "clear everything" shape).
        let reqs = vec![
            QueryPayload::SetProfile {
                assistant_name: Some("Aria".into()),
                operator_profile: Some("Indie dev".into()),
                communication_style: Some("terse".into()),
                primary_use_cases: Some(vec!["coding".into(), "research".into()]),
                behavioral_preferences: Some(vec!["cite sources".into()]),
                behavioral_constraints: Some(vec!["no secrets in logs".into()]),
            },
            QueryPayload::SetProfile {
                assistant_name: None,
                operator_profile: None,
                communication_style: None,
                primary_use_cases: None,
                behavioral_preferences: None,
                behavioral_constraints: None,
            },
        ];
        for payload in reqs {
            let msg = FrontendMessage::Query {
                id: "p".into(),
                payload: payload.clone(),
            };
            let frame = encode_frame(&msg).expect("encode");
            let (decoded, _): (FrontendMessage, _) = decode_frame(&frame).expect("decode");
            assert_eq!(decoded, msg, "set-profile query round-trips");
        }
    }

    #[test]
    fn get_profile_from_disk_defaults_false_and_round_trips() {
        // Wire-compat: a pre-Chapter-V client sends `{"kind":"GetProfile"}`
        // with no `from_disk` field; it must decode to the running-state
        // meaning (`false`), never silently re-reading disk.
        let legacy: QueryPayload = serde_json::from_str(r#"{"kind":"GetProfile"}"#).expect("decode");
        assert_eq!(legacy, QueryPayload::GetProfile { from_disk: false });

        // And the explicit editor form (`true`) survives a frame round-trip.
        for from_disk in [false, true] {
            let msg = FrontendMessage::Query {
                id: "gp".into(),
                payload: QueryPayload::GetProfile { from_disk },
            };
            let frame = encode_frame(&msg).expect("encode");
            let (decoded, _): (FrontendMessage, _) = decode_frame(&frame).expect("decode");
            assert_eq!(decoded, msg);
        }
    }

    #[test]
    fn set_profile_defaults_all_fields_absent() {
        // A minimal SetProfile clears every declared field (all None) — no
        // field is required on the wire, matching SetBudget's posture.
        let json = r#"{"kind":"SetProfile"}"#;
        let decoded: QueryPayload = serde_json::from_str(json).expect("decode");
        assert_eq!(
            decoded,
            QueryPayload::SetProfile {
                assistant_name: None,
                operator_profile: None,
                communication_style: None,
                primary_use_cases: None,
                behavioral_preferences: None,
                behavioral_constraints: None,
            }
        );
    }

    #[test]
    fn set_profile_explicit_empty_list_decodes_as_some() {
        // An explicit `[]` is "declared but empty" — Some(vec![]) — distinct
        // from absent (None). The web editor relies on this to clear-vs-declare.
        let json = r#"{"kind":"SetProfile","primary_use_cases":[]}"#;
        let decoded: QueryPayload = serde_json::from_str(json).expect("decode");
        assert_eq!(
            decoded,
            QueryPayload::SetProfile {
                assistant_name: None,
                operator_profile: None,
                communication_style: None,
                primary_use_cases: Some(vec![]),
                behavioral_preferences: None,
                behavioral_constraints: None,
            }
        );
    }

    #[test]
    fn profile_applied_response_round_trips() {
        let payload = QueryResponsePayload::ProfileApplied {
            profile: ProfileSummary {
                assistant_name: "Aria".into(),
                assistant_name_source: "Toml".into(),
                operator_profile: Some("Indie dev".into()),
                communication_style: Some("terse".into()),
                primary_use_cases: vec!["coding".into()],
                behavioral_preferences: vec!["cite sources".into()],
                behavioral_constraints: vec![],
                injection_enabled: true,
            },
            restart_required: true,
        };
        let frame = encode_frame(&payload).expect("encode");
        let (back, _): (QueryResponsePayload, _) = decode_frame(&frame).expect("decode");
        assert_eq!(back, payload, "profile-applied response round-trips");
    }

    // ---- DaemonLifecycleEvent round-trip ----

    #[test]
    fn daemon_lifecycle_event_round_trips() {
        let cases = vec![
            DaemonLifecycleEvent::DaemonReady {
                version: "0.1".into(),
            },
            DaemonLifecycleEvent::ShuttingDown {
                reason: "operator requested".into(),
            },
            DaemonLifecycleEvent::RecoveryNotice {
                lost_sessions: vec!["ses-1".into(), "ses-2".into()],
                lost_turns: vec!["ses-1:turn".into()],
                stale_since: 1713700000,
            },
        ];
        for msg in cases {
            let frame = encode_frame(&msg).expect("encode");
            let (decoded, consumed): (DaemonLifecycleEvent, _) =
                decode_frame(&frame).expect("decode");
            assert_eq!(decoded, msg);
            assert_eq!(consumed, frame.len());
        }
    }

    // ---- Max payload size boundary ----

    #[test]
    fn encode_rejects_oversized_payload() {
        let huge = "x".repeat(MAX_PAYLOAD_SIZE as usize + 1);
        let msg = FrontendMessage::SubmitInput {
            session_id: "s".into(),
            text: huge,
            mission_id: None,
            attachments: vec![],
            headless: false,
        };
        let err = encode_frame(&msg).unwrap_err();
        assert!(matches!(err, FrameError::PayloadTooLarge(_)));
    }

    #[test]
    fn decode_rejects_oversized_length_prefix() {
        let mut buf = vec![0u8; 8];
        let bad_len: u32 = MAX_PAYLOAD_SIZE + 1;
        buf[0..4].copy_from_slice(&bad_len.to_be_bytes());
        let err = decode_frame::<FrontendMessage>(&buf).unwrap_err();
        assert!(matches!(err, FrameError::PayloadTooLarge(_)));
    }

    // ---- Incomplete buffer ----

    #[test]
    fn decode_returns_incomplete_for_short_buffer() {
        assert!(matches!(
            decode_frame::<FrontendMessage>(&[0, 0]),
            Err(FrameError::IncompleteBuf)
        ));
        // Header says 10 bytes but only 2 payload bytes present
        let buf = [0, 0, 0, 10, b'h', b'i'];
        assert!(matches!(
            decode_frame::<FrontendMessage>(&buf),
            Err(FrameError::IncompleteBuf)
        ));
    }

    // ---- DaemonEnvelope demux ----

    #[test]
    fn daemon_envelope_demuxes_all_variants() {
        let lifecycle = DaemonLifecycleEvent::DaemonReady {
            version: "0.1".into(),
        };
        let frame = encode_frame(&lifecycle).expect("encode lifecycle");
        let (envelope, _): (DaemonEnvelope, _) = decode_frame(&frame).expect("decode");
        assert!(matches!(envelope, DaemonEnvelope::DaemonReady { .. }));

        let turn = DaemonMessage::TurnComplete {
            session_id: "s1".into(),
            outcome: "done".into(),
        };
        let frame = encode_frame(&turn).expect("encode turn");
        let (envelope, _): (DaemonEnvelope, _) = decode_frame(&frame).expect("decode");
        assert!(matches!(envelope, DaemonEnvelope::TurnComplete { .. }));

        let accepted = DaemonMessage::ProtocolAccepted {
            version: "0.1".into(),
        };
        let frame = encode_frame(&accepted).expect("encode accepted");
        let (envelope, _): (DaemonEnvelope, _) = decode_frame(&frame).expect("decode");
        assert!(matches!(envelope, DaemonEnvelope::ProtocolAccepted { .. }));

        let rejected = DaemonMessage::ProtocolRejected {
            supported: vec!["0.1".into()],
        };
        let frame = encode_frame(&rejected).expect("encode rejected");
        let (envelope, _): (DaemonEnvelope, _) = decode_frame(&frame).expect("decode");
        assert!(matches!(envelope, DaemonEnvelope::ProtocolRejected { .. }));

        let recovery = DaemonLifecycleEvent::RecoveryNotice {
            lost_sessions: vec![],
            lost_turns: vec![],
            stale_since: 0,
        };
        let frame = encode_frame(&recovery).expect("encode recovery");
        let (envelope, _): (DaemonEnvelope, _) = decode_frame(&frame).expect("decode");
        assert!(matches!(envelope, DaemonEnvelope::RecoveryNotice { .. }));

        // Phase 69 — DesktopNotification demux from a DaemonMessage frame.
        let desktop = DaemonMessage::DesktopNotification {
            title: "hello".into(),
            body: "world".into(),
        };
        let frame = encode_frame(&desktop).expect("encode desktop");
        let (envelope, _): (DaemonEnvelope, _) = decode_frame(&frame).expect("decode");
        match envelope {
            DaemonEnvelope::DesktopNotification { title, body } => {
                assert_eq!(title, "hello");
                assert_eq!(body, "world");
            }
            other => panic!("expected DesktopNotification, got {other:?}"),
        }

        // Chapter Mission Control — TeamMissionUpdated demux from a
        // DaemonMessage frame.
        let update = DaemonMessage::TeamMissionUpdated {
            view: crate::TeamMissionView {
                id: "m2".into(),
                goal: "another goal".into(),
                lead: "coordinator".into(),
                phase: crate::TeamMissionPhase::Done,
                pending_gate: None,
                halt_reason: None,
                verify_attempts: 0,
                progress: 100,
                steps: vec![],
            },
        };
        let frame = encode_frame(&update).expect("encode update");
        let (envelope, _): (DaemonEnvelope, _) = decode_frame(&frame).expect("decode");
        match envelope {
            DaemonEnvelope::TeamMissionUpdated { view } => {
                assert_eq!(view.id, "m2");
                assert_eq!(view.progress, 100);
            }
            other => panic!("expected TeamMissionUpdated, got {other:?}"),
        }
    }

    // ---- StreamEventPayload covers all variants ----

    #[test]
    fn stream_event_payload_all_variants_round_trip() {
        let cases = vec![
            StreamEventPayload::Text {
                text: "hello".into(),
            },
            StreamEventPayload::Status {
                status: "thinking...".into(),
            },
            StreamEventPayload::ToolCallStarted {
                tool_id: "id-1".into(),
                tool_name: "memory.read".into(),
                input: serde_json::json!({"topic": "notes"}),
            },
            StreamEventPayload::ToolCallFinished {
                tool_id: "id-1".into(),
                tool_name: "memory.read".into(),
                outcome_summary: "3 entries".into(),
            },
            StreamEventPayload::ToolOutput {
                tool_id: "id-1".into(),
                tool_name: "web.fetch".into(),
                chunk: "<html>...".into(),
            },
            StreamEventPayload::ApprovalGate {
                mission_id: "m-001".into(),
                gate_id: "g-001".into(),
                reason: "deploy to production?".into(),
                scope: Some("shell.exec".into()),
            },
            StreamEventPayload::ApprovalGate {
                mission_id: "m-002".into(),
                gate_id: "g-010".into(),
                reason: "proceed with analysis?".into(),
                scope: None,
            },
        ];
        for payload in cases {
            let json = serde_json::to_string(&payload).expect("serialize");
            let back: StreamEventPayload = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back, payload);
        }
    }

    // ---- Protocol version constant ----

    #[test]
    fn protocol_version_matches_spec() {
        assert_eq!(PROTOCOL_VERSION, "0.1");
    }

    // ---- Socket path resolver ----

    #[test]
    fn default_socket_path_uses_xdg_runtime_dir_when_set() {
        // We can't mutate env in parallel tests safely, so just verify
        // the function returns Ok when HOME is set (which it always is
        // in CI and dev). The exact path depends on the environment.
        let result = default_socket_path();
        assert!(
            result.is_ok(),
            "default_socket_path must succeed when HOME is set: {result:?}"
        );
        let path = result.unwrap();
        assert!(
            path.ends_with("daemon.sock"),
            "path must end with daemon.sock: {path:?}"
        );
    }

    #[test]
    fn default_pid_path_is_sibling_of_socket_path() {
        let pid = default_pid_path();
        assert!(
            pid.is_ok(),
            "default_pid_path must succeed when HOME is set: {pid:?}"
        );
        let path = pid.unwrap();
        assert!(
            path.ends_with("daemon.pid"),
            "path must end with daemon.pid: {path:?}"
        );
        let sock = default_socket_path().unwrap();
        assert_eq!(
            path.parent(),
            sock.parent(),
            "pid and socket paths must share the same parent directory"
        );
    }

    // ---- render_for_cli ----

    #[test]
    fn render_for_cli_text_passes_through() {
        let payload = StreamEventPayload::Text {
            text: "hello world".into(),
        };
        assert_eq!(payload.render_for_cli(), "hello world");
    }

    #[test]
    fn render_for_cli_tool_call_started_includes_arrow_and_name() {
        let payload = StreamEventPayload::ToolCallStarted {
            tool_id: "id".into(),
            tool_name: "fs.read".into(),
            input: serde_json::json!({"path": "/tmp"}),
        };
        let rendered = payload.render_for_cli();
        assert!(rendered.starts_with("  → fs.read"), "got: {rendered}");
        assert!(rendered.contains("/tmp"), "got: {rendered}");
    }

    #[test]
    fn render_for_cli_tool_call_finished_includes_arrow_and_summary() {
        let payload = StreamEventPayload::ToolCallFinished {
            tool_id: "id".into(),
            tool_name: "memory.read".into(),
            outcome_summary: "3 entries".into(),
        };
        let rendered = payload.render_for_cli();
        assert!(rendered.starts_with("  ← memory.read"), "got: {rendered}");
        assert!(rendered.contains("3 entries"), "got: {rendered}");
    }

    #[test]
    fn render_for_cli_approval_gate_with_scope() {
        let payload = StreamEventPayload::ApprovalGate {
            mission_id: "m-001".into(),
            gate_id: "g-001".into(),
            reason: "deploy to production?".into(),
            scope: Some("shell.exec".into()),
        };
        let rendered = payload.render_for_cli();
        assert!(rendered.contains("APPROVAL GATE"), "got: {rendered}");
        assert!(rendered.contains("m-001/g-001"), "got: {rendered}");
        assert!(rendered.contains("deploy to production?"), "got: {rendered}");
        assert!(rendered.contains("scope: shell.exec"), "got: {rendered}");
    }

    #[test]
    fn render_for_cli_approval_gate_without_scope() {
        let payload = StreamEventPayload::ApprovalGate {
            mission_id: "m-002".into(),
            gate_id: "g-010".into(),
            reason: "proceed?".into(),
            scope: None,
        };
        let rendered = payload.render_for_cli();
        assert!(rendered.contains("m-002/g-010"), "got: {rendered}");
        assert!(!rendered.contains("scope:"), "got: {rendered}");
    }

    // ---- concat_text_events / turn_outcome_correction (turn-outcome
    // correction follow-up to POLISH_WAVES.md sub-project 4) ----

    #[test]
    fn concat_text_events_joins_only_text_chunks() {
        let events = vec![
            StreamEventPayload::Status {
                status: "thinking".into(),
            },
            StreamEventPayload::Text { text: "Hi".into() },
            StreamEventPayload::ToolCallStarted {
                tool_id: "id".into(),
                tool_name: "web_search".into(),
                input: serde_json::json!({}),
            },
            StreamEventPayload::Text {
                text: " there".into(),
            },
        ];
        assert_eq!(concat_text_events(&events), "Hi there");
    }

    #[test]
    fn concat_text_events_empty_for_no_text_events() {
        let events = vec![StreamEventPayload::Status {
            status: "thinking".into(),
        }];
        assert_eq!(concat_text_events(&events), "");
    }

    #[test]
    fn turn_outcome_correction_none_when_text_matches_final_message() {
        assert_eq!(
            turn_outcome_correction("Your home airport is Jandakot.", "completed: Your home airport is Jandakot."),
            None
        );
    }

    #[test]
    fn turn_outcome_correction_no_reply_when_both_empty() {
        assert_eq!(
            turn_outcome_correction("", "completed: "),
            Some("(no reply)".to_string())
        );
    }

    #[test]
    fn turn_outcome_correction_shows_final_message_when_nothing_displayed() {
        assert_eq!(
            turn_outcome_correction("", "completed: I wasn't able to produce a usable reply this turn — please try again."),
            Some("I wasn't able to produce a usable reply this turn — please try again.".to_string())
        );
    }

    #[test]
    fn turn_outcome_correction_flags_a_correction_when_displayed_and_final_differ() {
        let leaked = "{\"path\": \"airports.csv\"}";
        let corrected = "completed: I wasn't able to produce a usable reply this turn — please try again.";
        assert_eq!(
            turn_outcome_correction(leaked, corrected),
            Some("⚠ corrected: I wasn't able to produce a usable reply this turn — please try again.".to_string())
        );
    }

    #[test]
    fn turn_outcome_correction_always_surfaces_non_completed_outcomes() {
        assert_eq!(
            turn_outcome_correction("partial answer", "timed out"),
            Some("timed out".to_string())
        );
        assert_eq!(
            turn_outcome_correction("", "stopped: 3 repeated identical tool calls"),
            Some("stopped: 3 repeated identical tool calls".to_string())
        );
        assert_eq!(
            turn_outcome_correction("some text", "escalated: shell.exec needs approval"),
            Some("escalated: shell.exec needs approval".to_string())
        );
    }

    #[test]
    fn turn_outcome_correction_none_when_displayed_has_preamble_before_a_tool_call() {
        // Multi-step turn: the model narrated before calling a tool,
        // then gave its real final answer. displayed contains BOTH;
        // final_message is only the last step's text. Nothing to
        // correct — the real answer matches the tail of what streamed.
        assert_eq!(
            turn_outcome_correction(
                "Let me check that.Your home airport is Jandakot.",
                "completed: Your home airport is Jandakot."
            ),
            None
        );
    }

    #[test]
    fn turn_outcome_correction_shows_only_the_new_annotation_not_the_whole_message() {
        assert_eq!(
            turn_outcome_correction(
                "Your home airport is Jandakot.",
                "completed: Your home airport is Jandakot.\n\n⚠ I said I would check the calendar but never called calendar.list."
            ),
            Some("⚠ I said I would check the calendar but never called calendar.list.".to_string())
        );
    }

    #[test]
    fn turn_outcome_correction_shows_only_new_annotations_with_preamble_too() {
        // Both effects at once: multi-step narration AND a trailing
        // annotation. Only the annotation should show.
        assert_eq!(
            turn_outcome_correction(
                "Let me check.Your home airport is Jandakot.",
                "completed: Your home airport is Jandakot.\n\n⚠ note."
            ),
            Some("⚠ note.".to_string())
        );
    }

    #[test]
    fn turn_outcome_correction_shows_both_annotations_when_two_fired() {
        assert_eq!(
            turn_outcome_correction(
                "Answer.",
                "completed: Answer.\n\n⚠ note1.\n\n⚠ note2."
            ),
            Some("⚠ note1.\n\n⚠ note2.".to_string())
        );
    }

    #[test]
    fn turn_outcome_correction_finds_the_real_annotation_boundary_past_organic_warning_text() {
        // Re-review fix — the model's OWN text can legitimately open a
        // paragraph with a bare "⚠ " before a real annotation is ever
        // appended (e.g. warning the operator about a destructive
        // action). The first "\n\n⚠ " in final_message is that organic
        // paragraph, not the real annotation boundary; only the SECOND
        // one's preceding text actually matches `displayed`.
        let displayed = "Here are the risks:\n\n⚠ This deletes the table.";
        let outcome = "completed: Here are the risks:\n\n⚠ This deletes the table.\n\n⚠ I said I would check the calendar but never called calendar.list.";
        assert_eq!(
            turn_outcome_correction(displayed, outcome),
            Some("⚠ I said I would check the calendar but never called calendar.list.".to_string())
        );
    }

    // ---- Phase 45 — IpcAttachment ----

    #[test]
    fn ipc_submit_with_attachment_roundtrip() {
        let msg = FrontendMessage::SubmitInput {
            session_id: "s1".into(),
            text: "describe this".into(),
            mission_id: None,
            attachments: vec![IpcAttachment {
                media_type: "image/png".into(),
                data_base64: "iVBORw0KGgo=".into(),
                filename: Some("screenshot.png".into()),
            }],
            headless: false,
        };
        let frame = encode_frame(&msg).expect("encode");
        let (decoded, consumed): (FrontendMessage, _) = decode_frame(&frame).expect("decode");
        assert_eq!(decoded, msg);
        assert_eq!(consumed, frame.len());
    }

    #[test]
    fn ipc_submit_no_attachment_backwards_compat() {
        // Simulate an old client that omits the `attachments` field entirely.
        let json = r#"{"type":"SubmitInput","session_id":"s1","text":"hello"}"#;
        let msg: FrontendMessage = serde_json::from_str(json).expect("parse");
        match msg {
            FrontendMessage::SubmitInput {
                text,
                attachments,
                headless,
                ..
            } => {
                assert_eq!(text, "hello");
                assert!(attachments.is_empty(), "default should be empty vec");
                // Chapter H — absent `headless` defaults to interactive.
                assert!(!headless, "headless defaults to false (interactive)");
            }
            other => panic!("expected SubmitInput, got {other:?}"),
        }
    }

    #[test]
    fn ipc_attachment_serde_roundtrip() {
        let att = IpcAttachment {
            media_type: "image/jpeg".into(),
            data_base64: "AAAA".into(),
            filename: None,
        };
        let json = serde_json::to_string(&att).expect("ser");
        let back: IpcAttachment = serde_json::from_str(&json).expect("de");
        assert_eq!(back, att);
    }

    #[test]
    fn redacted_secret_round_trips_without_the_real_value() {
        let s = RedactedSecret { configured: true, source: "toml".to_string() };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"configured\":true"));
        assert!(json.contains("\"source\":\"toml\""));
        let back: RedactedSecret = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
    }

    #[test]
    fn set_mcp_server_round_trips() {
        let msg = QueryPayload::SetMcpServer {
            name: "github".to_string(),
            transport: "stdio".to_string(),
            command: Some("npx".to_string()),
            args: vec!["-y".to_string()],
            env: vec![("TOKEN".to_string(), "${GITHUB_TOKEN}".to_string())],
            headers: Vec::new(),
            url: None,
            enabled: true,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: QueryPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn mcp_server_config_view_round_trips() {
        let view = McpServerConfigView {
            name: "github".to_string(),
            transport: "stdio".to_string(),
            command: Some("npx".to_string()),
            args: vec!["-y".to_string()],
            env: Vec::new(),
            headers: Vec::new(),
            url: None,
            enabled: true,
        };
        let json = serde_json::to_string(&view).unwrap();
        let back: McpServerConfigView = serde_json::from_str(&json).unwrap();
        assert_eq!(back, view);
    }

    #[test]
    fn set_notify_target_round_trips() {
        let msg = QueryPayload::SetNotifyTarget {
            name: "ops".to_string(),
            kind: "telegram".to_string(),
            chat_id: Some("123456".to_string()),
            url: None,
            to: None,
            enabled: true,
            is_default: false,
            retry_count: 0,
            retry_backoff_ms_start: 500,
            rate_limit_max: None,
            rate_limit_window_secs: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: QueryPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn notify_target_config_view_round_trips() {
        let view = NotifyTargetConfigView {
            name: "ops".to_string(),
            kind: "telegram".to_string(),
            chat_id: Some("123456".to_string()),
            url: None,
            to: None,
            enabled: true,
            is_default: false,
            retry_count: 0,
            retry_backoff_ms_start: 500,
            rate_limit_max: None,
            rate_limit_window_secs: None,
        };
        let json = serde_json::to_string(&view).unwrap();
        let back: NotifyTargetConfigView = serde_json::from_str(&json).unwrap();
        assert_eq!(back, view);
    }

    #[test]
    fn set_reflection_schedule_round_trips_over_json() {
        let msg = QueryPayload::SetReflectionSchedule {
            name: "nightly".to_string(),
            cron: "0 0 9 * * * *".to_string(),
            lookback_window_secs: 86_400,
            enabled: true,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: QueryPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }

    #[test]
    fn reflection_schedule_config_view_round_trips_over_json() {
        let view = ReflectionScheduleConfigView {
            name: "nightly".to_string(),
            cron: "0 0 9 * * * *".to_string(),
            lookback_window_secs: 86_400,
            enabled: true,
        };
        let json = serde_json::to_string(&view).unwrap();
        let back: ReflectionScheduleConfigView = serde_json::from_str(&json).unwrap();
        assert_eq!(view, back);
    }

    #[test]
    fn mcp_server_test_result_round_trips() {
        let msg = QueryResponsePayload::McpServerTestResult {
            ok: true,
            tool_count: 4,
            error: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: QueryResponsePayload = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msg);
    }

    // ---- Piece C (2026-08-23) — RunTeamMissionChannel IPC ----

    #[test]
    fn run_team_mission_channel_round_trips_through_frontend_message() {
        let msg = FrontendMessage::RunTeamMissionChannel {
            goal: "close the books".to_string(),
        };
        let json = serde_json::to_string(&msg).expect("serialize");
        let back: FrontendMessage = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(msg, back);
    }

    #[test]
    fn team_mission_channel_started_round_trips_daemon_message_to_envelope() {
        // DaemonMessage (what the daemon writes) must decode as the
        // matching DaemonEnvelope variant (what the client reads) — the
        // same cross-type compatibility every other daemon->client
        // message in this protocol already relies on.
        let msg = DaemonMessage::TeamMissionChannelStarted {
            mission_id: "m-1".to_string(),
        };
        let json = serde_json::to_string(&msg).expect("serialize");
        let envelope: DaemonEnvelope = serde_json::from_str(&json).expect("deserialize as envelope");
        assert_eq!(
            envelope,
            DaemonEnvelope::TeamMissionChannelStarted {
                mission_id: "m-1".to_string()
            }
        );
    }

    #[test]
    fn email_config_view_round_trips_with_redacted_password() {
        let view = EmailConfigView {
            host: Some("smtp.example.com".to_string()),
            port: Some(587),
            tls_mode: Some("starttls".to_string()),
            username: Some("bot@example.com".to_string()),
            password: RedactedSecret { configured: true, source: "toml".to_string() },
            from: Some("bot@example.com".to_string()),
        };
        let json = serde_json::to_string(&view).unwrap();
        assert!(!json.contains("hunter2"), "no real secret value in the type at all, sanity check on the test itself");
        let back: EmailConfigView = serde_json::from_str(&json).unwrap();
        assert_eq!(back, view);
    }

    #[test]
    fn set_slack_config_round_trips() {
        let msg = QueryPayload::SetSlackConfig {
            bot_token: Some("xoxb-1".to_string()),
            app_token: None,
            team_id: None,
            team_run_channel: None,
            team_trigger_rate_limit: None,
            team_command_allowed_senders: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: QueryPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(back, msg);
    }

    #[test]
    fn mcp_server_call_stats_round_trips_over_json() {
        let mut outcomes = std::collections::BTreeMap::new();
        outcomes.insert("completed".to_string(), 3u64);
        outcomes.insert("failed".to_string(), 1u64);
        let stats = McpServerCallStats {
            server_name: "comfyui".to_string(),
            calls: 4,
            outcomes,
            total_duration_ms: 400,
        };
        let json = serde_json::to_string(&stats).unwrap();
        let back: McpServerCallStats = serde_json::from_str(&json).unwrap();
        assert_eq!(stats, back);
    }

    #[test]
    fn get_mcp_server_call_stats_round_trips_over_json() {
        let msg = QueryPayload::GetMcpServerCallStats { window_secs: Some(86_400) };
        let json = serde_json::to_string(&msg).unwrap();
        let back: QueryPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(msg, back);
    }
}
