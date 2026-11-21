# The work board (preview)

The board is where you write down the work you want done and who should do
it. You plan it; your agents work it. Amendment A20
(`docs/amendments/2026-11-18-work-board.md`) records the design decisions;
this page is the operator's guide.

> **Preview.** The board, its `aivyx-pa board` commands and its storage
> format are outside the 1.x stability promise (`docs/COMPATIBILITY.md`)
> until it has been used for a while. It exists only in daemon mode: the
> in-process chat (`--no-daemon`) has no board and no board tools.

## Goals, projects, tasks

- A **goal** is an outcome ("Grow the shop's mailing list").
- A **project** belongs to one goal ("Spring newsletter").
- A **task** belongs to one project, or is a **subtask** of another task.
  It has a title, a description (the brief), an **owner**, a **state** and
  a **thread**: an append-only list of comments, state changes and run
  results.

Everything is stored encrypted in its own store domain (`KeyDomain::Board`).

## Owners: role runs and team runs

A task is owned by a **role** (`role:<name>`, a role from your config,
`default` included), by the **team** (the daemon's active team: the default
roster, or an installed pack's team), by a standing **agent**
(`agent:<name>`, see [Agents and heartbeats](#agents-and-heartbeats)), by
**aivyx-coder** in a registered folder (`coder:<workspace>`, see
[Give coding tasks to aivyx-coder](#give-coding-tasks-to-aivyx-coder)), or
by nobody yet. An agent-owned task runs as the agent's role, the team or
its coder workspace.

**A role run** is one turn of the daemon's agent:

- One turn: at most 32 steps and the turn timeout,
  `[agent] turn_timeout_secs`, 120 s by default. Longer work belongs on a
  team-owned task.
- It is the daemon agent under the role's **scopes**. The role's
  `tool_allowlist`, system prompt and model aren't applied. The run is capped
  to the role's resolved envelope (narrowed by its parent chain; the floor
  if the role declares no scopes), then to the role's trust ceiling, and
  then `board.write` is added. Because `board.write` is added after the
  trust cap, a SemiTrusted role's run still holds it.
- The run gets a mission record (it shows under Missions), and its id is
  in the run's thread entry.
- The run's token count is the turn's `LlmCost` entries in the audit log.
  Chat turns don't wait for board runs, so a chat turn you have running at
  the same time can add its tokens to that count.
- If the **active** role declares its own `capability_scopes` or
  `tool_allowlist` and they leave out `board.write` or the board tools,
  the agent doesn't hold them, and no role run can comment or add
  subtasks: a run's ceiling only narrows what the agent holds. This is the
  same as federation's tools. List `board.write` (and, with an allowlist,
  `board.comment` and `board.subtask`) in that role.

**A team run** is a team mission whose goal is the brief, tagged
`board:<task id>` (it shows in the team view). It is planned **headless**:
the planner is told to plan no human gates, and a step that needs you parks
in the review queue instead of waiting at a gate. If the mission stops
early (a specialist that can't finish its turn, a budget cap), the task
goes back to To do with the reason. A team run's tokens and cost are
counted only when a mission budget is set (`[budget] per_mission_tokens` or
`per_mission_usd`); otherwise its run entry shows "—" instead of a token
count.

## The brief

Each run is given:

- the goal's, the project's and the task's titles;
- the task's description;
- every comment **you** wrote in the thread, in order, including each
  send-back reason (agents' comments are left out);
- a closing line telling the agent about `board.comment` and
  `board.subtask`.

## States, and who may change them

```
To do ──Start──▶ Running ──success──▶ Review ──Approve──▶ Done
  ▲                 │   └──success, review off──────────▶ Done
  │                 │
  └──failure, denial, timeout, interruption
  ▲                                     │
  └──────────────Send back (with a note)┘

To do, Running or Review ──Cancel──▶ Cancelled
```

- Only you move a task between states (Start, Approve, Send back, Cancel)
  and assign owners, except that a standing agent starts its own To-do
  tasks when it wakes. A run's end moves it out of Running.
- **Start** is refused while the task is Running, from Done or Cancelled,
  and when it has no owner.
- A task added with `--no-review` goes straight to Done when its run
  succeeds.
- Done and Cancelled are final.

## Start, Review, Approve, Send back, Cancel

- **Start** begins a run for the task's owner and returns its mission id.
- **Review**: a successful run puts the task in Review with a `Run` entry
  holding the final answer (a role run's final message, or the team
  mission's last step's output), the token count and, when known, the
  cost.
- **Approve** moves it to Done.
- **Send back** needs a note saying what to change. The task goes back to
  To do, and the note is in the next run's brief.
- **Cancel** stops a running run (a role run's turn is cancelled; a team
  run's mission is aborted, its waiting gate rejected, or, if you paused it
  from the team view, halted) and closes the task. A team mission still
  being planned when you cancel is aborted as soon as it starts running;
  if it doesn't start within about a minute, the daemon log says so, and
  `aivyx-pa team abort <id>` ends it.

## What agents can do

In a role run, and only there, the agent has two tools, both under the
`board.write` scope:

- `board.comment { text }`: add a note to the run's own task;
- `board.subtask { title, description }`: add an unowned subtask under the
  run's own task. Its description starts with "(written by an agent) ", so
  you (and the subtask's own run, which reads it as its brief) can tell it
  from yours. You assign and start it.

The model never names a task: the board knows which task each role run's
session is working, and refuses a call from any other session. Agents can't
change owners or states, or touch any other task. Team specialists don't get
these tools in this version (each runs in its own session).

## Approvals park in the review queue

Board runs are unattended, like routines. A step that needs your approval
(in a `supervised` area) is parked in the review queue, `aivyx-pa review`
or the Studio's "Needs you" card, instead of blocking the run. The run goes
on without it; approve the parked step later and it runs then. Elsewhere, a
step that would need you is refused, and a role run that stops there goes
back to To do with the reason. Autonomy levels, budget caps and the audit
log apply as for any other run.

## The review notice

When a task reaches Review, the daemon sends one message to your default
notification target: `Task '<title>' is ready for review`, with the subject
`board: <task id>`. A failed run sends none.

## Restarts

When the daemon starts, every task left Running goes back to To do with a
`Run` entry "interrupted by a daemon restart". A role run's mission record
is closed as Cancelled. A team run's mission is halted: the team missions'
own restart sweep halts one that was executing, and the board halts one
that was paused or waiting at a gate (its gate is dropped), with the reason
"its board task was interrupted by a daemon restart". Start the task again
when you're ready.

## Agents and heartbeats

A **standing agent** is a named worker on the board that starts its own
tasks. You still write the work, assign it and review the results; the
agent saves you pressing Start. Amendment A21
(`docs/amendments/2026-11-19-board-agents.md`) records the design.

An agent has:

- a **name** (letters, digits, `-` and `_`, up to 64; unique) and an
  optional **title** ("Keeps the books");
- **works as**: `role:<name>`, `team` or `coder:<workspace>`. A task owned
  by the agent runs exactly as a task owned by that role, the team or that
  workspace would; the task keeps `agent:<name>` as its owner;
- **reports to**: another agent, optional. This only draws the **org
  chart**: `board agent list` prints a tree in name order, each agent under
  the one it reports to. A loop is refused when you make it; if one appears
  anyway, its members are listed after the tree, marked "(reports-to
  loop)";
- a **heartbeat** (a cron pattern, optional; six fields, seconds first,
  as for routines: `0 0 9 * * *` is 09:00 every day, in local time) and/or
  **wake on assign**.

### When an agent wakes

- The daemon's heartbeat loop **ticks every 15 s**, so times below are
  rounded up to the next tick.
- **Wake on assign:** assigning a task to the agent (`board assign`, or
  `task add --owner agent:<name>`) makes a wake due **30 s** later. Each
  later assignment pushes it back to 30 s from then, so a burst of
  assignments gives one wake.
- **Heartbeat:** the agent wakes when its cron pattern fires. There is **no
  catch-up**: after downtime, or a pause, only the most recent firing since
  the last wake counts, so it wakes once, not once per missed firing.
- **`board agent wake <name>`** wakes it now.

A wake starts the agent's To-do tasks **oldest first**, **one at a time**
(it waits for each run to end before starting the next), **at most 5** per
wake, and each task **at most once** per wake: a run that fails sends its
task back to To do, and the next wake tries it again. While one of the
agent's tasks is running (a run you started counts too), the loop doesn't
wake it; a wake that is due waits for that run to end. A second wake while
one is in progress does nothing.

### Pause

`board agent pause <name>` stops the agent from starting anything more: a
wake in progress stops before its next task, and a pending assignment wake
is dropped. A run already going finishes normally. `board agent resume
<name>` lets it wake again; a wake-on-assign agent that owns To-do tasks
gets a wake 30 s later, and a heartbeat agent wakes at its next tick if a
firing fell during the pause.

### Restarts and refused starts

After the daemon starts (and its sweep has returned interrupted tasks to To
do), every wake-on-assign agent that owns To-do tasks gets a wake 30 s
later. After a restart, a heartbeat agent wakes at the first tick if a
firing fell while the daemon was down (once; no catch-up).

If the board refuses to start a task when an agent wakes (for example, the
agent works as a role that isn't in your config, or as the team when no team
is set up), the task gets one comment, "the agent couldn't start this task:
<reason>", and the wake moves on. A later wake doesn't add the same comment
again while it is still the task's last entry.

### Managing agents

Only you create, edit, pause, resume, wake and remove agents, from the CLI
or the Studio; the board tools give runs no way to touch them. These count
as operator activity, like the other board commands. Removing an agent is
refused while it owns a To-do, Running or Review task: reassign them first.

## Give coding tasks to aivyx-coder

Some tasks are code work: change a project, run its tests, report back. A
task owned by `coder:<workspace>` is worked by
[aivyx-coder](https://github.com/Aivyx-Agent/aivyx-coder), the terminal
coding agent, running headless in a folder you registered. You review the
result like any other run's. Amendment A22
(`docs/amendments/2026-11-20-board-coder-workers.md`) records the design.

**Local only.** aivyx-pa and aivyx-coder are local-first and use only local
models. pa never passes a cloud credential to coder: it removes its own
secrets (`AIVYX_PA_*`) and the well-known cloud-provider key variables
(`OPENAI_API_KEY`, `ANTHROPIC_API_KEY` and the like) from coder's
environment. Point coder's own config (`~/.config/aivyx-coder/config.toml`,
`[backend] base_url`) at your local llama-server.

### Workspaces

A **workspace** is a folder aivyx-coder may work in. Only you register one,
from the CLI:

```
aivyx-pa board workspace add shop ~/src/shop
aivyx-pa board workspace list
aivyx-pa board workspace remove shop
```

- The name is letters, digits, `-` and `_`, up to 64, unique among
  workspaces. A relative path is made absolute against your current
  directory, and the daemon canonicalizes it (following symlinks) and stores
  that.
- Refused: a path that doesn't exist or isn't a folder; `/`; your home
  folder itself (a folder inside it is fine); pa's own data and config
  folders, anything inside them, and anything that contains them; a folder
  that is already registered.
- The folder is checked again at every Start, so a workspace whose folder
  has gone, or now resolves somewhere refused, doesn't start.
- `workspace list` prints `<name>  <path>  <busy|idle>  open <n>`: whether
  a coder run is working in it, and how many open tasks it owns.
- Removing a workspace is refused while a To-do, Running or Review task is
  owned by it, or while an agent works as it. To change a workspace's path,
  remove it and add it again.
- Board tools, runs, team specialists and federation peers can't register
  or remove a workspace, and nothing else ever supplies a path: a run can
  only go to a folder you registered.

### Coder owners and coder agents

`coder:<workspace>` is accepted wherever an owner is: `board assign <task>
coder:shop`, `task add --owner coder:shop`, and for a standing agent
`agent add|edit --works-as coder:shop`. An agent that works as a coder
workspace wakes as any other agent does (on assignment, on its heartbeat, or
on `agent wake`) and starts its tasks in that workspace. In the Studio the
owner picker and the agent form list "coder <workspace>".

### What a run does

A Start runs, in the workspace folder:

```
aivyx-coder --auto "<brief>" --headless --report <tmp>/report.json
```

with stdin closed and the last 64 KiB of its output kept. The brief is the
same as a role run's (the goal's, project's and task's titles, the
description and your notes). The binary is `$AIVYX_CODER` when it is set,
otherwise `aivyx-coder` on the daemon's `PATH`; if there is neither, Start
is refused ("aivyx-coder isn't installed"). Each run gets a mission record
labelled `coder:<workspace name>`, which shows under Missions.

How it ends is decided by coder's exit code:

| Exit | Task goes to | Thread note |
|---|---|---|
| 0 | Review (Done with `--no-review`) | coder's stop notice |
| 1 | Review (Done with `--no-review`) | the notice + " — a check failed" |
| 2 | To do | "coder's budget ran out" |
| 3 | To do | "coder failed: <its last stderr lines>" |
| 4 | To do | "coder stopped: possible prompt injection" |
| 130, or Cancel | Cancelled | — |
| couldn't start | To do | the error |
| anything else, or a signal | To do | "coder exited unexpectedly (<code>)" |

The run entry's token count is the report's prompt plus completion tokens,
and its answer ends with "N changed files; N checkpoints — undo with /undo
in aivyx-coder". If coder wrote no report, the exit code still decides and
the note says "(coder wrote no report)".

Things to know:

- **coder's own config and sandbox decide what it may do.** pa adds no
  scopes and no allow-list: coder's `[permissions]`, deny list and kernel
  sandbox (Landlock and seccomp on Linux) apply as in any headless run, and
  it edits only inside the workspace. A headless `--auto` run asks nobody,
  so its unattended limits apply (no `run_shell`, no commits).
- **`--auto` needs a test command.** coder refuses to start an autonomous
  run without one: set `[verification] command` (naming a
  `[[permissions.allowed_commands]]` entry) in coder's config, or have a
  project whose test command coder detects (`cargo test` for a Cargo
  project, and so on). Without one the task goes back to To do with coder's
  error.
- **Undo** is coder's: each run saves a coder session with its
  checkpoints. To take a run's work back, open that session in the
  workspace with `aivyx-coder --resume` (the latest session; `/sessions`
  lists older ones) and type `/undo`, which shows the files and asks first.
  The checkpoints are also plain git refs (`git for-each-ref
  refs/aivyx/checkpoints/`). Send back puts the task in To do with your
  note, and the next run starts from the folder as it is now.
- **Cancel, the ceiling and a restart don't undo anything:** edits coder
  made before it stopped stay in the folder. Use `/undo` as above.
- `board show` cuts a run's answer at 200 characters, so with a long
  notice the counts at its end may not show there.
- **A very long brief** (over about 128 KiB, the system's limit on one
  argument) can't be passed, so the start fails and the task goes back to
  To do with the error.

### Limits and cancel

- **One run per workspace.** A second Start in a workspace with a run going
  is refused: "the workspace is busy". Runs in different workspaces go side
  by side.
- **A 4 h ceiling.** A run still going after 4 hours gets SIGTERM, which
  makes coder stop cleanly and write its report; if it hasn't exited 30 s
  later, it is killed. The task goes back to To do, "coder ran past the run
  ceiling and was stopped". coder's own `[autonomous] max_duration_secs`
  (an hour by default) usually stops it first, with exit 2.
- **Cancel** sends SIGTERM the same way (a kill 30 s later), and the task
  ends Cancelled.
- **Daemon stop or restart** kills the coder process; at startup the
  restart sweep puts the task back in To do, "interrupted by a daemon
  restart", and closes its mission record, as for role runs.
- **On macOS**, a hard kill (after the 30 s grace) can leave behind a
  command coder had started (a test run, say): coder's clean-up of its own
  children when it dies is Linux-only.

### No turn lock: share a llama with aivyx-broker

A coder run doesn't take the daemon's turn lock, so chat, routines and
other board runs carry on while it works (a lock held for hours would block
them all). When pa and coder share one llama-server, run
[aivyx-broker](https://github.com/Aivyx-Agent/aivyx-broker) to coordinate
their access to the GPU slots, and point both at it (coder's
`[backend] kind = "llama_server_broker"` with `[broker] base_url`).

## Audit

Every change writes one `BoardChanged { op, object, actor }` event:
`op` is a fixed word (`created`, `assigned`, `started`, `finished`,
`approved`, `sent_back`, `cancelled`, `commented`, `subtask_added`,
`interrupted`, for agents `agent_created`, `agent_edited`,
`agent_paused`, `agent_resumed`, `agent_removed` and `woke`, and for
workspaces `workspace_added` and `workspace_removed`), `object` is an id and
`actor` is `operator` or `agent`. A workspace event's object is the
workspace id: no path or name goes into the log. A coder run writes
`started` and `finished` like any other run; coder's own output stays in
the task's thread and coder's own logs, not in pa's audit chain. A wake is `woke` with the agent's
id and actor `operator`; each start it makes is `started` with actor
`agent`. No title, description, comment text or agent name goes into the
audit log.
`aivyx-pa audit export --event-type board_changed` lists them.

## The CLI

`aivyx-pa board` talks to the running daemon:

```
aivyx-pa board list [--project <id>] [--state todo|running|review|done|cancelled]
aivyx-pa board show <task>
aivyx-pa board goal add "<title>" [--description "<text>"]
aivyx-pa board goal list
aivyx-pa board project add <goal> "<title>" [--description "<text>"]
aivyx-pa board project list [--goal <id>]
aivyx-pa board task add (--project <id> | --parent <task>) "<title>"
    [--description "<text>"]
    [--owner role:<name>|team|agent:<name>|coder:<name>] [--no-review]
aivyx-pa board agent list
aivyx-pa board agent add <name> --works-as role:<name>|team|coder:<name>
    [--title "<t>"] [--reports-to <agent>] [--heartbeat "<cron>"]
    [--wake-on-assign]
aivyx-pa board agent edit <name> [--name <new>] [--title "<t>"]
    [--reports-to <agent>|--no-reports-to]
    [--works-as role:<name>|team|coder:<name>]
    [--heartbeat "<cron>"|--no-heartbeat]
    [--wake-on-assign|--no-wake-on-assign]
aivyx-pa board agent pause|resume|remove|wake <name>
aivyx-pa board workspace list
aivyx-pa board workspace add <name> <path>
aivyx-pa board workspace remove <name>
aivyx-pa board assign <task> role:<name>|team|agent:<name>|coder:<name>|none
aivyx-pa board start <task>
aivyx-pa board approve <task>
aivyx-pa board send-back <task> "<why>"
aivyx-pa board cancel <task>
aivyx-pa board comment <task> "<text>"
```

Ids are `g-`, `p-`, `t-`, `a-` and `w-` followed by eight hex characters;
the agent and workspace commands, and `coder:<name>`, take names. `board agent list` prints the org
chart, one line per agent: its name, title, works-as, heartbeat,
`[paused]`, the last wake and how many open tasks it owns. `board show`
prints the thread; a run entry reads
`run <mission id> succeeded, <n> tokens: <answer>` (with `—` in place of
`<n> tokens` when the run wasn't metered).

## The Studio

**Board** sits under Missions in the sidebar, with a badge counting the
tasks in Review. The view has four columns (To do, Running, Review, Done),
filters by goal and project, and forms to add a goal, a project and a task.
Clicking a task opens its panel: the brief, the owner (type the role's
name), the thread with each run's answer and mission id, a comment box, and
the buttons that apply in its state: Start, Approve, Send back (asks for a
note) and Cancel. The owner picker lists your agents next to the roles and
the team. The view refreshes every 5 seconds while it's open.

A **Tasks | Agents** switch at the top of the view shows the agents
instead: the org chart as a tree, each agent with its works-as, heartbeat,
a paused badge and its open tasks, and **Pause**/**Resume**, **Wake now**
and **Edit** buttons, with a form to add or edit an agent below. There is
no Remove button in the Studio: use `aivyx-pa board agent remove`.

The Agents tab also lists your **workspaces** (name, path, busy or idle,
open tasks), read-only: you add and remove them with `aivyx-pa board
workspace`. The owner picker and the agent form offer "coder <workspace>"
for each one, and a coder run's thread entry shows coder's notice, the
changed-file and checkpoint counts and the mission id.

## Coming later

Not in this version: agents that add tasks or plan for the agents that
report to them; budgets per agent and per project that pause work; cron
catch-up; several coder runs in one workspace, per-workspace budgets and
remote workspaces; your other PA machines (federated peers) as workers; a TUI board view; drag-and-drop; anything
multi-user.
