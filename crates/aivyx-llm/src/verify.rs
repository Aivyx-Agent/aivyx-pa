//! Phase 104 — model verification against a local server.
//!
//! `GET {base_url}/v1/models` on an OpenAI-compatible server (llama-server,
//! Jan, Lemonade, the broker, …) returns `{ data: [ { id, … }, … ] }`.
//! [`verify_model_listed`] issues that GET, parses the list and confirms
//! the chosen model is present. Errors carry enough detail for a caller to
//! re-prompt the right field (auth → key; model-not-found → model).
//!
//! 2.0 (local-only): the cloud key checks (Anthropic's and OpenAI's own
//! `/v1/models`) are gone. `base_url` must be local; it is checked before
//! the request, and the client refuses redirects and non-local addresses.

use std::time::Duration;

use thiserror::Error;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Why [`verify_model_listed`] failed. The variants are discriminated by
/// failure class so a caller knows which prompt to re-show on retry —
/// auth errors → re-prompt key, model-not-found → re-prompt model,
/// network/other → both.
#[derive(Debug, Error)]
pub enum VerifyError {
    /// 401 / 403 from the server. The supplied API key is missing,
    /// malformed, or revoked.
    #[error("authentication failed: {0}")]
    Auth(String),

    /// The server answered but the requested model id was not present in
    /// the returned list. `available` is a human-readable summary of what
    /// *is* available.
    #[error(
        "model `{model}` is not available on this server; \
         available models: {available}"
    )]
    ModelNotFound { model: String, available: String },

    /// Network-layer failure — DNS, connect timeout, TLS, a non-local
    /// URL, etc.
    #[error("network error: {0}")]
    Network(String),

    /// The server returned a non-auth non-success status, or the response
    /// body did not match the expected shape.
    #[error("provider returned unexpected response: {0}")]
    Other(String),
}

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Connect + total timeout for the verify GET. Short — callers are
/// interactive and a stalled verify is worse than a crisp `Network` error
/// the operator can retry from.
const VERIFY_TIMEOUT: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

/// Verify that `model` is listed by the local server at `base_url`
/// (`GET {base_url}/v1/models`, with `api_key` as a bearer token when
/// given). `base_url` must be local.
pub async fn verify_model_listed(
    base_url: &str,
    api_key: Option<&str>,
    model: &str,
) -> Result<(), VerifyError> {
    let url = format!("{}/v1/models", base_url.trim_end_matches('/'));
    crate::local_only::require_local_async(&url)
        .await
        .map_err(|e| VerifyError::Network(e.to_string()))?;

    let client = crate::local_only::model_client_builder()
        .timeout(VERIFY_TIMEOUT)
        .connect_timeout(VERIFY_TIMEOUT)
        .build()
        .map_err(|e| VerifyError::Network(format!("failed to build HTTP client: {e}")))?;

    let mut req = client.get(&url);
    if let Some(key) = api_key {
        req = req.header("Authorization", format!("Bearer {key}"));
    }

    let resp = req
        .send()
        .await
        .map_err(|e| VerifyError::Network(format!("failed to reach {url}: {e}")))?;

    let status = resp.status().as_u16();
    let body = resp
        .text()
        .await
        .map_err(|e| VerifyError::Network(format!("failed to read response: {e}")))?;

    classify_response(status, &body, model)
}

// ---------------------------------------------------------------------------
// Pure response classification (extracted for testability)
// ---------------------------------------------------------------------------

/// Map an HTTP status + response body + chosen model to a
/// [`Result<(), VerifyError>`]. Extracted from
/// [`verify_model_listed`] so unit tests can drive every
/// branch without a real HTTP server.
pub(crate) fn classify_response(
    status: u16,
    body: &str,
    model: &str,
) -> Result<(), VerifyError> {
    if status == 401 || status == 403 {
        return Err(VerifyError::Auth(format!(
            "HTTP {status}: {}",
            truncate(body, 200)
        )));
    }

    if !(200..300).contains(&status) {
        return Err(VerifyError::Other(format!(
            "HTTP {status}: {}",
            truncate(body, 200)
        )));
    }

    let models = parse_models_response(body)
        .map_err(|e| VerifyError::Other(format!("unparseable models list: {e}")))?;

    if models.iter().any(|id| id == model) {
        Ok(())
    } else {
        Err(VerifyError::ModelNotFound {
            model: model.to_string(),
            available: summarize_models(&models, 8),
        })
    }
}

/// Parse the `{ data: [ { id, … }, … ] }` shape shared by
/// OpenAI-compatible `/v1/models` responses.
pub(crate) fn parse_models_response(body: &str) -> Result<Vec<String>, String> {
    let v: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| format!("invalid JSON: {e}"))?;

    let arr = v
        .get("data")
        .and_then(|d| d.as_array())
        .ok_or_else(|| "missing or non-array `data` field".to_string())?;

    let ids = arr
        .iter()
        .filter_map(|m| m.get("id").and_then(|i| i.as_str()).map(String::from))
        .collect::<Vec<_>>();

    Ok(ids)
}

/// Truncate a string to `max` chars, appending `…` if cut. Used
/// when echoing provider error bodies into [`VerifyError`] messages
/// so a multi-KB HTML 5xx page doesn't blow up the wizard's
/// re-prompt output.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max).collect();
        format!("{head}…")
    }
}

