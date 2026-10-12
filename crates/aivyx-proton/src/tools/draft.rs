//! `proton.draft` — save a draft to Drafts. Never sends.

use async_trait::async_trait;
use serde_json::{json, Value};

use aivyx_capability::Scope;
use aivyx_core::{Tool, ToolContext, ToolId, ToolOutcome, Verification};

use super::{build, compose_schema, fail, parse_compose, Mail};

pub struct ProtonDraft {
    id: ToolId,
    schema: Value,
    mail: Mail,
    from: String,
}

impl ProtonDraft {
    /// `from`: the address drafts are written from (`credentials.toml`'s
    /// `from_address`).
    pub fn new(mail: Mail, from: String) -> Self {
        ProtonDraft {
            id: ToolId::new(),
            schema: compose_schema(),
            mail,
            from,
        }
    }
}

#[async_trait]
impl Tool for ProtonDraft {
    fn id(&self) -> ToolId {
        self.id
    }

    fn name(&self) -> &str {
        "proton.draft"
    }

    fn description(&self) -> &str {
        "Save a plain-text draft in the operator's Proton Mail Drafts \
         folder — it is NOT sent; the operator sends it from Proton Mail. \
         A new message needs `to`, `subject` and `body`; a reply needs \
         `reply_to_id` (from proton.search) and `body`, and is threaded, \
         addressed to the original sender and titled Re: <subject> unless \
         you give `to` / `subject`. Optional `cc`."
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn required_scope(&self, _input: &Value) -> Scope {
        Scope::parse("email.write").expect("email.write is in KNOWN_BASES")
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        let compose = match parse_compose(&input) {
            Ok(c) => c,
            Err(e) => return fail(self.id, "proton.draft", e),
        };
        let mail = match build(self.mail.as_ref(), &self.from, compose).await {
            Ok(m) => m,
            Err(e) => return fail(self.id, "proton.draft", e),
        };
        match self.mail.append_draft(&mail).await {
            Ok(()) => ToolOutcome::Completed {
                output: json!({"saved": true, "to": mail.to, "subject": mail.subject}),
                verified: Verification::Unverified,
            },
            Err(e) => fail(self.id, "proton.draft", e),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::message::MailMessage;
    use crate::tools::test_support::ctx;
    use crate::transport::{InboxStatus, ScriptedTransport};

    #[tokio::test]
    async fn a_reply_draft_is_threaded_to_the_original_sender() {
        let original = MailMessage {
            uid: 7,
            subject: "Invoice".into(),
            from: Some("ann@x.com".into()),
            message_id: Some("inv@x.com".into()),
            ..Default::default()
        };
        let t = Arc::new(ScriptedTransport::new(InboxStatus::default(), vec![]).with_all_mail(vec![original]));
        let tool = ProtonDraft::new(t.clone(), "me@proton.me".into());
        assert_eq!(tool.required_scope(&json!({})).to_string(), "email.write");
        let out = tool.execute(json!({"reply_to_id": 7, "body": "Paid."}), &ctx()).await;
        assert!(matches!(out, ToolOutcome::Completed { .. }), "{out:?}");
        let d = &t.drafts().await[0];
        assert_eq!(d.from, "me@proton.me");
        assert_eq!(d.to, vec!["ann@x.com"]);
        assert_eq!(d.subject, "Re: Invoice");
        assert_eq!(d.in_reply_to.as_deref(), Some("inv@x.com"));
        assert!(t.sent().await.is_empty(), "a draft is never sent");
    }

    #[tokio::test]
    async fn replying_to_a_missing_message_fails() {
        let t = Arc::new(ScriptedTransport::new(InboxStatus::default(), vec![]));
        let tool = ProtonDraft::new(t, "me@proton.me".into());
        let out = tool.execute(json!({"reply_to_id": 7, "body": "x"}), &ctx()).await;
        assert!(matches!(out, ToolOutcome::Failed(_)));
    }
}
