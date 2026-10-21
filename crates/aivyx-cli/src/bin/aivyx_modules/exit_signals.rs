//! Exiting without leaving commands behind.
//!
//! Every `shell.exec` / `git.*` command runs in a process group of its own,
//! killed when the tool call's guard is dropped. Nothing is dropped when
//! aivyx-pa ends on a signal or calls `std::process::exit`, and the
//! command's own group never sees the terminal's `SIGINT`/`SIGHUP`, so a
//! running command outlived aivyx-pa indefinitely, reparented to init
//! (live check, 2026-10-21: in-process REPL on SIGTERM, SIGHUP and a second
//! Ctrl-C; `daemon run` on SIGHUP). Every exit that doesn't unwind goes
//! through [`exit`] instead, which kills them first
//! (`aivyx_core::kill_all_process_groups`). `SIGKILL` can't be caught and
//! still leaves them running.

/// Kill every command still running, then exit with `code`.
pub(crate) fn exit(code: i32) -> ! {
    aivyx_core::kill_all_process_groups();
    std::process::exit(code)
}

/// For every mode but `daemon run` (which shuts down gracefully, see
/// `run_async`): on `SIGTERM`, or `SIGHUP` when the terminal goes away, kill
/// every running command and exit. With `interrupt`, `SIGINT` (Ctrl-C) too —
/// for a mode that runs tools itself and has no Ctrl-C handler of its own;
/// the modes that do have one (the REPL's cancel-then-exit) call [`exit`].
///
/// Runs on a thread of its own with its own runtime, so it covers every
/// mode whatever runtime that mode builds. Before this, both signals took
/// their default action (exit at once), so installing the handler changes
/// nothing else about how aivyx-pa ends.
pub(crate) fn spawn_exit_on_signal(interrupt: bool) {
    #[cfg(unix)]
    {
        let spawned = std::thread::Builder::new()
            .name("aivyx-exit-signals".into())
            .spawn(move || {
                let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    return;
                };
                runtime.block_on(async move {
                    use tokio::signal::unix::{SignalKind, signal};
                    let (Ok(mut terminate), Ok(mut hangup)) = (
                        signal(SignalKind::terminate()),
                        signal(SignalKind::hangup()),
                    ) else {
                        return;
                    };
                    // Only registered with `interrupt`: registering SIGINT
                    // replaces its default action.
                    let mut int = if interrupt {
                        signal(SignalKind::interrupt()).ok()
                    } else {
                        None
                    };
                    let interrupted = async {
                        match int.as_mut() {
                            Some(int) => int.recv().await,
                            None => std::future::pending().await,
                        }
                    };
                    // The shell's convention for "ended by signal N": 128 + N.
                    let code = tokio::select! {
                        _ = terminate.recv() => 128 + 15,
                        _ = hangup.recv() => 128 + 1,
                        _ = interrupted => 128 + 2,
                    };
                    exit(code);
                });
            });
        // Without the thread, both signals keep their default action.
        drop(spawned);
    }
    #[cfg(not(unix))]
    let _ = interrupt;
}

/// Wait for a request to stop: Ctrl-C, `SIGTERM` or `SIGHUP`. For the
/// daemon's graceful shutdown. A signal that can't be listened to is left
/// out (and off Unix, where only Ctrl-C exists, failing to listen to it
/// exits through [`exit`] at once, as before).
pub(crate) async fn stop_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let listen = |kind: SignalKind| async move {
            match signal(kind) {
                Ok(mut stream) => {
                    stream.recv().await;
                }
                Err(_) => std::future::pending().await,
            }
        };
        tokio::select! {
            r = tokio::signal::ctrl_c() => {
                if r.is_err() {
                    std::future::pending::<()>().await;
                }
            }
            () = listen(SignalKind::terminate()) => {}
            () = listen(SignalKind::hangup()) => {}
        }
    }
    #[cfg(not(unix))]
    if tokio::signal::ctrl_c().await.is_err() {
        exit(130);
    }
}
