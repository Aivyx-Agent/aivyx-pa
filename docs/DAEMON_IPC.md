## Aivyx PA Daemon IPC Protocol — Phase 16

A cross-phase reference document specifying the IPC protocol shape
that the daemon (Phase 16+) and all frontends communicate over.
Settled during Phase 16 (Daemon Migration phase 1 of N). Future
daemon phases inherit these decisions as givens; changes require a
protocol-version bump and a documented migration path.

For the product commitment this protocol delivers against, see
[`../PRODUCT.md` P4](../PRODUCT.md) (Daemon-Default Architecture).
For the phase journal, see [`PHASE_16.md`](archive/phases/PHASE_16.md).

---

### Transport

**Unix domain socket** at a well-known path:

```text
$XDG_RUNTIME_DIR/aivyx-pa/daemon.sock      (preferred)
$HOME/.local/share/aivyx-pa/daemon.sock     (fallback when XDG_RUNTIME_DIR is unset)
```

The daemon creates the socket file with mode `0600`, owned by the
daemon's effective UID. Per P4.4, any process that can read the
socket file is by definition the operator. The daemon verifies
peer identity via `SO_PEERCRED` on Linux (UID match); other
platforms are deferred to their respective porting phases.

The daemon removes (unlinks) any stale socket file at startup
before binding. A frontend that finds no socket file (or gets
`ECONNREFUSED`) knows no daemon is running.

---

### Wire format

**Length-prefixed JSON frames.** Each frame is:

```text
┌──────────────────┬──────────────────────────────────────┐
│ 4 bytes          │ N bytes                              │
│ big-endian u32   │ UTF-8 JSON payload                   │
│ (payload length) │                                      │
└──────────────────┴──────────────────────────────────────┘
```

- **Max payload size:** 16 MiB (16,777,216 bytes). A frame whose
  length prefix exceeds this limit is a protocol error; the
  receiver closes the connection.
- **No trailer, no compression, no TLS.** The socket is local-only
  and OS-permission-protected; encryption is unnecessary. Compression
  is unnecessary at LLM-token-rate throughput.
- **`serde_json`** is the serialization library (already in the
  workspace). Zero new dependencies.

The choice of JSON over a binary format is deliberate for Phase 16:
the PoC's primary value is debuggability (inspect frames with a hex
dump or `socat`), not throughput. A future phase may upgrade to a
binary format by bumping the protocol version; the transport and
framing decisions survive that swap.

---

### Message types

Three top-level message envelopes flow over the socket. Each is a
JSON object with a `"type"` discriminator field.

#### `FrontendMessage` (frontend → daemon)

| Variant          | Payload fields                  | Semantics                                                |
|------------------|---------------------------------|----------------------------------------------------------|
| `StartSession`   | `role: Option<String>`          | Request a new session under the named role (or default).  |
| `SubmitInput`    | `session_id: String`, `text: String`, `mission_id: Option<String>` | Send one user input line. When `mission_id` is set and the turn escalates, the daemon creates a gate on that mission and emits `ApprovalGate`. |
| `CancelTurn`     | `session_id: String`            | Request cancellation of the in-flight turn.              |
| `ResolveGate`    | `mission_id: String`, `gate_id: String`, `approved: bool` | Operator resolves a pending mission approval gate. |
| `Disconnect`     | *(none)*                        | Graceful frontend disconnect. Daemon may keep the session alive. |

#### `DaemonMessage` (daemon → frontend)

