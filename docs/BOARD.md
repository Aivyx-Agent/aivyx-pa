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
(`agent:<name>`, see [Agents and heartbeats](#agents-and-heartbeats)), or by
nobody yet. An agent-owned task runs as the agent's role or the team.

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
- **works as**: `role:<name>` or `team`. A task owned by the agent runs
  exactly as a task owned by that role or by the team would; the task keeps
  `agent:<name>` as its owner;
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
later. A heartbeat agent just waits for its next firing.

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

## Audit

Every change writes one `BoardChanged { op, object, actor }` event:
`op` is a fixed word (`created`, `assigned`, `started`, `finished`,
`approved`, `sent_back`, `cancelled`, `commented`, `subtask_added`,
`interrupted`, and for agents `agent_created`, `agent_edited`,
`agent_paused`, `agent_resumed`, `agent_removed` and `woke`), `object` is an
id and `actor` is `operator` or `agent`. A wake is `woke` with the agent's
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
    [--description "<text>"] [--owner role:<name>|team|agent:<name>] [--no-review]
aivyx-pa board agent list
aivyx-pa board agent add <name> --works-as role:<name>|team [--title "<t>"]
    [--reports-to <agent>] [--heartbeat "<cron>"] [--wake-on-assign]
aivyx-pa board agent edit <name> [--name <new>] [--title "<t>"]
    [--reports-to <agent>|--no-reports-to] [--works-as role:<name>|team]
    [--heartbeat "<cron>"|--no-heartbeat] [--wake-on-assign|--no-wake-on-assign]
aivyx-pa board agent pause|resume|remove|wake <name>
aivyx-pa board assign <task> role:<name>|team|agent:<name>|none
aivyx-pa board start <task>
aivyx-pa board approve <task>
aivyx-pa board send-back <task> "<why>"
aivyx-pa board cancel <task>
aivyx-pa board comment <task> "<text>"
```

Ids are `g-`, `p-`, `t-` and `a-` followed by eight hex characters; the
agent commands take the agent's name. `board agent list` prints the org
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

## Coming later

Not in this version: agents that add tasks or plan for the agents that
report to them; budgets per agent and per project that pause work; cron
catch-up; aivyx-coder and your other PA machines
(federated peers) as workers; a TUI board view; drag-and-drop; anything
multi-user.
