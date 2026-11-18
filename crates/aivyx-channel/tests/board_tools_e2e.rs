//! A20, end to end at the agent level: a board role run whose turn calls
//! `board.comment` through the real `ConcreteAgent` turn loop, tool registry
//! and capability check. Together the tests show the three things that must
//! hold for the comment to land:
//! - the floor: the agent's own set holds `board.write` (the daemon adds it);
//!   without it the call is denied, since a ceiling only narrows;
//! - the ceiling: the run is capped to the role's scopes plus `board.write`,
//!   so a scope the agent holds but the role doesn't is denied in the run;
//! - the session binding: the same call from a session the board didn't
//!   register is refused by the tool.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};

use aivyx_audit::PersistentAuditLog;
use aivyx_capability::{CapabilitySet, Scope};
use aivyx_channel::board::runner::{BoardRunContext, role_ceiling};
use aivyx_channel::board::tools::{BoardCommentTool, BoardSubtaskTool};
use aivyx_channel::board::{BoardDeps, BoardService};
use aivyx_core::{
    Agent, AgentId, CancellationToken, ConcreteAgent, Message, NextStep, NullAuditHook, SessionId,
    Tool, ToolContext, ToolId, ToolOutcome, ToolRegistry, VecPlanner, Verification,
};
use aivyx_crypto::MasterKey;
use aivyx_ipc::board::{Author, EntryKind, NewTask, Owner, Task, TaskState};
use aivyx_storage::{KeyDomain, RedbStorage, Storage, StorageConfig};

/// A tool needing `fs.read`: the agent holds it, the role doesn't.
struct Probe {
    id: ToolId,
    schema: Value,
    ran: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for Probe {
    fn id(&self) -> ToolId {
        self.id
    }
    fn name(&self) -> &str {
        "probe.read"
    }
    fn description(&self) -> &str {
        "test probe"
    }
    fn input_schema(&self) -> &Value {
        &self.schema
    }
    fn required_scope(&self, _input: &Value) -> Scope {
        Scope::parse("fs.read").unwrap()
    }
    async fn execute(&self, _input: Value, _ctx: &ToolContext<'_>) -> ToolOutcome {
        self.ran.fetch_add(1, Ordering::SeqCst);
        ToolOutcome::Completed {
            output: json!({}),
            verified: Verification::NotApplicable,
        }
    }
}

struct Fx {
    _dir: tempfile::TempDir,
    board: Arc<BoardService>,
    agent: Arc<dyn Agent>,
    probe_ran: Arc<AtomicUsize>,
}

/// The daemon's wiring in miniature: the tools are made first, the agent
/// over a real registry, then the board over the agent, then the tools get
/// the board. `floor_has_board_write` is the daemon-mode floor grant.
async fn fixture(floor_has_board_write: bool) -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let storage: Arc<dyn Storage> = RedbStorage::open(
        StorageConfig::new(dir.path().join("s.redb")),
        MasterKey::from_raw([4u8; 32]),
    )
    .await
    .unwrap();
    let audit = Arc::new(
        PersistentAuditLog::open(Arc::clone(&storage), [8u8; 32])
            .await
            .unwrap(),
    );

    let comment = Arc::new(BoardCommentTool::new());
    let subtask = Arc::new(BoardSubtaskTool::new());
    let probe_ran = Arc::new(AtomicUsize::new(0));
    let probe = Arc::new(Probe {
        id: ToolId::new(),
        schema: json!({"type": "object"}),
        ran: Arc::clone(&probe_ran),
    });
    let plan = vec![
        NextStep::ToolCall {
            tool_id: probe.id,
            input: json!({}),
            auto_corrected_from: None,
            extracted_from_text: None,
        },
        NextStep::ToolCall {
            tool_id: comment.id(),
            input: json!({"text": "found the Q4 numbers"}),
            auto_corrected_from: None,
            extracted_from_text: None,
        },
        NextStep::FinalMessage("drafted".to_string()),
    ];

    let mut floor = vec![
        Scope::parse("fs.read").unwrap(),
        Scope::parse("memory.read").unwrap(),
    ];
    if floor_has_board_write {
        floor.push(Scope::parse("board.write").unwrap());
    }
    let tools: Vec<Arc<dyn Tool>> = vec![
        comment.clone() as Arc<dyn Tool>,
        subtask.clone() as Arc<dyn Tool>,
        probe as Arc<dyn Tool>,
    ];
    let agent: Arc<dyn Agent> = Arc::new(ConcreteAgent::new(
        AgentId::new(),
        CapabilitySet::from_scopes(floor),
        Arc::new(ToolRegistry::new(tools)),
        Arc::new(NullAuditHook),
        move || Box::new(VecPlanner::new(plan.clone())),
    ));

