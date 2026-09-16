//! `ShellExecTool` — Phase 11 Task 3, the first shell-execution tool.
//!
//! ## What it does
//!
//! Executes a shell command under a pre-canonicalized sandbox root
//! (a `cwd_root`) via `tokio::process::Command`. The agent must
//! hold a scope like `shell.exec:cwd:<cwd_root>/**` — the capability
//! layer's existing path-glob qualifier matching attenuates per-
//! path (any `/` in a qualifier string triggers `QualifierKind::
//! PathGlob`), so a held `shell.exec:cwd:/repo/**` grants
//! `shell.exec:cwd:/repo/sub` but not `shell.exec:cwd:/etc`. The
//! `cwd:` qualifier prefix is the Phase 11 Task 3 convention; it
//! distinguishes the path-attenuation shape from the program-
//! allowlist shape (`shell.exec:git,ls,cat`) that the capability
//! layer also supports. Both coexist on the same base because the
//! dispatcher picks kind by the *shape* of the qualifier string,
//! not by the base name.
//!
//! Output is bundled: `{stdout: String, stderr: String, exit_code:
//! i32, timed_out: bool}`. No streaming, no PTY, no signal handling
//! beyond the wall-clock timeout. If the caller needs real-time
//! progress, they can call the tool with a short timeout in a loop
//! — the shell-exec streaming story is a later phase.
//!
//! ## Defense in depth
//!
//! Two independent layers, the same shape as `fs.read`:
//!
//! 1. **Lexical layer (`required_scope`).** Resolves the requested
//!    `args.cwd` (or the `cwd_root` default) lexically against the
//!    pre-canonicalized `cwd_root`. Produces
//!    `shell.exec:cwd:<lexical>` as the needed scope. A
//!    `args.cwd = /repo/../etc` input
//!    collapses to `/etc`, which does NOT start with `/repo`, so
//!    the lexical resolve returns `None` and the tool emits a
//!    deny scope. The loop's scope gate then denies the call and
//!    the planner sees `Denied` — never `Failed`, because the
//!    validator already passed.
//!
//! 2. **Canonical layer (`execute`).** Right before spawning the
//!    child, `execute` calls `std::fs::canonicalize` on the cwd.
//!    If the canonical path no longer lives under the canonical
//!    `cwd_root`, the tool returns `ToolOutcome::Failed` with a
//!    "escapes sandbox" detail. This catches symlink-based
//!    traversal (`/repo/escape -> /etc`) that the lexical layer
//!    cannot see.
//!
//! 3. **Sensitive-path guard (Chapters Ward/Portcullis).** Before
//!    spawning `sh`, the command text is scanned for references to
//!    protected locations (`~/.ssh`, `.env`, cloud creds, and
//!    persistence targets like `.bashrc`/`authorized_keys`/`crontab`).
//!    A hit is refused, closing the residual that `cat ~/.ssh/id_rsa`
//!    and `echo >> ~/.bashrc` route around the fs-tool guards. Disabled
//!    by default; best-effort (obfuscated paths are out of scope — see
//!    `sensitive_command_hit`).
//!
//! ## Trust-tier gate
//!
//! `shell.exec` is **not** in the SemiTrusted ceiling — see
//! `aivyx-capability::ceilings::CEILING_SEMITRUSTED`. That means
//! even if a SemiTrusted channel (e.g. Telegram) somehow has the
//! tool in its registry, every call gets stripped at dispatch by
//! the ceiling intersection. The binary's Phase 11 Task 3 stance
//! is stricter still: `aivyx.rs` registers the tool **only** in
//! the `ChannelKind::Local` branch, so a SemiTrusted channel
//! never sees `shell.exec` at all — not even as a denial in the
//! audit chain. Registration-time gate plus dispatch-time ceiling
//! = defense in depth.
//!
//! ## What this tool deliberately does NOT do
//!
//! - No interactive shells, no PTY. These are deferred.
//! - No streaming of stdout/stderr as `StreamEvent::ToolOutput`.
//!   Output is bundled into one `Completed` result — keeps the
//!   `lib.rs` streak baseline stable and lets Task 4's planner
//!   wiring treat shell.exec identically to every other tool.
//!
//! ## Phase 42 additions
//!
//! - **Process-group execution.** `process_group(0)` puts `sh -c`
//!   and all grandchildren under one PGID. On timeout, the tool
//!   SIGTERMs the group, waits 2s, then SIGKILLs. Prevents zombie
//!   grandchildren from outliving the turn.
//! - **Environment isolation.** `env_clear()` strips the daemon's
//!   env (including secrets like `ANTHROPIC_API_KEY`). Only safe
//!   defaults (`PATH`, `HOME`, `USER`, `LANG`, `TERM`) plus
//!   explicitly declared `env` vars are injected.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::process::Command;

use aivyx_capability::Scope;

use crate::{
    AivyxError, ExecutionConfiner, Tool, ToolContext, ToolId, ToolOutcome, Verification,
    default_confiner,
};

/// Default wall-clock timeout for a single `shell.exec` invocation.
/// 30 seconds is generous enough for most `cargo check`-style
/// commands on a mid-size project and stringent enough that a
/// runaway loop doesn't stall the turn's 120-second budget. Const,
/// not config knob — the per-call `args.timeout_ms` field lets
/// callers override upward, capped at [`MAX_TIMEOUT_MS`].
pub const DEFAULT_TIMEOUT_MS: u64 = 30_000;

/// Hard upper bound on a single `shell.exec` timeout. 10 minutes.
/// Anything longer than this would blow past the turn budget
/// anyway, so rejecting it at the schema level produces a clearer
/// error message than waiting for the turn-level timeout to fire.
pub const MAX_TIMEOUT_MS: u64 = 600_000;

/// Construction inputs for [`ShellExecTool`]. Split from the tool
/// itself so the fallible `canonicalize()` call on the sandbox root
/// happens at `build()` time and the resulting `ShellExecTool` is
/// infallible to construct — same split `fs.read` uses.
pub struct ShellExecToolConfig {
    cwd_root: PathBuf,
}

