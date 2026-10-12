//! JSON-RPC 2.0 to signal-cli's daemon over its Unix socket: one JSON
//! object per line each way, responses matched to requests by `id`, and
//! `receive` notifications handed out in arrival order.
//!
//! One background task reads the socket. When the connection ends, every
//! waiting call and the notification stream fail with [`RpcError::Closed`];
//! the transport (`transport::CliTransport`) then reconnects.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use serde_json::{json, Value};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot, Mutex};

/// The longest line accepted from signal-cli (a `receive` notification with
/// a long message is a few KiB; anything near this is not signal-cli).
const MAX_LINE: u64 = 4 * 1024 * 1024;
/// How long one request may wait for its response.
const CALL_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RpcError {
    #[error("can't reach signal-cli: {0}")]
    Io(String),
    /// signal-cli answered with a JSON-RPC error.
    #[error("signal-cli: {message}")]
    Remote { code: i64, message: String },
    #[error("signal-cli closed the connection")]
    Closed,
}

type Pending = Arc<StdMutex<HashMap<u64, oneshot::Sender<Result<Value, RpcError>>>>>;

pub struct RpcClient {
    writer: Mutex<Box<dyn AsyncWrite + Send + Unpin>>,
    pending: Pending,
    next_id: AtomicU64,
    notifications: Mutex<mpsc::UnboundedReceiver<Value>>,
    reader: tokio::task::JoinHandle<()>,
}

impl Drop for RpcClient {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

impl RpcClient {
    /// Connects to signal-cli's socket at `path`.
    pub async fn connect(path: &Path) -> Result<Self, RpcError> {
        let stream = tokio::net::UnixStream::connect(path)
            .await
            .map_err(|e| RpcError::Io(format!("{}: {e}", path.display())))?;
        Ok(Self::from_stream(stream))
    }

    /// Wraps any byte stream (tests use an in-memory pipe).
    pub fn from_stream<S: AsyncRead + AsyncWrite + Send + 'static>(stream: S) -> Self {
        let (read, write) = tokio::io::split(stream);
        let pending: Pending = Arc::default();
        let (tx, rx) = mpsc::unbounded_channel();
        let reader = tokio::spawn(read_loop(read, Arc::clone(&pending), tx));
        RpcClient {
            writer: Mutex::new(Box::new(write)),
            pending,
            next_id: AtomicU64::new(1),
            notifications: Mutex::new(rx),
            reader,
        }
    }

    /// Calls `method` and waits for its result.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .expect("pending mutex poisoned")
            .insert(id, tx);
        let mut line = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))
        .map_err(|e| RpcError::Io(e.to_string()))?;
        line.push(b'\n');
        let written = {
            let mut w = self.writer.lock().await;
            match w.write_all(&line).await {
                Ok(()) => w.flush().await,
                Err(e) => Err(e),
            }
        };
        if let Err(e) = written {
            self.pending.lock().expect("pending mutex poisoned").remove(&id);
            return Err(RpcError::Io(e.to_string()));
        }
        match tokio::time::timeout(CALL_TIMEOUT, rx).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(RpcError::Closed),
            Err(_) => {
                self.pending.lock().expect("pending mutex poisoned").remove(&id);
                Err(RpcError::Io(format!("signal-cli didn't answer `{method}` in time")))
            }
        }
    }

    /// The next `receive` notification's `params`. `Closed` once the
    /// connection has ended and every earlier notification was taken.
    pub async fn next_notification(&self) -> Result<Value, RpcError> {
        self.notifications
            .lock()
            .await
            .recv()
            .await
            .ok_or(RpcError::Closed)
    }
}

async fn read_loop<R: AsyncRead + Unpin>(
    read: R,
    pending: Pending,
    notifications: mpsc::UnboundedSender<Value>,
) {
    let mut reader = BufReader::new(read);
    loop {
        let mut line = Vec::new();
        let n = match (&mut reader).take(MAX_LINE).read_until(b'\n', &mut line).await {
            Ok(n) => n,
            Err(_) => break,
        };
        if n == 0 || !line.ends_with(b"\n") {
            // EOF, or a line longer than MAX_LINE: the stream is unusable.
            break;
        }
        let Ok(msg) = serde_json::from_slice::<Value>(&line) else {
            continue;
        };
        dispatch(msg, &pending, &notifications);
    }
    for (_, tx) in pending.lock().expect("pending mutex poisoned").drain() {
        let _ = tx.send(Err(RpcError::Closed));
    }
}

