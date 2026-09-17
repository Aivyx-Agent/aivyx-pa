# Aivyx PA Tool SDK

**v0 — subject to change without deprecation policy.** Phase 49
ships the *contract*; API stability is deferred per
[`PRODUCT.md` P11](../PRODUCT.md) until the SDK has stabilized in
real third-party use. Expect minor breaking changes; expect
integration guarantees to hold.

This document is the third-party contract for building an Aivyx PA
**tool process** — a process spawned by the daemon at startup
that registers one or more tools and answers invocation requests
during agent turns.

> **Looking for the tools that already exist?** See the
> [tool catalog (`TOOLS.md`)](TOOLS.md) — every tool, its capability scope,
> trust tier, and delivery tier. At runtime the agent can enumerate its own
> tools via the `tools.list` tool.

It is the operator-facing sibling of the channel SDK:

| Doc | Audience | What it covers |
|---|---|---|
| [`TOOL_SDK.md`](TOOL_SDK.md) (this doc) | Tool authors | What your tool process must do, and what you get for free. |
| [`CHANNEL_SDK.md`](CHANNEL_SDK.md) | Channel adapter authors | The parallel contract for frontends. |
| [`DAEMON_IPC.md`](DAEMON_IPC.md) | Protocol implementers | The framing layer (length-prefixed JSON) both SDKs share. |
| [`THREAT_MODEL.md`](THREAT_MODEL.md) | Operators | The threat model your tool inherits. |

If you're writing a tool process, read this doc first, then
drop into the Python reference at `examples/python-tool/` for a
worked example.

---

## 1. What a tool process is

A tool process is a long-running OS process the daemon spawns
at startup, communicating with it via JSON frames on its
**stdin and stdout**. The daemon spawns one process per
`[[tool_process]]` entry in `aivyx-pa.toml`; the process is killed
on daemon shutdown.

Stdin = daemon-to-tool messages.
Stdout = tool-to-daemon messages.
Stderr = free for the tool's own logging (the daemon may
forward it to its log).

The framing layer is **identical** to the channel SDK
(`docs/DAEMON_IPC.md`): 4-byte big-endian u32 length prefix,
then UTF-8 JSON payload. Authors who shipped a Phase 48 channel
adapter recognize the shape immediately.

---

## 2. Trust model — what your tool inherits

Tool processes inherit OS-level identity from the daemon —
they run as the same OS user. Per `PRODUCT.md` P12:

> Operators install third-party tools knowing they run under
> their own OS user. This is the same trust model as installing
> any other software on a personal box.

What's *different* from a generic subprocess is that:

1. **Capability scope is bound at handshake.** Your tool
   declares the scope it needs (e.g., `fs.read:/home/.../**`);
   the operator confirms or attenuates in `[[tool_process]]`;
   the daemon rejects scope requests outside the active role's
   envelope. The agent cannot call your tool with authority you
   didn't declare.

2. **Audit is automatic.** Every invocation is recorded in the
   HMAC-chained audit log as a `ToolCall` event before your
   process even sees the request. You do not write audit
   entries; you cannot bypass them.

3. **Cancellation flows through.** When the operator cancels a
   turn (`CancelTurn` frame on the channel side), the daemon
   sends a `CancelInvocation` frame on your tool's stdin for
   any in-flight `InvokeTool`. Your tool is expected to wind
   down promptly; the daemon will not wait forever, but a
   well-behaved tool returns `ToolError { code: "cancelled" }`
   within a few hundred milliseconds.

---

## 3. Lifecycle

