# Supervised batching — design

Date: 2026-10-07. Status: approved in conversation. Second of two specs
(the first, per-area autonomy, shipped in v0.16.0).

## Goal

At the `supervised` autonomy level, an unattended run that reaches a step
needing approval **parks that exact call for later review** instead of
refusing it. You review the parked steps together — in the Studio or the
terminal — and an approved step then runs exactly as parked.

## Where it applies

- **Unattended runs:** routines (`[[schedule]]`), webhooks, file watches,
  the autonomous loop, team missions — every run whose gate policy is
  `RejectAndAbort` today.
- **Per area:** a call parks if its area's level (per-area autonomy:
  `[[autonomy.override]]`, else the global level) is `supervised`. In any
  other area it is refused, exactly as today.
- **Which calls:** the ones that already escalate for approval — deletes and
  overwrites (`confirm_destructive`), integration writes (`email.send`,
  `drive.write`, …), git commits, any tool that returns
  `RequiresEscalation`. Nothing new becomes gated.
- Attended conversations are unchanged (they ask at once).

## Parking

- The turn loop's existing approval hook (`ChannelContext::request_approval`,
  used by chat approvals) gains a new answer, `Approval::Parked { id }`.
  `ApprovalRequest` gains the call's capability scope base, so a channel
  can tell the area.
- The unattended run's channel context answers `Parked` for a call in a
  `supervised` area after writing the call to the queue, and `Unavailable`
  (today's refusal) otherwise.
- The model's tool result: "This step needs the operator's approval, so it
  was parked for review (id `<id>`) and not taken. Don't rely on it having
  happened." The turn carries on.
- Audit: `StepParked { id, tool, summary, area, origin }`.

## The queue

- A new encrypted storage domain, `ParkedSteps`. One record per step:
  `id`, `tool`, `input` (the exact arguments, with any model-supplied
  `confirmed` removed), `summary`, `reason`, `area`, `origin` (which run:
  routine name / webhook / file watch / loop story / mission + step),
  `parked_at`, and `state`: `pending | approved | denied | lapsed | failed`
  with `resolved_at` and a short `result`.
- Survives restarts. Resolved entries stay for the record (the newest 200
  are kept).

## Review

- **Studio:** the Command Center's "Needs you" lists each pending parked
  step — summary, which run parked it, how long ago, and a **fresh preview**
  of the current state (for a file write or delete: the file as it is now;
  otherwise the arguments) — with **Approve** and **Deny**. Resolved ones
  appear in the logbook ("Since you were last here").
- **Terminal:** `aivyx-pa review` lists pending steps; `aivyx-pa review
  approve <id>` and `aivyx-pa review deny <id>` resolve one (approve shows
  the preview and asks `[y/N]` unless `--yes`). A new `help.rs` entry and
  CLI-reference section (the drift test requires it).
- **IPC:** `GetParkedSteps` / `ResolveParkedStep { id, approve }` queries;
  the briefing (`GetBriefing`) includes pending parked steps in "Needs you".
- **Notification:** when a step parks and a default notify target exists,
  it gets one line ("A routine parked a step for your review: …").
  Rate-limited by the existing per-target limits.

## Approving

- The daemon runs the parked call **once**, operator-approved, through a new
  `ConcreteAgent::run_approved_call(tool_name, input, origin)` built on the
  existing `run_tool_call` (capability check → audit → execute → audit) with
  `operator_approved: true`, under the daemon agent's current role and
  capabilities. No model is involved.
- The result (success, or the tool's error) is stored on the entry and
  shown. Audit: `ParkedStepResolved { id, outcome }`.
- If the tool no longer exists or the capability isn't granted any more, the
  entry becomes `failed` with that reason; nothing runs.

## Expiry

- `[autonomy] review_expiry_days` (default 7). A pending entry older than
  that becomes `lapsed` (audited, never run). Checked when the queue is read
  and by the daemon's existing periodic tick.

## Errors

- Resolving an id that isn't pending (already resolved, lapsed, unknown) is
  a clear error; nothing runs twice.
- A parking write that fails (storage error) falls back to today's refusal,
  with the reason in the audit log — never a silent drop or an unparked run.

## Testing

- Core: `Approval::Parked` maps to the "parked" tool result and the turn
  continues; `ApprovalRequest` carries the scope base.
- Channel: the unattended context parks only in `supervised` areas and
  refuses elsewhere; storage failure falls back to refusal.
- Store: round-trip, resolve-once, expiry, retention cap.
- `run_approved_call`: runs once with `operator_approved`, denies a missing
  capability, audits.
- End to end: a routine at `supervised` parks an `fs.delete`, the file still
  exists, approving deletes it; denying leaves it.
- CLI: `review` list/approve/deny; the help-table drift test.
- Studio: the "Needs you" card renders (wasm compile + render test).
- No `supervised` area anywhere ⇒ behaviour unchanged.

## Out of scope

Approving from chat apps, editing a parked call before approving, batching
in attended conversations, the expert escape hatch.
