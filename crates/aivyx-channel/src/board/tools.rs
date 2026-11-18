//! A20 — the two board tools a role run gets. Both act only on the task of
//! the run whose session called them; the model never names a task.

use std::sync::{Arc, OnceLock};

use aivyx_capability::Scope;
use aivyx_core::{AivyxError, Tool, ToolContext, ToolId, ToolOutcome, Verification};
use async_trait::async_trait;
use serde_json::{Value, json};

use super::service::BoardService;

pub struct BoardCommentTool {
    id: ToolId,
    schema: Value,
    service: OnceLock<Arc<BoardService>>,
}

impl Default for BoardCommentTool {
    fn default() -> Self {
        Self::new()
    }
}

impl BoardCommentTool {
    pub fn new() -> Self {
        BoardCommentTool {
            id: ToolId::new(),
            schema: json!({
                "type": "object",
                "properties": { "text": { "type": "string", "description": "What the operator should read." } },
                "required": ["text"]
            }),
            service: OnceLock::new(),
        }
    }
    pub fn set_service(&self, s: Arc<BoardService>) -> Result<(), Arc<BoardService>> {
        self.service.set(s)
    }
}

#[async_trait]
impl Tool for BoardCommentTool {
    fn id(&self) -> ToolId {
        self.id
    }
    fn name(&self) -> &str {
        "board.comment"
    }
    fn description(&self) -> &str {
        "Add a note to the board task you are working on, for the operator to read. \
         Only works while you are running a task from the operator's board."
    }
    fn input_schema(&self) -> &Value {
        &self.schema
    }
    fn required_scope(&self, _input: &Value) -> Scope {
        Scope::parse("board.write").expect("A20 base")
    }
    async fn execute(&self, input: Value, ctx: &ToolContext<'_>) -> ToolOutcome {
        let fail = |detail: String| {
            ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail,
            })
        };
        let Some(service) = self.service.get() else {
            return fail("board.comment: the board isn't available".into());
        };
        let text = input.get("text").and_then(Value::as_str).unwrap_or("");
        match service.agent_comment(&ctx.session_id, text).await {
            Ok(()) => ToolOutcome::Completed {
                output: json!({ "added": true }),
                verified: Verification::NotApplicable,
            },
            Err(e) => fail(format!("board.comment: {e}")),
        }
    }
}

pub struct BoardSubtaskTool {
    id: ToolId,
    schema: Value,
    service: OnceLock<Arc<BoardService>>,
}

impl Default for BoardSubtaskTool {
    fn default() -> Self {
        Self::new()
    }
}

impl BoardSubtaskTool {
    pub fn new() -> Self {
        BoardSubtaskTool {
            id: ToolId::new(),
            schema: json!({
                "type": "object",
                "properties": {
                    "title": { "type": "string", "description": "A short name for the subtask." },
                    "description": { "type": "string", "description": "What the subtask should do." }
                },
                "required": ["title"]
            }),
            service: OnceLock::new(),
        }
    }
    pub fn set_service(&self, s: Arc<BoardService>) -> Result<(), Arc<BoardService>> {
        self.service.set(s)
    }
}

