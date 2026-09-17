# Aivyx PA Threat Model

**Status:** Draft. **Last reviewed:** Phase 180 exit (2026-06-06)
— the bundled secure-by-default sandbox preset (§6, §4.10); prior
pass added the productivity-tool OAuth asset (§3), the autonomous
loop (§4.9), and productivity-tool egress (§4.10). **Owners:**
the operator.

This document is **operator-facing**. It states plainly what Aivyx PA
defends against, what it does not, and where each defense lives in
the code. It is a sibling of `DESIGN.md` (technical contract) and
`PRODUCT.md` (product contract), not a derivation of them — those
two documents describe *how the agent works*; this document
describes *what an operator can and cannot rely on it for*.

If a claim in this document disagrees with the code, the code is
right and the document is wrong — file an issue.

---

## 1. Scope

Aivyx PA is a **single-operator personal agent**. The threat model is
written around exactly one human, on hardware they control, talking
to LLM providers under their own API key, holding secrets they
own.

This is the same posture OpenClaw and Hermes Agent take. The model
is **not**:

- a multi-tenant SaaS,
- a shared workstation tool where two humans take turns,
- a server-side bot answering anonymous web traffic.

Operators who run Aivyx PA in a context that breaks the single-operator
assumption (e.g., a shared dev box where a second user can `read(2)`
the IPC socket) are responsible for understanding that the model no
longer applies.

## 2. The operator and their adversaries

The operator's identity is **the OS user who owns the daemon
process** (`PRODUCT.md` P6). There is no Aivyx PA-level account, no
password, no token. If you can read the daemon's IPC socket
(mode `0600`, owner = operator UID), you are by definition the
operator. Rotation of an "Aivyx PA account" is therefore not a
concept; rotation of the redb passphrase is.

The model recognizes four **adversary archetypes**, aligned with
the trust tiers in `aivyx-capability/src/lib.rs:503` (D5):

| Tier | Archetype | Example | Default authority |
|---|---|---|---|
| `Kernel` | Aivyx PA itself | the turn loop, audit writer | unconditional (internal use only) |
| `Trusted` | the operator at their own keyboard | Local CLI, Web UI on `127.0.0.1` | near-total, with extra audit on destructive ops |
| `SemiTrusted` | the operator over a remote, authenticated channel | their own Telegram bot, with chat-id allowlisted | narrowed — no unqualified shell, no `fs.delete`, qualifier required on `fs.*` and `net.post` |
| `Untrusted` | anyone the operator has not authenticated | webhook requests, unallowlisted senders | near-empty — read public memory, that's it |

A turn loop computes the **effective capability set** exactly
once per turn, before any LLM call:

```rust
let effective = self.capabilities().intersect(tier.default_ceiling());
```

This snapshot is recorded in the `TurnStarted` audit event
(`aivyx-audit/src/lib.rs:91`) and is authoritative for the
entire turn. Mid-turn capability escalation is not supported.

## 3. Assets

What an attacker would gain by compromising each.

| Asset | Where it lives | If compromised |
|---|---|---|
| Passphrase | In RAM during cold start; never on disk | Full read/write of the encrypted store. |
| Master key | `MasterKey` in daemon RAM, zeroize-on-drop (`aivyx-crypto/src/lib.rs:175`) | Same. |
| Encrypted store | `$XDG_DATA_HOME/aivyx-pa/store.redb`, chmod 0600 | Confidential without the passphrase; needs Argon2id work to brute. |
| Audit chain | redb `KeyDomain::Audit` | Reading reveals every tool call ever made. Tampering trips `AuditError::ChainBroken` on next open. |
| API keys (LLM provider, Telegram bot token) | `KeyDomain::Secrets`, AEAD-sealed under a domain subkey | Spend on operator's LLM account; impersonate operator's bot. |
| Productivity-tool OAuth tokens (Gmail, Calendar, Drive, Notion, …) | A **per-tool-process token file**, owned by the separate tool binary — *not* the daemon store | Act as the operator on that one external service. Scoped to the single tool process; a daemon-store compromise does not reach them, and vice versa. |
| Memory entries | `KeyDomain::Memory`, AEAD-sealed under a domain subkey | Reveals everything the operator told the agent across sessions. |
| Daemon IPC socket | `$XDG_RUNTIME_DIR/aivyx-pa/daemon.sock`, mode 0600 | Anything the operator can do. |
| Source code & config | `~/Projects/.../aivyx/`, `aivyx-pa.toml` | Loosen role envelopes, add malicious tools. |

