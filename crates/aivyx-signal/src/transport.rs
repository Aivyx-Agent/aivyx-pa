//! The seam between the channel/tool and signal-cli. Production is
//! [`CliTransport`] (JSON-RPC over signal-cli's socket); tests use
//! [`ScriptedTransport`]. No JSON crosses this boundary.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{json, Value};
use thiserror::Error;
use tokio::sync::Mutex;

use crate::envelope::{self, Inbound};
use crate::rpc::{RpcClient, RpcError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustLevel {
    Untrusted,
    TrustedUnverified,
    TrustedVerified,
}

impl TrustLevel {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "UNTRUSTED" => Some(TrustLevel::Untrusted),
            "TRUSTED_UNVERIFIED" => Some(TrustLevel::TrustedUnverified),
            "TRUSTED_VERIFIED" => Some(TrustLevel::TrustedVerified),
            _ => None,
        }
    }
}

/// A contact's identity key as signal-cli knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub trust: TrustLevel,
    /// The safety number as the Signal app shows it (digits, may contain spaces).
    pub safety_number: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TransportError {
    /// signal-cli isn't reachable (not running, socket missing, connection
    /// dropped). Retried.
    #[error("signal-cli isn't reachable ({0}) — is the signal-cli daemon running?")]
    Unreachable(String),
    /// Signal's server no longer accepts the assistant's number. Retrying
    /// can't help.
    #[error(
        "Signal no longer accepts the assistant's number ({0}) — re-register it with \
         signal-cli, then run `aivyx-pa connect signal`"
    )]
    Unregistered(String),
    /// signal-cli refused this one request (e.g. an untrusted recipient key).
    #[error("signal-cli refused: {0}")]
    Refused(String),
}

impl TransportError {
    /// Ends the channel: no retry or backoff can recover from it.
    pub fn is_fatal(&self) -> bool {
        matches!(self, TransportError::Unregistered(_))
    }

    /// signal-cli refused to send because the recipient's identity key
    /// isn't trusted.
    pub fn is_untrusted_recipient(&self) -> bool {
        matches!(self, TransportError::Refused(m) if m.to_ascii_lowercase().contains("untrusted"))
    }
}

impl From<RpcError> for TransportError {
    fn from(e: RpcError) -> Self {
        match e {
            RpcError::Io(m) => TransportError::Unreachable(m),
            RpcError::Closed => TransportError::Unreachable("connection closed".into()),
            RpcError::Remote { message, .. } => {
                // Signal's server answers 401/403 for a number that was
                // unregistered or taken over elsewhere.
                let lower = message.to_ascii_lowercase();
                if lower.contains("authorization failed") || lower.contains("[401]") || lower.contains("[403]") {
                    TransportError::Unregistered(message)
                } else {
                    TransportError::Refused(message)
                }
            }
        }
    }
}

#[async_trait]
pub trait SignalTransport: Send + Sync {
    /// The next incoming event (subscribing to messages if needed).
    async fn next_event(&self) -> Result<Inbound, TransportError>;
    /// Sends `text` to `to` (E.164 or ACI); returns the message timestamp.
    async fn send_text(&self, to: &str, text: &str) -> Result<u64, TransportError>;
    /// Starts (`stop == false`) or stops the typing indicator for `to`.
    async fn typing(&self, to: &str, stop: bool) -> Result<(), TransportError>;
    /// A read receipt for `to`'s message sent at `timestamp`.
    async fn read_receipt(&self, to: &str, timestamp: u64) -> Result<(), TransportError>;
    /// `who`'s identity key, if signal-cli has seen one.
    async fn identity(&self, who: &str) -> Result<Option<Identity>, TransportError>;
    /// Marks `who`'s key verified, given the safety number the operator compared.
    async fn trust_verified(&self, who: &str, safety_number: &str) -> Result<(), TransportError>;
    /// signal-cli's version — proves the daemon answers.
    async fn version(&self) -> Result<String, TransportError>;
}

