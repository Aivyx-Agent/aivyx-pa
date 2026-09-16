//! Webhook agent tools — Phase 27 Task 3.
//!
//! Three tools following the `OnceLock`-factory pattern:
//!
//! - `webhook.create` — create a new webhook trigger
//! - `webhook.list` — list all webhooks
//! - `webhook.delete` — delete a webhook by ID

use std::sync::OnceLock;

use async_trait::async_trait;
use serde_json::{json, Value};

use aivyx_capability::Scope;
use aivyx_core::{AivyxError, Tool, ToolContext, ToolId, ToolOutcome, Verification};
use aivyx_storage::{DomainHandle, KeyDomain};

use crate::webhook::{self, WebhookRecord};
use crate::webhook_listener::DEFAULT_WEBHOOK_PORT;

// ---------------------------------------------------------------------------
// webhook.create
// ---------------------------------------------------------------------------

pub struct WebhookCreateTool {
    id: ToolId,
    schema: Value,
    store: OnceLock<DomainHandle>,
}

impl std::fmt::Debug for WebhookCreateTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebhookCreateTool")
            .field("id", &self.id)
            .finish()
    }
}

impl Default for WebhookCreateTool {
    fn default() -> Self {
        Self::new()
    }
}

impl WebhookCreateTool {
    pub fn new() -> Self {
        WebhookCreateTool {
            id: ToolId::new(),
            schema: json!({
                "type": "object",
                "properties": {
                    "role": {
                        "type": "string",
                        "description": "Role name to run the webhook turn under."
                    },
                    "prompt": {
                        "type": "string",
                        "description": "The prompt text submitted as a turn when the webhook fires."
                    }
                },
                "required": ["prompt"]
            }),
            store: OnceLock::new(),
        }
    }

    pub fn set_webhook_store(&self, handle: DomainHandle) -> Result<(), DomainHandle> {
        assert_eq!(handle.domain(), KeyDomain::Webhooks);
        self.store.set(handle)
    }
}

#[async_trait]
impl Tool for WebhookCreateTool {
    fn id(&self) -> ToolId {
        self.id
    }

    fn name(&self) -> &str {
        "webhook.create"
    }

    fn description(&self) -> &str {
        "Create a new webhook trigger. The webhook fires when an HTTP POST \
         is sent to http://127.0.0.1:<port>/trigger/<webhook_id>. Returns \
         the webhook_id and the trigger URL."
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn required_scope(&self, _input: &Value) -> Scope {
        Scope::parse("webhook.create").expect("known base")
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        let Some(store) = self.store.get() else {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: "webhook.create: no webhook store configured".to_string(),
            });
        };

        let prompt = input
            .get("prompt")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let role = input
            .get("role")
            .and_then(|v| v.as_str())
            .unwrap_or("default")
            .to_string();

        if prompt.is_empty() {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: "webhook.create requires a non-empty `prompt` field".to_string(),
            });
        }

        let webhook_id = format!("wh-{}", uuid::Uuid::new_v4().as_simple());
        let record = WebhookRecord::new(webhook_id.clone(), role, prompt);

        if let Err(e) = webhook::create_webhook(store, &record).await {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: format!("failed to persist webhook: {e}"),
            });
        }

        let trigger_url = format!(
            "http://127.0.0.1:{}/trigger/{}",
            DEFAULT_WEBHOOK_PORT, webhook_id
        );

        // Task 1 fix-round-1 (2026-09-16 review) — `webhook.create` is an
        // agent tool: its `output` feeds straight back into the LLM's
        // context (see aivyx-core/src/agent.rs, planner.tool_result_texts()),
        // lands in the session transcript, and is chained into the audit
        // log. Putting the live bearer secret in `output` therefore ships
        // the credential to whatever cloud LLM provider is configured for
        // this agent. Instead, print it once to the daemon's own stderr —
        // the same mechanism the CLI's `[[webhook]]` config-sync path
        // already uses (`aivyx-cli/src/bin/aivyx.rs`) — and keep it out of
        // the LLM-visible/audited `output` entirely. `webhook.list`
        // deliberately never includes it either (see that tool's output
        // below); losing it means deleting and recreating the webhook.
        eprintln!(
            "aivyx-pa daemon: webhook {:?} secret (save this, shown once): {}",
            webhook_id, record.secret
        );
        eprintln!(
            "aivyx-pa daemon: include it as: Authorization: Bearer {}",
            record.secret
        );

        ToolOutcome::Completed {
            output: json!({
                "webhook_id": webhook_id,
                "trigger_url": trigger_url,
                "secret": "printed to the daemon log at creation time — see stderr/journal",
                "note": format!(
                    "The bearer secret was printed to the daemon's stderr/journal \
                     at creation time, not returned here, so it never enters this \
                     conversation's context or the audit trail. Trigger this webhook \
                     with: curl -X POST -H 'Authorization: Bearer <secret>' {}",
                    trigger_url
                ),
            }),
            verified: Verification::NotApplicable,
        }
    }
}

