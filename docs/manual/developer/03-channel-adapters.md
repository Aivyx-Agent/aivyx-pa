# Channel adapters

A channel adapter connects some way of talking — a chat platform, a
device, a custom UI — to the daemon. The built-in ones (terminal, Studio,
Telegram, Discord, Slack, voice) use exactly the interface described here,
and you can write one in any language.

The full contract is
[`docs/CHANNEL_SDK.md`](https://github.com/Aivyx-Agent/aivyx-pa/blob/main/docs/CHANNEL_SDK.md);
this page is the overview.

## What an adapter is

A process, on the same machine, that:

1. connects to the daemon's socket —
   `$XDG_RUNTIME_DIR/aivyx-pa/daemon.sock` (or
   `~/.local/share/aivyx-pa/daemon.sock` without `XDG_RUNTIME_DIR`; a
   [named instance](../../guide/16-named-instances.md) uses
   `…/aivyx-pa/instances/<name>/daemon.sock`),
2. reads and writes **length-prefixed JSON frames** (a 4-byte big-endian
   length, then UTF-8 JSON; at most 16 MiB),
3. turns user input into `SubmitInput` frames, and the daemon's
   `StreamEvent` / `TurnComplete` frames back into messages on its
   platform.

That's all. Capability checks, auditing, cancellation and role handling
happen in the daemon, for every adapter alike.

## Lifecycle

1. Connect. The daemon sends `DaemonReady { version }`.
2. Optionally negotiate the protocol version.
3. `StartSession { role, frontend_type }` → `SessionStarted { session_id }`.
4. For each message: `SubmitInput` → a stream of `StreamEvent` frames
   (text as it's generated, tool calls, approvals) → `TurnComplete`.
5. `CancelTurn` stops the turn in progress; closing the socket ends the
   session.

## Trust

There is no token or password: if a process can open the `0600` socket, it
is the operator. What an adapter brings is a **trust tier**, taken from the
`FrontendType` it declares — `Local` and `Web` are Trusted; `Telegram`,
`Discord` and `Slack` are SemiTrusted, which rules out `shell.exec` and
`fs.delete` and narrows file and network scopes. The tier caps what any
role can do over that channel. An adapter for a remote platform should
also allowlist who may talk to it, as the built-in ones do (`chat_id`,
`channel_filter`).

## Start from an example

- [`examples/python-channel/`](https://github.com/Aivyx-Agent/aivyx-pa/tree/main/examples/python-channel)
  — a minimal terminal adapter in plain Python, with a conformance suite
  (`python3 -m unittest discover examples/python-channel/tests`) that needs
  no daemon.
- `crates/aivyx-telegram/`, `crates/aivyx-discord/`, `crates/aivyx-slack/`
  — complete Rust adapters, including allowlists, approvals in chat and
  `/cancel`.

The message types themselves are defined once, in
`crates/aivyx-ipc/src/protocol.rs`; see [IPC protocol](06-ipc-protocol.md).
