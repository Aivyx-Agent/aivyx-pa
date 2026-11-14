# YubiKey Unlock Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let an operator enroll OpenPGP cards (YubiKeys) that unlock Aivyx PA's encrypted store with a PIN and a touch, so no passphrase has to be saved. The daemon can then start locked and be unlocked later.

**Architecture:** Each card's decryption-slot X25519 key wraps a copy of the existing master key, so the store format is untouched. The entries live in a per-instance `card-unlock.toml`. Card access runs in a one-shot helper binary, `aivyx-pa-card`, linked to PC/SC, because PA's static musl release can't link it; the daemon and the CLI talk to it as a child process over JSON on stdin and stdout. A daemon with no passphrase source and at least one enrolled card serves a minimal "locked" socket until an `Unlock` request yields the master key, then carries on with the normal start-up.

**Tech Stack:** Rust 2024; `openpgp-card` 0.7 and `card-backend` 0.2 (in aivyx-yubi); `x25519-dalek` 2; `chacha20poly1305` 0.10 and `hkdf` 0.12 (aivyx-crypto); tokio Unix sockets; serde_json and toml.

**Spec:** `docs/superpowers/specs/2026-11-14-yubikey-unlock-design.md`. **Amendment:** A18 (`docs/amendments/2026-11-14-card-unlock.md`).

## Global Constraints

- Repos: `aivyx-yubi` (Part A), then `aivyx-pa` (Part B). Commit with `git commit -s` in both.
- Every commit ends with `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- aivyx-pa: zero clippy warnings, `cargo clippy --workspace --all-targets -- -D warnings`; full `cargo test --workspace` before each push.
- aivyx-yubi: `cargo test`, `cargo test --features test-util`, `cargo clippy --all-targets -- -D warnings`.
- Never touch a real card from a test; every card test uses `aivyx_yubi::testing::FakeCard`.
- HKDF salt for wrap keys: exactly `b"aivyx-pa-card-unlock-v1"`.
- The file is `card-unlock.toml`, mode `0600`, beside `aivyx-pa.toml` (in `InstancePaths::config_dir()`). Its path is built only by `InstancePaths`.
- A PIN never appears in argv or the environment; it travels only on stdin, the `0600` socket, or in memory as `secrecy::SecretString`.
- The touch window is the card's own (about 15 s). The helper adds no extra wait.
- After 5 failed unlocks in a row, the daemon refuses unlocks for 30 s.
- The helper binary is named `aivyx-pa-card`. Its crate is `crates/aivyx-card-helper`, with the PC/SC code behind the feature `card`.
- PA's existing `yubikey` feature (federation) is unchanged.
- No `cargo fmt` over whole files in aivyx-pa; format new files only, with `rustfmt --edition 2024 <file>`.

---

## Part A — aivyx-yubi

### Task 1: FakeCard decryption slot

**Files:**
- Modify: `aivyx-yubi/Cargo.toml`
- Modify: `aivyx-yubi/src/testing.rs`

**Interfaces:**
- Produces, on `FakeCard`:
  - `with_decryption_key(self, secret: [u8; 32]) -> Self`: the slot holds that X25519 key.
  - `with_rsa_decryption_key(self) -> Self`: the slot holds an RSA key.
  - `decryption_key_generated(&self) -> bool`.
  - `decryption_public(&self) -> Option<[u8; 32]>`.
  - `last_deciphered(&self) -> Vec<u8>`: the raw PSO:DECIPHER data field.
- Card behaviour:
  - GENERATE with CRT `B8` answers the decryption slot;
  - PUT DATA `C2` (algorithm) and `D7` (UIF dec) are stored;
  - VERIFY P2 `82` sets `verified_pw1_user`;
  - PSO:DECIPHER (P1 `80`, P2 `86`) returns `X25519(secret, eph)`.

- [ ] **Step 1: Add x25519-dalek for the fake only.** In `Cargo.toml`:

```toml
[dependencies]
# Only for testing.rs's FakeCard decryption slot (real X25519 math).
x25519-dalek = { version = "2", features = ["static_secrets"], optional = true }
zeroize = "1"

[dev-dependencies]
x25519-dalek = { version = "2", features = ["static_secrets"] }

[features]
test-util = ["dep:x25519-dalek"]
```

- [ ] **Step 2: Write the failing tests** at the end of `testing.rs`'s `mod tests`:

```rust
    #[test]
    fn the_decryption_slot_does_real_x25519() {
        use x25519_dalek::{PublicKey, StaticSecret};
        let secret = [7u8; 32];
        let fake = FakeCard::new().with_decryption_key(secret);
        let probe = fake.clone();
        let mut card = Card::new(fake).unwrap();
        let mut tx = card.transaction().unwrap();
        let card_pub = probe.decryption_public().unwrap();
        assert_eq!(card_pub, *PublicKey::from(&StaticSecret::from(secret)).as_bytes());

        let eph = StaticSecret::from([9u8; 32]);
        let eph_pub = PublicKey::from(&eph);
        tx.verify_user_pin(SecretString::from("123456")).unwrap();
        let shared = tx
            .card()
            .decipher(openpgp_card::ocard::crypto::Cryptogram::ECDH(eph_pub.as_bytes()))
            .unwrap();
        assert_eq!(shared, eph.diffie_hellman(&PublicKey::from(card_pub)).as_bytes().to_vec());
        assert!(probe.last_deciphered().ends_with(eph_pub.as_bytes()));
    }

    #[test]
    fn decipher_needs_the_user_pin() {
        let mut card = Card::new(FakeCard::new().with_decryption_key([7u8; 32])).unwrap();
        let mut tx = card.transaction().unwrap();
        let err = tx
            .card()
            .decipher(openpgp_card::ocard::crypto::Cryptogram::ECDH(&[1u8; 32]))
            .unwrap_err();
        assert!(err.to_string().contains("ecurity status"), "{err}");
    }
```

- [ ] **Step 3: Run them and see them fail.**
  - Run: `cargo test the_decryption_slot decipher_needs`
  - Expected: they don't compile, because `with_decryption_key` doesn't exist.

- [ ] **Step 4: Implement it.** In `testing.rs`:

```rust
/// PSO P1/P2 for "DECIPHER" (`ocard/commands.rs::decryption`).
const PSO_DECIPHER: (u8, u8) = (0x80, 0x86);
/// Algorithm id for ECDH (`ocard/data/algo_attrs.rs`); Cv25519 follows as an OID.
const ALGO_ID_ECDH: u8 = 0x12;
/// Cv25519 ECDH attribute bytes, as `set_algorithm(Decryption, Curve25519)` writes them.
const ALGO_ATTRS_CV25519: [u8; 11] =
    [0x12, 0x2B, 0x06, 0x01, 0x04, 0x01, 0x97, 0x55, 0x01, 0x05, 0x01];
```

Add these fields to `Inner`, initialised in `FakeCard::new`:
- `algo_dec: Vec<u8>`, starting as `ALGO_ATTRS_RSA2048.to_vec()`;
- `dec_secret: Option<[u8; 32]>`, starting as `None`;
- `dec_rsa: bool`, starting as `false`;
- `uif_dec: [u8; 2]`, starting as `[0x00, 0x20]`;
- `verified_pw1_user: bool`, starting as `false`;
- `last_deciphered: Vec<u8>`, starting empty.

Reset `verified_pw1_user = false` in `CardBackend::transaction`, next to the other two flags.

Change `build_application_related_data` to take `algo_dec: &[u8]` and `uif_dec: [u8; 2]`. Emit `tlv(&[0xc2], algo_dec)` and `tlv(&[0xd7], &uif_dec)` in place of the fixed values, and pass `&card.algo_dec, card.uif_dec` from the GET DATA arm.

Add the public key helper and the builders:

```rust
fn x25519_public(secret: &[u8; 32]) -> [u8; 32] {
    *x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(*secret)).as_bytes()
}

impl FakeCard {
    /// The decryption slot holds this X25519 key (a card with an existing key).
    pub fn with_decryption_key(self, secret: [u8; 32]) -> Self {
        {
            let mut inner = self.inner.lock().unwrap();
            inner.algo_dec = ALGO_ATTRS_CV25519.to_vec();
            inner.dec_secret = Some(secret);
        }
        self
    }
    /// The decryption slot holds an RSA key (not usable for card unlock).
    pub fn with_rsa_decryption_key(self) -> Self {
        self.inner.lock().unwrap().dec_rsa = true;
        self
    }
    pub fn decryption_key_generated(&self) -> bool {
        self.inner.lock().unwrap().dec_secret.is_some()
    }
    pub fn decryption_public(&self) -> Option<[u8; 32]> {
        self.inner.lock().unwrap().dec_secret.as_ref().map(x25519_public)
    }
    pub fn last_deciphered(&self) -> Vec<u8> {
        self.inner.lock().unwrap().last_deciphered.clone()
    }
}
```

In the `GENERATE_ASYMMETRIC_KEY_PAIR` arm, branch on the CRT byte before the existing code. The existing code becomes the `B6` path:

```rust
                // CRT: B6 signature, B8 decryption, A4 authentication.
                let crt = cmd.get(5).copied().unwrap_or(0xB6);
                if crt == 0xB8 {
                    return match p1 {
                        0x80 => {
                            if !card.verified_pw3_admin {
                                return Ok(vec![0x69, 0x82]);
                            }
                            if card.algo_dec.first() != Some(&ALGO_ID_ECDH) {
                                card.dec_rsa = true;
                                let mut v = tlv(&[0x81], &FAKE_RSA_MODULUS);
                                v.extend(tlv(&[0x82], &FAKE_RSA_EXPONENT));
                                return Ok(ok(tlv(&[0x7f, 0x49], &v)));
                            }
                            let secret: [u8; 32] = rand_secret();
                            card.dec_secret = Some(secret);
                            Ok(ok(tlv(&[0x7f, 0x49], &tlv(&[0x86], &x25519_public(&secret)))))
                        }
                        0x81 => match (&card.dec_secret, card.dec_rsa) {
                            (Some(s), _) => Ok(ok(tlv(&[0x7f, 0x49], &tlv(&[0x86], &x25519_public(s))))),
                            (None, true) => {
                                let mut v = tlv(&[0x81], &FAKE_RSA_MODULUS);
                                v.extend(tlv(&[0x82], &FAKE_RSA_EXPONENT));
                                Ok(ok(tlv(&[0x7f, 0x49], &v)))
                            }
                            (None, false) => Ok(vec![0x6A, 0x88]),
                        },
                        _ => Ok(vec![0x6A, 0x88]),
                    };
                }
```

with this helper, deterministic per call so it needs no RNG dependency:

```rust
/// A fresh-looking secret for a generated decryption key. Not random;
/// only distinct per call, which is all a fake needs.
fn rand_secret() -> [u8; 32] {
    static N: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(1);
    [N.fetch_add(1, std::sync::atomic::Ordering::Relaxed); 32]
}
```

The algorithm for an RSA slot needs care: when `dec_rsa` is set by `with_rsa_decryption_key`, the `C2` attribute should still read RSA. That's already the default, so nothing more is needed.

In the `PUT_DATA` arm, after the existing `C1` block:

```rust
                if (p1, p2) == (0x00, 0xc2) {
                    if let Some(&lc) = cmd.get(4) {
                        if let Some(bytes) = cmd.get(5..5 + lc as usize) {
                            card.algo_dec = bytes.to_vec();
                        }
                    }
                }
                if (p1, p2) == (0x00, 0xd7) {
                    if let Some(bytes) = cmd.get(5..)
                        && bytes.len() >= 2
                    {
                        card.uif_dec = [bytes[0], bytes[1]];
                    }
                }
```

In the `VERIFY` arm's `match p2` that records verification, add `VERIFY_P2_USER => card.verified_pw1_user = verified,`.

Add the PSO:DECIPHER arm before `_ =>`:

```rust
            ins::PSO if (p1, p2) == PSO_DECIPHER => {
                if !card.verified_pw1_user {
                    return Ok(vec![0x69, 0x82]); // SecurityStatusNotSatisfied
                }
                if card.touch_timeout {
                    return Err(SmartcardError::Error("Transmit failed: Timeout".to_string()));
                }
                let lc = cmd.get(4).copied().unwrap_or(0) as usize;
                let data = cmd.get(5..5 + lc).unwrap_or(&[]).to_vec();
                card.last_deciphered = data.clone();
                let Some(secret) = card.dec_secret else {
                    return Ok(vec![0x6A, 0x88]);
                };
                // Cipher DO A6 { 7F49 { 86 <32-byte point> } }: the point is the tail.
                let Some(eph) = data.len().checked_sub(32).map(|i| &data[i..]) else {
                    return Ok(vec![0x6A, 0x80]); // IncorrectParametersCommandDataField
                };
                let eph: [u8; 32] = eph.try_into().unwrap();
                let shared = x25519_dalek::StaticSecret::from(secret)
                    .diffie_hellman(&x25519_dalek::PublicKey::from(eph));
                Ok(ok(shared.as_bytes().to_vec()))
            }
```

- [ ] **Step 5: Run the tests.**
  - Run: `cargo test` and `cargo test --features test-util`
  - Expected: everything passes, including the existing signature-slot tests.

- [ ] **Step 6: Commit.**

```bash
git add Cargo.toml Cargo.lock src/testing.rs
git commit -s -m "test: FakeCard decryption slot with real X25519 ECDH

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

### Task 2: `decrypt` module — public key, generate, ECDH

**Files:**
- Create: `aivyx-yubi/src/decrypt.rs`
- Modify: `aivyx-yubi/src/lib.rs` (add `pub mod decrypt;` and two error variants)
- Modify: `aivyx-yubi/src/provision.rs` (`key_slot_fingerprint` becomes `pub(crate)`)
- Modify: `aivyx-yubi/src/sign.rs` (`map_sign_error` becomes `pub(crate)`)
- Modify: `aivyx-yubi/README.md` (a "Decryption slot" section)

**Interfaces:**
- Produces:

```rust
pub fn decryption_public_key(tx: &mut Card<Transaction<'_>>) -> Result<Option<PublicKeyBytes>, YubiError>;
pub fn generate_decryption_key(admin: &mut Card<Admin<'_, '_>>) -> Result<PublicKeyBytes, YubiError>;
pub fn generate_decryption_key_overwriting(admin: &mut Card<Admin<'_, '_>>) -> Result<PublicKeyBytes, YubiError>;
pub struct SharedSecret(/* [u8; 32], zeroized on drop */);
impl SharedSecret { pub fn as_bytes(&self) -> &[u8; 32]; }
pub fn ecdh(tx: &mut Card<Transaction<'_>>, pin: SecretString, their_public: &[u8; 32]) -> Result<SharedSecret, YubiError>;
// New YubiError variants:
YubiError::DecryptionSlotOccupied
YubiError::UnsupportedDecryptionKey(String)
```

- [ ] **Step 1: Write the failing tests** in `src/decrypt.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeCard;
    use openpgp_card::Card;
    use x25519_dalek::{PublicKey, StaticSecret};

    fn pin() -> SecretString {
        SecretString::from("123456")
    }

    #[test]
    fn an_empty_slot_has_no_public_key() {
        let mut card = Card::new(FakeCard::new()).unwrap();
        let mut tx = card.transaction().unwrap();
        assert!(decryption_public_key(&mut tx).unwrap().is_none());
    }

    #[test]
    fn an_existing_x25519_key_is_read() {
        let fake = FakeCard::new().with_decryption_key([5u8; 32]);
        let want = fake.decryption_public().unwrap();
        let mut card = Card::new(fake).unwrap();
        let mut tx = card.transaction().unwrap();
        assert_eq!(<[u8; 32]>::from(decryption_public_key(&mut tx).unwrap().unwrap()), want);
    }

    #[test]
    fn an_rsa_key_is_unsupported() {
        let mut card = Card::new(FakeCard::new().with_rsa_decryption_key()).unwrap();
        let mut tx = card.transaction().unwrap();
        assert!(matches!(
            decryption_public_key(&mut tx),
            Err(YubiError::UnsupportedDecryptionKey(_))
        ));
    }

    #[test]
    fn generate_makes_an_x25519_key_with_touch_fixed_and_refuses_a_second() {
        use openpgp_card::ocard::data::TouchPolicy;
        let fake = FakeCard::new();
        let probe = fake.clone();
        let mut card = Card::new(fake).unwrap();
        let mut tx = card.transaction().unwrap();
        let mut admin = tx
            .to_admin_card(SecretString::from("12345678"))
            .unwrap();
        let public = generate_decryption_key(&mut admin).unwrap();
        assert_eq!(Some(<[u8; 32]>::from(public)), probe.decryption_public());
        assert!(matches!(
            generate_decryption_key(&mut admin),
            Err(YubiError::DecryptionSlotOccupied)
        ));
        let uif = admin
            .as_transaction()
            .user_interaction_flag(openpgp_card::ocard::KeyType::Decryption)
            .unwrap()
            .unwrap();
        assert_eq!(uif.touch_policy(), TouchPolicy::Fixed);
    }

    #[test]
    fn ecdh_matches_the_math_and_a_wrong_pin_is_refused() {
        let fake = FakeCard::new().with_decryption_key([5u8; 32]);
        let card_pub = fake.decryption_public().unwrap();
        let mut card = Card::new(fake).unwrap();
        let mut tx = card.transaction().unwrap();
        let eph = StaticSecret::from([8u8; 32]);
        let eph_pub = *PublicKey::from(&eph).as_bytes();
        let shared = ecdh(&mut tx, pin(), &eph_pub).unwrap();
        assert_eq!(shared.as_bytes(), eph.diffie_hellman(&PublicKey::from(card_pub)).as_bytes());
        assert!(matches!(
            ecdh(&mut tx, SecretString::from("000000"), &eph_pub),
            Err(YubiError::PinIncorrect)
        ));
        assert!(matches!(ecdh(&mut tx, SecretString::from(""), &eph_pub), Err(YubiError::EmptyPin)));
    }

    #[test]
    fn ecdh_without_a_key_says_so_and_a_touch_timeout_maps() {
        let mut card = Card::new(FakeCard::new()).unwrap();
        let mut tx = card.transaction().unwrap();
        assert!(matches!(
            ecdh(&mut tx, pin(), &[1u8; 32]),
            Err(YubiError::UnsupportedDecryptionKey(_))
        ));
        let mut card =
            Card::new(FakeCard::new().with_decryption_key([5u8; 32]).with_touch_timeout()).unwrap();
        let mut tx = card.transaction().unwrap();
        assert!(matches!(ecdh(&mut tx, pin(), &[1u8; 32]), Err(YubiError::TouchTimeout)));
    }
}
```

Check the exact name of the admin transition in `provision.rs`'s test helper `admin_authenticated_card()`, and use whatever it calls. If it isn't `to_admin_card`, copy that helper's body.

- [ ] **Step 2: Run them and see them fail.**
  - Run: `cargo test decrypt`
  - Expected: compile errors, because the module doesn't exist yet.

- [ ] **Step 3: Implement it.** In `lib.rs`, add `pub mod decrypt;` after `pub mod discovery;`, and these variants to `YubiError`:

```rust
    /// The decryption slot already holds a key. `generate_decryption_key`
    /// refuses rather than destroy it (many owners keep a GPG encryption key there).
    #[error("the card's decryption slot already holds a key")]
    DecryptionSlotOccupied,

    /// The decryption slot holds no key, or a key that isn't X25519 (Cv25519 ECDH).
    #[error("{0}")]
    UnsupportedDecryptionKey(String),
```

`src/decrypt.rs` (above the tests):

```rust
//! The OpenPGP card's decryption slot as an X25519 key-agreement device:
//! read its public key, generate one on the card (touch fixed), and run
//! ECDH (PSO:DECIPHER) after a User PIN check (PW1 mode 82). Used by
//! aivyx-pa's card unlock; knows nothing about it.

use openpgp_card::{
    Card,
    ocard::{
        KeyType, StatusBytes,
        algorithm::{AlgoSimple, AlgorithmAttributes, Curve},
        crypto::{Cryptogram, EccType, PublicKeyMaterial},
        data::TouchPolicy,
    },
    state::{Admin, Transaction},
};
use secrecy::{ExposeSecret, SecretString};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::{YubiError, provision::PublicKeyBytes};

/// A 32-byte X25519 shared secret, zeroed on drop.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct SharedSecret([u8; 32]);

impl SharedSecret {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl std::fmt::Debug for SharedSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SharedSecret(<redacted>)")
    }
}

fn x25519_point(material: PublicKeyMaterial) -> Result<PublicKeyBytes, YubiError> {
    match material {
        PublicKeyMaterial::E(ecc) => {
            let is_x25519 = matches!(
                ecc.algo(),
                AlgorithmAttributes::Ecc(a) if a.ecc_type() == EccType::ECDH && *a.curve() == Curve::Curve25519
            );
            if !is_x25519 {
                return Err(YubiError::UnsupportedDecryptionKey(format!(
                    "the decryption slot holds a {:?} key, not X25519",
                    ecc.algo()
                )));
            }
            let bytes: [u8; 32] = ecc.data().try_into().map_err(|_| {
                YubiError::UnsupportedDecryptionKey(format!(
                    "the decryption key is {} bytes, expected 32 (X25519)",
                    ecc.data().len()
                ))
            })?;
            Ok(PublicKeyBytes::new(bytes))
        }
        PublicKeyMaterial::R(_) => Err(YubiError::UnsupportedDecryptionKey(
            "the decryption slot holds an RSA key, not X25519".into(),
        )),
    }
}

/// The decryption slot's X25519 public key; `None` when the slot is empty.
pub fn decryption_public_key(
    tx: &mut Card<Transaction<'_>>,
) -> Result<Option<PublicKeyBytes>, YubiError> {
    match tx.public_key_material(KeyType::Decryption) {
        Ok(m) => x25519_point(m).map(Some),
        Err(openpgp_card::Error::CardStatus(StatusBytes::ReferencedDataNotFound)) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Generate an X25519 key on the card, refusing if the slot holds one.
pub fn generate_decryption_key(
    admin: &mut Card<Admin<'_, '_>>,
) -> Result<PublicKeyBytes, YubiError> {
    match admin.as_transaction().public_key_material(KeyType::Decryption) {
        Ok(_) => return Err(YubiError::DecryptionSlotOccupied),
        Err(openpgp_card::Error::CardStatus(StatusBytes::ReferencedDataNotFound)) => {}
        Err(e) => return Err(e.into()),
    }
    generate_decryption_key_overwriting(admin)
}

/// [`generate_decryption_key`] without the check: destroys any key there.
pub fn generate_decryption_key_overwriting(
    admin: &mut Card<Admin<'_, '_>>,
) -> Result<PublicKeyBytes, YubiError> {
    admin
        .set_algorithm(KeyType::Decryption, AlgoSimple::Curve25519)
        .map_err(crate::provision::map_admin_op_error)?;
    let (material, _) = admin
        .generate_key(crate::provision::key_slot_fingerprint, KeyType::Decryption)
        .map_err(crate::provision::map_admin_op_error)?;
    let public = x25519_point(material)?;
    admin
        .set_touch_policy(KeyType::Decryption, TouchPolicy::Fixed)
        .map_err(crate::provision::map_admin_op_error)?;
    Ok(public)
}

/// X25519 between the card's decryption key and `their_public`, on the
/// card: verifies the User PIN (mode 82), then PSO:DECIPHER, which waits
/// for a touch when the slot's touch policy is on.
pub fn ecdh(
    tx: &mut Card<Transaction<'_>>,
    pin: SecretString,
    their_public: &[u8; 32],
) -> Result<SharedSecret, YubiError> {
    if pin.expose_secret().is_empty() {
        return Err(YubiError::EmptyPin);
    }
    if decryption_public_key(tx)?.is_none() {
        return Err(YubiError::UnsupportedDecryptionKey(
            "the card's decryption slot is empty".into(),
        ));
    }
    tx.verify_user_pin(pin).map_err(|e| match e {
        openpgp_card::Error::CardStatus(StatusBytes::AuthenticationMethodBlocked) => {
            YubiError::PinBlocked {
                pin_kind: "User",
                recovery_hint: "Unblock it with the Admin PIN (RESET RETRY COUNTER).",
            }
        }
        other => other.into(),
    })?;
    let out = tx
        .card()
        .decipher(Cryptogram::ECDH(their_public))
        .map_err(crate::sign::map_sign_error)?;
    let bytes: [u8; 32] = out.as_slice().try_into().map_err(|_| {
        YubiError::Other(format!("the card returned {} bytes for ECDH, expected 32", out.len()))
    })?;
    Ok(SharedSecret(bytes))
}
```

In `provision.rs`, make `key_slot_fingerprint` and `map_admin_op_error` `pub(crate)`. In `sign.rs`, make `map_sign_error` `pub(crate)`. If `recovery_hint` is a `String` rather than `&'static str`, add `.into()`.

- [ ] **Step 4: Run the tests and clippy.**
  - Run: `cargo test && cargo test --features test-util && cargo clippy --all-targets --all-features -- -D warnings`
  - Expected: all pass, clippy is clean.

- [ ] **Step 5: Update the README** with a "Decryption slot (X25519 ECDH)" section. It says what the three calls do, that `generate_decryption_key` never overwrites (only `_overwriting` does), that `ecdh` needs the User PIN and a touch, and that the crate never exports the secret key.

- [ ] **Step 6: Commit and push. Record the revision for Part B.**

```bash
git add -A
git commit -s -m "feat: decryption slot — X25519 public key, on-card generation, ECDH

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
git push
git rev-parse HEAD   # YUBI_REV for Task 4
```

---

## Part B — aivyx-pa

### Task 3: Wrapping the master key (aivyx-crypto)

**Files:**
- Modify: `crates/aivyx-crypto/src/lib.rs`

**Interfaces:**
- Produces:

```rust
pub const CARD_WRAP_SALT: &[u8] = b"aivyx-pa-card-unlock-v1";
pub struct WrapKey { /* [u8; 32], ZeroizeOnDrop */ }
impl WrapKey { pub fn derive(shared_secret: &[u8; 32], info: &[u8]) -> Result<Self, CryptoError>; }
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrappedMasterKey { pub nonce: [u8; NONCE_LEN], pub ciphertext: Vec<u8> }
impl MasterKey {
    pub fn wrap(&self, key: &WrapKey, aad: &[u8]) -> Result<WrappedMasterKey, CryptoError>;
    pub fn unwrap(wrapped: &WrappedMasterKey, key: &WrapKey, aad: &[u8]) -> Result<MasterKey, CryptoError>;
    /// Equality without exposing bytes (constant time).
    pub fn same_as(&self, other: &MasterKey) -> bool;
}
```

- [ ] **Step 1: Write the failing tests** in the crate's `mod tests`:

```rust
    #[test]
    fn a_wrapped_master_key_round_trips_and_anything_changed_is_refused() {
        let master = MasterKey::from_raw([3u8; 32]);
        let key = WrapKey::derive(&[4u8; 32], b"info").unwrap();
        let w = master.wrap(&key, b"aad").unwrap();
        assert!(MasterKey::unwrap(&w, &key, b"aad").unwrap().same_as(&master));

        let mut bad = w.clone();
        bad.ciphertext[0] ^= 1;
        assert!(matches!(MasterKey::unwrap(&bad, &key, b"aad"), Err(CryptoError::AeadOpenFailed)));
        let mut bad = w.clone();
        bad.nonce[0] ^= 1;
        assert!(MasterKey::unwrap(&bad, &key, b"aad").is_err());
        assert!(MasterKey::unwrap(&w, &key, b"other").is_err());
        let other = WrapKey::derive(&[4u8; 32], b"other info").unwrap();
        assert!(MasterKey::unwrap(&w, &other, b"aad").is_err());
    }

    #[test]
    fn two_wraps_use_different_nonces() {
        let master = MasterKey::from_raw([3u8; 32]);
        let key = WrapKey::derive(&[4u8; 32], b"info").unwrap();
        assert_ne!(master.wrap(&key, b"a").unwrap().nonce, master.wrap(&key, b"a").unwrap().nonce);
    }
```

