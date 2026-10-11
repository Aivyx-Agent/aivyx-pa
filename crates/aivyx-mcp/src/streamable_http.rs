//! Streamable HTTP transport (MCP 2025-03-26+) — the modern transport
//! that supersedes the HTTP+SSE pair.
//!
//! One HTTP endpoint. The client `POST`s a JSON-RPC message; the server
//! replies with either a single JSON object (`application/json`) or an
//! SSE stream (`text/event-stream`) carrying the response (and possibly
//! notifications). The server may assign an `Mcp-Session-Id` header on
//! the `initialize` reply, which the client then echoes on every later
//! request.
//!
//! This maps onto the [`McpTransport`] `send`/`receive` split by doing
//! the round-trip in `send`: the POST response is parsed into queued
//! JSON-RPC message(s), which `receive` drains in order — exactly the
//! request→response cadence the bridge's `call` expects.
//!
//! Limitation (shared with the SSE transport): the bridge reads one
//! reply per call, so a server that interleaves server-initiated
//! notifications inside a response stream is not handled (the standalone
//! GET listener + `tools/list_changed` re-discovery is a later phase).

use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::transport_trait::McpTransport;

/// Protocol version advertised in the `MCP-Protocol-Version` header.
const PROTOCOL_VERSION: &str = "2025-03-26";

pub struct StreamableHttpTransport {
    client: reqwest::Client,
    endpoint: String,
    /// Operator headers (CD.2) re-applied to every request.
    headers: Vec<(String, String)>,
    /// Server-assigned session id (from the `initialize` response),
    /// echoed on every subsequent request once known.
    session_id: Mutex<Option<String>>,
    /// JSON-RPC messages parsed from POST responses, awaiting `receive`.
    queue: Mutex<VecDeque<String>>,
}

impl StreamableHttpTransport {
    /// Build a transport for an MCP endpoint URL. No handshake here —
    /// the bridge drives `initialize` through `send`/`receive`.
    pub async fn connect(endpoint: &str, headers: &[(String, String)]) -> Result<Self, String> {
        if endpoint.trim().is_empty() {
            return Err("streamable-http: empty endpoint URL".into());
        }
        let client = reqwest::Client::builder()
            .build()
            .map_err(|e| format!("streamable-http client: {e}"))?;
        Ok(StreamableHttpTransport {
            client,
            endpoint: endpoint.to_string(),
            headers: headers.to_vec(),
            session_id: Mutex::new(None),
            queue: Mutex::new(VecDeque::new()),
        })
    }

    /// The session id captured from the server, if any.
    pub fn session_id(&self) -> Option<String> {
        self.session_id.lock().unwrap().clone()
    }

    /// The pure half of `send`: capture the session id and queue the
    /// JSON-RPC message(s) parsed from a POST response. Unit-testable
    /// without HTTP.
    fn ingest(&self, content_type: Option<&str>, body: &str, session_header: Option<&str>) {
        if let Some(sid) = session_header.filter(|s| !s.trim().is_empty()) {
            *self.session_id.lock().unwrap() = Some(sid.to_string());
        }
        let mut q = self.queue.lock().unwrap();
        for msg in extract_messages(content_type, body) {
            q.push_back(msg);
        }
    }
}

/// Extract JSON-RPC message(s) from a POST response body: `text/event-
/// stream` yields one message per SSE `data:` frame; anything else
/// (`application/json`) is one message (the whole non-empty body). Pure.
fn extract_messages(content_type: Option<&str>, body: &str) -> Vec<String> {
    let is_sse = content_type
        .map(|c| c.contains("text/event-stream"))
        .unwrap_or(false);
    if is_sse {
        sse_data_messages(body)
    } else {
        let t = body.trim();
        if t.is_empty() {
            Vec::new()
        } else {
            vec![t.to_string()]
        }
    }
}

/// Collect the `data:` payload of each SSE frame in a complete body.
/// Handles `\r\n`, comment/heartbeat lines (`:`), the optional leading
/// space after `data:`, and the last `data:` line winning per frame.
/// Pure.
fn sse_data_messages(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    for frame in body.replace("\r\n", "\n").split("\n\n") {
        let mut data: Option<String> = None;
        for line in frame.split('\n') {
            let line = line.trim_end();
            if line.is_empty() || line.starts_with(':') {
                continue;
            }
            if let Some(v) = line.strip_prefix("data:") {
                data = Some(v.strip_prefix(' ').unwrap_or(v).to_string());
            }
        }
        if let Some(d) = data {
            if !d.trim().is_empty() {
                out.push(d);
            }
        }
    }
    out
}

