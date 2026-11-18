# Amendment A20 — The Work Board

**Date:** 2026-11-18
**Phase:** Chapter 6 (Factory), the work board.
**Extends:** **D7** (one new encrypted storage domain), **D4** (one new
capability base), the audit event set, and the IPC request set.
**Leaves unchanged:** D1 (the turn loop contract), D2, D3, D5 (tiers), D6,
and the existing mission and team-mission records.
**Reference design:** `docs/superpowers/specs/2026-11-18-factory-board-design.md`
(in `aivyx-ecosystem`).

---

## Why

The operator has roles, teams, missions and routines, but no place to say
what work there is and who should do it. The work board is that place, for
the operator's own agents: goals, projects and tasks, each task owned by a
role or by the team, with a thread of comments and run results. The operator
writes and assigns the work; agents work it. This amendment records the
decisions that need an architecture change. The code lands in later tasks.

## D7 storage

A new domain, `KeyDomain::Board` (`as_bytes()` = `b"board"`, table
`aivyx_board_v1`), holds goals, projects and tasks as JSON records. Encrypted
domains go **28 → 29**. The HKDF subkey is derived like every other domain's.
The domain's format lines in `docs/stable/formats.txt` are tagged `PREVIEW`
while the board is a preview.

## D4 capability

One new base, `board.write`, unqualified. It is Trusted-tier only, **not**
irreversible and not read-only: it changes things, but only inside the board.
The daemon agent holds it in daemon mode through the backcompat floor (the
grant lands in a later task; this amendment only documents it). It is useful
only in a session the board registered for a run: the tools it gates work on
that run's own task and are refused anywhere else. The count moves
**102 → 103**.

## Runs

- A **role run** is one System-origin agent turn. Its channel's capability
  ceiling is the role's own scopes plus `board.write`. It is headless: a step
  that needs approval is parked for the operator's review, and the run does
  not block on it.
- A **team run** is a team mission whose `triggered_by` is `board:<task id>`.
- Start is refused while a task is running, from a terminal state, and with
  no owner.

## Audit

One new event, `BoardChanged { op, object, actor }`, per board change. `op`
is a fixed word, `object` an id, `actor` is `operator` or `agent`. It never
carries a title, description or comment text.

## IPC

The board query family: list and get, create and edit, assign, start,
approve, send back and cancel. The requests live in `aivyx-ipc` and stay
wasm-clean.

## Not in A20

Standing agents and heartbeats; per-agent and per-project budgets with
auto-pause; other runtimes (aivyx-coder, federated pa machines) as workers;
multi-user.
