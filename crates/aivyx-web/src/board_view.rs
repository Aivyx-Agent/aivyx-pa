//! A20 — the Studio's Board view: four columns, filters, and a task panel.

use aivyx_ipc::board::{
    Author, BoardSnapshot, EntryKind, NewTask, Owner, RunOutcome, Task, TaskState,
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

pub fn owner_label(o: Option<&Owner>) -> String {
    match o {
        None => "unassigned".into(),
        Some(Owner::Role { name }) => format!("role {name}"),
        Some(Owner::Team) => "team".into(),
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
            let cost = cost_usd.map(|c| format!(", ${c:.2}")).unwrap_or_default();
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
            format!("run {mission_id} {outcome} ({tokens} tokens{cost}){answer}")
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

/// The owner picker's choice for an owner: `""`, `"team"` or `"role"`.
pub fn owner_choice(o: Option<&Owner>) -> &'static str {
    match o {
        None => "",
        Some(Owner::Team) => "team",
        Some(Owner::Role { .. }) => "role",
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
        _ => None,
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

#[component]
pub fn BoardPanel() -> Element {
    let ws = use_context::<Sender>();
    let mut ui = use_context::<Signal<BoardUi>>();

    // Open on a fresh snapshot (the App poll keeps it fresh after).
    use_effect(move || {
        ws.send(snapshot_query());
    });

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
        div { class: "settings",
            div { class: "panel-head",
                h3 { "Board" }
                if s.loaded {
                    span { class: "label-tech", "{s.snapshot.tasks.len()} tasks" }
                }
                button { class: "btn-ghost", onclick: move |_| { ws.send(snapshot_query()); }, "Refresh" }
            }
            if let Some(err) = s.error.clone() {
                div { class: "notice error", role: "alert",
                    "{err} "
                    button { class: "btn-ghost", onclick: move |_| ui.write().error = None, "Dismiss" }
                }
            }
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
                                            let owner = owner_label(t.owner.as_ref());
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
}

#[component]
fn TaskPanel(task: Task) -> Element {
    let ws = use_context::<Sender>();
    let mut ui = use_context::<Signal<BoardUi>>();
    let mut choice = use_signal(|| owner_choice(task.owner.as_ref()).to_string());
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
                span { class: "label-tech", "{column_title(task.state)} · {owner_label(task.owner.as_ref())}" }
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
        assert_eq!(owner_label(None), "unassigned");
        assert_eq!(owner_label(Some(&Owner::Team)), "team");
        assert_eq!(
            owner_label(Some(&Owner::Role {
                name: "coder".into()
            })),
            "role coder"
        );
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
        assert_eq!(
            owner_choice(Some(&Owner::Role { name: "a".into() })),
            "role"
        );
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
        assert_eq!(entry_line(&failed), "run m failed: timed out (0 tokens)");
        let free = EntryKind::Run {
            mission_id: "m".into(),
            outcome: RunOutcome::Interrupted,
            answer: String::new(),
            tokens: 0,
            cost_usd: None,
        };
        assert_eq!(entry_line(&free), "run m interrupted (0 tokens)");
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
}