Twenty encrypted domains exist today: the original nine
(Sessions, Memory, Audit, Secrets, ChannelState, Missions,
Schedules, Webhooks, FileWatches) plus the Persona /
self-learning / loop / reminder domains added since (Persona,
PersonaProposals, MemoryVectors, RecallEvents, ProactiveLog,
HelpfulnessLedger, CooccurrenceLedger, ToolRelevanceLedger,
CorrectionLedger, LoopBacklog, Reminders). Each is sealed under its own
HKDF-derived subkey so a leak of one domain's plaintext does not
compromise another — and the productivity-tool OAuth tokens sit
outside this set entirely, in their own per-tool-process files.

## 4. Threats we defend against

This section names each threat, the mitigation, and the code that
implements it. Anything not on this list is in section 5.

### 4.1 An LLM is steered into running a destructive command

**Example.** A prompt-injection payload in a web page or a malicious
upstream MCP-server tool description steers the LLM into emitting
`shell.exec("rm -rf $HOME")`.

**Mitigation chain:**

1. **Capability scope check** before execution. The tool's
   `required_scope(input)` is checked against the turn's effective
   capability set (`aivyx-core/src/agent.rs`). A `SemiTrusted`
   turn does not hold `shell.exec` at all
   (`CEILING_SEMITRUSTED`, `aivyx-capability/src/lib.rs:627`).
2. **Per-role allowlist.** Even at `Trusted`, if the active role's
   `tool_allowlist` omits `shell.exec`, the synthetic
   `tool.allowlist:<tool>` scope is denied
   (`aivyx-capability/src/lib.rs:65`).
3. **Process-group isolation.** The shell subprocess is launched in
   its own process group; SIGTERM→SIGKILL escalation on timeout
   prevents zombie grandchildren (Phase 42).
4. **Environment isolation.** `shell.exec` strips API keys and
   provider tokens from the child's environment so an LLM-generated
   command cannot exfiltrate them via `echo $ANTHROPIC_API_KEY`
   (Phase 42).
5. **OS-level process confinement.** Even a call that clears every
   gate above still runs the spawned `sh` under Landlock + seccomp-bpf
   confinement (`aivyx-confine`, on by default). `shell.exec`'s write
   grant is scoped to `cwd_root` plus a fixed system/toolchain list —
   `rm -rf $HOME` targets almost entirely outside that grant, so the
   kernel denies the deletions regardless of what the capability/
   allowlist layers above did or didn't catch. See §5.6 / property 7
   in §6 for the mechanism and its current scope.
6. **Audit trail.** The denied call (or, if it ran, the call +
   output hash) lands in the HMAC-chained audit log
   synchronously. There is no path that runs a tool without
   appending an audit row first.

### 4.2 The on-disk store is read by a process that is not the daemon

**Mitigation:** ChaCha20-Poly1305 AEAD under a subkey derived from
the operator's passphrase via Argon2id (m=64 MiB, t=3, p=4) and
HKDF-SHA256 with a versioned salt (`aivyx-crypto/src/lib.rs:58`).
The store file is `chmod 0600` on every cold open
(`aivyx-storage/src/lib.rs:471`).

**Caveats.** Argon2id parameters are tunable. The salt is versioned
(`aivyx-v1-storage`) so a future key-schedule migration can produce
entirely different subkeys from the same master.

### 4.3 The audit log is tampered with on disk

