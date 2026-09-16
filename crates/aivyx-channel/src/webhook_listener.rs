//! Webhook HTTP listener — Phase 27 Task 3.
//!
//! A minimal `hyper` HTTP/1.1 server bound to `127.0.0.1` that accepts
//! `POST /trigger/<webhook_id>` requests and fires the associated
//! webhook's prompt through the shared `TriggerDispatch`. Non-matching
//! paths and methods return 404/405. The listener runs as a background
//! task inside the daemon, alongside the cron scheduler.
//!
//! The server is localhost-only per PRODUCT.md P6 (local execution,
//! privacy): "A webhook-triggered run is delivered by a daemon thread
//! already running under the operator's OS user."

use std::sync::Arc;

use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use http_body_util::Full;
use tokio::net::TcpListener;

use aivyx_core::CancellationToken;
use aivyx_storage::DomainHandle;

use crate::trigger::{TriggerDispatch, TriggerSource};
use crate::webhook;

/// Default webhook listener port.
pub const DEFAULT_WEBHOOK_PORT: u16 = 7842;

/// Run the webhook HTTP listener. This future never returns normally —
/// it runs until `shutdown` is cancelled.
pub async fn run_webhook_listener(
    dispatch: TriggerDispatch,
    store: DomainHandle,
    port: u16,
    shutdown: CancellationToken,
) -> Result<(), String> {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|e| format!("webhook listener: failed to bind {addr}: {e}"))?;

    eprintln!("aivyx-pa webhook: listening on http://{addr}");

    let store = Arc::new(store);

    loop {
        let (stream, _remote) = tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok(conn) => conn,
                    Err(e) => {
                        eprintln!("aivyx-pa webhook: accept error: {e}");
                        continue;
                    }
                }
            }
            _ = shutdown.cancelled() => return Ok(()),
        };

        let dispatch = dispatch.clone();
        let store = Arc::clone(&store);
        let conn_shutdown = shutdown.clone();

        tokio::spawn(async move {
            let svc = service_fn(move |req| {
                let dispatch = dispatch.clone();
                let store = Arc::clone(&store);
                async move { handle_request(req, &dispatch, &store).await }
            });

            let io = TokioIo::new(stream);
            let conn = http1::Builder::new().serve_connection(io, svc);
            tokio::select! {
                result = conn => {
                    if let Err(e) = result {
                        eprintln!("aivyx-pa webhook: connection error: {e}");
                    }
                }
                _ = conn_shutdown.cancelled() => {}
            }
        });
    }
}

