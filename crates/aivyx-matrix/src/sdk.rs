//! The real transport over `matrix-sdk`: restore the sealed session,
//! open the encrypted SQLite store, sync in the background and turn
//! room events into `Inbound` items.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use matrix_sdk::{
    authentication::matrix::MatrixSession,
    config::SyncSettings,
    deserialized_responses::{EncryptionInfo, VerificationState},
    ruma::{
        api::{client::uiaa, error::ErrorKind},
        events::room::{
            encrypted::OriginalSyncRoomEncryptedEvent,
            member::StrippedRoomMemberEvent,
            message::{MessageType, OriginalSyncRoomMessageEvent, RoomMessageEventContent},
        },
        OwnedDeviceId, OwnedUserId, RoomId,
    },
    store::RoomLoadSettings,
    Client, Room, RoomState, SessionMeta, SessionTokens,
};
use matrix_sdk_crypto::CollectStrategy;
use tokio::sync::{mpsc, Mutex};
use tokio::task::AbortHandle;

use crate::credentials::Session;
use crate::transport::{Inbound, IncomingMessage, MatrixTransport, OutgoingMessage, TransportError};

/// Every client shares room keys only with devices their owner has
/// cross-signed (`IdentityBasedStrategy`), not the SDK's default of every
/// device in the room: a malicious homeserver, or someone with the
/// operator's password, can add a device to the operator's account but
/// can't cross-sign it, so it never receives the keys for a reply.
async fn client(homeserver: &str, store_path: &Path, passphrase: &str) -> Result<Client, String> {
    Client::builder()
        .homeserver_url(homeserver)
        .with_room_key_recipient_strategy(CollectStrategy::IdentityBasedStrategy)
        .sqlite_store(store_path, Some(passphrase))
        .build()
        .await
        .map_err(|e| format!("matrix client: {e}"))
}

fn unix_ts() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Moves a freshly-logged-in store into place: an existing `store` becomes
/// `store.old-<ts>` (never deleted), then `store.new-<ts>` becomes `store`.
/// Returns the archive path if there was an old store. If the second rename
/// fails the old store is put back.
fn swap_in_new_store(store_dir: &Path, ts: u64) -> Result<Option<PathBuf>, String> {
    let (live, fresh) = (store_dir.join("store"), store_dir.join(format!("store.new-{ts}")));
    let archived = if live.exists() {
        let old = store_dir.join(format!("store.old-{ts}"));
        std::fs::rename(&live, &old).map_err(|e| format!("move old store aside to {}: {e}", old.display()))?;
        Some(old)
    } else {
        None
    };
    if let Err(e) = std::fs::rename(&fresh, &live) {
        if let Some(old) = &archived {
            let _ = std::fs::rename(old, &live);
        }
        return Err(format!("install new store {}: {e}", live.display()));
    }
    Ok(archived)
}

/// Logs in once with a password and returns the session to seal, plus the
/// path an earlier store was archived to (if there was one). The password is
/// not kept.
///
/// The login runs against a scratch store (`store.new-<ts>`). Only on success
/// is an existing `store` renamed to `store.old-<ts>` (never deleted; the old
/// device's account would otherwise fail with a mismatch) and the scratch store
/// moved into place. On failure the scratch store is removed and `store` is
/// untouched.
pub async fn login(
    homeserver: &str,
    store_dir: &Path,
    passphrase: &str,
    user: &str,
    password: &str,
) -> Result<(Session, Option<PathBuf>), String> {
    let ts = unix_ts();
    let scratch = store_dir.join(format!("store.new-{ts}"));
    let result = async {
        let client = client(homeserver, &scratch, passphrase).await?;
        let r = client
            .matrix_auth()
            .login_username(user, password)
            .initial_device_display_name("Aivyx PA")
            .await
            .map_err(|e| format!("matrix login failed: {e}"))?;
        bootstrap_cross_signing(&client, r.user_id.as_str(), password).await?;
        Ok::<Session, String>(Session {
            homeserver: homeserver.to_string(),
            user_id: r.user_id.to_string(),
            device_id: r.device_id.to_string(),
            access_token: r.access_token,
        })
    }
    .await;
    match result {
        Ok(session) => swap_in_new_store(store_dir, ts).map(|archived| (session, archived)),
        Err(e) => {
            let _ = std::fs::remove_dir_all(&scratch);
            Err(e)
        }
    }
}