**Mitigation:** Every `SignedEntry` carries an HMAC-SHA256 tag over
`prev_mac || canonical_bytes(event)` (`aivyx-audit/src/lib.rs:164`).
The genesis seed is `b"aivyx-audit-v1-genesis"`. Canonicalization
is JCS (RFC 8785) via `serde_jcs`. The HMAC key is itself an HKDF
subkey under `KeyDomain::Audit`. Tampering with any byte of any
entry breaks the chain at the first modified row and trips
`AuditError::ChainBroken` on the next open.

Deleting rows from the *middle* of the chain is likewise caught —
the reopen scan re-derives each row's expected `seq` from its scan
position, so a gap surfaces as `AuditError::CorruptStoredEntry` or
`ChainBroken`. A **tail** truncation (deleting only the most recent
N rows, leaving the remaining rows a shorter but internally
self-consistent chain) needed a separate mechanism, since "the log
never grew past here" and "the log's tail was deleted" are
byte-identical on disk from a pure scan-position replay. This is
closed by a small persisted chain anchor — the last known `seq` +
`mac`, written only after that entry is durably persisted, and
checked against the real on-disk tail on every open/verify — that
trips a distinct `AuditError::TailTruncated`
(`aivyx-audit/src/persistent.rs`, Task 11 of the 2026-09-16 security
audit).

`aivyx-pa --verify-only` cold-verifies the full chain without an LLM
API key, so audit verification works on a machine that has never
been online.

### 4.4 A process on the same machine tries to talk to the daemon

**Mitigation:** The daemon's IPC socket is a Unix domain socket
under `$XDG_RUNTIME_DIR` with mode `0600`, owner = operator UID
(`aivyx-channel/src/daemon_server.rs:177`). There is no network
surface. There is no token exchange. If you can `read(2)` the
socket, you are the operator by OS-level identity (`PRODUCT.md` P6).

**Caveats.** This assumes the runtime directory is also private to
the operator (true under standard systemd-logind setups). On a box
where another OS user has `read` on `$XDG_RUNTIME_DIR/aivyx-pa/`, the
model breaks — but that already required compromising the
operator's user account.

### 4.5 The passphrase is captured from memory

**Mitigation, partial:** The transient passphrase buffer is wiped
via `zeroize::Zeroize` immediately after Argon2id derivation
(`aivyx-channel/src/passphrase.rs`). `MasterKey` and `SubKey` are
`ZeroizeOnDrop` (`aivyx-crypto/src/lib.rs`) and have no public API
that hands out raw bytes — callers get `seal`/`open` methods.
`Debug` is redacted.

**Caveats.** A core dump, a debugger attached to the running
daemon, or a memory-scraping rootkit will defeat this. We do not
defend against an attacker with kernel-level access to the
operator's machine.

### 4.6 A SemiTrusted channel tries to act with Trusted authority

**Example.** An attacker controls a Telegram chat that the operator
allowlisted. They ask the agent to `cat ~/.ssh/id_rsa`.

**Mitigation:** `CEILING_SEMITRUSTED` does not include `shell.exec`,
`fs.delete`, `config.write`, or unqualified `fs.read` /
`fs.write`. The turn-loop intersection (`effective = agent_caps ∩
tier_ceiling`) is computed *before* the LLM sees the message
(`aivyx-capability/src/lib.rs:516`). The agent literally cannot
emit a passing tool call for these scopes from a SemiTrusted turn.

### 4.7 A scheduled run, webhook, or file watcher executes with too much authority

**Mitigation:** Trigger-launched turns inherit the OS user's
identity and run under a configured role's envelope, not under
`Kernel`. Webhook triggers bind to `127.0.0.1` only
(`aivyx-channel/src/webhook_listener.rs`). The webhook source is
classified as a channel and given a tier — `Untrusted` by default.
Triggered missions can opt into `wrap_mission = true` so every
triggered run lands in the mission audit surface.

### 4.8 Reflection / self-modification runs without operator oversight

**Mitigation:** The reflection loop is three audited steps
(Phases 28–30):

1. `turn.history` — agent reads its own recent outcomes (audited as
   a regular tool call).
2. `reflection.propose` — agent writes a proposed change to a gate
   queue. **The change is not applied.** A `mission.gate` audit
   event names the operator-approval requirement.
