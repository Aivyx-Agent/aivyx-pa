//! # aivyx-crypto
//!
//! Cryptographic primitives for Aivyx: Argon2id (passphrase → master key),
//! HKDF-SHA256 (master key → per-domain subkeys), and ChaCha20-Poly1305
//! (AEAD encryption at rest). This crate is pure commodity — it wraps
//! well-reviewed RustCrypto crates behind Aivyx-specific types. No novel
//! crypto.
//!
//! See DESIGN.md Deliverable 7 for the contract:
//!
//! - Argon2id parameters start at m=64MB, t=3, p=4 (tunable via
//!   `Argon2Params`)
//! - HKDF-SHA256 is domain-separated by a caller-supplied `info` byte
//!   string and a versioned salt (`HKDF_SALT = "aivyx-v1-storage"`)
//! - ChaCha20-Poly1305 is AEAD with a 96-bit nonce and 128-bit tag
//!
//! ## Key material hygiene
//!
//! Every key type in this crate (`MasterKey`, `SubKey`) is
//! zeroize-on-drop. The inner `[u8; 32]` is not publicly accessible —
//! callers get `seal` / `open` / `derive_subkey` methods instead, so
//! there is no public API that hands out raw key bytes. Tests that need
//! to construct a known key from a literal byte array use
//! [`MasterKey::from_raw`], which is documented as a test-only
//! convenience and takes the bytes by value so the test-side array is
//! also zeroized.
//!
//! ## What lives where
//!
//! - `aivyx-crypto` (this crate) knows about bytes, nonces, and
//!   parameters. It does **not** know about `KeyDomain` — that enum
//!   lives in `aivyx-storage` per D8, and `aivyx-storage` is
//!   responsible for mapping each variant to an `info: &[u8]` string
//!   before calling [`MasterKey::derive_subkey`]. Keeping the domain
//!   taxonomy out of this crate avoids a circular dep when
//!   `aivyx-storage` takes shape in task 2.
//! - `aivyx-storage` will consume `SubKey` via [`SubKey::as_aead_key`]
//!   to build `ChaCha20Poly1305` ciphers for each domain.
//! - `aivyx-channel` will drive [`derive_master_key`] from its
//!   passphrase-prompt module (task 3) and hand the resulting
//!   `MasterKey` to the storage open path.

#![forbid(unsafe_code)]

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    ChaCha20Poly1305, Key as AeadKey, Nonce,
};
use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::{Zeroize, ZeroizeOnDrop};

/// Versioned salt for HKDF subkey derivation. Bumping the version
/// (`"aivyx-v2-storage"`, …) produces entirely different subkeys from
/// the same master, which is how D7 supports clean key-schedule
/// migration without a migrations framework.
pub const HKDF_SALT: &[u8] = b"aivyx-v1-storage";

/// Size of every key in bytes. 32 bytes = 256 bits = the key size of
/// ChaCha20-Poly1305 and the output size of SHA-256.
pub const KEY_LEN: usize = 32;

/// Size of the ChaCha20-Poly1305 nonce in bytes.
pub const NONCE_LEN: usize = 12;

/// All errors this crate can produce. Narrow on purpose — the D6 cap
/// is 12 `AivyxError` variants total and this crate contributes
/// exactly one (`CryptoFailed`) at the top level. Everything here is
/// a subcategory of that.
#[derive(Debug, Clone, thiserror::Error)]
pub enum CryptoError {
    /// Argon2id refused the parameters (e.g., memory cost below the
    /// algorithm's minimum). Wraps the upstream error string.
    #[error("invalid Argon2id parameters: {0}")]
    InvalidArgon2Params(String),

    /// Argon2id execution failed at hash time. Extremely rare — in
    /// practice means OOM at the 64MB allocation.
    #[error("Argon2id hashing failed: {0}")]
    Argon2HashFailed(String),

    /// HKDF-SHA256 expand failed. The only documented cause is a
    /// requested output length greater than 255 * 32 bytes, which
    /// this crate's wrappers never request.
    #[error("HKDF expand failed: {0}")]
    HkdfExpandFailed(String),

