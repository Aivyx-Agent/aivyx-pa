# Federation between your own machines (preview)

Federation lets your PA instances talk to each other. For example, the
assistant on your laptop can ask the assistant on your desktop to look
something up in the desktop's files, with your approval on the desktop.

This is a **preview** (amendment A19). It is for **one operator's own
machines** only: there is no relay, no discovery and no way to reach other
people's instances. It is not covered by the 1.x compatibility promise
until the two-machine checklist
([`checklists/federation-two-machines.md`](checklists/federation-two-machines.md))
passes. The design record is
[`amendments/2026-11-16-federation-transport.md`](amendments/2026-11-16-federation-transport.md).

## How it works

- Each instance has a federation id (such as `laptop`), an Ed25519 identity
  key and an X25519 transport key. Both keys are sealed under the
  instance's master key, in `federation/identity.key` and
  `federation/transport.key` next to the store.
- A **peer card** (`aivyx-peer-v1:…`) holds the id and both public keys,
  signed by the identity key. You copy each machine's card to the other
  machine. Pasting a card with `peer add` is the trust decision.
- The machines talk over TCP, encrypted with Noise `XX`. During the
  handshake each side checks that the other's key is the one on a card it
  added.
- Each request is signed, checked against replay, and limited to what you
  allowed for that peer. Every request, reply and refusal is written to the
  audit log on both sides as a `FederationCrossing` event (peer, direction,
  verb, outcome and a fixed reason; never the text).

There are three kinds of request (verbs):

| Verb | What the receiving instance does |
|---|---|
| `chat` | Answers one message with **no tools**. |
| `task` | Asks you first, then does the task with **only the scopes you allowed** for that peer. |
| `search` | Returns the learned skills that match a query: name, trigger and steps only. Runs no model turn. |

## Before you start

- PA set up on both machines (`aivyx-pa init`), each with its own store and
  passphrase.
- A network path between them. A tailnet (Tailscale or similar) is the
  easy way: each machine gets a stable private address, such as
  `100.64.0.2`, and nothing is exposed to the internet. A home LAN works
  too. PA does no NAT traversal.
- Federation runs only in the **daemon**. The listener and the federation
  tools load after the daemon unlocks the store. The in-process chat
  (`aivyx-pa --no-daemon`) does not get the federation tools;
  `aivyx-pa --headless` runs over the daemon, so it does.

The examples use two machines, `desktop` (`100.64.0.2`) and `laptop`
(`100.64.0.3`), and port `7650` on both.

## 1. Make each machine's card

On each machine, **stop the daemon first** (`aivyx-pa daemon stop`), then:

```bash
aivyx-pa federation card
```

The first time, it asks for this instance's federation id (letters,
digits, `-` and `_`), writes `[federation] id = "…"` to the config, creates
the two key files and prints the card:

```
This instance's peer card (desktop, key a7e9be75411c3bf7):

aivyx-peer-v1:…

On the other machine, run:
  aivyx-pa federation peer add desktop <this card> --address <this machine's host:port>
```

Without a terminal (over SSH with no TTY, say), set the id first by adding
this to the config file, then run `card` again:

```toml
[federation]
id = "desktop"
```

