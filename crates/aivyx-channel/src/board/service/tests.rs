//! Tests for the board service: a temp store, a real audit chain, and a
//! scripted agent that records what each turn saw.

use super::*;
use aivyx_capability::Scope;
use aivyx_core::{AgentId, MessageContent, MessageOrigin, TokenUsage, TurnId};
use aivyx_crypto::MasterKey;
use aivyx_ipc::board::{Author, EntryKind};
use aivyx_storage::{KeyDomain, RedbStorage, Storage, StorageConfig};
use async_trait::async_trait;
use std::sync::Mutex as StdMutex;

const WAIT: Duration = Duration::from_secs(5);

/// What one turn saw.
#[derive(Debug, Clone)]
struct Seen {
    text: String,
    origin: MessageOrigin,
    ceiling: Option<CapabilitySet>,
    session: SessionId,
}

#[derive(Clone)]
enum Reply {
    Done(String),
    Fail(String),
    Panic,
}

/// Records each turn, optionally holds until released (or cancelled),
/// optionally appends an `LlmCost`, then replies as scripted.
struct ScriptedAgent {
    id: AgentId,
    caps: CapabilitySet,
    seen: Arc<StdMutex<Vec<Seen>>>,
    reply: StdMutex<Reply>,
    hold: bool,
    started: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    cost_log: Option<Arc<PersistentAuditLog>>,
}

impl ScriptedAgent {
    fn new(reply: Reply) -> Self {
        ScriptedAgent {
            id: AgentId::new(),
            caps: CapabilitySet::empty(),
            seen: Arc::new(StdMutex::new(Vec::new())),
            reply: StdMutex::new(reply),
            hold: false,
            started: Arc::new(tokio::sync::Notify::new()),
            release: Arc::new(tokio::sync::Notify::new()),
            cost_log: None,
        }
    }
}

#[async_trait]
impl aivyx_core::Agent for ScriptedAgent {
    fn id(&self) -> AgentId {
        self.id
    }
    fn capabilities(&self) -> &CapabilitySet {
        &self.caps
    }
    async fn turn(&self, message: Message, channel: &dyn ChannelContext) -> TurnOutcome {
        let text = match &message.content {
            MessageContent::Text(t) => t.clone(),
            _ => String::new(),
        };
        self.seen.lock().unwrap().push(Seen {
            text,
            origin: message.origin,
            ceiling: channel.capability_ceiling(),
            session: channel.session_id(),
        });
        self.started.notify_one();
        if self.hold {
            let cancel = channel.cancellation_token();
            tokio::select! {
                _ = self.release.notified() => {}
                _ = cancel.cancelled() => return TurnOutcome::Cancelled { tool_calls_made: 0 },
            }
        }
        if let Some(log) = &self.cost_log {
            let usage = TokenUsage {
                input_tokens: 100,
                output_tokens: 20,
                ..TokenUsage::default()
            };
            log.append(AuditEvent::LlmCost {
                turn_id: TurnId::new(),
                model: "m".into(),
                usage,
            })
            .unwrap();
        }
        match self.reply.lock().unwrap().clone() {
            Reply::Done(text) => TurnOutcome::Completed {
                final_message: text,
                tool_calls_made: 0,
                duration: Duration::ZERO,
            },
            Reply::Fail(why) => TurnOutcome::Failed(aivyx_core::AivyxError::Internal(why)),
            Reply::Panic => panic!("scripted agent panic"),
        }
    }
}

struct Fx {
    _dir: tempfile::TempDir,
    storage: Arc<dyn Storage>,
    board: BoardService,
    audit: Arc<PersistentAuditLog>,
    missions: DomainHandle,
}

struct Setup {
    agent: Arc<ScriptedAgent>,
    team: Option<TeamMissionService>,
    notify: Option<Arc<NotifyDispatcher>>,
}

async fn open() -> (tempfile::TempDir, Arc<dyn Storage>, Arc<PersistentAuditLog>) {
    let dir = tempfile::tempdir().unwrap();
    let storage: Arc<dyn Storage> = RedbStorage::open(
        StorageConfig::new(dir.path().join("s.redb")),
        MasterKey::from_raw([3u8; 32]),
    )
    .await
    .unwrap();
    let audit = Arc::new(
        PersistentAuditLog::open(Arc::clone(&storage), [7u8; 32])
            .await
            .unwrap(),
    );
    (dir, storage, audit)
}

fn roles() -> HashMap<String, CapabilitySet> {
    let mut roles = HashMap::new();
    roles.insert(
        "writer".to_string(),
        CapabilitySet::from_scopes([
            Scope::parse("fs.read").unwrap(),
            Scope::parse("memory.read").unwrap(),
        ]),
    );
    roles
}

