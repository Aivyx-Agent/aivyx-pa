//! Identity (FED.1 / Chapter Passport PP.1) — *who is this agent?*
//!
//! An operator-owned **Ed25519 keypair per instance** (sovereignty: the
//! identity is the operator's), and the **signed, replay-guarded request
//! envelope** every cross-boundary request carries.
//!
//! Lifted from the archived `auth.rs` and modernized onto the new core (PP.0):
//! - errors map to [`crate::FederationError`] (no shared `AivyxError`);
//! - **key-at-rest is sealed with a subkey derived from the storage
//!   [`MasterKey`](aivyx_crypto::MasterKey)** via the new core's audited HKDF +
//!   ChaCha20-Poly1305 ([`SubKey::seal`](aivyx_crypto::SubKey::seal)), replacing
//!   the salvage's hand-rolled AEAD.
//!
//! Preserved invariants: the manual [`Debug`] that **redacts the signing key**,
//! `0o600` key-file permissions, and `instance_id` charset validation — *key
//! material is never logged* (FED.0 §7).

use std::path::Path;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use ed25519_dalek::{Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

use aivyx_crypto::{MasterKey, NONCE_LEN};

use crate::FederationError;

/// Maximum age of a signed request before it is considered stale.
const MAX_REQUEST_AGE_SECS: u64 = 60;

/// How far into the future a header's timestamp may be before it's rejected
/// outright, rather than accepted as merely "not yet expired". Without this
/// bound, `now.saturating_sub(header.timestamp)` on an artificially
/// future-dated header saturates to `0` and the header would never expire
/// (a small allowance covers legitimate clock skew between peers).
const FUTURE_SKEW_TOLERANCE_SECS: u64 = 5;

/// Maximum number of nonces the replay guard's `seen` set may hold before it
/// evicts the oldest entry to bound memory. Without this cap, an
/// authenticated peer sending a flood of distinct (but individually valid)
/// requests grows the set forever.
const MAX_REPLAY_GUARD_ENTRIES: usize = 10_000;

/// HKDF `info` that derives the federation identity-key-wrapping subkey from the
/// storage [`MasterKey`]. Versioned so a future rotation is a new label, never a
/// silent reinterpretation of old bytes.
const KEY_WRAP_INFO: &[u8] = b"aivyx-federation-identity-key-v1";

/// AEAD additional-authenticated-data binding the sealed key to its purpose.
const KEY_WRAP_AAD: &[u8] = b"aivyx-federation-identity";

/// Encrypted key-file layout: `nonce(12) || ciphertext+tag(48)`.
const ENCRYPTED_KEY_LEN: usize = NONCE_LEN + 48;

/// A signed federation request header (FED.0 §2). Travels with every
/// cross-boundary request; verified against the peer's known public key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignedHeader {
    /// The sender's instance id.
    pub instance_id: String,
    /// Unix timestamp (secs) when the request was signed.
    pub timestamp: u64,
    /// Base64 Ed25519 signature over `"{instance_id}:{timestamp}:{body_hash}"`.
    pub signature: String,
}

/// Replay guard — remembers recently seen `instance_id:timestamp:signature`
/// nonces and rejects duplicates within the freshness window. Call
/// [`check_and_record`](ReplayGuard::check_and_record) **after** signature
/// verification succeeds.
///
/// **Process-local, not persisted.** This guard's state lives entirely in
/// memory; a process restart reopens a full `MAX_REQUEST_AGE_SECS`-wide
/// replay window for every nonce recorded before the restart (an attacker
/// who captured a request just before a restart can replay it right after
/// one). Deliberately out of scope for the 2026-09-16 security-audit fix
/// that hardened this guard's in-memory eviction/skew/cap logic — this
/// primitive isn't wired into any shipped transport yet (see
/// `docs/FEDERATION.md` §2). **Revisit this before wiring `ReplayGuard`
/// into a real transport** — either persist the seen-set (e.g. a small
/// on-disk/redb-backed table, since the window is short enough that only
/// recent entries matter) or accept the restart-reopens-the-window
/// limitation explicitly in that transport's own threat model.
pub struct ReplayGuard {
    /// nonce -> the header's own `timestamp` field (**not** the wall-clock
    /// instant this guard happened to record it at). Keying off the
    /// header's own timestamp, rather than insertion time, is what makes
    /// this guard's retention window identical *by construction* to
    /// [`Identity::verify_request`]'s acceptance window: that function
    /// accepts a header with timestamp `T` for any `now` in
    /// `[T - FUTURE_SKEW_TOLERANCE_SECS, T + MAX_REQUEST_AGE_SECS]`
    /// (inclusive both ends), so a nonce recorded here must remain
    /// rejectable as a replay for exactly that same span -- no more, no
    /// less. (An earlier version keyed off insertion time with a strict
    /// `<` sweep bound instead of `<=`, which was narrower than
    /// `verify_request`'s own inclusive bound and let a captured header be
    /// replayed exactly once more, right at the boundary.)
    ///
    /// A map rather than a set specifically so eviction can be per-entry
    /// (each nonce ages out `MAX_REQUEST_AGE_SECS` after *its own* header
    /// timestamp) instead of a global periodic clear, which used to give a
    /// nonce recorded just before the clear boundary up to that whole
    /// window of extra replayability.
    seen: Mutex<std::collections::HashMap<String, u64>>,
}

