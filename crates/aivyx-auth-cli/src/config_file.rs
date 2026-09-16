//! Config file loading for Chapter F third-party tool
//! processes.
//!
//! Owns the **IO + parse** layer (`NotFound`, `Io`,
//! `Parse`). Service-specific validation errors stay
//! on the consumer side as a separate enum the
//! consumer composes with this one. Rationale: lifting
//! the validation errors would force every consumer to
//! report the same set of failure modes, but each
//! service has distinct invariants (Notion's token
//! must be non-empty, Obsidian's vault path must be
//! absolute, n8n needs both a base URL and an API
//! key). Keeping them separate preserves
//! service-specific error messages without polluting
//! the shared substrate.

use std::io;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ConfigFileError {
    /// The config file does not exist. Common operator
    /// failure: first-time setup before the operator
    /// has written the per-service `config.toml`.
    #[error("config file at {path:?} not found")]
    NotFound { path: PathBuf },

    /// Filesystem I/O error reading the config file
    /// (permissions, EIO, etc.).
    #[error("I/O error reading {path:?}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    /// The config file exists but is not valid TOML or
    /// does not deserialise into the consumer's
    /// `Config` shape.
    #[error("parse error in {path:?}: {reason}")]
    Parse { path: PathBuf, reason: String },
}

/// Compute the default config-file path for a Chapter F
/// service. Returns `None` if `$HOME` is not set in
/// the environment (rare but possible inside locked-
/// down container builds).
///
/// `service_subdir` is the leaf directory name —
/// `"notion"`, `"obsidian"`, `"n8n"`, etc.
pub fn default_config_path(service_subdir: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join(".aivyx-pa")
            .join("tool-processes")
            .join(service_subdir)
            .join("config.toml"),
    )
}

/// Load a TOML config file and deserialise into the
/// consumer's `Config` type.
///
/// Errors flow through [`ConfigFileError`]:
/// - File missing → [`ConfigFileError::NotFound`].
/// - Other I/O failure → [`ConfigFileError::Io`].
/// - Body not valid TOML or doesn't fit `T` →
///   [`ConfigFileError::Parse`].
///
/// Service-specific validation (e.g. "token must be
/// non-empty") happens on the consumer side **after**
/// this returns `Ok(T)`. The substrate deliberately
/// does not own per-field invariants — each service's
/// `Config::validate` carries those.
pub fn load_toml<T: DeserializeOwned>(path: &Path) -> Result<T, ConfigFileError> {
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
    // Best-effort permission tightening. Unlike `tokens.json`
    // (written programmatically by the OAuth flow and given
    // atomic-0600 treatment via `write_secure` in
    // aivyx-google-oauth's storage.rs), `config.toml` here is
    // operator-authored — the operator hand-creates it and
    // pastes in the Notion token / n8n API key / Google
    // client_secret (see each consumer's doc comments). There
    // is no software write site to apply `write_secure` to, so
    // this is the load-time equivalent: every time a tool
    // process starts and loads its config, permissions are
    // normalized back to 0600 if the operator's editor (or a
    // `cp`) left it wider. Failure here does not block loading
    // — the file was already read successfully by this point,
    // and refusing to start over a chmod failure (e.g. an
    // unusual filesystem) would be a worse outcome than leaving
    // permissions untightened for one more run.
    let _ = enforce_secure_permissions(path);
    toml::from_str(&body).map_err(|e| ConfigFileError::Parse {
        path: path.to_path_buf(),
        reason: e.to_string(),
    })
}

