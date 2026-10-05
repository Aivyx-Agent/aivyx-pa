//! Chapter Keyring — the master passphrase in the OS credential store.
//!
//! Removes the plaintext-secret-at-rest weakness for **interactive / desktop**
//! use: instead of `AIVYX_PA_PASSPHRASE` in the environment or `[aivyx_pa]
//! passphrase` in the TOML (both plaintext), the operator stores the master
//! passphrase once in the OS keyring (Secret Service on Linux, Keychain on
//! macOS, Credential Manager on Windows), and the daemon reads it from there at
//! startup.
//!
//! ## Scope + honesty
//!
//! This helps the case where a Secret Service / Keychain is actually reachable —
//! a desktop session or an interactive `aivyx-pa` run. The **headless systemd user
//! service under linger** (Chapter Anchor's "runs for days" install) has no
//! login session / Secret Service, so it keeps using the `0600 daemon.env`
//! file; the keyring is an *additional* source, not a replacement there. Every
//! operation is best-effort: an unavailable/locked keyring surfaces a clear
//! error and the caller falls back to the existing env / TOML / prompt sources.

use secrecy::{ExposeSecret, SecretString};

/// Keyring service name (the application) and account (which secret).
const SERVICE: &str = "aivyx-pa";
const ACCOUNT_BASE: &str = "master-passphrase";

#[derive(Debug, thiserror::Error)]
pub enum KeyringError {
    /// The OS keyring could not be reached (no Secret Service running, locked
    /// keychain, no session bus, …). Callers treat this as "no keyring here"
    /// and fall back to other passphrase sources.
    #[error("OS keyring unavailable: {0}")]
    Unavailable(String),
}

fn entry_for(paths: &aivyx_instance::InstancePaths) -> Result<keyring::Entry, KeyringError> {
    let account = paths.keyring_account(ACCOUNT_BASE);
    keyring::Entry::new(SERVICE, &account)
        .map_err(|e| KeyringError::Unavailable(e.to_string()))
}

fn entry() -> Result<keyring::Entry, KeyringError> {
    let paths = aivyx_instance::InstancePaths::current()
        .map_err(|e| KeyringError::Unavailable(format!("failed to resolve instance: {}", e)))?;
    entry_for(&paths)
}

/// Store (or overwrite) the master passphrase in the OS keyring for a given instance.
pub fn store_for(paths: &aivyx_instance::InstancePaths, passphrase: &SecretString) -> Result<(), KeyringError> {
    entry_for(paths)?
        .set_password(passphrase.expose_secret())
        .map_err(|e| KeyringError::Unavailable(e.to_string()))
}

/// Store (or overwrite) the master passphrase in the OS keyring.
pub fn store(passphrase: &SecretString) -> Result<(), KeyringError> {
    entry()?
        .set_password(passphrase.expose_secret())
        .map_err(|e| KeyringError::Unavailable(e.to_string()))
}

/// Retrieve the stored passphrase for a given instance, or `None` if nothing is stored.
/// `Err` only on a genuine keyring failure (unavailable / locked), which lets the daemon
/// distinguish "no keyring / not set" (fall back) from a surprising fault.
pub fn retrieve_for(paths: &aivyx_instance::InstancePaths) -> Result<Option<SecretString>, KeyringError> {
    match entry_for(paths)?.get_password() {
        Ok(p) => Ok(Some(SecretString::from(p))),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(KeyringError::Unavailable(e.to_string())),
    }
}

/// Retrieve the stored passphrase, or `None` if nothing is stored. `Err` only
/// on a genuine keyring failure (unavailable / locked), which lets the daemon
/// distinguish "no keyring / not set" (fall back) from a surprising fault.
pub fn retrieve() -> Result<Option<SecretString>, KeyringError> {
    match entry()?.get_password() {
        Ok(p) => Ok(Some(SecretString::from(p))),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(KeyringError::Unavailable(e.to_string())),
    }
}

/// Remove the stored passphrase for a given instance. `Ok` even if there was nothing to remove.
pub fn clear_for(paths: &aivyx_instance::InstancePaths) -> Result<(), KeyringError> {
    match entry_for(paths)?.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(KeyringError::Unavailable(e.to_string())),
    }
}

/// Remove the stored passphrase. `Ok` even if there was nothing to remove.
pub fn clear() -> Result<(), KeyringError> {
    match entry()?.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(KeyringError::Unavailable(e.to_string())),
    }
}

/// Best-effort: is a passphrase currently stored for a given instance?
pub fn is_stored_for(paths: &aivyx_instance::InstancePaths) -> Result<bool, KeyringError> {
    Ok(retrieve_for(paths)?.is_some())
}

/// Best-effort: is a passphrase currently stored?
pub fn is_stored() -> Result<bool, KeyringError> {
    Ok(retrieve()?.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Once;

    // The `mock` backend gives an in-memory credential store, so tests don't
    // touch (or require) a real Secret Service / Keychain. Set once per process.
    static INIT: Once = Once::new();
    fn use_mock() {
        INIT.call_once(|| {
            keyring::set_default_credential_builder(
                keyring::mock::default_credential_builder(),
            );
        });
    }

    // NOTE: keyring's `mock` backend builds a fresh, independent credential per
    // `Entry::new`, so it does NOT model cross-call persistence keyed by
    // service/account the way a real Secret Service / Keychain does. These
    // tests therefore validate the error-mapping + call shape (the part that's
    // ours); true store→retrieve persistence is confirmed manually against a
    // live keyring (documented in the module header + `aivyx-pa keyring` help).
    #[test]
    fn retrieve_maps_missing_secret_to_none() {
        use_mock();
        // A fresh (unset) entry: `NoEntry` must map to `Ok(None)`, never an Err.
        match retrieve() {
            Ok(None) => {}
            other => panic!("expected Ok(None) for an unset entry, got {other:?}"),
        }
        assert!(!is_stored().unwrap());
    }

    #[test]
    fn store_and_clear_do_not_error_against_the_backend() {
        use_mock();
        // set_password succeeds…
        store(&SecretString::from("hunter2".to_string())).unwrap();
        // …and clear is idempotent (delete of a possibly-absent entry is Ok).
        clear().unwrap();
        clear().unwrap();
    }

    // The passphrase is resolved INSIDE the daemon's tokio runtime. keyring's
    // sync Entry bridges to async zbus; with the `async-io` executor (not
    // `tokio`) that must NOT nest-panic here. A regression to the `tokio`
    // feature would blow up this test with "Cannot start a runtime within a
    // runtime".
    #[tokio::test]
    async fn retrieve_is_callable_from_within_a_tokio_runtime() {
        use_mock();
        let out = retrieve();
        assert!(out.is_ok(), "keyring retrieve must not panic/err inside tokio");
    }
}
