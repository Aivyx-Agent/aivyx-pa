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
}
