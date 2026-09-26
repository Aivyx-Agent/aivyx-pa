# Amendment A3 — Capability Taxonomy Growth

**Date:** 2026-04-17
**Phase:** 22
**Supersedes:** Extends D4 (Capability Taxonomy). No text
removed — additive only.
**Implementing phases:** 11, 14, 21

---

## What changed

D4's v1 active namespace listed 21 scopes across 7 families
(fs, net, shell, llm, memory, channel, audit, config). Phases
11, 14, and 21 added two new families and grew the total to
**23 known bases**. This amendment documents the full current
inventory and the infrastructure-vs-substrate distinction that
governs which bases count against P10's seven-tool cap.

---

## Full capability base inventory (23 bases)

### Substrate bases (D4 original, unchanged)

These are the bases that gate the seven substrate tools from
P10 and the existing platform primitives.

| Family | Base | Qualifier kind | Added |
|---|---|---|---|
| fs | `fs.read` | path glob | Phase 0 |
| fs | `fs.write` | path glob | Phase 0 |
| fs | `fs.delete` | path glob | Phase 0 |
| fs | `fs.metadata` | path glob | Phase 0 |
| net | `net.fetch` | URL prefix | Phase 0 |
| net | `net.post` | URL prefix | Phase 0 |
| net | `net.dns` | domain glob | Phase 0 |
| shell | `shell.exec` | command allowlist | Phase 0 |
| shell | `shell.spawn` | command allowlist | Phase 0 |
| llm | `llm.call` | model name glob | Phase 0 |
| llm | `llm.embed` | model name glob | Phase 0 |
| memory | `memory.read` | session/topic glob | Phase 0 |
| memory | `memory.write` | session/topic glob | Phase 0 |
| memory | `memory.forget` | session/topic glob | Phase 0 |
| channel | `channel.send` | channel name glob | Phase 0 |
| channel | `channel.receive` | channel name glob | Phase 0 |
| audit | `audit.read` | event type glob | Phase 0 |
| config | `config.read` | key glob | Phase 0 |
| config | `config.write` | key glob | Phase 0 |

### Infrastructure bases (post-Phase-0 additions)

Infrastructure bases gate tools the agent uses to manage
itself per P10's infrastructure tool taxonomy. They are not
counted against the seven-tool cap.

| Family | Base | Qualifier kind | Added | Purpose |
|---|---|---|---|---|
| tool | `tool.allowlist` | *(synthetic)* | Phase 11 | Role-level tool allowlist enforcement |
| role | `role.switch` | role name (SimpleGlob) | Phase 14 | Sub-agent role switching per P1 |
| mission | `mission.create` | *(none yet)* | Phase 21 | Mission lifecycle initiation per P2 |
| mission | `mission.gate` | *(none yet)* | Phase 21 | Approval gate resolution per P2 |

### Qualifier dispatch — `QualifierKind`

D4 described four qualifier semantics (path glob, URL prefix,
allowlist, simple glob). The `QualifierKind` enum implements
these as dispatch arms:

```
QualifierKind::of(base, qualifier) -> {
    PathGlob    — base starts with "fs." or qualifier contains "/"
    UrlPrefix   — qualifier contains "://"
    Allowlist   — qualifier contains ","
    SimpleGlob  — fallback for all other qualifiers
}
```

Phase 14's `role.switch` scope uses `SimpleGlob` dispatch for
role-name matching (e.g., `role.switch:researcher`). No new
`QualifierKind` variant was needed — bare identifiers fall
through to exact-string equality as a degenerate glob case.

### Tier ceiling placement for new bases

| Base | Kernel | Trusted | SemiTrusted | Untrusted |
|---|---|---|---|---|
| `tool.allowlist` | granted | granted | granted | denied |
| `role.switch` | granted | granted | denied | denied |
| `mission.create` | granted | granted | conditional (triangle) | denied |
| `mission.gate` | granted | granted | denied | denied |

The tier ceilings are defined in `CEILING_TRUSTED`,
`CEILING_SEMITRUSTED`, and `CEILING_UNTRUSTED` in
`crates/aivyx-capability/src/lib.rs`. `CEILING_KERNEL` is
derived from `KNOWN_BASES` iteration (grants everything).

---

## The three-tier tool taxonomy

PRODUCT.md P10 establishes a three-tier taxonomy that this
amendment makes concrete:

1. **Substrate tools** (7, capped by P10): `fs.read`,
   `fs.write`, `memory.read`, `memory.write`, `memory.forget`,
   `shell.exec`, `web.fetch`. Operator-facing primitives.

2. **Infrastructure tools** (4 and growing): `tool.allowlist`,
   `role.switch`, `mission.create`, `mission.gate`. Machinery
   the agent uses to manage itself. May grow without P10
   amendment as G3 (reflection), G4 (sub-agents), and G5
   (scheduled execution) require.

3. **Third-party tools** (0 today): implemented against the
   SDK contract from P11, distributed per P12. Not yet shipped.

The `KNOWN_BASES` array in `aivyx-capability/src/lib.rs` is
the single registry for all three tiers. `Scope::parse`
rejects any base not in the array at parse time.

---

## Traceability

| Phase | What was added | Commit |
|---|---|---|
| Phase 11 | `tool.allowlist` (role primitive) | `16422e2` |
| Phase 14 | `role.switch` (sub-agent role-switching) | `0d94d32` |
| Phase 21 | `mission.create`, `mission.gate` (mission primitive) | `05cc349` |

---

## Kitchen vertical-pack addendum — `kitchen.*` (2026-06-07)

> *Added with the first Aivyx PA **vertical pack** — Kitchen /
> Back-of-House (see `docs/VERTICAL_PACKS.md`). Three new bases gate
> the `aivyx-kitchen` tool process over the existing KitchenDB:
> `kitchen.read` (inventory / recipe / supplier / PO / alert reads +
> the pure `kitchen.recipe.scale` / `kitchen.par.reorder` compute),
> `kitchen.write` (inventory counts / adjustments), and
> `kitchen.order.send` (dispatching a purchase order to a supplier),
> and `kitchen.haccp.log` (append-only food-safety records — the
> compliance wedge). All Trusted-tier-only at the ceiling, matching
> every other third-party-tool-process surface (`email.*` /
> `web.search` / `drive.*`). `kitchen.order.send` is **additionally
> confirm-first** at the tool level — money leaving the building
> requires the `confirmed: true` protocol the `skills.teach` family
> established. `kitchen.haccp.log` is ungated (logging a fridge temp
> must be friction-free) but immutable: every call lands on the
> tamper-evident HMAC audit chain (tool id, scope, input hash, time,
> outcome). This is the pack model proving out: a vertical adds
> **additive bases + a tool process**, never a substrate fork.*

