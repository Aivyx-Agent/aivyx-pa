//! Operator-supplied KitchenDB connection config.
//!
//! Chapter Brigade (BG.1). Loaded once at tool-process startup from
//! `~/.aivyx-pa/tool-processes/kitchen/config.toml`. The kitchen toolkit talks to
//! the operator's **KitchenDB** (Postgres + PostgREST) — the system of record —
//! so it needs the PostgREST base URL, an API key, and the multi-tenant
//! `organization_id` every KitchenDB RPC takes (`p_organization_id`).
//!
//! ## File format
//!
//! ```toml
//! # ~/.aivyx-pa/tool-processes/kitchen/config.toml
//!
//! [kitchen_db]
//! # The PostgREST base URL (Supabase: https://<ref>.supabase.co/rest/v1).
//! base_url = "https://your-kitchen.example/rest/v1"
//! # The PostgREST API key (Supabase anon/service key). Sent as both the
//! # `apikey` header and the `Authorization: Bearer` token.
//! api_key = "eyJ..."
//! # The tenant this agent operates on — passed as `p_organization_id` to
//! # every RPC.
//! organization_id = "00000000-0000-0000-0000-000000000000"
//! ```

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use aivyx_instance::InstancePaths;

#[derive(Debug, Error)]
pub enum ConfigFileError {
    #[error("config file I/O failed at {path:?}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("config file at {path:?} not found — create it with the KitchenDB base_url, api_key, and organization_id (see docs/BRIGADE.md)")]
    NotFound { path: PathBuf },
    #[error("config file at {path:?} failed to parse as TOML: {reason}")]
    Parse { path: PathBuf, reason: String },
    #[error("config file at {path:?} is missing the [kitchen_db] section — the kitchen tools cannot reach KitchenDB without it")]
    MissingKitchenDb { path: PathBuf },
    #[error("$HOME is unset; cannot resolve default config path")]
    NoHome,
}

/// Operator config root. `kitchen_db` is `Option` so a parse of an empty file
/// succeeds with a distinct [`ConfigFileError::MissingKitchenDb`] surfaced by
/// [`load_config`] (clearer than a serde "missing field" error).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct KitchenConfig {
    #[serde(default)]
    pub kitchen_db: Option<KitchenDbConfig>,
}

/// KitchenDB PostgREST connection. All three fields are required.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KitchenDbConfig {
    /// PostgREST base URL (no trailing `/rpc`). RPCs are POSTed to
    /// `<base_url>/rpc/<function>`.
    pub base_url: String,
    /// PostgREST API key — sent as `apikey` + `Authorization: Bearer`.
    pub api_key: String,
    /// The tenant id, passed as `p_organization_id` to every RPC.
    pub organization_id: String,
}

/// Default config path for a given instance: `~/.aivyx-pa[/instances/<n>]/tool-processes/kitchen/config.toml`.
pub fn default_config_path_for(instance: &InstancePaths) -> Option<PathBuf> {
    instance
        .tool_process_dir("kitchen")
        .map(|d| d.join("config.toml"))
}

/// Default config path: `$HOME/.aivyx-pa/tool-processes/kitchen/config.toml`.
pub fn default_config_path() -> Result<PathBuf, ConfigFileError> {
    let instance = InstancePaths::current().map_err(|_| ConfigFileError::NoHome)?;
    default_config_path_for(&instance).ok_or(ConfigFileError::NoHome)
}

/// Load + validate the kitchen config. `NotFound` and `MissingKitchenDb` are
/// distinct so the binary can print operator-actionable guidance.
pub fn load_config(path: &Path) -> Result<KitchenDbConfig, ConfigFileError> {
    let body = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(ConfigFileError::NotFound { path: path.to_path_buf() });
        }
        Err(e) => {
            return Err(ConfigFileError::Io { path: path.to_path_buf(), source: e });
        }
    };
    let parsed: KitchenConfig = toml::from_str(&body).map_err(|e| ConfigFileError::Parse {
        path: path.to_path_buf(),
        reason: e.to_string(),
    })?;
    parsed
        .kitchen_db
        .ok_or_else(|| ConfigFileError::MissingKitchenDb { path: path.to_path_buf() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn scratch_dir() -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let tmp = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string());
        let dir = PathBuf::from(tmp).join(format!(
            "aivyx-kitchen-cfg-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn load_config_reads_the_kitchen_db_section() {
        let dir = scratch_dir();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "[kitchen_db]\nbase_url = \"https://k/rest/v1\"\napi_key = \"KEY\"\norganization_id = \"ORG\"\n",
        )
        .unwrap();
        let cfg = load_config(&path).expect("load");
        assert_eq!(cfg.base_url, "https://k/rest/v1");
        assert_eq!(cfg.api_key, "KEY");
        assert_eq!(cfg.organization_id, "ORG");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_config_missing_section_is_distinct() {
        let dir = scratch_dir();
        let path = dir.join("config.toml");
        std::fs::write(&path, "# no kitchen_db here\n").unwrap();
        match load_config(&path) {
            Err(ConfigFileError::MissingKitchenDb { .. }) => {}
            other => panic!("expected MissingKitchenDb; got {other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_config_not_found_is_distinct() {
        let dir = scratch_dir();
        match load_config(&dir.join("missing.toml")) {
            Err(ConfigFileError::NotFound { .. }) => {}
            other => panic!("expected NotFound; got {other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_config_parse_error_carries_path() {
        let dir = scratch_dir();
        let path = dir.join("bad.toml");
        std::fs::write(&path, "not toml =").unwrap();
        match load_config(&path) {
            Err(ConfigFileError::Parse { path: p, .. }) => assert_eq!(p, path),
            other => panic!("expected Parse; got {other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn default_config_path_under_home() {
        let Some(_home) = std::env::var_os("HOME") else { return };
        let p = default_config_path().expect("HOME set");
        let s = p.to_string_lossy();
        assert!(s.contains("tool-processes"), "{s}");
        assert!(s.contains("kitchen"), "{s}");
        assert!(s.ends_with("config.toml"), "{s}");
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
            PathBuf::from("/home/user/.aivyx-pa/instances/research/tool-processes/kitchen/config.toml")
        );
    }
}
