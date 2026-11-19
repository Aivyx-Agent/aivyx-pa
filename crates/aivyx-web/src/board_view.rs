//! A20 — the Studio's Board view: four columns, filters, and a task panel.

use aivyx_ipc::board::{
    Agent, Author, BoardSnapshot, EntryKind, NewAgent, NewTask, Owner, RunOutcome, Task,
    TaskState, WorksAs,
};
use aivyx_ipc::protocol::{FrontendMessage, QueryPayload};
use dioxus::prelude::*;

use crate::Sender;

pub const COLUMNS: [TaskState; 4] = [
    TaskState::Todo,
    TaskState::Running,
    TaskState::Review,
    TaskState::Done,
];

pub fn column_title(s: TaskState) -> &'static str {
    match s {
        TaskState::Todo => "To do",
        TaskState::Running => "Running",
        TaskState::Review => "Review",
        TaskState::Done => "Done",
        TaskState::Cancelled => "Cancelled",
    }
}

/// Tasks in `state`, filtered by goal and/or project, newest first.
pub fn tasks_in<'a>(
    snap: &'a BoardSnapshot,
    state: TaskState,
    goal: Option<&str>,
    project: Option<&str>,
) -> Vec<&'a Task> {
    let mut v: Vec<&Task> = snap
        .tasks
        .iter()
        .filter(|t| t.state == state)
        .filter(|t| project.is_none_or(|p| t.project_id == p))
        .filter(|t| {
            goal.is_none_or(|g| {
                snap.projects
                    .iter()
                    .any(|p| p.id == t.project_id && p.goal_id == g)
            })
        })
        .collect();
    v.sort_by_key(|t| std::cmp::Reverse(t.updated_at));
    v
}

pub fn review_count(snap: &BoardSnapshot) -> usize {
    snap.tasks
        .iter()
        .filter(|t| t.state == TaskState::Review)
        .count()
}

pub fn owner_label(o: Option<&Owner>, agents: &[Agent]) -> String {
    match o {
        None => "unassigned".into(),
        Some(Owner::Role { name }) => format!("role {name}"),
        Some(Owner::Team) => "team".into(),
        Some(Owner::Agent { id }) => format!("agent {}", agent_name(agents, id)),
    }
}

/// An agent's name by id; an unknown id shows as itself.
pub fn agent_name(agents: &[Agent], id: &str) -> String {
    agents
        .iter()
        .find(|a| a.id == id)
        .map_or_else(|| id.to_string(), |a| a.name.clone())
}

/// Tasks an agent still has to work on: To do, Running or Review.
pub fn open_count(tasks: &[Task], agent_id: &str) -> usize {
    tasks
        .iter()
        .filter(|t| matches!(&t.owner, Some(Owner::Agent { id }) if id == agent_id))
        .filter(|t| {
            matches!(
                t.state,
                TaskState::Todo | TaskState::Running | TaskState::Review
            )
        })
        .count()
}

/// The org chart, depth first: `(depth, agent, in_loop)`. Roots (no
/// reports-to, or one that names no agent) and children are in name order.
/// Every agent shows once, so a stored reports-to loop cannot recurse; the
/// agents a loop leaves unreached follow as extra roots with `in_loop` set.
pub fn agent_tree(agents: &[Agent]) -> Vec<(usize, &Agent, bool)> {
    fn walk<'a>(
        agent: &'a Agent,
        agents: &'a [Agent],
        depth: usize,
        in_loop: bool,
        seen: &mut Vec<&'a str>,
        out: &mut Vec<(usize, &'a Agent, bool)>,
    ) {
        if seen.contains(&agent.id.as_str()) {
            return;
        }
        seen.push(&agent.id);
        out.push((depth, agent, in_loop));
        let mut kids: Vec<&Agent> = agents
            .iter()
            .filter(|a| a.reports_to.as_deref() == Some(agent.id.as_str()))
            .collect();
        kids.sort_by(|a, b| a.name.cmp(&b.name));
        for k in kids {
            walk(k, agents, depth + 1, in_loop, seen, out);
        }
    }
    let mut seen: Vec<&str> = Vec::new();
    let mut out = Vec::new();
    let mut roots: Vec<&Agent> = agents
        .iter()
        .filter(|a| {
            !a.reports_to
                .as_ref()
                .is_some_and(|b| agents.iter().any(|x| x.id == *b))
        })
        .collect();
    roots.sort_by(|a, b| a.name.cmp(&b.name));
    for r in roots {
        walk(r, agents, 0, false, &mut seen, &mut out);
    }
    let mut rest: Vec<&Agent> = agents
        .iter()
        .filter(|a| !seen.contains(&a.id.as_str()))
        .collect();
    rest.sort_by(|a, b| a.name.cmp(&b.name));
    for a in rest {
        walk(a, agents, 0, true, &mut seen, &mut out);
    }
    out
}

/// "—", or the cron pattern.
pub fn heartbeat_label(a: &Agent) -> String {
    a.heartbeat.clone().unwrap_or_else(|| "—".into())
}

/// "role coder" or "team".
pub fn works_as_label(w: &WorksAs) -> String {
    match w {
        WorksAs::Role { name } => format!("role {name}"),
        WorksAs::Team => "team".into(),
    }
}

