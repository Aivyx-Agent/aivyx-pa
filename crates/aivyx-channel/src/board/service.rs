//! A20 — the board's service: the operator's operations, the runs that work
//! tasks, cancel, the restart sweep, the session registry the agent tools
//! check, and the review notice. Every change appends one `BoardChanged`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use aivyx_audit::{AuditEvent, AuditWriter, PersistentAuditLog};
use aivyx_capability::CapabilitySet;
use aivyx_core::{CancellationToken, ChannelContext, Message, SessionId, TurnOutcome};
use aivyx_ipc::board::{
    Agent, BoardSnapshot, Goal, NewAgent, NewTask, Owner, Project, RunKind, RunOutcome, RunRef,
    Task, TaskState, WorksAs, Workspace,
};
use aivyx_ipc::team_mission::{TeamMissionPhase, TeamMissionRecord};
use aivyx_storage::{DomainHandle, StorageError};

use super::agents;
use super::workspaces;
use super::model::{self, Actor, BoardError, new_id, now_ms};
use super::runner::{BoardRunContext, brief, role_ceiling, turn_result};
use super::store;
use crate::mission;
use crate::notify_dispatcher::NotifyDispatcher;
use crate::parked_steps::{ParkingChannel, StepParker};
use crate::team_mission_driver::TeamMissionService;

impl From<StorageError> for BoardError {
    fn from(e: StorageError) -> Self {
        BoardError::Storage(e.to_string())
    }
}

/// How often a team run's poller looks at its mission.
const TEAM_POLL: Duration = Duration::from_secs(2);

/// How many times a cancel tries to abort a team mission that isn't running
/// yet (planning, or just started), one [`TEAM_POLL`] apart.
const ABORT_TRIES: u32 = 30;

/// The prefix of a subtask description `board.subtask` wrote.
const AGENT_WRITTEN: &str = "(written by an agent) ";

/// The halt reason of a paused team mission whose board task was cancelled.
const CANCELLED_REASON: &str = "its board task was cancelled";

/// The halt reason of a parked team mission whose board task was running
/// when the daemon stopped.
const INTERRUPTED_REASON: &str = "its board task was interrupted by a daemon restart";

/// What the board needs from the daemon.
pub struct BoardDeps {
    /// `KeyDomain::Board`.
    pub store: DomainHandle,
    /// `KeyDomain::Missions`: a role run's `MissionRecord`.
    pub missions: DomainHandle,
    pub audit: Arc<PersistentAuditLog>,
    pub agent: Arc<dyn aivyx_core::Agent>,
    /// The trigger dispatch's lock: role runs take turns with fired turns.
    pub turn_lock: Arc<tokio::sync::Mutex<()>>,
    pub parker: Option<Arc<StepParker>>,
    /// Role name → the role's `capability_scopes`.
    pub roles: HashMap<String, CapabilitySet>,
    pub team: Option<TeamMissionService>,
    pub notify: Option<Arc<NotifyDispatcher>>,
    pub notify_target: Option<String>,
    /// pa's own data and config folders: a workspace can't contain or sit
    /// inside them.
    pub protected_paths: Vec<std::path::PathBuf>,
    /// The operator's home folder, which can't itself be a workspace.
    pub home: Option<std::path::PathBuf>,
}

struct Inner {
    deps: BoardDeps,
    /// Serializes every read-modify-write of a board record.
    write_lock: tokio::sync::Mutex<()>,
    /// A role run's session → `(task id, mission id)`.
    runs: std::sync::Mutex<HashMap<SessionId, (String, String)>>,
    /// A running task → the token that stops its run.
    cancels: std::sync::Mutex<HashMap<String, CancellationToken>>,
    team_poll: Duration,
    /// Called with an agent's id after a task is assigned to it, when the
    /// agent has `wake_on_assign`. The heartbeat installs it.
    assign_hook: AssignHook,
    /// Called with an agent's id after it is paused (`false`) or resumed
    /// (`true` when it should wake: `wake_on_assign` and To-do tasks). The
    /// heartbeat installs it.
    pause_hook: PauseHook,
}

type AssignHook = std::sync::RwLock<Option<Arc<dyn Fn(&str) + Send + Sync>>>;
type PauseHookFn = Arc<dyn Fn(&str, bool) + Send + Sync>;
type PauseHook = std::sync::RwLock<Option<PauseHookFn>>;

/// The work board. Cheap to clone.
#[derive(Clone)]
pub struct BoardService {
    inner: Arc<Inner>,
}

fn storage_err(e: impl std::fmt::Display) -> BoardError {
    BoardError::Storage(e.to_string())
}

impl BoardService {
    pub fn new(deps: BoardDeps) -> Self {
        Self::with_team_poll(deps, TEAM_POLL)
    }

    fn with_team_poll(deps: BoardDeps, team_poll: Duration) -> Self {
        BoardService {
            inner: Arc::new(Inner {
                deps,
                write_lock: tokio::sync::Mutex::new(()),
                runs: std::sync::Mutex::new(HashMap::new()),
                cancels: std::sync::Mutex::new(HashMap::new()),
                team_poll,
                assign_hook: std::sync::RwLock::new(None),
                pause_hook: std::sync::RwLock::new(None),
            }),
        }
    }

    fn deps(&self) -> &BoardDeps {
        &self.inner.deps
    }

