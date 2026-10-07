//! Per-instance path and name resolver for Aivyx PA.
//!
//! Supports running multiple independent Aivyx PA instances under one OS user.
//! The default instance uses the standard XDG paths; named instances nest
//! directories under `/instances/<name>` variants.

use std::path::PathBuf;

pub const ENV_INSTANCE: &str = "AIVYX_PA_INSTANCE";
pub const DEFAULT_INSTANCE: &str = "default";

/// Instance name — validated alphanumeric + hyphens, max 32 chars.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstanceName(String);

impl InstanceName {
    /// Parse and validate an instance name.
    /// Allows "default" and alphanumeric + hyphens (1-32 chars, no leading/trailing hyphens).
    pub fn parse(s: &str) -> Result<Self, String> {
        if s.is_empty() || s.len() > 32 {
            return Err(format!(
                "instance name must be 1-32 characters, got {}",
                s.len()
            ));
        }

        if s.starts_with('-') || s.ends_with('-') {
            return Err("instance name cannot start or end with hyphen".to_string());
        }

        // Check for valid characters: alphanumeric and hyphens only
        if !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return Err(format!(
                "instance name must contain only lowercase alphanumeric and hyphens, got {}",
                s
            ));
        }

        // Check if all lowercase (numbers and hyphens are fine, but letters must be lowercase)
        if s.chars().any(|c| c.is_ascii_uppercase()) {
            return Err(format!("instance name must be lowercase, got {}", s));
        }

        Ok(InstanceName(s.to_string()))
    }

    /// The default instance.
    pub fn default_instance() -> Self {
        InstanceName(DEFAULT_INSTANCE.to_string())
    }

    /// Get the instance name as a string.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Check if this is the default instance.
    pub fn is_default(&self) -> bool {
        self.0 == DEFAULT_INSTANCE
    }

    /// Parse instance name from the `AIVYX_PA_INSTANCE` environment variable.
    /// Unset or empty env var returns the default instance.
    pub fn from_env() -> Result<Self, String> {
        match std::env::var(ENV_INSTANCE) {
            Ok(s) if !s.is_empty() => Self::parse(&s),
            _ => Ok(Self::default_instance()),
        }
    }
}

impl std::fmt::Display for InstanceName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Base directory paths from the environment.
#[derive(Clone, Debug)]
pub struct BaseDirs {
    pub home: Option<PathBuf>,
    pub xdg_config_home: Option<PathBuf>,
    pub xdg_data_home: Option<PathBuf>,
    pub xdg_runtime_dir: Option<PathBuf>,
}

impl BaseDirs {
    /// Construct BaseDirs from the current process environment.
    pub fn from_process() -> Self {
        BaseDirs {
            home: std::env::var("HOME").ok().map(PathBuf::from),
            xdg_config_home: std::env::var("XDG_CONFIG_HOME")
                .ok()
                .and_then(|s| if s.is_empty() { None } else { Some(PathBuf::from(s)) }),
            xdg_data_home: std::env::var("XDG_DATA_HOME")
                .ok()
                .and_then(|s| if s.is_empty() { None } else { Some(PathBuf::from(s)) }),
            xdg_runtime_dir: std::env::var("XDG_RUNTIME_DIR")
                .ok()
                .and_then(|s| if s.is_empty() { None } else { Some(PathBuf::from(s)) }),
        }
    }
}

/// Per-instance directory paths.
#[derive(Clone, Debug)]
pub struct InstancePaths {
    name: InstanceName,
    dirs: BaseDirs,
}

impl InstancePaths {
    /// Create instance paths from a name and base directories.
    pub fn new(name: InstanceName, dirs: BaseDirs) -> Self {
        InstancePaths { name, dirs }
    }

    /// Create instance paths from the current environment.
    /// Uses `InstanceName::from_env()` and `BaseDirs::from_process()`.
    pub fn current() -> Result<Self, String> {
        let name = InstanceName::from_env()?;
        let dirs = BaseDirs::from_process();
        Ok(InstancePaths { name, dirs })
    }

    /// The selected instance (as [`InstancePaths::current`]), but rooted at
    /// `home` instead of `$HOME` — for code that is handed a home directory
    /// by its caller.
    pub fn with_home(home: &std::path::Path) -> Result<Self, String> {
        let mut paths = Self::current()?;
        paths.dirs.home = Some(home.to_path_buf());
        Ok(paths)
    }

