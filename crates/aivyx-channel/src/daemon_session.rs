//! Daemon-backed REPL session — Phase 18 Task 2.
//!
//! Provides [`run_daemon_session`], a drop-in REPL loop that
//! auto-spawns a daemon (if needed), connects via [`DaemonSession`],
//! reads lines from stdin, submits each as a turn, and renders
//! streamed events to a writer via [`StreamEventPayload::render_for_cli`].
//!
//! This is the daemon-mode counterpart of [`crate::run_session`]:
//! same UX (banner, prompt, ctrl-D to exit), but the turn loop runs
//! in the background daemon process rather than in-process.

use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::daemon_client::{spawn_daemon_and_wait, DaemonSession, AUTO_SPAWN_TIMEOUT};
use crate::daemon_ipc::{concat_text_events, turn_outcome_correction, FrontendType, StreamEventPayload};
use crate::session::SessionReport;

/// Configuration for a daemon-backed REPL session.
pub struct DaemonSessionConfig {
    pub socket_path: std::path::PathBuf,
    pub role: Option<String>,
    pub prompt: String,
    pub banner: Option<String>,
    /// Shared flag for the signal handler's "already cancelled this turn"
    /// state. The REPL loop resets this to `false` before each
    /// `submit_input` so that the first ctrl-C of a new turn always
    /// sends `CancelTurn` instead of exiting.
    pub cancel_flag: Option<Arc<AtomicBool>>,
    /// Frontend type sent in `StartSession` so the daemon can construct
    /// the appropriate `ChannelContext` per connection.
    pub frontend_type: Option<FrontendType>,
}

/// Drive a daemon-backed CLI session to completion.
///
/// The loop mirrors [`crate::run_session`]: read a line, submit it
/// to the daemon, render streamed events, repeat until EOF. Returns
/// a [`SessionReport`] with the same shape as the in-process path.
///
/// If no daemon is listening at `config.socket_path`, attempts to
/// auto-spawn one via `spawn_daemon_and_wait`. Returns `Err` if both
/// the spawn and the connect fail.
pub async fn run_daemon_session<R, W>(
    config: DaemonSessionConfig,
    reader: R,
    writer: W,
) -> Result<SessionReport, String>
where
    R: BufRead,
    W: Write,
{
    let session = match DaemonSession::connect(&config.socket_path, config.role.clone(), config.frontend_type).await {
        Ok(s) => s,
        Err(_) => {
            spawn_daemon_and_wait(&config.socket_path, AUTO_SPAWN_TIMEOUT)
                .await
                .map_err(|e| e.to_string())?;
            DaemonSession::connect(&config.socket_path, config.role, config.frontend_type)
                .await
                .map_err(|e| e.to_string())?
        }
    };

    run_daemon_session_inner(session, config.prompt, config.banner, config.cancel_flag, reader, writer).await
}

/// Run a daemon-backed REPL with an already-connected session.
pub async fn run_daemon_session_connected<R, W>(
    session: DaemonSession,
    config: DaemonSessionConfig,
    reader: R,
    writer: W,
) -> Result<SessionReport, String>
where
    R: BufRead,
    W: Write,
{
    run_daemon_session_inner(session, config.prompt, config.banner, config.cancel_flag, reader, writer).await
}