- [ ] **Step 2: Run them and see them fail.**
  - Run: `cargo test -p aivyx-crypto wrapped`
  - Expected: compile errors, because `WrapKey` doesn't exist.

- [ ] **Step 3: Implement it.** Below `derive_master_key`:

```rust
/// HKDF salt for card-unlock wrap keys (Amendment A18).
pub const CARD_WRAP_SALT: &[u8] = b"aivyx-pa-card-unlock-v1";

/// A key that wraps the master key for one enrolled card. Built only by
/// HKDF from a card's X25519 shared secret; zeroized on drop.
#[derive(ZeroizeOnDrop)]
pub struct WrapKey {
    bytes: [u8; KEY_LEN],
}

impl WrapKey {
    pub fn derive(shared_secret: &[u8; 32], info: &[u8]) -> Result<Self, CryptoError> {
        let hk = Hkdf::<Sha256>::new(Some(CARD_WRAP_SALT), shared_secret);
        let mut bytes = [0u8; KEY_LEN];
        hk.expand(info, &mut bytes)
            .map_err(|e| CryptoError::HkdfExpandFailed(e.to_string()))?;
        Ok(Self { bytes })
    }
}

/// The master key encrypted under a [`WrapKey`] (ChaCha20-Poly1305).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrappedMasterKey {
    pub nonce: [u8; NONCE_LEN],
    pub ciphertext: Vec<u8>,
}

impl MasterKey {
    pub fn wrap(&self, key: &WrapKey, aad: &[u8]) -> Result<WrappedMasterKey, CryptoError> {
        use chacha20poly1305::aead::{AeadCore, OsRng};
        let cipher = ChaCha20Poly1305::new(AeadKey::from_slice(&key.bytes));
        let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
        let ciphertext = cipher
            .encrypt(&nonce, Payload { msg: &self.bytes, aad })
            .map_err(|e| CryptoError::AeadSealFailed(e.to_string()))?;
        Ok(WrappedMasterKey { nonce: nonce.into(), ciphertext })
    }

    pub fn unwrap(
        wrapped: &WrappedMasterKey,
        key: &WrapKey,
        aad: &[u8],
    ) -> Result<MasterKey, CryptoError> {
        let cipher = ChaCha20Poly1305::new(AeadKey::from_slice(&key.bytes));
        let mut plain = cipher
            .decrypt(Nonce::from_slice(&wrapped.nonce), Payload { msg: &wrapped.ciphertext, aad })
            .map_err(|_| CryptoError::AeadOpenFailed)?;
        let result = <[u8; KEY_LEN]>::try_from(plain.as_slice())
            .map(|bytes| MasterKey { bytes })
            .map_err(|_| CryptoError::InvalidKeyLength(plain.len()));
        plain.zeroize();
        result
    }

    pub fn same_as(&self, other: &MasterKey) -> bool {
        self.bytes.iter().zip(other.bytes.iter()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
    }
}
```

If `NONCE_LEN` isn't a `pub const` already, add `pub const NONCE_LEN: usize = 12;` beside `KEY_LEN`. If `chacha20poly1305`'s `getrandom` feature is off, enable it in `crates/aivyx-crypto/Cargo.toml` (`chacha20poly1305 = { version = "0.10", features = ["getrandom"] }`).

- [ ] **Step 4: Run the tests.**
  - Run: `cargo test -p aivyx-crypto && cargo clippy -p aivyx-crypto --all-targets -- -D warnings`
  - Expected: they pass.

- [ ] **Step 5: Commit.**

```bash
git add crates/aivyx-crypto
git commit -s -m "feat(crypto): wrap the master key under a card-derived key (A18)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

### Task 4: The helper's protocol and client (`aivyx-card-helper`, no PC/SC)

**Files:**
- Create: `crates/aivyx-card-helper/Cargo.toml`
- Create: `crates/aivyx-card-helper/src/lib.rs` (protocol types and `CardHelper` trait)
- Create: `crates/aivyx-card-helper/src/client.rs` (`ProcessHelper`: runs the binary)
- Create: `crates/aivyx-card-helper/src/fake.rs` (`FakeHelper`, real X25519 math, `#[cfg(any(test, feature = "test-util"))]`)
- Modify: the root `Cargo.toml` (`[workspace] members` if it isn't a glob; `[workspace.dependencies] aivyx-card-helper = { path = "crates/aivyx-card-helper" }`; `x25519-dalek = { version = "2", features = ["static_secrets"] }`)

**Interfaces:**
- Produces:

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    List,
    PublicKey { serial: String },
    Generate { serial: String, admin_pin: String },
    Ecdh { serial: String, pin: String, their_public: String /* base64 */ },
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Reply {
    Cards { cards: Vec<CardInfo> },
    PublicKey { public: Option<String> },
    Shared { secret: String },
    Error { kind: ErrorKind, message: String, tries_left: Option<u8> },
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CardInfo { pub serial: String, pub has_decryption_key: bool }
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind { NoReader, CardNotFound, WrongPin, PinBlocked, TouchTimeout, SlotOccupied, UnsupportedKey, Other }
#[derive(Debug, thiserror::Error)]
pub enum HelperError {
    #[error("card unlock needs the `aivyx-pa-card` helper: run `aivyx-pa tool-programs install`")]
    NotInstalled,
    #[error("{message}")]
    Card { kind: ErrorKind, message: String, tries_left: Option<u8> },
    #[error("the card helper failed: {0}")]
    Broken(String),
}
pub trait CardHelper: Send + Sync {
    fn list(&self) -> Result<Vec<CardInfo>, HelperError>;
    fn public_key(&self, serial: &str) -> Result<Option<[u8; 32]>, HelperError>;
    fn generate(&self, serial: &str, admin_pin: &SecretString) -> Result<[u8; 32], HelperError>;
    fn ecdh(&self, serial: &str, pin: &SecretString, their_public: &[u8; 32]) -> Result<Zeroizing<[u8; 32]>, HelperError>;
}
pub struct ProcessHelper { /* path */ }
impl ProcessHelper { pub fn find() -> Result<Self, HelperError>; pub fn at(path: PathBuf) -> Self; }
pub struct FakeHelper { /* behind test-util */ }
impl FakeHelper {
    pub fn with_card(serial: &str, secret: [u8; 32]) -> Self;
    pub fn empty_card(serial: &str) -> Self;
    pub fn pin(self, pin: &str) -> Self;   // the PIN it accepts; default "123456"
    pub fn unplugged(&self, serial: &str); // simulate unplugging
    pub fn calls(&self) -> usize;          // how many ecdh calls it saw
}
```

- [ ] **Step 1: Make the crate.** `Cargo.toml`:

```toml
[package]
name = "aivyx-card-helper"
version.workspace = true
edition = "2024"
license = "BUSL-1.1"
publish = false
description = "The aivyx-pa-card helper: OpenPGP-card ECDH for card unlock (A18)"

[[bin]]
name = "aivyx-pa-card"
path = "src/main.rs"
required-features = ["card"]

[dependencies]
serde = { workspace = true, features = ["derive"] }
serde_json = { workspace = true }
base64 = { workspace = true }
secrecy = { workspace = true }
zeroize = "1.8"
thiserror = { workspace = true }
x25519-dalek = { workspace = true, optional = true }
aivyx-yubi = { git = "https://github.com/Aivyx-Agent/aivyx-yubi", rev = "<YUBI_REV>", optional = true }

[features]
card = ["dep:aivyx-yubi"]
test-util = ["dep:x25519-dalek"]

[dev-dependencies]
x25519-dalek = { workspace = true }
aivyx-yubi = { git = "https://github.com/Aivyx-Agent/aivyx-yubi", rev = "<YUBI_REV>", features = ["test-util"] }
```

Use the root manifest's existing aivyx-yubi entry style, and replace `<YUBI_REV>` with the revision from Task 2, Step 6. If the root already pins aivyx-yubi in `[workspace.dependencies]`, bump that pin to the new revision and use `aivyx-yubi = { workspace = true, optional = true }` here. Check whether `version.workspace` and `thiserror`/`base64`/`secrecy` are workspace dependencies (`grep -n "^thiserror\|^base64\|^secrecy\|^\[workspace.package\]" Cargo.toml`), and fall back to explicit versions matching the other crates where they aren't.

The dev-dependency on aivyx-yubi pulls in PC/SC for tests. CI already builds the `yubikey` feature (check `.github/workflows/ci.yml` installs `libpcsclite-dev`); if it doesn't, add `sudo apt-get install -y libpcsclite-dev` to the test job in this task.

- [ ] **Step 2: Write the failing tests** in `src/lib.rs` (wire format) and `src/fake.rs` (behaviour):

```rust
// lib.rs
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn the_wire_format_is_stable() {
        let r = Request::Ecdh { serial: "0006:00112233".into(), pin: "123456".into(), their_public: "AA==".into() };
        assert_eq!(
            serde_json::to_string(&r).unwrap(),
            r#"{"op":"ecdh","serial":"0006:00112233","pin":"123456","their_public":"AA=="}"#
        );
        let e: Reply = serde_json::from_str(
            r#"{"result":"error","kind":"wrong_pin","message":"PIN incorrect","tries_left":2}"#,
        ).unwrap();
        assert_eq!(e, Reply::Error { kind: ErrorKind::WrongPin, message: "PIN incorrect".into(), tries_left: Some(2) });
    }
}

// fake.rs
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
        let s = h.ecdh("S1", &SecretString::from("123456"), &eph_pub).unwrap();
        assert_eq!(*s, *eph.diffie_hellman(&PublicKey::from(card_pub)).as_bytes());
        let err = h.ecdh("S1", &SecretString::from("000000"), &eph_pub).unwrap_err();
        assert!(matches!(err, HelperError::Card { kind: ErrorKind::WrongPin, tries_left: Some(2), .. }));
        h.unplugged("S1");
        assert!(h.list().unwrap().is_empty());
    }
}
```

- [ ] **Step 3: Run them and see them fail.**
  - Run: `cargo test -p aivyx-card-helper --features test-util`
  - Expected: compile errors.

- [ ] **Step 4: Implement it.** `src/lib.rs` holds the types from Interfaces, plus:

```rust
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

impl From<Reply> for HelperError {
    fn from(r: Reply) -> Self {
        match r {
            Reply::Error { kind, message, tries_left } => HelperError::Card { kind, message, tries_left },
            other => HelperError::Broken(format!("unexpected reply {other:?}")),
        }
    }
}
```

`src/client.rs`:

```rust
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use secrecy::{ExposeSecret, SecretString};
use zeroize::Zeroizing;

use crate::{CardHelper, CardInfo, HelperError, Reply, Request, decode_key, encode_key};

pub const HELPER_NAME: &str = "aivyx-pa-card";

/// Runs `aivyx-pa-card` once per call: the request goes on stdin (never
/// argv, so a PIN can't show in `ps`), the reply comes back on stdout.
pub struct ProcessHelper {
    path: PathBuf,
}

impl ProcessHelper {
    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    /// Next to the running executable first, then on `PATH`.
    pub fn find() -> Result<Self, HelperError> {
        if let Ok(exe) = std::env::current_exe()
            && let Some(dir) = exe.parent()
        {
            let p = dir.join(HELPER_NAME);
            if p.is_file() {
                return Ok(Self::at(p));
            }
        }
        std::env::var_os("PATH")
            .and_then(|paths| {
                std::env::split_paths(&paths).map(|d| d.join(HELPER_NAME)).find(|p| p.is_file())
            })
            .map(Self::at)
            .ok_or(HelperError::NotInstalled)
    }

    fn call(&self, req: &Request) -> Result<Reply, HelperError> {
        let mut child = Command::new(&self.path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .spawn()
            .map_err(|e| HelperError::Broken(format!("start {}: {e}", self.path.display())))?;
        let body = Zeroizing::new(serde_json::to_vec(req).map_err(|e| HelperError::Broken(e.to_string()))?);
        child
            .stdin
            .take()
            .ok_or_else(|| HelperError::Broken("no stdin".into()))?
            .write_all(&body)
            .map_err(|e| HelperError::Broken(e.to_string()))?;
        let out = child.wait_with_output().map_err(|e| HelperError::Broken(e.to_string()))?;
        let stdout = Zeroizing::new(out.stdout);
        serde_json::from_slice(&stdout)
            .map_err(|e| HelperError::Broken(format!("bad reply (exit {}): {e}", out.status)))
    }
}

impl CardHelper for ProcessHelper {
    fn list(&self) -> Result<Vec<CardInfo>, HelperError> {
        match self.call(&Request::List)? {
            Reply::Cards { cards } => Ok(cards),
            r => Err(r.into()),
        }
    }
    fn public_key(&self, serial: &str) -> Result<Option<[u8; 32]>, HelperError> {
        match self.call(&Request::PublicKey { serial: serial.into() })? {
            Reply::PublicKey { public: None } => Ok(None),
            Reply::PublicKey { public: Some(p) } => {
                decode_key(&p).map(Some).ok_or_else(|| HelperError::Broken("bad public key".into()))
            }
            r => Err(r.into()),
        }
    }
    fn generate(&self, serial: &str, admin_pin: &SecretString) -> Result<[u8; 32], HelperError> {
        let req = Request::Generate { serial: serial.into(), admin_pin: admin_pin.expose_secret().to_string() };
        match self.call(&req)? {
            Reply::PublicKey { public: Some(p) } => {
                decode_key(&p).ok_or_else(|| HelperError::Broken("bad public key".into()))
            }
            r => Err(r.into()),
        }
    }
    fn ecdh(&self, serial: &str, pin: &SecretString, their_public: &[u8; 32]) -> Result<Zeroizing<[u8; 32]>, HelperError> {
        let req = Request::Ecdh {
            serial: serial.into(),
            pin: pin.expose_secret().to_string(),
            their_public: encode_key(their_public),
        };
        match self.call(&req)? {
            Reply::Shared { secret } => decode_key(&secret)
                .map(Zeroizing::new)
                .ok_or_else(|| HelperError::Broken("bad shared secret".into())),
            r => Err(r.into()),
        }
    }
}
```

The `Request` that holds the PIN is dropped right after `call`. `String` isn't zeroized, which is acceptable here because the helper process exits right after; note it in a comment.

`src/fake.rs` keeps a `Mutex<HashMap<String, FakeCardState { secret: Option<[u8; 32]>, pin: String, tries: u8, present: bool }>>` plus an `AtomicUsize` call counter:
- `ecdh` checks that the card is present (else `CardNotFound`), that it has a key (else `UnsupportedKey`), and that the PIN is right. A wrong PIN takes one try off and returns `WrongPin` with `tries_left: Some(n)`; at 0 it returns `PinBlocked`. On success it returns `StaticSecret::from(secret).diffie_hellman(&PublicKey::from(*their_public))`.
- `generate` refuses with `SlotOccupied` when a key exists; otherwise it sets the secret to `[0x42; 32]` and returns that key's public half.
- `list` returns the present cards.

- [ ] **Step 5: Run the tests.**
  - Run: `cargo test -p aivyx-card-helper --features test-util && cargo clippy -p aivyx-card-helper --all-targets --features test-util -- -D warnings`
  - Expected: they pass.

- [ ] **Step 6: Commit.**

```bash
git add Cargo.toml Cargo.lock crates/aivyx-card-helper
git commit -s -m "feat: aivyx-card-helper protocol, process client and fake (A18)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

### Task 5: The `aivyx-pa-card` binary

**Files:**
- Create: `crates/aivyx-card-helper/src/serve.rs` (`#[cfg(feature = "card")]`)
- Create: `crates/aivyx-card-helper/src/main.rs`

**Interfaces:**
- Consumes: `aivyx_yubi::{discovery, decrypt, pin}` (Task 2).
- Produces: `pub fn handle(req: Request, open: &dyn Fn(Option<&str>) -> Result<Card<Open>, YubiError>, list: &dyn Fn() -> Result<Vec<CardSummary>, YubiError>) -> Reply`.

- [ ] **Step 1: Write the failing tests** in `serve.rs`, against FakeCard:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_yubi::testing::FakeCard;
    use openpgp_card::Card;

