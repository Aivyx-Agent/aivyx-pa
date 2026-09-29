//! The async terminal driver — Phase 185 Task 4.
//!
//! [`run`] is the TUI's entry point: it connects a [`DaemonSession`]
//! (auto-spawning the daemon if none is listening — exactly like the
//! REPL), enters the terminal via [`Tui`], and runs the event loop
//! that ties the pure [`crate::model`] / [`crate::event`] cores to
//! daemon I/O. Keystrokes become [`Action`]s; pure actions update the
//! state in-process, while submit / cancel / gate-resolve perform a
//! daemon round-trip and then apply the matching reducer message.
//!
//! The loop body is the integration surface — it talks to a live
//! daemon and a live terminal, so it is **operator-verified** rather
//! than unit-tested. The pieces it orchestrates ([`key_to_action`],
//! [`update`], [`render`](crate::render)) are tested in their own
//! modules. The pure CLI seams (`tui` arg parse) are tested in the
//! binary.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};

use aivyx_channel::daemon_client::{
    list_audit_entries, resolve_team_gate, spawn_daemon_and_wait, team_mission_list,
    team_run_goal, DaemonCancelHandle, DaemonSession, AUTO_SPAWN_TIMEOUT,
};
use aivyx_channel::daemon_ipc::{FrontendType, StreamEventPayload};

use crate::event::{key_to_action, Action};
use crate::model::{
    audit_initial_fetch_correction, audit_page_from_seq, mission_rows_from_views, update,
    AppState, Msg, View,
};
use crate::terminal::Tui;

/// Poll cadence for terminal key events. Short enough that an
/// in-flight turn cancels promptly; long enough to idle near-zero CPU.
const POLL: Duration = Duration::from_millis(100);

/// Chapter L.6 — how often the Missions panel polls the daemon's
/// `TeamMissionList` feed. Poll-based, mirroring `aivyx-pa loop status`; a live
/// mission updates within this window.
const MISSION_POLL: Duration = Duration::from_millis(1500);

/// `/classic` retirement (Task E) — the Audit view's page size. This is
/// an operator-paginated screen, not a background-polled live feed like
/// Missions, so there is no timer-driven refresh — only a fetch on
/// switching into the view and on each pagination keypress.
const AUDIT_PAGE_SIZE: u32 = 50;

/// Connect (auto-spawning the daemon if needed), enter the terminal,
/// and drive the TUI to completion. Restores the terminal on every
/// exit path (including panics, via [`Tui`]'s panic hook).
pub async fn run(socket_path: PathBuf, role: Option<String>) -> Result<(), String> {
    // Connect, or spawn a daemon and connect — same dance as the REPL.
    let mut session = match DaemonSession::connect(
        &socket_path,
        role.clone(),
        Some(FrontendType::Local),
    )
    .await
    {
        Ok(s) => s,
        Err(_) => {
            spawn_daemon_and_wait(&socket_path, AUTO_SPAWN_TIMEOUT)
                .await
                .map_err(|e| e.to_string())?;
            DaemonSession::connect(&socket_path, role.clone(), Some(FrontendType::Local))
                .await
                .map_err(|e| e.to_string())?
        }
    };
    let cancel = session.cancel_handle();

    let mut tui = Tui::init().map_err(|e| format!("terminal init: {e}"))?;
    let mut state = update(AppState::new(), Msg::Connected { role });

    let loop_result =
        run_loop(&mut tui, &mut session, &cancel, &socket_path, &mut state).await;

    // Restore the terminal *before* the disconnect / error surfaces.
    drop(tui);
    let _ = session.disconnect().await;
    loop_result
}

/// Apply a pure reducer message to the borrowed state in place.
fn apply(state: &mut AppState, msg: Msg) {
    *state = update(std::mem::take(state), msg);
}