    /// [`InstancePaths::with_home`], falling back to the default instance if
    /// `AIVYX_PA_INSTANCE` is invalid (`main` rejects that at startup, so
    /// this only matters to code called outside the CLI). Home-based paths
    /// (`home_dir`, `tool_process_dir`, `sandbox_dir`) are then always `Some`.
    pub fn with_home_or_default(home: &std::path::Path) -> Self {
        Self::with_home(home).unwrap_or_else(|_| {
            Self::new(
                InstanceName::default_instance(),
                BaseDirs {
                    home: Some(home.to_path_buf()),
                    xdg_config_home: None,
                    xdg_data_home: None,
                    xdg_runtime_dir: None,
                },
            )
        })
    }

    /// Get the instance name.
    pub fn name(&self) -> &InstanceName {
        &self.name
    }

    /// Config directory: `<config_home>/aivyx-pa[/instances/<n>]`
    fn config_dir_base(&self) -> Option<PathBuf> {
        let config_home = self
            .dirs
            .xdg_config_home
            .as_ref()
            .cloned()
            .or_else(|| {
                self.dirs
                    .home
                    .as_ref()
                    .map(|h| h.join(".config"))
            });

        config_home.map(|h| h.join("aivyx-pa"))
    }

    pub fn config_dir(&self) -> Option<PathBuf> {
        let base = self.config_dir_base()?;
        if self.name.is_default() {
            Some(base)
        } else {
            Some(base.join("instances").join(self.name.as_str()))
        }
    }

    /// Config file path: `config_dir/aivyx-pa.toml`
    pub fn config_file(&self) -> Option<PathBuf> {
        self.config_dir().map(|d| d.join("aivyx-pa.toml"))
    }

    /// Data directory: `<data_home>/aivyx-pa[/instances/<n>]`
    fn data_dir_base(&self) -> Option<PathBuf> {
        let data_home = self
            .dirs
            .xdg_data_home
            .as_ref()
            .cloned()
            .or_else(|| {
                self.dirs
                    .home
                    .as_ref()
                    .map(|h| h.join(".local").join("share"))
            });

        data_home.map(|h| h.join("aivyx-pa"))
    }

    pub fn data_dir(&self) -> Option<PathBuf> {
        let base = self.data_dir_base()?;
        if self.name.is_default() {
            Some(base)
        } else {
            Some(base.join("instances").join(self.name.as_str()))
        }
    }

    /// Store file path: `data_dir/store.redb`
    pub fn store_file(&self) -> Option<PathBuf> {
        self.data_dir().map(|d| d.join("store.redb"))
    }

    /// Runtime directory: `$XDG_RUNTIME_DIR/aivyx-pa[/instances/<n>]`, else `$HOME/.local/share/aivyx-pa`
    /// (falls back to hardcoded `~/.local/share`, not `$XDG_DATA_HOME`, matching today's protocol.rs)
    fn runtime_dir_base(&self) -> Option<PathBuf> {
        self.dirs
            .xdg_runtime_dir
            .as_ref()
            .cloned()
            .map(|r| r.join("aivyx-pa"))
            .or_else(|| {
                self.dirs
                    .home
                    .as_ref()
                    .map(|h| h.join(".local").join("share").join("aivyx-pa"))
            })
    }

    pub fn runtime_dir(&self) -> Option<PathBuf> {
        let base = self.runtime_dir_base()?;
        if self.name.is_default() {
            Some(base)
        } else {
            Some(base.join("instances").join(self.name.as_str()))
        }
    }

    /// `$HOME/.local/share/aivyx-pa[/instances/<n>]`, ignoring
    /// `XDG_DATA_HOME` — for the few historical paths (vision output) that
    /// were always home-based, so the default instance's never move.
    pub fn home_data_dir(&self) -> Option<PathBuf> {
        let base = self.dirs.home.as_ref()?.join(".local").join("share").join("aivyx-pa");
        Some(if self.name.is_default() {
            base
        } else {
            base.join("instances").join(self.name.as_str())
        })
    }

    /// Socket path: `runtime_dir/daemon.sock`
    pub fn socket_path(&self) -> Option<PathBuf> {
        self.runtime_dir().map(|d| d.join("daemon.sock"))
    }

