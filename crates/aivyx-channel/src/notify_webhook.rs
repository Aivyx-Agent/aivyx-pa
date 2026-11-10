//! Phase 62 Task 6 — Generic webhook notification backend.
//!
//! HTTP POST with a small JSON body to an operator-configured
//! `https://...` URL. Covers ntfy.sh, Pushover, IFTTT, and
//! custom endpoints. Slack-flavored payload (`{text: ...}`) is a
//! Phase 62 deferral — it's a real shape mismatch and warrants a
//! separate `kind` if pressure surfaces.
//!
//! ## Payload shape (Q5(a) at sign-off)
//!
//! ```json
//! {
//!   "version":   1,
//!   "source":    "aivyx-pa",
//!   "target":    "<target_name>",
//!   "subject":   "<subject>" | null,
//!   "message":   "<message>",
//!   "timestamp": "<RFC 3339 UTC>"
//! }
//! ```
//!
//! Content-Type: `application/json`. Timeout: 5 seconds.
//!
//! `version` ([`NOTIFY_WEBHOOK_PAYLOAD_VERSION`]) changes only when a
//! field is removed or changes meaning; new fields can appear without a
//! bump, so a receiver ignores fields it does not know.
//!
//! ## Status code mapping
//!
//! - 2xx              → `Ok(())`
//! - 401 / 403        → `NotifyError::Auth`
//! - 4xx other        → `NotifyError::Rejected(status)`
//! - 5xx              → `NotifyError::Transport`
//! - reqwest timeout  → `NotifyError::Timeout`
//! - reqwest other    → `NotifyError::Transport`

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use serde::Serialize;

use crate::notify_dispatcher::{NotifyBackend, NotifyError};

/// Per-target webhook backend. Phase 62 Task 6.
///
/// The backend holds a target_name, a URL, and an
/// `Arc<dyn WebhookSender>` so tests can inject a scripted
/// sender. The default sender is [`ReqwestWebhookSender`]
/// wrapping a `reqwest::Client` with a 5s total timeout.
pub struct NotifyWebhookBackend {
    target_name: String,
    url: String,
    sender: Arc<dyn WebhookSender>,
}

impl NotifyWebhookBackend {
    /// Construct with the default reqwest-based sender. Used by
    /// the daemon's startup path; tests use [`with_sender`](Self::with_sender)
    /// to inject a scripted sender.
    pub fn new(target_name: impl Into<String>, url: impl Into<String>) -> Self {
        Self {
            target_name: target_name.into(),
            url: url.into(),
            sender: Arc::new(ReqwestWebhookSender::new()),
        }
    }

    /// Construct with an explicit sender. Test-only in practice
    /// but `pub` so a future operator-facing path could inject a
    /// custom sender if needed (e.g. signed requests, retries
    /// at the sender level).
    pub fn with_sender(
        target_name: impl Into<String>,
        url: impl Into<String>,
        sender: Arc<dyn WebhookSender>,
    ) -> Self {
        Self {
            target_name: target_name.into(),
            url: url.into(),
            sender,
        }
    }

    /// Build the JSON payload for a notification. Public for
    /// testing; serialization is via `serde_json::to_string`.
    pub fn render_payload(
        target_name: &str,
        message: &str,
        subject: Option<&str>,
    ) -> WebhookPayload {
        WebhookPayload {
            version: NOTIFY_WEBHOOK_PAYLOAD_VERSION,
            source: "aivyx-pa",
            target: target_name.to_string(),
            subject: subject.map(|s| s.to_string()),
            message: message.to_string(),
            timestamp: Utc::now().to_rfc3339(),
        }
    }
}

#[async_trait]
impl NotifyBackend for NotifyWebhookBackend {
    async fn send(
        &self,
        message: &str,
        subject: Option<&str>,
    ) -> Result<(), NotifyError> {
        let payload = Self::render_payload(&self.target_name, message, subject);
        let body =
            serde_json::to_vec(&payload).map_err(|e| NotifyError::Transport(format!("serialize payload: {e}")))?;
        self.sender.post_json(&self.url, body).await
    }

    fn kind(&self) -> &'static str {
        "webhook"
    }
}

/// The notify webhook payload's version (R14; docs/stable/formats.txt).
pub const NOTIFY_WEBHOOK_PAYLOAD_VERSION: u32 = 1;

