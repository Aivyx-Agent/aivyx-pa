//! `aivyx-pa pack` — Chapter Freight (FR.2): the operator/publisher CLI
//! over the `aivyx-pack` bundle format.
//!
//! - `keygen <keyfile>` — publisher-side keypair (secret 0600; prints
//!   the base64 verifying key operators add to `[pack]
//!   trusted_publishers`).
//! - `build <staging> --key <keyfile> --out <file>` — payload from a
//!   staging dir (`manifest.toml` + `bin/` + `config/`), signed bundle
//!   out.
//! - `inspect <file>` — verify against the trusted set + print the
//!   manifest (`--allow-untrusted` prints anyway, loudly).
//! - `install <file>` — verify → host-target + min-daemon checks →
//!   unpack to `~/.aivyx-pa/packs/<name>/<version>/` → wire the config the
//!   Mise way (`[[tool_process]]` entries; `[team] config_path` only if
//!   absent).
//!
//! `pack update` is deliberately absent until the v1.0 web presence
//! provides a distribution endpoint. See docs/FREIGHT.md.

use std::path::{Path, PathBuf};

use aivyx_config::{AivyxConfig, LoadOptions};
use aivyx_pack::{
    HOST_TARGET, PackError, PackManifest, build_payload, daemon_version_ok, keygen_to_file,
    load_signing_key, read_bundle, read_manifest, unpack_payload, verify_bundle,
};

use super::connect::{append_tool_process, find_aivyx_toml, tool_process_present};
use super::connect_kitchen::set_team_config_path_if_absent;
use crate::PackSubcommand;

/// Entry point for `aivyx-pa pack <subcommand>`.
pub fn run_pack(sub: PackSubcommand) -> Result<(), String> {
    match sub {
        PackSubcommand::Keygen { keyfile } => {
            let path = Path::new(&keyfile);
            if path.exists() {
                return Err(format!(
                    "refusing to overwrite existing keyfile {keyfile:?}"
                ));
            }
            let pubkey = keygen_to_file(path).map_err(|e| e.to_string())?;
            println!("wrote signing key to {keyfile} (keep it secret)");
            println!("verifying key (give to operators):");
            println!("  {pubkey}");
            println!("operators trust it via:\n  [pack]\n  trusted_publishers = [\"{pubkey}\"]");
            Ok(())
        }
        PackSubcommand::Build { staging, key, out } => {
            let signing_key = load_signing_key(Path::new(&key)).map_err(|e| e.to_string())?;
            let payload = build_payload(Path::new(&staging)).map_err(|e| e.to_string())?;
            let manifest = read_manifest(&payload).map_err(|e| e.to_string())?;
            aivyx_pack::write_bundle(&payload, &signing_key, Path::new(&out))
                .map_err(|e| e.to_string())?;
            println!(
                "built {out}: pack {} v{} for {} (min daemon {})",
                manifest.name, manifest.version, manifest.target, manifest.min_daemon_version,
            );
            Ok(())
        }
        PackSubcommand::Inspect {
            file,
            allow_untrusted,
        } => {
            let bundle = read_bundle(Path::new(&file)).map_err(|e| e.to_string())?;
            let trusted = trusted_publishers()?;
            match verify_bundle(&bundle, &trusted) {
                Ok(_) => println!("signature: VERIFIED (trusted publisher)"),
                Err(e @ PackError::UntrustedPublisher { .. }) if allow_untrusted => {
                    println!("signature: NOT TRUSTED — {e}");
                    println!("(--allow-untrusted: printing the manifest anyway)");
                }
                Err(e) => return Err(e.to_string()),
            }
            let manifest = read_manifest(&bundle.payload).map_err(|e| e.to_string())?;
            print!("{}", render_manifest(&manifest));
            Ok(())
        }
        PackSubcommand::Install { file } => install(Path::new(&file)),
    }
}

