//! `aivyx-pa instances list | create <name> | remove <name>` — named
//! instances: several fully separate agents for one OS user, each with its
//! own config, store, socket, working dirs, keyring entry, service and
//! Studio port (`docs/amendments/2026-10-05-named-instances.md`). Every
//! path comes from `aivyx_instance::InstancePaths`.

use std::path::{Path, PathBuf};

use aivyx_instance::{BaseDirs, InstanceName, InstancePaths, list_instances};

/// The default instance's Studio port (`[daemon] web_ui_port` unset).
const DEFAULT_PORT: u16 = aivyx_channel::web_ui::DEFAULT_WEB_UI_PORT;
/// Named instances get the lowest free port from here up.
const FIRST_NAMED_PORT: u16 = 7844;

/// The lowest port ≥ [`FIRST_NAMED_PORT`] that no instance's config already
/// uses and nothing is listening on.
pub fn choose_port(used: &[u16], busy: &[u16]) -> u16 {
    (FIRST_NAMED_PORT..=u16::MAX)
        .find(|p| !used.contains(p) && !busy.contains(p))
        .unwrap_or(FIRST_NAMED_PORT)
}

/// `default` can't be removed, and neither can an instance whose daemon is
/// running.
pub fn check_removable(name: &InstanceName, running: bool) -> Result<(), String> {
    if name.is_default() {
        return Err("the default instance can't be removed".into());
    }
    if running {
        return Err(format!(
            "instance `{name}` is running — stop it first: aivyx-pa --instance {name} daemon stop"
        ));
    }
    Ok(())
}

/// Every directory `remove` would delete for `paths`: those that exist, in
/// a stable order (config, data, runtime, home working dir, sandbox).
pub fn removal_plan(paths: &InstancePaths) -> Vec<PathBuf> {
    [
        paths.config_dir(),
        paths.data_dir(),
        paths.runtime_dir(),
        paths.home_dir(),
        paths.sandbox_dir(),
    ]
    .into_iter()
    .flatten()
    .filter(|p| p.exists())
    .fold(Vec::new(), |mut acc, p| {
        if !acc.contains(&p) {
            acc.push(p);
        }
        acc
    })
}

/// The Studio port an instance's config sets, or the default port when it
/// sets none (or can't be read).
fn configured_port(paths: &InstancePaths) -> u16 {
    paths
        .config_file()
        .and_then(|f| std::fs::read_to_string(f).ok())
        .and_then(|text| text.parse::<toml_edit::DocumentMut>().ok())
        .and_then(|doc| doc.get("daemon")?.get("web_ui_port")?.as_integer())
        .and_then(|p| u16::try_from(p).ok())
        .unwrap_or(DEFAULT_PORT)
}

/// Write `[daemon] web_ui_port = <port>` into `config`, keeping everything
/// else (comments included) as it is.
pub(crate) fn write_port(config: &Path, port: u16) -> Result<(), String> {
    let text = std::fs::read_to_string(config)
        .map_err(|e| format!("read {}: {e}", config.display()))?;
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e| format!("parse {}: {e}", config.display()))?;
    if doc.get("daemon").is_none() {
        doc["daemon"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    doc["daemon"]["web_ui_port"] = toml_edit::value(i64::from(port));
    std::fs::write(config, doc.to_string()).map_err(|e| format!("write {}: {e}", config.display()))
}

fn port_is_busy(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_err()
}

async fn is_running(paths: &InstancePaths) -> bool {
    match paths.socket_path() {
        Some(socket) => aivyx_channel::daemon_client::daemon_status(&socket).await.running,
        None => false,
    }
}

fn paths_for(name: InstanceName) -> InstancePaths {
    InstancePaths::new(name, BaseDirs::from_process())
}

/// `aivyx-pa instances list`.
pub async fn run_list() -> Result<(), String> {
    let dirs = BaseDirs::from_process();
    let names = list_instances(&dirs);
    if names.is_empty() {
        println!("No instances yet. Run `aivyx-pa init` to set up the default one.");
        return Ok(());
    }
    println!("{:<20} {:<9} {:<7} {:<24} CONFIG", "INSTANCE", "RUNNING", "STUDIO", "PACK");
    for name in names {
        let paths = paths_for(name.clone());
        let running = if is_running(&paths).await { "yes" } else { "no" };
        let config = paths
            .config_file()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "-".into());
        let pack = paths
            .config_file()
            .and_then(|f| std::fs::read_to_string(f).ok())
            .map(|text| pack_source_of(&text))
            .unwrap_or_else(|| "-".into());
        println!(
            "{:<20} {:<9} {:<7} {:<24} {config}",
            name.as_str(),
            running,
            configured_port(&paths),
            pack
        );
    }
    Ok(())
}

