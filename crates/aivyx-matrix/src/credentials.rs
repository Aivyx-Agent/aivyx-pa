//! The assistant's Matrix login, sealed under the operator's master key:
//! `<matrix_dir>/session.enc` = 12-byte nonce || ChaCha20-Poly1305(JSON).
//! Not pa's redb store — the running daemon holds that locked, and
//! `connect matrix` must work either way.

use std::path::Path;

use aivyx_crypto::{MasterKey, SubKey};
use serde::{Deserialize, Serialize};

pub const SESSION_FILE: &str = "session.enc";
const AAD: &[u8] = b"aivyx-pa matrix session v1";

pub struct MatrixKeys {
    session: SubKey,
    store: SubKey,
}

impl MatrixKeys {
    pub fn derive(master: &MasterKey) -> Result<Self, String> {
        Ok(MatrixKeys {
            session: master.derive_subkey(b"aivyx-pa matrix session").map_err(|e| e.to_string())?,
            store: master.derive_subkey(b"aivyx-pa matrix store").map_err(|e| e.to_string())?,
        })
    }

    /// The `matrix-sdk` SQLite store passphrase. `SubKey::as_bytes` is the
    /// discouraged accessor; this is a justified caller: `matrix-sdk`
    /// takes its store key as a passphrase string, which it stretches
    /// itself, so the subkey's hex is the only way to hand it over.
    pub fn store_passphrase(&self) -> String {
        hex::encode(self.store.as_bytes())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub homeserver: String,
    pub user_id: String,
    pub device_id: String,
    pub access_token: String,
}

pub fn save(keys: &MatrixKeys, dir: &Path, session: &Session) -> Result<(), String> {
    use rand::RngCore;
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let mut nonce = [0u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let plain = serde_json::to_vec(session).map_err(|e| e.to_string())?;
    let sealed = keys.session.seal(&nonce, AAD, &plain).map_err(|e| e.to_string())?;
    let mut out = nonce.to_vec();
    out.extend_from_slice(&sealed);
    let path = dir.join(SESSION_FILE);
    let tmp = dir.join(".session.enc.tmp");
    {
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut f = options.open(&tmp).map_err(|e| format!("write {}: {e}", tmp.display()))?;
        f.write_all(&out).map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())?;
    }
    std::fs::rename(&tmp, &path).map_err(|e| format!("write {}: {e}", path.display()))
}

pub fn load(keys: &MatrixKeys, dir: &Path) -> Result<Option<Session>, String> {
    let path = dir.join(SESSION_FILE);
    let raw = match std::fs::read(&path) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    };
    if raw.len() < 12 {
        return Err(format!("{} is damaged — run `aivyx-pa connect matrix` again", path.display()));
    }
    let (nonce, sealed) = raw.split_at(12);
    let plain = keys.session.open(nonce, AAD, sealed).map_err(|_| {
        format!("{} can't be opened with this passphrase — run `aivyx-pa connect matrix` again", path.display())
    })?;
    serde_json::from_slice(&plain).map(Some).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> MatrixKeys {
        MatrixKeys::derive(&aivyx_crypto::MasterKey::from_raw([3u8; 32])).unwrap()
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("aivyx-matrix-cred-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn sample() -> Session {
        Session {
            homeserver: "https://matrix.example".into(),
            user_id: "@bot:example".into(),
            device_id: "DEVICE".into(),
            access_token: "syt_secret".into(),
        }
    }

    #[test]
    fn a_saved_session_loads_back_and_is_not_plaintext() {
        let dir = scratch("roundtrip");
        save(&keys(), &dir, &sample()).unwrap();
        assert_eq!(load(&keys(), &dir).unwrap(), Some(sample()));
        let raw = std::fs::read(dir.join(SESSION_FILE)).unwrap();
        assert!(!raw.windows(10).any(|w| w == b"syt_secret"), "token must not be stored in the clear");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join(SESSION_FILE)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn no_session_is_none_and_another_key_cant_open_it() {
        let dir = scratch("wrongkey");
        assert_eq!(load(&keys(), &dir).unwrap(), None);
        save(&keys(), &dir, &sample()).unwrap();
        let other = MatrixKeys::derive(&aivyx_crypto::MasterKey::from_raw([4u8; 32])).unwrap();
        assert!(load(&other, &dir).is_err());
    }

    #[test]
    fn the_store_passphrase_is_stable_hex_and_differs_per_key() {
        assert_eq!(keys().store_passphrase(), keys().store_passphrase());
        assert_eq!(keys().store_passphrase().len(), 64);
        let other = MatrixKeys::derive(&aivyx_crypto::MasterKey::from_raw([4u8; 32])).unwrap();
        assert_ne!(keys().store_passphrase(), other.store_passphrase());
    }
}
