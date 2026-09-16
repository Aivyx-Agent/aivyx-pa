//! Phase 109 — `git.status` and `git.diff` substrate tools.
//! Chapter Forge (FG.3) — `git.commit`, the destructive sibling.
//!
//! The read tools (`git.status` / `git.diff`) share the `git.read`
//! scope base; `git.commit` is gated by the separate `git.write`
//! base added at Chapter Forge FG.2 (Amendment A13). Writing repo
//! history is at least as sensitive as `shell.exec` / `fs.delete`,
//! so `git.write` is Trusted-tier only and `git.commit` is
//! confirm-first when the operator enables `[access]
//! confirm_destructive`. The write tool reuses the **same**
//! operator `[git] repos` allow-set the read tools gate against —
//! a commit is only allowed inside a configured repo.
//!
//! Both tool families share a single `git.read` scope base qualified
//! by repo path. The shared-scope design is documented in
//! Amendment A12 (`docs/amendments/2026-05-28-substrate-tool-count-thirteen.md`):
//! `git.status` and `git.diff` are both read-only inspection
//! of the same repo, so the natural capability grant is "this
//! role can read this repo," not "this role can run `git
//! status` but not `git diff`."
//!
//! ## Repo-path validation
//!
//! Both tools take a `repo` field on their input that names
//! one of the operator's configured allowed repos. The
//! validator canonicalizes the input against the allow-set;
//! any repo path not on the list refuses to dispatch.
//!
//! Why an explicit allow-list rather than free-path
//! traversal: `git` happily walks parent `.git/` directories
//! (`-C /tmp` resolves to whatever git repo `/tmp/.git` points
//! to, or panics if there isn't one). Without the allow-list,
//! an agent with `git.read:**` could inspect any git repo on
//! the operator's filesystem. The allow-list pattern matches
//! how `fs.read` requires a sandbox root: the operator gates
//! which paths are inspectable, the tool gates which
//! operations.
//!
//! ## Shell-out, not git2
//!
//! Phase 109 ships these by shelling out to the system `git`
//! binary rather than depending on the `git2` Rust crate.
//! The trade-off: `git2` is faster, type-safer, and version-
//! independent — but `git` is universally available on any
//! developer's machine, has zero binary-size cost, and means
//! Phase 109 ships with zero new workspace deps. A future
//! phase that wants per-call performance or richer parsing
//! can swap in `git2` behind the same tool surface without
//! changing the `Tool` impl.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::{
    AivyxError, CapabilitySet, ExecutionConfiner, GitCheckpointer, NoopConfiner, Tool, ToolContext,
    ToolId, ToolOutcome, Verification, default_confiner,
};
use aivyx_capability::Scope;

// ---------------------------------------------------------------------------
// Config + shared builder
// ---------------------------------------------------------------------------

/// Construction inputs for [`GitStatusTool`] / [`GitDiffTool`].
/// The two tools share configuration because they share the
/// `git.read` scope base — same allow-set of repo paths gates
/// both.
pub struct GitReadToolConfig {
    repos: Vec<PathBuf>,
    require_enforcement: bool,
}

impl GitReadToolConfig {
    /// Construct from an operator-supplied list of repo paths.
    /// Each path will be canonicalized at `build` time and
    /// any path that does not resolve to a directory containing
    /// a `.git` entry will fail startup with `AivyxError::Config`.
    pub fn new(repos: impl IntoIterator<Item = PathBuf>) -> Self {
        GitReadToolConfig {
            repos: repos.into_iter().collect(),
            require_enforcement: true,
        }
    }

    /// See `GitWriteToolConfig::with_require_enforcement` — same flag,
    /// same default, same reasoning.
    pub fn with_require_enforcement(mut self, require_enforcement: bool) -> Self {
        self.require_enforcement = require_enforcement;
        self
    }

    /// Canonicalize the allow-set and return a ready-to-register
    /// `(GitStatusTool, GitDiffTool)` pair. The pair shares the
    /// canonicalized allow-set behind an `Arc<[PathBuf]>` so the
    /// two tools register independently in the dispatch registry
    /// while still gating against the same allow-set.
    pub fn build(self) -> Result<(GitStatusTool, GitDiffTool), AivyxError> {
        let allow_set: Arc<[PathBuf]> = canonicalize_repo_allow_set(self.repos, "git.read")?.into();
        Ok((
            GitStatusTool {
                id: ToolId::new(),
                repos: Arc::clone(&allow_set),
                schema: status_input_schema(),
                require_enforcement: self.require_enforcement,
            },
            GitDiffTool {
                id: ToolId::new(),
                repos: allow_set,
                schema: diff_input_schema(),
                require_enforcement: self.require_enforcement,
            },
        ))
    }
}

/// Construction inputs for [`GitCommitTool`] — Chapter Forge (FG.3).
/// Mirrors [`GitReadToolConfig`] (same canonicalized repo allow-set)
/// but carries the `confirm_destructive` flag, since a commit is an
/// irreversible write of repo history.
pub struct GitWriteToolConfig {
    repos: Vec<PathBuf>,
    confirm_destructive: bool,
    require_enforcement: bool,
    checkpointers: HashMap<PathBuf, Arc<GitCheckpointer>>,
}

impl GitWriteToolConfig {
    /// Construct from an operator-supplied list of repo paths — the
    /// **same** `[git] repos` allow-set the read tools gate against.
    pub fn new(repos: impl IntoIterator<Item = PathBuf>) -> Self {
        GitWriteToolConfig {
            repos: repos.into_iter().collect(),
            confirm_destructive: false,
            require_enforcement: true,
            checkpointers: HashMap::new(),
        }
    }

    /// Enable confirm-first gating: when on, `git.commit` refuses to
    /// run without `confirmed: true` in its input, matching the
    /// `fs.delete` / `fs.write`-overwrite confirm-first pattern
    /// (Chapter N). Wired from `[access] confirm_destructive`.
    pub fn with_confirm_destructive(mut self, confirm: bool) -> Self {
        self.confirm_destructive = confirm;
        self
    }

    /// Whether Landlock confinement (via `aivyx-confine`) must succeed
    /// for `git.commit` to run at all. `true` (fail-closed) by default.
    pub fn with_require_enforcement(mut self, require_enforcement: bool) -> Self {
        self.require_enforcement = require_enforcement;
        self
    }

    /// Attach a pre-built per-repo checkpoint map: one `GitCheckpointer`
    /// per entry in the `[git] repos` allow-set that both is a real git
    /// work tree and had `GitCheckpointer::detect` succeed for it, keyed
    /// by that repo's own canonical path. Building these requires an
    /// async `detect()` call per repo plus operator sensitive-path config
    /// this crate has no visibility into, so the caller (the binary)
    /// builds the map once at startup and hands it in fully-formed —
    /// `build()` below stays synchronous. Defaults to an empty map (no
    /// checkpointing for any repo) when this method is never called. A
    /// repo missing from the map simply gets no checkpoint before its
    /// commits — the same graceful-degradation contract `fs_root`'s own
    /// checkpointer already has when `fs_root` isn't a git repo.
    pub fn with_checkpointers(
        mut self,
        checkpointers: HashMap<PathBuf, Arc<GitCheckpointer>>,
    ) -> Self {
        self.checkpointers = checkpointers;
        self
    }