async fn fixture_with(setup: Setup) -> Fx {
    let (dir, storage, audit) = open().await;
    fixture_on(dir, storage, audit, setup).await
}

async fn fixture_on(
    dir: tempfile::TempDir,
    storage: Arc<dyn Storage>,
    audit: Arc<PersistentAuditLog>,
    setup: Setup,
) -> Fx {
    let missions = storage.domain(KeyDomain::Missions);
    let deps = BoardDeps {
        store: storage.domain(KeyDomain::Board),
        missions: missions.clone(),
        audit: Arc::clone(&audit),
        agent: setup.agent,
        turn_lock: Arc::new(tokio::sync::Mutex::new(())),
        parker: None,
        roles: roles(),
        team: setup.team,
        notify_target: setup.notify.as_ref().map(|_| "ops".to_string()),
        notify: setup.notify,
    };
    Fx {
        _dir: dir,
        storage,
        board: BoardService::with_team_poll(deps, Duration::from_millis(20)),
        audit,
        missions,
    }
}

async fn fixture(agent: Arc<ScriptedAgent>) -> Fx {
    fixture_with(Setup {
        agent,
        team: None,
        notify: None,
    })
    .await
}

/// A goal, a project and one task owned by `owner`.
async fn seed(fx: &Fx, owner: Option<Owner>) -> String {
    let g = fx.board.create_goal("Grow sales", "").await.unwrap();
    let p = fx.board.create_project(&g, "Q4 report", "").await.unwrap();
    fx.board
        .create_task(NewTask {
            project_id: p,
            parent_task: None,
            title: "Draft".into(),
            description: "the numbers".into(),
            owner,
            review_required: true,
        })
        .await
        .unwrap()
}

fn writer() -> Option<Owner> {
    Some(Owner::Role {
        name: "writer".into(),
    })
}

