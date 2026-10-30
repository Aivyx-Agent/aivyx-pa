//! Phase 174 — the autonomous-loop gate runner.
//!
//! Driver-side verification that the tree is green. Phase 173
//! left gate enforcement entirely to the agent (via the
//! canonical prompt); Phase 174 makes the **driver** run the
//! operator-configured gate command (`[loop].gate_command`,
//! e.g. `"cargo test"`) before the first iteration and after
//! every iteration. A red result stops the run.
//!
//! The [`GateRunner`] trait keeps the driver's verification
//! logic testable without shelling out; the production
//! [`ShellGateRunner`] runs the command in `working_dir` via
//! `sh -c` with a wall-clock timeout.
//!
//! ## Privilege note
//!
//! The command itself is operator config (like a cron command), but
//! what it executes is the loop agent's work: `cargo test` runs the
//! `build.rs`, tests and `.cargo/config.toml` the agent just wrote.
//! So the gate does **not** run at daemon privilege. It runs under
//! the same aivyx-confine confinement as `shell.exec` (the working
//! directory writable, pa's own state denied), with the daemon's
//! environment cleared down to a small allow-list, in its own
//! process group (killed on timeout and at daemon shutdown).
//!
//! ## Scope note (Chapter Circuit, CI.3)
//!
//! This gate is a **tree-level regression guard, not a per-story
//! completion verifier**. A green gate means "the iteration did
//! not break the build/tests," not "the story was actually done":
//! the driver still trusts the agent's self-reported
//! `loop.complete` for *what* was accomplished. With no
//! `gate_command` configured (the default), there is no driver-side
//! verification at all — completion rests entirely on the agent's
//! attestation, backstopped by the CI.1 stall breaker and the
//! prompt's "drafting, not done, is not progress." A genuine
//! per-story acceptance check (an LLM judge against the story's
//! definition-of-done) is a deferred enhancement, not part of this
//! gate's contract.

use std::path::PathBuf;
use std::time::Duration;

use async_trait::async_trait;

/// The result of one gate run. Only [`GateOutcome::Passed`] is
/// "green" — every other variant stops the run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateOutcome {
    /// The gate command exited 0 — the tree is green.
    Passed,
    /// The gate command exited non-zero — the tree is red.
    Failed { code: Option<i32> },
    /// The gate command ran longer than the configured timeout
    /// and was killed — treated as red.
    TimedOut,
    /// The gate command could not be launched / waited on (e.g.
    /// the shell is missing, the working dir doesn't exist).
    /// Treated as red — better to halt than to charge ahead on
    /// an unverifiable tree.
    Errored { detail: String },
}

impl GateOutcome {
    /// Only `Passed` lets a run continue.
    pub fn is_green(&self) -> bool {
        matches!(self, GateOutcome::Passed)
    }

    /// Operator-readable one-liner for the stop reason / logs.
    pub fn label(&self) -> String {
        match self {
            GateOutcome::Passed => "gate passed".to_string(),
            GateOutcome::Failed { code: Some(c) } => {
                format!("gate failed (exit {c})")
            }
            GateOutcome::Failed { code: None } => {
                "gate failed (killed by signal)".to_string()
            }
            GateOutcome::TimedOut => "gate timed out".to_string(),
            GateOutcome::Errored { detail } => {
                format!("gate could not run: {detail}")
            }
        }
    }
}

/// Runs the loop's gate. Abstracted so the driver's
/// verification logic is unit-testable without a real process.
#[async_trait]
pub trait GateRunner: Send + Sync {
    async fn run(&self) -> GateOutcome;
}

/// Environment variables the gate keeps from the daemon's own: just enough
/// for a toolchain to find itself. Everything else (the passphrase, provider
/// keys, tokens) is dropped; the operator extends the list with
/// `[loop] gate_env` for things like `CARGO_HOME` or a proxy.
pub const GATE_SAFE_ENV: &[&str] = &["PATH", "HOME", "LANG", "TERM"];

