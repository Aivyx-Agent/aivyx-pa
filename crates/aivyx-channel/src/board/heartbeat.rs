//! A21 — the heartbeat loop: standing agents wake on their cron heartbeat,
//! or [`ASSIGN_DELAY`] after a task is assigned to them, and start their
//! To-do tasks one after another (oldest first, at most [`MAX_PER_WAKE`],
//! each at most once per wake).

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aivyx_core::CancellationToken;
use aivyx_ipc::board::{EntryKind, Owner, Task, TaskState};

use super::BoardService;
use super::agents::is_due;
use super::model::{BoardError, now_ms};

/// How often the loop looks for due agents.
pub const TICK: Duration = Duration::from_secs(15);
/// How long after an assignment the agent wakes.
pub const ASSIGN_DELAY: Duration = Duration::from_secs(30);
/// The most tasks one wake starts.
pub const MAX_PER_WAKE: usize = 5;
/// How often a wake looks at the task it started.
pub const POLL: Duration = Duration::from_secs(2);
/// A safety net: a wake stops waiting for a run after this long.
pub const RUN_CEILING: Duration = Duration::from_secs(25 * 3600);

/// The loop's timings. `Default` is the real ones; tests shorten them.
#[derive(Debug, Clone, Copy)]
pub struct Durations {
    pub tick: Duration,
    pub assign_delay: Duration,
    pub poll: Duration,
    pub run_ceiling: Duration,
}

impl Default for Durations {
    fn default() -> Self {
        Durations {
            tick: TICK,
            assign_delay: ASSIGN_DELAY,
            poll: POLL,
            run_ceiling: RUN_CEILING,
        }
    }
}

/// Agent id → when its assignment wake is due (ms).
type Pending = Arc<Mutex<HashMap<String, u64>>>;

/// The heartbeat. Cheap to clone; clones share their state.
#[derive(Clone)]
pub struct BoardHeartbeat {
    board: BoardService,
    pending: Pending,
    /// The agents with a wake in progress.
    waking: Arc<Mutex<HashSet<String>>>,
    durations: Durations,
}

/// Holds an agent's place in `waking`; frees it however the wake ends.
struct WakeGuard {
    waking: Arc<Mutex<HashSet<String>>>,
    agent_id: String,
}

impl Drop for WakeGuard {
    fn drop(&mut self) {
        if let Ok(mut w) = self.waking.lock() {
            w.remove(&self.agent_id);
        }
    }
}

fn set_pending(pending: &Pending, agent_id: &str, delay: Duration) {
    let at = now_ms().saturating_add(delay.as_millis() as u64);
    // A debounce: a later assignment moves the wake to its own delay.
    pending.lock().unwrap().insert(agent_id.to_string(), at);
}

fn owned_by<'a>(tasks: &'a [Task], agent_id: &'a str) -> impl Iterator<Item = &'a Task> + 'a {
    tasks
        .iter()
        .filter(move |t| matches!(&t.owner, Some(Owner::Agent { id }) if id == agent_id))
}

impl BoardHeartbeat {
    /// The heartbeat over `board`, with the real timings. Installs the
    /// board's assign hook.
    pub fn new(board: BoardService) -> Self {
        Self::with_durations(board, Durations::default())
    }

    /// [`new`](Self::new) with other timings (tests).
    pub fn with_durations(board: BoardService, durations: Durations) -> Self {
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        // The hook holds only the pending map, not the heartbeat: the board
        // owns the hook, so holding the board would make a cycle.
        let hook_pending = Arc::clone(&pending);
        let delay = durations.assign_delay;
        board.set_assign_hook(Arc::new(move |id| set_pending(&hook_pending, id, delay)));
        // A pause drops the pending wake (a stale one would fire right after
        // the resume); a resume with To-do tasks to wake for sets a fresh one.
        let pause_pending = Arc::clone(&pending);
        board.set_pause_hook(Arc::new(move |id, rewake| {
            if rewake {
                set_pending(&pause_pending, id, delay);
            } else {
                pause_pending.lock().unwrap().remove(id);
            }
        }));
        BoardHeartbeat {
            board,
            pending,
            waking: Arc::new(Mutex::new(HashSet::new())),
            durations,
        }
    }

    /// Wake `agent_id` once `delay` has passed (the loop picks it up).
    pub fn wake_after(&self, agent_id: &str, delay: Duration) {
        set_pending(&self.pending, agent_id, delay);
    }

    /// The pending wake of `agent_id`, if any (ms).
    pub fn pending_wake(&self, agent_id: &str) -> Option<u64> {
        self.pending.lock().unwrap().get(agent_id).copied()
    }

    /// Take `agent_id`'s place in `waking`, unless a wake holds it.
    fn claim(&self, agent_id: &str) -> Option<WakeGuard> {
        let mut w = self.waking.lock().unwrap();
        w.insert(agent_id.to_string()).then(|| WakeGuard {
            waking: Arc::clone(&self.waking),
            agent_id: agent_id.to_string(),
        })
    }