#[async_trait]
impl McpTransport for StreamableHttpTransport {
    async fn send(&self, message: &str) -> Result<(), String> {
        let mut req = self
            .client
            .post(&self.endpoint)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .header("MCP-Protocol-Version", PROTOCOL_VERSION);
        if let Some(sid) = self.session_id() {
            req = req.header("Mcp-Session-Id", sid);
        }
        // Chapter Conduit (CD.2) — operator headers (e.g. Authorization),
        // never overriding the protocol-reserved names above.
        req = crate::apply_operator_headers(
            req,
            &self.headers,
            &["content-type", "accept", "mcp-protocol-version", "mcp-session-id"],
        )
        .body(message.to_string());

        let resp = req
            .send()
            .await
            .map_err(|e| format!("streamable-http POST: {e}"))?;
        let status = resp.status();
        let session = header(&resp, "Mcp-Session-Id");
        let content_type = header(&resp, "Content-Type");
        let body = crate::limits::collect_capped(resp.bytes_stream(), crate::limits::MAX_MESSAGE_BYTES)
            .await
            .map_err(|e| format!("streamable-http body: {e}"))?;
        if !status.is_success() {
            return Err(format!("streamable-http {status}: {body}"));
        }
        self.ingest(content_type.as_deref(), &body, session.as_deref());
        Ok(())
    }

    async fn receive(&self) -> Result<String, String> {
        self.queue.lock().unwrap().pop_front().ok_or_else(|| {
            "streamable-http: no queued message (server returned no response body)".to_string()
        })
    }
}

fn header(resp: &reqwest::Response, name: &str) -> Option<String> {
    resp.headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn connect_rejects_empty_endpoint() {
        assert!(StreamableHttpTransport::connect("  ", &[]).await.is_err());
    }

    #[test]
    fn extract_application_json_is_one_message() {
        let msgs = extract_messages(Some("application/json"), r#"{"id":1,"result":{}}"#);
        assert_eq!(msgs, vec![r#"{"id":1,"result":{}}"#.to_string()]);
    }

    #[test]
    fn extract_empty_body_is_no_messages() {
        assert!(extract_messages(Some("application/json"), "").is_empty());
        assert!(extract_messages(None, "   ").is_empty());
    }

    #[test]
    fn extract_event_stream_yields_one_per_frame() {
        let body = "event: message\ndata: {\"id\":1}\n\ndata: {\"id\":2}\n\n";
        let msgs = extract_messages(Some("text/event-stream"), body);
        assert_eq!(msgs, vec![r#"{"id":1}"#.to_string(), r#"{"id":2}"#.to_string()]);
    }

    #[test]
    fn sse_handles_crlf_comments_and_leading_space() {
        let body = ": keep-alive\r\nevent: message\r\ndata: {\"ok\":true}\r\n\r\n";
        assert_eq!(sse_data_messages(body), vec![r#"{"ok":true}"#.to_string()]);
    }

    #[tokio::test]
    async fn ingest_captures_session_and_queues_in_order() {
        let t = StreamableHttpTransport::connect("http://x/mcp", &[]).await.unwrap();
        t.ingest(
            Some("application/json"),
            r#"{"id":1,"result":{}}"#,
            Some("sess-abc"),
        );
        assert_eq!(t.session_id().as_deref(), Some("sess-abc"));
        // A second response (SSE, two messages); no session header now.
        t.ingest(
            Some("text/event-stream"),
            "data: {\"id\":2}\n\ndata: {\"id\":3}\n\n",
            None,
        );
        assert_eq!(t.session_id().as_deref(), Some("sess-abc"), "session retained");
        assert_eq!(t.receive().await.unwrap(), r#"{"id":1,"result":{}}"#);
        assert_eq!(t.receive().await.unwrap(), r#"{"id":2}"#);
        assert_eq!(t.receive().await.unwrap(), r#"{"id":3}"#);
        assert!(t.receive().await.is_err(), "drained");
    }

    #[tokio::test]
    async fn empty_session_header_is_ignored() {
        let t = StreamableHttpTransport::connect("http://x/mcp", &[]).await.unwrap();
        t.ingest(Some("application/json"), r#"{"id":1}"#, Some("  "));
        assert!(t.session_id().is_none());
    }
}
