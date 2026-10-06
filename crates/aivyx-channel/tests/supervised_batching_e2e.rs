//! Supervised batching, end to end: a routine whose `fs` area is
//! `supervised` reaches a real `fs.delete` (confirm-first on). The step is
//! parked, not taken — the file is still there — and the turn carries on.
//! Approving it through the daemon's query handler deletes the file; denying
//! it leaves the file alone.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::json;

use aivyx_capability::{CapabilitySet, Scope};
use aivyx_channel::LocalChannel;
use aivyx_channel::daemon_ipc::FrontendType;
use aivyx_channel::daemon_server::ChannelFactory;
use aivyx_channel::parked_steps::{StepParker, handle_parked_query};
use aivyx_channel::trigger::{TriggerDispatch, TriggerSource};
use aivyx_core::tools::FsDeleteToolConfig;
use aivyx_core::{
    Agent, AgentId, AreaFlags, ChannelContext, ConcreteAgent, NextStep, NullAuditHook, Tool,
    ToolRegistry, VecPlanner,
};
use aivyx_crypto::MasterKey;
use aivyx_ipc::parked::ParkedState;
use aivyx_ipc::protocol::{QueryPayload, QueryResponsePayload};
use aivyx_storage::{KeyDomain, RedbStorage, Storage, StorageConfig};

struct Scratch {
    dir: PathBuf,
}
impl Scratch {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("aivyx-batching-e2e-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("sandbox")).unwrap();
        Scratch { dir }
    }
    fn sandbox(&self) -> PathBuf {
        std::fs::canonicalize(self.dir.join("sandbox")).unwrap()
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct Setup {
    agent: Arc<dyn Agent>,
    factory: ChannelFactory,
    parker: Arc<StepParker>,
    file: PathBuf,
}

async fn setup(scratch: &Scratch) -> Setup {
    let sandbox = scratch.sandbox();
    let file = sandbox.join("old.txt");
    std::fs::write(&file, "stale notes").unwrap();

    let tool: Arc<dyn Tool> = Arc::new(
        FsDeleteToolConfig::new(sandbox.clone())
            .with_confirm_destructive(true)
            .build()
            .unwrap(),
    );
    let tool_id = tool.id();
    let caps = CapabilitySet::from_scopes([
        Scope::parse(&format!("fs.delete:{}/**", sandbox.display())).unwrap(),
    ]);
    let plan = vec![
        NextStep::ToolCall {
            tool_id,
            // The model even claims it's confirmed — that must not count.
            input: json!({"path": "old.txt", "confirmed": true}),
            auto_corrected_from: None,
            extracted_from_text: None,
        },
        NextStep::FinalMessage("tidied".to_string()),
    ];
    let agent: Arc<dyn Agent> = Arc::new(ConcreteAgent::new(
        AgentId::new(),
        caps,
        Arc::new(ToolRegistry::new(vec![tool])),
        Arc::new(NullAuditHook),
        move || Box::new(VecPlanner::new(plan.clone())),
    ));
    let factory: ChannelFactory = Arc::new(|_ft: FrontendType| {
        Arc::new(LocalChannel::new("routine", Vec::<u8>::new())) as Arc<dyn ChannelContext + Send + Sync>
    });
    let storage: Arc<dyn Storage> = RedbStorage::open(
        StorageConfig::new(scratch.dir.join("s.redb")),
        MasterKey::from_raw([9u8; 32]),
    )
    .await
    .unwrap();
    let mut supervised = AreaFlags::everywhere(false);
    supervised.areas.insert("fs".into(), true);
    let parker = Arc::new(
        StepParker::new(storage.domain(KeyDomain::ParkedSteps), supervised, 7).with_preview_root(sandbox),
    );
    Setup { agent, factory, parker, file }
}

async fn run_routine(s: &Setup) -> String {
    TriggerDispatch::new(Arc::clone(&s.agent), Arc::clone(&s.factory))
        .with_step_parker(Arc::clone(&s.parker))
        .fire(TriggerSource::Cron, "cfg-tidy", "tidy the sandbox", false, &[], aivyx_config::NotifyWhen::Always)
        .await;
    let steps = s.parker.list().await.unwrap();
    assert_eq!(steps.len(), 1, "one step parked");
    let step = &steps[0];
    assert_eq!(step.state, ParkedState::Pending);
    assert_eq!(step.tool, "fs.delete");
    assert_eq!(step.area, "fs");
    assert_eq!(step.origin, "routine tidy");
    assert_eq!(step.input, json!({"path": "old.txt"}), "no model-supplied `confirmed`");
    assert_eq!(step.preview.as_deref(), Some("Now: old.txt (11 bytes)\nstale notes"));
    assert!(s.file.exists(), "a parked step is not taken");
    step.id.clone()
}

async fn resolve(s: &Setup, id: &str, approve: bool) -> QueryResponsePayload {
    handle_parked_query(
        &QueryPayload::ResolveParkedStep { id: id.to_string(), approve },
        Some(&s.parker),
        &s.agent,
        &s.factory,
    )
    .await
    .expect("a parked-step query")
}

#[tokio::test]
async fn a_supervised_routine_parks_the_delete_and_approving_runs_it() {
    let scratch = Scratch::new();
    let s = setup(&scratch).await;
    let id = run_routine(&s).await;

    match resolve(&s, &id, true).await {
        QueryResponsePayload::ParkedStepResolved { step } => {
            assert_eq!(step.state, ParkedState::Approved, "{:?}", step.result);
        }
        other => panic!("expected ParkedStepResolved, got {other:?}"),
    }
    assert!(!s.file.exists(), "approving ran the delete");

    // Nothing runs twice.
    match resolve(&s, &id, true).await {
        QueryResponsePayload::QueryError { code, message } => {
            assert_eq!(code, "resolve_parked_step_failed");
            assert!(message.contains("already approved"), "{message}");
        }
        other => panic!("expected QueryError, got {other:?}"),
    }
}

#[tokio::test]
async fn denying_a_parked_delete_leaves_the_file() {
    let scratch = Scratch::new();
    let s = setup(&scratch).await;
    let id = run_routine(&s).await;
    match resolve(&s, &id, false).await {
        QueryResponsePayload::ParkedStepResolved { step } => assert_eq!(step.state, ParkedState::Denied),
        other => panic!("expected ParkedStepResolved, got {other:?}"),
    }
    assert!(s.file.exists());
}

#[tokio::test]
async fn without_a_parker_the_queries_say_nothing_is_parked() {
    let scratch = Scratch::new();
    let s = setup(&scratch).await;
    for q in [QueryPayload::GetParkedSteps, QueryPayload::ResolveParkedStep { id: "x".into(), approve: true }] {
        match handle_parked_query(&q, None, &s.agent, &s.factory).await {
            Some(QueryResponsePayload::QueryError { code, .. }) => assert_eq!(code, "parked_steps_unavailable"),
            other => panic!("expected QueryError, got {other:?}"),
        }
    }
    assert!(handle_parked_query(&QueryPayload::TeamMissionList, None, &s.agent, &s.factory).await.is_none());
}
