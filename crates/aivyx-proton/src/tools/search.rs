//! `proton.search` — search All Mail.

use async_trait::async_trait;
use serde_json::{json, Value};

use aivyx_capability::Scope;
use aivyx_core::{Tool, ToolContext, ToolId, ToolOutcome, Verification};

use super::{fail, opt_str, summary, Mail};
use crate::transport::SearchQuery;

const DEFAULT_RESULTS: usize = 10;
const MAX_RESULTS: usize = 25;

pub struct ProtonSearch {
    id: ToolId,
    schema: Value,
    mail: Mail,
}

impl ProtonSearch {
    pub fn new(mail: Mail) -> Self {
        ProtonSearch {
            id: ToolId::new(),
            schema: json!({
                "type": "object",
                "properties": {
                    "from": {"type": "string"},
                    "to": {"type": "string"},
                    "subject": {"type": "string"},
                    "text": {"type": "string", "description": "Words anywhere in the message."},
                    "since": {"type": "string", "description": "YYYY-MM-DD, inclusive."},
                    "before": {"type": "string", "description": "YYYY-MM-DD, exclusive."},
                    "max_results": {"type": "integer", "minimum": 1, "maximum": MAX_RESULTS}
                },
                "additionalProperties": false
            }),
            mail,
        }
    }
}

fn parse(input: &Value) -> Result<SearchQuery, String> {
    let max_results = match input.get("max_results") {
        None | Some(Value::Null) => DEFAULT_RESULTS,
        Some(v) => v
            .as_u64()
            .filter(|n| *n >= 1)
            .ok_or("`max_results` must be a positive integer")?
            .min(MAX_RESULTS as u64) as usize,
    };
    let q = SearchQuery {
        from: opt_str(input, "from")?,
        to: opt_str(input, "to")?,
        subject: opt_str(input, "subject")?,
        text: opt_str(input, "text")?,
        since: opt_str(input, "since")?,
        before: opt_str(input, "before")?,
        max_results,
    };
    q.to_imap()?;
    Ok(q)
}

#[async_trait]
impl Tool for ProtonSearch {
    fn id(&self) -> ToolId {
        self.id
    }

    fn name(&self) -> &str {
        "proton.search"
    }

    fn output_is_untrusted(&self) -> bool {
        true
    }

    fn description(&self) -> &str {
        "Search the operator's Proton Mail (All Mail) by `from`, `to`, \
         `subject`, `text` (anywhere in the message) and a `since` / \
         `before` date range (YYYY-MM-DD); every given field must match. \
         Returns up to `max_results` (default 10, at most 25) messages, \
         newest first, as `{id, from, subject, date, snippet}`. Pass an \
         `id` to `proton.read` for the whole message."
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn required_scope(&self, _input: &Value) -> Scope {
        Scope::parse("email.read").expect("email.read is in KNOWN_BASES")
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        let q = match parse(&input) {
            Ok(q) => q,
            Err(e) => return fail(self.id, "proton.search", e),
        };
        match self.mail.search(&q).await {
            Ok(found) => ToolOutcome::Completed {
                output: json!({"messages": found.iter().map(summary).collect::<Vec<_>>()}),
                verified: Verification::NotApplicable,
            },
            Err(e) => fail(self.id, "proton.search", e),
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

    fn mail(uid: u32) -> MailMessage {
        MailMessage { uid, subject: format!("s{uid}"), from: Some("a@x.com".into()), text: "hello".into(), ..Default::default() }
    }

    #[tokio::test]
    async fn results_are_newest_first_and_capped() {
        let t = Arc::new(ScriptedTransport::new(InboxStatus::default(), vec![]).with_all_mail((1..=30).map(mail).collect()));
        let tool = ProtonSearch::new(t.clone());
        assert!(tool.output_is_untrusted());
        assert_eq!(tool.required_scope(&json!({})).to_string(), "email.read");
        let out = tool.execute(json!({"subject": "s", "max_results": 99}), &ctx()).await;
        let ToolOutcome::Completed { output, .. } = out else { panic!("{out:?}") };
        let msgs = output["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), MAX_RESULTS);
        assert_eq!(msgs[0]["id"], 30);
        assert_eq!(t.searches().await[0].subject.as_deref(), Some("s"));
    }

    #[test]
    fn bad_input_is_refused_before_any_search() {
        assert!(parse(&json!({"since": "yesterday"})).is_err());
        assert!(parse(&json!({"text": "a\r\nb"})).is_err());
        assert!(parse(&json!({"max_results": 0})).is_err());
        assert_eq!(parse(&json!({})).unwrap().max_results, DEFAULT_RESULTS);
    }
}
