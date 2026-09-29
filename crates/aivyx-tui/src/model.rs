//! The pure TUI model — Phase 185 Task 2.
//!
//! This module is the **testable core** of the terminal UI: an
//! [`AppState`] plus an [`update`] reducer (`(AppState, Msg) ->
//! AppState`) and the [`lines_from_event`] mapping that turns daemon
//! [`StreamEventPayload`]s into chat lines. It is deliberately free of
//! any `ratatui` / `crossterm` types — the terminal driver (Task 3)
//! translates key events into [`Msg`]s and renders [`AppState`], but
//! the *logic* lives here where it can be unit-tested without a
//! terminal (the same operator-verification split the phase doc calls
//! for: pure model tested in CI, visual rendering verified on the
//! operator's host).
//!
//! The daemon is the agent; this is just a render + interaction layer
//! over the IPC stream — no capability, trust, or audit concern.

use aivyx_channel::daemon_ipc::{
    concat_text_events, turn_outcome_correction, AuditEntrySummary, StreamEventPayload,
};
use aivyx_channel::team_mission::{TeamMissionPhase, TeamMissionView, TeamStepState};

/// The provenance of a rendered chat line. The terminal driver maps
/// each kind to a style; the pure model only tags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    /// Something the operator typed and submitted.
    Operator,
    /// Agent (assistant) text from a `Text` event.
    Agent,
    /// A tool-call breadcrumb (started / finished / output).
    Tool,
    /// A daemon status line (`⋯ working…`-style).
    Status,
    /// An approval-gate announcement.
    Gate,
    /// A locally-generated note (errors, cancellation, connection).
    System,
}

/// One rendered line in the scrollback. `text` is already a single
/// display line (no embedded newlines) — multi-line events are split
/// into several `ChatLine`s by [`lines_from_event`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatLine {
    pub kind: LineKind,
    pub text: String,
}

impl ChatLine {
    fn new(kind: LineKind, text: impl Into<String>) -> Self {
        Self {
            kind,
            text: text.into(),
        }
    }
}

/// An approval gate awaiting an in-TUI approve/reject decision. While
/// `Some`, the driver renders an approve/reject prompt; resolving it
/// (via [`Msg::GateResolved`]) clears it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingGate {
    pub mission_id: String,
    pub gate_id: String,
    pub reason: String,
    pub scope: Option<String>,
}

/// The status bar's model: who we're talking as, whether the daemon
/// is connected, and whether a turn is in flight.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Status {
    pub role: Option<String>,
    pub daemon_connected: bool,
    /// A turn was submitted and we're awaiting its result — the
    /// "working…" state. Input submission is suppressed while true.
    pub working: bool,
}

/// A top-level view in the TUI. `Chat` is the shipped interactive
/// surface; the others are read-only panels. Missions/Audit/Tools were
/// already live-data-wired by Phase 185 and POLISH_WAVES.md sub-project
/// 8 — Dashboard was Phase 186's one remaining stub. The order is the
/// tab order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum View {
    #[default]
    Chat,
    /// The Nonagon Missions/Fleet panel — a running team's mission DAG
    /// (Chapter J.7). Live-fed by [`Msg::MissionsUpdated`].
    Missions,
    Dashboard,
    Audit,
    Tools,
}

impl View {
    /// Every view, in tab order.
    pub const ALL: [View; 5] =
        [View::Chat, View::Missions, View::Dashboard, View::Audit, View::Tools];

    /// The tab label.
    pub fn label(self) -> &'static str {
        match self {
            View::Chat => "Chat",
            View::Missions => "Missions",
            View::Dashboard => "Dashboard",
            View::Audit => "Audit",
            View::Tools => "Tools",
        }
    }

    fn index(self) -> usize {
        View::ALL.iter().position(|v| *v == self).unwrap_or(0)
    }

    /// The next view (wraps).
    pub fn next(self) -> View {
        View::ALL[(self.index() + 1) % View::ALL.len()]
    }

    /// The previous view (wraps).
    pub fn prev(self) -> View {
        View::ALL[(self.index() + View::ALL.len() - 1) % View::ALL.len()]
    }
}

/// A running mission's lifecycle phase, as shown in the Missions panel.
/// A view-model — the daemon/driver maps a team's `MissionStatus` +
/// live progress onto these; the TUI stays free of `aivyx-team` types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissionPhase {
    /// Decomposed, not yet running.
    Planning,
    /// At least one step is running.
    Executing,
    /// Blocked on an operator approval gate.
    AwaitingApproval,
    /// Chapter Mission Control — paused at a wave boundary by an explicit
    /// operator pause request. Non-terminal; resumable.
    Paused,
    /// Every step completed.
    Done,
    /// A quality gate rejected the work (`MissionStatus::GateRejected`).
    Rejected,
    /// Chapter Ballast — halted by a per-mission budget cap.
    Halted,
}

impl MissionPhase {
    pub fn label(self) -> &'static str {
        match self {
            MissionPhase::Planning => "planning",
            MissionPhase::Executing => "executing",
            MissionPhase::AwaitingApproval => "approval",
            MissionPhase::Paused => "paused",
            MissionPhase::Done => "done",
            MissionPhase::Rejected => "rejected",
            MissionPhase::Halted => "halted",
        }
    }
}

/// A mission DAG step's state, mirroring how the [`crate`]'s runtime
/// walks a plan (ready → running → done, or gated/failed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepState {
    Pending,
    Running,
    Done,
    /// A gate step still awaiting its verdict.
    Gated,
    Failed,
}

impl StepState {
    /// The timeline dot the renderer shows for this state.
    pub fn dot(self) -> &'static str {
        match self {
            StepState::Pending => "○",
            StepState::Running => "◐",
            StepState::Done => "●",
            StepState::Gated => "⚑",
            StepState::Failed => "✗",
        }
    }
}

/// One step in a mission's timeline.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissionStep {
    /// e.g. `"stocktake — count closing stock"`.
    pub label: String,
    pub state: StepState,
}

/// One mission in the panel: a team lead running a DAG, with a step
/// timeline and a rolled-up phase + progress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissionRow {
    pub id: String,
    pub title: String,
    /// The team lead running it (e.g. `coordinator`, `aria`).
    pub lead: String,
    pub phase: MissionPhase,
    /// Completion percent in `0..=100`.
    pub progress: u16,
    pub steps: Vec<MissionStep>,
    /// Chapter L.6 — the step id awaiting an operator decision (set iff
    /// `phase == AwaitingApproval`); what the panel's approve/reject keys
    /// target via `ResolveTeamGate`.
    pub pending_gate: Option<String>,
}

/// The Missions panel's model: the rows + which is selected for detail.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MissionsState {
    pub rows: Vec<MissionRow>,
    /// Index into `rows`; kept in range by the reducer.
    pub selected: usize,
}

impl MissionsState {
    /// The selected mission, if any.
    pub fn selected_row(&self) -> Option<&MissionRow> {
        self.rows.get(self.selected)
    }

    /// Keep `selected` a valid index (0 when empty).
    fn clamp(&mut self) {
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
    }
}

