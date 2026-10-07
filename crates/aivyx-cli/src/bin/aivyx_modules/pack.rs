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

use aivyx_instance::{BaseDirs, InstanceName, InstancePaths};
use aivyx_pack::{ConfigPackManifest, Manifest, read_any_manifest};

use super::connect::{append_tool_process, find_aivyx_toml, tool_process_present};
use super::init_templates::{Template, TemplateSource};
use super::pack_check::{PaPartSummary, check_pa_part};
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
            let manifest = read_any_manifest(&payload).map_err(|e| e.to_string())?;
            aivyx_pack::write_bundle(&payload, &signing_key, Path::new(&out))
                .map_err(|e| e.to_string())?;
            match &manifest {
                Manifest::Binary(m) => println!(
                    "built {out}: pack {} v{} for {} (min daemon {})",
                    m.name, m.version, m.target, m.min_daemon_version,
                ),
                Manifest::Config(_) => println!(
                    "built {out}: pack {} v{} ({})",
                    manifest.name(),
                    manifest.version(),
                    aivyx_pack::describe::kind(&manifest)
                ),
            }
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
            match read_any_manifest(&bundle.payload).map_err(|e| e.to_string())? {
                Manifest::Binary(manifest) => print!("{}", render_manifest(&manifest)),
                Manifest::Config(manifest) => {
                    let check = manifest.pa.as_ref().map(|_| check_payload(&bundle.payload));
                    print!("{}", render_config_inspect(&Manifest::Config(manifest), check.as_ref()));
                }
            }
            Ok(())
        }
        PackSubcommand::Install { file } => install(Path::new(&file)),
        PackSubcommand::Check { dir } => crate::pack_check::run_check(Path::new(&dir)),
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
    let toml_path = find_aivyx_toml()
        .ok_or_else(|| "no aivyx-pa.toml found — run `aivyx-pa init` first".to_string())?;
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

// ---------------------------------------------------------------------------
// Config packs (format 2): a pack becomes a new agent
// ---------------------------------------------------------------------------

/// Everything decided before the setup wizard runs.
pub struct ConfigInstall {
    pub instance: InstanceName,
    /// `…/packs/<name>/<version>`, the unpacked pack.
    pub pack_dir: PathBuf,
    /// The pack's template with the installer's keys spliced in.
    pub template: Template,
    pub summary: PaPartSummary,
}

/// The instance a config pack installs as: the selected one, or — when
/// none was chosen (`default`) — one named after the pack.
fn target_instance(
    manifest: &ConfigPackManifest,
    selected: &InstanceName,
) -> Result<InstanceName, String> {
    if selected.is_default() {
        InstanceName::parse(&manifest.name).map_err(|e| {
            format!(
                "the pack's name `{}` can't be an instance name ({e}) — pick one with \
                 `aivyx-pa --instance <name> pack install …`",
                manifest.name
            )
        })
    } else {
        Ok(selected.clone())
    }
}

/// For `main`, before the runtime exists: the instance a config pack will
/// install as, or `None` for a tool pack (installed the old way).
pub fn config_pack_target(
    file: &Path,
    selected: &InstanceName,
) -> Result<Option<InstanceName>, String> {
    let bundle = read_bundle(file).map_err(|e| e.to_string())?;
    match read_any_manifest(&bundle.payload).map_err(|e| e.to_string())? {
        Manifest::Binary(_) => Ok(None),
        Manifest::Config(m) => target_instance(&m, selected).map(Some),
    }
}

