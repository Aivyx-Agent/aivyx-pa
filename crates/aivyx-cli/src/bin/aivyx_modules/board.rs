//! `aivyx-pa board` — the work board (A20): goals, projects and tasks you
//! plan and assign, worked by role runs and team missions. IPC-backed; the
//! render helpers are pure so tests drive them without a daemon. Every string
//! that came from a title, a comment or a run answer goes through
//! `neutralise` before it is printed.

use std::path::Path;

use aivyx_capability::display::neutralise;
use aivyx_channel::daemon_client::{
    board_approve, board_assign, board_cancel, board_comment, board_create_goal,
    board_create_project, board_create_task, board_send_back, board_snapshot, board_start,
    board_task, daemon_is_running,
};
use aivyx_channel::daemon_ipc::default_socket_path;
use aivyx_ipc::board::{
    Author, BoardSnapshot, EntryKind, NewTask, Owner, RunOutcome, Task, TaskState,
};

pub const USAGE: &str = "usage:
  aivyx-pa board list [--project <id>] [--state todo|running|review|done|cancelled]
  aivyx-pa board show <task>
  aivyx-pa board goal add \"<title>\" [--description \"<text>\"]
  aivyx-pa board goal list
  aivyx-pa board project add <goal> \"<title>\" [--description \"<text>\"]
  aivyx-pa board project list [--goal <id>]
  aivyx-pa board task add (--project <id> | --parent <task>) \"<title>\"
      [--description \"<text>\"] [--owner role:<name>|team] [--no-review]
  aivyx-pa board assign <task> role:<name>|team|none
  aivyx-pa board start <task>
  aivyx-pa board approve <task>
  aivyx-pa board send-back <task> \"<why>\"
  aivyx-pa board cancel <task>
  aivyx-pa board comment <task> \"<text>\"";

/// Where a `task add` puts the task.
#[derive(Debug, Clone, PartialEq)]
pub enum TaskParent {
    Project(String),
    Task(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum BoardCommand {
    List {
        project: Option<String>,
        state: Option<TaskState>,
    },
    Show {
        task: String,
    },
    GoalAdd {
        title: String,
        description: String,
    },
    GoalList,
    ProjectAdd {
        goal: String,
        title: String,
        description: String,
    },
    ProjectList {
        goal: Option<String>,
    },
    TaskAdd {
        parent: TaskParent,
        title: String,
        description: String,
        owner: Option<Owner>,
        review_required: bool,
    },
    Assign {
        task: String,
        owner: Option<Owner>,
    },
    Start {
        task: String,
    },
    Approve {
        task: String,
    },
    SendBack {
        task: String,
        why: String,
    },
    Cancel {
        task: String,
    },
    Comment {
        task: String,
        text: String,
    },
}

/// What `list` keeps.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ListFilter {
    pub project: Option<String>,
    pub state: Option<TaskState>,
}

fn parse_state(s: &str) -> Result<TaskState, String> {
    match s {
        "todo" => Ok(TaskState::Todo),
        "running" => Ok(TaskState::Running),
        "review" => Ok(TaskState::Review),
        "done" => Ok(TaskState::Done),
        "cancelled" => Ok(TaskState::Cancelled),
        other => Err(format!(
            "unknown state `{other}` (use todo, running, review, done or cancelled)"
        )),
    }
}

/// `role:<name>` | `team` | `none`.
pub fn parse_owner(s: &str) -> Result<Option<Owner>, String> {
    match s {
        "team" => Ok(Some(Owner::Team)),
        "none" => Ok(None),
        _ => match s.strip_prefix("role:") {
            Some(name) if !name.is_empty() => Ok(Some(Owner::Role { name: name.into() })),
            _ => Err(format!(
                "unknown owner `{s}` (use role:<name>, team or none)"
            )),
        },
    }
}

/// Positional words and `--flag value` pairs, in the order given. `bools`
/// names the flags that take no value.
struct Split {
    positional: Vec<String>,
    flags: Vec<(String, Option<String>)>,
}

fn split(args: &[String], valued: &[&str], bools: &[&str]) -> Result<Split, String> {
    let mut positional = Vec::new();
    let mut flags = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if bools.contains(&a.as_str()) {
            flags.push((a.clone(), None));
        } else if valued.contains(&a.as_str()) {
            let v = it
                .next()
                .ok_or_else(|| format!("`{a}` needs a value\n{USAGE}"))?;
            flags.push((a.clone(), Some(v.clone())));
        } else if a.starts_with("--") {
            return Err(format!("unrecognized option `{a}`\n{USAGE}"));
        } else {
            positional.push(a.clone());
        }
    }
    Ok(Split { positional, flags })
}

impl Split {
    fn value(&self, name: &str) -> Option<&str> {
        self.flags
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .and_then(|(_, v)| v.as_deref())
    }
    fn has(&self, name: &str) -> bool {
        self.flags.iter().any(|(n, _)| n == name)
    }
}

/// Parse the words after `board`.
pub fn parse(args: &[String]) -> Result<BoardCommand, String> {
    let Some((sub, rest)) = args.split_first() else {
        return Err(USAGE.to_string());
    };
    match sub.as_str() {
        "list" => {
            let s = split(rest, &["--project", "--state"], &[])?;
            if !s.positional.is_empty() {
                return Err(USAGE.to_string());
            }
            Ok(BoardCommand::List {
                project: s.value("--project").map(String::from),
                state: s.value("--state").map(parse_state).transpose()?,
            })
        }
        "show" => one_task(rest).map(|task| BoardCommand::Show { task }),
        "start" => one_task(rest).map(|task| BoardCommand::Start { task }),
        "approve" => one_task(rest).map(|task| BoardCommand::Approve { task }),
        "cancel" => one_task(rest).map(|task| BoardCommand::Cancel { task }),
        "send-back" => match rest {
            [task, why] => Ok(BoardCommand::SendBack {
                task: task.clone(),
                why: why.clone(),
            }),
            _ => Err(USAGE.to_string()),
        },
        "comment" => match rest {
            [task, text] => Ok(BoardCommand::Comment {
                task: task.clone(),
                text: text.clone(),
            }),
            _ => Err(USAGE.to_string()),
        },
        "assign" => match rest {
            [task, owner] => Ok(BoardCommand::Assign {
                task: task.clone(),
                owner: parse_owner(owner)?,
            }),
            _ => Err(USAGE.to_string()),
        },
        "goal" => match rest.split_first() {
            Some((a, r)) if a == "add" => {
                let s = split(r, &["--description"], &[])?;
                match s.positional.as_slice() {
                    [title] => Ok(BoardCommand::GoalAdd {
                        title: title.clone(),
                        description: s.value("--description").unwrap_or("").into(),
                    }),
                    _ => Err(USAGE.to_string()),
                }
            }
            Some((a, r)) if a == "list" && r.is_empty() => Ok(BoardCommand::GoalList),
            _ => Err(USAGE.to_string()),
        },
        "project" => match rest.split_first() {
            Some((a, r)) if a == "add" => {
                let s = split(r, &["--description"], &[])?;
                match s.positional.as_slice() {
                    [goal, title] => Ok(BoardCommand::ProjectAdd {
                        goal: goal.clone(),
                        title: title.clone(),
                        description: s.value("--description").unwrap_or("").into(),
                    }),
                    _ => Err(USAGE.to_string()),
                }
            }
            Some((a, r)) if a == "list" => {
                let s = split(r, &["--goal"], &[])?;
                if !s.positional.is_empty() {
                    return Err(USAGE.to_string());
                }
                Ok(BoardCommand::ProjectList {
                    goal: s.value("--goal").map(String::from),
                })
            }
            _ => Err(USAGE.to_string()),
        },
        "task" => match rest.split_first() {
            Some((a, r)) if a == "add" => {
                let s = split(
                    r,
                    &["--project", "--parent", "--description", "--owner"],
                    &["--no-review"],
                )?;
                let parent = match (s.value("--project"), s.value("--parent")) {
                    (Some(_), Some(_)) => {
                        return Err(format!("give `--project` or `--parent`, not both\n{USAGE}"));
                    }
                    (Some(p), None) => TaskParent::Project(p.into()),
                    (None, Some(t)) => TaskParent::Task(t.into()),
                    (None, None) => {
                        return Err(format!(
                            "`task add` needs `--project <id>` or `--parent <task>`\n{USAGE}"
                        ));
                    }
                };
                let [title] = s.positional.as_slice() else {
                    return Err(USAGE.to_string());
                };
                let owner = match s.value("--owner") {
                    Some(o) => parse_owner(o)?,
                    None => None,
                };
                Ok(BoardCommand::TaskAdd {
                    parent,
                    title: title.clone(),
                    description: s.value("--description").unwrap_or("").into(),
                    owner,
                    review_required: !s.has("--no-review"),
                })
            }
            _ => Err(USAGE.to_string()),
        },
        other => Err(format!(
            "unrecognized argument to `aivyx-pa board`: `{other}`\n{USAGE}"
        )),
    }
}

fn one_task(rest: &[String]) -> Result<String, String> {
    match rest {
        [task] => Ok(task.clone()),
        _ => Err(USAGE.to_string()),
    }
}

async fn require_daemon_running(socket_path: &Path) -> Result<(), String> {
    if daemon_is_running(socket_path).await {
        return Ok(());
    }
    Err(format!(
        "aivyx-pa board: no daemon running on socket {} — \
         start the daemon first with `aivyx-pa daemon run`",
        socket_path.display(),
    ))
}

pub async fn run_board(cmd: BoardCommand) -> Result<(), String> {
    let socket = default_socket_path()?;
    require_daemon_running(&socket).await?;
    let e = |err: aivyx_channel::daemon_server::DaemonError| err.to_string();
    match cmd {
        BoardCommand::List { project, state } => {
            let snap = board_snapshot(&socket).await.map_err(e)?;
            print!("{}", render_list(&snap, &ListFilter { project, state }));
        }
        BoardCommand::Show { task } => {
            let t = board_task(&socket, task).await.map_err(e)?;
            print!("{}", render_task(&t));
        }
        BoardCommand::GoalAdd { title, description } => {
            let id = board_create_goal(&socket, title, description)
                .await
                .map_err(e)?;
            println!("Added goal {}", neutralise(&id));
        }
        BoardCommand::GoalList => {
            let snap = board_snapshot(&socket).await.map_err(e)?;
            print!("{}", render_goals(&snap));
        }
        BoardCommand::ProjectAdd {
            goal,
            title,
            description,
        } => {
            let id = board_create_project(&socket, goal, title, description)
                .await
                .map_err(e)?;
            println!("Added project {}", neutralise(&id));
        }
        BoardCommand::ProjectList { goal } => {
            let snap = board_snapshot(&socket).await.map_err(e)?;
            print!("{}", render_projects(&snap, goal.as_deref()));
        }
        BoardCommand::TaskAdd {
            parent,
            title,
            description,
            owner,
            review_required,
        } => {
            let (project_id, parent_task) = match parent {
                TaskParent::Project(p) => (p, None),
                TaskParent::Task(t) => {
                    let parent = board_task(&socket, t).await.map_err(e)?;
                    (parent.project_id, Some(parent.id))
                }
            };
            let id = board_create_task(
                &socket,
                NewTask {
                    project_id,
                    parent_task,
                    title,
                    description,
                    owner,
                    review_required,
                },
            )
            .await
            .map_err(e)?;
            println!("Added task {}", neutralise(&id));
        }
        BoardCommand::Assign { task, owner } => {
            board_assign(&socket, task.clone(), owner.clone())
                .await
                .map_err(e)?;
            println!(
                "Task {} is now owned by {}",
                neutralise(&task),
                neutralise(&owner_label(owner.as_ref()))
            );
        }
        BoardCommand::Start { task } => {
            let run = board_start(&socket, task.clone()).await.map_err(e)?;
            println!(
                "Started task {} (run {})",
                neutralise(&task),
                neutralise(&run)
            );
        }
        BoardCommand::Approve { task } => {
            board_approve(&socket, task.clone()).await.map_err(e)?;
            println!("Approved task {}", neutralise(&task));
        }
        BoardCommand::SendBack { task, why } => {
            board_send_back(&socket, task.clone(), why)
                .await
                .map_err(e)?;
            println!("Sent task {} back to todo", neutralise(&task));
        }
        BoardCommand::Cancel { task } => {
            board_cancel(&socket, task.clone()).await.map_err(e)?;
            println!("Cancelled task {}", neutralise(&task));
        }
        BoardCommand::Comment { task, text } => {
            board_comment(&socket, task.clone(), text)
                .await
                .map_err(e)?;
            println!("Commented on task {}", neutralise(&task));
        }
    }
    Ok(())
}

fn owner_label(owner: Option<&Owner>) -> String {
    match owner {
        Some(Owner::Role { name }) => format!("role:{name}"),
        Some(Owner::Team) => "team".into(),
        None => "-".into(),
    }
}

fn state_label(state: TaskState) -> &'static str {
    match state {
        TaskState::Todo => "todo",
        TaskState::Running => "running",
        TaskState::Review => "review",
        TaskState::Done => "done",
        TaskState::Cancelled => "cancelled",
    }
}

