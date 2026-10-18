# Harden Local Tool-Calling (Chapter Bridle)

> **Status:** ✅ **COMPLETE (BR.0–BR.6).** The follow-on to [Chapter Stencil](STENCIL.md):
> Stencil made a small local GGUF emit a *valid, real-named* tool call by
> construction; **Bridle keeps it from running away.** Stencil's live verification
> (ST.4) proved the grammar but surfaced a real failure — the constrained Qwen3-4B
> **looped**, re-emitting an identical `memory.write` until the deadline because it
> never chose the `respond` sentinel to finish. Bridle is the safety harness around
> that: a turn-loop **repeated-call breaker** (A), a constrained-mode **`respond`
> preamble** (B), and an operator-configurable **turn timeout** for slow local
> backends (C). No new tool, base, scope, P10 amendment, or dependency — a
> robustness refinement of the turn loop + the MistralRs provider. Each piece is
> guarded so well-behaved (esp. cloud) turns stay byte-identical.

## 1. Why this chapter

A small local model that can finally *call* a tool (Stencil) is only useful if the
agent loop is **robust to how badly it can behave**. ST.4 (docs/STENCIL.md §7)
caught three concrete ways the local path still breaks, none of them grammar
defects:

1. **It loops with no terminator.** The constrained 4B emitted the *same*
   `memory.write` (identical `tool_id` + `input_hash`) over and over and never
   selected `respond` to end the turn. The existing `MAX_STEPS_PER_TURN = 32` cap
   eventually bounds this, but 32 identical no-progress calls is a terrible
   experience and burns the whole step + token budget.
2. **It doesn't know how to finish.** The `respond` sentinel branch (Stencil ST.1)
   *exists* in the grammar, but the model is never *told* it's the way to reply in
   plain text and stop. Root cause of (1).
3. **Slow local backends can't even complete a turn.** CPU inference of a 4B
   exceeds the hardcoded 120s `TURN_TIMEOUT`, so a legitimate (just slow) local
   turn is killed mid-generation. ST.4 had to bump the const locally to verify.

Bridle fixes all three. (A) is the generalizable safety net — it helps *every*
local model, constrained or not. (B) is the root-cause fix for the loop. (C) lets a
slow-but-correct local run actually finish.

## 2. Architecture & governance decisions (locked)

### This is turn-loop + provider robustness — **no new capability surface**
No tool, `KNOWN_BASES` base, scope, P10 amendment, or dependency. The trust-tier
model, per-role allowlist, sandboxing, and audit chain are untouched. The changes
are: one new turn outcome, two new **default-safe** `ConcreteAgent` builder knobs,
and a provider-internal prompt augmentation.

### A — Repeated-call breaker (the turn loop)
The loop (aivyx-core/src/agent.rs) already gives each step a `tool_id` + `input`.
Bridle tracks a **consecutive-identical-call signature** (`tool_id` + a hash of the
canonical `input`) and, when the same signature fires `N` times in a row,
terminates the turn via a **new `LoopOutcome::Looping` → `TurnOutcome::Looping`**
outcome (distinct from `MaxStepsExceeded` so operators see *why* — a loop, not a
budget exhaustion). Decisions:

- **New outcome variant, not reuse.** Legibility: `[turn stopped: repeated tool
  call]` reads differently from `[turn max steps exceeded]`. Touches `LoopOutcome`,
  `TurnOutcome`, `TurnOutcomeSummary` + its `From`, and the render.rs marker — a
  contained contract change. (`TurnOutcome` is **not** an `AuditEvent`, so this does
  *not* hit the e2e audit-event-count assertions; still, run the full suite.)
- **Counts *consecutive identical*, resets on any different call.** A→A→B→A is not a
  loop; A→A→A is. Progress (any distinct call, or a tool *result* that differs)
  clears the counter. This is what makes it safe to default ON: a healthy turn never
  trips it.
- **Default ON, threshold configurable, `0`/disabled = the old behavior.** A
  builder knob `with_repeat_call_limit(n)` on `ConcreteAgent` (pattern: the existing
  `with_budget_gate` / `with_rate_gate` optional knobs). Default `N = 3`. Setting it
  to `0`/`None` restores the pre-Bridle "only `MAX_STEPS_PER_TURN` bounds it"
  behavior for anyone who wants it. Because it only fires on *identical* repeats,
  defaulting ON cannot change a well-behaved turn — only a pathological one, where
  terminating at 3 instead of 32 is the desired change.
- **The terminated turn still yields something.** On break, synthesize a
  `final_message` (the accumulated assistant text if any, else a clear note that the
  turn was stopped after a repeated identical call) so the operator/channel isn't
  left empty-handed.

### B — Constrained-mode `respond` preamble (the provider)
When `constrain_tool_calls` is on, the MistralRs provider augments the **system
message** it builds with a short, fixed note: *"You may call a tool, or reply to the
user by emitting `{"name":"respond","parameters":{"text":"…"}}`. When you have
finished or need no tool, use `respond` to answer and end your turn."* This is the
one thing the model needs to know to use the sentinel that Stencil's grammar already
admits. Decisions:

