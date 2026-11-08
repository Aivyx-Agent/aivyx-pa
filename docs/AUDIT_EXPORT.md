# Audit Chain Export (`aivyx-pa audit export`)

Phase 105 added a read-only offline export of Aivyx PA's HMAC-chained audit
log. Every tool call, scope denial, turn boundary, memory access, and
auto-notify dispatch lands in the chain as a structured `AuditEvent`;
`aivyx-pa audit export` plumbs those entries out as **JSONL** on stdout so
downstream tooling — `jq`, training pipelines, forensic auditors — can
consume them without reaching into `redb` by hand.

## At a glance

```text
aivyx-pa audit export [--from <seq>] [--limit <N>] > trajectory.jsonl
```

- **Offline**: opens encrypted storage cold with the operator's
  passphrase, the same path `aivyx-pa --verify-only` uses. Works whether
  the daemon is running or not.
- **Read-only**: the chain itself stays append-only; the export never
  mutates a byte.
- **Sequence-filtered**: `--from <seq>` skips ahead, `--limit <N>` caps
  the output. Both forward straight into
  `PersistentAuditLog::entries_range`.
- **JSONL**: one JSON object per line, terminated with `\n`. Includes
  the MAC fields so the chain is **re-verifiable downstream** given a
  separately-supplied genesis seed.

## Line shape

Every line is a flat JSON object with six keys:

| Key              | Type           | Notes                                                |
|------------------|----------------|------------------------------------------------------|
| `version`        | `u32`          | The line shape's version, `1`. Changes only when a field is removed or changes meaning; new fields can appear without it. |
| `seq`            | `u64`          | Monotonic 0-based sequence number.                   |
| `appended_at_ms` | `u64`          | Wall-clock append time, milliseconds since epoch.    |
| `prev_mac`       | `string` (hex) | 32-byte HMAC of the previous entry. Lowercase hex.   |
| `mac`            | `string` (hex) | 32-byte HMAC of this entry. Lowercase hex.           |
| `event`          | `object`       | The `AuditEvent` payload, tagged by `kind`.          |

The fields are intentionally **flat** (no nested `signed` envelope) so a
filter like `jq '.event.kind == "ToolCall"'` works without an extra
indirection.

## Example lines

A turn-started event:

```json
{"version":1,"seq":17,"appended_at_ms":1716750000123,"prev_mac":"4a...","mac":"7b...","event":{"kind":"TurnStarted","turn_id":"...","session_id":"...","channel":"Local","trust_tier":"Trusted","effective_capabilities":{...}}}
```

A tool call:

```json
{"version":1,"seq":18,"appended_at_ms":1716750000456,"prev_mac":"7b...","mac":"9c...","event":{"kind":"ToolCall","turn_id":"...","tool_id":"...","scope_used":"fs.read:/home/me/notes/**","input_hash":"...","outcome":{...},"duration":{"secs":0,"nanos":4200000}}}
```

The `AuditEvent` variants are defined in
`crates/aivyx-audit/src/lib.rs`; the `kind` discriminator names match
those variants exactly: `ToolCall`, `ScopeDenied`, `TurnStarted`,
`TurnEnded`, `MemoryAccess`, `AutoNotifyDispatched`.

## Filters

### `--from <seq>`

Start emitting at entry `<seq>`. Defaults to 0 (the whole chain).

```bash
# Skip the first 1000 entries.
aivyx-pa audit export --from 1000 > tail.jsonl
```

### `--limit <N>`

Emit at most `<N>` entries. Defaults to "no upper bound." `--limit 0` is
rejected at parse time (operator almost certainly meant to omit the
flag).

```bash
# Just the next 100 after entry 1000.
aivyx-pa audit export --from 1000 --limit 100 > sample.jsonl
```

Flag order does not matter:

```bash
aivyx-pa audit export --limit 100 --from 1000   # equivalent to above
```

### `--event-type <type>`

Emit only one kind of event: `tool_call`, `scope_denied`, `turn_started`,
`turn_ended`, `memory_access`, `auto_notify_dispatched`,
`skill_auto_proposal` or `skill_invocation`. The older spellings, the
event's `kind` (`ToolCall`, ...), still work.

```bash
aivyx-pa audit export --event-type tool_call > tool-calls.jsonl
```

### Time-range and correlation filters

Not in v1 (Phase 105 Q2a sign-off). Operators wanting time-windowed or
session/mission-correlated exports pipe through `jq`:

```bash
# Last hour, by appended_at_ms.
aivyx-pa audit export \
  | jq -c 'select(.appended_at_ms > (now * 1000 - 3600000))' \
  > last-hour.jsonl

# All entries for one session.
aivyx-pa audit export \
  | jq -c 'select(.event.session_id == "abc-123")' \
  > session.jsonl

# Just tool calls + their outcome kinds.
aivyx-pa audit export \
  | jq -c 'select(.event.kind == "ToolCall") | {seq, tool: .event.tool_id, outcome: .event.outcome.kind}'
```

If pipe-through-`jq` becomes a recurring pain point, time and
correlation filters can land as a focused follow-on phase.

## Re-verifying the chain downstream

Each line carries `prev_mac` and `mac` for the entry's slot in the
chain. A consumer that wants to re-verify the chain can:

1. Obtain the **genesis seed** separately (the export does not emit it
   on purpose — it's a derived key bound to the encrypted store).
2. For each line in order: compute
   `HMAC-SHA256(prev_mac || canonical_json(event))` and compare against
   the line's `mac`. The very first entry's `prev_mac` is the genesis
   seed.
3. `prev_mac` of entry `N+1` must equal `mac` of entry `N`. The export
   preserves chain order (sorted by `seq` ascending).

A break at any point is structurally significant — the on-disk chain
itself uses the same algorithm, so an export that re-verifies cleanly
proves on-disk integrity at the moment of export. The HMAC algorithm
and chain structure are documented in `docs/DESIGN.md`'s Phase 6
audit-chain section.

## Security posture

`aivyx-pa audit export` cannot be triggered remotely over the daemon
socket (Q3a — offline-only). The export requires the operator's
passphrase, the same as `aivyx-pa --verify-only`. The dump lands on the
operator's stdout, where the operator chooses what to do with it —
pipe to a file, pipe to `jq`, pipe to a training pipeline, throw away.

The chain's `MemoryAccess.query_or_key` field is plain text; by
convention operators don't pass raw secrets through memory tools, but
the chain does not enforce that. **Treat the export the same as the
encrypted store itself** — operator-private, not for sharing without
review.

The `input_hash` on `ToolCall` is SHA-256 of the raw tool input (per
the substrate-level "hash, not raw input" convention), so tool inputs
themselves do not leak through the chain or its export.

## What's not in v1

Three deferrals worth naming so a future micro-phase can pick them up
when operator pressure surfaces:

1. **Time-range filters** (`--since <ISO>` / `--until <ISO>`).
2. **Correlation filters** (`--session <id>` / `--mission <id>`).
3. **Daemon-mode export** (`Query::ExportAudit` over IPC) — would let
   the export include in-memory not-yet-flushed entries.

Each is a focused add to the existing surface, not a substrate change.

## See also

- `aivyx-pa --verify-only` — companion forensic mode, walks the chain
  without emitting it. Run it first if you suspect tamper.
- `crates/aivyx-audit/src/lib.rs` — canonical source for the
  `AuditEvent` variants and `SignedEntry` shape.
- `docs/DESIGN.md`'s Phase 6 audit-chain section — HMAC algorithm and
  genesis-seed binding.