3. `reflection.apply` — runs only after the operator answers the
   gate (CLI prompt, Telegram `/approve` command, Web UI button).
   The mutation lands in the audit chain.

The agent has no path to silent self-modification. Both `memory`
and runtime role overrides (`RoleOverrides`, Phase 30) flow through
the same approval gate.

### 4.9 The autonomous loop runs unbounded (the Aivyx PA Ralph loop)

**Mitigation:** The autonomous loop (Phases 173–177) is a
self-re-arming agent that works a backlog without per-iteration
operator prompting — so its containment is structural, not
interactive. Three independent caps bound every run:
`max_iterations`, `max_run_secs` (wall-clock), and
`max_run_tokens` (turn spend summed over the run window), with a
pure `decide()` termination function that always halts. Each
iteration is a fresh agent turn under the loop's configured role
envelope — it cannot exceed the capabilities that role grants,
and every tool call is audited like any other. A backlog story
is only marked complete after **driver-side gate verification**
confirms the loop's build/test gates actually ran and passed
(Phase 174), so a model that merely *claims* success cannot
advance the backlog. The backlog itself is an HMAC-chained
append-only substrate: a tampered or reordered entry trips the
chain check. The operator can stop a run at any time
(`aivyx-pa loop stop`) and inspect live spend (`aivyx-pa loop status`).

As of **Chapter Throttle**, an opt-in `[rate_limit]` adds a fourth
bound that applies *within* every turn (loop iteration, Nonagon
sub-turn, or interactive turn alike): per-turn-per-tool, per-turn-total,
and sliding-window caps on **how often a tool is called**, with an
`alert` or `deny` action. This complements the run-level caps above —
where `max_iterations`/`max_run_tokens` bound the *run*, the rate gate
bounds a single runaway turn from hammering `web.fetch` / `shell.exec`.
A throttled call is refused before execution and lands a dedicated
`RateLimited` audit record. Uncapped by default; see
[`RATE_LIMITS.md`](RATE_LIMITS.md).

### 4.10 A productivity tool exfiltrates data to an external service

**Mitigation:** The Chapter F/G productivity integrations
(Gmail, Calendar, Drive, Notion, Obsidian, n8n, the toolkit) are
the only components that egress to the public internet on the
operator's behalf, and each is a **separate sandboxed binary**
behind the tool-process IPC bridge — not code in the daemon's
address space. Each holds only its own OAuth token (in its own
per-tool-process file), is reachable only through a capability
scope the operator granted, and every invocation is audited as a
`ToolCall` with the scope used. A compromised or buggy
productivity tool can misuse the one service it is authorized
for; it cannot read the daemon's store, another tool's token, or
a scope it was never granted. As of **Phase 180**, on a new
config these binaries are additionally **OS-sandboxed by
default** (the `[sandbox] default_backend = "auto"` preset —
filesystem-isolated with only the per-tool token dir writable;
see §5.6), so even the tool's own filesystem reach is contained
to its token dir + read-only system. (The residual "a malicious
tool process abuses its own grant" case is the same class as
§5.2 / §5.6 below — out of scope by the same reasoning.)

### 4.11 A web page in the operator's browser drives the daemon (cross-origin / DNS rebinding)

**Mitigation:** The Studio web UI binds `127.0.0.1` only, but a
loopback bind is *not* by itself a boundary against the browser:
WebSocket connections are exempt from the same-origin policy, so a
malicious page the operator visits could otherwise open
`ws://127.0.0.1:7843/ws` and drive the already-unlocked daemon —
and the Studio can now **write config** (access level, budgets) and
the **filesystem** (the Documents editor), not just chat. The
daemon defends the `/ws` upgrade with an **`Origin` check**
(`aivyx-channel/src/web_ui.rs`, `ws_origin_allowed`): an upgrade is
accepted only when the `Origin` header is absent (a non-browser
client — the CLI / IPC probe / a native app, which can already
reach the Unix socket and so is inside the trust boundary) or
exactly matches one of our loopback origins on the bound port
(`http://127.0.0.1:<port>`, `http://localhost:<port>`,
`http://[::1]:<port>`). A cross-site page's real origin, a
rebinding attacker's hostname (resolving to `127.0.0.1` but
presenting its own `Origin`), a wrong-port local app, and a
sandboxed `null` origin are all rejected with `403`. This closes
the Cross-Site WebSocket Hijacking / DNS-rebinding vector against
the daemon's now-writable web surface.