/// The render → read-key → act loop. Returns on `Quit` (or a terminal
/// I/O error).
async fn run_loop(
    tui: &mut Tui,
    session: &mut DaemonSession,
    cancel: &DaemonCancelHandle,
    socket_path: &Path,
    state: &mut AppState,
) -> Result<(), String> {
    // Seed the Missions panel before the first draw.
    poll_missions(socket_path, state).await;

    loop {
        tui.draw(state).map_err(|e| format!("draw: {e}"))?;
        if state.should_quit {
            return Ok(());
        }

        // Wait for a key, but wake on the mission-poll cadence so the panel
        // stays live without a keystroke. On a tick we refresh and redraw.
        let key = tokio::select! {
            k = wait_for_key() => k,
            _ = tokio::time::sleep(MISSION_POLL) => {
                poll_missions(socket_path, state).await;
                // Phase 186 — piggyback loop-status/reminders refresh on
                // the same tick, but only while Dashboard is the active
                // view (unlike Missions, which polls unconditionally) so
                // an operator parked on Dashboard sees a running loop's
                // iteration count climb without a second timer, and
                // nothing is queried while Dashboard isn't on screen.
                // Audit deliberately does not ride this tick — see
                // `switching_to_dashboard`'s own comment below.
                if should_poll_dashboard(state.view) {
                    fetch_loop_status(socket_path, state).await;
                    fetch_reminders(socket_path, state).await;
                }
                continue;
            }
        };
        match key_to_action(key, state) {
            Action::None | Action::Cancel => {
                // `Cancel` is only meaningful during a turn (handled in
                // `run_turn`); at idle there's nothing to cancel.
            }
            Action::Update(msg) => {
                // `/classic` retirement (Task E) — switching into the Audit
                // view seeds it with the newest page; the view itself has
                // no background refresh, so this is the only fetch until
                // the operator pages (Action::AuditPage, below).
                let switching_to_audit = matches!(msg, Msg::SwitchView(View::Audit));
                // POLISH_WAVES.md sub-project 8 item B — same posture for
                // the Tools view: seed on switch, no background refresh.
                let switching_to_tools = matches!(msg, Msg::SwitchView(View::Tools));
                // Phase 186 — same posture again for Dashboard: seed all
                // three of its panels on switch (loop/reminders also ride
                // the Missions poll tick below while Dashboard stays
                // active; the audit summary does not, matching Audit's
                // own static-until-paginate behavior).
                let switching_to_dashboard = matches!(msg, Msg::SwitchView(View::Dashboard));
                apply(state, msg);
                if switching_to_audit {
                    // See `fetch_latest_audit_page`'s own doc comment for
                    // the guess-then-correct rationale.
                    fetch_latest_audit_page(socket_path, state).await;
                }
                if switching_to_tools {
                    fetch_tool_stats(socket_path, state).await;
                }
                if switching_to_dashboard {
                    fetch_loop_status(socket_path, state).await;
                    fetch_reminders(socket_path, state).await;
                    fetch_latest_audit_page(socket_path, state).await;
                }
            }
            Action::Quit => apply(state, Msg::Quit),
            Action::ResolveTeamGate(approved) => {
                resolve_team_gate_action(socket_path, state, approved).await;
            }
            Action::SubmitMission => {
                let goal = state.mission_compose.take().unwrap_or_default().trim().to_string();
                if !goal.is_empty() {
                    // Show the "starting…" indicator across the (slow) LLM
                    // decomposition, then refresh so the new mission appears.
                    state.mission_starting = true;
                    tui.draw(state).map_err(|e| format!("draw: {e}"))?;
                    // The TUI new-mission prompt runs on the daemon's default
                    // team; pack selection is a later UI affordance.
                    if let Err(e) = team_run_goal(socket_path, goal, None).await {
                        apply(state, Msg::Error(e.to_string()));
                    }
                    state.mission_starting = false;
                    poll_missions(socket_path, state).await;
                }
            }
            Action::Submit => {
                let Some(text) = state.submittable() else {
                    continue;
                };
                apply(state, Msg::Submit); // echo operator line + working
                tui.draw(state).map_err(|e| format!("draw: {e}"))?;
                match run_turn(session, cancel, text).await {
                    Ok((events, outcome)) => {
                        apply(state, Msg::TurnFinished { events, outcome })
                    }
                    Err(e) => apply(state, Msg::Error(e)),
                }
            }
            Action::ResolveGate(approved) => {
                if let Some(gate) = state.gate.clone() {
                    match session
                        .resolve_gate(gate.mission_id, gate.gate_id, approved)
                        .await
                    {
                        Ok(()) => apply(state, Msg::GateResolved { approved }),
                        Err(e) => apply(state, Msg::Error(e.to_string())),
                    }
                }
            }
            Action::AuditPage { forward } => {
                // The window currently on screen starts at its first
                // entry's `seq` (entries are contiguous, ascending); `0`
                // before any page has loaded. See `audit_page_from_seq`'s
                // own doc comment for why this isn't derived from
                // `state.audit_total` alone.
                let current_from_seq =
                    state.audit_entries.first().map(|e| e.seq).unwrap_or(0);
                let from_seq = audit_page_from_seq(
                    current_from_seq,
                    state.audit_total,
                    AUDIT_PAGE_SIZE as u64,
                    forward,
                );
                fetch_audit_page(socket_path, state, from_seq).await;
            }
        }
    }
}

