//! `aivyx-pa daemon install` — first-class persistent service (Chapter Anchor).
//!
//! A local-first agent meant to run for *days* needs a supported way to stay
//! running. Today the only paths are the Docker appliance (Chapter Harbor) and
//! the desktop app's autostart; the **bare daemon** — the default install — has
//! none, so on the dogfood rig we hand-rolled `loginctl enable-linger` +
//! `systemd-run`. No real user will do that. This module turns it into one
//! command.
//!
//! ## AN.0 design decisions
//!
//! - **Linux = a systemd *user* unit** at `~/.config/systemd/user/aivyx-pa-daemon.service`,
//!   plus `loginctl enable-linger <user>` so it runs **without an active login
//!   session** (the runs-for-days requirement). Not a system unit — no root, no
//!   sudo; the agent is the user's, not the machine's.
//! - **macOS = a launchd `LaunchAgent`** plist at
//!   `~/Library/LaunchAgents/com.aivyx-pa.daemon.plist` (rendered in AN.2).
//! - **Windows = out of scope** for now (documented; the desktop app covers it).
//! - **Command surface:** `aivyx-pa daemon install [--web-ui] [--no-start]` and
//!   `aivyx-pa daemon uninstall` (wired in AN.1).
//! - **Secret handling (the real decision):** the unit references an *optional*
//!   env file (`EnvironmentFile=-<path>`, mode `0o600`) rather than baking the
//!   passphrase into the unit (the hand-rolled rig version put `AIVYX_PA_PASSPHRASE`
//!   plaintext into the unit's `--setenv`, world-readable in `systemctl cat`).
//!   `daemon install` captures the passphrase (from the env or a prompt) and
//!   writes the `0o600` env file (AN.1); the **rendered unit carries no secret**,
//!   only the file path. Plaintext-at-rest under `0o600` is the same protection
//!   level as the redb store passphrase and the federation key; a keyring
//!   backend is a future enhancement.
//! - **Idempotent:** re-install overwrites the unit + reloads; uninstall stops,
//!   disables, and removes it. **No new capability base / no P10** — this is
//!   operator-facing management that shells out to `systemctl`/`launchctl`, not
//!   an agent tool.
//!
//! AN.0 ships the pure, tested render/plan layer below; AN.1 wires the CLI and
//! performs the side effects (write the unit, enable linger, `enable --now`).

use std::path::{Path, PathBuf};
use std::process::Command;

/// The host service manager Anchor targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    /// systemd user unit + linger.
    Linux,
    /// launchd LaunchAgent (AN.2).
    MacOs,
    /// No supported service manager — `install` errors with guidance.
    Unsupported,
}

impl Platform {
    /// Detect the current host's service manager.
    pub fn detect() -> Self {
        if cfg!(target_os = "linux") {
            Platform::Linux
        } else if cfg!(target_os = "macos") {
            Platform::MacOs
        } else {
            Platform::Unsupported
        }
    }
}

/// The unit / plist name (stable across platforms for `status`/`uninstall`).
pub const SERVICE_UNIT: &str = "aivyx-pa-daemon.service";
/// The launchd label (macOS, AN.2).
pub const LAUNCHD_LABEL: &str = "com.aivyx-pa.daemon";

/// Where the secret env file lives (referenced by the unit, written 0o600 by
/// `install`). Relative to the user's config dir.
pub const ENV_FILE_REL: &str = "aivyx-pa/daemon.env";

/// A resolved install plan — the concrete paths + contents an install will
/// write. Pure data so a `--dry-run`/preview (AN.1) can show exactly what will
/// happen before any side effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServicePlan {
    /// Where the unit/plist file goes.
    pub unit_path: PathBuf,
    /// The rendered unit/plist contents (carries **no secret**).
    pub unit_contents: String,
    /// The 0o600 env file the unit references (written separately by install).
    pub env_file_path: PathBuf,
}

