use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use secrecy::{ExposeSecret, SecretString};
use zeroize::Zeroizing;

use crate::{CardHelper, CardInfo, HelperError, Reply, Request, decode_key, encode_key};

pub const HELPER_NAME: &str = "aivyx-pa-card";

/// Backstop: the card's own touch window is ~15 s and PIN entry happens
/// before the call, so a helper silent for this long is wedged.
const HELPER_TIMEOUT: Duration = Duration::from_secs(60);

/// Runs `aivyx-pa-card` once per call: the request goes on stdin (never
/// argv, so a PIN can't show in `ps`), the reply comes back on stdout.
pub struct ProcessHelper {
    path: PathBuf,
    timeout: Duration,
    /// Tests run their stub scripts as `/bin/sh <script>`: sh only reads the
    /// file, so a concurrent fork still holding it open for writing can't make
    /// the spawn fail with ETXTBSY (as exec'ing the script itself can).
    #[cfg(test)]
    interpreter: Option<PathBuf>,
}

impl ProcessHelper {
    pub fn at(path: PathBuf) -> Self {
        Self {
            path,
            timeout: HELPER_TIMEOUT,
            #[cfg(test)]
            interpreter: None,
        }
    }

    #[cfg(test)]
    fn with_interpreter(mut self, interpreter: PathBuf) -> Self {
        self.interpreter = Some(interpreter);
        self
    }

    #[cfg(not(test))]
    fn command(&self) -> Command {
        Command::new(&self.path)
    }

    #[cfg(test)]
    fn command(&self) -> Command {
        match &self.interpreter {
            Some(sh) => {
                let mut c = Command::new(sh);
                c.arg(&self.path);
                c
            }
            None => Command::new(&self.path),
        }
    }

    #[cfg(test)]
    fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
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

    // The `Request` lives in this long-running parent (the daemon or the
    // CLI), so its PIN fields are `Sensitive` and zeroize on drop; the
    // serialized body and the raw reply are zeroized too.
    fn call(&self, req: &Request) -> Result<Reply, HelperError> {
        // Serialize first: nothing to clean up if it fails.
        let body = Zeroizing::new(
            serde_json::to_vec(req).map_err(|e| HelperError::Broken(e.to_string()))?,
        );
        let mut cmd = self.command();
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .env_clear();
        for (k, v) in helper_env(|k| std::env::var_os(k)) {
            cmd.env(k, v);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| HelperError::Broken(format!("start {}: {e}", self.path.display())))?;

        // Read stdout on its own thread so the wait below can time out.
        let mut stdout_pipe = child.stdout.take().expect("stdout is piped");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = stdout_pipe.read_to_end(&mut buf);
            let _ = tx.send(Zeroizing::new(buf));
        });

        // A failed write (e.g. EPIPE: the helper exited early) is not fatal
        // yet: the helper may still have printed a reply explaining why.
        let mut stdin = child.stdin.take().expect("stdin is piped");
        let write_err = stdin.write_all(&body).err();
        // The child sees EOF on stdin only once this handle drops.
        drop(stdin);

        let deadline = Instant::now() + self.timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(st)) => break st,
                Ok(None) if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(HelperError::Broken(format!(
                        "the card helper didn't answer within {} s",
                        self.timeout.as_secs()
                    )));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(HelperError::Broken(e.to_string()));
                }
            }
        };
        // The child has exited and been reaped; its pipe closes with it
        // (the grace only matters if it left a grandchild holding stdout).
        let stdout = rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap_or_else(|_| Zeroizing::new(Vec::new()));
        match serde_json::from_slice::<Reply>(&stdout) {
            Ok(reply) => Ok(reply),
            Err(e) => Err(HelperError::Broken(match write_err {
                Some(w) => format!("writing the request failed ({w}); helper exited {status}"),
                None => format!("bad reply (exit {status}): {e}"),
            })),
        }
    }
}

