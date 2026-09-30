//! The daemon's side of chat approvals: sends an `ApprovalRequest` to the
//! connection's frontend and waits for the matching `ResolveApproval`
//! (routed here by the connection loop while the turn runs), a timeout, a
//! cancel or a disconnect.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use aivyx_core::{APPROVAL_TIMEOUT, Approval, ApprovalRequest, CancellationToken};
use tokio::io::AsyncWriteExt;
use tokio::sync::oneshot;

use crate::daemon_ipc::{DaemonMessage, StreamEventPayload, encode_frame};

/// Open approval prompts for one connection, keyed by `request_id`.
#[derive(Default)]
pub struct ApprovalDesk {
    waiting: Mutex<HashMap<String, oneshot::Sender<bool>>>,
}

impl ApprovalDesk {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Ask and wait. A write failure, a closed desk, or `cancel` → `Denied`;
    /// no answer in [`APPROVAL_TIMEOUT`] → `TimedOut`.
    pub async fn ask<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        writer: &tokio::sync::Mutex<W>,
        session_id: &str,
        request: &ApprovalRequest,
        cancel: &CancellationToken,
    ) -> Approval {
        self.ask_with_timeout(writer, session_id, request, cancel, APPROVAL_TIMEOUT)
            .await
    }

    pub(crate) async fn ask_with_timeout<W: tokio::io::AsyncWrite + Unpin>(
        &self,
        writer: &tokio::sync::Mutex<W>,
        session_id: &str,
        request: &ApprovalRequest,
        cancel: &CancellationToken,
        timeout: std::time::Duration,
    ) -> Approval {
        let request_id = format!("approval-{}", uuid::Uuid::new_v4());
        let (tx, rx) = oneshot::channel();
        if let Ok(mut w) = self.waiting.lock() {
            w.insert(request_id.clone(), tx);
        }
        let msg = DaemonMessage::StreamEvent {
            session_id: session_id.to_string(),
            event: StreamEventPayload::ApprovalRequest {
                request_id: request_id.clone(),
                tool: request.tool.clone(),
                summary: request.summary.clone(),
                input: request.input.clone(),
                reason: request.reason.clone(),
                expires_in_secs: timeout.as_secs(),
            },
        };
        let sent = match encode_frame(&msg) {
            Ok(frame) => writer.lock().await.write_all(&frame).await.is_ok(),
            Err(_) => false,
        };
        let answer = if !sent {
            Approval::Denied
        } else {
            tokio::select! {
                r = rx => match r {
                    Ok(true) => Approval::Approved,
                    _ => Approval::Denied,
                },
                _ = tokio::time::sleep(timeout) => Approval::TimedOut,
                _ = cancel.cancelled() => Approval::Denied,
            }
        };
        if let Ok(mut w) = self.waiting.lock() {
            w.remove(&request_id);
        }
        answer
    }

    /// Route a `ResolveApproval`. Unknown ids (already answered or timed
    /// out) are ignored.
    pub fn resolve(&self, request_id: &str, approved: bool) {
        let tx = self.waiting.lock().ok().and_then(|mut w| w.remove(request_id));
        if let Some(tx) = tx {
            let _ = tx.send(approved);
        }
    }

    /// The frontend went away: every open prompt is denied.
    pub fn deny_all(&self) {
        if let Ok(mut w) = self.waiting.lock() {
            for (_, tx) in w.drain() {
                let _ = tx.send(false);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon_ipc::decode_frame;

    fn request() -> ApprovalRequest {
        ApprovalRequest {
            tool: "fs.delete".into(),
            summary: "fs.delete todo.md".into(),
            input: serde_json::json!({"path": "todo.md"}),
            reason: "can't be undone".into(),
        }
    }

    async fn sent_request_id(buf: &tokio::sync::Mutex<Vec<u8>>) -> String {
        loop {
            let bytes = buf.lock().await.clone();
            if let Ok((
                DaemonMessage::StreamEvent {
                    event: StreamEventPayload::ApprovalRequest { request_id, .. },
                    ..
                },
                _,
            )) = decode_frame::<DaemonMessage>(&bytes)
            {
                return request_id;
            }
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test]
    async fn an_answer_a_timeout_and_a_disconnect() {
        let desk = ApprovalDesk::new();
        let cancel = CancellationToken::new();

        let writer = Arc::new(tokio::sync::Mutex::new(Vec::<u8>::new()));
        let (d, w, c) = (Arc::clone(&desk), Arc::clone(&writer), cancel.clone());
        let asking = tokio::spawn(async move { d.ask(&w, "s", &request(), &c).await });
        let id = sent_request_id(&writer).await;
        desk.resolve(&id, true);
        assert_eq!(asking.await.unwrap(), Approval::Approved);

        let quick = desk
            .ask_with_timeout(
                &tokio::sync::Mutex::new(Vec::<u8>::new()),
                "s",
                &request(),
                &cancel,
                std::time::Duration::from_millis(10),
            )
            .await;
        assert_eq!(quick, Approval::TimedOut);

        let writer2 = Arc::new(tokio::sync::Mutex::new(Vec::<u8>::new()));
        let (d, w, c) = (Arc::clone(&desk), Arc::clone(&writer2), cancel.clone());
        let asking = tokio::spawn(async move { d.ask(&w, "s", &request(), &c).await });
        sent_request_id(&writer2).await;
        desk.deny_all();
        assert_eq!(asking.await.unwrap(), Approval::Denied);
    }
}