    fn record(&self, op: &str, object: &str, actor: &str) {
        let event = AuditEvent::BoardChanged {
            op: op.into(),
            object: object.into(),
            actor: actor.into(),
        };
        if let Err(e) = self.deps().audit.append(event) {
            eprintln!("aivyx-pa board: couldn't audit: {e}");
        }
    }

    /// The lock role runs take; the daemon's trigger dispatch shares it.
    pub fn turn_lock(&self) -> Arc<tokio::sync::Mutex<()>> {
        Arc::clone(&self.deps().turn_lock)
    }

    async fn load_task(&self, id: &str) -> Result<Task, BoardError> {
        store::get_task(&self.deps().store, id)
            .await?
            .ok_or(BoardError::NotFound("task"))
    }

    /// Get, change and put one task under the write lock, then audit `op`.
    async fn change_task<T>(
        &self,
        id: &str,
        op: &str,
        actor: &str,
        f: impl FnOnce(&mut Task, u64) -> Result<T, BoardError>,
    ) -> Result<T, BoardError> {
        let _w = self.inner.write_lock.lock().await;
        let mut task = self.load_task(id).await?;
        let out = f(&mut task, now_ms())?;
        store::put_task(&self.deps().store, &task).await?;
        self.record(op, id, actor);
        Ok(out)
    }

    pub async fn snapshot(&self) -> Result<BoardSnapshot, BoardError> {
        let h = &self.deps().store;
        let mut goals = store::list_goals(h).await?;
        let mut projects = store::list_projects(h).await?;
        let mut tasks = store::list_tasks(h).await?;
        let mut agents = store::list_agents(h).await?;
        let mut workspaces = store::list_workspaces(h).await?;
        goals.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
        projects.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
        tasks.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
        agents.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
        workspaces.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
        Ok(BoardSnapshot {
            goals,
            projects,
            tasks,
            agents,
            workspaces,
        })
    }

    pub async fn task(&self, id: &str) -> Result<Task, BoardError> {
        self.load_task(id).await
    }

    pub async fn create_goal(&self, title: &str, description: &str) -> Result<String, BoardError> {
        if title.trim().is_empty() {
            return Err(BoardError::Empty("the title"));
        }
        let now = now_ms();
        let goal = Goal {
            id: new_id('g'),
            title: title.trim().into(),
            description: description.trim().into(),
            created_at: now,
            updated_at: now,
        };
        let _w = self.inner.write_lock.lock().await;
        store::put_goal(&self.deps().store, &goal).await?;
        self.record("created", &goal.id, "operator");
        Ok(goal.id)
    }

    pub async fn create_project(
        &self,
        goal_id: &str,
        title: &str,
        description: &str,
    ) -> Result<String, BoardError> {
        if title.trim().is_empty() {
            return Err(BoardError::Empty("the title"));
        }
        let _w = self.inner.write_lock.lock().await;
        if store::get_goal(&self.deps().store, goal_id)
            .await?
            .is_none()
        {
            return Err(BoardError::NotFound("goal"));
        }
        let now = now_ms();
        let project = Project {
            id: new_id('p'),
            goal_id: goal_id.into(),
            title: title.trim().into(),
            description: description.trim().into(),
            created_at: now,
            updated_at: now,
        };
        store::put_project(&self.deps().store, &project).await?;
        self.record("created", &project.id, "operator");
        Ok(project.id)
    }

    /// The operator adds a task (or a subtask, with `parent_task`).
    pub async fn create_task(&self, new: NewTask) -> Result<String, BoardError> {
        if new.title.trim().is_empty() {
            return Err(BoardError::Empty("the title"));
        }
        let (task_id, woken) = {
        let _w = self.inner.write_lock.lock().await;
        if store::get_project(&self.deps().store, &new.project_id)
            .await?
            .is_none()
        {
            return Err(BoardError::NotFound("project"));
        }
        if let Some(parent) = &new.parent_task {
            let parent = self.load_task(parent).await?;
            if parent.project_id != new.project_id {
                return Err(BoardError::NotAllowed(
                    "a subtask lives in its parent's project",
                ));
            }
            model::add_subtask_check(&parent, &Actor::Operator)?;
        }
        self.check_owner_workspace(new.owner.as_ref()).await?;
        let mut woken = None;
        if let Some(Owner::Agent { id }) = &new.owner {
            let agent = store::get_agent(&self.deps().store, id)
                .await?
                .ok_or(BoardError::NotFound("agent"))?;
            if agent.wake_on_assign {
                woken = Some(agent.id);
            }
        }
        let now = now_ms();
        let task = Task {
            id: new_id('t'),
            project_id: new.project_id,
            parent_task: new.parent_task,
            title: new.title.trim().into(),
            description: new.description.trim().into(),
            owner: new.owner,
            review_required: new.review_required,
            state: TaskState::Todo,
            thread: vec![],
            active_run: None,
            created_at: now,
            updated_at: now,
        };
        store::put_task(&self.deps().store, &task).await?;
        self.record("created", &task.id, "operator");
        (task.id, woken)
        };
        if let Some(id) = woken {
            let hook = self.inner.assign_hook.read().unwrap().clone();
            if let Some(hook) = hook {
                hook(&id);
            }
        }
        Ok(task_id)
    }

