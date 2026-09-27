# Aivyx PA Design — Phase 0

Started 2026-04-13 as an agent-first rebuild following the archival of the
pre-rebuild codebase (`~/_archive/aivyx-2026-04-13/`). Agent-first means the
turn loop is the core of `aivyx-core` from commit one; every other crate
justifies its existence relative to what a running turn loop needs.

---

## Deliverable 1 — The Turn Loop Contract (LOCKED 2026-04-13)

This paragraph is the north star. Every subsequent deliverable is measured
against it: does this make the paragraph easier or harder to say?

> A turn begins when a `ChannelContext` delivers an inbound `Message` to an
> `Agent`. The agent resolves the channel's trust tier and loads the
> corresponding `CapabilitySet`, then enters a tool-calling loop with its
> `LlmProvider`. On each LLM step, the agent either receives streamed text
> to relay back through the channel or receives a structured tool call.
> Each tool call — including memory recall, which is itself a tool — is
> scope-checked against the agent's capability set before execution, and
> on completion returns a `ToolOutcome` that distinguishes "the call
> returned `Ok`" from "the intended effect was observed." Every tool call,
> scope check, and outcome is appended to an HMAC-chained audit log
> synchronously as it happens. The loop terminates when the LLM emits a
> final assistant message, when a tool call returns `RequiresEscalation`,
> when a timeout fires, or when the channel cancels. The turn ends by
> flushing any pending audit entries, returning a `TurnOutcome` to the
> channel, and yielding control.

### What this commits us to

1. **No entry point bypasses a channel.** CLI, desktop, REST, Telegram —
   all are channels. No "internal" turn with no channel context. This
   closes the `aivyx-pa/src/api.rs:526` class of bug by construction.

2. **Trust tier resolution happens first.** Before any LLM call, before
   any tool call, the agent knows what it's allowed to do. Capability
   attenuation is not retrofitted onto a running agent.

3. **Memory is a tool, not an ambient system.** Lazy recall via
   `memory.recall(query)` with scope `memory.read`. Writes via
   `memory.write` with scope `memory.write`. Every memory access is
   audited because it's just a tool call.

4. **Tool success ≠ intent completed.** `ToolOutcome::Completed` carries
   a `verified: bool` flag. Tool authors must think about verification at
   the type level. Closes `feedback_tool_success_vs_intent.md`.

5. **Audit is synchronous, inline, HMAC-chained.** If the process crashes
   mid-turn, the audit log still tells the truth about what got executed.
   The cost is microseconds per tool call; the trustworthiness gain is
   large.

6. **Four termination conditions, named explicitly.** Final assistant
   message, `RequiresEscalation`, timeout, channel cancel. A fifth is a
   design conversation.

7. **Turn ends by yielding control to the channel.** The agent doesn't
   spin waiting for "next message" — control returns to the channel
   adapter between turns.

### What the paragraph deliberately does not say

- Streaming protocol details — deferred to Phase 1 trait design
- Single-shot vs. multi-step tool loop structure — covered by "tool-calling loop"
- Concurrency model (sequential? parallel tools?) — **resolved by
  [Amendment A6](docs/amendments/2026-04-21-parallel-tool-execution.md)**:
  batch dispatch via `join_all` when planner returns `NextStep::ToolCalls`
- Multi-agent coordination — out of scope for v1; **shipped post-v1 as
  Chapter J (the Nonagon)** — a lead convening ≤9 *attenuated, in-process*
  specialists on the one daemon/chain, which preserves (not breaks) the
  single-agent ethos. See [`docs/NONAGON.md`](docs/NONAGON.md).
- Federation / remote agents — out of scope for v1 (still deferred)

### Scenario tests

Four scenarios the paragraph should make easy to reason about:

1. **"Tell me what I worked on yesterday"** — LLM emits `memory.recall`
   tool call, scope-checked, returns recalled entries, LLM summarizes.
   ✓ Paragraph holds.

2. **"Close the terminal window"** — LLM emits `window.close_focused`,
   tool executes and verifies the window is gone, `ToolOutcome::Completed
   { verified: true }`. ✓ Paragraph holds; closes the Alfred window-close
   bug class.

3. **"Run rm -rf ~ from Telegram"** — Trust tier 2 excludes `shell.exec`.
   Scope check denies. `ToolOutcome::Denied`. LLM sees denial and either
   explains or escalates. ✓ Paragraph holds.

4. **"User hits Ctrl-C mid-tool-call"** — In-flight tool finishes, loop
   checks cancellation before next LLM step, terminates with
   `TurnOutcome::Cancelled`. ✓ Paragraph holds; open question is
   *when* the cancellation check fires (between LLM steps, not
   mid-tool-call). Defer to Phase 1.

### Concepts named but not yet defined (Phase 1 work)

- **`ChannelContext`** — delivers messages, carries trust tier, supports
  cancellation. Likely `&dyn ChannelContext` at trait boundaries.

- **`TurnOutcome`** — at minimum: `Completed`, `Cancelled`, `Escalated`,
  `TimedOut`, `Failed(AivyxError)`. Possibly `AwaitingInput`.

- **`ToolOutcome`** — at minimum: `Completed { verified: bool }`,
  `Denied { scope: Scope }`, `RequiresEscalation { reason: String }`,
  `Failed(AivyxError)`.

> **Amendment (2026-04-17):** The turn loop described above now runs
> inside a daemon process. Channel frontends deliver messages over a
> Unix-domain-socket IPC protocol rather than by direct trait call.
> See amendment
> [`docs/amendments/2026-04-17-daemon-ipc-protocol.md`](docs/amendments/2026-04-17-daemon-ipc-protocol.md)
> for the daemon execution topology, IPC envelope types, and
> multi-connection model that implement this contract in production.
> See also amendment
> [`docs/amendments/2026-04-17-mission-state-machine.md`](docs/amendments/2026-04-17-mission-state-machine.md)
> for the fifth termination condition (`TurnOutcome::Escalated` as
> mission gate suspension).
>
> **Amendment (2026-04-21):** The IPC protocol now supports optional
> version negotiation via `ProtocolNegotiation`/`ProtocolAccepted`/
> `ProtocolRejected` messages. For v0.1, the daemon always accepts.
> The daemon also writes a `daemon.state` file for crash recovery
> and emits `RecoveryNotice` lifecycle events on restart after
> unclean shutdown. See amendment
> [`docs/amendments/2026-04-21-protocol-negotiation.md`](docs/amendments/2026-04-21-protocol-negotiation.md).

