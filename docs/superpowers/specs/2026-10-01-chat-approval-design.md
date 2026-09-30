# Chat approval: pause, ask, continue

Date: 2026-10-01 · Status: approved design, not yet implemented

## Problem

When a tool call in an ordinary chat turn needs the operator's approval — a
delete or overwrite (`fs.*`, the data writers), a `git.commit`, a bridged
confirm-first tool (`kitchen.order.send`), or a withheld integration write
(email, Drive, Calendar, Obsidian, …) — there is no way to say yes and carry on.
Only mission-wrapped turns get an `ApprovalGate`; a plain chat turn ends
`Escalated`, and v0.12.1/0.12.2 fall back to "your next message approves"
(`aivyx_core::confirm::OperatorConfirmations`, one ledger per tool plus one in
the agent). The model then re-issues the call from scratch, with arguments you
never saw. Separately, the daemon handles a connection one frame at a time and
awaits a chat turn inline, so nothing sent during a turn — including
`CancelTurn` (Ctrl-C) — is read until the turn ends.

## Goal

In the terminal chat (daemon and in-process) and the Studio (and so the desktop
app), a call that needs approval pauses the turn, shows what it will do, and
waits for Approve / Deny. Approved: the exact paused call runs and the turn
continues. Denied or no answer in 10 minutes: the model is told the operator
declined and the turn continues. Chat apps (Telegram/Discord/Slack) keep the
reply-approves rule in this version.

Out of scope: chat-app approval (`/approve` in Telegram etc.), "always allow for
this conversation", wiring the autonomy dial's gate posture (manual = confirm
everything), missions (their gate flow is unchanged).

## Design

### 1. The approval hook

`ChannelContext` gains a default async method:

```rust
async fn request_approval(&self, request: &ApprovalRequest) -> Approval {
    Approval::Unavailable
}
```

- `ApprovalRequest { tool: String, summary: String, input: Value, reason: String }`
- `Approval::{Approved, Denied, TimedOut, Unavailable}`

A default method keeps every existing channel compiling and behaving as today —
the same non-amendment precedent as `session_partition` under D2.

### 2. One rule, in the agent

The agent's `run_tool_call` owns operator confirmation; the tool-level ledgers
(`fs.rs`, `git.rs`, `aivyx-dataread`'s `ReaderSandbox`, `aivyx-tool`'s
`ToolProxy`) are removed.

- **Confirm-first tools** are those whose input schema declares `confirmed`.
  For them the agent strips a model-supplied `confirmed: true` before
  dispatch, unless the operator approved this call (below). The data writers
  (`data.xlsx.write`, `data.pdf.write`) gain the `confirmed` property: under
  `[access] confirm_destructive`, replacing an existing file needs `overwrite`
  **and** an operator `confirmed`.
- **Tools signal "needs approval" with `ToolOutcome::RequiresEscalation`**
  (`fs.delete`, `fs.write` overwrite, `git.commit`, the data writers switch to it
  from `Failed`; `kitchen.order.send` already uses it). The agent-level withheld
  integration gate already produces it. No new outcome variant, so D3 is
  unchanged.
- **On `RequiresEscalation` in an interactive turn**, the agent calls
  `channel.request_approval(...)`:
  - `Approved` → re-dispatch the same call with the original input plus
    `confirmed: true` (for confirm-first tools) or with the integration gate
    passed (for withheld integration tools). The re-dispatch goes through the
    normal capability check → audit → execute → audit path (D1).
  - `Denied` / `TimedOut` → the model receives a tool result "the operator
    declined this action" and the turn continues.
  - `Unavailable` → today's behaviour: the turn ends `Escalated`, and the
    agent's `OperatorConfirmations` ledger (session + tool name; the model
    regenerates arguments, so a per-argument key would rarely match) lets the
    re-issued call run once in the operator's next turn. The ledger records a
    refusal only when the tool actually escalated.
- **Unattended runs** (`GatePolicy::RejectAndAbort`) never ask: refused, as now.
- **Audit:** new audit events `ApprovalRequested { tool, summary }` and
  `ApprovalResolved { tool, outcome }` (additive `AuditEvent` variants, same
  shape discipline as the existing additive variants), emitted around the wait.