/// Render the systemd **user** unit. Pure over its inputs so the exact bytes are
/// testable. The unit references the env file via `EnvironmentFile=-` (the `-`
/// makes it optional, so a missing/rotated secret file degrades to a clear
/// startup error rather than a unit that won't load).
pub fn render_systemd_unit(
    bin_path: &str,
    web_ui: bool,
    env_file: &str,
    working_dir: &str,
) -> String {
    let web_ui_flag = if web_ui { " --web-ui" } else { "" };
    format!(
        "[Unit]\n\
         Description=Aivyx personal-assistant daemon\n\
         Documentation=https://github.com/Aivyx-Agent/aivyx\n\
         After=network-online.target\n\
         Wants=network-online.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={bin_path} daemon run{web_ui_flag}\n\
         WorkingDirectory={working_dir}\n\
         EnvironmentFile=-{env_file}\n\
         Restart=on-failure\n\
         RestartSec=5\n\
         \n\
         [Install]\n\
         WantedBy=default.target\n"
    )
}

/// Render the 0o600 env file the unit references. The **only** place the
/// passphrase is written, and it lives at rest under owner-only permissions
/// (set by the caller) — never in the unit, never in the process table.
pub fn render_env_file(passphrase: &str) -> String {
    format!("AIVYX_PA_PASSPHRASE={passphrase}\n")
}

/// Write the `daemon.env` file at `0600` from the moment it exists — never a
/// `write`-then-`chmod` window at a wider mode (Task 8, 2026-09-16 security
/// audit: the prior `std::fs::write` + `set_permissions_600` two-step here
/// left exactly that window, same TOCTOU class as Task 7's socket bind, but
/// for the file holding the actual master passphrase). Reuses
/// `connect::write_file_at_0600` — Task 5 already established this exact
/// atomic-0600 pattern in this crate for `config.toml`; no reason to invent
/// a third variant of it here.
fn write_env_file_secure(path: &Path, passphrase: &str) -> std::io::Result<()> {
    crate::connect::write_file_at_0600(path, render_env_file(passphrase).as_bytes())
}

/// Compute the concrete Linux install plan — pure over its inputs so the paths
/// and unit contents are testable without touching the real home or running any
/// command.
pub fn plan_linux(
    config_dir: &Path,
    bin_path: &str,
    working_dir: &str,
    web_ui: bool,
) -> ServicePlan {
    let unit_path = config_dir.join("systemd/user").join(SERVICE_UNIT);
    let env_file_path = config_dir.join(ENV_FILE_REL);
    let unit_contents = render_systemd_unit(
        bin_path,
        web_ui,
        &env_file_path.display().to_string(),
        working_dir,
    );
    ServicePlan {
        unit_path,
        unit_contents,
        env_file_path,
    }
}

/// `aivyx-pa daemon install` — install + (by default) start the daemon as a
/// persistent user service. Linux now; macOS in AN.2.
pub fn run_install(web_ui: bool, start: bool) -> Result<(), String> {
    match Platform::detect() {
        Platform::Linux => install_linux(web_ui, start),
        Platform::MacOs => install_macos(web_ui, start),
        Platform::Unsupported => Err(
            "no supported service manager on this platform — run `aivyx-pa daemon run` \
             directly, or use the Docker appliance (docs/INSTALL.md)."
                .into(),
        ),
    }
}

/// `aivyx-pa daemon uninstall` — stop, disable, and remove the service.
pub fn run_uninstall() -> Result<(), String> {
    match Platform::detect() {
        Platform::Linux => uninstall_linux(),
        Platform::MacOs => uninstall_macos(),
        Platform::Unsupported => Err("no service was installed by aivyx-pa on this platform.".into()),
    }
}

/// The installed unit/plist path, if the service is installed on this host.
/// `None` when not installed (or unsupported platform). Used by `aivyx-pa doctor`.
pub fn installed_unit_path() -> Option<PathBuf> {
    let path = match Platform::detect() {
        Platform::Linux => user_config_dir()
            .ok()?
            .join("systemd/user")
            .join(SERVICE_UNIT),
        Platform::MacOs => macos_plist_path().ok()?,
        Platform::Unsupported => return None,
    };
    path.exists().then_some(path)
}