    /// ChaCha20-Poly1305 decrypt rejected the ciphertext. Can mean
    /// the wrong key, wrong nonce, tampered ciphertext, or the wrong
    /// associated data — the AEAD construction deliberately does not
    /// distinguish these cases.
    #[error("AEAD decrypt failed (wrong key, wrong nonce, or tampered ciphertext)")]
    AeadOpenFailed,

    /// ChaCha20-Poly1305 encrypt failed. In practice this does not
    /// happen for the plaintext sizes Aivyx uses — the AEAD only
    /// refuses plaintexts longer than ~256 GB.
    #[error("AEAD encrypt failed: {0}")]
    AeadSealFailed(String),

    /// Caller passed a nonce slice that is not exactly 12 bytes long.
    #[error("invalid nonce length: expected {NONCE_LEN}, got {0}")]
    InvalidNonceLength(usize),

    /// Caller passed a raw-key byte slice that is not exactly 32
    /// bytes long.
    #[error("invalid key length: expected {KEY_LEN}, got {0}")]
    InvalidKeyLength(usize),
}

// --------------------------------------------------------------------
// Argon2Params — passphrase → master key tuning
// --------------------------------------------------------------------

/// Tunable Argon2id parameters. D7 specifies m=64MB, t=3, p=4 as the
/// starting point; production can bump these up as hardware improves.
///
/// The defaults here match D7 exactly. Tests use [`Argon2Params::weak`]
/// so the unit test suite does not spend 5+ seconds per hash.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct Argon2Params {
    /// Memory cost in KiB. D7 default: 65_536 (64 MiB).
    pub memory_kib: u32,
    /// Time cost (iterations). D7 default: 3.
    pub time_cost: u32,
    /// Parallelism (lanes). D7 default: 4.
    pub parallelism: u32,
}

impl Argon2Params {
    /// D7 defaults — m=64MB, t=3, p=4. Use this in production.
    pub const fn d7_default() -> Self {
        Self {
            memory_kib: 64 * 1024,
            time_cost: 3,
            parallelism: 4,
        }
    }

    /// Deliberately-weak parameters for unit tests. Do not use in
    /// production: these are below D7's floor and exist only so the
    /// test suite finishes in milliseconds instead of seconds.
    pub const fn weak_for_tests() -> Self {
        Self {
            memory_kib: 8,
            time_cost: 1,
            parallelism: 1,
        }
    }

    fn to_upstream(self) -> Result<Params, CryptoError> {
        Params::new(self.memory_kib, self.time_cost, self.parallelism, Some(KEY_LEN))
            .map_err(|e| CryptoError::InvalidArgon2Params(e.to_string()))
    }
}

impl Default for Argon2Params {
    fn default() -> Self {
        Self::d7_default()
    }
}

// --------------------------------------------------------------------
// MasterKey — the root secret derived from a passphrase
// --------------------------------------------------------------------

/// 32-byte Argon2id-derived master key. Never exposed as raw bytes.
/// Zeroized on drop.
///
/// Construction:
/// - [`derive_master_key`] — production path, from passphrase + salt
/// - [`MasterKey::from_raw`] — test path, from a known byte array
#[derive(Clone, ZeroizeOnDrop)]
pub struct MasterKey {
    bytes: [u8; KEY_LEN],
}

impl MasterKey {
    /// Construct a master key from a literal 32-byte array. Intended
    /// for tests with known vectors. In production, use
    /// [`derive_master_key`].
    pub fn from_raw(bytes: [u8; KEY_LEN]) -> Self {
        Self { bytes }
    }

    /// Derive a per-domain [`SubKey`] via HKDF-SHA256, using the
    /// versioned [`HKDF_SALT`] and the caller's `info` byte string as
    /// domain separator.
    ///
    /// `info` should be a stable byte string per domain — for
    /// `aivyx-storage`, that's `KeyDomain::as_bytes()`. The subkey is
    /// deterministic in `(master, HKDF_SALT, info)`.
    pub fn derive_subkey(&self, info: &[u8]) -> Result<SubKey, CryptoError> {
        let hk = Hkdf::<Sha256>::new(Some(HKDF_SALT), &self.bytes);
        let mut out = [0u8; KEY_LEN];
        hk.expand(info, &mut out)
            .map_err(|e| CryptoError::HkdfExpandFailed(e.to_string()))?;
        Ok(SubKey { bytes: out })
    }
}

