//! `aivyx-pa --headless "<task>"` — one-shot unattended turn (Chapter H
//! follow-on (a)).
//!
//! Chapter H made the daemon's turn path *honor* an unattended posture:
//! a headless run refuses (records the reason on the audit chain) at any
//! approval gate rather than parking for an operator (H.2/H.5/H.6). The
//! per-run `headless: true` IPC field and the `submit_input_headless`
//! client primitive already existed — this module is the missing CLI
//! consumer that makes the whole chapter reachable from a terminal,
//! a cron line, or a batch script.
//!
//! It connects to a **running daemon** (no in-process fallback — headless
//! relies on the daemon's gate interception), submits one turn over IPC,
//! streams the output, and maps the turn's terminal outcome onto a
//! process exit code so an unattended caller can branch on it:
//!
//! - `0` — the turn **completed**.
//! - `3` — the turn was **refused at a gate** (the headless posture: an
//!   escalation the daemon would normally hand to a human), or it
//!   **stopped to ask for cloud consent** (routing's cloud escalation in
//!   `ask` mode) — nothing ran, so a caller must not read it as success.
//! - `1` — any other non-completion (failed / timed out / step cap /
//!   cycle breaker / cancelled).

use std::path::Path;

use aivyx_channel::daemon_client::{DaemonSession, daemon_is_running};
use aivyx_channel::daemon_ipc::{FrontendType, StreamEventPayload, default_socket_path};

/// Shown after a one-shot run stops for cloud consent: the stop's own text
/// says to send /allow-cloud and resend, which a one-shot run (a fresh
/// conversation each time) can't do.
const ONE_SHOT_CONSENT_HINT: &str = "aivyx-pa --headless: a one-shot run can't give cloud consent \
     (each run is a new conversation). Allow it in `aivyx-pa` chat or the Studio, or pipe both \
     lines into one conversation: printf '/allow-cloud\\n<task>\\n' | aivyx-pa --headless";

/// The process exit code for one headless turn: a cloud-consent stop is
/// the refusal code (3) even though its outcome reads `completed:`;
/// otherwise [`headless_exit_code`].
fn turn_exit_code(outcome: &str, events: &[StreamEventPayload]) -> i32 {
    if events
        .iter()
        .any(|e| matches!(e, StreamEventPayload::CloudConsentRequested { .. }))
    {
        3
    } else {
        headless_exit_code(outcome)
    }
}

/// Entry point for `aivyx-pa --headless "<task>"`.
///
/// Returns `Err` for setup failures (no daemon, transport error) so the
/// caller renders them like any other CLI error (exit 1). A turn that
/// *ran* but did not complete (refused/failed/…) is **not** an `Err` — it
/// exits the process directly with the [`headless_exit_code`] mapping so
/// the distinct "refused" code (3) survives, which a `Result<(), String>`
/// (always exit 1) could not express.
pub async fn run_headless(task: &str) -> Result<(), String> {
    let socket_path = default_socket_path()?;
    require_daemon_running(&socket_path).await?;

    let mut session = DaemonSession::connect(&socket_path, None, Some(FrontendType::Local))
        .await
        .map_err(|e| {
            format!(
                "aivyx-pa --headless: failed to connect to the daemon on {} — {e}",
                socket_path.display(),
            )
        })?;

    let (events, outcome) = session
        .submit_input_headless(task.to_string())
        .await
        .map_err(|e| format!("aivyx-pa --headless: turn failed — {e}"))?;
    let _ = session.disconnect().await;

    // Stream the turn's output exactly as the interactive daemon REPL
    // would render it (reusing the shared `render_for_cli`).
    for event in &events {
        print!("{}", event.render_for_cli());
    }

    let code = turn_exit_code(&outcome, &events);
    if code == 3 && outcome.starts_with("completed:") {
        // A cloud-consent stop: the request text is the outcome.
        eprintln!("aivyx-pa --headless: {outcome}");
        eprintln!("{ONE_SHOT_CONSENT_HINT}");
        std::process::exit(code);
    }
    if code == 0 {
        // `completed: <final_message>` — the final message already
        // streamed as Text events; print the terminal line to stderr so
        // stdout stays the agent's answer.
        eprintln!("aivyx-pa --headless: {outcome}");
        return Ok(());
    }

    // A turn that ran but did not complete. Surface why on stderr and
    // exit with the classified code so cron/batch callers can branch.
    eprintln!("aivyx-pa --headless: {outcome}");
    std::process::exit(code);
}

