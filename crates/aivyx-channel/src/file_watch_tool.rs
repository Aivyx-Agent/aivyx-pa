//! File-watch agent tools — Phase 27 Task 4.
//!
//! Three tools following the `OnceLock`-factory pattern:
//!
//! - `file_watch.create` — create a new file-watch trigger
//! - `file_watch.list` — list all file watches
//! - `file_watch.delete` — delete a file watch by ID

use std::sync::OnceLock;

use async_trait::async_trait;
use serde_json::{json, Value};

use aivyx_capability::Scope;
use aivyx_core::{AivyxError, Tool, ToolContext, ToolId, ToolOutcome, Verification};
use aivyx_storage::{DomainHandle, KeyDomain};

use crate::file_watch::{self, FileWatchRecord};

// ---------------------------------------------------------------------------
// file_watch.create
// ---------------------------------------------------------------------------

pub struct FileWatchCreateTool {
    id: ToolId,
    schema: Value,
    store: OnceLock<DomainHandle>,
}

impl std::fmt::Debug for FileWatchCreateTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileWatchCreateTool")
            .field("id", &self.id)
            .finish()
    }
}

impl Default for FileWatchCreateTool {
    fn default() -> Self {
        Self::new()
    }
}

impl FileWatchCreateTool {
    pub fn new() -> Self {
        FileWatchCreateTool {
            id: ToolId::new(),
            schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Filesystem path to watch. Can be a file or directory."
                    },
                    "role": {
                        "type": "string",
                        "description": "Role name to run the triggered turn under."
                    },
                    "prompt": {
                        "type": "string",
                        "description": "The prompt text submitted as a turn when a file change is detected."
                    },
                    "debounce_ms": {
                        "type": "integer",
                        "description": "Debounce interval in milliseconds. Default: 2000."
                    }
                },
                "required": ["path", "prompt"]
            }),
            store: OnceLock::new(),
        }
    }

    pub fn set_file_watch_store(&self, handle: DomainHandle) -> Result<(), DomainHandle> {
        assert_eq!(handle.domain(), KeyDomain::FileWatches);
        self.store.set(handle)
    }
}

#[async_trait]
impl Tool for FileWatchCreateTool {
    fn id(&self) -> ToolId {
        self.id
    }

    fn name(&self) -> &str {
        "file_watch.create"
    }

    fn description(&self) -> &str {
        "Create a new file-watch trigger. The watch fires when files at the \
         specified path are modified. Includes debounce to prevent rapid \
         re-fires from editor save storms. Returns the watch_id."
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn required_scope(&self, _input: &Value) -> Scope {
        Scope::parse("file_watch.create").expect("known base")
    }

    async fn execute(&self, input: Value, ctx: &ToolContext<'_>) -> ToolOutcome {
        // A19 final review I1 — a file watch's runs execute later as System
        // turns with the role's full authority, outside the peer channel's
        // ceiling, so a federation peer's request never creates one.
        if ctx.message_origin == aivyx_core::MessageOrigin::Peer {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: "file_watch.create isn't available to a request from a federation peer: \
                         a file watch starts runs later, outside the peer's limits"
                    .to_string(),
            });
        }
        // Like schedule.create: a triggered or scheduled run (System origin)
        // never creates more automation, so an unattended run can't spread.
        if ctx.message_origin == aivyx_core::MessageOrigin::System {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: "file_watch.create: cannot be called from within a triggered or scheduled \
                         run — creating new file watches is an operator/interactive-only \
                         action, to prevent unattended runs from propagating more \
                         automation"
                    .to_string(),
            });
        }
        let Some(store) = self.store.get() else {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: "file_watch.create: no file-watch store configured".to_string(),
            });
        };

        let path = input
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
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

        if path.is_empty() {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: "file_watch.create requires a non-empty `path` field".to_string(),
            });
        }
        if prompt.is_empty() {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: "file_watch.create requires a non-empty `prompt` field".to_string(),
            });
        }

        let watch_id = format!("fw-{}", uuid::Uuid::new_v4().as_simple());
        let mut record = FileWatchRecord::new(watch_id.clone(), path, role, prompt);

        if let Some(debounce) = input.get("debounce_ms").and_then(|v| v.as_u64()) {
            record.debounce_ms = debounce;
        }

        if let Err(e) = file_watch::create_file_watch(store, &record).await {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: format!("failed to persist file watch: {e}"),
            });
        }

        ToolOutcome::Completed {
            output: json!({
                "watch_id": watch_id,
                "path": record.path,
                "debounce_ms": record.debounce_ms,
            }),
            verified: Verification::NotApplicable,
        }
    }
}

