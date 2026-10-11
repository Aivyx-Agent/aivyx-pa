//! Stdio transport — communicates with an MCP server over
//! stdin/stdout of a child process using newline-delimited
//! JSON-RPC 2.0.

use std::sync::Arc;

use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Child;
use tokio::sync::Mutex;

use crate::transport_trait::McpTransport;

/// Phase 55 — operator-supplied command wrapper that hardens an
/// MCP server spawn.
///
/// Parallel to `aivyx_tool::SandboxConfig` (Phase 52). Same wire
/// shape (`wrapper`, `args`); independent type so MCP and
/// Aivyx-native tools can diverge if their sandbox needs differ
/// later. See `docs/TOOL_SDK.md` §9 for worked examples and
/// `docs/THREAT_MODEL.md` §5.2 for the threat this addresses.
///
/// The wrapper is responsible for setting up isolation (mount
/// namespaces, network namespaces, seccomp filters, etc.) and
/// then `exec`'ing the real command. Standard sandbox tools all
/// support this `wrapper [wrapper-args...] command [command-args...]`
/// shape natively.
#[derive(Debug, Clone)]
pub struct SandboxConfig {
    pub wrapper: String,
    pub args: Vec<String>,
}

/// Chapter Conduit (CD.3) — the last N stderr lines kept per stdio
/// server, so a misconfigured server's own diagnostic survives instead
/// of going to `/dev/null`. Caller-owned (the daemon holds a clone) so
/// the tail can be read for the failure log and `aivyx-pa mcp status`
/// without putting it on the [`McpTransport`] trait.
const MAX_STDERR_LINES: usize = 50;

#[derive(Clone, Default)]
pub struct StderrLog(Arc<std::sync::Mutex<std::collections::VecDeque<String>>>);

impl StderrLog {
    pub fn new() -> Self {
        Self::default()
    }

    fn push(&self, line: String) {
        let mut g = self.0.lock().unwrap();
        if g.len() >= MAX_STDERR_LINES {
            g.pop_front();
        }
        g.push_back(line);
    }

    /// The captured stderr lines, oldest first.
    pub fn tail(&self) -> Vec<String> {
        self.0.lock().unwrap().iter().cloned().collect()
    }
}

/// Owns a child process and implements `McpTransport` over its
/// stdin/stdout pipes. The child is spawned with `kill_on_drop(true)`
/// so it is cleaned up when the transport is dropped.
pub struct StdioTransport {
    #[allow(dead_code)]
    child: Mutex<Child>,
    writer: Arc<Mutex<tokio::process::ChildStdin>>,
    reader: Arc<Mutex<BufReader<tokio::process::ChildStdout>>>,
}

