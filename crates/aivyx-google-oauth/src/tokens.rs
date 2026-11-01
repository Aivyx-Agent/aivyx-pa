//! OAuth token primitives.
//!
//! Phase 123 Task 2 — the on-disk shape that Task 3's `auth init`
//! writes and Tasks 4-7's Gmail API calls read.
//!
//! Google's OAuth response is documented at
//! <https://developers.google.com/identity/protocols/oauth2/native-app#exchange-authorization-code>.
//! We deserialize the subset of fields we care about and discard
//! the rest (Google adds fields over time; forward-compat).

use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Refresh leeway — refresh when the access token has at most
/// this many seconds remaining. 60s matches Google's documented
/// minimum clock-skew window and gives the refresh round-trip
/// time to complete before any in-flight API call hits a
/// 401-expired response.
pub const REFRESH_LEEWAY_SECS: i64 = 60;

/// Full token set: what Task 3 captures from Google after the
/// auth-code exchange, what Tasks 4-7 read on each Gmail API
/// call, and what [`crate::storage`] persists to disk.
///
/// **Refresh-token handling.** Google issues a refresh token
/// only on the *first* successful auth-code exchange (subsequent
/// exchanges of refresh tokens return `None` for
/// `refresh_token`). On refresh, the loader merges:
/// - new access_token + new expires_at
/// - **preserves** the original refresh_token from disk
///
/// This means once the operator runs `auth init` once, the
/// refresh token persists across refreshes forever (until
/// `auth revoke` or until Google invalidates it for inactivity /
/// password change / scope change).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenSet {
    /// Short-lived access token used as the bearer credential
    /// for Gmail API calls.
    pub access_token: String,
    /// Long-lived refresh token. Only present after the initial
    /// auth-code exchange; subsequent refresh responses return
    /// `None` and the disk-stored token persists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Absolute UNIX timestamp (seconds since epoch) when the
    /// access token expires. Stored absolute rather than as a
    /// `expires_in`-style relative duration so reload-after-
    /// process-restart correctly identifies expired tokens.
    pub expires_at_unix_secs: i64,
    /// Granted scopes (space-delimited as Google returns them).
    /// May be a subset of what was requested if the user
    /// declined some scopes at consent time.
    pub granted_scope: String,
    /// `Bearer` per Google's spec; pinned in case Google ever
    /// returns a different type so the bearer-header
    /// construction can be conditional.
    pub token_type: String,
}

// Hand-written: the derived `Debug` would print both tokens (audit P14).
impl std::fmt::Debug for TokenSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenSet")
            .field("access_token", &"<redacted>")
            .field("refresh_token", &self.refresh_token.as_ref().map(|_| "<redacted>"))
            .field("expires_at_unix_secs", &self.expires_at_unix_secs)
            .field("granted_scope", &self.granted_scope)
            .field("token_type", &self.token_type)
            .finish()
    }
}

impl TokenSet {
    /// Build a `TokenSet` from a fresh auth-code exchange
    /// response. `expires_in_secs` is Google's relative
    /// expires_in field; converted to absolute on construction.
    pub fn from_exchange(
        access_token: String,
        refresh_token: Option<String>,
        expires_in_secs: i64,
        granted_scope: String,
        token_type: String,
    ) -> Self {
        Self {
            access_token,
            refresh_token,
            expires_at_unix_secs: now_unix_secs() + expires_in_secs,
            granted_scope,
            token_type,
        }
    }

    /// Merge a refresh-response into an existing token set.
    /// Preserves the disk-stored refresh_token if the refresh
    /// response didn't include one (Google's typical behavior).
    pub fn merge_refresh_response(
        &self,
        new_access_token: String,
        new_refresh_token: Option<String>,
        new_expires_in_secs: i64,
        new_granted_scope: Option<String>,
        new_token_type: Option<String>,
    ) -> Self {
        Self {
            access_token: new_access_token,
            refresh_token: new_refresh_token.or_else(|| self.refresh_token.clone()),
            expires_at_unix_secs: now_unix_secs() + new_expires_in_secs,
            granted_scope: new_granted_scope
                .unwrap_or_else(|| self.granted_scope.clone()),
            token_type: new_token_type
                .unwrap_or_else(|| self.token_type.clone()),
        }
    }

    /// `true` if the access token is expired or within the
    /// refresh-leeway window. Callers should refresh before
    /// using the access token when this returns `true`.
    pub fn needs_refresh(&self) -> bool {
        self.needs_refresh_at(now_unix_secs())
    }

    /// Test seam — same as [`Self::needs_refresh`] with the
    /// "now" timestamp injectable.
    pub fn needs_refresh_at(&self, now_unix_secs: i64) -> bool {
        self.expires_at_unix_secs - now_unix_secs <= REFRESH_LEEWAY_SECS
    }

    /// `true` iff a refresh token is available to do a refresh.
    /// `false` means the operator must re-run `auth init` from
    /// scratch (no automated recovery path).
    pub fn can_refresh(&self) -> bool {
        self.refresh_token.is_some()
    }

