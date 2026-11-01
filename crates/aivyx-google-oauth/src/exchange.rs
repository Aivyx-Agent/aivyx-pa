//! OAuth 2.0 token-endpoint HTTP client.
//!
//! Phase 123 Task 2. Two operations:
//!
//! 1. [`exchange_code`] — auth-code → token set. Called once by
//!    Task 3's `aivyx-gmail auth init` after the operator
//!    completes the consent flow.
//! 2. [`refresh_access_token`] — refresh-token → fresh access
//!    token. Called by Tasks 4-7's Gmail API tools when the
//!    on-disk token set's [`TokenSet::needs_refresh`] returns
//!    true.
//!
//! Both speak HTTP against Google's token endpoint
//! (`https://oauth2.googleapis.com/token`) with
//! `application/x-www-form-urlencoded` request bodies, per the
//! OAuth 2.0 spec and Google's documented flow.
//!
//! Test seam: both functions take a `&reqwest::Client` plus a
//! `token_endpoint: &str` so unit tests can point at a mock
//! server (httpmock-style) without monkey-patching globals or
//! pulling in a heavy dev-dep.

use reqwest::Client;
use serde::Deserialize;
use thiserror::Error;

use super::config::OAuthConfig;
use super::tokens::TokenSet;

/// Google's OAuth 2.0 authorization endpoint. The CLI in Task 3
/// builds the consent URL by appending query parameters to this
/// base.
pub const GOOGLE_AUTH_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/v2/auth";

/// Google's OAuth 2.0 token endpoint. Both `exchange_code` and
/// `refresh_access_token` POST here.
pub const GOOGLE_TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";

#[derive(Debug, Error)]
pub enum ExchangeError {
    #[error("HTTP request to token endpoint failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("token endpoint returned status {status}; body: {body}")]
    NonSuccessStatus { status: u16, body: String },
    #[error("token endpoint response missing expected field: {0}")]
    MissingField(&'static str),
    #[error("token endpoint response failed to parse: {0}")]
    MalformedResponse(String),
}

/// Wire-shape of Google's token-endpoint response. Fields
/// `Option` so a partial response (refresh response without
/// refresh_token is normal) deserializes cleanly.
#[derive(Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
    scope: Option<String>,
    token_type: Option<String>,
}

// Hand-written so a stray `{:?}` never prints the secret (audit P14).
impl std::fmt::Debug for TokenResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenResponse")
            .field("access_token", &"<redacted>")
            .field("refresh_token", &"<redacted>")
            .field("expires_in", &self.expires_in)
            .field("scope", &self.scope)
            .field("token_type", &self.token_type)
            .finish()
    }
}

/// Exchange an authorization code for a token set.
///
/// Called once per `auth init` run. Google issues the
/// refresh_token only on this initial exchange (subsequent
/// refreshes don't return one); the returned `TokenSet` is the
/// authoritative one to persist via
/// [`crate::storage::save_tokens`].
pub async fn exchange_code(
    client: &Client,
    token_endpoint: &str,
    config: &OAuthConfig,
    auth_code: &str,
) -> Result<TokenSet, ExchangeError> {
    let params = [
        ("code", auth_code),
        ("client_id", config.client_id.as_str()),
        ("client_secret", config.client_secret.as_str()),
        ("redirect_uri", config.redirect_uri.as_str()),
        ("grant_type", "authorization_code"),
    ];
    let response = post_form(client, token_endpoint, &params).await?;
    let access_token = response
        .access_token
        .ok_or(ExchangeError::MissingField("access_token"))?;
    let expires_in = response
        .expires_in
        .ok_or(ExchangeError::MissingField("expires_in"))?;
    let granted_scope = response
        .scope
        .unwrap_or_else(|| config.scopes_space_delimited());
    let token_type = response
        .token_type
        .unwrap_or_else(|| "Bearer".to_string());
    Ok(TokenSet::from_exchange(
        access_token,
        response.refresh_token,
        expires_in,
        granted_scope,
        token_type,
    ))
}

