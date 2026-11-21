# Amendment A22 — aivyx-coder as a Board Worker

**Date:** 2026-11-20
**Phase:** Chapter 6 (Factory), the work board, third step.
**Extends:** **A20** (the work board) and **A21** (standing agents): one new
record kind in the existing `KeyDomain::Board`, one new task owner, one new
agent `works_as`, a new run kind, the audit op set, and the IPC request set.
**Leaves unchanged:** D1 (the turn loop contract), D2, D3, D4 (no new
capability), D5 (tiers), D6, D7 (no new storage domain), and A20's role and
team runs.
**Reference design:** `docs/superpowers/specs/2026-11-20-board-coder-workers-design.md`
(in `aivyx-ecosystem`).

---

## Why

Some board tasks are code work: change a project, run its checks, report back.
`aivyx-coder` already does that headless. A task should be able to say "this
one goes to the coder, in that folder", and the operator reviews the result
like any other run's.

## The workspace record

A workspace is a JSON record in `KeyDomain::Board` under the key
`workspace\0<id>`. There is no new domain, so the encrypted domain count stays
at 29. Fields: `id` (`w-` plus 8 hex characters), `name`
(`[A-Za-z0-9_-]{1,64}`, unique among workspaces), `path` (absolute,
canonicalized at registration) and `created_at`.

Registration is refused for a path that doesn't exist or isn't a folder; `/`;
the operator's home folder itself (`$HOME`, canonicalized; a folder inside it
is fine); and pa's own data and config folders, or any path inside them, or
any path that contains them. Removal is refused while an open task (To do,
Running, Review) is owned by the workspace, or while an agent works as it.

## `Owner::Coder` and `WorksAs::Coder`

`Owner::Coder { workspace }` and `WorksAs::Coder { workspace }` (the workspace
id) join the existing variants, serde-tagged the same way, so old rows decode
unchanged. A standing agent working as a coder keeps `Owner::Agent` on its
tasks; a start resolves it to the workspace.

## The coder run

`RunKind::Coder`. A start spawns
`aivyx-coder --auto <brief> --headless --report <tmpdir>/report.json` with the
workspace path as its current directory, stdin null, stdout and stderr
captured (the last 64 KiB kept) and `kill_on_drop`. The binary is
`AIVYX_CODER` when set and non-empty, otherwise `aivyx-coder` on `PATH`.

Exit codes: 0 goes to Review (Done with review off) with the report's notice;
1 the same, with "a check failed" added; 2 back to To do, "coder's budget ran
out"; 3 back to To do with the last stderr lines; 4 back to To do, "possible
prompt injection"; only pa's own Cancel is Cancelled; 130 without one (a
daemon stop, restart or Ctrl-C reaching coder) is To do, "coder was stopped
by a signal (130)", so the task isn't left terminal; a spawn error, any other
code, or death by a signal ("killed by a signal") is To do with the reason.
Tokens are the report's prompt plus completion tokens; the answer starts with
the changed-file and checkpoint counts and the undo hint (`aivyx-coder
--resume` in the workspace, then `/undo`), then the notice or the reason. A
missing report doesn't fail the run by itself.

The child runs in its own process group (a terminal Ctrl-C to pa doesn't reach
it directly) and, on Linux, with `PR_SET_PDEATHSIG` = SIGTERM, so a pa crash
stops it; on macOS a hard kill or a pa crash can leave coder or a command it
started running. pa removes `AIVYX_PA_*`, a list of cloud-provider credential
variables, and any name ending in `_API_KEY`, `_API_TOKEN`,
`_SECRET_ACCESS_KEY` or `_SESSION_TOKEN` from the child's environment. An
agent-written subtask's title is marked in the brief ("Task (written by an
agent)"), as its description already is. Removing a workspace is also refused
while a coder run is still in it.

Limits: one coder run per workspace (a second Start is refused, "the
workspace is busy"); a 4 h ceiling, then SIGTERM and, 30 s later, a kill;
coder runs **do not take the shared turn lock**. Each run gets a
`MissionRecord` labelled `coder:<workspace name>`, closed on finish like a
role run's, and the restart sweep covers coder runs the same way.

## Audit

`BoardChanged` gains the ops `workspace_added` and `workspace_removed`
(object: the workspace id, no paths; actor `operator`). A coder run writes
`started` and `finished` like any other run. No free text goes into the chain.

## IPC

The board query family gains the workspace queries: list, add and remove.
`BoardSnapshot` carries the workspaces (`#[serde(default)]`, so an old
snapshot decodes). The types live in `aivyx-ipc` and stay wasm-clean.

## Who may change workspaces

The operator only, through IPC and the CLI. Board tools and runs never
register or remove a workspace, and nothing else supplies a path.

## Not in A22

Coder runs that spawn further runs; several coder runs in one workspace;
per-workspace budgets; remote or federated workspaces; the coder's own
approval prompts (a headless run is `--auto`); changing a workspace's path
(remove and add again).