- **Lives in the provider, gated by `constrain_tool_calls`.** Co-located with the
  constraint (mistral_rs/provider.rs), intrinsic to constrained mode — no separate
  config. Off when the flag is off → unchanged system prompt.
- **Augment, don't replace.** Append to the operator/role system prompt; never
  clobber it.
- **A pure, testable string.** The preamble text is a `const` (or pure fn) so it can
  be unit-asserted without the engine.

### C — Operator-configurable turn timeout (config → agent)
`TURN_TIMEOUT` is a deliberate const today ("a caller who needs a custom budget is
almost certainly papering over a real bug"). That reasoning holds for *cloud*
models; it does **not** hold for a legitimately slow local CPU backend. Bridle adds
an **opt-in override** without weakening the default:

- **New `[agent] turn_timeout_secs` config field, `Option<u64>`.** Unset → the
  120s const, byte-identical. Set → that many seconds.
- **Threaded via a `with_turn_timeout(Duration)` builder on `ConcreteAgent`** (same
  optional-knob pattern), consumed where the deadline task is spawned (agent.rs:340).
  The const stays the default; the field overrides it.
- **Keep the const's spirit:** document that raising it is for slow *local* backends,
  not for papering over stuck cloud turns — and note that the repeat-call breaker
  (A) now catches the most common "stuck" case independent of the timeout.

### D — CUDA 13.x is a tracked watch-item, **not** a deliverable
mistralrs 0.8.1 → `cudarc 0.19.7` hard-rejects CUDA 13.3. There is nothing to code
until an upstream `cudarc`/`mistralrs` bump lands; Bridle only records it (here +
the memory) so a future dep-bump sweep picks it up. No phase.

## 3. Scope

**In:** the repeat-call breaker + its outcome variant + the `with_repeat_call_limit`
knob (A); the constrained-mode preamble in the MistralRs provider (B); the
`[agent] turn_timeout_secs` field + `with_turn_timeout` knob (C); their tests; the
doc/memory updates. **Out:** CUDA/cudarc (watch-item D); any change to the grammar
itself (Stencil owns that); any new tool/base/scope/dep; streaming the MistralRs
provider; and a *cross-turn* loop detector (Bridle is within-turn only).

## 4. Phase plan (docs-first, small phases per convention)

| Phase | Deliverable | Notes |
|---|---|---|
| **BR.0** 🟡 | **This design contract** | Locked reference; banner flips per phase. |
| **BR.1** ✅ | **`Looping` outcome** | DONE. Added `LoopOutcome::Looping {final_message, repeat_limit}` (agent.rs) + `TurnOutcome::Looping {final_message, tool_calls_made, duration, repeat_limit}` + `TurnOutcomeSummary::Looping` (+ `From`) + the `LoopOutcome→TurnOutcome` translation, all in aivyx-core. Render marker `[turn stopped: repeated tool call]` (render.rs), distinct from max-steps. Filled every exhaustive match the compiler flagged across the workspace (10 sites: role_switch, discord/slack/telegram channel suffixes, trigger ×2, daemon_server ×2, reflection_scheduler, skill_auto_proposer) — `Looping` joins the non-success terminations (mission cancel; `FailureKind::Failed`) and surfaces its synthesized message. No behavior change yet (nothing emits `Looping`). Render test extended (5→7 kinds, all distinct). `cargo build --workspace --tests` clean; aivyx-core (1011) + aivyx-channel suites green; clippy clean on both. |
| **BR.2** ✅ | **Repeat-call breaker (A)** | DONE. `DEFAULT_REPEAT_CALL_LIMIT = 3` + `ConcreteAgent.repeat_call_limit` field + `with_repeat_call_limit(n)` builder (`0` disables). Turn loop tracks consecutive-identical signatures (`call_signature` = `tool_id` + `to_string`(pre-injection input), folded over a batch via `batch_signature`); `note_repeat` bumps/resets the run; on trip → `LoopOutcome::Looping` with `looping_message(...)` **before** the tripping call dispatches (so 2 execute, the 3rd stops). Default ON — safe because it only fires on *identical* repeats. Tests: A,A,A trips at 3 (2 ToolCall audits, `Looping` summary); A,A,B,A,A completes (5 calls); `with_repeat_call_limit(0)` falls through to `MaxStepsExceeded`. **Pre-existing tests updated for the now-on breaker:** the max-steps runaway test switched to *distinct* inputs (it tests progress-without-finishing, not a loop); two rate-gate tests (identical calls to hit the cap) set `with_repeat_call_limit(0)` to isolate their subject. aivyx-core (563) + channel/team/audit suites green; clippy clean. |
| **BR.3** ✅ | **Constrained preamble (B)** | DONE. `RESPOND_PREAMBLE` const (tool-calling-mode instruction: use `{"name":"respond",…}` to answer/finish; don't repeat a call) + pure `system_message_for(base, constrain)` helper that appends it after the operator prompt only when constraining. Wired into `chat_stream`: `constrain` is now computed *before* the system message so the preamble lands; off → the system prompt is byte-identical. Unit test: appended iff constrained, operator prompt preserved (starts-with), names the `respond` sentinel, preamble-alone when no base. 10 provider tests green (feature build), clippy clean. |
| **BR.4** ✅ | **Configurable timeout (C)** | DONE. `RawAgent.turn_timeout_secs` + resolved `AivyxConfig.turn_timeout_secs: Option<u64>`; `ConcreteAgent.turn_timeout` field (defaults to `TURN_TIMEOUT`) + `with_turn_timeout(Duration)` builder; the deadline task now sleeps `self.turn_timeout`. Threaded through `SessionConfig.turn_timeout` → `AgentStackSpec` → `build_agent_stack` (applies `with_turn_timeout` iff `Some`), and the binary's `SessionConfig` maps `turn_timeout_secs` → `Duration`. Updated 5 e2e `SessionConfig` literals (`turn_timeout: None`) + the binary's `AivyxConfig` destructure. Tests: config round-trip + default-`None`; agent honors a 5s override via virtual-time (`TimedOut` fires well under the 120s default). Default-unset → 120s const, byte-identical. core (564) + config (355) + channel suites green; clippy clean. |
| **BR.5** ✅ | **Wiring + live re-verify** | DONE — re-ran the exact Stencil ST.4 scenario (Qwen3-4B-Instruct-2507, constrained, CPU) with breaker + preamble + BR.4 timeout all live. **Result: the model now finishes cleanly.** It emitted one real `fs.read {"path":"probe.txt"}`, then answered in plain text via the `respond` sentinel (*"The secret word in probe.txt is GRIMALKIN."*) and ended the turn — `[turn completed]`. Audit chain: **exactly one** `ToolCall` (`fs.read:…`) and `TurnEnded` outcome `Completed` — versus ST.4's ~18 repeated `memory.write`s to the wall cap. **BR.3's preamble fixed the root cause** (the model chose `respond`); BR.2's breaker wasn't needed live (unit-proven safety net); **BR.4's config knob ran the slow CPU turn with `[agent] turn_timeout_secs = 1800` — no `TURN_TIMEOUT` const hack** (the const is untouched, unlike ST.4). One incident: the on-disk GGUF had silently corrupted between sessions (EIO on full read) — re-downloaded + md5-verified before the run; not a code issue. |
| **BR.6** ✅ | **Finalize** | DONE. Full workspace suite green (93 `test result: ok`, 0 failed); `cargo clippy --workspace --all-targets` clean (+ `-p aivyx-llm --features provider-mistral-rs` clean, 10 provider tests); `cargo deny check licenses advisories` ok (no new deps). `stencil-st4-findings` memory's loop finding flipped to resolved; Bridle chapter memory written; status → COMPLETE. CUDA 13.x (watch-item D) remains open upstream. |

**Discipline:** BR.1 lands the outcome plumbing first (pure, no behavior), so BR.2's
breaker is a thin policy over a proven variant. A and C are independent
default-safe knobs; B is intrinsic to constrained mode. Test band: **moderate** —
dense in BR.2 (the consecutive-identical state machine + reset semantics) and BR.4
(config round-trip + deadline plumbing); price ~25–35 new tests.

## 5. Open questions (resolve in-phase)

- **Breaker default `N` (BR.2)** — `3` is the proposed default. Too low risks
  cutting a *legitimate* retry-after-transient-error pattern (but those usually
  differ in input or interleave a different call); too high wastes budget. Confirm
  `3` against the ST.4 trace; make it the knob's default, not a const.
- **What `input` hash to compare (BR.2)** — hash the *canonical* tool input
  (post-injection of `session`/`role_prefix`? or pre-injection?). Pre-injection is
  the model's actual intent and the right signal; confirm the injected keys don't
  defeat equality.
- **Synthesized `final_message` wording (BR.2)** — surface the model's accumulated
  text if any; else a terse "stopped after repeated identical tool calls" so the
  channel shows *something*. Keep it operator-legible, not a stack-trace.
- **Preamble vs. few-shot (BR.3)** — a one-line instruction is the minimal fix; if
  the 4B still won't use `respond` in BR.5, a single worked example may be needed
  (escalate only if observed, per the substrate-phase lesson that examples are the
  next lever after instructions).

---

*Chapter Bridle is the harness that makes Stencil's primitive usable in the wild: a
grammar that forces a valid tool call is necessary but not sufficient if the model
then can't stop calling it. The repeat-call breaker is a generalizable safety net
for any local model; the `respond` preamble teaches the constrained model how to
finish; the configurable timeout lets a slow-but-correct local backend complete.
Together they turn "the small model can call a tool" into "the small model can run a
turn to completion" — the difference between a demo and a daily driver.*
