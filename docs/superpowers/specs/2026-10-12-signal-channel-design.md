# Signal channel and `signal.send` — design

Status: approved in brainstorming, 2026-10-12. Sub-project 3 of 3 of the
private-channels work (Matrix — done; Proton Mail — done; Signal — this
spec).

## Goal

The operator messages their assistant on Signal and gets the answer back
there; and the agent can send a Signal message to a contact the operator
listed, with approval each time. The assistant has **its own Signal number**.

## Decisions

- **signal-cli as a separate process, run by the operator.** libsignal and
  presage are AGPL and can't be linked into BUSL-1.1 code; signal-cli (GPL)
  runs as its own daemon and pa talks JSON-RPC to it over a Unix socket. It
  is the same shape as Proton Mail Bridge: a pa crash or restart never
  touches the Signal session, and pa holds no Signal credential (signal-cli
  holds the account).
- **One shared signal-cli** for the channel and the tool: signal-cli locks
  its account data, so they can't each start one.
- **Trust:** a turn only from an allowed number whose identity key the
  operator has **verified** (safety number compared) — the same posture as
  Matrix's verified devices. A SIM-swapped or re-registered number can't
  drive turns. **pa** enforces this (`TRUSTED_VERIFIED`, checked on every
  message); signal-cli runs with its default on-first-use key trust.
  *Changed 2026-10-12 (final review):* this spec first ran signal-cli with
  `--trust-new-identities never`, but a key signal-cli doesn't trust can't
  even decrypt the first message, so the "please verify" reply could never
  be delivered and setup couldn't work. With on-first-use trust the first
  message arrives as `TRUSTED_UNVERIFIED`, gets the verify reply, and the
  security check stays in pa.
- **`signal.send`:** only to numbers in `send_to` whose key the operator
  verified (`TRUSTED_VERIFIED`, checked by the tool), `Trusted` only,
  approval on every call (unless `[access] confirm_destructive = false`).
- **Architecture:** a new crate `crates/aivyx-signal` holding the channel and
  the tool, shaped like `aivyx-proton`.

## signal-cli

The operator registers the assistant's number with signal-cli (`register`,
`verify`, a captcha if Signal asks) — pa doesn't — and runs:

```
signal-cli -a +ASSISTANT \
  daemon --socket <instance>/tool-processes/signal/signal-cli.sock \
  --receive-mode manual --ignore-attachments
```

- The socket lives in the instance's **signal tool folder**
  (`InstancePaths::tool_process_dir("signal")`), so the sandboxed tool process
  can reach it under the bundled bubblewrap preset (which binds only that
  folder), exactly like Gmail's and Proton's credentials.
- `--receive-mode manual`: signal-cli fetches messages only for a client that
  calls `subscribeReceive`. The channel subscribes; the tool never does. So
  nothing is received (and lost) while the channel isn't running — messages
  wait on Signal's server.
- No `--trust-new-identities` flag: signal-cli's default trusts a new key on
  first sight (TOFU, `TRUSTED_UNVERIFIED`). pa answers, and `signal.send`
  sends to, only a key the operator verified; a changed key is
  `UNTRUSTED` (isn't verified), so it's refused until verified again.
- The guide gives a systemd user unit for it. signal-cli needs Java 25 and
  must be updated at least every three months (Signal's server drops older
  clients).

## Architecture

`crates/aivyx-signal`:

- `rpc.rs` — a JSON-RPC 2.0 client over the Unix socket (line-delimited
  requests, responses matched by `id`, `receive` notifications delivered in
  order), reconnecting with backoff.
- `transport.rs` — a `SignalTransport` trait (next inbound event, send a
  message, send typing/read receipt, the trust level and safety number of a
  sender, `trust` with a verified safety number, the account the daemon
  serves) with the real implementation over `rpc.rs` and a scripted double
  for tests. No JSON crosses this boundary.
- `envelope.rs` — parsing signal-cli `receive` envelopes into a small
  `Inbound` type (1:1 text message / other), from recorded samples.
- `policy.rs` — pure: which events become turns (Trust), what gets the
  one-time "please verify" reply.
- `signal_channel.rs` + `session.rs` — the `ChannelContext` (reply sent once
  complete) and the in-process receive loop; one conversation per sender.
- Daemon mode — `FrontendType::Signal` in `aivyx-ipc` and
  `aivyx-channel/src/signal_daemon_frontend.rs`, using the shared `/team`
  chain (`channel_team_chain`); the policy runs in the frontend, so only
  approved messages reach the daemon.
- `tools/send.rs` + the `aivyx-signal` tool-process binary — `signal.send`,
  wired via `[[tool_process]] name = "signal"`, sandboxed like Gmail.
- `lock.rs` — one `--channel signal` per instance (`InstancePaths::signal_dir()`
  = `<data dir>/signal`), the same mechanism as Proton's.

Every aivyx-pa path is built through `aivyx-instance`.

## Trust

A Signal event becomes a turn — at `SemiTrusted`, like Matrix — only when
**all** hold:

1. it is a 1:1 `dataMessage` with text. Group messages, sync messages,
   typing and receipt events, reactions and empty messages are ignored
   silently;
2. its sender (`sourceNumber`, or `sourceUuid` — the ACI — when Signal
   hides the number) is in `allowed_senders` (numbers or ACIs);
3. signal-cli's `listIdentities` for that sender reports the key as
   **`TRUSTED_VERIFIED`**. Checked on every message, uncached: signal-cli
   doesn't tell clients when a key changes, so a cache could serve a stale
   "verified".

An allowed sender whose key is unverified or changed gets **one** reply per
run — "I only answer a verified safety number. Run `aivyx-pa signal verify`
and compare it with your phone." — and nothing runs. Everyone else is
ignored silently.

`aivyx-pa signal verify` shows the safety number (from `listIdentities`)
for each allowed sender **and each `send_to` contact** — `signal.send`
also refuses to send to an unverified key; the operator compares it with their phone (Signal → the chat → View
safety number) and, on "yes", pa calls `trust` with
`--verified-safety-number`. A changed key needs verifying again. signal-cli
knows a contact's key once they've messaged the assistant.

## Message flow

- **Conversations:** one per sender (a session per number).
- **Input:** the message text, capped at 4 KiB (Signal's own limit is about
  2,000 characters). Attachments are not downloaded
  (`--ignore-attachments`); the reply says they were ignored.
- **While a turn runs:** a typing indicator; a read receipt for the incoming
  message.
- **Reply:** sent once complete, split into several messages past ~2,000
  characters. A disappearing-messages timer is the chat's setting, which
  signal-cli applies to the assistant's replies itself (checked on the
  manual checklist).
