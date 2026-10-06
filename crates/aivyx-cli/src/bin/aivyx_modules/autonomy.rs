//! `aivyx-pa autonomy` — the operator-facing autonomy-dial Settings command
//! (Chapter Reins RN.6). `show` prints the resolved autonomy level, the posture
//! it expands to, and any per-domain overrides + auto-approve allowlist;
//! `set <level>` rewrites the `[autonomy] level` key of `aivyx-pa.toml`
//! (re-confirming the autonomy-granting levels).
//!
//! Like `aivyx-pa access`, these are synchronous file operations — no daemon, no
//! passphrase, no API key. The change takes effect on the next daemon start.
//!
//! Scope note (RN.6a): `set` rewrites only the `level`. Per-domain overrides and
//! the auto-approve allowlist are hand-edited in `aivyx-pa.toml` for now (and
//! *displayed* by `show`); a richer editor + the Studio "Autonomy" section are
//! RN.6b.

use std::io::{BufRead, Write};
use std::path::Path;

use aivyx_config::{
    AivyxConfig, AutonomyLevel, AutonomyPosture, FieldSource, GatePosture, GrowthAdoption,
    LoadOptions, write_autonomy_section,
};

/// Module-local copy of the default config path (mirrors the other subcommand
/// modules — no coupling to `crate::DEFAULT_TOML_PATH`).
/// The operator's `aivyx-pa.toml` — see `aivyx_config::resolve_config_path`.
fn config_path() -> std::path::PathBuf {
    aivyx_config::resolve_config_path()
}

/// `aivyx-pa autonomy show` — print the current level + the posture it resolves to.
pub fn run_autonomy_show() -> Result<(), String> {
    let cfg = load_config_for_inspection(&config_path())?;
    print!("{}", render_autonomy_for_show(&cfg));
    Ok(())
}

/// `aivyx-pa autonomy set <level> [--yes]` — rewrite `[autonomy] level`. The
/// autonomy-granting levels (`autonomous` / `unleashed`) require a confirmation
/// unless `--yes`.
pub fn run_autonomy_set(level: AutonomyLevel, yes: bool) -> Result<(), String> {
    run_autonomy_set_at(&config_path(), level, yes)
}

/// Path-parametrized core of [`run_autonomy_set`] — lets tests drive an
/// isolated temp file without touching the process-global cwd.
fn run_autonomy_set_at(path: &Path, level: AutonomyLevel, yes: bool) -> Result<(), String> {
    // Levels that let the agent act unattended are deliberate: confirm first.
    if grants_unattended_autonomy(level) && !yes {
        let warning = match level {
            AutonomyLevel::Unleashed => {
                " — this runs an armed, self-directing agent with \
                 `confirm_destructive` OFF; intended for a dedicated, isolated host"
            }
            AutonomyLevel::Autonomous => {
                " — this lets the agent pursue goals unattended within its caps \
                 (irreversible actions are still refused without a human)"
            }
            _ => "",
        };
        if !confirm(&format!("Set autonomy to '{level}'{warning}?"))? {
            return Err("aborted — autonomy level unchanged.".into());
        }
    }

    write_autonomy_section(path, level).map_err(|e| e.to_string())?;

    eprintln!("Autonomy level set to `{level}` in {}.", path.display());
    eprintln!("  Run `aivyx-pa autonomy show` to see the posture it resolves to.");
    eprintln!(
        "  Note: the dial's runtime effects are being wired incrementally \
         (see https://github.com/Aivyx-Agent/aivyx-pa/blob/main/docs/AUTONOMY.md); each dimension takes effect on the next daemon \
         start as its phase lands."
    );
    Ok(())
}

/// Parse the `<level>` token of `aivyx-pa autonomy set`.
pub fn parse_level(s: &str) -> Result<AutonomyLevel, String> {
    // The string⇄level mapping lives once in `aivyx_config::AutonomyLevel`; this
    // wraps it with the CLI's operator-facing error text.
    AutonomyLevel::from_wire(s).ok_or_else(|| {
        format!(
            "unknown autonomy level `{s}`. \
             Supported: manual, assisted, supervised, autonomous, unleashed"
        )
    })
}

