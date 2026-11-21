//! A22 — a coder run: `aivyx-coder --auto <brief> --headless --report
//! <tmp>/report.json` as a child process in a registered workspace, its
//! output capped, its report parsed, its exit code mapped to a run outcome.
//! Cancel and the ceiling send SIGTERM, then kill after a grace period.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use aivyx_core::CancellationToken;
use aivyx_ipc::board::RunOutcome;
use serde::Deserialize;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};

/// A coder run's hard ceiling.
pub const CEILING: Duration = Duration::from_secs(4 * 3600);
/// How long a SIGTERMed coder gets to write its report before a kill.
pub const GRACE: Duration = Duration::from_secs(30);
/// How much of the child's stdout and stderr is kept (the last bytes).
pub const TAIL_CAP: usize = 64 * 1024;
/// A report bigger than this is treated as unreadable.
const REPORT_CAP: u64 = 4 * 1024 * 1024;
/// How many of stderr's last lines an exit-3 note quotes.
const NOTE_LINES: usize = 5;
/// How long the output drains get after the child exits (a grandchild may
/// still hold a pipe open).
const DRAIN_WAIT: Duration = Duration::from_secs(2);

/// The coder binary: `AIVYX_CODER` when set and non-empty, else
/// `aivyx-coder` on `PATH`.
pub fn find_coder() -> Option<PathBuf> {
    find_coder_in(std::env::var_os("AIVYX_CODER"), std::env::var_os("PATH"))
}

fn find_coder_in(env: Option<OsString>, path: Option<OsString>) -> Option<PathBuf> {
    if let Some(bin) = env.filter(|b| !b.is_empty()) {
        return Some(PathBuf::from(bin));
    }
    std::env::split_paths(&path?)
        .filter(|d| !d.as_os_str().is_empty())
        .map(|d| d.join("aivyx-coder"))
        .find(|p| is_executable(p))
}

fn is_executable(p: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(p) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        meta.is_file()
    }
}

/// One coder run.
#[derive(Debug, Clone)]
pub struct CoderRunSpec {
    pub bin: PathBuf,
    /// The workspace's canonical path: the child's current directory.
    pub workspace: PathBuf,
    pub brief: String,
    pub ceiling: Duration,
    pub grace: Duration,
}

/// What coder's `--report` file says (the fields pa uses).
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct CoderReport {
    pub outcome: String,
    pub notice: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub checkpoints: u32,
    #[serde(deserialize_with = "count_files")]
    pub changed_files: usize,
}

/// `changed_files` is an array of objects in the report; pa only counts it.
fn count_files<'de, D: serde::Deserializer<'de>>(d: D) -> Result<usize, D::Error> {
    let v = Vec::<serde::de::IgnoredAny>::deserialize(d)?;
    Ok(v.len())
}

/// How a coder run ended.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CoderOutcome {
    /// The exit code; `None` when it died by a signal or never ran.
    pub exit: Option<i32>,
    pub signalled: bool,
    /// The ceiling passed and pa stopped it.
    pub timed_out: bool,
    /// The run's token was cancelled and pa stopped it.
    pub cancelled: bool,
    pub report: Option<CoderReport>,
    /// The last [`TAIL_CAP`] bytes of stderr.
    pub tail: String,
    /// It couldn't be started (or its report folder couldn't be made).
    pub spawn_error: Option<String>,
}

/// `(outcome, answer, tokens)` for a run's thread entry, by the exit-code
/// table in A22.
pub fn to_run_result(o: &CoderOutcome) -> (RunOutcome, String, u64) {
    let tokens = o
        .report
        .as_ref()
        .map_or(0, |r| r.prompt_tokens.saturating_add(r.completion_tokens));
    let failed = |note: String| {
        let answer = with_counts(&note, o.report.as_ref());
        (RunOutcome::Failed { reason: note }, answer, tokens)
    };
    if let Some(e) = &o.spawn_error {
        return failed(format!("couldn't start aivyx-coder: {e}"));
    }
    if o.cancelled {
        return (RunOutcome::Cancelled, String::new(), tokens);
    }
    if o.timed_out {
        return failed("coder ran past the run ceiling and was stopped".into());
    }
    let missing = if o.report.is_none() {
        " (coder wrote no report)"
    } else {
        ""
    };
    let notice = o
        .report
        .as_ref()
        .map(|r| r.notice.trim())
        .filter(|n| !n.is_empty())
        .unwrap_or("coder finished");
    match o.exit {
        Some(0) => {
            let note = format!("{notice}{missing}");
            (
                RunOutcome::Succeeded,
                with_counts(&note, o.report.as_ref()),
                tokens,
            )
        }
        Some(1) => {
            let note = format!("{notice} — a check failed{missing}");
            (
                RunOutcome::Succeeded,
                with_counts(&note, o.report.as_ref()),
                tokens,
            )
        }
        Some(2) => failed(format!("coder's budget ran out{missing}")),
        Some(3) => {
            let lines = last_lines(&o.tail, NOTE_LINES);
            if lines.is_empty() {
                failed(format!("coder failed{missing}"))
            } else {
                failed(format!("coder failed: {lines}{missing}"))
            }
        }
        Some(4) => failed(format!("coder stopped: possible prompt injection{missing}")),
        Some(130) => (RunOutcome::Cancelled, String::new(), tokens),
        Some(code) => failed(format!("coder exited unexpectedly ({code}){missing}")),
        None => failed(format!(
            "coder exited unexpectedly (killed by a signal){missing}"
        )),
    }
}

