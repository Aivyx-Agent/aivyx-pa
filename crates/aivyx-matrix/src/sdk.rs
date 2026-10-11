//! The real transport over `matrix-sdk`: restore the sealed session,
//! open the encrypted SQLite store, sync in the background and turn
//! room events into `Inbound` items.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use matrix_sdk::{
    authentication::matrix::MatrixSession,
    config::SyncSettings,
    deserialized_responses::{EncryptionInfo, VerificationState},
    ruma::{
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
pub async fn login(
    homeserver: &str,
    store_dir: &Path,
    passphrase: &str,
    user: &str,
    password: &str,
) -> Result<Session, String> {
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
    pub async fn connect(session: &Session, store_dir: &Path, passphrase: &str) -> Result<Arc<Self>, String> {
        let client = restore(session, store_dir, passphrase).await?;
        let (tx, rx) = mpsc::channel(64);
        install_handlers(&client, tx.clone());
        let sync_client = client.clone();
        tokio::spawn(async move {
            if let Err(e) = sync_client.sync(SyncSettings::default()).await {
                let text = e.to_string();
                let err = if text.contains("M_UNKNOWN_TOKEN") {
                    TransportError::Unauthorized(text)
                } else {
                    TransportError::Platform(format!("sync stopped: {text}"))
                };
                let _ = tx.send(Err(err)).await;
            }
        });
        Ok(Arc::new(SdkTransport { client, inbound: Mutex::new(rx) }))
    }

    /// One sync round so room state (membership, encryption) is known
    /// before the first `room_is_encrypted` call.
    pub async fn sync_once(&self) -> Result<(), String> {
        self.client
            .sync_once(SyncSettings::default())
            .await
            .map(|_| ())
            .map_err(|e| format!("matrix sync: {e}"))
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