- **Time:** the turn's wall-clock deadline is paused while waiting for an
  answer.

`summary` is built by the agent: tool name plus its key argument (`path`,
`repo`, `to`/recipient, `purchase_order_id`, else the first string field), e.g.
`fs.delete todo.md`.

### 3. Daemon and protocol

- **Concurrent connection reading.** Each daemon connection gets a reader task
  that decodes frames onto an mpsc queue. A chat turn runs concurrently with
  the connection loop: while it runs, `CancelTurn` and `ResolveApproval` are
  acted on immediately; every other frame waits until the turn ends
  (unchanged ordering). Applies to every connection kind. This also makes
  mid-turn Ctrl-C effective.
- **New messages** (additive; `docs/DAEMON_IPC.md` updated):
  - frontend → daemon: `FrontendMessage::SetApprovals { enabled }` — a frontend
    that can show the prompt opts in once per connection (the terminal chat and
    the Studio's web server do). Every other connection — the TUI, chat-app
    frontends, one-shot `--headless` — never gets a request: its bridge reports
    `Unavailable`, so nothing ever waits 10 minutes on a client that can't
    answer.
  - daemon → frontend: `StreamEventPayload::ApprovalRequest { request_id, tool,
    summary, input, reason, expires_in_secs }`
  - frontend → daemon: `FrontendMessage::ResolveApproval { request_id, approved }`
- **`IpcChannelBridge::request_approval`** sends the event and awaits a oneshot
  keyed by `request_id`, resolved by `ResolveApproval`; 10 minutes → `TimedOut`;
  connection closed or turn cancelled → `Denied`.
- **Connections that didn't send `SetApprovals`** (chat-app daemon frontends,
  the TUI, headless submits) report `Unavailable`. Missions unchanged.
- **Terminal client:** today `DaemonSession::send_and_collect` buffers every
  event until `TurnComplete`; it gains an optional approver hook called when an
  `ApprovalRequest` arrives mid-turn, then sends `ResolveApproval`.

### 4. Surfaces

- **Terminal, daemon mode** (`daemon_session` / `repl_daemon`): on
  `ApprovalRequest`, print

  ```
  ⚑ Approval needed — fs.delete todo.md
    why: deleting a file can't be undone
    Approve? [y/N] (10 min)
  ```

  with the arguments when short (trimmed with "…" when long). `y` approves;
  Enter or anything else denies; Ctrl-C denies and cancels the turn.
- **Terminal, in-process** (`LocalChannel`): same prompt, answer read from the
  controlling terminal (`/dev/tty`, as passphrase prompts do) because the chat
  loop holds stdin. Non-interactive (piped) input → `Unavailable`. The reader
  is injectable for tests.
- **Studio Chat** (and the desktop app): an approval card in the transcript
  (summary, reason, expandable arguments, countdown, Approve / Deny). After an
  answer it stays in the transcript marked "approved" / "declined". A card
  whose request was denied by disconnect or timeout shows that.

## Testing

- Agent unit tests with a fake channel scripting `request_approval`: approved
  → the exact call runs once with `confirmed`; denied / timed out → "declined"
  result and the turn continues; unavailable → `Escalated`, re-issued call
  passes next turn; unattended → refused; model-supplied `confirmed: true`
  always stripped; both audit events present; the deadline does not run during
  the wait.
- Tool tests updated: `fs` / `git` / data writers / proxy return
  `RequiresEscalation` without `confirmed`, and run with it; the ledger tests
  move to the agent.
- Daemon end-to-end (real IPC server, scripted provider): `ApprovalRequest`
  arrives and `ResolveApproval` resumes the same turn; disconnect mid-wait →
  denied; mid-turn `CancelTurn` cancels (regression test for the connection
  fix); a frame sent during a turn is handled after it.
- Live: terminal (pty harness) and Studio (headless Chrome), real Lemonade
  model, deleting a file — approve and deny.

## Docs and rollout

`docs/DAEMON_IPC.md`, the guide's *Chat & missions* and *Access & settings*
pages, `docs/SECURITY_POSTURE.md`, `CHANGELOG.md`. One aivyx-pa release; no
config changes; chat apps unchanged.
