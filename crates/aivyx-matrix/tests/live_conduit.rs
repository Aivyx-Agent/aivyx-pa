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
//!   for i in $(seq 60); do curl -sf http://127.0.0.1:6167/_matrix/client/versions >/dev/null && break; sleep 1; done
//!   cargo test -p aivyx-matrix --test live_conduit -- --ignored --nocapture
//!   docker stop aivyx-conduit
//!
//! Two tests: an unverified operator device reaches the transport as
//! `verified == false`; and after `aivyx_matrix::verify::run` with a
//! cross-signed operator, the operator's message arrives `verified == true`
//! and the policy makes it a turn.
//!
//! Conduit sends welcome/announcement messages from @conduit:localhost to
//! new accounts; the tests skip those on purpose.

// matrix-sdk's deep async nesting overflows the default limit when proving
// `Send` for a spawned sync task (same as the library crate).
#![recursion_limit = "256"]

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use aivyx_matrix::credentials::{self, MatrixKeys, Session};
use aivyx_matrix::policy::{decide, Action, MatrixPolicy};
use aivyx_matrix::sdk::SdkTransport;
use aivyx_matrix::transport::{Inbound, IncomingMessage, MatrixTransport, OutgoingMessage};
use futures_util::StreamExt;
use matrix_sdk::{
    config::SyncSettings,
    encryption::verification::{SasState, VerificationRequestState},
    ruma::{
        api::client::{account::register::v3::Request as Register, room::create_room::v3::Request as CreateRoom, uiaa},
        events::{
            room::{
                encrypted::OriginalSyncRoomEncryptedEvent,
                encryption::RoomEncryptionEventContent,
                message::{OriginalSyncRoomMessageEvent, RoomMessageEventContent},
            },
            InitialStateEvent,
        },
        OwnedDeviceId, OwnedUserId, RoomId,
    },
    Client, Room, RoomMemberships,
};

const HS: &str = "http://127.0.0.1:6167";
const PW: &str = "pw-live-test";

/// Removes the test's scratch directory even when an assertion fails.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("aivyx-matrix-live-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Aborts a background task (a client's sync loop) when the test ends.
struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

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

/// The operator's own cross-signing, as Element sets it up: their device
/// ends up signed by their self-signing key.
async fn bootstrap_operator_cross_signing(client: &Client) {
    if let Err(e) = client.encryption().bootstrap_cross_signing(None).await {
        let info = e.as_uiaa_response().unwrap_or_else(|| panic!("operator cross-signing: {e}"));
        let mut pw = uiaa::Password::new(
            uiaa::UserIdentifier::Matrix(uiaa::MatrixUserIdentifier::new(client.user_id().unwrap().to_string())),
            PW.to_string(),
        );
        pw.session = info.session.clone();
        client.encryption().bootstrap_cross_signing(Some(uiaa::AuthData::Password(pw))).await.unwrap();
    }
}

/// The assistant logs in through `aivyx_matrix::sdk::login` and seals the
/// session, as `aivyx-pa connect matrix` does.
async fn connect_bot(bot: &str, dir: &Path, keys: &MatrixKeys) -> Session {
    let (session, archived) = aivyx_matrix::sdk::login(HS, dir, &keys.store_passphrase(), bot, PW).await.unwrap();
    assert!(archived.is_none());
    credentials::save(keys, dir, &session).unwrap();
    let loaded = credentials::load(keys, dir).unwrap().expect("sealed session loads back");
    assert_eq!(loaded, session);
    session
}

/// The operator creates an encrypted room and invites the assistant.
async fn create_encrypted_room(operator: &Client, bot_user: &str) -> Room {
    let mut create = CreateRoom::new();
    let bot_id: OwnedUserId = bot_user.try_into().unwrap();
    create.invite = vec![bot_id];
    create.initial_state =
        vec![InitialStateEvent::with_empty_state_key(RoomEncryptionEventContent::with_recommended_defaults()).to_raw_any()];
    operator.create_room(create).await.unwrap()
}

/// Next inbound item before `deadline`, skipping Conduit's announcements.
async fn next_relevant(transport: &SdkTransport, deadline: Instant) -> Option<Inbound> {
    loop {
        let left = deadline.checked_duration_since(Instant::now())?;
        match tokio::time::timeout(left.min(Duration::from_secs(3)), transport.next_inbound()).await {
            Err(_) if Instant::now() >= deadline => return None,
            Err(_) => continue,
            Ok(item) => match item.unwrap() {
                Inbound::Message(m) if m.sender.starts_with("@conduit:") => {}
                other => return Some(other),
            },
        }
    }
}

