//! Transport seam between [`TelegramChannel`](super::TelegramChannel)
//! and the Bot API.
//!
//! The real implementation ([`ReqwestTransport`]) is a thin wrapper
//! around `frankenstein::client_reqwest::Bot`. The point of the
//! [`TelegramTransport`] trait is to let unit tests swap in a scripted
//! double without touching the network — see `tests::ScriptedTransport`.
//!
//! ## Why an internal trait rather than a generic over `AsyncTelegramApi`
//!
//! `frankenstein` already defines an `AsyncTelegramApi` trait that the
//! reqwest client implements. We could generic the channel over that
//! directly and skip this wrapper. We *don't*, for two reasons:
//!
//! 1. **Surface narrowing.** `AsyncTelegramApi` has ~90 methods. The
//!    channel needs exactly two (get_updates, send_message). Defining
//!    our own trait with just those two pins the surface, makes the
//!    test double trivially small, and means a `frankenstein` bump
//!    that adds a method can't accidentally break our test fakes.
//! 2. **Error-shape collapse.** `frankenstein`'s errors wrap both
//!    HTTP failures and Telegram API errors in one enum; we collapse
//!    that to a single [`TransportError::Platform`] string at the
//!    seam so the channel's error handling is uniform. Callers inside
//!    `TelegramChannel::listen` never need to match on a
//!    frankenstein-specific variant.

use async_trait::async_trait;
use thiserror::Error;

/// One outgoing Telegram update the channel has already reduced to
/// its post-`SendMessage`-params shape. Keeping this as a plain
/// `(chat_id, text)` pair rather than a `frankenstein::SendMessageParams`
/// makes the scripted transport's capture buffer directly inspectable
/// in tests without pulling `frankenstein` types into assertion code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingMessage {
    pub chat_id: i64,
    pub text: String,
}

/// Image payload extracted from a Telegram photo message. Carries
/// the raw bytes and MIME type so the session layer can construct a
/// `Message::image` or `Message::text_with_image` without touching
/// the Bot API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImagePayload {
    pub media_type: String,
    pub data: Vec<u8>,
}

/// One inbound update the channel wants to route into the turn loop.
/// Only fields the channel actually consumes are kept — `message_id`,
/// `chat_id`, `user_id`, and the user's text. Everything else
/// (entities, attachments, forwarded_from, edit_date, ...) is
/// deliberately dropped at the transport boundary so the channel's
/// turn-loop glue has one obvious shape to handle.
///
/// Phase 45 added the optional `image` field for photo messages.
/// When a Telegram message contains a photo, the transport layer
/// downloads the largest size via `getFile` + HTTP GET and populates
/// this field. The `text` field carries the caption (if any) when
/// an image is present.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingMessage {
    pub update_id: i64,
    pub chat_id: i64,
    pub user_id: i64,
    pub text: String,
    /// Phase 45 — optional image payload for photo messages.
    pub image: Option<ImagePayload>,
}

#[derive(Debug, Error)]
pub enum TransportError {
    /// A Telegram API call failed — network error, 4xx from Bot API,
    /// deserialization mismatch, or a platform-side rate-limit. The
    /// string is diagnostic only; callers treat it as opaque.
    #[error("telegram transport platform error: {0}")]
    Platform(String),
}

#[async_trait]
pub trait TelegramTransport: Send + Sync {
    /// Long-poll the Bot API for new updates. The `offset` is the
    /// "all updates with id >= offset" cursor — callers pass the
    /// last-seen `update_id + 1` to acknowledge everything before it.
    /// `timeout_secs` is forwarded to Bot API `getUpdates?timeout=…`;
    /// the server holds the request open up to that many seconds
    /// before returning an empty list.
    ///
    /// Task 1 has no inbound-routing caller yet — task 4's `listen()`
    /// loop is where this becomes load-bearing. The allow is on the
    /// trait method because the test `ScriptedTransport::get_updates`
    /// impl is also currently unused at runtime.
    async fn get_updates(
        &self,
        offset: i64,
        timeout_secs: u32,
    ) -> Result<Vec<IncomingMessage>, TransportError>;

    /// Send one text message to a chat. Text-only by design — Phase 8
    /// explicitly defers rich media (photos, files, inline keyboards)
    /// per PHASE_8.md's non-goals list.
    async fn send_message(&self, msg: OutgoingMessage) -> Result<(), TransportError>;
}

// ---------------------------------------------------------------------------
// Production impl — frankenstein + reqwest
// ---------------------------------------------------------------------------

/// Real transport: wraps a `frankenstein::client_reqwest::Bot` and
/// adapts its types to our narrow `IncomingMessage` / `OutgoingMessage`
/// shape.
///
/// Phase 8 Task 1 ships the *wiring* but **no Task 1 unit test
/// exercises this type**. It exists so the trait has a real consumer
/// and the `frankenstein` dep is a legitimate compile target — that
/// catches "we're importing a crate we don't actually use" at build
/// time rather than at phase exit. A later Task 7 smoke test against
/// a real bot token is what actually covers this code path.
pub struct ReqwestTransport {
    bot: frankenstein::client_reqwest::Bot,
    /// Stored separately for constructing file download URLs.
    /// The Bot API file endpoint is `https://api.telegram.org/file/bot{token}/{path}`.
    token: String,
}

impl ReqwestTransport {
    pub fn new(token: &str) -> Self {
        ReqwestTransport {
            bot: frankenstein::client_reqwest::Bot::new(token),
            token: token.to_string(),
        }
    }