impl ShellExecToolConfig {
    pub fn new(cwd_root: impl Into<PathBuf>) -> Self {
        ShellExecToolConfig {
            cwd_root: cwd_root.into(),
        }
    }

    /// Canonicalize the cwd sandbox root and return a ready-to-
    /// register [`ShellExecTool`]. Fails if the root doesn't exist
    /// or isn't a directory — those are configuration errors the
    /// caller needs to see at startup, not at tool-call time.
    pub fn build(self) -> Result<ShellExecTool, AivyxError> {
        let canonical = std::fs::canonicalize(&self.cwd_root).map_err(|e| {
            AivyxError::Config(format!(
                "shell.exec cwd_root {:?} cannot be canonicalized: {e}",
                self.cwd_root
            ))
        })?;
        if !canonical.is_dir() {
            return Err(AivyxError::Config(format!(
                "shell.exec cwd_root {canonical:?} is not a directory"
            )));
        }
        let confiner = default_confiner(&canonical, &[], &[], true);
        Ok(ShellExecTool {
            id: ToolId::new(),
            cwd_root: Arc::from(canonical),
            schema: shell_exec_input_schema_value(),
            // Default disabled ⇒ byte-identical to pre-guard behavior until the
            // binary installs a real policy (mirrors `fs.read`/`fs.write`).
            sensitive: Arc::new(crate::sensitive_paths::SensitivePolicy::disabled()),
            confiner,
        })
    }
}

/// Reference shell-execution tool. Agents holding
/// `shell.exec:cwd:<cwd_root>/**` can run any command from any
/// directory under `cwd_root` with a ≤10-minute timeout.
pub struct ShellExecTool {
    id: ToolId,
    /// Pre-canonicalized absolute path. `Arc<Path>` for the same
    /// reason `FsReadTool::sandbox_root` uses it — cheap shared
    /// ownership across concurrent turn tasks.
    cwd_root: Arc<Path>,
    schema: Value,
    /// Chapters Ward/Portcullis, extended to `shell.exec` — the
    /// sensitive-path guard (disabled by default). The fs tools guard
    /// their own path; a shell command bypasses them entirely, so this
    /// scans the command string for references to protected locations.
    sensitive: Arc<crate::sensitive_paths::SensitivePolicy>,
    /// OS-level process confinement (Landlock + seccomp-bpf, via
    /// `aivyx-confine`) — the kernel-level counterpart to `sensitive`'s
    /// string-level guard above. Built as a real, on-by-default confiner
    /// at `build()` time so every caller gets it even if they never call
    /// `with_confiner` explicitly (mirrors `sensitive`'s own "default
    /// disabled ⇒ safe" shape, but inverted: this one defaults ON).
    confiner: Arc<dyn ExecutionConfiner>,
}

// Hand-rolled (not `#[derive(Debug)]`): `dyn ExecutionConfiner` has no
// `Debug` impl (it's a plain confine-a-Command trait, not a data type
// worth formatting), so the field is named as a placeholder instead of
// derived away entirely.
impl std::fmt::Debug for ShellExecTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellExecTool")
            .field("id", &self.id)
            .field("cwd_root", &self.cwd_root)
            .field("schema", &self.schema)
            .field("sensitive", &self.sensitive)
            .field("confiner", &"<dyn ExecutionConfiner>")
            .finish()
    }
}

impl ShellExecTool {
    /// Expose the canonicalized cwd sandbox root. Used by the
    /// binary's startup banner and by tests.
    pub fn cwd_root(&self) -> &Path {
        &self.cwd_root
    }

    /// Install the sensitive-path guard. When enabled, a command whose
    /// text references a protected location (`~/.ssh`, `.env`, cloud
    /// creds, or a persistence target like `.bashrc` / `authorized_keys`
    /// / `crontab`) is refused before `sh -c` ever runs — closing the
    /// documented residual that `cat ~/.ssh/id_rsa` and `echo >> ~/.bashrc`
    /// route around Ward/Portcullis. Best-effort by nature (a shell can
    /// obfuscate paths); it raises the bar on the obvious cases and the
    /// docs still point to OS-level isolation for hard guarantees.
    pub fn with_sensitive_policy(
        mut self,
        policy: Arc<crate::sensitive_paths::SensitivePolicy>,
    ) -> Self {
        self.sensitive = policy;
        self
    }

    /// Override the confiner `build()` set by default. The real binary
    /// call site uses this to pass the operator's configured
    /// `require_enforcement` value instead of the hardcoded `true`
    /// `build()` itself uses.
    pub fn with_confiner(mut self, confiner: Arc<dyn ExecutionConfiner>) -> Self {
        self.confiner = confiner;
        self
    }
}

/// Best-effort scan of a shell command string for a reference to a
/// protected location. Returns `Some((token, reason))` for the first hit.
///
/// This is deliberately conservative-but-simple: it splits the command on
/// shell word/redirect/pipe separators, unquotes each token, expands a
/// leading `~` and `$HOME`/`${HOME}` against the real home dir, and runs
/// every candidate through [`SensitivePolicy::classify_write`] — the
/// *superset* check (read-sensitive ∪ persistence), because a shell command
/// can both read and write and we cannot tell which from the text. The
/// classifier matches on path *components*, so `~/.ssh/id_rsa`, an absolute
/// `/home/u/.aws/credentials`, and a bare `id_rsa` all trip the same rules
/// the fs tools enforce. Obfuscation (base64, `$(printf …)`, hex escapes) is
/// out of scope by design — see the tool doc-comment.
fn sensitive_command_hit(
    cmd: &str,
    policy: &crate::sensitive_paths::SensitivePolicy,
) -> Option<(String, String)> {
    let home = std::env::var("HOME").ok();
    for raw in cmd.split(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                '|' | '&' | ';' | '<' | '>' | '(' | ')' | '`' | '"' | '\'' | '='
            )
    }) {
        if raw.is_empty() {
            continue;
        }
        // Expand a leading `~` and any `$HOME` / `${HOME}` so allow-listed
        // absolute paths are honored and matching lands on an absolute path.
        let mut tok = raw.to_string();
        if let Some(h) = &home {
            if let Some(rest) = tok.strip_prefix("~/") {
                tok = format!("{h}/{rest}");
            } else if tok == "~" {
                tok = h.clone();
            }
            tok = tok.replace("${HOME}", h).replace("$HOME", h);
        }
        if let Some(reason) = policy.classify_write(Path::new(&tok)) {
            return Some((raw.to_string(), reason));
        }
    }
    None
}