    fn opener(fake: FakeCard) -> impl Fn(Option<&str>) -> Result<Card<Open>, YubiError> {
        move |_| Card::new(fake.clone()).map_err(YubiError::from)
    }
    fn no_list() -> Result<Vec<aivyx_yubi::discovery::CardSummary>, YubiError> { Ok(vec![]) }

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
        let Reply::Shared { secret } = handle(req, &opener(fake), &no_list) else { panic!() };
        assert_eq!(crate::decode_key(&secret).unwrap(), *eph.diffie_hellman(&PublicKey::from(card_pub)).as_bytes());
    }

    #[test]
    fn a_wrong_pin_reports_the_tries_left() {
        let fake = FakeCard::new().with_decryption_key([5u8; 32]);
        let req = Request::Ecdh { serial: "x".into(), pin: "000000".into(), their_public: crate::encode_key(&[1u8; 32]) };
        assert!(matches!(
            handle(req, &opener(fake), &no_list),
            Reply::Error { kind: ErrorKind::WrongPin, tries_left: Some(2), .. }
        ));
    }

    #[test]
    fn generate_refuses_an_occupied_slot() {
        let fake = FakeCard::new().with_decryption_key([5u8; 32]);
        let req = Request::Generate { serial: "x".into(), admin_pin: "12345678".into() };
        assert!(matches!(handle(req, &opener(fake), &no_list), Reply::Error { kind: ErrorKind::SlotOccupied, .. }));
    }

    #[test]
    fn an_empty_slot_has_no_public_key() {
        let req = Request::PublicKey { serial: "x".into() };
        assert_eq!(handle(req, &opener(FakeCard::new()), &no_list), Reply::PublicKey { public: None });
    }
}
```

- [ ] **Step 2: Run them and see them fail.**
  - Run: `cargo test -p aivyx-card-helper --features card,test-util serve`
  - Expected: compile errors.

- [ ] **Step 3: Implement it.** `serve.rs`:

```rust
use aivyx_yubi::{YubiError, decrypt, discovery, pin, SecretString};
use openpgp_card::{Card, state::Open};

use crate::{CardInfo, ErrorKind, Reply, Request, decode_key, encode_key};

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
    Reply::Error { kind, message: e.to_string(), tries_left }
}

pub fn handle(
    req: Request,
    open: &dyn Fn(Option<&str>) -> Result<Card<Open>, YubiError>,
    list: &dyn Fn() -> Result<Vec<discovery::CardSummary>, YubiError>,
) -> Reply {
    let run = || -> Result<Reply, Reply> {
        match req {
            Request::List => {
                let cards = list().map_err(|e| error(&e, None))?;
                let mut out = Vec::new();
                for c in cards {
                    let mut card = open(Some(&c.serial)).map_err(|e| error(&e, None))?;
                    let mut tx = card.transaction().map_err(|e| error(&e.into(), None))?;
                    let has = matches!(decrypt::decryption_public_key(&mut tx), Ok(Some(_)));
                    out.push(CardInfo { serial: c.serial, has_decryption_key: has });
                }
                Ok(Reply::Cards { cards: out })
            }
            Request::PublicKey { serial } => {
                let mut card = open(Some(&serial)).map_err(|e| error(&e, None))?;
                let mut tx = card.transaction().map_err(|e| error(&e.into(), None))?;
                let public = decrypt::decryption_public_key(&mut tx).map_err(|e| error(&e, None))?;
                Ok(Reply::PublicKey { public: public.map(|p| encode_key(&p.into())) })
            }
            Request::Generate { serial, admin_pin } => {
                let mut card = open(Some(&serial)).map_err(|e| error(&e, None))?;
                let mut tx = card.transaction().map_err(|e| error(&e.into(), None))?;
                let mut admin = tx
                    .to_admin_card(SecretString::from(admin_pin))
                    .map_err(|e| error(&e.into(), None))?;
                let public = decrypt::generate_decryption_key(&mut admin).map_err(|e| error(&e, None))?;
                Ok(Reply::PublicKey { public: Some(encode_key(&public.into())) })
            }
            Request::Ecdh { serial, pin: user_pin, their_public } => {
                let theirs = decode_key(&their_public).ok_or_else(|| Reply::Error {
                    kind: ErrorKind::Other,
                    message: "their_public is not a 32-byte base64 key".into(),
                    tries_left: None,
                })?;
                let mut card = open(Some(&serial)).map_err(|e| error(&e, None))?;
                let mut tx = card.transaction().map_err(|e| error(&e.into(), None))?;
                match decrypt::ecdh(&mut tx, SecretString::from(user_pin), &theirs) {
                    Ok(s) => Ok(Reply::Shared { secret: encode_key(s.as_bytes()) }),
                    Err(e @ YubiError::PinIncorrect) => {
                        let left = pin::pin_retries(&mut tx).ok().map(|(pw1, _)| pw1);
                        Err(error(&e, left))
                    }
                    Err(e) => Err(error(&e, None)),
                }
            }
        }
    };
    run().unwrap_or_else(|r| r)
}
```

Use the admin transition name you found in Task 2, Step 1, in place of `to_admin_card`. Add `openpgp-card = "0.7"` (optional, in the `card` feature) to the helper's `Cargo.toml`, since `Card<Open>` appears in the signature.

`main.rs`:

```rust
//! `aivyx-pa-card`: one JSON request on stdin, one reply on stdout (A18).
use std::io::Read;

use aivyx_card_helper::{ErrorKind, Reply, Request, serve};

