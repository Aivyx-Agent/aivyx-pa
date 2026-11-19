//! `aivyx-pa board` — the work board (A20): goals, projects and tasks you
//! plan and assign, worked by role runs and team missions. IPC-backed; the
//! render helpers are pure so tests drive them without a daemon. Every string
//! that came from a title, a comment or a run answer goes through
//! `neutralise` before it is printed.

use std::path::Path;

use aivyx_capability::display::neutralise;
use aivyx_channel::daemon_client::{
    board_agents, board_approve, board_assign, board_cancel, board_comment, board_create_agent,
    board_create_goal, board_create_project, board_create_task, board_edit_agent,
    board_pause_agent, board_remove_agent, board_resume_agent, board_send_back, board_snapshot,
    board_start, board_task, board_wake_agent, daemon_is_running,
};
use aivyx_channel::daemon_ipc::default_socket_path;
use aivyx_ipc::board::{
    Agent, Author, BoardSnapshot, EntryKind, NewAgent, NewTask, Owner, RunOutcome, Task,
    TaskState, WorksAs,
};

pub const USAGE: &str = "usage:
  aivyx-pa board list [--project <id>] [--state todo|running|review|done|cancelled]
  aivyx-pa board show <task>
  aivyx-pa board goal add \"<title>\" [--description \"<text>\"]
  aivyx-pa board goal list
  aivyx-pa board project add <goal> \"<title>\" [--description \"<text>\"]
  aivyx-pa board project list [--goal <id>]
  aivyx-pa board task add (--project <id> | --parent <task>) \"<title>\"
      [--description \"<text>\"] [--owner role:<name>|team|agent:<name>] [--no-review]
  aivyx-pa board agent list
  aivyx-pa board agent add <name> --works-as role:<name>|team [--title \"<t>\"]
      [--reports-to <agent>] [--heartbeat \"<cron>\"] [--wake-on-assign]
  aivyx-pa board agent edit <name> [--name <new>] [--title \"<t>\"]
      [--reports-to <agent>|--no-reports-to] [--works-as role:<name>|team]
      [--heartbeat \"<cron>\"|--no-heartbeat] [--wake-on-assign|--no-wake-on-assign]
  aivyx-pa board agent pause|resume|remove|wake <name>
  aivyx-pa board assign <task> role:<name>|team|agent:<name>|none
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
        owner: AssignTo,
        review_required: bool,
    },
    Assign {
        task: String,
        owner: AssignTo,
    },
    AgentList,
    AgentAdd {
        name: String,
        title: Option<String>,
        reports_to: Option<String>,
        works_as: WorksAs,
        heartbeat: Option<String>,
        wake_on_assign: bool,
    },
    AgentEdit {
        name: String,
        new_name: Option<String>,
        title: Option<String>,
        reports_to: Change<String>,
        works_as: Option<WorksAs>,
        heartbeat: Change<String>,
        wake_on_assign: Option<bool>,
    },
    AgentPause {
        name: String,
    },
    AgentResume {
        name: String,
    },
    AgentRemove {
        name: String,
    },
    AgentWake {
        name: String,
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

/// Who `assign` hands a task to. An agent is named; the name becomes an id
/// when the command runs.
#[derive(Debug, Clone, PartialEq)]
pub enum AssignTo {
    None,
    Team,
    Role(String),
    Agent(String),
}

/// What an `agent edit` does to an optional field.
#[derive(Debug, Clone, PartialEq)]
pub enum Change<T> {
    Keep,
    Set(T),
    Clear,
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

/// `role:<name>` | `team` | `agent:<name>` | `none`.
pub fn parse_assign(s: &str) -> Result<AssignTo, String> {
    match s {
        "team" => Ok(AssignTo::Team),
        "none" => Ok(AssignTo::None),
        _ => match (s.strip_prefix("role:"), s.strip_prefix("agent:")) {
            (Some(name), _) if !name.is_empty() => Ok(AssignTo::Role(name.into())),
            (_, Some(name)) if !name.is_empty() => Ok(AssignTo::Agent(name.into())),
            _ => Err(format!(
                "unknown owner `{s}` (use role:<name>, team, agent:<name> or none)"
            )),
        },
    }
}

/// `role:<name>` | `team`.
pub fn parse_works_as(s: &str) -> Result<WorksAs, String> {
    match s {
        "team" => Ok(WorksAs::Team),
        _ => match s.strip_prefix("role:") {
            Some(name) if !name.is_empty() => Ok(WorksAs::Role { name: name.into() }),
            _ => Err(format!("unknown works-as `{s}` (use role:<name> or team)")),
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
                owner: parse_assign(owner)?,
            }),
            _ => Err(USAGE.to_string()),
        },
        "agent" => parse_agent(rest),
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
                    Some(o) => parse_assign(o)?,
                    None => AssignTo::None,
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

/// An optional field's `--x <v>` / `--no-x` pair.
fn change(s: &Split, flag: &str, no_flag: &str) -> Result<Change<String>, String> {
    match (s.value(flag), s.has(no_flag)) {
        (Some(_), true) => Err(format!("give `{flag}` or `{no_flag}`, not both\n{USAGE}")),
        (Some(v), false) => Ok(Change::Set(v.into())),
        (None, true) => Ok(Change::Clear),
        (None, false) => Ok(Change::Keep),
    }
}

fn one_name(rest: &[String]) -> Result<String, String> {
    match rest {
        [name] => Ok(name.clone()),
        _ => Err(USAGE.to_string()),
    }
}

fn parse_agent(rest: &[String]) -> Result<BoardCommand, String> {
    let Some((sub, rest)) = rest.split_first() else {
        return Err(USAGE.to_string());
    };
    match sub.as_str() {
        "list" if rest.is_empty() => Ok(BoardCommand::AgentList),
        "pause" => one_name(rest).map(|name| BoardCommand::AgentPause { name }),
        "resume" => one_name(rest).map(|name| BoardCommand::AgentResume { name }),
        "remove" => one_name(rest).map(|name| BoardCommand::AgentRemove { name }),
        "wake" => one_name(rest).map(|name| BoardCommand::AgentWake { name }),
        "add" => {
            let s = split(
                rest,
                &["--works-as", "--title", "--reports-to", "--heartbeat"],
                &["--wake-on-assign"],
            )?;
            let [name] = s.positional.as_slice() else {
                return Err(USAGE.to_string());
            };
            let Some(works_as) = s.value("--works-as") else {
                return Err(format!(
                    "`agent add` needs `--works-as role:<name>|team`\n{USAGE}"
                ));
            };
            Ok(BoardCommand::AgentAdd {
                name: name.clone(),
                title: s.value("--title").map(String::from),
                reports_to: s.value("--reports-to").map(String::from),
                works_as: parse_works_as(works_as)?,
                heartbeat: s.value("--heartbeat").map(String::from),
                wake_on_assign: s.has("--wake-on-assign"),
            })
        }
        "edit" => {
            let s = split(
                rest,
                &["--name", "--title", "--reports-to", "--works-as", "--heartbeat"],
                &[
                    "--no-reports-to",
                    "--no-heartbeat",
                    "--wake-on-assign",
                    "--no-wake-on-assign",
                ],
            )?;
            let [name] = s.positional.as_slice() else {
                return Err(USAGE.to_string());
            };
            let wake_on_assign = match (s.has("--wake-on-assign"), s.has("--no-wake-on-assign")) {
                (true, true) => {
                    return Err(format!(
                        "give `--wake-on-assign` or `--no-wake-on-assign`, not both\n{USAGE}"
                    ));
                }
                (true, false) => Some(true),
                (false, true) => Some(false),
                (false, false) => None,
            };
            let cmd = BoardCommand::AgentEdit {
                name: name.clone(),
                new_name: s.value("--name").map(String::from),
                title: s.value("--title").map(String::from),
                reports_to: change(&s, "--reports-to", "--no-reports-to")?,
                works_as: s.value("--works-as").map(parse_works_as).transpose()?,
                heartbeat: change(&s, "--heartbeat", "--no-heartbeat")?,
                wake_on_assign,
            };
            if s.flags.is_empty() {
                return Err(format!("`agent edit` needs something to change\n{USAGE}"));
            }
            Ok(cmd)
        }
        _ => Err(USAGE.to_string()),
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
            let agents = board_agents(&socket).await.map_err(e)?;
            print!("{}", render_task(&t, &agents));
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
            let owner = resolve_owner(&socket, owner).await?.0;
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
            let (owner, label) = resolve_owner(&socket, owner).await?;
            board_assign(&socket, task.clone(), owner).await.map_err(e)?;
            println!(
                "Task {} is now owned by {}",
                neutralise(&task),
                neutralise(&label)
            );
        }
        BoardCommand::AgentList => {
            let agents = board_agents(&socket).await.map_err(e)?;
            let snap = board_snapshot(&socket).await.map_err(e)?;
            print!("{}", render_agents(&agents, &snap.tasks));
        }
        BoardCommand::AgentAdd {
            name,
            title,
            reports_to,
            works_as,
            heartbeat,
            wake_on_assign,
        } => {
            let reports_to = match reports_to {
                Some(boss) => Some(resolve_agent(&socket, &boss).await?),
                None => None,
            };
            board_create_agent(
                &socket,
                NewAgent {
                    name: Some(name.clone()),
                    title,
                    reports_to,
                    works_as: Some(works_as),
                    heartbeat,
                    wake_on_assign: wake_on_assign.then_some(true),
                    ..NewAgent::default()
                },
            )
            .await
            .map_err(e)?;
            println!("Added agent {}", neutralise(&name));
        }
        BoardCommand::AgentEdit {
            name,
            new_name,
            title,
            reports_to,
            works_as,
            heartbeat,
            wake_on_assign,
        } => {
            let mut agent = NewAgent {
                name: new_name.clone(),
                title,
                works_as,
                wake_on_assign,
                ..NewAgent::default()
            };
            match reports_to {
                Change::Keep => {}
                Change::Clear => agent.clear_reports_to = true,
                Change::Set(boss) => agent.reports_to = Some(resolve_agent(&socket, &boss).await?),
            }
            match heartbeat {
                Change::Keep => {}
                Change::Clear => agent.clear_heartbeat = true,
                Change::Set(cron) => agent.heartbeat = Some(cron),
            }
            board_edit_agent(&socket, name.clone(), agent)
                .await
                .map_err(e)?;
            println!("Updated agent {}", neutralise(new_name.as_deref().unwrap_or(&name)));
        }
        BoardCommand::AgentPause { name } => {
            board_pause_agent(&socket, name.clone()).await.map_err(e)?;
            println!("Paused agent {}", neutralise(&name));
        }
        BoardCommand::AgentResume { name } => {
            board_resume_agent(&socket, name.clone()).await.map_err(e)?;
            println!("Resumed agent {}", neutralise(&name));
        }
        BoardCommand::AgentRemove { name } => {
            board_remove_agent(&socket, name.clone()).await.map_err(e)?;
            println!("Removed agent {}", neutralise(&name));
        }
        BoardCommand::AgentWake { name } => {
            let agents = board_agents(&socket).await.map_err(e)?;
            let msg = wake_message(&agents, &name)?;
            if msg.is_none() {
                board_wake_agent(&socket, name.clone()).await.map_err(e)?;
            }
            println!(
                "{}",
                msg.unwrap_or_else(|| format!(
                    "Wake requested for agent {} (it runs in the background)",
                    neutralise(&name)
                ))
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

/// What `agent wake` says instead of waking: an error for an unknown name,
/// the hint for a paused agent, `None` when the wake should go ahead.
fn wake_message(agents: &[Agent], name: &str) -> Result<Option<String>, String> {
    let agent = agents
        .iter()
        .find(|a| a.name == name)
        .ok_or_else(|| format!("no agent named `{}`", neutralise(name)))?;
    Ok(agent
        .paused
        .then(|| format!("{} is paused; resume it to wake it", neutralise(name))))
}

/// An `AssignTo` as the owner to send, and its label. An agent's name
/// becomes its id.
async fn resolve_owner(socket: &Path, to: AssignTo) -> Result<(Option<Owner>, String), String> {
    Ok(match to {
        AssignTo::None => (None, "-".to_string()),
        AssignTo::Team => (Some(Owner::Team), "team".to_string()),
        AssignTo::Role(name) => {
            let label = format!("role:{name}");
            (Some(Owner::Role { name }), label)
        }
        AssignTo::Agent(name) => {
            let id = resolve_agent(socket, &name).await?;
            (Some(Owner::Agent { id }), format!("agent:{name}"))
        }
    })
}

/// An agent's name to its id.
async fn resolve_agent(socket: &Path, name: &str) -> Result<String, String> {
    let agents = board_agents(socket).await.map_err(|e| e.to_string())?;
    agents
        .into_iter()
        .find(|a| a.name == name)
        .map(|a| a.id)
        .ok_or_else(|| format!("no agent named `{name}`"))
}

fn owner_label(owner: Option<&Owner>, agents: &[Agent]) -> String {
    match owner {
        Some(Owner::Role { name }) => format!("role:{name}"),
        Some(Owner::Team) => "team".into(),
        Some(Owner::Coder { workspace }) => format!("coder:{workspace}"),
        Some(Owner::Agent { id }) => agents
            .iter()
            .find(|a| a.id == *id)
            .map_or_else(|| format!("agent:{id}"), |a| format!("agent:{}", a.name)),
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
    let agents = snap.agents.as_slice();
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
                neutralise(&owner_label(t.owner.as_ref(), agents)),
                neutralise(&t.title)
            ));
        }
    }
    if out.is_empty() {
        out.push_str("No tasks.\n");
    }
    out
}

/// The org chart: roots (no boss, or a boss that's gone) first, each with
/// its reports indented two spaces beneath.
pub fn render_agents(agents: &[Agent], tasks: &[Task]) -> String {
    if agents.is_empty() {
        return "No agents.\n".into();
    }
    let by_name = |list: &mut Vec<&Agent>| list.sort_by(|a, b| a.name.cmp(&b.name));
    let mut out = String::new();
    let mut seen: Vec<&str> = Vec::new();
    let mut roots: Vec<&Agent> = agents
        .iter()
        .filter(|a| {
            !a.reports_to
                .as_ref()
                .is_some_and(|b| agents.iter().any(|x| x.id == *b))
        })
        .collect();
    by_name(&mut roots);
    for root in roots {
        agent_lines(root, agents, tasks, 0, false, &mut seen, &mut out);
    }
    // Agents a stored loop left unreached: show each as a root, marked.
    let mut rest: Vec<&Agent> = agents
        .iter()
        .filter(|a| !seen.contains(&a.id.as_str()))
        .collect();
    by_name(&mut rest);
    for a in rest {
        agent_lines(a, agents, tasks, 0, true, &mut seen, &mut out);
    }
    out
}

fn agent_lines<'a>(
    agent: &'a Agent,
    agents: &'a [Agent],
    tasks: &[Task],
    depth: usize,
    in_loop: bool,
    seen: &mut Vec<&'a str>,
    out: &mut String,
) {
    if seen.contains(&agent.id.as_str()) {
        return;
    }
    seen.push(&agent.id);
    let open = tasks
        .iter()
        .filter(|t| matches!(&t.owner, Some(Owner::Agent { id }) if *id == agent.id))
        .filter(|t| matches!(t.state, TaskState::Todo | TaskState::Running | TaskState::Review))
        .count();
    let works_as = match &agent.works_as {
        WorksAs::Role { name } => format!("role:{name}"),
        WorksAs::Team => "team".into(),
        WorksAs::Coder { workspace } => format!("coder:{workspace}"),
    };
    let mut line = format!("{}{}", "  ".repeat(depth), neutralise(&agent.name));
    if !agent.title.is_empty() {
        line.push_str(&format!("  {}", neutralise(&agent.title)));
    }
    line.push_str(&format!(
        "  works as {}  heartbeat {}",
        neutralise(&works_as),
        agent.heartbeat.as_deref().map_or_else(|| "—".to_string(), neutralise)
    ));
    if agent.paused {
        line.push_str("  [paused]");
    }
    if in_loop {
        line.push_str("  (reports-to loop)");
    }
    line.push_str(&format!(
        "  last wake {}  open {open}\n",
        agent.last_wake.map_or_else(|| "never".to_string(), when)
    ));
    out.push_str(&line);
    let mut children: Vec<&Agent> = agents
        .iter()
        .filter(|a| a.reports_to.as_deref() == Some(agent.id.as_str()))
        .collect();
    children.sort_by(|a, b| a.name.cmp(&b.name));
    for child in children {
        agent_lines(child, agents, tasks, depth + 1, in_loop, seen, out);
    }
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
pub fn render_task(t: &Task, agents: &[Agent]) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "{}  {}\n",
        neutralise(&t.id),
        neutralise(&t.title)
    ));
    out.push_str(&format!("  state:    {}\n", state_label(t.state)));
    out.push_str(&format!(
        "  owner:    {}\n",
        neutralise(&owner_label(t.owner.as_ref(), agents))
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
                owner: AssignTo::Role("writer".into()),
                review_required: false
            }
        );
        assert_eq!(
            p("task add --parent t-1 \"Sub\" --description \"d\" --owner team").unwrap(),
            BoardCommand::TaskAdd {
                parent: TaskParent::Task("t-1".into()),
                title: "Sub".into(),
                description: "d".into(),
                owner: AssignTo::Team,
                review_required: true
            }
        );
        match p("task add --project p-1 \"x\" --owner agent:ana").unwrap() {
            BoardCommand::TaskAdd { owner, .. } => assert_eq!(owner, AssignTo::Agent("ana".into())),
            other => panic!("{other:?}"),
        }
        match p("task add --project p-1 \"x\"").unwrap() {
            BoardCommand::TaskAdd { owner, .. } => assert_eq!(owner, AssignTo::None),
            other => panic!("{other:?}"),
        }
        assert_eq!(
            p("assign t-1 role:writer").unwrap(),
            BoardCommand::Assign {
                task: "t-1".into(),
                owner: AssignTo::Role("writer".into())
            }
        );
        assert_eq!(
            p("assign t-1 team").unwrap(),
            BoardCommand::Assign {
                task: "t-1".into(),
                owner: AssignTo::Team
            }
        );
        assert_eq!(
            p("assign t-1 none").unwrap(),
            BoardCommand::Assign {
                task: "t-1".into(),
                owner: AssignTo::None
            }
        );
        assert_eq!(
            p("assign t-1 agent:ana").unwrap(),
            BoardCommand::Assign {
                task: "t-1".into(),
                owner: AssignTo::Agent("ana".into())
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

    #[test]
    fn every_agent_form_parses() {
        assert_eq!(p("agent list").unwrap(), BoardCommand::AgentList);
        assert_eq!(
            p("agent add ana --works-as role:writer --title \"Lead writer\" --reports-to boss --heartbeat \"0 0 9 * * *\" --wake-on-assign").unwrap(),
            BoardCommand::AgentAdd {
                name: "ana".into(),
                title: Some("Lead writer".into()),
                reports_to: Some("boss".into()),
                works_as: WorksAs::Role { name: "writer".into() },
                heartbeat: Some("0 0 9 * * *".into()),
                wake_on_assign: true,
            }
        );
        assert_eq!(
            p("agent add bo --works-as team").unwrap(),
            BoardCommand::AgentAdd {
                name: "bo".into(),
                title: None,
                reports_to: None,
                works_as: WorksAs::Team,
                heartbeat: None,
                wake_on_assign: false,
            }
        );
        assert_eq!(
            p("agent edit ana --name anna --title \"T\" --reports-to boss --works-as team --heartbeat \"* * * * *\" --no-wake-on-assign").unwrap(),
            BoardCommand::AgentEdit {
                name: "ana".into(),
                new_name: Some("anna".into()),
                title: Some("T".into()),
                reports_to: Change::Set("boss".into()),
                works_as: Some(WorksAs::Team),
                heartbeat: Change::Set("* * * * *".into()),
                wake_on_assign: Some(false),
            }
        );
        assert_eq!(
            p("agent edit ana --no-reports-to --no-heartbeat --wake-on-assign").unwrap(),
            BoardCommand::AgentEdit {
                name: "ana".into(),
                new_name: None,
                title: None,
                reports_to: Change::Clear,
                works_as: None,
                heartbeat: Change::Clear,
                wake_on_assign: Some(true),
            }
        );
        assert_eq!(
            p("agent pause ana").unwrap(),
            BoardCommand::AgentPause { name: "ana".into() }
        );
        assert_eq!(
            p("agent resume ana").unwrap(),
            BoardCommand::AgentResume { name: "ana".into() }
        );
        assert_eq!(
            p("agent remove ana").unwrap(),
            BoardCommand::AgentRemove { name: "ana".into() }
        );
        assert_eq!(
            p("agent wake ana").unwrap(),
            BoardCommand::AgentWake { name: "ana".into() }
        );
    }

    #[test]
    fn bad_agent_input_is_refused() {
        let e = p("agent add ana").unwrap_err();
        assert!(e.contains("--works-as"), "{e}");
        assert!(p("agent add ana --works-as nobody").is_err());
        let e = p("agent edit ana --heartbeat x --no-heartbeat").unwrap_err();
        assert!(e.contains("not both"), "{e}");
        let e = p("agent edit ana --reports-to b --no-reports-to").unwrap_err();
        assert!(e.contains("not both"), "{e}");
        let e = p("agent edit ana --wake-on-assign --no-wake-on-assign").unwrap_err();
        assert!(e.contains("not both"), "{e}");
        let e = p("agent edit ana").unwrap_err();
        assert!(e.contains("something to change"), "{e}");
        assert!(p("agent").is_err());
        assert!(p("agent pause").is_err());
        assert!(p("agent pause a b").is_err());
        assert!(p("agent list extra").is_err());
        assert!(p("agent frob ana").is_err());
        assert!(p("assign t-1 agent:").is_err());
    }

    #[test]
    fn wake_says_paused_and_unknown_plainly() {
        let mut ana = agent("a-1", "ana", None);
        assert_eq!(wake_message(std::slice::from_ref(&ana), "ana"), Ok(None));
        ana.paused = true;
        assert_eq!(
            wake_message(std::slice::from_ref(&ana), "ana"),
            Ok(Some("ana is paused; resume it to wake it".into()))
        );
        assert_eq!(
            wake_message(&[ana], "bob"),
            Err("no agent named `bob`".into())
        );
    }

    fn agent(id: &str, name: &str, boss: Option<&str>) -> Agent {
        Agent {
            id: id.into(),
            name: name.into(),
            title: String::new(),
            reports_to: boss.map(String::from),
            works_as: WorksAs::Team,
            heartbeat: None,
            wake_on_assign: false,
            paused: false,
            last_wake: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    #[test]
    fn the_org_chart_nests_reports_under_their_boss() {
        let mut boss = agent("a-1", "boss", None);
        boss.title = "Head".into();
        boss.heartbeat = Some("0 0 9 * * *".into());
        boss.works_as = WorksAs::Role { name: "lead".into() };
        let mut ana = agent("a-2", "ana", Some("a-1"));
        ana.paused = true;
        let deep = agent("a-3", "deep", Some("a-2"));
        let solo = agent("a-4", "solo", None);
        let tasks = vec![
            task("t-1", "p", TaskState::Todo, Some(Owner::Agent { id: "a-2".into() })),
            task("t-2", "p", TaskState::Review, Some(Owner::Agent { id: "a-2".into() })),
            task("t-3", "p", TaskState::Done, Some(Owner::Agent { id: "a-2".into() })),
        ];
        let out = render_agents(&[boss, ana, deep, solo], &tasks);
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 4, "{out}");
        assert_eq!(
            lines[0],
            "boss  Head  works as role:lead  heartbeat 0 0 9 * * *  last wake never  open 0"
        );
        assert_eq!(
            lines[1],
            "  ana  works as team  heartbeat —  [paused]  last wake never  open 2"
        );
        assert!(lines[2].starts_with("    deep  works as team"), "{out}");
        assert!(lines[3].starts_with("solo  works as team"), "{out}");
        assert_eq!(render_agents(&[], &[]), "No agents.\n");
    }

    #[test]
    fn a_stored_loop_still_renders_and_names_sort() {
        let a = agent("a-1", "zed", Some("a-2"));
        let b = agent("a-2", "amy", Some("a-1"));
        let out = render_agents(&[a, b], &[]);
        assert_eq!(out.lines().count(), 2, "{out}");
        assert!(out.contains("zed") && out.contains("amy"), "{out}");
        assert!(out.lines().all(|l| l.contains("(reports-to loop)")), "{out}");

        // Created zed, mid, amy, kid-of-zed: roots and children by name.
        let agents = [
            agent("a-1", "zed", None),
            agent("a-2", "mid", None),
            agent("a-3", "amy", None),
            agent("a-4", "yan", Some("a-1")),
            agent("a-5", "bob", Some("a-1")),
        ];
        let out = render_agents(&agents, &[]);
        let names: Vec<&str> = out
            .lines()
            .map(|l| l.split_whitespace().next().unwrap())
            .collect();
        assert_eq!(names, ["amy", "mid", "zed", "bob", "yan"], "{out}");
    }

    #[test]
    fn agent_text_is_neutralised_and_owners_show_the_name() {
        let mut a = agent("a-1", "ana", None);
        a.title = "x\u{1b}[31mred".into();
        let out = render_agents(&[a.clone()], &[]);
        assert!(!out.contains('\u{1b}'), "{out}");

        let mut snap = snap();
        snap.agents = vec![a.clone()];
        snap.tasks[0].owner = Some(Owner::Agent { id: "a-1".into() });
        let out = render_list(&snap, &ListFilter::default());
        assert!(out.contains("t-1  todo  agent:ana  title t-1"), "{out}");
        let out = render_task(&snap.tasks[0], &[a]);
        assert!(out.contains("owner:    agent:ana"), "{out}");
        // A removed agent's id still shows, not nothing.
        let out = render_task(&snap.tasks[0], &[]);
        assert!(out.contains("owner:    agent:a-1"), "{out}");
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
            agents: vec![],
            workspaces: vec![],
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
        let out = render_task(&t, &[]);
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
        let out = render_task(&t, &[]);
        assert!(
            out.contains("agent  run m-2 failed: it broke, —\n"),
            "{out}"
        );
    }
}
