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
    /// There's no config yet: a first launch. `terminal_failed` when the
    /// app tried to open setup in a terminal and couldn't.
    NotSetUp { bin: String, terminal_failed: bool },
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

/// The config `aivyx-pa` reads: `AIVYX_PA_CONFIG_PATH` when set, else the
/// instance's standard file. No file there means nothing is set up yet.
pub fn config_file(env_override: Option<String>, standard: Option<PathBuf>) -> Option<PathBuf> {
    env_override.filter(|v| !v.is_empty()).map(PathBuf::from).or(standard)
}

/// `bin init` in a terminal window that stays open when setup ends, as
/// the programs to try in order. macOS uses Terminal; Linux tries the
/// distribution's default terminal, then the common ones.
pub fn setup_terminal_commands(bin: &str, macos: bool) -> Vec<(String, Vec<String>)> {
    let quoted = format!("'{}'", bin.replace('\'', r"'\''"));
    let script = format!(
        "{quoted} init; echo; echo 'Setup finished. You can close this window and \
         return to Aivyx PA.'; read -r _"
    );
    if macos {
        let applescript = script.replace('\\', r"\\").replace('"', r#"\""#);
        return vec![(
            "osascript".to_string(),
            vec![
                "-e".to_string(),
                format!(r#"tell application "Terminal" to do script "{applescript}""#),
                "-e".to_string(),
                r#"tell application "Terminal" to activate"#.to_string(),
            ],
        )];
    }
    let sh = |flag: &str| -> Vec<String> {
        let mut args: Vec<String> = flag.split_whitespace().map(str::to_string).collect();
        args.extend(["sh".to_string(), "-c".to_string(), script.clone()]);
        args
    };
    vec![
        ("x-terminal-emulator".to_string(), sh("-e")),
        ("gnome-terminal".to_string(), sh("--")),
        ("konsole".to_string(), sh("-e")),
        ("xfce4-terminal".to_string(), sh("-x")),
        ("kitty".to_string(), sh("")),
        ("alacritty".to_string(), sh("-e")),
        ("xterm".to_string(), sh("-e")),
    ]
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
    if let SetupProblem::NotSetUp { bin, terminal_failed } = problem {
        return welcome_page_html(bin, *terminal_failed);
    }
    let (lead, detail, steps) = match problem {
        SetupProblem::NotSetUp { .. } => unreachable!("handled above"),
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

/// The first-launch page: what setup is, a button that opens it, and one
/// to continue when it's done.
fn welcome_page_html(bin: &str, terminal_failed: bool) -> String {
    let fallback = if terminal_failed {
        format!(
            "<p class=\"note\">The app couldn't open a terminal on its own. Open \
             one yourself and run <code>{} init</code>, then come back.</p>",
            escape(bin)
        )
    } else {
        String::new()
    };
    format!(
        r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><title>Aivyx PA — welcome</title>
<style>
:root {{ --bg:#11151c; --card:#1b2029; --ink:#e7e2d6; --muted:#9aa3ad; --accent:#c9a24b; --line:#2a313d; }}
@media (prefers-color-scheme: light) {{ :root {{ --bg:#f6f3ec; --card:#fffdf8; --ink:#1d2129; --muted:#5b6470; --accent:#7f6223; --line:#ddd5c2; }} }}
body {{ margin:0; background:var(--bg); color:var(--ink); font:16px/1.55 system-ui, sans-serif; }}
main {{ max-width:640px; margin:10vh auto; padding:0 24px; }}
.card {{ background:var(--card); border-radius:12px; padding:28px 32px; }}
h1 {{ font-size:1.5rem; margin:0 0 8px; }}
p.muted {{ color:var(--muted); }}
p.note {{ border-left:3px solid var(--accent); padding-left:12px; }}
code {{ font-size:.9em; word-break:break-all; }}
ol {{ padding-left:1.2em; }} li {{ margin:.4em 0; }}
.actions {{ display:flex; gap:12px; flex-wrap:wrap; margin-top:16px; }}
button {{ padding:10px 22px; border-radius:8px; font-weight:600; font-size:1rem; cursor:pointer; }}
button.primary {{ border:0; background:var(--accent); color:var(--bg); }}
button.secondary {{ border:1px solid var(--line); background:transparent; color:var(--ink); }}
</style></head>
<body><main><div class="card">
<h1>Welcome to Aivyx PA</h1>
<p>Before your assistant can start, it needs a one-time setup. It takes a few minutes:</p>
<ol>
<li>pick the AI model it uses (one running on this computer, or a cloud account),</li>
<li>a few questions about you and what you want it for,</li>
<li>a passphrase that keeps its memory encrypted.</li>
</ol>
<p class="muted">Setup runs in a terminal window. Answer its questions there, then come back here.</p>
{fallback}
<div class="actions">
<button class="primary" onclick="window.ipc.postMessage('setup')">Open setup</button>
<button class="secondary" onclick="this.disabled=true;this.textContent='Starting…';window.ipc.postMessage('retry')">Setup's done — continue</button>
</div>
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
    fn the_config_is_where_aivyx_pa_looks_for_it() {
        let standard = Some(PathBuf::from("/h/.config/aivyx-pa/aivyx-pa.toml"));
        assert_eq!(config_file(None, standard.clone()), standard);
        assert_eq!(
            config_file(Some("/etc/pa.toml".into()), standard.clone()),
            Some(PathBuf::from("/etc/pa.toml"))
        );
        assert_eq!(config_file(Some(String::new()), standard.clone()), standard);
    }

    /// Onboarding audit 3 (H5): a first launch is a welcome, not a failure,
    /// and setup is one button away.
    #[test]
    fn a_first_launch_welcomes_and_offers_to_open_setup() {
        let page = setup_page_html(&SetupProblem::NotSetUp {
            bin: "/usr/bin/aivyx-pa".into(),
            terminal_failed: false,
        });
        assert!(page.contains("Welcome"), "{page}");
        assert!(!page.contains("isn't running"), "{page}");
        assert!(!page.contains("stopped"), "{page}");
        assert!(page.contains("window.ipc.postMessage('setup')"));
        assert!(page.contains("window.ipc.postMessage('retry')"));
    }

    #[test]
    fn without_a_terminal_the_page_gives_the_command() {
        let page = setup_page_html(&SetupProblem::NotSetUp {
            bin: "/usr/bin/aivyx-pa".into(),
            terminal_failed: true,
        });
        assert!(page.contains("<code>/usr/bin/aivyx-pa init</code>"), "{page}");
        assert!(page.contains("couldn't open a terminal"), "{page}");
    }

    #[test]
    fn setup_opens_in_a_terminal_that_runs_init() {
        let linux = setup_terminal_commands("/usr/bin/aivyx-pa", false);
        assert_eq!(linux[0].0, "x-terminal-emulator");
        assert!(linux.iter().any(|(prog, _)| prog == "gnome-terminal"));
        for (_, args) in &linux {
            assert!(args.join(" ").contains("'/usr/bin/aivyx-pa' init"), "{args:?}");
        }
        let mac = setup_terminal_commands("/Applications/Aivyx PA.app/Contents/MacOS/aivyx-pa", true);
        assert_eq!(mac.len(), 1);
        assert_eq!(mac[0].0, "osascript");
        // The path is quoted for the shell inside Terminal.
        assert!(
            mac[0].1.join(" ").contains(r"'/Applications/Aivyx PA.app/Contents/MacOS/aivyx-pa' init"),
            "{:?}",
            mac[0].1
        );
    }

    /// The script really runs `<bin> init`, even with a space and a quote
    /// in the path (an .app bundle's, say).
    #[cfg(unix)]
    #[test]
    fn the_setup_script_runs_init_with_an_awkward_path() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("aivyx desk'top {}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("aivyx-pa");
        // Outside the awkward folder, so the stand-in's own quoting is simple.
        let marker = std::env::temp_dir().join(format!("aivyx-desktop-ran-{}", std::process::id()));
        std::fs::write(&bin, format!("#!/bin/sh\necho \"$@\" > '{}'\n", marker.display())).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let (_, args) = setup_terminal_commands(&bin.display().to_string(), false)
            .into_iter()
            .find(|(program, _)| program == "xterm")
            .unwrap();
        let script = args.last().unwrap();
        // The trailing `read` (which keeps a real window open) sees end of
        // input here, so the exit status isn't the point; the marker is.
        std::process::Command::new("sh")
            .args(["-c", script])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert_eq!(std::fs::read_to_string(&marker).unwrap().trim(), "init");
        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_file(&marker).unwrap();
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