    pub async fn assign(&self, task_id: &str, owner: Option<Owner>) -> Result<(), BoardError> {
        let woken = {
            let _w = self.inner.write_lock.lock().await;
            let mut task = self.load_task(task_id).await?;
            self.check_owner_workspace(owner.as_ref()).await?;
            let mut woken = None;
            if let Some(Owner::Agent { id }) = &owner {
                let agent = store::get_agent(&self.deps().store, id)
                    .await?
                    .ok_or(BoardError::NotFound("agent"))?;
                if agent.wake_on_assign {
                    woken = Some(agent.id);
                }
            }
            model::assign(&mut task, owner, now_ms())?;
            store::put_task(&self.deps().store, &task).await?;
            self.record("assigned", task_id, "operator");
            woken
        };
        if let Some(id) = woken {
            let hook = self.inner.assign_hook.read().unwrap().clone();
            if let Some(hook) = hook {
                hook(&id);
            }
        }
        Ok(())
    }

    /// Install the hook [`assign`](Self::assign) calls with an agent's id
    /// after a task is assigned to an agent with `wake_on_assign`.
    pub fn set_assign_hook(&self, hook: Arc<dyn Fn(&str) + Send + Sync>) {
        *self.inner.assign_hook.write().unwrap() = Some(hook);
    }

    /// Install the hook [`set_agent_paused`](Self::set_agent_paused) calls
    /// with an agent's id and whether a resume should wake it.
    pub fn set_pause_hook(&self, hook: PauseHookFn) {
        *self.inner.pause_hook.write().unwrap() = Some(hook);
    }

    pub async fn agents(&self) -> Result<Vec<Agent>, BoardError> {
        let mut list = store::list_agents(&self.deps().store).await?;
        list.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
        Ok(list)
    }

    pub async fn agent_by_name(&self, name: &str) -> Result<Agent, BoardError> {
        store::list_agents(&self.deps().store)
            .await?
            .into_iter()
            .find(|a| a.name == name)
            .ok_or(BoardError::NotFound("agent"))
    }

    /// A `works_as` the daemon can run: the role exists, or a team is set up.
    async fn check_works_as(&self, works_as: &WorksAs) -> Result<(), BoardError> {
        match works_as {
            WorksAs::Coder { workspace } => self.check_workspace(workspace).await,
            WorksAs::Role { name } if !self.deps().roles.contains_key(name) => {
                Err(BoardError::NotAllowed("no such role in the config"))
            }
            WorksAs::Team if self.deps().team.is_none() => {
                Err(BoardError::NotAllowed("no team is set up"))
            }
            _ => Ok(()),
        }
    }

    async fn check_workspace(&self, id: &str) -> Result<(), BoardError> {
        store::get_workspace(&self.deps().store, id)
            .await?
            .map(|_| ())
            .ok_or(BoardError::NotFound("workspace"))
    }

    async fn check_owner_workspace(&self, owner: Option<&Owner>) -> Result<(), BoardError> {
        match owner {
            Some(Owner::Coder { workspace }) => self.check_workspace(workspace).await,
            _ => Ok(()),
        }
    }

    pub async fn workspaces(&self) -> Result<Vec<Workspace>, BoardError> {
        let mut list = store::list_workspaces(&self.deps().store).await?;
        list.sort_by(|a, b| (a.created_at, &a.id).cmp(&(b.created_at, &b.id)));
        Ok(list)
    }

    pub async fn workspace_by_name(&self, name: &str) -> Result<Workspace, BoardError> {
        store::list_workspaces(&self.deps().store)
            .await?
            .into_iter()
            .find(|w| w.name == name)
            .ok_or(BoardError::NotFound("workspace"))
    }

    /// The operator registers a folder; the stored path is canonical.
    pub async fn add_workspace(&self, name: &str, path: &str) -> Result<String, BoardError> {
        workspaces::validate_name(name)?;
        let canon = workspaces::validate_path(
            std::path::Path::new(path),
            self.deps().home.as_deref(),
            &self.deps().protected_paths,
        )?;
        let _w = self.inner.write_lock.lock().await;
        if store::list_workspaces(&self.deps().store)
            .await?
            .iter()
            .any(|w| w.name == name)
        {
            return Err(BoardError::NotAllowed("a workspace with that name exists"));
        }
        let ws = Workspace {
            id: new_id('w'),
            name: name.into(),
            path: canon.to_string_lossy().into_owned(),
            created_at: now_ms(),
        };
        store::put_workspace(&self.deps().store, &ws).await?;
        self.record("workspace_added", &ws.id, "operator");
        Ok(ws.id)
    }

    /// Refused while an open task is owned by the workspace or an agent
    /// works as it.
    pub async fn remove_workspace(&self, name: &str) -> Result<(), BoardError> {
        let _w = self.inner.write_lock.lock().await;
        let ws = self.workspace_by_name(name).await?;
        let h = &self.deps().store;
        let agents = store::list_agents(h).await?;
        let in_use = agents
            .iter()
            .any(|a| matches!(&a.works_as, WorksAs::Coder { workspace } if *workspace == ws.id))
            || store::list_tasks(h).await?.iter().any(|t| {
                matches!(t.state, TaskState::Todo | TaskState::Running | TaskState::Review)
                    && matches!(&t.owner, Some(Owner::Coder { workspace }) if *workspace == ws.id)
            });
        if in_use {
            return Err(BoardError::NotAllowed("the workspace is in use"));
        }
        store::delete_workspace(h, &ws.id).await?;
        self.record("workspace_removed", &ws.id, "operator");
        Ok(())
    }