impl ReplayGuard {
    pub fn new() -> Self {
        Self {
            seen: Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Record this header's nonce; `Err` if it was already seen (a replay).
    pub fn check_and_record(&self, header: &SignedHeader) -> Result<(), FederationError> {
        self.check_and_record_at(header, now_secs())
    }

    /// [`Self::check_and_record`] with an explicit `now`, so tests can drive
    /// per-entry expiry across what used to be a flush boundary without
    /// sleeping in real time.
    fn check_and_record_at(&self, header: &SignedHeader, now: u64) -> Result<(), FederationError> {
        let nonce = format!(
            "{}:{}:{}",
            header.instance_id, header.timestamp, header.signature
        );
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());

        // Check this nonce's own expiry lazily, on lookup, instead of
        // sweeping the whole map on every single call (a full `retain` plus
        // `min_by_key`'s own O(n) scan on every request, unconditionally,
        // is up to ~20,000 iterations per call while holding this guard's
        // only mutex -- reachable at the cap, since
        // MAX_REPLAY_GUARD_ENTRIES nonces per MAX_REQUEST_AGE_SECS is only
        // ~167 req/s). Keyed off the header's own `timestamp` (the value
        // stored below), matching `verify_request`'s inclusive boundary
        // exactly (`<=`, not `<`) -- see this struct's doc comment on
        // `seen` for why the two must match exactly. A future-dated-but-
        // within-skew header can have `header.timestamp > now`, in which
        // case `now.saturating_sub(existing_ts)` floors to `0`, which is
        // `<= MAX_REQUEST_AGE_SECS`, so it correctly stays rejectable as a
        // replay rather than being treated as already-expired.
        if let Some(&existing_ts) = seen.get(&nonce) {
            if now.saturating_sub(existing_ts) <= MAX_REQUEST_AGE_SECS {
                return Err(FederationError::Identity("replayed federation request".into()));
            }
        }

        // Only run the full expiry sweep -- to actually reclaim memory --
        // once we're at the cap, rather than unconditionally on every call;
        // the lazy per-nonce check above already gives correct replay
        // rejection regardless of whether a stale entry has been swept out
        // of the map yet. This does mean reclamation is deliberately
        // deferred, not eager: a peer that sends a burst of distinct
        // nonces and then goes quiet leaves them resident in the map
        // indefinitely (long past their own MAX_REQUEST_AGE_SECS
        // expiry) until *some* caller's traffic pushes the map back up to
        // the cap again. Correctness is unaffected (the lazy check above
        // is authoritative regardless of how stale the map is), and
        // memory stays bounded by the cap either way -- this is a
        // deliberate latency/memory trade-off, not an oversight.
        if seen.len() >= MAX_REPLAY_GUARD_ENTRIES {
            seen.retain(|_, &mut ts| now.saturating_sub(ts) <= MAX_REQUEST_AGE_SECS);
        }

        if seen.len() >= MAX_REPLAY_GUARD_ENTRIES {
            // The sweep above didn't free enough room (every remaining
            // entry is still fresh) -- bound memory by dropping the single
            // oldest entry (by its own header timestamp) rather than
            // refusing a legitimately-fresh new request outright.
            //
            // Security trade-off, stated explicitly rather than left
            // implicit: oldest-first eviction means a peer capable of
            // flooding more than MAX_REPLAY_GUARD_ENTRIES distinct,
            // signature-valid nonces within the freshness window could
            // evict another peer's recently-recorded nonce before its
            // natural expiry, and then replay that other peer's captured
            // request. This is accepted as a deliberate trade-off: the
            // alternative -- fail closed and reject new, legitimate
            // requests once the map is full -- hands that same flooding
            // attacker a trivial denial-of-service instead, which is
            // worse. Don't mistake this cap for security-neutral.
            if let Some(oldest) = seen.iter().min_by_key(|(_, t)| *t).map(|(k, _)| k.clone()) {
                seen.remove(&oldest);
            }
        }

        seen.insert(nonce, header.timestamp);
        Ok(())
    }
}

impl Default for ReplayGuard {
    fn default() -> Self {
        Self::new()
    }
}

/// Abstraction over "something that can hardware-sign like a YubiKey".
///
/// Exists purely as an internal storage/dispatch seam so this crate's own
/// tests can exercise [`IdentitySigner::Hardware`]'s dispatch and
/// [`Identity::load_hardware`]'s serial-mismatch rejection without real
/// YubiKey hardware. `aivyx_yubi::YubiKeySigner`'s own public API has no way
/// to construct a hardware-backed instance without either real PC/SC
/// hardware discovery (`YubiKeySigner::new`) or `aivyx-yubi`'s private,
/// crate-internal-only test seams (`from_open_card`/`discover_and_construct`
/// — not `pub`, not reachable from outside that crate even under its
/// `test-util` feature) — see that crate's `src/sign.rs`. `Identity::
/// load_hardware`'s public signature still takes a concrete
/// `aivyx_yubi::YubiKeySigner` directly (matching what Task 8's CLI
/// constructs); this trait never appears in this crate's public API.
#[cfg(feature = "yubikey")]
trait HardwareSigner: Send + Sync {
    fn sign(&self, message: &[u8]) -> Result<[u8; 64], String>;
    fn public_key(&self) -> [u8; 32];
    fn card_serial(&self) -> &str;
}

#[cfg(feature = "yubikey")]
impl HardwareSigner for aivyx_yubi::YubiKeySigner {
    fn sign(&self, message: &[u8]) -> Result<[u8; 64], String> {
        aivyx_yubi::YubiKeySigner::sign(self, message).map_err(|e| e.to_string())
    }

