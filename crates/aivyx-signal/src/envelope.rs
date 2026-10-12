//! signal-cli `receive` notifications → [`Inbound`]. Only a 1:1 message
//! with text is interesting; everything else (groups, sync messages from
//! the assistant's own devices, receipts, typing, reactions, edits,
//! deletes, stories, calls) is [`Inbound::Other`].

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TextMessage {
    /// E.164, when Signal shares it.
    pub sender_number: Option<String>,
    /// The sender's ACI (UUID), lowercased.
    pub sender_uuid: Option<String>,
    /// The message's own timestamp — what a read receipt names.
    pub timestamp: u64,
    pub text: String,
    /// How many attachments came with it (never downloaded).
    pub attachments: usize,
}

impl TextMessage {
    /// How to address the sender back: the number if known, else the ACI.
    pub fn sender(&self) -> &str {
        self.sender_number
            .as_deref()
            .or(self.sender_uuid.as_deref())
            .unwrap_or("")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inbound {
    Text(TextMessage),
    Other,
}

/// `params` of a `receive` notification — either `{"envelope": …}` or,
/// for an explicit `subscribeReceive`, `{"subscription": n, "result":
/// {"envelope": …}}`.
pub fn parse(params: &Value) -> Inbound {
    let body = params.get("result").unwrap_or(params);
    let Some(env) = body.get("envelope") else {
        return Inbound::Other;
    };
    let Some(data) = env.get("dataMessage").filter(|d| d.is_object()) else {
        return Inbound::Other;
    };
    // Groups and anything that isn't a plain message are not turns.
    for key in ["groupInfo", "reaction", "remoteDelete", "sticker", "payment"] {
        if data.get(key).is_some_and(|v| !v.is_null()) {
            return Inbound::Other;
        }
    }
    let text = data
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    if text.trim().is_empty() {
        return Inbound::Other;
    }
    let str_field = |k: &str| {
        env.get(k)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    let sender_number = str_field("sourceNumber");
    let sender_uuid = str_field("sourceUuid").map(|u| u.to_ascii_lowercase());
    if sender_number.is_none() && sender_uuid.is_none() {
        return Inbound::Other;
    }
    let timestamp = data
        .get("timestamp")
        .and_then(Value::as_u64)
        .or_else(|| env.get("timestamp").and_then(Value::as_u64))
        .unwrap_or(0);
    Inbound::Text(TextMessage {
        sender_number,
        sender_uuid,
        timestamp,
        text,
        attachments: data
            .get("attachments")
            .and_then(Value::as_array)
            .map_or(0, Vec::len),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture(name: &str) -> Value {
        let path = format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn a_direct_text_message_is_parsed() {
        let Inbound::Text(m) = parse(&fixture("receive-direct.json")) else {
            panic!("expected text");
        };
        assert_eq!(m.sender_number.as_deref(), Some("+15550001111"));
        assert_eq!(m.sender_uuid.as_deref(), Some("8f3c7a1e-0b5d-4c2a-9e61-3d7f2b9a4c10"));
        assert_eq!(m.timestamp, 1_791_763_166_000);
        assert_eq!(m.text, "What's on tomorrow?");
        assert_eq!(m.attachments, 0);
        assert_eq!(m.sender(), "+15550001111");
    }

    #[test]
    fn the_subscription_wrapper_is_unwrapped() {
        let wrapped = json!({"subscription": 0, "result": fixture("receive-direct.json")});
        assert!(matches!(parse(&wrapped), Inbound::Text(_)));
    }

    #[test]
    fn groups_sync_receipts_and_reactions_are_not_turns() {
        for name in ["receive-group.json", "receive-sync.json", "receive-receipt.json", "receive-reaction.json"] {
            assert_eq!(parse(&fixture(name)), Inbound::Other, "{name}");
        }
    }

    #[test]
    fn attachments_are_counted_and_empty_text_ignored() {
        let mut v = fixture("receive-direct.json");
        v["envelope"]["dataMessage"]["attachments"] = json!([{"id": "a"}, {"id": "b"}]);
        let Inbound::Text(m) = parse(&v) else { panic!() };
        assert_eq!(m.attachments, 2);
        v["envelope"]["dataMessage"]["message"] = json!("   ");
        assert_eq!(parse(&v), Inbound::Other);
        v["envelope"]["dataMessage"]["message"] = Value::Null;
        assert_eq!(parse(&v), Inbound::Other);
    }

    #[test]
    fn a_hidden_number_falls_back_to_the_aci() {
        let mut v = fixture("receive-direct.json");
        v["envelope"]["sourceNumber"] = Value::Null;
        let Inbound::Text(m) = parse(&v) else { panic!() };
        assert_eq!(m.sender(), "8f3c7a1e-0b5d-4c2a-9e61-3d7f2b9a4c10");
        v["envelope"]["sourceUuid"] = Value::Null;
        assert_eq!(parse(&v), Inbound::Other);
    }
}