/// Best-effort liveness of the installed service. `None` when it can't be
/// determined (unsupported platform, or the query command isn't available).
pub fn is_active() -> Option<bool> {
    match Platform::detect() {
        Platform::Linux => {
            let out = Command::new("systemctl")
                .args(["--user", "is-active", SERVICE_UNIT])
                .output()
                .ok()?;
            Some(String::from_utf8_lossy(&out.stdout).trim() == "active")
        }
        Platform::MacOs => {
            let uid = current_uid().ok()?;
            let out = Command::new("launchctl")
                .args(["print", &format!("gui/{uid}/{LAUNCHD_LABEL}")])
                .output()
                .ok()?;
            Some(out.status.success())
        }
        Platform::Unsupported => None,
    }
}

fn install_linux(web_ui: bool, start: bool) -> Result<(), String> {
    let bin = current_exe_path()?;
    let config_dir = user_config_dir()?;
    let working_dir = install_working_dir();
    let plan = plan_linux(&config_dir, &bin, &working_dir, web_ui);

    // The passphrase: env first (the established policy), else a no-echo prompt.
    let passphrase = resolve_passphrase()?;

    // Write the 0o600 env file (the only secret on disk), then the unit.
    if let Some(parent) = plan.env_file_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("create config dir {}: {e}", parent.display()))?;
    }
    write_env_file_secure(&plan.env_file_path, &passphrase)
        .map_err(|e| format!("write env file: {e}"))?;

    if let Some(parent) = plan.unit_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("create unit dir {}: {e}", parent.display()))?;
    }
    std::fs::write(&plan.unit_path, &plan.unit_contents)
        .map_err(|e| format!("write unit {}: {e}", plan.unit_path.display()))?;

    // Linger so the service runs without an active login session (runs-for-days).
    run_cmd("loginctl", &["enable-linger", &current_user()])?;
    run_cmd("systemctl", &["--user", "daemon-reload"])?;
    run_cmd("systemctl", &["--user", "enable", SERVICE_UNIT])?;
    if start {
        // restart (not just start) so a re-install picks up the new unit/env.
        run_cmd("systemctl", &["--user", "restart", SERVICE_UNIT])?;
    }

    eprintln!(
        "aivyx-pa daemon: installed as a user service.\n  \
         unit:   {}\n  \
         env:    {} (0600)\n  \
         status: systemctl --user status aivyx-pa-daemon\n  \
         logs:   journalctl --user -u aivyx-pa-daemon -f{}",
        plan.unit_path.display(),
        plan.env_file_path.display(),
        if start {
            "\n  (started; runs across reboots via linger)"
        } else {
            "\n  (enabled; start with `systemctl --user start aivyx-pa-daemon`)"
        },
    );
    Ok(())
}

fn uninstall_linux() -> Result<(), String> {
    let config_dir = user_config_dir()?;
    let unit_path = config_dir.join("systemd/user").join(SERVICE_UNIT);
    let env_file_path = config_dir.join(ENV_FILE_REL);

    // Stop + disable; ignore failures (the unit may already be gone/stopped).
    let _ = run_cmd("systemctl", &["--user", "disable", "--now", SERVICE_UNIT]);

    let mut removed = false;
    if unit_path.exists() {
        std::fs::remove_file(&unit_path)
            .map_err(|e| format!("remove unit {}: {e}", unit_path.display()))?;
        removed = true;
    }
    // The env file holds the passphrase — remove it on uninstall (hygiene).
    if env_file_path.exists() {
        std::fs::remove_file(&env_file_path)
            .map_err(|e| format!("remove env file {}: {e}", env_file_path.display()))?;
    }
    let _ = run_cmd("systemctl", &["--user", "daemon-reload"]);

    if removed {
        eprintln!(
            "aivyx-pa daemon: service uninstalled (unit + env file removed). \
             Linger was left enabled; disable with `loginctl disable-linger`."
        );
    } else {
        eprintln!("aivyx-pa daemon: no installed service found — nothing to remove.");
    }
    Ok(())
}