    fn public_key(&self) -> [u8; 32] {
        aivyx_yubi::YubiKeySigner::public_key(self)
    }

    fn card_serial(&self) -> &str {
        aivyx_yubi::YubiKeySigner::card_serial(self)
    }
}

/// The two backends an [`Identity`] can sign with. Never exposed publicly —
/// callers only ever see [`Identity`]'s own methods.
enum IdentitySigner {
    /// A software-generated key, sealed at rest under the storage
    /// [`MasterKey`] (see [`Identity::load_or_generate`]). Boxed: `SigningKey`
    /// is >200 bytes, and without this the whole enum pays that size for
    /// every `Hardware` instance too (`clippy::large_enum_variant`).
    Software(Box<SigningKey>),
    /// A hardware-backed signer (a YubiKey's OpenPGP card applet, or, in
    /// this crate's own tests, a fake — see [`HardwareSigner`]'s doc
    /// comment). `Arc`, not `Box`: [`Identity::sign_request`]'s hardware arm
    /// moves a clone of this into a [`tokio::task::spawn_blocking`] closure
    /// (the touch-confirmation wait is a genuinely blocking call and must
    /// not run on the calling async executor thread), which needs an owned,
    /// `'static` handle rather than a borrow of `&self`.
    #[cfg(feature = "yubikey")]
    Hardware(std::sync::Arc<dyn HardwareSigner>),
}

impl IdentitySigner {
    /// The software-backed key, or an error if this is a hardware-backed
    /// signer (which has no software key to hand back). Split into two
    /// `#[cfg]`'d bodies rather than one `match` with a `#[cfg]`'d arm:
    /// with the `yubikey` feature off, `IdentitySigner` has only the
    /// `Software` variant, so a `match` here would be clippy's
    /// `infallible_destructuring_match` (a `match` used to destructure a
    /// pattern that can't fail) under `-D warnings` — a plain, genuinely
    /// irrefutable `let` is the correct shape for that configuration, and a
    /// real two-armed `match` is the correct shape once the feature adds a
    /// second variant.
    #[cfg(feature = "yubikey")]
    fn as_software(&self) -> Result<&SigningKey, FederationError> {
        match self {
            IdentitySigner::Software(k) => Ok(k.as_ref()),
            IdentitySigner::Hardware(_) => Err(FederationError::Identity(
                "cannot seal a hardware-backed identity's key to disk (no software key exists)"
                    .into(),
            )),
        }
    }

    #[cfg(not(feature = "yubikey"))]
    fn as_software(&self) -> Result<&SigningKey, FederationError> {
        let IdentitySigner::Software(k) = self;
        Ok(k.as_ref())
    }
}

/// An operator-owned Ed25519 federation identity: `instance_id` + keypair,
/// backed by either a software key or a hardware signer (see
/// [`IdentitySigner`]).
///
/// `Debug` is implemented manually so the signing key is **never** rendered.
pub struct Identity {
    instance_id: String,
    signer: IdentitySigner,
    verifying_key: VerifyingKey,
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Redact just as thoroughly for the hardware path as for software:
        // no PIN state or key material is ever printed. `card_serial()`
        // isn't itself secret, but the conservative posture this file
        // documents (FED.0 §7) applies to the signer as a whole, not just
        // the software case — never print anything from `self.signer` here.
        f.debug_struct("Identity")
            .field("instance_id", &self.instance_id)
            .field("signer", &"[redacted]")
            .field("verifying_key", &self.public_key_base64())
            .finish()
    }
}

impl Identity {
    /// Build from an existing signing key. Validates `instance_id`.
    pub fn new(instance_id: String, signing_key: SigningKey) -> Result<Self, FederationError> {
        validate_instance_id(&instance_id)?;
        let verifying_key = signing_key.verifying_key();
        Ok(Self {
            instance_id,
            signer: IdentitySigner::Software(Box::new(signing_key)),
            verifying_key,
        })
    }

    /// Generate a fresh random keypair for `instance_id`.
    pub fn generate(instance_id: String) -> Result<Self, FederationError> {
        let mut rng = rand::thread_rng();
        Self::new(instance_id, SigningKey::generate(&mut rng))
    }

    /// Build an `Identity` backed by a YubiKey's OpenPGP card applet instead
    /// of a software-generated key. `signer` must already be constructed —
    /// obtaining the User PIN and calling `aivyx_yubi::YubiKeySigner::new`
    /// is the caller's responsibility (Task 8's CLI, not this crate); this
    /// keeps `aivyx-federation` from needing to know anything about PIN
    /// acquisition UX. `expected_serial` is the card serial this identity
    /// was originally provisioned against (persisted by that same caller) —
    /// if the currently connected card's serial doesn't match, this fails
    /// loudly rather than silently trusting a different physical device.
    #[cfg(feature = "yubikey")]
    pub fn load_hardware(
        instance_id: String,
        signer: aivyx_yubi::YubiKeySigner,
        expected_serial: &str,
    ) -> Result<Self, FederationError> {
        Self::from_hardware_signer(instance_id, std::sync::Arc::new(signer), expected_serial)
    }

