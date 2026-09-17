# Kitchen Toolkit — give the BOH brigade real tools (Chapter Brigade)

> **Status:** ✅ **COMPLETE (BG.0–BG.5).** The first vertical pack's specialists
> can lead, delegate, and talk — but they can't *act*: the BOH Nonagon
> ([Chapter J](NONAGON.md) + the editable roster of [Chapter Roster](ROSTER.md))
> declares `kitchen.*` scopes, yet **no `kitchen.*` tools exist**. Chapter Brigade
> builds them: a **tool-process binary** (`aivyx-kitchen-toolkit`, the
> [Chapter F/G](VERTICAL_PACKS.md) substrate pattern) that talks to the operator's
> **KitchenDB** (Postgres + PostgREST RPCs) and registers the kitchen tool surface
> — inventory/recipe/supplier reads, stock/batch writes, purchase-order dispatch
> (confirm-first), and append-only HACCP logging. The `kitchen.*` capability bases
> are **already in `KNOWN_BASES`**, so there is **no new base, no P10 amendment, no
> change to `aivyx-core`/capability/daemon**. Declared as a `[[tool_process]]`, the
> toolkit's tools join the daemon `tool_list` that already feeds
> `TeamAssembly::base_tools` — so the BOH brigade gets them, capability-attenuated
> (NT-02), with no team-engine change.

## 1. Why this chapter

The team arc is mature — Nonagon (J), daemon-side missions (L), editable rosters
(Roster). The kitchen vertical pack ships a 9-role BOH brigade (`kitchen-boh.toml`)
whose members declare least-privilege `kitchen.*` scopes. But those scopes gate a
tool surface that **was never built**: `aivyx-capability` has the `kitchen.read /
write / order.send / haccp.log` bases, the pack names them in tool allowlists, and
the daemon attenuates them at spawn — yet `grep` finds no `kitchen.*` `Tool` impl.
So the brigade can decompose a goal and dialogue, then stall: there is nothing to
count stock with, no PO to dispatch, no HACCP row to write. Chapter Brigade closes
that gap — the first proof that a **vertical pack ships real domain tools** — and
turns the whole J/L/Roster team machinery into something that does commercial work.

## 2. Architecture & governance decisions (locked)

### A tool-process binary — third-party tier, the F/G substrate pattern
A new binary crate **`aivyx-kitchen-toolkit`** (mirroring `aivyx-gmail` /
`aivyx-toolkit`): `main.rs` runs the **multi-tool harness**
(`aivyx_tool::run_multi_tool_subprocess`), registering the kitchen tools. It is a
**third-party tool process** per P10/P11/P12 — **not** in the thirteen-tool core
cap, **not** an `aivyx-core` change. The operator declares it as a
`[[tool_process]]` in `aivyx-pa.toml`; the daemon spawns it and proxies its tools
(`ToolProxy` over the bridge) into the live `tool_list`. The `kitchen.*` bases
**already exist** in `KNOWN_BASES` (registered when the pack landed), so Chapter
Brigade adds **no capability base and is not a P10 amendment** — it is the
tool-process tier, exactly like [Chapter Abacus](ABACUS.md)'s toolkit surface.

### KitchenDB is the system of record — the agent calls it, never reimplements
The toolkit is a thin **PostgREST RPC client**: `POST <base_url>/rpc/<fn>` with
`p_organization_id` + params, auth from config. The operator's **KitchenDB** (the
mature `kitchen_os_db` Postgres domain model — inventory, recipes, production,
purchase_orders, receiving, suppliers, HACCP) owns all domain logic via its stable
`get_*` / `*_v2` RPCs. The agent **calls** those RPCs; it never re-encodes inventory
math or PO rules. Operator config lives at
`~/.aivyx-pa/tool-processes/kitchen/config.toml`: `base_url`, `api_key` (the PostgREST
`apikey` / bearer), `organization_id` (the multi-tenant key every RPC takes). No
secret is ever logged; the config file is `0600` (the substrate norm).

### Reaches the brigade with no team-engine change
A tool process's tools land in the daemon's `tool_list`, which Chapter J/Roster
already hand to `TeamAssembly::base_tools`. A BOH specialist whose `tool_allowlist`
names `kitchen.read` (etc.) receives exactly that tool, **capability-attenuated to
`declared ∩ lead`** at spawn (NT-02, unchanged). So the payoff — the brigade can
act — is delivered by the **existing** wiring; Chapter Brigade builds tools, not
plumbing.

