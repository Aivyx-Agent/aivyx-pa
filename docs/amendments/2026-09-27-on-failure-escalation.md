# Amendment A16 — `on_failure` Cloud Escalation

**Date:** 2026-09-27
**Phase:** Model routing Part 3b (cloud escalation + sensitivity taint) — the `on_failure` trigger A15 deferred.
**Amends:** A15's "What this does not say" bullet, "It does not
authorize `on_failure` escalation." A15 is otherwise unchanged: G6 and
N5 are not narrowed or relaxed, and A15's other four "What this does
not say" bullets still hold in full.
**Implementing phase:** Model routing Part 3b (continued). The
governance gate precedes the feature, as in A15 (the A13 / FG.2
precedent).
**Reference design:** `aivyx-ecosystem/docs/superpowers/specs/2026-09-27-on-failure-escalation-design.md`.

---

## What changed

A15 authorized two escalation triggers (`no_local_candidate`, `tiers`)
and explicitly deferred a third: "escalating a retry after a local
attempt demonstrably fails." A16 authorizes that trigger, `on_failure`,
under a new `[routing.escalation] on_failure = false` key (default
off, alongside `no_local_candidate` and `tiers`), bounded by the
operator's decisions of 2026-09-27:

1. **What gets escalated.** The **next turn only**. The failed turn
   ends exactly as it does today — nothing is replayed, no tool call
   runs twice, and the model never changes mid-turn. This is A15's
   stop-and-allow rule, unchanged.
2. **Which failures count.** **Looping** (the repeated-call breaker,
   Chapter Bridle), **tool-call repair exhausted** (both
   `invalid_input` repair rounds spent in the turn), and, in the
   autonomous loop only, a judge **Verdict FAIL** and a **Circuit
   stall**. Audited signal names: `"looping"`,
   `"tool_call_repair_exhausted"`, `"loop_verdict_fail"`,
   `"loop_stall_rescue"`. **Not** `MaxStepsExceeded` — a weaker signal
   — and **never** `Failed(..)` in general: network, missing-server
   and budget errors are not evidence the model failed, and must not
   push a conversation to the cloud.
3. **The autonomous loop.** Arms only when `mode = "auto"` and
   `on_failure = true`. A stall gets exactly one armed rescue
   iteration per run, then the run stops as today. In `ask` mode the
   loop never arms — no operator is present mid-loop to consent.
4. **Mechanism.** A one-shot **armed** mark per conversation on the
   daemon's shared `RoutingGuard` (the same object that already holds
   taint and consent). `aivyx-route` is unchanged; escalation stays
   `aivyx-pa`-only.

---

## Why

A15 already lets a local-first configuration send a specific
conversation's call to an operator-configured cloud endpoint when a
trigger fires and consent passes. `on_failure` closes the remaining
gap: today, when the local model demonstrably fails a conversation —
it loops, exhausts its tool-call repair budget, or a supervising judge
or circuit breaker says so — the operator's only recourse is to
intervene by hand and resend under a different model. `on_failure`
lets the *next* turn escalate instead, under the same consent gate,
the same taint rule, and the same audit trail as A15's other two
triggers. It is bounded the same way: off by default, and inert with
no cloud endpoint configured or with `mode = "never"`.

---

## What it says

These bullets are part of the contract, same as A15's six rules; each
is covered by tests in the implementing phase.

1. **`on_failure` is subject to every one of A15's six rules, with no
   exception.** Taint blocks it in every mode (rule 1); it is
   persisted taint, never re-derived (rule 2); a call without a
   conversation session never escalates under it (rule 3);
   escalation still goes only to operator-configured
   `[routing.endpoints]` with the operator's own keys (rule 4); every
   decision it makes is audited with a payload hash, never content
   (rule 5); and where `ask` mode applies to it, consent stays
   in-memory and per-conversation (rule 6). A cloud model that serves
   an armed turn never becomes the conversation's sticky model: the
   conversation stays on its local model, and only that one turn went
   to the cloud.
2. **The armed mark is a new, distinct piece of in-memory state, held
   to the same standard as consent.** One-shot, per conversation,
   never written to disk: a daemon restart clears it, exactly as it
   clears a consent grant. Arming itself is audited once per new mark
   as `EscalationArmed`; the resulting escalation decision is audited
   as `CloudEscalation` with trigger `"on_failure"`, same as any other
   trigger.
3. **DESIGN.md's per-turn capability rule is unchanged, because the
   escalated turn is a new turn.** Effective capabilities are still
   computed once per turn and not changed mid-turn; "mid-turn
   capability changes... are NOT supported in v1" still holds exactly
   as A15 left it. The failed turn records the armed mark for the
   *next* turn as it finishes (never for itself), and the next turn — sent automatically under `auto`,
   or after an `/allow-cloud` resend under `ask` — is where escalation
   takes effect. This is A15's stop-and-allow mechanism, unmodified;
   `on_failure` relies on it rather than replacing it.
4. **In `ask` mode, an arming turn's reply may gain one line**, added
   only when a cloud candidate exists and the conversation isn't
   tainted: `The local model got stuck; send /allow-cloud and resend
   to retry on \`<model>\`.` This is a hint text, not a capability
   grant and not an escalation in itself — it says nothing when no
   cloud candidate exists or the conversation is tainted. If the
   operator instead sends an ordinary message, that turn is armed and
   stops for consent as A15's `ask` mode does; the stop leaves a
   **pending offer**, not a new mark. `/allow-cloud` (chat, IPC or
   CLI) turns the offer into an armed mark, so the resend escalates
   exactly once. The next turn started without `/allow-cloud` declines
   the offer: it runs locally, and no further consent stops come from
   the old failure. An ignored offer lapses; it never locks the
   conversation into repeated consent stops.
5. **Defaults change nothing.** With `on_failure = false` (the
   default), or with no cloud endpoint configured: nothing arms, no
   new audit entries appear, and output is byte-identical to A15.
   Every existing test passes unchanged.

---

## What this does not say

- **It does not authorize escalating `MaxStepsExceeded`.** That
  remains a weaker signal than the four this amendment authorizes, and
  stays out of scope.
- **It does not authorize replaying a turn.** The failed turn's tool
  calls are never re-executed; only the *next* turn is ever escalated.
- **It does not authorize `ask`-mode escalation inside the autonomous
  loop.** The loop arms only under `mode = "auto"`; in `ask` it never
  arms, because no operator is present mid-loop to consent.
- **It does not authorize arming from a system-originated turn.**
  Only an operator's own turn arms its conversation. A trigger, cron,
  webhook or file-watch fire runs on a session that never gets a next
  turn; the autonomous loop arms its iterations itself, under `auto`
  only.
- **It does not authorize persisting armed marks across a restart.**
  Like consent, the mark lives in memory only.
- **It does not authorize escalating `Failed(..)` in general.**
  Network, missing-server and budget failures are excluded, per
  decision 2 above — they are not evidence the model itself failed.
- **It does not change A15's other two triggers, its modes, its taint
  rule, or N5.** `no_local_candidate` and `tiers` are unaffected;
  cloud escalation still goes nowhere but an operator-configured
  `[routing.endpoints]` destination, still with no Aivyx-hosted
  component, no key proxying, and no telemetry in the path.
- **It does not wire up a failure classifier.** Detecting the four
  signals (Looping, repair-exhausted, Verdict FAIL, Circuit stall) is
  separate implementation work already named by existing mechanisms
  (Bridle, the repair loop, the judge, the loop driver's stall
  breaker); this amendment authorizes escalating on them once
  detected, not how they are detected.
