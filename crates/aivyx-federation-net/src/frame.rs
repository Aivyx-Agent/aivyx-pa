//! Application envelopes carried over a [`Link`](crate::Link) (A19 §2).

use serde::{Deserialize, Serialize};

use aivyx_federation::identity::SignedHeader;
use aivyx_ipc::federation::RelayResponse;

/// Largest application payload one frame may carry (1 MiB).
pub const MAX_FRAME: usize = 1_048_576;

/// A signed relay request: the Ed25519 [`SignedHeader`] over the exact body
/// bytes, and those bytes (a JSON `RelayRequest`) as standard base64 so the
/// signature covers precisely what was sent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestFrame {
    pub header: SignedHeader,
    /// Standard base64 of the signed body bytes.
    pub body: String,
}

/// The peer's answer to a [`RequestFrame`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum ResponseFrame {
    Ok { response: RelayResponse },
    Denied { reason: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_ipc::federation::RelayResponse;

    #[test]
    fn response_frames_are_tagged_by_status() {
        let ok = ResponseFrame::Ok {
            response: RelayResponse::Chat { reply: "hi".into() },
        };
        let json = serde_json::to_string(&ok).unwrap();
        assert!(json.contains("\"status\":\"ok\""), "{json}");
        let denied = ResponseFrame::Denied {
            reason: "no".into(),
        };
        let json = serde_json::to_string(&denied).unwrap();
        assert!(json.contains("\"status\":\"denied\""), "{json}");
        let back: ResponseFrame = serde_json::from_str(&json).unwrap();
        assert!(matches!(back, ResponseFrame::Denied { reason } if reason == "no"));
    }
}
