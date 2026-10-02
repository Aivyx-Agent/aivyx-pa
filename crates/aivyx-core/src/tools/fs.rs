//! `FsReadTool` — the first concrete filesystem tool.
//!
//! ## What it does
//!
//! Reads a UTF-8 (or lossily-decoded binary) file from under an
//! agent-configured sandbox root and returns its contents as a
//! `ToolOutcome::Completed` JSON payload. The agent must hold a scope
//! like `fs.read:/sandbox/**` in its effective capability set; the loop's
//! scope gate at `agent.rs` ensures only in-scope reads reach
//! [`FsReadTool::execute`].
//!
//! ## Defense in depth
//!
//! Path traversal is the attack this tool defends against, and it
//! defends in **two independent layers**:
//!
//! 1. **Lexical layer (`required_scope`).** Purely string-based
//!    normalization of the input path against the pre-canonicalized
//!    sandbox root. Handles `.` and `..` without touching the
//!    filesystem. Produces the `Scope` the loop's scope gate checks.
//!    An attacker input like `"../../etc/passwd"` lexically resolves
//!    to a scope *outside* the sandbox prefix and the scope gate
//!    denies the call before [`execute`] runs.
//!
//! 2. **Canonical layer (`execute`).** Right before opening the file,
//!    `execute` calls `std::fs::canonicalize`, which resolves every
//!    symlink in the path. If the canonicalized path no longer starts
//!    with the canonicalized sandbox root, the tool returns
//!    `ToolOutcome::Failed(AivyxError::Tool { detail: "…escapes
//!    sandbox…" })`. This catches symlink-based traversal that the
//!    lexical layer cannot see: an attacker who creates a symlink
//!    `/sandbox/escape → /etc/shadow` and then asks the agent to read
//!    `"escape"` passes the lexical check (the input path is inside
//!    the sandbox) and is stopped by the canonical check.
//!
//! The two layers catch two independent attack classes. Removing
//! either one leaves a hole. Documented here so a future contributor
//! who thinks "why are we canonicalizing twice?" gets the answer
//! in-place.
//!
//! ## Purity of `required_scope`
//!
//! `Tool::required_scope` is documented as pure ("must not perform
//! side effects"). The Phase 4 task 2 decision is to interpret that
//! as **logically pure**: deterministic, referentially transparent,
//! no observable side effects. Reading the filesystem to canonicalize
//! *could* be argued as a side effect (it loads pages into the kernel
//! cache), but `required_scope` is called at most once per tool call
//! and is idempotent, so the pragmatic cost is zero.
//!
//! The cleaner answer is that this tool **does not call the filesystem
//! from `required_scope` at all**. The sandbox root is canonicalized
//! **once at construction time** by [`FsReadToolConfig::build`], and
//! `required_scope` performs only lexical path work against the
//! pre-canonicalized root. Tests can construct an `FsReadTool` against
//! any existing directory and then call `required_scope` with any
//! input, no further I/O required.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use aivyx_capability::Scope;

use crate::{AivyxError, Tool, ToolContext, ToolId, ToolOutcome, Verification};

/// Default cap on the number of bytes read from a single file. Files
/// larger than this return their first `MAX_READ_BYTES` bytes along
/// with `"truncated": true`. 256 KiB is big enough for most config
/// files, source files, and notes; small enough that an agent cannot
/// drain a multi-gigabyte log into a single LLM turn by accident.
/// Const, not config knob — same philosophy as `TURN_TIMEOUT`.
pub const MAX_READ_BYTES: usize = 256 * 1024;

/// Construction inputs for [`FsReadTool`]. Split from the tool itself
/// so that the fallible `canonicalize()` call on the sandbox root
/// happens at `build()` time and the resulting `FsReadTool` is
/// infallible to construct (holding a pre-canonicalized absolute path).
pub struct FsReadToolConfig {
    sandbox_root: PathBuf,
    sensitive: Arc<crate::sensitive_paths::SensitivePolicy>,
}

impl FsReadToolConfig {
    pub fn new(sandbox_root: impl Into<PathBuf>) -> Self {
        FsReadToolConfig {
            sandbox_root: sandbox_root.into(),
            // Default disabled ⇒ byte-identical to pre-Ward behavior until the
            // binary wires in the operator's policy.
            sensitive: Arc::new(crate::sensitive_paths::SensitivePolicy::disabled()),
        }
    }

    /// Chapter Ward — install the sensitive-path read guard. When enabled, a
    /// canonical path matching the built-in secret set (minus the operator's
    /// allow-list) is refused even if it's inside the sandbox root.
    pub fn with_sensitive_policy(
        mut self,
        policy: Arc<crate::sensitive_paths::SensitivePolicy>,
    ) -> Self {
        self.sensitive = policy;
        self
    }

    /// Canonicalize the sandbox root and return a ready-to-register
    /// [`FsReadTool`]. Fails if the root doesn't exist or isn't a
    /// directory — those are configuration errors the caller needs to
    /// see at startup, not at tool-call time.
    pub fn build(self) -> Result<FsReadTool, AivyxError> {
        let canonical = std::fs::canonicalize(&self.sandbox_root).map_err(|e| {
            AivyxError::Config(format!(
                "fs.read sandbox root {:?} cannot be canonicalized: {e}",
                self.sandbox_root
            ))
        })?;
        if !canonical.is_dir() {
            return Err(AivyxError::Config(format!(
                "fs.read sandbox root {canonical:?} is not a directory"
            )));
        }
        Ok(FsReadTool {
            id: ToolId::new(),
            sandbox_root: Arc::from(canonical),
            schema: read_input_schema_value(),
            sensitive: self.sensitive,
        })
    }
}

/// Reference filesystem read tool. Agents holding
/// `fs.read:<sandbox_root>/**` can read any file under the sandbox,
/// regardless of symlink topology, up to [`MAX_READ_BYTES`].
#[derive(Debug)]
pub struct FsReadTool {
    id: ToolId,
    /// Pre-canonicalized absolute path. `Arc<Path>` because the tool
    /// is registered in an `Arc<ToolRegistry>` and cloned across
    /// concurrent turns; shared ownership of an immutable path is
    /// cheaper than cloning a `PathBuf` per call.
    sandbox_root: Arc<Path>,
    schema: Value,
    /// Chapter Ward — the sensitive-path read guard (disabled by default).
    sensitive: Arc<crate::sensitive_paths::SensitivePolicy>,
}

impl FsReadTool {
    /// Expose the canonicalized sandbox root (primarily for tests and
    /// for the binary's startup banner — the registered agent needs
    /// to know which directory it was actually granted access to).
    pub fn sandbox_root(&self) -> &Path {
        &self.sandbox_root
    }
}

/// Lexically resolve `input_path` (which may be relative) against
/// `sandbox_root`, returning an absolute path with all `.` and `..`
/// segments collapsed. **Does not touch the filesystem** — symlinks
/// are not resolved here.
///
/// Shared by [`FsReadTool`], [`FsWriteTool`], and [`FsDeleteTool`]
/// since all three need the same purely-lexical TOCTOU-resistant
/// path joining, but the canonical fence each runs afterwards
/// differs (reads canonicalize the file itself; writes and deletes
/// canonicalize the *parent directory* because the final entry may
/// not exist yet, or may be a symlink that must not be followed).
///
/// Returns `None` if the lexical resolution escapes the sandbox
/// root (e.g., more `..` segments than there are components below
/// the root). The caller uses this signal to produce a deny-by-
/// construction scope.
// Chapter Z — promoted to `pub` so the daemon's read-only Documents browser
// (`aivyx-channel::document_browse`) reuses the exact same lexical-resolve
// guard the fs tools use, rather than reimplementing escape protection.
pub fn lexical_resolve(sandbox_root: &Path, input_path: &Path) -> Option<PathBuf> {
    // Join semantics: if `input_path` is absolute, `PathBuf::push`
    // *replaces* the current path. That's the right thing for an
    // agent that tries to pass an absolute path: it lands
    // wherever the absolute path points, and the post-collapse
    // prefix check will reject it if it's outside the sandbox.
    let mut joined = PathBuf::from(sandbox_root);
    joined.push(input_path);

    // Collapse `.` and `..`. We iterate components and maintain a
    // stack: `CurDir` is skipped, `ParentDir` pops the stack but
    // only if the stack has more components than the sandbox
    // root's component count (so `../` out of the sandbox root
    // itself returns None).
    let root_components: Vec<Component<'_>> = sandbox_root.components().collect();
    let mut stack: Vec<Component<'_>> = Vec::with_capacity(16);
    for comp in joined.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                // Pop, unless popping would take us out of the
                // sandbox root.
                if stack.len() <= root_components.len() {
                    return None;
                }
                stack.pop();
            }
            other => stack.push(other),
        }
    }

    // Verify the resolved stack still starts with the sandbox
    // root. A case this catches: a Unix absolute input like
    // `/etc/passwd` replaces the prefix entirely via `push()`,
    // so `stack` ends up as `[/, etc, passwd]` with no sandbox
    // prefix, and the check below fires.
    for (i, root_c) in root_components.iter().enumerate() {
        if stack.get(i) != Some(root_c) {
            return None;
        }
    }

    Some(stack.into_iter().collect())
}

/// True when any component of `path` is exactly `.git`.
///
/// A write (or delete) anywhere under a `.git` directory can redefine what
/// `git` itself does on a *later*, completely unrelated command: a
/// `core.fsmonitor` or `filter.<name>.clean`/`.smudge` entry in
/// `.git/config` (paired with a matching `.gitattributes` line) makes the
/// next `git status`/`diff`/`add`/`commit` run an arbitrary program, and a
/// file dropped in `.git/hooks/` runs on the next `commit`/`checkout`/etc.
/// `aivyx-checkpoint`'s `GitCheckpointer` runs `git add -A` **unconfined**
/// (outside Landlock) before every mutating tool call on a checkpointed
/// git repo (the `fs` root and the `workspace` notebook, when either is a
/// git repo — see `crates/aivyx-core/src/agent.rs`'s `checkpointer` field
/// and `crates/aivyx-core/src/tools/workspace.rs`). So a tool write into
/// `.git/` turns "the agent may write a file" into "the agent may run any
/// program," entirely outside the confinement the rest of this module
/// exists to provide. This predicate is the shared check every mutating
/// fs/workspace/data-writer tool runs, unconditionally, after resolving
/// the input path and before any confirm-first/escalation logic — it is
/// not a `[access] allow_sensitive_paths`-style opt-out-able guard.
///
/// Matching is on path *components*, not a substring, so a legitimate
/// `.github/workflow.yml` or `.gitignore` (which share the `.git` prefix
/// as text but are different path components) are unaffected.
///
/// Compared case-insensitively (ASCII) unconditionally — not gated on the
/// host OS — because macOS (a shipped target) and Windows both commonly
/// run case-insensitive filesystems where `.GIT` and `.git` name the same
/// directory; on a case-sensitive filesystem this is simply a stricter,
/// harmless superset of the exact-case check.
pub fn touches_git_metadata(path: &Path) -> bool {
    path.components().any(|c| match c {
        Component::Normal(seg) => seg.eq_ignore_ascii_case(".git"),
        _ => false,
    })
}

/// Shared refusal text for every tool that blocks a write/delete under
/// `.git` — see [`touches_git_metadata`].
pub const GIT_METADATA_WRITE_BLOCKED: &str = "Writing inside .git is blocked: git runs programs \
named there (hooks, filters, fsmonitor). Ask the operator to change git settings themselves.";

#[async_trait]
impl Tool for FsReadTool {
    fn id(&self) -> ToolId {
        self.id
    }

    fn name(&self) -> &str {
        "fs.read"
    }

    // Chapter Bulwark — a file's contents are untrusted: it may be attacker-
    // supplied (downloaded, shared, or in a broad-access location) and carry a
    // prompt-injection payload. Fence it as data, not instructions.
    fn output_is_untrusted(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "Read a UTF-8 (or binary) file from under the agent's sandbox root. \
         Input is a JSON object with a `path` field (relative paths are \
         resolved against the sandbox root; absolute paths must already be \
         under the sandbox root or the call is denied). Files larger than \
         256 KiB are truncated."
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn required_scope(&self, input: &Value) -> Scope {
        let Some(path_str) = input.get("path").and_then(|v| v.as_str()) else {
            // Missing or non-string `path`. Produce a scope no agent
            // holds so the loop's scope gate denies the call and the
            // planner sees `Denied`. We deliberately do NOT return a
            // `Failed` here because `required_scope` returns `Scope`,
            // not `Result`, and a Denied outcome is the closest
            // equivalent to "input is malformed, don't run."
            return read_deny_scope();
        };

        match lexical_resolve(&self.sandbox_root, Path::new(path_str)) {
            Some(abs) => {
                Scope::parse(&format!("fs.read:{}", abs.display())).unwrap_or_else(read_deny_scope)
            }
            None => read_deny_scope(),
        }
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        // ---- Re-parse and re-resolve the input --------------------
        //
        // The loop has already verified the *derived* scope is in the
        // agent's set, but that verification used the lexical resolve.
        // `execute` must do the canonical resolve as the second fence
        // (see the module-level "Defense in depth" note).
        let path_str = match input.get("path").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => {
                return ToolOutcome::Failed(AivyxError::Tool {
                    tool: self.id,
                    detail: "input must have a string `path` field".to_string(),
                });
            }
        };

        let lexical_abs = match lexical_resolve(&self.sandbox_root, Path::new(path_str)) {
            Some(p) => p,
            None => {
                // Should never reach here — the scope gate would have
                // denied a lexical-escape input before execute was
                // called. Treat as an internal invariant violation so
                // a bug in the gate shows up loudly in audit.
                return ToolOutcome::Failed(AivyxError::Internal(format!(
                    "fs.read: lexical resolve escaped sandbox after scope gate \
                     admitted the call (path={path_str:?})"
                )));
            }
        };

        // ---- Canonical fence --------------------------------------
        //
        // Resolve every symlink. If the canonicalized path no longer
        // lives under the canonicalized sandbox root, the call is
        // refused. This is the TOCTOU-resistant check the lexical
        // resolve cannot perform.
        let canonical = match std::fs::canonicalize(&lexical_abs) {
            Ok(p) => p,
            Err(e) => {
                return ToolOutcome::Failed(AivyxError::Tool {
                    tool: self.id,
                    detail: format!("cannot canonicalize {lexical_abs:?}: {e}"),
                });
            }
        };
        if !canonical.starts_with(&*self.sandbox_root) {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: format!(
                    "path {canonical:?} escapes sandbox root {:?} after \
                     symlink resolution",
                    self.sandbox_root
                ),
            });
        }

        // ---- Chapter Ward — sensitive-path guard -------------------
        //
        // Checked on the CANONICAL path (symlinks resolved), so a symlink
        // pointing at a secret is refused too. Independent of the sandbox
        // root: even at `full` reach, credential stores stay off-limits
        // unless the operator allow-lists them.
        if let Some(reason) = self.sensitive.classify(&canonical) {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: format!(
                    "refusing to read {} — {reason}. This is a protected \
                     location; add it to `[access] allow_sensitive_paths` if \
                     you intend the agent to read it.",
                    canonical.display()
                ),
            });
        }

        // ---- Read --------------------------------------------------
        //
        // Read up to MAX_READ_BYTES + 1 so we can tell "exactly at cap"
        // apart from "over cap" in one syscall.
        use std::io::Read;
        let mut file = match std::fs::File::open(&canonical) {
            Ok(f) => f,
            Err(e) => {
                return ToolOutcome::Failed(AivyxError::Tool {
                    tool: self.id,
                    detail: format!("cannot open {canonical:?}: {e}"),
                });
            }
        };

        let mut buf = Vec::with_capacity(8 * 1024);
        let mut probe = [0u8; 8 * 1024];
        let mut total_read = 0usize;
        loop {
            match file.read(&mut probe) {
                Ok(0) => break,
                Ok(n) => {
                    total_read += n;
                    if buf.len() + n > MAX_READ_BYTES {
                        let room = MAX_READ_BYTES.saturating_sub(buf.len());
                        buf.extend_from_slice(&probe[..room]);
                        break;
                    }
                    buf.extend_from_slice(&probe[..n]);
                    if total_read >= MAX_READ_BYTES {
                        break;
                    }
                }
                Err(e) => {
                    return ToolOutcome::Failed(AivyxError::Tool {
                        tool: self.id,
                        detail: format!("read error on {canonical:?}: {e}"),
                    });
                }
            }
        }

        // Drain any trailing bytes past the cap so we can set the
        // `truncated` flag honestly even when the file is bigger
        // than what we read.
        let mut truncated = buf.len() >= MAX_READ_BYTES;
        if !truncated {
            // Try one more read — if there's more data, it's truncated.
            let mut extra = [0u8; 1];
            if let Ok(n) = file.read(&mut extra) {
                if n > 0 {
                    truncated = true;
                }
            }
        }

        // ---- Decode ------------------------------------------------
        //
        // Use `from_utf8_lossy` so binary files produce *something*
        // rather than a hard error — the agent can then decide what
        // to do with the result. Flag non-UTF8 data explicitly so the
        // LLM knows its input was lossy.
        let (text, is_binary) = match std::str::from_utf8(&buf) {
            Ok(s) => (s.to_string(), false),
            Err(_) => (String::from_utf8_lossy(&buf).into_owned(), true),
        };

        ToolOutcome::Completed {
            output: json!({
                "path": canonical.display().to_string(),
                "bytes": buf.len(),
                "truncated": truncated,
                "binary": is_binary,
                "text": text,
            }),
            // A file read has no effect to verify — it's a pure query.
            verified: Verification::NotApplicable,
        }
    }
}

