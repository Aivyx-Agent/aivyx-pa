//! `signal.send` — a text message from the assistant's Signal number to a
//! contact the operator listed in `[signal] send_to`. Scope
//! `message.send`: Trusted-only, and the turn loop pauses for approval on
//! every call unless `[access] confirm_destructive = false` (a
//! withheld-integration base, like `email.send`). Only a recipient whose
//! key the operator verified (`TRUSTED_VERIFIED`) is sent to.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use aivyx_capability::Scope;
use aivyx_core::{AivyxError, Tool, ToolContext, ToolId, ToolOutcome, Verification};

use crate::session::split_reply;
use crate::transport::{SignalTransport, TrustLevel};

/// The most text one call may send (several Signal messages).
const MAX_TEXT: usize = 8 * 1024;

pub struct SignalSend {
    id: ToolId,
    schema: Value,
    transport: Arc<dyn SignalTransport>,
    send_to: Vec<String>,
}

impl SignalSend {
    pub fn new(transport: Arc<dyn SignalTransport>, send_to: Vec<String>) -> Self {
        SignalSend {
            id: ToolId::new(),
            schema: json!({
                "type": "object",
                "properties": {
                    "to": {"type": "string", "description": "A number from the operator's [signal] send_to list, e.g. +15550002222."},
                    "text": {"type": "string", "description": "Plain text."}
                },
                "required": ["to", "text"],
                "additionalProperties": false
            }),
            transport,
            send_to: send_to.iter().map(|n| n.trim().to_string()).collect(),
        }
    }

    fn fail(&self, detail: impl std::fmt::Display) -> ToolOutcome {
        ToolOutcome::Failed(AivyxError::Tool {
            tool: self.id,
            detail: format!("signal.send: {detail}"),
        })
    }
}

#[async_trait]
impl Tool for SignalSend {
    fn id(&self) -> ToolId {
        self.id
    }

    fn name(&self) -> &str {
        "signal.send"
    }

