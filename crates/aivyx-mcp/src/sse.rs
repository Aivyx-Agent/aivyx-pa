//! SSE transport — communicates with an MCP server over HTTP
//! using Server-Sent Events for server→client messages and
//! HTTP POST for client→server messages.
//!
//! ## MCP SSE protocol
//!
//! 1. Client GETs the SSE endpoint — long-lived connection.
//! 2. Server sends an `endpoint` SSE event whose `data` field
//!    contains the URL for client→server JSON-RPC messages.
//! 3. Client POSTs JSON-RPC requests to that endpoint URL.
//! 4. Server sends `message` SSE events containing JSON-RPC
//!    responses on the original SSE stream.

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::StreamExt;
use tokio::sync::{mpsc, Mutex};

use crate::transport_trait::McpTransport;

// ---------------------------------------------------------------------------
// Minimal SSE parser (self-contained, no LlmError dependency)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
struct SseEvent {
    event: String,
    data: String,
}

/// Scan `buf` for a frame terminator (`\n\n` or `\r\n\r\n`).
/// Returns `(offset, terminator_len)`.
fn find_double_newline(buf: &[u8]) -> Option<(usize, usize)> {
    if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
        return Some((pos, 4));
    }
    buf.windows(2)
        .position(|w| w == b"\n\n")
        .map(|pos| (pos, 2))
}

/// Try to parse one complete SSE event from the front of `buf`.
/// Returns `Ok(None)` if there isn't enough data yet.
fn try_parse_one(buf: &mut Vec<u8>) -> Result<Option<SseEvent>, String> {
    let Some((end, term_len)) = find_double_newline(buf) else {
        return Ok(None);
    };

    let frame_bytes = Bytes::copy_from_slice(&buf[..end]);
    buf.drain(..end + term_len);

    let frame_text = std::str::from_utf8(&frame_bytes)
        .map_err(|e| format!("SSE frame is not UTF-8: {e}"))?;

    let mut event_name: Option<String> = None;
    let mut data_line: Option<String> = None;

    for raw_line in frame_text.split('\n') {
        let line = raw_line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with(':') {
            continue;
        }
        if let Some(value) = line.strip_prefix("event:") {
            event_name = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("data:") {
            let trimmed = value.strip_prefix(' ').unwrap_or(value);
            data_line = Some(trimmed.to_string());
        }
    }

    match (event_name, data_line) {
        (Some(event), Some(data)) => Ok(Some(SseEvent { event, data })),
        (None, Some(data)) => {
            // MCP servers may omit event: for plain messages.
            Ok(Some(SseEvent {
                event: "message".into(),
                data,
            }))
        }
        (Some(_), None) => Err("SSE frame had event: but no data: line".into()),
        (None, None) => Ok(None),
    }
}

// ---------------------------------------------------------------------------
// SseTransport
// ---------------------------------------------------------------------------

/// Connects to an MCP server over HTTP/SSE. Owns the SSE stream
/// (via a background reader task) and an `reqwest::Client` for
/// POSTing JSON-RPC requests.
pub struct SseTransport {
    /// POST target discovered from the `endpoint` SSE event.
    post_url: String,
    /// HTTP client shared between `send` calls.
    client: reqwest::Client,
    /// Operator headers (CD.2) re-applied to every POST.
    headers: Vec<(String, String)>,
    /// Receiver end of the channel fed by the background SSE reader.
    rx: Mutex<mpsc::Receiver<Result<String, String>>>,
}

impl SseTransport {
    /// Connect to an MCP server's SSE endpoint. Performs the initial
    /// GET, waits for the `endpoint` event, and spawns a background
    /// task to feed subsequent `message` events into a channel.
    pub async fn connect(sse_url: &str, headers: &[(String, String)]) -> Result<Self, String> {
        let client = reqwest::Client::new();

        // Chapter Conduit (CD.2) — operator headers (e.g. Authorization)
        // on the initial GET; `Accept` is protocol-reserved.
        let get = client.get(sse_url).header("Accept", "text/event-stream");
        let response = crate::apply_operator_headers(get, headers, &["accept"])
            .send()
            .await
            .map_err(|e| format!("SSE GET {sse_url}: {e}"))?;

        if !response.status().is_success() {
            return Err(format!(
                "SSE endpoint returned {}",
                response.status()
            ));
        }

        let mut stream = response.bytes_stream();
        let mut buf = Vec::with_capacity(4096);

        // Read until we get the `endpoint` event.
        let post_url = 'outer: loop {
            match stream.next().await {
                Some(Ok(chunk)) => {
                    buf.extend_from_slice(&chunk);
                    if buf.len() > crate::limits::MAX_MESSAGE_BYTES {
                        return Err("SSE event during connect is too large".into());
                    }
                    while let Some(event) = try_parse_one(&mut buf)
                        .map_err(|e| format!("SSE parse during connect: {e}"))?
                    {
                        if event.event == "endpoint" {
                            break 'outer resolve_url(sse_url, &event.data);
                        }
                    }
                }
                Some(Err(e)) => {
                    return Err(format!("SSE stream error during connect: {e}"));
                }
                None => {
                    return Err(
                        "SSE stream ended before endpoint event".into(),
                    );
                }
            }
        };

        // Spawn a background task to read remaining SSE events and
        // feed `message` data into the channel.
        let (tx, rx) = mpsc::channel::<Result<String, String>>(64);
        tokio::spawn(sse_reader_task(stream, buf, tx));

        Ok(SseTransport {
            post_url,
            client,
            headers: headers.to_vec(),
            rx: Mutex::new(rx),
        })
    }
}