| Variant           | Payload fields                        | Semantics                                                    |
|-------------------|---------------------------------------|--------------------------------------------------------------|
| `SessionStarted`  | `session_id: String`                  | Acknowledges `StartSession`; the session is ready for input. |
| `StreamEvent`     | `session_id: String`, `event: StreamEventPayload` | One streamed event from the turn loop.          |
| `TurnComplete`    | `session_id: String`, `outcome: String` | Terminal frame for a turn. `outcome` is human-readable, and is **authoritative** — the turn loop's own post-processing (a final-message floor, Candor's claim-check, an identifier-fidelity check) only ever touches `outcome`'s underlying `final_message`, never the raw `StreamEvent::Text` chunks a frontend may have already displayed. An adapter that renders the streamed text directly (POLISH_WAVES.md sub-project 4's own finding — 6 first-party surfaces did exactly this) can silently show a *pre-correction* answer. Reconcile the two with `turn_outcome_correction`/`concat_text_events` in `aivyx-ipc`'s `protocol.rs` — every first-party frontend now does. |
| `Error`           | `code: String`, `message: String`     | Protocol-level or session-level error.                       |
| `MissionCreated`  | `mission_id: String`                  | Acknowledges mission creation.                               |
| `MissionStateChanged` | `mission_id: String`, `state: String` | Mission transitioned to a new state.                      |
| `GateResolved`    | `mission_id: String`, `gate_id: String`, `approved: bool` | Gate resolution confirmed.                    |

#### `DaemonLifecycleEvent` (daemon → frontend, separate from `DaemonMessage`)

| Variant           | Payload fields                  | Semantics                                             |
|-------------------|---------------------------------|-------------------------------------------------------|
| `DaemonReady`     | `version: String`               | Sent once after the frontend connects.                |
| `ShuttingDown`    | `reason: String`                | Daemon is shutting down; frontend should disconnect.  |

Per Phase 16 Q4 resolution (a): lifecycle events are a **separate
message type** from `DaemonMessage`. The frontend's IPC receive
loop demuxes on the `"type"` discriminator into three categories
(`FrontendMessage`, `DaemonMessage`, `DaemonLifecycleEvent`). This
keeps `StreamEvent` in `aivyx-core/src/lib.rs` untouched and
preserves the production-core streak.

---

### `StreamEventPayload`

The `StreamEventPayload` carried inside `DaemonMessage::StreamEvent`
is a JSON-serializable mirror of `aivyx_core::StreamEvent<'a>`. The
core enum uses borrowed references (`&str`, `&[u8]`) and is not
`Serialize`; the IPC layer defines an owned, serializable counterpart
that converts to/from the core type at the process boundary.

| Variant              | Fields                                                 |
|----------------------|--------------------------------------------------------|
| `Text`               | `text: String`                                         |
| `Status`             | `status: String`                                       |
| `ToolCallStarted`    | `tool_id: String`, `tool_name: String`, `input: Value` |
| `ToolCallFinished`   | `tool_id: String`, `tool_name: String`, `outcome_summary: String` |
| `ToolOutput`         | `tool_id: String`, `tool_name: String`, `chunk: String` |
| `ApprovalGate`       | `mission_id: String`, `gate_id: String`, `reason: String`, `scope: Option<String>` |
| `ModelRouted`        | `model: String`, `task: String`, `reason: String` — daemon-only; see the routing visibility addendum |
| `CloudConsentRequested` | `model: String`, `endpoint: String`, `why: String`, `estimated_tokens: u32`, `can_allow_here: bool` — daemon-only; see the routing visibility addendum |

`Attachment` is excluded from the Phase 16 PoC. Binary payloads
over JSON require base64 encoding; the complexity is deferred to a
phase that actually exercises attachments over IPC.

---

### Error model

Every `FrontendMessage` that expects a response gets exactly one
terminal frame (`SessionStarted`, `TurnComplete`, or `Error`) plus
zero or more intermediate `StreamEvent` frames between
`SubmitInput` and `TurnComplete`. A frontend that receives `Error`
in response to `StartSession` knows the session was not created.

Error codes are short string tags, not numeric. Phase 16 defines:

- `"invalid_message"` — the daemon could not parse the frame.
- `"unknown_session"` — `session_id` does not match a live session.
- `"no_mission_store"` — `ResolveGate` received but no mission store configured.
- `"gate_create_failed"` — escalation→gate creation failed (missing mission, wrong state).
- `"gate_resolve_failed"` — gate resolution failed (missing mission/gate, wrong state).
- `"internal"` — catch-all for unexpected daemon-side failures.

### Escalation→gate turn-loop wiring (Phase 23)

When a `SubmitInput` carries a `mission_id` and the agent's turn returns
`TurnOutcome::Escalated`, the daemon:

1. Loads the mission from redb.
2. Creates a `GateRecord` via `mission::add_gate`, transitioning the
   mission to `GatePending`.
3. Emits `StreamEventPayload::ApprovalGate` to the frontend.
4. Sends `TurnComplete` with outcome `"escalated: <reason>"`.

When the frontend sends `ResolveGate` with `approved: true`, the daemon:

1. Resolves the gate, transitioning the mission back to `Running`.
2. Sends `GateResolved`.
3. Starts a new turn with the approval context as input, streaming
   events and ending with a second `TurnComplete`.

When rejected, the mission transitions to `Failed` and no resume turn
occurs.

---

### Auth model

**OS-user ownership per P4.4.** The daemon sets the socket file to
mode `0600`. On Linux, the daemon additionally verifies via
`SO_PEERCRED` that the connecting process's effective UID matches the
daemon's own UID. A UID mismatch is a hard `Error` and the
connection is closed immediately.

There is no Aivyx PA-level password, token, or challenge-response
handshake. Per P6, identity equals OS user.

---

### Protocol versioning

The `DaemonReady` lifecycle event carries a `version` string. Phase
16 defines version `"0.1"`. The frontend checks the version on
connect; a version mismatch is a warning (not a hard error) in
Phase 16, becoming a hard error once the protocol stabilizes in a
future SDK phase.

---

### Decisions record (for PHASE_16.md cross-reference)

| Question | Resolution | Rationale |
|----------|------------|-----------|
| **Q3** — Wire format | **(a)** Hand-rolled length-prefixed JSON | Debuggability over throughput for the PoC phase. `serde_json` already in tree. Zero new deps. |
| **Q4** — Daemon lifecycle shape | **(a)** Separate message type, never crosses `StreamEvent` | Preserves production-core streak. Frontend demuxes on `"type"` discriminator. |

---

## Phase 47 addendum — Query/QueryResponse envelope

> *Section added at Phase 54 to document the Phase 47 protocol
> extension. The Phase 47 changes are additive — any Phase 16
> frontend that didn't ask Query questions continued to work
> unchanged.*

Phase 47 added a **read-only inspection-query layer** on top of
the existing event-stream IPC. The daemon's state — active
sessions, persisted missions, the audit chain — became
introspectable from any connected frontend without going
through the turn loop.

### Wire shape

Two new variants on the existing envelopes:

```rust
// Frontend → Daemon
FrontendMessage::Query { id: String, payload: QueryPayload }

// Daemon → Frontend
DaemonMessage::QueryResponse { id: String, payload: QueryResponsePayload }
```

`id` is a caller-supplied correlation string. The daemon echoes it
verbatim in the response so concurrent queries can be
demultiplexed.

### `QueryPayload` variants

| Variant | Returns | Notes |
|---|---|---|
| `ListSessions` | `Vec<SessionSummary>` from in-memory `DaemonState.sessions` | One entry per active connection. |
| `ListMissions` | `Vec<MissionSummary>` from `KeyDomain::Missions` | Walks the redb scan; cheap at typical operator scales. |
| `GetMission { mission_id }` | `Option<MissionDetail>` | `None` is not an error — it means the id is absent. |
| `ListAuditEntries { from_seq, limit }` | `(Vec<AuditEntrySummary>, total_len)` | Server caps `limit` at 500 (Phase 47 Q3). `total_len` lets the frontend show "showing N..M of T." |
| `VerifyAuditChain` | `{ ok: bool, entries_verified: u64, error: Option<String> }` | Runs `HmacChainLog::verify`. Tamper detection by walking the chain offline. |

### Authorization

**No capability check at the query layer.** The IPC socket is
`mode 0600` owned by the operator UID; anyone who can `read(2)`
the socket *is* the operator by definition (P6 +
`THREAT_MODEL.md` §4.4). Gating queries against `audit.read`
would only check the operator's own role envelope against their
own inspection — not the threat model.

This is the **Q2 resolution** from Phase 47's open doc and is
documented inline at `aivyx-channel::daemon_server::handle_query`.

### Audit entry projection

`AuditEntrySummary` is a flattened view of `SignedEntry`:

```rust
pub struct AuditEntrySummary {
    pub seq: u64,
    pub appended_at_unix_ms: u64,        // SystemTime → millis at the IPC boundary
    pub event_type: String,              // "ToolCall" / "ScopeDenied" / etc.
    pub event: serde_json::Value,        // the structured event body
    pub mac_hex: String,                 // hex-encoded HMAC tag for display
}
```

The wire schema deliberately holds the event body as
`serde_json::Value` so new `AuditEvent` variants land additively
without bumping the protocol version. Frontends that don't
recognize an event type can render `event_type` + the raw JSON.

### Frontend perspective

The Web UI (Phase 47) uses these queries to render the Missions,
Audit, and Sessions tabs. Third-party frontends — including the
Phase 48 Python channel reference — get the same surface via
the same wire format. See `CHANNEL_SDK.md` §4 (the message
envelope cheatsheet has `Query` and `QueryResponse` listed
alongside the event-stream variants).

### Forward compatibility

New `QueryPayload` and `QueryResponsePayload` variants land
additively. The recommended posture for frontends is the same
as for `DaemonMessage` and `StreamEventPayload`: decode by tag,
skip unknown variants gracefully. The Phase 48
`examples/python-channel/` reference adapter demonstrates the
pattern.

## Phase 102 addendum — `GetToolStats` tool-observability query

`QueryPayload::GetToolStats { window_secs: Option<u64> }` is a
read-only query backing the `aivyx-pa tools` CLI subcommand.
`window_secs = None` scopes the answer to the whole audit
chain; `Some(n)` to `ToolCall` events from the last `n`
seconds.

The daemon answers with `QueryResponsePayload::ToolStats {
tools: Vec<ToolStat> }`. It joins two sources: the registered
tool set, snapshotted from the `ToolRegistry` at daemon
construction, and the audit chain, walked and folded over
`AuditEvent::ToolCall` events keyed by `scope_used.base()` —
the stable capability base (`fs.read`, `net.fetch`), not the
per-process `tool_id`. Each `ToolStat` row carries `name`,
`description`, `scope_base`, `registered` (false = a base with
call history but no currently registered tool), `calls`, a
per-outcome `outcomes` map (`completed` / `failed` / `denied`
/ `not_in_role` / `requires_escalation`), and
`total_duration_ms`. Rows are ordered by call count
descending, then name ascending.

The query returns `QueryError { code: "no_audit_log" }` on a
daemon with no audit log configured — the same posture as the
audit-entry queries.

## Sub-project 8 addendum — `GetMcpServerCallStats` per-MCP-server query

`QueryPayload::GetMcpServerCallStats { window_secs: Option<u64> }` is a
read-only query backing the Studio MCP panel's rolling health chip.
`window_secs = None` scopes the answer to the whole audit chain;
`Some(n)` to `ToolCall` events from the last `n` seconds — same
convention as `GetToolStats`.