## 5. Threats we explicitly do not defend against

The honest section. These are out-of-scope by design; if they
matter to your deployment, you need additional controls *outside*
Aivyx PA.

### 5.1 The operator's machine being root-compromised

If an attacker has the operator's UID or kernel access, every
in-RAM key, every plaintext memory entry, and every audit row is
theirs. The threat model assumes the OS underneath Aivyx PA is sound.

### 5.2 A malicious MCP server

**Status update (Phase 55):** the worst-case posture of this gap
has narrowed; the residual risk is operator-configurable.

Aivyx PA ships MCP support (Phases 23/24/32). MCP servers run as
**child processes of the daemon**, spawned via `aivyx-mcp`'s
stdio transport, under the operator's UID. There is **no
signed-server registry, no content-level scan of the server
binary, no Aivyx PA-curated allowlist**. The MCP ecosystem's
discovery surface (GitHub search, blog posts, Slack threads) is
the npm-style problem the Hermes threat model named explicitly.

**What is defended at the protocol layer.** Tool calls into an
MCP server are gated by capability scopes
(`mcp.call:<server>:<tool>`); an MCP tool that asks for
`shell.exec` does not silently get it. The capability check
happens server-side in the daemon, before `tools/call` is
dispatched.

**What is now operator-defended at the process layer.** Phase 55
added a `[mcp_server.sandbox]` config block parallel to Phase 52's
`[tool_process.sandbox]`. When an operator declares a wrapper
(bubblewrap, firejail, Docker, sandbox-exec), the daemon spawns
`wrapper wrapper_args... mcp-server mcp-args...` instead of the
bare MCP command. Operators on a hardened deployment can prevent
a malicious MCP server from reading `~/.ssh/id_rsa` even though
the agent never asked it to. See `docs/TOOL_SDK.md` §9 for worked
examples.

**Residual risk that remains in scope:**

- An operator who installs an MCP server *without* configuring
  `[mcp_server.sandbox]` is in the original threat shape: the
  server runs with operator OS authority. Phase 55 makes
  hardening *available*, not *automatic*.
- A misconfigured wrapper that lets the MCP server retain access
  to sensitive paths is the operator's responsibility. Aivyx PA
  doesn't validate wrapper policies.
- The SSE transport (remote MCP server over HTTP) is not
  sandbox-able because there's no local child — its threat
  profile sits under §5.4 instead.

The operator-facing advice remains unchanged: **treat each MCP
server install with the same caution as installing a CLI tool
from a stranger's tarball.** Phase 55 just gives that caution
teeth.

### 5.3 Prompt injection beyond capability gating

