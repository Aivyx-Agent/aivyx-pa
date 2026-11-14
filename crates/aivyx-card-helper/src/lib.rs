//! The `aivyx-pa-card` helper (Amendment A18). PA's static musl release
//! can't link PC/SC, so card work runs in this separate, glibc/macOS
//! binary: one JSON `Request` on stdin, one `Reply` on stdout, then exit.
//! This library half (always built) holds the wire types and the client;
//! the binary (feature `card`) holds the card code.

pub mod client;
#[cfg(any(test, feature = "test-util"))]
pub mod fake;
#[cfg(feature = "card")]
pub mod serve;

pub use client::ProcessHelper;
#[cfg(any(test, feature = "test-util"))]
pub use fake::FakeHelper;

use base64::Engine;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

pub(crate) const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

pub fn encode_key(k: &[u8; 32]) -> String {
    B64.encode(k)
}

pub fn decode_key(s: &str) -> Option<[u8; 32]> {
    B64.decode(s).ok()?.try_into().ok()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    List,
    PublicKey {
        serial: String,
    },
    Generate {
        serial: String,
        admin_pin: String,
    },
    Ecdh {
        serial: String,
        pin: String,
        their_public: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Reply {
    Cards {
        cards: Vec<CardInfo>,
    },
    PublicKey {
        public: Option<String>,
    },
    Shared {
        secret: String,
    },
    Error {
        kind: ErrorKind,
        message: String,
        tries_left: Option<u8>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CardInfo {
    pub serial: String,
    pub has_decryption_key: bool,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    NoReader,
    CardNotFound,
    WrongPin,
    PinBlocked,
    TouchTimeout,
    SlotOccupied,
    UnsupportedKey,
    Other,
}

#[derive(Debug, thiserror::Error)]
pub enum HelperError {
    #[error("card unlock needs the `aivyx-pa-card` helper: run `aivyx-pa tool-programs install`")]
    NotInstalled,
    #[error("{message}")]
    Card {
        kind: ErrorKind,
        message: String,
        tries_left: Option<u8>,
    },
    #[error("the card helper failed: {0}")]
    Broken(String),
}

pub trait CardHelper: Send + Sync {
    fn list(&self) -> Result<Vec<CardInfo>, HelperError>;
    fn public_key(&self, serial: &str) -> Result<Option<[u8; 32]>, HelperError>;
    fn generate(&self, serial: &str, admin_pin: &SecretString) -> Result<[u8; 32], HelperError>;
    fn ecdh(
        &self,
        serial: &str,
        pin: &SecretString,
        their_public: &[u8; 32],
    ) -> Result<Zeroizing<[u8; 32]>, HelperError>;
}

/// A helper that is not installed: every call says so.
pub struct MissingHelper;

impl CardHelper for MissingHelper {
    fn list(&self) -> Result<Vec<CardInfo>, HelperError> {
        Err(HelperError::NotInstalled)
    }
    fn public_key(&self, _: &str) -> Result<Option<[u8; 32]>, HelperError> {
        Err(HelperError::NotInstalled)
    }
    fn generate(&self, _: &str, _: &SecretString) -> Result<[u8; 32], HelperError> {
        Err(HelperError::NotInstalled)
    }
    fn ecdh(
        &self,
        _: &str,
        _: &SecretString,
        _: &[u8; 32],
    ) -> Result<Zeroizing<[u8; 32]>, HelperError> {
        Err(HelperError::NotInstalled)
    }
}

impl From<Reply> for HelperError {
    fn from(r: Reply) -> Self {
        match r {
            Reply::Error {
                kind,
                message,
                tries_left,
            } => HelperError::Card {
                kind,
                message,
                tries_left,
            },
            other => HelperError::Broken(format!("unexpected reply {other:?}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_wire_format_is_stable() {
        let r = Request::Ecdh {
            serial: "0006:00112233".into(),
            pin: "123456".into(),
            their_public: "AA==".into(),
        };
        assert_eq!(
            serde_json::to_string(&r).unwrap(),
            r#"{"op":"ecdh","serial":"0006:00112233","pin":"123456","their_public":"AA=="}"#
        );
        let e: Reply = serde_json::from_str(
            r#"{"result":"error","kind":"wrong_pin","message":"PIN incorrect","tries_left":2}"#,
        )
        .unwrap();
        assert_eq!(
            e,
            Reply::Error {
                kind: ErrorKind::WrongPin,
                message: "PIN incorrect".into(),
                tries_left: Some(2)
            }
        );
    }
}
