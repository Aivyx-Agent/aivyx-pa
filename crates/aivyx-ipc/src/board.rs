//! A20 — the work board's wire types, shared by the daemon, the CLI and the
//! Studio (wasm-clean: serde only). The store, state machine and runs live
//! in `aivyx-channel::board`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Goal {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub goal_id: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub created_at: u64,
    pub updated_at: u64,
}

/// Who works a task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Owner {
    /// One bounded turn capped to this role's scopes.
    Role { name: String },
    /// The daemon's active team (a team mission).
    Team,
    /// A standing agent (A21); runs as the agent's works_as.
    Agent { id: String },
    /// `aivyx-coder` run headless in a registered workspace (A22); `workspace`
    /// is the workspace id.
    Coder { workspace: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Todo,
    Running,
    Review,
    Done,
    Cancelled,
}

impl TaskState {
    pub fn is_terminal(self) -> bool {
        matches!(self, TaskState::Done | TaskState::Cancelled)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunKind {
    Role,
    Team,
    Coder,
}

/// The run a Running task is waiting on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRef {
    pub mission_id: String,
    pub kind: RunKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Author {
    Operator,
    Agent { mission_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunOutcome {
    Succeeded,
    Failed { reason: String },
    Cancelled,
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EntryKind {
    Comment {
        text: String,
    },
    StateChange {
        from: TaskState,
        to: TaskState,
    },
    Run {
        mission_id: String,
        outcome: RunOutcome,
        #[serde(default)]
        answer: String,
        #[serde(default)]
        tokens: u64,
        #[serde(default)]
        cost_usd: Option<f64>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ThreadEntry {
    pub at: u64,
    pub author: Author,
    pub entry: EntryKind,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub project_id: String,
    /// Set for a subtask; the parent is in the same project.
    #[serde(default)]
    pub parent_task: Option<String>,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub owner: Option<Owner>,
    #[serde(default = "yes")]
    pub review_required: bool,
    pub state: TaskState,
    #[serde(default)]
    pub thread: Vec<ThreadEntry>,
    #[serde(default)]
    pub active_run: Option<RunRef>,
    pub created_at: u64,
    pub updated_at: u64,
}

/// What `BoardCreateTask` carries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NewTask {
    pub project_id: String,
    #[serde(default)]
    pub parent_task: Option<String>,
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub owner: Option<Owner>,
    #[serde(default = "yes")]
    pub review_required: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BoardSnapshot {
    pub goals: Vec<Goal>,
    pub projects: Vec<Project>,
    pub tasks: Vec<Task>,
    #[serde(default)]
    pub agents: Vec<Agent>,
    #[serde(default)]
    pub workspaces: Vec<Workspace>,
}

/// A registered folder `aivyx-coder` may work in (A22). The path is canonical
/// and absolute; only the operator registers one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    pub id: String,
    pub name: String,
    pub path: String,
    #[serde(default)]
    pub created_at: u64,
}

/// How a standing agent's tasks run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorksAs {
    Role { name: String },
    Team,
    /// Work as `aivyx-coder` in this workspace id (A22).
    Coder { workspace: String },
}

/// A standing agent on the board (A21).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Agent {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub reports_to: Option<String>,
    pub works_as: WorksAs,
    #[serde(default)]
    pub heartbeat: Option<String>,
    #[serde(default)]
    pub wake_on_assign: bool,
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub last_wake: Option<u64>,
    pub created_at: u64,
    pub updated_at: u64,
}

/// What creating or editing an agent carries. On an edit, `None` keeps the
/// current value; `clear_heartbeat` / `clear_reports_to` clear the field
/// (and win over a value given alongside them). aivyx-ipc has no
/// `serde_with`, so the clear is explicit rather than `Option<Option<_>>`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NewAgent {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub reports_to: Option<String>,
    #[serde(default)]
    pub clear_reports_to: bool,
    #[serde(default)]
    pub works_as: Option<WorksAs>,
    #[serde(default)]
    pub heartbeat: Option<String>,
    #[serde(default)]
    pub clear_heartbeat: bool,
    #[serde(default)]
    pub wake_on_assign: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_task_round_trips_and_old_rows_decode() {
        let t = Task {
            id: "t-1".into(),
            project_id: "p-1".into(),
            parent_task: None,
            title: "Draft".into(),
            description: String::new(),
            owner: Some(Owner::Role {
                name: "default".into(),
            }),
            review_required: true,
            state: TaskState::Todo,
            thread: vec![ThreadEntry {
                at: 1,
                author: Author::Operator,
                entry: EntryKind::Comment { text: "go".into() },
            }],
            active_run: None,
            created_at: 1,
            updated_at: 1,
        };
        let json = serde_json::to_string(&t).unwrap();
        assert_eq!(serde_json::from_str::<Task>(&json).unwrap(), t);
        let old = r#"{"id":"t-2","project_id":"p-1","title":"x","state":"todo","created_at":1,"updated_at":1}"#;
        let t: Task = serde_json::from_str(old).unwrap();
        assert!(t.review_required, "review defaults on");
        assert!(t.thread.is_empty() && t.owner.is_none());
    }

    #[test]
    fn coder_types_round_trip_and_old_rows_decode() {
        let o = Owner::Coder {
            workspace: "w-0000000a".into(),
        };
        let j = serde_json::to_string(&o).unwrap();
        assert_eq!(j, r#"{"kind":"coder","workspace":"w-0000000a"}"#);
        assert_eq!(serde_json::from_str::<Owner>(&j).unwrap(), o);
        let w = WorksAs::Coder {
            workspace: "w-1".into(),
        };
        let j = serde_json::to_string(&w).unwrap();
        assert_eq!(serde_json::from_str::<WorksAs>(&j).unwrap(), w);
        let snap: BoardSnapshot =
            serde_json::from_str(r#"{"goals":[],"projects":[],"tasks":[]}"#).unwrap();
        assert!(snap.workspaces.is_empty());
        let ws: Workspace =
            serde_json::from_str(r#"{"id":"w-1","name":"x","path":"/a"}"#).unwrap();
        assert_eq!(ws.created_at, 0);
        let old = r#"{"id":"t-2","project_id":"p-1","title":"x","state":"todo","owner":{"kind":"role","name":"dev"},"created_at":1,"updated_at":1}"#;
        let t: Task = serde_json::from_str(old).unwrap();
        assert_eq!(t.owner, Some(Owner::Role { name: "dev".into() }));
    }

    #[test]
    fn agent_types_round_trip_and_old_json_decodes() {
        let o = Owner::Agent { id: "a-1".into() };
        let j = serde_json::to_string(&o).unwrap();
        assert_eq!(serde_json::from_str::<Owner>(&j).unwrap(), o);
        let snap: BoardSnapshot =
            serde_json::from_str(r#"{"goals":[],"projects":[],"tasks":[]}"#).unwrap();
        assert!(snap.agents.is_empty());
        let a: Agent = serde_json::from_str(
            r#"{"id":"a-1","name":"x","works_as":{"kind":"team"},"created_at":1,"updated_at":1}"#,
        )
        .unwrap();
        assert_eq!(a.works_as, WorksAs::Team);
        assert!(a.heartbeat.is_none() && !a.paused && a.title.is_empty());
        let keep: NewAgent = serde_json::from_str("{}").unwrap();
        let clear: NewAgent = serde_json::from_str(r#"{"clear_heartbeat":true}"#).unwrap();
        assert!(keep.heartbeat.is_none() && !keep.clear_heartbeat);
        assert!(clear.heartbeat.is_none() && clear.clear_heartbeat);
        assert_ne!(keep, clear);
    }
}