// ---------------------------------------------------------------------------
// file_watch.list
// ---------------------------------------------------------------------------

pub struct FileWatchListTool {
    id: ToolId,
    schema: Value,
    store: OnceLock<DomainHandle>,
}

impl std::fmt::Debug for FileWatchListTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileWatchListTool")
            .field("id", &self.id)
            .finish()
    }
}

impl Default for FileWatchListTool {
    fn default() -> Self {
        Self::new()
    }
}

impl FileWatchListTool {
    pub fn new() -> Self {
        FileWatchListTool {
            id: ToolId::new(),
            schema: json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
            store: OnceLock::new(),
        }
    }

    pub fn set_file_watch_store(&self, handle: DomainHandle) -> Result<(), DomainHandle> {
        assert_eq!(handle.domain(), KeyDomain::FileWatches);
        self.store.set(handle)
    }
}

#[async_trait]
impl Tool for FileWatchListTool {
    fn id(&self) -> ToolId {
        self.id
    }

    fn name(&self) -> &str {
        "file_watch.list"
    }

    fn description(&self) -> &str {
        "List all file-watch triggers. Returns an array of watch objects \
         with id, path, role, prompt, enabled status, and debounce interval."
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn required_scope(&self, _input: &Value) -> Scope {
        Scope::parse("file_watch.list").expect("known base")
    }

    async fn execute(&self, _input: Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        let Some(store) = self.store.get() else {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: "file_watch.list: no file-watch store configured".to_string(),
            });
        };

        match file_watch::list_file_watches(store).await {
            Ok(watches) => {
                let entries: Vec<Value> = watches
                    .iter()
                    .map(|w| {
                        json!({
                            "watch_id": w.watch_id,
                            "path": w.path,
                            "role": w.role_name,
                            "prompt": w.prompt,
                            "enabled": w.enabled,
                            "debounce_ms": w.debounce_ms,
                        })
                    })
                    .collect();
                ToolOutcome::Completed {
                    output: json!({ "file_watches": entries }),
                    verified: Verification::NotApplicable,
                }
            }
            Err(e) => ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: format!("failed to list file watches: {e}"),
            }),
        }
    }
}

// ---------------------------------------------------------------------------
// file_watch.delete
// ---------------------------------------------------------------------------

pub struct FileWatchDeleteTool {
    id: ToolId,
    schema: Value,
    store: OnceLock<DomainHandle>,
}

impl std::fmt::Debug for FileWatchDeleteTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileWatchDeleteTool")
            .field("id", &self.id)
            .finish()
    }
}

impl Default for FileWatchDeleteTool {
    fn default() -> Self {
        Self::new()
    }
}

impl FileWatchDeleteTool {
    pub fn new() -> Self {
        FileWatchDeleteTool {
            id: ToolId::new(),
            schema: json!({
                "type": "object",
                "properties": {
                    "watch_id": {
                        "type": "string",
                        "description": "The ID of the file watch to delete."
                    }
                },
                "required": ["watch_id"]
            }),
            store: OnceLock::new(),
        }
    }

    pub fn set_file_watch_store(&self, handle: DomainHandle) -> Result<(), DomainHandle> {
        assert_eq!(handle.domain(), KeyDomain::FileWatches);
        self.store.set(handle)
    }
}

#[async_trait]
impl Tool for FileWatchDeleteTool {
    fn id(&self) -> ToolId {
        self.id
    }

    fn name(&self) -> &str {
        "file_watch.delete"
    }

    fn description(&self) -> &str {
        "Delete a file-watch trigger by ID. The watch will no longer fire. \
         Returns whether the watch was found and deleted."
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn required_scope(&self, _input: &Value) -> Scope {
        Scope::parse("file_watch.delete").expect("known base")
    }

    async fn execute(&self, input: Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        let Some(store) = self.store.get() else {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: "file_watch.delete: no file-watch store configured".to_string(),
            });
        };