/// Lexically resolve `input_cwd` against `cwd_root`. Mirrors
/// `tools::fs::lexical_resolve` — the two tools share this
/// identical path-collapse shape but the implementations are
/// kept in separate files so that a future divergence (e.g.
/// shell.exec wanting a per-command sub-root) doesn't need to
/// push back up into `fs`'s helper.
///
/// Returns `None` if the resolution escapes the sandbox root.
fn lexical_resolve(cwd_root: &Path, input_cwd: &Path) -> Option<PathBuf> {
    let mut joined = PathBuf::from(cwd_root);
    joined.push(input_cwd);

    let root_components: Vec<Component<'_>> = cwd_root.components().collect();
    let mut stack: Vec<Component<'_>> = Vec::with_capacity(16);
    for comp in joined.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if stack.len() <= root_components.len() {
                    return None;
                }
                stack.pop();
            }
            other => stack.push(other),
        }
    }

    for (i, root_c) in root_components.iter().enumerate() {
        if stack.get(i) != Some(root_c) {
            return None;
        }
    }

    Some(stack.into_iter().collect())
}

/// A scope no real agent should ever hold. Returned by
/// `required_scope` when the input is malformed or escapes the
/// sandbox lexically. Same shape as `tools::fs`'s deny helper.
fn deny_scope() -> Scope {
    Scope::parse("shell.exec:cwd:/aivyx/__deny__/invalid-input").expect("deny scope must parse")
}

/// Build the advertised nested input schema for `shell.exec`. The
/// exact shape pins the Phase 11 Task 3 schema-extension contract:
/// validator nested-object support is exercised by this fixture
/// every time a shell.exec call runs through the loop.
/// Safe default environment variables injected into every
/// shell.exec invocation. These are the minimum set needed for
/// well-behaved Unix commands (locale, terminal, path lookup).
/// All other env vars from the daemon process are stripped via
/// `env_clear()`. Phase 42.
const SAFE_ENV_DEFAULTS: &[&str] = &["PATH", "HOME", "USER", "LANG", "TERM"];

fn shell_exec_input_schema_value() -> Value {
    json!({
        "type": "object",
        "properties": {
            "cmd": {
                "type": "string",
                "description": "Shell command to execute, passed to \
                                `sh -c`. Non-empty required. The \
                                tool does not interpret pipes/redirects \
                                itself — `sh` handles that."
            },
            "args": {
                "type": "object",
                "properties": {
                    "cwd": {
                        "type": "string",
                        "description": "Working directory for the \
                                        command. Must resolve under the \
                                        agent's shell.exec sandbox root. \
                                        Default: the sandbox root itself."
                    },
                    "timeout_ms": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_TIMEOUT_MS as i64,
                        "description": "Wall-clock timeout in milliseconds. \
                                        Default 30000; maximum 600000 \
                                        (10 minutes)."
                    },
                    "env": {
                        "type": "object",
                        "description": "Environment variables to set for \
                                        this command. Keys are variable \
                                        names, values are strings. The \
                                        daemon's own environment is NOT \
                                        inherited — only PATH, HOME, USER, \
                                        LANG, TERM are injected as defaults, \
                                        plus any vars declared here.",
                        "additionalProperties": { "type": "string" }
                    }
                },
                "additionalProperties": false
            }
        },
        "required": ["cmd"],
        "additionalProperties": false
    })
}

/// Pull the requested `cwd` from a pre-validated input. Missing
/// means "use the sandbox root itself" — the common case where
/// the LLM just says "run this command" without a directory.
fn input_cwd(input: &Value) -> Option<&str> {
    input.get("args")?.get("cwd")?.as_str()
}

/// Pull the requested `timeout_ms`, clamped to the hard ceiling.
/// Missing means [`DEFAULT_TIMEOUT_MS`]. The validator has already
/// enforced the minimum/maximum bounds from the schema, so the
/// clamp here is belt-and-suspenders for callers that bypass the
/// validator (unit tests that call `execute` directly).
fn input_timeout_ms(input: &Value) -> u64 {
    let ms = input
        .get("args")
        .and_then(|a| a.get("timeout_ms"))
        .and_then(Value::as_u64)
        .unwrap_or(DEFAULT_TIMEOUT_MS);
    ms.clamp(1, MAX_TIMEOUT_MS)
}

/// Extract the optional `env` object from the input. Returns an
/// empty map if missing or wrong type. Values that aren't strings
/// are silently skipped — the schema enforces string values, so
/// non-string values only appear if the caller bypasses validation.
fn input_env(input: &Value) -> Vec<(String, String)> {
    let Some(env_obj) = input
        .get("args")
        .and_then(|a| a.get("env"))
        .and_then(Value::as_object)
    else {
        return Vec::new();
    };
    env_obj
        .iter()
        .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
        .collect()
}

#[async_trait]
impl Tool for ShellExecTool {
    fn id(&self) -> ToolId {
        self.id
    }

    fn name(&self) -> &str {
        "shell.exec"
    }

    /// An arbitrary shell command can mutate anything under fs_root —
    /// checkpoint before every call, same as fs.write/fs.delete.
    fn mutates_fs_root(&self) -> bool {
        true
    }