    /// Canonicalize the allow-set (same validation as the read pair —
    /// each entry must be a directory containing a `.git/`) and return
    /// a ready-to-register [`GitCommitTool`].
    pub fn build(self) -> Result<GitCommitTool, AivyxError> {
        let allow_set: Arc<[PathBuf]> =
            canonicalize_repo_allow_set(self.repos, "git.write")?.into();
        Ok(GitCommitTool {
            id: ToolId::new(),
            repos: allow_set,
            confirm_destructive: self.confirm_destructive,
            schema: commit_input_schema(),
            require_enforcement: self.require_enforcement,
            checkpointers: self.checkpointers,
        })
    }
}

// ---------------------------------------------------------------------------
// Tool: git.status
// ---------------------------------------------------------------------------

/// `git.status` — runs `git status --porcelain --untracked-files=all`
/// against an operator-allowed repo path and returns parsed
/// entries as JSON.
#[derive(Debug)]
pub struct GitStatusTool {
    id: ToolId,
    repos: Arc<[PathBuf]>,
    schema: Value,
    require_enforcement: bool,
}

impl GitStatusTool {
    /// The canonical allow-set this tool gates against.
    pub fn repos(&self) -> &[PathBuf] {
        &self.repos
    }
}

#[async_trait]
impl Tool for GitStatusTool {
    fn id(&self) -> ToolId {
        self.id
    }

    fn name(&self) -> &str {
        "git.status"
    }

    // Chapter Bulwark/Picket — porcelain output includes third-party-
    // authored file paths (e.g. from a pulled branch) with no operator
    // review before it enters model context. Fence it as untrusted data
    // and run the injection scan over it, same as fs.read.
    fn output_is_untrusted(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "Run `git status --porcelain` against a configured repo \
         path. Input is a JSON object with a `repo` field naming \
         one of the operator's allowed git repos (canonical path \
         match). Returns a JSON object with an `entries` array — \
         each entry has `status_code` (the two-char porcelain \
         code) and `path`."
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    // git.read is never auto-granted via the backcompat floor's generic
    // sweep either, for the same reason as git.write -- an operator
    // declares `git.read:**` or a per-repo grant explicitly.
    fn required_scope(&self, input: &Value) -> Scope {
        match resolve_repo(input, &self.repos) {
            Some(abs) => {
                Scope::parse(&format!("git.read:{}", abs.display())).unwrap_or_else(deny_scope)
            }
            None => deny_scope(),
        }
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        let repo = match resolve_repo(&input, &self.repos) {
            Some(p) => p,
            None => {
                return ToolOutcome::Failed(AivyxError::Tool {
                    tool: self.id,
                    detail: "git.status: `repo` field missing or not in allow-set".to_string(),
                });
            }
        };

        let mut command = tokio::process::Command::new("git");
        command
            .arg("-C")
            .arg(&repo)
            .arg("status")
            .arg("--porcelain")
            .arg("--untracked-files=all");
        let confiner = confiner_for(&repo, self.require_enforcement);
        let mut command = confiner.confine(command);
        let output = match command.output().await {
            Ok(o) => o,
            Err(e) => {
                return ToolOutcome::Failed(AivyxError::Tool {
                    tool: self.id,
                    detail: format!("git.status: spawn failed: {e}"),
                });
            }
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: format!(
                    "git.status: exit code {:?}: {}",
                    output.status.code(),
                    stderr.trim()
                ),
            });
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let entries = parse_porcelain(&stdout);

        ToolOutcome::Completed {
            output: json!({ "repo": repo.display().to_string(), "entries": entries }),
            verified: Verification::Verified,
        }
    }
}

// ---------------------------------------------------------------------------
// Tool: git.diff
// ---------------------------------------------------------------------------

/// `git.diff` — runs `git diff [--cached] [<path>]` against
/// an operator-allowed repo path and returns the unified-diff
/// output as a string.
#[derive(Debug)]
pub struct GitDiffTool {
    id: ToolId,
    repos: Arc<[PathBuf]>,
    schema: Value,
    require_enforcement: bool,
}

impl GitDiffTool {
    /// The canonical allow-set this tool gates against.
    pub fn repos(&self) -> &[PathBuf] {
        &self.repos
    }
}

#[async_trait]
impl Tool for GitDiffTool {
    fn id(&self) -> ToolId {
        self.id
    }

    fn name(&self) -> &str {
        "git.diff"
    }

    // Chapter Bulwark/Picket — a diff's hunks and file paths are real
    // third-party-authored content once a branch is pulled, with no
    // operator review before it enters model context. Fence it as
    // untrusted data and run the injection scan over it, same as fs.read.
    fn output_is_untrusted(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "Run `git diff` against a configured repo path. Input \
         is a JSON object with a `repo` field naming one of the \
         operator's allowed git repos (canonical path match), \
         an optional `cached` boolean (`true` for the staged \
         diff, default false for the working-tree diff), and an \
         optional `path` string scoping the diff to a single \
         file or directory within the repo. Returns a JSON \
         object with a `diff` field carrying the unified-diff \
         output as a string."
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn required_scope(&self, input: &Value) -> Scope {
        match resolve_repo(input, &self.repos) {
            Some(abs) => {
                Scope::parse(&format!("git.read:{}", abs.display())).unwrap_or_else(deny_scope)
            }
            None => deny_scope(),
        }
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        let repo = match resolve_repo(&input, &self.repos) {
            Some(p) => p,
            None => {
                return ToolOutcome::Failed(AivyxError::Tool {
                    tool: self.id,
                    detail: "git.diff: `repo` field missing or not in allow-set".to_string(),
                });
            }
        };

        let cached = input
            .get("cached")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let path = input.get("path").and_then(|v| v.as_str());

        let mut cmd = tokio::process::Command::new("git");
        cmd.arg("-C").arg(&repo).arg("diff");
        if cached {
            cmd.arg("--cached");
        }
        if let Some(p) = path {
            // Refuse path traversal — `git diff` would happily
            // accept `../sibling/file.txt` and walk outside
            // the configured repo. We require the path to be
            // a plain relative path.
            if p.starts_with('/') || p.contains("..") {
                return ToolOutcome::Failed(AivyxError::Tool {
                    tool: self.id,
                    detail: format!(
                        "git.diff: `path` must be repo-relative and \
                         must not contain `..`; got {p:?}"
                    ),
                });
            }
            cmd.arg("--").arg(p);
        }

        let confiner = confiner_for(&repo, self.require_enforcement);
        let mut cmd = confiner.confine(cmd);
        let output = match cmd.output().await {
            Ok(o) => o,
            Err(e) => {
                return ToolOutcome::Failed(AivyxError::Tool {
                    tool: self.id,
                    detail: format!("git.diff: spawn failed: {e}"),
                });
            }
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: format!(
                    "git.diff: exit code {:?}: {}",
                    output.status.code(),
                    stderr.trim()
                ),
            });
        }

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();

