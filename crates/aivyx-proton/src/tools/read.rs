//! `proton.read` — one message, headers and text.

use async_trait::async_trait;
use serde_json::{json, Value};

use aivyx_capability::Scope;
use aivyx_core::{Tool, ToolContext, ToolId, ToolOutcome, Verification};

use super::{fail, Mail};
use crate::message::{cap, TOOL_TEXT_CAP};

pub struct ProtonRead {
    id: ToolId,
    schema: Value,
    mail: Mail,
}

impl ProtonRead {
    pub fn new(mail: Mail) -> Self {
        ProtonRead {
            id: ToolId::new(),
            schema: json!({
                "type": "object",
                "properties": {"id": {"type": "integer", "description": "An id from proton.search."}},
                "required": ["id"],
                "additionalProperties": false
            }),
            mail,
        }
    }
}

#[async_trait]
impl Tool for ProtonRead {
    fn id(&self) -> ToolId {
        self.id
    }

    fn name(&self) -> &str {
        "proton.read"
    }

    fn output_is_untrusted(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "Read one Proton Mail message by the `id` from proton.search: \
         from, to, cc, subject, date, message_id, the text (at most 32 KiB; \
         `truncated` says if it was cut) and attachment names (their \
         content is not available)."
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn required_scope(&self, _input: &Value) -> Scope {
        Scope::parse("email.read").expect("email.read is in KNOWN_BASES")
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        let Some(id) = input.get("id").and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok()) else {
            return fail(self.id, "proton.read", "`id` must be an id from proton.search");
        };
        match self.mail.fetch(id).await {
            Ok(Some(m)) => {
                let (text, truncated) = cap(&m.text, TOOL_TEXT_CAP);
                ToolOutcome::Completed {
                    output: json!({
                        "id": m.uid,
                        "from": m.from,
                        "to": m.to,
                        "cc": m.cc,
                        "subject": m.subject,
                        "date": m.date,
                        "message_id": m.message_id,
                        "text": text,
                        "truncated": truncated,
                        "attachments": m.attachments,
                    }),
                    verified: Verification::NotApplicable,
                }
            }
            Ok(None) => fail(self.id, "proton.read", format!("no message with id {id}")),
            Err(e) => fail(self.id, "proton.read", e),
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
    async fn a_message_is_returned_capped_with_attachment_names() {
        let m = MailMessage {
            uid: 4,
            subject: "big".into(),
            text: "z".repeat(TOOL_TEXT_CAP + 10),
            attachments: vec!["a.pdf".into()],
            ..Default::default()
        };
        let t = Arc::new(ScriptedTransport::new(InboxStatus::default(), vec![]).with_all_mail(vec![m]));
        let tool = ProtonRead::new(t);
        assert!(tool.output_is_untrusted());
        let out = tool.execute(json!({"id": 4}), &ctx()).await;
        let ToolOutcome::Completed { output, .. } = out else { panic!("{out:?}") };
        assert_eq!(output["text"].as_str().unwrap().len(), TOOL_TEXT_CAP);
        assert_eq!(output["truncated"], true);
        assert_eq!(output["attachments"][0], "a.pdf");
        let missing = tool.execute(json!({"id": 5}), &ctx()).await;
        assert!(matches!(missing, ToolOutcome::Failed(_)));
        let bad = tool.execute(json!({"id": "x"}), &ctx()).await;
        assert!(matches!(bad, ToolOutcome::Failed(_)));
    }
}
