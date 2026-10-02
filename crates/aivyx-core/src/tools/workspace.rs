//! Chapter O — the agent's personal workspace.
//!
//! A dedicated, always-available directory the agent OWNS (default
//! `~/.aivyx-pa/workspace/`), for its own thoughts, ideas, plans, and multi-file
//! projects — the third leg alongside `memory.*` (recall facts) and the
//! operator's `fs.*` / `fs_root` (shared work, Chapter N access levels). It is
//! independent of the access level: even a fully-sandboxed agent has its own
//! notebook here.
//!
//! This module provides [`provision_workspace`] (idempotent startup seeding).
//! The `workspace.*` tools that operate within it land in O.2.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use aivyx_capability::Scope;

use crate::tools::fs::{GIT_METADATA_WRITE_BLOCKED, lexical_resolve, touches_git_metadata};
use crate::{AivyxError, GitCheckpointer, Tool, ToolContext, ToolId, ToolOutcome, Verification};

/// Seed README written into a fresh workspace (only when absent — the agent's
/// own edits are never clobbered). Addressed to the agent itself.
const WORKSPACE_README: &str = "\
# Your workspace

This directory is **yours** — your own space, separate from the operator's
files. Use it freely for your own purposes: keep a journal, sketch ideas,
draft and revise plans, and scaffold your own projects. Nothing here is the
operator's; organize it however helps you think.

Suggested layout (just a starting point — make your own):

- `journal/` — dated entries; what you did, noticed, or are mulling over.
- `ideas/`   — half-formed thoughts and sketches worth keeping.
- `plans/`   — plans you draft and revise over time.
- `projects/`— your own multi-file projects.

Use the `workspace.*` tools to read, write, list, and append here. The
operator can see this space, so it is a window into your thinking — but it is
yours to use.
";

/// The seed subdirectories created in a fresh workspace.
pub const WORKSPACE_SUBDIRS: &[&str] = &["journal", "ideas", "plans", "projects"];

/// Create the workspace directory + seed structure, idempotently. Safe to call
/// on every startup: `create_dir_all` no-ops on existing dirs, and the README
/// is written only when absent so the agent's own edits survive.
pub fn provision_workspace(root: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(root)?;
    for sub in WORKSPACE_SUBDIRS {
        std::fs::create_dir_all(root.join(sub))?;
    }
    let readme = root.join("README.md");
    if !readme.exists() {
        std::fs::write(&readme, WORKSPACE_README)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// workspace.* tools (Chapter O.2)
//
// All workspace tools share one capability base — `workspace` — granted at
// the workspace root. The tools are namespaced `workspace.read/write/...` for
// the model's clarity, but the agent's own contained notebook doesn't need
// per-op capability granularity (you grant the agent its workspace, or you
// don't). Containment is the same lexical fence `fs.*` uses (`lexical_resolve`).
// ---------------------------------------------------------------------------

/// Max bytes returned by `workspace.read` / written by `workspace.write`.
const MAX_WORKSPACE_BYTES: usize = 256 * 1024;

/// The capability base every workspace tool requires.
const WORKSPACE_SCOPE_BASE: &str = "workspace";

fn deny_scope() -> Scope {
    Scope::parse("workspace:/aivyx/__deny__/invalid-input").expect("static deny scope parses")
}

/// Resolve `path` (relative to the workspace root) inside the root, or `None`
/// if it escapes — the same lexical containment `fs.*` uses.
fn resolve_in_workspace(root: &Path, path: &str) -> Option<PathBuf> {
    lexical_resolve(root, Path::new(path))
}

/// Chapter O.2 hardening (found missing 2026-07-07 via Chapter Almanac's
/// guard-coverage audit) — the canonical layer `fs.*` pairs with its lexical
/// resolve (see `fs.rs`'s module doc: "two independent layers"), which
/// `workspace.*` never had despite its own comment claiming parity. Without
/// this, a symlink planted inside the workspace *after* construction (e.g.
/// via a chained `shell.exec` call: `ln -s /etc/shadow escape`) would be
/// followed straight through by `workspace.read`/`.list` — the lexical
/// layer alone cannot see it.
///
/// For a path that must already exist (read, list): canonicalize and verify
/// the result is still under the canonical workspace root.
fn canonical_fence(root: &Path, lexical_abs: &Path) -> Result<PathBuf, String> {
    let canonical = std::fs::canonicalize(lexical_abs)
        .map_err(|e| format!("cannot canonicalize {lexical_abs:?}: {e}"))?;
    if !canonical.starts_with(root) {
        return Err(format!(
            "path {canonical:?} escapes workspace root {root:?} after symlink resolution"
        ));
    }
    Ok(canonical)
}

/// Same fence for a path that may not exist yet (write, delete, note):
/// canonicalize the *parent* (which must exist), verify containment, then
/// rejoin the final component. The final component is deliberately not
/// canonicalize-followed, so a symlink planted as the final component is
/// unlinked/overwritten as itself rather than followed through — mirrors
/// `FsWriteTool`/`FsDeleteTool`'s identical parent-then-rejoin shape.
fn canonical_fence_parent(root: &Path, lexical_abs: &Path) -> Result<PathBuf, String> {
    let parent = lexical_abs
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| format!("path {lexical_abs:?} has no parent directory"))?;
    let file_name = lexical_abs
        .file_name()
        .ok_or_else(|| format!("path {lexical_abs:?} has no final component"))?;
    let canonical_parent = std::fs::canonicalize(parent)
        .map_err(|e| format!("cannot canonicalize parent {parent:?}: {e}"))?;
    if !canonical_parent.starts_with(root) {
        return Err(format!(
            "parent {canonical_parent:?} escapes workspace root {root:?} after symlink resolution"
        ));
    }
    Ok(canonical_parent.join(file_name))
}

/// `workspace:<abs>` scope for a resolved path, or the deny scope.
fn scope_for(abs: &Path) -> Scope {
    Scope::parse(&format!("{WORKSPACE_SCOPE_BASE}:{}", abs.display())).unwrap_or_else(deny_scope)
}

fn tool_fail(id: ToolId, detail: impl Into<String>) -> ToolOutcome {
    ToolOutcome::Failed(AivyxError::Tool {
        tool: id,
        detail: detail.into(),
    })
}

/// Pull a string field from the input, or fail.
fn str_field(input: &Value, key: &str) -> Result<String, String> {
    input
        .get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| format!("input must have a string `{key}` field"))
}

