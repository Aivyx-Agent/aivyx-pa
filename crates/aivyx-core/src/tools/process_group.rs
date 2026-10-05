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

use std::process::Output;
use std::time::Duration;

use tokio::process::{Child, Command};

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
    pub(crate) fn new(child: &Child) -> Self {
        ProcessGroupGuard { pgid: child.id() }
    }

    /// `SIGKILL` the group now and disarm the guard. A group that no
    /// longer exists is not an error; any other failure is ignored too —
    /// there is nothing more a tool call can do about it.
    pub(crate) fn kill_now(&mut self) {
        if let Some(pgid) = self.pgid.take() {
            let _ = kill_group(pgid);
        }
    }

    /// Disarm the guard without killing, handing the group id to a caller
    /// that takes over its lifetime (the `shell.exec` timeout path's
    /// SIGTERM-then-SIGKILL sequence).
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
/// drained. The group is also killed if this future is dropped (a
/// cancelled turn). With `timeout`, a command still running when it
/// fires gets SIGTERM on its whole group, then SIGKILL two seconds later.
pub(crate) async fn run_in_own_group(
    mut command: Command,
    timeout: Option<Duration>,
) -> Result<GroupRun, RunError> {
    command
        .process_group(0)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn().map_err(RunError::Spawn)?;
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
    let finished = match timeout {
        Some(limit) => match tokio::time::timeout(limit, run).await {
            Ok(result) => Some(result),
            Err(_elapsed) => None,
        },
        None => Some(run.await),
    };
    match finished {
        Some(result) => result.map(GroupRun::Finished).map_err(RunError::Wait),
        None => {
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
                    let _ = kill_group(pgid);
                });
            }
            Ok(GroupRun::TimedOut)
        }
    }
}

/// [`run_in_own_group`] without a timeout, as an `io::Result<Output>`
/// like `Command::output()`. stdin is `/dev/null`.
pub(crate) async fn confined_output(mut command: Command) -> std::io::Result<Output> {
    command.stdin(std::process::Stdio::null());
    match run_in_own_group(command, None).await {
        Ok(GroupRun::Finished(output)) => Ok(output),
        Ok(GroupRun::TimedOut) => unreachable!("no timeout was set"),
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

#[cfg(test)]
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
