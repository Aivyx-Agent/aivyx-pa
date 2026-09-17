//! `ToolProcessBridge` — daemon-side adapter for a spawned tool
//! process.
//!
//! Owns the child process, performs the `ToolHello` →
//! `ToolRegister` handshake on startup, and routes per-invocation
//! `InvokeTool` → `ToolResult`/`ToolError` traffic on demand.
//!
//! Phase 49 — foundation phase shipped the third-party path.
//!
//! Phase 50 (P12 closeout) wired the two deferred refinements:
//! `ToolEvent` frames are now relayed to the caller via an mpsc
//! channel (so a `ToolProxy` can forward them to its
//! `ChannelContext`), and the caller-supplied `call_id` lets
//! cancellation be targeted at a specific in-flight invocation.

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use thiserror::Error;
use tokio::io::BufReader;
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::{mpsc, Mutex};

use crate::frame::{read_frame, write_frame, FrameError};
use crate::wire::{
    DaemonToTool, ToolDescriptor, ToolEventPayload, ToolToDaemon, TOOL_PROTOCOL_VERSION,
};

/// Phase 191 — injected by the daemon binary (which owns
/// capability-checking and the real notification dispatcher)
/// so `aivyx-tool` stays free of any dependency on
/// `aivyx-channel`/`aivyx-capability`. `None` (the default) means
/// this tool process's `DispatchNotification` frames are silently
/// dropped — existing tool processes that never send them are
/// unaffected either way.
///
/// Not `async fn` — implementations that need to await (the real
/// one does, to call `NotifyDispatcher::dispatch`) should spawn
/// their own task internally and return immediately, so the
/// reader loop is never blocked waiting on a notification send.
pub trait NotificationSink: Send + Sync {
    fn dispatch(&self, target: String, message: String, subject: Option<String>);
}

#[derive(Debug, Error)]
pub enum ToolBridgeError {
    #[error("failed to spawn tool process `{command}`: {source}")]
    Spawn {
        command: String,
        source: std::io::Error,
    },
    #[error("tool process exited during handshake")]
    HandshakeClosed,
    #[error("expected ToolRegister, got {0:?}")]
    HandshakeUnexpected(ToolToDaemon),
    #[error("framing error: {0}")]
    Frame(#[from] FrameError),
    #[error("invocation `{call_id}` mismatched response (got call_id `{got}`)")]
    CallIdMismatch { call_id: String, got: String },
    #[error("tool returned error: [{code}] {message}")]
    ToolError { code: String, message: String },
    #[error("tool process closed the connection mid-invocation")]
    InvocationClosed,
    #[error("tool process produced an unparseable frame: {0}")]
    Decode(String),
}

/// Outcome of an `invoke` — distinguishes success and tool-side
/// failure so the caller can map them onto the right
/// `aivyx_core::ToolOutcome` variant.
#[derive(Debug, Clone)]
pub enum InvocationOutcome {
    Completed {
        verified: crate::wire::Verification,
        output: serde_json::Value,
    },
    ToolError {
        code: String,
        message: String,
    },
    /// Task 4 (HIGH, 2026-09-16 audit) — mirrors
    /// `wire::ToolToDaemon::RequiresEscalation`. Kept distinct from
    /// `ToolError` so `ToolProxy::execute` can map it onto the real
    /// `aivyx_core::ToolOutcome::RequiresEscalation` instead of a
    /// generic `Failed`.
    RequiresEscalation {
        reason: String,
    },
}

/// One message pumped from the reader loop to a pending invocation.
/// Phase 50 — Phase 49's reader_loop dropped `ToolEvent` frames on
/// the floor; Phase 50 surfaces them on the same channel as the
/// terminal `Outcome` variant so `invoke_with_events` can deliver
/// both kinds in order.
#[derive(Debug, Clone)]
enum BridgeMessage {
    Event(ToolEventPayload),
    Outcome(InvocationOutcome),
}

/// Configuration for spawning a tool process.
#[derive(Clone)]
pub struct ToolProcessConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    /// Phase 52 — optional command-wrapper sandbox. When `Some`,
    /// the bridge spawns `wrapper wrapper_args... command
    /// command_args...` instead of `command command_args...`.
    /// Aivyx supplies the policy slot; the operator supplies the
    /// policy (bubblewrap / firejail / Docker / sandbox-exec /
    /// nothing).
    pub sandbox: Option<SandboxConfig>,
    /// Phase 191 — injected sink for unprompted `DispatchNotification`
    /// frames (see `NotificationSink`). `None` for every tool process
    /// that doesn't need it (the default for all existing callers).
    pub notification_sink: Option<Arc<dyn NotificationSink>>,
}

