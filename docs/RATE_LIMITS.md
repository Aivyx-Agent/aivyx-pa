# Tool-Call Rate Limits & Quotas (Chapter Throttle)

> **Status:** ✅ **shipped** (Chapter Throttle complete, TH.0–TH.4). This began
> as the design contract and is now implemented: the `RateLimiter` core
> (`aivyx-cost`), the `RateGate` trait + `ToolOutcome::RateLimited` (`aivyx-core`),
> the `AuditEvent::RateLimited` record (`aivyx-audit`), the `[rate_limit]` config
> (`aivyx-config`), the `ChannelRateGate` wired into the daemon + voice turn loops
> (`aivyx-channel`/`aivyx-cli`), and a `THREAT_MODEL.md` §4.9 note. **One deferral:**
> `write_rate_limit_section` — its only consumer is a future Studio rate-limit
> editor (§6 defers that editor); the `[rate_limit]` config is hand-editable and
> enforced today.
>
> Throttle gives the operator **control over how often tools run**. Capabilities
> gate *what* a tool may do; budgets ([`COST_GOVERNANCE.md`](COST_GOVERNANCE.md))
> gate *how much it spends*; Throttle gates *how often it is called*. It is
> **free core** — self-protection + observability, not customer metering — and
> closes finding **F2** of the
> [2026-06-16 backend audit](BACKEND_AUDIT_2026-06-16.md).

## 1. The gap — calls are unbounded within a turn

Today the only call-frequency bound is `max_iterations` on the **autonomous
loop** (`loop_driver.rs`). *Within* a single turn — and across a Nonagon
mission or a loop iteration — the agent can call `web.fetch` / `shell.exec` /
any tool an unbounded number of times, limited only by LLM behaviour and the
token budget (if one is set, and only for *model* calls, not tool calls).

For a security-focused framework that runs **unattended** (the Ralph loop,
daemon-side teams, cron/webhook triggers), that is a real exposure:

- **Runaway self-DoS** — a planner stuck in a tool-call loop hammers a remote
  endpoint or the local shell.
- **Cost amplification** — each tool call can fan out into more model calls;
  unbounded tool calls means unbounded turns.
- **Abuse of a granted scope** — a capability grant is "may use `web.fetch`",
  never "may use it 10,000 times a minute."

Capabilities and budgets do not cover this. Throttle does.

## 2. The key insight — mirror the budget gate, don't invent

Chapter K already shipped the exact shape this needs, for model spend. Throttle is
its sibling for **call counts**, reusing the same proven pattern rather than a
new mechanism:

| Budget (Chapter K) | → Throttle (this chapter) |
|---|---|
| `BudgetGate` trait in `aivyx-core` (keeps `ConcreteAgent` decoupled) | `RateGate` trait in `aivyx-core`, same role |
| `BudgetAction::{Alert, Deny}` | `RateAction::{Alert, Deny}` |
| pre-call **reserve** = dollar deny gate | pre-call **check** = call-count deny gate |
| `[budget]` config (token caps; the `*_usd` money caps were removed in 2.0) | `[rate_limit]` config (call caps, per-tool overrides) |
| `AuditEvent::LlmCost` per turn | `AuditEvent::RateLimited` per throttled call |
| `aivyx-pa cost [--today]` surfacing | a line in `aivyx-pa loop status` / report |

The limiter logic lives in **`aivyx-cost`** (renamed in spirit to "the
governance crate"; no new crate — it already owns `BudgetEnforcer`, and a
`RateLimiter` next to it shares the window/aggregation idioms) or a small
sibling module — decided at TH.1. The **trait** lives in `aivyx-core` so the
turn loop can call it without a `aivyx-cost` dependency leaking into the core.

## 3. What's deliberately *not* here

- **Not customer metering / billing.** Same line as Chapter K — this protects
  the operator's own machine and spend, it is not a usage-based pricing meter.
- **Not a global token-bucket scheduler.** No cross-process coordination, no
  distributed limiter. One daemon, in-memory counters on the one chain.
- **Not a replacement for capabilities or budgets.** It is the third gate in
  the same dispatch path, forensically distinct from both.
- **Not network-level rate limiting.** It counts *tool invocations*, not
  packets or HTTP requests a tool makes internally.

## 4. The three limits

Each is **optional** and independently configurable. A call is checked against
all configured limits; the **first** that would be exceeded decides the outcome
(and names itself in the audit reason).

1. **Per-turn, per-tool** — at most `per_turn` calls to a given tool within one
   turn. Catches a planner spamming a single tool.
2. **Per-turn, total** — at most `per_turn_total` tool calls in one turn. The
   turn-level cap that does not exist today.