/// JSON-RPC to the signal-cli daemon at `socket`. Connects lazily and
/// reconnects after any connection error; subscribes to incoming messages
/// the first time [`SignalTransport::next_event`] is called on a
/// connection (only the channel does; the tool never subscribes).
pub struct CliTransport {
    socket: PathBuf,
    conn: Mutex<Option<Conn>>,
}

struct Conn {
    client: Arc<RpcClient>,
    subscribed: bool,
}

impl CliTransport {
    pub fn new(socket: PathBuf) -> Self {
        CliTransport {
            socket,
            conn: Mutex::new(None),
        }
    }

    async fn client(&self) -> Result<Arc<RpcClient>, TransportError> {
        let mut guard = self.conn.lock().await;
        if let Some(c) = guard.as_ref() {
            return Ok(Arc::clone(&c.client));
        }
        let client = Arc::new(RpcClient::connect(&self.socket).await?);
        *guard = Some(Conn {
            client: Arc::clone(&client),
            subscribed: false,
        });
        Ok(client)
    }

    /// Calls `method`; a connection error drops the connection so the next
    /// call reconnects.
    async fn call(&self, method: &str, params: Value) -> Result<Value, TransportError> {
        let client = self.client().await?;
        match client.call(method, params).await {
            Ok(v) => Ok(v),
            Err(e) => {
                if !matches!(e, RpcError::Remote { .. }) {
                    *self.conn.lock().await = None;
                }
                Err(e.into())
            }
        }
    }
}

#[async_trait]
impl SignalTransport for CliTransport {
    async fn next_event(&self) -> Result<Inbound, TransportError> {
        let client = self.client().await?;
        let needs_subscribe = {
            let guard = self.conn.lock().await;
            guard.as_ref().is_some_and(|c| !c.subscribed)
        };
        if needs_subscribe {
            self.call("subscribeReceive", json!({})).await?;
            if let Some(c) = self.conn.lock().await.as_mut() {
                c.subscribed = true;
            }
        }
        match client.next_notification().await {
            Ok(params) => Ok(envelope::parse(&params)),
            Err(e) => {
                *self.conn.lock().await = None;
                Err(e.into())
            }
        }
    }

    async fn send_text(&self, to: &str, text: &str) -> Result<u64, TransportError> {
        let r = self
            .call("send", json!({"recipient": [to], "message": text}))
            .await?;
        Ok(r.get("timestamp").and_then(Value::as_u64).unwrap_or(0))
    }

    async fn typing(&self, to: &str, stop: bool) -> Result<(), TransportError> {
        self.call("sendTyping", json!({"recipient": [to], "stop": stop}))
            .await
            .map(|_| ())
    }

    async fn read_receipt(&self, to: &str, timestamp: u64) -> Result<(), TransportError> {
        self.call(
            "sendReceipt",
            json!({"recipient": to, "targetTimestamp": [timestamp], "type": "read"}),
        )
        .await
        .map(|_| ())
    }

    async fn identity(&self, who: &str) -> Result<Option<Identity>, TransportError> {
        let r = self.call("listIdentities", json!({"number": who})).await?;
        Ok(r.as_array().and_then(|ids| {
            ids.iter().find_map(|id| {
                Some(Identity {
                    trust: TrustLevel::parse(id.get("trustLevel")?.as_str()?)?,
                    safety_number: id.get("safetyNumber")?.as_str()?.to_string(),
                })
            })
        }))
    }

    async fn trust_verified(&self, who: &str, safety_number: &str) -> Result<(), TransportError> {
        self.call(
            "trust",
            json!({"recipient": who, "verifiedSafetyNumber": safety_number}),
        )
        .await
        .map(|_| ())
    }

    async fn version(&self) -> Result<String, TransportError> {
        let r = self.call("version", json!({})).await?;
        Ok(r.get("version")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string())
    }
}