/// Build all workspace tools rooted at `root` (canonicalized). Returns the
/// tools plus the canonical root so the binary can mint the operator-held
/// `workspace:<root>/**` + bare-root grant. Fails if the root isn't a dir.
///
/// `checkpointer` is `None` when `root` isn't (yet) a git repo -- opt-in
/// only, there is no auto `git init` here. Only the three mutating tools
/// (`WorkspaceWriteTool`/`WorkspaceDeleteTool`/`WorkspaceNoteTool`) receive
/// it; `WorkspaceReadTool`/`WorkspaceListTool` are read-only and don't need it.
pub fn build_workspace_tools(
    root: &Path,
    checkpointer: Option<Arc<GitCheckpointer>>,
) -> Result<(Vec<Arc<dyn Tool>>, PathBuf), AivyxError> {
    let canonical = std::fs::canonicalize(root).map_err(|e| {
        AivyxError::Config(format!(
            "workspace root {root:?} cannot be canonicalized: {e}"
        ))
    })?;
    if !canonical.is_dir() {
        return Err(AivyxError::Config(format!(
            "workspace root {canonical:?} is not a directory"
        )));
    }
    let root: Arc<Path> = Arc::from(canonical.as_path());
    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(WorkspaceReadTool::new(root.clone())),
        Arc::new(WorkspaceWriteTool::new(root.clone(), checkpointer.clone())),
        Arc::new(WorkspaceListTool::new(root.clone())),
        Arc::new(WorkspaceDeleteTool::new(root.clone(), checkpointer.clone())),
        Arc::new(WorkspaceNoteTool::new(root.clone(), checkpointer)),
    ];
    Ok((tools, canonical))
}

macro_rules! ws_tool {
    ($name:ident, $tool_name:literal, $desc:literal, $schema:expr) => {
        #[derive(Debug)]
        pub struct $name {
            id: ToolId,
            root: Arc<Path>,
            schema: Value,
        }
        impl $name {
            pub fn new(root: Arc<Path>) -> Self {
                Self {
                    id: ToolId::new(),
                    root,
                    schema: $schema,
                }
            }
        }
    };
}

ws_tool!(WorkspaceReadTool, "workspace.read", "", read_schema());
ws_tool!(WorkspaceListTool, "workspace.list", "", list_schema());

// WorkspaceWriteTool/WorkspaceDeleteTool/WorkspaceNoteTool are hand-written
// below rather than generated by `ws_tool!`, since they carry an extra
// `checkpointer` field the two read-only tools above don't need. Debug is
// implemented by hand (not derived) because `GitCheckpointer` doesn't
// implement `Debug` -- the field is summarized as a bool instead of
// printed directly.

pub struct WorkspaceWriteTool {
    id: ToolId,
    root: Arc<Path>,
    schema: Value,
    checkpointer: Option<Arc<GitCheckpointer>>,
}
impl WorkspaceWriteTool {
    pub fn new(root: Arc<Path>, checkpointer: Option<Arc<GitCheckpointer>>) -> Self {
        Self {
            id: ToolId::new(),
            root,
            schema: write_schema(),
            checkpointer,
        }
    }
}
impl std::fmt::Debug for WorkspaceWriteTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceWriteTool")
            .field("id", &self.id)
            .field("root", &self.root)
            .field("schema", &self.schema)
            .field("checkpoint_enabled", &self.checkpointer.is_some())
            .finish()
    }
}

pub struct WorkspaceDeleteTool {
    id: ToolId,
    root: Arc<Path>,
    schema: Value,
    checkpointer: Option<Arc<GitCheckpointer>>,
}
impl WorkspaceDeleteTool {
    pub fn new(root: Arc<Path>, checkpointer: Option<Arc<GitCheckpointer>>) -> Self {
        Self {
            id: ToolId::new(),
            root,
            schema: delete_schema(),
            checkpointer,
        }
    }
}
impl std::fmt::Debug for WorkspaceDeleteTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceDeleteTool")
            .field("id", &self.id)
            .field("root", &self.root)
            .field("schema", &self.schema)
            .field("checkpoint_enabled", &self.checkpointer.is_some())
            .finish()
    }
}

pub struct WorkspaceNoteTool {
    id: ToolId,
    root: Arc<Path>,
    schema: Value,
    checkpointer: Option<Arc<GitCheckpointer>>,
}
impl WorkspaceNoteTool {
    pub fn new(root: Arc<Path>, checkpointer: Option<Arc<GitCheckpointer>>) -> Self {
        Self {
            id: ToolId::new(),
            root,
            schema: note_schema(),
            checkpointer,
        }
    }
}
impl std::fmt::Debug for WorkspaceNoteTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceNoteTool")
            .field("id", &self.id)
            .field("root", &self.root)
            .field("schema", &self.schema)
            .field("checkpoint_enabled", &self.checkpointer.is_some())
            .finish()
    }
}

fn path_schema(desc: &str) -> Value {
    json!({ "type": "string", "description": desc })
}
fn read_schema() -> Value {
    json!({"type":"object","properties":{"path":path_schema("Path within your workspace to read.")},"required":["path"]})
}
fn write_schema() -> Value {
    json!({"type":"object","properties":{
        "path":path_schema("Path within your workspace to write (parent dirs are created)."),
        "content":{"type":"string","description":"UTF-8 content to write."}},"required":["path","content"]})
}
fn list_schema() -> Value {
    json!({"type":"object","properties":{"path":path_schema("Optional sub-path to list; omit for the workspace root.")}})
}
fn delete_schema() -> Value {
    json!({"type":"object","properties":{"path":path_schema("Path within your workspace to delete (file or empty dir).")},"required":["path"]})
}
fn note_schema() -> Value {
    json!({"type":"object","properties":{
        "content":{"type":"string","description":"The note/thought to append."},
        "category":{"type":"string","description":"Optional bucket: 'journal' (default), 'ideas', or 'plans'."}},
        "required":["content"]})
}