/// Render up to `cap` model ids comma-separated, with a tail
/// `(+N more)` when truncated. Used in [`VerifyError::ModelNotFound`]
/// so the wizard's retry prompt can show the operator what is
/// available.
pub(crate) fn summarize_models(models: &[String], cap: usize) -> String {
    if models.is_empty() {
        return "(none returned)".to_string();
    }
    let shown: Vec<&str> = models.iter().take(cap).map(|s| s.as_str()).collect();
    let suffix = if models.len() > cap {
        format!(", … (+{} more)", models.len() - cap)
    } else {
        String::new()
    };
    format!("{}{}", shown.join(", "), suffix)
}

// ---------------------------------------------------------------------------
// Tests — pure-function coverage only. The live HTTP path in
// `verify_model_listed` is integration-tested in the
// wizard's `cargo test` suite via the same model the rest of
// `aivyx-llm` uses: the parse + classify helpers are exhaustively
// unit-tested, and the HTTP layer is treated as
// integration-tested-only (matching the wizard's existing
// `parse_model_names` / `detect_ollama` split in
// `aivyx_modules/init.rs`).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_non_local_server_is_refused_before_any_request() {
        for base in ["https://api.openai.com", "https://api.anthropic.com", "http://203.0.113.7:8080"] {
            match verify_model_listed(base, Some("sk-x"), "m").await {
                Err(VerifyError::Network(msg)) => assert!(msg.contains("is not local"), "{msg}"),
                other => panic!("{base}: expected a local-only refusal, got {other:?}"),
            }
        }
    }

    // -- parse_models_response -------------------------------------------

    #[test]
    fn parse_extracts_ids_from_data_array() {
        let body = r#"{
            "data": [
                { "id": "claude-sonnet-4-6", "type": "model" },
                { "id": "claude-opus-4-7", "type": "model" }
            ]
        }"#;
        let ids = parse_models_response(body).unwrap();
        assert_eq!(ids, vec!["claude-sonnet-4-6", "claude-opus-4-7"]);
    }

    #[test]
    fn parse_skips_entries_without_id() {
        let body = r#"{ "data": [
            { "id": "gpt-4.1" },
            { "type": "model" },
            { "id": "gpt-4.1-mini" }
        ] }"#;
        let ids = parse_models_response(body).unwrap();
        assert_eq!(ids, vec!["gpt-4.1", "gpt-4.1-mini"]);
    }

    #[test]
    fn parse_empty_data_returns_empty_vec() {
        let body = r#"{ "data": [] }"#;
        let ids = parse_models_response(body).unwrap();
        assert!(ids.is_empty());
    }

    #[test]
    fn parse_missing_data_errors() {
        let body = r#"{ "models": [] }"#;
        assert!(parse_models_response(body).is_err());
    }

    #[test]
    fn parse_invalid_json_errors() {
        assert!(parse_models_response("not json").is_err());
    }

    // -- classify_response -----------------------------------------------

    fn body_with_models(ids: &[&str]) -> String {
        let entries: Vec<String> = ids
            .iter()
            .map(|id| format!(r#"{{"id":"{id}"}}"#))
            .collect();
        format!(r#"{{"data":[{}]}}"#, entries.join(","))
    }

    #[test]
    fn classify_ok_when_model_present() {
        let body = body_with_models(&["claude-sonnet-4-6", "claude-opus-4-7"]);
        let result = classify_response(200, &body, "claude-sonnet-4-6");
        assert!(matches!(result, Ok(())));
    }

    #[test]
    fn classify_401_is_auth() {
        let result = classify_response(401, "{\"error\":\"bad key\"}", "any");
        assert!(matches!(result, Err(VerifyError::Auth(_))));
    }

    #[test]
    fn classify_403_is_auth() {
        let result = classify_response(403, "forbidden", "any");
        assert!(matches!(result, Err(VerifyError::Auth(_))));
    }

    #[test]
    fn classify_500_is_other() {
        let result = classify_response(500, "<html>oops</html>", "any");
        assert!(matches!(result, Err(VerifyError::Other(_))));
    }

    #[test]
    fn classify_model_not_in_returned_list() {
        let body = body_with_models(&["claude-sonnet-4-6"]);
        let result = classify_response(200, &body, "claude-typo-4-6");
        match result {
            Err(VerifyError::ModelNotFound { model, available }) => {
                assert_eq!(model, "claude-typo-4-6");
                assert!(available.contains("claude-sonnet-4-6"));
            }
            other => panic!("expected ModelNotFound, got {other:?}"),
        }
    }

    #[test]
    fn classify_200_with_unparseable_body_is_other() {
        let result = classify_response(200, "not json at all", "any");
        assert!(matches!(result, Err(VerifyError::Other(_))));
    }

    #[test]
    fn classify_truncates_long_error_bodies() {
        let huge = "x".repeat(5000);
        let err = classify_response(500, &huge, "any").unwrap_err();
        let msg = err.to_string();
        // Body is truncated to 200 chars + "…" in the error message.
        assert!(msg.len() < 500, "error message too long: {} chars", msg.len());
        assert!(msg.contains('…'));
    }

    // -- summarize_models ------------------------------------------------

    #[test]
    fn summarize_empty_list() {
        assert_eq!(summarize_models(&[], 8), "(none returned)");
    }

    #[test]
    fn summarize_within_cap() {
        let models = vec!["a".to_string(), "b".to_string()];
        assert_eq!(summarize_models(&models, 8), "a, b");
    }

    #[test]
    fn summarize_truncates_with_count() {
        let models: Vec<String> = (0..12).map(|i| format!("m{i}")).collect();
        let s = summarize_models(&models, 3);
        assert!(s.starts_with("m0, m1, m2"));
        assert!(s.contains("+9 more"));
    }
}