/// The agent form's payload. On an edit an empty heartbeat or reports-to
/// clears the field; on a create it is just left out.
#[allow(clippy::too_many_arguments)]
pub fn agent_payload(
    name: &str,
    title: &str,
    works: &str,
    role_name: &str,
    reports_to: &str,
    heartbeat: &str,
    wake_on_assign: bool,
    editing: bool,
) -> NewAgent {
    let works_as = if works == "role" {
        WorksAs::Role {
            name: role_name.trim().into(),
        }
    } else {
        WorksAs::Team
    };
    let heartbeat = heartbeat.trim();
    NewAgent {
        name: Some(name.trim().into()),
        title: Some(title.trim().into()),
        reports_to: (!reports_to.is_empty()).then(|| reports_to.to_string()),
        clear_reports_to: editing && reports_to.is_empty(),
        works_as: Some(works_as),
        heartbeat: (!heartbeat.is_empty()).then(|| heartbeat.to_string()),
        clear_heartbeat: editing && heartbeat.is_empty(),
        wake_on_assign: Some(wake_on_assign),
    }
}

/// One line for a thread entry.
pub fn entry_line(e: &EntryKind) -> String {
    match e {
        EntryKind::Comment { text } => text.clone(),
        EntryKind::StateChange { from, to } => {
            format!("{} → {}", column_title(*from), column_title(*to))
        }
        EntryKind::Run {
            mission_id,
            outcome,
            answer,
            tokens,
            cost_usd,
        } => {
            // The answer is what the operator reviews; the mission id finds
            // the run under Missions.
            // No tokens and no cost: unmetered (a team run without a
            // mission budget counts nothing), not free.
            let spend = match (tokens, cost_usd) {
                (0, None) => "—".to_string(),
                (n, None) => format!("{n} tokens"),
                (n, Some(c)) => format!("{n} tokens, ${c:.2}"),
            };
            let outcome = match outcome {
                RunOutcome::Succeeded => "succeeded".to_string(),
                RunOutcome::Failed { reason } => format!("failed: {reason}"),
                RunOutcome::Cancelled => "cancelled".into(),
                RunOutcome::Interrupted => "interrupted".into(),
            };
            let answer = if answer.is_empty() {
                String::new()
            } else {
                format!("\n{answer}")
            };
            format!("run {mission_id} {outcome} ({spend}){answer}")
        }
    }
}

/// "you" for the operator, "agent" for a run.
pub fn author_label(a: &Author) -> &'static str {
    match a {
        Author::Operator => "you",
        Author::Agent { .. } => "agent",
    }
}

/// The owner picker's choice for an owner: `""`, `"team"`, `"role"` or
/// `"agent:<id>"`.
pub fn owner_choice(o: Option<&Owner>) -> String {
    match o {
        None => String::new(),
        Some(Owner::Team) => "team".into(),
        Some(Owner::Role { .. }) => "role".into(),
        Some(Owner::Agent { id }) => format!("agent:{id}"),
    }
}

/// Builds an owner from the picker's choice and the typed role name. A role
/// choice with an empty name is no owner.
pub fn owner_from_choice(choice: &str, role_name: &str) -> Option<Owner> {
    match choice {
        "team" => Some(Owner::Team),
        "role" => {
            let name = role_name.trim();
            (!name.is_empty()).then(|| Owner::Role { name: name.into() })
        }
        other => other
            .strip_prefix("agent:")
            .filter(|id| !id.is_empty())
            .map(|id| Owner::Agent { id: id.into() }),
    }
}

/// Start: Todo with an owner.
pub fn can_start(t: &Task) -> bool {
    t.state == TaskState::Todo && t.owner.is_some()
}

/// Approve and Send back: Review.
pub fn can_review(t: &Task) -> bool {
    t.state == TaskState::Review
}

/// Cancel: not terminal.
pub fn can_cancel(t: &Task) -> bool {
    !t.state.is_terminal()
}

/// The Board screen's state: the last snapshot plus the view's own selection.
#[derive(Default, Clone, PartialEq)]
pub struct BoardUi {
    pub snapshot: BoardSnapshot,
    pub loaded: bool,
    pub goal: Option<String>,
    pub project: Option<String>,
    pub selected: Option<String>,
    pub error: Option<String>,
    /// A one-line confirmation, e.g. "Wake requested".
    pub notice: Option<String>,
}

impl BoardUi {
    /// A snapshot arrived. It never clears an error: the one that follows a
    /// failed action would wipe it before the operator sees it.
    pub fn on_snapshot(&mut self, snapshot: BoardSnapshot) {
        self.snapshot = snapshot;
        self.loaded = true;
    }

    /// The operator started a new action, so the last error is stale.
    pub fn begin_action(&mut self) {
        self.error = None;
        self.notice = None;
    }

    /// A wake request was accepted; the wake itself runs on the daemon's
    /// next tick.
    pub fn on_woke(&mut self) {
        self.notice = Some("Wake requested".into());
    }

    /// A `board-*` query failed.
    pub fn on_error(&mut self, message: String) {
        self.error = Some(message);
    }
}

pub fn snapshot_query() -> FrontendMessage {
    FrontendMessage::Query {
        id: "board-snapshot".into(),
        payload: QueryPayload::BoardSnapshot,
    }
}

/// Sends one action. A new action clears the last error; the refresh comes
/// from the daemon's Ack/Created/Started reply (see `read_task`).
fn act(ws: Sender, mut ui: Signal<BoardUi>, id: &str, payload: QueryPayload) {
    ui.write().begin_action();
    ws.send(FrontendMessage::Query {
        id: format!("board-{id}"),
        payload,
    });
}

fn opt(s: String) -> Option<String> {
    (!s.is_empty()).then_some(s)
}

#[derive(Clone, Copy, PartialEq)]
enum BoardTab {
    Tasks,
    Agents,
}