The daemon answers with `QueryResponsePayload::McpServerCallStats {
servers: Vec<McpServerCallStats> }`. Unlike `GetToolStats` (which
groups by `scope_used.base()`, the stable capability base — `"mcp.call"`
for every MCP-bridged tool call regardless of server), this query
groups by the MCP server name recovered from the scope's qualifier
(`mcp.call:<server>:<tool>`, split from the left) — closing the gap
where `GetToolStats` alone can't distinguish one configured
`[[mcp_server]]`'s health from another's. Each `McpServerCallStats` row
carries `server_name`, `calls`, a per-outcome `outcomes` map (only
`completed`/`failed` are reachable for `mcp.call`-scoped entries — see
`McpServerCallStats`'s own doc comment in `aivyx-ipc`), and
`total_duration_ms`. No registry join: a server with zero calls in the
window has no row at all — the caller (Studio's `McpPanel`) joins this
against its own `GetMcpStatus` server list client-side to render a
"no recent activity" state for a configured-but-unused server.

The query returns `QueryError { code: "no_audit_log" }` on a daemon
with no audit log configured — the same posture as `GetToolStats`.

## Routing visibility addendum (B1) — routed-model and cloud-consent events; routing queries

Additive: adapters skip stream-event kinds they don't know (the Channel
SDK contract), so older front ends are unaffected.

### Stream events

Both are daemon-only — there is no core `StreamEvent` counterpart — and
both are sent after the turn's own events, before `TurnComplete`.

**`ModelRouted { model, task, reason }`** — the model routing last chose
for this conversation (`Router::last_decision`), sent after every turn
once the conversation has a routed call. `model` is `id@endpoint`,
`task` the routed call's kind (`chat`, `plan`, …), `reason` the router's
human sentence. Never sent with `[routing]` off, or before the
conversation's first routed call.

```json
{"kind": "ModelRouted", "model": "qwen3:8b@default", "task": "chat",
 "reason": "…"}
```

**`CloudConsentRequested { model, endpoint, why, estimated_tokens,
can_allow_here }`** — the turn stopped to ask for cloud consent (A15
`ask` mode). `model` is the model id, `endpoint` the
`[routing.endpoints.*]` name, `why` plain words (`no local model can
handle this request`, `this kind of request is set to use the cloud`,
`the local model got stuck`), `estimated_tokens` what would be sent.
`can_allow_here` is `true` when the session's channel is Trusted or
Kernel — the channels `/allow-cloud` accepts consent from.

```json
{"kind": "CloudConsentRequested", "model": "claude-sonnet-4-5",
 "endpoint": "anthropic", "why": "no local model can handle this request",
 "estimated_tokens": 12578, "can_allow_here": true}
```

When the event is sent, the `TurnComplete` outcome text is replaced by
the same request worded for the channel (no `LLM error:` framing), so a
front end that ignores the event still shows it:

- `can_allow_here`: "This needs a cloud model: `<model>` (your
  `<endpoint>` endpoint), because `<why>`. About `<N>` tokens — this
  conversation plus the assistant's instructions — would be sent. Send
  /allow-cloud to allow it for this conversation, then resend your
  message."
- otherwise: the same first two sentences, then "Cloud use can only be
  allowed by the operator — from the terminal (`/allow-cloud`) or the
  Studio."

A consent request never outlives its turn: the daemon drops any request
left for the conversation before the turn runs, and takes it after. It
is shown only when the turn's outcome actually carries it — a routed
side call that stopped for consent while the turn still answered never
replaces the answer.

### Queries

**`GetRoutingStatus { session_id: Option<String> }`** →
`RoutingStatus(RoutingStatusView)`. Built by the same function as the
`routing.status` tool. Fields: `enabled`, `default_model`
(`id@endpoint`), `candidates` (each `model`, `tier`, `capabilities`,
`unknown_capabilities`, `context_window`, `availability`, `residency`:
`loaded` / `needs_load` / `wont_fit` / `null`), `vram_total_bytes`,
`vram_available_bytes`, `escalation_mode` (`off` / `ask` / `auto` /
`never`), `classifier_enabled`, and — for the given session — `session`
(`session_id`, `current_model`, `pinned`, `last_model`, `last_reason`,
`tainted`, `cloud_allowed`). With routing off the answer is `enabled:
false` with empty lists and `escalation_mode: "off"`, never an error. A
`session_id` that isn't a session UUID is `QueryError { code:
"invalid_session" }`.

**`SetRoutingPin { session_id: String, model: Option<String> }`** →
`RoutingPinned { model: Option<String> }` (the resolved `id@endpoint`,
or `None` after unpinning). `model` is `id@endpoint`, or a bare id
served by exactly one endpoint — resolved like `aivyx-coder`'s `/model`.
Errors (`QueryError`):

| `code` | `message` |
|---|---|
| `routing_disabled` | "Model routing commands are not available here — they need `[routing] enabled = true`." |
| `unknown_model` | "No model `<arg>` — see /models." or "`<id>` is served by several endpoints (a@x, b@y) — use id@endpoint." |
| `invalid_session` | "`<id>` is not a session id" |

Trust: the same as the other Studio settings queries — the socket's
`0600` mode is the auth boundary. A pin changes which local model a
conversation uses; it never grants cloud escalation (A15 unchanged).
