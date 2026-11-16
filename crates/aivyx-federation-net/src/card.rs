//! The peer card (A19 §2): what an instance hands another to pair.
//!
//! A card names the instance, its Ed25519 identity key, its X25519 transport
//! key, and the identity's signature binding the two
//! (`aivyx_federation::identity::binding_message`). On the wire it is
//! [`CARD_PREFIX`] followed by base64url (no padding) of the card's compact
//! JSON; the key and signature fields inside are standard base64.

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use aivyx_federation::identity::{Identity, validate_instance_id, verify_binding};

use crate::NetError;
use crate::key::TransportKey;

/// Prefix of an encoded peer card (versioned).
pub const CARD_PREFIX: &str = "aivyx-peer-v1:";

/// Longest encoded card [`PeerCard::decode`] accepts.
const MAX_CARD_LEN: usize = 2048;

/// A peer's pairing card. Construct with [`PeerCard::issue`] or
/// [`PeerCard::decode`]; trust it only after [`PeerCard::verify`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerCard {
    pub instance_id: String,
    /// Standard base64 of the 32-byte Ed25519 identity key.
    pub ed25519_public: String,
    /// Standard base64 of the 32-byte X25519 transport key.
    pub x25519_public: String,
    /// Standard base64 Ed25519 signature over the binding message.
    pub binding: String,
}

impl PeerCard {
    /// Issue this instance's card: `identity` signs its binding to `key`.
    pub async fn issue(identity: &Identity, key: &TransportKey) -> Result<Self, NetError> {
        let public = key.public();
        let binding = identity
            .sign_binding(&public)
            .await
            .map_err(|e| NetError::Card(format!("sign binding: {e}")))?;
        Ok(Self {
            instance_id: identity.instance_id().to_string(),
            ed25519_public: identity.public_key_base64(),
            x25519_public: base64_encode(&public),
            binding,
        })
    }

    /// Check the instance id and the binding signature; return the X25519 key
    /// the identity vouches for.
    pub fn verify(&self) -> Result<[u8; 32], NetError> {
        validate_instance_id(&self.instance_id).map_err(|e| NetError::Card(e.to_string()))?;
        let x25519: [u8; 32] = STANDARD
            .decode(&self.x25519_public)
            .map_err(|_| NetError::Card("transport key is not valid base64".into()))?
            .try_into()
            .map_err(|_| NetError::Card("transport key is not 32 bytes".into()))?;
        verify_binding(
            &self.ed25519_public,
            &self.instance_id,
            &x25519,
            &self.binding,
        )
        .map_err(|_| {
            NetError::Card("the identity's binding to the transport key does not verify".into())
        })?;
        Ok(x25519)
    }

    /// Encode as `aivyx-peer-v1:<base64url(JSON)>`.
    pub fn encode(&self) -> String {
        // Serializing four `String` fields cannot fail.
        let json = serde_json::to_vec(self).unwrap_or_default();
        format!("{CARD_PREFIX}{}", URL_SAFE_NO_PAD.encode(json))
    }

    /// Decode a card produced by [`PeerCard::encode`]. Surrounding whitespace
    /// is ignored. Decoding does not verify; call [`PeerCard::verify`].
    pub fn decode(s: &str) -> Result<Self, NetError> {
        let s = s.trim();
        if s.len() > MAX_CARD_LEN {
            return Err(NetError::Card(format!(
                "card is longer than {MAX_CARD_LEN} characters"
            )));
        }
        let body = s
            .strip_prefix(CARD_PREFIX)
            .ok_or_else(|| NetError::Card(format!("card does not start with {CARD_PREFIX}")))?;
        let json = URL_SAFE_NO_PAD
            .decode(body)
            .map_err(|_| NetError::Card("card is not valid base64url".into()))?;
        serde_json::from_slice(&json)
            .map_err(|e| NetError::Card(format!("card is not valid JSON: {e}")))
    }
}

/// A short, stable label for an X25519 key: the first 16 hex characters of
/// its SHA-256. Safe to log and audit.
pub fn fingerprint(x25519_public: &[u8; 32]) -> String {
    let mut h = hex::encode(Sha256::digest(x25519_public));
    h.truncate(16);
    h
}

/// Standard base64, as the card's key fields use.
pub(crate) fn base64_encode(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key::TransportKey;
    use aivyx_federation::identity::Identity;

    #[tokio::test]
    async fn a_card_round_trips_and_verifies() {
        let id = Identity::generate("desktop".into()).unwrap();
        let key = TransportKey::generate().unwrap();
        let card = PeerCard::issue(&id, &key).await.unwrap();
        let text = card.encode();
        assert!(text.starts_with(CARD_PREFIX));
        let back = PeerCard::decode(&text).unwrap();
        assert_eq!(back.verify().unwrap(), key.public());
    }

    #[tokio::test]
    async fn a_card_with_a_swapped_transport_key_fails() {
        let id = Identity::generate("desktop".into()).unwrap();
        let mut card = PeerCard::issue(&id, &TransportKey::generate().unwrap())
            .await
            .unwrap();
        card.x25519_public = base64_encode(&TransportKey::generate().unwrap().public());
        assert!(card.verify().is_err());
    }

    #[test]
    fn garbage_cards_are_refused() {
        for s in ["", "aivyx-peer-v1:", "aivyx-peer-v1:!!!", "other:abc"] {
            assert!(PeerCard::decode(s).is_err(), "{s}");
        }
    }

    #[test]
    fn an_oversized_card_is_refused() {
        let s = format!("{CARD_PREFIX}{}", "A".repeat(2048));
        assert!(PeerCard::decode(&s).is_err());
    }

    #[test]
    fn a_fingerprint_is_16_hex_chars() {
        let f = fingerprint(&[9u8; 32]);
        assert_eq!(f.len(), 16);
        assert!(f.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
