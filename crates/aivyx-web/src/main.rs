//! Aivyx Studio — a Dioxus (Rust→WASM) browser client (Chapter M; reskinned to
//! the **Stitch** design system in Chapter R).
//!
//! Two live views over the daemon's `/ws` bridge, speaking the protocol through
//! the shared [`aivyx_ipc`] types (the browser sends `serde_json(FrontendMessage)`
//! and receives `serde_json(DaemonEnvelope)` — what `web_ui.rs` relays):
//!
//! - **Missions**: a live `TeamMissionList` feed, start-a-mission, approve/reject
//!   of human gates — the Mission-Orchestration look.
//! - **Chat**: submit a turn, render streamed events, resolve the single-agent
//!   gate — the Terminal look.
//!
//! Chapter R is **presentation-only**: the shell (Sidebar / Topbar / StatusBar),
//! the Stitch token CSS, self-hosted fonts and brand icons, and a small component
//! kit. The WebSocket task, the `aivyx_ipc` data flow, and every handler are
//! unchanged from Chapter M. Stitch tokens are the single source of truth
//! (`aivyx-brand/design-tokens.md`); see `docs/FRONTEND.md`.

use aivyx_ipc::protocol::{
    AuditEntrySummary, DaemonEnvelope, DiscordConfigView, DocEntry, DocFile, EffectivePersonaSummary,
    EmailConfigView, EmbeddingConfigView, FrontendMessage,
    GalleryImage, McpServerCallStats, McpServerConfigView, McpServerStatusView, MemoryEntrySummary, MemoryGraphNode,
    MemoryProfileConfigView,
    NotificationHistoryEntry, NotifyTargetConfigView, NotifyTargetView, PersonaDeltaSummary,
    PersonaProposalResolution,
    PersonaProposalSummary, PersonaSeedWire, ProactiveConfigView, ProfileDraftWire, ProfileSummary, QueryPayload,
    QueryResponsePayload, ReflectionScheduleConfigView, ReminderView, ScheduleView, SeedSkillWire, SessionSummary, SettingsSnapshot,
    SkillAuthorOp, SkillView, SlackConfigView, StreamEventPayload, TelegramConfigView,
    ToolCatalogEntry, VoiceSettingsSnapshot,
    turn_outcome_correction,
};
use aivyx_ipc::{
    LoopRunState, PairScore, ProposedPersonaDelta, TeamConfig, TeamMember, TeamMissionPhase, TeamMissionView,
    TeamStepState, TeamStepView, TrustTier,
};
use aivyx_ipc::wiki::{WikiPage, WikiPageSummary};
use aivyx_ipc::graph::{GraphEntity, GraphTriple};
use std::collections::{HashMap, HashSet};

/// End-user guide content + markdown rendering for the Guide screen.
mod guide;

/// How many recent audit entries the Command Center feed shows.
const AUDIT_FEED_N: u32 = 8;
/// Chapter Herald — how many recent notification-history entries the
/// poll keeps in view (mirrors the audit feed's self-correcting window).
const NOTIFICATION_FEED_N: u32 = 50;
/// Page size for memory topic-entry and search queries.
const MEMORY_LIMIT: u32 = 50;

use dioxus::prelude::*;
use futures_util::{SinkExt, StreamExt};
use gloo_net::websocket::{futures::WebSocket, Message};
use gloo_timers::future::TimeoutFuture;

const POLL_INTERVAL_MS: u32 = 1500;

// ── Bundled assets (every asset goes through `asset!()` so it lands in the
//    bundle and is served offline by the daemon — no CDN). ──────────────────
const STITCH_CSS: Asset = asset!("/assets/stitch.css");
const FAVICON: Asset = asset!("/assets/logos/aivyx-favicon.svg");
const LOGOMARK: Asset = asset!("/assets/logos/aivyx-logomark.svg");
const FONT_DISPLAY: Asset = asset!("/assets/fonts/fraunces-var.woff2");
const FONT_BODY: Asset = asset!("/assets/fonts/ibm-plex-sans-var.woff2");
const FONT_MONO: Asset = asset!("/assets/fonts/ibm-plex-mono-var.woff2");
const ICON_COMMAND: Asset = asset!("/assets/icons/command-center.svg");
const ICON_CHAT: Asset = asset!("/assets/icons/chat.svg");
const ICON_MISSIONS: Asset = asset!("/assets/icons/missions.svg");
const ICON_TEAMS: Asset = asset!("/assets/icons/teams.svg");
const ICON_AGENTS: Asset = asset!("/assets/icons/agents.svg");
const ICON_MEMORY: Asset = asset!("/assets/icons/memory.svg");
const ICON_DOCUMENTS: Asset = asset!("/assets/icons/documents.svg");
const ICON_VOICE: Asset = asset!("/assets/icons/voice.svg");
const ICON_SETTINGS: Asset = asset!("/assets/icons/settings.svg");
const ICON_THEME: Asset = asset!("/assets/icons/theme-toggle.svg");
// Distinct per-screen nav icons (UI polish): every sidebar item gets its own
// glyph instead of sharing one. `plugins`/`candle-flame` are pre-existing brand
// spares; `wiki`/`graph`/`skills`/`guide` were authored to match the set.
const ICON_WIKI: Asset = asset!("/assets/icons/wiki.svg");
const ICON_GRAPH: Asset = asset!("/assets/icons/graph.svg");
const ICON_SKILLS: Asset = asset!("/assets/icons/skills.svg");
const ICON_GUIDE: Asset = asset!("/assets/icons/guide.svg");
const ICON_PLUGINS: Asset = asset!("/assets/icons/plugins.svg");
const ICON_CREATE: Asset = asset!("/assets/icons/candle-flame.svg");
const ICON_SCHEDULES: Asset = asset!("/assets/icons/schedules.svg");
const ICON_LOOP: Asset = asset!("/assets/icons/schedules.svg");
const ICON_NOTIFICATIONS: Asset = asset!("/assets/icons/notifications.svg");
const ICON_TOOLS: Asset = asset!("/assets/icons/tools.svg");
const ICON_GALLERY: Asset = asset!("/assets/icons/gallery.svg");
// POLISH_WAVES.md sub-project 6, item D — vendored, not referenced from
// the base app shell (see FileViewer's mermaid loader below): loading it
// eagerly on every Studio boot would cost every operator a few hundred
// KB of transfer for a screen most sessions never open. `with_minify
// (false)` because the file is already minified upstream — running it
// through the bundler's own minifier again is redundant risk for zero
// benefit.
const MERMAID_JS: Asset = asset!(
    "/assets/vendor/mermaid.min.js",
    JsAssetOptions::new().with_minify(false)
);

/// The shared WebSocket-sender handle (poll loop + UI handlers send to it).
type Sender = Coroutine<FrontendMessage>;

/// Top-level view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    Command,
    Missions,
    /// Chapter Mission Control — a live view of ONE active mission's
    /// LEAD/specialist graph, with drill-in and controls (approve/reject,
    /// abort, pause/resume). Distinct from `Missions` (a flat list/history
    /// plus a "start a new mission" bar) — this is the deep-dive,
    /// one-mission-at-a-time surface.
    MissionControl,
    /// Chapter Chime — cron routines: config/operator/agent-created
    /// schedules, with create/toggle/delete + the agent-proposal
    /// approval flow.
    Schedules,
    /// Chapter Herald — configured notify targets (read-only) +
    /// dispatch history, for missions/schedules that notify outside
    /// the Studio.
    Notifications,
    Loop,
    Reminders,
    Chat,
    Memory,
    /// Chapter Codex — the knowledge-wiki: synthesized per-topic pages.
    Wiki,
    /// Chapter Lattice — the typed knowledge graph: entities + directed
    /// typed relations.
    Lattice,
    /// Chapter Repertoire — the Skills library: every skill + its
    /// effectiveness + provenance/lineage.
    Skills,
    Settings,
    Agents,
    Teams,
    Documents,
    /// Chapter Lantern — the MCP screen: each configured MCP server's
    /// last-start health (connected + tool count, or failed + reason).
    Mcp,
    /// Chapter Almanac — the Tools screen: a read-only, searchable
    /// catalog of every registered tool (name, capability base, minimum
    /// trust tier, description).
    Tools,
    Voice,
    /// The in-app end-user guide — the `docs/guide/*.md` pages rendered in the
    /// Studio (see `guide.rs`). Pure static content, no daemon IPC.
    Guide,
    /// Chapter Genesis — the guided agent-creation flow (Profile → Persona seed
    /// → access). First-run lands here when the Profile isn't yet declared.
    Onboarding,
    /// Studio Gallery — recent images generated via the configured
    /// `comfyui` `[[mcp_server]]`, read from ComfyUI's own `/history` API.
    Gallery,
    Audit,
    /// `/classic` retirement — every active daemon session (channel, trust
    /// tier, created/last-active), replacing `/classic`'s own sessions pane.
    Sessions,
}

impl View {
    /// Every view — drives the command palette + slug lookup. NOT sidebar
    /// order: `Audit`/`Sessions` (`/classic` retirement) are appended here
    /// after `Guide` to minimize diff noise against this array, while the
    /// sidebar itself places them mid-"System" group (see `Sidebar`'s own
    /// `groups`) — reordering this array is a bigger, riskier change than
    /// this comment fix, since other code (e.g. Tab-cycling) may depend on
    /// this exact order.
    const ALL: [View; 24] = [
        View::Command,
        View::Chat,
        View::Missions,
        View::MissionControl,
        View::Schedules,
        View::Notifications,
        View::Memory,
        View::Wiki,
        View::Lattice,
        View::Onboarding,
        View::Agents,
        View::Skills,
        View::Teams,
        View::Documents,
        View::Gallery,
        View::Mcp,
        View::Tools,
        View::Voice,
        View::Settings,
        View::Guide,
        View::Audit,
        View::Sessions,
        View::Loop,
        View::Reminders,
    ];

    /// The URL-hash slug for this view (deep-linking: `…/#memory`).
    fn slug(self) -> &'static str {
        match self {
            View::Command => "command",
            View::Missions => "missions",
            View::MissionControl => "mission-control",
            View::Schedules => "schedules",
            View::Notifications => "notifications",
            View::Chat => "chat",
            View::Memory => "memory",
            View::Wiki => "wiki",
            View::Lattice => "graph",
            View::Skills => "skills",
            View::Settings => "settings",
            View::Agents => "agents",
            View::Teams => "teams",
            View::Documents => "documents",
            View::Gallery => "gallery",
            View::Mcp => "mcp",
            View::Tools => "tools",
            View::Voice => "voice",
            View::Guide => "guide",
            View::Onboarding => "create",
            View::Audit => "audit",
            View::Sessions => "sessions",
            View::Loop => "loop",
            View::Reminders => "reminders",
        }
    }

    /// Parse a slug back to a view (for reading the URL hash on load / back).
    fn from_slug(s: &str) -> Option<View> {
        View::ALL.into_iter().find(|v| v.slug() == s)
    }

    /// Human label for the command palette (matches the sidebar).
    fn label(self) -> &'static str {
        match self {
            View::Command => "Command",
            View::Missions => "Missions",
            View::MissionControl => "Mission Control",
            View::Schedules => "Schedules",
            View::Notifications => "Notifications",
            View::Chat => "Chat",
            View::Memory => "Memory",
            View::Wiki => "Wiki",
            View::Lattice => "Graph",
            View::Skills => "Skills",
            View::Settings => "Settings",
            View::Agents => "Agents",
            View::Teams => "Teams",
            View::Documents => "Documents",
            View::Gallery => "Gallery",
            View::Mcp => "MCP",
            View::Tools => "Tools",
            View::Voice => "Voice",
            View::Guide => "Guide",
            View::Onboarding => "Create",
            View::Audit => "Audit",
            View::Sessions => "Sessions",
            View::Loop => "Loop",
            View::Reminders => "Reminders",
        }
    }
}

/// Memory browser state — read-only snapshots fanned in by `ws_task`.
#[derive(Clone, Default, PartialEq)]
struct MemoryState {
    topics: Vec<String>,
    entries: Vec<MemoryEntrySummary>,
    /// True when a semantic search was transparently served by the keyword path.
    fell_back: bool,
    /// MG — the knowledge-graph nodes (topics + entry counts).
    graph_nodes: Vec<MemoryGraphNode>,
    /// MG — the weighted co-occurrence edges (empty ⇒ a topic cloud).
    graph_edges: Vec<PairScore>,
    /// `false` until the first entries snapshot arrives — distinguishes "still
    /// loading" from "genuinely no memories yet" so the panel shows a skeleton.
    loaded: bool,
    /// POLISH_WAVES.md sub-project 5, item E — Concord-detected memory
    /// contradictions, refreshed on view-open and after every
    /// resolve/dismiss ack.
    conflicts: Vec<aivyx_ipc::conflict::MemoryConflict>,
}

/// Chapter Codex — knowledge-wiki browser state. `pages` is the index
/// (compact rows); `selected` is the open page (full summary + backlinks
/// + source seqs). Read-only snapshots fanned in by `ws_task`.
#[derive(Clone, Default, PartialEq)]
struct WikiState {
    pages: Vec<WikiPageSummary>,
    selected: Option<WikiPage>,
}

/// Chapter Repertoire — Skills library state: the skill inventory (each
/// `SkillView` = a `LearnedSkill` + its WH.2 effectiveness) + the count of
/// pending skill proposals (governed in Agents). Read-only snapshot fanned
/// in by `ws_task`.
#[derive(Clone, Default, PartialEq)]
struct SkillsState {
    skills: Vec<SkillView>,
    pending_proposals: usize,
    loaded: bool,
}

/// Chapter Lattice — typed knowledge-graph state: entity nodes + the
/// directed typed edges, fanned in by `ws_task`.
#[derive(Clone, Default, PartialEq)]
struct GraphKnowledgeState {
    entities: Vec<GraphEntity>,
    edges: Vec<GraphTriple>,
}

/// Chapter Lantern — MCP screen state: each configured server's last-start
/// health + the snapshot's capture time. Read-only snapshot fanned in by
/// `ws_task`. `loaded` flips on the first `GetMcpStatus` response so the
/// panel can tell "still loading" from "genuinely no servers".
#[derive(Clone, Default, PartialEq)]
struct McpState {
    servers: Vec<McpServerStatusView>,
    captured_unix: u64,
    loaded: bool,
    configs: Vec<McpServerConfigView>,
    /// POLISH_WAVES.md sub-project 8 item C — per-server call stats
    /// from `GetMcpServerCallStats`, distinct from `servers` above
    /// (`GetMcpStatus`'s boot-time snapshot). Joined against `servers`
    /// by `server_name == name` when rendering `McpServerCard`.
    call_stats: Vec<McpServerCallStats>,
}

/// Chapter I Phase 188 — the Loop screen's state: the daemon's
/// autonomous-loop run status, fanned in by `read_task` from
/// `QueryResponsePayload::LoopStatus`. `loaded` distinguishes "still
/// loading" from "daemon reports armed=false, nothing running" (same
/// convention `McpState`/`ToolsState` already use).
#[derive(Clone, Default, PartialEq)]
struct LoopUiState {
    state: LoopRunState,
    remaining: usize,
    armed: bool,
    gate_enabled: bool,
    max_run_secs: Option<u64>,
    max_run_tokens: Option<u64>,
    max_run_usd: Option<f64>,
    max_idle_iterations: u32,
    loaded: bool,
    /// The last Start/Stop attempt's outcome, for the inline banner.
    /// `None` before any control action this session.
    last_control_result: Option<(bool, String)>,
}

/// Chapter I Phase 188 — the Reminders screen's state: pending
/// reminders, fanned in by `read_task` from
/// `QueryResponsePayload::Reminders`. Read-only, matching the TUI
/// Dashboard's own posture for the same data (Phase 186) -- no
/// set/cancel UI exists in Studio either.
#[derive(Clone, Default, PartialEq)]
struct RemindersState {
    reminders: Vec<ReminderView>,
    loaded: bool,
}

/// Chapter I Phase 188 — a reminder's due time as a short relative
/// offset. Plain integer-second arithmetic, matching
/// `crates/aivyx-tui/src/render.rs`'s own `format_due_offset` (a
/// separate, non-wasm crate -- nothing is literally shared, this is
/// an independent re-implementation of the same approach for the
/// same reason: no new date/time dependency in this wasm-clean
/// crate).
fn format_due_offset(due_unix: i64, now_unix: i64) -> String {
    let delta = due_unix.saturating_sub(now_unix);
    let abs = delta.unsigned_abs();
    let (value, unit) = if abs < 60 {
        (abs, "s")
    } else if abs < 3_600 {
        (abs / 60, "m")
    } else if abs < 86_400 {
        (abs / 3_600, "h")
    } else {
        (abs / 86_400, "d")
    };
    if delta >= 0 {
        format!("in {value}{unit}")
    } else {
        format!("{value}{unit} overdue")
    }
}

/// Chapter I Phase 188 — pure Start/Stop button-disabled logic for the
/// Loop screen. Returns `(start_disabled, stop_disabled)`. Extracted
/// as its own function so it's testable without rendering anything —
/// this crate's own established convention (see `mcp_health_chip`).
fn loop_button_state(armed: bool, active: bool) -> (bool, bool) {
    let start_disabled = !armed || active;
    let stop_disabled = !active;
    (start_disabled, stop_disabled)
}

/// Chapter Almanac — Tools screen state: the daemon's registered tool
/// catalog (name, description, capability base, minimum trust tier).
/// Read-only snapshot fanned in by `ws_task`. `loaded` distinguishes
/// "still loading" from "the daemon reports zero tools" (should never
/// happen, but the same defensive convention as `SkillsState`/`McpState`).
#[derive(Clone, Default, PartialEq)]
struct ToolsState {
    tools: Vec<ToolCatalogEntry>,
    loaded: bool,
}

/// Studio Gallery state — recent images generated via the `comfyui`
/// `[[mcp_server]]`, read from ComfyUI's own `/history` API (not the MCP
/// tool surface). Read-only snapshot fanned in by `ws_task`. `available`
/// is `false` when no `comfyui` server is configured at all (distinct from
/// "configured but zero generations yet").
#[derive(Clone, Default, PartialEq)]
struct GalleryState {
    available: bool,
    images: Vec<GalleryImage>,
    loaded: bool,
}

/// Settings screen state — the on-disk config snapshot + the last write outcome.
/// Chapter U: the first **write** surface, so it also carries a notice banner
/// and the "restart to apply" flag (config is load-time).
#[derive(Clone, Default, PartialEq)]
struct SettingsState {
    snapshot: Option<SettingsSnapshot>,
    /// Last write outcome: `(ok, message)`. `None` until the first write.
    notice: Option<(bool, String)>,
    /// True after a successful write — a write updates aivyx-pa.toml but the
    /// running daemon won't pick it up until it restarts.
    restart_required: bool,
    /// POLISH_WAVES.md sub-project 7 plan 3 — `[memory] profile`.
    memory_profile: Option<MemoryProfileConfigView>,
    /// `[embedding]`'s primary fields.
    embedding: Option<EmbeddingConfigView>,
    /// `[proactive]`'s primary fields.
    proactive: Option<ProactiveConfigView>,
}

/// Chapter Mission Control — the abort/pause/resume control surface's own
/// last-outcome banner. A dedicated struct (not a bare `Signal<Option<
/// (bool, String)>>`) matches every other panel's own state-struct-plus-
/// notice convention (`SettingsState`, `TeamsState`, ...) and can't
/// collide with any other context by type the way a bare `Signal<Option<
/// (bool, String)>>` could.
#[derive(Clone, Default, PartialEq)]
struct MissionControlUi {
    /// Last abort/pause/resume outcome: `(ok, message)`. `None` until the
    /// first control click fails or succeeds.
    notice: Option<(bool, String)>,
}

/// Teams screen state — Chapters Y (read) + Roster (RO.3, edit). The active
/// roster (re-read from disk after a save) plus the last write outcome + the
/// load-time restart flag. The editor seeds a local draft from `roster`.
#[derive(Clone, Default, PartialEq)]
struct TeamsState {
    /// The daemon's active team config. `None` until the first load.
    roster: Option<TeamConfig>,
    /// Last save outcome: `(ok, message)`. `None` until the first save.
    notice: Option<(bool, String)>,
    /// True after a successful save — the team file is written but the running
    /// daemon won't adopt it until it restarts (the team service is boot-built).
    restart_required: bool,
    /// Chapter Nonagon Templates — true while a DraftTeamTemplate round
    /// trip is in flight (drives the button's spinner/disabled state).
    drafting: bool,
    /// Chapter Nonagon Templates — the last draft outcome: `(ok, message)`.
    /// Separate from `notice` (a save outcome) so the two don't clobber
    /// each other on screen.
    draft_notice: Option<(bool, String)>,
    /// Chapter Nonagon Templates — the drafted roster, handed off to the
    /// panel's local edit-draft signal via `draft_resp` (the same
    /// tick-and-consume pattern the Onboarding seed/profile drafts use).
    drafted_roster: Option<TeamConfig>,
    /// Bumped once per `TeamTemplateDrafted` response so the panel's
    /// effect fires exactly once per draft, even if the config is
    /// identical to a previous one.
    draft_resp: u64,
}

/// Agents screen state — Chapter V. The operator-declared Profile half (V.3)
/// plus the self-learned Persona-governance half (V.4): the folded effective
/// persona, the pending proposals the operator gates, and the approved delta
/// chain the operator can revert.
#[derive(Clone, Default, PartialEq)]
struct AgentsState {
    profile: Option<ProfileSummary>,
    /// The folded effective persona (read-only viewer). `None` until first load.
    persona: Option<EffectivePersonaSummary>,
    /// Pending persona proposals awaiting the operator's gate.
    proposals: Vec<PersonaProposalSummary>,
    /// The approved persona delta chain (newest first), each revertable.
    deltas: Vec<PersonaDeltaSummary>,
    /// Last write/action outcome: `(ok, message)`. `None` until the first one.
    notice: Option<(bool, String)>,
    /// True after a successful **Profile** write — `aivyx-pa.toml` is updated but
    /// the running daemon won't pick it up until restart (Profile is load-time).
    /// Persona actions are live (the daemon recomputes runtime state), so they
    /// never set this.
    restart_required: bool,
    /// Bumped on each persona resolve/revert ack so the panel re-queries the
    /// proposals + deltas + effective persona (the live-refresh signal).
    refresh_tick: u64,
    /// X.3 — the latest LLM-drafted seed (the onboarding card fills its form
    /// from this); `None` until a draft arrives or after a failed draft.
    seed_draft: Option<PersonaSeedWire>,
    /// X.3 — bumped on every `DraftPersonaSeed` response (success or failure) so
    /// the onboarding card can clear its "Drafting…" state and re-seed its form.
    seed_draft_resp: u64,
    /// GE.3 — the latest LLM-drafted **Profile** (the onboarding flow's step 1
    /// fills its six fields from this); `None` until a draft arrives or fails.
    profile_draft: Option<ProfileDraftWire>,
    /// GE.3 — bumped on every `DraftProfile` response (success or failure) so the
    /// onboarding flow can clear its "Drafting…" state and re-fill its form.
    profile_draft_resp: u64,
}

/// Voice config screen state — Chapter Voice. The on-disk `[voice]` snapshot
/// (+ readiness) plus the last write outcome + the load-time restart flag.
#[derive(Clone, Default, PartialEq)]
struct VoiceState {
    snapshot: Option<VoiceSettingsSnapshot>,
    notice: Option<(bool, String)>,
    restart_required: bool,
}

/// Documents browser state — Chapter Z. The active root + path + the current
/// directory listing, the open file (if any), and the last error notice. All
/// read-only; the data is fanned in by `ws_task`.
#[derive(Clone, Default, PartialEq)]
struct DocumentsState {
    /// `"workspace"` | `"fs"` — empty until the first load (then `"workspace"`).
    root: String,
    /// Current directory, relative to `root`.
    path: String,
    /// The current directory's listing (dirs first).
    entries: Vec<DocEntry>,
    /// The open file in the viewer, or `None` when showing the listing.
    file: Option<DocFile>,
    /// Last outcome `(ok, message)` (a denied path / read failure / DW write).
    notice: Option<(bool, String)>,
    /// `false` until the first directory listing arrives (the `root` field is
    /// set client-side immediately, so it can't signal load) — drives the
    /// listing skeleton.
    loaded: bool,
}

/// Command Center dashboard state — read-only snapshots fanned in by `ws_task`.
#[derive(Clone, Default, PartialEq)]
struct Dashboard {
    audit_entries: Vec<AuditEntrySummary>,
    audit_total: u64,
    chain_ok: Option<bool>,
    assistant_name: Option<String>,
    /// The running agent's vitals (model / provider / context / autonomy /
    /// access) from `GetSettings` — drives the agent-vitals rail.
    settings: Option<SettingsSnapshot>,
    /// The agent's scheduled background routines (`GetSchedules`) — drives the
    /// Routines panel + stat card, the "live agent working on its own" signal.
    schedules: Vec<ScheduleView>,
    /// POLISH_WAVES.md sub-project 7 plan 3 — the editable
    /// `[[reflection_schedule]]` list, distinct from `schedules` above
    /// (regular `[[schedule]]` entries). Populated by
    /// `GetReflectionScheduleConfigs`/`ReflectionScheduleConfigApplied`.
    reflection_schedules: Vec<ReflectionScheduleConfigView>,
    /// `/classic` retirement — the self-learning digest (VITRINE.md's
    /// Learning pane, folded in here rather than a dedicated screen).
    /// `None` until the first response arrives.
    learning: Option<aivyx_ipc::insights::LearningDigest>,
    /// `false` until the first dashboard snapshot (the audit-entries response)
    /// arrives. Distinguishes "not loaded yet" from "loaded and genuinely
    /// empty" so the Command Center shows a skeleton instead of flashing zeros.
    loaded: bool,
}

/// Chapter Herald — Notifications screen + header-bell state. Targets
/// and history are both polled every 5 s (the same cadence Schedules
/// uses); `last_seen_seq` is purely client-side (never persisted) —
/// visiting the Notifications screen clears the header badge by
/// bumping it to the newest seq seen.
#[derive(Clone, Default, PartialEq)]
struct NotificationsState {
    targets: Vec<NotifyTargetView>,
    history: Vec<NotificationHistoryEntry>,
    total_len: u64,
    last_seen_seq: u64,
    /// POLISH_WAVES.md sub-project 7 plan 2 — the editable notify-target
    /// list (distinct from `targets`, the read-only status view above),
    /// mirroring how `McpState` carries both `servers` and `configs`.
    configs: Vec<NotifyTargetConfigView>,
    /// POLISH_WAVES.md sub-project 7 plan 2 (Task 7) — the 4 channel-
    /// adapter singleton configs feeding the "Channel adapters" section.
    /// Each carries only `RedactedSecret`s for its token/password fields —
    /// never a real value — so this state is safe to hold in the client.
    email: Option<EmailConfigView>,
    telegram: Option<TelegramConfigView>,
    discord: Option<DiscordConfigView>,
    slack: Option<SlackConfigView>,
}

/// `/classic` retirement — the dedicated Audit screen's state (distinct
/// from `Dashboard.audit_entries`, the Command Center's own short,
/// auto-following tail — this one is explicitly paginated by the
/// operator). Chain-verify reuses `Dashboard.chain_ok` directly rather
/// than duplicating it; see the AuditPanel component below.
#[derive(Clone, Default, PartialEq)]
struct AuditState {
    entries: Vec<AuditEntrySummary>,
    total_len: u64,
    /// The `from_seq` most recently *requested* (set at the request side —
    /// mount, "Older", "Newer" — never inferred from a response), so the
    /// UI's own notion of "what window am I looking at" can never drift
    /// from what was actually asked for.
    from_seq: u64,
    /// `true` from the moment a request is sent whose `from_seq` was
    /// computed from a stale/unknown `total_len` — i.e. `AuditPanel`'s own
    /// mount-time guess, which cannot know the chain's true length before
    /// its first response arrives. Cleared back to `false` by the very next
    /// "audit-page" response, whether or not that response turns out to
    /// need a correction. A deliberate operator action ("Older"/"Newer")
    /// clears it to `false` itself at the moment it fires, so a guess
    /// response that happens to land late can never override a deliberate
    /// pagination click. Set back to `true` by *every* fresh `AuditPanel`
    /// mount — this is what makes the self-correction available once per
    /// mount, for the life of the session, rather than once ever.
    pending_guess: bool,
    /// Set by the "audit-page" response handler when it finds that a
    /// `pending_guess` request guessed wrong (now that the response reveals
    /// the real `total_len`): holds the corrected `from_seq` that needs to
    /// be (re-)requested. Consumed (set back to `None`) by `App`'s effect,
    /// which is the only place in scope with access to the outbound `ws`
    /// sender needed to actually fire that follow-up query.
    pending_correction: Option<u64>,
}

/// `/classic` retirement — the Sessions screen's state. Loaded fresh
/// each time the view opens (sessions are short-lived and this isn't
/// a background-poll surface like Missions/Notifications).
#[derive(Clone, Default, PartialEq)]
struct SessionsState {
    sessions: Vec<SessionSummary>,
}

/// Chapter Chime — Schedules screen UI state (the list itself lives in
/// `Dashboard::schedules`, already polled every 5 s).
#[derive(Clone, Default, PartialEq)]
struct SchedulesUi {
    /// `(ok, text)` outcome of the last mutation ack.
    notice: Option<(bool, String)>,
    /// Two-step delete: the schedule_id awaiting its confirm click.
    confirm_delete: Option<String>,
}

/// Chapter Tutor — Skills screen "Teach a skill" form UI state. The form's
/// own text fields (name/trigger/procedure) and open/closed toggle are
/// local `use_signal`s inside `SkillsPanel` itself, not here — this only
/// holds the cross-cutting mutation-ack outcome, exactly like
/// `SchedulesUi.notice` above (the ack arrives in the shared `read_task`
/// response dispatch, which doesn't have direct access to `SkillsPanel`'s
/// own local component state).
#[derive(Clone, Default, PartialEq)]
struct SkillsUi {
    /// `(ok, text)` outcome of the last `AuthorSkill` (Teach) attempt.
    notice: Option<(bool, String)>,
}

/// POLISH_WAVES.md sub-project 5, item E — Memory screen UI state
/// (resolve/dismiss action feedback), mirroring `SkillsUi`/`SchedulesUi`'s
/// own minimal shape exactly.
#[derive(Clone, Default, PartialEq)]
struct MemoryUi {
    notice: Option<(bool, String)>,
}

/// POLISH_WAVES.md sub-project 7, item B — MCP config-write UI state
/// (save/delete outcome feedback), mirroring `MemoryUi`'s own minimal shape.
#[derive(Clone, Default, PartialEq)]
struct McpConfigUi {
    notice: Option<(bool, String)>,
    /// Chapter Lantern follow-up — the most recent
    /// `TestMcpServerConnection` result (`ok`, display text), rendered
    /// inline in `McpServerForm`.
    test_result: Option<(bool, String)>,
}

/// POLISH_WAVES.md sub-project 7 plan 2 — notify-target/channel-adapter
/// config-write UI state, mirroring `McpConfigUi`'s own minimal shape.
#[derive(Clone, Default, PartialEq)]
struct NotifyConfigUi {
    notice: Option<(bool, String)>,
}

/// POLISH_WAVES.md sub-project 6, item B — tracks the most recent
/// `DaemonEnvelope::ServerInfo.boot_id` Studio has seen, and whether a
/// *different* one has arrived since (meaning the daemon this tab now
/// talks to isn't the one it started against).
#[derive(Clone, Default, PartialEq)]
struct ServerInfoUi {
    boot_id: Option<String>,
    update_available: bool,
}

/// The state transition `ServerInfoUi` takes on receiving a `boot_id`:
/// the first one seen this session is just recorded (no banner); any
/// later one that *differs* latches `update_available` on. It stays on
/// once set — a flapping reconnect landing back on the same new boot_id
/// doesn't clear it, and a manual dismiss (not modeled here; see the
/// render step) is the only way off, so a real update can't be hidden by
/// a lucky match.
fn apply_server_info(current: &ServerInfoUi, boot_id: String) -> ServerInfoUi {
    match &current.boot_id {
        None => ServerInfoUi { boot_id: Some(boot_id), update_available: false },
        Some(seen) if *seen == boot_id => current.clone(),
        Some(_) => ServerInfoUi { boot_id: Some(boot_id), update_available: true },
    }
}

#[cfg(test)]
mod server_info_tests {
    use super::*;

    #[test]
    fn first_boot_id_is_recorded_without_a_banner() {
        let next = apply_server_info(&ServerInfoUi::default(), "a".to_string());
        assert_eq!(next.boot_id, Some("a".to_string()));
        assert!(!next.update_available);
    }

    #[test]
    fn same_boot_id_again_does_not_trigger_the_banner() {
        let seen = ServerInfoUi { boot_id: Some("a".to_string()), update_available: false };
        let next = apply_server_info(&seen, "a".to_string());
        assert!(!next.update_available);
    }

    #[test]
    fn a_different_boot_id_triggers_the_banner() {
        let seen = ServerInfoUi { boot_id: Some("a".to_string()), update_available: false };
        let next = apply_server_info(&seen, "b".to_string());
        assert_eq!(next.boot_id, Some("b".to_string()));
        assert!(next.update_available);
    }

    #[test]
    fn banner_stays_on_across_a_further_reconnect_to_the_same_new_id() {
        let updated = ServerInfoUi { boot_id: Some("b".to_string()), update_available: true };
        let next = apply_server_info(&updated, "b".to_string());
        assert!(next.update_available);
    }
}

/// One rendered chat transcript line.
#[derive(Clone, PartialEq)]
struct ChatLine {
    role: Role,
    text: String,
}

#[derive(Clone, Copy, PartialEq)]
enum Role {
    Operator,
    Assistant,
    System,
    Error,
}

impl ChatLine {
    fn operator(text: String) -> Self {
        Self { role: Role::Operator, text }
    }
    fn assistant(text: String) -> Self {
        Self { role: Role::Assistant, text }
    }
    fn system(text: String) -> Self {
        Self { role: Role::System, text }
    }
    fn error(text: String) -> Self {
        Self { role: Role::Error, text }
    }
    fn class(&self) -> &'static str {
        match self.role {
            Role::Operator => "line op",
            Role::Assistant => "line asst",
            Role::System => "line sys",
            Role::Error => "line err",
        }
    }
}

/// A pending single-agent approval gate.
#[derive(Clone, PartialEq)]
struct GateInfo {
    mission_id: String,
    gate_id: String,
    reason: String,
}

fn main() {
    console_error_panic_hook::set_once();
    dioxus::launch(App);
}

/// Set `data-theme` on `<html>` so the `[data-theme="light"]` token overrides
/// cascade to `:root` + `body` (dark is the default — no attribute).
fn apply_theme(light: bool) {
    if let Some(el) = web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.document_element())
    {
        let _ = el.set_attribute("data-theme", if light { "light" } else { "dark" });
    }
}

/// `@font-face` rules built from the hashed asset paths (so the fonts always
/// resolve to the bundled, offline woff2 — not a CDN).
fn font_faces() -> String {
    format!(
        "@font-face{{font-family:'Fraunces';src:url('{FONT_DISPLAY}') format('woff2');font-weight:100 900;font-display:swap;}}\
         @font-face{{font-family:'IBM Plex Sans';src:url('{FONT_BODY}') format('woff2');font-weight:100 700;font-display:swap;}}\
         @font-face{{font-family:'IBM Plex Mono';src:url('{FONT_MONO}') format('woff2');font-weight:100 700;font-display:swap;}}"
    )
}

/// The view named by the current URL hash (`…/#memory`), if it's a known slug.
fn hash_view() -> Option<View> {
    let h = web_sys::window()?.location().hash().ok()?;
    View::from_slug(h.trim_start_matches('#'))
}

/// The view to start on: the URL hash if it names a real screen, else Command.
/// (First-run onboarding still takes over via the no-profile redirect.)
fn initial_view() -> View {
    hash_view().unwrap_or(View::Command)
}

#[component]
fn App() -> Element {
    // Deep-linking: the active view is mirrored in the URL hash, so screens are
    // bookmarkable/shareable and survive a reload, and back/forward navigate.
    let view = use_signal(initial_view);
    // view -> URL hash.
    use_effect(move || {
        let slug = view().slug();
        if let Some(loc) = web_sys::window().map(|w| w.location()) {
            if loc.hash().unwrap_or_default().trim_start_matches('#') != slug {
                let _ = loc.set_hash(slug);
            }
        }
    });
    // URL hash -> view (back/forward, manual edits). Registered once.
    use_hook(|| {
        use wasm_bindgen::JsCast;
        let mut view = view;
        let cb = wasm_bindgen::closure::Closure::<dyn FnMut()>::new(move || {
            if let Some(v) = hash_view() {
                if view.peek().slug() != v.slug() {
                    view.set(v);
                }
            }
        });
        if let Some(w) = web_sys::window() {
            let _ = w
                .add_event_listener_with_callback("hashchange", cb.as_ref().unchecked_ref());
        }
        cb.forget();
    });

    // Command palette (Ctrl/Cmd-K) — a global keydown listener toggles it.
    let palette_open = use_signal(|| false);
    use_hook(|| {
        use wasm_bindgen::JsCast;
        let mut palette_open = palette_open;
        let cb = wasm_bindgen::closure::Closure::<dyn FnMut(web_sys::Event)>::new(
            move |e: web_sys::Event| {
                let Some(ke) = e.dyn_ref::<web_sys::KeyboardEvent>() else {
                    return;
                };
                if (ke.ctrl_key() || ke.meta_key()) && ke.key() == "k" {
                    e.prevent_default();
                    let now = *palette_open.peek();
                    palette_open.set(!now);
                } else if ke.key() == "Escape" && *palette_open.peek() {
                    palette_open.set(false);
                }
            },
        );
        if let Some(w) = web_sys::window() {
            let _ = w.add_event_listener_with_callback("keydown", cb.as_ref().unchecked_ref());
        }
        cb.forget();
    });

    let connected = use_signal(|| false);
    let light = use_signal(|| false);
    // Mobile drawer state: below the shell breakpoint the sidebar is off-canvas
    // and this toggles it. Ignored on desktop (the sidebar is always in-grid).
    let mut nav_open = use_signal(|| false);
    // The Guide's current page — App-owned so the topbar "?" can deep-link it.
    let guide_page = use_signal(|| 0usize);
    let missions = use_signal(Vec::<TeamMissionView>::new);
    // Chapter Mission Control — see `apply_running_overlay`'s own doc
    // comment: the poll's `to_view()` projection can never produce
    // `Running` on its own, so this overlay is what keeps a live
    // `TeamMissionUpdated`'s Running marker from being erased by the next
    // poll tick.
    let running_overlay = use_signal(HashMap::<String, HashSet<usize>>::new);
    let dashboard = use_signal(Dashboard::default);
    let memory = use_signal(MemoryState::default);
    let wiki = use_signal(WikiState::default);
    let lattice = use_signal(GraphKnowledgeState::default);
    let settings = use_signal(SettingsState::default);
    let agents = use_signal(AgentsState::default);
    let teams = use_signal(TeamsState::default);
    let documents = use_signal(DocumentsState::default);
    let voice = use_signal(VoiceState::default);
    let skills = use_signal(SkillsState::default);
    let skills_ui = use_signal(SkillsUi::default);
    let memory_ui = use_signal(MemoryUi::default);
    let server_info = use_signal(ServerInfoUi::default);
    let mcp = use_signal(McpState::default);
    let mcp_config_ui = use_signal(McpConfigUi::default);
    let tools = use_signal(ToolsState::default);
    let gallery = use_signal(GalleryState::default);
    let schedules_ui = use_signal(SchedulesUi::default);
    let notifications = use_signal(NotificationsState::default);
    let loop_ui = use_signal(LoopUiState::default);
    let reminders_ui = use_signal(RemindersState::default);
    let notify_config_ui = use_signal(NotifyConfigUi::default);
    let mut audit_page = use_signal(AuditState::default);
    let sessions_page = use_signal(SessionsState::default);
    // Chat state, shared with the read task + the Chat view (via context).
    let session = use_signal(|| None::<String>);
    let transcript = use_signal(Vec::<ChatLine>::new);
    let streaming = use_signal(String::new);
    let gate = use_signal(|| None::<GateInfo>);
    // Chapter Mission Control — pure UI-navigation state (which mission's
    // graph is currently open); never written by `ws_task`'s coroutine, so
    // deliberately not threaded into its parameter list below. Passed to
    // `MissionControlPanel` as an explicit prop (see its call site below),
    // NOT via `use_context_provider` -- `Signal<Option<String>>` is also
    // `session`'s own context type (the Chat state, above), and Dioxus
    // contexts are keyed purely by type: a second `use_context_provider`
    // for the same type in the same scope silently overwrites the first.
    // Providing this one shadowed `session` and broke `ChatPanel`'s own
    // `use_context::<Signal<Option<String>>>()` read. Do not re-add the
    // provider call for this signal.
    let selected_mission = use_signal(|| None::<String>);
    // Chapter Mission Control — the abort/pause/resume control surface's
    // last-outcome banner. Safe to provide via context: `MissionControlUi`
    // is a dedicated struct type (see its definition below), so it can't
    // collide with any other context the way a bare `Signal<Option<(bool,
    // String)>>` could.
    let mission_ui = use_signal(MissionControlUi::default);

    let ws: Sender = use_coroutine(move |rx| {
        ws_task(
            rx, missions, running_overlay, dashboard, memory, memory_ui, wiki, lattice, settings, agents,
            teams, documents, voice, skills, skills_ui, mcp, mcp_config_ui, tools, gallery, schedules_ui,
            notifications, loop_ui, reminders_ui, notify_config_ui, audit_page, sessions_page, connected, session, transcript, streaming,
            gate, mission_ui, server_info,
        )
    });
    use_context_provider(|| ws);
    // Connection state, so action surfaces (mission bar, roster save) can
    // gate on a live socket instead of sending into a zombie page.
    use_context_provider(|| connected);
    use_context_provider(|| memory);
    use_context_provider(|| memory_ui);
    use_context_provider(|| wiki);
    use_context_provider(|| lattice);
    use_context_provider(|| settings);
    use_context_provider(|| agents);
    use_context_provider(|| teams);
    use_context_provider(|| documents);
    use_context_provider(|| voice);
    use_context_provider(|| skills);
    use_context_provider(|| skills_ui);
    use_context_provider(|| mcp);
    use_context_provider(|| mcp_config_ui);
    use_context_provider(|| tools);
    use_context_provider(|| gallery);
    use_context_provider(|| schedules_ui);
    use_context_provider(|| notifications);
    use_context_provider(|| loop_ui);
    use_context_provider(|| reminders_ui);
    use_context_provider(|| notify_config_ui);
    use_context_provider(|| audit_page);
    use_context_provider(|| sessions_page);
    // Chapter Chime — the Schedules screen reads the routine list from
    // the dashboard snapshot (already polled every 5 s). Dashboard had
    // only ever been passed as a prop; the missing provider panicked
    // the whole app on first navigate (operator-found, 2026-07-06).
    use_context_provider(|| dashboard);
    // Chapter Repertoire — the Skills screen's "review in Agents" pointer
    // switches the active view.
    use_context_provider(|| view);
    use_context_provider(|| missions);
    use_context_provider(|| running_overlay);
    use_context_provider(|| session);
    use_context_provider(|| transcript);
    use_context_provider(|| streaming);
    use_context_provider(|| gate);
    use_context_provider(|| mission_ui);

    // Reflect the theme signal onto `<html data-theme>`.
    use_effect(move || apply_theme(light()));

    // First-run routing: when the profile snapshot says pre-genesis and
    // the operator hasn't navigated anywhere yet, land on the Create
    // wizard (its nav entry only exists pre-genesis, so the wizard must
    // present itself). One-shot — never hijacks later navigation.
    let mut genesis_routed = use_signal(|| false);
    use_effect(move || {
        let pre_genesis = agents()
            .profile
            .as_ref()
            .is_some_and(|p| p.assistant_name_source != "toml");
        if pre_genesis && !genesis_routed() && view() == View::Command {
            genesis_routed.set(true);
            let mut v = view;
            v.set(View::Onboarding);
        }
    });

    // /classic retirement — the dedicated Audit screen's own mount query
    // (in `AuditPanel`) can't know the chain's true `total_len` before its
    // first response arrives, so on a chain longer than one page it
    // necessarily asks for the OLDEST page first (from_seq=0), not the
    // newest — unlike the Command Center's own `mc-audit` tail below,
    // which self-corrects every poll tick, `AuditPanel`'s query is a
    // one-shot `use_future` with no ongoing poll (by design — this screen
    // doesn't need the Command Center's continuous refresh).
    //
    // The actual "was the guess wrong, and by how much" check lives in the
    // "audit-page" response handler below (it alone knows, atomically, both
    // the just-requested `from_seq` and the freshly-arrived `total_len`,
    // and it only ever runs when a response has genuinely arrived — so it
    // can't fire on stale data the way a signal-watching effect could).
    // This effect's only job is to actually *send* the corrected query once
    // the handler asks for one, because `read_task` (where that handler
    // lives) is a plain async fn with no `ws` sender of its own — see
    // `AuditState.pending_correction`'s doc comment. Because the handler
    // re-derives eligibility fresh from `AuditState.pending_guess` on every
    // single "audit-page" response — and `AuditPanel`'s mount `use_future`
    // sets `pending_guess = true` again on every mount — this corrects
    // itself once per *mount*, not once ever for the whole session.
    use_effect(move || {
        let a = audit_page();
        if let Some(from_seq) = a.pending_correction {
            {
                let mut aw = audit_page.write();
                aw.from_seq = from_seq;
                aw.pending_correction = None;
            }
            ws.send(FrontendMessage::Query {
                id: "audit-page".to_string(),
                payload: QueryPayload::ListAuditEntries { from_seq, limit: AUDIT_PAGE_SIZE },
            });
        }
    });

    // Live poll: mission feed + the newest audit tail, every interval. The audit
    // window self-corrects to the newest entries once `audit_total` is known.
    use_future(move || async move {
        loop {
            ws.send(FrontendMessage::Query {
                id: "mc-poll".to_string(),
                payload: QueryPayload::TeamMissionList,
            });
            let from_seq = dashboard().audit_total.saturating_sub(AUDIT_FEED_N as u64);
            ws.send(FrontendMessage::Query {
                id: "mc-audit".to_string(),
                payload: QueryPayload::ListAuditEntries { from_seq, limit: AUDIT_FEED_N },
            });
            // Vitrine final sweep — routines have live-changing fields
            // (last-fired / next-fire move as crons tick), so the
            // Routines panel polls with the missions + audit feed
            // instead of staying a page-load snapshot. The schedule
            // list is tiny; the query is cheap.
            ws.send(FrontendMessage::Query {
                id: "mc-schedules".to_string(),
                payload: QueryPayload::GetSchedules,
            });
            // Chapter Herald — the notify-target list rarely changes (TOML-
            // managed) but is cheap; history grows, so the header bell's
            // unseen count stays live the same way the routines panel does.
            ws.send(FrontendMessage::Query {
                id: "mc-notify-targets".to_string(),
                payload: QueryPayload::GetNotifyTargets,
            });
            let notify_from_seq = notifications()
                .total_len
                .saturating_sub(NOTIFICATION_FEED_N as u64);
            ws.send(FrontendMessage::Query {
                id: "mc-notify-history".to_string(),
                payload: QueryPayload::ListNotificationHistory {
                    from_seq: notify_from_seq,
                    limit: NOTIFICATION_FEED_N,
                    target_filter: None,
                },
            });
            TimeoutFuture::new(POLL_INTERVAL_MS).await;
        }
    });

    // One-shot: the agent profile + a chain integrity check for the dashboard.
    use_future(move || async move {
        ws.send(FrontendMessage::Query {
            id: "mc-profile".to_string(),
            // Dashboard shows the *running* agent's name — the active snapshot.
            payload: QueryPayload::GetProfile { from_disk: false },
        });
        ws.send(FrontendMessage::Query {
            id: "mc-verify".to_string(),
            payload: QueryPayload::VerifyAuditChain,
        });
        // Agent vitals (model / provider / context / autonomy / access) +
        // the scheduled background routines — the "live, working agent" panels.
        ws.send(FrontendMessage::Query {
            id: "mc-settings".to_string(),
            payload: QueryPayload::GetSettings,
        });
        ws.send(FrontendMessage::Query {
            id: "mc-schedules".to_string(),
            payload: QueryPayload::GetSchedules,
        });
        // `/classic` retirement — the self-learning digest, folded into the
        // Command Center rail rather than a dedicated screen.
        ws.send(FrontendMessage::Query {
            id: "mc-learning".to_string(),
            payload: QueryPayload::GetLearningInsights { window_secs: None },
        });
    });

    let title = match view() {
        View::Command => "Command Center",
        View::Missions => "Mission Orchestration",
        View::MissionControl => "Mission Control",
        View::Schedules => "Schedules",
        View::Notifications => "Notifications",
        View::Chat => "Terminal",
        View::Memory => "Memory",
        View::Wiki => "Knowledge Wiki",
        View::Lattice => "Knowledge Graph",
        View::Skills => "Skills",
        View::Settings => "Settings",
        View::Agents => "Agents",
        View::Teams => "Teams",
        View::Documents => "Documents",
        View::Gallery => "Gallery",
        View::Mcp => "MCP Servers",
        View::Tools => "Tools",
        View::Voice => "Voice",
        View::Guide => "Guide",
        View::Onboarding => "Create your agent",
        View::Audit => "Audit",
        View::Sessions => "Sessions",
        View::Loop => "Autonomous Loop",
        View::Reminders => "Reminders",
    };

    rsx! {
        document::Title { "Aivyx PA Studio" }
        document::Link { rel: "icon", href: FAVICON }
        document::Stylesheet { href: STITCH_CSS }
        style { {font_faces()} }
        div { class: if nav_open() { "app nav-open" } else { "app" },
            // Keyboard a11y: first focusable element jumps past the nav.
            a { class: "skip-link", href: "#main-content", "Skip to content" }
            Sidebar { view, nav_open }
            // Mobile-only scrim behind the open drawer; tap to dismiss.
            div { class: "nav-backdrop", onclick: move |_| nav_open.set(false) }
            div { class: "main",
                if !connected() {
                    div {
                        style: "position:sticky;top:0;z-index:1000;background:var(--danger, #b91c1c);color:#fff;text-align:center;padding:6px 12px;font-size:13px;letter-spacing:0.02em;",
                        role: "alert",
                        "Connection to the agent lost — reconnecting…"
                    }
                }
                if server_info().update_available {
                    div { class: "notice info reload-hint", role: "status",
                        // POLISH_WAVES.md sub-project 6 Minor follow-up:
                        // `boot_id` (a fresh random value minted once per
                        // daemon process, compared across reconnects — see
                        // `apply_server_info`'s own doc comment above)
                        // detects "the daemon restarted," not "a new bundle
                        // shipped" — a plain `aivyx-pa daemon stop && aivyx-pa
                        // daemon run` with no code change still changes
                        // `boot_id`. The copy below says what's actually
                        // known, not what's merely likely.
                        "The daemon has restarted since this page loaded — reload to make sure \
                         you're running its current version. "
                        button {
                            class: "btn btn-primary btn-xs",
                            onclick: move |_| {
                                if let Some(w) = web_sys::window() {
                                    let _ = w.location().reload();
                                }
                            },
                            "Reload"
                        }
                        button {
                            class: "btn btn-glass btn-xs",
                            onclick: move |_| {
                                // `server_info` is passed PLAIN (non-`mut`) into `App`'s
                                // `use_coroutine` call per this file's established
                                // convention (see `memory_ui`); shadow it with a local
                                // `mut` binding here to call `.set()`, mirroring the
                                // `view`/`palette_open` pattern above.
                                let mut server_info = server_info;
                                let mut cur = server_info();
                                cur.update_available = false;
                                server_info.set(cur);
                            },
                            "Dismiss"
                        }
                    }
                }
                Topbar { title, light, nav_open, view, guide_page }
                main { class: "view fade-in", id: "main-content", tabindex: "-1",
                    match view() {
                        View::Command => rsx! {
                            CommandPanel { missions: missions(), dashboard: dashboard(), connected: connected() }
                        },
                        View::Missions => rsx! { MissionsPanel { missions: missions() } },
                        View::MissionControl => rsx! {
                            MissionControlPanel { missions: missions(), selected_mission }
                        },
                        View::Schedules => rsx! { SchedulesPanel {} },
                        View::Notifications => rsx! { NotificationsPanel {} },
                        View::Chat => rsx! { ChatPanel {} },
                        View::Memory => rsx! { MemoryPanel {} },
                        View::Wiki => rsx! { WikiPanel {} },
                        View::Lattice => rsx! { LatticePanel {} },
                        View::Skills => rsx! { SkillsPanel {} },
                        View::Loop => rsx! { LoopPanel {} },
                        View::Reminders => rsx! { RemindersPanel {} },
                        View::Settings => rsx! { SettingsPanel {} },
                        View::Agents => rsx! { AgentsPanel {} },
                        View::Teams => rsx! { TeamsPanel {} },
                        View::Documents => rsx! { DocumentsPanel {} },
                        View::Gallery => rsx! { GalleryPanel {} },
                        View::Mcp => rsx! { McpPanel {} },
                        View::Tools => rsx! { ToolsPanel {} },
                        View::Voice => rsx! { VoicePanel {} },
                        View::Guide => rsx! { GuidePanel { page: guide_page } },
                        View::Onboarding => rsx! { OnboardingPanel { view } },
                        View::Audit => rsx! { AuditPanel {} },
                        View::Sessions => rsx! { SessionsPanel {} },
                    }
                }
            }
            StatusBar { connected: connected(), agent_name: dashboard().assistant_name.clone().unwrap_or_default() }
            if palette_open() {
                CommandPalette { view, open: palette_open }
            }
        }
    }
}

/// Ctrl/Cmd-K command palette — fuzzy-jump to any screen. Type to filter,
/// arrows to move, Enter to go, Esc/backdrop to dismiss.
#[component]
fn CommandPalette(view: Signal<View>, open: Signal<bool>) -> Element {
    let mut query = use_signal(String::new);
    let mut selected = use_signal(|| 0usize);
    // Post-genesis the Create wizard leaves the jump list too (it
    // mirrors the sidebar — operator nav-cleanup ask, 2026-07-06).
    let agents = use_context::<Signal<AgentsState>>();
    let genesis_done = agents()
        .profile
        .as_ref()
        .map(|p| p.assistant_name_source == "toml")
        .unwrap_or(true);

    // The filtered screen list (case-insensitive label contains).
    let q = query().to_lowercase();
    let results: Vec<View> = View::ALL
        .into_iter()
        .filter(|v| !(genesis_done && *v == View::Onboarding))
        .filter(|v| q.is_empty() || v.label().to_lowercase().contains(&q))
        .collect();
    let sel = selected().min(results.len().saturating_sub(1));
    let kb = results.clone(); // snapshot moved into the keydown handler

    rsx! {
        div { class: "palette-backdrop", onclick: move |_| open.set(false),
            div { class: "palette", onclick: move |e| e.stop_propagation(),
                input {
                    class: "palette-input",
                    r#type: "text",
                    autofocus: true,
                    "aria-label": "Jump to a screen",
                    placeholder: "Jump to a screen…",
                    value: "{query}",
                    oninput: move |e| { query.set(e.value()); selected.set(0); },
                    onkeydown: move |e| {
                        let n = kb.len();
                        match e.key() {
                            Key::ArrowDown => { e.prevent_default(); if n > 0 { selected.set((sel + 1) % n); } }
                            Key::ArrowUp => { e.prevent_default(); if n > 0 { selected.set((sel + n - 1) % n); } }
                            Key::Enter => {
                                if let Some(v) = kb.get(sel).copied() { view.set(v); open.set(false); }
                            }
                            Key::Escape => open.set(false),
                            _ => {}
                        }
                    },
                }
                div { class: "palette-list",
                    if results.is_empty() {
                        div { class: "palette-empty label-tech", "No matching screen" }
                    } else {
                        for (i, v) in results.iter().copied().enumerate() {
                            button {
                                key: "{v.slug()}",
                                class: if i == sel { "palette-item active" } else { "palette-item" },
                                onmouseenter: move |_| selected.set(i),
                                onclick: move |_| { view.set(v); open.set(false); },
                                span { class: "palette-item-label", "{v.label()}" }
                                span { class: "palette-item-slug label-tech", "/#{v.slug()}" }
                            }
                        }
                    }
                }
                div { class: "palette-hint label-tech", "↑↓ navigate · ↵ open · esc close" }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// App shell — Sidebar / Topbar / StatusBar
// ---------------------------------------------------------------------------

/// One sidebar nav entry: its icon, label, and the view it opens.
type NavEntry = (Asset, &'static str, View);
/// A labeled sidebar group (`""` header ⇒ no label) and its entries.
type NavGroup = (&'static str, Vec<NavEntry>);

/// The sidebar navigation, grouped into labeled sections. Data-driven so the
/// IA is one table to read/reorder, and every item closes the mobile drawer on
/// click. An empty group header (`""`) renders no label (the lone Command item).
#[component]
fn Sidebar(view: Signal<View>, nav_open: Signal<bool>) -> Element {
    // Operator ask (2026-07-06): the Create wizard is a pre-genesis
    // surface — once an agent exists (an operator-declared Profile),
    // the nav entry is dead weight; editing lives in Agents/Settings.
    // While the snapshot is loading we assume post-genesis (the common
    // case) so the entry doesn't flash in and out.
    let agents = use_context::<Signal<AgentsState>>();
    let genesis_done = agents()
        .profile
        .as_ref()
        .map(|p| p.assistant_name_source == "toml")
        .unwrap_or(true);
    let agent_group: Vec<NavEntry> = if genesis_done {
        vec![
            (ICON_AGENTS, "Agents", View::Agents),
            (ICON_SKILLS, "Skills", View::Skills),
            (ICON_TEAMS, "Teams", View::Teams),
        ]
    } else {
        vec![
            (ICON_CREATE, "Create", View::Onboarding),
            (ICON_AGENTS, "Agents", View::Agents),
            (ICON_SKILLS, "Skills", View::Skills),
            (ICON_TEAMS, "Teams", View::Teams),
        ]
    };
    let groups: Vec<NavGroup> = vec![
        ("", vec![(ICON_COMMAND, "Command", View::Command)]),
        (
            "Workspace",
            vec![
                (ICON_CHAT, "Chat", View::Chat),
                (ICON_MISSIONS, "Missions", View::Missions),
                (ICON_MISSIONS, "Mission Control", View::MissionControl),
                (ICON_SCHEDULES, "Schedules", View::Schedules),
            ],
        ),
        (
            "Knowledge",
            vec![
                (ICON_MEMORY, "Memory", View::Memory),
                (ICON_WIKI, "Wiki", View::Wiki),
                (ICON_GRAPH, "Graph", View::Lattice),
            ],
        ),
        ("Agent", agent_group),
        (
            "System",
            vec![
                (ICON_DOCUMENTS, "Documents", View::Documents),
                (ICON_DOCUMENTS, "Audit", View::Audit),
                (ICON_TEAMS, "Sessions", View::Sessions),
                (ICON_GALLERY, "Gallery", View::Gallery),
                (ICON_NOTIFICATIONS, "Notifications", View::Notifications),
                (ICON_LOOP, "Loop", View::Loop),
                (ICON_NOTIFICATIONS, "Reminders", View::Reminders),
                (ICON_PLUGINS, "MCP", View::Mcp),
                (ICON_TOOLS, "Tools", View::Tools),
                (ICON_VOICE, "Voice", View::Voice),
                (ICON_SETTINGS, "Settings", View::Settings),
                (ICON_GUIDE, "Guide", View::Guide),
            ],
        ),
    ];

    rsx! {
        aside { class: "sidebar",
            div { class: "brand-lockup",
                img { src: LOGOMARK, alt: "Aivyx PA" }
                span { class: "wordmark", "AIVYX PA" }
            }
            nav { "aria-label": "Primary",
                for (header, items) in groups {
                    if !header.is_empty() {
                        div { class: "nav-section label-tech", "{header}" }
                    }
                    for (icon, label, v) in items {
                        NavItem { icon, label, active: view() == v,
                            onclick: move |_| { view.set(v); nav_open.set(false); } }
                    }
                }
            }
            div { style: "flex:1" }
        }
    }
}

#[component]
fn NavItem(icon: Asset, label: &'static str, active: bool, onclick: EventHandler<MouseEvent>) -> Element {
    rsx! {
        button {
            class: if active { "nav-item active" } else { "nav-item" },
            "aria-current": if active { "page" } else { "false" },
            onclick: move |e| onclick.call(e),
            span { class: "ico", style: "--ico: url({icon})" }
            "{label}"
        }
    }
}

/// The Guide screen — a page list plus the rendered markdown body. Pure static
/// content (bundled `docs/guide/*.md`), no daemon IPC. `selected` is the index
/// into [`guide::PAGES`]; the HTML is memoized so switching an unrelated signal
/// never re-parses the markdown.
///
/// Cross-page links inside the rendered markdown are real `.md` anchors (so they
/// also work on GitHub). In-app they aren't Dioxus-managed elements — they live
/// inside `dangerous_inner_html` — so we delegate: catch clicks on the content
/// container, walk up to the clicked `<a>`, and if its `href` names a guide page
/// ([`guide::index_for_href`]) switch to it instead of letting the browser
/// navigate away. External links (`http(s)://`, …) don't match and behave
/// normally.
#[component]
fn GuidePanel(page: Signal<usize>) -> Element {
    use dioxus::web::WebEventExt;
    use wasm_bindgen::JsCast;

    // The current page is shared (App-owned) so the topbar "?" help button can
    // open the Guide directly to the page relevant to the screen you were on.
    let mut selected = page;
    let body_html = use_memo(move || {
        let idx = selected().min(guide::PAGES.len().saturating_sub(1));
        guide::render(guide::PAGES[idx].body)
    });

    let on_content_click = move |evt: Event<MouseData>| {
        let Some(web_evt) = evt.try_as_web_event() else { return };
        let Some(target) = web_evt.target() else { return };
        let Some(el) = target.dyn_ref::<web_sys::Element>() else { return };
        // Nearest enclosing anchor (the click may land on text inside the <a>).
        let Ok(Some(anchor)) = el.closest("a") else { return };
        let Some(href) = anchor.get_attribute("href") else { return };
        if let Some(idx) = guide::index_for_href(&href) {
            evt.prevent_default();
            selected.set(idx);
        }
    };

    rsx! {
        div { class: "guide",
            nav { class: "guide-nav",
                div { class: "guide-nav-head label-tech", "User guide" }
                for (i, page) in guide::PAGES.iter().enumerate() {
                    button {
                        key: "{page.id}",
                        class: if selected() == i { "guide-link active" } else { "guide-link" },
                        onclick: move |_| selected.set(i),
                        "{page.title}"
                    }
                }
            }
            article {
                class: "guide-content glass-card",
                onclick: on_content_click,
                dangerous_inner_html: body_html(),
            }
        }
    }
}

#[component]
fn Topbar(
    title: &'static str,
    light: Signal<bool>,
    nav_open: Signal<bool>,
    mut view: Signal<View>,
    mut guide_page: Signal<usize>,
) -> Element {
    // Chapter Herald — the unseen count is purely client-side: every
    // history entry with a seq newer than the last time the operator
    // opened the Notifications screen. Resets to 0 the moment they
    // visit it (below), same "badge clears on view" convention as
    // any notification center.
    let mut notifications = use_context::<Signal<NotificationsState>>();
    let unseen = {
        let n = notifications();
        n.history.iter().filter(|e| e.seq > n.last_seen_seq).count()
    };
    rsx! {
        header { class: "topbar",
            // Hamburger — CSS shows it only below the shell breakpoint.
            button {
                class: "icon-btn nav-toggle",
                "aria-label": "Toggle navigation",
                onclick: move |_| nav_open.toggle(),
                span { class: "hamburger" }
            }
            span { class: "title", "{title}" }
            div { class: "spacer" }
            // Daemon connection status lives in the status bar (footer) as the
            // single source — the topbar no longer duplicates it.
            // Contextual help: open the Guide to the page for the current screen.
            button {
                class: "icon-btn",
                title: "Help for this screen",
                "aria-label": "Open the guide for this screen",
                onclick: move |_| {
                    guide_page.set(guide_page_for(view()));
                    view.set(View::Guide);
                },
                span { class: "ico", style: "--ico: url({ICON_GUIDE})" }
            }
            button {
                class: "icon-btn notify-bell",
                style: "position:relative;",
                title: "Notifications",
                "aria-label": if unseen > 0 { format!("{unseen} unread notifications") } else { "Notifications".to_string() },
                onclick: move |_| {
                    let max_seq = notifications().history.iter().map(|e| e.seq).max().unwrap_or(0);
                    notifications.write().last_seen_seq = max_seq;
                    view.set(View::Notifications);
                },
                span { class: "ico", style: "--ico: url({ICON_NOTIFICATIONS})" }
                if unseen > 0 {
                    span {
                        class: "badge",
                        style: "position:absolute; top:2px; right:2px; min-width:14px; height:14px; border-radius:7px; background:var(--danger, #b91c1c); border: 1px solid var(--color-primary); color:#fff; font-size:9px; line-height:14px; text-align:center; padding:0 3px;",
                        if unseen > 9 { "9+" } else { "{unseen}" }
                    }
                }
            }
            button {
                class: "icon-btn",
                title: "Toggle theme",
                "aria-label": "Toggle light/dark theme",
                onclick: move |_| light.toggle(),
                span { class: "ico", style: "--ico: url({ICON_THEME})" }
            }
        }
    }
}

/// Map a screen to the most relevant end-user guide page (index into
/// [`guide::PAGES`]) for the topbar "?" help button.
fn guide_page_for(v: View) -> usize {
    let id = match v {
        View::Chat | View::Missions => "chat-and-missions",
        View::Memory | View::Wiki | View::Lattice => "memory",
        View::Skills | View::Agents => "skills-and-persona",
        View::Teams => "teams",
        View::Settings => "access-and-settings",
        View::Onboarding => "create-your-agent",
        View::Guide => "welcome",
        // Command / Documents / Mcp / Voice → the screens overview.
        _ => "screens",
    };
    guide::PAGES.iter().position(|p| p.id == id).unwrap_or(0)
}

#[component]
fn StatusBar(connected: bool, agent_name: String) -> Element {
    // Vitrine final sweep (2026-07-05): this segment was a hardcoded
    // "AGENT · NONAGON" literal — the one static datum in the shell.
    // It now shows the RUNNING agent's name from the dashboard profile
    // snapshot (empty until the first snapshot arrives).
    let agent = if agent_name.trim().is_empty() {
        "AGENT · —".to_string()
    } else {
        format!("AGENT · {}", agent_name.to_uppercase())
    };
    rsx! {
        footer { class: "statusbar label-tech",
            div { class: if connected { "seg live" } else { "seg" },
                span { class: "dot" }
                if connected { "DAEMON · CONNECTED" } else { "DAEMON · OFFLINE" }
            }
            div { class: "seg seg-mid", "{agent}" }
            div { class: "seg seg-ver", {format!("AIVYX PA · v{}", env!("CARGO_PKG_VERSION"))} }
        }
    }
}

// ---------------------------------------------------------------------------
// Command Center — the home dashboard (read-only)
// ---------------------------------------------------------------------------

#[component]
fn CommandPanel(missions: Vec<TeamMissionView>, dashboard: Dashboard, connected: bool) -> Element {
    // Until the first dashboard snapshot arrives, show a skeleton instead of
    // flashing placeholder zeros (which then pop to real values on load).
    if !dashboard.loaded {
        return rsx! { CommandSkeleton {} };
    }
    let active = missions
        .iter()
        .filter(|m| !m.phase.is_terminal())
        .count();
    let chain = dashboard.chain_ok;
    let routines = dashboard.schedules.clone();
    let routines_total = routines.len();
    let routines_on = routines.iter().filter(|r| r.enabled).count();
    rsx! {
        div { class: "stat-row stat-row-5",
            StatCard { icon: ICON_MISSIONS, label: "Missions", value: "{missions.len()}", tone: None }
            StatCard { icon: ICON_AGENTS, label: "Active", value: "{active}", tone: None }
            StatCard { icon: ICON_COMMAND, label: "Routines", value: "{routines_on}/{routines_total}", tone: None }
            StatCard { icon: ICON_MEMORY, label: "Audit Events", value: "{dashboard.audit_total}", tone: None }
            StatCard { icon: ICON_SETTINGS, label: "Chain", value: chain_label(chain).to_string(), tone: chain_tone(chain) }
        }
        div { class: "dash-grid",
            div { class: "dash-main",
                section { class: "panel",
                    div { class: "panel-head",
                        h3 { "Active Missions" }
                        span { class: "label-tech", "{missions.len()} total" }
                    }
                    if missions.is_empty() {
                        div { class: "glass-card empty", p { class: "label-tech", "No missions yet — start one from the Missions tab." } }
                    } else {
                        div { class: "feed",
                            for m in missions.iter().take(4) {
                                DashMissionRow { mission: m.clone() }
                            }
                        }
                    }
                }
                section { class: "panel",
                    div { class: "panel-head",
                        h3 { "Routines" }
                        span { class: "label-tech", "{routines_on} of {routines_total} active" }
                    }
                    if routines.is_empty() {
                        div { class: "glass-card empty", p { class: "label-tech", "No background routines configured." } }
                    } else {
                        div { class: "feed",
                            for r in routines.iter() {
                                RoutineRow { routine: r.clone() }
                            }
                        }
                    }
                }
                section { class: "panel",
                    div { class: "panel-head",
                        h3 { "Audit Trail" }
                        span { class: "label-tech", "newest {AUDIT_FEED_N}" }
                    }
                    AuditFeed { entries: dashboard.audit_entries.clone() }
                }
            }
            aside { class: "dash-rail",
                AgentStatus { name: dashboard.assistant_name.clone(), connected, chain_ok: chain, settings: dashboard.settings.clone() }
                section { class: "panel",
                    div { class: "panel-head", h3 { "Learning" } }
                    match &dashboard.learning {
                        None => rsx! {
                            div { class: "glass-card empty",
                                p { class: "label-tech", "Loading…" }
                            }
                        },
                        Some(d) if d.recalls_total == 0 => rsx! {
                            div { class: "glass-card empty",
                                p { class: "label-tech", "Nothing learned yet — no recalls in the lookback window." }
                            }
                        },
                        Some(d) => rsx! {
                            div { class: "glass-card",
                                p { class: "label-tech", "{d.recalls_scored}/{d.recalls_total} recalls scored · {d.promoted} promoted · {d.proposals_in_window} proposals this window" }
                                if !d.top_helpful.is_empty() {
                                    p { class: "label-tech", style: "margin-top:6px;",
                                        "Most helpful: "
                                        for (topic, score) in d.top_helpful.iter().take(3) {
                                            span { style: "margin-right:8px;", "{topic} ({score:.2})" }
                                        }
                                    }
                                }
                            }
                        },
                    }
                }
            }
        }
    }
}

/// Loading skeleton for the Command Center — mirrors the real layout (4 stat
/// cards + the two-panel main + rail) so swapping in live data causes no shift.
#[component]
fn CommandSkeleton() -> Element {
    rsx! {
        div { class: "stat-row",
            for i in 0..4 {
                div { key: "{i}", class: "glass-card stat-card",
                    div { class: "stat-top",
                        span { class: "skeleton sk-ico" }
                        span { class: "skeleton sk-line sk-w40" }
                    }
                    span { class: "skeleton sk-value" }
                }
            }
        }
        div { class: "dash-grid",
            div { class: "dash-main",
                section { class: "panel",
                    div { class: "panel-head", span { class: "skeleton sk-line sk-w30" } }
                    div { class: "glass-card",
                        span { class: "skeleton sk-line sk-w70" }
                        span { class: "skeleton sk-line sk-w50" }
                    }
                }
                section { class: "panel",
                    div { class: "panel-head", span { class: "skeleton sk-line sk-w30" } }
                    div { class: "glass-card",
                        for i in 0..3 {
                            span { key: "{i}", class: "skeleton sk-line sk-w60" }
                        }
                    }
                }
            }
            aside { class: "dash-rail",
                div { class: "glass-card",
                    span { class: "skeleton sk-line sk-w50" }
                    span { class: "skeleton sk-line sk-w80" }
                    span { class: "skeleton sk-line sk-w70" }
                }
            }
        }
    }
}

/// Generic loading skeleton — `rows` shimmer cards stacked vertically. For
/// list-style screens (Memory entries, Documents files, Teams roster). Reuses
/// the `.feed` layout so the swap to real rows causes no shift.
#[component]
fn SkeletonList(rows: usize) -> Element {
    rsx! {
        div { class: "feed",
            for i in 0..rows {
                div { key: "{i}", class: "glass-card",
                    span { class: "skeleton sk-line sk-w40" }
                    span { class: "skeleton sk-line sk-w80" }
                }
            }
        }
    }
}

/// Generic loading skeleton — `cards` shimmer cards in the auto-fill grid. For
/// grid-style screens (Skills, MCP).
#[component]
fn SkeletonCards(cards: usize) -> Element {
    rsx! {
        div { class: "mcp-grid",
            for i in 0..cards {
                div { key: "{i}", class: "glass-card",
                    span { class: "skeleton sk-line sk-w50" }
                    span { class: "skeleton sk-line sk-w70" }
                    span { class: "skeleton sk-line sk-w30" }
                }
            }
        }
    }
}

#[component]
fn StatCard(icon: Asset, label: &'static str, value: String, tone: Option<&'static str>) -> Element {
    rsx! {
        div { class: "glass-card stat-card",
            div { class: "stat-top",
                span { class: "ico", style: "--ico: url({icon})" }
                span { class: "label-tech", "{label}" }
            }
            span {
                class: if let Some(t) = tone { "value {t}" } else { "value" },
                "{value}"
            }
        }
    }
}

#[component]
fn DashMissionRow(mission: TeamMissionView) -> Element {
    let pct = mission.progress.min(100);
    rsx! {
        div { class: "glass-card dash-mission",
            div { class: "row1",
                span { class: "chip {phase_class(mission.phase)}", "{phase_label(mission.phase)}" }
                span { class: "goal", "{mission.goal}" }
            }
            div { class: "progress", div { class: "fill", style: "width: {pct}%;" } }
        }
    }
}

#[component]
fn RoutineRow(routine: ScheduleView) -> Element {
    let last = routine
        .last_fired_unix_ms
        .map(rel_time)
        .unwrap_or_else(|| "never".to_string());
    let next = if routine.enabled {
        routine
            .next_fire_unix_ms
            .map(until_time)
            .unwrap_or_else(|| "—".to_string())
    } else {
        "paused".to_string()
    };
    rsx! {
        div { class: "glass-card routine-row",
            div { class: "row1",
                span { class: if routine.enabled { "dot live" } else { "dot off" } }
                span { class: "name", "{routine.name}" }
                span { class: "label-tech cron", "{routine.cron}" }
            }
            div { class: "row2 label-tech",
                span { "next " span { class: "v", "{next}" } }
                span { "last " span { class: "v", "{last}" } }
            }
        }
    }
}

// ── Chapter Chime — the Schedules screen ────────────────────────────────

fn schedules_refresh_query() -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-schedules".to_string(),
        payload: QueryPayload::GetSchedules,
    }
}

fn reflection_schedule_configs_query() -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-reflection-configs".to_string(),
        payload: QueryPayload::GetReflectionScheduleConfigs,
    }
}
fn set_reflection_schedule_query(entry: ReflectionScheduleConfigView) -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-reflection-set".to_string(),
        payload: QueryPayload::SetReflectionSchedule {
            name: entry.name,
            cron: entry.cron,
            lookback_window_secs: entry.lookback_window_secs,
            enabled: entry.enabled,
        },
    }
}
fn delete_reflection_schedule_query(name: String) -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-reflection-delete".to_string(),
        payload: QueryPayload::DeleteReflectionSchedule { name },
    }
}

/// Sort key: agent proposals awaiting approval first, then enabled
/// routines, then the rest, each bucket alphabetical.
fn schedule_sort_key(v: &ScheduleView) -> (u8, String) {
    let bucket = if v.created_by == "agent" && !v.enabled {
        0
    } else if v.enabled {
        1
    } else {
        2
    };
    (bucket, v.name.clone())
}

#[component]
fn SchedulesPanel() -> Element {
    let ws = use_context::<Sender>();
    let dashboard = use_context::<Signal<Dashboard>>();
    let mut ui = use_context::<Signal<SchedulesUi>>();
    let connected = use_context::<Signal<bool>>();

    // Fresh list on open (the 5 s poll keeps it live afterwards).
    use_future(move || async move {
        ws.send(schedules_refresh_query());
        ws.send(reflection_schedule_configs_query());
    });

    let mut name = use_signal(String::new);
    // Chime UX pass (operator finding): a raw 7-field cron is developer
    // UX. The builder generates it from frequency + time + day; the raw
    // field stays available as "Custom (advanced)".
    let mut freq = use_signal(|| "daily".to_string());
    let mut at_time = use_signal(|| "09:00".to_string());
    let mut weekday = use_signal(|| "Mon".to_string());
    let mut every_hours = use_signal(|| "6".to_string());
    let mut cron = use_signal(String::new);
    let mut prompt = use_signal(String::new);
    let mut start_enabled = use_signal(|| true);
    // POLISH_WAVES.md sub-project 7 plan 3 — the reflection-schedules
    // section's own add/edit state, separate from the regular-schedule
    // form above.
    let mut refl_editing = use_signal(|| None::<ReflectionScheduleConfigView>);
    let mut refl_adding = use_signal(|| false);

    // The generated cron + a human sentence, from the builder state.
    let built = use_memo(move || {
        let (h, m) = {
            let t = at_time();
            let mut it = t.splitn(2, ':');
            let h = it.next().unwrap_or("9").trim_start_matches('0');
            let m = it.next().unwrap_or("0").trim_start_matches('0');
            (
                if h.is_empty() { "0".to_string() } else { h.to_string() },
                if m.is_empty() { "0".to_string() } else { m.to_string() },
            )
        };
        match freq().as_str() {
            "daily" => (
                format!("0 {m} {h} * * * *"),
                format!("fires daily at {}", at_time()),
            ),
            "weekly" => (
                format!("0 {m} {h} * * {} *", weekday()),
                format!("fires every {} at {}", weekday(), at_time()),
            ),
            "hourly" => (
                format!("0 0 */{} * * * *", every_hours()),
                format!("fires every {} hours", every_hours()),
            ),
            _ => (cron().trim().to_string(), "custom cron (advanced)".to_string()),
        }
    });

    let mut rows = dashboard().schedules.clone();
    rows.sort_by_key(schedule_sort_key);
    let pending: usize = rows
        .iter()
        .filter(|r| r.created_by == "agent" && !r.enabled)
        .count();

    let create = move |_| {
        let n = name().trim().to_string();
        let c = built().0;
        let pr = prompt().trim().to_string();
        if n.is_empty() || c.is_empty() || pr.is_empty() {
            ui.write().notice =
                Some((false, "name, cron, and prompt are all required".into()));
            return;
        }
        ws.send(FrontendMessage::CreateSchedule {
            id: format!("mc-sched-create-{n}"),
            name: n,
            cron: c,
            prompt: pr,
            enabled: start_enabled(),
        });
        ws.send(schedules_refresh_query());
        name.set(String::new());
        cron.set(String::new());
        prompt.set(String::new());
    };

    rsx! {
        if let Some((ok, text)) = ui().notice {
            div {
                class: "glass-card",
                style: if ok {
                    "border-left: 3px solid var(--ok, #16a34a); margin-bottom: 12px; padding: 8px 12px;"
                } else {
                    "border-left: 3px solid var(--danger, #b91c1c); margin-bottom: 12px; padding: 8px 12px;"
                },
                p { class: "label-tech", "{text}" }
            }
        }
        div { class: "dash-grid",
            div { class: "dash-main",
                section { class: "panel",
                    div { class: "panel-head",
                        h3 { "Schedules" }
                        span { class: "label-tech",
                            if pending > 0 {
                                "{rows.len()} total · {pending} awaiting approval"
                            } else {
                                "{rows.len()} total"
                            }
                        }
                    }
                    if rows.is_empty() {
                        div { class: "glass-card empty",
                            p { class: "label-tech", "No schedules yet — create one below, or ask the agent to schedule something." }
                        }
                    } else {
                        div { class: "feed",
                            for r in rows.iter() {
                                ScheduleAdminRow { schedule: r.clone() }
                            }
                        }
                    }
                }
                section { class: "panel",
                    div { class: "panel-head",
                        h3 { "Reflection schedules" }
                        button {
                            class: "btn btn-primary btn-xs",
                            onclick: move |_| { refl_editing.set(None); refl_adding.set(true); },
                            "Add reflection schedule"
                        }
                    }
                    p { class: "label-tech",
                        "Distinct from regular schedules above — each entry fires a canonical \
                         reflection turn (outcome-summary input, persistent proposal store) on \
                         its own cron. role_override/skip_when_idle/min_audit_entries_to_fire \
                         stay TOML-only for now."
                    }
                    if refl_adding() || refl_editing().is_some() {
                        ReflectionScheduleForm {
                            key: "{refl_editing().map(|e| e.name.clone()).unwrap_or_else(|| \"new\".to_string())}",
                            initial: refl_editing(),
                            on_cancel: move |_| { refl_adding.set(false); refl_editing.set(None); },
                            on_save: move |entry: ReflectionScheduleConfigView| {
                                ws.send(set_reflection_schedule_query(entry));
                                refl_adding.set(false);
                                refl_editing.set(None);
                            },
                        }
                    } else if dashboard().reflection_schedules.is_empty() {
                        div { class: "glass-card empty", p { class: "label-tech", "No `[[reflection_schedule]]` entries configured yet." } }
                    } else {
                        div { class: "feed",
                            for r in dashboard().reflection_schedules.iter() {
                                {
                                    let r2 = r.clone();
                                    let rname = r.name.clone();
                                    rsx! {
                                        div { key: "{r.name}", class: "glass-card routine-row",
                                            div { class: "row1",
                                                span { class: "dot live" }
                                                span { class: "name", "{r.name}" }
                                                span { class: "label-tech", style: "opacity:0.7;", "{r.cron}" }
                                                span { class: if r.enabled { "chip success" } else { "chip" }, if r.enabled { "enabled" } else { "disabled" } }
                                            }
                                            div { style: "display:flex; gap:8px; margin-top:8px;",
                                                button { class: "btn btn-glass btn-xs", onclick: move |_| { refl_adding.set(false); refl_editing.set(Some(r2.clone())); }, "Edit" }
                                                button { class: "btn btn-glass btn-xs", onclick: move |_| ws.send(delete_reflection_schedule_query(rname.clone())), "Delete" }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            aside { class: "dash-rail",
                section { class: "panel",
                    div { class: "panel-head", h3 { "New schedule" } }
                    div { class: "glass-card",
                        label { class: "label-tech", "Name" }
                        input {
                            class: "input",
                            placeholder: "coffee-stock-check",
                            value: "{name}",
                            oninput: move |e| name.set(e.value()),
                        }
                        label { class: "label-tech", "When" }
                        select {
                            class: "input",
                            value: "{freq}",
                            onchange: move |e| freq.set(e.value()),
                            option { value: "daily", "Every day" }
                            option { value: "weekly", "Once a week" }
                            option { value: "hourly", "Every few hours" }
                            option { value: "custom", "Custom (advanced)" }
                        }
                        if freq() == "daily" || freq() == "weekly" {
                            div { style: "display:flex; gap:8px; align-items:center; margin:6px 0;",
                                if freq() == "weekly" {
                                    select {
                                        class: "input",
                                        style: "flex:1;",
                                        value: "{weekday}",
                                        onchange: move |e| weekday.set(e.value()),
                                        option { value: "Mon", "Monday" }
                                        option { value: "Tue", "Tuesday" }
                                        option { value: "Wed", "Wednesday" }
                                        option { value: "Thu", "Thursday" }
                                        option { value: "Fri", "Friday" }
                                        option { value: "Sat", "Saturday" }
                                        option { value: "Sun", "Sunday" }
                                    }
                                }
                                span { class: "label-tech", "at" }
                                input {
                                    class: "input",
                                    style: "flex:1;",
                                    r#type: "time",
                                    value: "{at_time}",
                                    oninput: move |e| at_time.set(e.value()),
                                }
                            }
                        }
                        if freq() == "hourly" {
                            div { style: "display:flex; gap:8px; align-items:center; margin:6px 0;",
                                span { class: "label-tech", "every" }
                                select {
                                    class: "input",
                                    style: "flex:1;",
                                    value: "{every_hours}",
                                    onchange: move |e| every_hours.set(e.value()),
                                    option { value: "1", "1 hour" }
                                    option { value: "2", "2 hours" }
                                    option { value: "3", "3 hours" }
                                    option { value: "4", "4 hours" }
                                    option { value: "6", "6 hours" }
                                    option { value: "12", "12 hours" }
                                }
                            }
                        }
                        if freq() == "custom" {
                            label { class: "label-tech", "Cron (sec min hour dom month dow year — local time)" }
                            input {
                                class: "input",
                                placeholder: "0 0 9 * * * *",
                                value: "{cron}",
                                oninput: move |e| cron.set(e.value()),
                            }
                        }
                        p { class: "label-tech", style: "opacity:0.7; margin:4px 0;",
                            "{built().1} · cron: {built().0}"
                        }
                        label { class: "label-tech", "Prompt (what the agent should do when it fires)" }
                        textarea {
                            class: "input",
                            rows: "4",
                            placeholder: "Check the workspace journal and summarize anything new.",
                            value: "{prompt}",
                            oninput: move |e| prompt.set(e.value()),
                        }
                        label { class: "label-tech", style: "display:flex; align-items:center; gap:6px; margin:6px 0;",
                            input {
                                r#type: "checkbox",
                                checked: start_enabled(),
                                onchange: move |e| start_enabled.set(e.checked()),
                            }
                            "start enabled"
                        }
                        button {
                            class: "btn btn-primary btn-xs",
                            disabled: !connected(),
                            onclick: create,
                            "Create schedule"
                        }
                    }
                }
            }
        }
    }
}

/// One admin row: provenance badge + live timing + approve / pause /
/// resume / delete controls. Deleting is a two-step confirm; config
/// routines expose pause/resume only (the daemon refuses their delete —
/// the boot sync would resurrect them).
#[component]
fn ScheduleAdminRow(schedule: ScheduleView) -> Element {
    let ws = use_context::<Sender>();
    let mut ui = use_context::<Signal<SchedulesUi>>();
    let sid = schedule.schedule_id.clone();
    let awaiting = schedule.created_by == "agent" && !schedule.enabled;
    let confirm_armed = ui().confirm_delete.as_deref() == Some(sid.as_str());
    let next = if schedule.enabled {
        schedule
            .next_fire_unix_ms
            .map(until_time)
            .unwrap_or_else(|| "—".to_string())
    } else {
        "paused".to_string()
    };
    let last = schedule
        .last_fired_unix_ms
        .map(rel_time)
        .unwrap_or_else(|| "never".to_string());

    let toggle_id = sid.clone();
    let toggle_to = !schedule.enabled;
    let delete_id = sid.clone();
    let confirm_id = sid.clone();

    rsx! {
        div { class: "glass-card routine-row",
            div { class: "row1",
                span { class: if schedule.enabled { "dot live" } else { "dot off" } }
                span { class: "name", "{schedule.name}" }
                span { class: "label-tech", style: "opacity:0.7;", "[{schedule.created_by}]" }
                if awaiting {
                    span { class: "label-tech", style: "color: var(--warn, #d97706);", "awaiting approval" }
                }
                span { class: "label-tech cron", "{schedule.cron}" }
            }
            if !schedule.prompt.is_empty() {
                div { class: "row2 label-tech", style: "opacity:0.8;",
                    "{schedule.prompt}"
                }
            }
            div { class: "row2 label-tech",
                span { "next " span { class: "v", "{next}" } }
                span { "last " span { class: "v", "{last}" } }
                span { style: "margin-left:auto; display:flex; gap:6px;",
                    button {
                        class: "btn btn-glass",
                        onclick: move |_| {
                            ws.send(FrontendMessage::UpdateSchedule {
                                id: format!("mc-sched-toggle-{toggle_id}"),
                                schedule_id: toggle_id.clone(),
                                enabled: Some(toggle_to),
                                cron: None,
                                prompt: None,
                            });
                            ws.send(schedules_refresh_query());
                        },
                        if awaiting { "Approve" } else if schedule.enabled { "Pause" } else { "Resume" }
                    }
                    if schedule.created_by != "config" {
                        if confirm_armed {
                            button {
                                class: "btn btn-glass",
                                style: "color: var(--danger, #b91c1c);",
                                onclick: move |_| {
                                    ws.send(FrontendMessage::DeleteSchedule {
                                        id: format!("mc-sched-delete-{delete_id}"),
                                        schedule_id: delete_id.clone(),
                                    });
                                    ws.send(schedules_refresh_query());
                                },
                                "Confirm delete"
                            }
                        } else {
                            button {
                                class: "btn btn-glass",
                                onclick: move |_| ui.write().confirm_delete = Some(confirm_id.clone()),
                                "Delete"
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn AuditFeed(entries: Vec<AuditEntrySummary>) -> Element {
    rsx! {
        div { class: "audit-feed",
            if entries.is_empty() {
                p { class: "label-tech empty", "No audit events yet." }
            } else {
                // entries arrive oldest→newest; show newest first.
                for e in entries.iter().rev() {
                    div { class: "audit-row",
                        span { class: "ev", "{e.event_type}" }
                        span { class: "when label-tech", "{rel_time(e.appended_at_unix_ms)}" }
                        span { class: "seq label-tech", "#{e.seq}" }
                    }
                }
            }
        }
    }
}

// ── Chapter Herald — the Notifications screen ───────────────────────────

#[component]
fn NotificationsPanel() -> Element {
    let ws = use_context::<Sender>();
    let n = use_context::<Signal<NotificationsState>>();
    let notify_config_ui = use_context::<Signal<NotifyConfigUi>>();
    let mut editing = use_signal(|| None::<NotifyTargetConfigView>);
    let mut adding = use_signal(|| false);
    let state = n();

    // POLISH_WAVES.md sub-project 7 plan 2 — load the editable target list
    // each time the screen opens, alongside the existing target-status
    // query (the read-only "Targets" rail's data is otherwise refreshed
    // by the App-level 5 s poll — see `GetNotifyTargets` above — but
    // resending it here too, McpPanel-style, means this screen doesn't
    // depend on the poll having already ticked once).
    use_future(move || async move {
        ws.send(FrontendMessage::Query {
            id: "mc-notify-targets".to_string(),
            payload: QueryPayload::GetNotifyTargets,
        });
        ws.send(notify_target_configs_query());
        ws.send(get_email_config_query());
        ws.send(get_telegram_config_query());
        ws.send(get_discord_config_query());
        ws.send(get_slack_config_query());
    });

    rsx! {
        div { class: "dash-grid",
            div { class: "dash-main",
                section { class: "panel",
                    div { class: "panel-head",
                        h3 { "Notification history" }
                        span { class: "label-tech", "{state.total_len} total" }
                    }
                    if state.history.is_empty() {
                        div { class: "glass-card empty",
                            p { class: "label-tech",
                                if state.targets.is_empty() {
                                    "No notify targets configured yet — add a [[notify_target]] to aivyx-pa.toml, or the Studio's own target arms automatically once one is running."
                                } else {
                                    "No notifications dispatched yet."
                                }
                            }
                        }
                    } else {
                        div { class: "feed",
                            // newest first (the query already returns the
                            // newest window; oldest→newest within it).
                            for e in state.history.iter().rev() {
                                NotificationHistoryRow { entry: e.clone() }
                            }
                        }
                    }
                }
                section { class: "panel",
                    div { class: "panel-head",
                        h3 { "Configure targets" }
                        button { class: "btn btn-primary btn-xs", onclick: move |_| { editing.set(None); adding.set(true); }, "Add target" }
                    }
                    if let Some((ok, text)) = notify_config_ui().notice {
                        div { class: if ok { "notice ok" } else { "notice err" }, "{text}" }
                    }
                    if adding() || editing().is_some() {
                        NotifyTargetForm {
                            // Plan 1's `McpServerForm` final-review fix,
                            // applied from the start here: forces a fresh
                            // component instance (rather than diffing
                            // props onto the live one) whenever which
                            // target is being edited changes, including
                            // the "editing X" → "adding new" transition —
                            // otherwise the form's `use_signal` seed
                            // initializers (first-mount-only) would keep
                            // showing the previous target's stale field
                            // values, with Save overwriting the wrong entry.
                            key: "{editing().map(|e| e.name.clone()).unwrap_or_else(|| \"new\".to_string())}",
                            initial: editing(),
                            on_cancel: move |_| { adding.set(false); editing.set(None); },
                            on_save: move |entry: NotifyTargetConfigView| {
                                ws.send(set_notify_target_query(entry));
                                adding.set(false);
                                editing.set(None);
                            },
                        }
                    } else if state.configs.is_empty() {
                        div { class: "glass-card empty", p { class: "label-tech", "No `[[notify_target]]` entries configured yet." } }
                    } else {
                        div { class: "feed",
                            for cfg in state.configs.iter() {
                                {
                                    let cfg2 = cfg.clone();
                                    let name = cfg.name.clone();
                                    rsx! {
                                        div { key: "{cfg.name}", class: "glass-card routine-row",
                                            div { class: "row1",
                                                span { class: "dot live" }
                                                span { class: "name", "{cfg.name}" }
                                                span { class: "label-tech", style: "opacity:0.7;", "[{cfg.kind}]" }
                                                if cfg.is_default {
                                                    span { class: "label-tech", style: "color: var(--ok, #16a34a);", "default" }
                                                }
                                                span { class: if cfg.enabled { "chip success" } else { "chip" }, if cfg.enabled { "enabled" } else { "disabled" } }
                                            }
                                            div { style: "display:flex; gap:8px; margin-top:8px;",
                                                button { class: "btn btn-glass btn-xs", onclick: move |_| { adding.set(false); editing.set(Some(cfg2.clone())); }, "Edit" }
                                                button { class: "btn btn-glass btn-xs", onclick: move |_| ws.send(delete_notify_target_query(name.clone())), "Delete" }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                ChannelAdaptersSection { state: n() }
            }
            aside { class: "dash-rail",
                section { class: "panel",
                    div { class: "panel-head", h3 { "Targets" } }
                    if state.targets.is_empty() {
                        div { class: "glass-card empty",
                            p { class: "label-tech", "None configured." }
                        }
                    } else {
                        div { class: "feed",
                            for t in state.targets.iter() {
                                div { class: "glass-card routine-row",
                                    div { class: "row1",
                                        span { class: "dot live" }
                                        span { class: "name", "{t.name}" }
                                        span { class: "label-tech", style: "opacity:0.7;", "[{t.kind}]" }
                                        if t.is_default {
                                            span { class: "label-tech", style: "color: var(--ok, #16a34a);", "default" }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                section { class: "panel",
                    div { class: "panel-head", h3 { "About" } }
                    div { class: "glass-card",
                        p { class: "label-tech",
                            "Add or edit targets in the \"Configure targets\" section. Changes take effect on the next daemon restart. Any mission or schedule with no explicit notify target falls back to whichever target above is marked default."
                        }
                    }
                }
            }
        }
    }
}

/// POLISH_WAVES.md sub-project 7 plan 2 — the editable notify-target
/// query builders, mirroring `mcp_server_configs_query`/`set_mcp_server_
/// query`/`delete_mcp_server_query`'s own shape exactly.
fn notify_target_configs_query() -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-notify-configs".to_string(),
        payload: QueryPayload::GetNotifyTargetConfigs,
    }
}

fn set_notify_target_query(target: NotifyTargetConfigView) -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-notify-set".to_string(),
        payload: QueryPayload::SetNotifyTarget {
            name: target.name,
            kind: target.kind,
            chat_id: target.chat_id,
            url: target.url,
            to: target.to,
            enabled: target.enabled,
            is_default: target.is_default,
            retry_count: target.retry_count,
            retry_backoff_ms_start: target.retry_backoff_ms_start,
            rate_limit_max: target.rate_limit_max,
            rate_limit_window_secs: target.rate_limit_window_secs,
        },
    }
}

fn delete_notify_target_query(name: String) -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-notify-delete".to_string(),
        payload: QueryPayload::DeleteNotifyTarget { name },
    }
}

/// POLISH_WAVES.md sub-project 7 plan 2 (Task 7) — the 4 channel-adapter
/// query builders. All share the `"mc-notify-channels"` id, which the
/// `id.starts_with("mc-notify")` `QueryError` routing arm already catches
/// by prefix — a rejected save on any of these lands on the same
/// `notify_config_ui` notice banner the notify-target form uses. These
/// builders only expose the fields the Task 7 forms edit —
/// `team_run_channel`/`team_trigger_rate_limit`/`team_command_allowed_
/// senders` stay TOML-only for this pass; a future pass can extend the
/// forms without changing the wire types, which already carry them.
fn get_email_config_query() -> FrontendMessage {
    FrontendMessage::Query { id: "mc-notify-channels".to_string(), payload: QueryPayload::GetEmailConfig }
}
fn set_email_config_query(
    host: Option<String>,
    port: Option<u16>,
    tls_mode: Option<String>,
    username: Option<String>,
    password: Option<String>,
    from: Option<String>,
) -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-notify-channels".to_string(),
        payload: QueryPayload::SetEmailConfig { host, port, tls_mode, username, password, from },
    }
}
fn get_telegram_config_query() -> FrontendMessage {
    FrontendMessage::Query { id: "mc-notify-channels".to_string(), payload: QueryPayload::GetTelegramConfig }
}
fn set_telegram_config_query(token: Option<String>, chat_id: Option<i64>) -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-notify-channels".to_string(),
        payload: QueryPayload::SetTelegramConfig {
            token,
            chat_id,
            team_run_channel: None,
            team_trigger_rate_limit: None,
            team_command_allowed_senders: None,
        },
    }
}
fn get_discord_config_query() -> FrontendMessage {
    FrontendMessage::Query { id: "mc-notify-channels".to_string(), payload: QueryPayload::GetDiscordConfig }
}
fn set_discord_config_query(token: Option<String>, application_id: Option<u64>) -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-notify-channels".to_string(),
        payload: QueryPayload::SetDiscordConfig {
            token,
            application_id,
            team_run_channel: None,
            team_trigger_rate_limit: None,
            team_command_allowed_senders: None,
        },
    }
}
fn get_slack_config_query() -> FrontendMessage {
    FrontendMessage::Query { id: "mc-notify-channels".to_string(), payload: QueryPayload::GetSlackConfig }
}
fn set_slack_config_query(bot_token: Option<String>, app_token: Option<String>, team_id: Option<String>) -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-notify-channels".to_string(),
        payload: QueryPayload::SetSlackConfig {
            bot_token,
            app_token,
            team_id,
            team_run_channel: None,
            team_trigger_rate_limit: None,
            team_command_allowed_senders: None,
        },
    }
}

#[component]
fn NotifyTargetForm(
    initial: Option<NotifyTargetConfigView>,
    on_cancel: EventHandler<()>,
    on_save: EventHandler<NotifyTargetConfigView>,
) -> Element {
    let seed = initial.clone().unwrap_or(NotifyTargetConfigView {
        name: String::new(),
        kind: "telegram".to_string(),
        chat_id: None,
        url: None,
        to: None,
        enabled: true,
        is_default: false,
        retry_count: 0,
        retry_backoff_ms_start: 500,
        rate_limit_max: None,
        rate_limit_window_secs: None,
    });
    let editing_existing = initial.is_some();
    let mut name = use_signal(|| seed.name.clone());
    let mut kind = use_signal(|| seed.kind.clone());
    let mut chat_id = use_signal(|| seed.chat_id.clone().unwrap_or_default());
    let mut url = use_signal(|| seed.url.clone().unwrap_or_default());
    let mut to = use_signal(|| seed.to.clone().unwrap_or_default());
    let mut enabled = use_signal(|| seed.enabled);
    let mut is_default = use_signal(|| seed.is_default);

    rsx! {
        div { class: "glass-card",
            div { class: "field-row",
                label { "Name" }
                input { class: "input", value: "{name}", disabled: editing_existing, oninput: move |e| name.set(e.value()) }
            }
            div { class: "field-row",
                label { "Kind" }
                select { class: "input", value: "{kind}", onchange: move |e| kind.set(e.value()),
                    option { value: "telegram", "telegram" }
                    option { value: "webhook", "webhook" }
                    option { value: "email", "email" }
                    option { value: "web-ui", "web-ui" }
                }
            }
            if kind() == "telegram" {
                div { class: "field-row",
                    label { "Chat ID" }
                    input { class: "input", value: "{chat_id}", oninput: move |e| chat_id.set(e.value()) }
                }
            } else if kind() == "webhook" {
                div { class: "field-row",
                    label { "URL" }
                    input { class: "input", value: "{url}", oninput: move |e| url.set(e.value()) }
                }
            } else if kind() == "email" {
                div { class: "field-row",
                    label { "To" }
                    input { class: "input", value: "{to}", oninput: move |e| to.set(e.value()) }
                }
            }
            div { class: "field-row",
                label { "Enabled" }
                input { r#type: "checkbox", checked: enabled(), onchange: move |e| enabled.set(e.checked()) }
            }
            div { class: "field-row",
                label { "Default target" }
                input { r#type: "checkbox", checked: is_default(), onchange: move |e| is_default.set(e.checked()) }
            }
            div { style: "display:flex; gap:8px; margin-top:12px;",
                button {
                    class: "btn btn-primary btn-xs",
                    onclick: move |_| {
                        let k = kind();
                        let entry = NotifyTargetConfigView {
                            name: name().trim().to_string(),
                            kind: k.clone(),
                            chat_id: if k == "telegram" && !chat_id().trim().is_empty() { Some(chat_id().trim().to_string()) } else { None },
                            url: if k == "webhook" && !url().trim().is_empty() { Some(url().trim().to_string()) } else { None },
                            to: if k == "email" && !to().trim().is_empty() { Some(to().trim().to_string()) } else { None },
                            enabled: enabled(),
                            is_default: is_default(),
                            retry_count: seed.retry_count,
                            retry_backoff_ms_start: seed.retry_backoff_ms_start,
                            rate_limit_max: seed.rate_limit_max,
                            rate_limit_window_secs: seed.rate_limit_window_secs,
                        };
                        on_save.call(entry);
                    },
                    "Save"
                }
                button { class: "btn btn-glass btn-xs", onclick: move |_| on_cancel.call(()), "Cancel" }
            }
        }
    }
}

/// POLISH_WAVES.md sub-project 7 plan 3 — add/edit form for one
/// `[[reflection_schedule]]` entry. Duplicates (rather than extracting
/// into a shared component) the freq/time/day cron builder
/// `SchedulesPanel`'s own regular-schedule create form already has —
/// that builder is inline in `SchedulesPanel`, not its own component,
/// and this is the only other cron-shaped form in the codebase.
#[component]
fn ReflectionScheduleForm(
    initial: Option<ReflectionScheduleConfigView>,
    on_cancel: EventHandler<()>,
    on_save: EventHandler<ReflectionScheduleConfigView>,
) -> Element {
    let seed = initial.clone().unwrap_or(ReflectionScheduleConfigView {
        name: String::new(),
        cron: String::new(),
        lookback_window_secs: 86_400,
        enabled: true,
    });
    let editing_existing = initial.is_some();
    let mut ui = use_context::<Signal<SchedulesUi>>();
    let mut name = use_signal(|| seed.name.clone());
    // Editing an existing entry must open in "custom" mode so `built()`'s
    // fallback arm (`_ => cron().trim().to_string()`) reads the real seeded
    // cron instead of silently recomputing a fresh daily/09:00 cron and
    // overwriting whatever was actually stored (weekly/hourly/hand-written)
    // on the next Save.
    let mut freq = use_signal(|| if editing_existing { "custom".to_string() } else { "daily".to_string() });
    let mut at_time = use_signal(|| "09:00".to_string());
    let mut weekday = use_signal(|| "Mon".to_string());
    let mut every_hours = use_signal(|| "6".to_string());
    let mut cron = use_signal(|| seed.cron.clone());
    let mut lookback_hours = use_signal(|| (seed.lookback_window_secs / 3600).max(1).to_string());
    // Source of truth for the actual value that will be saved — seeded to the
    // entry's EXACT on-disk value (not the truncated hours display), and only
    // overwritten when the hours field parses to a valid `>= 1` whole number.
    // This means an entry whose lookback_window_secs isn't an exact multiple
    // of 3600 (e.g. hand-edited to 5400) round-trips unchanged if the operator
    // never touches this field, instead of silently rounding down to 3600 on
    // every save.
    let mut lookback_secs = use_signal(|| seed.lookback_window_secs.max(60));
    let mut enabled = use_signal(|| seed.enabled);

    let built = use_memo(move || {
        let (h, m) = {
            let t = at_time();
            let mut it = t.splitn(2, ':');
            let h = it.next().unwrap_or("9").trim_start_matches('0');
            let m = it.next().unwrap_or("0").trim_start_matches('0');
            (
                if h.is_empty() { "0".to_string() } else { h.to_string() },
                if m.is_empty() { "0".to_string() } else { m.to_string() },
            )
        };
        match freq().as_str() {
            "daily" => format!("0 {m} {h} * * * *"),
            "weekly" => format!("0 {m} {h} * * {} *", weekday()),
            "hourly" => format!("0 0 */{} * * * *", every_hours()),
            _ => cron().trim().to_string(),
        }
    });

    rsx! {
        div { class: "glass-card",
            div { class: "field-row",
                label { "Name" }
                input { class: "input", value: "{name}", disabled: editing_existing, oninput: move |e| name.set(e.value()) }
            }
            label { class: "label-tech", "When" }
            select {
                class: "input",
                value: "{freq}",
                onchange: move |e| freq.set(e.value()),
                option { value: "daily", "Every day" }
                option { value: "weekly", "Once a week" }
                option { value: "hourly", "Every few hours" }
                option { value: "custom", "Custom (advanced)" }
            }
            if freq() == "daily" || freq() == "weekly" {
                div { style: "display:flex; gap:8px; align-items:center; margin:6px 0;",
                    if freq() == "weekly" {
                        select {
                            class: "input", style: "flex:1;", value: "{weekday}",
                            onchange: move |e| weekday.set(e.value()),
                            option { value: "Mon", "Monday" }
                            option { value: "Tue", "Tuesday" }
                            option { value: "Wed", "Wednesday" }
                            option { value: "Thu", "Thursday" }
                            option { value: "Fri", "Friday" }
                            option { value: "Sat", "Saturday" }
                            option { value: "Sun", "Sunday" }
                        }
                    }
                    span { class: "label-tech", "at" }
                    input { class: "input", style: "flex:1;", r#type: "time", value: "{at_time}", oninput: move |e| at_time.set(e.value()) }
                }
            }
            if freq() == "hourly" {
                div { style: "display:flex; gap:8px; align-items:center; margin:6px 0;",
                    span { class: "label-tech", "every" }
                    select {
                        class: "input", style: "flex:1;", value: "{every_hours}",
                        onchange: move |e| every_hours.set(e.value()),
                        option { value: "1", "1 hour" }
                        option { value: "2", "2 hours" }
                        option { value: "3", "3 hours" }
                        option { value: "4", "4 hours" }
                        option { value: "6", "6 hours" }
                        option { value: "12", "12 hours" }
                    }
                }
            }
            if freq() == "custom" {
                label { class: "label-tech", "Cron (sec min hour dom month dow year — local time)" }
                input { class: "input", placeholder: "0 0 9 * * * *", value: "{cron}", oninput: move |e| cron.set(e.value()) }
            }
            p { class: "label-tech", style: "opacity:0.7; margin:4px 0;", "cron: {built()}" }
            div { class: "field-row",
                label { "Lookback (hours)" }
                input {
                    class: "input",
                    value: "{lookback_hours}",
                    oninput: move |e| {
                        let v = e.value();
                        lookback_hours.set(v.clone());
                        if let Ok(h) = v.trim().parse::<u64>() {
                            if h >= 1 {
                                lookback_secs.set(h.saturating_mul(3600));
                            }
                        }
                        // Invalid/empty input: lookback_secs deliberately keeps
                        // its last valid value (or the untouched seed) until a
                        // valid one is typed — Save below re-validates the
                        // currently-displayed text and blocks if it's invalid.
                    },
                }
            }
            div { class: "field-row",
                label { "Enabled" }
                input { r#type: "checkbox", checked: enabled(), onchange: move |e| enabled.set(e.checked()) }
            }
            div { style: "display:flex; gap:8px; margin-top:12px;",
                button {
                    class: "btn btn-primary btn-xs",
                    onclick: move |_| {
                        let n = name().trim().to_string();
                        let c = built();
                        if n.is_empty() || c.is_empty() {
                            ui.write().notice = Some((false, "name and cron are both required".into()));
                            return;
                        }
                        let hours_raw = lookback_hours().trim().to_string();
                        let valid = matches!(hours_raw.parse::<u64>(), Ok(h) if h >= 1);
                        if !valid {
                            ui.write().notice = Some((
                                false,
                                format!(
                                    "{hours_raw:?} is not a valid whole number of hours (>= 1) — Save was not sent."
                                ),
                            ));
                            return;
                        }
                        on_save.call(ReflectionScheduleConfigView {
                            name: n,
                            cron: c,
                            lookback_window_secs: lookback_secs(),
                            enabled: enabled(),
                        });
                    },
                    "Save"
                }
                button { class: "btn btn-glass btn-xs", onclick: move |_| on_cancel.call(()), "Cancel" }
            }
        }
    }
}

/// POLISH_WAVES.md sub-project 7 plan 2 (Task 7) — the "Channel adapters"
/// section: one card per channel (email/telegram/discord/slack), each with
/// a masked-secret field pattern. Every secret field shows only a
/// "configured"/"not set" label sourced from `RedactedSecret.configured` —
/// never a pre-filled or echoed real value — and a blank input on Save
/// sends `None`, which the daemon treats as "don't change this token"
/// rather than "clear it" (the same partial-update convention Tasks 1-2
/// established server-side).
/// The Email (SMTP) card, split out of [`ChannelAdaptersSection`] into its
/// own component so its `host`/`username`/`from` `use_signal` seeds are
/// correct on first mount.
///
/// `NotificationsState` is App-level context, so it can survive a full
/// navigation away from and back to the Notifications screen. On a
/// *return* visit, `state.email` may already hold `Some` stale value left
/// over from the prior visit at the instant this component (re)mounts —
/// the fresh `GetEmailConfig` response for *this* visit hasn't landed yet.
/// Without a `key`, Dioxus reuses the existing component instance across
/// that later state update, so the `use_signal` seeds latch onto the stale
/// value at mount and never re-seed once the fresh response arrives
/// (final-review finding #3). The call site below keys this component on
/// the `Debug` representation of `email` itself (there's no natural
/// unique id on a singleton config the way `NotifyTargetForm` keys on the
/// target's `name`) so a genuinely new value — including the very first
/// fresh response replacing a stale one — forces a fresh mount and correct
/// seeds, matching the same "key on what identifies this state" precedent
/// `NotifyTargetForm` already establishes for the array-entry case.
///
/// `host`/`port`/`username`/`from` are plain (non-secret) fields on
/// `EmailConfigView`, so they're safe to pre-fill and edit as plain text —
/// unlike `password`, which stays masked and always starts blank
/// ("blank on Save" = "don't change this field", the established
/// partial-update convention). `tls_mode` stays out of scope for this pass
/// (final-review finding #1(b)) — the loader defaults it to `"starttls"`
/// when absent, and exposing it isn't needed to fix the bricking bug.
#[component]
fn EmailAdapterCard(email: EmailConfigView) -> Element {
    let ws = use_context::<Sender>();
    let mut notify_config_ui = use_context::<Signal<NotifyConfigUi>>();
    let mut host = use_signal(|| email.host.clone().unwrap_or_default());
    let mut port = use_signal(|| email.port.map(|p| p.to_string()).unwrap_or_default());
    let mut username = use_signal(|| email.username.clone().unwrap_or_default());
    let mut from = use_signal(|| email.from.clone().unwrap_or_default());
    let mut password = use_signal(String::new);

    rsx! {
        div { class: "glass-card",
            h4 { "Email (SMTP)" }
            div { class: "field-row",
                label { "Host" }
                input { class: "input", value: "{host}", oninput: move |e| host.set(e.value()) }
            }
            div { class: "field-row",
                label { "Port" }
                input { class: "input", value: "{port}", oninput: move |e| port.set(e.value()) }
            }
            div { class: "field-row",
                label { "Username" }
                input { class: "input", value: "{username}", oninput: move |e| username.set(e.value()) }
            }
            div { class: "field-row",
                label { "From" }
                input { class: "input", value: "{from}", oninput: move |e| from.set(e.value()) }
            }
            p { class: "label-tech",
                {if email.password.configured { "Password: configured" } else { "Password: not set" }}
            }
            input { class: "input", placeholder: "New password (leave blank to keep current)",
                r#type: "password", value: "{password}",
                oninput: move |e| password.set(e.value()) }
            button {
                class: "btn btn-primary btn-xs",
                onclick: move |_| {
                    let h = host();
                    let p = port().trim().to_string();
                    let u = username();
                    let f = from();
                    let pw = password();
                    // final-review finding #4: `.parse::<u16>().ok()` turns
                    // an unparseable or out-of-range port (e.g. "5877x",
                    // "99999") into `None`, which the write path reads as
                    // "don't touch this field" — Save then reports success
                    // while the port the operator actually typed was
                    // silently dropped. A non-empty field that fails to
                    // parse is refused client-side, before the query is
                    // even sent, rather than being treated as "unchanged".
                    let port_value = if p.is_empty() {
                        None
                    } else {
                        match p.parse::<u16>() {
                            Ok(n) => Some(n),
                            Err(_) => {
                                notify_config_ui.write().notice = Some((
                                    false,
                                    format!(
                                        "Port {p:?} is not a valid port number (expected 1-65535) — Save was not sent."
                                    ),
                                ));
                                return;
                            }
                        }
                    };
                    ws.send(set_email_config_query(
                        if h.trim().is_empty() { None } else { Some(h.trim().to_string()) },
                        port_value,
                        None,
                        if u.trim().is_empty() { None } else { Some(u.trim().to_string()) },
                        if pw.trim().is_empty() { None } else { Some(pw.trim().to_string()) },
                        if f.trim().is_empty() { None } else { Some(f.trim().to_string()) },
                    ));
                    password.set(String::new());
                },
                "Save"
            }
        }
    }
}

/// POLISH_WAVES.md sub-project 7 plan 3 — `[memory] profile` picker.
/// Keyed on the seed data by its caller (like `EmailAdapterCard`), so a
/// changed on-disk value re-seeds the form instead of leaving stale
/// local state.
#[component]
fn MemoryProfileCard(config: MemoryProfileConfigView) -> Element {
    let ws = use_context::<Sender>();
    let mut profile = use_signal(|| config.profile.clone());

    rsx! {
        div { class: "glass-card settings-section",
            div { class: "panel-head", h3 { "Memory profile" } span { class: "chip", "{config.profile}" } }
            p { class: "label-tech",
                "Off: today's behavior. Lite: recall fusion over existing memory, no paid \
                 generation. Smart: adds the wiki/graph extraction sweeps. Takes effect on \
                 the next daemon restart."
            }
            div { class: "field-row",
                label { "Profile" }
                select {
                    class: "input",
                    value: "{profile}",
                    onchange: move |e| profile.set(e.value()),
                    option { value: "off", "off" }
                    option { value: "lite", "lite" }
                    option { value: "smart", "smart" }
                }
            }
            button {
                class: "btn btn-primary btn-xs",
                onclick: move |_| ws.send(set_memory_profile_query(profile())),
                "Save"
            }
        }
    }
}

/// POLISH_WAVES.md sub-project 7 plan 3 — `[embedding]`'s primary
/// fields. `api_key`'s masked "configured"/"not set" + blank-input UX
/// mirrors `EmailAdapterCard`'s password field exactly.
#[component]
fn EmbeddingConfigCard(config: EmbeddingConfigView) -> Element {
    let ws = use_context::<Sender>();
    let mut base_url = use_signal(|| config.base_url.clone().unwrap_or_default());
    let mut model = use_signal(|| config.model.clone().unwrap_or_default());
    let mut api_key = use_signal(String::new);

    rsx! {
        div { class: "glass-card settings-section",
            div { class: "panel-head", h3 { "Embedding" } }
            p { class: "label-tech",
                "Configures the OpenAI-compatible embedding backend that powers semantic memory \
                 search. Point base_url at a local server to keep memory content on this box. \
                 Takes effect on the next daemon restart."
            }
            div { class: "field-row",
                label { "Base URL" }
                input { class: "input", placeholder: "https://api.openai.com", value: "{base_url}", oninput: move |e| base_url.set(e.value()) }
            }
            div { class: "field-row",
                label { "Model" }
                input { class: "input", placeholder: "text-embedding-3-small", value: "{model}", oninput: move |e| model.set(e.value()) }
            }
            p { class: "label-tech",
                {if config.api_key.configured { "API key: configured" } else { "API key: not set" }}
            }
            input { class: "input", placeholder: "New API key (leave blank to keep current)",
                r#type: "password", value: "{api_key}",
                oninput: move |e| api_key.set(e.value()) }
            button {
                class: "btn btn-primary btn-xs",
                onclick: move |_| {
                    let b = base_url();
                    let m = model();
                    let k = api_key();
                    ws.send(set_embedding_config_query(
                        if b.trim().is_empty() { None } else { Some(b.trim().to_string()) },
                        if m.trim().is_empty() { None } else { Some(m.trim().to_string()) },
                        if k.trim().is_empty() { None } else { Some(k.trim().to_string()) },
                    ));
                    api_key.set(String::new());
                },
                "Save"
            }
        }
    }
}

/// POLISH_WAVES.md sub-project 7 plan 3 — `[proactive]`'s primary
/// fields. `targets` is the live `[[notify_target]]` list (plan 2),
/// rendered as a `<select>` so an invalid target is unreachable through
/// this form — the write path (`write_proactive_section`) still checks
/// independently, per this sub-project's defense-in-depth precedent.
/// The picker also filters out disabled targets (falling back to showing
/// the currently-saved one, greyed, if it happens to be disabled) so a
/// target that would be rejected at write time can't be picked here.
///
/// Known limitation: `targets` only reflects target state as of page
/// load / last refresh. If a `[[notify_target]]` is disabled or deleted
/// through the Notifications screen in another tab/session, this card
/// does not retroactively invalidate an already-saved `[proactive].target`
/// — that only gets caught the next time the operator revisits and
/// re-saves this card (see `write_proactive_section`'s own doc comment
/// for the matching server-side deferral).
#[component]
fn ProactiveConfigCard(config: ProactiveConfigView, targets: Vec<NotifyTargetConfigView>) -> Element {
    let ws = use_context::<Sender>();
    let mut settings = use_context::<Signal<SettingsState>>();
    let mut enabled = use_signal(|| config.enabled);
    let mut target = use_signal(|| config.target.clone().unwrap_or_default());
    let mut max_per_window = use_signal(|| config.max_per_window.to_string());
    let mut window_secs = use_signal(|| config.window_secs.to_string());

    rsx! {
        div { class: "glass-card settings-section",
            div { class: "panel-head", h3 { "Proactive surfacing" } span { class: "chip", if config.enabled { "on" } else { "off" } } }
            p { class: "label-tech",
                "The assistant reaching out unprompted (e.g. a due reminder). Off unless enabled \
                 and a target is picked. Hard-capped by max sends per window. Takes effect on \
                 the next daemon restart."
            }
            div { class: "field-row",
                label { "Enabled" }
                input { r#type: "checkbox", checked: enabled(), onchange: move |e| enabled.set(e.checked()) }
            }
            div { class: "field-row",
                label { "Target" }
                select {
                    class: "input",
                    value: "{target}",
                    onchange: move |e| target.set(e.value()),
                    option { value: "", "— choose a notify target —" }
                    for t in targets.iter().filter(|t| t.enabled || t.name == target()) {
                        option {
                            value: "{t.name}",
                            disabled: !t.enabled,
                            if t.enabled { "{t.name}" } else { "{t.name} (disabled)" }
                        }
                    }
                }
            }
            div { class: "field-row",
                label { "Max sends per window" }
                input { class: "input", value: "{max_per_window}", oninput: move |e| max_per_window.set(e.value()) }
            }
            div { class: "field-row",
                label { "Window (seconds)" }
                input { class: "input", value: "{window_secs}", oninput: move |e| window_secs.set(e.value()) }
            }
            button {
                class: "btn btn-primary btn-xs",
                onclick: move |_| {
                    let t = target();
                    let mpw_raw = max_per_window().trim().to_string();
                    let secs_raw = window_secs().trim().to_string();
                    // Mirrors EmailAdapterCard's port-parsing guard
                    // (plan 2 final-review finding #4): a non-empty
                    // field that fails to parse must block Save, not be
                    // silently treated as "leave unchanged."
                    let mpw = if mpw_raw.is_empty() {
                        None
                    } else {
                        match mpw_raw.parse::<u32>() {
                            Ok(n) => Some(n),
                            Err(_) => {
                                settings.write().notice =
                                    Some((false, format!("{mpw_raw:?} is not a valid whole number — Save was not sent.")));
                                return;
                            }
                        }
                    };
                    let ws_secs = if secs_raw.is_empty() {
                        None
                    } else {
                        match secs_raw.parse::<u64>() {
                            Ok(n) => Some(n),
                            Err(_) => {
                                settings.write().notice = Some((
                                    false,
                                    format!("{secs_raw:?} is not a valid whole number of seconds — Save was not sent."),
                                ));
                                return;
                            }
                        }
                    };
                    ws.send(set_proactive_config_query(
                        Some(enabled()),
                        if t.trim().is_empty() { None } else { Some(t.trim().to_string()) },
                        mpw,
                        ws_secs,
                    ));
                },
                "Save"
            }
        }
    }
}

#[component]
fn ChannelAdaptersSection(state: NotificationsState) -> Element {
    let ws = use_context::<Sender>();
    let mut telegram_token = use_signal(String::new);
    let mut discord_token = use_signal(String::new);
    let mut slack_bot_token = use_signal(String::new);
    let mut slack_app_token = use_signal(String::new);

    rsx! {
        section { class: "panel",
            div { class: "panel-head", h3 { "Channel adapters" } }
            if let Some(email) = &state.email {
                EmailAdapterCard { key: "{email:?}", email: email.clone() }
            }
            if let Some(tg) = &state.telegram {
                div { class: "glass-card",
                    h4 { "Telegram" }
                    p { class: "label-tech",
                        {if tg.token.configured { "Token: configured" } else { "Token: not set" }}
                    }
                    input { class: "input", placeholder: "New bot token (leave blank to keep current)",
                        r#type: "password", value: "{telegram_token}",
                        oninput: move |e| telegram_token.set(e.value()) }
                    button {
                        class: "btn btn-primary btn-xs",
                        onclick: move |_| {
                            let t = telegram_token();
                            ws.send(set_telegram_config_query(
                                if t.trim().is_empty() { None } else { Some(t.trim().to_string()) },
                                None,
                            ));
                            telegram_token.set(String::new());
                        },
                        "Save"
                    }
                }
            }
            if let Some(d) = &state.discord {
                div { class: "glass-card",
                    h4 { "Discord" }
                    p { class: "label-tech",
                        {if d.token.configured { "Token: configured" } else { "Token: not set" }}
                    }
                    input { class: "input", placeholder: "New bot token (leave blank to keep current)",
                        r#type: "password", value: "{discord_token}",
                        oninput: move |e| discord_token.set(e.value()) }
                    button {
                        class: "btn btn-primary btn-xs",
                        onclick: move |_| {
                            let t = discord_token();
                            ws.send(set_discord_config_query(
                                if t.trim().is_empty() { None } else { Some(t.trim().to_string()) },
                                None,
                            ));
                            discord_token.set(String::new());
                        },
                        "Save"
                    }
                }
            }
            if let Some(s) = &state.slack {
                div { class: "glass-card",
                    h4 { "Slack" }
                    p { class: "label-tech",
                        {if s.bot_token.configured { "Bot token: configured" } else { "Bot token: not set" }}
                    }
                    input { class: "input", placeholder: "New bot token (leave blank to keep current)",
                        r#type: "password", value: "{slack_bot_token}",
                        oninput: move |e| slack_bot_token.set(e.value()) }
                    p { class: "label-tech",
                        {if s.app_token.configured { "App token: configured" } else { "App token: not set" }}
                    }
                    input { class: "input", placeholder: "New app token (leave blank to keep current)",
                        r#type: "password", value: "{slack_app_token}",
                        oninput: move |e| slack_app_token.set(e.value()) }
                    button {
                        class: "btn btn-primary btn-xs",
                        onclick: move |_| {
                            let bt = slack_bot_token();
                            let at = slack_app_token();
                            ws.send(set_slack_config_query(
                                if bt.trim().is_empty() { None } else { Some(bt.trim().to_string()) },
                                if at.trim().is_empty() { None } else { Some(at.trim().to_string()) },
                                None,
                            ));
                            slack_bot_token.set(String::new());
                            slack_app_token.set(String::new());
                        },
                        "Save"
                    }
                }
            }
        }
    }
}

/// One notification-history row: outcome-colored dot, target, trigger
/// kind/id, relative time, and the outcome detail (error message /
/// skip reason) when present.
#[component]
fn NotificationHistoryRow(entry: NotificationHistoryEntry) -> Element {
    let tone = match entry.outcome_kind.as_str() {
        "delivered" => "live",
        "failed" => "off",
        _ => "off",
    };
    rsx! {
        div { class: "glass-card routine-row",
            div { class: "row1",
                span { class: if tone == "live" { "dot live" } else { "dot off" } }
                span { class: "name", "{entry.target_name}" }
                span { class: "label-tech", style: "opacity:0.7;", "[{entry.trigger_kind} · {entry.trigger_id}]" }
                span { class: "label-tech", "{entry.outcome_kind}" }
            }
            div { class: "row2 label-tech",
                span { "{rel_time(entry.dispatched_at_unix_ms)}" }
                if !entry.outcome_detail.is_empty() {
                    span { style: "opacity:0.8;", "{entry.outcome_detail}" }
                }
            }
        }
    }
}

// ── /classic retirement — the Sessions screen ───────────────────────────

/// One rendered row: channel/trust-tier badge, session id, age, last
/// active. `SessionSummary` fields are all `Copy`/cheap to read
/// directly — no separate row-view type needed.
#[component]
fn SessionRow(entry: SessionSummary) -> Element {
    rsx! {
        div { class: "glass-card routine-row",
            div { class: "row1",
                span { class: "dot live" }
                span { class: "name", "{entry.session_id}" }
                span { class: "label-tech", style: "opacity:0.7;", "[{entry.channel:?} · {entry.trust_tier:?}]" }
            }
            div { class: "row2 label-tech",
                span { "created {rel_time(entry.created_at_ms)}" }
                span { style: "opacity:0.8;", "active {rel_time(entry.last_active_at_ms)}" }
            }
        }
    }
}

#[component]
fn SessionsPanel() -> Element {
    let ws = use_context::<Sender>();
    let sessions = use_context::<Signal<SessionsState>>();

    // Load the current session list each time the view opens.
    use_future(move || async move {
        ws.send(FrontendMessage::Query {
            id: "sessions-page".to_string(),
            payload: QueryPayload::ListSessions,
        });
    });

    let state = sessions();
    let mut rows = state.sessions.clone();
    rows.sort_by_key(|a| std::cmp::Reverse(a.last_active_at_ms));

    rsx! {
        div { class: "dash-grid",
            div { class: "dash-main",
                section { class: "panel",
                    div { class: "panel-head",
                        h3 { "Active sessions" }
                        span { class: "label-tech", "{rows.len()} connected" }
                    }
                    if rows.is_empty() {
                        div { class: "glass-card empty",
                            p { class: "label-tech", "No active sessions." }
                        }
                    } else {
                        div { class: "feed",
                            for s in rows.iter() {
                                SessionRow { entry: s.clone() }
                            }
                        }
                    }
                }
            }
        }
    }
}

// ── /classic retirement — the dedicated Audit screen ────────────────────

/// `/classic` retirement — the dedicated, paginated Audit screen.
/// Reuses the existing AuditFeed row-renderer (built for the Command
/// Center's short tail) rather than a second copy of the same markup.
const AUDIT_PAGE_SIZE: u32 = 50;

#[component]
fn AuditPanel() -> Element {
    let ws = use_context::<Sender>();
    let mut audit = use_context::<Signal<AuditState>>();
    let dashboard = use_context::<Signal<Dashboard>>();

    // Load the newest page on mount. This blind guess can't know the
    // chain's true `total_len` before this first response arrives — it's
    // computed from whatever `total_len` happens to be cached from a
    // previous visit (or 0, on the very first visit ever), which on a
    // chain longer than one page (or one that has grown since the last
    // visit) is generally wrong. `pending_guess = true` marks this specific
    // request as an unverified guess so the "audit-page" response handler
    // (in `read_task`) can check it against the real `total_len` once the
    // response lands, and — via `App`'s effect — fire exactly one
    // corrective follow-up if needed. Setting `pending_guess = true` here,
    // on every mount, is what makes that correction available every time
    // this panel is (re-)opened, not just the first time ever.
    use_future(move || async move {
        let total = audit().total_len;
        let from_seq = total.saturating_sub(AUDIT_PAGE_SIZE as u64);
        // Request-side bookkeeping: `AuditState.from_seq` always reflects
        // "what we last asked for" (set here, at mount, and again at each
        // Older/Newer click below) — never inferred from the response.
        {
            let mut a = audit.write();
            a.from_seq = from_seq;
            a.pending_guess = true;
        }
        ws.send(FrontendMessage::Query {
            id: "audit-page".to_string(),
            payload: QueryPayload::ListAuditEntries { from_seq, limit: AUDIT_PAGE_SIZE },
        });
    });

    let state = audit();
    let chain_ok = dashboard().chain_ok;

    let at_oldest = state.from_seq == 0;
    let at_newest = state.from_seq + AUDIT_PAGE_SIZE as u64 >= state.total_len;
    let window_end = (state.from_seq + AUDIT_PAGE_SIZE as u64).min(state.total_len);
    let window_start_display = if state.total_len == 0 { 0 } else { state.from_seq + 1 };

    rsx! {
        div { class: "dash-grid",
            div { class: "dash-main",
                section { class: "panel",
                    div { class: "panel-head",
                        h3 { "Audit chain" }
                        span { class: "label-tech", "{state.total_len} total events" }
                    }
                    div { class: "glass-card", style: "margin-bottom:12px;",
                        button {
                            onclick: move |_| ws.send(FrontendMessage::Query {
                                id: "mc-verify".to_string(),
                                payload: QueryPayload::VerifyAuditChain,
                            }),
                            "Verify chain"
                        }
                        match chain_ok {
                            Some(true) => rsx! { span { style: "color: var(--ok, #16a34a); margin-left:8px;", span { class: "dial-glyph", style: "border-color: var(--ok, #16a34a);" } "chain intact" } },
                            Some(false) => rsx! { span { style: "color: var(--danger, #b91c1c); margin-left:8px;", span { class: "dial-glyph", style: "border-color: var(--danger, #b91c1c);" } "chain verification failed" } },
                            None => rsx! { span {} },
                        }
                    }
                    div {
                        class: "glass-card",
                        style: "margin-bottom:12px; display:flex; align-items:center; gap:12px;",
                        button {
                            class: "btn btn-ghost",
                            disabled: at_oldest,
                            onclick: move |_| {
                                let from_seq = audit().from_seq.saturating_sub(AUDIT_PAGE_SIZE as u64);
                                {
                                    let mut a = audit.write();
                                    a.from_seq = from_seq;
                                    // Deliberate operator action, not a
                                    // guess — must never be overridden by a
                                    // delayed auto-correction meant for an
                                    // earlier, still-in-flight mount guess.
                                    a.pending_guess = false;
                                    a.pending_correction = None;
                                }
                                ws.send(FrontendMessage::Query {
                                    id: "audit-page".to_string(),
                                    payload: QueryPayload::ListAuditEntries { from_seq, limit: AUDIT_PAGE_SIZE },
                                });
                            },
                            "← Older"
                        }
                        button {
                            class: "btn btn-ghost",
                            disabled: at_newest,
                            onclick: move |_| {
                                let a = audit();
                                let newest_from_seq = a.total_len.saturating_sub(AUDIT_PAGE_SIZE as u64);
                                let from_seq = (a.from_seq + AUDIT_PAGE_SIZE as u64).min(newest_from_seq);
                                {
                                    let mut a = audit.write();
                                    a.from_seq = from_seq;
                                    // Same reasoning as "← Older" above.
                                    a.pending_guess = false;
                                    a.pending_correction = None;
                                }
                                ws.send(FrontendMessage::Query {
                                    id: "audit-page".to_string(),
                                    payload: QueryPayload::ListAuditEntries { from_seq, limit: AUDIT_PAGE_SIZE },
                                });
                            },
                            "Newer →"
                        }
                        span { class: "label-tech", "Showing {window_start_display}–{window_end} of {state.total_len}" }
                    }
                    AuditFeed { entries: state.entries.clone() }
                }
            }
            aside { class: "dash-rail",
                section { class: "panel",
                    div { class: "panel-head", h3 { "About" } }
                    div { class: "glass-card",
                        p { class: "label-tech",
                            "Every allowed or denied action, HMAC-chained and offline-verifiable. This screen shows {AUDIT_PAGE_SIZE} events at a time — use Older/Newer to page through the chain. The Command Center's own short tail is separate and always shows the very latest few."
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn AgentStatus(
    name: Option<String>,
    connected: bool,
    chain_ok: Option<bool>,
    settings: Option<SettingsSnapshot>,
) -> Element {
    let agent = name.unwrap_or_else(|| "—".to_string());
    let chain_class = match chain_tone(chain_ok) {
        Some(t) => format!("v {t}"),
        None => "v".to_string(),
    };
    let chain = chain_label(chain_ok);
    // Live agent vitals from the running snapshot (GetSettings). Precomputed so
    // the rsx stays declarative.
    let has_vitals = settings.is_some();
    let (model, provider, ctx, autonomy, access) = match &settings {
        Some(s) => {
            let ctx = match s.num_ctx {
                Some(n) if n % 1024 == 0 => format!("{}k tok", n / 1024),
                Some(n) => format!("{n} tok"),
                None => "auto".to_string(),
            };
            (
                s.model.clone(),
                s.provider.clone(),
                ctx,
                s.autonomy_level.clone(),
                s.access_level.clone(),
            )
        }
        None => (
            "—".to_string(),
            "—".to_string(),
            "—".to_string(),
            "—".to_string(),
            "—".to_string(),
        ),
    };
    rsx! {
        section { class: "glass-card agent-status",
            div { class: "panel-head", h3 { "Agent" } }
            div { class: "kv",
                span { class: "label-tech", "Name" }
                span { class: "v", "{agent}" }
            }
            div { class: "kv",
                span { class: "label-tech", "Daemon" }
                span { class: if connected { "v ok" } else { "v off" },
                    if connected { span { class: "dot live" } }
                    if connected { "online" } else { "offline" }
                }
            }
            if has_vitals {
                div { class: "kv",
                    span { class: "label-tech", "Model" }
                    span { class: "v mono", "{model}" }
                }
                div { class: "kv",
                    span { class: "label-tech", "Provider" }
                    span { class: "v", "{provider} · {ctx}" }
                }
                div { class: "kv",
                    span { class: "label-tech", "Autonomy" }
                    span { class: "v", "{autonomy}" }
                }
                div { class: "kv",
                    span { class: "label-tech", "Access" }
                    span { class: "v", "{access}" }
                }
            }
            div { class: "kv",
                span { class: "label-tech", "Chain" }
                span { class: "{chain_class}", "{chain}" }
            }
        }
    }
}

/// "Secure" / "FAILED" / "…" for the chain status.
fn chain_label(ok: Option<bool>) -> &'static str {
    match ok {
        Some(true) => "Secure",
        Some(false) => "FAILED",
        None => "…",
    }
}

fn chain_tone(ok: Option<bool>) -> Option<&'static str> {
    match ok {
        Some(true) => Some("ok"),
        Some(false) => Some("off"),
        None => None,
    }
}

/// Relative time from a unix-ms timestamp, using the browser clock.
fn rel_time(ms: u64) -> String {
    let now = js_sys::Date::now() as u64;
    if ms == 0 || ms >= now {
        return "now".to_string();
    }
    let secs = (now - ms) / 1000;
    if secs < 60 {
        format!("{secs}s ago")
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86_400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86_400)
    }
}

// ---------------------------------------------------------------------------
// Missions view — the orchestration look
// ---------------------------------------------------------------------------

#[component]
fn MissionsPanel(missions: Vec<TeamMissionView>) -> Element {
    rsx! {
        NewMissionBar {}
        div { class: "feed",
            if missions.is_empty() {
                div { class: "empty card",
                    p { "No team missions yet." }
                    p { class: "label-tech", "Start one above to dispatch the Nonagon." }
                }
            } else {
                for m in missions.iter() {
                    MissionRow { mission: m.clone() }
                }
            }
        }
    }
}

#[component]
fn NewMissionBar() -> Element {
    let ws = use_context::<Sender>();
    let connected = use_context::<Signal<bool>>();
    let mut goal = use_signal(String::new);
    let ready = connected();
    rsx! {
        div { class: "newbar",
            input {
                class: "input",
                "aria-label": "New mission goal",
                placeholder: if ready { "new mission goal — e.g. \"audit the deps for CVEs\"" } else { "reconnecting…" },
                disabled: !ready,
                value: "{goal}",
                oninput: move |e| goal.set(e.value()),
                onkeydown: move |e| {
                    if e.key() == Key::Enter {
                        let g = goal().trim().to_string();
                        if !g.is_empty() { ws.send(start_query(g)); goal.set(String::new()); }
                    }
                },
            }
            button {
                class: "btn btn-primary",
                disabled: !ready,
                onclick: move |_| {
                    let g = goal().trim().to_string();
                    if !g.is_empty() { ws.send(start_query(g)); goal.set(String::new()); }
                },
                "Run"
            }
        }
    }
}

#[component]
fn MissionRow(mission: TeamMissionView) -> Element {
    let pct = mission.progress.min(100);
    let awaiting = mission.phase == TeamMissionPhase::AwaitingApproval;
    rsx! {
        div { class: "glass-card mission",
            div { class: "row1",
                span { class: "chip {phase_class(mission.phase)}", "{phase_label(mission.phase)}" }
                span { class: "goal", "{mission.goal}" }
                span { class: "lead label-tech", "{mission.lead}" }
            }
            // POLISH_WAVES.md sub-project 5, item A — the operator
            // previously saw REJECTED/HALTED with no explanation;
            // halt_reason already carries the judge's precise verdict
            // (or the halt cause) and already flows over the wire.
            if let Some(reason) = mission.halt_reason.as_ref() {
                div { class: "notice err", "{reason}" }
            }
            div { class: "progress", div { class: "fill", style: "width: {pct}%;" } }
            div { class: "steps",
                for step in mission.steps.iter() {
                    span { class: "step label-tech", "{step.label}" }
                }
            }
            if awaiting {
                if let Some(gate) = mission.pending_gate.clone() {
                    GateControls { mission_id: mission.id.clone(), step: gate, verify_attempts: mission.verify_attempts }
                }
            }
        }
    }
}

/// POLISH_WAVES.md sub-project 5, item B — the gate label, with attempt
/// context only when this isn't the first attempt (a first-attempt gate
/// needs no "(attempt 1)" noise). Extracted as a pure function so it's
/// testable without a Dioxus runtime.
fn gate_label(step: &str, verify_attempts: u32) -> String {
    // Final-review fix (POLISH_WAVES.md sub-project 5) — verify_attempts
    // counts FAILED verifications, so a value of 1 means the mission is
    // already on its second attempt. The original `> 1` threshold with a
    // bare `{verify_attempts}` display was both unreachable in practice
    // (MAX_MISSION_ATTEMPTS caps this at 1) and off-by-one even if it had
    // fired.
    if verify_attempts >= 1 {
        format!("⚑ awaiting approval — {step} (attempt {})", verify_attempts + 1)
    } else {
        format!("⚑ awaiting approval — {step}")
    }
}

#[component]
fn GateControls(mission_id: String, step: String, verify_attempts: u32) -> Element {
    let ws = use_context::<Sender>();
    let approve = (mission_id.clone(), step.clone());
    let reject = (mission_id.clone(), step.clone());
    let label = gate_label(&step, verify_attempts);
    rsx! {
        div { class: "gate",
            span { class: "gate-label", "{label}" }
            button {
                class: "btn btn-sage",
                onclick: move |_| ws.send(resolve_team_query(approve.0.clone(), approve.1.clone(), true)),
                "Approve Sequence"
            }
            button {
                class: "btn btn-ghost-danger",
                onclick: move |_| ws.send(resolve_team_query(reject.0.clone(), reject.1.clone(), false)),
                "Reject"
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Mission Control view — one active mission's LEAD/specialist graph
// (selector only in this task; the real graph is a later chapter task).
// ---------------------------------------------------------------------------

#[component]
fn MissionControlPanel(missions: Vec<TeamMissionView>, selected_mission: Signal<Option<String>>) -> Element {
    let ws = use_context::<Sender>();
    let teams = use_context::<Signal<TeamsState>>();
    let mut mission_ui = use_context::<Signal<MissionControlUi>>();
    use_effect(move || {
        ws.send(get_team_roster_query());
    });
    let mut selected_node = use_signal(|| None::<String>);
    let watchable = watchable_missions(&missions);
    let Some(current_id) = selected_mission() else {
        return rsx! {
            div { class: "mission-control",
                div { class: "panel-head", h3 { "Mission Control" } }
                if let Some((ok, msg)) = mission_ui().notice.clone() {
                    div { class: if ok { "notice ok" } else { "notice err" }, "{msg}" }
                }
                if watchable.is_empty() {
                    div { class: "empty card",
                        p { "No mission is currently executing, paused, or awaiting approval." }
                        p { class: "label-tech", "Start one from the Missions screen." }
                    }
                } else {
                    div { class: "mission-picker",
                        for m in watchable.iter() {
                            button {
                                class: "glass-card mission-pick",
                                key: "{m.id}",
                                onclick: {
                                    let id = m.id.clone();
                                    move |_| {
                                        mission_ui.write().notice = None;
                                        selected_mission.set(Some(id.clone()));
                                    }
                                },
                                span { class: "chip {phase_class(m.phase)}", "{phase_label(m.phase)}" }
                                span { class: "goal", "{m.goal}" }
                                span { class: "lead label-tech", "{m.lead}" }
                            }
                        }
                    }
                }
            }
        };
    };
    // If the selected mission is no longer watchable (finished/halted
    // while this view was open), fall back to the selector rather than
    // showing a stale/missing graph.
    let Some(current) = watchable.iter().find(|m| m.id == current_id) else {
        selected_mission.set(None);
        return rsx! { div { class: "mission-control", "…" } };
    };
    let Some(roster) = teams().roster else {
        return rsx! {
            div { class: "mission-control",
                div { class: "panel-head",
                    h3 { "Mission Control" }
                    button {
                        class: "btn btn-ghost",
                        onclick: move |_| {
                            mission_ui.write().notice = None;
                            selected_mission.set(None);
                        },
                        "← All missions"
                    }
                }
                SkeletonList { rows: 3 }
            }
        };
    };
    let graph = build_mission_graph(current, &roster);
    rsx! {
        div { class: "mission-control",
            div { class: "panel-head",
                h3 { "Mission Control" }
                button {
                    class: "btn btn-ghost",
                    onclick: move |_| {
                        mission_ui.write().notice = None;
                        selected_mission.set(None);
                    },
                    "← All missions"
                }
            }
            if let Some((ok, msg)) = mission_ui().notice.clone() {
                div { class: if ok { "notice ok" } else { "notice err" }, "{msg}" }
            }
            div { class: "row1",
                span { class: "chip {phase_class(current.phase)}", "{phase_label(current.phase)}" }
                span { class: "goal", "{current.goal}" }
            }
            if graph.nodes.is_empty() {
                div { class: "empty card",
                    p { "No roster members to show yet." }
                }
            } else {
                MissionGraphSvg {
                    graph: graph.clone(),
                    selected: selected_node(),
                    on_select: move |name: String| selected_node.set(Some(name)),
                }
            }
            MissionControls { mission: (*current).clone() }
            if let Some(name) = selected_node() {
                if let Some(node) = graph.nodes.iter().find(|n| n.name == name) {
                    SpecialistDrillIn { node: node.clone(), roster: roster.clone(), mission: (*current).clone() }
                }
            }
        }
    }
}

/// Chapter Mission Control — one mission's LEAD/specialist graph as a
/// directed SVG, mirroring `LatticeGraph`'s node/edge/arrow conventions
/// (Chapter MG) with `layout_mission_nodes`'s deterministic LEAD-centric
/// ring in place of a force simulation.
#[component]
fn MissionGraphSvg(graph: MissionGraph, selected: Option<String>, on_select: EventHandler<String>) -> Element {
    let pos = layout_mission_nodes(&graph.nodes);
    let idx: HashMap<&str, usize> = graph.nodes.iter().enumerate().map(|(i, n)| (n.name.as_str(), i)).collect();
    rsx! {
        div { class: "glass-card mission-graph",
            svg {
                class: "mission-graph-svg",
                view_box: "0 0 {MC_GRAPH_W} {MC_GRAPH_H}",
                defs {
                    marker {
                        id: "mission-arrow", view_box: "0 0 10 10",
                        ref_x: "9", ref_y: "5", marker_width: "7", marker_height: "7",
                        orient: "auto-start-reverse",
                        path { d: "M 0 0 L 10 5 L 0 10 z", class: "mission-arrowhead" }
                    }
                }
                // Edges first (under the nodes). A step's dependency edge
                // between two steps owned by the SAME specialist isn't a
                // meaningful cross-node line, so it's skipped.
                for e in graph.edges.iter() {
                    if e.from_member != e.to_member {
                        if let (Some(&i), Some(&j)) = (idx.get(e.from_member.as_str()), idx.get(e.to_member.as_str())) {
                            {
                                let (x1, y1) = pos[i];
                                let (x2c, y2c) = pos[j];
                                let dx = x2c - x1;
                                let dy = y2c - y1;
                                let d = (dx * dx + dy * dy).sqrt().max(0.01);
                                let (x2, y2) =
                                    (x2c - dx / d * (MC_NODE_R + 4.0), y2c - dy / d * (MC_NODE_R + 4.0));
                                rsx! {
                                    line {
                                        x1: "{x1}", y1: "{y1}", x2: "{x2}", y2: "{y2}",
                                        class: "mission-edge", marker_end: "url(#mission-arrow)",
                                    }
                                }
                            }
                        }
                    }
                }
                // Nodes.
                for (i, node) in graph.nodes.iter().enumerate() {
                    {
                        let (cx, cy) = pos[i];
                        let state_class = match node.state {
                            TeamStepState::Running | TeamStepState::Awaiting => "warning",
                            TeamStepState::Rejected => "error",
                            TeamStepState::Done => "success",
                            TeamStepState::Pending => "",
                        };
                        let mut classes = format!("mission-node {state_class}");
                        if node.is_lead { classes.push_str(" lead"); }
                        if !node.on_roster { classes.push_str(" off-roster"); }
                        if selected.as_deref() == Some(node.name.as_str()) { classes.push_str(" selected"); }
                        let name = node.name.clone();
                        rsx! {
                            g { class: "{classes}",
                                onclick: move |_| on_select.call(name.clone()),
                                circle { cx: "{cx}", cy: "{cy}", r: "{MC_NODE_R}" }
                                text { x: "{cx}", y: "{cy + MC_NODE_R + 13.0}", text_anchor: "middle", "{node.name}" }
                                if node.is_lead {
                                    text {
                                        x: "{cx}", y: "{cy - MC_NODE_R - 8.0}", text_anchor: "middle",
                                        class: "mission-node-badge", "LEAD"
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn MissionControls(mission: TeamMissionView) -> Element {
    let ws = use_context::<Sender>();
    let mut mission_ui = use_context::<Signal<MissionControlUi>>();
    let id = mission.id.clone();
    let shown = controls_for_phase(mission.phase);
    rsx! {
        div { class: "mission-controls",
            if shown.contains(&"gate") {
                if let Some(gate) = mission.pending_gate.clone() {
                    GateControls { mission_id: mission.id.clone(), step: gate, verify_attempts: mission.verify_attempts }
                }
            }
            if shown.contains(&"pause") {
                button {
                    class: "btn btn-ghost",
                    onclick: {
                        let id = id.clone();
                        move |_| {
                            mission_ui.write().notice = None;
                            ws.send(pause_team_mission_query(id.clone()));
                        }
                    },
                    "Pause"
                }
            }
            if shown.contains(&"abort") {
                button {
                    class: "btn btn-ghost-danger",
                    onclick: {
                        let id = id.clone();
                        move |_| {
                            mission_ui.write().notice = None;
                            ws.send(abort_team_mission_query(id.clone()));
                        }
                    },
                    "Abort"
                }
            }
            if shown.contains(&"resume") {
                button {
                    class: "btn btn-sage",
                    onclick: move |_| {
                        mission_ui.write().notice = None;
                        ws.send(resume_team_mission_query(id.clone()));
                    },
                    "Resume"
                }
            }
        }
    }
}

/// Chapter Mission Control — which controls a mission's current phase
/// shows, as opaque tags. `MissionControls`'s own rsx! branches on these
/// tags directly (see its body) rather than re-checking `mission.phase`,
/// so the two can never drift apart; also exercised standalone by
/// `mission_controls_shown_for_each_phase` (`#[cfg(test)]`, below)
/// without needing a Dioxus runtime.
fn controls_for_phase(phase: TeamMissionPhase) -> Vec<&'static str> {
    match phase {
        TeamMissionPhase::Executing => vec!["pause", "abort"],
        TeamMissionPhase::Paused => vec!["resume"],
        TeamMissionPhase::AwaitingApproval => vec!["gate"],
        TeamMissionPhase::Planning
        | TeamMissionPhase::Done
        | TeamMissionPhase::Rejected
        | TeamMissionPhase::Halted => vec![],
    }
}

/// Chapter Mission Control / Chapter Y — `lead`'s own declared capability
/// scopes within `team`, for the NT-02 "inert" hint: a specialist's own
/// declared scope the lead doesn't also hold is attenuated to nothing at
/// spawn (never granted). `None` if `lead` isn't a member of `team` at
/// all — distinct from `Some(<empty set>)` (a real lead with zero
/// declared scopes, which correctly flags every specialist scope as
/// inert). An unknown lead should suppress the hint rather than
/// manufacture a guaranteed false alarm — see `SpecialistDrillIn`.
fn scopes_of(team: &TeamConfig, lead: &str) -> Option<HashSet<String>> {
    team.members.iter().find(|m| m.name == lead).map(|m| m.capability_scopes.iter().cloned().collect())
}

/// Chapter Y — `TeamsPanel`'s own lead-scope lookup. `team.lead` is
/// always expected to be a real member of `team` (a `TeamConfig`
/// invariant `TeamsPanel` itself enforces on save), so this collapses
/// `scopes_of`'s `Option` to an empty set on the should-be-impossible
/// miss rather than surfacing it. Extracted from `TeamsPanel`'s own
/// inline computation so both surfaces share one implementation, not two
/// that could silently drift apart.
fn lead_scopes(team: &TeamConfig) -> HashSet<String> {
    scopes_of(team, &team.lead).unwrap_or_default()
}

#[component]
fn SpecialistDrillIn(node: MissionGraphNode, roster: TeamConfig, mission: TeamMissionView) -> Element {
    // The mission's OWN lead, not the current default roster's lead --
    // a pack-pinned mission's lead can differ from `roster.lead` (see
    // `scopes_of`'s own doc comment).
    let scopes = scopes_of(&roster, &mission.lead);
    let member = roster.members.iter().find(|m| m.name == node.name);
    let declared: Vec<String> = member.map(|m| m.capability_scopes.clone()).unwrap_or_default();
    let inert: Vec<String> = match &scopes {
        Some(s) => declared.iter().filter(|scope| !s.contains(*scope)).cloned().collect(),
        // The mission's own lead isn't in this (possibly wrong) roster at
        // all -- we have no real basis to say anything is inert, so stay
        // silent rather than flag every declared scope as a false alarm.
        None => Vec::new(),
    };
    let current_step_detail = node
        .current_step
        .as_ref()
        .and_then(|id| mission.steps.iter().find(|s| &s.step_id == id));
    rsx! {
        div { class: "glass-card drill-in",
            div { class: "row1",
                span { class: "goal", "{node.name}" }
                if node.is_lead { span { class: "chip", "LEAD" } }
            }
            if let Some(step) = current_step_detail {
                p { class: "label-tech", "currently running: {step.label}" }
            } else {
                p { class: "label-tech", "idle — no step currently running" }
            }
            if !declared.is_empty() {
                div { class: "scopes",
                    p { class: "label-tech", "declared capability scopes:" }
                    for s in declared.iter() {
                        span { class: "chip", "{s}" }
                    }
                }
            }
            if !inert.is_empty() {
                p { class: "label-tech inert-hint",
                    "Lead lacks {inert.join(\", \")} — inert until the lead holds them (attenuated at spawn)."
                }
            }
            if !node.on_roster {
                p { class: "label-tech roster-hint",
                    "Not on the daemon's current default roster — this mission's own team config pinned a different roster."
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Chat view — the terminal look
// ---------------------------------------------------------------------------

#[component]
fn ChatPanel() -> Element {
    let ws = use_context::<Sender>();
    let session = use_context::<Signal<Option<String>>>();
    let mut transcript = use_context::<Signal<Vec<ChatLine>>>();
    let streaming = use_context::<Signal<String>>();
    let gate = use_context::<Signal<Option<GateInfo>>>();
    let mut input = use_signal(String::new);
    let ready = session().is_some();

    rsx! {
        div { class: "chat",
            div { class: "transcript",
                for line in transcript().iter() {
                    div { class: "{line.class()}", "{line.text}" }
                }
                if !streaming().is_empty() {
                    div { class: "line asst streaming", "{streaming}" }
                }
                if transcript().is_empty() && streaming().is_empty() {
                    p { class: "empty label-tech", "Send a message to start a turn." }
                }
            }
            if let Some(g) = gate() {
                GatePrompt { gate: g }
            } else {
                div { class: "composer",
                    input {
                        class: "input",
                        "aria-label": "Message",
                        placeholder: if ready { "message…" } else { "connecting…" },
                        disabled: !ready,
                        value: "{input}",
                        oninput: move |e| input.set(e.value()),
                        onkeydown: move |e| {
                            if e.key() == Key::Enter
                                && let Some(sid) = session() {
                                let text = input().trim().to_string();
                                if !text.is_empty() {
                                    transcript.write().push(ChatLine::operator(text.clone()));
                                    ws.send(submit_query(sid, text));
                                    input.set(String::new());
                                }
                            }
                        },
                    }
                    button {
                        class: "btn btn-primary",
                        disabled: !ready,
                        onclick: move |_| {
                            if let Some(sid) = session() {
                                let text = input().trim().to_string();
                                if !text.is_empty() {
                                    transcript.write().push(ChatLine::operator(text.clone()));
                                    ws.send(submit_query(sid, text));
                                    input.set(String::new());
                                }
                            }
                        },
                        "Send"
                    }
                }
            }
        }
    }
}

#[component]
fn GatePrompt(gate: GateInfo) -> Element {
    let ws = use_context::<Sender>();
    let mut gate_sig = use_context::<Signal<Option<GateInfo>>>();
    let approve = (gate.mission_id.clone(), gate.gate_id.clone());
    let reject = (gate.mission_id.clone(), gate.gate_id.clone());
    rsx! {
        div { class: "glass-card gateprompt",
            span { class: "gate-label", "⚑ approval needed — {gate.reason}" }
            button {
                class: "btn btn-sage",
                onclick: move |_| {
                    ws.send(resolve_gate_query(approve.0.clone(), approve.1.clone(), true));
                    gate_sig.set(None);
                },
                "Approve"
            }
            button {
                class: "btn btn-ghost-danger",
                onclick: move |_| {
                    ws.send(resolve_gate_query(reject.0.clone(), reject.1.clone(), false));
                    gate_sig.set(None);
                },
                "Reject"
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Memory view — the self-learning knowledge browser (read-only)
// ---------------------------------------------------------------------------

#[component]
fn MemoryPanel() -> Element {
    let ws = use_context::<Sender>();
    let memory = use_context::<Signal<MemoryState>>();
    let memory_ui = use_context::<Signal<MemoryUi>>();
    let mut query = use_signal(String::new);
    let mut semantic = use_signal(|| false);
    // Active scope label: "recent" | "topic:<t>" | "search:<q>".
    let mut scope = use_signal(|| "recent".to_string());
    // MG — List ⇄ Graph view toggle (local; the graph data loads alongside).
    let mut graph_view = use_signal(|| false);

    // Load topics + the recent-across-all default + the graph each time the
    // view opens.
    use_future(move || async move {
        ws.send(mem_topics_query());
        ws.send(mem_search_query(String::new(), false));
        ws.send(mem_graph_query());
        ws.send(mem_conflicts_query());
    });

    let m = memory();
    rsx! {
        div { class: "mem",
            aside { class: "mem-rail",
                div { class: "panel-head", h3 { "Topics" } span { class: "label-tech", "{m.topics.len()}" } }
                button {
                    class: if scope() == "recent" { "mem-topic active" } else { "mem-topic" },
                    onclick: move |_| {
                        scope.set("recent".to_string());
                        ws.send(mem_search_query(String::new(), semantic()));
                    },
                    "Recent · all"
                }
                for t in m.topics.iter() {
                    {
                        let topic = t.clone();
                        let label = t.clone();
                        let sel = scope() == format!("topic:{t}");
                        let conflicted = m.conflicts.iter().any(|c| c.a.topic == topic || c.b.topic == topic);
                        rsx! {
                            button {
                                class: if sel { "mem-topic active" } else { "mem-topic" },
                                onclick: move |_| {
                                    scope.set(format!("topic:{topic}"));
                                    ws.send(mem_topic_query(topic.clone()));
                                },
                                if conflicted {
                                    span { class: "chip warning mem-topic-flag", title: "contradictory entries", "⚠" }
                                }
                                span { "{label}" }
                            }
                        }
                    }
                }
            }
            div { class: "mem-main",
                div { class: "mem-search",
                    input {
                        class: "input",
                        "aria-label": "Search memory",
                        placeholder: "search memory…",
                        value: "{query}",
                        oninput: move |e| query.set(e.value()),
                        onkeydown: move |e| {
                            if e.key() == Key::Enter {
                                let q = query();
                                scope.set(format!("search:{q}"));
                                ws.send(mem_search_query(q, semantic()));
                            }
                        },
                    }
                    button {
                        class: if semantic() { "btn btn-glass on" } else { "btn btn-glass" },
                        title: "Toggle keyword / semantic search",
                        onclick: move |_| semantic.toggle(),
                        if semantic() { "semantic" } else { "keyword" }
                    }
                    button {
                        class: "btn btn-primary",
                        onclick: move |_| {
                            let q = query();
                            scope.set(format!("search:{q}"));
                            ws.send(mem_search_query(q, semantic()));
                        },
                        "Search"
                    }
                }
                div { class: "panel-head",
                    h3 { "{scope_label(&scope())}" }
                    if m.fell_back {
                        span { class: "chip warning", "keyword fallback" }
                    }
                    div { class: "mem-viewtoggle",
                        button {
                            class: if graph_view() { "btn btn-glass btn-xs" } else { "btn btn-primary btn-xs" },
                            onclick: move |_| graph_view.set(false),
                            "List"
                        }
                        button {
                            class: if graph_view() { "btn btn-primary btn-xs" } else { "btn btn-glass btn-xs" },
                            onclick: move |_| graph_view.set(true),
                            "Graph"
                        }
                    }
                }
                if graph_view() {
                    if m.graph_nodes.is_empty() {
                        div { class: "glass-card empty",
                            p { class: "label-tech", "No topics to graph yet." }
                        }
                    } else {
                        MemoryGraph {
                            nodes: m.graph_nodes.clone(),
                            edges: m.graph_edges.clone(),
                            on_select: move |topic: String| {
                                graph_view.set(false);
                                scope.set(format!("topic:{topic}"));
                                ws.send(mem_topic_query(topic));
                            },
                        }
                    }
                } else if !m.loaded {
                    SkeletonList { rows: 4 }
                } else if m.entries.is_empty() {
                    div { class: "glass-card empty",
                        p { class: "label-tech", "No memory here yet — the agent writes memories as it learns what matters to you." }
                    }
                } else {
                    div { class: "mem-entries",
                        for e in m.entries.iter() {
                            MemoryEntry { entry: e.clone() }
                        }
                    }
                }
                if let Some(current_topic) = scope().strip_prefix("topic:").map(|s| s.to_string()) {
                    ConflictsPanel { topic: current_topic, conflicts: m.conflicts.clone() }
                }
                if let Some((ok, msg)) = memory_ui().notice.clone() {
                    div { class: if ok { "notice ok" } else { "notice err" }, "{msg}" }
                }
            }
        }
    }
}

/// POLISH_WAVES.md sub-project 5, item E — the currently-selected topic's
/// open conflicts (if any), with resolve/dismiss actions matching the
/// CLI's own `aivyx-pa memory conflicts` semantics exactly: "keep this one"
/// deletes the OTHER side (`ResolveMemoryConflict` names the loser's own
/// `topic`/`seq` as `archive_seq`); "not a conflict" dismisses the pair as
/// a false positive without deleting anything.
#[component]
fn ConflictsPanel(topic: String, conflicts: Vec<aivyx_ipc::conflict::MemoryConflict>) -> Element {
    let ws = use_context::<Sender>();
    let relevant: Vec<_> = conflicts
        .into_iter()
        .filter(|c| c.a.topic == topic || c.b.topic == topic)
        .collect();
    if relevant.is_empty() {
        return rsx! { Fragment {} };
    }
    rsx! {
        div { class: "conflicts",
            for c in relevant.iter() {
                {
                    let conflict_id = c.id.clone();
                    let a = c.a.clone();
                    let b = c.b.clone();
                    let (keep_a_topic, keep_a_seq) = (b.topic.clone(), b.seq);
                    let (keep_b_topic, keep_b_seq) = (a.topic.clone(), a.seq);
                    let dismiss_id = conflict_id.clone();
                    rsx! {
                        div { class: "glass-card conflict", key: "{conflict_id}",
                            p { class: "notice err", "{c.reason}" }
                            div { class: "conflict-side", span { class: "label-tech", "{a.topic} #{a.seq}" } p { "{a.body}" } }
                            div { class: "conflict-side", span { class: "label-tech", "{b.topic} #{b.seq}" } p { "{b.body}" } }
                            div { class: "conflict-actions",
                                button {
                                    class: "btn btn-success btn-xs",
                                    onclick: move |_| ws.send(resolve_memory_conflict_query(keep_a_topic.clone(), keep_a_seq)),
                                    "Keep \"{a.topic} #{a.seq}\""
                                }
                                button {
                                    class: "btn btn-success btn-xs",
                                    onclick: move |_| ws.send(resolve_memory_conflict_query(keep_b_topic.clone(), keep_b_seq)),
                                    "Keep \"{b.topic} #{b.seq}\""
                                }
                                button {
                                    class: "btn btn-ghost btn-xs",
                                    onclick: move |_| ws.send(dismiss_memory_conflict_query(dismiss_id.clone())),
                                    "Not a conflict"
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn resolve_memory_conflict_query(topic: String, archive_seq: u64) -> FrontendMessage {
    FrontendMessage::ResolveMemoryConflict {
        id: "mc-mem-conflict-resolve".to_string(),
        topic,
        archive_seq,
    }
}

fn dismiss_memory_conflict_query(conflict_id: String) -> FrontendMessage {
    FrontendMessage::DismissMemoryConflict {
        id: "mc-mem-conflict-dismiss".to_string(),
        conflict_id,
    }
}

#[component]
fn MemoryEntry(entry: MemoryEntrySummary) -> Element {
    rsx! {
        div { class: "glass-card mem-entry",
            div { class: "mem-entry-head",
                span { class: "chip", "{entry.topic}" }
                span { class: "when label-tech", "{rel_time_secs(entry.created_at_secs)}" }
                span { class: "seq label-tech", "#{entry.seq}" }
            }
            p { class: "mem-body", "{entry.body}" }
        }
    }
}

fn mem_topics_query() -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-mem-topics".to_string(),
        payload: QueryPayload::ListMemoryTopics,
    }
}

fn mem_topic_query(topic: String) -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-mem-topic".to_string(),
        payload: QueryPayload::GetMemoryTopicEntries { topic, limit: MEMORY_LIMIT },
    }
}

fn mem_search_query(query: String, semantic: bool) -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-mem-search".to_string(),
        payload: QueryPayload::SearchMemory { query, limit: MEMORY_LIMIT, semantic },
    }
}

fn mem_graph_query() -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-mem-graph".to_string(),
        payload: QueryPayload::GetMemoryGraph { limit: 60 },
    }
}

fn mem_conflicts_query() -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-mem-conflicts".to_string(),
        payload: QueryPayload::GetMemoryConflicts,
    }
}

// ---------------------------------------------------------------------------
// Wiki view — Chapter Codex (CX.5). The synthesized per-topic knowledge
// pages: an index rail → a page (summary + co-occurrence backlinks +
// source-entry refs). Read-only; pages are derived from memory.
// ---------------------------------------------------------------------------

#[component]
fn WikiPanel() -> Element {
    let ws = use_context::<Sender>();
    let wiki = use_context::<Signal<WikiState>>();

    // Load the page index each time the view opens.
    use_future(move || async move {
        ws.send(wiki_list_query());
    });

    let w = wiki();
    let selected_topic = w.selected.as_ref().map(|p| p.topic.clone());
    rsx! {
        div { class: "mem",
            aside { class: "mem-rail",
                div { class: "panel-head", h3 { "Pages" } span { class: "label-tech", "{w.pages.len()}" } }
                if w.pages.is_empty() {
                    p { class: "label-tech", style: "padding:8px",
                        "No pages yet. Set [memory] profile = \"smart\" (or [wiki] enabled = true) and the agent consolidates each memory topic into a page."
                    }
                }
                for p in w.pages.iter() {
                    {
                        let topic = p.topic.clone();
                        let sel = selected_topic.as_deref() == Some(p.topic.as_str());
                        rsx! {
                            button {
                                class: if sel { "mem-topic active" } else { "mem-topic" },
                                onclick: move |_| ws.send(wiki_page_query(topic.clone())),
                                "{p.topic}"
                                span { class: "label-tech", style: "float:right", "{p.entry_count}" }
                            }
                        }
                    }
                }
            }
            div { class: "mem-main",
                match w.selected.clone() {
                    Some(page) => rsx! { WikiPageView { page } },
                    None => rsx! {
                        div { class: "glass-card empty",
                            p { class: "label-tech",
                                "Select a page. Each is the agent's consolidated summary of one memory topic — what it knows, not just what it logged."
                            }
                        }
                    },
                }
            }
        }
    }
}

#[component]
fn WikiPageView(page: WikiPage) -> Element {
    let ws = use_context::<Sender>();
    rsx! {
        div { class: "glass-card",
            div { class: "mem-entry-head",
                h3 { "{page.topic}" }
                span { class: "chip", "{page.entry_count} entries" }
                span { class: "when label-tech", "updated {rel_time_secs(page.updated_at)}" }
            }
            p { class: "mem-body", style: "white-space:pre-wrap", "{page.summary}" }
            if !page.backlinks.is_empty() {
                div { class: "panel-head", h3 { class: "label-tech", "Related" } }
                div { class: "wiki-backlinks",
                    for b in page.backlinks.iter() {
                        {
                            let topic = b.topic.clone();
                            rsx! {
                                button {
                                    class: "chip",
                                    title: "affinity {b.affinity:.2} · {b.hops} hop(s)",
                                    onclick: move |_| ws.send(wiki_page_query(topic.clone())),
                                    "{b.topic}"
                                }
                            }
                        }
                    }
                }
            }
            div { class: "when label-tech", style: "margin-top:8px",
                "consolidated from {page.source_seqs.len()} memory entr",
                if page.source_seqs.len() == 1 { "y" } else { "ies" }
            }
        }
    }
}

fn wiki_list_query() -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-wiki-list".to_string(),
        payload: QueryPayload::ListWikiPages,
    }
}

fn wiki_page_query(topic: String) -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-wiki-page".to_string(),
        payload: QueryPayload::GetWikiPage { topic },
    }
}

// ---------------------------------------------------------------------------
// Skills view — Chapter Repertoire (RP.2). The agent's whole repertoire of
// skills (operator-taught, agent-authored, agent-refined) with their WH.2
// effectiveness + provenance/lineage. Read-only; governance (approve/edit/
// reject of skill proposals) stays in the Agents screen — this points there.
// ---------------------------------------------------------------------------

fn skills_query() -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-skills".to_string(),
        payload: QueryPayload::GetSkills,
    }
}

fn forget_skill_query(name: &str) -> FrontendMessage {
    FrontendMessage::ForgetSkill {
        id: format!("mc-skill-forget-{name}"),
        name: name.to_string(),
    }
}

/// Effectiveness bucket label + bar fraction from the WH.2 EWMA + samples.
/// `samples == 0` ⇒ unmeasured.
fn skill_effectiveness(view: &SkillView) -> (&'static str, &'static str, f32) {
    if view.samples == 0 {
        return ("unmeasured", "skill-eff-unmeasured", 0.0);
    }
    // Normalize the (unbounded) EWMA into a 0..1 bar via a soft squash.
    let frac = (view.ewma_score / (view.ewma_score.abs() + 2.0) + 1.0) / 2.0;
    if view.ewma_score < 0.0 {
        ("underperforming", "skill-eff-bad", frac.clamp(0.0, 1.0))
    } else if view.ewma_score > 0.0 {
        ("helping", "skill-eff-good", frac.clamp(0.0, 1.0))
    } else {
        ("neutral", "skill-eff-neutral", 0.5)
    }
}

#[component]
fn SkillsPanel() -> Element {
    let ws = use_context::<Sender>();
    let skills = use_context::<Signal<SkillsState>>();
    // Chapter Repertoire (approve-in-place) — reuse the shared persona-
    // proposal feed + ProposalCard, filtered to skill proposals.
    let agents = use_context::<Signal<AgentsState>>();
    // Chapter Tutor — "Teach a skill" form. Local fields (same pattern
    // SchedulesPanel's own "Create schedule" form uses: cron/prompt are
    // local signals there too, only the ack notice is shared context).
    let mut skills_ui = use_context::<Signal<SkillsUi>>();
    let mut teach_open = use_signal(|| false);
    let mut teach_name = use_signal(String::new);
    let mut teach_trigger = use_signal(String::new);
    let mut teach_procedure = use_signal(String::new);
    let teach = move |_| {
        let n = teach_name().trim().to_string();
        let t = teach_trigger().trim().to_string();
        let p = teach_procedure().trim().to_string();
        if n.is_empty() || t.is_empty() || p.is_empty() {
            skills_ui.write().notice =
                Some((false, "name, trigger, and procedure are all required".into()));
            return;
        }
        ws.send(FrontendMessage::AuthorSkill {
            id: format!("mc-skill-teach-{n}"),
            op: SkillAuthorOp::Teach,
            name: n,
            trigger: Some(t),
            procedure: Some(p),
        });
        ws.send(skills_query());
        teach_name.set(String::new());
        teach_trigger.set(String::new());
        teach_procedure.set(String::new());
    };

    // Load the inventory + the pending proposals each time the view opens,
    // and re-load after any proposal-resolve/revert ack bumps the shared
    // refresh tick — approving a LearnedSkill from this screen must show
    // the landed skill without a manual reload (Vitrine §6 operator
    // finding). Same memo-isolation as the Agents panel: the effect
    // re-runs only on mount and on the tick, never on the state writes
    // its own queries produce.
    let tick = use_memo(move || agents().refresh_tick);
    use_effect(move || {
        let _ = tick();
        ws.send(skills_query());
        ws.send(list_proposals_query());
    });

    let s = skills();
    // Pending skill proposals (Whetstone refinements + Praxis authored) —
    // approve / edit / reject right here, via the existing ProposalCard.
    let skill_proposals: Vec<PersonaProposalSummary> = agents()
        .proposals
        .into_iter()
        .filter(|p| p.category == "LearnedSkill" && p.status == "Pending")
        .collect();
    // Effectiveness-descending, with unmeasured (samples 0) grouped last.
    let mut rows = s.skills.clone();
    rows.sort_by(|a, b| {
        let am = a.samples == 0;
        let bm = b.samples == 0;
        am.cmp(&bm).then_with(|| {
            b.ewma_score
                .partial_cmp(&a.ewma_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    });

    rsx! {
        div { class: "skills",
            div { class: "panel-head",
                h3 { "Skills" }
                span { class: "label-tech", "{s.skills.len()}" }
                button {
                    class: "btn btn-secondary btn-xs",
                    onclick: move |_| teach_open.set(!teach_open()),
                    if teach_open() { "Cancel" } else { "+ Teach a skill" }
                }
            }
            if let Some((ok, text)) = skills_ui().notice {
                div {
                    class: "glass-card",
                    style: if ok {
                        "border-left: 3px solid var(--ok, #16a34a); margin-bottom: 12px; padding: 8px 12px;"
                    } else {
                        "border-left: 3px solid var(--danger, #b91c1c); margin-bottom: 12px; padding: 8px 12px;"
                    },
                    p { class: "label-tech", "{text}" }
                }
            }
            if teach_open() {
                div { class: "glass-card", style: "margin-bottom: 12px; padding: 12px;",
                    label { class: "label-tech", "Name" }
                    input {
                        class: "input",
                        placeholder: "summarize-document",
                        value: "{teach_name}",
                        oninput: move |e| teach_name.set(e.value()),
                    }
                    label { class: "label-tech", "Trigger (when should the agent use this?)" }
                    input {
                        class: "input",
                        placeholder: "When the operator asks for a summary of a document or file.",
                        value: "{teach_trigger}",
                        oninput: move |e| teach_trigger.set(e.value()),
                    }
                    label { class: "label-tech", "Procedure (what should the agent do?)" }
                    textarea {
                        class: "input",
                        rows: "4",
                        placeholder: "1. Read the file. 2. Identify the key points. 3. Reply with a concise summary.",
                        value: "{teach_procedure}",
                        oninput: move |e| teach_procedure.set(e.value()),
                    }
                    button {
                        class: "btn btn-primary btn-xs",
                        onclick: teach,
                        "Teach"
                    }
                }
            }
            if !skill_proposals.is_empty() {
                div { class: "skills-proposals",
                    div { class: "panel-head",
                        h3 { class: "label-tech", "Pending proposals" }
                        span { class: "chip warning", "{skill_proposals.len()}" }
                    }
                    for p in skill_proposals.iter() {
                        { rsx! { ProposalCard { key: "{p.id}", p: p.clone() } } }
                    }
                }
            }
            if !s.loaded {
                SkeletonCards { cards: 4 }
            } else if s.skills.is_empty() {
                div { class: "glass-card empty",
                    p { class: "label-tech",
                        "No skills yet. Teach one in chat (\"learn this skill…\"), or enable [skill_authoring] so the agent writes specialized skills from what it knows."
                    }
                }
            } else {
                div { class: "skills-grid",
                    for sv in rows.iter() {
                        { rsx! { SkillCard { view: sv.clone() } } }
                    }
                }
            }
        }
    }
}

#[component]
fn SkillCard(view: SkillView) -> Element {
    let ws = use_context::<Sender>();
    let mut confirming = use_signal(|| false);
    let sk = &view.skill;
    let (eff_label, eff_class, eff_frac) = skill_effectiveness(&view);
    let agent = sk.provenance.author == aivyx_ipc::persona::SkillAuthor::Agent;
    let forget_name = sk.name.clone();
    rsx! {
        div { class: "glass-card skill-card",
            div { class: "skill-card-head",
                h3 { "{sk.name}" }
                div { class: "skill-badges",
                    span {
                        class: if agent { "chip skill-prov-agent" } else { "chip skill-prov-op" },
                        if agent { "agent" } else { "operator" }
                    }
                    if let Some(d) = sk.domain.as_ref() {
                        span { class: "chip", "{d}" }
                    }
                    span { class: "label-tech", "v{sk.version}" }
                }
            }
            p { class: "skill-trigger", "{sk.trigger}" }
            if let Some(from) = sk.refined_from.as_ref() {
                p { class: "label-tech", "refined from {from}" }
            }
            div { class: "skill-eff",
                span { class: "chip {eff_class}", "{eff_label}" }
                div { class: "skill-eff-bar",
                    div { class: "skill-eff-fill {eff_class}", style: "width:{(eff_frac*100.0) as u32}%" }
                }
                span { class: "label-tech",
                    if view.samples == 0 { "no data" } else { "score {view.ewma_score:.1} · {view.samples} sample(s)" }
                }
                span { class: "label-tech", "· invoked {view.invocations}×" }
            }
            details { class: "skill-proc",
                summary { class: "label-tech", "procedure" }
                p { class: "mem-body", style: "white-space:pre-wrap", "{sk.procedure}" }
            }
            div { class: "skill-actions",
                if confirming() {
                    span { class: "label-tech", "Forget this skill?" }
                    button { class: "btn-danger",
                        onclick: move |_| { ws.send(forget_skill_query(&forget_name)); confirming.set(false); },
                        "Confirm"
                    }
                    button { class: "btn-ghost", onclick: move |_| confirming.set(false), "Cancel" }
                } else {
                    button { class: "btn-ghost", onclick: move |_| confirming.set(true), "Forget" }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// MCP screen — Chapter Lantern (LN.3). The web port of `aivyx-pa mcp status`:
// each configured MCP server's last-start health (connected + tool count,
// or failed + reason + captured stderr), read from the daemon's snapshot
// over GetMcpStatus. Read-only — adding/removing servers stays in
// aivyx-pa.toml (the screen shows, it does not edit).
// ---------------------------------------------------------------------------

fn reminders_query() -> FrontendMessage {
    FrontendMessage::Query {
        id: "reminders".to_string(),
        payload: QueryPayload::GetReminders,
    }
}

fn loop_status_query() -> FrontendMessage {
    FrontendMessage::Query {
        id: "loop-status".to_string(),
        payload: QueryPayload::LoopStatus,
    }
}

fn loop_start_query() -> FrontendMessage {
    FrontendMessage::Query {
        id: "loop-start".to_string(),
        payload: QueryPayload::LoopStart { max_iterations: None },
    }
}

fn loop_stop_query() -> FrontendMessage {
    FrontendMessage::Query {
        id: "loop-stop".to_string(),
        payload: QueryPayload::LoopStop,
    }
}

fn mcp_query() -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-mcp-status".to_string(),
        payload: QueryPayload::GetMcpStatus,
    }
}

/// POLISH_WAVES.md sub-project 8 item C — the rolling per-server
/// health query, fixed to a 24h window (matching `[proactive]`'s own
/// `DEFAULT_PROACTIVE_WINDOW_SECS` — no operator-configurable picker,
/// YAGNI). Shares the `"mc-mcp-status"` id with `mcp_query()` — both
/// are read-only status fetches for the same panel, and neither is
/// expected to error under normal operation (only `GetMcpServerCall
/// Stats`'s "no_audit_log" case would, which is as rare as `GetMcpStatus`
/// itself failing).
fn mcp_server_call_stats_query() -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-mcp-status".to_string(),
        payload: QueryPayload::GetMcpServerCallStats { window_secs: Some(86_400) },
    }
}

fn mcp_server_configs_query() -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-mcp-configs".to_string(),
        payload: QueryPayload::GetMcpServerConfigs,
    }
}

fn set_mcp_server_query(entry: McpServerConfigView) -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-mcp-set".to_string(),
        payload: QueryPayload::SetMcpServer {
            name: entry.name,
            transport: entry.transport,
            command: entry.command,
            args: entry.args,
            env: entry.env,
            headers: entry.headers,
            url: entry.url,
            enabled: entry.enabled,
        },
    }
}

fn delete_mcp_server_query(name: String) -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-mcp-delete".to_string(),
        payload: QueryPayload::DeleteMcpServer { name },
    }
}

#[component]
fn McpPanel() -> Element {
    let ws = use_context::<Sender>();
    let mcp = use_context::<Signal<McpState>>();
    let mcp_config_ui = use_context::<Signal<McpConfigUi>>();
    let mut editing = use_signal(|| None::<McpServerConfigView>);
    let mut adding = use_signal(|| false);

    // Load the snapshot each time the view opens (it only changes on a
    // daemon restart, so on-open + a manual refresh is enough — no poll).
    use_future(move || async move {
        ws.send(mcp_query());
        ws.send(mcp_server_configs_query());
        ws.send(mcp_server_call_stats_query());
    });

    let m = mcp();
    let connected = m.servers.iter().filter(|s| s.connected).count();
    rsx! {
        div { class: "mcp",
            div { class: "panel-head",
                h3 { "MCP Servers" }
                if m.loaded && !m.servers.is_empty() {
                    span { class: "label-tech", "{connected}/{m.servers.len()} connected" }
                }
                button {
                    class: "btn-ghost",
                    onclick: move |_| {
                        ws.send(mcp_query());
                        ws.send(mcp_server_configs_query());
                        ws.send(mcp_server_call_stats_query());
                    },
                    "Refresh"
                }
            }
            if !m.loaded {
                SkeletonCards { cards: 3 }
            } else if m.servers.is_empty() {
                div { class: "glass-card empty",
                    p { class: "label-tech",
                        "No MCP servers reported at the last daemon start. Add one below, then restart the daemon."
                    }
                }
            } else {
                div { class: "mcp-grid",
                    for sv in m.servers.iter() {
                        {
                            let stats = m.call_stats.iter().find(|s| s.server_name == sv.name).cloned();
                            rsx! { McpServerCard { key: "{sv.name}", view: sv.clone(), call_stats: stats } }
                        }
                    }
                }
            }

            div { class: "panel-head", style: "margin-top:22px;",
                h3 { "Configured servers" }
                button { class: "btn btn-primary btn-xs", onclick: move |_| { editing.set(None); adding.set(true); }, "Add server" }
            }
            if let Some((ok, text)) = mcp_config_ui().notice {
                div { class: if ok { "notice ok" } else { "notice err" }, "{text}" }
            }
            if adding() || editing().is_some() {
                McpServerForm {
                    // Final-review fix #3 — `key` forces Dioxus to remount a
                    // fresh `McpServerForm` instance (rather than diffing
                    // props onto the live one) whenever which server is
                    // being edited changes, including the "editing X" →
                    // "adding new" transition. Without this, the form's
                    // `use_signal` seed initializers (which only run on
                    // first mount) would keep showing the previous server's
                    // stale field values with Save now creating/overwriting
                    // the wrong entry.
                    key: "{editing().map(|e| e.name.clone()).unwrap_or_else(|| \"new\".to_string())}",
                    initial: editing(),
                    on_cancel: move |_| { adding.set(false); editing.set(None); },
                    on_save: move |entry: McpServerConfigView| {
                        ws.send(set_mcp_server_query(entry));
                        adding.set(false);
                        editing.set(None);
                    },
                }
            } else if m.configs.is_empty() {
                div { class: "glass-card empty", p { class: "label-tech", "No `[[mcp_server]]` entries configured yet." } }
            } else {
                div { class: "mcp-grid",
                    for cfg in m.configs.iter() {
                        {
                            let cfg2 = cfg.clone();
                            let name = cfg.name.clone();
                            rsx! {
                                div { key: "{cfg.name}", class: "glass-card mcp-card",
                                    div { class: "mcp-card-head",
                                        span { class: "mcp-name", "{cfg.name}" }
                                        span { class: "label-tech", "{cfg.transport}" }
                                        span { class: if cfg.enabled { "chip success" } else { "chip" }, if cfg.enabled { "enabled" } else { "disabled" } }
                                    }
                                    div { style: "display:flex; gap:8px; margin-top:8px;",
                                        button { class: "btn btn-glass btn-xs", onclick: move |_| { adding.set(false); editing.set(Some(cfg2.clone())); }, "Edit" }
                                        button { class: "btn btn-glass btn-xs", onclick: move |_| ws.send(delete_mcp_server_query(name.clone())), "Delete" }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn LoopPanel() -> Element {
    let ws = use_context::<Sender>();
    let loop_ui = use_context::<Signal<LoopUiState>>();

    use_future(move || async move {
        loop {
            ws.send(loop_status_query());
            TimeoutFuture::new(POLL_INTERVAL_MS).await;
        }
    });

    let l = loop_ui();
    let (start_disabled, stop_disabled) = loop_button_state(l.armed, l.state.active);
    rsx! {
        div { class: "settings",
            div { class: "panel-head",
                h3 { "Autonomous Loop" }
                button {
                    class: "btn-ghost",
                    onclick: move |_| { ws.send(loop_status_query()); },
                    "Refresh"
                }
            }
            if let Some((ok, text)) = l.last_control_result.clone() {
                div { class: if ok { "notice ok" } else { "notice err" }, "{text}" }
            }
            if !l.loaded {
                SkeletonCards { cards: 1 }
            } else if !l.armed {
                div { class: "glass-card empty",
                    p { class: "label-tech", "No [loop] section configured -- nothing to start." }
                }
            } else {
                div { class: "glass-card",
                    p {
                        if l.state.consecutive_idle > 0 && l.state.active {
                            "Stalled ({l.state.consecutive_idle} consecutive idle iterations)"
                        } else if l.state.active {
                            "Running -- iteration {l.state.iteration}/{l.state.max_iterations}"
                        } else {
                            {
                                let reason = l.state.last_stop_reason.as_deref().unwrap_or("never run");
                                rsx! { "Idle ({reason})" }
                            }
                        }
                    }
                    p { class: "label-tech",
                        "Spend: ${(l.state.spent_cents as f64 / 100.0):.2} -- {l.state.tokens_used / 1000}k tokens -- backlog: {l.remaining} remaining"
                    }
                    div { style: "display:flex; gap:10px; margin-top:12px;",
                        button {
                            class: "btn btn-primary",
                            disabled: start_disabled,
                            onclick: move |_| {
                                ws.send(loop_start_query());
                                ws.send(loop_status_query());
                            },
                            "Start"
                        }
                        button {
                            class: "btn-ghost",
                            disabled: stop_disabled,
                            onclick: move |_| {
                                ws.send(loop_stop_query());
                                ws.send(loop_status_query());
                            },
                            "Stop"
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn RemindersPanel() -> Element {
    let ws = use_context::<Sender>();
    let reminders_ui = use_context::<Signal<RemindersState>>();

    use_future(move || async move {
        loop {
            ws.send(reminders_query());
            TimeoutFuture::new(POLL_INTERVAL_MS).await;
        }
    });

    let r = reminders_ui();
    let now_unix = js_sys::Date::now() as i64 / 1000;
    rsx! {
        div { class: "settings",
            div { class: "panel-head",
                h3 { "Reminders" }
                if r.loaded && !r.reminders.is_empty() {
                    span { class: "label-tech", "{r.reminders.len()} pending" }
                }
                button {
                    class: "btn-ghost",
                    onclick: move |_| { ws.send(reminders_query()); },
                    "Refresh"
                }
            }
            if !r.loaded {
                SkeletonCards { cards: 2 }
            } else if r.reminders.is_empty() {
                div { class: "glass-card empty",
                    p { class: "label-tech", "No pending reminders." }
                }
            } else {
                {
                    let mut sorted: Vec<_> = r.reminders.iter().collect();
                    sorted.sort_by_key(|reminder| (reminder.due_unix, reminder.id.clone()));
                    rsx! {
                        div { class: "mcp-grid",
                            for reminder in sorted.iter() {
                                div {
                                    key: "{reminder.id}",
                                    class: "glass-card",
                                    style: "display:flex; flex-direction:column; gap:6px;",
                                    span { class: "label-tech", "{format_due_offset(reminder.due_unix, now_unix)}" }
                                    span { "{reminder.message}" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// POLISH_WAVES.md sub-project 8 item C — the health-chip class + label
/// for one server's rolling call stats. A pure function (no `Element`,
/// no context) so it's directly unit-testable, mirroring this file's
/// existing `phase_class`/`eff_class`-style small helpers.
///
/// `stats: None` means no `mcp.call` audit entries for this server in
/// the window — a configured-but-unused server is not itself unhealthy,
/// so this renders neutral, not amber/red. `"failed"` and `"denied"`
/// outcomes both count toward the unhealthy tally: a denial is still a
/// call that didn't do what the operator configured it to do.
fn mcp_health_chip(stats: Option<&McpServerCallStats>) -> (&'static str, String) {
    let Some(s) = stats else {
        return ("chip", "no recent activity".to_string());
    };
    let bad = s.outcomes.get("failed").copied().unwrap_or(0)
        + s.outcomes.get("denied").copied().unwrap_or(0);
    let ok = s.calls.saturating_sub(bad);
    if bad == 0 {
        ("chip success", format!("{ok} ok"))
    } else if bad.saturating_mul(2) > s.calls {
        ("chip error", format!("{ok} ok / {bad} failed"))
    } else {
        ("chip warning", format!("{ok} ok / {bad} failed"))
    }
}

#[component]
fn McpServerCard(view: McpServerStatusView, call_stats: Option<McpServerCallStats>) -> Element {
    let (pill_class, pill_label) = if view.connected {
        ("chip success", "connected")
    } else {
        ("chip error", "failed")
    };
    // POLISH_WAVES.md sub-project 8 item C — additive to the boot-time
    // pill above, not a replacement: a server can be `connected` (it
    // answered the startup handshake) while this chip is red (its
    // tools have been failing since) -- that combination is the exact
    // finding this item exists to surface.
    let (health_class, health_label) = mcp_health_chip(call_stats.as_ref());
    rsx! {
        div { class: "glass-card mcp-card",
            div { class: "mcp-card-head",
                span { class: "mcp-name", "{view.name}" }
                span { class: "label-tech", "{view.transport}" }
                span { class: pill_class, "{pill_label}" }
                span { class: health_class, title: "last 24h", "{health_label}" }
            }
            if view.connected {
                p { class: "label-tech", "{view.tool_count} tool(s) registered" }
            } else {
                if let Some(err) = view.error.as_ref() {
                    p { class: "mcp-error", "{err}" }
                }
                if !view.stderr_tail.is_empty() {
                    details { class: "mcp-stderr",
                        summary { class: "label-tech", "captured stderr ({view.stderr_tail.len()} line(s))" }
                        pre {
                            for line in view.stderr_tail.iter() {
                                "{line}\n"
                            }
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod mcp_health_chip_tests {
    use super::*;

    #[test]
    fn mcp_health_chip_no_stats_is_neutral() {
        let (class, label) = mcp_health_chip(None);
        assert_eq!(class, "chip");
        assert_eq!(label, "no recent activity");
    }

    #[test]
    fn loop_button_state_both_enabled_when_armed_and_idle() {
        let (start_disabled, stop_disabled) = loop_button_state(true, false);
        assert!(!start_disabled, "armed + idle: Start should be enabled");
        assert!(stop_disabled, "idle: Stop should stay disabled");
    }

    #[test]
    fn loop_button_state_stop_enabled_when_active() {
        let (start_disabled, stop_disabled) = loop_button_state(true, true);
        assert!(start_disabled, "already active: Start should be disabled");
        assert!(!stop_disabled, "active: Stop should be enabled");
    }

    #[test]
    fn loop_button_state_start_disabled_when_not_armed() {
        let (start_disabled, stop_disabled) = loop_button_state(false, false);
        assert!(start_disabled, "not armed: nothing to start");
        assert!(stop_disabled, "not armed and idle: nothing to stop");
    }

    #[test]
    fn loop_button_state_stop_enabled_even_when_not_armed_if_somehow_active() {
        // Defensive case: armed=false but active=true shouldn't happen in
        // practice (armed reflects [loop] config presence, active reflects
        // a running driver), but Stop must never be the wrong answer if it
        // does -- an operator must always be able to stop a running loop.
        let (start_disabled, stop_disabled) = loop_button_state(false, true);
        assert!(start_disabled);
        assert!(!stop_disabled);
    }

    #[test]
    fn format_due_offset_future_minutes() {
        assert_eq!(format_due_offset(660, 60), "in 10m");
    }

    #[test]
    fn format_due_offset_future_hours() {
        assert_eq!(format_due_offset(7_260, 60), "in 2h");
    }

    #[test]
    fn format_due_offset_future_days() {
        assert_eq!(format_due_offset(90_060, 60), "in 1d");
    }

    #[test]
    fn format_due_offset_overdue() {
        assert_eq!(format_due_offset(60, 660), "10m overdue");
    }

    #[test]
    fn format_due_offset_exactly_now() {
        assert_eq!(format_due_offset(60, 60), "in 0s");
    }

    #[test]
    fn mcp_health_chip_all_ok_is_success() {
        let mut outcomes = std::collections::BTreeMap::new();
        outcomes.insert("completed".to_string(), 5u64);
        let stats = McpServerCallStats {
            server_name: "comfyui".to_string(),
            calls: 5,
            outcomes,
            total_duration_ms: 500,
        };
        let (class, label) = mcp_health_chip(Some(&stats));
        assert_eq!(class, "chip success");
        assert_eq!(label, "5 ok");
    }

    #[test]
    fn mcp_health_chip_minority_failures_is_warning() {
        let mut outcomes = std::collections::BTreeMap::new();
        outcomes.insert("completed".to_string(), 8u64);
        outcomes.insert("failed".to_string(), 2u64);
        let stats = McpServerCallStats {
            server_name: "duckduckgo-search".to_string(),
            calls: 10,
            outcomes,
            total_duration_ms: 1000,
        };
        let (class, label) = mcp_health_chip(Some(&stats));
        assert_eq!(class, "chip warning");
        assert_eq!(label, "8 ok / 2 failed");
    }

    #[test]
    fn mcp_health_chip_majority_failures_is_error() {
        let mut outcomes = std::collections::BTreeMap::new();
        outcomes.insert("completed".to_string(), 2u64);
        outcomes.insert("failed".to_string(), 8u64);
        let stats = McpServerCallStats {
            server_name: "duckduckgo-search".to_string(),
            calls: 10,
            outcomes,
            total_duration_ms: 1000,
        };
        let (class, label) = mcp_health_chip(Some(&stats));
        assert_eq!(class, "chip error");
        assert_eq!(label, "2 ok / 8 failed");
    }

    #[test]
    fn mcp_health_chip_counts_denied_as_unhealthy_too() {
        let mut outcomes = std::collections::BTreeMap::new();
        outcomes.insert("completed".to_string(), 3u64);
        outcomes.insert("denied".to_string(), 1u64);
        let stats = McpServerCallStats {
            server_name: "comfyui".to_string(),
            calls: 4,
            outcomes,
            total_duration_ms: 400,
        };
        let (class, label) = mcp_health_chip(Some(&stats));
        assert_eq!(class, "chip warning");
        assert_eq!(label, "3 ok / 1 failed");
    }
}

#[component]
fn McpServerForm(
    initial: Option<McpServerConfigView>,
    on_cancel: EventHandler<()>,
    on_save: EventHandler<McpServerConfigView>,
) -> Element {
    let ws = use_context::<Sender>();
    let mcp_config_ui = use_context::<Signal<McpConfigUi>>();
    let seed = initial.clone().unwrap_or(McpServerConfigView {
        name: String::new(),
        transport: "stdio".to_string(),
        command: None,
        args: Vec::new(),
        env: Vec::new(),
        headers: Vec::new(),
        url: None,
        enabled: true,
    });
    let editing_existing = initial.is_some();
    let mut name = use_signal(|| seed.name.clone());
    let mut transport = use_signal(|| seed.transport.clone());
    let mut command = use_signal(|| seed.command.clone().unwrap_or_default());
    let mut args_raw = use_signal(|| seed.args.join(" "));
    let mut url = use_signal(|| seed.url.clone().unwrap_or_default());
    let mut env_raw = use_signal(|| {
        seed.env.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("\n")
    });
    let mut headers_raw = use_signal(|| {
        seed.headers.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("\n")
    });
    let mut enabled = use_signal(|| seed.enabled);
    let is_stdio = transport() == "stdio";

    let parse_pairs = |raw: &str| -> Vec<(String, String)> {
        raw.lines()
            .filter_map(|line| line.split_once('='))
            .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
            .filter(|(k, _)| !k.is_empty())
            .collect()
    };

    rsx! {
        div { class: "glass-card",
            div { class: "field-row",
                label { "Name" }
                input { class: "input", value: "{name}", disabled: editing_existing, oninput: move |e| name.set(e.value()) }
            }
            div { class: "field-row",
                label { "Transport" }
                select { class: "input", value: "{transport}", onchange: move |e| transport.set(e.value()),
                    option { value: "stdio", "stdio" }
                    option { value: "sse", "sse" }
                    option { value: "http", "http" }
                }
            }
            if is_stdio {
                div { class: "field-row",
                    label { "Command" }
                    input { class: "input", value: "{command}", oninput: move |e| command.set(e.value()) }
                }
                div { class: "field-row",
                    label { "Args (space-separated)" }
                    input { class: "input", value: "{args_raw}", oninput: move |e| args_raw.set(e.value()) }
                }
                div { class: "field-row",
                    label { "Env (one KEY=value per line)" }
                    textarea { class: "doc-edit", value: "{env_raw}", oninput: move |e| env_raw.set(e.value()) }
                }
            } else {
                div { class: "field-row",
                    label { "URL" }
                    input { class: "input", value: "{url}", oninput: move |e| url.set(e.value()) }
                }
                div { class: "field-row",
                    label { "Headers (one Name=value per line)" }
                    textarea { class: "doc-edit", value: "{headers_raw}", oninput: move |e| headers_raw.set(e.value()) }
                }
            }
            div { class: "field-row",
                label { "Enabled" }
                input { r#type: "checkbox", checked: enabled(), onchange: move |e| enabled.set(e.checked()) }
            }
            div { style: "display:flex; gap:8px; margin-top:12px;",
                button {
                    class: "btn btn-primary btn-xs",
                    onclick: move |_| {
                        let entry = McpServerConfigView {
                            name: name().trim().to_string(),
                            transport: transport(),
                            command: if is_stdio && !command().trim().is_empty() { Some(command().trim().to_string()) } else { None },
                            args: if is_stdio { args_raw().split_whitespace().map(str::to_string).collect() } else { Vec::new() },
                            env: if is_stdio { parse_pairs(&env_raw()) } else { Vec::new() },
                            headers: if is_stdio { Vec::new() } else { parse_pairs(&headers_raw()) },
                            url: if is_stdio || url().trim().is_empty() { None } else { Some(url().trim().to_string()) },
                            enabled: enabled(),
                        };
                        on_save.call(entry);
                    },
                    "Save"
                }
                button {
                    class: "btn btn-glass btn-xs",
                    onclick: move |_| {
                        let msg = FrontendMessage::Query {
                            id: "mc-mcp-test".to_string(),
                            payload: QueryPayload::TestMcpServerConnection {
                                transport: transport(),
                                command: if is_stdio && !command().trim().is_empty() { Some(command().trim().to_string()) } else { None },
                                args: if is_stdio { args_raw().split_whitespace().map(str::to_string).collect() } else { Vec::new() },
                                env: if is_stdio { parse_pairs(&env_raw()) } else { Vec::new() },
                                headers: if is_stdio { Vec::new() } else { parse_pairs(&headers_raw()) },
                                url: if is_stdio || url().trim().is_empty() { None } else { Some(url().trim().to_string()) },
                            },
                        };
                        ws.send(msg);
                    },
                    "Test connection"
                }
                button { class: "btn btn-glass btn-xs", onclick: move |_| on_cancel.call(()), "Cancel" }
            }
            if let Some((ok, text)) = mcp_config_ui().test_result {
                div { class: if ok { "notice ok" } else { "notice err" }, "{text}" }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Studio Gallery — recent images generated via the `comfyui` `[[mcp_server]]`.
// The daemon reads ComfyUI's own `/history` API directly (not the MCP tool
// surface — see memory `comfyui-mcp-integration`) and this screen renders
// each result's bytes through the authenticated `/studio-asset` proxy route,
// never reaching ComfyUI's own (loopback-only) port from the browser.
// ---------------------------------------------------------------------------

fn gallery_query() -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-gallery".to_string(),
        payload: QueryPayload::GetGallery,
    }
}

/// `/studio-asset` URL for one gallery image — the same query params
/// `serve_comfy_asset` expects on the daemon side.
fn gallery_asset_url(img: &GalleryImage) -> String {
    format!(
        "/studio-asset?filename={}&subfolder={}&type={}",
        js_encode_uri(&img.filename),
        js_encode_uri(&img.subfolder),
        js_encode_uri(&img.folder_type),
    )
}

/// Percent-encode a query value client-side. Mirrors the daemon's own
/// encoder (`percent_encode` in `web_ui.rs`) closely enough that filenames
/// with the odd space/unicode character still round-trip.
fn js_encode_uri(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[component]
fn GalleryPanel() -> Element {
    let ws = use_context::<Sender>();
    let gallery = use_context::<Signal<GalleryState>>();
    // Which image (by index) is open in the lightbox, if any.
    let mut open_index = use_signal(|| None::<usize>);

    // Same "load on open + manual refresh" cadence as the MCP screen — a
    // ComfyUI generation only happens on an explicit tool call, so there's
    // nothing to poll for between opens.
    use_future(move || async move {
        ws.send(gallery_query());
    });

    let g = gallery();
    rsx! {
        div { class: "gallery",
            div { class: "panel-head",
                h3 { "Gallery" }
                if g.loaded && g.available && !g.images.is_empty() {
                    span { class: "label-tech", "{g.images.len()} image(s)" }
                }
                button { class: "btn-ghost", onclick: move |_| ws.send(gallery_query()), "Refresh" }
            }
            if !g.loaded {
                SkeletonCards { cards: 6 }
            } else if !g.available {
                div { class: "glass-card empty",
                    p { class: "label-tech",
                        "No `comfyui` MCP server is configured. Add a `[[mcp_server]]` block named \"comfyui\" in aivyx-pa.toml pointing at a running ComfyUI instance, then restart the daemon."
                    }
                }
            } else if g.images.is_empty() {
                div { class: "glass-card empty",
                    p { class: "label-tech", "No images generated yet." }
                }
            } else {
                div { class: "gallery-grid",
                    for (i , img) in g.images.iter().enumerate() {
                        div {
                            key: "{img.prompt_id}",
                            class: "glass-card gallery-card",
                            onclick: move |_| open_index.set(Some(i)),
                            img { class: "gallery-thumb", src: gallery_asset_url(img), loading: "lazy" }
                            if let Some(caption) = img.caption.as_ref() {
                                p { class: "gallery-caption", "{caption}" }
                            }
                            if let Some(ts) = img.created_unix {
                                span { class: "label-tech", "{rel_time_secs(ts)}" }
                            }
                        }
                    }
                }
            }
        }

        if let Some(i) = open_index() {
            if let Some(img) = g.images.get(i) {
                div { class: "modal-scrim", onclick: move |_| open_index.set(None),
                    div {
                        class: "glass-card modal gallery-lightbox",
                        onclick: move |e| e.stop_propagation(),
                        img { class: "gallery-full", src: gallery_asset_url(img) }
                        if let Some(caption) = img.caption.as_ref() {
                            p { "{caption}" }
                        }
                        div { class: "actions",
                            button { class: "btn btn-glass", onclick: move |_| open_index.set(None), "Close" }
                        }
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tools screen — Chapter Almanac. A read-only, searchable catalog of every
// tool the daemon has registered: name, description, capability base, and
// the minimum trust tier a channel needs before the tool becomes reachable
// at all (see `TrustTier::min_for_scope` in aivyx-capability). Distinct
// from the MCP screen (server health) and from `aivyx-pa tools` / GetToolStats
// (audit-derived call counts) — this is a pure registry browse, grouped by
// domain (the tool name's leading segment: `fs.read` → `fs`).
// ---------------------------------------------------------------------------

fn tool_catalog_query() -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-tool-catalog".to_string(),
        payload: QueryPayload::GetToolCatalog,
    }
}

fn tool_domain(name: &str) -> &str {
    name.split('.').next().unwrap_or(name)
}

fn tier_label(t: TrustTier) -> &'static str {
    match t {
        TrustTier::Kernel => "kernel",
        TrustTier::Trusted => "trusted",
        TrustTier::SemiTrusted => "semi-trusted",
        TrustTier::Untrusted => "untrusted",
    }
}

fn tier_chip_class(t: TrustTier) -> &'static str {
    match t {
        TrustTier::Kernel => "chip error",
        TrustTier::Trusted => "chip success",
        TrustTier::SemiTrusted => "chip warning",
        TrustTier::Untrusted => "chip muted",
    }
}

#[component]
fn ToolsPanel() -> Element {
    let ws = use_context::<Sender>();
    let tools = use_context::<Signal<ToolsState>>();
    let mut query = use_signal(String::new);

    // Load the snapshot each time the view opens (the registry only
    // changes on a daemon restart or an MCP hot-swap, so on-open + a
    // manual refresh is enough — no poll, mirrors the MCP screen).
    use_future(move || async move {
        ws.send(tool_catalog_query());
    });

    let t = tools();
    let q = query().to_lowercase();
    let mut filtered: Vec<ToolCatalogEntry> = t
        .tools
        .iter()
        .filter(|e| {
            q.is_empty()
                || e.name.to_lowercase().contains(&q)
                || e.description.to_lowercase().contains(&q)
                || e.scope_base.to_lowercase().contains(&q)
        })
        .cloned()
        .collect();
    filtered.sort_by(|a, b| a.name.cmp(&b.name));

    let mut groups: Vec<(String, Vec<ToolCatalogEntry>)> = Vec::new();
    for entry in filtered {
        let domain = tool_domain(&entry.name).to_string();
        match groups.iter_mut().find(|(d, _)| *d == domain) {
            Some((_, rows)) => rows.push(entry),
            None => groups.push((domain, vec![entry])),
        }
    }
    groups.sort_by(|a, b| a.0.cmp(&b.0));

    rsx! {
        div { class: "tools",
            div { class: "panel-head",
                h3 { "Tools" }
                if t.loaded {
                    span { class: "label-tech", "{t.tools.len()}" }
                }
                button {
                    class: "btn-ghost",
                    onclick: move |_| ws.send(tool_catalog_query()),
                    "Refresh"
                }
            }
            div { class: "tools-search",
                input {
                    class: "input",
                    "aria-label": "Search tools",
                    placeholder: "search tools by name, description, or scope…",
                    value: "{query}",
                    oninput: move |e| query.set(e.value()),
                }
            }
            if !t.loaded {
                SkeletonCards { cards: 4 }
            } else if t.tools.is_empty() {
                div { class: "glass-card empty",
                    p { class: "label-tech", "No tools reported by the daemon." }
                }
            } else if groups.is_empty() {
                div { class: "glass-card empty",
                    p { class: "label-tech", "No tools match \"{query}\"." }
                }
            } else {
                for (domain, rows) in groups.iter() {
                    div { class: "tools-group", key: "{domain}",
                        div { class: "panel-head",
                            h3 { class: "label-tech", "{domain}" }
                            span { class: "label-tech", "{rows.len()}" }
                        }
                        div { class: "tools-grid",
                            for entry in rows.iter() {
                                { rsx! { ToolCard { key: "{entry.name}", entry: entry.clone() } } }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[component]
fn ToolCard(entry: ToolCatalogEntry) -> Element {
    rsx! {
        div { class: "glass-card tool-card",
            div { class: "tool-card-head",
                span { class: "tool-name", "{entry.name}" }
                span { class: tier_chip_class(entry.min_tier), "{tier_label(entry.min_tier)}" }
            }
            p { class: "label-tech", "{entry.scope_base}" }
            p { class: "tool-desc", "{entry.description}" }
        }
    }
}

// ---------------------------------------------------------------------------
// Lattice view — Chapter Lattice (LT.5). The typed knowledge graph:
// entity nodes (sized by degree) + DIRECTED, labeled relation edges,
// force-laid-out in-WASM. Read-only; distinct from the MG co-occurrence
// view (undirected, topics).
// ---------------------------------------------------------------------------

#[component]
fn LatticePanel() -> Element {
    let ws = use_context::<Sender>();
    let lattice = use_context::<Signal<GraphKnowledgeState>>();

    use_future(move || async move {
        ws.send(knowledge_graph_query());
    });

    let g = lattice();
    rsx! {
        div { class: "lattice",
            div { class: "panel-head",
                h3 { "Knowledge Graph" }
                span { class: "label-tech", "{g.entities.len()} entities · {g.edges.len()} relations" }
            }
            if g.entities.is_empty() {
                div { class: "glass-card empty",
                    p { class: "label-tech",
                        "No graph yet. Set [memory] profile = \"smart\" (or [graph] enabled = true) and the agent extracts typed relations — (subject)-[predicate]->(object) — from its memory."
                    }
                }
            } else {
                LatticeGraph { entities: g.entities.clone(), edges: g.edges.clone() }
            }
        }
    }
}

/// The typed knowledge graph as a directed, labeled SVG. Reuses the
/// force-directed `compute_layout` (entities → nodes, triples → edges by
/// connectivity), then draws each edge as an arrowed line with its
/// predicate label at the midpoint.
#[component]
fn LatticeGraph(entities: Vec<GraphEntity>, edges: Vec<GraphTriple>) -> Element {
    // Map to the layout types (the FR layout cares only about
    // connectivity, not direction).
    let nodes: Vec<MemoryGraphNode> = entities
        .iter()
        .map(|e| MemoryGraphNode { topic: e.name.clone(), entry_count: e.degree })
        .collect();
    let layout_edges: Vec<PairScore> = edges
        .iter()
        .map(|t| PairScore {
            a: t.subject.clone(),
            b: t.object.clone(),
            score: t.mentions.max(1) as f32,
            samples: t.mentions,
        })
        .collect();
    // Memoized on `nodes`/`layout_edges` content (not on the `zoom`/`pan`/
    // `dragging`/`hovered` signals read further down in this same
    // component) — otherwise every pan-drag `onmousemove` or hover
    // enter/leave re-renders this component and would re-run the O(n²)
    // force-directed layout for no reason, since node positions only
    // depend on the graph's own nodes/edges, never the viewport or hover
    // state.
    let layout = use_memo(use_reactive!(|nodes, layout_edges| {
        let pos = compute_layout(&nodes, &layout_edges);
        let sides = label_sides(&nodes, &pos);
        (pos, sides)
    }));
    let (pos, sides) = layout();
    // POLISH_WAVES.md sub-project 6, item C — same threshold/rationale as
    // MemoryGraph; also gates the edge-predicate labels below, which are
    // an even denser source of overlap than the node labels alone.
    const LABEL_ALWAYS_ON_MAX: usize = 25;
    let always_on = nodes.len() <= LABEL_ALWAYS_ON_MAX;
    let mut hovered = use_signal(|| None::<String>);
    // POLISH_WAVES.md sub-project 6, item C — pan/zoom so a dense graph
    // can be explored instead of always fit-to-canvas. `zoom` scales the
    // visible viewBox (>1.0 = zoomed in / a smaller visible area); `pan`
    // is the viewBox's top-left corner in the same SVG-unit space.
    let mut zoom = use_signal(|| 1.0_f64);
    let mut pan = use_signal(|| (0.0_f64, 0.0_f64));
    let mut dragging = use_signal(|| None::<(f64, f64)>);
    let (vb_x, vb_y) = pan();
    let vb_w = GRAPH_W / zoom();
    let vb_h = GRAPH_H / zoom();
    let idx: std::collections::HashMap<&str, usize> =
        nodes.iter().enumerate().map(|(i, nd)| (nd.topic.as_str(), i)).collect();

    rsx! {
        div { class: "glass-card mem-graph-card",
            svg {
                class: "mem-graph",
                view_box: "{vb_x} {vb_y} {vb_w} {vb_h}",
                onwheel: move |e| {
                    e.prevent_default();
                    let dy = e.delta().strip_units().y;
                    let factor = if dy > 0.0 { 0.9 } else { 1.1 };
                    let z = (zoom() * factor).clamp(0.4, 3.0);
                    zoom.set(z);
                },
                onmousedown: move |e| {
                    let p = e.client_coordinates();
                    dragging.set(Some((p.x, p.y)));
                },
                onmousemove: move |e| {
                    if let Some((sx, sy)) = dragging() {
                        let p = e.client_coordinates();
                        let (dx, dy) = (p.x - sx, p.y - sy);
                        // Drag right/down should move the *view* left/up
                        // (the content should follow the cursor), and the
                        // delta is in screen pixels while pan is in
                        // viewBox units — scale by the current zoom so a
                        // drag feels the same speed at any zoom level.
                        let (px, py) = pan();
                        pan.set((px - dx / zoom(), py - dy / zoom()));
                        dragging.set(Some((p.x, p.y)));
                    }
                },
                onmouseup: move |_| dragging.set(None),
                // A simpler fallback for "the drag ended off-element"
                // than a window-level listener: releasing outside the
                // SVG just stops the pan, it doesn't need to resume.
                onmouseleave: move |_| dragging.set(None),
                defs {
                    marker {
                        id: "lattice-arrow", view_box: "0 0 10 10",
                        ref_x: "9", ref_y: "5", marker_width: "7", marker_height: "7",
                        orient: "auto-start-reverse",
                        path { d: "M 0 0 L 10 5 L 0 10 z", class: "lattice-arrowhead" }
                    }
                }
                // Directed edges (under the nodes), shortened to the target
                // node's rim so the arrowhead is visible.
                for t in edges.iter() {
                    if let (Some(&i), Some(&j)) = (idx.get(t.subject.as_str()), idx.get(t.object.as_str())) {
                        {
                            let (x1, y1) = pos[i];
                            let (x2c, y2c) = pos[j];
                            let r = node_radius(entities[j].degree) + 4.0;
                            let dx = x2c - x1; let dy = y2c - y1;
                            let d = (dx * dx + dy * dy).sqrt().max(0.01);
                            let (x2, y2) = (x2c - dx / d * r, y2c - dy / d * r);
                            let (mx, my) = ((x1 + x2) / 2.0, (y1 + y2) / 2.0);
                            let label = t.predicate.clone();
                            rsx! {
                                line {
                                    x1: "{x1}", y1: "{y1}", x2: "{x2}", y2: "{y2}",
                                    class: "lattice-edge", marker_end: "url(#lattice-arrow)",
                                }
                                if always_on {
                                    text { x: "{mx}", y: "{my}", class: "lattice-edge-label", text_anchor: "middle", "{label}" }
                                }
                            }
                        }
                    }
                }
                // Entity nodes.
                for (i, ent) in entities.iter().enumerate() {
                    {
                        let (cx, cy) = pos[i];
                        let r = node_radius(ent.degree);
                        let side = sides[i];
                        let ly = cy + side * (r + 11.0);
                        let name_hover = ent.name.clone();
                        let name_leave = ent.name.clone();
                        let show_label = always_on || hovered() == Some(ent.name.clone());
                        rsx! {
                            g { class: "mem-node",
                                onmouseenter: move |_| hovered.set(Some(name_hover.clone())),
                                onmouseleave: move |_| {
                                    if hovered() == Some(name_leave.clone()) {
                                        hovered.set(None);
                                    }
                                },
                                circle { cx: "{cx}", cy: "{cy}", r: "{r}" }
                                if show_label {
                                    text { x: "{cx}", y: "{ly}", text_anchor: "middle", "{ent.name}" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn knowledge_graph_query() -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-knowledge-graph".to_string(),
        payload: QueryPayload::GetKnowledgeGraph { limit: 80 },
    }
}

// ── MG — the knowledge-graph view (force-directed layout, in-WASM). ──

/// The graph canvas size (SVG viewBox units).
const GRAPH_W: f64 = 760.0;
const GRAPH_H: f64 = 460.0;

/// A stable, deterministic hash of a topic name — seeds the layout so the graph
/// doesn't jitter between renders.
fn stable_hash(s: &str) -> u64 {
    // FNV-1a.
    let mut h: u64 = 0xcbf29ce484222325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Deterministic Fruchterman–Reingold layout: repulsion between all nodes,
/// attraction along (weighted) edges, cooled over a fixed iteration count.
/// Returns one `(x, y)` per node, index-aligned with `nodes`.
fn compute_layout(nodes: &[MemoryGraphNode], edges: &[PairScore]) -> Vec<(f64, f64)> {
    let n = nodes.len();
    if n == 0 {
        return Vec::new();
    }
    let k = (GRAPH_W * GRAPH_H / n as f64).sqrt() * 0.55; // ideal edge length
    // Seed positions on a spiral from a stable per-topic hash.
    let mut pos: Vec<(f64, f64)> = nodes
        .iter()
        .map(|node| {
            let h = stable_hash(&node.topic);
            let ang = (h % 628) as f64 / 100.0; // 0..2π
            let r = 40.0 + (h / 628 % 170) as f64;
            (GRAPH_W / 2.0 + r * ang.cos(), GRAPH_H / 2.0 + r * ang.sin())
        })
        .collect();
    let idx: std::collections::HashMap<&str, usize> =
        nodes.iter().enumerate().map(|(i, nd)| (nd.topic.as_str(), i)).collect();

    let mut temp = GRAPH_W / 8.0;
    for _ in 0..220 {
        let mut disp = vec![(0.0_f64, 0.0_f64); n];
        // Repulsion (all pairs).
        for i in 0..n {
            for j in (i + 1)..n {
                let dx = pos[i].0 - pos[j].0;
                let dy = pos[i].1 - pos[j].1;
                let d = (dx * dx + dy * dy).sqrt().max(0.01);
                let f = k * k / d;
                let (ux, uy) = (dx / d * f, dy / d * f);
                disp[i].0 += ux;
                disp[i].1 += uy;
                disp[j].0 -= ux;
                disp[j].1 -= uy;
            }
        }
        // Attraction along edges (stronger for higher-affinity pairs).
        for e in edges {
            if let (Some(&i), Some(&j)) = (idx.get(e.a.as_str()), idx.get(e.b.as_str())) {
                let w = (e.score.max(0.1) as f64).min(4.0);
                let dx = pos[i].0 - pos[j].0;
                let dy = pos[i].1 - pos[j].1;
                let d = (dx * dx + dy * dy).sqrt().max(0.01);
                let f = d * d / k * (0.5 + 0.25 * w);
                let (ux, uy) = (dx / d * f, dy / d * f);
                disp[i].0 -= ux;
                disp[i].1 -= uy;
                disp[j].0 += ux;
                disp[j].1 += uy;
            }
        }
        // Apply, capped by temperature, clamped to the canvas.
        for i in 0..n {
            let dl = (disp[i].0 * disp[i].0 + disp[i].1 * disp[i].1).sqrt().max(0.01);
            let mv = dl.min(temp);
            pos[i].0 = (pos[i].0 + disp[i].0 / dl * mv).clamp(28.0, GRAPH_W - 28.0);
            pos[i].1 = (pos[i].1 + disp[i].1 / dl * mv).clamp(28.0, GRAPH_H - 28.0);
        }
        temp *= 0.965;
    }
    pos
}

/// Node radius from entry count (sqrt-scaled, clamped).
fn node_radius(entry_count: u32) -> f64 {
    (6.0 + (entry_count as f64).sqrt() * 3.0).min(24.0)
}

/// Approximate on-screen width of a label in SVG viewBox units. No real
/// text-measurement API exists outside the DOM, so this is a fixed
/// per-character estimate tuned to `.mem-node text`'s font-size — a
/// heuristic for a collision *check*, not a pixel-perfect layout.
const LABEL_CHAR_WIDTH: f64 = 6.0;
const LABEL_HEIGHT: f64 = 12.0;

fn label_width(label: &str) -> f64 {
    label.chars().count() as f64 * LABEL_CHAR_WIDTH
}

/// Two labels "collide" when their approximate bounding boxes — centered
/// on `(ax, ay)`/`(bx, by)`, `label_width` wide, `LABEL_HEIGHT` tall —
/// overlap.
fn labels_collide(ax: f64, ay: f64, a_label: &str, bx: f64, by: f64, b_label: &str) -> bool {
    let (aw, bw) = (label_width(a_label), label_width(b_label));
    let dx = (ax - bx).abs();
    let dy = (ay - by).abs();
    dx < (aw + bw) / 2.0 && dy < LABEL_HEIGHT
}

/// One label placement side per node, in `nodes`/`pos` order: `1.0` places
/// the label below the node (today's only behavior), `-1.0` places it
/// above. A node's label goes above only when placing it below would
/// collide with an EARLIER node's label at that node's own decided side —
/// a single greedy left-to-right pass, not a full layout solve, but
/// enough to break the dense-cluster case that made every label overlap.
/// Shared by `MemoryGraph` and `LatticeGraph` (the latter already builds
/// a `Vec<MemoryGraphNode>` locally to reuse `compute_layout`, and reuses
/// this the same way).
fn label_sides(nodes: &[MemoryGraphNode], pos: &[(f64, f64)]) -> Vec<f64> {
    let mut sides: Vec<f64> = Vec::with_capacity(nodes.len());
    for i in 0..nodes.len() {
        let (xi, yi) = pos[i];
        let ri = node_radius(nodes[i].entry_count);
        let below = yi + ri + 11.0;
        let collides = (0..i).any(|j| {
            let (xj, yj) = pos[j];
            let rj = node_radius(nodes[j].entry_count);
            let yj_label = yj + sides[j] * (rj + 11.0);
            labels_collide(xi, below, &nodes[i].topic, xj, yj_label, &nodes[j].topic)
        });
        sides.push(if collides { -1.0 } else { 1.0 });
    }
    sides
}

#[cfg(test)]
mod graph_label_tests {
    use super::*;

    fn node(topic: &str, entry_count: u32) -> MemoryGraphNode {
        MemoryGraphNode { topic: topic.to_string(), entry_count }
    }

    #[test]
    fn far_apart_labels_both_go_below() {
        let nodes = vec![node("alpha", 1), node("beta", 1)];
        let pos = vec![(0.0, 0.0), (500.0, 400.0)];
        let sides = label_sides(&nodes, &pos);
        assert_eq!(sides, vec![1.0, 1.0]);
    }

    #[test]
    fn close_labels_alternate_to_avoid_collision() {
        let nodes = vec![node("alpha", 1), node("beta", 1)];
        // Same y, close x — "below" placement for both would overlap.
        let pos = vec![(100.0, 100.0), (108.0, 100.0)];
        let sides = label_sides(&nodes, &pos);
        assert_eq!(sides[0], 1.0);
        assert_eq!(sides[1], -1.0);
    }
}

/// The memory knowledge graph — a force-directed SVG of topic nodes (sized by
/// entry count) + weighted co-occurrence edges. Clicking a node selects that
/// topic. Read-only. The layout is deterministic (no animation loop).
#[component]
fn MemoryGraph(
    nodes: Vec<MemoryGraphNode>,
    edges: Vec<PairScore>,
    on_select: EventHandler<String>,
) -> Element {
    // Memoized on `nodes`/`edges` content (not on the `zoom`/`pan`/
    // `dragging`/`hovered` signals read further down in this same
    // component) — otherwise every pan-drag `onmousemove` or hover
    // enter/leave re-renders this component and would re-run the O(n²)
    // force-directed layout for no reason, since node positions only
    // depend on the graph's own nodes/edges, never the viewport or hover
    // state.
    let layout = use_memo(use_reactive!(|nodes, edges| {
        let pos = compute_layout(&nodes, &edges);
        let sides = label_sides(&nodes, &pos);
        (pos, sides)
    }));
    let (pos, sides) = layout();
    // POLISH_WAVES.md sub-project 6, item C — past this many nodes,
    // always-on labels overlap into an unreadable smear; show a label
    // only for the hovered node instead.
    const LABEL_ALWAYS_ON_MAX: usize = 25;
    let always_on = nodes.len() <= LABEL_ALWAYS_ON_MAX;
    let mut hovered = use_signal(|| None::<String>);
    // POLISH_WAVES.md sub-project 6, item C — pan/zoom so a dense graph
    // can be explored instead of always fit-to-canvas. `zoom` scales the
    // visible viewBox (>1.0 = zoomed in / a smaller visible area); `pan`
    // is the viewBox's top-left corner in the same SVG-unit space.
    let mut zoom = use_signal(|| 1.0_f64);
    let mut pan = use_signal(|| (0.0_f64, 0.0_f64));
    let mut dragging = use_signal(|| None::<(f64, f64)>);
    let (vb_x, vb_y) = pan();
    let vb_w = GRAPH_W / zoom();
    let vb_h = GRAPH_H / zoom();
    let idx: std::collections::HashMap<&str, usize> =
        nodes.iter().enumerate().map(|(i, nd)| (nd.topic.as_str(), i)).collect();
    let max_score = edges.iter().map(|e| e.score).fold(0.1_f32, f32::max);

    rsx! {
        div { class: "glass-card mem-graph-card",
            if edges.is_empty() {
                p { class: "label-tech sub", "No co-occurrence links yet — topics appear as a cloud until the agent recalls them together." }
            }
            svg {
                class: "mem-graph",
                view_box: "{vb_x} {vb_y} {vb_w} {vb_h}",
                onwheel: move |e| {
                    e.prevent_default();
                    let dy = e.delta().strip_units().y;
                    let factor = if dy > 0.0 { 0.9 } else { 1.1 };
                    let z = (zoom() * factor).clamp(0.4, 3.0);
                    zoom.set(z);
                },
                onmousedown: move |e| {
                    let p = e.client_coordinates();
                    dragging.set(Some((p.x, p.y)));
                },
                onmousemove: move |e| {
                    if let Some((sx, sy)) = dragging() {
                        let p = e.client_coordinates();
                        let (dx, dy) = (p.x - sx, p.y - sy);
                        // Drag right/down should move the *view* left/up
                        // (the content should follow the cursor), and the
                        // delta is in screen pixels while pan is in
                        // viewBox units — scale by the current zoom so a
                        // drag feels the same speed at any zoom level.
                        let (px, py) = pan();
                        pan.set((px - dx / zoom(), py - dy / zoom()));
                        dragging.set(Some((p.x, p.y)));
                    }
                },
                onmouseup: move |_| dragging.set(None),
                // A simpler fallback for "the drag ended off-element"
                // than a window-level listener: releasing outside the
                // SVG just stops the pan, it doesn't need to resume.
                onmouseleave: move |_| dragging.set(None),
                // Edges first (under the nodes).
                for e in edges.iter() {
                    if let (Some(&i), Some(&j)) = (idx.get(e.a.as_str()), idx.get(e.b.as_str())) {
                        {
                            let (x1, y1) = pos[i];
                            let (x2, y2) = pos[j];
                            let frac = (e.score / max_score).clamp(0.1, 1.0) as f64;
                            let w = 0.6 + frac * 3.4;
                            let op = 0.08 + frac * 0.4;
                            rsx! {
                                line {
                                    x1: "{x1}", y1: "{y1}", x2: "{x2}", y2: "{y2}",
                                    class: "mem-edge",
                                    stroke_width: "{w}", opacity: "{op}",
                                }
                            }
                        }
                    }
                }
                // Nodes.
                for (i, node) in nodes.iter().enumerate() {
                    {
                        let (cx, cy) = pos[i];
                        let r = node_radius(node.entry_count);
                        let side = sides[i];
                        let ly = cy + side * (r + 11.0);
                        let topic = node.topic.clone();
                        let topic_hover = node.topic.clone();
                        let topic_leave = node.topic.clone();
                        let show_label = always_on || hovered() == Some(node.topic.clone());
                        rsx! {
                            g { class: "mem-node",
                                onclick: move |_| on_select.call(topic.clone()),
                                onmouseenter: move |_| hovered.set(Some(topic_hover.clone())),
                                onmouseleave: move |_| {
                                    if hovered() == Some(topic_leave.clone()) {
                                        hovered.set(None);
                                    }
                                },
                                circle { cx: "{cx}", cy: "{cy}", r: "{r}" }
                                if show_label {
                                    text { x: "{cx}", y: "{ly}", text_anchor: "middle", "{node.topic}" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Human label for the active memory scope.
fn scope_label(scope: &str) -> String {
    if scope == "recent" {
        "Recent · all topics".to_string()
    } else if let Some(t) = scope.strip_prefix("topic:") {
        format!("Topic · {t}")
    } else if let Some(q) = scope.strip_prefix("search:") {
        if q.is_empty() {
            "Recent · all topics".to_string()
        } else {
            format!("Search · \"{q}\"")
        }
    } else {
        scope.to_string()
    }
}

/// `rel_time` for a unix-**seconds** timestamp (memory entries store seconds).
fn rel_time_secs(secs: u64) -> String {
    rel_time(secs.saturating_mul(1000))
}

/// Relative time to a FUTURE unix-ms timestamp ("in 6h", "in 2d"); past/now →
/// "due". Used for a routine's next scheduled fire.
fn until_time(ms: u64) -> String {
    let now = js_sys::Date::now() as u64;
    if ms == 0 || ms <= now {
        return "due".to_string();
    }
    let secs = (ms - now) / 1000;
    if secs < 60 {
        format!("in {secs}s")
    } else if secs < 3600 {
        format!("in {}m", secs / 60)
    } else if secs < 86_400 {
        format!("in {}h", secs / 3600)
    } else {
        format!("in {}d", secs / 86_400)
    }
}

// ---------------------------------------------------------------------------
// Settings — the first config write surface (Chapter U)
// ---------------------------------------------------------------------------

#[component]
fn SettingsPanel() -> Element {
    let ws = use_context::<Sender>();
    let settings = use_context::<Signal<SettingsState>>();
    // POLISH_WAVES.md sub-project 7 plan 3 — the notify-target list
    // (plan 2) feeds the "Proactive surfacing" target picker below.
    let notifications = use_context::<Signal<NotificationsState>>();

    // Editable form state, seeded from the on-disk snapshot.
    let mut level = use_signal(String::new);
    let mut root = use_signal(String::new);
    let mut per_run = use_signal(String::new);
    let mut per_day = use_signal(String::new);
    let mut on_exceeded = use_signal(|| "deny".to_string());
    let mut alert_at = use_signal(String::new);
    let mut confirm_open = use_signal(|| false);
    // Chapter Reins — the autonomy dial (level picker + confirm-on-autonomy).
    let mut auto_level = use_signal(String::new);
    let mut auto_confirm_open = use_signal(|| false);
    // The snapshot the form was last seeded from — so a write *error* (snapshot
    // unchanged) doesn't wipe the operator's in-progress edits.
    let mut last_seed = use_signal(|| None::<SettingsSnapshot>);

    // Load the current settings when the view opens.
    use_future(move || async move {
        ws.send(get_settings_query());
        ws.send(memory_profile_config_query());
        ws.send(embedding_config_query());
        ws.send(proactive_config_query());
        ws.send(notify_target_configs_query());
    });

    // Seed the form whenever the snapshot content changes (first load + after a
    // successful write), but not on a notice-only change.
    use_effect(move || {
        let snap = settings().snapshot.clone();
        if snap != last_seed() {
            if let Some(s) = snap.as_ref() {
                level.set(s.access_level.clone());
                root.set(
                    if s.access_level == "workspace" || s.access_level == "custom" {
                        s.fs_root.clone()
                    } else {
                        String::new()
                    },
                );
                per_run.set(s.budget.per_run_usd.map(|v| v.to_string()).unwrap_or_default());
                per_day.set(s.budget.per_day_usd.map(|v| v.to_string()).unwrap_or_default());
                on_exceeded.set(s.budget.on_exceeded.clone());
                alert_at.set(s.budget.alert_at.map(|v| v.to_string()).unwrap_or_default());
                auto_level.set(s.autonomy_level.clone());
            }
            last_seed.set(snap);
        }
    });

    let st = settings();
    let snap = match st.snapshot.clone() {
        Some(s) => s,
        None => {
            return rsx! {
                div { class: "settings",
                    div { class: "glass-card empty",
                        p { class: "label-tech", "Loading settings…" }
                    }
                }
            }
        }
    };

    let needs_root = level() == "workspace" || level() == "custom";
    let expanded = level() != "sandbox";
    let cycle_on = snap.cycle_detection;
    // Chapter Reins — the autonomy-granting levels confirm first (server-side too).
    let auto_grants = auto_level() == "autonomous" || auto_level() == "unleashed";

    rsx! {
        div { class: "settings",

            if st.restart_required {
                div { class: "glass-card restart-banner",
                    strong { "Saved — restart the daemon to apply." }
                    p { class: "label-tech",
                        "Settings are read once at startup. Run  "
                        code { "aivyx-pa daemon stop && aivyx-pa daemon run" }
                    }
                }
            }

            if let Some((ok, msg)) = st.notice.clone() {
                div { class: if ok { "notice ok" } else { "notice err" }, "{msg}" }
            }

            // ── Access level (editable, confirm-first on expansion) ──
            div { class: "glass-card settings-section",
                div { class: "panel-head",
                    h3 { "Access level" }
                    span { class: "chip", "{snap.access_level}" }
                }
                p { class: "label-tech",
                    "How far the agent can reach on disk. Expanding beyond the sandbox is confirmed first."
                }
                div { class: "field-row",
                    label { class: "label-tech", "Level" }
                    select {
                        class: "input",
                        value: "{level}",
                        onchange: move |e| level.set(e.value()),
                        option { value: "sandbox", "sandbox — ~/aivyx-pa-sandbox" }
                        option { value: "workspace", "workspace — a chosen directory" }
                        option { value: "home", "home — your home directory" }
                        option { value: "full", "full — the whole machine" }
                        option { value: "custom", "custom — a chosen directory" }
                    }
                }
                if needs_root {
                    div { class: "field-row",
                        label { class: "label-tech", "Root" }
                        input {
                            class: "input",
                            placeholder: "/path/to/directory",
                            value: "{root}",
                            oninput: move |e| root.set(e.value()),
                        }
                    }
                }
                p { class: "label-tech sub", "Current reach: {snap.fs_root}" }
                div { class: "actions",
                    button {
                        class: "btn btn-primary",
                        onclick: move |_| {
                            if expanded {
                                confirm_open.set(true);
                            } else {
                                ws.send(set_access_query(level(), None, false));
                            }
                        },
                        "Apply access level"
                    }
                }
            }

            // ── Autonomy (editable, confirm-first on autonomy-granting levels) ──
            div { class: "glass-card settings-section",
                div { class: "panel-head",
                    h3 { "Autonomy" }
                    span { class: "chip", "{snap.autonomy_level}" }
                }
                p { class: "label-tech",
                    "How autonomous the agent is. One dial that composes the safety \
                     knobs; supervised and above arm the autonomous loop. Granting \
                     unattended autonomy is confirmed first."
                }
                div { class: "field-row",
                    label { class: "label-tech", "Level" }
                    select {
                        class: "input",
                        value: "{auto_level}",
                        onchange: move |e| auto_level.set(e.value()),
                        option { value: "manual", "manual — confirm everything" }
                        option { value: "assisted", "assisted — reversible free, irreversible confirmed (default)" }
                        option { value: "supervised", "supervised — armed loop, a human nearby" }
                        option { value: "autonomous", "autonomous — pursues goals unattended (capped)" }
                        option { value: "unleashed", "unleashed — isolated host, eyes-open" }
                    }
                }
                p { class: "label-tech sub",
                    "Per-domain overrides and the auto-approve allowlist are edited in "
                    code { "aivyx-pa.toml" }
                    " for now. Takes effect on the next restart."
                }
                div { class: "actions",
                    button {
                        class: "btn btn-primary",
                        onclick: move |_| {
                            if auto_grants {
                                auto_confirm_open.set(true);
                            } else {
                                ws.send(set_autonomy_query(auto_level(), false));
                            }
                        },
                        "Apply autonomy level"
                    }
                }
            }

            // ── Budget (editable) ──
            div { class: "glass-card settings-section",
                div { class: "panel-head", h3 { "Budget" } }
                p { class: "label-tech", "Dollar caps on spend. Leave a cap blank for unlimited." }
                div { class: "field-row",
                    label { class: "label-tech", "Per run ($)" }
                    input {
                        class: "input", r#type: "number", placeholder: "unlimited",
                        value: "{per_run}", oninput: move |e| per_run.set(e.value()),
                    }
                }
                div { class: "field-row",
                    label { class: "label-tech", "Per day ($)" }
                    input {
                        class: "input", r#type: "number", placeholder: "unlimited",
                        value: "{per_day}", oninput: move |e| per_day.set(e.value()),
                    }
                }
                div { class: "field-row",
                    label { class: "label-tech", "On exceeded" }
                    select {
                        class: "input", value: "{on_exceeded}",
                        onchange: move |e| on_exceeded.set(e.value()),
                        option { value: "deny", "deny — block the call" }
                        option { value: "alert", "alert — warn, proceed" }
                    }
                }
                div { class: "field-row",
                    label { class: "label-tech", "Alert at (0–1)" }
                    input {
                        class: "input", r#type: "number", placeholder: "0.8",
                        value: "{alert_at}", oninput: move |e| alert_at.set(e.value()),
                    }
                }
                div { class: "actions",
                    button {
                        class: "btn btn-primary",
                        onclick: move |_| ws.send(set_budget_query(
                            parse_opt_f64(&per_run()),
                            parse_opt_f64(&per_day()),
                            Some(on_exceeded()),
                            parse_opt_f64(&alert_at()),
                        )),
                        "Save budget"
                    }
                }
            }

            // ── Agent loop safety (editable) ──
            div { class: "glass-card settings-section",
                div { class: "panel-head", h3 { "Agent" } }
                p { class: "label-tech",
                    "Loop protection. Stops the agent if it falls into a repeating cycle \
                     of the same actions (e.g. A→B→A→B) instead of making progress."
                }
                div { class: "field-row",
                    label { class: "label-tech", "Cycle breaker" }
                    div { class: "toggle-line",
                        span { class: "chip", {if cycle_on { "on" } else { "off" }} }
                        button {
                            class: "btn btn-glass",
                            onclick: move |_| ws.send(set_cycle_detection_query(!cycle_on)),
                            {if cycle_on { "Disable" } else { "Enable" }}
                        }
                    }
                }
                p { class: "label-tech sub",
                    "Off by default for the interactive agent; autonomous team agents always \
                     have it on. Takes effect on the next restart."
                }
            }

            // ── Provider / model (read-only — change via `aivyx-pa init`) ──
            div { class: "glass-card settings-section",
                div { class: "panel-head", h3 { "Model" } span { class: "chip", "read-only" } }
                div { class: "kv-grid",
                    div { span { class: "label-tech", "Provider" } div { "{snap.provider}" } }
                    div { span { class: "label-tech", "Model" } div { "{snap.model}" } }
                    div {
                        span { class: "label-tech", "Context" }
                        div { {snap.num_ctx.map(|n| n.to_string()).unwrap_or_else(|| "default".to_string())} }
                    }
                    div {
                        span { class: "label-tech", "Embeddings" }
                        div { {if snap.embeddings_available { "available" } else { "off" }} }
                    }
                }
                p { class: "label-tech sub", "Change the provider, model, or keys with  " code { "aivyx-pa init" } }
            }

            if let Some(cfg) = settings().memory_profile.clone() {
                MemoryProfileCard { key: "{cfg:?}", config: cfg.clone() }
            }
            if let Some(cfg) = settings().embedding.clone() {
                EmbeddingConfigCard { key: "{cfg:?}", config: cfg.clone() }
            }
            if let Some(cfg) = settings().proactive.clone() {
                ProactiveConfigCard { key: "{cfg:?}", config: cfg.clone(), targets: notifications().configs.clone() }
            }
        }

        // Confirm-first modal for expanded access levels (Chapter N posture).
        if confirm_open() {
            div { class: "modal-scrim",
                div { class: "glass-card modal",
                    h3 { "Grant '{level()}' access?" }
                    p { "{confirm_blurb(&level())}" }
                    div { class: "actions",
                        button { class: "btn btn-glass", onclick: move |_| confirm_open.set(false), "Cancel" }
                        button {
                            class: "btn btn-primary",
                            onclick: move |_| {
                                let r = if needs_root { Some(root()) } else { None };
                                ws.send(set_access_query(level(), r, true));
                                confirm_open.set(false);
                            },
                            "Grant access"
                        }
                    }
                }
            }
        }

        // Confirm-first modal for the autonomy-granting levels (Chapter Reins).
        if auto_confirm_open() {
            div { class: "modal-scrim",
                div { class: "glass-card modal",
                    h3 { "Set autonomy to '{auto_level()}'?" }
                    p { "{autonomy_confirm_blurb(&auto_level())}" }
                    div { class: "actions",
                        button { class: "btn btn-glass", onclick: move |_| auto_confirm_open.set(false), "Cancel" }
                        button {
                            class: "btn btn-primary",
                            onclick: move |_| {
                                ws.send(set_autonomy_query(auto_level(), true));
                                auto_confirm_open.set(false);
                            },
                            "Set autonomy"
                        }
                    }
                }
            }
        }
    }
}

fn get_settings_query() -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-settings-get".to_string(),
        payload: QueryPayload::GetSettings,
    }
}

/// POLISH_WAVES.md sub-project 7 plan 3 — the Settings-coverage query
/// builders. All 3 sections share the "mc-settings" id prefix, which
/// the existing `id.starts_with("mc-settings")` `QueryError` routing
/// arm already catches (same tradeoff `mc-mcp`/`mc-notify` already
/// make for their own status/poll ids).
fn memory_profile_config_query() -> FrontendMessage {
    FrontendMessage::Query { id: "mc-settings-memory".to_string(), payload: QueryPayload::GetMemoryProfileConfig }
}
fn set_memory_profile_query(profile: String) -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-settings-memory".to_string(),
        payload: QueryPayload::SetMemoryProfile { profile },
    }
}
fn embedding_config_query() -> FrontendMessage {
    FrontendMessage::Query { id: "mc-settings-embedding".to_string(), payload: QueryPayload::GetEmbeddingConfig }
}
fn set_embedding_config_query(base_url: Option<String>, model: Option<String>, api_key: Option<String>) -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-settings-embedding".to_string(),
        payload: QueryPayload::SetEmbeddingConfig { base_url, model, api_key },
    }
}
fn proactive_config_query() -> FrontendMessage {
    FrontendMessage::Query { id: "mc-settings-proactive".to_string(), payload: QueryPayload::GetProactiveConfig }
}
fn set_proactive_config_query(
    enabled: Option<bool>,
    target: Option<String>,
    max_per_window: Option<u32>,
    window_secs: Option<u64>,
) -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-settings-proactive".to_string(),
        payload: QueryPayload::SetProactiveConfig { enabled, target, max_per_window, window_secs },
    }
}

fn set_access_query(level: String, root: Option<String>, confirm: bool) -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-settings-access".to_string(),
        payload: QueryPayload::SetAccessLevel { level, root, confirm },
    }
}

fn set_budget_query(
    per_run_usd: Option<f64>,
    per_day_usd: Option<f64>,
    on_exceeded: Option<String>,
    alert_at: Option<f64>,
) -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-settings-budget".to_string(),
        payload: QueryPayload::SetBudget { per_run_usd, per_day_usd, on_exceeded, alert_at },
    }
}

fn set_cycle_detection_query(enabled: bool) -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-settings-cycle".to_string(),
        payload: QueryPayload::SetCycleDetection { enabled },
    }
}

fn set_autonomy_query(level: String, confirm: bool) -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-settings-autonomy".to_string(),
        payload: QueryPayload::SetAutonomyLevel { level, confirm },
    }
}

/// One-line risk blurb for the autonomy confirm modal (Chapter Reins).
fn autonomy_confirm_blurb(level: &str) -> &'static str {
    match level {
        "autonomous" => {
            "The agent will pursue goals unattended within its caps. Irreversible \
             actions are still refused without a human."
        }
        "unleashed" => {
            "Runs an armed, self-directing agent with confirm-first OFF. Intended \
             only for a dedicated, isolated host where the agent's blast radius is \
             the host."
        }
        _ => "This grants the agent unattended autonomy.",
    }
}

/// Parse a numeric form field: empty ⇒ `None` (clear / unlimited), unparseable
/// ⇒ `None` (the daemon validates and reports anything truly wrong).
fn parse_opt_f64(s: &str) -> Option<f64> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        t.parse().ok()
    }
}

/// The confirm-modal blurb for an expanded access level.
fn confirm_blurb(level: &str) -> String {
    match level {
        "full" => "This grants access to the ENTIRE filesystem, including system files.".to_string(),
        "home" => "This grants full read / write / shell across your home directory.".to_string(),
        "workspace" | "custom" => {
            "This grants full read / write / shell within the chosen directory.".to_string()
        }
        _ => "This reaches beyond the default sandbox.".to_string(),
    }
}

// ---------------------------------------------------------------------------
// Voice — the host voice channel, configured (Chapter Voice). A config-write
// screen for [voice] + a readiness check + the launch command. The audio loop
// (mic/ASR/TTS) is a host process; this screen never touches audio.
// ---------------------------------------------------------------------------

#[component]
fn VoicePanel() -> Element {
    let ws = use_context::<Sender>();
    let voice = use_context::<Signal<VoiceState>>();

    let mut asr_engine = use_signal(String::new);
    let mut tts_engine = use_signal(String::new);
    let mut asr_model_path = use_signal(String::new);
    let mut asr_language = use_signal(String::new);
    let mut asr_beam_size = use_signal(String::new);
    let mut tts_model_dir = use_signal(String::new);
    let mut tts_voice_name = use_signal(String::new);
    let mut tts_speed = use_signal(String::new);
    let mut input_device = use_signal(String::new);
    let mut output_device = use_signal(String::new);
    let mut last_seed = use_signal(|| None::<VoiceSettingsSnapshot>);

    use_future(move || async move {
        ws.send(get_voice_query());
    });

    // Re-seed the form from the on-disk snapshot (first load + after a write),
    // guarded so a write error doesn't wipe in-progress edits.
    use_effect(move || {
        let snap = voice().snapshot.clone();
        if snap != last_seed() {
            if let Some(s) = snap.as_ref() {
                asr_engine.set(s.asr_engine.clone().unwrap_or_default());
                tts_engine.set(s.tts_engine.clone().unwrap_or_default());
                asr_model_path.set(s.asr_model_path.clone().unwrap_or_default());
                asr_language.set(s.asr_language.clone().unwrap_or_default());
                asr_beam_size.set(s.asr_beam_size.map(|n| n.to_string()).unwrap_or_default());
                tts_model_dir.set(s.tts_model_dir.clone().unwrap_or_default());
                tts_voice_name.set(s.tts_voice_name.clone().unwrap_or_default());
                tts_speed.set(s.tts_speed.map(|n| n.to_string()).unwrap_or_default());
                input_device.set(s.input_device.clone().unwrap_or_default());
                output_device.set(s.output_device.clone().unwrap_or_default());
            }
            last_seed.set(snap);
        }
    });

    let st = voice();
    let snap = match st.snapshot.clone() {
        Some(s) => s,
        None => {
            return rsx! {
                div { class: "settings voice",
                    div { class: "glass-card empty", p { class: "label-tech", "Loading voice config…" } }
                }
            }
        }
    };

    rsx! {
        div { class: "settings voice",

            if st.restart_required {
                div { class: "glass-card restart-banner",
                    strong { "Saved — restart voice to apply." }
                    p { class: "label-tech",
                        "[voice] is read when the voice channel starts. Stop and re-run  "
                        code { "aivyx-pa --channel voice" }
                    }
                }
            }
            if let Some((ok, msg)) = st.notice.clone() {
                div { class: if ok { "notice ok" } else { "notice err" }, "{msg}" }
            }

            // ── Readiness ──
            div { class: "glass-card settings-section",
                div { class: "panel-head", h3 { "Readiness" } }
                p { class: "label-tech",
                    "Voice runs on the host (microphone + speakers). These files must exist before you launch."
                }
                div { class: "kv-grid",
                    ReadinessRow { label: "Whisper model", status: snap.asr_model_status.clone() }
                    ReadinessRow { label: "Kokoro model (.onnx)", status: snap.tts_model_status.clone() }
                    ReadinessRow { label: "Kokoro voices (.bin)", status: snap.tts_voices_status.clone() }
                }
            }

            // ── Models & engines ──
            div { class: "glass-card settings-section",
                div { class: "panel-head", h3 { "Models & engines" } }
                div { class: "field-row",
                    label { class: "label-tech", "ASR engine" }
                    select { class: "input", value: "{asr_engine}", onchange: move |e| asr_engine.set(e.value()),
                        option { value: "", "default (whisper-rs)" }
                        option { value: "whisper-rs", "whisper-rs" }
                        option { value: "whisper-cpp-plus", "whisper-cpp-plus" }
                    }
                }
                div { class: "field-row",
                    label { class: "label-tech", "Whisper model" }
                    input { class: "input", placeholder: "/path/to/ggml-model.bin",
                        value: "{asr_model_path}", oninput: move |e| asr_model_path.set(e.value()) }
                }
                div { class: "field-row",
                    label { class: "label-tech", "TTS engine" }
                    select { class: "input", value: "{tts_engine}", onchange: move |e| tts_engine.set(e.value()),
                        option { value: "", "default (kokoro)" }
                        option { value: "kokoro", "kokoro" }
                    }
                }
                div { class: "field-row",
                    label { class: "label-tech", "Kokoro model dir" }
                    input { class: "input", placeholder: "/path/to/kokoro (holds the .onnx + voices-*.bin)",
                        value: "{tts_model_dir}", oninput: move |e| tts_model_dir.set(e.value()) }
                }
                div { class: "field-row",
                    label { class: "label-tech", "Kokoro voice" }
                    input { class: "input", placeholder: "af_heart",
                        value: "{tts_voice_name}", oninput: move |e| tts_voice_name.set(e.value()) }
                }
                div { class: "field-row",
                    label { class: "label-tech", "Speaking rate" }
                    input { class: "input", r#type: "number", placeholder: "1.0",
                        value: "{tts_speed}", oninput: move |e| tts_speed.set(e.value()) }
                }
                div { class: "field-row",
                    label { class: "label-tech", "ASR language" }
                    input { class: "input", placeholder: "en (or auto)",
                        value: "{asr_language}", oninput: move |e| asr_language.set(e.value()) }
                }
                div { class: "field-row",
                    label { class: "label-tech", "Beam size" }
                    input { class: "input", r#type: "number", placeholder: "5",
                        value: "{asr_beam_size}", oninput: move |e| asr_beam_size.set(e.value()) }
                }
            }

            // ── Audio devices ──
            div { class: "glass-card settings-section",
                div { class: "panel-head", h3 { "Audio devices" } span { class: "chip muted", "optional" } }
                div { class: "field-row",
                    label { class: "label-tech", "Input" }
                    input { class: "input", placeholder: "system default",
                        value: "{input_device}", oninput: move |e| input_device.set(e.value()) }
                }
                div { class: "field-row",
                    label { class: "label-tech", "Output" }
                    input { class: "input", placeholder: "system default",
                        value: "{output_device}", oninput: move |e| output_device.set(e.value()) }
                }
            }

            // ── Launch ──
            div { class: "glass-card settings-section",
                div { class: "panel-head", h3 { "Launch" } }
                p { class: "label-tech", "Start voice as its own foreground process on this machine:" }
                pre { class: "launch-cmd", "aivyx-pa --channel voice" }
                p { class: "label-tech sub", "The audio loop (mic → Whisper → agent → Kokoro → speakers) runs on the host, not in the browser." }
            }

            div { class: "actions sticky-save",
                button {
                    class: "btn btn-primary",
                    onclick: move |_| ws.send(set_voice_query(
                        opt_str(&asr_engine()), opt_str(&tts_engine()), opt_str(&asr_model_path()),
                        opt_str(&asr_language()), parse_opt_u32(&asr_beam_size()), opt_str(&tts_model_dir()),
                        opt_str(&tts_voice_name()), parse_opt_f32(&tts_speed()),
                        opt_str(&input_device()), opt_str(&output_device()),
                    )),
                    "Save voice config"
                }
            }
        }
    }
}

/// One readiness row — a label + a status chip (present = sage, missing = error,
/// unset = muted). A `.kv-grid` cell.
#[component]
fn ReadinessRow(label: String, status: String) -> Element {
    let (cls, txt) = match status.as_str() {
        "present" => ("sage", "present"),
        "missing" => ("error", "missing"),
        _ => ("muted", "not set"),
    };
    rsx! {
        div {
            span { class: "label-tech", "{label}" }
            div { span { class: "chip {cls}", "{txt}" } }
        }
    }
}

fn get_voice_query() -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-voice-get".to_string(),
        payload: QueryPayload::GetVoiceSettings,
    }
}

#[allow(clippy::too_many_arguments)]
fn set_voice_query(
    asr_engine: Option<String>,
    tts_engine: Option<String>,
    asr_model_path: Option<String>,
    asr_language: Option<String>,
    asr_beam_size: Option<u32>,
    tts_model_dir: Option<String>,
    tts_voice_name: Option<String>,
    tts_speed: Option<f32>,
    input_device: Option<String>,
    output_device: Option<String>,
) -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-voice-set".to_string(),
        payload: QueryPayload::SetVoice {
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
        },
    }
}

/// Parse a numeric form field to `Option<u32>` (blank / unparseable ⇒ `None`).
fn parse_opt_u32(s: &str) -> Option<u32> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        t.parse().ok()
    }
}

/// Parse a numeric form field to `Option<f32>` (blank / unparseable ⇒ `None`).
fn parse_opt_f32(s: &str) -> Option<f32> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        t.parse().ok()
    }
}

// ---------------------------------------------------------------------------
// Agents — the identity editor (Chapter V)
//
// V.3 ships the **Profile editor** half: the operator-declared `[profile]`
// layer (assistant name, operator context, communication style, and three
// declared lists). A save round-trips through `SetProfile`, the daemon rewrites
// `[profile]` in `aivyx-pa.toml`, and — because Profile is read at startup — the
// screen shows the same "restart to apply" banner the Settings writes do. The
// agent's self-learned *Persona* governance panel is V.4.
// ---------------------------------------------------------------------------

#[component]
fn AgentsPanel() -> Element {
    let ws = use_context::<Sender>();
    let agents = use_context::<Signal<AgentsState>>();

    // Editable form state, seeded from the on-disk Profile snapshot.
    let mut name = use_signal(String::new);
    let mut operator = use_signal(String::new);
    let mut comm_style = use_signal(String::new);
    let use_cases = use_signal(Vec::<String>::new);
    let prefs = use_signal(Vec::<String>::new);
    let constraints = use_signal(Vec::<String>::new);
    // The snapshot the form was last seeded from — so a write *error* (snapshot
    // unchanged) doesn't wipe the operator's in-progress edits.
    let mut last_seed = use_signal(|| None::<ProfileSummary>);

    // Load the current Profile when the view opens. (The Command-Center one-shot
    // may already have populated it; re-asking is cheap and keeps this panel
    // self-contained.)
    use_future(move || async move {
        ws.send(get_profile_query());
    });

    // Persona governance load + live-refresh. The memo isolates the refresh tick
    // so this effect re-runs ONLY on mount (tick 0) and after a resolve/revert
    // ack bumps it — not on every unrelated AgentsState write (which would loop,
    // since the queries below feed AgentsState).
    let tick = use_memo(move || agents().refresh_tick);
    use_effect(move || {
        let _ = tick();
        ws.send(get_effective_persona_query());
        ws.send(list_proposals_query());
        ws.send(list_deltas_query());
    });

    // Seed the form whenever the snapshot content changes (first load + after a
    // successful write), but not on a notice-only change.
    let mut use_cases_s = use_cases;
    let mut prefs_s = prefs;
    let mut constraints_s = constraints;
    use_effect(move || {
        let snap = agents().profile.clone();
        if snap != last_seed() {
            if let Some(p) = snap.as_ref() {
                // Only seed the name when it is operator-declared; a `default`
                // source means "Aivyx" is the fallback, so leave the field blank
                // (saving blank keeps it at the default rather than re-declaring).
                name.set(if p.assistant_name_source == "toml" {
                    p.assistant_name.clone()
                } else {
                    String::new()
                });
                operator.set(p.operator_profile.clone().unwrap_or_default());
                comm_style.set(p.communication_style.clone().unwrap_or_default());
                use_cases_s.set(p.primary_use_cases.clone());
                prefs_s.set(p.behavioral_preferences.clone());
                constraints_s.set(p.behavioral_constraints.clone());
            }
            last_seed.set(snap);
        }
    });

    let st = agents();
    let profile = match st.profile.clone() {
        Some(p) => p,
        None => {
            return rsx! {
                div { class: "settings agents",
                    div { class: "glass-card empty",
                        p { class: "label-tech", "Loading profile…" }
                    }
                }
            }
        }
    };

    // X.3 — a *fresh* agent (the effective persona is loaded and empty, the
    // delta chain is empty, and nothing is pending) gets the onboarding seed
    // card instead of the (empty) governance view. Once seeded, the refresh
    // re-query flips `is_non_empty` and the governance view takes over.
    let is_fresh = st
        .persona
        .as_ref()
        .map(|p| !p.is_non_empty)
        .unwrap_or(false)
        && st.deltas.is_empty()
        && st.proposals.is_empty();

    rsx! {
        div { class: "settings agents",

            if st.restart_required {
                div { class: "glass-card restart-banner",
                    strong { "Saved — restart the daemon to apply." }
                    p { class: "label-tech",
                        "The Profile shapes every turn's system prompt at startup. Run  "
                        code { "aivyx-pa daemon stop && aivyx-pa daemon run" }
                    }
                }
            }

            if let Some((ok, msg)) = st.notice.clone() {
                div { class: if ok { "notice ok" } else { "notice err" }, "{msg}" }
            }

            // ── Declared identity (Profile scalars) ──
            div { class: "glass-card settings-section",
                div { class: "panel-head",
                    h3 { "Declared identity" }
                    span { class: if profile.injection_enabled { "chip" } else { "chip muted" },
                        {if profile.injection_enabled { "shaping prompts" } else { "passthrough" }}
                    }
                }
                p { class: "label-tech",
                    "What you declare about your assistant and yourself. The agent reads this at startup; leave a field blank to clear it."
                }
                div { class: "field-row",
                    label { class: "label-tech", "Assistant name" }
                    input {
                        class: "input", placeholder: "Aivyx PA (default)",
                        value: "{name}", oninput: move |e| name.set(e.value()),
                    }
                }
                div { class: "field-row",
                    label { class: "label-tech", "About you" }
                    textarea {
                        class: "input", rows: "2",
                        placeholder: "e.g. Indie game developer; prefers concise, technical answers",
                        value: "{operator}", oninput: move |e| operator.set(e.value()),
                    }
                }
                div { class: "field-row",
                    label { class: "label-tech", "Communication style" }
                    textarea {
                        class: "input", rows: "2",
                        placeholder: "e.g. Terse, no preamble, code-first",
                        value: "{comm_style}", oninput: move |e| comm_style.set(e.value()),
                    }
                }
            }

            // ── Declared lists ──
            ListEditor {
                title: "Primary use cases",
                hint: "What you mostly use the agent for.",
                items: use_cases,
            }
            ListEditor {
                title: "Behavioral preferences",
                hint: "How you'd like the agent to behave (soft guidance).",
                items: prefs,
            }
            ListEditor {
                title: "Behavioral constraints",
                hint: "Lines the agent should not cross.",
                items: constraints,
            }

            div { class: "actions sticky-save",
                button {
                    class: "btn btn-primary",
                    onclick: move |_| ws.send(set_profile_query(
                        opt_str(&name()),
                        opt_str(&operator()),
                        opt_str(&comm_style()),
                        opt_list(&use_cases()),
                        opt_list(&prefs()),
                        opt_list(&constraints()),
                    )),
                    "Save profile"
                }
            }

            // ── Self-learned persona (governance — never hand-edited) ──
            div { class: "section-divider label-tech", "Self-learned persona" }
            p { class: "label-tech persona-blurb",
                "The agent proposes these from reflection; you approve or revert. \
                 Changes apply on the next turn — no restart needed."
            }

            if is_fresh {
                SeedOnboardingCard {}
            } else {

            // Pending proposals — the operator's gate.
            div { class: "glass-card settings-section",
                div { class: "panel-head",
                    h3 { "Pending proposals" }
                    span { class: if st.proposals.is_empty() { "chip muted" } else { "chip warning" },
                        "{st.proposals.len()}"
                    }
                }
                if st.proposals.is_empty() {
                    p { class: "label-tech sub", "Nothing awaiting review." }
                } else {
                    div { class: "proposal-list",
                        for p in st.proposals.clone() {
                            ProposalCard { key: "{p.id}", p: p.clone() }
                        }
                    }
                }
            }

            // Effective persona — the folded, read-only view.
            div { class: "glass-card settings-section",
                div { class: "panel-head", h3 { "Effective persona" } span { class: "chip", "read-only" } }
                match st.persona.clone() {
                    Some(p) if p.is_non_empty => rsx! {
                        div { class: "persona-facets",
                            PersonaFacet { label: "Learned context", items: p.learned_context }
                            PersonaFacet { label: "Communication adaptations", items: p.communication_adaptations }
                            PersonaFacet { label: "Character traits", items: p.character_traits }
                            PersonaFacet { label: "Relationship milestones", items: p.relationship_milestones }
                            PersonaFacet { label: "Behavioral preferences", items: p.behavioral_preferences }
                            PersonaFacet { label: "Behavioral constraints", items: p.behavioral_constraints }
                        }
                    },
                    _ => rsx! {
                        p { class: "label-tech sub", "The agent hasn't learned anything yet." }
                    },
                }
            }

            // Change history — the approved delta chain, each revertable.
            div { class: "glass-card settings-section",
                div { class: "panel-head", h3 { "Change history" } span { class: "chip", "{st.deltas.len()}" } }
                if st.deltas.is_empty() {
                    p { class: "label-tech sub", "No approved changes yet." }
                } else {
                    div { class: "delta-list",
                        for d in st.deltas.clone() {
                            DeltaRow { key: "{d.delta_id}", d: d.clone() }
                        }
                    }
                }
            }

            } // end else (governance vs. onboarding seed card)
        }
    }
}

/// Chapter Genesis (GE.3) — the guided agent-creation flow. Three sequenced
/// steps over existing IPC: (1) Profile — the declared identity, optionally
/// LLM-drafted via `DraftProfile`, persisted via `SetProfile`; (2) Persona seed
/// — the learned voice, via the X.3 `SeedOnboardingCard`; (3) Access — how far
/// the agent reaches, via `SetAccessLevel`. The operator authors every field;
/// the LLM only drafts. Targets a running daemon (the cold-start path is
/// `aivyx-pa init`); Profile + access are load-time so a restart applies them.
#[component]
fn OnboardingPanel(view: Signal<View>) -> Element {
    let step = use_signal(|| 0u8);
    let ws = use_context::<Sender>();
    let settings = use_context::<Signal<SettingsState>>();

    // GE.4 — provider/model is CLI/installer-set (it must precede daemon boot),
    // so the flow only *shows* it read-only. Fetch the snapshot on mount.
    use_effect(move || {
        ws.send(get_settings_query());
    });
    let model_line = settings()
        .snapshot
        .map(|s| format!("Connected to {} · {}", s.provider, s.model));

    rsx! {
        div { class: "view-stack",
            div { class: "glass-card settings-section",
                div { class: "panel-head",
                    h3 { "Create your agent" }
                    span { class: "chip success", "step {step() + 1} of 4" }
                }
                p { class: "muted",
                    "Shape your assistant's identity, voice, and reach. You're the author of "
                    "record — the model only drafts. Profile and access apply after a daemon restart."
                }
                if let Some(line) = model_line {
                    p { class: "muted", style: "font-size:12px;",
                        "{line} — set the provider/model with `aivyx-pa init` or in your config."
                    }
                }
                div { class: "step-rail",
                    StepDot { n: 1, label: "Profile", active: step() == 0, done: step() > 0 }
                    StepDot { n: 2, label: "Persona", active: step() == 1, done: step() > 1 }
                    StepDot { n: 3, label: "Team", active: step() == 2, done: step() > 2 }
                    StepDot { n: 4, label: "Access", active: step() == 3, done: false }
                }
            }
            match step() {
                0 => rsx! { OnboardingProfileStep { step } },
                1 => rsx! {
                    div { class: "view-stack",
                        SeedOnboardingCard {}
                        div { class: "wizard-nav",
                            button { class: "btn ghost", onclick: move |_| { let mut s = step; s.set(0); }, "Back" }
                            button { class: "btn", onclick: move |_| { let mut s = step; s.set(2); }, "Continue →" }
                        }
                    }
                },
                2 => rsx! { OnboardingTeamStep { step, view } },
                _ => rsx! { OnboardingAccessStep { step, view } },
            }
        }
    }
}

/// One dot in the onboarding step rail.
#[component]
fn StepDot(n: u8, label: &'static str, active: bool, done: bool) -> Element {
    let cls = if active { "step-dot active" } else if done { "step-dot done" } else { "step-dot" };
    rsx! {
        div { class: "{cls}",
            span { class: "step-num", if done { "✓" } else { "{n}" } }
            span { class: "step-label", "{label}" }
        }
    }
}

/// GE.3 step 1 — the declared Profile. Four onboarding answers feed an optional
/// LLM draft (`DraftProfile`); the six Profile fields below are then editable
/// and saved via `SetProfile`.
#[component]
fn OnboardingProfileStep(step: Signal<u8>) -> Element {
    let ws = use_context::<Sender>();
    let agents = use_context::<Signal<AgentsState>>();

    // The four relationship answers (LLM draft inputs).
    let mut intent = use_signal(String::new);
    let mut role = use_signal(String::new);
    let mut tone = use_signal(String::new);
    let mut never_do = use_signal(String::new);

    // The six declared Profile fields (editable; the draft fills them).
    let mut assistant_name = use_signal(String::new);
    let mut operator_profile = use_signal(String::new);
    let mut communication_style = use_signal(String::new);
    let mut use_cases = use_signal(String::new);
    let mut prefs = use_signal(String::new);
    let mut constraints = use_signal(String::new);

    let mut drafting = use_signal(|| false);
    let mut last_resp = use_signal(|| 0u64);

    // Fill the six fields when a Profile draft arrives (success or failure).
    use_effect(move || {
        let a = agents();
        if a.profile_draft_resp != last_resp() {
            last_resp.set(a.profile_draft_resp);
            drafting.set(false);
            if let Some(d) = a.profile_draft.as_ref() {
                assistant_name.set(d.assistant_name.clone().unwrap_or_default());
                operator_profile.set(d.operator_profile.clone().unwrap_or_default());
                communication_style.set(d.communication_style.clone().unwrap_or_default());
                use_cases.set(d.primary_use_cases.join(", "));
                prefs.set(d.behavioral_preferences.join(", "));
                constraints.set(d.behavioral_constraints.join(", "));
            }
        }
    });

    let st = agents();
    let saved = st.restart_required;

    rsx! {
        div { class: "glass-card settings-section",
            div { class: "panel-head", h3 { "1 · Profile — who your assistant is" } }
            p { class: "muted", "Answer in your own words, then let the model draft a starting Profile — or fill the six fields yourself." }

            label { class: "field-label", "What do you want this assistant to be for you?" }
            textarea { class: "input", rows: "2", value: "{intent}", oninput: move |e| intent.set(e.value()) }
            label { class: "field-label", "What role should it play?" }
            input { class: "input", value: "{role}", placeholder: "collaborator / coach / assistant …", oninput: move |e| role.set(e.value()) }
            label { class: "field-label", "How should it talk?" }
            input { class: "input", value: "{tone}", placeholder: "warm but concise …", oninput: move |e| tone.set(e.value()) }
            label { class: "field-label", "What must it never do?" }
            input { class: "input", value: "{never_do}", placeholder: "never flatter; always confirm destructive actions …", oninput: move |e| never_do.set(e.value()) }

            div { class: "wizard-nav",
                button {
                    class: "btn ghost",
                    disabled: drafting(),
                    onclick: move |_| {
                        drafting.set(true);
                        ws.send(draft_profile_query(intent(), role(), tone(), never_do()));
                    },
                    if drafting() { "Drafting…" } else { "✦ Draft with AI" }
                }
            }

            hr { class: "divider" }
            div { class: "panel-head", h4 { "Your Profile" } }
            label { class: "field-label", "Assistant name" }
            input { class: "input", value: "{assistant_name}", oninput: move |e| assistant_name.set(e.value()) }
            label { class: "field-label", "About you (operator profile)" }
            textarea { class: "input", rows: "2", value: "{operator_profile}", oninput: move |e| operator_profile.set(e.value()) }
            label { class: "field-label", "Communication style" }
            input { class: "input", value: "{communication_style}", oninput: move |e| communication_style.set(e.value()) }
            label { class: "field-label", "Primary use cases (comma-separated)" }
            input { class: "input", value: "{use_cases}", oninput: move |e| use_cases.set(e.value()) }
            label { class: "field-label", "Behavioral preferences (comma-separated)" }
            input { class: "input", value: "{prefs}", oninput: move |e| prefs.set(e.value()) }
            label { class: "field-label", "Behavioral constraints (comma-separated)" }
            input { class: "input", value: "{constraints}", oninput: move |e| constraints.set(e.value()) }

            if let Some((ok, msg)) = st.notice.clone() {
                div { class: if ok { "notice ok" } else { "notice err" }, "{msg}" }
            }
            if saved {
                div { class: "notice ok", "Profile saved — it shapes every turn after the next daemon restart." }
            }

            div { class: "wizard-nav",
                button {
                    class: "btn",
                    onclick: move |_| {
                        ws.send(set_profile_query(
                            opt_str(&assistant_name()),
                            opt_str(&operator_profile()),
                            opt_str(&communication_style()),
                            csv_opt(&use_cases()),
                            csv_opt(&prefs()),
                            csv_opt(&constraints()),
                        ));
                        let mut s = step; s.set(1);
                    },
                    "Save & continue →"
                }
                button { class: "btn ghost", onclick: move |_| { let mut s = step; s.set(1); }, "Skip for now" }
            }
        }
    }
}

/// Chapter Roster (RO.4) — onboarding "Team" step. Shows the active roster (the
/// default Nonagon on a fresh install, fetched via `GetTeamRoster`) and routes
/// to the full Teams editor (RO.3). Keeping the default is a no-op; pack presets
/// are an `aivyx-pa team init` CLI affordance (the team engine isn't wasm, so the
/// browser can't construct a pack — it edits the loaded one). Read-only here.
#[component]
fn OnboardingTeamStep(step: Signal<u8>, view: Signal<View>) -> Element {
    let ws = use_context::<Sender>();
    let teams = use_context::<Signal<TeamsState>>();
    use_effect(move || {
        ws.send(get_team_roster_query());
    });
    let roster = teams().roster;

    rsx! {
        div { class: "glass-card settings-section",
            div { class: "panel-head", h3 { "3 · Team — who works for you" } }
            p { class: "muted",
                "Your assistant leads a team of specialists. Keep the default Nonagon, or shape "
                "the lead, specialists, and their reach in the Teams editor (you can always change it later)."
            }
            {match roster {
                Some(t) => rsx! {
                    div { class: "kv-grid",
                        div { span { class: "label-tech", "Team" } div { "{t.name}" } }
                        div { span { class: "label-tech", "Lead" } div { "{t.lead}" } }
                        div { span { class: "label-tech", "Members" } div { "{t.members.len()}" } }
                    }
                    button { class: "btn btn-glass", onclick: move |_| view.set(View::Teams),
                        "Customize team →" }
                },
                None => rsx! { p { class: "label-tech", "Loading team…" } },
            }}
            div { class: "wizard-nav",
                button { class: "btn ghost", onclick: move |_| { let mut s = step; s.set(1); }, "Back" }
                button { class: "btn", onclick: move |_| { let mut s = step; s.set(3); }, "Continue →" }
            }
        }
    }
}

/// GE.3 step 3 — access level, via the existing `SetAccessLevel` (confirm-first
/// on any expansion beyond the sandbox).
#[component]
fn OnboardingAccessStep(step: Signal<u8>, view: Signal<View>) -> Element {
    let ws = use_context::<Sender>();
    let agents = use_context::<Signal<AgentsState>>();
    let mut level = use_signal(|| "sandbox".to_string());
    let st = agents();

    rsx! {
        div { class: "glass-card settings-section",
            div { class: "panel-head", h3 { "4 · Access — how far it reaches" } }
            p { class: "muted", "Start narrow; you can widen later in Settings. Expanding beyond the sandbox is confirmed first." }
            select {
                class: "input",
                value: "{level}",
                onchange: move |e| level.set(e.value()),
                option { value: "sandbox", "sandbox — ~/aivyx-pa-sandbox" }
                option { value: "home", "home — your home directory" }
                option { value: "full", "full — the whole machine" }
            }
            if let Some((ok, msg)) = st.notice.clone() {
                div { class: if ok { "notice ok" } else { "notice err" }, "{msg}" }
            }
            div { class: "wizard-nav",
                button { class: "btn ghost", onclick: move |_| { let mut s = step; s.set(2); }, "Back" }
                button {
                    class: "btn",
                    onclick: move |_| {
                        // Confirm-first: the daemon gates any expansion beyond sandbox.
                        ws.send(set_access_query(level(), None, true));
                        view.set(View::Command);
                    },
                    "Finish — go to Command Center"
                }
            }
        }
    }
}

/// X.3 — the "Seed your assistant" onboarding card, shown for a fresh agent.
/// Describe the assistant → optionally let the model draft a starting set →
/// edit → plant. The seed goes onto the signed chain via `SeedPersona` (the
/// same primitive the boot-seed uses); the operator is always the author.
#[component]
fn SeedOnboardingCard() -> Element {
    let ws = use_context::<Sender>();
    let agents = use_context::<Signal<AgentsState>>();

    let mut description = use_signal(String::new);
    let traits = use_signal(Vec::<String>::new);
    let adaptations = use_signal(Vec::<String>::new);
    let mut context = use_signal(String::new);
    let mut skill_name = use_signal(String::new);
    let mut skill_trigger = use_signal(String::new);
    let mut skill_procedure = use_signal(String::new);
    let mut drafting = use_signal(|| false);
    let mut last_resp = use_signal(|| 0u64);

    // When a draft response arrives (success or failure), clear the spinner and
    // — on success — fill the form from the drafted seed. The operator edits
    // from there.
    let mut traits_s = traits;
    let mut adaptations_s = adaptations;
    use_effect(move || {
        let a = agents();
        if a.seed_draft_resp != last_resp() {
            last_resp.set(a.seed_draft_resp);
            drafting.set(false);
            if let Some(d) = a.seed_draft.as_ref() {
                traits_s.set(d.character_traits.clone());
                adaptations_s.set(d.communication_adaptations.clone());
                context.set(d.learned_context.first().cloned().unwrap_or_default());
                if let Some(s) = d.skills.first() {
                    skill_name.set(s.name.clone());
                    skill_trigger.set(s.trigger.clone());
                    skill_procedure.set(s.procedure.clone());
                }
            }
        }
    });

    let st = agents();

    rsx! {
        div { class: "glass-card settings-section seed-card",
            div { class: "panel-head",
                h3 { "Seed your assistant" }
                span { class: "chip success", "fresh" }
            }
            p { class: "label-tech",
                "This agent hasn't learned a personality yet. Give it a head start — \
                 it keeps growing from use. Describe it, optionally let the model draft \
                 a set, edit, then plant."
            }

            if let Some((ok, msg)) = st.notice.clone() {
                div { class: if ok { "notice ok" } else { "notice err" }, "{msg}" }
            }

            div { class: "field-row",
                label { class: "label-tech", "Describe it" }
                textarea {
                    class: "input", rows: "2",
                    placeholder: "e.g. a witty, terse pair-programmer who cites sources",
                    value: "{description}", oninput: move |e| description.set(e.value()),
                }
            }
            div { class: "actions",
                button {
                    class: "btn btn-glass",
                    disabled: drafting(),
                    onclick: move |_| { drafting.set(true); ws.send(draft_seed_query(description())); },
                    {if drafting() { "Drafting…" } else { "Draft with AI" }}
                }
            }

            ListEditor { title: "Character traits", hint: "Voice properties to start with.", items: traits }
            ListEditor {
                title: "Communication adaptations",
                hint: "Refinements to how it talks (optional).",
                items: adaptations,
            }
            div { class: "field-row",
                label { class: "label-tech", "Day-one context" }
                textarea {
                    class: "input", rows: "2",
                    placeholder: "Anything it should know about you / your work (optional)",
                    value: "{context}", oninput: move |e| context.set(e.value()),
                }
            }

            div { class: "panel-head", h4 { "Starter skill (optional)" } }
            div { class: "field-row",
                label { class: "label-tech", "Name" }
                input { class: "input", placeholder: "rust-review",
                    value: "{skill_name}", oninput: move |e| skill_name.set(e.value()) }
            }
            div { class: "field-row",
                label { class: "label-tech", "When" }
                input { class: "input", placeholder: "when reviewing Rust",
                    value: "{skill_trigger}", oninput: move |e| skill_trigger.set(e.value()) }
            }
            div { class: "field-row",
                label { class: "label-tech", "Does what" }
                input { class: "input", placeholder: "check unwraps; cite file:line",
                    value: "{skill_procedure}", oninput: move |e| skill_procedure.set(e.value()) }
            }

            div { class: "actions sticky-save",
                button {
                    class: "btn btn-primary",
                    onclick: move |_| ws.send(seed_persona_query(build_seed_wire(
                        &traits(), &adaptations(), &context(),
                        &skill_name(), &skill_trigger(), &skill_procedure(),
                    ))),
                    "Plant seed"
                }
            }
        }
    }
}

/// One folded persona facet — a labeled list, rendered only when non-empty.
#[component]
fn PersonaFacet(label: String, items: Vec<String>) -> Element {
    if items.is_empty() {
        return rsx! {};
    }
    rsx! {
        div { class: "facet",
            span { class: "label-tech", "{label}" }
            ul { class: "facet-list",
                for item in items {
                    li { "{item}" }
                }
            }
        }
    }
}

/// The three review modes a proposal card can be in.
#[derive(Clone, Copy, PartialEq)]
enum ProposalMode {
    View,
    Editing,
    Rejecting,
}

/// One pending persona proposal with the operator's gate actions: approve,
/// approve-with-edit (reword the proposed value), or reject with a reason.
/// Chapter V — the self-learning human gate, in the Studio.
#[component]
fn ProposalCard(p: PersonaProposalSummary) -> Element {
    let ws = use_context::<Sender>();
    let mut mode = use_signal(|| ProposalMode::View);
    let mut draft = use_signal(String::new);

    let op_desc = render_op(&p.category, &p.proposed_op);
    let editable = op_value(&p.proposed_op);
    let pid = p.id.clone();
    let category = p.category.clone();
    let op = p.proposed_op.clone();

    rsx! {
        div { class: "glass-card proposal-card",
            div { class: "panel-head",
                h4 { "{p.category}" }
                span { class: "chip warning", "pending" }
            }
            p { class: "op-desc", "{op_desc}" }
            if let Some(reason) = p.proposed_reason.clone() {
                p { class: "label-tech reason", "“{reason}”" }
            }

            match mode() {
                ProposalMode::View => rsx! {
                    div { class: "actions",
                        {
                            let pid_a = pid.clone();
                            rsx! {
                                button {
                                    class: "btn btn-primary",
                                    onclick: move |_| ws.send(resolve_proposal_query(
                                        &pid_a, PersonaProposalResolution::Approve,
                                    )),
                                    "Approve"
                                }
                            }
                        }
                        if let Some(v) = editable.clone() {
                            button {
                                class: "btn btn-glass",
                                onclick: move |_| { draft.set(v.clone()); mode.set(ProposalMode::Editing); },
                                "Edit & approve"
                            }
                        }
                        button {
                            class: "btn btn-glass danger",
                            onclick: move |_| { draft.set(String::new()); mode.set(ProposalMode::Rejecting); },
                            "Reject"
                        }
                    }
                },
                ProposalMode::Editing => rsx! {
                    div { class: "field-row",
                        label { class: "label-tech", "Edited value" }
                        input { class: "input", value: "{draft}", oninput: move |e| draft.set(e.value()) }
                    }
                    div { class: "actions",
                        button { class: "btn btn-glass", onclick: move |_| mode.set(ProposalMode::View), "Cancel" }
                        {
                            let (pid_e, cat_e, op_e) = (pid.clone(), category.clone(), op.clone());
                            rsx! {
                                button {
                                    class: "btn btn-primary",
                                    onclick: move |_| {
                                        if let Some(res) = approve_with_edited_value(&cat_e, &op_e, &draft()) {
                                            ws.send(resolve_proposal_query(&pid_e, res));
                                            mode.set(ProposalMode::View);
                                        }
                                    },
                                    "Approve edit"
                                }
                            }
                        }
                    }
                },
                ProposalMode::Rejecting => rsx! {
                    div { class: "field-row",
                        label { class: "label-tech", "Reason (optional)" }
                        input {
                            class: "input", placeholder: "why you're rejecting…",
                            value: "{draft}", oninput: move |e| draft.set(e.value()),
                        }
                    }
                    div { class: "actions",
                        button { class: "btn btn-glass", onclick: move |_| mode.set(ProposalMode::View), "Cancel" }
                        {
                            let pid_r = pid.clone();
                            rsx! {
                                button {
                                    class: "btn btn-primary",
                                    onclick: move |_| {
                                        ws.send(resolve_proposal_query(
                                            &pid_r,
                                            PersonaProposalResolution::Reject { reason: opt_str(&draft()) },
                                        ));
                                        mode.set(ProposalMode::View);
                                    },
                                    "Confirm reject"
                                }
                            }
                        }
                    }
                },
            }
        }
    }
}

/// One approved persona delta with a two-click inline revert (reverting appends
/// an inverse delta — the chain stays append-only). Chapter V.
#[component]
fn DeltaRow(d: PersonaDeltaSummary) -> Element {
    let ws = use_context::<Sender>();
    let mut confirming = use_signal(|| false);
    let desc = render_op(&d.category, &d.op);
    let did = d.delta_id.clone();

    rsx! {
        div { class: "delta-row",
            div { class: "delta-main",
                span { class: "chip", "#{d.seq}" }
                // Mark deltas planted by the onboarding seed (W.2 sentinel) so
                // they're visibly distinct from the agent's learned deltas.
                if d.proposal_id == "genesis-seed" {
                    span { class: "chip success", title: "Planted at first launch from [persona_seed]", "seed" }
                }
                span { class: "delta-cat label-tech", "{d.category}" }
                span { class: "op-desc", "{desc}" }
            }
            if confirming() {
                div { class: "actions",
                    button { class: "btn btn-glass", onclick: move |_| confirming.set(false), "Cancel" }
                    button {
                        class: "btn btn-primary",
                        onclick: move |_| { ws.send(revert_delta_query(&did)); confirming.set(false); },
                        "Confirm revert"
                    }
                }
            } else {
                button { class: "btn btn-glass", onclick: move |_| confirming.set(true), "Revert" }
            }
        }
    }
}

/// A small add/remove list editor over a shared `Signal<Vec<String>>`. Owns its
/// own draft-entry input; mutations flow straight back to the parent's signal
/// so the Save handler reads the live list. Chapter V.
#[component]
fn ListEditor(title: String, hint: String, items: Signal<Vec<String>>) -> Element {
    let mut items = items;
    let mut draft = use_signal(String::new);
    let add = move |_: MouseEvent| {
        let v = draft().trim().to_string();
        if !v.is_empty() {
            items.write().push(v);
            draft.set(String::new());
        }
    };
    rsx! {
        div { class: "glass-card settings-section",
            div { class: "panel-head",
                h3 { "{title}" }
                span { class: "chip", "{items().len()}" }
            }
            p { class: "label-tech", "{hint}" }
            if items().is_empty() {
                p { class: "label-tech sub", "None declared." }
            } else {
                div { class: "list-editor",
                    for (i, entry) in items().into_iter().enumerate() {
                        div { class: "chip-removable", key: "{i}",
                            span { "{entry}" }
                            button {
                                class: "chip-x",
                                title: "Remove",
                                onclick: move |_| { items.write().remove(i); },
                                "×"
                            }
                        }
                    }
                }
            }
            div { class: "add-row",
                input {
                    class: "input",
                    placeholder: "Add an entry…",
                    value: "{draft}",
                    oninput: move |e| draft.set(e.value()),
                }
                button { class: "btn btn-glass", onclick: add, "Add" }
            }
        }
    }
}

fn get_profile_query() -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-agents-get".to_string(),
        // The editor seeds from the **on-disk** profile (what it writes), so a
        // save-before-restart followed by a reload shows the pending values —
        // not the stale running snapshot — and never clobbers a pending edit.
        payload: QueryPayload::GetProfile { from_disk: true },
    }
}

#[allow(clippy::too_many_arguments)]
fn set_profile_query(
    assistant_name: Option<String>,
    operator_profile: Option<String>,
    communication_style: Option<String>,
    primary_use_cases: Option<Vec<String>>,
    behavioral_preferences: Option<Vec<String>>,
    behavioral_constraints: Option<Vec<String>>,
) -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-agents-profile".to_string(),
        payload: QueryPayload::SetProfile {
            assistant_name,
            operator_profile,
            communication_style,
            primary_use_cases,
            behavioral_preferences,
            behavioral_constraints,
        },
    }
}

/// A scalar form field → `Some(trimmed)` or `None` when blank (clear the key).
fn opt_str(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

/// GE.3 — a comma-separated list field → `Some(cleaned)` of trimmed non-empty
/// entries, or `None` when the field is blank (clear the key).
fn csv_opt(s: &str) -> Option<Vec<String>> {
    let cleaned: Vec<String> = s
        .split(',')
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
        .collect();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

/// A list field → `Some(cleaned)` of trimmed non-empty entries, or `None` when
/// empty (clear the key — "no declared entries", distinct from `[]`).
fn opt_list(v: &[String]) -> Option<Vec<String>> {
    let cleaned: Vec<String> = v
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

// ── Persona governance queries (Chapter V.4) — all over existing IPC. ──

fn get_effective_persona_query() -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-agents-persona".to_string(),
        payload: QueryPayload::GetEffectivePersona,
    }
}

fn list_proposals_query() -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-agents-proposals".to_string(),
        payload: QueryPayload::ListPersonaProposals { status_filter: "pending".to_string(), limit: 50 },
    }
}

fn list_deltas_query() -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-agents-deltas".to_string(),
        payload: QueryPayload::ListPersonaDeltas { from_seq: 0, limit: 50 },
    }
}

fn resolve_proposal_query(
    proposal_id: &str,
    resolution: PersonaProposalResolution,
) -> FrontendMessage {
    FrontendMessage::ResolvePersonaProposal {
        id: format!("mc-agents-resolve-{proposal_id}"),
        proposal_id: proposal_id.to_string(),
        resolution,
    }
}

fn revert_delta_query(target_delta_id: &str) -> FrontendMessage {
    FrontendMessage::RevertPersonaDelta {
        id: format!("mc-agents-revert-{target_delta_id}"),
        target_delta_id: target_delta_id.to_string(),
    }
}

// ── GE.3 — Genesis onboarding: LLM-drafted Profile (step 1). ──

fn draft_profile_query(
    intent: String,
    role: String,
    tone: String,
    never_do: String,
) -> FrontendMessage {
    FrontendMessage::DraftProfile {
        id: "mc-onboard-profile-draft".to_string(),
        intent,
        role,
        tone,
        never_do,
    }
}

// ── X.3 — persona seed onboarding (web authoring + LLM draft). ──

fn draft_seed_query(description: String) -> FrontendMessage {
    FrontendMessage::DraftPersonaSeed {
        id: "mc-agents-draft".to_string(),
        description,
    }
}

fn seed_persona_query(seed: PersonaSeedWire) -> FrontendMessage {
    FrontendMessage::SeedPersona {
        id: "mc-agents-seed".to_string(),
        seed,
    }
}

/// Build a `PersonaSeedWire` from the onboarding form. Lists are trimmed +
/// de-blanked; the day-one context becomes a single `learned_context` entry; a
/// skill is included only when it has a name. (The daemon adds the genesis
/// milestone and refuses an empty seed.)
fn build_seed_wire(
    traits: &[String],
    adaptations: &[String],
    context: &str,
    skill_name: &str,
    skill_trigger: &str,
    skill_procedure: &str,
) -> PersonaSeedWire {
    let clean = |v: &[String]| -> Vec<String> {
        v.iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    };
    let learned_context = match context.trim() {
        "" => Vec::new(),
        c => vec![c.to_string()],
    };
    let skills = if skill_name.trim().is_empty() {
        Vec::new()
    } else {
        vec![SeedSkillWire {
            name: skill_name.trim().to_string(),
            trigger: skill_trigger.trim().to_string(),
            procedure: skill_procedure.trim().to_string(),
        }]
    };
    PersonaSeedWire {
        learned_context,
        communication_adaptations: clean(adaptations),
        character_traits: clean(traits),
        relationship_milestones: Vec::new(),
        skills,
    }
}

/// Render a persona delta `op` JSON (`{kind, value}`) into a human sentence for
/// the proposal/delta cards. Mirrors `PersonaDeltaOp`'s serde repr.
fn render_op(category: &str, op: &serde_json::Value) -> String {
    let kind = op.get("kind").and_then(|k| k.as_str()).unwrap_or("?");
    let value = op
        .get("value")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    match kind {
        "SetScalar" => format!("set {category} → “{value}”"),
        "AppendList" => format!("add to {category}: “{value}”"),
        "RemoveList" => format!("remove from {category}: “{value}”"),
        "Revert" => format!("revert a prior {category} change"),
        other => format!("{category}: {other}"),
    }
}

/// The single editable string value of an op, for `SetScalar`/`AppendList`/
/// `RemoveList` — the kinds the operator can reword on approve. `None` for ops
/// with no editable string (so the "Edit & approve" affordance is hidden).
fn op_value(op: &serde_json::Value) -> Option<String> {
    match op.get("kind").and_then(|k| k.as_str()) {
        Some("SetScalar") | Some("AppendList") | Some("RemoveList") => op
            .get("value")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        _ => None,
    }
}

/// Build an `ApproveWithEdit` resolution from a proposal's category + op with
/// the operator's reworded value. Reconstructs a typed `ProposedPersonaDelta`
/// by deserialization (the summary's category label equals the serde repr; the
/// op JSON is `PersonaDeltaOp`'s own serde shape), so the daemon re-validates
/// and re-signs the edited op exactly as it would a fresh proposal. Returns
/// `None` if the edited op fails to reconstruct (then the caller does nothing).
fn approve_with_edited_value(
    category: &str,
    op: &serde_json::Value,
    new_value: &str,
) -> Option<PersonaProposalResolution> {
    let mut edited_op = op.clone();
    if let Some(obj) = edited_op.as_object_mut() {
        obj.insert("value".to_string(), serde_json::Value::String(new_value.to_string()));
    }
    let pd_json = serde_json::json!({ "category": category, "op": edited_op });
    serde_json::from_value::<ProposedPersonaDelta>(pd_json)
        .ok()
        .map(|edited_op| PersonaProposalResolution::ApproveWithEdit { edited_op })
}

// ---------------------------------------------------------------------------
// Wire helpers + the WebSocket task (unchanged from Chapter M)
// ---------------------------------------------------------------------------

fn start_query(goal: String) -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-start".to_string(),
        payload: QueryPayload::TeamRunGoal { goal, config: None },
    }
}

fn resolve_team_query(mission_id: String, step: String, approve: bool) -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-gate".to_string(),
        payload: QueryPayload::ResolveTeamGate { mission_id, step, approve },
    }
}

fn abort_team_mission_query(mission_id: String) -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-abort".to_string(),
        payload: QueryPayload::AbortTeamMission { mission_id },
    }
}

fn pause_team_mission_query(mission_id: String) -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-pause".to_string(),
        payload: QueryPayload::PauseTeamMission { mission_id },
    }
}

fn resume_team_mission_query(mission_id: String) -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-resume".to_string(),
        payload: QueryPayload::ResumeTeamMission { mission_id },
    }
}

fn resolve_gate_query(mission_id: String, gate_id: String, approved: bool) -> FrontendMessage {
    FrontendMessage::ResolveGate { mission_id, gate_id, approved }
}

fn submit_query(session_id: String, text: String) -> FrontendMessage {
    FrontendMessage::SubmitInput {
        session_id,
        text,
        mission_id: None,
        attachments: Vec::new(),
        headless: false,
    }
}

fn phase_label(p: TeamMissionPhase) -> &'static str {
    match p {
        TeamMissionPhase::Planning => "planning",
        TeamMissionPhase::Executing => "executing",
        TeamMissionPhase::AwaitingApproval => "awaiting approval",
        TeamMissionPhase::Paused => "paused",
        TeamMissionPhase::Done => "done",
        TeamMissionPhase::Rejected => "rejected",
        TeamMissionPhase::Halted => "halted",
    }
}

fn phase_class(p: TeamMissionPhase) -> &'static str {
    match p {
        TeamMissionPhase::AwaitingApproval => "warning",
        TeamMissionPhase::Paused => "warning",
        TeamMissionPhase::Done => "success",
        TeamMissionPhase::Rejected => "error",
        TeamMissionPhase::Halted => "error",
        _ => "",
    }
}

/// Chapter Mission Control — apply a live `TeamMissionUpdated` broadcast to
/// the current missions list: replace the entry with a matching id, or
/// append it if this is a mission the client hasn't seen yet (e.g. it was
/// created after the last poll). Pure and Signal-free so it's unit-testable
/// without a Dioxus runtime.
fn upsert_mission_view(missions: &mut Vec<TeamMissionView>, updated: TeamMissionView) {
    if let Some(existing) = missions.iter_mut().find(|m| m.id == updated.id) {
        *existing = updated;
    } else {
        missions.push(updated);
    }
}

/// Chapter Mission Control — which missions this view's selector offers:
/// genuinely worth watching or acting on right now. Narrower than the
/// Command Center's own "Active" stat (which also counts `Halted`, since
/// that's a general dashboard metric) — a `Halted` mission has nothing
/// left to watch or resume, so it's excluded here.
fn watchable_missions(missions: &[TeamMissionView]) -> Vec<&TeamMissionView> {
    missions
        .iter()
        .filter(|m| {
            matches!(
                m.phase,
                TeamMissionPhase::Executing
                    | TeamMissionPhase::Paused
                    | TeamMissionPhase::AwaitingApproval
            )
        })
        .collect()
}

/// Chapter Mission Control — per-mission set of step INDICES (position in
/// `TeamMissionView.steps`) the daemon's last broadcast said are running
/// right now. Fed by `DaemonEnvelope::TeamMissionUpdated`; re-applied by
/// the poll (`QueryResponsePayload::TeamMissionList`) after each refresh,
/// since the poll re-projects raw records client-side via `to_view()`,
/// which never yields `Running` on its own — without this overlay, the
/// poll would silently erase every live signal within one poll interval.
/// Index-keyed rather than step-id-keyed: a mission's step order is
/// stable for the mission's whole lifetime (the `plan` never changes
/// after creation), so position is already a safe, sufficient key for
/// aligning two same-shaped step lists (last poll vs. this poll) --
/// `TeamStepView` does also carry a real `step_id` now (Task 1), but this
/// overlay has no need to look anything up by it.
fn running_step_indices(view: &TeamMissionView) -> HashSet<usize> {
    view.steps
        .iter()
        .enumerate()
        .filter(|(_, s)| s.state == TeamStepState::Running)
        .map(|(i, _)| i)
        .collect()
}

/// Chapter Mission Control — re-apply `overlay`'s remembered running
/// indices onto `views` (a freshly poll-projected list), and prune the
/// overlay in the same pass: an index only stays overlaid (and only stays
/// in `overlay`) if the fresh poll's own checkpoint-derived state for that
/// step is still `Pending` — once a step's real output lands (`Done`/
/// `Rejected`/`Awaiting`), that is strictly more authoritative than a
/// possibly-stale remembered "was running" marker (e.g. if a broadcast was
/// missed on a lagged/reconnecting WebSocket), so the overlay self-heals
/// and never leaks a stale entry forever. Returns nothing; mutates both
/// arguments in place.
fn apply_running_overlay(views: &mut [TeamMissionView], overlay: &mut HashMap<String, HashSet<usize>>) {
    for view in views.iter_mut() {
        let Some(indices) = overlay.get_mut(&view.id) else { continue };
        indices.retain(|&i| {
            let Some(step) = view.steps.get_mut(i) else { return false };
            if step.state == TeamStepState::Pending {
                step.state = TeamStepState::Running;
                true
            } else {
                false
            }
        });
        if indices.is_empty() {
            overlay.remove(&view.id);
        }
    }
}

/// Chapter Mission Control — one node in a mission's live graph: the LEAD
/// or a specialist, with the "worst" (most attention-worthy) state across
/// every step of theirs in this mission.
#[derive(Debug, Clone, PartialEq)]
struct MissionGraphNode {
    name: String,
    is_lead: bool,
    state: TeamStepState,
    /// The step id this node is currently `Running`, if any -- for the
    /// drill-in panel (Task 5) to show "doing: <step>".
    current_step: Option<String>,
    /// False if `name` is not a member of the roster this graph was built
    /// against (`TeamsState.roster` -- the daemon's *current default*
    /// team, which a pack-pinned mission's own config can differ from;
    /// `TeamMissionView` doesn't expose the mission's own pinned config).
    /// A step-touching member always gets a node either way -- this only
    /// controls whether the UI flags it as off the visible default roster.
    on_roster: bool,
}

/// Chapter Mission Control — one dependency edge between two steps
/// (`TeamStepView::deps`, Task 1), carrying the owning member of each end
/// so the graph can draw a node-to-node line without re-deriving step
/// ownership at render time.
#[derive(Debug, Clone, PartialEq)]
struct MissionGraphEdge {
    from_step: String,
    to_step: String,
    from_member: String,
    to_member: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
struct MissionGraph {
    nodes: Vec<MissionGraphNode>,
    edges: Vec<MissionGraphEdge>,
}

/// Chapter Mission Control — the pure transform the rendering layer
/// consumes: project one mission's live steps onto the team roster's real
/// member list, so every roster member gets a node (including one with no
/// step run yet -- genuinely idle, not merely absent from the DAG), PLUS a
/// node for any step-touching member absent from that roster (a
/// pack-pinned mission's own team can differ from the current default —
/// see `MissionGraphNode::on_roster`'s doc comment). Edges come from each
/// step's real `deps` (Task 1), not from parsing `label`. `is_lead` is
/// read from `mission.lead` (the mission's own real lead), never
/// `roster.lead` (which may not even be this mission's lead if the
/// rosters differ).
fn build_mission_graph(mission: &TeamMissionView, roster: &TeamConfig) -> MissionGraph {
    let mut nodes: Vec<MissionGraphNode> =
        roster.members.iter().map(|member| mission_graph_node(mission, &member.name, true)).collect();

    let mut known: HashSet<&str> = roster.members.iter().map(|m| m.name.as_str()).collect();
    let mut unrostered: Vec<&str> =
        mission.steps.iter().map(|s| s.member.as_str()).filter(|name| !known.contains(name)).collect();
    unrostered.sort_unstable();
    unrostered.dedup();
    for name in unrostered {
        nodes.push(mission_graph_node(mission, name, false));
        known.insert(name);
    }
    // The mission's own LEAD must always get a node -- even one who
    // isn't on the current default roster AND touches no step of their
    // own (e.g. a pack-pinned lead that only ever delegates/reviews).
    // Without this, `layout_mission_nodes` has no LEAD to center the
    // ring on and silently promotes an arbitrary specialist to the hub.
    if !known.contains(mission.lead.as_str()) {
        nodes.push(mission_graph_node(mission, &mission.lead, false));
    }

    let step_member: HashMap<&str, &str> =
        mission.steps.iter().map(|s| (s.step_id.as_str(), s.member.as_str())).collect();
    let mut edges = Vec::new();
    for step in mission.steps.iter() {
        for dep in step.deps.iter() {
            if let Some(&from_member) = step_member.get(dep.as_str()) {
                edges.push(MissionGraphEdge {
                    from_step: dep.clone(),
                    to_step: step.step_id.clone(),
                    from_member: from_member.to_string(),
                    to_member: step.member.clone(),
                });
            }
        }
    }
    MissionGraph { nodes, edges }
}

/// One `MissionGraphNode` for `name`, from `name`'s own steps in
/// `mission` (empty if `name` has none — genuinely idle).
fn mission_graph_node(mission: &TeamMissionView, name: &str, on_roster: bool) -> MissionGraphNode {
    let member_steps: Vec<&TeamStepView> = mission.steps.iter().filter(|s| s.member == name).collect();
    let state = step_state_priority(&member_steps);
    let current_step =
        member_steps.iter().find(|s| s.state == TeamStepState::Running).map(|s| s.step_id.clone());
    MissionGraphNode { name: name.to_string(), is_lead: name == mission.lead, state, current_step, on_roster }
}

/// Chapter Mission Control — the single most attention-worthy state across
/// a specialist's own steps in this mission, in priority order: `Running`
/// (something's happening right now), then `Awaiting` (blocked on a
/// decision), then `Pending` (still work to do, even if some of their
/// steps are `Done`), then `Rejected`, then `Done` (only if every one of
/// their steps is `Done`), and `Pending` again for genuinely idle (no
/// steps at all). Priority order chosen so a specialist with mixed
/// Done/Pending steps never reads as "finished."
fn step_state_priority(steps: &[&TeamStepView]) -> TeamStepState {
    if steps.is_empty() {
        return TeamStepState::Pending;
    }
    if steps.iter().any(|s| s.state == TeamStepState::Running) {
        return TeamStepState::Running;
    }
    if steps.iter().any(|s| s.state == TeamStepState::Awaiting) {
        return TeamStepState::Awaiting;
    }
    if steps.iter().any(|s| s.state == TeamStepState::Pending) {
        return TeamStepState::Pending;
    }
    if steps.iter().any(|s| s.state == TeamStepState::Rejected) {
        return TeamStepState::Rejected;
    }
    TeamStepState::Done
}

/// Chapter Mission Control — the graph canvas size (SVG viewBox units).
/// Deliberately smaller than the Lattice/Memory graphs' 760x460 canvas --
/// a mission's roster is LEAD + a handful of specialists, not an
/// open-ended knowledge graph.
const MC_GRAPH_W: f64 = 520.0;
const MC_GRAPH_H: f64 = 360.0;
const MC_NODE_R: f64 = 26.0;

/// Chapter Mission Control — deterministic LEAD-centric layout: the LEAD
/// sits at the canvas center, every other node is placed evenly around a
/// fixed-radius ring centered on the LEAD (first specialist at 12
/// o'clock, clockwise). No force simulation, unlike `compute_layout`
/// (Chapter MG) -- a mission's roster is small and inherently star-shaped
/// around its LEAD. Returns one `(x, y)` per node, index-aligned with
/// `nodes` (mirrors `compute_layout`'s own return convention). Panics
/// never: an empty slice returns an empty vec. If no node has `is_lead`
/// set (should not happen — `build_mission_graph` always gives the
/// mission's own lead a node), `nodes[0]` is centered instead of a real
/// LEAD; this is a defensive fallback, not the intended path.
fn layout_mission_nodes(nodes: &[MissionGraphNode]) -> Vec<(f64, f64)> {
    if nodes.is_empty() {
        return Vec::new();
    }
    let center = (MC_GRAPH_W / 2.0, MC_GRAPH_H / 2.0);
    let lead_idx = nodes.iter().position(|n| n.is_lead).unwrap_or(0);
    let others: Vec<usize> = (0..nodes.len()).filter(|&i| i != lead_idx).collect();
    let radius = (MC_GRAPH_W.min(MC_GRAPH_H) / 2.0 - MC_NODE_R - 24.0).max(40.0);
    let mut pos = vec![(0.0_f64, 0.0_f64); nodes.len()];
    pos[lead_idx] = center;
    let n = others.len().max(1) as f64;
    for (k, &i) in others.iter().enumerate() {
        let ang = -std::f64::consts::FRAC_PI_2 + (k as f64) * (2.0 * std::f64::consts::PI) / n;
        pos[i] = (center.0 + radius * ang.cos(), center.1 + radius * ang.sin());
    }
    pos
}

#[cfg(test)]
mod mission_control_tests {
    use super::*;
    use aivyx_ipc::TeamStepView;

    fn view(id: &str, progress: u16) -> TeamMissionView {
        TeamMissionView {
            id: id.to_string(),
            goal: "goal".into(),
            lead: "coordinator".into(),
            phase: TeamMissionPhase::Executing,
            pending_gate: None,
            halt_reason: None,
            verify_attempts: 0,
            progress,
            steps: vec![],
        }
    }

    /// Verified real `TeamMember` shape (`crates/aivyx-team-types/src/config.rs`)
    /// has 8 fields, not just `name`/`capability_scopes` — this literal
    /// matches the exact construction pattern already used elsewhere in
    /// this same file (`TeamsPanel`'s own "add specialist" button, which
    /// pushes a `TeamMember { .. }` literal with all 8 fields).
    fn sample_member(name: &str) -> TeamMember {
        TeamMember {
            name: name.to_string(),
            role: "Specialist".to_string(),
            soul: String::new(),
            tool_allowlist: vec!["team.message".to_string()],
            capability_scopes: vec![],
            trust_ceiling: TrustTier::SemiTrusted,
            model: None,
            base_url: None,
        }
    }

    /// `TeamConfig` also carries `name`/`description`/`dialogue`, not just
    /// `lead`/`members` (verified against the real struct in
    /// `crates/aivyx-team-types/src/config.rs`, which has no `Default` impl
    /// of its own) -- `dialogue` uses `Default::default()` since
    /// `DialogueConfig` itself does derive-free-`impl Default` but isn't
    /// re-exported through `aivyx_ipc`, so it's inferred from the field's
    /// type rather than named directly.
    fn sample_roster() -> TeamConfig {
        TeamConfig {
            name: "test-team".to_string(),
            description: String::new(),
            lead: "coordinator".to_string(),
            members: vec![
                sample_member("coordinator"),
                sample_member("inventory"),
                sample_member("purchasing"),
            ],
            dialogue: Default::default(),
        }
    }

    #[test]
    fn upsert_replaces_an_existing_mission_by_id() {
        let mut missions = vec![view("m1", 10), view("m2", 50)];
        upsert_mission_view(&mut missions, view("m1", 30));
        assert_eq!(missions.len(), 2, "no duplicate inserted");
        assert_eq!(missions[0].progress, 30);
        assert_eq!(missions[1].progress, 50, "m2 untouched");
    }

    #[test]
    fn upsert_appends_an_unseen_mission() {
        let mut missions = vec![view("m1", 10)];
        upsert_mission_view(&mut missions, view("m2", 0));
        assert_eq!(missions.len(), 2);
        assert_eq!(missions[1].id, "m2");
    }

    #[test]
    fn running_step_indices_finds_the_running_positions() {
        let mut v = view("m1", 10);
        v.steps = vec![
            TeamStepView {
                label: "a".into(),
                state: TeamStepState::Done,
                step_id: "a".into(),
                member: "m".into(),
                kind: "delegate".into(),
                deps: vec![],
            },
            TeamStepView {
                label: "b".into(),
                state: TeamStepState::Running,
                step_id: "b".into(),
                member: "m".into(),
                kind: "delegate".into(),
                deps: vec!["a".into()],
            },
            TeamStepView {
                label: "c".into(),
                state: TeamStepState::Pending,
                step_id: "c".into(),
                member: "m".into(),
                kind: "delegate".into(),
                deps: vec!["b".into()],
            },
        ];
        let indices = running_step_indices(&v);
        assert_eq!(indices, [1].into_iter().collect());
    }

    #[test]
    fn apply_running_overlay_marks_a_still_pending_step_running() {
        let mut v = view("m1", 0);
        v.steps = vec![TeamStepView {
            label: "a".into(),
            state: TeamStepState::Pending,
            step_id: "a".into(),
            member: "m".into(),
            kind: "delegate".into(),
            deps: vec![],
        }];
        let mut views = vec![v];
        let mut overlay: HashMap<String, HashSet<usize>> =
            [("m1".to_string(), [0usize].into_iter().collect())].into_iter().collect();
        apply_running_overlay(&mut views, &mut overlay);
        assert_eq!(views[0].steps[0].state, TeamStepState::Running);
        assert!(overlay.contains_key("m1"), "still tracked -- still pending after overlay applied");
    }

    #[test]
    fn watchable_missions_excludes_terminal_and_halted() {
        let mut done = view("m1", 100);
        done.phase = TeamMissionPhase::Done;
        let mut rejected = view("m2", 0);
        rejected.phase = TeamMissionPhase::Rejected;
        let mut halted = view("m3", 50);
        halted.phase = TeamMissionPhase::Halted;
        let mut executing = view("m4", 20);
        executing.phase = TeamMissionPhase::Executing;
        let mut paused = view("m5", 60);
        paused.phase = TeamMissionPhase::Paused;
        let mut awaiting = view("m6", 40);
        awaiting.phase = TeamMissionPhase::AwaitingApproval;

        let all = vec![done, rejected, halted, executing.clone(), paused.clone(), awaiting.clone()];
        let watchable = watchable_missions(&all);
        let ids: Vec<&str> = watchable.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, vec!["m4", "m5", "m6"], "only Executing/Paused/AwaitingApproval, in original order");
    }

    #[test]
    fn apply_running_overlay_prunes_a_resolved_step_and_does_not_downgrade_it() {
        // The fresh poll already shows this step Done -- the overlay must
        // NOT downgrade it back to Running, and must stop tracking it.
        let mut v = view("m1", 100);
        v.steps = vec![TeamStepView {
            label: "a".into(),
            state: TeamStepState::Done,
            step_id: "a".into(),
            member: "m".into(),
            kind: "delegate".into(),
            deps: vec![],
        }];
        let mut views = vec![v];
        let mut overlay: HashMap<String, HashSet<usize>> =
            [("m1".to_string(), [0usize].into_iter().collect())].into_iter().collect();
        apply_running_overlay(&mut views, &mut overlay);
        assert_eq!(views[0].steps[0].state, TeamStepState::Done, "checkpoint wins, not overwritten");
        assert!(!overlay.contains_key("m1"), "pruned once resolved");
    }

    #[test]
    fn build_mission_graph_has_one_node_per_roster_member_including_idle_ones() {
        let mut m = view("v1", 33);
        m.lead = "coordinator".to_string();
        m.steps = vec![
            TeamStepView { label: "count — inventory (delegate)".into(), state: TeamStepState::Running, step_id: "count".into(), member: "inventory".into(), kind: "delegate".into(), deps: vec![] },
        ];
        let roster = sample_roster();
        let graph = build_mission_graph(&m, &roster);
        // All 3 roster members get a node, even "purchasing" (idle -- no
        // step of theirs has run or is running yet).
        let names: Vec<&str> = graph.nodes.iter().map(|n| n.name.as_str()).collect();
        assert!(names.contains(&"coordinator"));
        assert!(names.contains(&"inventory"));
        assert!(names.contains(&"purchasing"));
        let lead_node = graph.nodes.iter().find(|n| n.name == "coordinator").unwrap();
        assert!(lead_node.is_lead);
        let inventory_node = graph.nodes.iter().find(|n| n.name == "inventory").unwrap();
        assert!(!inventory_node.is_lead);
        assert_eq!(inventory_node.state, TeamStepState::Running, "inventory is running the 'count' step");
        let purchasing_node = graph.nodes.iter().find(|n| n.name == "purchasing").unwrap();
        assert_eq!(purchasing_node.state, TeamStepState::Pending, "idle -- no step touches purchasing yet");
    }

    #[test]
    fn build_mission_graph_edges_reflect_step_deps() {
        let mut m = view("v1", 0);
        m.steps = vec![
            TeamStepView { label: "a".into(), state: TeamStepState::Done, step_id: "a".into(), member: "inventory".into(), kind: "delegate".into(), deps: vec![] },
            TeamStepView { label: "b".into(), state: TeamStepState::Pending, step_id: "b".into(), member: "purchasing".into(), kind: "delegate".into(), deps: vec!["a".to_string()] },
        ];
        let roster = sample_roster();
        let graph = build_mission_graph(&m, &roster);
        assert_eq!(graph.edges.len(), 1);
        assert_eq!(graph.edges[0].from_step, "a");
        assert_eq!(graph.edges[0].to_step, "b");
    }

    #[test]
    fn build_mission_graph_a_specialist_with_multiple_steps_shows_the_most_attention_worthy_state() {
        // A specialist who ran one step to Done and has another Pending
        // should show Pending (still work to do), not Done (which would
        // read as "finished" when they aren't).
        let mut m = view("v1", 0);
        m.steps = vec![
            TeamStepView { label: "a".into(), state: TeamStepState::Done, step_id: "a".into(), member: "inventory".into(), kind: "delegate".into(), deps: vec![] },
            TeamStepView { label: "b".into(), state: TeamStepState::Pending, step_id: "b".into(), member: "inventory".into(), kind: "delegate".into(), deps: vec!["a".to_string()] },
        ];
        let roster = sample_roster();
        let graph = build_mission_graph(&m, &roster);
        let inventory_node = graph.nodes.iter().find(|n| n.name == "inventory").unwrap();
        assert_eq!(inventory_node.state, TeamStepState::Pending);
    }

    #[test]
    fn build_mission_graph_includes_a_specialist_pinned_off_the_current_default_roster() {
        // A pack-pinned mission's own TeamConfig can differ from the daemon's
        // current default roster (all `build_mission_graph` is given --
        // `TeamMissionView` doesn't expose the mission's own pinned config).
        // A specialist named in `mission.steps` but absent from
        // `roster.members` must still get a node -- not silently vanish --
        // marked `on_roster: false`.
        let mut m = view("v1", 0);
        m.lead = "coordinator".to_string();
        m.steps = vec![TeamStepView {
            label: "audit".into(),
            state: TeamStepState::Running,
            step_id: "audit".into(),
            member: "auditor".into(),
            kind: "delegate".into(),
            deps: vec![],
        }];
        let roster = sample_roster(); // coordinator/inventory/purchasing -- no "auditor"
        let graph = build_mission_graph(&m, &roster);
        let node = graph
            .nodes
            .iter()
            .find(|n| n.name == "auditor")
            .expect("an unrostered but step-touching member still gets a node");
        assert!(!node.on_roster, "auditor is not on the current default roster");
        assert!(!node.is_lead);
        assert_eq!(node.state, TeamStepState::Running);
        assert!(graph.nodes.iter().any(|n| n.name == "coordinator" && n.on_roster));
    }

    #[test]
    fn build_mission_graph_is_lead_follows_the_missions_own_lead_not_the_default_rosters() {
        let mut m = view("v1", 0);
        m.lead = "inventory".to_string(); // differs from roster.lead ("coordinator")
        let roster = sample_roster();
        let graph = build_mission_graph(&m, &roster);
        let inventory_node = graph.nodes.iter().find(|n| n.name == "inventory").unwrap();
        assert!(inventory_node.is_lead, "is_lead follows mission.lead, not roster.lead");
        let coordinator_node = graph.nodes.iter().find(|n| n.name == "coordinator").unwrap();
        assert!(!coordinator_node.is_lead);
    }

    #[test]
    fn build_mission_graph_gives_the_missions_own_lead_a_node_even_when_they_touch_no_step() {
        // A pack-pinned lead who only ever delegates/reviews (owns no step of
        // their own) and isn't a member of the current default roster must
        // still get a node -- otherwise there's no LEAD to center the graph
        // on.
        let mut m = view("v1", 0);
        m.lead = "packlead".to_string();
        m.steps = vec![TeamStepView {
            label: "count".into(),
            state: TeamStepState::Pending,
            step_id: "count".into(),
            member: "inventory".into(),
            kind: "delegate".into(),
            deps: vec![],
        }];
        let roster = sample_roster(); // coordinator/inventory/purchasing -- no "packlead"
        let graph = build_mission_graph(&m, &roster);
        let lead_node = graph
            .nodes
            .iter()
            .find(|n| n.name == "packlead")
            .expect("the mission's own lead always gets a node, even off-roster and step-touching-free");
        assert!(lead_node.is_lead);
        assert!(!lead_node.on_roster);
    }

    #[test]
    fn build_mission_graph_does_not_duplicate_an_off_roster_lead_who_also_touches_a_step() {
        // The more likely pack-pinned shape: the lead is off-roster AND
        // owns a step themselves (unlike the previous test, where the
        // lead touches no step at all). The `unrostered` loop already
        // gives them a node from their own step -- the later "give the
        // lead a node" union must recognize that and NOT add a second
        // one, or the graph would draw two circles for the same person
        // (one centered, one on the ring), both badged LEAD, with edges
        // landing on whichever one a name-keyed lookup finds last.
        let mut m = view("v1", 0);
        m.lead = "packlead".to_string();
        m.steps = vec![TeamStepView {
            label: "audit".into(),
            state: TeamStepState::Running,
            step_id: "audit".into(),
            member: "packlead".into(),
            kind: "delegate".into(),
            deps: vec![],
        }];
        let roster = sample_roster(); // coordinator/inventory/purchasing -- no "packlead"
        let graph = build_mission_graph(&m, &roster);
        let count = graph.nodes.iter().filter(|n| n.name == "packlead").count();
        assert_eq!(count, 1, "an off-roster, step-touching lead gets exactly one node, not two");
    }

    #[test]
    fn build_mission_graph_skips_an_edge_whose_dep_step_id_is_not_in_the_mission() {
        let mut m = view("v1", 0);
        m.steps = vec![TeamStepView {
            label: "b".into(),
            state: TeamStepState::Pending,
            step_id: "b".into(),
            member: "inventory".into(),
            kind: "delegate".into(),
            deps: vec!["nope".to_string()],
        }];
        let roster = sample_roster();
        let graph = build_mission_graph(&m, &roster);
        assert!(graph.edges.is_empty(), "a dep referencing an unknown step id is skipped, not fabricated");
    }

    #[test]
    fn layout_mission_nodes_centers_the_lead_and_rings_the_specialists_equidistant() {
        let nodes = vec![
            MissionGraphNode {
                name: "inventory".into(), is_lead: false, state: TeamStepState::Pending,
                current_step: None, on_roster: true,
            },
            MissionGraphNode {
                name: "coordinator".into(), is_lead: true, state: TeamStepState::Pending,
                current_step: None, on_roster: true,
            },
            MissionGraphNode {
                name: "purchasing".into(), is_lead: false, state: TeamStepState::Pending,
                current_step: None, on_roster: true,
            },
        ];
        let pos = layout_mission_nodes(&nodes);
        assert_eq!(pos.len(), 3);
        let center = (MC_GRAPH_W / 2.0, MC_GRAPH_H / 2.0);
        assert_eq!(pos[1], center, "the LEAD (deliberately at index 1, not 0) sits at the canvas center");
        let dist = |p: (f64, f64)| ((p.0 - center.0).powi(2) + (p.1 - center.1).powi(2)).sqrt();
        let (d0, d2) = (dist(pos[0]), dist(pos[2]));
        assert!(d0 > 10.0, "specialists are not collapsed onto the LEAD");
        assert!((d0 - d2).abs() < 0.01, "every specialist sits on the same ring around the LEAD");
        assert!(
            (pos[0].0 - pos[2].0).abs() > 1.0 || (pos[0].1 - pos[2].1).abs() > 1.0,
            "distinct specialists get distinct positions"
        );
    }

    #[test]
    fn layout_mission_nodes_handles_a_lead_only_roster() {
        let nodes = vec![MissionGraphNode {
            name: "coordinator".into(), is_lead: true, state: TeamStepState::Pending,
            current_step: None, on_roster: true,
        }];
        let pos = layout_mission_nodes(&nodes);
        assert_eq!(pos, vec![(MC_GRAPH_W / 2.0, MC_GRAPH_H / 2.0)]);
    }

    #[test]
    fn lead_scopes_returns_the_leads_own_declared_scopes() {
        let mut roster = sample_roster();
        roster.members[0].capability_scopes = vec!["fs.write".to_string(), "net.fetch".to_string()];
        // A non-lead member ("inventory", `members[1]` -- confirmed not the
        // lead, which is "coordinator" at `members[0]` per `sample_roster`)
        // with DIFFERENT scopes -- these must NOT leak into lead_scopes's
        // result, or a "union of all members" bug (instead of "just the
        // lead's own scopes") would silently pass.
        roster.members[1].capability_scopes = vec!["shell.exec".to_string()];
        let scopes = lead_scopes(&roster);
        assert!(scopes.contains("fs.write"));
        assert!(scopes.contains("net.fetch"));
        assert!(!scopes.contains("shell.exec"), "a non-lead member's own scope must not leak into lead_scopes");
        assert_eq!(scopes.len(), 2);
    }

    #[test]
    fn lead_scopes_is_empty_when_the_lead_is_not_in_members() {
        let mut roster = sample_roster();
        roster.lead = "nobody".to_string();
        assert!(lead_scopes(&roster).is_empty());
    }

    #[test]
    fn scopes_of_returns_the_named_leads_own_scopes_not_the_default_rosters_lead() {
        let mut roster = sample_roster(); // lead = "coordinator"
        roster.members[0].capability_scopes = vec!["fs.write".to_string()]; // coordinator
        roster.members[1].capability_scopes = vec!["net.fetch".to_string()]; // inventory
        let scopes = scopes_of(&roster, "inventory");
        assert_eq!(scopes, Some(["net.fetch".to_string()].into_iter().collect()));
    }

    #[test]
    fn scopes_of_returns_none_for_a_lead_not_on_the_given_roster() {
        let roster = sample_roster();
        assert_eq!(scopes_of(&roster, "packlead"), None);
    }

    #[test]
    fn gate_label_omits_attempt_suffix_on_first_attempt() {
        assert_eq!(
            gate_label("gate_review_brief", 0),
            "⚑ awaiting approval — gate_review_brief"
        );
    }

    #[test]
    fn gate_label_includes_attempt_suffix_on_retry() {
        // verify_attempts == 1 means one verification has already failed —
        // the mission is on its 2nd attempt (MAX_MISSION_ATTEMPTS caps this
        // count at 1, so this is the only value that ever fires in practice).
        assert_eq!(
            gate_label("gate_review_brief", 1),
            "⚑ awaiting approval — gate_review_brief (attempt 2)"
        );
    }

    #[test]
    fn mission_controls_shown_for_each_phase() {
        assert!(controls_for_phase(TeamMissionPhase::Planning).is_empty());
        assert_eq!(controls_for_phase(TeamMissionPhase::Executing), vec!["pause", "abort"]);
        assert_eq!(controls_for_phase(TeamMissionPhase::Paused), vec!["resume"]);
        assert_eq!(controls_for_phase(TeamMissionPhase::AwaitingApproval), vec!["gate"]);
        assert!(controls_for_phase(TeamMissionPhase::Done).is_empty());
        assert!(controls_for_phase(TeamMissionPhase::Rejected).is_empty());
        assert!(controls_for_phase(TeamMissionPhase::Halted).is_empty());
    }
}

// ---------------------------------------------------------------------------
// Teams — the Nonagon roster (Chapters Y read + Roster RO.3 edit). Renders the
// daemon's active TeamConfig as an editable form: team name/description, the
// lead pick, and per-member role / trust / scopes / tools / soul, with
// add/remove specialist (≤9) and Save → SetTeamRoster (server-validated; the
// team is adopted on the next daemon restart). NT-02 is unchanged — a member
// scope the lead lacks is flagged inert, never granted.
// ---------------------------------------------------------------------------

#[component]
fn TeamsPanel() -> Element {
    let ws = use_context::<Sender>();
    let connected = use_context::<Signal<bool>>();
    let mut teams = use_context::<Signal<TeamsState>>();
    let missions = use_context::<Signal<Vec<TeamMissionView>>>();

    // The edit draft, seeded once from the loaded roster.
    let mut draft = use_signal(|| None::<TeamConfig>);
    use_future(move || async move {
        ws.send(get_team_roster_query());
    });
    use_effect(move || {
        if let Some(r) = teams().roster {
            if draft.peek().is_none() {
                draft.set(Some(r));
            }
        }
    });

    // Chapter Nonagon Templates — "Draft from my role" card state.
    let mut show_draft_box = use_signal(|| false);
    let mut draft_description = use_signal(String::new);
    // A drafted roster REPLACES the edit draft (the operator reviews/edits
    // it in the exact same form manual edits use — no separate preview
    // UI). Fires once per response via the tick, not on every unrelated
    // TeamsState write.
    let draft_tick = use_memo(move || teams().draft_resp);
    use_effect(move || {
        let t = draft_tick();
        if t > 0 {
            if let Some(r) = teams().drafted_roster {
                draft.set(Some(r));
                show_draft_box.set(false);
            }
        }
    });

    let st = teams();
    let Some(team) = draft() else {
        return rsx! {
            div { class: "teams",
                div { class: "panel-head", h3 { "Team" } }
                SkeletonList { rows: 4 }
            }
        };
    };

    // The lead's declared scopes — for the NT-02 "inert" hint on specialists.
    let lead_scopes = lead_scopes(&team);
    let specialist_count = team.members.iter().filter(|m| m.name != team.lead).count();
    let active = missions()
        .iter()
        .filter(|m| !matches!(m.phase, TeamMissionPhase::Done | TeamMissionPhase::Rejected))
        .count();
    let member_names: Vec<String> = team.members.iter().map(|m| m.name.clone()).collect();
    let dirty = st.roster.as_ref() != Some(&team);
    // Chapter Nonagon Templates — the cap was specialists-only
    // (lead + 9 = 10 total), one over the shape the name promises.
    // 9 total = lead + 8 specialists.
    let can_add = specialist_count < 8;

    rsx! {
        div { class: "teams",
            if st.restart_required {
                div { class: "glass-card restart-banner",
                    strong { "Saved — restart the daemon to run the new team." }
                    p { class: "label-tech",
                        "The team is assembled at startup. Run  "
                        code { "aivyx-pa daemon stop && aivyx-pa daemon run" }
                    }
                }
            }
            if let Some((ok, msg)) = st.notice.clone() {
                div { class: if ok { "notice ok" } else { "notice err" }, "{msg}" }
            }
            if let Some((ok, msg)) = st.draft_notice.clone() {
                div { class: if ok { "notice ok" } else { "notice err" }, "{msg}" }
            }

            // Chapter Nonagon Templates — draft a role-tailored roster.
            // The result REPLACES the edit draft below; nothing saves
            // until the operator reviews it and clicks Save team, same
            // as any manual edit.
            div { class: "glass-card settings-section",
                div { class: "panel-head",
                    h3 { "Draft a team for my role" }
                    span { class: "label-tech",
                        "generates all 8 specialists from your Profile role + use cases"
                    }
                }
                if show_draft_box() {
                    div { class: "field-row",
                        label { class: "label-tech", "Extra context (optional)" }
                        textarea {
                            class: "input",
                            rows: "3",
                            placeholder: "e.g. we run 15 kitchens and care most about food safety and cost control",
                            value: "{draft_description}",
                            oninput: move |e| draft_description.set(e.value()),
                        }
                    }
                    div { style: "display:flex; gap:6px;",
                        button {
                            class: "btn btn-primary btn-xs",
                            disabled: !connected() || st.drafting,
                            onclick: move |_| {
                                teams.write().drafting = true;
                                ws.send(draft_team_template_query(draft_description()));
                            },
                            if st.drafting { "Drafting…" } else { "Generate" }
                        }
                        button {
                            class: "btn btn-glass",
                            disabled: st.drafting,
                            onclick: move |_| show_draft_box.set(false),
                            "Cancel"
                        }
                    }
                } else {
                    button {
                        class: "btn btn-glass",
                        disabled: !connected(),
                        onclick: move |_| show_draft_box.set(true),
                        "Draft from my role"
                    }
                }
            }

            // Team identity.
            div { class: "glass-card settings-section",
                div { class: "panel-head",
                    h3 { "Team" }
                    span { class: "chip", "{team.members.len()} members" }
                    span { class: "chip", "{active} active" }
                }
                div { class: "field-row",
                    label { class: "label-tech", "Name" }
                    input { class: "input", value: "{team.name}",
                        oninput: move |e| { if let Some(t) = draft.write().as_mut() { t.name = e.value(); } } }
                }
                div { class: "field-row",
                    label { class: "label-tech", "Description" }
                    input { class: "input", value: "{team.description}",
                        oninput: move |e| { if let Some(t) = draft.write().as_mut() { t.description = e.value(); } } }
                }
                div { class: "field-row",
                    label { class: "label-tech", "Lead" }
                    select { class: "input", value: "{team.lead}",
                        onchange: move |e| { if let Some(t) = draft.write().as_mut() { t.lead = e.value(); } },
                        for n in member_names.clone() {
                            option { value: "{n}", "{n}" }
                        }
                    }
                }
            }

            // Per-member editors.
            div { class: "roster-grid",
                {team.members.clone().into_iter().enumerate().map(|(i, m)| {
                    let is_lead = m.name == team.lead;
                    let scopes_text = m.capability_scopes.join("\n");
                    let tools_text = m.tool_allowlist.join("\n");
                    let widened: Vec<String> = if is_lead {
                        Vec::new()
                    } else {
                        m.capability_scopes.iter().filter(|s| !lead_scopes.contains(*s)).cloned().collect()
                    };
                    let widened_text = widened.join(", ");
                    rsx! {
                        div { key: "{i}",
                            class: if is_lead { "glass-card member-card lead" } else { "glass-card member-card" },
                            div { class: "panel-head",
                                if is_lead { span { class: "chip warning", "lead" } }
                                span { class: "chip {trust_class(m.trust_ceiling)}", "{trust_label(m.trust_ceiling)}" }
                                if !is_lead {
                                    button { class: "btn btn-ghost-danger btn-xs",
                                        onclick: move |_| { if let Some(t) = draft.write().as_mut() { t.members.remove(i); } },
                                        "Remove" }
                                }
                            }
                            div { class: "field-row",
                                label { class: "label-tech", "Name" }
                                input { class: "input", value: "{m.name}",
                                    oninput: move |e| { if let Some(t) = draft.write().as_mut() { t.members[i].name = e.value(); } } }
                            }
                            div { class: "field-row",
                                label { class: "label-tech", "Role" }
                                input { class: "input", value: "{m.role}",
                                    oninput: move |e| { if let Some(t) = draft.write().as_mut() { t.members[i].role = e.value(); } } }
                            }
                            div { class: "field-row",
                                label { class: "label-tech", "Trust" }
                                select { class: "input", value: "{trust_label(m.trust_ceiling)}",
                                    onchange: move |e| {
                                        if let Some(tt) = trust_from_label(&e.value()) {
                                            if let Some(t) = draft.write().as_mut() { t.members[i].trust_ceiling = tt; }
                                        }
                                    },
                                    for tt in [TrustTier::Untrusted, TrustTier::SemiTrusted, TrustTier::Trusted, TrustTier::Kernel] {
                                        option { value: "{trust_label(tt)}", "{trust_label(tt)}" }
                                    }
                                }
                            }
                            div { class: "field-row",
                                label { class: "label-tech", "Scopes" }
                                textarea { class: "input", rows: "2", placeholder: "fs.read\nmemory.write",
                                    value: "{scopes_text}",
                                    oninput: move |e| { if let Some(t) = draft.write().as_mut() { t.members[i].capability_scopes = parse_token_list(&e.value()); } } }
                            }
                            div { class: "field-row",
                                label { class: "label-tech", "Tools" }
                                textarea { class: "input", rows: "2", placeholder: "fs.read\nteam.message",
                                    value: "{tools_text}",
                                    oninput: move |e| { if let Some(t) = draft.write().as_mut() { t.members[i].tool_allowlist = parse_token_list(&e.value()); } } }
                            }
                            div { class: "field-row",
                                label { class: "label-tech", "Soul" }
                                textarea { class: "input", rows: "3", value: "{m.soul}",
                                    oninput: move |e| { if let Some(t) = draft.write().as_mut() { t.members[i].soul = e.value(); } } }
                            }
                            if !widened_text.is_empty() {
                                p { class: "label-tech sub",
                                    "Lead lacks {widened_text} — inert until the lead holds them (attenuated at spawn)." }
                            }
                        }
                    }
                })}
            }

            // Add specialist + Save / Discard.
            div { class: "actions",
                button { class: "btn btn-glass", disabled: !can_add,
                    onclick: move |_| {
                        if let Some(t) = draft.write().as_mut() {
                            let n = t.members.len();
                            t.members.push(TeamMember {
                                name: format!("specialist-{n}"),
                                role: "Specialist".to_string(),
                                soul: String::new(),
                                tool_allowlist: vec!["team.message".to_string()],
                                capability_scopes: Vec::new(),
                                trust_ceiling: TrustTier::SemiTrusted,
                                // Chapter Ensemble — per-role model/endpoint
                                // default to the team's shared backend.
                                model: None,
                                base_url: None,
                            });
                        }
                    },
                    {if can_add { "Add specialist" } else { "Nonagon full (9 members)" }}
                }
                button { class: "btn btn-primary", disabled: !dirty || !connected(),
                    onclick: move |_| { if let Some(t) = draft() { ws.send(set_team_roster_query(&t)); } },
                    {if connected() { "Save team" } else { "reconnecting…" }} }
                button { class: "btn btn-glass", disabled: !dirty,
                    onclick: move |_| { draft.set(teams().roster); },
                    "Discard changes" }
            }
        }
    }
}

fn get_team_roster_query() -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-teams".to_string(),
        payload: QueryPayload::GetTeamRoster,
    }
}

/// Chapter Roster (RO.3) — persist the edited roster. The id is prefixed
/// `mc-teams` so a server-side validation `QueryError` routes to the Teams
/// banner.
fn set_team_roster_query(roster: &TeamConfig) -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-teams-set".to_string(),
        payload: QueryPayload::SetTeamRoster { roster: roster.clone() },
    }
}

/// Chapter Nonagon Templates — ask the daemon to draft a role-tailored
/// Nonagon. `description` is optional extra context beyond the operator's
/// declared Profile role/use-cases.
fn draft_team_template_query(description: String) -> FrontendMessage {
    FrontendMessage::DraftTeamTemplate {
        id: "mc-teams-draft".to_string(),
        description,
    }
}

/// Parse a scopes/tools textarea (newline-, comma-, or space-separated) into a
/// trimmed, non-empty token list.
fn parse_token_list(raw: &str) -> Vec<String> {
    raw.split(|c: char| c == '\n' || c == ',' || c.is_whitespace())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

/// Inverse of [`trust_label`] — parse a trust tier from the editor's `<select>`.
fn trust_from_label(s: &str) -> Option<TrustTier> {
    match s {
        "untrusted" => Some(TrustTier::Untrusted),
        "semi-trusted" => Some(TrustTier::SemiTrusted),
        "trusted" => Some(TrustTier::Trusted),
        "kernel" => Some(TrustTier::Kernel),
        _ => None,
    }
}

fn trust_label(t: TrustTier) -> &'static str {
    match t {
        TrustTier::Untrusted => "untrusted",
        TrustTier::SemiTrusted => "semi-trusted",
        TrustTier::Trusted => "trusted",
        TrustTier::Kernel => "kernel",
    }
}

/// Chip accent for a trust tier — higher trust reads success (calm), lower warning.
fn trust_class(t: TrustTier) -> &'static str {
    match t {
        TrustTier::Trusted | TrustTier::Kernel => "success",
        TrustTier::SemiTrusted => "warning",
        TrustTier::Untrusted => "muted",
    }
}

// ---------------------------------------------------------------------------
// Documents — the read-only file browser (Chapter Z). Two roots (workspace +
// the access-scoped fs_root); list + read, escape-guarded daemon-side.
// ---------------------------------------------------------------------------

#[component]
fn DocumentsPanel() -> Element {
    let ws = use_context::<Sender>();
    let mut documents = use_context::<Signal<DocumentsState>>();

    // Toolbar create state: Some(true)=new file, Some(false)=new folder.
    let mut new_kind = use_signal(|| None::<bool>);
    let mut new_name = use_signal(String::new);
    // Per-entry rename (the entry name being renamed) + the pending value.
    let mut rename_of = use_signal(|| None::<String>);
    let mut rename_to = use_signal(String::new);
    // The entry path pending a delete confirmation (→ modal).
    let mut delete_of = use_signal(|| None::<String>);

    // First entry → default to the workspace root.
    use_future(move || async move {
        if documents.read().root.is_empty() {
            documents.write().root = "workspace".to_string();
            ws.send(list_dir_query("workspace", ""));
        }
    });

    let d = documents();
    let root = if d.root.is_empty() { "workspace".to_string() } else { d.root.clone() };
    let cur = d.path.clone();
    let viewing = d.file.is_some();

    rsx! {
        div { class: "documents",
            // Toolbar: root switcher + New file / New folder.
            div { class: "doc-toolbar",
                button {
                    class: if root == "workspace" { "btn btn-primary btn-xs" } else { "btn btn-glass btn-xs" },
                    onclick: move |_| switch_doc_root(documents, ws, "workspace"),
                    "Workspace"
                }
                button {
                    class: if root == "fs" { "btn btn-primary btn-xs" } else { "btn btn-glass btn-xs" },
                    onclick: move |_| switch_doc_root(documents, ws, "fs"),
                    "Files"
                }
                if !viewing {
                    div { style: "flex:1" }
                    button { class: "btn btn-glass btn-xs", onclick: move |_| { new_name.set(String::new()); new_kind.set(Some(true)); }, "New file" }
                    button { class: "btn btn-glass btn-xs", onclick: move |_| { new_name.set(String::new()); new_kind.set(Some(false)); }, "New folder" }
                }
            }

            // New file/folder name input (inline).
            if let Some(is_file) = new_kind() {
                div { class: "add-row",
                    input { class: "input", placeholder: if is_file { "new-file.md" } else { "new-folder" },
                        value: "{new_name}", oninput: move |e| new_name.set(e.value()) }
                    {
                        let (r, base) = (root.clone(), cur.clone());
                        rsx! {
                            button {
                                class: "btn btn-primary btn-xs",
                                onclick: move |_| {
                                    let nm = new_name();
                                    if !nm.trim().is_empty() {
                                        let target = join_doc_path(&base, nm.trim());
                                        if is_file {
                                            ws.send(write_file_query(&r, &target, String::new(), false));
                                        } else {
                                            ws.send(make_dir_query(&r, &target));
                                        }
                                        ws.send(list_dir_refresh_query(&r, &base));
                                    }
                                    new_kind.set(None);
                                },
                                "Create"
                            }
                        }
                    }
                    button { class: "btn btn-glass btn-xs", onclick: move |_| new_kind.set(None), "Cancel" }
                }
            }

            // Breadcrumb: root + each ancestor segment, clickable to ascend.
            div { class: "breadcrumb",
                {
                    let r = root.clone();
                    rsx! {
                        button { class: "crumb", onclick: move |_| ws.send(list_dir_query(&r, "")),
                            {if root == "fs" { "fs_root" } else { "workspace" }} }
                    }
                }
                for (label, prefix) in breadcrumb_segments(&d.path) {
                    {
                        let (r, p) = (root.clone(), prefix.clone());
                        rsx! {
                            span { class: "crumb-sep", "/" }
                            button { class: "crumb", onclick: move |_| ws.send(list_dir_query(&r, &p)), "{label}" }
                        }
                    }
                }
            }

            if let Some((ok, msg)) = d.notice.clone() {
                div { class: if ok { "notice ok" } else { "notice err" }, "{msg}" }
            }

            // File viewer/editor (when one is open) else the directory listing.
            if let Some(file) = d.file.clone() {
                FileViewer { key: "{file.path}", file: file.clone(), root: root.clone() }
            } else if !d.loaded {
                SkeletonList { rows: 5 }
            } else {
                div { class: "glass-card doc-listing",
                    if d.entries.is_empty() {
                        p { class: "label-tech sub", "Empty directory." }
                    } else {
                        for e in d.entries.clone() {
                            {
                                let target = join_doc_path(&d.path, &e.name);
                                let (r, base) = (root.clone(), cur.clone());
                                let is_dir = e.kind == "dir";
                                let renaming = rename_of() == Some(e.name.clone());
                                rsx! {
                                    div { class: "doc-row",
                                        if renaming {
                                            input { class: "input", value: "{rename_to}", oninput: move |ev| rename_to.set(ev.value()) }
                                            {
                                                let (r2, base2, tgt2) = (r.clone(), base.clone(), target.clone());
                                                rsx! {
                                                    button { class: "btn btn-primary btn-xs",
                                                        onclick: move |_| {
                                                            let nm = rename_to();
                                                            if !nm.trim().is_empty() {
                                                                let new_target = join_doc_path(&base2, nm.trim());
                                                                ws.send(rename_path_query(&r2, &tgt2, &new_target));
                                                                ws.send(list_dir_refresh_query(&r2, &base2));
                                                            }
                                                            rename_of.set(None);
                                                        },
                                                        "Save"
                                                    }
                                                }
                                            }
                                            button { class: "btn btn-glass btn-xs", onclick: move |_| rename_of.set(None), "Cancel" }
                                        } else {
                                            {
                                                let (r3, tgt3) = (r.clone(), target.clone());
                                                rsx! {
                                                    button { class: "doc-open",
                                                        onclick: move |_| {
                                                            if is_dir { ws.send(list_dir_query(&r3, &tgt3)); }
                                                            else { ws.send(read_file_query(&r3, &tgt3)); }
                                                        },
                                                        span { class: "doc-ico", {kind_glyph(&e.kind)} }
                                                        span { class: "doc-name", "{e.name}" }
                                                        span { class: "doc-size label-tech",
                                                            {if e.kind == "file" { fmt_size(e.size_bytes) } else { String::new() }} }
                                                    }
                                                }
                                            }
                                            div { class: "doc-actions",
                                                {
                                                    let nm = e.name.clone();
                                                    rsx! {
                                                        button { class: "btn btn-glass btn-xs", title: "Rename", "aria-label": "Rename",
                                                            onclick: move |_| { rename_to.set(nm.clone()); rename_of.set(Some(nm.clone())); }, "✎" }
                                                    }
                                                }
                                                {
                                                    let tgt4 = target.clone();
                                                    rsx! {
                                                        button { class: "btn btn-glass btn-xs danger", title: "Delete", "aria-label": "Delete",
                                                            onclick: move |_| delete_of.set(Some(tgt4.clone())), "🗑" }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Delete confirm modal (the one hard-gated, no-undo action).
        if let Some(path) = delete_of() {
            div { class: "modal-scrim",
                div { class: "glass-card modal",
                    h3 { "Delete?" }
                    p { "Permanently delete  " code { "{path}" } "  from {root}? This cannot be undone." }
                    div { class: "actions",
                        button { class: "btn btn-glass", onclick: move |_| delete_of.set(None), "Cancel" }
                        {
                            let (r, base, p) = (root.clone(), cur.clone(), path.clone());
                            rsx! {
                                button { class: "btn btn-primary",
                                    onclick: move |_| {
                                        ws.send(delete_file_query(&r, &p));
                                        ws.send(list_dir_refresh_query(&r, &base));
                                        delete_of.set(None);
                                    },
                                    "Delete"
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

#[wasm_bindgen::prelude::wasm_bindgen(inline_js = "
export function mermaid_run() {
    if (window.mermaid) {
        window.mermaid.run();
    }
}
")]
extern "C" {
    fn mermaid_run();
}

/// Load the vendored mermaid.js exactly once per page session (checking
/// `window.mermaid` first so a second `.md` file with a mermaid fence
/// doesn't re-inject the `<script>` tag), then call `mermaid.run()` once
/// it's loaded — or immediately if it was already loaded by an earlier
/// call. `dangerous_inner_html`-injected `<script>` tags never execute
/// per the HTML spec, so this creates a real, appended `<script>` element
/// via `web_sys` instead.
fn ensure_mermaid_loaded_then_run() {
    use wasm_bindgen::JsCast;
    let Some(window) = web_sys::window() else { return };
    let Some(document) = window.document() else { return };
    let already_loaded = js_sys::Reflect::has(&window, &"mermaid".into()).unwrap_or(false);
    if already_loaded {
        mermaid_run();
        return;
    }
    let Ok(script) = document.create_element("script") else { return };
    script.set_attribute("src", &MERMAID_JS.to_string()).ok();
    let onload = wasm_bindgen::closure::Closure::<dyn FnMut()>::new(move || {
        mermaid_run();
    });
    if let Some(el) = script.dyn_ref::<web_sys::HtmlScriptElement>() {
        el.set_onload(Some(onload.as_ref().unchecked_ref()));
    }
    onload.forget();
    if let Some(head) = document.head() {
        let _ = head.append_child(&script);
    }
}

/// The file content pane — an editor for text files (textarea + Save), or a
/// "not shown" note for binary / over-cap files.
#[component]
fn FileViewer(file: DocFile, root: String) -> Element {
    let ws = use_context::<Sender>();
    let mut documents = use_context::<Signal<DocumentsState>>();
    // Seeded once per file (the panel keys this component by path, so it
    // remounts — and re-seeds — when a different file is opened).
    let mut edited = use_signal(|| file.content.clone().unwrap_or_default());
    let editable = file.content.is_some() && !file.binary;
    // POLISH_WAVES.md sub-project 6, item D — a non-truncated .md file
    // with content defaults to a rendered Preview instead of the plain
    // editable textarea every other file type gets; any file can still be
    // flipped to Source (which is exactly today's textarea/pre behavior,
    // unchanged) to see or edit the raw text.
    let is_md = guide::is_markdown_path(&file.path) && file.content.is_some() && !file.truncated;
    let mut preview = use_signal(move || is_md);

    // Computed once per render (not inside the `use_effect` below) so the
    // rendered HTML is reused rather than rebuilt twice; only actually
    // rendering markdown when there's a chance it's shown avoids paying for
    // it in Source mode.
    let mut preview_html: Option<String> = None;
    let mut has_mermaid = false;
    if is_md && preview() {
        let content = file.content.as_deref().unwrap_or_default();
        let html = guide::render_untrusted_markdown(content);
        has_mermaid = html.contains("class=\"mermaid\"");
        preview_html = Some(html);
    }

    // Hoisted unconditional (not inside `if has_mermaid { ... }`, which
    // would violate the rules of hooks) so it's called on every render, and
    // reads `preview()` inside its own body so it re-subscribes and re-runs
    // whenever Source <-> Preview is toggled — not just on the component's
    // first render. Toggling back into Preview re-inserts fresh, unprocessed
    // `<pre class="mermaid">` markup via `dangerous_inner_html`, which needs
    // mermaid.js to run again to typeset it.
    use_effect(move || {
        if preview() && is_md && has_mermaid {
            ensure_mermaid_loaded_then_run();
        }
    });

    rsx! {
        div { class: "glass-card doc-viewer",
            div { class: "panel-head",
                h4 { "{file.path}" }
                span { class: "chip", {fmt_size(file.size_bytes)} }
                if is_md {
                    button {
                        class: "btn btn-glass btn-xs",
                        onclick: move |_| preview.set(!preview()),
                        {if preview() { "Source" } else { "Preview" }}
                    }
                }
                if editable && !(is_md && preview()) {
                    {
                        let (r, p) = (root.clone(), file.path.clone());
                        rsx! {
                            button { class: "btn btn-primary btn-xs",
                                onclick: move |_| {
                                    ws.send(write_file_query(&r, &p, edited(), true));
                                    // Vitrine §8 — the bridge handles frames in
                                    // order, so this re-read returns the
                                    // post-write content and refreshes the open
                                    // file in place (no screen reload needed).
                                    ws.send(read_file_query(&r, &p));
                                },
                                "Save"
                            }
                        }
                    }
                }
                button { class: "btn btn-glass btn-xs", onclick: move |_| documents.write().file = None, "Close" }
            }
            if file.truncated {
                div { class: "notice err", "Showing the first 256 KB of a larger file — editing is disabled to avoid truncating it." }
            }
            if is_md && preview() {
                {
                    let html = preview_html.clone().unwrap_or_default();
                    rsx! {
                        div {
                            class: "guide-content doc-preview",
                            dangerous_inner_html: html,
                        }
                    }
                }
            } else if editable && !file.truncated {
                textarea { class: "doc-edit", spellcheck: "false",
                    value: "{edited}", oninput: move |e| edited.set(e.value()) }
            } else {
                match &file.content {
                    Some(text) => rsx! { pre { class: "doc-text", "{text}" } },
                    None => rsx! {
                        p { class: "label-tech sub",
                            {if file.binary {
                                format!("Binary file — {} not shown.", fmt_size(file.size_bytes))
                            } else {
                                "File too large to display.".to_string()
                            }}
                        }
                    },
                }
            }
        }
    }
}

/// Switch the Documents browser to `to` ("workspace" | "fs"), reset to that
/// root's top, and re-list. A free fn so both toolbar buttons can call it
/// (a shared closure can't be moved into two handlers).
fn switch_doc_root(mut documents: Signal<DocumentsState>, ws: Sender, to: &'static str) {
    {
        let mut st = documents.write();
        st.root = to.to_string();
        st.file = None;
        st.path = String::new();
    }
    ws.send(list_dir_query(to, ""));
}

fn list_dir_query(root: &str, path: &str) -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-docs-list".to_string(),
        payload: QueryPayload::ListDir { root: root.to_string(), path: path.to_string() },
    }
}

/// Re-list after a mutation — a distinct id so the ws_task keeps the outcome
/// notice (a navigation list clears it).
fn list_dir_refresh_query(root: &str, path: &str) -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-docs-refresh".to_string(),
        payload: QueryPayload::ListDir { root: root.to_string(), path: path.to_string() },
    }
}

// ── DW — the Documents write queries. ──

fn write_file_query(root: &str, path: &str, content: String, overwrite: bool) -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-docs-write".to_string(),
        payload: QueryPayload::WriteFile {
            root: root.to_string(),
            path: path.to_string(),
            content,
            overwrite,
        },
    }
}

fn delete_file_query(root: &str, path: &str) -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-docs-delete".to_string(),
        payload: QueryPayload::DeleteFile {
            root: root.to_string(),
            path: path.to_string(),
            confirm: true,
        },
    }
}

fn rename_path_query(root: &str, path: &str, new_path: &str) -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-docs-rename".to_string(),
        payload: QueryPayload::RenamePath {
            root: root.to_string(),
            path: path.to_string(),
            new_path: new_path.to_string(),
        },
    }
}

fn make_dir_query(root: &str, path: &str) -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-docs-mkdir".to_string(),
        payload: QueryPayload::MakeDir { root: root.to_string(), path: path.to_string() },
    }
}

fn read_file_query(root: &str, path: &str) -> FrontendMessage {
    FrontendMessage::Query {
        id: "mc-docs-read".to_string(),
        payload: QueryPayload::ReadFile { root: root.to_string(), path: path.to_string() },
    }
}

/// Join a relative dir `base` with an entry `name` (slash-separated).
fn join_doc_path(base: &str, name: &str) -> String {
    if base.is_empty() {
        name.to_string()
    } else {
        format!("{base}/{name}")
    }
}

/// `(label, cumulative-prefix)` for each segment of `path`, for the breadcrumb.
fn breadcrumb_segments(path: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut prefix = String::new();
    for seg in path.split('/').filter(|s| !s.is_empty()) {
        if prefix.is_empty() {
            prefix = seg.to_string();
        } else {
            prefix = format!("{prefix}/{seg}");
        }
        out.push((seg.to_string(), prefix.clone()));
    }
    out
}

fn kind_glyph(kind: &str) -> &'static str {
    match kind {
        "dir" => "📁",
        "symlink" => "🔗",
        "file" => "📄",
        _ => "•",
    }
}

/// Human-readable byte size (B / KB / MB).
fn fmt_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// The single WebSocket task: open `/ws`, run the read loop (fans inbound
/// envelopes into the view signals), and drain outbound `FrontendMessage`s.
#[allow(clippy::too_many_arguments)]
async fn ws_task(
    mut rx: UnboundedReceiver<FrontendMessage>,
    missions: Signal<Vec<TeamMissionView>>,
    running_overlay: Signal<HashMap<String, HashSet<usize>>>,
    dashboard: Signal<Dashboard>,
    memory: Signal<MemoryState>,
    memory_ui: Signal<MemoryUi>,
    wiki: Signal<WikiState>,
    lattice: Signal<GraphKnowledgeState>,
    settings: Signal<SettingsState>,
    agents: Signal<AgentsState>,
    teams: Signal<TeamsState>,
    documents: Signal<DocumentsState>,
    voice: Signal<VoiceState>,
    skills: Signal<SkillsState>,
    skills_ui: Signal<SkillsUi>,
    mcp: Signal<McpState>,
    mcp_config_ui: Signal<McpConfigUi>,
    tools: Signal<ToolsState>,
    gallery: Signal<GalleryState>,
    schedules_ui: Signal<SchedulesUi>,
    notifications: Signal<NotificationsState>,
    loop_ui: Signal<LoopUiState>,
    reminders_ui: Signal<RemindersState>,
    notify_config_ui: Signal<NotifyConfigUi>,
    audit_page: Signal<AuditState>,
    sessions_page: Signal<SessionsState>,
    mut connected: Signal<bool>,
    session: Signal<Option<String>>,
    transcript: Signal<Vec<ChatLine>>,
    streaming: Signal<String>,
    gate: Signal<Option<GateInfo>>,
    mission_ui: Signal<MissionControlUi>,
    server_info: Signal<ServerInfoUi>,
) {
    // Vitrine walkthrough fix (2026-07-05, third operator casualty): a
    // daemon restart used to END this task — the socket died, `connected`
    // flipped false (visible only as the Command Center chip), the write
    // loop broke, and every later `ws.send` from every screen vanished
    // silently into a dead coroutine. The page looked alive (stale
    // signals still rendered) while edits, mission starts, and roster
    // saves went nowhere. This loop reconnects with backoff, replays the
    // dashboard boot queries on every (re)connect, and re-sends the one
    // in-flight message a dying socket rejected. Outbound traffic flows
    // at least every POLL_INTERVAL_MS (the mission/audit poll), so a dead
    // socket is detected within one poll tick.
    let mut attempt: u32 = 0;
    // The message a dying socket refused — re-sent first on reconnect so
    // an operator action that raced the disconnect still lands.
    let mut unsent: Option<String> = None;
    loop {
        let ws = match WebSocket::open(&ws_url()) {
            Ok(ws) => ws,
            Err(_) => {
                connected.set(false);
                attempt = attempt.saturating_add(1);
                TimeoutFuture::new(reconnect_backoff_ms(attempt)).await;
                continue;
            }
        };
        connected.set(true);
        attempt = 0;
        let (mut write, read) = ws.split();

        spawn(read_task(
            read, missions, running_overlay, dashboard, memory, memory_ui, wiki, lattice, settings, agents,
            teams, documents, voice, skills, skills_ui, mcp, mcp_config_ui, tools, gallery, schedules_ui,
            notifications, loop_ui, reminders_ui, notify_config_ui, audit_page, sessions_page, connected, session, transcript, streaming,
            gate, mission_ui, server_info,
        ));

        // (Re)hydrate the dashboard one-shots — on a fresh page load this
        // duplicates the boot `use_future` harmlessly; on a reconnect it is
        // what refreshes the stale screens.
        for q in reconnect_boot_queries() {
            if let Ok(json) = serde_json::to_string(&q) {
                let _ = write.send(Message::Text(json)).await;
            }
        }
        if let Some(json) = unsent.take() {
            if write.send(Message::Text(json.clone())).await.is_err() {
                // This socket is already dead — stash the message back
                // and go straight to the next reconnect attempt.
                unsent = Some(json);
                connected.set(false);
                attempt = attempt.saturating_add(1);
                TimeoutFuture::new(reconnect_backoff_ms(attempt)).await;
                continue;
            }
        }

        // Outbound relay: runs until the socket dies (write error) or the
        // app tears down (rx closed).
        loop {
            match rx.next().await {
                Some(msg) => {
                    let Ok(json) = serde_json::to_string(&msg) else {
                        continue;
                    };
                    if write.send(Message::Text(json.clone())).await.is_err() {
                        unsent = Some(json);
                        break;
                    }
                }
                None => return,
            }
        }
        connected.set(false);
        attempt = attempt.saturating_add(1);
        TimeoutFuture::new(reconnect_backoff_ms(attempt)).await;
    }
}

/// Reconnect backoff: 1s, 2s, 4s, then 8s forever. Fast enough that a
/// deploy restart heals in seconds; slow enough not to hammer a daemon
/// that is genuinely down.
fn reconnect_backoff_ms(attempt: u32) -> u32 {
    match attempt {
        0 | 1 => 1_000,
        2 => 2_000,
        3 => 4_000,
        _ => 8_000,
    }
}

/// The dashboard's boot queries, re-sent on every (re)connect so a
/// reconnected page refreshes without navigation. Mission list + audit
/// refresh via the standing poll; screen-local data refreshes on view
/// switch.
fn reconnect_boot_queries() -> Vec<FrontendMessage> {
    let q = |id: &str, payload: QueryPayload| FrontendMessage::Query {
        id: id.to_string(),
        payload,
    };
    vec![
        q("mc-profile", QueryPayload::GetProfile { from_disk: false }),
        q("mc-verify", QueryPayload::VerifyAuditChain),
        q("mc-settings", QueryPayload::GetSettings),
        q("mc-schedules", QueryPayload::GetSchedules),
        q("mc-teams-roster", QueryPayload::GetTeamRoster),
        q("mc-learning", QueryPayload::GetLearningInsights { window_secs: None }),
    ]
}

#[allow(clippy::too_many_arguments)]
async fn read_task(
    mut read: futures_util::stream::SplitStream<WebSocket>,
    mut missions: Signal<Vec<TeamMissionView>>,
    mut running_overlay: Signal<HashMap<String, HashSet<usize>>>,
    mut dashboard: Signal<Dashboard>,
    mut memory: Signal<MemoryState>,
    mut memory_ui: Signal<MemoryUi>,
    mut wiki: Signal<WikiState>,
    mut lattice: Signal<GraphKnowledgeState>,
    mut settings: Signal<SettingsState>,
    mut agents: Signal<AgentsState>,
    mut teams: Signal<TeamsState>,
    mut documents: Signal<DocumentsState>,
    mut voice: Signal<VoiceState>,
    mut skills: Signal<SkillsState>,
    mut skills_ui: Signal<SkillsUi>,
    mut mcp: Signal<McpState>,
    mut mcp_config_ui: Signal<McpConfigUi>,
    mut tools: Signal<ToolsState>,
    mut gallery: Signal<GalleryState>,
    mut schedules_ui: Signal<SchedulesUi>,
    mut notifications: Signal<NotificationsState>,
    mut loop_ui: Signal<LoopUiState>,
    mut reminders_ui: Signal<RemindersState>,
    mut notify_config_ui: Signal<NotifyConfigUi>,
    mut audit_page: Signal<AuditState>,
    mut sessions_page: Signal<SessionsState>,
    mut connected: Signal<bool>,
    mut session: Signal<Option<String>>,
    mut transcript: Signal<Vec<ChatLine>>,
    mut streaming: Signal<String>,
    mut gate: Signal<Option<GateInfo>>,
    mut mission_ui: Signal<MissionControlUi>,
    mut server_info: Signal<ServerInfoUi>,
) {
    // POLISH_WAVES.md sub-project 5, item E — the conflict resolve/dismiss
    // acks below need to re-issue `mem_conflicts_query()` after a
    // successful mutation. `read_task` is a spawned task (not a
    // component), so it can't take `Sender` as an explicit prop the way
    // components grab it via `use_context::<Sender>()` — but this task IS
    // scope-bound to `App` (spawned by `ws_task`, itself the body of
    // `App`'s own `use_coroutine`), and `use_coroutine` auto-registers its
    // returned handle as context on that same scope, so consume_context()
    // is the non-hook equivalent, avoiding a rules-of-hooks violation from
    // calling a hook outside a render pass, while resolving the same
    // context.
    let ws = consume_context::<Sender>();
    {
        while let Some(Ok(Message::Text(text))) = read.next().await {
            let Ok(env) = serde_json::from_str::<DaemonEnvelope>(&text) else {
                continue;
            };
            match env {
                DaemonEnvelope::SessionStarted { session_id } => session.set(Some(session_id)),
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::TeamMissionList { missions: records },
                    ..
                } => {
                    let mut views: Vec<TeamMissionView> =
                        records.iter().map(|r| r.to_view()).collect();
                    let mut overlay = running_overlay();
                    apply_running_overlay(&mut views, &mut overlay);
                    running_overlay.set(overlay);
                    missions.set(views);
                }
                // Chapter Mission Control — a live push: apply it in place
                // rather than waiting for the next poll. The poll above
                // stays as-is (a reconnect/missed-broadcast reconciliation
                // fallback), not removed.
                DaemonEnvelope::TeamMissionUpdated { view } => {
                    let indices = running_step_indices(&view);
                    let mut overlay = running_overlay();
                    if indices.is_empty() {
                        overlay.remove(&view.id);
                    } else {
                        overlay.insert(view.id.clone(), indices);
                    }
                    running_overlay.set(overlay);
                    let mut current = missions();
                    upsert_mission_view(&mut current, view);
                    missions.set(current);
                }
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::GetTeamRoster { roster: cfg },
                    ..
                } => {
                    teams.write().roster = Some(cfg);
                }
                // Chapter Roster (RO.3) — a SetTeamRoster save was accepted: the
                // daemon echoes the validated, re-read roster + restart flag.
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::TeamRosterApplied { roster: cfg, restart_required },
                    ..
                } => {
                    let mut t = teams.write();
                    t.roster = Some(cfg);
                    t.restart_required = restart_required;
                    t.notice = Some((true, "Team saved to the team config file.".to_string()));
                }
                // Chapter Z — Documents browser: a directory listing arrived;
                // the echoed `path` is authoritative. A *refresh* re-list (after
                // a DW mutation) keeps the outcome notice; a *navigation* re-list
                // clears it.
                DaemonEnvelope::QueryResponse {
                    id,
                    payload: QueryResponsePayload::ListDir { entries, path },
                } => {
                    let mut d = documents.write();
                    d.entries = entries;
                    d.path = path;
                    d.loaded = true;
                    // A *refresh* re-list (after a DW mutation) keeps the
                    // outcome notice AND the open file — Vitrine §8: closing
                    // the viewer out from under an edit read as data loss. A
                    // navigation re-list clears both.
                    if !id.starts_with("mc-docs-refresh") {
                        d.file = None;
                        d.notice = None;
                    }
                }
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::ReadFile { file },
                    ..
                } => {
                    let mut d = documents.write();
                    d.file = Some(file);
                    d.notice = None;
                }
                // DW — a write mutation acked: set the outcome notice + bump the
                // refresh tick so the panel re-lists the directory.
                DaemonEnvelope::QueryResponse {
                    id,
                    payload: QueryResponsePayload::FsMutation { ok, error },
                } if id.starts_with("mc-docs") => {
                    let mut d = documents.write();
                    d.notice = Some(if ok {
                        let what = if id.contains("delete") {
                            "Deleted."
                        } else if id.contains("rename") {
                            "Renamed."
                        } else if id.contains("mkdir") {
                            "Folder created."
                        } else {
                            "Saved."
                        };
                        (true, what.to_string())
                    } else {
                        (false, error.unwrap_or_else(|| "Operation failed.".to_string()))
                    });
                }
                // A denied path / read failure on the Documents screen (ids
                // prefixed `mc-docs`) → a notice, leaving the listing intact.
                DaemonEnvelope::QueryResponse {
                    id,
                    payload: QueryResponsePayload::QueryError { message, .. },
                } if id.starts_with("mc-docs") => {
                    documents.write().notice = Some((false, message));
                }
                // /classic retirement — the dedicated Audit screen sends its
                // own ListAuditEntries with a different id ("audit-page");
                // guard this arm to the Command Center poll's own id so the
                // two screens' state don't cross-populate (same pattern the
                // GetProfile handler already uses to route mc-agents-get vs
                // the dashboard's own snapshot).
                DaemonEnvelope::QueryResponse {
                    id,
                    payload: QueryResponsePayload::ListAuditEntries { entries, total_len },
                } if id == "mc-audit" => {
                    let mut d = dashboard.write();
                    d.audit_entries = entries;
                    d.audit_total = total_len;
                    // First dashboard snapshot in — switch off the skeleton.
                    d.loaded = true;
                }
                DaemonEnvelope::QueryResponse {
                    id,
                    payload: QueryResponsePayload::ListAuditEntries { entries, total_len },
                } if id == "audit-page" => {
                    // `from_seq` is deliberately NOT set here from the
                    // response's own data — it's set at each request site
                    // (mount / Older / Newer, in `AuditPanel`; the
                    // correction follow-up, in `App`'s effect) right before
                    // the query is sent, so `AuditState.from_seq` always
                    // reflects "what we last asked for" rather than
                    // something inferred (unreliably) from the response.
                    //
                    // What this response's own `total_len` DOES let us
                    // check: whether the request that produced it was an
                    // unverified mount-time guess (`pending_guess`), and if
                    // so, whether that guess was wrong now that the real
                    // `total_len` is known. This check is per-response, not
                    // a one-shot-per-session latch — `pending_guess` is
                    // reset to `true` by every fresh `AuditPanel` mount, so
                    // a chain that grew while the operator was on another
                    // screen gets corrected again on every later visit, not
                    // just the first one ever.
                    let mut a = audit_page.write();
                    let was_pending_guess = a.pending_guess;
                    a.entries = entries;
                    a.total_len = total_len;
                    a.pending_guess = false;
                    if was_pending_guess {
                        let newest_from_seq = total_len.saturating_sub(AUDIT_PAGE_SIZE as u64);
                        if a.from_seq != newest_from_seq {
                            // Hand off to `App`'s effect, which alone holds
                            // the `ws` sender needed to actually fire the
                            // follow-up query — see `pending_correction`'s
                            // doc comment.
                            a.pending_correction = Some(newest_from_seq);
                        }
                    }
                }
                // `/classic` retirement — the Sessions screen: no id-guard
                // needed, `ListSessions` has no other consumer today.
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::ListSessions { sessions },
                    ..
                } => {
                    sessions_page.write().sessions = sessions;
                }
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::VerifyAuditChain { ok, .. },
                    ..
                } => {
                    dashboard.write().chain_ok = Some(ok);
                }
                DaemonEnvelope::QueryResponse {
                    id,
                    payload: QueryResponsePayload::GetProfile { profile },
                } => {
                    // Route by query id: the editor's request (`mc-agents-get`,
                    // from_disk) seeds the editor's on-disk view; every other
                    // GetProfile is the dashboard's active/running snapshot for
                    // the name chip. They carry different data now, so they must
                    // not cross-populate.
                    if id == "mc-agents-get" {
                        agents.write().profile = Some(profile);
                    } else {
                        dashboard.write().assistant_name = Some(profile.assistant_name);
                    }
                }
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::ListMemoryTopics { topics },
                    ..
                } => {
                    memory.write().topics = topics;
                }
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::GetMemoryGraph { nodes, edges },
                    ..
                } => {
                    let mut m = memory.write();
                    m.graph_nodes = nodes;
                    m.graph_edges = edges;
                }
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::GetMemoryTopicEntries { entries },
                    ..
                } => {
                    let mut m = memory.write();
                    m.entries = entries;
                    m.fell_back = false;
                    m.loaded = true;
                }
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::SearchMemory { matches, fell_back_to_keyword },
                    ..
                } => {
                    let mut m = memory.write();
                    m.entries = matches;
                    m.fell_back = fell_back_to_keyword;
                    m.loaded = true;
                }
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::MemoryConflicts { conflicts },
                    ..
                } => {
                    memory.write().conflicts = conflicts;
                }
                DaemonEnvelope::MemoryConflictResolved { ok, removed, error, .. } => {
                    let msg = if ok {
                        if removed {
                            "Conflict resolved.".to_string()
                        } else {
                            "That entry was already gone.".to_string()
                        }
                    } else {
                        error.unwrap_or_else(|| "Resolve failed.".to_string())
                    };
                    memory_ui.write().notice = Some((ok, msg));
                    if ok {
                        ws.send(mem_conflicts_query());
                    }
                }
                DaemonEnvelope::MemoryConflictDismissed { ok, error, .. } => {
                    let msg = if ok {
                        "Dismissed — kept both.".to_string()
                    } else {
                        error.unwrap_or_else(|| "Dismiss failed.".to_string())
                    };
                    memory_ui.write().notice = Some((ok, msg));
                    if ok {
                        ws.send(mem_conflicts_query());
                    }
                }
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::ListWikiPages { pages },
                    ..
                } => {
                    wiki.write().pages = pages;
                }
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::GetSkills { skills: sk, pending_proposals },
                    ..
                } => {
                    let mut s = skills.write();
                    s.skills = sk;
                    s.pending_proposals = pending_proposals;
                    s.loaded = true;
                }
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::GetMcpStatus { captured_unix, servers },
                    ..
                } => {
                    // Chapter Lantern — the MCP screen's snapshot.
                    let mut m = mcp.write();
                    m.servers = servers;
                    m.captured_unix = captured_unix;
                    m.loaded = true;
                }
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::McpServerCallStats { servers },
                    ..
                } => {
                    // POLISH_WAVES.md sub-project 8 item C.
                    mcp.write().call_stats = servers;
                }
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::GetMcpServerConfigs { servers },
                    ..
                } => {
                    mcp.write().configs = servers;
                }
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::LoopStatus {
                        state,
                        remaining,
                        armed,
                        gate_enabled,
                        max_run_secs,
                        max_run_tokens,
                        max_run_usd,
                        max_idle_iterations,
                    },
                    ..
                } => {
                    let mut l = loop_ui.write();
                    l.state = state;
                    l.remaining = remaining;
                    l.armed = armed;
                    l.gate_enabled = gate_enabled;
                    l.max_run_secs = max_run_secs;
                    l.max_run_tokens = max_run_tokens;
                    l.max_run_usd = max_run_usd;
                    l.max_idle_iterations = max_idle_iterations;
                    l.loaded = true;
                }
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::LoopControl { ok, message },
                    ..
                } => {
                    loop_ui.write().last_control_result = Some((ok, message));
                }
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::Reminders { reminders },
                    ..
                } => {
                    let mut r = reminders_ui.write();
                    r.reminders = reminders;
                    r.loaded = true;
                }
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::McpServersApplied { servers, .. },
                    ..
                } => {
                    mcp.write().configs = servers;
                    mcp_config_ui.write().notice = Some((true, "Saved — restart the daemon to apply.".to_string()));
                }
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::McpServerTestResult { ok, tool_count, error },
                    ..
                } => {
                    let text = if ok {
                        format!("Connected — {tool_count} tool(s) found.")
                    } else {
                        error.unwrap_or_else(|| "connection failed".to_string())
                    };
                    mcp_config_ui.write().test_result = Some((ok, text));
                }
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::Gallery { available, images },
                    ..
                } => {
                    // Studio Gallery — recent ComfyUI generations.
                    let mut g = gallery.write();
                    g.available = available;
                    g.images = images;
                    g.loaded = true;
                }
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::GetToolCatalog { tools: entries },
                    ..
                } => {
                    // Chapter Almanac — the Tools screen's registry snapshot.
                    let mut t = tools.write();
                    t.tools = entries;
                    t.loaded = true;
                }
                DaemonEnvelope::SkillForgotten { ok, removed, name, .. } if ok && removed => {
                    // Chapter Repertoire — drop the forgotten skill locally.
                    skills.write().skills.retain(|s| s.skill.name != name);
                }
                DaemonEnvelope::SkillAuthored { ok, error, .. } => {
                    // Chapter Tutor — Studio "Teach a skill" form ack.
                    // Matches ScheduleMutated's own shape exactly: the
                    // form already cleared its local fields optimistically
                    // on submit (Step 6 below); this only ever updates the
                    // notice banner.
                    skills_ui.write().notice = Some(if ok {
                        (true, "Skill taught.".to_string())
                    } else {
                        (false, error.unwrap_or_else(|| "teach failed".into()))
                    });
                }
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::GetWikiPage { page },
                    ..
                } => {
                    wiki.write().selected = page;
                }
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::GetKnowledgeGraph { entities, edges },
                    ..
                } => {
                    let mut l = lattice.write();
                    l.entities = entities;
                    l.edges = edges;
                }
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::GetSettings { settings: snap },
                    ..
                } => {
                    // Feeds both the Settings screen and the Command Center's
                    // agent-vitals rail (same snapshot — model/provider/ctx/
                    // autonomy/access).
                    dashboard.write().settings = Some(snap.clone());
                    settings.write().snapshot = Some(snap);
                }
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::Schedules { schedules },
                    ..
                } => {
                    dashboard.write().schedules = schedules;
                }
                // POLISH_WAVES.md sub-project 7 plan 3 — the editable
                // reflection-schedule list, mirroring the notify-target
                // list's own two handlers.
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::GetReflectionScheduleConfigs { schedules },
                    ..
                } => {
                    dashboard.write().reflection_schedules = schedules;
                }
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::ReflectionScheduleConfigApplied { schedules, .. },
                    ..
                } => {
                    dashboard.write().reflection_schedules = schedules;
                    schedules_ui.write().notice = Some((true, "Saved — restart the daemon to apply.".to_string()));
                }
                // `/classic` retirement — the self-learning digest. `proposals`
                // and `persona_selection` are intentionally dropped here: this
                // panel surfaces the digest only, matching the legacy pane's
                // own primary content.
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::LearningInsights { digest, .. },
                    ..
                } => {
                    dashboard.write().learning = Some(digest);
                }
                // Chapter Chime — schedule mutation acks. The list itself
                // refreshes via the GetSchedules chase the sender fired
                // right behind the mutation (the Documents write→read
                // bridge pattern) plus the 5 s poll.
                // Chapter Herald — notify targets + history feed the
                // Notifications screen and the header bell's badge.
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::GetNotifyTargets { targets },
                    ..
                } => {
                    notifications.write().targets = targets;
                }
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::ListNotificationHistory { entries, total_len },
                    ..
                } => {
                    let mut n = notifications.write();
                    n.history = entries;
                    n.total_len = total_len;
                }
                // POLISH_WAVES.md sub-project 7 plan 2 — the editable
                // notify-target list feeding the Notifications screen's
                // "Configure targets" section, mirroring the MCP config
                // list's own two handlers.
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::GetNotifyTargetConfigs { targets },
                    ..
                } => {
                    notifications.write().configs = targets;
                }
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::NotifyTargetsApplied { targets, .. },
                    ..
                } => {
                    notifications.write().configs = targets;
                    notify_config_ui.write().notice = Some((true, "Saved — restart the daemon to apply.".to_string()));
                }
                // POLISH_WAVES.md sub-project 7 plan 2 (Task 7) — the 4
                // channel-adapter configs feeding the "Channel adapters"
                // section. Get* (on screen mount) just seeds the panel
                // silently; *ConfigApplied (after a Save) also sets the
                // same restart-required notice the notify-target form uses
                // (final-review finding #3 — Save used to give zero
                // visible confirmation).
                DaemonEnvelope::QueryResponse { payload: QueryResponsePayload::GetEmailConfig { config }, .. } => {
                    notifications.write().email = Some(config);
                }
                DaemonEnvelope::QueryResponse { payload: QueryResponsePayload::EmailConfigApplied { config, .. }, .. } => {
                    notifications.write().email = Some(config);
                    notify_config_ui.write().notice = Some((true, "Saved — restart the daemon to apply.".to_string()));
                }
                DaemonEnvelope::QueryResponse { payload: QueryResponsePayload::GetTelegramConfig { config }, .. } => {
                    notifications.write().telegram = Some(config);
                }
                DaemonEnvelope::QueryResponse { payload: QueryResponsePayload::TelegramConfigApplied { config, .. }, .. } => {
                    notifications.write().telegram = Some(config);
                    notify_config_ui.write().notice = Some((true, "Saved — restart the daemon to apply.".to_string()));
                }
                DaemonEnvelope::QueryResponse { payload: QueryResponsePayload::GetDiscordConfig { config }, .. } => {
                    notifications.write().discord = Some(config);
                }
                DaemonEnvelope::QueryResponse { payload: QueryResponsePayload::DiscordConfigApplied { config, .. }, .. } => {
                    notifications.write().discord = Some(config);
                    notify_config_ui.write().notice = Some((true, "Saved — restart the daemon to apply.".to_string()));
                }
                DaemonEnvelope::QueryResponse { payload: QueryResponsePayload::GetSlackConfig { config }, .. } => {
                    notifications.write().slack = Some(config);
                }
                DaemonEnvelope::QueryResponse { payload: QueryResponsePayload::SlackConfigApplied { config, .. }, .. } => {
                    notifications.write().slack = Some(config);
                    notify_config_ui.write().notice = Some((true, "Saved — restart the daemon to apply.".to_string()));
                }
                DaemonEnvelope::ScheduleMutated { ok, schedule_id, error, .. } => {
                    let mut ui = schedules_ui.write();
                    ui.confirm_delete = None;
                    ui.notice = Some(if ok {
                        (true, format!("Schedule {schedule_id} saved."))
                    } else {
                        (false, error.unwrap_or_else(|| "schedule change failed".into()))
                    });
                }
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::SettingsApplied { settings: snap, restart_required },
                    ..
                } => {
                    let mut s = settings.write();
                    s.snapshot = Some(snap);
                    s.restart_required = restart_required;
                    s.notice = Some((true, "Saved to aivyx-pa.toml.".to_string()));
                }
                // POLISH_WAVES.md sub-project 7 plan 3 — the 3
                // Settings-coverage sections. Get* (on screen mount)
                // seeds the panel silently; *ConfigApplied (after a
                // Save) also sets the restart-required notice, matching
                // the channel-adapter cards' own convention.
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::GetMemoryProfileConfig { config },
                    ..
                } => {
                    settings.write().memory_profile = Some(config);
                }
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::MemoryProfileConfigApplied { config, .. },
                    ..
                } => {
                    let mut s = settings.write();
                    s.memory_profile = Some(config);
                    s.restart_required = true;
                    s.notice = Some((true, "Saved — restart the daemon to apply.".to_string()));
                }
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::GetEmbeddingConfig { config },
                    ..
                } => {
                    settings.write().embedding = Some(config);
                }
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::EmbeddingConfigApplied { config, .. },
                    ..
                } => {
                    let mut s = settings.write();
                    s.embedding = Some(config);
                    s.restart_required = true;
                    s.notice = Some((true, "Saved — restart the daemon to apply.".to_string()));
                }
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::GetProactiveConfig { config },
                    ..
                } => {
                    settings.write().proactive = Some(config);
                }
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::ProactiveConfigApplied { config, .. },
                    ..
                } => {
                    let mut s = settings.write();
                    s.proactive = Some(config);
                    s.restart_required = true;
                    s.notice = Some((true, "Saved — restart the daemon to apply.".to_string()));
                }
                // Chapter Voice — the [voice] config + readiness.
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::GetVoiceSettings { settings: snap },
                    ..
                } => {
                    voice.write().snapshot = Some(snap);
                }
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::VoiceApplied { settings: snap, restart_required },
                    ..
                } => {
                    let mut v = voice.write();
                    v.snapshot = Some(snap);
                    v.restart_required = restart_required;
                    v.notice = Some((true, "Saved to aivyx-pa.toml.".to_string()));
                }
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::ProfileApplied { profile, restart_required },
                    ..
                } => {
                    let mut a = agents.write();
                    a.profile = Some(profile);
                    a.restart_required = restart_required;
                    a.notice = Some((true, "Saved to aivyx-pa.toml.".to_string()));
                }
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::GetEffectivePersona { persona },
                    ..
                } => {
                    agents.write().persona = Some(persona);
                }
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::ListPersonaProposals { proposals, .. },
                    ..
                } => {
                    agents.write().proposals = proposals;
                }
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::ListPersonaDeltas { mut entries, .. },
                    ..
                } => {
                    // Newest first for the change-history view.
                    entries.reverse();
                    agents.write().deltas = entries;
                }
                // Persona governance acks (live — the daemon has already
                // recomputed runtime state). Bump the refresh tick so the panel
                // re-queries proposals + deltas + the folded persona.
                DaemonEnvelope::PersonaProposalResolved { ok, success, error, .. } => {
                    let mut a = agents.write();
                    a.refresh_tick += 1;
                    a.notice = Some(if ok {
                        let status = success
                            .map(|s| s.proposal_status.to_lowercase())
                            .unwrap_or_else(|| "resolved".to_string());
                        (true, format!("Proposal {status} — effective next turn."))
                    } else {
                        (false, error.unwrap_or_else(|| "resolve failed".to_string()))
                    });
                }
                DaemonEnvelope::PersonaRevertResolved { ok, error, .. } => {
                    let mut a = agents.write();
                    a.refresh_tick += 1;
                    a.notice = Some(if ok {
                        (true, "Delta reverted — effective next turn.".to_string())
                    } else {
                        (false, error.unwrap_or_else(|| "revert failed".to_string()))
                    });
                }
                // X.3 — LLM seed draft arrived (or failed). The onboarding card
                // watches `seed_draft_resp` to clear its spinner + re-seed its
                // form from `seed_draft`.
                // Chapter Nonagon Templates — role-tailored roster draft
                // arrived (or failed). The Teams panel's effect watches
                // draft_resp to pick up drafted_roster into its local edit
                // state exactly once per response.
                DaemonEnvelope::TeamTemplateDrafted { draft, error, .. } => {
                    let mut t = teams.write();
                    t.drafting = false;
                    t.draft_resp = t.draft_resp.wrapping_add(1);
                    if draft.is_some() {
                        t.drafted_roster = draft;
                        t.draft_notice = None;
                    } else {
                        t.drafted_roster = None;
                        t.draft_notice = Some((
                            false,
                            error.unwrap_or_else(|| "couldn't draft a team".to_string()),
                        ));
                    }
                }
                DaemonEnvelope::PersonaSeedDrafted { draft, error, .. } => {
                    let mut a = agents.write();
                    a.seed_draft_resp += 1;
                    let had_draft = draft.is_some();
                    a.seed_draft = draft;
                    if !had_draft {
                        a.notice = Some((
                            false,
                            error.unwrap_or_else(|| "couldn't draft a seed".to_string()),
                        ));
                    }
                }
                // GE.3 — LLM Profile draft arrived (or failed). The onboarding
                // flow's step 1 watches `profile_draft_resp` to clear its
                // spinner + fill its six fields from `profile_draft`.
                DaemonEnvelope::ProfileDrafted { draft, error, .. } => {
                    let mut a = agents.write();
                    a.profile_draft_resp += 1;
                    let had_draft = draft.is_some();
                    a.profile_draft = draft;
                    if !had_draft {
                        a.notice = Some((
                            false,
                            error.unwrap_or_else(|| "couldn't draft a profile".to_string()),
                        ));
                    }
                }
                // X.3 — live seed planted (or refused). On success bump the
                // refresh tick so the panel re-queries and the card gives way to
                // the normal governance view.
                DaemonEnvelope::PersonaSeedResolved { ok, appended, error, .. } => {
                    let mut a = agents.write();
                    a.notice = Some(if ok {
                        a.refresh_tick += 1;
                        (true, format!("Seeded {appended} trait(s) — effective next turn."))
                    } else {
                        (false, error.unwrap_or_else(|| "seeding failed".to_string()))
                    });
                }
                // Route a Settings write/read failure to its panel (the query
                // ids are prefixed so other QueryErrors don't hijack the banner).
                DaemonEnvelope::QueryResponse {
                    id,
                    payload: QueryResponsePayload::QueryError { message, .. },
                } if id.starts_with("mc-settings") => {
                    settings.write().notice = Some((false, message));
                }
                // Same routing for a Profile write/read failure on the Agents
                // screen (ids prefixed `mc-agents`).
                DaemonEnvelope::QueryResponse {
                    id,
                    payload: QueryResponsePayload::QueryError { message, .. },
                } if id.starts_with("mc-agents") => {
                    agents.write().notice = Some((false, message));
                }
                // And a [voice] write/read failure (ids prefixed `mc-voice`).
                DaemonEnvelope::QueryResponse {
                    id,
                    payload: QueryResponsePayload::QueryError { message, .. },
                } if id.starts_with("mc-voice") => {
                    voice.write().notice = Some((false, message));
                }
                // Chapter Roster (RO.3) — a team save/read failure (e.g. the
                // server-side `TeamConfig::validate` rejected the roster). Ids
                // are prefixed `mc-teams` so it lands on the Teams banner.
                DaemonEnvelope::QueryResponse {
                    id,
                    payload: QueryResponsePayload::QueryError { message, .. },
                } if id.starts_with("mc-teams") => {
                    teams.write().notice = Some((false, message));
                }
                // Chapter Lantern — an MCP server config save/delete failure
                // (e.g. `"stdio transport requires command"`). Ids are
                // prefixed `mc-mcp` (covers both `mc-mcp-set` and
                // `mc-mcp-delete`) so it lands on the MCP config banner.
                DaemonEnvelope::QueryResponse {
                    id,
                    payload: QueryResponsePayload::QueryError { message, .. },
                } if id.starts_with("mc-mcp") => {
                    mcp_config_ui.write().notice = Some((false, message));
                }
                // POLISH_WAVES.md sub-project 7 plan 2 — a notify-target
                // config save/delete failure. Ids are prefixed `mc-notify`
                // so it lands on the notify-target config banner (also
                // covers the pre-existing `mc-notify-targets`/`mc-notify-
                // history` poll ids, which are query-error-safe in
                // practice — same tradeoff `mc-mcp` already makes for
                // `mc-mcp-status`).
                DaemonEnvelope::QueryResponse {
                    id,
                    payload: QueryResponsePayload::QueryError { message, .. },
                } if id.starts_with("mc-notify") => {
                    notify_config_ui.write().notice = Some((false, message));
                }
                // POLISH_WAVES.md sub-project 7 plan 3 — a reflection-
                // schedule config save/delete failure. Ids are prefixed
                // `mc-reflection` so it lands on the Schedules screen's
                // banner (shared with the regular-schedule mutations'
                // own `ScheduleMutated` notice).
                DaemonEnvelope::QueryResponse {
                    id,
                    payload: QueryResponsePayload::QueryError { message, .. },
                } if id.starts_with("mc-reflection") => {
                    schedules_ui.write().notice = Some((false, message));
                }
                // Chapter Mission Control — an abort/pause/resume rejected by
                // the daemon (e.g. resume on a mission that isn't paused).
                // Ids are prefixed `mc-abort`/`mc-pause`/`mc-resume` so it
                // lands on the Mission Control banner, not silently dropped.
                DaemonEnvelope::QueryResponse {
                    id,
                    payload: QueryResponsePayload::QueryError { message, .. },
                } if id.starts_with("mc-abort") || id.starts_with("mc-pause") || id.starts_with("mc-resume") => {
                    mission_ui.write().notice = Some((false, message));
                }
                // Chapter Mission Control — the abort/pause requests
                // succeeded (they always do once the daemon accepts them;
                // the mission itself halts/pauses asynchronously at its
                // next wave boundary, reflected later via the existing
                // `TeamMissionUpdated` broadcast, not here).
                DaemonEnvelope::QueryResponse {
                    payload:
                        QueryResponsePayload::TeamMissionAborted { message, .. }
                        | QueryResponsePayload::TeamMissionPaused { message, .. },
                    ..
                } => {
                    mission_ui.write().notice = Some((true, message));
                }
                // Chapter Mission Control — the resume succeeded; the
                // mission moves back to `Executing` and drives in the
                // background (again reflected via `TeamMissionUpdated`).
                DaemonEnvelope::QueryResponse {
                    payload: QueryResponsePayload::TeamMissionResumed { phase, .. },
                    ..
                } => {
                    mission_ui.write().notice = Some((true, format!("Resumed — now {}.", phase_label(phase))));
                }
                DaemonEnvelope::StreamEvent { event, .. } => match event {
                    StreamEventPayload::Text { text } => streaming.write().push_str(&text),
                    StreamEventPayload::Status { status } => {
                        transcript.write().push(ChatLine::system(format!("· {status}")));
                    }
                    StreamEventPayload::ToolCallStarted { tool_name, input, .. } => {
                        // POLISH_WAVES.md sub-project 4, item D — parity
                        // with mission/cron turns, which already journal
                        // full args via StreamEventPayload::render_for_cli
                        // (`→ {tool_name} {input}`); chat previously
                        // dropped `input` here, so distinct calls with
                        // different arguments looked like stuck repetition.
                        let input_oneline = serde_json::to_string(&input).unwrap_or_default();
                        transcript.write().push(ChatLine::system(format!("→ {tool_name} {input_oneline}")));
                    }
                    StreamEventPayload::ApprovalGate { mission_id, gate_id, reason, .. } => {
                        gate.set(Some(GateInfo { mission_id, gate_id, reason }));
                    }
                    _ => {}
                },
                DaemonEnvelope::TurnComplete { outcome, .. } => {
                    let text = streaming();
                    if !text.is_empty() {
                        transcript.write().push(ChatLine::assistant(text.clone()));
                    }
                    if let Some(note) = turn_outcome_correction(&text, &outcome) {
                        transcript.write().push(ChatLine::system(note));
                    }
                    streaming.set(String::new());
                }
                DaemonEnvelope::Error { message, .. } => {
                    transcript.write().push(ChatLine::error(message));
                }
                DaemonEnvelope::ServerInfo { boot_id } => {
                    let current = server_info();
                    server_info.set(apply_server_info(&current, boot_id));
                }
                _ => {}
            }
        }
        // Socket died: flip the banner on immediately and clear the chat
        // session — the ws bridge mints a fresh one on reconnect.
        connected.set(false);
        session.set(None);
    }
}

/// Build the `ws[s]://<host>/ws` URL from the page's location.
fn ws_url() -> String {
    let location = web_sys::window().expect("window").location();
    let scheme = match location.protocol().as_deref() {
        Ok("https:") => "wss",
        _ => "ws",
    };
    let host = location.host().unwrap_or_else(|_| "127.0.0.1".to_string());
    format!("{scheme}://{host}/ws")
}
