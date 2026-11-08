//! Passphrase → `MasterKey` derivation for the local CLI channel.
//!
//! ## What this module owns
//!
//! D7 (LOCKED 2026-04-13) commits that the passphrase prompt lives in
//! the **channel adapter**, not the storage layer: `aivyx-storage`
//! never sees a raw passphrase. This module is the channel-side half
//! of that contract for the `LocalChannel` / CLI binary. It:
//!
//! 1. Sources a passphrase from one of several places (env var, test
//!    fixture, or an interactive TTY prompt via `rpassword`).
//! 2. Loads (or on first run, generates + persists) a 16-byte salt
//!    from a sidecar file next to the redb store.
//! 3. Runs Argon2id through [`aivyx_crypto::derive_master_key`] to
//!    produce a [`aivyx_crypto::MasterKey`].
//! 4. Zeroizes the transient passphrase buffer before returning.
//!
//! The **only** value that crosses this module's boundary is the
//! `MasterKey` — the passphrase bytes live inside a single `Vec<u8>`
//! that is explicitly zeroized between derivation and return. Any
//! future code that wants the passphrase back (rotation, change-
//! passphrase flow) will have to re-source it, which is the point.
//!
//! ## Why a sidecar salt file?
//!
//! Argon2id needs a per-store salt to defeat rainbow tables. The salt
//! cannot live inside the encrypted redb store — the master key is
//! needed to open the store, and the salt is needed to derive the
//! master key, which is a chicken-and-egg. Salts are not secret
//! (that's the whole point of Argon2's design), so they can live in
//! a plaintext sidecar file without compromising the security model.
//!
//! The sidecar path convention: if the redb store is `store.redb`
//! the salt file is `store.redb.salt`. The binary in task 4 will
//! adopt this convention; tests construct both paths explicitly.
//!
//! ## Source selection — binary decides
//!
//! Phase 7 task 4 lit up `PassphraseSource::InteractivePrompt` with a
//! real `rpassword::prompt_password` call, but the *policy* for
//! which source to pick lives in the `aivyx-pa` binary, not in this
//! module. The binary checks `AIVYX_PA_PASSPHRASE` first (non-empty →
//! `Env`), then `stdin().is_terminal()` as a hint (true →
//! `InteractivePrompt`), then bails. Keeping the decision outside
//! the library means `fetch_passphrase_bytes` is still a pure
//! function of its input enum and stays unit-testable in isolation —
//! the `Env` tests never have to fake a tty, and the
//! `InteractivePrompt` tests inject a `BufRead` via
//! `rpassword::prompt_password_from_bufread` rather than trying to
//! intercept `/dev/tty`.

use std::fs;
use std::path::{Path, PathBuf};

use aivyx_crypto::{derive_master_key as crypto_derive_master_key, Argon2Params, MasterKey};
use secrecy::{ExposeSecret, SecretString};
use zeroize::Zeroize;

/// Length of the Argon2id salt in bytes. 16 is the OWASP-recommended
/// floor for password hashing and matches every Argon2 reference
/// implementation in the RustCrypto ecosystem.
pub const SALT_LEN: usize = 16;

/// Default env var name for the env-sourced passphrase. The binary
/// and tests refer to this constant rather than hardcoding the
/// string, so a rename stays a one-line edit.
pub const DEFAULT_ENV_VAR: &str = "AIVYX_PA_PASSPHRASE";

// --------------------------------------------------------------------
// Errors
// --------------------------------------------------------------------

/// Errors this module produces. All fold into the existing
/// `AivyxError::Crypto` / `AivyxError::Config` slots at the
/// `aivyx-core` boundary — no new top-level variant needed.
#[derive(Debug, thiserror::Error)]
pub enum PassphraseError {
    /// `AIVYX_PA_PASSPHRASE` (or the caller's chosen env var) was not
    /// set when a `PassphraseSource::Env` was requested.
    #[error("passphrase env var `{0}` is not set")]
    EnvNotSet(String),

    /// `AIVYX_PA_PASSPHRASE` was set but empty. Rejected explicitly
    /// because Argon2id happily hashes an empty input and the
    /// resulting key would be trivially brute-forceable.
    #[error("passphrase env var `{0}` is empty")]
    EnvEmpty(String),

    /// Reading or writing the salt sidecar file failed. Wraps
    /// `std::io::Error::to_string()` to avoid dragging the whole
    /// `io::Error` type across the module boundary.
    #[error("salt sidecar I/O failed at {path:?}: {reason}")]
    SaltIo { path: PathBuf, reason: String },

    /// The salt sidecar file exists but is not exactly [`SALT_LEN`]
    /// bytes long. This means the file was written by a different
    /// tool, truncated, or corrupted.
    #[error("salt sidecar at {path:?} is {len} bytes, expected {SALT_LEN}")]
    SaltMalformed { path: PathBuf, len: usize },