fn install(file: &Path) -> Result<(), String> {
    let home = dirs_home()?;
    let bundle = read_bundle(file).map_err(|e| e.to_string())?;
    let trusted = trusted_publishers()?;
    verify_bundle(&bundle, &trusted).map_err(|e| e.to_string())?;
    let manifest = read_manifest(&bundle.payload).map_err(|e| e.to_string())?;

    // Host + daemon gates before anything touches disk.
    if manifest.target != HOST_TARGET {
        return Err(PackError::WrongTarget {
            pack: manifest.target.clone(),
            host: HOST_TARGET.to_string(),
        }
        .to_string());
    }
    daemon_version_ok(&manifest.min_daemon_version, env!("CARGO_PKG_VERSION"))
        .map_err(|e| e.to_string())?;

    // Unpack to the versioned install dir.
    let install_dir = home
        .join(".aivyx-pa/packs")
        .join(&manifest.name)
        .join(&manifest.version);
    std::fs::create_dir_all(&install_dir)
        .map_err(|e| format!("create {}: {e}", install_dir.display()))?;
    unpack_payload(&bundle.payload, &install_dir).map_err(|e| e.to_string())?;
    println!("unpacked to {}", install_dir.display());

    // Wire the config the Mise way.
    let toml_path = find_aivyx_toml(&home)
        .ok_or_else(|| "no aivyx-pa.toml found (cwd or home) — run `aivyx-pa init` first".to_string())?;
    let text = std::fs::read_to_string(&toml_path)
        .map_err(|e| format!("read {}: {e}", toml_path.display()))?;
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e| format!("parse {}: {e}", toml_path.display()))?;

    let mut wired = Vec::new();
    for tp in &manifest.tool_processes {
        if tool_process_present(&doc, &tp.name) {
            println!("tool process {:?} already wired — leaving it", tp.name);
            continue;
        }
        let bin = resolve_bin_path(&install_dir, &tp.bin)?;
        if !bin.is_file() {
            return Err(format!(
                "manifest names bin {:?} but the payload didn't provide it",
                tp.bin
            ));
        }
        append_tool_process(&mut doc, &tp.name, &bin.display().to_string());
        if !tp.args.is_empty() {
            set_last_tool_process_args(&mut doc, &tp.args);
        }
        wired.push(tp.name.clone());
    }

    let mut team_set = false;
    if let Some(team_rel) = &manifest.team_config {
        let team_path = resolve_team_config_path(&install_dir, team_rel)?;
        if !team_path.is_file() {
            return Err(format!(
                "manifest names team_config {team_rel:?} but the payload \
                 didn't provide it"
            ));
        }
        team_set = set_team_config_path_if_absent(&mut doc, &team_path.display().to_string());
        if !team_set {
            println!(
                "[team] config_path already set — not clobbering (pack team \
                 config at {})",
                team_path.display()
            );
        }
    }

    std::fs::write(&toml_path, doc.to_string())
        .map_err(|e| format!("write {}: {e}", toml_path.display()))?;

    println!(
        "installed pack {} v{}: {} tool process(es) wired{}; restart the \
         daemon to load it",
        manifest.name,
        manifest.version,
        wired.len(),
        if team_set { ", team config set" } else { "" },
    );
    Ok(())
}

/// Resolve a manifest's `bin` path under `install_dir/bin`. Rejects any
/// `bin` value that would escape `install_dir` (e.g. `../../../../usr/bin/curl`)
/// using the same check that already guards pack archive extraction.
fn resolve_bin_path(install_dir: &Path, bin: &str) -> Result<PathBuf, String> {
    let rel = Path::new(bin);
    if !aivyx_pack::safe_relative(rel) {
        return Err(format!(
            "manifest 'bin' path {bin:?} escapes the install directory"
        ));
    }
    Ok(install_dir.join("bin").join(rel))
}

/// Resolve a manifest's `team_config` path under `install_dir/config`.
/// Rejects any path that would escape `install_dir`.
fn resolve_team_config_path(install_dir: &Path, team_rel: &str) -> Result<PathBuf, String> {
    let rel = Path::new(team_rel);
    if !aivyx_pack::safe_relative(rel) {
        return Err(format!(
            "manifest 'team_config' path {team_rel:?} escapes the install directory"
        ));
    }
    Ok(install_dir.join("config").join(rel))
}

/// The last-appended `[[tool_process]]` gains an `args` array. Split
/// from `append_tool_process` so connect's callers stay untouched.
fn set_last_tool_process_args(doc: &mut toml_edit::DocumentMut, args: &[String]) {
    if let Some(aot) = doc
        .get_mut("tool_process")
        .and_then(|i| i.as_array_of_tables_mut())
    {
        if let Some(last) = aot.iter_mut().last() {
            let mut arr = toml_edit::Array::new();
            for a in args {
                arr.push(a.as_str());
            }
            last["args"] = toml_edit::value(arr);
        }
    }
}

