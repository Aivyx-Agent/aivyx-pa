//! SAS (emoji) verification, run from the operator's terminal while the
//! channel is stopped: the operator starts verification from their Matrix
//! client, compares the emoji shown here, and confirms on both sides.

use std::path::Path;
use std::sync::Arc;

use futures_util::StreamExt;
use matrix_sdk::{
    config::SyncSettings,
    encryption::verification::{SasState, Verification, VerificationRequest, VerificationRequestState},
    ruma::events::{
        key::verification::request::ToDeviceKeyVerificationRequestEvent,
        room::message::{MessageType, OriginalSyncRoomMessageEvent},
    },
    Client,
};
use tokio::sync::mpsc;

use crate::credentials::Session;

pub type Confirm = Arc<dyn Fn(String) -> bool + Send + Sync>;

/// Waits for a verification request from one of `allowed_senders`, runs
/// it to the end, and returns whether it succeeded.
pub async fn run(
    session: &Session,
    store_dir: &Path,
    passphrase: &str,
    allowed_senders: Vec<String>,
    confirm: Confirm,
) -> Result<bool, String> {
    let client = crate::sdk::restore(session, store_dir, passphrase).await?;
    let (done_tx, mut done_rx) = mpsc::channel::<bool>(1);
    install(&client, allowed_senders, confirm, done_tx);
    let sync_client = client.clone();
    let sync = tokio::spawn(async move { sync_client.sync(SyncSettings::default()).await });
    let ok = done_rx.recv().await.unwrap_or(false);
    sync.abort();
    Ok(ok)
}

fn install(client: &Client, allowed: Vec<String>, confirm: Confirm, done: mpsc::Sender<bool>) {
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

async fn drive(request: VerificationRequest, confirm: Confirm, done: mpsc::Sender<bool>) {
    if request.accept().await.is_err() {
        let _ = done.send(false).await;
        return;
    }
    let mut changes = request.changes();
    while let Some(state) = changes.next().await {
        match state {
            VerificationRequestState::Transitioned { verification: Verification::SasV1(sas) } => {
                if sas.accept().await.is_err() {
                    let _ = done.send(false).await;
                    return;
                }
                let mut sas_changes = sas.changes();
                while let Some(s) = sas_changes.next().await {
                    match s {
                        SasState::KeysExchanged { emojis: Some(emojis), .. } => {
                            let shown = emojis
                                .emojis
                                .iter()
                                .map(|e| format!("{} {}", e.symbol, e.description))
                                .collect::<Vec<_>>()
                                .join("   ");
                            if confirm(shown) {
                                let _ = sas.confirm().await;
                            } else {
                                let _ = sas.mismatch().await;
                            }
                        }
                        SasState::Done { .. } => {
                            let _ = done.send(true).await;
                            return;
                        }
                        SasState::Cancelled(_) => {
                            let _ = done.send(false).await;
                            return;
                        }
                        _ => {}
                    }
                }
                return;
            }
            VerificationRequestState::Cancelled(_) => {
                let _ = done.send(false).await;
                return;
            }
            _ => {}
        }
    }
}