    /// Argon2id or the upstream crypto layer refused the derivation.
    /// In practice this only happens if [`Argon2Params`] is set to a
    /// value below Argon2's minimum (e.g., memory_kib = 0).
    #[error("crypto derivation failed: {0}")]
    Crypto(#[from] aivyx_crypto::CryptoError),

    /// The interactive TTY prompt failed — `/dev/tty` was not
    /// available, the read was interrupted, or the underlying
    /// terminal I/O errored. Wraps `std::io::Error::to_string()`
    /// to avoid dragging the whole `io::Error` type across the
    /// module boundary.
    #[error("interactive passphrase prompt failed: {reason}")]
    InteractiveIo { reason: String },

    /// The interactive prompt read a zero-length password. Rejected
    /// explicitly for the same reason [`PassphraseError::EnvEmpty`]
    /// is rejected: Argon2id happily hashes an empty input and the
    /// resulting key would be trivially brute-forceable.
    #[error("interactive passphrase prompt returned an empty password")]
    InteractiveEmpty,
}

// --------------------------------------------------------------------
// PassphraseSource
// --------------------------------------------------------------------

/// Where to source the passphrase bytes.
///
/// The production binary picks between [`PassphraseSource::Env`]
/// and [`PassphraseSource::InteractivePrompt`] based on whether
/// `AIVYX_PA_PASSPHRASE` is set and whether stdin is a terminal; tests
/// use [`PassphraseSource::Fixture`] to inject known bytes without
/// touching the environment or a tty, or drive `InteractivePrompt`
/// via the `rpassword::prompt_password_from_bufread` seam under the
/// hood.
pub enum PassphraseSource {
    /// Read from a named environment variable. Fails with
    /// [`PassphraseError::EnvNotSet`] if unset or
    /// [`PassphraseError::EnvEmpty`] if set to "". Pass
    /// [`DEFAULT_ENV_VAR`] for the standard `AIVYX_PA_PASSPHRASE` name.
    Env { var_name: String },

    /// Phase 51 Task 4 — passphrase supplied by the config loader.
    ///
    /// Previously `[aivyx] passphrase` in TOML was parsed by
    /// `aivyx-config` but ignored at derivation time:
    /// `select_passphrase_source` returned `Env` even when the
    /// config had a value, so a TOML-only setup errored with
    /// "passphrase env var not set." This variant closes that
    /// inconsistency — the binary picks `FromConfig` whenever
    /// the loader produced a passphrase, and the derive path
    /// uses the secret directly.
    ///
    /// The `SecretString` is consumed on first use; its contents
    /// are zeroized after the master key is derived.
    FromConfig(SecretString),

    /// Test-only: invoke a caller-supplied closure to produce the
    /// passphrase bytes. The closure is called once and its return
    /// value is zeroized after the master key is derived. Production
    /// code should not use this variant — there's nothing stopping
    /// it, but the idiomatic binary wiring is `Env` or `FromConfig`.
    Fixture(Box<dyn FnOnce() -> Vec<u8> + Send>),

    /// Interactive TTY prompt via `rpassword::prompt_password`.
    /// Opens `/dev/tty` directly on Unix (so it works even when
    /// stdin is piped, as long as a controlling terminal exists),
    /// reads a single line with echo disabled, strips the trailing
    /// newline, and zeroizes the internal buffer on the way out.
    /// Returns [`PassphraseError::InteractiveIo`] if `/dev/tty` is
    /// not reachable, or [`PassphraseError::InteractiveEmpty`] if
    /// the user pressed enter on an empty line.
    ///
    /// `confirm` — when `true` (creating a brand-new store), prompts
    /// twice and requires a match before returning, re-prompting the
    /// whole pair on mismatch; mirrors `aivyx-pa keyring set`'s existing
    /// prompt+confirm+match-check shape. When `false` (unlocking an
    /// existing store), behavior is unchanged from before this field
    /// existed: one prompt, no confirmation — a wrong guess there
    /// fails cleanly at decrypt time and the user just retries the
    /// command, so there's nothing to protect against a typo for.
    InteractivePrompt { confirm: bool },
}

impl std::fmt::Debug for PassphraseSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            // Name variants only — never even hint at what the
            // closure might return.
            PassphraseSource::Env { var_name } => f
                .debug_struct("Env")
                .field("var_name", var_name)
                .finish(),
            PassphraseSource::FromConfig(_) => {
                // Never even hint at the secret contents.
                f.debug_struct("FromConfig").finish_non_exhaustive()
            }
            PassphraseSource::Fixture(_) => {
                f.debug_struct("Fixture").finish_non_exhaustive()
            }
            PassphraseSource::InteractivePrompt { confirm } => f
                .debug_struct("InteractivePrompt")
                .field("confirm", confirm)
                .finish(),
        }
    }
}

// --------------------------------------------------------------------
// Salt sidecar
// --------------------------------------------------------------------

/// Load the salt from `salt_path` if it exists, otherwise generate
/// a fresh 16-byte salt from `Uuid::new_v4()` and persist it to the
/// same path.
///
/// The salt is written atomically in the "simple" sense: since this
/// is a 16-byte file, a single `fs::write` either succeeds fully or
/// leaves no file at all on every filesystem this binary supports.
/// No temp-and-rename ritual is needed at this size.
///
/// **Phase 7 task 6 — permissions.** On first-run (the `NotFound`
/// arm), the freshly-created salt file is `chmod 0600`'d before the
/// function returns. The earlier doc claim "salts are not secret" is
/// still true in the cryptographic sense — knowledge of the salt
/// does not shortcut Argon2id — but the uniform "every file aivyx-pa
/// writes looks the same to an auditor" discipline from Task 6
/// applies anyway. The chmod only runs on the create path: if a
/// caller has deliberately re-permed an existing salt file, a warm
/// reopen does not silently fight them back to 0600.
pub fn load_or_create_salt(salt_path: &Path) -> Result<[u8; SALT_LEN], PassphraseError> {
    match fs::read(salt_path) {
        Ok(bytes) => {
            if bytes.len() != SALT_LEN {
                return Err(PassphraseError::SaltMalformed {
                    path: salt_path.to_path_buf(),
                    len: bytes.len(),
                });
            }
            let mut salt = [0u8; SALT_LEN];
            salt.copy_from_slice(&bytes);
            Ok(salt)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // First-run path. Generate from Uuid::new_v4() (same
            // entropy source aivyx-storage uses for AEAD nonces)
            // and persist plaintext.
            let uuid_bytes = *uuid::Uuid::new_v4().as_bytes(); // 16 bytes
            fs::write(salt_path, uuid_bytes).map_err(|e| PassphraseError::SaltIo {
                path: salt_path.to_path_buf(),
                reason: e.to_string(),
            })?;
            // Phase 7 task 6 — chmod 0600 the freshly-created salt.
            // Failure is a hard error (policy: if we can't prove
            // 0600, we must not pretend we did). See `chmod_user_only`
            // for the cross-platform shape.
            chmod_user_only(salt_path).map_err(|e| PassphraseError::SaltIo {
                path: salt_path.to_path_buf(),
                reason: format!("failed to chmod 0600: {e}"),
            })?;
            Ok(uuid_bytes)
        }
        Err(e) => Err(PassphraseError::SaltIo {
            path: salt_path.to_path_buf(),
            reason: e.to_string(),
        }),
    }
}