/// Chapter L.6 — poll the daemon's team-mission feed and push it into the
/// Missions panel. Best-effort: a poll error (e.g. a daemon with no team
/// service) leaves the panel as-is rather than surfacing a chat error every
/// cadence.
async fn poll_missions(socket_path: &Path, state: &mut AppState) {
    if let Ok(records) = team_mission_list(socket_path).await {
        let views = records.iter().map(|r| r.to_view()).collect();
        apply(state, Msg::MissionsUpdated(mission_rows_from_views(views)));
    }
}

/// `/classic` retirement (Task E) — fetch a fresh page of audit entries
/// and push it into the Audit view. Called on switching into the view and
/// on each pagination keypress. Best-effort, matching `poll_missions`: a
/// fetch error (e.g. a daemon with no audit log configured) leaves the
/// panel as-is rather than surfacing a chat error.
async fn fetch_audit_page(socket_path: &Path, state: &mut AppState, from_seq: u64) {
    if let Ok((entries, total_len)) =
        list_audit_entries(socket_path, from_seq, AUDIT_PAGE_SIZE).await
    {
        apply(state, Msg::AuditUpdated { entries, total_len });
    }
}

/// POLISH_WAVES.md sub-project 8 item B — fetch a fresh `GetToolStats`
/// snapshot and push it into the Tools view. Called once on switching
/// into the view, mirroring `fetch_audit_page`'s own "no background
/// refresh" posture — `window_secs: None` (whole audit chain), matching
/// the `aivyx-pa tools` CLI's own default. Best-effort: a fetch error
/// (e.g. no audit log configured) leaves the panel as-is.
async fn fetch_tool_stats(socket_path: &Path, state: &mut AppState) {
    if let Ok(tools) = aivyx_channel::daemon_client::get_tool_stats(socket_path, None).await {
        apply(state, Msg::ToolStatsUpdated(tools));
    }
}

/// Phase 186 — seed the newest audit page, self-correcting the guessed
/// window against the real total. Shared by `switching_to_audit` and
/// `switching_to_dashboard` (Dashboard's audit summary shows the same
/// "newest page," just fewer lines of it) — extracted from what was
/// previously `switching_to_audit`'s own inline body so both call sites
/// share one implementation.
async fn fetch_latest_audit_page(socket_path: &Path, state: &mut AppState) {
    let guessed_from_seq = state.audit_total.saturating_sub(AUDIT_PAGE_SIZE as u64);
    fetch_audit_page(socket_path, state, guessed_from_seq).await;
    if let Some(corrected_from_seq) = audit_initial_fetch_correction(
        guessed_from_seq,
        state.audit_total,
        AUDIT_PAGE_SIZE as u64,
    ) {
        fetch_audit_page(socket_path, state, corrected_from_seq).await;
    }
}