fn dispatch(msg: Value, pending: &Pending, notifications: &mpsc::UnboundedSender<Value>) {
    if let Some(id) = msg.get("id").and_then(Value::as_u64) {
        let Some(tx) = pending.lock().expect("pending mutex poisoned").remove(&id) else {
            return;
        };
        let result = match msg.get("error") {
            Some(err) => Err(RpcError::Remote {
                code: err.get("code").and_then(Value::as_i64).unwrap_or(0),
                message: err
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error")
                    .to_string(),
            }),
            None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
        };
        let _ = tx.send(result);
    } else if msg.get("method").and_then(Value::as_str) == Some("receive") {
        let _ = notifications.send(msg.get("params").cloned().unwrap_or(Value::Null));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::DuplexStream;

    /// A scripted signal-cli: answers each request line with `respond`,
    /// and first writes `before` (e.g. notifications).
    fn server(
        before: Vec<Value>,
        respond: fn(&Value) -> Option<Value>,
    ) -> (DuplexStream, tokio::task::JoinHandle<Vec<Value>>) {
        let (client, srv) = tokio::io::duplex(64 * 1024);
        let h = tokio::spawn(async move {
            let (r, mut w) = tokio::io::split(srv);
            for m in before {
                w.write_all(format!("{m}\n").as_bytes()).await.unwrap();
            }
            let mut lines = BufReader::new(r).lines();
            let mut seen = Vec::new();
            while let Ok(Some(line)) = lines.next_line().await {
                let req: Value = serde_json::from_str(&line).unwrap();
                if let Some(resp) = respond(&req) {
                    w.write_all(format!("{resp}\n").as_bytes()).await.unwrap();
                }
                seen.push(req);
            }
            seen
        });
        (client, h)
    }

    #[tokio::test]
    async fn calls_are_matched_by_id_and_errors_surface() {
        let (stream, _srv) = server(vec![], |req| {
            let id = req["id"].clone();
            Some(match req["method"].as_str() {
                Some("version") => json!({"jsonrpc":"2.0","id":id,"result":{"version":"0.14.6"}}),
                _ => json!({"jsonrpc":"2.0","id":id,"error":{"code":-1,"message":"Unknown method"}}),
            })
        });
        let c = RpcClient::from_stream(stream);
        assert_eq!(c.call("version", json!({})).await.unwrap()["version"], "0.14.6");
        assert_eq!(
            c.call("nope", json!({})).await.unwrap_err(),
            RpcError::Remote { code: -1, message: "Unknown method".into() }
        );
    }

    #[tokio::test]
    async fn requests_are_well_formed_json_rpc() {
        let (stream, srv) = server(vec![], |req| {
            Some(json!({"jsonrpc":"2.0","id":req["id"],"result":null}))
        });
        let c = RpcClient::from_stream(stream);
        c.call("send", json!({"recipient":["+15550001"],"message":"hi"})).await.unwrap();
        drop(c);
        let seen = srv.await.unwrap();
        assert_eq!(seen[0]["jsonrpc"], "2.0");
        assert_eq!(seen[0]["method"], "send");
        assert_eq!(seen[0]["params"]["recipient"][0], "+15550001");
    }

    #[tokio::test]
    async fn receive_notifications_arrive_in_order_and_others_are_ignored() {
        let (stream, _srv) = server(
            vec![
                json!({"jsonrpc":"2.0","method":"receive","params":{"n":1}}),
                json!({"jsonrpc":"2.0","method":"callEvent","params":{"n":99}}),
                json!({"jsonrpc":"2.0","method":"receive","params":{"n":2}}),
            ],
            |_| None,
        );
        let c = RpcClient::from_stream(stream);
        assert_eq!(c.next_notification().await.unwrap()["n"], 1);
        assert_eq!(c.next_notification().await.unwrap()["n"], 2);
    }

    #[tokio::test]
    async fn a_closed_connection_fails_waiting_calls_and_the_stream() {
        let (client, srv) = tokio::io::duplex(1024);
        let c = RpcClient::from_stream(client);
        drop(srv);
        // Writing to a closed pipe fails (Io) or the reader saw EOF first
        // (Closed); the transport treats both as "unreachable".
        assert!(matches!(
            c.call("version", json!({})).await,
            Err(RpcError::Closed | RpcError::Io(_))
        ));
        assert_eq!(c.next_notification().await.unwrap_err(), RpcError::Closed);
    }
}