### Confirm-first on what's irreversible or outbound
`kitchen.order.send` **dispatches a purchase order — money leaves the building** —
so it is **confirm-first**: it returns `RequiresEscalation` unless invoked with
`confirmed: true` (the established `git.commit` / `skills.teach` pattern). [Chapter
H](HEADLESS_MODE.md) already treats it as structurally blocked under any
non-interactive policy — that invariant holds here for free. Reads and the
append-only `kitchen.haccp.log` are not confirm-first. Each `kitchen.haccp.log`
call is **one row on the HMAC audit chain** — the tamper-evident HACCP record the
vertical's pitch rests on (EHO export), achieved without a new `AuditEvent` variant
(so no [[e2e-audit-chain-count-assertions]] breakage).

## 3. Scope

**In:** the `aivyx-kitchen-toolkit` binary crate + PostgREST RPC client + config
(BG.1); the read tools `kitchen.read` — inventory list / low-stock / value, recipe
search, supplier list (BG.1); the gated write tools `kitchen.write` — stock
adjustment / production-batch lifecycle (BG.2); `kitchen.order.send` — confirm-first
PO dispatch (BG.3); `kitchen.haccp.log` — append-only HACCP write, audit-chained
(BG.4); registration as a `[[tool_process]]` + a BOH-brigade live-tools check
(BG.4); tests (RPC request/parse + harness gating) + an end-to-end `multi_harness`
IPC drive against a mock PostgREST + an operator live-DB runbook (BG.5). **Out:**
re-implementing any KitchenDB domain logic (the DB owns it); a new capability base
or P10 amendment (the `kitchen.*` bases exist); changes to `aivyx-core` / capability
/ the team engine / the daemon tool-list path; the Kitchen OS Flutter GUI (stays the
operator's rich front-end); a second vertical; bundling Aivyx PA's own OAuth or a
hosted KitchenDB (the operator runs their own).

## 4. Phase plan (docs-first, small phases per convention)

| Phase | Deliverable | Notes |
|---|---|---|
| **BG.0** 🟡 | **This design contract** | Locked reference; banner flips per phase. |
| **BG.1** ✅ | **Crate + RPC client + read tools** | DONE. New `aivyx-kitchen-toolkit` binary crate (workspace member, `dist=false`) over `run_multi_tool_subprocess`. `KitchenClient::call_rpc` = `POST <base>/rpc/<fn>` with `apikey`+`Bearer` headers and **client-injected `p_organization_id`** (always wins over caller-supplied — the tenant is the client's, not the LLM's); typed `KitchenError` (BadParams/Http/Status/Parse). `config.rs` loads `[kitchen_db]` (base_url/api_key/organization_id) from `~/.aivyx-pa/tool-processes/kitchen/config.toml` (NotFound/MissingKitchenDb/Parse distinct). 5 `kitchen.read` tools: `kitchen.inventory.list` (optional `location`→`p_location`), `.low_stock`, `.value`, `kitchen.recipe.search` (`query`→`p_query`), `kitchen.supplier.list`; array→`{<key>:[...],count}`, scalar→`{<key>:v}`. **No new capability base** (kitchen.read already in KNOWN_BASES). 23 tests (config ×5, client ×7 incl. in-process PostgREST mock asserting path/auth/tenant-injection/error-status/parse, tool param-mapping + shape + names/scopes); clippy `-D warnings` + `cargo deny` green. RPC fn names (`get_inventory`/`get_low_stock_items`/`get_inventory_value`/`search_recipes`/`get_suppliers`) are the assumed KitchenDB convention — confirmed vs the live schema in-phase (OQ-3). |
| **BG.2** ✅ | **Gated write tools** | DONE. 3 `kitchen.write` tools reusing the BG.1 client via a new `run_write` helper (wraps the KitchenDB row as `{<key>:v}`, `Verification::Unverified` — Ok from the DB, no separate confirming read): `kitchen.inventory.adjust` (`sku`+signed `delta`+optional `reason` → `adjust_inventory`), `kitchen.batch.start` (`recipe_id`+positive `quantity`+optional `notes` → `start_production_batch`), `kitchen.batch.complete` (`batch_id`+optional `actual_yield` → `complete_production_batch`). KitchenDB owns the batch state machine + stock math; the tools only map params. No new base (`kitchen.write` already in KNOWN_BASES). +8 tests (param mapping incl. required/positivity/type rejects + names/scopes); crate at 31 tests, clippy `-D warnings` green. |
| **BG.3** ✅ | **PO dispatch (confirm-first)** | DONE. `kitchen.order.send` (own base, distinct from `kitchen.write` — a roster can grant stock edits without ordering power) dispatches a drafted PO (`purchase_order_id` + optional `notes` → `send_purchase_order`). A pure `decide_order` applies the gate: malformed → `Failed`; well-formed but unconfirmed → **`ToolOutcome::RequiresEscalation`** (the daemon turns it into a human gate; [Chapter H](HEADLESS_MODE.md) auto-blocks it); `confirmed: true` → the RPC. +5 tests (escalate-when-unconfirmed incl. `confirmed:false`, send-when-confirmed with trimming, hard-error-on-missing-id-even-when-confirmed, name/scope); crate at 36 tests, clippy `-D warnings` green. |
| **BG.4** ✅ | **HACCP + registration** | DONE. `kitchen.haccp.log` (own base; append-only; `check_type` required + optional `value`/`unit`/`location`/`passed`/`notes` → `log_haccp_record`; one HMAC audit row per call = tamper-evident, no new `AuditEvent`). `all_tools()` registry (10 tools across 4 bases) shared by the binary + tests. **Reconciled the BOH pack** (`kitchen_boh_team()` + `kitchen-boh.toml`) — its specialist `tool_allowlist`s named aspirational tools (`inventory.count`/`po.draft`/`haccp.log`) that didn't exist; now name the real toolkit tools (stocktake→`kitchen.inventory.list`+`.adjust`, inventory→`.low_stock`, purchasing→`kitchen.supplier.list`+`kitchen.order.send`, haccp→`kitchen.haccp.log`). A cross-crate **coherence test** (`tests/boh_coherence.rs`, `aivyx-kitchen` dev-dep, no cycle) fails if the pack ever references a `kitchen.*` tool the toolkit doesn't provide. Registration recipe (§6) + crate at 41 tests, clippy `-D warnings` green. |
| **BG.5** ✅ | **Finalize** | DONE. `tests/harness_e2e.rs` drives the **real built `aivyx-kitchen-toolkit` binary** over the multi-tool harness (`ToolProcessBridge::spawn` + `ToolProxy::execute`, HOME pointed at a temp config) against an **in-process mock PostgREST**: registers all 10 tools, runs `kitchen.inventory.list`, asserts the shaped `{items,count}` — full config→register→IPC→RPC→shape path on a built artifact (graceful-skip if spawn is blocked). Operator live-KitchenDB runbook added (§7). Full workspace suite (103 ok suites, 0 failures) + clippy `-D warnings` + `cargo deny` green. |

**Discipline:** the RPC client is structured so the request-build + JSON-parse halves
are testable without a live server (a transport seam + canned fixtures); the live
KitchenDB is a **runbook, not a CI dependency**. The one new dependency is the HTTP
client (`reqwest`, already in-tree for `web.search` / Google integrations) — confirm
`cargo deny` stays green. Test band: **moderate–high** — per-tool request/parse +
harness gating + the e2e drive; price **~30–45 new tests**.

## 5. Open questions (resolve in-phase)

- **OQ-1 — crate home (BG.1).** A dedicated `aivyx-kitchen-toolkit` binary crate
  (locked lean — the substrate convention is one binary per integration, keeps the
  `aivyx-kitchen` pack lib separate from the tools) vs. a `[[bin]]` inside
  `aivyx-kitchen`. Revisit only if the split causes friction.
- **OQ-2 — test transport (BG.1/BG.5).** A transport seam (a trait the real reqwest
  client implements; tests inject canned JSON) **plus** one end-to-end drive against
  a hand-rolled in-process mock — vs. a `wiremock`/`httpmock` dev-dep. Lean the seam
  + minimal in-process mock (no new dev-dep; `cargo deny`-friendly).
- **OQ-3 — RPC surface fidelity (BG.1+).** The exact `get_*` / `*_v2` function names
  + params are confirmed against the operator's live KitchenDB schema in-phase
  (the operator has the DB); the contract fixes the *shape* (org-scoped RPC calls),
  not the catalog.
- **OQ-4 — HACCP depth (BG.4).** Rely on the per-call HMAC audit row for HACCP
  tamper-evidence (locked — no new `AuditEvent` variant, no count-assertion churn)
  vs. a richer HACCP-specific audit event (deferred; additive if EHO export later
  wants structured fields).

## 6. Recipe — wire the kitchen toolkit (BG.4)

**1. Operator config** — point the toolkit at the KitchenDB (PostgREST):

```toml
# ~/.aivyx-pa/tool-processes/kitchen/config.toml  (0600)
[kitchen_db]
base_url = "https://your-kitchen.example/rest/v1"  # PostgREST base, no /rpc
api_key = "..."                                     # PostgREST apikey / bearer
organization_id = "00000000-0000-0000-0000-000000000000"  # the tenant
```

**2. Register the tool process** in `aivyx-pa.toml` — the daemon spawns it and
proxies its `kitchen.*` tools into the live tool list:

```toml
[[tool_process]]
name = "kitchen"
command = "aivyx-kitchen-toolkit"
# Operator CAN narrow, never widen. A read-only deployment, for example,
# drops the write/order/haccp scopes so only the kitchen.read tools surface:
# [tool_process.scope_overrides]
# ...
# Optional: a ceiling that refuses registration outright if the process's
# self-declared scope isn't covered by what you expect (doesn't narrow,
# just validates — see docs/TOOL_SDK.md §6):
# [tool_process.expected_scopes]
# kitchen = "kitchen.read"
```

**3. Run the BOH brigade on it** — point the team at the bundled pack (Chapter
Roster's `[team] config_path`, or `aivyx-pa team init --pack
crates/verticals/aivyx-kitchen/assets/kitchen-boh.toml`):

```toml
[team]
config_path = "kitchen-boh.toml"
```

Now each specialist receives exactly the `kitchen.*` tools its `tool_allowlist`
names, **capability-attenuated to `declared ∩ lead`** at spawn (NT-02): stocktake
gets `kitchen.inventory.list` + `.adjust`, purchasing gets `kitchen.supplier.list`
+ the confirm-first `kitchen.order.send`, HACCP gets only `kitchen.haccp.log`.
The `tests/boh_coherence.rs` drift guard keeps the pack and the toolkit in lockstep.

## 7. Operator live-KitchenDB verification (runbook)

In-tree tests run against an **in-process mock PostgREST** (`tests/harness_e2e.rs`
drives the real binary end to end); the operator's live `kitchen_os_db` is a
**runbook, not a CI dependency**. To verify against a real KitchenDB (~5 min):

1. **Confirm the RPC names.** The toolkit assumes `get_inventory`,
   `get_low_stock_items`, `get_inventory_value`, `search_recipes`,
   `get_suppliers`, `adjust_inventory`, `start_production_batch`,
   `complete_production_batch`, `send_purchase_order`, `log_haccp_record` —
   each org-scoped via `p_organization_id` (OQ-3). Reconcile against
   `\df public.*` in the live DB; rename the `*_FN` constants if they differ.
2. **Write the config** (§6 step 1) with the live `base_url` / `api_key` /
   `organization_id`.
3. **Smoke a read** directly:
   `curl -s -X POST "$BASE/rpc/get_inventory" -H "apikey: $KEY" \
   -H 'Content-Type: application/json' -d '{"p_organization_id":"'$ORG'"}'` —
   confirms the URL + key + tenant before the agent touches it.
4. **Register** the `[[tool_process]]` (§6 step 2), restart the daemon, and ask
   the agent (or run the BOH `team run` overnight-close) — `kitchen.inventory.*`
   reads should return live rows; `kitchen.order.send` should pause for approval.

---

*Chapter Brigade gives the kitchen brigade its knives. The team that Chapters J, L,
and Roster taught to organize — a lead and its least-privileged specialists — can
finally do the work: read the walk-in, adjust a count, draft and (with a human nod)
send the order, and write the HACCP log onto a tamper-evident chain. The system of
record stays KitchenDB; Aivyx PA is the conversational, auditable, gate-safe hands on
it. The first vertical pack stops describing a kitchen and starts running one.*
