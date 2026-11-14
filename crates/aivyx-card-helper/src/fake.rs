//! A card helper with real X25519 math and no hardware, for tests.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use secrecy::{ExposeSecret, SecretString};
use x25519_dalek::{PublicKey, StaticSecret};
use zeroize::Zeroizing;

use crate::{CardHelper, CardInfo, ErrorKind, HelperError};

struct FakeCardState {
    secret: Option<[u8; 32]>,
    tries: u8,
    present: bool,
}

pub struct FakeHelper {
    cards: Mutex<HashMap<String, FakeCardState>>,
    pin: String,
    calls: AtomicUsize,
}

fn card_err(kind: ErrorKind, message: &str, tries_left: Option<u8>) -> HelperError {
    HelperError::Card {
        kind,
        message: message.into(),
        tries_left,
    }
}

impl FakeHelper {
    fn new() -> Self {
        Self {
            cards: Mutex::new(HashMap::new()),
            pin: "123456".into(),
            calls: AtomicUsize::new(0),
        }
    }

    pub fn with_card(serial: &str, secret: [u8; 32]) -> Self {
        let h = Self::new();
        h.add_card(serial, secret);
        h
    }

    pub fn empty_card(serial: &str) -> Self {
        let h = Self::new();
        h.cards.lock().unwrap().insert(
            serial.into(),
            FakeCardState {
                secret: None,
                tries: 3,
                present: true,
            },
        );
        h
    }

    /// Plug in another card holding `secret`.
    pub fn add_card(&self, serial: &str, secret: [u8; 32]) {
        self.cards.lock().unwrap().insert(
            serial.into(),
            FakeCardState {
                secret: Some(secret),
                tries: 3,
                present: true,
            },
        );
    }

    /// The PIN it accepts (default "123456").
    pub fn pin(mut self, pin: &str) -> Self {
        self.pin = pin.into();
        self
    }

    /// Simulate unplugging.
    pub fn unplugged(&self, serial: &str) {
        if let Some(c) = self.cards.lock().unwrap().get_mut(serial) {
            c.present = false;
        }
    }