    fn description(&self) -> &str {
        "Send a Signal text message from the assistant's own Signal number \
         to one of the contacts the operator listed (`[signal] send_to`). \
         **It goes out immediately — there is no undo.** Input: `to` (a \
         listed number, e.g. +15550002222) and `text`. Long text is sent as \
         several messages. The contact's safety number must be verified \
         (`aivyx-pa signal verify`). Asks the operator to approve every send."
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn required_scope(&self, _input: &Value) -> Scope {
        Scope::parse("message.send").expect("message.send is in KNOWN_BASES")
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        let Some(to) = input.get("to").and_then(Value::as_str).map(str::trim) else {
            return self.fail("`to` is required");
        };
        let Some(text) = input.get("text").and_then(Value::as_str) else {
            return self.fail("`text` is required");
        };
        if !self.send_to.iter().any(|n| n == to) {
            return self.fail(format!(
                "{to} isn't in [signal] send_to — the operator must add it (and run \
                 `aivyx-pa connect signal`)"
            ));
        }
        if text.trim().is_empty() {
            return self.fail("`text` is empty");
        }
        if text.len() > MAX_TEXT {
            return self.fail("`text` is longer than 8 KiB");
        }
        // signal-cli trusts a new key on first sight; pa sends only to a
        // key the operator verified.
        match self.transport.identity(to).await {
            Ok(Some(id)) if id.trust == TrustLevel::TrustedVerified => {}
            Ok(Some(_)) => {
                return self.fail(format!(
                    "{to}'s safety number isn't verified — the operator must run \
                     `aivyx-pa signal verify` first"
                ));
            }
            Ok(None) => {
                return self.fail(format!(
                    "signal-cli hasn't seen {to}'s key yet — they need to message the \
                     assistant first, then the operator runs `aivyx-pa signal verify`"
                ));
            }
            Err(e) => return self.fail(e),
        }
        let parts = split_reply(text);
        let mut timestamp = 0;
        for part in &parts {
            match self.transport.send_text(to, part).await {
                Ok(ts) => timestamp = ts,
                Err(e) if e.is_untrusted_recipient() => {
                    return self.fail(format!(
                        "{to}'s safety number isn't verified — the operator must run \
                         `aivyx-pa signal verify` first"
                    ));
                }
                Err(e) => return self.fail(e),
            }
        }
        ToolOutcome::Completed {
            output: json!({"sent": true, "to": to, "parts": parts.len(), "timestamp": timestamp}),
            verified: Verification::Unverified,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{ScriptedTransport, TransportError, TrustLevel};
    use aivyx_core::{
        AgentId, CancellationToken, ChannelContext, ChannelError, ChannelPlatform, MessageOrigin,
        NullAuditHook, SessionId, StreamEvent, TurnId, TurnOutcome,
    };

    struct NoopChannel;

    #[async_trait]
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

    fn ctx() -> ToolContext<'static> {
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

    const SAM: &str = "+15550002222";

    #[tokio::test]
    async fn a_listed_contact_gets_the_message() {
        let t = Arc::new(verified_sam());
        let tool = SignalSend::new(t.clone(), vec![SAM.into()]);
        assert_eq!(tool.required_scope(&json!({})).to_string(), "message.send");
        assert!(!tool.output_is_untrusted());
        let out = tool.execute(json!({"to": SAM, "text": "Running late."}), &ctx()).await;
        let ToolOutcome::Completed { output, .. } = out else { panic!("{out:?}") };
        assert_eq!(output["parts"], 1);
        assert_eq!(t.sent().await, vec![(SAM.to_string(), "Running late.".to_string())]);
    }

    #[tokio::test]
    async fn an_unlisted_number_is_refused_before_anything_is_sent() {
        let t = Arc::new(verified_sam());
        let tool = SignalSend::new(t.clone(), vec![SAM.into()]);
        let out = tool.execute(json!({"to": "+15559999999", "text": "hi"}), &ctx()).await;
        assert!(matches!(out, ToolOutcome::Failed(_)));
        assert!(t.sent().await.is_empty());
        for bad in [json!({"to": SAM}), json!({"text": "x"}), json!({"to": SAM, "text": "  "})] {
            assert!(matches!(tool.execute(bad, &ctx()).await, ToolOutcome::Failed(_)));
        }
    }

    #[tokio::test]
    async fn an_unverified_or_unknown_key_is_refused_and_nothing_is_sent() {
        for (t, says) in [
            (ScriptedTransport::new(vec![]).with_identity(SAM, TrustLevel::TrustedUnverified, "1"), "signal verify"),
            (ScriptedTransport::new(vec![]).with_identity(SAM, TrustLevel::Untrusted, "1"), "signal verify"),
            (ScriptedTransport::new(vec![]), "message the assistant"),
        ] {
            let t = Arc::new(t);
            let tool = SignalSend::new(t.clone(), vec![SAM.into()]);
            let out = tool.execute(json!({"to": SAM, "text": "hi"}), &ctx()).await;
            let ToolOutcome::Failed(e) = out else { panic!("{out:?}") };
            assert!(e.to_string().contains(says), "{e}");
            assert!(t.sent().await.is_empty());
        }
    }

    #[tokio::test]
    async fn a_key_lookup_error_fails_the_send() {
        let t = Arc::new(
            ScriptedTransport::new(vec![])
                .failing_identity(SAM, TransportError::Unreachable("no socket".into())),
        );
        let tool = SignalSend::new(t.clone(), vec![SAM.into()]);
        let out = tool.execute(json!({"to": SAM, "text": "hi"}), &ctx()).await;
        let ToolOutcome::Failed(e) = out else { panic!("{out:?}") };
        assert!(e.to_string().contains("isn't reachable"), "{e}");
        assert!(t.sent().await.is_empty());
    }

    #[tokio::test]
    async fn an_untrusted_recipient_key_says_to_verify() {
        // signal-cli's own refusal (the key changed between check and send).
        let t = Arc::new(verified_sam().failing_send(TransportError::Refused(
            "Failed to send message due to untrusted identities".into(),
        )));
        let tool = SignalSend::new(t, vec![SAM.into()]);
        let out = tool.execute(json!({"to": SAM, "text": "hi"}), &ctx()).await;
        let ToolOutcome::Failed(e) = out else { panic!() };
        assert!(e.to_string().contains("signal verify"), "{e}");
    }

    fn verified_sam() -> ScriptedTransport {
        ScriptedTransport::new(vec![]).with_identity(SAM, TrustLevel::TrustedVerified, "22222")
    }
}
