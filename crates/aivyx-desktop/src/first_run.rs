//! First-run E — what the window shows when there's no daemon to host.
//!
//! The shell used to load the Studio URL whether or not the daemon had
//! started, so a first-time user saw a browser connection error and no way
//! forward. This module holds the pure pieces: which `aivyx-pa` to run,
//! where its output goes, and the local "set up" page shown instead.

use std::path::{Path, PathBuf};

/// Why the Studio isn't there to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupProblem {
    /// `aivyx-pa` itself couldn't be found or run.
    NotInstalled { bin: String, error: String },
    /// The daemon started and exited; `log_tail` is the end of its output.
    Exited { log_tail: String, log_path: PathBuf },
    /// The daemon is still running but never started serving.
    NoAnswer { log_path: PathBuf },
}

/// The `aivyx-pa` to drive: `AIVYX_PA_BIN` when set, else the one installed
/// beside this app (the packages ship both, so their versions match), else
/// `aivyx-pa` on `PATH`. `sibling` is passed only when that file exists.
pub fn pick_aivyx_bin(env: Option<String>, sibling: Option<PathBuf>) -> String {
    env.filter(|v| !v.is_empty())
        .or_else(|| sibling.map(|p| p.display().to_string()))
        .unwrap_or_else(|| "aivyx-pa".to_string())
}

/// Where the daemon this app starts writes its output:
/// `$XDG_STATE_HOME/aivyx-pa/desktop-daemon.log` (default `~/.local/state`).
pub fn daemon_log_path(xdg_state_home: Option<&str>, home: Option<&str>) -> Option<PathBuf> {
    let state = xdg_state_home
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            home.filter(|h| !h.is_empty())
                .map(|h| Path::new(h).join(".local/state"))
        })?;
    Some(state.join("aivyx-pa").join("desktop-daemon.log")) // instance-paths: ok — the desktop app serves the default instance only (v1)
}