/// A scope no real agent should ever hold. Returned by `required_scope`
/// when the input is malformed or escapes the sandbox lexically. The
/// loop's scope gate will deny the call and the planner sees
/// `ToolOutcome::Denied`. The specific string is meaningless — any
/// legal scope with an impossible qualifier works; the value here is
/// chosen for grep-ability in audit trails.
///
/// Parameterized by base so `fs.read` and `fs.write` produce distinct
/// deny scopes — the audit entry for a denied call carries the base,
/// and distinguishing "which tool tried to escape" is useful grep
/// context when an LLM is probing.
fn deny_scope_for(base: &str) -> Scope {
    Scope::parse(&format!("{base}:/aivyx/__deny__/invalid-input")).expect("deny scope must parse")
}

fn read_deny_scope() -> Scope {
    deny_scope_for("fs.read")
}

fn read_input_schema_value() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "Path to read. Relative paths resolve against \
                               the agent's sandbox root. Absolute paths must \
                               already be under the sandbox root."
            }
        },
        "required": ["path"]
    })
}

fn write_deny_scope() -> Scope {
    deny_scope_for("fs.write")
}

/// Chapter N — confirm-first gate. When the operator has enabled
/// `[access] confirm_destructive`, an irreversible op (a delete, or an
/// overwrite of an existing file) must carry `confirmed: true`. The model
/// is instructed to set it only AFTER showing the operator what will be
/// affected and getting their approval — the same operator-in-the-loop
/// pattern as the irreversible `skills.teach`. Until then the tool refuses,
/// so a hallucinated path can't silently destroy data.
fn is_confirmed(input: &Value) -> bool {
    input.get("confirmed").and_then(|v| v.as_bool()) == Some(true)
}

const DESTRUCTIVE_CONFIRM_HINT: &str = "It needs approval first.";

/// The `confirmed` schema property shared by the confirm-first tools.
fn confirmed_schema_property() -> Value {
    json!({
        "type": "boolean",
        "description": "Set to `true` ONLY after the operator has approved this \
                        specific irreversible action. Required when the operator \
                        has enabled confirm-first for destructive operations."
    })
}

fn write_input_schema_value() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "Path to write. Relative paths resolve against \
                               the agent's sandbox root. Absolute paths must \
                               already be under the sandbox root. Parent \
                               directories are created as needed, but only \
                               within the sandbox root."
            },
            "content": {
                "type": "string",
                "description": "UTF-8 content to write. Existing files are \
                               overwritten atomically via a same-directory \
                               temp file plus rename."
            },
            "confirmed": confirmed_schema_property()
        },
        "required": ["path", "content"]
    })
}

// ---------------------------------------------------------------------------
// FsWriteTool — Phase 4 task 3
//
// Writes a UTF-8 file under the sandbox root. Atomic via same-directory
// temp file + rename — a partial write across a ctrl-C leaves the
// target file untouched rather than half-written. Verification re-stats
// the file after rename and checks the byte count matches what we
// wrote, surfacing as `Verification::Verified` in the `ToolOutcome`.
//
// Shares `lexical_resolve` with `FsReadTool` but runs a *different*
// canonical fence: for writes, the target file may not exist yet, so
// we canonicalize the *parent directory* (after ensuring it exists
// within the sandbox via `create_dir_all`) and check the canonicalized
// parent is still inside the sandbox root. The final target path is
// `canonical_parent.join(file_name)`, which is the write destination.
//
// See Q3 in PHASE_4.md for the atomic-vs-plain-vs-backup trade-off.
// Resolution: atomic temp+rename, no backup file. Rationale:
//   - Partial writes across ctrl-C are the dominant failure mode for
//     a tool called by an LLM that may be interrupted mid-turn.
//   - Backup files clutter directories, double I/O on every write,
//     and the audit chain already records enough context
//     (`input_hash` covering content bytes) for recovery.
// ---------------------------------------------------------------------------

/// Maximum size of a single write, in bytes. Symmetric with
/// `MAX_READ_BYTES`. An LLM that tries to emit a megabyte in one turn
/// is almost always a bug — either hallucination loop or stuck
/// generation. Const, not config.
pub const MAX_WRITE_BYTES: usize = 256 * 1024;

/// Construction inputs for [`FsWriteTool`]. Same split pattern as
/// [`FsReadToolConfig`]: fallible canonicalization at build time,
/// infallible tool construction.
pub struct FsWriteToolConfig {
    sandbox_root: PathBuf,
    confirm_destructive: bool,
    sensitive: Arc<crate::sensitive_paths::SensitivePolicy>,
}

impl FsWriteToolConfig {
    pub fn new(sandbox_root: impl Into<PathBuf>) -> Self {
        FsWriteToolConfig {
            sandbox_root: sandbox_root.into(),
            confirm_destructive: false,
            sensitive: Arc::new(crate::sensitive_paths::SensitivePolicy::disabled()),
        }
    }

    /// Chapter N — require `confirmed: true` to OVERWRITE an existing file
    /// (a fresh write to a new path never gates). Off by default.
    pub fn with_confirm_destructive(mut self, confirm: bool) -> Self {
        self.confirm_destructive = confirm;
        self
    }

    /// Chapter Portcullis — install the sensitive-path guard so writes to
    /// secret + persistence locations (shell rc, authorized_keys, systemd/
    /// autostart/cron, git hooks) are refused. `classify_write` is used.
    pub fn with_sensitive_policy(
        mut self,
        policy: Arc<crate::sensitive_paths::SensitivePolicy>,
    ) -> Self {
        self.sensitive = policy;
        self
    }

    /// Canonicalize the sandbox root and return a ready-to-register
    /// [`FsWriteTool`]. Fails at startup if the root doesn't exist
    /// or isn't a directory — configuration errors must not surface
    /// at tool-call time.
    pub fn build(self) -> Result<FsWriteTool, AivyxError> {
        let canonical = std::fs::canonicalize(&self.sandbox_root).map_err(|e| {
            AivyxError::Config(format!(
                "fs.write sandbox root {:?} cannot be canonicalized: {e}",
                self.sandbox_root
            ))
        })?;
        if !canonical.is_dir() {
            return Err(AivyxError::Config(format!(
                "fs.write sandbox root {canonical:?} is not a directory"
            )));
        }
        Ok(FsWriteTool {
            id: ToolId::new(),
            sandbox_root: Arc::from(canonical),
            schema: write_input_schema_value(),
            confirm_destructive: self.confirm_destructive,
            sensitive: self.sensitive,
        })
    }
}

/// Reference filesystem write tool. Agents holding
/// `fs.write:<sandbox_root>/**` can write any file under the sandbox,
/// creating parent directories on demand within the sandbox, up to
/// [`MAX_WRITE_BYTES`] per call. Writes are atomic via same-directory
/// temp file + rename.
#[derive(Debug)]
pub struct FsWriteTool {
    id: ToolId,
    sandbox_root: Arc<Path>,
    schema: Value,
    /// Chapter N — when true, overwriting an existing file needs
    /// `confirmed: true`, from a turn after the refusal.
    confirm_destructive: bool,
    /// Chapter Portcullis — write guard for secret + persistence paths.
    sensitive: Arc<crate::sensitive_paths::SensitivePolicy>,
}

impl FsWriteTool {
    pub fn sandbox_root(&self) -> &Path {
        &self.sandbox_root
    }
}

#[async_trait]
impl Tool for FsWriteTool {
    fn id(&self) -> ToolId {
        self.id
    }

    fn name(&self) -> &str {
        "fs.write"
    }