/// Creates the assistant's own cross-signing identity (master, self-signing
/// and user-signing keys) and signs this device with it. Without it the
/// assistant can't sign the operator's identity during verification, and
/// `matrix-sdk` never reports the operator's messages as `Verified`.
///
/// The upload needs user-interactive auth: the first attempt goes without
/// auth (some servers accept that for a first identity); if the server
/// answers with a UIAA challenge, retry once with the password and the
/// challenge's session. The private keys stay in the encrypted store. The
/// password is only borrowed for the call; it isn't logged or kept.
async fn bootstrap_cross_signing(client: &Client, user_id: &str, password: &str) -> Result<(), String> {
    let e = match client.encryption().bootstrap_cross_signing(None).await {
        Ok(()) => return Ok(()),
        Err(e) => e,
    };
    let Some(uiaa) = e.as_uiaa_response() else {
        return Err(format!("matrix cross-signing setup failed: {e}"));
    };
    let mut auth = uiaa::Password::new(
        uiaa::UserIdentifier::Matrix(uiaa::MatrixUserIdentifier::new(user_id.to_string())),
        password.to_string(),
    );
    auth.session = uiaa.session.clone();
    client
        .encryption()
        .bootstrap_cross_signing(Some(uiaa::AuthData::Password(auth)))
        .await
        .map_err(|e| format!("matrix cross-signing setup failed: {e}"))
}

/// Backoff before the next sync attempt: 1 s doubling, capped at 60 s.
pub(crate) fn backoff(failures: u32) -> Duration {
    Duration::from_secs((1u64 << failures.min(6)).min(60))
}

/// A rejected token is `Unauthorized`; anything else is a transient
/// `Platform` failure.
fn classify(kind: Option<&ErrorKind>, text: String) -> TransportError {
    match kind {
        Some(ErrorKind::UnknownToken { .. }) => TransportError::Unauthorized(text),
        _ => TransportError::Platform(text),
    }
}

fn classify_sdk(e: &matrix_sdk::Error) -> TransportError {
    classify(e.client_api_error_kind(), e.to_string())
}

pub(crate) async fn restore(session: &Session, store_dir: &Path, passphrase: &str) -> Result<Client, String> {
    let client = client(&session.homeserver, &store_dir.join("store"), passphrase).await?;
    let user_id: OwnedUserId = session.user_id.as_str().try_into().map_err(|e| format!("{e}"))?;
    let device_id: OwnedDeviceId = session.device_id.as_str().into();
    client
        .matrix_auth()
        .restore_session(
            MatrixSession {
                meta: SessionMeta { user_id, device_id },
                tokens: SessionTokens { access_token: session.access_token.clone(), refresh_token: None },
            },
            RoomLoadSettings::default(),
        )
        .await
        .map_err(|e| match classify_sdk(&e) {
            err @ TransportError::Unauthorized(_) => err.to_string(),
            TransportError::Platform(_) => format!("matrix session restore failed: {e}"),
        })?;
    // Logins made before the assistant created its own cross-signing
    // identity can never see the operator as verified: make it explicit.
    if !client.encryption().cross_signing_status().await.is_some_and(|s| s.is_complete()) {
        return Err("the assistant's Matrix device has no cross-signing identity — run \
                    `aivyx-pa connect matrix` again"
            .into());
    }
    Ok(client)
}

type InboundTx = mpsc::UnboundedSender<Result<Inbound, TransportError>>;