fn when(at_ms: u64) -> String {
    use chrono::TimeZone;
    chrono::Local
        .timestamp_millis_opt(at_ms as i64)
        .single()
        .map_or_else(
            || "-".to_string(),
            |t| t.format("%Y-%m-%d %H:%M").to_string(),
        )
}

/// Tasks under `<goal> / <project>` headings, filtered client-side. With a
/// filter, projects with no matching task are left out.
pub fn render_list(snap: &BoardSnapshot, filter: &ListFilter) -> String {
    let filtered = filter.project.is_some() || filter.state.is_some();
    let mut out = String::new();
    for project in &snap.projects {
        if filter.project.as_deref().is_some_and(|p| p != project.id) {
            continue;
        }
        let tasks: Vec<&Task> = snap
            .tasks
            .iter()
            .filter(|t| t.project_id == project.id)
            .filter(|t| filter.state.is_none_or(|s| t.state == s))
            .collect();
        if tasks.is_empty() && filtered {
            continue;
        }
        let goal = snap
            .goals
            .iter()
            .find(|g| g.id == project.goal_id)
            .map_or("(no goal)", |g| g.title.as_str());
        out.push_str(&format!(
            "{} / {}  [{}]\n",
            neutralise(goal),
            neutralise(&project.title),
            neutralise(&project.id)
        ));
        for t in tasks {
            out.push_str(&format!(
                "  {}  {}  {}  {}\n",
                neutralise(&t.id),
                state_label(t.state),
                neutralise(&owner_label(t.owner.as_ref())),
                neutralise(&t.title)
            ));
        }
    }
    if out.is_empty() {
        out.push_str("No tasks.\n");
    }
    out
}