/// Chapter Wire — `aivyx-pa --headless` with no task: read newline-
/// delimited turns from piped stdin and run them as consecutive turns
/// of ONE daemon session (conversation continuity, session-partitioned
/// memory, and consecutive-turn signals like the correction proxy all
/// apply — none of which a one-task-per-process caller can reach).
///
/// Fail-fast: the stream stops at the first non-completed turn and
/// exits with that turn's Chapter H code (3 gate-refusal / 1 other), so
/// a batch caller keeps the branchable codes and later lines of a
/// broken conversation never run. Blank lines are skipped. EOF with
/// every turn completed → exit 0.
pub async fn run_headless_stdin() -> Result<(), String> {
    use std::io::{BufRead, IsTerminal};

    if std::io::stdin().is_terminal() {
        return Err("`aivyx-pa --headless` without a task reads turns from piped \
             stdin — pipe newline-delimited turns in (e.g. `printf \
             \"first\\nsecond\\n\" | aivyx-pa --headless`) or pass a single \
             task: `aivyx-pa --headless \"<task>\"`"
            .to_string());
    }

    let socket_path = default_socket_path()?;
    require_daemon_running(&socket_path).await?;

    let mut session = DaemonSession::connect(&socket_path, None, Some(FrontendType::Local))
        .await
        .map_err(|e| {
            format!(
                "aivyx-pa --headless: failed to connect to the daemon on {} — {e}",
                socket_path.display(),
            )
        })?;

    let stdin = std::io::stdin();
    for line in stdin.lock().lines() {
        let line = line.map_err(|e| format!("aivyx-pa --headless: stdin read failed — {e}"))?;
        let task = line.trim();
        if task.is_empty() {
            continue;
        }
        let (events, outcome) = session
            .submit_input_headless(task.to_string())
            .await
            .map_err(|e| format!("aivyx-pa --headless: turn failed — {e}"))?;
        for event in &events {
            print!("{}", event.render_for_cli());
        }
        eprintln!("aivyx-pa --headless: {outcome}");
        let code = turn_exit_code(&outcome, &events);
        if code != 0 {
            let _ = session.disconnect().await;
            std::process::exit(code);
        }
    }
    let _ = session.disconnect().await;
    Ok(())
}

async fn require_daemon_running(socket_path: &Path) -> Result<(), String> {
    if daemon_is_running(socket_path).await {
        return Ok(());
    }
    Err(format!(
        "aivyx-pa --headless: no daemon running on socket {} — \
         start the daemon first with `aivyx-pa daemon run`",
        socket_path.display(),
    ))
}

/// Map a daemon `TurnComplete` outcome string (see `format_outcome` in
/// `daemon_server.rs`) onto a process exit code. Pure so it can be unit
/// tested against the exact prefixes the daemon emits.
///
/// - `completed: …` → `0`
/// - `escalated: …` → `3` (the headless refusal — the one code a caller
///   most wants to distinguish from a hard failure)
/// - anything else → `1` (failed / timed out / aborted / stopped /
///   cancelled)
fn headless_exit_code(outcome: &str) -> i32 {
    if outcome.starts_with("completed:") {
        0
    } else if outcome.starts_with("escalated:") {
        3
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::{headless_exit_code, turn_exit_code};
    use aivyx_channel::daemon_ipc::StreamEventPayload;

    #[test]
    fn a_cloud_consent_stop_is_the_refusal_code_not_success() {
        // The outcome reads "completed: …" (the consent request is the
        // turn's reply), but nothing ran — a cron caller must not see 0.
        let events = vec![StreamEventPayload::CloudConsentRequested {
            model: "claude-sonnet-5".into(),
            endpoint: "claude".into(),
            why: "this kind of request is set to use the cloud".into(),
            estimated_tokens: 12_578,
            can_allow_here: true,
        }];
        assert_eq!(turn_exit_code("completed: This needs a cloud model: …", &events), 3);
        assert_eq!(turn_exit_code("completed: 4", &[]), 0);
        assert_eq!(turn_exit_code("failed: provider error", &[]), 1);
    }

    #[test]
    fn completed_outcome_is_zero() {
        assert_eq!(headless_exit_code("completed: here is your answer"), 0);
    }

    #[test]
    fn escalated_outcome_is_the_distinct_refusal_code() {
        assert_eq!(
            headless_exit_code("escalated: shell.exec needs operator approval"),
            3
        );
    }

    #[test]
    fn other_non_completions_are_generic_failure() {
        assert_eq!(headless_exit_code("failed: provider error"), 1);
        assert_eq!(headless_exit_code("timed out"), 1);
        assert_eq!(headless_exit_code("aborted: planner exceeded 32 steps"), 1);
        assert_eq!(
            headless_exit_code("stopped: 3 repeated identical tool calls"),
            1
        );
        assert_eq!(headless_exit_code("cancelled"), 1);
    }
}
