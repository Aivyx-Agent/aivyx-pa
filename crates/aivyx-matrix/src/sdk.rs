//! The real transport over `matrix-sdk`: restore the sealed session,
//! open the encrypted SQLite store, sync in the background and turn
//! room events into `Inbound` items.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use matrix_sdk::{
    authentication::matrix::MatrixSession,
    config::SyncSettings,
    deserialized_responses::{EncryptionInfo, VerificationState},
    ruma::{
        api::error::ErrorKind,
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
use tokio::sync::{mpsc, Mutex};

use crate::credentials::Session;
use crate::transport::{Inbound, IncomingMessage, MatrixTransport, OutgoingMessage, TransportError};

async fn client(homeserver: &str, store_dir: &Path, passphrase: &str) -> Result<Client, String> {
    Client::builder()
        .homeserver_url(homeserver)
        .sqlite_store(store_dir.join("store"), Some(passphrase))
        .build()
        .await
        .map_err(|e| format!("matrix client: {e}"))
}

/// Logs in once with a password and returns the session to seal. The
/// password is not kept.
///
/// Always starts from a fresh crypto store: an existing `store_dir/store`
/// (from an earlier, since logged-out device) would fail with a mismatched
/// account, so it is renamed to `store.old-<unix timestamp>` first (never
/// deleted). Call [`archived_store_path`] beforehand to tell the operator
/// where it will go.
pub async fn login(
    homeserver: &str,
    store_dir: &Path,
    passphrase: &str,
    user: &str,
    password: &str,
) -> Result<Session, String> {
    if let Some(old) = archived_store_path(store_dir) {
        std::fs::rename(store_dir.join("store"), &old)
            .map_err(|e| format!("move old store aside to {}: {e}", old.display()))?;
    }
    let client = client(homeserver, store_dir, passphrase).await?;
    let r = client
        .matrix_auth()
        .login_username(user, password)
        .initial_device_display_name("Aivyx PA")
        .await
        .map_err(|e| format!("matrix login failed: {e}"))?;
    Ok(Session {
        homeserver: homeserver.to_string(),
        user_id: r.user_id.to_string(),
        device_id: r.device_id.to_string(),
        access_token: r.access_token,
    })
}

/// Where `login` will move an existing store to, or `None` if there is none.
pub fn archived_store_path(store_dir: &Path) -> Option<std::path::PathBuf> {
    if !store_dir.join("store").exists() {
        return None;
    }
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Some(store_dir.join(format!("store.old-{ts}")))
}

/// Backoff before the next sync attempt: 1 s doubling, capped at 60 s.
pub(crate) fn backoff(failures: u32) -> Duration {
    Duration::from_secs((1u64 << failures.min(6)).min(60))
}

fn is_unknown_token(e: &matrix_sdk::Error) -> bool {
    matches!(e.client_api_error_kind(), Some(ErrorKind::UnknownToken { .. }))
}

pub(crate) async fn restore(session: &Session, store_dir: &Path, passphrase: &str) -> Result<Client, String> {
    let client = client(&session.homeserver, store_dir, passphrase).await?;
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
        .map_err(|e| format!("matrix session restore failed: {e}"))?;
    Ok(client)
}

pub struct SdkTransport {
    client: Client,
    inbound: Mutex<mpsc::Receiver<Result<Inbound, TransportError>>>,
}

impl SdkTransport {
    /// Restores the session, does one initial sync (so rooms and their
    /// encryption state are known), then keeps syncing in the background
    /// from that sync's token. A transient sync failure is reported on the
    /// channel and retried with backoff; an unknown token ends the loop.
    pub async fn connect(session: &Session, store_dir: &Path, passphrase: &str) -> Result<Arc<Self>, String> {
        let client = restore(session, store_dir, passphrase).await?;
        let (tx, rx) = mpsc::channel(64);
        install_handlers(&client, tx.clone());
        let first = client
            .sync_once(SyncSettings::default())
            .await
            .map_err(|e| format!("matrix sync: {e}"))?;
        let mut token = first.next_batch;
        let sync_client = client.clone();
        tokio::spawn(async move {
            let mut failures: u32 = 0;
            loop {
                match sync_client.sync_once(SyncSettings::default().token(token.clone())).await {
                    Ok(resp) => {
                        token = resp.next_batch;
                        failures = 0;
                    }
                    Err(e) if is_unknown_token(&e) => {
                        let _ = tx.send(Err(TransportError::Unauthorized(e.to_string()))).await;
                        return;
                    }
                    Err(e) => {
                        if tx.send(Err(TransportError::Platform(format!("sync: {e}")))).await.is_err() {
                            return;
                        }
                        tokio::time::sleep(backoff(failures)).await;
                        failures = failures.saturating_add(1);
                    }
                }
            }
        });
        Ok(Arc::new(SdkTransport { client, inbound: Mutex::new(rx) }))
    }

    fn room(&self, room_id: &str) -> Result<Room, TransportError> {
        let id = <&RoomId>::try_from(room_id).map_err(|e| TransportError::Platform(e.to_string()))?;
        self.client
            .get_room(id)
            .ok_or_else(|| TransportError::Platform(format!("the assistant isn't in room {room_id}")))
    }
}

fn install_handlers(client: &Client, tx: mpsc::Sender<Result<Inbound, TransportError>>) {
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
            let _ = tx
                .send(Ok(Inbound::Message(IncomingMessage {
                    room_id: room.room_id().to_string(),
                    sender: ev.sender.to_string(),
                    text: text.body,
                    verified,
                })))
                .await;
        }
    });
    let tx_enc = tx.clone();
    client.add_event_handler(move |ev: OriginalSyncRoomEncryptedEvent, room: Room| {
        let tx = tx_enc.clone();
        async move {
            let _ = tx
                .send(Ok(Inbound::Undecryptable { room_id: room.room_id().to_string(), sender: ev.sender.to_string() }))
                .await;
        }
    });
    client.add_event_handler(move |ev: StrippedRoomMemberEvent, room: Room, client: Client| {
        let tx = tx.clone();
        async move {
            if Some(ev.state_key.as_ref()) != client.user_id().map(|u| u.as_str()) || room.state() != RoomState::Invited {
                return;
            }
            let _ = tx
                .send(Ok(Inbound::Invite { room_id: room.room_id().to_string(), inviter: ev.sender.to_string() }))
                .await;
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

    #[test]
    fn archived_store_path_is_none_without_a_store_and_stamped_with_one() {
        let d = std::env::temp_dir().join(format!("aivyx-matrix-sdk-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        assert!(archived_store_path(&d).is_none());
        std::fs::create_dir_all(d.join("store")).unwrap();
        let p = archived_store_path(&d).unwrap();
        assert!(p.file_name().unwrap().to_string_lossy().starts_with("store.old-"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