    /// Wake `agent_id` now: start its To-do tasks one after another. Returns
    /// how many it started. A paused agent, or one already waking, starts
    /// none.
    pub async fn wake_now(&self, agent_id: &str) -> Result<usize, BoardError> {
        let Some(guard) = self.claim(agent_id) else {
            return Ok(0);
        };
        self.wake_claimed(guard).await
    }

    async fn wake_claimed(&self, guard: WakeGuard) -> Result<usize, BoardError> {
        let agent_id = guard.agent_id.as_str();
        let agent = self
            .board
            .agents()
            .await?
            .into_iter()
            .find(|a| a.id == agent_id)
            .ok_or(BoardError::NotFound("agent"))?;
        if agent.paused {
            return Ok(0);
        }
        self.pending.lock().unwrap().remove(agent_id);
        self.board.record_wake(agent_id).await?;

        let mut tried: HashSet<String> = HashSet::new();
        let mut started = 0;
        while started < MAX_PER_WAKE {
            // One task at a time: a run the operator started counts too.
            if !self.wait_idle(agent_id).await? {
                break;
            }
            // Paused (or removed) mid-wake: start nothing more. Snapshot
            // tasks come oldest first.
            let snap = self.board.snapshot().await?;
            if !snap.agents.iter().any(|a| a.id == agent_id && !a.paused) {
                break;
            }
            let tasks = snap.tasks;
            let Some(next) = owned_by(&tasks, agent_id)
                .find(|t| t.state == TaskState::Todo && !tried.contains(&t.id))
            else {
                break;
            };
            let task_id = next.id.clone();
            tried.insert(task_id.clone());
            match self.board.start_as(&task_id, "agent").await {
                Ok(_) => started += 1,
                Err(e) => {
                    let text = format!("the agent couldn't start this task: {e}");
                    // Once: a cron agent that can't start its tasks would
                    // otherwise add the same comment at every firing.
                    let said = matches!(
                        next.thread.last().map(|e| &e.entry),
                        Some(EntryKind::Comment { text: last }) if *last == text
                    );
                    if said {
                        continue;
                    }
                    if let Err(e) = self.board.comment(&task_id, &text).await {
                        eprintln!("aivyx-pa board: couldn't comment on task {task_id}: {e}");
                    }
                }
            }
        }
        drop(guard);
        Ok(started)
    }

    /// Wait until `agent_id` owns no Running task. False when the ceiling
    /// passed first.
    async fn wait_idle(&self, agent_id: &str) -> Result<bool, BoardError> {
        let deadline = tokio::time::Instant::now() + self.durations.run_ceiling;
        loop {
            let tasks = self.board.snapshot().await?.tasks;
            if !owned_by(&tasks, agent_id).any(|t| t.state == TaskState::Running) {
                return Ok(true);
            }
            if tokio::time::Instant::now() >= deadline {
                eprintln!(
                    "aivyx-pa board: agent {agent_id}'s task is still running after {:?}; \
                     the wake stops",
                    self.durations.run_ceiling
                );
                return Ok(false);
            }
            tokio::time::sleep(self.durations.poll).await;
        }
    }

    /// One pass: wake every due agent that isn't waking already.
    async fn tick(&self) -> Result<(), BoardError> {
        let snap = self.board.snapshot().await?;
        let now = now_ms();
        {
            // Forget the pending wakes of removed agents.
            let mut p = self.pending.lock().unwrap();
            p.retain(|id, _| snap.agents.iter().any(|a| &a.id == id));
        }
        for agent in &snap.agents {
            let busy = owned_by(&snap.tasks, &agent.id).any(|t| t.state == TaskState::Running);
            if !is_due(agent, busy, self.pending_wake(&agent.id), now) {
                continue;
            }
            let Some(guard) = self.claim(&agent.id) else {
                continue;
            };
            let this = self.clone();
            tokio::spawn(async move {
                let id = guard.agent_id.clone();
                if let Err(e) = this.wake_claimed(guard).await {
                    eprintln!("aivyx-pa board: agent {id}'s wake failed: {e}");
                }
            });
        }
        Ok(())
    }

    /// The loop: every tick, wake the due agents, until `shutdown`.
    pub async fn run(self, shutdown: CancellationToken) {
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => return,
                _ = tokio::time::sleep(self.durations.tick) => {}
            }
            if let Err(e) = self.tick().await {
                eprintln!("aivyx-pa board: the heartbeat couldn't read the board: {e}");
            }
        }
    }

    /// After the restart sweep: every `wake_on_assign` agent that owns
    /// To-do tasks wakes [`ASSIGN_DELAY`] from now (pending wakes don't
    /// survive a restart).
    pub async fn rewake_on_startup(&self) {
        let snap = match self.board.snapshot().await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("aivyx-pa board: couldn't re-wake the agents: {e}");
                return;
            }
        };
        for agent in snap.agents.iter().filter(|a| a.wake_on_assign) {
            if owned_by(&snap.tasks, &agent.id).any(|t| t.state == TaskState::Todo) {
                self.wake_after(&agent.id, self.durations.assign_delay);
            }
        }
    }
}