/// JSON payload shape per Q5(a) at Phase 62 sign-off, plus `version`.
#[derive(Debug, Clone, Serialize)]
pub struct WebhookPayload {
    pub version: u32,
    pub source: &'static str,
    pub target: String,
    pub subject: Option<String>,
    pub message: String,
    pub timestamp: String,
}

/// Abstraction over the HTTP POST so tests don't need a real
/// HTTP server. Production impl: [`ReqwestWebhookSender`].
#[async_trait]
pub trait WebhookSender: Send + Sync {
    /// POST `body` as `application/json` to `url`. Map the HTTP
    /// outcome to a [`NotifyError`] per the Phase 62 Q5 mapping.
    async fn post_json(&self, url: &str, body: Vec<u8>) -> Result<(), NotifyError>;
}

/// Production HTTP sender with a fixed 5-second total timeout. A webhook on
/// a local address (a self-hosted service) is posted directly, never
/// through a configured proxy; anything else honours the proxy
/// (aivyx-vision audit 2026-11-11, X1).
pub struct ReqwestWebhookSender {
    client: reqwest::Client,
    direct: reqwest::Client,
}

impl ReqwestWebhookSender {
    /// Build a client with `connect_timeout` and total `timeout`
    /// both set to 5s. The TLS layer is rustls (already the
    /// workspace default for reqwest); no system openssl
    /// dependency.
    pub fn new() -> Self {
        let build = |builder: reqwest::ClientBuilder| {
            builder
                .connect_timeout(Duration::from_secs(5))
                .timeout(Duration::from_secs(5))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new())
        };
        Self {
            client: build(reqwest::Client::builder()),
            direct: build(aivyx_llm::transport::direct_client_builder()),
        }
    }
}

impl ReqwestWebhookSender {
    fn client_for(&self, url: &str) -> &reqwest::Client {
        if aivyx_route::url_locality(url) == aivyx_route::Locality::Local {
            &self.direct
        } else {
            &self.client
        }
    }
}

impl Default for ReqwestWebhookSender {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl WebhookSender for ReqwestWebhookSender {
    async fn post_json(&self, url: &str, body: Vec<u8>) -> Result<(), NotifyError> {
        let resp = self
            .client_for(url)
            .post(url)
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await
            .map_err(map_reqwest_error)?;
        map_http_status(resp.status().as_u16())
    }
}

/// Map an HTTP status code to either `Ok(())` (2xx) or a
/// classified [`NotifyError`]. Pure; testable in isolation.
pub fn map_http_status(status: u16) -> Result<(), NotifyError> {
    if (200..300).contains(&status) {
        Ok(())
    } else if status == 401 || status == 403 {
        Err(NotifyError::Auth(format!("HTTP {status}")))
    } else if (400..500).contains(&status) {
        Err(NotifyError::Rejected(status))
    } else if (500..600).contains(&status) {
        Err(NotifyError::Transport(format!("HTTP {status}")))
    } else {
        // 1xx / 3xx — reqwest follows redirects by default, so
        // 3xx shouldn't reach here. 1xx is informational. Treat
        // anything else as a Transport-class anomaly.
        Err(NotifyError::Transport(format!("unexpected HTTP {status}")))
    }
}

fn map_reqwest_error(e: reqwest::Error) -> NotifyError {
    if e.is_timeout() {
        NotifyError::Timeout
    } else if e.is_connect() {
        NotifyError::Transport(format!("connect: {e}"))
    } else {
        NotifyError::Transport(format!("reqwest: {e}"))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Scripted webhook sender for tests — records the URL and
    /// body of every POST and returns a caller-configurable
    /// outcome.
    struct ScriptedSender {
        calls: Mutex<Vec<(String, Vec<u8>)>>,
        outcome: Mutex<Result<(), NotifyError>>,
    }

    impl ScriptedSender {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::new(Vec::new()),
                outcome: Mutex::new(Ok(())),
            })
        }