/// Handle a single HTTP request. Only `POST /trigger/<id>` is valid.
///
/// Generic over the body type `B` (rather than pinned to `Incoming`) purely
/// so `#[cfg(test)]` can drive this handler with a lightweight `Request<()>`
/// — the body is never read on any code path here, so no `Body` trait bound
/// is needed. Production always instantiates `B = Incoming` via the
/// `service_fn` closure in `run_webhook_listener`.
async fn handle_request<B>(
    req: Request<B>,
    dispatch: &TriggerDispatch,
    store: &DomainHandle,
) -> Result<Response<Full<Bytes>>, std::convert::Infallible> {
    let path = req.uri().path().to_owned();
    let method = req.method().clone();

    // Route: POST /trigger/<webhook_id>
    if let Some(webhook_id) = path.strip_prefix("/trigger/") {
        if webhook_id.is_empty() {
            return Ok(json_response(
                StatusCode::BAD_REQUEST,
                r#"{"error":"missing webhook id"}"#,
            ));
        }

        if method != hyper::Method::POST {
            return Ok(json_response(
                StatusCode::METHOD_NOT_ALLOWED,
                r#"{"error":"use POST"}"#,
            ));
        }

        // Task 1 (2026-09-16 audit) — look up the record *before* firing
        // so the bearer secret can be checked ahead of ever calling
        // fire_webhook_record / dispatch.fire(). An unauthenticated
        // request must never reach the agent turn loop.
        let record = match webhook::get_webhook(store, webhook_id).await {
            Ok(Some(r)) => r,
            Ok(None) => {
                return Ok(json_response(
                    StatusCode::NOT_FOUND,
                    r#"{"error":"webhook not found"}"#,
                ));
            }
            Err(e) => {
                eprintln!("aivyx-pa webhook: storage error looking up {webhook_id:?}: {e}");
                return Ok(json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    r#"{"error":"internal error"}"#,
                ));
            }
        };

        if !authorized(&req, &record.secret) {
            return Ok(json_response(
                StatusCode::UNAUTHORIZED,
                r#"{"error":"unauthorized"}"#,
            ));
        }

        return Ok(fire_webhook_record(dispatch, store, record).await);
    }

    // Health check endpoint
    if path == "/health" && method == hyper::Method::GET {
        return Ok(json_response(StatusCode::OK, r#"{"status":"ok"}"#));
    }

    Ok(json_response(
        StatusCode::NOT_FOUND,
        r#"{"error":"not found"}"#,
    ))
}

/// True when `req` carries a valid `Authorization: Bearer <secret>` header
/// matching `expected_secret`. Constant-time compared via
/// [`crate::web_ui::ct_eq`] — the same primitive `web_ui.rs` uses for the
/// web-UI auth token, reused rather than duplicated.
///
/// An empty `expected_secret` (a record persisted before Task 1, or any
/// other reason a record's secret is unset) never authorizes, even against
/// a client sending an empty bearer token.
fn authorized<B>(req: &Request<B>, expected_secret: &str) -> bool {
    if expected_secret.is_empty() {
        return false;
    }
    let Some(header) = req.headers().get(hyper::header::AUTHORIZATION) else {
        return false;
    };
    let Ok(header_str) = header.to_str() else {
        return false;
    };
    let Some(token) = header_str.strip_prefix("Bearer ") else {
        return false;
    };
    crate::web_ui::ct_eq(token.as_bytes(), expected_secret.as_bytes())
}

/// Fire an already-fetched, already-authorized webhook record.
async fn fire_webhook_record(
    dispatch: &TriggerDispatch,
    store: &DomainHandle,
    record: webhook::WebhookRecord,
) -> Response<Full<Bytes>> {
    if !record.enabled {
        return json_response(
            StatusCode::CONFLICT,
            r#"{"error":"webhook is disabled"}"#,
        );
    }

    // Fire asynchronously — the HTTP response returns immediately
    // with "accepted", and the turn runs in the background.
    let dispatch = dispatch.clone();
    let id = record.webhook_id.clone();
    let prompt = record.prompt.clone();
    let notify_targets = record.notify_targets.clone();
    let notify_when = record.notify_when;
    let store_clone = store.clone();
    tokio::spawn(async move {
        dispatch
            .fire(
                TriggerSource::Webhook,
                &id,
                &prompt,
                record.wrap_mission,
                &notify_targets,
                notify_when,
            )
            .await;

        // Update last_fired_at.
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;
        let mut updated = record;
        updated.last_fired_at = Some(now_ms);
        if let Err(e) = webhook::update_webhook(&store_clone, &updated).await {
            eprintln!("aivyx-pa webhook: failed to update last_fired_at for {}: {e}", updated.webhook_id);
        }
    });

    json_response(StatusCode::ACCEPTED, r#"{"status":"accepted"}"#)
}

fn json_response(status: StatusCode, body: &str) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(body.to_owned())))
        .unwrap()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use aivyx_capability::{CapabilitySet, TrustTier};
    use aivyx_core::{
        AgentId, CancellationToken as CoreCancellationToken, ChannelContext, ChannelError,
        ChannelPlatform, Message, SessionId, StreamEvent, TurnOutcome,
    };
    use aivyx_crypto::MasterKey;
    use aivyx_storage::{KeyDomain, RedbStorage, StorageConfig};

    use crate::daemon_ipc::FrontendType;
    use crate::daemon_server::ChannelFactory;

    #[test]
    fn json_response_sets_content_type() {
        let resp = json_response(StatusCode::OK, r#"{"ok":true}"#);
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "application/json"
        );
    }

    #[test]
    fn json_response_not_found() {
        let resp = json_response(StatusCode::NOT_FOUND, r#"{"error":"nope"}"#);
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            resp.headers().get("content-type").unwrap(),
            "application/json"
        );
    }

    // -----------------------------------------------------------------
    // `authorized()` — pure header-parsing logic, no store/dispatch needed.
    // -----------------------------------------------------------------

    fn req_with_auth_header(value: Option<&str>) -> Request<()> {
        let mut builder = Request::builder().method("POST").uri("/trigger/wh1");
        if let Some(v) = value {
            builder = builder.header(hyper::header::AUTHORIZATION, v);
        }
        builder.body(()).unwrap()
    }

    #[test]
    fn authorized_rejects_missing_header() {
        let req = req_with_auth_header(None);
        assert!(!authorized(&req, "correct-secret"));
    }

    #[test]
    fn authorized_rejects_wrong_secret() {
        let req = req_with_auth_header(Some("Bearer wrong-secret"));
        assert!(!authorized(&req, "correct-secret"));
    }

    #[test]
    fn authorized_rejects_malformed_header() {
        // No "Bearer " scheme prefix at all.
        let req = req_with_auth_header(Some("correct-secret"));
        assert!(!authorized(&req, "correct-secret"));
        // Wrong scheme.
        let req = req_with_auth_header(Some("Basic correct-secret"));
        assert!(!authorized(&req, "correct-secret"));
    }

    #[test]
    fn authorized_accepts_correct_bearer_token() {
        let req = req_with_auth_header(Some("Bearer correct-secret"));
        assert!(authorized(&req, "correct-secret"));
    }

    #[test]
    fn authorized_never_matches_an_empty_stored_secret() {
        // A legacy record with no secret (or an empty one for any other
        // reason) must never authorize — not even against a client that
        // sends an empty bearer token.
        let req = req_with_auth_header(Some("Bearer "));
        assert!(!authorized(&req, ""));
        let req_no_header = req_with_auth_header(None);
        assert!(!authorized(&req_no_header, ""));
    }

    // -----------------------------------------------------------------
    // `handle_request()` end-to-end — auth gate before dispatch.fire().
    // -----------------------------------------------------------------

    /// RAII tempdir for a throwaway encrypted store, mirroring the pattern
    /// `aivyx-storage`'s own tests use (`StoreDir`) and the `TestDir` helpers
    /// in `persona.rs`/`passphrase.rs` — rolled locally rather than pulling
    /// in `tempfile` or exporting a test-only helper across crates.
    struct TestStoreDir {
        dir: PathBuf,
    }

    impl TestStoreDir {
        fn new() -> Self {
            let tmp = std::env::var("TMPDIR")
                .or_else(|_| std::env::var("TEMP"))
                .unwrap_or_else(|_| "/tmp".to_string());
            let dir = PathBuf::from(tmp)
                .join(format!("aivyx-webhook-listener-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&dir).expect("test store dir must be creatable");
            TestStoreDir { dir }
        }

        fn db_path(&self) -> PathBuf {
            self.dir.join("store.redb")
        }
    }

    impl Drop for TestStoreDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    async fn test_store() -> (TestStoreDir, DomainHandle) {
        let dir = TestStoreDir::new();
        let master = MasterKey::from_raw([9u8; 32]);
        let storage = RedbStorage::open(StorageConfig::new(dir.db_path()), master)
            .await
            .expect("open throwaway test store");
        let handle = storage.domain(KeyDomain::Webhooks);
        (dir, handle)
    }

    /// Minimal `Agent` impl — `fire_webhook_record` spawns a background
    /// task that calls `dispatch.fire()`, which calls `agent.turn()`. The
    /// auth tests don't assert on what the turn produced, only on the HTTP
    /// response `handle_request` returns before that task is even spawned.
    struct StubAgent {
        id: AgentId,
        caps: CapabilitySet,
    }

    #[async_trait::async_trait]
    impl aivyx_core::Agent for StubAgent {
        fn id(&self) -> AgentId {
            self.id
        }

        fn capabilities(&self) -> &CapabilitySet {
            &self.caps
        }

        async fn turn(&self, _message: Message, _channel: &dyn ChannelContext) -> TurnOutcome {
            TurnOutcome::Completed {
                final_message: "stub turn".to_string(),
                tool_calls_made: 0,
                duration: std::time::Duration::from_millis(0),
            }
        }
    }

    struct StubChannel;

    #[async_trait::async_trait]
    impl ChannelContext for StubChannel {
        fn channel_name(&self) -> &str {
            "stub"
        }

        fn platform(&self) -> ChannelPlatform {
            ChannelPlatform::Local
        }

        fn trust_tier(&self) -> TrustTier {
            TrustTier::Trusted
        }

        fn session_id(&self) -> SessionId {
            SessionId::new()
        }

        async fn stream_event(&self, _event: StreamEvent<'_>) -> Result<(), ChannelError> {
            Ok(())
        }

        async fn finalize(&self, _outcome: &TurnOutcome) -> Result<(), ChannelError> {
            Ok(())
        }

        fn cancellation_token(&self) -> CoreCancellationToken {
            CoreCancellationToken::new()
        }
    }

    fn test_dispatch() -> TriggerDispatch {
        let channel_factory: ChannelFactory =
            std::sync::Arc::new(|_ft: FrontendType| {
                std::sync::Arc::new(StubChannel) as std::sync::Arc<dyn ChannelContext + Send + Sync>
            });
        TriggerDispatch::new(
            std::sync::Arc::new(StubAgent {
                id: AgentId::new(),
                caps: CapabilitySet::empty(),
            }),
            channel_factory,
        )
    }

    #[tokio::test]
    async fn fire_webhook_without_bearer_token_is_rejected() {
        let (_dir, store) = test_store().await;
        webhook::create_webhook_for_test(&store, "wh1", "do the thing", "test-secret-abc123".to_string())
            .await;
        let dispatch = test_dispatch();

        let req = Request::builder()
            .method("POST")
            .uri("/trigger/wh1")
            .body(())
            .unwrap();

        let resp = handle_request(req, &dispatch, &store).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn fire_webhook_with_wrong_bearer_token_is_rejected() {
        let (_dir, store) = test_store().await;
        webhook::create_webhook_for_test(&store, "wh1", "do the thing", "test-secret-abc123".to_string())
            .await;
        let dispatch = test_dispatch();

        let req = Request::builder()
            .method("POST")
            .uri("/trigger/wh1")
            .header(hyper::header::AUTHORIZATION, "Bearer nope")
            .body(())
            .unwrap();

        let resp = handle_request(req, &dispatch, &store).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn fire_webhook_with_correct_bearer_token_succeeds() {
        let (_dir, store) = test_store().await;
        let secret = "test-secret-abc123".to_string();
        webhook::create_webhook_for_test(&store, "wh1", "do the thing", secret.clone()).await;
        let dispatch = test_dispatch();

        let req = Request::builder()
            .method("POST")
            .uri("/trigger/wh1")
            .header(hyper::header::AUTHORIZATION, format!("Bearer {secret}"))
            .body(())
            .unwrap();

        let resp = handle_request(req, &dispatch, &store).await.unwrap();
        assert_eq!(resp.status(), StatusCode::ACCEPTED);
    }

    #[tokio::test]
    async fn fire_webhook_unknown_id_returns_not_found_not_unauthorized() {
        // Pre-existing 404-for-unknown-id behavior must survive the Task 1
        // refactor unchanged: the lookup now happens before the auth check
        // (needed to find the record's secret at all), so a request against
        // an ID that was never created still 404s rather than 401ing.
        let (_dir, store) = test_store().await;
        let dispatch = test_dispatch();

        let req = Request::builder()
            .method("POST")
            .uri("/trigger/does-not-exist")
            .body(())
            .unwrap();

        let resp = handle_request(req, &dispatch, &store).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