    async fn agent_map(&self) -> Result<HashMap<String, Agent>, BoardError> {
        Ok(store::list_agents(&self.deps().store)
            .await?
            .into_iter()
            .map(|a| (a.id.clone(), a))
            .collect())
    }

    /// Check a name is valid and not another agent's.
    fn check_name(
        name: &str,
        except: Option<&str>,
        all: &HashMap<String, Agent>,
    ) -> Result<(), BoardError> {
        agents::validate_name(name)?;
        if all
            .values()
            .any(|a| a.name == name && Some(a.id.as_str()) != except)
        {
            return Err(BoardError::NotAllowed("an agent with that name exists"));
        }
        Ok(())
    }

    pub async fn create_agent(&self, a: NewAgent) -> Result<String, BoardError> {
        let name = a.name.ok_or(BoardError::Empty("the name"))?;
        let works_as = a
            .works_as
            .ok_or(BoardError::NotAllowed("an agent needs a works-as"))?;
        let _w = self.inner.write_lock.lock().await;
        let all = self.agent_map().await?;
        Self::check_name(&name, None, &all)?;
        self.check_works_as(&works_as).await?;
        let heartbeat = if a.clear_heartbeat { None } else { a.heartbeat };
        if let Some(h) = &heartbeat {
            agents::validate_heartbeat(h)?;
        }
        let reports_to = if a.clear_reports_to {
            None
        } else {
            a.reports_to
        };
        let id = new_id('a');
        agents::check_reports_to(&id, reports_to.as_deref(), &all)?;
        let now = now_ms();
        let agent = Agent {
            id: id.clone(),
            name,
            title: a.title.unwrap_or_default().trim().into(),
            reports_to,
            works_as,
            heartbeat,
            wake_on_assign: a.wake_on_assign.unwrap_or(false),
            paused: false,
            last_wake: None,
            created_at: now,
            updated_at: now,
        };
        store::put_agent(&self.deps().store, &agent).await?;
        self.record("agent_created", &id, "operator");
        Ok(id)
    }

    /// Get, change and put one agent (found by name) under the write lock,
    /// then audit `op`. `f` also sees every agent.
    async fn change_agent(
        &self,
        name: &str,
        op: &str,
        f: impl FnOnce(&mut Agent, &HashMap<String, Agent>, u64) -> Result<(), BoardError>,
    ) -> Result<(), BoardError> {
        let _w = self.inner.write_lock.lock().await;
        let all = self.agent_map().await?;
        let mut agent = all
            .values()
            .find(|a| a.name == name)
            .cloned()
            .ok_or(BoardError::NotFound("agent"))?;
        f(&mut agent, &all, now_ms())?;
        store::put_agent(&self.deps().store, &agent).await?;
        self.record(op, &agent.id, "operator");
        Ok(())
    }

    pub async fn edit_agent(&self, name: &str, a: NewAgent) -> Result<(), BoardError> {
        if let Some(w) = &a.works_as {
            self.check_works_as(w).await?;
        }
        self.change_agent(name, "agent_edited", |agent, all, now| {
            if let Some(n) = &a.name {
                Self::check_name(n, Some(&agent.id), all)?;
            }
            if let Some(h) = a.heartbeat.as_deref().filter(|_| !a.clear_heartbeat) {
                agents::validate_heartbeat(h)?;
            }
            if let Some(r) = a.reports_to.as_deref().filter(|_| !a.clear_reports_to) {
                agents::check_reports_to(&agent.id, Some(r), all)?;
            }
            if let Some(n) = a.name {
                agent.name = n;
            }
            if let Some(t) = a.title {
                agent.title = t.trim().into();
            }
            if let Some(w) = a.works_as {
                agent.works_as = w;
            }
            if a.clear_heartbeat {
                agent.heartbeat = None;
            } else if a.heartbeat.is_some() {
                agent.heartbeat = a.heartbeat;
            }
            if a.clear_reports_to {
                agent.reports_to = None;
            } else if a.reports_to.is_some() {
                agent.reports_to = a.reports_to;
            }
            if let Some(w) = a.wake_on_assign {
                agent.wake_on_assign = w;
            }
            agent.updated_at = now;
            Ok(())
        })
        .await
    }

    pub async fn set_agent_paused(&self, name: &str, paused: bool) -> Result<(), BoardError> {
        let op = if paused {
            "agent_paused"
        } else {
            "agent_resumed"
        };
        let mut changed = None;
        self.change_agent(name, op, |agent, _, now| {
            agent.paused = paused;
            agent.updated_at = now;
            changed = Some((agent.id.clone(), agent.wake_on_assign));
            Ok(())
        })
        .await?;
        let Some((id, wake_on_assign)) = changed else {
            return Ok(());
        };
        let rewake = !paused
            && wake_on_assign
            && match store::list_tasks(&self.deps().store).await {
                Ok(tasks) => tasks.iter().any(|t| {
                    t.state == TaskState::Todo
                        && matches!(&t.owner, Some(Owner::Agent { id: o }) if *o == id)
                }),
                Err(e) => {
                    eprintln!("aivyx-pa board: resume of {name}: task list failed, no re-wake: {e}");
                    false
                }
            };
        let hook = self.inner.pause_hook.read().unwrap().clone();
        if let Some(hook) = hook {
            hook(&id, rewake);
        }
        Ok(())
    }