#[async_trait]
impl Tool for BoardSubtaskTool {
    fn id(&self) -> ToolId {
        self.id
    }
    fn name(&self) -> &str {
        "board.subtask"
    }
    fn description(&self) -> &str {
        "Add a subtask under the board task you are working on. The operator assigns and \
         starts it. Only works while you are running a task from the operator's board."
    }
    fn input_schema(&self) -> &Value {
        &self.schema
    }
    fn required_scope(&self, _input: &Value) -> Scope {
        Scope::parse("board.write").expect("A20 base")
    }
    async fn execute(&self, input: Value, ctx: &ToolContext<'_>) -> ToolOutcome {
        let fail = |detail: String| {
            ToolOutcome::Failed(AivyxError::Tool {
                tool: self.id,
                detail,
            })
        };
        let Some(service) = self.service.get() else {
            return fail("board.subtask: the board isn't available".into());
        };
        let title = input.get("title").and_then(Value::as_str).unwrap_or("");
        let description = input
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or("");
        match service
            .agent_subtask(&ctx.session_id, title, description)
            .await
        {
            Ok(id) => ToolOutcome::Completed {
                output: json!({ "task_id": id }),
                verified: Verification::NotApplicable,
            },
            Err(e) => fail(format!("board.subtask: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;

    use aivyx_audit::PersistentAuditLog;
    use aivyx_capability::CapabilitySet;
    use aivyx_core::{
        Agent, AgentId, CancellationToken, ChannelContext, Message, NullAuditHook, SessionId,
        TurnId, TurnOutcome,
    };
    use aivyx_crypto::MasterKey;
    use aivyx_ipc::board::{Author, EntryKind, NewTask, Owner, TaskState};
    use aivyx_storage::{KeyDomain, RedbStorage, Storage, StorageConfig};

    use crate::board::runner::BoardRunContext;
    use crate::board::service::BoardDeps;

    /// Runs one tool from inside its turn, with the run's own session, and
    /// keeps the outcome.
    struct CallingAgent {
        id: AgentId,
        caps: CapabilitySet,
        tool: Arc<dyn Tool>,
        input: Value,
        outcome: Arc<StdMutex<Option<ToolOutcome>>>,
    }

    #[async_trait]
    impl Agent for CallingAgent {
        fn id(&self) -> AgentId {
            self.id
        }
        fn capabilities(&self) -> &CapabilitySet {
            &self.caps
        }
        async fn turn(&self, _message: Message, channel: &dyn ChannelContext) -> TurnOutcome {
            let token = CancellationToken::new();
            let ctx = ToolContext {
                agent_id: self.id,
                session_id: channel.session_id(),
                turn_id: TurnId::new(),
                channel,
                audit: &NullAuditHook,
                cancellation: &token,
                message_origin: aivyx_core::MessageOrigin::System,
            };
            let out = self.tool.execute(self.input.clone(), &ctx).await;
            *self.outcome.lock().unwrap() = Some(out);
            TurnOutcome::Completed {
                final_message: "done".into(),
                tool_calls_made: 1,
                duration: Duration::ZERO,
            }
        }
    }

    struct Fx {
        _dir: tempfile::TempDir,
        board: Arc<BoardService>,
        outcome: Arc<StdMutex<Option<ToolOutcome>>>,
    }

    async fn fixture(tool: Arc<dyn Tool>, input: Value) -> Fx {
        let dir = tempfile::tempdir().unwrap();
        let storage: Arc<dyn Storage> = RedbStorage::open(
            StorageConfig::new(dir.path().join("s.redb")),
            MasterKey::from_raw([5u8; 32]),
        )
        .await
        .unwrap();
        let audit = Arc::new(
            PersistentAuditLog::open(Arc::clone(&storage), [7u8; 32])
                .await
                .unwrap(),
        );
        let outcome = Arc::new(StdMutex::new(None));
        let agent = Arc::new(CallingAgent {
            id: AgentId::new(),
            caps: CapabilitySet::empty(),
            tool,
            input,
            outcome: Arc::clone(&outcome),
        });
        let mut roles = HashMap::new();
        roles.insert(
            "writer".to_string(),
            CapabilitySet::from_scopes([Scope::parse("memory.read").unwrap()]),
        );
        let board = Arc::new(BoardService::new(BoardDeps {
            store: storage.domain(KeyDomain::Board),
            missions: storage.domain(KeyDomain::Missions),
            audit,
            agent,
            turn_lock: Arc::new(tokio::sync::Mutex::new(())),
            parker: None,
            roles,
            team: None,
            notify: None,
            notify_target: None,
        }));
        Fx {
            _dir: dir,
            board,
            outcome,
        }
    }

    /// Seeds a writer-owned task, starts it and waits for the run to end.
    async fn run_task(fx: &Fx) -> String {
        let g = fx.board.create_goal("Grow", "").await.unwrap();
        let p = fx.board.create_project(&g, "Report", "").await.unwrap();
        let id = fx
            .board
            .create_task(NewTask {
                project_id: p,
                parent_task: None,
                title: "Draft".into(),
                description: String::new(),
                owner: Some(Owner::Role {
                    name: "writer".into(),
                }),
                review_required: true,
            })
            .await
            .unwrap();
        fx.board.start(&id).await.unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while fx.board.task(&id).await.unwrap().state == TaskState::Running {
            assert!(tokio::time::Instant::now() < deadline, "still Running");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        id
    }

    fn take(fx: &Fx) -> ToolOutcome {
        fx.outcome.lock().unwrap().take().expect("the tool ran")
    }

    fn failure(outcome: ToolOutcome) -> String {
        match outcome {
            ToolOutcome::Failed(AivyxError::Tool { detail, .. }) => detail,
            other => panic!("expected a tool error, got {other:?}"),
        }
    }

    /// A call from a session the board never registered.
    async fn call_outside(tool: &dyn Tool, input: Value) -> ToolOutcome {
        let ch = BoardRunContext {
            session: SessionId::new(),
            ceiling: CapabilitySet::empty(),
            cancel: CancellationToken::new(),
        };
        let token = CancellationToken::new();
        let ctx = ToolContext {
            agent_id: AgentId::new(),
            session_id: ch.session,
            turn_id: TurnId::new(),
            channel: &ch,
            audit: &NullAuditHook,
            cancellation: &token,
            message_origin: aivyx_core::MessageOrigin::Operator,
        };
        tool.execute(input, &ctx).await
    }

    #[test]
    fn both_tools_need_board_write() {
        let c = BoardCommentTool::new();
        let s = BoardSubtaskTool::new();
        assert_eq!(c.name(), "board.comment");
        assert_eq!(s.name(), "board.subtask");
        assert_eq!(c.required_scope(&json!({})).as_str(), "board.write");
        assert_eq!(s.required_scope(&json!({})).as_str(), "board.write");
    }

    #[tokio::test]
    async fn a_comment_from_the_run_lands_on_its_task() {
        let tool = Arc::new(BoardCommentTool::new());
        let fx = fixture(tool.clone(), json!({ "text": "halfway there" })).await;
        tool.set_service(Arc::clone(&fx.board)).ok().unwrap();
        let id = run_task(&fx).await;
        match take(&fx) {
            ToolOutcome::Completed { output, .. } => assert_eq!(output, json!({ "added": true })),
            other => panic!("expected Completed, got {other:?}"),
        }
        let task = fx.board.task(&id).await.unwrap();
        assert!(task.thread.iter().any(|e| {
            matches!(e.author, Author::Agent { .. })
                && matches!(&e.entry, EntryKind::Comment { text } if text == "halfway there")
        }));
    }

    #[tokio::test]
    async fn a_subtask_from_the_run_is_added_under_its_task() {
        let tool = Arc::new(BoardSubtaskTool::new());
        let fx = fixture(
            tool.clone(),
            json!({ "title": "Check the totals", "description": "the Q4 column" }),
        )
        .await;
        tool.set_service(Arc::clone(&fx.board)).ok().unwrap();
        let id = run_task(&fx).await;
        let sub = match take(&fx) {
            ToolOutcome::Completed { output, .. } => {
                output["task_id"].as_str().unwrap().to_string()
            }
            other => panic!("expected Completed, got {other:?}"),
        };
        let sub = fx.board.task(&sub).await.unwrap();
        assert_eq!(sub.parent_task.as_deref(), Some(id.as_str()));
        assert_eq!(sub.title, "Check the totals");
        assert_eq!(sub.description, "(written by an agent) the Q4 column");
        assert_eq!(sub.owner, None);
    }

    #[tokio::test]
    async fn a_session_outside_a_board_run_is_refused() {
        let comment = Arc::new(BoardCommentTool::new());
        let subtask = BoardSubtaskTool::new();
        let fx = fixture(comment.clone(), json!({ "text": "x" })).await;
        comment.set_service(Arc::clone(&fx.board)).ok().unwrap();
        subtask.set_service(Arc::clone(&fx.board)).ok().unwrap();
        let e = failure(call_outside(comment.as_ref(), json!({ "text": "x" })).await);
        assert!(e.contains("not in a board run"), "{e}");
        let e = failure(call_outside(&subtask, json!({ "title": "x" })).await);
        assert!(e.contains("not in a board run"), "{e}");
    }

    #[tokio::test]
    async fn without_a_service_the_board_isnt_available() {
        let e = failure(call_outside(&BoardCommentTool::new(), json!({ "text": "x" })).await);
        assert!(e.contains("isn't available"), "{e}");
        let e = failure(call_outside(&BoardSubtaskTool::new(), json!({ "title": "x" })).await);
        assert!(e.contains("isn't available"), "{e}");
    }
}
