//! Process-group lifetime for confined spawns.
//!
//! `aivyx-confine`'s `LandlockConfiner` makes every confined command the
//! leader of a new process group and refuses `setsid`/`setpgid` inside
//! the sandbox (unless `[confine] allow_leaving_process_group` is set),
//! so everything a command starts — a `cmd &` background job, a git hook's
//! daemonised grandchild — stays in that group. The caller owns the
//! group's lifetime: [`ProcessGroupGuard`] records the leader's pid right
//! after `spawn()` and `SIGKILL`s the whole group when it is dropped, so
//! the group dies when the tool call finishes, fails, times out or is
//! cancelled (the `execute` future dropped mid-await).
//!
//! Every spawn site that uses a guard also calls `process_group(0)` on the
//! command itself before confining it, so the group exists even under
//! `NoopConfiner` (which changes nothing) and the kill can never reach
//! the daemon's own group.
//!
//! A guard only runs when it is dropped, and nothing is dropped when the
//! process ends on a signal (`SIGTERM`, `SIGHUP` when its terminal goes
//! away) or calls `std::process::exit`. So every live group is also kept in
//! a process-wide registry, and [`kill_all_process_groups`] kills them all
//! before such an exit — without it a running command outlived aivyx-pa
//! indefinitely, reparented to init (live check, 2026-10-21).

use std::collections::BTreeSet;
use std::process::Output;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use tokio::process::{Child, Command};
use tokio_util::sync::CancellationToken;

/// Every group a guard (or a timed-out call's delayed kill) still owns.
/// `closed` is set by [`kill_all_process_groups`]: the process is on its
/// way out, so a group spawned after that is killed straight away.
struct Registry {
    live: BTreeSet<u32>,
    closed: bool,
}

static REGISTRY: Mutex<Registry> = Mutex::new(Registry {
    live: BTreeSet::new(),
    closed: false,
});

fn registry() -> MutexGuard<'static, Registry> {
    REGISTRY
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Record a live group. `false` once the registry is closed: the caller
/// must kill the group itself.
fn register(pgid: u32) -> bool {
    let mut registry = registry();
    if registry.closed {
        return false;
    }
    registry.live.insert(pgid);
    true
}

/// Kill a registered group and forget it.
fn kill_registered(pgid: u32) -> std::io::Result<()> {
    registry().live.remove(&pgid);
    kill_group(pgid)
}

/// Kill every command still running — every process group a
/// `shell.exec` or `git.*` call still owns — and refuse new ones: a group
/// spawned afterwards is killed as soon as it starts. For a process about
/// to exit without unwinding (a signal handler, `std::process::exit`) and
/// for a daemon starting its shutdown, which would otherwise wait for
/// those commands to finish. `SIGKILL` can't be caught, so it still
/// leaves them running.
pub fn kill_all_process_groups() {
    let live = {
        let mut registry = registry();
        registry.closed = true;
        std::mem::take(&mut registry.live)
    };
    for pgid in live {
        let _ = kill_group(pgid);
    }
}

/// Kills the process group led by a spawned child when dropped (or when
/// [`ProcessGroupGuard::kill_now`] is called, whichever comes first).
#[derive(Debug)]
pub(crate) struct ProcessGroupGuard {
    pgid: Option<u32>,
}

impl ProcessGroupGuard {
    /// Record `child`'s pid as its process-group id. Call right after
    /// `spawn()`: `Child::id()` is `None` once the child has been reaped.
    /// The command must have been spawned with `process_group(0)`.
    /// Also registers the group (see [`kill_all_process_groups`]); after
    /// that has run, the group is killed at once.
    pub(crate) fn new(child: &Child) -> Self {
        let pgid = child.id();
        if let Some(pgid) = pgid
            && !register(pgid)
        {
            let _ = kill_group(pgid);
        }
        ProcessGroupGuard { pgid }
    }

    /// `SIGKILL` the group now and disarm the guard. A group that no
    /// longer exists is not an error; any other failure is ignored too —
    /// there is nothing more a tool call can do about it.
    pub(crate) fn kill_now(&mut self) {
        if let Some(pgid) = self.pgid.take() {
            let _ = kill_registered(pgid);
        }
    }

