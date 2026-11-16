//! The X25519 transport key (A19 §2): the Noise static key, sealed at rest
//! under a subkey of the storage [`MasterKey`], exactly like the Ed25519
//! federation identity (`Identity::load_or_generate`).

use std::path::Path;

use aivyx_crypto::{MasterKey, NONCE_LEN};
use snow::resolvers::{CryptoResolver, DefaultResolver};
use zeroize::Zeroize as _;

use crate::NetError;
use crate::card::fingerprint;
use crate::link::NOISE_PARAMS;

/// HKDF `info` for the transport-key wrapping subkey (versioned).
const KEY_WRAP_INFO: &[u8] = b"aivyx-federation-transport-key-v1";
/// AEAD additional data binding the sealed key to its purpose.
const KEY_WRAP_AAD: &[u8] = b"aivyx-federation-transport";
/// Sealed key file: `nonce(12) || ciphertext+tag(48)`.
const ENCRYPTED_KEY_LEN: usize = NONCE_LEN + 48;

/// This instance's X25519 static key for the Noise link.
///
/// `Debug` shows only the public key's fingerprint; the private half is
/// zeroed on drop.
pub struct TransportKey {
    private: [u8; 32],
    public: [u8; 32],
}

impl TransportKey {
    /// Generate a fresh random keypair.
    pub fn generate() -> Result<Self, NetError> {
        let params = NOISE_PARAMS
            .parse()
            .map_err(|e| NetError::Key(format!("noise params: {e}")))?;
        let mut kp = snow::Builder::new(params)
            .generate_keypair()
            .map_err(|e| NetError::Key(format!("generate keypair: {e}")))?;
        let private: Result<[u8; 32], _> = kp.private.as_slice().try_into();
        let public: Result<[u8; 32], _> = kp.public.as_slice().try_into();
        kp.private.zeroize();
        match (private, public) {
            (Ok(private), Ok(public)) => Ok(Self { private, public }),
            _ => Err(NetError::Key("generated key is not 32 bytes".into())),
        }
    }

    /// Rebuild from a private key, deriving the public half.
    fn from_private(private: [u8; 32]) -> Result<Self, NetError> {
        let mut dh = DefaultResolver
            .resolve_dh(&snow::params::DHChoice::Curve25519)
            .ok_or_else(|| NetError::Key("no X25519 implementation".into()))?;
        dh.set(&private);
        let public: [u8; 32] = dh
            .pubkey()
            .try_into()
            .map_err(|_| NetError::Key("derived public key is not 32 bytes".into()))?;
        Ok(Self { private, public })
    }

    /// Load the key sealed at `path`, or generate, seal and save one if the
    /// file does not exist. The file is `0o600`. A wrong master key, a
    /// tampered file or a file of the wrong size is an error, never a silent
    /// regeneration.
    pub fn load_or_generate(path: &Path, master: &MasterKey) -> Result<Self, NetError> {
        let subkey = master
            .derive_subkey(KEY_WRAP_INFO)
            .map_err(|e| NetError::Key(format!("derive transport wrap key: {e}")))?;

        if path.exists() {
            let data = std::fs::read(path)
                .map_err(|e| NetError::Key(format!("read transport key: {e}")))?;
            if data.len() != ENCRYPTED_KEY_LEN {
                return Err(NetError::Key(format!(
                    "transport key file has unexpected size {} (expected {ENCRYPTED_KEY_LEN})",
                    data.len()
                )));
            }
            let mut plaintext = subkey
                .open(&data[..NONCE_LEN], KEY_WRAP_AAD, &data[NONCE_LEN..])
                .map_err(|_| NetError::Key("transport key decryption failed".into()))?;
            let private: Result<[u8; 32], _> = plaintext.as_slice().try_into();
            plaintext.zeroize();
            let private =
                private.map_err(|_| NetError::Key("decrypted key is not 32 bytes".into()))?;
            Self::from_private(private)
        } else {
            let key = Self::generate()?;
            key.save_sealed(path, &subkey)?;
            Ok(key)
        }
    }

    fn save_sealed(&self, path: &Path, subkey: &aivyx_crypto::SubKey) -> Result<(), NetError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| NetError::Key(format!("create key dir: {e}")))?;
        }
        let mut nonce = [0u8; NONCE_LEN];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut nonce);
        let ciphertext = subkey
            .seal(&nonce, KEY_WRAP_AAD, &self.private)
            .map_err(|e| NetError::Key(format!("seal transport key: {e}")))?;
        let mut data = Vec::with_capacity(ENCRYPTED_KEY_LEN);
        data.extend_from_slice(&nonce);
        data.extend_from_slice(&ciphertext);
        write_new_private_file(path, &data)?;
        set_file_permissions_600(path)
    }

    /// The X25519 public key.
    pub fn public(&self) -> [u8; 32] {
        self.public
    }

    /// The private key, for the Noise builder (and tests).
    pub(crate) fn private_bytes(&self) -> &[u8; 32] {
        &self.private
    }
}

impl std::fmt::Debug for TransportKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TransportKey")
            .field("public", &fingerprint(&self.public))
            .finish()
    }
}

impl Drop for TransportKey {
    fn drop(&mut self) {
        // `zeroize` uses volatile writes the optimiser cannot drop. Known
        // limit: snow's `Dh25519` keeps its own copy of the private key for
        // the life of each handshake/transport state, and does not zero it.
        self.private.zeroize();
    }
}

/// Create `path` (refusing to overwrite) and write `data`. On Unix the file
/// is created `0o600`, so the sealed key is never briefly world-readable.
fn write_new_private_file(path: &Path, data: &[u8]) -> Result<(), NetError> {
    use std::io::Write as _;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts
        .open(path)
        .map_err(|e| NetError::Key(format!("create transport key file: {e}")))?;
    file.write_all(data)
        .and_then(|()| file.sync_all())
        .map_err(|e| NetError::Key(format!("write transport key: {e}")))
}

#[cfg(unix)]
fn set_file_permissions_600(path: &Path) -> Result<(), NetError> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| NetError::Key(format!("set key file permissions: {e}")))
}

#[cfg(not(unix))]
fn set_file_permissions_600(_path: &Path) -> Result<(), NetError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sealed_key_reloads_and_a_wrong_master_fails() {
        let dir = std::env::temp_dir().join(format!("fednet-{}", uuid::Uuid::new_v4()));
        let path = dir.join("transport.key");
        let m1 = aivyx_crypto::MasterKey::from_raw([1u8; 32]);
        let a = TransportKey::load_or_generate(&path, &m1).unwrap();
        let b = TransportKey::load_or_generate(&path, &m1).unwrap();
        assert_eq!(a.public(), b.public());
        let m2 = aivyx_crypto::MasterKey::from_raw([2u8; 32]);
        assert!(TransportKey::load_or_generate(&path, &m2).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn debug_never_shows_the_private_key() {
        let k = TransportKey::generate().unwrap();
        assert!(!format!("{k:?}").contains(&hex::encode(k.private_bytes())));
    }
}