fn trusted_publishers() -> Result<Vec<String>, String> {
    let home = dirs_home()?;
    let toml_path = find_aivyx_toml(&home)
        .ok_or_else(|| "no aivyx-pa.toml found (cwd or home) — run `aivyx-pa init` first".to_string())?;
    let opts = LoadOptions {
        toml_path: Some(toml_path.clone()),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    let cfg = AivyxConfig::load_from_env_and_toml(&opts)
        .map_err(|e| format!("failed to load {}: {e}", toml_path.display()))?;
    Ok(cfg.pack_trusted_publishers)
}

fn dirs_home() -> Result<PathBuf, String> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| "HOME is not set".to_string())
}

/// Pure renderer for `pack inspect`.
pub fn render_manifest(m: &PackManifest) -> String {
    let mut out = format!(
        "pack {} v{}\n  publisher : {}\n  target    : {}\n  min daemon: {}\n",
        m.name, m.version, m.publisher, m.target, m.min_daemon_version,
    );
    for tp in &m.tool_processes {
        out.push_str(&format!(
            "  tool proc : {} (bin/{}{})\n",
            tp.name,
            tp.bin,
            if tp.args.is_empty() {
                String::new()
            } else {
                format!(" {}", tp.args.join(" "))
            },
        ));
    }
    if let Some(tc) = &m.team_config {
        out.push_str(&format!("  team cfg  : config/{tc} (wired if absent)\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> PackManifest {
        PackManifest::parse(
            r#"
name = "kitchen"
version = "1.0.0"
target = "x86_64-unknown-linux-gnu"
min_daemon_version = "0.8.0"
publisher = "Aivyx"
team_config = "kitchen-boh.toml"

[[tool_process]]
name = "kitchen-toolkit"
bin = "aivyx-kitchen-toolkit"
args = ["--flag"]
"#,
        )
        .unwrap()
    }

    #[test]
    fn render_shows_every_wiring_relevant_fact() {
        let out = render_manifest(&manifest());
        assert!(out.contains("pack kitchen v1.0.0"));
        assert!(out.contains("min daemon: 0.8.0"));
        assert!(out.contains("kitchen-toolkit (bin/aivyx-kitchen-toolkit --flag)"));
        assert!(out.contains("config/kitchen-boh.toml"));
    }

    #[test]
    fn team_wiring_renders_a_proper_section_not_an_inline_table() {
        let mut doc: toml_edit::DocumentMut = "".parse().unwrap();
        assert!(set_team_config_path_if_absent(&mut doc, "/x/team.toml"));
        let rendered = doc.to_string();
        assert!(
            rendered.contains("[team]"),
            "must be a [team] section, got: {rendered}"
        );
        // No-clobber half of the Mise rule.
        assert!(!set_team_config_path_if_absent(&mut doc, "/y/other.toml"));
        assert!(doc.to_string().contains("/x/team.toml"));
    }

    #[test]
    fn resolve_bin_path_rejects_a_traversal_bin_path() {
        let install_dir = Path::new("/opt/aivyx-pa/packs/kitchen/1.0.0");
        let result = resolve_bin_path(install_dir, "../../../../usr/bin/curl");
        assert!(result.is_err(), "expected a traversal bin path to be rejected");
    }

    #[test]
    fn resolve_bin_path_accepts_a_normal_bin_path() {
        let install_dir = Path::new("/opt/aivyx-pa/packs/kitchen/1.0.0");
        let result = resolve_bin_path(install_dir, "kitchen-tool");
        assert_eq!(
            result.unwrap(),
            install_dir.join("bin").join("kitchen-tool")
        );
    }

    #[test]
    fn resolve_team_config_path_rejects_a_traversal_path() {
        let install_dir = Path::new("/opt/aivyx-pa/packs/kitchen/1.0.0");
        let result = resolve_team_config_path(install_dir, "../../../../etc/passwd");
        assert!(
            result.is_err(),
            "expected a traversal team_config path to be rejected"
        );
    }

    #[test]
    fn resolve_team_config_path_accepts_a_normal_path() {
        let install_dir = Path::new("/opt/aivyx-pa/packs/kitchen/1.0.0");
        let result = resolve_team_config_path(install_dir, "kitchen-boh.toml");
        assert_eq!(
            result.unwrap(),
            install_dir.join("config").join("kitchen-boh.toml")
        );
    }

    #[test]
    fn args_land_on_the_appended_tool_process() {
        let mut doc: toml_edit::DocumentMut = "".parse().unwrap();
        append_tool_process(&mut doc, "kitchen-toolkit", "/x/bin/tk");
        set_last_tool_process_args(&mut doc, &["--a".to_string(), "--b".to_string()]);
        let rendered = doc.to_string();
        assert!(rendered.contains("name = \"kitchen-toolkit\""));
        assert!(rendered.contains("args = [\"--a\", \"--b\"]"));
    }
}
