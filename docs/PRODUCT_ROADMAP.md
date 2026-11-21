## Aivyx PA Product Roadmap

A living list of **product-shape milestones** derived from the
forward commitments in [`../PRODUCT.md`](../PRODUCT.md). This is
the product analogue of [`ROADMAP.md`](ROADMAP.md): that file
tracks technical phases (Phase 13, Phase 14, …); this file tracks
product-shape work (Daemon Migration, Role-Config Migration,
Reflection Layer, …) that may span multiple phases or fold into
one.

> **2.0 (local-only):** entries below that mention Anthropic, OpenAI or a
> cloud `base_url` record 1.x history. Since 2.0 every model call goes to a
> local server (Amendment A23).

**This is not a contract.** Milestone shapes are revised whenever
a phase exit teaches us something new. Sequencing is loose — if
the Reflection milestone turns out to need the Role-Config
milestone first, we reorder here and it costs one commit, not an
amendment. The locked product contract is `../PRODUCT.md`; this
document only answers *"what comes after the current phase, in
product terms?"*.

For the locked product contract, see [`../PRODUCT.md`](../PRODUCT.md).
For the locked design contract, see [`../DESIGN.md`](../DESIGN.md).
For numbered technical phases, see [`ROADMAP.md`](ROADMAP.md).

## How this document is maintained

- **One paragraph per milestone, maximum.** If a milestone grows
  task lists or open questions, it has outgrown the roadmap and
  belongs in its own `PHASE_N.md` — which means that milestone
  is probably ready to open as a phase (or a sequence of phases).
- **Milestones are named, not numbered.** A milestone may map to
  one phase, several phases, or share a phase with another
  milestone. The `ROADMAP.md` numbering is the source of truth
  for sequence; this document is the source of truth for
  *product-shape intent*.
- **At every phase exit**, any milestone that the exiting phase
  advanced is updated here with whatever was learned, and any
  milestone that landed entirely is moved to the "Delivered"
  section at the bottom (with a pointer to the phase that
  shipped it).
- **At every phase entry**, the relevant milestone entries here
  inform the new `PHASE_N.md` scaffold but are not copied into
  it — the phase doc carries the task list, the milestone entry
  carries the intent.

## Milestone — Daemon Migration (architecturally complete)

**Forward commitment:** [`PRODUCT.md` P4](../PRODUCT.md). **Keystone:** unlocks
P5, P12, P2, P1. **Status:** Architecturally complete after
five dedicated phases (16–20). Remaining daemon work is
incremental (crash recovery, in-flight turn replay) rather
than architectural.

The single biggest reshape on the product roadmap. Today's binary
runs as a one-shot per-channel process; the daemon milestone moves
state ownership (redb, audit chain, role registry, capability
ceiling) into a long-lived background process and reshapes every
existing channel adapter into a thin frontend that auto-attaches
to the running daemon over a local IPC socket. The migration spans
multiple dedicated phases. The shape of the IPC protocol is the
load-bearing design decision this milestone had to settle first,
because P5 (channel SDK) and P12 (tool IPC) both consume it.

**Phase 16 (Protocol Settlement, 2026-04-16):** settled the IPC
protocol shape — length-prefixed JSON frames over Unix domain
sockets, `DaemonLifecycleEvent` as a separate message type from
`StreamEvent`, OS-user auth per P4.4. Landed a PoC daemon server +
client + one round-trip integration test proving the protocol
carries one turn end-to-end. All byte-identity streaks held; six
architectural questions resolved with streak-preserving options.
See [`docs/PHASE_16.md`](archive/phases/PHASE_16.md).

**Phase 17 (Production Hardening, 2026-04-16):** converted
the PoC into a production-ready daemon substrate: multi-turn
session server with graceful shutdown via `CancellationToken`,
`daemon run` subcommand with full agent-stack wiring (CliMode
enum refactor), and a multi-turn client library (`DaemonSession`)
with `spawn_daemon_and_wait` auto-spawn logic. Closed 3 of 5
Phase 16 net-new deferrals. All byte-identity streaks held;
production-core streak at six consecutive phases (longest in
project history). See [`docs/PHASE_17.md`](archive/phases/PHASE_17.md).

**Phase 18 (Frontend Wiring, 2026-04-16):** wired the default
`aivyx-pa` invocation to auto-spawn a daemon, connect via
`DaemonSession`, and run a REPL loop rendering `StreamEventPayload`s
via `render_for_cli()`. Daemon-first dispatch with in-process
fallback. `DaemonCancelHandle` for ctrl-C cancellation over IPC
(first ctrl-C sends `CancelTurn`, second exits). Discovered and
fixed a cancel-flag reset bug between turns. All byte-identity
streaks held; production-core at seven consecutive phases (longest
in project history). Closed the REPL-mode-over-IPC deferral.
See [`docs/PHASE_18.md`](archive/phases/PHASE_18.md).

**Phase 19 (Multi-Connection + Telegram Port, 2026-04-16):**
upgraded the daemon from single-connection to multi-connection
(task-per-connection with `ChannelFactory` dispatching on
`FrontendType`). Ported the Telegram adapter behind the IPC
boundary — `aivyx-pa --channel telegram` now auto-attaches to the
daemon the same way the local CLI does. Transport types widened
to `pub` for binary access; binary line-count managed via
extraction to `telegram_daemon_frontend.rs`. All byte-identity
streaks held; production-core at eight consecutive phases.
Closed the Telegram-over-daemon deferral from Phase 16.
See [`docs/PHASE_19.md`](archive/phases/PHASE_19.md).

**Phase 20 (Daemon Management + Deferral Cleanup, 2026-04-16):**
non-product-shape cleanup phase closing six daemon-management
and capability-system deferrals accumulated during the migration:
`daemon status`/`stop` subcommands with `FrontendMessage::Shutdown`
IPC, PID file with `Drop` guard, `--no-daemon` flag for in-process-
only mode, daemon-mode banner parity (no IPC change needed —
metadata already in scope on the frontend side), `CapabilitySet::
grants` reflexivity investigation (reflexive for all practical
scopes), and `CEILING_SEMITRUSTED` doc-comment rewrite. Rolling
backlog 16 → 10. All byte-identity streaks held; production-core
at nine consecutive phases (new record). See
[`docs/PHASE_20.md`](archive/phases/PHASE_20.md).

**Phase 21 (Mission Primitive, 2026-04-17):** delivered the first
concrete piece of **P2 — Mission Primitive** as a product-shape
phase. Mission state model with six-state machine (`Created →
Running → GatePending → Completed | Failed | Cancelled`) persisted
to redb under `KeyDomain::Missions`. Two new capability bases
(`mission.create`, `mission.gate`) with tier ceilings. IPC protocol
extended with `ApprovalGate`, `ResolveGate`, `MissionCreated`,
`MissionStateChanged`, `GateResolved`. `MissionCreateTool` with
OnceLock factory pattern (preserving production-core streak).
Daemon `ResolveGate` handler wired end-to-end. CLI interactive
gate prompt (`Approve? [y/N]:`) and Telegram `/approve`/`/reject`
text commands. Five design decisions, five Q-block questions
resolved. All byte-identity streaks held; production-core at ten
consecutive phases (new record). Test delta +29 (569→598). See
[`docs/PHASE_21.md`](archive/phases/PHASE_21.md).

**Next:** Escalation→gate turn-loop wiring (daemon-side
orchestration between `TurnOutcome::Escalated` and
`mission::add_gate`), completing the full approval-gate lifecycle.
All primitives in place; the missing piece is the daemon's
turn-boundary gate creation logic.

## Milestone — Role-Config Migration (shipped in Phase 13)

**Forward commitment:** [`PRODUCT.md` P9](../PRODUCT.md). **Keystone:** unlocks
P1 (sub-agents), P2 (missions).

Phase 11 introduced the role primitive but left every capability
grant inside `crates/aivyx-channel/src/bin/aivyx.rs` at the
operator level. This milestone migrated those grants out of the
binary and into a per-role config file that declares **the
complete capability envelope** of each role: tool allowlist,
scope set with qualifiers, trust ceiling, memory topic prefix,
and the parent role it inherits from per **P7**.

**Landed shape (Phase 13, 2026-04-15):** per-role
`[[role]]` table-array entries in a single TOML file (not
one file per role), with `capability_scopes` parsed
directly via `Scope::parse` at config-load time and
`parent_role` as explicit single-inheritance (no implicit
`default` parenting). Attenuation is enforced against
declared sets only, walking up through empty ancestors;
an empty child's envelope triggers a one-step-deep
backcompat-floor substitution at runtime. A
worked-example `examples/aivyx-pa.toml` demonstrates four
roles (`default`, `coder`, `researcher`,
`junior_researcher`) including the deliberate empty-
child surprise case, and a `--print-role <name>` debug
flag lets operators inspect the effective envelope of
any role without side effects. See
[`docs/PHASE_13.md`](archive/phases/PHASE_13.md) for the full phase
record.

Once the binary no longer hard-codes role bodies, two derived
capabilities become mechanical to add: a primary agent can
switch into a child role mid-session under an attenuated
envelope (the substrate **P1** needs), and a long-running
mission can be tied to a specific role's envelope independent
of which channel kicked it off (the substrate **P2** needs).
Both are now unblocked. Two non-blocking follow-ups are
recorded in Phase 13's deferrals (lift
`assemble_role_envelope` from the binary into
`aivyx-channel/src/lib.rs` for cross-crate integration
tests; ship per-tier worked examples for SemiTrusted and
Untrusted channels) — neither requires a dedicated sub-
phase and both can be picked up reactively whenever a
future phase needs them.

## Milestone — Mission Primitive ✓

**Delivered across Phases 21, 23, 28, 35.** **Forward commitment:**
[`PRODUCT.md` P2](../PRODUCT.md).

Phase 21 introduced the mission state machine (six states:
`Created → Running → GatePending → Completed | Failed |
Cancelled`) with HMAC-bounded persistence under
`KeyDomain::Missions`, two new capability bases
(`mission.create`, `mission.gate`), IPC protocol extensions
(`ApprovalGate`, `ResolveGate`, `MissionCreated`,
`MissionStateChanged`, `GateResolved`), `MissionCreateTool` with
the `OnceLock` factory pattern, daemon `ResolveGate` handler,
CLI interactive gate prompt, and Telegram `/approve` / `/reject`
text commands. Phase 23 added the escalation → gate turn-loop
wiring. Phase 28 added `mission.list` / `mission.status`
read-only inspection tools. Phase 35 wired
`ToolOutcome::RequiresEscalation` through the turn loop to
`TurnOutcome::Escalated` for the trigger path. P2 fully
delivered.

## Milestone — Sub-Agent Role-Switching (shipped in Phase 14)

**Forward commitment:** [`PRODUCT.md` P1](../PRODUCT.md). **Couples to:**
Role-Config Migration.

Once the Role-Config Migration lands, sub-agents become a
relatively small additional primitive: a turn-loop instruction
that switches the active role from parent to child for the
remainder of a sub-task and switches back when the sub-task
completes. The capability envelope of the child role is
mechanically the intersection of the parent's envelope and
whatever the child role's config declares — the type system
enforces attenuation per **P7**, so escalation is structurally
impossible. The milestone is expected to be small (one phase,
possibly shared) but it has to land *after* role-config
migration and *before* the mission primitive, because a
mission's approval gate may want to spawn a constrained
sub-task as part of how it checkpoints progress.

**Landed shape (Phase 14, 2026-04-16):** a `role.switch`
capability scope with a target-role `QualifierKind` in
`aivyx-capability` (parses as either bare `role.switch` or
`role.switch:<target>`; rejected `role.switch:*` because the
unqualified form is already the wildcard), a `RoleSwitchTool`
registered in the core tool registry, and an inline sub-
session dispatcher implemented as an `OnceLock`-backed factory
closure on the tool. The factory is constructed in
`crates/aivyx-channel/src/bin/aivyx.rs`'s startup path and
captures the provider, audit hook, tool registry, role table,
backcompat floor, and model — *without* extending
`ToolContext`'s shape, which keeps `aivyx-core/src/lib.rs`
byte-identical and the production-core streak intact at three
phases. The factory closure has exactly one `CapabilitySet`-
producing call: `assemble_role_envelope(&target_role, &roles,
&backcompat_floor)`. There is no second route. **P1.3
"structural impossibility of escalation" is a documentation
property pinned by integration tests** rather than a
type-system property in the strict sense — the documentation
property is stronger in practice because the call site is
auditable and the tests verify the invariant directly. The
sub-session is one level deep (the child cannot itself invoke
`role.switch` unless its own role declares the scope, which
no role in `examples/aivyx-pa.toml` does); multi-level nesting
is the single net-new Phase 14 deferral, with no urgency
because the no-op-by-default failure mode is already correct.
**P1.4 "each turn tagged by role active at turn-start" is
satisfied** through distinct `TurnId` values across the
parent and child `TurnStarted` audit events — no dedicated
audit tag was added (the `aivyx-audit` chain walker can
reconstruct the boundary from the role-name transition). The
clean-slate child conversation history (Q2) and per-role
memory-topic prefix (Q3) were both pinned at implementation
time. The `--print-role` debug flag gained a mechanical
"reachable role.switch targets" enumerator that reads from
the same `assemble_role_envelope`-produced `CapabilitySet`
the production dispatcher reads from, so the operator's
debug-time view of sub-session reachability is guaranteed to
agree with the runtime's dispatch-time view by construction.
See [`docs/PHASE_14.md`](archive/phases/PHASE_14.md) for the full phase
record.