    pub async fn remove_agent(&self, name: &str) -> Result<(), BoardError> {
        let _w = self.inner.write_lock.lock().await;
        let all = self.agent_map().await?;
        let agent = all
            .values()
            .find(|a| a.name == name)
            .cloned()
            .ok_or(BoardError::NotFound("agent"))?;
        let tasks = store::list_tasks(&self.deps().store).await?;
        if !agents::open_tasks_owned_by(&agent.id, &tasks).is_empty() {
            return Err(BoardError::NotAllowed(
                "reassign the agent's open tasks first",
            ));
        }
        store::delete_agent(&self.deps().store, &agent.id).await?;
        // Agents that reported to it now report to no one.
        let now = now_ms();
        for mut other in all.into_values() {
            if other.reports_to.as_deref() == Some(agent.id.as_str()) {
                other.reports_to = None;
                other.updated_at = now;
                store::put_agent(&self.deps().store, &other).await?;
            }
        }
        self.record("agent_removed", &agent.id, "operator");
        Ok(())
    }

    /// The heartbeat records a wake: `last_wake = now`, audited `woke`.
    /// Returns the agent as saved.
    pub async fn record_wake(&self, agent_id: &str) -> Result<Agent, BoardError> {
        let _w = self.inner.write_lock.lock().await;
        let mut agent = store::get_agent(&self.deps().store, agent_id)
            .await?
            .ok_or(BoardError::NotFound("agent"))?;
        agent.last_wake = Some(now_ms());
        store::put_agent(&self.deps().store, &agent).await?;
        self.record("woke", agent_id, "operator");
        Ok(agent)
    }

    /// The owner a run executes as: a role or team as is; an agent as its
    /// `works_as`.
    async fn resolve_owner(&self, owner: &Owner) -> Result<Owner, BoardError> {
        match owner {
            Owner::Agent { id } => {
                let agent = store::get_agent(&self.deps().store, id)
                    .await?
                    .ok_or(BoardError::NotFound("agent"))?;
                Ok(match agent.works_as {
                    WorksAs::Role { name } => Owner::Role { name },
                    WorksAs::Team => Owner::Team,
                    WorksAs::Coder { workspace } => Owner::Coder { workspace },
                })
            }
            other => Ok(other.clone()),
        }
    }

    pub async fn approve(&self, task_id: &str) -> Result<(), BoardError> {
        self.change_task(task_id, "approved", "operator", model::approve)
            .await
    }

    pub async fn send_back(&self, task_id: &str, why: &str) -> Result<(), BoardError> {
        self.change_task(task_id, "sent_back", "operator", |t, now| {
            model::send_back(t, why, now)
        })
        .await
    }

    pub async fn comment(&self, task_id: &str, text: &str) -> Result<(), BoardError> {
        self.change_task(task_id, "commented", "operator", |t, now| {
            model::comment(t, &Actor::Operator, text, now)
        })
        .await
    }

    /// Cancel a task; a running task's run is stopped (a role run's turn is
    /// cancelled, a team run's mission aborted).
    pub async fn cancel(&self, task_id: &str) -> Result<(), BoardError> {
        let run = self
            .change_task(task_id, "cancelled", "operator", model::cancel)
            .await?;
        if run.is_some() {
            let token = self.inner.cancels.lock().unwrap().remove(task_id);
            if let Some(token) = token {
                token.cancel();
            }
        }
        Ok(())
    }

    /// The brief for `task`: its project's and goal's titles plus its notes.
    async fn brief_for(&self, task: &Task) -> Result<String, BoardError> {
        let h = &self.deps().store;
        let project = store::get_project(h, &task.project_id)
            .await?
            .ok_or(BoardError::NotFound("project"))?;
        let goal = store::get_goal(h, &project.goal_id)
            .await?
            .ok_or(BoardError::NotFound("goal"))?;
        Ok(brief(task, &project, &goal))
    }

    /// Start a task's run; returns the run's mission id. The run itself is
    /// spawned.
    pub async fn start(&self, task_id: &str) -> Result<String, BoardError> {
        self.start_as(task_id, "operator").await
    }

    /// [`start`](Self::start), audited `started` with `actor` (`operator`,
    /// or `agent` for a start from an agent's wake).
    pub async fn start_as(&self, task_id: &str, actor: &'static str) -> Result<String, BoardError> {
        let task = self.load_task(task_id).await?;
        match task.owner.clone() {
            Some(stored) => match self.resolve_owner(&stored).await? {
                Owner::Role { name } => self.start_role(task_id, &name, &stored, actor).await,
                Owner::Team => self.start_team(task_id, &stored, actor).await,
                // Until the coder run lands (A22, a later task).
                Owner::Coder { .. } => Err(BoardError::NotAllowed(
                    "coder runs land in a later task",
                )),
                // resolve_owner never returns an agent.
                Owner::Agent { .. } => {
                    Err(BoardError::NotAllowed("an agent can't work as an agent"))
                }
            },
            None => {
                // Let the model say why (a bad state before the owner).
                let mut probe = task;
                model::start(
                    &mut probe,
                    RunRef {
                        mission_id: String::new(),
                        kind: RunKind::Role,
                    },
                    now_ms(),
                )?;
                Err(BoardError::NotAllowed(
                    "assign an owner before starting the task",
                ))
            }
        }
    }