/// Production gate runner: `sh -c "<command>"` in `working_dir`, confined
/// like `shell.exec`, with a wall-clock timeout.
///
/// The files the gate executes (`build.rs`, a test, a Makefile, a script)
/// are written by the loop's confined agent, so the gate gets the same
/// confinement the agent has: aivyx-confine (Landlock + seccomp) with
/// `working_dir` writable and pa's own state directories denied; a cleared
/// environment plus [`GATE_SAFE_ENV`] and the operator's `gate_env`; and its
/// own process group, killed on timeout and when the daemon shuts down.
pub struct ShellGateRunner {
    command: String,
    working_dir: Option<PathBuf>,
    timeout: Duration,
    env_passthrough: Vec<String>,
    confine_options: aivyx_core::ConfineOptions,
    deny_paths: Vec<PathBuf>,
}

impl ShellGateRunner {
    pub fn new(
        command: String,
        working_dir: Option<PathBuf>,
        timeout: Duration,
    ) -> Self {
        // Every instance's config / data / `~/.aivyx-pa` / runtime directory,
        // as `shell.exec`'s confiner denies them.
        let deny_paths = aivyx_instance::InstancePaths::current()
            .map(|i| i.all_instances_state_dirs())
            .unwrap_or_default();
        ShellGateRunner {
            command,
            working_dir,
            timeout,
            env_passthrough: Vec::new(),
            confine_options: aivyx_core::ConfineOptions::new(),
            deny_paths,
        }
    }

    /// Extra variables to copy from the daemon's environment (`[loop]
    /// gate_env`), beyond [`GATE_SAFE_ENV`].
    pub fn with_env_passthrough(mut self, names: Vec<String>) -> Self {
        self.env_passthrough = names;
        self
    }

    /// The operator's `[confine]` posture.
    pub fn with_confine_options(mut self, options: aivyx_core::ConfineOptions) -> Self {
        self.confine_options = options;
        self
    }

    /// Paths the confiner carves out on top of the instance state dirs.
    pub fn with_extra_deny_paths(mut self, paths: Vec<PathBuf>) -> Self {
        self.deny_paths.extend(paths);
        self
    }
}

#[async_trait]
impl GateRunner for ShellGateRunner {
    async fn run(&self) -> GateOutcome {
        // The confined folder is the gate's working directory; without one
        // there is nothing safe to grant write access to.
        let Some(dir) = &self.working_dir else {
            return GateOutcome::Errored {
                detail: "the gate runs confined to a folder: set `[loop] working_dir`".into(),
            };
        };
        let canonical = match std::fs::canonicalize(dir) {
            Ok(c) if c.is_dir() => c,
            Ok(c) => {
                return GateOutcome::Errored {
                    detail: format!("working_dir {} is not a directory", c.display()),
                };
            }
            Err(e) => {
                return GateOutcome::Errored {
                    detail: format!("working_dir {}: {e}", dir.display()),
                };
            }
        };
        // A state folder that CONTAINS the working directory (the default
        // workspace lives under `~/.aivyx-pa`) can't be denied without
        // denying the gate its own folder; the confiner grants write only to
        // the working directory, so the rest of it stays unwritable anyway.
        let deny: Vec<PathBuf> = self
            .deny_paths
            .iter()
            .filter(|d| !canonical.starts_with(d))
            .cloned()
            .collect();
        let confiner = aivyx_core::default_confiner_with_options(
            &canonical,
            &[],
            &deny,
            self.confine_options.clone(),
        );

        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c").arg(&self.command);
        cmd.current_dir(&canonical);
        cmd.stdin(std::process::Stdio::null());
        // Not the daemon's environment: it holds the passphrase and every
        // provider key.
        cmd.env_clear();
        for name in GATE_SAFE_ENV
            .iter()
            .copied()
            .chain(self.env_passthrough.iter().map(String::as_str))
        {
            if let Ok(val) = std::env::var(name) {
                cmd.env(name, val);
            }
        }
        let cmd = confiner.confine(cmd);

        match aivyx_core::run_command_in_group(cmd, self.timeout).await {
            Ok(aivyx_core::CommandRun::Finished(output)) => {
                if output.status.success() {
                    GateOutcome::Passed
                } else {
                    // The gate's output used to land in the daemon log by
                    // inheritance; keep the tail of it there.
                    log_tail("stdout", &output.stdout);
                    log_tail("stderr", &output.stderr);
                    GateOutcome::Failed { code: output.status.code() }
                }
            }
            Ok(aivyx_core::CommandRun::TimedOut) => GateOutcome::TimedOut,
            Err(detail) => GateOutcome::Errored { detail },
        }
    }
}

