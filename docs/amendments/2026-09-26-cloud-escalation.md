# Amendment A15 — Consent-Gated Cloud Escalation Under G6

**Date:** 2026-09-26
**Phase:** Model routing Part 3b (cloud escalation + sensitivity taint).
**Extends:** **G6** (Local execution, cloud inference, privacy
non-negotiable) and **N5** (No inference-side compromises for
convenience). Neither is narrowed or relaxed: G6's privacy clause and
every N5 prohibition apply to the mechanism added here exactly as they
apply to the rest of Aivyx PA. No PRODUCT.md or DESIGN.md text is
superseded.
**Implementing phase:** Model routing Part 3b. This amendment lands
*before* the code it authorizes — the governance gate precedes the
feature, never trails it (the A13 / FG.2 precedent).
**Reference design:** `aivyx-ecosystem/docs/superpowers/specs/2026-09-25-model-routing-design.md`,
section "Part 3 only — Cloud escalation and sensitivity".

---

## What changed

G6 already permits cloud inference: an operator may point the `[agent]`
provider at a cloud LLM provider, using the operator's own API key, and
every call then goes to that provider. That is a whole-install choice,
made once in configuration.

Model routing Part 3a added task-aware routing across **local**
endpoints (`[routing.endpoints]`), with cloud endpoint kinds rejected at
startup. A15 authorizes one new thing: a **local-first** configuration
may send a **specific conversation's call** to a **cloud endpoint the
operator has configured** in `[routing.endpoints]`, when an escalation
trigger fires and the consent gate passes. This is *cloud escalation*.

The mechanism, as authorized:

- **Triggers.** Exactly two in this release, each toggleable under
  `[routing.escalation]`:
  - **`no_local_candidate`** (default on) — local routing found no
    model able to serve the call.
  - **`tiers`** (default empty) — task kinds the operator has listed
    route straight to cloud (e.g. `tiers = ["plan"]`).
- **Modes.** `[routing.escalation] mode = "never" | "ask" | "auto"`,
  default **`ask`**, shared by both triggers.
  - `never` — cloud endpoints are never used; configuring a cloud
    endpoint kind in `[routing.endpoints]` while `mode = "never"` is a
    startup error.
  - `ask` — **stop-and-allow**. When a trigger fires and the
    conversation has no consent grant, the turn stops without sending
    anything to the cloud and tells the operator which cloud model would
    be used, which trigger fired, and the approximate outbound token
    count. The operator may then allow cloud escalation for that one
    conversation by an explicit action (a chat command, an IPC request,
    or a CLI command) and resend. Nothing is sent before that grant
    exists.
  - `auto` — escalation proceeds without a per-conversation prompt,
    subject to every rule below. `auto` is itself the operator's consent,
    given in configuration.
- **Sensitivity taint.** A conversation becomes *tainted* when sensitive
  data enters it: output from a tool matching a configured sensitive
  prefix (`[routing.sensitive] tool_prefixes`), content injected by a
  context provider that reports itself sensitive (memory recall does),
  or an inbound message on a channel listed as sensitive
  (`[routing.sensitive] channels`). A tainted conversation never
  escalates.
- **Defaults change nothing.** With no cloud endpoint in
  `[routing.endpoints]`, there is no escalation target: behaviour,
  outputs, audit entries and tool catalog are identical to Part 3a.
  `[routing.escalation]` and `[routing.sensitive]` may be absent; their
  defaults only take effect once the operator adds a cloud endpoint.

---

## Why

Part 3a made local-first routing practical: an operator can run several
local models and let each call go to the one that fits. What it cannot
do is serve a call no local model can handle, or deliberately send a
narrow class of work (such as planning) to a stronger model. Today the
only answer is to switch the whole install to a cloud `[agent]`
provider — which sends *every* conversation, including ones full of
email, calendar and memory content, to the cloud.

