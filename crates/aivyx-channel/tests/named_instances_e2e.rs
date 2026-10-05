//! Named instances — two daemons for one OS user, side by side: each at the
//! socket its `InstancePaths` resolves, both answering at once, and
//! stopping one leaves the other running.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;

use aivyx_capability::CapabilitySet;
use aivyx_channel::LocalChannel;
use aivyx_channel::daemon_client::daemon_status;
use aivyx_channel::daemon_server::run_daemon_compat;
use aivyx_core::{Agent, AgentId, CancellationToken, ChannelContext, Message, TurnOutcome};
use aivyx_instance::{BaseDirs, InstanceName, InstancePaths};

struct IdleAgent {
    id: AgentId,
    caps: CapabilitySet,
}

#[async_trait]
impl Agent for IdleAgent {
    fn id(&self) -> AgentId {
        self.id
    }
    fn capabilities(&self) -> &CapabilitySet {
        &self.caps
    }
    async fn turn(&self, _message: Message, _channel: &dyn ChannelContext) -> TurnOutcome {
        TurnOutcome::Completed {
            final_message: "ok".into(),
            tool_calls_made: 0,
            duration: Duration::from_millis(1),
        }
    }
}

fn paths(name: &str, home: &Path) -> InstancePaths {
    InstancePaths::new(
        InstanceName::parse(name).unwrap(),
        BaseDirs {
            home: Some(home.to_path_buf()),
            xdg_config_home: None,
            xdg_data_home: None,
            xdg_runtime_dir: Some(home.join("run")),
        },
    )
}

fn start(socket: PathBuf, shutdown: CancellationToken) -> tokio::task::JoinHandle<()> {
    std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
    let agent: Arc<dyn Agent> = Arc::new(IdleAgent { id: AgentId::new(), caps: CapabilitySet::empty() });
    let channel = Arc::new(LocalChannel::new("instances-e2e", Vec::<u8>::new()));
    tokio::spawn(async move {
        run_daemon_compat(&socket, agent, channel, shutdown).await.expect("daemon must run");
    })
}

#[tokio::test]
async fn two_instances_run_side_by_side() {
    let tmp = tempfile::tempdir().unwrap();
    let default = paths("default", tmp.path());
    let research = paths("research", tmp.path());
    let default_socket = default.socket_path().unwrap();
    let research_socket = research.socket_path().unwrap();
    assert_ne!(default_socket, research_socket);
    assert!(research_socket.starts_with(tmp.path().join("run/aivyx-pa/instances/research")));

    let stop_default = CancellationToken::new();
    let stop_research = CancellationToken::new();
    let d = start(default_socket.clone(), stop_default.clone());
    let r = start(research_socket.clone(), stop_research.clone());
    tokio::time::sleep(Duration::from_millis(100)).await;

    assert!(daemon_status(&default_socket).await.running);
    assert!(daemon_status(&research_socket).await.running);

    stop_research.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(5), r).await;
    assert!(!daemon_status(&research_socket).await.running);
    assert!(daemon_status(&default_socket).await.running, "default must outlive research");

    stop_default.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(5), d).await;
}