    /// The shared core of [`Self::load_hardware`], generic over
    /// [`HardwareSigner`] so this crate's own tests can exercise it against
    /// a fake (see [`HardwareSigner`]'s doc comment for why a real
    /// `aivyx_yubi::YubiKeySigner` can't be fabricated in a test).
    #[cfg(feature = "yubikey")]
    fn from_hardware_signer(
        instance_id: String,
        signer: std::sync::Arc<dyn HardwareSigner>,
        expected_serial: &str,
    ) -> Result<Self, FederationError> {
        validate_instance_id(&instance_id)?;
        if signer.card_serial() != expected_serial {
            return Err(FederationError::Hardware(format!(
                "wrong YubiKey inserted: expected card serial {expected_serial}, found {}",
                signer.card_serial()
            )));
        }
        let public_key_bytes = signer.public_key();
        let verifying_key = VerifyingKey::from_bytes(&public_key_bytes).map_err(|e| {
            FederationError::Hardware(format!("invalid Ed25519 key from card: {e}"))
        })?;
        Ok(Self {
            instance_id,
            signer: IdentitySigner::Hardware(signer),
            verifying_key,
        })
    }

    /// Test-only mirror of [`Self::load_hardware`] that accepts any
    /// [`HardwareSigner`] (a fake, in practice) instead of a concrete
    /// `aivyx_yubi::YubiKeySigner` — see [`HardwareSigner`]'s doc comment
    /// for why the real type can't be constructed in a test.
    #[cfg(all(test, feature = "yubikey"))]
    fn load_hardware_for_test(
        instance_id: String,
        signer: impl HardwareSigner + 'static,
        expected_serial: &str,
    ) -> Result<Self, FederationError> {
        Self::from_hardware_signer(instance_id, std::sync::Arc::new(signer), expected_serial)
    }

    /// This instance's public key as base64 — what a peer records to verify us.
    pub fn public_key_base64(&self) -> String {
        BASE64.encode(self.verifying_key.as_bytes())
    }

    /// This instance's id.
    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    /// Sign a request body, producing a [`SignedHeader`]. Async because the
    /// hardware-backed path can legitimately block for several seconds
    /// waiting on a physical touch — callers must `.await` this even though
    /// the software-backed path completes synchronously in practice. Takes
    /// `&self` (not `&mut self`): neither backend mutates any state on
    /// `self` while signing (`aivyx_yubi::YubiKeySigner::sign` is itself
    /// `&self` for exactly this reason — see its own doc comment), so
    /// there's no need to force callers into a `Mutex<Identity>` to hold a
    /// `&mut` across a multi-second `.await`.
    pub async fn sign_request(&self, body: &[u8]) -> Result<SignedHeader, FederationError> {
        let timestamp = now_secs();
        let body_hash = sha256_hex(body);
        let message = format!("{}:{}:{}", self.instance_id, timestamp, body_hash);
        let signature_bytes = match &self.signer {
            IdentitySigner::Software(k) => k.sign(message.as_bytes()).to_bytes(),
            #[cfg(feature = "yubikey")]
            IdentitySigner::Hardware(h) => {
                // The real signing call blocks the calling thread for the
                // whole touch-confirmation window (can be several seconds)
                // — `spawn_blocking` moves it off whatever executor thread
                // called `sign_request`, so a `current_thread` runtime
                // (several of this project's own CLI subcommands use one)
                // isn't frozen for the duration. `Arc::clone` gives the
                // closure an owned, `'static` handle without needing `&mut
                // self` or disturbing `&self`'s borrow across the `.await`.
                let signer = std::sync::Arc::clone(h);
                let message = message.clone();
                tokio::task::spawn_blocking(move || signer.sign(message.as_bytes()))
                    .await
                    .map_err(|e| {
                        FederationError::Hardware(format!(
                            "hardware signing task panicked or was cancelled: {e}"
                        ))
                    })?
                    .map_err(FederationError::Hardware)?
            }
        };
        Ok(SignedHeader {
            instance_id: self.instance_id.clone(),
            timestamp,
            signature: BASE64.encode(signature_bytes),
        })
    }

    /// Verify a peer's signed request: fresh timestamp + valid signature over
    /// `id:timestamp:body_hash` under `peer_public_key` (base64). Pair with a
    /// [`ReplayGuard`] to reject replays within the freshness window.
    pub fn verify_request(
        peer_public_key: &str,
        header: &SignedHeader,
        body: &[u8],
    ) -> Result<(), FederationError> {
        Self::verify_request_at(peer_public_key, header, body, now_secs())
    }

    /// [`Self::verify_request`] with an explicit `now`, so a boundary test
    /// can pin an exact instant instead of racing the real wall clock —
    /// `verify_request` itself calls `now_secs()` once and delegates here,
    /// so this is the one real check both the production path and tests
    /// exercise, not a parallel copy.
    fn verify_request_at(
        peer_public_key: &str,
        header: &SignedHeader,
        body: &[u8],
        now: u64,
    ) -> Result<(), FederationError> {
        let is_too_old = now.saturating_sub(header.timestamp) > MAX_REQUEST_AGE_SECS;
        let is_too_far_in_future = header.timestamp > now.saturating_add(FUTURE_SKEW_TOLERANCE_SECS);
        if is_too_old || is_too_far_in_future {
            return Err(FederationError::Identity("federation request expired".into()));
        }
        let key_bytes = BASE64
            .decode(peer_public_key)
            .map_err(|e| FederationError::Identity(format!("invalid peer public key: {e}")))?;
        let key_array: [u8; 32] = key_bytes
            .try_into()
            .map_err(|_| FederationError::Identity("peer public key must be 32 bytes".into()))?;
        let verifying_key = VerifyingKey::from_bytes(&key_array)
            .map_err(|e| FederationError::Identity(format!("invalid Ed25519 key: {e}")))?;
        let sig_bytes = BASE64
            .decode(&header.signature)
            .map_err(|e| FederationError::Identity(format!("invalid signature encoding: {e}")))?;
        let sig_array: [u8; 64] = sig_bytes
            .try_into()
            .map_err(|_| FederationError::Identity("signature must be 64 bytes".into()))?;
        let signature = ed25519_dalek::Signature::from_bytes(&sig_array);
        let body_hash = sha256_hex(body);
        let message = format!("{}:{}:{}", header.instance_id, header.timestamp, body_hash);
        verifying_key
            .verify(message.as_bytes(), &signature)
            .map_err(|_| FederationError::Identity("federation signature verification failed".into()))
    }

