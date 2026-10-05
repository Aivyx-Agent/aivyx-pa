//! Operator-supplied toolkit configuration.
//!
//! Phase 125 Task 2. Loaded once at tool-process startup from
//! `~/.aivyx-pa/tool-processes/toolkit/config.toml`. Different
//! shape than `aivyx-gmail`'s config (no OAuth client; the
//! services this bundle integrates use API keys or no auth
//! at all).
//!
//! ## File format
//!
//! ```toml
//! # ~/.aivyx-pa/tool-processes/toolkit/config.toml
//!
//! # Top-level scalar keys (like this one) MUST come before any
//! # [table] header below — TOML attributes a bare key to whichever
//! # table most recently opened, and this loader has no
//! # deny_unknown_fields to catch the mistake, so a key placed after
//! # [brave_search] would silently land nowhere useful.
//! #
//! # Phase 191 — notify target health-check alerts dispatch to
//! # automatically, both directions (down and recovered). Unset
//! # means alerts are skipped silently; see `health_polling.rs`.
//! default_notify_target = "phone"
//!
//! [brave_search]
//! # Get a free key at https://api.search.brave.com/.
//! # Required for the `web.search` tool. If absent, web.search
//! # reports a clear error at first invocation.
//! api_key = "BSA-..."
//! ```
//!
//! Future per-tool sub-tables (e.g. `[health_check]` for
//! polling defaults, `[tasks]` for storage location overrides)
//! land additively without breaking existing operator
//! installs.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use aivyx_instance::InstancePaths;

#[derive(Debug, Error)]
pub enum ConfigFileError {
    #[error("config file I/O failed at {path:?}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("config file at {path:?} not found — create it with operator's tool API keys (see `aivyx-toolkit help`)")]
    NotFound { path: PathBuf },
    #[error("config file at {path:?} failed to parse as TOML: {reason}")]
    Parse { path: PathBuf, reason: String },
    #[error("$HOME is unset; cannot resolve default config path")]
    NoHome,
}

/// Operator config root. All sub-tables are optional so an
/// operator who only uses one tool category doesn't have to
/// populate the others.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ToolkitConfig {
    #[serde(default)]
    pub brave_search: Option<BraveSearchConfig>,

    /// Phase 191 — the notify target health-check alerts dispatch
    /// to automatically. `None` (unset) means alerts are skipped
    /// silently; see `health_polling.rs`.
    #[serde(default)]
    pub default_notify_target: Option<String>,
}

/// Brave Search API config. Operator generates an API key at
/// <https://api.search.brave.com/>; free tier allows 2000
/// queries/month at the time of writing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BraveSearchConfig {
    pub api_key: String,
}

/// Default config path for a given instance:
/// `~/.aivyx-pa[/instances/<n>]/tool-processes/toolkit/config.toml`.
pub fn default_config_path_for(instance: &InstancePaths) -> Option<PathBuf> {
    instance
        .tool_process_dir("toolkit")
        .map(|d| d.join("config.toml"))
}

/// Default config path:
/// `$HOME/.aivyx-pa/tool-processes/toolkit/config.toml`.
pub fn default_config_path() -> Result<PathBuf, ConfigFileError> {
    let instance = InstancePaths::current().map_err(|_| ConfigFileError::NoHome)?;
    default_config_path_for(&instance).ok_or(ConfigFileError::NoHome)
}

/// Default state-directory path for a given instance:
/// `~/.aivyx-pa[/instances/<n>]/tool-processes/toolkit/`. Returned without
/// `config.toml` appended so callers (task storage,
/// health-state storage) can join their own file names.
pub fn default_state_dir_for(instance: &InstancePaths) -> Option<PathBuf> {
    instance.tool_process_dir("toolkit")
}

/// Default state-directory path:
/// `$HOME/.aivyx-pa/tool-processes/toolkit/`. Returned without
/// `config.toml` appended so callers (task storage,
/// health-state storage) can join their own file names.
pub fn default_state_dir() -> Result<PathBuf, ConfigFileError> {
    let instance = InstancePaths::current().map_err(|_| ConfigFileError::NoHome)?;
    default_state_dir_for(&instance).ok_or(ConfigFileError::NoHome)
}