/// Whether a level lets the agent act unattended (so `set` confirms first).
fn grants_unattended_autonomy(level: AutonomyLevel) -> bool {
    matches!(level, AutonomyLevel::Autonomous | AutonomyLevel::Unleashed)
}

fn render_autonomy_for_show(cfg: &AivyxConfig) -> String {
    let src = |s: FieldSource| match s {
        FieldSource::Default => "default",
        FieldSource::Toml => "aivyx-pa.toml",
        FieldSource::Env => "env",
        FieldSource::EncryptedStore => "encrypted-store",
    };
    let mut out = String::new();
    out.push_str("aivyx-pa autonomy:\n");
    out.push_str(&format!(
        "  level     = {} ({})\n",
        cfg.autonomy_level.value,
        src(cfg.autonomy_level.source),
    ));
    out.push_str(&render_posture("  global", &cfg.effective_autonomy(None)));

    if cfg.autonomy_overrides.is_empty() {
        out.push_str("  overrides = (none)\n");
    } else {
        out.push_str("  overrides:\n");
        let looser = aivyx_config::looser_overrides(cfg.autonomy_level.value, &cfg.autonomy_overrides);
        for ov in &cfg.autonomy_overrides {
            let posture = ov.level.expand();
            let effect = if matches!(posture.gate, aivyx_config::GatePosture::ConfirmAll) {
                "asks before any change"
            } else if posture.confirm_destructive {
                "deletes and overwrites ask"
            } else {
                "deletes and overwrites run"
            };
            let note = if looser.iter().any(|o| o.domain == ov.domain) {
                "; looser than global"
            } else {
                ""
            };
            out.push_str(&format!("    [{}] → {} ({effect}{note})\n", ov.domain, ov.level));
        }
    }
    out.push_str(&format!(
        "  areas: {}\n",
        aivyx_capability::areas().into_iter().collect::<Vec<_>>().join(", ")
    ));

    if cfg.autonomy_auto_approve.is_empty() {
        out.push_str("  auto_approve = (none)\n");
    } else {
        out.push_str(&format!(
            "  auto_approve = {} (reversible scopes; never irreversible)\n",
            cfg.autonomy_auto_approve.join(", "),
        ));
    }
    out.push_str(
        "  (the autonomy level applies to the local operator; remote channels \
         stay tier-attenuated)\n",
    );
    out
}

/// One-line summary of a resolved posture.
fn render_posture(label: &str, p: &AutonomyPosture) -> String {
    let gate = match p.gate {
        GatePosture::ConfirmAll => "confirm-all",
        GatePosture::ConfirmIrreversible => "confirm-irreversible",
        GatePosture::BatchIrreversible => "batch-irreversible",
        GatePosture::RejectUnattended => "reject-unattended",
    };
    let growth = match p.growth {
        GrowthAdoption::None => "none",
        GrowthAdoption::ProposeOnly => "propose-only",
        GrowthAdoption::LowRiskAuto => "low-risk-auto",
        GrowthAdoption::PolicyAuto => "policy-auto",
        GrowthAdoption::BroadAuto => "broad-auto",
    };
    format!(
        "{label}    = gate:{gate}  loop:{}  confirm_destructive:{}  growth:{growth}\n",
        if p.loop_enabled { "armed" } else { "off" },
        p.confirm_destructive,
    )
}