The Mission Primitive is now the next keystone that couples
to this milestone — its approval-gate's "spawn a constrained
sub-task" pattern can compose directly against `role.switch`
without needing additional primitive work. The one remaining
sub-phase candidate for P1 itself is multi-level nesting,
which is recorded as a Phase 14 deferral and gated on a
concrete recursive-role-switching use case rather than a
forward-commitment requirement.

## Milestone — Reflection Layer ✓ (shipped across Phases 28–30)

**Forward commitments:** [`PRODUCT.md` P8](../PRODUCT.md), [`PRODUCT.md` G3](../PRODUCT.md).
**Couples to:** Outcome history exposure, runtime role mutation.

The product contract's commitment to *outcome-driven audited
reflection* is the most ambitious forward item: the agent
observes its own success/failure outcomes via the existing audit
chain and uses that history to evolve its memory and (within
guardrails) its own role config. The milestone has two halves
that may sequence as separate phases. The first half is purely
substrate: expose the audit chain as an introspection surface
the agent can read from (today it's only readable by
`--verify-only`). The second half is the reflection loop itself:
a periodic or operator-triggered pass where the agent reads
recent outcomes, proposes memory edits and role-config edits,
and surfaces them through an approval gate (likely the same
approval primitive the Mission Milestone introduces). The
reflection loop must itself be auditable — every proposed
edit, every approval/rejection, every applied change is an
audit event under the operator's chain.

**Phase 21 note (2026-04-17):** The mission gate primitive
now provides the approval-gate substrate the reflection loop's
second half needs. Gate rendering (CLI prompt, Telegram
`/approve`/`/reject`) is operational. The escalation→gate
turn-loop wiring (Phase 22 Task 8) will complete the daemon-
side orchestration, at which point the reflection loop can
compose directly against the existing gate machinery for
surfacing proposed self-modifications to the operator.

**Phase 28 (Reflection Foundation, 2026-04-18):** frozen.
First Reflection Layer primitive — `turn.history` audit
introspection tool giving the agent read access to its own
recent turn outcomes. Combined with deferral cleanup
(`mission.list`/`mission.status` tools, webhook port config,
forensic `ToolOutcome::NotInRole`). See
[`docs/PHASE_28.md`](archive/phases/PHASE_28.md).

**Phase 29 (Agent Reflection Loop, 2026-04-18):** frozen.
Completes the Reflection Layer — `reflection.propose` and
`reflection.apply` tools for the full observe-propose-approve-apply
cycle. Memory-only scope for this phase; runtime role-config
mutation deferred. See [`docs/PHASE_29.md`](archive/phases/PHASE_29.md).

**Phase 30 (Runtime Role Mutation, 2026-04-18):** frozen.
Completes P8 — `RoleOverrides` struct with prompt appendix and
allowlist add/remove, `role.update` tool and capability base,
planner factory integration reading overrides per-turn, and
extended `reflection.apply` for role mutations. Full
observe-propose-approve-apply cycle now operational for both
memory writes and runtime role-config changes. See
[`docs/PHASE_30.md`](archive/phases/PHASE_30.md).

**Status: G3 / P8 — Reflection Layer is now fully delivered**
across Phases 28–30.

## Milestone — Channel SDK Surface ✓ (shipped in Phase 48)

**Forward commitment:** [`PRODUCT.md` P5](../PRODUCT.md). **Couples to:**
Daemon Migration.

The product contract commits to an open first-party SDK for
building channel adapters. Today the in-tree adapters
(`LocalChannel`, `aivyx-telegram`) directly depend on
`aivyx-core` and re-implement `ChannelContext` from scratch each
time. This milestone extracts the adapter surface into a
documented, versioned, third-party-consumable form — almost
certainly as a separate crate plus a written contract document
in `docs/`. The shape of the surface is largely fixed by what
the daemon IPC protocol turns out to look like, so the milestone
is expected to land *immediately after* the Daemon Migration's
first phase and may share a phase with it if the daemon work
turns out to be lighter than expected.

## Milestone — Tool Process IPC ✓ (shipped in Phases 49–50, hardened in 52)

**Forward commitment:** [`PRODUCT.md` P12](../PRODUCT.md). **Couples to:**
Daemon Migration, Channel SDK Surface.

The product contract commits to running tools as separate
processes that speak an IPC protocol matching the channel
adapter protocol. Today's tools are in-process trait
implementations. This milestone introduces the tool-process
IPC contract — likely the same wire format as the channel SDK,
since both are "external code that talks to the daemon under a
declared capability envelope." The substrate tools (the seven
in **P10**) move into the daemon process itself or into a
trusted in-process registry; the third-party tools become
separate processes the daemon spawns under a specific role's
envelope. The load-bearing decision is whether to fold the
tool IPC into the channel SDK protocol (one wire format, two
client roles) or to keep them separate (two protocols, easier
to evolve independently). Deferred to milestone open.

## Milestone — SDK Documentation Surface ✓ (shipped in Phases 48–49)

**Forward commitment:** [`PRODUCT.md` P11](../PRODUCT.md).

A docs-and-examples phase after the in-tree SDK has stabilized.
The product contract commits to publishing the SDK as a
versioned interface with worked examples and an integration
test surface a third-party developer can run against their own
adapter. This milestone is the lightest on the roadmap: it
ships no new substrate and revises no architecture, it just
takes the in-tree adapter and tool surfaces (after the Channel
SDK and Tool Process IPC milestones have landed) and dresses
them as a publishable contract. Expected to be one phase, late
in the sequence.

## Milestone — MCP Integration ✓

**Forward commitment candidate:** not yet locked (requires
product-shape review). **Couples to:** P11 (SDK Contract),
P12 (Tool Process IPC), Daemon Migration.

The `Tool` trait's shape (`name`, `description`,
`input_schema`, `required_scope`, `execute`) maps near-1:1
to MCP's tool interface. This milestone adds an MCP client
adapter that bridges external MCP servers into Aivyx PA's tool
registry. Each MCP tool gets a declared scope in the
capability system, audit logging as a standard tool call,
and role allowlisting through the existing config surface.
The load-bearing design decision — bridge category vs P12
process model — was resolved in Phase 23 in favor of bridge
(in-process `McpToolProxy` delegating over stdio JSON-RPC).

**Phase 23 (MCP Foundation, 2026-04-17):** new `aivyx-mcp`
crate (11th workspace member), stdio transport,
`McpServerBridge` + `McpToolProxy`, `mcp.call` scope base
in `aivyx-capability`. 8 tests. Programmatic API only.

**Phase 24 (Config + Binary Wiring, 2026-04-17):**
`[[mcp_server]]` TOML config entries, daemon-side bridge
lifecycle (eager startup, tool registration, `kill_on_drop`
shutdown), workspace layout amendment addendum, `--mcp-server`
CLI flag. Closed the MCP config surface deferral. 9 tests.

**Phase 32 (MCP SSE Transport, 2026-04-19):**
`McpTransport` trait abstraction, `StdioTransport` extraction,
`SseTransport` over HTTP+SSE with background event reader,
`McpTransportKind` config enum, `--mcp-sse` CLI flag. Closes the
Phase 23 SSE deferral. 23 new tests (9 parser, 8 config/CLI,
6 integration). Milestone fully delivered.

## Milestone — Multi-Provider Support ✓

**Delivered in Phase 25** (2026-04-17). **Coupled to:**
`LlmProvider` trait (Phase 1).

The `LlmProvider` trait was already provider-agnostic. Phase 25
added an OpenAI-compatible adapter covering GPT-4, Ollama, and
any OpenAI-API-compatible endpoint. Capability differences
between providers (tool-calling wire format) are handled
internally by the adapter — no core type changes were needed.
Delivered in 1 phase as predicted.

## Milestone — Web UI Channel ✓ (shipped across Phases 39, 47)

**Forward commitment candidate:** not yet locked. **Couples
to:** P4 (Daemon), P5 (Channel SDK).

A `127.0.0.1`-only web interface that connects to the daemon
over the existing IPC protocol. The daemon architecture makes
this cheap — the frontend is a thin client rendering
`StreamEventPayload` events. First phase: minimal chat UI
with the same rendering as the CLI REPL. Second phase: mission
management, gate resolution buttons, audit inspection surface.
The `FrontendType` enum already has the extension point
(`FrontendType::Web`).

**Phase 39 (Chat Interface, 2026-04-20):** delivered Phase 1
of the Web UI Channel. `FrontendType::Web` variant,
`WebDaemonChannel` stub, `tokio-tungstenite` WebSocket bridge
(TCP peek routing, no hyper in WS path), embedded HTML/CSS/JS
frontend with streaming text, collapsible tool-call cards,
approval-gate Approve/Deny buttons, cancel button.
`--web-ui` CLI flag and `[daemon] web_ui` / `web_ui_port`
TOML config. 801 tests (+13), zero clippy warnings. All three
byte-level streaks intact (DESIGN.md 16, PRODUCT.md 2, lib.rs 3).

## Milestone — Scheduled Execution ✓ (shipped across Phases 26–27)

**Forward commitment:** [`PRODUCT.md` G5](../PRODUCT.md).
**Couples to:** Daemon Migration, Mission Primitive.

G5 commits to autonomous and scheduled execution. The daemon
substrate exists and missions survive restarts. This milestone
adds cron-like timer primitives, webhook trigger endpoints
(localhost-only per P6), and file-change watchers. Each
trigger creates a daemon turn attributed to the operator's
identity.

**Phase 26 (Timer Primitives, 2026-04-17):** delivered cron
scheduler — `KeyDomain::Schedules`, `ScheduleRecord` CRUD,
`[[schedule]]` TOML config, daemon scheduler loop with
adaptive-tick and dedup, four agent tools (`schedule.create`,
`.list`, `.delete`, `.update`) gated to `CEILING_TRUSTED`.
660 tests. All byte-identity streaks held. See
[`docs/PHASE_26.md`](archive/phases/PHASE_26.md).

**Phase 27 (Webhook Triggers + File Watchers, 2026-04-18):**
completed G5. Webhook HTTP listener (localhost-only, hyper,
127.0.0.1:7842), file-change watcher (`notify` crate,
cross-platform, per-watch debounce), `TriggerDispatch`
unification with shared turn-lock, opt-in `wrap_mission`
on all trigger configs closing the Phase 26 deferral.
Nine storage domains. +30 tests (660→690). See
[`docs/PHASE_27.md`](archive/phases/PHASE_27.md).

**Status: G5 — Autonomous and Scheduled Execution is now
fully delivered** across Phases 26–27.

## Milestone — Assistant Profile ✓ (delivered across Phases 57–58)

**Forward commitment:** PRODUCT.md P13 (amendment A9, Phase 56).
**Couples to:** Per-Role Envelope (P9), `aivyx-pa init` wizard,
system-prompt assembly.
**Status:** Fully delivered. P13 closed at Phase 58 exit
(2026-05-12).

The operator's stated vision (post-Phase-55) reframes Aivyx PA
from "personal autonomous agent platform" into a
*self-learning, self-improving AI-personal assistant with a
user-defined **Profile** and **Persona** based on the
end-user use-case*. The **Profile** is the operator-declared,
mostly-static layer: who this specific assistant is for,
what use cases it serves, how the operator wants it to
communicate, and the high-level constraints that flavor every
turn. It is distinct from the *role envelope* (which gates
capabilities) — Profile shapes the assistant's *identity*
where roles shape its *authority*.

The expected fields (subject to Phase 57 design):