Per-conversation escalation is the more private option, not the less
private one: cloud is used only where the operator asked for it, only
for conversations that have not touched sensitive data, and never
silently. G6 says privacy is "never compromised in the name of a
feature". A15 exists so that this feature is bounded by written rules
before a line of it is built, rather than by whatever the implementation
happens to do.

---

## The rules that keep G6 intact

These six rules are part of the contract. Each is covered by tests in
the implementing phase; weakening any of them requires a further
amendment.

1. **A tainted conversation never escalates, in any mode.** `auto` does
   not bypass taint. There is no per-conversation override. When a
   trigger fires on a tainted conversation, the call falls back to the
   best local model, or fails with a clear error if there is none. The
   block is audited, and when no local model can serve the call, the
   reason is surfaced in the error.
2. **Taint is persisted and never cleared for that session.** It
   survives a daemon restart and survives compaction. It is recorded on
   the conversation, deliberately *not* re-derived from current history,
   because a compaction summary can carry sensitive content after the
   tool call that produced it is gone.
3. **Calls without a conversation session never escalate.** Judges,
   mission planning, and any other call made outside a conversation
   session have no taint record, so their taint cannot be known; they
   are never sent to a cloud escalation endpoint.
4. **Escalation goes only to endpoints the operator configured, with
   the operator's own keys.** A cloud endpoint exists only if the
   operator wrote it into `[routing.endpoints]`, and it authenticates
   with the operator's existing provider key (a missing key is a
   startup error). Aivyx PA adds no default, built-in, or discovered
   cloud destination.
5. **Every escalation decision other than "no escalation needed" writes
   an audit entry** (for example: allowed, awaiting consent, or
   blocked), recording the model (where one was chosen), trigger, mode
   and outcome, and **a hash of the outbound payload, never its
   content.**
6. **Consent grants are in-memory only and per conversation.** A grant
   covers one conversation; it is never written to disk, so a restart
   re-asks. It never carries over to another conversation.

N5 is unchanged and applies in full: **no Aivyx-hosted component** sits
in the escalation path, the operator's key is **never proxied**, prompts
are never batched or stored on Aivyx servers (there are none in the
path), and there is **no telemetry**. What the cloud provider itself
retains is governed by that provider's own terms with the operator.
An escalated call goes from the operator's machine directly to the
provider the operator configured, exactly as a cloud `[agent]` call
already does under G6.

---

## What this does not say

- **It does not change DESIGN.md's per-turn capability rule.** Effective
  capabilities are still computed once per turn and not changed
  mid-turn; "mid-turn capability changes (e.g., user-confirmed
  escalation) are NOT supported in v1" still holds. Stop-and-allow is
  built to respect it: the turn that needs consent *ends*, the grant is
  recorded between turns, and the operator's resend starts a new turn.
  Cloud escalation also grants no capability scope; it changes which
  model serves a call, not what the agent may do.
- **It does not authorize `on_failure` escalation.** The design's third
  trigger — escalating a retry after a local attempt demonstrably fails
  — is deferred. There is no `on_failure` key; an unknown key under
  `[routing.escalation]` is a configuration error, so a typo cannot
  silently weaken a privacy setting. Adding `on_failure` requires a
  further amendment.
- **It does not change the `[agent]` provider.** If the operator's
  default provider is already a cloud provider, routing may pick other
  models on that same provider without escalation, as in Part 3a — no
  new destination is involved. Escalation concerns only cloud endpoints
  in `[routing.endpoints]`.
- **It does not offer a way to un-taint a conversation**, or to
  escalate a tainted one "just this once". A later amendment could add
  such an override; this one deliberately does not.
- **It does not promise that sensitivity detection is complete.** Taint
  comes from configured tool prefixes, sensitive context providers and
  sensitive channels. Data the operator types into a conversation
  directly is not classified. The operator controls both the prefix list
  and whether any cloud endpoint exists at all; `mode = "never"`, or no
  cloud endpoint, keeps every call local.
