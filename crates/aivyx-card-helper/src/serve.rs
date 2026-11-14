//! Request handling for `aivyx-pa-card`, generic over how a card is opened
//! so tests can run against `FakeCard`.
use aivyx_yubi::{YubiError, decrypt, discovery, pin};
use openpgp_card::{Card, state::Open};

use crate::{CardInfo, ErrorKind, Reply, Request, decode_key, encode_key};

/// Opens the card with the given serial (or the only card, for `None`).
pub type OpenCard<'a> = dyn Fn(Option<&str>) -> Result<Card<Open>, YubiError> + 'a;

fn error(e: &YubiError, tries_left: Option<u8>) -> Reply {
    let kind = match e {
        YubiError::CardNotFound(m) if m.contains("pcscd") => ErrorKind::NoReader,
        YubiError::CardNotFound(_) | YubiError::WrongCard { .. } => ErrorKind::CardNotFound,
        YubiError::PinIncorrect | YubiError::EmptyPin => ErrorKind::WrongPin,
        YubiError::PinBlocked { .. } => ErrorKind::PinBlocked,
        YubiError::TouchTimeout => ErrorKind::TouchTimeout,
        YubiError::DecryptionSlotOccupied => ErrorKind::SlotOccupied,
        YubiError::UnsupportedDecryptionKey(_) => ErrorKind::UnsupportedKey,
        _ => ErrorKind::Other,
    };
    Reply::Error {
        kind,
        message: e.to_string(),
        tries_left,
    }
}

pub fn handle(
    req: Request,
    open: &OpenCard<'_>,
    list: &dyn Fn() -> Result<Vec<discovery::CardSummary>, YubiError>,
) -> Reply {
    let run = || -> Result<Reply, Reply> {
        match req {
            Request::List => {
                let cards = list().map_err(|e| error(&e, None))?;
                let mut out = Vec::new();
                for c in cards {
                    let mut card = open(Some(&c.serial)).map_err(|e| error(&e, None))?;
                    let mut tx = card
                        .transaction()
                        .map_err(|e| error(&YubiError::from(e), None))?;
                    let has = matches!(decrypt::decryption_public_key(&mut tx), Ok(Some(_)));
                    out.push(CardInfo {
                        serial: c.serial,
                        has_decryption_key: has,
                    });
                }
                Ok(Reply::Cards { cards: out })
            }
            Request::PublicKey { serial } => {
                let mut card = open(Some(&serial)).map_err(|e| error(&e, None))?;
                let mut tx = card
                    .transaction()
                    .map_err(|e| error(&YubiError::from(e), None))?;
                let public =
                    decrypt::decryption_public_key(&mut tx).map_err(|e| error(&e, None))?;
                Ok(Reply::PublicKey {
                    public: public.map(|p| encode_key(&p.into())),
                })
            }
            Request::Generate { serial, admin_pin } => {
                let mut card = open(Some(&serial)).map_err(|e| error(&e, None))?;
                let mut tx = card
                    .transaction()
                    .map_err(|e| error(&YubiError::from(e), None))?;
                let mut admin = tx
                    .as_admin_card(admin_pin.into_secret())
                    .map_err(|e| error(&YubiError::from(e), None))?;
                let public =
                    decrypt::generate_decryption_key(&mut admin).map_err(|e| error(&e, None))?;
                Ok(Reply::PublicKey {
                    public: Some(encode_key(&public.into())),
                })
            }
            Request::Ecdh {
                serial,
                pin: user_pin,
                their_public,
            } => {
                let theirs = decode_key(&their_public).ok_or_else(|| Reply::Error {
                    kind: ErrorKind::Other,
                    message: "their_public is not a 32-byte base64 key".into(),
                    tries_left: None,
                })?;
                let mut card = open(Some(&serial)).map_err(|e| error(&e, None))?;
                let mut tx = card
                    .transaction()
                    .map_err(|e| error(&YubiError::from(e), None))?;
                match decrypt::ecdh(&mut tx, user_pin.into_secret(), &theirs) {
                    Ok(s) => Ok(Reply::Shared {
                        secret: encode_key(s.as_bytes()).into(),
                    }),
                    Err(e @ YubiError::PinIncorrect) => {
                        // The card's retry counter is cached when the card is
                        // opened, so re-open to read the post-failure value.
                        drop(tx);
                        drop(card);
                        let left = open(Some(&serial)).ok().and_then(|mut c| {
                            let mut t = c.transaction().ok()?;
                            pin::pin_retries(&mut t).ok().map(|(pw1, _)| pw1)
                        });
                        Err(error(&e, left))
                    }
                    Err(e) => Err(error(&e, None)),
                }
            }
        }
    };
    run().unwrap_or_else(|r| r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_yubi::testing::FakeCard;

    fn opener(fake: FakeCard) -> impl Fn(Option<&str>) -> Result<Card<Open>, YubiError> {
        move |_| Card::new(fake.clone()).map_err(YubiError::from)
    }
    fn no_list() -> Result<Vec<discovery::CardSummary>, YubiError> {
        Ok(vec![])
    }

    #[test]
    fn ecdh_over_the_wire_matches_the_card() {
        use x25519_dalek::{PublicKey, StaticSecret};
        let fake = FakeCard::new().with_decryption_key([5u8; 32]);
        let card_pub = fake.decryption_public().unwrap();
        let eph = StaticSecret::from([8u8; 32]);
        let req = Request::Ecdh {
            serial: "x".into(),
            pin: "123456".into(),
            their_public: crate::encode_key(PublicKey::from(&eph).as_bytes()),
        };
        let Reply::Shared { secret } = handle(req, &opener(fake), &no_list) else {
            panic!()
        };
        assert_eq!(
            crate::decode_key(secret.expose()).unwrap(),
            *eph.diffie_hellman(&PublicKey::from(card_pub)).as_bytes()
        );
    }

    #[test]
    fn a_wrong_pin_reports_the_tries_left() {
        let fake = FakeCard::new().with_decryption_key([5u8; 32]);
        let req = Request::Ecdh {
            serial: "x".into(),
            pin: "000000".into(),
            their_public: crate::encode_key(&[1u8; 32]),
        };
        assert!(matches!(
            handle(req, &opener(fake), &no_list),
            Reply::Error {
                kind: ErrorKind::WrongPin,
                tries_left: Some(2),
                ..
            }
        ));
    }

    #[test]
    fn generate_refuses_an_occupied_slot() {
        let fake = FakeCard::new().with_decryption_key([5u8; 32]);
        let req = Request::Generate {
            serial: "x".into(),
            admin_pin: "12345678".into(),
        };
        assert!(matches!(
            handle(req, &opener(fake), &no_list),
            Reply::Error {
                kind: ErrorKind::SlotOccupied,
                ..
            }
        ));
    }

    #[test]
    fn an_empty_slot_has_no_public_key() {
        let req = Request::PublicKey { serial: "x".into() };
        assert_eq!(
            handle(req, &opener(FakeCard::new()), &no_list),
            Reply::PublicKey { public: None }
        );
    }

    #[test]
    fn an_unknown_request_is_refused() {
        assert!(serde_json::from_slice::<Request>(br#"{"op":"format_disk"}"#).is_err());
    }
}
