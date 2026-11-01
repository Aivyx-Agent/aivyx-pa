//! Notion v1 REST API client.
//!
//! Phase 130 Task 2 — skeleton; per-tool tasks 3-9 fill
//! in the actual HTTP operations. Mirrors the
//! `DriveClient` / `CalendarClient` shape but with two
//! Notion-specific twists:
//!
//! 1. **Bearer-token auth** (no OAuth refresh dance) —
//!    the token comes directly from the operator's
//!    config.toml; no on-disk token state.
//! 2. **`Notion-Version` header** — Notion's API
//!    requires a date-pinned version header on every
//!    request. The crate-level
//!    [`crate::NOTION_VERSION`] constant carries the
//!    pin.

use std::sync::Arc;

use reqwest::Client;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::{NOTION_API_BASE, NOTION_VERSION};

#[derive(Debug, Error)]
pub enum NotionClientError {
    #[error("HTTP transport error: {0}")]
    Transport(String),
    #[error("Notion API returned HTTP {status}: {message}")]
    Api { status: u16, message: String },
    #[error("response parse error: {0}")]
    Parse(String),
    #[error("input validation: {0}")]
    InvalidInput(String),
    /// Notion-specific: integration doesn't have access to
    /// the requested resource because the operator hasn't
    /// shared it via the Notion UI. Distinct from generic
    /// 404 so the tool layer can surface an actionable
    /// error message.
    #[error(
        "page or database not accessible — operator may need to share it with the integration via Notion's UI (Share → Invite → select the integration). Underlying API response: {0}"
    )]
    NotShared(String),
}

/// Operator-supplied configuration. Mirrors the OAuth-
/// flavored crates' `OAuthConfig` posture but much
/// smaller — just the integration token.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotionConfig {
    /// Notion Integration Token. Format:
    /// `ntn_XXXXXXXXXXXXXXXXXXX` or the older
    /// `secret_XXXXXXXX` format.
    pub notion_token: String,
}

// Hand-written so a stray `{:?}` never prints the secret (audit P14).
impl std::fmt::Debug for NotionConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NotionConfig")
            .field("notion_token", &"<redacted>")
            .finish()
    }
}

impl NotionConfig {
    pub fn new(notion_token: impl Into<String>) -> Self {
        Self {
            notion_token: notion_token.into(),
        }
    }
}

/// Token-authenticated Notion API client.
pub struct NotionClient {
    http: Client,
    config: NotionConfig,
}

impl NotionClient {
    pub fn new(http: Client, config: NotionConfig) -> Self {
        Self { http, config }
    }

    /// GET `{NOTION_API_BASE}{path}` with bearer auth +
    /// version header + caller-supplied query params.
    pub async fn get_json<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<T, NotionClientError> {
        let url = format!("{}{}", NOTION_API_BASE, path);
        let resp = self
            .http
            .get(&url)
            .bearer_auth(&self.config.notion_token)
            .header("Notion-Version", NOTION_VERSION)
            .query(query)
            .send()
            .await
            .map_err(|e| NotionClientError::Transport(e.to_string()))?;
        decode_response(resp).await
    }

    /// POST `{NOTION_API_BASE}{path}` with JSON body.
    pub async fn post_json<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &Value,
    ) -> Result<T, NotionClientError> {
        let url = format!("{}{}", NOTION_API_BASE, path);
        let resp = self
            .http
            .post(&url)
            .bearer_auth(&self.config.notion_token)
            .header("Notion-Version", NOTION_VERSION)
            .json(body)
            .send()
            .await
            .map_err(|e| NotionClientError::Transport(e.to_string()))?;
        decode_response(resp).await
    }

    /// PATCH `{NOTION_API_BASE}{path}` with JSON body.
    /// Used by `notion.update_page_properties` and
    /// `notion.archive_page` (archiving is a property
    /// patch on the page, not a DELETE).
    pub async fn patch_json<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &Value,
    ) -> Result<T, NotionClientError> {
        let url = format!("{}{}", NOTION_API_BASE, path);
        let resp = self
            .http
            .patch(&url)
            .bearer_auth(&self.config.notion_token)
            .header("Notion-Version", NOTION_VERSION)
            .json(body)
            .send()
            .await
            .map_err(|e| NotionClientError::Transport(e.to_string()))?;
        decode_response(resp).await
    }
}

async fn decode_response<T: DeserializeOwned>(
    resp: reqwest::Response,
) -> Result<T, NotionClientError> {
    let status = resp.status();
    let body = resp
        .text()
        .await
        .map_err(|e| NotionClientError::Transport(e.to_string()))?;
    if !status.is_success() {
        // Notion returns 404 + a code "object_not_found"
        // when the integration hasn't been shared with the
        // requested resource. Surface this distinctly so
        // the tool layer can give an actionable error
        // message pointing at the share-with-integration
        // step.
        if status.as_u16() == 404 && body.contains("object_not_found") {
            return Err(NotionClientError::NotShared(body));
        }
        return Err(NotionClientError::Api {
            status: status.as_u16(),
            message: body,
        });
    }
    serde_json::from_str(&body).map_err(|e| {
        NotionClientError::Parse(format!("Notion API body: {e}"))
    })
}

pub type SharedNotionClient = Arc<NotionClient>;

#[cfg(test)]
mod tests {
    /// Audit P14: `Debug` of the Notion config hides the token.
    #[test]
    fn debug_hides_the_token() {
        let c = NotionConfig { notion_token: "SECRET-NOTION".into() };
        let shown = format!("{c:?}");
        assert!(!shown.contains("SECRET-NOTION") && shown.contains("<redacted>"), "{shown}");
    }

    use super::*;

    #[test]
    fn api_base_is_v1() {
        assert!(NOTION_API_BASE.ends_with("/v1"));
    }

    #[test]
    fn notion_version_is_date_pinned() {
        // Just a stability test — the pin shouldn't move
        // accidentally. Bump via a deliberate substrate
        // phase if Notion deprecates the version.
        assert_eq!(NOTION_VERSION, "2022-06-28");
    }

    #[test]
    fn notion_client_error_display_includes_status() {
        let e = NotionClientError::Api {
            status: 403,
            message: "forbidden".into(),
        };
        assert!(e.to_string().contains("403"));
    }

    #[test]
    fn not_shared_error_mentions_share_step() {
        let e = NotionClientError::NotShared("object_not_found".into());
        let s = e.to_string();
        assert!(s.contains("Share"), "should mention the share step");
        assert!(s.contains("integration"), "should mention the integration");
    }

    #[test]
    fn notion_config_serde_round_trip() {
        let cfg = NotionConfig::new("ntn_abc123");
        let json = serde_json::to_value(&cfg).unwrap();
        let back: NotionConfig = serde_json::from_value(json).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn notion_config_loads_from_toml() {
        let toml_body = r#"
            notion_token = "ntn_test_token"
        "#;
        let cfg: NotionConfig = toml::from_str(toml_body).expect("parse");
        assert_eq!(cfg.notion_token, "ntn_test_token");
    }
}