        ToolOutcome::Completed {
            output: json!({
                "repo": repo.display().to_string(),
                "cached": cached,
                "path": path,
                "diff": stdout,
            }),
            verified: Verification::Verified,
        }
    }
}

// ---------------------------------------------------------------------------
// Tool: git.commit — Chapter Forge (FG.3)
// ---------------------------------------------------------------------------

/// `git.commit` — stages the given repo-relative paths and commits
/// them with a message inside an operator-allowed repo. The
/// destructive sibling to `git.status` / `git.diff`: gated by the
/// `git.write` scope (Trusted-tier only) and confirm-first when the
/// operator enables `[access] confirm_destructive`. Shells out to the
/// system `git` (same as the read tools — no `git2` dep).
pub struct GitCommitTool {
    id: ToolId,
    repos: Arc<[PathBuf]>,
    confirm_destructive: bool,
    schema: Value,
    require_enforcement: bool,
    checkpointers: HashMap<PathBuf, Arc<GitCheckpointer>>,
}

impl std::fmt::Debug for GitCommitTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitCommitTool")
            .field("id", &self.id)
            .field("repos", &self.repos)
            .field("confirm_destructive", &self.confirm_destructive)
            .field("schema", &self.schema)
            .field("require_enforcement", &self.require_enforcement)
            .field("checkpointed_repos", &self.checkpointers.len())
            .finish()
    }
}

impl GitCommitTool {
    /// The canonical allow-set this tool gates against.
    pub fn repos(&self) -> &[PathBuf] {
        &self.repos
    }
}

#[async_trait]
impl Tool for GitCommitTool {
    fn id(&self) -> ToolId {
        self.id
    }

    fn name(&self) -> &str {
        "git.commit"
    }

    // Chapter Bulwark/Picket — this tool's output can carry attacker-
    // authored content (e.g. `git commit`'s stdout/stderr echoing a
    // crafted message on failure) with no operator review before it
    // enters model context. Fence it as untrusted data and run the
    // injection scan over it, same as fs.read.
    fn output_is_untrusted(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "Stage the given paths and create a commit in a configured \
         repo. Input is a JSON object with a `repo` field naming one \
         of the operator's allowed git repos (canonical path match), \
         a `message` string (the commit message), and a `paths` array \
         of repo-relative file paths to stage (each must not start \
         with `/` or contain `..`). When the operator has enabled \
         confirm-first for destructive ops, also pass `confirmed: \
         true` after showing them what will be committed. Returns the \
         new commit hash. Writing history is irreversible — Trusted \
         tier only."
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    // git.write is never auto-granted via the backcompat floor's generic
    // sweep (Tool::auto_grantable_in_backcompat_floor's default `false`,
    // not overridden here) -- an operator who wants the agent to commit
    // declares `git.write:<repo>` in a role's own `capability_scopes`.
    fn required_scope(&self, input: &Value) -> Scope {
        match resolve_repo(input, &self.repos) {
            Some(abs) => Scope::parse(&format!("git.write:{}", abs.display()))
                .unwrap_or_else(write_deny_scope),
            None => write_deny_scope(),
        }
    }

    async fn execute(&self, input: Value, ctx: &ToolContext<'_>) -> ToolOutcome {
        let repo = match resolve_repo(&input, &self.repos) {
            Some(p) => p,
            None => {
                return ToolOutcome::Failed(AivyxError::Tool {
                    tool: self.id,
                    detail: "git.commit: `repo` field missing or not in allow-set".to_string(),
                });
            }
        };

        let confiner = confiner_for(&repo, self.require_enforcement);

        let message = match input.get("message").and_then(|v| v.as_str()) {
            Some(m) if !m.trim().is_empty() => m,
            _ => {
                return ToolOutcome::Failed(AivyxError::Tool {
                    tool: self.id,
                    detail: "git.commit: `message` must be a non-empty string".to_string(),
                });
            }
        };

        // Collect + validate the repo-relative paths to stage. At
        // least one is required: we stage explicit paths rather than
        // `git commit -a` so a hallucinated catch-all can't sweep
        // unintended changes into a commit.
        let paths: Vec<&str> = match input.get("paths").and_then(|v| v.as_array()) {
            Some(arr) if !arr.is_empty() => {
                let mut out = Vec::with_capacity(arr.len());
                for item in arr {
                    let Some(p) = item.as_str() else {
                        return ToolOutcome::Failed(AivyxError::Tool {
                            tool: self.id,
                            detail: "git.commit: every entry in `paths` must be a string"
                                .to_string(),
                        });
                    };
                    // Same traversal guard as `git.diff`'s `path`:
                    // `git add` would happily stage `../sibling/x`
                    // outside the configured repo.
                    if p.starts_with('/') || p.contains("..") {
                        return ToolOutcome::Failed(AivyxError::Tool {
                            tool: self.id,
                            detail: format!(
                                "git.commit: each `paths` entry must be repo-relative \
                                 and must not contain `..`; got {p:?}"
                            ),
                        });
                    }
                    out.push(p);
                }
                out
            }
            _ => {
                return ToolOutcome::Failed(AivyxError::Tool {
                    tool: self.id,
                    detail: "git.commit: `paths` must be a non-empty array of \
                             repo-relative file paths to stage"
                        .to_string(),
                });
            }
        };

        // ---- Chapter N confirm-first: refuse an unconfirmed commit
        // when the operator enabled `[access] confirm_destructive`. ----
        if self.confirm_destructive && !git_is_confirmed(&input) {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: format!(
                    "git.commit: refusing to commit {} path(s) to {} without confirmation. \
                     This writes repo history and the operator enabled confirm-first \
                     (`[access] confirm_destructive`). Show the operator the paths and \
                     message, get approval, then re-call with `confirmed: true`.",
                    paths.len(),
                    repo.display(),
                ),
            });
        }

        if let Some(checkpointer) = self.checkpointers.get(&repo) {
            checkpointer
                .checkpoint("git.commit", ctx.cancellation)
                .await;
        }

        // Stage: `git -C <repo> add -- <paths...>`.
        let mut add_cmd = tokio::process::Command::new("git");
        add_cmd.arg("-C").arg(&repo).arg("add").arg("--");
        for p in &paths {
            add_cmd.arg(p);
        }
        let mut add_cmd = confiner.confine(add_cmd);
        match add_cmd.output().await {
            Ok(o) if o.status.success() => {}
            Ok(o) => {
                let stderr = String::from_utf8_lossy(&o.stderr).to_string();
                return ToolOutcome::Failed(AivyxError::Tool {
                    tool: self.id,
                    detail: format!(
                        "git.commit: `git add` exit code {:?}: {}",
                        o.status.code(),
                        stderr.trim()
                    ),
                });
            }
            Err(e) => {
                return ToolOutcome::Failed(AivyxError::Tool {
                    tool: self.id,
                    detail: format!("git.commit: `git add` spawn failed: {e}"),
                });
            }
        }