async fn run_daemon_session_inner<R, W>(
    mut session: DaemonSession,
    prompt: String,
    banner: Option<String>,
    cancel_flag: Option<Arc<AtomicBool>>,
    mut reader: R,
    mut writer: W,
) -> Result<SessionReport, String>
where
    R: BufRead,
    W: Write,
{
    // Banner.
    if let Some(banner) = banner.as_deref() {
        writeln!(writer, "{banner}").map_err(|e| format!("banner write: {e}"))?;
        writer.flush().map_err(|e| format!("banner flush: {e}"))?;
    }

    // REPL loop.
    let mut turns_run: usize = 0;
    let mut last_outcome_str: Option<String> = None;
    let mut line = String::new();

    loop {
        // Prompt.
        if !prompt.is_empty() {
            write!(writer, "{}", prompt).map_err(|e| format!("prompt write: {e}"))?;
            writer.flush().map_err(|e| format!("prompt flush: {e}"))?;
        }

        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(e) => return Err(format!("read error: {e}")),
        }

        let input = line.trim();
        if input.is_empty() {
            continue;
        }

        if let Some(ref flag) = cancel_flag {
            flag.store(false, Ordering::Relaxed);
        }

        let (events, outcome) = session.submit_input(input.to_string())
            .await
            .map_err(|e| e.to_string())?;

        for event in &events {
            let rendered = event.render_for_cli();
            write!(writer, "{rendered}").map_err(|e| format!("render write: {e}"))?;
        }
        writer.flush().map_err(|e| format!("render flush: {e}"))?;

        // Turn-outcome-correction follow-up (POLISH_WAVES.md
        // sub-project 4) — show the turn's own authoritative outcome
        // when it diverges from what the streamed events alone
        // rendered (a reply floor, a Candor/identifier-fidelity
        // annotation, or a non-completed outcome's reason). `outcome`
        // was already captured here (used below for the session
        // report) but never written to the terminal.
        let displayed = concat_text_events(&events);
        if let Some(note) = turn_outcome_correction(&displayed, &outcome) {
            writeln!(writer, "{note}").map_err(|e| format!("outcome-correction write: {e}"))?;
            writer.flush().map_err(|e| format!("outcome-correction flush: {e}"))?;
        }

        for event in &events {
            if let StreamEventPayload::ApprovalGate {
                mission_id,
                gate_id,
                ..
            } = event
            {
                write!(writer, "  Approve? [y/N]: ")
                    .map_err(|e| format!("gate prompt write: {e}"))?;
                writer.flush().map_err(|e| format!("gate prompt flush: {e}"))?;
                let mut gate_line = String::new();
                match reader.read_line(&mut gate_line) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(e) => return Err(format!("gate input read: {e}")),
                }
                let approved = matches!(
                    gate_line.trim().to_lowercase().as_str(),
                    "y" | "yes"
                );
                session
                    .resolve_gate(
                        mission_id.clone(),
                        gate_id.clone(),
                        approved,
                    )
                    .await
                    .map_err(|e| e.to_string())?;
                let status = if approved { "approved" } else { "rejected" };
                writeln!(writer, "  Gate {status}.")
                    .map_err(|e| format!("gate status write: {e}"))?;
                writer.flush().map_err(|e| format!("gate status flush: {e}"))?;
            }
        }

        turns_run += 1;
        last_outcome_str = Some(outcome);
    }

    let _ = session.disconnect().await;

    Ok(SessionReport {
        turns_run,
        last_outcome: last_outcome_str.map(|s| outcome_str_to_turn_outcome(&s)),
    })
}

/// Best-effort parse of the daemon's outcome string back into a
/// `TurnOutcome`. The daemon sends `format_outcome` strings like
/// "completed: Hello from daemon!". This is lossy — we recover the
/// variant and the message but not the metadata (duration, tool count).
fn outcome_str_to_turn_outcome(s: &str) -> aivyx_core::TurnOutcome {
    if let Some(msg) = s.strip_prefix("completed: ") {
        aivyx_core::TurnOutcome::Completed {
            final_message: msg.to_string(),
            tool_calls_made: 0,
            duration: Duration::ZERO,
        }
    } else if let Some(msg) = s.strip_prefix("failed: ") {
        aivyx_core::TurnOutcome::Failed(aivyx_core::AivyxError::Config(msg.to_string()))
    } else if s == "cancelled" {
        aivyx_core::TurnOutcome::Cancelled {
            tool_calls_made: 0,
        }
    } else if s == "timed out" {
        aivyx_core::TurnOutcome::TimedOut {
            tool_calls_made: 0,
            elapsed: Duration::ZERO,
        }
    } else if let Some(reason) = s.strip_prefix("escalated: ") {
        aivyx_core::TurnOutcome::Escalated {
            reason: reason.to_string(),
            pending_tool: aivyx_core::ToolId::new(),
            // RN.3 — reconstructed from the wire outcome string; the scope is
            // not transported in this legacy path.
            scope: None,
            tool_calls_made: 0,
        }
    } else {
        aivyx_core::TurnOutcome::Completed {
            final_message: s.to_string(),
            tool_calls_made: 0,
            duration: Duration::ZERO,
        }
    }
}