impl std::fmt::Debug for ToolProcessConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolProcessConfig")
            .field("name", &self.name)
            .field("command", &self.command)
            .field("args", &self.args)
            .field("env", &self.env)
            .field("sandbox", &self.sandbox)
            .field("notification_sink", &self.notification_sink.is_some())
            .finish()
    }
}

/// Phase 52 — generic command wrapper around a tool process spawn.
///
/// The wrapper is responsible for setting up isolation (mount
/// namespaces, network namespaces, seccomp filters, etc.) and
/// then `exec`'ing the real command. Standard sandbox tools all
/// support this `wrapper [wrapper-args...] command [command-args...]`
/// shape natively — see `docs/TOOL_SDK.md` §9 for worked examples.
///
/// The wrapper must:
/// 1. Pass stdin/stdout/stderr through to the wrapped command
///    unchanged (the tool IPC protocol uses stdio).
/// 2. Forward signals so `kill_on_drop` can clean up the whole
///    chain. Most sandbox tools do this by default; verify per
///    tool.
/// 3. Not buffer or transform the tool's I/O.
#[derive(Debug, Clone)]
pub struct SandboxConfig {
    pub wrapper: String,
    pub args: Vec<String>,
}

/// Daemon-side bridge to a spawned tool process. One bridge per
/// `[[tool_process]]` config entry.
///
/// On `spawn`, the child is started, `ToolHello` is written to
/// its stdin, and `ToolRegister` is read from its stdout. The
/// resulting bridge holds the descriptors and a writer half;
/// invocations are dispatched via `invoke`, each waiting for a
/// matching `ToolResult` / `ToolError` on the shared reader.
///
/// The shared reader runs as a background task to allow
/// concurrent invocations (Amendment A6 — parallel tool dispatch).
pub struct ToolProcessBridge {
    config: ToolProcessConfig,
    descriptors: Vec<ToolDescriptor>,
    stdin: Arc<Mutex<ChildStdin>>,
    pending: Arc<Mutex<HashMap<String, mpsc::UnboundedSender<BridgeMessage>>>>,
    _child: Child,
    _reader_handle: tokio::task::JoinHandle<()>,
}

impl std::fmt::Debug for ToolProcessBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolProcessBridge")
            .field("config", &self.config)
            .field("descriptors", &self.descriptors)
            .finish()
    }
}