#[async_trait]
impl Tool for WorkspaceReadTool {
    fn id(&self) -> ToolId {
        self.id
    }
    fn name(&self) -> &str {
        "workspace.read"
    }
    fn description(&self) -> &str {
        "Read a file from YOUR workspace (your own private space for notes, \
         ideas, plans, and projects). Input: `{ path }`."
    }
    fn input_schema(&self) -> &Value {
        &self.schema
    }
    fn required_scope(&self, input: &Value) -> Scope {
        match input.get("path").and_then(|v| v.as_str()) {
            Some(p) => match resolve_in_workspace(&self.root, p) {
                Some(abs) => scope_for(&abs),
                None => deny_scope(),
            },
            None => deny_scope(),
        }
    }
    async fn execute(&self, input: Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        let path = match str_field(&input, "path") {
            Ok(p) => p,
            Err(e) => return tool_fail(self.id, e),
        };
        let Some(lexical_abs) = resolve_in_workspace(&self.root, &path) else {
            return tool_fail(self.id, format!("path {path:?} escapes the workspace"));
        };
        let abs = match canonical_fence(&self.root, &lexical_abs) {
            Ok(p) => p,
            Err(e) => return tool_fail(self.id, e),
        };
        match std::fs::read(&abs) {
            Ok(bytes) => {
                let truncated = bytes.len() > MAX_WORKSPACE_BYTES;
                let slice = &bytes[..bytes.len().min(MAX_WORKSPACE_BYTES)];
                let content = String::from_utf8_lossy(slice).to_string();
                ToolOutcome::Completed {
                    output: json!({ "content": content, "truncated": truncated }),
                    verified: Verification::NotApplicable,
                }
            }
            Err(e) => tool_fail(self.id, format!("cannot read {path:?}: {e}")),
        }
    }
}

#[async_trait]
impl Tool for WorkspaceWriteTool {
    fn id(&self) -> ToolId {
        self.id
    }
    fn name(&self) -> &str {
        "workspace.write"
    }
    fn description(&self) -> &str {
        "Write (create or overwrite) a file in YOUR private notebook workspace — \
         for your own notes and drafts, not the operator's files (use `fs.write` \
         for a file they ask for). Parent dirs are created as needed. Input: \
         `{ path, content }`."
    }
    fn input_schema(&self) -> &Value {
        &self.schema
    }
    fn required_scope(&self, input: &Value) -> Scope {
        match input.get("path").and_then(|v| v.as_str()) {
            Some(p) => match resolve_in_workspace(&self.root, p) {
                Some(abs) => scope_for(&abs),
                None => deny_scope(),
            },
            None => deny_scope(),
        }
    }
    async fn execute(&self, input: Value, ctx: &ToolContext<'_>) -> ToolOutcome {
        let path = match str_field(&input, "path") {
            Ok(p) => p,
            Err(e) => return tool_fail(self.id, e),
        };
        let content = match str_field(&input, "content") {
            Ok(c) => c,
            Err(e) => return tool_fail(self.id, e),
        };
        if content.len() > MAX_WORKSPACE_BYTES {
            return tool_fail(
                self.id,
                format!("content exceeds {MAX_WORKSPACE_BYTES} bytes"),
            );
        }
        let Some(lexical_abs) = resolve_in_workspace(&self.root, &path) else {
            return tool_fail(self.id, format!("path {path:?} escapes the workspace"));
        };
        // git-metadata block (unconditional) — see `touches_git_metadata`'s
        // doc comment in `tools::fs`.
        if touches_git_metadata(&lexical_abs) {
            return tool_fail(self.id, GIT_METADATA_WRITE_BLOCKED);
        }
        if let Some(parent) = lexical_abs.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return tool_fail(self.id, format!("cannot create parent of {path:?}: {e}"));
            }
        }
        let abs = match canonical_fence_parent(&self.root, &lexical_abs) {
            Ok(p) => p,
            Err(e) => return tool_fail(self.id, e),
        };
        // git-metadata block, re-checked on the CANONICAL target — a
        // symlink alias (`<root>/link -> .git`) lets `path: "link/config"`
        // lexically resolve with no `.git` component at all, while still
        // landing inside `.git` once the parent is canonicalized through
        // the symlink a few lines up. See `tools::fs::touches_git_metadata`.
        if touches_git_metadata(&abs) {
            return tool_fail(self.id, GIT_METADATA_WRITE_BLOCKED);
        }
        // A pre-existing symlink at the write target is refused outright —
        // checked via `is_symlink` (an `lstat`, not `stat`), not `exists()`,
        // so a *dangling* symlink (pointing at a destination that doesn't
        // exist yet) is still caught: `exists()` follows the link and
        // would report `false` for a dangling one, silently skipping this
        // check while `std::fs::write` itself would still follow it and
        // create the destination file wherever the link points — even one
        // outside the workspace, since we cannot canonicalize a
        // not-yet-existing destination to check containment the way the
        // `abs.exists()` case can. Refusing outright (rather than trying
        // to unlink first, which would need that same containment check
        // to be safe) sidesteps the ambiguity entirely: a symlink has no
        // legitimate reason to already sit at a workspace write target.
        if abs.is_symlink() {
            return tool_fail(
                self.id,
                format!(
                    "refusing to write through an existing symlink at {abs:?} — \
                     delete it first with workspace.delete if you intend to replace it"
                ),
            );
        }
        if let Some(checkpointer) = &self.checkpointer {
            checkpointer.checkpoint(self.name(), ctx.cancellation).await;
        }
        match std::fs::write(&abs, content.as_bytes()) {
            Ok(()) => ToolOutcome::Completed {
                output: json!({ "written": true, "bytes": content.len() }),
                verified: Verification::NotApplicable,
            },
            Err(e) => tool_fail(self.id, format!("cannot write {path:?}: {e}")),
        }
    }
}