/// Set `[table] key = value`, creating the table as a `[table]` header.
fn set_key(doc: &mut toml_edit::DocumentMut, table: &str, key: &str, value: &str) {
    if !doc.contains_table(table) {
        doc[table] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    doc[table][key] = toml_edit::value(value);
}

/// Verify-free preparation (the caller verified `payload`): check the
/// version and the target instance, unpack into the instance's packs dir
/// via a temporary folder, run the product checks, and splice the
/// template. Leaves nothing behind on failure.
pub fn prepare_config_install(
    payload: &[u8],
    manifest: &ConfigPackManifest,
    selected: &InstanceName,
    dirs: &BaseDirs,
    existing: &[InstanceName],
) -> Result<ConfigInstall, String> {
    let pa = manifest
        .pa
        .as_ref()
        .ok_or("this pack has no aivyx-pa part — it's for aivyx-coder")?;
    daemon_version_ok(&pa.min_version, env!("CARGO_PKG_VERSION")).map_err(|e| e.to_string())?;
    let instance = target_instance(manifest, selected)?;
    if instance.is_default() {
        return Err("a pack installs as a new agent, not into `default` — pick a name with \
                    `aivyx-pa --instance <name> pack install …`"
            .into());
    }
    if existing.contains(&instance) {
        return Err(format!(
            "instance `{instance}` already exists — pick another with \
             `aivyx-pa --instance <name> pack install …`"
        ));
    }

    let packs = InstancePaths::new(instance.clone(), dirs.clone())
        .packs_dir()
        .ok_or("can't resolve the instance's folder (is HOME set?)")?
        .join(&manifest.name);
    let tmp = packs.join(format!(
        ".tmp-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let pack_dir = packs.join(&manifest.version);
    let staged = (|| -> Result<PaPartSummary, String> {
        std::fs::create_dir_all(&tmp).map_err(|e| format!("create {}: {e}", tmp.display()))?;
        unpack_payload(payload, &tmp).map_err(|e| e.to_string())?;
        let summary = check_pa_part(&tmp).map_err(|problems| {
            format!("the pack doesn't pass its checks:\n  - {}", problems.join("\n  - "))
        })?;
        if pack_dir.exists() {
            std::fs::remove_dir_all(&pack_dir)
                .map_err(|e| format!("remove {}: {e}", pack_dir.display()))?;
        }
        std::fs::rename(&tmp, &pack_dir)
            .map_err(|e| format!("move into {}: {e}", pack_dir.display()))?;
        Ok(summary)
    })();
    let summary = match staged {
        Ok(s) => s,
        Err(e) => {
            let _ = std::fs::remove_dir_all(&tmp);
            remove_if_empty(&packs);
            return Err(e);
        }
    };

    let spliced = (|| -> Result<Template, String> {
        let text = std::fs::read_to_string(pack_dir.join(&pa.template))
            .map_err(|e| format!("read the template: {e}"))?;
        let mut doc: toml_edit::DocumentMut =
            text.parse().map_err(|e| format!("the template isn't valid TOML: {e}"))?;
        set_key(&mut doc, "pack", "source", &format!("{}@{}", manifest.name, manifest.version));
        if let Some(rel) = &pa.team_config {
            set_key(&mut doc, "team", "config_path", &pack_dir.join(rel).display().to_string());
        }
        if let Some(rel) = &pa.skills {
            set_key(
                &mut doc,
                "skill_defaults",
                "project_dir",
                &pack_dir.join(rel).display().to_string(),
            );
        }
        Ok(Template {
            name: manifest.name.clone(),
            description: format!("pack {} v{}", manifest.name, manifest.version),
            source: TemplateSource::Pack,
            toml_content: doc.to_string(),
        })
    })();
    match spliced {
        Ok(template) => Ok(ConfigInstall { instance, pack_dir, template, summary }),
        Err(e) => {
            let _ = std::fs::remove_dir_all(&pack_dir);
            remove_if_empty(&packs);
            Err(e)
        }
    }
}

/// Remove `dir` and its now-empty parents up to the instance home, so a
/// failed install leaves no empty `packs/<name>` behind.
fn remove_if_empty(dir: &Path) {
    let mut d = Some(dir);
    while let Some(p) = d {
        if std::fs::remove_dir(p).is_err() {
            break;
        }
        d = p.parent().filter(|p| p.file_name().is_some_and(|n| n == "packs"));
    }
}

/// Publishers trusted for a new agent: the default instance's `[pack]
/// trusted_publishers` (the operator's existing setup) when it has a
/// config; the compiled-in keys always apply.
fn install_trusted_publishers(dirs: &BaseDirs) -> Result<Vec<String>, String> {
    let Some(config) = InstancePaths::new(InstanceName::default_instance(), dirs.clone())
        .config_file()
        .filter(|c| c.exists())
    else {
        return Ok(Vec::new());
    };
    let opts = LoadOptions {
        toml_path: Some(config.clone()),
        require_api_key: false,
        require_telegram_token: false,
        require_discord_token: false,
        require_slack_tokens: false,
        role_override: None,
    };
    AivyxConfig::load_from_env_and_toml(&opts)
        .map(|cfg| cfg.pack_trusted_publishers)
        .map_err(|e| format!("failed to load {}: {e}", config.display()))
}

/// What to do after a config pack installs.
pub fn next_steps(instance: &InstanceName, summary: &PaPartSummary, port: u16) -> String {
    let m = &summary.manifest;
    let pa = m.pa.as_ref().expect("checked: has a pa part");
    let mut out = format!(
        "Installed {} v{} as instance `{instance}` (Studio port {port}).\nNext:\n",
        m.name, m.version
    );
    let connect: Vec<&str> = pa
        .requires
        .iter()
        .map(String::as_str)
        .filter(|r| crate::pack_check::CONNECTABLE.contains(r))
        .collect();
    if let Some((first, rest)) = connect.split_first() {
        out.push_str(&format!("  aivyx-pa --instance {instance} connect {first}"));
        if !rest.is_empty() {
            out.push_str(&format!("      (also: {})", rest.join(", ")));
        }
        out.push('\n');
    }
    for r in pa.requires.iter().filter(|r| !connect.contains(&r.as_str())) {
        out.push_str(&format!("  add the {r} as a [[tool_process]] — see docs/INSTALL.md\n"));
    }
    if !pa.optional.is_empty() {
        out.push_str(&format!("  optional: {}\n", pa.optional.join(", ")));
    }
    out.push_str(&format!(
        "Start it with `aivyx-pa --instance {instance} daemon run` \
         (or `daemon install` to run it as a service).\n"
    ));
    out
}

/// `aivyx-pa pack install <file>` for a config pack. `main` has already
/// selected the target instance (`AIVYX_PA_INSTANCE`) so the setup wizard
/// writes the new agent's config, store and keyring entry.
pub async fn run_config_install(file: &Path) -> Result<(), String> {
    let dirs = BaseDirs::from_process();
    let bundle = read_bundle(file).map_err(|e| e.to_string())?;
    verify_bundle(&bundle, &install_trusted_publishers(&dirs)?).map_err(|e| e.to_string())?;
    let Manifest::Config(manifest) = read_any_manifest(&bundle.payload).map_err(|e| e.to_string())?
    else {
        return Err("this is a tool pack; install it into an existing agent".into());
    };
    let selected = InstanceName::from_env()?;
    let existing = aivyx_instance::list_instances(&dirs);
    let install = prepare_config_install(&bundle.payload, &manifest, &selected, &dirs, &existing)?;
    println!(
        "Setting up `{}` from pack {} v{}.\n",
        install.instance, manifest.name, manifest.version
    );

    let config = InstancePaths::new(install.instance.clone(), dirs.clone()).config_file();
    let undo = |why: String| -> String {
        let _ = std::fs::remove_dir_all(&install.pack_dir);
        if let Some(parent) = install.pack_dir.parent() {
            remove_if_empty(parent);
        }
        why
    };
    crate::init::run_init_wizard(Some(&install.template)).await.map_err(undo)?;
    let Some(config) = config.filter(|c| c.exists()) else {
        return Err(undo("setup didn't finish, so nothing was installed".into()));
    };
    let port = crate::instances::pick_port();
    crate::instances::write_port(&config, port)?;
    print!("\n{}", next_steps(&install.instance, &install.summary, port));
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
    let toml_path = find_aivyx_toml()
        .ok_or_else(|| "no aivyx-pa.toml found — run `aivyx-pa init` first".to_string())?;
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

/// Unpack a config pack to a scratch folder and run the aivyx-pa checks on
/// it, so `inspect` can show routines and autonomy and flag problems
/// before anything is installed.
fn check_payload(payload: &[u8]) -> Result<PaPartSummary, Vec<String>> {
    let dir = std::env::temp_dir().join(format!(
        "aivyx-pa-inspect-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let result = unpack_payload(payload, &dir)
        .map_err(|e| vec![e.to_string()])
        .and_then(|()| check_pa_part(&dir));
    let _ = std::fs::remove_dir_all(&dir);
    result
}

/// `pack inspect` for a config pack: the shared description, then — for
/// an aivyx-pa part — what the checks found.
pub fn render_config_inspect(
    manifest: &Manifest,
    check: Option<&Result<PaPartSummary, Vec<String>>>,
) -> String {
    let mut out = aivyx_pack::describe::describe(manifest);
    match check {
        None => {}
        Some(Ok(s)) => out.push_str(&format!(
            "  routines: {}\n  autonomy: {}\n  checks: OK\n",
            s.routines,
            super::pack_check::autonomy_line(s)
        )),
        Some(Err(problems)) => {
            out.push_str(&format!(
                "  checks: {} problem{} — this pack won't install\n",
                problems.len(),
                if problems.len() == 1 { "" } else { "s" }
            ));
            for p in problems {
                out.push_str(&format!("    - {p}\n"));
            }
        }
    }
    out
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

    // ---- Config packs (format 2) ----

    use crate::pack_check::tests::{good_pack, rewrite, tmp};

    fn base_dirs(tag: &str) -> BaseDirs {
        let home = tmp(tag);
        BaseDirs { home: Some(home), xdg_config_home: None, xdg_data_home: None, xdg_runtime_dir: None }
    }

    fn config_payload(dir: &Path) -> (Vec<u8>, ConfigPackManifest) {
        let payload = build_payload(dir).unwrap();
        let Manifest::Config(m) = read_any_manifest(&payload).unwrap() else { panic!("config pack") };
        (payload, m)
    }

    fn name(n: &str) -> InstanceName {
        InstanceName::parse(n).unwrap()
    }

    fn packs_root(dirs: &BaseDirs, instance: &str) -> PathBuf {
        InstancePaths::new(name(instance), dirs.clone()).packs_dir().unwrap()
    }

    #[test]
    fn a_config_pack_prepares_a_new_instance() {
        let (payload, m) = config_payload(&good_pack("prep"));
        let dirs = base_dirs("prep-home");
        let c = prepare_config_install(&payload, &m, &InstanceName::default_instance(), &dirs, &[])
            .unwrap();
        assert_eq!(c.instance.as_str(), "business-manager");
        assert!(c.pack_dir.ends_with("instances/business-manager/packs/business-manager/0.1.0"));
        assert!(c.pack_dir.join("pa/skills/example/SKILL.md").is_file());
        let doc: toml_edit::DocumentMut = c.template.toml_content.parse().unwrap();
        assert_eq!(doc["pack"]["source"].as_str(), Some("business-manager@0.1.0"));
        assert!(doc["team"]["config_path"].as_str().unwrap().ends_with("0.1.0/pa/team.toml"));
        assert!(doc["skill_defaults"]["project_dir"].as_str().unwrap().ends_with("0.1.0/pa/skills"));
        assert_eq!(doc["profile"]["assistant_name"].as_str(), Some("Manager"), "the rest is kept");
        assert_eq!(c.template.source, TemplateSource::Pack);
        let leftovers: Vec<_> = std::fs::read_dir(c.pack_dir.parent().unwrap())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "no temporary folder left");
    }

    #[test]
    fn the_selected_instance_name_wins() {
        let (payload, m) = config_payload(&good_pack("selected"));
        let dirs = base_dirs("selected-home");
        let c = prepare_config_install(&payload, &m, &name("shop"), &dirs, &[]).unwrap();
        assert_eq!(c.instance.as_str(), "shop");
        assert!(c.pack_dir.starts_with(packs_root(&dirs, "shop")));
    }

    #[test]
    fn an_existing_instance_is_refused_and_nothing_is_left() {
        let (payload, m) = config_payload(&good_pack("existing"));
        let dirs = base_dirs("existing-home");
        let err = prepare_config_install(
            &payload,
            &m,
            &InstanceName::default_instance(),
            &dirs,
            &[name("business-manager")],
        )
        .err()
        .unwrap();
        assert!(err.contains("already exists"), "{err}");
        assert!(!packs_root(&dirs, "business-manager").exists());
    }

    #[test]
    fn a_pack_that_fails_its_checks_leaves_nothing() {
        let dir = good_pack("failing");
        rewrite(&dir, "pa/aivyx-pa.toml", |t| {
            t.replace(r#"level = "assisted""#, r#"level = "unleashed""#)
        });
        let (payload, m) = config_payload(&dir);
        let dirs = base_dirs("failing-home");
        let err = prepare_config_install(&payload, &m, &InstanceName::default_instance(), &dirs, &[])
            .err()
            .unwrap();
        assert!(err.contains("supervised"), "{err}");
        assert!(
            !packs_root(&dirs, "business-manager").join("business-manager").exists(),
            "the pack folder was removed"
        );
    }

    #[test]
    fn a_pack_needing_a_newer_aivyx_pa_is_refused() {
        let dir = good_pack("newer");
        rewrite(&dir, "manifest.toml", |t| t.replace(r#"min_version = "0.17.0""#, r#"min_version = "99.0.0""#));
        let (payload, m) = config_payload(&dir);
        let err = prepare_config_install(&payload, &m, &InstanceName::default_instance(), &base_dirs("newer-home"), &[])
            .err()
            .unwrap();
        assert!(err.contains("upgrade"), "{err}");
    }

    #[test]
    fn a_coder_only_pack_is_explained() {
        let dir = tmp("coderonly");
        std::fs::create_dir_all(dir.join("coder")).unwrap();
        std::fs::write(dir.join("coder/AGENTS.md"), "# Instructions\n").unwrap();
        std::fs::write(
            dir.join("manifest.toml"),
            "format = 2\nname = \"c\"\nversion = \"0.1.0\"\npublisher = \"A\"\nproducts = [\"coder\"]\n\n\
             [coder]\nmin_version = \"0.5.0\"\nagents_file = \"coder/AGENTS.md\"\n",
        )
        .unwrap();
        let (payload, m) = config_payload(&dir);
        let err = prepare_config_install(&payload, &m, &InstanceName::default_instance(), &base_dirs("coderonly-home"), &[])
            .err()
            .unwrap();
        assert!(err.contains("no aivyx-pa part"), "{err}");
    }

    #[test]
    fn config_pack_target_tells_tool_packs_apart() {
        let dir = tmp("target");
        let key = dir.join("key.bin");
        aivyx_pack::keygen_to_file(&key).unwrap();
        let signing = load_signing_key(&key).unwrap();
        let bundle = dir.join("bm.aivyxpack");
        aivyx_pack::write_bundle(&build_payload(&good_pack("target-pack")).unwrap(), &signing, &bundle)
            .unwrap();
        assert_eq!(
            config_pack_target(&bundle, &InstanceName::default_instance()).unwrap(),
            Some(name("business-manager"))
        );
        assert_eq!(config_pack_target(&bundle, &name("shop")).unwrap(), Some(name("shop")));

        let tool = tmp("target-tool");
        std::fs::create_dir_all(tool.join("bin")).unwrap();
        std::fs::write(
            tool.join("manifest.toml"),
            "name = \"k\"\nversion = \"1.0.0\"\ntarget = \"x\"\nmin_daemon_version = \"0.8.0\"\npublisher = \"A\"\n",
        )
        .unwrap();
        let tool_bundle = dir.join("k.aivyxpack");
        aivyx_pack::write_bundle(&build_payload(&tool).unwrap(), &signing, &tool_bundle).unwrap();
        assert_eq!(config_pack_target(&tool_bundle, &InstanceName::default_instance()).unwrap(), None);
    }

    #[test]
    fn inspect_shows_a_config_packs_routines_and_autonomy() {
        let (payload, m) = config_payload(&good_pack("inspect"));
        let text = render_config_inspect(&Manifest::Config(m), Some(&check_payload(&payload)));
        assert!(text.contains("kind:        config pack"), "{text}");
        assert!(text.contains("  requires: gmail\n"), "{text}");
        assert!(text.contains("  routines: 1\n  autonomy: assisted (fs: supervised)\n  checks: OK"), "{text}");

        let bad = good_pack("inspect-bad");
        rewrite(&bad, "pa/aivyx-pa.toml", |t| t.replace(r#"level = "assisted""#, r#"level = "unleashed""#));
        let (payload, m) = config_payload(&bad);
        let text = render_config_inspect(&Manifest::Config(m), Some(&check_payload(&payload)));
        assert!(text.contains("checks: 1 problem — this pack won't install"), "{text}");
    }

    #[test]
    fn next_steps_say_what_to_connect() {
        let summary = check_pa_part(&good_pack("next")).unwrap();
        let text = next_steps(&name("shop"), &summary, 7845);
        assert!(text.starts_with("Installed business-manager v0.1.0 as instance `shop` (Studio port 7845)."), "{text}");
        assert!(text.contains("aivyx-pa --instance shop connect gmail\n"), "{text}");
        assert!(text.contains("optional: notion"), "{text}");
        assert!(text.contains("aivyx-pa --instance shop daemon run"), "{text}");
    }

    /// A config pack (format 2) needs a newer aivyx-pa: building one and
    /// reading it back the way `inspect`/`install` do refuses it clearly.
    #[test]
    fn a_config_pack_is_refused_with_a_clear_message() {
        let dir = std::env::temp_dir().join(format!("aivyx-pa-cfgpack-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("pa")).unwrap();
        std::fs::write(
            dir.join("manifest.toml"),
            "format = 2\nname = \"bm\"\nversion = \"0.1.0\"\npublisher = \"Aivyx\"\n\
             products = [\"pa\"]\n\n[pa]\nmin_version = \"0.18.0\"\ntemplate = \"pa/aivyx-pa.toml\"\n",
        )
        .unwrap();
        std::fs::write(dir.join("pa/aivyx-pa.toml"), "[profile]\n").unwrap();
        let payload = build_payload(&dir).unwrap();
        let err = read_manifest(&payload).unwrap_err().to_string();
        assert!(err.contains("format 2"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

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