    /// Writes land on disk under fs_root — checkpoint before every call.
    fn mutates_fs_root(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "Write a UTF-8 file — the operator's files, under your fs root — atomically. \
         Input is a JSON object with `path` (where to write) and `content` \
         (the UTF-8 text to write). Relative paths resolve against the \
         sandbox root; absolute paths must already be under it. Parent \
         directories are created on demand inside the sandbox. Existing \
         files are overwritten. Max 256 KiB per call."
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn required_scope(&self, input: &Value) -> Scope {
        let Some(path_str) = input.get("path").and_then(|v| v.as_str()) else {
            return write_deny_scope();
        };
        // We don't require `content` to be present here — that's an
        // execute-time validation. `required_scope` cares only about
        // *where* the agent wants to write, which is the scope-gated
        // decision. An agent with a valid path but missing content
        // still deserves to see the call admitted-then-failed at the
        // tool level so the LLM learns from the tool error text. A
        // caller missing both fields has bigger problems.

        match lexical_resolve(&self.sandbox_root, Path::new(path_str)) {
            Some(abs) => Scope::parse(&format!("fs.write:{}", abs.display()))
                .unwrap_or_else(write_deny_scope),
            None => write_deny_scope(),
        }
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        // ---- Validate input fields -------------------------------
        let path_str = match input.get("path").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => {
                return ToolOutcome::Failed(AivyxError::Tool {
                    tool: self.id,
                    detail: "input must have a string `path` field".to_string(),
                });
            }
        };
        let content_str = match input.get("content").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => {
                return ToolOutcome::Failed(AivyxError::Tool {
                    tool: self.id,
                    detail: "input must have a string `content` field".to_string(),
                });
            }
        };
        let content_bytes = content_str.as_bytes();
        if content_bytes.len() > MAX_WRITE_BYTES {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: format!(
                    "content is {} bytes; limit is {MAX_WRITE_BYTES}",
                    content_bytes.len()
                ),
            });
        }

        // ---- Lexical resolve (mirrors FsReadTool::execute) -------
        let lexical_abs = match lexical_resolve(&self.sandbox_root, Path::new(path_str)) {
            Some(p) => p,
            None => {
                return ToolOutcome::Failed(AivyxError::Internal(format!(
                    "fs.write: lexical resolve escaped sandbox after scope gate \
                     admitted the call (path={path_str:?})"
                )));
            }
        };

        // ---- git-metadata block (unconditional) -------------------
        // Checked before any confirm-first/escalation logic, and never
        // gated by `[access] allow_sensitive_paths` — see
        // `touches_git_metadata`'s doc comment for why.
        if touches_git_metadata(&lexical_abs) {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: GIT_METADATA_WRITE_BLOCKED.to_string(),
            });
        }

        // ---- Chapter N: confirm-first when OVERWRITING -----------
        // A fresh write to a new path is not destructive and never gates;
        // clobbering an existing file is irreversible and needs
        // `confirmed: true` when the operator enabled confirm-first.
        if self.confirm_destructive && lexical_abs.exists() && !is_confirmed(&input) {
            return ToolOutcome::RequiresEscalation {
                reason: format!(
                    "overwriting {path_str:?} can't be undone. {DESTRUCTIVE_CONFIRM_HINT}"
                ),
                scope: None,
            };
        }

        // The lexical path has a parent (it's absolute and has at
        // least the sandbox-root components plus a file name). If it
        // doesn't, the input was "just the root" — refuse.
        let lexical_parent = match lexical_abs.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => {
                return ToolOutcome::Failed(AivyxError::Tool {
                    tool: self.id,
                    detail: format!(
                        "path {lexical_abs:?} has no parent directory — cannot \
                         write the sandbox root itself"
                    ),
                });
            }
        };
        let file_name = match lexical_abs.file_name() {
            Some(n) => n.to_owned(),
            None => {
                return ToolOutcome::Failed(AivyxError::Tool {
                    tool: self.id,
                    detail: format!(
                        "path {lexical_abs:?} has no final component (trailing \
                         slash?)"
                    ),
                });
            }
        };

        // ---- Ensure parent exists (within sandbox) ---------------
        //
        // `create_dir_all` is the right tool here but it could follow
        // a symlink that points outside the sandbox, creating
        // directories in unexpected places. We defend by first
        // verifying the lexical parent is still under the lexical
        // sandbox root (which `lexical_resolve` already guaranteed),
        // then running `create_dir_all`, then canonicalizing the
        // parent and re-verifying against the canonical sandbox root.
        // The canonical check catches any symlink in the parent
        // chain that escapes.
        if let Err(e) = std::fs::create_dir_all(&lexical_parent) {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: format!("cannot create parent {lexical_parent:?}: {e}"),
            });
        }

        // ---- Canonical fence on the parent -----------------------
        let canonical_parent = match std::fs::canonicalize(&lexical_parent) {
            Ok(p) => p,
            Err(e) => {
                return ToolOutcome::Failed(AivyxError::Tool {
                    tool: self.id,
                    detail: format!("cannot canonicalize parent {lexical_parent:?}: {e}"),
                });
            }
        };
        if !canonical_parent.starts_with(&*self.sandbox_root) {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: format!(
                    "parent {canonical_parent:?} escapes sandbox root {:?} \
                     after symlink resolution",
                    self.sandbox_root
                ),
            });
        }
        let canonical_target = canonical_parent.join(&file_name);

        // ---- git-metadata block, re-checked on the CANONICAL target ----
        //
        // The lexical check above only sees the input path as typed; a
        // symlink alias (`<sandbox>/link -> .git`) lets `path: "link/config"`
        // lexically resolve to a path with no `.git` component at all, while
        // still landing at `<sandbox>/.git/config` once the parent is
        // canonicalized through the symlink a few lines up. Re-running the
        // same unconditional predicate on `canonical_target` (post-symlink-
        // resolution, pre-mutation) closes that gap; the lexical check above
        // stays as a cheap early refusal for the common (non-symlink) case.
        if touches_git_metadata(&canonical_target) {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: GIT_METADATA_WRITE_BLOCKED.to_string(),
            });
        }

        // ---- Chapter Portcullis — sensitive-write guard -----------
        //
        // Refuse writes to secret + persistence locations (shell rc files,
        // ~/.ssh/authorized_keys, systemd/autostart/cron, git hooks, and the
        // read-sensitive set) even inside the sandbox — the persistence /
        // backdoor vector `confirm_destructive` only soft-gates. Checked on
        // the resolved parent + name (the file itself may not exist yet).
        if let Some(reason) = self.sensitive.classify_write(&canonical_target) {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: format!(
                    "refusing to write {} — {reason}. Add it to `[access] \
                     allow_sensitive_paths` if you intend the agent to write it.",
                    canonical_target.display()
                ),
            });
        }

        // If the target already exists as a symlink (not a regular
        // file), we must also canonicalize it and verify the resolved
        // path is still in-sandbox. Otherwise an attacker who got
        // `fs.write:<sandbox>/link` admitted could still redirect the
        // final write to wherever `<sandbox>/link` points by using
        // `rename` to replace it — except `rename` on Unix replaces
        // the symlink itself, not the target. But a previously-
        // existing symlink that we're about to overwrite via rename
        // is actually safe (rename replaces the symlink with the
        // temp file), whereas a previously-existing *regular file*
        // that the agent is legitimately overwriting is the happy
        // path. The one edge case is if `canonical_target` points at
        // a symlink that already exists *inside* the sandbox root
        // but resolves to something outside. For that, we check:
        if canonical_target.exists() {
            let target_canonical = match std::fs::canonicalize(&canonical_target) {
                Ok(p) => p,
                Err(e) => {
                    return ToolOutcome::Failed(AivyxError::Tool {
                        tool: self.id,
                        detail: format!(
                            "cannot canonicalize existing target {canonical_target:?}: {e}"
                        ),
                    });
                }
            };
            if !target_canonical.starts_with(&*self.sandbox_root) {
                return ToolOutcome::Failed(AivyxError::Tool {
                    tool: self.id,
                    detail: format!(
                        "existing target {target_canonical:?} escapes sandbox \
                         root {:?} after symlink resolution",
                        self.sandbox_root
                    ),
                });
            }
            // If the existing target is itself a symlink (even one
            // pointing in-sandbox), remove it before the atomic
            // rename so we replace the *link*, not whatever it points
            // at. Otherwise `rename(tmp, link)` would follow the link
            // and overwrite the pointed-to file, which is surprising.
            if canonical_target.is_symlink() {
                if let Err(e) = std::fs::remove_file(&canonical_target) {
                    return ToolOutcome::Failed(AivyxError::Tool {
                        tool: self.id,
                        detail: format!("cannot unlink existing symlink {canonical_target:?}: {e}"),
                    });
                }
            }
        }

        // ---- Atomic write: temp file in same dir + rename --------
        //
        // Temp file *must* live in the same directory as the target
        // so that `rename` is atomic (POSIX rename within a single
        // filesystem is atomic; cross-filesystem falls back to
        // copy+delete). Name includes a UUID to avoid collisions if
        // two concurrent `FsWriteTool` calls happen to touch the
        // same directory — registered tools are shared across turns
        // in `Arc<ToolRegistry>`, so concurrent turns calling
        // `fs.write` on the same file is a real scenario.
        let tmp_name = format!(".aivyx-fswrite-{}.tmp", uuid::Uuid::new_v4().simple());
        let tmp_path = canonical_parent.join(&tmp_name);

        // Use OpenOptions with `create_new` so we fail loudly if a
        // temp file with our UUID already exists (which shouldn't
        // happen, but makes the invariant explicit).
        use std::io::Write as IoWrite;
        let mut tmp_file = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
        {
            Ok(f) => f,
            Err(e) => {
                return ToolOutcome::Failed(AivyxError::Tool {
                    tool: self.id,
                    detail: format!("cannot create temp file {tmp_path:?}: {e}"),
                });
            }
        };
        if let Err(e) = tmp_file.write_all(content_bytes) {
            // Clean up the temp file before bailing.
            let _ = std::fs::remove_file(&tmp_path);
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: format!("write error on temp file {tmp_path:?}: {e}"),
            });
        }
        // Drop the file handle before rename — on Windows rename
        // would fail if the source is still open. Linux doesn't
        // require this, but dropping early is cheap and keeps the
        // code portable.
        drop(tmp_file);

        if let Err(e) = std::fs::rename(&tmp_path, &canonical_target) {
            // Rename failed — the temp file may still exist. Try to
            // clean it up; ignore failure there.
            let _ = std::fs::remove_file(&tmp_path);
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: format!("rename {tmp_path:?} → {canonical_target:?} failed: {e}"),
            });
        }

        // ---- Verification: re-stat and check size ---------------
        //
        // Verification::Verified requires that we confirm the effect.
        // A `metadata` call on the just-renamed target gives us the
        // on-disk byte count; compare against what we wrote. If the
        // stat fails or the count doesn't match, we still return
        // Completed (the write did happen) but mark the verification
        // as Unverified so the audit trail shows we couldn't confirm.
        let verified = match std::fs::metadata(&canonical_target) {
            Ok(md) if md.len() as usize == content_bytes.len() => Verification::Verified,
            _ => Verification::Unverified,
        };

        ToolOutcome::Completed {
            output: json!({
                "path": canonical_target.display().to_string(),
                "bytes": content_bytes.len(),
            }),
            verified,
        }
    }
}

// ---------------------------------------------------------------------------
// FsDeleteTool — Phase 100 task 3 (Chapter B, Amendment A11)
//
// Deletes a file, symlink, or *empty* directory under the sandbox root.
// Deletion is deliberately non-recursive: `remove_file` for files and
// symlinks, `remove_dir` for directories — and `remove_dir` returns an
// error on a non-empty directory rather than recursing. That error is
// surfaced verbatim; it is never escalated to `remove_dir_all`. Per
// PHASE_100.md Q3 the destructive blast radius is exactly one entry per
// call.
//
// Shares `lexical_resolve` with `FsReadTool` / `FsWriteTool` and runs
// the same parent-canonicalize fence `FsWriteTool` uses. The final path
// component is deliberately *not* canonicalize-followed: a symlink must
// be unlinked as the link, not as whatever it points at, so the entry
// is classified with `symlink_metadata` and a symlink is removed with
// `remove_file`.
//
// `fs.delete` is destructive. The registration-time trust gate that
// keeps it off SemiTrusted channels lives in the binary (Phase 100
// task 5), structurally parallel to `shell.exec`'s gate — the tool
// itself carries no tier logic.
// ---------------------------------------------------------------------------

fn delete_deny_scope() -> Scope {
    deny_scope_for("fs.delete")
}

fn delete_input_schema_value() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "Path to delete. Relative paths resolve \
                               against the agent's sandbox root; absolute \
                               paths must already be under it. Deletion is \
                               non-recursive — a non-empty directory is \
                               refused."
            },
            "confirmed": confirmed_schema_property()
        },
        "required": ["path"]
    })
}

/// Construction inputs for [`FsDeleteTool`]. Same split pattern as
/// [`FsReadToolConfig`] / [`FsWriteToolConfig`]: fallible
/// canonicalization at build time, infallible tool construction.
pub struct FsDeleteToolConfig {
    sandbox_root: PathBuf,
    confirm_destructive: bool,
    sensitive: Arc<crate::sensitive_paths::SensitivePolicy>,
}

impl FsDeleteToolConfig {
    pub fn new(sandbox_root: impl Into<PathBuf>) -> Self {
        FsDeleteToolConfig {
            sandbox_root: sandbox_root.into(),
            confirm_destructive: false,
            // Default disabled ⇒ byte-identical to pre-Portcullis behavior
            // until the binary wires in the operator's policy.
            sensitive: Arc::new(crate::sensitive_paths::SensitivePolicy::disabled()),
        }
    }

    /// Chapter N — require `confirmed: true` on every delete. Off by default.
    pub fn with_confirm_destructive(mut self, confirm: bool) -> Self {
        self.confirm_destructive = confirm;
        self
    }

    /// Chapter Portcullis — install the sensitive-write guard. Found missing
    /// 2026-07-07 via Chapter Almanac's guard-coverage audit: `FsReadTool`
    /// and `FsWriteTool` both check `SensitivePolicy`, but `FsDeleteTool`
    /// never did — a canonical secret/persistence path was one un-guarded
    /// `fs.delete` call away from permanent destruction, `confirm_destructive`
    /// notwithstanding (that's a self-declared flag, not a categorical block).
    pub fn with_sensitive_policy(
        mut self,
        policy: Arc<crate::sensitive_paths::SensitivePolicy>,
    ) -> Self {
        self.sensitive = policy;
        self
    }

    /// Canonicalize the sandbox root and return a ready-to-register
    /// [`FsDeleteTool`]. Fails at startup if the root doesn't exist or
    /// isn't a directory — configuration errors must not surface at
    /// tool-call time.
    pub fn build(self) -> Result<FsDeleteTool, AivyxError> {
        let canonical = std::fs::canonicalize(&self.sandbox_root).map_err(|e| {
            AivyxError::Config(format!(
                "fs.delete sandbox root {:?} cannot be canonicalized: {e}",
                self.sandbox_root
            ))
        })?;
        if !canonical.is_dir() {
            return Err(AivyxError::Config(format!(
                "fs.delete sandbox root {canonical:?} is not a directory"
            )));
        }
        Ok(FsDeleteTool {
            id: ToolId::new(),
            sandbox_root: Arc::from(canonical),
            schema: delete_input_schema_value(),
            confirm_destructive: self.confirm_destructive,
            sensitive: self.sensitive,
        })
    }
}

/// Reference filesystem delete tool. Agents holding
/// `fs.delete:<sandbox_root>/**` can delete any file, symlink, or
/// empty directory under the sandbox. Non-recursive: a non-empty
/// directory is refused, never wiped.
#[derive(Debug)]
pub struct FsDeleteTool {
    id: ToolId,
    sandbox_root: Arc<Path>,
    schema: Value,
    /// Chapter N — when true, every delete needs `confirmed: true`, from
    /// a turn after the refusal.
    confirm_destructive: bool,
    /// Chapter Portcullis — the sensitive-path write guard, checked
    /// alongside the sandbox fence so a protected path can't be deleted
    /// even inside the sandbox root.
    sensitive: Arc<crate::sensitive_paths::SensitivePolicy>,
}

impl FsDeleteTool {
    pub fn sandbox_root(&self) -> &Path {
        &self.sandbox_root
    }
}

#[async_trait]
impl Tool for FsDeleteTool {
    fn id(&self) -> ToolId {
        self.id
    }

    fn name(&self) -> &str {
        "fs.delete"
    }

    /// Deletes remove content from fs_root — checkpoint before every call.
    fn mutates_fs_root(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "Delete one of the operator's files, a symlink, or an empty directory under the agent's \
         sandbox root. Input is a JSON object with a `path` field \
         (relative paths resolve against the sandbox root; absolute \
         paths must already be under it). Deletion is non-recursive: a \
         file or symlink is unlinked directly, an empty directory is \
         removed, and a non-empty directory is refused. The sandbox \
         root itself cannot be deleted."
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn required_scope(&self, input: &Value) -> Scope {
        let Some(path_str) = input.get("path").and_then(|v| v.as_str()) else {
            return delete_deny_scope();
        };
        match lexical_resolve(&self.sandbox_root, Path::new(path_str)) {
            Some(abs) => Scope::parse(&format!("fs.delete:{}", abs.display()))
                .unwrap_or_else(delete_deny_scope),
            None => delete_deny_scope(),
        }
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        // ---- Validate input --------------------------------------
        let path_str = match input.get("path").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => {
                return ToolOutcome::Failed(AivyxError::Tool {
                    tool: self.id,
                    detail: "input must have a string `path` field".to_string(),
                });
            }
        };

        // ---- Lexical resolve (mirrors FsWriteTool::execute) ------
        let lexical_abs = match lexical_resolve(&self.sandbox_root, Path::new(path_str)) {
            Some(p) => p,
            None => {
                return ToolOutcome::Failed(AivyxError::Internal(format!(
                    "fs.delete: lexical resolve escaped sandbox after scope \
                     gate admitted the call (path={path_str:?})"
                )));
            }
        };

        // ---- git-metadata block (unconditional) -------------------
        // Same shared check as `FsWriteTool::execute` — see
        // `touches_git_metadata`'s doc comment.
        if touches_git_metadata(&lexical_abs) {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: GIT_METADATA_WRITE_BLOCKED.to_string(),
            });
        }

        // ---- Chapter N: confirm-first on this irreversible op ----
        // Keyed on the resolved path, so "todo.md" and its absolute form
        // are the same confirmation.
        if self.confirm_destructive && !is_confirmed(&input) {
            return ToolOutcome::RequiresEscalation {
                reason: format!("deleting {path_str:?} can't be undone. {DESTRUCTIVE_CONFIRM_HINT}"),
                scope: None,
            };
        }

        // Refuse to delete the sandbox root itself. Without this the
        // call would fail later with a confusing "parent escapes
        // sandbox" message (the root's parent is outside the sandbox);
        // catching it here gives the agent an actionable error.
        if lexical_abs.as_path() == &*self.sandbox_root {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: "cannot delete the sandbox root itself".to_string(),
            });
        }

        let lexical_parent = match lexical_abs.parent() {
            Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
            _ => {
                return ToolOutcome::Failed(AivyxError::Tool {
                    tool: self.id,
                    detail: format!("path {lexical_abs:?} has no parent directory"),
                });
            }
        };
        let file_name = match lexical_abs.file_name() {
            Some(n) => n.to_owned(),
            None => {
                return ToolOutcome::Failed(AivyxError::Tool {
                    tool: self.id,
                    detail: format!(
                        "path {lexical_abs:?} has no final component \
                         (trailing slash?)"
                    ),
                });
            }
        };

        // ---- Canonical fence on the parent -----------------------
        //
        // The parent directory must already exist — delete never
        // creates directories. Canonicalizing it resolves any symlink
        // in the parent chain; the result must still be under the
        // sandbox root. The final component is *not* canonicalize-
        // followed: a symlinked target is unlinked as the link.
        let canonical_parent = match std::fs::canonicalize(&lexical_parent) {
            Ok(p) => p,
            Err(e) => {
                return ToolOutcome::Failed(AivyxError::Tool {
                    tool: self.id,
                    detail: format!("cannot canonicalize parent {lexical_parent:?}: {e}"),
                });
            }
        };
        if !canonical_parent.starts_with(&*self.sandbox_root) {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: format!(
                    "parent {canonical_parent:?} escapes sandbox root {:?} \
                     after symlink resolution",
                    self.sandbox_root
                ),
            });
        }
        let target = canonical_parent.join(&file_name);

        // ---- git-metadata block, re-checked on the CANONICAL target ----
        // Same symlink-alias gap as `FsWriteTool::execute` — see that
        // call site's comment for the exploit shape.
        if touches_git_metadata(&target) {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: GIT_METADATA_WRITE_BLOCKED.to_string(),
            });
        }

        // ---- Chapter Portcullis — sensitive-write guard -----------
        //
        // Found missing 2026-07-07 (Chapter Almanac guard-coverage audit):
        // FsWriteTool checks this on the identical resolved-parent-+-name
        // shape, but FsDeleteTool never did, so a secret/persistence path
        // was one delete call away from permanent destruction even though
        // Ward/Portcullis already refuse to read or overwrite it.
        // `confirm_destructive` alone doesn't cover this — it's a
        // self-declared `confirmed: true` flag, not a categorical block.
        if let Some(reason) = self.sensitive.classify_write(&target) {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: format!(
                    "refusing to delete {} — {reason}. Add it to `[access] \
                     allow_sensitive_paths` if you intend the agent to delete it.",
                    target.display()
                ),
            });
        }

        // ---- Classify the entry without following a final symlink -
        let meta = match std::fs::symlink_metadata(&target) {
            Ok(m) => m,
            Err(e) => {
                return ToolOutcome::Failed(AivyxError::Tool {
                    tool: self.id,
                    detail: format!("cannot delete {target:?}: {e}"),
                });
            }
        };
        let ft = meta.file_type();

        // ---- Delete (non-recursive) ------------------------------
        let (kind, result) = if ft.is_symlink() {
            // `remove_file` unlinks the symlink itself, never its
            // target — the safe behavior for an in-sandbox link that
            // may point anywhere.
            ("symlink", std::fs::remove_file(&target))
        } else if ft.is_dir() {
            // `remove_dir` removes an *empty* directory and errors on
            // a non-empty one. That error is the non-recursive
            // guarantee in action — surfaced, never escalated.
            ("directory", std::fs::remove_dir(&target))
        } else {
            ("file", std::fs::remove_file(&target))
        };
        if let Err(e) = result {
            let hint = if kind == "directory" {
                " (directories must be empty — deletion is non-recursive)"
            } else {
                ""
            };
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: format!("cannot delete {kind} {target:?}: {e}{hint}"),
            });
        }

        // ---- Verification: the entry must no longer exist --------
        let verified = match std::fs::symlink_metadata(&target) {
            Err(_) => Verification::Verified,
            Ok(_) => Verification::Unverified,
        };

        ToolOutcome::Completed {
            output: json!({
                "path": target.display().to_string(),
                "deleted": true,
                "kind": kind,
            }),
            verified,
        }
    }
}