pub struct SdkTransport {
    client: Client,
    inbound: Mutex<mpsc::UnboundedReceiver<Result<Inbound, TransportError>>>,
    sync_task: AbortHandle,
}

impl Drop for SdkTransport {
    fn drop(&mut self) {
        self.sync_task.abort();
    }
}

impl SdkTransport {
    /// Restores the session, does one initial sync (so rooms and their
    /// encryption state are known), then keeps syncing in the background
    /// from that sync's token. A transient sync failure is reported on the
    /// channel and retried with backoff; an unknown token ends the loop.
    pub async fn connect(session: &Session, store_dir: &Path, passphrase: &str) -> Result<Arc<Self>, String> {
        let client = restore(session, store_dir, passphrase).await?;
        // Unbounded on purpose: handlers run inline inside `sync_once` (and
        // nothing drains the channel until `connect` returns), so a bounded
        // channel could deadlock the initial sync. A sync response is bounded.
        let (tx, rx) = mpsc::unbounded_channel();
        install_handlers(&client, tx.clone());
        let first = client.sync_once(SyncSettings::default()).await.map_err(|e| {
            let err = classify_sdk(&e);
            match err {
                TransportError::Unauthorized(_) => err.to_string(),
                TransportError::Platform(_) => format!("matrix sync: {e}"),
            }
        })?;
        let mut token = first.next_batch;
        let sync_client = client.clone();
        let task = tokio::spawn(async move {
            let mut failures: u32 = 0;
            loop {
                match sync_client.sync_once(SyncSettings::default().token(token.clone())).await {
                    Ok(resp) => {
                        token = resp.next_batch;
                        failures = 0;
                        if tx.is_closed() {
                            return;
                        }
                    }
                    Err(e) => match classify_sdk(&e) {
                        err @ TransportError::Unauthorized(_) => {
                            let _ = tx.send(Err(err));
                            return;
                        }
                        TransportError::Platform(text) => {
                            if tx.send(Err(TransportError::Platform(format!("sync: {text}")))).is_err() {
                                return;
                            }
                            tokio::time::sleep(backoff(failures)).await;
                            failures = failures.saturating_add(1);
                        }
                    },
                }
            }
        });
        Ok(Arc::new(SdkTransport { client, inbound: Mutex::new(rx), sync_task: task.abort_handle() }))
    }

    fn room(&self, room_id: &str) -> Result<Room, TransportError> {
        let id = <&RoomId>::try_from(room_id).map_err(|e| TransportError::Platform(e.to_string()))?;
        self.client
            .get_room(id)
            .ok_or_else(|| TransportError::Platform(format!("the assistant isn't in room {room_id}")))
    }
}

fn install_handlers(client: &Client, tx: InboundTx) {
    let own = client.user_id().map(|u| u.to_owned());
    let tx_msg = tx.clone();
    client.add_event_handler(move |ev: OriginalSyncRoomMessageEvent, room: Room, info: Option<EncryptionInfo>| {
        let (tx, own) = (tx_msg.clone(), own.clone());
        async move {
            if own.as_deref() == Some(ev.sender.as_ref()) {
                return;
            }
            let MessageType::Text(text) = ev.content.msgtype else { return };
            let verified = matches!(info.map(|i| i.verification_state), Some(VerificationState::Verified));
            let _ = tx.send(Ok(Inbound::Message(IncomingMessage {
                room_id: room.room_id().to_string(),
                sender: ev.sender.to_string(),
                text: text.body,
                verified,
            })));
        }
    });
    let tx_enc = tx.clone();
    client.add_event_handler(move |ev: OriginalSyncRoomEncryptedEvent, room: Room| {
        let tx = tx_enc.clone();
        async move {
            let _ = tx.send(Ok(Inbound::Undecryptable { room_id: room.room_id().to_string(), sender: ev.sender.to_string() }));
        }
    });
    client.add_event_handler(move |ev: StrippedRoomMemberEvent, room: Room, client: Client| {
        let tx = tx.clone();
        async move {
            if Some(ev.state_key.as_ref()) != client.user_id().map(|u| u.as_str()) || room.state() != RoomState::Invited {
                return;
            }
            let _ = tx.send(Ok(Inbound::Invite { room_id: room.room_id().to_string(), inviter: ev.sender.to_string() }));
        }
    });
}

