//! n8n REST API client.
//!
//! Phase 131 Task 2 — skeleton. Token + operator-supplied
//! base URL + `X-N8N-API-KEY` header on every request.

use std::sync::Arc;

use reqwest::{Client, StatusCode};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// API path prefix relative to the operator-supplied base
/// URL. n8n's REST API lives at
/// `{base_url}/api/v1/<resource>`.
pub const N8N_API_PREFIX: &str = "/api/v1";

/// HTTP header name n8n uses for API key auth. Distinct
/// from the standard `Authorization: Bearer <token>`
/// pattern — n8n's own convention.
pub const N8N_API_KEY_HEADER: &str = "X-N8N-API-KEY";

#[derive(Debug, Error)]
pub enum N8nClientError {
    #[error("HTTP transport error: {0}")]
    Transport(String),
    #[error("n8n API returned HTTP {status}: {message}")]
    Api { status: u16, message: String },
    #[error("response parse error: {0}")]
    Parse(String),
    #[error("input validation: {0}")]
    InvalidInput(String),
    #[error("resource not found (HTTP 404)")]
    NotFound,
}

/// Operator-supplied configuration. Two fields: the base
/// URL of the operator's n8n instance + the API key
/// generated in n8n's Settings → API page.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct N8nConfig {
    /// Base URL of the n8n instance — e.g.,
    /// `http://localhost:5678` or
    /// `https://n8n.example.com`. No trailing slash; the
    /// crate appends `/api/v1/<resource>` paths.
    pub n8n_base_url: String,
    /// API key from n8n's Settings → API.
    pub n8n_api_key: String,
}

// Hand-written so a stray `{:?}` never prints the secret (audit P14).
impl std::fmt::Debug for N8nConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("N8nConfig")
            .field("n8n_base_url", &self.n8n_base_url)
            .field("n8n_api_key", &"<redacted>")
            .finish()
    }
}

impl N8nConfig {
    pub fn new(
        n8n_base_url: impl Into<String>,
        n8n_api_key: impl Into<String>,
    ) -> Self {
        Self {
            n8n_base_url: n8n_base_url.into(),
            n8n_api_key: n8n_api_key.into(),
        }
    }
}

pub struct N8nClient {
    http: Client,
    config: N8nConfig,
}

impl N8nClient {
    pub fn new(http: Client, config: N8nConfig) -> Self {
        // Trim trailing slash from base URL to avoid
        // `https://x.com//api/v1/` double-slash bugs.
        let mut config = config;
        while config.n8n_base_url.ends_with('/') {
            config.n8n_base_url.pop();
        }
        Self { http, config }
    }

    fn url_for(&self, path: &str) -> String {
        format!("{}{}{}", self.config.n8n_base_url, N8N_API_PREFIX, path)
    }

    /// GET `{base}/api/v1{path}` with auth header.
    pub async fn get_json<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T, N8nClientError> {
        let resp = self
            .http
            .get(self.url_for(path))
            .header(N8N_API_KEY_HEADER, &self.config.n8n_api_key)
            .query(query)
            .send()
            .await
            .map_err(|e| N8nClientError::Transport(e.to_string()))?;
        decode_response(resp).await
    }

    /// POST JSON body.
    pub async fn post_json<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &Value,
    ) -> Result<T, N8nClientError> {
        let resp = self
            .http
            .post(self.url_for(path))
            .header(N8N_API_KEY_HEADER, &self.config.n8n_api_key)
            .json(body)
            .send()
            .await
            .map_err(|e| N8nClientError::Transport(e.to_string()))?;
        decode_response(resp).await
    }

    /// PATCH JSON body.
    pub async fn patch_json<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &Value,
    ) -> Result<T, N8nClientError> {
        let resp = self
            .http
            .patch(self.url_for(path))
            .header(N8N_API_KEY_HEADER, &self.config.n8n_api_key)
            .json(body)
            .send()
            .await
            .map_err(|e| N8nClientError::Transport(e.to_string()))?;
        decode_response(resp).await
    }

    /// PUT JSON body. n8n's update-workflow endpoint
    /// (`PUT /workflows/{id}`) expects a full replacement
    /// rather than a partial patch.
    pub async fn put_json<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &Value,
    ) -> Result<T, N8nClientError> {
        let resp = self
            .http
            .put(self.url_for(path))
            .header(N8N_API_KEY_HEADER, &self.config.n8n_api_key)
            .json(body)
            .send()
            .await
            .map_err(|e| N8nClientError::Transport(e.to_string()))?;
        decode_response(resp).await
    }

    /// DELETE returning the raw status code so the caller
    /// can distinguish 204/200 from 404 (idempotent
    /// delete pattern).
    pub async fn delete(&self, path: &str) -> Result<StatusCode, N8nClientError> {
        let resp = self
            .http
            .delete(self.url_for(path))
            .header(N8N_API_KEY_HEADER, &self.config.n8n_api_key)
            .send()
            .await
            .map_err(|e| N8nClientError::Transport(e.to_string()))?;
        let status = resp.status();
        if status.is_success() || status == StatusCode::NOT_FOUND {
            Ok(status)
        } else {
            let body = resp
                .text()
                .await
                .unwrap_or_else(|_| "<no body>".to_string());
            Err(N8nClientError::Api {
                status: status.as_u16(),
                message: body,
            })
        }
    }
}