#[async_trait]
impl Tool for WorkspaceListTool {
    fn id(&self) -> ToolId {
        self.id
    }
    fn name(&self) -> &str {
        "workspace.list"
    }
    fn description(&self) -> &str {
        "List YOUR workspace (or a sub-path). Input: `{ path? }` — omit `path` \
         for the workspace root."
    }
    fn input_schema(&self) -> &Value {
        &self.schema
    }
    fn required_scope(&self, input: &Value) -> Scope {
        let p = input.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        match resolve_in_workspace(&self.root, p) {
            Some(abs) => scope_for(&abs),
            None => deny_scope(),
        }
    }
    async fn execute(&self, input: Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        let path = input.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let Some(lexical_abs) = resolve_in_workspace(&self.root, path) else {
            return tool_fail(self.id, format!("path {path:?} escapes the workspace"));
        };
        let abs = match canonical_fence(&self.root, &lexical_abs) {
            Ok(p) => p,
            Err(e) => return tool_fail(self.id, e),
        };
        match std::fs::read_dir(&abs) {
            Ok(rd) => {
                let mut entries: Vec<Value> = Vec::new();
                for e in rd.flatten() {
                    let kind = match e.file_type() {
                        Ok(ft) if ft.is_dir() => "directory",
                        Ok(ft) if ft.is_symlink() => "symlink",
                        _ => "file",
                    };
                    entries.push(json!({ "name": e.file_name().to_string_lossy(), "kind": kind }));
                }
                entries.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
                ToolOutcome::Completed {
                    output: json!({ "entries": entries }),
                    verified: Verification::NotApplicable,
                }
            }
            Err(e) => tool_fail(self.id, format!("cannot list {path:?}: {e}")),
        }
    }
}

#[async_trait]
impl Tool for WorkspaceDeleteTool {
    fn id(&self) -> ToolId {
        self.id
    }
    fn name(&self) -> &str {
        "workspace.delete"
    }
    fn description(&self) -> &str {
        "Delete a file or empty directory from YOUR private notebook workspace \
         (the operator's files are deleted with `fs.delete`). Input: `{ path }`."
    }
    fn input_schema(&self) -> &Value {
        &self.schema
    }
    fn required_scope(&self, input: &Value) -> Scope {
        match input.get("path").and_then(|v| v.as_str()) {
            Some(p) => match resolve_in_workspace(&self.root, p) {
                Some(abs) => scope_for(&abs),
                None => deny_scope(),
            },
            None => deny_scope(),
        }
    }
    async fn execute(&self, input: Value, ctx: &ToolContext<'_>) -> ToolOutcome {
        let path = match str_field(&input, "path") {
            Ok(p) => p,
            Err(e) => return tool_fail(self.id, e),
        };
        let Some(lexical_abs) = resolve_in_workspace(&self.root, &path) else {
            return tool_fail(self.id, format!("path {path:?} escapes the workspace"));
        };
        if lexical_abs.as_path() == &*self.root {
            return tool_fail(self.id, "cannot delete the workspace root itself");
        }
        // git-metadata block (unconditional) — see `touches_git_metadata`'s
        // doc comment in `tools::fs`.
        if touches_git_metadata(&lexical_abs) {
            return tool_fail(self.id, GIT_METADATA_WRITE_BLOCKED);
        }
        let abs = match canonical_fence_parent(&self.root, &lexical_abs) {
            Ok(p) => p,
            Err(e) => return tool_fail(self.id, e),
        };
        // git-metadata block, re-checked on the CANONICAL target — same
        // symlink-alias gap as `WorkspaceWriteTool::execute`.
        if touches_git_metadata(&abs) {
            return tool_fail(self.id, GIT_METADATA_WRITE_BLOCKED);
        }
        let md = match std::fs::symlink_metadata(&abs) {
            Ok(m) => m,
            Err(e) => return tool_fail(self.id, format!("cannot stat {path:?}: {e}")),
        };
        if let Some(checkpointer) = &self.checkpointer {
            checkpointer.checkpoint(self.name(), ctx.cancellation).await;
        }
        let res = if md.is_dir() {
            std::fs::remove_dir(&abs)
        } else {
            std::fs::remove_file(&abs)
        };
        match res {
            Ok(()) => ToolOutcome::Completed {
                output: json!({ "deleted": true }),
                verified: Verification::NotApplicable,
            },
            Err(e) => tool_fail(self.id, format!("cannot delete {path:?}: {e}")),
        }
    }
}

#[async_trait]
impl Tool for WorkspaceNoteTool {
    fn id(&self) -> ToolId {
        self.id
    }
    fn name(&self) -> &str {
        "workspace.note"
    }
    fn description(&self) -> &str {
        "Append a timestamped entry to your journal (or another bucket) — the \
         quick way to jot a thought, idea, or plan. Input: `{ content, \
         category? }` (category: 'journal' default, 'ideas', 'plans')."
    }
    fn input_schema(&self) -> &Value {
        &self.schema
    }
    fn required_scope(&self, _input: &Value) -> Scope {
        // Always writes under the workspace root; the bare-root grant covers it.
        scope_for(&self.root)
    }
    async fn execute(&self, input: Value, ctx: &ToolContext<'_>) -> ToolOutcome {
        let content = match str_field(&input, "content") {
            Ok(c) => c,
            Err(e) => return tool_fail(self.id, e),
        };
        let category = input
            .get("category")
            .and_then(|v| v.as_str())
            .unwrap_or("journal");
        // Sanitize category to a single path component.
        let category =
            category.trim_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '_');
        let category = if category.is_empty() {
            "journal"
        } else {
            category
        };
        let (date, secs) = current_date_and_unix();
        let rel = format!("{category}/{date}.md");
        let Some(lexical_abs) = resolve_in_workspace(&self.root, &rel) else {
            return tool_fail(self.id, "internal: journal path escaped workspace");
        };
        // git-metadata block (unconditional) — `category` survives
        // `trim_matches` with interior `/`/`.` intact (e.g. `x/.git/hooks`
        // trims only the leading/trailing char class), so this is reachable
        // through the model-influenced `category` field, not just `path`.
        // See `touches_git_metadata`'s doc comment in `tools::fs`.
        if touches_git_metadata(&lexical_abs) {
            return tool_fail(self.id, GIT_METADATA_WRITE_BLOCKED);
        }
        if let Some(parent) = lexical_abs.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let abs = match canonical_fence_parent(&self.root, &lexical_abs) {
            Ok(p) => p,
            Err(e) => return tool_fail(self.id, e),
        };
        // git-metadata block, re-checked on the CANONICAL target — covers
        // a `category` that is itself an existing symlink alias to `.git`
        // (e.g. `<root>/link -> .git`, `category: "link"`), which the
        // lexical check above cannot see.
        if touches_git_metadata(&abs) {
            return tool_fail(self.id, GIT_METADATA_WRITE_BLOCKED);
        }
        // A pre-existing symlink at the journal path is refused outright —
        // checked via `is_symlink` so a *dangling* symlink is caught too
        // (see `WorkspaceWriteTool`'s identical check for why `exists()`
        // alone would miss it). A journal file is always created as a
        // plain file by a prior `workspace.note` call; it never
        // legitimately becomes a symlink, so there is no "safe" case to
        // preserve — refusing is unambiguous and never destroys real
        // journal history (append mode's whole point is reusing the
        // *same regular file* across calls, not tolerating a link there).
        if abs.is_symlink() {
            return tool_fail(
                self.id,
                format!("refusing to append through an existing symlink at {abs:?}"),
            );
        }
        let entry = format!("\n## {date} (t={secs})\n\n{content}\n");
        use std::io::Write as _;
        if let Some(checkpointer) = &self.checkpointer {
            checkpointer.checkpoint(self.name(), ctx.cancellation).await;
        }
        let res = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&abs)
            .and_then(|mut f| f.write_all(entry.as_bytes()));
        match res {
            Ok(()) => ToolOutcome::Completed {
                output: json!({ "appended_to": rel }),
                verified: Verification::NotApplicable,
            },
            Err(e) => tool_fail(self.id, format!("cannot append note: {e}")),
        }
    }
}