/// Accepts the invite to the operator's room and waits until the background
/// sync reports the room as encrypted. Returns the room ID.
async fn join_invited_room(transport: &SdkTransport) -> String {
    let deadline = Instant::now() + Duration::from_secs(20);
    let room_id = match next_relevant(transport, deadline).await.expect("no invite within 20 s") {
        Inbound::Invite { room_id, .. } => room_id,
        other => panic!("expected an invite, got {other:?}"),
    };
    transport.join_room(&room_id).await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    while !transport.room_is_encrypted(&room_id).await.unwrap_or(false) {
        assert!(Instant::now() < deadline, "room never reported as encrypted");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    room_id
}

/// The operator sends `text` once the assistant has joined, re-syncing in
/// between when no background sync runs (`sync_inline`), and returns the
/// first message the assistant receives.
async fn send_and_receive(operator: &Client, room: &Room, transport: &SdkTransport, text: &str, sync_inline: bool) -> IncomingMessage {
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut sent = false;
    loop {
        assert!(Instant::now() < deadline, "no decryptable message arrived within 60 s");
        if sync_inline {
            operator.sync_once(SyncSettings::default().timeout(Duration::from_secs(1))).await.unwrap();
        } else if !sent {
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        if !sent {
            if room.joined_members_count() < 2 {
                continue;
            }
            room.send(RoomMessageEventContent::text_plain(text)).await.unwrap();
            sent = true;
            println!("operator sent {text:?}");
        }
        let short = (Instant::now() + Duration::from_secs(3)).min(deadline);
        match next_relevant(transport, short).await {
            Some(Inbound::Message(m)) => {
                println!("bot received: {m:?}");
                return m;
            }
            Some(other @ Inbound::Undecryptable { .. }) => panic!("bot could not decrypt: {other:?}"),
            Some(Inbound::Invite { .. }) | None => {}
        }
    }
}

/// The assistant replies in `room_id`; returns what the operator's client
/// made of it: `Some(text)` if it decrypted, `None` if it could not.
async fn operator_reads_reply(operator: &Client, transport: &SdkTransport, room_id: &str, text: &str, sync_inline: bool) -> Option<String> {
    let bot = transport_user(operator, room_id).await;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Option<String>>();
    let (tx_plain, bot_plain) = (tx.clone(), bot.clone());
    let plain = operator.add_event_handler(move |ev: OriginalSyncRoomMessageEvent| {
        let (tx, bot) = (tx_plain.clone(), bot_plain.clone());
        async move {
            if ev.sender == bot {
                let _ = tx.send(Some(ev.content.body().to_string()));
            }
        }
    });
    let enc = operator.add_event_handler(move |ev: OriginalSyncRoomEncryptedEvent| {
        let (tx, bot) = (tx.clone(), bot.clone());
        async move {
            if ev.sender == bot {
                let _ = tx.send(None);
            }
        }
    });
    transport
        .send_message(OutgoingMessage { room_id: room_id.to_string(), text: text.to_string() })
        .await
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let seen = loop {
        assert!(Instant::now() < deadline, "the operator never saw the assistant's reply");
        if sync_inline {
            operator.sync_once(SyncSettings::default().timeout(Duration::from_secs(1))).await.unwrap();
        }
        if let Ok(Some(item)) = tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
            break item;
        }
    };
    operator.remove_event_handler(plain);
    operator.remove_event_handler(enc);
    println!("operator read the reply as: {seen:?}");
    seen
}

/// The assistant's user ID: the room's other joined member.
async fn transport_user(operator: &Client, room_id: &str) -> OwnedUserId {
    let room = operator.get_room(<&RoomId>::try_from(room_id).unwrap()).expect("operator is in the room");
    let me = operator.user_id().unwrap();
    room.members(RoomMemberships::JOIN)
        .await
        .unwrap()
        .into_iter()
        .map(|m| m.user_id().to_owned())
        .find(|u| u != me)
        .expect("the assistant joined")
}

#[tokio::test]
#[ignore = "needs a local Conduit homeserver in Docker — see the module doc"]
async fn an_encrypted_message_reaches_the_transport() {
    let stamp = std::process::id();
    let (bot, me) = (format!("pa{stamp}"), format!("me{stamp}"));
    let scratch = Scratch::new("unverified");
    let dir = &scratch.0;

    // Create the bot's account (the throwaway client is dropped; the
    // assistant's own login below makes the device that counts).
    drop(register(&bot, &dir.join("bot-scratch")).await);
    let operator = register(&me, &dir.join("operator")).await;

    let keys = MatrixKeys::derive(&aivyx_crypto::MasterKey::from_raw([5u8; 32])).unwrap();
    let session = connect_bot(&bot, dir, &keys).await;
    let room = create_encrypted_room(&operator, &session.user_id).await;

    let transport = SdkTransport::connect(&session, dir, &keys.store_passphrase()).await.unwrap();
    join_invited_room(&transport).await;

    // The operator must see the bot's membership and device keys before
    // sending, or the bot isn't a recipient of the room key.
    let m = send_and_receive(&operator, &room, &transport, "hello from live", true).await;
    assert_eq!(m.text, "hello from live");
    assert!(!m.verified, "an unverified operator device must not count as verified");

    // Replies go only to cross-signed devices: this operator has no
    // cross-signing, so their device can't read what the assistant sends.
    let room_id = room.room_id().to_string();
    let seen = operator_reads_reply(&operator, &transport, &room_id, "reply to an unsigned device", true).await;
    assert_eq!(seen, None, "an unsigned device must not receive the room key");
}

#[tokio::test]
#[ignore = "needs a local Conduit homeserver in Docker — see the module doc"]
async fn after_verification_the_operator_drives_a_turn() {
    let stamp = std::process::id();
    let (bot, me) = (format!("pv{stamp}"), format!("mv{stamp}"));
    let scratch = Scratch::new("verified");
    let dir = &scratch.0;

    drop(register(&bot, &dir.join("bot-scratch")).await);
    let operator = register(&me, &dir.join("operator")).await;
    bootstrap_operator_cross_signing(&operator).await;
    let operator_id = operator.user_id().unwrap().to_owned();
    let sync_client = operator.clone();
    let _operator_sync = AbortOnDrop(tokio::spawn(async move { sync_client.sync(SyncSettings::default()).await }));

    let keys = MatrixKeys::derive(&aivyx_crypto::MasterKey::from_raw([6u8; 32])).unwrap();
    let session = connect_bot(&bot, dir, &keys).await;
    let bot_id: OwnedUserId = session.user_id.as_str().try_into().unwrap();
    let bot_device: OwnedDeviceId = session.device_id.as_str().into();

    // The guide's order: the operator invites the assistant first.
    let room = create_encrypted_room(&operator, &session.user_id).await;

    // The assistant's side: `aivyx-pa matrix verify`, confirming the emoji.
    let verify = {
        let (session, dir, pass) = (session.clone(), dir.clone(), keys.store_passphrase());
        let sender = operator_id.to_string();
        let confirm: aivyx_matrix::verify::Confirm = Arc::new(|emoji: String| {
            println!("bot sees: {emoji}");
            true
        });
        tokio::spawn(async move { aivyx_matrix::verify::run(&session, &dir, &pass, vec![sender], confirm).await })
    };

    // The operator's side: find the assistant's device and verify it by emoji.
    let deadline = Instant::now() + Duration::from_secs(30);
    let device = loop {
        assert!(Instant::now() < deadline, "operator never saw the assistant's device");
        operator.encryption().request_user_identity(&bot_id).await.unwrap();
        if let Some(d) = operator.encryption().get_device(&bot_id, &bot_device).await.unwrap() {
            break d;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    let request = device.request_verification().await.unwrap();
    let operator_side = async {
        let mut changes = request.changes();
        while !request.is_ready() {
            match changes.next().await {
                Some(VerificationRequestState::Cancelled(c)) => panic!("request cancelled: {c:?}"),
                Some(_) => {}
                None => panic!("request stream ended"),
            }
        }
        let sas = request.start_sas().await.unwrap().expect("SAS started");
        let mut sas_changes = sas.changes();
        while let Some(state) = sas_changes.next().await {
            match state {
                SasState::KeysExchanged { emojis, .. } => {
                    let shown: Vec<_> = emojis.expect("emoji SAS").emojis.iter().map(|e| e.symbol).collect();
                    println!("operator sees: {}", shown.join(" "));
                    sas.confirm().await.unwrap();
                }
                SasState::Done { .. } => return,
                SasState::Cancelled(c) => panic!("SAS cancelled: {c:?}"),
                _ => {}
            }
        }
        panic!("SAS stream ended before Done");
    };
    tokio::time::timeout(Duration::from_secs(60), operator_side).await.expect("operator side of SAS timed out");
    let verified = tokio::time::timeout(Duration::from_secs(60), verify)
        .await
        .expect("verify::run timed out")
        .unwrap()
        .expect("verify::run failed");
    assert!(verified, "verify::run must report success");

    // Now the channel. The invite was sent before `matrix verify` ran (as
    // the guide orders it), so verify's sync consumed it; the transport
    // must still report it.
    let transport = SdkTransport::connect(&session, dir, &keys.store_passphrase()).await.unwrap();
    let room_id = join_invited_room(&transport).await;

    let m = send_and_receive(&operator, &room, &transport, "hello verified", false).await;
    assert_eq!(m.text, "hello verified");
    assert!(m.verified, "a cross-signed device of a verified operator must count as verified");
    let policy = MatrixPolicy { room_filter: room_id, allowed_senders: vec![operator_id.to_string()] };
    let room_id = policy.room_filter.clone();
    assert!(matches!(decide(&policy, Inbound::Message(m)), Action::Turn(_)), "the policy must make it a turn");

    // The reply reaches the operator's cross-signed device and decrypts.
    let seen = operator_reads_reply(&operator, &transport, &room_id, "verified reply", false).await;
    assert_eq!(seen.as_deref(), Some("verified reply"));
}
