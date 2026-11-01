//! Operator-supplied OAuth client configuration.
//!
//! Phase 129 lift from `aivyx-gmail::oauth::config`. The
//! shape is unchanged; the only substantive change is that
//! `DEFAULT_GMAIL_SCOPES` no longer lives here — each
//! consumer crate (`aivyx-gmail`, `aivyx-calendar`,
//! `aivyx-drive`, …) provides its own default scope set as
//! a service-specific constant.
//!
//! **Lifecycle (unchanged):** loaded once at tool-process
//! startup from
//! `~/.aivyx-pa/tool-processes/{service}/config.toml`;
//! passed by-value to [`crate::exchange::exchange_code`] +
//! [`crate::exchange::refresh_access_token`] as needed.

use serde::{Deserialize, Serialize};

/// Operator-provided OAuth client configuration.
///
/// **Privacy posture:** `client_secret` is sensitive but not
/// cryptographically valuable on its own — without `client_id`
/// it's useless, and Google's OAuth flow binds the secret to
/// the redirect URI registered against the OAuth app. Still
/// stored in a 0600 file alongside the tokens; not logged.
///
/// **Scopes** — per-service. Each consumer crate provides a
/// `DEFAULT_X_SCOPES: &[&str]` constant; the consumer's
/// factory function chains `.with_scopes(...)` on top of the
/// no-scope [`Self::new`] constructor to populate them. This
/// lift removes the Gmail-specific default from the shared
/// substrate.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthConfig {
    /// Google Cloud OAuth 2.0 client ID. Format:
    /// `XXXXXX-XXXXXXXX.apps.googleusercontent.com`.
    pub client_id: String,
    /// Google Cloud OAuth 2.0 client secret. Bound to
    /// `redirect_uri`.
    pub client_secret: String,
    /// Redirect URI registered against the OAuth app in Google
    /// Cloud Console. For Aivyx-local use this is always a
    /// loopback URI like `http://127.0.0.1:<port>/callback`.
    /// Operators may also use `urn:ietf:wg:oauth:2.0:oob` for
    /// headless setups but it's deprecated by Google.
    pub redirect_uri: String,
    /// OAuth scopes to request at consent time. No substrate-
    /// level default — each consumer crate provides its own
    /// `DEFAULT_X_SCOPES` constant chained via
    /// [`Self::with_scopes`].
    #[serde(default)]
    pub scopes: Vec<String>,
}

// Hand-written so a stray `{:?}` never prints the secret (audit P14).
impl std::fmt::Debug for OAuthConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthConfig")
            .field("client_id", &self.client_id)
            .field("client_secret", &"<redacted>")
            .field("redirect_uri", &self.redirect_uri)
            .field("scopes", &self.scopes)
            .finish()
    }
}

impl OAuthConfig {
    /// Build an OAuthConfig with EMPTY scopes. Consumers must
    /// chain [`Self::with_scopes`] to populate their service's
    /// default scope set; the substrate has no opinion on
    /// which scopes are appropriate.
    pub fn new(
        client_id: impl Into<String>,
        client_secret: impl Into<String>,
        redirect_uri: impl Into<String>,
    ) -> Self {
        Self {
            client_id: client_id.into(),
            client_secret: client_secret.into(),
            redirect_uri: redirect_uri.into(),
            scopes: Vec::new(),
        }
    }

    /// Replace the current scope set with the supplied scopes.
    /// Consumers chain this on top of [`Self::new`] in their
    /// service-specific factory.
    pub fn with_scopes<I, S>(mut self, scopes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.scopes = scopes.into_iter().map(Into::into).collect();
        self
    }

    /// Render the scope set as a space-delimited string, as
    /// required by both Google's authorization endpoint and the
    /// token endpoint.
    pub fn scopes_space_delimited(&self) -> String {
        self.scopes.join(" ")
    }
}

#[cfg(test)]
mod tests {
    /// Audit P14: `Debug` of the OAuth client config hides the client secret.
    #[test]
    fn debug_hides_the_client_secret() {
        let c = OAuthConfig {
            client_id: "id".into(),
            client_secret: "SECRET-CLIENT".into(),
            redirect_uri: "http://localhost".into(),
            scopes: vec![],
        };
        let shown = format!("{c:?}");
        assert!(!shown.contains("SECRET-CLIENT") && shown.contains("<redacted>"), "{shown}");
    }

    use super::*;

    #[test]
    fn new_populates_empty_scopes() {
        let cfg = OAuthConfig::new("id", "secret", "http://127.0.0.1:1234/cb");
        assert_eq!(cfg.client_id, "id");
        assert_eq!(cfg.client_secret, "secret");
        assert_eq!(cfg.redirect_uri, "http://127.0.0.1:1234/cb");
        assert!(
            cfg.scopes.is_empty(),
            "substrate-level new() produces empty scopes; consumers chain .with_scopes()"
        );
    }

    #[test]
    fn with_scopes_populates() {
        let cfg = OAuthConfig::new("id", "secret", "http://localhost:0/cb")
            .with_scopes([
                "https://www.googleapis.com/auth/drive",
                "https://www.googleapis.com/auth/calendar",
            ]);
        assert_eq!(cfg.scopes.len(), 2);
    }

    #[test]
    fn scopes_space_delimited_joins_with_single_space() {
        let cfg = OAuthConfig::new("id", "secret", "http://localhost:0/cb")
            .with_scopes([
                "https://www.googleapis.com/auth/drive",
                "https://www.googleapis.com/auth/calendar",
            ]);
        let joined = cfg.scopes_space_delimited();
        // Two scopes → one space between them.
        assert_eq!(joined.matches(' ').count(), 1);
        for scope in &cfg.scopes {
            assert!(joined.contains(scope), "missing {scope}");
        }
    }

    #[test]
    fn config_serde_roundtrips_through_toml_shape() {
        // The config file is TOML at the consumer layer;
        // verify the serde derive accepts a JSON shape that
        // matches what TOML deserialization will produce
        // (TOML maps cleanly to JSON for our flat field set).
        let cfg = OAuthConfig::new("id-x", "secret-x", "http://127.0.0.1:8765/cb")
            .with_scopes(["https://www.googleapis.com/auth/drive"]);
        let json = serde_json::to_value(&cfg).unwrap();
        let back: OAuthConfig = serde_json::from_value(json).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn config_deserialize_without_scopes_yields_empty_vec() {
        // Operator omits scopes field → empty Vec via
        // serde's `#[serde(default)]`. Consumer code is
        // responsible for populating defaults at the
        // factory layer.
        let json = serde_json::json!({
            "client_id": "id",
            "client_secret": "secret",
            "redirect_uri": "http://127.0.0.1:0/cb",
        });
        let cfg: OAuthConfig = serde_json::from_value(json).unwrap();
        assert!(cfg.scopes.is_empty());
    }
}