    /// Disarm the guard without killing, handing the group id to a caller
    /// that takes over its lifetime (the `shell.exec` timeout path's
    /// SIGTERM-then-SIGKILL sequence). The group stays registered until
    /// that caller kills it with [`kill_registered`].
    pub(crate) fn disarm(&mut self) -> Option<u32> {
        self.pgid.take()
    }
}

/// `aivyx_confine::kill_process_group` on Linux. Elsewhere aivyx-confine
/// is built without its sandbox backend and that function is a no-op
/// (`NoopConfiner` makes no groups), but these spawn sites put the command
/// in its own group themselves, so kill it directly.
pub(crate) fn kill_group(pgid: u32) -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        aivyx_confine::kill_process_group(pgid)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let pgid = libc::pid_t::try_from(pgid)
            .ok()
            .filter(|&p| p > 0)
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
        // SAFETY: plain syscall with a validated, positive group id.
        if unsafe { libc::killpg(pgid, libc::SIGKILL) } == 0 {
            return Ok(());
        }
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        Err(err)
    }
}

impl Drop for ProcessGroupGuard {
    fn drop(&mut self) {
        self.kill_now();
    }
}

/// How [`run_in_own_group`] ended.
#[derive(Debug)]
pub(crate) enum GroupRun {
    /// The command exited; its status and everything it wrote.
    Finished(Output),
    /// The timeout fired first. The group has been sent SIGTERM and gets
    /// SIGKILL two seconds later; nothing that was written is returned.
    TimedOut,
    /// The cancellation token fired first (the turn was cancelled or hit
    /// its deadline). The group was killed; what it had written by then.
    Cancelled(Output),
}

/// Why [`run_in_own_group`] failed.
#[derive(Debug)]
pub(crate) enum RunError {
    Spawn(std::io::Error),
    Wait(std::io::Error),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::Spawn(e) => write!(f, "spawn failed: {e}"),
            RunError::Wait(e) => write!(f, "wait failed: {e}"),
        }
    }
}

/// Run an (already confined) command under the process-group contract
/// and capture its output. Shared by `shell.exec` and the `git.*` tools.
///
/// The command leads its own group (`process_group(0)`). The call ends
/// when the command itself exits, not when its pipes reach EOF: the
/// group is killed first, which closes any pipe a background job (`cmd
/// &`, a git hook's detached child) still holds, and then the pipes are
/// drained. The group is also killed if this future is dropped. With
/// `timeout`, a command still running when it fires gets SIGTERM on its
/// whole group, then SIGKILL two seconds later. With `cancellation`, a
/// command still running when the token fires is killed at once, and the
/// call returns what it wrote — the turn loop waits for a running tool
/// rather than dropping it, so without this a cancelled turn (Ctrl-C, its
/// deadline) sat out the command's whole timeout.
pub(crate) async fn run_in_own_group(
    mut command: Command,
    timeout: Option<Duration>,
    cancellation: Option<&CancellationToken>,
) -> Result<GroupRun, RunError> {
    command
        .process_group(0)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(RunError::Spawn)?;
    let pgid = child.id();
    let mut group = ProcessGroupGuard::new(&child);
    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();

    let run = async {
        let (status, stdout, stderr) = tokio::join!(
            async {
                let status = child.wait().await;
                group.kill_now();
                status
            },
            read_pipe(stdout_pipe.as_mut()),
            read_pipe(stderr_pipe.as_mut()),
        );
        status.map(|status| Output {
            status,
            stdout,
            stderr,
        })
    };
    let mut run = Box::pin(run);
    let deadline = async {
        match timeout {
            Some(limit) => tokio::time::sleep(limit).await,
            None => std::future::pending().await,
        }
    };
    let cancelled = async {
        match cancellation {
            Some(token) => token.cancelled().await,
            None => std::future::pending().await,
        }
    };
    enum Ended {
        Finished(std::io::Result<Output>),
        TimedOut,
        Cancelled,
    }
    let ended = tokio::select! {
        biased;
        result = &mut run => Ended::Finished(result),
        () = deadline => Ended::TimedOut,
        () = cancelled => Ended::Cancelled,
    };
    match ended {
        Ended::Finished(result) => result.map(GroupRun::Finished).map_err(RunError::Wait),
        Ended::Cancelled => {
            // Killing the group ends `sh` and closes every pipe, so `run`
            // finishes promptly with what was written so far.
            if let Some(pgid) = pgid {
                let _ = kill_group(pgid);
            }
            run.await.map(GroupRun::Cancelled).map_err(RunError::Wait)
        }
        Ended::TimedOut => {
            drop(run);
            // Graceful shutdown: SIGTERM the group so processes can flush
            // and clean up, SIGKILL it two seconds later. The guard is
            // disarmed so it doesn't SIGKILL straight away on return.
            if let Some(pgid) = group.disarm() {
                if let Ok(raw) = libc::pid_t::try_from(pgid) {
                    // SAFETY: killpg with a positive group id. A group that
                    // already exited returns ESRCH, ignored.
                    unsafe {
                        libc::killpg(raw, libc::SIGTERM);
                    }
                }
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    let _ = kill_registered(pgid);
                });
            }
            Ok(GroupRun::TimedOut)
        }
    }
}

