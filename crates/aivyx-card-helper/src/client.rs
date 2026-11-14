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
                std::env::split_paths(&paths)
                    .map(|d| d.join(HELPER_NAME))
                    .find(|p| p.is_file())
            })
            .map(Self::at)
            .ok_or(HelperError::NotInstalled)
    }

    // The `Request` holds the PIN as a plain `String` (not zeroized); that is
    // acceptable because the helper process exits right after the call. The
    // serialized body and the reply are zeroized.
    fn call(&self, req: &Request) -> Result<Reply, HelperError> {
        let mut child = Command::new(&self.path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .spawn()
            .map_err(|e| HelperError::Broken(format!("start {}: {e}", self.path.display())))?;
        let body = Zeroizing::new(
            serde_json::to_vec(req).map_err(|e| HelperError::Broken(e.to_string()))?,
        );
        child
            .stdin
            .take()
            .ok_or_else(|| HelperError::Broken("no stdin".into()))?
            .write_all(&body)
            .map_err(|e| HelperError::Broken(e.to_string()))?;
        let out = child
            .wait_with_output()
            .map_err(|e| HelperError::Broken(e.to_string()))?;
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
        match self.call(&Request::PublicKey {
            serial: serial.into(),
        })? {
            Reply::PublicKey { public: None } => Ok(None),
            Reply::PublicKey { public: Some(p) } => decode_key(&p)
                .map(Some)
                .ok_or_else(|| HelperError::Broken("bad public key".into())),
            r => Err(r.into()),
        }
    }

    fn generate(&self, serial: &str, admin_pin: &SecretString) -> Result<[u8; 32], HelperError> {
        let req = Request::Generate {
            serial: serial.into(),
            admin_pin: admin_pin.expose_secret().to_string(),
        };
        match self.call(&req)? {
            Reply::PublicKey { public: Some(p) } => {
                decode_key(&p).ok_or_else(|| HelperError::Broken("bad public key".into()))
            }
            r => Err(r.into()),
        }
    }

    fn ecdh(
        &self,
        serial: &str,
        pin: &SecretString,
        their_public: &[u8; 32],
    ) -> Result<Zeroizing<[u8; 32]>, HelperError> {
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