// ---------------------------------------------------------------------------
// webhook.list
// ---------------------------------------------------------------------------

pub struct WebhookListTool {
    id: ToolId,
    schema: Value,
    store: OnceLock<DomainHandle>,
}

impl std::fmt::Debug for WebhookListTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebhookListTool")
            .field("id", &self.id)
            .finish()
    }
}

impl Default for WebhookListTool {
    fn default() -> Self {
        Self::new()
    }
}

impl WebhookListTool {
    pub fn new() -> Self {
        WebhookListTool {
            id: ToolId::new(),
            schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
            store: OnceLock::new(),
        }
    }

    pub fn set_webhook_store(&self, handle: DomainHandle) -> Result<(), DomainHandle> {
        assert_eq!(handle.domain(), KeyDomain::Webhooks);
        self.store.set(handle)
    }
}

#[async_trait]
impl Tool for WebhookListTool {
    fn id(&self) -> ToolId {
        self.id
    }

    fn name(&self) -> &str {
        "webhook.list"
    }

    fn description(&self) -> &str {
        "List all webhook triggers. Returns an array of webhook objects \
         with id, role, prompt, enabled status, and trigger URL."
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn required_scope(&self, _input: &Value) -> Scope {
        Scope::parse("webhook.list").expect("known base")
    }

    async fn execute(&self, _input: Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        let Some(store) = self.store.get() else {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: "webhook.list: no webhook store configured".to_string(),
            });
        };

        match webhook::list_webhooks(store).await {
            Ok(webhooks) => {
                let entries: Vec<Value> = webhooks
                    .iter()
                    .map(|w| {
                        let trigger_url = format!(
                            "http://127.0.0.1:{}/trigger/{}",
                            DEFAULT_WEBHOOK_PORT, w.webhook_id
                        );
                        json!({
                            "webhook_id": w.webhook_id,
                            "role": w.role_name,
                            "prompt": w.prompt,
                            "enabled": w.enabled,
                            "trigger_url": trigger_url,
                        })
                    })
                    .collect();
                ToolOutcome::Completed {
                    output: json!({ "webhooks": entries }),
                    verified: Verification::NotApplicable,
                }
            }
            Err(e) => ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: format!("failed to list webhooks: {e}"),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// webhook.delete
// ---------------------------------------------------------------------------

pub struct WebhookDeleteTool {
    id: ToolId,
    schema: Value,
    store: OnceLock<DomainHandle>,
}

impl std::fmt::Debug for WebhookDeleteTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebhookDeleteTool")
            .field("id", &self.id)
            .finish()
    }
}

impl Default for WebhookDeleteTool {
    fn default() -> Self {
        Self::new()
    }
}

impl WebhookDeleteTool {
    pub fn new() -> Self {
        WebhookDeleteTool {
            id: ToolId::new(),
            schema: json!({
                "type": "object",
                "properties": {
                    "webhook_id": {
                        "type": "string",
                        "description": "The ID of the webhook to delete."
                    }
                },
                "required": ["webhook_id"]
            }),
            store: OnceLock::new(),
        }
    }

