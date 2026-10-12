//! `proton.send` — send now, through Bridge. The `email.send` scope makes
//! the turn loop pause for the operator's approval on every call.

use async_trait::async_trait;
use serde_json::{json, Value};

use aivyx_capability::Scope;
use aivyx_core::{Tool, ToolContext, ToolId, ToolOutcome, Verification};

use super::{build, compose_schema, fail, parse_compose, Mail};

pub struct ProtonSend {
    id: ToolId,
    schema: Value,
    mail: Mail,
    from: String,
}

impl ProtonSend {
    /// `from`: the address mail is sent from (`credentials.toml`'s
    /// `from_address`).
    pub fn new(mail: Mail, from: String) -> Self {
        ProtonSend {
            id: ToolId::new(),
            schema: compose_schema(),
            mail,
            from,
        }
    }
}

#[async_trait]
impl Tool for ProtonSend {
    fn id(&self) -> ToolId {
        self.id
    }

    fn name(&self) -> &str {
        "proton.send"
    }

    fn description(&self) -> &str {
        "Send a plain-text email from the operator's Proton Mail account. \
         **It goes out immediately — there is no undo.** A new message \
         needs `to`, `subject` and `body`; a reply needs `reply_to_id` \
         (from proton.search) and `body`, and is threaded, addressed to the \
         original sender and titled Re: <subject> unless you give `to` / \
         `subject`. Optional `cc`. Asks the operator to approve every send."
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn required_scope(&self, _input: &Value) -> Scope {
        Scope::parse("email.send").expect("email.send is in KNOWN_BASES")
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        let compose = match parse_compose(&input) {
            Ok(c) => c,
            Err(e) => return fail(self.id, "proton.send", e),
        };
        let mail = match build(self.mail.as_ref(), &self.from, compose).await {
            Ok(m) => m,
            Err(e) => return fail(self.id, "proton.send", e),
        };
        match self.mail.send(&mail).await {
            Ok(()) => ToolOutcome::Completed {
                output: json!({"sent": true, "to": mail.recipients(), "subject": mail.subject, "message_id": mail.message_id}),
                verified: Verification::Unverified,
            },
            Err(e) => fail(self.id, "proton.send", e),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::tools::test_support::ctx;
    use crate::transport::{InboxStatus, ScriptedTransport};

    #[tokio::test]
    async fn a_new_message_is_sent_from_the_configured_address() {
        let t = Arc::new(ScriptedTransport::new(InboxStatus::default(), vec![]));
        let tool = ProtonSend::new(t.clone(), "me@proton.me".into());
        assert_eq!(tool.required_scope(&json!({})).to_string(), "email.send");
        let out = tool
            .execute(json!({"to": ["ann@x.com"], "cc": ["bob@y.com"], "subject": "Hi", "body": "Hello"}), &ctx())
            .await;
        let ToolOutcome::Completed { output, .. } = out else { panic!("{out:?}") };
        assert_eq!(output["to"], json!(["ann@x.com", "bob@y.com"]));
        let s = &t.sent().await[0];
        assert_eq!(s.from, "me@proton.me");
        assert_eq!(s.subject, "Hi");
    }

    #[tokio::test]
    async fn a_failed_send_is_a_failure() {
        let t = Arc::new(ScriptedTransport::new(InboxStatus::default(), vec![]).failing_send());
        let tool = ProtonSend::new(t, "me@proton.me".into());
        let out = tool.execute(json!({"to": ["ann@x.com"], "subject": "Hi", "body": "x"}), &ctx()).await;
        assert!(matches!(out, ToolOutcome::Failed(_)));
    }
}