/// The environment the helper keeps after `env_clear()`: `PATH`, and
/// pcsc-lite's `PCSCLITE_CSOCK_NAME` when set (a non-default pcscd socket,
/// e.g. in a container or a Flatpak), without which it finds no reader.
fn helper_env(
    get: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> Vec<(&'static str, std::ffi::OsString)> {
    let mut env = vec![("PATH", get("PATH").unwrap_or_default())];
    if let Some(sock) = get("PCSCLITE_CSOCK_NAME") {
        env.push(("PCSCLITE_CSOCK_NAME", sock));
    }
    env
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
            admin_pin: admin_pin.expose_secret().into(),
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
            pin: pin.expose_secret().into(),
            their_public: encode_key(their_public),
        };
        match self.call(&req)? {
            Reply::Shared { secret } => decode_key(secret.expose())
                .map(Zeroizing::new)
                .ok_or_else(|| HelperError::Broken("bad shared secret".into())),
            r => Err(r.into()),
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A stub helper script, run as `/bin/sh <script>` (not exec'd, so
    /// it needs no exec bit and can't hit ETXTBSY under parallel tests).
    fn stub(dir: &tempfile::TempDir, body: &str) -> ProcessHelper {
        let p = dir.path().join("stub.sh");
        std::fs::write(&p, format!("{body}\n")).unwrap();
        ProcessHelper::at(p)
            .with_interpreter(PathBuf::from("/bin/sh"))
            .with_timeout(Duration::from_secs(5))
    }

    #[test]
    fn list_round_trips_and_stdin_carries_the_request_not_argv() {
        let d = tempfile::tempdir().unwrap();
        let (inp, args) = (d.path().join("in"), d.path().join("args"));
        let h = stub(
            &d,
            &format!(
                "printf '%s' \"$*\" > {a}\ncat > {i}\necho '{{\"result\":\"cards\",\"cards\":[]}}'",
                a = args.display(),
                i = inp.display()
            ),
        );
        assert!(h.list().unwrap().is_empty());
        assert!(!std::fs::read_to_string(&inp).unwrap().is_empty());
        assert_eq!(std::fs::read_to_string(&args).unwrap(), "");
    }

    #[test]
    fn ecdh_pin_goes_on_stdin_only() {
        let d = tempfile::tempdir().unwrap();
        let (inp, args) = (d.path().join("in"), d.path().join("args"));
        let secret = encode_key(&[9u8; 32]);
        let h = stub(
            &d,
            &format!(
                "printf '%s' \"$*\" > {a}\ncat > {i}\necho '{{\"result\":\"shared\",\"secret\":\"{secret}\"}}'",
                a = args.display(),
                i = inp.display()
            ),
        );
        // A valid reply, so a failed spawn can't pass unnoticed.
        let shared = h
            .ecdh("S1", &SecretString::from("424242"), &[7u8; 32])
            .unwrap();
        assert_eq!(*shared, [9u8; 32]);
        assert!(std::fs::read_to_string(&inp).unwrap().contains("424242"));
        assert_eq!(std::fs::read_to_string(&args).unwrap(), "");
    }

    #[test]
    fn garbage_stdout_is_broken() {
        let d = tempfile::tempdir().unwrap();
        let h = stub(&d, "cat > /dev/null\necho 'not json'");
        assert!(matches!(h.list(), Err(HelperError::Broken(_))));
    }

    #[test]
    fn empty_stdout_with_exit_1_is_broken() {
        let d = tempfile::tempdir().unwrap();
        let h = stub(&d, "cat > /dev/null\nexit 1");
        assert!(matches!(h.list(), Err(HelperError::Broken(_))));
    }

    #[test]
    fn an_early_exit_reply_wins_over_the_write_error() {
        let d = tempfile::tempdir().unwrap();
        let h = stub(
            &d,
            "echo '{\"result\":\"error\",\"kind\":\"card_not_found\",\"message\":\"nope\"}'\nexit 2",
        );
        // Big request so the write is likely to hit EPIPE after the exit.
        let r = h.ecdh("S1", &SecretString::from("1".repeat(1 << 20)), &[1u8; 32]);
        assert!(matches!(r, Err(HelperError::Card { .. })), "got {r:?}");
    }

    #[test]
    fn a_silent_helper_is_killed_at_the_timeout() {
        let d = tempfile::tempdir().unwrap();
        let h = stub(&d, "sleep 30").with_timeout(Duration::from_millis(300));
        let t = Instant::now();
        let r = h.list();
        assert!(t.elapsed() < Duration::from_secs(5));
        match r {
            Err(HelperError::Broken(m)) => assert!(m.contains("didn't answer"), "{m}"),
            other => panic!("got {other:?}"),
        }
    }

    #[test]
    fn the_helper_keeps_path_and_the_pcscd_socket_only() {
        let env = helper_env(|k| match k {
            "PATH" => Some("/usr/bin".into()),
            "PCSCLITE_CSOCK_NAME" => Some("/run/pcscd/alt.comm".into()),
            _ => Some("leak".into()),
        });
        assert_eq!(
            env,
            vec![
                ("PATH", "/usr/bin".into()),
                ("PCSCLITE_CSOCK_NAME", "/run/pcscd/alt.comm".into())
            ]
        );
        let env = helper_env(|k| (k == "PATH").then(|| "/bin".into()));
        assert_eq!(env, vec![("PATH", "/bin".into())]);
    }

    #[test]
    fn a_missing_binary_is_broken() {
        let h = ProcessHelper::at(PathBuf::from("/nonexistent/aivyx-pa-card"));
        assert!(matches!(h.list(), Err(HelperError::Broken(_))));
    }
}
