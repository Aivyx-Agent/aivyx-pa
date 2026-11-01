# Aivyx PA Roadmap

A living list of planned phases beyond the one currently active.

**This is not a contract.** Phase goals are revised at every phase
exit based on what the prior phase uncovered. Phase numbering is
loose — if Phase N's exit reveals that Phase N+2 should come before
Phase N+1, we reorder here and it costs one commit, not an amendment.

For the locked design contract, see [`../DESIGN.md`](../DESIGN.md).
For the current active phase, see the PHASE_N.md file listed in
[`README.md`](README.md). This document answers the question *"what
comes after the current phase?"* — nothing more.

## How this document is maintained

- At **every phase exit**, the outgoing phase's entry here is removed
  (it's now frozen in `PHASE_N.md`), and the next phase's one-line
  intent is refined with whatever the exiting phase learned.
- At **every phase entry**, a new `PHASE_N.md` is scaffolded from
  this document's entry for that phase, then the entry is replaced
  with a pointer like *"Active — see PHASE_N.md."*
- **Phase goals here are one paragraph maximum.** If an entry grows
  task lists or open questions, it has outgrown the roadmap and
  belongs in its own PHASE_N.md — which means that phase is probably
  ready to open.

## Backlog left open by the 2026-10-28 audit

Mediums P4-P7 and P17, and Lows P8-P12, P14, P18-P20 and P22, are fixed (see
[`audits/2026-10-28-code-audit.md`](audits/2026-10-28-code-audit.md)). What stays
open, and why:

- **P13 (part) — third-party actions are pinned by tag, not commit.** Pinning
  needs each action's commit looked up and a bump routine (Dependabot for
  Actions); left for a CI-maintenance pass. The token permissions and the
  `workflow_dispatch` input are fixed.
- **P19 (part) — why the 9B ends some post-tool steps with no text.** pa now
  says what ran instead of "try again", but the empty final message itself is
  the model's: the floor fires only on an empty or bare-object reply, and pa's
  provider layer does not read `reasoning_content`. Worth a live look with the
  llama-server logs before changing the provider.
- **P21 — a paraphrased injection ("Assistant note from the site owner: ...")
  passes the phrase tripwire.** A phrase list is a tripwire, not a defence; the
  fix is structural (P1/P16 made writes from fetched content ask), not a longer
  list. No change planned.
- **Studio bundle.** The approval card change (P4) and the `Set*Config` secret
  type (P14) are in `crates/aivyx-web`; `crates/aivyx-web/dist/` is rebuilt
  with `just build-web` at release time.

## Channel Activation Milestone — operator verification across all channels

**Status:** Scheduled after the Phase sequence closes. Not a
numbered phase.

The Channel Activation Milestone is a dedicated operator-
verification pass that runs **after** the architectural phase
sequence is complete. Its job is to take every channel adapter
that has shipped by that point (`aivyx-telegram` from Phase 8,
and whatever other adapters land in Phase 9+) and run each one
end-to-end against its **real** protocol, credentials, and
network — as a single coherent batch rather than as a per-phase
manual smoke test at each adapter's ship time.

**Why batched rather than per-phase:** manual operator smoke
tests each carry their own credential-juggling tax (BotFather
setup, chat_id discovery, env-var hygiene, real-network latency,
flaky CI hooks). Running them once at the end against a full
adapter matrix is cheaper than running them N times during the
phase sequence, and it catches **cross-adapter interaction**
bugs (e.g., a single audit chain written to by both a local turn
and a Telegram turn, `--verify-only` reporting the combined
count) that per-phase tests structurally can't.

**What the milestone contains:**

1. **Phase 8 Task 7** (deferred): real-bot Telegram smoke test —
   BotFather setup, `--channel telegram` launch, two-message
   persistent-memory round trip across a process restart,
   `--verify-only` forensic walk confirming the cross-restart
   audit chain is intact. The six-step runbook was drafted
   during the Phase 8 working session and will be re-scaffolded
   into the milestone doc when it opens.
2. **Any real-protocol smoke test** for channel adapters that
   ship during Phase 9 and later. Each future adapter brings
   its own scripted-transport unit test suite (like Phase 8
   `aivyx-telegram`'s `run_telegram_session_two_chats_persistent
   _e2e`) and **defers** its real-protocol verification to this
   milestone. **Discord (Phase 107)** is the first to defer here:
   real-bot setup at the Discord Developer Portal, `--channel
   discord` launch, two-message DM round trip across a process
   restart, the same `--verify-only` forensic walk Telegram
   uses. **Slack (Phase 108)** joins next — same shape with
   `xoxb-*` + `xapp-*` token setup at api.slack.com, Socket
   Mode connection establishment, two-message DM round trip
   across restart. Slack also brings two Phase-108-internal
   wirings that complete at the same milestone: the
   `SlackMorphismTransport` callback-state-passing via
   `SlackClientEventsUserState` (which can only be validated
   against live bot traffic), and the daemon-frontend
   `/approve` / `/reject` text-command gate-resolve routing
   bundled with the Phase 107 Discord deferral.
3. **Cross-channel regression sweep:** one local turn + one
   Telegram turn + one-of-each-other-adapter turn against the
   **same** persistent audit chain, then `aivyx-pa --verify-only`
   reporting a combined event count. This is the Phase 8 exit
   criterion rewritten to be N-channel rather than Telegram-
   specific.

The milestone is **not** a phase because it ships no code and
revises no architecture — it's a scheduled operator pass that
either passes (all channels live, cross-channel sweep green)
or produces a list of regressions that open as tickets against
the individual adapter crates. It runs when the Phase sequence
is complete enough that operator verification is worth the
setup cost, which is a judgement call to be made at the time.

## Phase 13 — Role-Config Migration (shipped)

**Frozen — see [PHASE_13.md](archive/phases/PHASE_13.md).** Opened and
exited 2026-04-15. Delivered **P9 — Per-Role Full
Capability Declaration** in four tasks: per-role envelope
fields in `aivyx-config` (Task 1, `2c7acfe`), binary
capability assembly rewritten to walk the declared
parent chain with backcompat-floor substitution per
empty level (Task 2, `af89874`), worked-example
`examples/aivyx-pa.toml` demonstrating the inheritance
primitive including the empty-child surprise case
(Task 3, `a19c6e4`), and a `--print-role` debug flag
for operator introspection of effective envelopes
(Task 4, `3e83422`). Phase 12 Task 3 `default role
config file` deferral closed directly. Workspace tests
rolled 453 → 480 (+27). DESIGN.md streak rolls to
thirteen; PRODUCT.md streak begins at one; production-
core `aivyx-core/src/lib.rs` streak re-establishes to
two (first re-established production-core streak since
Phase 10/11 held and Phase 12 broke it).

## Phase 14 — Sub-Agent Role-Switching (shipped)

**Frozen — see [PHASE_14.md](archive/phases/PHASE_14.md).** Opened and
exited 2026-04-16. Delivered the first concrete piece of
**PRODUCT.md P1 — Sub-Agent Mode via Role-Switching** in
four tasks: lift `assemble_role_envelope` from the
binary into `aivyx-channel/src/role_envelope.rs` (Task
1, `96814e7`, closing the first net-new Phase 13
deferral), add a `role.switch` capability scope with a
target-role `QualifierKind` plus `RoleSwitchTool`
registration in the core tool registry (Task 2,
`7364504`), wire sub-session nesting via an inline child
agent constructed by an `OnceLock`-backed factory closure
on `RoleSwitchTool` (Task 3, `74883e2`), and extend
`--print-role` with a mechanical reachable-switch-target
enumerator (Task 4, `91053ec`). Workspace tests rolled
480 → 509 (+29). All three byte-identity streaks held:
DESIGN.md → fourteen consecutive phases, PRODUCT.md →
two, production-core `aivyx-core/src/lib.rs` → three
(the at-risk streak the phase-open doc flagged for
Task 3 survived via an inline factory-closure path that
required zero `lib.rs` edits). The P1.3 "structural
impossibility of escalation" guarantee is pinned by
integration tests against narrowed-caps child snapshots
*and* by the debug-surface enumerator's structural-
impossibility test, which read from the same
`assemble_role_envelope`-produced `CapabilitySet`.

## Phase 15 — Channel-Lib Consolidation (shipped)

**Frozen — see [PHASE_15.md](archive/phases/PHASE_15.md).** Opened
and exited 2026-04-16 as the **first non-product-shape
sub-phase** in project history. Five tasks: open
commit (Task 1, `2d97cfd`), cross-crate integration
test against `examples/aivyx-pa.toml` closing the Phase
13 Task 3 cross-crate half (Task 2, `1cc94d6`),
renderer lift into `crates/aivyx-channel/src/role_
render.rs` picking up the Phase 14 Task 5 optional
cleanup and shrinking the binary 2706 → 2071 (−635)
(Task 3, `8afa00a`), per-tier worked example
`examples/aivyx-semitrusted.toml` +
`semitrusted_example_e2e.rs` closing the Phase 13
Task 3 per-tier-example half with a mechanical
teaching-comment correction caught inside the task
(D4 Rule 1 short-circuits before qualifier rules —
the original prediction that path-qualified
`fs.read:/tmp/notes/**` would survive
`CEILING_SEMITRUSTED` was wrong; the ceiling omits
the `fs.read` *base* entirely) (Task 4, `02d658d`),
and exit freeze (Task 5). Workspace tests rolled
509 → 519 (+10). All three byte-identity streaks
held, all extending: DESIGN.md → fifteen consecutive
phases, PRODUCT.md → three, production-core
`aivyx-core/src/lib.rs` → four (the longest
production-core run in project history, exceeding
the original Phase 10/11 baseline at its re-
establishment point). Phase 13 Task 3's three-part
deferral is now fully closed across Phase 14 Task 1
(lift), Phase 15 Task 2 (cross-crate test), and
Phase 15 Task 4 (per-tier example). Rolling backlog
10 → 8 items at exit. Only net-new deferral is a
~5-line doc-comment rewrite on
`CEILING_SEMITRUSTED`'s ▲-row wording (tagged with
the Phase 13 Task 4 reflexivity investigation as
the natural co-home). The lift pattern from Phase
14 Task 1 is now validated at a second, much larger
case (635 lines vs. 130); "lift private fns from
the binary into the channel lib" is a confirmed
reusable pattern rather than a one-shot trick.

## Phase 16 — Daemon Migration: Protocol Settlement (phase 1 of N) (frozen)

**Frozen — see [PHASE_16.md](archive/phases/PHASE_16.md).** Opened
and closed 2026-04-16 as the first phase of the
**Daemon Migration keystone** (P4 Daemon-Default
Architecture). Delivered: (1) `docs/DAEMON_IPC.md`
— the load-bearing IPC protocol specification
(length-prefixed JSON over Unix domain sockets,
`DaemonLifecycleEvent` as a separate message type,
OS-user auth per P4.4); (2) a PoC daemon server
(`daemon_server.rs`) + PoC client (`daemon_client.rs`)
+ one round-trip integration test proving the protocol
carries one turn end-to-end over a real Unix socket;
(3) three forward-investment helpers (`PROTOCOL_VERSION`
constant, `default_socket_path()`, `render_for_cli()`).
All six Q-block questions resolved: Q1→(a), Q2→(a),
Q3→(a), Q4→(a), Q5→(a), Q6→(b). **All four byte-
identity streaks held** — the open doc's prediction
that the production-core streak would "probably break"
was wrong; every mitigation argument held. Test delta
+14 (519→533). Zero new workspace dependencies.

## Phase 17 — Daemon Migration: Production Hardening (phase 2 of N) (frozen)

**Frozen — see [PHASE_17.md](archive/phases/PHASE_17.md).** Opened
and exited 2026-04-16 as the second phase of the
**Daemon Migration keystone**. Delivered: (1) multi-
turn daemon server with graceful shutdown via
`CancellationToken` (`daemon_server.rs`, Task 2);
(2) `daemon run` subcommand with `CliMode` enum
refactor and full agent-stack wiring (`aivyx.rs`,
Task 3); (3) multi-turn client library with
`DaemonSession` struct, `daemon_is_running` utility,
and `spawn_daemon_and_wait` auto-spawn logic
(`daemon_client.rs`, Task 4). All six Q-block
questions resolved: Q1→(a), Q2→(a), Q3→(b), Q4→(a),
Q5→(a), Q6→(c+). Closed 3 of 5 Phase 16 net-new
deferrals (production lifecycle, auto-spawn, `daemon`
subcommand). **All four byte-identity streaks held**
— the production-core streak at six consecutive
phases is the longest in project history. Test delta
+9 (533→542). Zero new workspace dependencies.

## Phase 18 — Daemon Migration: Frontend Wiring (phase 3 of N) (frozen)

**Frozen — see [PHASE_18.md](archive/phases/PHASE_18.md).** Opened
and exited 2026-04-16 as the third phase of the
**Daemon Migration keystone**. Delivered: (1) daemon-
backed REPL loop (`run_daemon_session` +
`run_daemon_session_connected` in `daemon_session.rs`)
with try-connect-then-spawn auto-attach and
`render_for_cli()` streaming output (Task 2);
(2) binary dispatch wiring — `ChannelKind::Local`
tries daemon mode first, falls back to in-process
— with `DaemonCancelHandle` for ctrl-C cancellation
over IPC (Task 3); (3) cancel-flag reset bug fix
replacing a local `bool` with a shared `Arc<AtomicBool>`
that the REPL loop resets before each turn (Task 4).
All four Q-block questions resolved: Q1→variant of (b),
Q2→(a), Q3→(a), Q4→(a). **All four byte-identity
streaks held** — DESIGN.md at eighteen, PRODUCT.md at
six, production-core at seven (longest in project
history), zero-new-dep. Test delta +4 (542→546).
Closed the REPL-mode-over-IPC deferral from Phase 17.

## Phase 19 — Daemon Migration: Multi-Connection + Telegram Port (phase 4 of N) (frozen)

Daemon Migration phase 4. Multi-connection daemon
server (task-per-connection with `ChannelFactory`),
`FrontendType` enum + `StartSession` protocol
extension, Telegram adapter ported behind IPC boundary
with daemon-first + in-process fallback. Transport
types widened to `pub`. Binary line-count extraction
into `telegram_daemon_frontend.rs`. All three byte-
identity streaks held (DESIGN.md at 19, PRODUCT.md at
7, production-core at 8). Zero-new-dep. Test delta +4
(546→550). Closed the Telegram-over-daemon deferral
from Phase 16.

## Phase 20 — Daemon Management + Deferral Cleanup (frozen)

**Frozen — see [PHASE_20.md](archive/phases/PHASE_20.md).** Non-product-
shape cleanup phase (same category as Phase 15). Closed
six of sixteen rolling deferrals: `daemon status`/`stop`
subcommands (Phase 17), PID file (Phase 17), `--no-daemon`
flag (Phase 18), daemon-mode banner parity (Phase 18),
`CapabilitySet::grants` reflexivity investigation (Phase 13),
and `CEILING_SEMITRUSTED` ▲-row doc-comment rewrite
(Phase 15). Rolling backlog 16 → 10. Zero net-new deferrals.
Test delta +19 (550→569). All three byte-identity streaks
held: DESIGN.md at twenty, PRODUCT.md at eight, production-
core at nine (new record).

## Phase 21 — Mission Primitive (P2) (frozen)

**Frozen — see [PHASE_21.md](archive/phases/PHASE_21.md).** Delivered the
first concrete piece of **PRODUCT.md P2 — Mission Primitive**
in eight tasks: mission state model with six-state machine +
16 unit tests (Task 3), `mission.create` and `mission.gate`
capability scopes with tier ceilings (Task 4), IPC protocol
extensions for gates and mission lifecycle (Task 5),
`MissionCreateTool` with OnceLock factory pattern + daemon
`ResolveGate` handler (Task 6), CLI interactive gate prompt
and Telegram `/approve`/`/reject` text commands (Task 7).
Five design decisions, five Q-block questions resolved. All
three byte-identity streaks held: DESIGN.md at twenty-one,
PRODUCT.md at nine, production-core at ten (new record). Test
delta +29 (569→598). Rolling backlog 10 → 12 (+2 net-new:
escalation→gate turn-loop wiring, mission list/status tools).

## Phase 22 — Contract Refresh (frozen)

**Frozen — see [PHASE_22.md](archive/phases/PHASE_22.md).** Opened and
exited 2026-04-17. First-ever contract amendment batch: four
amendments to `DESIGN.md` (Daemon IPC Protocol, Mission State
Machine, Capability Taxonomy Growth, Workspace Layout),
Delivery Status section for `PRODUCT.md` mapping all 12
commitments to implementation state, and milestone refresh
for `PRODUCT_ROADMAP.md` adding four new milestones (MCP
Integration, Multi-Provider, Web UI Channel, Scheduled
Execution). **Docs-only phase — zero code changes.** Both
long-running byte-identity streaks intentionally ended through
the formal amendment process: DESIGN.md at twenty-one phases,
PRODUCT.md at nine. Production-core streak extended to eleven
(new record). Test count unchanged at 598. Three Q-block
questions resolved. Zero net-new deferrals.

## Phase 23 — Escalation→Gate Wiring + MCP Foundation (frozen)

**Frozen — see [PHASE_23.md](archive/phases/PHASE_23.md).** Mixed phase:
(1) escalation→gate turn-loop wiring closing the Phase 21
deferral — `SubmitInput.mission_id` + daemon-side gate
creation + resume turn on approval (Task 2); (2) MCP client
adapter foundation — new `aivyx-mcp` crate (11th workspace
member), stdio transport, `McpServerBridge` + `McpToolProxy`,
`mcp.call` scope base in `aivyx-capability` (Task 3). Three
Q-block questions resolved. Production-core streak extends
to thirteen (new record). Test delta +10 (598→608). Rolling
backlog 12→13 (−1 closed, +2 net-new: MCP config surface,
MCP SSE transport).

## Phase 24 — MCP Integration: Config + Binary Wiring (frozen)

**Frozen — see [PHASE_24.md](archive/phases/PHASE_24.md).** Opened and
exited 2026-04-17 as the second MCP Integration phase.
Five tasks: `[[mcp_server]]` TOML config entries with
`McpServerConfig` struct + disabled-server filtering (Task 2,
`b8228e8`), daemon-side MCP bridge lifecycle with eager
startup + `kill_on_drop` safety net (Task 3, `b847bb5`),
workspace layout amendment addendum for the 11-crate reality
(Task 4, `d226ad1`), and `--mcp-server` repeatable CLI flag
with `splitn(3, ':')` format (Task 5, `53ce7bc`). Closed the
Phase 23 MCP config surface deferral. DESIGN.md edited for
amendment addendum (A4 workspace layout: 10→11 crates, 23→24
known bases). PRODUCT.md unchanged. Production-core streak
extends to fourteen consecutive phases (new record). Test
delta +9 (608→617). Rolling backlog 13→12 (−1 closed, 0
net-new). Two Q-block questions resolved.

## Phase 25 — Multi-Provider Support (frozen)

**Frozen — see [PHASE_25.md](archive/phases/PHASE_25.md).** Opened and
exited 2026-04-17. OpenAI-compatible `LlmProvider` adapter
(`provider-openai` feature in `aivyx-llm`) with `OpenAiProvider`
implementing `stream_turn` for `/v1/chat/completions`. Shared
`HttpTransport` seam lifted to crate root. Config + CLI wiring:
`ProviderKind` enum, `--provider` flag, `AIVYX_PA_PROVIDER` env,
`[agent] provider` + `[openai]` TOML sections. Provider-aware
`validate()`. PRODUCT_ROADMAP Multi-Provider milestone delivered.
DESIGN.md unchanged. PRODUCT.md unchanged. Production-core
streak extends to sixteen. Test delta +18 (617→635). Two Q-block
questions resolved. One new deferral (provider-specific token
counting).

## Phase 26 — Scheduled Execution: Timer Primitives (frozen)

**Frozen — see [PHASE_26.md](archive/phases/PHASE_26.md).** Opened and
exited 2026-04-17. First phase of the Scheduled Execution
milestone (PRODUCT.md G5). Delivered: `KeyDomain::Schedules`
(7th encrypted storage domain), `ScheduleRecord` CRUD with
`cron` crate 7-field validation, `[[schedule]]` TOML config
surface with `ScheduleConfig`, daemon scheduler loop
(`daemon_scheduler.rs`) — adaptive-tick background task with
deduplication and TOML-to-storage sync, four agent-facing
tools (`schedule.create`, `.list`, `.delete`, `.update`) gated
to `CEILING_TRUSTED`. Production-core streak extends to
seventeen consecutive phases (new record). DESIGN.md and
PRODUCT.md both untouched. Test delta +17 (643→660). Rolling
backlog 13→14 (+1 net-new: automatic mission wrapping for
scheduled turns).

## Phase 27 — Scheduled Execution Phase 2: Webhook Triggers + File Watchers (frozen)

**Frozen — see [PHASE_27.md](archive/phases/PHASE_27.md).** Opened and
exited 2026-04-18. Completed **PRODUCT.md G5 — Autonomous and
Scheduled Execution** by adding webhook triggers (localhost-only
hyper HTTP/1.1 listener on 127.0.0.1:7842) and file-change
watchers (`notify` crate, cross-platform). Unified all trigger
sources under `TriggerDispatch` with shared `Mutex<()>` turn
serialization. Closed the Phase 26 automatic-mission-wrapping
deferral with opt-in `wrap_mission = true` on all trigger configs.
Nine storage domains, 27 known capability bases. Production-core
streak extends to eighteen consecutive phases (new record).
DESIGN.md and PRODUCT.md both untouched. Test delta +30 (660→690).
Two new Cargo.lock entries: httpdate (transitive), notify (direct).

## Phase 28 — Deferral Cleanup + Reflection Foundation

**Frozen** (`2cd41d9`). Hybrid phase: closed two formal deferrals
(`mission.list`/`mission.status` tools, forensic
`ToolOutcome::NotInRole`) plus webhook port configurability
(Phase 27 decision item). Shipped the first Reflection Layer
primitive — `turn.history` audit introspection tool. Backlog
13 → 11. Production-core streak broken at 18 (expected — NotInRole
variant). DESIGN.md and PRODUCT.md untouched. Test delta +7
(690→697).

## Phase 29 — Agent Reflection Loop

**Frozen** (`03a804b`). Completed the Reflection Layer (memory-only
scope of PRODUCT.md G3 / P8). Delivered `reflection.propose` and
`reflection.apply` tools: the agent reads its own recent outcomes
via `turn.history`, proposes memory adjustments, and surfaces them
through mission gates for operator approval. Deferral backlog
unchanged at 11. DESIGN.md, PRODUCT.md, and production-core all
untouched. Test delta +4 (697->701).

## Phase 30 — Runtime Role Mutation (P8 completion) (frozen)

**Frozen — see [PHASE_30.md](archive/phases/PHASE_30.md).** Completed
PRODUCT.md P8 — Outcome-Driven Audited Reflection. Added
`RoleOverrides` struct with prompt appendix and allowlist
add/remove, `role.update` tool and capability base, planner
factory integration reading overrides per-turn, and extended
`reflection.apply` for role mutations. Full
observe-propose-approve-apply cycle now operational for both
memory writes and runtime role-config changes. PRODUCT.md P8
and G3 delivery status updated. Production-core streak extends
to 2. Test delta +9 (701->710). Deferral backlog unchanged
at 11.

## Phase 31 — Deferral Cleanup (frozen)

**Frozen** (`7f16f7a`). Non-product-shape cleanup phase.
Closed 3 of 11 rolling deferrals: content-type in web.fetch
output (Phase 12 Q3), provider-reported token usage via
`TokenUsage` in `TurnEnded` audit event (Phase 25), SemiTrusted
regression tests for role primitive (Phase 11 Q6). Stretch goals
(MCP SSE, multi-level nesting) not attempted — better scoped as
own phase. DESIGN.md untouched. PRODUCT.md untouched (streak 1).
Production-core broke at Task 3 (TokenUsage struct). Test delta
+3 (710->713). Deferral backlog 11 -> 8.

## Phase 32 — MCP SSE Transport (frozen)

**Frozen** (`4aabea8`). HTTP/SSE transport for remote MCP
servers. `McpTransport` trait extraction, `SseTransport`
implementation, `McpTransportKind` config enum, `--mcp-sse`
CLI flag. Channel-backed integration tests. Closed Phase 23
MCP SSE deferral, completing the MCP Integration milestone.
DESIGN.md and PRODUCT.md untouched. Production-core streak
extends to 1. Test delta +23 (713→736). Deferral backlog
8→7.

## Phase 33 — Multi-Level Sub-Agent Nesting (frozen)

**Frozen** (`d02dda6`). Closed the Phase 14 Task 3 deferral:
multi-level sub-agent nesting. Config change granting
`researcher` `role.switch:junior_researcher`, plus 4
integration tests proving capability-bounded recursion
terminates correctly. Zero code changes to the architecture.
DESIGN.md, PRODUCT.md, and production-core all untouched.
Test delta +4 (736→740). Deferral backlog 7→6.

## Phase 34 — Ollama Foundation (frozen)

**Frozen** (`0004cf2`). First-class local LLM experience via
Ollama. `ProviderKind::Ollama` config sugar, optional API
key, graceful `stream_options` handling, connection health
check with actionable errors, worked example config.
DESIGN.md, PRODUCT.md, and production-core all untouched.
Test delta +16 (740→756). Deferral backlog unchanged at 6.

## Phase 35 — P2 Completion + Delivery Status Refresh (frozen)

**Frozen** (`5786449`). Wired `ToolOutcome::RequiresEscalation`
through the turn loop to `TurnOutcome::Escalated`, activating
the daemon's existing gate-creation handler. Trigger path
also creates gates for escalated missions. Refreshed
PRODUCT.md Delivery Status from Phase 21 to Phase 35. P2
moved to Fully Delivered. DESIGN.md and production-core
untouched; PRODUCT.md edited (intentional). Test delta +1
(756→757). Deferral backlog unchanged at 6.

## Phase 36 — Ollama Model Management (frozen)

**Frozen** (`1f726f9`). Three agent-facing tools
(`ollama.list`, `ollama.show`, `ollama.pull`) for inspecting
and managing local Ollama models. Tools in `aivyx-channel`,
Trusted-only ceiling, `reqwest` directly (not through
`HttpTransport`), conditional registration when
`provider = "ollama"`. Completes the local LLM story started
in Phase 34. DESIGN.md, PRODUCT.md, and production-core all
untouched. Test delta +14 (757→771). Deferral backlog
unchanged at 6.

## Phase 37 — WebFetchTool Hardening

**Frozen.** Closed all three web-fetch deferrals: binary
response bodies (base64 fallback), non-GET verbs (new
`WebPostTool` with `net.post` scope), and redirect following
(opt-in manual loop with per-hop scope re-check). Cuts
deferral backlog from 6 to 3. DESIGN.md and PRODUCT.md
untouched; lib.rs touched (pub-use re-export). Test delta
+17 (771→788).

## Phase 38 — Foundation Audit

**Frozen.** Post-37-phase health check: Amendment A5 (P10
substrate tool count 7→8 for `web.post`), clippy warning
cleanup (13→0), PRODUCT.md delivery status refresh
(Phase 35→38), deferral backlog review (3→1, only protocol
versioning remains). DESIGN.md and lib.rs untouched;
PRODUCT.md touched. Test count unchanged at 788.

## Phase 39 — Web UI Channel (Phase 1: Chat Interface)

**Frozen — see [PHASE_39.md](archive/phases/PHASE_39.md).** First phase of the
Web UI Channel milestone. Added a localhost-only web chat interface
(`127.0.0.1:7843`) that connects to the daemon over the existing
IPC protocol via WebSocket. `FrontendType::Web` variant,
`WebDaemonChannel` stub, `tokio-tungstenite` WebSocket bridge,
embedded HTML/CSS/JS frontend with streaming text, tool-call
cards, and approval-gate buttons. `--web-ui` CLI flag and
`[daemon] web_ui` config. 801 tests, zero clippy warnings.
DESIGN.md untouched (streak 16), PRODUCT.md untouched (streak 2),
lib.rs untouched (streak 3).

## Phase 40 — Parallel Tool Execution

**Shipped — see [PHASE_40.md](archive/phases/PHASE_40.md).** Resolves the D1
deferred concurrency decision (line 67) via Amendment A6.
Implements batch tool dispatch: `LlmStepEnd::ToolCalls` surfaces
all tool-use blocks from providers, `NextStep::ToolCalls` carries
batches through the planner, and the turn loop dispatches them
concurrently via `futures::future::join_all`. Anthropic serializer
groups consecutive `ToolResult` entries into one user message.
Step accounting: batch=1 step, N tool_calls. Breaks DESIGN.md
streak (A6) and lib.rs streak (core changes).

## Phase 41 — Daemon Hardening & Error Typing

**Shipped — see [PHASE_41.md](archive/phases/PHASE_41.md).** Hardening phase:
`DaemonConfig` parameter-object refactor (10 params → 1 struct),
`DaemonError` thiserror enum replacing ~30 `Result<_, String>`
signatures across 5 files, crash-recovery metadata (`daemon.state`
+ `StateGuard` RAII + `RecoveryNotice` lifecycle event), and
protocol version negotiation (`ProtocolNegotiation`/`Accepted`/
`Rejected` messages, v0.1 always-accept). DESIGN.md amendment A7
filed for protocol negotiation. Closes sole remaining deferral
(backlog 1 → 0). 814 tests, zero clippy warnings. DESIGN.md
touched (A7), PRODUCT.md untouched (streak 5), lib.rs untouched
(streak 2).

## Phase 42 — Shell Hardening & Memory GC (frozen)

**Frozen — see [PHASE_42.md](archive/phases/PHASE_42.md).** Closed two
operational safety gaps: zombie grandchildren (process-group
execution with SIGTERM→SIGKILL on timeout) and unbounded
memory growth (per-topic cap enforcement, TTL-based expiry,
hourly daemon GC timer). Shell env isolation prevents API
key leakage to LLM-generated commands. Agent-invocable
`memory.gc` tool in channel layer (35 known bases). DESIGN.md,
PRODUCT.md, and production-core all untouched. Test delta +25
(814→839). Deferral backlog unchanged at 0.

## Phase 43 — Context Window Management ✓ FROZEN

Token counting (`chars.div_ceil(4)` heuristic), 80%-budget
history pruning in `LlmPlanner::next_step`, `PruneSink` trait
for persisting pruned summaries to memory, `TokenUsage`
extended with `context_tokens_before/after_pruning`. Per-provider
context window defaults (200k/128k/8k). 857 tests, zero clippy.
DESIGN.md streak → 2, PRODUCT.md streak → 7, lib.rs streak broken.

## Phase 44 — `aivyx-pa init` Interactive First-Run Wizard (shipped)

Interactive setup wizard for non-technical end users. Detects
Ollama locally (zero API key path), walks through provider/model
selection, writes `aivyx-pa.toml` with 0600 permissions. 876 tests,
zero clippy. DESIGN.md streak → 3, PRODUCT.md streak → 8,
lib.rs streak → 1.

## Phase 45 — Rich Input (Multimodal Messages) [SHIPPED]

`ContentBlock` enum with `Text` and `ImageBase64` variants.
`LlmMessage::User` carries `Vec<ContentBlock>`. Anthropic and
OpenAI providers serialize image content blocks. `MessageContent`
gains `Image` and `Mixed` variants. CLI `/image <path>` command.
Telegram photo extraction via Bot API. Daemon IPC carries
attachments with `#[serde(default)]` backwards compat. 893 tests,
zero clippy. DESIGN.md streak -> 4, PRODUCT.md streak -> 9,
lib.rs streak -> 0.

## Phase 46 — Web Search + Document Retrieval (Bundled MCP) [SHIPPED]

First bundled MCP server: `aivyx-pa mcp-server web-search`. Two
tools: `web_search` (query → results) and `web_read` (URL →
cleaned text). Search backend hierarchy: Brave Search API →
SerpAPI → DuckDuckGo HTML scraping (zero-config fallback).
Self-spawning embedded server with `bundled = true` config flag.
Init wizard integration. 936 tests, zero clippy. DESIGN.md
streak → 5, PRODUCT.md streak → 10, lib.rs streak → 1.

## Phase 47 — Web UI Phase 2 (Mission Dashboard + Audit Viewer) [SHIPPED]

**Frozen — see [PHASE_47.md](archive/phases/PHASE_47.md).** Extended the daemon
IPC protocol with a read-only `Query`/`QueryResponse` envelope and
shipped four backend queries (`ListSessions`, `ListMissions`,
`GetMission`, `ListAuditEntries`, `VerifyAuditChain`). Added
`HmacChainLog::entries_range` for paginated audit reads. Rebuilt
`web_ui_static.html` as a tabbed SPA over the existing WebSocket
bridge. 948 tests, zero clippy warnings. DESIGN.md streak → 6,
PRODUCT.md streak → 11, lib.rs streak → 2. Three net-new
deferrals: live audit push, read-write inspection from the
dashboard, `handle_connection` parameter-struct lift.

## Phase 48 — Channel Adapter SDK & Documentation (P5 + P11) [SHIPPED]

**Frozen — see [PHASE_48.md](archive/phases/PHASE_48.md).** Shipped
`docs/CHANNEL_SDK.md` (the v0 third-party contract document),
extended `docs/ADAPTER_PATTERN.md` with an out-of-tree section,
and built `examples/python-channel/` — a stdlib-only Python
reference adapter with a 15-test conformance suite that runs
daemon-free. 948 Rust tests unchanged, 15 new Python tests,
zero clippy warnings. All three streak predictions held:
DESIGN.md → 7, PRODUCT.md → 12, `aivyx-core/lib.rs` → 3.
Delivers PRODUCT.md P5 + P11 — only P12 (Tool Process IPC)
remains as a forward commitment.

## Phase 55 — MCP Server Sandbox Layer [SHIPPED]

**Frozen — see [PHASE_55.md](archive/phases/PHASE_55.md).** First post-Chapter-A
phase. Ported the Phase 52 generic command-wrapper sandbox from
`[[tool_process]]` to `[[mcp_server]]`, closing THREAT_MODEL.md
§5.2. New `aivyx-mcp::SandboxConfig` parallel to
`aivyx-tool::SandboxConfig` (no new dep edges between adapter
crates). `StdioTransport::start` gained an `Option<&SandboxConfig>`
parameter; `McpServerBridge::start_with_sandbox` is the new
entry point. `[mcp_server.sandbox]` TOML schema parallel to
`[tool_process.sandbox]`. Loader rejects sandbox declared on
SSE transport (no local child to wrap). 992 Rust tests (+8),
zero clippy. All three streak predictions correct (DESIGN.md
→ 2, PRODUCT.md → 6, lib.rs → 4). Demonstrates the post-
Chapter-A posture: operator-feedback-shaped trigger +
proven-pattern port + small task list.

## Phase 56 — Profile + Persona Amendments (P13 + P14)

**Scheduled** — docs-only phase, same shape as Phase 22 (the
contract-refresh precedent). Files two PRODUCT.md amendments:
**P13 — Assistant Profile** (operator-declared static
identity layer: name, operator_profile, communication_style,
primary_use_cases, preferences, constraints) and **P14 —
Persona** (reflection-written dynamic identity layer:
seed_pointer to Profile, learned_context,
communication_adaptations, character_traits,
relationship_milestones, delta_log). Updates the PRODUCT.md
pitch from "personal autonomous agent platform" to the
operator's vision statement
("self-learning, self-improving AI-personal assistant with
a user-defined Profile and Persona based on the end-user
use-case"). Adds Forward milestones to PRODUCT_ROADMAP.md.
Adds Phase 57–60 entries to ROADMAP.md (this commit
provides the entries themselves). Zero code changes. Expected
streak break: PRODUCT.md streak ends by amendment (same
precedent as Phase 22). DESIGN.md and lib.rs streaks should
hold.

## Phase 57 — Profile Foundation [SHIPPED]

**Frozen — see [PHASE_57.md](archive/phases/PHASE_57.md).** First code
phase of the Profile + Persona arc. Delivered the Profile
substrate per PRODUCT.md P13:

- `aivyx-config::Profile` struct with six P13-commit-5
  fields (`assistant_name`, `operator_profile`,
  `communication_style`, `primary_use_cases`,
  `behavioral_preferences`, `behavioral_constraints`).
- `[profile]` TOML table per Q1(a) (plain-text-
  inspectable, in `aivyx-pa.toml` alongside roles).
- `Profile::default()` synthesizing Q5(b) fallback
  (`assistant_name = DEFAULT_ASSISTANT_NAME`, all else
  empty) — every pre-Phase-57 `aivyx-pa.toml` keeps working
  unchanged.
- `aivyx-channel::assemble_session_prompt` helper composing
  Profile + role envelope into a labeled system prompt per
  Q3(c) (*"## About this assistant"* + *"## Active role:
  <name>"* layout), with passthrough behavior for legacy
  configs where no operator content is declared.
- Wiring through both the parent session-build path
  (`run_async`'s `system_prompt` local) and the role-switch
  child factory (`profile_for_factory` capture), so
  sub-sessions inherit the same Profile section as their
  parent.
- `aivyx-pa init` extended with three opt-in Profile prompts
  per Q4(c) (assistant name, primary use case,
  communication style); `render_toml` emits `[profile]`
  only when the operator customized at least one field.
- Startup-banner `profile` row surfacing assistant_name
  provenance and a count of operator-declared extras.

All six Q-block questions resolved with the recommended
defaults. Tests +14 across aivyx-config and aivyx-channel
(992 → 1006). Zero clippy warnings. All three streak
predictions correct: DESIGN.md → 4, PRODUCT.md → 1, lib.rs
→ 6 (longest run since the Phase 51 deliberate break at 6).
Phase 58 (Inspection) is next.

## Phase 58 — Profile Inspection (closes P13) [SHIPPED]

**Frozen — see [PHASE_58.md](archive/phases/PHASE_58.md).** Operator-facing
inspection and edit surface that closes the Assistant Profile
milestone. After Phase 58, **P1–P13 are all fully shipped**;
only P14 (Persona, Phases 59–60) remains forward.

- `aivyx-pa profile show` (CLI) — reads `aivyx-pa.toml` via the
  existing config loader path, renders the resolved Profile
  in labeled banner-style format per Q3(a). Works whether
  the daemon is running or not.
- `aivyx-pa profile edit` (CLI) — opens the `[profile]`
  section in `$EDITOR` against a tempfile, merges back via
  `toml_edit` surgical update per Q2(a) preserving every
  other section and every comment in `aivyx-pa.toml`. Prints
  a `aivyx-pa daemon stop && aivyx-pa` restart reminder on save
  per Q5(a) load-time-only semantics.
- `CliMode::Profile(ProfileSubcommand)` nested enum per
  Q1(a); five new parser tests cover happy paths and
  error cases.
- Web UI Profile pane via new `Query::GetProfile` IPC
  envelope + `ProfileSummary` wire-shape per Q4(a)
  read-only. Mirrors the show CLI output with an
  injection-status banner.
- PRODUCT.md Delivery Status refreshed (P13 → Fully
  Delivered, Task 5 streak-break).

`toml_edit = "0.22"` is the first new workspace crate
added since Phase 27's `notify`. Tests +17 across the
phase (1006 → 1023), zero clippy warnings. All three
streak predictions correct: DESIGN.md → 5 (untouched),
PRODUCT.md → broke at 2 (Task 5, intentional), lib.rs
→ 7 (untouched). Phase 59 (Persona Foundation) is next.

## Phase 59 — Persona Foundation [SHIPPED]

**Frozen — see [PHASE_59.md](archive/phases/PHASE_59.md).** First code phase
of the Persona half of the Profile + Persona arc. Delivered
the Persona substrate per PRODUCT.md P14 across six Q-block
resolutions:

- **Q1(a) — one field-edit per delta.** Fine-grained
  `PersonaDelta` records with `PersonaDeltaCategory` (10
  variants: 6 Profile-mirror + 4 Persona-specific) and
  `PersonaDeltaOp` (SetScalar / AppendList / RemoveList).
  `(category, op)` pair validated at append time.
- **Q2(a) — parallel HMAC chain.** New `KeyDomain::Persona`
  (10th storage domain). `PersonaChainLog` in-memory
  primitive + `PersistentPersonaLog` storage wrapper.
  Distinct `PERSONA_GENESIS_SEED` from the audit chain so a
  chain-confusion attack (swapping entries between chains)
  is structurally rejected.
- **Q3(c) — Profile-mirror + Persona-specific.** Operator
  can refine Profile fields OR add Persona-specific
  content (LearnedContext, CommunicationAdaptations,
  CharacterTraits, RelationshipMilestones).
- **Q4(a) — extend reflection.propose.** Schema gains a
  `persona_deltas` array; `required_scope` escalates to
  `persona.propose` when the array is non-empty.
  `ProposalRecord` round-trips deltas through the mission
  description.
- **Q5(a) — shared-state hot-reload (deferred to Phase 60).**
  `SharedEffectivePersona = Arc<RwLock<EffectivePersona>>`
  with `apply_delta_to_shared` helper. `reflection.apply`
  writes both the persistent chain and the shared state.
  Per-turn planner-factory re-call (full hot-reload UX)
  defers to Phase 60 alongside the operator-facing
  dashboards. Phase 59 ships snapshot-at-session-build
  freshness — operators who approve mid-session see effect
  on next daemon startup (Profile-shaped semantics).
- **Q6(a) — three labeled sections.**
  `assemble_session_prompt` gains an
  `Option<&EffectivePersona>` parameter; renders
  "## How I have learned to communicate" between Profile
  and Active role when Persona is non-empty.

New `persona.propose` capability scope in
`aivyx-capability::KNOWN_BASES` + `CEILING_TRUSTED`. Three
direct deps added on aivyx-channel (`hmac`, `sha2`,
`serde_jcs` — already transitive). Tests +29 across the
phase (1023 → 1052). Zero clippy warnings. All three
streak predictions correct: DESIGN.md → 6 (untouched),
PRODUCT.md → 1 (untouched), lib.rs → 7 (untouched —
the "at risk" prediction resolved cleanly because the
shared-state plumbing went through aivyx-channel + the
existing planner factory pattern without touching core).
Phase 60 (Persona Visualization) is next.

## Phase 60 — Persona Visualization (closes P14 + ledger) [SHIPPED]

**Frozen — see [PHASE_60.md](archive/phases/PHASE_60.md).** Closes the
Profile + Persona forward arc and the entire PRODUCT.md
forward-commitment ledger. Delivered across five Q-block
resolutions:

- **Q1(c) — nested CLI enum.** New `CliMode::Persona(PersonaSubcommand)`
  with `Show` / `List` / `Revert { target_delta_id }`
  variants. `aivyx-pa persona show` prints the effective
  state; `list` prints the chain; `revert` operator-
  initiated undo.
- **Q2(a) — direct closure capture.** Per-turn planner-
  factory refresh threaded through three sites (parent
  run_session, daemon-run path, role-switch child factory)
  via captured Arc<RwLock<EffectivePersona>> clones reading
  per-turn. Closes the Phase 59 Q5(a) hot-reload deferral.
- **Q3(a) — Web UI click-to-revert.** New Persona tab
  rendering the effective state + delta timeline with
  per-entry Revert buttons. `FrontendMessage::RevertPersonaDelta`
  via the existing WebSocket bridge;
  `DaemonMessage::PersonaRevertResolved` reply.
- **Q4(a) — Revert as a chain op.** New
  `PersonaDeltaOp::Revert { target_delta_id }` variant.
  Folder consults the chain and applies the inverse of
  the target's op; revert-of-revert restores the original
  effect (recursive resolution, termination guaranteed by
  strictly-decreasing index).
- **Q5(a) — operator-only reverts.** Agent cannot propose
  Revert via `reflection.propose`; reverts come from the
  CLI / Web UI surfaces and are auto-approved (no gate
  prompt since the operator is the proposer).

Other surface: `Query::GetEffectivePersona` +
`Query::ListPersonaDeltas` IPC envelopes;
`EffectivePersonaSummary` + `PersonaDeltaSummary` wire
types; `recompute_shared_from_entries` (replaces the Phase
59 single-delta `apply_delta_to_shared` because Revert
needs chain context); 19 new tests (7 revert folder + 4
render + 8 parser); zero clippy warnings. **Tests
1052 → 1071.** All three streak predictions correct:
DESIGN.md → 7 (untouched), PRODUCT.md → broke at 2 (Task 7
Delivery Status refresh, intentional), lib.rs → 8
(untouched).

**Identity export/import** is the one deferred piece — P14
permits it and Phase 60 flagged it optional. Re-binding the
HMAC chain on import is the load-bearing question; lands as
a focused future micro-phase if operator pressure surfaces.

After Phase 60 the project sits at every PRODUCT.md
commitment (P1–P14) fully delivered, identity layer fully
shaped, and the forward-commitment ledger closed. Future
numbered phases land in response to operator feedback or as
amendment-introduced commitments.

## Phase 61 — Distribution: Release Pipeline (Pipeline Ready, Publication Held)

**Frozen — see [PHASE_61.md](archive/phases/PHASE_61.md).** First phase past
the closed forward-commitment ledger. Operator-feedback-shaped
work on the post-Phase-60 substrate-ergonomics axis, opening
the **Distribution Milestone** (phase 1 of N).

Delivered across six engineering tasks + one mid-phase fixup:

- **Task 2 — `aivyx-pa --version` / `-V` CLI flag.** New
  `CliMode::Version` variant in the hand-rolled parser; prints
  `aivyx-pa <CARGO_PKG_VERSION>` and exits 0 without touching the
  config loader, storage layer, or daemon socket. Three parser
  tests (long, short, extra-args rejection).
- **Task 3 — `cargo-dist` initialization.** Q1(a) sign-off:
  cargo-dist v0.31.0 (binary name `dist`) over hand-rolled CI.
  `dist init` generates `dist-workspace.toml` + `.github/
  workflows/release.yml`. Two cleanups on top of the bare
  output: target matrix trimmed to Q2(a) musl-static + macOS
  only (no gnu variants, no `x86_64-pc-windows-msvc`); and
  `aivyx-tool` marked `[package.metadata.dist] dist = false`
  to exclude the `fs_read_subprocess_fixture` test binary
  from release artifacts.
- **Task 4 — CI gates.** Three workflow files for defense in
  depth: `quality-gate.yml` (reusable `workflow_call`,
  `cargo clippy --workspace --all-targets -- -D warnings` +
  `cargo test --workspace`), `ci.yml` (calls quality-gate on
  every push to main and every PR), and a `plan-jobs =
  ["./quality-gate"]` entry in `dist-workspace.toml` that
  wires the same gate into the release pipeline's `plan` job
  — every release-pipeline artifact transitively depends on
  the gate.
- **Task 5 — README rewrite.** Status table refreshed
  (Phase 54 → Phase 61 numbers); Five-minute setup quickstart
  framing prepared for prebuilt binaries.
- **Task 6 — `docs/INSTALL.md`.** New file. Full install
  matrix: supported targets, build-from-source path, where
  files land, first-run checklist, uninstall.
- **Task 5/6 fixup — pipeline-ready framing.** Mid-phase
  the operator pivoted to a "VPS-private-first,
  GitHub-later" posture. Tasks 5 and 6 had landed a
  "v0.1.0 published" framing that was no longer true; the
  fixup reverted README's Five-minute setup to the
  build-from-source primary path and reframed `docs/
  INSTALL.md` with a "Current install state" disclaimer +
  shell-installer-as-forward-looking.

**Task 7 deferred indefinitely.** Phase 61's stated goal at
open was to cut `v0.1.0` on GitHub Releases. Under the
VPS-first pivot, the operator-side publication step (create
public repo, push history, tag `v0.1.0`) is held until
public hosting is configured. The release substrate is
in place and fires the first time a `v*.*.*` tag is pushed
to a public GitHub remote — a focused micro-phase
(numbered later) ships the actual `v0.1.0` when ready.

All three streak predictions correct: DESIGN.md → 8,
PRODUCT.md → 1 (recovered after Phase 60's intentional
break), `aivyx-core/src/lib.rs` → 9 (new record, beating
Phase 60's 8). Test count +3 (1071 → 1074) — Task 2's
three `--version` parser tests. Zero clippy warnings. One
net-new deferral (Task 7 — v0.1.0 publication).

## Phase 62 — Reach: Agent-Initiated Outbound Notifications

**Frozen — see [PHASE_62.md](archive/phases/PHASE_62.md).** Second phase past
the closed forward-commitment ledger and the first phase of the
new **Reach Milestone**. Gives the agent a `notify.send`
infrastructure tool that pushes a message to an
operator-configured `[[notify_target]]` (Telegram chat or
generic webhook URL). Transforms Aivyx PA from purely reactive
("I talk to it") to proactive ("it can wake my phone").

Delivered across seven engineering tasks:

- **Task 2 — `notify.send` capability scope** in
  `aivyx-capability`: new base in KNOWN_BASES with
  `<target_name>` qualifier (matches `role.switch:<name>`
  precedent), included in CEILING_TRUSTED only. Wildcard form
  `notify.send:*` rejected at parse time alongside
  `role.switch:*` — same Rule 2 rationale.
- **Task 3 — `[[notify_target]]` TOML config surface** in
  `aivyx-config`: `NotifyTargetConfig` + `NotifyTargetKind`
  enum making invalid kind/field combinations unrepresentable
  at the runtime layer; load-time validations for unique
  names, non-empty fields, kind-required fields, and
  http(s)-scheme URLs.
- **Task 4 — `NotifyDispatcher` + `NotifyBackend` trait** in
  `aivyx-channel`: async trait with `send(message, subject)
  -> Result<(), NotifyError>` and a `kind()` discriminator;
  five-variant `NotifyError` (Transport / Auth / Rejected /
  Timeout / UnknownTarget) classified for agent retry
  guidance; name-keyed registry with `register` /
  `dispatch` / `list_targets`.
- **Task 5 — Telegram outbound backend** wraps the existing
  `aivyx-telegram` `ReqwestTransport`. Per-target backend
  shares the transport across multiple Telegram targets;
  `chat_id` parses from config string to `i64` at
  construction; negative group chat_ids supported; heuristic
  status-code classification mapping `TransportError::Platform`
  strings to the right `NotifyError` variant.
- **Task 6 — Generic webhook backend** posts JSON
  `{source, target, subject?, message, timestamp}` per
  Q5(a). Trait abstraction (`WebhookSender`) so tests don't
  need a real HTTP server; production `ReqwestWebhookSender`
  uses a 5s timeout. `map_http_status` pure-function tested
  across all status ranges.
- **Task 7 — `NotifySendTool`** with OnceLock factory pattern
  matching `MissionCreateTool` / `RoleSwitchTool`. Input
  schema `{target, message, subject?}` per Q3(b). Output
  per Q4(a): `Completed` with `success: bool` so the agent
  reads structured retry data without `ToolOutcome::Failed`
  escalating up the turn loop. `required_scope` builds
  `notify.send:<target>` from input.
- **Task 8 — daemon wiring + worked example.**
  `build_notify_dispatcher` factory walks the config and
  constructs the right backend per target. Binary's
  session-build path builds a dedicated `ReqwestTransport`
  when Telegram targets exist, constructs the dispatcher,
  registers `NotifySendTool` with it. `examples/aivyx-pa.toml`
  gains a documented `[[notify_target]]` section (commented
  out by default).

Streak predictions all correct: DESIGN.md → 9 (extended);
PRODUCT.md → 2 (extended); `aivyx-core/src/lib.rs` → 10 (new
record, beating Phase 61's 9). Tests +65 (1074 → 1139) —
underestimated at open (predicted ~+20); the six concurrent
new surfaces each carried ~10 tests. Zero clippy warnings.
Zero new workspace deps.

**Scope adjustment at exit:** the `assemble_session_prompt`
extension originally planned for Task 8 (the agent learns
target names from the system prompt) deferred to a follow-on
phase. The tool is fully functional without it — the agent
learns from the input schema; the operator's
`[profile] communication_style` or role `system_prompt` can
name specific targets when needed.

## Phase 63 — Reach Phase 2: Trigger-Config Notify Sugar

**Frozen — see [PHASE_63.md](archive/phases/PHASE_63.md).** Second phase of
the Reach Milestone. Closes the Phase 62-deferred trigger-
config sugar alternative: each `[[schedule]]`, `[[webhook]]`,
`[[file_watch]]` entry accepts an optional
`notify_target = "..."` field; when the trigger fires and the
turn completes, the daemon auto-dispatches the agent's final
response to the named target. No agent involvement, no
system-prompt instruction. The "scheduled summary lands on my
phone" use case now Just Works.

Delivered across six engineering tasks (Tasks 1, 2, 3+4+5
combined, 6, 7):

- **Task 2 — Config field + cross-validation.** All three
  Raw* trigger types gain `notify_target: Option<String>`;
  mirror to public ScheduleConfig / WebhookConfig /
  FileWatchConfig. New `validate_trigger_notify_targets`
  helper called after notify_targets is built but before
  AivyxConfig assembly. Two failure modes: unknown-target,
  and missing-capability (walks the trigger's role parent
  chain, accumulates declared scopes into a CapabilitySet,
  intersects with the trust ceiling, checks
  `notify.send:<target>` is granted). Nine new tests
  including the SemiTrusted-ceiling-drops-notify regression
  and inheritance-via-parent-role.
- **Task 3 — TriggerDispatch auto-notify hook.** New
  `with_notify_dispatcher` builder; `fire()` signature
  extends with `notify_target: Option<&str>`. After the turn
  completes, if both are Some, dispatch the agent's response
  with subject `<kind>: <trigger-id>`. `render_notify_body`
  pure function maps each TurnOutcome variant to a body
  string (Completed → final_message; Failed → "Turn failed:
  <reason>"; Escalated → "Turn escalated: <reason>";
  TimedOut + Cancelled → marker strings). Empty Completed
  body skips per Q2(a). Six render-body unit tests.
- **Task 3 plumbing.** All three Record types
  (ScheduleRecord, WebhookRecord, FileWatchRecord) gain
  `notify_target` with `#[serde(default)]` for backward
  compat with pre-Phase-63 encrypted records. The three
  fire() call sites + WatchState + config-to-record paths
  all propagate the field. DaemonConfig gains
  `notify_dispatcher: Option<Arc<NotifyDispatcher>>`; the
  binary shares one Arc between `NotifySendTool` (Phase 62)
  and `DaemonConfig` for the trigger path.
- **Task 6 — Worked example.** New section in
  `examples/aivyx-pa.toml` documents the schedule + notify_target
  pattern with operator rationale.

Streak predictions all correct: DESIGN.md → 10, PRODUCT.md → 3,
`aivyx-core/src/lib.rs` → 11 (new record, beating Phase 62's
10). Tests +15 (1139 → 1154), inside the predicted +15–20
range. Zero clippy warnings. Zero new workspace deps.

**Scope adjustment at exit:** Q1(a) sign-off chose a new
`AuditEventKind::AutoNotifyDispatched` variant for forensic
search. Implementation revealed `TriggerDispatch` doesn't hold
an audit-hook reference today; wiring one in touches multiple
plumbing layers and is materially larger than the notify hook
itself. Deferred to a focused follow-on phase. Auto-notify is
eprintln-logged matching existing trigger.rs patterns (mission
state changes aren't audit-logged either). Per-kind
dispatcher-recording integration tests similarly folded into
the `render_notify_body` unit coverage.

## Phase 64 — Identity Export (Persona Phase 3)

**Frozen — see [PHASE_64.md](archive/phases/PHASE_64.md).** Closes the oldest
open deferral: Phase 60's "identity export/import" item. The
phase ships **export only** per the implementation-time scope
adjustment; import lands in Phase 65 with focused destructive-
write design attention.

Delivered across five engineering tasks (Tasks 1, 2+4, 3+6, 7,
8):

- **Task 2+4 — Format substrate.** New module
  `crates/aivyx-channel/src/identity_export.rs` with:
    * `IdentityExport` top-level bundle (schema_version,
      exported_at, source_host, profile, persona).
    * `ProfileExport` — plain-values projection of
      `aivyx_config::Profile` dropping `Sourced<...>` metadata.
    * `PersonaExport` — `Vec<DeltaExport>` (MAC-stripped) +
      `effective_at_export` snapshot.
    * `DeltaExport { seq, delta }` — preserves seq for
      parse-time gap detection.
    * `build()` pure assembler + `parse_and_validate()` with
      five failure modes (Json, SchemaVersion, NonMonotonicSeq,
      InvalidDelta, EffectiveMismatch).
  Drive-by: `EffectivePersona` gains `Serialize` + `Deserialize`
  derives. 9 new tests.

- **Task 3+6 — CLI + IPC wiring.** New IPC envelope
  `QueryPayload::ExportPersonaChain` with single-shot response
  capped at 100,000 entries. Daemon handler reads
  `persona_log.entries()` + the shared persona state. Client
  wrapper `export_persona_chain` mirrors the
  `list_persona_deltas` pattern. New CLI module
  `aivyx_modules/identity.rs` with `run_identity_export(path)`
  — fetches Persona via IPC, loads Profile via aivyx-config
  directly, writes 0600 JSON. New `CliMode::Identity(...)` +
  `IdentitySubcommand::Export` variant. `aivyx-pa identity import`
  recognized but routes to a descriptive Phase-65 deferral
  error. 6 parser tests + 2 module tests.

- **Task 7 — Operator docs.** New "Moving Aivyx PA to a new
  machine" section in `docs/INSTALL.md` documents the export
  flow, the JSON shape, the HMAC re-bind design (Q1(a)), and
  the Phase 65 deferral.

Streak predictions all correct: DESIGN.md → 11, PRODUCT.md → 4,
`aivyx-core/src/lib.rs` → 12 (new record, longest production-
core run in project history; beats Phase 63's 11). Tests +17
(1154 → 1171). Zero clippy warnings. Zero new workspace deps.

**Scope adjustment at exit:** Task 5 (`aivyx-pa identity import
<path>`) deferred to Phase 65 per the implementation-time
sign-off on Option B (future-proofing argument). The import
side carries substantial substrate of its own — new IPC
envelopes for destructive writes, conflict resolution, atomic
replay, `--force` flag, integration tests for each failure
mode. Splitting the work yields cleaner phases and focused
design attention for the locked-in IPC semantics. Phase 65
opens next with the dedicated scope.

## Phase 65 — Identity Import (Persona Phase 4)

**Frozen — see [PHASE_65.md](archive/phases/PHASE_65.md).** Closes the Phase
60 identity-deferral end to end. `aivyx-pa identity import
<path>` replays an exported bundle onto the local persona
chain, re-signing each delta against the target host's HMAC
key. Together with Phase 64's export, the operator now has a
full multi-host identity transfer path.

Delivered across three engineering commits (Open, Tasks 2–6
combined, Tasks 7+8 combined, Exit):

- **IPC envelope pair.** `FrontendMessage::ImportPersonaChain
  { id, deltas, effective_at_export, force }` →
  `DaemonMessage::PersonaImportResolved { id, ok, success,
  error }` with `PersonaImportSuccess { deltas_imported,
  final_chain_seq }` per Q4(a). `DaemonEnvelope` gains the
  response variant for client-side decode.
- **Daemon handler.** `resolve_persona_import` runs the full
  flow: server-side re-validation, conflict check (Q3(a)),
  optional force-wipe, per-delta replay through the existing
  `append` path (re-signs against local key per Phase 60
  Q1(a)), runtime refresh via `recompute_shared_from_entries`.
  Best-effort atomicity per Q1(a): daemon crash mid-import
  leaves chain partial; operator re-imports.
- **`PersistentPersonaLog::clear`** — new method wipes both
  persisted rows and the in-memory chain. HMAC key preserved
  so subsequent appends produce a fresh chain at seq 0.
- **Client wrapper + CLI handler.** `import_persona_chain`
  mirrors `revert_persona_delta`'s shape. CLI handler reads
  file, locally validates via Phase 64's
  `parse_and_validate`, forwards to daemon, prints
  per-Q4(a) summary + Q2(a) Profile-hand-edit reminder.
- **Parser update.** `aivyx-pa identity import <path>
  [--force]` replaces the Phase 64 deferral message;
  trailing `--force` accepted, double-force and unknown
  args rejected.
- **Docs.** `docs/INSTALL.md` "Moving Aivyx PA to a new
  machine" updated with the import command, the conflict-
  resolution explanation, and the Q2(a) Profile note.

Streak predictions all correct: DESIGN.md → 12, PRODUCT.md →
5, `aivyx-core/src/lib.rs` → 13 (new record — longest
production-core run in project history; beats Phase 64's 12).
Tests +5 (1171 → 1176), under the predicted +10–15 range
because Phase 65 leaned heavily on the Phase 64 substrate
(parse_and_validate from Phase 64, append from Phase 59,
recompute helper from Phase 60). Zero clippy warnings. Zero
new workspace deps.

**Identity export + import are now both shipped.** The Phase
60 deferral closes entirely. Future Persona work is operator-
feedback-shaped (merge-strategy imports, encrypted export
format, multi-source merge, schema migrations, real atomic
import).

## Phase 66 — Starter Profile Templates

**Frozen — see [PHASE_66.md](archive/phases/PHASE_66.md).** Closes the third
post-Phase-60 codebase-review direction (after Distribution
and Reach). Ships `aivyx-pa init --template <name>` so a fresh
operator gets from "downloaded the binary" to "useful agent"
without writing aivyx-pa.toml from scratch. Three starter
templates cover the common archetypes: `coder` (software
engineering), `researcher` (research + synthesis), `personal`
(personal task management + briefings).

Delivered across three engineering commits (Open, Tasks 2–8
combined, Tasks 9+10):

- **Template registry substrate.** New
  `init_templates.rs` module with `Template { name,
  description, source, toml_content }`, `bundled_templates()`
  via `include_str!`, `user_templates()` reading from
  `$XDG_DATA_HOME/aivyx-pa/templates/` or
  `~/.local/share/aivyx-pa/templates/`, `list_templates()`
  unioning both with user-dir winning on collision,
  `load_template(name)` with descriptive miss error, and
  `parse_description` reading the `# description: …` marker
  from leading TOML comments.
- **CLI flags.** `CliMode::Init` becomes `Init(InitMode)`
  with `Interactive`, `InteractiveFromTemplate { name }`,
  `ListTemplates` variants. Parser accepts `aivyx-pa init`,
  `aivyx-pa init --template <name>`, `aivyx-pa init --template`
  (no name → list), `aivyx-pa init --list-templates`.
- **Wizard pre-fill.** New `TemplateDefaults` extracts
  provider, model, fs_root, storage_path, assistant_name,
  primary_use_case, communication_style from a template's
  TOML via `toml_edit`. Each wizard prompt's default
  switches to the template-sourced value when present.
  `render_with_template` splices wizard answers into the
  template's `DocumentMut` and serializes — comments, role
  declarations, MCP server blocks, commented-out sections
  all survive.
- **Three starter templates** in `examples/templates/`. Each
  is a complete `aivyx-pa.toml` with archetype-appropriate
  Profile content, role envelope, and MCP/notify defaults
  (commented-out where appropriate).
- **20 unit tests** (14 registry + 6 parser) including
  per-template TOML validity checks.
- **`docs/TEMPLATES.md`** as the operator-facing reference.
  `INSTALL.md` First-run checklist + README quickstart
  updated to point at the fast-path.

Streak predictions all correct: DESIGN.md → 13, PRODUCT.md →
6, `aivyx-core/src/lib.rs` → 14 (new record, longest
production-core run in project history — beats Phase 65's
13). Tests +18 (1176 → 1194), slightly under the predicted
+20–30 range. Zero clippy warnings. Zero new workspace deps.

**Q-block sign-off ambition held cleanly.** The operator
chose the more ambitious options across all four questions
at design time (hybrid location, three templates, pre-fill
wizard, both discovery modes). All four sign-offs delivered
as designed; no scope adjustments at implementation time.
Phase 66 substantial-scope-but-clean shape.

## Phase 67 — Auto-Notify Audit Event

**Frozen — see [PHASE_67.md](archive/phases/PHASE_67.md).** Closes the Phase
63 deferred Q1(a) sign-off: shipping
`AuditEvent::AutoNotifyDispatched` as a new variant of the
audit chain enum + wiring `TriggerDispatch` with the audit
hook needed to emit it.

After Phase 67, every trigger-fired auto-notify (delivered,
skipped-empty-response, or failed) lands as an entry in the
persistent audit chain alongside `TurnStarted` / `TurnEnded`.
Operators can answer "why didn't my morning briefing arrive?"
from the audit chain alone — eprintln moves from primary
evidence to debugging supplement.

Delivered across four engineering commits (Open, Tasks 2–4
combined, Tasks 5+6, Exit):

- **Audit variant in `aivyx-audit`.** New
  `AuditEvent::AutoNotifyDispatched { session_id,
  trigger_kind, trigger_id, target_name, outcome,
  dispatched_at_unix_ms }` + supporting
  `TriggerKindSummary` (Cron / Webhook / FileWatch) +
  `AutoNotifyOutcomeSummary` (Delivered /
  SkippedEmptyResponse / Failed { error_kind,
  error_message }) enums. All `#[serde(tag = "kind")]` so
  existing chain readers (Web UI Audit tab, `aivyx-pa
  --verify-only`) parse the new variant without per-reader
  changes.
- **Plumbing.** `TriggerDispatch` gains an
  `audit_log: Option<Arc<PersistentAuditLog>>` field +
  `with_audit_log` builder. `run_daemon` wires
  `DaemonConfig::audit_log` (Phase 47 field) through to the
  dispatch instance.
- **Emission.** New `emit_auto_notify_audit` method on
  `TriggerDispatch`; the three auto-notify branches
  (skip-empty / dispatch-Ok / dispatch-Err) all converge on
  it. Append failures are eprintln-logged per Q3(a) — the
  notify already happened or didn't; audit failure shouldn't
  conflate the outcome.
- **Conversion helpers.** `From<TriggerSource>` for
  `TriggerKindSummary`; `outcome_from_notify_error` maps
  the five `NotifyError` variants to the
  `AutoNotifyOutcomeSummary::Failed` shape using the same
  `error_kind` labels (`transport` / `auth` / `rejected` /
  `timeout` / `unknown_target`) the `notify.send` tool
  emits — forensic searches grep across both
  agent-initiated and daemon-initiated notify failures
  uniformly.
- **Docs.** New "Debugging missing notifications" section
  in `docs/INSTALL.md` walks operators through the audit
  chain + the three outcome shapes + `session_id`
  correlation back to `TurnStarted` / `TurnEnded`.

Streak predictions all correct: DESIGN.md → 14, PRODUCT.md →
7, `aivyx-core/src/lib.rs` → 15 (new record, longest
production-core run in project history — beats Phase 66's 14).
Tests +9 (1194 → 1203), slight undershoot of the predicted
+10–15 (3 audit + 6 conversion-helper unit tests; full
end-to-end fire integration deferred to dogfooding). Zero
clippy warnings. Zero new workspace deps.

**Implementation-time scope adjustment held cleanly.** Q1
sign-off chose rich event with turn_id; investigation
revealed `TurnOutcome` doesn't carry `turn_id` (lifting it
would touch 120 match sites). Solved at design time with
`session_id` correlation — already minted per trigger fire
and recorded on `TurnStarted` audit entries. Same forensic
payoff; no upstream refactor. `turn_id` correlation recorded
as a dedicated future-phase deferral.

## Phase 68 — Email SMTP Notify Backend (Reach Phase 3)

**Frozen — see [PHASE_68.md](archive/phases/PHASE_68.md).** Third notify
backend after Phase 62's Telegram + webhook. Closes the
largest remaining adoption-shape gap on the Reach axis:
every operator has email; most don't run Telegram bots.
After Phase 68 the supported notify kinds are `telegram`,
`webhook`, and `email`.

Delivered across three engineering commits (Open, Tasks 2–5
combined, Exit):

- **Config in `aivyx-config`.** `EmailConfig` + `TlsMode`
  enum + `NotifyTargetKind::Email { to }` variant.
  `[email]` section parses via a `RawEmail` shape;
  `build_email_config` validates required-when-present
  semantics + the security rules (tls_mode = "none"
  rejected per Q4; default port from tls_mode; address
  `@` checks; email target without `[email]` section is a
  load-time error naming the offending target).
- **`notify_email.rs` in `aivyx-channel`.** `EmailSender`
  trait + `LettreEmailSender` production impl (built once
  per deployment, shared via `Arc`) + `NotifyEmailBackend`
  wrapping the shared sender with per-target from/to.
  `map_lettre_error` classifies lettre errors into the
  five-variant `NotifyError` taxonomy `notify.send`
  already uses. Auth mechanisms declared explicitly as
  PLAIN + LOGIN.
- **Dispatcher + binary wiring.** `build_notify_dispatcher`
  gains an `EmailDispatchContext` parameter; binary builds
  the `LettreEmailSender` once if any email target exists.
- **Docs + worked example.** `examples/aivyx-pa.toml` gains
  commented `[email]` + email `[[notify_target]]` blocks
  with provider-specific setup (Gmail / Fastmail /
  ProtonMail Bridge / SES / self-hosted). `docs/INSTALL.md`
  "Email notifications" section walks operators through
  quick-setup, the TLS-mandatory rule, and the
  no-OAuth2-yet caveat.

Streak predictions all correct: DESIGN.md → 15, PRODUCT.md →
8, `aivyx-core/src/lib.rs` → 16 (new record, longest
production-core run in project history — beats Phase 67's
15). Tests +19 (1203 → 1222). Zero clippy warnings.

**New workspace dep:** `lettre` with `default-features =
false` + explicit rustls features. First net-new workspace
dep since Phase 58's `toml_edit` (six phases ago). Verified
zero openssl pulls; TLS stays uniformly rustls across
reqwest + lettre.

After Phase 68 the Reach Milestone covers the three most
common operator channels (Telegram for chat-style, webhook
for tooling-style, email for everyone else). Remaining
Reach-axis deferrals (Web UI desktop notify, OS-level
notify, default-target sugar, retry semantics, multi-target
dispatch, conditional notify) are operator-feedback-shaped
follow-ups; the substrate is extensible.

## Phase 69 — Web UI Desktop Notifications (Reach Phase 4)

**Frozen — see [PHASE_69.md](archive/phases/PHASE_69.md).** Fourth notify
backend after Phase 62's Telegram + webhook and Phase 68's
email. Closes the focused-at-the-laptop case: operators who
already keep the Web UI tab open at `127.0.0.1:7843` get
OS-level desktop notifications + an in-page toast banner with
no API keys, no SMTP setup, no bot tokens.

Delivered across nine engineering commits (Open, Tasks 2–10
each as their own commit, Exit):

- **Config in `aivyx-config`.** `NotifyTargetKind::WebUi`
  unit variant (no per-target fields per Q3(a)). Loader
  accepts `kind = "web-ui"`; unknown-kind error message
  lists `web-ui` in the supported-kinds suggestion.
- **IPC envelope.** `DaemonMessage::DesktopNotification {
  title, body }` + matching `DaemonEnvelope` variant per
  Q4(a). Distinct from `StreamEvent` (per-session) because
  broadcast events deserve their own variant.
- **`notify_webui.rs` in `aivyx-channel`.** `WebUiBroadcaster`
  wraps a `tokio::sync::broadcast::Sender<DesktopNotificationFrame>`;
  `NotifyWebUiBackend` implements `NotifyBackend::send` by
  pushing onto the broadcaster. Per Q1(a), zero subscribers
  yields `Ok(())` — fire-and-forget broadcast model. Audit
  chain (Phase 67) still records every dispatch.
- **WS handler subscription.** Each browser WS connection
  subscribes a fresh broadcast receiver; a third concurrent
  loop relays each frame onto the WS as
  `DaemonEnvelope::DesktopNotification` JSON.
- **Dispatcher + binary wiring.** `build_notify_dispatcher`
  gains the broadcaster as a fourth parameter; binary
  constructs one `Arc<WebUiBroadcaster>` at startup whenever
  the Web UI is enabled and Arc-shares it between the
  dispatcher (push side) and `DaemonConfig` (subscribe side).
- **Web UI JS.** Handler for `{type: "DesktopNotification"}`
  triggers `new Notification(title, {body})` (browser API)
  AND renders a stackable in-page toast banner per Q2 (both
  UX modes). One-time "Enable notifications" prompt on page
  load when `Notification.permission === "default"`.
- **Docs + worked example.** `examples/aivyx-pa.toml` gains a
  commented `kind = "web-ui"` block; `docs/INSTALL.md`
  "Web UI desktop notifications" subsection covers the
  two-step enable, the browser-tab-must-be-open caveat, and
  the pair-with-email-for-persistence guidance.

Streak predictions all correct: DESIGN.md → 16, PRODUCT.md →
9, `aivyx-core/src/lib.rs` → 17 (new record, longest
production-core run in project history — beats Phase 68's
16). Tests +12 (1222 → 1234). Zero clippy warnings.
Zero new workspace deps (`tokio::sync::broadcast` was
already available via the existing tokio dep).

After Phase 69 the Reach Milestone covers all four
operator-shape categories: chat-style (Telegram), tooling-
style (webhook), inbox-style (email), focused-at-the-laptop
(Web UI desktop). Remaining Reach-axis deferrals are smaller
operator-feedback shapes: WebPush / service-worker
notifications for closed-tab delivery (real engineering —
needs VAPID + service-worker registration), default-target
sugar, per-target rate limits, retry semantics, multi-target
dispatch, conditional notify, notification urgency / sound /
icons.

## Phase 70 — Reflection Auto-Loop (P14 Self-Learning Closure)

**Frozen — see [PHASE_70.md](archive/phases/PHASE_70.md).** Closes the
long-held self-learning half of **P14 Persona**. Phase 29
(frozen) shipped the reflection substrate — `ReflectionProposeTool`
+ `ReflectionApplyTool` — but the agent's persona-delta
proposals went through a synchronous mission-gate flow that
required the operator to be present at the agent's terminal
at the moment of proposal. Phase 70 adds the **asynchronous
review surface**: agent calls `reflection.propose` →
proposals land as Pending rows in a new encrypted proposal
chain → operator reviews on their own schedule via the Web UI
Proposals pane or `aivyx-pa persona proposals` CLI →
approval/rejection appends to the persona chain (or audit
trail) accordingly.

Delivered across ten engineering commits (Open, Tasks 2–10,
Exit):

- **Config in `aivyx-config`.** `[[reflection_schedule]]`
  section parses + validates (cron non-empty, lookback bounds
  60s–30 days, name uniqueness across both schedule and
  reflection-schedule namespaces, role_override existence).
- **Storage in `aivyx-storage`.** `KeyDomain::PersonaProposals`
  variant with HKDF info bytes `persona-proposals`, table
  name `aivyx_persona_proposals_v1`, integrated into the
  fixed-size subkey array and the all-variants tripwire test.
- **`persona_proposal.rs` in `aivyx-channel`.** HMAC-chained
  proposal log mirroring `PersistentPersonaLog`. Status as
  state machine derived from chain entries (Pending → Approved
  / Rejected / Superseded); each transition appends a new
  signed row rather than mutating, preserving the audit story.
  Distinct genesis seed (`aivyx-proposal-genesis-v1`) from
  the persona chain so chain-confusion attacks are
  structurally rejected at MAC verification (Q4(a)).
- **IPC envelopes.** Two new `QueryPayload` variants
  (`ListPersonaProposals`, `GetPersonaProposal`), the wire-
  format `PersonaProposalSummary`, `FrontendMessage::ResolvePersonaProposal`
  with the three-variant `PersonaProposalResolution` tagged
  enum (`Approve | ApproveWithEdit { edited_op } | Reject {
  reason }` per Q3(a)), and the matching
  `DaemonMessage::PersonaProposalResolved` response with
  `PersonaProposalResolveSuccess { proposal_status,
  applied_seq }`.
- **Daemon-side resolution.** `resolve_persona_proposal` in
  `daemon_server.rs`: Approve / ApproveWithEdit validate the
  applied op, append to the persona chain, then record an
  Approved row bound to the resulting seq; Reject just
  records Rejected. Shared persona state is recomputed on
  approve. `handle_query` gains arms for both proposal-list
  queries with status-filter parsing.
- **Web UI Proposals pane.** New tab with filter chips
  (`pending | approved | rejected | all`), per-proposal
  cards showing category / id / source-reflection-session /
  status badge / proposed op / agent reason, three actions
  per Pending card (Approve verbatim / inline JSON editor +
  Save & Approve for edit-on-approve / Reject with optional
  reason). Approved cards with operator-edited applied_op
  render both ops for audit visibility.
- **CLI subcommands.** `aivyx-pa persona proposals list
  [--status STATUS]` (default `pending`), `show <id>`,
  `approve <id>`, `reject <id> [--reason TEXT]`. Pretty-
  prints proposals with all status-specific fields.
- **Agent-write integration.** `ReflectionProposeTool` gains
  a proposal-log setter; when set (binary's startup path
  wires it in), every agent-supplied `ProposedPersonaDelta`
  is appended to the proposal chain as a Pending row in
  addition to the existing mission-gate flow.

Streak predictions all correct: DESIGN.md → 17, PRODUCT.md →
10, `aivyx-core/src/lib.rs` → 18 (new project record, beats
Phase 69's 17). Tests +41 (1234 → 1275), exceeding the
+25-35 prediction. Zero clippy warnings. Zero new workspace
deps.

**Scope note — cron auto-firing deferred.** The
`[[reflection_schedule]]` config section parses + validates
end-to-end, and the proposal substrate accepts agent-
generated deltas via `reflection.propose` today; the
dedicated scheduler-loop that fires reflection turns on the
configured cron is a deferred follow-up. Operators who want
auto-reflection today wire a regular `[[schedule]]` entry
with a reflection-flavored prompt; the proposals land in the
same chain and surface in the same Web UI / CLI panes either
way.

## Phase 71 — Reflection Scheduler Loop (closes Phase 70 deferral)

**Frozen — see [PHASE_71.md](archive/phases/PHASE_71.md).** Closes the
cron-auto-firing deferral carried at Phase 70 exit. After
Phase 71 the self-learning loop is genuinely autonomous: at
each configured cron boundary the daemon synthesizes recent
turn outcomes from the audit chain, fires a reflection turn
carrying the canonical reflection prompt + outcome summary
block, and any persona deltas the agent proposes land in the
Phase 70 proposal chain for asynchronous operator review.

Delivered across four engineering commits (Open, Tasks 2-4,
Task 5, Tasks 7+Exit):

- **New `reflection_scheduler.rs` module in `aivyx-channel`.**
  `run_reflection_scheduler` async loop with adaptive
  sleep-until-earliest-fire (cap 60s); per-schedule in-memory
  last-fired-at; cron parsing reuses the existing `cron` crate
  from Phase 26. `fire_reflection` summarizes recent outcomes,
  formats the prompt block, calls `TriggerDispatch::fire(...)`.
- **Canonical reflection prompt.** Hardcoded
  `REFLECTION_SYSTEM_PROMPT` constant (Q2(a)) with
  conservative behavioral framing: propose only on ≥3-turn
  pattern recurrence, prefer narrower categories, every
  proposal lands Pending until operator approves. A smoke
  test guards the constraint set from accidental gutting.
- **Audit walker + LRU cache.** `OutcomeSummary` struct
  + `summarize_recent_outcomes_from_entries` pairs
  TurnStarted/TurnEnded by turn_id, filters by lookback
  window, sorts most-recent-first; in-flight + un-paired
  entries skipped. `OutcomeSummaryCache` is a bounded
  VecDeque-backed LRU keyed by
  `(lookback_secs, audit_chain_len)` per Q1(c).
- **TriggerSource + TriggerKindSummary variants.** New
  `Reflection` variant in both runtime + audit enums so
  forensic searches distinguish self-learning reflection
  turns from operator-declared crons.
- **Binary wire-up.** `DaemonConfig` gains
  `reflection_schedules: Vec<ReflectionScheduleConfig>`. When
  non-empty AND an audit log is available, the daemon spawns
  the scheduler task alongside the existing scheduler /
  webhook listener / file watcher. Startup banner prints one
  line per registered schedule. Graceful degradation: when
  schedules exist but no audit log is available, a clear
  diagnostic prints and the scheduler is not spawned.
- **Docs.** `examples/aivyx-pa.toml` and `docs/INSTALL.md` flip
  from the Phase 70 "deferred-polish" caveat to a concrete
  setup walkthrough.

Streak predictions all correct: DESIGN.md → 18, PRODUCT.md →
11, `aivyx-core/src/lib.rs` → 19 (new project record, beats
Phase 70's 18). Tests +14 (1275 → 1289), one below the
+15-25 prediction floor — the binary wire-up didn't add
net-new tests because the spawn pattern was already
exercised by the existing scheduler e2e. Zero clippy
warnings. Zero new workspace deps.

**Scope note — Q3(a) role_override is recorded but not
runtime-honored.** The schedule config validates that
`role_override` references an existing role and the scheduler
logs the override for forensic attribution, but the per-fire
runtime role swap is a deferred polish: v1 runs the
reflection turn under the daemon's active role. Operators
who want a dedicated reflection envelope today declare a
`[[role]]` and run the daemon under it via the existing
role-switching path.

After Phase 71 the self-learning half of P14 is genuinely
autonomous end-to-end.

## Phase 72 — Reach Polish: Multi-Target, Default, Conditional

**Frozen — see [PHASE_72.md](archive/phases/PHASE_72.md).** Closes three
Tier-1 operator-feedback shapes carried from the Reach
Milestone (Phases 62-69) in one phase:

- **Multi-target dispatch.** A single trigger fans out to N
  notify targets in one fire. `notify_targets: Vec<String>`
  on trigger configs; the singular `notify_target` stays as
  a backwards-compat alias. Dispatch uses
  `futures_util::future::join_all` for concurrent fan-out
  (Q4(a)); each per-target backend outcome is audited
  independently as a separate `AutoNotifyDispatched` entry,
  so one target's transport failure doesn't block the
  others.
- **Default-target sugar.** `default = true` on one
  `[[notify_target]]` block marks it as the global default;
  triggers that omit `notify_targets` fall through to it at
  config-load time so runtime dispatch never has to resolve
  defaults again. Loader rejects multiple defaults with a
  clear "phone, desktop" multi-name error.
- **Conditional notify.** New `NotifyWhen` enum
  (`Always | OnFailed | OnCompletedNonEmpty`) gates dispatch
  by turn outcome. A gate-skipped dispatch records the new
  `AutoNotifyOutcomeSummary::SkippedByCondition { condition }`
  audit variant so forensic searches can answer "why didn't
  this fire?" definitively.

Schedule / Webhook / FileWatch storage records gain
`notify_targets` + `notify_when` fields (serde-defaulted for
backwards compatibility); the config-to-record bridges copy
both through. All four `dispatch.fire(...)` callers updated
to the new signature.

Streak predictions all correct: DESIGN.md → 19, PRODUCT.md →
12, `aivyx-core/src/lib.rs` → **20** (new project record +
**two-decade milestone**, beating Phase 71's 19). Tests +13
(1289 → 1302), below the +20-30 prediction floor — honest
miss called out in the phase doc: fan-out integration tests
require heavier scaffolding (mocked dispatcher + audit log +
spawned futures) than fit cleanly in the phase. Zero clippy
warnings. Zero new workspace deps.

Tier-2 polish (per-target retry semantics, per-target rate
limits, Web UI notification history pane) defers to a focused
follow-up — different cluster of concerns.

## Phase 73 — Reach Tier-2 Polish: Retry, Rate Limit, History

**Frozen — see [PHASE_73.md](archive/phases/PHASE_73.md).** Closes the
Tier-2 polish backlog Phase 72 explicitly deferred. Three
items shipped in one phase:

- **Per-target retry.** Flat fields `retry_count` +
  `retry_backoff_ms_start` on `NotifyTargetConfig` per
  Q1(b). On transient failures (`Transport`, `Timeout`, or
  `Rejected` with HTTP status ≥ 500 per Q2(b)), the
  dispatcher retries up to `retry_count` times with
  exponential backoff. `Auth`, `UnknownTarget`, and
  `Rejected` < 500 never retry. Capped at 10 retries by the
  loader (footgun guard).
- **Per-target rate limit.** In-memory sliding-window token
  bucket per target via the new `RateLimitRegistry` per
  Q3(a). Both `rate_limit_max` + `rate_limit_window_secs`
  must be set together. Exhausted bucket records the new
  `AutoNotifyOutcomeSummary::SkippedByRateLimit { limit,
  window_secs }` audit variant. Daemon-lifetime state;
  restart resets the bucket.
- **Notification history pane.** New
  `QueryPayload::ListNotificationHistory { from_seq, limit,
  target_filter }` walks the audit chain for
  `AutoNotifyDispatched` events with server-side
  pagination (cap 500, matches the audit pane). Web UI
  Notifications tab renders a 4-column grid with
  colour-coded outcome badges; auto-populated per-target
  chips filter the view. CLI parity: `aivyx-pa notify history
  [--target NAME] [--limit N]`.

`NotificationHistoryEntry` is the flat wire shape; the
daemon's `render_notify_outcome_for_history` helper renders
each `AutoNotifyOutcomeSummary` variant into stable lowercase
`outcome_kind` + variant-specific `outcome_detail` strings.

Streak predictions all correct: DESIGN.md → 20, PRODUCT.md →
13, `aivyx-core/src/lib.rs` → **21** (new project record,
beating Phase 72's 20). Tests +31 (1302 → 1333), comfortably
inside the +25-35 prediction. Zero clippy warnings. Zero new
workspace deps.

After Phase 73 the Reach Milestone polish backlog is closed
end-to-end. Remaining notify-shaped deferrals (WebPush,
Slack-flavored webhooks, XOAUTH2, persisted rate-limit
buckets) are operator-feedback-gated and ship if/when real
pressure surfaces.

## Phase 74 — Memory Polish: Search, Retention, LRU, Web UI Pane

**Frozen — see [PHASE_74.md](archive/phases/PHASE_74.md).** Completes the
self-learning triad — Persona (P14), reflection (Phases
70-71), and now a first-class memory surface. Four items:

- **Keyword search.** `Memory::search` (case-insensitive
  substring across topics + bodies) + the `memory.search`
  agent tool (cross-topic wildcard scope) + `SearchMemory`
  IPC + `aivyx-pa memory search` CLI + Web UI search bar. No
  embedding dep per Q1(a) — semantic retrieval defers to a
  future RAG arc.
- **Per-topic retention.** `[[memory.retention]]` config
  blocks with topic-glob patterns + `forever` |
  `retention_days = N` policies (Q2(a)). The hourly GC pass
  applies the first matching rule; unmatched topics fall
  through to the global `ttl_secs`. New
  `Memory::gc_expired_with_rules` + `RetentionMatcher`
  boundary type keep the memory crate config-agnostic.
- **LRU eviction.** `MemoryEntry::last_read_at_secs`
  (serde-defaulted), stamped by `get_recent` on both substrate
  impls; `evict_oldest_unread` ranks `(last_read ASC, seq
  ASC)` per Q3(a). Replaces FIFO-on-write — a recalled old
  note now survives a never-read younger one.
- **Operator surfaces.** Web UI Memory pane (two-column
  browse + search + per-topic confirm-gated Evict per Q4(a))
  + `aivyx-pa memory list/show/search/evict` CLI parity. New
  IPC: ListMemoryTopics / GetMemoryTopicEntries /
  SearchMemory queries + EvictMemoryTopic frontend message.

Streak predictions all correct: DESIGN.md → 21, PRODUCT.md →
14, `aivyx-core/src/lib.rs` → **22** (new project record,
beating Phase 73's 21). Tests +45 (1333 → 1378), above the
+30-40 prediction. Zero clippy warnings. Zero new workspace
deps.

After Phase 74 the self-learning triad is complete. Likely
follow-ups (semantic RAG, fuzzy match, edit-content Web UI,
per-topic eviction-strategy override) are operator-feedback-
gated.

## Phase 75 — Semantic RAG Memory Arc

**Frozen — see [PHASE_75.md](archive/phases/PHASE_75.md).** Picks up the
Phase 74 deferred "semantic RAG" follow-up: embedding-ranked
memory retrieval layered on top of keyword search, off by
default. Eight tasks:

- **`KeyDomain::MemoryVectors`** — a new encrypted, HKDF-
  isolated storage domain for vectors (precedent: Phases
  21/26/27/56/70). Entry bodies and vectors are never
  decryptable with the same subkey.
- **`EmbeddingProvider` trait + OpenAI-compatible HTTP impl** —
  reuses the existing `aivyx-llm` `reqwest` transport via a
  new non-streaming `post_json` seam. **Zero new workspace
  deps** (Q1(a)). `EmbeddingError` taxonomy mirrors the
  notify-error classification.
- **`[embedding]` config** — `base_url` / `model` / `api_key`
  / `dimensions`. Absent section → semantic disabled, keyword
  unchanged. API key resolves env > TOML > encrypted store
  (same two-phase pattern as the anthropic/openai keys).
- **Vector store + cosine** — `Memory` gains `put_vector` /
  `load_all_vectors` / `semantic_search`; an in-memory flat
  index rebuilt at open; hand-rolled cosine (no linalg dep).
  `forget` + `evict_oldest_unread` drop vectors; orphan
  vectors are skipped at query time (vector store may lag
  entry GC).
- **Write-time embed + lazy backfill (Q2(a))** — an
  `EmbeddingHook` seam keeps `aivyx-memory` free of an
  `aivyx-llm` dep; `MemoryWriteTool` embeds inline (non-fatal)
  and a bounded hourly backfill (reusing the GC timer) indexes
  the back-catalog and re-embeds stale-dimension vectors.
- **`mode` flag + keyword fallback (Q4(a))** — `memory.search`
  gains `mode = keyword|semantic` (default keyword, no
  behavior change); semantic transparently falls back to
  keyword — flagged — when no provider, embed failure, or an
  empty index. Threaded through the agent tool, `SearchMemory`
  IPC (serde-defaulted for round-trip back-compat), CLI
  `--semantic`, and a Web UI toggle.

Streak predictions all correct: DESIGN.md → **22**,
PRODUCT.md → **15**, `aivyx-core/src/lib.rs` → **23** (new
project record, beating Phase 74's 22). Tests +46 (1378 →
1424), inside the +35-50 prediction. Zero clippy warnings.
Zero new workspace deps. Privacy is the operator's `base_url`
choice — cloud or fully on-device.

Likely follow-ups (ANN index, explicit `aivyx-pa memory reembed`,
hybrid keyword+semantic fusion, query-embedding cache) are
operator-feedback-gated.

## Phase 76 — Automatic Semantic Recall (RAG context injection)

**Frozen — see [PHASE_76.md](archive/phases/PHASE_76.md).** Closes the RAG arc
Phase 75 set up: the agent no longer only recalls when it calls
`memory.search` — every turn it embeds the user's message and
auto-injects the most relevant past memories. Pure integration
on the Phase 75 substrate, zero new deps.

- **`ContextProvider` planner hook (Q1a).** Read-side sibling
  of `PruneSink`; `LlmPlannerConfig::with_context_provider` +
  a `begin_turn` invocation that prepends the recalled block
  as a distinct leading text block in the user message (not
  the static system prompt; avoids provider role-alternation).
  Deliberately **not** re-exported from `aivyx-core/src/lib.rs`
  (reachable via `aivyx_core::llm_planner::ContextProvider`) —
  this is what protected the core streak.
- **`rag_top_k` (5) + `rag_min_similarity` (0.20)** on
  `[embedding]`; the floor is what stops naive-RAG noise.
- **`SemanticMemoryContext`** embeds the latest user message
  (Q2a), `semantic_search_scored` top-K, drops sub-floor hits
  (Q3a), formats an injection-safe reference-only block;
  silent no-op on embed-fail / empty index / all-below-floor —
  recall never errors a turn.
- Wired into all three planner factories (local-CLI, daemon,
  child-agent — sub-agents recall too).
- **Visible marker (Q4b) — streak-forced deviation:** a
  `ContextRecall` `AuditTag` variant would have broken the
  core streak (the enum lives in the streak file), so the
  marker is the established stderr-breadcrumb convention
  (`aivyx-pa recall: injected N memories […]`); the recalled
  content is independently visible as the in-turn block. No
  separate Web UI indicator. The streak discipline had teeth
  this phase — a late cost was paid in scope, not in the
  contract.

Streak all three correct: DESIGN.md → 23, PRODUCT.md → 16,
`aivyx-core/src/lib.rs` → **24** (new record, beats Phase 75's
23). Tests +15 (1424 → 1439) — **below** the +25-40 prediction
(Task 5 wiring-only; Task 6 collapsed by the streak deviation;
no Web UI smoke). Honest miss, documented. Zero clippy
warnings. Zero new workspace deps.

Likely follow-ups (conversational-window query, heuristic
recall gate, token-budget context sizing, query-embedding
cache, recall-usage feedback into reflection) are
operator-feedback-gated.

## Phase 77 — Recall → Reflection Feedback Loop

**Frozen — see [PHASE_77.md](archive/phases/PHASE_77.md).** Closes the loop
Phase 76 opened: recall stops being a bigger cache and starts
*teaching* the system. Pure integration on the 75/76/70-71
substrate, zero new deps.

- **`KeyDomain::RecallEvents`** — a dedicated encrypted domain
  for the signal (the Phase 76 audit-streak lesson applied by
  design: route *around* `AuditTag`, not through it).
- **`PersistentRecallLog`** — `ts_be||uuid` keys (time-ordered
  scan, collision-free appends), `events_since` lookback +
  independent `gc_older_than` clamp.
- **Capture (Q1a/Q2a):** `ContextProvider::recall()` gains a
  `SessionId` (llm_planner.rs only — `lib.rs` byte-identical);
  `SemanticMemoryContext` appends a session-correlated
  `RecallEvent` per injected recall, strictly best-effort.
- **Structural correlator (Q1a):** pure, no LLM — matches a
  recall to its turn and signs it from the audit chain's
  existing `OutcomeSummary` (completed-no-followup → +;
  failed / quick-comeback → −; escalated/cancelled → 0).
- **Both actuators (Q3c):** memory-retention self-tuning
  (helpful entries kept LRU-warm via a targeted
  `promote_recall_helpful` — *no new eviction primitive*) and
  operator-gated **Pending** Persona proposals (deterministic,
  deduped, never auto-applied — Phase 70 P14 authority rule).
- **Piggybacked (Q4a):** the existing cron reflection pass
  runs correlate → retention → proposals → recall-log GC over
  the same lookback window. Zero new scheduler; whole pass is
  a no-op when the substrate is absent.

Streak all three correct: DESIGN.md → 24, PRODUCT.md → 17,
`aivyx-core/src/lib.rs` → **25** (new record, beats Phase
76's 24). Tests +22 (1439 → 1461) — **below** the +35-55
prediction (the actuators deliberately reused existing
machinery rather than adding primitives, so each is lean;
honest miss, documented). Zero clippy warnings. Zero new
workspace deps.

Likely follow-ups (`[recall_feedback]` tuning knob,
LLM-judged recall usefulness, cross-session pattern learning)
are operator-feedback-gated.

## Phase 78 — Learning Observability & Trust Surface

**Frozen — see [PHASE_78.md](archive/phases/PHASE_78.md).** Makes the closed
Phase 75–77 self-learning loop *legible*: an autonomous system
the operator can't see is one they can't trust. A read-only
view of what the assistant has learned and why, with full
parity, reusing existing machinery — zero new behaviour, zero
new storage, zero new deps.

- **`recall_insights` (Q1a):** `recall_feedback` refactored to
  `correlate_detailed` (tally + per-recall detail; `correlate`
  delegates — all 15 Phase 77 tests still green, one matching
  pass so the surface can never disagree with the loop).
  `build_digest` (recall counts, promoted/aging, top
  helpful/unhelpful) + `build_provenance` (each `recall-fb:`
  proposal traced to its contributing recalls/turns,
  reconstructed on-query — no schema/chain migration).
- **`GetLearningInsights` IPC + handler:** computed on-query
  from the live recall log + audit `OutcomeSummary` + the
  proposal chain (the same builders the reflection loop uses).
  No recall substrate → an empty digest, a valid "nothing
  learned yet" answer, not an error.
- **Full parity (Q2a):** `aivyx-pa learning [--window <secs>]`
  CLI + a read-only Web UI **Learning** tab; approve/reject
  stays in the existing Proposals surface.

Streak all three correct: DESIGN.md → 25, PRODUCT.md → 18,
`aivyx-core/src/lib.rs` → **26** (new record, beats Phase
77's 25) — the surface lives entirely in `aivyx-channel`,
reusing existing types; `lib.rs` byte-identical, the
streak-shaped-architecture discipline continued. Tests +12
(1461 → 1473) — **below** even the deliberately-lowered
+18-30 prediction (third consecutive miss; pure derivation +
reused round-trip harness + thin compositional handler).
Honest, documented; calibration tightened. Zero clippy
warnings. Zero new workspace deps.

Likely follow-ups (per-memory-entry drill-down, history
beyond the recall-log window, Web UI live refresh, actionable
insights) are operator-feedback-gated.

## Phase 79 — Adaptive Persona (contextual "Soul" selection)

**Frozen — see [PHASE_79.md](archive/phases/PHASE_79.md).** The accreted
Persona was dumped whole into every system prompt, unbounded
and turn-blind. Phase 79 makes the Soul *adaptive*: per-turn
semantic selection of the relevant facets, reusing the Phase
76 begin_turn seam and the Phase 78 trust surface. Zero new
deps.

- **`SystemPromptRefiner` hook (Q1a):** sibling of Phase 76's
  `ContextProvider` in `llm_planner.rs` (not `lib.rs` —
  reachable via `aivyx_core::llm_planner::`); `begin_turn`
  swaps the turn's system prompt on `Some`. The planner is
  per-turn so the swap is naturally turn-scoped.
- **Core invariant (Q2a), structurally enforced:**
  `reduce_persona` copies scalar identity +
  `behavioral_constraints` through *unconditionally*; `keep`
  only ever touches the six soft list categories. No caller
  can drop identity or guardrails — proven end-to-end.
- **`PersonaContextRefiner` (Q3a):** embed the message,
  cosine-rank facets, top-K above a floor, re-assemble via the
  *unchanged* `assemble_session_prompt`. Below a size
  threshold / no embedding / embed failure → `None` →
  byte-identical full Persona. The feature is invisible until
  the Soul is large enough to need bounding.
- **Legible (Q4a):** per-turn `aivyx-pa persona: injected N/M
  facets` breadcrumb + a `persona_selection` field on the
  Phase 78 `GetLearningInsights` surface (CLI + Web UI).

Streak all three correct: DESIGN.md → 26, PRODUCT.md → 19,
`aivyx-core/src/lib.rs` → **27** (new record, beats Phase
78's 26) — every line lives in `aivyx-channel` /
`llm_planner.rs`, reusing existing types; `lib.rs`
byte-identical, the streak-shaped-architecture discipline
continued. Tests +14 (1473 → 1487) — just under the
calibrated +15-25 (4th consecutive small miss; the band has
converged — reuse-heavy phases land ~+10-15, this had genuine
new selection logic so topped that band at +14). No
deviations: every planned surface shipped as scoped. Zero
clippy warnings. Zero new workspace deps.

Likely follow-ups (`[persona]` tuning block, Persona
consolidation/supersession/decay, behavioural Persona,
conversational-window selection) are operator-feedback-gated.

## Phase 80 — Proactive Surfacing (the assistant brings things to you)

**Frozen — see [PHASE_80.md](archive/phases/PHASE_80.md).** For 79 phases the
assistant only ever acted when prompted. Phase 80 is the
capstone of the 75–79 arc: on its existing reflection cadence
it notices a concrete, high-confidence reason to reach out and
**surfaces it unprompted** — the single biggest step from "a
tool you query" to "an assistant that brings things to you."
An unprompted *outbound* message is the highest-trust-stakes
action, so it ships off by default, hard-capped, and fully
explainable. Zero new deps.

- **Piggyback the reflection cron (Q1a):** no new scheduler;
  the reflection pass also runs the detector, `RecallFeedback`
  wiring precedent. No `[proactive]` / disabled / no schedule
  → complete no-op (pre-Phase-80 behaviour).
- **Structural gate, no extra LLM (Q2a):** surfaces only on a
  concrete reason in three conservative classes — `TtlExpiry`
  (entry near TTL eviction), `RecallCluster` (Phase-77 net
  helpfulness strongly positive), `DueReminder` (`@due:` time
  arrived). The Phase 77 no-self-judgement ethos applied to
  the highest-stakes action; pure, per-class + empty tested.
- **Reuse the notify dispatcher (Q3a):** every send is a
  normal auto-notify — same `AutoNotifyDispatched` audit
  event, same notify history, Phase 73's per-target
  rate-limit.
- **Default-off, capped, explainable (Q4a):** opt-in
  `[proactive]` in `aivyx-config`; an HKDF-isolated
  `KeyDomain::ProactiveLog` never-nag dedup store
  (`was_surfaced` + GC clamp); a deterministic hard
  `max_per_window` cap on top of the rate-limit; per-cycle
  breadcrumb + a `proactive` field on the Phase 78
  `GetLearningInsights` surface (CLI + Web UI).

Streak all three correct: DESIGN.md → **27**, PRODUCT.md →
**20**, `aivyx-core/src/lib.rs` → **28** (new record, beats
Phase 79's 27) — the proactive pass is a structural,
no-LLM/no-turn composition through the existing dispatcher, so
it produced the *existing* audit event with no new `AuditTag`;
every line lives in `aivyx-channel` / `aivyx-config` /
`aivyx-storage`; `lib.rs` byte-identical, the
streak-shaped-architecture discipline continued. Tests **+20**
(1487 → 1507) — **in band** (predicted ~+14-22, top), the
first in-band landing after four consecutive small misses: a
new KeyDomain + config section + a heavily-tested pure
detector + an integration pass carried the genuine new
surface the converged calibration anticipated. No deviations:
every planned surface shipped as scoped. Zero clippy warnings.
Zero new workspace deps.

Likely follow-ups (standalone `[[proactive_schedule]]`,
LLM-composed proactive prose, conversational/interactive
proactive, additional signal classes) are
operator-feedback-gated.

## Phase 81 — Persona Lifecycle (the Soul that refines itself)

**Frozen — see [PHASE_81.md](archive/phases/PHASE_81.md).** For 80 phases
the Persona only ever grew. Phase 81 closes the open half of
the identity arc (the Phase 79 deferral, made consequential by
Phase 80): on the existing reflection cadence the assistant
*proposes* consolidation of near-duplicate facets and decay of
long-unreinforced ones — the "self-improving" half of the
identity layer. Zero new deps.

- **Piggyback the reflection cron (Q1a):** `PersonaLifecycleDeps`
  threaded into `run_reflection_scheduler` exactly like Phase
  77/80; no new scheduler. No section / disabled / no schedule
  → complete no-op.
- **Propose-only, operator-gated (Q2a):** the pass files
  normal Pending `PersonaProposal`s (`RemoveList` ops) and
  **never resolves** them — the Phase 77/79 no-self-mutation
  ethos applied to the highest-stakes layer; `Revert` makes
  every approved action reversible. Consolidation removes the
  shorter near-duplicates and keeps the longest *existing*
  member, so no `AppendList` is needed and each removal is
  independently safe (a deliberate simplification of the plan's
  "RemoveList×N + AppendList" recipe — see prediction vs.
  reality).
- **Structural + embedding, no LLM (Q3a):** `soft_facets_of`
  is the core-protection choke point (six soft lists only —
  scalars + `behavioral_constraints` can never become an
  action, the Phase 79 invariant extended, proven by test);
  consolidation reuses the Phase 79 cosine seam, decay is
  age-based; deterministic, no new persistence.
- **Default-off, legible (Q4a):** opt-in `[persona_lifecycle]`
  in `aivyx-config`; deterministic proposal ids dedup across
  cycles against the chain in *any* status (never nag); a
  per-cycle breadcrumb + a `persona_lifecycle` field on the
  Phase 78 `GetLearningInsights` surface (CLI + Web UI).

Streak all three correct: DESIGN.md → **28**, PRODUCT.md →
**21**, `aivyx-core/src/lib.rs` → **29** (new record, beats
Phase 80's 28) — lifecycle actions are existing-shape
proposals through the existing proposal/persona chains, so no
new `KeyDomain` and no new `AuditTag`; every line lives in
`aivyx-channel` / `aivyx-config`; `lib.rs` byte-identical, the
streak-shaped-architecture discipline continued. Tests **+17**
(1507 → 1524) — **in band** (predicted ~+16-22), the second
consecutive in-band landing; slightly lighter than Phase 80's
+20 exactly as predicted (no new KeyDomain), the
embedding-clustering detector + core-protection invariant +
config carrying the genuine new surface. One scoped
simplification (consolidation needs no `AppendList`), recorded
honestly. Zero clippy warnings. Zero new workspace deps.

Likely follow-ups (helpfulness-driven decay,
contradiction-based supersession, standalone
`[[persona_lifecycle_schedule]]`, facet-scoped one-click
revert) are operator-feedback-gated.

## Phase 82 — Persistent Helpfulness Ledger (durable, longitudinal self-learning)

**Frozen — see [PHASE_82.md](archive/phases/PHASE_82.md).** For 81 phases
the "did recalling this topic help" signal was ephemeral
(Phase 77 recomputed a tally each window and discarded it) —
the common cause behind Phase 81's age-only decay, Phase 77's
deferred cross-session patterns, and Phase 78's deferred
longitudinal history. Phase 82 makes it durable: a persisted,
per-topic, time-decayed EWMA folded in on the existing
reflection cadence. Zero new deps.

- **Zero-config (Q4a):** the Phase 77 / `RecallEvents`
  precedent — a passive internal signal, auto-initialised, no
  `[…]` block. New HKDF-isolated `KeyDomain::HelpfulnessLedger`
  (15th variant, the Phase 80 checklist). It changes no
  behaviour on its own (folded *after* the recall-feedback
  actuators, byte-identical).
- **Recency-weighted EWMA (Q1a/Q2a):** per topic
  `{ ewma_score, samples, last_update_secs }`; each cycle the
  stored score is decayed by `0.5^(dt/half_life)` (~60d) then
  the window net added. A topic that stopped helping fades on
  its own; per-topic grain survives memory eviction. Self-
  pruning (decayed-to-epsilon + stale → dropped) so growth
  mirrors the signal's own decay.
- **Fold-in on the existing pass (Q… cadence):** the Phase 77
  `run_recall_feedback_pass` aggregates `tally.ranked()` to a
  per-topic net and folds + prunes — no new scheduler, no new
  pass, threaded via the existing `RecallFeedbackDeps`.
- **Surface-only this phase (Q3a):** the read-only
  `GetLearningInsights` gains the decayed accumulated
  top-helpful/unhelpful view + sample counts (the Phase 78
  longitudinal deferral), rendered in the CLI + Web UI. It
  does **not** rewire Phase 81 decay — the topic→category
  mapping is the explicit next phase.

Streak all three correct: DESIGN.md → **29**, PRODUCT.md →
**22**, `aivyx-core/src/lib.rs` → **30** (new record, beats
Phase 81's 29) — the new store is a `KeyDomain` in
`aivyx-storage`; the ledger, fold-in, and surface live in
`aivyx-channel`; no new `AuditTag`, `lib.rs` byte-identical.
Tests **+10** (1524 → 1534) — a **MISS below the predicted
~+18-24 band** (first miss after two in-band landings): the
prediction over-weighted "has a new KeyDomain ≈ Phase 80's
+20" and under-weighted that this phase is *zero-config*
(no config-validation tests — Phase 80 had ~6) and
*surface-only* (no detector/behaviour test breadth). The
honest recalibration: a new KeyDomain alone is ~+7; it is the
config + detector + behaviour breadth that drives test count,
not the storage variant. Every planned surface still shipped;
this is a calibration miss, not a scope miss. One unplanned
**test-only** fix (a pre-existing `pid-nanos` shared-store
path collision in `storage_persistence_e2e`, exposed by the
extra KeyDomain shifting redb-open timing under loaded
parallel runs — fixed with a uuid suffix; no production
change, no new dep). Zero clippy warnings. Zero new workspace
deps.

Likely follow-ups (helpfulness-driven Persona decay — needs a
topic→category mapping; operator-tunable half-life/retention;
topic canonicalization) are operator-feedback-gated.

## Phase 83 — Cross-Session Pattern Learning (the durable co-occurrence ledger)

**Frozen — see [PHASE_83.md](archive/phases/PHASE_83.md).** Phase 77's
headline deferral, unblocked by the Phase 82 durable-ledger
model. Per-topic helpfulness is shallow; the relationships
*between* topics — which travel together and jointly help —
are where cross-session structure lives. Phase 83 adds a
persistent, time-decayed **co-occurrence ledger** folded in on
the existing reflection cadence. Zero new deps.

- **Co-occurrence pairs (Q1a):** unordered `{A,B}` recalled in
  the same `RecallEvent`; the Phase 77 uniform per-event
  signal makes "this co-recalled set landed in a helpful
  turn" directly observable. Sequential/n-ary defer.
- **Durable derived store (Q2a):** a new HKDF-isolated
  `KeyDomain::CooccurrenceLedger` (16th), key = a
  collision-safe **length-prefixed** canonical pair (so
  `{A,B}=={B,A}` yet `("a|","b")≠("a","|b")`), value = the
  Phase 82 EWMA `{score,samples,last_update}`. The raw recall
  log GC's at 30 days, so the durable derived store is the
  only genuine cross-session path. Exactly the Phase 82
  model.
- **Surface-only (Q3a):** detect + persist + show on the
  Phase 78 surface; change no behaviour. Consumption is the
  explicit next phase — the Phase 82 substrate-then-
  consumption discipline.
- **Zero-config, bounded (Q4a):** the Phase 77/82
  passive-signal precedent; the O(n²) blowup bounded by
  folding only pairs among the **top-8 highest-scoring
  distinct topics** per event; EWMA-decay + self-prune;
  surfaced on `GetLearningInsights` (CLI + Web UI) +
  breadcrumb; constants not config.

Streak all three correct: DESIGN.md → **30**, PRODUCT.md →
**23**, `aivyx-core/src/lib.rs` → **31** (new record, beats
Phase 82's 30) — the new store is a `KeyDomain` in
`aivyx-storage`; the ledger, fold-in, and surface live in
`aivyx-channel`; no new `AuditTag`, `lib.rs` byte-identical.
Tests **+10** (1534 → 1544) — **a small miss vs the
phase-specific ~+12-16 refinement**, but squarely in the
Phase-82-recalibrated ≈ +8-12 band: the cross-session pair
detector landed at *exactly* Phase 82's +10, not above it.
The empirical lesson (refined again): a zero-config,
surface-only, one-new-KeyDomain phase is ≈ +10 regardless of
whether the store is per-topic or per-pair — the test surface
is dominated by the fixed scaffolding (KeyDomain isolation ×2,
ledger CRUD/decay/prune ×~6, integration ×1, render ×1), not
by detector complexity. Every planned surface shipped. One
in-scope clippy resolution: the `LearningInsights` protocol
payload accretes one read-only field per learning phase, so
`#[allow(clippy::large_enum_variant)]` on the
`DaemonMessage`/`DaemonEnvelope` envelopes with justification
(consistent with the codebase's existing
`#[allow(too_many_arguments)]` practice). Zero clippy
warnings. Zero new workspace deps.

Likely follow-ups (helpfulness-driven Persona decay; pattern
*consumption* — cluster-aware co-recall, pattern-driven
proposals; sequential/temporal patterns; n-ary clusters;
operator-tunable top-K/half-life) are operator-feedback-gated.

## Phase 84 — Cluster-Aware Co-Recall (the first consumption phase)

**Frozen — see [PHASE_84.md](archive/phases/PHASE_84.md).** Phases 82–83
built durable learning substrate surface-only; Phase 84 is the
first phase that *acts* on it, consuming the freshest piece
(the Phase 83 co-occurrence ledger). Auto-recall (Phase 76)
surfaces only literal keyword/semantic matches; now, when a
topic is recalled, its durable affined siblings the query
missed are also surfaced — recall becomes associative. Zero
new deps.

- **Bounded sibling injection (Q1a):** after the base recall,
  `siblings_of` (a new one-scan per-topic ledger query) yields
  the strongest affined siblings; the most-recent memory under
  each new sibling topic is injected. Re-rank-only was
  rejected — it cannot surface a sibling the query never
  retrieved.
- **Opt-in (Q2a):** `[recall_cluster]` in `aivyx-config`, off
  by default — the first phase that changes hot-path context,
  so the Phase 80/81 behaviour-change discipline.
- **Self-policing (Q3a):** a serde-safe `cluster` marker on
  `RecallHit`; the Phase 83 co-occurrence fold *excludes*
  marked hits (the ledger never learns from its own expansion
  — no runaway self-reinforcement), while the Phase 77/82
  helpfulness signal still measures them (a bad expansion
  self-penalises and the driving affinity decays).
- **Budget-neutral + legible (Q4a):** siblings share the
  existing `rag_top_k` budget (displace the weakest primary
  hits — zero context/token growth); hard `max_siblings` cap
  + `min_affinity` floor; a per-turn breadcrumb + a Phase 78
  `cluster_recall` surface (CLI + Web UI), the persona-
  selection shared-handle pattern.

Streak all three correct: DESIGN.md → **31**, PRODUCT.md →
**24**, `aivyx-core/src/lib.rs` → **32** (new record, beats
Phase 83's 31) — the recall provider is the existing
`ContextProvider` (the Phase 76/79 `llm_planner.rs` seam, not
`lib.rs`); config/query/marker/expansion/surface all in
`aivyx-channel` / `aivyx-config`; no new `AuditTag`,
byte-identical `lib.rs`. Tests **+10** (1544 → 1554) — **a
second consecutive miss vs the ~+16-22 prediction**; it
landed at the same flat ≈ +10 as Phases 82/83. The recurring,
now-confirmed calibration law: realized test count is driven
by **new standalone pure modules each carrying a per-branch
unit suite** (a detector ≈ +7, a new `KeyDomain` ≈ +2, a
config section ≈ +5-6) — *not* by config-presence or
hot-path-behaviour breadth. Phase 84 added **no** new detector
module and **no** new `KeyDomain` (it reused `memory_recall` +
the Phase 83 ledger, integration-testing the new behaviour),
so it sits at the floor ≈ +10 regardless of being a
config+behaviour phase. Future predictions: count new
unit-tested pure modules, not surface area. A calibration
miss, not a scope miss — every planned surface shipped. Two
in-scope clippy resolutions, recorded honestly:
`#[allow(large_enum_variant)]` was already in place (Phase
83); `render_insights` crossed `too_many_arguments` (one
display param per learning phase) → justified
`#[allow(too_many_arguments)]`, the Phase 81 fire_reflection
precedent. Zero new clippy warnings. Zero new workspace deps.

Likely follow-ups (pattern-driven Persona proposals; affinity
re-ranking of existing candidates; sequential/temporal
patterns; operator-tunable affinity policy) are
operator-feedback-gated.

## Phase 85 — Helpfulness-Driven Persona Decay (the self-improving Soul, completed)

**Frozen — see [PHASE_85.md](archive/phases/PHASE_85.md).** The symmetric
consumption to Phase 84 and the long-deferred Phase 81+82
capstone: Phase 81 decay was age-only; Phase 85 wires the
durable Phase 82 helpfulness ledger so the Soul retires
identity that demonstrably *stopped helping* and protects old
identity that *still helps*. Zero new deps.

- **Provenance-only association (Q1a):** the topic is
  recovered structurally from `proposal_id ==
  "recall-fb:{topic}"` (it survives onto the
  `PersonaDelta`); only recall-feedback-derived facets are
  helpfulness-gated, reflection-authored facets stay age-only
  (no fuzzy embedding driving identity removal). The
  long-flagged "blocker" turned out to be exact.
- **Symmetric gate (Q2a):** sustained-negative triggers decay
  before the age horizon; sustained-positive protects an
  age-old facet from age-decay. Propose-only + `Revert` +
  operator-gated + core-protected — every Phase 81 safety
  property carries over.
- **Conservative evidence (Q3a):** decayed EWMA at/below
  `decay_unhelpful_threshold` **and** `samples >=
  decay_min_samples`; never on thin evidence; recency is
  inherent in the ledger half-life.
- **Cohesive + graceful (Q4a):** two knobs on the existing
  `[persona_lifecycle]` block (gated by `signal_decay`); no
  helpfulness ledger → pure age-only, byte-identical to Phase
  81; the decay `reason` cites the evidence; the existing
  Phase 78 lifecycle surface renders it unchanged (no new IPC
  field).

Streak all three correct: DESIGN.md → **32**, PRODUCT.md →
**25**, `aivyx-core/src/lib.rs` → **33** (new record, beats
Phase 84's 32) — decay actions remain existing-shape
proposals through the existing chains; detector/pass/config/
surface all in `aivyx-channel` / `aivyx-config`; no new
`AuditTag`, byte-identical `lib.rs`. Tests **+5** (1554 →
1559) — below the ~+8-12 nominal but exactly the "slightly
under, no new config *section*" case the open doc explicitly
hedged: the now-converged calibration law (count tracks new
unit-tested pure modules) gains a sharper sub-rule —
**config *knobs on an existing block* ≈ +1, extension-only
detector/pass changes ride existing test files (≈ +3 units +
1 integration)**, so a phase with no new module, no
`KeyDomain`, *and* no new config section floors at ≈ +5, not
+10. First prediction in the recent run to call the
under-shoot direction correctly (the hedge held). A
*calibration* refinement, not a *scope* miss — every planned
surface (provenance recovery, the symmetric gate, the
evidence floor, graceful no-ledger fallback, evidence-cited
reason) shipped exactly as scoped. Zero clippy warnings. Zero
new workspace deps. With no ledger, decay is byte-identical
to Phase 81 (asserted).

Likely follow-ups (helpfulness-driven *consolidation*;
helpfulness decay for reflection-authored facets via fuzzy
embedding; pattern-driven Persona proposals; topic
canonicalization) are operator-feedback-gated.

## Phase 86 — Conversational-Window Relevance (sharpening the whole stack's input, completed)

**Frozen — see [PHASE_86.md](archive/phases/PHASE_86.md).** The twice-deferred
(Phase 76 *and* Phase 79) input-quality gap: for 85 phases the
assistant judged relevance off *one line*. Phase 86 gives both
relevance consumers — auto-recall (76) and adaptive Persona
selection (79) — a recent conversational window: a small,
recency-ordered slice of the last few `(user, assistant)`
turns concatenated into the same single embed they already
make (current message last so it dominates). Sharper input
under the entire self-learning stack with zero new deps and
opt-in defaults.

- **Daemon-scoped session-keyed buffer (Q1a):** a new
  `conversation_window` module in `aivyx-channel` —
  `Arc<RwLock<HashMap<SessionId, ConversationWindow>>>` shared
  at daemon startup (the Phase 82/84 shared-handle precedent),
  written by the daemon turn loop on each
  `TurnOutcome::Completed`. Ephemeral by design (no new
  `KeyDomain`); a restart starts fresh.
- **Single-vector composition (Q2a):** the last
  `recall_window_turns - 1` prior turns (oldest → newest,
  role-labelled) followed by the current message, char-budgeted
  with oldest-first eviction; current message is never
  truncated. One embed call, unchanged ranking math.
- **Both consumers (Q3a):** auto-recall *and* Persona
  selection — same seam, same knob; the deferral came from
  both, fixing one without the other was incoherent.
- **Opt-in, byte-identical by default (Q4a):**
  `[embedding].recall_window_turns` (default `1` = exactly the
  latest single message); below the floor the embedded query
  is bit-for-bit pre-Phase-86. The trait-extension ripple
  (adding `session_id` to `SystemPromptRefiner::refine`) lives
  in `llm_planner.rs`, *not* the byte-identical `lib.rs`. A
  positive cascade: the daemon's `Message::session_id` is now
  stable across turns (was fresh per turn), which both makes
  the buffer key load-bearing and corrects Phase 77 recall
  correlation.

Streak all three correct: DESIGN.md → **33**, PRODUCT.md →
**26**, `aivyx-core/src/lib.rs` → **34** (new record, beats
Phase 85's 33) — the trait edit lives in `llm_planner.rs`, the
new module + handle thread through `aivyx-channel`, and the
config knob is a field on the existing `[embedding]` block.
Test count delta within the converged calibration band — one
new pure module + a knob on an existing config section + two
provider integration tests on each side (the assemble-engaged
and fallback-matrix pairs). Zero clippy warnings. Zero new
workspace deps. With `recall_window_turns = 1` (the default),
both consumers are byte-identical to pre-Phase-86 (asserted on
both providers via recording-provider matrix tests).

Likely follow-ups (token-budget context sizing, embed-each-
and-pool windows, persisted windows, heuristic recall gate)
are operator-feedback-gated.

## Phase 87 — Pattern-Driven Persona Proposals (the self-improving Soul, the second consumption, completed)

**Frozen — see [PHASE_87.md](archive/phases/PHASE_87.md).** Closes the
deliberate Phase 85 deferral. After 84 (recall acts on the
Phase 83 co-occurrence ledger) and 85 (decay acts on the
Phase 82 helpfulness ledger), the visible asymmetry was that
the co-occurrence ledger fed only *recall*, not the *Soul*.
Phase 87 closes the symmetric arc: durable consistently-
co-occurring pairs of *helpful* topics propose a new
`learned_context` facet through the existing Phase 70 chain —
same propose-only + edit-then-approve + Revert + core-
protected flow, just driven by the second durable signal.

- **Conservative double-gate (Q1a):** pair affinity AND both
  endpoints helpful — a pattern made of topics that
  individually hurt is never proposed. Mirrors Phase 85's
  evidence-floor discipline; same `min_topic_helpfulness`
  knob defaults to `0.0` (non-negative).
- **LLM-summarized facets (Q2b):** the existing reflection
  LLM phrases each surviving pair into a one-sentence
  `learned_context` facet. The operator is still the final
  filter (edit-then-approve / reject); a per-candidate LLM
  hiccup skips that pair, a cycle-wide outage records
  `llm_unavailable = true` on the Phase 78 surface so a quiet
  cycle stays distinguishable from a broken one.
- **Reflection cron + cap + dual dedup (Q3a):** the
  established "act on durable learning" cadence (Phase 77 /
  82 / 83 / 85); idempotent — a pair already in the proposal
  chain (any status) is never re-filed; per-cycle filings
  bounded by `max_proposals_per_cycle`.
- **Opt-in by default (Q4a):** new `[persona_consolidation]`
  config block, `enabled = false` default; with no block the
  pass never runs (byte-identical to pre-Phase-87). The Phase
  80/81/84 actuator posture.

Streak all three correct: DESIGN.md → **34**, PRODUCT.md →
**27**, `aivyx-core/src/lib.rs` → **35** (new project
record, beats Phase 86's 34) — the new pass + config block +
Phase 78 surface stat all live in `aivyx-channel` /
`aivyx-config` / `bin/aivyx-pa`; proposals land through the
existing `PersistentPersonaProposalLog::append` API (no new
chain operation); no new `AuditTag`. Test count delta within
the predicted `+11-15` band (`+15` exactly — config section
+6, pure module +8, integration +1). Zero clippy warnings.
Zero new workspace deps.

Likely follow-ups (**pattern-driven Persona *decay* —
shipped in Phase 88**, n-ary cluster proposals, pattern-
driven supersession, operator-tunable LLM prompt, topic
canonicalization) are operator-feedback-gated.

## Phase 88 — Pattern-Driven Persona Decay (the decay-side of the Phase 87 arc, completed)

**Frozen — see [PHASE_88.md](archive/phases/PHASE_88.md).** Closes the
deliberate Phase 87 deferral with the decay half of the
symmetric arc. After Phase 87 made the Phase 83 co-occurrence
ledger drive Persona *construction*, Phase 88 makes the
**same ledger** drive Persona *decay*: a `consolidate-pair:`
facet whose underlying pair has demonstrably weakened
(decayed Phase 83 affinity below the new
`decay_pair_below_affinity` floor) is decay-proposed — the
relationship that justified the identity no longer holds.
Symmetrically, a still-durable pair **protects** its facet
from age-decay (the relationship still applies, so the
identity still applies). After Phase 88, every durable
learning signal is consumed on both sides of the Soul
lifecycle.

- **Single-signal decay (Q1a/Q2a):** decayed affinity below
  the floor is sufficient. Endpoint helpfulness is not
  double-consulted; conditioning would leave drifted-but-
  warm pair facets in place forever — the very case Phase
  88 is meant to handle.
- **Symmetric protection (Q3a):** pair at/above the floor
  protects from age-decay. Reuses the Phase 85 OR-protection
  machinery on a parallel provenance arm.
- **Same `[persona_lifecycle]` block (Q4a):** new
  `decay_pair_below_affinity` knob (default `1.0` — mirrors
  Phase 87's `min_affinity` so the construction and decay
  floors coincide by default; operators tune below for
  hysteresis). Gated by the existing `signal_decay`.

Streak all three correct: DESIGN.md → **35**, PRODUCT.md →
**28**, `aivyx-core/src/lib.rs` → **36** (new project
record, beats Phase 87's 35) — detector extension + config
knob + fold-site read all live in `aivyx-channel` /
`aivyx-config`; the decay proposals land through the existing
`PersistentPersonaProposalLog::append` API (no new chain
operation, no new `AuditTag` — the Phase 85 precedent).
Test count delta `+6` (`+1` config, `+4` detector, `+1`
integration) — inside the predicted `+3-7` band. Zero clippy
warnings. Zero new workspace deps. With no co-occurrence
ledger the pair arm sits out entirely → byte-identical to
Phase 85 (asserted on the existing Phase 85 test, which
runs with `cooccurrence_ledger: None`).

## Phase 89 — Topic Canonicalization (sharper learning through sharper bookkeeping, completed)

**Frozen — see [PHASE_89.md](archive/phases/PHASE_89.md).** Closes the
longest-standing learning-stack deferral — the Phase 82
deferral carried forward six times through Phases 83-88. For
88 phases every topic-keyed accumulator (the Phase 7 memory,
the Phase 77 recall log, the Phase 82 helpfulness ledger,
the Phase 83 co-occurrence ledger, the Phase 87 consolidate-
pair proposal IDs) keyed by the operator's typed topic string
verbatim — so `deploy`, `Deploy`, `deploys`, `deploying` were
four distinct topics across every signal, and the value that
should add up across them was silently fragmented. Phase 89
is the first phase since the act-on-durable-learning arc
closed that **sharpens existing signals** rather than adding
a new capability — the natural infrastructure pause before
the next big surface.

The fix is the smallest possible substrate change: an opt-in
canonicalization seam at the `Memory` trait's topic-string
boundary. Every downstream consumer inherits clean signal
through the existing pipeline — no per-layer plumbing.

- **Hand-rolled English stemmer (Q1a):** lowercase + trim +
  whitespace fold + one suffix-strip rule with min-length
  guards (`ies → y`, `ing` / `ed` / hissing-`es` / `s`).
  The `es` rule fires only when the stem ends in a hissing
  sound (`sh` / `ch` / `s` / `x` / `z`) — the real English
  plural rule — so `boxes → box` but `roles` falls through
  to `s` rule → `role`. Idempotent. Zero new deps.
- **Write-side only (Q2a):** no migration. Existing
  fragmented signal decays out via the Phase 82/83 ~60-day
  half-life + the Phase 77 ~30-day recall-log retention;
  the past converges to clean within ~quarter without
  intervention; MAC-signed Persona chain entries stay
  untouched.
- **`Memory::put` boundary (Q3a):** a single wrapper-
  delegate (`CanonicalizingMemory`) canonicalizes at every
  topic-keyed trait entry point (`put` / `get_recent` /
  `forget` / `gc_topic` / `evict_oldest_unread` /
  `put_vector` / `promote_recall_helpful`). One
  canonicalization site per method; prefix matching,
  text-search queries, and topic-less methods pass through
  unchanged.
- **Opt-in (Q4a):** `[memory].canonicalize_topics: bool`,
  default `false`. Matches the 88-phase
  behaviour-change-is-opt-in discipline; with the flag off
  the memory layer is byte-identical to pre-Phase-89.

Streak all three correct: DESIGN.md → **36**, PRODUCT.md →
**29**, `aivyx-core/src/lib.rs` → **37** (new project record,
beats Phase 88's 36) — the canonicalization function +
wrapper live in `aivyx-memory`, not `aivyx-core`; the config
knob + binary wiring touch no production-core code. Test
count delta `+20` workspace (`+3` config, `+13` pure module,
`+4` wrapper) — comfortably above the predicted `+6-10`
band; the rich rule coverage in Task 3 (each suffix rule +
the hissing-sound guard + idempotency + non-ASCII pass-
through + path-like-topic + short-string guards) all earned
their own test. Zero clippy warnings. Zero new workspace
deps. With `canonicalize_topics = false` (the default), the
memory layer is byte-identical to pre-Phase-89 (asserted on
the `without_wrapper_variants_stay_distinct_baseline` test).

Likely follow-ups (operator-tunable `[[topic_alias]]`
mappings, topic-by-topic exception list, one-time migration
of existing fragmented data, non-ASCII / Unicode stemming)
are operator-feedback-gated.

## Phase 90 — Heuristic Recall Gate (the third move in the input-quality arc, completed)

**Frozen — see [PHASE_90.md](archive/phases/PHASE_90.md).** Closes the
longest-running recall-side deferral (Phase 76, carried
forward 14 phases through 77-89). For 89 phases both
auto-recall (Phase 76) and adaptive Persona selection
(Phase 79) fired on **every** conversational turn —
including turns where the user message is a single-token
acknowledgment (`ok` / `thanks` / `yes` / `cool`) that
cannot meaningfully steer recall or facet selection.

Phase 90 closes this with the smallest possible gate: a
length-based heuristic at the top of both relevance hooks
that skips the embed (and everything downstream) when the
trimmed user message is shorter than `recall_gate_min_chars`
Unicode characters. Both consumers share the same gate
under one opt-in knob, exactly as Phase 86's window work
shipped both consumers under one switch.

The third move in the input-quality arc:
- **Phase 86** — sharpened *what* gets embedded (windows)
- **Phase 89** — sharpened *how* signals key (canonical)
- **Phase 90** — sharpens *when* recall fires at all

- **Length-based gate (Q1a):** trimmed Unicode-char count
  strictly-less-than `recall_gate_min_chars` → short-
  circuit. Simple, deterministic, language-agnostic.
- **Opt-in (Q2a):** `[embedding].recall_gate_min_chars`,
  default `0` (gate disabled = byte-identical to
  pre-Phase-90). Matches the 89-phase
  behaviour-change-is-opt-in discipline.
- **Both consumers (Q3a):** `SemanticMemoryContext::recall`
  AND `PersonaContextRefiner::refine` short-circuit on the
  same gate under the same shared knob. Symmetric Phase 86
  design.
- **Skip embed entirely (Q4a):** the gated turn pays zero
  embed cost (not just memory-walk or ranking) — the
  cheapest possible noise-turn path; uses the existing
  best-effort `None` fallback contract both providers
  already honoured.

Streak all three correct: DESIGN.md → **37**, PRODUCT.md →
**30**, `aivyx-core/src/lib.rs` → **38** (new project
record, beats Phase 89's 37) — the gate function +
provider short-circuits live in `aivyx-channel`, not
`aivyx-core`; the config knob is a new field on the
existing `EmbeddingConfig`; zero new `AuditTag`. Test count
delta **`+15`** workspace (`+3` config, `+6` pure helper,
`+6` provider integration) — over the predicted `+6-10`
band; the recording-provider matrix on both providers
earned its own coverage. Zero clippy warnings. Zero new
workspace deps. With `recall_gate_min_chars = 0` (the
default), both providers are byte-identical to pre-Phase-90.

Likely follow-ups (pattern-based stoplist, LLM-judged gate,
adaptive thresholds from operator message-length
distribution, token-budget context sizing) are
operator-feedback-gated.

## Phase 91 — LLM-Judged Recall Usefulness (the missing half of the learning loop, completed)

**Frozen — see [PHASE_91.md](archive/phases/PHASE_91.md).** Closes the
longest-running feedback-side deferral — the Phase 77
deferral carried forward 14 phases through 78-90. For 90
phases the recall-feedback signal has been STRUCTURAL: every
recall in a successfully-completed turn inherits `+1`
helpfulness, every recall in a failed turn inherits `-1`. The
proxy works but operates at turn-level granularity. Phase 91
adds an opt-in LLM-judged per-recall classification
alongside the structural proxy — a 3-way verdict
(`Used` / `Irrelevant` / `Hurt`) recorded on each recall
hit, finer than turn-level.

After the input-quality arc (86 windows, 89 canonical, 90
gate), Phase 91 is the symmetric **feedback-quality** move
that completes the learning-loop picture:

|                                                | Sharpens               |
|------------------------------------------------|------------------------|
| Phase 86 — Conversational window               | *What* gets embedded   |
| Phase 89 — Topic canonicalization              | *How* signals key      |
| Phase 90 — Heuristic recall gate               | *When* recall fires    |
| **Phase 91** — LLM-judged recall usefulness    | **Whether** it helped  |

- **Reflection cron, batched (Q1a):** one LLM call per
  cron tick judging every unjudged recall in the lookback
  window (up to `max_recalls_per_cycle`); the remainder
  rolls to the next cycle. Bounded cost.
- **3-way structured (Q2a):** the LLM produces `Used` /
  `Irrelevant` / `Hurt` per recall — easy to aggregate to
  the existing `+1 / 0 / -1` helpfulness shape, deterministic
  decode, trivially mockable.
- **Augment, NOT replace (Q3a):** the new
  `judgment: Option<RecallJudgment>` field on `RecallHit` is
  captured but no existing accumulator (Phase 82 helpfulness
  ledger, Phase 83 co-occurrence ledger, Phase 85/88 decay,
  Phase 87 proposals) consumes it in v1. Every existing
  behaviour stays byte-identical to pre-Phase-91. A future
  phase reads the new signal once it's validated in
  production.
- **Opt-in (Q4a):** `[recall_judgment].enabled`, default
  `false`. The LLM call has real cost; the operator opts
  into paying it.

Streak all three correct: DESIGN.md → **38**, PRODUCT.md →
**31**, `aivyx-core/src/lib.rs` → **39** (new project
record, beats Phase 90's 38) — the new trait + adapter +
pass + stat + IPC field all live in `aivyx-channel`; the
config block + provenance tracking in `aivyx-config`; no
new `AuditTag` (the judgment is recorded on the existing
recall log, not on the audit chain). Test count delta
**`+14`** workspace (`+5` config, `+8` module + wire-compat,
`+1` reflection-scheduler integration) — inside the
predicted `+9-15` band. Zero clippy warnings. Zero new
workspace deps.

**v1 simplification.** The judge classifies based on the
recall's `(topic, body)` content + a weak context hint —
not the model's actual response text (which isn't in the
audit chain today). The Q3a augment posture means even this
weaker v1 judgment changes nothing; future phases enrich
the input via audit-chain extension or per-turn capture.

Likely follow-ups (actuator-side switch from structural
proxy to the new judgment signal, per-recall LLM critique,
adaptive batch size, multi-model ensembling, response-text
recovery via audit-chain extension) are operator-feedback-
gated.

## Phase 92 — Pattern-Driven Supersession (the longest-running Persona-actuator deferral, closed)

**Frozen — see [PHASE_92.md](archive/phases/PHASE_92.md).** Closes the
Phase 70 proposal-supersession deferral — 22 phases old,
deferred again at Phase 87 and Phase 88. After Phase 87
(pattern-driven construction) and Phase 88 (pattern-driven
decay), the Soul actuator handled a shifting co-occurrence
pair `(A, B) → (A, C)` as TWO independent operator
decisions. Phase 92 introduces opt-in **shared-endpoint
supersession**: when an existing applied `consolidate-pair:`
facet's pair decays while a new pair sharing one endpoint
strengthens, the consolidation pass files the `RemoveList`
+ `AppendList` proposals **linked by metadata** so the
operator-facing surface presents them as a single
supersession decision.

- **Shared-endpoint detection (Q1a):** old `(A, B)`
  decayed below the Phase 88 floor AND new `(A, C)`
  sharing one endpoint above the Phase 87 construction
  floor with both endpoints helpful. Conservative;
  deterministic; only fires on clear "replacement"
  relationships.
- **Reuse Phase 87's `PairPhraser` (Q2a):** structural
  detection is pure; the new facet's prose comes from
  the same LLM seam Phase 87 already pays. Per-candidate
  phrasing failure → skip that supersession; the facet
  stays via the standard Phase 87/88 flow.
- **Linked, not atomic (Q3a):** the new optional
  `supersedes_proposal_id: Option<String>` field on
  `ProposedPersonaDelta` carries the cross-link. The
  `AppendList`-side proposal points at the
  `RemoveList`-side proposal_id; the `RemoveList`-side
  points back. Each half remains independently
  `Revert`-able; no new proposal kind; no chain-schema
  migration. Wire-compat via the Phase 84 / Phase 91
  `#[serde(default, skip_serializing_if = "...")]`
  precedent.
- **Opt-in (Q4a):** new
  `enable_supersession: bool` knob on the existing
  `[persona_consolidation]` block, default `false`.
  Operators running Phase 87 consolidation enable it with
  one extra key; with the knob off the Phase 87 / Phase 88
  proposal flow is byte-identical to pre-Phase-92.

Streak all three correct: DESIGN.md → **39**, PRODUCT.md →
**32**, `aivyx-core/src/lib.rs` → **40** (new project
record, beats Phase 91's 39) — detector + field + pass
integration all in `aivyx-channel`; config knob in
`aivyx-config`; no new `AuditTag`. Test count delta
**`+10`** workspace (`+3` config, `+6` field + detector,
`+1` integration) — squarely inside the predicted `+6-10`
band. Zero clippy warnings. Zero new workspace deps.

Likely follow-ups (Web UI visual grouping of linked
supersession proposals, atomic chain-level supersession
primitive via a new `Supersede` proposal kind or
`Compound` proposals, n-ary cluster supersession,
semantic-similarity supersession for synonym pairs that
share zero literal endpoints) are operator-feedback-
gated.

## Phase 93 — Recall-Feedback Switches to LLM-Judgment Signal (closing the Phase 91 deferral)

**Frozen — see [PHASE_93.md](archive/phases/PHASE_93.md).** Closes the
Phase 91 deferral named verbatim in the Phase 92 open doc:
"actuator-side switch from structural proxy to the new
judgment signal." Phase 91 added the per-hit
`judgment: Option<RecallJudgment>` field on `RecallHit`
but explicitly scoped the change as **v1 augment, not
replace** — the field was recorded into the audit chain
and the Phase 78 insights surface, but no runtime
accumulator consumed it. Phase 93 wires the consumer:
`correlate_detailed` — the single source of truth that
drives memory promotion (Phase 77 Task 6) and Persona
proposal filing (Phase 77 Task 7) — now reads the per-hit
verdict where present and falls back to the existing
turn-level structural proxy where absent.

- **Augment, not replace (Q1a):** per-hit `Some(judgment)`
  overrides the turn-level structural signal for that hit;
  `None` hits keep using the structural proxy. Smooth
  migration — judgments take effect incrementally as the
  Phase 91 cron processes hits; the loop never loses
  signal while the cron catches up. Matches the Phase 91
  field doc's own framing.
- **Symmetric ±WEIGHT mapping (Q2a):**
  `Used → +WEIGHT`, `Hurt → -WEIGHT`,
  `Irrelevant → 0` (dead weight; neither rewards nor
  punishes). Same magnitude as the existing structural
  mapping — single source of magnitude.
- **Consumer-side knob (Q3a):** new
  `[recall_feedback].use_judgment_signal: bool`, default
  `false`. Matches the Phase 87 / 88 / 91 / 92 actuator
  opt-in pattern. The knob lives on the consumer
  (`[recall_feedback]`), separate from the Phase 91
  producer-side `[recall_judgment]`, so the two configs
  remain independently reason-aboutable. Turning the
  judge on alone keeps Phase 91 in visibility-only mode
  (the v1 default posture); turning both on closes the
  self-improving loop end-to-end.
- **Single-fixture integration test (Q4a):** one recall
  with three hits — `Used` / `Hurt` / un-judged — on a
  +WEIGHT structural turn. With the knob on: Used
  promoted, Hurt NOT promoted (judgment overrides
  positive structural), un-judged promoted via fallback.

Streak all three correct: DESIGN.md → **40**, PRODUCT.md
→ **33**, `aivyx-core/src/lib.rs` → **41** (new project
record, beats Phase 92's 40) — augmentation in
`aivyx-channel`; config knob in `aivyx-config`; the
`HelpfulnessTally` shape unchanged so downstream
actuators are byte-identical. Test count delta **`+11`**
workspace (`+4` config knob, `+5` augmentation +
judgment-signal mapping, `+1` reflection-cron integration,
`+1` surface banner) — one over the predicted `+6-10`
band, accounted for by the wire-compat surface field
earning its own test alongside the field plumbing. Zero
clippy warnings. Zero new workspace deps.

Mid-phase scope correction: the open commit
(`9f875c6`) scoped Phase 93 around calibrating a per-
domain `min_score` recall-gate threshold. On reading the
codebase that knob doesn't exist — Phase 90's gate is a
single global `recall_gate_min_chars` input-length check,
not a per-domain post-recall score filter. The
re-scope commit (`27d8c41`) pivoted to the loop closure
that actually exists in the codebase (the Phase 91
deferral). Phase ritual followed: new Q-block resolved
pre-Task 2.

Likely follow-ups (per-domain or per-topic weights for
the verdict→signal mapping; "replace" mode that drops
the structural fallback; asymmetric Hurt penalty; sum
mode that stacks both signals — see PHASE_93.md
deferrals list) are operator-feedback-gated.

## Phase 94 — Web UI Grouping for Linked Supersession Proposals (Phase 92's first deferral, closed)

**Frozen — see [PHASE_94.md](archive/phases/PHASE_94.md).** Closes the
Phase 92 deferral that headlined that phase's "likely
follow-ups" list: *"Web UI visual grouping of linked
supersession proposals."* Phase 92 shipped the structured
`supersedes_proposal_id` linkage on `ProposedPersonaDelta`
but rendered it only via `reason` text on each half.
Phase 94 makes the linkage operator-visible at a glance
in both surfaces: the `aivyx-pa persona proposals` CLI shows
linked pairs with `└─ supersedes:` indicators; the Web UI
Proposals tab renders the pair as one card with a primary
"Approve both" action plus a `⋮ Split` menu for partial
actions.

- **Pure structural pass (Q1a):** new
  `group_supersession_pairs` helper in `aivyx-channel`
  consumes the flat `Vec<PersonaProposal>` the IPC already
  returns and produces `Vec<ProposalRendering>`. No IPC
  contract change for the grouping itself (the
  `supersedes_proposal_id` field was added to
  `PersonaProposalSummary` with full wire-compat via
  `#[serde(default, skip_serializing_if = "Option::is_none")]`).
  Both surfaces call the same algorithm — Rust helper on
  the CLI side, line-for-line JS port on the Web UI side.
- **Generic via small trait (`GroupableProposal`):** the
  helper takes any type implementing `id()`,
  `supersedes_proposal_id()`, and `op_kind()` accessors;
  impls exist for both the typed `PersonaProposal`
  (daemon-side use) and the wire `PersonaProposalSummary`
  (CLI use). One source of truth, no algorithm
  duplication.
- **Defensive degradation:** self-reference, dangling
  partner id, asymmetric link (A→B without B→A), and
  same-op pair (two `AppendList`s or two `RemoveList`s)
  all degrade to standalone rendering rather than panic.
  The grouping invariant requires exactly one
  `RemoveList` + one `AppendList` with mutual
  cross-reference.
- **One-click ergonomics + Phase 92 guarantee preserved
  (Q2a):** the Web UI's primary action fires two
  sequential `ResolvePersonaProposal` IPC calls
  (RemoveList first, then AppendList). Phase 92's
  explicit `each half independently Revert-able`
  guarantee covers the half-approved-on-failure case
  without needing a transactional chain primitive; the
  `⋮ Split` menu exposes the partial-action paths the
  operator may need.

Streak all three correct: DESIGN.md → **41**, PRODUCT.md
→ **34**, `aivyx-core/src/lib.rs` → **42** (new project
record, beats Phase 93's 41) — grouping helper +
generic trait + CLI render in `aivyx-channel`; Web UI
changes in the embedded static HTML asset; IPC field
added with full wire-compat. Test count delta `+11`
workspace (`+8` pure helper covering Q4a mixed fixture
plus six dedicated edge cases, `+3` CLI rendering;
the Web UI JS is a port of the Rust helper covered by
those tests + manual browser testing). **One over the
predicted `+6-10` band** — accounted for by the helper
earning extra edge-case coverage (self-ref / asymmetric /
same-op / dangling / position-determinism / empty all
worth a test apiece). Zero clippy warnings. Zero new
workspace deps.

Likely follow-ups (atomic
`ResolveSupersessionGroup` IPC primitive that wraps the
two-call dance transactionally; backend-side grouping
enrichment via a `linked_with: Vec<String>` field on
`ProposalSummary`; drag-to-merge / drag-to-split UI
affordances; n-ary group rendering once Phase 83's n-ary
cluster supersession deferral closes — see PHASE_94.md
deferrals list) are operator-feedback-gated.

## Phase 95 — Reflection Cadence Learning (Skip-When-Idle) (Phase 71's reflection-cadence deferral, closed)

**Frozen — see [PHASE_95.md](archive/phases/PHASE_95.md).** Closes the
Phase 71 deferral that's been carried 24 phases:
*"reflection cadence learning."* The reflection cron has
been firing unconditionally on its `cron` schedule since
Phase 71, paying LLM cost for Phase 87 phrasing + Phase 91
judgment + Phase 92 supersession passes even on idle days
where there's nothing actionable to find. Phase 95 closes
the deferral with the simplest leverage shape: **skip-when-
idle**. The scheduler reads audit-chain growth since the
last *fired* cycle for that schedule; if growth is below
the operator-configured threshold AND `skip_when_idle =
true`, the cycle is skipped entirely (no LLM calls; just a
log line + a counter bump). The operator's cron remains
the **upper bound** on firing rate — cadence learning is
monotonic-slower-only, never faster.

- **Audit-growth signal (Q1a):** the scheduler reads
  `audit_log.len()` delta since the last fired cycle for
  the same schedule. Simple, deterministic, no LLM cost,
  no new substrate. The first cycle after a daemon boot
  fires unconditionally (no prior baseline to compare).
- **Skip-when-idle binary (Q2a):** either fire or skip;
  monotonic-slower-only. The boundary uses `>=` so
  `min_audit_entries_to_fire = 1` means "any new entry
  fires," not "any entry beyond the first."
- **Per-schedule knob (Q3a):** two new optional fields on
  the existing `[[reflection_schedule]]` block.
  `skip_when_idle: bool` (default `false`) +
  `min_audit_entries_to_fire: u32` (default `1`). Wire-
  compat via the established `Option<T>` +
  `#[serde(default)]` pattern. Different schedules may
  carry different idleness tolerances — a daily housekeeping
  schedule wants a high threshold; an hourly responsive
  schedule wants a low one.
- **Observable (Q4a):** daemon log on skip
  (`aivyx-pa reflection: schedule "X" — skipped (audit-growth
  K below threshold M)`) + `aivyx-pa learning` surface
  block (`Reflection cadence (Phase 95):` with per-
  schedule `K fired, S skipped` counts). Real-time + aggregate.

Streak all three correct: DESIGN.md → **42**, PRODUCT.md →
**35**, `aivyx-core/src/lib.rs` → **43** (new project
record, beats Phase 94's 42) — helper + state + integration
all in `aivyx-channel/src/reflection_scheduler.rs`; config
knobs in `aivyx-config`; surface in
`bin/aivyx_modules/learning.rs`; IPC field added with full
wire-compat. Test count delta `+12` workspace (`+3` config
knobs covering defaults / explicit values / staged
posture / zero-threshold rejection, `+7` helper covering
boundary + defended-zero + stat default + serde wire-
compat, `+1` multi-cycle integration over the helpers, `+1`
surface rendering with empty / all-zero / mixed cases) —
**two over** the predicted `+6-10` band, accounted for by
the helper + stat earning more individual unit tests than
the calibration law anticipated for "new pure module with
multiple boundary cases." Zero clippy warnings (one
`large_enum_variant` allow added to `QueryResponsePayload`
since the addition tipped a long-running additive-fields
variant past clippy's threshold). Zero new workspace deps.

Likely follow-ups (backoff-multiplier mode; adaptive
interval; time-of-day pattern learning; persisted cadence
stat across daemon restarts; LLM-based signal-density
classifier; per-pass skip granularity — see PHASE_95.md
deferrals list) are operator-feedback-gated.

## Phase 96 — ANN Index for Semantic Memory Search (Phase 75's ANN-index deferral, closed)

**Frozen — see [PHASE_96.md](archive/phases/PHASE_96.md).** Closes the
Phase 75 deferral that's been carried 20 phases: the
approximate-nearest-neighbor index for semantic memory
search. The brute-force `rank_by_cosine` over the full
`vector_index: Vec<(String, u64, Vec<f32>)>` has been
the only path since Phase 75; it scales linearly with
memory size. Phase 96 adds opt-in **IVF-style
clustering** that scales `O(N) → O(√N)` at query time.

- **Hand-rolled IVF (Q1a):** vectors partition into
  `K ≈ √N` clusters at build time via deterministic
  spaced-sampling seeds + one-pass nearest-centroid
  assignment. ~150 lines in
  `aivyx-memory::ann_index`. Preserves the project's
  zero-new-deps streak.
- **In-memory + rebuild-on-demand (Q2a):** the
  `AnnIndex` lives in a `tokio::sync::Mutex<Option<...>>`
  alongside the flat `vector_index`; rebuilt on the first
  ANN query after boot AND when the
  `writes_since_ann_build: AtomicU32` counter crosses
  the operator-configured threshold. No new schema, no
  serialization, no incremental-update complexity.
- **ANN narrows → brute-force re-ranks (Q3a):** the ANN
  returns a candidate pool of `4 * limit` candidates
  searched from `default_top_clusters(K) = max(2, K/4)`
  clusters; the existing brute-force ordering rule then
  re-ranks within and returns the top-K. The exact-
  cosine guarantee on the final ordering is preserved
  within the candidate set.
- **Two consumer-side knobs (Q4a):**
  `[embedding].ann_index: bool` (default `false`) +
  `ann_rebuild_threshold: u32` (default `100`).
  Validation: `ann_rebuild_threshold >= 1` when armed
  (zero would force a rebuild every recall). With the
  knob off, recall is byte-identical to pre-Phase-96.

Streak all three correct: DESIGN.md → **43**, PRODUCT.md
→ **36**, `aivyx-core/src/lib.rs` → **44** (new project
record, beats Phase 95's 43) — the ANN data structure +
build + query + RedbMemory integration all in
`aivyx-memory`; the dispatch in
`aivyx-channel::memory_recall`; the config knobs in
`aivyx-config`. No `aivyx-core` touch; no new
`AuditTag`; no new `KeyDomain`. Test count delta `+22`
workspace (`+4` config knobs, `+15` pure ANN module
with comprehensive boundary coverage, `+3` RedbMemory
integration — small-N brute-equivalence, large-N
top-1 recall, stale-counter increment/reset) —
**squarely inside the predicted +15-25 band**. Zero
clippy warnings. Zero new workspace deps.

Likely follow-ups (HNSW-quality recall for very large
stores; iterative k-means refinement; persisted index
across daemon restarts; incremental updates; topic-aware
centroid seeding; ANN for the `aivyx-pa memory search`
operator path — see PHASE_96.md deferrals list) are
operator-feedback-gated.

## Phase 97 — Token-Budget Context Sizing (Phase 76's + Phase 86's longest-running content deferral, closed)

**Frozen — see [PHASE_97.md](archive/phases/PHASE_97.md).** Closes the
twice-deferred token-budget item carried 21 phases
(Phase 76) and 11 phases (Phase 86). Auto-recall,
adaptive Persona selection, and the conversational
window have all capped injection by **entry count** —
a proxy for token cost, not the cost itself. A single
4 KB memory body silently displaced multiple shorter
ones from the same `rag_top_k` budget; a grown Persona
facet ate turn after turn of input. Phase 97 adds an
opt-in token budget that caps both recall + Persona
injection paths AFTER their existing rank-and-filter
steps: the lowest-ranked items drop until the running
estimate fits.

- **Hand-rolled `chars/4` estimator (Q1a):** preserves
  the project's zero-new-deps streak; ~20 lines of code
  in `aivyx-channel::token_budget`. Accuracy ~±20% for
  English — adequate for budget enforcement at any
  realistic operator threshold. Unicode `chars()`-
  counted, not bytes.
- **Combined recall + Persona scope (Q2a):** one knob
  caps both. The existing `rag_top_k` and Persona
  K-facet caps become soft hints; the token budget is
  the hard cap. One coherent operator-facing number.
- **Drop-lowest-ranked eviction (Q3a):** the first item
  whose addition would exceed the budget AND every item
  after it are dropped. No mid-item truncation. The
  caller pre-ranked, so the dropped tail is by
  construction the lowest-priority subset.
- **Single opt-in knob (Q4a):** new
  `[embedding].recall_token_budget: u32` (default `0`
  = disabled). Matches the established Phase 87 / 88 /
  91 / 92 / 93 / 95 / 96 actuator opt-in pattern. With
  the knob at `0`, the recall + Persona paths are
  byte-identical to pre-Phase-97.

The protected Persona core (behavioral constraints +
identity scalars) is ALWAYS present regardless of
budget — the budget only trims soft-facet selection.
The recall breadcrumb + Phase 78 learning surface +
Phase 84 cluster stat + Phase 77 recall_log all see
the post-budget set so observers match what's actually
injected. Edge case: if every recall hit falls out of
the budget, auto-recall returns no block (planner falls
back to the base prompt).

Streak all three correct: DESIGN.md → **44**, PRODUCT.md
→ **37**, `aivyx-core/src/lib.rs` → **45** (new project
record, beats Phase 96's 44) — `token_budget` module +
recall integration + Persona integration all in
`aivyx-channel`; config knob in `aivyx-config`. Test
count delta `+21` workspace (`+3` config knob, `+13`
pure module with comprehensive coverage of both
`estimate_tokens` and `apply_token_budget` boundary
cases, `+3` recall integration including the "every hit
fell out → None" path, `+2` Persona integration
including the "protected core always present" pin) —
**over** the predicted `+10-15` band, accounted for by
the pure helper module earning ~12 individual tests
instead of the calibration law's expected 5-7. Zero
clippy warnings (one trivial `into_iter` cleanup). Zero
new workspace deps.

Likely follow-ups (exact-tokenizer integration; per-
category budgets; auto-derive from model context window;
conversational-window budget; mid-item truncation
strategy; surface line for dropped-by-budget count —
see PHASE_97.md deferrals list) are operator-feedback-
gated.

## Phase 98 — Hybrid Keyword+Semantic Recall Fusion (Phase 75's hybrid-fusion deferral, closed)

**Frozen — see [PHASE_98.md](archive/phases/PHASE_98.md).** Closes
Phase 75's 23-phase-old hybrid-fusion deferral. The
recall pipeline has ranked by cosine similarity over
embeddings since Phase 75 — strong on semantic
relationships but weak on rare-term recall (acronyms,
proper nouns, code identifiers, project codenames). The
keyword search tool (Phase 74) handles those exact-
match cases but operates as a separate manual path.

Phase 98 fuses the two paths via **Reciprocal Rank
Fusion (RRF)**. With `[embedding].recall_hybrid = true`,
auto-recall runs both the semantic ranker AND the
existing `Memory::search` substring search at recall
time; the two rankings fuse via RRF before feeding the
downstream pipeline (cluster expansion, token budget,
etc.).

- **Reciprocal Rank Fusion (Q1a):** industry-standard
  fusion. `score = Σ 1 / (k + rank + 1)` with `k = 60`
  (Cormack et al.'s standard value). Rank-based —
  cosine scores and substring hit counts don't need
  normalization because RRF doesn't read scores. ~30
  lines of pure code in
  `aivyx-channel::recall_fusion`.
- **Reuse Phase 74's substring search (Q2a):**
  `Memory::search` is what the operator-facing search
  already uses. No new tokenization; no corpus stats;
  no BM25-style state to maintain. RRF is rank-based,
  so the simple substring path gives RRF the ranks it
  needs.
- **Same query text both sides (Q3a):** Phase 86's
  conversational window (if engaged) or the bare
  current message — whatever the semantic side
  embeds. Single source of truth; consistent ranking
  targets.
- **Single opt-in knob (Q4a):**
  `[embedding].recall_hybrid: bool` (default `false`).
  Matches the established actuator opt-in pattern.

Trade-off documented: the `rag_min_similarity` floor is
**skipped on the hybrid path** because RRF scores
aren't on the cosine scale. The `rag_top_k` cap still
limits the fused output; items only one ranker surfaces
get small RRF scores that get pushed out by stronger
items. A separate `rag_hybrid_min_rrf` knob is a
documented deferral.

Streak all three correct: DESIGN.md → **45**, PRODUCT.md
→ **38**, `aivyx-core/src/lib.rs` → **46** (new project
record, beats Phase 97's 45) — `recall_fusion` module +
recall integration all in `aivyx-channel`; config knob
in `aivyx-config`. Test count delta `+16` workspace
(`+3` config knob, `+10` pure RRF helper with the full
boundary set including the core "item in both rankings
outranks single-ranker top hits" claim, `+3` recall
integration including the rare-term case the deferral
named) — slightly over the predicted `+10-15` band,
accounted for by the RRF module's comprehensive
boundary coverage. Zero clippy warnings (two trivial
cleanups). Zero new workspace deps.

Likely follow-ups (rag_hybrid_min_rrf floor knob; BM25-
style keyword scoring; tokenization-aware substring
matching; operator-tunable recall_hybrid_k; surface
line for fused stats; shared embedding cache between
rankers — see PHASE_98.md deferrals list) are
operator-feedback-gated.

## Phase 99 — Local Testing Setup (operator-requested)

**Frozen — see [PHASE_99.md](archive/phases/PHASE_99.md).** The first
operator-feedback infrastructure phase. Ninety-nine phases
of substrate shipped with the test pyramid resting entirely
on `cargo test`; what the project never had was a one-command
way to build the `aivyx-pa` binary and drive the real agent
stack against a real LLM backend locally. Phase 99 built
that loop: a `scripts/dev-run.sh` launcher (interactive
session against a fully local Ollama backend, all state under
a gitignored `.dev-run/`) and a `scripts/dev-verify.sh`
scripted verification pass (audit chain, daemon lifecycle,
memory/fs tool probes — the tool paths an interactive chat
test cannot cover). Shell tooling only — no crate touched,
no workspace test added (`cargo test` stays at 1746); all
three contract streaks held byte-identical: DESIGN.md → 46,
PRODUCT.md → 39, `aivyx-core/src/lib.rs` → 47 (new project
record, beats Phase 98's 46). Opened under an explicit
**local-build posture**: builds stay local while repo
infrastructure (CI, remote runners, publication) is still
being decided — that work belongs to the Distribution
milestone, not here. Phase 99 is the prerequisite for the
Channel Activation Milestone, which consumes this harness
for operator verification across all channels.

## Chapter B — Tooling (Phases 100+)

Phase 99 established the local operator loop; the
`dev-verify.sh` pass it shipped immediately surfaced the
agent's **tool surface** as the next area worth focused
work — both the breadth of what the agent can do and the
reliability with which it does it. Chapter B is the tooling
arc. It expands the first-party tool surface, then hardens
the substrate around it: how reliably models invoke tools,
how the operator observes tool usage, and how third parties
author new tools. Each item lands as its own focused phase
in the small-scope, Q-block-signed-off rhythm Chapter A
established. The chapter opens with the most concrete gap —
capability scopes declared in `aivyx-capability` that have
no tool behind them.

**Expected phases:**

- **Phase 100 — Tool-Surface Gap Closure.** Shipped — see
  below and [PHASE_100.md](archive/phases/PHASE_100.md).
- **Phase 101 — Tool-Call Input Validation & Repair.**
  Shipped — see below and [PHASE_101.md](archive/phases/PHASE_101.md). The
  reliability item: the planner validates a known tool's
  call input against the tool's schema before dispatch and
  loops the model to repair a malformed call.
- **Phase 102 — Tool Observability (`aivyx-pa tools`).**
  Shipped — see below and [PHASE_102.md](archive/phases/PHASE_102.md). A
  read-only subcommand that lists every registered tool
  and annotates each with audit-derived call/outcome
  stats, with a `--window` filter.
- **Phase 103 — External Tool Ergonomics (`aivyx-pa tool init`).**
  Shipped — see below and [PHASE_103.md](archive/phases/PHASE_103.md). The
  closing Chapter B item: an `aivyx-pa tool init <path>`
  subcommand that scaffolds a runnable Rust tool-process
  starter (Cargo.toml, src/main.rs handshake + invocation
  loop, README, conformance test, `[[tool_process]]`
  snippet) so a third-party tool author edits one function
  body rather than copying `examples/python-tool/` and
  porting it to Rust by hand.

**Chapter B is complete.** All four expected phases shipped
(100, 101, 102, 103); subsequent tool-layer work is
operator-feedback-gated, in the project's established
post-Chapter posture. The next chapter — **Chapter C —
Operator Onboarding** — opens at Phase 104.

## Chapter C — Operator Onboarding (Phases 104+)

Chapter B closed the tool layer in good shape; the next axis
of work is the *fresh operator's* experience. From the
moment they decide to try Aivyx PA to the moment their first
turn returns a useful answer, every step is operator-
visible and every paper-cut compounds. Chapter C is the
arc that closes those paper-cuts: init wizard polish,
provider-default refresh, default-model recommendations,
docs landing, and — when the held public-hosting decision
resolves — the Phase 61 `v0.1.0` publication that turns
"build from source" into "install the binary." Each item
lands as its own focused phase in the small-scope,
Q-block-signed-off rhythm Chapters A and B established.
The chapter opens with the first thing every new operator
touches: the `aivyx-pa init` wizard itself.

**Expected phases (subject to revision at each exit):**

- **Phase 104 — `aivyx-pa init` Polish.** Shipped — see
  below and [PHASE_104.md](archive/phases/PHASE_104.md). Refreshed
  stale provider defaults, named a concrete starter
  model on the empty-Ollama path, and added a verify-
  before-write step against `GET /v1/models` so a broken
  config never lands on disk.
- **Phase 105+ candidates** at chapter open were docs
  landing rewrite, the held Phase 61 `v0.1.0` publication
  once the hosting decision lands, and operator-feedback
  follow-ons from Phase 104. The post-Phase-104 review
  surfaced a separate axis — a comparison pass against
  the Hermes Agent (Nous Research) named five concrete
  out-of-the-box surface gaps (channel breadth, tool
  breadth, MCP server breadth, skills auto-creation,
  trajectory logging) that warranted their own arc.
  Those five land as **Chapter D — Substrate Breadth**
  below. Chapter C's onboarding remit (docs landing,
  `v0.1.0` publication) stays paused, not retired —
  whichever thread surfaces first opens the next
  Chapter C phase.

**Chapter C status — open with one phase shipped.**
Phase 104 closed the wizard-side first-touch paper-cuts;
the docs-landing and `v0.1.0`-publication threads named
at chapter open are paused. **Chapter D — Substrate
Breadth** takes the immediate post-Phase-104 sequence.

## Chapter D — Substrate Breadth (Phases 105–110+)

Phase 104 closed with a comparison pass between Aivyx PA and
the **Hermes Agent** (Nous Research, MIT, Python — the
closest public reference for a personal-AI-agent
substrate). The comparison found the Aivyx PA substrate sound
but its *out-of-the-box surface* narrower than the
reference along five axes: a smaller channel-adapter set
(Local / Telegram / Web UI vs. Hermes's six), a smaller
first-party tool count (ten vs. forty-plus), a single
bundled MCP server (the Phase 46 `web-search` vs. an
opt-in matrix), no agent-drafted skill substrate, and no
trajectory-export path for the audit chain even though
every event is already structured for it. Chapter D is the
arc that closes those gaps. The five items land
easy-wins-first so the substrate-design items (Amendment
A12 for the tool count, reflection extension for skills)
arrive after the lighter items build momentum and real
operator pressure tightens the exact scope.

**Expected phases (subject to revision at each exit):**

- **Phase 105 — Trajectory Logging (`aivyx-pa audit export`).**
  Shipped — see below and [PHASE_105.md](archive/phases/PHASE_105.md).
  Lowest-risk Chapter D item. The HMAC audit chain already
  carried the structured per-turn / per-tool-call rows a
  trajectory exporter needs; Phase 105 wired
  `PersistentAuditLog::entries_range` into a JSONL emitter
  with `--from <seq>` and `--limit <N>` filters. All three
  byte-identity streaks held → DESIGN.md 52, PRODUCT.md 5,
  `aivyx-core/src/lib.rs` 5.

- **Phase 106 — MCP Server Breadth (Curated Recipes).**
  Shipped — see below and [PHASE_106.md](archive/phases/PHASE_106.md). Q1a
  at sign-off chose **recipes-only** scope: zero new
  bundled-server code paths. Shipped `docs/MCP_RECIPES.md`
  cataloguing 12 well-supported MCP servers (filesystem,
  github, gitlab, sqlite, postgres, time, fetch,
  brave-search, slack, memory, puppeteer, everything) with
  paste-able `[[mcp_server]]` + inline `[mcp_server.sandbox]`
  blocks per Q3a + env vars + capability-scope notes. Plus a
  new `aivyx-pa mcp recipes [<name>]` CLI surface per Q2a. All
  three byte-identity streaks held → DESIGN.md 53,
  PRODUCT.md 6, `aivyx-core/src/lib.rs` 6. New bundled-
  server code stays a deferral pending operator pressure.

- **Phase 107 — Discord Channel Adapter (`aivyx-discord`).**
  Shipped — see below and [PHASE_107.md](archive/phases/PHASE_107.md). Full
  parity with `aivyx-telegram` at the in-process layer.
  `aivyx-pa --channel discord` runs an end-to-end Discord bot
  against twilight-rs (twilight-gateway + twilight-http +
  twilight-model). Two streak-prediction surprises in the
  operator's favor: `aivyx-core/src/lib.rs` held
  byte-identical (Phase 8 forward-enumerated
  `ChannelPlatform::Discord`); DESIGN.md held (A4 addendum
  in the amendment file per Phase 49 precedent). Test count
  delta `+29` (workspace `1859 → 1888`), below the predicted
  `+50`–`+100` because Discord's push-based Gateway
  simplified the substrate the scripted suite covers. Two
  Phase-107-internal deferrals named honestly: Discord
  daemon-frontend (`FrontendType::Discord` +
  `discord_daemon_frontend.rs` mirroring Phase 19) and
  `/approve` / `/reject` text-command gate-resolve routing
  — both land alongside in a focused follow-on.

- **Phase 108 — Slack Channel Adapter (`aivyx-slack`).**
  Shipped — see below and [PHASE_108.md](archive/phases/PHASE_108.md).
  Fourth in-tree channel adapter, four-data-point
  confirmation for `docs/ADAPTER_PATTERN.md` (was
  confirmed-at-three at Phase 107). slack-morphism 2.22 SDK,
  Socket Mode only, `(team_id, channel_id)` colon-joined
  partition key. **All three byte-identity streak
  predictions held** — `ChannelPlatform::Slack` was
  already in `aivyx-core` since Phase 8 (same Phase 107
  surprise pattern). Zero-new-deps broke as predicted at
  the slack-morphism adoption. Workspace tests `+25` →
  1913 (inside the predicted `+20`–`+30` band). Two
  Phase-108-internal deferrals bundled with the Phase 107
  daemon-frontend follow-on: production
  `SlackMorphismTransport` callback-state-passing wiring
  (slack-morphism's `SlackClientEventsUserState` API
  surface needs proper UserState-backed design), and
  `/approve` / `/reject` gate-resolve routing.

- **Phase 109 — Tool Breadth (Amendment A12).** Shipped —
  see below and [PHASE_109.md](archive/phases/PHASE_109.md). Q1a chose
  **`git.status` + `git.diff`** sharing one new `git.read`
  scope base (qualified by repo path) as the headline two
  tools. Q2a also closed **`net.dns`** as a bonus closure
  of one of Phase 100's eight audit-deferred toolless
  scopes. A12 takes P10 from 10 → 13 substrate tools.
  All three byte-identity streaks reset together — DESIGN.md
  + PRODUCT.md broke at A12 as predicted; **`aivyx-core/src/lib.rs`
  broke too, against the open-doc prediction** (tools
  live in aivyx-core/src/tools/ per the Phase 4 substrate
  convention, not aivyx-channel; honest break per the
  Phase 6 Q5 convention). First triple-reset since Phase
  56's P13/P14 amendment double-break. Zero new workspace
  deps. Workspace tests `+25` → 1938 (inside the predicted
  `+15`–`+25` band at the upper edge).

- **Phase 110 — Skills Auto-Creation (Reflection Staging).**
  Shipped — see below and [PHASE_110.md](archive/phases/PHASE_110.md). The
  last Chapter D item — closes the sixth and final
  Hermes-comparison gap. LearnedSkill as the 11th
  PersonaDeltaCategory variant reusing the entire Phase
  59/60/70 Persona substrate; three new scope bases
  (skills.propose, skills.list, skills.invoke) in
  CEILING_TRUSTED; two substrate tools (skills.list +
  skills.invoke) gated by their own bases; `## Learned
  skills` system-prompt section between Persona and active
  role. **Second consecutive triple-streak-reset** —
  DESIGN.md broke at the new D4 skills section; PRODUCT.md
  broke against the prediction (P8 delivery-status refresh);
  `aivyx-core/src/lib.rs` broke at the new tool re-exports.
  Workspace tests +12 → 1950 (below the predicted +25-+40
  band; substrate-extension work was largely covered by the
  existing profile_prompt / persona test surface). Zero new
  workspace deps. Agent-side auto-proposer heuristic deferred
  to a follow-on phase.

Chapter D ends when each item has shipped or been
explicitly retired. The post-110 ledger picks up Chapter
C's paused threads (docs landing, `v0.1.0` publication)
or opens against operator feedback as it arises. Phase
ordering inside Chapter D is "easy wins first" by design;
inversion at any phase exit costs one ROADMAP commit, not
an amendment.

## Phase 111 — Adapter Production Wiring (operator-requested follow-on)

**Frozen — see [PHASE_111.md](archive/phases/PHASE_111.md).** Standalone
phase past Chapter D's close (no chapter framing, matching
the Phase 99 precedent for operator-requested follow-on
work). Landed the two Phase-107/108-internal carve-outs:
- **Discord daemon-frontend** (Phase 107 Task 5 carve-out)
  shipped mirroring Phase 19's Telegram-over-daemon pattern.
- **Slack Socket Mode live wiring** (Phase 108 Task 3
  carve-out) shipped replacing the stub
  `SlackMorphismTransport` with the real implementation
  via `SlackClientEventsUserState` callback-state-passing.

After Phase 111, all five adapters (Local, Telegram, Web
UI, Discord, Slack) are production-ready end-to-end at the
in-process AND daemon-mode levels. The **Channel
Activation Milestone** is now unblocked and runnable as
the operator-verification pass it was always meant to be —
no code, just real-bot smoke tests across every adapter.

**Exit outcomes:**
- All three streak predictions held — first phase since
  Phase 56/108 with every prediction correct and every
  streak extending (DESIGN.md, PRODUCT.md,
  aivyx-core/src/lib.rs all reach streak=2).
- Q2a shared-substrate question answered affirmatively at
  three data points: extracted `gate_command::parse` into
  `aivyx-channel/src/gate_command.rs`; broader pump-shape
  extraction deferred until a fourth adapter forces it.
- Test count delta `+14` (1950 → 1964), below the `+25`
  to `+40` prediction — honest break per Phase 6 Q5
  attributable to scripted-only test posture (Q3a)
  collapsing per-adapter integration tests into a single
  cross-renderer parity assertion.
- Zero new workspace deps; `http = "1"` direct-promoted in
  `aivyx-slack` for callback signature legibility but it
  was already transitive.

## Chapter E — Self-Improvement Loop Deepening (Phases 114+)

After Chapter D closed the Hermes-comparison gaps and
Phase 111-113 cleared the operator-surface deferral
ledger, Chapter E opens against the project-vision
critical path: deepening the **self-improvement loop**
beyond the skill-specific framework Phase 112 shipped.
Phase 112's auto-proposer drafts new skills from complex
turns; Chapter E generalizes the loop to the full
self-improvement surface — every PersonaDeltaCategory,
tool-selection learning, recall-quality self-tuning,
self-correction on failed turns.

Chapter ordering follows the "easy wins first" discipline
established for Chapter D: the most natural extension of
the freshly-warm Phase 112 substrate ships first. The
operator picks subsequent direction at each phase exit
based on observed value vs the next-axis options.

**Expected phases (subject to revision at each exit):**

- **Phase 114 — Persona Auto-Proposer Generalization.**
  Active — see below and [PHASE_114.md](archive/phases/PHASE_114.md).
  Extends the Phase 112 auto-proposer from `LearnedSkill`
  to every PersonaDeltaCategory variant. Per-category
  TOML config (operator-picked over uniform-single); same
  four-signal heuristic gate; reuses existing scope bases
  (no KNOWN_BASES growth).

Subsequent Chapter E phases will be picked from the
remaining self-improvement axes (tool/skill selection
learning from outcomes; self-correction loop on failed
turns; outcome-driven Profile/Role refinement) based on
operator pressure and observed value from Phase 114.

## Chapter F — External Productivity Integrations (Phases 123+)

After Chapter E's #4 closed by Phase 118 and three named
Local-LLM Rehab phases (120 + 121 + 122) shipped real
substrate, Chapter F opens a fresh thematic axis:
**Aivyx PA as a productivity assistant, not just a chat
surface.** External integrations — Gmail, Calendar, Drive,
GitHub, etc — as first-class operator-facing capabilities.

Per P10 + P11 + P12, every Chapter F integration ships as
a separate third-party tool process via the existing
`[[tool_process]]` substrate (Phase 49). Aivyx PA core
stays at exactly thirteen substrate tools forever; each
Chapter F integration is a separate binary the operator
installs and registers. P10 explicitly names email and
calendar as third-party territory; the architecture is
the contract's intended shape, not a constraint to work
around.

Chapter F is also the first real consumer of the third-
party SDK contract (P11 + P12). The SDK has shipped but
has no non-trivial external consumer yet. Phase 123 is the
first; SDK-contract validation findings from each Chapter F
phase feed back into substrate-improvement candidates for
subsequent phases.

Chapter ordering follows "easy wins first" per Chapter D:
Gmail picked first as the Google OAuth substrate
establisher (Calendar / Drive reuse the same pattern);
subsequent integrations picked by operator pressure at
each phase exit.

**Expected phases (subject to revision at each exit):**

- **Phase 123 — Gmail Integration.** Frozen — see below
  and [PHASE_123.md](archive/phases/PHASE_123.md). Operator-provided
  Google OAuth (Q1a Recommended); full read + draft +
  send tool surface (Q2c non-Recommended; honest scope
  acceptance); per-tool-process token file storage
  (Q3a re-asked under P10 third-party constraint).
  Validated the third-party SDK contract on the first
  real consumer; surfaced one substrate gap (single-tool
  harness assumption); recommended for follow-on lift.

Subsequent Chapter F phases (Calendar, Drive, GitHub,
shared credential vault) picked at each phase exit based
on operator pressure and SDK validation findings.

## Chapter G — Operator-Facing Personal Assistant Capabilities (Phases 125+)

After Chapter F #1 (Phase 123 Gmail) shipped and Phase 124
exited with the local-LLM-rehab axis exhausted, the
operator framed the next direction: tooling has to be
fully functional and complete before Channel Activation
matters. Channels-without-working-tools is wrong-order.

Chapter G fills operator-facing tool surface gaps the
Phase 122/124 transcripts surfaced (gemma4's hallucinated
enumeration partly mapped to existing Aivyx PA tools, partly
to gaps). Different from Chapter F (Gmail / Calendar /
etc are specific external service integrations); Chapter
G is broader operator capability — web search, task
tracking, monitoring, future tools like calendar
reminders, expense tracking, etc.

Per P10 + P11 + P12, every Chapter G integration ships as
a third-party tool process. Aivyx PA core stays at the
thirteen-tools-forever cap. Chapter G reuses Phase 123's
substrate (multi-tool harness + per-tool-process config +
per-tool-process file storage + capability-base extension
pattern) without architectural additions.

**Expected phases (subject to revision at each exit):**

- **Phase 125 — Personal Assistant Tool Bundle.** Frozen
  — see below and [PHASE_125.md](archive/phases/PHASE_125.md). Bundled
  eight tools through one `aivyx-toolkit` binary:
  `web.search` (Brave API), four `task.*` (TODO CRUD),
  three `health.check.*` (scheduled URL monitoring with
  agent-composable alerts). Three-of-three streak hold;
  test count `+102` inside `+80 to +130` prediction;
  Phase 123 SDK-validation finding now twice-duplicated
  across gmail + toolkit harnesses (lift recommended
  alongside the next Chapter F/G integration).

- **Phase 191 — Daemon-side automatic alert dispatch for tool
  processes.** Shipped 2026-09-05 — see
  [PHASE_191.md](archive/phases/PHASE_191.md). By the time this phase
  started, two of Phase 125's original three Chapter G #2 candidates were
  already closed (calendar reminders: Phases 141-142; budget tracking:
  Phases 143-144) and the third had itself been split — Phase 147 shipped
  `health.check.remove` but held automatic alert dispatch back
  deliberately, "intentionally held with Channel Activation" per that
  phase's own retrospective. This phase closes that last remaining half.
  A new call_id-free wire variant lets a tool process's own background
  task (not just a daemon-initiated call) push a notification,
  capability-gated by a new `notify.dispatch` scope. Three separate
  layers of the same "the scope parses but can never actually be
  granted" bug class were found and fixed across this phase's own review
  cycle (`KNOWN_BASES` vs. `CEILING_TRUSTED`; a daemon-side placeholder
  target that no task could ever fill; the backcompat-floor auto-grant
  mechanism only picking up scopes declared by real `Tool` objects) plus
  a real handshake-race concurrency bug — the final whole-branch review's
  fix wave added genuine end-to-end test coverage that the re-review
  confirmed would have caught all of them.

Subsequent Chapter G phases (the original Chapter G #2 candidate list is
now fully closed) picked at each phase exit based on operator pressure
and observed first-real-use signal.

## Chapter N — Release & Distribution Integrity (Phases 192, 193, 197) [COMPLETE]

Opened 2026-09-05 after investigating "how would an end user deploy
Aivyx PA on their own bare metal" surfaced that `v0.9.0` — the version
`README.md`'s own "Status" line named as current — was never actually
published as a real GitHub Release. Root cause, confirmed against real
GitHub/CI state rather than documentation claims: `aivyx-confine`,
`aivyx-checkpoint`, and `aivyx-kvcache` were private `Aivyx-Agent`
repos wired into `aivyx-core` (a dependency of every shipped binary)
as pinned-rev git dependencies — a CI runner's ambient `GITHUB_TOKEN`
can't authenticate against a different, private repo, so every
`v0.9.0` release workflow failed before reaching its real
cross-compile/package/publish work, and the same failure almost
certainly blocked "build from source" for any real outside
contributor too. Full grounding:
`docs/superpowers/specs/2026-09-05-release-distribution-integrity-design.md`.

Goal: an unaffiliated outside end user can get a current, working
`aivyx-pa` binary through every path `docs/INSTALL.md` documents, and the
release pipeline can't silently regress into shipping stale or broken
artifacts again without being caught immediately.

**Expected phases (subject to revision at each exit):**

- **Phase 192 — Unblock the pipeline.** Shipped 2026-09-05 — see
  [PHASE_192.md](archive/phases/PHASE_192.md). Made all three repos
  public (their privacy was never intentional — infrastructure
  utilities adopted from `aivyx-coder`, not business logic or secrets)
  and proved it with a new, reusable `scripts/check-git-deps-public.sh`
  anonymous-clone probe rather than assuming the fix worked. The final
  whole-branch review caught two real issues a task-level review
  couldn't have — a factually wrong date/causality claim the plan's own
  literal text had specified for the new `Cargo.toml` comment, and a
  `set -e` silent-abort bug in the script's zero-git-deps path — both
  fixed and independently re-verified with fresh evidence, not trusted
  from reports.
- **Phase 193 — Cut and verify a real release; prove build-from-source
  works for a real outsider.** Shipped 2026-09-05 — see
  [PHASE_193.md](archive/phases/PHASE_193.md). Reconciled a
  long-diverged worktree branch into `main`, cut `v0.9.4`, and along
  the way found a second real regression independent of the code:
  `aivyx` had silently reverted from public back to private, which
  explained both a credential-less build-from-source failure and every
  release workflow instant-failing — not the GitHub secondary-rate-limit
  throttle this session had assumed. Fixed by flipping visibility back
  public (root cause of the revert itself stays unresolved — no
  audit-log access at this account tier). `v0.9.4` is now `Latest`
  with all real build/desktop/WSL/GHCR artifacts verified, and a
  genuinely credential-less container build-from-source run confirmed
  the documented install sequence works end to end.
- **Phase 197 — Regression guard + doc correction.** Shipped
  2026-09-06 — see [PHASE_197.md](archive/phases/PHASE_197.md). Wired
  `check-git-deps-public.sh` into `quality-gate.yml`; corrected six
  stale pre-launch version references across `README.md` and
  `docs/INSTALL.md` (not just the two originally scoped — a broader
  sweep found three more of the same category, including a literal
  installer command pointing at a `v0.1.0` tag that was never actually
  created, and the final whole-branch review caught a sixth); backfilled
  `CHANGELOG.md` with the one real release (`v0.9.4`) it was missing,
  correctly attributing this phase's own CI-guard work to `[Unreleased]`
  rather than the already-published release. **Chapter N is now
  complete.**

**Resolved 2026-09-06:** audited `aivyx-confine`/`aivyx-checkpoint`/
`aivyx-kvcache`/`aivyx-injection-guard`'s full git history (not just
current tree) for accidentally-committed secrets now that all four are
public — this had never been checked before the visibility flips.
`gitleaks detect` (full-history scan, default ruleset) found zero leaks
across all four repos (10/6/23/1 commits scanned respectively); a
supplementary manual sweep of every filename ever committed, across
full history, for credential-shaped names (`.env`, `.pem`, `id_rsa`,
`id_ed25519`, `credentials`, `.npmrc`, `.netrc`, `*secret*`) found none.
Clean.

## Chapter Picket — Prompt-Injection Tripwire (Phases 194–196) [COMPLETE]

Opened 2026-09-05, while investigating end-user deployment options
surfaced a broader question: given `aivyx-confine`/`aivyx-checkpoint`/
`aivyx-kvcache` were all extracted from `aivyx-coder` and adopted by
`aivyx-pa`, are there other similar frameworks worth considering? A fresh
survey (real code opened in both repos) found one strong candidate:
`aivyx-coder`'s `aivyx-sandbox/src/injection_scan.rs` — a phrase-list
prompt-injection tripwire, real and extensively wired into five call
sites there (editor context, repo map, user/project `AGENTS.md`, generic
tool output). `aivyx-pa` has only a passive defense (Bulwark's
`fence_untrusted_output`, which labels untrusted content for the model
but never actively scans for known injection phrasings or halts
execution) — a real gap given `aivyx-pa`'s autonomy dial runs fully
unattended at `Autonomous`/`Unleashed` tiers. Full design:
`docs/superpowers/specs/2026-09-05-injection-guard-design.md`.

Grounding found `TurnOutcome::Escalated` and the `ApprovalGate`/
`HeadlessRefusal` split (`escalation_parks`) already exist and already
correctly branch on attended-vs-unattended — nothing currently triggers
`Escalated` from an injection match, but zero new plumbing is needed once
something does.

**Expected phases (subject to revision at each exit):**

- **Phase 194 — Create the aivyx-injection-guard repo.** Shipped
  2026-09-05 — see [PHASE_194.md](archive/phases/PHASE_194.md). A verbatim
  extraction (confirmed byte-identical to the real source via direct
  `diff`) into a new, public, standalone, zero-dependency repo —
  `Aivyx-Agent/aivyx-injection-guard` — the same `aivyx-confine`/
  `aivyx-checkpoint`/`aivyx-kvcache` pattern, applied a fourth time. The
  final whole-branch review step was deliberately skipped this phase (and
  documented why, in the phase retrospective): the entire deliverable is
  one commit already fully reviewed at the task level, and there's no
  `aivyx`-repo-local branch to merge since the new repo is standalone.
- **Phase 195 — Migrate aivyx-coder onto the new crate.** Shipped
  2026-09-05 — see [PHASE_195.md](archive/phases/PHASE_195.md).
  `aivyx-sandbox` now depends on `aivyx-injection-guard` the same way it
  already depends on `aivyx-confine`; the local `injection_scan.rs` (260
  lines, 10 tests) is gone, and every other real call site needed zero
  changes (confirmed via `grep`, then independently re-confirmed twice).
  Grounding narrowed the doc-fix scope from what was assumed: `README.md`'s
  own "Known limitations" section, read in full, turned out to already be
  accurate — only `CLAUDE.md`'s condensed "re-enters context untagged"
  summary was stale. The final whole-branch review caught a second real
  gap a per-task review couldn't have: `CLAUDE.md`'s architecture table
  documents the `aivyx-confine`/`aivyx-checkpoint` extractions on their
  crate rows but said nothing about this phase's new dependency — fixed
  directly rather than carried as debt.
- **Phase 196 — Adopt into aivyx.** Shipped 2026-09-05 — see
  [PHASE_196.md](archive/phases/PHASE_196.md). The task-level review
  passed clean, but the final whole-branch review found a real,
  HIGH-severity bug in the *plan's own design* (not an implementer
  deviation): the scan ran after tool execution and rewrote a mutating,
  already-executed tool's real `Completed` outcome into
  `RequiresEscalation` — the plan had only checked `fs.read`/`web.fetch`
  for `output_is_untrusted()`, missing that every tool-process/MCP tool
  (including irreversible ones like `kitchen.order.send`) declares it
  too, which made the audit chain misrepresent completed actions as
  pending and risked double-execution on approval-resume. Redesigned
  (operator's explicit choice over a narrower patch) so `run_tool_call`
  always returns the real outcome, with a side-channel signal the turn
  loop checks *after* recording it, breaking for the next step instead
  of misrepresenting the one that ran — zero changes needed to the
  existing `RequiresEscalation` handling. The re-review mutation-tested
  the fix (reintroduced the exact bug to confirm the new test actually
  catches it) and found a second finding was only partially resolved —
  the injection excerpt still reached two unfenced, model-readable sinks
  (`mission.status`'s gate output, the opt-in skill auto-proposer's LLM
  judge prompt) — fixed by dropping the excerpt from the escalation
  reason entirely. Three real, independently-verified bugs found and
  fixed in one review cycle, every one confirmed via direct code tracing
  or mutation testing.

- **Phase 198 (Finding 3 follow-up: close the productivity-integration
  coverage gap) shipped 2026-09-06** — see
  [PHASE_198.md](archive/phases/PHASE_198.md). Grounding found the
  original Finding 3 framing (a false-positive/calibration risk on
  Gmail/Calendar/Slack/MCP content) was wrong about coverage: `grep`
  across `aivyx-gmail`/`aivyx-calendar`/`aivyx-drive`/`aivyx-contacts`/
  `aivyx-notion`/`aivyx-obsidian`/`aivyx-n8n`/`aivyx-toolkit` found
  zero `output_is_untrusted()` overrides anywhere in those eight
  crates — this content was neither fenced by Bulwark nor scanned by
  Picket at all, a real open injection surface, not a tuning problem
  on already-scanned content. Flagged 28 tools (plus a ninth-crate gap
  of the same class, `aivyx-toolkit`'s `WebSearch`) via one mechanical
  trait-method addition each, riding the existing centralized call
  site with zero turn-loop changes. The final whole-branch review
  caught 5 override comments that described content their tools don't
  actually return (verified against each tool's real `execute()`
  body) — the `true` flag was correct in every case, only the
  audit-trail comment text was wrong; fixed in one commit. The
  config-knob half of the original Finding 3 framing remains a
  separate, still-open follow-up.

**Known follow-up, not yet scheduled to a phase:** the phrase list itself
is ported verbatim, not expanded — real-usage-driven additions to
`INJECTION_MARKERS` are a candidate for whenever either consumer's actual
operation surfaces a real miss.

- **Phase 199 (Finding 3 follow-up: the config-knob half) shipped
  2026-09-06** — see [PHASE_199.md](archive/phases/PHASE_199.md). Added
  `[agent] injection_scan_enabled` (global on/off) and `[agent]
  injection_scan_exempt` (per-tool-name exemption list) — the user's
  own choice of the richer shape over a plain boolean, since the
  original finding said "disable *or tune*." Threaded through
  `aivyx-config` → `aivyx-core`'s `ConcreteAgent` →
  `aivyx-cli`'s daemon-startup wiring, gating only the active scan;
  Bulwark's fencing stays unconditional, confirmed by the final review
  tracing it to its one real call site. The same review found the
  design spec's own premise ("only two `ConcreteAgent::new` sites exist
  in this binary") was wrong — team missions build agents through a
  separate path (`aivyx-team`'s `SpecialistFactory::build`) this phase
  never touched, so the two knobs don't yet reach team missions
  (fail-safe: they stay scan-always-on there). Further grounding found
  the real fix is a genuine multi-crate propagation (`SpecialistFactory`
  is constructed from four separate files), not a quick patch — logged
  as its own follow-up below rather than rushed into this phase.

- **Phase 200 (Finding 3 follow-up: closing the team-mission gap) shipped
  2026-09-06** — see [PHASE_200.md](archive/phases/PHASE_200.md). Fresh
  re-grounding corrected Phase 199's own retrospective estimate: only 3
  of the "four separate files" were real production code (the other 3
  were test-only fixtures) — the real gap was exactly 3 previously-
  uncovered construction paths. Extended `TurnSafety`
  (`crates/aivyx-core/src/agent.rs` — the codebase's own existing choke
  point for exactly this class of bug) to carry
  `injection_scan_enabled`/`injection_scan_exempt`, and threaded it
  through all 6 real `TurnSafety::interactive`/`autonomous` call sites in
  the system (converting Phase 199's own 2 already-covered sites to the
  same mechanism too, per the user's explicit choice, for full
  consistency). The final whole-branch review (Opus) found a real,
  Critical regression the branch itself introduced: `TurnSafety`'s
  derived `Default` had `injection_scan_enabled: false`, and this
  branch's own change to `apply()` (writing that field unconditionally)
  turned it into a silent kill-switch on 4 live production paths
  (standalone Telegram x2/Discord/Slack channel sessions) that construct
  agents via `TurnSafety::default()` directly — correctly out of this
  phase's propagation scope, but newly exposed to `apply()`'s changed
  semantics. The design spec's own dismissal of this exact risk ("no
  such production call exists today") was factually wrong — a spec-level
  miss, not an implementer error — fixed with a hand-written `Default`
  impl preserving scan-on, independently verified against all 4 call
  sites directly. Chapter Picket's Finding 3 is now fully closed.

- **Phase 201 (channel-session config threading) shipped 2026-09-06** —
  see [PHASE_201.md](archive/phases/PHASE_201.md). Grounding found the
  gap was narrower than it sounded: only the 3 channels' in-process
  fallback (used on `--no-daemon` or when no daemon socket is reachable)
  ever called `TurnSafety::default()` — the daemon-mode dispatch path
  never constructs an agent client-side at all. Added the same 4 fields
  to `TelegramSessionConfig`/`DiscordSessionConfig`/`SlackSessionConfig`
  (mirroring `tool_allowlist`/`memory_topic_prefix`) and converted all 4
  real `TurnSafety::default()` call sites to `TurnSafety::interactive(...)`.
  A real plan defect (not an implementer error) was caught mid-execution:
  the new tests' assertion expected an exact-match reply text, but every
  channel unconditionally renders a tool-call progress line regardless of
  scanning — fixed by asserting on the absence of the escalation footer
  instead, independently mutation-tested by 3 of 4 task reviews. The
  final whole-branch review (Opus) re-verified every grounding claim from
  scratch and found one more instance of Phase 200's own regression
  class, one level up: `TurnSafety::default()`'s hand-written `Default`
  impl (Phase 200's fix) had a doc comment naming the exact callers this
  branch just moved off of it, with zero test enforcing the invariant —
  a future "helpful" `#[derive(Default)]` could have silently re-opened
  the identical regression, undetectably this time. Fixed with a reworded
  comment plus a dedicated test pinning
  `TurnSafety::default().injection_scan_enabled == true`. Chapter
  Picket's team-mission/channel-session propagation arc (Phases 199-201)
  is now fully closed — every real agent-construction site in the system
  receives the operator's full `[agent]` config through one mechanism.

## Phase 202 — First-Launch Store Safety [COMPLETE]

Opened 2026-09-06 from a direct audit of what happens when an End User
launches Aivyx PA for the first time — traced against real code, not
inferred from docs. Shipped the same day — see
[PHASE_202.md](archive/phases/PHASE_202.md). Found and closed a real bug
chain: running bare `aivyx-pa` before `aivyx-pa init` on a fresh machine
silently created a permanent, unconfirmed encrypted store (real mkdirs,
an unconfirmed interactive passphrase prompt, a real `RedbStorage::open`)
before config validation ever ran, and a later, real `aivyx-pa init` run
could then collide with that orphaned store under a different
passphrase — an opaque, undiagnosable decrypt failure. Three
mechanisms: an early-validate gate in `run()` that only activates when
no store exists yet (preserving the store-can-supply-a-secret fallback
for every returning install, byte-for-byte, independently verified by
building and running the real binary for both paths), a store-collision
guard in `aivyx-pa init` mirroring its existing `aivyx-pa.toml`-overwrite
guard, and confirm-reentry on the first-time interactive passphrase
prompt scoped to new-store creation only. The final whole-branch review
found 3 Important issues by going further than reading the diff — it
built and ran the real binary itself: a zeroize-hygiene regression in
the new confirm-reentry function (contradicting the module's own
documented invariant), a real pre-existing `$XDG_DATA_HOME` divergence
between `aivyx-pa init`'s own path defaults and `aivyx-config`'s loader
that this phase's own design spec incorrectly claimed didn't exist
(letting the new store-collision guard silently miss an orphaned store
on some machines), and two stale `#[allow(dead_code)]` attributes from
an intermediate branch state. All three fixed and independently
re-verified.

## Phase 203 — Closing Phase 202's Three Logged Follow-Ups [COMPLETE]

Opened and shipped 2026-09-06 — see [PHASE_203.md](archive/phases/PHASE_203.md).
Closes all 3 follow-ups Phase 202's own final review logged: the real
gap (`verify-only`/`audit export`/`cost` could still create a store on
an unconfigured machine, since those 3 modes never require an API key
so `config.validate()` could never catch it) plus 2 cosmetic issues
(doubled `aivyx-pa:`-prefixed output, an untestable passphrase-retry
message). Task 1's own review caught a real bug its implementer's
self-report missed entirely — a doc comment left genuinely duplicated
by an imperfect find/replace, found only by reading the real file
directly. The final whole-branch review found something more serious:
Task 1's own fix had reintroduced the exact doubled-prefix symptom it
existed to remove, and traced that the diagnostic-mode hint pointed
operators at `aivyx-pa init` — which never creates a store — walking
through the real dead-end loop that would produce. Both fixed and
independently re-verified against the real, built binary (not just the
diff), confirming a single prefix, a correct remedy, and zero files
created on the failure path.

## Phase 204 — Expanding INJECTION_MARKERS [COMPLETE]

Opened and shipped 2026-09-07 — see [PHASE_204.md](archive/phases/PHASE_204.md).
Chapter Picket's `INJECTION_MARKERS` phrase list (in the separate
`aivyx-injection-guard` repo, pinned into `aivyx` by git `rev`) grew from
9 entries to 22, grouped into 4 categories, proactively rather than in
response to any observed real-usage miss. The final whole-branch review
found the content itself needed a second pass before merge: 2 of the 14
proposed markers (`"developer mode"`, an ordinary technical noun phrase
in Chrome/Android/iOS/Windows settings UI, and `"you are no longer"`,
which collides with routine email-unsubscribe footers) were meaningfully
more false-positive-prone than the rest — and a match here isn't cosmetic,
it hard-stops the agent's turn (`LoopOutcome::Escalated`). Both were
dropped/narrowed before merge. The same review caught a doc comment on
`aivyx-core`'s `check_for_injection` that had gone stale on two counts —
claiming the list was still "ported verbatim from aivyx-coder" (no longer
true post-expansion) and "no config knob to disable the tripwire" (false
even before this phase; `injection_scan_enabled`/`injection_scan_exempt`
already existed) — and a process gap in this phase's own execution (the
implementation plan was written but never committed). All fixed and
independently re-verified directly against the real diffs and a fresh
`cargo test`/`clippy` run in both repos, not from subagent reports.
`aivyx-coder`'s own separate pin was deliberately left untouched (a
different, unrelated active project) and now diverges from `aivyx`'s.

## Phase 205 — Reconciling FRONTEND.md's Studio Screen Inventory [COMPLETE]

Opened and shipped 2026-09-07 — see [PHASE_205.md](archive/phases/PHASE_205.md).
Came out of a scoping attempt for Chapter I's "polish" placeholder, which
was paused (needs live-backend infra — a real LLM + Playwright — not
currently running in this environment). Picked up instead: `docs/
FRONTEND.md`'s §3 screen-inventory table, flagged stale in 3 straight
retrospectives (188/189/190) and never fixed. Direct grounding against
the real `View` enum and `Sidebar` component found the staleness was
larger than any retrospective specified — 11 of 23 real screens were
missing entirely, and the existing Voice row's "In progress" state was
itself stale (it shipped). Rewrote the table with a new `Group` column
mirroring the real `Sidebar`'s own grouping, so a future addition that
isn't mirrored in the doc reads as obviously incomplete rather than
silently absent. Every cell traces to an existing source (a `View` enum
doc comment or existing roadmap prose) — nothing invented, independently
re-verified against the real code twice.

## Phase 206 — Configurable KV-Cache Store Path Sharing (Aivyx PA ↔ Aivyx Coder) [COMPLETE]

Opened and shipped 2026-09-07 — see [PHASE_206.md](archive/phases/PHASE_206.md).
Followed a direct-code interoperability audit (aivyx-pa and aivyx-coder can
already interact today via a real, working MCP bridge — aivyx-coder
exposes `code`/`code_reply` MCP tools that aivyx-pa's Nonagon team bridges
in as an out-of-process specialist) and a refinements audit that found
`aivyx-kvcache`'s cross-process store — built specifically so the two
apps could share prefill work — could never actually be shared, since
both hardcoded their own app-name-scoped store path with no override.
Added one to each side, plus a security fix each (aivyx-coder's own
deny_paths protection for the directory was blind to any override;
aivyx-pa had *no* protection for its own kvcache directory at all). The
final whole-branch review (Opus, spanning both repos) found the largest
finding set of any phase this run — 1 Critical + 6 Important + 3 Minor,
led by a doc that told operators to write a TOML key that didn't match
the real loader (would have silently half-configured the pairing) and a
core claim that was itself wrong (the two apps' cache keys can never
actually match, so sharing the directory doesn't share cache hits — the
real reason is a shared llama-server has exactly one `--slot-save-path`,
so both sides must agree on it for correct size accounting). All fixed
and independently re-verified. A real infra quirk surfaced mid-run —
switching the session's own active worktree while a background subagent
was still running caused a commit to land on the wrong (harmless)
branch, caught only by checking `git worktree list` directly rather than
trusting the subagent's own reported hash.

## Phase 207 — `aivyx-pa` client integration for `aivyx-broker` (multi-process GPU-slot coordination) [COMPLETE]

Opened and shipped 2026-09-07 — see [PHASE_207.md](archive/phases/PHASE_207.md).
The larger of two opportunities surfaced by a strategic question about
building a custom LLM inference engine (the smaller, an embedded
`mistral.rs` port, shipped separately in `aivyx-coder`). Confirmed via
direct-code grounding — including verifying llama.cpp's own
`server-context.cpp` — that two independent processes sharing one
`llama-server` really do race for the same slot with zero coordination
today, but the server itself already defers same-slot collisions safely
rather than corrupting state, narrowing the problem from data corruption
to silent head-of-line blocking and cache-locality thrash. Built as a
new standalone repo, `aivyx-broker` (a loopback-only daemon owning
cache-locality-aware slot admission and the full `aivyx-kvcache`
restore/warm/save lifecycle), with this phase wiring `aivyx-pa` up as a
client via a new `ProviderKind::Broker`. The review found a real gap the
plan hadn't anticipated: team-mission specialists shared the
broker-pointed backend but weren't wired for it, so a hint-less
specialist request could silently clear a hinted request's cache-locality
bookkeeping — fixed by mirroring the existing `kv_cache_handles` plumbing
chain through all five files it actually spans. Also found and fixed: a
third, independent `--provider` CLI parse site that rejected the new
provider despite the config file and env var already accepting it, and a
startup banner that printed the wrong base_url field for broker mode.
`aivyx-broker` itself (and the equivalent `aivyx-coder` client
integration) are logged in their own repos, not here — see
`aivyx-ecosystem/ROADMAP.md` for the cross-repo account.

## Phase 208 — `aivyx-pa` client integration for `aivyx-yubi` (hardware-backed federation identity) [COMPLETE]

Opened and shipped 2026-09-08 — see [PHASE_208.md](archive/phases/PHASE_208.md).
A second security-focused idea from the operator, scoped independently
from Phase 207's GPU-slot work: could a YubiKey harden `aivyx-federation`'s
Ed25519 identity keypair, moving the private key into hardware entirely
(never on disk, even encrypted) with every signature requiring a physical
touch? Grounding ruled out the more obvious-looking PIV route (the
`yubikey` crate is unmaintained, lacks Ed25519) in favor of the OpenPGP
applet (actively maintained, Ed25519 since 2019). Built as a new
standalone repo, `aivyx-yubi` — given a real GitHub remote mid-effort once
it became clear `aivyx-federation` genuinely needs it as a Cargo
dependency (unlike `aivyx-broker`, reached only over HTTP) — with this
phase wiring `aivyx-federation`'s `Identity` (its first production
consumer ever, confirmed via direct grounding that no crate depended on
it before this phase) and a new `aivyx-pa federation yubikey-init` CLI
subcommand. Two structurally real bugs were caught in review, both fixed
and independently re-verified against real vendored source: an incomplete
first attempt at feature-gating the hardware dependency (Cargo resolves
`path` dependencies at manifest-load time regardless of feature state,
breaking CI and every other contributor's build until the crate got a
real `git` remote), and a genuine PC/SC deadlock in the CLI's own
provisioning flow (holding one exclusive card transaction open while
attempting to open a second on the same reader would hang the command
forever on real hardware, after the card had already been irreversibly
re-keyed). `aivyx-yubi` itself is logged in its own repo, not here — see
`aivyx-ecosystem/ROADMAP.md` for the cross-repo account.

## Chapter H — Productize: From Mature Substrate to Launchable Product (Phases 180–184) [COMPLETE]

After the Phase 172–179 correction-learning + autonomous-loop
arc, the Phase 179 backend review
([BACKEND_REVIEW_2026-06-06.md](archive/BACKEND_REVIEW_2026-06-06.md))
took stock against the founding goal — a *Local-First,
Security-Focused, fully customizable autonomous Personal
Assistant* an End User shapes by Personality + Role at first
launch. The finding: the substrate **substantially realizes four
of the five pillars** (Local-First, Security, Autonomous,
general-purpose), with the self-learning correction loop as the
genuine edge over Openclaw / Hermes. The remaining gaps are **not
architectural** — they are the "productize it" layer between a
mature substrate and a product an end user can launch and trust.
Chapter H closes the review's Tier-1 and Tier-2 gaps in
value / dependency order. Per P10–P12, any new capability still
ships as a tool process; the thirteen-tool substrate core stays
frozen.

**Shipped — all five gaps closed (see the phase log below + each
`PHASE_N.md`):**

- **Phase 180 — Security-by-default sandbox preset.** Bundled
  bubblewrap / firejail preset, auto-detected, applied to new
  launches via the wizard; existing configs unchanged.
- **Phase 181 — Guided first-launch identity builder.** The End
  User shapes the full six-field Profile (Personality + Role),
  LLM-drafted from a relationship conversation with a fully
  offline manual fallback + a "meet your assistant" preview.
- **Phase 182 — Guided in-agent credential onboarding.**
  `aivyx-pa connect <service>` — Google Cloud guidance, config
  write, shell-out to the tested `auth init`, `[[tool_process]]`
  auto-wire, startup surfacing.
- **Phase 183 — Reminders (everyday-PA breadth #1).** Daemon-
  native `remind.*` over the notify dispatcher. (The breadth
  sub-sequence can continue — weather / contacts / news — picked
  at each exit by operator pressure.)
- **Phase 184 — Conversational skill-teaching.** `skills.teach /
  update / forget` — the End User teaches a skill in chat,
  confirm-then-save into the Persona chain.

**Outcome:** the founding "**fully customizable**" promise is
realized end-to-end — Profile, Persona, Roles, and skills are all
user-shaped — with the 13-tool substrate cap untouched and an
unbroken DESIGN / PRODUCT / `lib.rs` streak across the chapter.

**Bookends (not Chapter H phases) — still open:**

- **Distribution — cut `v0.1.0`.** A Tier-1 gap, but gated on an
  external public-hosting decision rather than a code phase (the
  deferred Phase 61 Task 7). Cut when hosting is settled; the
  release substrate is already wired.
- **Channel Activation Milestone.** Remains the closing milestone
  (its own section above) — cross-channel session continuity so
  the agent feels like *one* assistant across Local / voice /
  Discord / Slack / Telegram. Scheduled after the phase sequence
  settles.

**What's next.** With Chapter H complete, the chosen next axis is
**interface & reach** — see Chapter I below. (Other open
directions remain on the shelf: the everyday-PA breadth
continuation, the dep-requiring substrate hardenings, cutting
`v0.1.0`, and the Channel Activation Milestone.)

## Chapter J — Nonagon: Multi-Agent Teams (Phases J.1–J.7) ✅ COMPLETE

The free-core **multi-agent** capability: a *lead* agent convenes up
to **9 attenuated specialists**, decomposes a mission into a DAG,
delegates, verifies, and synthesizes — all inside the **one daemon**
on the **one HMAC audit chain**, preserving the single-agent ethos
(one lead; ephemeral, subordinate, least-privileged specialists). The
key de-risk: it's a `SpecialistPool` + delegation **tools** on the
existing turn loop, not a loop rewrite — and the safety invariant
(**NT-02:** specialist `⊆` lead) rests on the `CapabilitySet`/`Scope`
attenuation the core already ships. The **engine is free**; **verticals
customise the team** (kitchen ships a Back-of-House Nonagon). Full
design contract + phase plan in [`NONAGON.md`](NONAGON.md).
Ported/re-grounded from the pre-rebuild archive's `aivyx-team` crate.
Plugs into [`VERTICAL_PACKS.md`](VERTICAL_PACKS.md).

**Shipped (J.1–J.7):** the `aivyx-team` crate — `TeamConfig`/roster,
`attenuate_for_member` (NT-02), `SpecialistFactory`/`SpecialistPool`,
the `MessageBus`, the `MissionPlan` **DAG** (cycle detection + ready-set)
+ the concurrent `TeamRuntime`, the `decompose_task`/`synthesize_results`/
`verify_output`/`delegate_task`/`query_agent`/`send_message`/`read_message`
tools, and `TeamAssembly` (the wiring entrypoint). The CLI: **`aivyx-pa team
run "<mission>" [--config <pack.toml>]`** (in-process, on the HMAC chain,
specialists holding their real attenuated tools) and **`aivyx-pa team roster
[--config <pack.toml>]`**. First vertical: the **`aivyx-kitchen`** pack
(Aria's Back-of-House Nonagon + the overnight-close mission, shipped as a
`TeamConfig` + a TOML asset). TUI: the live **Missions** panel
(`View::Missions`, master/detail). Two new scope bases (`team.delegate`,
`team.message`); the kitchen pack adds none (reuses the pre-registered
`kitchen.*`). **Deferred follow-ons** (out of chapter scope): a vertical's
*domain* tools (the kitchen toolkit crate's KitchenDB RPCs) joining
`base_tools`; **daemon-side** team execution + the live Missions IPC feed
(today `team run` is a one-shot CLI); optional autonomous-loop integration
of team missions.

## Chapter I — Interface & Reach (Phases 185+)

How the End User **connects to, interacts with, launches, and
runs** their Aivyx PA Agent. The load-bearing framing (DESIGN /
PRODUCT P5): there is exactly **one daemon** — it holds the
agent, state, capabilities, and audit — and every interface is a
**frontend client** that connects over the local Unix-socket IPC
and detaches (the daemon survives any frontend exiting). So this
chapter is not "build a UI" — it is **richer frontends + the
launch/run lifecycle around the daemon.**

Today's surface: a plain line-based REPL; a localhost-only Web UI
SPA (Chat / Audit / Verify / Memory / Mission / Persona tabs, but
feature-stale — no loop / reminders / skills / connect / identity,
and no auth); the Telegram / Discord / Slack / voice channels
(the only remote reach); and auto-spawn launch with manual
systemd/launchd wiring.

**Operator-chosen direction:**

- **Local-first preserved.** No networked remote access — the
  threat model (Unix socket `0600` + OS-user isolation) is
  untouched; reaching the agent from another device stays the
  job of the existing messaging channels.
- **Headline frontend: a rich terminal TUI** (replacing the
  REPL).
- **Productize launch/run:** `aivyx-pa service install` (always-on
  daemon) + a first-run launch flow.

> **A deliberate streak break.** A real TUI needs a terminal-UI
> library — **`ratatui` + `crossterm`** (operator-confirmed). This
> consciously **ends the zero-new-workspace-dependency streak**
> held across the entire 172–184 run. The judgment: a load-bearing,
> widely-audited, pure-Rust dependency for a feature that genuinely
> requires it is the right trade — and it is **quarantined to the
> TUI frontend** (a dedicated crate), so the substrate crates
> (`aivyx-core`, `aivyx-capability`, …) stay dependency-clean.

**Expected phases (subject to revision at each exit):**

- **Phase 185 — Terminal TUI foundation.** Replace the REPL with
  a `ratatui`/`crossterm` frontend over the daemon IPC: streaming
  chat pane + scrollback, an input line, a status bar
  (role / daemon / in-flight), clean keybindings. The agent's
  data already flows over IPC — this is the render + interaction
  layer. The new deps live in a dedicated frontend crate.
- **Phase 186 — TUI state panels.** Shipped 2026-09-03 — see
  [PHASE_186.md](archive/phases/PHASE_186.md). Didn't fold into
  185: that phase shipped Missions/Audit/Tools as their own tabs,
  narrowing 186 to a real Dashboard (loop status + reminders +
  mission/audit summaries) plus scroll support the design hadn't
  anticipated. Reminders gained their first frontend IPC surface
  anywhere in the product (`GetReminders`) — previously agent-tool-
  only since Phase 183.
- **Phase 187 — Service install + first-run launch.** Shipped
  2026-09-03 — see [PHASE_187.md](archive/phases/PHASE_187.md). The
  unit-generation half was already shipped (Chapter Anchor,
  `v0.7.6`, 2026-06-28) before this roadmap entry even existed —
  real remaining scope was wiring an interactive offer into the
  `init` wizard's ending, which now calls that existing machinery
  directly instead of only printing the command.
- **Phase 188 — Studio Loop + Reminders screens.** Shipped
  2026-09-04 — see [PHASE_188.md](archive/phases/PHASE_188.md). Of
  the 5 named features, 3 (skills, connect, identity) were already
  shipped before this phase started; only loop and reminders were
  genuinely missing, and both reused queries already built for other
  frontends (the CLI, the TUI Dashboard) — zero new backend work.
  "polish" and "mobile-responsive" stayed unscoped, real candidates
  for a future phase.
- **Phase 189 — Studio mobile-responsive verification pass.** Shipped
  2026-09-04 — see [PHASE_189.md](archive/phases/PHASE_189.md). The
  first phase this run with nothing pre-named to ground against —
  Chapter I's own "Expected phases" list ended at 188. Grounding found
  Phase 188's "mobile-responsive" framing was itself stale: a complete
  responsive shell (drawer sidebar, hamburger, 3 breakpoints) had
  already shipped 2026-06-23, months earlier — the real gap was that
  it had never actually been watched render, only confirmed present in
  the built bundle. A live-backend test environment (real LLM via the
  GPU rig, real Chrome via Playwright) produced 92 real screenshots
  across 23 screens × 4 widths, found and fixed 3 genuine CSS bugs, and
  — per this session's now-standard pattern — the final whole-branch
  review still caught something every other pass missed: the capture
  tool's screenshots were silently viewport-clipped despite
  `fullPage: true`, fixed by a real mechanism change plus a full
  re-audit (zero new bugs found). "polish" and `.btn-xs`/`.icon-btn`
  tap-target sizing remain open, logged as follow-ups.
- **Phase 190 — Tap-target sizing fix.** Shipped 2026-09-04 — see
  [PHASE_190.md](archive/phases/PHASE_190.md). Picked up the one
  concrete gap Phase 189 measured but left unfixed: `.icon-btn`
  (34×34px) bumped to 40×40 globally (4 sites, low risk); `.btn-xs`
  (~22.5px) bumped to 40px minimum height only at ≤860px via a new
  media query, leaving its 48 desktop usage sites untouched. A real
  build-tooling bug (a stale `dx bundle` cache silently reusing old
  output on a same-session rebuild) was found, fixed, and documented
  so it doesn't cost a future phase the same debugging cycle. The
  final review hit an Opus session usage limit mid-run — handled the
  same way as every prior Opus unavailability this run: asked before
  falling back to Sonnet, which still found one real (Minor) gap.
  "polish" remains the only item from Chapter I's original roadmap
  wording still unscoped.

Subsequent phases (and whether 185/186 merge) are chosen at each
exit; ordering revised as each exit teaches us something.

**Known follow-ups (carried from the Phase 185 real-session
verify):**

- **Daemon banner bleed on auto-spawn — FIXED.** The auto-spawned
  daemon inherited the frontend's controlling TTY, so its startup
  banner (`aivyx-pa config sources…`, `… listening on …`, the webhook
  line) printed onto the launching terminal — in the TUI it bled
  under the alternate screen. `spawn_daemon_and_wait` now redirects
  the daemon's stdout/stderr to a sibling `daemon.log` (next to the
  socket + pid). Verified: screen-banner count 0, `daemon.log`
  banner count 3. Direct `aivyx-pa daemon run` is unaffected.
- **`RecoveryNotice` broke the first connect after an unclean
  daemon shutdown — FIXED.** The daemon sends a `RecoveryNotice`
  envelope (take-once) between `DaemonReady` and `SessionStarted` to
  the first frontend that connects after it restarts with stale
  `daemon.state` (the previous instance was killed / crashed rather
  than `daemon stop`-ed). `DaemonSession::connect`'s
  `SessionStarted`-wait loop did **not** handle it — it hit the
  catch-all and failed with *"expected SessionStarted, got
  RecoveryNotice"*, affecting **every** frontend (REPL, TUI,
  channels share `connect`) in exactly the recovery scenario where
  reconnect most needs to be smooth. `connect` now drains + skips
  the notice (surfacing it to stderr only when sessions/turns were
  actually lost) and keeps waiting for `SessionStarted`. Verified
  end-to-end: reconnect after a `kill -9` of the daemon (stale
  `daemon.state` present) now succeeds + runs a real turn; +1
  handshake integration test. Supersedes the earlier vague *"Broken
  pipe"* note (same connect-time dance; this was the root cause).

## Phase 184 — Conversational Skill-Teaching (Chapter H #5)

**Frozen — see [PHASE_184.md](archive/phases/PHASE_184.md).** The End User can
*teach* the agent a skill in conversation (`skills.teach` /
`skills.update` / `skills.forget`): the agent drafts it, shows it
back, and on explicit confirmation saves it as a callable skill
on the HMAC persona chain. Made safe-by-contract with a required
`confirmed: true`, Trusted-tier-only (`skills.write`), full audit
+ revert. Completes the fully-customizable promise; 13-tool cap
untouched.

## Phase 183 — Reminders (Everyday-PA Breadth #1, Chapter H #4)

**Frozen — see [PHASE_183.md](archive/phases/PHASE_183.md).** One-shot reminders
— *"remind me at 6pm"* → a notification arrives. Daemon-native
(`KeyDomain::Reminders` + a `ReminderStore` + `remind.*`
channel-tier tools + a re-arming driver that pushes due reminders
through the notify dispatcher), because push-at-a-time is the
daemon's job, not a pull-only tool process.

## Phase 182 — Guided In-Agent Credential Onboarding (Chapter H #3)

**Frozen — see [PHASE_182.md](archive/phases/PHASE_182.md).** `aivyx-pa connect
<service>` replaces the undocumented connect-a-tool sequence: it
guides the Google Cloud app setup, writes `config.toml`, shells
out to the tested per-service `auth init`, confirms, and offers
to wire `[[tool_process]]`. Surfaces "run `aivyx-pa connect`" for an
unauthenticated tool at startup.

## Phase 181 — Guided First-Launch Identity Builder (Chapter H #2)

**Frozen — see [PHASE_181.md](archive/phases/PHASE_181.md).** First launch
becomes shaping a relationship: the End User describes what they
want, the LLM drafts all six P13 Profile fields from a
relationship conversation (with a fully-offline manual fallback),
they review/edit each, and meet their assistant in a warm preview
before it's written. Local-first; the Persona/Soul is left to
grow.

## Phase 180 — Security-by-Default: Bundled Sandbox Preset (Chapter H #1)

**Frozen — see [PHASE_180.md](archive/phases/PHASE_180.md).** Tool processes are
OS-isolated by default for new launches: a bundled
bubblewrap / firejail preset (auto-detected,
filesystem-isolated with only the per-tool token dir writable,
network on), applied via the wizard writing `[sandbox]
default_backend = "auto"`; the in-code default stays `none` so
existing configs are byte-identical.

## Phase 179 — Tool Surfacing in OutcomeSummary

**Frozen — see [PHASE_179.md](archive/phases/PHASE_179.md).** The reflection
family's `OutcomeSummary` now carries each turn's **tools** (the
restart-safe `ToolCall` scope base — no `tool_id` map), rendered
into the reflection prompt and, opt-in via
`[correction_signal].attribute_tools`, attributed in the
correction signal — an outcome-driven `detect_tool_corrections`
that unblocks the no-recall turns the recall-driven detector
misses. Streaks held; `+8` tests (below the predicted `+14..+24`
— the band lesson refined: price by dense-test *components*, not
a substrate-vs-loop label).

## Phase 178 — LLM-Judged Correction Classification

**Frozen — see [PHASE_178.md](archive/phases/PHASE_178.md).** Closed the Phase
172 debt: the correction signal is now LLM-judged (genuine
`rework` vs `praise` vs `unrelated`), folding only real reworks.
A truncated, encrypted `query_text` on the recall log gives the
judge the follow-up message with perfect correlation and
graceful degradation. `[correction_judgment]` opt-in. `+21`
tests, in band.

## Phase 177 — Loop Loose-Ends Bundle

**Frozen — see [PHASE_177.md](archive/phases/PHASE_177.md).** Operator polish on
the autonomous loop: live `tokens used` in `aivyx-pa loop status`,
`aivyx-pa loop skip <id>`, and progress-note de-duplication.

## Phase 176 — Loop Token-Budget Cap

**Frozen — see [PHASE_176.md](archive/phases/PHASE_176.md).** A third loop cap —
`max_run_tokens` — alongside the iteration and wall-clock caps,
summing turn spend over the run window with a `StopBudget`
termination reason.

## Phase 175 — Loop Progress Log

**Frozen — see [PHASE_175.md](archive/phases/PHASE_175.md).** Cross-iteration
learning: a `loop.note` tool writes to a reserved progress topic
that is auto-injected into each iteration's prompt, so a long
backlog gets easier as it goes.

## Phase 174 — Loop Hardening (Gate Verification)

**Frozen — see [PHASE_174.md](archive/phases/PHASE_174.md).** Driver-side
verification that the loop's build/test gates actually ran and
passed before a backlog story is marked complete.

## Phase 173 — Autonomous Loop Foundation (the Aivyx PA Ralph Loop)

**Frozen — see [PHASE_173.md](archive/phases/PHASE_173.md).** Aivyx PA's native
answer to the "Ralph" technique: a fully autonomous,
self-re-arming agent loop over an HMAC-chained append-only
backlog substrate, a re-arming driver (sibling of the reflection
scheduler), a canonical loop system prompt, iteration +
wall-clock caps, and a pure `decide()` termination function.

## Phase 172 — Correction-Signal Learning Loop

**Frozen — see [PHASE_172.md](archive/phases/PHASE_172.md).** The agent now
notices when the operator *corrects* it: a structural
correction detector (`completed`-then-rapid-followup), a durable
decayed correction ledger, and an opt-in
`[correction_consolidation]` pass that turns repeated corrections
on a topic into operator-gated Persona proposals.

## Phase 171 — Loose Ends Bundle (GIF/WebP Clipboard + calendarList Cache TTL Knob)

**Frozen — see [PHASE_171.md](archive/phases/PHASE_171.md).** Capstone phase
before extended review. Two small substrate
cleanups: Phase 170's just-shipped clipboard
signature carry-over (GIF + WebP byte signature
recognition added to infer_clipboard_media_type)
and Phase 158's long-stale calendarList cache TTL
knob (CalendarClient builder + env var override
for the 5-min hardcoded default). Zero new
workspace deps.

## Phase 170 — Voice Polish Bundle (Mid-Recording /image + Clipboard Source + Abort UX Knob)

**Frozen — see [PHASE_170.md](archive/phases/PHASE_170.md).** Three deferred
voice carry-overs in one bundle. Mid-recording
`/image <path>` now queues the image and
continues recording instead of stopping. New
`/image clipboard` reads from the platform
clipboard tool (wl-paste / xclip / pbpaste).
Phase 146's mid-synthesis abort gains an
operator-tunable `abort_requires_double_enter`
knob (default false preserves single-Enter
behavior). Round-number Phase 170 milestone
spent on long-deferred voice polish. Zero new
workspace deps.

## Phase 169 — Phase 166 + 167 Retry/Filter Refinements (Actor Filter + 503/429 Retry + Jitter)

**Frozen — see [PHASE_169.md](archive/phases/PHASE_169.md).** Three small
substrate follow-ons: Phase 167's lone open
carry-over (actor_email post-fetch filter on
drive.recent_activity), Phase 166's 503/429
retry classification (extending send_with_retry
beyond the err-only path), and Phase 166's
backoff jitter (using stdlib SystemTime nanos
as PRNG source — no new dep). Closes one Phase
167 carry-over and two Phase 166 carry-overs.
Zero new workspace deps.

## Phase 168 — Phase 161 + 165 Stragglers (Read-Stalled-Bytes Timeout + Catalog-Aware PDF Page Count)

**Frozen — see [PHASE_168.md](archive/phases/PHASE_168.md).** Two unrelated
honest-debts from two different parent phases.
Read-stalled-bytes timeout defeats slow-trickle
URL servers that fall under the per-request
timeout (Phase 161 carry-over). Catalog-aware
PDF page count augments Phase 165's per-page
byte scan with the catalog's declared /Count
field so compressed PDFs surface their page
count via metadata rather than zero. Smaller
bundle than typical (2-piece) and genuinely
harder than typical — both pieces involve
substrate-level depth. Zero new workspace deps.

## Phase 167 — drive.recent_activity Follow-Ons (Action Filter + Consolidation Knob + Parent Folder)

**Frozen — see [PHASE_167.md](archive/phases/PHASE_167.md).** Phase 159 close-
out, deferred 8 phases (the longest gap in the
project's close-out ledger; user declined this
surface six times before picking it). Three input
additions on the Activity API tool: action_type_
filter for pre-fetch DSL filtering by action
category; consolidation strategy knob (legacy /
none); parent_folder_id mapping to the Activity
API's ancestorName field for subtree scoping.
Honest scope risk: actor filter deferred to
Phase 168+ (the Activity API DSL has no native
actor predicate; would need post-fetch shaping).
Zero new workspace deps.

## Phase 166 — Small Carry-Overs Bundle (PDF Page Cap Knob + URL Retry + Drive Walk Floor)

**Frozen — see [PHASE_166.md](archive/phases/PHASE_166.md).** Three small
substrate carry-overs from three different
phases: Phase 165's hardcoded
`ANTHROPIC_PDF_PAGE_CAP` becomes a config knob
with env-var fallback (`AnthropicConfig::
pdf_page_cap`); Phase 161's URL fetch gains
retry-on-transient-timeout via
`VoiceImageConfig::url_retry_count` +
`url_retry_backoff_ms` with exponential
backoff; Phase 160's drive walk gains a
`walk_min_concurrent` floor companion mirroring
Phase 158's calendar pattern. Touches three
different crates. Zero new workspace deps.

## Phase 165 — Multimodal Stragglers Bundle (Office Formats + Per-URL Headers + PDF Page Cap)

**Frozen — see [PHASE_165.md](archive/phases/PHASE_165.md).** Three multimodal
honest-debts in one phase. Five additional Office
formats (DOC/RTF/ODT/pptx/xlsx) extend the Phase
164 DOCX inference. Per-URL header presets via
new `[voice.image.url_header_presets]` TOML block
+ `--headers <preset>` flag on `/image`
(Phase 162 carry-over). Best-effort PDF page-
count cap on Anthropic via byte-scan; honest
about false negatives on compressed PDFs.
Symmetric to Phase 157/158/160/161 close-out
cadence. Zero new workspace deps.

## Phase 164 — Phase 163 Cleanups (Anthropic Model Guard + DOCX Inference)

**Frozen — see [PHASE_164.md](archive/phases/PHASE_164.md).** Phase 163 just-
shipped follow-ons (two-debt bundle; smaller than
the typical three). Anthropic provider gains a
pre-flight model-version guard so document blocks
on pre-Claude-3.5 models surface a clear client-
side error instead of a 400 from the API. DOCX
extension + MIME inference lands and routes via
the Phase 163 `Document` variant; provider
rejection on Anthropic surfaces same-posture as
SVG/TIFF. Phase 163's named "remove dead
Message::text_with_image" sub-task dropped
honestly — Telegram + channel still call it.
Zero new workspace deps.

## Phase 163 — `ContentPart::Document` for PDF Routing

**Frozen — see [PHASE_163.md](archive/phases/PHASE_163.md).** First DESIGN.md
amendment in 54 phases (A13 under
`docs/amendments/`). Phase 162's just-shipped
honest-debt: PDFs flow through
`ContentPart::Image` and Anthropic returns 400.
Phase 163 adds a `Document` variant across the
content-block stack (aivyx-core, aivyx-llm, four
provider modules, aivyx-voice routing) so PDFs
route as document blocks at the Anthropic API
and skip-and-warn on providers that don't
support them. Multi-crate touch; DESIGN.md and
`aivyx-core/src/lib.rs` streaks both reset.
Zero new workspace deps.

## Phase 162 — Phase 156 + 161 Stragglers (PDF/SVG/TIFF + Authenticated URL Fetch)

**Frozen — see [PHASE_162.md](archive/phases/PHASE_162.md).** Phase 156's last
two carry-overs. PDF/SVG/TIFF media type inference
extends `infer_image_media_type` and
`media_type_from_content_type` (the downstream
`ContentPart::Image::media_type` is already an
opaque String — no core change). Authenticated URL
fetch adds `[voice.image.url_headers]` TOML map
threaded into HEAD + GET via reqwest headers.
Smaller surface than the typical 3-debt bundle but
same close-out shape; takes Phase 156's debt ledger
to zero. Zero new workspace deps.

## Phase 161 — Multimodal Carry-Overs Bundle (Image Size Cap + URL Fetch Timeout + HEAD Pre-Fetch)

**Frozen — see [PHASE_161.md](archive/phases/PHASE_161.md).** Phase 156 close-out
deferred 4 phases. Three multimodal honest-debts
in one phase: operator-tunable image size cap
(replaces the hardcoded 10MB with a
`[voice.image] size_cap_mb` knob), URL fetch
timeout (`url_timeout_secs`, default 30s,
replaces the bare `reqwest::get(url)`), and HEAD
pre-fetch with Content-Length size check before
download (`head_precheck`, default true).
Symmetric to Phase 157/158/160 bundle pattern.
Zero new workspace deps.

## Phase 160 — Throttle the Drive Walk: `walk_folder_tree` max_concurrent

**Frozen — see [PHASE_160.md](archive/phases/PHASE_160.md).** Phase 157 close-out
deferred 2 phases. Single honest-debt: the parallel
level-BFS walk fires unthrottled concurrent
`/files?q=...` queries. Adds a Semaphore-based
`max_concurrent` clamp on `walk_folder_tree` plus an
operator-facing `walk_max_concurrent` input (cap 32)
on `drive.recent_files` and `drive.recent_changes`.
Symmetric to Phase 155 throttling the calendar fan-
out after Phase 151 parallelized it. Zero new
workspace deps.

## Phase 159 — Drive Activity API: `drive.recent_activity`

**Frozen — see [PHASE_159.md](archive/phases/PHASE_159.md).** Twice-deferred
Recommended pivot. New `drive.recent_activity` tool
backed by the Drive Activity API (separate googleapis
subdomain, separate OAuth scope). Answers "who did
what to my files" — actor / action / target /
timestamp envelope — where Phase 145's `recent_*`
tools only answer "what files moved." Substrate adds
`drive.activity.readonly` scope to
`DEFAULT_DRIVE_SCOPES`, a `DRIVE_ACTIVITY_API_BASE`
constant, and a `post_json_activity` client helper.
Operators with existing tokens re-run
`aivyx-drive auth init` once. Zero new workspace deps.

## Phase 158 — Calendar Follow-Ons Bundle (Sliding-Window Dedup + calendarList Cache + min_concurrent)

**Frozen — see [PHASE_158.md](archive/phases/PHASE_158.md).** Phase 155 close-out
deferred 3 phases. Three calendar honest-debts in one
phase: sliding-window adjacency merge replaces the
5-min bucket flooring in fuzzy dedup, calendarList
cache lifts the per-call writable_only round-trip to
a 5-min-TTL session cache, `min_concurrent` input
knob pairs with Phase 155's `max_concurrent`.
Symmetric to Phase 157 closing Phase 153's three
honest-debts. Zero new workspace deps.

## Phase 157 — Drive Recursive Walk Close-Out Bundle

**Frozen — see [PHASE_157.md](archive/phases/PHASE_157.md).** Phase 153 close-out
deferred 4 phases. Three drive honest-debts in one
phase: parallel walk_folder_tree via level-BFS +
join_all, drive_id scope on the recursive walk
(corpora=drive + driveId + includeItemsFromAllDrives +
supportsAllDrives threaded through), operator-tunable
recursive_max_depth (cap 20) + recursive_max_folders
(cap 1000) input knobs. Symmetric to Phase 148/151/
152/153/155/156 close-out cadence. futures-util added
to aivyx-drive as crate dep; zero new workspace deps.

## Phase 156 — Multimodal Close-Out Bundle (Multi-Image + URL Source + Size Cap)

**Frozen — see [PHASE_156.md](archive/phases/PHASE_156.md).** Phase 154 close-out
deferred 2 phases. Three multimodal honest-debts in one
phase: multi-image queue (Vec<(media_type, data)>), URL
source via reqwest (operator types `/image https://...`
and loop fetches), client-side size cap (10MB matching
Drive inline cap). Symmetric to Phase 148/151/152/153/155
close-out cadence. reqwest added to aivyx-voice as crate
dep (already a workspace dep); zero new workspace deps.

## Phase 155 — Calendar: Small Follow-Ons Bundle

**Frozen — see [PHASE_155.md](archive/phases/PHASE_155.md).** Phase 151 follow-on
deferred 4 phases. Three small close-outs in one phase:
fuzzy dedup (normalize summary + 5-min time bucket;
`fuzzy_dedup` knob defaults true), writable_only filter
on calendar.upcoming (internal list_calendars round-trip
+ access_role intersection), max_concurrent knob on
parallel fan-out (tokio::sync::Semaphore throttle for
rate-limited operators). Symmetric to Phase 148/151/152/153
close-out pattern. Zero new workspace deps.

## Phase 154 — Multimodal: Image Attachment via Voice

**Frozen — see [PHASE_154.md](archive/phases/PHASE_154.md).** New feature surface
after 3 debt-close phases (151-153). Wires existing
MessageContent::Mixed shape into the voice channel:
operator types `/image <path>` at the PTT prompt instead
of pressing Enter; image loads + media-type-infers + queues
on the channel; next recording iteration's Message includes
the image. Vision-capable LLM (Qwen-VL via mistral.rs /
Ollama / Anthropic Claude-Vision) describes via TTS.
Zero new workspace deps; substrate already in place.

## Phase 153 — Drive: `recent_*` Close-Out Bundle

**Frozen — see [PHASE_153.md](archive/phases/PHASE_153.md).** Phase 148 close-out
deferred 5 phases. Closes two drive honest-debts in one
phase: drive_id parameter on drive.recent_files +
drive.recent_changes for Team-Drive-scoped recent
queries (corpora=drive + driveId + includeItemsFromAllDrives),
recursive parent_folder_id filter via
walk_folder_tree BFS (max_depth 5, max_folders 100).
Symmetric to Phase 151's calendar bundle + Phase 152's
voice bundle pattern. Zero new workspace deps.

## Phase 152 — Voice Carry-Overs Bundle

**Frozen — see [PHASE_152.md](archive/phases/PHASE_152.md).** Pivot back to voice
after 5 non-voice phases. Closes three voice honest-debts
in one phase: aggressive abort (drop cpal stream for
instant silence instead of rodio clear+sleep_until_end,
Phase 146 #1), partial-text preservation on abort
(surface assembled response_text in abort path, Phase
146 #2), VAD config bounded-range validation (reject
nonsense values at PTT loop entry, Phase 140 carry-over).
Symmetric to Phase 151's calendar debt-cleanup bundle
pattern. Zero new workspace deps.

## Phase 151 — Calendar: Phase 142 Debt Cleanup Bundle

**Frozen — see [PHASE_151.md](archive/phases/PHASE_151.md).** Phase 142 close-out
deferred 9 phases. Closes three calendar honest-debts in
one phase: sequential→parallel fan-out via join_all on
multi-calendar upcoming, cross-calendar event dedup by
(title, start) key, access_role→can_read/can_write
boolean mapping on list_calendars output. Symmetric to
Phase 148's drive cleanup. Zero new workspace deps.

## Phase 150 — Budget: Category Whitelist + Case-Fold + Suggest-Existing

**Frozen — see [PHASE_150.md](archive/phases/PHASE_150.md).** Phase 143
longest-standing honest-debt close-out (carried 6
phases). Free-text categories from Phase 143 meant
"Food" / "food" / "fod" were three different categories;
Phase 150 case-folds new entries to canonical
lowercase + trimmed form, surfaces a `category_suggestion`
("did you mean food") in record/update output when
the input is Levenshtein-distance ≤ 2 from an
existing category, and adds a `budget.categories`
tool returning the unique sorted category list.
Pure substrate; zero new workspace deps.

## Phase 149 — Budget: `budget.trend` (Month-Over-Month Deltas)

**Frozen — see [PHASE_149.md](archive/phases/PHASE_149.md).** Phase 143 #4
honest-debt close-out. Pivot from drive/calendar
after four consecutive debt-closure phases (146-148):
Phase 149 ships genuinely new operator-facing
capability. budget.trend(months_back?, category?)
returns per-month totals + delta vs prior month +
percentage change so the agent can answer "is my food
spending up this quarter" naturally. Pure substrate
aggregation over existing entries. Toolkit harness
13 → 14 tools. Zero new workspace deps.

## Phase 148 — Drive: `list_drives` + `parent_folder_id` Filter

**Frozen — see [PHASE_148.md](archive/phases/PHASE_148.md).** Phase 145 close-out.
Phase 145 shipped drive.recent_files +
drive.recent_changes but called out two specific
honest-debts: no parent_folder_id filter (operators
went back to drive.search for folder-scoped queries)
and no drive.list_drives (shared/Team Drives invisible
to the agent). Phase 148 closes both. Symmetric to
Phase 142's calendar debt cleanup pattern. Drive
surface 9 → 10 tools; zero new workspace deps.

## Phase 147 — Chapter G: `health.check.remove`

**Frozen — see [PHASE_147.md](archive/phases/PHASE_147.md).** Phase 125 final
candidate close-out. Phase 125 framed three Chapter G #2
candidates: calendar reminders (✓ 141-142), budget
tracking (✓ 143-144), and health.check.remove + alert
dispatch (this phase ships .remove; alert dispatch
intentionally held with Channel Activation). Pure
addition: HealthStore::remove_watcher substrate +
health.check.remove tool. Idempotent shape matching
budget.delete and calendar.delete_event. Zero new
workspace deps.

## Phase 146 — Voice Mid-Synthesis Abort UX

**Frozen — see [PHASE_146.md](archive/phases/PHASE_146.md).** Phase 138 close-out
deferred 7 phases. Phase 138 shipped streaming TTS and
called out mid-synthesis abort UX as honest-debt; every
subsequent voice phase carried it without resolution.
Phase 146 closes it: tokio::select! races the streaming
turn future against line_rx.recv() during synthesis; on
Enter, signals the consumer task to call a new
AudioOut::stop_playback() + cancels the agent via the
existing channel cancellation token. Pure substrate
refactor; zero new workspace deps.

## Phase 145 — Drive: `recent_files` + `recent_changes`

**Frozen — see [PHASE_145.md](archive/phases/PHASE_145.md).** Pivot from toolkit
after four consecutive Chapter F/G phases (141-144).
Drive's 7-tool surface has been quiet since Phase 129;
Phase 145 adds the LLM-ergonomic shape that mirrors
Phase 141's calendar.upcoming pattern. Two tools for
two cognitive shapes: drive.recent_files (owned files,
last N days, "what I worked on") and
drive.recent_changes (any accessible files, last N
hours, "what changed in my Drive"). Both reuse Phase
129's pub(crate) file_summary helper; zero new
workspace deps.

## Phase 144 — Close Phase 143 Debt: budget.update + budget.delete + secure_io extract

**Frozen — see [PHASE_144.md](archive/phases/PHASE_144.md).** Phase 143 close-out.
Phase 143 shipped budget.record + budget.summary but
without edit/delete tools and with save_to_disk +
create_dir_all_secure + write_secure duplicated across
task_store and budget_store. Phase 144 closes both
debts in one phase: budget.update + budget.delete reach
CRUD parity with task.* and calendar.*; secure_io
extract folds three shared helpers into one module.
Pure substrate work; zero new workspace deps.

## Phase 143 — Chapter G: Budget Tracking

**Frozen — see [PHASE_143.md](archive/phases/PHASE_143.md).** Pivot from calendar
to another Phase 125 Chapter G #2 candidate. Adds
`budget.record(amount, category, note?)` +
`budget.summary(period?, since?, until?)` to the
aivyx-toolkit harness. Persistence follows the existing
task_store pattern: JSON at
`~/.aivyx-pa/tool-processes/toolkit/budget.json` with
schema_version + 0600 perms + atomic write. Two new
capability bases (budget.read / budget.write) Trusted-
only by default like the rest of the toolkit. Zero new
workspace deps.

## Phase 142 — `calendar.list_calendars` + Multi-Calendar `upcoming`

**Frozen — see [PHASE_142.md](archive/phases/PHASE_142.md).** Phase 141 follow-on.
Phase 141 made calendar.upcoming ergonomic but on `primary`
only. Real operators have 3-5 calendars (work, personal,
shared, project). Phase 142 adds calendar.list_calendars so
the agent can discover them, and extends calendar.upcoming
to accept calendar_ids: [String] with sequential fan-out +
sorted merge. Pure substrate plumbing on top of Phase 128's
existing client + Phase 141's promoted helpers; zero new
workspace deps.

## Phase 141 — Chapter G: `calendar.upcoming` Tool

**Frozen — see [PHASE_141.md](archive/phases/PHASE_141.md).** Pivot from voice
after six consecutive voice phases (135-140). Phase 125's
Chapter G #2 candidate list included calendar reminders;
Phase 141 ships the read-side tool: `calendar.upcoming`
surfaces imminent events with relative-time formatting
("in 5 minutes", "in 2 hours") and an `is_imminent` flag
(within 30 minutes). Builds on Phase 128's existing
OAuth + calendar_client substrate; adds chrono dep to
aivyx-calendar (already in workspace). Proactive
reminder dispatch (agent-initiated, not user-asked) is
a separate Phase 142+ architectural question.

## Phase 140 — Close Phase 139 Debt: TOML Config + Manual-Abort UX

**Frozen — see [PHASE_140.md](archive/phases/PHASE_140.md).** Phase 139 close-out.
Phase 139 shipped energy-threshold VAD with two
documented debts: hardcoded thresholds (no operator
tunability) and lost manual-abort affordance (can't
cancel mid-recording). Phase 140 closes both in one
phase: `[voice.vad]` TOML section with defaults table,
plus a long-lived stdin reader task that races the
silence detector via tokio::select! so Enter
mid-recording aborts cleanly. Zero new workspace deps;
tokio::io::stdin + serde already in workspace.

## Phase 139 — Voice Activity Detection (Energy Threshold)

**Frozen — see [PHASE_139.md](archive/phases/PHASE_139.md).** Phase 138 follow-on.
Phase 138 collapsed voice latency-to-first-audio with
streaming TTS; Phase 139 attacks the next UX irritant
in PTT: operator currently presses Enter twice (start +
stop). Phase 139 replaces the second Enter with
energy-threshold voice activity detection — operator
speaks, stops, mic auto-stops on detected silence.
Pure-substrate RMS-over-window detector + AudioIn
integration + tokio::time::sleep poll loop. Zero new
workspace dependencies; ML VAD (silero) deferred to
Phase 140+ if operators hit noisy environments.

## Phase 138 — Streaming TTS During LLM Generation

**Frozen — see [PHASE_138.md](archive/phases/PHASE_138.md).** Phase 137 follow-on.
Phase 137 gave voice feature parity with Local; Phase 138
attacks the biggest remaining UX weakness — latency-to-
first-audio. Today a long agent reply produces tens of
seconds of silence before any playback. Phase 138
pipelines the LLM stream into the TTS engine on sentence
boundaries: operator hears sentence one while the LLM is
still generating sentence three. Adds a
`drain_complete_sentences` substrate helper, a text-sink
hook on `VoiceChannel`, and a streaming session driver
with a serial mpsc-driven playback consumer task. Zero
new workspace dependencies; tokio mpsc already in use.

## Phase 137 — Voice Agent Feature Parity with Local

**Frozen — see [PHASE_137.md](archive/phases/PHASE_137.md).** Phase 136
follow-on. Phase 135 shipped the voice substrate;
Phase 136 wired the audio I/O loop end-to-end. But
the binary's voice arm still constructs a *minimal*
ConcreteAgent — no role overrides, no recall
context, no memory prune sinks, no prompt refresher.
Phase 137 extracts the agent-stack-construction
logic from `run_session` into a reusable
`build_agent_stack(provider, audit, spec)` helper
so the voice arm gains feature parity with Local.

**Smallest scope possible.** ~67 lines of session.rs
already encapsulate every feature voice is missing;
extracting them into the helper is a pure refactor
behind a Local-channel-tests regression boundary.

**Three-of-three streak HOLDs predicted.**
DESIGN.md → 28, PRODUCT.md → 28,
`aivyx-core/src/lib.rs` → 3.

**Zero new workspace dependencies.**

**Twenty-sixth consecutive deferral of the Channel
Activation Milestone** — intentional hold per
operator framing.

## Phase 136 — Voice Audio I/O Loop: Close-Out

**Frozen — see [PHASE_136.md](archive/phases/PHASE_136.md).** Phase 135
close-out. Phase 135 shipped the voice substrate
(ASR + TTS adapters, channel impl, session seam)
but deferred the cpal + rodio audio I/O loop body
as operator-validation work. Phase 136 fills it in
so voice **actually works end-to-end** the moment
the binary is built with the right features.

**Smallest scope possible.** The substrate seam
(`run_one_voice_turn`) is fully wired and
unit-tested. Phase 136 implements exactly two
pieces: cpal mic capture and rodio speaker
playback, plus the surrounding stdin /
Enter-driven prompt loop.

**Three-of-three streak HOLDs predicted.**
DESIGN.md → 27, PRODUCT.md → 27,
`aivyx-core/src/lib.rs` → 2 (continuing the
post-Phase-135 reset).

**Zero new workspace dependencies** — cpal +
rodio already in `aivyx-voice` from Phase 135.

**Same hardware-validation posture as Phases
134 + 135.** Bridge code compiles cleanly +
unit-tested helpers; real-mic + real-speaker
validation is operator work.

**Twenty-fifth consecutive deferral of the
Channel Activation Milestone** — intentional
hold per operator framing.

## Phase 135 — Voice I/O: Talk to the Agent, Agent Talks Back

**Frozen — see [PHASE_135.md](archive/phases/PHASE_135.md).** First multimodal-
interaction phase. Aivyx PA has shipped text + image input
through cloud LLMs since Phase 45, but operator-facing
input has always been keyboard-only and agent output
text-only. Phase 135 changes that: the operator speaks
to the agent through their microphone; the agent
speaks back through the speakers. End-to-end voice
loop, fully local — Whisper STT + Piper TTS both run
in-process via Rust bindings (`whisper-rs`,
`piper1-rs`), zero outbound network calls during
inference.

**Direct operator request:** "Talk to the agent and
the agent talks back instead of just text on screen."
Voice is the next UX axis Aivyx PA covers.

**Q-block — 3 Recommended + 1 non-Recommended:**
- Q1a — new `aivyx-voice` crate (Chapter-F-style
  optional channel adapter; feature-gated through
  aivyx-channel; zero binary-size impact when
  unused).
- Q2c (non-Recommended) — both `whisper-rs`
  (default) and `whisper-cpp-plus` (alternative)
  ship as STT engine options. Doubles test
  surface; mirrors the Phase 133 multi-provider
  pattern.
- Q3a — Piper via `piper1-rs` for TTS. CPU-friendly,
  real-time on Raspberry Pi class hardware,
  Apache 2.0, 50+ voices across 30+ languages.
- Q4a — push-to-talk MVP. Operator presses Enter
  to start/stop recording. Wake-word activation
  ("Hey Aivyx PA") + always-listening with VAD trim
  are Phase 136+.

**Streak predictions — two HOLDs + one break:**
- DESIGN.md HOLD → 26 (no contract amendment).
- PRODUCT.md HOLD → 26 (voice is natural extension
  of "AI personal assistant").
- **`aivyx-core/src/lib.rs` streak breaks at 8 and
  resets to 1.** The new `ChannelPlatform::Voice`
  variant lives where every existing platform
  variant lives. Honest break over uglier
  workaround.

**Three new workspace dependencies behind opt-in
gates:** `cpal` + `rodio` + `voice_activity_detector`
always-on inside `aivyx-voice`; `whisper-rs`,
`piper1-rs`, `whisper-cpp-plus` per-engine optional.

**Honest scope risks:**
- Real audio I/O testing is operator work; unit
  tests cover conversion logic only.
- Q2c doubles the ASR test surface.
- ONNX runtime build-time prereq (per
  piper1-rs).
- Whisper model size (150MB-1.5GB) and Piper
  voice model size (~20-50MB).
- No streaming TTS during LLM generation
  (Phase 136+).
- No multimodal output yet.

**Twenty-fourth consecutive deferral of the
Channel Activation Milestone** — per operator
framing, intentional hold.

## Phase 134 — Direction B: Embedded Rust-Native Inference (`mistral.rs`)

**Frozen — see [PHASE_134.md](archive/phases/PHASE_134.md).** Largest
architectural phase since Phase 121. Phase 133 set
up the question — three first-class out-of-process
providers; now what? Phase 134 answers: **Aivyx PA can
embed a Rust-native LLM engine and ship as a single
batteries-included binary**, by linking against
`mistralrs` as a Rust dependency.

**Privacy-end-state.** Embedded inference has zero
outbound network calls. Operator loads a local
GGUF file, model runs in-process. Strongest
possible privacy posture short of air-gapping
the machine.

**Q-block — 1 Recommended + 2 non-Recommended:**
- Q1c — opt-in feature flag bundled in a
  `recommended-providers` meta-feature. One-line
  install for new users; lean builds preserved via
  `--no-default-features`.
- Q2c — CPU + Metal + CUDA in one phase. Triples
  the test matrix; each backend gates behind its
  own Cargo feature.
- Q3a — document recommended GGUF models, don't
  bundle. Strongest privacy posture; clearest
  operator consent.

**Three-of-three streak HOLDs predicted.**
DESIGN.md → 25, PRODUCT.md → 25,
`aivyx-core/src/lib.rs` → 8. **One** new workspace
dep (mistralrs, behind opt-in gate). Test count
`+15` to `+35`.

**Honest scope risks at sign-off:**
- Compile time will balloon with `--features
  provider-mistral-rs` (5-10 min initial build).
- Release binary size adds 100-200MB on the CPU
  variant.
- CUDA backend needs the CUDA toolkit; Metal
  needs macOS.
- `mistralrs` is pre-1.0 — pinned to
  `=0.8.*`; upgrade-by-Aivyx-PA-version contract.
- Per-model tool-call format detection may
  behave differently under embedded vs Ollama
  (Phase 135+ empirical validation).

**Twenty-third consecutive deferral of the Channel
Activation Milestone.**

## Phase 133 — Local LLM Provider Alternatives (`llama-server` + Jan)

**Frozen — see [PHASE_133.md](archive/phases/PHASE_133.md).** First multi-provider
phase since Phase 121 (which swapped Ollama's
OpenAI-compat path for a native `/api/chat`
adapter). Phase 133 widens Aivyx PA's local-LLM story
from "Ollama only" to "Ollama as one option among
first-class equals."

**Direction A picked over Direction B at the
direction question.** Two architectural directions
emerged from the local-LLM research that preceded
this phase: (A) incremental multi-provider — keep
the client/server shape and add llama-server + Jan;
(B) embedded Rust-native inference via mistral.rs
or Candle, single-binary install. Direction A
first; Direction B is the leading Phase 134+
candidate.

**Q-block — 3 Recommended:**
- Q1b — llama-server + Jan, not just one. Two
  providers cover two distinct UX axes (power-user
  CLI vs GUI-first end user).
- Q2a — Ollama privacy posture lands as an
  INSTALL.md section (vs empirical tcpdump audit or
  deferred phase).
- Q3b — alternatives documented as equal-status;
  Ollama stays default. Zero behaviour change for
  existing operators.

**Privacy posture motivation:** Ollama's binary
makes outbound calls (update checks, telemetry) the
project doesn't publicly enumerate. The January
2026 SentinelOne/Censys investigation found 175,000
publicly-exposed Ollama hosts across 130 countries
— governance gaps + prompt-injection proxy
potential. Aivyx PA markets itself as privacy-first
local-agent; "Ollama only" is misaligned without
honest caveats.

**Three-of-three streak HOLDs predicted.** DESIGN.md
→ 24, PRODUCT.md → 24, `aivyx-core/src/lib.rs` → 7.
Zero new deps; test count `+10` to `+25` lib tests.

**Honest scope risks:**
- Default base URLs are upstream defaults; custom
  ports need operator override.
- llama-server / Jan don't expose `/api/show`
  family metadata — textual extractor falls through
  to heuristic detection.
- `ollama.list/show/pull` agent tools are
  Ollama-specific; alternatives use GUI / manual
  download.
- `default_context_window` for new providers is a
  conservative placeholder (8000); config override
  is the escape hatch.

**Twenty-second consecutive deferral of the Channel
Activation Milestone.**

## Phase 132 — `aivyx-auth-cli` Substrate Lift

**Frozen — see [PHASE_132.md](archive/phases/PHASE_132.md).** First substrate
phase since Phase 129 (Google OAuth lift). Operator-
picked at the Phase 131 direction question after the
auth_cli posture reached three concrete data points
(`aivyx-notion`, `aivyx-obsidian`, `aivyx-n8n`) all
carrying near-identical slim non-OAuth auth_cli
surfaces.

**The lift:** ~1177 LoC of duplicated auth_cli code
across the three crates collapses into a ~250 LoC
`aivyx-auth-cli` substrate plus ~50 LoC of consumer
adapters per crate. Net delta: ~-770 LoC.

**Q-block — 2 Recommended picks:**
- Q1a — shared-types-and-helpers granularity (over
  full-trait `ServiceAuth` or hybrid).
- Q2a — new crate `aivyx-auth-cli` (over adding to
  `aivyx-tool`).

**Surface (Task 2 ships):**
- `BinaryMode { Help, Auth(AuthMode), IpcLoop }` /
  `AuthMode { Status, Check }`.
- `parse_cli_args(argv, binary_name)`.
- `ConfigFileError { NotFound, Io, Parse }` — service-
  specific "EmptyToken" / "NotAbsolute" stay on the
  consumer side as a separate enum.
- `default_config_path(service_subdir)`.
- `load_toml<T>`.
- `StatusReport` / `CheckReport` Display impls.

**Three-of-three streak HOLDs predicted.** DESIGN.md →
23, PRODUCT.md → 23, `aivyx-core/src/lib.rs` → 6.
Zero new deps; net LoC negative; test count
approximately neutral.

**Honest scope risks:**
- Three-consumer concurrent migration. Mitigation:
  substrate first (Task 2), then one consumer at a
  time (Tasks 3-5), each behind a green test run.
- Test consolidation may surface inter-consumer
  inconsistencies. Lift picks a single consistent
  behaviour; outliers updated.
- First-substrate-phase-since-129 ergonomics may
  surface follow-on cleanup.

**Twenty-first consecutive deferral of the Channel
Activation Milestone** if Phase 132 ships without
taking it.

## Phase 131 — n8n Workflow Automation Integration (Chapter F #7)

**Frozen — see [PHASE_131.md](archive/phases/PHASE_131.md).** Seventh Chapter F
integration; first targeting a workflow-automation
service. Operator-picked at the Phase 131 direction
question (n8n bundled with the same posture as Phase
130 Notion+Obsidian).

**Substrate generalization:** introduces a fourth Chapter
F substrate shape — REST API + bearer token + operator-
configurable base URL (n8n is self-hosted, no
`api.n8n.io` equivalent). Prior shapes: OAuth (gmail/
calendar/drive), bearer-token-fixed-base (notion), no-API
filesystem (obsidian).

Phase 131 adds:

- **`aivyx-n8n` binary crate** — REST API client against
  `{operator-supplied base URL}/api/v1` with the n8n-
  specific `X-N8N-API-KEY` header.
- **Ten n8n tools** (Q1c non-Recommended over Q1a's
  7-tool default):
  - **Read (`n8n.read`):** `n8n.list_workflows`,
    `n8n.get_workflow`, `n8n.list_executions`,
    `n8n.get_execution`.
  - **Trigger + lifecycle (`n8n.write`,
    CEILING_TRUSTED):** `n8n.execute_workflow`,
    `n8n.activate_workflow`,
    `n8n.deactivate_workflow`.
  - **CRUD (`n8n.write`, CEILING_TRUSTED):**
    `n8n.create_workflow`, `n8n.update_workflow`,
    `n8n.delete_workflow`. The CRUD trio is the
    load-bearing-risky surface — workflow JSON is
    complex, malformed updates can silently break
    automations. Tests exercise required-field
    validation; integration testing against a real
    n8n instance is operator responsibility per Q4a.
- **Two new capability bases:** `n8n.read`, `n8n.write`.
- **A3 amendment:** KNOWN_BASES_COUNT 65 → 67.
- **INSTALL.md walkthrough** with operator setup, the
  workflow-CRUD-is-risky note, n8n version
  compatibility flag, and the execute_workflow async
  semantic (operators poll via get_execution).

**Q-block — 2 Recommended + 1 non-Recommended:**
- Q1c — 10 tools including workflow CRUD (non-
  Recommended; operator-picked).
- Q2a — `n8n_base_url` + `n8n_api_key` in config.toml
  (Recommended).
- Q4a — Operator-discretionary live verification
  (Recommended).

**Streak predictions — three of three HOLD anticipated.**
DESIGN.md HOLD → 22; PRODUCT.md HOLD → 22;
`aivyx-core/src/lib.rs` HOLD → 5 (Phase 130 ticked to 4;
Phase 131 ticks to 5). Zero new workspace deps. Test
count `+140` to `+200` — about half the size of Phase
130's bundled Notion+Obsidian.

**Honest scope risks at sign-off:**
- Q1c's workflow CRUD is the load-bearing risk
  (malformed PATCH can silently break automations).
  PR-merge-time scope reduction (defer the 3 CRUD
  tools to Phase 132+) is the escape hatch.
- n8n version compatibility: targets n8n 1.x REST API;
  bumps in a future substrate phase if n8n revs
  breaking changes.
- Self-hosted base URL hardening: operators may use
  HTTP on localhost (fine) or public-internet HTTP
  (leaks API key). INSTALL.md flags.
- `execute_workflow` returns immediately; operators
  must poll `get_execution` for results. Documented
  in tool description.
- **Auth_cli lift posture: second reinforcing data
  point.** Phase 130 validated; Phase 131 reinforces.
  Phase 132+ candidate.

**Twentieth consecutive deferral of the Channel
Activation Milestone** at Phase 131 exit. The
deferral-count signal-strength reaches its twentieth
phase; Phase 132 candidates now explicitly flag this
as the leading pick.

**At Phase 131 exit:** ten tools shipped (4 read, 3
lifecycle, 3 CRUD); 109 lib tests added; three-of-
three streak HOLD predictions hit; test count miss
(`+109` vs predicted `+140 to +200`) honestly framed
as a tooling-density artifact (symmetric tool pairs +
shared parse_id helper; no coverage gap). Auth_cli
lift now has three concrete data points; Phase 132+
candidate strengthens to "high-leverage". The first
operator-supplied-base-URL Chapter F substrate
shipped clean — verified template for future
self-hosted integrations (GitHub Enterprise, on-prem
Jira, etc.).

## Phase 130 — Notion + Obsidian Knowledge-Management Bundle (Chapter F #5)

**Frozen — see [PHASE_130.md](archive/phases/PHASE_130.md).** Largest Chapter F
phase yet by tool count (13) and task count (17).
Operator-picked at the Phase 130 direction question: bundle
Notion and Obsidian under a "knowledge management" theme
rather than sequencing them across two phases. Two new
crates, two distinct substrate patterns (REST API + token
vs local filesystem + markdown). First non-Google + first
non-OAuth Chapter F phase — proves the substrate
generalizes beyond Google's OAuth dialect.

**Why this, why now:** Knowledge-management picked over
Sheets / Docs / Photos / Tasks for maximum operator
value across the population. Substrate generalization
test: three consecutive Google integrations (Phases
123, 128, 129) might give the impression that Chapter F
is "Google integrations only" — Phase 130 proves the
chapter pattern works for non-OAuth bearer-token auth
(Notion) and no-external-API integrations (Obsidian
filesystem-only). Empirical test signal for the
per-binary auth_cli posture: Notion's slim auth flow
empirically tests whether the per-binary auth_cli is
sustainable at N=4 (gmail + calendar + drive + notion)
or whether the auth_cli lift becomes necessary.

Phase 130 adds:

- **`aivyx-notion` binary crate** — Notion REST API
  client; `api.notion.com/v1` with
  `Authorization: Bearer <integration-token>` +
  `Notion-Version: 2022-06-28` headers.
- **Seven Notion tools** (Q1a):
  - `notion.search` (capability `notion.read`)
  - `notion.get_page` (`notion.read`)
  - `notion.list_database` (`notion.read`)
  - `notion.create_page` (`notion.write`,
    CEILING_TRUSTED)
  - `notion.append_blocks` (`notion.write`,
    CEILING_TRUSTED)
  - `notion.update_page_properties` (`notion.write`,
    CEILING_TRUSTED)
  - `notion.archive_page` (`notion.write`,
    CEILING_TRUSTED)
- **`aivyx-obsidian` binary crate** — local Markdown
  vault client; filesystem operations against a
  configured vault directory with path-traversal
  protection + frontmatter parsing + wikilink/tag
  extraction.
- **Six Obsidian tools** (Q2a):
  - `obsidian.search` (`obsidian.read`)
  - `obsidian.get_note` (`obsidian.read`)
  - `obsidian.list_folder` (`obsidian.read`)
  - `obsidian.create_note` (`obsidian.write`,
    CEILING_TRUSTED)
  - `obsidian.update_note` (`obsidian.write`,
    CEILING_TRUSTED)
  - `obsidian.delete_note` (`obsidian.write`,
    CEILING_TRUSTED)
- **Four new capability bases:** `notion.read`,
  `notion.write`, `obsidian.read`, `obsidian.write`.
- **A3 amendment:** KNOWN_BASES_COUNT 61 → 65.
- **INSTALL.md walkthrough** for both integrations
  including Notion's critical "share each page with
  the integration" UX quirk and Obsidian's path-
  traversal guard posture.

**Q-block — all four Recommended.** Q1a Notion 7-tool
surface; Q2a Obsidian 6-tool surface; Q3a single-vault
config; Q4a operator-discretionary live verification.

**Streak predictions — three of three HOLD anticipated.**
DESIGN.md HOLD → 21; PRODUCT.md HOLD → 21;
`aivyx-core/src/lib.rs` HOLD → 4 (Phase 129 ticked to 3;
Phase 130 ticks to 4). All Phase 130 work in
`aivyx-notion` (new), `aivyx-obsidian` (new),
`aivyx-capability` (four new bases). NO core changes.
Zero new workspace deps (frontmatter parsing is
hand-written — same posture as Phase 127's Python-call
parser). Test count `+200` to `+280` — largest phase to
date by test count.

**Honest scope risks at sign-off:**
- Bundled scope is the largest Chapter F phase yet;
  PR-merge-time scope reduction (defer one integration
  to Phase 131) is the escape hatch.
- Notion's "share each page" UX requires very clear
  INSTALL.md documentation.
- Obsidian's path-traversal guard is load-bearing for
  security; symlink + `..` + absolute-path escape all
  need explicit tests at Task 10.
- Obsidian wikilink fuzzy-resolution deferred (raw
  link strings in the output; Phase 131+ candidate).
- Notion cursor-based pagination has different naming
  from Drive/Calendar's `next_page_token`.
- Notion-Version date pin (`2022-06-28`) shields
  against API breaking changes; future substrate phase
  bumps if Notion deprecates.
- Hand-written YAML frontmatter parser handles the
  common Obsidian shape; exotic frontmatter drops to
  raw-string passthrough.
- Auth CLI lift posture empirically tested by Notion's
  slim auth flow.

**Eighteenth consecutive deferral of the Channel
Activation Milestone.** Honest tracking continues.
Audit's #1. The deferral count's signal-strength is
now load-bearing — Phase 131+ should weigh this
explicitly when picking direction.

**Phase 130 at exit:**
- DESIGN.md HELD → 21. PRODUCT.md HELD → 21.
  aivyx-core/src/lib.rs HELD → 4.
- Test count `+220` (3103 → 3323) — inside the
  predicted `+200 to +280` range.
- Zero new workspace deps. Zero clippy warnings.
- All four Q-block picks Recommended; bundled scope
  stayed tractable.
- **Auth CLI lift posture empirically validated** as
  Phase 131+ substrate work — Notion's slim auth flow
  was awkward to wrap in OAuth-shaped helpers; the
  signal flagged in honest-scope-risks materialized.
- Path-traversal substrate worked as designed; 13
  vault_client tests covering `..` / absolute / symlink
  escape cases.
- 13 tools shipped: 7 Notion + 6 Obsidian.

## Phase 129 — Google Drive + OAuth Substrate Lift (Chapter F #3)

**Frozen — see [PHASE_129.md](archive/phases/PHASE_129.md).** Chapter F
third integration; first Chapter F phase to ship two pieces of
substrate work in one phase — the OAuth substrate lift
Phase 128 Q2a deferred to "the third Google integration"
fires here. Three would-be in-tree OAuth copies (gmail +
calendar + drive) collapse to one shared
`aivyx-google-oauth` crate before the third lands.

**Why this, why now:** Operator-picked over GitHub /
Notion / Slack-tools at the Phase 129 direction question
for the "highest substrate-completion value: triggers
the OAuth lift" framing. Mirrors Phase 128's SDK-harness-
lift posture exactly — Phase 125 named the harness lift's
N=3 trigger; Phase 128 fired it. Phase 128 named the
OAuth lift's N=3 trigger; Phase 129 fires it.

Phase 129 adds:

- **OAuth substrate lift** (Task 2) — extract gmail's
  `oauth/` + `auth_cli/helpers` to a shared
  `aivyx-google-oauth` crate; migrate `aivyx-gmail` +
  `aivyx-calendar` to consume the lifted crate; build
  `aivyx-drive` against it from the start. Both existing
  crates retain their binary's CLI surface as thin
  wrappers over the lifted helpers, parameterized by
  service name + default scope set.
- **`aivyx-drive` binary crate** — Chapter F #3 separate
  binary; consumes the lifted OAuth substrate.
- **Seven Drive tools** (Q2b non-Recommended operator
  pick over Q2a's 5-tool default):
  - `drive.search` (capability `drive.read`)
  - `drive.get_metadata` (`drive.read`)
  - `drive.list_folder` (`drive.read`)
  - `drive.create_folder` (`drive.write`,
    CEILING_TRUSTED)
  - `drive.download_file` (`drive.read`; 10 MB cap with
    `content_truncated` flag above)
  - `drive.upload_file` (`drive.write`,
    CEILING_TRUSTED; 10 MB cap)
  - `drive.delete_file` (`drive.write`,
    CEILING_TRUSTED; idempotent)
- **Two new capability bases:** `drive.read`,
  `drive.write`.
- **A3 amendment:** KNOWN_BASES_COUNT 59 → 61.
- **INSTALL.md walkthrough** with operator-setup paths
  (already-have-Gmail/Calendar reuse, fresh GCP project
  setup), scope-narrowing options, troubleshooting,
  10 MB cap notes.

**Q-block (3 Recommended + 1 non-Recommended):**
- Q1a — Bundle OAuth substrate lift into Phase 129
  (Recommended).
- Q2b — 7 tools (read + write + folder management)
  (non-Recommended; operator-picked over Q2a's 5-tool
  default for hierarchical-document workflow value).
- Q3a — Base64 in JSON with size cap at 10 MB
  (Recommended).
- Q4a — Operator-discretionary live verification
  (Recommended).

**Streak predictions — three of three HOLD anticipated.**
DESIGN.md HOLD → 20; PRODUCT.md HOLD → 20;
`aivyx-core/src/lib.rs` HOLD → 3 (Phase 128 ticked to 2;
Phase 129 ticks to 3). All Phase 129 work in
`aivyx-google-oauth` (new), `aivyx-drive` (new),
`aivyx-gmail` + `aivyx-calendar` (OAuth migration),
`aivyx-capability` (two new bases). NO core changes.
Zero new workspace deps. Test count `+120` to `+170` —
lower than Phase 128's `+167` because Phase 128 inherited
the OAuth+auth_cli body via inline copy; Phase 129
consolidates it to one shared crate (Drive inherits via
dependency, not duplication).

**Honest scope risks at sign-off, status at exit:**
- Q2b's 7-tool surface stayed tractable; no PR-merge-
  time scope reduction needed. Folder-semantics test
  surface paid off in correctness (single-quote /
  backslash escaping caught at unit-test layer).
- OAuth lift did NOT break behavior. Net diff at
  Task 2 commit: **-900 LoC** (substantial duplication
  collapsed). 117 gmail + 131 calendar tests pass
  unchanged; the 30 each delta reflects oauth
  implementation tests consolidating to the lifted
  crate.
- 10 MB cap operator-visible as planned; INSTALL.md
  documents.
- Google-native types handled via export endpoint with
  default mime mappings (Docs→PDF, Sheets→CSV, etc).
- Resumable upload deferred per Q3a.
- Drive's auth/drive default is broad; INSTALL.md
  documents three narrower options.

**Streak predictions — three of three correct.**
DESIGN.md HELD → 20; PRODUCT.md HELD → 20;
`aivyx-core/src/lib.rs` HELD → 3 (Phase 128 ticked to
2; Phase 129 ticks to 3). Test count **`+117`
undershot the predicted `+120 to +170` range by 3 tests**
— honest report: the OAuth lift's test consolidation
worked better than estimated (61 duplicated oauth tests
collapsed to one shared crate's 31). Not a "skipped
tests" undershoot; a "substrate-consolidation worked"
undershoot. Zero new workspace deps. Zero clippy
warnings.

**Q-block went through as picked.** Three Recommended
+ one non-Recommended (Q2b 7-tool surface). No mid-task
re-asks.

**Auth CLI still per-binary as an honest tech debt
flag.** Each Chapter F binary keeps its own auth_cli
module after the OAuth lift; the helpers could lift in a
future substrate phase if operator pressure surfaces.

**Seventeenth consecutive deferral of the Channel
Activation Milestone.** Honest tracking continues.
Audit's #1. The deferral count is now a load-bearing
signal in its own right.

## Phase 128 — Google Calendar Integration (Chapter F #2)

**Frozen — see [PHASE_128.md](archive/phases/PHASE_128.md).** Chapter F
second integration; first Chapter F phase since Phase 123
shipped Gmail. Operator-pressure pick after Phase 127 exit listed
Chapter F #2 / Chapter G #2 as operator-tool-surface
candidates; Calendar picked over GitHub / Drive / Budget
tracking for the "common operator ask, load-bearing for the
personal-assistant value prop" framing.

**Lands the SDK harness lift alongside.** Phase 125 exit
explicitly named "land the lift alongside whichever Chapter
F #2 or Chapter G #2 ships first" — Phase 128 is that
moment. Three would-be in-tree copies (gmail + toolkit +
calendar) crosses the clear-win threshold; lift collapses
to one shared substrate before the third copy lands.

Phase 128 ships:

- **SDK harness lift** — extract the multi-tool harness
  from `aivyx-gmail/src/harness.rs` +
  `aivyx-toolkit/src/harness.rs` to a shared module in
  `aivyx-tool`; both crates migrate to consume the lifted
  version with behavior preservation.
- **`aivyx-calendar` binary crate** — separate binary per
  Chapter F pattern; inline-copies the OAuth substrate
  from `aivyx-gmail` (third Google integration triggers
  the OAuth lift; Calendar is N=2).
- **Five Calendar tools** (Q3b non-Recommended operator
  pick):
  - `calendar.list_events` (capability `calendar.read`)
  - `calendar.get_event` (`calendar.read`)
  - `calendar.create_event` (`calendar.write`,
    CEILING_TRUSTED)
  - `calendar.update_event` (`calendar.write`,
    CEILING_TRUSTED)
  - `calendar.delete_event` (`calendar.write`,
    CEILING_TRUSTED)
- **Two new capability bases:** `calendar.read`,
  `calendar.write`.
- **`aivyx-pa auth` extension** — Calendar uses the same
  OAuth flow as Gmail with a different scope
  (`auth/calendar`); auth_cli copy parameterizes.
- **INSTALL.md walkthrough** — operator-side setup
  mirroring Phase 123's Gmail section; same GCP project
  works for both APIs.

**Q-block (3 Recommended + 1 non-Recommended):**
- Q1a — Bundle SDK harness lift (Recommended).
- Q2a — Copy OAuth substrate inline from aivyx-gmail
  (Recommended; OAuth lift deferred to N=3 trigger).
- Q3b — 5 tools (non-Recommended; operator-picked over
  Q3a's 4-tool surface for the marginal `update_event`
  value).
- Q4a — Operator-discretionary live verification
  (Recommended).

**Streak predictions — three of three HOLD anticipated.**
DESIGN.md HOLD → 19; PRODUCT.md HOLD → 19;
`aivyx-core/src/lib.rs` HOLD → 2 (Phase 127 rebuilt to 1;
Phase 128 ticks to 2). All Phase 128 work in
`aivyx-calendar` (new), `aivyx-tool` (harness lift),
`aivyx-gmail` + `aivyx-toolkit` (harness migration). NO
core changes. Zero new workspace deps. Test count `+150`
to `+200` anchored on Phase 123's `+151` for 4 tools
through OAuth.

**Honest scope risks at sign-off, status at exit:**
- Q3b's doubled write-tool review surface stayed
  tractable; all five tools shipped without scope
  reduction. Per-tool test parity (~16 tests each).
- OAuth copy is N=2 in-tree as predicted; OAuth
  substrate lift deferred to the next Google
  integration (N=3 trigger).
- Harness lift did NOT break behavior; all 1,108 prior
  tests across gmail + toolkit pass unchanged. Net
  diff -304 LoC despite adding 488 lines of new
  substrate (488 - 2×417 + 2×17 = -312 LoC delta).
- Google Calendar API quotas remain operator-side as
  documented in INSTALL.md.
- Default scope is the broad `auth/calendar`;
  INSTALL.md documents three narrower options for
  operators wanting tighter posture.

**Streak predictions — three of three correct.**
DESIGN.md HELD → 19; PRODUCT.md HELD → 19;
`aivyx-core/src/lib.rs` HELD → 2 (Phase 127 rebuilt to
1; Phase 128 ticks to 2). Test count `+167` landed
inside the predicted `+150 to +200` range. Zero new
workspace deps. Zero clippy warnings.

**Q-block went through as picked.** Three Recommended
+ one non-Recommended (Q3b 5-tool surface). No mid-task
re-asks.

**Sixteenth consecutive deferral of the Channel
Activation Milestone.** Honest tracking continues.
Audit's #1.

## Phase 127 — Multi-Format Tool-Call Extraction (substrate completion)

**Frozen — see [PHASE_127.md](archive/phases/PHASE_127.md).** Phase 126's
substrate gap finished. Phase 126 shipped a textual
extractor handling two emission formats — the two
Phase 124 happened to observe. Phase 126 close-out
research catalogued ~10 distinct text-form tool-call
formats across local LLM families (Llama, Mistral,
Qwen, Qwen3-Coder, DeepSeek, Phi, Gemma 3, Gemma 4,
bare-JSON, tool-code-JSON). Phase 127 finishes the
substrate by adding the four load-bearing-missing
parsers + a hybrid family-hint architecture so the
substrate is genuinely model-agnostic.

**Why this, why now:** end-users pick their local
model based on their hardware (Llama 3.x for CPU-
friendly setups, Qwen3-Coder for code-heavy work,
Mistral Nemo for the midrange, Phi-4-mini for edge
devices, Gemma 3/4 for the Google-fine-tuned path,
DeepSeek R1 for reasoning). Aivyx PA's tool-call
substrate has to handle whichever the operator picked.
The Phase 126 amendment was explicit: fix the tooling,
not document the gap.

Phase 127 adds:

- **Qwen3-Coder XML parser** (Task 2) — for the
  emission qwen3.5/3.6 actually produces per Ollama
  issue #14745 (`<tool_call><function=N><parameter=K>V</parameter></function></tool_call>`).
  Most load-bearing for the user's local stack.
- **Phi-4-mini `<|tool_call|>` wrapper** (Task 3) —
  JSON-list inside special-token wrapper.
- **Gemma 3 ```tool_code` python-fence** (Task 4) —
  markdown-fence wrapper + hand-written Python-call
  to JSON-args translator.
- **Bare-JSON extractor with FP guard** (Task 5) —
  raw JSON without wrapper (qwen3:32b#11662 pattern);
  guard against extracting JSON the operator mentions
  in prose.
- **Hybrid family-hint architecture** (Task 6) — query
  Ollama `/api/show` once at session start, use
  reported family to prioritize parser choice;
  permissive fallback. Not family-strict (because
  Ollama lies about family vs. pipeline-wiring per
  the upstream bug literature).
- **INSTALL.md substrate-coverage matrix +
  operator-facing model-picking guidance** (Task 8).

**Q-block — all six Recommended** (Q1a Qwen3-Coder XML
+ Q2a bare-JSON with FP guard + Q3a Phi-4-mini wrapper
+ Q4a Gemma 3 python-fence + translator + Q5a hybrid
family-hint + Q6a INSTALL.md matrix). Fourth all-
Recommended phase in a row (Phase 124, 125, 126, 127).

**Streak predictions — three-of-three HOLD anticipated.**
DESIGN.md HOLD → 18; PRODUCT.md HOLD → 18;
aivyx-core/src/lib.rs HOLD → 1 (Phase 126 reset; Phase
127 rebuilds from 0). Substrate work is purely additive
in `textual_tool_call.rs` + new `family_hint`-adjacent
module in aivyx-llm. Zero new workspace deps. Test
count `+70` to `+90` (Python-call grammar in Task 4 is
the test-heaviest surface).

**No live verification** as an exit criterion. Phase
127 substrate is unit-tested per-format; family-hint
architecture is unit-tested for routing. The operator
can run an interactive session post-exit to validate
empirically; this is operator-discretionary, not a
phase exit gate.

**Honest scope risks at sign-off, status at exit:**
- Qwen3.5/3.6 may still fail even after Task 2 because
  Ollama issue #14601 leaves tool *definitions* malformed
  (Go struct strings in modelfile template); model can't
  see correct schemas. Phase 127 closes the parsing gap,
  not the upstream Ollama schema-rendering gap.
  INSTALL.md surfaces the `qwen3-coder:N` workaround.
- Python-call translator is the most complex piece;
  malformed-but-recoverable inputs may drop instead of
  repairing. Conservative posture matches Phase 126.
- Family-hint is a hint, not a contract — permissive
  fallback covers Ollama's pipeline-wiring mismatches.
- Phase 127 doesn't address LLM-side tool-call
  reliability; a model that doesn't *want* to call a
  tool can't be rescued by any parser.

**Streak predictions — three of three correct.**
DESIGN.md HELD → 18; PRODUCT.md HELD → 18;
`aivyx-core/src/lib.rs` HELD → 1 (Phase 126 reset; Phase
127 rebuilds from 0 with this phase). Test count `+95`
overshot the predicted `+70 to +90` range — honest
report: Task 4's Python-call grammar (ten value types ×
ten malformed-input cases) and Task 5's bare-JSON FP
guard (the substrate's most FP-prone parser) ran
test-heavier than the entry-doc estimate, both for
sound substrate reasons. Zero new workspace
dependencies. Zero clippy warnings.

**Q-block went through as picked.** All six
Recommended (Q1a Qwen3-Coder XML + Q2a bare-JSON FP
guard + Q3a Phi-4-mini wrapper + Q4a Gemma 3
python-fence + Q5a hybrid family-hint + Q6a INSTALL.md
matrix). Fourth all-Recommended phase in a row (Phase
124, 125, 126, 127). No mid-task re-asks.

**Helper-refactor surfaces (unplanned but honest).**
Task 3 refactored `RECOGNIZED_WRAPPERS: &[&str]` →
`WRAPPERS: &[WrapperSpec]` with explicit
`(tag, open, close)` triples to model Phi-4-mini's
asymmetric `<|tool_call|>` / `<|/tool_call|>` wrapper
and Gemma 3's markdown fence. Task 3 also changed
`parse_inner`'s return from `Option<...>` to
`Vec<...>` because Phi-4-mini's JSON-list shape
produces N calls per wrapper. Both refactors preserved
Phase 126's existing tests unchanged.

**Live verification deliberately skipped** per the
Phase 126 amendment precedent. The substrate is
unit-tested per-format; the family-hint architecture is
unit-tested for routing + caching + failure handling.
Operators can run an interactive session post-exit to
validate empirically; not a phase exit gate.

**Fifteenth consecutive deferral of the Channel
Activation Milestone.** Honest tracking continues.
Audit's #1.

## Phase 126 — Textual Tool-Call Extraction

**Frozen — see [PHASE_126.md](archive/phases/PHASE_126.md).** Operator
picked this as the next direction after Phase 125 exit
listed it as the most concrete substrate move remaining
on the local-LLM-rehab axis. Phase 124's secondary finding
(qwen3 emits structurally correct tool-call JSON in
RESPONSE TEXT, wrong channel) is what this phase rescues.

**Not "Local-LLM Rehab #5"** — the substrate is provider-
agnostic. Any model emitting tool-call JSON in response
text gets rescued by a planner-side extractor; just
happens that the load-bearing motivating case is qwen3.

Phase 126 adds:
- New `aivyx-core/src/textual_tool_call.rs` module —
  pure-function extractor recognizing `<tool_code>` and
  `<tool_call>` wrappers, both `name`/`arguments` and
  `tool`/`parameters` JSON shapes.
- Planner integration in `llm_planner` — when the LLM
  response has zero protocol tool_calls AND content has
  extractable text, extract → synthesize → dispatch.
  Phase 120 fuzzy-recovery composes on the extracted
  tool name (catches gemma4's `fs.write_file` →
  `fs.write` at lowered threshold ~0.60).
- New `AuditTag::ToolCall.extracted_from_text:
  Option<String>` field for forensic visibility.

**Q-block (all four Recommended — third all-Recommended
phase in a row):**
- Q1a — Planner-side extraction.
- Q2a — Both `<tool_code>` AND `<tool_call>` wrappers +
  both JSON shapes.
- Q3a — New `extracted_from_text` audit field
  (**breaks lib.rs streak at 6** — honest framing per
  Phase 6 Q5; the alternative made audit chain
  misleading).
- Q4a — Live verification against qwen3 + gemma4 at
  exit; honest reporting regardless.

**Streak predictions — two of three correct + one
break as predicted.** DESIGN.md HELD → 17; PRODUCT.md
HELD → 17; **aivyx-core/src/lib.rs BROKE as predicted
via Q3a** (resets 6 → 0; rebuild starts Phase 127+).
Test count `+35` undershot the predicted `+40 to +60`
range — honest report: the Task 4 helper-refactor
(extracted `process_one_call` from the Phase 120/101
inline loop) meant the new extraction branch reused
existing 55-test coverage rather than duplicating
tests. Zero new workspace dependencies.

**Q-block went through as picked** (all four
Recommended; Q3a's streak-break trade-off held).

**Honest scope risks at sign-off, status at exit:**
- Streak break locked in by Q3a; reset confirmed.
- Extraction works but model intent may not — this
  is the load-bearing live-verification question;
  exit doc carries the empirical findings.
- gemma4 rescue paired with operator lowering
  `tool_name_auto_correct_threshold` to ~0.60;
  INSTALL.md documents this.
- qwen3 `<tool_code>` emission was observed in one
  Phase 124 session; reproduction is the live test's
  signal.

**Live verification — amended out at close-out.** The
operator amended Q4a after pre-flight surfaced both
the empirical signal (dev-verify against qwen3.6:27b
on Ollama 0.24.0: 5 turns, `tool_calls_made: 0`
everywhere, zero extracted calls) AND the substrate
gap (literature shows qwen3.5/3.6 emits Qwen3-Coder
XML inside `<tool_call>`, not the JSON Phase 126's
parser handles; per Ollama issue #14493 the qwen3.5
family is wired to the wrong renderer/parser pipeline
upstream). A redundant live test was rejected in favor
of moving directly into substrate-completion work.
See PHASE_126.md "Research-driven amendment" section.

**Phase 127 picks up the substrate work** — Multi-
Format Tool-Call Extraction — adds Qwen3-Coder XML,
bare-JSON, Phi-4-mini, and Gemma 3 python-fence
parsers, plus a hybrid family-hint architecture
backed by Ollama `/api/show`.

**Fourteenth consecutive deferral of the Channel
Activation Milestone.** Honest tracking. Audit's #1
unchanged.

## Phase 125 — Personal Assistant Tool Bundle (Chapter G #1)

**Frozen — see [PHASE_125.md](archive/phases/PHASE_125.md).** Chapter G
opener. Operator-pressure pick after the Phase 124 exit
framing left operator-tooling-coverage as the load-bearing
question.

Phase 125 ships `aivyx-toolkit` — a single binary registering
8 tools through Phase 123's multi-tool harness substrate:

- **`web.search`** (1 tool, scope `web.search`) — Brave
  Search API; operator-provided API key in
  `~/.aivyx-pa/tool-processes/toolkit/config.toml`. Free tier
  fits personal use.
- **`task.create / list / complete / delete`** (4 tools,
  scopes `task.read` + `task.write`) — lightweight TODO
  tracking; JSON file storage at
  `~/.aivyx-pa/tool-processes/toolkit/tasks.json` with
  0600 perms.
- **`health.check.add / list / recent_changes`** (3 tools,
  scopes `health.read` + `health.write`) — scheduled URL
  monitoring; tool-process-side polling loop; state
  transitions recorded in ring buffer; alert dispatch
  composed by agent loop via `recent_changes` +
  `notify.send` (substrate-minimal — defers
  daemon-side automatic dispatch to Phase 126+).

**Q-block (all three operator-resolved pre-Task-2):**
- Q1 — Bundle three tools in one phase (over sequence /
  hybrid).
- Q2 — `web.search` first within the bundle.
- Q3 — `health.check` shape: scheduled monitoring with
  state + alerts (Recommended; over one-shot / defer).

**Capability bases:** five new bases in `KNOWN_BASES`
(`web.search`, `task.read`, `task.write`, `health.read`,
`health.write`); all CEILING_TRUSTED only by default.
A3 amendment bumped 52 → 57 with new "Personal assistant
tool process scopes" category.

**Streak predictions — three of three correct.** DESIGN.md
HELD → 16; PRODUCT.md HELD → 16; aivyx-core/src/lib.rs
HELD → 6 (90/10 hold case held; new crate; no core
changes). Test count `+102` inside `+80 to +130`
predicted range (8 tools × per-tool tests + the
substantial Task 5 polling-loop substrate landed in the
middle of the range). Zero new workspace dependencies
as predicted.

**Q-block went through as picked.** All three Recommended
(Q1a bundle + Q2a web.search first + Q3a scheduled
monitoring). No mid-task re-asks; no architectural
constraints surfaced post-sign-off.

**Honest scope risks at sign-off, materialized at exit:**
8 tools in one phase was dense but tractable — Phase 123's
multi-tool harness substrate carried most of the per-tool
boilerplate. Local-LLM invocation reliability is unchanged
as predicted; Phase 125 expanded the surface that works
WHEN tool invocation works. The health-check alert
composition recipe works reliably under cloud providers;
local Ollama won't reliably chain `recent_changes` →
`notify.send`.

**SDK-validation finding from Phase 123 now twice-
duplicated.** `aivyx-gmail/src/harness.rs` +
`aivyx-toolkit/src/harness.rs` are line-for-line copies
of the multi-tool harness. Two in-tree copies is the
clear-win threshold for the lift. Phase 126 should land
the lift alongside whichever Chapter F #2 or Chapter G
#2 ships first.

**Thirteenth consecutive deferral of the Channel
Activation Milestone.** Operator framing on the deferral
is load-bearing correct: channels without working tools
is wrong-order. Phase 125 prioritized the tools; the
deferral count now stands at 13. Phase 126 candidates
include the milestone itself; honest tracking continues.

## Phase 124 — Few-Shot Tool-Call Examples (Local-LLM Rehab #4)

**Frozen — see [PHASE_124.md](archive/phases/PHASE_124.md).** Fourth named
phase against the local-LLM rehabilitation axis. Operator-
pressure pick after the Phase 124 direction question:
rejected all standing candidates (Chapter F #2, Channel
Activation Milestone, release prep, SDK harness lift) and
named the Phase 122 gemma4-refusal transcript line as the
specific issue to focus on.

Phase 124 extends `OllamaFamilyStrategy` with a third
variant `FewShotExamples` — appends 2-3 worked tool-call
examples (with explicit WRONG/RIGHT framing against the
"I don't have X" refusal pattern) after Phase 122's
structured-injection block. Per-family defaults upgraded:
qwen3 + gemma4 → FewShotExamples (was StructuredInjection);
llama3 → None unchanged. New `apply_ollama_prompt_strategy`
dispatcher hides the per-call-site logic across the 5
prompt-assembly wiring sites in the binary.

**Q-block (all three Recommended — first all-Recommended
through-as-picked since Phase 121):**
- Q1a — Few-shot examples in the prompt (Recommended).
- Q2a — Static hardcoded examples (Recommended).
- Q3a — Live verification against both qwen3.6 + gemma4
  at exit; honest reporting (Recommended).

**Streak predictions — three of three correct.** DESIGN.md
HELD → 15; PRODUCT.md HELD → 15; `aivyx-core/src/lib.rs`
HELD → 5 (90/10 hold; substrate stays in aivyx-channel +
aivyx-config). Test count `+12` inside `+15 to +30` (close
to lower bound; substrate-additive shape kept tests
focused on helper formatting + dispatcher invariants).
Zero new workspace deps.

**Honest scope risk that materialized at sign-off:** Phase
122 demonstrated that putting the tool name directly in
the prompt doesn't fix the capability-denial prior. Few-
shot examples are a different mechanism but not a
guaranteed one. The live-verification outcome at exit
determines whether this substrate breaks through or
whether four substrate phases (120 + 121 + 122 + 124) all
hit the same model-layer wall.

**Live verification outcome — fourth-substrate-phase
failure on the load-bearing invocation question.** Neither
qwen3.6:27b nor gemma4:31b produced an actual fs.write
invocation through the protocol. A bonus glm-4.7-flash
control (undetected family → strategy=none) showed the
same failure mode without any Aivyx PA substrate, confirming
the failure is at the model layer, not Phase 124's
substrate. **The model-layer ceiling holds definitively
after four substrate attempts (Phase 120 fuzzy recovery
+ Phase 121 native protocol + Phase 122 structured catalog
+ Phase 124 few-shot examples).**

Three secondary findings worth carrying forward to
Phase 125: (1) qwen3 emitted structurally-correct
`<tool_code>` JSON as TEXT — wrong channel, but textual-
tool-call extraction would rescue it; (2) gemma4's
invented `fs.write_file` sits at similarity 0.667,
below Phase 120's 0.80 default — operators can opt-in
by lowering the threshold; (3) the failure mode is
identical with NO substrate (glm control), so substrate
isn't making things worse.

**FewShotExamples stays as the default** for qwen3 +
gemma4 — not because it broke through, but because the
Phase 124 outcomes (text-form tool-call attempt;
hallucinated alternative) are slightly more substrate-
rescuable than Phase 122's outcomes (timeout; refusal)
even though neither produces a real invocation.

**Phase 125 should pivot to operator-side mitigation,
not a fifth substrate attempt** — see PHASE_124.md
exit doc for the full reasoning.

**Thirteenth consecutive deferral of the Channel
Activation Milestone.** Honest tracking. Audit's #1
unchanged.

## Phase 123 — Gmail Integration (Chapter F #1)

**Frozen — see [PHASE_123.md](archive/phases/PHASE_123.md).** Chapter F
opener. Operator-pressure pick after the audit's #1
(Channel Activation Milestone) was deferred for the
twelfth time. Targets the external-productivity-
integrations axis: Aivyx PA as productivity assistant, not
just chat surface.

Phase 123 ships Gmail as a separate `aivyx-gmail` binary
wired via the existing `[[tool_process]]` substrate (P12,
Phase 49). Architectural constraint surfaced post-Q-block
sign-off: P10 caps substrate at thirteen tools forever and
explicitly names email as third-party territory; Phase
123 is the first real consumer of the third-party SDK
contract (P11 + P12). Q3 (token storage) was re-asked
under the corrected constraint and resolved cleanly to
per-tool-process file storage.

**Tool surface (Q2c non-Recommended — all four shipped):**
- `gmail.search` — Gmail query DSL (capability scope
  `email.read`).
- `gmail.read` — one full message by ID (`email.read`);
  flattens recursive MIME payload + decodes body parts +
  enumerates attachment metadata.
- `gmail.draft` — RFC 5322 draft creation (`email.write`);
  drafts require explicit Gmail-UI send by the operator.
- `gmail.send` — direct send (`email.send`); Trusted-tier
  only by default (mirrors `shell.exec` / `notify.send`
  gating per Phase 62 Q2(a)). No undo from Aivyx PA.

**Auth substrate (Q1a Recommended):**
- Operator-provided Google OAuth app — operator creates
  own GCP project, pastes `client_id` + `client_secret`
  into `~/.aivyx-pa/tool-processes/gmail/config.toml`. Aivyx PA
  ships no shared OAuth app.
- `aivyx-gmail auth init` CLI handles auth-code exchange
  via a local-loopback HTTP listener; CSRF state binding
  via UUID v4.
- Per-tool-process token file at
  `~/.aivyx-pa/tool-processes/gmail/tokens.json` (0600 perms,
  atomic write-then-rename). Auto-refresh ~60s before
  token expiry; refresh-token preserved across refreshes
  (Google's typical behavior).

**Streak predictions (3-of-3 held):** DESIGN.md HELD → 14;
PRODUCT.md HELD → 14 (P10/P11/P12/G6 already covered the
case); `aivyx-core/src/lib.rs` HELD → 4 (90/10 hold case
held; new crate, no core change). Test count `+151`
substantially overshot the predicted `+30 to +55` —
honest report per Phase 6 Q5; per-task breakdown in the
exit doc. Zero new workspace dependencies (predicted "at
most 1"; reality: 0).

**Load-bearing SDK-validation finding from exit:**
[`aivyx_tool::run_tool_as_subprocess`] wraps exactly ONE
tool; Chapter F integrations naturally have multiple
tools per process. Phase 123 wrote a multi-tool harness
inline in `aivyx-gmail`; recommendation for a follow-on
phase is to lift it back into `aivyx-tool` as
`run_tools_as_subprocess`. Cheap (~40 LoC + 15 LoC tests);
best landed alongside the first Chapter F #2 integration.

**Honest scope risks named at sign-off, materialized at exit:**
- **Q2c full-surface risk:** shipped clean; the four
  tools share substrate (GmailClient + mime module) so
  the per-tool marginal cost was small. Q2c was the
  right pick.
- **OAuth complexity:** INSTALL.md walkthrough landed
  with 6 numbered setup steps + 5 troubleshooting items.
  Operator onboarding is testable only by operators
  actually onboarding; left to operator setup.
- **SDK contract gap:** ONE gap surfaced as documented
  above; rest of the SDK held perfectly.
- **Google's sensitive-scope verification:** documented
  in INSTALL.md as a heads-up; out of scope for self-
  hosted single-operator use.

**Twelfth consecutive deferral of the Channel Activation
Milestone.** Honest tracking. The audit's #1 has now been
chosen against twelve times; thirteenth deferral if
Phase 124 also skips it.

## Phase 122 — Per-Model Prompt Variants (Local-LLM Rehab #2)

**Frozen — see [PHASE_122.md](archive/phases/PHASE_122.md).** Third-named
phase against the audit's local-LLM rehabilitation axis,
opened against signal from real interactive use after
Phase 121 shipped: 13 turns × 0 tool calls between
qwen3.6:27b and gemma4:31b. Both models confabulate
their tool catalogs at the prose level; both refuse or
return empty when commanded to invoke. Phase 120
substrate is orthogonal to this failure mode (it
catches hallucinated invocations; this is hallucinated
capability denial).

Phase 122 ships per-family TOML config + structured
per-turn tool catalog injection (the new
`aivyx_channel::profile_prompt::append_tool_catalog`
helper). Per-family detection from model name prefix
(qwen3.6 → `qwen3`, gemma4 → `gemma4`, llama3.1 →
`llama3`); per-family defaults supplied for `qwen3`
and `gemma4` (both `structured_injection`); operator-
tunable via `[ollama.prompt_strategies] <family> =
"..."`. Startup config banner surfaces resolved
strategy + provenance (`family: qwen3, default` /
`family: qwen3, override` / `family: undetected`).

**Q-block (two non-Recommended) — all three through as picked:**
- Q1c — Both substrate moves (non-Recommended). Per
  Phase 6 Q5 honest framing correction documented in
  open doc: "suppress in-prompt catalog" is a no-op
  (Aivyx PA's system prompt has no tool catalog; tools
  flow via Ollama's protocol tools array only). The
  pick reduced to "ship per-turn injection."
- Q2a — Per-model-family TOML config (Recommended).
- Q3b — Try everything; declare reality at exit (non-
  Recommended; refused pre-emptive scope reduction).

**Streak predictions — three of three correct:**
DESIGN.md HELD → 13; PRODUCT.md HELD → 13;
`aivyx-core/src/lib.rs` HELD → 3 (85/15 hold case
held; substrate placed entirely in aivyx-channel +
aivyx-config). Test count `+38` within `+35 to +60`.
Zero new workspace deps.

**Mid-Task-2 honest-scope correction:** the
`detect_model_family` helper's original draft mapped
qwen3.6 → `qwen35` via a `take(2)`-on-digits collapse
the docstring asserted but the code contradicted
(actually produced `qwen36`). Switched to major-
version-only (matching gemma + llama) in the same
task; tests and defaults updated. Phase 6 Q5 honesty
applied at the task level.

**Live verification outcome — partial substrate effect;
capability-denial prior is prompt-unreachable.** Both
qwen3.6:27b and gemma4:31b ran under
`structured_injection` (banner confirmed). Catalog
confabulation **measurably reduced** on both: qwen3.6
stopped inventing "Good Morning"-style names; gemma4
stopped generating 60+ fictional tools (though gemma4
went to vague prose categorization rather than exact-
name listing). But the load-bearing test —
*invocation on command* — **failed for both**:
qwen3.6's fs.write turn timed out; gemma4 explicitly
refused while `fs.write` was literally listed in the
injection block. Audit: zero tool calls across both
sessions. The substrate ceiling sits at the model
layer, exactly as the open-doc honest-scope flag
predicted. Operators keep `structured_injection` for
enumeration honesty or fall back to `prompt_strategy =
"none"` for cost-sensitive deployments. See PHASE_122.md
"Live verification (Task 7)" for the empirical table.

**Eleventh consecutive substrate/polish phase picked
over the Channel Activation Milestone.** Honest
tracking. The audit's #1 has been chosen against
eleven times. After Phase 122, Phase 123 candidates:
Channel Activation Milestone (twelfth-in-a-row
deferral if skipped), release prep, Chapter F,
operator-pressure-driven new direction.

## Phase 121 — Native Ollama Tool-Calling Path (Local-LLM Rehab #3)

**Frozen — see [PHASE_121.md](archive/phases/PHASE_121.md).** Second-
named phase against the audit's local-LLM rehabilitation
axis. New dedicated `OllamaProvider` in
`aivyx-llm/src/ollama/` mirroring `aivyx-llm/src/openai/`.
Talks Ollama's `/api/chat` natively (JSONL streaming
protocol); bypasses the OpenAI-compat translation layer
entirely. `ProviderKind::Ollama` routes to the new
adapter by default; OpenAI-compat stays untouched for
`provider = "openai"`.

**Q-block (one non-Recommended):**
- Q1a — New dedicated `OllamaProvider` (Recommended).
- Q2a — Full streaming via JSONL protocol (non-
  Recommended; picked over pragmatic "non-streaming for
  tools" Recommended). Honest scope expansion; the Q2a
  pick paid off — full JSONL streaming supports text-
  deltas-then-tools shape cleanly.
- Q3a — `ProviderKind::Ollama` switches to native by
  default (Recommended). Operators get native benefits
  transparently.

**Streak outcomes:** 3-of-3 predictions correct. Phase
121 returns to the Phase 119 posture: all three streaks
held; the lib.rs streak re-establishes to 2 after
Phase 120's break. Q1a's "dedicated module" pick
deliberately kept changes off lib.rs and the 80% hold
case landed.
- DESIGN.md: HELD as predicted. Streak → 12.
- PRODUCT.md: HELD as predicted. Streak → 12.
- `aivyx-core/src/lib.rs`: HELD as predicted (80% hold).
  New adapter lives in aivyx-llm; OllamaOptions config
  struct in aivyx-config; dispatch routing in
  aivyx-channel. Streak re-establishes 1 → 2.

Test count: 2346 → 2400 (+54). Within the predicted
`+40 to +70` band. Each task carries focused coverage
for one Phase 121 surface.

Zero new workspace deps (`provider-ollama` feature
reuses existing transport deps).

**Honest scope caveat held at exit.** The open doc
flagged that the native adapter does NOT fix model-
shaped hallucination patterns directly; reality
matched. Phase 121's contribution is native protocol
fidelity, operator-tunable Ollama options
(`num_ctx`, `num_predict`, `mirostat`, etc.), and zero
translation-layer debugging. Phase 120's substrate
recovery flows uniformly through both adapters.

**Tenth consecutive substrate/polish phase picked over
the Channel Activation Milestone.** Honest tracking;
the audit ranking has stayed unchanged for ten phases.

After Phase 121, Phase 122 candidates: Channel
Activation Milestone (eleventh-in-a-row deferral),
per-model prompt variants (local-LLM rehab #2, still
unshipped), release prep, Chapter F.

## Phase 120 — Tool-Call Validation + Recovery (Local-LLM Rehabilitation #1)

**Frozen — see [PHASE_120.md](archive/phases/PHASE_120.md).** First-named
phase against the audit's local-LLM rehabilitation axis.
Closes the qwen3.6/gemma4 tool-name hallucination failure
mode (per phase-99-local-builds memory): when a local
model emits a hallucinated tool name like `fs_read`, the
LLM provider flags it at the LLM boundary; the planner
runs fuzzy-match auto-correct (Phase 112 `title_similarity`
reuse, threshold `0.80`); above threshold → auto-correct
dispatch + audit-event; below → structured "did you
mean?" message to the model so it retries.

**Q-block (two Recommended, one non-Recommended):**
- Q1a — Tool-call validation + recovery only
  (Recommended).
- Q2c — Both layers (non-Recommended; picked over
  provider-only Recommended). Belt-and-suspenders
  posture.
- Q3a — Fuzzy-match auto-correct above threshold
  (Recommended).

**Streak outcomes:** 2-of-3 predictions correct as the
open doc framed.
- DESIGN.md: HELD as predicted. Streak → 11.
- PRODUCT.md: HELD as predicted. Streak → 11.
- `aivyx-core/src/lib.rs`: **BROKE as predicted** (70/30
  break case fired). Additive `auto_corrected_from:
  Option<String>` field on `AuditTag::ToolCall` is the
  minimum-touch mechanism for threading auto-correction
  through to the audit emission. Streak resets 3 → 1.

Test count: 2309 → 2346 (+37). Within the predicted
`+25 to +45` band. Each task carries focused coverage
for one Phase 120 surface.

Zero new workspace deps. Zero clippy warnings.

**Honest scope correction (Task 5):** the open doc
described `0.0` as "never auto-correct" but actual
semantics are "every match clears". Conservative disable
is `1.0` (exact-match only). Pinned in
`phase_120_zero_threshold_disables_fuzzy_recovery` test
body.

After Phase 120, Phase 121 candidates (per audit + Phase
120 exit ranking): Channel Activation Milestone, per-
model prompt variants (local-LLM rehab #2), native Ollama
path (#3), release prep, new Chapter F.

## Phase 119 — Phase 118 Apply-Side Closeout + Tool-Relevance Dump (Operator-Value Polish)

**Frozen — see [PHASE_119.md](archive/phases/PHASE_119.md).** Audit-
informed phase. Closes the manual-edit gap left by
Phase 118 and the deferred `aivyx-pa tool-relevance dump`
CLI from Phase 116. Three new operator-side CLI
commands shipped together:
- `aivyx-pa profile apply-hint <id>` — applies an
  approved `ProfileHint` to `aivyx-pa.toml`'s `[profile]`
  section atomically.
- `aivyx-pa role import <id>` — adds a new
  `[roles.<name>]` section from an approved
  `RoleDefinitionSuggestion` (with parent inheritance
  honored; refuses overwrite without `--force`).
- `aivyx-pa tool-relevance dump` — renders the encrypted
  Phase 116 relevance ledger as a human-readable table
  per keyword-key.

**Q-block (all Recommended):**
- Q1a — Both apply-helpers + tool-relevance dump
  (Recommended). Three CLI commands ship in one phase.
- Q2a — Separate approve + apply commands
  (Recommended). The existing approve command stays
  category-agnostic; the new apply commands are
  independent operator gestures.
- Q3a — New `ProfileHintApplied` + `RoleDraftImported`
  audit-event variants (Recommended). Wire-compat
  serde defaults; the audit chain records the
  operator's act-on-approval gesture distinctly from
  the approval itself.

**Streak outcomes:** all three predictions correct,
second phase in a row (first 2-in-a-row 3-of-3 streak
since Phase 92).
- DESIGN.md: HELD as predicted. Streak → 10.
- PRODUCT.md: HELD as predicted. Streak → 10.
- `aivyx-core/src/lib.rs`: HELD as predicted (70/30
  hold case did NOT fire). Daemon-side audit writes
  went via `AuditWriter::append` directly, bypassing
  the lib.rs `AuditTag` bridge. Streak → 3.

Test count: 2248 → 2309 (+61). **Above** the predicted
`+25 to +45` band — honest scope reporting (Phase 6 Q5):
three distinct CLI commands plus atomic TOML primitive
plus two audit-event variants each carrying their own
thorough parser-coverage / wire-compat / validation
blocks. CLI parser bugs surface late in real use;
thorough coverage is worth the count. Same posture as
Phase 118's +60 overshoot.

Zero new workspace deps (`toml_edit` already
transitively present from Phase 58 `aivyx-pa profile
edit`). Zero clippy warnings.

After Phase 119, **every named operator-value deferral
from Phases 112-118 is closed**. The Channel Activation
Milestone becomes the highest-information-value
direction for Phase 120.

## Phase 118 — Outcome-Driven Profile/Role Refinement (Chapter E #4 — closer)

**Frozen — see [PHASE_118.md](archive/phases/PHASE_118.md).** The last
named Chapter E axis. Adds `ProfileHint` and
`RoleDefinitionSuggestion` variants to
`PersonaDeltaCategory`, extends the Phase 114 auto-
proposer pipeline (heuristic + LLM-judge + routing) to
fire for both, forces always-staged routing for the two
new categories regardless of confidence, and surfaces
approved drafts through the existing `aivyx-pa persona
proposals` CLI for operator copy into `aivyx-pa.toml`.

**Q-block (one non-Recommended):**
- Q1c — **Both Profile attributes AND new Role
  definitions** (non-Recommended; picked over the
  Profile-attribute-only Recommended). Larger surface;
  closes Chapter E #4 in one phase.
- Q2a — Always-staged for operator approval
  (Recommended). Preserves P13 (Profile-operator-owned)
  and P9 (Role-config operator-curated). Hard-coded
  routing override, not operator policy.
- Q3a — Extend Phase 114 auto-proposer with new
  categories (Recommended). Maximum substrate reuse.

**Streak outcomes:** all three predictions correct.
- DESIGN.md: HELD as predicted. Streak → 9.
- PRODUCT.md: HELD as predicted. Streak → 9.
- `aivyx-core/src/lib.rs`: HELD as predicted (60/40 hold
  case fired). No new AuditTag needed; new draft types
  fit inside existing `pub mod skill_proposer`
  boundary. Streak resets 1 → 2.

Test count: 2188 → 2248 (+60). **Above** the predicted
`+25 to +50` band — honest scope reporting (Phase 6 Q5):
Q1c's broader scope (both Profile attributes AND Role
suggestions) plus Task 5's per-category integration
(load-bearing across both runtime and aivyx-config TOML
surfaces) produced more test surface than anticipated
at sign-off. Each test pins one concrete behavior.

Zero new workspace deps. Zero clippy warnings.

**Chapter E closes with Phase 118.** All four named
axes shipped (Phases 114-118). Post-Phase-118 deferral
ledger is empty. The next phase opens against the
Channel Activation Milestone OR a new thematic Chapter F
shaped by operator pressure.

## Phase 117 — Phase 116 Deferral Closeout (live-prompt pipe + per-skill tracking)

**Frozen — see [PHASE_117.md](archive/phases/PHASE_117.md).** Standalone
phase that closes both Phase-116-internal deferrals
together per Q1b sign-off. After Phase 117, the Phase 116
relevance substrate reaches the LLM in live turns AND
records per-skill outcomes accurately — the operator value
Phase 116 aimed at lands in full.

**Q-block (one non-Recommended):**
- Q1b — **Bundle both deferrals together** (non-
  Recommended; picked over the focused single-deferral
  option). Larger scope; cleaner ledger + prompt surface
  immediately.

Phase 113's Operator-Surface Polish precedent: batching
small named deferrals into one cleanup phase. Phase 117 is
NOT a substrate-novelty phase; ships focused integration
work taking the Phase 116 substrate and plumbing it into
live use.

**Streak outcomes:**
- DESIGN.md: HELD as predicted. Streak → 8.
- PRODUCT.md: HELD as predicted. Streak → 8.
- `aivyx-core/src/lib.rs`: BROKE against predicted hold
  (70/30 risk acknowledged at sign-off; the 30% case
  fired at the new `AuditTag::SkillInvocation` variant).
  Streak resets 1 → 1.

Test count: 2174 → 2188 (+14). **Below** the predicted
+30 to +60 band — honest scope reduction in Task 2
(reusing existing `SystemPromptRefiner` trait rather
than introducing a new `DynamicSystemPromptBuilder`
trait + ConcreteAgent surface) cut test surface
significantly. The substrate end state is the same; the
simpler path got there with fewer test artifacts.
Phase 6 Q5 honesty.

Zero new workspace deps. Zero clippy warnings.

After Phase 117, the last named Chapter E axis (outcome-
driven Profile/Role refinement) is the only Chapter E
direction remaining. The post-Phase-117 deferral ledger
is empty — both Phase 116 named deferrals closed in
this phase.

## Phase 116 — Tool/Skill Selection Learning from Outcomes (Chapter E #3)

**Frozen — see [PHASE_116.md](archive/phases/PHASE_116.md).** Third phase
of Chapter E. The agent's tool/skill selection has been
pure LLM intuition since Phase 0; Phase 116 builds a
relevance ledger that tracks per-tool/per-skill success/
failure outcomes per keyword-extracted turn pattern, and
surfaces accumulated signal in the next turn's system
prompt as a passive augmentation. The LLM still picks;
just better informed.

**Q-block (all three Recommended) — conservative shape:**
- Q1a — **Keyword-set from user input** (Recommended).
  Cheap deterministic; zero LLM cost per turn. Phase 95
  precedent for cheap-deterministic-signal substrate.
- Q2a — **System-prompt section** (Recommended). Passive
  augmentation; no tool-call substrate change.
- Q3a — **Tools AND skills together** (Recommended).
  Symmetric coverage.

**Streak outcomes — two positive surprises:**
- DESIGN.md: HELD byte-identical against predicted break.
  Streak → 7. The ledger lives in aivyx-channel like every
  other learning ledger (Phase 78 / 82 / 83); none of
  those broke D, and this one didn't either.
- PRODUCT.md: HELD byte-identical against predicted break.
  Streak → 7. P8 envelope absorbed "outcome-tracked
  selection hint" cleanly without amendment.
- `aivyx-core/src/lib.rs`: BROKE as predicted at the new
  `pub mod relevance` line. Streak resets 4 → 1.

Test count: 2129 → 2174 (**+45**), inside the predicted
`+40 to +70` band. Zero new workspace deps. Zero clippy
warnings.

**Two Phase-116-internal deferrals named:**
- Live-prompt augmentation pipe. Renderer is ready;
  integration awaits a per-turn prompt-reassembly
  substrate change (the planner today builds the system
  prompt once at session-construction).
- Per-skill outcome tracking. Skills currently record as
  `skills.invoke` itself; per-skill granularity needs a
  side-channel capture path that bypasses audit
  input-hash protection.

After Phase 116, one named Chapter E axis remains
(outcome-driven Profile/Role refinement). The next
Chapter E phase opens against that axis OR closes the
Phase-116-internal deferrals — operator-pressure-shaped at
the next phase exit.

## Phase 115 — Self-Correction Loop on Failed Turns (Chapter E #2)

**Frozen — see [PHASE_115.md](archive/phases/PHASE_115.md).** Second
phase of Chapter E. Closes the symmetric negative-
feedback half of Phase 114's auto-proposer. The agent
observes failed turns (Failed / Cancelled / TimedOut /
Escalated) and proposes targeted Persona refinements to
prevent recurrence. Same pipeline, broader trigger —
Q3a sign-off (extend Phase 114 substrate).

**Q-block (one non-Recommended, two Recommended):**
- Q1c — **All non-Completed outcomes** (non-
  Recommended). Maximum-signal posture; per-failure-
  type enable flags + judge confidence threshold +
  per-category enables gate noise.
- Q2a — **Let judge pick from full surface**
  (Recommended). Same polymorphic surface as Phase 114;
  per-category enables apply uniformly to failure-
  driven proposals.
- Q3a — **Extend Phase 114 substrate** (Recommended).
  Same orchestration; new `from_failed_turns` config
  knob; new `source` field on audit-event with
  serde-skip-if-none for backward compatibility.

**Streak outcomes** — all three predictions held; fourth
all-hold result in a row (Phase 112-115). Longest
streak-hold run in project history; substrate is
genuinely mature.
- DESIGN.md: HELD as predicted (no D-section touch).
  Streak → 6.
- PRODUCT.md: HELD as predicted (P8 envelope).
  Streak → 6.
- `aivyx-core/src/lib.rs`: HELD as predicted (extensions
  inside the existing `skill_proposer` boundary).
  Streak → 4.

Test count: 2105 → 2129 (**+24**), inside the predicted
`+20` to `+35` band. Zero new workspace deps. Zero
clippy warnings.

Backward-compatible audit chain: `AuditEvent::
SkillAutoProposal` gained a `source` field with
`#[serde(default, skip_serializing_if = "Option::is_none")]`;
pre-Phase-115 entries verify byte-identically. Phase 92's
`supersedes_proposal_id` pattern.

One Phase-115-internal deferral named: operator-resolve
escalation (`/reject`) integration would require hooking
the persona-proposal-resolve site separately from the
turn-finalize hook — held unless operator pressure
surfaces.

After Phase 115, the agent self-learns AND self-corrects
across the same 11-category PersonaDelta surface. Chapter
E's remaining named axes (tool/skill selection learning;
outcome-driven Profile/Role refinement) ship operator-
pressure-shaped at the next phase exit.

## Phase 114 — Persona Auto-Proposer Generalization (Chapter E opener)

**Frozen — see [PHASE_114.md](archive/phases/PHASE_114.md).** First
phase of Chapter E. Generalizes the Phase 112 skill auto-
proposer from `LearnedSkill` to the full 11-category
`PersonaDeltaCategory` surface. After Phase 114, the
inline-at-turn-boundary auto-proposer can draft
BehavioralPreferences, LearnedContext,
CommunicationAdaptations, and the other 7 categories in
addition to skills — every Persona axis self-learns end-
to-end.

**Q-block at sign-off (one Recommended, two non-
Recommended):**
- Q1b — **Per-category TOML config block** (non-
  Recommended; operator picked over uniform-single).
  More flexible; more substrate.
- Q2a — **Reuse the four Phase 112 signals**
  (Recommended). Same heuristic gate; judge handles
  per-category specifics.
- Q3a — **Reuse existing scope bases** (Recommended).
  `skills.propose` for `LearnedSkill`; `persona.propose`
  for the other 10 categories. No new KNOWN_BASES entry;
  A3 stays at 49.

**Streak outcomes** — all three predictions held;
third all-hold result in a row (Phase 112 first, 113
second, 114 third — substrate momentum at its highest
point of the project).
- DESIGN.md: HELD as predicted (no D-section touch).
  Streak → 5.
- PRODUCT.md: HELD as predicted (P8 envelope).
  Streak → 5.
- `aivyx-core/src/lib.rs`: HELD as predicted (no new
  `pub mod`). Streak → 3.

Test count: 2068 → 2105 (**+37**), inside the predicted
`+30` to `+50` band. Zero new workspace deps. Zero
clippy warnings.

Backward-compatible audit chain: `AuditEvent::
SkillAutoProposal` gained an optional `category` field
with `#[serde(default, skip_serializing_if =
"Option::is_none")]`; existing chains verify
byte-identically. Phase 92's `supersedes_proposal_id`
precedent.

## Phase 113 — Operator-Surface Polish (deferral cleanup)

**Frozen — see [PHASE_113.md](archive/phases/PHASE_113.md).** Standalone
phase past Chapter D's close, mirroring the Phase 20
(Daemon Management + Deferral Cleanup) precedent: batch
five small named operator-surface deferrals into one
focused phase rather than carrying them across more
substrate work.

Closes the accumulated operator-surface deferrals from
Chapter D and Phase 112:
- Phase 112 TOML `[skills.auto_propose]` config loader
  in `aivyx-config`.
- Phase 112 daemon auto-construction of
  `SkillAutoProposerContext` from the loaded TOML (makes
  the auto-proposer actually enable-able via config
  rather than only via source-edit).
- Phase 112 operator-inspection flags: `aivyx-pa persona
  list --auto-only` / `--manual-only` filter the persona
  chain by `pd-auto-*` `delta_id` prefix; `aivyx-pa audit
  export --event-type SkillAutoProposal` filter
  on the existing audit-export JSONL emitter.
- Phase 110-deferred A3 amendment addendum: `KNOWN_BASES`
  inventory catches up from 43 (Phase 54 refresh) to ~47
  (add `git.read` from Phase 109 + `skills.propose` +
  `skills.list` + `skills.invoke` from Phase 110).
- Phase 112 INSTALL.md paragraph covering the
  `[skills.auto_propose]` config + revert escape hatch.

**Q-block at sign-off:** Q1a — **bundle A3 into Phase
113** (operator-picked Recommended).

**Streak outcomes** — all three predictions held; first
all-hold result since Phase 56 / Phase 108:
- DESIGN.md: HELD against predicted break. The A3
  addendum lives in `docs/amendments/`, not DESIGN.md
  proper. Streak → 4.
- PRODUCT.md: HELD as predicted (no contract touch).
  Streak → 4.
- `aivyx-core/src/lib.rs`: HELD as predicted (work
  lives in `aivyx-config`, `aivyx-channel/bin`, docs
  tree only). Streak → 2.

Test count: 2044 → 2068 (**+24**), inside the predicted
`+15` to `+25` band. Zero new workspace deps. Zero
clippy warnings.

KNOWN_BASES catch-up landed at +6 (43 → 49), not the
open doc's ~+4 estimate. Phase 113 surfaced two
post-Phase-54 entries the open doc missed:
`persona.propose` (Phase 59) and `notify.send`
(Phase 62). The A3 addendum traces all six.

After Phase 113, every named Chapter D + Phase 112
operator-surface deferral is closed; the Channel
Activation Milestone is fully ready to run.

## Phase 112 — Skill Auto-Proposer (Phase 110's named follow-on)

**Frozen — see [PHASE_112.md](archive/phases/PHASE_112.md).** Shipped the
agent-side auto-proposer that closes the last named Chapter
D follow-on (Phase 110's deferral) and the project-vision
critical-path piece. After Phase 112, the agent self-learns
at the skill layer end-to-end: complex turns fire the
auto-proposer in a detached background task; high-confidence
verdicts auto-accept into the LearnedSkill chain; below-
threshold verdicts stage for operator review through the
existing `aivyx-pa persona proposals approve` surface.

**Streak outcomes** — 2 of 3 streaks held (1 positive
surprise), 1 broke as predicted:
- DESIGN.md: HELD (predicted to break). The substrate
  slotted into the existing D4 surface without a new
  section. Streak → 3.
- PRODUCT.md: HELD as predicted. P8 covers inline-fired
  reflection identically to cron-fired. Streak → 3.
- `aivyx-core/src/lib.rs`: BROKE as predicted with the new
  `pub mod skill_proposer`. Streak resets to 1.

Test count: 1950 → 2044 (**+94**, way past the predicted
`+30` to `+50` band). Zero new workspace deps; the LLM
substrate (Phase 25 / Phase 91), tokio, audit log, and
persona chain were all already vendored. Zero clippy
warnings.

**Two Phase-112-internal deferrals** — small operator-
surface pieces shipped as a focused follow-on:
- TOML `[skills.auto_propose]` config-section loader in
  `aivyx-config` (the struct exists in `aivyx-channel`
  with `Default` impl; promotion follows the Phase 91
  `RecallJudgmentConfig` precedent).
- `aivyx-pa persona list --auto-only` / `--manual-only` and
  `aivyx-pa audit export --event-type` filter flags. Data is
  already audit-logged; these are operator-convenience.

**The Channel Activation Milestone is still unblocked**
(Phase 111 closed the adapter-wiring carve-outs) and the
self-learning loop is now closed end-to-end at the
substrate level. The operator can run the milestone +
exercise the auto-proposer in the same session whenever
the verification window opens.

 Standalone
phase past Chapter D's close (matching the Phase 111
precedent for closing Chapter-D-internal deferrals as
follow-on phases). Closes the last named Chapter D
deferral: Phase 110's *"agent-side auto-proposer heuristic
(fire reflection-cron-style after complex turns, draft
skill proposals automatically)."*

After Phase 112, the agent self-learns at the skill layer:
complex turns trigger an LLM-judged proposal, high-
confidence proposals auto-accept into the LearnedSkill
chain, and the next turn's system prompt already carries
the new skill. The propose-approve-render-invoke substrate
shipped at Phase 110 becomes a propose-judge-accept-
render-invoke loop where "judge" replaces the operator's
manual approval for the steady-state high-confidence
case.

**Q-block (three of four non-Recommended; operator picked
the more autonomous shape):**
- Q1b — heuristic + LLM-judge (Recommended).
- Q2b — **inline at turn boundary** (non-Recommended;
  picked over cron-fired). Background-task spawned
  post-`finalize` keeps critical-path latency at zero.
- Q3b — **threshold-gated auto-accept** (non-Recommended;
  picked over always-staged). Operator-configurable TOML
  threshold defaulting `0.85`.
- Q4b — **title fuzzy-match + LLM semantic check** (non-
  Recommended; picked over title-only). LLM check
  piggybacks on the Q1b judge call.

This is the **project-vision critical path** — the
operator's stated vision is "Self-Learning, Self-Improving
AI Personal Assistant," and Phase 112 turns the Phase 110
operator-driven framework into actual self-learning. All
five adapters inherit the auto-proposer for free (it
hooks the turn-finalize event, not any specific adapter).
Zero new workspace deps; LLM substrate already in place
(Phase 25 multi-provider, Phase 87 phrasing, Phase 91
judgment, Phase 92 supersession).

DESIGN.md streak predicted to break (D4 skills section
extended); PRODUCT.md predicted to extend to three (P8
envelope); `aivyx-core/src/lib.rs` predicted to break
(new `skill_proposer` module).

## Phase 110 — Skills Auto-Creation (Reflection Staging) (Chapter D)

**Frozen — see [PHASE_110.md](archive/phases/PHASE_110.md).** The sixth and
final Chapter D item — the substrate-design-heavy piece the
ROADMAP flagged with "highest amendment risk." Extends the
existing reflection layer (Phase 29 propose / apply, Phase
30 role-mutation, Phase 59 Persona propose, Phase 60 Persona
revert, Phase 70 proposal review surface) with a **skills
primitive**: procedural patterns the agent drafts after
complex turns, staged as Persona-style deltas the operator
approves via the existing persona-proposal surface, then
rendered into the agent's system prompt alongside Persona
content on every subsequent turn. Mid-ground between
Hermes's autonomous skill creation and Aivyx PA's current
per-action reflection propose / apply; stays inside P8's
"outcome-driven audited reflection" envelope. Q1a chose
**11th PersonaDeltaCategory variant `LearnedSkill`** reusing
the entire Persona substrate. Q2b chose **new
`skills.propose` scope base** for per-category granularity
(operator pick over the Q2a "extend persona.propose"
recommendation). Q3c chose **both render-in-prompt AND
callable `skills.list` / `skills.invoke` tool surface**
(operator pick over the Q3a "render-only" recommendation).
Q4a kept **foundation scope** — propose + approve + render
+ tool-surface; agent-side auto-proposer heuristic
deferred. DESIGN.md break predicted at the new scope base;
`aivyx-core/src/lib.rs` break predicted at the two new
tool re-exports; PRODUCT.md predicted to hold (P8 envelope
unchanged). All three streaks already reset at Phase
109's triple-break, so Phase 110 lands its predicted breaks
without compounding break-on-break framing.

## Phase 109 — Tool Breadth + Amendment A12 (Chapter D)

**Frozen — see [PHASE_109.md](archive/phases/PHASE_109.md).** The fifth
Chapter D item — Hermes-comparison tool-breadth gap closure.
Three new substrate tools across A12 + one Phase-100 audit
closure: `git.status` and `git.diff` sharing a new
`git.read` capability scope (qualified by repo path; shells
out to system `git` binary; no Rust deps), plus `net.dns`
closing one of Phase 100's eight declared-but-toolless
scopes (`tokio::net::lookup_host`; uses the existing
`net.dns` scope base from Phase 0; no amendment needed for
the scope itself). A12 takes P10's substrate tool count
from 10 → 13. Q3a chose **mirror A11 exactly** — short
amendment file structurally identical to A11 (which itself
mirrored A5 from Phase 37). DESIGN.md and PRODUCT.md
streaks deliberately break at A12 (same pattern as the
A11 / A5 amendment commits); `aivyx-core/src/lib.rs`
streak extends (tools land in `aivyx-channel`, not core).
Zero new workspace deps. The other seven Phase-100-audited
toolless scopes (`shell.spawn`, `audit.read`,
`config.read`, `config.write`, etc.) stay deferred per
Phase 100's audit conclusions.

## Phase 108 — Slack Channel Adapter (`aivyx-slack`) (Chapter D)

**Frozen — see [PHASE_108.md](archive/phases/PHASE_108.md).** The fourth
Chapter D item and the four-data-point confirmation for the
adapter pattern Phase 9 wrote down (then Phase 107 promoted
to confirmed-at-three). Adds a new workspace crate
(`aivyx-slack`) at foundation scope — DMs + channel
messages only; threads, Block Kit, attachments, slash
commands all stay named deferrals. The headline outcome: an
operator with a Slack workspace runs `aivyx-pa --channel slack`
with a bot token + an app-level Socket Mode token, talks to
the bot from any DM or invited channel, and gets the same
agent experience they already get on Discord and Telegram.
Q1a chose **slack-morphism** as the SDK (thin protocol
wrapper, matches Phase 107's twilight-rs decision). Q2a
chose **Socket Mode only** (WebSocket initiated outbound
from aivyx-pa, no public endpoint — matches Discord's Gateway
shape and the substrate's local-daemon posture). Q3a chose
`format!("{team_id}:{channel_id}")` as the partition key —
**confirms three-data-point `Option<String>` partition
shape at four data points**, explicitly punts the Phase 9
Q7 richer-type question to a future Matrix-shaped adapter
where `room_id + homeserver` makes the structured type
genuinely necessary. Q4a chose **foundation scope** over
full Phase-107-style parity — Slack's protocol surface is
simpler than Discord's so foundation is genuinely the right
call here. One streak break predicted: zero-new-deps
(slack-morphism adoption); two surprises expected to repeat
the Phase 107 pattern (DESIGN.md held byte-identical via
A4-addendum-in-amendment-file pattern; `aivyx-core/src/lib.rs`
held byte-identical because `ChannelPlatform::Slack` was
already forward-enumerated in Phase 8). Daemon-frontend
variant carved out as Phase-108-internal deferral that
bundles with the Phase 107 daemon-frontend follow-on.

## Phase 107 — Discord Channel Adapter (`aivyx-discord`) (Chapter D)

**Frozen — see [PHASE_107.md](archive/phases/PHASE_107.md).** The third
Chapter D item and the first to genuinely grow the substrate.
Phases 105 and 106 were lower-risk reader / docs work; Phase
107 added a new workspace crate (`aivyx-discord`) at full
parity with the Phase 8/9 `aivyx-telegram` adapter at the
in-process layer — a `ChannelContext` impl, a private
`DiscordTransport` trait + scripted double, a
`run_discord_session` sibling of `run_session` /
`run_telegram_session`, and binary wiring through
`ChannelKind::Discord`. twilight-rs 0.16 (Q1a — thin
protocol wrapper matching the Phase 8 frankenstein-vs-
teloxide decision; three new direct workspace deps:
`twilight-gateway`, `twilight-http`, `twilight-model`).
Full-parity scope (Q2c). Text commands for `/approve` /
`/reject` matching Telegram (Q3a). Two streak-prediction
surprises in the operator's favor: **`aivyx-core/src/lib.rs`
held byte-identical** (Phase 8 had already
forward-enumerated `ChannelPlatform::Discord` alongside
Slack / Matrix / Email / Rest; no variant-lift needed) and
**DESIGN.md held byte-identical** (A4 addendum landed in
the amendment file per the Phase 49 precedent). Streaks at
exit: DESIGN.md → 54, PRODUCT.md → 7, `aivyx-core/src/lib.rs`
→ 7. Workspace tests `+29` → 1888 (below the predicted
`+50`–`+100` band — Discord's push-based Gateway simplified
the substrate the scripted suite covers; the Telegram
precedent's larger test count is substrate-discovery work
Phase 107 didn't need to redo). Two Phase-107-internal
deferrals named honestly at exit: **Discord daemon-frontend**
(`FrontendType::Discord` + `discord_daemon_frontend.rs`
mirroring Phase 19's Telegram-over-daemon path), and
**`/approve` / `/reject` gate-resolve routing** (which lives
in the daemon-frontend half of the Telegram precedent). The
in-process Discord adapter is end-to-end functional today;
daemon-mode is a deployment optimization that lands as a
focused follow-on commit. A4 amendment addendum filed
(12 → 13 crates). Real-protocol smoke test deferred to the
Channel Activation Milestone per the
`docs/ADAPTER_PATTERN.md` checklist.

## Phase 106 — MCP Server Breadth (Curated Recipes) (Chapter D)

**Frozen — see [PHASE_106.md](archive/phases/PHASE_106.md).** The second
Chapter D item. Phase 46 shipped the first bundled MCP server
(`aivyx-pa mcp-server web-search`); Phase 24 / 32 shipped the
external `[[mcp_server]]` TOML surface for plugging in any
MCP-compatible binary; Phase 55 shipped the sandbox layer
that wraps each spawn. What was missing was the **catalog** —
an operator's discovery story for "which MCP servers should I
actually enable, and what do their blocks look like with the
right sandbox config?" Phase 106 shipped that catalog as
`docs/MCP_RECIPES.md` (12 worked recipes: filesystem, github,
gitlab, sqlite, postgres, time, fetch, brave-search, slack,
memory, puppeteer, everything) plus a new `aivyx-pa mcp recipes
[<name>]` CLI subcommand for in-shell discovery. Q1a chose
recipes-only scope (no new bundled-server code paths); Q2a
chose the doc + CLI surface (mirrors Phase 103's
`aivyx-pa tool init`); Q3a chose inline `[mcp_server.sandbox]`
per recipe so a copy-paste produces a sandboxed config out of
the gate (Phase 55 substrate-default posture). All three
streak predictions held: DESIGN.md → 53, PRODUCT.md → 6,
`aivyx-core/src/lib.rs` → 6. Zero new workspace deps;
workspace tests `+19` → 1862 (above the predicted `+8`–`+12`
band by seven; over-shoot called out in the
prediction-vs-reality section — pinning every recipe's
`[[mcp_server]]` + `[mcp_server.sandbox]` blocks as twin
shape tests names the Q3a sandbox-by-default contract more
clearly than a single combined test would). New
bundled-server code stays a Chapter D deferral pending
operator pressure.

## Phase 105 — Trajectory Logging (`aivyx-pa audit export`) (Chapter D opener)

**Frozen — see [PHASE_105.md](archive/phases/PHASE_105.md).** Chapter D's
opener and the easiest-wins-first item of the Hermes-
comparison-driven arc. A read-only offline subcommand that
emits the HMAC audit chain as JSONL on stdout. Each line is
the full `SignedEntry` projection (`seq +
appended_at_ms + prev_mac + mac + event` — Q1a), enough that
downstream tooling can re-verify the HMAC against a
separately-supplied genesis seed. Filters in v1 are
sequence-based only (`--from <seq>` + `--limit <N>` — Q2a)
mapping directly to `PersistentAuditLog::entries_range`,
which Phase 47 already shipped for the Web UI paginated
viewer. Source path is offline-only via cold-start storage
open (Q3a) — same code path as `aivyx-pa --verify-only`,
requires the passphrase, no new IPC variant. All three
streak predictions held: DESIGN.md → 52, PRODUCT.md → 5,
`aivyx-core/src/lib.rs` → 5. Zero new workspace deps;
workspace tests `+18` → 1843 (above the predicted `+6`–`+12`
band by six; over-shoot in parse-test coverage of error
paths + per-helper coverage in `audit_export.rs`). New
`docs/AUDIT_EXPORT.md` reference doc carries the JSONL shape,
worked `jq` examples, and the re-verify-downstream procedure.
Three named deferrals at exit: time-range filters
(`--since` / `--until`), correlation filters
(`--session` / `--mission`), and daemon-mode export over
the Phase 47 `Query` envelope.

## Phase 104 — `aivyx-pa init` Polish (Chapter C opener)

**Frozen — see [PHASE_104.md](archive/phases/PHASE_104.md).** Chapter C's
opener. After 60-plus phases of substrate work, the init
wizard's first-touch UX had accumulated three concrete
paper-cuts: stale Anthropic/OpenAI default model strings
(`claude-sonnet-4-20250514` was a year out of date; `gpt-4o`
was no longer the current flagship), no-models guidance on
the Ollama path that said `Run "ollama pull <model>" first.`
with no concrete recommendation, and a write-then-find-out-
later validation gap where a typo'd key or non-existent
model only surfaced on first turn (after the operator had
already committed to a passphrase and opened the daemon).
Phase 104 closed all three: refreshed the defaults to
current generation (`claude-sonnet-4-6` / `gpt-4.1` per Q3a),
added a single hardcoded `Try: ollama pull llama3.2:3b`
suggestion on the empty-models path per Q4a, and added a
`verify-before-write` step that hits `GET /v1/models` with
the supplied key, confirms the chosen model is in the
returned list, and re-prompts on failure with a three-retry
cap and a `Write anyway?` escape hatch (Q1a/Q2a). Additive
on the operator-facing surface; new `aivyx-llm::verify`
module gated on existing provider features. All three
streak predictions held: DESIGN.md → 51 (one past the
Phase 103 half-hundred milestone), PRODUCT.md → 4,
`aivyx-core/src/lib.rs` → 4. Zero new workspace deps;
workspace tests `+17` → 1825 (above the predicted `+8`–`+12`
band; over-shoot in `verify.rs`'s exhaustive parse+classify
coverage for the auth-vs-model-not-found classifier the
wizard's retry loop branches on).

## Phase 100 — Tool-Surface Gap Closure (Chapter B opener)

**Frozen — see [PHASE_100.md](archive/phases/PHASE_100.md).** Chapter B's
opener. `fs.delete` and `fs.metadata` are D4-original
substrate scope bases (Phase 0, in Amendment A3's inventory)
that have never had a first-party tool — the agent can read
and write a file but cannot delete or stat one. Closing the
gap is gated by P10: Amendment A5 locked the substrate tool
list at exactly eight, so Phase 100 is an **amendment
phase** — it files Amendment A11 extending P10 to ten tools
(the A5 pattern that took the count seven → eight when Phase
37 added `web.post`), then ships `fs.delete` and
`fs.metadata` behind it. Directory listing folds into
`fs.metadata` or earns a new `fs.list` scope (Q1). The other
eight declared-but-toolless scopes (`shell.spawn`,
`net.dns`, `audit.read`, `config.read`, `config.write`,
`display.window_close`, `memory.gc`, `mission.gate`) are
audited and each ruled "tool later" or "deliberately
reserved." Streaks at exit: PRODUCT.md broke at 39
(Amendment A11) and `aivyx-core/src/lib.rs` broke at 47
(new tool re-exports); DESIGN.md held → 47 (Q1 kept
directory listing inside `fs.metadata`). Workspace tests
`+32` → 1778.

## Phase 101 — Tool-Call Input Validation & Repair (Chapter B)

**Frozen — see [PHASE_101.md](archive/phases/PHASE_101.md).** Chapter B's
reliability item. Two `dev-verify` runs (Phases 99, 100)
caught local models emitting tool calls with malformed
arguments. The planner already loops the model to retry an
**unknown tool name**; a *known* tool called with bad input
is dispatched blind and fails ad-hoc inside the tool. Phase
101 makes the two halves symmetric: a **validate-before-
dispatch** step checks a known call's input against the
tool's `input_schema()` (the JSON Schema every `Tool`
already exposes), and on a mismatch appends a structured
`invalid_input` result echoing the expected schema, looping
the model to **repair** the call. A two-repair cap then
dispatches as-is (the tool's own `execute` validation is the
floor). Validation uses the `jsonschema` crate — the first
net-new workspace dependency since Phase 27, a deliberate
Q2 choice of spec-correct validation over a hand-rolled
checker. Additive: a well-formed call leaves the planner
byte-identical to pre-Phase-101. Streaks at exit: all three
held — DESIGN.md → 48, PRODUCT.md and `aivyx-core/src/lib.rs`
each re-establish to 1 after their Phase 100 breaks.
Workspace tests `+9` → 1787.

## Phase 102 — Tool Observability (`aivyx-pa tools`) (Chapter B)

**Frozen — see [PHASE_102.md](archive/phases/PHASE_102.md).** Chapter B's
observability item. Phase 100 widened the tool surface and
Phase 101 made tool calls more reliable; neither gave the
operator a way to *see* the tool layer. Phase 102 adds
`aivyx-pa tools` — a read-only subcommand, sibling of `aivyx-pa
memory` / `aivyx-pa learning`, that lists every registered
tool (name, description, capability base) and annotates
each with audit-derived call statistics: total calls, the
outcome breakdown (completed / failed / denied / …), and
timing, with a `--window <secs>` filter. The data already
exists — every tool call is an `AuditEvent::ToolCall` in
the chain, keyed by the stable `scope_used.base()`. A new
daemon IPC `GetToolStats` query carries it; the variant is
additive and backward-compatible under the Phase 41
protocol handshake. All `aivyx-channel`; `aivyx-core`
untouched. Streaks at exit: all three held — DESIGN.md → 49,
PRODUCT.md → 2, `aivyx-core/src/lib.rs` → 2. Zero new deps;
workspace tests `+12` → 1799.

## Phase 103 — External Tool Ergonomics (`aivyx-pa tool init`) (Chapter B)

**Frozen — see [PHASE_103.md](archive/phases/PHASE_103.md).** Chapter B's
closing item. Three Chapter B phases shipped the *operator's*
tool experience (Phase 100 surface, Phase 101 reliability,
Phase 102 observability); Phase 103 closes the chapter on the
*third-party tool author's* experience. `aivyx-pa tool init
<path>` writes a runnable Rust tool-process project: a
`Cargo.toml` depending on the existing `aivyx-tool` crate
(which already re-exports the wire types and framing — no new
SDK helper to add), a `src/main.rs` with the handshake +
invocation main loop and a handler stub the author replaces,
a README, a conformance test, and a `[[tool_process]]`
snippet to paste into `aivyx-pa.toml`. Rust over Python at
operator choice (Q2): the existing `examples/python-tool/`
already covers stdlib-Python, and the missing scaffold is for
authors who want the `wire.rs` enums' type-safety and the
`cargo` toolchain. Additive — a new CLI subcommand, no
daemon IPC, no existing path altered. Streaks at exit: all
three held — DESIGN.md → 50 (project milestone, fifty
consecutive phases), PRODUCT.md → 3, `aivyx-core/src/lib.rs`
→ 3. Zero new deps; workspace tests `+9` → 1808.

**Chapter B closes here.** Phase 100 widened the tool surface
(`fs.delete`, `fs.metadata`, A11 amending P10 to ten tools);
Phase 101 added planner validate-before-dispatch + repair so
malformed tool calls round-trip back to the model for fixing;
Phase 102 added `aivyx-pa tools`, the read-only observability
view; Phase 103 added `aivyx-pa tool init`, the scaffolder for
third-party tool authors. All four expected items shipped;
no further pre-named Chapter B phases remain.

## Chapter A — Foundation Closeout (Phases 50–54) [COMPLETE]

After Phase 49 closed the PRODUCT.md forward-commitment ledger,
the project pivoted from "deliver remaining commitments" to
"close out Phase 0–49 loose ends." Chapter A was the
finish-the-job arc: pay down the deferral backlog, close the
P12 deferred half, harden the tool-process surface with
container isolation, and exit with a documentation sweep.

| Phase | Status | Headline |
|---|---|---|
| 50 | ✓ shipped | P12 closeout — `run_tool_as_subprocess<T: Tool>` + `p12_equivalence.rs` |
| 51 | ✓ shipped | Cleanup — typed `AivyxError`, `ConnectionContext`, TOML passphrase |
| 52 | ✓ shipped | Sandbox layer — generic command wrapper for `[[tool_process]]` |
| 53 | skipped | Audit log rotation — no pressure to date |
| 54 | ✓ shipped | Final documentation sweep |

**Chapter A retrospective.** The arc ended with the project at
*every PRODUCT.md commitment honored, every visible loose end
closed, every doc in sync with the substrate*. Five phases held
the discipline of small scopes + Q-block sign-off + honest
streak predictions. The deliberate break (lib.rs streak at
Phase 51 Q1) was the right call: D6's `AivyxError` shape had
been wrong for 50 phases; honoring it was overdue. No
regression-induced rollbacks; no contract amendments that
required reshaping shipped surface. The substrate held. Future
work is operator-feedback-driven and amendment-driven rather
than speculative — the next numbered phase opens in response
to a specific need.

## Phase 54 — Final Documentation Sweep (Chapter A Closer) [SHIPPED]

**Frozen — see [PHASE_54.md](archive/phases/PHASE_54.md).** Closed Chapter A
with a docs catch-up. Root README rewritten (179 lines, current
state + five-minute setup). PRODUCT_ROADMAP Delivered section
refreshed with all 12 PRODUCT.md commitments grouped by category
+ Chapter A entries. DAEMON_IPC Phase 47 Query/QueryResponse
addendum. A3 amendment addendum brings scope-base count from
"24" to the current 43 with a full enumeration grouped by tier.
Cross-doc consistency spot-check found no drift. DESIGN.md
streak broke at 4 (A3 addendum, predicted); PRODUCT.md and
lib.rs streaks held at 4 and 2. No code changes.

## Phase 53 — (skipped)

Audit log rotation/compaction was scheduled here. Skipped at
Phase 52 exit: audit chain growth is bounded by tool-call
frequency × uptime; even at 10k turns/year the chain stays well
under 1M rows. If chain-size pressure surfaces during real
operation, a future cleanup phase will absorb it.

## Phase 52 — Tool Process Sandbox Layer [SHIPPED]

**Frozen — see [PHASE_52.md](archive/phases/PHASE_52.md).** Added a generic
command-wrapper sandbox layer to `[[tool_process]]`.
`[tool_process.sandbox] { wrapper, args }` is prepended to the
spawn — `wrapper wrapper_args... command command_args...`.
Aivyx PA supplies the policy slot; the operator supplies the
policy (bubblewrap, firejail, docker, sandbox-exec). Closes
the Phase 49 sandboxing deferral and narrows THREAT_MODEL.md
§5.6. New `aivyx-tool::SandboxConfig` + matching `aivyx-config`
type + binary wiring. Integration test uses POSIX `env` as a
universal no-op wrapper so the suite runs anywhere `cargo test`
runs. `docs/TOOL_SDK.md` §9 documents three worked examples
(bwrap / firejail / docker). 984 tests, zero clippy. All three
streak predictions correct (DESIGN.md → 4, PRODUCT.md → 3,
lib.rs → 1).

## Phase 51 — Cleanup: Error Typing + ConnectionContext + Passphrase Path [SHIPPED]

**Frozen — see [PHASE_51.md](archive/phases/PHASE_51.md).** Mechanical
Chapter A cleanup phase. Closed three independent items:

1. **`AivyxError::{Storage,Crypto}` typed nested errors.** The
   two TODOs in `aivyx-core/lib.rs:689,693` that have sat in
   production code since Phase 1. D6 prescribed `#[from]
   StorageError` / `CryptoError`; Phase 51 finally wires them.
   `StorageError` and `CryptoError` gained `Clone` derives;
   `aivyx-core` gained intra-workspace deps on both.
2. **`handle_connection` → `ConnectionContext` lift.** Removes
   the Phase 47 Task 4 `#[allow(clippy::too_many_arguments)]`
   shortcut; same shape as Phase 41 `DaemonConfig`.
3. **`AIVYX_PA_PASSPHRASE` TOML/env inconsistency.** New
   `PassphraseSource::FromConfig(SecretString)` makes the
   `[aivyx_pa] passphrase` TOML field actually drive Argon2id
   derivation. Phase 47 visual-pass footgun closed.

979 Rust tests (+6), zero clippy. lib.rs streak broke at 6
(deliberate, Q1 honored D6 after 50 phases of stub). DESIGN.md
streak → 3, PRODUCT.md streak → 2.

## Phase 50 — P12 Closeout: First-Party In-Process Protocol Unification [SHIPPED]

**Frozen — see [PHASE_50.md](archive/phases/PHASE_50.md).** Wired the two Phase
49 deferred bridge stubs: `ToolEvent` frames now relay onto the
channel (`Status`/`OutputChunk` → `StreamEvent`), and
cancellation is targeted via `CancelInvocation { call_id }`
using a caller-supplied id. Added `run_tool_as_subprocess<T:
Tool>` harness in `aivyx-tool::harness` — a generic function
that wraps any `aivyx_core::Tool` impl as a tool-process binary
with a synthesized `ToolContext` (channel relays events back as
`ToolEvent` frames; audit is null per the one-row-per-call
invariant). The canonical proof of P12's "extractable without
rewriting" clause is `tests/p12_equivalence.rs`, which drives
the same `FsReadTool` invocation through in-process `execute()`
and through the harness-wrapped subprocess via
`ToolProcessBridge` + `ToolProxy`, then asserts byte-identical
`ToolOutcome::Completed`. PRODUCT.md P12 moved to **Fully
Delivered**. 973 tests, zero clippy. lib.rs streak held at 6;
DESIGN.md held at 2; PRODUCT.md broke at delivery-status
refresh (honest break, predicted).

## Phase 49 — Tool Process IPC Foundation (P12) [SHIPPED]

**Frozen — see [PHASE_49.md](archive/phases/PHASE_49.md).** Delivered the
last `PRODUCT.md` forward commitment. New 12th workspace crate
`aivyx-tool` shipping `ToolProcessBridge` (spawns + handshakes a
child over length-prefixed JSON on stdin/stdout, `kill_on_drop`
safety net), `ToolProxy` (implements `aivyx_core::Tool` by
delegating to the bridge), `[[tool_process]]` TOML config with
operator scope-narrowing overrides, `docs/TOOL_SDK.md` as the
v0 third-party contract, and `examples/python-tool/` — a stdlib-
only Python wordcount reference with a 9-test conformance suite
driving the real subprocess. Foundation phase: first-party
in-process protocol unification deferred to a future phase per
Q5. **All twelve PRODUCT.md commitments now shipped — the
forward-commitment ledger is closed.** DESIGN.md A4 addendum
filed (11 → 12 crates); PRODUCT.md Delivery Status refreshed
(both streak predictions broken deliberately, lib.rs streak
held at 4). 966 Rust tests + 24 Python conformance tests, zero
clippy.
