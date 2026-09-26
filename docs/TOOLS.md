# Aivyx PA Tool Catalog

> **Chapter Atlas (AT.1).** The reference for *what tools the agent has*, the
> capability scope each needs, the minimum trust tier it's available at by default,
> and where it's delivered from. Generated/curated against the authoritative
> capability registry `KNOWN_BASES` (`crates/aivyx-capability/src/lib.rs`) and
> **drift-guarded** by `tools_catalog_documents_every_known_base` — adding a new
> capability base fails CI until it's documented here. For *building* a tool, see
> [`TOOL_SDK.md`](TOOL_SDK.md); for the design rationale, [`ATLAS.md`](ATLAS.md).

## How tools are delivered

Every tool implements the `aivyx_core::Tool` trait: a **pure `required_scope(input)`**
computes the capability the call needs, which the daemon enforces **before**
`execute` runs. Tools arrive in four ways (PRODUCT.md **P10** governance):

| Delivery | What | Where |
|---|---|---|
| **Substrate** | the irreducible in-process core, **capped at 13** (amendment-gated) | `aivyx-core` |
| **Infrastructure** | in-process agent machinery (missions, schedules, reflection, …), uncapped | `aivyx-channel`, `aivyx-core` |
| **Tool process** | out-of-process binaries over the Tool SDK; connected via `aivyx-pa connect <x>` | `aivyx-gmail`, `aivyx-calendar`, … |
| **MCP** | external Model Context Protocol servers, bridged through one `mcp.call` tool | operator-configured |

## How to read the scope / tier columns

- **Scope** is the tool's capability **base** (an entry in `KNOWN_BASES`). A `[role]`
  grant of that base (optionally qualified) is what authorizes the tool.
- **Min tier** is the lowest trust tier whose *default ceiling* includes the base.
  **SemiTrusted** bases: `fs.metadata`, `net.fetch`, `net.dns`, `llm.call`,
  `llm.embed`, `memory.read`, `memory.write`, `config.read`. **Untrusted** sees only
  public `memory.read` + `audit.read`. Everything else is **Trusted+** by default
  (write/personal-data/management surfaces — including `config.write`, `role.switch`,
  `role.update`, all deliberately Trusted-tier per PRODUCT.md P1, not Kernel). The one
  genuine **Kernel**-only base is `tool.allowlist`, a synthetic dispatch-layer label no
  real tier's ceiling ever holds (see `aivyx-capability`'s
  `tool_allowlist_parses_and_is_absent_from_real_ceilings`). Operators can narrow per
  role; they cannot widen past the tier ceiling. (Corrected 2026-07-07 — Chapter
  Almanac's `TrustTier::min_for_scope` surfaced that this section had drifted from the
  ceiling tables; the three rows below were "Kernel" here but Trusted in code.)

---

## Filesystem & workspace (substrate)

| Tool | Scope | Min tier | Notes |
|---|---|---|---|
| `fs.read` | `fs.read` | Trusted | read a file within the access-scoped `fs_root` |
| `fs.write` | `fs.write` | Trusted | write a file within `fs_root` |
| `fs.delete` | `fs.delete` | Trusted | delete; Local-channel + confirm-first gated |
| `fs.metadata` | `fs.metadata` | SemiTrusted | stat a path (size/kind/mtime) |
| `workspace.read` / `.write` / `.list` / `.delete` / `.note` | `workspace` | Trusted | the agent's own private notebook dir (Chapter O), independent of `fs_root` |

## Structured-data readers + writers (infrastructure — Chapter Sheaf)

Reuse the `fs.read` / `fs.write` capabilities + sandbox (no new base, no new I/O
reach): they only parse/produce bytes the agent could already `fs.read` /
`fs.write`. Registered beside `fs.read`/`fs.write`.

| Tool | Scope | Min tier | Notes |
|---|---|---|---|
| `data.csv` | `fs.read` | Trusted | read a CSV/TSV file into `{headers, rows, …}` (SH.1) |
| `data.xlsx` | `fs.read` | Trusted | read an .xlsx sheet into `{sheet, sheet_names, headers, rows, …}` (SH.2, `calamine`) |
| `data.pdf` | `fs.read` | Trusted | extract a PDF's text layer into `{text, pages, …}` (SH.3, `pdf-extract`; no OCR) |
| `data.xlsx.write` | `fs.write` | Trusted | write `rows` (+ optional `headers`) into a new .xlsx spreadsheet (SH.6, `rust_xlsxwriter`) |
| `data.pdf.write` | `fs.write` | Trusted | lay `text` out into a new PDF, single font, auto-wrap + pagination (SH.6, `lopdf`; no rich formatting) |

## Network & web (substrate)