```
┌────────────────────────────────────────────────────────────────┐
│  1. Daemon spawns the tool process                             │
│  2. Daemon writes: ToolHello { protocol_version }              │
│  3. Tool writes:  ToolRegister {                               │
│                     tool_process_name,                         │
│                     tools: [ToolDescriptor, ...]               │
│                   }                                            │
│     where each ToolDescriptor declares:                        │
│       name, description, input_schema, required_scope          │
│                                                                │
│  4. Daemon validates each declared scope is grantable under    │
│     at least one active role. Tools whose scope is rejected    │
│     are logged + excluded from the registry; the rest are      │
│     registered.                                                │
│                                                                │
│  ┌─── per invocation (any tool, any time) ──────────────┐      │
│  │ Daemon writes: InvokeTool {                          │      │
│  │   call_id, tool_name, input, turn_id                 │      │
│  │ }                                                    │      │
│  │ Tool writes (0..N): ToolEvent {                      │      │
│  │   call_id, event: <streaming progress>               │      │
│  │ }                                                    │      │
│  │ Tool writes (terminal):                              │      │
│  │   ToolResult { call_id, verified, output }           │      │
│  │   ↑ or ↓                                             │      │
│  │   ToolError { call_id, code, message }               │      │
│  │                                                      │      │
│  │ Daemon may interject: CancelInvocation { call_id }   │      │
│  │   → tool must respond with ToolError                 │      │
│  │     { code: "cancelled" } promptly                   │      │
│  └──────────────────────────────────────────────────────┘      │
│                                                                │
│  5. On daemon shutdown:                                        │
│     Daemon writes: ToolShutdown                                │
│     Tool exits gracefully; daemon kills any process that       │
│     does not exit within a short grace window.                 │
└────────────────────────────────────────────────────────────────┘
```

A few invariants:

- **`ToolHello` first, unsolicited.** Read it before writing.
- **`ToolRegister` once.** Re-`ToolRegister` is a protocol error.
- **`call_id` correlates each invocation.** Two invocations may
  be in flight simultaneously (the agent may dispatch tool
  calls in parallel — Amendment A6). Your tool must keep track
  of `call_id` to send the right `ToolResult` back.
- **Stdout writes must be framed.** Plain text written to stdout
  will be interpreted as a length-prefixed frame and probably
  panic the bridge. Use stderr for free-form logs.

---

## 4. Message envelopes

The Rust authoritative source is `crates/aivyx-tool/src/wire.rs`.
All frames are length-prefixed JSON per
[`DAEMON_IPC.md`](DAEMON_IPC.md).

### Daemon → tool

| Variant | When | Fields |
|---|---|---|
| `ToolHello` | Once, immediately after spawn | `protocol_version: String` |
| `InvokeTool` | Per invocation | `call_id: String`, `tool_name: String`, `input: serde_json::Value`, `turn_id: String` |
| `CancelInvocation` | When operator cancels a turn mid-invocation | `call_id: String` |
| `ToolShutdown` | On daemon shutdown | _empty_ |

### Tool → daemon

| Variant | When | Fields |
|---|---|---|
| `ToolRegister` | Once, after `ToolHello` | `tool_process_name: String`, `tools: Vec<ToolDescriptor>` |
| `ToolEvent` | Streaming progress (0..N per invocation) | `call_id: String`, `event: ToolEventPayload` |
| `ToolResult` | Terminal — success | `call_id: String`, `verified: Verification`, `output: serde_json::Value` |
| `ToolError` | Terminal — failure | `call_id: String`, `code: String`, `message: String` |
| `RequiresEscalation` | Terminal — needs operator approval before proceeding | `call_id: String`, `reason: String` |
| `DispatchNotification` | Unprompted, when tool needs to alert | `target: String`, `message: String`, `subject: Option<String>` |

`RequiresEscalation` mirrors `aivyx_core::ToolOutcome::RequiresEscalation` and
surfaces on the daemon side as the turn loop's real `TurnOutcome::Escalated`
— not a generic failure. `scope` is deliberately not one of the wire fields:
the daemon always overwrites it with the `required_scope` it already
checked before dispatch, so a tool process's own opinion of its scope would
be discarded anyway. Added in the 2026-09-16 security audit fix (Task 4);
before that, a tool process requesting escalation was flattened into
`ToolError { code: "requires_escalation", .. }`, which meant it reached the
daemon as a generic failure instead of a real escalation.