**Updated post-Phase-180, corrected again for Phase 197's own
follow-up work** (this section's own "last reviewed" line at the top
of the document still reads "Phase 180 exit" — out of scope for this
correction; only the two factual claims below, which this phase's own
work directly contradicts, are being fixed here). Chapter Bulwark
added real prompt-injection resistance since Phase 180: fetched,
parsed, and tool-process content is fenced as untrusted data at every
ingress (web fetches, file reads, MCP tool output, operator-provided
context files, the productivity integrations' externally-authored
content — Gmail, Calendar, Drive, Contacts, Notion, Obsidian, n8n —
and the toolkit's web search), so the model sees that content marked
as data, not as instructions it should follow. Since Chapter Picket
(Phases 194-196), this is no longer only a structural mitigation:
`aivyx-injection-guard` is a real content-level scanner for
prompt-injection *payloads*, layered on top of Bulwark's fencing — it
actively scans the same untrusted content for known injection
phrasings and escalates the turn on a match, the same category of
defense Hermes Agent's Tirith provides.

Beyond Bulwark's fencing, the remaining defense is capability gating:
an LLM that has been jail-broken into trying to exfiltrate data still
cannot emit a passing tool call for a scope its role does not hold.

This is intentional within a known limitation: capability gating
is a stronger boundary than pattern-matching scanners, but it does
not catch attacks that stay within the agent's *legitimate*
authority (e.g., a prompt that convinces the agent to read a file
it is allowed to read and post it to a URL it is allowed to post
to). Operators are responsible for not granting roles authority
they would regret if the LLM acted maliciously.

### 5.4 Network-level eavesdropping on LLM provider traffic

Aivyx PA uses `rustls` over HTTPS for every LLM provider call. We
trust the TLS stack and the operator's CA roots. If a corporate
or hostile MITM has injected a root CA into the operator's trust
store, the agent will use it.

### 5.5 The LLM provider itself being malicious

API keys go straight to Anthropic / OpenAI / Ollama. We do not
defend against an LLM provider exfiltrating prompt content,
returning poisoned tool calls, or correlating the operator's API
usage. The privacy guarantee is *not* "the cloud cannot see your
prompts" — it is *"only the provider you chose can see your
prompts, with the API key you supplied, under your account"*
(`PRODUCT.md` G6, N5).

### 5.6 Tools running in the same address space as the daemon

**Status update (Phase 49, 50, 52):** the strict reading of this
gap has narrowed substantially.

- **Phase 49** shipped PRODUCT.md P12: third-party tool
  processes now run as separate OS processes (spawned with
  `kill_on_drop`, communicating via length-prefixed JSON over
  stdio). The first-party in-process path is preserved for
  latency reasons, but the equivalence is proven by Phase 50's
  `p12_equivalence.rs` test.
- **Phase 52** added an optional command-wrapper sandbox layer
  on top of process isolation. Operators declare
  `[tool_process.sandbox] wrapper = "..." args = [...]` and the
  daemon spawns `wrapper wrapper_args... command command_args...`
  instead of the bare command. Bubblewrap, firejail, Docker,
  sandbox-exec — Aivyx PA supplies the policy slot; the operator
  supplies the policy. See `docs/TOOL_SDK.md` §9 for worked
  examples.
- **Phase 180 — secure-by-default.** A *bundled* default preset
  closes the "unsandboxed unless configured" gap for
  `[[tool_process]]`. `[sandbox] default_backend = "auto"`
  (which the `aivyx-pa init` wizard now writes into every new
  config) detects bubblewrap / firejail on `PATH` and applies a
  conservative-but-functional preset automatically: read-only
  system dirs, a private `/tmp`, an isolated PID namespace,
  `$HOME` hidden except a writable bind of the per-tool token
  dir, and network left on (it is already capability-gated at the
  IPC boundary). The in-code default with no `[sandbox]` section
  stays `none`, so existing configs are unchanged; an operator
  opts in by adding the section, opts a single tool out with
  `disable_sandbox = true`, or overrides with an explicit
  `[tool_process.sandbox]` block. **Scope:** the bundled preset
  applies to `[[tool_process]]`, not `[mcp_server]` — MCP servers
  are operator-configured external programs with unknown
  filesystem needs and keep the Phase 55 explicit-wrapper model.
  The preset's argv is unit-tested; whether it actually contains
  a given process is operator-verified (no sandbox backend in
  CI).

**What is still in scope of this section:**

- **First-party tools** still run in the daemon's address space
  by default (the eight P10 substrate tools, infrastructure
  tools like `mission.*`/`reflection.*`, MCP proxies). Buffer
  overflows in any of these would corrupt daemon memory in
  principle. The defense-in-depth picture is:
  - `#![forbid(unsafe_code)]` on `aivyx-crypto`, `aivyx-storage`,
    and `aivyx-telegram`.
  - Within this workspace's own crates, the only production
    `unsafe` is in `aivyx-core::tools::shell` — `libc::killpg` for
    process-group teardown when a shell invocation times out
    (Phase 42, narrowly scoped).
  - `aivyx-confine` (an external dependency, Linux builds only —
    see `aivyx-core`'s Cargo.toml target-gating) adds its own
    narrowly-scoped `unsafe`, compiled into the same production
    binary: a read-only raw `landlock_create_ruleset` probe syscall
    for kernel-support detection, and the `pre_exec` hook that
    applies the Landlock ruleset + seccomp-bpf filter in the forked
    child before `exec` — written allocation-free per
    async-signal-safety constraints (see that crate's own
    `confiner.rs` doc comments for the invariants each block
    upholds). This runs for every `shell.exec`/`git.rs` spawn, not
    just on timeout, so it is broader in frequency than the
    `killpg` call above, though still a fixed, audited surface
    rather than free-form `unsafe`.
  - Test-only `unsafe` blocks for `std::env::set_var` /
    `remove_var` exist in `aivyx-channel::passphrase` and
    `aivyx-config::tests`. Rust 2024 marks env-var mutation
    unsafe; these blocks never compile into production binaries.

  This is harder than in C; it does not make it impossible. If a
  first-party tool grows a real `unsafe` need beyond the
  killpg call, it should be wrapped behind a typed safe API in
  a leaf crate with `#![forbid(unsafe_code)]` elsewhere, the
  same posture `aivyx-crypto` takes.
- **Third-party tools without an operator-configured wrapper**
  run with the operator's full OS authority (file access,
  network access, etc.) within their own process. The Phase 49
  capability gate prevents the agent from calling a tool with
  authority the tool didn't declare, but the tool process
  itself runs with the operator's UID and can read whatever
  files the operator can read.

The Phase 52 sandbox layer narrows the second item — operators
who care about confinement can wrap with their tool of choice
without Aivyx PA prescribing one.

### 5.7 Side channels (timing, power, electromagnetic)

We use constant-time AEAD primitives from RustCrypto. We do not
defend against an attacker who can measure the daemon's wall-clock
behavior or power draw. This is appropriate for a personal agent;
operators in adversarial environments (red-team training labs,
nation-state targets) should not rely on Aivyx PA for this.

### 5.8 Channel-platform compromise

If Telegram is compromised, the operator's bot token leaks and an
attacker can send messages as the operator. Aivyx PA will then
classify them as `SemiTrusted` (because the chat-id allowlist
matches) and run them within the tier ceiling. The damage is
bounded by `CEILING_SEMITRUSTED`, but it is not zero. This is the
trade for using third-party messaging platforms at all.

### 5.9 Adversarial co-tenants

A second OS user on the same machine who has been granted access
to the operator's home directory, runtime directory, or store file
defeats the OS-level identity model. The fix is the OS's job:
don't share UIDs. Aivyx PA does not enforce isolation between OS
users of the same instance because — per `PRODUCT.md` P1 — there
is no such thing as a multi-tenant Aivyx PA instance.

### 5.10 `aivyx-desktop`'s unmaintained GTK3 dependency stack (Linux)

`aivyx-desktop` (the native desktop shell, opt-in, not the daemon or
any operator-facing security boundary described elsewhere in this
document) links `tao` + `wry` + `tray-icon` for its window, embedded
webview, and system tray on Linux. All three transitively pull the
`gtk-rs` GTK3 bindings (`gtk`/`gdk`/`atk`/`glib` and their `-sys`
crates), which carry 11 separate RustSec "unmaintained" advisories
(the GTK3 gtk-rs generation stopped receiving updates after `0.18.x`)
plus one real unsoundness bug, `RUSTSEC-2024-0429`: unsound
`Iterator`/`DoubleEndedIterator` impls on `glib::VariantStrIter`.

**Confirmed 2026-08-27, not just re-read from an older note:** the
*latest* published versions of all three consuming crates as of this
check (`tao 0.37.0`, `wry 0.56.1`, `tray-icon 0.24.2` — each newer
than what `aivyx-desktop` currently pins) still resolve to `gtk 0.18.2`
/ `glib 0.18.5` / `webkit2gtk 2.0.2` on Linux. The tauri-ecosystem
crates have not shipped a GTK4 Linux backend, over two years after the
advisory. A real fix within the current architecture would mean
forking and maintaining patched builds of the whole mutually
version-locked GTK3 binding stack — adopting a maintenance burden
upstream itself hasn't taken on, not a scoped project. The only path
that actually removes the dependency is dropping `aivyx-desktop`'s
embedded-webview model on Linux (e.g. launching the Studio in the
operator's default system browser instead), which is a genuine
architectural change to that crate, not a dependency bump — not
undertaken here.

**Accepted, monitored, not fixed.** `aivyx-desktop` is an optional,
separately-packaged shell (excluded from the CLI's `dist` build,
`crates/aivyx-desktop/Cargo.toml`'s own `dist = false`) around the same
local Studio the daemon already serves over the (auth-gated, see
Gatehouse) Web UI — it adds native chrome, not a new trust boundary or
attack surface beyond "an unmaintained GTK3 binding is loaded into an
opt-in native process on the operator's own machine." Revisit if/when
`tao`/`wry` ship a GTK4 Linux backend, or if the desktop shell's
webview model changes for other reasons.

## 6. Property summary

For operators asking "what should I be able to assume about a
running Aivyx PA daemon":

1. **Confidentiality at rest:** Yes, against anyone without the
   passphrase. AEAD + Argon2id.
2. **Tamper-evidence of the audit chain:** Yes. HMAC-SHA256 chain,
   genesis-seeded, JCS-canonical, cold-verifiable offline.
3. **Authority bounding by tier:** Yes. The agent cannot exceed
   the tier ceiling for a turn, full stop.
4. **Authority bounding by role:** Yes. The operator's role config
   (`aivyx-pa.toml`) attenuates further per the single-inheritance
   tree (`PRODUCT.md` P7, P9).
5. **No silent self-modification:** Yes. Reflection writes go
   through operator-approved gates.
6. **No hosted control plane:** Yes. The operator's API key talks
   directly to the model provider; storage stays on the operator's
   hardware (`PRODUCT.md` G6, N5).
7. **Container-level sandboxing of tools:** **Partial.** `shell.exec`
   and `git.rs`'s three tools (`git.status`/`git.diff`/`git.commit`)
   confine every spawned child process with Landlock + seccomp-bpf
   (`aivyx-confine`, on by default — there is no config option to turn
   confinement itself off; `[confine] require_enforcement`, default
   `true`, only governs whether a *failure* to establish the Landlock
   ruleset fails the spawn closed or lets it run unconfined) — see
   §5.6. One real, code-level exception: a `[git] repos` entry that is
   a linked git worktree or submodule (its `.git` is a file pointing
   elsewhere, not a directory) runs fully unconfined instead — Landlock
   can't reach the real gitdir from the worktree root alone, so `git.rs`
   falls back to no confinement for that specific repo rather than
   breaking it outright. `[[tool_process]]`/MCP external tool
   processes remain on the separate, pre-existing operator-configured
   `bwrap`/`firejail`/`docker` wrapper mechanism (`aivyx-tool/src/
   sandbox.rs`, Phase 52/55/180); that mechanism is opt-in/preset-based,
   not the `aivyx-confine` boundary described here.
8. **Prompt-injection content scanning:** **No.** Operators are
   responsible for what they grant a role authority to do.
9. **Defense against a compromised OS user:** **No.** Outside scope.

## 7. Reporting a vulnerability

If you find a way to break any "Yes" in section 6, or any defense
in section 4, please open an issue or contact the maintainers
privately. Do not publish exploits against the audit chain, the
storage cipher, or the capability check before the maintainers
have had a chance to ship a fix; the operator population is small
enough that responsible disclosure makes a real difference.

## 8. Document discipline

This file is a draft until reviewed alongside `DESIGN.md` D4 + D5
and `PRODUCT.md` P1 + P6 + P10 + P12. When it lands as
non-draft, treat it the way the phase journals treat their
contract documents: edits go through a phase commit so drift is
visible. Section 4 grows when new defenses ship; section 5 shrinks
when forward commitments deliver (notably P12).
