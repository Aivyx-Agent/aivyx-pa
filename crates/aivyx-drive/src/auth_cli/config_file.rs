//! Load the operator-supplied OAuth client config from
//! `~/.aivyx-pa/tool-processes/drive/config.toml`.
//!
//! Q1a (Recommended) at Phase 123 sign-off: operator creates
//! their own Google Cloud OAuth client and pastes the
//! `client_id` + `client_secret` + `redirect_uri` into this
//! file. Aivyx ships no shared OAuth app.
//!
//! ## File format
//!
//! ```toml
//! # ~/.aivyx-pa/tool-processes/drive/config.toml
//! client_id = "XXXXX.apps.googleusercontent.com"
//! client_secret = "GOCSPX-..."
//! redirect_uri = "http://127.0.0.1:8088/oauth/callback"
//!
//! # Optional. Defaults to the full Q2c scope set
//! # (drive.readonly + drive.compose + drive.send).
//! # Operators may narrow to a subset.
//! # scopes = [
//! #   "https://www.googleapis.com/auth/drive.readonly",
//! # ]
//! ```

use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

use aivyx_instance::InstancePaths;

use crate::OAuthConfig;

#[derive(Debug, Error)]
pub enum ConfigFileError {
    #[error("config file I/O failed at {path:?}: {source}")]
    Io {
        path: PathBuf,
        source: io::Error,
    },
    #[error("config file at {path:?} not found — create it with operator's OAuth client_id + client_secret + redirect_uri (see `aivyx-drive help`)")]
    NotFound { path: PathBuf },
    #[error("config file at {path:?} failed to parse as TOML: {reason}")]
    Parse { path: PathBuf, reason: String },
    #[error("$HOME is unset; cannot resolve default config path")]
    NoHome,
}

/// Default path for a given instance: `~/.aivyx-pa[/instances/<n>]/tool-processes/drive/config.toml`.
pub fn default_config_path_for(instance: &InstancePaths) -> Option<PathBuf> {
    instance
        .tool_process_dir("drive")
        .map(|d| d.join("config.toml"))
}

/// Default path: `$HOME/.aivyx-pa/tool-processes/drive/config.toml`.
pub fn default_config_path() -> Result<PathBuf, ConfigFileError> {
    let instance = InstancePaths::current().map_err(|_| ConfigFileError::NoHome)?;
    default_config_path_for(&instance).ok_or(ConfigFileError::NoHome)
}

/// Load the OAuth config from the given path. Surfaces a
/// distinct `NotFound` error so callers can print an
/// actionable message pointing operators at the file format
/// docs.
pub fn load_oauth_config(path: &Path) -> Result<OAuthConfig, ConfigFileError> {
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
    let mut cfg: OAuthConfig =
        toml::from_str(&body).map_err(|e| ConfigFileError::Parse {
            path: path.to_path_buf(),
            reason: e.to_string(),
        })?;
    // Phase 129 OAuth lift: substrate yields empty scopes
    // on absence; populate drive default here.
    if cfg.scopes.is_empty() {
        cfg.scopes = crate::DEFAULT_DRIVE_SCOPES
            .iter()
            .map(|s| (*s).to_string())
            .collect();
    }
    Ok(cfg)
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
            "aivyx-drive-cfg-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn load_oauth_config_from_minimal_toml() {
        let dir = scratch_dir();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            r#"client_id = "id-x.apps.googleusercontent.com"
client_secret = "GOCSPX-xxxx"
redirect_uri = "http://127.0.0.1:8088/oauth/callback"
"#,
        )
        .unwrap();
        let cfg = load_oauth_config(&path).expect("load");
        assert_eq!(cfg.client_id, "id-x.apps.googleusercontent.com");
        assert_eq!(cfg.client_secret, "GOCSPX-xxxx");
        assert_eq!(cfg.redirect_uri, "http://127.0.0.1:8088/oauth/callback");
        // Scopes default to the full set.
        assert_eq!(cfg.scopes.len(), crate::DEFAULT_DRIVE_SCOPES.len());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_oauth_config_with_explicit_scopes() {
        let dir = scratch_dir();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            r#"client_id = "id"
client_secret = "secret"
redirect_uri = "http://127.0.0.1:0/cb"
scopes = ["https://www.googleapis.com/auth/drive.readonly"]
"#,
        )
        .unwrap();
        let cfg = load_oauth_config(&path).expect("load");
        assert_eq!(cfg.scopes.len(), 1);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_oauth_config_surfaces_not_found_distinctly() {
        let dir = scratch_dir();
        let path = dir.join("missing.toml");
        match load_oauth_config(&path) {
            Err(ConfigFileError::NotFound { .. }) => {}
            other => panic!("expected NotFound; got {other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_oauth_config_surfaces_parse_error_with_path() {
        let dir = scratch_dir();
        let path = dir.join("bad.toml");
        std::fs::write(&path, "not toml at all =").unwrap();
        match load_oauth_config(&path) {
            Err(ConfigFileError::Parse {
                path: p,
                reason: _,
            }) => assert_eq!(p, path),
            other => panic!("expected Parse; got {other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn load_oauth_config_rejects_missing_required_field() {
        let dir = scratch_dir();
        let path = dir.join("missing-field.toml");
        // client_secret missing → serde fails the required
        // field constraint.
        std::fs::write(
            &path,
            r#"client_id = "id"
redirect_uri = "http://localhost:0/cb"
"#,
        )
        .unwrap();
        match load_oauth_config(&path) {
            Err(ConfigFileError::Parse { reason, .. }) => {
                assert!(
                    reason.contains("client_secret"),
                    "error should name the missing field; got: {reason}"
                );
            }
            other => panic!("expected Parse; got {other:?}"),
        }
        std::fs::remove_dir_all(&dir).ok();
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
            PathBuf::from("/home/user/.aivyx-pa/instances/research/tool-processes/drive/config.toml")
        );
    }
}