/// `note` plus the changed-file and checkpoint counts, when there is a report.
fn with_counts(note: &str, report: Option<&CoderReport>) -> String {
    match report {
        Some(r) => format!(
            "{note}\n\n{} changed file{}; {} checkpoint{} — undo with /undo in aivyx-coder",
            r.changed_files,
            if r.changed_files == 1 { "" } else { "s" },
            r.checkpoints,
            if r.checkpoints == 1 { "" } else { "s" },
        ),
        None => note.to_string(),
    }
}

/// The last `n` non-empty lines of `text`, joined with " / ".
fn last_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    lines[lines.len().saturating_sub(n)..].join(" / ")
}

/// A per-run folder for the report, removed on drop.
struct RunDir(PathBuf);

impl RunDir {
    fn new() -> std::io::Result<Self> {
        let dir = std::env::temp_dir().join(format!(
            "aivyx-pa-coder-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4().simple()
        ));
        let mut b = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            b.mode(0o700);
        }
        b.create(&dir)?;
        Ok(RunDir(dir))
    }
}

impl Drop for RunDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Keep the last [`TAIL_CAP`] bytes `r` produces.
async fn drain(r: Option<impl AsyncRead + Unpin>) -> Vec<u8> {
    let mut out = Vec::new();
    let Some(mut r) = r else {
        return out;
    };
    let mut buf = [0u8; 8192];
    loop {
        match r.read(&mut buf).await {
            Ok(0) | Err(_) => return out,
            Ok(n) => {
                out.extend_from_slice(&buf[..n]);
                if out.len() > TAIL_CAP {
                    let cut = out.len() - TAIL_CAP;
                    out.drain(..cut);
                }
            }
        }
    }
}

/// SIGTERM, then a kill after `grace`. Returns the exit status, if any.
async fn stop(child: &mut Child, grace: Duration) -> Option<std::process::ExitStatus> {
    #[cfg(unix)]
    if let Some(pid) = child.id().and_then(|p| i32::try_from(p).ok()) {
        // SAFETY: `pid` is our own child, not yet reaped (`id()` is `Some`
        // only until `wait` collects it), so it can't name another process.
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
    }
    #[cfg(not(unix))]
    let _ = child.start_kill();
    match tokio::time::timeout(grace, child.wait()).await {
        Ok(s) => s.ok(),
        Err(_) => {
            let _ = child.start_kill();
            child.wait().await.ok()
        }
    }
}

/// Well-known cloud-provider credential variables. pa and aivyx-coder are
/// local-only (local LLMs), so pa never hands a cloud credential to a worker.
const CLOUD_CREDENTIAL_VARS: &[&str] = &[
    "OPENAI_API_KEY",
    "OPENAI_ORG_ID",
    "ANTHROPIC_API_KEY",
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
    "MISTRAL_API_KEY",
    "GROQ_API_KEY",
    "OPENROUTER_API_KEY",
    "DEEPSEEK_API_KEY",
    "XAI_API_KEY",
    "COHERE_API_KEY",
    "TOGETHER_API_KEY",
    "FIREWORKS_API_KEY",
    "PERPLEXITY_API_KEY",
    "AZURE_OPENAI_API_KEY",
    "AWS_BEARER_TOKEN_BEDROCK",
];