    // The role declares its own scopes and doesn't list `board.write`.
    let mut roles = HashMap::new();
    roles.insert(
        "writer".to_string(),
        CapabilitySet::from_scopes([Scope::parse("memory.read").unwrap()]),
    );
    let board = Arc::new(BoardService::new(BoardDeps {
        store: storage.domain(KeyDomain::Board),
        missions: storage.domain(KeyDomain::Missions),
        audit,
        agent: Arc::clone(&agent),
        turn_lock: Arc::new(tokio::sync::Mutex::new(())),
        parker: None,
        roles,
        team: None,
        notify: None,
        notify_target: None,
    }));
    comment.set_service(Arc::clone(&board)).ok().unwrap();
    subtask.set_service(Arc::clone(&board)).ok().unwrap();
    Fx {
        _dir: dir,
        board,
        agent,
        probe_ran,
    }
}

async fn seed(fx: &Fx) -> String {
    let g = fx.board.create_goal("Grow sales", "").await.unwrap();
    let p = fx.board.create_project(&g, "Q4 report", "").await.unwrap();
    fx.board
        .create_task(NewTask {
            project_id: p,
            parent_task: None,
            title: "Draft".into(),
            description: "the numbers".into(),
            owner: Some(Owner::Role {
                name: "writer".into(),
            }),
            review_required: true,
        })
        .await
        .unwrap()
}

async fn run(fx: &Fx, id: &str) -> Task {
    fx.board.start(id).await.unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let t = fx.board.task(id).await.unwrap();
        if t.state != TaskState::Running {
            return t;
        }
        assert!(tokio::time::Instant::now() < deadline, "still Running");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn agent_comments(t: &Task) -> Vec<String> {
    t.thread
        .iter()
        .filter_map(|e| match (&e.author, &e.entry) {
            (Author::Agent { .. }, EntryKind::Comment { text }) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_role_run_comments_on_its_own_task_through_the_turn_loop() {
    let fx = fixture(true).await;
    let id = seed(&fx).await;
    let task = run(&fx, &id).await;

    assert_eq!(task.state, TaskState::Review, "{:?}", task.thread);
    assert_eq!(agent_comments(&task), ["found the Q4 numbers"]);
    // The comment carries the run's own mission id.
    let mission = task
        .thread
        .iter()
        .find_map(|e| match &e.author {
            Author::Agent { mission_id } => Some(mission_id.clone()),
            _ => None,
        })
        .unwrap();
    assert!(mission.starts_with("b-"), "{mission}");
    // The ceiling: `fs.read` is in the agent's set but not the role's.
    assert_eq!(
        fx.probe_ran.load(Ordering::SeqCst),
        0,
        "the ceiling let fs.read through"
    );
}

#[tokio::test]
async fn without_the_floor_grant_the_ceiling_alone_cant_grant_board_write() {
    let fx = fixture(false).await;
    let id = seed(&fx).await;
    let task = run(&fx, &id).await;

    assert!(agent_comments(&task).is_empty(), "{:?}", task.thread);
    assert_eq!(task.state, TaskState::Review, "the turn itself still ends");
}

#[tokio::test]
async fn the_same_call_outside_a_board_run_is_refused() {
    let fx = fixture(true).await;
    let id = seed(&fx).await;

    // A session the board never registered, with the same ceiling a run
    // gets: the scope check passes, the tool refuses.
    let ch = BoardRunContext {
        session: SessionId::new(),
        ceiling: role_ceiling(&CapabilitySet::from_scopes([
            Scope::parse("memory.read").unwrap()
        ])),
        cancel: CancellationToken::new(),
    };
    let outcome = fx
        .agent
        .turn(Message::text(ch.session, "go").system_originated(), &ch)
        .await;
    assert!(
        matches!(outcome, aivyx_core::TurnOutcome::Completed { .. }),
        "{outcome:?}"
    );

    let task = fx.board.task(&id).await.unwrap();
    assert_eq!(task.state, TaskState::Todo);
    assert!(task.thread.is_empty(), "{:?}", task.thread);
}
