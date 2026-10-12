//! The `proton.*` tools, served by the `aivyx-proton` tool process. Each
//! holds the shared [`MailTransport`] (Bridge in production).
//!
//! - `proton.search` / `proton.read` — `email.read`, output untrusted
//!   (Picket + Bulwark see mail content before the model does).
//! - `proton.draft` — `email.write`: saved to Drafts, never sent.
//! - `proton.send` — `email.send`: pauses for approval on every call, like
//!   `gmail.send` (the turn loop does this for the `email.send` scope).
//!
//! All four need `Trusted`, like Gmail's, so they are not available inside
//! an email turn (`SemiTrusted`).

pub mod draft;
pub mod read;
pub mod search;
pub mod send;

pub use draft::ProtonDraft;
pub use read::ProtonRead;
pub use search::ProtonSearch;
pub use send::ProtonSend;

use std::sync::Arc;

use serde_json::{json, Value};

use aivyx_core::{AivyxError, ToolId, ToolOutcome};

use crate::message::{cap, MailMessage};
use crate::outgoing::{re_subject, OutgoingMail};
use crate::transport::MailTransport;

pub type Mail = Arc<dyn MailTransport>;

pub(crate) fn fail(tool: ToolId, name: &str, detail: impl std::fmt::Display) -> ToolOutcome {
    ToolOutcome::Failed(AivyxError::Tool {
        tool,
        detail: format!("{name}: {detail}"),
    })
}

/// An optional string field; present but not a string is an error.
pub(crate) fn opt_str(input: &Value, key: &str) -> Result<Option<String>, String> {
    match input.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(format!("`{key}` must be a string")),
    }
}

/// The search-result view of a message.
pub(crate) fn summary(m: &MailMessage) -> Value {
    let (snippet, _) = cap(m.text.trim(), 200);
    json!({
        "id": m.uid,
        "from": m.from,
        "subject": m.subject,
        "date": m.date,
        "snippet": snippet,
    })
}

/// What `proton.draft` and `proton.send` take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Compose {
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub subject: Option<String>,
    pub body: String,
    /// A `proton.search` / `proton.read` id to reply to.
    pub reply_to_id: Option<u32>,
}

fn addresses(input: &Value, key: &str) -> Result<Vec<String>, String> {
    let list = match input.get(key) {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(a)) => a,
        Some(_) => return Err(format!("`{key}` must be a list of addresses")),
    };
    list.iter()
        .map(|v| {
            let a = v.as_str().ok_or_else(|| format!("`{key}` must be a list of addresses"))?.trim();
            if !a.contains('@') || a.contains(['<', '>', ' ', '\r', '\n', ',']) {
                return Err(format!("`{key}`: {a:?} isn't an email address"));
            }
            Ok(a.to_string())
        })
        .collect()
}

pub(crate) fn parse_compose(input: &Value) -> Result<Compose, String> {
    let body = opt_str(input, "body")?.ok_or("`body` is required")?;
    let reply_to_id = match input.get("reply_to_id") {
        None | Some(Value::Null) => None,
        Some(v) => Some(
            v.as_u64()
                .and_then(|n| u32::try_from(n).ok())
                .ok_or("`reply_to_id` must be an id from proton.search")?,
        ),
    };
    let subject = opt_str(input, "subject")?;
    if subject.as_deref().is_some_and(|s| s.contains(['\r', '\n'])) {
        return Err("`subject` must be one line".into());
    }
    let c = Compose {
        to: addresses(input, "to")?,
        cc: addresses(input, "cc")?,
        subject,
        body,
        reply_to_id,
    };
    if c.reply_to_id.is_none() && c.to.is_empty() {
        return Err("give `to`, or `reply_to_id` to answer a message".into());
    }
    if c.reply_to_id.is_none() && c.subject.is_none() {
        return Err("`subject` is required for a new message".into());
    }
    Ok(c)
}