/// pa's own secrets (passphrase, API and bot tokens: `AIVYX_PA_*`) and
/// cloud-provider credentials stay with pa.
fn strip_pa_env(cmd: &mut Command, names: impl Iterator<Item = OsString>) {
    for k in names {
        if k.to_str()
            .is_some_and(|k| k.starts_with("AIVYX_PA_") || CLOUD_CREDENTIAL_VARS.contains(&k))
        {
            cmd.env_remove(k);
        }
    }
}

enum Ended {
    Exited(std::io::Result<std::process::ExitStatus>),
    Cancelled,
    Ceiling,
}

/// Run coder once and report how it ended. Never fails: a spawn error is
/// part of the outcome.
pub async fn run(spec: CoderRunSpec, cancel: CancellationToken) -> CoderOutcome {
    let dir = match RunDir::new() {
        Ok(d) => d,
        Err(e) => {
            return CoderOutcome {
                spawn_error: Some(format!("couldn't make the report folder: {e}")),
                ..CoderOutcome::default()
            };
        }
    };
    let report_path = dir.0.join("report.json");
    let mut cmd = Command::new(&spec.bin);
    cmd.arg("--auto")
        .arg(&spec.brief)
        .arg("--headless")
        .arg("--report")
        .arg(&report_path)
        .current_dir(&spec.workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    strip_pa_env(&mut cmd, std::env::vars_os().map(|(k, _)| k));
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return CoderOutcome {
                spawn_error: Some(format!("{}: {e}", spec.bin.display())),
                ..CoderOutcome::default()
            };
        }
    };
    let out = tokio::spawn(drain(child.stdout.take()));
    let err = tokio::spawn(drain(child.stderr.take()));

    let ended = tokio::select! {
        s = child.wait() => Ended::Exited(s),
        _ = cancel.cancelled() => Ended::Cancelled,
        _ = tokio::time::sleep(spec.ceiling) => Ended::Ceiling,
    };
    let mut outcome = CoderOutcome::default();
    let status = match ended {
        Ended::Exited(s) => s.ok(),
        Ended::Cancelled => {
            outcome.cancelled = true;
            stop(&mut child, spec.grace).await
        }
        Ended::Ceiling => {
            outcome.timed_out = true;
            stop(&mut child, spec.grace).await
        }
    };
    match status.and_then(|s| s.code()) {
        Some(code) => outcome.exit = Some(code),
        None => outcome.signalled = true,
    }

    // stdout is drained so coder never blocks on a full pipe; only stderr
    // is kept for the note.
    let abort_out = out.abort_handle();
    let abort_err = err.abort_handle();
    let _ = tokio::time::timeout(DRAIN_WAIT, out).await;
    abort_out.abort();
    if let Ok(Ok(bytes)) = tokio::time::timeout(DRAIN_WAIT, err).await {
        outcome.tail = String::from_utf8_lossy(&bytes).into_owned();
    }
    abort_err.abort();

    outcome.report = read_report(&report_path);
    drop(dir);
    outcome
}