/// Phase 7 task 6 — `chmod 0600` the given path on Unix, no-op on
/// other platforms. Intentionally duplicated with the sibling in
/// `aivyx-storage::lib` per Q6a: two ~5-line helpers are cheaper than
/// a new `aivyx-core` public surface, and the logic is stable. Both
/// copies should stay byte-identical in shape; if they diverge, that's
/// a bug, not a feature.
#[cfg(unix)]
fn chmod_user_only(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn chmod_user_only(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

// --------------------------------------------------------------------
// derive_master_key
// --------------------------------------------------------------------

/// Source a passphrase from `source`, load or create the salt at
/// `salt_path`, and derive a [`MasterKey`] via Argon2id.
///
/// The transient passphrase `Vec<u8>` is explicitly zeroized between
/// the Argon2id call returning and this function returning, so the
/// raw bytes do not linger on the heap past master-key derivation.
///
/// # Errors
///
/// - [`PassphraseError::EnvNotSet`] / [`PassphraseError::EnvEmpty`]
///   if `source` is `Env` and the variable is missing or empty
/// - [`PassphraseError::SaltIo`] / [`PassphraseError::SaltMalformed`]
///   if the salt sidecar can't be read, written, or is the wrong size
/// - [`PassphraseError::Crypto`] if Argon2id refuses the params
/// - [`PassphraseError::InteractiveNotImplemented`] if `source` is
///   `InteractivePrompt`
pub fn derive_master_key(
    source: PassphraseSource,
    salt_path: &Path,
    params: Argon2Params,
) -> Result<MasterKey, PassphraseError> {
    let mut passphrase = fetch_passphrase_bytes(source)?;
    let salt = load_or_create_salt(salt_path)?;

    // `?` is fine here — the transient `passphrase` Vec zeroes on
    // drop regardless of return path, but only because of the
    // explicit `zeroize()` below + the immediately-following drop.
    // A panic would leave the Vec on the stack until unwind, which
    // is acceptable; this is a best-effort hygiene step, not a
    // hard guarantee.
    let result = crypto_derive_master_key(&passphrase, &salt, params);

    passphrase.zeroize();
    drop(passphrase);

    Ok(result?)
}

fn fetch_passphrase_bytes(source: PassphraseSource) -> Result<Vec<u8>, PassphraseError> {
    match source {
        PassphraseSource::Env { var_name } => match std::env::var(&var_name) {
            Ok(s) if s.is_empty() => Err(PassphraseError::EnvEmpty(var_name)),
            Ok(s) => Ok(s.into_bytes()),
            Err(_) => Err(PassphraseError::EnvNotSet(var_name)),
        },
        PassphraseSource::FromConfig(secret) => {
            // Read the secret into an owned Vec<u8>. The SecretString
            // itself zeroizes on drop; we additionally zeroize the
            // intermediate clone via the standard derive path
            // (`derive_master_key` already zeroizes the Vec after
            // hashing).
            let bytes = secret.expose_secret().as_bytes().to_vec();
            if bytes.is_empty() {
                // Same posture as EnvEmpty: empty passphrase is
                // refused outright. Argon2id would happily hash it.
                Err(PassphraseError::EnvEmpty("config:[storage]passphrase".into()))
            } else {
                Ok(bytes)
            }
        }
        PassphraseSource::Fixture(f) => Ok(f()),
        PassphraseSource::InteractivePrompt { confirm: false } => {
            // `rpassword::prompt_password` opens `/dev/tty` on Unix,
            // echoes the prompt, reads one line with echo disabled,
            // and returns a `String`. Under `cargo test` there's no
            // controlling tty, so the unit test path routes through
            // `read_interactive_password_inner` with a `BufRead` +
            // `Write` seam instead (see
            // `interactive_source_reads_password_from_bufread`).
            read_interactive_password_inner(|| {
                rpassword::prompt_password("aivyx-pa passphrase: ")
            })
        }
        PassphraseSource::InteractivePrompt { confirm: true } => {
            // Creating a brand-new store — prompt twice and require a
            // match, the same shape `aivyx-pa keyring set` already uses.
            // See `read_interactive_password_with_confirm`.
            read_interactive_password_with_confirm(|prompt| {
                rpassword::prompt_password(prompt)
            })
        }
    }
}

/// Shared empty-check + I/O-error-mapping step for a single
/// interactive read. Factored out of `read_interactive_password_inner`
/// so `read_interactive_password_with_confirm` (below) can reuse it
/// for each of its two reads without duplicating the mapping logic.
fn map_read_result(result: std::io::Result<String>) -> Result<Vec<u8>, PassphraseError> {
    let pass = result.map_err(|e| PassphraseError::InteractiveIo { reason: e.to_string() })?;
    if pass.is_empty() {
        return Err(PassphraseError::InteractiveEmpty);
    }
    // `String::into_bytes` hands over the existing heap allocation
    // — no copy — so the outer zeroize path owns the one-and-only
    // persistent copy of the passphrase bytes.
    Ok(pass.into_bytes())
}

/// Shared plumbing for the interactive path. Takes a `read`
/// closure that produces the passphrase `String` (or an I/O error),
/// applies the empty-check + error-mapping discipline, and returns
/// a `Vec<u8>` the outer `derive_master_key` zeroize path can take
/// ownership of.
///
/// In production `read` wraps `rpassword::prompt_password` (real
/// `/dev/tty`). In unit tests it wraps
/// `rpassword::prompt_password_from_bufread` against an in-memory
/// `&[u8]` reader and a sink writer, so the test can assert the
/// full InteractivePrompt → Argon2id → MasterKey round-trip without
/// needing a tty.
fn read_interactive_password_inner<F>(read: F) -> Result<Vec<u8>, PassphraseError>
where
    F: FnOnce() -> std::io::Result<String>,
{
    map_read_result(read())
}

/// New-store passphrase entry: prompts, prompts again, and requires a
/// match before returning — the confirm-reentry counterpart to
/// `read_interactive_password_inner`'s single unconfirmed prompt.
/// Loops (re-prompting both) on mismatch, matching this codebase's
/// existing "loop forever on invalid input" idiom
/// (`aivyx_modules::init::prompt_yes_no`).
///
/// Takes one `FnMut(&str) -> io::Result<String>` closure (parameterized
/// by the prompt text) rather than two separate closures — two closures
/// each capturing the same test reader/writer would violate the borrow
/// checker, since both would need to exist simultaneously as function
/// arguments. One closure invoked twice sequentially avoids that.
///
/// This closure-per-prompt shape also doubles as this function's only
/// output channel: on a mismatch, the retry notice ("Passphrases
/// didn't match. Try again.") is folded into the *next* prompt string
/// rather than printed separately, so it's observable through the same
/// seam every other prompt already uses — no separate `eprintln!`, no
/// new parameter.
fn read_interactive_password_with_confirm<F>(mut read: F) -> Result<Vec<u8>, PassphraseError>
where
    F: FnMut(&str) -> std::io::Result<String>,
{
    const FIRST_PROMPT: &str = "aivyx-pa passphrase (new store — you'll need this every time): ";
    const RETRY_PROMPT: &str = concat!(
        "Passphrases didn't match. Try again.\n",
        "aivyx-pa passphrase (new store — you'll need this every time): "
    );
    let mut prompt = FIRST_PROMPT;
    loop {
        let first = map_read_result(read(prompt))?;
        let mut confirm = map_read_result(read("Confirm passphrase: "))?;
        if first == confirm {
            confirm.zeroize();
            return Ok(first);
        }
        // Mismatch: neither copy is going anywhere near a master key,
        // so zeroize both before looping — same discipline the
        // invariant above requires for every passphrase byte that
        // ever touches the heap.
        let mut first = first;
        first.zeroize();
        confirm.zeroize();
        prompt = RETRY_PROMPT;
    }
}

// --------------------------------------------------------------------
// Tests
// --------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Env vars are process-global state. Run every env-touching test
    // under a single mutex so `cargo test` parallelism can't make
    // one test's unset race with another's set. This is the same
    // pattern aivyx-audit uses for its HMAC-key env fixture.
    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: Mutex<()> = Mutex::new(());
        LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// RAII temp dir — same `$TMPDIR + uuid` pattern aivyx-storage
    /// and aivyx-core::tools::fs use, to avoid adding `tempfile`.
    struct TestDir {
        dir: PathBuf,
    }

    impl TestDir {
        fn new() -> Self {
            let tmp = std::env::var("TMPDIR")
                .or_else(|_| std::env::var("TEMP"))
                .unwrap_or_else(|_| "/tmp".to_string());
            let dir = PathBuf::from(tmp)
                .join(format!("aivyx-passphrase-test-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&dir).expect("test dir must be creatable");
            TestDir { dir }
        }

        fn salt(&self) -> PathBuf {
            self.dir.join("store.redb.salt")
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn fixture(bytes: &'static [u8]) -> PassphraseSource {
        PassphraseSource::Fixture(Box::new(move || bytes.to_vec()))
    }

    // ---- Salt sidecar -----------------------------------------------

    #[test]
    fn salt_is_generated_on_first_call_and_persisted() {
        let dir = TestDir::new();
        let salt_path = dir.salt();

        assert!(!salt_path.exists(), "precondition: salt file missing");
        let s1 = load_or_create_salt(&salt_path).unwrap();
        assert_eq!(s1.len(), SALT_LEN);
        assert!(salt_path.exists(), "salt file must be created on first call");

        // Second call reads the same bytes back, does not regenerate.
        let s2 = load_or_create_salt(&salt_path).unwrap();
        assert_eq!(s1, s2);
    }

    #[test]
    fn salt_differs_between_independent_stores() {
        let dir_a = TestDir::new();
        let dir_b = TestDir::new();
        let s_a = load_or_create_salt(&dir_a.salt()).unwrap();
        let s_b = load_or_create_salt(&dir_b.salt()).unwrap();
        // Random from Uuid::new_v4 — the collision probability is
        // 2^-128, small enough to treat this assertion as a
        // determinism sanity check.
        assert_ne!(s_a, s_b);
    }

    #[test]
    fn salt_rejects_wrong_length_file() {
        let dir = TestDir::new();
        let salt_path = dir.salt();
        // Write 8 bytes instead of 16.
        fs::write(&salt_path, [0u8; 8]).unwrap();
        let err = load_or_create_salt(&salt_path).unwrap_err();
        assert!(matches!(
            err,
            PassphraseError::SaltMalformed { len: 8, .. }
        ));
    }

    // ---- Phase 7 task 6: filesystem permission hardening -----------

    #[cfg(unix)]
    #[test]
    fn fresh_salt_is_chmod_0600() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TestDir::new();
        let salt_path = dir.salt();
        assert!(
            !salt_path.exists(),
            "precondition: salt path must not exist"
        );

        let _salt = load_or_create_salt(&salt_path).unwrap();

        let meta = fs::metadata(&salt_path).expect("salt file must exist");
        let mode = meta.permissions().mode() & 0o777;
        assert_eq!(
            mode, 0o600,
            "freshly-created salt must be chmod 0600, got 0o{mode:o}"
        );

        // Warm reopen must not fight a deliberately re-permed file.
        // Same cold-only policy as the storage side of Task 6 (Q6d).
        fs::set_permissions(&salt_path, fs::Permissions::from_mode(0o640))
            .expect("chmod pre-mutation must succeed");
        let _salt2 = load_or_create_salt(&salt_path).unwrap();
        let mode2 = fs::metadata(&salt_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(
            mode2, 0o640,
            "warm reopen must not force perms back to 0600, got 0o{mode2:o}"
        );
    }

    // ---- Env var source ---------------------------------------------

    #[test]
    fn env_source_success_derives_master_key() {
        let _lock = env_lock();
        let dir = TestDir::new();
        // SAFETY: `set_var` is safe under our env_lock serialization.
        unsafe {
            std::env::set_var("AIVYX_PA_PASSPHRASE_TEST_OK", "correct horse battery staple");
        }
        let master = derive_master_key(
            PassphraseSource::Env {
                var_name: "AIVYX_PA_PASSPHRASE_TEST_OK".to_string(),
            },
            &dir.salt(),
            Argon2Params::weak_for_tests(),
        )
        .expect("env source must succeed");
        // Prove the key actually round-trips by deriving a subkey —
        // if Argon2 silently produced zeros, HKDF would still work
        // but the Debug redaction check in aivyx-crypto already
        // catches that, so here we just confirm no error.
        let _sub = master.derive_subkey(b"sessions").unwrap();
        unsafe {
            std::env::remove_var("AIVYX_PA_PASSPHRASE_TEST_OK");
        }
    }

    #[test]
    fn env_source_not_set_fails_cleanly() {
        let _lock = env_lock();
        let dir = TestDir::new();
        // SAFETY: `remove_var` is safe under the env_lock.
        unsafe {
            std::env::remove_var("AIVYX_PA_PASSPHRASE_TEST_MISSING");
        }
        let err = derive_master_key(
            PassphraseSource::Env {
                var_name: "AIVYX_PA_PASSPHRASE_TEST_MISSING".to_string(),
            },
            &dir.salt(),
            Argon2Params::weak_for_tests(),
        )
        .unwrap_err();
        assert!(matches!(err, PassphraseError::EnvNotSet(ref v) if v == "AIVYX_PA_PASSPHRASE_TEST_MISSING"));
    }

    #[test]
    fn env_source_empty_fails_cleanly() {
        let _lock = env_lock();
        let dir = TestDir::new();
        unsafe {
            std::env::set_var("AIVYX_PA_PASSPHRASE_TEST_EMPTY", "");
        }
        let err = derive_master_key(
            PassphraseSource::Env {
                var_name: "AIVYX_PA_PASSPHRASE_TEST_EMPTY".to_string(),
            },
            &dir.salt(),
            Argon2Params::weak_for_tests(),
        )
        .unwrap_err();
        assert!(matches!(err, PassphraseError::EnvEmpty(ref v) if v == "AIVYX_PA_PASSPHRASE_TEST_EMPTY"));
        unsafe {
            std::env::remove_var("AIVYX_PA_PASSPHRASE_TEST_EMPTY");
        }
    }

    // ---- FromConfig source (Phase 51 Task 4) ------------------------
    //
    // The PassphraseSource::FromConfig variant is what makes the
    // [storage] passphrase TOML field actually drive derivation. Phase
    // 47 visual-pass discovered a footgun: TOML was parsed but the
    // binary required AIVYX_PA_PASSPHRASE in env anyway. Phase 51
    // fixes it; these tests pin the fix.

    #[test]
    fn from_config_source_derives_master_key() {
        use secrecy::SecretString;
        let dir = TestDir::new();
        let master = derive_master_key(
            PassphraseSource::FromConfig(SecretString::from("toml-passphrase".to_string())),
            &dir.salt(),
            Argon2Params::weak_for_tests(),
        )
        .expect("FromConfig source must succeed");
        let _sub = master
            .derive_subkey(b"sessions")
            .expect("derive_subkey from a FromConfig-derived master must work");
    }

    #[test]
    fn from_config_and_env_produce_same_master_key_for_same_bytes() {
        use secrecy::SecretString;
        let _lock = env_lock();
        let dir = TestDir::new();
        let phrase = "shared-bytes-1234";

        // Derive via FromConfig.
        let master_from_config = derive_master_key(
            PassphraseSource::FromConfig(SecretString::from(phrase.to_string())),
            &dir.salt(),
            Argon2Params::weak_for_tests(),
        )
        .expect("FromConfig");

        // Derive via Env over the same salt sidecar.
        unsafe {
            std::env::set_var("AIVYX_PA_PASSPHRASE_FROM_CONFIG_EQUIV", phrase);
        }
        let master_from_env = derive_master_key(
            PassphraseSource::Env {
                var_name: "AIVYX_PA_PASSPHRASE_FROM_CONFIG_EQUIV".to_string(),
            },
            &dir.salt(),
            Argon2Params::weak_for_tests(),
        )
        .expect("Env");
        unsafe {
            std::env::remove_var("AIVYX_PA_PASSPHRASE_FROM_CONFIG_EQUIV");
        }

        // Two MasterKeys derived from the same bytes + same salt
        // must produce the same subkey for any domain — that's the
        // determinism property aivyx-crypto guarantees.
        let sub_a = master_from_config.derive_subkey(b"audit").unwrap();
        let sub_b = master_from_env.derive_subkey(b"audit").unwrap();
        assert_eq!(
            sub_a.as_bytes(),
            sub_b.as_bytes(),
            "FromConfig and Env paths must derive identical master keys \
             from identical bytes (the whole point of the Phase 51 fix)",
        );
    }

    #[test]
    fn from_config_empty_string_fails_cleanly() {
        use secrecy::SecretString;
        let dir = TestDir::new();
        let err = derive_master_key(
            PassphraseSource::FromConfig(SecretString::from(String::new())),
            &dir.salt(),
            Argon2Params::weak_for_tests(),
        )
        .unwrap_err();
        // Empty passphrase is rejected with the same posture as
        // Env-empty — Argon2id would happily hash it.
        assert!(matches!(err, PassphraseError::EnvEmpty(_)));
    }

    // ---- Fixture source ---------------------------------------------

    #[test]
    fn fixture_source_derives_master_key() {
        let dir = TestDir::new();
        let master = derive_master_key(
            fixture(b"fixture-passphrase"),
            &dir.salt(),
            Argon2Params::weak_for_tests(),
        )
        .expect("fixture source must succeed");
        let _sub = master.derive_subkey(b"sessions").unwrap();
    }

    #[test]
    fn fixture_and_env_produce_same_master_key_for_same_bytes() {
        let _lock = env_lock();
        let dir = TestDir::new();
        let salt_path = dir.salt();

        // Both sources use the same passphrase string; the salt is
        // shared across both calls (load_or_create_salt will generate
        // it on the first call and read it on the second).
        unsafe {
            std::env::set_var("AIVYX_PA_PASSPHRASE_TEST_PARITY", "same-bytes");
        }
        let m_env = derive_master_key(
            PassphraseSource::Env {
                var_name: "AIVYX_PA_PASSPHRASE_TEST_PARITY".to_string(),
            },
            &salt_path,
            Argon2Params::weak_for_tests(),
        )
        .unwrap();
        unsafe {
            std::env::remove_var("AIVYX_PA_PASSPHRASE_TEST_PARITY");
        }

        let m_fix = derive_master_key(
            fixture(b"same-bytes"),
            &salt_path,
            Argon2Params::weak_for_tests(),
        )
        .unwrap();

        // Compare via a derived subkey since MasterKey's bytes are
        // not publicly accessible. If they match, the master keys
        // are equal (HKDF is deterministic).
        let s_env = m_env.derive_subkey(b"sessions").unwrap();
        let s_fix = m_fix.derive_subkey(b"sessions").unwrap();
        assert_eq!(
            subkey_fingerprint(&s_env),
            subkey_fingerprint(&s_fix),
            "same passphrase + same salt must produce same master key"
        );
    }

    /// Public MasterKey/SubKey types hide their bytes; to prove two
    /// subkeys are equal in a test, we seal the same plaintext with
    /// both and check that each can open the other's ciphertext.
    /// Equality-by-function-extensionality instead of equality-by-
    /// bytes — same correctness, different axis.
    fn subkey_fingerprint(sub: &aivyx_crypto::SubKey) -> Vec<u8> {
        let nonce = [0u8; aivyx_crypto::NONCE_LEN];
        sub.seal(&nonce, b"fingerprint", b"probe").unwrap()
    }

    // ---- Interactive path (Phase 7 task 4) ---------------------------
    //
    // Production `fetch_passphrase_bytes` calls
    // `rpassword::prompt_password`, which opens `/dev/tty` directly —
    // unavailable under `cargo test`. These tests drive the shared
    // `read_interactive_password_inner` seam with closures that wrap
    // `rpassword::prompt_password_from_bufread` (pure BufRead + Write,
    // no tty), exercising the same empty-check, error-mapping, and
    // zeroize discipline the production path owns.

    /// Drive the interactive helper end-to-end by wrapping
    /// `prompt_password_from_bufread` against an in-memory `&[u8]`.
    /// This proves the bytes flowing out of `rpassword` round-trip
    /// cleanly into `Vec<u8>` with the right trimming.
    #[test]
    #[allow(deprecated)] // test seam: rpassword 7.5 deprecated prompt_password_from_bufread; prod uses prompt_password
    fn interactive_source_reads_password_from_bufread() {
        let mut reader = &b"pipe-passphrase\n"[..];
        let mut sink: Vec<u8> = Vec::new();
        let bytes = read_interactive_password_inner(|| {
            rpassword::prompt_password_from_bufread(
                &mut reader,
                &mut sink,
                "aivyx-pa passphrase: ",
            )
        })
        .expect("bufread-backed interactive prompt must succeed");
        // The trailing newline must not survive the read — Argon2id
        // would happily hash it, and a user pasting a passphrase into
        // a pipe vs. typing it at a tty should produce the same key.
        assert_eq!(bytes, b"pipe-passphrase");
        // The prompt itself must have been written to the sink, so a
        // real tty would see it. We assert it's non-empty rather than
        // an exact match to avoid coupling to rpassword's trailing
        // flush behavior.
        assert!(!sink.is_empty(), "prompt string must be written to the writer");
    }

    /// An empty line (user pressed enter without typing anything)
    /// must surface as `InteractiveEmpty`, mirroring how the env-var
    /// path rejects an empty `AIVYX_PA_PASSPHRASE`. An empty passphrase
    /// would derive a deterministic master key and defeat the whole
    /// Argon2id layer.
    #[test]
    #[allow(deprecated)] // test seam: rpassword 7.5 deprecated prompt_password_from_bufread; prod uses prompt_password
    fn interactive_source_rejects_empty_password() {
        let mut reader = &b"\n"[..];
        let mut sink: Vec<u8> = Vec::new();
        let err = read_interactive_password_inner(|| {
            rpassword::prompt_password_from_bufread(&mut reader, &mut sink, "p: ")
        })
        .unwrap_err();
        assert!(matches!(err, PassphraseError::InteractiveEmpty));
    }

    /// An I/O error from the read closure (e.g., a truncated pipe)
    /// must be wrapped in `InteractiveIo`, not silently treated as
    /// an empty password.
    #[test]
    fn interactive_source_wraps_read_errors_as_interactive_io() {
        let err = read_interactive_password_inner(|| {
            Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "simulated eof",
            ))
        })
        .unwrap_err();
        match err {
            PassphraseError::InteractiveIo { reason } => {
                assert!(
                    reason.contains("simulated eof"),
                    "InteractiveIo must surface the underlying reason, got {reason:?}"
                );
            }
            other => panic!("expected InteractiveIo, got {other:?}"),
        }
    }

    /// Prove the full `derive_master_key` pipeline works end-to-end
    /// against a bufread-sourced passphrase by wiring the same seam
    /// through a `Fixture` variant. This test is belt-and-braces —
    /// the per-arm tests above cover the interactive path surface,
    /// but this one pins the invariant that "whatever the
    /// interactive helper produces, the outer `derive_master_key`
    /// treats it identically to an equally-valued `Fixture`."
    #[test]
    #[allow(deprecated)] // test seam: rpassword 7.5 deprecated prompt_password_from_bufread; prod uses prompt_password
    fn interactive_and_fixture_produce_same_master_key_for_same_bytes() {
        let dir = TestDir::new();
        let salt_path = dir.salt();

        // Fixture path as the oracle.
        let m_fix = derive_master_key(
            fixture(b"pipe-passphrase"),
            &salt_path,
            Argon2Params::weak_for_tests(),
        )
        .unwrap();

        // Interactive path via the bufread seam. We can't pass the
        // seam through `derive_master_key` directly (production API
        // doesn't take a reader), so we mimic its zeroize-after-
        // derive discipline inline: read via the helper, feed bytes
        // to `aivyx_crypto::derive_master_key` with the same salt,
        // zeroize.
        let mut reader = &b"pipe-passphrase\n"[..];
        let mut sink: Vec<u8> = Vec::new();
        let mut bytes = read_interactive_password_inner(|| {
            rpassword::prompt_password_from_bufread(&mut reader, &mut sink, "p: ")
        })
        .unwrap();
        let salt = load_or_create_salt(&salt_path).unwrap();
        let m_inter = crypto_derive_master_key(&bytes, &salt, Argon2Params::weak_for_tests())
            .unwrap();
        bytes.zeroize();

        assert_eq!(
            subkey_fingerprint(&m_fix.derive_subkey(b"sessions").unwrap()),
            subkey_fingerprint(&m_inter.derive_subkey(b"sessions").unwrap()),
            "interactive and fixture paths must produce the same master key \
             for the same passphrase + same salt",
        );
    }

    // ---- Debug redaction tripwire — Env + InteractivePrompt ---------
    //
    // `PassphraseSource` has a custom `Debug` impl at
    // `passphrase.rs:142` whose whole purpose is to redact secrets.
    // The `Fixture` variant is already pinned by the older
    // `debug_impl_does_not_leak_fixture_closure_contents` test; the
    // two tests below extend coverage to the `Env` variant (name
    // should appear, value should *not*) and the `InteractivePrompt`
    // variant (format must not touch the tty). Together they cover
    // all three variants against the "someone ever replaces the
    // custom impl with `#[derive(Debug)]`" regression.

    #[test]
    fn debug_env_renders_var_name_but_not_value() {
        let _lock = env_lock();
        unsafe {
            std::env::set_var("AIVYX_PA_PASSPHRASE_TRIPWIRE", "should-not-appear");
        }
        let source = PassphraseSource::Env {
            var_name: "AIVYX_PA_PASSPHRASE_TRIPWIRE".to_string(),
        };
        let rendered = format!("{source:?}");
        assert!(
            rendered.contains("AIVYX_PA_PASSPHRASE_TRIPWIRE"),
            "Debug should include the var name, got {rendered:?}"
        );
        assert!(
            !rendered.contains("should-not-appear"),
            "Debug must not read the env var or leak its value, got {rendered:?}"
        );
        unsafe {
            std::env::remove_var("AIVYX_PA_PASSPHRASE_TRIPWIRE");
        }
    }

    #[test]
    fn debug_interactive_prompt_renders_without_side_effects() {
        // The tripwire here is that `format!` must not touch the tty
        // or block on a read.
        let rendered = format!(
            "{:?}",
            PassphraseSource::InteractivePrompt { confirm: false }
        );
        assert_eq!(rendered, "InteractivePrompt { confirm: false }");
    }

    #[test]
    #[allow(deprecated)] // test seam: rpassword 7.5 deprecated prompt_password_from_bufread; prod uses prompt_password
    fn confirm_reentry_matching_pair_succeeds() {
        let mut reader = &b"same-pass\nsame-pass\n"[..];
        let mut sink: Vec<u8> = Vec::new();
        let bytes = read_interactive_password_with_confirm(|prompt| {
            rpassword::prompt_password_from_bufread(&mut reader, &mut sink, prompt)
        })
        .expect("matching pair must succeed");
        assert_eq!(bytes, b"same-pass");
    }

    #[test]
    #[allow(deprecated)]
    fn confirm_reentry_mismatch_reprompts_until_matching() {
        // First pair ("typo-a" / "typo-b") mismatches and must be
        // silently discarded, not returned or mixed with the second
        // pair. Second pair ("real-pass" / "real-pass") matches.
        let mut reader = &b"typo-a\ntypo-b\nreal-pass\nreal-pass\n"[..];
        let mut sink: Vec<u8> = Vec::new();
        let bytes = read_interactive_password_with_confirm(|prompt| {
            rpassword::prompt_password_from_bufread(&mut reader, &mut sink, prompt)
        })
        .expect("second, matching pair must eventually succeed");
        assert_eq!(bytes, b"real-pass");
        let written = String::from_utf8_lossy(&sink);
        assert!(
            written.contains("Passphrases didn't match"),
            "retry prompt must carry the mismatch notice: {written:?}"
        );
    }

    #[test]
    #[allow(deprecated)]
    fn confirm_reentry_empty_first_entry_is_rejected() {
        let mut reader = &b"\n"[..];
        let mut sink: Vec<u8> = Vec::new();
        let err = read_interactive_password_with_confirm(|prompt| {
            rpassword::prompt_password_from_bufread(&mut reader, &mut sink, prompt)
        })
        .unwrap_err();
        assert!(matches!(err, PassphraseError::InteractiveEmpty));
    }

    #[test]
    #[allow(deprecated)]
    fn confirm_reentry_wording_differs_from_unconfirmed_prompt() {
        // The new-store prompt must name the stakes; the existing
        // unlock prompt (confirm: false) must stay exactly as it was.
        let mut reader = &b"x\nx\n"[..];
        let mut sink: Vec<u8> = Vec::new();
        read_interactive_password_with_confirm(|prompt| {
            rpassword::prompt_password_from_bufread(&mut reader, &mut sink, prompt)
        })
        .expect("must succeed");
        let written = String::from_utf8_lossy(&sink);
        assert!(
            written.contains("new store"),
            "new-store prompt must mention it's a new store: {written:?}"
        );
    }

    // ---- End-to-end with aivyx-storage -------------------------------

    #[tokio::test]
    async fn passphrase_to_storage_full_round_trip() {
        // The whole point of the task — proves the three-layer chain
        // (passphrase → MasterKey → RedbStorage → get/put) actually
        // composes. Task 4 will wire this same shape into
        // SessionConfig; this test is its regression precedent.
        use aivyx_storage::{KeyDomain, RedbStorage, StorageConfig};

        let dir = TestDir::new();
        let store_path = dir.dir.join("store.redb");
        let salt_path = dir.salt();
        let passphrase: &[u8] = b"e2e-passphrase";

        // Session A: derive, open, write, drop.
        {
            let master_a = derive_master_key(
                fixture(passphrase),
                &salt_path,
                Argon2Params::weak_for_tests(),
            )
            .expect("session A: derive");

            let store_a = RedbStorage::open(StorageConfig::new(store_path.clone()), master_a)
                .await
                .expect("session A: open");

            store_a
                .domain(KeyDomain::Sessions)
                .put(b"last-seen", b"turn-99")
                .await
                .expect("session A: put");

            drop(store_a);
        }

        // Session B: re-derive from the same passphrase + same salt
        // file (load_or_create_salt reads the existing bytes), open
        // the same redb file, read back the persisted value.
        {
            let master_b = derive_master_key(
                fixture(passphrase),
                &salt_path,
                Argon2Params::weak_for_tests(),
            )
            .expect("session B: re-derive");

            let store_b = RedbStorage::open(StorageConfig::new(store_path.clone()), master_b)
                .await
                .expect("session B: reopen");

            let got = store_b
                .domain(KeyDomain::Sessions)
                .get(b"last-seen")
                .await
                .expect("session B: get");
            assert_eq!(got, Some(b"turn-99".to_vec()));
        }
    }

    #[tokio::test]
    async fn wrong_passphrase_fails_to_decrypt_storage() {
        // Complements the storage-layer wrong-key test: task 3's
        // contract says "same passphrase + same salt → same master →
        // same subkeys," so a *different* passphrase under the same
        // salt must produce a master that cannot decrypt the
        // ciphertext. Proves the full chain is sensitive to
        // passphrase bytes, not just to explicit MasterKey
        // construction.
        use aivyx_storage::{KeyDomain, RedbStorage, StorageConfig, StorageError};

        let dir = TestDir::new();
        let store_path = dir.dir.join("store.redb");
        let salt_path = dir.salt();

        // Write with passphrase A.
        {
            let master = derive_master_key(
                fixture(b"passphrase-a"),
                &salt_path,
                Argon2Params::weak_for_tests(),
            )
            .unwrap();
            let store = RedbStorage::open(StorageConfig::new(store_path.clone()), master)
                .await
                .unwrap();
            store
                .domain(KeyDomain::Secrets)
                .put(b"api-key", b"sk-xxx")
                .await
                .unwrap();
            drop(store);
        }

        // Reopen with passphrase B; get fails at decrypt time.
        let master = derive_master_key(
            fixture(b"passphrase-b"),
            &salt_path,
            Argon2Params::weak_for_tests(),
        )
        .unwrap();
        let store = RedbStorage::open(StorageConfig::new(store_path.clone()), master)
            .await
            .unwrap();
        let err = store
            .domain(KeyDomain::Secrets)
            .get(b"api-key")
            .await
            .unwrap_err();
        assert!(
            matches!(err, StorageError::DecryptFailed { domain: KeyDomain::Secrets }),
            "unexpected error: {err:?}"
        );
    }

    #[test]
    fn debug_impl_does_not_leak_fixture_closure_contents() {
        // Regression lock: a `dbg!(source)` on a Fixture variant
        // must not somehow print the passphrase that the closure
        // would return. The closure is opaque-by-design and the
        // Debug impl must reflect that.
        let src = fixture(b"super-secret");
        let dbg = format!("{src:?}");
        assert!(dbg.contains("Fixture"));
        assert!(!dbg.contains("super-secret"));
    }
}
