//! The Command Center briefing over the real daemon IPC server.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use aivyx_capability::CapabilitySet;
use aivyx_channel::LocalChannel;
use aivyx_channel::activity::Activity;
use aivyx_channel::daemon_ipc::{
    DaemonEnvelope, FrameError, FrontendMessage, QueryPayload, QueryResponsePayload, decode_frame,
    encode_frame,
};
use aivyx_channel::daemon_server::{ChannelFactory, DaemonConfig, run_daemon, run_daemon_compat};
use aivyx_core::{Agent, AgentId, CancellationToken, ChannelContext, Message, TurnOutcome};

struct QuietAgent {
    id: AgentId,
    caps: CapabilitySet,
}

#[async_trait]
impl Agent for QuietAgent {
    fn id(&self) -> AgentId {
        self.id
    }
    fn capabilities(&self) -> &CapabilitySet {
        &self.caps
    }
    async fn turn(&self, _m: Message, _c: &dyn ChannelContext) -> TurnOutcome {
        TurnOutcome::Completed {
            final_message: "ok".into(),
            tool_calls_made: 0,
            duration: Duration::from_millis(1),
        }
    }
}

async fn send(stream: &mut UnixStream, msg: &FrontendMessage) {
    stream.write_all(&encode_frame(msg).unwrap()).await.unwrap();
}

async fn query(
    stream: &mut UnixStream,
    buf: &mut Vec<u8>,
    id: &str,
    payload: QueryPayload,
) -> QueryResponsePayload {
    let msg = FrontendMessage::Query {
        id: id.into(),
        payload,
    };
    stream
        .write_all(&encode_frame(&msg).unwrap())
        .await
        .unwrap();
    loop {
        match decode_frame::<DaemonEnvelope>(buf) {
            Ok((env, n)) => {
                buf.drain(..n);
                if let DaemonEnvelope::QueryResponse { id: got, payload } = env
                    && got == id
                {
                    return payload;
                }
            }
            Err(FrameError::IncompleteBuf) => {
                let mut tmp = [0u8; 4096];
                let n = stream.read(&mut tmp).await.unwrap();
                assert!(n > 0, "daemon closed the connection");
                buf.extend_from_slice(&tmp[..n]);
            }
            Err(e) => panic!("decode: {e}"),
        }
    }
}