    /// Under the write lock: the task still has the owner `start_as` saw,
    /// and an agent owner still works as `want` (a `works_as` edit may have
    /// landed since).
    async fn check_owner_unchanged(
        &self,
        task: &Task,
        expected: &Owner,
        want: Owner,
    ) -> Result<(), BoardError> {
        if task.owner.as_ref() != Some(expected) || self.resolve_owner(expected).await? != want {
            return Err(BoardError::NotAllowed("the task's owner changed"));
        }
        Ok(())
    }

    async fn start_role(
        &self,
        task_id: &str,
        role: &str,
        expected: &Owner,
        actor: &'static str,
    ) -> Result<String, BoardError> {
        let scopes = self
            .deps()
            .roles
            .get(role)
            .ok_or(BoardError::NotAllowed("no such role in the config"))?;
        let ceiling = role_ceiling(scopes);
        let mission_id = new_id('b');
        let session = SessionId::new();
        let cancel = CancellationToken::new();

        let (task, text) = {
            let _w = self.inner.write_lock.lock().await;
            let mut task = self.load_task(task_id).await?;
            self.check_owner_unchanged(&task, expected, Owner::Role { name: role.into() })
                .await?;
            let text = self.brief_for(&task).await?;
            model::start_by(
                &mut task,
                RunRef {
                    mission_id: mission_id.clone(),
                    kind: RunKind::Role,
                },
                now_ms(),
                actor == "agent",
            )?;
            let mut record =
                mission::MissionRecord::new(mission_id.clone(), role.into(), task.title.clone());
            mission::transition_to_running(&mut record).map_err(storage_err)?;
            mission::create_mission(&self.deps().missions, &record).await?;
            store::put_task(&self.deps().store, &task).await?;
            self.record("started", task_id, actor);
            // Register before the write lock drops, so a cancel queued on
            // the lock always finds the token.
            self.inner
                .runs
                .lock()
                .unwrap()
                .insert(session, (task.id.clone(), mission_id.clone()));
            self.inner
                .cancels
                .lock()
                .unwrap()
                .insert(task.id.clone(), cancel.clone());
            (task, text)
        };

        let this = self.clone();
        let mid = mission_id.clone();
        tokio::spawn(async move {
            this.run_role(task.id, mid, session, ceiling, cancel, text)
                .await;
        });
        Ok(mission_id)
    }

    async fn run_role(
        &self,
        task_id: String,
        mission_id: String,
        session: SessionId,
        ceiling: CapabilitySet,
        cancel: CancellationToken,
        text: String,
    ) {
        let registration = RunRegistration {
            inner: Arc::clone(&self.inner),
            session,
            task_id: task_id.clone(),
        };
        let ctx = BoardRunContext {
            session,
            ceiling,
            cancel: cancel.clone(),
        };
        let channel: Arc<dyn ChannelContext + Send + Sync> = match &self.deps().parker {
            Some(parker) => Arc::new(ParkingChannel::new(
                Arc::new(ctx),
                Arc::clone(parker),
                format!("board task {task_id}"),
            )),
            None => Arc::new(ctx),
        };

        let (outcome, tokens) = {
            let _turn = self.deps().turn_lock.lock().await;
            let audit = &self.deps().audit;
            let pre = audit.len();
            let outcome = if cancel.is_cancelled() {
                TurnOutcome::Cancelled { tool_calls_made: 0 }
            } else {
                let msg = Message::text(session, text).system_originated();
                self.deps().agent.turn(msg, channel.as_ref()).await
            };
            // The turn lock serializes fired turns (triggers and board
            // runs), so every `LlmCost` in the range is this run's, with one
            // known gap: interactive chat turns don't take the lock, and one
            // running concurrently adds its tokens to this count.
            let len = audit.len();
            let tokens: u64 = audit
                .entries_range(pre as u64, len.saturating_sub(pre))
                .unwrap_or_default()
                .iter()
                .filter_map(|e| match &e.event {
                    AuditEvent::LlmCost { usage, .. } => {
                        Some(u64::from(usage.input_tokens) + u64::from(usage.output_tokens))
                    }
                    _ => None,
                })
                .sum();
            (outcome, tokens)
        };
        let (run_outcome, answer) = turn_result(&outcome);

        drop(registration);

        // The mission record first, so it is closed by the time the task
        // leaves Running.
        if let Err(e) = self
            .close_mission(&mission_id, run_outcome == RunOutcome::Succeeded)
            .await
        {
            eprintln!("aivyx-pa board: mission {mission_id}: {e}");
        }
        self.finish(&task_id, run_outcome, answer, tokens, None)
            .await;
    }

    async fn close_mission(&self, mission_id: &str, succeeded: bool) -> Result<(), BoardError> {
        let h = &self.deps().missions;
        let mut record = mission::get_mission(h, mission_id)
            .await?
            .ok_or(BoardError::NotFound("mission"))?;
        if record.is_terminal() {
            return Ok(());
        }
        if succeeded {
            mission::complete_mission(&mut record).map_err(storage_err)?;
        } else {
            mission::cancel_mission(&mut record).map_err(storage_err)?;
        }
        mission::update_mission(h, &record).await?;
        Ok(())
    }