    /// How many `ecdh` calls it saw.
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl CardHelper for FakeHelper {
    fn list(&self) -> Result<Vec<CardInfo>, HelperError> {
        let cards = self.cards.lock().unwrap();
        let mut v: Vec<CardInfo> = cards
            .iter()
            .filter(|(_, c)| c.present)
            .map(|(s, c)| CardInfo {
                serial: s.clone(),
                has_decryption_key: c.secret.is_some(),
            })
            .collect();
        v.sort_by(|a, b| a.serial.cmp(&b.serial));
        Ok(v)
    }

    fn public_key(&self, serial: &str) -> Result<Option<[u8; 32]>, HelperError> {
        let cards = self.cards.lock().unwrap();
        let c = cards
            .get(serial)
            .filter(|c| c.present)
            .ok_or_else(|| card_err(ErrorKind::CardNotFound, "card not found", None))?;
        Ok(c.secret
            .map(|s| *PublicKey::from(&StaticSecret::from(s)).as_bytes()))
    }

    fn generate(&self, serial: &str, _admin_pin: &SecretString) -> Result<[u8; 32], HelperError> {
        let mut cards = self.cards.lock().unwrap();
        let c = cards
            .get_mut(serial)
            .filter(|c| c.present)
            .ok_or_else(|| card_err(ErrorKind::CardNotFound, "card not found", None))?;
        if c.secret.is_some() {
            return Err(card_err(
                ErrorKind::SlotOccupied,
                "the decryption slot already holds a key",
                None,
            ));
        }
        let secret = [0x42u8; 32];
        c.secret = Some(secret);
        Ok(*PublicKey::from(&StaticSecret::from(secret)).as_bytes())
    }

    fn ecdh(
        &self,
        serial: &str,
        pin: &SecretString,
        their_public: &[u8; 32],
    ) -> Result<Zeroizing<[u8; 32]>, HelperError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let mut cards = self.cards.lock().unwrap();
        let c = cards
            .get_mut(serial)
            .filter(|c| c.present)
            .ok_or_else(|| card_err(ErrorKind::CardNotFound, "card not found", None))?;
        let Some(secret) = c.secret else {
            return Err(card_err(
                ErrorKind::UnsupportedKey,
                "the card has no decryption key",
                None,
            ));
        };
        if c.tries == 0 {
            return Err(card_err(ErrorKind::PinBlocked, "PIN blocked", Some(0)));
        }
        if pin.expose_secret() != self.pin {
            c.tries -= 1;
            return Err(if c.tries == 0 {
                card_err(ErrorKind::PinBlocked, "PIN blocked", Some(0))
            } else {
                card_err(ErrorKind::WrongPin, "PIN incorrect", Some(c.tries))
            });
        }
        c.tries = 3;
        let shared = StaticSecret::from(secret).diffie_hellman(&PublicKey::from(*their_public));
        Ok(Zeroizing::new(*shared.as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::SecretString;
    use x25519_dalek::{PublicKey, StaticSecret};
    #[test]
    fn the_fake_does_real_ecdh_and_checks_the_pin() {
        let h = FakeHelper::with_card("S1", [5u8; 32]);
        let card_pub = h.public_key("S1").unwrap().unwrap();
        let eph = StaticSecret::from([8u8; 32]);
        let eph_pub = *PublicKey::from(&eph).as_bytes();
        let s = h
            .ecdh("S1", &SecretString::from("123456"), &eph_pub)
            .unwrap();
        assert_eq!(
            *s,
            *eph.diffie_hellman(&PublicKey::from(card_pub)).as_bytes()
        );
        let err = h
            .ecdh("S1", &SecretString::from("000000"), &eph_pub)
            .unwrap_err();
        assert!(matches!(
            err,
            HelperError::Card {
                kind: ErrorKind::WrongPin,
                tries_left: Some(2),
                ..
            }
        ));
        h.unplugged("S1");
        assert!(h.list().unwrap().is_empty());
    }

    fn eph() -> [u8; 32] {
        *PublicKey::from(&StaticSecret::from([8u8; 32])).as_bytes()
    }

    #[test]
    fn blocked_at_zero_tries() {
        let h = FakeHelper::with_card("S1", [5u8; 32]);
        let bad = SecretString::from("000000");
        for _ in 0..3 {
            let _ = h.ecdh("S1", &bad, &eph());
        }
        // Even the right PIN is refused now.
        let err = h
            .ecdh("S1", &SecretString::from("123456"), &eph())
            .unwrap_err();
        assert!(matches!(
            err,
            HelperError::Card {
                kind: ErrorKind::PinBlocked,
                tries_left: Some(0),
                ..
            }
        ));
    }

    #[test]
    fn tries_reset_on_success() {
        let h = FakeHelper::with_card("S1", [5u8; 32]);
        let _ = h.ecdh("S1", &SecretString::from("000000"), &eph());
        h.ecdh("S1", &SecretString::from("123456"), &eph()).unwrap();
        let err = h
            .ecdh("S1", &SecretString::from("000000"), &eph())
            .unwrap_err();
        assert!(matches!(
            err,
            HelperError::Card {
                kind: ErrorKind::WrongPin,
                tries_left: Some(2),
                ..
            }
        ));
    }

    #[test]
    fn generate_refused_on_an_occupied_slot() {
        let h = FakeHelper::with_card("S1", [5u8; 32]);
        let err = h
            .generate("S1", &SecretString::from("12345678"))
            .unwrap_err();
        assert!(matches!(
            err,
            HelperError::Card {
                kind: ErrorKind::SlotOccupied,
                ..
            }
        ));
    }

    #[test]
    fn generate_then_public_key_returns_the_new_key() {
        let h = FakeHelper::empty_card("S1");
        assert_eq!(h.public_key("S1").unwrap(), None);
        let generated = h.generate("S1", &SecretString::from("12345678")).unwrap();
        assert_eq!(h.public_key("S1").unwrap(), Some(generated));
    }

    #[test]
    fn missing_helper_is_not_installed() {
        assert!(matches!(
            crate::MissingHelper.list(),
            Err(HelperError::NotInstalled)
        ));
    }
}