fn main() {
    let mut input = zeroize::Zeroizing::new(Vec::new());
    let reply = match std::io::stdin().take(64 * 1024).read_to_end(&mut input) {
        Err(e) => Reply::Error { kind: ErrorKind::Other, message: format!("read stdin: {e}"), tries_left: None },
        Ok(_) => match serde_json::from_slice::<Request>(&input) {
            Err(e) => Reply::Error { kind: ErrorKind::Other, message: format!("bad request: {e}"), tries_left: None },
            Ok(req) => serve::handle(
                req,
                &|serial| match serial {
                    Some(s) => aivyx_yubi::discovery::discover_real_card_by_serial(s),
                    None => aivyx_yubi::discovery::discover_real_card(),
                },
                &aivyx_yubi::discovery::list_cards,
            ),
        },
    };
    println!("{}", serde_json::to_string(&reply).expect("reply serializes"));
}
```

Add a test for the binary's input handling, in `serve.rs`'s tests: `serde_json::from_slice::<Request>(br#"{"op":"format_disk"}"#)` is an `Err`. This is how an unknown request gets refused.

- [ ] **Step 4: Run the tests, build the binary, and smoke-test it with no card.**
  - Run: `cargo test -p aivyx-card-helper --features card,test-util && cargo build -p aivyx-card-helper --features card --release`
  - Run: `echo '{"op":"list"}' | target/release/aivyx-pa-card`
  - Expected: `{"result":"cards","cards":[]}`, or an error naming `pcscd`.
  - Run: `cargo tree -p aivyx-cli -e normal | grep -c pcsc`
  - Expected: `0`. The CLI must not link PC/SC by default.

- [ ] **Step 5: Commit.**

```bash
git add crates/aivyx-card-helper Cargo.lock
git commit -s -m "feat: aivyx-pa-card helper binary (list, public key, generate, ECDH)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

### Task 6: `card_unlock` — the file, enroll, unlock, list, remove (+ path, Ward)

**Files:**
- Modify: `crates/aivyx-instance/src/lib.rs` (add `card_unlock_file()`)
- Modify: `crates/aivyx-core/src/sensitive_paths.rs` (basename `card-unlock.toml`, plus a test entry)
- Create: `crates/aivyx-channel/src/card_unlock.rs`
- Modify: `crates/aivyx-channel/src/lib.rs` (`pub mod card_unlock;`)
- Modify: `crates/aivyx-channel/Cargo.toml` (deps: `aivyx-card-helper`, `x25519-dalek`; dev: `aivyx-card-helper` with `test-util`)

**Interfaces:**
- Consumes: `aivyx_crypto::{MasterKey, WrapKey, WrappedMasterKey}` (Task 3); `aivyx_card_helper::{CardHelper, HelperError}` (Task 4).
- Produces:

```rust
pub const FILE_VERSION: u32 = 1;
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CardEntry { pub serial: String, pub label: String, pub added: String,
    pub card_public: String, pub ephemeral_public: String, pub nonce: String, pub wrapped: String }
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CardUnlockFile { pub version: u32, #[serde(default)] pub cards: Vec<CardEntry> }
#[derive(Debug, thiserror::Error)]
pub enum CardUnlockError {
    #[error("{0}")] Helper(#[from] HelperError),
    #[error("none of your enrolled cards is plugged in (enrolled: {0})")] NoEnrolledCardPresent(String),
    #[error("card {0} isn't enrolled")] NotEnrolled(String),
    #[error("card {serial}'s entry doesn't open — remove and re-add it (aivyx-pa card remove {serial})")] EntryDoesNotOpen { serial: String },
    #[error("{path} is broken: {reason}")] BrokenFile { path: String, reason: String },
    #[error("{0}")] Io(String),
}
impl CardUnlockFile {
    pub fn load(path: &Path) -> Result<Option<Self>, CardUnlockError>;  // Ok(None) if missing
    pub fn save(&self, path: &Path) -> Result<(), CardUnlockError>;       // 0600, temp + rename
}
pub fn binding_info(instance: &str, serial: &str, card_public: &[u8; 32], ephemeral_public: &[u8; 32]) -> Vec<u8>;
pub fn enroll(file: &mut CardUnlockFile, master: &MasterKey, instance: &str, serial: &str, label: &str,
    card_public: [u8; 32], helper: &dyn CardHelper, pin: &SecretString) -> Result<(), CardUnlockError>;
pub fn unlock_with(file: &CardUnlockFile, instance: &str, serial: Option<&str>,
    helper: &dyn CardHelper, pin: &SecretString) -> Result<(MasterKey, String /* serial used */), CardUnlockError>;
pub fn remove(file: &mut CardUnlockFile, serial: &str) -> Result<CardEntry, CardUnlockError>;
```

- [ ] **Step 1: Add the path accessor and a test** in `aivyx-instance`. Find the `#[cfg(test)]` module that tests `config_file`, and add the test there:

```rust
    /// Card unlock file (A18): `config_dir/card-unlock.toml`, beside `aivyx-pa.toml`.
    pub fn card_unlock_file(&self) -> Option<PathBuf> {
        self.config_dir().map(|d| d.join("card-unlock.toml"))
    }
```

```rust
    #[test]
    fn the_card_unlock_file_sits_beside_the_config() {
        let p = /* build InstancePaths the way the neighbouring config_file test does */;
        assert_eq!(p.card_unlock_file().unwrap(), p.config_dir().unwrap().join("card-unlock.toml"));
    }
```

Copy the `InstancePaths` construction from the existing `config_file` test, so the test uses this repo's real helper.

- [ ] **Step 2: Add Ward's basename.** In `sensitive_paths.rs`, add `"card-unlock.toml",` after `"daemon.env",` with the comment `// A18: wrapped master keys (useless without the card, still never readable by a tool).` Add `"/home/alice/.config/aivyx-pa/card-unlock.toml",` to the "crown jewels" list in the blocking test.

- [ ] **Step 3: Write the failing tests** for `card_unlock.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_card_helper::FakeHelper;

    fn master() -> MasterKey { MasterKey::from_raw([3u8; 32]) }
    fn pin() -> SecretString { SecretString::from("123456") }

    fn enrolled(h: &FakeHelper, serial: &str) -> CardUnlockFile {
        let mut f = CardUnlockFile::default();
        let public = h.public_key(serial).unwrap().unwrap();
        enroll(&mut f, &master(), "default", serial, "laptop", public, h, &pin()).unwrap();
        f
    }

    #[test]
    fn enroll_then_unlock_gives_the_same_master_key() {
        let h = FakeHelper::with_card("S1", [5u8; 32]);
        let f = enrolled(&h, "S1");
        let (m, used) = unlock_with(&f, "default", None, &h, &pin()).unwrap();
        assert!(m.same_as(&master()));
        assert_eq!(used, "S1");
    }

    #[test]
    fn another_instance_or_a_changed_serial_does_not_open() {
        let h = FakeHelper::with_card("S1", [5u8; 32]);
        let f = enrolled(&h, "S1");
        assert!(matches!(unlock_with(&f, "work", None, &h, &pin()), Err(CardUnlockError::EntryDoesNotOpen { .. })));
        let mut g = f.clone();
        g.cards[0].serial = "S2".into();
        let h2 = FakeHelper::with_card("S2", [5u8; 32]);
        assert!(matches!(unlock_with(&g, "default", None, &h2, &pin()), Err(CardUnlockError::EntryDoesNotOpen { .. })));
    }

    #[test]
    fn swapped_ephemeral_keys_and_flipped_bytes_do_not_open() {
        let h = FakeHelper::with_card("S1", [5u8; 32]);
        h.add_card("S2", [6u8; 32]);
        let mut f = enrolled(&h, "S1");
        let public2 = h.public_key("S2").unwrap().unwrap();
        enroll(&mut f, &master(), "default", "S2", "spare", public2, &h, &pin()).unwrap();
        let e0 = f.cards[0].ephemeral_public.clone();
        f.cards[0].ephemeral_public = f.cards[1].ephemeral_public.clone();
        f.cards[1].ephemeral_public = e0;
        assert!(unlock_with(&f, "default", Some("S1"), &h, &pin()).is_err());

        let mut g = enrolled(&h, "S1");
        let mut raw = B64.decode(&g.cards[0].wrapped).unwrap();
        raw[0] ^= 1;
        g.cards[0].wrapped = B64.encode(raw);
        assert!(unlock_with(&g, "default", None, &h, &pin()).is_err());
    }

    #[test]
    fn the_first_plugged_in_enrolled_card_is_used_and_absence_is_explained() {
        let h = FakeHelper::with_card("S1", [5u8; 32]);
        let f = enrolled(&h, "S1");
        h.unplugged("S1");
        let err = unlock_with(&f, "default", None, &h, &pin()).unwrap_err();
        assert!(matches!(err, CardUnlockError::NoEnrolledCardPresent(ref s) if s.contains("laptop") && s.contains("S1")));
        assert!(matches!(unlock_with(&f, "default", Some("S9"), &h, &pin()), Err(CardUnlockError::NotEnrolled(_))));
    }

    #[test]
    fn the_file_is_0600_round_trips_and_a_broken_one_is_reported() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("card-unlock.toml");
        assert!(CardUnlockFile::load(&path).unwrap().is_none());
        let h = FakeHelper::with_card("S1", [5u8; 32]);
        let f = enrolled(&h, "S1");
        f.save(&path).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(CardUnlockFile::load(&path).unwrap().unwrap(), f);
        std::fs::write(&path, "version = 1\n[[cards]]\nserial = 3\n").unwrap();
        assert!(matches!(CardUnlockFile::load(&path), Err(CardUnlockError::BrokenFile { .. })));
    }

    #[test]
    fn enrolling_the_same_card_again_replaces_its_entry_and_remove_deletes_it() {
        let h = FakeHelper::with_card("S1", [5u8; 32]);
        let mut f = enrolled(&h, "S1");
        let public = h.public_key("S1").unwrap().unwrap();
        enroll(&mut f, &master(), "default", "S1", "renamed", public, &h, &pin()).unwrap();
        assert_eq!(f.cards.len(), 1);
        assert_eq!(f.cards[0].label, "renamed");
        assert_eq!(remove(&mut f, "S1").unwrap().label, "renamed");
        assert!(matches!(remove(&mut f, "S1"), Err(CardUnlockError::NotEnrolled(_))));
    }
}
```

Add `pub fn add_card(&self, serial: &str, secret: [u8; 32])` to `FakeHelper` (Task 4's file) for the two-card test. If `tempfile` isn't already a dev-dependency of aivyx-channel, add it.

- [ ] **Step 4: Run them and see them fail.**
  - Run: `cargo test -p aivyx-channel card_unlock`
  - Expected: compile errors.

- [ ] **Step 5: Implement it.** `crates/aivyx-channel/src/card_unlock.rs`:

```rust
//! Card unlock (Amendment A18): per enrolled OpenPGP card, the master key
//! wrapped under HKDF(X25519(ephemeral, card)). The ephemeral secret is
//! dropped at enrollment, so only the card's ECDH rebuilds the wrap key.

use std::path::Path;

use aivyx_card_helper::{CardHelper, HelperError, decode_key, encode_key};
use aivyx_crypto::{MasterKey, WrapKey, WrappedMasterKey};
use base64::Engine;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;
pub const FILE_VERSION: u32 = 1;

// (types from Interfaces here)

/// `info` and AAD: binds an entry to its instance, card and both keys.
pub fn binding_info(instance: &str, serial: &str, card_public: &[u8; 32], ephemeral_public: &[u8; 32]) -> Vec<u8> {
    let mut v = Vec::new();
    for part in [instance.as_bytes(), serial.as_bytes()] {
        v.extend_from_slice(&(part.len() as u32).to_be_bytes());
        v.extend_from_slice(part);
    }
    v.extend_from_slice(card_public);
    v.extend_from_slice(ephemeral_public);
    v
}

impl CardUnlockFile {
    pub fn load(path: &Path) -> Result<Option<Self>, CardUnlockError> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(CardUnlockError::Io(format!("read {}: {e}", path.display()))),
        };
        let broken = |reason: String| CardUnlockError::BrokenFile { path: path.display().to_string(), reason };
        let file: Self = toml::from_str(&text).map_err(|e| broken(e.to_string()))?;
        if file.version != FILE_VERSION {
            return Err(broken(format!("version {} (this aivyx-pa reads {FILE_VERSION})", file.version)));
        }
        Ok(Some(file))
    }

    pub fn save(&self, path: &Path) -> Result<(), CardUnlockError> {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let io = |e: std::io::Error| CardUnlockError::Io(format!("write {}: {e}", path.display()));
        let text = toml::to_string_pretty(self).map_err(|e| CardUnlockError::Io(e.to_string()))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(io)?;
        }
        let tmp = path.with_extension("toml.tmp");
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp).map_err(io)?;
        f.write_all(text.as_bytes()).map_err(io)?;
        f.sync_all().map_err(io)?;
        std::fs::rename(&tmp, path).map_err(io)
    }
}

pub fn enroll(
    file: &mut CardUnlockFile,
    master: &MasterKey,
    instance: &str,
    serial: &str,
    label: &str,
    card_public: [u8; 32],
    helper: &dyn CardHelper,
    pin: &SecretString,
) -> Result<(), CardUnlockError> {
    use rand::rngs::OsRng;
    let eph = x25519_dalek::StaticSecret::random_from_rng(OsRng);
    let eph_public = *x25519_dalek::PublicKey::from(&eph).as_bytes();
    drop(eph); // never stored: only the card can rebuild the shared secret
    // The card computes it (PIN + touch), which also proves the card works.
    let shared = helper.ecdh(serial, pin, &eph_public)?;
    let info = binding_info(instance, serial, &card_public, &eph_public);
    let key = WrapKey::derive(&shared, &info).map_err(|e| CardUnlockError::Io(e.to_string()))?;
    let wrapped = master.wrap(&key, &info).map_err(|e| CardUnlockError::Io(e.to_string()))?;
    let entry = CardEntry {
        serial: serial.into(),
        label: label.into(),
        added: chrono::Utc::now().to_rfc3339(),
        card_public: encode_key(&card_public),
        ephemeral_public: encode_key(&eph_public),
        nonce: B64.encode(wrapped.nonce),
        wrapped: B64.encode(&wrapped.ciphertext),
    };
    file.version = FILE_VERSION;
    file.cards.retain(|c| c.serial != serial);
    file.cards.push(entry);
    Ok(())
}

fn open_entry(entry: &CardEntry, instance: &str, helper: &dyn CardHelper, pin: &SecretString) -> Result<MasterKey, CardUnlockError> {
    let bad = || CardUnlockError::EntryDoesNotOpen { serial: entry.serial.clone() };
    let card_public = decode_key(&entry.card_public).ok_or_else(bad)?;
    let eph_public = decode_key(&entry.ephemeral_public).ok_or_else(bad)?;
    let nonce: [u8; 12] = B64.decode(&entry.nonce).ok().and_then(|n| n.try_into().ok()).ok_or_else(bad)?;
    let ciphertext = B64.decode(&entry.wrapped).map_err(|_| bad())?;
    let shared = helper.ecdh(&entry.serial, pin, &eph_public)?;
    let info = binding_info(instance, &entry.serial, &card_public, &eph_public);
    let key = WrapKey::derive(&shared, &info).map_err(|_| bad())?;
    MasterKey::unwrap(&WrappedMasterKey { nonce, ciphertext }, &key, &info).map_err(|_| bad())
}

pub fn unlock_with(
    file: &CardUnlockFile,
    instance: &str,
    serial: Option<&str>,
    helper: &dyn CardHelper,
    pin: &SecretString,
) -> Result<(MasterKey, String), CardUnlockError> {
    let entry = match serial {
        Some(s) => file.cards.iter().find(|c| c.serial == s).ok_or_else(|| CardUnlockError::NotEnrolled(s.into()))?,
        None => {
            let present: Vec<String> = helper.list()?.into_iter().map(|c| c.serial).collect();
            file.cards.iter().find(|c| present.contains(&c.serial)).ok_or_else(|| {
                CardUnlockError::NoEnrolledCardPresent(
                    file.cards.iter().map(|c| format!("{} ({})", c.label, c.serial)).collect::<Vec<_>>().join(", "),
                )
            })?
        }
    };
    open_entry(entry, instance, helper, pin).map(|m| (m, entry.serial.clone()))
}

pub fn remove(file: &mut CardUnlockFile, serial: &str) -> Result<CardEntry, CardUnlockError> {
    let i = file.cards.iter().position(|c| c.serial == serial).ok_or_else(|| CardUnlockError::NotEnrolled(serial.into()))?;
    Ok(file.cards.remove(i))
}
```

Check that `chrono`, `rand` (0.8, whose `OsRng` x25519-dalek 2 accepts), `toml` and `base64` are already dependencies of aivyx-channel (`grep -n "^chrono\|^rand\|^toml\|^base64" crates/aivyx-channel/Cargo.toml`), and add any that are missing as `{ workspace = true }`. Add `x25519-dalek = { workspace = true }` and `aivyx-card-helper = { workspace = true }`, plus in `[dev-dependencies]` `aivyx-card-helper = { workspace = true, features = ["test-util"] }`. `rustfmt --edition 2024 crates/aivyx-channel/src/card_unlock.rs`.

- [ ] **Step 6: Run the tests.**
  - Run: `cargo test -p aivyx-channel card_unlock && cargo test -p aivyx-instance && cargo test -p aivyx-core sensitive`
  - Expected: all pass, and the instance-paths guard test still passes (the path is built only in `InstancePaths`).

- [ ] **Step 7: Commit.**

```bash
git add crates/aivyx-instance crates/aivyx-core/src/sensitive_paths.rs crates/aivyx-channel Cargo.lock
git commit -s -m "feat: card_unlock — enroll, unlock, list, remove; card-unlock.toml path + Ward (A18)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

### Task 7: IPC messages for lock state

**Files:**
- Modify: `crates/aivyx-ipc/src/protocol.rs` (`FrontendMessage`, `DaemonMessage`, `DaemonEnvelope`, round-trip tests)

**Interfaces:**
- Produces new variants. Strings only, so the crate stays wasm-clean:

```rust
// FrontendMessage
GetLockState,
Unlock { pin: String, #[serde(default)] serial: Option<String> },
UnlockWithPassphrase { passphrase: String },
Lock,
EnrollCard { serial: String, label: String, pin: String, #[serde(default)] generate: bool, #[serde(default)] admin_pin: Option<String> },
// DaemonMessage and DaemonEnvelope (both, as the file mirrors them)
LockState { locked: bool, cards: Vec<LockCardView> },
Unlocked { method: String },               // "card <serial>" | "passphrase"
UnlockFailed { code: String, message: String, #[serde(default)] tries_left: Option<u8> },
Locked { message: String },                // refusal of any other request while locked
CardEnrolled { serial: String },
// new struct
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockCardView { pub serial: String, pub label: String }
```

`UnlockFailed.code` takes one of these values: `wrong_pin`, `pin_blocked`, `no_card`, `not_enrolled`, `touch_timeout`, `entry`, `helper_missing`, `busy`, `backoff`, `wrong_passphrase`, `not_locked`, `other`.

- [ ] **Step 1: Write the failing test.** Add it next to the existing frame round-trip tests (search `fn ` near line 3690):

```rust
    #[test]
    fn lock_state_messages_round_trip() {
        for m in [
            FrontendMessage::GetLockState,
            FrontendMessage::Unlock { pin: "1".into(), serial: Some("S".into()) },
            FrontendMessage::UnlockWithPassphrase { passphrase: "p".into() },
            FrontendMessage::Lock,
            FrontendMessage::EnrollCard { serial: "S".into(), label: "l".into(), pin: "1".into(), generate: false, admin_pin: None },
        ] {
            let (back, _) = decode_frame::<FrontendMessage>(&encode_frame(&m).unwrap()).unwrap();
            assert_eq!(back, m);
        }
        let d = DaemonMessage::UnlockFailed { code: "wrong_pin".into(), message: "PIN incorrect".into(), tries_left: Some(2) };
        let (env, _) = decode_frame::<DaemonEnvelope>(&encode_frame(&d).unwrap()).unwrap();
        assert!(matches!(env, DaemonEnvelope::UnlockFailed { tries_left: Some(2), .. }));
    }
```

- [ ] **Step 2: Run it and see it fail.**
  - Run: `cargo test -p aivyx-ipc lock_state`
  - Expected: compile errors.

- [ ] **Step 3: Add the variants** to all three enums, with doc comments that cite A18. Then fix every exhaustive `match` on these enums that the compiler now flags. Run `cargo check --workspace --all-targets` and follow the errors. Most will be in `daemon_server.rs` (`handle_connection`) and the frontends' receive loops:
  - For a new `FrontendMessage` arm in the unlocked daemon: `GetLockState` answers `LockState { locked: false, cards }` (cards from `CardUnlockFile`; empty if no file). `Unlock` and `UnlockWithPassphrase` answer `UnlockFailed { code: "not_locked", .. }`. `Lock` and `EnrollCard` answer `Error { code: "unsupported", .. }` for now; Task 9 implements them.
  - For a new `DaemonEnvelope` arm in a frontend: show `Locked { message }` as an error line, like `Error`, and ignore the rest.

  Also run `cargo check -p aivyx-web --target wasm32-unknown-unknown` (`just check-web`) to keep the web client compiling.

- [ ] **Step 4: Run the tests.**
  - Run: `cargo test -p aivyx-ipc && cargo clippy --workspace --all-targets -- -D warnings`
  - Expected: they pass.

- [ ] **Step 5: Commit.**

```bash
git add -A crates
git commit -s -m "feat(ipc): lock-state messages (GetLockState, Unlock, Lock, EnrollCard; A18)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

### Task 8: The locked daemon

**Files:**
- Create: `crates/aivyx-channel/src/locked_daemon.rs`
- Modify: `crates/aivyx-channel/src/lib.rs` (`pub mod locked_daemon;`)
- Modify: `crates/aivyx-channel/src/daemon_server.rs` (`bind_unix_socket_0600` becomes `pub(crate)`)

**Interfaces:**
- Consumes: `card_unlock::{CardUnlockFile, unlock_with, CardUnlockError}` (Task 6); `aivyx_ipc` messages (Task 7); `aivyx_card_helper::CardHelper`.
- Produces:

```rust
pub struct LockedConfig {
    pub socket_path: PathBuf,
    pub instance: String,
    pub cards: CardUnlockFile,
    pub helper: Arc<dyn CardHelper>,
    /// Derives the master key from a passphrase (Argon2id + the store's salt); injected for tests.
    pub from_passphrase: Arc<dyn Fn(&SecretString) -> Result<MasterKey, String> + Send + Sync>,
    /// Checks a candidate key really opens this store (e.g. derive the audit
    /// subkey and verify the chain head); a wrong passphrase fails here.
    pub verify: Arc<dyn Fn(&MasterKey) -> bool + Send + Sync>,
    pub backoff: Duration,    // 30 s in production
    pub max_failures: u32,    // 5 in production
}
pub enum UnlockMethod { Card { serial: String }, Passphrase }
pub enum LockedOutcome { Unlocked(MasterKey, UnlockMethod), Stopped }
pub async fn run_locked(cfg: LockedConfig) -> Result<LockedOutcome, String>;
```

The locked server sends `DaemonReady` first, exactly as the full daemon does, so `daemon_status` and the clients keep working. It then answers frames one connection at a time. `run_locked` returns after a successful unlock **without removing the socket file**: the full daemon's own bind replaces it, since `bind_unix_socket_0600` renames over the path.

- [ ] **Step 1: Write the failing tests** (tokio, a real socket in a tempdir, `FakeHelper`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_card_helper::FakeHelper;
    use aivyx_ipc::protocol::{DaemonEnvelope, FrontendMessage, decode_frame, encode_frame};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixStream;

    fn master() -> MasterKey { MasterKey::from_raw([3u8; 32]) }

    fn config(dir: &Path, helper: Arc<FakeHelper>) -> LockedConfig {
        let mut cards = CardUnlockFile::default();
        let public = helper.public_key("S1").unwrap().unwrap();
        crate::card_unlock::enroll(&mut cards, &master(), "default", "S1", "laptop", public, &*helper, &SecretString::from("123456")).unwrap();
        LockedConfig {
            socket_path: dir.join("d.sock"),
            instance: "default".into(),
            cards,
            helper,
            from_passphrase: Arc::new(|p| if p.expose_secret() == "right" { Ok(MasterKey::from_raw([3u8; 32])) } else { Ok(MasterKey::from_raw([9u8; 32])) }),
            verify: Arc::new(|m| m.same_as(&MasterKey::from_raw([3u8; 32]))),
            backoff: Duration::from_millis(200),
            max_failures: 2,
        }
    }

    /// Connect, read DaemonReady, send one message, read one reply.
    async fn ask(path: &Path, m: FrontendMessage) -> DaemonEnvelope {
        let mut s = UnixStream::connect(path).await.unwrap();
        let mut buf = vec![0u8; 65536];
        let n = s.read(&mut buf).await.unwrap();
        let (ready, _) = decode_frame::<DaemonEnvelope>(&buf[..n]).unwrap();
        assert!(matches!(ready, DaemonEnvelope::DaemonReady { .. }));
        s.write_all(&encode_frame(&m).unwrap()).await.unwrap();
        let n = s.read(&mut buf).await.unwrap();
        decode_frame::<DaemonEnvelope>(&buf[..n]).unwrap().0
    }

    async fn wait_for(path: &Path) {
        for _ in 0..100 {
            if path.exists() { return; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("socket never appeared");
    }

    #[tokio::test]
    async fn it_refuses_everything_but_lock_requests_then_unlocks_with_the_card() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(dir.path(), Arc::new(FakeHelper::with_card("S1", [5u8; 32])));
        let path = cfg.socket_path.clone();
        let task = tokio::spawn(run_locked(cfg));
        wait_for(&path).await;
        let r = ask(&path, FrontendMessage::StartSession { role: None, frontend_type: None }).await;
        assert!(matches!(r, DaemonEnvelope::Locked { ref message } if message.contains("aivyx-pa unlock")));
        let r = ask(&path, FrontendMessage::GetLockState).await;
        assert!(matches!(r, DaemonEnvelope::LockState { locked: true, ref cards } if cards[0].label == "laptop"));
        let r = ask(&path, FrontendMessage::Unlock { pin: "123456".into(), serial: None }).await;
        assert!(matches!(r, DaemonEnvelope::Unlocked { ref method } if method == "card S1"));
        let LockedOutcome::Unlocked(m, UnlockMethod::Card { serial }) = task.await.unwrap().unwrap() else { panic!() };
        assert!(m.same_as(&master()));
        assert_eq!(serial, "S1");
    }

    #[tokio::test]
    async fn a_wrong_pin_reports_tries_and_repeated_failures_back_off() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(dir.path(), Arc::new(FakeHelper::with_card("S1", [5u8; 32])));
        let path = cfg.socket_path.clone();
        let _task = tokio::spawn(run_locked(cfg));
        wait_for(&path).await;
        let r = ask(&path, FrontendMessage::Unlock { pin: "000000".into(), serial: None }).await;
        assert!(matches!(r, DaemonEnvelope::UnlockFailed { ref code, tries_left: Some(2), .. } if code == "wrong_pin"));
        let _ = ask(&path, FrontendMessage::UnlockWithPassphrase { passphrase: "wrong".into() }).await;
        let r = ask(&path, FrontendMessage::Unlock { pin: "123456".into(), serial: None }).await;
        assert!(matches!(r, DaemonEnvelope::UnlockFailed { ref code, .. } if code == "backoff"));
        tokio::time::sleep(Duration::from_millis(250)).await;
        let r = ask(&path, FrontendMessage::Unlock { pin: "123456".into(), serial: None }).await;
        assert!(matches!(r, DaemonEnvelope::Unlocked { .. }));
    }

    #[tokio::test]
    async fn the_passphrase_unlocks_and_a_wrong_one_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(dir.path(), Arc::new(FakeHelper::with_card("S1", [5u8; 32])));
        let path = cfg.socket_path.clone();
        let task = tokio::spawn(run_locked(cfg));
        wait_for(&path).await;
        let r = ask(&path, FrontendMessage::UnlockWithPassphrase { passphrase: "nope".into() }).await;
        assert!(matches!(r, DaemonEnvelope::UnlockFailed { ref code, .. } if code == "wrong_passphrase"));
        let r = ask(&path, FrontendMessage::UnlockWithPassphrase { passphrase: "right".into() }).await;
        assert!(matches!(r, DaemonEnvelope::Unlocked { ref method } if method == "passphrase"));
        assert!(matches!(task.await.unwrap().unwrap(), LockedOutcome::Unlocked(_, UnlockMethod::Passphrase)));
    }

    #[tokio::test]
    async fn shutdown_stops_a_locked_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(dir.path(), Arc::new(FakeHelper::with_card("S1", [5u8; 32])));
        let path = cfg.socket_path.clone();
        let task = tokio::spawn(run_locked(cfg));
        wait_for(&path).await;
        let _ = ask(&path, FrontendMessage::Shutdown).await;
        assert!(matches!(task.await.unwrap().unwrap(), LockedOutcome::Stopped));
    }
}
```

`Shutdown`'s reply is the existing `ShuttingDown` lifecycle event, sent with the same helper the full daemon uses (`send_shutting_down`; make it `pub(crate)`), so `daemon_stop` works unchanged.

- [ ] **Step 2: Run them and see them fail.**
  - Run: `cargo test -p aivyx-channel locked_daemon`
  - Expected: compile errors.

- [ ] **Step 3: Implement it.** The core loop:

```rust
pub async fn run_locked(cfg: LockedConfig) -> Result<LockedOutcome, String> {
    let listener = crate::daemon_server::bind_unix_socket_0600(&cfg.socket_path)
        .map_err(|e| format!("bind {}: {e}", cfg.socket_path.display()))?;
    let mut failures = 0u32;
    let mut locked_until: Option<tokio::time::Instant> = None;
    loop {
        let (stream, _) = listener.accept().await.map_err(|e| e.to_string())?;
        let (mut r, mut w) = stream.into_split();
        send(&mut w, &DaemonMessageOrLifecycle::Ready).await; // DaemonReady { version: PROTOCOL_VERSION }
        let Some(msg) = read_one(&mut r).await else { continue };
        match msg {
            FrontendMessage::GetLockState => reply(&mut w, lock_state(&cfg)).await,
            FrontendMessage::Shutdown => {
                crate::daemon_server::send_shutting_down(&mut w, "stopped while locked").await;
                return Ok(LockedOutcome::Stopped);
            }
            FrontendMessage::Unlock { pin, serial } => {
                if let Some(until) = locked_until && tokio::time::Instant::now() < until {
                    reply(&mut w, failed("backoff", "Too many failed unlocks — try again in 30 s", None)).await;
                    continue;
                }
                let helper = cfg.helper.clone();
                let (cards, inst) = (cfg.cards.clone(), cfg.instance.clone());
                let pin = SecretString::from(pin);
                // One unlock at a time: the accept loop is sequential, so a second
                // client waits in the listen backlog until this one finishes.
                let res = tokio::task::spawn_blocking(move || {
                    crate::card_unlock::unlock_with(&cards, &inst, serial.as_deref(), &*helper, &pin)
                }).await.map_err(|e| e.to_string())?;
                match res {
                    Ok((key, serial)) => {
                        reply(&mut w, DaemonMessage::Unlocked { method: format!("card {serial}") }).await;
                        return Ok(LockedOutcome::Unlocked(key, UnlockMethod::Card { serial }));
                    }
                    Err(e) => {
                        failures += 1;
                        if failures >= cfg.max_failures {
                            locked_until = Some(tokio::time::Instant::now() + cfg.backoff);
                            failures = 0;
                        }
                        reply(&mut w, unlock_error(e)).await;
                    }
                }
            }
            FrontendMessage::UnlockWithPassphrase { passphrase } => {
                // Same backoff check as above.
                let candidate = (cfg.from_passphrase)(&SecretString::from(passphrase));
                match candidate {
                    Ok(key) if (cfg.verify)(&key) => {
                        reply(&mut w, DaemonMessage::Unlocked { method: "passphrase".into() }).await;
                        return Ok(LockedOutcome::Unlocked(key, UnlockMethod::Passphrase));
                    }
                    _ => { /* count a failure as above */ reply(&mut w, failed("wrong_passphrase", "That passphrase doesn't open this store", None)).await; }
                }
            }
            _ => reply(&mut w, DaemonMessage::Locked {
                message: "PA is locked — run `aivyx-pa unlock`".into(),
            }).await,
        }
    }
}
```

Write the small helpers: `send` and `reply` (`encode_frame` then `write_all`), `read_one` (read until `decode_frame` succeeds or EOF, capped at `MAX_PAYLOAD_SIZE`), `failed(code, message, tries)` building `DaemonMessage::UnlockFailed`, and `lock_state(&cfg)` building `DaemonMessage::LockState { locked: true, cards }` from `cfg.cards`.

`unlock_error` maps:
- `CardUnlockError::Helper(HelperError::Card { kind, message, tries_left })` to a code by `ErrorKind`: `WrongPin` → `wrong_pin`, `PinBlocked` → `pin_blocked`, `CardNotFound` or `NoReader` → `no_card`, `TouchTimeout` → `touch_timeout`, anything else → `other`;
- `Helper(NotInstalled)` → `helper_missing`;
- `NoEnrolledCardPresent` → `no_card`;
- `NotEnrolled` → `not_enrolled`;
- `EntryDoesNotOpen` → `entry`.

Every message is the error's own `Display`, so the user sees the spec's wording.

The backoff counter follows the spec: 5 failures in a row, then 30 s. The tests inject 2 failures and 200 ms. Count passphrase and card failures together, and reset the count on success.

- [ ] **Step 4: Run the tests.**
  - Run: `cargo test -p aivyx-channel locked_daemon && cargo clippy -p aivyx-channel --all-targets -- -D warnings`
  - Expected: they pass.

- [ ] **Step 5: Commit.**

```bash
git add crates/aivyx-channel
git commit -s -m "feat: locked daemon — serve only lock requests until a card or passphrase unlocks (A18)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

### Task 9: Wire it into start-up, the audit, `EnrollCard` and `Lock`

**Files:**
- Modify: `crates/aivyx-cli/src/bin/aivyx.rs`: the master-key section, around `let passphrase_source = select_passphrase_source(...)` (≈ line 1492) and `let master_key = derive_master_key(...)` (≈ line 1567); the audit open (search `PersonaSeeded` ≈ line 8085); and the `DaemonConfig { .. }` construction.
- Modify: `crates/aivyx-audit/src/lib.rs` (`AuditEvent::StoreUnlocked { method: String }`)
- Modify: `crates/aivyx-channel/src/daemon_server.rs` (`DaemonConfig.card_unlock: Option<CardUnlockContext>`; `EnrollCard` and `Lock` arms)
- Modify: `crates/aivyx-channel/src/card_unlock.rs` (`CardUnlockContext`)

**Interfaces:**
- Consumes: `locked_daemon::{run_locked, LockedConfig, LockedOutcome, UnlockMethod}` (Task 8); `card_unlock::*` (Task 6).
- Produces:

```rust
// card_unlock.rs
pub struct CardUnlockContext {
    pub instance: String,
    pub file_path: PathBuf,
    pub helper: Arc<dyn CardHelper>,
    /// A copy of the master key, held only so `EnrollCard` can wrap it.
    pub master: MasterKey,
    /// False when a passphrase source is saved (keyring, daemon.env, env, TOML):
    /// a relocked daemon would unlock itself again, so `Lock` is refused.
    pub can_lock: bool,
    /// Set by `Lock`; read by aivyx.rs after `run_daemon` returns to re-exec.
    pub relock_requested: Arc<AtomicBool>,
}
// aivyx-audit
AuditEvent::StoreUnlocked { method: String } // "passphrase" | "card <serial>"
```

- [ ] **Step 1: Write the failing tests.**
  - **(a) Audit, in `aivyx-audit`:** `StoreUnlocked` serialises with `method`. Copy the shape of the nearest existing event round-trip test.
  - **(b) Source choice, in `aivyx.rs`'s `mod tests`**, for a new pure function:

```rust
/// What start-up does for the master key (A18).
#[derive(Debug, PartialEq)]
enum MasterKeyPlan { Passphrase, Locked, Fail(String) }

fn plan_master_key(source: Result<(), String>, enrolled_cards: usize, is_daemon: bool) -> MasterKeyPlan
```

```rust
    #[test]
    fn a_daemon_with_no_passphrase_and_an_enrolled_card_starts_locked() {
        assert_eq!(plan_master_key(Ok(()), 1, true), MasterKeyPlan::Passphrase);
        assert_eq!(plan_master_key(Err("no passphrase".into()), 1, true), MasterKeyPlan::Locked);
        assert_eq!(plan_master_key(Err("no passphrase".into()), 0, true), MasterKeyPlan::Fail("no passphrase".into()));
        // Only the daemon starts locked; one-shot commands keep today's error.
        assert_eq!(plan_master_key(Err("no passphrase".into()), 1, false), MasterKeyPlan::Fail("no passphrase".into()));
    }
```

  - **(c) Daemon server, in `daemon_server.rs`'s tests,** using the existing harness that starts `run_daemon` on a temp socket. Search the test module for an existing test that sends a `FrontendMessage` and reads the reply, and copy its setup.
    - `EnrollCard` with a `CardUnlockContext` holding `FakeHelper::with_card("S2", ..)` answers `CardEnrolled { serial: "S2" }`, and the file on disk then has an entry that `unlock_with` opens to the same master key.
    - `Lock` with `can_lock: false` answers `Error { code: "lock_unavailable", .. }`, with a message saying a saved passphrase would unlock it again.
    - `Lock` with `can_lock: true` sets `relock_requested` and the daemon shuts down.

- [ ] **Step 2: Run them and see them fail.**
  - Run: `cargo test -p aivyx-audit StoreUnlocked; cargo test -p aivyx-cli plan_master_key; cargo test -p aivyx-channel enroll_card`
  - Expected: compile errors.

- [ ] **Step 3: Implement start-up.** Replace `let passphrase_source = select_passphrase_source(config.passphrase.as_ref(), new_store)?;` with:

```rust
    // A18 — a daemon with no passphrase source but an enrolled card starts locked.
    let card_file_path = aivyx_instance::InstancePaths::current().ok().and_then(|p| p.card_unlock_file());
    let card_file = match card_file_path.as_deref().map(aivyx_channel::card_unlock::CardUnlockFile::load) {
        Some(Ok(f)) => f,
        Some(Err(e)) => { eprintln!("aivyx-pa: {e}; `aivyx-pa unlock --passphrase` still works"); None }
        None => None,
    };
    let enrolled = card_file.as_ref().map_or(0, |f| f.cards.len());
    let selected = select_passphrase_source(config.passphrase.as_ref(), new_store);
    let is_daemon = matches!(mode, CliMode::DaemonRun);
    let plan = plan_master_key(selected.as_ref().map(|_| ()).map_err(Clone::clone), enrolled, is_daemon);
    let passphrase_source = match plan {
        MasterKeyPlan::Passphrase => Some(selected?),
        MasterKeyPlan::Locked => None,
        MasterKeyPlan::Fail(e) => return Err(e),
    };
    let passphrase_saved = passphrase_source.is_some();
```

A broken file counts as zero cards for this decision, which would fail the start. That contradicts the spec's "starts locked and says so". So when a broken file exists, the daemon starts locked with an empty card list, and only `UnlockWithPassphrase` can open it: treat `Some(Err(_))` as `enrolled = 1` with `card_file = Some(CardUnlockFile::default())`. Write it that way, and add the case to the Step 1(b) test.

Then replace `let master_key = derive_master_key(passphrase_source, ...)?;` with:

```rust
    let (master_key, unlock_method) = match passphrase_source {
        Some(source) => (
            derive_master_key(source, &salt_path, Argon2Params::d7_default())
                .map_err(|e| format!("failed to derive master key: {e}"))?,
            "passphrase".to_string(),
        ),
        None => {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()
                .map_err(|e| format!("failed to build tokio runtime: {e}"))?;
            let helper: std::sync::Arc<dyn aivyx_card_helper::CardHelper> = match aivyx_card_helper::ProcessHelper::find() {
                Ok(h) => std::sync::Arc::new(h),
                Err(e) => { eprintln!("aivyx-pa: {e}"); std::sync::Arc::new(MissingHelper) }
            };
            let salt = salt_path.clone();
            let store = storage_path.clone();
            eprintln!("aivyx-pa: starting locked — run `aivyx-pa unlock`");
            let outcome = rt.block_on(aivyx_channel::locked_daemon::run_locked(aivyx_channel::locked_daemon::LockedConfig {
                socket_path: aivyx_ipc::protocol::default_socket_path()?,
                instance: aivyx_instance::InstancePaths::current().map_err(|e| e.to_string())?.name().as_str().to_string(),
                cards: card_file.clone().unwrap_or_default(),
                helper,
                from_passphrase: std::sync::Arc::new(move |p| {
                    derive_master_key(PassphraseSource::FromConfig(p.clone()), &salt, Argon2Params::d7_default())
                        .map_err(|e| e.to_string())
                }),
                verify: std::sync::Arc::new(move |key| store_opens_with(&store, key)),
                backoff: std::time::Duration::from_secs(30),
                max_failures: 5,
            }))?;
            match outcome {
                aivyx_channel::locked_daemon::LockedOutcome::Stopped => return Ok(()),
                aivyx_channel::locked_daemon::LockedOutcome::Unlocked(key, method) => (key, match method {
                    aivyx_channel::locked_daemon::UnlockMethod::Card { serial } => format!("card {serial}"),
                    aivyx_channel::locked_daemon::UnlockMethod::Passphrase => "passphrase".into(),
                }),
            }
        }
    };
```

Supporting pieces:
- `MissingHelper` is a small `CardHelper` whose every method returns `Err(HelperError::NotInstalled)`. Put it in `aivyx-card-helper`'s `lib.rs` as `pub struct MissingHelper;` so other code can use it.
- `store_opens_with(store, key)` returns true if the key opens the store. Open a `RedbStorage` read-only check if one exists. Otherwise derive the `audit` subkey and verify the first entry of the persistent audit log (`aivyx_audit::persistent` has a verify path; see `--verify-only`'s use). A key that fails is a wrong passphrase. If there's neither a store nor an audit log (a first run), return true: a new store has nothing to check, and a locked start never happens on a first run anyway because no card can be enrolled before a store exists.
- Check the exact name of the instance-name accessor on `InstancePaths` (`grep -n "pub fn name" crates/aivyx-instance/src/lib.rs`).

- [ ] **Step 4: Record the unlock in the audit log.** Add to `AuditEvent` (with a doc comment citing A18):

```rust
    /// A18 — how the store was unlocked this session. The first entry a daemon appends.
    StoreUnlocked { method: String },
```

Right after `persistent_audit` is opened, and before the `PersonaSeeded` block:

```rust
    {
        use aivyx_audit::AuditWriter;
        if let Err(e) = persistent_audit.append(aivyx_audit::AuditEvent::StoreUnlocked { method: unlock_method.clone() }) {
            eprintln!("aivyx-pa daemon: failed to audit the unlock: {e}");
        }
    }
```

Fix any exhaustive `match` on `AuditEvent` that the compiler flags. Render it as "store unlocked (card S1)" wherever events are formatted for `aivyx-pa audit`.

- [ ] **Step 5: Implement `EnrollCard` and `Lock` in the running daemon.** In `aivyx.rs`, before `master_key` is consumed (it's `Clone`), build:

```rust
    let card_ctx = card_file_path.clone().map(|file_path| aivyx_channel::card_unlock::CardUnlockContext {
        instance: /* the instance name, as above */,
        file_path,
        helper: aivyx_card_helper::ProcessHelper::find()
            .map(|h| std::sync::Arc::new(h) as std::sync::Arc<dyn aivyx_card_helper::CardHelper>)
            .unwrap_or_else(|_| std::sync::Arc::new(aivyx_card_helper::MissingHelper)),
        master: master_key.clone(),
        can_lock: !passphrase_saved && enrolled > 0,
        relock_requested: relock_flag.clone(),
    });
```

Pass it into `DaemonConfig { card_unlock: card_ctx, .. }` (a new `Option<CardUnlockContext>` field; every other construction site of `DaemonConfig`, including tests, gets `card_unlock: None`).

In `daemon_server.rs`'s `handle_connection` (which replaces Task 7's `unsupported` stubs):

```rust
            FrontendMessage::EnrollCard { serial, label, pin, generate, admin_pin } => {
                let reply = match ctx.card_unlock.as_ref() {
                    None => DaemonMessage::Error { code: "card_unlock_unavailable".into(), message: "card unlock isn't set up for this daemon".into() },
                    Some(c) => match enroll_card_blocking(c, &serial, &label, pin, generate, admin_pin).await {
                        Ok(()) => DaemonMessage::CardEnrolled { serial },
                        Err(e) => DaemonMessage::Error { code: "enroll_failed".into(), message: e },
                    },
                };
                /* write reply */
            }
            FrontendMessage::Lock => match ctx.card_unlock.as_ref() {
                Some(c) if c.can_lock => {
                    c.relock_requested.store(true, Ordering::SeqCst);
                    /* trigger the same shutdown path `FrontendMessage::Shutdown` uses, reason "locked" */
                }
                _ => { /* write Error { code: "lock_unavailable", message:
                    "PA can't lock: a saved passphrase would unlock it again. Remove it (`aivyx-pa keyring clear`, or delete daemon.env) and enroll a card first." } */ }
            },
```

`enroll_card_blocking` runs in `spawn_blocking`:
1. load the file (or default);
2. get the card's public key, or call `generate` when `generate` is true (refusing a non-empty slot comes from the helper);
3. call `card_unlock::enroll` and `save`.

After `run_daemon` returns in `aivyx.rs`, re-exec the binary if `relock_flag.load(SeqCst)`:

```rust
    if relock_flag.load(std::sync::atomic::Ordering::SeqCst) {
        use std::os::unix::process::CommandExt;
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        drop(master_key_copies_still_in_scope); // all keys drop (zeroize) before exec
        let err = std::process::Command::new(exe).args(std::env::args_os().skip(1)).exec();
        return Err(format!("relock: re-exec failed: {err}"));
    }
```

The re-exec keeps the process id, so a service manager sees no restart. The new image finds no passphrase source and starts locked. Before `exec`, make sure every `MasterKey`, `SubKey` and chain-key array in scope has been dropped; move the re-exec to the outermost scope where they are already gone.

- [ ] **Step 6: Run the tests.**
  - Run: `cargo test -p aivyx-audit && cargo test -p aivyx-channel && cargo test -p aivyx-cli && cargo clippy --workspace --all-targets -- -D warnings`
  - Expected: all pass. A normal passphrase start is unchanged: the existing daemon start-up tests pass untouched.

- [ ] **Step 7: Commit.**

```bash
git add -A crates
git commit -s -m "feat: start locked with an enrolled card; audit the unlock; EnrollCard and Lock (A18)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

### Task 10: CLI — `card add|list|remove`, `unlock`, `lock`, status

**Files:**
- Create: `crates/aivyx-cli/src/bin/aivyx_modules/card.rs`
- Modify: `crates/aivyx-cli/src/bin/aivyx.rs` (`CliMode::Card(CardSubcommand)`, `CliMode::Unlock { card, passphrase }`, `CliMode::Lock`; parsing beside `keyring` ≈ line 5589; dispatch beside `CliMode::Keyring` ≈ line 876; `DaemonStatus` prints `locked:`)
- Modify: `crates/aivyx-cli/src/bin/aivyx_modules/help.rs` (the help text for the new commands)
- Modify: `crates/aivyx-channel/src/daemon_client.rs` (`lock_state`, `unlock`, `unlock_with_passphrase`, `lock`, `enroll_card`: one-shot request helpers shaped like `daemon_stop`)

**Interfaces:**
- Consumes: Task 7 messages; Task 6 `card_unlock`; Task 4 `ProcessHelper`.
- Produces:

```rust
#[derive(Debug, PartialEq, Eq, Clone)]
enum CardSubcommand { Add { card: Option<String>, label: Option<String> }, List, Remove { serial: String } }
// daemon_client.rs
pub async fn lock_state(socket: &Path) -> Result<(bool, Vec<LockCardView>), DaemonError>;
pub async fn unlock(socket: &Path, pin: String, serial: Option<String>) -> Result<String /* method */, UnlockRefused>;
pub async fn unlock_with_passphrase(socket: &Path, passphrase: String) -> Result<String, UnlockRefused>;
pub async fn lock(socket: &Path) -> Result<(), String>;
pub async fn enroll_card(socket: &Path, serial: String, label: String, pin: String, generate: bool, admin_pin: Option<String>) -> Result<(), String>;
pub struct UnlockRefused { pub code: String, pub message: String, pub tries_left: Option<u8> }
```

- [ ] **Step 1: Write the failing parse tests.** Add them beside the `keyring` parse tests (search `vec!["keyring", "bogus"]` ≈ line 17557):

```rust
    #[test]
    fn card_unlock_and_lock_parse() {
        let p = |v: &[&str]| parse_args(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>()).map(|a| a.mode);
        assert_eq!(p(&["card", "add"]).unwrap(), CliMode::Card(CardSubcommand::Add { card: None, label: None }));
        assert_eq!(p(&["card", "add", "--card", "0006:1", "--label", "laptop"]).unwrap(),
            CliMode::Card(CardSubcommand::Add { card: Some("0006:1".into()), label: Some("laptop".into()) }));
        assert_eq!(p(&["card", "list"]).unwrap(), CliMode::Card(CardSubcommand::List));
        assert_eq!(p(&["card", "remove", "0006:1"]).unwrap(), CliMode::Card(CardSubcommand::Remove { serial: "0006:1".into() }));
        assert!(p(&["card", "remove"]).is_err());
        assert!(p(&["card", "bogus"]).is_err());
        assert_eq!(p(&["unlock"]).unwrap(), CliMode::Unlock { card: None, passphrase: false });
        assert_eq!(p(&["unlock", "--passphrase"]).unwrap(), CliMode::Unlock { card: None, passphrase: true });
        assert_eq!(p(&["lock"]).unwrap(), CliMode::Lock);
        assert!(p(&["lock", "x"]).is_err());
    }
```

Use the real parse function's name: the `keyring` tests call it, so copy their call.

- [ ] **Step 2: Write the failing tests for the last-card guard and the prompts**, in `card.rs`, for pure functions:

```rust
/// Removing the last card with no saved passphrase source needs this exact phrase.
pub const LAST_CARD_PHRASE: &str = "I know my passphrase";
pub fn removal_needs_confirmation(cards_left_after: usize, passphrase_saved: bool) -> bool {
    cards_left_after == 0 && !passphrase_saved
}
pub fn unlock_failure_text(code: &str, message: &str, tries_left: Option<u8>) -> String;
```

```rust
    #[test]
    fn the_last_card_needs_the_phrase_only_without_a_saved_passphrase() {
        assert!(removal_needs_confirmation(0, false));
        assert!(!removal_needs_confirmation(0, true));
        assert!(!removal_needs_confirmation(1, false));
    }
    #[test]
    fn unlock_failures_read_plainly() {
        assert_eq!(unlock_failure_text("wrong_pin", "PIN incorrect", Some(2)), "Wrong PIN — 2 tries left.");
        assert_eq!(unlock_failure_text("touch_timeout", "", None), "No touch — still locked.");
        assert!(unlock_failure_text("helper_missing", "card unlock needs …", None).contains("tool-programs install"));
    }
```

- [ ] **Step 3: Run them and see them fail.**
  - Run: `cargo test -p aivyx-cli card_unlock_and_lock_parse the_last_card unlock_failures`
  - Expected: compile errors.

- [ ] **Step 4: Implement it.**
  - **Parsing:** copy the `keyring` block's structure, including the `CliArgs { .. }` fields it fills in, for `card`, `unlock` and `lock`.
  - **Dispatch:** `card.rs` gets these entry points:
    - `pub async fn run_card(sub: CardSubcommand, socket: &Path) -> Result<(), String>`;
    - `pub async fn run_unlock(card: Option<String>, passphrase: bool, socket: &Path) -> Result<(), String>`;
    - `pub async fn run_lock(socket: &Path) -> Result<(), String>`.

    Call them from the early-dispatch section, beside `CliMode::Keyring`, with a current-thread runtime, before any store is opened.
  - **`unlock`:**
    1. `lock_state`: if the daemon isn't running, say "aivyx-pa isn't running — start it with `aivyx-pa daemon start`"; if it's not locked, print "PA is already unlocked." and exit 0.
    2. Otherwise read the PIN with `rpassword::prompt_password("YubiKey PIN: ")`, or the same no-echo reader `passphrase.rs` uses; look for `read_interactive_password`.
    3. Print "Touch your YubiKey…" and call `daemon_client::unlock`.
    4. On `wrong_pin` with `tries_left == Some(1)`, ask "One try left before the PIN blocks. Try again? [y/N]" and, on y, prompt again.
    5. Print `unlock_failure_text(..)` on failure (exit 1), or "PA is unlocked." on success.

    `--passphrase` prompts "Passphrase: " and calls `unlock_with_passphrase`.
  - **`lock`:** calls `daemon_client::lock` and prints "PA is locked. Run `aivyx-pa unlock` to open it again.", or the daemon's refusal text.
  - **`card list`:** reads `card-unlock.toml` directly (no daemon needed). For each card it prints `label  serial  added (date part)  plugged in: yes/no`, using `ProcessHelper::find()?.list()` for "plugged in". If the helper is missing, that column says "unknown (helper not installed)". With no file, it prints "No cards enrolled. Add one with `aivyx-pa card add`."
  - **`card add`:**
    1. Pick the card: `--card`, or the only card `list()` returns, or ask for a choice when there are several.
    2. If `public_key` is `None`, ask "This card has no decryption key. Make one on the card? [y/N]"; on y, read the admin PIN (no echo). If the helper reports `UnsupportedKey`, print it and stop.
    3. The label defaults to the serial.
    4. Read the PIN and print "Touch your YubiKey…".
    5. If the daemon is running and unlocked, call `daemon_client::enroll_card`.

       If no daemon is running, prompt "Passphrase: " and derive the master key (`derive_master_key(PassphraseSource::FromConfig(..), &salt_path, Argon2Params::d7_default())`). Check it with the same `store_opens_with` (move it to `card.rs` as `pub fn`). Then run `card_unlock::enroll` with the found `ProcessHelper` and `save`.

       If the daemon is locked, say "Unlock PA first (`aivyx-pa unlock --passphrase`), then add the card."
    6. Print "PA can now be unlocked with this card." Then, if a passphrase is saved in the keyring (`keyring_store::retrieve()` is `Ok(Some)`) or in `daemon.env` (`daemon_service::read_passphrase_file`), ask "Remove the saved passphrase from <the OS keyring | daemon.env>? [y/N]". On y, clear it with the existing `keyring clear` function or by deleting the file.
  - **`card remove <serial>`:**
    1. Load the file and find the entry (else "card <serial> isn't enrolled").
    2. If `removal_needs_confirmation(left, passphrase_saved)`, require typing `LAST_CARD_PHRASE` exactly.
    3. `remove` and `save`, then print "Removed <label> (<serial>)."

    No unlock is needed: removing an entry can't open anything, and the file is the operator's own.
  - **`daemon status`:** after the `running` line, call `lock_state` and print `locked: yes` or `locked: no`, plus `cards: <n>` when the daemon is locked.
  - **Help:** add the commands to `help.rs`.

- [ ] **Step 5: Run the tests**, including the CLI docs-coverage test that `help.rs` drives. It fails until Task 12 documents the commands, so run it after Task 12. For now:
  - Run: `cargo test -p aivyx-cli card_unlock_and_lock_parse the_last_card unlock_failures && cargo clippy --workspace --all-targets -- -D warnings`
  - Expected: they pass.

- [ ] **Step 6: Commit.**

```bash
git add -A crates
git commit -s -m "feat(cli): aivyx-pa card add|list|remove, unlock, lock; status shows locked (A18)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

### Task 11: Ship the helper in the tool-programs archive

**Files:**
- Modify: `.github/workflows/tools-release.yml`
- Modify: `crates/aivyx-cli/src/bin/aivyx_modules/tools_install.rs` (the expected-programs list gains `aivyx-pa-card`)

**Interfaces:**
- Consumes: the `aivyx-pa-card` binary (Task 5).
- Produces: `aivyx-pa-tools-<target>.tar.gz` contains `aivyx-pa-card`, so `aivyx-pa tool-programs install` puts it beside `aivyx-pa`, where `ProcessHelper::find()` looks first.

- [ ] **Step 1: Write the failing test** in `tools_install.rs`'s tests: the expected-programs list (whatever `status()` iterates, ≈ line 221) contains `"aivyx-pa-card"`.

- [ ] **Step 2: Run it and see it fail.**
  - Run: `cargo test -p aivyx-cli tools_install`
  - Expected: FAIL.

- [ ] **Step 3: Implement it.**
  - Add `"aivyx-pa-card"` to that list.
  - In `tools-release.yml`:
    - Install `libpcsclite-dev` on Linux (`sudo apt-get install -y musl-tools libpcsclite-dev`).
    - Add a step after "Build the tool programs" that builds the helper. It must be the gnu target on Linux, because it dynamically links the system's PC/SC; on macOS it's the matrix target:

```yaml
      - name: Build the card helper (glibc on Linux: links the system PC/SC)
        shell: bash
        run: |
          case "${{ matrix.target }}" in
            x86_64-unknown-linux-musl)  ht=x86_64-unknown-linux-gnu ;;
            aarch64-unknown-linux-musl) ht=aarch64-unknown-linux-gnu ;;
            *) ht=${{ matrix.target }} ;;
          esac
          rustup target add "$ht"
          cargo build --release --locked --target "$ht" -p aivyx-card-helper --features card
          echo "HELPER_TARGET=$ht" >> "$GITHUB_ENV"
```

    - In "Package", after the loop, add `cp "target/$HELPER_TARGET/release/aivyx-pa-card" "$name/"`.
  - Update the workflow's header comment to say the archive also carries `aivyx-pa-card`, built for glibc on Linux (Ubuntu 22.04, so glibc 2.35 or newer) because PC/SC can't be linked statically.

- [ ] **Step 4: Check it.**
  - Run: `cargo test -p aivyx-cli tools_install`
  - Expected: PASS.
  - Run the workflow once by hand (`gh workflow run tools-release.yml`) and check the artifact holds `aivyx-pa-card`. A `workflow_dispatch` run needs the file on the default branch; if that isn't the case yet, record the check for the release instead.

- [ ] **Step 5: Commit.**

```bash
git add .github/workflows/tools-release.yml crates/aivyx-cli/src/bin/aivyx_modules/tools_install.rs
git commit -s -m "build: ship aivyx-pa-card in the tool-programs archive (A18)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

### Task 12: Docs, the real-card checklist, final checks

**Files:**
- Modify: `docs/manual/reference/01-cli.md`: sections for `aivyx-pa card add`, `card list`, `card remove`, `aivyx-pa unlock`, `aivyx-pa lock`, and the `locked:` line under `daemon status`.
- Modify: `docs/manual/reference/04-files-and-paths.md`: a `card-unlock.toml` row (config dir, `0600`, "the wrapped master key per enrolled card; useless without the card; back it up with your config").
- Modify: `docs/guide/` (the guide holding the keyring/passphrase chapter; `grep -ln "keyring set" docs/guide`): a section, "Unlock with a YubiKey".
- Modify: `docs/THREAT_MODEL.md`: a "Card unlock (A18)" entry.
- Create: `docs/checklists/card-unlock-real-card.md` (use the folder the other real-client checklists use, `grep -rln "checklist" docs | head`).
- Modify: `CHANGELOG.md` (Unreleased).

- [ ] **Step 1: Write the docs.**
  - **The guide section** covers:
    1. Install the helper with `aivyx-pa tool-programs install`, and have `pcscd` running.
    2. `aivyx-pa card add`.
    3. Answer yes to removing the saved passphrase.
    4. After the next login the service starts locked: `aivyx-pa unlock`.
    5. `aivyx-pa lock` when stepping away.
    6. Lost card: `aivyx-pa unlock --passphrase`, then `card remove`.
    7. Enroll a second card as a spare.

    It says the passphrase still works, so the user must keep it somewhere safe.
  - **`THREAT_MODEL.md`:**
    - What a card protects: no passphrase on disk; the store can't be opened from a copied disk or a copied `card-unlock.toml` without the card and its PIN.
    - What it doesn't: a running, unlocked daemon (its memory holds the keys); root on the machine (it can read process memory or capture the PIN); anyone who knows the passphrase.
  - **The checklist** follows the spec's manual list:
    1. Enroll a fresh card (generate a key), and a card with an existing decryption key.
    2. Restart the service, check `daemon status` shows `locked: yes`, then `unlock` (PIN and touch).
    3. `lock`, then unlock again.
    4. Unplug the card and run `unlock --passphrase`.
    5. Wrong PIN twice (the tries left are shown), then the right one.
    6. No touch for 15 s ("No touch — still locked").
    7. `card remove`, including the last-card phrase.
    8. `cat ~/.config/aivyx-pa/card-unlock.toml` through PA's `fs.read` tool is refused (Ward).

- [ ] **Step 2: Run the full checks.**
  - Run: `cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`
  - Expected: zero warnings, zero failures, the docs-coverage test included.
  - Run: `just check-web`
  - Expected: the web client still compiles for wasm32.
  - Run: `cargo tree -p aivyx-cli -e normal | grep -c pcsc`
  - Expected: `0`.

- [ ] **Step 3: Commit and push.**

```bash
git add -A docs CHANGELOG.md
git commit -s -m "docs: YubiKey unlock — CLI, files, guide, threat model, real-card checklist (A18)

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
git push
```

- [ ] **Step 4: Run the real-card checklist** on this machine with a YubiKey, with the user present: it needs their PIN and touches. Record the results in the checklist file and commit. Any failure goes back to the task that owns it before release.
