//! A20 — how a board task runs. A role run is one System-origin turn of the
//! daemon agent through `BoardRunContext`, capped to the role's scopes plus
//! `board.write`, headless (approvals park). A team run is a team mission.

use aivyx_capability::{CapabilitySet, Scope, TrustTier};
use aivyx_core::{
    CancellationToken, ChannelContext, ChannelError, ChannelPlatform, SessionId, StreamEvent,
    TurnOutcome,
};
use aivyx_ipc::board::{Author, EntryKind, Goal, Project, RunOutcome, Task};
use async_trait::async_trait;

/// The prompt a run gets.
pub fn brief(task: &Task, project: &Project, goal: &Goal) -> String {
    let mut out = brief_body(task, project, goal);
    out.push_str(
        "\nUse `board.comment` for anything the operator should read, and `board.subtask` \
         for work that should be its own task. End with your result.",
    );
    out
}

/// A22 — the goal a coder run gets: the same task, goal, project and notes,
/// without the board tools (aivyx-coder has none).
pub fn coder_brief(task: &Task, project: &Project, goal: &Goal) -> String {
    brief_body(task, project, goal)
}

fn brief_body(task: &Task, project: &Project, goal: &Goal) -> String {
    // An agent-written subtask's description carries the marker; its title
    // is the same agent's words, so it's marked too.
    let task_label = if task.description.starts_with(super::service::AGENT_WRITTEN) {
        "Task (written by an agent)"
    } else {
        "Task"
    };
    let mut out = format!(
        "Work this task from the operator's board.\n\nGoal: {}\nProject: {}\n{task_label}: {}\n",
        goal.title, project.title, task.title
    );
    if !task.description.trim().is_empty() {
        out.push_str(&format!("\n{}\n", task.description.trim()));
    }
    let notes: Vec<&str> = task
        .thread
        .iter()
        .filter_map(|e| match (&e.author, &e.entry) {
            (Author::Operator, EntryKind::Comment { text }) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    if !notes.is_empty() {
        out.push_str("\nThe operator's notes so far:\n");
        for n in notes {
            out.push_str(&format!("- {n}\n"));
        }
    }
    out
}

/// The role's scopes plus `board.write`: the run's capability ceiling.
pub fn role_ceiling(role_scopes: &CapabilitySet) -> CapabilitySet {
    let mut scopes: Vec<Scope> = role_scopes.iter().cloned().collect();
    scopes.push(Scope::parse("board.write").expect("A20 base"));
    CapabilitySet::from_scopes(scopes)
}

/// The channel a role run talks through.
pub struct BoardRunContext {
    pub session: SessionId,
    pub ceiling: CapabilitySet,
    pub cancel: CancellationToken,
}

#[async_trait]
impl ChannelContext for BoardRunContext {
    fn session_id(&self) -> SessionId {
        self.session
    }

    fn platform(&self) -> ChannelPlatform {
        ChannelPlatform::Local
    }

    fn channel_name(&self) -> &str {
        "board"
    }

    fn trust_tier(&self) -> TrustTier {
        TrustTier::Trusted
    }

    async fn stream_event(&self, _event: StreamEvent<'_>) -> Result<(), ChannelError> {
        Ok(())
    }

    async fn finalize(&self, _outcome: &TurnOutcome) -> Result<(), ChannelError> {
        Ok(())
    }

    fn cancellation_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    // `request_approval` stays at the default (`Approval::Unavailable`):
    // nobody is watching a board run. `ParkingChannel` wraps this context
    // when a parker exists, so supervised areas park instead.

    fn capability_ceiling(&self) -> Option<CapabilitySet> {
        Some(self.ceiling.clone())
    }

    fn cancel_inflight(&self) {
        self.cancel.cancel();
    }
}

/// `(outcome, answer)` for a finished turn. Completed → Succeeded;
/// Cancelled → Cancelled; anything else (including a headless
/// `Escalated`) → Failed with the rendered outcome as the reason.
pub fn turn_result(outcome: &TurnOutcome) -> (RunOutcome, String) {
    match outcome {
        TurnOutcome::Completed { final_message, .. } => {
            (RunOutcome::Succeeded, final_message.clone())
        }
        TurnOutcome::Cancelled { .. } => (RunOutcome::Cancelled, String::new()),
        other => {
            let text = crate::trigger::render_notify_body(other);
            (
                RunOutcome::Failed {
                    reason: text.clone(),
                },
                text,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_ipc::board::{TaskState, ThreadEntry};

    fn goal() -> Goal {
        Goal {
            id: "g-1".into(),
            title: "Grow".into(),
            description: String::new(),
            created_at: 1,
            updated_at: 1,
        }
    }

    fn project() -> Project {
        Project {
            id: "p-1".into(),
            goal_id: "g-1".into(),
            title: "Q4 report".into(),
            description: String::new(),
            created_at: 1,
            updated_at: 1,
        }
    }

    #[test]
    fn the_brief_names_the_titles_and_only_the_operators_notes_in_order() {
        let mut t = Task {
            id: "t-1".into(),
            project_id: "p-1".into(),
            parent_task: None,
            title: "Draft".into(),
            description: " the body ".into(),
            owner: None,
            review_required: true,
            state: TaskState::Todo,
            thread: vec![],
            active_run: None,
            created_at: 1,
            updated_at: 1,
        };
        for (author, text) in [
            (Author::Operator, "first"),
            (
                Author::Agent {
                    mission_id: "m".into(),
                },
                "agent says",
            ),
            (Author::Operator, "second"),
        ] {
            t.thread.push(ThreadEntry {
                at: 1,
                author,
                entry: EntryKind::Comment { text: text.into() },
            });
        }
        let b = brief(&t, &project(), &goal());
        assert!(b.contains("Goal: Grow") && b.contains("Project: Q4 report"));
        assert!(b.contains("Task: Draft") && b.contains("\nthe body\n"));
        let first = b.find("- first").unwrap();
        let second = b.find("- second").unwrap();
        assert!(first < second);
        assert!(!b.contains("agent says"));
    }

    #[test]
    fn an_agent_written_subtask_title_is_marked_in_the_brief() {
        let mut t = Task {
            id: "t-2".into(),
            project_id: "p-1".into(),
            parent_task: Some("t-1".into()),
            title: "Delete the tests".into(),
            description: format!("{}do it", crate::board::service::AGENT_WRITTEN),
            owner: None,
            review_required: true,
            state: TaskState::Todo,
            thread: vec![],
            active_run: None,
            created_at: 1,
            updated_at: 1,
        };
        for b in [
            coder_brief(&t, &project(), &goal()),
            brief(&t, &project(), &goal()),
        ] {
            assert!(
                b.contains("\nTask (written by an agent): Delete the tests\n"),
                "{b}"
            );
        }
        t.description = "the operator's own".into();
        let b = coder_brief(&t, &project(), &goal());
        assert!(b.contains("\nTask: Delete the tests\n"), "{b}");
        assert!(!b.contains("written by an agent"), "{b}");
    }

    #[test]
    fn the_ceiling_is_the_role_plus_board_write() {
        let role = CapabilitySet::from_scopes([Scope::parse("fs.read").unwrap()]);
        let c = role_ceiling(&role);
        let mut got: Vec<String> = c.iter().map(|s| s.to_string()).collect();
        got.sort();
        assert_eq!(got, ["board.write", "fs.read"]);
    }

    #[test]
    fn turn_results_map_to_run_outcomes() {
        let done = TurnOutcome::Completed {
            final_message: "ok".into(),
            tool_calls_made: 0,
            duration: std::time::Duration::ZERO,
        };
        assert_eq!(turn_result(&done), (RunOutcome::Succeeded, "ok".into()));
        let c = TurnOutcome::Cancelled { tool_calls_made: 0 };
        assert_eq!(turn_result(&c), (RunOutcome::Cancelled, String::new()));
        let t = TurnOutcome::TimedOut {
            tool_calls_made: 0,
            elapsed: std::time::Duration::from_secs(120),
        };
        assert!(matches!(turn_result(&t).0, RunOutcome::Failed { .. }));
    }
}