/// Chapter L.6 — map the daemon's neutral mission views (polled via
/// `team_mission_list` then `TeamMissionRecord::to_view`) onto the panel's
/// [`MissionRow`]s. The TUI's single point of contact with the team feed,
/// keeping the rest of the model free of `aivyx-team` engine types.
pub fn mission_rows_from_views(views: Vec<TeamMissionView>) -> Vec<MissionRow> {
    views.into_iter().map(mission_row_from_view).collect()
}

fn mission_row_from_view(v: TeamMissionView) -> MissionRow {
    let steps = v
        .steps
        .into_iter()
        .map(|s| MissionStep {
            label: s.label,
            state: step_state_from(s.state),
        })
        .collect();
    MissionRow {
        id: v.id,
        title: v.goal,
        // The team lead (the pack's lead, or `coordinator` for the default
        // Nonagon) — the channel-side projection resolved it from the record.
        lead: v.lead,
        phase: phase_from(v.phase),
        progress: v.progress,
        steps,
        pending_gate: v.pending_gate,
    }
}

fn phase_from(p: TeamMissionPhase) -> MissionPhase {
    match p {
        TeamMissionPhase::Planning => MissionPhase::Planning,
        TeamMissionPhase::Executing => MissionPhase::Executing,
        TeamMissionPhase::AwaitingApproval => MissionPhase::AwaitingApproval,
        TeamMissionPhase::Paused => MissionPhase::Paused,
        TeamMissionPhase::Done => MissionPhase::Done,
        TeamMissionPhase::Rejected => MissionPhase::Rejected,
        TeamMissionPhase::Halted => MissionPhase::Halted,
    }
}

fn step_state_from(s: TeamStepState) -> StepState {
    match s {
        TeamStepState::Pending => StepState::Pending,
        TeamStepState::Running => StepState::Running,
        TeamStepState::Done => StepState::Done,
        TeamStepState::Awaiting => StepState::Gated,
        TeamStepState::Rejected => StepState::Failed,
    }
}

/// The complete UI state. Owned, cloneable, and free of terminal
/// types so the reducer is pure.
///
/// `Eq` was dropped from this derive when the Audit view's
/// `audit_entries: Vec<AuditEntrySummary>` field was added
/// (`/classic` retirement, Task E): `AuditEntrySummary` carries a
/// `serde_json::Value` body and only derives `PartialEq`, not `Eq`, so
/// this struct can no longer either. Nothing in this crate compares
/// `AppState` for `Eq` (no `HashSet<AppState>` etc.) — every existing
/// use is `assert_eq!`, which only needs `PartialEq` + `Debug`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct AppState {
    /// The active top-level view (Chat by default).
    pub view: View,
    /// Scrollback, oldest first.
    pub history: Vec<ChatLine>,
    /// The current input buffer.
    pub input: String,
    /// Cursor position within `input`, as a **char** index in
    /// `[0, input.chars().count()]`.
    pub cursor: usize,
    /// Lines scrolled up from the bottom. `0` == pinned to the latest
    /// line. Bounded in `[0, history.len()]` by the reducer; the
    /// render layer further clamps to the viewport height.
    pub scroll: usize,
    pub status: Status,
    /// The Nonagon Missions panel's state (Chapter J.7).
    pub missions: MissionsState,
    /// A pending approval gate, if any.
    pub gate: Option<PendingGate>,
    /// Chapter L — the in-progress "new mission" goal the operator is typing
    /// in the Missions panel (`Some` ⇒ the compose box is open and captures
    /// input). `None` ⇒ closed.
    pub mission_compose: Option<String>,
    /// Chapter L — set while a submitted goal is being decomposed + started on
    /// the daemon (the LLM planning call); the panel shows a "starting…"
    /// indicator and input is inert until it resolves.
    pub mission_starting: bool,
    /// Set once the operator asks to quit; the driver's event loop
    /// observes this and tears down the terminal.
    pub should_quit: bool,
    /// `/classic` retirement — the Audit view's current page.
    pub audit_entries: Vec<AuditEntrySummary>,
    pub audit_total: u64,
    /// POLISH_WAVES.md sub-project 8 item B — the Tools view's data,
    /// fetched once on switching into the view (no background poll,
    /// same posture as `audit_entries` before pagination).
    pub tool_stats: Vec<aivyx_channel::daemon_ipc::ToolStat>,
    /// Phase 186 — the Dashboard's loop-status panel. `None` until the
    /// first fetch (switching onto Dashboard) resolves; re-fetched on
    /// the Missions poll tick while Dashboard stays the active view (see
    /// `app.rs`'s `run_loop`) so a running loop's iteration count is
    /// visibly live, not a one-shot snapshot like Audit/Tools.
    pub loop_status: Option<LoopStatusView>,
    /// Phase 186 — the Dashboard's reminders panel, soonest-due-first
    /// (matches `ReminderStore::list`'s own order). Same fetch posture
    /// as `loop_status`: `None` until the first fetch (switching onto
    /// Dashboard) resolves, or if the last fetch attempt errored —
    /// distinct from `Some(vec![])`, which means a fetch succeeded and
    /// the daemon genuinely has no pending reminders. Collapsing these
    /// two states into a bare `Vec` was a final-review finding (both
    /// rendered as "none pending", so an operator couldn't tell a dead
    /// daemon connection from an empty reminder list) — keep them
    /// distinct.
    pub reminders: Option<Vec<aivyx_channel::daemon_ipc::ReminderView>>,
}

/// Phase 186 — a named-field wrapper around `daemon_client::
/// loop_status`'s 8-tuple response, so the Dashboard's render code
/// doesn't index into a tuple.
#[derive(Debug, Clone, PartialEq)]
pub struct LoopStatusView {
    pub state: aivyx_channel::loop_driver::LoopRunState,
    pub remaining: usize,
    pub armed: bool,
    pub gate_enabled: bool,
    pub max_run_secs: Option<u64>,
    pub max_run_tokens: Option<u64>,
    pub max_run_usd: Option<f64>,
    pub max_idle_iterations: u32,
}

/// A message into the reducer. Key events become editing / scroll /
/// submit messages; daemon round-trips become result messages.
#[derive(Debug, Clone, PartialEq)]
pub enum Msg {
    // ---- input editing ----
    /// Insert a character at the cursor.
    InsertChar(char),
    /// Delete the character before the cursor.
    Backspace,
    /// Delete the character at the cursor.
    Delete,
    /// Move the cursor one char left.
    CursorLeft,
    /// Move the cursor one char right.
    CursorRight,
    /// Move the cursor to the start of the input.
    CursorHome,
    /// Move the cursor to the end of the input.
    CursorEnd,

    // ---- submission ----
    /// Submit the current input as a turn. A no-op while a turn is in
    /// flight or when the input is blank. On success the input is
    /// echoed as an [`LineKind::Operator`] line, the buffer cleared,
    /// and [`Status::working`] set.
    Submit,

    // ---- view navigation ----
    /// Switch to the next view (wraps).
    NextView,
    /// Switch to the previous view (wraps).
    PrevView,
    /// Jump directly to a view.
    SwitchView(View),

