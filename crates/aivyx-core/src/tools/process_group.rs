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

/// `Command::output()` for a confined command, with the process-group
/// contract applied: the command leads its own group, stdout/stderr are
/// captured, and the whole group is killed once the command has finished
/// — or as soon as this future is dropped, if the call is cancelled.
pub(crate) async fn confined_output(mut command: Command) -> std::io::Result<Output> {
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let child = command.spawn()?;
    let mut guard = ProcessGroupGuard::new(&child);
    let output = child.wait_with_output().await;
    guard.kill_now();
    output
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
                    let state = stat.rsplit_once(')').and_then(|(_, rest)| {
                        rest.split_whitespace().next().map(str::to_owned)
                    });
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