async fn wait_not_running(fx: &Fx, id: &str) -> Task {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let t = fx.board.task(id).await.unwrap();
        if t.state != TaskState::Running {
            return t;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "task {id} still Running"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn mission_state(fx: &Fx, id: &str) -> mission::MissionState {
    mission::get_mission(&fx.missions, id)
        .await
        .unwrap()
        .unwrap()
        .state
}

/// `(op, actor)` of every `BoardChanged` on the chain, in order.
fn ops(audit: &PersistentAuditLog) -> Vec<(String, String)> {
    audit
        .entries()
        .unwrap()
        .into_iter()
        .filter_map(|e| match e.event {
            AuditEvent::BoardChanged { op, actor, .. } => Some((op, actor)),
            _ => None,
        })
        .collect()
}

fn op_names(audit: &PersistentAuditLog) -> Vec<String> {
    ops(audit).into_iter().map(|(op, _)| op).collect()
}

fn last_run(t: &Task) -> (String, RunOutcome, String, u64) {
    t.thread
        .iter()
        .rev()
        .find_map(|e| match &e.entry {
            EntryKind::Run {
                mission_id,
                outcome,
                answer,
                tokens,
                ..
            } => Some((mission_id.clone(), outcome.clone(), answer.clone(), *tokens)),
            _ => None,
        })
        .expect("a Run entry")
}

async fn assert_chain_verifies(fx: &Fx) {
    assert!(fx.audit.is_healthy());
    fx.audit.verify().unwrap();
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let r = PersistentAuditLog::verify_from_disk(Arc::clone(&fx.storage), [7u8; 32]).await;
        match &r {
            Ok(rep) if rep.entries_verified == fx.audit.len() => return,
            _ if tokio::time::Instant::now() >= deadline => {
                panic!("chain does not verify from disk: {r:?}")
            }
            _ => tokio::time::sleep(Duration::from_millis(20)).await,
        }
    }
}

// ---- 1. creation --------------------------------------------------------

#[tokio::test]
async fn creation_persists_and_checks_parents_and_titles() {
    let fx = fixture(Arc::new(ScriptedAgent::new(Reply::Done("done".into())))).await;
    let t = seed(&fx, None).await;
    let snap = fx.board.snapshot().await.unwrap();
    assert_eq!(snap.goals.len(), 1);
    assert_eq!(snap.projects.len(), 1);
    assert_eq!(snap.tasks.len(), 1);
    assert_eq!(snap.tasks[0].id, t);
    assert_eq!(snap.projects[0].goal_id, snap.goals[0].id);
    assert_eq!(snap.tasks[0].project_id, snap.projects[0].id);
    assert_eq!(snap.tasks[0].state, TaskState::Todo);

    assert_eq!(
        fx.board.create_project("g-nope", "P", "").await,
        Err(BoardError::NotFound("goal"))
    );
    let bad = NewTask {
        project_id: "p-nope".into(),
        parent_task: None,
        title: "T".into(),
        description: String::new(),
        owner: None,
        review_required: true,
    };
    assert_eq!(
        fx.board.create_task(bad.clone()).await,
        Err(BoardError::NotFound("project"))
    );
    assert_eq!(
        fx.board.create_goal("  ", "").await,
        Err(BoardError::Empty("the title"))
    );
    let g = snap.goals[0].id.clone();
    assert_eq!(
        fx.board.create_project(&g, "", "").await,
        Err(BoardError::Empty("the title"))
    );
    let empty = NewTask {
        project_id: snap.projects[0].id.clone(),
        title: " ".into(),
        ..bad
    };
    assert_eq!(
        fx.board.create_task(empty).await,
        Err(BoardError::Empty("the title"))
    );
    // Only the three successful creations reached the chain.
    assert_eq!(op_names(&fx.audit), ["created", "created", "created"]);
    assert!(ops(&fx.audit).iter().all(|(_, a)| a == "operator"));
}

// ---- 2. role run, success -----------------------------------------------

#[tokio::test]
async fn a_role_run_succeeds_into_review() {
    let (dir, storage, audit) = open().await;
    let mut agent = ScriptedAgent::new(Reply::Done("done".into()));
    agent.cost_log = Some(Arc::clone(&audit));
    let agent = Arc::new(agent);
    let seen = Arc::clone(&agent.seen);
    let fx = fixture_on(
        dir,
        storage,
        audit,
        Setup {
            agent,
            team: None,
            notify: None,
        },
    )
    .await;
    let t = seed(&fx, writer()).await;

    let mid = fx.board.start(&t).await.unwrap();
    assert!(mid.starts_with("b-") && mid.len() == 10, "{mid}");
    assert!(matches!(
        fx.board.task(&t).await.unwrap().state,
        TaskState::Running | TaskState::Review
    ));

    let task = wait_not_running(&fx, &t).await;
    assert_eq!(task.state, TaskState::Review);
    assert!(task.active_run.is_none());
    let (run_mid, outcome, answer, tokens) = last_run(&task);
    assert_eq!(run_mid, mid);
    assert_eq!(outcome, RunOutcome::Succeeded);
    assert_eq!(answer, "done");
    assert_eq!(tokens, 120, "input + output of the turn's LlmCost");
    assert_eq!(
        mission_state(&fx, &mid).await,
        mission::MissionState::Completed
    );
    let record = mission::get_mission(&fx.missions, &mid)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(record.role_name, "writer");

    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].origin, MessageOrigin::System);
    for title in ["Draft", "Grow sales", "Q4 report", "the numbers"] {
        assert!(seen[0].text.contains(title), "{title} in {}", seen[0].text);
    }
    let mut ceiling: Vec<String> = seen[0]
        .ceiling
        .as_ref()
        .expect("a ceiling")
        .iter()
        .map(|s| s.to_string())
        .collect();
    ceiling.sort();
    assert_eq!(ceiling, ["board.write", "fs.read", "memory.read"]);

    // 8. audit: three creations, started, finished; the chain verifies.
    assert_eq!(
        ops(&fx.audit),
        [
            ("created".into(), "operator".into()),
            ("created".into(), "operator".into()),
            ("created".into(), "operator".into()),
            ("started".into(), "operator".into()),
            ("finished".into(), "agent".into()),
        ]
    );
    fx.board.approve(&t).await.unwrap();
    assert_eq!(fx.board.task(&t).await.unwrap().state, TaskState::Done);
    assert_eq!(op_names(&fx.audit).last().unwrap(), "approved");
    assert_chain_verifies(&fx).await;
}

// ---- 3. role run, failure -----------------------------------------------

#[tokio::test]
async fn a_failed_role_run_goes_back_to_todo() {
    let fx = fixture(Arc::new(ScriptedAgent::new(Reply::Fail("boom".into())))).await;
    let t = seed(&fx, writer()).await;
    let mid = fx.board.start(&t).await.unwrap();
    let task = wait_not_running(&fx, &t).await;
    assert_eq!(task.state, TaskState::Todo);
    let (_, outcome, _, _) = last_run(&task);
    match outcome {
        RunOutcome::Failed { reason } => assert!(reason.contains("boom"), "{reason}"),
        other => panic!("expected Failed, got {other:?}"),
    }
    assert_eq!(
        mission_state(&fx, &mid).await,
        mission::MissionState::Cancelled
    );
    assert_eq!(
        op_names(&fx.audit)[3..],
        ["started".to_string(), "finished".to_string()]
    );
    assert_chain_verifies(&fx).await;
}

// ---- 4. send-back comments reach the brief -------------------------------

