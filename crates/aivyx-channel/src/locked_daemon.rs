//! The locked daemon (Amendment A18): when PA starts with no passphrase
//! source but an enrolled card, this minimal server owns the normal socket
//! and answers only lock requests until an unlock yields the master key.
//! The full daemon then binds the same path (its bind renames over ours).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use aivyx_card_helper::{CardHelper, ErrorKind, HelperError};
use aivyx_crypto::MasterKey;
use aivyx_ipc::protocol::{
    DaemonLifecycleEvent, DaemonMessage, FrontendMessage, LockCardView, MAX_PAYLOAD_SIZE,
    PROTOCOL_VERSION, decode_frame, encode_frame,
};
use secrecy::SecretString;
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};

use crate::card_unlock::{CardUnlockError, CardUnlockFile};

/// A client that connects and never sends must not block unlocks forever.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

pub type FromPassphrase = Arc<dyn Fn(&SecretString) -> Result<MasterKey, String> + Send + Sync>;

pub struct LockedConfig {
    pub socket_path: PathBuf,
    pub instance: String,
    /// The cards at start-up: the fallback when a reload of `cards_path` fails.
    pub cards: CardUnlockFile,
    /// `card-unlock.toml`, re-read on each `Unlock` and `GetLockState` so a
    /// card removed (`aivyx-pa card remove`) stops working at once.
    pub cards_path: Option<PathBuf>,
    pub helper: Arc<dyn CardHelper>,
    /// Derives the master key from a passphrase (Argon2id + the store's salt).
    pub from_passphrase: FromPassphrase,
    /// Checks a candidate key really opens this store; a wrong passphrase fails here.
    pub verify: Arc<dyn Fn(&MasterKey) -> bool + Send + Sync>,
    pub backoff: Duration,
    pub max_failures: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnlockMethod {
    Card { serial: String },
    Passphrase,
}

pub enum LockedOutcome {
    Unlocked(MasterKey, UnlockMethod),
    Stopped,
}

async fn send<T: Serialize>(w: &mut OwnedWriteHalf, msg: &T) {
    if let Ok(frame) = encode_frame(msg) {
        let _ = w.write_all(&frame).await;
    }
}

/// Read until one full frame decodes; `None` on EOF, timeout, or a bad frame.
async fn read_one(r: &mut OwnedReadHalf) -> Option<FrontendMessage> {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match decode_frame::<FrontendMessage>(&buf) {
            Ok((msg, _)) => return Some(msg),
            Err(aivyx_ipc::protocol::FrameError::IncompleteBuf) => {}
            Err(_) => return None,
        }
        if buf.len() > MAX_PAYLOAD_SIZE as usize {
            return None;
        }
        let n = tokio::time::timeout(READ_TIMEOUT, r.read(&mut chunk))
            .await
            .ok()?
            .ok()?;
        if n == 0 {
            return None;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

fn failed(code: &str, message: impl Into<String>, tries_left: Option<u8>) -> DaemonMessage {
    DaemonMessage::UnlockFailed {
        code: code.into(),
        message: message.into(),
        tries_left,
    }
}

/// The enrolled cards now: `card-unlock.toml` re-read (a missing file has
/// none), or the start-up list when the reload fails. Blocking.
fn current_cards(path: Option<&std::path::Path>, fallback: &CardUnlockFile) -> CardUnlockFile {
    let Some(path) = path else {
        return fallback.clone();
    };
    match CardUnlockFile::load(path) {
        Ok(f) => f.unwrap_or_default(),
        Err(e) => {
            eprintln!("aivyx-pa: {e}; using the card list from start-up");
            fallback.clone()
        }
    }
}

async fn reload_cards(cfg: &LockedConfig) -> CardUnlockFile {
    let (path, fallback) = (cfg.cards_path.clone(), cfg.cards.clone());
    let backup = fallback.clone();
    tokio::task::spawn_blocking(move || current_cards(path.as_deref(), &fallback))
        .await
        .unwrap_or(backup)
}

fn lock_state(cards: &CardUnlockFile) -> DaemonMessage {
    DaemonMessage::LockState {
        locked: true,
        cards: cards
            .cards
            .iter()
            .map(|c| LockCardView {
                serial: c.serial.clone(),
                label: c.label.clone(),
            })
            .collect(),
    }
}

fn unlock_error(e: CardUnlockError) -> DaemonMessage {
    let message = e.to_string();
    match e {
        CardUnlockError::Helper(HelperError::Card {
            kind, tries_left, ..
        }) => {
            let code = match kind {
                ErrorKind::WrongPin => "wrong_pin",
                ErrorKind::PinBlocked => "pin_blocked",
                ErrorKind::CardNotFound | ErrorKind::NoReader => "no_card",
                ErrorKind::TouchTimeout => "touch_timeout",
                _ => "other",
            };
            failed(code, message, tries_left)
        }
        CardUnlockError::Helper(HelperError::NotInstalled) => {
            failed("helper_missing", message, None)
        }
        CardUnlockError::NoEnrolledCardPresent(_) => failed("no_card", message, None),
        CardUnlockError::NotEnrolled(_) => failed("not_enrolled", message, None),
        CardUnlockError::EntryDoesNotOpen { .. } => failed("entry", message, None),
        _ => failed("other", message, None),
    }
}

/// Failure counter shared by card and passphrase attempts.
struct Backoff {
    failures: u32,
    until: Option<tokio::time::Instant>,
}

impl Backoff {
    fn active(&self) -> bool {
        self.until.is_some_and(|u| tokio::time::Instant::now() < u)
    }

    fn fail(&mut self, cfg: &LockedConfig) {
        self.failures += 1;
        if self.failures >= cfg.max_failures {
            self.until = Some(tokio::time::Instant::now() + cfg.backoff);
            self.failures = 0;
        }
    }
}

pub async fn run_locked(cfg: LockedConfig) -> Result<LockedOutcome, String> {
    // A boot-time start finds no runtime folder yet; `run_daemon` makes it too.
    if let Some(parent) = cfg.socket_path.parent() {
        crate::daemon_server::create_dir_all_0700(parent)
            .map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    // The bind renames over the path, and the locked path opens no store
    // (whose lock would refuse a second daemon), so check here first.
    if crate::daemon_client::daemon_is_running(&cfg.socket_path).await {
        return Err(already_running(&cfg.socket_path));
    }
    let listener = crate::daemon_server::bind_unix_socket_0600(&cfg.socket_path)
        .map_err(|e| format!("bind {}: {e}", cfg.socket_path.display()))?;
    let mut backoff = Backoff {
        failures: 0,
        until: None,
    };
    loop {
        let (stream, _) = listener.accept().await.map_err(|e| e.to_string())?;
        let (mut r, mut w) = stream.into_split();
        send(
            &mut w,
            &DaemonLifecycleEvent::DaemonReady {
                version: PROTOCOL_VERSION.into(),
            },
        )
        .await;
        let Some(msg) = read_one(&mut r).await else {
            continue;
        };
        match msg {
            FrontendMessage::GetLockState => {
                send(&mut w, &lock_state(&reload_cards(&cfg).await)).await
            }
            FrontendMessage::Shutdown => {
                crate::daemon_server::send_shutting_down(&mut w, "stopped while locked").await;
                return Ok(LockedOutcome::Stopped);
            }
            FrontendMessage::Unlock { pin, serial } => {
                if backoff.active() {
                    send(&mut w, &too_many(&cfg)).await;
                    continue;
                }
                let helper = cfg.helper.clone();
                let verify = cfg.verify.clone();
                let (path, fallback) = (cfg.cards_path.clone(), cfg.cards.clone());
                let inst = cfg.instance.clone();
                let pin = SecretString::from(pin.into_inner());
                let res = tokio::task::spawn_blocking(move || {
                    let cards = current_cards(path.as_deref(), &fallback);
                    let (key, serial) = crate::card_unlock::unlock_with(
                        &cards,
                        &inst,
                        serial.as_deref(),
                        &*helper,
                        &pin,
                    )?;
                    // The entry decrypted, but is it this store's key?
                    if verify(&key) {
                        Ok((key, serial))
                    } else {
                        Err(CardUnlockError::EntryDoesNotOpen { serial })
                    }
                })
                .await
                .map_err(|e| e.to_string())?;
                match res {
                    Ok((key, serial)) => {
                        send(
                            &mut w,
                            &DaemonMessage::Unlocked {
                                method: format!("card {serial}"),
                            },
                        )
                        .await;
                        return Ok(LockedOutcome::Unlocked(key, UnlockMethod::Card { serial }));
                    }
                    Err(e) => {
                        backoff.fail(&cfg);
                        send(&mut w, &unlock_error(e)).await;
                    }
                }
            }
            FrontendMessage::UnlockWithPassphrase { passphrase } => {
                if backoff.active() {
                    send(&mut w, &too_many(&cfg)).await;
                    continue;
                }
                let derive = cfg.from_passphrase.clone();
                let verify = cfg.verify.clone();
                let passphrase = SecretString::from(passphrase.into_inner());
                let key = tokio::task::spawn_blocking(move || {
                    derive(&passphrase).map(|k| Some(k).filter(|k| verify(k)))
                })
                .await
                .map_err(|e| e.to_string())?;
                match key {
                    // Not a wrong guess (the salt file, Argon2): log the
                    // reason (never the input) and don't count it.
                    Err(e) => {
                        eprintln!("aivyx-pa: deriving the key from the passphrase failed: {e}");
                        send(
                            &mut w,
                            &failed("other", format!("deriving the key failed: {e}"), None),
                        )
                        .await;
                    }
                    Ok(Some(key)) => {
                        send(
                            &mut w,
                            &DaemonMessage::Unlocked {
                                method: "passphrase".into(),
                            },
                        )
                        .await;
                        return Ok(LockedOutcome::Unlocked(key, UnlockMethod::Passphrase));
                    }
                    Ok(None) => {
                        backoff.fail(&cfg);
                        send(
                            &mut w,
                            &failed(
                                "wrong_passphrase",
                                "That passphrase doesn't open this store",
                                None,
                            ),
                        )
                        .await;
                    }
                }
            }
            _ => {
                send(
                    &mut w,
                    &DaemonMessage::Locked {
                        message: "PA is locked — run `aivyx-pa unlock`".into(),
                    },
                )
                .await
            }
        }
    }
}

/// The full daemon's wording for the same collision (aivyx.rs).
fn already_running(socket: &std::path::Path) -> String {
    format!(
        "a daemon is already running on {}. Stop it first with `aivyx-pa daemon stop`.",
        socket.display()
    )
}

fn too_many(cfg: &LockedConfig) -> DaemonMessage {
    let secs = cfg.backoff.as_secs_f64().ceil() as u64;
    failed(
        "backoff",
        format!("Too many failed unlocks — try again in {secs} s"),
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_card_helper::FakeHelper;
    use aivyx_ipc::protocol::DaemonEnvelope;
    use secrecy::ExposeSecret;
    use std::path::Path;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixStream;

    fn master() -> MasterKey {
        MasterKey::from_raw([3u8; 32])
    }

    fn config(dir: &Path, helper: Arc<FakeHelper>) -> LockedConfig {
        let mut cards = CardUnlockFile::default();
        let public = helper.public_key("S1").unwrap().unwrap();
        crate::card_unlock::enroll(
            &mut cards,
            &master(),
            "default",
            "S1",
            "laptop",
            public,
            &*helper,
            &SecretString::from("123456"),
        )
        .unwrap();
        LockedConfig {
            socket_path: dir.join("d.sock"),
            instance: "default".into(),
            cards,
            cards_path: None,
            helper,
            from_passphrase: Arc::new(|p| {
                if p.expose_secret() == "right" {
                    Ok(MasterKey::from_raw([3u8; 32]))
                } else {
                    Ok(MasterKey::from_raw([9u8; 32]))
                }
            }),
            verify: Arc::new(|m| m.same_as(&MasterKey::from_raw([3u8; 32]))),
            backoff: Duration::from_secs(1),
            max_failures: 2,
        }
    }

    /// Connect, read DaemonReady, send one message, read one reply.
    async fn ask(path: &Path, m: FrontendMessage) -> DaemonEnvelope {
        let mut s = UnixStream::connect(path).await.unwrap();
        let mut buf = vec![0u8; 65536];
        let n = s.read(&mut buf).await.unwrap();
        let (ready, _) = decode_frame::<DaemonEnvelope>(&buf[..n]).unwrap();
        assert!(matches!(ready, DaemonEnvelope::DaemonReady { .. }));
        s.write_all(&encode_frame(&m).unwrap()).await.unwrap();
        let n = s.read(&mut buf).await.unwrap();
        decode_frame::<DaemonEnvelope>(&buf[..n]).unwrap().0
    }

    async fn wait_for(path: &Path) {
        for _ in 0..500 {
            if path.exists() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("socket never appeared");
    }

    #[tokio::test]
    async fn it_refuses_everything_but_lock_requests_then_unlocks_with_the_card() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(dir.path(), Arc::new(FakeHelper::with_card("S1", [5u8; 32])));
        let path = cfg.socket_path.clone();
        let task = tokio::spawn(run_locked(cfg));
        wait_for(&path).await;
        let r = ask(
            &path,
            FrontendMessage::StartSession {
                role: None,
                frontend_type: None,
            },
        )
        .await;
        assert!(
            matches!(r, DaemonEnvelope::Locked { ref message } if message.contains("aivyx-pa unlock"))
        );
        let r = ask(&path, FrontendMessage::GetLockState).await;
        assert!(
            matches!(r, DaemonEnvelope::LockState { locked: true, ref cards } if cards[0].label == "laptop")
        );
        let r = ask(
            &path,
            FrontendMessage::Unlock {
                pin: "123456".into(),
                serial: None,
            },
        )
        .await;
        assert!(matches!(r, DaemonEnvelope::Unlocked { ref method } if method == "card S1"));
        let LockedOutcome::Unlocked(m, UnlockMethod::Card { serial }) =
            task.await.unwrap().unwrap()
        else {
            panic!()
        };
        assert!(m.same_as(&master()));
        assert_eq!(serial, "S1");
    }

    #[tokio::test]
    async fn a_wrong_pin_reports_tries_and_repeated_failures_back_off() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(dir.path(), Arc::new(FakeHelper::with_card("S1", [5u8; 32])));
        let path = cfg.socket_path.clone();
        let _task = tokio::spawn(run_locked(cfg));
        wait_for(&path).await;
        let r = ask(
            &path,
            FrontendMessage::Unlock {
                pin: "000000".into(),
                serial: None,
            },
        )
        .await;
        assert!(
            matches!(r, DaemonEnvelope::UnlockFailed { ref code, tries_left: Some(2), .. } if code == "wrong_pin")
        );
        let _ = ask(
            &path,
            FrontendMessage::UnlockWithPassphrase {
                passphrase: "wrong".into(),
            },
        )
        .await;
        let r = ask(
            &path,
            FrontendMessage::Unlock {
                pin: "123456".into(),
                serial: None,
            },
        )
        .await;
        assert!(
            matches!(r, DaemonEnvelope::UnlockFailed { ref code, ref message, .. } if code == "backoff" && message.contains("in 1 s"))
        );
        tokio::time::sleep(Duration::from_millis(1300)).await;
        let r = ask(
            &path,
            FrontendMessage::Unlock {
                pin: "123456".into(),
                serial: None,
            },
        )
        .await;
        assert!(matches!(r, DaemonEnvelope::Unlocked { .. }));
    }

    #[tokio::test]
    async fn the_passphrase_unlocks_and_a_wrong_one_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(dir.path(), Arc::new(FakeHelper::with_card("S1", [5u8; 32])));
        let path = cfg.socket_path.clone();
        let task = tokio::spawn(run_locked(cfg));
        wait_for(&path).await;
        let r = ask(
            &path,
            FrontendMessage::UnlockWithPassphrase {
                passphrase: "nope".into(),
            },
        )
        .await;
        assert!(
            matches!(r, DaemonEnvelope::UnlockFailed { ref code, .. } if code == "wrong_passphrase")
        );
        let r = ask(
            &path,
            FrontendMessage::UnlockWithPassphrase {
                passphrase: "right".into(),
            },
        )
        .await;
        assert!(matches!(r, DaemonEnvelope::Unlocked { ref method } if method == "passphrase"));
        assert!(matches!(
            task.await.unwrap().unwrap(),
            LockedOutcome::Unlocked(_, UnlockMethod::Passphrase)
        ));
    }

    #[tokio::test]
    async fn a_card_key_that_does_not_open_the_store_is_refused_and_counted() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = config(dir.path(), Arc::new(FakeHelper::with_card("S1", [5u8; 32])));
        // The entry decrypts, but to a key this store doesn't take.
        cfg.verify = Arc::new(|_| false);
        cfg.max_failures = 1;
        let path = cfg.socket_path.clone();
        let _task = tokio::spawn(run_locked(cfg));
        wait_for(&path).await;
        let unlock = || FrontendMessage::Unlock {
            pin: "123456".into(),
            serial: None,
        };
        let r = ask(&path, unlock()).await;
        assert!(
            matches!(r, DaemonEnvelope::UnlockFailed { ref code, .. } if code == "entry"),
            "{r:?}"
        );
        let r = ask(&path, unlock()).await;
        assert!(
            matches!(r, DaemonEnvelope::UnlockFailed { ref code, .. } if code == "backoff"),
            "{r:?}"
        );
    }

    #[tokio::test]
    async fn a_card_removed_from_the_file_stops_working_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = config(dir.path(), Arc::new(FakeHelper::with_card("S1", [5u8; 32])));
        let file = dir.path().join("card-unlock.toml");
        cfg.cards.save(&file).unwrap();
        cfg.cards_path = Some(file.clone());
        let path = cfg.socket_path.clone();
        let _task = tokio::spawn(run_locked(cfg));
        wait_for(&path).await;
        // `aivyx-pa card remove S1` while locked.
        let mut f = CardUnlockFile::load(&file).unwrap().unwrap();
        crate::card_unlock::remove(&mut f, "S1").unwrap();
        f.save(&file).unwrap();
        let r = ask(&path, FrontendMessage::GetLockState).await;
        assert!(
            matches!(r, DaemonEnvelope::LockState { ref cards, .. } if cards.is_empty()),
            "{r:?}"
        );
        let r = ask(
            &path,
            FrontendMessage::Unlock {
                pin: "123456".into(),
                serial: None,
            },
        )
        .await;
        assert!(
            matches!(r, DaemonEnvelope::UnlockFailed { ref code, .. } if code == "no_card"),
            "{r:?}"
        );
        // A broken file falls back to the start-up list.
        std::fs::write(&file, "not toml [").unwrap();
        let r = ask(&path, FrontendMessage::GetLockState).await;
        assert!(
            matches!(r, DaemonEnvelope::LockState { ref cards, .. } if cards.len() == 1),
            "{r:?}"
        );
    }

    #[tokio::test]
    async fn a_failed_derive_is_reported_and_not_counted() {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = config(dir.path(), Arc::new(FakeHelper::with_card("S1", [5u8; 32])));
        cfg.from_passphrase = Arc::new(|_| Err("salt file unreadable".into()));
        cfg.max_failures = 1;
        let path = cfg.socket_path.clone();
        let _task = tokio::spawn(run_locked(cfg));
        wait_for(&path).await;
        for _ in 0..2 {
            let r = ask(
                &path,
                FrontendMessage::UnlockWithPassphrase {
                    passphrase: "secret-input".into(),
                },
            )
            .await;
            match r {
                DaemonEnvelope::UnlockFailed { code, message, .. } => {
                    assert_eq!(code, "other");
                    assert!(message.contains("salt file unreadable"), "{message}");
                    assert!(!message.contains("secret-input"), "{message}");
                }
                other => panic!("got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn a_live_daemon_on_the_socket_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(dir.path(), Arc::new(FakeHelper::with_card("S1", [5u8; 32])));
        let path = cfg.socket_path.clone();
        // A daemon already answers on the path.
        let live = tokio::net::UnixListener::bind(&path).unwrap();
        let server = tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = live.accept().await else {
                    return;
                };
                let ready = DaemonLifecycleEvent::DaemonReady {
                    version: PROTOCOL_VERSION.into(),
                };
                let _ = s.write_all(&encode_frame(&ready).unwrap()).await;
            }
        });
        use std::os::unix::fs::MetadataExt;
        let inode = std::fs::metadata(&path).unwrap().ino();
        let err = match run_locked(cfg).await {
            Err(e) => e,
            Ok(_) => panic!("a locked start must not take a live daemon's socket"),
        };
        assert!(err.contains("already running"), "{err}");
        assert_eq!(std::fs::metadata(&path).unwrap().ino(), inode);
        // The live daemon still answers.
        let mut s = UnixStream::connect(&path).await.unwrap();
        let mut buf = vec![0u8; 4096];
        let n = s.read(&mut buf).await.unwrap();
        assert!(matches!(
            decode_frame::<DaemonEnvelope>(&buf[..n]).unwrap().0,
            DaemonEnvelope::DaemonReady { .. }
        ));
        server.abort();
    }

    #[tokio::test]
    async fn shutdown_stops_a_locked_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = config(dir.path(), Arc::new(FakeHelper::with_card("S1", [5u8; 32])));
        let path = cfg.socket_path.clone();
        let task = tokio::spawn(run_locked(cfg));
        wait_for(&path).await;
        let _ = ask(&path, FrontendMessage::Shutdown).await;
        assert!(matches!(
            task.await.unwrap().unwrap(),
            LockedOutcome::Stopped
        ));
    }
}
