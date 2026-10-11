//! The transport seam between the channel and Matrix. Production is
//! `sdk::SdkTransport`; tests use [`ScriptedTransport`]. No `matrix-sdk`
//! type crosses this boundary.

use std::collections::VecDeque;
use std::time::Duration;

use async_trait::async_trait;
use thiserror::Error;
use tokio::sync::Mutex;

/// A decrypted text message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingMessage {
    pub room_id: String,
    pub sender: String,
    pub text: String,
    /// Sent from a device the assistant trusts (`VerificationState::Verified`).
    pub verified: bool,
}

/// Everything the session loop reacts to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inbound {
    Message(IncomingMessage),
    /// An encrypted event the assistant couldn't decrypt.
    Undecryptable { room_id: String, sender: String },
    /// An invite to a room.
    Invite { room_id: String, inviter: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingMessage {
    pub room_id: String,
    pub text: String,
}

#[derive(Debug, Error)]
pub enum TransportError {
    #[error("matrix transport error: {0}")]
    Platform(String),
    /// The homeserver rejected the access token — the device was logged
    /// out. The channel stops; the operator runs `connect matrix` again.
    #[error("matrix rejected the assistant's login ({0}) — run `aivyx-pa connect matrix` again")]
    Unauthorized(String),
}

#[async_trait]
pub trait MatrixTransport: Send + Sync {
    async fn next_inbound(&self) -> Result<Inbound, TransportError>;
    async fn send_message(&self, msg: OutgoingMessage) -> Result<(), TransportError>;
    async fn join_room(&self, room_id: &str) -> Result<(), TransportError>;
    async fn decline_invite(&self, room_id: &str) -> Result<(), TransportError>;
    async fn room_is_encrypted(&self, room_id: &str) -> Result<bool, TransportError>;
}

/// Test double: pops queued inbound items, records everything sent.
pub struct ScriptedTransport {
    queue: Mutex<VecDeque<Inbound>>,
    sent: Mutex<Vec<OutgoingMessage>>,
    joined: Mutex<Vec<String>>,
    declined: Mutex<Vec<String>>,
    encrypted: Vec<String>,
}

impl ScriptedTransport {
    pub fn with_queue(queue: Vec<Inbound>) -> Self {
        ScriptedTransport {
            queue: Mutex::new(queue.into()),
            sent: Mutex::new(Vec::new()),
            joined: Mutex::new(Vec::new()),
            declined: Mutex::new(Vec::new()),
            encrypted: Vec::new(),
        }
    }

    /// Rooms `room_is_encrypted` reports as encrypted (all others: not).
    pub fn encrypted_rooms(mut self, rooms: Vec<&str>) -> Self {
        self.encrypted = rooms.into_iter().map(str::to_string).collect();
        self
    }

    pub async fn sent(&self) -> Vec<OutgoingMessage> {
        self.sent.lock().await.clone()
    }

    pub async fn joined(&self) -> Vec<String> {
        self.joined.lock().await.clone()
    }

    pub async fn declined(&self) -> Vec<String> {
        self.declined.lock().await.clone()
    }

    pub async fn push_inbound(&self, item: Inbound) {
        self.queue.lock().await.push_back(item);
    }
}

#[async_trait]
impl MatrixTransport for ScriptedTransport {
    async fn next_inbound(&self) -> Result<Inbound, TransportError> {
        loop {
            if let Some(item) = self.queue.lock().await.pop_front() {
                return Ok(item);
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    async fn send_message(&self, msg: OutgoingMessage) -> Result<(), TransportError> {
        self.sent.lock().await.push(msg);
        Ok(())
    }

    async fn join_room(&self, room_id: &str) -> Result<(), TransportError> {
        self.joined.lock().await.push(room_id.to_string());
        Ok(())
    }

    async fn decline_invite(&self, room_id: &str) -> Result<(), TransportError> {
        self.declined.lock().await.push(room_id.to_string());
        Ok(())
    }

    async fn room_is_encrypted(&self, room_id: &str) -> Result<bool, TransportError> {
        Ok(self.encrypted.iter().any(|r| r == room_id))
    }
}

#[cfg(test)]
mod transport_tests {
    use super::*;

    fn msg(text: &str) -> Inbound {
        Inbound::Message(IncomingMessage {
            room_id: "!r:x".into(),
            sender: "@me:x".into(),
            text: text.into(),
            verified: true,
        })
    }

    #[tokio::test]
    async fn queue_pops_in_order_and_sends_are_recorded() {
        let t = ScriptedTransport::with_queue(vec![msg("a"), msg("b")]);
        assert_eq!(t.next_inbound().await.unwrap(), msg("a"));
        assert_eq!(t.next_inbound().await.unwrap(), msg("b"));
        t.send_message(OutgoingMessage { room_id: "!r:x".into(), text: "hi".into() })
            .await
            .unwrap();
        t.join_room("!r:x").await.unwrap();
        t.decline_invite("!s:x").await.unwrap();
        assert_eq!(t.sent().await.len(), 1);
        assert_eq!(t.joined().await, vec!["!r:x".to_string()]);
        assert_eq!(t.declined().await, vec!["!s:x".to_string()]);
    }

    #[tokio::test]
    async fn encrypted_rooms_are_reported() {
        let t = ScriptedTransport::with_queue(vec![]).encrypted_rooms(vec!["!e:x"]);
        assert!(t.room_is_encrypted("!e:x").await.unwrap());
        assert!(!t.room_is_encrypted("!plain:x").await.unwrap());
    }

    #[test]
    fn unauthorized_tells_the_operator_what_to_do() {
        let e = TransportError::Unauthorized("M_UNKNOWN_TOKEN".into());
        assert!(e.to_string().contains("aivyx-pa connect matrix"));
    }
}