    // Chapter Bulwark/Picket — a shell command's stdout/stderr can carry
    // attacker-authored content (e.g. `curl` output from a remote server)
    // with no operator review before it enters model context. Fence it as
    // untrusted data and run the injection scan over it, same as fs.read
    // and every tool-process proxy.
    fn output_is_untrusted(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "Run a shell command inside the agent's shell.exec sandbox \
         root and return its stdout, stderr, and exit code. The \
         `args.cwd` field (optional) picks a subdirectory of the \
         sandbox root; `args.timeout_ms` (optional, default 30000, \
         max 600000) sets a wall-clock timeout. Output is bundled \
         into one result — no streaming."
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn required_scope(&self, input: &Value) -> Scope {
        let Some(cmd) = input.get("cmd").and_then(Value::as_str) else {
            return deny_scope();
        };
        if cmd.is_empty() {
            return deny_scope();
        }

        let requested = match input_cwd(input) {
            Some(c) => PathBuf::from(c),
            // No cwd supplied: the command runs at the sandbox
            // root itself, so the needed scope is keyed on the
            // root path.
            None => (*self.cwd_root).to_path_buf(),
        };

        match lexical_resolve(&self.cwd_root, &requested) {
            Some(abs) => Scope::parse(&format!("shell.exec:cwd:{}", abs.display()))
                .unwrap_or_else(deny_scope),
            None => deny_scope(),
        }
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        // ---- Re-parse the input ------------------------------------
        let cmd = match input.get("cmd").and_then(Value::as_str) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => {
                return ToolOutcome::Failed(AivyxError::Tool {
                    tool: self.id,
                    detail: "input must have a non-empty string `cmd` field".to_string(),
                });
            }
        };
        let timeout_ms = input_timeout_ms(&input);

        // ---- Chapters Ward/Portcullis — sensitive-path guard ------
        //
        // The fs tools guard their own path, but a shell command can `cat`
        // a secret or `>>` a persistence file directly, bypassing them. Scan
        // the command text (best-effort) and refuse before spawning `sh`.
        // Disabled-by-default policy ⇒ this is a no-op until the binary
        // installs a real one, keeping pre-guard behavior byte-identical.
        if let Some((token, reason)) = sensitive_command_hit(&cmd, &self.sensitive) {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: format!(
                    "refusing to run this command — it references {token}, a \
                     protected location ({reason}). shell.exec will not read \
                     secrets or write persistence targets; add the path to \
                     `[access] allow_sensitive_paths` if you intend it."
                ),
            });
        }

        // ---- Lexical + canonical cwd resolve ----------------------
        //
        // The scope gate has already verified the lexical resolve's
        // result is in scope (or denied us before we got here). We
        // redo the lexical resolve to get an absolute path, then
        // canonicalize as a second fence for symlink traversal.
        let requested = match input_cwd(&input) {
            Some(c) => PathBuf::from(c),
            None => (*self.cwd_root).to_path_buf(),
        };
        let lexical_abs = match lexical_resolve(&self.cwd_root, &requested) {
            Some(p) => p,
            None => {
                return ToolOutcome::Failed(AivyxError::Internal(format!(
                    "shell.exec: lexical resolve escaped sandbox after scope gate \
                     admitted the call (cwd={requested:?})"
                )));
            }
        };
        let canonical_cwd = match std::fs::canonicalize(&lexical_abs) {
            Ok(p) => p,
            Err(e) => {
                return ToolOutcome::Failed(AivyxError::Tool {
                    tool: self.id,
                    detail: format!("cannot canonicalize cwd {lexical_abs:?}: {e}"),
                });
            }
        };
        if !canonical_cwd.starts_with(&*self.cwd_root) {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: format!(
                    "cwd {canonical_cwd:?} escapes sandbox root {:?} after \
                     symlink resolution",
                    self.cwd_root
                ),
            });
        }

        // ---- Spawn + bounded wait ---------------------------------
        //
        // `sh -c` because agents emit free-form command strings, not
        // pre-split argv vectors — this matches what the LLM already
        // produces in practice and avoids re-implementing shell
        // quoting in-tree. The drawback is that pipes and redirects
        // are interpreted by the shell, not the tool. That's fine
        // for Phase 11: the tool is Trusted-only, and a Trusted
        // agent calling `echo hi > file` is a feature, not a bug.
        let mut command = Command::new("sh");
        command.arg("-c").arg(&cmd).current_dir(&canonical_cwd);
        // Pipe stdout/stderr so wait_with_output() captures them.
        // Without this, child output goes to the parent's terminal
        // and wait_with_output() returns empty buffers.
        command.stdout(std::process::Stdio::piped());
        command.stderr(std::process::Stdio::piped());
        // Phase 42 — clear inherited environment so the child
        // cannot read ANTHROPIC_API_KEY, AIVYX_PA_PASSPHRASE, or
        // any other secret from the daemon's process env. Then
        // inject only safe defaults (PATH, HOME, USER, LANG,
        // TERM) from the current env, plus any vars the caller
        // explicitly declared in the `env` input field.
        command.env_clear();
        for &var in SAFE_ENV_DEFAULTS {
            if let Ok(val) = std::env::var(var) {
                command.env(var, val);
            }
        }
        let declared_env = input_env(&input);
        for (k, v) in &declared_env {
            command.env(k, v);
        }
        // Phase 42 — put the child and all its descendants into
        // their own process group (PGID = child PID). Without
        // this, `sh -c "cmd1 | cmd2"` forks cmd1 and cmd2 as
        // separate processes that outlive the direct child if we
        // only signal `sh`. With process_group(0), a single
        // killpg() reaches the entire tree.
        command.process_group(0);
        // Kill the child if the parent task is dropped (e.g. the
        // turn is cancelled mid-exec). Without this the child
        // would keep running until its own exit, leaking CPU
        // beyond the turn's wall-clock budget. With process_group
        // this only kills the direct child; the timeout path below
        // handles the full group via killpg().
        command.kill_on_drop(true);

        let mut command = self.confiner.confine(command);

        let child = match command.spawn() {
            Ok(c) => c,
            Err(e) => {
                return ToolOutcome::Failed(AivyxError::Tool {
                    tool: self.id,
                    detail: format!("spawn failed: {e}"),
                });
            }
        };

        // Capture the PID before wait_with_output() consumes the
        // child. The PID equals the PGID because we called
        // process_group(0). We need it in the timeout path to
        // signal the entire process group.
        let child_pid = child.id();

        let output =
            match tokio::time::timeout(Duration::from_millis(timeout_ms), child.wait_with_output())
                .await
            {
                Ok(Ok(out)) => out,
                Ok(Err(e)) => {
                    return ToolOutcome::Failed(AivyxError::Tool {
                        tool: self.id,
                        detail: format!("wait failed: {e}"),
                    });
                }
                Err(_elapsed) => {
                    // Phase 42 — graceful process-group shutdown:
                    // 1. SIGTERM the entire process group (child +
                    //    grandchildren). This lets processes flush
                    //    buffers and clean up temp files.
                    // 2. Wait 2 seconds for graceful exit.
                    // 3. SIGKILL the process group if still alive.
                    //
                    // The child's PID equals its PGID because we
                    // called process_group(0). child_pid is None
                    // only if the child exited before we read it,
                    // which would be surprising here (we just timed
                    // out waiting for it), but we handle it.
                    if let Some(pid) = child_pid {
                        let pgid = pid as i32;
                        // SIGTERM the process group.
                        // Safety: killpg is a standard POSIX call.
                        // pgid is always positive (u32 -> i32 of a
                        // real PID). A stale pgid (process already
                        // exited) returns ESRCH, which we ignore.
                        unsafe {
                            libc::killpg(pgid, libc::SIGTERM);
                        }

                        // Give the group 2 seconds to exit gracefully,
                        // then SIGKILL. We spawn a brief background
                        // reaper — the timeout future already dropped
                        // the child handle, so we can't await it here.
                        // Instead we wait synchronously (non-blocking
                        // for already-exited processes) via killpg
                        // after a sleep.
                        tokio::spawn(async move {
                            tokio::time::sleep(Duration::from_secs(2)).await;
                            // If the group is still alive, force-kill.
                            unsafe {
                                libc::killpg(pgid, libc::SIGKILL);
                            }
                        });
                    }

                    return ToolOutcome::Completed {
                        output: json!({
                            "cmd": cmd,
                            "cwd": canonical_cwd.display().to_string(),
                            "stdout": "",
                            "stderr": "",
                            "exit_code": -1_i64,
                            "timed_out": true,
                            "timeout_ms": timeout_ms,
                        }),
                        verified: Verification::NotApplicable,
                    };
                }
            };

        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let exit_code = output.status.code().unwrap_or(-1);

        ToolOutcome::Completed {
            output: json!({
                "cmd": cmd,
                "cwd": canonical_cwd.display().to_string(),
                "stdout": stdout,
                "stderr": stderr,
                "exit_code": exit_code,
                "timed_out": false,
                "timeout_ms": timeout_ms,
            }),
            // Verification is NotApplicable — shell output is
            // freeform text the tool cannot introspect, and a
            // nonzero exit code is a *result* the agent needs to
            // observe, not an indication the tool itself failed.
            verified: Verification::NotApplicable,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AgentId, CancellationToken, ChannelContext, ChannelError, ChannelPlatform, MessageOrigin,
        NullAuditHook, SessionId, StreamEvent, TurnId, TurnOutcome,
    };
    use std::path::PathBuf;

    // ---- Scratch dir helper (matches the in-tree convention) -------

    struct Scratch {
        dir: PathBuf,
    }

    impl Scratch {
        fn new() -> Self {
            let tmp = std::env::var("TMPDIR")
                .or_else(|_| std::env::var("TEMP"))
                .unwrap_or_else(|_| "/tmp".to_string());
            let dir = PathBuf::from(tmp).join(format!("aivyx-shell-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).expect("scratch dir must be creatable");
            // canonicalize so the tool's constructor doesn't
            // emit a surprising symlink-resolved root that then
            // breaks `starts_with` checks inside tests.
            let canonical = std::fs::canonicalize(&dir).expect("canonicalize scratch");
            Scratch { dir: canonical }
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn build_tool(root: &Path) -> ShellExecTool {
        ShellExecToolConfig::new(root)
            .build()
            .expect("shell tool builds against a real directory")
    }

    // ---- Minimal ChannelContext fake for ToolContext construction --

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

    fn fresh_channel() -> NoopChannel {
        NoopChannel {
            session: SessionId::new(),
            token: CancellationToken::new(),
        }
    }

    fn make_ctx<'a>(channel: &'a NoopChannel, audit: &'a dyn crate::AuditHook) -> ToolContext<'a> {
        ToolContext {
            agent_id: AgentId::new(),
            session_id: channel.session,
            turn_id: TurnId::new(),
            channel,
            audit,
            cancellation: &channel.token,
            message_origin: MessageOrigin::Operator,
        }
    }

    // ---- Scope derivation ------------------------------------------

    #[test]
    fn shell_exec_mutates_fs_root() {
        let scratch = Scratch::new();
        assert!(build_tool(&scratch.dir).mutates_fs_root());
    }

    #[test]
    fn shell_exec_output_is_untrusted_for_bulwark() {
        let scratch = Scratch::new();
        assert!(build_tool(&scratch.dir).output_is_untrusted());
    }

    #[test]
    fn required_scope_uses_canonical_cwd_root_for_missing_cwd() {
        let scratch = Scratch::new();
        let tool = build_tool(&scratch.dir);
        let scope = tool.required_scope(&json!({"cmd": "echo hi"}));
        // Base is always `shell.exec`; qualifier is `cwd:<path>`
        // — the `cwd:` prefix lives inside the qualifier so the
        // capability layer dispatches on it as a path-glob (any
        // `/` in the qualifier string triggers PathGlob). The
        // held capability is the one that carries `/**`.
        assert_eq!(scope.base(), "shell.exec");
        let qualifier = scope.qualifier().unwrap();
        assert_eq!(qualifier, format!("cwd:{}", scratch.dir.display()));
    }

    #[test]
    fn required_scope_uses_subdir_when_cwd_set() {
        let scratch = Scratch::new();
        let sub = scratch.dir.join("sub");
        std::fs::create_dir(&sub).unwrap();
        let tool = build_tool(&scratch.dir);
        let scope = tool.required_scope(&json!({
            "cmd": "ls",
            "args": { "cwd": sub.display().to_string() }
        }));
        assert_eq!(scope.qualifier().unwrap(), format!("cwd:{}", sub.display()));
    }

    #[test]
    fn required_scope_for_missing_cmd_is_deny_scope() {
        let scratch = Scratch::new();
        let tool = build_tool(&scratch.dir);
        let scope = tool.required_scope(&json!({}));
        assert!(scope.qualifier().unwrap().contains("__deny__"));
    }

    #[test]
    fn required_scope_for_empty_cmd_is_deny_scope() {
        let scratch = Scratch::new();
        let tool = build_tool(&scratch.dir);
        let scope = tool.required_scope(&json!({"cmd": ""}));
        assert!(scope.qualifier().unwrap().contains("__deny__"));
    }

    #[test]
    fn required_scope_lexical_cwd_escape_is_deny_scope() {
        // `/repo/../etc` lexically collapses to `/etc`, which is
        // not under `/repo`, so the lexical resolve returns None
        // and the tool emits a deny scope. The loop's scope gate
        // will then route the call through `Denied` (not
        // `Failed`) because the validator has already passed.
        let scratch = Scratch::new();
        let tool = build_tool(&scratch.dir);
        let escape = format!("{}/../../../etc", scratch.dir.display());
        let scope = tool.required_scope(&json!({
            "cmd": "cat passwd",
            "args": { "cwd": escape }
        }));
        assert!(scope.qualifier().unwrap().contains("__deny__"));
    }

    #[test]
    fn held_path_glob_grants_subdir_scope() {
        // Capability-layer integration anchor: a held
        // `shell.exec:cwd:<root>/**` must grant the per-call
        // `shell.exec:cwd:<root>/sub` the tool emits. The existing
        // PathGlob qualifier-kind handles this (any `/` in either
        // side triggers the path-glob dispatch, and the `cwd:`
        // prefix appears on both sides so it matches literally).
        use aivyx_capability::CapabilitySet;
        let scratch = Scratch::new();
        let sub = scratch.dir.join("sub");
        std::fs::create_dir(&sub).unwrap();
        let tool = build_tool(&scratch.dir);

        let held = CapabilitySet::from_scopes([Scope::parse(&format!(
            "shell.exec:cwd:{}/**",
            scratch.dir.display()
        ))
        .unwrap()]);
        let needed = tool.required_scope(&json!({
            "cmd": "ls",
            "args": { "cwd": sub.display().to_string() }
        }));
        assert!(
            held.grants(&needed),
            "held cwd:<root>/** must grant needed cwd:<root>/sub"
        );

        // And a held root-only (no glob) must NOT grant a
        // subdirectory call, or the whole attenuation story is
        // pointless.
        let held_strict = CapabilitySet::from_scopes([Scope::parse(&format!(
            "shell.exec:cwd:{}/unrelated",
            scratch.dir.display()
        ))
        .unwrap()]);
        assert!(
            !held_strict.grants(&needed),
            "held <root>/unrelated must NOT grant needed <root>/sub"
        );
    }

    // ---- Execution happy path --------------------------------------

    #[tokio::test]
    async fn execute_captures_stdout_and_exit_code() {
        let scratch = Scratch::new();
        let tool = build_tool(&scratch.dir);
        let channel = fresh_channel();
        let audit = NullAuditHook;
        let ctx = make_ctx(&channel, &audit);

        let out = tool.execute(json!({"cmd": "printf hello"}), &ctx).await;

        match out {
            ToolOutcome::Completed { output, verified } => {
                assert_eq!(output["stdout"], "hello");
                assert_eq!(output["exit_code"], 0);
                assert_eq!(output["timed_out"], false);
                assert_eq!(verified, Verification::NotApplicable);
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn execute_nonzero_exit_is_still_completed() {
        // A nonzero exit is an agent-observable *result*, not a
        // tool failure. The tool must Complete with the nonzero
        // code so the planner sees it.
        let scratch = Scratch::new();
        let tool = build_tool(&scratch.dir);
        let channel = fresh_channel();
        let audit = NullAuditHook;
        let ctx = make_ctx(&channel, &audit);

        let out = tool.execute(json!({"cmd": "sh -c 'exit 7'"}), &ctx).await;

        match out {
            ToolOutcome::Completed { output, .. } => {
                assert_eq!(output["exit_code"], 7);
                assert_eq!(output["timed_out"], false);
            }
            other => panic!("expected Completed with exit 7, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn execute_timeout_fires_and_returns_timed_out_true() {
        let scratch = Scratch::new();
        let tool = build_tool(&scratch.dir);
        let channel = fresh_channel();
        let audit = NullAuditHook;
        let ctx = make_ctx(&channel, &audit);

        // Sleep longer than the 50ms timeout we pass. The tool
        // must return `timed_out: true` and not hang the test.
        let out = tool
            .execute(
                json!({
                    "cmd": "sleep 5",
                    "args": { "timeout_ms": 50 }
                }),
                &ctx,
            )
            .await;

        match out {
            ToolOutcome::Completed { output, .. } => {
                assert_eq!(output["timed_out"], true);
                assert_eq!(output["timeout_ms"], 50);
            }
            other => panic!("expected Completed timed_out, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn execute_uses_provided_cwd() {
        // Write a sentinel file inside a subdirectory of the
        // sandbox root, then `cat` it from that subdirectory —
        // the only way to see it is if `cwd` actually moves the
        // child's working directory.
        let scratch = Scratch::new();
        let sub = scratch.dir.join("workdir");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("sentinel.txt"), "marker").unwrap();

        let tool = build_tool(&scratch.dir);
        let channel = fresh_channel();
        let audit = NullAuditHook;
        let ctx = make_ctx(&channel, &audit);

        let out = tool
            .execute(
                json!({
                    "cmd": "cat sentinel.txt",
                    "args": { "cwd": sub.display().to_string() }
                }),
                &ctx,
            )
            .await;

        match out {
            ToolOutcome::Completed { output, .. } => {
                assert_eq!(output["stdout"], "marker");
                assert_eq!(output["exit_code"], 0);
            }
            other => panic!("expected Completed marker read, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn execute_cwd_outside_sandbox_fails_with_escape_detail() {
        // Execute path reached with a lexical escape that the
        // scope gate would normally have denied. This is the
        // "reached execute with invariant violation" path — the
        // tool must fail loudly rather than silently running.
        // We call execute directly to bypass the scope gate and
        // verify the canonical fence catches it.
        let scratch = Scratch::new();
        let tool = build_tool(&scratch.dir);
        let channel = fresh_channel();
        let audit = NullAuditHook;
        let ctx = make_ctx(&channel, &audit);

        let escape = format!("{}/../../../etc", scratch.dir.display());
        let out = tool
            .execute(
                json!({
                    "cmd": "true",
                    "args": { "cwd": escape }
                }),
                &ctx,
            )
            .await;

        match out {
            ToolOutcome::Failed(AivyxError::Internal(msg)) => {
                assert!(
                    msg.contains("lexical resolve escaped sandbox"),
                    "unexpected internal detail: {msg}",
                );
            }
            other => panic!("expected Failed internal lexical escape, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn execute_timeout_kills_process_group_including_grandchildren() {
        // Phase 42 load-bearing test: a grandchild spawned by `sh -c`
        // must be killed when the tool times out, not left running as
        // an orphan. The command writes its grandchild PID to a file,
        // then sleeps. On timeout the tool SIGTERMs the process group.
        // We verify that the grandchild PID is gone after the tool
        // returns.
        let scratch = Scratch::new();
        let pid_file = scratch.dir.join("grandchild.pid");
        let tool = build_tool(&scratch.dir);
        let channel = fresh_channel();
        let audit = NullAuditHook;
        let ctx = make_ctx(&channel, &audit);

        // The command: (1) fork a background grandchild that writes
        // its PID to a file then sleeps, (2) parent sleeps too.
        // Both will be killed by the process-group signal on timeout.
        let cmd = format!("( echo $$ > {} ; sleep 30 ) & sleep 30", pid_file.display());

        let out = tool
            .execute(
                json!({
                    "cmd": cmd,
                    "args": { "timeout_ms": 200 }
                }),
                &ctx,
            )
            .await;

        match &out {
            ToolOutcome::Completed { output, .. } => {
                assert_eq!(output["timed_out"], true);
            }
            other => panic!("expected Completed timed_out, got {other:?}"),
        }

        // Give the SIGTERM/SIGKILL reaper a moment to fire.
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Read the grandchild PID and verify it's no longer running.
        if let Ok(pid_str) = std::fs::read_to_string(&pid_file) {
            if let Ok(pid) = pid_str.trim().parse::<i32>() {
                // kill(pid, 0) checks if the process exists without
                // sending a signal. Returns 0 if alive, -1 if not.
                let alive = unsafe { libc::kill(pid, 0) };
                assert_eq!(
                    alive, -1,
                    "grandchild PID {pid} should be dead after \
                     process-group kill, but kill(pid, 0) returned 0 \
                     (still alive)"
                );
            }
        }
        // If the pid file doesn't exist, the grandchild never got
        // a chance to write it (the timeout was faster than the
        // fork) — that's fine, the process group is still killed.
    }

    #[tokio::test]
    async fn execute_env_is_cleared_by_default() {
        // Phase 42 — the daemon's environment must not leak to
        // child processes. We set a canary var in the current
        // process, run a command that reads it, and verify it's
        // absent. The env_clear() call strips everything except
        // the safe defaults (PATH, HOME, USER, LANG, TERM).
        let scratch = Scratch::new();
        let tool = build_tool(&scratch.dir);
        let channel = fresh_channel();
        let audit = NullAuditHook;
        let ctx = make_ctx(&channel, &audit);

        // Set a canary that would leak if env_clear is missing.
        // Safety: set_var is unsafe since Rust 2024 edition.
        // This is a test-only narrow allow.
        #[allow(unsafe_code)]
        unsafe {
            std::env::set_var("AIVYX_TEST_SECRET_CANARY", "leaked");
        }

        let out = tool
            .execute(
                json!({"cmd": "printenv AIVYX_TEST_SECRET_CANARY || echo ABSENT"}),
                &ctx,
            )
            .await;

        // Clean up the canary immediately.
        #[allow(unsafe_code)]
        unsafe {
            std::env::remove_var("AIVYX_TEST_SECRET_CANARY");
        }

        match out {
            ToolOutcome::Completed { output, .. } => {
                let stdout = output["stdout"].as_str().unwrap();
                assert!(
                    stdout.contains("ABSENT"),
                    "canary var should not leak to child; got: {stdout}"
                );
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn execute_declared_env_vars_are_passed() {
        // Phase 42 — explicitly declared env vars in the `env`
        // input field must be available to the child.
        let scratch = Scratch::new();
        let tool = build_tool(&scratch.dir);
        let channel = fresh_channel();
        let audit = NullAuditHook;
        let ctx = make_ctx(&channel, &audit);

        let out = tool
            .execute(
                json!({
                    "cmd": "printenv MY_CUSTOM_VAR",
                    "args": {
                        "env": { "MY_CUSTOM_VAR": "hello_phase42" }
                    }
                }),
                &ctx,
            )
            .await;

        match out {
            ToolOutcome::Completed { output, .. } => {
                let stdout = output["stdout"].as_str().unwrap().trim();
                assert_eq!(stdout, "hello_phase42");
                assert_eq!(output["exit_code"], 0);
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn execute_safe_defaults_are_injected() {
        // Phase 42 — PATH must survive env_clear so basic
        // commands like `printf` resolve. HOME, USER, etc.
        // should also be present if set in the daemon's env.
        let scratch = Scratch::new();
        let tool = build_tool(&scratch.dir);
        let channel = fresh_channel();
        let audit = NullAuditHook;
        let ctx = make_ctx(&channel, &audit);

        let out = tool.execute(json!({"cmd": "printenv PATH"}), &ctx).await;

        match out {
            ToolOutcome::Completed { output, .. } => {
                let stdout = output["stdout"].as_str().unwrap().trim();
                assert!(
                    !stdout.is_empty(),
                    "PATH should be injected as a safe default"
                );
                assert_eq!(output["exit_code"], 0);
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    // ---- Sensitive-path guard (Ward/Portcullis on shell.exec) ------

    fn guarded_tool(root: &Path) -> ShellExecTool {
        build_tool(root).with_sensitive_policy(Arc::new(
            crate::sensitive_paths::SensitivePolicy::new(vec![], vec![]),
        ))
    }

    #[tokio::test]
    async fn guard_refuses_reading_ssh_key_via_shell() {
        let scratch = Scratch::new();
        let tool = guarded_tool(&scratch.dir);
        let channel = fresh_channel();
        let audit = NullAuditHook;
        let ctx = make_ctx(&channel, &audit);

        for cmd in [
            "cat ~/.ssh/id_rsa",
            "cat /home/someone/.aws/credentials",
            "cp ~/.gnupg/secring.gpg /tmp/x",
            "cat app.env",
        ] {
            let out = tool.execute(json!({ "cmd": cmd }), &ctx).await;
            match out {
                ToolOutcome::Failed(AivyxError::Tool { detail, .. }) => {
                    assert!(
                        detail.contains("protected location"),
                        "cmd {cmd:?} should be refused; got: {detail}"
                    );
                }
                other => panic!("cmd {cmd:?} expected refusal, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn guard_refuses_persistence_write_via_shell() {
        let scratch = Scratch::new();
        let tool = guarded_tool(&scratch.dir);
        let channel = fresh_channel();
        let audit = NullAuditHook;
        let ctx = make_ctx(&channel, &audit);

        for cmd in [
            "echo evil >> ~/.bashrc",
            "echo key >> ~/.ssh/authorized_keys",
            "crontab -l",
        ] {
            let out = tool.execute(json!({ "cmd": cmd }), &ctx).await;
            match out {
                ToolOutcome::Failed(AivyxError::Tool { detail, .. }) => {
                    assert!(detail.contains("protected location"), "{detail}");
                }
                other => panic!("cmd {cmd:?} expected refusal, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn guard_allows_ordinary_commands() {
        let scratch = Scratch::new();
        std::fs::write(scratch.dir.join("notes.txt"), "hello").unwrap();
        let tool = guarded_tool(&scratch.dir);
        let channel = fresh_channel();
        let audit = NullAuditHook;
        let ctx = make_ctx(&channel, &audit);

        for cmd in ["echo hi", "ls -la", "cat notes.txt"] {
            let out = tool.execute(json!({ "cmd": cmd }), &ctx).await;
            assert!(
                matches!(out, ToolOutcome::Completed { .. }),
                "cmd {cmd:?} should run; got {out:?}"
            );
        }
    }

    #[tokio::test]
    async fn disabled_policy_is_byte_identical_no_op() {
        // The default (no policy installed) must NOT block a command that
        // merely mentions a secret-shaped token — pre-guard behavior.
        let scratch = Scratch::new();
        let tool = build_tool(&scratch.dir); // no with_sensitive_policy
        let channel = fresh_channel();
        let audit = NullAuditHook;
        let ctx = make_ctx(&channel, &audit);

        let out = tool
            .execute(json!({ "cmd": "echo ~/.ssh/id_rsa" }), &ctx)
            .await;
        assert!(
            matches!(out, ToolOutcome::Completed { .. }),
            "disabled policy must not block; got {out:?}"
        );
    }

    // ---- OS-level confinement (aivyx-confine) -----------------------

    #[tokio::test]
    async fn execute_denies_a_write_outside_cwd_root_under_the_default_confiner() {
        let dir = tempfile::tempdir().unwrap();
        let tool = ShellExecToolConfig::new(dir.path().to_path_buf())
            .build()
            .expect("shell tool should build");
        let channel = fresh_channel();
        let audit = NullAuditHook;
        let ctx = make_ctx(&channel, &audit);

        // Outside the sandbox root entirely — /var/tmp, not another
        // tempfile::tempdir() (which would also resolve under /tmp,
        // itself write-granted by aivyx-confine's default write scope).
        // Skip (not panic) on a machine where /var/tmp isn't writable,
        // matching `tools::git`'s `init_temp_repo` skip-not-fail posture
        // for environment-dependent fixtures.
        let Ok(outside) = tempfile::Builder::new().tempdir_in("/var/tmp") else {
            return;
        };
        let target = outside.path().join("should-not-exist.txt");

        let input = serde_json::json!({
            "cmd": format!("echo hi > {}", target.display()),
        });

        let outcome = tool.execute(input, &ctx).await;

        assert!(
            !target.exists(),
            "write outside cwd_root must be denied by Landlock"
        );
        // The shell command itself still "completes" (sh runs, the redirect
        // just fails inside it) -- the ToolOutcome variant is still worth
        // pinning so this test doesn't pass vacuously if the whole call
        // were refused for an unrelated reason (e.g. a spawn-level
        // failure): it must be Completed with a nonzero exit code, not
        // Failed, since `sh -c` swallows the redirect failure into its
        // own exit status rather than a spawn-level error.
        match &outcome {
            ToolOutcome::Completed { output, .. } => {
                assert_ne!(
                    output["exit_code"], 0,
                    "the denied redirect should have made `sh -c` exit nonzero"
                );
            }
            other => panic!(
                "expected Completed (the shell command itself still runs; only the \
                 redirect fails inside it), got {other:?}"
            ),
        }
    }

    #[tokio::test]
    async fn input_schema_matches_nested_shell_exec_contract() {
        // Pins the exact schema shape: top-level required=[cmd],
        // nested `args` is an optional object with cwd+timeout_ms+env.
        // If a future refactor accidentally drops the nested
        // structure or renames a field, this test catches it.
        let scratch = Scratch::new();
        let tool = build_tool(&scratch.dir);
        let schema = tool.input_schema();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"], json!(["cmd"]));
        assert_eq!(schema["additionalProperties"], false);
        let args_schema = &schema["properties"]["args"];
        assert_eq!(args_schema["type"], "object");
        assert_eq!(args_schema["additionalProperties"], false);
        assert_eq!(args_schema["properties"]["cwd"]["type"], "string");
        assert_eq!(args_schema["properties"]["timeout_ms"]["type"], "integer");
        // Phase 42 — env field is an object with string values.
        assert_eq!(args_schema["properties"]["env"]["type"], "object");
        assert_eq!(
            args_schema["properties"]["env"]["additionalProperties"],
            json!({"type": "string"})
        );
    }
}