| Source | Bases added | Provenance |
|---|---|---|
| Kitchen pack | `kitchen.read` | Read + compute tools (KitchenDB RPC reads + the pure recipe-scale / par-reorder compute). |
| Kitchen pack | `kitchen.write` | Inventory counts / adjustments (gated write surface). |
| Kitchen pack | `kitchen.order.send` | Dispatch a purchase order to a supplier — Trusted-tier **and** confirm-first at the tool level. |
| Kitchen pack | `kitchen.haccp.log` | Append-only food-safety (HACCP) records; anchored on the HMAC audit chain (the compliance wedge). |

### Current full enumeration after the Kitchen pack (79 bases)

The Phase 184 enumeration plus four new **infrastructure** bases
(`kitchen.read`, `kitchen.write`, `kitchen.order.send`,
`kitchen.haccp.log`), so the infrastructure family grows 34 → 38:

Total: 16 + 5 + 38 + 20 = 79.

### Nonagon multi-agent chapter addendum (+2 → 81 bases)

The Nonagon chapter (`docs/NONAGON.md`) adds two **infrastructure** bases:

| Chapter | Base | Purpose |
|---------|------|---------|
| Nonagon | `team.delegate` | The lead's authority to convene + delegate to specialists (`delegate_task` / `query_agent`). **Not** inherited by specialists — the lead→specialist attenuation drops it, so a specialist cannot convene its own team. Trusted-tier default. |
| Nonagon | `team.message` | Team dialogue on the message bus (`send_message` / `read_message`). Held by **every** member (lead + specialists) so peers can talk; distinct from `team.delegate`. Trusted-tier default. |

So the infrastructure family grows 38 → 40:

Total: 16 + 5 + 40 + 20 = 81.

### Chapter L (daemon-side teams) addendum (+1 → 82 bases)

Chapter L (`docs/DAEMON_TEAMS.md`) adds one **infrastructure** base:

| Chapter | Base | Purpose |
|---------|------|---------|
| Chapter L (L.7) | `team.run` | Gates the daemon-side `team.run` tool — delegating a free-text goal to a **durable** daemon team mission (decomposed + checkpoint/resume-driven, gate-pausable, shown in the TUI Missions panel). Lets an autonomous-loop iteration hand a large story to a team rather than implementing it single-handed. Distinct from `team.delegate` (the in-assembly lead authority): `team.run` starts a whole mission. Trusted-tier default. |

So the infrastructure family grows 40 → 41:

Total: 16 + 5 + 41 + 20 = 82.

---

## Phase 54 addendum — current scope-base count (2026-05-12)

> *Added at Phase 54 exit during the Chapter A docs sweep. The
> traceability table above stopped at Phase 21 and the
> "12 → 23" headline figure has been wrong for ~30 phases.
> This addendum brings the count current.*

`aivyx-capability::KNOWN_BASES.len() = 43` as of Phase 54 exit.

### What changed since the table above

| Phase | Bases added | Provenance |
|---|---|---|
| Phase 26 | `schedule.create`, `schedule.list`, `schedule.delete`, `schedule.update` | Scheduled execution G5 |
| Phase 27 | `webhook.create`, `webhook.list`, `webhook.delete`, `file_watch.create`, `file_watch.list`, `file_watch.delete` | Webhook + file-watch triggers G5 |
| Phase 23 | `mcp.call` | MCP client adapter |
| Phase 28 | `mission.list`, `mission.status` | Mission read-only inspection (P2 deferral closure) |
| Phase 29 | `reflection.propose`, `reflection.apply` | Reflection loop G3/P8 |
| Phase 30 | `role.update` | Runtime role mutation P8 completion |
| Phase 36 | `ollama.list`, `ollama.show`, `ollama.pull` | Ollama model management |
| Phase 37 | `net.post` | Web post primitive (substrate cap raised from 7→8, A5) |
| Phase 42 | `memory.gc` | Memory garbage collection |

### Current full enumeration

Substrate (8, capped by P10 + A5):
- `fs.read`, `fs.write`, `memory.read`, `memory.write`,
  `memory.forget`, `shell.exec`, `web.fetch`, `web.post`

Other operator-facing scopes (8):
- `fs.delete`, `fs.metadata`, `net.fetch`, `net.dns`,
  `shell.spawn`, `llm.call`, `llm.embed`, `memory.gc`

Channel / audit / config (5):
- `channel.send`, `channel.receive`, `audit.read`,
  `config.read`, `config.write`

Infrastructure tools (22, allowed to grow per P10's three-tier
taxonomy):
- Role primitive: `tool.allowlist`, `role.switch`, `role.update`
- Mission: `mission.create`, `mission.gate`, `mission.list`,
  `mission.status`
- Scheduling: `schedule.create`, `schedule.list`,
  `schedule.delete`, `schedule.update`
- Triggers: `webhook.create`, `webhook.list`, `webhook.delete`,
  `file_watch.create`, `file_watch.list`, `file_watch.delete`
- MCP: `mcp.call`
- Reflection: `reflection.propose`, `reflection.apply`
- Ollama management: `ollama.list`, `ollama.show`, `ollama.pull`