    /// Construct the `Authorization` header value for Gmail API
    /// calls. Format: `Bearer <access_token>`.
    pub fn bearer_header(&self) -> String {
        format!("{} {}", self.token_type, self.access_token)
    }
}

/// Current UNIX timestamp in seconds. `i64` for arithmetic on
/// signed differences (we subtract `now` from
/// `expires_at_unix_secs`).
fn now_unix_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    /// Audit P14: `Debug` of a token set never prints a token.
    #[test]
    fn debug_hides_both_tokens() {
        let t = TokenSet::from_exchange(
            "ya29.ACCESS-SECRET".into(),
            Some("1//REFRESH-SECRET".into()),
            3600,
            "scope".into(),
            "Bearer".into(),
        );
        let shown = format!("{t:?}");
        assert!(!shown.contains("ACCESS-SECRET") && !shown.contains("REFRESH-SECRET"), "{shown}");
        assert!(shown.contains("<redacted>"), "{shown}");
    }

    use super::*;

    fn sample_token_set() -> TokenSet {
        TokenSet {
            access_token: "ya29.access-x".to_string(),
            refresh_token: Some("1//refresh-x".to_string()),
            expires_at_unix_secs: now_unix_secs() + 3600,
            granted_scope: "https://www.googleapis.com/auth/gmail.readonly".to_string(),
            token_type: "Bearer".to_string(),
        }
    }

    #[test]
    fn from_exchange_sets_absolute_expiry() {
        let now = now_unix_secs();
        let ts = TokenSet::from_exchange(
            "a".to_string(),
            Some("r".to_string()),
            3600,
            "scope".to_string(),
            "Bearer".to_string(),
        );
        // Window: expiry should land within [now+3599, now+3601].
        assert!(ts.expires_at_unix_secs >= now + 3599);
        assert!(ts.expires_at_unix_secs <= now + 3601);
    }

    #[test]
    fn needs_refresh_true_when_expired() {
        let mut ts = sample_token_set();
        ts.expires_at_unix_secs = now_unix_secs() - 10;
        assert!(ts.needs_refresh());
    }

    #[test]
    fn needs_refresh_true_within_leeway() {
        let mut ts = sample_token_set();
        // 30s remaining < 60s leeway → refresh.
        ts.expires_at_unix_secs = now_unix_secs() + 30;
        assert!(ts.needs_refresh());
    }

    #[test]
    fn needs_refresh_false_well_before_expiry() {
        let mut ts = sample_token_set();
        ts.expires_at_unix_secs = now_unix_secs() + 600;
        assert!(!ts.needs_refresh());
    }

    #[test]
    fn merge_refresh_preserves_existing_refresh_token() {
        let ts = sample_token_set();
        let merged = ts.merge_refresh_response(
            "ya29.new-access".to_string(),
            None, // Google's typical refresh response: no new refresh_token
            3600,
            None,
            None,
        );
        // Access token replaced; refresh_token preserved from disk.
        assert_eq!(merged.access_token, "ya29.new-access");
        assert_eq!(merged.refresh_token, Some("1//refresh-x".to_string()));
        // Scope + token_type also preserved when refresh response omits them.
        assert_eq!(merged.granted_scope, ts.granted_scope);
        assert_eq!(merged.token_type, ts.token_type);
    }

    #[test]
    fn merge_refresh_overrides_when_refresh_response_supplies_new_refresh_token() {
        let ts = sample_token_set();
        let merged = ts.merge_refresh_response(
            "new-access".to_string(),
            Some("new-refresh".to_string()),
            3600,
            None,
            None,
        );
        assert_eq!(merged.refresh_token, Some("new-refresh".to_string()));
    }

    #[test]
    fn can_refresh_reflects_refresh_token_presence() {
        let mut ts = sample_token_set();
        assert!(ts.can_refresh());
        ts.refresh_token = None;
        assert!(!ts.can_refresh());
    }

    #[test]
    fn bearer_header_uses_token_type_as_scheme() {
        let ts = sample_token_set();
        assert_eq!(ts.bearer_header(), "Bearer ya29.access-x");
    }

    #[test]
    fn needs_refresh_at_uses_injected_now() {
        let ts = sample_token_set();
        // expires_at = now+3600. At now+3000 → 600s remaining → no refresh.
        assert!(!ts.needs_refresh_at(now_unix_secs() + 3000));
        // At now+3550 → 50s remaining < 60s leeway → refresh.
        assert!(ts.needs_refresh_at(now_unix_secs() + 3550));
    }

    #[test]
    fn token_set_serde_roundtrips_with_optional_refresh_token() {
        let with = sample_token_set();
        let json = serde_json::to_value(&with).unwrap();
        let back: TokenSet = serde_json::from_value(json).unwrap();
        assert_eq!(with, back);

        let without = TokenSet {
            refresh_token: None,
            ..sample_token_set()
        };
        let json = serde_json::to_value(&without).unwrap();
        // skip_serializing_if drops the field when None.
        assert!(json.get("refresh_token").is_none());
        let back: TokenSet = serde_json::from_value(json).unwrap();
        assert_eq!(without, back);
    }
}
