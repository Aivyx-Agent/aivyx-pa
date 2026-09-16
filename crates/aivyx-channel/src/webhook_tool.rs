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

        // Task 1 (2026-09-16 audit) — the bearer secret is shown exactly
        // once, here, at creation time. `webhook.list` deliberately never
        // includes it (see that tool's output below); losing it means
        // deleting and recreating the webhook.
        ToolOutcome::Completed {
            output: json!({
                "webhook_id": webhook_id,
                "trigger_url": trigger_url,
                "secret": record.secret,
                "note": format!(
                    "Save this secret now — it will not be shown again. \
                     Trigger this webhook with: curl -X POST -H 'Authorization: Bearer {}' {}",
                    record.secret, trigger_url
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
}