// ---------------------------------------------------------------------------
// FsMetadataTool — Phase 100 task 4 (Chapter B, Amendment A11)
//
// Read-only `stat` for a path under the sandbox root: size, kind,
// modified time, and permissions. On a *directory*, the call also
// returns the directory's entries — per PHASE_100.md Q1, directory
// listing folds into `fs.metadata` rather than spawning a separate
// `fs.list` tool and scope.
//
// Mirrors `FsReadTool`'s fence (it canonicalizes the path itself —
// the target must exist — and a final symlink is followed, the same
// way `fs.read` reads through a symlink to the file it names). No
// trust gate: inspecting metadata is non-destructive.
// ---------------------------------------------------------------------------

/// Maximum number of directory entries returned by a single
/// `fs.metadata` call on a directory. A directory with more entries
/// returns the first `MAX_DIR_ENTRIES` (sorted by name) plus
/// `"entries_truncated": true`. Same philosophy as [`MAX_READ_BYTES`]:
/// an agent cannot drain a 100k-entry directory into one LLM turn.
pub const MAX_DIR_ENTRIES: usize = 1024;

fn metadata_deny_scope() -> Scope {
    deny_scope_for("fs.metadata")
}

fn metadata_input_schema_value() -> Value {
    json!({
        "type": "object",
        "properties": {
            "path": {
                "type": "string",
                "description": "Path to inspect. Relative paths resolve \
                               against the agent's sandbox root; absolute \
                               paths must already be under it. On a \
                               directory, the call additionally returns the \
                               directory's entries."
            }
        },
        "required": ["path"]
    })
}

/// Construction inputs for [`FsMetadataTool`]. Same split pattern as
/// the other filesystem tools.
pub struct FsMetadataToolConfig {
    sandbox_root: PathBuf,
    sensitive: Arc<crate::sensitive_paths::SensitivePolicy>,
}

impl FsMetadataToolConfig {
    pub fn new(sandbox_root: impl Into<PathBuf>) -> Self {
        FsMetadataToolConfig {
            sandbox_root: sandbox_root.into(),
            // Default disabled ⇒ byte-identical to pre-Ward behavior until
            // the binary wires in the operator's policy.
            sensitive: Arc::new(crate::sensitive_paths::SensitivePolicy::disabled()),
        }
    }

    /// Chapter Ward — install the sensitive-path read guard. Found missing
    /// 2026-07-07 via Chapter Almanac's guard-coverage audit: `FsReadTool`
    /// checks this, but `FsMetadataTool` never did, so stat-ing a secret
    /// path (or listing a protected directory's filenames) was reachable
    /// at SemiTrusted even though reading its *content* is Ward-refused.
    pub fn with_sensitive_policy(
        mut self,
        policy: Arc<crate::sensitive_paths::SensitivePolicy>,
    ) -> Self {
        self.sensitive = policy;
        self
    }

    /// Canonicalize the sandbox root and return a ready-to-register
    /// [`FsMetadataTool`]. Fails at startup if the root doesn't exist
    /// or isn't a directory.
    pub fn build(self) -> Result<FsMetadataTool, AivyxError> {
        let canonical = std::fs::canonicalize(&self.sandbox_root).map_err(|e| {
            AivyxError::Config(format!(
                "fs.metadata sandbox root {:?} cannot be canonicalized: {e}",
                self.sandbox_root
            ))
        })?;
        if !canonical.is_dir() {
            return Err(AivyxError::Config(format!(
                "fs.metadata sandbox root {canonical:?} is not a directory"
            )));
        }
        Ok(FsMetadataTool {
            id: ToolId::new(),
            sandbox_root: Arc::from(canonical),
            schema: metadata_input_schema_value(),
            sensitive: self.sensitive,
        })
    }
}

/// Reference filesystem metadata tool. Agents holding
/// `fs.metadata:<sandbox_root>/**` can stat any path under the
/// sandbox and list any directory. Read-only.
#[derive(Debug)]
pub struct FsMetadataTool {
    id: ToolId,
    sandbox_root: Arc<Path>,
    schema: Value,
    /// Chapter Ward — the sensitive-path read guard, checked alongside
    /// the sandbox fence so a protected path can't be stat'd or have its
    /// directory entries listed even inside the sandbox root.
    sensitive: Arc<crate::sensitive_paths::SensitivePolicy>,
}

impl FsMetadataTool {
    pub fn sandbox_root(&self) -> &Path {
        &self.sandbox_root
    }
}

#[async_trait]
impl Tool for FsMetadataTool {
    fn id(&self) -> ToolId {
        self.id
    }

    fn name(&self) -> &str {
        "fs.metadata"
    }

    fn description(&self) -> &str {
        "Inspect a file or directory under the agent's sandbox root: \
         size, kind, last-modified time, and permissions. Input is a \
         JSON object with a `path` field (relative paths resolve \
         against the sandbox root; absolute paths must already be \
         under it). On a directory, the call also returns the \
         directory's entries (up to 1024, sorted). Read-only — \
         nothing is modified."
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn required_scope(&self, input: &Value) -> Scope {
        let Some(path_str) = input.get("path").and_then(|v| v.as_str()) else {
            return metadata_deny_scope();
        };
        match lexical_resolve(&self.sandbox_root, Path::new(path_str)) {
            Some(abs) => Scope::parse(&format!("fs.metadata:{}", abs.display()))
                .unwrap_or_else(metadata_deny_scope),
            None => metadata_deny_scope(),
        }
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        // ---- Validate input --------------------------------------
        let path_str = match input.get("path").and_then(|v| v.as_str()) {
            Some(s) => s,
            None => {
                return ToolOutcome::Failed(AivyxError::Tool {
                    tool: self.id,
                    detail: "input must have a string `path` field".to_string(),
                });
            }
        };

        // ---- Lexical resolve (mirrors FsReadTool::execute) -------
        let lexical_abs = match lexical_resolve(&self.sandbox_root, Path::new(path_str)) {
            Some(p) => p,
            None => {
                return ToolOutcome::Failed(AivyxError::Internal(format!(
                    "fs.metadata: lexical resolve escaped sandbox after scope \
                     gate admitted the call (path={path_str:?})"
                )));
            }
        };

        // ---- Canonical fence -------------------------------------
        //
        // The path must exist (you cannot stat what is not there).
        // Canonicalizing resolves every symlink; the result must
        // still live under the canonicalized sandbox root.
        let canonical = match std::fs::canonicalize(&lexical_abs) {
            Ok(p) => p,
            Err(e) => {
                return ToolOutcome::Failed(AivyxError::Tool {
                    tool: self.id,
                    detail: format!("cannot canonicalize {lexical_abs:?}: {e}"),
                });
            }
        };
        if !canonical.starts_with(&*self.sandbox_root) {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: format!(
                    "path {canonical:?} escapes sandbox root {:?} after \
                     symlink resolution",
                    self.sandbox_root
                ),
            });
        }