        // Commit: `git -C <repo> commit -m <message>`. Author identity
        // comes from the repo's own git config (operator-owned), same
        // as a manual commit.
        let mut commit_cmd = tokio::process::Command::new("git");
        commit_cmd
            .arg("-C")
            .arg(&repo)
            .arg("commit")
            .arg("-m")
            .arg(message);
        let mut commit_cmd = confiner.confine(commit_cmd);
        let commit_out = match commit_cmd.output().await {
            Ok(o) => o,
            Err(e) => {
                return ToolOutcome::Failed(AivyxError::Tool {
                    tool: self.id,
                    detail: format!("git.commit: `git commit` spawn failed: {e}"),
                });
            }
        };
        if !commit_out.status.success() {
            let stderr = String::from_utf8_lossy(&commit_out.stderr).to_string();
            let stdout = String::from_utf8_lossy(&commit_out.stdout).to_string();
            // `git commit` writes "nothing to commit" to stdout, the
            // real errors (missing identity, etc.) to stderr — surface
            // both so the agent can act on it.
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: format!(
                    "git.commit: `git commit` exit code {:?}: {}",
                    commit_out.status.code(),
                    if stderr.trim().is_empty() {
                        stdout.trim()
                    } else {
                        stderr.trim()
                    }
                ),
            });
        }

        // Resolve the new HEAD so the caller gets the commit hash.
        let mut rev_parse_cmd = tokio::process::Command::new("git");
        rev_parse_cmd
            .arg("-C")
            .arg(&repo)
            .arg("rev-parse")
            .arg("HEAD");
        let mut rev_parse_cmd = confiner.confine(rev_parse_cmd);
        let commit_hash = match rev_parse_cmd.output().await {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
            // The commit succeeded; failing to read HEAD back is
            // non-fatal — report the commit without the hash rather
            // than a false failure.
            _ => String::new(),
        };

        ToolOutcome::Completed {
            output: json!({
                "repo": repo.display().to_string(),
                "committed": true,
                "commit": commit_hash,
                "paths": paths,
                "message": message,
            }),
            verified: Verification::Verified,
        }
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Per-repo latch: `true` the first time `repo` is seen (the caller
/// should log a warning), `false` on every subsequent call for the same
/// canonicalized repo — keeps routine git activity against a
/// worktree/submodule repo from flooding the daemon log with the
/// identical warning on every single tool call. Process-lifetime only
/// (resets on restart), which is fine: the point is deduplicating noise
/// within one running session, not persisting the fact across restarts.
/// Falls back to the raw (non-canonicalized) path on a canonicalization
/// failure — that just means two paths that *should* dedupe (e.g. a
/// symlinked alias) won't, not a correctness issue.
fn should_warn_once(repo: &Path) -> bool {
    static WARNED: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<PathBuf>>> =
        std::sync::OnceLock::new();
    let canonical = repo.canonicalize().unwrap_or_else(|_| repo.to_path_buf());
    let set = WARNED.get_or_init(|| std::sync::Mutex::new(std::collections::HashSet::new()));
    set.lock().unwrap().insert(canonical)
}

/// Build the confiner to use for a git command about to run against
/// `repo`. A linked git worktree or submodule's `.git` is a **file**
/// (not a directory) containing `gitdir: <path-to-the-real-gitdir>`,
/// which typically lives outside `repo` — Landlock confinement scoped to
/// `repo` alone would cut git off from its own real gitdir and break
/// every operation on it (`fatal: not a git repository`). Detect that
/// shape here and fall back to `NoopConfiner` for this one repo rather
/// than the real backend, logging why so an operator sees it in the
/// daemon log rather than silently getting an unconfined `git`.
///
/// Centralizing this (used by all three tools' `execute()`) means the
/// worktree/submodule check and the default-confiner construction can't
/// drift apart across call sites the way three separate copies could.
fn confiner_for(repo: &Path, require_enforcement: bool) -> Arc<dyn ExecutionConfiner> {
    if repo.join(".git").is_file() {
        if should_warn_once(repo) {
            // No `tracing` dependency in this crate (the rest of `aivyx-pa`
            // logs operator-facing warnings via `eprintln!`, e.g.
            // `aivyx-cli/src/bin/aivyx.rs`) — match that convention rather
            // than pulling in a new logging dependency for one line.
            eprintln!(
                "aivyx-pa: skipping Landlock confinement for {}: its .git is a file, \
                 not a directory, so this repo is a git worktree or submodule \
                 whose real gitdir lives outside the repo root — confining to the \
                 repo root would break git entirely here",
                repo.display(),
            );
        }
        return Arc::new(NoopConfiner);
    }
    default_confiner(repo, &[], &[], require_enforcement)
}

/// Canonicalize an operator repo allow-set: each entry must
/// canonicalize, be a directory, and contain a `.git/` entry.
/// Shared by [`GitReadToolConfig`] and [`GitWriteToolConfig`] so the
/// read and write tools validate the **same** `[git] repos` list
/// identically. `label` names the scope base in the error so a
/// startup misconfig is attributable.
fn canonicalize_repo_allow_set(
    repos: Vec<PathBuf>,
    label: &str,
) -> Result<Vec<PathBuf>, AivyxError> {
    let mut canonical = Vec::with_capacity(repos.len());
    for repo in repos {
        let abs = std::fs::canonicalize(&repo).map_err(|e| {
            AivyxError::Config(format!(
                "{label} allow-set entry {repo:?} cannot be canonicalized: {e}"
            ))
        })?;
        if !abs.is_dir() {
            return Err(AivyxError::Config(format!(
                "{label} allow-set entry {abs:?} is not a directory"
            )));
        }
        if !abs.join(".git").exists() {
            return Err(AivyxError::Config(format!(
                "{label} allow-set entry {abs:?} is not a git repo (no .git/ entry)"
            )));
        }
        canonical.push(abs);
    }
    Ok(canonical)
}

/// Confirm-first check for `git.commit`, matching `fs`'s `is_confirmed`
/// (the `confirmed: true` convention shared by every confirm-first
/// tool so the model's learned behavior transfers).
fn git_is_confirmed(input: &Value) -> bool {
    input.get("confirmed").and_then(|v| v.as_bool()) == Some(true)
}

/// Resolve the `repo` input field against the allow-set. Returns
/// the canonical path if the input names an allowed repo;
/// returns `None` otherwise. Canonicalization happens at build
/// time (in `GitReadToolConfig::build`), so this is a pure
/// string-compare against pre-canonicalized paths plus a
/// per-call `canonicalize` on the input to handle equivalent
/// path forms (`./foo` vs. `foo`, symlinks, etc.).
fn resolve_repo(input: &Value, repos: &[PathBuf]) -> Option<PathBuf> {
    let path_str = input.get("repo").and_then(|v| v.as_str())?;
    let input_path = Path::new(path_str);
    let canonical = std::fs::canonicalize(input_path).ok()?;
    if repos
        .iter()
        .any(|allowed| allowed.as_path() == canonical.as_path())
    {
        Some(canonical)
    } else {
        None
    }
}

/// Parse `git status --porcelain` output into a `Vec` of JSON
/// objects with `status_code` and `path` fields. The porcelain
/// format is two characters of status code, a space, then the
/// path. We tolerate the rename arrow notation (`R  old -> new`)
/// by carrying both halves in `path` as-is.
fn parse_porcelain(stdout: &str) -> Vec<Value> {
    stdout
        .lines()
        .filter_map(|line| {
            if line.len() < 4 {
                return None;
            }
            let status_code = &line[..2];
            let path = line[3..].to_string();
            Some(json!({ "status_code": status_code, "path": path }))
        })
        .collect()
}

/// Unsatisfiable scope used when the input is malformed.
/// Mirrors `fs::delete_deny_scope` shape — pin one scope the
/// agent definitely does not hold so the deny path is
/// deterministic.
fn deny_scope() -> Scope {
    Scope::parse("git.read:/__aivyx_unresolvable__")
        .expect("git.read:/__aivyx_unresolvable__ must parse — `git.read` is in KNOWN_BASES")
}

/// `git.write` counterpart to [`deny_scope`] — an unsatisfiable
/// `git.write` scope used when `git.commit`'s input is malformed, so
/// the deny is on the write base (no `git.read` grant could satisfy it
/// either).
fn write_deny_scope() -> Scope {
    Scope::parse("git.write:/__aivyx_unresolvable__")
        .expect("git.write:/__aivyx_unresolvable__ must parse — `git.write` is in KNOWN_BASES")
}

fn status_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "repo": {
                "type": "string",
                "description": "Path to a configured allowed git repo."
            }
        },
        "required": ["repo"]
    })
}