- `name` — what the operator calls this assistant
- `operator_profile` — short paragraph about the operator
  (role, expertise, primary work context). Drives
  domain-specific language and assumed background knowledge
- `communication_style` — terse / detailed, formal /
  casual, with-source-citations / synthesized, etc.
- `primary_use_cases` — the 1–3 use-case archetypes the
  assistant is being shaped around (e.g. "Rust systems
  programming," "personal-finance analysis," "research
  synthesis")
- `preferences` — non-capability behavioral preferences
  ("prefer integration tests over mocks", "always cite
  sources when summarizing")
- `constraints` — non-capability behavioral constraints
  ("never autonomously commit code", "always confirm
  destructive shell commands")

**Substrate coupling.** Profile injects into the system
prompt at turn start (assembled alongside the role-derived
envelope description). The per-role inheritance tree from
P9 is unchanged — Profile is a peer concept that flavors
the assistant's voice and judgment across every role.
`aivyx-pa init` extends with use-case prompts that populate
Profile fields.

**Expected phases:**

- **Phase 57 (Foundation, shipped 2026-05-12):** delivered
  the Profile substrate. `aivyx-config::Profile` struct
  with six P13-commit-5 fields; `[profile]` TOML table
  parsed at config-load time per Q1(a); `Profile::default()`
  synthesizing the Q5(b) `assistant_name = "Aivyx PA"` fallback
  for legacy configs; `aivyx-channel::assemble_session_prompt`
  helper composing Profile + role envelope into a labeled
  system prompt per Q3(c); wiring through both the parent
  session-build path and the role-switch child factory so
  sub-sessions inherit the same Profile section; `aivyx-pa
  init` extended with three opt-in Profile prompts per
  Q4(c) (assistant name, primary use case, communication
  style); startup-banner row surfacing Profile provenance
  to the operator. Tests +14 across aivyx-config and
  aivyx-channel (992 → 1006). All three streak predictions
  correct: DESIGN.md → 4, PRODUCT.md → 1, lib.rs → 6
  (longest run since Phase 51's deliberate break at 6).
- **Phase 58 (Inspection, shipped 2026-05-12):** delivered
  the operator-facing CLI + Web UI surface and closed the
  P13 milestone. `aivyx-pa profile show` reads `aivyx-pa.toml`
  via the existing config loader path and renders the
  resolved Profile in labeled banner-style format per
  Q3(a). `aivyx-pa profile edit` opens the `[profile]`
  section in `$EDITOR` against a tempfile and merges the
  result back via `toml_edit` surgical update per Q2(a)
  — preserves comments, whitespace, and every other
  section in `aivyx-pa.toml`. New `CliMode::Profile(ProfileSubcommand)`
  nested enum per Q1(a). Web UI Profile pane shows live
  daemon state via a new `Query::GetProfile` IPC envelope
  + `ProfileSummary` wire-shape per Q4(a) read-only.
  Reload semantics: load-time-only per Q5(a) — edit
  prints a `aivyx-pa daemon stop && aivyx-pa` restart reminder
  on save. PRODUCT.md Delivery Status refreshed: P13 →
  Fully Delivered (Task 5 streak-breaker). `toml_edit =
  "0.22"` is the first new workspace crate added since
  Phase 27's `notify`. Tests +17 across aivyx-channel
  bin (parser + render + merge) and lib (IPC handler
  conversion), 1006 → 1023. All three streak predictions
  correct: DESIGN.md → 5, PRODUCT.md → broke at 2
  (Task 5, intentional), lib.rs → 7.

## Milestone — Persona ✓ (delivered across Phases 59–60)

**Forward commitment:** PRODUCT.md P14 (amendment A10, Phase 56).
**Couples to:** Profile (P13), Reflection Layer (P8), Memory
persistence (G3).
**Status:** Fully delivered. P14 closed at Phase 60 exit
(2026-05-12). After Phase 60, **P1–P14 are all fully shipped**;
the PRODUCT.md forward-commitment ledger closes.

**Persona** (operator's framing: "Soul") is the dynamic
counterpart to Profile — the evolving character-layer that
emerges from accumulated reflection-approved deltas over the
assistant's lifetime. Where Profile is operator-declared and
mostly-static, Persona is *reflection-written* and
*operator-gated*: the assistant proposes refinements to its
own learned voice via `reflection.propose`, the operator
approves them through the existing mission-gate machinery,
and the deltas accumulate into the Persona surface that
flavors future turns.

**Why "Persona" not "Soul" in contract docs.** The operator's
vision uses "Soul" as the evocative term; contract docs and
code use "Persona" for the same concept. Both refer to the
same evolving identity layer — Persona is the
contract-document spelling.

The expected fields (subject to Phase 59 design):

- `seed_pointer` — Profile name this Persona is grown from
  (Profile is the static seed; Persona is what it becomes)
- `learned_context` — accumulated facts about the operator
  and their domain that the assistant has internalized
  (proposed by reflection, gated by operator)
- `communication_adaptations` — refinements to
  Profile.communication_style learned over time (e.g.
  "operator prefers conclusion-first paragraphs", "operator
  finds three-bullet lists optimal")
- `character_traits` — emergent voice properties (e.g.
  "leans toward conservative recommendations on
  irreversible operations", "preempts ambiguity with
  clarifying questions")
- `relationship_milestones` — operator-significant events
  the assistant references for continuity (the human
  equivalent of "remember when we…")
- `delta_log` — the append-only HMAC-chained history of
  every approved Persona delta (audit-verifiable like the
  rest of the chain)

**Substrate coupling.** Persona delta proposals extend
`reflection.propose` with a new delta category; gate
threading reuses the Phase 21 / 28–30 machinery; the
effective-identity assembly at turn start composes
Profile + Persona deltas into the system-prompt voice
layer.

**Expected phases:**

- **Phase 59 (Foundation, shipped 2026-05-12):** delivered
  the Persona substrate per Q1–Q6 at sign-off. New
  `KeyDomain::Persona` (10th storage domain) parallel to
  `KeyDomain::Audit`; `PersonaDelta` struct with 10
  `PersonaDeltaCategory` variants (6 Profile-mirror +
  4 Persona-specific) and `PersonaDeltaOp` (SetScalar /
  AppendList / RemoveList); `PersonaChainLog` HMAC-SHA256
  chain primitive with a distinct genesis seed (chain-
  confusion attacks structurally rejected);
  `PersistentPersonaLog` storage wrapper (one redb row per
  signed entry, keyed by big-endian seq);
  `EffectivePersona` replay state +
  `compute_effective_persona` pure folder;
  `SharedEffectivePersona = Arc<RwLock<EffectivePersona>>`
  with `apply_delta_to_shared` helper; new
  `persona.propose` capability scope (KNOWN_BASES + 
  CEILING_TRUSTED); `reflection.propose` schema extended
  with `persona_deltas` array and `required_scope`
  escalates to `persona.propose` when the array is
  non-empty; `reflection.apply` writes approved deltas
  to the chain AND mutates the shared runtime state;
  `assemble_session_prompt` extended with an
  `Option<&EffectivePersona>` parameter and a new
  "## How I have learned to communicate" labeled
  section; binary opens the chain at startup, replays
  into shared state, registers on the apply tool, and
  composes the system prompt from Profile + Persona +
  Role for both parent and role-switch child sessions.
  Tests +29 (1023 → 1052 across the phase). All three
  streak predictions correct: DESIGN.md → 6, PRODUCT.md
  → 1, lib.rs → 7. **Per-turn freshness deferred to
  Phase 60** — Phase 59 ships snapshot-at-session-build;
  the planner-factory per-turn re-call lands alongside
  the Web UI / revert / CLI surfaces.
- **Phase 60 (Visualization, shipped 2026-05-12):** closes
  the milestone and the entire forward-commitment ledger.
  `PersonaDeltaOp::Revert { target_delta_id }` variant +
  inverse-apply folder for revert semantics including
  revert-of-revert (P14 commit 4); per-turn planner-factory
  refresh (closes Phase 59 Q5(a) deferral — approved deltas
  take effect on next turn without restart);
  `Query::GetEffectivePersona` + `Query::ListPersonaDeltas`
  IPC envelopes + `FrontendMessage::RevertPersonaDelta`;
  `aivyx-pa persona show / list / revert` CLI subcommands
  (daemon-IPC-backed per Q3(a) at sign-off); Web UI Persona
  pane with effective-state rendering and click-to-revert.
  Reverts are operator-only per Q5(a), auto-approved (the
  operator is the proposer). Per Q4(a), revert appends a
  delta to the append-only chain rather than mutating in
  place. Identity export/import deferred to a future
  micro-phase if pressure surfaces — **fully closed:** export
  shipped in Phase 64 (2026-05-14), import shipped in Phase
  65 (2026-05-14). Operators can now transfer Profile +
  Persona between hosts via `aivyx-pa identity export <path>` +
  `aivyx-pa identity import <path> [--force]`. Tests +19
  (1052 → 1071).
  All three streak predictions correct: DESIGN.md → 7,
  PRODUCT.md → broke at 2 (Task 7 Delivery Status refresh,
  intentional), lib.rs → 8. After Phase 60, **P1–P14 are
  all fully shipped**. The PRODUCT.md forward-commitment
  ledger closes here.

- **Phase 71 (Reflection scheduler loop, shipped 2026-05-15).**
  Closes the cron-auto-firing deferral carried at Phase 70
  exit. New `reflection_scheduler.rs` module in
  `aivyx-channel` owns a `run_reflection_scheduler` async
  loop spawned alongside the existing scheduler / webhook /
  file-watch tasks. On each cron boundary it walks the audit
  chain for `TurnStarted`/`TurnEnded` pairs in the lookback
  window (bounded LRU cache per Q1(c)), formats a canonical
  reflection prompt + outcome summary block, and calls
  `TriggerDispatch::fire(TriggerSource::Reflection, ...)`.
  Hardcoded `REFLECTION_SYSTEM_PROMPT` const per Q2(a) with
  conservative behavioral framing (propose only on ≥3-turn
  pattern recurrence; prefer narrow categories; empty
  reflection is valid). Errors log + audit + skip per Q4(a);
  no in-window retry. After Phase 71 the self-learning half
  of P14 is genuinely autonomous: at the configured cadence
  the agent reflects on its own behavior without operator
  prompting, and proposed deltas land in the Phase 70 review
  pane. Tests +14 (1275 → 1289). All three streak predictions
  correct: DESIGN.md → 18, PRODUCT.md → 11, lib.rs → 19 (new
  project record). Zero new workspace deps. **Scope note:**
  Q3(a) role_override is recorded for forensic attribution
  but not runtime-honored in v1 — per-fire role swap is a
  deferred polish.

- **Phase 70 (Reflection auto-loop, shipped 2026-05-15).**
  Closes the **self-learning half** of P14. PRODUCT.md:1133
  always promised "Reflection is the engine that proposes
  Persona deltas"; Phase 60 delivered the operator-edited
  half (revert, visualization) and Phase 29 shipped the
  reflection tooling substrate, but until Phase 70 the agent's
  persona-delta proposals flowed only through a synchronous
  mission-gate that needed the operator present at proposal
  time. Phase 70 makes the review loop asynchronous: agent
  calls `reflection.propose` → Pending rows in a new
  encrypted proposal chain (KeyDomain::PersonaProposals,
  distinct genesis seed from the persona chain per Q4(a)) →
  operator reviews at leisure via the new Web UI Proposals
  pane or `aivyx-pa persona proposals` CLI → approve verbatim,
  approve-with-edit (Q3(a) — operator tweaks the op before
  applying), or reject with optional reason → daemon
  validates, appends a PersonaDelta to the persona chain on
  approve, and recomputes shared state. Both `proposed_op`
  and `applied_op` survive in the proposal chain for audit.
  Tests +41 (1234 → 1275). All three streak predictions
  correct: DESIGN.md → 17, PRODUCT.md → 10, lib.rs → 18 (new
  project record, beats Phase 69's 17). Zero new workspace
  deps. **Scope note:** cron-fired auto-reflection (a
  `[[reflection_schedule]]` scheduler loop) parses + validates
  end-to-end but the loop-firing wire-up is deferred to a
  follow-up phase; operators today fire reflection turns via
  the existing `[[schedule]]` substrate and proposals land
  in the same chain.

## Milestone — Distribution (in progress, Pipeline Ready)

**Forward commitment:** none — operator-feedback-shaped
substrate-ergonomics work post-ledger-closure.
**Couples to:** `aivyx-pa init` (Phase 44 wizard), the operator
identity layer (P13 + P14).
**Status:** Phase 1 of N delivered as "Pipeline Ready,
Publication Held." First published release pending public
hosting; future micro-phases extend reach.

After Phase 60 closed the forward-commitment ledger, the
codebase review surfaced **distribution** as the largest
adoption-shape gap: end users had to run `cargo run --release
--bin aivyx-pa` from source because there were no prebuilt
binaries on any platform. This milestone closes that gap
incrementally — release pipeline first, then publication,
then platform expansion (Windows, Homebrew, Docker, signing),
then the long-tail polish (signed third-party tool
registries, automated update channels). Each item lands as a
focused phase rather than one monolithic distribution effort.

**Expected sub-phases / micro-phases:**

- **Phase 61 (Release Pipeline, shipped 2026-05-13 as
  "Pipeline Ready").** First phase of the milestone. Wired
  the release substrate: `aivyx-pa --version` flag, cargo-dist
  config (`dist-workspace.toml`), four-target matrix (Linux
  x86_64/aarch64 musl + macOS x86_64/aarch64), three
  workflow files (`ci.yml`, `quality-gate.yml`, dist's
  `release.yml`), and the `plan-jobs = ["./quality-gate"]`
  wiring that gates every release-pipeline artifact behind
  `cargo clippy --workspace --all-targets -- -D warnings` +
  `cargo test --workspace`. README's "Five-minute setup"
  refresh prepared the install-script framing; mid-phase
  the operator pivoted to a VPS-private-first posture and
  the Task 5/6 fixup reframed the docs to honest
  "build-from-source primary, shell-installer when
  published." Tests +3 (1071 → 1074). All three streak
  predictions correct: DESIGN.md → 8, PRODUCT.md → 1,
  lib.rs → 9 (new record).
- **v0.1.0 publication (deferred to future micro-phase).**
  Operator-side: create public GitHub repo, push history,
  tag `v0.1.0`. The pipeline fires automatically and
  publishes prebuilt binaries + shell installer. Re-opens
  when public hosting goes live.
- **Native Windows port (future).** Daemon IPC NamedPipe
  replacement for the Unix-socket-only substrate;
  platform-conditional spawn for `shell.exec`; parallel CI
  matrix. Most architecturally heavy item on the milestone.
- **Package manager presence (future).** Homebrew tap;
  optionally crates.io publishing (12-crate namespace
  check + versioning policy); optionally Linux distro
  packages (AUR / `.deb` / `.rpm`).
- **Container distribution (future).** Docker image with
  the daemon as the entrypoint.
- **macOS signing + notarization (future, gated on Apple
  Developer account).** Removes the Gatekeeper friction at
  first launch.

## Milestone — Onboarding Templates (Phase 66 shipped)

**Forward commitment:** none — operator-feedback-shaped
substrate from the post-Phase-60 codebase review.
**Couples to:** `aivyx-pa init` (Phase 44 wizard).
**Status:** Phase 66 delivered the substrate + three starter
templates (2026-05-14). Future phases extend the template
library and add parameter substitution / web UI surface /
sharing primitives as adoption shape demands.

The original codebase review named three adoption-shape gaps
post-Phase-60: Distribution (Phases 61), Reach (Phases 62–63),
and Use-case onboarding. Phase 66 closes the third — operators
no longer write `aivyx-pa.toml` from scratch; the starter
templates ship sensible defaults for the common archetypes.

**Expected sub-phases / micro-phases:**

- **Phase 66 (Starter Profile Templates, shipped
  2026-05-14).** Template registry substrate (bundled +
  user-dir hybrid), CLI flags (`--template`,
  `--list-templates`), wizard pre-fill via
  `TemplateDefaults` + `render_with_template` splice-back,
  three bundled templates (`coder` / `researcher` /
  `personal`). Operator can run
  `aivyx-pa init --template coder` and get a useful
  `aivyx-pa.toml` with role declarations, MCP web search,
  and behavioral preferences baked in. Tests +18 (1176 →
  1194). All four streak predictions correct: DESIGN.md
  → 13, PRODUCT.md → 6, lib.rs → 14 (new record).

- **More starter templates (future).** `data-analyst`,
  `writer`, `student`, `devops-on-call`, etc. The substrate
  supports arbitrary additions — each is just a complete
  `aivyx-pa.toml` with archetype-appropriate defaults.
- **Template parameter substitution (future).**
  `{{operator_name}}` placeholders prompted at init time
  (vs Phase 66's literal-default-per-prompt approach).
- **Web UI template selection (future).** Today the
  template picker is CLI-only.
- **Template sharing primitives (future).** Today operators
  copy `.toml` files into `~/.local/share/aivyx-pa/templates/`
  manually. A curl-from-URL or community registry would let
  templates spread.

## Milestone — Reach (in progress, agent-initiated outbound)

**Forward commitment:** none — operator-feedback-shaped work
on the "what does the agent reach OUT to" axis.
**Couples to:** `aivyx-telegram` channel adapter (Phase 8),
operator identity layer (P13 + P14), schedules / webhooks /
file-watchers (Phases 26–27).
**Status:** Phase 1 of N delivered as "agent-driven notify.send
+ Telegram + webhook." Future sub-phases extend the reach
surface (trigger-config sugar, new channels, system-prompt
enumeration).

The post-Phase-60 codebase review surfaced **reach** as the
next-largest adoption-shape gap after distribution: schedules,
webhooks, and file-watchers fire turns, but the agent's output
stops at the audit log — there's no path from "agent has
something to say" to "operator's phone buzzes." This milestone
gives the agent the substrate to *initiate* contact, not just
respond to it. Closing the gap is the inflection point between
"thing I talk to" and "thing that talks to me."

**Expected sub-phases / micro-phases:**

- **Phase 62 (Agent-Initiated Outbound Notifications, shipped
  2026-05-13).** First phase of the milestone. Shipped the
  `notify.send` infrastructure tool, the `[[notify_target]]`
  config surface, and two backends (Telegram + generic
  webhook). Substrate: `notify.send` capability scope
  (CEILING_TRUSTED only) with `<target_name>` qualifier,
  `NotifyDispatcher` + `NotifyBackend` trait + 5-variant
  `NotifyError` classification, `build_notify_dispatcher`
  factory consuming the loaded config. Behavior: agent calls
  `notify.send {target, message, subject?}`; on success the
  tool returns `Completed` with `{success: true,
  delivered_at}`; on delivery failure the tool returns
  `Completed` with `{success: false, error_kind,
  error_message}` so the agent gets structured retry
  guidance. Daemon wired end-to-end. Tests +65 (1074 → 1139).
  All three streak predictions correct: DESIGN.md → 9,
  PRODUCT.md → 2, lib.rs → 10 (new record). Zero new
  workspace deps.

- **System-prompt enumeration (deferred from Phase 62 Task 8).**
  The `assemble_session_prompt` extension that surfaces
  reachable target names to the agent. Originally planned for
  Phase 62 Task 8 but deferred at implementation time —
  threading the dispatcher through six call sites with proper
  per-role capability checks was scope-creepy for one phase.
  Lands as a follow-on micro-phase when adoption surfaces
  friction.

- **Phase 63 (Trigger-Config Notify Sugar, shipped
  2026-05-13).** Closes the Phase 62-deferred trigger-config
  alternative. `[[schedule]]`, `[[webhook]]`,
  `[[file_watch]]` entries each accept an optional
  `notify_target = "..."`. When the trigger fires, the daemon
  auto-dispatches the agent's final response to the named
  target after the turn completes — operator-correct subject
  `<kind>: <trigger-id>`, no agent involvement, no
  system-prompt instruction. Config-load-time capability
  validation (Q5(a)) catches role/target mismatches at startup
  rather than at 9am the next morning. Three Record types
  gain `notify_target` with `#[serde(default)]` for backward
  compat. Tests +15. All three streak predictions correct:
  DESIGN.md → 10, PRODUCT.md → 3, lib.rs → 11 (new record).
  **Q1(a) audit-event variant scope-adjusted at exit:**
  `AuditEventKind::AutoNotifyDispatched` deferred to a
  follow-on phase because `TriggerDispatch` didn't hold an
  audit-hook reference. **Closed by Phase 67 (2026-05-14):**
  the variant shipped, the plumbing landed, and every
  trigger-fired auto-notify now records to the audit chain.
  Auto-notify still eprintln-logs for live visibility, but
  the audit chain is now the canonical record.

- **Phase 68 (Email SMTP backend, shipped 2026-05-15).**
  Adds the `kind = "email"` notify_target backend.
  `lettre` workspace dep with rustls TLS (no openssl
  pulls). Shared `[email]` config + per-target recipient
  (Q2(a)). STARTTLS port 587 default (Q3(a)). PLAIN+LOGIN
  auth with TLS required (Q4(a)). `LettreEmailSender`
  built once per deployment, shared via Arc into each
  email target's `NotifyEmailBackend`. Provider-specific
  setup notes in `docs/INSTALL.md` (Gmail / Fastmail /
  ProtonMail Bridge / AWS SES / self-hosted). Tests +19
  (1203 → 1222). All three streak predictions correct:
  DESIGN.md → 15, PRODUCT.md → 8, lib.rs → 16 (new
  record). First net-new workspace dep since Phase 58.

- **Phase 69 (Web UI desktop notifications, shipped 2026-05-15).**
  Adds the `kind = "web-ui"` notify_target backend. A single
  `WebUiBroadcaster` (wrapping `tokio::sync::broadcast`)
  fans out `DesktopNotificationFrame`s to every connected
  Web UI WebSocket; each browser tab subscribes a fresh
  receiver. New `DaemonMessage::DesktopNotification {
  title, body }` IPC envelope (Q4(a)). Browser-side wiring:
  `Notification` API + in-page toast banner per Q2 (both
  UX modes), one-time permission prompt on first page load.
  Zero subscribers yields `Ok(())` per Q1(a) — broadcast-
  style fire-and-forget; audit chain still records every
  dispatch. After Phase 69 the supported notify kinds are
  `telegram`, `webhook`, `email`, and `web-ui`. Tests +12
  (1222 → 1234). All three streak predictions correct:
  DESIGN.md → 16, PRODUCT.md → 9, lib.rs → 17 (new record,
  beats Phase 68's 16). Zero new workspace deps.

- **Phase 72 (Reach Tier-1 polish, shipped 2026-05-15).**
  Closes three operator-feedback shapes from the Reach
  Milestone backlog in one phase: multi-target dispatch
  (triggers fan out to N notify targets concurrently via
  `futures::join_all`, per-target audit per Q4(a)), default-
  target sugar (`default = true` on a `[[notify_target]]`
  block resolves into empty trigger lists at config-load
  time, one default allowed globally per Q2(a)), and
  conditional notify (`notify_when` enum gates dispatch by
  outcome: `Always | OnFailed | OnCompletedNonEmpty` per
  Q3(a)). New `AutoNotifyOutcomeSummary::SkippedByCondition`
  audit variant makes gate-skipped dispatches forensically
  distinguishable. Backwards-compat: singular `notify_target`
  stays valid as a one-element alias per Q1(a); declaring
  both forms on one trigger rejects at load. Tests +13
  (1289 → 1302), below the +20-30 prediction floor —
  fan-out integration test scaffolding deferred to a
  follow-up. All three streak predictions correct: DESIGN.md
  → 19, PRODUCT.md → 12, lib.rs → **20** (new record +
  two-decade milestone, beats Phase 71's 19). Zero new
  workspace deps.

- **Phase 73 (Reach Tier-2 polish, shipped 2026-05-15).**
  Closes the Tier-2 polish backlog Phase 72 deferred. Per-
  target retry on transient failures (Transport / Timeout /
  Rejected ≥ 500 per Q2(b)) via flat
  `retry_count` + `retry_backoff_ms_start` fields (Q1(b))
  with exponential backoff. Per-target in-memory token
  bucket rate limits (Q3(a)) with the new
  `AutoNotifyOutcomeSummary::SkippedByRateLimit` audit
  variant. Notification history surface — both Web UI
  Notifications pane and `aivyx-pa notify history` CLI walk
  the audit chain via the new `ListNotificationHistory`
  IPC (Q4(a)). After Phase 73 the Reach Milestone polish
  backlog is closed end-to-end. Tests +31 (1302 → 1333).
  All three streak predictions correct: DESIGN.md → 20,
  PRODUCT.md → 13, lib.rs → **21** (new record, beating
  Phase 72's 20). Zero new workspace deps.

- **Phase 74 (Memory polish, shipped 2026-05-16).** Completes
  the self-learning triad alongside Persona (P14) and
  reflection. `Memory` trait gains `search` (substring,
  case-insensitive), `list_topics`, `evict_oldest_unread`
  (LRU on `last_read_at_secs` per Q3(a)), and
  `gc_expired_with_rules`. `[[memory.retention]]` config
  blocks with topic-glob patterns drive per-topic-class
  retention (`forever` | `retention_days = N`, first-match
  wins per Q2(a)); unmatched topics fall through to the
  global `ttl_secs`. Operator surfaces: `memory.search`
  agent tool + Web UI Memory pane (read-only browse +
  search + per-topic evict per Q4(a)) + `aivyx-pa memory
  list/show/search/evict` CLI. Keyword-only search per
  Q1(a) — no embedding dep; semantic RAG defers. Tests +45
  (1333 → 1378). All three streak predictions correct:
  DESIGN.md → 21, PRODUCT.md → 14, lib.rs → **22** (new
  record, beating Phase 73's 21). Zero new workspace deps.

- **Phase 75 (Semantic RAG memory arc, shipped 2026-05-16).**
  Picks up Phase 74's deferred semantic-retrieval follow-up.
  Quality improvement to G3 memory substrate — not a new
  PRODUCT.md commitment. New encrypted `KeyDomain::MemoryVectors`;
  an `EmbeddingProvider` trait + OpenAI-compatible HTTP impl
  reusing the existing `aivyx-llm` transport (zero new deps,
  Q1(a)); `[embedding]` config (absent → semantic off, keyword
  unchanged); `Memory` vector store + hand-rolled cosine
  `semantic_search` + in-memory index; write-time embed
  (non-fatal) + bounded hourly backfill on the GC cadence
  (Q2(a)); `memory.search` `mode = keyword|semantic` with
  transparent flagged keyword fallback (Q4(a)) across the agent
  tool, `SearchMemory` IPC, `aivyx-pa memory search --semantic`
  CLI, and a Web UI toggle. **Privacy is the operator's
  `base_url`** — cloud API or a local OpenAI-compatible server
  (fully on-device). Tests +46 (1378 → 1424). All three streak
  predictions correct: DESIGN.md → 22, PRODUCT.md → 15,
  lib.rs → **23** (new record, beating Phase 74's 22). Zero
  new workspace deps.

- **Phase 76 (Automatic semantic recall, shipped 2026-05-16).**
  Closes the RAG arc Phase 75 set up — a G3 memory-substrate
  quality improvement, not a new PRODUCT.md commitment. A
  per-turn `ContextProvider` planner hook (read-side sibling
  of `PruneSink`, kept out of `aivyx-core/src/lib.rs` to
  protect the streak) embeds each user message and
  auto-injects the top semantically-relevant memories
  (`rag_top_k` / `rag_min_similarity` floor), as an
  injection-safe reference-only block, across the local-CLI,
  daemon, and child-agent planner paths. Silent no-op when
  embedding is unavailable — recall never errors a turn. The
  Q4b visible marker shipped as the established
  stderr-breadcrumb convention rather than a new `AuditTag`
  variant: the audit enum is in the streak file, and breaking
  the core streak to buy an observability nicety was the wrong
  trade — a deliberate, documented streak-forced deviation.
  Tests +15 (1424 → 1439), **below** the +25-40 prediction
  (honest miss — wiring-only Task 5, streak-collapsed Task 6).
  Streak all three correct: DESIGN.md → 23, PRODUCT.md → 16,
  lib.rs → **24** (new record, beating Phase 75's 23). Zero
  new workspace deps.

- **Phase 77 (Recall→reflection feedback loop, shipped
  2026-05-16).** Closes the self-learning loop — a quality
  improvement uniting already-delivered G3 (memory) / P8
  (reflection) / P14 (Persona), not a new commitment. A
  dedicated `KeyDomain::RecallEvents` log captures every
  auto-recall; a **structural, no-LLM** correlator scores
  recalled memories against the audit chain's existing turn
  outcomes; on the existing reflection cron the loop self-
  tunes memory retention (helpful memories kept LRU-warm, no
  new eviction primitive) and files **operator-gated Pending**
  Persona proposals (never auto-applied — the P14 authority
  rule holds). Deliberately routes the signal *around* the
  streak-locked `AuditTag` (the Phase 76 lesson applied by
  design). Tests +22 (1439 → 1461), **below** the +35-55
  prediction (honest miss — the actuators reused existing
  machinery rather than adding primitives). Streak all three
  correct: DESIGN.md → 24, PRODUCT.md → 17, lib.rs → **25**
  (new record, beating Phase 76's 24). Zero new workspace
  deps.

- **Phase 78 (Learning observability & trust surface, shipped
  2026-05-17).** Makes the closed self-learning loop legible —
  a transparency improvement to already-delivered G3/P8/P14,
  not a new commitment. A read-only view (computed on-query
  from the live recall log + audit + proposal chain — zero new
  storage) of what the assistant has learned and why: a
  per-window digest + per-Pending-proposal provenance tracing
  each recall-driven Persona proposal back to the recalls/turn
  outcomes that motivated it. Full parity: `GetLearningInsights`
  IPC + `aivyx-pa learning` CLI + a read-only Web UI Learning
  tab; approve/reject stays in the existing operator gate.
  Reuses existing `aivyx-channel` machinery (correlate_detailed
  shares the loop's single matching pass), so the operator
  never sees numbers that disagree with what the loop did.
  Tests +12 (1461 → 1473) — below the deliberately-lowered
  +18-30 prediction (third consecutive honest miss; pure
  derivation + reused round-trip harness + thin handler).
  Streak all three correct: DESIGN.md → 25, PRODUCT.md → 18,
  lib.rs → **26** (new record, beating Phase 77's 25). Zero
  new workspace deps.

- **Phase 79 (Adaptive Persona, shipped 2026-05-17).** Refines
  *how* the already-delivered P14 Persona is applied — no new
  product commitment, and the operator-visible contract is
  *strengthened*: a structurally-enforced invariant guarantees
  declared identity + every behavioral_constraint are always
  injected in full, while only the soft learned facets are
  selected per turn (semantic, reusing the Phase 76 embedding
  seam). Below a size threshold / no [embedding] / embed
  failure → byte-identical full Persona, so it is never a
  regression and engages only once the Soul is large. Made
  legible via the Phase 78 surface (a `persona_selection`
  field + per-turn breadcrumb). All hooks live in
  `aivyx-channel`/`llm_planner.rs` reusing existing types.
  Tests +14 (1473 → 1487) — just under the calibrated +15-25
  (4th consecutive small miss; the reuse-phase band has
  converged). Streak all three correct: DESIGN.md → 26,
  PRODUCT.md → 19, lib.rs → **27** (new record, beating Phase
  78's 26). Zero new workspace deps.

- **Phase 80 (Proactive Surfacing, shipped 2026-05-17).** The
  capstone of the 75–79 arc: the assistant now reaches out
  *first*. It introduces **no new product commitment** and
  weakens none — it is a quality/vision deepening of the
  already-delivered G3 (memory) / P8 (reflection) substrate,
  delivered as conservatively as the trust stakes demand:
  **off by default**, a structural no-LLM gate (it interrupts
  only when it can name a concrete reason — a TTL boundary, a
  strong Phase-77 recall cluster, a due reminder), a hard
  per-window cap on top of Phase 73's rate-limit, a never-nag
  dedup store, and every send recorded in the notify history +
  the Phase 78 learning surface. Reuse was near-total: a
  proactive surfacing is an auto-notify with a specific shape
  — no new scheduler, no LLM call, no agent turn, the existing
  `AutoNotifyDispatched` audit event. All code lives in
  `aivyx-channel` / `aivyx-config` / `aivyx-storage`. Tests
  **+20** (1487 → 1507) — **in band** (predicted ~+14-22,
  top), the first in-band landing after four consecutive small
  misses. Streak all three correct: DESIGN.md → 27,
  PRODUCT.md → **20**, lib.rs → **28** (new record, beating
  Phase 79's 27). Zero new workspace deps.

- **Phase 81 (Persona Lifecycle, shipped 2026-05-18).** Closes
  the open half of the identity arc: for 80 phases the Persona
  only ever grew. It introduces **no new product commitment**
  and weakens none — it refines *how* the already-delivered
  P14 Persona maintains itself, and the operator-facing
  contract is *strengthened*: a structurally-enforced
  guarantee that the scalar identity + every
  `behavioral_constraint` can never be consolidated or
  decayed, and full reversibility of every lifecycle action
  via the existing proposal + `Revert` chain. Delivered as
  conservatively as the trust stakes demand: **off by
  default**, **propose-only** (the loop never edits identity —
  it files normal Pending proposals the operator
  approves/rejects), a structural no-LLM gate (near-duplicate
  cosine clustering + age-based decay), and a deterministic
  never-nag dedup. Reuse was near-total — lifecycle actions
  are existing-shape `RemoveList` proposals through the
  existing chains, so no schema migration, no new `KeyDomain`,
  no new `AuditTag`. All code lives in `aivyx-channel` /
  `aivyx-config`. Tests **+17** (1507 → 1524) — **in band**
  (predicted ~+16-22), the second consecutive in-band
  landing. Streak all three correct: DESIGN.md → 28,
  PRODUCT.md → **21**, lib.rs → **29** (new record, beating
  Phase 80's 28). Zero new workspace deps.

- **Phase 82 (Persistent Helpfulness Ledger, shipped
  2026-05-18).** Makes the Phase 77 self-learning signal
  *durable*: for 81 phases it was recomputed each reflection
  window and discarded. It introduces **no new product
  commitment** and weakens none — it deepens the
  already-delivered G3/P8 substrate, and the operator-facing
  contract is *strengthened* (a longitudinal "what has
  consistently helped" view that did not exist). Delivered as
  conservatively as the signal's passive nature warrants:
  **zero-config** (the Phase 77 ethos — auto-built whenever
  auto-recall is on, no block), it **changes no behaviour on
  its own** (folded *after* the recall-feedback actuators,
  byte-identical), recency-weighted (a ~60-day EWMA half-life
  so stale signal fades), and self-pruning. Reuse near-total —
  a fold of the existing tally into a new HKDF-isolated
  `KeyDomain` on the existing reflection cron; no new
  scheduler, no LLM, no new `AuditTag`. All code in
  `aivyx-storage` (the KeyDomain) + `aivyx-channel`. Tests
  **+10** (1524 → 1534) — a **miss below the predicted
  ~+18-24** (first after two in-band landings): a zero-config,
  surface-only phase lands lighter than a config+detector
  phase even with a new KeyDomain; recalibrated honestly in
  PHASE_82.md. One test-only fix (a pre-existing shared-store
  path-collision flake exposed by the timing shift). Streak
  all three correct: DESIGN.md → 29, PRODUCT.md → **22**,
  lib.rs → **30** (new record, beating Phase 81's 29). Zero
  new workspace deps.

- **Phase 83 (Cross-Session Pattern Learning, shipped
  2026-05-18).** Phase 77's headline deferral, unblocked by
  the Phase 82 durable-ledger model. It introduces **no new
  product commitment** and weakens none — it deepens the
  already-delivered G3/P8 substrate, and the operator-facing
  contract is *strengthened* (a cross-session "topics that
  consistently help together" view that did not exist).
  Delivered as conservatively as the passive nature warrants:
  **zero-config** (the Phase 77/82 ethos — auto-built when
  auto-recall is on, no block), it **changes no behaviour on
  its own** (folded *after* the recall-feedback actuators and
  the Phase 82 ledger, both byte-identical), recency-weighted
  (the same ~60-day half-life), top-8-bounded, and
  self-pruning. Reuse near-total — a fold of already-
  correlated recall data into a new HKDF-isolated `KeyDomain`
  on the existing reflection cron; no new scheduler, no LLM,
  no new `AuditTag`. All code in `aivyx-storage` (the
  KeyDomain) + `aivyx-channel`. Tests **+10** (1534 → 1544) —
  a small miss vs the ~+12-16 refinement but squarely in the
  Phase-82-recalibrated ≈ +8-12 band; it landed at exactly
  Phase 82's +10 (the fixed scaffolding, not detector
  complexity, dominates this regime's test surface).
  Surface-only this phase; *consuming* the patterns is the
  explicit next phase. Streak all three correct: DESIGN.md →
  30, PRODUCT.md → **23**, lib.rs → **31** (new record,
  beating Phase 82's 30). Zero new workspace deps.

- **Phase 84 (Cluster-Aware Co-Recall, shipped 2026-05-19).**
  The first phase that *acts* on the durable learning
  substrate (Phases 82–83 were surface-only). It introduces
  **no new product commitment** and weakens none — it deepens
  the already-delivered G3 recall substrate, and the
  operator-facing contract is *strengthened* (opt-in,
  hard-bounded, budget-neutral, fully legible, self-policing).
  Recall becomes associative: a recalled topic's durable
  affined siblings (the Phase 83 ledger) the literal query
  missed are also surfaced, **sharing** the existing
  `rag_top_k` budget (zero context/token growth). The first
  hot-path behaviour change, so opt-in (the Phase 80/81
  discipline); cluster hits are excluded from the
  co-occurrence fold (no self-reinforcement) yet still scored
  by the helpfulness loop (a bad expansion self-penalises).
  Reuse high — a bounded post-step on the existing Phase 76
  recall seam + one new ledger query + a serde-safe marker;
  no new scheduler/KeyDomain/LLM/AuditTag. Tests **+10**
  (1544 → 1554) — a second consecutive miss vs the ~+16-22
  prediction; confirmed the calibration law that realized
  test count tracks *new unit-tested pure modules* (detector
  ≈ +7, KeyDomain ≈ +2, config ≈ +5-6), not config/behaviour
  breadth — Phase 84 added neither a detector module nor a
  KeyDomain, so it sits at the ≈ +10 floor. Streak all three
  correct: DESIGN.md → 31, PRODUCT.md → **24**, lib.rs →
  **32** (new record, beating Phase 83's 31). Zero new
  workspace deps.

- **Phase 85 (Helpfulness-Driven Persona Decay, shipped
  2026-05-19).** Completes the "self-improving Soul": Phase
  81 Persona decay was age-only; Phase 85 consumes the
  durable Phase 82 helpfulness ledger so a learned facet
  decays when its associated recall topic demonstrably
  stopped helping, and an old facet whose topic still helps
  is protected. **No new product commitment**, none weakened;
  the operator-facing contract is *strengthened* (decay now
  cites concrete helpfulness evidence and protects
  still-helpful identity). The long-flagged blocker dissolved
  — facet→topic is structurally exact via the
  `recall-fb:{topic}` proposal-id provenance — so the design
  stayed precise (only recall-feedback-derived facets gated;
  reflection-authored facets unchanged) and propose-only +
  `Revert` + core-protected (every Phase 81 safety property
  intact). With no helpfulness ledger it degrades gracefully
  to byte-identical Phase 81. Reuse near-total — extend the
  existing detector/pass/config/surface; no new
  scheduler/module/KeyDomain/LLM/AuditTag. Tests **+5** (1554
  → 1559) — below the ~+8-12 nominal but exactly the hedged
  "no new config section either" floor; the calibration law
  is now fully converged (count tracks new unit-tested pure
  modules, with config-knobs-on-an-existing-block ≈ +1). A
  calibration refinement, not a scope miss. Streak all three
  correct: DESIGN.md → 32, PRODUCT.md → **25**, lib.rs →
  **33** (new record, beating Phase 84's 32). Zero new
  workspace deps.

- **Phase 86 (Conversational-Window Relevance, shipped
  2026-05-20).** The twice-deferred (Phase 76 *and* Phase 79)
  input-quality gap closed. For 85 phases the assistant judged
  relevance off *one line* — the latest user message — so
  auto-recall pulled the wrong memories and the Soul selected
  the wrong facets in exactly the multi-turn usage that
  matters most. Phase 86 gives both consumers a recent
  conversational window: a small recency-ordered slice of the
  last few `(user, assistant)` turns concatenated into the
  same single embed they already make (current message last
  so it dominates). **No new product commitment**, none
  weakened; the operator-facing contract is *strengthened*
  (G3 recall + P14 adaptive Persona now see multi-turn intent
  instead of a one-liner). Opt-in by a single
  `[embedding].recall_window_turns` knob (default `1` =
  pre-Phase-86 byte-identical); the buffer is ephemeral
  (daemon-memory only); the existing `rag_min_similarity` /
  Persona-selection floors are the unchanged safety net
  against a drifted window. Streak all three correct:
  DESIGN.md → **33**, PRODUCT.md → **26**, lib.rs → **34**
  (new record, beating Phase 85's 33) — the trait-extension
  ripple is `llm_planner.rs`-only, the new module lives in
  `aivyx-channel`, and the knob is a field on the existing
  `[embedding]` block. Zero new workspace deps. Positive
  cascade: the daemon's `Message::session_id` is now stable
  across turns (was fresh-per-turn), which both makes the
  buffer key load-bearing and corrects Phase 77's recall
  correlation.

- **Phase 87 (Pattern-Driven Persona Proposals, shipped
  2026-05-20).** Closes the deliberate Phase 85 deferral.
  After Phase 84 (recall acts on the Phase 83 co-occurrence
  ledger) and Phase 85 (decay acts on the Phase 82
  helpfulness ledger), the visible asymmetry was that the
  co-occurrence ledger fed only *recall*, not the *Soul*.
  Phase 87 closes the symmetric arc: durable consistently-
  co-occurring pairs of *helpful* topics propose a new
  `learned_context` facet through the existing Phase 70
  proposal chain — same propose-only + edit-then-approve +
  Revert + core-protected flow, just driven by the second
  durable signal. **No new product commitment**, none
  weakened; the operator-facing contract is *strengthened*
  (P14 Persona now also learns cross-topic *relationships*,
  not only per-topic warmth). Opt-in via a new
  `[persona_consolidation]` block (`enabled = false`
  default — the Phase 80/81/84 actuator posture);
  conservative double-gate (pair affinity + samples + both
  endpoints individually helpful); LLM-summarized facet prose
  with operator-as-final-filter; reflection-cron cadence with
  hard per-cycle cap + absolute dedup against the proposal
  chain in any status. Streak all three correct: DESIGN.md →
  34, PRODUCT.md → **27**, lib.rs → **35** (new project
  record, beating Phase 86's 34) — pass + config + Phase 78
  surface stat all live in `aivyx-channel` / `aivyx-config` /
  `bin/aivyx-pa`; proposals land through the existing chain API
  (no new chain operation); no new AuditTag. Test count delta
  `+15` (workspace 1573 → 1588), exactly the upper edge of
  the predicted `+11-15` band. Zero new workspace deps.

- **Phase 88 (Pattern-Driven Persona Decay, shipped
  2026-05-20).** Closes the deliberate Phase 87 deferral with
  the decay half of the symmetric arc. After Phase 87 made
  the co-occurrence ledger drive Persona *construction*,
  Phase 88 makes the **same ledger** drive Persona *decay*:
  a `consolidate-pair:` facet whose underlying pair has
  demonstrably weakened is decay-proposed (the relationship
  that justified the identity no longer holds), and
  symmetrically, a still-durable pair **protects** its facet
  from age-decay. After Phase 88, every durable learning
  signal feeds both sides of the Soul lifecycle. **No new
  product commitment**, none weakened; the operator-facing
  contract is *strengthened* (the Soul now retires identity
  when the relationship behind it dissolves, not only when
  the underlying topic stopped helping). Single new
  `decay_pair_below_affinity` knob on the existing
  `[persona_lifecycle]` block (default `1.0` — mirrors
  Phase 87's `min_affinity`); gated by the existing
  `signal_decay`; no helpfulness conditioning (single-
  signal gate); symmetric OR-protection extension. Streak
  all three correct: DESIGN.md → 35, PRODUCT.md → **28**,
  lib.rs → **36** (new project record, beating Phase 87's
  35) — detector extension + config knob + fold-site read
  all in `aivyx-channel` / `aivyx-config`; proposals land
  through the existing chain (no new operation, no new
  AuditTag). Test count delta `+6` (workspace 1588 → 1594),
  inside the predicted `+3-7` band. Zero new workspace deps.

- **Phase 89 (Topic Canonicalization, shipped 2026-05-20).**
  Closes the longest-standing learning-stack deferral — the
  Phase 82 deferral carried forward six times. For 88 phases
  every topic-keyed accumulator (memory, recall log,
  helpfulness ledger, co-occurrence ledger, Persona
  consolidate-pair provenance) keyed by the operator's typed
  topic VERBATIM — so `deploy` / `Deploy` / `deploys` /
  `deploying` were four distinct topics across every signal,
  and the value that should add up across them was silently
  fragmented. The first phase since the act-on-durable-
  learning arc closed that **sharpens existing signals**
  rather than adding a new capability. **No new product
  commitment**, none weakened; the operator-facing contract
  is *strengthened* (G3 recall and P14 Persona learn from
  cleaner accumulated signal). Single opt-in
  `[memory].canonicalize_topics: bool` knob (default `false`,
  matching the 88-phase behaviour-change-is-opt-in
  discipline); hand-rolled English stemmer (lowercase + trim
  + whitespace fold + `ies → y` / `ing` / `ed` / hissing-`es`
  / `s` strips, idempotent, zero new deps); write-side only
  (no migration; existing fragmented signal decays out via
  the Phase 82/83 ~60-day half-life + the Phase 77 ~30-day
  recall-log retention). Implemented as a thin
  `CanonicalizingMemory` wrapper-delegate at the `Memory`
  trait boundary — one canonicalization site per method,
  applied uniformly to whichever inner impl the binary
  picked. Streak all three correct: DESIGN.md → 36, PRODUCT.md
  → **29**, lib.rs → **37** (new project record, beating
  Phase 88's 36) — Memory trait + helper + wrapper all live
  in `aivyx-memory`, not aivyx-core. Test count delta `+20`
  (workspace 1594 → 1614), comfortably above the predicted
  `+6-10` band. Zero new workspace deps.

- **Phase 90 (Heuristic Recall Gate, shipped 2026-05-20).**
  Closes the longest-running recall-side deferral (Phase 76,
  carried forward 14 phases). For 89 phases auto-recall and
  adaptive Persona selection fired on EVERY turn, including
  single-token acknowledgments (`ok` / `thanks` / `yes` /
  `cool`) where the bare-message embed is essentially a
  random vector that pollutes the ranker. **No new product
  commitment**, none weakened; the operator-facing contract
  is *strengthened* (G3 recall + P14 Persona no longer waste
  embed cost or pollute their rankers on noise turns). The
  third move in the input-quality arc after Phase 86
  (windows) + Phase 89 (canonicalization): Phase 90 sharpens
  *when* recall fires at all. Single opt-in
  `[embedding].recall_gate_min_chars` knob (default `0` =
  disabled = byte-identical to pre-Phase-90; raise to gate
  trimmed-Unicode-char-count shorter messages). Same gate
  drives both relevance providers (auto-recall + adaptive
  Persona); both short-circuit to `None` before any embed
  call. Streak all three correct: DESIGN.md → 37, PRODUCT.md
  → **30**, lib.rs → **38** (new project record, beating
  Phase 89's 37) — gate function + provider short-circuits
  all in `aivyx-channel`, knob is a new field on the
  existing `EmbeddingConfig`. Test count delta `+15`
  (workspace 1614 → 1629), over the predicted `+6-10` band
  — recording-provider matrix on both providers earned its
  coverage. Zero new workspace deps.

- **Phase 91 (LLM-Judged Recall Usefulness, shipped
  2026-05-21).** Closes the longest-running feedback-side
  deferral (Phase 77, carried forward 14 phases). For 90
  phases the recall-feedback signal had been STRUCTURAL
  (turn-level proxy) — every recall in a successful turn
  inherited `+1` helpfulness, every recall in a failed turn
  `-1`. Phase 91 adds an opt-in LLM-judged per-recall
  classification (`Used` / `Irrelevant` / `Hurt`) alongside
  the structural proxy. **Augment, not replace (Q3a):** the
  new `judgment: Option<RecallJudgment>` field on
  `RecallHit` is captured but every existing accumulator
  (Phase 82 ledger, Phase 83 co-occurrence, Phase 85/88
  decay, Phase 87 proposals) stays byte-identical in v1; a
  future phase consumes the new signal once validated in
  production. **No new product commitment**, none weakened;
  the operator-facing contract is *strengthened* (the
  learning loop now captures a sharper signal even though
  consumers continue to use the structural proxy). After
  the input-quality arc (86/89/90), Phase 91 is the
  symmetric **feedback-quality** move that completes the
  learning-loop picture. Single opt-in `[recall_judgment]`
  block (`enabled = false` default). Reflection-cron batched
  (one LLM call per cron tick, the Phase 87
  `LlmPairPhraser` precedent). Streak all three correct:
  DESIGN.md → 38, PRODUCT.md → **31**, lib.rs → **39** (new
  project record, beating Phase 90's 38) — new trait +
  adapter + pass + stat + IPC field all in `aivyx-channel`,
  config in `aivyx-config`, no new AuditTag. Test count
  delta `+14` (workspace 1629 → 1643), inside the predicted
  `+9-15` band. Zero new workspace deps.

- **Phase 92 (Pattern-Driven Supersession, shipped
  2026-05-21).** Closes the longest-running Persona-actuator
  deferral — the Phase 70 proposal-supersession deferral, 22
  phases old, deferred again at Phase 87 and Phase 88. After
  Phases 87 + 88 closed the construction + decay arcs, the
  Soul actuator handled a shifting co-occurrence pair
  `(A, B) → (A, C)` as TWO independent operator decisions
  (Phase 88 proposing decay of the old facet, Phase 87
  proposing the new one). Phase 92 introduces opt-in
  shared-endpoint supersession: when the conditions align,
  the `RemoveList` + `AppendList` proposals are filed
  **linked by metadata** so the operator-facing surface
  presents them as a single supersession decision. **No new
  product commitment**, none weakened; the operator-facing
  contract is *strengthened* (the Soul's proposal flow
  groups linked decisions). Single new `enable_supersession`
  knob on the existing `[persona_consolidation]` block
  (default `false`). Reuses Phase 87's `PairPhraser`;
  reuses the existing Phase 70 proposal chain (no new
  proposal kind, no chain-schema migration); new optional
  `supersedes_proposal_id: Option<String>` field on
  `ProposedPersonaDelta` with full wire-compat via the
  Phase 84 / Phase 91 `#[serde(default, skip_serializing_
  if = "...")]` shape. Each half remains independently
  `Revert`-able. Streak all three correct: DESIGN.md →
  39, PRODUCT.md → **32**, lib.rs → **40** (new project
  record, beating Phase 91's 39) — detector + field + pass
  integration all in `aivyx-channel`, config in
  `aivyx-config`, no new AuditTag. Test count delta `+10`
  (workspace 1643 → 1653), squarely inside the predicted
  `+6-10` band. Zero new workspace deps.

- **Phase 93 (Recall-Feedback → Judgment Signal, shipped
  2026-05-21).** Closes the Phase 91 deferral named
  verbatim in the Phase 92 open doc: *"actuator-side
  switch from structural proxy to the new judgment
  signal."* Phase 91 introduced `LlmRecallJudge` and
  recorded per-hit `judgment: Option<RecallJudgment>` on
  every recall hit, but explicitly scoped that change as
  **v1 augment, not replace** — the judgments flowed into
  the audit chain and Phase 78 surface without any
  runtime actuator consuming them. Phase 93 wires the
  consumer: `correlate_detailed` (the function driving
  memory promotion and Persona proposal filing) now reads
  per-hit verdicts where present
  (`Used → +WEIGHT`, `Hurt → -WEIGHT`,
  `Irrelevant → 0`) and falls back to the existing
  Phase 77 turn-level structural proxy where absent.
  Single new
  `[recall_feedback].use_judgment_signal: bool` knob
  (default `false`); with it off the correlator is
  byte-identical to pre-Phase-93. The knob lives on the
  consumer side, separate from the Phase 91 producer-side
  `[recall_judgment]` block, keeping the two configs
  independently reason-aboutable. **No new product
  commitment**, none weakened; the operator-facing
  contract is *strengthened* (the self-improving loop is
  closed end-to-end when both knobs are on). The
  `LearningDigest` gains an optional `judgment_signal`
  field surfacing the augment state to the operator. Mid-
  phase the scope was corrected — the original open
  commit pivoted around a per-domain `min_score`
  threshold that doesn't exist in the codebase; the
  re-scope commit redirected Phase 93 to the loop closure
  that actually exists. Streak all three correct: DESIGN.md
  → **40**, PRODUCT.md → **33**, lib.rs → **41** (new
  project record, beating Phase 92's 40) —
  augmentation in `aivyx-channel`, config in
  `aivyx-config`, `HelpfulnessTally` shape unchanged so
  downstream actuators byte-identical. Test count delta
  `+11` (workspace 1653 → 1664), one over the predicted
  `+6-10` band, accounted for by the optional
  `judgment_signal` digest field earning its own surface-
  rendering test alongside the field plumbing. Zero new
  workspace deps.

- **Phase 94 (Web UI Grouping for Linked Supersession
  Proposals, shipped 2026-05-21).** Closes Phase 92's
  first deferral: the linked supersession pair (the
  `RemoveList` half retiring an old `consolidate-pair:`
  facet + the `AppendList` half proposing the new one,
  cross-referenced via Phase 92's `supersedes_proposal_id`)
  now renders as one grouped unit in both the
  `aivyx-pa persona proposals` CLI and the Web UI Persona-
  pane Proposals tab, instead of two unrelated rows. The
  CLI gets `└─ supersedes:` / `└─ superseded by:`
  indicators under each half; the Web UI gets a single
  outer card with a `↔ linked supersession` banner, both
  halves stacked with a `↓ supersedes ↓` arrow between
  them, and a shared action row offering primary
  **Approve both** + **⋮ Split** menu + **Reject both**.
  **No new product commitment**, none weakened; the
  operator-facing contract is *strengthened* (a confusing
  two-decision flow becomes one ergonomic decision). Pure
  client-side rendering — no chain primitives changed,
  no daemon-side enrichment, no new IPC method; the
  `supersedes_proposal_id` field added to
  `PersonaProposalSummary` is wire-compatible via
  `#[serde(default, skip_serializing_if =
  "Option::is_none")]`. Generic via a small
  `GroupableProposal` trait so the same algorithm runs
  on both surfaces. Web UI **Approve both** fires two
  sequential `ResolvePersonaProposal` IPC calls; Phase
  92's `each half independently Revert-able` guarantee
  covers the half-approved-on-failure case without
  needing a transactional primitive. Streak all three
  correct: DESIGN.md → **41**, PRODUCT.md → **34**, lib.rs
  → **42** (new project record, beating Phase 93's 41) —
  helper + CLI render in `aivyx-channel`, Web UI in the
  embedded static HTML asset. Test count delta `+11`
  (workspace 1664 → 1675), one over the predicted `+6-10`
  band, accounted for by the helper earning extra edge-
  case coverage (self-reference, asymmetric link, same-op
  pair, dangling partner, position-determinism, empty
  input all worth a test apiece). Zero new workspace deps.

- **Phase 95 (Reflection Cadence Learning — Skip-When-Idle,
  shipped 2026-05-21).** Closes the Phase 71 deferral
  *"reflection cadence learning"* carried 24 phases. The
  reflection cron has been firing unconditionally on its
  configured `cron` since Phase 71, paying LLM cost for
  Phase 87 phrasing + Phase 91 judgment + Phase 92
  supersession passes even on idle days. Phase 95 closes
  the deferral with the simplest leverage shape — the
  scheduler reads audit-chain growth since the last
  *fired* cycle for that schedule; if growth is below
  `min_audit_entries_to_fire` AND `skip_when_idle = true`,
  the cycle is skipped entirely (no LLM calls; just a log
  line + a counter bump). Two new optional fields on the
  existing `[[reflection_schedule]]` block;
  per-schedule independence so different schedules can carry
  different idleness tolerances. The operator's cron
  remains the **upper bound** on firing rate — cadence
  learning is monotonic-slower-only, never faster.
  **No new product commitment**, none weakened; the
  operator-facing contract is *strengthened* (operators
  with idle days no longer pay LLM tokens for
  passes that find nothing actionable when they opt in).
  First cycle after daemon boot is unconditional (no prior
  baseline); subsequent cycles consult audit-growth. The
  `aivyx-pa learning` surface gains a "Reflection cadence"
  block with per-schedule `K fired, S skipped` counts;
  daemon log shows skipped cycles in real time. Streak all
  three correct: DESIGN.md → **42**, PRODUCT.md → **35**,
  lib.rs → **43** (new project record, beating Phase
  94's 42) — helper + state + integration all in
  `aivyx-channel`, config knobs in `aivyx-config`. Test
  count delta `+12` (workspace 1675 → 1687), two over the
  predicted `+6-10` band, accounted for by the helper +
  state earning more individual boundary-case tests than
  the calibration law anticipated. Zero new workspace
  deps; one `large_enum_variant` allow added to
  `QueryResponsePayload` since the addition tipped a long-
  running additive-fields variant past clippy's threshold.

- **Phase 96 (ANN Index for Semantic Memory Search,
  shipped 2026-05-21).** Closes the Phase 75 deferral
  carried 20 phases: the approximate-nearest-neighbor
  index for semantic memory search. The brute-force
  `rank_by_cosine` has been the only path since Phase 75
  and scales linearly with memory size. Phase 96 adds
  opt-in IVF-style clustering — vectors partition into
  K ≈ √N clusters at build time; queries cosine-rank the
  centroids, take top-N clusters, and brute-force within
  those. The existing brute-force then re-ranks the
  candidate set so the final top-K ordering is exact
  within candidates. End-to-end: O(N) → O(√N) per query.
  Hand-rolled (~150 lines in aivyx-memory) — preserves
  the project's zero-new-deps streak. Index lives in-
  memory, rebuilt on demand when an atomic write-count
  counter crosses the operator-configured threshold.
  Two new optional fields on `[embedding]`:
  `ann_index: bool` (default `false`) +
  `ann_rebuild_threshold: u32` (default `100`). With
  `ann_index = false` the recall path is byte-identical
  to pre-Phase-96 brute-force. **No new product
  commitment**, none weakened; the operator-facing
  contract is *strengthened* (large memory stores stay
  responsive when the operator opts in). Streak all
  three correct: DESIGN.md → **43**, PRODUCT.md → **36**,
  lib.rs → **44** (new project record, beating Phase
  95's 43) — ANN module + RedbMemory integration in
  aivyx-memory, dispatch in aivyx-channel, config in
  aivyx-config. Test count delta `+22` (workspace 1687 →
  1709), squarely inside the predicted +15-25 band.
  Zero new workspace deps.

- **Phase 97 (Token-Budget Context Sizing, shipped
  2026-05-21).** Closes the twice-deferred token-budget
  item carried 21 phases (Phase 76) and 11 phases (Phase
  86) — the longest-running content/correctness deferral
  on the backlog. Auto-recall, adaptive Persona, and
  the conversational window have all capped injection
  by ENTRY COUNT (a proxy for token cost, not the cost
  itself). A single 4 KB memory body could silently
  displace multiple shorter ones from the same
  `rag_top_k` budget; a grown Persona facet could eat
  turn after turn of input — sometimes enough to bump
  the prompt past the model's context limit. Phase 97
  adds an opt-in `recall_token_budget` that caps both
  recall + Persona injection paths AFTER their existing
  rank-and-filter steps: lowest-ranked items drop until
  the running estimate fits. Hand-rolled `chars/4`
  estimator (~±20% accuracy; no new tokenizer dep). No
  mid-item truncation — full items or nothing. The
  protected Persona core (constraints + identity
  scalars) is always present regardless of budget; the
  budget only trims soft-facet selection. **No new
  product commitment**, none weakened; the operator-
  facing contract is *strengthened* (the loop no longer
  silently inflates context cost when entries grow
  long). Streak all three correct: DESIGN.md → **44**,
  PRODUCT.md → **37**, lib.rs → **45** (new project
  record, beating Phase 96's 44) — token_budget module
  + recall integration + Persona integration all in
  aivyx-channel, config in aivyx-config. Test count
  delta `+21` (workspace 1709 → 1730), over the
  predicted +10-15 band, accounted for by the pure
  helper module earning ~12 individual boundary-case
  tests instead of the calibration law's expected 5-7.
  Zero new workspace deps.

- **Phase 98 (Hybrid Keyword+Semantic Recall Fusion,
  shipped 2026-05-21).** Closes Phase 75's 23-phase-old
  hybrid-fusion deferral. Auto-recall has ranked by
  cosine similarity over embeddings since Phase 75 —
  strong on semantic relationships but weak on rare-term
  recall (acronyms, proper nouns, code identifiers,
  project codenames). The keyword search tool (Phase 74,
  `Memory::search`) handles those exact-match cases via
  case-insensitive substring matching but operated as a
  separate manual path. Phase 98 fuses the two via
  Reciprocal Rank Fusion (RRF) — the industry-standard
  rank-aggregation approach. With
  `[embedding].recall_hybrid = true`, auto-recall runs
  both rankers at recall time and combines their
  rankings via `score = Σ 1 / (k + rank + 1)` with
  `k = 60`. Rank-based fusion means cosine scores and
  substring hit counts don't need normalization. Hand-
  rolled (~30 lines in aivyx-channel::recall_fusion);
  preserves the zero-new-deps streak. The
  `rag_min_similarity` floor is skipped on the hybrid
  path (RRF scores aren't on the cosine scale); a
  separate `rag_hybrid_min_rrf` knob is a documented
  deferral. **No new product commitment**, none
  weakened; the operator-facing contract is
  *strengthened* (rare-term queries like "ATC-417" or
  "Jane Henderson" or `HashMap::insert` now reliably
  surface memories about those terms when the operator
  opts in). Streak all three correct: DESIGN.md →
  **45**, PRODUCT.md → **38**, lib.rs → **46** (new
  project record, beating Phase 97's 45) — recall_fusion
  module + recall integration in aivyx-channel, config
  in aivyx-config. Test count delta `+16` (workspace
  1730 → 1746), slightly over the predicted +10-15
  band, accounted for by the RRF module's comprehensive
  boundary coverage. Zero new workspace deps.

- **WebPush / service-worker notifications (future).**
  Phase 69 requires the Web UI tab to be open. WebPush
  would let notifications fire even with the tab closed;
  needs VAPID key generation + service-worker registration
  + push subscription persistence. Real engineering;
  speculative pending operator pressure.

- **Slack-flavored webhook payload (future, gated on use).**
  Slack incoming webhooks expect `{text: ...}` not
  `{message: ...}` — a real shape mismatch. Lands as a
  separate `kind = "slack-webhook"` if Slack-using
  operators surface.

- **Agent-initiated proactive Telegram session (future,
  speculative).** Today's Telegram adapter reacts to inbound
  messages. A future evolution would let the agent open a
  session-of-its-own-initiative — e.g. on a schedule-fired
  turn that wants to start a multi-turn conversation, not
  just push a single notification. Architecturally heavier;
  not on the current sub-phase ladder.

## Sequencing notes (revised at Phase 56 sign-off, 2026-05-12)

Through Phase 55, every PRODUCT.md commitment (P1–P12) is
delivered. Chapter A (Phases 50–54) closed Foundation
Closeout. Phase 55 demonstrated the post-Chapter-A posture
by porting the Phase 52 sandbox pattern to `[[mcp_server]]`
in response to THREAT_MODEL §5.2.

**Forward arc — Profile + Persona (Phases 56–60).** The
operator's stated vision (self-learning, self-improving
AI-personal assistant with a user-defined Profile and
Persona) reframes the project's next phase block. Phase 56
files the P13 + P14 amendments and updates PRODUCT.md (docs
only, no code). Phases 57–58 deliver the Profile half.
Phases 59–60 deliver the Persona half. After Phase 60, the
project will have a full operator-declared + agent-learned
identity layer composed against the existing
substrate.

For the per-phase narrative including Chapter A, see
[`ROADMAP.md`](ROADMAP.md). For the current implementation
state of each P1–P12 commitment, see the Delivery Status
section in [`../PRODUCT.md`](../PRODUCT.md).

### Other forward work (operator-feedback-shaped)

Beyond the Profile + Persona arc, future phases continue to
work against:

- **Hardening & operator-facing polish** — items that have not
  yet shown enough pressure to be milestones. Examples:
  container-sandbox profile presets, audit log rotation if
  chain size becomes load-bearing, additional channel adapters
  (Matrix / Signal / Discord), additional tool-process
  examples, signed third-party tool registries.
- **Strategic conversations** — anything that would amend
  DESIGN.md or PRODUCT.md goes through the amendment process
  before opening a phase. Seven amendments stand as the
  precedent (Phase 56 will file the eighth and ninth).
- **Operator feedback loops** — at this point the project
  benefits more from real-world operator use than from
  speculative forward work outside the Profile + Persona arc.

## Delivered

Every product-shape milestone shipped across the 54-phase arc.
Grouped by commitment for traceability.

### `PRODUCT.md` commitments

- **P1 — Sub-Agent Role-Switching** — Phases 14, 33. Inline
  sub-session nesting via an `OnceLock`-backed `RoleSwitchTool`
  factory; multi-level nesting closed in Phase 33. Structural
  impossibility of escalation pinned by integration tests +
  `--print-role` reachable-targets enumerator.

- **P2 — Mission Primitive** — Phases 21, 23, 28, 35. Six-state
  machine, `mission.create` / `mission.gate` capability bases,
  IPC extensions, CLI + Telegram gate prompts, escalation → gate
  turn-loop wiring, `mission.list` / `mission.status` inspection.

- **P3 — Goals and Non-Goals** — all 7 goal commitments (G1–G7)
  shipped, see "Goal commitments" subsection below.

- **P4 — Daemon-Default Architecture** — Phases 16–20. Protocol
  settlement, production hardening, REPL wiring, multi-connection
  + Telegram port, daemon management.

- **P5 — Open First-Party Channel Surface** — Phase 48. v0
  `CHANNEL_SDK.md` + `examples/python-channel/` + 15-test
  conformance suite.

- **P6 — OS-Level Operator Identity** — always true by
  construction. IPC socket mode 0600 owned by operator UID.

- **P7 — Single-Inheritance Role Tree** — Phases 11, 13. Strict
  attenuation along every dimension, validated at config load.

- **P8 — Outcome-Driven Audited Reflection** — Phases 28–30.
  Audit introspection (`turn.history`), reflection loop
  (`reflection.propose` / `reflection.apply`), runtime role
  mutation (`role.update` + planner factory integration).

- **P9 — Per-Role Full Capability Declaration** — Phase 13.
  `capability_scopes` parsed via `Scope::parse` at config load.

- **P10 — Substrate-Only Core, Ten Tools Forever** — true by
  contract. `web.post` added in Phase 37, A5 amendment Phase 38;
  `fs.delete` + `fs.metadata` added in Phase 100, A11 amendment
  the same phase.

- **P11 — SDK Contract** — Phases 48 (channel half) + 49 (tool
  half). `docs/CHANNEL_SDK.md` + `docs/TOOL_SDK.md` v0 contracts.

- **P12 — Tools as Separate Processes Over Daemon IPC** —
  Phase 49 foundation + Phase 50 closeout + Phase 52 sandbox
  hardening. `aivyx-tool` crate, `ToolProcessBridge`,
  `ToolProxy`, `run_tool_as_subprocess<T: Tool>` proven
  equivalent in `p12_equivalence.rs`, optional
  `[tool_process.sandbox]` wrapper layer.

### Goal commitments (P3)

- **G1 — Web interaction** — `web.fetch` (Phase 12), `web.post`
  (Phase 37), binary body + redirects (Phase 37). Rich web
  interaction (page rendering) not pursued — `web_search` and
  `web_read` bundled MCP tools cover the operator-useful subset.
- **G2 — Code interaction** — `fs.read`, `fs.write`,
  `shell.exec` operational with role gating; shell hardened
  in Phase 42.
- **G3 — Memory Reflection** — Phases 28–30. Memory substrate,
  reflection loop, runtime role mutation.
- **G4 — Sub-agent orchestration** — Phases 14, 33. Multi-level
  role-switching with capability attenuation.
- **G5 — Autonomous and scheduled execution** — Phases 26–27.
  Cron schedules, webhooks, file watchers, trigger unification,
  mission wrapping.
- **G6 — Local execution, privacy non-negotiable** — Phase 34
  (Ollama first-class with health check). No hosted control
  plane at any point.
- **G7 — Third-party tool SDK** — Phases 48, 49. v0 channel SDK
  + v0 tool SDK + MCP integration (Phases 23–24, 32) as a
  parallel adapter.

### Cross-cutting initiatives

- **Daemon Migration** — Phases 16–20 (architecturally
  complete). Five-phase keystone that reshaped the binary
  lifecycle and unlocked P5, P12, P2, P1.
- **Role-Config Migration** — Phase 13. Per-role envelope,
  single-inheritance.
- **MCP Integration** — Phases 23–24, 32. `aivyx-mcp` crate,
  stdio + SSE transports, daemon-side bridge lifecycle.
- **Multi-Provider Support** — Phases 25, 34. Anthropic,
  OpenAI-compatible, Ollama-first-class.
- **Web UI Channel** — Phases 39, 47. Localhost-only chat
  surface + mission/audit/sessions inspection panes.
- **Bundled MCP Web Search** — Phase 46. `aivyx-pa mcp-server
  web-search` with Brave / SerpAPI / DuckDuckGo backend chain.
- **Rich Input (Multimodal)** — Phase 45. `ContentBlock`,
  image input across Anthropic / OpenAI / Telegram / Web UI.
- **`aivyx-pa init` Wizard** — Phase 44. First-run interactive
  setup, Ollama auto-detection.

### Chapter A — Foundation Closeout (Phases 50–54)

The arc that brought every loose end from Phases 0–49 to a
close.

- **Phase 50 — P12 closeout.** Wired the two Phase 49 deferred
  bridge stubs (`ToolEvent` channel relay, per-call cancellation).
  Added `run_tool_as_subprocess<T: Tool>` harness. Proved
  "extractable without rewriting" with `p12_equivalence.rs`.
- **Phase 51 — Cleanup.** Closed three pre-existing items:
  `AivyxError::{Storage,Crypto}` typed nested errors (D6
  honored after 50 phases), `handle_connection` parameter-struct
  lift, `AIVYX_PA_PASSPHRASE` TOML/env footgun (TOML path now
  drives derivation).
- **Phase 52 — Sandbox layer.** Generic command-wrapper
  sandbox for `[[tool_process]]`. Operator supplies the policy
  (bubblewrap / firejail / Docker / sandbox-exec). Narrowed
  THREAT_MODEL.md §5.6.
- **Phase 53 — (skipped).** Audit log rotation deferred
  indefinitely; chain growth is bounded by tool-call frequency
  × uptime.
- **Phase 54 — Final docs sweep.** Brought the docs back in
  sync with the substrate. Root README rewrite, this
  PRODUCT_ROADMAP refresh, DAEMON_IPC Phase 47 addendum, A3
  scope-base count addendum, walkthrough refresh, cross-doc
  consistency spot-check.

After Phase 54 the project sits at: 984 Rust tests + 24 Python
conformance tests passing, zero clippy warnings, 12 workspace
crates, 43 capability scope bases, 9 encrypted storage domains,
7 contract amendments filed, 4 deferrals carried forward (none
load-bearing).

## Milestone — Forward-Commitment Ledger Closed (Phase 61)

**Status:** Closed. By Phase 61 every PRODUCT.md forward
commitment (P1–P14) and every goal commitment (G1–G7) had
shipped. With the contract surface complete, **this document
stops adding milestones** — there are no further PRODUCT.md-
derived product-shape milestones to track, because the contract
is closed. Phases 56–60 delivered the last two commitments (the
P13 Profile + P14 Persona layers); Phase 61 wired the release
substrate (publication still pending public hosting).

Everything after Phase 61 is **platform extension within the
locked contract** — new capability *surface*, not new
contractual *commitments*. That work is organised as named
chapters and tracked in [`ROADMAP.md`](ROADMAP.md), not here:

- **Chapter B — Tooling**, **C — Operator Onboarding**,
  **D — Substrate Breadth**, **E — Self-Improvement Loop
  Deepening** (Phases 100–122).
- **Chapter F — External Productivity Integrations** and
  **Chapter G — Operator-Facing Capabilities** (Phases 123+):
  operator-OAuth productivity tools (Gmail, Calendar, Drive,
  Notion, Obsidian, n8n) + the `aivyx-toolkit` bundle, plus the
  Discord / Slack / voice channels — each shipped as a
  third-party tool process or channel adapter, leaving the
  thirteen-tool substrate core untouched (DESIGN A12).
- **Phases 172–179** — the correction-signal learning loop
  (structural → LLM-judged → tool-attributed) and the
  **Aivyx PA Ralph loop** (a capped, gate-verified autonomous
  agent loop over an HMAC-chained backlog).

For the current point-in-time state (crate count, test count,
scope bases, domains), see [`../README.md`](../README.md). For
the per-phase narrative, see [`ROADMAP.md`](ROADMAP.md). The one
product-shape milestone still genuinely *open* is the **Channel
Activation Milestone** — operator verification across all
channels — intentionally held until the phase sequence settles.
