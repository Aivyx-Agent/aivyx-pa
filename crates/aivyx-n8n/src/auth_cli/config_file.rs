//! Config file loading for `aivyx-n8n`.
//!
//! Thin wrapper over `aivyx_auth_cli::load_toml` —
//! Phase 132 lift. Service-specific validation
//! (the base URL and the API key must both be
//! non-empty) lives here.

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::N8nConfig;

const SERVICE_SUBDIR: &str = "n8n";

/// Composes the substrate's IO/parse errors with
/// n8n-specific validation errors. See
/// `aivyx-notion::auth_cli::config_file` for the
/// shape rationale.
#[derive(Debug, Error)]
pub enum ConfigFileError {
    /// IO / parse error from `aivyx-auth-cli`.
    #[error(transparent)]
    Substrate(#[from] aivyx_auth_cli::ConfigFileError),

    /// Service-specific: the base URL is empty.
    #[error("config field `n8n_base_url` is empty in {path:?}")]
    EmptyBaseUrl { path: PathBuf },

    /// Service-specific: the API key is empty.
    #[error("config field `n8n_api_key` is empty in {path:?}")]
    EmptyApiKey { path: PathBuf },
}

pub fn default_config_path() -> Result<PathBuf, ConfigFileError> {
    aivyx_auth_cli::default_config_path(SERVICE_SUBDIR).ok_or_else(|| {
        ConfigFileError::Substrate(aivyx_auth_cli::ConfigFileError::NotFound {
            path: PathBuf::from(format!(
                "~/.aivyx-pa/tool-processes/{SERVICE_SUBDIR}/config.toml"
            )),
        })
    })
}

pub fn load_config(path: &Path) -> Result<N8nConfig, ConfigFileError> {
    let cfg: N8nConfig = aivyx_auth_cli::load_toml(path)?;
    if cfg.n8n_base_url.trim().is_empty() {
        return Err(ConfigFileError::EmptyBaseUrl {
            path: path.to_path_buf(),
        });
    }
    if cfg.n8n_api_key.trim().is_empty() {
        return Err(ConfigFileError::EmptyApiKey {
            path: path.to_path_buf(),
        });
    }
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmpfile(body: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "aivyx-n8n-cfg-{}-{}.toml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        path
    }

    // Substrate-level behaviour (NotFound on missing
    // path, Parse on malformed TOML) is tested in
    // aivyx-auth-cli. These tests cover n8n-specific
    // validation: empty base URL + empty API key.

    #[test]
    fn rejects_empty_base_url() {
        let path = tmpfile(
            r#"n8n_base_url = "  "
n8n_api_key = "k""#,
        );
        let e = load_config(&path).expect_err("must error");
        assert!(matches!(e, ConfigFileError::EmptyBaseUrl { .. }));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn rejects_empty_api_key() {
        let path = tmpfile(
            r#"n8n_base_url = "https://n8n.example.com"
n8n_api_key = """#,
        );
        let e = load_config(&path).expect_err("must error");
        assert!(matches!(e, ConfigFileError::EmptyApiKey { .. }));
        let _ = std::fs::remove_file(&path);
    }

    #[cfg(unix)]
    #[test]
    fn load_config_tightens_permissions_to_0600() {
        use std::os::unix::fs::PermissionsExt;
        // load_config wraps aivyx_auth_cli::load_toml, which
        // owns the actual chmod logic (see its own tests for
        // the exhaustive cases); this confirms the tightening
        // survives n8n's wrapper.
        let path = tmpfile(
            r#"n8n_base_url = "https://n8n.example.com"
n8n_api_key = "ntn_test""#,
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let _cfg = load_config(&path).expect("load");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn accepts_valid_config() {
        let path = tmpfile(
            r#"n8n_base_url = "https://n8n.example.com"
n8n_api_key = "ntn_test""#,
        );
        let cfg = load_config(&path).expect("ok");
        assert_eq!(cfg.n8n_base_url, "https://n8n.example.com");
        assert_eq!(cfg.n8n_api_key, "ntn_test");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn substrate_not_found_flows_through_transparently() {
        let e = load_config(Path::new("/nope/missing.toml")).expect_err("must error");
        assert!(matches!(
            e,
            ConfigFileError::Substrate(aivyx_auth_cli::ConfigFileError::NotFound { .. })
        ));
    }
}