    pub fn set_webhook_store(&self, handle: DomainHandle) -> Result<(), DomainHandle> {
        assert_eq!(handle.domain(), KeyDomain::Webhooks);
        self.store.set(handle)
    }
}

#[async_trait]
impl Tool for WebhookDeleteTool {
    fn id(&self) -> ToolId {
        self.id
    }

    fn name(&self) -> &str {
        "webhook.delete"
    }

    fn description(&self) -> &str {
        "Delete a webhook trigger by ID. The webhook will no longer fire. \
         Returns whether the webhook was found and deleted."
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn required_scope(&self, _input: &Value) -> Scope {
        Scope::parse("webhook.delete").expect("known base")
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        let Some(store) = self.store.get() else {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: "webhook.delete: no webhook store configured".to_string(),
            });
        };

        let webhook_id = input
            .get("webhook_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if webhook_id.is_empty() {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: "webhook.delete requires a non-empty `webhook_id` field".to_string(),
            });
        }

        let exists = match webhook::get_webhook(store, &webhook_id).await {
            Ok(Some(_)) => true,
            Ok(None) => false,
            Err(e) => {
                return ToolOutcome::Failed(AivyxError::Tool {
                    tool: self.id,
                    detail: format!("failed to check webhook: {e}"),
                });
            }
        };

        if !exists {
            return ToolOutcome::Completed {
                output: json!({
                    "deleted": false,
                    "reason": format!("webhook {webhook_id} not found")
                }),
                verified: Verification::NotApplicable,
            };
        }

        if let Err(e) = webhook::delete_webhook(store, &webhook_id).await {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: format!("failed to delete webhook: {e}"),
            });
        }

        ToolOutcome::Completed {
            output: json!({ "deleted": true, "webhook_id": webhook_id }),
            verified: Verification::NotApplicable,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_core::{
        AgentId, CancellationToken, ChannelContext, ChannelError, ChannelPlatform, SessionId,
        StreamEvent, TurnId, TurnOutcome,
    };
    use aivyx_crypto::MasterKey;
    use aivyx_storage::{RedbStorage, StorageConfig};

    // Mirrors `reminder_tool.rs`'s own test scaffolding (`NoopChannel`/
    // `NoopAudit`/`make_ctx`) — no such helper is exported across crates,
    // so it's re-rolled locally, `#[cfg(test)]`-only.
    struct NoopChannel {
        session: SessionId,
        token: CancellationToken,
    }
    #[async_trait]
    impl ChannelContext for NoopChannel {
        fn session_id(&self) -> SessionId {
            self.session
        }
        fn platform(&self) -> ChannelPlatform {
            ChannelPlatform::Local
        }
        fn channel_name(&self) -> &str {
            "test"
        }
        fn trust_tier(&self) -> aivyx_capability::TrustTier {
            aivyx_capability::TrustTier::Trusted
        }
        async fn stream_event(&self, _event: StreamEvent<'_>) -> Result<(), ChannelError> {
            Ok(())
        }
        async fn finalize(&self, _outcome: &TurnOutcome) -> Result<(), ChannelError> {
            Ok(())
        }
        fn cancellation_token(&self) -> CancellationToken {
            self.token.clone()
        }
    }
    struct NoopAudit;
    impl aivyx_core::AuditHook for NoopAudit {
        fn on_event(&self, _tag: aivyx_core::AuditTag) {}
    }
    fn ctx_parts() -> (NoopChannel, NoopAudit) {
        (
            NoopChannel {
                session: SessionId::new(),
                token: CancellationToken::new(),
            },
            NoopAudit,
        )
    }
    fn make_ctx<'a>(ch: &'a NoopChannel, audit: &'a dyn aivyx_core::AuditHook) -> ToolContext<'a> {
        ToolContext {
            agent_id: AgentId::new(),
            session_id: ch.session,
            turn_id: TurnId::new(),
            channel: ch,
            audit,
            cancellation: &ch.token,
            message_origin: aivyx_core::MessageOrigin::Operator,
        }
    }

    async fn test_store() -> DomainHandle {
        let dir =
            std::env::temp_dir().join(format!("aivyx-webhook-tool-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let storage = RedbStorage::open(
            StorageConfig::new(dir.join("store.redb")),
            MasterKey::from_raw([11u8; 32]),
        )
        .await
        .unwrap();
        storage.domain(KeyDomain::Webhooks)
    }

    #[test]
    fn webhook_create_scope() {
        let tool = WebhookCreateTool::new();
        assert_eq!(
            tool.required_scope(&json!({})).as_str(),
            "webhook.create"
        );
    }

    #[test]
    fn webhook_create_name_and_schema() {
        let tool = WebhookCreateTool::new();
        assert_eq!(tool.name(), "webhook.create");
        let schema = tool.input_schema();
        assert!(schema["required"].as_array().unwrap().contains(&json!("prompt")));
    }

    #[test]
    fn webhook_list_scope() {
        let tool = WebhookListTool::new();
        assert_eq!(
            tool.required_scope(&json!({})).as_str(),
            "webhook.list"
        );
    }

    #[test]
    fn webhook_list_name() {
        let tool = WebhookListTool::new();
        assert_eq!(tool.name(), "webhook.list");
    }

    #[test]
    fn webhook_delete_scope() {
        let tool = WebhookDeleteTool::new();
        assert_eq!(
            tool.required_scope(&json!({})).as_str(),
            "webhook.delete"
        );
    }

    #[test]
    fn webhook_delete_name_and_schema() {
        let tool = WebhookDeleteTool::new();
        assert_eq!(tool.name(), "webhook.delete");
        let schema = tool.input_schema();
        assert!(schema["required"].as_array().unwrap().contains(&json!("webhook_id")));
    }

    /// Task 1 fix-round-1 (2026-09-16 review) — `webhook.create`'s returned
    /// `output` feeds straight back into the LLM's context, so it must
    /// never carry the live bearer secret. This test creates a real
    /// webhook through the tool, pulls the actual persisted secret out of
    /// storage independently, and asserts that exact value is absent from
    /// the tool's JSON output (recursively, not just at the top level).
    #[tokio::test]
    async fn webhook_create_output_never_contains_the_raw_secret() {
        let handle = test_store().await;
        let tool = WebhookCreateTool::new();
        tool.set_webhook_store(handle.clone()).unwrap();
        let (ch, audit) = ctx_parts();
        let ctx = make_ctx(&ch, &audit);

        let out = tool
            .execute(json!({ "prompt": "do the thing" }), &ctx)
            .await;
        let output = match out {
            ToolOutcome::Completed { output, .. } => output,
            other => panic!("expected Completed, got {other:?}"),
        };

        let webhook_id = output["webhook_id"].as_str().unwrap().to_string();
        let record = webhook::get_webhook(&handle, &webhook_id)
            .await
            .unwrap()
            .expect("webhook was persisted");
        assert!(
            !record.secret.is_empty(),
            "sanity check: a real secret must have been generated"
        );

        // Recursively walk the JSON output and assert the raw secret
        // string never appears anywhere in it — not in `secret`, not
        // folded into `note`, not anywhere else.
        fn contains_str(value: &Value, needle: &str) -> bool {
            match value {
                Value::String(s) => s.contains(needle),
                Value::Array(items) => items.iter().any(|v| contains_str(v, needle)),
                Value::Object(map) => map.values().any(|v| contains_str(v, needle)),
                _ => false,
            }
        }
        assert!(
            !contains_str(&output, &record.secret),
            "tool output must not contain the raw webhook secret: {output}"
        );

        // The placeholder `secret` field should say where to actually find
        // it, not contain the credential itself.
        assert_eq!(
            output["secret"],
            json!("printed to the daemon log at creation time — see stderr/journal")
        );
    }
}
