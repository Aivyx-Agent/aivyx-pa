//! First-run coherence A1 — the interactive REPL starts the daemon when it
//! can, and says why when it can't.
//!
//! `aivyx-pa` on a terminal, with no daemon listening, used to chat
//! in-process and never start one, so the Studio, scheduled routines and
//! cloud escalation silently didn't work. Now [`decide`] picks one of:
//!
//! - [`Decision::Connect`] — a daemon is already listening;
//! - [`Decision::SpawnThenConnect`] — none is, but the passphrase comes from
//!   a source a background daemon inherits (the environment, the TOML config
//!   or the OS keyring), so start one and connect;
//! - [`Decision::InProcess`] — otherwise, with the reason.
//!
//! The decision runs in `run()` *before* the master key is derived and the
//! store opened: a daemon (already running or about to be started) holds the
//! store lock, so the REPL must not have opened it first. Piped (non-TTY)
//! input and `--no-daemon` keep the old in-process path, byte for byte.

/// The shared half of the in-process notices: what running without the
/// daemon costs.
const WITHOUT_THE_DAEMON: &str = "Running without the daemon — the Studio, scheduled routines \
                                  and cloud escalation need it.";

/// Why the REPL runs in-process instead of over the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InProcessReason {
    /// stdin is not a terminal (piped / scripted input). Unchanged
    /// behaviour: in-process, no daemon started, no new notice.
    NotATerminal,
    /// `--no-daemon` — the operator asked for in-process explicitly.
    NoDaemonFlag,
    /// The passphrase can only come from an interactive prompt, which a
    /// background daemon can't show.
    PassphraseNeedsPrompt,
    /// `--provider`, `--mcp-server` or `--mcp-sse` was given: they configure
    /// this session only, and a daemon runs on its own config.
    SessionOverrides,
}

/// What the interactive REPL does about the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Connect,
    SpawnThenConnect,
    InProcess { reason: InProcessReason },
}

/// Decide how the REPL reaches the agent. Pure; the caller supplies the
/// facts. `passphrase_non_interactive` is whether `select_passphrase_source`
/// picked the environment, the TOML config or the OS keyring (anything but
/// the interactive prompt).
pub fn decide(
    is_tty: bool,
    no_daemon: bool,
    session_overrides: bool,
    daemon_running: bool,
    passphrase_non_interactive: bool,
) -> Decision {
    if no_daemon {
        return Decision::InProcess {
            reason: InProcessReason::NoDaemonFlag,
        };
    }
    if !is_tty {
        return Decision::InProcess {
            reason: InProcessReason::NotATerminal,
        };
    }
    if session_overrides {
        return Decision::InProcess {
            reason: InProcessReason::SessionOverrides,
        };
    }
    if daemon_running {
        return Decision::Connect;
    }
    if passphrase_non_interactive {
        return Decision::SpawnThenConnect;
    }
    Decision::InProcess {
        reason: InProcessReason::PassphraseNeedsPrompt,
    }
}

/// The one line printed after the REPL started the daemon itself.
pub fn started_line(log_path: &std::path::Path) -> String {
    format!(
        "Started the aivyx-pa daemon in the background (log: {}; stop it with \
         `aivyx-pa daemon stop`).",
        log_path.display()
    )
}

/// The in-process notice for a REPL that could have used the daemon but
/// can't: why, and what is unavailable. `None` for the reasons that keep
/// today's behaviour (piped input, `--no-daemon`).
pub fn in_process_notice(reason: &InProcessReason) -> Option<String> {
    match reason {
        InProcessReason::PassphraseNeedsPrompt => Some(format!(
            "{WITHOUT_THE_DAEMON} Store your passphrase (`aivyx-pa keyring set`, or \
             export AIVYX_PA_PASSPHRASE where there's no keyring) and aivyx-pa will start \
             it for you."
        )),
        InProcessReason::SessionOverrides => Some(
            "Running in-process: --provider, --mcp-server and --mcp-sse apply only to \
             this session, and a daemon runs on its own config."
                .to_string(),
        ),
        InProcessReason::NotATerminal | InProcessReason::NoDaemonFlag => None,
    }
}