pub fn render_goals(snap: &BoardSnapshot) -> String {
    if snap.goals.is_empty() {
        return "No goals.\n".into();
    }
    snap.goals
        .iter()
        .map(|g| format!("{}  {}\n", neutralise(&g.id), neutralise(&g.title)))
        .collect()
}

pub fn render_projects(snap: &BoardSnapshot, goal: Option<&str>) -> String {
    let mut out = String::new();
    for p in snap
        .projects
        .iter()
        .filter(|p| goal.is_none_or(|g| p.goal_id == g))
    {
        let gtitle = snap
            .goals
            .iter()
            .find(|g| g.id == p.goal_id)
            .map_or("(no goal)", |g| g.title.as_str());
        out.push_str(&format!(
            "{}  {}  ({})\n",
            neutralise(&p.id),
            neutralise(&p.title),
            neutralise(gtitle)
        ));
    }
    if out.is_empty() {
        out.push_str("No projects.\n");
    }
    out
}

/// The task's fields, then its thread, one entry per line.
pub fn render_task(t: &Task) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{}  {}\n",
        neutralise(&t.id),
        neutralise(&t.title)
    ));
    out.push_str(&format!("  state:    {}\n", state_label(t.state)));
    out.push_str(&format!(
        "  owner:    {}\n",
        neutralise(&owner_label(t.owner.as_ref()))
    ));
    out.push_str(&format!("  project:  {}\n", neutralise(&t.project_id)));
    if let Some(p) = &t.parent_task {
        out.push_str(&format!("  parent:   {}\n", neutralise(p)));
    }
    out.push_str(&format!(
        "  review:   {}\n",
        if t.review_required {
            "required"
        } else {
            "not required"
        }
    ));
    if let Some(run) = &t.active_run {
        out.push_str(&format!("  run:      {}\n", neutralise(&run.mission_id)));
    }
    if !t.description.is_empty() {
        out.push_str(&format!("  {}\n", neutralise(&t.description)));
    }
    if t.thread.is_empty() {
        out.push_str("No thread yet.\n");
        return out;
    }
    out.push_str("Thread:\n");
    for entry in &t.thread {
        let who = match entry.author {
            Author::Operator => "operator",
            Author::Agent { .. } => "agent",
        };
        let body = match &entry.entry {
            EntryKind::Comment { text } => neutralise(text),
            EntryKind::StateChange { from, to } => {
                format!("{} → {}", state_label(*from), state_label(*to))
            }
            EntryKind::Run {
                mission_id,
                outcome,
                answer,
                tokens,
                cost_usd,
            } => {
                let outcome = match outcome {
                    RunOutcome::Succeeded => "succeeded".to_string(),
                    RunOutcome::Failed { reason } => format!("failed: {}", neutralise(reason)),
                    RunOutcome::Cancelled => "cancelled".into(),
                    RunOutcome::Interrupted => "interrupted".into(),
                };
                // No tokens and no cost: unmetered (a team run without a
                // mission budget counts nothing), not free.
                let spend = match (tokens, cost_usd) {
                    (0, None) => "—".to_string(),
                    (n, None) => format!("{n} tokens"),
                    (n, Some(c)) => format!("{n} tokens, ${c:.4}"),
                };
                let head: String = answer.chars().take(200).collect();
                // The mission id links the entry to its Missions record. A
                // failed run has no answer: no dangling `: `.
                let answer = if head.is_empty() {
                    String::new()
                } else {
                    format!(": {}", neutralise(&head))
                };
                format!(
                    "run {} {outcome}, {spend}{answer}",
                    neutralise(mission_id)
                )
            }
        };
        out.push_str(&format!("  {}  {}  {}\n", when(entry.at), who, body));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_ipc::board::{Goal, Project, ThreadEntry};

    fn args(s: &str) -> Vec<String> {
        // Quoted segments stay one word, as a shell would hand them over.
        let mut out = Vec::new();
        let mut cur = String::new();
        let mut quoted = false;
        for c in s.chars() {
            match c {
                '"' => quoted = !quoted,
                ' ' if !quoted => {
                    if !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                    }
                }
                c => cur.push(c),
            }
        }
        if !cur.is_empty() {
            out.push(cur);
        }
        out
    }

    fn p(s: &str) -> Result<BoardCommand, String> {
        parse(&args(s))
    }

    #[test]
    fn every_form_parses() {
        assert_eq!(
            p("list").unwrap(),
            BoardCommand::List {
                project: None,
                state: None
            }
        );
        assert_eq!(
            p("list --project p-1 --state review").unwrap(),
            BoardCommand::List {
                project: Some("p-1".into()),
                state: Some(TaskState::Review)
            }
        );
        assert_eq!(
            p("show t-1").unwrap(),
            BoardCommand::Show { task: "t-1".into() }
        );
        assert_eq!(
            p("goal add \"Ship it\" --description \"all of it\"").unwrap(),
            BoardCommand::GoalAdd {
                title: "Ship it".into(),
                description: "all of it".into()
            }
        );
        assert_eq!(p("goal list").unwrap(), BoardCommand::GoalList);
        assert_eq!(
            p("project add g-1 \"Launch\"").unwrap(),
            BoardCommand::ProjectAdd {
                goal: "g-1".into(),
                title: "Launch".into(),
                description: String::new()
            }
        );
        assert_eq!(
            p("project list --goal g-1").unwrap(),
            BoardCommand::ProjectList {
                goal: Some("g-1".into())
            }
        );
        assert_eq!(
            p("project list").unwrap(),
            BoardCommand::ProjectList { goal: None }
        );
        assert_eq!(
            p("task add --project p-1 \"Draft\" --owner role:writer --no-review").unwrap(),
            BoardCommand::TaskAdd {
                parent: TaskParent::Project("p-1".into()),
                title: "Draft".into(),
                description: String::new(),
                owner: Some(Owner::Role {
                    name: "writer".into()
                }),
                review_required: false
            }
        );
        assert_eq!(
            p("task add --parent t-1 \"Sub\" --description \"d\" --owner team").unwrap(),
            BoardCommand::TaskAdd {
                parent: TaskParent::Task("t-1".into()),
                title: "Sub".into(),
                description: "d".into(),
                owner: Some(Owner::Team),
                review_required: true
            }
        );
        assert_eq!(
            p("assign t-1 role:writer").unwrap(),
            BoardCommand::Assign {
                task: "t-1".into(),
                owner: Some(Owner::Role {
                    name: "writer".into()
                })
            }
        );
        assert_eq!(
            p("assign t-1 team").unwrap(),
            BoardCommand::Assign {
                task: "t-1".into(),
                owner: Some(Owner::Team)
            }
        );
        assert_eq!(
            p("assign t-1 none").unwrap(),
            BoardCommand::Assign {
                task: "t-1".into(),
                owner: None
            }
        );
        assert_eq!(
            p("start t-1").unwrap(),
            BoardCommand::Start { task: "t-1".into() }
        );
        assert_eq!(
            p("approve t-1").unwrap(),
            BoardCommand::Approve { task: "t-1".into() }
        );
        assert_eq!(
            p("send-back t-1 \"too long\"").unwrap(),
            BoardCommand::SendBack {
                task: "t-1".into(),
                why: "too long".into()
            }
        );
        assert_eq!(
            p("cancel t-1").unwrap(),
            BoardCommand::Cancel { task: "t-1".into() }
        );
        assert_eq!(
            p("comment t-1 \"hello there\"").unwrap(),
            BoardCommand::Comment {
                task: "t-1".into(),
                text: "hello there".into()
            }
        );
    }

    #[test]
    fn bad_input_is_refused() {
        let e = p("bogus").unwrap_err();
        assert!(e.contains("unrecognized") && e.contains("bogus"), "{e}");
        let e = p("task add --project p-1 --parent t-1 \"x\"").unwrap_err();
        assert!(e.contains("not both"), "{e}");
        assert!(p("task add \"x\"").is_err());
        assert!(p("list --state nope").is_err());
        let e = p("assign t-1 bob").unwrap_err();
        assert!(e.contains("role:<name>") && e.contains("team"), "{e}");
        assert!(parse(&[]).is_err());
    }

    fn task(id: &str, project: &str, state: TaskState, owner: Option<Owner>) -> Task {
        Task {
            id: id.into(),
            project_id: project.into(),
            parent_task: None,
            title: format!("title {id}"),
            description: String::new(),
            owner,
            review_required: true,
            state,
            thread: vec![],
            active_run: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn snap() -> BoardSnapshot {
        BoardSnapshot {
            goals: vec![Goal {
                id: "g-1".into(),
                title: "Launch".into(),
                description: String::new(),
                created_at: 0,
                updated_at: 0,
            }],
            projects: vec![
                Project {
                    id: "p-1".into(),
                    goal_id: "g-1".into(),
                    title: "Site".into(),
                    description: String::new(),
                    created_at: 0,
                    updated_at: 0,
                },
                Project {
                    id: "p-2".into(),
                    goal_id: "g-1".into(),
                    title: "Docs".into(),
                    description: String::new(),
                    created_at: 0,
                    updated_at: 0,
                },
            ],
            tasks: vec![
                task(
                    "t-1",
                    "p-1",
                    TaskState::Todo,
                    Some(Owner::Role {
                        name: "writer".into(),
                    }),
                ),
                task("t-2", "p-1", TaskState::Review, Some(Owner::Team)),
                task("t-3", "p-2", TaskState::Done, None),
            ],
        }
    }

    #[test]
    fn list_groups_under_goal_and_project_and_filters() {
        let all = render_list(&snap(), &ListFilter::default());
        assert!(all.contains("Launch / Site"), "{all}");
        assert!(all.contains("Launch / Docs"), "{all}");
        assert!(all.contains("t-1  todo  role:writer  title t-1"), "{all}");
        assert!(all.contains("t-2  review  team  title t-2"), "{all}");
        assert!(all.contains("t-3  done  -  title t-3"), "{all}");

        let review = render_list(
            &snap(),
            &ListFilter {
                project: None,
                state: Some(TaskState::Review),
            },
        );
        assert!(
            review.contains("t-2") && !review.contains("t-1"),
            "{review}"
        );
        assert!(!review.contains("Docs"), "{review}");

        let p2 = render_list(
            &snap(),
            &ListFilter {
                project: Some("p-2".into()),
                state: None,
            },
        );
        assert!(p2.contains("t-3") && !p2.contains("t-2"), "{p2}");
        assert_eq!(
            render_list(&BoardSnapshot::default(), &ListFilter::default()),
            "No tasks.\n"
        );
    }

    #[test]
    fn show_renders_the_thread_and_neutralises_text() {
        let mut t = task("t-1", "p-1", TaskState::Review, None);
        t.thread = vec![
            ThreadEntry {
                at: 1_000,
                author: Author::Operator,
                entry: EntryKind::Comment {
                    text: "go\u{1b}[31m\nnow".into(),
                },
            },
            ThreadEntry {
                at: 2_000,
                author: Author::Operator,
                entry: EntryKind::StateChange {
                    from: TaskState::Todo,
                    to: TaskState::Running,
                },
            },
            ThreadEntry {
                at: 3_000,
                author: Author::Agent {
                    mission_id: "m-1".into(),
                },
                entry: EntryKind::Run {
                    mission_id: "m-1".into(),
                    outcome: RunOutcome::Succeeded,
                    answer: "x".repeat(300),
                    tokens: 42,
                    cost_usd: Some(0.5),
                },
            },
        ];
        let out = render_task(&t);
        assert!(out.contains("operator  go"), "{out}");
        assert!(
            !out.contains('\u{1b}'),
            "escape bytes must not reach the terminal"
        );
        assert!(out.contains("todo → running"), "{out}");
        assert!(
            out.contains("agent  run m-1 succeeded, 42 tokens, $0.5000: "),
            "{out}"
        );
        assert!(
            out.contains(&"x".repeat(200)) && !out.contains(&"x".repeat(201)),
            "{out}"
        );

        // A failed run has no answer: the line ends at the reason.
        t.thread = vec![ThreadEntry {
            at: 4_000,
            author: Author::Agent {
                mission_id: "m-2".into(),
            },
            entry: EntryKind::Run {
                mission_id: "m-2".into(),
                outcome: RunOutcome::Failed {
                    reason: "it broke".into(),
                },
                answer: String::new(),
                tokens: 0,
                cost_usd: None,
            },
        }];
        let out = render_task(&t);
        assert!(
            out.contains("agent  run m-2 failed: it broke, —\n"),
            "{out}"
        );
    }
}