#[async_trait]
impl MatrixTransport for SdkTransport {
    async fn next_inbound(&self) -> Result<Inbound, TransportError> {
        match self.inbound.lock().await.recv().await {
            Some(item) => item,
            None => Err(TransportError::Platform("sync task ended".into())),
        }
    }

    async fn send_message(&self, msg: OutgoingMessage) -> Result<(), TransportError> {
        self.room(&msg.room_id)?
            .send(RoomMessageEventContent::text_plain(msg.text))
            .await
            .map(|_| ())
            .map_err(|e| TransportError::Platform(format!("send: {e}")))
    }

    async fn join_room(&self, room_id: &str) -> Result<(), TransportError> {
        self.room(room_id)?.join().await.map_err(|e| TransportError::Platform(format!("join: {e}")))
    }

    async fn decline_invite(&self, room_id: &str) -> Result<(), TransportError> {
        self.room(room_id)?.leave().await.map_err(|e| TransportError::Platform(format!("decline: {e}")))
    }

    async fn room_is_encrypted(&self, room_id: &str) -> Result<bool, TransportError> {
        Ok(self
            .room(room_id)?
            .latest_encryption_state()
            .await
            .map_err(|e| TransportError::Platform(format!("encryption state: {e}")))?
            .is_encrypted())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_from_one_second_and_caps_at_sixty() {
        let secs: Vec<u64> = (0..8).map(|n| backoff(n).as_secs()).collect();
        assert_eq!(secs, [1, 2, 4, 8, 16, 32, 60, 60]);
        assert_eq!(backoff(u32::MAX).as_secs(), 60);
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("aivyx-matrix-sdk-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn swap_archives_the_old_store_and_installs_the_new_one() {
        let d = scratch("swap-old");
        std::fs::create_dir_all(d.join("store")).unwrap();
        std::fs::write(d.join("store/marker"), "old").unwrap();
        std::fs::create_dir_all(d.join("store.new-7")).unwrap();
        std::fs::write(d.join("store.new-7/marker"), "new").unwrap();
        let archived = swap_in_new_store(&d, 7).unwrap();
        assert_eq!(archived, Some(d.join("store.old-7")));
        assert_eq!(std::fs::read_to_string(d.join("store/marker")).unwrap(), "new");
        assert_eq!(std::fs::read_to_string(d.join("store.old-7/marker")).unwrap(), "old");
        assert!(!d.join("store.new-7").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn swap_without_an_old_store_just_installs() {
        let d = scratch("swap-fresh");
        std::fs::create_dir_all(d.join("store.new-7")).unwrap();
        assert_eq!(swap_in_new_store(&d, 7).unwrap(), None);
        assert!(d.join("store").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_failed_swap_puts_the_old_store_back() {
        let d = scratch("swap-fail");
        std::fs::create_dir_all(d.join("store")).unwrap();
        std::fs::write(d.join("store/marker"), "old").unwrap();
        // no store.new-7: the second rename fails
        assert!(swap_in_new_store(&d, 7).is_err());
        assert_eq!(std::fs::read_to_string(d.join("store/marker")).unwrap(), "old");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn only_an_unknown_token_is_unauthorized() {
        let unknown = ErrorKind::UnknownToken(Default::default());
        assert!(matches!(classify(Some(&unknown), "x".into()), TransportError::Unauthorized(_)));
        assert!(matches!(classify(Some(&ErrorKind::Unrecognized), "x".into()), TransportError::Platform(_)));
        assert!(matches!(classify(None, "x".into()), TransportError::Platform(_)));
        assert!(classify(Some(&unknown), "x".into()).to_string().contains("connect matrix"));
    }
}