    /// PID path: `runtime_dir/daemon.pid`
    pub fn pid_path(&self) -> Option<PathBuf> {
        self.runtime_dir().map(|d| d.join("daemon.pid"))
    }

    /// Home directory: `~/.aivyx-pa[/instances/<n>]`
    fn home_dir_base(&self) -> Option<PathBuf> {
        self.dirs.home.as_ref().map(|h| h.join(".aivyx-pa"))
    }

    pub fn home_dir(&self) -> Option<PathBuf> {
        let base = self.home_dir_base()?;
        if self.name.is_default() {
            Some(base)
        } else {
            Some(base.join("instances").join(self.name.as_str()))
        }
    }

    /// Tool process directory: `home_dir/tool-processes/<tool>`
    pub fn tool_process_dir(&self, tool: &str) -> Option<PathBuf> {
        self.home_dir()
            .map(|h| h.join("tool-processes").join(tool))
    }

    /// Installed packs: `home_dir/packs` (`<name>/<version>/` inside).
    pub fn packs_dir(&self) -> Option<PathBuf> {
        self.home_dir().map(|h| h.join("packs"))
    }

    /// Sandbox directory: `~/aivyx-pa-sandbox[-<n>]`
    pub fn sandbox_dir(&self) -> Option<PathBuf> {
        self.dirs.home.as_ref().map(|h| {
            if self.name.is_default() {
                h.join("aivyx-pa-sandbox")
            } else {
                h.join(format!("aivyx-pa-sandbox-{}", self.name.as_str()))
            }
        })
    }

    /// Keyring account name: `base` or `base:<n>`
    pub fn keyring_account(&self, base: &str) -> String {
        if self.name.is_default() {
            base.to_string()
        } else {
            format!("{}:{}", base, self.name.as_str())
        }
    }

    /// Systemd unit name: `aivyx-pa-daemon[-<n>].service`
    pub fn systemd_unit(&self) -> String {
        if self.name.is_default() {
            "aivyx-pa-daemon.service".to_string()
        } else {
            format!("aivyx-pa-daemon-{}.service", self.name.as_str())
        }
    }

    /// Launchd label: `com.aivyx-pa.daemon[.<n>]`
    pub fn launchd_label(&self) -> String {
        if self.name.is_default() {
            "com.aivyx-pa.daemon".to_string()
        } else {
            format!("com.aivyx-pa.daemon.{}", self.name.as_str())
        }
    }
}