---

---

## Deliverable 2 — The Open-Core Line (LOCKED 2026-04-13)

> **Amendment A14 (2026-06-19) — relicense MIT → BUSL-1.1.** Chapter Charter
> ([`docs/LICENSING.md`](docs/LICENSING.md),
> [Amendment A14](docs/amendments/2026-06-19-busl-relicense.md)) supersedes the
> *mechanism* of this Deliverable. Every "MIT" entry in the rule and tables below
> now reads **"BUSL-1.1 (→ MIT after 4 years)"**: the whole public workspace is
> **source-available under BUSL-1.1** — free for personal/non-commercial use,
> paid for commercial use, auto-reverting to MIT four years after each release.
> The *spirit* (auditable free core + sustainable commercial side) is preserved
> and strengthened — the core now carries its own monetization hook, not just the
> separate products below. v0.2.0 and prior remain MIT in perpetuity. The
> branding exception is unchanged in substance (`aivyx-pa` is now "BUSL-1.1 +
> branded"). Read the original text below as historical-but-superseded.

### The Rule

> **Protocol and security surface are MIT. Products are commercial.
> Ambiguous cases default to MIT if they would otherwise weaken the
> agent's auditability.**

### How to apply it

For any new crate or component, ask in order:

1. **Is it a protocol?** (Trait, interface, wire format, namespace.) → MIT
2. **Is it a security surface?** (Crypto, capability, audit, auth.) → MIT
3. **Is it a product?** (Runtime, marketplace, hosted service.) → Commercial
4. **If ambiguous:** Would closing it weaken auditability or block
   third-party integration? → MIT. Otherwise → Commercial.

### The v1 table

| Crate / Artifact | License | Rationale |
|---|---|---|
| `aivyx-core` | MIT | The protocol itself — traits, IDs, turn loop. |
| `aivyx-crypto` | MIT | Commodity crypto primitives. No moat. |
| `aivyx-capability` | MIT | Security surface. Must be readable to be trustable. |
| `aivyx-audit` | MIT | Security surface. Closed audit is worth less. |
| `aivyx-config` | MIT | Commodity config loader. |
| `aivyx-storage` | MIT | redb + HKDF wrapper. Security surface. |
| `aivyx-channel` | MIT | Trait + `LocalChannel` reference impl. Protocol. |
| `aivyx-llm` | MIT | `LlmProvider` trait + reference impls. Protocol. |
| `aivyx-memory` | MIT | `Memory` trait + default impl. Protocol + commodity. |
| `aivyx-pa` (rebuilt PA) | **MIT + branded** | Code MIT, name/distribution trademarked. |
| Channel adapters (Telegram/Discord/Slack/Matrix/Email) | MIT | Reference impls of `aivyx-channel`. |

**11 crates in v1. All MIT.** The commercial side of the open-core line
is composed entirely of Phase 7+ products built on top of v1.

### Deferred to later phases (not in v1)

| Component | License | Notes |
|---|---|---|
| `aivyx-engine` / factory | Commercial | Multi-tenant runtime. The product. |
| `aivyx-hub` | Commercial | Marketplace + network effects. |
| `aivyx-federation` | Commercial *(tentative)* | Revisit when built. |
| `aivyx-mcp` | MIT *(when built)* | Bridge to an open protocol. |

### The branding exception (aivyx-pa)

`aivyx-pa` is MIT + branded. This means:

- **The code** is MIT-licensed. Anyone can fork, modify, redistribute,
  commercialize.
- **The name, logo, and official distribution channels** are trademarked.
  A fork cannot call itself "Aivyx PA" or ship via Aivyx-branded app
  store listings, official installers, or aivyx.ai domains.
- Enforcement: trademark, not license. `LICENSE` says MIT; `TRADEMARK.md`
  documents the brand restriction.
- Precedents: Code-OSS / VSCode, Chromium / Chrome, Firefox / Iceweasel.

---

## Deliverable 3 — Agent Trait + Outcome Types (LOCKED 2026-04-13)

Pseudo-code sketches, not compilable Rust. The goal is pinning down shapes
and names, not producing something `cargo check` accepts. Supporting types
(`TrustTier`, `CapabilitySet`, `Scope`, `AivyxError`) are referenced here
and defined in later deliverables.

### Message — the inbound unit

```rust
pub struct Message {
    pub id: MessageId,
    pub session_id: SessionId,
    pub content: MessageContent,
    pub received_at: SystemTime,
}

pub enum MessageContent {
    Text(String),
    // Phase 2: Image, Audio, File, StructuredData
}
```

> **Multimodal extension** — the actual `MessageContent` /
> `ContentPart` enums in `aivyx-core` carry `Image` and
> `Document` variants alongside `Text`. The `Image` variant
> landed during the multimodal MVP work; the `Document`
> variant is governed by
> [`docs/amendments/2026-06-04-content-part-document.md`](docs/amendments/2026-06-04-content-part-document.md)
> (A13) so PDFs route to provider-specific document content
> blocks rather than image blocks.

### ChannelContext — the delivery surface

```rust
#[async_trait]
pub trait ChannelContext: Send + Sync {
    fn channel_name(&self) -> &str;
    fn platform(&self) -> ChannelPlatform;
    fn trust_tier(&self) -> TrustTier;
    fn session_id(&self) -> SessionId;

    async fn stream_event(&self, event: StreamEvent<'_>) -> Result<(), ChannelError>;
    async fn finalize(&self, outcome: &TurnOutcome) -> Result<(), ChannelError>;

    fn cancellation_token(&self) -> CancellationToken;
}

pub enum ChannelPlatform {
    Local,       // CLI, desktop app
    Telegram,
    Discord,
    Slack,
    Matrix,
    Email,
    Rest,        // HTTP API
    // extensible
}

pub enum StreamEvent<'a> {
    /// LLM token stream — the 95% case.
    Text(&'a str),

    /// Status signal for long-running tools: "Searching…", "Downloading…".
    Status(&'a str),

    /// A tool call is about to execute. Channels may render (e.g., GUI)
    /// or ignore (e.g., minimal REST channels).
    ToolCallStarted {
        tool: ToolId,
        input: &'a serde_json::Value,
    },

    /// A tool call finished. Outcome summary is a human-readable
    /// one-liner, not a structured result.
    ToolCallFinished {
        tool: ToolId,
        outcome_summary: &'a str,
    },

    /// File, image, or audio attachment. Matrix/Email channels use
    /// this on day one when they're rebuilt.
    Attachment {
        kind: AttachmentKind,
        data: &'a [u8],
        filename: Option<&'a str>,
    },
}

pub enum AttachmentKind {
    Image { mime: &'static str },  // e.g., "image/png"
    Audio { mime: &'static str },
    File  { mime: &'static str },
}
```

**Key commitments:**
- `&dyn ChannelContext` at trait boundaries (dynamic dispatch — swapping channels at runtime is worth the cost)
- `trust_tier()` is *read* from the channel, not set on it
- **`stream_event` is a rich enum from day one** (decision: "start how we mean to go on"). Text is the 95% case; Status / ToolCallStarted / ToolCallFinished / Attachment cover the rest.
- `StreamEvent<'a>` is borrowed — agents build events that reference their own buffers without allocating. Channels that need ownership can serialize or clone.
- Channels are free to **ignore** any variant they don't care about. `StreamEvent` is a superset of what a rich channel *can* do, not a minimum every channel must honor.
- MIME types are `&'static str` for now — small, finite, auditable set. Promote to `Cow<'static, str>` if user-supplied MIMEs become necessary.
- `cancellation_token` resolves scenario-4 ambiguity: cancellation is checked between LLM steps, not mid-tool-call

### ToolOutcome — with typed verification

```rust
pub enum ToolOutcome {
    Completed {
        output: serde_json::Value,
        verified: Verification,
    },
    Denied {
        scope: Scope,
        held: CapabilitySet,
    },
    RequiresEscalation {
        reason: String,
    },
    Failed(AivyxError),
}

pub enum Verification {
    /// Tool queried the system and confirmed its effect happened.
    Verified,
    /// Tool returned Ok but did not verify.
    Unverified,
    /// Verification not meaningful (e.g., pure read-only query).
    NotApplicable,
}
```

**Why `Verification` is an enum, not a bool:** `NotApplicable` captures
the "calculator doesn't need to verify 2+2=4" case explicitly, so tool
authors don't lie by marking everything `Verified`. Encodes
`feedback_tool_success_vs_intent.md` at the type level.

### TurnOutcome — the five termination paths

```rust
pub enum TurnOutcome {
    Completed {
        final_message: String,
        tool_calls_made: usize,
        duration: Duration,
    },
    Escalated {
        reason: String,
        pending_tool: ToolId,
        tool_calls_made: usize,
    },
    TimedOut {
        tool_calls_made: usize,
        elapsed: Duration,
    },
    Cancelled {
        tool_calls_made: usize,
    },
    Failed(AivyxError),
}
```

Five variants matching the four paragraph-named termination conditions
plus failure. No `AwaitingInput` — speculative, not needed yet.

### Agent — four lines

```rust
#[async_trait]
pub trait Agent: Send + Sync {
    fn id(&self) -> AgentId;
    fn capabilities(&self) -> &CapabilitySet;

    async fn turn(
        &self,
        message: Message,
        channel: &dyn ChannelContext,
    ) -> TurnOutcome;
}
```

**Key commitments:**
- `&self`, not `&mut self` — agents shared across concurrent turns via `Arc<dyn Agent>`
- Returns `TurnOutcome` directly, **not** `Result<TurnOutcome, _>`. Any error is part of the turn's history (`Failed(AivyxError)` variant). A turn always *completes in some way*.
- No explicit timeout parameter — budget lives in the agent's config
- No session parameter — `session_id` lives inside `Message`

**The unusual choice:** returning `TurnOutcome` rather than `Result<TurnOutcome, _>`
is contrarian. The Rust instinct is to surface errors in the type. But for
`turn`, errors are *part of what happened*, audited inline, and need to be
rendered by the channel. Burying them inside `TurnOutcome::Failed` keeps
the return type honest about "every turn completes."

### Tool — with ToolContext

```rust
#[async_trait]
pub trait Tool: Send + Sync {
    fn id(&self) -> ToolId;
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn input_schema(&self) -> &serde_json::Value;  // JSON Schema
    fn required_scope(&self) -> Scope;

    async fn execute(
        &self,
        input: serde_json::Value,
        context: &ToolContext<'_>,
    ) -> ToolOutcome;
}

pub struct ToolContext<'a> {
    pub agent_id: AgentId,
    pub session_id: SessionId,
    pub channel: &'a dyn ChannelContext,
    pub audit: &'a dyn AuditWriter,
    pub cancellation: &'a CancellationToken,
}
```

**Key commitments:**
- `execute` returns `ToolOutcome` directly, same pattern as `Agent::turn`
- `ToolContext` gives tools access to channel (progress streaming), audit writer (structured entries), and cancellation
- `required_scope` returns single `Scope` — composite-scope handling deferred to Deliverable 4

### Paragraph-to-type traceability

Every clause in the Deliverable 1 paragraph traces to a concrete type
operation:

| Paragraph clause | Type operation |
|---|---|
| "ChannelContext delivers Message to Agent" | `agent.turn(message, channel)` |
| "resolves the channel's trust tier" | `channel.trust_tier()` |
| "loads the corresponding CapabilitySet" | Agent attenuates `self.capabilities()` by trust tier |
| "tool-calling loop with its LlmProvider" | Internal `llm.chat_stream(...)` |
| "streamed text to relay back through the channel" | `channel.stream_event(StreamEvent::Text(chunk)).await` |
| "scope-checked before execution" | `if !caps.grants(tool.required_scope())` → `ToolOutcome::Denied` |
| "on completion returns a ToolOutcome" | `tool.execute(input, &ctx).await` |
| "distinguishes Ok from intended effect" | `ToolOutcome::Completed { verified: Verification }` |
| "HMAC-chained audit synchronously as it happens" | `ctx.audit.append(entry).await` (blocking) |
| "LLM emits a final assistant message" | Stream end → `TurnOutcome::Completed` |
| "tool call returns RequiresEscalation" | Match on `ToolOutcome::RequiresEscalation` → `TurnOutcome::Escalated` |
| "timeout fires" | Elapsed > budget → `TurnOutcome::TimedOut` |
| "channel cancels" | `cancellation.is_cancelled()` → `TurnOutcome::Cancelled` |
| "returning TurnOutcome to the channel" | `channel.finalize(&outcome).await; outcome` |
| "yielding control" | Function returns, tokio yields to runtime |

The paragraph compiles.

### Deliberately omitted from this sketch

- **`LlmProvider`** — already stable in the archived codebase, re-derived in Phase 1 without design changes
- **`AuditWriter` full definition** — referenced in `ToolContext`, fully defined in Deliverable 6 (error contract) or a separate audit deliverable
- **`SessionStore` / `MemoryStore`** — implementation details of concrete agents, not core contracts. Memory is a tool (Deliverable 1 commitment A2); underlying storage is private to the concrete agent
- **Agent construction / builder** — the sketch shows what an agent *is*, not how to build one. Builder lives in concrete impl

> **Amendment (2026-04-17):** The `ChannelContext` trait is now
> implemented twice per channel: once for in-process mode (the
> original adapter) and once for daemon mode (`IpcChannelBridge`
> on the server side). `StreamEvent<'a>` has an owned IPC mirror
> (`StreamEventPayload`) for serialization over the wire. See
> amendment
> [`docs/amendments/2026-04-17-daemon-ipc-protocol.md`](docs/amendments/2026-04-17-daemon-ipc-protocol.md).

---

## Deliverable 4 — Capability Taxonomy (LOCKED 2026-04-13)

Four foundational decisions (locked):
- **Scope shape**: hierarchical strings with a type wrapper (`Scope(String)`)
- **Attenuation**: prefix-based (glob / URL-prefix / allowlist / simple-glob)
- **Taxonomy completeness**: start with ~20 active scopes, reserve the rest
- **Audit events**: per-tool for grants, per-scope for denials, with dedicated `MemoryAccess` for queryability

### The Scope type

```rust
/// Hierarchical string form: `base` or `base:qualifier`.
/// Base is a dotted identifier; qualifier is an optional attenuation string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Scope(String);

impl Scope {
    /// Returns None if the base is not a known scope in v1 or any registered extension.
    pub fn parse(s: &str) -> Option<Scope>;
    pub fn base(&self) -> &str;
    pub fn qualifier(&self) -> Option<&str>;

    /// Prefix-attenuation check: true iff `self` is granted by `other`.
    pub fn is_granted_by(&self, other: &Scope) -> bool;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilitySet {
    scopes: Vec<Scope>,  // deduplicated, sorted for deterministic serialization
}

impl CapabilitySet {
    pub fn empty() -> Self;
    pub fn from_scopes(scopes: impl IntoIterator<Item = Scope>) -> Self;
    pub fn grants(&self, needed: &Scope) -> bool;
    pub fn intersect(&self, other: &CapabilitySet) -> CapabilitySet;
    pub fn iter(&self) -> impl Iterator<Item = &Scope>;
}
```

**Key commitments:**
- `Scope::parse` returns `Option` — unknown scopes fail at parse time, not check time. Registration lives in `aivyx-capability`.
- `is_granted_by` is the *only* authoritative check function. No scattered string comparisons.
- `CapabilitySet::intersect` is how trust tiers cap agent caps: `effective = agent_caps.intersect(&tier_ceiling)`.
- Sorted vec, not HashSet — deterministic serialization is required for HMAC-chained audit.

### Attenuation rules

When a scope has a qualifier, `is_granted_by` applies:

1. **Bases must match exactly.** `fs.read:/foo` is not granted by `fs.write:/foo`.
2. **Unqualified held grants any qualified needed** with the same base.
3. **Qualified held grants qualified needed** iff the qualifier matches:
   - Path-like (`/` or contains `/`): glob semantics
   - URL-like (contains `://`): URL prefix match
   - Allowlist (comma-separated idents): superset check
   - Simple glob: glob match on the string
4. **Qualified held does NOT grant unqualified needed.** A tool requesting bare `fs.read` wants unrestricted access; a restricted capability cannot grant that.

Rule 4 is intentional: tools must be specific. Unrestricted requests are visible to users at the capability-set level and require justification.

### The v1 active namespace (21 scopes)

**`fs` — filesystem**

| Scope | Qualifier | Description |
|---|---|---|
| `fs.read` | path glob | Read file contents or list directory |
| `fs.write` | path glob | Create or overwrite file |
| `fs.delete` | path glob | Remove file or directory |
| `fs.metadata` | path glob | Stat — size, mtime, perms. Weaker than fs.read. |

**`net` — network**

| Scope | Qualifier | Description |
|---|---|---|
| `net.fetch` | URL prefix | HTTP GET / HEAD |
| `net.post` | URL prefix | HTTP POST / PUT / PATCH / DELETE |
| `net.dns` | domain glob | Hostname resolution |

**`shell` — subprocess**

| Scope | Qualifier | Description |
|---|---|---|
| `shell.exec` | command allowlist | Run a command, capture output |
| `shell.spawn` | command allowlist | Spawn long-lived subprocess |

**`git` — version control** *(read added in Amendment A12, Phase 109; write added in Amendment A13b, Chapter Forge)*

| Scope | Qualifier | Description |
|---|---|---|
| `git.read` | repo path glob | Read repo state — `git.status` and `git.diff` share this base |
| `git.write` | repo path glob | Write repo history — `git.commit` (stage + commit); Trusted-tier only, confirm-first |

> **Amendment (2026-05-28):** `git.read` joins the substrate
> base table as the qualifier-by-repo-path gate for the
> `git.status` + `git.diff` tools added in Phase 109. See
> amendment
> [`docs/amendments/2026-05-28-substrate-tool-count-thirteen.md`](docs/amendments/2026-05-28-substrate-tool-count-thirteen.md)
> for the P10 count update from ten to thirteen and the
> shared-scope-base rationale (one `git.read` rather than
> separate `git.status` / `git.diff` bases, mirroring the
> read-invariant grouping).
>
> **Amendment (2026-06-19):** `git.write` joins the table as the
> destructive sibling A12 anticipated — the qualifier-by-repo-
> path gate for the `git.commit` tool (Chapter Forge). It reuses
> `git.read`'s `[git] repos` allow-set, sits at `CEILING_TRUSTED`
> only (writing history is as sensitive as `shell.exec` /
> `fs.delete`), and is confirm-first when `confirm_destructive`
> is on. See amendment
> [`docs/amendments/2026-06-19-substrate-tool-count-fifteen.md`](docs/amendments/2026-06-19-substrate-tool-count-fifteen.md)
> for the P10 count update from thirteen to fifteen (also adding
> `web.extract` over the existing `net.fetch` base).

**`skills` — learned procedural pattern surface** *(added in Phase 110)*

| Scope | Qualifier | Description |
|---|---|---|
| `skills.propose` | — | Propose a `LearnedSkill` delta through `reflection.propose` |
| `skills.list` | — | Enumerate operator-approved skills |
| `skills.invoke` | — | Render the full procedure body of one approved skill |

> **Phase 110 — Skills Auto-Creation (Reflection Staging).**
> Three new scope bases gate the skills primitive:
> `skills.propose` is the sibling capability to
> `persona.propose` for proposals whose `persona_deltas` array
> contains any `PersonaDeltaCategory::LearnedSkill` entry
> (Q2b at Phase 110 sign-off — operator picked per-category
> granularity over extending `persona.propose`).
> `skills.list` and `skills.invoke` gate the two substrate
> tools that let agents read the approved skill set
> (enumeration vs. on-demand procedure-body render). All three
> bases live in `CEILING_TRUSTED` only; SemiTrusted remote
> adapters do not get skills.* by default. **No amendment
> filed** — Phase 110 stays inside PRODUCT.md P8's
> outcome-driven-audited-reflection envelope; the
> propose-approve-apply shape P8 commits to is unchanged, and
> the LearnedSkill extension is a new category inside that
> shape rather than a new commitment. P10's substrate tool
> count was last set to thirteen at A12 (Phase 109); skills.*
> tools are **infrastructure tools** per P10's
> substrate/infrastructure/third-party taxonomy (the agent
> uses them to manage itself's identity layer) so they are
> not counted against P10's substrate-tool cap.

**`graph` — typed knowledge graph** *(added in Chapter Lattice)*

| Scope | Qualifier | Description |
|---|---|---|
| `graph.read` | — | Query the agent's typed knowledge graph — a read-only multi-hop directed traversal (`graph.query`) |

> **Chapter Lattice — typed knowledge graph.** `graph.read`
> gates `graph.query`, a read-only multi-hop traversal of the
> agent's own knowledge graph (entities + directed
> `(subject)-[predicate]->(object)` relations extracted from
> memory). Like `skills.*`, it is an **infrastructure tool**
> per P10's taxonomy — the agent querying its own *derived*
> self-knowledge, not a new operator-owned resource primitive —
> so it grows `KNOWN_BASES` (86 → 87) **without a P10 substrate-
> count amendment**, exactly the `skills.*` / `loop.*` /
> `reflection.*` precedent. Bare base (like `skills.list`),
> `CEILING_TRUSTED` only; SemiTrusted does not get it by
> default. See
> [`docs/amendments/2026-04-17-capability-taxonomy-growth.md`](docs/amendments/2026-04-17-capability-taxonomy-growth.md)
> (Lattice addendum).

**`llm` — language model**

| Scope | Qualifier | Description |
|---|---|---|
| `llm.call` | model name glob | Chat endpoint call |
| `llm.embed` | model name glob | Embedding endpoint call |

**`memory` — agent memory**

| Scope | Qualifier | Description |
|---|---|---|
| `memory.read` | session/topic glob | Recall entries matching a query |
| `memory.write` | session/topic glob | Store entry |
| `memory.forget` | session/topic glob | Delete entries matching filter |

**`channel` — communication**

| Scope | Qualifier | Description |
|---|---|---|
| `channel.send` | channel name glob | Send on an outbound channel (not necessarily the inbound one) |
| `channel.receive` | channel name glob | Listen on a channel |

The turn's own inbound channel is **implicitly granted** — you don't need `channel.send` to reply where the message came from.

**`audit` — introspection**

| Scope | Qualifier | Description |
|---|---|---|
| `audit.read` | event type glob | Read audit log entries |

There is no `audit.write`. Only the kernel writes audit entries.

**`config` — configuration**

| Scope | Qualifier | Description |
|---|---|---|
| `config.read` | key glob | Read config value |
| `config.write` | key glob | Update config value |

### Reserved — named but not defined in v1

These are part of the public namespace but have no implementation. Any
tool using them fails at `Scope::parse` time until promoted.

- **Agent coordination**: `agent.spawn`, `agent.delegate`, `agent.terminate`
- **Scheduling**: `schedule.create`, `schedule.read`, `schedule.delete`, `schedule.run`
- **System**: `system.shutdown`, `system.notify`, `system.clipboard_read`, `system.clipboard_write`
- **Display / window**: `display.window_focus`, `display.window_close`, `display.screenshot`
- **Federation** (Phase 7+): `federation.discover`, `federation.call`, `federation.accept`

### Audit event naming (mixed model)

```rust
pub enum AuditEvent {
    /// A tool executed. Primary key: tool_id.
    ToolCall {
        tool_id: ToolId,
        scope_used: Scope,
        input_hash: [u8; 32],           // hash, not raw input — secrets safety
        outcome: ToolOutcomeSummary,
        duration: Duration,
    },

    /// A scope check denied a tool call. Primary key: scope.
    ScopeDenied {
        scope_requested: Scope,
        scope_qualifier: Option<String>,
        tool_attempted: ToolId,
        held_capabilities: CapabilitySet,  // snapshot, not reference
    },

    /// Turn started — for correlation via turn_id.
    TurnStarted {
        turn_id: TurnId,
        session_id: SessionId,
        channel: ChannelPlatform,
        trust_tier: TrustTier,
        effective_capabilities: CapabilitySet,
    },

    /// Turn ended — paired with TurnStarted.
    TurnEnded {
        turn_id: TurnId,
        outcome: TurnOutcomeSummary,
        tool_calls_made: usize,
        duration: Duration,
    },

    /// Dedicated view of memory operations for queryability.
    /// Redundant with ToolCall (every memory op is also a tool call).
    MemoryAccess {
        turn_id: TurnId,
        operation: MemoryOperation,
        scope: Scope,
        query_or_key: String,
    },
}

pub enum MemoryOperation { Read, Write, Forget }
```

**Commitments:**
- Every event is self-contained — readable without cross-referencing others.
- `input_hash` (not raw input) prevents secrets leaking into the audit log.
- `held_capabilities` on denials is a snapshot — the set at denial time is preserved even if caps change later.
- `MemoryAccess` is redundant with `ToolCall` but indexed for fast memory-specific queries. This is the one deviation from the strict mixed model.

### Findings from sanity-checking against D1 scenarios

**A. `required_scope()` needs to be computable from input, not static.**
A tool may need different scopes depending on its arguments. The `Tool`
trait's `required_scope()` should become:

```rust
fn required_scope(&self, input: &serde_json::Value) -> Scope;
```

This lets a memory-recall tool request `memory.read:session:<id>` when
called with a specific session ID, or bare `memory.read` when called
with no filter. **Update to Deliverable 3 pending** — will fold into the
final D3 skeleton during Phase 1 trait design.

**B. Scenario 2 (window close) requires scopes in the Reserved section.**
The v1 PA does not handle window control. Window-control tools and their
scopes (`display.window_close`, etc.) are deferred to Phase 1, when the
tool crate is built. The Reserved section already lists them; they'll be
promoted to active when the tool lands.

> **Amendment (2026-04-17):** The capability taxonomy has grown
> from 12 to 23 known bases across Phases 1–21. Two bases
> added by Phase 21 (`mission.create`, `mission.gate`) are
> infrastructure tools per P10's taxonomy. See amendment
> [`docs/amendments/2026-04-17-mission-state-machine.md`](docs/amendments/2026-04-17-mission-state-machine.md)
> for the mission-specific bases and amendment
> [`docs/amendments/2026-04-17-capability-taxonomy-growth.md`](docs/amendments/2026-04-17-capability-taxonomy-growth.md)
> for the full 23-base inventory.

---

## Deliverable 5 — Trust Tier Model (LOCKED 2026-04-13)

### The TrustTier type

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum TrustTier {
    /// Tier 3 — Untrusted. Public webhooks, anonymous HTTP, unknown senders.
    Untrusted,

    /// Tier 2 — SemiTrusted. Authenticated user on a remote channel.
    SemiTrusted,

    /// Tier 1 — Trusted. Authenticated user on an owned channel.
    Trusted,

    /// Tier 0 — Kernel. Unconditional. Never assigned to a user-facing channel.
    Kernel,
}

impl TrustTier {
    pub fn default_ceiling(self) -> &'static CapabilitySet;
    pub fn is_more_trusted_than(self, other: TrustTier) -> bool;
}
```

**Note on ordering:** the variants are declared in *ascending trust order*
so the derived `Ord` gives `Kernel > Trusted > SemiTrusted > Untrusted`.
The display-label numbers (Tier 0 = Kernel, Tier 3 = Untrusted) run
opposite to `Ord`. The numeric labels are a *naming convention*; the
ordering used in code is the enum's `Ord`.

### Tier definitions

**Tier 0 — Kernel.** Unconditional. The turn loop itself, audit writer,
capability enforcer, storage encryption. Never held by a user-facing
channel — a channel returning `Kernel` from `trust_tier()` is a bug.
Default ceiling: unlimited. Exists so internal operations pass capability
checks without being special-cased as "exempt."

**Tier 1 — Trusted.** Authenticated user on an owned channel:
- Local CLI
- Desktop GUI (Tauri app on own machine)
- Local REST API bound to 127.0.0.1 only

The user is presumed present; the channel cannot be reached from the
network without explicit local access. Default ceiling: near-total, with
extra audit on `fs.delete` / `shell.exec` / `shell.spawn`.

**Tier 2 — SemiTrusted.** Authenticated user on a remote channel:
- Telegram DMs from allowlisted user
- Slack DMs from allowlisted user
- Matrix rooms where sender is authenticated
- Email from SPF/DKIM-verified, allowlisted sender
- Discord DMs from allowlisted user

The user is identified but the transport is out of our control; identity
verification is best-effort. Default ceiling: substantially narrower
than Tier 1 — no unqualified shell, no `fs.delete`, no `config.write`,
remote-write/remote-post require explicit qualifiers.

**Tier 3 — Untrusted.** Anonymous or unverified contexts:
- WhatsApp webhooks (public, HMAC-verified but sender spoofable)
- Anonymous HTTP endpoints (if they exist)
- Email from non-allowlisted senders
- Any context where identity cannot be trusted

Default ceiling: near-empty. Channels at this tier can *deliver* a
message but the agent cannot exercise meaningful capabilities — only
canned responses or public-safe reads.

### Per-tier ceiling table

Notation: ✓ = granted unqualified, ▲ = granted only with explicit
qualifier, ⊘ = denied entirely.

| Scope | Tier 1 (Trusted) | Tier 2 (SemiTrusted) | Tier 3 (Untrusted) |
|---|---|---|---|
| `fs.read` | ✓ | ▲ path qualifier required | ⊘ |
| `fs.write` | ✓ | ▲ path qualifier required | ⊘ |
| `fs.delete` | ✓ (audited) | ⊘ | ⊘ |
| `fs.metadata` | ✓ | ✓ | ⊘ |
| `net.fetch` | ✓ | ✓ | ⊘ |
| `net.post` | ✓ | ▲ URL prefix required | ⊘ |
| `net.dns` | ✓ | ✓ | ⊘ |
| `shell.exec` | ✓ (audited) | ⊘ | ⊘ |
| `shell.spawn` | ✓ (audited) | ⊘ | ⊘ |
| `llm.call` | ✓ | ✓ | ⊘ |
| `llm.embed` | ✓ | ✓ | ⊘ |
| `memory.read` | ✓ | ✓ | ▲ `memory.read:scope:public:*` only |
| `memory.write` | ✓ | ✓ | ⊘ |
| `memory.forget` | ✓ | ▲ scope qualifier required | ⊘ |
| `channel.send` | ✓ | ▲ channel allowlist | ⊘ |
| `channel.receive` | ✓ | ▲ channel allowlist | ⊘ |
| `audit.read` | ✓ | ▲ event type qualifier | ▲ `audit.read:public` only |
| `config.read` | ✓ | ✓ | ⊘ |
| `config.write` | ✓ | ⊘ | ⊘ |

**The ▲ semantic:** when a tier grants a scope "only with qualifier,"
held capability must be a *qualified* form — holding bare `fs.write`
does NOT satisfy `▲` at Tier 2. Remote channels must request narrowly
scoped capabilities, not broad ones.

### The effective-capabilities computation

```rust
// Inside Agent::turn, exactly once before the tool-calling loop:
let tier = channel.trust_tier();
let ceiling = tier.default_ceiling();
let effective = self.capabilities().intersect(ceiling);
```

**Commitments:**
- Computed **once per turn**, before any LLM call. Not recomputed mid-turn.
- Recorded as `effective_capabilities` in the `TurnStarted` audit event.
- This snapshot is authoritative for the entire turn.
- Mid-turn capability changes (e.g., user-confirmed escalation) are NOT
  supported in v1 — they're a dedicated escalation mechanism, out of scope.

### Design principle — "build narrower tools, don't loosen ceilings"

When a tier can't execute a scope and you want that capability from that
tier, the right answer is **a dedicated narrower-scope tool**, not a
ceiling relaxation. Example:

- Problem: "I want `git status` to work from Telegram"
- Wrong fix: grant `shell.exec` at Tier 2
- Right fix: build a `git.status` tool that holds a narrower scope
  (e.g., `git.read:<repo>`) which is grantable at Tier 2

This keeps the ceilings stable and the attack surface understandable.
Every loosened ceiling is a new class of attack; every narrower tool is
a new controlled primitive.

### Sanity check against D1 scenarios

| Scenario | Tier | Outcome |
|---|---|---|
| "What did I work on yesterday" (CLI) | Trusted | ✓ `memory.read:session:*` granted, tool runs |
| "Close terminal window" | Trusted | Deferred — window scopes are Reserved |
| "Run rm -rf from Telegram" | SemiTrusted | ⊘ `shell.exec` denied by ceiling, `ToolOutcome::Denied` |
| "git status from Telegram" | SemiTrusted | ⊘ Same denial — use a dedicated `git.status` tool |
| User cancellation mid-turn | Any | Tier-independent, handled by `cancellation_token` |

---

## Deliverable 6 — Error Contract (LOCKED 2026-04-13)

### The variants (14 of a 15-variant cap)

```rust
#[derive(Debug, thiserror::Error)]
pub enum AivyxError {
    // Configuration & Startup
    #[error("configuration error: {0}")]
    Config(String),

    #[error("storage error: {0}")]
    Storage(#[from] StorageError),

    #[error("crypto error: {0}")]
    Crypto(#[from] CryptoError),

    // Capability & Trust
    #[error("capability denied: scope {scope} not held")]
    CapabilityDenied { scope: Scope, held: CapabilitySet },

    #[error("invalid scope: {0}")]
    InvalidScope(String),

    // Agent Turn Execution
    #[error("LLM provider error: {0}")]
    Llm(#[from] LlmError),

    #[error("tool error in {tool}: {source}")]
    Tool {
        tool: ToolId,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },

    #[error("tool {tool} requires escalation: {reason}")]
    ToolEscalation { tool: ToolId, reason: String },

    // Channel & Transport
    #[error("channel error: {0}")]
    Channel(#[from] ChannelError),

    // Audit & Integrity
    #[error("audit integrity error: {0}")]
    Audit(String),

    // Time & Cancellation
    #[error("operation timed out after {0:?}")]
    Timeout(std::time::Duration),

    #[error("operation cancelled")]
    Cancelled,

    // Lookup & Identity
    #[error("not found: {kind} {id}")]
    NotFound { kind: &'static str, id: String },

    // Catch-all
    #[error("internal error: {0}")]
    Internal(String),
}
```

### Why each variant exists

| Variant | Source in D1–D5 | Why not another variant |
|---|---|---|
| `Config` | D7 config loading | Startup failures, before any tool/channel exists |
| `Storage` | D7 redb wrapping | Rich nested failure modes (corruption, locking, crypto) |
| `Crypto` | D2, D7 | Rich nested failure modes (AEAD, HKDF, Argon2) |
| `CapabilityDenied` | D4, D5 | Carries scope + held set for diagnostics |
| `InvalidScope` | D4 `Scope::parse` | Parse failure is distinct from check failure |
| `Llm` | D1 tool loop | Rich nested: rate limits, context overflow, auth |
| `Tool` | D1, D3 `ToolOutcome::Failed` | Plugin surface — source is erased |
| `ToolEscalation` | D1, D3 `RequiresEscalation` | Not a failure — a structured pause request |
| `Channel` | D1, D3 `stream_event` | Platform-specific failures preserved in nested type |
| `Audit` | D1 HMAC-chained audit | Integrity failures are their own class — not `Storage` |
| `Timeout` | D1, D3 `TimedOut` | Unambiguous, carries budget |
| `Cancelled` | D1, D3 `Cancelled` | User-initiated, not a real error — separable in match |
| `NotFound` | D3 lookups | `kind` field avoids needing 4 variants |
| `Internal` | Last resort | Should be rare; each usage tagged TODO for promotion |

### Nested error types (not counted against the cap)

Four variants wrap nested types that live in their own crates and can be
as rich as needed: `StorageError`, `CryptoError`, `LlmError`, `ChannelError`.
Top-level `AivyxError` captures **kind for a caller's purposes** (retry?
escalate? give up?); nested enums capture **detail for a debugger's
purposes**.

### The design rule

> **Adding a 15th variant requires a design note in DESIGN.md explaining
> why no existing variant fits. Adding a 16th requires a conversation.**

This is the forcing function against drift. The archived codebase had 22
variants; the cap keeps future-us honest.

### Match rule at call sites

> **Match specifically what you can handle; propagate the rest with `?`.**

Don't catch `AivyxError` wholesale. Catch `CapabilityDenied` to trigger
escalation, `Timeout` to retry, `Cancelled` to clean up. Let everything
else propagate. The turn loop is the outermost matcher and converts each
to the appropriate `TurnOutcome`.

---

## Deliverable 7 — Storage Decision (LOCKED 2026-04-13)

### The stack (carried over from the archive, unchanged)

- **redb** — embedded pure-Rust ACID KV store, single file, cross-platform
- **ChaCha20-Poly1305** — at-rest encryption (AEAD, constant-time)
- **Argon2id** — passphrase → master key (m=64MB, t=3, p=4 starting params, tunable)
- **HKDF-SHA256** — domain-separated subkey derivation from master
- **Single-writer, single-process** — explicit non-goal to support otherwise

### HKDF domain list

```rust
pub enum KeyDomain {
    Sessions,      // session metadata, turn history
    Memory,        // memory.read / memory.write substrate
    Audit,         // HMAC-chained audit log entries
    Secrets,       // encrypted config values (API keys, tokens)
    ChannelState,  // per-channel persistent state (Matrix sync tokens, IMAP UIDs, etc.)
}
```

Subkey derivation:

```
subkey_{domain} = HKDF-SHA256(
    ikm    = master_key,
    salt   = "aivyx-v1-storage",   // versioned, allows clean key rotation
    info   = domain.as_bytes(),    // "sessions" | "memory" | ...
    length = 32                     // ChaCha20-Poly1305 key size
)
```

The **versioned salt** is new in the rebuild. Bumping to `"aivyx-v2-storage"`
later produces entirely different subkeys from the same master, enabling
clean migration.

### Three changes from the archive

1. **Versioned HKDF salt** (above) — enables clean key-schedule migration.

2. **Passphrase prompt lives in the channel adapter, not the storage
   layer.** In the archive, `aivyx serve` prompted directly at startup,
   mixing concerns. In the rebuild, the **local CLI channel** obtains
   the passphrase (terminal prompt, OS keyring, env var, test fixture)
   and passes the *derived master key* to the agent at construction.
   Storage never sees raw passphrases. Different channels can use
   different passphrase flows.

3. **Storage is opened once per process.** The archive had paths that
   re-opened redb per-request in the HTTP layer. The rebuild commits to
   **one open redb handle per running process**, held in the concrete
   agent impl. `StorageHandle` is `Clone + Send + Sync`, wrapping
   `Arc<Database>`.

### Storage trait (high-level sketch)

```rust
#[async_trait]
pub trait Storage: Send + Sync {
    async fn open(config: &StorageConfig, master_key: &MasterKey) -> Result<Self, StorageError>
    where
        Self: Sized;

    fn domain(&self, domain: KeyDomain) -> DomainHandle;

    async fn flush(&self) -> Result<(), StorageError>;
}

pub struct DomainHandle { /* wraps a redb table + subkey */ }

impl DomainHandle {
    pub async fn get(&self, key: &[u8]) -> Result<Option<Vec<u8>>, StorageError>;
    pub async fn put(&self, key: &[u8], value: &[u8]) -> Result<(), StorageError>;
    pub async fn delete(&self, key: &[u8]) -> Result<(), StorageError>;
    pub async fn scan(&self, prefix: &[u8]) -> Result<BoxStream<'_, Result<(Vec<u8>, Vec<u8>), StorageError>>, StorageError>;
}
```

**Commitments:**
- Single `Storage` trait + single concrete impl (`RedbStorage`) in `aivyx-storage`
- `DomainHandle` is per-domain so callers can't leak across domains
- All methods async; redb sync calls wrapped in `spawn_blocking`
- `scan` returns a stream — memory recalls iterate without loading everything

### Non-goals (explicit)

- **No multi-writer.** One process opens the store at a time. Enforced by redb file lock.
- **No distributed replication.** No sharding, no read replicas, no sync-between-devices.
- **No schema migrations framework.** Schema changes are one-off scripts + HKDF salt bump.

---

## Deliverable 8 — Repo Skeleton (LOCKED)

- [x] **Deliverable 8** — Repo skeleton (9-crate workspace, `cargo check` green)

### Workspace layout

```
~/Projects/aivyx/
├── Cargo.toml            workspace manifest (resolver = "2", edition 2024)
├── Cargo.lock            committed — this workspace produces binaries
├── rust-toolchain.toml   pinned stable + rustfmt + clippy
├── DESIGN.md             this document
├── LICENSE               MIT, © 2026 Julian (Aivyx)
├── TRADEMARK.md          MIT + branded usage rule
├── README.md             stub pointing at DESIGN.md
├── .gitignore            standard Rust ignores (Cargo.lock NOT ignored)
└── crates/
    ├── aivyx-core/       turn loop, Agent/Tool traits, TurnOutcome
    ├── aivyx-crypto/     HKDF, ChaCha20-Poly1305, Argon2id
    ├── aivyx-capability/ Scope, CapabilitySet, TrustTier
    ├── aivyx-audit/      HMAC-chained audit log
    ├── aivyx-config/     config loading, secret-field resolution
    ├── aivyx-storage/    redb-backed, KeyDomain, Storage trait
    ├── aivyx-llm/        LlmProvider trait + reference impls
    ├── aivyx-memory/     memory.{read,write,forget} tools
    ├── aivyx-channel/    ChannelContext, StreamEvent, LocalChannel
    └── aivyx-mcp/        MCP client adapter (Phase 23)
```

Every crate is a stub at this point: doc comments pointing at DESIGN.md
deliverables, `#![allow(dead_code)]`, and the minimum placeholder types
referenced by sibling crates (`Agent`, `Tool`, `Message`, `TurnOutcome`,
`ToolOutcome`, `Verification`, `Scope`, `CapabilitySet`, `TrustTier`,
`KeyDomain`). No runtime logic. No dependencies between crates yet —
each one has an empty `[dependencies]` section.

### Verification

`cargo check --workspace` compiles all 11 crates clean on rust 1.85 /
edition 2024 in ~0.03s. The skeleton is the minimum artifact that
proves the design is buildable — it is not an implementation.

> **Amendment (2026-04-17):** The workspace has grown from 9
> to 11 crates (adding `aivyx-telegram` in Phase 8 and
> `aivyx-mcp` in Phase 23), and `aivyx-channel` has expanded
> into the platform's integration
> hub with 15 modules. See amendment
> [`docs/amendments/2026-04-17-workspace-layout.md`](docs/amendments/2026-04-17-workspace-layout.md)
> for the full current layout and module map.

### Phase 1 entry criterion

Phase 1 begins from this commit. The first Phase 1 task is to wire
`aivyx-core` against `aivyx-capability` and `aivyx-channel` to produce
the first compiling (though not yet running) turn-loop skeleton. All
subsequent work happens against trait shapes already locked in
Deliverables 3–7 of this document — any deviation requires a Phase 0
amendment, not a silent drift.

Known Phase 1 refinements already flagged:
- **D3**: `Tool::required_scope` signature should become
  `required_scope(&self, input: &Value) -> Scope` to express
  input-dependent scope requirements (e.g., `memory.read` needing
  different scopes based on session filter). Caught during D4 sanity
  check against D1 scenarios.
- **D4**: `display.window_close` and related window-control scopes
  are Reserved in v1; promoting them to active is a Phase 1 tool-crate
  decision, not a Phase 0 scope taxonomy change.

---

## Status

Phase 0 is closed. This document is the **locked contract** — edits
require an amendment under `docs/amendments/`. For the Phase 0 exit
record and the lessons carried forward, see
[`docs/PHASE_0.md`](docs/archive/phases/PHASE_0.md). For the current phase and its
open tasks, see [`docs/PHASE_1.md`](docs/archive/phases/PHASE_1.md). The split
between this contract document and the per-phase journals is
explained in [`docs/README.md`](docs/README.md).
