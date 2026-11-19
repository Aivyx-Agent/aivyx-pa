# Amendment A21 — Standing Agents and Heartbeats

**Date:** 2026-11-19
**Phase:** Chapter 6 (Factory), the work board, second step.
**Extends:** **A20** (the work board): one new record kind in the existing
`KeyDomain::Board`, one new task owner, the audit op set, and the IPC request
set.
**Leaves unchanged:** D1 (the turn loop contract), D2, D3, D4 (no new
capability), D5 (tiers), D6, D7 (no new storage domain) and A20's runs.
**Reference design:** `docs/superpowers/specs/2026-11-19-board-agents-design.md`
(in `aivyx-ecosystem`).

---

## Why

On the A20 board every task waits for the operator to press Start. Tasks
should be able to start themselves: a standing agent is a named worker that
wakes when work is assigned to it, or on a schedule, and starts its own To-do
tasks. The operator still writes and assigns the work and still reviews the
results.

## The agent record

An agent is a JSON record in `KeyDomain::Board` under the key `agent\0<id>`.
There is no new domain, so the encrypted domain count stays at 29. Fields:
`id` (`a-` plus 8 hex characters), `name` (`[A-Za-z0-9_-]{1,64}`, unique),
`title`, `reports_to` (another agent's id; a loop is refused), `works_as`
(`Role { name }` or `Team`), `heartbeat` (an optional cron pattern),
`wake_on_assign`, `paused`, `last_wake` (ms), `created_at` and `updated_at`.
Removing an agent is refused while it owns a To-do, Running or Review task.

## `Owner::Agent`

`Owner::Agent { id }` joins `Role` and `Team`, serde-tagged the same way, so
old rows decode unchanged. Starting a task owned by an agent runs it as the
agent's `works_as`; the task keeps `Owner::Agent`.

## The heartbeat loop

One loop in the daemon ticks every **15 s**. An assignment wake is due
**30 s** after the assignment. A wake starts the agent's To-do tasks oldest
first, each at most once per wake and at most **5** per wake. An agent runs
one task at a time and a paused agent never wakes. Cron has **no catch-up**:
only the most recent firing after `last_wake` counts. After the board's
restart sweep, every `wake_on_assign` agent that owns To-do tasks is woken 30
s after startup. A start the board refuses (an unknown role, no team) adds one
operator comment to the task and the wake moves on.

## Audit

`BoardChanged` gains the ops `agent_created`, `agent_edited`, `agent_paused`,
`agent_resumed`, `agent_removed` and `woke` (object: the agent id; actor:
`operator`). A start from a wake is `started` with actor `agent`. No free
text goes into the chain.

## IPC

The board query family gains the agent queries: list and get, create, edit,
pause, resume and remove. `BoardSnapshot` carries the agents. The types live
in `aivyx-ipc` and stay wasm-clean.

## Who may change agents

The operator only, through IPC, the CLI and the Studio. The board tools never
touch agents.

## Not in A21

Agents adding tasks; lead-agent planning over `reports_to`; per-agent and
per-project budgets; other runtimes (aivyx-coder, federated pa machines) as
workers; cron catch-up.