### `ToolDescriptor` shape

```text
{
  "name": "wordcount",
  "description": "Count words, chars, lines in a string.",
  "input_schema": { ... JSON Schema for the input ... },
  "required_scope": "tool.wordcount"
}
```

- `name` must be a non-empty identifier; unique within the tool
  process.
- `description` is shown to the LLM. Keep it action-oriented.
- `input_schema` is JSON Schema, and it is load-bearing. The
  planner validates a call's input against it before dispatch
  (Phase 101): invalid input never reaches your tool — instead
  the model is handed a structured `invalid_input` result that
  echoes this schema and is looped to repair the call. A
  precise schema (accurate `type`s, a complete `required`
  list) therefore directly improves tool-call reliability; a
  vague one lets malformed calls through to your tool's own
  validation.
- `required_scope` is the capability scope the daemon checks
  against the active role's envelope before dispatching. New
  scope bases must be declared in `aivyx-capability`'s
  `KNOWN_BASES` (or land via a future "open scope namespace"
  amendment).

### `Verification` semantics

```text
"Verified"      — your tool queried the system and confirmed the effect.
"Unverified"    — your tool returned Ok but did not check.
"NotApplicable" — verification is not meaningful (read-only query).
```

The verification kind lands in the audit chain. **Don't lie.**
The point of the enum (per DESIGN.md D3) is that tool authors
think about verification at the type level.

### `ToolEventPayload`

```text
{ "kind": "Status",       "status": "indexing..." }
{ "kind": "OutputChunk",  "chunk": "partial output..." }
{ "kind": "Log",          "level": "warn", "message": "..." }
```

Streaming progress for long-running tools. The agent's render
layer surfaces these to the operator. New variants may land;
treat unknown kinds as "ignore" (see § 8).

### `DispatchNotification`

```text
{
  "target": "user@example.com",
  "message": "Service health degraded: response times increased.",
  "subject": "Health Alert"
}
```

An unprompted notification dispatched by your tool process
directly to the operator (not in response to any `InvokeTool`).
Unlike the per-tool `required_scope` check described above under
`ToolDescriptor` shape — which your tool process itself declares,
and which is checked once, at the `ToolRegister` handshake —
`notify.dispatch` is **not** a scope your tool process declares or
holds anywhere. The daemon checks its own active role's capability
set for `notify.dispatch` on every `DispatchNotification` frame
(inside `dispatch()` itself, not once at connection time), and
silently drops frames if the daemon's active role lacks it,
regardless of anything your tool process's `ToolRegister` claimed.
There is no `required_scope`-style declaration mechanism for this
variant — nothing you add to your tool's own manifest changes
whether frames get through.
Because the check is generic (attached to every spawned tool
process, not special-cased to `aivyx-toolkit`), granting
`notify.dispatch` to the daemon's active role lets **any** configured
tool-process binary push notifications to any registered target —
it is not scoped per tool process.
`notify.dispatch` is
Trusted-tier-only (it's in `aivyx-capability`'s `CEILING_TRUSTED`
array, the same restriction `notify.send` carries): a SemiTrusted
role can never be granted this scope, no matter what an operator
writes in `capability_scopes`.
The `subject` field is optional; `target` and `message` are required.

---

## 5. What you get for free

The daemon does *not* trust your tool process beyond its
declared scope. Every invocation is still:

1. **Scope-checked at handshake.** Tools that declare scopes
   outside the active role's envelope are rejected at startup
   — they do not appear in the agent's registry. The
   tool process keeps running (in case the rejection was a
   typo and the operator restarts with corrected config); it
   just receives no invocations.

2. **Audited.** `ToolCall { tool_id, scope_used, input_hash,
   outcome, duration }` lands in the HMAC chain before your
   process exits the invocation. There is no path that
   bypasses audit.