#[tokio::test]
async fn get_briefing_answers_over_ipc() {
    let dir: PathBuf =
        std::env::temp_dir().join(format!("aivyx-briefing-e2e-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("daemon.sock");
    let shutdown = CancellationToken::new();
    let agent: Arc<dyn Agent> = Arc::new(QuietAgent {
        id: AgentId::new(),
        caps: CapabilitySet::empty(),
    });
    let channel = Arc::new(LocalChannel::new("briefing-e2e", Vec::<u8>::new()));
    let (s, sd) = (socket.clone(), shutdown.clone());
    tokio::spawn(async move {
        let _ = run_daemon_compat(&s, agent, channel, sd).await;
    });
    for _ in 0..100 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let mut stream = UnixStream::connect(&socket).await.unwrap();
    let mut buf = Vec::new();

    let QueryResponsePayload::Briefing { briefing } =
        query(&mut stream, &mut buf, "b1", QueryPayload::GetBriefing).await
    else {
        panic!("expected a Briefing");
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    assert_eq!(briefing.last_active_unix, None);
    assert!((briefing.window_start_unix - (now - 24 * 3600)).abs() < 60);
    assert!(!briefing.window_capped);

    // No reminder store in the compat daemon → a clean "not ok", not an error.
    let resp = query(
        &mut stream,
        &mut buf,
        "r1",
        QueryPayload::CompleteReminder { id: "x".into() },
    )
    .await;
    assert_eq!(
        resp,
        QueryResponsePayload::ReminderUpdated {
            id: "x".into(),
            ok: false,
            due_unix: None
        }
    );

    shutdown.cancel();
    let _ = std::fs::remove_dir_all(&dir);
}

/// The persisted operator-activity record, read straight from the store the
/// daemon writes it to.
async fn persisted(store: &aivyx_storage::DomainHandle) -> Option<Activity> {
    store
        .get(b"operator.activity")
        .await
        .unwrap()
        .map(|b| serde_json::from_slice(&b).unwrap())
}

/// The connection's reader task stamps operator activity for what the
/// operator *does* (a reminder action) — not for looking (`GetBriefing`) or
/// for an automated, headless submit.
#[tokio::test]
async fn operator_actions_are_stamped_over_ipc_but_looking_is_not() {
    use aivyx_crypto::MasterKey;
    use aivyx_storage::{KeyDomain, RedbStorage, Storage, StorageConfig};

    let dir: PathBuf =
        std::env::temp_dir().join(format!("aivyx-briefing-e2e-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let storage: Arc<dyn Storage> = RedbStorage::open(
        StorageConfig::new(dir.join("store.redb")),
        MasterKey::from_raw([9u8; 32]),
    )
    .await
    .unwrap();
    let activity_store = storage.domain(KeyDomain::ChannelState);

    let socket = dir.join("daemon.sock");
    let shutdown = CancellationToken::new();
    let agent: Arc<dyn Agent> = Arc::new(QuietAgent {
        id: AgentId::new(),
        caps: CapabilitySet::empty(),
    });
    let channel: Arc<dyn ChannelContext + Send + Sync> =
        Arc::new(LocalChannel::new("briefing-e2e", Vec::<u8>::new()));
    let factory: ChannelFactory = Arc::new(move |_| Arc::clone(&channel));
    let config = DaemonConfig {
        socket_path: socket.clone(),
        agent,
        channel_factory: factory,
        shutdown: shutdown.clone(),
        mission_store: None,
        notify_dispatcher: None,
        default_notify_target: None,
        notify_targets: Vec::new(),
        schedule_store: None,
        webhook_store: None,
        file_watch_store: None,
        webhook_port: None,
        web_ui_port: None,
        web_ui_host: None,
        web_ui_allowed_origins: Vec::new(),
        web_ui_auth_token: None,
        comfyui_base_url: None,
        memory: None,
        memory_ttl_secs: None,
        audit_log: None,
        profile: Arc::new(aivyx_config::Profile::default()),
        persona_log: None,
        shared_persona: aivyx_channel::persona::shared_effective_persona(
            aivyx_channel::persona::EffectivePersona::default(),
        ),
        web_ui_broadcaster: None,
        persona_proposal_log: None,
        reflection_schedules: Vec::new(),
        target_policies: std::collections::HashMap::new(),
        embedding_provider: None,
        recall_log: None,
        helpfulness_ledger: None,
        cooccurrence_ledger: None,
        correction_ledger: None,
        persona_selection_stat: None,
        recall_cluster_stat: None,
        proactive_config: None,
        proactive_log: None,
        proactive_stat: None,
        persona_lifecycle_config: None,
        persona_lifecycle_stat: None,
        memory_retention: Vec::new(),
        conversation_windows: None,
        persona_consolidation_config: None,
        persona_consolidation_stat: None,
        persona_consolidation_phraser: None,
        correction_consolidation_config: None,
        correction_consolidation_stat: None,
        correction_consolidation_phraser: None,
        loop_backlog: None,
        loop_state: None,
        loop_config: None,
        team_missions: None,
        step_parker: None,
        gate_policy: aivyx_core::GatePolicy::default(),
        workspace_journaling_interval: None,
        pricing: Default::default(),
        config_toml_path: None,
        role_override: None,
        team_config_write_path: None,
        seed_draft_llm: None,
        document_roots: Default::default(),
        reminder_store: None,
        activity_store: Some(activity_store.clone()),
        routing_guard: None,
        routed: None,
        escalation_arming: None,
        loop_escalate_on_failure: false,
        wiki_sweep: None,
        wiki_store: None,
        graph_sweep: None,
        graph_store: None,
        conflict_dismissals: None,
        recall_judgment_config: None,
        recall_judgment_stat: None,
        recall_judge: None,
        correction_judgment_config: None,
        correction_judge: None,
        correction_judgment_stat: None,
        correction_signal_config: None,
        recall_feedback_config: None,
        tool_descriptors: Vec::new(),
        skill_auto_proposer: None,
        tool_relevance_ledger: None,
        skill_effectiveness_ledger: None,
        skill_refinement_config: None,
        skill_refinement_drafter: None,
        skill_authoring_config: None,
        skill_authoring_drafter: None,
    };
    tokio::spawn(async move {
        let _ = run_daemon(config).await;
    });
    for _ in 0..100 {
        if socket.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let mut stream = UnixStream::connect(&socket).await.unwrap();
    let mut buf = Vec::new();

    // Looking, and an automated submit, leave no trace. (Frames are read in
    // order, so once the briefing answers the submit has been seen too.)
    send(
        &mut stream,
        &FrontendMessage::SubmitInput {
            session_id: "s".into(),
            text: "scheduled".into(),
            mission_id: None,
            attachments: Vec::new(),
            headless: true,
        },
    )
    .await;
    let QueryResponsePayload::Briefing { .. } =
        query(&mut stream, &mut buf, "b1", QueryPayload::GetBriefing).await
    else {
        panic!("expected a Briefing");
    };
    assert_eq!(persisted(&activity_store).await, None);

    // Acting does: the reader task stamps it before the action is answered.
    let before = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let _ = query(
        &mut stream,
        &mut buf,
        "r1",
        QueryPayload::CompleteReminder { id: "x".into() },
    )
    .await;
    let stamped = persisted(&activity_store)
        .await
        .expect("an operator action is persisted");
    let at = stamped.last_action.expect("last_action is set");
    assert!(
        (before..before + 60).contains(&at),
        "stamped at {at}, expected ~{before}"
    );
    assert_eq!(stamped.anchor, None);

    shutdown.cancel();
    let _ = std::fs::remove_dir_all(&dir);
}