fn diff_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "repo": {
                "type": "string",
                "description": "Path to a configured allowed git repo."
            },
            "cached": {
                "type": "boolean",
                "description": "When true, show the staged diff (`git diff --cached`). \
                                Default false."
            },
            "path": {
                "type": "string",
                "description": "Optional repo-relative path to scope the diff to one \
                                file or directory. Must not start with `/` and must \
                                not contain `..`."
            }
        },
        "required": ["repo"]
    })
}

fn commit_input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "repo": {
                "type": "string",
                "description": "Path to a configured allowed git repo."
            },
            "message": {
                "type": "string",
                "description": "Commit message. Must be non-empty."
            },
            "paths": {
                "type": "array",
                "items": { "type": "string" },
                "minItems": 1,
                "description": "Repo-relative file paths to stage and commit. Each \
                                must not start with `/` and must not contain `..`."
            },
            "confirmed": {
                "type": "boolean",
                "description": "Set to `true` ONLY after the operator has approved this \
                                specific commit. Required when the operator has enabled \
                                confirm-first for destructive operations."
            }
        },
        "required": ["repo", "message", "paths"]
    })
}

// Suppress unused warning on CapabilitySet — it's pulled in by
// the `Tool` trait bound chain but not referenced directly here
// since `required_scope` returns a single `Scope` rather than a
// `CapabilitySet`. The use lives at the top of the file to match
// the convention sibling tool modules follow.
#[allow(dead_code)]
fn _unused_capability_set_ref(_cs: &CapabilitySet) {}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod git_tests {
    use super::*;
    use crate::{
        AgentId, ChannelContext, ChannelError, ChannelPlatform, MessageOrigin, NullAuditHook,
        SessionId, StreamEvent, TurnId, TurnOutcome,
    };
    use tokio_util::sync::CancellationToken;

    // Minimal ChannelContext fake so the git.commit tests can build a
    // `ToolContext` (git.commit ignores it, but `execute` requires one).
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

    #[test]
    fn parse_porcelain_handles_typical_status_lines() {
        let input = " M src/foo.rs\n?? new.txt\nA  staged.rs\n";
        let entries = parse_porcelain(input);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0]["status_code"], " M");
        assert_eq!(entries[0]["path"], "src/foo.rs");
        assert_eq!(entries[1]["status_code"], "??");
        assert_eq!(entries[1]["path"], "new.txt");
        assert_eq!(entries[2]["status_code"], "A ");
        assert_eq!(entries[2]["path"], "staged.rs");
    }

    #[test]
    fn parse_porcelain_empty_stdout_returns_empty_vec() {
        assert!(parse_porcelain("").is_empty());
    }

    #[test]
    fn parse_porcelain_handles_rename_arrow_form() {
        let input = "R  old.rs -> new.rs\n";
        let entries = parse_porcelain(input);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["status_code"], "R ");
        assert_eq!(entries[0]["path"], "old.rs -> new.rs");
    }

    #[test]
    fn parse_porcelain_skips_too_short_lines() {
        // Defense: a line that is only 1-3 chars can't carry a
        // status code + space + path. Should be skipped, not
        // panic.
        let input = "X\n  \nA  ok.rs\n";
        let entries = parse_porcelain(input);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["path"], "ok.rs");
    }

    #[test]
    fn deny_scope_parses_and_is_unsatisfiable() {
        let scope = deny_scope();
        assert_eq!(scope.base(), "git.read");
        // The qualifier is the unresolvable sentinel; a role with
        // any normal `git.read:<path>` does not grant this scope.
        let real = Scope::parse("git.read:/home/me/projects/aivyx").unwrap();
        assert!(!scope.is_granted_by(&real));
    }

    #[test]
    fn resolve_repo_returns_none_when_input_missing() {
        let allow: Vec<PathBuf> = vec![PathBuf::from("/tmp")];
        let input = json!({});
        assert!(resolve_repo(&input, &allow).is_none());
    }

    #[test]
    fn resolve_repo_returns_none_when_input_not_in_allow_set() {
        // Use a real tmp path so canonicalize succeeds but the
        // path isn't in the allow-set.
        let tmp = std::env::temp_dir();
        let allow: Vec<PathBuf> = vec![PathBuf::from("/definitely-not-a-real-path-aivyx")];
        let input = json!({ "repo": tmp.display().to_string() });
        assert!(resolve_repo(&input, &allow).is_none());
    }

    #[test]
    fn status_input_schema_requires_repo() {
        let s = status_input_schema();
        assert_eq!(s["required"][0], "repo");
    }

    #[test]
    fn diff_input_schema_requires_repo_and_allows_optional_cached_and_path() {
        let s = diff_input_schema();
        assert_eq!(s["required"][0], "repo");
        assert!(s["properties"].as_object().unwrap().contains_key("cached"));
        assert!(s["properties"].as_object().unwrap().contains_key("path"));
    }

    #[test]
    fn config_build_rejects_nonexistent_path() {
        let cfg =
            GitReadToolConfig::new(vec![PathBuf::from("/definitely-not-a-real-path-aivyx-git")]);
        let err = cfg.build().expect_err("nonexistent path must error");
        assert!(matches!(err, AivyxError::Config(_)));
    }

    #[test]
    fn config_build_rejects_path_that_is_not_a_git_repo() {
        // tmpdir is a directory but isn't a git repo.
        let tmp = std::env::temp_dir();
        let cfg = GitReadToolConfig::new(vec![tmp]);
        let err = cfg.build().expect_err("non-repo dir must error");
        match err {
            AivyxError::Config(msg) => {
                assert!(msg.contains("not a git repo"), "got: {msg}");
            }
            other => panic!("expected Config, got {other:?}"),
        }
    }

    // Note: a build()-success test requires a real git repo on
    // disk. The workspace's own root (`/home/julian/Projects/Rust/aivyx`)
    // is a git repo, but hard-coding that path would make the
    // test fragile across operator environments. The integration
    // test in Task 5 covers the success path via a real tmpdir
    // `git init` setup.

    #[test]
    fn git_status_output_is_untrusted_for_bulwark() {
        let (status_tool, _diff_tool) = GitReadToolConfig::new(Vec::<PathBuf>::new())
            .build()
            .expect("build with empty allow-set");
        assert!(status_tool.output_is_untrusted());
    }

    #[test]
    fn git_diff_output_is_untrusted_for_bulwark() {
        let (_status_tool, diff_tool) = GitReadToolConfig::new(Vec::<PathBuf>::new())
            .build()
            .expect("build with empty allow-set");
        assert!(diff_tool.output_is_untrusted());
    }

    // ---- git.commit (Chapter Forge FG.3) ----

    #[test]
    fn write_deny_scope_is_git_write_and_unsatisfiable() {
        let scope = write_deny_scope();
        assert_eq!(scope.base(), "git.write");
        // A normal git.write grant for a real repo does not grant the
        // unresolvable sentinel.
        let real = Scope::parse("git.write:/home/me/projects/aivyx").unwrap();
        assert!(!scope.is_granted_by(&real));
        // Nor does any git.read grant — different base.
        let read = Scope::parse("git.read:/home/me/projects/aivyx").unwrap();
        assert!(!scope.is_granted_by(&read));
    }

    #[test]
    fn commit_input_schema_requires_repo_message_and_paths() {
        let s = commit_input_schema();
        let required: Vec<&str> = s["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(required.contains(&"repo"));
        assert!(required.contains(&"message"));
        assert!(required.contains(&"paths"));
        assert!(
            s["properties"]
                .as_object()
                .unwrap()
                .contains_key("confirmed")
        );
    }

    #[test]
    fn write_config_rejects_non_repo_dir() {
        let tmp = std::env::temp_dir();
        let err = GitWriteToolConfig::new(vec![tmp])
            .build()
            .expect_err("non-repo must error");
        match err {
            AivyxError::Config(msg) => assert!(msg.contains("not a git repo"), "got: {msg}"),
            other => panic!("expected Config, got {other:?}"),
        }
    }

    #[test]
    fn git_commit_does_not_mutate_fs_root() {
        // git.commit checkpoints itself, per-repo, inside execute() -- it must
        // never be wired into ConcreteAgent's fs_root-scoped checkpointer hook
        // (agent.rs's dispatch loop fires that hook for any tool where this
        // returns true), since git.commit's target repo is not fs_root.
        let tool = GitWriteToolConfig::new(Vec::<PathBuf>::new())
            .build()
            .expect("build");
        assert!(!tool.mutates_fs_root());
    }

    #[test]
    fn git_commit_output_is_untrusted_for_bulwark() {
        let tool = GitWriteToolConfig::new(Vec::<PathBuf>::new())
            .build()
            .expect("build");
        assert!(tool.output_is_untrusted());
    }

    // ---- Integration: a real tmpdir git repo ----

    /// `git init` a fresh repo under a unique tmp dir with a committable
    /// identity, returning its path. Returns `None` (the test then skips) if
    /// `git` isn't on PATH **or** if any setup step can't succeed — e.g. a
    /// sandboxed/locked-down CI runner where `git init` can't write its config.
    /// A flaky environment must skip the integration coverage, never fail the
    /// suite (and so never block a release): the v0.7.1 release runner hit
    /// exactly this when an `assert!(git init …)` tripped on an env quirk
    /// unrelated to the code under test.
    fn init_temp_repo() -> Option<PathBuf> {
        use std::process::Command;
        if Command::new("git").arg("--version").output().is_err() {
            return None;
        }
        let dir = std::env::temp_dir().join(format!(
            "aivyx-git-commit-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).ok()?;
        // Skip (not fail) if any git invocation can't be spawned or returns
        // non-zero — mirrors the git-absent skip above.
        let run = |args: &[&str]| -> Option<()> {
            let ok = Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .output()
                .ok()?
                .status
                .success();
            ok.then_some(())
        };
        run(&["init"])?;
        run(&["config", "user.email", "test@aivyx.local"])?;
        run(&["config", "user.name", "Aivyx Test"])?;
        // canonicalize so it matches the allow-set form.
        std::fs::canonicalize(&dir).ok()
    }

    fn ctx_less_outcome_detail(outcome: &ToolOutcome) -> String {
        match outcome {
            ToolOutcome::Failed(AivyxError::Tool { detail, .. }) => detail.clone(),
            other => panic!("expected Failed(Tool), got {other:?}"),
        }
    }

    #[tokio::test]
    async fn commit_happy_path_stages_and_commits() {
        let Some(repo) = init_temp_repo() else { return };
        std::fs::write(repo.join("hello.txt"), b"hi\n").unwrap();
        let tool = GitWriteToolConfig::new(vec![repo.clone()])
            .build()
            .expect("build");
        let channel = fresh_channel();
        let audit = NullAuditHook;
        let ctx = make_ctx(&channel, &audit);
        let outcome = tool
            .execute(
                json!({
                    "repo": repo.display().to_string(),
                    "message": "add hello",
                    "paths": ["hello.txt"],
                }),
                &ctx,
            )
            .await;
        match outcome {
            ToolOutcome::Completed { output, .. } => {
                assert_eq!(output["committed"], true);
                assert!(
                    output["commit"].as_str().unwrap().len() >= 7,
                    "expected a hash"
                );
            }
            other => panic!("expected Completed, got {other:?}"),
        }
        // The working tree should now be clean (the file is committed).
        let st = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .arg("status")
            .arg("--porcelain")
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&st.stdout).trim().is_empty(),
            "tree not clean"
        );
        std::fs::remove_dir_all(&repo).ok();
    }

    #[tokio::test]
    async fn commit_denied_for_repo_outside_allow_set() {
        let Some(repo) = init_temp_repo() else { return };
        // Build with an EMPTY allow-set (well: a different, unrelated
        // repo would also work; empty is simplest and still valid).
        let tool = GitWriteToolConfig::new(Vec::<PathBuf>::new())
            .build()
            .expect("build");
        // required_scope must deny (sentinel) for a repo not on the list.
        let scope = tool.required_scope(&json!({ "repo": repo.display().to_string() }));
        assert_eq!(format!("{scope:?}"), format!("{:?}", write_deny_scope()));
        // And execute refuses with the allow-set message.
        let channel = fresh_channel();
        let audit = NullAuditHook;
        let ctx = make_ctx(&channel, &audit);
        let outcome = tool
            .execute(
                json!({ "repo": repo.display().to_string(), "message": "x", "paths": ["a"] }),
                &ctx,
            )
            .await;
        assert!(ctx_less_outcome_detail(&outcome).contains("not in allow-set"));
        std::fs::remove_dir_all(&repo).ok();
    }

    #[tokio::test]
    async fn commit_confirm_first_refuses_without_confirmation() {
        let Some(repo) = init_temp_repo() else { return };
        std::fs::write(repo.join("f.txt"), b"x\n").unwrap();
        let tool = GitWriteToolConfig::new(vec![repo.clone()])
            .with_confirm_destructive(true)
            .build()
            .expect("build");
        let channel = fresh_channel();
        let audit = NullAuditHook;
        let ctx = make_ctx(&channel, &audit);
        // No `confirmed` → refused.
        let refused = tool
            .execute(
                json!({ "repo": repo.display().to_string(), "message": "m", "paths": ["f.txt"] }),
                &ctx,
            )
            .await;
        assert!(ctx_less_outcome_detail(&refused).contains("without confirmation"));
        // With `confirmed: true` → commits.
        let ok = tool
            .execute(
                json!({
                    "repo": repo.display().to_string(),
                    "message": "m",
                    "paths": ["f.txt"],
                    "confirmed": true,
                }),
                &ctx,
            )
            .await;
        assert!(
            matches!(ok, ToolOutcome::Completed { .. }),
            "expected Completed, got {ok:?}"
        );
        std::fs::remove_dir_all(&repo).ok();
    }

    #[tokio::test]
    async fn git_commit_denies_a_read_outside_the_repo_via_a_malicious_hook() {
        let Some(repo) = init_temp_repo() else { return };

        // Same skip-not-fail posture as `init_temp_repo` above: a
        // sandboxed/locked-down environment where `/var/tmp` isn't
        // writable must skip this test, not panic the whole suite.
        let Ok(outside) = tempfile::Builder::new().tempdir_in("/var/tmp") else {
            std::fs::remove_dir_all(&repo).ok();
            return;
        };
        let secret = outside.path().join("secret.txt");
        std::fs::write(&secret, "top secret").unwrap();

        // Note: a plain `cat secret > repo/leaked.txt` is not a valid
        // proof of denial here — the shell creates/truncates the
        // redirect target *before* running `cat`, so `leaked.txt` would
        // exist (empty) even when the read is denied. Capture into a
        // shell variable first so the assignment's exit status carries
        // `cat`'s failure, and only write `leaked.txt` when that
        // succeeded — this way its existence is a genuine signal that
        // the hook's read of the outside file succeeded.
        let hook_path = repo.join(".git/hooks/post-commit");
        std::fs::write(
            &hook_path,
            format!(
                "#!/bin/sh\ncontent=$(cat {} 2>/dev/null) && printf '%s' \"$content\" > {}/leaked.txt\n",
                secret.display(),
                repo.display()
            ),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        std::fs::write(repo.join("a.txt"), "hello").unwrap();

        let tool = GitWriteToolConfig::new(vec![repo.clone()])
            .build()
            .expect("git.commit should build");

        let input = serde_json::json!({
            "repo": repo.display().to_string(),
            "message": "test commit",
            "paths": ["a.txt"],
        });

        let channel = fresh_channel();
        let audit = NullAuditHook;
        let ctx = make_ctx(&channel, &audit);
        let outcome = tool.execute(input, &ctx).await;

        // The commit itself is not blocked — only the hook's attempted
        // read of the outside file is. Asserting `Completed` here (rather
        // than discarding the outcome) proves the leaked.txt-absence
        // check below is a genuine confinement signal, not a vacuous pass
        // from the whole tool call having been refused for some unrelated
        // reason.
        assert!(
            matches!(outcome, ToolOutcome::Completed { .. }),
            "git.commit itself should still succeed; got {outcome:?}"
        );
        assert!(
            !repo.join("leaked.txt").exists(),
            "the post-commit hook must not be able to read a file outside the repo root"
        );
        std::fs::remove_dir_all(&repo).ok();
    }

    #[tokio::test]
    async fn commit_rejects_path_traversal_and_empty_inputs() {
        let Some(repo) = init_temp_repo() else { return };
        let tool = GitWriteToolConfig::new(vec![repo.clone()])
            .build()
            .expect("build");
        let channel = fresh_channel();
        let audit = NullAuditHook;
        let ctx = make_ctx(&channel, &audit);
        let base = repo.display().to_string();

        let traversal = tool
            .execute(
                json!({ "repo": base, "message": "m", "paths": ["../escape.txt"] }),
                &ctx,
            )
            .await;
        assert!(ctx_less_outcome_detail(&traversal).contains("must not contain"));

        let empty_msg = tool
            .execute(
                json!({ "repo": base, "message": "  ", "paths": ["a"] }),
                &ctx,
            )
            .await;
        assert!(ctx_less_outcome_detail(&empty_msg).contains("message"));

        let no_paths = tool
            .execute(json!({ "repo": base, "message": "m", "paths": [] }), &ctx)
            .await;
        assert!(ctx_less_outcome_detail(&no_paths).contains("paths"));
        std::fs::remove_dir_all(&repo).ok();
    }

    // ---- confiner_for: worktree/submodule fallback (final-review Fix 2) ---

    #[tokio::test]
    async fn confiner_for_falls_back_to_noop_when_git_is_a_file() {
        // A linked worktree or submodule's `.git` is a plain file (not a
        // directory) containing `gitdir: <real-gitdir-elsewhere>`. We
        // simulate the shape directly rather than needing a real `git
        // worktree add` fixture — `confiner_for` only inspects
        // `repo.join(".git")`'s file-vs-directory-ness, so the exact
        // gitdir target (even a dangling, nonexistent one) doesn't matter
        // for this test.
        let repo = tempfile::tempdir().unwrap();
        std::fs::write(repo.path().join(".git"), "gitdir: /some/nonexistent/path").unwrap();

        let confiner = confiner_for(repo.path(), true);

        // Behavioral proof of which variant came back: `ExecutionConfiner`
        // has no `Debug`/`PartialEq`, so confine a trivial command and
        // check an effect only `NoopConfiner` would allow. A real
        // `LandlockConfiner` confined to `repo.path()` would deny a write
        // to an unrelated `/var/tmp` directory; `NoopConfiner` never
        // touches the command at all.
        let Ok(outside) = tempfile::Builder::new().tempdir_in("/var/tmp") else {
            return;
        };
        let target = outside.path().join("proof.txt");
        let mut command = tokio::process::Command::new("sh");
        command.args(["-c", &format!("echo hi > {}", target.display())]);
        let mut command = confiner.confine(command);
        let output = command.output().await.expect("command should spawn");

        assert!(
            output.status.success(),
            "NoopConfiner must not block this write (worktree/submodule fallback \
             should skip Landlock entirely): {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(target.exists(), "the write should have actually landed");
    }

    #[tokio::test]
    async fn confiner_for_uses_the_real_confiner_for_a_normal_repo() {
        // Sanity check for the branch condition itself: an ordinary repo
        // (`.git` is a directory, the common case) must NOT take the
        // worktree/submodule fallback path — confirmed indirectly via
        // `init_temp_repo`'s real `git init`, whose `.git` is always a
        // directory.
        let Some(repo) = init_temp_repo() else { return };
        assert!(repo.join(".git").is_dir());
        // Just exercise construction — `default_confiner` itself is
        // covered by aivyx-confine's own test suite; this only confirms
        // `confiner_for` takes the non-fallback branch without panicking.
        let _confiner = confiner_for(&repo, true);
        std::fs::remove_dir_all(&repo).ok();
    }

    #[test]
    fn should_warn_once_fires_once_per_repo_then_stays_silent() {
        let repo = tempfile::tempdir().unwrap();
        assert!(
            should_warn_once(repo.path()),
            "the first call for a repo must report true (caller should warn)"
        );
        assert!(
            !should_warn_once(repo.path()),
            "a second call for the SAME repo must report false (already warned)"
        );

        // A different repo warns independently -- the latch is per-repo,
        // not a single global "only ever warn once" flag.
        let other_repo = tempfile::tempdir().unwrap();
        assert!(
            should_warn_once(other_repo.path()),
            "a different repo must warn on its own first call"
        );
    }

    // ---- git.commit checkpointing (aivyx-checkpoint git.rs/workspace.rs adoption) ----

    #[tokio::test]
    async fn commit_checkpoints_the_correct_repo_when_multiple_are_configured() {
        let Some(repo_a) = init_temp_repo() else {
            return;
        };
        let Some(repo_b) = init_temp_repo() else {
            std::fs::remove_dir_all(&repo_a).ok();
            return;
        };
        std::fs::write(repo_a.join("a.txt"), b"a\n").unwrap();

        let checkpointer_a = crate::GitCheckpointer::detect(&repo_a, Vec::new())
            .await
            .expect("repo_a is a real git repo");
        let checkpointer_b = crate::GitCheckpointer::detect(&repo_b, Vec::new())
            .await
            .expect("repo_b is a real git repo");
        let mut checkpointers = HashMap::new();
        checkpointers.insert(repo_a.clone(), Arc::new(checkpointer_a));
        checkpointers.insert(repo_b.clone(), Arc::new(checkpointer_b));

        let tool = GitWriteToolConfig::new(vec![repo_a.clone(), repo_b.clone()])
            .with_checkpointers(checkpointers)
            .build()
            .expect("build");

        let channel = fresh_channel();
        let audit = NullAuditHook;
        let ctx = make_ctx(&channel, &audit);
        let outcome = tool
            .execute(
                json!({
                    "repo": repo_a.display().to_string(),
                    "message": "add a",
                    "paths": ["a.txt"],
                }),
                &ctx,
            )
            .await;
        assert!(
            matches!(outcome, ToolOutcome::Completed { .. }),
            "expected Completed, got {outcome:?}"
        );

        let refs_a = aivyx_checkpoint::test_support::git(
            &repo_a,
            &["for-each-ref", "refs/aivyx/checkpoints/"],
        )
        .await;
        assert!(
            !refs_a.trim().is_empty(),
            "repo A must have a checkpoint ref"
        );

        let refs_b = aivyx_checkpoint::test_support::git(
            &repo_b,
            &["for-each-ref", "refs/aivyx/checkpoints/"],
        )
        .await;
        assert!(
            refs_b.trim().is_empty(),
            "repo B must NOT have any checkpoint ref"
        );

        std::fs::remove_dir_all(&repo_a).ok();
        std::fs::remove_dir_all(&repo_b).ok();
    }

    /// Genuinely discriminates the checkpoint-ordering fix: a confirm-first
    /// refusal must short-circuit *before* the checkpoint call is ever
    /// reached. Under the bug (checkpoint placed right after
    /// `resolve_repo`, before the confirm-first check), this refused,
    /// unconfirmed call would still leave a checkpoint ref behind; under the
    /// fix (checkpoint placed after the confirm-first check, immediately
    /// before `git add`), the early return means the checkpointer is never
    /// invoked at all.
    #[tokio::test]
    async fn commit_does_not_checkpoint_a_confirm_first_refused_call() {
        let Some(repo) = init_temp_repo() else { return };
        std::fs::write(repo.join("f.txt"), b"x\n").unwrap();

        let checkpointer = crate::GitCheckpointer::detect(&repo, Vec::new())
            .await
            .expect("repo is a real git repo");
        let mut checkpointers = HashMap::new();
        checkpointers.insert(repo.clone(), Arc::new(checkpointer));

        let tool = GitWriteToolConfig::new(vec![repo.clone()])
            .with_confirm_destructive(true)
            .with_checkpointers(checkpointers)
            .build()
            .expect("build");

        let channel = fresh_channel();
        let audit = NullAuditHook;
        let ctx = make_ctx(&channel, &audit);

        // No `confirmed: true` -- must be refused before ever reaching the
        // checkpoint call, which now sits after this exact refusal check.
        let outcome = tool
            .execute(
                json!({ "repo": repo.display().to_string(), "message": "m", "paths": ["f.txt"] }),
                &ctx,
            )
            .await;
        assert!(
            ctx_less_outcome_detail(&outcome).contains("without confirmation"),
            "expected the confirm-first refusal, got {outcome:?}"
        );

        let refs = aivyx_checkpoint::test_support::git(
            &repo,
            &["for-each-ref", "refs/aivyx/checkpoints/"],
        )
        .await;
        assert!(
            refs.trim().is_empty(),
            "a refused, unconfirmed commit must NOT have been checkpointed -- \
             this is the regression this test guards: the checkpoint call must \
             sit after the confirm-first check, not before it"
        );

        std::fs::remove_dir_all(&repo).ok();
    }

    #[tokio::test]
    async fn commit_skips_checkpoint_for_a_repo_with_no_entry_in_the_map() {
        let Some(repo) = init_temp_repo() else { return };
        std::fs::write(repo.join("b.txt"), b"b\n").unwrap();

        // Default GitWriteToolConfig (no `.with_checkpointers` call) --
        // the map defaults empty, so this repo has no checkpointer even
        // though it's a real git repo.
        let tool = GitWriteToolConfig::new(vec![repo.clone()])
            .build()
            .expect("build");

        let channel = fresh_channel();
        let audit = NullAuditHook;
        let ctx = make_ctx(&channel, &audit);
        let outcome = tool
            .execute(
                json!({
                    "repo": repo.display().to_string(),
                    "message": "add b",
                    "paths": ["b.txt"],
                }),
                &ctx,
            )
            .await;
        assert!(
            matches!(outcome, ToolOutcome::Completed { .. }),
            "commit must still succeed: {outcome:?}"
        );

        let refs = aivyx_checkpoint::test_support::git(
            &repo,
            &["for-each-ref", "refs/aivyx/checkpoints/"],
        )
        .await;
        assert!(
            refs.trim().is_empty(),
            "no checkpointer configured -- no checkpoint ref should exist"
        );

        std::fs::remove_dir_all(&repo).ok();
    }
}