/// Prompt the operator for a yes/no on stdin; defaults to NO on empty input.
fn confirm(question: &str) -> Result<bool, String> {
    print!("{question} [y/N]: ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut line)
        .map_err(|e| format!("failed to read confirmation: {e}"))?;
    Ok(matches!(
        line.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn load_config_for_inspection(path: &Path) -> Result<AivyxConfig, String> {
    let opts = LoadOptions {
        toml_path: Some(path.to_path_buf()),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    AivyxConfig::load_from_env_and_toml(&opts)
        .map_err(|e| format!("failed to load {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_level_accepts_known_levels() {
        assert_eq!(parse_level("assisted").unwrap(), AutonomyLevel::Assisted);
        assert_eq!(
            parse_level("autonomous").unwrap(),
            AutonomyLevel::Autonomous
        );
        assert_eq!(parse_level("unleashed").unwrap(), AutonomyLevel::Unleashed);
        assert!(parse_level("bogus").is_err());
    }

    #[test]
    fn only_autonomous_and_unleashed_need_confirmation() {
        assert!(grants_unattended_autonomy(AutonomyLevel::Autonomous));
        assert!(grants_unattended_autonomy(AutonomyLevel::Unleashed));
        for safe in [
            AutonomyLevel::Manual,
            AutonomyLevel::Assisted,
            AutonomyLevel::Supervised,
        ] {
            assert!(!grants_unattended_autonomy(safe));
        }
    }

    #[test]
    fn set_writes_the_level_and_preserves_other_sections() {
        let dir = std::env::temp_dir().join(format!("aivyx-autonomy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let toml = dir.join("aivyx-pa.toml");
        // A pre-existing [access] section + an [[autonomy.override]] must survive.
        std::fs::write(
            &toml,
            "[access]\nlevel = \"home\"\n\n[autonomy]\nlevel = \"assisted\"\n\
             \n[[autonomy.override]]\ndomain = \"email\"\nlevel = \"manual\"\n",
        )
        .unwrap();
        run_autonomy_set_at(&toml, AutonomyLevel::Supervised, true).unwrap();
        let written = std::fs::read_to_string(&toml).unwrap();
        assert!(written.contains("level = \"supervised\""), "{written}");
        assert!(
            written.contains("[access]"),
            "other sections preserved: {written}"
        );
        assert!(
            written.contains("[[autonomy.override]]") && written.contains("email"),
            "overrides must survive a level rewrite: {written}"
        );
    }

    #[test]
    fn show_renders_level_posture_and_overrides() {
        let dir = std::env::temp_dir().join(format!("aivyx-autonomy-show-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let toml = dir.join("aivyx-pa.toml");
        std::fs::write(
            &toml,
            "[autonomy]\nlevel = \"supervised\"\n\
             \n[[autonomy.override]]\ndomain = \"shell\"\nlevel = \"autonomous\"\n\
             \n[autonomy.auto_approve]\nscopes = [\"fs.write\"]\n",
        )
        .unwrap();
        let out = render_autonomy_for_show(&load_config_for_inspection(&toml).unwrap());
        assert!(out.contains("level     = supervised"), "{out}");
        assert!(
            out.contains("loop:armed"),
            "supervised arms the loop: {out}"
        );
        assert!(
            out.contains("[shell] → autonomous"),
            "override shown: {out}"
        );
        assert!(out.contains("fs.write"), "allowlist shown: {out}");
    }

    #[test]
    fn show_explains_each_area_override() {
        let dir = std::env::temp_dir().join(format!("aivyx-autonomy-areas-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let toml = dir.join("aivyx-pa.toml");
        std::fs::write(
            &toml,
            "[autonomy]\nlevel = \"assisted\"\n\
             \n[[autonomy.override]]\ndomain = \"email\"\nlevel = \"manual\"\n\
             \n[[autonomy.override]]\ndomain = \"fs\"\nlevel = \"unleashed\"\n",
        )
        .unwrap();
        let out = render_autonomy_for_show(&load_config_for_inspection(&toml).unwrap());
        assert!(out.contains("[email] → manual (asks before any change)"), "{out}");
        assert!(
            out.contains("[fs] → unleashed (deletes and overwrites run; looser than global)"),
            "{out}"
        );
        assert!(out.contains("areas:") && out.contains("calendar"), "{out}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
