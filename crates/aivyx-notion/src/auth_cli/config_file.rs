//! Config file loading for `aivyx-notion`.
//!
//! Thin wrapper over `aivyx_auth_cli::load_toml` —
//! Phase 132 lift. Service-specific validation
//! (the Notion Integration Token must be non-empty)
//! lives here.

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::NotionConfig;

const SERVICE_SUBDIR: &str = "notion";

/// Composes the substrate's IO/parse errors with
/// Notion-specific validation errors. Callers `match`
/// to distinguish substrate vs service-specific
/// failures; `{e}` Display works for either layer.
#[derive(Debug, Error)]
pub enum ConfigFileError {
    /// IO / parse error from `aivyx-auth-cli`. The
    /// substrate's variants (NotFound / Io / Parse)
    /// flow through verbatim so operators see the
    /// same wording the substrate produces.
    #[error(transparent)]
    Substrate(#[from] aivyx_auth_cli::ConfigFileError),

    /// Service-specific: the token field is empty.
    /// Distinct variant so callers can surface a
    /// Notion-targeted fix-it hint without parsing the
    /// substrate Parse error's `reason` field.
    #[error("config field `notion_token` is empty in {path:?}. Create the integration at Notion's Integrations dashboard (Settings → My integrations → New integration → Internal) and copy the token here.")]
    EmptyToken { path: PathBuf },
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

pub fn load_config(path: &Path) -> Result<NotionConfig, ConfigFileError> {
    let cfg: NotionConfig = aivyx_auth_cli::load_toml(path)?;
    if cfg.notion_token.trim().is_empty() {
        return Err(ConfigFileError::EmptyToken {
            path: path.to_path_buf(),
        });
    }
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn tmpfile(contents: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "aivyx-notion-cfg-{}-{}.toml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        path
    }

    // Substrate behaviour (NotFound on missing path, Parse
    // on malformed TOML, etc.) is tested in aivyx-auth-cli
    // directly. These tests cover Notion-specific
    // composition: empty-token rejection and the actionable
    // fix-it message.

    #[test]
    fn load_config_rejects_empty_token_with_service_specific_variant() {
        let path = tmpfile(r#"notion_token = "  ""#);
        let e = load_config(&path).expect_err("must error");
        assert!(
            matches!(e, ConfigFileError::EmptyToken { .. }),
            "must be EmptyToken, got {e:?}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[cfg(unix)]
    #[test]
    fn load_config_tightens_permissions_to_0600() {
        use std::os::unix::fs::PermissionsExt;
        // load_config wraps aivyx_auth_cli::load_toml, which
        // owns the actual chmod logic (see its own tests for
        // the exhaustive cases); this confirms the tightening
        // survives Notion's wrapper.
        let path = tmpfile(r#"notion_token = "ntn_test_xyz""#);
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let _cfg = load_config(&path).expect("load");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_config_accepts_valid_token() {
        let path = tmpfile(r#"notion_token = "ntn_test_xyz""#);
        let cfg = load_config(&path).expect("load");
        assert_eq!(cfg.notion_token, "ntn_test_xyz");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn substrate_not_found_flows_through_transparently() {
        let nonexistent = std::env::temp_dir().join(format!(
            "aivyx-notion-missing-{}.toml",
            std::process::id(),
        ));
        let e = load_config(&nonexistent).expect_err("must error");
        // The Substrate variant carries the substrate error verbatim,
        // and Display routes through to it via #[error(transparent)].
        assert!(matches!(
            e,
            ConfigFileError::Substrate(aivyx_auth_cli::ConfigFileError::NotFound { .. })
        ));
    }

    #[test]
    fn empty_token_error_message_points_at_notion_integrations() {
        let e = ConfigFileError::EmptyToken {
            path: PathBuf::from("/tmp/c.toml"),
        };
        let s = e.to_string();
        assert!(s.contains("notion_token"));
        assert!(s.contains("Integrations dashboard"));
    }
}