async fn decode_response<T: DeserializeOwned>(
    resp: reqwest::Response,
) -> Result<T, N8nClientError> {
    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| N8nClientError::Transport(e.to_string()))?;
    if status == StatusCode::NOT_FOUND {
        return Err(N8nClientError::NotFound);
    }
    if !status.is_success() {
        return Err(N8nClientError::Api {
            status: status.as_u16(),
            message: body,
        });
    }
    serde_json::from_str(&body)
        .map_err(|e| N8nClientError::Parse(format!("n8n API body: {e}")))
}

pub type SharedN8nClient = Arc<N8nClient>;

#[cfg(test)]
mod tests {
    /// Audit P14: `Debug` of the n8n config hides the key.
    #[test]
    fn debug_hides_the_api_key() {
        let c = N8nConfig { n8n_base_url: "http://x".into(), n8n_api_key: "SECRET-N8N".into() };
        let shown = format!("{c:?}");
        assert!(!shown.contains("SECRET-N8N") && shown.contains("<redacted>"), "{shown}");
    }

    use super::*;

    #[test]
    fn url_prefix_is_api_v1() {
        assert_eq!(N8N_API_PREFIX, "/api/v1");
    }

    #[test]
    fn url_header_name_is_n8n_specific() {
        assert_eq!(N8N_API_KEY_HEADER, "X-N8N-API-KEY");
    }

    #[test]
    fn n8n_client_trims_trailing_slash_from_base_url() {
        let client = N8nClient::new(
            reqwest::Client::new(),
            N8nConfig::new("https://n8n.example.com///", "ntn_x"),
        );
        assert_eq!(client.config.n8n_base_url, "https://n8n.example.com");
    }

    #[test]
    fn url_for_concatenates_correctly() {
        let client = N8nClient::new(
            reqwest::Client::new(),
            N8nConfig::new("https://n8n.example.com", "k"),
        );
        assert_eq!(
            client.url_for("/workflows"),
            "https://n8n.example.com/api/v1/workflows"
        );
    }

    #[test]
    fn url_for_handles_subresource_paths() {
        let client = N8nClient::new(
            reqwest::Client::new(),
            N8nConfig::new("https://x.com", "k"),
        );
        assert_eq!(
            client.url_for("/workflows/abc/execute"),
            "https://x.com/api/v1/workflows/abc/execute"
        );
    }

    #[test]
    fn n8n_config_serde_roundtrip() {
        let cfg = N8nConfig::new("https://n8n.example.com", "ntn_test");
        let json = serde_json::to_value(&cfg).unwrap();
        let back: N8nConfig = serde_json::from_value(json).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn n8n_config_loads_from_toml() {
        let toml = r#"
            n8n_base_url = "https://n8n.example.com"
            n8n_api_key = "ntn_test_xyz"
        "#;
        let cfg: N8nConfig = toml::from_str(toml).expect("parse");
        assert_eq!(cfg.n8n_base_url, "https://n8n.example.com");
        assert_eq!(cfg.n8n_api_key, "ntn_test_xyz");
    }

    #[test]
    fn error_display_includes_status() {
        let e = N8nClientError::Api {
            status: 401,
            message: "Unauthorized".into(),
        };
        assert!(e.to_string().contains("401"));
    }

    #[test]
    fn not_found_error_is_distinct_variant() {
        let e = N8nClientError::NotFound;
        assert!(e.to_string().contains("404"));
    }
}