/// The in-process notice when starting (or connecting to) the daemon
/// failed: it leads with the failure.
pub fn start_failed_notice(error: &str, log_path: &std::path::Path) -> String {
    format!(
        "The aivyx-pa daemon didn't start ({error}; log: {}). {WITHOUT_THE_DAEMON}",
        log_path.display()
    )
}

/// The in-process startup warning when routing is enabled and
/// `[routing.endpoints]` names a cloud endpoint: escalation needs the
/// daemon, so this session can't use it.
pub fn in_process_cloud_warning(routing: Option<&aivyx_route::RoutingConfig>) -> Option<String> {
    // A cloud endpoint under disabled routing is inert either way.
    let routing = routing.filter(|r| r.enabled)?;
    let cloud: Vec<&str> = routing
        .endpoints
        .iter()
        .filter(|(_, ep)| ep.kind.locality() == aivyx_route::Locality::Cloud)
        .map(|(name, _)| name.as_str())
        .collect();
    if cloud.is_empty() {
        return None;
    }
    Some(format!(
        "aivyx-pa: [routing.endpoints] has a cloud endpoint ({}), but cloud escalation \
         needs the daemon — this in-process session won't use it.",
        cloud.join(", ")
    ))
}

/// Drive the REPL over an already-connected daemon session: the two-stage
/// ctrl-C handler (first cancels the in-flight turn, second exits) and the
/// daemon-backed read loop. `Err` is the session failing, for the caller to
/// report.
pub async fn run_connected(
    session: aivyx_channel::daemon_client::DaemonSession,
    socket_path: &std::path::Path,
    role: &str,
    prompt: &str,
    banner: String,
) -> Result<(), String> {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    let cancel_handle = session.cancel_handle();
    let cancelled_once = Arc::new(AtomicBool::new(false));
    let flag_for_signal = Arc::clone(&cancelled_once);

    // Signal task (daemon mode): first ctrl-C sends CancelTurn; second
    // ctrl-C exits. The REPL loop resets `cancelled_once` to false before
    // each turn via `DaemonSessionConfig::cancel_flag`.
    tokio::spawn(async move {
        loop {
            if tokio::signal::ctrl_c().await.is_err() {
                std::process::exit(130);
            }
            if flag_for_signal.load(Ordering::Relaxed) {
                eprintln!("\naivyx: interrupted, exiting.");
                std::process::exit(130);
            }
            eprintln!("\naivyx: cancelling in-flight turn (ctrl-C again to exit).");
            cancel_handle.cancel().await;
            flag_for_signal.store(true, Ordering::Relaxed);
        }
    });

    let daemon_config = aivyx_channel::DaemonSessionConfig {
        socket_path: socket_path.to_path_buf(),
        role: Some(role.to_string()),
        prompt: prompt.to_string(),
        banner: Some(banner),
        cancel_flag: Some(cancelled_once),
        frontend_type: Some(aivyx_channel::daemon_ipc::FrontendType::Local),
    };
    let stdin = std::io::stdin();
    let reader = stdin.lock();
    aivyx_channel::run_daemon_session_connected(session, daemon_config, reader, std::io::stdout())
        .await
        .map(|_report| ())
}

/// What [`start_and_connect`] leaves the caller to do.
pub enum Reached {
    /// The REPL ran over the daemon to completion (EOF / exit).
    Done,
    /// Starting or connecting failed: run in-process with this notice.
    InProcess(String),
}

/// The banner-side facts for a daemon-backed REPL, all from the config (the
/// REPL has not opened the store: the daemon holds it).
pub struct BannerFacts {
    pub fs_root: std::path::PathBuf,
    pub studio_line: Option<String>,
}