- **Commands:** `/cancel` during a turn (in-process, as on Matrix); `/team …`
  gated exactly as on Matrix (daemon mode).

## `signal.send`

| Tool | Capability | What it does |
|---|---|---|
| `signal.send` | `message.send` | Send a text message to a number in `send_to`; returns the timestamp. |

- **New scope base `message.send`**, additive like `email.*`: added to
  `KNOWN_BASES`, `CEILING_TRUSTED` only, and the withheld-integration bases
  (so the turn loop pauses for approval on every call, as for `email.send`).
  Not `channel.send`, which remote channels' ceilings already include.
- `Trusted` only, so a Signal turn itself can't use it.
- A recipient not in `send_to` is refused before anything is sent.
- Before sending, the tool looks up the recipient's key: unless it is
  `TRUSTED_VERIFIED` the call fails with "run `aivyx-pa signal verify`
  first" (no key yet: the contact must message the assistant first), and
  nothing is sent. signal-cli refusing an untrusted key at send time gets
  the same message.

## Config — `[signal]`

`account` (the assistant's number, E.164), `allowed_senders` (your numbers,
E.164), `send_to` (E.164 numbers `signal.send` may message), and the shared
`team_run_channel`, `team_trigger_rate_limit`, `team_command_allowed_senders`.
Validation fails loudly on a malformed number or an empty `allowed_senders`.
The socket path is not config: it is always the instance's
`tool-processes/signal/signal-cli.sock`.

## Setup

1. Register the assistant's number with signal-cli (documented).
2. Start the signal-cli daemon (the guide's systemd user unit).
3. Add `[signal]`, and the `[[tool_process]]` entry for `signal.send`.
4. `aivyx-pa connect signal` — checks the socket answers (`version`) and
   writes `tool-processes/signal/signal-tool.toml` with `send_to` (the
   sandboxed tool can't read `aivyx-pa.toml`); run it again after changing
   `send_to`.
5. Message the assistant (one reply asks you to verify), then `aivyx-pa
   signal verify` — verify each allowed sender's safety number — and message
   it again.
6. `aivyx-pa --channel signal`.

## Errors

- **signal-cli unreachable** (socket missing, daemon down): reconnect with
  backoff capped at 60 s, logged, never fatal — including at startup.
- **Account not registered or unregistered** (the number was taken over
  elsewhere): the channel stops with "re-register the assistant's number with
  signal-cli, then run `aivyx-pa connect signal`".
- **A reply fails to send:** logged, not retried (the turn itself is audited as usual).
- **`signal.send` fails:** the tool returns the error.
- `[budget]` and `[rate_limit]` apply as on every channel.

## Testing

- Unit tests against the scripted transport: every trust case (unknown
  sender; allowed but unverified; changed key; group message; sync message;
  verified → turn), the once-per-run verify reply, splitting, the
  disappearing timer, `/cancel` and `/team` gating, the `send_to` allowlist,
  the `message.send` scope and approval pause.
- Envelope parsing against recorded signal-cli `receive` samples (fixtures).
- A fake JSON-RPC server on a Unix socket replaying recorded notifications
  and responses, exercising the real socket client, subscription and
  reconnect. (An automated live test would need two registered Signal
  numbers.)
- A manual checklist against the operator's real assistant number: verify,
  then a turn; an unverified new phone refused once; `signal.send` to a
  `send_to` contact asks for approval; a disappearing timer is mirrored;
  stopping signal-cli mid-run and the channel recovering.

## Docs

The chat-apps guide (Signal: registration, the systemd unit, verify), the
generated config and tools references, the CLI reference (`connect signal`,
`signal verify`, `--channel signal`), TOOLS.md (`signal.send`,
`message.send`), and THREAT_MODEL.md (the verified-safety-number rule, why
signal-cli runs separately, what `send_to` limits).

## Out of scope

Groups, attachments (in and out), voice notes, linking pa as a secondary
device, and pa registering the number itself.