#[tokio::test]
async fn send_back_notes_reach_the_next_brief() {
    let agent = Arc::new(ScriptedAgent::new(Reply::Done("done".into())));
    let seen = Arc::clone(&agent.seen);
    let fx = fixture(agent).await;
    let t = seed(&fx, writer()).await;
    fx.board.start(&t).await.unwrap();
    assert_eq!(wait_not_running(&fx, &t).await.state, TaskState::Review);
    fx.board
        .send_back(&t, "use last month's figures")
        .await
        .unwrap();
    assert_eq!(fx.board.task(&t).await.unwrap().state, TaskState::Todo);
    fx.board.start(&t).await.unwrap();
    wait_not_running(&fx, &t).await;
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 2);
    assert!(!seen[0].text.contains("last month"));
    assert!(seen[1].text.contains("use last month's figures"));
    assert_eq!(
        op_names(&fx.audit)[3..],
        ["started", "finished", "sent_back", "started", "finished"].map(String::from)
    );
}

// ---- 5. cancel while running --------------------------------------------

#[tokio::test]
async fn cancel_stops_the_run_and_stays_cancelled() {
    let mut agent = ScriptedAgent::new(Reply::Done("done".into()));
    agent.hold = true;
    let agent = Arc::new(agent);
    let started = Arc::clone(&agent.started);
    let fx = fixture(agent).await;
    let t = seed(&fx, writer()).await;
    let mid = fx.board.start(&t).await.unwrap();
    tokio::time::timeout(WAIT, started.notified())
        .await
        .expect("the turn started");

    fx.board.cancel(&t).await.unwrap();
    assert_eq!(fx.board.task(&t).await.unwrap().state, TaskState::Cancelled);

    // The run ends (the token fired) and closes its mission record.
    let deadline = tokio::time::Instant::now() + WAIT;
    while mission_state(&fx, &mid).await != mission::MissionState::Cancelled {
        assert!(tokio::time::Instant::now() < deadline, "mission not closed");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let task = fx.board.task(&t).await.unwrap();
    assert_eq!(task.state, TaskState::Cancelled);
    assert!(
        !task
            .thread
            .iter()
            .any(|e| matches!(e.entry, EntryKind::Run { .. })),
        "the cancelled run didn't finish the task"
    );
    assert_eq!(
        op_names(&fx.audit)[3..],
        ["started".to_string(), "cancelled".to_string()]
    );
    assert!(fx.board.inner.runs.lock().unwrap().is_empty());
    assert!(fx.board.inner.cancels.lock().unwrap().is_empty());
    assert_chain_verifies(&fx).await;
}

// ---- 6. agent tools -----------------------------------------------------

#[tokio::test]
async fn agent_tools_work_only_from_the_runs_session() {
    let mut agent = ScriptedAgent::new(Reply::Done("done".into()));
    agent.hold = true;
    let agent = Arc::new(agent);
    let (started, release, seen) = (
        Arc::clone(&agent.started),
        Arc::clone(&agent.release),
        Arc::clone(&agent.seen),
    );
    let fx = fixture(agent).await;
    let t = seed(&fx, writer()).await;
    let mid = fx.board.start(&t).await.unwrap();
    tokio::time::timeout(WAIT, started.notified())
        .await
        .expect("the turn started");
    let session = seen.lock().unwrap()[0].session;
    assert_eq!(
        fx.board.run_binding(&session),
        Some((t.clone(), mid.clone()))
    );

    fx.board.agent_comment(&session, "halfway").await.unwrap();
    assert_eq!(
        fx.board.agent_comment(&SessionId::new(), "sneaky").await,
        Err(BoardError::NotAllowed("not in a board run"))
    );
    let sub = fx
        .board
        .agent_subtask(&session, "Check sources", "the PDFs")
        .await
        .unwrap();
    assert_eq!(
        fx.board.agent_subtask(&SessionId::new(), "x", "").await,
        Err(BoardError::NotAllowed("not in a board run"))
    );

    let parent = fx.board.task(&t).await.unwrap();
    let comment = parent
        .thread
        .iter()
        .find(|e| matches!(&e.entry, EntryKind::Comment { text } if text == "halfway"))
        .expect("the comment");
    assert_eq!(
        comment.author,
        Author::Agent {
            mission_id: mid.clone()
        }
    );
    let sub = fx.board.task(&sub).await.unwrap();
    assert_eq!(sub.parent_task.as_deref(), Some(t.as_str()));
    assert_eq!(sub.project_id, parent.project_id);
    assert_eq!(sub.owner, None);
    assert!(sub.review_required);
    assert_eq!(sub.state, TaskState::Todo);

    release.notify_one();
    wait_not_running(&fx, &t).await;
    assert_eq!(fx.board.run_binding(&session), None, "unregistered");
    assert_eq!(
        fx.board.agent_comment(&session, "late").await,
        Err(BoardError::NotAllowed("not in a board run"))
    );
    assert_eq!(
        ops(&fx.audit)[3..],
        [
            ("started".into(), "operator".into()),
            ("commented".into(), "agent".into()),
            ("subtask_added".into(), "agent".into()),
            ("finished".into(), "agent".into()),
        ]
    );
    assert_chain_verifies(&fx).await;
}

#[tokio::test]
async fn a_panicking_run_still_unregisters_its_session() {
    let agent = Arc::new(ScriptedAgent::new(Reply::Panic));
    let seen = Arc::clone(&agent.seen);
    let fx = fixture(agent).await;
    let t = seed(&fx, writer()).await;
    fx.board.start(&t).await.unwrap();
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let ran = !seen.lock().unwrap().is_empty();
        if ran
            && fx.board.inner.runs.lock().unwrap().is_empty()
            && fx.board.inner.cancels.lock().unwrap().is_empty()
        {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "still registered");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let session = seen.lock().unwrap()[0].session;
    assert_eq!(fx.board.run_binding(&session), None);
}

// ---- 7. restart sweep ---------------------------------------------------

#[tokio::test]
async fn the_restart_sweep_returns_running_tasks_to_todo() {
    let fx = fixture(Arc::new(ScriptedAgent::new(Reply::Done("done".into())))).await;
    let t = seed(&fx, writer()).await;
    let idle = seed(&fx, writer()).await;
    // A task left Running by a daemon that died, with its mission record.
    let mut task = fx.board.task(&t).await.unwrap();
    model::start(
        &mut task,
        RunRef {
            mission_id: "b-dead0001".into(),
            kind: RunKind::Role,
        },
        now_ms(),
    )
    .unwrap();
    store::put_task(&fx.board.inner.deps.store, &task)
        .await
        .unwrap();
    let mut record =
        mission::MissionRecord::new("b-dead0001".into(), "writer".into(), "Draft".into());
    mission::transition_to_running(&mut record).unwrap();
    mission::create_mission(&fx.missions, &record)
        .await
        .unwrap();
    let before = op_names(&fx.audit).len();

    assert_eq!(fx.board.reconcile_at_startup().await.unwrap(), 1);
    let task = fx.board.task(&t).await.unwrap();
    assert_eq!(task.state, TaskState::Todo);
    assert_eq!(last_run(&task).1, RunOutcome::Interrupted);
    assert_eq!(
        mission_state(&fx, "b-dead0001").await,
        mission::MissionState::Cancelled
    );
    assert_eq!(fx.board.task(&idle).await.unwrap().state, TaskState::Todo);
    assert_eq!(op_names(&fx.audit)[before..], ["interrupted".to_string()]);
    assert_eq!(fx.board.reconcile_at_startup().await.unwrap(), 0);
}

// ---- 8. audit for the remaining operator operations ---------------------

#[tokio::test]
async fn each_operator_operation_appends_one_board_changed() {
    let fx = fixture(Arc::new(ScriptedAgent::new(Reply::Done("done".into())))).await;
    let t = seed(&fx, None).await;
    fx.board.comment(&t, "note").await.unwrap();
    fx.board.assign(&t, writer()).await.unwrap();
    // Refused operations append nothing.
    assert!(fx.board.approve(&t).await.is_err());
    assert!(fx.board.send_back(&t, "why").await.is_err());
    assert!(fx.board.comment(&t, " ").await.is_err());
    fx.board.cancel(&t).await.unwrap();
    assert!(fx.board.cancel(&t).await.is_err());
    assert!(fx.board.start(&t).await.is_err());
    assert_eq!(
        op_names(&fx.audit),
        [
            "created",
            "created",
            "created",
            "commented",
            "assigned",
            "cancelled"
        ]
        .map(String::from)
    );
    // No titles or comment text on the chain.
    let json = serde_json::to_string(&fx.audit.entries().unwrap()).unwrap();
    for word in ["Grow sales", "Q4 report", "Draft", "note"] {
        assert!(!json.contains(word), "{word} leaked into the chain");
    }
    assert_chain_verifies(&fx).await;
}

#[tokio::test]
async fn start_needs_a_known_role_and_an_owner() {
    let fx = fixture(Arc::new(ScriptedAgent::new(Reply::Done("done".into())))).await;
    let t = seed(&fx, None).await;
    assert_eq!(
        fx.board.start(&t).await,
        Err(BoardError::NotAllowed(
            "assign an owner before starting the task"
        ))
    );
    fx.board
        .assign(
            &t,
            Some(Owner::Role {
                name: "ghost".into(),
            }),
        )
        .await
        .unwrap();
    assert_eq!(
        fx.board.start(&t).await,
        Err(BoardError::NotAllowed("no such role in the config"))
    );
    assert_eq!(fx.board.task(&t).await.unwrap().state, TaskState::Todo);
    assert_eq!(
        fx.board.start("t-nope").await,
        Err(BoardError::NotFound("task"))
    );
}

// ---- 9. review notice ---------------------------------------------------

#[tokio::test]
async fn a_successful_run_sends_the_review_notice() {
    use crate::team_mission_driver::tests::RecordingNotifyBackend;
    let backend = RecordingNotifyBackend::new();
    let mut dispatcher = NotifyDispatcher::new();
    dispatcher.register("ops", backend.clone());
    let fx = fixture_with(Setup {
        agent: Arc::new(ScriptedAgent::new(Reply::Done("done".into()))),
        team: None,
        notify: Some(Arc::new(dispatcher)),
    })
    .await;
    let t = seed(&fx, writer()).await;
    fx.board.start(&t).await.unwrap();
    wait_not_running(&fx, &t).await;
    let deadline = tokio::time::Instant::now() + WAIT;
    while backend.calls.lock().unwrap().is_empty() {
        assert!(tokio::time::Instant::now() < deadline, "no notice");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        backend.calls.lock().unwrap().clone(),
        [(
            "Task 'Draft' is ready for review".to_string(),
            Some(format!("board: {t}"))
        )]
    );
}

#[tokio::test]
async fn a_failed_run_sends_no_notice() {
    use crate::team_mission_driver::tests::RecordingNotifyBackend;
    let backend = RecordingNotifyBackend::new();
    let mut dispatcher = NotifyDispatcher::new();
    dispatcher.register("ops", backend.clone());
    let fx = fixture_with(Setup {
        agent: Arc::new(ScriptedAgent::new(Reply::Fail("no".into()))),
        team: None,
        notify: Some(Arc::new(dispatcher)),
    })
    .await;
    let t = seed(&fx, writer()).await;
    fx.board.start(&t).await.unwrap();
    wait_not_running(&fx, &t).await;
    assert!(backend.calls.lock().unwrap().is_empty());
}

// ---- 10. team owner with no team ----------------------------------------

#[tokio::test]
async fn a_team_task_needs_a_team() {
    let fx = fixture(Arc::new(ScriptedAgent::new(Reply::Done("done".into())))).await;
    let t = seed(&fx, Some(Owner::Team)).await;
    assert_eq!(
        fx.board.start(&t).await,
        Err(BoardError::NotAllowed("no team is set up"))
    );
    assert_eq!(fx.board.task(&t).await.unwrap().state, TaskState::Todo);
}

// ---- model: finish never moves a task that isn't Running -----------------

#[test]
fn finish_on_a_settled_task_is_bad_state_and_changes_nothing() {
    for state in [TaskState::Cancelled, TaskState::Done, TaskState::Review] {
        let original = Task {
            id: "t-1".into(),
            project_id: "p-1".into(),
            parent_task: None,
            title: "Draft".into(),
            description: String::new(),
            owner: writer(),
            review_required: true,
            state,
            thread: vec![],
            active_run: None,
            created_at: 1,
            updated_at: 1,
        };
        let mut t = original.clone();
        assert_eq!(
            model::finish(&mut t, RunOutcome::Succeeded, "x".into(), 5, None, 9),
            Err(BoardError::BadState {
                from: state,
                op: "finish"
            })
        );
        assert_eq!(t, original);
    }
}

// ---- team runs ----------------------------------------------------------

mod team {
    use super::*;
    use crate::team_mission_driver::SharedMissionState;
    use crate::team_mission_driver::tests::{deps, team_domain};
    use aivyx_core::GatePolicy;
    use aivyx_llm::{
        LlmError, LlmProvider, LlmRequest, LlmStepEnd, LlmStream, LlmStreamEvent, LlmUsage,
    };

    fn service(
        d: crate::team_mission_driver::TeamRunDeps,
        domain: DomainHandle,
    ) -> TeamMissionService {
        TeamMissionService::new(
            SharedMissionState::new(domain),
            d,
            aivyx_team::default_nonagon(),
            GatePolicy::Interactive,
        )
    }

    async fn wait_phase(svc: &TeamMissionService, id: &str, want: TeamMissionPhase) {
        let deadline = tokio::time::Instant::now() + WAIT;
        loop {
            let phase = svc.snapshot(id).map(|r| r.phase);
            if phase == Some(want) {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "mission {id} is {phase:?}, not {want:?}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    #[tokio::test]
    async fn a_team_run_finishes_with_the_last_steps_output() {
        let p = provider(ONE_STEP, false);
        let planner_prompt = Arc::clone(&p.planner_prompt);
        let mut d = deps("unused");
        d.provider = Arc::new(p);
        let svc = service(d, team_domain().await);
        let fx = fixture_with(Setup {
            agent: Arc::new(ScriptedAgent::new(Reply::Done("unused".into()))),
            team: Some(svc.clone()),
            notify: None,
        })
        .await;
        let t = seed(&fx, Some(Owner::Team)).await;
        let mid = fx.board.start(&t).await.unwrap();
        let record = svc.snapshot(&mid).expect("registered");
        assert_eq!(record.triggered_by, Some(format!("board:{t}")));
        // Planned unattended: the planner was told to plan no gates.
        let prompt = planner_prompt.lock().unwrap().clone().expect("planned");
        assert!(prompt.contains("runs UNATTENDED"), "{prompt}");
        assert!(!prompt.contains("Use a \"human\" gate"));
        assert!(record.goal.contains("Draft") && record.goal.contains("Q4 report"));

        let task = wait_not_running(&fx, &t).await;
        assert_eq!(task.state, TaskState::Review);
        let (run_mid, outcome, answer, _) = last_run(&task);
        assert_eq!(run_mid, mid);
        assert_eq!(outcome, RunOutcome::Succeeded);
        let record = svc.snapshot(&mid).unwrap();
        assert_eq!(record.phase, TeamMissionPhase::Done);
        let last = record.plan.steps.last().unwrap().id.clone();
        assert_eq!(answer, record.outputs[&last]);
        assert_eq!(answer, "step output 1", "the scripted step output");
        assert_eq!(
            op_names(&fx.audit)[3..],
            ["started".to_string(), "finished".to_string()]
        );
        assert!(fx.board.inner.cancels.lock().unwrap().is_empty());
    }

    /// Call 0 returns `plan`; call `n` > 0 answers `step output <n>`, and
    /// with `hold` call 1 (the first step) waits until released.
    struct ScriptedProvider {
        plan: &'static str,
        hold: bool,
        calls: std::sync::atomic::AtomicUsize,
        started: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
        /// The planner call's system prompt.
        planner_prompt: Arc<StdMutex<Option<String>>>,
        /// Every call after the plan fails.
        fail_steps: bool,
    }

    fn provider(plan: &'static str, hold: bool) -> ScriptedProvider {
        ScriptedProvider {
            plan,
            hold,
            calls: std::sync::atomic::AtomicUsize::new(0),
            started: Arc::new(tokio::sync::Notify::new()),
            release: Arc::new(tokio::sync::Notify::new()),
            planner_prompt: Arc::new(StdMutex::new(None)),
            fail_steps: false,
        }
    }

    const ONE_STEP: &str =
        r#"{"goal":"g","steps":[{"id":"a","specialist":"writer","prompt":"draft"}]}"#;

    const TWO_STEPS: &str = r#"{"goal":"g","steps":[{"id":"a","specialist":"writer","prompt":"draft"},{"id":"b","specialist":"writer","prompt":"polish","deps":["a"]}]}"#;

    #[async_trait]
    impl LlmProvider for ScriptedProvider {
        async fn chat_stream(
            &self,
            req: LlmRequest<'_>,
            _cancel: &CancellationToken,
        ) -> Result<Box<dyn LlmStream>, LlmError> {
            let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n == 0 {
                *self.planner_prompt.lock().unwrap() = req.system.map(str::to_string);
                return Ok(Box::new(OneShot(Some(self.plan.to_string()))));
            }
            if self.fail_steps {
                return Err(LlmError::Transport("scripted step failure".into()));
            }
            if n == 1 && self.hold {
                self.started.notify_one();
                self.release.notified().await;
            }
            let text = format!("step output {n}");
            Ok(Box::new(OneShot(Some(text))))
        }
    }

    struct OneShot(Option<String>);

    #[async_trait]
    impl LlmStream for OneShot {
        async fn next_event(&mut self) -> Result<Option<LlmStreamEvent>, LlmError> {
            Ok(None)
        }
        async fn finish(mut self: Box<Self>) -> Result<LlmStepEnd, LlmError> {
            Ok(LlmStepEnd::FinalMessage {
                text: self.0.take().unwrap_or_default(),
                usage: LlmUsage::default(),
            })
        }
    }

    #[tokio::test]
    async fn cancelling_a_team_task_aborts_its_mission() {
        let p = provider(TWO_STEPS, true);
        let (started, release) = (Arc::clone(&p.started), Arc::clone(&p.release));
        let mut d = deps("unused");
        d.provider = Arc::new(p);
        let svc = service(d, team_domain().await);
        let fx = fixture_with(Setup {
            agent: Arc::new(ScriptedAgent::new(Reply::Done("unused".into()))),
            team: Some(svc.clone()),
            notify: None,
        })
        .await;
        let t = seed(&fx, Some(Owner::Team)).await;
        let mid = fx.board.start(&t).await.unwrap();
        tokio::time::timeout(WAIT, started.notified())
            .await
            .expect("step a started");
        wait_phase(&svc, &mid, TeamMissionPhase::Executing).await;

        fx.board.cancel(&t).await.unwrap();
        assert_eq!(fx.board.task(&t).await.unwrap().state, TaskState::Cancelled);
        assert!(fx.board.inner.cancels.lock().unwrap().is_empty());
        // The poller wakes on the fired token and requests the abort (a
        // synchronous call) once this single-threaded test yields; then let
        // `a` end.
        for _ in 0..10 {
            tokio::task::yield_now().await;
        }
        release.notify_one();

        wait_phase(&svc, &mid, TeamMissionPhase::Halted).await;
        let record = svc.snapshot(&mid).unwrap();
        assert!(
            !record.outputs.contains_key("b"),
            "step b never ran: {:?}",
            record.outputs
        );
        let task = fx.board.task(&t).await.unwrap();
        assert_eq!(task.state, TaskState::Cancelled);
        assert_eq!(
            op_names(&fx.audit)[3..],
            ["started".to_string(), "cancelled".to_string()]
        );
    }

    /// A specialist that can't finish its turn stops the drive with an
    /// error. The mission must not stay Executing (nothing would ever end
    /// it), and the task must leave Running with the reason.
    #[tokio::test]
    async fn a_team_run_whose_drive_fails_goes_back_to_todo() {
        let mut p = provider(ONE_STEP, false);
        p.fail_steps = true;
        let mut d = deps("unused");
        d.provider = Arc::new(p);
        let svc = service(d, team_domain().await);
        let fx = fixture_with(Setup {
            agent: Arc::new(ScriptedAgent::new(Reply::Done("unused".into()))),
            team: Some(svc.clone()),
            notify: None,
        })
        .await;
        let t = seed(&fx, Some(Owner::Team)).await;
        let mid = fx.board.start(&t).await.unwrap();
        wait_phase(&svc, &mid, TeamMissionPhase::Halted).await;
        let record = svc.snapshot(&mid).unwrap();
        let reason = record.halt_reason.clone().unwrap_or_default();
        assert!(reason.contains("writer"), "{reason}");

        let task = wait_not_running(&fx, &t).await;
        assert_eq!(task.state, TaskState::Todo);
        let (run_mid, outcome, _, _) = last_run(&task);
        assert_eq!(run_mid, mid);
        match outcome {
            RunOutcome::Failed { reason: r } => assert_eq!(r, reason),
            other => panic!("{other:?}"),
        }
        assert!(fx.board.inner.cancels.lock().unwrap().is_empty());
    }

    /// A planner that ignores the instruction and plans a human gate anyway.
    const GATED: &str = r#"{"goal":"g","steps":[{"id":"a","specialist":"writer","prompt":"draft"},{"id":"ok","reviewer":"reviewer","criteria":"fine?","mode":"human","deps":["a"]},{"id":"b","specialist":"writer","prompt":"send","deps":["ok"]}]}"#;

    #[tokio::test]
    async fn cancelling_a_team_task_at_a_gate_rejects_the_gate() {
        let mut d = deps("unused");
        d.provider = Arc::new(provider(GATED, false));
        let svc = service(d, team_domain().await);
        let fx = fixture_with(Setup {
            agent: Arc::new(ScriptedAgent::new(Reply::Done("unused".into()))),
            team: Some(svc.clone()),
            notify: None,
        })
        .await;
        let t = seed(&fx, Some(Owner::Team)).await;
        let mid = fx.board.start(&t).await.unwrap();
        wait_phase(&svc, &mid, TeamMissionPhase::AwaitingApproval).await;
        assert_eq!(fx.board.task(&t).await.unwrap().state, TaskState::Running);

        fx.board.cancel(&t).await.unwrap();
        let deadline = tokio::time::Instant::now() + WAIT;
        let phase = loop {
            let phase = svc.snapshot(&mid).unwrap().phase;
            if phase.is_terminal() {
                break phase;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "mission still {phase:?}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        };
        assert!(
            matches!(phase, TeamMissionPhase::Rejected | TeamMissionPhase::Halted),
            "{phase:?}"
        );
        assert!(!svc.snapshot(&mid).unwrap().outputs.contains_key("b"));
        let task = fx.board.task(&t).await.unwrap();
        assert_eq!(task.state, TaskState::Cancelled);
        assert_eq!(
            op_names(&fx.audit)[3..],
            ["started".to_string(), "cancelled".to_string()]
        );
    }
}