/// Render the launchd `LaunchAgent` plist.
///
/// Unlike systemd, launchd has **no `EnvironmentFile` equivalent**, so the
/// passphrase rides the plist's `EnvironmentVariables` (the standard launchd
/// pattern) — the plist itself is therefore written `0o600`. This is the one
/// place macOS differs from Linux's secret-out-of-the-unit design; the at-rest
/// protection (owner-only) is the same. (A future enhancement could teach the
/// daemon a passphrase-*file* source for parity.) All interpolated values are
/// XML-escaped so a `&`/`<` in a path or passphrase can't break the plist.
/// `KeepAlive`/`SuccessfulExit=false` mirrors systemd's `Restart=on-failure`
/// (restart on crash, but honor a clean `daemon stop`).
pub fn render_launchd_plist(
    bin_path: &str,
    web_ui: bool,
    working_dir: &str,
    passphrase: &str,
) -> String {
    let mut program_args = format!(
        "        <string>{}</string>\n        <string>daemon</string>\n        <string>run</string>\n",
        xml_escape(bin_path),
    );
    if web_ui {
        program_args.push_str("        <string>--web-ui</string>\n");
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \x20   <key>Label</key>\n    <string>{label}</string>\n\
         \x20   <key>ProgramArguments</key>\n    <array>\n{program_args}    </array>\n\
         \x20   <key>WorkingDirectory</key>\n    <string>{workdir}</string>\n\
         \x20   <key>EnvironmentVariables</key>\n    <dict>\n\
         \x20       <key>AIVYX_PA_PASSPHRASE</key>\n        <string>{pass}</string>\n    </dict>\n\
         \x20   <key>RunAtLoad</key>\n    <true/>\n\
         \x20   <key>KeepAlive</key>\n    <dict>\n        <key>SuccessfulExit</key>\n        <false/>\n    </dict>\n\
         </dict>\n\
         </plist>\n",
        label = LAUNCHD_LABEL,
        workdir = xml_escape(working_dir),
        pass = xml_escape(passphrase),
    )
}

/// Minimal XML text escaping for plist string values.
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn macos_plist_path() -> Result<PathBuf, String> {
    let home = std::env::var("HOME").map_err(|_| "HOME is not set".to_string())?;
    Ok(PathBuf::from(home)
        .join("Library/LaunchAgents")
        .join(format!("{LAUNCHD_LABEL}.plist")))
}

fn install_macos(web_ui: bool, start: bool) -> Result<(), String> {
    let bin = current_exe_path()?;
    let working_dir = install_working_dir();
    let passphrase = resolve_passphrase()?;
    let plist_path = macos_plist_path()?;

    if let Some(parent) = plist_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create LaunchAgents dir: {e}"))?;
    }
    crate::connect::write_file_at_0600(
        &plist_path,
        render_launchd_plist(&bin, web_ui, &working_dir, &passphrase).as_bytes(),
    )
    .map_err(|e| format!("write plist {}: {e}", plist_path.display()))?;

    if start {
        let uid = current_uid()?;
        let domain = format!("gui/{uid}");
        let plist = plist_path.display().to_string();
        // bootout first so a re-install replaces a running agent (idempotent);
        // ignore the error when nothing is loaded yet.
        let _ = run_cmd(
            "launchctl",
            &["bootout", &format!("{domain}/{LAUNCHD_LABEL}")],
        );
        run_cmd("launchctl", &["bootstrap", &domain, &plist])?;
        let _ = run_cmd(
            "launchctl",
            &["enable", &format!("{domain}/{LAUNCHD_LABEL}")],
        );
    }

    eprintln!(
        "aivyx-pa daemon: installed as a launchd LaunchAgent.\n  \
         plist:  {} (0600 — carries the passphrase)\n  \
         logs:   log show --predicate 'process == \"aivyx-pa\"'{}",
        plist_path.display(),
        if start {
            "\n  (started; runs at login)"
        } else {
            "\n  (written; load with `launchctl bootstrap gui/$(id -u) <plist>`)"
        },
    );
    Ok(())
}

