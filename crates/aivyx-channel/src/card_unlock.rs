//! Card unlock (Amendment A18): per enrolled OpenPGP card, the master key
//! wrapped under HKDF(X25519(ephemeral, card)). The ephemeral secret is
//! dropped at enrollment, so only the card's ECDH rebuilds the wrap key.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use aivyx_card_helper::{CardHelper, HelperError, decode_key, encode_key};
use aivyx_crypto::{MasterKey, WrapKey, WrappedMasterKey};
use base64::Engine;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;
pub const FILE_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CardEntry {
    pub serial: String,
    pub label: String,
    pub added: String,
    pub card_public: String,
    pub ephemeral_public: String,
    pub nonce: String,
    pub wrapped: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CardUnlockFile {
    pub version: u32,
    #[serde(default)]
    pub cards: Vec<CardEntry>,
}

#[derive(Debug, thiserror::Error)]
pub enum CardUnlockError {
    #[error("{0}")]
    Helper(#[from] HelperError),
    #[error("none of your enrolled cards is plugged in (enrolled: {0})")]
    NoEnrolledCardPresent(String),
    #[error("card {0} isn't enrolled")]
    NotEnrolled(String),
    #[error(
        "card {serial}'s entry doesn't open — remove and re-add it (aivyx-pa card remove {serial})"
    )]
    EntryDoesNotOpen { serial: String },
    #[error("{path} is broken: {reason}")]
    BrokenFile { path: String, reason: String },
    #[error("{0}")]
    Io(String),
}

/// `info` and AAD: binds an entry to its instance, card and both keys.
pub fn binding_info(
    instance: &str,
    serial: &str,
    card_public: &[u8; 32],
    ephemeral_public: &[u8; 32],
) -> Vec<u8> {
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
        let broken = |reason: String| CardUnlockError::BrokenFile {
            path: path.display().to_string(),
            reason,
        };
        let file: Self = toml::from_str(&text).map_err(|e| broken(e.to_string()))?;
        if file.version != FILE_VERSION {
            return Err(broken(format!(
                "version {} (this aivyx-pa reads {FILE_VERSION})",
                file.version
            )));
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
        // `mode` only applies when the file is created: a stale temp file
        // left with wider permissions would keep them, so start fresh.
        match std::fs::remove_file(&tmp) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(io(e)),
            _ => {}
        }
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(io)?;
        f.write_all(text.as_bytes()).map_err(io)?;
        f.sync_all().map_err(io)?;
        std::fs::rename(&tmp, path).map_err(io)
    }
}

#[allow(clippy::too_many_arguments)]
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
    use rand::RngCore;
    let mut raw = zeroize::Zeroizing::new([0u8; 32]);
    rand::rngs::OsRng.fill_bytes(&mut *raw);
    let eph = x25519_dalek::StaticSecret::from(*raw);
    let eph_public = *x25519_dalek::PublicKey::from(&eph).as_bytes();
    drop(eph); // never stored: only the card can rebuild the shared secret
    drop(raw);
    // The card computes it (PIN + touch), which also proves the card works.
    let shared = helper.ecdh(serial, pin, &eph_public)?;
    let info = binding_info(instance, serial, &card_public, &eph_public);
    let key = WrapKey::derive(&shared, &info).map_err(|e| CardUnlockError::Io(e.to_string()))?;
    let wrapped = master
        .wrap(&key, &info)
        .map_err(|e| CardUnlockError::Io(e.to_string()))?;
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