/// The config pack an instance came from (`[pack] source`), or `-`.
pub fn pack_source_of(config_text: &str) -> String {
    config_text
        .parse::<toml_edit::DocumentMut>()
        .ok()
        .and_then(|doc| doc.get("pack")?.get("source")?.as_str().map(str::to_string))
        .unwrap_or_else(|| "-".into())
}

/// The ports every existing instance's config uses.
fn used_ports(dirs: &BaseDirs) -> Vec<u16> {
    list_instances(dirs)
        .into_iter()
        .map(|n| configured_port(&InstancePaths::new(n, dirs.clone())))
        .collect()
}

/// The Studio port for a new named instance: the lowest one no instance
/// uses and nothing is listening on.
pub(crate) fn pick_port() -> u16 {
    let used = used_ports(&BaseDirs::from_process());
    let busy: Vec<u16> = (FIRST_NAMED_PORT..FIRST_NAMED_PORT + 64)
        .filter(|p| port_is_busy(*p))
        .collect();
    choose_port(&used, &busy)
}

/// Validate `name` for `create`: not `default`, not already an instance.
pub fn check_creatable(name: &InstanceName, existing: &[InstanceName]) -> Result<(), String> {
    if name.is_default() {
        return Err("`default` is reserved — it's the instance you get without --instance".into());
    }
    if existing.contains(name) {
        return Err(format!("instance `{name}` already exists — see `aivyx-pa instances list`"));
    }
    Ok(())
}

/// `aivyx-pa instances create <name>`: picks a Studio port, runs the
/// normal setup wizard for the new instance, then records the port in its
/// config. The caller has already selected `name` as the process's
/// instance (`AIVYX_PA_INSTANCE`), so the wizard writes the new instance's
/// config, store and keyring entry.
pub async fn run_create(name: &InstanceName) -> Result<(), String> {
    let dirs = BaseDirs::from_process();
    check_creatable(name, &list_instances(&dirs))?;
    let port = pick_port();
    println!("Setting up instance `{name}` (Studio port {port}).\n");

    crate::init::run_init_wizard(None).await?;

    let paths = paths_for(name.clone());
    let config = paths
        .config_file()
        .ok_or("can't resolve the new instance's config path (is HOME set?)")?;
    if config.exists() {
        write_port(&config, port)?;
    }
    println!(
        "\nInstance `{name}` is ready. Use it with `aivyx-pa --instance {name} …`;\n\
         its Studio runs on http://127.0.0.1:{port}."
    );
    Ok(())
}