| Tool | Scope | Min tier | Notes |
|---|---|---|---|
| `web.fetch` | `net.fetch` | SemiTrusted | HTTP GET a URL (read) |
| `web.post` | `net.post` | Trusted | HTTP POST to a URL |
| `web.extract` | `net.fetch` | SemiTrusted | GET a URL and return its readable article text (title + clean body), not raw HTML |
| `net.dns` | `net.dns` | SemiTrusted | resolve a hostname |

## Shell (substrate)

| Tool | Scope | Min tier | Notes |
|---|---|---|---|
| `shell.exec` | `shell.exec` | Trusted | run a command; Local-channel gated + sandboxable |
| *(spawn)* | `shell.spawn` | Trusted | long-running spawn capability |

## Git (substrate)

| Tool | Scope | Min tier | Notes |
|---|---|---|---|
| `git.status` | `git.read` | Trusted | working-tree status of a configured repo |
| `git.diff` | `git.read` | Trusted | diff of a configured repo |
| `git.commit` | `git.write` | Trusted | stage given repo-relative paths + commit with a message in a configured repo; confirm-first when `confirm_destructive` |

*The `git.write` base was added at Chapter Forge FG.2 — the destructive
sibling A12 anticipated — gating `git.commit`. Trusted-tier only (writing
history is as sensitive as `shell.exec` / `fs.delete`); reuses `git.read`'s
`[git] repos` allow-set. See `docs/amendments/2026-06-19-substrate-tool-count-fifteen.md`.*

## Knowledge graph (infrastructure)

| Tool | Scope | Min tier | Notes |
|---|---|---|---|
| `graph.query` | `graph.read` | Trusted | read-only multi-hop directed/typed traversal of the agent's knowledge graph (entities + `(subject)-[predicate]->(object)` relations extracted from memory) |

*The `graph.read` base was added at Chapter Lattice LT.4. It is
**infrastructure**, not substrate (the agent querying its own *derived*
self-knowledge, like `skills.list` / `audit.read`) — so it grows
`KNOWN_BASES` (86 → 87) with **no P10 substrate-count amendment**. Bare
base, Trusted-tier only. See the Lattice addendum in
`docs/amendments/2026-04-17-capability-taxonomy-growth.md`.*

## LLM (substrate)

| Tool | Scope | Min tier | Notes |
|---|---|---|---|
| `llm.call` | `llm.call` | SemiTrusted | sub-call to the configured LLM |
| `llm.embed` | `llm.embed` | SemiTrusted | embeddings for semantic memory |

## Memory (substrate)

| Tool | Scope | Min tier | Notes |
|---|---|---|---|
| `memory.read` | `memory.read` | SemiTrusted (public subset: Untrusted) | recall stored memories |
| `memory.write` | `memory.write` | SemiTrusted | store a memory |
| `memory.forget` | `memory.forget` | Trusted | delete a memory |
| `memory.gc` | `memory.gc` | Trusted | retention sweep |

## Channel & audit

| Tool | Scope | Min tier | Notes |
|---|---|---|---|
| *(send/receive)* | `channel.send`, `channel.receive` | Trusted | channel I/O capability |
| `notify.send` | `notify.send` | Trusted | push a message to the operator (Trusted-only — cross-boundary leak guard) |
| *(daemon-side dispatch)* | `notify.dispatch` | Trusted | gates a tool process's own `DispatchNotification` wire frame — an unprompted, daemon-routed push (e.g. a toolkit watcher alerting on a detected condition) rather than a model-invoked tool call; no registered `Tool`, so it never appears in a live tool catalog. Trusted-only, same cross-boundary leak guard as `notify.send` — Phase 191 |
| `turn.history` | `audit.read` | Trusted | read recent turn outcomes from the audit chain |
| `daemon.state` | `audit.read` | Trusted | read daemon/agent status |
| `tools.list` | `audit.read` | Trusted | **enumerate the agent's own tools** (name + description; `detail=true` for input schemas; optional `filter`). Live, ground-truth introspection — Chapter Atlas AT.2 |

## Config

| Tool | Scope | Min tier | Notes |
|---|---|---|---|
| `config.read` | `config.read` | SemiTrusted | read effective config |
| `config.write` | `config.write` | Trusted | rewrite a config section (no registered `Tool` — gates the Settings-screen write IPC path only, so it never appears in a live tool catalog) |
| *(allowlist)* | `tool.allowlist` | Kernel | per-role tool allowlist (synthetic) |

## Skills, reflection & persona (infrastructure)

