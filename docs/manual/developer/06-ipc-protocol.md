# IPC protocol

How front ends talk to the daemon. The authoritative description, with
every message and its addenda, is
[`docs/DAEMON_IPC.md`](https://github.com/Aivyx-Agent/aivyx-pa/blob/main/docs/DAEMON_IPC.md);
the types themselves are in `crates/aivyx-ipc/src/protocol.rs`.

## Transport

A Unix domain socket:

| Instance | Socket |
|---|---|
| default | `$XDG_RUNTIME_DIR/aivyx-pa/daemon.sock`, else `~/.local/share/aivyx-pa/daemon.sock` |
| named `<n>` | `…/aivyx-pa/instances/<n>/daemon.sock` |

The daemon creates it with mode `0600` and, on Linux, checks the peer's
user id. Anyone who can open it is the operator; there is no other
authentication. A missing socket or a refused connection means no daemon
is running.

## Framing

Each frame is a 4-byte big-endian length followed by that many bytes of
UTF-8 JSON, at most 16 MiB. No compression, no TLS — it never leaves the
machine. The same framing is used between the daemon and
[tool processes](04-tool-processes.md), over their stdin and stdout.

## Message families

- **Session and turns** — `StartSession`, `SubmitInput` and `CancelTurn`
  from the front end; `SessionStarted`, `StreamEvent` and `TurnComplete`
  from the daemon. Closing the connection ends the session.
- **Streaming** — `StreamEvent` carries a `StreamEventPayload`: text as it's
  generated, tool calls and results, approval requests, routing decisions.
  Every front end renders the same events.
- **Queries** — a `Query` / `QueryResponse` pair for reading state without
  a turn: the audit log, missions, schedules, tool and MCP statistics,
  routing state, the Command Center briefing and more.
- **Approvals** — opt-in per connection. A front end that sends
  `SetApprovals { enabled: true }` gets an `ApprovalRequest` stream event
  when a call needs the operator, and answers with
  `ResolveApproval { request_id, approved }`; the paused call then runs (or
  is declined) and the turn carries on. No answer within 600 seconds, a
  cancelled turn or a closed connection denies it. The terminal chat and
  the Studio opt in; a connection that doesn't sees such calls end the
  turn as escalated instead.
- **Lifecycle** — `DaemonReady { version }` on connect, and out-of-band
  daemon events.

## Versioning

`DaemonReady` carries the protocol version. Today a mismatch is a warning,
not an error; that tightens once the protocol is declared stable. New
fields and messages are added compatibly, and the addenda in DAEMON_IPC.md
record each one.

## The Studio speaks it too

The Studio is a WebAssembly app. The daemon bridges its WebSocket to the
same protocol, and `aivyx-ipc` is kept free of anything that won't compile
to WebAssembly so the browser and daemon share one set of types. The
WebSocket accepts only allowlisted origins and needs the Studio sign-in
token.