/// Builds the message; a reply is threaded and defaults to the original's
/// sender and `Re:` subject.
pub(crate) async fn build(mail: &dyn MailTransport, from: &str, c: Compose) -> Result<OutgoingMail, String> {
    let Some(id) = c.reply_to_id else {
        return Ok(OutgoingMail::new(from, c.to, c.cc, c.subject.as_deref().unwrap_or(""), c.body));
    };
    let original = mail
        .fetch(id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("no message with id {id}"))?;
    let to = if c.to.is_empty() {
        original.from.clone().into_iter().collect()
    } else {
        c.to
    };
    let mut out = OutgoingMail::reply(from, to, &original, c.body);
    out.cc = c.cc;
    if let Some(s) = c.subject {
        out.subject = re_subject(&s);
    }
    Ok(out)
}

pub(crate) fn compose_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "to": {"type": "array", "items": {"type": "string"}, "description": "Recipient addresses. Optional for a reply (defaults to the original sender)."},
            "cc": {"type": "array", "items": {"type": "string"}},
            "subject": {"type": "string", "description": "Required for a new message; a reply defaults to Re: <original subject>."},
            "body": {"type": "string", "description": "Plain text."},
            "reply_to_id": {"type": "integer", "description": "An id from proton.search or proton.read, to reply in that thread."}
        },
        "required": ["body"],
        "additionalProperties": false
    })
}

#[cfg(test)]
pub(crate) mod test_support {
    use aivyx_core::{
        AgentId, CancellationToken, ChannelContext, ChannelError, ChannelPlatform, MessageOrigin,
        NullAuditHook, SessionId, StreamEvent, ToolContext, TurnId, TurnOutcome,
    };

    struct NoopChannel;

    #[async_trait::async_trait]
    impl ChannelContext for NoopChannel {
        fn channel_name(&self) -> &str {
            "t"
        }
        fn platform(&self) -> ChannelPlatform {
            ChannelPlatform::Local
        }
        fn trust_tier(&self) -> aivyx_capability::TrustTier {
            aivyx_capability::TrustTier::Trusted
        }
        fn session_id(&self) -> SessionId {
            SessionId::new()
        }
        async fn stream_event(&self, _e: StreamEvent<'_>) -> Result<(), ChannelError> {
            Ok(())
        }
        async fn finalize(&self, _o: &TurnOutcome) -> Result<(), ChannelError> {
            Ok(())
        }
        fn cancellation_token(&self) -> CancellationToken {
            CancellationToken::new()
        }
    }

    pub fn ctx() -> ToolContext<'static> {
        ToolContext {
            agent_id: AgentId::new(),
            session_id: SessionId::new(),
            turn_id: TurnId::new(),
            channel: Box::leak(Box::new(NoopChannel)),
            audit: Box::leak(Box::new(NullAuditHook)),
            cancellation: Box::leak(Box::new(CancellationToken::new())),
            message_origin: MessageOrigin::Operator,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_needs_a_body_and_a_recipient_or_a_reply_id() {
        assert!(parse_compose(&json!({"to": ["a@x.com"], "subject": "s"})).is_err());
        assert!(parse_compose(&json!({"body": "b", "subject": "s"})).is_err());
        assert!(parse_compose(&json!({"to": ["a@x.com"], "body": "b"})).is_err());
        assert!(parse_compose(&json!({"body": "b", "reply_to_id": 4})).is_ok());
        assert!(parse_compose(&json!({"to": ["not-an-address"], "subject": "s", "body": "b"})).is_err());
        assert!(parse_compose(&json!({"to": ["a@x.com>\r\nBcc: e@x"], "subject": "s", "body": "b"})).is_err());
        assert!(parse_compose(&json!({"to": ["a@x.com"], "subject": "s\nBcc: e@x", "body": "b"})).is_err());
        let c = parse_compose(&json!({"to": ["a@x.com"], "cc": ["c@x.com"], "subject": "s", "body": "b"})).unwrap();
        assert_eq!(c.to, vec!["a@x.com"]);
        assert_eq!(c.cc, vec!["c@x.com"]);
    }

    #[test]
    fn summaries_are_short() {
        let m = MailMessage { uid: 3, text: "y".repeat(500), ..Default::default() };
        let s = summary(&m);
        assert_eq!(s["id"], 3);
        assert_eq!(s["snippet"].as_str().unwrap().len(), 200);
    }
}