    // ---- Missions panel (Chapter J.7) ----
    /// Replace the missions snapshot — the driver pushes this from the
    /// running team's mission state (selection is re-clamped in range).
    MissionsUpdated(Vec<MissionRow>),
    /// Move the Missions selection to the next / previous mission (clamped).
    MissionSelectNext,
    MissionSelectPrev,
    /// Chapter L — open the "new mission" compose box (empty goal).
    MissionComposeOpen,
    /// Chapter L — close the compose box without starting a mission.
    MissionComposeCancel,
    /// Chapter L — append a typed char to the in-progress goal.
    MissionComposeChar(char),
    /// Chapter L — delete the last char of the in-progress goal.
    MissionComposeBackspace,

    // ---- scrolling ----
    /// Scroll up (toward older lines) by `n` lines.
    ScrollUp(usize),
    /// Scroll down (toward newer lines) by `n` lines.
    ScrollDown(usize),
    /// Pin back to the latest line.
    ScrollToBottom,

    // ---- daemon round-trips ----
    /// A turn finished: append its events as chat lines, clear
    /// `working`, and pick up any approval gate it surfaced.
    TurnFinished {
        events: Vec<StreamEventPayload>,
        outcome: String,
    },
    /// An approval gate was resolved in-TUI.
    GateResolved { approved: bool },
    /// The in-flight turn was cancelled.
    Cancelled,
    /// An error occurred talking to the daemon.
    Error(String),

    // ---- lifecycle ----
    /// The session connected; records the role for the status bar.
    Connected { role: Option<String> },
    /// Request to quit the application.
    Quit,

    // ---- Audit view (`/classic` retirement, Task E) ----
    /// A fresh page of audit entries arrived; replaces the current page +
    /// total wholesale (this is a paginated view, not an append-only feed
    /// like Missions).
    AuditUpdated {
        entries: Vec<AuditEntrySummary>,
        total_len: u64,
    },
    /// POLISH_WAVES.md sub-project 8 item B — a fresh `GetToolStats`
    /// snapshot, pushed on switching into `View::Tools`.
    ToolStatsUpdated(Vec<aivyx_channel::daemon_ipc::ToolStat>),
    /// Phase 186 — a fresh loop-status fetch resolved.
    LoopStatusUpdated(LoopStatusView),
    /// Phase 186 — a fresh reminders fetch resolved.
    RemindersUpdated(Vec<aivyx_channel::daemon_ipc::ReminderView>),
}

impl AppState {
    /// A fresh state with the input cursor at 0.
    pub fn new() -> Self {
        Self::default()
    }

    /// The number of chars in the input buffer.
    fn input_char_len(&self) -> usize {
        self.input.chars().count()
    }

    /// Byte offset in `input` for char index `cursor`.
    fn cursor_byte(&self) -> usize {
        self.input
            .char_indices()
            .nth(self.cursor)
            .map(|(b, _)| b)
            .unwrap_or(self.input.len())
    }

    /// The text the operator would submit, trimmed — `None` if blank.
    /// The driver uses this to decide whether to spawn a turn.
    pub fn submittable(&self) -> Option<String> {
        let trimmed = self.input.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    }

    /// Push a chat line and re-pin to the bottom so new content is
    /// always visible.
    fn push_line(&mut self, line: ChatLine) {
        self.history.push(line);
        self.scroll = 0;
    }
}

/// The `Msg::ScrollUp` clamp ceiling for [`View::Dashboard`] — the
/// exact total line count `render.rs`'s `dashboard_lines` produces for
/// this state, so `audit_scroll_offset`'s pin-to-top math (reused for
/// Dashboard in `render_panel`) can always reach the panel's true
/// bottom row. Hand-derived from `dashboard_lines`' fixed structure:
///
/// - header block (role/daemon/status/session/blank): 5 fixed lines
/// - LOOP (header/one status line/blank): 3 fixed lines
/// - REMINDERS (header/blank, fixed) + content: 2 fixed + 1 line if
///   empty, else 1 "N pending" line + up to 3 reminder rows
/// - MISSIONS (header/one summary line/blank): 3 fixed lines — the
///   summary line is always exactly 1 line whether rows are empty or not
/// - AUDIT (header/total line, fixed) + up to 3 recent-entry rows, no
///   trailing blank (it's the last section)
///
/// 5 + 3 + 2 + 3 + 2 = 15 fixed lines, plus the two variable pieces
/// below. Cross-checked against `dashboard_lines`' real output length
/// by `render.rs`'s `dashboard_scroll_max_matches_dashboard_lines`
/// test — this is what keeps this formula from silently drifting out
/// of sync with `dashboard_lines`, the exact bug class POLISH_WAVES.md
/// sub-project 8 found in the Tools view's own scroll clamp.
pub(crate) fn dashboard_scroll_max(state: &AppState) -> usize {
    let reminder_lines = match &state.reminders {
        None => 1,
        Some(reminders) if reminders.is_empty() => 1,
        Some(reminders) => 1 + reminders.len().min(3),
    };
    let audit_lines = state.audit_entries.len().min(3);
    15 + reminder_lines + audit_lines
}