(`turn.history` from Phase 28 reuses `audit.read` rather than
declaring its own base — recorded here so a future reader
doesn't go hunting for it in `KNOWN_BASES`.)

Total: 8 + 8 + 5 + 22 = 43.

### Drift posture

The traceability table above is no longer maintained
per-base — at ~3 bases/phase it became more noise than signal.
The single source of truth is `KNOWN_BASES` in
`aivyx-capability/src/lib.rs`. This addendum is the
backwards-looking reconciliation; future amendments need not
duplicate per-base provenance.

---

## Phase 113 addendum — current scope-base count (2026-05-28)

> *Added at Phase 113 exit during the operator-surface-polish
> deferral-cleanup phase. The Phase 54 addendum (above)
> caught up to 43 bases; six more landed across the Persona,
> Reach, and Chapter D work between Phase 56 and Phase 110.
> This addendum brings the count current and pins the
> auditor-readable provenance.*

`aivyx-capability::KNOWN_BASES.len() = 49` as of Phase 113 exit.

### What changed since the Phase 54 addendum

| Phase | Bases added | Provenance |
|---|---|---|
| Phase 59 | `persona.propose` | Persona Foundation P14 — operator-side proposal-write gate distinct from `reflection.propose` |
| Phase 62 | `notify.send` | Reach: Agent-Initiated Outbound Notifications |
| Phase 109 | `git.read` | A12 — `git.status` + `git.diff` substrate tools (one base, two tools) |
| Phase 110 | `skills.propose` | Skills Auto-Creation — distinct from `persona.propose` so a role granting Persona-edit rights doesn't implicitly grant skill-draft rights |
| Phase 110 | `skills.list` | Read-only enumeration of approved skill set |
| Phase 110 | `skills.invoke` | On-demand procedure rendering of one named skill |

(Phase 113 itself ships no new bases — the deferral-cleanup
phase touches only operator-surface flags + TOML loading + this
addendum. The `[skills.auto_propose]` TOML config introduced in
Phase 113 reuses the existing `persona.propose` and
`skills.propose` bases for the auto-proposer's chain-write
path; no new capability gate.)

## Phase 123 addendum — Chapter F #1 Gmail (2026-05-30)

> *Added at Phase 123 exit. Chapter F (External Productivity
> Integrations) opens with Gmail as the first integration;
> per P10/P11/P12 it ships as a third-party tool process,
> NOT as substrate. The three new bases below gate the four
> Gmail tools (`gmail.search`, `gmail.read`, `gmail.draft`,
> `gmail.send`) that the `aivyx-gmail` tool process registers.
> All three Trusted-tier-only by default (mirrors
> `shell.exec` / `notify.send` gating per Phase 62 Q2(a)).*

| Phase | Bases added | Provenance |
|---|---|---|
| Phase 123 | `email.read` | Chapter F #1 — `gmail.search` + `gmail.read` (read-only inbox + message access via the Gmail third-party tool process) |
| Phase 123 | `email.write` | Chapter F #1 — `gmail.draft` (creates a Gmail draft; safe write — requires explicit Gmail-UI send) |
| Phase 123 | `email.send` | Chapter F #1 — `gmail.send` (direct send; Trusted-tier only at the ceiling level, matching `shell.exec` / `notify.send`) |

## Phase 125 addendum — Chapter G #1 personal assistant tool bundle (2026-05-31)

> *Added at Phase 125 exit. Chapter G (Operator-Facing Personal
> Assistant Capabilities) opens with the aivyx-toolkit bundle
> as the first integration. Per P10/P11/P12 the entire bundle
> ships as a single third-party tool process registering eight
> tools across three categories. The five new bases below gate
> those tools. All five Trusted-tier-only by default — same
> gating pattern as `email.*` and `notify.send` per Phase 62
> Q2(a): personal-assistant tools shouldn't be reachable from
> remote channels without explicit role grant.*

| Phase | Bases added | Provenance |
|---|---|---|
| Phase 125 | `web.search` | Chapter G #1 — `web.search` tool (Brave Search API; operator-provided key; distinct from `net.fetch` because the search API is a credentialed external service, not generic HTTP fetch) |
| Phase 125 | `task.read` | Chapter G #1 — `task.list` (lightweight TODO listing) |
| Phase 125 | `task.write` | Chapter G #1 — `task.create`, `task.complete`, `task.delete` (TODO CRUD; separate from `task.read` so a read-only role can browse tasks without write authority) |
| Phase 125 | `health.read` | Chapter G #1 — `health.check.list`, `health.check.recent_changes` (URL monitor state inspection) |
| Phase 125 | `health.write` | Chapter G #1 — `health.check.add` (register new URL watcher for the polling loop) |

## Phase 130 addendum (Task 2) — Chapter F #5 Notion (2026-06-01)

> *Added at Phase 130 Task 2 (Notion crate skeleton).
> Chapter F's fifth integration — Notion via the
> `aivyx-notion` third-party tool process. First non-Google
> + first non-OAuth Chapter F integration; uses Notion's
> Integration token (bearer-token) auth. Per Phase 130 Q1a,
> seven tools ship: `notion.search`, `notion.get_page`,
> `notion.list_database`, `notion.create_page`,
> `notion.append_blocks`, `notion.update_page_properties`,
> `notion.archive_page`. Two new bases gate them.
>
> The Obsidian bases (`obsidian.read`, `obsidian.write`)
> land at Phase 130 Task 10 and will extend this addendum
> with an additional row.*

| Phase | Bases added | Provenance |
|---|---|---|
| Phase 130 | `notion.read` | Chapter F #5 — `notion.search`, `notion.get_page`, `notion.list_database` against the operator's shared Notion content |
| Phase 130 | `notion.write` | Chapter F #5 — `notion.create_page`, `notion.append_blocks`, `notion.update_page_properties`, `notion.archive_page` (full page-lifecycle mutation; Trusted-tier-only at the ceiling level, matching `email.send` / `calendar.write` / `drive.write`) |

### Current enumeration after Task 2 (63 bases — superseded by Task 10 enumeration below)

### Phase 130 Task 10 addendum — Chapter F #6 Obsidian (2026-06-01)

> *Added at Phase 130 Task 10 (Obsidian skeleton).
> Chapter F's sixth integration — Obsidian vault via the
> `aivyx-obsidian` third-party tool process. First Chapter F
> integration with no external API; operates on filesystem
> reads/writes under a configured vault directory with
> load-bearing path-traversal protection. Per Phase 130 Q2a,
> six tools ship: `obsidian.search`, `obsidian.get_note`,
> `obsidian.list_folder`, `obsidian.create_note`,
> `obsidian.update_note`, `obsidian.delete_note`. Two new
> bases gate them.*

| Phase | Bases added | Provenance |
|---|---|---|
| Phase 130 | `obsidian.read` | Chapter F #6 — `obsidian.search`, `obsidian.get_note`, `obsidian.list_folder` against the operator's configured vault |
| Phase 130 | `obsidian.write` | Chapter F #6 — `obsidian.create_note`, `obsidian.update_note`, `obsidian.delete_note` (Trusted-tier-only at the ceiling level, matching the Chapter F write-tool gating pattern) |

### Current enumeration after Phase 130 (65 bases — superseded by Phase 131 enumeration below)

### Phase 131 addendum — Chapter F #7 n8n (2026-06-01)

> *Added at Phase 131 Task 2 (n8n skeleton). Chapter F's
> seventh integration — n8n workflow automation via the
> `aivyx-n8n` third-party tool process. First Chapter F
> integration with an **operator-supplied base URL**
> (self-hosted n8n instances are the norm); the crate
> constructs every request as
> `{n8n_base_url}/api/v1/<resource>` and authenticates with
> the n8n-specific `X-N8N-API-KEY` header (not Bearer). Per
> Phase 131 Q1c (operator-picked over the Recommended
> Q1b 7-tool default), ten tools ship: read surface
> `n8n.list_workflows`, `n8n.get_workflow`,
> `n8n.list_executions`, `n8n.get_execution`; lifecycle
> writes `n8n.execute_workflow`, `n8n.activate_workflow`,
> `n8n.deactivate_workflow`; and CRUD writes
> `n8n.create_workflow`, `n8n.update_workflow`,
> `n8n.delete_workflow`. The CRUD writes carry the
> highest blast radius of any Chapter F surface to date —
> a workflow definition can call arbitrary HTTP, mutate
> the operator's other services, or schedule recurring
> side effects — and ride the same Trusted-tier-only
> ceiling that every other Chapter F write-base uses.
> Two new bases gate the ten tools.*

| Phase | Bases added | Provenance |
|---|---|---|
| Phase 131 | `n8n.read` | Chapter F #7 — `n8n.list_workflows`, `n8n.get_workflow`, `n8n.list_executions`, `n8n.get_execution` against the operator's self-hosted n8n instance |
| Phase 131 | `n8n.write` | Chapter F #7 — `n8n.execute_workflow`, `n8n.activate_workflow`, `n8n.deactivate_workflow`, `n8n.create_workflow`, `n8n.update_workflow`, `n8n.delete_workflow` (Trusted-tier-only at the ceiling level, matching the Chapter F write-tool gating pattern; the CRUD trio carries definition-write blast radius that operators may want to attenuate further with role-level `capability_scopes`) |

### Current full enumeration after Phase 131 (67 bases)

Substrate-facing operator scopes (16):
- `fs.read`, `fs.write`, `fs.delete`, `fs.metadata`
- `net.fetch`, `net.post`, `net.dns`
- `shell.exec`, `shell.spawn`
- `llm.call`, `llm.embed`
- `memory.read`, `memory.write`, `memory.forget`, `memory.gc`
- `git.read`

Channel / audit / config (5)

Infrastructure (28)

Third-party tool process scopes (18):
- Email (Chapter F #1, Phase 123): `email.read`, `email.write`,
  `email.send`
- Personal assistant tool bundle (Chapter G #1, Phase 125):
  `web.search`, `task.read`, `task.write`, `health.read`,
  `health.write`
- Calendar (Chapter F #2, Phase 128): `calendar.read`,
  `calendar.write`
- Drive (Chapter F #3, Phase 129): `drive.read`, `drive.write`
- Notion (Chapter F #5, Phase 130 Task 2): `notion.read`,
  `notion.write`
- Obsidian (Chapter F #6, Phase 130 Task 10): `obsidian.read`,
  `obsidian.write`
- n8n (Chapter F #7, Phase 131 Task 2): `n8n.read`,
  `n8n.write`

Total: 16 + 5 + 28 + 18 = 67.

## Phase 143 addendum — Chapter G #2 budget tracking (2026-06-03)

> *Added at Phase 143 exit. Chapter G's second
> bundled capability after Phase 125's web.search +
> task.* + health.* surface. The aivyx-toolkit
> harness gains two budget-tracking tools surfacing
> a JSON-persisted entry store at
> `~/.aivyx-pa/tool-processes/toolkit/budget.json`:
> reads via `budget.summary` (aggregate totals +
> by-category breakdown over a period); writes via
> `budget.record` (single new entry append). Two
> new capability bases gate the pair, sharing the
> read/write split the task.* and health.* siblings
> already use.*

| Phase | Bases added | Provenance |
|---|---|---|
| Phase 143 | `budget.read` | Chapter G #2 — `budget.summary` (aggregate totals + by-category breakdown). |
| Phase 143 | `budget.write` | Chapter G #2 — `budget.record` (single-entry append; Trusted-tier-only at the ceiling level matching the rest of the toolkit + Chapter F gating). |

### Current full enumeration after Phase 143 (69 bases)

Substrate-facing operator scopes (16):
- `fs.read`, `fs.write`, `fs.delete`, `fs.metadata`
- `net.fetch`, `net.post`, `net.dns`
- `shell.exec`, `shell.spawn`
- `llm.call`, `llm.embed`
- `memory.read`, `memory.write`, `memory.forget`, `memory.gc`
- `git.read`

Channel / audit / config (5)

Infrastructure (28)

Third-party tool process scopes (20):
- Email (Chapter F #1, Phase 123): `email.read`, `email.write`,
  `email.send`
- Personal assistant tool bundle (Chapter G #1, Phase 125):
  `web.search`, `task.read`, `task.write`, `health.read`,
  `health.write`
- Calendar (Chapter F #2, Phase 128): `calendar.read`,
  `calendar.write`
- Drive (Chapter F #3, Phase 129): `drive.read`, `drive.write`
- Notion (Chapter F #5, Phase 130 Task 2): `notion.read`,
  `notion.write`
- Obsidian (Chapter F #6, Phase 130 Task 10): `obsidian.read`,
  `obsidian.write`
- n8n (Chapter F #7, Phase 131 Task 2): `n8n.read`,
  `n8n.write`
- Budget tracking (Chapter G #2, Phase 143): `budget.read`,
  `budget.write`

Total: 16 + 5 + 28 + 20 = 69.

## Phase 173 addendum — Autonomous Loop backlog tools (2026-06-05)

> *Added at Phase 173 exit. The Aivyx PA-native answer to the
> Ralph technique (snarktank/ralph): an autonomous,
> self-re-arming task loop that fires a fresh-context agent
> turn per iteration over an operator-stocked backlog. Two
> new channel-tier substrate tools let the loop agent
> interact with the HMAC-chained backlog substrate
> (`KeyDomain::LoopBacklog`): `loop.next` returns the next
> pending story; `loop.complete` marks a story done. Two new
> bases gate them, sharing the read/write split the
> `mission.*` and `reflection.*` siblings already use. Both
> Trusted-tier-only at the ceiling — the loop driver fires
> local `TriggerSource::Loop` turns, the same envelope as
> reflection; a SemiTrusted remote adapter must not be able
> to drive an autonomous code-committing loop. The
> thirteen-tool substrate core (amendment A12) is
> untouched.*

| Phase | Bases added | Provenance |
|---|---|---|
| Phase 173 | `loop.next` | Autonomous Loop — read the next pending backlog story (the Ralph task-selection step). |
| Phase 173 | `loop.complete` | Autonomous Loop — mark a backlog story Done (Trusted-tier-only at the ceiling, matching `mission.*` / `reflection.*`). |

### Current full enumeration after Phase 173 (71 bases)

The Phase 143 enumeration plus two new **infrastructure**
bases (`loop.next`, `loop.complete`), so the infrastructure
family grows 28 → 30:

Total: 16 + 5 + 30 + 20 = 71.

## Phase 175 addendum — Loop progress log (2026-06-06)

> *Added at Phase 175 exit. The Aivyx PA Ralph loop's
> cross-iteration learning: a reserved memory topic
> (`loop:progress`) holds durable notes the driver injects
> into each fresh iteration's prompt. One new channel-tier
> tool, `loop.note`, lets the loop agent append a learning to
> that topic (the tool owns the topic so the agent can't
> mis-route it). One new base gates it, Trusted-tier at the
> ceiling like the other `loop.*` tools — a SemiTrusted remote
> adapter must not write the loop's progress log. The
> thirteen-tool substrate core (amendment A12) is untouched.*

| Phase | Bases added | Provenance |
|---|---|---|
| Phase 175 | `loop.note` | Loop progress log — append a learning to the reserved progress topic the driver injects into each fresh iteration (Trusted-tier-only at the ceiling, matching `loop.next` / `loop.complete`). |

### Current full enumeration after Phase 175 (72 bases)

The Phase 173 enumeration plus one new **infrastructure** base
(`loop.note`), so the infrastructure family grows 30 → 31:

Total: 16 + 5 + 31 + 20 = 72.

## Phase 183 addendum — Reminders (everyday-PA breadth #1) (2026-06-06)

> *Added at Phase 183 exit. The first everyday-PA breadth pick:
> one-shot reminders. Two new channel-tier tools' worth of bases —
> `remind.read` gates `remind.list`, `remind.write` gates
> `remind.set` / `remind.cancel` — Trusted-tier at the ceiling
> like the `loop.*` tools (a SemiTrusted remote adapter must not
> set reminders that push notifications). A daemon-native
> capability (push-at-a-time needs the scheduler + notify, which a
> separate tool process lacks). The thirteen-tool substrate core
> (amendment A12) is untouched.*

| Phase | Bases added | Provenance |
|---|---|---|
| Phase 183 | `remind.read`, `remind.write` | Reminder tools — list pending (`read`) + set / cancel (`write`); the reminder driver fires due reminders through the notify dispatcher (Trusted-tier-only at the ceiling, matching `loop.*`). |

### Current full enumeration after Phase 183 (74 bases)

The Phase 175 enumeration plus two new **infrastructure** bases
(`remind.read`, `remind.write`), so the infrastructure family
grows 31 → 33:

Total: 16 + 5 + 33 + 20 = 74.

## Phase 184 addendum — Conversational skill-teaching (2026-06-06)

> *Added at Phase 184 exit. The last Chapter H phase: make the
> `LearnedSkill` layer operator-authorable. One new channel-tier
> base, `skills.write`, gates the three edit tools (`skills.teach`
> / `skills.update` / `skills.forget`) that append LearnedSkill
> deltas to the Persona chain after the agent confirms the
> drafted skill with the operator. Trusted-tier at the ceiling
> like the existing `skills.propose` / `skills.list` /
> `skills.invoke` — a SemiTrusted remote adapter must not edit
> the skill set (it is identity). Distinct from `skills.propose`
> (the gated reflection path). The thirteen-tool substrate core
> (amendment A12) is untouched.*

| Phase | Bases added | Provenance |
|---|---|---|
| Phase 184 | `skills.write` | Operator-authored skill editing — teach / update / forget a `LearnedSkill` after in-chat confirmation (Trusted-tier-only at the ceiling, matching the other `skills.*` bases). |

### Current full enumeration after Phase 184 (75 bases)

The Phase 183 enumeration plus one new **infrastructure** base
(`skills.write`), so the infrastructure family grows 33 → 34:

Total: 16 + 5 + 34 + 20 = 75.

## Chapter Contacts addendum — Google People API (Broaden #1) (2026-06-17)

> *Added at Chapter Contacts CT.1. The first **Broaden**-track
> domain (backend-audit F4 — everyday-PA breadth). Contacts is
> the fifth Google integration, via the `aivyx-contacts`
> third-party tool process over the People API. Six tools ship
> (3 read / 3 write, mirroring `drive.*`): `contacts.search`,
> `contacts.list`, `contacts.get`, `contacts.create`,
> `contacts.update`, `contacts.delete`. Two new bases gate them:
> `contacts.read` for the three read tools and `contacts.write`
> for the three write tools. Both Trusted-tier-only by default —
> same gating pattern as `email.*` / `calendar.*` / `drive.*` per
> the Phase 62 Q2(a) precedent. `contacts.delete` is irreversible
> and additionally confirm-first at the tool level (the
> Documents-`delete` policy).*

| Chapter | Bases added | Provenance |
|---|---|---|
| Contacts | `contacts.read` | Broaden #1 — `contacts.search`, `contacts.list`, `contacts.get` against the operator's authorized Google contacts (People API) |
| Contacts | `contacts.write` | Broaden #1 — `contacts.create`, `contacts.update` (etag round-trip), `contacts.delete` (confirm-first, irreversible; Trusted-tier-only at the ceiling, matching `drive.write` / `calendar.write` / `email.send`) |

### Running count

`KNOWN_BASES.len()` moves **83 → 85** (the two new third-party
tool-process bases). The `known_bases_count_matches_phase_143_a3_addendum`
test pins the new total at **85**, so this addendum and the runtime
stay in sync.

## Chapter Forge addendum — `git.write` (FG.2) (2026-06-19)

> *Added at Chapter Forge FG.2, the first new-tools breadth
> chapter after Atlas. `git.write` is the destructive git base
> A12 explicitly anticipated ("a future destructive git tool
> would warrant a separate `git.write` scope"). It gates the
> `git.commit` tool (stage + commit), shipping at FG.3; the base
> + count contract lands at FG.2, ahead of the tool. Qualified by
> canonical repo path, checked against the same operator
> `[git] repos` allow-set as `git.read`. `CEILING_TRUSTED` only —
> writing history is as sensitive as `shell.exec` / `fs.delete`,
> so a remote SemiTrusted adapter cannot hold it by default;
> confirm-first at the tool level when `confirm_destructive` is
> on. The companion `web.extract` substrate tool (FG.1) needs no
> new base — it reuses the existing `net.fetch` base — so it does
> not move this count.*

| Chapter | Bases added | Provenance |
|---|---|---|
| Forge | `git.write` | FG.2 — gates `git.commit` (FG.3), the destructive sibling to `git.status` / `git.diff`; Trusted-tier-only, confirm-first, reuses the `git.read` repo allow-set |

### Running count

`KNOWN_BASES.len()` moves **85 → 86** (the one new `git.write`
substrate base). The `known_bases_count_matches_phase_143_a3_addendum`
test pins the new total at **86**, so this addendum and the runtime
stay in sync.

## Chapter Lattice addendum — `graph.read` (LT.4) (2026-06-20)

> *Added at Chapter Lattice LT.4, the typed knowledge-graph chapter.
> `graph.read` gates the `graph.query` tool — a read-only **multi-hop,
> directed, typed traversal** of the agent's own knowledge graph
> (entities + `(subject)-[predicate]->(object)` relations extracted from
> memory). **Infrastructure, not substrate:** the agent querying its own
> derived self-knowledge, exactly like `skills.list` / `audit.read` —
> the graph is derived from memory, not a new operator-owned resource
> primitive. So it grows `KNOWN_BASES` **without a P10 substrate-count
> amendment** (the same precedent as `skills.*` / `loop.*` /
> `reflection.*` — every prior infrastructure base). Bare base (like
> `skills.list`), `CEILING_TRUSTED` only; SemiTrusted does not get it by
> default.*

| Chapter | Bases added | Provenance |
|---|---|---|
| Lattice | `graph.read` | LT.4 — gates `graph.query`, a read-only multi-hop traversal of the typed knowledge graph; infrastructure (no P10 amendment), Trusted-tier-only at the ceiling |

### Running count

`KNOWN_BASES.len()` moves **86 → 87** (the one new `graph.read`
infrastructure base). The `known_bases_count_matches_phase_143_a3_addendum`
test pins the new total at **87**, so this addendum and the runtime stay
in sync.

## Chapter Abacus addendum — `calc.eval` (AB.1) (2026-06-21)

> *Added at Chapter Abacus AB.1, the second new-tools breadth chapter
> after Forge (the utilities pack off the Atlas §6 backlog). `calc.eval`
> gates the `calc.eval` tool — an exact arithmetic-expression evaluator
> in the `aivyx-toolkit` tool process. **Tool-process tier, not
> substrate:** it sits beside `web.search` / `task.*` / `budget.*`, so
> it grows `KNOWN_BASES` **without a P10 substrate-count amendment**
> (same precedent as every Chapter F/G tool-process base). Its one
> distinguishing note is the **tier floor**: unlike every other toolkit
> base (Trusted-only, like `email.*`), a calculator is side-effect-free
> and offline — no network, no filesystem, no operator data — so it is
> the **first toolkit base reachable at SemiTrusted** (present in
> `CEILING_SEMITRUSTED`, and therefore Trusted/Kernel too). See
> docs/ABACUS.md §2.*

| Chapter | Bases added | Provenance |
|---|---|---|
| Abacus | `calc.eval` | AB.1 — gates `calc.eval`, a pure-compute arithmetic evaluator; tool-process tier (no P10 amendment), the first toolkit base reachable at SemiTrusted (no I/O, no operator data) |

### Running count

`KNOWN_BASES.len()` moves **87 → 88** (the one new `calc.eval`
tool-process base). The `known_bases_count_matches_phase_143_a3_addendum`
test pins the new total at **88**, so this addendum and the runtime stay
in sync.

## Chapter Abacus addendum — `convert.units` (AB.2) (2026-06-21)

> *Added at Chapter Abacus AB.2, the convert group of the utilities
> pack. One base, `convert.units`, gates **both** convert tools —
> `convert.units` (unit conversion across length / mass / temperature
> / volume / digital families, a hand-rolled curated table) and
> `convert.time` (IANA-named-timezone conversion via the bundled
> `chrono-tz` zone database, the chapter's one new dependency). Same
> tool-process tier + SemiTrusted floor as `calc.eval` (AB.1): both
> are side-effect-free and offline, so they need no P10 substrate
> amendment and are reachable below Trusted. Per the per-group base
> decision (OQ-1), the convert group is one base, not two. See
> docs/ABACUS.md §2.*

| Chapter | Bases added | Provenance |
|---|---|---|
| Abacus | `convert.units` | AB.2 — gates `convert.units` (curated unit table) and `convert.time` (`chrono-tz` IANA zones); tool-process tier (no P10 amendment), SemiTrusted-reachable (no I/O, no operator data) |

### Running count

`KNOWN_BASES.len()` moves **88 → 89** (the one new `convert.units`
group base). The `known_bases_count_matches_phase_143_a3_addendum`
test pins the new total at **89**, so this addendum and the runtime stay
in sync.

## Chapter Abacus addendum — `date.compute` (AB.3) (2026-06-21)

> *Added at Chapter Abacus AB.3, the date group of the utilities pack.
> One base, `date.compute`, gates **both** date tools — `date.diff`
> (the signed span between two instants) and `date.add` (add a
> possibly-negative duration to a date). Both reuse the existing
> `chrono` dependency (no new dep). Same tool-process tier +
> SemiTrusted floor as `calc.eval` / `convert.units`: they compute over
> their inputs with no I/O, so no P10 substrate amendment and reachable
> below Trusted. The one wrinkle, scoped to this group: a missing
> date defaults to **now** (`Utc::now()`) — the pack's only clock
> dependence — so the tools answer "from now" when no explicit date is
> given. See docs/ABACUS.md §2.*

| Chapter | Bases added | Provenance |
|---|---|---|
| Abacus | `date.compute` | AB.3 — gates `date.diff` + `date.add`, calendar-correct date arithmetic over `chrono`; tool-process tier (no P10 amendment), SemiTrusted-reachable (a missing date defaults to now) |

### Running count

`KNOWN_BASES.len()` moves **89 → 90** (the one new `date.compute`
group base). The `known_bases_count_matches_phase_143_a3_addendum`
test pins the new total at **90**, so this addendum and the runtime stay
in sync. *(Chapter Abacus is complete: three group bases — `calc.eval`,
`convert.units`, `date.compute` — for the five-tool utilities pack.)*

## Piece C addendum — `team.run.channel` (2026-08-23)

> *Added at Piece C (channel-triggered team missions, `docs/DAEMON_TEAMS.md`
> §6's Chat surface). One base, `team.run.channel`, gates the narrow,
> channel-only `/team run <goal>` trigger — starting a new Nonagon team
> mission from a chat command, never from the model. Unlike every other
> base in this file, it is deliberately absent from every real trust-tier
> ceiling (Trusted included): authorization is a bespoke, daemon-side,
> per-channel-type config check (`ChannelTriggerAuthz` in
> `daemon_server.rs`, driven by the operator's own `team_run_channel`
> TOML opt-in), since the ordinary CapabilitySet/TrustTier ceiling has no
> per-channel-type granularity to hang this on. Present in `KNOWN_BASES`
> purely for audit-trail/drift-guard consistency with every other gated
> capability surface, not because any tier grants it.*

| Chapter | Bases added | Provenance |
|---|---|---|
| Piece C | `team.run.channel` | Channel-only sibling to `team.run` — gates `/team run <goal>` from Telegram/Discord/Slack; granted by no real tier ceiling, authorized instead by the daemon's own per-channel `team_run_channel` config check |

### Running count

`KNOWN_BASES.len()` moves **93 → 94** (the one new `team.run.channel`
base). The `known_bases_count_matches_phase_143_a3_addendum` test pins
the new total at **94**, so this addendum and the runtime stay in sync.
(This file's own local running-count chain above had already fallen
behind the real `KNOWN_BASES.len()` before this base was added — a
pre-existing gap this entry doesn't attempt to reconcile, only to not
compound.)

## Phase 191 addendum — daemon-side automatic alert dispatch (2026-09-04)

> *Added at Phase 191 (daemon-side automatic alert dispatch for tool
> processes). One base, `notify.dispatch`, gates a tool process's own
> ability to push a notification through the daemon's `NotifyDispatcher`
> without a model round-trip — e.g. a toolkit health-check watcher
> detecting a low-stock or overdue-order transition and alerting the
> operator directly. Distinct from `notify.send`: that base gates the
> model-invoked `notify.send` infrastructure tool; this one gates the
> daemon-side sink a tool process's unprompted `DispatchNotification`
> wire frame is routed through. Trusted-tier only at the ceiling,
> matching `notify.send`'s own tier restriction (same cross-boundary
> data-exfil rationale — a SemiTrusted tool process must not be able to
> push arbitrary content to an operator-configured notify target).
> Currently unqualified-only (no per-target qualifier): the configured
> tool process has exactly one default notify target for this phase.*

| Chapter | Bases added | Provenance |
|---|---|---|
| Phase 191 | `notify.dispatch` | Daemon-side sink gate for a tool process's own `DispatchNotification` wire frame — Trusted-tier-only at the ceiling, matching `notify.send` |

### Running count

`KNOWN_BASES.len()` moves **94 → 95** (the one new `notify.dispatch`
base). The `known_bases_count_matches_phase_143_a3_addendum` test pins
the new total at **95**, so this addendum and the runtime stay in sync.

## Aivyx-Skills Part 3 addendum — `skill_defaults.*` (2026-09-23)

> *Added for Part 3 of the cross-repo Aivyx-Skills initiative (Parts 1/2
> — the shared `aivyx-skills` crate itself, and `aivyx-coder`'s own
> integration — already shipped in their respective repos).
> `skill_defaults.list` / `skill_defaults.read` gate two read-only tools
> over the compiled-in default skill library (plus optional
> `[skill_defaults]` project/user overlay directories) from the pinned
> `aivyx-skills` crate. **Infrastructure, not substrate:** the agent
> reading its own bundled, self-contained procedure library — the same
> precedent as `skills.list` / `graph.read` — not a new operator-owned
> resource primitive. So it grows `KNOWN_BASES` **without a P10
> substrate-count amendment**. Bare bases (like `skills.list`),
> `CEILING_TRUSTED` only; SemiTrusted does not get them by default. No
> `PathGlob` qualifier — the tools take a skill name, never a path, so
> the reachable set is fixed entirely by server-side `[skill_defaults]`
> config, not by anything the model can widen.*

| Chapter | Bases added | Provenance |
|---|---|---|
| Aivyx-Skills Part 3 | `skill_defaults.list`, `skill_defaults.read` | Gates the default-skill-library read tools; infrastructure (no P10 amendment), Trusted-tier-only at the ceiling |

### Running count

`KNOWN_BASES.len()` moves **96 → 98** (the two new
`skill_defaults.*` infrastructure bases). The
`known_bases_count_matches_phase_143_a3_addendum` test pins the new
total at **98**, so this addendum and the runtime stay in sync.
(This file's own local running-count chain above jumps from Phase
191's **95** straight to this section's **96**: the one-base gap in
between is `vision.generate`, added for Aivyx-Vision Milestone 1
[2026-09-18] — a real, already-shipped `KNOWN_BASES` addition, but one
this file no longer carries its own addendum section for, so the
narrative skips a step here even though both endpoints are correct. A
pre-existing-gap situation this entry doesn't attempt to reconcile,
only to not compound — same posture as the Piece C addendum's own note
above.)

## Model routing Part 3a addendum — `routing.*` (2026-09-26)

> *Added for Part 3a of the cross-repo model-routing initiative (the
> shared `aivyx-route` crate's `Router`, wrapped by `aivyx-llm`'s
> `RoutedProvider`). `routing.status` / `routing.read` gate two read-only
> tools over the daemon's own model router: `routing.status` lists the
> routing candidates (model id@endpoint, tier, known/unknown
> capabilities, context window, availability) and `routing.explain`
> (scope `routing.read`) returns the router's last decision for a
> session. **Infrastructure, not substrate:** the agent reading its own
> runtime's model-selection state — the same precedent as
> `skill_defaults.*` / `graph.read` — not a new operator-owned resource
> primitive. So it grows `KNOWN_BASES` **without a P10 substrate-count
> amendment**. Bare bases, `CEILING_TRUSTED` only; SemiTrusted does not
> get them by default. Both tools opt into the zero-config backcompat
> floor (read-only), and are registered only when `[routing] enabled`.*

| Chapter | Bases added | Provenance |
|---|---|---|
| Model routing Part 3a | `routing.status`, `routing.read` | Gates the router status / decision-explain read tools; infrastructure (no P10 amendment), Trusted-tier-only at the ceiling |

### Running count

`KNOWN_BASES.len()` moves **98 → 100** (the two new `routing.*`
infrastructure bases). The
`known_bases_count_matches_phase_143_a3_addendum` test pins the new
total at **100**, so this addendum and the runtime stay in sync.

## Phase 129 addendum — Chapter F #3 Google Drive (2026-06-01)

> *Added at Phase 129 exit. Chapter F's third integration —
> Google Drive via the `aivyx-drive` third-party tool
> process. Per Phase 129 Q2b (operator-picked over Q2a's
> 5-tool default), seven tools ship: `drive.search`,
> `drive.get_metadata`, `drive.list_folder`,
> `drive.create_folder`, `drive.download_file`,
> `drive.upload_file`, `drive.delete_file`. Two new bases
> gate them: `drive.read` for the four read tools and
> `drive.write` for the three write tools. Both Trusted-tier-
> only by default — same gating pattern as `email.*` /
> `calendar.*` per the Phase 62 Q2(a) precedent.*

| Phase | Bases added | Provenance |
|---|---|---|
| Phase 129 | `drive.read` | Chapter F #3 — `drive.search`, `drive.get_metadata`, `drive.list_folder`, `drive.download_file` against the operator's authorized Google Drive |
| Phase 129 | `drive.write` | Chapter F #3 — `drive.create_folder`, `drive.upload_file`, `drive.delete_file` (full file-lifecycle mutation; Trusted-tier-only at the ceiling level, matching `email.send` / `calendar.write` / `shell.exec`) |

### Current full enumeration (61 bases)

Substrate-facing operator scopes (16):
- `fs.read`, `fs.write`, `fs.delete`, `fs.metadata`
- `net.fetch`, `net.post`, `net.dns`
- `shell.exec`, `shell.spawn`
- `llm.call`, `llm.embed`
- `memory.read`, `memory.write`, `memory.forget`, `memory.gc`
- `git.read`

Channel / audit / config (5):
- `channel.send`, `channel.receive`, `audit.read`,
  `config.read`, `config.write`

Infrastructure (28):
- Role primitive: `tool.allowlist`, `role.switch`, `role.update`
- Mission: `mission.create`, `mission.gate`, `mission.list`,
  `mission.status`
- Scheduling: `schedule.create`, `schedule.list`,
  `schedule.delete`, `schedule.update`
- Triggers: `webhook.create`, `webhook.list`, `webhook.delete`,
  `file_watch.create`, `file_watch.list`, `file_watch.delete`
- MCP: `mcp.call`
- Reflection: `reflection.propose`, `reflection.apply`
- Persona / Skills: `persona.propose`, `skills.propose`,
  `skills.list`, `skills.invoke`
- Notify: `notify.send`
- Ollama management: `ollama.list`, `ollama.show`, `ollama.pull`

Third-party tool process scopes (12):
- Email (Chapter F #1, Phase 123): `email.read`, `email.write`,
  `email.send`
- Personal assistant tool bundle (Chapter G #1, Phase 125):
  `web.search`, `task.read`, `task.write`, `health.read`,
  `health.write`
- Calendar (Chapter F #2, Phase 128): `calendar.read`,
  `calendar.write`
- Drive (Chapter F #3, Phase 129): `drive.read`, `drive.write`

Total: 16 + 5 + 28 + 12 = 61.

### Verification

A unit test in `aivyx-capability/src/lib.rs` pins the
count so this addendum and the runtime stay in sync; any
future base added without an accompanying addendum bump
surfaces as a test failure rather than silent drift.

## Phase 128 addendum — Chapter F #2 Google Calendar (2026-06-01)

> *Added at Phase 128 exit. Chapter F's second integration —
> Google Calendar via the `aivyx-calendar` third-party tool
> process. Per Phase 128 Q3b (operator-picked over Q3a's
> 4-tool default), five tools ship: `calendar.list_events`,
> `calendar.get_event`, `calendar.create_event`,
> `calendar.update_event`, `calendar.delete_event`. Two new
> bases gate them: `calendar.read` for the two read tools
> and `calendar.write` for the three write tools. Both
> Trusted-tier-only by default — same gating pattern as
> `email.*` per the Phase 62 Q2(a) precedent.*

| Phase | Bases added | Provenance |
|---|---|---|
| Phase 128 | `calendar.read` | Chapter F #2 — `calendar.list_events` (range query) + `calendar.get_event` (single fetch by ID) against the operator's authorized Google Calendar(s) |
| Phase 128 | `calendar.write` | Chapter F #2 — `calendar.create_event`, `calendar.update_event`, `calendar.delete_event` (full event-lifecycle mutation; Trusted-tier-only at the ceiling level, matching `email.send` / `shell.exec` / `notify.send`) |

### Current full enumeration (59 bases)

Substrate-facing operator scopes (16):
- `fs.read`, `fs.write`, `fs.delete`, `fs.metadata`
- `net.fetch`, `net.post`, `net.dns`
- `shell.exec`, `shell.spawn`
- `llm.call`, `llm.embed`
- `memory.read`, `memory.write`, `memory.forget`, `memory.gc`
- `git.read`

Channel / audit / config (5):
- `channel.send`, `channel.receive`, `audit.read`,
  `config.read`, `config.write`

Infrastructure (28):
- Role primitive: `tool.allowlist`, `role.switch`, `role.update`
- Mission: `mission.create`, `mission.gate`, `mission.list`,
  `mission.status`
- Scheduling: `schedule.create`, `schedule.list`,
  `schedule.delete`, `schedule.update`
- Triggers: `webhook.create`, `webhook.list`, `webhook.delete`,
  `file_watch.create`, `file_watch.list`, `file_watch.delete`
- MCP: `mcp.call`
- Reflection: `reflection.propose`, `reflection.apply`
- Persona / Skills: `persona.propose`, `skills.propose`,
  `skills.list`, `skills.invoke`
- Notify: `notify.send`
- Ollama management: `ollama.list`, `ollama.show`, `ollama.pull`

Third-party tool process scopes (10):
- Email (Chapter F #1, Phase 123): `email.read`, `email.write`,
  `email.send`
- Personal assistant tool bundle (Chapter G #1, Phase 125):
  `web.search`, `task.read`, `task.write`, `health.read`,
  `health.write`
- Calendar (Chapter F #2, Phase 128): `calendar.read`,
  `calendar.write`

Total: 16 + 5 + 28 + 10 = 59.

### Verification

A unit test in `aivyx-capability/src/lib.rs` pins the
count so this addendum and the runtime stay in sync; any
future base added without an accompanying addendum bump
surfaces as a test failure rather than silent drift.