    /// Record a run's end. A task that is no longer Running (cancelled
    /// meanwhile) is left as it is.
    async fn finish(
        &self,
        task_id: &str,
        outcome: RunOutcome,
        answer: String,
        tokens: u64,
        cost_usd: Option<f64>,
    ) {
        let result = self
            .change_task(task_id, "finished", "agent", |t, now| {
                model::finish(t, outcome, answer, tokens, cost_usd, now)?;
                Ok(t.clone())
            })
            .await;
        match result {
            Ok(task) if task.state == TaskState::Review => self.review_notice(&task).await,
            Ok(_) | Err(BoardError::BadState { .. }) => {}
            Err(e) => eprintln!("aivyx-pa board: couldn't finish task {task_id}: {e}"),
        }
    }

    async fn review_notice(&self, task: &Task) {
        let (Some(notify), Some(target)) = (&self.deps().notify, &self.deps().notify_target) else {
            return;
        };
        let body = format!("Task '{}' is ready for review", task.title);
        let subject = format!("board: {}", task.id);
        if let Err(e) = notify.dispatch(target, &body, Some(&subject)).await {
            eprintln!("aivyx-pa board: couldn't send the review notice: {e}");
        }
    }

    async fn start_team(
        &self,
        task_id: &str,
        expected: &Owner,
        actor: &'static str,
    ) -> Result<String, BoardError> {
        let svc = self
            .deps()
            .team
            .clone()
            .ok_or(BoardError::NotAllowed("no team is set up"))?;
        // Check first, then plan without the lock (decomposition is an LLM
        // call), then record the start.
        let text = {
            let _w = self.inner.write_lock.lock().await;
            let task = self.load_task(task_id).await?;
            let mut probe = task.clone();
            model::start(
                &mut probe,
                RunRef {
                    mission_id: String::new(),
                    kind: RunKind::Team,
                },
                now_ms(),
            )?;
            self.brief_for(&task).await?
        };
        let mission_id = svc
            .start_from_goal_for_board(&text, task_id)
            .await
            .map_err(|e| BoardError::Storage(format!("team mission: {e}")))?;

        let cancel = CancellationToken::new();
        let started = {
            let _w = self.inner.write_lock.lock().await;
            async {
                let mut task = self.load_task(task_id).await?;
                self.check_owner_unchanged(&task, expected, Owner::Team)
                    .await?;
                model::start_by(
                    &mut task,
                    RunRef {
                        mission_id: mission_id.clone(),
                        kind: RunKind::Team,
                    },
                    now_ms(),
                    actor == "agent",
                )?;
                store::put_task(&self.deps().store, &task).await?;
                self.record("started", task_id, actor);
                // Register before the write lock drops (see `start_role`).
                self.inner
                    .cancels
                    .lock()
                    .unwrap()
                    .insert(task_id.to_string(), cancel.clone());
                Ok(())
            }
            .await
        };
        if let Err(e) = started {
            // The task moved on while the team planned: stop the mission
            // (retrying while it is still planning).
            let this = self.clone();
            let mid = mission_id.clone();
            tokio::spawn(async move { this.abort_team(&svc, &mid).await });
            return Err(e);
        }
        let this = self.clone();
        let (tid, mid) = (task_id.to_string(), mission_id.clone());
        tokio::spawn(async move { this.poll_team(svc, tid, mid, cancel).await });
        Ok(mission_id)
    }