/// `(YYYY-MM-DD, unix_seconds)` from the system clock — dep-free civil date
/// (Howard Hinnant's `civil_from_days`), good for any date after 1970.
fn current_date_and_unix() -> (String, u64) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    // civil_from_days: days since 1970-01-01 → (y, m, d).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let y = if m <= 2 { y + 1 } else { y };
    (format!("{y:04}-{m:02}-{d:02}"), secs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MessageOrigin;

    fn tmp(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("aivyx-ws-{}-{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn provision_creates_dir_seed_and_subdirs() {
        let root = tmp("provision");
        provision_workspace(&root).unwrap();
        assert!(root.join("README.md").is_file());
        for sub in WORKSPACE_SUBDIRS {
            assert!(root.join(sub).is_dir(), "{sub} should exist");
        }
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn provision_is_idempotent_and_preserves_edits() {
        let root = tmp("idempotent");
        provision_workspace(&root).unwrap();
        // Operator/agent edits the README + adds a file.
        std::fs::write(root.join("README.md"), "my own notes").unwrap();
        std::fs::write(root.join("journal").join("day1.md"), "entry").unwrap();
        // Re-provision must not clobber either.
        provision_workspace(&root).unwrap();
        assert_eq!(
            std::fs::read_to_string(root.join("README.md")).unwrap(),
            "my own notes"
        );
        assert!(root.join("journal").join("day1.md").is_file());
        std::fs::remove_dir_all(&root).ok();
    }

    // ---- workspace.* tools ------------------------------------------

    fn run_execute(tool: &dyn Tool, input: Value) -> ToolOutcome {
        use crate::{AgentId, CancellationToken, NullAuditHook, SessionId, TurnId};
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
                _e: crate::StreamEvent<'_>,
            ) -> Result<(), crate::ChannelError> {
                Ok(())
            }
            async fn finalize(&self, _o: &crate::TurnOutcome) -> Result<(), crate::ChannelError> {
                Ok(())
            }
            fn cancellation_token(&self) -> CancellationToken {
                self.token.clone()
            }
        }
        let channel = NoopChannel {
            session: SessionId::new(),
            token: CancellationToken::new(),
        };
        let audit = NullAuditHook;
        let ctx = ToolContext {
            agent_id: AgentId::new(),
            session_id: channel.session,
            turn_id: TurnId::new(),
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

    fn tools_at(tag: &str) -> (std::path::PathBuf, Vec<Arc<dyn Tool>>) {
        let root = tmp(tag);
        provision_workspace(&root).unwrap();
        let (tools, _) = build_workspace_tools(&root, None).unwrap();
        (root, tools)
    }

    fn tools_at_with_checkpointer(
        tag: &str,
        checkpointer: Option<Arc<GitCheckpointer>>,
    ) -> (std::path::PathBuf, Vec<Arc<dyn Tool>>) {
        let root = tmp(tag);
        provision_workspace(&root).unwrap();
        let (tools, _) = build_workspace_tools(&root, checkpointer).unwrap();
        (root, tools)
    }
    fn named<'a>(tools: &'a [Arc<dyn Tool>], name: &str) -> &'a dyn Tool {
        tools
            .iter()
            .find(|t| t.name() == name)
            .expect("tool present")
            .as_ref()
    }

    /// Extract the ref name (third whitespace-separated field) from the
    /// first line of `git for-each-ref` output (`<sha> <type> <refname>`
    /// per line).
    fn first_ref_name(for_each_ref_output: &str) -> String {
        for_each_ref_output
            .lines()
            .next()
            .expect("at least one checkpoint ref")
            .split_whitespace()
            .nth(2)
            .expect("for-each-ref line has a ref name field")
            .to_string()
    }

    #[test]
    fn write_then_read_roundtrips() {
        let (root, tools) = tools_at("rw");
        let w = run_execute(
            named(&tools, "workspace.write"),
            json!({"path":"ideas/spark.md","content":"a bright idea"}),
        );
        assert!(matches!(w, ToolOutcome::Completed { .. }));
        assert!(root.join("ideas/spark.md").is_file());
        let r = run_execute(
            named(&tools, "workspace.read"),
            json!({"path":"ideas/spark.md"}),
        );
        match r {
            ToolOutcome::Completed { output, .. } => assert_eq!(output["content"], "a bright idea"),
            other => panic!("expected Completed, got {other:?}"),
        }
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn note_appends_to_a_dated_journal_file() {
        let (root, tools) = tools_at("note");
        let n = run_execute(
            named(&tools, "workspace.note"),
            json!({"content":"today I learned X"}),
        );
        let appended = match n {
            ToolOutcome::Completed { output, .. } => {
                output["appended_to"].as_str().unwrap().to_string()
            }
            other => panic!("expected Completed, got {other:?}"),
        };
        assert!(appended.starts_with("journal/") && appended.ends_with(".md"));
        let body = std::fs::read_to_string(root.join(&appended)).unwrap();
        assert!(body.contains("today I learned X"));
        // A second note appends rather than clobbers.
        run_execute(
            named(&tools, "workspace.note"),
            json!({"content":"second thought"}),
        );
        let body2 = std::fs::read_to_string(root.join(&appended)).unwrap();
        assert!(body2.contains("today I learned X") && body2.contains("second thought"));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn write_refuses_dotgit_config() {
        let (root, tools) = tools_at("write-dotgit");
        let outcome = run_execute(
            named(&tools, "workspace.write"),
            json!({"path": ".git/config", "content": "[core]\n\tfsmonitor = /tmp/x\n"}),
        );
        match outcome {
            ToolOutcome::Failed(AivyxError::Tool { detail, .. }) => {
                assert!(detail.contains("Writing inside .git is blocked"), "{detail}");
            }
            other => panic!("expected refusal for .git/config, got {other:?}"),
        }
        assert!(!root.join(".git/config").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    #[cfg(unix)]
    fn write_refuses_dotgit_via_a_symlink_alias() {
        // `<root>/.git/` is a real dir; `<root>/link -> .git` is a symlink
        // alias. `path: "link/config"` lexically resolves with no `.git`
        // *component* at all, but canonicalizes through the symlink to
        // `<root>/.git/config` once the parent is resolved.
        use std::os::unix::fs::symlink;
        let (root, tools) = tools_at("write-dotgit-symlink");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        symlink(root.join(".git"), root.join("link")).expect("can create alias symlink");

        let outcome = run_execute(
            named(&tools, "workspace.write"),
            json!({"path": "link/config", "content": "[core]\n\tfsmonitor = /tmp/x\n"}),
        );
        match outcome {
            ToolOutcome::Failed(AivyxError::Tool { detail, .. }) => {
                assert!(detail.contains("Writing inside .git is blocked"), "{detail}");
            }
            other => panic!("expected refusal for link/config, got {other:?}"),
        }
        assert!(
            !root.join(".git/config").exists(),
            ".git/config must not have been created via the symlink alias"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn delete_refuses_dotgit_config() {
        let (root, tools) = tools_at("delete-dotgit");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/config"), b"[core]\n").unwrap();

        let outcome = run_execute(
            named(&tools, "workspace.delete"),
            json!({"path": ".git/config"}),
        );
        match outcome {
            ToolOutcome::Failed(AivyxError::Tool { detail, .. }) => {
                assert!(detail.contains("Writing inside .git is blocked"), "{detail}");
            }
            other => panic!("expected refusal for .git/config, got {other:?}"),
        }
        assert!(root.join(".git/config").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    #[cfg(unix)]
    fn delete_refuses_dotgit_via_a_symlink_alias() {
        use std::os::unix::fs::symlink;
        let (root, tools) = tools_at("delete-dotgit-symlink");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/config"), b"[core]\n").unwrap();
        symlink(root.join(".git"), root.join("link")).expect("can create alias symlink");

        let outcome = run_execute(
            named(&tools, "workspace.delete"),
            json!({"path": "link/config"}),
        );
        match outcome {
            ToolOutcome::Failed(AivyxError::Tool { detail, .. }) => {
                assert!(detail.contains("Writing inside .git is blocked"), "{detail}");
            }
            other => panic!("expected refusal for link/config, got {other:?}"),
        }
        assert!(
            root.join(".git/config").exists(),
            ".git/config must not have been deleted via the symlink alias"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    #[cfg(unix)]
    fn note_refuses_a_category_that_is_a_symlink_alias_to_dotgit() {
        // `category` sanitizes to a bare path component (no `/` survives a
        // *single*-component category — this targets the case where the
        // category itself IS an existing symlink pointing at `.git`, so
        // `rel = "<category>/<date>.md"` canonicalizes through it).
        use std::os::unix::fs::symlink;
        let (root, tools) = tools_at("note-dotgit-symlink");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        symlink(root.join(".git"), root.join("link")).expect("can create alias symlink");

        let outcome = run_execute(
            named(&tools, "workspace.note"),
            json!({"content": "pwned", "category": "link"}),
        );
        match outcome {
            ToolOutcome::Failed(AivyxError::Tool { detail, .. }) => {
                assert!(detail.contains("Writing inside .git is blocked"), "{detail}");
            }
            other => panic!("expected refusal for a symlink-aliased category, got {other:?}"),
        }
        let leaked = std::fs::read_dir(root.join(".git"))
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().ends_with(".md"));
        assert!(!leaked, "no .md journal file must have landed inside .git");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn note_refuses_a_category_that_smuggles_a_dotgit_component() {
        // `category` is sanitized with `trim_matches`, which only strips
        // leading/trailing non-alphanumeric chars — an interior `/.git/`
        // survives untouched, so `category` is a second model-influenced
        // path input alongside `path` on the other tools.
        let (root, tools) = tools_at("note-dotgit");
        let outcome = run_execute(
            named(&tools, "workspace.note"),
            json!({"content": "pwned", "category": "x/.git/hooks"}),
        );
        match outcome {
            ToolOutcome::Failed(AivyxError::Tool { detail, .. }) => {
                assert!(detail.contains("Writing inside .git is blocked"), "{detail}");
            }
            other => panic!("expected refusal for a .git-smuggling category, got {other:?}"),
        }
        assert!(!root.join("x/.git").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn list_shows_seeded_structure() {
        let (root, tools) = tools_at("list");
        let l = run_execute(named(&tools, "workspace.list"), json!({}));
        match l {
            ToolOutcome::Completed { output, .. } => {
                let names: Vec<&str> = output["entries"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|e| e["name"].as_str().unwrap())
                    .collect();
                assert!(names.contains(&"journal") && names.contains(&"README.md"));
            }
            other => panic!("expected Completed, got {other:?}"),
        }
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn delete_removes_a_file() {
        let (root, tools) = tools_at("del");
        std::fs::write(root.join("ideas/tmp.md"), "x").unwrap();
        let d = run_execute(
            named(&tools, "workspace.delete"),
            json!({"path":"ideas/tmp.md"}),
        );
        assert!(matches!(d, ToolOutcome::Completed { .. }));
        assert!(!root.join("ideas/tmp.md").exists());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn traversal_outside_workspace_is_denied_by_scope() {
        let (root, tools) = tools_at("traverse");
        let read = named(&tools, "workspace.read");
        // A `..` escape must yield the deny scope (not a real workspace path).
        let scope = read.required_scope(&json!({"path":"../../etc/passwd"}));
        assert!(
            scope.as_str().contains("__deny__"),
            "got: {}",
            scope.as_str()
        );
        // And an in-workspace path yields a real workspace scope.
        let ok = read.required_scope(&json!({"path":"journal/x.md"}));
        assert!(ok.as_str().starts_with("workspace:") && !ok.as_str().contains("__deny__"));
        std::fs::remove_dir_all(&root).ok();
    }

    // ---- Chapter O.2 hardening: the canonical layer (2026-07-07) -----
    //
    // Regression for a real gap (found via Chapter Almanac's guard-coverage
    // audit): workspace.rs's own comment claimed "the same lexical fence
    // fs.* uses", but fs.* pairs that lexical layer with a canonical
    // re-check at execute time specifically to catch a symlink planted
    // after construction — workspace.* never had that second layer. These
    // tests plant exactly that symlink and confirm each tool now refuses it.

    #[test]
    #[cfg(unix)]
    fn read_refuses_a_symlink_escaping_the_workspace() {
        use std::os::unix::fs::symlink;
        let (root, tools) = tools_at("read-escape");
        let outside = tmp("read-escape-outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "outside content").unwrap();
        symlink(outside.join("secret.txt"), root.join("escape")).unwrap();

        let outcome = run_execute(named(&tools, "workspace.read"), json!({"path":"escape"}));
        match outcome {
            ToolOutcome::Failed(AivyxError::Tool { detail, .. }) => {
                assert!(detail.contains("escapes workspace root"), "{detail}");
            }
            other => panic!("symlink escape must be refused, got {other:?}"),
        }
        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&outside).ok();
    }

    #[test]
    #[cfg(unix)]
    fn list_refuses_a_symlink_escaping_the_workspace() {
        use std::os::unix::fs::symlink;
        let (root, tools) = tools_at("list-escape");
        let outside = tmp("list-escape-outside");
        std::fs::create_dir_all(&outside).unwrap();
        symlink(&outside, root.join("escape_dir")).unwrap();

        let outcome = run_execute(
            named(&tools, "workspace.list"),
            json!({"path":"escape_dir"}),
        );
        match outcome {
            ToolOutcome::Failed(AivyxError::Tool { detail, .. }) => {
                assert!(detail.contains("escapes workspace root"), "{detail}");
            }
            other => panic!("symlink escape must be refused, got {other:?}"),
        }
        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&outside).ok();
    }

    #[test]
    #[cfg(unix)]
    fn write_refuses_a_symlink_escaping_the_workspace() {
        use std::os::unix::fs::symlink;
        let (root, tools) = tools_at("write-escape");
        let outside = tmp("write-escape-outside");
        std::fs::create_dir_all(&outside).unwrap();
        // A pre-existing symlink at the write target, pointing outside.
        symlink(outside.join("clobbered.txt"), root.join("escape.md")).unwrap();

        let outcome = run_execute(
            named(&tools, "workspace.write"),
            json!({"path":"escape.md","content":"pwned"}),
        );
        match outcome {
            ToolOutcome::Failed(AivyxError::Tool { detail, .. }) => {
                assert!(detail.contains("existing symlink"), "{detail}");
            }
            other => panic!("symlink escape must be refused, got {other:?}"),
        }
        assert!(
            !outside.join("clobbered.txt").exists(),
            "the write must never have followed the symlink through"
        );
        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&outside).ok();
    }

    #[test]
    #[cfg(unix)]
    fn delete_refuses_a_symlink_escaping_the_workspace() {
        use std::os::unix::fs::symlink;
        let (root, tools) = tools_at("delete-escape");
        let outside = tmp("delete-escape-outside");
        std::fs::create_dir_all(&outside).unwrap();
        symlink(&outside, root.join("escape_dir")).unwrap();

        let outcome = run_execute(
            named(&tools, "workspace.delete"),
            json!({"path":"escape_dir/anything"}),
        );
        match outcome {
            ToolOutcome::Failed(AivyxError::Tool { detail, .. }) => {
                assert!(detail.contains("escapes workspace root"), "{detail}");
            }
            other => panic!("symlink escape must be refused, got {other:?}"),
        }
        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&outside).ok();
    }

    #[test]
    #[cfg(unix)]
    fn note_refuses_a_symlink_escaping_the_workspace() {
        use std::os::unix::fs::symlink;
        let (root, tools) = tools_at("note-escape");
        let outside = tmp("note-escape-outside");
        std::fs::create_dir_all(&outside).unwrap();
        // Plant the escaping symlink at the exact category/date path
        // workspace.note would write to.
        let (date, _) = current_date_and_unix();
        std::fs::create_dir_all(root.join("journal")).unwrap();
        symlink(
            outside.join("clobbered.md"),
            root.join("journal").join(format!("{date}.md")),
        )
        .unwrap();

        let outcome = run_execute(named(&tools, "workspace.note"), json!({"content":"pwned"}));
        match outcome {
            ToolOutcome::Failed(AivyxError::Tool { detail, .. }) => {
                assert!(detail.contains("existing symlink"), "{detail}");
            }
            other => panic!("symlink escape must be refused, got {other:?}"),
        }
        assert!(
            !outside.join("clobbered.md").exists(),
            "the note must never have followed the symlink through"
        );
        std::fs::remove_dir_all(&root).ok();
        std::fs::remove_dir_all(&outside).ok();
    }

    // ---- workspace.* checkpointing (aivyx-checkpoint git.rs/workspace.rs adoption) ----

    #[test]
    fn write_checkpoints_before_writing_when_configured() {
        let root = tmp("write-checkpoint");
        provision_workspace(&root).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(aivyx_checkpoint::test_support::init_repo(&root));
        let checkpointer = rt
            .block_on(GitCheckpointer::detect(&root, Vec::new()))
            .expect("root is a real git repo");
        let (tools, _) = build_workspace_tools(&root, Some(Arc::new(checkpointer))).unwrap();

        let w = run_execute(
            named(&tools, "workspace.write"),
            json!({"path":"ideas/spark.md","content":"a bright idea"}),
        );
        assert!(matches!(w, ToolOutcome::Completed { .. }));

        let refs = rt.block_on(aivyx_checkpoint::test_support::git(
            &root,
            &["for-each-ref", "refs/aivyx/checkpoints/"],
        ));
        assert!(
            !refs.trim().is_empty(),
            "a checkpoint ref must exist before the write"
        );

        // Prove the checkpoint captured PRE-mutation state: the write
        // target didn't exist on disk until `workspace.write`'s own
        // `std::fs::write` ran, which happens strictly *after* the
        // checkpoint call inside `execute()` -- so the checkpoint's tree
        // must not contain it.
        let ref_name = first_ref_name(&refs);
        let tree_check = rt.block_on(aivyx_checkpoint::test_support::git(
            &root,
            &["ls-tree", "-r", "--name-only", &ref_name],
        ));
        assert!(
            !tree_check.contains("ideas/spark.md"),
            "checkpoint tree must predate the new file, got: {tree_check}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn delete_checkpoints_before_deleting_when_configured() {
        let root = tmp("delete-checkpoint");
        provision_workspace(&root).unwrap();
        std::fs::write(root.join("ideas/tmp.md"), "x").unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(aivyx_checkpoint::test_support::init_repo(&root));
        let checkpointer = rt
            .block_on(GitCheckpointer::detect(&root, Vec::new()))
            .expect("root is a real git repo");
        let (tools, _) = build_workspace_tools(&root, Some(Arc::new(checkpointer))).unwrap();

        let d = run_execute(
            named(&tools, "workspace.delete"),
            json!({"path":"ideas/tmp.md"}),
        );
        assert!(matches!(d, ToolOutcome::Completed { .. }));

        let refs = rt.block_on(aivyx_checkpoint::test_support::git(
            &root,
            &["for-each-ref", "refs/aivyx/checkpoints/"],
        ));
        assert!(
            !refs.trim().is_empty(),
            "a checkpoint ref must exist before the delete"
        );

        // Prove the checkpoint captured PRE-mutation state: the deleted
        // file was written to disk before the tool call and the
        // checkpoint runs before `workspace.delete`'s own removal, so the
        // checkpoint's tree must still contain it.
        let ref_name = first_ref_name(&refs);
        let tree_check = rt.block_on(aivyx_checkpoint::test_support::git(
            &root,
            &["ls-tree", "-r", "--name-only", &ref_name],
        ));
        assert!(
            tree_check.contains("ideas/tmp.md"),
            "checkpoint tree must still contain the file that's about to be \
             deleted, got: {tree_check}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn note_checkpoints_before_appending_when_configured() {
        let root = tmp("note-checkpoint");
        provision_workspace(&root).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(aivyx_checkpoint::test_support::init_repo(&root));
        let checkpointer = rt
            .block_on(GitCheckpointer::detect(&root, Vec::new()))
            .expect("root is a real git repo");
        let (tools, _) = build_workspace_tools(&root, Some(Arc::new(checkpointer))).unwrap();

        let n = run_execute(
            named(&tools, "workspace.note"),
            json!({"content":"today I learned X"}),
        );
        let appended_to = match &n {
            ToolOutcome::Completed { output, .. } => {
                output["appended_to"].as_str().unwrap().to_string()
            }
            other => panic!("expected Completed, got {other:?}"),
        };

        let refs = rt.block_on(aivyx_checkpoint::test_support::git(
            &root,
            &["for-each-ref", "refs/aivyx/checkpoints/"],
        ));
        assert!(
            !refs.trim().is_empty(),
            "a checkpoint ref must exist before the note append"
        );

        // Prove the checkpoint captured PRE-mutation state: the dated
        // journal file didn't exist on disk until `workspace.note`'s own
        // append-open ran, which happens strictly *after* the checkpoint
        // call inside `execute()` -- so the checkpoint's tree must not
        // contain it.
        let ref_name = first_ref_name(&refs);
        let tree_check = rt.block_on(aivyx_checkpoint::test_support::git(
            &root,
            &["ls-tree", "-r", "--name-only", &ref_name],
        ));
        assert!(
            !tree_check.contains(&appended_to),
            "checkpoint tree must predate the new journal file, got: {tree_check}"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn write_skips_checkpoint_when_no_checkpointer_configured() {
        let root = tmp("write-no-checkpoint");
        provision_workspace(&root).unwrap();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(aivyx_checkpoint::test_support::init_repo(&root));
        // Root IS a real git repo, but no checkpointer was wired in --
        // proves workspace.rs's own gate is `self.checkpointer.is_some()`,
        // not "does this directory happen to be a git repo" (that
        // decision belongs to the binary's construction site, not here).
        let (tools, _) = build_workspace_tools(&root, None).unwrap();

        let w = run_execute(
            named(&tools, "workspace.write"),
            json!({"path":"ideas/spark.md","content":"a bright idea"}),
        );
        assert!(matches!(w, ToolOutcome::Completed { .. }));

        let refs = rt.block_on(aivyx_checkpoint::test_support::git(
            &root,
            &["for-each-ref", "refs/aivyx/checkpoints/"],
        ));
        assert!(
            refs.trim().is_empty(),
            "no checkpointer configured -- no ref should exist"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    // ---- mutates_fs_root regression (final-review Fix 3) ----
    //
    // Each mutating workspace.* tool checkpoints itself, scoped to
    // workspace_root -- it must never be wired into ConcreteAgent's
    // fs_root-scoped checkpointer hook (agent.rs's dispatch loop fires that
    // hook for any tool where this returns true), since workspace_root is
    // not fs_root.

    #[test]
    fn workspace_write_does_not_mutate_fs_root() {
        let (root, tools) = tools_at("write-mutates-fs-root");
        assert!(!named(&tools, "workspace.write").mutates_fs_root());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn workspace_delete_does_not_mutate_fs_root() {
        let (root, tools) = tools_at("delete-mutates-fs-root");
        assert!(!named(&tools, "workspace.delete").mutates_fs_root());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn workspace_note_does_not_mutate_fs_root() {
        let (root, tools) = tools_at("note-mutates-fs-root");
        assert!(!named(&tools, "workspace.note").mutates_fs_root());
        std::fs::remove_dir_all(&root).ok();
    }
}