The first `card` refuses while a daemon holds the store ("stop the daemon
first"). Once the keys exist, `card` works while the daemon runs, so you
can print the card again at any time. The card holds public keys only; it
is not a secret, but anyone who has it and can reach your machine can try
to connect, so keep it between your own machines.

## 2. Add each machine as a peer of the other

`peer add` says two things at once:

- **what the other machine may ask this one for** (`--verbs`, `--scopes`,
  `--ceiling`), and
- **that this machine may send to it.** `peer add` is the grant: each
  configured peer gives the default agent `federation.relay:<id>`.

On the desktop, paste the laptop's card. This lets the laptop chat with
the desktop and give it tasks that may read files:

```bash
aivyx-pa federation peer add laptop "aivyx-peer-v1:…" \
    --address 100.64.0.3:7650 --verbs chat,task --scopes fs.read,fs.metadata
```

On the laptop, paste the desktop's card. This lets the desktop only chat
with the laptop:

```bash
aivyx-pa federation peer add desktop "aivyx-peer-v1:…" --address 100.64.0.2:7650
```

The options:

| Option | Default | Meaning |
|---|---|---|
| `--address <host:port>` | required | Where the peer's listener is. |
| `--verbs chat,task,search` | `chat` | What the peer may ask **this** instance for. |
| `--scopes <scope,...>` | none | The most a peer's task may use here, such as `fs.read` (read a file) and `fs.metadata` (list a folder). With none, every task from the peer is refused. Warning: writes that persist (`memory.write`, `skills.write`, `persona.*`) let a peer shape your later turns. `schedule.*`, `webhook.*`, `file_watch.*` and `team.*` are refused: they would let a peer start runs outside its limits. |
| `--ceiling manual\|assisted` | `manual` | How the peer's task asks you. `manual`: every action that changes something asks. `assisted`: only irreversible actions ask. Higher levels are refused. "Irreversible" at `assisted` means what this instance's own `assisted` level asks about: a granted `shell.exec`, for example, runs commands that delete or overwrite nothing without asking. If this instance doesn't ask before deletes and overwrites somewhere (an `unleashed` level or area override, or `[access] confirm_destructive = false`), every `assisted` peer runs as `manual`, and the daemon says so when it starts. |
| `--task-timeout-secs <N>` | `900` | How long a peer's task may take on this machine, approval included. At most `86400` (one day). |

`peer add` refuses a card whose signature does not verify, or whose id is
not the id you typed. Adding a peer with an id that already exists
replaces it. `aivyx-pa federation peer list` shows the peers, and
`aivyx-pa federation peer remove <id>` removes one. These commands edit
`[[federation.peers]]` in the config file; restart the daemon afterwards.

A role that declares its own `capability_scopes` does not get the grant
automatically: add `federation.relay:<id>` (or `federation.relay` for every
peer) to that role.

## 3. Turn on the listener

On each machine, add the address to listen on to `[federation]`:

```toml
[federation]
id = "desktop"
listen = "100.64.0.2:7650"
```

Bind your tailnet or LAN address, not `0.0.0.0`. A wildcard address is
refused unless you also set `listen_any = true`. Without `listen`, the
machine can still send but receives nothing.

Restart the daemon (`aivyx-pa daemon stop`, then start it again, or
restart the service). Its log shows `aivyx-pa federation: listening on
100.64.0.2:7650`. If your firewall blocks the port, allow it on the
tailnet interface only.

## 4. Check the link

On the laptop (with both daemons running):

```bash
aivyx-pa federation ping desktop
```

```
Pinging `desktop` at 100.64.0.2:7650
  connect … ok
  handshake … ok
  key matches the card … ok
  the peer knows this instance … ok
```

Ping the other way too. `ping` works while the daemon runs and writes
nothing to the audit log. If a step fails, see
[Troubleshooting](#troubleshooting).

## 5. Use it

**From a session (the usual way).** In the Studio, the terminal chat
(`aivyx-pa`) or a chat app, ask the assistant, for example "ask the
desktop which files are in its sandbox". It uses one of three tools:
`federation.chat`, `federation.task` or `federation.search`.
`federation.relay` is an irreversible, outbound capability, so each
outbound request asks you first, at every autonomy level. Headless runs
(`aivyx-pa --headless`), routines and team runs can't ask, so they park or
escalate the request instead of sending it. Only an explicit
`[access] confirm_destructive = false` lets a granted request go without
asking.

**From the command line.** `send` sends one request and prints the reply:

```bash
aivyx-pa federation send desktop chat "what can you do for me?"
aivyx-pa federation send desktop task "list the files in your sandbox"
```

`send` refuses while a daemon runs on the sending machine ("stop the
daemon or use the federation tools from a session"), because it writes its
audit entry to the store the daemon holds. With a daemon running, use the
tools from a session instead.

### Approving a task

Every incoming task needs your approval on the receiving machine first. It
appears as a **waiting** step:

```bash
aivyx-pa review
```

```
Waiting for your review:
  17fbf332  Run a task from peer laptop: list the files in your sandbox
        from peer laptop, … — peer 'laptop' requested a 'task' that would act on your instance — your approval is required before it runs
```

Approve it with `aivyx-pa review approve <id>` (or in the Studio's review
queue), or refuse it with `aivyx-pa review deny <id>`. After that, the
task's own actions ask as the peer's `--ceiling` says, as further waiting
steps. A read-only action, such as listing a folder, does not ask again.

The peer waits while the step waits, up to its task timeout. A daemon
restart lapses every waiting step: the peer's request ends with an error,
nothing runs, and the peer can ask again.

## What is and isn't shared

A peer's turn starts with a fixed system prompt on a fresh session. It
gets **no** memory recall, **no** persona or profile and **no** skill
triggers, and it never sees your conversations. What a peer can learn is
what the tools you allowed for it return; for `search`, the names,
triggers and steps of matching learned skills; and, because its model is
offered this instance's tools, their names and descriptions, including
the ids of your configured peers in the `federation.*` tools.

The reply that comes back is untrusted text from another instance. The
sending assistant treats it as data: it is scanned for prompt-injection
phrases and fenced before the model sees it.

## Limits

| Limit | Value |
|---|---|
| One message (frame) | 1 MiB |
| Handshake | 10 s |
| Time for a connected peer to send its request | 30 s |
| Connections served at once | 16 in total, 4 per peer |
| Tasks per peer | one at a time; a second gets `busy` |
| Task timeout | `task_timeout_secs`, default 900 s, approval included |
| Wait for a chat or search reply | 120 s |
| Wait for a task reply | the peer's task timeout + 60 s |
| Peer task ceiling | `manual` or `assisted` only |

## Troubleshooting

### By `ping` step

| Step that fails | What it means | What to do |
|---|---|---|
| `connect` | Nothing answered at the address. | Is the peer's daemon running with `[federation] listen` set? Is the address right? Is the port open on the tailnet? |
| `handshake` | Something answered, but not a PA federation listener, or it took longer than 10 s. | Check the port; check the network. |
| `key matches the card` | The peer's key is not the one on its card. | The peer has made new keys. Run `card` there and `peer add` it again here. |
| `the peer knows this instance` | The peer closed the link. | The peer hasn't added this machine's card, or hasn't restarted its daemon since. On the peer, check `aivyx-pa federation peer list`. |

### Refusals

A refused request prints one of these fixed reasons, and both audit logs
record it:

| Reason | Meaning |
|---|---|
| `verb not allowed for this peer` | The receiving machine's `--verbs` for you doesn't include this verb. |
| `no scopes are granted to this peer for tasks` | The receiving machine gave you no `--scopes`. |
| `refused by the operator` | The task was denied in review. |
| `timed out waiting for approval` | Nobody approved it in time. |
| `busy` | A task from you is still running there. |
| `the request ran past its time limit` | The task took longer than its timeout. |
| `the approval could not be queued` | The receiving daemon couldn't park the approval. |
| `the request signature did not verify or the request expired` | Check both machines' clocks. |
| `the request was signed under another instance id` | The sending machine's `[federation] id` changed. Re-add its card. |
| `replayed request` | A request was sent twice. |
| `a request field is too long` | The text is too long. |

A request that fails on the sending side never reaches the peer's
operator. It prints one of these, and only the sending machine's audit
log records it. For each, run `aivyx-pa federation ping <peer>` and see
[By `ping` step](#by-ping-step):

| Reason | Meaning |
|---|---|
| `the peer could not be reached` | Nothing answered at the peer's address, or the peer closed the link because it hasn't added this machine's card. |
| `the handshake with the peer failed` | Something answered, but the encrypted handshake didn't finish. |
| `the peer presented a key its card does not vouch for` | The peer has made new keys; add its new card. |
| `the peer's card does not verify` | The card stored for that peer is damaged; add it again. |
| `the request could not be signed` | This machine's identity key couldn't sign the request. |
| `the peer did not reply in time` | The link worked but no reply came within the wait in [Limits](#limits). |
| `the peer's reply could not be read` | The reply was garbled or too large. |
| `the peer replied to a different verb` | The reply didn't match the request. |
| `cancelled before the reply` | You cancelled the turn before the reply came. |
| `refused by the peer (reason not recognised)` | The peer sent a reason this version doesn't know (a newer peer, say). |

On the receiving side, a connection from a key no configured peer has is
recorded as `unknown:<fingerprint>` with `unknown transport key`.

### Other errors

- **`no PA store at …`**: run `aivyx-pa init` on that machine first.
- **`the store is in use: stop the daemon first`** from `card`: the first
  `card` creates the keys and needs the store; stop the daemon, run it,
  start the daemon again.
- **"unexpected size" when reading `transport.key` or `identity.key`**: a
  crash during the first `card` left a half-written key file. Delete
  `federation/transport.key` (or `identity.key`) next to the store and run
  `card` again. This makes a new key, so every peer must re-add the new
  card.
- **A bad card at daemon start**: the daemon refuses to start and names the
  peer. Fix or remove that `[[federation.peers]]` entry.

### Seeing what crossed

```bash
aivyx-pa audit export --event-type federation_crossing
```

Each line shows the peer, `inbound` or `outbound`, the verb, and `relayed`
or `denied` with its reason. A request cancelled after it was sent is
`denied` with `cancelled before the reply` on the sending side, while the
peer may still record it as `relayed`.
