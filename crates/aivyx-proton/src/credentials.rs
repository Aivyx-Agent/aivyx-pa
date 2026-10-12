//! The Bridge login and certificate pins, in
//! `<tool_process_dir("proton")>/credentials.toml` (mode `0600`).
//!
//! Not the OS keyring: the `proton.*` tools run under the bundled
//! bubblewrap sandbox, which hides `$HOME` except this folder and has no
//! session bus — the same posture as Gmail's `tokens.json`. Written only by
//! `aivyx-pa connect proton`; read by the channel and the tool process.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use aivyx_instance::InstancePaths;

use serde::{Deserialize, Serialize};

use crate::bridge::{BridgeDial, BridgeTransport, Login};
use crate::tls;

pub const CREDENTIALS_FILE: &str = "credentials.toml";

/// `<tool_process_dir("proton")>/credentials.toml` for `instance` — the
/// folder the bundled sandbox binds for the `proton` tool process.
pub fn path_for(instance: &InstancePaths) -> Option<PathBuf> {
    instance
        .tool_process_dir("proton")
        .map(|d| d.join(CREDENTIALS_FILE))
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BridgeCredentials {
    /// Bridge's IMAP/SMTP username (Bridge → account → mailbox details).
    pub username: String,
    /// Bridge's own generated password — not your Proton password.
    pub password: String,
    pub imap_addr: String,
    pub smtp_addr: String,
    /// Hex SHA-256 of each port's certificate.
    pub imap_pin: String,
    pub smtp_pin: String,
    /// What `proton.draft` / `proton.send` send from: your first
    /// `own_addresses` entry.
    pub from_address: String,
}

impl fmt::Debug for BridgeCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BridgeCredentials")
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .field("imap_addr", &self.imap_addr)
            .field("smtp_addr", &self.smtp_addr)
            .field("from_address", &self.from_address)
            .finish()
    }
}

impl BridgeCredentials {
    pub fn dial(&self) -> Result<BridgeDial, String> {
        Ok(BridgeDial {
            imap_addr: self.imap_addr.clone(),
            smtp_addr: self.smtp_addr.clone(),
            imap_pin: tls::pin_from_hex(&self.imap_pin)?,
            smtp_pin: tls::pin_from_hex(&self.smtp_pin)?,
        })
    }

    /// The real transport; `folder` is where handled mail goes.
    pub fn transport(&self, folder: &str) -> Result<BridgeTransport, String> {
        Ok(BridgeTransport::new(
            Arc::new(self.dial()?),
            Login {
                username: self.username.clone(),
                password: self.password.clone(),
            },
            folder,
        ))
    }
}

/// `None` when `connect proton` hasn't run. Refuses a file others can read.
pub fn load(path: &Path) -> Result<Option<BridgeCredentials>, String> {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(path)
            .map_err(|e| format!("read {}: {e}", path.display()))?
            .permissions()
            .mode();
        if mode & 0o077 != 0 {
            return Err(format!(
                "{} is readable by other users — run `chmod 600 {}` (or `aivyx-pa connect proton` again)",
                path.display(),
                path.display()
            ));
        }
    }
    let text = String::from_utf8(bytes).map_err(|e| format!("read {}: {e}", path.display()))?;
    toml::from_str(&text)
        .map(Some)
        .map_err(|e| format!("read {}: {e}", path.display()))
}

/// Creates the folder (`0700`) and writes the file `0600` via a temporary
/// file and a rename.
pub fn save(path: &Path, creds: &BridgeCredentials) -> Result<(), String> {
    let dir = path
        .parent()
        .ok_or_else(|| format!("{} has no folder", path.display()))?;
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(dir)
        .map_err(|e| format!("create {}: {e}", dir.display()))?;
    let text = toml::to_string(creds).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("toml.tmp");
    // Never reuse a leftover (or planted) temporary file: its permissions
    // would survive and the rename.
    match std::fs::remove_file(&tmp) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("remove {}: {e}", tmp.display())),
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    {
        use std::io::Write;
        let mut f = options
            .open(&tmp)
            .map_err(|e| format!("write {}: {e}", tmp.display()))?;
        f.write_all(text.as_bytes())
            .map_err(|e| format!("write {}: {e}", tmp.display()))?;
    }
    std::fs::rename(&tmp, path).map_err(|e| format!("write {}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn creds() -> BridgeCredentials {
        BridgeCredentials {
            username: "me@proton.me".into(),
            password: "bridge-pw".into(),
            imap_addr: "127.0.0.1:1143".into(),
            smtp_addr: "127.0.0.1:1025".into(),
            imap_pin: "00".repeat(32),
            smtp_pin: "11".repeat(32),
            from_address: "me@proton.me".into(),
        }
    }

    #[test]
    fn save_then_load_round_trips_privately() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("proton").join(CREDENTIALS_FILE);
        assert_eq!(load(&path).unwrap(), None);
        save(&path, &creds()).unwrap();
        assert_eq!(load(&path).unwrap(), Some(creds()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
            assert_eq!(
                std::fs::metadata(path.parent().unwrap()).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_file_others_can_read_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CREDENTIALS_FILE);
        save(&path, &creds()).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load(&path).unwrap_err().contains("chmod 600"));
    }

    #[cfg(unix)]
    #[test]
    fn a_stale_temporary_file_is_never_reused() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CREDENTIALS_FILE);
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, "planted").unwrap();
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644)).unwrap();
        save(&path, &creds()).unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(load(&path).unwrap(), Some(creds()));
        assert!(!tmp.exists());
    }

    #[test]
    fn the_file_lives_in_the_instances_proton_tool_folder() {
        let dirs = aivyx_instance::BaseDirs {
            home: Some(PathBuf::from("/h")),
            xdg_config_home: None,
            xdg_data_home: None,
            xdg_runtime_dir: None,
        };
        let work = InstancePaths::new(aivyx_instance::InstanceName::parse("work").unwrap(), dirs);
        assert_eq!(
            path_for(&work),
            Some(PathBuf::from("/h/.aivyx-pa/instances/work/tool-processes/proton/credentials.toml"))
        );
    }

    #[test]
    fn the_password_never_prints_and_pins_decode() {
        let c = creds();
        assert!(!format!("{c:?}").contains("bridge-pw"));
        let d = c.dial().unwrap();
        assert_eq!(d.imap_pin, [0u8; 32]);
        assert_eq!(d.smtp_pin, [0x11u8; 32]);
        let bad = BridgeCredentials { imap_pin: "zz".into(), ..creds() };
        assert!(bad.dial().is_err());
    }
}