| Tool | Scope | Min tier | Notes |
|---|---|---|---|
| `skills.list` / `skills.invoke` | `skills.list`, `skills.invoke` | Trusted | enumerate / run learned skills |
| `skills.teach` / `skills.update` / `skills.forget` / `skills.propose` | `skills.write`, `skills.propose` | Trusted | manage learned skills |
| `skill_defaults.list` / `skill_defaults.read` | `skill_defaults.list`, `skill_defaults.read` | Trusted | enumerate / render compiled-in default skill library (Aivyx-Skills Part 3) |
| `routing.status` / `routing.explain` | `routing.status`, `routing.read` | Trusted | model router's candidates / a session's last routing decision (model routing Part 3a; registered only when `[routing] enabled`) |
| `reflection.propose` / `reflection.apply` | `reflection.propose`, `reflection.apply` | Trusted | self-improvement proposals |
| `persona.propose` | `persona.propose` | Trusted | persona-evolution proposals |
| `role.switch` | `role.switch` | Trusted | sub-agent role switching |
| `role.update` | `role.update` | Trusted | update a role definition |

## Missions, scheduling & automation (infrastructure)

| Tool | Scope | Min tier | Notes |
|---|---|---|---|
| `mission.create` / `.gate` / `.list` / `.status` | `mission.create`, `mission.gate`, `mission.list`, `mission.status` | Trusted | Nonagon mission lifecycle |
| `team.run` / `team.delegate` / `team.message` | `team.run`, `team.delegate`, `team.message` | Trusted | multi-agent team execution |
| *(channel-trigger)* | `team.run.channel` | (bespoke) | channel-triggered team mission — daemon-side per-channel config, not trust-tier gated |
| `schedule.create` / `.list` / `.delete` / `.update` | `schedule.create`, `schedule.list`, `schedule.delete`, `schedule.update` | Trusted | cron-style scheduled runs; `.create`/`.update`/`.delete` refuse from within any triggered/scheduled run (cron, webhook, file-watch, reflection, loop, or any team mission started from within such a run) — operator/interactive-only, so an unattended run can't recursively create more automation |
| `webhook.create` / `.list` / `.delete` | `webhook.create`, `webhook.list`, `webhook.delete` | Trusted | inbound webhook triggers |
| *(file-watch)* | `file_watch.create`, `file_watch.list`, `file_watch.delete` | Trusted | filesystem-change triggers |
| `loop.next` / `.complete` / `.note` | `loop.next`, `loop.complete`, `loop.note` | Trusted | the autonomous (Ralph) loop |
| `remind.set` / `.list` / `.cancel` | `remind.write`, `remind.read` | Trusted | reminders (everyday-PA) |
| `mcp.call` | `mcp.call` | Trusted | bridge to an external MCP server tool (`<server>:<tool>`) |

## Local-model management (infrastructure)

| Tool | Scope | Min tier | Notes |
|---|---|---|---|
| `ollama.list` / `ollama.show` / `ollama.pull` | `ollama.list`, `ollama.show`, `ollama.pull` | Trusted | manage local Ollama models |

## Email — Gmail (tool process `aivyx-gmail`)

| Tool | Scope | Min tier | Notes |
|---|---|---|---|
| `gmail.search` / `gmail.read` | `email.read` | Trusted | search / read mail |
| `gmail.draft` | `email.write` | Trusted | create a draft |
| `gmail.send` | `email.send` | Trusted | send mail |

## Calendar (tool process `aivyx-calendar`)

| Tool | Scope | Min tier | Notes |
|---|---|---|---|
| `calendar.list_events` / `calendar.get_event` | `calendar.read` | Trusted | read events |
| `calendar.create_event` / `update_event` / `delete_event` | `calendar.write` | Trusted | manage events |

## Drive (tool process `aivyx-drive`)

| Tool | Scope | Min tier | Notes |
|---|---|---|---|
| `drive.search` / `get_metadata` / `list_folder` / `download_file` | `drive.read` | Trusted | read files/folders |
| `drive.create_folder` / `upload_file` / `delete_file` | `drive.write` | Trusted | manage files |

## Contacts (tool process `aivyx-contacts`)

| Tool | Scope | Min tier | Notes |
|---|---|---|---|
| `contacts.search` / `list` / `get` | `contacts.read` | Trusted | read contacts |
| `contacts.create` / `update` / `delete` | `contacts.write` | Trusted | manage contacts |

## Notion (tool process `aivyx-notion`)

| Tool | Scope | Min tier | Notes |
|---|---|---|---|
| `notion.search` / `get_page` / `list_database` | `notion.read` | Trusted | read pages/databases |
| `notion.create_page` / `update_page_properties` | `notion.write` | Trusted | manage pages |

## Obsidian (tool process `aivyx-obsidian`)

| Tool | Scope | Min tier | Notes |
|---|---|---|---|
| `obsidian.search` / `get_note` / `list_folder` | `obsidian.read` | Trusted | read vault notes |
| `obsidian.create_note` / `update_note` / `delete_note` | `obsidian.write` | Trusted | manage vault notes |

## n8n workflows (tool process `aivyx-n8n`)