/// How [`run_command_in_group`] ended.
#[derive(Debug)]
pub enum CommandRun {
    /// The command exited; its status and everything it wrote.
    Finished(Output),
    /// The timeout fired first; the whole group was sent SIGTERM, then SIGKILL.
    TimedOut,
}

/// Run an (already confined) command under the process-group contract with a
/// wall-clock `timeout`, for callers outside this crate (the loop's gate
/// command). The command leads its own group, the group is killed when the
/// command exits or this future is dropped, on timeout, and by
/// [`kill_all_process_groups`] when the daemon shuts down.
pub async fn run_command_in_group(
    command: Command,
    timeout: Duration,
) -> Result<CommandRun, String> {
    match run_in_own_group(command, Some(timeout), None).await {
        Ok(GroupRun::Finished(output) | GroupRun::Cancelled(output)) => {
            Ok(CommandRun::Finished(output))
        }
        Ok(GroupRun::TimedOut) => Ok(CommandRun::TimedOut),
        Err(e) => Err(e.to_string()),
    }
}

/// [`run_in_own_group`] without a timeout, as an `io::Result<Output>`
/// like `Command::output()`. stdin is `/dev/null`.
pub(crate) async fn confined_output(mut command: Command) -> std::io::Result<Output> {
    command.stdin(std::process::Stdio::null());
    match run_in_own_group(command, None, None).await {
        Ok(GroupRun::Finished(output)) => Ok(output),
        Ok(GroupRun::TimedOut | GroupRun::Cancelled(_)) => {
            unreachable!("no timeout or cancellation was set")
        }
        Err(RunError::Spawn(e) | RunError::Wait(e)) => Err(e),
    }
}

/// Read a child's captured pipe to EOF. A read error ends the capture
/// early with what was read so far — the command's own exit status is
/// what the caller acts on, not a broken pipe.
async fn read_pipe<R: tokio::io::AsyncRead + Unpin>(pipe: Option<&mut R>) -> Vec<u8> {
    use tokio::io::AsyncReadExt;
    let mut buf = Vec::new();
    if let Some(pipe) = pipe {
        let _ = pipe.read_to_end(&mut buf).await;
    }
    buf
}

#[cfg(all(test, target_os = "linux"))]
pub(crate) mod test_support {
    use std::time::{Duration, Instant};

    /// Whether `pid` is gone (no `/proc` entry, or a zombie waiting to be
    /// reaped), polling for up to three seconds so a just-sent `SIGKILL`
    /// has time to land.
    pub(crate) fn wait_until_gone(pid: u32) -> bool {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
                Err(_) => return true,
                Ok(stat) => {
                    // Field 3 (after the parenthesised comm) is the state.
                    let state = stat
                        .rsplit_once(')')
                        .and_then(|(_, rest)| rest.split_whitespace().next().map(str::to_owned));
                    if matches!(state.as_deref(), Some("Z") | Some("X")) {
                        return true;
                    }
                }
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Kill a leftover test process so a failing test doesn't leave a
    /// `sleep 300` behind.
    pub(crate) fn cleanup(pid: u32) {
        if let Ok(pid) = i32::try_from(pid) {
            // SAFETY: plain kill(2) on a positive pid.
            unsafe {
                libc::kill(pid, libc::SIGKILL);
            }
        }
    }
}