fn log_tail(label: &str, bytes: &[u8]) {
    const MAX: usize = 4000;
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim_end();
    if text.is_empty() {
        return;
    }
    let start = text.len().saturating_sub(MAX);
    let start = (start..text.len()).find(|i| text.is_char_boundary(*i)).unwrap_or(0);
    eprintln!("aivyx-pa loop gate {label}: {}", &text[start..]);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh folder for a gate to run in (the gate is confined to it).
    fn scratch_dir() -> PathBuf {
        let base = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into());
        let dir = PathBuf::from(base).join(format!("aivyx-gate-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn only_passed_is_green() {
        assert!(GateOutcome::Passed.is_green());
        assert!(!GateOutcome::Failed { code: Some(1) }.is_green());
        assert!(!GateOutcome::TimedOut.is_green());
        assert!(!GateOutcome::Errored {
            detail: "x".into()
        }
        .is_green());
    }

    #[test]
    fn labels_are_distinct() {
        assert!(GateOutcome::Passed.label().contains("passed"));
        assert!(GateOutcome::Failed { code: Some(2) }
            .label()
            .contains("exit 2"));
        assert!(GateOutcome::TimedOut.label().contains("timed out"));
        assert!(GateOutcome::Errored {
            detail: "boom".into()
        }
        .label()
        .contains("boom"));
    }

    #[tokio::test]
    async fn shell_true_passes() {
        let r = ShellGateRunner::new(
            "true".into(),
            Some(scratch_dir()),
            Duration::from_secs(10),
        );
        assert_eq!(r.run().await, GateOutcome::Passed);
    }

    #[tokio::test]
    async fn shell_false_fails() {
        let r = ShellGateRunner::new(
            "false".into(),
            Some(scratch_dir()),
            Duration::from_secs(10),
        );
        assert!(matches!(r.run().await, GateOutcome::Failed { .. }));
    }

    #[tokio::test]
    async fn shell_nonzero_exit_code_is_captured() {
        let r = ShellGateRunner::new(
            "exit 7".into(),
            Some(scratch_dir()),
            Duration::from_secs(10),
        );
        assert_eq!(
            r.run().await,
            GateOutcome::Failed { code: Some(7) }
        );
    }

    #[tokio::test]
    async fn shell_timeout_kills_and_reports() {
        let r = ShellGateRunner::new(
            "sleep 30".into(),
            Some(scratch_dir()),
            Duration::from_millis(150),
        );
        assert_eq!(r.run().await, GateOutcome::TimedOut);
    }

    #[tokio::test]
    async fn working_dir_is_where_the_gate_runs() {
        // The gate runs in `working_dir`: a command that only
        // passes when run inside a temp dir (checks for a marker
        // file we plant there) is green only with the dir set.
        let base = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into());
        let dir = std::path::PathBuf::from(base)
            .join(format!("aivyx-gate-cwd-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("MARKER"), b"x").unwrap();

        let in_dir = ShellGateRunner::new(
            "test -f MARKER".into(),
            Some(dir.clone()),
            Duration::from_secs(10),
        );
        assert_eq!(in_dir.run().await, GateOutcome::Passed);

        // Without the working dir, the marker isn't found → red.
        let no_dir = ShellGateRunner::new(
            "test -f MARKER".into(),
            Some(std::env::temp_dir()),
            Duration::from_secs(10),
        );
        assert!(matches!(
            no_dir.run().await,
            GateOutcome::Failed { .. }
        ));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn shell_features_work_in_gate_command() {
        // The gate runs under `sh -c`, so shell operators work.
        let r = ShellGateRunner::new(
            "test 1 -eq 1 && echo ok".into(),
            Some(scratch_dir()),
            Duration::from_secs(10),
        );
        assert_eq!(r.run().await, GateOutcome::Passed);
    }

    #[tokio::test]
    async fn bad_working_dir_errors() {
        let r = ShellGateRunner::new(
            "true".into(),
            Some(PathBuf::from("/no/such/dir/aivyx-xyz")),
            Duration::from_secs(5),
        );
        // sh either fails to spawn (current_dir missing) or sh
        // runs and errors — both are non-green.
        assert!(!r.run().await.is_green());
    }

    #[tokio::test]
    async fn a_gate_without_a_working_dir_does_not_run() {
        let r = ShellGateRunner::new("true".into(), None, Duration::from_secs(5));
        assert!(matches!(r.run().await, GateOutcome::Errored { .. }));
    }

    /// Audit 2026-10-28 (P2): the gate executes what the confined loop
    /// agent wrote, so it must not see the daemon's environment.
    #[tokio::test]
    async fn the_gate_sees_a_cleared_environment_not_the_daemons_secrets() {
        let dir = scratch_dir();
        let out = dir.join("env.txt");
        // SAFETY: the only test in this binary touching these variables.
        unsafe {
            std::env::set_var("AIVYX_PA_PASSPHRASE", "daemon-master-passphrase-canary");
            std::env::set_var("ANTHROPIC_API_KEY", "provider-key-canary");
            std::env::set_var("AIVYX_GATE_EXTRA", "operator-allowed");
        }
        let r = ShellGateRunner::new(
            format!("env > {}", out.display()),
            Some(dir.clone()),
            Duration::from_secs(10),
        )
        .with_env_passthrough(vec!["AIVYX_GATE_EXTRA".into()]);
        assert_eq!(r.run().await, GateOutcome::Passed);
        let env = std::fs::read_to_string(&out).unwrap();
        assert!(!env.contains("canary"), "the gate saw daemon secrets:\n{env}");
        assert!(!env.contains("AIVYX_PA_PASSPHRASE") && !env.contains("ANTHROPIC_API_KEY"));
        assert!(env.contains("PATH="), "the safe allow-list is kept:\n{env}");
        assert!(
            env.contains("AIVYX_GATE_EXTRA=operator-allowed"),
            "the operator's gate_env is passed through:\n{env}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Audit 2026-10-28 (P2): a gate that writes outside its folder is
    /// refused by the confiner (the folder is the only writable place).
    #[tokio::test]
    async fn the_gate_cannot_write_outside_its_folder() {
        let dir = scratch_dir();
        let outside = scratch_dir();
        let target = outside.join("planted.txt");
        let r = ShellGateRunner::new(
            format!("echo hi > {}", target.display()),
            Some(dir.clone()),
            Duration::from_secs(10),
        );
        assert!(
            matches!(r.run().await, GateOutcome::Failed { .. }),
            "a write outside the folder must fail the gate"
        );
        assert!(!target.exists(), "the confined gate wrote outside its folder");
        // Inside the folder it can write (a build needs `target/`).
        let inside = ShellGateRunner::new(
            "echo hi > built.txt".into(),
            Some(dir.clone()),
            Duration::from_secs(10),
        );
        assert_eq!(inside.run().await, GateOutcome::Passed);
        assert!(dir.join("built.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
    }

    /// The gate's whole process group dies on timeout, background jobs too.
    #[tokio::test]
    async fn timeout_kills_the_gates_background_children() {
        let dir = scratch_dir();
        let marker = dir.join("late.txt");
        let r = ShellGateRunner::new(
            format!("(sleep 3; echo late > {}) & sleep 30", marker.display()),
            Some(dir.clone()),
            Duration::from_millis(300),
        );
        assert_eq!(r.run().await, GateOutcome::TimedOut);
        tokio::time::sleep(Duration::from_secs(4)).await;
        assert!(!marker.exists(), "a background child outlived the gate's timeout");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
