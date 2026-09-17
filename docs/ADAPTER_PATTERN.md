# Adapter pattern — how to add a new `ChannelContext`

This document is the **future-proof checklist** for adding a new
channel adapter to Aivyx PA. It was written at Phase 9 exit with exactly
two adapters in the tree (`LocalChannel` in `aivyx-channel` and
`TelegramChannel` in `aivyx-telegram`). Phase 107 added the third
data point (`DiscordChannel` in `aivyx-discord`); Phase 108 added
the fourth (`SlackChannel` in `aivyx-slack`). All Phase 9 rules
held across four data points; the **Three-data-point update** and
**Four-data-point update** subsections at the bottom document what
the third and fourth adapters confirmed plus the one open question
each adapter either resolved or punted forward. Every claim below
points at a concrete line in one of those four adapters so a future
contributor can copy shapes rather than re-derive them.

**There are two ways to adapt a channel:**

| Where you live | Use this | Audience |
|---|---|---|
| **In-tree, Rust** — your adapter lives in this workspace and implements `ChannelContext` directly | This whole document — read top to bottom | First-party adapter authors |
| **Out-of-tree, any language** — your adapter is a separate process that talks to the daemon over IPC | Jump to [§ Out-of-tree adapters](#out-of-tree-adapters) at the bottom + read [`CHANNEL_SDK.md`](CHANNEL_SDK.md) | Third-party adapter authors |

The in-tree sections below focus on Rust `ChannelContext` impls.
The out-of-tree section explains where the rules differ when your
adapter speaks the daemon's IPC protocol from another process.

Status: **confirmed at four data points** (was tentative at two —
Phase 9; promoted to confirmed-at-three at Phase 107). Phase 108's
`aivyx-slack` adapter is the fourth data point. Every Phase 9 rule
survived the four-adapter sanity check. The Discord and Slack
adapters between them resolved one Phase 9 open question (the
sibling `run_*_session` extraction is **answered "no extraction"**
at four data points) and **punted another** (the Phase 9 Q7
richer-than-`Option<String>` partition return type — Slack fit
cleanly as a colon-joined `(team_id, channel_id)` string, so the
question still waits for a Matrix-shaped adapter where
`room_id + homeserver` actually forces it). See the
**Four-data-point update** subsection at the bottom for the
detail. The Phase 6 Q5 convention ("honesty over streak
preservation") still applies: if a future fifth adapter breaks a
rule below, the right move is to update this document in the same
commit, not to work around the rule.

## The trait surface

`ChannelContext` lives in `crates/aivyx-core/src/lib.rs:161-199`. It
has eight methods; the first seven are the D2 contract and the eighth
(`session_partition`) was added in Phase 8 Task 2 as a non-breaking
default:

| Method                 | Purpose                                          |
|------------------------|--------------------------------------------------|
| `channel_name()`       | Human-readable tag for audit / logs.             |
| `platform()`           | Enum the audit chain records per turn.           |
| `trust_tier()`         | Drives the capability ceiling (see below).       |
| `session_id()`         | Stable UUID per channel instance.                |
| `stream_event(event)`  | Tokens / status / tool markers during a turn.    |
| `finalize(outcome)`    | End-of-turn commit point.                        |
| `cancellation_token()` | Token the turn loop checks between LLM steps.    |
| `session_partition()`  | Optional per-instance memory partition.          |

The trait is `Send + Sync`. Implementations must hold any interior
mutation (buffers, token slots) behind a `Mutex` so the async
`&self`-taking methods can still mutate. **Never hold a `std::sync::
Mutex` across an `.await`.** Both existing adapters take this rule
seriously — the `TelegramChannel::finalize` path at
`crates/aivyx-telegram/src/telegram_channel.rs:243-277` drains the
buffer under the lock and explicitly drops the guard *before* the
network call, with a comment calling out that tokio doesn't understand
std mutexes.

## The tier-ceiling contract

A channel's `trust_tier()` is the entire capability story. When the
turn loop runs, it fetches the tier and intersects the agent's raw
capability set against `tier.default_ceiling()`:

```text
crates/aivyx-core/src/agent.rs:120
    let tier = channel.trust_tier();
    let effective = self.capabilities.intersect(tier.default_ceiling());
```

Whatever scopes the agent had on paper, the *effective* set for this
turn is whatever survives the intersection. A `Trusted`-tier channel
sees close to the full set; a `SemiTrusted` channel sees a narrower
slice; an `Untrusted` channel sees the smallest rung.

**Tier selection guidance.** D4 locks the four rungs (Kernel / Trusted
/ SemiTrusted / Untrusted); adapters do not get to invent a fifth. The
heuristic the two existing adapters use:

- **`Trusted`** — local user, direct process access. `LocalChannel` at
  `crates/aivyx-channel/src/local.rs:126-131` picks this because a
  CLI REPL runs as the user, in the user's shell, against the user's
  filesystem. Desktop GUI apps would sit here too.
- **`SemiTrusted`** — authenticated remote human *on an allowlisted
  chat/channel*. `TelegramChannel::trust_tier()`
  (`crates/aivyx-telegram/src/telegram_channel.rs`) picks this: the
  user is authenticated (Telegram's own login), but the channel
  crosses the network and runs under the bot token's identity rather
  than the user's machine identity. Matrix, Signal, Discord, Slack
  DMs all live here. **Security-audit fix (Task 10, 2026-09-16):**
  this is conditional, not automatic — `SemiTrusted` requires the
  chat/channel to match the operator's configured allowlist
  (`chat_filter`/`channel_filter`, see `docs/INSTALL.md`); an
  unallowlisted chat/channel on the same platform is `Untrusted`
  instead. All three shipped adapters (`aivyx-telegram`,
  `aivyx-discord`, `aivyx-slack`) implement this the same way — see
  their `trust_tier()` for the exact match logic before copying the
  pattern into a new adapter.
- **`Untrusted`** — anonymous or drive-by traffic. An unauthenticated
  HTTP POST endpoint, a public chatroom with no membership gating, a
  webhook from an external service. No adapter currently ships at
  this tier.
- **`Kernel`** is reserved for the turn loop's own bookkeeping; do
  not implement a channel at this rung.

If you find yourself wanting a fifth rung, stop and re-read the
heuristic — usually the answer is "your channel is `SemiTrusted` but
the *tool* you're worried about should attenuate its own scope," not
"the tier ladder is wrong." If the answer is genuinely "the tier
ladder is wrong," that's a D4 amendment, not a workaround — see
`docs/README.md` for the amendment process.

## The sibling `run_*_session` pattern

Aivyx PA has two session drivers. They are **deliberately not a shared
function**:

- `crates/aivyx-channel/src/session.rs:150` — `run_session` drives
  the line-buffered stdin REPL.
- `crates/aivyx-telegram/src/session.rs:346` — `run_telegram_session`
  (and its transport-generic inner at `:382`,
  `run_telegram_session_with_transport`) drives a long-poll cursor
  loop.

The middle of both functions is identical copy:

```text
    let registry = config.tools;
    let planner_config = LlmPlannerConfig::new(config.model)
        .with_system_prompt(config.system_prompt)
        .with_max_tokens(config.max_tokens);
    let agent = ConcreteAgent::new(
        AgentId::new(),
        config.capabilities,
        registry,
        audit,
        move || Box::new(LlmPlanner::new(...)),
    );
```

Phase 8 considered extracting this into a `run_any_session<C:
ChannelContext>(channel, ...)` helper and **rejected the extraction**.
The reason: the outer loops are fundamentally shaped differently.
`run_session` reads one line from stdin, rotates the channel's
cancellation token, drives one turn, loops. `run_telegram_session`
holds a long-poll cursor, batches inbound updates from N chats, has
its own shutdown token separate from the per-turn one, and (Phase 9
Task 1) interleaves a `/cancel` scan with turn execution via
`tokio::select!`. A shared helper would have to parameterize over
"how do you get the next message" and "what do you do between turns"
and "what's your shutdown story," and the result was larger than the
duplicated ~50 lines it would have replaced.

**The rule:** break the sibling pattern *only* if a concrete fourth
adapter forces it. Two data points rejected the extraction; three
data points either confirm "this was never going to be shared" (keep
the sibling pattern permanently) or force the extraction (at which
point the third adapter is load-bearing evidence for why). Do not
extract on the two-data-point evidence already in tree — the rejection
is documented in `docs/PHASE_8.md` Task 1 and this document will be
updated when the extraction case actually lands.

### What to copy vs what to write fresh

If you're adding adapter #3, the copy-vs-fresh boundary is:

- **Copy verbatim** (~50 lines): the planner factory closure, the
  `ConcreteAgent::new` block, the `LlmPlannerConfig` construction.
  These are identical in both existing adapters and should stay
  identical in yours.
- **Write fresh**: the outer loop (how you get the next user
  message), the shutdown story (distinct from per-turn cancellation,
  if your adapter has one), and the finalize-side rendering (how
  `stream_event` buffers or flushes).

## The private `XxxTransport` trait seam

`TelegramChannel` is generic over a private trait:

```text
crates/aivyx-telegram/src/transport.rs:72
    pub(crate) trait TelegramTransport: Send + Sync {
        async fn get_updates(&self, offset: i64, timeout_secs: u32)
            -> Result<Vec<IncomingMessage>, TransportError>;
        async fn send_message(&self, msg: OutgoingMessage)
            -> Result<(), TransportError>;
    }
```

Exactly two methods. Two concrete impls: `ReqwestTransport` at
`crates/aivyx-telegram/src/transport.rs:111` wraps
`frankenstein::client_reqwest::Bot` for production; `ScriptedTransport`
in the crate's `tests` module is a deterministic double that captures
outgoing messages and replays scripted inbound batches.

**Why a private 2-method trait instead of generic'ing over the SDK's
own trait.** `frankenstein` exposes a 90-method `AsyncTelegramApi`
trait that the reqwest client implements. `TelegramChannel` could
have been generic over that and skipped the wrapper. It deliberately
doesn't:

1. **Surface narrowing.** The channel consumes exactly two Bot API
   methods. Pinning the surface to those two means a `frankenstein`
   upgrade that adds a method cannot accidentally break the test
   double, and the test double is ~40 lines instead of ~400.
2. **Error-shape collapse.** The SDK returns a rich error enum. The
   seam collapses everything to a single `TransportError::Platform
   (String)` so the channel's error handling is uniform.
3. **Dependency hygiene.** The private trait means the SDK type
   `frankenstein::Bot` never appears in a `TelegramChannel` method
   signature, so `aivyx-telegram`'s public surface is free of
   frankenstein references. Reverse-fan-in is minimized.

**For adapter #3:** create a private `pub(crate) trait XxxTransport`
in your crate with exactly the methods you actually call. Production
impl wraps whatever SDK you're using. Tests impl is a scripted
double living in the tests module or a `src/tests.rs` file. If your
adapter is SDK-free (e.g., raw `reqwest` against a REST endpoint),
the transport trait is still worth it — your scripted double captures
the request/response pairs without standing up a mock HTTP server.

## `session_partition` and the multi-tenant story

A single process running `aivyx-pa --channel telegram` can serve N
Telegram chats. Each chat needs its own memory namespace or chats
will see each other's `memory.read`/`memory.write` output. The
mechanism is `ChannelContext::session_partition()`, which returns
an opaque `Option<String>`:

- **`LocalChannel` inherits the default `None`** at
  `crates/aivyx-core/src/lib.rs:196-198`. One local process, one user,
  one partition. This is Phase 9 Q6's resolution (Option A — keep
  `None`): Phase 6's cross-restart recall story depends on local
  memory being shared across invocations, and the per-terminal-
  partition shape (Option B) would regress that. If a future local
  identity boundary appears, upgrade to Option C (stable per-machine
  identifier) at that time.
- **`TelegramChannel` returns `Some(chat_id.to_string())`** at
  `crates/aivyx-telegram/src/telegram_channel.rs:224-232`. Each chat
  is its own partition. The stringified `chat_id` is Telegram's
  authoritative, stable identity for a conversation.

The turn loop injects the partition into tool input JSON *before*
`required_scope` runs:

```text
crates/aivyx-core/src/agent.rs:314-330
    if let Some(partition) = channel.session_partition()
        && let Some(obj) = input.as_object_mut()
    {
        obj.insert("session".to_string(), serde_json::Value::String(partition));
    }
```

This injection is load-bearing for three reasons:

1. The tool's `required_scope` function sees the `session` field and
   derives a dual-qualifier scope like `memory.read:topic:notes:
   session:12345`. The audit chain records that full scope, so
   per-chat evidence lands in the chain alongside per-turn evidence.
2. The tool's `execute` function sees the same `session` field and
   routes to a physical topic key via `namespaced_topic()` (see next
   section). Same function sees same shape — the gate and the
   executor can't disagree.
3. The LLM never sees this field. It is not in any advertised
   `input_schema` and is added after the planner emits the tool
   call. A misbehaving LLM cannot forge a `"session"` field that
   controls which partition it reads, because the turn loop
   overwrites whatever the LLM emitted with the channel's
   authoritative partition.

**For adapter #3:** decide at channel construction time what your
partition identity is. If your adapter has one identity per instance
(like `LocalChannel`), inherit the default and return `None`. If it
has many (like `TelegramChannel`, one per chat), return
`Some(stable_id_string)`. The string is opaque to the turn loop — the
only requirement is that it's stable for the life of the channel
instance and unique across instances that should not see each other's
state.

## `namespaced_topic` stays in `aivyx-memory`

`namespaced_topic` at `crates/aivyx-memory/src/tools.rs:192` is the
helper that turns a logical topic + optional session into a physical
byte string (`\x01s\x01<session>\x01<logical>`). It uses ASCII `0x01`
as a prefix marker that's unusable in well-formed topics, so
namespaced and non-namespaced keys cannot collide.

Phase 9 Q5 asked whether this helper should move to `aivyx-capability`
so non-memory tools can partition their state too. **Resolution: it
stays where it is.** The reasoning:

- Memory is currently the only tool family whose state is
  *partitioned by session*. `fs.read` / `fs.write` use the per-process
  `fs_root` and are not per-chat scoped. Shell-exec tools (when they
  land) should not be scoped to a chat at all. LLM-provider tools
  don't carry state.
- Promoting `namespaced_topic` to `aivyx-capability` would be speculative
  generalization — no current consumer needs it there, and
  `aivyx-capability` already owns `Scope` + `TrustTier`, which are the
  cross-tool primitives. Partition namespacing is substrate-specific
  (redb flat-topic-map shape), not a capability primitive.

If a future tool family contradicts this — for example, a
per-chat-scoped `shell.exec` state-tracker — promote the helper at
that time. Until then, `aivyx_memory::tools::namespaced_topic` is
pub-visible inside the crate only, and the pattern is "if your tool
needs per-session state, write your own `namespaced_key` helper in
your own crate using the same `\x01`-prefixed layout."

## Per-task clippy policy

Phase 8 Task 8 established the convention that every task runs `cargo
clippy --workspace --all-targets -- -D warnings` before shipping, not
just at phase exit. Phase 9 Task 3 backed this with a pre-commit hook
at `scripts/pre-commit.sh`, installable via `scripts/install-hooks.sh`.

**For adapter #3:** the hook catches you for free if you've run the
installer. If you haven't, run clippy by hand before each commit. A
dirty clippy run that survives into `main` is the shape of bug
Phase 8 Task 4 shipped, Task 8 caught, and Task 3's hook now blocks
— don't reopen the gap.

## The zero-core-touch target

Both Phase 8 (Telegram adapter) and Phase 9 (config, `/cancel`,
multi-chat) have held the invariant that `crates/aivyx-core/` stays
unchanged. Phase 8 Task 2 made one exception — a non-breaking
additive `session_partition()` default method with a matching
injection site in `agent.rs` — and it was called out in the commit
message and PHASE_8.md Task 2 "Streak impact" section as a deliberate
contract extension, not a contract amendment.

**For adapter #3:** aim for zero touches to `aivyx-core`. The turn
loop, `ChannelContext` trait, and audit chain are load-bearing
primitives that every other adapter depends on. If your adapter
thinks it needs a core change, stop and check whether the change can
land as a new method with a default (like `session_partition()` did)
or as an opt-in trait extension. If it genuinely needs a breaking
change, that's a D2 amendment — file one. Do not rename or reshape
existing methods to fit your adapter's preferences.

## The checklist

When you sit down to add adapter #3, the concrete steps:

1. **Create the crate.** `crates/aivyx-<platform>/` with
   `Cargo.toml`, `src/lib.rs`, `src/<platform>_channel.rs`,
   `src/transport.rs`, `src/session.rs`, `src/tests.rs`. Four module
   files + tests is the shape `aivyx-telegram` landed on.
2. **Write the private transport trait.** Two-to-four methods
   covering only what your channel calls. Production impl wraps the
   SDK. Test impl is a scripted double with a capture buffer.
3. **Write the `ChannelContext` impl.** Pick your trust tier from
   the table above. Decide whether `session_partition()` returns
   `None` or `Some(stable_id)`. Hold any buffers / token slots behind
   `std::sync::Mutex` and never hold the lock across an `.await`.
4. **Write the session driver.** Copy the planner/agent construction
   from `run_session` or `run_telegram_session_with_transport`
   verbatim. Write your own outer loop. Rotate the channel's
   cancellation token between turns via a `reset_cancellation()`
   method on your channel.
5. **Write the scripted e2e test.** Drive the transport double
   through at least a round-trip and a cancellation case. Use the
   `aivyx-telegram` `src/tests.rs` `two_chats_persistent_e2e` test
   as a reference for multi-partition coverage. Hit the real
   persistent audit chain in the test — mock audits don't catch the
   bugs persistent audits do.
6. **Wire the binary.** Add a `ChannelKind::<Yours>` variant to the
   `aivyx-pa` binary's channel dispatch. `aivyx-config` already owns
   env-var / TOML / encrypted-store config loading, so your adapter
   plugs into the existing shape rather than inventing its own
   env-var vocabulary.
7. **Defer the real-protocol smoke test** to the Channel Activation
   Milestone (see `docs/ROADMAP.md`). Do not try to credentialize a
   real adapter at ship time; the scripted transport covers enough
   to ship, and the real-protocol pass runs once per milestone
   against the full adapter matrix.
8. **Run clippy before every commit.** Install the pre-commit hook
   if you haven't.
9. **Update this document** if any step above felt wrong, and say
   so in your adapter's ship commit. Two-data-point patterns become
   three-data-point patterns by someone explicitly writing down what
   the third data point taught us.

## Known unresolved questions

- **When does the sibling pattern break?** Unresolved at Phase 9
  exit. Two data points kept it; a third will either confirm or
  break. See PHASE_9.md Q1's Fork A vs Fork B discussion for the
  framing.
- **Does the partition type need to be richer than `Option<String>`?**
  PHASE_9.md Q7 left this for the first adapter with structured
  identity (e.g., Matrix: room_id + homeserver). Phase 9 didn't
  force the issue. If adapter #3 is Matrix, start here.
- **Does the per-chat shutdown token story scale past one
  multiplexer?** Phase 9 Task 2's multi-chat pumping uses one outer
  shutdown token + per-turn rotation per chat. A future adapter
  with a different connection model (persistent websocket, gRPC
  stream) may need a different story — revisit at that point.

---

## Out-of-tree adapters

This section was added in Phase 48. Everything above assumes you
are writing a Rust adapter that lives in this workspace and
implements `ChannelContext` directly. Out-of-tree adapters —
written in any language, living in their own repo, talking to
the daemon over the same Unix-socket IPC protocol the in-tree
adapters use — follow a different (smaller) checklist.

The contract document for out-of-tree adapters is
[`CHANNEL_SDK.md`](CHANNEL_SDK.md); the wire format is
[`DAEMON_IPC.md`](DAEMON_IPC.md); a reference implementation in
Python lives at `examples/python-channel/`.

### What stays the same

Out-of-tree adapters inherit, by construction:

- **The trust tier** — declared via the `FrontendType` field on
  `StartSession`. The daemon enforces the per-tier capability
  ceiling server-side, exactly as it does for in-tree adapters.
- **The audit chain** — every tool call, every scope check, every
  outcome lands in the HMAC-chained audit log inside the daemon's
  turn loop. Your adapter has no way to bypass this; there is no
  audit-writer API exposed across the IPC.
- **Cancellation** — `CancelTurn` propagates through a
  `CancellationToken` the tool implementations check between
  steps. Same mechanism the in-tree adapters use, surfaced as a
  one-line IPC message.
- **Role attenuation** — the `role` field on `StartSession`
  selects which role config applies, and the daemon intersects
  that role's declared scopes with the tier ceiling before the
  LLM sees anything.

You do *not* implement these; you receive them by talking to the
daemon.

### What changes

- **You do not implement `ChannelContext`.** That trait is the
  in-tree contract. The IPC protocol is the out-of-tree contract,
  and the daemon side has an in-tree `IpcChannelBridge` that
  implements `ChannelContext` *for* your remote adapter, mapping
  the wire messages into the trait surface.
- **You do not maintain interior mutexes for `stream_event`
  buffers.** Streaming is one direction over the wire — the
  daemon emits `StreamEvent` frames, your adapter renders them
  to whatever transport you own. No `&self`-mutex pattern.
- **You do not call `tier.default_ceiling()`.** The daemon does.
  You declare your `FrontendType` and the daemon picks the
  ceiling.
- **You do not write audit entries.** The daemon writes them
  before your adapter even sees the result.
- **You handle protocol-level concerns** the in-tree adapters
  don't: partial frame reads, JSON decode errors, reconnect on
  daemon restart, unknown variant graceful skip. See
  [`CHANNEL_SDK.md` §8](CHANNEL_SDK.md) for the common pitfalls.

### Trust-tier selection

Same four rungs (Kernel / Trusted / SemiTrusted / Untrusted), but
the choice is wrapped in a `FrontendType`:

| Your adapter shape | Suggested `FrontendType` | Resulting tier |
|---|---|---|
| CLI REPL on the operator's machine | `Local` | `Trusted` |
| Localhost web/IPC bridge for a browser | `Web` | `Trusted` |
| Authenticated remote messenger (Telegram-like) | `Telegram` | `SemiTrusted` |
| Anything anonymous (webhook-driven, public chat) | _(not yet — needs a new variant + a phase to add it)_ | `Untrusted` |

If your adapter doesn't cleanly fit one of the existing variants,
**don't lie about the tier** — file a phase to add the right
`FrontendType` enum value. The tier is the entire capability
story (see § The tier-ceiling contract above); a misclassified
adapter is the same as a hostile one.

### Audit / observability

The same audit chain that records in-tree adapter activity
records yours. There is no out-of-tree-specific audit surface.
What's special is that, from the audit chain's perspective, an
out-of-tree adapter looks identical to an in-tree one — the
`TurnStarted` event records the `channel: ChannelPlatform`,
which for now maps every `FrontendType::{Local, Web}` to
`ChannelPlatform::Local` and `FrontendType::Telegram` to
`ChannelPlatform::Telegram`. A future phase that wants forensic
discrimination between "I ran my own CLI" and "someone else's
Python REPL drove a turn" can extend `ChannelPlatform`.

### Where to look for examples

| Adapter | Where | Language | Notes |
|---|---|---|---|
| `LocalChannel` | `crates/aivyx-channel/src/local.rs` | Rust | In-tree, in-process, `ChannelContext` impl. |
| `TelegramChannel` | `crates/aivyx-telegram/` | Rust | In-tree, in-process, with daemon frontend wrapper. |
| Web UI | `crates/aivyx-channel/src/web_ui.rs` + `web_ui_static.html` | Rust + JS | In-tree daemon frontend; the JS side is effectively an out-of-tree adapter speaking the IPC protocol over WebSocket. |
| Python reference | `examples/python-channel/` | Python | Out-of-tree, drives the daemon IPC directly. The canonical worked example for this section. |

### Conformance — minimum bar for "this works"

A correct out-of-tree adapter can drive a complete turn against
a live daemon end-to-end under each of the following scenarios:

1. **Happy path.** Connect → read `DaemonReady` →
   (optional) `ProtocolNegotiation` → `StartSession` → receive
   `SessionStarted` → `SubmitInput` → receive `StreamEvent`
   frames → receive `TurnComplete` → `Disconnect`.
2. **Cancellation.** Send `CancelTurn` mid-turn; receive
   `TurnComplete` with cancelled outcome; reconnect-free
   continuation works.
3. **Approval gate.** Receive a `StreamEvent::ApprovalGate`
   mid-turn; respond with `ResolveGate`; turn continues or
   aborts depending on `approved`.
4. **Unknown variants are skipped gracefully** — the adapter
   handles a frame whose `type` or `kind` it doesn't recognize
   without crashing.

`examples/python-channel/tests/` exercises these scenarios; use
them as a template for your own adapter's conformance suite.

### The integration guarantees that ride with you

These hold regardless of language, library, transport-on-top,
or distribution form:

- The IPC socket is authenticated by file-mode 0600 + UID match
  via `SO_PEERCRED` (Linux). No Aivyx PA-level password.
- Every tool call is scope-checked before execution.
- Every tool call, scope check, and outcome lands in the audit
  chain synchronously.
- `CancelTurn` is honored.
- The tier ceiling is computed once per turn and is authoritative.

If any of those properties weakened between phases, you'd see an
amendment in `docs/amendments/` and an explicit migration note.

---

## Three-data-point update (Phase 107)

Phase 107 added `aivyx-discord` — the third in-tree adapter
this document was waiting on. Every Phase 9-era rule above
survived the third-adapter sanity check at full parity:

- **Trait surface** — `DiscordChannel` implements the same
  eight `ChannelContext` methods. No new variant or trait
  method needed. The Phase 8 forward-enumeration of
  `ChannelPlatform` (`Discord`, `Slack`, `Matrix`, `Email`,
  `Rest`) meant `ChannelPlatform::Discord` was already in
  `aivyx-core` — no core touch.
- **Tier-ceiling contract** — `TrustTier::SemiTrusted`,
  matching Telegram. The two `build_*_for_channel` registration-
  time gates (`shell.exec`, `fs.delete`) extended their
  `Telegram` arms to `Telegram | Discord` symmetrically.
- **Sibling `run_*_session` pattern** — `run_discord_session`
  is its own driver. The "extract shared abstraction?"
  question Phase 9 left open is **answered "no"** at three
  data points: Discord's outer loop has neither
  `get_updates`-cursor nor `scan_for_cancel`, simplifying
  the multiplexer further. Two adapters disagreed on shape;
  three adapters confirm the disagreement is real and the
  sibling pattern is the right call.
- **Private `XxxTransport` trait seam** — `DiscordTransport`
  with two methods (`next_message`, `send_message`). Same
  shape as `TelegramTransport`. Production wraps
  `twilight-gateway` + `twilight-http`; tests use
  `ScriptedTransport`.
- **`session_partition` and multi-tenant story** —
  `Some(channel_id.to_string())`. Same shape as Telegram's
  `chat_id`-string partitioning. The PHASE_9 Q7 question
  about whether the partition type needs to be richer than
  `Option<String>` is **still unresolved** — Discord's
  snowflake fit cleanly into a string, but a future adapter
  with structured identity (Matrix: `room_id` +
  `homeserver`) may force the richer type.

### One Discord-specific simplification

Discord's Gateway protocol simplifies the session-driver
shape compared to Telegram. The Phase 8/9 multi-chat shape
has three pieces:

- An outer multiplexer that polls `get_updates`.
- A `scan_for_cancel` probe that races `agent.turn` for
  in-band `/cancel` detection.
- Per-chat inner mailbox tasks.

Discord's Gateway is a **continuous push-based event
stream**, and `twilight-gateway::Shard::next_event` is the
entire "wait for input" surface. That collapses the three
pieces into two — the outer multiplexer's `next_message`
loop is itself the `scan_for_cancel` equivalent (a
`/cancel` arrives through the same channel as everything
else; the inner task's biased select against
`mailbox.recv()` is the cancel detector). The single-chat
helper from Telegram's Phase 8 era (`run_telegram_session_with_transport`)
has no Discord equivalent — there's no degenerate
"one chat" mode when one Gateway pumps multi-channel from
one shard.

**Implication:** future adapters with push-based protocols
(`twilight`-like SDKs, raw WebSocket gateways, gRPC server-
side streaming) follow the Discord two-piece shape;
future adapters with pull-based protocols (REST long-poll,
HTTP webhooks driven by external schedulers) follow the
Telegram three-piece shape. The two are not a single
abstraction — they are sibling implementations.

### What deferred to Phase 108 (Slack) for the four-data-point check

- **Discord daemon-frontend variant** (`FrontendType::Discord`
  + `discord_daemon_frontend.rs` mirroring Phase 19's
  `telegram_daemon_frontend.rs`) deferred internally
  inside Phase 107. The in-process path Task 5 landed is
  fully functional; daemon-mode-over-IPC is the
  deployment-optimization half.
- **`/approve` / `/reject` gate-resolve text-command
  parsing** lives in the daemon-frontend half (Telegram's
  `telegram_daemon_frontend.rs:219`). Folds into the
  daemon-frontend deferral above.

Phase 108 (Slack adapter, same `aivyx-telegram`-pattern
template) is the four-data-point confirmation. If the
Slack adapter shape forces any rule change above, this
document gets the four-data-point update at Phase 108
exit.

---

## Four-data-point update (Phase 108)

Phase 108 added `aivyx-slack` — the fourth in-tree adapter
this document was waiting on. Every Phase 9 rule (and every
Phase 107 update) survived the four-adapter sanity check at
foundation scope:

- **Trait surface** — `SlackChannel` implements the same
  eight `ChannelContext` methods. `ChannelPlatform::Slack`
  was already in `aivyx-core` since Phase 8's
  forward-enumeration; same surprise as Discord at Phase
  107 (no core touch needed).
- **Tier-ceiling contract** — `TrustTier::SemiTrusted`,
  matching Telegram + Discord. The two
  `build_*_for_channel` registration-time gates extended
  their arms from `Telegram | Discord` to
  `Telegram | Discord | Slack` — three remote adapters,
  one symmetric posture on destructive tools.
- **Sibling `run_*_session` pattern** — `run_slack_session`
  is its own driver. The "extract shared abstraction?"
  question Phase 9 left open is **answered "no extraction"
  at four data points.** Slack's outer loop has the same
  push-based shape Discord's does (Socket Mode's
  WebSocket is structurally identical to Discord's
  Gateway for the purposes of this trait) so the two
  protocols' implementations are very similar — but the
  protocol-specific details (`twilight-gateway` vs.
  `slack-morphism` callback shape, `u64` vs. `String` IDs,
  intents vs. scope OAuth, single channel id vs.
  `(team_id, channel_id)` pair) make any shared
  abstraction the wrong size. Four data points confirm
  the sibling pattern is permanent.
- **Private `XxxTransport` trait seam** — `SlackTransport`
  with two methods (`next_message`, `send_message`). Same
  shape as `TelegramTransport` + `DiscordTransport`.
  Production wraps `slack-morphism` Socket Mode + REST
  (with a Phase-108-internal deferral on the
  callback-state-passing wiring that's bundled with the
  Phase 107 daemon-frontend follow-on); tests use
  `ScriptedTransport`.
- **`session_partition` and multi-tenant story** — Slack's
  `format!("{team_id}:{channel_id}")` per Phase 108 Q3a.
  This is the most-structured identity any in-tree adapter
  carries, and it fit cleanly into `Option<String>`. The
  partition key is a colon-joined string that downstream
  consumers can `split_once(':')` if they ever need the
  components back. The Phase 9 Q7 question about whether
  the partition type needs to be richer than
  `Option<String>` is **still unresolved** — but now
  *deliberately* punted to the next adapter shape that
  forces it. Slack's `(team_id, channel_id)` was the most
  plausible four-data-point forcing function and it
  didn't. Matrix (`room_id` + `homeserver` + per-server
  routing details) remains the natural test case.

