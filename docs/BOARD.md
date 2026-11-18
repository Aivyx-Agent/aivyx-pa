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
roster, or an installed pack's team), or by nobody yet.

**A role run** is one turn of the daemon's agent:

- One turn: at most 32 steps and 120 seconds. Longer work belongs on a
  team-owned task.
- It uses the role's **scopes**, not its system prompt. The run is capped
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
`per_mission_usd`); otherwise its run entry shows 0 tokens.

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
  and assign owners. A run's end moves it out of Running.
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
- **Cancel** stops a running run (a role run's turn is cancelled, a team
  run's mission is aborted, or its waiting gate rejected) and closes the
  task. A team mission **you paused yourself** (from the team view) is not
  ended by a board Cancel: the task is cancelled, but end the mission from
  the team view (`aivyx-pa team abort`).

## What agents can do

In a role run, and only there, the agent has two tools, both under the
`board.write` scope:

- `board.comment { text }`: add a note to the run's own task;
- `board.subtask { title, description }`: add an unowned subtask under the
  run's own task. You assign and start it.

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
is closed as Cancelled; a team run's mission is already halted by the team
missions' own restart sweep. Start the task again when you're ready.

## Audit

Every change writes one `BoardChanged { op, object, actor }` event:
`op` is a fixed word (`created`, `assigned`, `started`, `finished`,
`approved`, `sent_back`, `cancelled`, `commented`, `subtask_added`,
`interrupted`), `object` is an id and `actor` is `operator` or `agent`. No
title, description or comment text goes into the audit log.
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
    [--description "<text>"] [--owner role:<name>|team] [--no-review]
aivyx-pa board assign <task> role:<name>|team|none
aivyx-pa board start <task>
aivyx-pa board approve <task>
aivyx-pa board send-back <task> "<why>"
aivyx-pa board cancel <task>
aivyx-pa board comment <task> "<text>"
```

Ids are `g-`, `p-` and `t-` followed by eight hex characters. `board show`
prints the thread; a run entry reads
`run <mission id> succeeded, <n> tokens: <answer>`.

## The Studio

**Board** sits under Missions in the sidebar, with a badge counting the
tasks in Review. The view has four columns (To do, Running, Review, Done),
filters by goal and project, and forms to add a goal, a project and a task.
Clicking a task opens its panel: the brief, the owner (type the role's
name), the thread with each run's answer and mission id, a comment box, and
the buttons that apply in its state: Start, Approve, Send back (asks for a
note) and Cancel. The view refreshes every 5 seconds while it's open.

## Coming later

Not in this version: standing agents woken by heartbeats; budgets per agent
and per project that pause work; aivyx-coder and your other PA machines
(federated peers) as workers; a TUI board view; drag-and-drop; anything
multi-user.