/// The last `n` non-empty lines of `text`.
pub fn tail_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The page shown in place of the Studio: what happened, how to fix it, and a
/// **Try again** button (it posts `retry` to the shell over the webview IPC).
pub fn setup_page_html(problem: &SetupProblem) -> String {
    let (lead, detail, steps) = match problem {
        SetupProblem::NotInstalled { bin, error } => (
            "The <code>aivyx-pa</code> command isn't installed.".to_string(),
            format!("Tried to run <code>{}</code>: {}", escape(bin), escape(error)),
            "<li>Install it with the one-line installer, in a terminal:<br>\
             <code>curl --proto '=https' --tlsv1.2 -LsSf \
             https://github.com/Aivyx-Agent/aivyx-pa/releases/latest/download/aivyx-cli-installer.sh | sh</code></li>\
             <li>Run <code>aivyx-pa init</code> to choose a model and a passphrase.</li>\
             <li>Come back and choose <b>Try again</b>.</li>"
                .to_string(),
        ),
        SetupProblem::Exited { log_tail, log_path } => (
            "Your assistant's background service stopped before it could start.".to_string(),
            format!(
                "It said:<pre>{}</pre>Full output: <code>{}</code>",
                escape(log_tail),
                escape(&log_path.display().to_string())
            ),
            "<li>If you haven't set it up yet, open a terminal and run \
             <code>aivyx-pa init</code> — it sets up your model and passphrase.</li>\
             <li>Otherwise, <code>aivyx-pa doctor</code> checks your setup and says what to fix.</li>\
             <li>Then choose <b>Try again</b>.</li>"
                .to_string(),
        ),
        SetupProblem::NoAnswer { log_path } => (
            "Your assistant's background service is running but hasn't answered yet.".to_string(),
            format!(
                "Its output is in <code>{}</code>.",
                escape(&log_path.display().to_string())
            ),
            "<li>It may still be loading — wait a moment.</li>\
             <li><code>aivyx-pa doctor</code> checks your setup and says what to fix.</li>\
             <li>Then choose <b>Try again</b>.</li>"
                .to_string(),
        ),
    };
    format!(
        r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>Aivyx PA — setup needed</title>
<style>
:root {{ --bg:#11151c; --card:#1b2029; --ink:#e7e2d6; --muted:#9aa3ad; --accent:#c9a24b; }}
@media (prefers-color-scheme: light) {{ :root {{ --bg:#f6f3ec; --card:#fffdf8; --ink:#1d2129; --muted:#5b6470; }} }}
body {{ margin:0; background:var(--bg); color:var(--ink); font:16px/1.55 system-ui, sans-serif; }}
main {{ max-width:640px; margin:10vh auto; padding:0 24px; }}
.card {{ background:var(--card); border-radius:12px; padding:28px 32px; }}
h1 {{ font-size:1.4rem; margin:0 0 8px; }}
p.detail, p.detail pre {{ color:var(--muted); }}
pre {{ white-space:pre-wrap; font-size:.85rem; }}
code {{ font-size:.9em; word-break:break-all; }}
ol {{ padding-left:1.2em; }} li {{ margin:.4em 0; }}
button {{ margin-top:12px; padding:10px 22px; border:0; border-radius:8px; background:var(--accent); color:#11151c; font-weight:600; font-size:1rem; cursor:pointer; }}
</style></head>
<body><main><div class="card">
<h1>Aivyx PA isn't running yet</h1>
<p>{lead}</p>
<p class="detail">{detail}</p>
<ol>{steps}</ol>
<button id="retry" onclick="this.disabled=true;this.textContent='Trying…';window.ipc.postMessage('retry')">Try again</button>
</div></main></body></html>"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_app_prefers_the_aivyx_pa_installed_beside_it() {
        let sibling = Some(PathBuf::from("/usr/bin/aivyx-pa"));
        assert_eq!(pick_aivyx_bin(None, sibling.clone()), "/usr/bin/aivyx-pa");
        // An explicit override still wins; nothing beside it falls back to PATH.
        assert_eq!(pick_aivyx_bin(Some("/opt/pa".into()), sibling), "/opt/pa");
        assert_eq!(pick_aivyx_bin(Some(String::new()), None), "aivyx-pa");
        assert_eq!(pick_aivyx_bin(None, None), "aivyx-pa");
    }

    #[test]
    fn the_daemon_log_lives_in_the_state_dir() {
        assert_eq!(
            daemon_log_path(Some("/s"), Some("/h")),
            Some(PathBuf::from("/s/aivyx-pa/desktop-daemon.log"))
        );
        assert_eq!(
            daemon_log_path(Some(""), Some("/h")),
            Some(PathBuf::from("/h/.local/state/aivyx-pa/desktop-daemon.log"))
        );
        assert_eq!(daemon_log_path(None, None), None);
    }

    #[test]
    fn tail_lines_keeps_the_last_non_empty_lines() {
        assert_eq!(tail_lines("a\n\nb\nc\n\n", 2), "b\nc");
        assert_eq!(tail_lines("only", 5), "only");
    }

    #[test]
    fn a_daemon_that_exited_shows_what_it_said_and_how_to_set_up() {
        let page = setup_page_html(&SetupProblem::Exited {
            log_tail: "aivyx-pa: no passphrase available <here>".into(),
            log_path: PathBuf::from("/s/aivyx-pa/desktop-daemon.log"),
        });
        assert!(page.contains("no passphrase available &lt;here&gt;"), "escaped");
        assert!(page.contains("/s/aivyx-pa/desktop-daemon.log"));
        assert!(page.contains("<code>aivyx-pa init</code>"));
        assert!(page.contains("window.ipc.postMessage('retry')"));
    }

    #[test]
    fn a_missing_command_says_how_to_install_it() {
        let page = setup_page_html(&SetupProblem::NotInstalled {
            bin: "aivyx-pa".into(),
            error: "No such file or directory (os error 2)".into(),
        });
        assert!(page.contains("isn't installed"));
        assert!(page.contains("aivyx-cli-installer.sh"));
        assert!(page.contains("Try again"));
    }
}
