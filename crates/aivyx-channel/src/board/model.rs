//! The board's state machine (A20): pure functions over a `Task`. Every
//! allowed move appends its thread entries; every refused move leaves the
//! task untouched.

use aivyx_ipc::board::{
    Author, EntryKind, Owner, RunKind, RunOutcome, RunRef, Task, TaskState, ThreadEntry,
};

/// Who asks for a change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Actor {
    Operator,
    /// A role run of `task_id`, recorded under `mission_id`.
    Run {
        task_id: String,
        mission_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BoardError {
    #[error("no {0} with that id")]
    NotFound(&'static str),
    #[error("{0}")]
    NotAllowed(&'static str),
    #[error("can't {op} a task that is {from:?}")]
    BadState { from: TaskState, op: &'static str },
    #[error("{0} can't be empty")]
    Empty(&'static str),
    #[error("storage: {0}")]
    Storage(String),
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// `g-1a2b3c4d`-style id.
pub fn new_id(prefix: char) -> String {
    let u = uuid::Uuid::new_v4().simple().to_string();
    format!("{prefix}-{}", &u[..8])
}

fn push(task: &mut Task, now: u64, author: Author, entry: EntryKind) {
    task.thread.push(ThreadEntry {
        at: now,
        author,
        entry,
    });
    task.updated_at = now;
}

fn move_to(task: &mut Task, now: u64, author: Author, to: TaskState) {
    let from = task.state;
    task.state = to;
    push(task, now, author, EntryKind::StateChange { from, to });
}

pub fn assign(task: &mut Task, owner: Option<Owner>, now: u64) -> Result<(), BoardError> {
    if task.state == TaskState::Running || task.state.is_terminal() {
        return Err(BoardError::BadState {
            from: task.state,
            op: "reassign",
        });
    }
    task.owner = owner;
    task.updated_at = now;
    Ok(())
}

pub fn start(task: &mut Task, run: RunRef, now: u64) -> Result<(), BoardError> {
    start_by(task, run, now, false)
}

/// [`start`], with the `todo → running` entry written by the run itself
/// when `by_agent` (a start from an agent's wake) rather than the operator.
pub fn start_by(
    task: &mut Task,
    run: RunRef,
    now: u64,
    by_agent: bool,
) -> Result<(), BoardError> {
    if task.state != TaskState::Todo {
        return Err(BoardError::BadState {
            from: task.state,
            op: "start",
        });
    }
    if task.owner.is_none() {
        return Err(BoardError::NotAllowed(
            "assign an owner before starting the task",
        ));
    }
    let author = if by_agent {
        Author::Agent {
            mission_id: run.mission_id.clone(),
        }
    } else {
        Author::Operator
    };
    task.active_run = Some(run);
    move_to(task, now, author, TaskState::Running);
    Ok(())
}

/// A run ended. `Succeeded` → Review (or Done with review off);
/// `Cancelled` → Cancelled (a coder run that stopped on a signal, exit 130;
/// a cancel from the operator has already moved the task); anything else →
/// Todo.
pub fn finish(
    task: &mut Task,
    outcome: RunOutcome,
    answer: String,
    tokens: u64,
    cost_usd: Option<f64>,
    now: u64,
) -> Result<(), BoardError> {
    if task.state != TaskState::Running {
        return Err(BoardError::BadState {
            from: task.state,
            op: "finish",
        });
    }
    let run = task
        .active_run
        .take()
        .ok_or(BoardError::NotAllowed("the task has no active run"))?;
    let author = Author::Agent {
        mission_id: run.mission_id.clone(),
    };
    let to = match outcome {
        RunOutcome::Succeeded if task.review_required => TaskState::Review,
        RunOutcome::Succeeded => TaskState::Done,
        RunOutcome::Cancelled => TaskState::Cancelled,
        _ => TaskState::Todo,
    };
    push(
        task,
        now,
        author.clone(),
        EntryKind::Run {
            mission_id: run.mission_id,
            outcome,
            answer,
            tokens,
            cost_usd,
        },
    );
    move_to(task, now, author, to);
    Ok(())
}

pub fn approve(task: &mut Task, now: u64) -> Result<(), BoardError> {
    if task.state != TaskState::Review {
        return Err(BoardError::BadState {
            from: task.state,
            op: "approve",
        });
    }
    move_to(task, now, Author::Operator, TaskState::Done);
    Ok(())
}

pub fn send_back(task: &mut Task, why: &str, now: u64) -> Result<(), BoardError> {
    if task.state != TaskState::Review {
        return Err(BoardError::BadState {
            from: task.state,
            op: "send back",
        });
    }
    if why.trim().is_empty() {
        return Err(BoardError::Empty("the reason"));
    }
    push(
        task,
        now,
        Author::Operator,
        EntryKind::Comment {
            text: why.trim().to_string(),
        },
    );
    move_to(task, now, Author::Operator, TaskState::Todo);
    Ok(())
}

/// Returns the run to stop, if one was active.
pub fn cancel(task: &mut Task, now: u64) -> Result<Option<RunRef>, BoardError> {
    if task.state.is_terminal() {
        return Err(BoardError::BadState {
            from: task.state,
            op: "cancel",
        });
    }
    let run = task.active_run.take();
    move_to(task, now, Author::Operator, TaskState::Cancelled);
    Ok(run)
}

pub fn comment(task: &mut Task, actor: &Actor, text: &str, now: u64) -> Result<(), BoardError> {
    if text.trim().is_empty() {
        return Err(BoardError::Empty("the comment"));
    }
    let author = match actor {
        Actor::Operator => Author::Operator,
        Actor::Run {
            task_id,
            mission_id,
        } => {
            run_may_touch(task, task_id)?;
            Author::Agent {
                mission_id: mission_id.clone(),
            }
        }
    };
    push(
        task,
        now,
        author,
        EntryKind::Comment {
            text: text.trim().to_string(),
        },
    );
    Ok(())
}

/// May `actor` add a subtask under `parent`?
pub fn add_subtask_check(parent: &Task, actor: &Actor) -> Result<(), BoardError> {
    if parent.state.is_terminal() {
        return Err(BoardError::BadState {
            from: parent.state,
            op: "add a subtask to",
        });
    }
    if let Actor::Run { task_id, .. } = actor {
        run_may_touch(parent, task_id)?;
    }
    Ok(())
}

fn run_may_touch(task: &Task, run_task_id: &str) -> Result<(), BoardError> {
    if task.id != run_task_id {
        return Err(BoardError::NotAllowed("a run may only touch its own task"));
    }
    if task.state != TaskState::Running {
        return Err(BoardError::NotAllowed("the task isn't running"));
    }
    Ok(())
}

/// Restart sweep: a Running task goes back to Todo.
pub fn interrupt(task: &mut Task, now: u64) -> Option<RunRef> {
    if task.state != TaskState::Running {
        return None;
    }
    let run = task.active_run.take();
    let mission_id = run
        .as_ref()
        .map(|r| r.mission_id.clone())
        .unwrap_or_default();
    push(
        task,
        now,
        Author::Operator,
        EntryKind::Run {
            mission_id,
            outcome: RunOutcome::Interrupted,
            answer: "interrupted by a daemon restart".into(),
            tokens: 0,
            cost_usd: None,
        },
    );
    move_to(task, now, Author::Operator, TaskState::Todo);
    run
}

/// The run kind an owner needs. An agent has none of its own: it runs as its
/// `works_as`, which the service resolves first.
pub fn run_kind(owner: &Owner) -> Option<RunKind> {
    match owner {
        Owner::Role { .. } => Some(RunKind::Role),
        Owner::Team => Some(RunKind::Team),
        Owner::Coder { .. } => Some(RunKind::Coder),
        Owner::Agent { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn todo_task() -> Task {
        Task {
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
            thread: vec![],
            active_run: None,
            created_at: 1,
            updated_at: 1,
        }
    }

    fn run_ref() -> RunRef {
        RunRef {
            mission_id: "m-1".into(),
            kind: RunKind::Role,
        }
    }

    fn running_task() -> Task {
        let mut t = todo_task();
        start(&mut t, run_ref(), 2).unwrap();
        t
    }

    fn last_change(t: &Task) -> (TaskState, TaskState) {
        match &t.thread.last().unwrap().entry {
            EntryKind::StateChange { from, to } => (*from, *to),
            other => panic!("expected StateChange, got {other:?}"),
        }
    }

    #[test]
    fn a_start_from_a_wake_is_written_by_the_agent() {
        let mut t = todo_task();
        start(&mut t, run_ref(), 2).unwrap();
        assert_eq!(t.thread.last().unwrap().author, Author::Operator);

        let mut t = todo_task();
        start_by(&mut t, run_ref(), 2, true).unwrap();
        assert_eq!(
            t.thread.last().unwrap().author,
            Author::Agent {
                mission_id: "m-1".into()
            }
        );
        assert_eq!(last_change(&t), (TaskState::Todo, TaskState::Running));
    }

    #[test]
    fn start_needs_an_owner_and_works_only_from_todo() {
        let mut t = todo_task();
        t.owner = None;
        assert_eq!(
            start(&mut t, run_ref(), 2),
            Err(BoardError::NotAllowed(
                "assign an owner before starting the task"
            ))
        );
        assert_eq!(t.state, TaskState::Todo);
        assert!(t.thread.is_empty() && t.active_run.is_none());

        let mut t = todo_task();
        start(&mut t, run_ref(), 2).unwrap();
        assert_eq!(t.state, TaskState::Running);
        assert_eq!(t.active_run, Some(run_ref()));
        assert_eq!(t.updated_at, 2);
        assert_eq!(last_change(&t), (TaskState::Todo, TaskState::Running));

        for s in [
            TaskState::Running,
            TaskState::Review,
            TaskState::Done,
            TaskState::Cancelled,
        ] {
            let mut t = todo_task();
            t.state = s;
            assert_eq!(
                start(&mut t, run_ref(), 3),
                Err(BoardError::BadState {
                    from: s,
                    op: "start"
                })
            );
            assert_eq!(t.state, s);
            assert!(t.active_run.is_none());
        }
    }

    #[test]
    fn finish_succeeded_goes_to_review_or_done_without_review() {
        let mut t = running_task();
        finish(&mut t, RunOutcome::Succeeded, "ok".into(), 10, Some(0.5), 3).unwrap();
        assert_eq!(t.state, TaskState::Review);
        assert!(t.active_run.is_none());
        match &t.thread[t.thread.len() - 2].entry {
            EntryKind::Run {
                mission_id,
                outcome,
                answer,
                tokens,
                cost_usd,
            } => {
                assert_eq!(mission_id, "m-1");
                assert_eq!(*outcome, RunOutcome::Succeeded);
                assert_eq!(answer, "ok");
                assert_eq!(*tokens, 10);
                assert_eq!(*cost_usd, Some(0.5));
            }
            other => panic!("expected Run, got {other:?}"),
        }
        assert_eq!(last_change(&t), (TaskState::Running, TaskState::Review));

        let mut t = todo_task();
        t.review_required = false;
        start(&mut t, run_ref(), 2).unwrap();
        finish(&mut t, RunOutcome::Succeeded, String::new(), 0, None, 3).unwrap();
        assert_eq!(t.state, TaskState::Done);
        assert_eq!(last_change(&t), (TaskState::Running, TaskState::Done));
    }

    #[test]
    fn finish_failed_returns_to_todo_with_the_reason_recorded() {
        let mut t = running_task();
        let outcome = RunOutcome::Failed {
            reason: "timed out".into(),
        };
        finish(&mut t, outcome.clone(), String::new(), 0, None, 3).unwrap();
        assert_eq!(t.state, TaskState::Todo);
        assert!(t.active_run.is_none());
        match &t.thread[t.thread.len() - 2].entry {
            EntryKind::Run { outcome: o, .. } => assert_eq!(*o, outcome),
            other => panic!("expected Run, got {other:?}"),
        }
        assert_eq!(last_change(&t), (TaskState::Running, TaskState::Todo));
    }

    #[test]
    fn finish_cancelled_ends_cancelled() {
        let mut t = running_task();
        finish(&mut t, RunOutcome::Cancelled, String::new(), 0, None, 3).unwrap();
        assert_eq!(t.state, TaskState::Cancelled);
        assert!(t.active_run.is_none());
        assert_eq!(last_change(&t), (TaskState::Running, TaskState::Cancelled));
    }

    #[test]
    fn finish_needs_a_running_task() {
        let mut t = todo_task();
        assert_eq!(
            finish(&mut t, RunOutcome::Succeeded, String::new(), 0, None, 3),
            Err(BoardError::BadState {
                from: TaskState::Todo,
                op: "finish"
            })
        );
        assert!(t.thread.is_empty());
    }

    #[test]
    fn approve_works_only_from_review() {
        let mut t = running_task();
        finish(&mut t, RunOutcome::Succeeded, String::new(), 0, None, 3).unwrap();
        approve(&mut t, 4).unwrap();
        assert_eq!(t.state, TaskState::Done);
        assert_eq!(last_change(&t), (TaskState::Review, TaskState::Done));

        for s in [
            TaskState::Todo,
            TaskState::Running,
            TaskState::Done,
            TaskState::Cancelled,
        ] {
            let mut t = todo_task();
            t.state = s;
            assert_eq!(
                approve(&mut t, 4),
                Err(BoardError::BadState {
                    from: s,
                    op: "approve"
                })
            );
            assert_eq!(t.state, s);
            assert!(t.thread.is_empty());
        }
    }

    #[test]
    fn send_back_needs_a_reason_adds_a_comment_and_goes_to_todo() {
        let mut t = todo_task();
        t.state = TaskState::Review;
        assert_eq!(
            send_back(&mut t, "  ", 5),
            Err(BoardError::Empty("the reason"))
        );
        assert_eq!(t.state, TaskState::Review);
        assert!(t.thread.is_empty());

        send_back(&mut t, " add tests ", 5).unwrap();
        assert_eq!(t.state, TaskState::Todo);
        assert_eq!(t.thread.len(), 2);
        assert_eq!(t.thread[0].author, Author::Operator);
        assert_eq!(
            t.thread[0].entry,
            EntryKind::Comment {
                text: "add tests".into()
            }
        );
        assert_eq!(last_change(&t), (TaskState::Review, TaskState::Todo));

        let mut t = todo_task();
        assert_eq!(
            send_back(&mut t, "why", 5),
            Err(BoardError::BadState {
                from: TaskState::Todo,
                op: "send back"
            })
        );
    }

    #[test]
    fn cancel_works_before_the_end_and_returns_the_active_run() {
        let mut t = todo_task();
        assert_eq!(cancel(&mut t, 3), Ok(None));
        assert_eq!(t.state, TaskState::Cancelled);
        assert_eq!(last_change(&t), (TaskState::Todo, TaskState::Cancelled));

        let mut t = running_task();
        assert_eq!(cancel(&mut t, 3), Ok(Some(run_ref())));
        assert_eq!(t.state, TaskState::Cancelled);
        assert!(t.active_run.is_none());
        assert_eq!(last_change(&t), (TaskState::Running, TaskState::Cancelled));

        let mut t = todo_task();
        t.state = TaskState::Review;
        assert_eq!(cancel(&mut t, 3), Ok(None));
        assert_eq!(last_change(&t), (TaskState::Review, TaskState::Cancelled));

        for s in [TaskState::Done, TaskState::Cancelled] {
            let mut t = todo_task();
            t.state = s;
            assert_eq!(
                cancel(&mut t, 3),
                Err(BoardError::BadState {
                    from: s,
                    op: "cancel"
                })
            );
            assert_eq!(t.state, s);
            assert!(t.thread.is_empty());
        }
    }

    #[test]
    fn assign_is_refused_while_running_or_finished() {
        let mut t = running_task();
        assert_eq!(
            assign(&mut t, Some(Owner::Team), 3),
            Err(BoardError::BadState {
                from: TaskState::Running,
                op: "reassign"
            })
        );
        assert_eq!(
            t.owner,
            Some(Owner::Role {
                name: "default".into()
            })
        );

        let mut t = todo_task();
        assign(&mut t, Some(Owner::Team), 3).unwrap();
        assert_eq!(t.owner, Some(Owner::Team));
        assert_eq!(t.updated_at, 3);
        assign(&mut t, None, 4).unwrap();
        assert_eq!(t.owner, None);

        t.state = TaskState::Done;
        assert!(assign(&mut t, Some(Owner::Team), 5).is_err());
    }

    fn actor(task_id: &str) -> Actor {
        Actor::Run {
            task_id: task_id.into(),
            mission_id: "m-1".into(),
        }
    }

    #[test]
    fn comment_by_a_run_is_limited_to_its_own_running_task() {
        let mut t = running_task();
        assert_eq!(
            comment(&mut t, &actor("t-other"), "hi", 3),
            Err(BoardError::NotAllowed("a run may only touch its own task"))
        );
        assert_eq!(t.thread.len(), 1, "only the Start state change");

        let mut idle = todo_task();
        assert_eq!(
            comment(&mut idle, &actor("t-1"), "hi", 3),
            Err(BoardError::NotAllowed("the task isn't running"))
        );
        assert!(idle.thread.is_empty());

        comment(&mut t, &actor("t-1"), " noted ", 3).unwrap();
        let e = t.thread.last().unwrap();
        assert_eq!(
            e.author,
            Author::Agent {
                mission_id: "m-1".into()
            }
        );
        assert_eq!(
            e.entry,
            EntryKind::Comment {
                text: "noted".into()
            }
        );
        assert_eq!(t.updated_at, 3);

        assert_eq!(
            comment(&mut t, &actor("t-1"), "  ", 4),
            Err(BoardError::Empty("the comment"))
        );
    }

    #[test]
    fn the_operator_may_comment_in_any_state() {
        let mut t = todo_task();
        t.state = TaskState::Done;
        comment(&mut t, &Actor::Operator, "later note", 3).unwrap();
        assert_eq!(t.thread[0].author, Author::Operator);
    }

    #[test]
    fn subtask_check_follows_the_same_rules() {
        let t = running_task();
        assert!(add_subtask_check(&t, &actor("t-1")).is_ok());
        assert_eq!(
            add_subtask_check(&t, &actor("t-other")),
            Err(BoardError::NotAllowed("a run may only touch its own task"))
        );
        assert_eq!(
            add_subtask_check(&todo_task(), &actor("t-1")),
            Err(BoardError::NotAllowed("the task isn't running"))
        );
        assert!(add_subtask_check(&todo_task(), &Actor::Operator).is_ok());
        let mut done = todo_task();
        done.state = TaskState::Done;
        assert_eq!(
            add_subtask_check(&done, &Actor::Operator),
            Err(BoardError::BadState {
                from: TaskState::Done,
                op: "add a subtask to"
            })
        );
    }

    #[test]
    fn interrupt_returns_a_running_task_to_todo_and_ignores_others() {
        let mut t = running_task();
        assert_eq!(interrupt(&mut t, 9), Some(run_ref()));
        assert_eq!(t.state, TaskState::Todo);
        assert!(t.active_run.is_none());
        match &t.thread[t.thread.len() - 2].entry {
            EntryKind::Run {
                mission_id,
                outcome,
                answer,
                ..
            } => {
                assert_eq!(mission_id, "m-1");
                assert_eq!(*outcome, RunOutcome::Interrupted);
                assert_eq!(answer, "interrupted by a daemon restart");
            }
            other => panic!("expected Run, got {other:?}"),
        }
        assert_eq!(last_change(&t), (TaskState::Running, TaskState::Todo));

        for s in [
            TaskState::Todo,
            TaskState::Review,
            TaskState::Done,
            TaskState::Cancelled,
        ] {
            let mut t = todo_task();
            t.state = s;
            let before = t.clone();
            assert_eq!(interrupt(&mut t, 9), None);
            assert_eq!(t, before);
        }
    }

    #[test]
    fn ids_and_run_kinds() {
        let id = new_id('t');
        assert!(id.starts_with("t-") && id.len() == 10, "{id}");
        assert_ne!(new_id('t'), new_id('t'));
        assert_eq!(run_kind(&Owner::Team), Some(RunKind::Team));
        assert_eq!(
            run_kind(&Owner::Role { name: "x".into() }),
            Some(RunKind::Role)
        );
        assert_eq!(
            run_kind(&Owner::Coder {
                workspace: "w-1".into()
            }),
            Some(RunKind::Coder)
        );
        assert_eq!(run_kind(&Owner::Agent { id: "a-1".into() }), None);
        assert!(now_ms() > 0);
    }
}