/// List all instances whose config or data directory exists.
/// Returns a sorted list with the default instance first.
pub fn list_instances(dirs: &BaseDirs) -> Vec<InstanceName> {
    let default_paths = InstancePaths::new(InstanceName::default_instance(), dirs.clone());

    let mut instances = std::collections::HashSet::new();

    // Check if default instance exists (has config or store file)
    if default_paths.config_dir().and_then(|d| {
        std::fs::metadata(d.join("aivyx-pa.toml")).ok()
    }).is_some() || default_paths.store_file().and_then(|f| {
        std::fs::metadata(f).ok()
    }).is_some() {
        instances.insert("default".to_string());
    }

    // Scan for named instances in config dir
    if let Some(config_base) = default_paths.config_dir_base() {
        let instances_dir = config_base.join("instances");
        if let Ok(entries) = std::fs::read_dir(&instances_dir) {
            for entry in entries.flatten() {
                if let Ok(name) = entry.file_name().into_string() {
                    if name != DEFAULT_INSTANCE && InstanceName::parse(&name).is_ok() {
                        instances.insert(name);
                    }
                }
            }
        }
    }

    // Scan for named instances in data dir
    if let Some(data_base) = default_paths.data_dir_base() {
        let instances_dir = data_base.join("instances");
        if let Ok(entries) = std::fs::read_dir(&instances_dir) {
            for entry in entries.flatten() {
                if let Ok(name) = entry.file_name().into_string() {
                    if name != DEFAULT_INSTANCE && InstanceName::parse(&name).is_ok() {
                        instances.insert(name);
                    }
                }
            }
        }
    }

    // Sort with default first, then alphabetically
    let mut result: Vec<InstanceName> = instances
        .iter()
        .filter_map(|n| InstanceName::parse(n).ok())
        .collect();

    result.sort_by(|a, b| {
        match (a.is_default(), b.is_default()) {
            (true, false) => std::cmp::Ordering::Less,
            (false, true) => std::cmp::Ordering::Greater,
            _ => a.as_str().cmp(b.as_str()),
        }
    });

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dirs(home: Option<&str>, cfg: Option<&str>, data: Option<&str>, run: Option<&str>) -> BaseDirs {
        BaseDirs {
            home: home.map(PathBuf::from),
            xdg_config_home: cfg.map(PathBuf::from),
            xdg_data_home: data.map(PathBuf::from),
            xdg_runtime_dir: run.map(PathBuf::from),
        }
    }

    fn p(name: &str, d: BaseDirs) -> InstancePaths {
        InstancePaths::new(InstanceName::parse(name).unwrap(), d)
    }

    #[test]
    fn with_home_roots_home_paths_at_the_given_dir() {
        let p = InstancePaths::with_home(std::path::Path::new("/tmp/h")).unwrap();
        assert_eq!(p.home_dir().unwrap(), PathBuf::from("/tmp/h/.aivyx-pa"));
    }

    #[test]
    fn home_data_dir_ignores_xdg_data_home() {
        let d = p("default", dirs(Some("/h"), None, Some("/d"), None));
        assert_eq!(d.home_data_dir().unwrap(), PathBuf::from("/h/.local/share/aivyx-pa"));
        let r = p("research", dirs(Some("/h"), None, Some("/d"), None));
        assert_eq!(r.home_data_dir().unwrap(), PathBuf::from("/h/.local/share/aivyx-pa/instances/research"));
    }

    #[test]
    fn packs_dir_sits_in_the_instance_home() {
        let d = p("default", dirs(Some("/h"), None, None, None));
        assert_eq!(d.packs_dir().unwrap(), PathBuf::from("/h/.aivyx-pa/packs"));
        let r = p("shop", dirs(Some("/h"), None, None, None));
        assert_eq!(r.packs_dir().unwrap(), PathBuf::from("/h/.aivyx-pa/instances/shop/packs"));
    }

    #[test]
    fn names_are_validated() {
        for ok in ["default", "research", "a", "home-2", "x1"] {
            assert!(InstanceName::parse(ok).is_ok(), "{ok}");
        }
        for bad in ["", "-a", "a-", "Research", "a_b", "a b", "../x", &"a".repeat(33)] {
            assert!(InstanceName::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn default_matches_todays_paths_with_xdg_set() {
        let d = p("default", dirs(Some("/h"), Some("/c"), Some("/d"), Some("/r")));
        assert_eq!(d.config_file().unwrap(), PathBuf::from("/c/aivyx-pa/aivyx-pa.toml"));
        assert_eq!(d.store_file().unwrap(), PathBuf::from("/d/aivyx-pa/store.redb"));
        assert_eq!(d.socket_path().unwrap(), PathBuf::from("/r/aivyx-pa/daemon.sock"));
        assert_eq!(d.pid_path().unwrap(), PathBuf::from("/r/aivyx-pa/daemon.pid"));
        assert_eq!(d.home_dir().unwrap(), PathBuf::from("/h/.aivyx-pa"));
        assert_eq!(d.tool_process_dir("gmail").unwrap(), PathBuf::from("/h/.aivyx-pa/tool-processes/gmail"));
        assert_eq!(d.sandbox_dir().unwrap(), PathBuf::from("/h/aivyx-pa-sandbox"));
        assert_eq!(d.systemd_unit(), "aivyx-pa-daemon.service");
        assert_eq!(d.launchd_label(), "com.aivyx-pa.daemon");
        assert_eq!(d.keyring_account("passphrase"), "passphrase");
    }

    #[test]
    fn default_matches_todays_paths_with_only_home() {
        let d = p("default", dirs(Some("/h"), None, None, None));
        assert_eq!(d.config_file().unwrap(), PathBuf::from("/h/.config/aivyx-pa/aivyx-pa.toml"));
        assert_eq!(d.store_file().unwrap(), PathBuf::from("/h/.local/share/aivyx-pa/store.redb"));
        assert_eq!(d.socket_path().unwrap(), PathBuf::from("/h/.local/share/aivyx-pa/daemon.sock"));
    }

    #[test]
    fn named_instances_nest_under_instances() {
        let r = p("research", dirs(Some("/h"), Some("/c"), Some("/d"), Some("/r")));
        assert_eq!(r.config_file().unwrap(), PathBuf::from("/c/aivyx-pa/instances/research/aivyx-pa.toml"));
        assert_eq!(r.store_file().unwrap(), PathBuf::from("/d/aivyx-pa/instances/research/store.redb"));
        assert_eq!(r.socket_path().unwrap(), PathBuf::from("/r/aivyx-pa/instances/research/daemon.sock"));
        assert_eq!(r.home_dir().unwrap(), PathBuf::from("/h/.aivyx-pa/instances/research"));
        assert_eq!(r.sandbox_dir().unwrap(), PathBuf::from("/h/aivyx-pa-sandbox-research"));
        assert_eq!(r.systemd_unit(), "aivyx-pa-daemon-research.service");
        assert_eq!(r.launchd_label(), "com.aivyx-pa.daemon.research");
        assert_eq!(r.keyring_account("passphrase"), "passphrase:research");
    }

    #[test]
    fn no_base_dirs_gives_none() {
        let d = p("default", dirs(None, None, None, None));
        assert!(d.config_file().is_none() && d.socket_path().is_none() && d.home_dir().is_none());
    }

    #[test]
    fn socket_fallback_ignores_xdg_data_home_like_today() {
        let d = p("default", dirs(Some("/h"), None, Some("/d"), None));
        assert_eq!(d.socket_path().unwrap(), PathBuf::from("/h/.local/share/aivyx-pa/daemon.sock"));
        let r = p("research", dirs(Some("/h"), None, Some("/d"), None));
        assert_eq!(r.socket_path().unwrap(), PathBuf::from("/h/.local/share/aivyx-pa/instances/research/daemon.sock"));
    }

    #[test]
    fn config_uses_xdg_config_home_when_set() {
        let d = p("default", dirs(Some("/h"), Some("/c"), None, None));
        assert_eq!(d.config_file().unwrap(), PathBuf::from("/c/aivyx-pa/aivyx-pa.toml"));
    }

    #[test]
    fn data_uses_xdg_data_home_when_set() {
        let d = p("default", dirs(Some("/h"), None, Some("/d"), None));
        assert_eq!(d.store_file().unwrap(), PathBuf::from("/d/aivyx-pa/store.redb"));
    }

    #[test]
    fn config_falls_back_to_home_when_xdg_config_not_set() {
        let d = p("default", dirs(Some("/h"), None, None, None));
        assert_eq!(d.config_file().unwrap(), PathBuf::from("/h/.config/aivyx-pa/aivyx-pa.toml"));
    }

    #[test]
    fn data_falls_back_to_home_when_xdg_data_not_set() {
        let d = p("default", dirs(Some("/h"), None, None, None));
        assert_eq!(d.store_file().unwrap(), PathBuf::from("/h/.local/share/aivyx-pa/store.redb"));
    }

    #[test]
    fn list_instances_finds_default_and_named() {
        let tmp = tempfile::tempdir().unwrap();
        let h = tmp.path();
        std::fs::create_dir_all(h.join(".config/aivyx-pa/instances/research")).unwrap();
        std::fs::create_dir_all(h.join(".local/share/aivyx-pa/instances/household")).unwrap();
        std::fs::create_dir_all(h.join(".config/aivyx-pa")).unwrap();
        std::fs::write(h.join(".config/aivyx-pa/aivyx-pa.toml"), "").unwrap();
        let d = BaseDirs { home: Some(h.to_path_buf()), xdg_config_home: None, xdg_data_home: None, xdg_runtime_dir: None };
        let names: Vec<String> = list_instances(&d).iter().map(|n| n.as_str().to_string()).collect();
        assert_eq!(names, vec!["default", "household", "research"]);
    // A stray `instances/default` dir is not a second default.
    std::fs::create_dir_all(h.join(".config/aivyx-pa/instances/default")).unwrap();
    let again: Vec<String> = list_instances(&d).iter().map(|n| n.as_str().to_string()).collect();
    assert_eq!(again, vec!["default", "household", "research"]);
    }
}