/// Test double: pops queued events, records everything sent.
#[derive(Default)]
pub struct ScriptedTransport {
    events: Mutex<VecDeque<Inbound>>,
    identities: HashMap<String, Identity>,
    fail_send: Option<TransportError>,
    fail_when_empty: Option<fn() -> TransportError>,
    sent: Mutex<Vec<(String, String)>>,
    typing: Mutex<Vec<(String, bool)>>,
    receipts: Mutex<Vec<(String, u64)>>,
    trusted: Mutex<Vec<(String, String)>>,
}

impl ScriptedTransport {
    pub fn new(events: Vec<Inbound>) -> Self {
        ScriptedTransport {
            events: Mutex::new(events.into()),
            ..Default::default()
        }
    }

    /// `who`'s identity as `identity()` reports it.
    pub fn with_identity(mut self, who: &str, trust: TrustLevel, safety_number: &str) -> Self {
        self.identities.insert(
            who.to_string(),
            Identity {
                trust,
                safety_number: safety_number.to_string(),
            },
        );
        self
    }

    /// Every `send_text` fails with `err`.
    pub fn failing_send(mut self, err: TransportError) -> Self {
        self.fail_send = Some(err);
        self
    }

    /// Once the events run out, `next_event` returns `Err(err())`.
    pub fn then_fail(mut self, err: fn() -> TransportError) -> Self {
        self.fail_when_empty = Some(err);
        self
    }

    pub async fn push(&self, event: Inbound) {
        self.events.lock().await.push_back(event);
    }
    pub async fn sent(&self) -> Vec<(String, String)> {
        self.sent.lock().await.clone()
    }
    pub async fn typing_events(&self) -> Vec<(String, bool)> {
        self.typing.lock().await.clone()
    }
    pub async fn receipts(&self) -> Vec<(String, u64)> {
        self.receipts.lock().await.clone()
    }
    pub async fn trusted(&self) -> Vec<(String, String)> {
        self.trusted.lock().await.clone()
    }
}