/// Resolve a potentially-relative URL against the SSE base URL.
fn resolve_url(base: &str, endpoint: &str) -> String {
    if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
        return endpoint.to_string();
    }
    // Relative path: take the scheme+authority from the base URL.
    if let Some(idx) = base.find("://") {
        let after_scheme = &base[idx + 3..];
        if let Some(slash) = after_scheme.find('/') {
            let origin = &base[..idx + 3 + slash];
            return format!("{origin}{endpoint}");
        }
    }
    // Fallback: just append.
    format!("{base}{endpoint}")
}

/// Background task: reads SSE events from the byte stream and sends
/// `message` event data through the channel.
async fn sse_reader_task(
    mut stream: impl futures_util::Stream<Item = Result<Bytes, reqwest::Error>>
        + Unpin
        + Send,
    mut buf: Vec<u8>,
    tx: mpsc::Sender<Result<String, String>>,
) {
    loop {
        // First drain any buffered complete events.
        loop {
            match try_parse_one(&mut buf) {
                Ok(Some(event)) => {
                    if event.event == "message"
                        && tx.send(Ok(event.data)).await.is_err()
                    {
                        return; // receiver dropped
                    }
                    // Skip non-message events (e.g. heartbeat comments).
                }
                Ok(None) => break,
                Err(e) => {
                    let _ = tx.send(Err(e)).await;
                    return;
                }
            }
        }

        // Pull more bytes from the HTTP stream.
        match stream.next().await {
            // `buf` holds only the unfinished event: complete ones were
            // parsed off above, so this bounds a single event.
            Some(Ok(chunk)) => {
                buf.extend_from_slice(&chunk);
                if buf.len() > crate::limits::MAX_MESSAGE_BYTES {
                    let _ = tx
                        .send(Err(format!(
                            "SSE event over {} bytes",
                            crate::limits::MAX_MESSAGE_BYTES
                        )))
                        .await;
                    return;
                }
            }
            Some(Err(e)) => {
                let _ = tx.send(Err(format!("SSE stream error: {e}"))).await;
                return;
            }
            None => {
                // Stream ended cleanly. If there's leftover data in buf
                // that doesn't form a complete event, it's trailing junk.
                if !buf.is_empty()
                    && !buf.iter().all(|b| matches!(b, b'\r' | b'\n' | b' '))
                {
                    let _ = tx
                        .send(Err(format!(
                            "SSE stream ended with {} trailing bytes",
                            buf.len()
                        )))
                        .await;
                }
                return;
            }
        }
    }
}

#[async_trait]
impl McpTransport for SseTransport {
    async fn send(&self, message: &str) -> Result<(), String> {
        let post = self
            .client
            .post(&self.post_url)
            .header("Content-Type", "application/json");
        let resp = crate::apply_operator_headers(post, &self.headers, &["content-type"])
            .body(message.to_string())
            .send()
            .await
            .map_err(|e| format!("POST to MCP server: {e}"))?;

        if !resp.status().is_success() {
            return Err(format!(
                "MCP server POST returned {}",
                resp.status()
            ));
        }
        Ok(())
    }

    async fn receive(&self) -> Result<String, String> {
        let mut rx = self.rx.lock().await;
        rx.recv()
            .await
            .unwrap_or_else(|| Err("SSE reader task ended".into()))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_single_event() {
        let mut buf =
            b"event: message\ndata: {\"id\":1}\n\n".to_vec();
        let ev = try_parse_one(&mut buf).unwrap().unwrap();
        assert_eq!(ev.event, "message");
        assert_eq!(ev.data, "{\"id\":1}");
        assert!(buf.is_empty());
    }

    #[test]
    fn parse_endpoint_event() {
        let mut buf =
            b"event: endpoint\ndata: /rpc\n\n".to_vec();
        let ev = try_parse_one(&mut buf).unwrap().unwrap();
        assert_eq!(ev.event, "endpoint");
        assert_eq!(ev.data, "/rpc");
    }

    #[test]
    fn parse_crlf() {
        let mut buf =
            b"event: message\r\ndata: ok\r\n\r\n".to_vec();
        let ev = try_parse_one(&mut buf).unwrap().unwrap();
        assert_eq!(ev.event, "message");
        assert_eq!(ev.data, "ok");
    }

    #[test]
    fn parse_ignores_comments() {
        let mut buf =
            b": heartbeat\nevent: message\ndata: hi\n\n".to_vec();
        let ev = try_parse_one(&mut buf).unwrap().unwrap();
        assert_eq!(ev.event, "message");
        assert_eq!(ev.data, "hi");
    }

    #[test]
    fn parse_incomplete_returns_none() {
        let mut buf = b"event: message\ndata: par".to_vec();
        assert!(try_parse_one(&mut buf).unwrap().is_none());
        // Buffer is unchanged.
        assert_eq!(buf.len(), 24);
    }

    #[test]
    fn parse_data_without_event_defaults_to_message() {
        let mut buf = b"data: {\"x\":1}\n\n".to_vec();
        let ev = try_parse_one(&mut buf).unwrap().unwrap();
        assert_eq!(ev.event, "message");
        assert_eq!(ev.data, "{\"x\":1}");
    }

    #[test]
    fn resolve_absolute_url_unchanged() {
        assert_eq!(
            resolve_url("http://host:8080/sse", "http://other/rpc"),
            "http://other/rpc"
        );
    }

    #[test]
    fn resolve_relative_url() {
        assert_eq!(
            resolve_url("http://host:8080/sse", "/rpc"),
            "http://host:8080/rpc"
        );
    }

    #[test]
    fn resolve_relative_no_path() {
        assert_eq!(
            resolve_url("http://host:8080", "/rpc"),
            "http://host:8080/rpc"
        );
    }
}