impl ToolProcessBridge {
    /// Spawn the configured tool process, perform the handshake,
    /// and return a bridge ready for `invoke`.
    pub async fn spawn(config: ToolProcessConfig) -> Result<Self, ToolBridgeError> {
        // Phase 52 — when a sandbox wrapper is configured, the
        // effective spawn shape is
        // `wrapper wrapper_args... command command_args...`.
        // The wrapper is responsible for isolation; we just thread
        // stdio through unchanged.
        let mut cmd = match &config.sandbox {
            Some(sandbox) => {
                let mut c = Command::new(&sandbox.wrapper);
                c.args(&sandbox.args);
                c.arg(&config.command);
                c.args(&config.args);
                c
            }
            None => {
                let mut c = Command::new(&config.command);
                c.args(&config.args);
                c
            }
        };
        // Task 13 (MEDIUM, 2026-09-16 audit) — clear the inherited
        // environment before applying the operator-configured `env`
        // map. Previously `cmd.envs(...)` only *added* to whatever
        // the daemon process itself was running with, so every tool
        // process (Gmail, Notion, n8n, ...) saw the daemon's entire
        // environment: LLM API keys, the `daemon.env` passphrase
        // variable, channel bot tokens — none of which that specific
        // tool process has any business seeing.
        //
        // `env_clear()` also wipes `PATH`, which matters here: both
        // `config.command` and `sandbox.wrapper` above are routinely
        // bare executable names (see this file's own tests — `"python3"`,
        // not an absolute path), resolved via `PATH` lookup at spawn
        // time. Per `std::process::Command`'s documented behavior,
        // that lookup uses the *child's* configured `PATH` (the one
        // set on this `Command`), not the parent's raw environment —
        // so without explicitly re-adding it here, clearing the
        // environment would break every non-absolute-path tool-process
        // spawn outright. We re-add the daemon's own inherited `PATH`
        // (not the tool process's — the daemon is the one resolving
        // `config.command`/`sandbox.wrapper` by name), which is not a
        // secret and is required for the child to exist at all.
        //
        // `HOME` gets the same treatment for a different reason: it's
        // not needed for spawning, but every first-party Google-OAuth-
        // backed tool process (aivyx-gmail, aivyx-calendar, aivyx-drive,
        // aivyx-contacts, ...) resolves its OAuth config/token storage
        // path unconditionally from `$HOME` (e.g.
        // `aivyx-gmail/src/auth_cli/config_file.rs::default_config_path`)
        // with no override mechanism — an empty `HOME` doesn't fail
        // safe, it makes the tool process unable to find its own
        // config file at all. This is existing, load-bearing behavior
        // in this codebase, not a hypothetical; re-adding it belongs
        // in this minimal set rather than becoming a silent regression
        // operators would have to work around via `config.env` on
        // every Google-integration entry. No locale variable
        // (`LANG`/`LC_ALL`/...) is read anywhere in the tool-process
        // crates (checked), so nothing else goes in this list —
        // anything else a specific tool process needs is the
        // operator's job to set via that process's `[[tool_process]]
        // .env` entry.
        cmd.env_clear();
        if let Ok(path) = std::env::var("PATH") {
            cmd.env("PATH", path);
        }
        if let Ok(home) = std::env::var("HOME") {
            cmd.env("HOME", home);
        }
        cmd.envs(config.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // SIGKILL the tool process if the bridge is dropped (or
            // the daemon panics) so we don't leave orphans behind.
            .kill_on_drop(true);

        let mut child = cmd.spawn().map_err(|e| ToolBridgeError::Spawn {
            // Report the actual program that failed to spawn —
            // when sandboxed, that's the wrapper (e.g., bwrap
            // not on PATH), not the wrapped command. Surfacing
            // the wrapper name lets operators diagnose missing
            // sandbox tools quickly.
            command: match &config.sandbox {
                Some(sandbox) => sandbox.wrapper.clone(),
                None => config.command.clone(),
            },
            source: e,
        })?;

        let stdin = child.stdin.take().expect("stdin piped");
        let stdout = child.stdout.take().expect("stdout piped");

        let stdin = Arc::new(Mutex::new(stdin));
        let pending: Arc<
            Mutex<HashMap<String, mpsc::UnboundedSender<BridgeMessage>>>,
        > = Arc::new(Mutex::new(HashMap::new()));

        // Send ToolHello.
        {
            let mut guard = stdin.lock().await;
            write_frame(
                &mut *guard,
                &DaemonToTool::ToolHello {
                    protocol_version: TOOL_PROTOCOL_VERSION.into(),
                },
            )
            .await?;
        }

        // Read ToolRegister. This blocks startup — first frame must
        // be ToolRegister; anything else is a protocol violation.
        let mut reader = BufReader::new(stdout);
        let body = read_frame(&mut reader)
            .await?
            .ok_or(ToolBridgeError::HandshakeClosed)?;
        let msg: ToolToDaemon = serde_json::from_str(&body)
            .map_err(|e| ToolBridgeError::Decode(format!("ToolRegister: {e}")))?;
        let descriptors = match msg {
            ToolToDaemon::ToolRegister { tools, .. } => tools,
            other => return Err(ToolBridgeError::HandshakeUnexpected(other)),
        };

        // Spawn the background reader that demultiplexes responses
        // by call_id.
        let pending_for_reader = Arc::clone(&pending);
        let notification_sink = config.notification_sink.clone();
        let reader_handle = tokio::spawn(async move {
            reader_loop(reader, pending_for_reader, notification_sink).await;
        });

        Ok(ToolProcessBridge {
            config,
            descriptors,
            stdin,
            pending,
            _child: child,
            _reader_handle: reader_handle,
        })
    }

    /// The tools this process registered.
    pub fn descriptors(&self) -> &[ToolDescriptor] {
        &self.descriptors
    }

    /// Configuration this bridge was spawned with.
    pub fn config(&self) -> &ToolProcessConfig {
        &self.config
    }

    /// Send an `InvokeTool` and await the matching `ToolResult` or
    /// `ToolError`. Concurrent calls are safe — the bridge
    /// demultiplexes responses by `call_id`. Any `ToolEvent`
    /// frames the tool emits mid-invocation are silently absorbed;
    /// use [`Self::invoke_with_events`] to surface them.
    ///
    /// Generates a fresh UUID `call_id` per call. Callers that need
    /// to send a targeted `CancelInvocation` while the invocation is
    /// in flight should use [`Self::invoke_with_events`] and pass
    /// their own `call_id`.
    pub async fn invoke(
        &self,
        tool_name: &str,
        input: serde_json::Value,
        turn_id: &str,
    ) -> Result<InvocationOutcome, ToolBridgeError> {
        let call_id = uuid::Uuid::new_v4().to_string();
        self.invoke_with_events(&call_id, tool_name, input, turn_id, |_| {})
            .await
    }

    /// Phase 50 — full-fidelity invocation. `call_id` is
    /// caller-supplied so cancellation can be targeted from outside
    /// (e.g., when a turn-loop cancellation token fires). Every
    /// `ToolEventPayload` the tool emits mid-invocation is delivered
    /// to `on_event` in order, before the terminal `InvocationOutcome`
    /// returns.
    ///
    /// **Concurrency:** safe with other invocations on the same
    /// bridge — each call_id has its own mpsc lane.
    pub async fn invoke_with_events<F>(
        &self,
        call_id: &str,
        tool_name: &str,
        input: serde_json::Value,
        turn_id: &str,
        mut on_event: F,
    ) -> Result<InvocationOutcome, ToolBridgeError>
    where
        F: FnMut(ToolEventPayload),
    {
        // Per-call mpsc; the reader_loop writes Events + the terminal
        // Outcome into it in order. We drain until we see Outcome.
        let (tx, mut rx) = mpsc::unbounded_channel::<BridgeMessage>();
        {
            let mut pending = self.pending.lock().await;
            pending.insert(call_id.to_string(), tx);
        }

        // Send the invocation.
        {
            let mut guard = self.stdin.lock().await;
            let frame = DaemonToTool::InvokeTool {
                call_id: call_id.to_string(),
                tool_name: tool_name.into(),
                input,
                turn_id: turn_id.into(),
            };
            if let Err(e) = write_frame(&mut *guard, &frame).await {
                self.pending.lock().await.remove(call_id);
                return Err(e.into());
            }
        }

        loop {
            match rx.recv().await {
                Some(BridgeMessage::Event(ev)) => on_event(ev),
                Some(BridgeMessage::Outcome(o)) => return Ok(o),
                None => return Err(ToolBridgeError::InvocationClosed),
            }
        }
    }

    /// Send `CancelInvocation` for a specific call_id. Best-effort:
    /// the tool process is expected to respond promptly with a
    /// `ToolError { code: "cancelled" }`.
    pub async fn cancel(&self, call_id: &str) -> Result<(), ToolBridgeError> {
        let mut guard = self.stdin.lock().await;
        write_frame(
            &mut *guard,
            &DaemonToTool::CancelInvocation {
                call_id: call_id.into(),
            },
        )
        .await?;
        Ok(())
    }

    /// Send `ToolShutdown` to the child. Caller should wait briefly
    /// after this — the child exits cleanly on receipt; if not,
    /// `kill_on_drop` SIGKILLs when the bridge drops.
    pub async fn shutdown(&self) -> Result<(), ToolBridgeError> {
        let mut guard = self.stdin.lock().await;
        write_frame(&mut *guard, &DaemonToTool::ToolShutdown).await?;
        Ok(())
    }
}

/// Background reader loop — demultiplexes `ToolToDaemon` frames
/// into per-call_id mpsc senders.
///
/// Phase 50 — events and terminal outcomes share the same lane
/// (`BridgeMessage`). The terminal outcome variants
/// (`ToolResult` / `ToolError`) also drop the pending entry so a
/// follow-up `CancelInvocation` on that call_id is a no-op.
async fn reader_loop(
    mut reader: BufReader<ChildStdout>,
    pending: Arc<Mutex<HashMap<String, mpsc::UnboundedSender<BridgeMessage>>>>,
    notification_sink: Option<Arc<dyn NotificationSink>>,
) {
    loop {
        let body = match read_frame(&mut reader).await {
            Ok(Some(b)) => b,
            Ok(None) => {
                // Clean EOF — child closed stdout. Drop all
                // pending so callers wake with InvocationClosed.
                pending.lock().await.clear();
                return;
            }
            Err(_) => {
                pending.lock().await.clear();
                return;
            }
        };

        // Decode permissively — unknown variants are skipped, not
        // fatal (per the v0 stability disclaimer in TOOL_SDK.md
        // § 7).
        let msg: ToolToDaemon = match serde_json::from_str(&body) {
            Ok(m) => m,
            Err(_) => continue,
        };

        match msg {
            ToolToDaemon::ToolResult {
                call_id,
                verified,
                output,
            } => {
                if let Some(tx) = pending.lock().await.remove(&call_id) {
                    let _ = tx.send(BridgeMessage::Outcome(
                        InvocationOutcome::Completed { verified, output },
                    ));
                }
            }
            ToolToDaemon::ToolError {
                call_id,
                code,
                message,
            } => {
                if let Some(tx) = pending.lock().await.remove(&call_id) {
                    let _ = tx.send(BridgeMessage::Outcome(
                        InvocationOutcome::ToolError { code, message },
                    ));
                }
            }
            ToolToDaemon::RequiresEscalation { call_id, reason } => {
                if let Some(tx) = pending.lock().await.remove(&call_id) {
                    let _ = tx.send(BridgeMessage::Outcome(
                        InvocationOutcome::RequiresEscalation { reason },
                    ));
                }
            }
            ToolToDaemon::ToolEvent { call_id, event } => {
                // Phase 50 — relay the event on the per-call mpsc
                // so `invoke_with_events` can forward it to the
                // caller (a `ToolProxy` typically routes it onto
                // `ChannelContext::stream_event`). If the pending
                // slot is gone (terminal already delivered, or the
                // caller dropped), drop the event silently.
                let pending = pending.lock().await;
                if let Some(tx) = pending.get(&call_id) {
                    let _ = tx.send(BridgeMessage::Event(event));
                }
            }
            ToolToDaemon::ToolRegister { .. } => {
                // Spurious — the handshake already consumed this. Ignore.
            }
            ToolToDaemon::DispatchNotification {
                target,
                message,
                subject,
            } => {
                // No call_id — this doesn't go through `pending` at
                // all, unlike every other variant in this match.
                if let Some(sink) = &notification_sink {
                    sink.dispatch(target, message, subject);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spawn an `awk` one-liner that acts as a minimal tool
    /// process. Verifies the bridge can spawn, handshake, and
    /// dispatch — no external dependencies beyond a standard
    /// awk in PATH.
    ///
    /// (The full Python reference is in
    /// `examples/python-tool/`; this test is the Rust-side
    /// shim that proves the wire works.)
    #[tokio::test]
    async fn bridge_handshakes_against_python_inline() {
        // Spawn a tiny Python script inline.
        let script = r#"
import sys, json, struct

def read_frame():
    hdr = sys.stdin.buffer.read(4)
    if not hdr or len(hdr) < 4:
        return None
    (n,) = struct.unpack(">I", hdr)
    return json.loads(sys.stdin.buffer.read(n).decode("utf-8"))

def write_frame(msg):
    body = json.dumps(msg).encode("utf-8")
    sys.stdout.buffer.write(struct.pack(">I", len(body)) + body)
    sys.stdout.buffer.flush()

# Handshake
hello = read_frame()
assert hello["type"] == "ToolHello"
write_frame({
    "type": "ToolRegister",
    "tool_process_name": "test-tool",
    "tools": [{
        "name": "echo",
        "description": "Echo the input.",
        "input_schema": {"type": "object"},
        "required_scope": "memory.read"
    }]
})

# Service one invocation, then exit.
inv = read_frame()
assert inv["type"] == "InvokeTool"
write_frame({
    "type": "ToolResult",
    "call_id": inv["call_id"],
    "verified": "NotApplicable",
    "output": {"echoed": inv["input"]}
})
sys.exit(0)
"#;
        let config = ToolProcessConfig {
            name: "test".into(),
            command: "python3".into(),
            args: vec!["-c".into(), script.into()],
            env: vec![],
            sandbox: None,
            notification_sink: None,
        };
        let bridge = match ToolProcessBridge::spawn(config).await {
            Ok(b) => b,
            Err(e) => {
                // Python may not be present in some CI environments;
                // skip rather than fail.
                eprintln!("skipping: python3 unavailable: {e}");
                return;
            }
        };

        assert_eq!(bridge.descriptors().len(), 1);
        assert_eq!(bridge.descriptors()[0].name, "echo");

        let outcome = bridge
            .invoke("echo", serde_json::json!({"hello": "world"}), "turn-1")
            .await
            .expect("invoke must succeed");

        match outcome {
            InvocationOutcome::Completed { verified, output } => {
                assert_eq!(verified, crate::wire::Verification::NotApplicable);
                assert_eq!(output["echoed"]["hello"], "world");
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn bridge_routes_dispatch_notification_with_no_call_id() {
        let script = r#"
import sys, json, struct

def read_frame():
    hdr = sys.stdin.buffer.read(4)
    if not hdr or len(hdr) < 4:
        return None
    (n,) = struct.unpack(">I", hdr)
    return json.loads(sys.stdin.buffer.read(n).decode("utf-8"))

def write_frame(msg):
    body = json.dumps(msg).encode("utf-8")
    sys.stdout.buffer.write(struct.pack(">I", len(body)) + body)
    sys.stdout.buffer.flush()

hello = read_frame()
assert hello["type"] == "ToolHello"
write_frame({
    "type": "ToolRegister",
    "tool_process_name": "test-tool",
    "tools": []
})

# Unprompted — no InvokeTool preceded this, no call_id at all.
write_frame({
    "type": "DispatchNotification",
    "target": "phone",
    "message": "watcher x went down",
    "subject": None
})
sys.exit(0)
"#;
        // A plain `std::sync::Mutex`, not tokio's — `dispatch` is
        // called synchronously from inside the reader loop's async
        // task, and `tokio::sync::Mutex::blocking_lock()` panics
        // when called from within an asynchronous execution context
        // (it's meant for blocking threads calling into async code,
        // not the reverse). A std mutex held only across a
        // non-blocking `push` is the right tool here.
        type RecordedCalls = Vec<(String, String, Option<String>)>;
        let recorded: Arc<std::sync::Mutex<RecordedCalls>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));

        struct TestSink(Arc<std::sync::Mutex<RecordedCalls>>);
        impl NotificationSink for TestSink {
            fn dispatch(&self, target: String, message: String, subject: Option<String>) {
                self.0.lock().unwrap().push((target, message, subject));
            }
        }

        let config = ToolProcessConfig {
            name: "test".into(),
            command: "python3".into(),
            args: vec!["-c".into(), script.into()],
            env: vec![],
            sandbox: None,
            notification_sink: Some(Arc::new(TestSink(Arc::clone(&recorded)))),
        };
        let bridge = match ToolProcessBridge::spawn(config).await {
            Ok(b) => b,
            Err(e) => {
                eprintln!("skipping: python3 unavailable: {e}");
                return;
            }
        };
        assert_eq!(bridge.descriptors().len(), 0);

        // Give the reader loop a moment to process the frame the
        // Python script sent immediately after ToolRegister.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let calls = recorded.lock().unwrap();
        assert_eq!(
            calls.len(),
            1,
            "expected exactly one DispatchNotification routed"
        );
        assert_eq!(calls[0].0, "phone");
        assert_eq!(calls[0].1, "watcher x went down");
        assert_eq!(calls[0].2, None);
    }

    #[tokio::test]
    async fn bridge_reports_handshake_failure_on_bad_command() {
        let config = ToolProcessConfig {
            name: "bad".into(),
            command: "/definitely/not/a/real/binary".into(),
            args: vec![],
            env: vec![],
            sandbox: None,
            notification_sink: None,
        };
        let result = ToolProcessBridge::spawn(config).await;
        assert!(matches!(result, Err(ToolBridgeError::Spawn { .. })));
    }

    #[tokio::test]
    async fn bridge_surfaces_tool_error() {
        // Inline Python script that returns ToolError instead of
        // ToolResult.
        let script = r#"
import sys, json, struct

def read_frame():
    hdr = sys.stdin.buffer.read(4)
    if not hdr or len(hdr) < 4:
        return None
    (n,) = struct.unpack(">I", hdr)
    return json.loads(sys.stdin.buffer.read(n).decode("utf-8"))

def write_frame(msg):
    body = json.dumps(msg).encode("utf-8")
    sys.stdout.buffer.write(struct.pack(">I", len(body)) + body)
    sys.stdout.buffer.flush()

read_frame()  # ToolHello
write_frame({"type":"ToolRegister","tool_process_name":"err-tool","tools":[
    {"name":"oops","description":"always fails.","input_schema":{},"required_scope":"memory.read"}
]})
inv = read_frame()
write_frame({"type":"ToolError","call_id":inv["call_id"],
             "code":"deliberate","message":"this fails on purpose"})
sys.exit(0)
"#;
        let config = ToolProcessConfig {
            name: "err".into(),
            command: "python3".into(),
            args: vec!["-c".into(), script.into()],
            env: vec![],
            sandbox: None,
            notification_sink: None,
        };
        let bridge = match ToolProcessBridge::spawn(config).await {
            Ok(b) => b,
            Err(_) => return,
        };

        let outcome = bridge
            .invoke("oops", serde_json::json!({}), "turn-1")
            .await
            .expect("invoke must succeed");

        match outcome {
            InvocationOutcome::ToolError { code, message } => {
                assert_eq!(code, "deliberate");
                assert!(message.contains("on purpose"));
            }
            other => panic!("expected ToolError, got {other:?}"),
        }
    }

    // ---- Phase 52 — sandbox wrapper ----

    #[tokio::test]
    async fn sandbox_wrapper_failure_reports_wrapper_name() {
        // When the wrapper itself fails to spawn (e.g., bwrap not
        // on PATH), the operator-facing error must name the
        // *wrapper* — not the wrapped command — so they can
        // diagnose the missing sandbox tool quickly.
        let config = ToolProcessConfig {
            name: "wrapped".into(),
            command: "python3".into(),
            args: vec![],
            env: vec![],
            sandbox: Some(SandboxConfig {
                wrapper: "/definitely/not/a/real/sandbox-binary".into(),
                args: vec!["--isolated".into()],
            }),
            notification_sink: None,
        };
        let err = ToolProcessBridge::spawn(config)
            .await
            .expect_err("missing wrapper must error at spawn");
        match err {
            ToolBridgeError::Spawn { command, .. } => {
                assert!(
                    command.contains("sandbox-binary"),
                    "error must name the wrapper, not the wrapped command — got {command:?}",
                );
            }
            other => panic!("expected ToolBridgeError::Spawn, got {other:?}"),
        }
    }

    // ---- Task 13 (MEDIUM, 2026-09-16 audit) — environment scrubbing ----

    #[tokio::test]
    async fn tool_process_does_not_inherit_an_unrelated_secret_env_var() {
        // SAFETY: no other test in this process reads or writes this
        // specific var name, so the mutation can't race a concurrent
        // reader of it.
        unsafe {
            std::env::set_var("AIVYX_TEST_SECRET_SHOULD_NOT_LEAK", "leaked-value");
        }

        // A tool process that follows the real handshake protocol,
        // then reports back (via its one tool's output) whatever it
        // sees for the secret var. If `spawn()` ever stops clearing
        // the environment before applying `config.env`, this comes
        // back as "leaked-value" instead of "absent".
        let script = r#"
import sys, json, struct, os

def read_frame():
    hdr = sys.stdin.buffer.read(4)
    if not hdr or len(hdr) < 4:
        return None
    (n,) = struct.unpack(">I", hdr)
    return json.loads(sys.stdin.buffer.read(n).decode("utf-8"))

def write_frame(msg):
    body = json.dumps(msg).encode("utf-8")
    sys.stdout.buffer.write(struct.pack(">I", len(body)) + body)
    sys.stdout.buffer.flush()

hello = read_frame()
assert hello["type"] == "ToolHello"
write_frame({
    "type": "ToolRegister",
    "tool_process_name": "env-check-tool",
    "tools": [{
        "name": "check_env",
        "description": "Report the secret env var, if visible.",
        "input_schema": {"type": "object"},
        "required_scope": "memory.read"
    }]
})

inv = read_frame()
assert inv["type"] == "InvokeTool"
write_frame({
    "type": "ToolResult",
    "call_id": inv["call_id"],
    "verified": "NotApplicable",
    "output": {
        "secret": os.environ.get("AIVYX_TEST_SECRET_SHOULD_NOT_LEAK", "absent"),
        "path_present": "PATH" in os.environ,
    }
})
sys.exit(0)
"#;
        let config = ToolProcessConfig {
            name: "env-check".into(),
            command: "python3".into(),
            args: vec!["-c".into(), script.into()],
            // Deliberately empty — the operator configured nothing
            // for this tool process, so it should see nothing beyond
            // the minimal PATH/HOME re-add.
            env: vec![],
            sandbox: None,
            notification_sink: None,
        };
        let bridge = match ToolProcessBridge::spawn(config).await {
            Ok(b) => b,
            Err(e) => {
                eprintln!("skipping: python3 unavailable: {e}");
                unsafe {
                    std::env::remove_var("AIVYX_TEST_SECRET_SHOULD_NOT_LEAK");
                }
                return;
            }
        };

        let outcome = bridge
            .invoke("check_env", serde_json::json!({}), "turn-1")
            .await
            .expect("invoke must succeed");

        unsafe {
            std::env::remove_var("AIVYX_TEST_SECRET_SHOULD_NOT_LEAK");
        }

        match outcome {
            InvocationOutcome::Completed { output, .. } => {
                assert_eq!(
                    output["secret"], "absent",
                    "tool process must not inherit the daemon's unrelated env vars"
                );
                // PATH must still be present — a fully-cleared
                // environment with no PATH re-add would have broken
                // this very spawn (python3 is a bare name, resolved
                // via PATH), so getting this far already partially
                // proves it, but assert explicitly for clarity.
                assert_eq!(
                    output["path_present"], true,
                    "PATH must be explicitly re-added after env_clear()"
                );
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }
}