/// Phase 186 — fetch loop status for the Dashboard's loop panel. Called
/// on switching onto Dashboard, and again on every Missions poll tick
/// while Dashboard stays the active view (see `run_loop`) so a running
/// loop's iteration count is visibly live.
async fn fetch_loop_status(socket_path: &Path, state: &mut AppState) {
    if let Ok((
        rs_state,
        remaining,
        armed,
        gate_enabled,
        max_run_secs,
        max_run_tokens,
        max_run_usd,
        max_idle_iterations,
    )) = aivyx_channel::daemon_client::loop_status(socket_path).await
    {
        apply(
            state,
            Msg::LoopStatusUpdated(crate::model::LoopStatusView {
                state: rs_state,
                remaining,
                armed,
                gate_enabled,
                max_run_secs,
                max_run_tokens,
                max_run_usd,
                max_idle_iterations,
            }),
        );
    }
}

/// Phase 186 — fetch reminders for the Dashboard's reminders panel.
/// Same refresh posture as `fetch_loop_status`.
async fn fetch_reminders(socket_path: &Path, state: &mut AppState) {
    if let Ok(reminders) = aivyx_channel::daemon_client::get_reminders(socket_path).await {
        apply(state, Msg::RemindersUpdated(reminders));
    }
}

/// Phase 186 — pure gate for the Missions-poll-tick branch above:
/// loop-status/reminders only re-fetch while Dashboard is the active
/// view. See the `should_poll_dashboard_only_when_dashboard_is_active`
/// test below for why this is its own function.
fn should_poll_dashboard(view: View) -> bool {
    matches!(view, View::Dashboard)
}

/// Chapter L.6 — resolve the selected mission's human-approval gate, then
/// refresh the feed so the panel reflects the new phase immediately.
async fn resolve_team_gate_action(socket_path: &Path, state: &mut AppState, approved: bool) {
    let Some((id, step)) = state
        .missions
        .selected_row()
        .and_then(|m| m.pending_gate.clone().map(|g| (m.id.clone(), g)))
    else {
        return; // selection isn't awaiting a gate — keys were inert anyway
    };
    match resolve_team_gate(socket_path, id, step, approved).await {
        Ok(_) => poll_missions(socket_path, state).await,
        Err(e) => apply(state, Msg::Error(e.to_string())),
    }
}

/// Submit a turn and await its collected events, while concurrently
/// watching for an Esc / Ctrl-C that cancels the in-flight turn. The
/// daemon ends the turn early on `CancelTurn`, so the submit future
/// resolves with whatever it had plus the cancelled outcome.
async fn run_turn(
    session: &mut DaemonSession,
    cancel: &DaemonCancelHandle,
    text: String,
) -> Result<(Vec<StreamEventPayload>, String), String> {
    let turn = session.submit_input(text);
    tokio::pin!(turn);
    loop {
        tokio::select! {
            res = &mut turn => return res.map_err(|e| e.to_string()),
            maybe_key = poll_key(POLL) => {
                if let Some(k) = maybe_key {
                    if is_cancel_key(&k) {
                        cancel.cancel().await;
                    }
                }
            }
        }
    }
}

/// Esc or Ctrl-C — the in-turn cancel binding.
fn is_cancel_key(k: &KeyEvent) -> bool {
    matches!(k.code, KeyCode::Esc)
        || (k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL))
}

/// Block (without busy-spinning the terminal) until a key event
/// arrives, redraw-free between polls so an idle TUI uses ~no CPU.
async fn wait_for_key() -> KeyEvent {
    loop {
        if let Some(k) = poll_key(POLL).await {
            return k;
        }
    }
}

/// Poll for a single key event with a timeout, off the async runtime's
/// worker threads (crossterm's poll/read are blocking). Returns `None`
/// on timeout or a non-key event.
async fn poll_key(timeout: Duration) -> Option<KeyEvent> {
    tokio::task::spawn_blocking(move || match event::poll(timeout) {
        Ok(true) => match event::read() {
            Ok(Event::Key(k)) => Some(k),
            _ => None,
        },
        _ => None,
    })
    .await
    .ok()
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_poll_dashboard_only_when_dashboard_is_active() {
        assert!(should_poll_dashboard(View::Dashboard));
        assert!(!should_poll_dashboard(View::Chat));
        assert!(!should_poll_dashboard(View::Missions));
        assert!(!should_poll_dashboard(View::Audit));
        assert!(!should_poll_dashboard(View::Tools));
    }
}