        fn with_outcome(outcome: Result<(), NotifyError>) -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::new(Vec::new()),
                outcome: Mutex::new(outcome),
            })
        }
    }

    #[async_trait]
    impl WebhookSender for ScriptedSender {
        async fn post_json(&self, url: &str, body: Vec<u8>) -> Result<(), NotifyError> {
            self.calls.lock().unwrap().push((url.to_string(), body));
            self.outcome.lock().unwrap().clone()
        }
    }

    #[test]
    fn render_payload_includes_all_required_fields() {
        let p = NotifyWebhookBackend::render_payload(
            "ops-alerts",
            "build failed",
            Some("CI"),
        );
        assert_eq!(p.version, 1);
        assert_eq!(p.source, "aivyx-pa");
        assert_eq!(p.target, "ops-alerts");
        assert_eq!(p.subject.as_deref(), Some("CI"));
        assert_eq!(p.message, "build failed");
        assert!(!p.timestamp.is_empty());
        // RFC 3339 has a `T` separator between date and time.
        assert!(p.timestamp.contains('T'), "ts: {}", p.timestamp);
    }

    #[test]
    fn render_payload_omits_subject_when_none() {
        let p = NotifyWebhookBackend::render_payload("ops", "msg", None);
        assert_eq!(p.subject, None);
    }

    #[test]
    fn render_payload_serializes_to_expected_json_shape() {
        let p = NotifyWebhookBackend::render_payload(
            "phone",
            "hello",
            Some("title"),
        );
        let v: serde_json::Value =
            serde_json::from_slice(&serde_json::to_vec(&p).unwrap()).unwrap();
        assert_eq!(v["source"], "aivyx-pa");
        assert_eq!(v["target"], "phone");
        assert_eq!(v["subject"], "title");
        assert_eq!(v["message"], "hello");
        // timestamp present + non-empty
        assert!(v["timestamp"].as_str().unwrap().len() > 10);
    }

    #[test]
    fn render_payload_subject_serializes_null_when_none() {
        let p = NotifyWebhookBackend::render_payload("t", "m", None);
        let v: serde_json::Value =
            serde_json::from_slice(&serde_json::to_vec(&p).unwrap()).unwrap();
        assert!(v["subject"].is_null());
    }

    #[tokio::test]
    async fn send_posts_to_target_url_with_json_body() {
        let sender = ScriptedSender::new();
        let backend = NotifyWebhookBackend::with_sender(
            "ops-alerts",
            "https://example.com/notify",
            sender.clone(),
        );
        backend.send("hello world", None).await.expect("send ok");

        let calls = sender.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "https://example.com/notify");
        let body: serde_json::Value = serde_json::from_slice(&calls[0].1).unwrap();
        assert_eq!(body["message"], "hello world");
        assert_eq!(body["target"], "ops-alerts");
    }

    #[tokio::test]
    async fn send_propagates_rejected_status_from_sender() {
        let sender = ScriptedSender::with_outcome(Err(NotifyError::Rejected(429)));
        let backend = NotifyWebhookBackend::with_sender(
            "t",
            "https://example.com/",
            sender,
        );
        let r = backend.send("x", None).await;
        assert!(matches!(r, Err(NotifyError::Rejected(429))));
    }

    #[test]
    fn map_http_status_2xx_is_ok() {
        assert!(map_http_status(200).is_ok());
        assert!(map_http_status(201).is_ok());
        assert!(map_http_status(204).is_ok());
        assert!(map_http_status(299).is_ok());
    }

    #[test]
    fn map_http_status_401_403_are_auth() {
        assert!(matches!(map_http_status(401), Err(NotifyError::Auth(_))));
        assert!(matches!(map_http_status(403), Err(NotifyError::Auth(_))));
    }

    #[test]
    fn map_http_status_other_4xx_are_rejected() {
        assert!(matches!(map_http_status(400), Err(NotifyError::Rejected(400))));
        assert!(matches!(map_http_status(404), Err(NotifyError::Rejected(404))));
        assert!(matches!(map_http_status(422), Err(NotifyError::Rejected(422))));
        assert!(matches!(map_http_status(429), Err(NotifyError::Rejected(429))));
    }

    #[test]
    fn map_http_status_5xx_are_transport() {
        assert!(matches!(
            map_http_status(500),
            Err(NotifyError::Transport(_))
        ));
        assert!(matches!(
            map_http_status(502),
            Err(NotifyError::Transport(_))
        ));
        assert!(matches!(
            map_http_status(599),
            Err(NotifyError::Transport(_))
        ));
    }

    #[test]
    fn map_http_status_1xx_3xx_outliers_are_transport() {
        // reqwest follows redirects; 3xx shouldn't reach here.
        // 1xx is informational. Both classify as Transport.
        assert!(matches!(
            map_http_status(100),
            Err(NotifyError::Transport(_))
        ));
        assert!(matches!(
            map_http_status(302),
            Err(NotifyError::Transport(_))
        ));
    }

    #[test]
    fn webhook_backend_kind_is_webhook() {
        let backend = NotifyWebhookBackend::new("t", "https://example.com/");
        assert_eq!(backend.kind(), "webhook");
    }
}
