# Aivyx PA Roadmap

A living list of planned phases beyond the one currently active.

**Current release: v0.19.4 (2026-11-01).** Finished phases and chapters
(Phases 13 to 208, Chapters A to N, Picket and Nonagon) are archived in
[`history/ROADMAP-phases-to-2026-10-28.md`](history/ROADMAP-phases-to-2026-10-28.md);
each closed phase also has its own doc in [`archive/phases/`](archive/phases/).

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
- **P19 (part) — pa's provider layer never reads `reasoning_content`.** pa now
  says what ran instead of "try again", but the empty final message itself is
  most likely pa's gap: a thinking model (the 9B) can end a post-tool step with
  its answer only in `reasoning_content` and an empty `content`, and
  `aivyx-llm` drops the former. aivyx-coder fixed the same thing in its audit 3
  ("reasoning-only replies"); port that handling to `aivyx-llm`. The floor
  fires only on an empty or bare-object reply, so there is no off-by-one in
  how the final reply is detected.
- **Destructive-shell detector: `GIT_DIR`, `HOME`, `XDG_CONFIG_HOME` before git,
  and `git -C <dir>` / `--git-dir=`.** They point git at a config or repo the
  agent may have written; pre-existing and not fixed in 10b (the detector is a
  seatbelt, not a sandbox).
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


## Active phase and planned work

No numbered phase is open as of v0.19.4; the last closed phase is
[Phase 208](archive/phases/PHASE_208.md). What is planned or on the shelf:

- **Channel Activation Milestone** (above), scheduled after the phase
  sequence settles.
- **Distribution.** The old "cut `v0.1.0`" bookend is superseded: releases
  ship through the release workflow (see `CHANGELOG.md`).
- **Studio polish.** The one item from Chapter I's original wording still
  unscoped.
- **Everyday-PA breadth** (weather, contacts, news) and the dependency-
  requiring substrate hardenings, picked by operator pressure at the next
  phase entry.