/// The banner of a REPL connected to a daemon it didn't open the store for.
pub fn connected_banner(socket_path: &std::path::Path, role: &str, facts: &BannerFacts) -> String {
    let mut banner = format!(
        "aivyx-pa {} (daemon) — type a message, ctrl-C to cancel, ctrl-D to exit.\n\
         daemon: {}\n\
         fs sandbox: {}\n\
         memory: live (recall persists across restarts)\n\
         audit: persistent (held by the daemon)\n\
         active role: {}",
        env!("CARGO_PKG_VERSION"),
        socket_path.display(),
        facts.fs_root.display(),
        role,
    );
    if let Some(line) = &facts.studio_line {
        banner.push('\n');
        banner.push_str(line);
    }
    banner
}

/// [`Decision::Connect`] / [`Decision::SpawnThenConnect`]: start the daemon
/// when asked (printing [`started_line`]), connect, and run the REPL over
/// it. A failure to start or connect comes back as
/// [`Reached::InProcess`] with [`start_failed_notice`]; a session that
/// fails after connecting is an `Err` — the daemon holds the store, so an
/// in-process fallback couldn't open it.
///
/// `studio_line` runs after the daemon is up, since a daemon started here
/// creates the Studio token file on its way up.
pub async fn start_and_connect(
    socket_path: &std::path::Path,
    spawn: bool,
    role: &str,
    prompt: &str,
    fs_root: std::path::PathBuf,
    studio_line: impl FnOnce() -> Option<String>,
) -> Result<Reached, String> {
    use aivyx_channel::daemon_client::{
        daemon_log_path, spawn_daemon_and_wait, DaemonSession, AUTO_SPAWN_TIMEOUT,
    };
    let log_path = daemon_log_path(socket_path);
    if spawn {
        if let Err(e) = spawn_daemon_and_wait(socket_path, AUTO_SPAWN_TIMEOUT).await {
            return Ok(Reached::InProcess(start_failed_notice(&e.to_string(), &log_path)));
        }
        eprintln!("{}", started_line(&log_path));
    }
    let session = match DaemonSession::connect(
        socket_path,
        Some(role.to_string()),
        Some(aivyx_channel::daemon_ipc::FrontendType::Local),
    )
    .await
    {
        Ok(session) => session,
        Err(e) => return Ok(Reached::InProcess(start_failed_notice(&e.to_string(), &log_path))),
    };
    let facts = BannerFacts {
        fs_root,
        studio_line: studio_line(),
    };
    let banner = connected_banner(socket_path, role, &facts);
    run_connected(session, socket_path, role, prompt, banner)
        .await
        .map(|()| Reached::Done)
        .map_err(|e| format!("daemon session failed: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    const NOTICE: &str = "Running without the daemon — the Studio, scheduled routines and \
                          cloud escalation need it. Store your passphrase (`aivyx-pa keyring \
                          set`, or export AIVYX_PA_PASSPHRASE where there's no keyring) and \
                          aivyx-pa will start it for you.";

    #[test]
    fn decide_covers_every_combination() {
        use Decision::*;
        use InProcessReason::*;
        // (is_tty, no_daemon, daemon_running, passphrase_non_interactive)
        for running in [false, true] {
            for non_interactive in [false, true] {
                // `--no-daemon` always wins, TTY or not.
                for tty in [false, true] {
                    assert_eq!(
                        decide(tty, true, false, running, non_interactive),
                        InProcess { reason: NoDaemonFlag },
                        "--no-daemon (tty={tty}, running={running}, ni={non_interactive})"
                    );
                }
                // Piped input keeps the old in-process path.
                assert_eq!(
                    decide(false, false, false, running, non_interactive),
                    InProcess { reason: NotATerminal },
                    "piped (running={running}, ni={non_interactive})"
                );
            }
            // A running daemon is connected to whatever the passphrase source.
        }
        assert_eq!(decide(true, false, false, true, false), Connect);
        assert_eq!(decide(true, false, false, true, true), Connect);
        // No daemon: start one only when it can get the passphrase itself.
        assert_eq!(decide(true, false, false, false, true), SpawnThenConnect);
        assert_eq!(
            decide(true, false, false, false, false),
            InProcess { reason: PassphraseNeedsPrompt }
        );
        // `--provider` / `--mcp-server` / `--mcp-sse` apply to this session
        // only: never started or connected past, whatever else holds.
        for running in [false, true] {
            for non_interactive in [false, true] {
                assert_eq!(
                    decide(true, false, true, running, non_interactive),
                    InProcess { reason: SessionOverrides },
                    "overrides (running={running}, ni={non_interactive})"
                );
            }
        }
    }

    #[test]
    fn started_line_is_the_specs_text() {
        assert_eq!(
            started_line(Path::new("/run/user/1000/aivyx-pa/daemon.log")),
            "Started the aivyx-pa daemon in the background (log: \
             /run/user/1000/aivyx-pa/daemon.log; stop it with `aivyx-pa daemon stop`)."
        );
    }

    #[test]
    fn in_process_notice_only_for_a_prompted_passphrase() {
        assert_eq!(
            in_process_notice(&InProcessReason::PassphraseNeedsPrompt).as_deref(),
            Some(NOTICE)
        );
        assert_eq!(in_process_notice(&InProcessReason::NotATerminal), None);
        assert_eq!(in_process_notice(&InProcessReason::NoDaemonFlag), None);
        let overrides = in_process_notice(&InProcessReason::SessionOverrides).unwrap();
        assert!(
            overrides.contains("--provider") && overrides.contains("only to this session"),
            "{overrides}"
        );
    }

    #[test]
    fn start_failed_notice_leads_with_the_failure() {
        let n = start_failed_notice("socket not found", Path::new("/r/aivyx-pa/daemon.log"));
        assert!(
            n.starts_with("The aivyx-pa daemon didn't start (socket not found; log: \
                           /r/aivyx-pa/daemon.log)."),
            "{n}"
        );
        assert!(
            n.contains(
                "Running without the daemon — the Studio, scheduled routines and cloud \
                 escalation need it."
            ),
            "{n}"
        );
    }

    #[test]
    fn connected_banner_adds_the_studio_line_when_there_is_one() {
        let facts = BannerFacts {
            fs_root: "/home/me/sandbox".into(),
            studio_line: Some("Studio: http://127.0.0.1:7843/?token=t".to_string()),
        };
        let b = connected_banner(Path::new("/r/aivyx-pa/daemon.sock"), "default", &facts);
        assert!(b.contains("daemon: /r/aivyx-pa/daemon.sock"), "{b}");
        assert!(b.contains("fs sandbox: /home/me/sandbox"), "{b}");
        assert!(b.contains("active role: default"), "{b}");
        assert!(b.ends_with("\nStudio: http://127.0.0.1:7843/?token=t"), "{b}");
        let without = connected_banner(
            Path::new("/r/aivyx-pa/daemon.sock"),
            "default",
            &BannerFacts { studio_line: None, ..facts },
        );
        assert!(!without.contains("Studio"), "{without}");
    }

    fn routing_with(kinds: &[(&str, &str)]) -> aivyx_route::RoutingConfig {
        let mut toml = String::from("enabled = true\n");
        for (name, kind) in kinds {
            toml.push_str(&format!("[endpoints.{name}]\nkind = \"{kind}\"\n"));
        }
        toml::from_str(&toml).expect("routing snippet parses")
    }

    #[test]
    fn cloud_warning_only_for_a_cloud_endpoint() {
        assert_eq!(in_process_cloud_warning(None), None);
        assert_eq!(in_process_cloud_warning(Some(&routing_with(&[("box", "ollama")]))), None);
        let w = in_process_cloud_warning(Some(&routing_with(&[
            ("box", "ollama"),
            ("claude", "anthropic"),
        ])))
        .expect("an anthropic endpoint warns");
        assert!(w.contains("claude"), "names the endpoint: {w}");
        assert!(w.contains("needs the daemon"), "{w}");
        assert!(
            in_process_cloud_warning(Some(&routing_with(&[("gpt", "openai")]))).is_some(),
            "openai is cloud too"
        );
        let mut off = routing_with(&[("claude", "anthropic")]);
        off.enabled = false;
        assert_eq!(in_process_cloud_warning(Some(&off)), None, "routing off: nothing to warn about");
    }
}