3. **Sliding-window, per-tool** — at most `per_window` calls to a tool within
   `window_secs`, measured across turns / loop iterations. Reuses the
   `notify_dispatcher` window-counter idiom; bounds *sustained* rate, not just
   per-turn bursts.

Windows 1–2 reset at turn boundaries (in-memory, per-turn state). Window 3 is a
daemon-lifetime sliding counter keyed by tool name.

## 5. Enforcement & forensics

A new **`ToolOutcome::RateLimited { tool, limit, scope, window }`** — added
beside `Denied` (capability) and `NotInRole` (role), and forensically distinct
from both, exactly as `NotInRole` was made distinct from `Denied` (lib.rs
Phase-28 precedent).

- **`RateAction::Deny`** — the call does **not** execute; the planner continues
  (the model sees a throttle result and can adapt, just as it sees a `Denied`).
- **`RateAction::Alert`** — the call executes, but a warning is recorded.
- The throttled call still counts in `tool_calls_made` and emits exactly **one**
  `ToolCall` audit entry with `outcome = RateLimited`, plus a dedicated
  **`AuditEvent::RateLimited { tool, limit, window, action }`** — consistent
  with how `Denied` / `NotInRole` already appear on the chain. Operators
  filtering a forensic walk can separate "throttled" from "scope-denied" from
  "role-forbidden."

## 6. Config

Mirrors `[budget]` + `[pricing.<model>]`. **Default = uncapped** (opt-in), the
same default-off stance budgets take, so every existing config behaves
identically after the upgrade. (A future phase may add a conservative
`alert`-at-threshold default once the shape is proven in the field.)

```toml
[rate_limit]
per_turn_total = 40             # max tool calls in one turn (omit = uncapped)
default_per_turn_per_tool = 10  # per-tool cap unless a [rate_limit.tools.*] overrides
action = "deny"                 # "deny" (block) or "alert" (warn + proceed); default "deny"

[rate_limit.tools."web.fetch"]
per_turn = 6                    # tighter per-turn cap for this tool
per_window = 20                 # sliding-window cap …
window_secs = 60                # … over this many seconds
```

Written via the same section-scoped `toml_edit` writer family the Studio
Settings screen uses (`write_*_section`) so it is editable by hand **and**
(future) from the web — though wiring a Studio editor is out of scope for this
chapter.

## 7. Phase plan

All phases ✅ shipped.

| Phase | Deliverable |
|---|---|
| **TH.0** | This contract. |
| **TH.1** | Limiter core in `aivyx-cost` (or a sibling module): `RateAction`, the per-turn + sliding-window counters, a pre-call `check(tool, now) -> RateDecision` that mirrors `BudgetEnforcer::reserve`. Unit-tested in isolation (per-tool cap, total cap, window expiry, alert-vs-deny), mirroring `budget.rs`. |
| **TH.2** | `RateGate` trait in `aivyx-core` + `ToolOutcome::RateLimited`. This adds an enum variant, so it ripples through the workspace's exhaustive `match` sites (the `AuditEvent`-variant lesson — run the **full** suite; expect several count-assertion and match-arm fixups). |
| **TH.3** | `[rate_limit]` config (`aivyx-config` parse + a `write_rate_limit_section` writer) + wire the gate into the turn-loop tool dispatch **beside the budget gate**; emit `AuditEvent::RateLimited`; surface counts in `aivyx-pa loop status` (and/or the cost report). |
| **TH.4** | Finalize: **F7 count-assertion tests** (`assert_eq!(KNOWN_BASES.len(), N)` + storage-domain + substrate-tool counts) bundled here as the "hardening" coda; docs (a `THREAT_MODEL.md` note that autonomous-loop tool-rate is now bounded; flip audit **F2 → resolved** + **F7 → resolved**); README/CHANGELOG; live-verify a deny + an alert on a real daemon. |

## 8. Locked decisions

- **Third gate, same path.** Throttle is checked in the turn-loop tool-dispatch
  alongside the capability and role gates and the budget gate — not a separate
  pre-pass. Order: capability → role → budget → **rate** (rate last, so a call
  that would be scope-denied is never "throttled").
- **Forensically distinct.** `RateLimited` is its own `ToolOutcome` and its own
  `AuditEvent`; never folded into `Denied`.
- **Opt-in, non-breaking.** Uncapped by default; existing configs unchanged.
- **In-memory, one daemon.** Counters are daemon-lifetime in-memory state; no
  persistence, no cross-process coordination. A daemon restart resets windows
  (acceptable — windows are seconds-to-minutes, restarts are rare).
- **Free core.** Self-protection + observability, not customer billing — the
  same boundary Chapter K holds.
- **Reuse over invention.** The `BudgetGate` / `BudgetEnforcer` pattern is the
  template; deviations must be justified at review.