impl StdioTransport {
    /// Spawn a child process and take ownership of its stdio handles.
    ///
    /// Phase 55: when `sandbox` is `Some`, the effective spawn is
    /// `wrapper wrapper_args... command command_args...`. The wrapper
    /// is responsible for stdio passthrough — see
    /// `docs/TOOL_SDK.md` §9 for the contract a wrapper must satisfy.
    pub async fn start(
        command: &str,
        args: &[&str],
        env: &[(String, String)],
        sandbox: Option<&SandboxConfig>,
        stderr_log: Option<&StderrLog>,
    ) -> Result<Self, String> {
        // Phase 55 — pick the spawn shape based on the sandbox
        // wrapper. Same factoring as `aivyx-tool::ToolProcessBridge::spawn`.
        let mut cmd = match sandbox {
            Some(s) => {
                let mut c = tokio::process::Command::new(&s.wrapper);
                c.args(&s.args);
                c.arg(command);
                c.args(args);
                c
            }
            None => {
                let mut c = tokio::process::Command::new(command);
                c.args(args);
                c
            }
        };
        // Chapter Conduit (CD.1) — operator-supplied env vars (e.g. a
        // server's API token). Set on the spawned command; a sandbox
        // wrapper inherits and passes them to the wrapped child.
        cmd.envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
        // Chapter Conduit (CD.3) — capture stderr into a bounded ring
        // buffer when the caller wants diagnostics; otherwise discard
        // it as before.
        let stderr_mode = if stderr_log.is_some() {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        };
        let mut child = cmd
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(stderr_mode)
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                // Report the actual binary that failed to launch.
                // When sandboxed that's the wrapper (e.g., "bwrap not
                // on PATH"), not the wrapped MCP command. Operators
                // diagnose missing sandbox tools quickly that way.
                let blamed = match sandbox {
                    Some(s) => &s.wrapper,
                    None => command,
                };
                format!("spawn MCP server `{blamed}`: {e}")
            })?;

        let stdin = child.stdin.take().ok_or("no stdin on child")?;
        let stdout = child.stdout.take().ok_or("no stdout on child")?;

        // Drain captured stderr into the caller's ring buffer.
        if let (Some(log), Some(stderr)) = (stderr_log.cloned(), child.stderr.take()) {
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    log.push(line);
                }
            });
        }

        Ok(StdioTransport {
            child: Mutex::new(child),
            writer: Arc::new(Mutex::new(stdin)),
            reader: Arc::new(Mutex::new(BufReader::new(stdout))),
        })
    }

    /// Kill the child process. Called during bridge shutdown.
    pub async fn kill(&self) {
        let mut child = self.child.lock().await;
        let _ = child.kill().await;
    }
}

#[async_trait]
impl McpTransport for StdioTransport {
    async fn send(&self, message: &str) -> Result<(), String> {
        let mut w = self.writer.lock().await;
        w.write_all(message.as_bytes())
            .await
            .map_err(|e| format!("write to MCP server: {e}"))?;
        w.flush()
            .await
            .map_err(|e| format!("flush to MCP server: {e}"))?;
        Ok(())
    }

    async fn receive(&self) -> Result<String, String> {
        let mut r = self.reader.lock().await;
        let mut line = crate::limits::read_line_capped(&mut *r, crate::limits::MAX_MESSAGE_BYTES)
            .await
            .map_err(|e| format!("read from MCP server: {e}"))?;

        if line.is_empty() {
            return Err("MCP server closed stdout".into());
        }

        // Strip trailing newline for consistency with the trait contract.
        if line.ends_with('\n') {
            line.pop();
        }

        Ok(line)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Phase 55 — when a sandbox wrapper is supplied and the wrapper
    /// itself is missing (e.g., `bwrap` not on PATH), the
    /// operator-facing error must name the *wrapper*, not the
    /// wrapped MCP command. This is the ergonomic that makes
    /// diagnosing "missing sandbox tool" quick.
    #[tokio::test]
    async fn sandbox_wrapper_failure_reports_wrapper_name() {
        let sandbox = SandboxConfig {
            wrapper: "/definitely/not/a/real/sandbox-binary".into(),
            args: vec!["--isolated".into()],
        };
        let result = StdioTransport::start("python3", &[], &[], Some(&sandbox), None).await;
        match result {
            Ok(_) => panic!("missing wrapper must error at spawn"),
            Err(err) => {
                assert!(
                    err.contains("sandbox-binary"),
                    "error must name the wrapper, not the wrapped command \
                     — got {err:?}",
                );
            }
        }
    }

    /// Sanity: with no sandbox, the existing failure path is
    /// preserved — error names the wrapped command itself.
    #[tokio::test]
    async fn unsandboxed_failure_reports_command_name() {
        let result =
            StdioTransport::start("/definitely/not/a/real/mcp-server", &[], &[], None, None).await;
        match result {
            Ok(_) => panic!("missing command must error at spawn"),
            Err(err) => {
                assert!(
                    err.contains("mcp-server"),
                    "error must name the wrapped command — got {err:?}",
                );
            }
        }
    }
}