fn uninstall_macos() -> Result<(), String> {
    let plist_path = macos_plist_path()?;
    if let Ok(uid) = current_uid() {
        let _ = run_cmd(
            "launchctl",
            &["bootout", &format!("gui/{uid}/{LAUNCHD_LABEL}")],
        );
    }
    let mut removed = false;
    if plist_path.exists() {
        std::fs::remove_file(&plist_path)
            .map_err(|e| format!("remove plist {}: {e}", plist_path.display()))?;
        removed = true;
    }
    if removed {
        eprintln!("aivyx-pa daemon: launchd service uninstalled (plist removed).");
    } else {
        eprintln!("aivyx-pa daemon: no installed service found — nothing to remove.");
    }
    Ok(())
}

fn current_uid() -> Result<String, String> {
    let out = Command::new("id")
        .arg("-u")
        .output()
        .map_err(|e| format!("resolve uid via `id -u`: {e}"))?;
    if !out.status.success() {
        return Err("`id -u` failed".into());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The passphrase for the unattended service: `AIVYX_PA_PASSPHRASE` if
/// set+non-empty (the established policy), else the OS keyring (Chapter
/// Keyring — Task 8, 2026-09-16 security audit: this used to skip straight
/// to the prompt/file-write fallback without ever checking the keyring,
/// unlike the daemon's own startup path, `select_passphrase_source` in
/// `aivyx.rs`), else a no-echo prompt.
fn resolve_passphrase() -> Result<String, String> {
    resolve_passphrase_with(aivyx_channel::keyring_store::retrieve)
}

/// Testable core of [`resolve_passphrase`]. Takes the keyring lookup as a
/// parameter rather than calling `keyring_store::retrieve` directly: the OS
/// keyring's `mock` backend builds a fresh, independent credential per
/// `Entry::new` (documented in `keyring_store`'s own tests), so it can't
/// model a store-then-retrieve round trip the way a real Secret Service /
/// Keychain does — dependency injection is what actually makes "the keyring
/// is checked before prompting" testable here.
fn resolve_passphrase_with(
    keyring_retrieve: impl FnOnce() -> Result<
        Option<secrecy::SecretString>,
        aivyx_channel::keyring_store::KeyringError,
    >,
) -> Result<String, String> {
    if let Ok(v) = std::env::var("AIVYX_PA_PASSPHRASE") {
        if !v.is_empty() {
            return Ok(v);
        }
    }
    // Chapter Keyring — prefer the OS keyring over an interactive prompt or
    // writing a fresh plaintext file, mirroring select_passphrase_source's
    // preference order. An unavailable/locked keyring is not fatal — fall
    // through to the prompt, same as that function.
    if let Ok(Some(secret)) = keyring_retrieve() {
        use secrecy::ExposeSecret;
        return Ok(secret.expose_secret().to_string());
    }
    let p =
        rpassword::prompt_password("Store passphrase for the unattended service (input hidden): ")
            .map_err(|e| format!("failed to read passphrase: {e}"))?;
    if p.is_empty() {
        return Err("passphrase must not be empty".into());
    }
    Ok(p)
}

fn current_exe_path() -> Result<String, String> {
    std::env::current_exe()
        .map_err(|e| format!("resolve aivyx-pa binary path: {e}"))
        .map(|p| p.display().to_string())
}

/// `$XDG_CONFIG_HOME` or `$HOME/.config`.
fn user_config_dir() -> Result<PathBuf, String> {
    if let Ok(x) = std::env::var("XDG_CONFIG_HOME") {
        if !x.is_empty() {
            return Ok(PathBuf::from(x));
        }
    }
    let home = std::env::var("HOME").map_err(|_| "HOME is not set".to_string())?;
    Ok(PathBuf::from(home).join(".config"))
}

/// The daemon loads `aivyx-pa.toml` from its working directory — use the install
/// cwd when it holds a config, else `$HOME`.
fn install_working_dir() -> String {
    if let Ok(cwd) = std::env::current_dir() {
        if cwd.join("aivyx-pa.toml").exists() {
            return cwd.display().to_string();
        }
    }
    std::env::var("HOME").unwrap_or_else(|_| ".".to_string())
}

fn current_user() -> String {
    std::env::var("USER").unwrap_or_default()
}

/// Run a command, mapping a non-zero exit (or spawn failure) to a readable
/// error that names the command.
fn run_cmd(program: &str, args: &[&str]) -> Result<(), String> {
    let status = Command::new(program)
        .args(args)
        .status()
        .map_err(|e| format!("`{program} {}` failed to run: {e}", args.join(" ")))?;
    if !status.success() {
        return Err(format!(
            "`{program} {}` exited with {}",
            args.join(" "),
            status
                .code()
                .map(|c| c.to_string())
                .unwrap_or_else(|| "signal".into()),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_picks_a_platform() {
        // On the build/CI host (Linux) detection is Linux; the point is it never
        // panics and returns a concrete platform.
        let p = Platform::detect();
        assert!(matches!(
            p,
            Platform::Linux | Platform::MacOs | Platform::Unsupported
        ));
    }

    #[test]
    fn systemd_unit_has_the_load_bearing_directives() {
        let unit = render_systemd_unit(
            "/home/u/.local/bin/aivyx-pa",
            false,
            "/home/u/.config/aivyx-pa/daemon.env",
            "/home/u",
        );
        assert!(unit.contains("ExecStart=/home/u/.local/bin/aivyx-pa daemon run\n"));
        assert!(unit.contains("Restart=on-failure"));
        assert!(unit.contains("WantedBy=default.target")); // user-unit auto-start target
        // optional env-file reference (the `-`), so a missing secret file doesn't
        // wedge the unit at load time.
        assert!(unit.contains("EnvironmentFile=-/home/u/.config/aivyx-pa/daemon.env"));
        assert!(unit.contains("WorkingDirectory=/home/u"));
    }

    #[test]
    fn web_ui_flag_is_threaded_into_execstart() {
        let with = render_systemd_unit("/b/aivyx-pa", true, "/e", "/w");
        assert!(with.contains("ExecStart=/b/aivyx-pa daemon run --web-ui\n"));
        let without = render_systemd_unit("/b/aivyx-pa", false, "/e", "/w");
        assert!(without.contains("ExecStart=/b/aivyx-pa daemon run\n"));
        assert!(!without.contains("--web-ui"));
    }

    #[test]
    fn unit_never_contains_a_secret() {
        // The rendered unit references the env-file path but never a passphrase.
        let unit = render_systemd_unit("/b/aivyx-pa", true, "/home/u/.config/aivyx-pa/daemon.env", "/w");
        assert!(!unit.to_lowercase().contains("passphrase"));
        assert!(!unit.contains("AIVYX_PA_PASSPHRASE"));
    }

    #[test]
    fn plan_resolves_unit_and_env_paths_under_config_dir() {
        let plan = plan_linux(
            Path::new("/home/u/.config"),
            "/home/u/.local/bin/aivyx-pa",
            "/home/u",
            false,
        );
        assert_eq!(
            plan.unit_path,
            PathBuf::from("/home/u/.config/systemd/user/aivyx-pa-daemon.service")
        );
        assert_eq!(
            plan.env_file_path,
            PathBuf::from("/home/u/.config/aivyx-pa/daemon.env")
        );
        // the unit references that exact env-file path
        assert!(
            plan.unit_contents
                .contains("EnvironmentFile=-/home/u/.config/aivyx-pa/daemon.env")
        );
    }

    #[test]
    fn env_file_holds_the_passphrase_and_nothing_else() {
        assert_eq!(render_env_file("s3cr3t"), "AIVYX_PA_PASSPHRASE=s3cr3t\n");
    }

    #[test]
    fn launchd_plist_is_well_formed_with_load_bearing_keys() {
        let plist = render_launchd_plist("/usr/local/bin/aivyx-pa", false, "/Users/u", "pw");
        assert!(plist.starts_with("<?xml version=\"1.0\""));
        assert!(plist.contains("<key>Label</key>\n    <string>com.aivyx-pa.daemon</string>"));
        assert!(plist.contains("<string>/usr/local/bin/aivyx-pa</string>"));
        assert!(plist.contains("<string>daemon</string>"));
        assert!(plist.contains("<string>run</string>"));
        assert!(plist.contains("<key>RunAtLoad</key>\n    <true/>"));
        // KeepAlive/SuccessfulExit=false mirrors Restart=on-failure
        assert!(plist.contains("<key>SuccessfulExit</key>"));
        assert!(plist.contains("</plist>"));
    }

    #[test]
    fn launchd_plist_threads_web_ui_and_carries_the_secret() {
        let with = render_launchd_plist("/b/aivyx-pa", true, "/w", "pw");
        assert!(with.contains("<string>--web-ui</string>"));
        let without = render_launchd_plist("/b/aivyx-pa", false, "/w", "pw");
        assert!(!without.contains("--web-ui"));
        // macOS DOES carry the secret in the plist (0600) — the documented
        // platform difference from Linux's env-file.
        assert!(without.contains("<key>AIVYX_PA_PASSPHRASE</key>\n        <string>pw</string>"));
    }

    #[test]
    fn launchd_plist_xml_escapes_values() {
        // a passphrase with XML-special chars must not break the plist
        let plist = render_launchd_plist("/b/aivyx-pa", false, "/w", "a&b<c>\"d'");
        assert!(plist.contains("a&amp;b&lt;c&gt;&quot;d&apos;"));
        assert!(!plist.contains("a&b<c>"));
    }

    #[cfg(unix)]
    #[test]
    fn env_file_is_never_written_at_a_wider_mode_than_0600() {
        use std::os::unix::fs::PermissionsExt;
        // Deliberately not manipulating the process umask here (Task 7's
        // review found that racy against this crate's own concurrent test
        // suite, since umask is process-global state). Not needed anyway:
        // `write_file_at_0600` passes the mode straight to `open(2)`'s
        // `O_CREAT` argument, and a umask can only ever clear bits from a
        // requested mode, never add them — 0600 has no group/other bits to
        // clear, so it lands at 0600 regardless of the ambient umask.
        let dir = std::env::temp_dir().join(format!(
            "aivyx-daemon-env-secure-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("daemon.env");

        write_env_file_secure(&path, "test-passphrase").unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "AIVYX_PA_PASSPHRASE=test-passphrase\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_passphrase_checks_keyring_before_prompting_or_writing_a_file() {
        // AIVYX_PA_PASSPHRASE is asserted absent so the env-var short
        // circuit can't mask a broken keyring check; if some ambient
        // environment happens to have it set, skip rather than false-fail.
        if std::env::var("AIVYX_PA_PASSPHRASE").is_ok() {
            eprintln!(
                "skipping resolve_passphrase_checks_keyring_before_prompting_or_writing_a_file: \
                 AIVYX_PA_PASSPHRASE is set in this environment"
            );
            return;
        }
        // A real `rpassword::prompt_password` call would block/fail reading
        // a TTY under `cargo test` — reaching it at all would fail this
        // test's premise. Never calling it is exactly what proves the
        // keyring is consulted first.
        let result = resolve_passphrase_with(|| {
            Ok(Some(secrecy::SecretString::from(
                "from-the-keyring".to_string(),
            )))
        });
        assert_eq!(result.unwrap(), "from-the-keyring");
        // Deliberately not also exercising the `Ok(None)` (keyring reachable,
        // nothing stored) branch here: it falls through to
        // `rpassword::prompt_password`, which opens `/dev/tty` directly and
        // would hang or misbehave under `cargo test`'s non-interactive
        // environment. The env-var short-circuit above and this keyring-hit
        // case are what's safely testable without a real TTY.
    }
}