fn read_report(path: &Path) -> Option<CoderReport> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > REPORT_CAP {
        return None;
    }
    let text = std::fs::read(path).ok()?;
    serde_json::from_slice(&text).ok()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn report(notice: &str) -> CoderReport {
        CoderReport {
            outcome: "goal_achieved".into(),
            notice: notice.into(),
            prompt_tokens: 100,
            completion_tokens: 23,
            checkpoints: 4,
            changed_files: 2,
        }
    }

    fn exited(code: i32, report: Option<CoderReport>) -> CoderOutcome {
        CoderOutcome {
            exit: Some(code),
            report,
            ..CoderOutcome::default()
        }
    }

    fn reason(r: &(RunOutcome, String, u64)) -> String {
        match &r.0 {
            RunOutcome::Failed { reason } => reason.clone(),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn exit_0_succeeds_with_the_notice_and_the_counts() {
        let r = to_run_result(&exited(0, Some(report("Added parse() and its test."))));
        assert_eq!(r.0, RunOutcome::Succeeded);
        assert!(r.1.contains("Added parse() and its test."), "{}", r.1);
        assert!(r.1.contains("2 changed files"), "{}", r.1);
        assert!(r.1.contains("4 checkpoints"), "{}", r.1);
        assert!(r.1.contains("undo with /undo"), "{}", r.1);
        assert_eq!(r.2, 123);
    }

    #[test]
    fn exit_1_succeeds_with_a_failed_check() {
        let r = to_run_result(&exited(1, Some(report("Done."))));
        assert_eq!(r.0, RunOutcome::Succeeded);
        assert!(r.1.starts_with("Done. — a check failed"), "{}", r.1);
        assert_eq!(r.2, 123);
    }

    #[test]
    fn exits_2_3_4_and_others_go_back_to_todo() {
        let r = to_run_result(&exited(2, Some(report("x"))));
        assert_eq!(reason(&r), "coder's budget ran out");
        assert_eq!(r.2, 123);
        assert!(r.1.contains("4 checkpoints"), "{}", r.1);

        let mut o = exited(3, None);
        o.tail = "warming up\n\nerror: backend refused\nbye\n".into();
        let r = to_run_result(&o);
        assert_eq!(
            reason(&r),
            "coder failed: warming up / error: backend refused / bye (coder wrote no report)"
        );
        assert_eq!(r.2, 0);

        let r = to_run_result(&exited(4, Some(report("x"))));
        assert_eq!(reason(&r), "coder stopped: possible prompt injection");

        let r = to_run_result(&exited(7, Some(report("x"))));
        assert_eq!(reason(&r), "coder exited unexpectedly (7)");
    }

    #[test]
    fn exit_130_and_a_cancel_are_cancelled() {
        assert_eq!(to_run_result(&exited(130, None)).0, RunOutcome::Cancelled);
        let o = CoderOutcome {
            exit: Some(0),
            cancelled: true,
            report: Some(report("x")),
            ..CoderOutcome::default()
        };
        let r = to_run_result(&o);
        assert_eq!(r.0, RunOutcome::Cancelled);
        assert_eq!(r.2, 123);
    }

    #[test]
    fn a_signal_a_timeout_and_a_spawn_error_go_back_to_todo() {
        let o = CoderOutcome {
            signalled: true,
            ..CoderOutcome::default()
        };
        assert!(reason(&to_run_result(&o)).contains("unexpectedly (killed by a signal)"));

        // A timeout wins over the 130 coder exits with on SIGTERM.
        let o = CoderOutcome {
            exit: Some(130),
            timed_out: true,
            ..CoderOutcome::default()
        };
        assert!(reason(&to_run_result(&o)).contains("run ceiling"));

        let o = CoderOutcome {
            spawn_error: Some("/x/aivyx-coder: No such file".into()),
            ..CoderOutcome::default()
        };
        let r = to_run_result(&o);
        assert!(reason(&r).contains("No such file"), "{}", reason(&r));
        assert_eq!(r.2, 0);
    }

    #[test]
    fn a_missing_report_is_noted_and_the_exit_code_decides() {
        let r = to_run_result(&exited(0, None));
        assert_eq!(r.0, RunOutcome::Succeeded);
        assert!(r.1.contains("coder wrote no report"), "{}", r.1);
        assert_eq!(r.2, 0);
        let r = to_run_result(&exited(3, None));
        assert_eq!(reason(&r), "coder failed (coder wrote no report)");
    }

    #[test]
    fn the_report_parses_leniently() {
        let r: CoderReport = serde_json::from_str(
            r#"{"report_version":2,"outcome":"goal_achieved","notice":"ok",
                "prompt_tokens":5,"completion_tokens":6,"checkpoints":3,
                "changed_files":[{"path":"a.rs"},{"path":"b.rs"}],"extra":true}"#,
        )
        .unwrap();
        assert_eq!(r.changed_files, 2);
        assert_eq!(r.checkpoints, 3);
        let r: CoderReport = serde_json::from_str(r#"{"notice":"only"}"#).unwrap();
        assert_eq!(r.notice, "only");
        assert_eq!(r.changed_files, 0);
    }

    #[cfg(unix)]
    #[test]
    fn find_coder_prefers_the_env_then_path() {
        let d = tempfile::tempdir().unwrap();
        assert_eq!(
            find_coder_in(Some("/opt/coder".into()), None),
            Some(PathBuf::from("/opt/coder"))
        );
        assert_eq!(find_coder_in(Some("".into()), Some(d.path().into())), None);
        let bin = d.path().join("aivyx-coder");
        std::fs::write(&bin, "#!/bin/sh\n").unwrap();
        // Not executable yet.
        assert_eq!(find_coder_in(None, Some(d.path().into())), None);
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = std::env::join_paths([PathBuf::from("/nonexistent"), d.path().into()]).unwrap();
        assert_eq!(find_coder_in(None, Some(path)), Some(bin));
    }

    // ---- the process driver, with fake coders ----

    /// A fake coder: `body` runs after `$REPORT` is set from `--report`; `$@`
    /// is left as given.
    #[cfg(unix)]
    pub(crate) fn fake_coder(dir: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(name);
        let script = format!(
            "#!/bin/sh\nREPORT=\nprev=\nfor a in \"$@\"; do\n  if [ \"$prev\" = --report ]; then REPORT=\"$a\"; fi\n  prev=\"$a\"\ndone\n{body}\n"
        );
        std::fs::write(&p, script).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[cfg(unix)]
    fn spec(bin: PathBuf, ws: &Path) -> CoderRunSpec {
        CoderRunSpec {
            bin,
            workspace: ws.to_path_buf(),
            brief: "Work this task".into(),
            ceiling: Duration::from_secs(20),
            grace: Duration::from_millis(500),
        }
    }

    #[cfg(unix)]
    const REPORT_JSON: &str = r#"{"outcome":"goal_achieved","notice":"all done","prompt_tokens":10,"completion_tokens":5,"checkpoints":2,"changed_files":[{"path":"a"}]}"#;

    #[cfg(unix)]
    #[tokio::test]
    async fn exit_0_with_a_report_runs_in_the_workspace() {
        let d = tempfile::tempdir().unwrap();
        let ws = d.path().join("ws");
        std::fs::create_dir(&ws).unwrap();
        let bin = fake_coder(
            d.path(),
            "ok",
            &format!("pwd > cwd.txt\ncat > /dev/null\necho '{REPORT_JSON}' > \"$REPORT\"\nexit 0"),
        );
        let o = run(spec(bin, &ws), CancellationToken::new()).await;
        assert_eq!(o.exit, Some(0), "{o:?}");
        assert!(!o.signalled && !o.cancelled && !o.timed_out);
        let r = o.report.expect("a report");
        assert_eq!(r.notice, "all done");
        assert_eq!(r.changed_files, 1);
        let cwd = std::fs::read_to_string(ws.join("cwd.txt")).unwrap();
        assert_eq!(cwd.trim(), ws.canonicalize().unwrap().to_str().unwrap());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exit_3_keeps_the_stderr_tail() {
        let d = tempfile::tempdir().unwrap();
        let bin = fake_coder(
            d.path(),
            "fail",
            "echo 'on stdout'\necho 'error: backend refused' >&2\nexit 3",
        );
        let o = run(spec(bin, d.path()), CancellationToken::new()).await;
        assert_eq!(o.exit, Some(3));
        assert!(o.report.is_none());
        assert_eq!(o.tail.trim(), "error: backend refused");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_tail_is_capped() {
        let d = tempfile::tempdir().unwrap();
        // ~200 KiB of stderr, then a last line.
        let bin = fake_coder(
            d.path(),
            "loud",
            "i=0\nwhile [ $i -lt 2000 ]; do\n  printf '%0100d\\n' 0 >&2\n  i=$((i+1))\ndone\necho LAST >&2\nexit 3",
        );
        let o = run(spec(bin, d.path()), CancellationToken::new()).await;
        assert_eq!(o.exit, Some(3));
        assert!(o.tail.len() <= TAIL_CAP, "{}", o.tail.len());
        assert!(o.tail.trim_end().ends_with("LAST"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exit_0_without_a_report() {
        let d = tempfile::tempdir().unwrap();
        let bin = fake_coder(d.path(), "silent", "exit 0");
        let o = run(spec(bin, d.path()), CancellationToken::new()).await;
        assert_eq!(o.exit, Some(0));
        assert!(o.report.is_none());
    }

    /// Polls (bounded) until a trapping fake coder has installed its trap;
    /// it touches `trap-ready` in its working directory right after.
    pub(crate) async fn wait_trap_ready(dir: &Path) {
        let f = dir.join("trap-ready");
        for _ in 0..500 {
            if f.exists() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("the fake coder never installed its trap");
    }

    /// Loops in short sleeps (so no long-lived grandchild is left behind);
    /// on TERM writes a report and exits 130.
    #[cfg(unix)]
    pub(crate) const TRAPS_TERM: &str = "trap 'echo \"{\\\"notice\\\":\\\"stopped\\\"}\" > \"$REPORT\"; exit 130' TERM\ntouch trap-ready\ni=0\nwhile [ $i -lt 300 ]; do sleep 0.1; i=$((i+1)); done\nexit 0";

    #[cfg(unix)]
    #[tokio::test]
    async fn cancel_sends_sigterm_and_the_report_is_still_read() {
        let d = tempfile::tempdir().unwrap();
        let bin = fake_coder(d.path(), "hold", TRAPS_TERM);
        let cancel = CancellationToken::new();
        let c = cancel.clone();
        let dir = d.path().to_path_buf();
        tokio::spawn(async move {
            wait_trap_ready(&dir).await;
            c.cancel();
        });
        let started = std::time::Instant::now();
        let o = run(spec(bin, d.path()), cancel).await;
        assert!(o.cancelled);
        assert_eq!(o.exit, Some(130), "{o:?}");
        assert!(!o.signalled);
        assert_eq!(o.report.unwrap().notice, "stopped");
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_coder_ignoring_sigterm_is_killed_after_the_grace() {
        let d = tempfile::tempdir().unwrap();
        // `exec`, so the ignoring process is the child itself and the kill
        // reaps it (an ignored signal stays ignored across exec).
        let bin = fake_coder(d.path(), "stubborn", "trap '' TERM\nexec sleep 30");
        let cancel = CancellationToken::new();
        let c = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            c.cancel();
        });
        let started = std::time::Instant::now();
        let o = run(spec(bin, d.path()), cancel).await;
        assert!(o.cancelled);
        assert!(o.signalled, "{o:?}");
        assert_eq!(o.exit, None);
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_ceiling_stops_a_hung_coder() {
        let d = tempfile::tempdir().unwrap();
        let bin = fake_coder(d.path(), "hung", TRAPS_TERM);
        let mut s = spec(bin, d.path());
        // `run` has no readiness hook, so the ceiling is made generous
        // against a slow shell start instead (the trap installs in ms).
        s.ceiling = Duration::from_millis(1500);
        let o = run(s, CancellationToken::new()).await;
        assert!(o.timed_out && !o.cancelled);
        assert_eq!(o.exit, Some(130));
        assert!(matches!(to_run_result(&o).0, RunOutcome::Failed { .. }));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_missing_binary_is_a_spawn_error() {
        let d = tempfile::tempdir().unwrap();
        let o = run(
            spec(d.path().join("no-such-coder"), d.path()),
            CancellationToken::new(),
        )
        .await;
        assert!(o.spawn_error.is_some());
        assert_eq!(o.exit, None);
    }

    #[test]
    fn pa_env_is_stripped_and_nothing_else() {
        let mut cmd = Command::new("x");
        strip_pa_env(
            &mut cmd,
            [
                "AIVYX_PA_PASSPHRASE",
                "AIVYX_CODER",
                "HOME",
                "AIVYX_PA_OPENAI_API_KEY",
            ]
            .into_iter()
            .map(OsString::from),
        );
        let mut removed: Vec<_> = cmd
            .as_std()
            .get_envs()
            .map(|(k, v)| (k.to_str().unwrap().to_string(), v.is_none()))
            .collect();
        removed.sort();
        assert_eq!(
            removed,
            [
                ("AIVYX_PA_OPENAI_API_KEY".to_string(), true),
                ("AIVYX_PA_PASSPHRASE".to_string(), true)
            ]
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_child_sees_no_pa_or_cloud_secrets() {
        // SAFETY: test-only; the names are unique to this test or canaries.
        unsafe {
            std::env::set_var("AIVYX_PA_ENVTEST_SECRET", "x");
            std::env::set_var("OPENAI_API_KEY", "x");
            std::env::set_var("AIVYX_ENVTEST_ORDINARY", "visible");
        }
        let d = tempfile::tempdir().unwrap();
        let bin = fake_coder(d.path(), "envdump", "env > env.txt\nexit 0");
        let o = run(spec(bin, d.path()), CancellationToken::new()).await;
        assert_eq!(o.exit, Some(0));
        let env = std::fs::read_to_string(d.path().join("env.txt")).unwrap();
        assert!(!env.contains("AIVYX_PA_"), "{env}");
        assert!(!env.contains("OPENAI_API_KEY"), "{env}");
        assert!(env.contains("AIVYX_ENVTEST_ORDINARY=visible"), "{env}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_report_dir_is_removed_after_the_run() {
        let d = tempfile::tempdir().unwrap();
        let bin = fake_coder(
            d.path(),
            "where",
            "echo \"$REPORT\" > report-path.txt\nexit 0",
        );
        let o = run(spec(bin, d.path()), CancellationToken::new()).await;
        assert_eq!(o.exit, Some(0));
        let rp = std::fs::read_to_string(d.path().join("report-path.txt")).unwrap();
        let rp = PathBuf::from(rp.trim());
        assert!(rp.ends_with("report.json"));
        assert!(!rp.parent().unwrap().exists());
    }
}