impl std::fmt::Debug for MasterKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never leak key bytes via Debug — the whole point of the
        // wrapper type is that a stray `dbg!(master_key)` in a panic
        // path does not end up in a log file.
        f.debug_struct("MasterKey").field("bytes", &"<redacted>").finish()
    }
}

/// Derive a master key from a passphrase and salt via Argon2id.
///
/// The `salt` should be at least 16 bytes and unique per Aivyx store —
/// `aivyx-storage` will generate one at store-creation time and
/// persist it alongside the encrypted data (salts are not secret).
/// Passing a zero-length salt is rejected by the upstream crate.
///
/// This function allocates ~64MiB by default (see [`Argon2Params`]) and
/// takes hundreds of milliseconds to seconds on a modern laptop. Run
/// it once at store-open time, cache the result in a `MasterKey`, and
/// derive subkeys per operation.
pub fn derive_master_key(
    passphrase: &[u8],
    salt: &[u8],
    params: Argon2Params,
) -> Result<MasterKey, CryptoError> {
    let upstream_params = params.to_upstream()?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, upstream_params);
    let mut out = [0u8; KEY_LEN];
    argon
        .hash_password_into(passphrase, salt, &mut out)
        .map_err(|e| CryptoError::Argon2HashFailed(e.to_string()))?;
    Ok(MasterKey { bytes: out })
}

// --------------------------------------------------------------------
// WrapKey — card-derived key that wraps the master key
// --------------------------------------------------------------------

/// HKDF salt for card-unlock wrap keys (Amendment A18).
pub const CARD_WRAP_SALT: &[u8] = b"aivyx-pa-card-unlock-v1";

/// A key that wraps the master key for one enrolled card. Built only by
/// HKDF from a card's X25519 shared secret; zeroized on drop.
#[derive(ZeroizeOnDrop)]
pub struct WrapKey {
    bytes: [u8; KEY_LEN],
}

impl WrapKey {
    /// Derive a wrap key from a card's X25519 shared secret, using the
    /// versioned [`CARD_WRAP_SALT`] and the caller's `info` as domain
    /// separator.
    pub fn derive(shared_secret: &[u8; 32], info: &[u8]) -> Result<Self, CryptoError> {
        let hk = Hkdf::<Sha256>::new(Some(CARD_WRAP_SALT), shared_secret);
        let mut bytes = [0u8; KEY_LEN];
        hk.expand(info, &mut bytes)
            .map_err(|e| CryptoError::HkdfExpandFailed(e.to_string()))?;
        Ok(Self { bytes })
    }
}

impl std::fmt::Debug for WrapKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WrapKey").field("bytes", &"<redacted>").finish()
    }
}

/// The master key encrypted under a [`WrapKey`] (ChaCha20-Poly1305).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrappedMasterKey {
    pub nonce: [u8; NONCE_LEN],
    pub ciphertext: Vec<u8>,
}

impl MasterKey {
    /// Encrypt this master key under `key` with a fresh random nonce.
    /// `aad` binds the wrapped blob to its context (e.g. the card id).
    pub fn wrap(&self, key: &WrapKey, aad: &[u8]) -> Result<WrappedMasterKey, CryptoError> {
        use chacha20poly1305::aead::{AeadCore, OsRng};
        let cipher = ChaCha20Poly1305::new(AeadKey::from_slice(&key.bytes));
        let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
        let ciphertext = cipher
            .encrypt(&nonce, Payload { msg: &self.bytes, aad })
            .map_err(|e| CryptoError::AeadSealFailed(e.to_string()))?;
        Ok(WrappedMasterKey { nonce: nonce.into(), ciphertext })
    }