### The Q3a stringification pattern

Slack's `(team_id, channel_id)` partition key is the
load-bearing piece of Phase 108. The
`SlackChannel::session_partition()` method returns
`Some(format!("{team_id}:{channel_id}"))`, and the outer
multiplexer in `crates/aivyx-slack/src/session.rs` keys its
per-partition mailbox `HashMap<String, _>` on the same
string. The
`two_channels_with_same_channel_id_but_different_team_id_partition_distinctly`
test pins the property: a Slack bot installed in two
workspaces that happen to allocate the same `channel_id`
partitions cleanly into two distinct buckets.

If a fifth adapter wants structured identity with three or
more components (e.g., Matrix `(homeserver, room_id,
event_id)`), the right move is one of:

1. Continue the Slack pattern: `format!("{a}:{b}:{c}")`
   and accept that parsing back is `split(':')`-shaped.
2. Lift the partition return type to
   `Option<SessionPartition>` where `SessionPartition` is
   a typed enum or struct in `aivyx-core` — the Phase 9
   Q7 substrate change, which would deliberately break the
   `aivyx-core/src/lib.rs` streak.

Option 1 has worked across four data points; option 2 is
substrate-design work that should happen when an operator
need (or test surface) demands it, not speculatively.

### What deferred from Phase 108

Two Phase-108-internal deferrals bundle with the Phase 107
daemon-frontend follow-on:

- **Production `SlackMorphismTransport` wiring** — the
  callback-state-passing via `SlackClientEventsUserState`
  needs proper UserState-backed design. The trait + the
  scripted-double are in tree; the production transport
  is a compile-only stub that returns a clean
  "not-yet-wired" error.
- **`/approve` / `/reject` text-command gate-resolve
  routing** — same Slack-side daemon-frontend gap as the
  Phase 107 Discord deferral.

Both deferrals land alongside the Phase 107 daemon-
frontend when an operator wants live-bot smoke testing —
the Channel Activation Milestone is the natural pass for
that work.