/// Refresh the access token using the on-disk refresh token.
///
/// Returns a new [`TokenSet`] merged from the refresh response
/// and the existing on-disk token set (preserves the
/// refresh_token when Google's response omits it, which is the
/// common case). Callers persist the result via
/// [`crate::storage::save_tokens`].
///
/// Returns `MissingField("refresh_token")` if `existing` has no
/// refresh token — operator must re-run `auth init`.
pub async fn refresh_access_token(
    client: &Client,
    token_endpoint: &str,
    config: &OAuthConfig,
    existing: &TokenSet,
) -> Result<TokenSet, ExchangeError> {
    let refresh_token = existing
        .refresh_token
        .as_deref()
        .ok_or(ExchangeError::MissingField("refresh_token"))?;
    let params = [
        ("refresh_token", refresh_token),
        ("client_id", config.client_id.as_str()),
        ("client_secret", config.client_secret.as_str()),
        ("grant_type", "refresh_token"),
    ];
    let response = post_form(client, token_endpoint, &params).await?;
    let access_token = response
        .access_token
        .ok_or(ExchangeError::MissingField("access_token"))?;
    let expires_in = response
        .expires_in
        .ok_or(ExchangeError::MissingField("expires_in"))?;
    Ok(existing.merge_refresh_response(
        access_token,
        response.refresh_token,
        expires_in,
        response.scope,
        response.token_type,
    ))
}