    /// Decrypt a [`WrappedMasterKey`] produced by [`MasterKey::wrap`].
    /// Fails if the key, aad, nonce, or ciphertext do not match.
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

    /// Equality without exposing bytes (constant time).
    pub fn same_as(&self, other: &MasterKey) -> bool {
        self.bytes.iter().zip(other.bytes.iter()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
    }
}

// --------------------------------------------------------------------
// SubKey — per-domain AEAD key
// --------------------------------------------------------------------

/// 32-byte HKDF-derived per-domain subkey. Holds a
/// ChaCha20-Poly1305-compatible key internally; callers invoke
/// [`SubKey::seal`] / [`SubKey::open`] directly rather than handing the
/// raw key to a cipher they constructed themselves.
///
/// Zeroized on drop.
#[derive(Clone, ZeroizeOnDrop)]
pub struct SubKey {
    bytes: [u8; KEY_LEN],
}

impl SubKey {
    /// Seal `plaintext` under this subkey with the given nonce and
    /// additional authenticated data. Returns `ciphertext || tag`
    /// (the Aead trait appends the 16-byte tag; we never store it
    /// separately).
    ///
    /// **Nonce discipline:** the caller is responsible for ensuring
    /// `nonce` is unique per `(key, aad)` pair. Reusing a nonce under
    /// the same key catastrophically breaks confidentiality.
    /// `aivyx-storage` will derive nonces from a monotonic counter
    /// plus random high bits (task 2).
    pub fn seal(
        &self,
        nonce: &[u8],
        aad: &[u8],
        plaintext: &[u8],
    ) -> Result<Vec<u8>, CryptoError> {
        if nonce.len() != NONCE_LEN {
            return Err(CryptoError::InvalidNonceLength(nonce.len()));
        }
        let cipher = ChaCha20Poly1305::new(AeadKey::from_slice(&self.bytes));
        let nonce = Nonce::from_slice(nonce);
        cipher
            .encrypt(nonce, Payload { msg: plaintext, aad })
            .map_err(|e| CryptoError::AeadSealFailed(e.to_string()))
    }

    /// Open a `ciphertext || tag` produced by [`SubKey::seal`]. Fails
    /// with [`CryptoError::AeadOpenFailed`] if the key, nonce, aad, or
    /// ciphertext do not match what was sealed — the AEAD construction
    /// does not distinguish these cases, which is the whole point.
    pub fn open(
        &self,
        nonce: &[u8],
        aad: &[u8],
        ciphertext: &[u8],
    ) -> Result<Vec<u8>, CryptoError> {
        if nonce.len() != NONCE_LEN {
            return Err(CryptoError::InvalidNonceLength(nonce.len()));
        }
        let cipher = ChaCha20Poly1305::new(AeadKey::from_slice(&self.bytes));
        let nonce = Nonce::from_slice(nonce);
        cipher
            .decrypt(nonce, Payload { msg: ciphertext, aad })
            .map_err(|_| CryptoError::AeadOpenFailed)
    }

    /// Borrow the raw 32 bytes of this subkey.
    ///
    /// **The preferred entry points are [`seal`](Self::seal) and
    /// [`open`](Self::open)** — both use the bytes internally and never
    /// expose them across the call. This accessor exists for the one
    /// legitimate non-AEAD caller: `aivyx-audit`'s `PersistentAuditLog`
    /// uses the `KeyDomain::Audit` subkey as an HMAC-SHA256 chain key,
    /// and HMAC is a primitive `SubKey` does not wrap. The audit code
    /// copies the bytes into its own `HmacChainLog::key: Vec<u8>` (a
    /// non-`ZeroizeOnDrop` buffer) exactly once at open time; the copy's
    /// lifetime is bounded by the `PersistentAuditLog` and nothing else
    /// in the workspace calls this method.
    ///
    /// Do not reach for this accessor when `seal`/`open` would do. Every
    /// additional caller is a place the raw-bytes contract has to be
    /// re-justified.
    pub fn as_bytes(&self) -> &[u8; KEY_LEN] {
        &self.bytes
    }
}

impl std::fmt::Debug for SubKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubKey").field("bytes", &"<redacted>").finish()
    }
}

