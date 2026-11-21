# Starter Templates

Phase 66 introduced **starter profile templates** so a fresh
operator gets from "downloaded the binary" to "useful agent"
without writing `aivyx-pa.toml` from scratch. Three templates
ship bundled:

```sh
aivyx-pa init --list-templates
# Available templates:
#
#   coder       (bundled)  Software engineering assistant — Rust/Python/etc. dev work…
#   researcher  (bundled)  Research and synthesis assistant — heavy web fetch + memory…
#   personal    (bundled)  Personal assistant — daily briefings, task management, …
#   kitchen     (bundled)  Back-of-house kitchen operations — inventory, reorder, HACCP (vertical pack)
#
# Use `aivyx-pa init --template <name>` to start the wizard pre-filled from a template.
```

`aivyx-pa init --template <name>` runs the existing interactive
wizard with prompt defaults pre-filled from the template; the
final `aivyx-pa.toml` is the template's content with the
operator's wizard answers spliced in. Comments, role
declarations, MCP server blocks, and commented-out sections
all survive.

## Bundled templates

### `coder` — software engineering

For developers writing code with Aivyx PA as a pair-programmer.

**Profile:** `assistant_name = "Codex"`, primary use case
`"software engineering"`, behavioral preferences around
integration-tests-over-mocks and citing source files,
constraints around code commits and destructive shell commands.

**Role envelope:** `fs.read`, `fs.write`, `shell.exec`,
`memory.{read,write,forget}`, `web.fetch`, `notify.send`.

**MCP:** bundled `web-search` enabled by default for
documentation lookups.

**Pick this when:** you're using Aivyx PA to help with a codebase,
need shell + filesystem access, and want behavioral nudges
toward test-first and citation discipline.

### `researcher` — research + synthesis

For literature review, research synthesis, and note-keeping
workflows that don't need shell access.

**Profile:** `assistant_name = "Inquiry"`, primary use cases
`["research and synthesis", "literature review"]`, behavioral
preferences around source citation and primary-source
preference.

**Role envelope:** `fs.read`, `fs.write` (notes sandbox only),
`memory.{read,write,forget}`, `web.fetch`. **No** `shell.exec`
— this archetype is deliberately read-and-write-notes, not
run-code.

**MCP:** bundled `web-search` prominently featured (the
workhorse for this archetype).

**Pick this when:** you're doing research, need rigorous
citation discipline, and don't want the agent running shell
commands.

### `personal` — personal task management

For personal-assistant use: daily briefings, journal-keeping,
notification-driven workflows.

**Profile:** `assistant_name = "Mira"`, primary use cases
`["personal task management", "daily briefings",
"journal-keeping"]`, behavioral preferences for
conclusion-first paragraphs and three-bullet summaries.

**Role envelope:** `fs.read`, `fs.write` (journal sandbox),
`memory.{read,write,forget}`, `web.fetch`, `notify.send`.

**MCP:** none enabled by default; `web.fetch` covers the
common case.

**Starter automation (commented out):** a `[[schedule]]` for
"morning briefing" at 9am and a `[[notify_target]] kind =
"telegram"` block ready to wire when the operator adds a bot
token + chat_id.

**Pick this when:** you want Aivyx PA to remember things over
time, summarize what's on your plate, and (with the commented
blocks uncommented) push briefings to your phone.

### `kitchen` — back-of-house kitchen operations (vertical pack)

The first Aivyx PA **vertical pack** — and richer than the three
archetypes above. It specializes the agent for a small commercial
kitchen's back-of-house: inventory, recipes, par-level reorder, and
HACCP food-safety logging over the existing KitchenDB. See
[`docs/VERTICAL_PACKS.md`](VERTICAL_PACKS.md) for the full design.

**Profile:** `assistant_name = "Aria"`, primary use cases
`["kitchen operations", "back of house"]`, constraints that hard-stop
autonomous purchase orders and require a corrective action on every
out-of-limit temperature.

**Role envelope (`boh`):** the `kitchen.*` capability scopes
(`kitchen.read`, `kitchen.write`, `kitchen.order.send`,
`kitchen.haccp.log`) + `memory.{read,write}`, `trust_ceiling =
"Trusted"`.

**Tool process:** wires the **`aivyx-kitchen`** tool process — the
read/compute/gated-write/HACCP tool surface over the KitchenDB RPC
API. Requires building the binary (`cargo build --release -p
aivyx-kitchen`) and a per-tool-process config with the KitchenDB
connection.

**Starter automation (commented out):** a `[[schedule]]` for the
**nightly autonomous par-level reorder** — it drafts per-supplier
purchase orders unattended and **stops at the confirm-first gate**, so
nothing is ordered without morning approval (VERTICAL_PACKS §3.5).

**Skills bundle:** starter routines (nightly reorder, fridge-temp
round, cook/hold check, stocktake, recipe scaling) ship with the pack
— `aivyx_kitchen::pack::skills_json()` — installed by teaching the
agent each via `skills.teach`.

**Pick this when:** you run a kitchen and want stock/reorder/recipe
help and tamper-evident food-safety records — with autonomous overnight
analysis but a human at every order and an immutable HACCP log.

## Custom templates

Drop a `.toml` file in `~/.local/share/aivyx-pa/templates/` (or
`$XDG_DATA_HOME/aivyx-pa/templates/` if you set that env var) and
`aivyx-pa init --template <name>` will find it. The user-dir
templates take precedence over bundled templates of the same
name, so you can override `coder` with your own version
without touching the binary.

**Description metadata:** the listing output reads the first
line of each template that matches `# description: <text>` —
add one to your custom template and it'll show up in
`aivyx-pa init --list-templates`:

```toml
# description: My customized coder template with the rustc-internals MCP server.

[agent]
provider = "llamacpp"
…
```

If the marker is absent, the listing shows `(no description)`.

## Authoring a custom template

A template is just a complete `aivyx-pa.toml` with sensible
defaults. The wizard reads these specific keys to pre-fill
prompts:

| TOML key | Wizard prompt |
|---|---|
| `[agent] provider` | Provider menu default |
| `[agent] model` | Model name default (per-provider) |
| `[fs] root` | Sandbox root default |
| `[storage] path` | Storage path default |
| `[profile] assistant_name` | Assistant name default |
| `[profile] primary_use_cases[0]` | Primary use case default |
| `[profile] communication_style` | Communication style default |

Any other content in the template (`[[role]]`, `[[mcp_server]]`,
`[[notify_target]]`, `[[schedule]]`, comments, etc.) is
preserved verbatim into the generated `aivyx-pa.toml`. Only the
seven keys above are overwritten by the wizard.

Reference the three bundled templates in
[`examples/templates/`](../examples/templates/) for fully
worked examples.

## What's not in Phase 66

A few deferrals recorded at the phase open:

- **More templates** — Phase 66 ships three archetypes.
  Future phases or community contributions can add more
  (`data-analyst`, `writer`, `student`, etc.).
- **Template parameter substitution** — no `{{operator_name}}`
  placeholders yet. Templates ship with literal defaults; the
  wizard's prompts let the operator override per-field.
- **Template tagging / search** — flat listing only today.
- **Web UI template selection** — CLI-only.
- **Versioned template registry** — single version per name.
- **Sharing templates between operators** — manual file
  copy today; no registry, no `curl`-from-URL.
