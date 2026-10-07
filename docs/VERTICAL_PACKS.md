# Aivyx PA Vertical Packs

How to specialize the *one* Aivyx PA agent to a domain **without forking
the substrate**. A vertical pack is configuration + a tool bundle, not a
codebase branch — so every pack inherits all future foundation work for
free. This document defines the pack format, gives a **step-by-step build
tutorial** ([§6](#6-build-your-own-pack--step-by-step)), and works through
the **canonical example**: a **Kitchen / Back-of-House (BOH)** pack over the
existing KitchenDB.

> **The Kitchen pack is the reference example.** It is open
> (`crates/verticals/aivyx-kitchen` + `aivyx-kitchen-toolkit`), complete, and
> meant to be **read and copied** — it is the worked answer to "how do I
> build a pack?" Paid packs live out-of-tree behind the same contract.

> Status: **live and complete end-to-end.** The pack ships the **BOH Nonagon
> team** (the customised `TeamConfig` — Aria + four least-privileged
> specialists over the `kitchen.*` scopes — + the overnight-close
> `MissionPlan`, as a Rust constructor and a committed TOML asset) **and** the
> real domain tools: the `aivyx-kitchen-toolkit` tool-process is a PostgREST
> RPC client to the operator's KitchenDB exposing eleven `kitchen.*` tools
> across four scope bases (read / write / `order.send` confirm-first /
> `haccp.log` append-only). It installs via `aivyx-pa connect kitchen` and is
> visible to `aivyx-pa doctor`. Chapters **J.6 / Brigade / Lockup / Mise**.

> **The boundary (new):** both kitchen crates now depend on **one** crate —
> [`aivyx-vertical-sdk`](../crates/aivyx-vertical-sdk) — a thin, semver-stable
> facade that re-exports *only* the pack-facing slice of the engine
> (`TeamConfig`, `MissionPlan`, `TrustTier`, `Scope`, the `Tool` trait, the
> tool-process harness). A pack's **shipping code never touches the engine
> crates directly**, so core refactors can't break it. This is the seam that
> makes a marketplace of packs maintainable, and the exact line a separate
> verticals repo would later cut along.

> **Config packs (2026-10-07).** Besides the tool packs this document
> describes, a pack can now be **configuration only** (`format = 2`): a
> starter config, a team, skills and routines over the tools Aivyx PA
> already has, for aivyx-pa and/or aivyx-coder. It installs as a **new
> named instance** (`aivyx-pa pack install`), is checked by
> `aivyx-pa pack check`, and can't set autonomy above `supervised`. See
> [`manual/developer/05-vertical-packs.md`](manual/developer/05-vertical-packs.md)
> and the shared [`aivyx-pack`](https://github.com/Aivyx-Agent/aivyx-pack)
> crate for the format.

---

## 1. Why packs, not forks

Aivyx PA is **one** agent, shaped by Profile (P13) + Persona (P14) + Roles
+ Skills + Tools + MCP + capability scopes + trust tiers. A "Chef agent"
is that same agent, configured — never a second codebase. Forking would
mean porting every hardened foundation fix forever; a pack **inherits**
them. For an *ecosystem* ("Aivyx PA core + a marketplace of vertical
packs"), this is the load-bearing decision.

`aivyx-core` / `aivyx-capability` / the daemon / the audit chain stay
**domain-neutral**. The domain lives only in the pack.

## 2. The pack format

A vertical pack is up to six things, each riding an existing primitive:

| Component | Primitive | New code? |
|---|---|---|
| **Template** | `aivyx-pa init --template <name>` (Phase 66) → seeds Profile + default Role | config only |
| **Toolkit crate** | a bundled multi-tool process, same shape as `aivyx-toolkit`/`aivyx-gmail` (Chapter F/G), built against `aivyx-vertical-sdk` (the `Tool` trait + `run_multi_tool_subprocess`) | new sibling crate |
| **Scopes + gate policy** | capability scope bases (additive to `aivyx-capability` `KNOWN_BASES`) + trust ceiling + gates | additive bases |
| **Team (Nonagon)** | a customised `aivyx_team::TeamConfig` — a lead + ≤8 least-privileged specialists (nine members in all) over the pack's scopes — loaded by `aivyx-pa team run --config <pack.toml>` (Chapter J) | config (TOML) |
| **Skills bundle** | starter conversationally-taught `LearnedSkill`s | config only |
| **Integrations** | `aivyx-pa connect` tool-processes / MCP servers (Chapter F) | config only |

The **Team** component is what makes a pack a *force multiplier*: the same
free engine, shaped into a domain expert crew. The kitchen pack's BOH Nonagon
(Aria + stocktake / inventory / purchasing / HACCP) is the worked example —
see [`NONAGON.md`](NONAGON.md) §9 and `crates/verticals/aivyx-kitchen`.

The only Rust that changes outside the pack crates is **additive scope
bases** in `aivyx-capability` (exactly how `web.search`, `gmail.*`,
`task.*` were added) — never a substrate fork. Everything else a pack needs
comes through the `aivyx-vertical-sdk` facade.

### Where a pack lives (settled topology, 2026-06-26)

A pack is two crates (a team-config crate + a toolkit tool-process crate), each
depending on **`aivyx-vertical-sdk` alone** for its shipping code. *Where* those
crates sit is now decided by **visibility**, because the core repo is public:

- **The open example (Kitchen) stays in the core repo**, in-tree at
  `crates/verticals/` — it is public on purpose, the worked answer to "how do I
  build a pack?", meant to be read and copied.
- **Paid packs live in a separate, private repo — `aivyx-verticals`** (a sibling
  of `aivyx` under `~/Projects/Rust/`; see the ecosystem topology). Private code
  never sits in a public repo behind only a `.gitignore` — the separate repo *is*
  the boundary. `aivyx-verticals` is a workspace of paid packs; create it when the
  **first paid pack exists** (until then, the in-tree `crates/verticals-private/`
  is just a scaffold — a `README` + `.gitignore`, no code).

**Building a private pack against the core** (the one real wrinkle — core crates
are `publish = false`):

- **Local dev:** path-dep `aivyx-vertical-sdk` at `../aivyx/crates/aivyx-vertical-sdk`
  (works because the repos are siblings).
- **CI / portability:** `git`-tag-dep the SDK (`publish = false` does *not* block
  git deps).

Because the SDK is the **only** engine surface a pack compiles against, it is the
clean cut-seam between repos: a paid pack pins `aivyx-vertical-sdk` (semver-stable)
and nothing else from the core, so engine refactors can't break it and the public/
private split costs nothing structurally. *(The in-tree `crates/verticals-private/*`
glob remains available as an optional local-dev convenience — drop a private pack
there to build it inside the core workspace — but the separate repo is the home.)*

---

## 3. Worked example: the Kitchen / BOH pack

### 3.1 The integration model — KitchenDB is the system of record

The existing **KitchenDB** (Supabase/Postgres, RPC-first) is the source
of truth. The agent does **not** reimplement the domain — it calls the
existing `public.*` RPCs. Two front-ends coexist on one DB:

```
        KitchenDB  (Supabase/Postgres — system of record, RPC API)
          ▲                                   ▲
          │ RPCs                              │ RPCs (read/write, gated)
   Kitchen OS (Flutter)              aivyx-kitchen toolkit
   rich GUI for managers      ←→     Aivyx PA agent: chat / voice / loop / TUI
```

The Flutter app is the *look-at* surface (dashboards, bulk entry); the
agent is the *talk-to* surface (voice on the line, overnight reorder
loop, HACCP logging, compliance export).

> **Dedicated toolkit, not raw `postgres` MCP.** A raw SQL `execute`
> tool would bypass the DB's RLS, the `_v2` API contract, and domain
> safety — the wrong amount of power for a kitchen. The toolkit wraps
> RPCs as *typed, individually-scopable, individually-gateable* tools.
> Raw postgres MCP stays available for operator debugging behind a
> high-trust tier.

### 3.2 RPC → tool surface (from the real KitchenDB)

The stable contract is the `public.get_*` / `public.*_v2` functions.
Mapping (✓ = Phase-1 read-only spike; ⚑ = gated; ⚑⚑ = double-gated):

| Agent tool | KitchenDB RPC | Gate |
|---|---|---|
| `inventory.list` ✓ | `get_inventory_items_with_details_v2(p_organization_id)` | — |
| `inventory.low_stock` ✓ | `get_low_stock_item_count_v2` | — |
| `inventory.value` ✓ | `get_total_inventory_value_v2` | — |
| `inventory.movements` | `get_inventory_item_movement_history_v2` | — |
| `inventory.waste` | `get_recent_waste_value_v2` / `get_top_wasted_items_v2` | — |
| `inventory.cogs` | `get_cogs_for_period_v2` | — |
| `inventory.count.open` | `create_inventory_count_with_items_v2` | ⚑ |
| `inventory.count.submit` | `process_inventory_count_v2` | ⚑ |
| `inventory.item.upsert` | `create/update_inventory_item_v2` | ⚑ |
| `recipe.search` ✓ | `search_recipes_v2` / `search_recipes_by_ingredients_v2` | — |
| `recipe.scale` ✓ | *(none — pure compute the DB doesn't do)* | — |
| `recipe.cost.refresh` | `refresh_recipe_ingredient_costs_v2` | ⚑ |
| `recipe.dashboard` | `get_recipe_dashboard_data_v2` | — |
| `production.batches` | `get_production_batches` | — |
| `production.batch.*` | `kitchen_production.batches_public_insert/update` | ⚑ |
| `po.list` | `get_purchase_orders` | — |
| `po.receive_item` | `receive_po_item_v2` | ⚑ |
| `po.send` | `purchase_orders_public_insert` + supplier dispatch | ⚑⚑ |
| `receiving.list` | `get_receiving_events` | — |
| `receiving.process` | `process_and_mark_receiving_event_v2` | ⚑ |
| `supplier.list` ✓ | `get_suppliers` / `get_suppliers_with_categories_v2` | — |
| `alert.list` | `get_alerts` | — |
| `alert.resolve` | `simple_mark_alert_resolved` | ⚑ |
| `task.list` | `get_tasks` | — |
| `haccp.log` ✓ | *(none — validated record anchored on the HMAC audit chain; no new DB table)* | append-only |
| `prep.list` | derived from `get_production_batches` + menu | — |

All RPCs take `p_organization_id` (multi-tenant) and run under
`tenancy.require_org_context()`; the toolkit config carries the org id +
the PostgREST credentials (operator-provided, per-tool-process token at
`0600` — the Chapter F pattern).

### 3.3 Scopes + gate policy (the trust model, applied)

- `kitchen.read.*` — open (list, value, search, alerts).
- `kitchen.write.*` — **gated** (counts, adjustments, batch lifecycle).
- `kitchen.order.send` — **double-gated** (a PO spends money; a human
  approves every time, even inside the autonomous loop).
- `kitchen.haccp.log` — **append-only**, every entry on the **HMAC
  audit chain**.

These become additive bases in `aivyx-capability::KNOWN_BASES`.

### 3.4 The compliance wedge

`haccp.log → audit chain` is the differentiator: fridge-temp checks,
corrective actions, use-by overrides become HMAC-chained,
offline-verifiable, exportable records (the TUI `audit` screen,
repopulated with `haccp.*` events). "Every food-safety action is
cryptographically recorded; one command exports the EHO pack" is a claim
most kitchen software can't make — **to be marketed only after the chain
verification + HACCP semantics are independently checked.**

### 3.5 Autonomous reorder — wiring the loop

The headline autonomy story, and the reason the gate model matters. The
loop itself is **existing daemon machinery** (`aivyx-pa loop`, the
HMAC-chained backlog, the `max_iterations` / wall-clock / token caps,
driver-side gate verification) + the **`[[schedule]]`** cron triggers —
the kitchen pack doesn't reimplement any of it. It just provides the
tools and a starter routine; the loop points at them.

**The nightly flow:**

```
[[schedule]] cron 02:00  →  aivyx-pa loop add "nightly par reorder"
        aivyx-pa loop start --max-iterations 3
                │
   ┌────────────┴─────────────────────────────────────────────┐
   │  agent works the story with the kitchen tools:            │
   │   1. kitchen.inventory.low_stock        (kitchen.read)    │
   │   2. kitchen.par.reorder  → draft POs   (kitchen.read)    │  ← runs unattended
   │   3. kitchen.po.send {confirmed:false}  (kitchen.order.send)
   │        → confirm-first GATE: no human at 02:00 →          │
   │          dispatches NOTHING; surfaces the draft POs        │  ← stops at the money step
   └───────────────────────────┬──────────────────────────────┘
                               │  morning
   operator reviews the draft POs  →  approves  →  kitchen.po.send {confirmed:true}
                                                    (one PO per supplier dispatched)
```

The loop does the tedious analysis **unattended overnight** and produces
ready-to-approve, per-supplier draft POs (`kitchen.par.reorder` now
emits `purchase_orders` grouped by supplier — one per `po.send`). It
**cannot** spend money autonomously: `kitchen.order.send` is
Trusted-tier *and* confirm-first, so the loop halts at the gate and
waits for a human. That is the whole safety argument — **autonomy up to
the consequential step, a human at it** — and it's enforced by two
independent mechanisms (the capability scope + the confirm-first
protocol), both audited.

**Starter skill (ships with the pack's skills bundle):**

> *"Nightly par reorder: read low stock, run `kitchen.par.reorder` to
> draft per-supplier POs, and present the drafts for approval. Never
> call `kitchen.po.send` with `confirmed: true` — leave that for the
> operator."*

---

## 4. Phasing

1. **Read + compute spike** — `aivyx-kitchen` crate: the KitchenDB RPC
   client + the `tools::catalog()` tool set (8 tools:
   `inventory.list/low_stock/value`, `recipe.search`, `recipe.scale`
   *(custom pure compute)*, `supplier.list`, `po.list`, `alert.list`) +
   a `dispatch` executor. All `kitchen.read`, ungated; zero write risk.
   Each `KitchenTool` descriptor is the spec for its `aivyx_core::Tool`
   impl. *(this PR)*
2. **Tool-process wiring** ✅ — `kitchen.read` registered in
   `aivyx-capability::KNOWN_BASES` (Trusted ceiling) + a generic
   `KitchenToolBinding` adapting every catalog entry to
   `aivyx_core::Tool` + the `aivyx-kitchen` binary serving them via
   `run_multi_tool_subprocess`. Register in `aivyx-pa.toml`:
   `[[tool_process]]` `name = "kitchen"`, `command =
   "…/aivyx-kitchen"`. *(done — needs a live KitchenDB + config to run
   end-to-end)*
3. **Gated writes** ✅ — `kitchen.par.reorder` (pure compute — what to
   order back to par; `kitchen.read`, ungated) + `kitchen.po.send` (the
   money action — `kitchen.order.send` scope **and** confirm-first) +
   the inventory writes (count-based, the KitchenDB model):
   `kitchen.inventory.count.open` (scope-gated draft),
   `kitchen.inventory.count.submit` (confirm-first — commits counted
   quantities to on-hand), `kitchen.inventory.item.upsert` (scope-gated
   create/update). The confirm-first gate is generalized
   (`require_confirmed`); `kitchen.write` / `kitchen.order.send`
   registered.
4. **Loop + PO** ✅ — `kitchen.par.reorder` now emits per-supplier draft
   POs (`group_into_pos`); the nightly autonomy flow is wired via the
   existing `aivyx-pa loop` + `[[schedule]]` machinery (§3.5): unattended
   draft, halt at the confirm-first `po.send` gate, human approves in
   the morning. Live supplier dispatch (the real `po.send` payload) is
   the remaining integration detail.
5. **HACCP + audit** 🚧 — `kitchen.haccp.log` built: a validated,
   canonicalized food-safety record (enforces *out-of-limit → corrective
   action*), ungated + append-only, whose call lands on the
   tamper-evident HMAC chain (tool id, scope, input hash, time,
   outcome). No new DB table — the chain *is* the anchor. **Next:** a
   durable record store + the EHO export (the chain filtered to
   `kitchen.haccp.log`, paired with the records its input hashes
   anchor).
6. **`kitchen` template + skills bundle** ✅ — `aivyx-pa init --template
   kitchen` (examples/templates/aivyx-kitchen.toml, wired into the
   bundled-template registry): a BOH role with the `kitchen.*` scopes,
   the `aivyx-kitchen` `[[tool_process]]`, and the opt-in nightly
   reorder `[[schedule]]`. The starter skills bundle (reorder /
   fridge-temp / cook-hold / stocktake / recipe-scale) ships as
   `aivyx_kitchen::pack::skills_json()`, installed via `skills.teach`.
   The pack is now installable end to end.

## 5. Open questions

- **Distribution shape** — managed/hosted product vs. self-installed
  local pack (changes whether a fleet/hosting story is needed on top of
  the local-first base).
- **First integration targets** — which POS / supplier / inventory
  systems beyond KitchenDB (the moat is integrations, not the agent).
- **Pack format as a first-class artifact** — the *code* boundary is now
  formalized by `aivyx-vertical-sdk` (a pack compiles against one stable
  crate). Still open: bundling template + toolkit + skills + scopes into a
  single installable artifact, and the out-of-tree / separate-repo packaging
  once a second pack exists.
- **Brand** — the Kitchen OS Flutter UI uses an Aivyx PA Studio-inspired
  coral/purple palette; the Aivyx PA TUI uses amber-on-near-black. Reconcile
  if the agent and the app are to feel like one product.

---

## 6. Build your own pack — step by step

This is the tutorial. It mirrors the Kitchen pack exactly, so every step
points at a real file you can open and copy. A pack is **two sibling crates**
under `crates/verticals/` — a team-config crate (`aivyx-kitchen`) and a
toolkit tool-process crate (`aivyx-kitchen-toolkit`) — plus a small, additive
touch to one engine crate (the scope bases) and some operator config. You
write Rust against **`aivyx-vertical-sdk` only**.

### Step 0 — the dependency rule

Both pack crates depend on the facade and nothing else from the engine:

```toml
# crates/verticals/<your-pack>/Cargo.toml
[dependencies]
aivyx-vertical-sdk = { path = "../../aivyx-vertical-sdk" }
```

Everything you need is re-exported under three modules — `capability`
(`Scope`, `TrustTier`), `team` (`TeamConfig`, `TeamMember`, `MissionPlan`,
`Step`, `attenuate_for_member`), and `tool` (`Tool`, `ToolContext`,
`ToolOutcome`, `AivyxError`, `Verification`, `run_multi_tool_subprocess`) —
or pull them all in with `use aivyx_vertical_sdk::prelude::*;`. **Do not add
`aivyx-core` / `aivyx-team` / `aivyx-capability` / `aivyx-tool` as regular
dependencies** — if you find you need something they expose, add it to the
facade first (one reviewed place), don't reach around it.

> **The one exception: tests.** A pack's e2e *test harness* drives the engine
> directly (mock `ChannelContext`, the tool-process bridge) — those engine
> crates are fine as **`[dev-dependencies]`**, exactly as the engine crates
> test each other. See `aivyx-kitchen-toolkit/Cargo.toml`. The boundary is
> about shipping code, not test scaffolding.

### Step 1 — the team crate (the Nonagon)

Reference: [`crates/verticals/aivyx-kitchen/src/lib.rs`](../crates/verticals/aivyx-kitchen/src/lib.rs).

A team crate ships a `TeamConfig` (a lead + ≤8 least-privileged specialists (nine members in all))
and the `MissionPlan`(s) they run. The pattern:

```rust
use aivyx_vertical_sdk::capability::TrustTier;
use aivyx_vertical_sdk::team::{DialogueConfig, MissionPlan, Step, TeamConfig, TeamMember};

fn member(name: &str, role: &str, soul: &str, tools: &[&str], scopes: &[&str]) -> TeamMember {
    TeamMember {
        name: name.to_string(),
        role: role.to_string(),
        soul: soul.to_string(),                                   // the member's voice/judgment
        tool_allowlist: tools.iter().map(|s| s.to_string()).collect(),
        capability_scopes: scopes.iter().map(|s| s.to_string()).collect(),
        trust_ceiling: TrustTier::Trusted,
    }
}

pub fn my_team() -> TeamConfig { /* lead + specialists; see kitchen_boh_team() */ }
pub fn my_mission() -> MissionPlan { /* Step::delegate(...).after([...]); a DAG */ }
```

Two rules the engine enforces, so design for them:

- **NT-02 least privilege** — every specialist's authority is *attenuated* to
  a subset of the operator's own real, configured authority (not the lead's
  own declared scopes specifically — a purely-orchestration lead can still
  delegate whatever the operator's real floor allows, even if it never
  declares those scopes for itself). Give each member only the scopes its
  job needs. Kitchen's `haccp` member holds *only* `kitchen.haccp.log` — it
  physically cannot send a PO or read inventory, and a test proves it
  (`nt02_haccp_cannot_exceed_aria_and_cannot_order`).
- **Ship the same roster as a committed TOML asset** and round-trip-test it
  against the constructor (`toml_asset_round_trips_with_the_constructor`), so
  the loadable `aivyx-pa team run --config <path>` artifact can't drift from the
  code.

### Step 2 — the toolkit crate (the real tools)

Reference: [`crates/verticals/aivyx-kitchen-toolkit/`](../crates/verticals/aivyx-kitchen-toolkit/) — `src/tools/*.rs`, `src/lib.rs`, `src/main.rs`.

This is a **tool-process**: a separate binary the daemon spawns (Chapter F/G
substrate pattern, same as `aivyx-gmail`). Each domain action is an
`impl Tool`; `required_scope` returns the `Scope` the call needs:

```rust
use aivyx_vertical_sdk::capability::Scope;
use aivyx_vertical_sdk::tool::{Tool, ToolContext, ToolId, ToolOutcome};

#[async_trait::async_trait]
impl Tool for InventoryList {
    fn id(&self) -> ToolId { /* ... */ }
    fn name(&self) -> &str { "kitchen.inventory.list" }
    fn description(&self) -> &str { "List current inventory items." }
    fn input_schema(&self) -> &serde_json::Value { /* JSON Schema */ }
    fn required_scope(&self, _input: &serde_json::Value) -> Scope {
        Scope::parse("kitchen.read").expect("registered base")
    }
    async fn execute(&self, input: serde_json::Value, ctx: &ToolContext<'_>) -> ToolOutcome {
        // call the system of record (here: KitchenDB PostgREST), return a ToolOutcome
    }
}
```

Gathered into a set and served from `main.rs`:

```rust
// src/lib.rs — one place both the binary and the coherence test use
pub fn all_tools(client: Arc<KitchenClient>) -> Vec<Arc<dyn Tool>> { /* ... */ }

// src/main.rs
run_multi_tool_subprocess(all_tools(client)).await
```

Gate policy lives in `required_scope` + the confirm-first flag:

- A **read** tool returns a read scope (`kitchen.read`) — open within tier.
- A **write** tool returns a write scope (`kitchen.write`) — gated.
- A **consequential** tool (spends money, dispatches externally) gets its
  *own* base **and** is confirm-first (`kitchen.order.send` — a human
  approves every send, even inside the autonomous loop). Keep the reversible
  half separate: Kitchen splits `kitchen.order.draft` (gated write, no
  confirm) from `kitchen.order.send` (confirm-first) so the loop can draft
  unattended and halt at the money step.
- An **append-only audit** tool lands every call on the HMAC chain
  (`kitchen.haccp.log`).

### Step 3 — register the scope bases (the one engine touch)

Add your `your.*` bases to `aivyx-capability`'s `KNOWN_BASES`, with the tier
ceiling each sits below — the *only* edit outside the pack crates, and
purely additive (the same move that added `web.search`, `gmail.*`). Unknown
bases fail `Scope::parse`, so this is what makes your scopes real. Mirror it
in `docs/TOOLS.md` (a drift-guard test checks the catalog against
`KNOWN_BASES`).

### Step 4 — wire it into the daemon + onboarding

- **Workspace** — for an in-tree pack, add both crates to `members` +
  `default-members` in the root `Cargo.toml`. For a **private** pack, drop it
  in `crates/verticals-private/` instead — the `crates/verticals-private/*`
  member glob picks it up automatically, no `Cargo.toml` edit (and it stays
  git-ignored). Either way the toolkit binary wants
  `[package.metadata.dist] dist = false` so only the top-level `aivyx-pa` binary
  ships as a release artifact.
- **Tool process** — `[[tool_process]]` in `aivyx-pa.toml` (`name`, `command`
  → the built binary) so the daemon spawns it and its tools reach the team
  via the `tool_list → TeamAssembly::base_tools` path.
- **Team config** — point `[team] config_path` at your roster TOML (or ship
  it as a template).
- **Onboarding** — a `aivyx-pa connect <pack>` branch that writes the pack's
  config and probes reachability (the non-OAuth vertical pattern, Chapter
  Mise: `aivyx-pa connect kitchen` writes `[kitchen_db]`, plants the roster,
  and probes KitchenDB), plus an `aivyx-pa doctor` section.

### Step 5 — test it

Three test shapes, all in the Kitchen crates:

1. **Team/mission validity** — `team.validate()`, the mission is a DAG,
   every step targets a real member, NT-02 attenuation holds, the TOML asset
   round-trips. (`aivyx-kitchen/src/lib.rs` tests.)
2. **Coherence** — the team's referenced `your.*` tool *names* must match the
   names the toolkit actually provides. (`tests/boh_coherence.rs`.)
3. **Real-binary e2e** — drive the actual built tool-process over the harness
   against a mock system-of-record, asserting a real tool call round-trips.
   (`tests/harness_e2e.rs` — this is the test that legitimately uses the
   engine crates as dev-dependencies.)

That is a complete pack: a domain crew + real gated tools + additive scopes +
daemon wiring + onboarding, all over the free engine, with the only stable
surface you build against being `aivyx-vertical-sdk`.

---

## 7. Pack anatomy — the canonical skeleton

The reference layout, generalized from the Kitchen pack. Every pack is **two
crates**: a **pack crate** (the crew + the plan — pure config) and a **toolkit
crate** (the real, gated domain tools — a tool-process binary). Copy this tree
and rename `<domain>`:

```
aivyx-<domain>/                     # the PACK crate — the Nonagon crew + missions
  Cargo.toml                        # deps: aivyx-vertical-sdk ONLY (shipping code)
  src/lib.rs                        # <domain>_team() -> TeamConfig
                                    #   + <flagship>_mission() -> MissionPlan
                                    #   + tests: team.validate(), DAG, NT-02, TOML round-trip
  assets/
    <domain>.toml                   # the committed TeamConfig as TOML (the loadable roster)

aivyx-<domain>-toolkit/             # the TOOLKIT crate — the real domain tools (a binary)
  Cargo.toml                        # deps: aivyx-vertical-sdk; integration client deps
                                    #   (reqwest/sqlx/…); engine crates only as dev-deps
  src/
    main.rs                         # the tool-process entrypoint: run_multi_tool_subprocess(tools)
    lib.rs                          # assembles the tool list; re-exports for tests
    config.rs                       # the integration config (endpoint, creds via env/token file)
    client.rs                       # the system-of-record client (RPC/SQL/HTTP)
    tools/
      mod.rs                        # collects the <domain>.* Tool impls into the list
      <area>.rs                     # one file per tool group — each a `Tool` impl with a
                                    #   pure required_scope(input) + execute(); gated by base
  tests/
    <domain>_coherence.rs           # the team's referenced tool NAMES match what the toolkit provides
    harness_e2e.rs                  # drives the REAL built binary over the harness vs a mock SoR
```

**The contract every pack honours (the framework, in one place):**

| Must provide | Where | Rule |
|---|---|---|
| A **`TeamConfig`** — a lead + ≤8 least-privileged specialists | pack `src/lib.rs` + `assets/<domain>.toml` | each member's scopes ⊆ the operator's own real, configured authority (**NT-02**); ≤9 agents total |
| At least one **`MissionPlan`** (a DAG) | pack `src/lib.rs` | validates: acyclic, every step targets a real member, no dead steps |
| The **domain tools** as `Tool` impls | toolkit `src/tools/*.rs` | each has a **pure `required_scope(input)`**; side-effects gated by a base; money/outbound = **confirm-first**; append-only logs never updated/deleted |
| **Scope bases** the tools need | one engine touch — `KNOWN_BASES` in `aivyx-capability` | additive; the only edit a pack makes to the core. Group bases (`<domain>.read/write/...`) keep the surface small |
| An **integration boundary** | toolkit `config.rs` + `client.rs` | the system-of-record (a DB, an API); creds via env / per-tool-process token file, never hard-coded |
| **Install + reachability** | `aivyx-pa connect <domain>` + an `aivyx-pa doctor` section | writes the pack's config, plants the roster (no-clobber), probes the SoR = the "connected" signal |
| **Three test shapes** | as above | team/mission validity · name-coherence · real-binary e2e vs a mock SoR |

**What a pack must NOT do** (the invariants that keep it maintainable + safe):

- **Never depend on engine crates in shipping code** — only `aivyx-vertical-sdk`
  (engine crates allowed *only* as `tests/` dev-deps for the real-binary e2e).
- **Never fork or patch the substrate** — a pack is configuration + tools; it
  inherits every future core hardening for free.
- **Never widen trust** — a pack adds scope *bases* and *gated tools*; it can't
  raise a tier ceiling, bypass a gate, or grant itself authority. The capability
  model is the core's, applied.
- **Never put domain logic in the core** — `aivyx-core` / `aivyx-capability` /
  the daemon stay domain-neutral; the domain lives only in the pack.

**Sizing a pack** (rough): a pack crate is ~one `lib.rs` (a `TeamConfig` + one or
two `MissionPlan`s) + a TOML asset; a toolkit is ~one tool file per tool group +
a client + a config. Kitchen is ~11 tools across 4 bases — a comfortable
reference size. A pack that needs new *I/O reach* the engine doesn't have is a
sign it wants a new core capability first (raise it as a core chapter), not a
pack workaround.