    async fn poll_team(
        &self,
        svc: TeamMissionService,
        task_id: String,
        mission_id: String,
        cancel: CancellationToken,
    ) {
        let record = loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    self.abort_team(&svc, &mission_id).await;
                    return;
                }
                _ = tokio::time::sleep(self.inner.team_poll) => {}
            }
            match svc.snapshot(&mission_id) {
                Some(r) if r.phase.is_terminal() => break Some(r),
                Some(_) => {}
                None => break None,
            }
        };
        self.inner.cancels.lock().unwrap().remove(&task_id);
        let (outcome, answer, tokens, cost) = match record {
            None => (
                RunOutcome::Failed {
                    reason: "the team mission is gone".into(),
                },
                String::new(),
                0,
                None,
            ),
            Some(r) => team_result(&r),
        };
        self.finish(&task_id, outcome, answer, tokens, cost).await;
    }

    /// Stop a cancelled task's mission. A running mission is aborted; one
    /// waiting at a gate has the gate rejected; a paused one is halted. One
    /// still planning, or `Executing` before its drive has armed the abort
    /// flag (a cancel just after a start), can't be aborted yet, so try
    /// again, up to [`ABORT_TRIES`] times, until it runs or ends.
    async fn abort_team(&self, svc: &TeamMissionService, mission_id: &str) {
        for _ in 0..ABORT_TRIES {
            let Err(e) = svc.abort(mission_id) else {
                return;
            };
            match svc.snapshot(mission_id).map(|r| (r.phase, r.pending_gate)) {
                Some((phase, _)) if phase.is_terminal() => return,
                Some((TeamMissionPhase::Planning | TeamMissionPhase::Executing, _)) => {
                    tokio::time::sleep(self.inner.team_poll).await;
                }
                Some((TeamMissionPhase::AwaitingApproval, Some(step))) => {
                    if let Err(e) = svc.resolve(mission_id, &step, false).await {
                        eprintln!(
                            "aivyx-pa board: couldn't reject gate {step} of team mission \
                             {mission_id}: {e}"
                        );
                    }
                    return;
                }
                Some((TeamMissionPhase::Paused, _)) => {
                    if let Err(e) = svc.halt_undriven(mission_id, CANCELLED_REASON).await {
                        eprintln!("aivyx-pa board: couldn't halt team mission {mission_id}: {e}");
                    }
                    return;
                }
                _ => {
                    eprintln!("aivyx-pa board: couldn't abort team mission {mission_id}: {e}");
                    return;
                }
            }
        }
        eprintln!(
            "aivyx-pa board: team mission {mission_id} didn't start running in time to be \
             aborted; stop it with `aivyx-pa team abort {mission_id}`"
        );
    }

    /// The task and mission a role run's session works, if it is one.
    pub fn run_binding(&self, session: &SessionId) -> Option<(String, String)> {
        self.inner.runs.lock().unwrap().get(session).cloned()
    }

    fn run_actor(&self, session: &SessionId) -> Result<Actor, BoardError> {
        let (task_id, mission_id) = self
            .run_binding(session)
            .ok_or(BoardError::NotAllowed("not in a board run"))?;
        Ok(Actor::Run {
            task_id,
            mission_id,
        })
    }

    /// `board.comment` from a role run: a comment on the run's own task.
    pub async fn agent_comment(&self, session: &SessionId, text: &str) -> Result<(), BoardError> {
        let actor = self.run_actor(session)?;
        let Actor::Run { task_id, .. } = &actor else {
            unreachable!()
        };
        let task_id = task_id.clone();
        self.change_task(&task_id, "commented", "agent", |t, now| {
            model::comment(t, &actor, text, now)
        })
        .await
    }

    /// `board.subtask` from a role run: an unowned subtask of the run's task.
    pub async fn agent_subtask(
        &self,
        session: &SessionId,
        title: &str,
        description: &str,
    ) -> Result<String, BoardError> {
        let actor = self.run_actor(session)?;
        let Actor::Run { task_id, .. } = &actor else {
            unreachable!()
        };
        if title.trim().is_empty() {
            return Err(BoardError::Empty("the title"));
        }
        let _w = self.inner.write_lock.lock().await;
        let parent = self.load_task(task_id).await?;
        model::add_subtask_check(&parent, &actor)?;
        let now = now_ms();
        let task = Task {
            id: new_id('t'),
            project_id: parent.project_id.clone(),
            parent_task: Some(parent.id.clone()),
            title: title.trim().into(),
            // Marked, so an operator (and the brief of a later run) can tell
            // an agent's words from their own.
            description: format!("{AGENT_WRITTEN}{}", description.trim()),
            owner: None,
            review_required: true,
            state: TaskState::Todo,
            thread: vec![],
            active_run: None,
            created_at: now,
            updated_at: now,
        };
        store::put_task(&self.deps().store, &task).await?;
        self.record("subtask_added", &task.id, "agent");
        Ok(task.id)
    }

    /// The restart sweep: every Running task goes back to Todo, a role run's
    /// mission record is closed, and a team run's mission is ended. Returns
    /// how many tasks it moved.
    pub async fn reconcile_at_startup(&self) -> Result<usize, BoardError> {
        let _w = self.inner.write_lock.lock().await;
        let mut moved = 0;
        for mut task in store::list_tasks(&self.deps().store).await? {
            let Some(run) = model::interrupt(&mut task, now_ms()) else {
                continue;
            };
            store::put_task(&self.deps().store, &task).await?;
            self.record("interrupted", &task.id, "operator");
            moved += 1;
            match run.kind {
                RunKind::Role | RunKind::Coder => {
                    if let Err(e) = self.close_mission(&run.mission_id, false).await {
                        eprintln!("aivyx-pa board: mission {}: {e}", run.mission_id);
                    }
                }
                // The team service's `reload()` (run first) halts only an
                // Executing mission; a Paused or AwaitingApproval one would
                // outlive its task, so end whatever is left here. Nothing
                // drives a mission yet at startup.
                RunKind::Team => {
                    if let Some(svc) = &self.deps().team
                        && let Err(e) = svc.halt_undriven(&run.mission_id, INTERRUPTED_REASON).await
                    {
                        eprintln!("aivyx-pa board: team mission {}: {e}", run.mission_id);
                    }
                }
            }
        }
        Ok(moved)
    }
}

/// Removes a role run's session and cancel token however its task exits,
/// a panic included.
struct RunRegistration {
    inner: Arc<Inner>,
    session: SessionId,
    task_id: String,
}

impl Drop for RunRegistration {
    fn drop(&mut self) {
        if let Ok(mut runs) = self.inner.runs.lock() {
            runs.remove(&self.session);
        }
        if let Ok(mut cancels) = self.inner.cancels.lock() {
            cancels.remove(&self.task_id);
        }
    }
}

/// `(outcome, answer, tokens, cost)` for a team mission that ended.
fn team_result(r: &TeamMissionRecord) -> (RunOutcome, String, u64, Option<f64>) {
    let cost = (r.spend_usd > 0.0).then_some(r.spend_usd);
    match r.phase {
        TeamMissionPhase::Done => {
            let answer = r
                .plan
                .steps
                .last()
                .and_then(|s| r.outputs.get(&s.id))
                .cloned()
                .unwrap_or_else(|| r.outputs.values().cloned().collect::<Vec<_>>().join("\n\n"));
            (RunOutcome::Succeeded, answer, r.spend_tokens, cost)
        }
        _ => (
            RunOutcome::Failed {
                reason: r.halt_reason.clone().unwrap_or_default(),
            },
            String::new(),
            r.spend_tokens,
            cost,
        ),
    }
}

#[cfg(test)]
mod tests;