/// The reducer. Pure: `(state, msg) -> state`. Owns the editing,
/// submission, scroll, and daemon-result transitions.
pub fn update(mut state: AppState, msg: Msg) -> AppState {
    match msg {
        Msg::InsertChar(c) => {
            let at = state.cursor_byte();
            state.input.insert(at, c);
            state.cursor += 1;
        }
        Msg::Backspace => {
            if state.cursor > 0 {
                state.cursor -= 1;
                let at = state.cursor_byte();
                state.input.remove(at);
            }
        }
        Msg::Delete => {
            if state.cursor < state.input_char_len() {
                let at = state.cursor_byte();
                state.input.remove(at);
            }
        }
        Msg::CursorLeft => {
            state.cursor = state.cursor.saturating_sub(1);
        }
        Msg::CursorRight => {
            if state.cursor < state.input_char_len() {
                state.cursor += 1;
            }
        }
        Msg::CursorHome => state.cursor = 0,
        Msg::CursorEnd => state.cursor = state.input_char_len(),

        Msg::NextView => state.view = state.view.next(),
        Msg::PrevView => state.view = state.view.prev(),
        Msg::SwitchView(v) => state.view = v,

        Msg::MissionsUpdated(rows) => {
            state.missions.rows = rows;
            state.missions.clamp();
        }
        Msg::MissionSelectNext => {
            let last = state.missions.rows.len().saturating_sub(1);
            state.missions.selected = (state.missions.selected + 1).min(last);
        }
        Msg::MissionSelectPrev => {
            state.missions.selected = state.missions.selected.saturating_sub(1);
        }
        Msg::MissionComposeOpen => state.mission_compose = Some(String::new()),
        Msg::MissionComposeCancel => state.mission_compose = None,
        Msg::MissionComposeChar(c) => {
            if let Some(goal) = state.mission_compose.as_mut() {
                goal.push(c);
            }
        }
        Msg::MissionComposeBackspace => {
            if let Some(goal) = state.mission_compose.as_mut() {
                goal.pop();
            }
        }

        Msg::Submit => {
            if state.status.working {
                return state; // ignore while a turn is in flight
            }
            if let Some(text) = state.submittable() {
                state.push_line(ChatLine::new(LineKind::Operator, text));
                state.input.clear();
                state.cursor = 0;
                state.status.working = true;
            }
        }

        Msg::ScrollUp(n) => {
            // The scroll counter is shared across views (re-pinned to 0 on
            // `SwitchView`-independent events like new chat content, same
            // as before); its ceiling must match whichever view is
            // currently reading it, or a view with a shorter backing
            // list than the chat history (e.g. Audit, page-capped at 50
            // entries) clamps scroll to a value too small to ever reach
            // its own top row.
            let max = match state.view {
                View::Audit => state.audit_entries.len(),
                View::Tools => {
                    1 + state.tool_stats.len()
                        + state.tool_stats.iter().filter(|t| t.calls > 0).count()
                }
                View::Dashboard => dashboard_scroll_max(&state),
                _ => state.history.len(),
            };
            state.scroll = (state.scroll + n).min(max);
        }
        Msg::ScrollDown(n) => {
            state.scroll = state.scroll.saturating_sub(n);
        }
        Msg::ScrollToBottom => state.scroll = 0,

        Msg::TurnFinished { events, outcome } => {
            // Turn-outcome-correction follow-up (POLISH_WAVES.md
            // sub-project 4) — compute what these events would display
            // BEFORE the coalescing loop below consumes `events` by
            // value, so it can be compared against the turn's own
            // authoritative outcome.
            let displayed = concat_text_events(&events);

            // Coalesce consecutive Text events first: the daemon
            // streams token-level chunks ("Hi", " there", "!"), and
            // rendering each as its own ChatLine put one word per
            // line (Vitrine §12).
            let mut merged: Vec<StreamEventPayload> = Vec::with_capacity(events.len());
            for event in events {
                if let StreamEventPayload::Text { text } = &event {
                    if let Some(StreamEventPayload::Text { text: prev }) = merged.last_mut() {
                        prev.push_str(text);
                        continue;
                    }
                }
                merged.push(event);
            }
            for event in &merged {
                for line in lines_from_event(event) {
                    state.push_line(line);
                }
                if let StreamEventPayload::ApprovalGate {
                    mission_id,
                    gate_id,
                    reason,
                    scope,
                } = event
                {
                    state.gate = Some(PendingGate {
                        mission_id: mission_id.clone(),
                        gate_id: gate_id.clone(),
                        reason: reason.clone(),
                        scope: scope.clone(),
                    });
                }
            }
            if let Some(note) = turn_outcome_correction(&displayed, &outcome) {
                state.push_line(ChatLine::new(LineKind::System, note));
            }
            state.status.working = false;
        }

        Msg::GateResolved { approved } => {
            let verdict = if approved { "approved" } else { "rejected" };
            state.push_line(ChatLine::new(
                LineKind::System,
                format!("Gate {verdict}."),
            ));
            state.gate = None;
        }

        Msg::Cancelled => {
            state.status.working = false;
            state.push_line(ChatLine::new(LineKind::System, "Turn cancelled."));
        }

        Msg::Error(e) => {
            state.status.working = false;
            state.push_line(ChatLine::new(LineKind::System, format!("Error: {e}")));
        }

        Msg::Connected { role } => {
            state.status.daemon_connected = true;
            state.status.role = role;
        }

        Msg::Quit => state.should_quit = true,

        Msg::AuditUpdated { entries, total_len } => {
            state.audit_entries = entries;
            state.audit_total = total_len;
        }

        Msg::ToolStatsUpdated(tools) => {
            state.tool_stats = tools;
        }

        Msg::LoopStatusUpdated(view) => {
            state.loop_status = Some(view);
        }

        Msg::RemindersUpdated(reminders) => {
            state.reminders = Some(reminders);
        }
    }
    state
}

/// `/classic` retirement (Task E) — the pure pagination-window
/// arithmetic behind the Audit view's Left/Right keys.
///
/// `current_from_seq` is where the window *currently on screen*
/// starts — the smallest `seq` among `state.audit_entries`, or `0`
/// before any page has loaded. `total_len` is the chain's total entry
/// count as of the last fetch; `page_size` is the fixed page length
/// (`AUDIT_PAGE_SIZE` in `app.rs`).
///
/// Moves the window by one page in the requested direction, clamped so
/// `from_seq` never goes below `0` (backward) and never pages past the
/// newest full window, `total_len.saturating_sub(page_size)` (forward)
/// — matching the vetted arithmetic behind the web Audit screen's own
/// Older/Newer buttons (Task 2's `AuditPanel`), which needed two
/// rounds of fixing exactly this class of off-by-one before it was
/// right. The naive formula this plan's own brief first proposed for
/// this task ignored `current_from_seq` entirely (deriving the next
/// window from `total_len` alone), which cannot page backward more
/// than once — this function is the corrected replacement.
pub fn audit_page_from_seq(
    current_from_seq: u64,
    total_len: u64,
    page_size: u64,
    forward: bool,
) -> u64 {
    if forward {
        (current_from_seq + page_size).min(audit_newest_from_seq(total_len, page_size))
    } else {
        current_from_seq.saturating_sub(page_size)
    }
}

/// The `from_seq` of the newest full page for a chain of length
/// `total_len` — shared by [`audit_page_from_seq`]'s forward clamp and
/// [`audit_initial_fetch_correction`]'s self-correction check.
fn audit_newest_from_seq(total_len: u64, page_size: u64) -> u64 {
    total_len.saturating_sub(page_size)
}

/// `/classic` retirement (Task 5 fix) — decides whether the Audit view's
/// *initial* fetch on switching into the view needs a follow-up
/// correction.
///
/// That first fetch has to guess `from_seq` before the true chain
/// length is known (from whatever `audit_total` happens to be cached —
/// `0` on a session's first visit, or possibly stale after a revisit
/// where the chain grew while the operator was on another tab). Once
/// the fetch returns, the real `total_len` is known, so the guess can
/// be checked against it: `guessed_from_seq` was right only if it
/// already equals the newest window's start for that `total_len`.
///
/// Returns `Some(corrected_from_seq)` when the guess was wrong (the
/// operator would otherwise be looking at a stale or oldest-first page
/// with no indication anything is off) — the caller should fetch again
/// with `corrected_from_seq`, once. Returns `None` when the guess was
/// already correct (a short chain that fits in one page, or a revisit
/// where the cache happened to be accurate) — no second fetch is
/// wasted.
///
/// This is a plain comparison, re-derived from the *current* `total_len`
/// every time it's called — it has no memory of "have I corrected once
/// already", so it self-corrects on every switch into the view, not
/// just the first one in a session (the exact latch bug the web Audit
/// screen's first fix attempt had to be reworked away from).
pub fn audit_initial_fetch_correction(
    guessed_from_seq: u64,
    total_len: u64,
    page_size: u64,
) -> Option<u64> {
    let newest_from_seq = audit_newest_from_seq(total_len, page_size);
    if guessed_from_seq == newest_from_seq {
        None
    } else {
        Some(newest_from_seq)
    }
}