#[component]
pub fn BoardPanel() -> Element {
    let ws = use_context::<Sender>();
    let mut ui = use_context::<Signal<BoardUi>>();
    let mut tab = use_signal(|| BoardTab::Tasks);

    // Open on a fresh snapshot (the App poll keeps it fresh after).
    use_effect(move || {
        ws.send(snapshot_query());
    });

    let s = ui();
    let t = tab();

    rsx! {
        div { class: "settings",
            div { class: "panel-head",
                h3 { "Board" }
                if s.loaded {
                    span { class: "label-tech", "{s.snapshot.tasks.len()} tasks · {s.snapshot.agents.len()} agents" }
                }
                button { class: "btn-ghost", onclick: move |_| { ws.send(snapshot_query()); }, "Refresh" }
            }
            if let Some(err) = s.error.clone() {
                div { class: "notice error", role: "alert",
                    "{err} "
                    button { class: "btn-ghost", onclick: move |_| ui.write().error = None, "Dismiss" }
                }
            }
            if let Some(n) = s.notice.clone() {
                div { class: "notice", role: "status", "{n}" }
            }
            div { role: "tablist", style: "display:flex; gap:8px; margin-bottom:12px;",
                button {
                    class: if t == BoardTab::Tasks { "btn" } else { "btn-ghost" },
                    role: "tab",
                    "aria-selected": "{t == BoardTab::Tasks}",
                    onclick: move |_| tab.set(BoardTab::Tasks),
                    "Tasks"
                }
                button {
                    class: if t == BoardTab::Agents { "btn" } else { "btn-ghost" },
                    role: "tab",
                    "aria-selected": "{t == BoardTab::Agents}",
                    onclick: move |_| tab.set(BoardTab::Agents),
                    "Agents"
                }
            }
            if t == BoardTab::Tasks {
                TasksTab {}
            } else {
                AgentsTab {}
            }
        }
    }
}