async fn post_form(
    client: &Client,
    endpoint: &str,
    params: &[(&str, &str)],
) -> Result<TokenResponse, ExchangeError> {
    let response = client
        .post(endpoint)
        .form(params)
        .send()
        .await
        .map_err(ExchangeError::Http)?;
    let status = response.status();
    let body = response.text().await.map_err(ExchangeError::Http)?;
    if !status.is_success() {
        return Err(ExchangeError::NonSuccessStatus {
            status: status.as_u16(),
            body,
        });
    }
    serde_json::from_str::<TokenResponse>(&body)
        .map_err(|e| ExchangeError::MalformedResponse(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::OAuthConfig;
    use std::sync::Arc;
    use std::sync::Mutex;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpListener;

    fn sample_config() -> OAuthConfig {
        OAuthConfig::new("test-id.apps.googleusercontent.com", "test-secret", "http://127.0.0.1:0/cb")
    }

    /// Minimal in-process mock OAuth server. Spawns a TCP
    /// listener, reads one HTTP request, captures the
    /// form-encoded body, and writes a canned response. Returns
    /// `(endpoint_url, captured_body)`.
    async fn spawn_mock_token_endpoint(
        canned_response_body: String,
        canned_status: u16,
    ) -> (String, Arc<Mutex<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let captured = Arc::new(Mutex::new(String::new()));
        let captured_clone = Arc::clone(&captured);

        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let (read_half, mut write_half) = sock.split();
            let mut reader = BufReader::new(read_half);

            // Parse request line + headers.
            let mut content_length: usize = 0;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).await.unwrap() == 0 {
                    break;
                }
                if line == "\r\n" {
                    break;
                }
                if let Some(rest) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    content_length = rest.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; content_length];
            reader.read_exact(&mut body).await.unwrap();
            *captured_clone.lock().unwrap() = String::from_utf8(body).unwrap();

            let status_phrase = match canned_status {
                200 => "OK",
                400 => "Bad Request",
                401 => "Unauthorized",
                _ => "OK",
            };
            let response = format!(
                "HTTP/1.1 {canned_status} {status_phrase}\r\n\
                 Content-Type: application/json\r\n\
                 Content-Length: {}\r\n\
                 Connection: close\r\n\
                 \r\n\
                 {}",
                canned_response_body.len(),
                canned_response_body,
            );
            write_half.write_all(response.as_bytes()).await.unwrap();
            write_half.flush().await.unwrap();
        });

        (format!("http://127.0.0.1:{port}/token"), captured)
    }

    #[tokio::test]
    async fn exchange_code_posts_form_with_required_fields() {
        let canned = serde_json::json!({
            "access_token": "ya29.access-x",
            "refresh_token": "1//refresh-x",
            "expires_in": 3600,
            "scope": "https://www.googleapis.com/auth/gmail.readonly",
            "token_type": "Bearer",
        })
        .to_string();
        let (endpoint, captured) = spawn_mock_token_endpoint(canned, 200).await;
        let client = Client::new();
        let config = sample_config();
        let tokens = exchange_code(&client, &endpoint, &config, "AUTH-CODE-XYZ")
            .await
            .expect("exchange");
        // Wire-shape: form body has all five required params.
        let body = captured.lock().unwrap().clone();
        assert!(body.contains("code=AUTH-CODE-XYZ"), "body missing code: {body}");
        assert!(body.contains("client_id=test-id"), "body missing client_id");
        assert!(body.contains("client_secret=test-secret"), "body missing client_secret");
        assert!(body.contains("grant_type=authorization_code"));
        // Returned token set has fields populated.
        assert_eq!(tokens.access_token, "ya29.access-x");
        assert_eq!(tokens.refresh_token, Some("1//refresh-x".to_string()));
    }

    #[tokio::test]
    async fn exchange_code_propagates_non_success_status() {
        let canned = r#"{"error":"invalid_grant"}"#.to_string();
        let (endpoint, _) = spawn_mock_token_endpoint(canned, 400).await;
        let client = Client::new();
        let config = sample_config();
        match exchange_code(&client, &endpoint, &config, "BAD-CODE").await {
            Err(ExchangeError::NonSuccessStatus { status, body }) => {
                assert_eq!(status, 400);
                assert!(body.contains("invalid_grant"));
            }
            other => panic!("expected NonSuccessStatus; got {other:?}"),
        }
    }

    #[tokio::test]
    async fn exchange_code_surfaces_missing_access_token() {
        // Successful HTTP status but no access_token in the body.
        let canned = r#"{"refresh_token":"x","expires_in":3600}"#.to_string();
        let (endpoint, _) = spawn_mock_token_endpoint(canned, 200).await;
        let client = Client::new();
        match exchange_code(&client, &endpoint, &sample_config(), "code").await {
            Err(ExchangeError::MissingField(f)) => assert_eq!(f, "access_token"),
            other => panic!("expected MissingField; got {other:?}"),
        }
    }

    #[tokio::test]
    async fn refresh_preserves_existing_refresh_token() {
        // Google's typical refresh response: NO refresh_token.
        let canned = serde_json::json!({
            "access_token": "ya29.new-access",
            "expires_in": 3600,
            "token_type": "Bearer",
        })
        .to_string();
        let (endpoint, captured) = spawn_mock_token_endpoint(canned, 200).await;
        let client = Client::new();
        let existing = TokenSet {
            access_token: "ya29.old".to_string(),
            refresh_token: Some("1//refresh-existing".to_string()),
            expires_at_unix_secs: 0,
            granted_scope: "scope-x".to_string(),
            token_type: "Bearer".to_string(),
        };
        let refreshed =
            refresh_access_token(&client, &endpoint, &sample_config(), &existing)
                .await
                .expect("refresh");
        // Body sent the existing refresh_token.
        let body = captured.lock().unwrap().clone();
        assert!(body.contains("refresh_token=1%2F%2Frefresh-existing"), "body: {body}");
        assert!(body.contains("grant_type=refresh_token"));
        // New access token.
        assert_eq!(refreshed.access_token, "ya29.new-access");
        // Disk refresh_token preserved.
        assert_eq!(
            refreshed.refresh_token,
            Some("1//refresh-existing".to_string()),
            "refresh_token must be preserved when Google omits it"
        );
        // Scope preserved from on-disk when response omits it.
        assert_eq!(refreshed.granted_scope, "scope-x");
    }

    #[tokio::test]
    async fn refresh_errors_when_existing_lacks_refresh_token() {
        let client = Client::new();
        let existing = TokenSet {
            access_token: "ya29.x".to_string(),
            refresh_token: None,
            expires_at_unix_secs: 0,
            granted_scope: "scope".to_string(),
            token_type: "Bearer".to_string(),
        };
        match refresh_access_token(
            &client,
            "http://unused.invalid/token",
            &sample_config(),
            &existing,
        )
        .await
        {
            Err(ExchangeError::MissingField(f)) => assert_eq!(f, "refresh_token"),
            other => panic!("expected MissingField; got {other:?}"),
        }
    }

    #[test]
    fn endpoints_are_google_official_urls() {
        assert!(GOOGLE_AUTH_ENDPOINT.starts_with("https://accounts.google.com"));
        assert!(GOOGLE_TOKEN_ENDPOINT.starts_with("https://oauth2.googleapis.com"));
    }
}
