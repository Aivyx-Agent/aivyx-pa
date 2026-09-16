# Headless Execution Mode — unattended, no-operator runs (Chapter H)

> **Status:** design contract. This is the spec Chapter H scaffolds from
> (mirrors `docs/DAEMON_TEAMS.md` / `docs/WEB_MISSION_CONTROL.md`).
>
> Today every approval point in Aivyx PA assumes a **human is reachable**: a
> tool that returns `RequiresEscalation` **inside a team mission** parks the
> turn behind an operator gate and *waits* (the daemon emits `ApprovalGate`,
> the operator answers `ResolveGate`); a team mission's `GateMode::Human` step
> pauses as `AwaitingApproval` until someone resolves it. A plain
> single-agent turn (no mission wrapping it) has no such park-and-wait today
> — see the "Single-agent escalation" bullet below for the precise, narrower
> claim (2026-09-16 security audit fix, Task 4's final review — corrected
> from this doc's original overstatement). That mission-scoped case is
> correct for interactive use and wrong for
> **unattended** use — a batch job, a cron-triggered run, or a fully-autonomous
> agent — where blocking on an absent human means hanging forever.
>
> **Headless mode** makes a run explicitly **non-interactive**: it does
> everything it can *without* a human, and at any approval point it **does not
> wait** — it applies a safe policy and moves on. This is the unattended
> counterpart to the interactive turn loop, sharing the same capability stack
> and HMAC audit chain.

---

## 0. Decisions (locked at scope time)

| Decision | Choice | Why |
|---|---|---|
| Gate posture (v1) | **Reject-and-abort** | At ANY gate (single-agent escalation or team human gate) headless stops that branch, records why, and moves on. No turn-resume machinery; no risk of auto-approving the very actions a tool asked a human about. Configurable **auto-approve** is a deliberate later increment, not v1. |
| Confirm-first / irreversible tools | **Always blocked** | Money/outbound/irreversible tools (`kitchen.order.send`, email send, the `confirmed:true` pattern) can **never** auto-proceed unattended — headless fails them with a clear "needs a human" reason, regardless of any future gate policy. The hard safety line. |
| Where it's set | **Per-run opt-in** (default = interactive) | A run *chooses* to be headless (a CLI flag / IPC field / the operator-absent drivers set it). The daemon's interactive turn path is byte-for-byte unchanged when headless is off. |
| Safety substrate | **Unchanged** | Headless rides the same capability attenuation (NT-02), per-call dollar/token budget gates, and the one HMAC audit chain. It *removes* the human, it never *widens* authority. |

---

## 1. What exists today

- **Single-agent escalation.** A tool returns `ToolOutcome::RequiresEscalation
  { reason }` (`aivyx-core/src/lib.rs`); the agent turn loop
  (`aivyx-core/src/agent.rs`) breaks and yields `TurnOutcome::Escalated {
  reason }`. **Corrected 2026-09-16 (Task 4's final review — this bullet
  previously overstated the mechanism as universal):** the daemon
  (`daemon_server.rs`, the gate-park block) only **parks** the escalation —
  creates a gate on a `MissionRecord` (`mission::add_gate`), persists it,
  and emits `StreamEventPayload::ApprovalGate` — when the turn is running
  **inside a team mission** (`Some(mission_id)` and `Some(mission_store)`
  both present); only then can the operator answer with
  `FrontendMessage::ResolveGate { mission_id, gate_id, approved }`. A plain
  single-agent turn with no mission wrapping it (the ordinary interactive
  chat/CLI/Telegram/Discord/Slack case) has **no park-and-resume path**
  today: the turn simply finalizes as `Escalated`, gets rendered as a
  one-line marker, and the specific paused tool call cannot be re-approved
  and replayed — the operator's only recourse is to re-issue the request
  after changing the gating posture. This is a known, fail-safe (blocks
  the action, never lets it through) limitation — see
  `docs/ACCESS_LEVELS.md`'s "single-agent gate-resume machinery Chapter H
  deferred" note and `docs/SECURITY_POSTURE.md`'s attended/unattended
  section for the full writeup. Building that resume machinery remains
  future work, not something this correction implements.
- **Team-mission human gates (Chapter L).** `StepKind::Gate { mode:
  GateMode::Human }`; `TeamRuntime::run_until_pause` returns
  `RunYield::AwaitingHuman { step, .. }`; the daemon marks the mission
  `AwaitingApproval` and waits for `ResolveTeamGate`.
- **Confirm-first tools.** Some tools are *deliberately* confirm-first at the
  tool level — `kitchen.order.send` (money leaves the building), the
  `skills.teach confirmed:true` pattern. Unconfirmed, they escalate.
- **The closest precedent — the autonomous loop.** The Phase-173 Ralph loop is
  *already* "fully autonomous, capped, no per-iteration operator gate"
  (`SharedLoopState`, `run_loop_driver`, `decide()` termination). Headless
  generalizes that posture from the loop to **any** run, and gives the loop a
  defined stance for the escalations it can hit mid-iteration.
- **Guardrails already in place.** The per-run dollar/token `BudgetGate`
  (Chapter K), the iteration/wall-clock caps (Phase 174/176), capability
  attenuation, and the audit chain — all of which a headless run still rides.

---

## 2. The crux — reject is cheap, approve is not (so v1 is reject-only)

The interception point is the daemon's handling of `TurnOutcome::Escalated`
(and the team driver's `RunYield::AwaitingHuman`). Two postures are possible:

- **Reject-and-abort** — *don't* create the pending gate / *don't* emit
  `ApprovalGate`; record the escalation's `reason` on the audit chain and end
  that branch (the turn finishes `Escalated`/refused; the team step's
  dependents are skipped, mission ends `Rejected`). **Cheap and total** — it
  reuses the existing terminal paths; nothing new to resume.
- **Auto-approve** — proceed as if the operator approved. For the team path
  this is nearly free (insert the gate's pass into the checkpoint, re-drive —
  the L.4 resume machinery already exists). But for the **single-agent** path
  the turn has *already broken* at the escalating tool call; auto-approving
  means **re-running the turn with that action pre-authorized**, which needs
  new turn-resume plumbing — and is precisely the risky case (auto-approving a
  tool that explicitly asked for a human).

So v1 is **reject-only**. The policy type leaves room for `AutoApprove` later,
but Chapter H does not build the single-agent resume path, and **never**
auto-approves a confirm-first/irreversible tool even if that increment lands.

---

## 3. Policy model

```
/// How an unattended run treats an approval point. (aivyx-core or a small
/// shared crate — pure data, no behavior.)
enum GatePolicy {
    /// Interactive: park + wait for the operator (today's behavior). Default.
    Interactive,
    /// Headless v1: never wait — record the reason and abort that branch.
    RejectAndAbort,
    // Future: AutoApprove { except_confirm_first: true } — NOT in v1.
}
```

- A run carries a `GatePolicy` (default `Interactive`). `RejectAndAbort` is
  "headless."
- **Confirm-first / irreversible tools are blocked under *any* non-interactive
  policy**, independent of the gate posture — a structural invariant, not a
  policy knob (so a future `AutoApprove` can't accidentally arm them).
- Headless changes **only** what happens at a gate. Capability checks, budget
  gates, caps, and the audit chain are untouched.

---

## 4. Where it's wired

- **Turn path.** The daemon turn handler consults the run's `GatePolicy`. When
  `RejectAndAbort` and a turn returns `TurnOutcome::Escalated`, it skips the
  `add_gate` + `ApprovalGate` emit and instead records the refusal (audit
  event) and finalizes the turn — no `mission` gate, no wait.
- **Team path.** A headless team run maps a `RunYield::AwaitingHuman` to a
  rejection: the mission goes `Rejected` (reason = the gate's criteria), the
  gate's dependents never run, partial outputs preserved — the same shape as an
  operator reject, just decided by policy. (`team_mission_driver` consults the
  policy instead of marking `AwaitingApproval`.)
- **Operator-absent drivers adopt it by default.** The autonomous loop,
  cron-`schedule` runs, and webhook/file-watch trigger runs have *no* operator
  by construction — they default to the headless policy (today they'd hang on
  an escalation). Interactive channels (REPL/TUI/web/Telegram/…) stay
  `Interactive`.
- **Explicit opt-in for interactive launchers.** `aivyx-pa --headless "<task>"` (a
  one-shot unattended run) and an IPC/`SubmitInput`-adjacent field for a client
  that wants an unattended turn.

---

## 5. Phase plan

| Phase | Deliverable |
|---|---|
| **H.0** | This design contract. |
| **H.1** | The `GatePolicy` type (+ the confirm-first/irreversible "always blocked when non-interactive" invariant) in a shared location; threaded as an optional run parameter, defaulting to `Interactive` so all current behavior is byte-for-byte unchanged. |
| **H.2** | Single-agent turn path: the daemon honors `RejectAndAbort` on `TurnOutcome::Escalated` — record the refusal on the audit chain, finalize, no gate/wait. Tests over the escalation path. |
| **H.3** | Team-mission path: a headless team run maps `AwaitingHuman` → `Rejected` (recorded reason), reusing the L.4 terminal path; `team_mission_driver` consults the policy. |
| **H.4** | Operator-absent drivers (autonomous loop, schedules, webhook/file-watch triggers) default to headless; interactive channels stay interactive. The loop's mid-iteration escalations now resolve by policy instead of hanging. |
| **H.5** | Surfaces: `aivyx-pa --headless` one-shot + the IPC field for an unattended turn; surface the chosen policy in status/reporting. |
| **H.6** | Audit + observability: every policy-driven rejection is a clear, queryable audit event (who/what/why-refused); a "headless run summary" (what completed, what was refused-for-a-human). |

~6 phases, smaller than L/M — it's a policy + a handful of interception points, not a new subsystem.

**Status: H.0–H.6 complete.** H.6 added a dedicated `AuditEvent::HeadlessRefusal { run_id, surface, reason }` (with a `HeadlessSurfaceSummary` of `AgentTurn` / `TeamMission { step }` / `Trigger { trigger_kind }`) emitted at all three refusal points — the single-agent turn (`daemon_server`) and trigger (`trigger.rs`) append it directly to the `PersistentAuditLog`; the team driver, which holds only an `Arc<dyn AuditHook>`, emits an `AuditTag::HeadlessRefusal` that the bridge maps onto the `TeamMission` surface. Each refusal also logs a one-line operator-readable summary to stderr. The event is queryable via `aivyx-pa audit export --event-type HeadlessRefusal`.

**Follow-on (a) shipped — the `aivyx-pa --headless "<task>"` CLI one-shot.** It
connects to a **running daemon** (no in-process fallback — headless relies on
the daemon's gate interception), submits one turn via `submit_input_headless`
(the per-run `headless: true` IPC field from H.5), streams the output through
the shared `render_for_cli`, and maps the turn's terminal outcome onto a
**process exit code** so cron/batch/autonomous callers can branch: `0`
completed, `3` refused-at-a-gate (the distinct headless-refusal code), `1` any
other non-completion. No daemon running → a clear "start `aivyx-pa daemon run`
first" error. No new base/P10/tool/dep; byte-identical when the flag is absent.

Remaining (deferred, not blocking): team.run-from-loop per-call headless
threading (needs the policy in `ToolContext` — an `aivyx-core` change), and an
*aggregate* "headless run summary" roll-up across a multi-turn run (the exit
code is the per-run summary today, not a cross-turn roll-up).

---

## 6. Invariants

- **Default is interactive.** With no `GatePolicy` set, every path behaves
  exactly as today; headless is strictly opt-in.
- **Never auto-approve in v1.** Headless only ever *refuses* at a gate. It does
  not proceed past one.
- **Confirm-first/irreversible never auto-runs.** Money/outbound/irreversible
  tools fail-with-reason under any non-interactive policy — a structural line,
  not a tunable.
- **No authority widening.** Headless removes the human; it does not grant
  scopes, raise trust tiers, or bypass the budget gate. Same capability math,
  same HMAC chain.
- **Legible refusals.** Every headless rejection lands on the audit chain with
  the escalation's reason, so an operator reviewing later sees exactly what was
  declined and why — the autonomous-action-must-stay-legible posture (Phase 78)
  extended to the unattended path.

---

## 7. Open questions (resolve in-phase, not blocking H.0)

- **Where the per-run `GatePolicy` literally lives** — `aivyx-core` (next to
  the turn loop) vs a small shared crate. Decide in H.1 by what imports it.
- **`AutoApprove` later** — if/when it lands, the single-agent turn-resume
  design + the explicit confirm-first exclusion are its own scope.
- **Per-channel default config** — whether the operator can declare a channel
  (e.g. a specific webhook) interactive-vs-headless in `aivyx-pa.toml`, beyond the
  built-in "operator-absent drivers default to headless."