    /// Download a photo by `file_id` via `getFile` + HTTP GET.
    /// Returns the raw bytes and a MIME type inferred from the file
    /// extension (defaulting to `image/jpeg` if unknown).
    async fn download_photo(&self, file_id: &str) -> Result<ImagePayload, TransportError> {
        use frankenstein::AsyncTelegramApi;
        use frankenstein::methods::GetFileParams;

        let params = GetFileParams::builder().file_id(file_id).build();
        let file_resp = self
            .bot
            .get_file(&params)
            .await
            .map_err(|e| TransportError::Platform(format!("get_file: {e}")))?;

        let file_path = file_resp
            .result
            .file_path
            .ok_or_else(|| TransportError::Platform("get_file: no file_path in response".into()))?;

        let url = format!(
            "https://api.telegram.org/file/bot{}/{}",
            self.token, file_path
        );
        let resp = reqwest::get(&url)
            .await
            .map_err(|e| TransportError::Platform(format!("photo download: {}", e.without_url())))?;

        if !resp.status().is_success() {
            return Err(TransportError::Platform(format!(
                "photo download: HTTP {}",
                resp.status()
            )));
        }

        let data = resp
            .bytes()
            .await
            .map_err(|e| {
                TransportError::Platform(format!("photo download body: {}", e.without_url()))
            })?
            .to_vec();

        // Infer MIME type from file extension.
        let media_type = file_path
            .rsplit('.')
            .next()
            .map(|ext| match ext.to_ascii_lowercase().as_str() {
                "png" => "image/png",
                "jpg" | "jpeg" => "image/jpeg",
                "gif" => "image/gif",
                "webp" => "image/webp",
                _ => "image/jpeg", // Telegram photos are almost always JPEG
            })
            .unwrap_or("image/jpeg")
            .to_string();

        Ok(ImagePayload { media_type, data })
    }
}

#[async_trait]
impl TelegramTransport for ReqwestTransport {
    async fn get_updates(
        &self,
        offset: i64,
        timeout_secs: u32,
    ) -> Result<Vec<IncomingMessage>, TransportError> {
        use frankenstein::AsyncTelegramApi;
        use frankenstein::methods::GetUpdatesParams;

        let params = GetUpdatesParams::builder()
            .offset(offset)
            .timeout(timeout_secs)
            .build();

        let response = self
            .bot
            .get_updates(&params)
            .await
            .map_err(|e| TransportError::Platform(format!("get_updates: {e}")))?;

        let mut messages = Vec::new();
        for u in response.result {
            let update_id = u.update_id as i64;
            let msg = match u.content {
                frankenstein::updates::UpdateContent::Message(m) => *m,
                _ => continue,
            };
            let from = match msg.from {
                Some(f) => f,
                None => continue,
            };

            // Phase 45 — photo extraction. If the message has a photo
            // array, pick the largest size (last element — Telegram
            // sorts smallest to largest), download via getFile + HTTP
            // GET, and attach to the IncomingMessage. The caption field
            // becomes the text; if no caption, text is empty.
            let (text, image) = if let Some(ref photos) = msg.photo {
                if let Some(largest) = photos.last() {
                    let image = self.download_photo(&largest.file_id).await;
                    let caption = msg.caption.clone().unwrap_or_default();
                    match image {
                        Ok(payload) => (caption, Some(payload)),
                        Err(e) => {
                            // Download failed — fall back to text-only
                            // with the caption so the user's message
                            // isn't silently lost.
                            eprintln!(
                                "aivyx-telegram: photo download failed ({e}); falling back to caption"
                            );
                            (caption, None)
                        }
                    }
                } else {
                    // Empty photo array — treat as text-only.
                    match msg.text {
                        Some(t) => (t, None),
                        None => continue,
                    }
                }
            } else {
                match msg.text {
                    Some(t) => (t, None),
                    None => continue,
                }
            };

            // Skip messages with no text and no image.
            if text.is_empty() && image.is_none() {
                continue;
            }

            messages.push(IncomingMessage {
                update_id,
                chat_id: msg.chat.id,
                user_id: from.id as i64,
                text,
                image,
            });
        }
        Ok(messages)
    }

    async fn send_message(&self, msg: OutgoingMessage) -> Result<(), TransportError> {
        use frankenstein::AsyncTelegramApi;
        use frankenstein::methods::SendMessageParams;

        let params = SendMessageParams::builder()
            .chat_id(msg.chat_id)
            .text(msg.text)
            .build();

        self.bot
            .send_message(&params)
            .await
            .map_err(|e| TransportError::Platform(format!("send_message: {e}")))?;
        Ok(())
    }
}

#[cfg(test)]
mod token_in_errors_tests {
    /// Audit P8: the file URL embeds the bot token, and reqwest prints the URL
    /// in its errors; the download path strips it before formatting.
    #[tokio::test]
    async fn a_failed_photo_download_error_carries_no_token() {
        let token = "123456:AAFAKE-SECRET-TOKEN-FOR-AUDIT";
        let url = format!("http://127.0.0.1:9/file/bot{token}/photos/file_1.jpg");
        let e = reqwest::get(&url).await.expect_err("closed port");
        assert!(format!("{e}").contains(token), "premise: reqwest prints the URL");
        let shown = format!("photo download: {}", e.without_url());
        assert!(!shown.contains(token), "token in error text: {shown}");
    }
}