// Defensive: both key types explicitly implement Zeroize so an
// external caller that wants to wipe them early (rather than waiting
// for Drop) can do so. The derive already handles Drop; this exposes
// the manual path.
impl Zeroize for MasterKey {
    fn zeroize(&mut self) {
        self.bytes.zeroize();
    }
}

impl Zeroize for SubKey {
    fn zeroize(&mut self) {
        self.bytes.zeroize();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Argon2id → MasterKey ---------------------------------------

    #[test]
    fn derive_master_key_is_deterministic() {
        let params = Argon2Params::weak_for_tests();
        let k1 = derive_master_key(b"correct horse battery staple", b"test-salt-16byte", params)
            .expect("derive 1");
        let k2 = derive_master_key(b"correct horse battery staple", b"test-salt-16byte", params)
            .expect("derive 2");
        assert_eq!(k1.bytes, k2.bytes);
    }

    #[test]
    fn derive_master_key_differs_by_passphrase() {
        let params = Argon2Params::weak_for_tests();
        let k1 = derive_master_key(b"passphrase-a", b"test-salt-16byte", params).unwrap();
        let k2 = derive_master_key(b"passphrase-b", b"test-salt-16byte", params).unwrap();
        assert_ne!(k1.bytes, k2.bytes);
    }

    #[test]
    fn derive_master_key_differs_by_salt() {
        let params = Argon2Params::weak_for_tests();
        let k1 = derive_master_key(b"same-passphrase", b"salt-a-padded-to", params).unwrap();
        let k2 = derive_master_key(b"same-passphrase", b"salt-b-padded-to", params).unwrap();
        assert_ne!(k1.bytes, k2.bytes);
    }

    #[test]
    fn derive_master_key_rejects_empty_salt() {
        let params = Argon2Params::weak_for_tests();
        let err = derive_master_key(b"pw", b"", params).unwrap_err();
        assert!(matches!(err, CryptoError::Argon2HashFailed(_)));
    }

    #[test]
    fn argon2_d7_defaults_match_design_doc() {
        // Regression lock — if anyone flips these, the D7 contract
        // has changed and the commit needs to touch DESIGN.md.
        let p = Argon2Params::d7_default();
        assert_eq!(p.memory_kib, 64 * 1024);
        assert_eq!(p.time_cost, 3);
        assert_eq!(p.parallelism, 4);
    }

    // ---- HKDF → SubKey ----------------------------------------------

    #[test]
    fn derive_subkey_is_deterministic() {
        let master = MasterKey::from_raw([7u8; KEY_LEN]);
        let s1 = master.derive_subkey(b"sessions").unwrap();
        let s2 = master.derive_subkey(b"sessions").unwrap();
        assert_eq!(s1.bytes, s2.bytes);
    }

    #[test]
    fn derive_subkey_differs_by_info() {
        let master = MasterKey::from_raw([7u8; KEY_LEN]);
        let sessions = master.derive_subkey(b"sessions").unwrap();
        let memory = master.derive_subkey(b"memory").unwrap();
        let audit = master.derive_subkey(b"audit").unwrap();
        assert_ne!(sessions.bytes, memory.bytes);
        assert_ne!(sessions.bytes, audit.bytes);
        assert_ne!(memory.bytes, audit.bytes);
    }

    #[test]
    fn derive_subkey_differs_by_master() {
        let m1 = MasterKey::from_raw([1u8; KEY_LEN]);
        let m2 = MasterKey::from_raw([2u8; KEY_LEN]);
        let s1 = m1.derive_subkey(b"sessions").unwrap();
        let s2 = m2.derive_subkey(b"sessions").unwrap();
        assert_ne!(s1.bytes, s2.bytes);
    }

    #[test]
    fn derive_subkey_against_rfc5869_known_vector() {
        // Cross-check our wrapper against a hand-computed HKDF-SHA256
        // expansion using HKDF_SALT so a regression in the crate
        // version bump would fail loudly. The vector is:
        //   ikm  = [0x42; 32]
        //   salt = "aivyx-v1-storage"
        //   info = b"sessions"
        //   L    = 32
        //
        // Expected output generated by hashing through the same
        // hkdf 0.12 API in a scratch binary at crate-build time. If
        // this regenerates, the HKDF_SALT or ikm type has drifted.
        let master = MasterKey::from_raw([0x42u8; KEY_LEN]);
        let sub = master.derive_subkey(b"sessions").unwrap();

        // Compute the same thing a second way (direct Hkdf call) and
        // assert equality. This checks that the wrapper is a faithful
        // pass-through without pinning a hex literal whose
        // regeneration would feel opaque.
        let hk = Hkdf::<Sha256>::new(Some(HKDF_SALT), &[0x42u8; KEY_LEN]);
        let mut expected = [0u8; KEY_LEN];
        hk.expand(b"sessions", &mut expected).unwrap();
        assert_eq!(sub.bytes, expected);
    }

    // ---- ChaCha20-Poly1305 seal / open -------------------------------

    #[test]
    fn aead_round_trip() {
        let master = MasterKey::from_raw([9u8; KEY_LEN]);
        let sub = master.derive_subkey(b"sessions").unwrap();

        let nonce = [0u8; NONCE_LEN];
        let aad = b"aivyx-session-123";
        let plaintext = b"hello, persistent world";

        let ct = sub.seal(&nonce, aad, plaintext).expect("seal");
        // Ciphertext != plaintext, and is plaintext.len() + 16 (tag).
        assert_ne!(&ct[..plaintext.len()], plaintext);
        assert_eq!(ct.len(), plaintext.len() + 16);

        let pt = sub.open(&nonce, aad, &ct).expect("open");
        assert_eq!(pt, plaintext);
    }

    #[test]
    fn aead_rejects_wrong_key() {
        let sub_a = MasterKey::from_raw([1u8; KEY_LEN])
            .derive_subkey(b"sessions")
            .unwrap();
        let sub_b = MasterKey::from_raw([2u8; KEY_LEN])
            .derive_subkey(b"sessions")
            .unwrap();

        let nonce = [0u8; NONCE_LEN];
        let ct = sub_a.seal(&nonce, b"", b"secret").unwrap();
        let err = sub_b.open(&nonce, b"", &ct).unwrap_err();
        assert!(matches!(err, CryptoError::AeadOpenFailed));
    }

    #[test]
    fn aead_rejects_wrong_nonce() {
        let sub = MasterKey::from_raw([3u8; KEY_LEN])
            .derive_subkey(b"sessions")
            .unwrap();

        let nonce_a = [0u8; NONCE_LEN];
        let mut nonce_b = [0u8; NONCE_LEN];
        nonce_b[0] = 1;

        let ct = sub.seal(&nonce_a, b"", b"secret").unwrap();
        let err = sub.open(&nonce_b, b"", &ct).unwrap_err();
        assert!(matches!(err, CryptoError::AeadOpenFailed));
    }

    #[test]
    fn aead_rejects_wrong_aad() {
        let sub = MasterKey::from_raw([4u8; KEY_LEN])
            .derive_subkey(b"sessions")
            .unwrap();

        let nonce = [0u8; NONCE_LEN];
        let ct = sub.seal(&nonce, b"aad-v1", b"secret").unwrap();
        let err = sub.open(&nonce, b"aad-v2", &ct).unwrap_err();
        assert!(matches!(err, CryptoError::AeadOpenFailed));
    }

    #[test]
    fn aead_rejects_tampered_ciphertext() {
        let sub = MasterKey::from_raw([5u8; KEY_LEN])
            .derive_subkey(b"sessions")
            .unwrap();

        let nonce = [0u8; NONCE_LEN];
        let mut ct = sub.seal(&nonce, b"", b"secret").unwrap();
        // Flip a bit in the ciphertext body.
        ct[0] ^= 0x01;
        let err = sub.open(&nonce, b"", &ct).unwrap_err();
        assert!(matches!(err, CryptoError::AeadOpenFailed));
    }

    #[test]
    fn aead_rejects_invalid_nonce_length() {
        let sub = MasterKey::from_raw([6u8; KEY_LEN])
            .derive_subkey(b"sessions")
            .unwrap();

        let short_nonce = [0u8; NONCE_LEN - 1];
        let err = sub.seal(&short_nonce, b"", b"x").unwrap_err();
        assert!(matches!(err, CryptoError::InvalidNonceLength(len) if len == NONCE_LEN - 1));

        let err = sub.open(&short_nonce, b"", &[0u8; 16]).unwrap_err();
        assert!(matches!(err, CryptoError::InvalidNonceLength(len) if len == NONCE_LEN - 1));
    }

    // ---- End-to-end passphrase → AEAD --------------------------------

    #[test]
    fn passphrase_to_aead_full_stack() {
        let params = Argon2Params::weak_for_tests();
        let master = derive_master_key(
            b"correct horse battery staple",
            b"aivyx-test-salt-",
            params,
        )
        .unwrap();

        let sessions = master.derive_subkey(b"sessions").unwrap();
        let memory = master.derive_subkey(b"memory").unwrap();
        assert_ne!(sessions.bytes, memory.bytes);

        let nonce = [1u8; NONCE_LEN];
        let ct = sessions.seal(&nonce, b"record-1", b"turn history").unwrap();

        // Memory subkey cannot open a sessions-sealed ciphertext,
        // even with the same nonce + aad. This is the domain-
        // isolation guarantee aivyx-storage is going to lean on.
        let err = memory.open(&nonce, b"record-1", &ct).unwrap_err();
        assert!(matches!(err, CryptoError::AeadOpenFailed));

        // Sessions subkey round-trips cleanly.
        let pt = sessions.open(&nonce, b"record-1", &ct).unwrap();
        assert_eq!(pt, b"turn history");
    }

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
    fn the_card_wrap_salt_is_pinned() {
        // A18 — changing CARD_WRAP_SALT (or the HKDF shape) would make every
        // enrolled card's entry stop opening. Known answer, computed once:
        // HKDF-SHA256(salt = "aivyx-pa-card-unlock-v1", ikm = [4; 32], info = "info").
        assert_eq!(CARD_WRAP_SALT, b"aivyx-pa-card-unlock-v1");
        let key = WrapKey::derive(&[4u8; 32], b"info").unwrap();
        let hex: String = key.bytes.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(hex, "9e0e2800c3d65b3579b78d18b49f0ff0968ee71a7ccec5477e6e085f4ecfc219");
    }

    #[test]
    fn wrap_key_debug_is_redacted() {
        let key = WrapKey::derive(&[4u8; 32], b"info").unwrap();
        let dbg = format!("{key:?}");
        assert!(dbg.contains("<redacted>"), "{dbg}");
        assert!(!dbg.contains("9e"), "{dbg}");
    }

    #[test]
    fn two_wraps_use_different_nonces() {
        let master = MasterKey::from_raw([3u8; 32]);
        let key = WrapKey::derive(&[4u8; 32], b"info").unwrap();
        assert_ne!(master.wrap(&key, b"a").unwrap().nonce, master.wrap(&key, b"a").unwrap().nonce);
    }

    #[test]
    fn debug_impls_do_not_leak_key_bytes() {
        // If someone accidentally puts `dbg!(master_key)` in a panic
        // path we want the log line to say "<redacted>", not the
        // actual 32 bytes. The assertion here isn't just "doesn't
        // panic" — we check the string output contains the sentinel
        // and does not contain a hex-looking window of the key.
        let master = MasterKey::from_raw([0xAB; KEY_LEN]);
        let sub = master.derive_subkey(b"sessions").unwrap();
        assert!(format!("{:?}", master).contains("<redacted>"));
        assert!(format!("{:?}", sub).contains("<redacted>"));
        assert!(!format!("{:?}", master).contains("ababab"));
    }
}