/// Tighten `path` to `0600` on Unix if its current mode is
/// wider. No-op (`Ok(())`) on non-Unix targets — matches
/// `write_secure`'s own Unix-only enforcement in
/// aivyx-google-oauth's storage.rs, documented there as
/// best-effort on Windows since its ACL model is different.
///
/// `pub` so consumers that don't route through [`load_toml`]
/// (`aivyx-gmail`'s `config_file.rs` parses independently, to
/// apply its own default-scopes fallback) can still reuse this
/// exact enforcement rather than reimplementing it.
pub fn enforce_secure_permissions(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path)?.permissions().mode() & 0o777;
        if mode != 0o600 {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use std::io::Write;

    #[derive(Debug, Deserialize, PartialEq)]
    struct FakeConfig {
        api_key: String,
        max_retries: Option<u32>,
    }

    fn tmpfile(body: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "aivyx-auth-cli-cfg-{}-{}.toml",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        path
    }

    #[test]
    fn returns_not_found_for_missing_path() {
        let nonexistent = std::env::temp_dir().join(format!(
            "aivyx-auth-cli-missing-{}.toml",
            std::process::id(),
        ));
        let e = load_toml::<FakeConfig>(&nonexistent).expect_err("must error");
        assert!(matches!(e, ConfigFileError::NotFound { .. }));
    }

    #[test]
    fn returns_parse_for_malformed_toml() {
        let path = tmpfile("not valid ====");
        let e = load_toml::<FakeConfig>(&path).expect_err("must error");
        assert!(matches!(e, ConfigFileError::Parse { .. }));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn returns_parse_when_toml_does_not_fit_target_type() {
        // valid TOML, but missing the required `api_key`
        let path = tmpfile(r#"max_retries = 3"#);
        let e = load_toml::<FakeConfig>(&path).expect_err("must error");
        assert!(matches!(e, ConfigFileError::Parse { .. }));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn loads_valid_toml_into_target_type() {
        let path = tmpfile(
            r#"api_key = "k_test"
max_retries = 5"#,
        );
        let cfg: FakeConfig = load_toml(&path).expect("ok");
        assert_eq!(cfg.api_key, "k_test");
        assert_eq!(cfg.max_retries, Some(5));
        let _ = std::fs::remove_file(&path);
    }

    #[cfg(unix)]
    #[test]
    fn load_toml_tightens_permissions_to_0600() {
        use std::os::unix::fs::PermissionsExt;
        let path = tmpfile(r#"api_key = "k_test""#);
        // tmpfile() creates via File::create, which lands at a
        // umask-derived mode (typically 0644) — set it
        // explicitly to a wider mode here so the test proves
        // load_toml is the one doing the tightening, not that
        // the file happened to already be 0600.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let _cfg: FakeConfig = load_toml(&path).expect("load");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let _ = std::fs::remove_file(&path);
    }

    #[cfg(unix)]
    #[test]
    fn load_toml_tightens_permissions_even_on_parse_failure() {
        use std::os::unix::fs::PermissionsExt;
        // Permission tightening runs before the parse attempt,
        // so a malformed config still gets its mode fixed —
        // the operator shouldn't have to fix the TOML first to
        // benefit from the permission hardening.
        let path = tmpfile("not valid ====");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let _ = load_toml::<FakeConfig>(&path).expect_err("must error");
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn default_config_path_uses_service_subdir() {
        // We can't fully test this without setting HOME, but we
        // can verify it returns Some when HOME is set and that
        // the path ends with the expected suffix.
        if std::env::var_os("HOME").is_some() {
            let p = default_config_path("notion").expect("HOME set");
            let s = p.to_string_lossy();
            assert!(s.ends_with("/.aivyx-pa/tool-processes/notion/config.toml"), "{s}");

            let p = default_config_path("n8n").unwrap();
            let s = p.to_string_lossy();
            assert!(s.ends_with("/.aivyx-pa/tool-processes/n8n/config.toml"), "{s}");
        }
    }

    #[test]
    fn not_found_error_message_includes_path() {
        let e = ConfigFileError::NotFound {
            path: PathBuf::from("/nope/cfg.toml"),
        };
        assert!(e.to_string().contains("/nope/cfg.toml"));
    }

    #[test]
    fn parse_error_message_includes_reason() {
        let e = ConfigFileError::Parse {
            path: PathBuf::from("/a.toml"),
            reason: "expected `=`".to_string(),
        };
        assert!(e.to_string().contains("expected `=`"));
        assert!(e.to_string().contains("/a.toml"));
    }
}