/// Map a single daemon [`StreamEventPayload`] to one or more chat
/// lines. Reuses the event's [`StreamEventPayload::render_for_cli`]
/// text content (the same rendering the REPL prints) and splits it
/// into single display lines, tagging each with the right
/// [`LineKind`]. Blank trailing lines from the CLI formatting are
/// dropped so the scrollback stays tight.
pub fn lines_from_event(event: &StreamEventPayload) -> Vec<ChatLine> {
    let kind = match event {
        StreamEventPayload::Text { .. } => LineKind::Agent,
        StreamEventPayload::Status { .. } => LineKind::Status,
        StreamEventPayload::ToolCallStarted { .. }
        | StreamEventPayload::ToolCallFinished { .. }
        | StreamEventPayload::ToolOutput { .. } => LineKind::Tool,
        StreamEventPayload::ApprovalGate { .. } => LineKind::Gate,
        // Routing visibility B1 — not chat lines: the routed model belongs
        // in the status bar, and the consent request reaches the chat as
        // the turn's outcome text (the daemon words it for this channel).
        StreamEventPayload::ModelRouted { .. }
        | StreamEventPayload::CloudConsentRequested { .. } => return Vec::new(),
    };

    let rendered = event.render_for_cli();
    let lines: Vec<ChatLine> = rendered
        .split('\n')
        .map(|l| l.trim_end_matches('\r'))
        .filter(|l| !l.trim().is_empty())
        .map(|l| ChatLine::new(kind, l.to_string()))
        .collect();

    // An all-whitespace event (e.g. a bare newline `Text`) still
    // deserves a blank line so spacing the agent intended survives.
    if lines.is_empty() && !rendered.is_empty() {
        return vec![ChatLine::new(kind, String::new())];
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(state: AppState, s: &str) -> AppState {
        s.chars().fold(state, |st, c| update(st, Msg::InsertChar(c)))
    }

    // ---- routing visibility B1 ----

    #[test]
    fn routing_events_add_no_chat_lines() {
        let routed = StreamEventPayload::ModelRouted {
            model: "small@default".into(),
            task: "chat".into(),
            reason: "r".into(),
        };
        let consent = StreamEventPayload::CloudConsentRequested {
            model: "m".into(),
            endpoint: "e".into(),
            why: "w".into(),
            estimated_tokens: 1,
            can_allow_here: true,
        };
        assert!(lines_from_event(&routed).is_empty());
        assert!(lines_from_event(&consent).is_empty());
    }

    // ---- input editing ----

    #[test]
    fn insert_appends_and_advances_cursor() {
        let s = typed(AppState::new(), "hi");
        assert_eq!(s.input, "hi");
        assert_eq!(s.cursor, 2);
    }

    #[test]
    fn insert_at_cursor_midline() {
        let mut s = typed(AppState::new(), "ac");
        s = update(s, Msg::CursorLeft); // between a and c
        s = update(s, Msg::InsertChar('b'));
        assert_eq!(s.input, "abc");
        assert_eq!(s.cursor, 2);
    }

    #[test]
    fn backspace_removes_before_cursor() {
        let mut s = typed(AppState::new(), "abc");
        s = update(s, Msg::Backspace);
        assert_eq!(s.input, "ab");
        assert_eq!(s.cursor, 2);
    }

    #[test]
    fn backspace_at_start_is_noop() {
        let mut s = typed(AppState::new(), "x");
        s = update(s, Msg::CursorHome);
        s = update(s, Msg::Backspace);
        assert_eq!(s.input, "x");
        assert_eq!(s.cursor, 0);
    }

    #[test]
    fn delete_removes_at_cursor() {
        let mut s = typed(AppState::new(), "abc");
        s = update(s, Msg::CursorHome);
        s = update(s, Msg::Delete);
        assert_eq!(s.input, "bc");
        assert_eq!(s.cursor, 0);
    }

    #[test]
    fn cursor_bounds_clamp() {
        let mut s = typed(AppState::new(), "ab");
        s = update(s, Msg::CursorRight); // already at end
        assert_eq!(s.cursor, 2);
        for _ in 0..5 {
            s = update(s, Msg::CursorLeft);
        }
        assert_eq!(s.cursor, 0);
        s = update(s, Msg::CursorEnd);
        assert_eq!(s.cursor, 2);
    }

    #[test]
    fn editing_handles_multibyte_chars() {
        // 'é' and '🦀' are multi-byte; cursor is a char index, so
        // byte-offset math must not panic on a char boundary.
        let mut s = typed(AppState::new(), "é🦀z");
        assert_eq!(s.cursor, 3);
        s = update(s, Msg::CursorLeft); // before z
        s = update(s, Msg::Backspace); // remove 🦀
        assert_eq!(s.input, "éz");
        assert_eq!(s.cursor, 1);
    }

    // ---- submission ----

    #[test]
    fn submit_appends_operator_line_and_clears() {
        let mut s = typed(AppState::new(), "hello");
        s = update(s, Msg::Submit);
        assert_eq!(s.history, vec![ChatLine::new(LineKind::Operator, "hello")]);
        assert_eq!(s.input, "");
        assert_eq!(s.cursor, 0);
        assert!(s.status.working);
    }

    #[test]
    fn submit_trims_whitespace() {
        let mut s = typed(AppState::new(), "  hi  ");
        s = update(s, Msg::Submit);
        assert_eq!(s.history[0].text, "hi");
    }

    #[test]
    fn submit_blank_is_noop() {
        let mut s = typed(AppState::new(), "   ");
        s = update(s, Msg::Submit);
        assert!(s.history.is_empty());
        assert!(!s.status.working);
        assert!(s.submittable().is_none());
    }

    #[test]
    fn submit_ignored_while_working() {
        let mut s = typed(AppState::new(), "first");
        s = update(s, Msg::Submit);
        // Still working; type + submit again — must be ignored.
        s = typed(s, "second");
        s = update(s, Msg::Submit);
        assert_eq!(s.history.len(), 1);
        assert_eq!(s.input, "second");
    }

    // ---- scrolling ----

    #[test]
    fn scroll_bounds() {
        let mut s = AppState::new();
        for i in 0..3 {
            s.history.push(ChatLine::new(LineKind::Agent, format!("l{i}")));
        }
        s = update(s, Msg::ScrollUp(10));
        assert_eq!(s.scroll, 3, "clamps to history length");
        s = update(s, Msg::ScrollDown(1));
        assert_eq!(s.scroll, 2);
        s = update(s, Msg::ScrollDown(10));
        assert_eq!(s.scroll, 0, "clamps to bottom");
        s = update(s, Msg::ScrollUp(1));
        s = update(s, Msg::ScrollToBottom);
        assert_eq!(s.scroll, 0);
    }

    #[test]
    fn new_content_repins_to_bottom() {
        let mut s = AppState::new();
        s.history.push(ChatLine::new(LineKind::Agent, "old"));
        s = update(s, Msg::ScrollUp(1));
        assert_eq!(s.scroll, 1);
        s = update(
            s,
            Msg::TurnFinished {
                events: vec![StreamEventPayload::Text {
                    text: "new".into(),
                }],
                outcome: "completed: new".into(),
            },
        );
        assert_eq!(s.scroll, 0, "appending re-pins to the latest line");
    }

    // ---- status transitions ----

    #[test]
    fn connected_sets_status() {
        let s = update(
            AppState::new(),
            Msg::Connected {
                role: Some("assistant".into()),
            },
        );
        assert!(s.status.daemon_connected);
        assert_eq!(s.status.role.as_deref(), Some("assistant"));
    }

    #[test]
    fn turn_finished_clears_working_and_appends() {
        let mut s = typed(AppState::new(), "q");
        s = update(s, Msg::Submit);
        assert!(s.status.working);
        s = update(
            s,
            Msg::TurnFinished {
                events: vec![StreamEventPayload::Text {
                    text: "an answer".into(),
                }],
                outcome: "completed: an answer".into(),
            },
        );
        assert!(!s.status.working);
        assert_eq!(s.history.last().unwrap().kind, LineKind::Agent);
        assert_eq!(s.history.last().unwrap().text, "an answer");
    }

    #[test]
    fn turn_finished_coalesces_token_level_text_events() {
        // Vitrine §12 — the daemon streams token-level Text chunks;
        // they must render as one chat line, not one word per line.
        let mut s = typed(AppState::new(), "q");
        s = update(s, Msg::Submit);
        s = update(
            s,
            Msg::TurnFinished {
                events: vec![
                    StreamEventPayload::Text { text: "Hi".into() },
                    StreamEventPayload::Text { text: " there".into() },
                    StreamEventPayload::Text { text: "!".into() },
                ],
                outcome: "completed: Hi there!".into(),
            },
        );
        let agent_lines: Vec<_> = s
            .history
            .iter()
            .filter(|l| l.kind == LineKind::Agent)
            .collect();
        assert_eq!(agent_lines.len(), 1);
        assert_eq!(agent_lines[0].text, "Hi there!");
    }

    #[test]
    fn turn_finished_appends_correction_when_outcome_differs() {
        let mut s = typed(AppState::new(), "q");
        s = update(s, Msg::Submit);
        s = update(
            s,
            Msg::TurnFinished {
                events: vec![StreamEventPayload::Text {
                    text: "{\"path\": \"airports.csv\"}".into(),
                }],
                outcome: "completed: I wasn't able to produce a usable reply this turn — please try again.".into(),
            },
        );
        let last = s.history.last().unwrap();
        assert_eq!(last.kind, LineKind::System);
        assert!(
            last.text.contains("corrected"),
            "expected a correction line, got: {}",
            last.text
        );
    }

    #[test]
    fn turn_finished_no_correction_line_when_outcome_matches() {
        let mut s = typed(AppState::new(), "q");
        s = update(s, Msg::Submit);
        let len_before = s.history.len();
        s = update(
            s,
            Msg::TurnFinished {
                events: vec![StreamEventPayload::Text {
                    text: "an answer".into(),
                }],
                outcome: "completed: an answer".into(),
            },
        );
        assert_eq!(s.history.last().unwrap().kind, LineKind::Agent);
        assert_eq!(
            s.history.len(),
            len_before + 1,
            "outcome matches displayed text — no extra correction line should be appended"
        );
    }

    #[test]
    fn turn_finished_surfaces_non_completed_outcome() {
        let mut s = typed(AppState::new(), "q");
        s = update(s, Msg::Submit);
        s = update(
            s,
            Msg::TurnFinished {
                events: vec![],
                outcome: "stopped: 3 repeated identical tool calls".into(),
            },
        );
        let last = s.history.last().unwrap();
        assert_eq!(last.kind, LineKind::System);
        assert_eq!(last.text, "stopped: 3 repeated identical tool calls");
    }

    #[test]
    fn cancelled_clears_working() {
        let mut s = typed(AppState::new(), "q");
        s = update(s, Msg::Submit);
        s = update(s, Msg::Cancelled);
        assert!(!s.status.working);
        assert_eq!(s.history.last().unwrap().kind, LineKind::System);
    }

    #[test]
    fn error_clears_working_and_notes() {
        let mut s = typed(AppState::new(), "q");
        s = update(s, Msg::Submit);
        s = update(s, Msg::Error("socket closed".into()));
        assert!(!s.status.working);
        assert!(s.history.last().unwrap().text.contains("socket closed"));
    }

    #[test]
    fn quit_sets_flag() {
        let s = update(AppState::new(), Msg::Quit);
        assert!(s.should_quit);
    }

    // ---- view navigation ----

    #[test]
    fn default_view_is_chat() {
        assert_eq!(AppState::new().view, View::Chat);
    }

    #[test]
    fn next_view_cycles_and_wraps() {
        let mut s = AppState::new();
        for expected in [
            View::Missions,
            View::Dashboard,
            View::Audit,
            View::Tools,
            View::Chat,
        ] {
            s = update(s, Msg::NextView);
            assert_eq!(s.view, expected);
        }
    }

    #[test]
    fn prev_view_wraps_backwards() {
        let s = update(AppState::new(), Msg::PrevView);
        assert_eq!(s.view, View::Tools);
    }

    #[test]
    fn switch_view_jumps_directly() {
        let s = update(AppState::new(), Msg::SwitchView(View::Audit));
        assert_eq!(s.view, View::Audit);
    }

    // ---- Missions panel (Chapter J.7) ----

    fn mission(id: &str, phase: MissionPhase) -> MissionRow {
        MissionRow {
            id: id.into(),
            title: format!("mission {id}"),
            lead: "aria".into(),
            phase,
            progress: 0,
            steps: vec![MissionStep {
                label: "count".into(),
                state: StepState::Running,
            }],
            pending_gate: None,
        }
    }

    #[test]
    fn missions_update_replaces_and_clamps_selection() {
        let mut s = AppState::new();
        s.missions.selected = 5; // stale out-of-range index
        s = update(
            s,
            Msg::MissionsUpdated(vec![
                mission("m1", MissionPhase::Executing),
                mission("m2", MissionPhase::Planning),
            ]),
        );
        assert_eq!(s.missions.rows.len(), 2);
        assert_eq!(s.missions.selected, 1, "selection clamped into range");
        assert_eq!(s.missions.selected_row().unwrap().id, "m2");
    }

    #[test]
    fn mission_selection_moves_within_bounds() {
        let mut s = AppState::new();
        s = update(
            s,
            Msg::MissionsUpdated(vec![
                mission("a", MissionPhase::Executing),
                mission("b", MissionPhase::Done),
            ]),
        );
        assert_eq!(s.missions.selected, 0);
        // Prev at the top is a no-op.
        s = update(s, Msg::MissionSelectPrev);
        assert_eq!(s.missions.selected, 0);
        // Next moves down, then clamps at the last row.
        s = update(s, Msg::MissionSelectNext);
        assert_eq!(s.missions.selected, 1);
        s = update(s, Msg::MissionSelectNext);
        assert_eq!(s.missions.selected, 1, "clamped at the last mission");
    }

    #[test]
    fn empty_missions_have_no_selected_row() {
        let s = AppState::new();
        assert!(s.missions.selected_row().is_none());
        // Navigating an empty list never panics.
        let s = update(s, Msg::MissionSelectNext);
        assert_eq!(s.missions.selected, 0);
    }

    // ---- Chapter L.6 — daemon mission view → row mapping ----
    use aivyx_channel::team_mission::TeamStepView;

    #[test]
    fn mission_rows_from_views_maps_phase_steps_and_gate() {
        let view = TeamMissionView {
            id: "m-1".into(),
            goal: "ship the note".into(),
            lead: "chef".into(),
            phase: TeamMissionPhase::AwaitingApproval,
            pending_gate: Some("approve".into()),
            halt_reason: None,
            verify_attempts: 0,
            progress: 33,
            steps: vec![
                TeamStepView {
                    label: "research — researcher (delegate)".into(),
                    state: TeamStepState::Done,
                    step_id: "research".into(),
                    member: "researcher".into(),
                    kind: "delegate".into(),
                    deps: vec![],
                },
                TeamStepView {
                    label: "approve — reviewer (gate)".into(),
                    state: TeamStepState::Awaiting,
                    step_id: "approve".into(),
                    member: "reviewer".into(),
                    kind: "gate".into(),
                    deps: vec!["research".into()],
                },
                TeamStepView {
                    label: "write — writer (delegate)".into(),
                    state: TeamStepState::Pending,
                    step_id: "write".into(),
                    member: "writer".into(),
                    kind: "delegate".into(),
                    deps: vec!["approve".into()],
                },
            ],
        };
        let rows = mission_rows_from_views(vec![view]);
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.id, "m-1");
        assert_eq!(row.title, "ship the note");
        assert_eq!(row.phase, MissionPhase::AwaitingApproval);
        assert_eq!(row.lead, "chef", "the pack's lead flows through to the row");
        assert_eq!(row.progress, 33);
        assert_eq!(row.pending_gate.as_deref(), Some("approve"));
        assert_eq!(row.steps[0].state, StepState::Done);
        assert_eq!(row.steps[1].state, StepState::Gated, "awaiting → gated dot");
        assert_eq!(row.steps[2].state, StepState::Pending);
    }

    #[test]
    fn mission_compose_open_edit_and_cancel() {
        let mut s = AppState::new();
        s = update(s, Msg::MissionComposeOpen);
        assert_eq!(s.mission_compose.as_deref(), Some(""));
        s = update(s, Msg::MissionComposeChar('h'));
        s = update(s, Msg::MissionComposeChar('i'));
        assert_eq!(s.mission_compose.as_deref(), Some("hi"));
        s = update(s, Msg::MissionComposeBackspace);
        assert_eq!(s.mission_compose.as_deref(), Some("h"));
        s = update(s, Msg::MissionComposeCancel);
        assert!(s.mission_compose.is_none());
    }

    #[test]
    fn mission_rows_from_views_maps_a_rejected_step_to_failed() {
        let view = TeamMissionView {
            id: "m-2".into(),
            goal: "g".into(),
            lead: "coordinator".into(),
            phase: TeamMissionPhase::Rejected,
            pending_gate: None,
            halt_reason: None,
            verify_attempts: 0,
            progress: 50,
            steps: vec![TeamStepView {
                label: "approve — reviewer (gate)".into(),
                state: TeamStepState::Rejected,
                step_id: "approve".into(),
                member: "reviewer".into(),
                kind: "gate".into(),
                deps: vec![],
            }],
        };
        let rows = mission_rows_from_views(vec![view]);
        assert_eq!(rows[0].phase, MissionPhase::Rejected);
        assert_eq!(rows[0].steps[0].state, StepState::Failed);
        assert!(rows[0].pending_gate.is_none());
    }

    #[test]
    fn paused_team_mission_phase_projects_to_paused_mission_phase() {
        let view = TeamMissionView {
            id: "m-3".into(),
            goal: "g".into(),
            lead: "coordinator".into(),
            phase: TeamMissionPhase::Paused,
            pending_gate: None,
            halt_reason: None,
            verify_attempts: 0,
            progress: 50,
            steps: vec![],
        };
        let rows = mission_rows_from_views(vec![view]);
        assert_eq!(rows[0].phase, MissionPhase::Paused);
    }

    #[test]
    fn phase_and_step_labels() {
        assert_eq!(MissionPhase::Rejected.label(), "rejected");
        assert_eq!(MissionPhase::AwaitingApproval.label(), "approval");
        assert_eq!(StepState::Done.dot(), "●");
        assert_eq!(StepState::Gated.dot(), "⚑");
    }

    // ---- approval-gate transitions ----

    #[test]
    fn gate_pending_set_on_turn_then_cleared() {
        let mut s = AppState::new();
        s = update(
            s,
            Msg::TurnFinished {
                events: vec![StreamEventPayload::ApprovalGate {
                    mission_id: "m1".into(),
                    gate_id: "g1".into(),
                    reason: "writes a file".into(),
                    scope: Some("fs.write".into()),
                }],
                outcome: "escalated: gate".into(),
            },
        );
        let gate = s.gate.clone().expect("gate set");
        assert_eq!(gate.mission_id, "m1");
        assert_eq!(gate.gate_id, "g1");
        assert_eq!(gate.scope.as_deref(), Some("fs.write"));

        s = update(s, Msg::GateResolved { approved: true });
        assert!(s.gate.is_none());
        assert!(s.history.last().unwrap().text.contains("approved"));
    }

    // ---- Audit view (`/classic` retirement, Task E) ----

    #[test]
    fn audit_updated_replaces_entries_and_total() {
        let s = AppState::new();
        assert!(s.audit_entries.is_empty());
        assert_eq!(s.audit_total, 0);

        let entries = vec![AuditEntrySummary {
            seq: 1,
            appended_at_unix_ms: 1_000,
            event_type: "TurnStarted".into(),
            event: serde_json::json!({}),
            mac_hex: "abc".into(),
        }];
        let s = update(s, Msg::AuditUpdated { entries: entries.clone(), total_len: 42 });
        assert_eq!(s.audit_entries, entries);
        assert_eq!(s.audit_total, 42);

        // A second update fully replaces, it doesn't append.
        let s = update(s, Msg::AuditUpdated { entries: vec![], total_len: 42 });
        assert!(s.audit_entries.is_empty());
    }

    #[test]
    fn tool_stats_updated_replaces_state() {
        use aivyx_channel::daemon_ipc::ToolStat;
        let s = AppState::new();
        let tools = vec![ToolStat {
            name: "fs.read".to_string(),
            description: "read a file".to_string(),
            scope_base: "fs.read".to_string(),
            registered: true,
            calls: 1,
            outcomes: std::collections::BTreeMap::new(),
            total_duration_ms: 10,
        }];
        let s = update(s, Msg::ToolStatsUpdated(tools.clone()));
        assert_eq!(s.tool_stats, tools);
    }

    #[test]
    fn scroll_up_clamps_to_rendered_tools_line_count_in_tools_view() {
        // Final-review Critical finding — render_panel emits 1 header line
        // + 1 name line per tool + 1 extra outcome-breakdown line for every
        // tool with calls > 0, so the clamp must be
        // `1 + tool_stats.len() + (tools with calls > 0)`, not just
        // `tool_stats.len()` (the old, buggy clamp), or the bottom rows
        // become permanently unreachable once tools have nonzero calls.
        use aivyx_channel::daemon_ipc::ToolStat;
        let mut s = AppState::new();
        s.view = View::Tools;
        s.tool_stats = vec![
            ToolStat {
                name: "a".to_string(),
                description: String::new(),
                scope_base: "a".to_string(),
                registered: true,
                calls: 0,
                outcomes: std::collections::BTreeMap::new(),
                total_duration_ms: 0,
            },
            ToolStat {
                name: "b".to_string(),
                description: String::new(),
                scope_base: "b".to_string(),
                registered: true,
                calls: 1,
                outcomes: std::collections::BTreeMap::from([("completed".to_string(), 1u64)]),
                total_duration_ms: 5,
            },
        ];
        s = update(s, Msg::ScrollUp(10));
        assert_eq!(
            s.scroll, 4,
            "clamps to 1 (header) + 2 (tools) + 1 (b's outcome line), not tool_stats length"
        );
    }

    // ---- Audit pagination arithmetic (`/classic` retirement, Task E) ----

    #[test]
    fn audit_page_forward_advances_by_one_page() {
        // total=120, page=50: newest window starts at 70. From the
        // oldest-visible window (0), forward moves ahead exactly one page.
        assert_eq!(audit_page_from_seq(0, 120, 50, true), 50);
    }

    #[test]
    fn audit_page_backward_retreats_by_one_page() {
        // From the newest window (70), backward moves ahead exactly one
        // page toward the past.
        assert_eq!(audit_page_from_seq(70, 120, 50, false), 20);
    }

    #[test]
    fn audit_page_backward_clamps_at_zero() {
        // Already within one page of the start — must not wrap or go
        // negative (saturating), and must land exactly on 0, not some
        // negative-clamped-to-huge value.
        assert_eq!(audit_page_from_seq(20, 120, 50, false), 0);
        assert_eq!(audit_page_from_seq(0, 120, 50, false), 0, "already oldest is a no-op");
    }

    #[test]
    fn audit_page_forward_clamps_at_newest_window_not_total_len() {
        // total=120, page=50: the newest *full* window starts at 70, not
        // 120 — paging forward must never request an from_seq that would
        // return an empty page.
        assert_eq!(audit_page_from_seq(70, 120, 50, true), 70, "already newest is a no-op");
        // One page short of newest: lands exactly on the newest window,
        // not one page past it.
        assert_eq!(audit_page_from_seq(50, 120, 50, true), 70);
    }

    #[test]
    fn audit_page_handles_a_chain_shorter_than_one_page() {
        // total=30 < page=50: the "newest window" start saturates to 0,
        // so both directions are no-ops from the only page there is.
        assert_eq!(audit_page_from_seq(0, 30, 50, true), 0);
        assert_eq!(audit_page_from_seq(0, 30, 50, false), 0);
    }

    // ---- Audit initial-fetch self-correction (review fix, review round 2) ----

    #[test]
    fn audit_initial_fetch_corrects_a_zero_guess_on_a_long_chain() {
        // First-ever visit of a session: audit_total was 0 before the
        // fetch, so the guess was 0 (oldest page) — but the fetch reveals
        // a 500-entry chain, so the newest window starts at 450.
        assert_eq!(
            audit_initial_fetch_correction(0, 500, 50),
            Some(450),
            "a zero guess on a long chain must be corrected to the newest window"
        );
    }

    #[test]
    fn audit_initial_fetch_corrects_a_stale_guess_after_the_chain_grew() {
        // A revisit: audit_total was cached at 500 from a previous visit
        // (guess = 450), but the chain grew to 900 while the operator was
        // on another tab, so the real newest window now starts at 850.
        // This must self-correct on *every* switch, not just the first
        // one in a session.
        assert_eq!(audit_initial_fetch_correction(450, 900, 50), Some(850));
    }

    #[test]
    fn audit_initial_fetch_needs_no_correction_for_a_short_chain() {
        // total=30 < page=50: the newest window saturates to 0, and a
        // first-ever guess (also 0, since audit_total starts at 0) is
        // already correct — no wasted second fetch.
        assert_eq!(audit_initial_fetch_correction(0, 30, 50), None);
    }

    #[test]
    fn audit_initial_fetch_needs_no_correction_when_the_cache_was_accurate() {
        // A revisit where the chain hasn't grown since the cached guess
        // was computed: the guess already lands on the true newest
        // window, so no second fetch fires.
        assert_eq!(audit_initial_fetch_correction(450, 500, 50), None);
    }

    #[test]
    fn audit_page_handles_an_empty_chain() {
        assert_eq!(audit_page_from_seq(0, 0, 50, true), 0);
        assert_eq!(audit_page_from_seq(0, 0, 50, false), 0);
    }

    // ---- event -> line mapping ----

    #[test]
    fn maps_text_event_to_agent_lines() {
        let ev = StreamEventPayload::Text {
            text: "line one\nline two\n".into(),
        };
        let lines = lines_from_event(&ev);
        assert_eq!(lines.len(), 2);
        assert!(lines.iter().all(|l| l.kind == LineKind::Agent));
        assert_eq!(lines[0].text, "line one");
        assert_eq!(lines[1].text, "line two");
    }

    #[test]
    fn maps_status_event() {
        let ev = StreamEventPayload::Status {
            status: "thinking".into(),
        };
        let lines = lines_from_event(&ev);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].kind, LineKind::Status);
        assert!(lines[0].text.contains("thinking"));
    }

    #[test]
    fn maps_tool_call_events_to_tool_kind() {
        let started = StreamEventPayload::ToolCallStarted {
            tool_id: "t1".into(),
            tool_name: "web.search".into(),
            input: serde_json::json!({"q": "rust"}),
        };
        let finished = StreamEventPayload::ToolCallFinished {
            tool_id: "t1".into(),
            tool_name: "web.search".into(),
            outcome_summary: "3 results".into(),
        };
        for ev in [started, finished] {
            let lines = lines_from_event(&ev);
            assert_eq!(lines.len(), 1);
            assert_eq!(lines[0].kind, LineKind::Tool);
            assert!(lines[0].text.contains("web.search"));
        }
    }

    #[test]
    fn loop_status_updated_replaces_state() {
        let s = AppState::new();
        assert!(s.loop_status.is_none());
        let view = LoopStatusView {
            state: aivyx_channel::loop_driver::LoopRunState {
                active: true,
                iteration: 3,
                max_iterations: 10,
                ..Default::default()
            },
            remaining: 2,
            armed: true,
            gate_enabled: false,
            max_run_secs: None,
            max_run_tokens: None,
            max_run_usd: None,
            max_idle_iterations: 0,
        };
        let s = update(s, Msg::LoopStatusUpdated(view.clone()));
        assert_eq!(s.loop_status, Some(view));
    }

    #[test]
    fn reminders_updated_replaces_list() {
        let s = AppState::new();
        assert!(s.reminders.is_none(), "not yet fetched");
        let reminders = vec![aivyx_channel::daemon_ipc::ReminderView {
            id: "r1".into(),
            due_unix: 100,
            message: "call mom".into(),
            notify_targets: vec![],
            created_unix: 0,
        }];
        let s = update(s, Msg::RemindersUpdated(reminders.clone()));
        assert_eq!(s.reminders, Some(reminders));
    }

    #[test]
    fn maps_gate_event_to_gate_kind() {
        let ev = StreamEventPayload::ApprovalGate {
            mission_id: "m1".into(),
            gate_id: "g1".into(),
            reason: "deletes data".into(),
            scope: None,
        };
        let lines = lines_from_event(&ev);
        assert!(!lines.is_empty());
        assert!(lines.iter().all(|l| l.kind == LineKind::Gate));
        assert!(lines.iter().any(|l| l.text.contains("deletes data")));
    }
}