fn open_entry(
    entry: &CardEntry,
    instance: &str,
    helper: &dyn CardHelper,
    pin: &SecretString,
) -> Result<MasterKey, CardUnlockError> {
    let bad = || CardUnlockError::EntryDoesNotOpen {
        serial: entry.serial.clone(),
    };
    let card_public = decode_key(&entry.card_public).ok_or_else(bad)?;
    let eph_public = decode_key(&entry.ephemeral_public).ok_or_else(bad)?;
    let nonce: [u8; 12] = B64
        .decode(&entry.nonce)
        .ok()
        .and_then(|n| n.try_into().ok())
        .ok_or_else(bad)?;
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
        Some(s) => file
            .cards
            .iter()
            .find(|c| c.serial == s)
            .ok_or_else(|| CardUnlockError::NotEnrolled(s.into()))?,
        None => {
            let present: Vec<String> = helper.list()?.into_iter().map(|c| c.serial).collect();
            file.cards
                .iter()
                .find(|c| present.contains(&c.serial))
                .ok_or_else(|| {
                    CardUnlockError::NoEnrolledCardPresent(
                        file.cards
                            .iter()
                            .map(|c| format!("{} ({})", c.label, c.serial))
                            .collect::<Vec<_>>()
                            .join(", "),
                    )
                })?
        }
    };
    open_entry(entry, instance, helper, pin).map(|m| (m, entry.serial.clone()))
}

/// See [`CardUnlockContext::lockable`].
pub type Lockable = Arc<dyn Fn() -> Result<(), String> + Send + Sync>;

/// What a running, unlocked daemon needs for `EnrollCard` and `Lock` (A18).
///
/// It holds a copy of the master key. A18 accepts this: the daemon already
/// holds the master key for as long as it runs (inside the store), and the
/// copy here lives exactly as long and is zeroed on drop like the original.
pub struct CardUnlockContext {
    pub instance: String,
    pub file_path: PathBuf,
    pub helper: Arc<dyn CardHelper>,
    /// A copy of the master key, held only so `EnrollCard` can wrap it.
    pub master: MasterKey,
    /// False when a passphrase source is saved (keyring, daemon.env, env, TOML):
    /// a relocked daemon would unlock itself again, so `Lock` is refused.
    /// Decided at start-up; [`Self::lockable`] re-checks at `Lock` time.
    pub can_lock: bool,
    /// Would a restart right now start locked? Re-checks the saved
    /// passphrase sources and the enrolled cards when `Lock` arrives
    /// (`Err` says why not). Blocking: call it from `spawn_blocking`.
    pub lockable: Lockable,
    /// Set by `Lock`; read by aivyx.rs after `run_daemon` returns to re-exec.
    pub relock_requested: Arc<AtomicBool>,
    /// Held across `EnrollCard`'s load-modify-save, so two enrollments at
    /// once can't drop each other's entry.
    pub enroll_lock: std::sync::Mutex<()>,
}

/// `EnrollCard` in a running daemon: load the file (or start one), get the
/// card's decryption key (making one first when `generate` is set: the
/// helper refuses a slot that already holds a key), wrap the master key to
/// it, and save. Blocking (runs the helper): call it from `spawn_blocking`.
pub fn enroll_card(
    ctx: &CardUnlockContext,
    serial: &str,
    label: &str,
    pin: &SecretString,
    generate: bool,
    admin_pin: Option<&SecretString>,
) -> Result<(), CardUnlockError> {
    let _held = ctx
        .enroll_lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut file = CardUnlockFile::load(&ctx.file_path)?.unwrap_or_default();
    let card_public = if generate {
        let admin_pin = admin_pin.ok_or_else(|| {
            CardUnlockError::Io("making a key on the card needs the admin PIN".into())
        })?;
        ctx.helper.generate(serial, admin_pin)?
    } else {
        ctx.helper.public_key(serial)?.ok_or_else(|| {
            CardUnlockError::Io(format!(
                "card {serial} has no decryption key: make one first (generate)"
            ))
        })?
    };
    enroll(
        &mut file,
        &ctx.master,
        &ctx.instance,
        serial,
        label,
        card_public,
        &*ctx.helper,
        pin,
    )?;
    file.save(&ctx.file_path)
}

