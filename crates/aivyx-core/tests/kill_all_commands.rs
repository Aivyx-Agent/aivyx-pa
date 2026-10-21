//! `kill_all_process_groups` acts on a process-wide registry (and closes
//! it for good), so it gets a test binary of its own: in the unit tests it
//! would kill the commands of every other test running alongside it.
#![cfg(target_os = "linux")]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use aivyx_core::{
    AgentId, CancellationToken, ChannelContext, ChannelError, ChannelPlatform, MessageOrigin,
    NullAuditHook, SessionId, ShellExecToolConfig, StreamEvent, Tool, ToolContext, ToolOutcome,
    TurnId, TurnOutcome, kill_all_process_groups,
};
use async_trait::async_trait;
use serde_json::json;

struct NoopChannel {
    session: SessionId,
    token: CancellationToken,
}

#[async_trait]
impl ChannelContext for NoopChannel {
    fn channel_name(&self) -> &str {
        "test"
    }
    fn platform(&self) -> ChannelPlatform {
        ChannelPlatform::Local
    }
    fn trust_tier(&self) -> aivyx_capability::TrustTier {
        aivyx_capability::TrustTier::Trusted
    }
    fn session_id(&self) -> SessionId {
        self.session
    }
    async fn stream_event(&self, _event: StreamEvent<'_>) -> Result<(), ChannelError> {
        Ok(())
    }
    async fn finalize(&self, _outcome: &TurnOutcome) -> Result<(), ChannelError> {
        Ok(())
    }
    fn cancellation_token(&self) -> CancellationToken {
        self.token.clone()
    }
}

/// Run `cmd` through `shell.exec` rooted at `dir`, as a tool call would.
async fn shell_exec(dir: PathBuf, cmd: &str) -> ToolOutcome {
    let tool = ShellExecToolConfig::new(&dir).build().unwrap();
    let channel = NoopChannel {
        session: SessionId::new(),
        token: CancellationToken::new(),
    };
    let ctx = ToolContext {
        agent_id: AgentId::new(),
        session_id: channel.session,
        turn_id: TurnId::new(),
        channel: &channel,
        audit: &NullAuditHook,
        cancellation: &channel.token,
        message_origin: MessageOrigin::Operator,
    };
    tool.execute(
        json!({ "cmd": cmd, "args": { "timeout_ms": 600_000 } }),
        &ctx,
    )
    .await
}

/// Whether `pid` is gone (or a zombie), polling for up to three seconds.
fn wait_until_gone(pid: u32) -> bool {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Err(_) => return true,
            Ok(stat) => {
                let state = stat
                    .rsplit_once(')')
                    .and_then(|(_, rest)| rest.split_whitespace().next());
                if matches!(state, Some("Z" | "X")) {
                    return true;
                }
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn read_pid(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

#[tokio::test(flavor = "multi_thread")]
async fn kill_all_ends_running_commands_and_any_started_after() {
    // Live check (2026-10-21): aivyx-pa ended by SIGTERM, a terminal
    // hang-up or a second Ctrl-C never dropped its process-group guards, so
    // a running shell.exec command outlived it indefinitely.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    let pid_file = root.join("bg.pid");

    let running = tokio::spawn(shell_exec(
        root.clone(),
        "sleep 300 >/dev/null 2>&1 & echo $! > bg.pid; sleep 300",
    ));
    let deadline = Instant::now() + Duration::from_secs(10);
    let pid = loop {
        if let Some(pid) = read_pid(&pid_file) {
            break pid;
        }
        assert!(Instant::now() < deadline, "the command never started");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };

    kill_all_process_groups();

    let outcome = tokio::time::timeout(Duration::from_secs(10), running)
        .await
        .expect("the running call must end once its group is killed")
        .unwrap();
    let gone = wait_until_gone(pid);
    // SAFETY: plain kill(2) on a positive pid, so a failure leaves nothing behind.
    unsafe {
        libc::kill(pid as i32, libc::SIGKILL);
    }
    assert!(
        gone,
        "background job {pid} survived kill_all_process_groups"
    );
    let ToolOutcome::Completed { output, .. } = outcome else {
        panic!("expected Completed, got {outcome:?}");
    };
    assert_eq!(output["exit_code"], -1, "killed by a signal: {output}");

    // The process is on its way out: a command started now dies at once.
    let late = shell_exec(root.clone(), "sleep 1; echo late > late.txt").await;
    assert!(matches!(late, ToolOutcome::Completed { .. }), "{late:?}");
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        !root.join("late.txt").exists(),
        "a command started after kill_all ran"
    );
}
