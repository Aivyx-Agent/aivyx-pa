//! SAS (emoji) verification, run from the operator's terminal while the
//! channel is stopped: the operator starts verification from their Matrix
//! client, compares the emoji shown here, and confirms on both sides.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use matrix_sdk::{
    config::SyncSettings,
    encryption::verification::{SasState, Verification, VerificationRequest, VerificationRequestState},
    ruma::{
        events::{
            key::verification::request::ToDeviceKeyVerificationRequestEvent,
            room::message::{MessageType, OriginalSyncRoomMessageEvent},
        },
        OwnedUserId, UserId,
    },
    Client,
};
use tokio::sync::mpsc;

use crate::credentials::Session;

pub type Confirm = Arc<dyn Fn(String) -> bool + Send + Sync>;

/// How long to wait, after the emoji match, for the homeserver to report
/// the assistant's signature on the operator's identity.
const SIGNATURE_WAIT: Duration = Duration::from_secs(30);

/// Waits for a verification request from one of `allowed_senders`, runs
/// it to the end, and returns whether it succeeded.
///
/// Success means more than a matching emoji: the assistant signs the
/// operator's cross-signing identity with its own user-signing key (that is
/// what `matrix-sdk` does when a SAS flow verifies another user's master
/// key), and `run` waits until the homeserver hands that signature back,
/// so the operator's identity is verified from the assistant's side. Only
/// then do messages from the operator's cross-signed devices count as
/// `Verified`.
pub async fn run(
    session: &Session,
    store_dir: &Path,
    passphrase: &str,
    allowed_senders: Vec<String>,
    confirm: Confirm,
) -> Result<bool, String> {
    let client = crate::sdk::restore(session, store_dir, passphrase).await?;
    // The SDK ignores a verification request from a device it doesn't know.
    // The assistant may share no room with the operator yet (the invite is
    // accepted only once the channel runs), so fetch their keys up front.
    for sender in &allowed_senders {
        let user = <&UserId>::try_from(sender.as_str()).map_err(|e| format!("allowed_senders: {sender}: {e}"))?;
        client
            .encryption()
            .request_user_identity(user)
            .await
            .map_err(|e| format!("matrix keys query for {sender}: {e}"))?;
    }
    let (done_tx, mut done_rx) = mpsc::channel::<Option<OwnedUserId>>(1);
    install(&client, allowed_senders, confirm, done_tx);
    let sync_client = client.clone();
    let mut sync = tokio::spawn(async move { sync_client.sync(SyncSettings::default()).await });
    let result = tokio::select! {
        done = done_rx.recv() => match done.flatten() {
            // Keep syncing while waiting: the signature upload goes out
            // with the sync loop's outgoing requests.
            Some(operator) => wait_until_verified(&client, &operator).await.map(|()| true),
            None => Ok(false),
        },
        ended = &mut sync => match ended {
            Ok(Ok(())) => Ok(false),
            Ok(Err(e)) => Err(format!("matrix sync: {e}")),
            Err(e) => Err(format!("matrix sync task: {e}")),
        },
    };
    sync.abort();
    result
}

/// Polls the homeserver until the operator's identity is verified from the
/// assistant's side (its user-signing key's signature on their master key is
/// back), bounded by [`SIGNATURE_WAIT`].
async fn wait_until_verified(client: &Client, operator: &UserId) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + SIGNATURE_WAIT;
    loop {
        let identity = client
            .encryption()
            .request_user_identity(operator)
            .await
            .map_err(|e| format!("matrix keys query for {operator}: {e}"))?;
        match identity {
            None => {
                return Err(format!(
                    "the emoji matched, but {operator} has no cross-signing identity, so there is \
                     nothing for the assistant to trust. Set up cross-signing (\"secure backup\") \
                     in your Matrix app, then run `aivyx-pa matrix verify` again."
                ))
            }
            Some(i) if i.is_verified() => return Ok(()),
            Some(_) => {}
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "the emoji matched, but the homeserver didn't confirm the assistant's signature \
                 on {operator}'s identity within {}s — run `aivyx-pa matrix verify` again",
                SIGNATURE_WAIT.as_secs()
            ));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

fn install(client: &Client, allowed: Vec<String>, confirm: Confirm, done: mpsc::Sender<Option<OwnedUserId>>) {
    let (allowed_dev, confirm_dev, done_dev) = (allowed.clone(), confirm.clone(), done.clone());
    client.add_event_handler(move |ev: ToDeviceKeyVerificationRequestEvent, client: Client| {
        let (allowed, confirm, done) = (allowed_dev.clone(), confirm_dev.clone(), done_dev.clone());
        async move {
            if !allowed.iter().any(|a| a == ev.sender.as_str()) {
                return;
            }
            if let Some(req) = client.encryption().get_verification_request(&ev.sender, &ev.content.transaction_id).await {
                tokio::spawn(drive(req, confirm, done));
            }
        }
    });
    client.add_event_handler(move |ev: OriginalSyncRoomMessageEvent, client: Client| {
        let (allowed, confirm, done) = (allowed.clone(), confirm.clone(), done.clone());
        async move {
            let MessageType::VerificationRequest(_) = &ev.content.msgtype else { return };
            if !allowed.iter().any(|a| a == ev.sender.as_str()) {
                return;
            }
            if let Some(req) = client.encryption().get_verification_request(&ev.sender, &ev.event_id).await {
                tokio::spawn(drive(req, confirm, done));
            }
        }
    });
}

async fn drive(request: VerificationRequest, confirm: Confirm, done: mpsc::Sender<Option<OwnedUserId>>) {
    if request.accept().await.is_err() {
        let _ = done.send(None).await;
        return;
    }
    let mut changes = request.changes();
    while let Some(state) = changes.next().await {
        match state {
            VerificationRequestState::Transitioned { verification: Verification::SasV1(sas) } => {
                if sas.accept().await.is_err() {
                    let _ = done.send(None).await;
                    return;
                }
                let mut sas_changes = sas.changes();
                while let Some(s) = sas_changes.next().await {
                    match s {
                        SasState::KeysExchanged { emojis: None, .. } => {
                            // Emoji comparison is required; refuse to confirm blind.
                            let _ = sas.mismatch().await;
                            let _ = done.send(None).await;
                            return;
                        }
                        SasState::KeysExchanged { emojis: Some(emojis), .. } => {
                            let shown = emojis
                                .emojis
                                .iter()
                                .map(|e| format!("{} {}", e.symbol, e.description))
                                .collect::<Vec<_>>()
                                .join("   ");
                            let confirm = confirm.clone();
                            if tokio::task::spawn_blocking(move || confirm(shown)).await.unwrap_or(false) {
                                let _ = sas.confirm().await;
                            } else {
                                let _ = sas.mismatch().await;
                            }
                        }
                        SasState::Done { .. } => {
                            let _ = done.send(Some(sas.other_user_id().to_owned())).await;
                            return;
                        }
                        SasState::Cancelled(_) => {
                            let _ = done.send(None).await;
                            return;
                        }
                        _ => {}
                    }
                }
                return;
            }
            VerificationRequestState::Cancelled(_) => {
                let _ = done.send(None).await;
                return;
            }
            _ => {}
        }
    }
}