#[component]
fn TasksTab() -> Element {
    let mut ui = use_context::<Signal<BoardUi>>();
    let s = ui();
    let goal = s.goal.clone();
    let project = s.project.clone();
    let projects_shown: Vec<_> = s
        .snapshot
        .projects
        .iter()
        .filter(|p| goal.as_deref().is_none_or(|g| p.goal_id == g))
        .cloned()
        .collect();
    let selected_task = s
        .selected
        .as_ref()
        .and_then(|id| s.snapshot.tasks.iter().find(|t| &t.id == id))
        .cloned();

    rsx! {
            div { style: "display:flex; gap:8px; flex-wrap:wrap; margin-bottom:12px;",
                select {
                    class: "input",
                    "aria-label": "Filter by goal",
                    onchange: move |e| {
                        let mut u = ui.write();
                        u.goal = opt(e.value());
                        u.project = None;
                    },
                    option { value: "", selected: goal.is_none(), "All goals" }
                    for g in s.snapshot.goals.iter() {
                        option { key: "{g.id}", value: "{g.id}", selected: goal.as_deref() == Some(g.id.as_str()), "{g.title}" }
                    }
                }
                select {
                    class: "input",
                    "aria-label": "Filter by project",
                    onchange: move |e| ui.write().project = opt(e.value()),
                    option { value: "", selected: project.is_none(), "All projects" }
                    for p in projects_shown.iter() {
                        option { key: "{p.id}", value: "{p.id}", selected: project.as_deref() == Some(p.id.as_str()), "{p.title}" }
                    }
                }
            }
            if !s.loaded {
                div { class: "glass-card empty", p { class: "help", "Loading the board…" } }
            } else {
                div { style: "display:grid; grid-template-columns:repeat(4,minmax(160px,1fr)); gap:12px; overflow-x:auto;",
                    for col in COLUMNS {
                        {
                            let tasks = tasks_in(&s.snapshot, col, goal.as_deref(), project.as_deref());
                            rsx! {
                                div { key: "{column_title(col)}", class: "glass-card", style: "min-height:120px;",
                                    div { class: "label-tech", "{column_title(col)} · {tasks.len()}" }
                                    for t in tasks {
                                        {
                                            let id = t.id.clone();
                                            let is_sel = s.selected.as_deref() == Some(t.id.as_str());
                                            let owner = owner_label(t.owner.as_ref(), &s.snapshot.agents);
                                            rsx! {
                                                button {
                                                    key: "{t.id}",
                                                    class: if is_sel { "btn" } else { "btn-ghost" },
                                                    style: "display:block; width:100%; text-align:left; margin-top:8px;",
                                                    onclick: move |_| ui.write().selected = Some(id.clone()),
                                                    div { "{t.title}" }
                                                    div { class: "help", "{owner} · {column_title(t.state)}" }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if let Some(task) = selected_task {
                {
                    let key = task.id.clone();
                    rsx! { TaskPanel { key: "{key}", task } }
                }
            }
            div { style: "display:grid; grid-template-columns:repeat(auto-fit,minmax(260px,1fr)); gap:12px; margin-top:16px;",
                NewTaskForm {}
                NewGoalForm {}
                NewProjectForm {}
            }
    }
}

#[component]
fn TaskPanel(task: Task) -> Element {
    let ws = use_context::<Sender>();
    let mut ui = use_context::<Signal<BoardUi>>();
    let mut choice = use_signal(|| owner_choice(task.owner.as_ref()));
    let agents = ui().snapshot.agents;
    let mut role_name = use_signal(|| match &task.owner {
        Some(Owner::Role { name }) => name.clone(),
        _ => String::new(),
    });
    let mut sending_back = use_signal(|| false);
    let mut why = use_signal(String::new);
    let mut comment = use_signal(String::new);

    let tid = task.id.clone();
    let (t_assign, t_start, t_approve, t_back, t_cancel, t_comment) = (
        tid.clone(),
        tid.clone(),
        tid.clone(),
        tid.clone(),
        tid.clone(),
        tid.clone(),
    );
    let editable = !task.state.is_terminal() && task.state != TaskState::Running;
    let c = choice();

    rsx! {
        div { class: "glass-card", style: "margin-top:16px;",
            div { class: "panel-head",
                h4 { "{task.title}" }
                span { class: "label-tech", "{column_title(task.state)} · {owner_label(task.owner.as_ref(), &agents)}" }
                button { class: "btn-ghost", onclick: move |_| ui.write().selected = None, "Close" }
            }
            if !task.description.is_empty() {
                p { "{task.description}" }
            }
            if editable {
                div { style: "display:flex; gap:8px; flex-wrap:wrap; align-items:center;",
                    label { class: "field-label", "Owner" }
                    select {
                        class: "input",
                        onchange: move |e| choice.set(e.value()),
                        option { value: "", selected: c.is_empty(), "unassigned" }
                        option { value: "team", selected: c == "team", "team" }
                        option { value: "role", selected: c == "role", "role…" }
                        for a in agents.iter() {
                            option { key: "{a.id}", value: "agent:{a.id}", selected: c == format!("agent:{}", a.id), "agent {a.name}" }
                        }
                    }
                    if c == "role" {
                        input {
                            class: "input",
                            placeholder: "role name",
                            value: "{role_name}",
                            oninput: move |e| role_name.set(e.value()),
                        }
                    }
                    button {
                        class: "btn-ghost",
                        disabled: c == "role" && role_name().trim().is_empty(),
                        onclick: move |_| act(ws, ui, "assign", QueryPayload::BoardAssign {
                            task_id: t_assign.clone(),
                            owner: owner_from_choice(&choice(), &role_name()),
                        }),
                        "Set owner"
                    }
                }
            }
            div { style: "display:flex; gap:8px; flex-wrap:wrap; margin:8px 0;",
                if can_start(&task) {
                    button { class: "btn", onclick: move |_| act(ws, ui, "start", QueryPayload::BoardStart { task_id: t_start.clone() }), "Start" }
                }
                if can_review(&task) {
                    button { class: "btn", onclick: move |_| act(ws, ui, "approve", QueryPayload::BoardApprove { task_id: t_approve.clone() }), "Approve" }
                    button { class: "btn-ghost", onclick: move |_| sending_back.toggle(), "Send back" }
                }
                if can_cancel(&task) {
                    button { class: "btn-danger", onclick: move |_| act(ws, ui, "cancel", QueryPayload::BoardCancel { task_id: t_cancel.clone() }), "Cancel" }
                }
            }
            if sending_back() && can_review(&task) {
                div {
                    textarea {
                        class: "input",
                        placeholder: "What needs to change?",
                        value: "{why}",
                        oninput: move |e| why.set(e.value()),
                    }
                    button {
                        class: "btn",
                        disabled: why().trim().is_empty(),
                        onclick: move |_| {
                            act(ws, ui, "send-back", QueryPayload::BoardSendBack {
                                task_id: t_back.clone(),
                                why: why().trim().to_string(),
                            });
                            why.set(String::new());
                            sending_back.set(false);
                        },
                        "Send back with this note"
                    }
                }
            }
            div { class: "label-tech", "Thread" }
            if task.thread.is_empty() {
                p { class: "help", "Nothing here yet." }
            }
            for (i, e) in task.thread.iter().enumerate() {
                div { key: "{i}", class: "help", style: "white-space:pre-wrap;", "{author_label(&e.author)}: {entry_line(&e.entry)}" }
            }
            div { style: "display:flex; gap:8px; margin-top:8px;",
                input {
                    class: "input",
                    placeholder: "Add a comment",
                    value: "{comment}",
                    oninput: move |e| comment.set(e.value()),
                }
                button {
                    class: "btn-ghost",
                    disabled: comment().trim().is_empty(),
                    onclick: move |_| {
                        act(ws, ui, "comment", QueryPayload::BoardComment {
                            task_id: t_comment.clone(),
                            text: comment().trim().to_string(),
                        });
                        comment.set(String::new());
                    },
                    "Comment"
                }
            }
        }
    }
}

#[component]
fn NewTaskForm() -> Element {
    let ws = use_context::<Sender>();
    let ui = use_context::<Signal<BoardUi>>();
    let mut title = use_signal(String::new);
    let mut description = use_signal(String::new);
    let mut project = use_signal(String::new);
    let mut choice = use_signal(String::new);
    let mut role_name = use_signal(String::new);
    let mut review = use_signal(|| true);
    let projects = ui().snapshot.projects;
    let agents = ui().snapshot.agents;
    // Default to the filtered project, else the first one.
    let chosen = if project().is_empty() {
        ui().project
            .or_else(|| projects.first().map(|p| p.id.clone()))
            .unwrap_or_default()
    } else {
        project()
    };
    let c = choice();
    let ready = !title().trim().is_empty() && !chosen.is_empty();
    let chosen_send = chosen.clone();

    rsx! {
        div { class: "glass-card",
            h4 { "New task" }
            if projects.is_empty() {
                p { class: "help", "Create a goal and a project first." }
            }
            input { class: "input", placeholder: "Title", value: "{title}", oninput: move |e| title.set(e.value()) }
            textarea { class: "input", placeholder: "Description", value: "{description}", oninput: move |e| description.set(e.value()) }
            select {
                class: "input",
                "aria-label": "Project",
                onchange: move |e| project.set(e.value()),
                for p in projects.iter() {
                    option { key: "{p.id}", value: "{p.id}", selected: chosen == p.id, "{p.title}" }
                }
            }
            select {
                class: "input",
                "aria-label": "Owner",
                onchange: move |e| choice.set(e.value()),
                option { value: "", selected: c.is_empty(), "unassigned" }
                option { value: "team", selected: c == "team", "team" }
                option { value: "role", selected: c == "role", "role…" }
                for a in agents.iter() {
                    option { key: "{a.id}", value: "agent:{a.id}", selected: c == format!("agent:{}", a.id), "agent {a.name}" }
                }
            }
            if c == "role" {
                input { class: "input", placeholder: "role name", value: "{role_name}", oninput: move |e| role_name.set(e.value()) }
            }
            label { class: "field-label",
                input { r#type: "checkbox", checked: review(), onchange: move |e| review.set(e.checked()) }
                " Review before done"
            }
            button {
                class: "btn",
                disabled: !ready,
                onclick: move |_| {
                    act(ws, ui, "create-task", QueryPayload::BoardCreateTask {
                        task: NewTask {
                            project_id: chosen_send.clone(),
                            parent_task: None,
                            title: title().trim().to_string(),
                            description: description(),
                            owner: owner_from_choice(&choice(), &role_name()),
                            review_required: review(),
                        },
                    });
                    title.set(String::new());
                    description.set(String::new());
                },
                "Add task"
            }
        }
    }
}

#[component]
fn NewGoalForm() -> Element {
    let ws = use_context::<Sender>();
    let ui = use_context::<Signal<BoardUi>>();
    let mut title = use_signal(String::new);
    let mut description = use_signal(String::new);
    rsx! {
        div { class: "glass-card",
            h4 { "New goal" }
            input { class: "input", placeholder: "Title", value: "{title}", oninput: move |e| title.set(e.value()) }
            textarea { class: "input", placeholder: "Description", value: "{description}", oninput: move |e| description.set(e.value()) }
            button {
                class: "btn",
                disabled: title().trim().is_empty(),
                onclick: move |_| {
                    act(ws, ui, "create-goal", QueryPayload::BoardCreateGoal {
                        title: title().trim().to_string(),
                        description: description(),
                    });
                    title.set(String::new());
                    description.set(String::new());
                },
                "Add goal"
            }
        }
    }
}

#[component]
fn NewProjectForm() -> Element {
    let ws = use_context::<Sender>();
    let ui = use_context::<Signal<BoardUi>>();
    let mut title = use_signal(String::new);
    let mut description = use_signal(String::new);
    let mut goal = use_signal(String::new);
    let goals = ui().snapshot.goals;
    let chosen = if goal().is_empty() {
        ui().goal
            .or_else(|| goals.first().map(|g| g.id.clone()))
            .unwrap_or_default()
    } else {
        goal()
    };
    let chosen_send = chosen.clone();
    rsx! {
        div { class: "glass-card",
            h4 { "New project" }
            if goals.is_empty() {
                p { class: "help", "Create a goal first." }
            }
            select {
                class: "input",
                "aria-label": "Goal",
                onchange: move |e| goal.set(e.value()),
                for g in goals.iter() {
                    option { key: "{g.id}", value: "{g.id}", selected: chosen == g.id, "{g.title}" }
                }
            }
            input { class: "input", placeholder: "Title", value: "{title}", oninput: move |e| title.set(e.value()) }
            textarea { class: "input", placeholder: "Description", value: "{description}", oninput: move |e| description.set(e.value()) }
            button {
                class: "btn",
                disabled: title().trim().is_empty() || chosen.is_empty(),
                onclick: move |_| {
                    act(ws, ui, "create-project", QueryPayload::BoardCreateProject {
                        goal_id: chosen_send.clone(),
                        title: title().trim().to_string(),
                        description: description(),
                    });
                    title.set(String::new());
                    description.set(String::new());
                },
                "Add project"
            }
        }
    }
}

#[component]
fn AgentsTab() -> Element {
    let ws = use_context::<Sender>();
    let ui = use_context::<Signal<BoardUi>>();
    // The agent being edited, by name; `None` is the add form.
    let mut editing = use_signal(|| None::<String>);
    let s = ui();
    let now = js_sys::Date::now() as i64 / 1000;
    let rows = agent_tree(&s.snapshot.agents);
    let edit_target = editing()
        .and_then(|n| s.snapshot.agents.iter().find(|a| a.name == n))
        .cloned();

    rsx! {
        if !s.loaded {
            div { class: "glass-card empty", p { class: "help", "Loading the board…" } }
        } else if rows.is_empty() {
            div { class: "glass-card empty",
                p { class: "help", "No agents yet. Add one below: a standing agent wakes on a schedule or when a task is assigned." }
            }
        } else {
            div { class: "glass-card",
                for (depth, a, in_loop) in rows {
                    {
                        let (n_pause, n_wake, n_edit) = (a.name.clone(), a.name.clone(), a.name.clone());
                        let paused = a.paused;
                        let open = open_count(&s.snapshot.tasks, &a.id);
                        let last = a.last_wake.map_or_else(
                            || "never".to_string(),
                            |ms| crate::command_center::ago((ms / 1000) as i64, now),
                        );
                        let pad = depth * 24;
                        rsx! {
                            div {
                                key: "{a.id}",
                                role: "treeitem",
                                style: "margin-left:{pad}px; padding:8px 0; border-bottom:1px solid var(--border, rgba(128,128,128,.2));",
                                div { style: "display:flex; gap:8px; flex-wrap:wrap; align-items:center;",
                                    strong { "{a.name}" }
                                    if !a.title.is_empty() {
                                        span { class: "help", "{a.title}" }
                                    }
                                    if paused {
                                        span { class: "label-tech", "paused" }
                                    }
                                    if in_loop {
                                        span { class: "label-tech", "(reports-to loop)" }
                                    }
                                }
                                div { class: "help",
                                    "works as {works_as_label(&a.works_as)} · heartbeat {heartbeat_label(a)} · last wake {last} · {open} open"
                                }
                                div { style: "display:flex; gap:8px; margin-top:4px;",
                                    button {
                                        class: "btn-ghost",
                                        onclick: move |_| {
                                            let name = n_pause.clone();
                                            act(ws, ui, "pause-agent", if paused {
                                                QueryPayload::BoardResumeAgent { name }
                                            } else {
                                                QueryPayload::BoardPauseAgent { name }
                                            });
                                        },
                                        if paused { "Resume" } else { "Pause" }
                                    }
                                    button {
                                        class: "btn-ghost",
                                        disabled: paused,
                                        onclick: move |_| act(ws, ui, "wake-agent", QueryPayload::BoardWakeAgent { name: n_wake.clone() }),
                                        "Wake now"
                                    }
                                    button {
                                        class: "btn-ghost",
                                        onclick: move |_| editing.set(Some(n_edit.clone())),
                                        "Edit"
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        {
            let key = edit_target.as_ref().map_or_else(|| "new".to_string(), |a| a.id.clone());
            rsx! {
                AgentForm {
                    key: "{key}",
                    agent: edit_target,
                    editing_name: editing,
                }
            }
        }
    }
}

/// Adds an agent, or edits `agent`.
#[component]
fn AgentForm(agent: Option<Agent>, editing_name: Signal<Option<String>>) -> Element {
    let ws = use_context::<Sender>();
    let ui = use_context::<Signal<BoardUi>>();
    let mut editing_name = editing_name;
    let mut name = use_signal(|| agent.as_ref().map(|a| a.name.clone()).unwrap_or_default());
    let mut title = use_signal(|| agent.as_ref().map(|a| a.title.clone()).unwrap_or_default());
    let mut works = use_signal(|| match agent.as_ref().map(|a| &a.works_as) {
        Some(WorksAs::Role { .. }) => "role".to_string(),
        _ => "team".to_string(),
    });
    let mut role_name = use_signal(|| match agent.as_ref().map(|a| &a.works_as) {
        Some(WorksAs::Role { name }) => name.clone(),
        _ => String::new(),
    });
    let mut reports_to = use_signal(|| {
        agent
            .as_ref()
            .and_then(|a| a.reports_to.clone())
            .unwrap_or_default()
    });
    let mut heartbeat = use_signal(|| {
        agent
            .as_ref()
            .and_then(|a| a.heartbeat.clone())
            .unwrap_or_default()
    });
    let mut wake = use_signal(|| agent.as_ref().is_some_and(|a| a.wake_on_assign));
    let agents = ui().snapshot.agents;
    let old_name = agent.as_ref().map(|a| a.name.clone());
    let own_id = agent.as_ref().map(|a| a.id.clone());
    let w = works();
    let ready = !name().trim().is_empty() && (w != "role" || !role_name().trim().is_empty());
    let heading = if old_name.is_some() { "Edit agent" } else { "Add agent" };
    let submit = if old_name.is_some() { "Save agent" } else { "Add agent" };
    let editing = old_name.is_some();

    rsx! {
        div { class: "glass-card", style: "margin-top:16px;",
            h4 { "{heading}" }
            input { class: "input", placeholder: "Name (letters, digits, - and _)", value: "{name}", oninput: move |e| name.set(e.value()) }
            input { class: "input", placeholder: "Title", value: "{title}", oninput: move |e| title.set(e.value()) }
            div { style: "display:flex; gap:8px; flex-wrap:wrap;",
                select {
                    class: "input",
                    "aria-label": "Works as",
                    onchange: move |e| works.set(e.value()),
                    option { value: "team", selected: w == "team", "team" }
                    option { value: "role", selected: w == "role", "role…" }
                }
                if w == "role" {
                    input { class: "input", placeholder: "role name", value: "{role_name}", oninput: move |e| role_name.set(e.value()) }
                }
            }
            select {
                class: "input",
                "aria-label": "Reports to",
                onchange: move |e| reports_to.set(e.value()),
                option { value: "", selected: reports_to().is_empty(), "reports to nobody" }
                for a in agents.iter().filter(|a| own_id.as_deref() != Some(a.id.as_str())) {
                    option { key: "{a.id}", value: "{a.id}", selected: reports_to() == a.id, "reports to {a.name}" }
                }
            }
            input { class: "input", placeholder: HEARTBEAT_PLACEHOLDER, value: "{heartbeat}", oninput: move |e| heartbeat.set(e.value()) }
            label { class: "field-label",
                input { r#type: "checkbox", checked: wake(), onchange: move |e| wake.set(e.checked()) }
                " Wake when a task is assigned"
            }
            div { style: "display:flex; gap:8px;",
                button {
                    class: "btn",
                    disabled: !ready,
                    onclick: move |_| {
                        let payload = agent_payload(
                            &name(), &title(), &works(), &role_name(), &reports_to(), &heartbeat(), wake(), editing,
                        );
                        match old_name.clone() {
                            Some(old) => act(ws, ui, "edit-agent", QueryPayload::BoardEditAgent { name: old, agent: payload }),
                            None => {
                                act(ws, ui, "create-agent", QueryPayload::BoardCreateAgent { agent: payload });
                                name.set(String::new());
                                title.set(String::new());
                                heartbeat.set(String::new());
                            }
                        }
                        editing_name.set(None);
                    },
                    "{submit}"
                }
                if editing {
                    button { class: "btn-ghost", onclick: move |_| editing_name.set(None), "Cancel" }
                }
            }
        }
    }
}

/// The heartbeat field's hint. Cron patterns start with seconds (six
/// fields), as for routines; a five-field Unix pattern is refused.
const HEARTBEAT_PLACEHOLDER: &str = "Heartbeat (cron, seconds first, e.g. 0 0 9 * * *); empty for none";

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_ipc::board::{Goal, Project};

    fn task(id: &str, project: &str, state: TaskState, updated: u64) -> Task {
        Task {
            id: id.into(),
            project_id: project.into(),
            parent_task: None,
            title: id.into(),
            description: String::new(),
            owner: None,
            review_required: true,
            state,
            thread: vec![],
            active_run: None,
            created_at: 0,
            updated_at: updated,
        }
    }

    fn proj(id: &str, goal: &str) -> Project {
        Project {
            id: id.into(),
            goal_id: goal.into(),
            title: id.into(),
            description: String::new(),
            created_at: 0,
            updated_at: 0,
        }
    }

    fn snap() -> BoardSnapshot {
        BoardSnapshot {
            agents: vec![],
            goals: vec![Goal {
                id: "g1".into(),
                title: "G1".into(),
                description: String::new(),
                created_at: 0,
                updated_at: 0,
            }],
            projects: vec![proj("p1", "g1"), proj("p2", "g2")],
            tasks: vec![
                task("a", "p1", TaskState::Todo, 1),
                task("b", "p1", TaskState::Todo, 5),
                task("c", "p2", TaskState::Todo, 3),
                task("d", "p2", TaskState::Review, 2),
                task("e", "p1", TaskState::Review, 2),
            ],
        }
    }

    fn ids(v: Vec<&Task>) -> Vec<&str> {
        v.into_iter().map(|t| t.id.as_str()).collect()
    }

    #[test]
    fn filters_by_goal_and_project() {
        let s = snap();
        assert_eq!(
            ids(tasks_in(&s, TaskState::Todo, None, None)),
            ["b", "c", "a"]
        );
        assert_eq!(
            ids(tasks_in(&s, TaskState::Todo, Some("g1"), None)),
            ["b", "a"]
        );
        assert_eq!(ids(tasks_in(&s, TaskState::Todo, Some("g2"), None)), ["c"]);
        assert_eq!(ids(tasks_in(&s, TaskState::Todo, None, Some("p2"))), ["c"]);
        assert!(tasks_in(&s, TaskState::Todo, Some("g1"), Some("p2")).is_empty());
        assert!(tasks_in(&s, TaskState::Done, None, None).is_empty());
    }

    #[test]
    fn an_error_survives_the_following_snapshot() {
        let mut ui = BoardUi::default();
        ui.begin_action();
        ui.on_error("board_no_owner".into());
        ui.on_snapshot(snap());
        assert!(ui.loaded);
        assert_eq!(ui.error.as_deref(), Some("board_no_owner"));
        ui.begin_action();
        assert_eq!(ui.error, None);
    }

    #[test]
    fn counts_review() {
        assert_eq!(review_count(&snap()), 2);
        assert_eq!(review_count(&BoardSnapshot::default()), 0);
    }

    #[test]
    fn labels_owners() {
        assert_eq!(owner_label(None, &[]), "unassigned");
        assert_eq!(owner_label(Some(&Owner::Team), &[]), "team");
        assert_eq!(
            owner_label(
                Some(&Owner::Role {
                    name: "coder".into()
                }),
                &[]
            ),
            "role coder"
        );
        let agents = [agent("a-1", "ana", None)];
        let owned = Owner::Agent { id: "a-1".into() };
        assert_eq!(owner_label(Some(&owned), &agents), "agent ana");
        assert_eq!(owner_label(Some(&owned), &[]), "agent a-1");
    }

    #[test]
    fn owner_picker_round_trips() {
        assert_eq!(owner_from_choice("", "x"), None);
        assert_eq!(owner_from_choice("team", ""), Some(Owner::Team));
        assert_eq!(owner_from_choice("role", "  "), None);
        assert_eq!(
            owner_from_choice("role", " dev "),
            Some(Owner::Role { name: "dev".into() })
        );
        assert_eq!(owner_choice(None), "");
        assert_eq!(owner_choice(Some(&Owner::Team)), "team");
        assert_eq!(owner_from_choice("agent:", ""), None);
        assert_eq!(
            owner_choice(Some(&Owner::Role { name: "a".into() })),
            "role"
        );
    }

    #[test]
    fn an_agent_owner_survives_the_picker() {
        let owner = Owner::Agent { id: "a-1".into() };
        let choice = owner_choice(Some(&owner));
        assert_eq!(choice, "agent:a-1");
        assert_eq!(owner_from_choice(&choice, ""), Some(owner));
    }

    fn agent(id: &str, name: &str, reports_to: Option<&str>) -> Agent {
        Agent {
            id: id.into(),
            name: name.into(),
            title: String::new(),
            reports_to: reports_to.map(Into::into),
            works_as: WorksAs::Team,
            heartbeat: None,
            wake_on_assign: false,
            paused: false,
            last_wake: None,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn rows(agents: &[Agent]) -> Vec<(usize, String, bool)> {
        agent_tree(agents)
            .into_iter()
            .map(|(d, a, l)| (d, a.name.clone(), l))
            .collect()
    }

    #[test]
    fn the_tree_nests_and_sorts_by_name() {
        let agents = [
            agent("a-1", "zed", None),
            agent("a-2", "mid", None),
            agent("a-3", "amy", Some("a-1")),
            agent("a-4", "bob", Some("a-1")),
            agent("a-5", "orphan", Some("a-gone")),
        ];
        assert_eq!(
            rows(&agents),
            [
                (0, "mid".to_string(), false),
                (0, "orphan".to_string(), false),
                (0, "zed".to_string(), false),
                (1, "amy".to_string(), false),
                (1, "bob".to_string(), false),
            ]
        );
    }

    #[test]
    fn a_stored_loop_renders_each_agent_once_marked() {
        let agents = [
            agent("a-1", "zed", Some("a-2")),
            agent("a-2", "amy", Some("a-1")),
            agent("a-3", "kid", Some("a-1")),
        ];
        let r = rows(&agents);
        assert_eq!(r.len(), 3);
        assert_eq!(r[0], (0, "amy".to_string(), true));
        assert!(r.iter().all(|(_, _, l)| *l));
        assert!(agent_tree(&[]).is_empty());
    }

    #[test]
    fn counts_open_tasks_per_agent() {
        let mut a = task("a", "p", TaskState::Todo, 0);
        a.owner = Some(Owner::Agent { id: "a-1".into() });
        let mut b = task("b", "p", TaskState::Review, 0);
        b.owner = a.owner.clone();
        let mut c = task("c", "p", TaskState::Done, 0);
        c.owner = a.owner.clone();
        let mut d = task("d", "p", TaskState::Running, 0);
        d.owner = Some(Owner::Agent { id: "a-2".into() });
        let tasks = [a, b, c, d, task("e", "p", TaskState::Todo, 0)];
        assert_eq!(open_count(&tasks, "a-1"), 2);
        assert_eq!(open_count(&tasks, "a-2"), 1);
        assert_eq!(open_count(&tasks, "a-9"), 0);
        assert_eq!(agent_name(&[agent("a-1", "ana", None)], "a-1"), "ana");
        assert_eq!(agent_name(&[], "a-1"), "a-1");
    }

    #[test]
    fn the_agent_form_clears_only_on_edit() {
        let create = agent_payload(" ana ", "Lead", "role", " dev ", "", "", true, false);
        assert_eq!(create.name.as_deref(), Some("ana"));
        assert_eq!(create.works_as, Some(WorksAs::Role { name: "dev".into() }));
        assert!(!create.clear_heartbeat && !create.clear_reports_to);
        assert_eq!(create.heartbeat, None);
        let edit = agent_payload("ana", "", "team", "", "", " ", false, true);
        assert_eq!(edit.works_as, Some(WorksAs::Team));
        assert!(edit.clear_heartbeat && edit.clear_reports_to);
        let kept = agent_payload("ana", "", "team", "", "a-2", "0 9 * * *", false, true);
        assert_eq!(kept.reports_to.as_deref(), Some("a-2"));
        assert_eq!(kept.heartbeat.as_deref(), Some("0 9 * * *"));
        assert!(!kept.clear_heartbeat && !kept.clear_reports_to);
    }

    #[test]
    fn describes_entries() {
        assert_eq!(entry_line(&EntryKind::Comment { text: "hi".into() }), "hi");
        assert_eq!(
            entry_line(&EntryKind::StateChange {
                from: TaskState::Todo,
                to: TaskState::Running
            }),
            "To do → Running"
        );
        let run = EntryKind::Run {
            mission_id: "m".into(),
            outcome: RunOutcome::Succeeded,
            answer: String::new(),
            tokens: 120,
            cost_usd: Some(0.5),
        };
        assert_eq!(entry_line(&run), "run m succeeded (120 tokens, $0.50)");
        let answered = EntryKind::Run {
            mission_id: "b-1".into(),
            outcome: RunOutcome::Succeeded,
            answer: "Two lines.".into(),
            tokens: 7,
            cost_usd: None,
        };
        assert_eq!(entry_line(&answered), "run b-1 succeeded (7 tokens)\nTwo lines.");
        let failed = EntryKind::Run {
            mission_id: "m".into(),
            outcome: RunOutcome::Failed {
                reason: "timed out".into(),
            },
            answer: String::new(),
            tokens: 0,
            cost_usd: None,
        };
        assert_eq!(entry_line(&failed), "run m failed: timed out (—)");
        let free = EntryKind::Run {
            mission_id: "m".into(),
            outcome: RunOutcome::Interrupted,
            answer: String::new(),
            tokens: 0,
            cost_usd: None,
        };
        assert_eq!(entry_line(&free), "run m interrupted (—)");
        assert_eq!(author_label(&Author::Operator), "you");
        assert_eq!(
            author_label(&Author::Agent {
                mission_id: "m".into()
            }),
            "agent"
        );
    }

    #[test]
    fn actions_follow_the_state() {
        let mut t = task("a", "p1", TaskState::Todo, 0);
        assert!(!can_start(&t));
        t.owner = Some(Owner::Team);
        assert!(can_start(&t) && can_cancel(&t) && !can_review(&t));
        t.state = TaskState::Review;
        assert!(!can_start(&t) && can_review(&t) && can_cancel(&t));
        t.state = TaskState::Running;
        assert!(!can_start(&t) && can_cancel(&t));
        t.state = TaskState::Done;
        assert!(!can_cancel(&t));
    }

    #[test]
    fn the_heartbeat_hint_is_a_six_field_pattern() {
        let example = HEARTBEAT_PLACEHOLDER
            .split("e.g. ")
            .nth(1)
            .and_then(|r| r.split(')').next())
            .unwrap();
        assert_eq!(example.split_whitespace().count(), 6, "{example}");
    }
}