| Tool | Scope | Min tier | Notes |
|---|---|---|---|
| *(read tools)* | `n8n.read` | Trusted | list/inspect workflows |
| `n8n.update_workflow` *(+ write tools)* | `n8n.write` | Trusted | manage workflows |

## Personal-assistant toolkit (tool process `aivyx-toolkit`)

| Tool | Scope | Min tier | Notes |
|---|---|---|---|
| `web.search` | `web.search` | Trusted | Brave Search |
| `task.create` / `task.list` / `task.complete` / `task.delete` | `task.write`, `task.read` | Trusted | lightweight task list |
| `health.check.add` / `health.check.list` / `recent_changes` | `health.write`, `health.read` | Trusted | personal health-check log |
| `budget.record` / `budget.summary` | `budget.write`, `budget.read` | Trusted | personal budget tracking |
| `calc.eval` | `calc.eval` | SemiTrusted | exact arithmetic evaluator (Chapter Abacus — pure compute, no I/O, so reachable below Trusted) |
| `convert.units` / `convert.time` | `convert.units` | SemiTrusted | unit conversion (length/mass/temp/volume/digital) + IANA-timezone conversion (Chapter Abacus — pure compute; one group base) |
| `date.diff` / `date.add` | `date.compute` | SemiTrusted | calendar-correct date arithmetic (signed span; add/subtract a duration) — Chapter Abacus; a missing date defaults to now |

## Kitchen vertical (tool process `aivyx-kitchen`)

| Tool | Scope | Min tier | Notes |
|---|---|---|---|
| `kitchen.read` / `kitchen.write` | `kitchen.read`, `kitchen.write` | Trusted | KitchenDB read/write |
| `kitchen.order.send` | `kitchen.order.send` | Trusted | submit an order |
| `kitchen.haccp.log` | `kitchen.haccp.log` | Trusted | HACCP compliance log |

## Open applications (tool process `aivyx-apps`, opt-in — Chapter Deckhand)

Opt-in via `[applications]`; default off. Lets the agent use the GUI apps already
open on the operator's own machine. Linux/X11 (+ Xwayland) first; Wayland-native
windows are a documented limitation. Both bases are Trusted-only and the
`app.control` input tools are confirm-first (see `docs/APPLICATIONS.md`).

| Tool | Scope | Min tier | Notes |
|---|---|---|---|
| `app.list` | `app.read` | Trusted | enumerate open windows (id/title/active) |
| `app.screenshot` | `app.read` | Trusted | capture the screen (a vision model interprets it) |
| `app.focus` | `app.control` | Trusted | raise/focus a window (reversible) |
| `app.type` / `app.key` / `app.click` | `app.input` | Trusted | inject input — **confirm-first** (irreversible) |

## Aivyx-Vision (tool process `aivyx-vision`, Milestone 1 — 2026-09-18; Milestone 2 Pass A — 2026-09-18)

One base shared by all three generation domains (vector/SVG, image, 3D)
— nothing to read separately from what's generated. SemiTrusted: narrower
and safer than `llm.call` (constrained prompt / bounded local generation,
sanitized or locally-written output), which is itself already
SemiTrusted-reachable.

| Tool | Scope | Min tier | Notes |
|---|---|---|---|
| `vision.generate_svg` | `vision.generate` | SemiTrusted | LLM-generated SVG from a text prompt (constrained prompt, sanitized output) |
| `vision.generate_image` | `vision.generate` | SemiTrusted | Local image generation via `mold serve` (requires `[mold]` config; writes a file under `output_dir`, returns its path) |
| `vision.generate_3d` | `vision.generate` | SemiTrusted | Local 3D model generation — **not yet implemented** (mold's Pass B); always fails today with a clear error |

---

## Tool-name → capability-scope mapping

Most tool names match their scope base. These differ by design (the tool name is
user-facing; the scope base groups capabilities):

| Tool name(s) | Capability base | Why |
|---|---|---|
| `web.fetch`, `web.extract` | `net.fetch` | "web" is the user-facing verb; both are outbound HTTP GETs (extract adds a readability pass) |
| `web.post` | `net.post` | same |
| `gmail.*` | `email.*` | the base is provider-neutral (`email.read/write/send`); Gmail is one implementation |
| `git.status`, `git.diff` | `git.read` | one read base shared by both read tools (A12) |
| `git.commit` | `git.write` | the destructive write base (A13, Chapter Forge); separate from `git.read` by invariant |
| `web.search` | `web.search` | (matches — listed for completeness) |

---

*This catalog is organized around `KNOWN_BASES`; the drift-guard test asserts every
one of the 87 bases appears here. Runtime, per-instance tool introspection (live
names + schemas the agent sees) is provided by the `tools.list` tool — Chapter Atlas
AT.2.*
