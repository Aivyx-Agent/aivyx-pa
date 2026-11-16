//! Federation transport (FED.3) — the network side of federation between one
//! operator's own PA instances. See amendment A19 §2
//! (`docs/amendments/2026-11-16-federation-transport.md`).
//!
//! - [`TransportKey`]: the X25519 static key, sealed at rest under the storage
//!   master key (same pattern as the Ed25519 federation identity).
//! - [`PeerCard`]: what one instance hands another to pair — its instance id,
//!   Ed25519 identity key, X25519 transport key and the identity's signature
//!   binding the two.
//! - [`RequestFrame`] / [`ResponseFrame`]: the application envelopes.
//! - [`Link`]: a mutually authenticated `Noise_XX_25519_ChaChaPoly_SHA256`
//!   session over TCP, with length-prefixed, chunked application frames of
//!   at most [`MAX_FRAME`] bytes.
//! - [`ping`] / [`request`]: the client side.
//!
//! Error messages never include key bytes; an unknown key is reported only by
//! its [`fingerprint`].
#![forbid(unsafe_code)]

pub mod card;
pub mod client;
pub mod frame;
pub mod key;
pub mod link;

pub use card::{CARD_PREFIX, PeerCard, fingerprint};
pub use client::{ping, request};
pub use frame::{MAX_FRAME, RequestFrame, ResponseFrame};
pub use key::TransportKey;
pub use link::{HANDSHAKE_TIMEOUT, Link, NOISE_PARAMS};

/// Errors from the federation transport. Messages never contain key material.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NetError {
    /// A socket error (connect, read, write, or the peer closed mid-frame).
    #[error("federation transport I/O error: {0}")]
    Io(String),
    /// The Noise handshake failed (malformed or undecryptable message).
    #[error("federation handshake failed: {0}")]
    Handshake(String),
    /// The remote static key is not the expected / a known peer's key.
    #[error("federation peer key not recognised (fingerprint {fingerprint})")]
    UnknownPeer { fingerprint: String },
    /// A malformed, undecryptable or over-cap application frame.
    #[error("federation frame error: {0}")]
    Frame(String),
    /// A step did not finish in time (`"handshake"`, `"reply"`).
    #[error("federation {0} timed out")]
    Timeout(&'static str),
    /// A peer card failed to decode or verify.
    #[error("invalid peer card: {0}")]
    Card(String),
    /// The transport key could not be generated, sealed or loaded.
    #[error("federation transport key error: {0}")]
    Key(String),
    /// The peer refused the request.
    #[error("federation request denied: {0}")]
    Denied(String),
}

impl From<std::io::Error> for NetError {
    fn from(e: std::io::Error) -> Self {
        NetError::Io(e.to_string())
    }
}