pub fn remove(file: &mut CardUnlockFile, serial: &str) -> Result<CardEntry, CardUnlockError> {
    let i = file
        .cards
        .iter()
        .position(|c| c.serial == serial)
        .ok_or_else(|| CardUnlockError::NotEnrolled(serial.into()))?;
    Ok(file.cards.remove(i))
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_card_helper::FakeHelper;

    fn master() -> MasterKey {
        MasterKey::from_raw([3u8; 32])
    }
    fn pin() -> SecretString {
        SecretString::from("123456")
    }

    fn enrolled(h: &FakeHelper, serial: &str) -> CardUnlockFile {
        let mut f = CardUnlockFile::default();
        let public = h.public_key(serial).unwrap().unwrap();
        enroll(
            &mut f,
            &master(),
            "default",
            serial,
            "laptop",
            public,
            h,
            &pin(),
        )
        .unwrap();
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
        assert!(matches!(
            unlock_with(&f, "work", None, &h, &pin()),
            Err(CardUnlockError::EntryDoesNotOpen { .. })
        ));
        let mut g = f.clone();
        g.cards[0].serial = "S2".into();
        let h2 = FakeHelper::with_card("S2", [5u8; 32]);
        assert!(matches!(
            unlock_with(&g, "default", None, &h2, &pin()),
            Err(CardUnlockError::EntryDoesNotOpen { .. })
        ));
    }

    #[test]
    fn swapped_ephemeral_keys_and_flipped_bytes_do_not_open() {
        let h = FakeHelper::with_card("S1", [5u8; 32]);
        h.add_card("S2", [6u8; 32]);
        let mut f = enrolled(&h, "S1");
        let public2 = h.public_key("S2").unwrap().unwrap();
        enroll(
            &mut f,
            &master(),
            "default",
            "S2",
            "spare",
            public2,
            &h,
            &pin(),
        )
        .unwrap();
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
        assert!(
            matches!(err, CardUnlockError::NoEnrolledCardPresent(ref s) if s.contains("laptop") && s.contains("S1"))
        );
        assert!(matches!(
            unlock_with(&f, "default", Some("S9"), &h, &pin()),
            Err(CardUnlockError::NotEnrolled(_))
        ));
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
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(CardUnlockFile::load(&path).unwrap().unwrap(), f);
        std::fs::write(&path, "version = 1\n[[cards]]\nserial = 3\n").unwrap();
        assert!(matches!(
            CardUnlockFile::load(&path),
            Err(CardUnlockError::BrokenFile { .. })
        ));
    }

    #[test]
    fn save_replaces_a_stale_wide_open_temp_file_with_a_0600_one() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("card-unlock.toml");
        let tmp = dir.path().join("card-unlock.toml.tmp");
        std::fs::write(&tmp, "stale").unwrap();
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644)).unwrap();
        let h = FakeHelper::with_card("S1", [5u8; 32]);
        enrolled(&h, "S1").save(&path).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn two_enrollments_at_once_keep_both_entries() {
        let dir = tempfile::tempdir().unwrap();
        let h = Arc::new(FakeHelper::with_card("S1", [5u8; 32]));
        h.add_card("S2", [6u8; 32]);
        let ctx = Arc::new(CardUnlockContext {
            instance: "default".into(),
            file_path: dir.path().join("card-unlock.toml"),
            helper: h,
            master: master(),
            can_lock: true,
            lockable: Arc::new(|| Ok(())),
            relock_requested: Arc::default(),
            enroll_lock: std::sync::Mutex::new(()),
        });
        let threads: Vec<_> = ["S1", "S2"]
            .into_iter()
            .map(|serial| {
                let ctx = Arc::clone(&ctx);
                std::thread::spawn(move || {
                    enroll_card(&ctx, serial, serial, &pin(), false, None).unwrap()
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        let file = CardUnlockFile::load(&ctx.file_path).unwrap().unwrap();
        assert_eq!(file.cards.len(), 2);
    }

    #[test]
    fn enrolling_the_same_card_again_replaces_its_entry_and_remove_deletes_it() {
        let h = FakeHelper::with_card("S1", [5u8; 32]);
        let mut f = enrolled(&h, "S1");
        let public = h.public_key("S1").unwrap().unwrap();
        enroll(
            &mut f,
            &master(),
            "default",
            "S1",
            "renamed",
            public,
            &h,
            &pin(),
        )
        .unwrap();
        assert_eq!(f.cards.len(), 1);
        assert_eq!(f.cards[0].label, "renamed");
        assert_eq!(remove(&mut f, "S1").unwrap().label, "renamed");
        assert!(matches!(
            remove(&mut f, "S1"),
            Err(CardUnlockError::NotEnrolled(_))
        ));
    }
}