3. **Cancellable.** A `CancelInvocation` frame propagates the
   turn-loop cancellation signal to your tool. Cancellation is
   cooperative — the daemon expects you to respond promptly,
   not synchronously kill in-flight work.

4. **Verified-or-not.** Your `ToolResult.verified` field lands
   directly in the `ToolOutcome::Completed { verified }` the
   turn loop receives. The audit chain records it.

5. **OS-level isolation.** Your tool runs as a separate
   process. A crash, OOM, or stuck loop in your tool does not
   take down the daemon. The daemon may surface a `ToolError {
   code: "tool_process_dead" }` to the agent and continue.

You **do not** need to:

- implement a scope check (the daemon does it at dispatch time)
- write audit entries
- track turn IDs (you receive them; you don't construct them)
- maintain capability sets
- worry about input schema validation (the daemon does it
  before your tool sees the input)

---

## 6. Capability scope declaration and operator override

The tool declares `required_scope` per tool in `ToolRegister`. This
is **self-asserted** — a substituted binary at the configured
`command` path can declare whatever scope it likes. The operator's
`aivyx-pa.toml` has two independent, per-tool-name mechanisms to
push back on that declaration:

```toml
[[tool_process]]
name = "wordcount"
command = "python3"
args = ["/path/to/examples/python-tool/tool.py"]

# Optional per-tool scope overrides.
# Operator can only narrow, never widen — the override *becomes* the
# effective scope, but only when it is itself covered by (is_granted_by)
# what the tool actually declared.
[tool_process.scope_overrides]
wordcount = "tool.wordcount:read-only"  # tighter than the tool's declared "tool.wordcount"

# Optional per-tool expected-scope ceiling (Task 15,
# security-audit-fixes 2026-09-16). Unlike scope_overrides, this never
# replaces the effective scope — it only validates: the declared scope
# must be covered by (is_granted_by) the value here, or registration
# is refused. This is the mechanism to use for a tool with no
# scope_overrides entry, which would otherwise have its self-declared
# scope trusted verbatim with no operator-side check at all.
[tool_process.expected_scopes]
wordcount = "tool.wordcount"
```

**Narrowing rules (`scope_overrides`):**
- Operator overrides must be `is_granted_by(declared)` — i.e.,
  strictly attenuated. An override that isn't covered by what the
  tool declared is a configuration error, and registration for that
  tool is refused (not silently widened to the override).
- The daemon checks the *override-or-declared* scope against the
  active role's envelope.
- Tools whose effective scope is not granted are silently
  excluded from the agent's tool registry. The daemon logs the
  rejection.

**Ceiling rule (`expected_scopes`):**
- When set for a tool name, the tool's declared scope must be
  `is_granted_by` the configured value (declared ⊆ expected), or
  registration for that tool is refused with a logged reason. This
  never changes what scope is granted — it is a pure validation gate,
  composable with `scope_overrides` for the same tool name (the
  ceiling is checked against the raw declared scope, then narrowing
  is applied as usual).
- **`expected_scopes` is also a tool-*name* allowlist once any entry
  exists for a `[[tool_process]]` entry.** A non-empty map means
  every tool name you want that process to register — including
  names you've *also* configured in `scope_overrides` — must appear
  as a key in `expected_scopes`, or that tool is refused outright.
  This is deliberate: without it, a substituted binary could defeat a
  carefully configured `expected_scopes` entry by simply registering
  its tool under a name the operator never anticipated. A tool
  process with an *empty* `expected_scopes` map (the default,
  unconfigured case) is unaffected — this rule only activates once
  the operator opts in for at least one tool name from that process.

Together these are the integration guarantee that makes
operator-side scope confinement meaningful — a malicious tool
declaring overbroad scopes cannot trick the operator into granting
them, because the operator's config is the floor, *provided the
operator has configured `scope_overrides` and/or `expected_scopes`
for that tool name*. A tool name with neither configured still has
its self-declared scope trusted verbatim (the daemon logs a startup
warning naming the tool and its self-declared scope when this
happens, so it's visible even though it isn't blocked) — operators
who want the guarantee for a given tool process must configure one
of the two for every tool name that process registers. Once
`expected_scopes` is used for *any* name in a process, remember it
becomes an allowlist for *all* names in that process — see the
allowlist bullet above.

---

## 7. Integration guarantees (committed) vs API surface (v0)

Phase 49 commits to these properties — they hold across phase
boundaries:

| Property | Committed |
|---|---|
| Tool processes run as the operator's OS user | ✓ |
| Capability scope checked at handshake against the role envelope | ✓ |
| Capability scope checked at every dispatch | ✓ |
| Every `ToolCall` recorded in the HMAC-chained audit log | ✓ |
| `CancelInvocation` propagated for the active turn's cancellation | ✓ |
| Tool process killed on daemon shutdown | ✓ |
| One-tool-process-per-config-entry, spawn-once | ✓ |
| `Verification` semantics surfaced verbatim in audit | ✓ |

These properties are **stable** in the sense that a tool
written against them today will continue to receive them in
future phases. If a property weakens, that's an amendment.

The following are **not** stable:

| Surface | Why not |
|---|---|
| `ToolEventPayload` variants | New streaming variants may land. |
| `ToolDescriptor` fields | May gain `#[serde(default)]` fields. |
| `ToolError.code` values | New codes may land. Treat unknown codes as "failed for some reason." |
| Per-tool `scope_overrides` config schema | May gain richer attenuation expressions. |
| Wire schema for new daemon-to-tool variants | New variants may land; treat unknown ones as "ignore + continue." |

---

## 8. Common pitfalls

- **Plain text on stdout.** Anything not framed is a protocol
  error. Use stderr for logs.

- **Forgetting `call_id`.** Two invocations can be in flight at
  once. Your `ToolResult` must echo the `call_id` of the
  request you're responding to.

- **Crashing the process on unknown variants.** New variants
  land in every phase. Build your decoder to skip unknown
  `type` / `kind` values; do not error.

- **Sending `ToolEvent` after `ToolResult`.** Once you've sent
  a terminal frame for a `call_id`, that invocation is done.
  Further frames for that `call_id` are ignored at best,
  protocol errors at worst.

- **Holding state across invocations.** Your process is long-
  lived, but the agent treats each `InvokeTool` as independent
  unless the tool's contract says otherwise. Per-process
  caches are fine; cross-turn state is your responsibility.

- **Slow shutdown.** When you receive `ToolShutdown`, exit
  cleanly within a few seconds. The daemon will SIGKILL after
  a grace window.

---

## 8.4 Starting a new Rust tool — `aivyx-pa tool init`

> *Section added at Phase 103 exit.*

The fastest path to a runnable Rust tool process is:

```sh
aivyx-pa tool init my-aivyx-tool
```

This writes a starter project at `my-aivyx-tool/` —
`Cargo.toml`, `src/main.rs` with the handshake +
invocation main loop, `README.md`, and a `tests/conformance.rs`
that round-trips a `ToolResult` through the framing this
SDK defines. The author edits the body of `handle_invocation`
(and optionally `descriptor()`) and has a buildable starting
point; the protocol scaffolding is done.

The generated `Cargo.toml` depends on the `aivyx-tool` crate
this document defines — the wire types and length-prefixed
framing are imported, not re-implemented. The crate is not
yet on crates.io (the Distribution milestone is in progress),
so the generated dep uses a `path` placeholder the operator
fills in once.

`aivyx-pa tool init` complements the existing scaffolds:

- `aivyx-pa init` scaffolds an operator config (Phase 44).
- `aivyx-pa init --template <name>` scaffolds a named profile
  (Phase 66).
- `aivyx-pa tool init <path>` scaffolds a third-party tool
  project (Phase 103).

For non-Rust tool authors, `examples/python-tool/` remains
the canonical stdlib-Python reference.

## 8.5 First-party tools speak this protocol too

> *Section added at Phase 50 exit.*

The Phase 49 foundation shipped the third-party path; Phase 50
closed the symmetry by proving that any in-tree `Tool` impl can
be served out-of-process **without any rewriting**.

The proof lives in [`aivyx-tool::run_tool_as_subprocess`](../crates/aivyx-tool/src/harness.rs):

```rust
pub async fn run_tool_as_subprocess<T: Tool + 'static>(
    tool: T,
    tool_process_name: impl Into<String>,
) -> Result<(), HarnessError>;
```

Pass any `aivyx_core::Tool` impl — including the ten P10
substrate tools (`fs.read`, `fs.write`, `fs.delete`,
`fs.metadata`, `memory.*`, `shell.exec`, `web.fetch`,
`web.post`) — and you get a process binary that
speaks the wire protocol byte-for-byte equivalently to a
hand-written tool process.

The synthesized child-side `ToolContext`:

| Field | What the harness provides |
|---|---|
| `channel` | A small `ChannelContext` impl that captures `StreamEvent::{Status, ToolOutput}` and relays them as `ToolEvent` frames on stdout. The parent's `ToolProxy` translates them back into channel events. |
| `audit` | `NullAuditHook`. The parent records `AuditEvent::ToolCall` when it sees the terminal `ToolResult`. The child has no audit chain; double-recording would break the one-row-per-call audit invariant. |
| `cancellation` | A per-call `CancellationToken`. `CancelInvocation { call_id }` fires it. |
| `agent_id` / `session_id` / `turn_id` | Fresh per-call IDs. The parent's `turn_id` is the audit-bearing one; the child's is internal. |

The canonical proof of equivalence is
[`tests/p12_equivalence.rs`](../crates/aivyx-tool/tests/p12_equivalence.rs):
the same `FsReadTool` invocation through in-process
`execute(...)` and through the harness-wrapped subprocess
produces byte-identical `ToolOutcome::Completed { output,
verified }`.

**Implication for first-party operators.** First-party tools
continue to run in-process by default — the latency cost of the
stdio hop is unnecessary when the daemon owns the tool's
implementation. The harness exists to make the **option**
available without restructuring: any substrate tool can be
extracted into a separate process for fault isolation, sandbox
hardening, or to test the protocol equivalence end-to-end.

## 9. Sandboxing tool processes

> *Section added at Phase 52. Generic-wrapper design — Aivyx PA
> supplies the policy slot; the operator supplies the policy.*

Phase 49 ships **process isolation** for third-party tools:
each `[[tool_process]]` runs as its own OS process. Phase 52
adds an optional **wrapper layer** on top — the daemon spawns
your sandbox tool first, which then `exec`s the real command.

### Config shape

```toml
[[tool_process]]
name = "wordcount"
command = "python3"
args = ["/path/to/tool.py"]

[tool_process.sandbox]
wrapper = "bwrap"
args = [
  "--ro-bind", "/", "/",
  "--proc", "/proc",
  "--dev", "/dev",
  "--tmpfs", "/tmp",
  "--unshare-all",
  "--die-with-parent",
  "--",
]
```

The effective spawn becomes:

```
bwrap --ro-bind / / --proc /proc --dev /dev --tmpfs /tmp \
      --unshare-all --die-with-parent -- python3 /path/to/tool.py
```

The trailing `--` separator between wrapper args and command is
the wrapper's convention, not Aivyx PA's — it lives in
`tool_process.sandbox.args`. Aivyx PA makes no assumptions about
wrapper-arg shape; whatever you put in `args` goes verbatim
before the wrapped command.

### What the wrapper must do

1. **Pass stdin/stdout/stderr through unchanged.** The tool IPC
   protocol uses stdio; the wrapper must not buffer, transform,
   or close them. `bwrap`, `firejail`, and `docker run -i`
   default to this.
2. **Forward signals** so the daemon's `kill_on_drop` can clean
   up the whole chain. Most sandbox tools do this by default;
   `docker run --init` may need extra wiring for proper PID-1
   semantics.
3. **`exec` rather than fork-and-supervise.** A wrapper that
   `fork`s and waits will break the parent-process-tracking the
   bridge uses. `bwrap` and `firejail` `exec` by default;
   `docker run` is the exception (runs as a child of the
   daemon).

### Worked examples

#### Bubblewrap (Linux, native, no daemon)

```toml
[tool_process.sandbox]
wrapper = "bwrap"
args = [
  # Read-only view of the host filesystem.
  "--ro-bind", "/", "/",
  # Standard pseudo-filesystems.
  "--proc", "/proc",
  "--dev", "/dev",
  # Writable tmpfs at /tmp — tool can buffer here.
  "--tmpfs", "/tmp",
  # No network namespace, no IPC, no user namespace pass-through.
  "--unshare-all",
  # If the daemon dies, take the tool with it.
  "--die-with-parent",
  # End of wrapper args.
  "--",
]
```

What it isolates: filesystem writes outside `/tmp`, network
access, IPC visibility to other host processes, signals from
unrelated processes.

What it does **not** isolate: anything inside the read-only
mounts that the tool can read (e.g., your `~/.ssh/`,
`~/.aws/credentials`, the redb store). If a tool needs
*confidentiality*, mount the home directory `--bind` to a
scrubbed copy.

#### Firejail (Linux, profile-driven)

```toml
[tool_process.sandbox]
wrapper = "firejail"
args = [
  "--quiet",
  "--profile=default",
  "--",
]
```

`firejail` ships profiles for common tools; the system-wide
`default` profile is a reasonable starting point. Profiles can
allowlist specific paths, deny network, enforce seccomp filters,
etc. — see `man firejail-profile`.

#### Docker (cross-platform, heavyweight)

```toml
[tool_process.sandbox]
wrapper = "docker"
args = [
  "run",
  "--rm",
  "-i",                              # keep stdin open
  "--network=none",                  # no network
  "--read-only",                     # read-only root filesystem
  "--tmpfs=/tmp",                    # writable tmpfs
  "--cap-drop=ALL",                  # drop all caps
  "--security-opt=no-new-privileges:true",
  "python:3.12-slim",                # image to run
]

# IMPORTANT: this REPLACES the command. Move the actual command
# into the image's ENTRYPOINT, or use a wrapper image that
# CMD's to your tool entrypoint.
command = "tool-entrypoint.sh"
args = ["/path/to/tool.py"]
```

What this isolates: filesystem writes, network, capabilities,
privilege escalation. Plus everything Docker isolates by default
(PID namespace, mount namespace, etc.).

What it does **not** isolate: kernel exploits (the container
shares the host kernel), bind-mounted volumes (none in this
example).

### Sandboxing affects nothing in the protocol

The sandbox layer is invisible to the IPC protocol itself.
`ToolEvent` frames, `CancelInvocation` semantics, and the
handshake all work identically through a wrapper as through a
bare spawn. The conformance suite at
`crates/aivyx-tool/tests/proxy_e2e.rs::sandbox_wrapper_passes_through_stdio_end_to_end`
proves this against POSIX `env` (no-op wrapper).

### Choosing a wrapper

| Wrapper | Linux | macOS | Windows | Notes |
|---|---|---|---|---|
| `bwrap` | ✓ | — | — | Native, no daemon, scriptable. The default choice on Linux. |
| `firejail` | ✓ | — | — | Profile-driven; sane defaults out of the box. |
| `sandbox-exec` | — | ✓ | — | macOS-native; profile-driven via `.sb` files. |
| `docker run` | ✓ | ✓ | ✓ | Cross-platform, image-based, heavyweight. |
| `podman run` | ✓ | ✓ | ✓ | Daemonless Docker drop-in. |
| `(none)` | n/a | n/a | n/a | Phase 49 default. Use when the tool is trusted (e.g., first-party). |

### When the wrapper itself fails

If `bwrap` is not on `$PATH` the daemon's startup log shows:

```
aivyx-pa: tool process "wordcount" failed to start: failed to spawn tool process `bwrap`: No such file or directory (os error 2)
```

Note that the error names `bwrap` — the **wrapper**, not the
wrapped command. This is deliberate: it tells you exactly which
binary is missing.

### Sandboxing MCP servers

> *Added at Phase 55. Closes the THREAT_MODEL.md §5.2 sandbox gap
> that the post-Phase-54 project review surfaced.*

The same wrapper layer applies to `[[mcp_server]]` entries via a
parallel `[mcp_server.sandbox]` block:

```toml
[[mcp_server]]
name = "external-thing"
command = "/usr/local/bin/external-mcp-server"
args = ["--whatever"]

[mcp_server.sandbox]
wrapper = "bwrap"
args = [
  "--ro-bind", "/", "/",
  "--proc", "/proc",
  "--tmpfs", "/tmp",
  "--unshare-all",
  "--die-with-parent",
  "--",
]
```

Effective spawn:

```
bwrap --ro-bind / / --proc /proc --tmpfs /tmp \
      --unshare-all --die-with-parent -- \
      /usr/local/bin/external-mcp-server --whatever
```

The wrapper contract (stdio passthrough, signal forwarding,
`exec`-not-fork) is identical to the `[tool_process.sandbox]`
case described above. Same caveats apply — bind-mounted volumes
remain readable; the kernel is shared with the host.

**What's different from `[[tool_process]]`:**

- **Stdio-only.** `[mcp_server.sandbox]` only applies to MCP
  servers with `transport = "stdio"`. Declaring a sandbox on a
  `transport = "sse"` entry is an operator config error and
  fails at startup with a clear message — there's no local
  child process to wrap on an SSE connection (the threat
  profile sits under THREAT_MODEL §5.4 instead).
- **MCP-specific protocol semantics.** The MCP server speaks
  JSON-RPC 2.0 over stdio. The wrapper must not buffer or
  transform stdio — `bwrap` and `firejail` default to this;
  `docker run -i` needs the `-i` flag.
- **No reflection through the Aivyx PA audit chain at the wrapper
  layer.** The daemon records every `mcp.call` as a `ToolCall`
  audit event regardless of whether the server was sandboxed.
  The sandbox layer hardens the OS-level surface; the audit
  layer is unchanged.

**Per-MCP-server `kill_on_drop` still applies.** When the
daemon shuts down, the wrapper process is killed via the same
`kill_on_drop(true)` mechanism. A well-behaved wrapper
propagates SIGTERM to the MCP server child; if it doesn't, the
fallback is SIGKILL on the wrapper, which leaves the wrapped
process orphaned and reapable by `init`. Worth knowing for
tools like Docker that may leave detached containers.

---

## 10. Where to look next

- The Python reference: `examples/python-tool/`
- The wire types: `crates/aivyx-tool/src/wire.rs`
- The bridge implementation: `crates/aivyx-tool/src/bridge.rs`
- The threat model your tool inherits:
  [`THREAT_MODEL.md`](THREAT_MODEL.md)
- The framing details: [`DAEMON_IPC.md`](DAEMON_IPC.md)
- The channel SDK (sibling contract):
  [`CHANNEL_SDK.md`](CHANNEL_SDK.md)

If you want to verify your tool process is conforming, run the
`examples/python-tool/tests/` suite against it as a template —
those tests exercise the protocol-level contract without
requiring a real daemon.