/// `aivyx-pa instances remove <name>`: refuses `default` and a running
/// instance, shows exactly what will go, and needs the name typed back.
pub async fn run_remove(name: &InstanceName) -> Result<(), String> {
    let paths = paths_for(name.clone());
    check_removable(name, is_running(&paths).await)?;
    let unit = paths.systemd_unit();
    let installed_unit = crate::daemon_service::installed_unit_path_for(&paths);
    if let Some(unit_path) = installed_unit {
        return Err(format!(
            "instance `{name}` still has an installed service ({}) — remove it first: \
             aivyx-pa --instance {name} daemon uninstall",
            unit_path.display()
        ));
    }
    let plan = removal_plan(&paths);
    println!("Removing instance `{name}` deletes:");
    for dir in &plan {
        println!("  {}", dir.display());
    }
    println!("  its saved passphrase in the OS keyring (if any)");
    println!("Its service name would be {unit} (not installed).");
    print!("\nType the instance name to confirm: ");
    use std::io::Write as _;
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    std::io::stdin()
        .read_line(&mut answer)
        .map_err(|e| format!("read confirmation: {e}"))?;
    if answer.trim() != name.as_str() {
        return Err("names didn't match — nothing was removed".into());
    }
    for dir in &plan {
        std::fs::remove_dir_all(dir).map_err(|e| format!("remove {}: {e}", dir.display()))?;
    }
    let _ = aivyx_channel::keyring_store::clear_for(&paths);
    println!("Instance `{name}` removed.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(n: &str) -> InstanceName {
        InstanceName::parse(n).unwrap()
    }

    #[test]
    fn pack_source_is_read_from_the_config() {
        assert_eq!(pack_source_of("[pack]\nsource = \"bm@0.1.0\"\n"), "bm@0.1.0");
        assert_eq!(pack_source_of("[profile]\n"), "-");
        assert_eq!(pack_source_of("not = [toml"), "-");
    }

    #[test]
    fn port_choice_skips_used_and_listening_ports() {
        assert_eq!(choose_port(&[7843], &[]), 7844);
        assert_eq!(choose_port(&[7843, 7844], &[7845]), 7846);
    }

    #[test]
    fn remove_refuses_default_and_running() {
        assert!(check_removable(&InstanceName::default_instance(), false).is_err());
        assert!(check_removable(&name("research"), true).is_err());
        assert!(check_removable(&name("research"), false).is_ok());
    }

    #[test]
    fn create_refuses_default_and_existing() {
        let existing = vec![InstanceName::default_instance(), name("research")];
        assert!(check_creatable(&InstanceName::default_instance(), &existing).is_err());
        assert!(check_creatable(&name("research"), &existing).is_err());
        assert!(check_creatable(&name("household"), &existing).is_ok());
    }

    #[test]
    fn removal_plan_lists_only_the_instances_own_existing_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = BaseDirs {
            home: Some(tmp.path().to_path_buf()),
            xdg_config_home: None,
            xdg_data_home: None,
            xdg_runtime_dir: None,
        };
        let research = InstancePaths::new(name("research"), dirs.clone());
        let default = InstancePaths::new(InstanceName::default_instance(), dirs);
        for d in [
            research.config_dir().unwrap(),
            research.data_dir().unwrap(),
            research.home_dir().unwrap(),
            research.sandbox_dir().unwrap(),
            default.config_dir().unwrap(),
            default.home_dir().unwrap(),
        ] {
            std::fs::create_dir_all(d).unwrap();
        }
        let plan = removal_plan(&research);
        let h = tmp.path();
        assert_eq!(
            plan,
            vec![
                h.join(".config/aivyx-pa/instances/research"),
                h.join(".local/share/aivyx-pa/instances/research"),
                h.join(".aivyx-pa/instances/research"),
                h.join("aivyx-pa-sandbox-research"),
            ]
        );
        assert!(!plan.contains(&default.config_dir().unwrap()));
    }

    #[test]
    fn write_port_sets_the_port_and_keeps_the_rest() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("aivyx-pa.toml");
        std::fs::write(&cfg, "# mine\n[agent]\nmodel = \"m\"\n").unwrap();
        write_port(&cfg, 7850).unwrap();
        let text = std::fs::read_to_string(&cfg).unwrap();
        assert!(text.contains("# mine"), "{text}");
        assert!(text.contains("model = \"m\""), "{text}");
        assert!(text.contains("web_ui_port = 7850"), "{text}");
    }

    #[test]
    fn configured_port_defaults_and_reads() {
        let tmp = tempfile::tempdir().unwrap();
        let dirs = BaseDirs {
            home: Some(tmp.path().to_path_buf()),
            xdg_config_home: None,
            xdg_data_home: None,
            xdg_runtime_dir: None,
        };
        let r = InstancePaths::new(name("research"), dirs);
        assert_eq!(configured_port(&r), DEFAULT_PORT);
        let cfg = r.config_file().unwrap();
        std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
        std::fs::write(&cfg, "[daemon]\nweb_ui_port = 7851\n").unwrap();
        assert_eq!(configured_port(&r), 7851);
    }
}