        let watch_id = input
            .get("watch_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        if watch_id.is_empty() {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: "file_watch.delete requires a non-empty `watch_id` field".to_string(),
            });
        }

        let exists = match file_watch::get_file_watch(store, &watch_id).await {
            Ok(Some(_)) => true,
            Ok(None) => false,
            Err(e) => {
                return ToolOutcome::Failed(AivyxError::Tool {
                    tool: self.id,
                    detail: format!("failed to check file watch: {e}"),
                });
            }
        };

        if !exists {
            return ToolOutcome::Completed {
                output: json!({
                    "deleted": false,
                    "reason": format!("file watch {watch_id} not found")
                }),
                verified: Verification::NotApplicable,
            };
        }

        if let Err(e) = file_watch::delete_file_watch(store, &watch_id).await {
            return ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail: format!("failed to delete file watch: {e}"),
            });
        }

        ToolOutcome::Completed {
            output: json!({ "deleted": true, "watch_id": watch_id }),
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
    fn file_watch_create_scope() {
        let tool = FileWatchCreateTool::new();
        assert_eq!(
            tool.required_scope(&json!({})).as_str(),
            "file_watch.create"
        );
    }

    #[test]
    fn file_watch_create_name_and_schema() {
        let tool = FileWatchCreateTool::new();
        assert_eq!(tool.name(), "file_watch.create");
        let schema = tool.input_schema();
        assert!(schema["required"].as_array().unwrap().contains(&json!("path")));
        assert!(schema["required"].as_array().unwrap().contains(&json!("prompt")));
    }

    #[test]
    fn file_watch_list_scope() {
        let tool = FileWatchListTool::new();
        assert_eq!(
            tool.required_scope(&json!({})).as_str(),
            "file_watch.list"
        );
    }

    #[test]
    fn file_watch_list_name() {
        let tool = FileWatchListTool::new();
        assert_eq!(tool.name(), "file_watch.list");
    }

    #[test]
    fn file_watch_delete_scope() {
        let tool = FileWatchDeleteTool::new();
        assert_eq!(
            tool.required_scope(&json!({})).as_str(),
            "file_watch.delete"
        );
    }

    #[test]
    fn file_watch_delete_name_and_schema() {
        let tool = FileWatchDeleteTool::new();
        assert_eq!(tool.name(), "file_watch.delete");
        let schema = tool.input_schema();
        assert!(schema["required"].as_array().unwrap().contains(&json!("watch_id")));
    }

    /// A19 final review I1 — a file watch's runs execute later as System
    /// turns outside the peer's ceiling, so a peer's request can't make one.
    #[tokio::test]
    async fn file_watch_create_refuses_a_peer_request() {
        let detail = refused_create_detail(aivyx_core::MessageOrigin::Peer).await;
        assert!(detail.contains("federation peer"), "{detail}");
    }

    /// Like schedule.create: a triggered or scheduled run can't create more
    /// automation.
    #[tokio::test]
    async fn file_watch_create_refuses_a_system_run() {
        let detail = refused_create_detail(aivyx_core::MessageOrigin::System).await;
        assert!(detail.contains("triggered or scheduled run"), "{detail}");
    }

    /// Calls `file_watch.create` from `origin`, asserts it was refused and
    /// stored nothing, and returns the refusal text.
    async fn refused_create_detail(origin: aivyx_core::MessageOrigin) -> String {
        use aivyx_core::{
            AgentId, CancellationToken, ChannelContext, ChannelError, ChannelPlatform, SessionId,
            StreamEvent, TurnId, TurnOutcome,
        };
        use aivyx_crypto::MasterKey;
        use aivyx_storage::{RedbStorage, StorageConfig};

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

        let dir = std::env::temp_dir()
            .join(format!("aivyx-file-watch-tool-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let storage = RedbStorage::open(
            StorageConfig::new(dir.join("store.redb")),
            MasterKey::from_raw([12u8; 32]),
        )
        .await
        .unwrap();
        let handle = storage.domain(KeyDomain::FileWatches);
        let tool = FileWatchCreateTool::new();
        tool.set_file_watch_store(handle.clone()).unwrap();

        let ch = NoopChannel { session: SessionId::new(), token: CancellationToken::new() };
        let audit = NoopAudit;
        let ctx = ToolContext {
            agent_id: AgentId::new(),
            session_id: ch.session,
            turn_id: TurnId::new(),
            channel: &ch,
            audit: &audit,
            cancellation: &ch.token,
            message_origin: origin,
        };
        let outcome = tool
            .execute(json!({ "path": dir.to_string_lossy(), "prompt": "summarise it" }), &ctx)
            .await;
        let ToolOutcome::Failed(AivyxError::Tool { detail, .. }) = outcome else {
            panic!("expected a refusal, got {outcome:?}");
        };
        assert!(file_watch::list_file_watches(&handle).await.unwrap().is_empty());
        std::fs::remove_dir_all(&dir).ok();
        detail
    }
}