    /// Load the identity from an encrypted key file, or generate + seal + save
    /// it if absent. The key at rest is sealed with a subkey derived from
    /// `master` (the storage [`MasterKey`]) — same root secret as the redb
    /// store, so the operator's identity is as protected as their data. The
    /// file is `nonce(12) || ciphertext+tag(48)`, mode `0o600`.
    pub fn load_or_generate(
        instance_id: String,
        key_path: &Path,
        master: &MasterKey,
    ) -> Result<Self, FederationError> {
        let subkey = master
            .derive_subkey(KEY_WRAP_INFO)
            .map_err(|e| FederationError::Identity(format!("derive identity wrap key: {e}")))?;

        if key_path.exists() {
            let data = std::fs::read(key_path)
                .map_err(|e| FederationError::Identity(format!("read federation key: {e}")))?;
            if data.len() != ENCRYPTED_KEY_LEN {
                return Err(FederationError::Identity(format!(
                    "federation key file has unexpected size {} (expected {ENCRYPTED_KEY_LEN})",
                    data.len()
                )));
            }
            let plaintext = subkey
                .open(&data[..NONCE_LEN], KEY_WRAP_AAD, &data[NONCE_LEN..])
                .map_err(|_| FederationError::Identity("federation key decryption failed".into()))?;
            let key_bytes: [u8; 32] = plaintext
                .try_into()
                .map_err(|_| FederationError::Identity("decrypted key is not 32 bytes".into()))?;
            Self::new(instance_id, SigningKey::from_bytes(&key_bytes))
        } else {
            let identity = Self::generate(instance_id)?;
            identity.save_sealed(key_path, &subkey)?;
            Ok(identity)
        }
    }

    /// Seal the signing key under `subkey` and write `nonce||ciphertext` at
    /// `0o600`. Only ever called from [`Self::load_or_generate`] right after
    /// [`Self::generate`], so `self.signer` is always [`IdentitySigner::
    /// Software`] in practice — a hardware-backed `Identity` has no software
    /// key to seal, so that case returns an error rather than panicking.
    fn save_sealed(
        &self,
        key_path: &Path,
        subkey: &aivyx_crypto::SubKey,
    ) -> Result<(), FederationError> {
        let signing_key = self.signer.as_software()?;
        if let Some(parent) = key_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| FederationError::Identity(format!("create key dir: {e}")))?;
        }
        let mut nonce = [0u8; NONCE_LEN];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut nonce);
        let ciphertext = subkey
            .seal(&nonce, KEY_WRAP_AAD, &signing_key.to_bytes())
            .map_err(|e| FederationError::Identity(format!("seal federation key: {e}")))?;
        let mut file_data = Vec::with_capacity(ENCRYPTED_KEY_LEN);
        file_data.extend_from_slice(&nonce);
        file_data.extend_from_slice(&ciphertext);
        std::fs::write(key_path, &file_data)
            .map_err(|e| FederationError::Identity(format!("write federation key: {e}")))?;
        set_file_permissions_600(key_path)?;
        Ok(())
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(data))
}

/// Owner-only (`0o600`) on Unix; no-op elsewhere — keeps the private key
/// unreadable by other users on the host.
#[cfg(unix)]
fn set_file_permissions_600(path: &Path) -> Result<(), FederationError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| FederationError::Identity(format!("set key file permissions: {e}")))
}

#[cfg(not(unix))]
fn set_file_permissions_600(_path: &Path) -> Result<(), FederationError> {
    Ok(())
}

