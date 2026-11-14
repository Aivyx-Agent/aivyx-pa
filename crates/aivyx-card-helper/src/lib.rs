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
    // The decoded bytes may be a shared secret: zeroize the buffer.
    let raw = Zeroizing::new(B64.decode(s).ok()?);
    <[u8; 32]>::try_from(raw.as_slice()).ok()
}

/// A PIN, admin PIN or shared secret on the wire: serialized as a plain
/// string, zeroized on drop, and `[redacted]` in `Debug`, so a `{:?}` of a
/// [`Request`] or [`Reply`] (or an error built from one) never shows it.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Sensitive(String);

impl Sensitive {
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Move the value into a `SecretString` without copying it.
    pub fn into_secret(mut self) -> SecretString {
        SecretString::from(std::mem::take(&mut self.0))
    }
}

impl From<String> for Sensitive {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl From<&str> for Sensitive {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl Drop for Sensitive {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.0.zeroize();
    }
}

impl std::fmt::Debug for Sensitive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("[redacted]")
    }
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
        admin_pin: Sensitive,
    },
    Ecdh {
        serial: String,
        pin: Sensitive,
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
        secret: Sensitive,
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

impl Reply {
    fn name(&self) -> &'static str {
        match self {
            Reply::Cards { .. } => "cards",
            Reply::PublicKey { .. } => "public_key",
            Reply::Shared { .. } => "shared",
            Reply::Error { .. } => "error",
        }
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
            // Name the variant only: never format a reply's fields.
            other => HelperError::Broken(format!("unexpected reply ({})", other.name())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_and_errors_never_show_a_pin_or_secret() {
        let r = Request::Ecdh {
            serial: "S1".into(),
            pin: "424242".into(),
            their_public: "AA==".into(),
        };
        let g = Request::Generate {
            serial: "S1".into(),
            admin_pin: "87654321".into(),
        };
        let shared = Reply::Shared {
            secret: "c2VjcmV0c2VjcmV0".into(),
        };
        for text in [
            format!("{r:?}"),
            format!("{g:?}"),
            format!("{shared:?}"),
            HelperError::from(shared.clone()).to_string(),
        ] {
            for secret in ["424242", "87654321", "c2VjcmV0c2VjcmV0"] {
                assert!(!text.contains(secret), "{text}");
            }
        }
        assert!(format!("{r:?}").contains("[redacted]"));
        assert_eq!(
            HelperError::from(shared).to_string(),
            "the card helper failed: unexpected reply (shared)"
        );
    }
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