#[async_trait]
impl SignalTransport for ScriptedTransport {
    async fn next_event(&self) -> Result<Inbound, TransportError> {
        loop {
            if let Some(e) = self.events.lock().await.pop_front() {
                return Ok(e);
            }
            if let Some(err) = self.fail_when_empty {
                return Err(err());
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    async fn send_text(&self, to: &str, text: &str) -> Result<u64, TransportError> {
        if let Some(e) = &self.fail_send {
            return Err(e.clone());
        }
        let mut sent = self.sent.lock().await;
        sent.push((to.to_string(), text.to_string()));
        Ok(sent.len() as u64)
    }

    async fn typing(&self, to: &str, stop: bool) -> Result<(), TransportError> {
        self.typing.lock().await.push((to.to_string(), stop));
        Ok(())
    }

    async fn read_receipt(&self, to: &str, timestamp: u64) -> Result<(), TransportError> {
        self.receipts.lock().await.push((to.to_string(), timestamp));
        Ok(())
    }

    async fn identity(&self, who: &str) -> Result<Option<Identity>, TransportError> {
        Ok(self.identities.get(who).cloned())
    }

    async fn trust_verified(&self, who: &str, safety_number: &str) -> Result<(), TransportError> {
        self.trusted
            .lock()
            .await
            .push((who.to_string(), safety_number.to_string()));
        Ok(())
    }

    async fn version(&self) -> Result<String, TransportError> {
        Ok("scripted".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixListener;

    #[test]
    fn errors_classify() {
        assert!(TransportError::Unregistered("x".into()).is_fatal());
        assert!(!TransportError::Unreachable("x".into()).is_fatal());
        assert!(!TransportError::Refused("x".into()).is_fatal());
        let e: TransportError = RpcError::Remote { code: -1, message: "Authorization failed!".into() }.into();
        assert!(e.is_fatal());
        assert!(e.to_string().contains("connect signal"));
        let e: TransportError = RpcError::Remote {
            code: -1,
            message: "Failed to send message due to untrusted identities".into(),
        }
        .into();
        assert!(e.is_untrusted_recipient());
        assert!(!e.is_fatal());
        assert!(matches!(TransportError::from(RpcError::Closed), TransportError::Unreachable(_)));
    }

    /// A fake signal-cli daemon on a real Unix socket: answers `version`,
    /// `subscribeReceive`, `listIdentities`, `send`; after a subscription
    /// it pushes one `receive` notification. `connections` counts accepts.
    async fn fake_daemon(path: PathBuf, drop_first_after_notify: bool) -> tokio::task::JoinHandle<()> {
        let listener = UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            let mut n = 0;
            loop {
                let Ok((stream, _)) = listener.accept().await else { return };
                n += 1;
                let drop_after = drop_first_after_notify && n == 1;
                tokio::spawn(async move {
                    let (r, mut w) = stream.into_split();
                    let mut lines = BufReader::new(r).lines();
                    while let Ok(Some(line)) = lines.next_line().await {
                        let req: Value = serde_json::from_str(&line).unwrap();
                        let id = req["id"].clone();
                        let result = match req["method"].as_str().unwrap() {
                            "version" => json!({"version": "0.14.6"}),
                            "subscribeReceive" => json!(0),
                            "listIdentities" => json!([{
                                "number": req["params"]["number"], "uuid": "u",
                                "fingerprint": "ab", "safetyNumber": "12345 67890",
                                "scannableSafetyNumber": null,
                                "trustLevel": "TRUSTED_VERIFIED", "addedTimestamp": 1
                            }]),
                            "send" => json!({"timestamp": 42, "results": []}),
                            _ => json!(null),
                        };
                        let resp = json!({"jsonrpc": "2.0", "id": id, "result": result});
                        w.write_all(format!("{resp}\n").as_bytes()).await.unwrap();
                        if req["method"] == "subscribeReceive" {
                            let note = json!({"jsonrpc": "2.0", "method": "receive", "params": {
                                "subscription": 0,
                                "result": {"envelope": {
                                    "sourceNumber": "+15550001111", "sourceUuid": "u",
                                    "timestamp": 7, "dataMessage": {"timestamp": 7, "message": "hi"}
                                }}
                            }});
                            w.write_all(format!("{note}\n").as_bytes()).await.unwrap();
                            if drop_after {
                                return;
                            }
                        }
                    }
                });
            }
        })
    }

    #[tokio::test]
    async fn the_cli_transport_talks_json_rpc_over_the_socket() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("signal-cli.sock");
        let _srv = fake_daemon(path.clone(), false).await;
        let t = CliTransport::new(path);
        assert_eq!(t.version().await.unwrap(), "0.14.6");
        let Inbound::Text(m) = t.next_event().await.unwrap() else { panic!() };
        assert_eq!(m.text, "hi");
        let id = t.identity("+15550001111").await.unwrap().unwrap();
        assert_eq!(id.trust, TrustLevel::TrustedVerified);
        assert_eq!(id.safety_number, "12345 67890");
        assert_eq!(t.send_text("+15550001111", "yo").await.unwrap(), 42);
    }

    #[tokio::test]
    async fn a_dropped_connection_reconnects_and_resubscribes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("signal-cli.sock");
        let _srv = fake_daemon(path.clone(), true).await;
        let t = CliTransport::new(path);
        assert!(matches!(t.next_event().await.unwrap(), Inbound::Text(_)));
        // The first connection closed after its notification.
        assert!(matches!(t.next_event().await, Err(TransportError::Unreachable(_))));
        // The next call reconnects and subscribes again.
        assert!(matches!(t.next_event().await.unwrap(), Inbound::Text(_)));
    }

    #[tokio::test]
    async fn no_daemon_is_unreachable_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let t = CliTransport::new(dir.path().join("missing.sock"));
        let e = t.version().await.unwrap_err();
        assert!(matches!(e, TransportError::Unreachable(_)), "{e}");
        assert!(!e.is_fatal());
    }
}