/// Instance ids appear in signed headers, audit payloads, and logs — restrict
/// to ASCII alphanumeric + `-`/`_` so they can't inject into any of those.
///
/// `pub` (not private): this is the one authoritative validation rule for an
/// `instance_id`, and callers that need to reject a bad id *before* doing
/// expensive or irreversible work of their own (e.g. `aivyx-cli`'s
/// `federation yubikey-init`, which must refuse a malformed instance id
/// before touching a YubiKey at all — see that command's own doc comment)
/// should call this directly rather than duplicating the character-class
/// rule inline.
pub fn validate_instance_id(id: &str) -> Result<(), FederationError> {
    if id.is_empty() {
        return Err(FederationError::Validation(
            "federation instance_id must not be empty".into(),
        ));
    }
    if !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(FederationError::Validation(format!(
            "federation instance_id contains invalid characters: '{id}' \
             (only ASCII alphanumeric, hyphens, and underscores allowed)"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_key_path(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("aivyx-fed-id-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("federation.key")
    }

    /// A fake [`HardwareSigner`] standing in for a real `aivyx_yubi::
    /// YubiKeySigner` — see that trait's doc comment for why the real type
    /// can't be fabricated from outside `aivyx-yubi`.
    #[cfg(feature = "yubikey")]
    struct FakeHardwareSigner {
        card_serial: String,
        public_key: [u8; 32],
        signing_key: SigningKey,
        fail_with: Option<String>,
    }

    #[cfg(feature = "yubikey")]
    impl FakeHardwareSigner {
        fn new(card_serial: &str) -> Self {
            let mut rng = rand::thread_rng();
            let signing_key = SigningKey::generate(&mut rng);
            let public_key = signing_key.verifying_key().to_bytes();
            Self {
                card_serial: card_serial.to_string(),
                public_key,
                signing_key,
                fail_with: None,
            }
        }

        /// Make `sign()` fail with `message`, simulating a `YubiError`
        /// (e.g. a touch timeout or PIN rejection) instead of producing a
        /// real signature.
        fn failing(mut self, message: &str) -> Self {
            self.fail_with = Some(message.to_string());
            self
        }
    }

    #[cfg(feature = "yubikey")]
    impl HardwareSigner for FakeHardwareSigner {
        fn sign(&self, message: &[u8]) -> Result<[u8; 64], String> {
            if let Some(err) = &self.fail_with {
                return Err(err.clone());
            }
            Ok(self.signing_key.sign(message).to_bytes())
        }

        fn public_key(&self) -> [u8; 32] {
            self.public_key
        }

        fn card_serial(&self) -> &str {
            &self.card_serial
        }
    }

    #[tokio::test]
    async fn sign_and_verify_roundtrips() {
        let id = Identity::generate("test-instance".into()).unwrap();
        let body = b"hello federation";
        let header = id.sign_request(body).await.unwrap();
        Identity::verify_request(&id.public_key_base64(), &header, body)
            .expect("verification should pass");
    }

    #[tokio::test]
    async fn rejects_tampered_body() {
        let id = Identity::generate("test-instance".into()).unwrap();
        let header = id.sign_request(b"original body").await.unwrap();
        assert!(Identity::verify_request(&id.public_key_base64(), &header, b"tampered").is_err());
    }

    #[tokio::test]
    async fn rejects_expired_request() {
        let id = Identity::generate("test-instance".into()).unwrap();
        let mut header = id.sign_request(b"body").await.unwrap();
        header.timestamp -= MAX_REQUEST_AGE_SECS + 10;
        assert!(Identity::verify_request(&id.public_key_base64(), &header, b"body").is_err());
    }

    #[tokio::test]
    async fn rejects_wrong_peer_key() {
        let a = Identity::generate("instance-a".into()).unwrap();
        let b = Identity::generate("instance-b".into()).unwrap();
        let header = a.sign_request(b"secret").await.unwrap();
        // verifying A's signature with B's key must fail
        assert!(Identity::verify_request(&b.public_key_base64(), &header, b"secret").is_err());
    }

    #[tokio::test]
    async fn signature_is_64_bytes_and_header_is_well_formed() {
        let id = Identity::generate("struct-test".into()).unwrap();
        let header = id.sign_request(b"payload").await.unwrap();
        assert_eq!(header.instance_id, "struct-test");
        assert_eq!(BASE64.decode(&header.signature).unwrap().len(), 64);
    }

    #[tokio::test]
    async fn replay_guard_rejects_second_use() {
        let id = Identity::generate("replay".into()).unwrap();
        let header = id.sign_request(b"once").await.unwrap();
        let guard = ReplayGuard::new();
        assert!(guard.check_and_record(&header).is_ok());
        assert!(guard.check_and_record(&header).is_err(), "replay must be rejected");
    }

    #[test]
    fn nonce_replay_is_rejected_up_to_its_own_expiry_boundary_then_admitted_again() {
        // Directly pins the positive per-entry expiry property (and, with
        // it, Finding 1's exact boundary fix): a nonce recorded for a header
        // timestamped at `t` must still be rejected as a replay all the way
        // up to *and including* `t + MAX_REQUEST_AGE_SECS` -- the same
        // inclusive instant `verify_request` itself would still (barely)
        // accept that header at -- and only stop being rejected one tick
        // past that.
        //
        // Uses a large fixed constant for `t` (not small values like
        // 59/61) specifically so `now.saturating_sub(t)` can never
        // accidentally saturate to 0 and mask a real bug -- the failure
        // mode the previous version of this test could not actually catch.
        // Header is fabricated directly (not signed via `Identity::
        // sign_request`) so the timestamp used for the expiry math is under
        // this test's exact control, independent of real wall-clock time.
        let t: u64 = 2_000_000_000;
        let header = SignedHeader {
            instance_id: "expiry-boundary".into(),
            timestamp: t,
            signature: "sig-expiry-boundary".into(),
        };
        let guard = ReplayGuard::new();

        assert!(
            guard.check_and_record_at(&header, t).is_ok(),
            "first use at t must be recorded"
        );
        assert!(
            guard.check_and_record_at(&header, t + MAX_REQUEST_AGE_SECS - 1).is_err(),
            "replay one tick before the boundary must still be rejected"
        );
        assert!(
            guard.check_and_record_at(&header, t + MAX_REQUEST_AGE_SECS).is_err(),
            "replay at exactly t + MAX_REQUEST_AGE_SECS must still be rejected -- \
             verify_request's own `is_too_old` check uses strict `>`, so a header \
             this old is still accepted there too; the guard must match that \
             inclusive boundary exactly, not expire one tick early"
        );
        assert!(
            guard.check_and_record_at(&header, t + MAX_REQUEST_AGE_SECS + 1).is_ok(),
            "one tick past the boundary, this is no longer flagged as a replay: \
             verify_request would now reject this same header as too old on its \
             own, so the guard has correctly let it age out rather than \
             remembering it forever"
        );
    }

    #[tokio::test]
    async fn a_future_dated_header_is_rejected_not_treated_as_permanently_fresh() {
        // Under the old `saturating_sub` freshness check, a header timestamped
        // in the future saturates the age computation to 0, so it reads as
        // maximally fresh forever -- it would never expire. A future-skew
        // bound must reject it outright instead.
        let id = Identity::generate("future-dated".into()).unwrap();
        let mut header = id.sign_request(b"body").await.unwrap();
        header.timestamp = now_secs() + 3600; // 1 hour in the future
        let result = Identity::verify_request(&id.public_key_base64(), &header, b"body");
        assert!(
            result.is_err(),
            "a header timestamped an hour in the future must be rejected"
        );
    }

    /// Signs `body` with a caller-chosen `timestamp` instead of
    /// `sign_request`'s own live `now_secs()`, so boundary tests can pin an
    /// exact timestamp while still producing a signature that verifies
    /// against it. Reaches into `Identity`'s private fields/methods, which
    /// is fine: `tests` is a descendant module of this file's module.
    fn sign_at(id: &Identity, body: &[u8], timestamp: u64) -> SignedHeader {
        let body_hash = sha256_hex(body);
        let message = format!("{}:{}:{}", id.instance_id, timestamp, body_hash);
        let signing_key = id
            .signer
            .as_software()
            .expect("test identities are always software-backed");
        let signature = signing_key.sign(message.as_bytes()).to_bytes();
        SignedHeader {
            instance_id: id.instance_id.clone(),
            timestamp,
            signature: BASE64.encode(signature),
        }
    }

    #[test]
    fn future_skew_boundary_is_pinned_exactly() {
        // Minor 3: the test above only checks a header deep in the future
        // (an hour), well past any boundary -- pin the actual off-by-one
        // instead: a header exactly FUTURE_SKEW_TOLERANCE_SECS ahead of now
        // is accepted (clock-skew tolerance), one second further is
        // rejected.
        //
        // Review round 3 (Minor B): the first version of this test called
        // `now_secs()` once for the "now" baseline and let `verify_request`
        // call `now_secs()` again internally for each assertion -- if a
        // real wall-clock second boundary fell between those calls (a rare
        // but real ~100us window), the two `now` values would differ by 1,
        // silently shifting both boundary checks and occasionally flipping
        // the "rejected" assertion to "accepted". Driving both assertions
        // from the same `now` via `verify_request_at` removes the race
        // entirely -- this test can no longer flake on wall-clock timing.
        let id = Identity::generate("future-boundary".into()).unwrap();
        let body: &[u8] = b"body";
        let now = now_secs();

        let within_tolerance = sign_at(&id, body, now + FUTURE_SKEW_TOLERANCE_SECS);
        Identity::verify_request_at(&id.public_key_base64(), &within_tolerance, body, now)
            .expect("a header exactly FUTURE_SKEW_TOLERANCE_SECS ahead of now must be accepted");

        let one_past_tolerance = sign_at(&id, body, now + FUTURE_SKEW_TOLERANCE_SECS + 1);
        assert!(
            Identity::verify_request_at(&id.public_key_base64(), &one_past_tolerance, body, now)
                .is_err(),
            "a header one second past FUTURE_SKEW_TOLERANCE_SECS ahead of now must be rejected"
        );
    }

    #[test]
    fn the_seen_set_evicts_the_oldest_entries_first_and_stays_bounded() {
        // Review round 3 (Minor A): a strictly-increasing *call-time* `now`
        // (the prior version of this test) lets the age-based `retain`
        // sweep reclaim the whole backlog the moment the map first reaches
        // the cap -- once that happens `len` never returns to the cap
        // again, so the size-based `min_by_key` eviction branch never
        // executes even once, and this test could pass under a mutant that
        // evicts the *newest* entry, or even one with the size-cap eviction
        // block deleted outright (verified by hand-simulating both
        // mutants against the previous version of this test: neither
        // failed it).
        //
        // Fixed by decoupling "what's fresh" from "what's distinct": every
        // call passes the *same* `now` (so `now.saturating_sub(ts)`
        // saturates to 0 -- well within the freshness window -- for every
        // entry regardless of its own `header.timestamp`, meaning the
        // age-based sweep can never reclaim anything here), while each
        // entry's own stored `header.timestamp` still increases
        // per-insertion (so `min_by_key`'s ordering has a genuine, checkable
        // "oldest" to find). This forces every eviction past the cap to go
        // through `min_by_key`, and lets this test actually distinguish
        // oldest-first from any other order.
        let guard = ReplayGuard::new();
        let total = MAX_REPLAY_GUARD_ENTRIES + 50;
        let now = 0u64;
        for i in 0..total {
            let header = SignedHeader {
                instance_id: "cap-test".into(),
                timestamp: i as u64,
                signature: format!("sig-{i}"),
            };
            guard
                .check_and_record_at(&header, now)
                .expect("each nonce here is distinct, so none should be a replay");
        }

        let seen = guard.seen.lock().unwrap();
        assert!(
            seen.len() <= MAX_REPLAY_GUARD_ENTRIES,
            "seen set grew to {}, exceeding the {MAX_REPLAY_GUARD_ENTRIES} cap",
            seen.len()
        );
        let earliest_nonce = "cap-test:0:sig-0".to_string();
        assert!(
            !seen.contains_key(&earliest_nonce),
            "the very-earliest nonce must have been evicted -- oldest-first, \
             not newest-first or some other order"
        );
        let latest_index = total - 1;
        let latest_nonce = format!("cap-test:{latest_index}:sig-{latest_index}");
        assert!(
            seen.contains_key(&latest_nonce),
            "the most-recently-inserted nonce must still be present"
        );
    }

    #[test]
    fn debug_redacts_the_signing_key() {
        let id = Identity::generate("redact-test".into()).unwrap();
        let dbg = format!("{id:?}");
        assert!(dbg.contains("[redacted]"));
        assert!(dbg.contains("redact-test"));
        // the raw signing-key bytes must never appear. Reuses
        // `IdentitySigner::as_software` (see its doc comment) rather than
        // destructuring here directly, for the same infallible-match
        // reason.
        let signing_key = id
            .signer
            .as_software()
            .expect("Identity::generate should always produce a software-backed signer");
        let raw = format!("{:?}", signing_key.to_bytes());
        assert!(!dbg.contains(&raw));
    }

    #[cfg(feature = "yubikey")]
    #[test]
    fn debug_redacts_the_hardware_signer() {
        let fake = FakeHardwareSigner::new("0006:00112233");
        let id = Identity::load_hardware_for_test("hw-redact".into(), fake, "0006:00112233")
            .unwrap();
        let dbg = format!("{id:?}");
        assert!(dbg.contains("[redacted]"));
        assert!(dbg.contains("hw-redact"));
        // the card serial (not secret, but conservatively redacted anyway
        // per FED.0 §7 -- see `Identity`'s manual `Debug` impl) must not
        // leak through the signer field.
        assert!(!dbg.contains("0006:00112233"));
    }

    #[cfg(feature = "yubikey")]
    #[tokio::test]
    async fn hardware_backed_sign_request_verifies_like_the_software_path() {
        let fake = FakeHardwareSigner::new("0006:00112233");
        let id =
            Identity::load_hardware_for_test("hw-instance".into(), fake, "0006:00112233").unwrap();
        let header = id.sign_request(b"hello from hardware").await.unwrap();
        Identity::verify_request(&id.public_key_base64(), &header, b"hello from hardware")
            .expect("a hardware-backed signature must verify identically to a software one");
    }

    #[cfg(feature = "yubikey")]
    #[test]
    fn load_hardware_rejects_a_mismatched_card_serial() {
        let fake = FakeHardwareSigner::new("0006:00112233");
        let err = Identity::load_hardware_for_test(
            "hw-mismatch".into(),
            fake,
            "0006:99999999", // a different serial than the fake reports
        )
        .expect_err("a mismatched card serial must be rejected");
        assert!(
            matches!(err, FederationError::Hardware(_)),
            "expected FederationError::Hardware, got {err:?}"
        );
    }

    #[cfg(feature = "yubikey")]
    #[tokio::test]
    async fn hardware_signing_failure_maps_to_federation_error_hardware() {
        let fake = FakeHardwareSigner::new("0006:00112233").failing("touch timeout");
        let id =
            Identity::load_hardware_for_test("hw-fails".into(), fake, "0006:00112233").unwrap();
        let err = id
            .sign_request(b"anything")
            .await
            .expect_err("a hardware signing failure must propagate as an Err, not a panic");
        match err {
            FederationError::Hardware(msg) => assert_eq!(msg, "touch timeout"),
            other => panic!("expected FederationError::Hardware, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn encrypted_key_roundtrips_via_masterkey() {
        let master = MasterKey::from_raw([7u8; 32]);
        let path = tmp_key_path("rt");
        // generate + seal
        let a = Identity::load_or_generate("enc".into(), &path, &master).unwrap();
        let pubkey = a.public_key_base64();
        // file is exactly nonce(12)+ciphertext+tag(48)
        assert_eq!(std::fs::read(&path).unwrap().len(), ENCRYPTED_KEY_LEN);
        // reload yields the same identity + signing still verifies
        let b = Identity::load_or_generate("enc".into(), &path, &master).unwrap();
        assert_eq!(b.public_key_base64(), pubkey);
        let header = b.sign_request(b"x").await.unwrap();
        Identity::verify_request(&pubkey, &header, b"x").unwrap();
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn wrong_masterkey_cannot_open_the_identity() {
        let master = MasterKey::from_raw([7u8; 32]);
        let wrong = MasterKey::from_raw([9u8; 32]);
        let path = tmp_key_path("wrong");
        Identity::load_or_generate("enc".into(), &path, &master).unwrap();
        assert!(Identity::load_or_generate("enc".into(), &path, &wrong).is_err());
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[cfg(unix)]
    #[test]
    fn key_file_is_600() {
        use std::os::unix::fs::PermissionsExt;
        let master = MasterKey::from_raw([7u8; 32]);
        let path = tmp_key_path("perms");
        Identity::load_or_generate("perms".into(), &path, &master).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "key file must be 0o600, got {mode:o}");
        std::fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn rejects_empty_and_malformed_instance_ids() {
        assert!(Identity::generate(String::new()).is_err());
        assert!(Identity::generate("bad id/slash".into()).is_err());
        assert!(Identity::generate("good-id_42".into()).is_ok());
    }
}
