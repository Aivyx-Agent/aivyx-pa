//! Live test against a local Conduit homeserver. Run by hand (copy-paste):
//!
//!   cat > /tmp/aivyx-conduit.toml <<'EOF'
//!   [global]
//!   server_name = "localhost"
//!   database_backend = "rocksdb"
//!   database_path = "/var/lib/matrix-conduit"
//!   address = "0.0.0.0"
//!   port = 6167
//!   allow_registration = true
//!   yes_i_am_very_very_sure_i_want_an_open_registration_server_prone_to_abuse = true
//!   allow_federation = false
//!   max_request_size = 20000000
//!   EOF
//!   chmod 644 /tmp/aivyx-conduit.toml
//!   docker run -d --rm --name aivyx-conduit -p 127.0.0.1:6167:6167 \
//!     -v /tmp/aivyx-conduit.toml:/etc/conduit.toml:ro \
//!     -e CONDUIT_CONFIG=/etc/conduit.toml matrixconduit/matrix-conduit:latest
//!   until curl -sf http://127.0.0.1:6167/_matrix/client/versions >/dev/null; do sleep 1; done
//!   cargo test -p aivyx-matrix --test live_conduit -- --ignored --nocapture
//!   docker stop aivyx-conduit
//!
//! Conduit sends welcome/announcement messages from @conduit:localhost to
//! new accounts; the test skips those on purpose.

use std::path::Path;
use std::time::{Duration, Instant};

use aivyx_matrix::credentials::{self, MatrixKeys};
use aivyx_matrix::transport::{Inbound, MatrixTransport};
use matrix_sdk::{
    config::SyncSettings,
    ruma::{
        api::client::{account::register::v3::Request as Register, room::create_room::v3::Request as CreateRoom, uiaa},
        events::{
            room::{encryption::RoomEncryptionEventContent, message::RoomMessageEventContent},
            InitialStateEvent,
        },
        OwnedUserId,
    },
    Client,
};

const HS: &str = "http://127.0.0.1:6167";
const PW: &str = "pw-live-test";

/// Registers `user` (ignoring "already exists") and logs in with an
/// encrypted SQLite store under `store`.
async fn register(user: &str, store: &Path) -> Client {
    let client = Client::builder()
        .homeserver_url(HS)
        .sqlite_store(store, None)
        .build()
        .await
        .unwrap();
    let mut req = Register::new();
    req.username = Some(user.to_string());
    req.password = Some(PW.to_string());
    req.auth = Some(uiaa::AuthData::Dummy(Default::default()));
    let _ = client.matrix_auth().register(req).await;
    if !client.matrix_auth().logged_in() {
        client.matrix_auth().login_username(user, PW).await.unwrap();
    }
    client
}

#[tokio::test]
#[ignore = "needs a local Conduit homeserver in Docker — see the module doc"]
async fn an_encrypted_message_reaches_the_transport() {
    let stamp = std::process::id();
    let (bot, me) = (format!("pa{stamp}"), format!("me{stamp}"));
    let dir = std::env::temp_dir().join(format!("aivyx-matrix-live-{stamp}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    // Create the bot's account (the throwaway client is dropped; the
    // assistant's own login below makes the device that counts).
    drop(register(&bot, &dir.join("bot-scratch")).await);
    let operator = register(&me, &dir.join("operator")).await;

    let keys = MatrixKeys::derive(&aivyx_crypto::MasterKey::from_raw([5u8; 32])).unwrap();
    let (session, archived) = aivyx_matrix::sdk::login(HS, &dir, &keys.store_passphrase(), &bot, PW).await.unwrap();
    assert!(archived.is_none());
    credentials::save(&keys, &dir, &session).unwrap();
    let loaded = credentials::load(&keys, &dir).unwrap().expect("sealed session loads back");
    assert_eq!(loaded, session);

    let mut create = CreateRoom::new();
    let bot_id: OwnedUserId = session.user_id.as_str().try_into().unwrap();
    create.invite = vec![bot_id];
    create.initial_state =
        vec![InitialStateEvent::with_empty_state_key(RoomEncryptionEventContent::with_recommended_defaults()).to_raw_any()];
    let room = operator.create_room(create).await.unwrap();

    let transport = aivyx_matrix::sdk::SdkTransport::connect(&session, &dir, &keys.store_passphrase()).await.unwrap();
    // The invite arrives; join it.
    // (Conduit also posts its own announcements to new accounts; skip those.)
    let room_id = loop {
        let item = tokio::time::timeout(Duration::from_secs(20), transport.next_inbound()).await.unwrap().unwrap();
        match item {
            Inbound::Invite { room_id, .. } => break room_id,
            Inbound::Message(m) if m.sender.starts_with("@conduit:") => {}
            other => panic!("expected an invite, got {other:?}"),
        }
    };
    transport.join_room(&room_id).await.unwrap();

    // Wait for the background sync to learn the room is encrypted.
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if transport.room_is_encrypted(&room_id).await.unwrap_or(false) {
            break;
        }
        assert!(Instant::now() < deadline, "room never reported as encrypted");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    // The operator must see the bot's membership and device keys before
    // sending, or the bot isn't a recipient of the room key. Sync and send
    // in a bounded loop until the bot reports the message.
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut attempt = 0u32;
    let mut sent = false;
    'outer: loop {
        assert!(Instant::now() < deadline, "no decryptable message arrived within 60 s");
        operator.sync_once(SyncSettings::default().timeout(Duration::from_secs(1))).await.unwrap();
        if !sent {
            let joined = room.joined_members_count();
            if joined >= 2 {
                attempt += 1;
                room.send(RoomMessageEventContent::text_plain("hello from live")).await.unwrap();
                sent = true;
                println!("operator sent (attempt {attempt})");
            }
        }
        if !sent {
            continue;
        }
        while let Ok(item) = tokio::time::timeout(Duration::from_secs(3), transport.next_inbound()).await {
            match item.unwrap() {
                Inbound::Message(m) => {
                    println!("bot received: {m:?}");
                    assert_eq!(m.text, "hello from live");
                    assert!(!m.verified, "an unverified operator device must not count as verified");
                    break 'outer;
                }
                other @ Inbound::Undecryptable { .. } => panic!("bot could not decrypt: {other:?}"),
                Inbound::Invite { .. } => {}
            }
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}