/// Load the toolkit config from disk. Surfaces a distinct
/// `NotFound` error so callers can print actionable guidance.
pub fn load_config(path: &Path) -> Result<ToolkitConfig, ConfigFileError> {
    let body = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(ConfigFileError::NotFound {
                path: path.to_path_buf(),
            });
        }
        Err(e) => {
            return Err(ConfigFileError::Io {
                path: path.to_path_buf(),
                source: e,
            });
        }
    };
    toml::from_str::<ToolkitConfig>(&body).map_err(|e| ConfigFileError::Parse {
        path: path.to_path_buf(),
        reason: e.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn scratch_dir() -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let tmp = std::env::var("TMPDIR")
            .or_else(|_| std::env::var("TEMP"))
            .unwrap_or_else(|_| "/tmp".to_string());
        let dir = PathBuf::from(tmp).join(format!(
            "aivyx-toolkit-cfg-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn load_config_with_brave_search_section() {
        let dir = scratch_dir();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "[brave_search]\napi_key = \"BSA-test-key\"\n",
        )
        .unwrap();
        let cfg = load_config(&path).expect("load");
        assert_eq!(
            cfg.brave_search.as_ref().unwrap().api_key,
            "BSA-test-key"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_config_empty_file_yields_all_none() {
        let dir = scratch_dir();
        let path = dir.join("config.toml");
        std::fs::write(&path, "").unwrap();
        let cfg = load_config(&path).expect("load");
        assert!(cfg.brave_search.is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_config_surfaces_not_found_distinctly() {
        let dir = scratch_dir();
        let path = dir.join("missing.toml");
        match load_config(&path) {
            Err(ConfigFileError::NotFound { .. }) => {}
            other => panic!("expected NotFound; got {other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_config_surfaces_parse_error_with_path() {
        let dir = scratch_dir();
        let path = dir.join("bad.toml");
        std::fs::write(&path, "not toml at all =").unwrap();
        match load_config(&path) {
            Err(ConfigFileError::Parse { path: p, .. }) => assert_eq!(p, path),
            other => panic!("expected Parse; got {other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn brave_search_config_serde_roundtrips() {
        let cfg = BraveSearchConfig {
            api_key: "BSA-rt".to_string(),
        };
        let json = serde_json::to_value(&cfg).unwrap();
        let back: BraveSearchConfig = serde_json::from_value(json).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn load_config_with_default_notify_target() {
        let dir = scratch_dir();
        let path = dir.join("config.toml");
        std::fs::write(&path, "default_notify_target = \"phone\"\n").unwrap();
        let cfg = load_config(&path).expect("load");
        assert_eq!(cfg.default_notify_target, Some("phone".to_string()));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_config_without_default_notify_target_is_none() {
        let dir = scratch_dir();
        let path = dir.join("config.toml");
        std::fs::write(&path, "").unwrap();
        let cfg = load_config(&path).expect("load");
        assert_eq!(cfg.default_notify_target, None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn default_state_dir_under_home() {
        let Some(home) = std::env::var_os("HOME") else {
            return;
        };
        let p = default_state_dir().expect("HOME set");
        let s = p.to_string_lossy();
        let home_s = home.to_string_lossy();
        assert!(s.starts_with(home_s.as_ref()), "{s}");
        assert!(s.contains("tool-processes"), "{s}");
        assert!(s.contains("toolkit"), "{s}");
        // No trailing file name — callers join their own.
        assert!(!s.ends_with(".toml"), "{s}");
    }

    #[test]
    fn default_config_path_for_named_instance() {
        use aivyx_instance::{BaseDirs, InstanceName};
        let dirs = BaseDirs {
            home: Some(PathBuf::from("/home/user")),
            xdg_config_home: Some(PathBuf::from("/etc/config")),
            xdg_data_home: Some(PathBuf::from("/var/data")),
            xdg_runtime_dir: Some(PathBuf::from("/run")),
        };
        let instance = InstancePaths::new(InstanceName::parse("research").unwrap(), dirs);
        let p = default_config_path_for(&instance).expect("path");
        assert_eq!(
            p,
            PathBuf::from("/home/user/.aivyx-pa/instances/research/tool-processes/toolkit/config.toml")
        );
    }

    #[test]
    fn default_state_dir_for_named_instance() {
        use aivyx_instance::{BaseDirs, InstanceName};
        let dirs = BaseDirs {
            home: Some(PathBuf::from("/home/user")),
            xdg_config_home: Some(PathBuf::from("/etc/config")),
            xdg_data_home: Some(PathBuf::from("/var/data")),
            xdg_runtime_dir: Some(PathBuf::from("/run")),
        };
        let instance = InstancePaths::new(InstanceName::parse("research").unwrap(), dirs);
        let p = default_state_dir_for(&instance).expect("path");
        assert_eq!(
            p,
            PathBuf::from("/home/user/.aivyx-pa/instances/research/tool-processes/toolkit")
        );
    }
}