        // ---- Chapter Ward — sensitive-path guard -------------------
        //
        // Found missing 2026-07-07 (Chapter Almanac guard-coverage audit):
        // FsReadTool checks this on the identical canonical path, but
        // FsMetadataTool never did — size/mtime/permissions of a secret
        // file, or the filenames inside a protected directory, leaked at
        // SemiTrusted even though the read-only guard is Trusted+.
        if let Some(reason) = self.sensitive.classify(&canonical) {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: format!(
                    "refusing to inspect {} — {reason}. This is a protected \
                     location; add it to `[access] allow_sensitive_paths` if \
                     you intend the agent to inspect it.",
                    canonical.display()
                ),
            });
        }

        // ---- Stat ------------------------------------------------
        let md = match std::fs::metadata(&canonical) {
            Ok(m) => m,
            Err(e) => {
                return ToolOutcome::Failed(AivyxError::Tool {
                    tool: self.id,
                    detail: format!("cannot stat {canonical:?}: {e}"),
                });
            }
        };

        let modified_unix_secs: Option<u64> = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs());
        let readonly = md.permissions().readonly();
        #[cfg(unix)]
        let unix_mode: Option<u32> = {
            use std::os::unix::fs::PermissionsExt;
            Some(md.permissions().mode())
        };
        #[cfg(not(unix))]
        let unix_mode: Option<u32> = None;

        // ---- Directory: also list entries ------------------------
        if md.is_dir() {
            let mut entries: Vec<(String, &'static str)> = Vec::new();
            match std::fs::read_dir(&canonical) {
                Ok(rd) => {
                    for entry in rd.flatten() {
                        let name = entry.file_name().to_string_lossy().into_owned();
                        let kind = match entry.file_type() {
                            Ok(ft) if ft.is_dir() => "directory",
                            Ok(ft) if ft.is_file() => "file",
                            Ok(ft) if ft.is_symlink() => "symlink",
                            _ => "other",
                        };
                        entries.push((name, kind));
                    }
                }
                Err(e) => {
                    return ToolOutcome::Failed(AivyxError::Tool {
                        tool: self.id,
                        detail: format!("cannot read directory {canonical:?}: {e}"),
                    });
                }
            }
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            let entries_truncated = entries.len() > MAX_DIR_ENTRIES;
            entries.truncate(MAX_DIR_ENTRIES);
            let entries_json: Vec<Value> = entries
                .into_iter()
                .map(|(name, kind)| json!({"name": name, "kind": kind}))
                .collect();

            return ToolOutcome::Completed {
                output: json!({
                    "path": canonical.display().to_string(),
                    "kind": "directory",
                    "size_bytes": md.len(),
                    "modified_unix_secs": modified_unix_secs,
                    "readonly": readonly,
                    "unix_mode": unix_mode,
                    "entries": entries_json,
                    "entries_truncated": entries_truncated,
                }),
                // A stat is a pure query — no effect to verify.
                verified: Verification::NotApplicable,
            };
        }

        // ---- File (or other non-directory) -----------------------
        let kind = if md.is_file() { "file" } else { "other" };
        ToolOutcome::Completed {
            output: json!({
                "path": canonical.display().to_string(),
                "kind": kind,
                "size_bytes": md.len(),
                "modified_unix_secs": modified_unix_secs,
                "readonly": readonly,
                "unix_mode": unix_mode,
            }),
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

    use std::fs;
    use std::io::Write;

    use crate::MessageOrigin;
    use aivyx_capability::{CapabilitySet, TrustTier};

    // ---- touches_git_metadata ========================================

    #[test]
    fn touches_git_metadata_true_for_dotgit_config() {
        assert!(touches_git_metadata(Path::new("/r/.git/config")));
    }

    #[test]
    fn touches_git_metadata_true_for_bare_dotgit() {
        assert!(touches_git_metadata(Path::new("/r/.git")));
    }

    #[test]
    fn touches_git_metadata_true_for_nested_dotgit_hooks() {
        assert!(touches_git_metadata(Path::new("/r/sub/.git/hooks/x")));
    }

    #[test]
    fn touches_git_metadata_false_for_dotgithub_workflow() {
        assert!(!touches_git_metadata(Path::new("/r/.github/w.yml")));
    }

    #[test]
    fn touches_git_metadata_false_for_dotgitignore() {
        assert!(!touches_git_metadata(Path::new("/r/.gitignore")));
    }

    #[test]
    fn touches_git_metadata_false_for_dotgitattributes() {
        assert!(!touches_git_metadata(Path::new("/r/.gitattributes")));
    }

    #[test]
    fn touches_git_metadata_true_for_uppercase_dotgit() {
        // Case-insensitive filesystems (macOS, shipped) resolve `.GIT` and
        // `.git` to the same directory — the component compare must not
        // be case-sensitive.
        assert!(touches_git_metadata(Path::new("/r/.GIT/config")));
    }

    #[test]
    fn touches_git_metadata_true_for_mixed_case_dotgit() {
        assert!(touches_git_metadata(Path::new("/r/.Git/hooks/x")));
    }

    #[test]
    fn touches_git_metadata_false_for_dotgithub_uppercase_is_still_distinct() {
        // `.GITHUB` must not collide with `.git` even case-insensitively —
        // it's a different (7-char) component, not a case variant of the
        // 4-char `.git`.
        assert!(!touches_git_metadata(Path::new("/r/.GITHUB/w.yml")));
    }

    /// RAII temp directory — creates `$TMPDIR/aivyx-fs-test-<uuid>/root`
    /// on construction, removes the whole tree on drop. Rolled here to
    /// avoid adding `tempfile` as a dep for ~50 lines of test hygiene.
    struct SandboxDir {
        root: PathBuf,
        _parent: PathBuf,
    }

    impl SandboxDir {
        fn new() -> Self {
            let tmp = std::env::var("TMPDIR")
                .or_else(|_| std::env::var("TEMP"))
                .unwrap_or_else(|_| "/tmp".to_string());
            let parent = PathBuf::from(tmp).join(format!("aivyx-fs-test-{}", uuid::Uuid::new_v4()));
            let root = parent.join("root");
            fs::create_dir_all(&root).expect("test sandbox root must be creatable");
            SandboxDir {
                root,
                _parent: parent,
            }
        }

        fn write_file(&self, rel: &str, contents: &[u8]) -> PathBuf {
            let p = self.root.join(rel);
            if let Some(parent) = p.parent() {
                fs::create_dir_all(parent).expect("mkdir -p parent");
            }
            let mut f = fs::File::create(&p).expect("create test file");
            f.write_all(contents).expect("write test file");
            p
        }
    }

    impl Drop for SandboxDir {
        fn drop(&mut self) {
            // Best-effort cleanup. A failed cleanup in a test harness
            // is a warning, not a fatal error.
            let _ = fs::remove_dir_all(&self._parent);
        }
    }

    fn build_tool(sandbox: &SandboxDir) -> FsReadTool {
        FsReadToolConfig::new(sandbox.root.clone())
            .build()
            .expect("sandbox root must be canonicalizable for tests")
    }

    /// The integration test in task 5 will drive `execute` through the
    /// full turn loop; unit tests need a minimal `ToolContext`. We
    /// don't run the execute path against `ctx` fields other than the
    /// cancellation token, so the cheapest fake is a channel and audit
    /// hook that do nothing. Import them from the core test helpers.
    fn run_execute(tool: &dyn Tool, input: Value) -> ToolOutcome {
        run_execute_in(tool, input, crate::SessionId::new(), crate::TurnId::new())
    }

    /// `run_execute` in a given session and turn — for the confirm-first
    /// rule, which depends on which turn a refusal happened in.
    fn run_execute_in(
        tool: &dyn Tool,
        input: Value,
        session: crate::SessionId,
        turn: crate::TurnId,
    ) -> ToolOutcome {
        use crate::{AgentId, CancellationToken, NullAuditHook, SessionId};

        // A minimal `ChannelContext` that ignores every call. This is
        // fine for a unit test that only exercises `execute`'s
        // filesystem and scope behavior — the channel is never touched.
        struct NoopChannel {
            session: SessionId,
            token: CancellationToken,
        }

        #[async_trait]
        impl crate::ChannelContext for NoopChannel {
            fn channel_name(&self) -> &str {
                "test"
            }
            fn platform(&self) -> crate::ChannelPlatform {
                crate::ChannelPlatform::Local
            }
            fn trust_tier(&self) -> aivyx_capability::TrustTier {
                aivyx_capability::TrustTier::Trusted
            }
            fn session_id(&self) -> SessionId {
                self.session
            }
            async fn stream_event(
                &self,
                _event: crate::StreamEvent<'_>,
            ) -> Result<(), crate::ChannelError> {
                Ok(())
            }
            async fn finalize(
                &self,
                _outcome: &crate::TurnOutcome,
            ) -> Result<(), crate::ChannelError> {
                Ok(())
            }
            fn cancellation_token(&self) -> CancellationToken {
                self.token.clone()
            }
        }

        let channel = NoopChannel {
            session,
            token: CancellationToken::new(),
        };
        let audit = NullAuditHook;
        let ctx = ToolContext {
            agent_id: AgentId::new(),
            session_id: channel.session,
            turn_id: turn,
            channel: &channel,
            audit: &audit,
            cancellation: &channel.token,
            message_origin: MessageOrigin::Operator,
        };

        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(tool.execute(input, &ctx))
    }

    // ---- Construction / config --------------------------------------

    #[test]
    fn build_fails_if_root_does_not_exist() {
        let err = FsReadToolConfig::new("/definitely/not/a/real/path/aivyx-test")
            .build()
            .expect_err("nonexistent root must fail to build");
        assert!(matches!(err, AivyxError::Config(_)));
    }

    #[test]
    fn build_fails_if_root_is_a_file_not_directory() {
        let sandbox = SandboxDir::new();
        let file = sandbox.write_file("not-a-dir", b"x");
        let err = FsReadToolConfig::new(file)
            .build()
            .expect_err("file-as-root must fail to build");
        assert!(matches!(err, AivyxError::Config(_)));
    }

    #[test]
    fn sandbox_root_is_pre_canonicalized() {
        let sandbox = SandboxDir::new();
        let tool = build_tool(&sandbox);
        // The canonicalized root must be absolute and must exist.
        assert!(tool.sandbox_root().is_absolute());
        assert!(tool.sandbox_root().exists());
    }

    // ---- required_scope (lexical layer) -----------------------------

    #[test]
    fn scope_for_relative_path_inside_sandbox() {
        let sandbox = SandboxDir::new();
        sandbox.write_file("notes/today.md", b"hi");
        let tool = build_tool(&sandbox);

        let scope = tool.required_scope(&json!({"path": "notes/today.md"}));
        assert_eq!(scope.base(), "fs.read");
        let q = scope.qualifier().expect("must have a qualifier");
        assert!(q.ends_with("/notes/today.md"));
        assert!(q.starts_with(&tool.sandbox_root().display().to_string()));
    }

    #[test]
    fn scope_for_dot_slash_path_is_accepted() {
        // `./notes/today.md` is equivalent to `notes/today.md`. The
        // `CurDir` component handler skips the `.`.
        let sandbox = SandboxDir::new();
        let tool = build_tool(&sandbox);
        let scope = tool.required_scope(&json!({"path": "./notes/today.md"}));
        let q = scope.qualifier().expect("qualifier");
        assert!(q.ends_with("/notes/today.md"));
    }

    #[test]
    fn scope_for_traversal_input_is_deny_scope() {
        // The attack: `../../etc/passwd` lexically escapes the sandbox
        // root. Must produce the deny scope so the loop's scope gate
        // denies the call.
        let sandbox = SandboxDir::new();
        let tool = build_tool(&sandbox);
        let scope = tool.required_scope(&json!({"path": "../../etc/passwd"}));
        let q = scope.qualifier().expect("deny scope has a qualifier");
        assert!(
            q.contains("__deny__"),
            "traversal input must yield deny scope, got {q:?}"
        );
    }

    #[test]
    fn scope_for_absolute_outside_sandbox_is_deny_scope() {
        // Absolute `/etc/passwd` replaces the sandbox prefix via
        // `PathBuf::push` semantics. The post-collapse prefix check
        // catches it.
        let sandbox = SandboxDir::new();
        let tool = build_tool(&sandbox);
        let scope = tool.required_scope(&json!({"path": "/etc/passwd"}));
        let q = scope.qualifier().expect("deny scope has a qualifier");
        assert!(q.contains("__deny__"));
    }

    #[test]
    fn scope_for_absolute_inside_sandbox_is_accepted() {
        // An absolute path that's already under the sandbox root
        // should be accepted — the agent might have learned the full
        // path from a prior tool call.
        let sandbox = SandboxDir::new();
        sandbox.write_file("notes/today.md", b"hi");
        let tool = build_tool(&sandbox);
        let abs = tool.sandbox_root().join("notes/today.md");
        let scope = tool.required_scope(&json!({"path": abs.display().to_string()}));
        let q = scope.qualifier().expect("qualifier");
        assert!(q.ends_with("/notes/today.md"));
        assert!(!q.contains("__deny__"));
    }

    #[test]
    fn scope_for_missing_path_field_is_deny_scope() {
        let sandbox = SandboxDir::new();
        let tool = build_tool(&sandbox);
        let scope = tool.required_scope(&json!({"not_path": "hi"}));
        let q = scope.qualifier().expect("deny scope has a qualifier");
        assert!(q.contains("__deny__"));
    }

    #[test]
    fn scope_is_pure_no_io() {
        // `required_scope` must not hit the filesystem. Prove it by
        // calling it for a path that *doesn't exist* — if the tool
        // were calling `canonicalize` under the hood, this would fail.
        // Since we use lexical resolution only, it must produce a
        // valid in-sandbox scope.
        let sandbox = SandboxDir::new();
        let tool = build_tool(&sandbox);
        let scope = tool.required_scope(&json!({"path": "does/not/exist.txt"}));
        let q = scope.qualifier().expect("qualifier");
        assert!(q.ends_with("/does/not/exist.txt"));
        assert!(!q.contains("__deny__"));
    }

    // ---- required_scope intersects with a sandbox capability -------

    #[test]
    fn sandbox_capability_grants_derived_in_sandbox_scope() {
        // End-to-end scope-layer check: an agent holding
        // `fs.read:<canonical_sandbox>/**` must cover a derived scope
        // for any path under the sandbox.
        let sandbox = SandboxDir::new();
        sandbox.write_file("notes/today.md", b"hi");
        let tool = build_tool(&sandbox);

        let held_pattern = format!("fs.read:{}/**", tool.sandbox_root().display());
        let held = CapabilitySet::from_scopes([Scope::parse(&held_pattern).unwrap()]);
        let effective = held.intersect(TrustTier::Trusted.default_ceiling());

        let needed = tool.required_scope(&json!({"path": "notes/today.md"}));
        assert!(
            effective.grants(&needed),
            "sandbox capability must grant in-sandbox read, got needed={needed:?}"
        );
    }

    #[test]
    fn sandbox_capability_denies_traversal_scope() {
        let sandbox = SandboxDir::new();
        let tool = build_tool(&sandbox);

        let held_pattern = format!("fs.read:{}/**", tool.sandbox_root().display());
        let held = CapabilitySet::from_scopes([Scope::parse(&held_pattern).unwrap()]);
        let effective = held.intersect(TrustTier::Trusted.default_ceiling());

        let attack_needed = tool.required_scope(&json!({"path": "../../etc/passwd"}));
        assert!(
            !effective.grants(&attack_needed),
            "traversal scope must not be granted; needed was {attack_needed:?}"
        );
    }

    // ---- execute (canonical layer + read) ---------------------------

    #[test]
    fn execute_happy_path_reads_file() {
        let sandbox = SandboxDir::new();
        sandbox.write_file("notes/today.md", b"hello world");
        let tool = build_tool(&sandbox);

        let outcome = run_execute(&tool, json!({"path": "notes/today.md"}));
        match outcome {
            ToolOutcome::Completed { output, verified } => {
                assert!(matches!(verified, Verification::NotApplicable));
                assert_eq!(output["text"], json!("hello world"));
                assert_eq!(output["bytes"], json!(11));
                assert_eq!(output["truncated"], json!(false));
                assert_eq!(output["binary"], json!(false));
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn execute_on_nonexistent_file_fails() {
        let sandbox = SandboxDir::new();
        let tool = build_tool(&sandbox);

        let outcome = run_execute(&tool, json!({"path": "missing.txt"}));
        match outcome {
            ToolOutcome::Failed(AivyxError::Tool { detail, .. }) => {
                assert!(
                    detail.contains("canonicalize"),
                    "expected canonicalize failure, got {detail}"
                );
            }
            other => panic!("expected Failed(Tool), got {other:?}"),
        }
    }

    #[test]
    fn execute_truncates_large_file() {
        let sandbox = SandboxDir::new();
        // A file bigger than the cap.
        let big = vec![b'A'; MAX_READ_BYTES + 1024];
        sandbox.write_file("big.bin", &big);
        let tool = build_tool(&sandbox);

        let outcome = run_execute(&tool, json!({"path": "big.bin"}));
        match outcome {
            ToolOutcome::Completed { output, .. } => {
                assert_eq!(output["bytes"], json!(MAX_READ_BYTES));
                assert_eq!(output["truncated"], json!(true));
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    fn build_guarded_tool(
        sandbox: &SandboxDir,
        policy: crate::sensitive_paths::SensitivePolicy,
    ) -> FsReadTool {
        FsReadToolConfig::new(sandbox.root.clone())
            .with_sensitive_policy(Arc::new(policy))
            .build()
            .expect("sandbox root canonicalizable")
    }

    #[test]
    fn fs_read_output_is_untrusted_for_bulwark() {
        // A file's contents are fenced as untrusted (prompt-injection defense).
        let sandbox = SandboxDir::new();
        assert!(build_tool(&sandbox).output_is_untrusted());
    }

    #[test]
    fn fs_write_mutates_fs_root() {
        let sandbox = SandboxDir::new();
        assert!(build_write_tool(&sandbox).mutates_fs_root());
    }

    #[test]
    fn fs_delete_mutates_fs_root() {
        let sandbox = SandboxDir::new();
        assert!(build_delete_tool(&sandbox).mutates_fs_root());
    }

    #[test]
    fn fs_read_does_not_mutate_fs_root() {
        // The default (false) — fs.read is unmodified by this plan, proving
        // the trait's default polarity without touching FsReadTool's impl.
        let sandbox = SandboxDir::new();
        assert!(!build_tool(&sandbox).mutates_fs_root());
    }

    #[test]
    fn ward_refuses_a_sensitive_file_inside_the_sandbox() {
        use crate::sensitive_paths::SensitivePolicy;
        let sandbox = SandboxDir::new();
        // A secret and an ordinary file, both inside the sandbox root.
        sandbox.write_file(".env", b"API_KEY=super-secret\n");
        sandbox.write_file("notes.md", b"hello\n");
        let tool = build_guarded_tool(&sandbox, SensitivePolicy::new(vec![], vec![]));

        // The secret is refused even though it's inside the sandbox…
        match run_execute(&tool, json!({ "path": ".env" })) {
            ToolOutcome::Failed(AivyxError::Tool { detail, .. }) => {
                assert!(detail.contains("protected"), "reason: {detail}");
                assert!(
                    !detail.contains("super-secret"),
                    "refusal must not leak contents: {detail}"
                );
            }
            other => panic!("expected Failed for .env, got {other:?}"),
        }
        // …while an ordinary file reads fine.
        assert!(matches!(
            run_execute(&tool, json!({ "path": "notes.md" })),
            ToolOutcome::Completed { .. }
        ));
    }

    #[test]
    fn ward_allowlist_permits_a_named_secret() {
        use crate::sensitive_paths::SensitivePolicy;
        let sandbox = SandboxDir::new();
        sandbox.write_file(".env", b"API_KEY=ok-to-read\n");
        // Allow-list the sandbox root → its .env is readable again.
        let policy = SensitivePolicy::new(vec![sandbox.root.canonicalize().unwrap()], vec![]);
        let tool = build_guarded_tool(&sandbox, policy);
        assert!(matches!(
            run_execute(&tool, json!({ "path": ".env" })),
            ToolOutcome::Completed { .. }
        ));
    }

    #[test]
    fn execute_binary_file_flags_and_lossy_decodes() {
        let sandbox = SandboxDir::new();
        // Invalid UTF-8 sequence.
        sandbox.write_file("blob.bin", &[0xFF, 0xFE, 0xFD, 0xFC]);
        let tool = build_tool(&sandbox);

        let outcome = run_execute(&tool, json!({"path": "blob.bin"}));
        match outcome {
            ToolOutcome::Completed { output, .. } => {
                assert_eq!(output["binary"], json!(true));
                assert!(output["text"].is_string());
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    #[cfg(unix)]
    fn execute_denies_symlink_that_escapes_sandbox() {
        // The symlink attack: create `<sandbox>/escape → /etc/passwd`.
        // `required_scope` produces the in-sandbox scope
        // `fs.read:<sandbox>/escape`, which the scope gate admits.
        // `execute` must then canonicalize and refuse because the
        // resolved path is outside the sandbox root.
        use std::os::unix::fs::symlink;

        let sandbox = SandboxDir::new();
        let target = "/etc/passwd";
        if !Path::new(target).exists() {
            // Skip on hosts without /etc/passwd (e.g., some minimal
            // containers). The symlink still demonstrates the escape
            // using whatever absolute file we can find, but if
            // /etc/passwd is missing the test is uninteresting.
            return;
        }
        let link = sandbox.root.join("escape");
        symlink(target, &link).expect("can create test symlink");
        let tool = build_tool(&sandbox);

        let outcome = run_execute(&tool, json!({"path": "escape"}));
        match outcome {
            ToolOutcome::Failed(AivyxError::Tool { detail, .. }) => {
                assert!(
                    detail.contains("escapes sandbox"),
                    "expected sandbox-escape failure, got {detail}"
                );
            }
            other => panic!("symlink escape must be refused by the canonical fence, got {other:?}"),
        }
    }

    #[test]
    #[cfg(unix)]
    fn execute_allows_symlink_that_stays_in_sandbox() {
        // The legitimate case: a symlink inside the sandbox pointing
        // to another file inside the sandbox. Canonicalization
        // resolves it, the resolved path is still in-sandbox, and the
        // read succeeds.
        use std::os::unix::fs::symlink;

        let sandbox = SandboxDir::new();
        let real = sandbox.write_file("real.md", b"inside content");
        let link = sandbox.root.join("alias");
        symlink(&real, &link).expect("can create intra-sandbox symlink");
        let tool = build_tool(&sandbox);

        let outcome = run_execute(&tool, json!({"path": "alias"}));
        match outcome {
            ToolOutcome::Completed { output, .. } => {
                assert_eq!(output["text"], json!("inside content"));
            }
            other => panic!("intra-sandbox symlink must succeed, got {other:?}"),
        }
    }

    // ---- FsWriteTool =================================================

    fn build_write_tool(sandbox: &SandboxDir) -> FsWriteTool {
        FsWriteToolConfig::new(sandbox.root.clone())
            .build()
            .expect("sandbox root must be canonicalizable for write tests")
    }

    #[test]
    fn write_refuses_dotgit_config_even_with_sensitive_guard_disabled() {
        // No `with_sensitive_policy` — Portcullis is off by default. The
        // `.git` metadata block must fire anyway: it's unconditional, not
        // an `[access] allow_sensitive_paths`-style opt-out-able guard.
        let sandbox = SandboxDir::new();
        let tool = build_write_tool(&sandbox);

        let outcome = run_execute(
            &tool,
            json!({"path": ".git/config", "content": "[core]\n\tfsmonitor = /tmp/x\n"}),
        );
        match outcome {
            ToolOutcome::Failed(AivyxError::Tool { detail, .. }) => {
                assert!(detail.contains("Writing inside .git is blocked"), "{detail}");
            }
            other => panic!("expected refusal for .git/config, got {other:?}"),
        }
        assert!(
            !sandbox.root.join(".git/config").exists(),
            ".git/config must not have been created"
        );
    }

    #[test]
    fn write_refuses_nested_dotgit_hooks_path() {
        let sandbox = SandboxDir::new();
        let tool = build_write_tool(&sandbox);

        let outcome = run_execute(
            &tool,
            json!({"path": "sub/.git/hooks/post-commit", "content": "#!/bin/sh\nrm -rf ~\n"}),
        );
        assert!(matches!(outcome, ToolOutcome::Failed(AivyxError::Tool { .. })));
        assert!(!sandbox.root.join("sub/.git/hooks/post-commit").exists());
    }

    #[test]
    fn write_still_allows_dotgithub_and_dotgitignore() {
        let sandbox = SandboxDir::new();
        let tool = build_write_tool(&sandbox);

        assert!(matches!(
            run_execute(
                &tool,
                json!({"path": ".github/workflows/ci.yml", "content": "name: ci\n"})
            ),
            ToolOutcome::Completed { .. }
        ));
        assert!(matches!(
            run_execute(&tool, json!({"path": ".gitignore", "content": "/target\n"})),
            ToolOutcome::Completed { .. }
        ));
    }

    #[test]
    #[cfg(unix)]
    fn write_refuses_dotgit_via_a_symlink_alias() {
        // `<sandbox>/.git/` is a real dir; `<sandbox>/link -> .git` is a
        // symlink alias. `path: "link/config"` lexically resolves to
        // `<sandbox>/link/config` -- no `.git` *component* in the lexical
        // path at all -- so the lexical-only check misses it, and the
        // write lands at the canonical target `<sandbox>/.git/config`
        // once the parent is canonicalized through the symlink.
        use std::os::unix::fs::symlink;

        let sandbox = SandboxDir::new();
        std::fs::create_dir_all(sandbox.root.join(".git")).unwrap();
        let link = sandbox.root.join("link");
        symlink(sandbox.root.join(".git"), &link).expect("can create alias symlink");

        let tool = build_write_tool(&sandbox);
        let outcome = run_execute(
            &tool,
            json!({"path": "link/config", "content": "[core]\n\tfsmonitor = /tmp/x\n"}),
        );
        match outcome {
            ToolOutcome::Failed(AivyxError::Tool { detail, .. }) => {
                assert!(detail.contains("Writing inside .git is blocked"), "{detail}");
            }
            other => panic!("expected refusal for link/config, got {other:?}"),
        }
        assert!(
            !sandbox.root.join(".git/config").exists(),
            ".git/config must not have been created via the symlink alias"
        );
    }

    #[test]
    fn portcullis_refuses_writes_to_persistence_and_secret_paths() {
        use crate::sensitive_paths::SensitivePolicy;
        let sandbox = SandboxDir::new();
        let tool = FsWriteToolConfig::new(sandbox.root.clone())
            .with_sensitive_policy(Arc::new(SensitivePolicy::new(vec![], vec![])))
            .build()
            .expect("build guarded write tool");

        // A persistence write (shell rc) inside the sandbox is refused…
        match run_execute(
            &tool,
            json!({ "path": ".bashrc", "content": "evil() { :; }" }),
        ) {
            ToolOutcome::Failed(AivyxError::Tool { detail, .. }) => {
                assert!(detail.contains("refusing to write"), "{detail}");
                assert!(
                    detail.contains("persistence") || detail.contains("startup"),
                    "{detail}"
                );
            }
            other => panic!("expected refusal for .bashrc, got {other:?}"),
        }
        // …and a secret write too.
        assert!(matches!(
            run_execute(&tool, json!({ "path": ".netrc", "content": "machine x" })),
            ToolOutcome::Failed(AivyxError::Tool { .. })
        ));
        // An ordinary write succeeds.
        assert!(matches!(
            run_execute(&tool, json!({ "path": "notes.md", "content": "hello" })),
            ToolOutcome::Completed { .. }
        ));
    }

    // ---- FsWriteTool: construction + config -----------------------

    #[test]
    fn write_build_fails_if_root_does_not_exist() {
        let err = FsWriteToolConfig::new("/definitely/not/a/real/aivyx-write-root")
            .build()
            .expect_err("nonexistent write root must fail to build");
        assert!(matches!(err, AivyxError::Config(_)));
    }

    #[test]
    fn write_build_fails_if_root_is_a_file() {
        let sandbox = SandboxDir::new();
        let file = sandbox.write_file("not-a-dir", b"x");
        let err = FsWriteToolConfig::new(file)
            .build()
            .expect_err("file-as-root must fail");
        assert!(matches!(err, AivyxError::Config(_)));
    }

    #[test]
    fn write_tool_descriptor_fields_are_what_the_planner_expects() {
        // The LLM planner builds its tool descriptor list by calling
        // `name`, `description`, and `input_schema` on every
        // registered tool. A regression on any of these three — a
        // rename, a docstring drift, a broken schema builder —
        // silently corrupts what the LLM sees, with no compile-time
        // signal. Pin them here.
        let sandbox = SandboxDir::new();
        let tool = build_write_tool(&sandbox);
        assert_eq!(tool.name(), "fs.write");
        let schema = tool.input_schema();
        assert_eq!(schema["type"], json!("object"));
        assert_eq!(schema["required"], json!(["path", "content"]));
        assert!(schema["properties"]["path"].is_object());
        assert!(schema["properties"]["content"].is_object());
    }

    // ---- FsWriteTool: required_scope (lexical layer) --------------

    #[test]
    fn write_scope_for_relative_path_inside_sandbox() {
        let sandbox = SandboxDir::new();
        let tool = build_write_tool(&sandbox);
        let scope = tool.required_scope(&json!({"path": "notes/today.md"}));
        assert_eq!(scope.base(), "fs.write");
        let q = scope.qualifier().expect("qualifier");
        assert!(q.ends_with("/notes/today.md"));
        assert!(q.starts_with(&tool.sandbox_root().display().to_string()));
        assert!(!q.contains("__deny__"));
    }

    #[test]
    fn write_scope_for_traversal_is_deny_scope() {
        let sandbox = SandboxDir::new();
        let tool = build_write_tool(&sandbox);
        let scope = tool.required_scope(&json!({"path": "../../etc/passwd"}));
        let q = scope.qualifier().expect("qualifier");
        assert_eq!(scope.base(), "fs.write");
        assert!(q.contains("__deny__"));
    }

    #[test]
    fn write_scope_missing_path_is_deny_scope() {
        let sandbox = SandboxDir::new();
        let tool = build_write_tool(&sandbox);
        let scope = tool.required_scope(&json!({"content": "hi"}));
        assert!(scope.qualifier().unwrap().contains("__deny__"));
    }

    #[test]
    fn write_scope_missing_content_is_still_admitted_for_scope_check() {
        // `required_scope` cares only about *where* the agent wants
        // to write. Missing content is an execute-time failure, not
        // a scope denial — the agent learns from the tool error text
        // rather than from a silent denial.
        let sandbox = SandboxDir::new();
        let tool = build_write_tool(&sandbox);
        let scope = tool.required_scope(&json!({"path": "notes/today.md"}));
        assert!(!scope.qualifier().unwrap().contains("__deny__"));
    }

    #[test]
    fn write_scope_base_is_fs_write_not_fs_read() {
        // Sanity: deny and happy-path must both report the `fs.write`
        // base so audit trails distinguish the two tools' denials.
        let sandbox = SandboxDir::new();
        let tool = build_write_tool(&sandbox);
        assert_eq!(
            tool.required_scope(&json!({"path": "../../etc/passwd"}))
                .base(),
            "fs.write"
        );
        assert_eq!(
            tool.required_scope(&json!({"path": "notes/today.md"}))
                .base(),
            "fs.write"
        );
    }

    // ---- FsWriteTool: execute (canonical + atomic rename) ---------

    #[test]
    fn write_happy_path_creates_file_with_content() {
        let sandbox = SandboxDir::new();
        let tool = build_write_tool(&sandbox);

        let outcome = run_execute(
            &tool,
            json!({"path": "hello.txt", "content": "hello world"}),
        );
        match outcome {
            ToolOutcome::Completed { output, verified } => {
                assert!(matches!(verified, Verification::Verified));
                assert_eq!(output["bytes"], json!(11));
                let abs = sandbox.root.join("hello.txt");
                let actual = std::fs::read_to_string(&abs).expect("file exists");
                assert_eq!(actual, "hello world");
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn write_creates_parent_directories_within_sandbox() {
        let sandbox = SandboxDir::new();
        let tool = build_write_tool(&sandbox);
        let outcome = run_execute(
            &tool,
            json!({"path": "deep/nested/path/note.md", "content": "n"}),
        );
        assert!(matches!(outcome, ToolOutcome::Completed { .. }));
        let abs = sandbox.root.join("deep/nested/path/note.md");
        assert!(abs.exists());
        assert_eq!(std::fs::read_to_string(&abs).unwrap(), "n");
    }

    #[test]
    fn write_overwrites_existing_file() {
        let sandbox = SandboxDir::new();
        sandbox.write_file("existing.txt", b"old content");
        let tool = build_write_tool(&sandbox);

        let outcome = run_execute(
            &tool,
            json!({"path": "existing.txt", "content": "new content"}),
        );
        assert!(matches!(outcome, ToolOutcome::Completed { .. }));
        let abs = sandbox.root.join("existing.txt");
        assert_eq!(std::fs::read_to_string(&abs).unwrap(), "new content");
    }

    #[test]
    fn write_atomic_temp_file_is_cleaned_up_on_success() {
        // After a successful write, the sandbox directory should
        // contain the target file and NOT any `.aivyx-fswrite-*.tmp`
        // sibling. The temp-then-rename approach should leave no
        // breadcrumbs on the happy path.
        let sandbox = SandboxDir::new();
        let tool = build_write_tool(&sandbox);

        let _ = run_execute(&tool, json!({"path": "clean.txt", "content": "hi"}));

        let stray_tmp: Vec<_> = std::fs::read_dir(&sandbox.root)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(".aivyx-fswrite-")
            })
            .collect();
        assert!(
            stray_tmp.is_empty(),
            "temp file must be renamed away on success"
        );
    }

    #[test]
    fn write_missing_content_field_fails_with_tool_error() {
        let sandbox = SandboxDir::new();
        let tool = build_write_tool(&sandbox);
        let outcome = run_execute(&tool, json!({"path": "x.txt"}));
        match outcome {
            ToolOutcome::Failed(AivyxError::Tool { detail, .. }) => {
                assert!(detail.contains("content"), "detail: {detail}");
            }
            other => panic!("expected Failed(Tool), got {other:?}"),
        }
    }

    #[test]
    fn write_oversize_content_is_refused() {
        let sandbox = SandboxDir::new();
        let tool = build_write_tool(&sandbox);
        // One byte over the cap.
        let big: String = "A".repeat(MAX_WRITE_BYTES + 1);
        let outcome = run_execute(&tool, json!({"path": "too-big.txt", "content": big}));
        match outcome {
            ToolOutcome::Failed(AivyxError::Tool { detail, .. }) => {
                assert!(detail.contains("limit"), "detail: {detail}");
            }
            other => panic!("expected oversize Failed, got {other:?}"),
        }
        // And no file was created.
        assert!(!sandbox.root.join("too-big.txt").exists());
    }

    #[test]
    #[cfg(unix)]
    fn write_denies_target_that_is_symlink_escaping_sandbox() {
        // An attacker creates `<sandbox>/escape → /tmp/evil-target`
        // (outside the sandbox) and tries to write through it. The
        // existing-target canonical check must refuse.
        use std::os::unix::fs::symlink;

        let sandbox = SandboxDir::new();
        // Create a target outside the sandbox.
        let outside_dir = sandbox._parent.join("outside");
        std::fs::create_dir_all(&outside_dir).unwrap();
        let outside_file = outside_dir.join("victim.txt");
        std::fs::write(&outside_file, b"original").unwrap();

        // Symlink inside sandbox pointing outside.
        let link = sandbox.root.join("escape");
        symlink(&outside_file, &link).expect("can create escape symlink");

        let tool = build_write_tool(&sandbox);
        let outcome = run_execute(&tool, json!({"path": "escape", "content": "PWNED"}));
        match outcome {
            ToolOutcome::Failed(AivyxError::Tool { detail, .. }) => {
                assert!(
                    detail.contains("escapes sandbox"),
                    "expected sandbox-escape refusal, got {detail}"
                );
            }
            other => panic!("symlink escape must be refused, got {other:?}"),
        }

        // And the outside victim file must be untouched.
        assert_eq!(
            std::fs::read_to_string(&outside_file).unwrap(),
            "original",
            "outside file must not have been overwritten"
        );
    }

    #[test]
    #[cfg(unix)]
    fn write_allows_overwriting_intra_sandbox_symlink_by_replacing_it() {
        // Overwriting an in-sandbox symlink: the tool should replace
        // the *link*, not follow it. A symlink `a → b` where both
        // are in-sandbox: writing to "a" should leave "a" as a
        // regular file with the new content, and "b" should keep
        // its old content.
        use std::os::unix::fs::symlink;

        let sandbox = SandboxDir::new();
        sandbox.write_file("target.txt", b"original target");
        let link = sandbox.root.join("link.txt");
        symlink("target.txt", &link).expect("create link");

        let tool = build_write_tool(&sandbox);
        let outcome = run_execute(&tool, json!({"path": "link.txt", "content": "replacement"}));
        assert!(matches!(outcome, ToolOutcome::Completed { .. }));

        // "link.txt" is now a regular file with new content.
        let link_content = std::fs::read_to_string(sandbox.root.join("link.txt")).unwrap();
        assert_eq!(link_content, "replacement");
        assert!(!sandbox.root.join("link.txt").is_symlink());

        // "target.txt" retains its original content — the rename
        // replaced the symlink itself, not what it pointed at.
        let target_content = std::fs::read_to_string(sandbox.root.join("target.txt")).unwrap();
        assert_eq!(target_content, "original target");
    }

    #[test]
    fn write_traversal_via_parent_dots_is_denied_lexically() {
        // The lexical resolver rejects `../../etc/passwd` as a target
        // path, producing the deny scope. But since we're going
        // through `execute` directly (not the loop), the deny scope
        // is not gated anywhere — instead execute itself should
        // detect the internal invariant violation. Real integration
        // (task 5) will test the full scope-gate path.
        let sandbox = SandboxDir::new();
        let tool = build_write_tool(&sandbox);
        let outcome = run_execute(
            &tool,
            json!({"path": "../../etc/test-should-never-exist", "content": "x"}),
        );
        match outcome {
            ToolOutcome::Failed(AivyxError::Internal(msg)) => {
                assert!(
                    msg.contains("escaped sandbox"),
                    "expected lexical-escape invariant error, got {msg}"
                );
            }
            other => panic!(
                "expected Internal invariant violation (task 5 tests the \
                 scope-gate path), got {other:?}"
            ),
        }
    }

    #[test]
    fn write_sandbox_capability_grants_in_sandbox_write_scope() {
        // Parallel to the read tool's capability test: an agent with
        // a broad sandbox write scope must cover a derived narrow
        // write scope under the sandbox.
        let sandbox = SandboxDir::new();
        let tool = build_write_tool(&sandbox);

        let held_pattern = format!("fs.write:{}/**", tool.sandbox_root().display());
        let held = CapabilitySet::from_scopes([Scope::parse(&held_pattern).unwrap()]);
        let effective = held.intersect(TrustTier::Trusted.default_ceiling());

        let needed = tool.required_scope(&json!({"path": "notes/today.md"}));
        assert!(
            effective.grants(&needed),
            "sandbox write capability must grant in-sandbox derived write, got needed={needed:?}"
        );
    }

    #[test]
    fn write_sandbox_capability_does_not_grant_fs_read() {
        // The cross-base check: holding `fs.write:<sandbox>/**` must
        // NOT grant any `fs.read:...` scope — D4 rule 1 (different
        // bases never match). Ensures the tools can't be confused
        // for each other in the capability layer.
        let sandbox = SandboxDir::new();
        let write_tool = build_write_tool(&sandbox);
        let read_tool = build_tool(&sandbox);

        let held_pattern = format!("fs.write:{}/**", write_tool.sandbox_root().display());
        let held = CapabilitySet::from_scopes([Scope::parse(&held_pattern).unwrap()]);
        let effective = held.intersect(TrustTier::Trusted.default_ceiling());

        let read_needed = read_tool.required_scope(&json!({"path": "notes/today.md"}));
        assert!(
            !effective.grants(&read_needed),
            "fs.write capability must not grant fs.read scope"
        );
    }

    // ---- FsDeleteTool ================================================

    fn build_delete_tool(sandbox: &SandboxDir) -> FsDeleteTool {
        FsDeleteToolConfig::new(sandbox.root.clone())
            .build()
            .expect("sandbox root must be canonicalizable for delete tests")
    }

    #[test]
    fn delete_build_fails_if_root_does_not_exist() {
        let err = FsDeleteToolConfig::new("/definitely/not/a/real/aivyx-delete-root")
            .build()
            .expect_err("nonexistent delete root must fail to build");
        assert!(matches!(err, AivyxError::Config(_)));
    }

    #[test]
    fn delete_build_fails_if_root_is_a_file() {
        let sandbox = SandboxDir::new();
        let file = sandbox.write_file("not-a-dir", b"x");
        let err = FsDeleteToolConfig::new(file)
            .build()
            .expect_err("file-as-root must fail");
        assert!(matches!(err, AivyxError::Config(_)));
    }

    #[test]
    fn delete_tool_descriptor_fields_are_what_the_planner_expects() {
        let sandbox = SandboxDir::new();
        let tool = build_delete_tool(&sandbox);
        assert_eq!(tool.name(), "fs.delete");
        let schema = tool.input_schema();
        assert_eq!(schema["type"], json!("object"));
        assert_eq!(schema["required"], json!(["path"]));
        assert!(schema["properties"]["path"].is_object());
    }

    #[test]
    fn portcullis_refuses_deleting_persistence_and_secret_paths() {
        // Regression for a real gap (found 2026-07-07 via Chapter
        // Almanac's guard-coverage audit): FsReadTool/FsWriteTool both
        // checked SensitivePolicy, but FsDeleteTool never did — a
        // protected path was one delete call away from permanent
        // destruction even though reading/overwriting it was refused.
        use crate::sensitive_paths::SensitivePolicy;
        let sandbox = SandboxDir::new();
        sandbox.write_file(".bashrc", b"evil() { :; }");
        sandbox.write_file(".netrc", b"machine x");
        sandbox.write_file("notes.md", b"hello");
        let tool = FsDeleteToolConfig::new(sandbox.root.clone())
            .with_sensitive_policy(Arc::new(SensitivePolicy::new(vec![], vec![])))
            .build()
            .expect("build guarded delete tool");

        // A persistence-path delete (shell rc) is refused…
        match run_execute(&tool, json!({ "path": ".bashrc" })) {
            ToolOutcome::Failed(AivyxError::Tool { detail, .. }) => {
                assert!(detail.contains("refusing to delete"), "{detail}");
            }
            other => panic!("expected refusal for .bashrc, got {other:?}"),
        }
        // …and a secret-path delete too.
        assert!(matches!(
            run_execute(&tool, json!({ "path": ".netrc" })),
            ToolOutcome::Failed(AivyxError::Tool { .. })
        ));
        // An ordinary delete still succeeds.
        assert!(matches!(
            run_execute(&tool, json!({ "path": "notes.md" })),
            ToolOutcome::Completed { .. }
        ));
    }

    #[test]
    fn delete_refuses_dotgit_config_even_with_sensitive_guard_disabled() {
        let sandbox = SandboxDir::new();
        sandbox.write_file(".git/config", b"[core]\n\trepositoryformatversion = 0\n");
        let tool = build_delete_tool(&sandbox);

        let outcome = run_execute(&tool, json!({"path": ".git/config"}));
        match outcome {
            ToolOutcome::Failed(AivyxError::Tool { detail, .. }) => {
                assert!(detail.contains("Writing inside .git is blocked"), "{detail}");
            }
            other => panic!("expected refusal for .git/config, got {other:?}"),
        }
        assert!(
            sandbox.root.join(".git/config").exists(),
            ".git/config must not have been deleted"
        );
    }

    #[test]
    fn delete_still_allows_dotgitignore() {
        let sandbox = SandboxDir::new();
        sandbox.write_file(".gitignore", b"/target\n");
        let tool = build_delete_tool(&sandbox);

        assert!(matches!(
            run_execute(&tool, json!({"path": ".gitignore"})),
            ToolOutcome::Completed { .. }
        ));
    }

    #[test]
    #[cfg(unix)]
    fn delete_refuses_dotgit_via_a_symlink_alias() {
        // Same symlink-alias bypass as `write_refuses_dotgit_via_a_symlink_alias`:
        // `path: "link/config"` has no `.git` component lexically, but
        // canonicalizes through `<sandbox>/link -> .git` to
        // `<sandbox>/.git/config`.
        use std::os::unix::fs::symlink;

        let sandbox = SandboxDir::new();
        std::fs::create_dir_all(sandbox.root.join(".git")).unwrap();
        std::fs::write(sandbox.root.join(".git/config"), b"[core]\n").unwrap();
        let link = sandbox.root.join("link");
        symlink(sandbox.root.join(".git"), &link).expect("can create alias symlink");

        let tool = build_delete_tool(&sandbox);
        let outcome = run_execute(&tool, json!({"path": "link/config"}));
        match outcome {
            ToolOutcome::Failed(AivyxError::Tool { detail, .. }) => {
                assert!(detail.contains("Writing inside .git is blocked"), "{detail}");
            }
            other => panic!("expected refusal for link/config, got {other:?}"),
        }
        assert!(
            sandbox.root.join(".git/config").exists(),
            ".git/config must not have been deleted via the symlink alias"
        );
    }

    // ---- FsDeleteTool: required_scope (lexical layer) -------------

    #[test]
    fn delete_scope_for_relative_path_inside_sandbox() {
        let sandbox = SandboxDir::new();
        let tool = build_delete_tool(&sandbox);
        let scope = tool.required_scope(&json!({"path": "notes/today.md"}));
        assert_eq!(scope.base(), "fs.delete");
        let q = scope.qualifier().expect("qualifier");
        assert!(q.ends_with("/notes/today.md"));
        assert!(!q.contains("__deny__"));
    }

    #[test]
    fn delete_scope_for_traversal_is_deny_scope() {
        let sandbox = SandboxDir::new();
        let tool = build_delete_tool(&sandbox);
        let scope = tool.required_scope(&json!({"path": "../../etc/passwd"}));
        assert_eq!(scope.base(), "fs.delete");
        assert!(scope.qualifier().unwrap().contains("__deny__"));
    }

    #[test]
    fn delete_scope_for_missing_path_is_deny_scope() {
        let sandbox = SandboxDir::new();
        let tool = build_delete_tool(&sandbox);
        let scope = tool.required_scope(&json!({"not_path": "x"}));
        assert!(scope.qualifier().unwrap().contains("__deny__"));
    }

    #[test]
    fn delete_scope_for_absolute_inside_sandbox_is_accepted() {
        let sandbox = SandboxDir::new();
        let tool = build_delete_tool(&sandbox);
        let abs = tool.sandbox_root().join("notes/today.md");
        let scope = tool.required_scope(&json!({"path": abs.display().to_string()}));
        assert!(!scope.qualifier().unwrap().contains("__deny__"));
    }

    // ---- FsDeleteTool: execute (canonical fence + non-recursive) --

    #[test]
    fn delete_happy_path_removes_a_file() {
        let sandbox = SandboxDir::new();
        sandbox.write_file("doomed.txt", b"bye");
        let tool = build_delete_tool(&sandbox);

        let outcome = run_execute(&tool, json!({"path": "doomed.txt"}));
        match outcome {
            ToolOutcome::Completed { output, verified } => {
                assert!(matches!(verified, Verification::Verified));
                assert_eq!(output["deleted"], json!(true));
                assert_eq!(output["kind"], json!("file"));
                assert!(!sandbox.root.join("doomed.txt").exists());
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    // -- Chapter N: confirm-first on destructive ops ---------------------

    #[test]
    fn delete_confirm_first_refuses_without_confirmed() {
        let sandbox = SandboxDir::new();
        sandbox.write_file("doomed.txt", b"bye");
        let tool = FsDeleteToolConfig::new(sandbox.root.clone())
            .with_confirm_destructive(true)
            .build()
            .unwrap();
        let outcome = run_execute(&tool, json!({"path": "doomed.txt"}));
        assert!(
            matches!(outcome, ToolOutcome::RequiresEscalation { .. }),
            "confirm-first must refuse an unconfirmed delete"
        );
        assert!(
            sandbox.root.join("doomed.txt").exists(),
            "file must survive"
        );
    }

    /// Confirm-first tools only say "needs approval" (the agent owns the
    /// rule that the operator, not the model, confirms — see
    /// `aivyx_core::confirm`): unconfirmed → `RequiresEscalation`,
    /// `confirmed: true` → runs.
    #[test]
    fn delete_confirm_first_asks_then_runs_when_confirmed() {
        let sandbox = SandboxDir::new();
        sandbox.write_file("doomed.txt", b"bye");
        let tool = FsDeleteToolConfig::new(sandbox.root.clone())
            .with_confirm_destructive(true)
            .build()
            .unwrap();
        let asked = run_execute(&tool, json!({"path": "doomed.txt"}));
        assert!(matches!(asked, ToolOutcome::RequiresEscalation { .. }), "{asked:?}");
        assert!(sandbox.root.join("doomed.txt").exists());
        let ran = run_execute(&tool, json!({"path": "doomed.txt", "confirmed": true}));
        assert!(matches!(ran, ToolOutcome::Completed { .. }), "{ran:?}");
        assert!(!sandbox.root.join("doomed.txt").exists());
    }

    #[test]
    fn overwrite_confirm_first_asks_then_runs_when_confirmed() {
        let sandbox = SandboxDir::new();
        sandbox.write_file("notes.txt", b"original");
        let tool = FsWriteToolConfig::new(sandbox.root.clone())
            .with_confirm_destructive(true)
            .build()
            .unwrap();
        let asked = run_execute(&tool, json!({"path": "notes.txt", "content": "new"}));
        assert!(matches!(asked, ToolOutcome::RequiresEscalation { .. }), "{asked:?}");
        let ran = run_execute(
            &tool,
            json!({"path": "notes.txt", "content": "new", "confirmed": true}),
        );
        assert!(matches!(ran, ToolOutcome::Completed { .. }), "{ran:?}");
    }

    #[test]
    fn write_confirm_first_refuses_overwrite_without_confirmed() {
        let sandbox = SandboxDir::new();
        sandbox.write_file("notes.txt", b"original");
        let tool = FsWriteToolConfig::new(sandbox.root.clone())
            .with_confirm_destructive(true)
            .build()
            .unwrap();
        let outcome = run_execute(&tool, json!({"path": "notes.txt", "content": "clobbered"}));
        assert!(
            matches!(outcome, ToolOutcome::RequiresEscalation { .. }),
            "overwriting an existing file must gate"
        );
        assert_eq!(
            fs::read_to_string(sandbox.root.join("notes.txt")).unwrap(),
            "original",
            "original content must survive the refusal"
        );
    }

    #[test]
    fn write_confirm_first_allows_new_file_without_confirmed() {
        // A fresh write to a NEW path is not destructive — it never gates.
        let sandbox = SandboxDir::new();
        let tool = FsWriteToolConfig::new(sandbox.root.clone())
            .with_confirm_destructive(true)
            .build()
            .unwrap();
        let outcome = run_execute(&tool, json!({"path": "fresh.txt", "content": "hello"}));
        assert!(matches!(outcome, ToolOutcome::Completed { .. }));
        assert_eq!(
            fs::read_to_string(sandbox.root.join("fresh.txt")).unwrap(),
            "hello"
        );
    }

    #[test]
    fn delete_removes_an_empty_directory() {
        let sandbox = SandboxDir::new();
        fs::create_dir(sandbox.root.join("empty-dir")).expect("mkdir");
        let tool = build_delete_tool(&sandbox);

        let outcome = run_execute(&tool, json!({"path": "empty-dir"}));
        match outcome {
            ToolOutcome::Completed { output, .. } => {
                assert_eq!(output["kind"], json!("directory"));
                assert!(!sandbox.root.join("empty-dir").exists());
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn delete_refuses_a_non_empty_directory() {
        // The non-recursive guarantee: a directory with contents is
        // refused, not wiped. `remove_dir`'s ENOTEMPTY surfaces as a
        // tool Failed and the directory + its file both survive.
        let sandbox = SandboxDir::new();
        sandbox.write_file("full-dir/keep.txt", b"still here");
        let tool = build_delete_tool(&sandbox);

        let outcome = run_execute(&tool, json!({"path": "full-dir"}));
        assert!(
            matches!(outcome, ToolOutcome::Failed(AivyxError::Tool { .. })),
            "non-empty directory delete must fail, got {outcome:?}"
        );
        assert!(
            sandbox.root.join("full-dir/keep.txt").exists(),
            "the directory and its contents must survive a refused delete"
        );
    }

    #[test]
    fn delete_nonexistent_path_fails() {
        let sandbox = SandboxDir::new();
        let tool = build_delete_tool(&sandbox);
        let outcome = run_execute(&tool, json!({"path": "ghost.txt"}));
        assert!(matches!(
            outcome,
            ToolOutcome::Failed(AivyxError::Tool { .. })
        ));
    }

    #[test]
    fn delete_missing_path_field_fails() {
        let sandbox = SandboxDir::new();
        let tool = build_delete_tool(&sandbox);
        let outcome = run_execute(&tool, json!({"nope": 1}));
        assert!(matches!(
            outcome,
            ToolOutcome::Failed(AivyxError::Tool { .. })
        ));
    }

    #[test]
    fn delete_refuses_the_sandbox_root_itself() {
        let sandbox = SandboxDir::new();
        let tool = build_delete_tool(&sandbox);
        let outcome = run_execute(&tool, json!({"path": "."}));
        match outcome {
            ToolOutcome::Failed(AivyxError::Tool { detail, .. }) => {
                assert!(detail.contains("sandbox root"), "got {detail:?}");
            }
            other => panic!("expected Tool failure, got {other:?}"),
        }
        assert!(sandbox.root.exists(), "sandbox root must survive");
    }

    #[cfg(unix)]
    #[test]
    fn delete_unlinks_a_symlink_not_its_target() {
        // Deleting a symlink removes the link, leaving the pointed-at
        // file intact — `remove_file` never follows the final link.
        use std::os::unix::fs::symlink as unix_symlink;
        let sandbox = SandboxDir::new();
        let real = sandbox.write_file("real.txt", b"keep me");
        unix_symlink(&real, sandbox.root.join("link")).expect("symlink");
        let tool = build_delete_tool(&sandbox);

        let outcome = run_execute(&tool, json!({"path": "link"}));
        match outcome {
            ToolOutcome::Completed { output, .. } => {
                assert_eq!(output["kind"], json!("symlink"));
            }
            other => panic!("expected Completed, got {other:?}"),
        }
        assert!(
            !sandbox.root.join("link").is_symlink(),
            "the symlink must be gone"
        );
        assert!(real.exists(), "the symlink's target must survive");
    }

    #[cfg(unix)]
    #[test]
    fn delete_refuses_a_path_whose_parent_symlinks_out_of_sandbox() {
        // A symlinked parent directory escaping the sandbox is caught
        // by the canonical-parent fence — the canonicalized parent no
        // longer starts with the sandbox root.
        use std::os::unix::fs::symlink as unix_symlink;
        let sandbox = SandboxDir::new();
        let outside = sandbox._parent.join("outside");
        fs::create_dir_all(&outside).expect("mkdir outside");
        let victim = outside.join("victim.txt");
        fs::write(&victim, b"do not delete me").expect("write victim");
        unix_symlink(&outside, sandbox.root.join("escape")).expect("symlink");
        let tool = build_delete_tool(&sandbox);

        let outcome = run_execute(&tool, json!({"path": "escape/victim.txt"}));
        assert!(
            matches!(outcome, ToolOutcome::Failed(AivyxError::Tool { .. })),
            "delete through a sandbox-escaping symlink parent must fail, \
             got {outcome:?}"
        );
        assert!(victim.exists(), "the out-of-sandbox victim must survive");
    }

    // ---- FsMetadataTool ==============================================

    fn build_metadata_tool(sandbox: &SandboxDir) -> FsMetadataTool {
        FsMetadataToolConfig::new(sandbox.root.clone())
            .build()
            .expect("sandbox root must be canonicalizable for metadata tests")
    }

    #[test]
    fn metadata_build_fails_if_root_does_not_exist() {
        let err = FsMetadataToolConfig::new("/definitely/not/a/real/aivyx-meta-root")
            .build()
            .expect_err("nonexistent metadata root must fail to build");
        assert!(matches!(err, AivyxError::Config(_)));
    }

    #[test]
    fn metadata_build_fails_if_root_is_a_file() {
        let sandbox = SandboxDir::new();
        let file = sandbox.write_file("not-a-dir", b"x");
        let err = FsMetadataToolConfig::new(file)
            .build()
            .expect_err("file-as-root must fail");
        assert!(matches!(err, AivyxError::Config(_)));
    }

    #[test]
    fn metadata_tool_descriptor_fields_are_what_the_planner_expects() {
        let sandbox = SandboxDir::new();
        let tool = build_metadata_tool(&sandbox);
        assert_eq!(tool.name(), "fs.metadata");
        let schema = tool.input_schema();
        assert_eq!(schema["type"], json!("object"));
        assert_eq!(schema["required"], json!(["path"]));
        assert!(schema["properties"]["path"].is_object());
    }

    #[test]
    fn ward_refuses_inspecting_a_sensitive_file_inside_the_sandbox() {
        // Regression for a real gap (found 2026-07-07 via Chapter
        // Almanac's guard-coverage audit): FsReadTool checked
        // SensitivePolicy, but FsMetadataTool never did — size/mtime/
        // permissions of a secret, or a protected directory's entry
        // listing, leaked without ever needing the read-content scope.
        use crate::sensitive_paths::SensitivePolicy;
        let sandbox = SandboxDir::new();
        sandbox.write_file(".env", b"API_KEY=super-secret\n");
        sandbox.write_file("notes.md", b"hello\n");
        let tool = FsMetadataToolConfig::new(sandbox.root.clone())
            .with_sensitive_policy(Arc::new(SensitivePolicy::new(vec![], vec![])))
            .build()
            .expect("build guarded metadata tool");

        match run_execute(&tool, json!({ "path": ".env" })) {
            ToolOutcome::Failed(AivyxError::Tool { detail, .. }) => {
                assert!(detail.contains("protected"), "reason: {detail}");
            }
            other => panic!("expected Failed for .env, got {other:?}"),
        }
        assert!(matches!(
            run_execute(&tool, json!({ "path": "notes.md" })),
            ToolOutcome::Completed { .. }
        ));
    }

    // ---- FsMetadataTool: required_scope (lexical layer) -----------

    #[test]
    fn metadata_scope_for_relative_path_inside_sandbox() {
        let sandbox = SandboxDir::new();
        let tool = build_metadata_tool(&sandbox);
        let scope = tool.required_scope(&json!({"path": "notes/today.md"}));
        assert_eq!(scope.base(), "fs.metadata");
        assert!(!scope.qualifier().unwrap().contains("__deny__"));
    }

    #[test]
    fn metadata_scope_for_traversal_is_deny_scope() {
        let sandbox = SandboxDir::new();
        let tool = build_metadata_tool(&sandbox);
        let scope = tool.required_scope(&json!({"path": "../../etc/passwd"}));
        assert_eq!(scope.base(), "fs.metadata");
        assert!(scope.qualifier().unwrap().contains("__deny__"));
    }

    #[test]
    fn metadata_scope_for_missing_path_is_deny_scope() {
        let sandbox = SandboxDir::new();
        let tool = build_metadata_tool(&sandbox);
        let scope = tool.required_scope(&json!({"x": 1}));
        assert!(scope.qualifier().unwrap().contains("__deny__"));
    }

    // ---- FsMetadataTool: execute (stat + directory listing) -------

    #[test]
    fn metadata_on_a_file_reports_size_and_kind() {
        let sandbox = SandboxDir::new();
        sandbox.write_file("hi.txt", b"hello!!"); // 7 bytes
        let tool = build_metadata_tool(&sandbox);
        let outcome = run_execute(&tool, json!({"path": "hi.txt"}));
        match outcome {
            ToolOutcome::Completed { output, verified } => {
                assert!(matches!(verified, Verification::NotApplicable));
                assert_eq!(output["kind"], json!("file"));
                assert_eq!(output["size_bytes"], json!(7));
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn metadata_on_a_directory_lists_its_entries() {
        let sandbox = SandboxDir::new();
        sandbox.write_file("proj/a.txt", b"a");
        sandbox.write_file("proj/b.txt", b"b");
        let tool = build_metadata_tool(&sandbox);
        let outcome = run_execute(&tool, json!({"path": "proj"}));
        match outcome {
            ToolOutcome::Completed { output, .. } => {
                assert_eq!(output["kind"], json!("directory"));
                let names: Vec<&str> = output["entries"]
                    .as_array()
                    .expect("entries array")
                    .iter()
                    .map(|e| e["name"].as_str().unwrap())
                    .collect();
                assert_eq!(names, vec!["a.txt", "b.txt"]);
                assert_eq!(output["entries_truncated"], json!(false));
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn metadata_directory_entries_are_sorted_by_name() {
        let sandbox = SandboxDir::new();
        sandbox.write_file("d/charlie", b"c");
        sandbox.write_file("d/alpha", b"a");
        sandbox.write_file("d/bravo", b"b");
        let tool = build_metadata_tool(&sandbox);
        let outcome = run_execute(&tool, json!({"path": "d"}));
        match outcome {
            ToolOutcome::Completed { output, .. } => {
                let names: Vec<&str> = output["entries"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|e| e["name"].as_str().unwrap())
                    .collect();
                assert_eq!(names, vec!["alpha", "bravo", "charlie"]);
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn metadata_directory_entry_kinds_distinguish_file_and_dir() {
        let sandbox = SandboxDir::new();
        sandbox.write_file("mix/file.txt", b"f");
        fs::create_dir(sandbox.root.join("mix/subdir")).expect("mkdir subdir");
        let tool = build_metadata_tool(&sandbox);
        let outcome = run_execute(&tool, json!({"path": "mix"}));
        match outcome {
            ToolOutcome::Completed { output, .. } => {
                let entries = output["entries"].as_array().unwrap();
                let kind_of = |n: &str| -> String {
                    entries
                        .iter()
                        .find(|e| e["name"] == json!(n))
                        .map(|e| e["kind"].as_str().unwrap().to_string())
                        .unwrap_or_default()
                };
                assert_eq!(kind_of("file.txt"), "file");
                assert_eq!(kind_of("subdir"), "directory");
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[test]
    fn metadata_nonexistent_path_fails() {
        let sandbox = SandboxDir::new();
        let tool = build_metadata_tool(&sandbox);
        let outcome = run_execute(&tool, json!({"path": "ghost"}));
        assert!(matches!(
            outcome,
            ToolOutcome::Failed(AivyxError::Tool { .. })
        ));
    }

    #[test]
    fn metadata_missing_path_field_fails() {
        let sandbox = SandboxDir::new();
        let tool = build_metadata_tool(&sandbox);
        let outcome = run_execute(&tool, json!({"nope": 1}));
        assert!(matches!(
            outcome,
            ToolOutcome::Failed(AivyxError::Tool { .. })
        ));
    }

    #[test]
    fn metadata_is_read_only_and_reports_a_modified_time() {
        let sandbox = SandboxDir::new();
        sandbox.write_file("stamp.txt", b"x");
        let tool = build_metadata_tool(&sandbox);
        let outcome = run_execute(&tool, json!({"path": "stamp.txt"}));
        match outcome {
            ToolOutcome::Completed { output, verified } => {
                assert!(matches!(verified, Verification::NotApplicable));
                assert!(
                    output["modified_unix_secs"].is_u64(),
                    "a just-written file must report a modified time"
                );
                assert!(output["readonly"].is_boolean());
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn metadata_reports_unix_mode() {
        let sandbox = SandboxDir::new();
        sandbox.write_file("perm.txt", b"x");
        let tool = build_metadata_tool(&sandbox);
        let outcome = run_execute(&tool, json!({"path": "perm.txt"}));
        match outcome {
            ToolOutcome::Completed { output, .. } => {
                assert!(
                    output["unix_mode"].is_u64(),
                    "unix_mode must be a number on unix"
                );
            }
            other => panic!("expected Completed, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn metadata_through_an_escaping_symlink_is_refused() {
        // `fs.metadata` canonicalizes the path itself (it follows the
        // final symlink, like `fs.read`). A symlink resolving outside
        // the sandbox is caught by the canonical fence.
        use std::os::unix::fs::symlink as unix_symlink;
        let sandbox = SandboxDir::new();
        let outside = sandbox._parent.join("meta-outside");
        fs::create_dir_all(&outside).expect("mkdir outside");
        fs::write(outside.join("secret.txt"), b"top secret").expect("write");
        unix_symlink(&outside, sandbox.root.join("peek")).expect("symlink");
        let tool = build_metadata_tool(&sandbox);

        let outcome = run_execute(&tool, json!({"path": "peek/secret.txt"}));
        assert!(
            matches!(outcome, ToolOutcome::Failed(AivyxError::Tool { .. })),
            "stat through a sandbox-escaping symlink must fail, got {outcome:?}"
        );
    }
}
