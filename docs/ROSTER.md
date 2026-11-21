# Team Roster — create & edit the Nonagon from the Studio (Chapter Roster)

> **Per-role models (Chapter Ensemble).** Each `[[team.member]]` may declare its
> own LLM backend, so a team is no longer one-model-fits-all:
>
> ```toml
> [[team.member]]
> name = "coordinator"
> # ... role / soul / scopes ...
> model = "qwen3:30b-a3b"          # the big brain for the lead
>
> [[team.member]]
> name = "researcher"
> model = "qwen3:8b"               # a fast/cheap model for grunt work
> base_url = "http://gpu-b:11434"  # a second endpoint → TRUE parallel
> ```
>
> Both are optional; omit them and the member uses the team's shared default
> provider + model (byte-identical to before). `model` alone gives role-fit +
> cost. `base_url` points a role at a different **same-kind** endpoint
> (e.g. a second Ollama / GPU) for true local parallelism instead of serializing
> on one server. v1 supports the **Ollama** kind for `base_url`; a per-role
> endpoint uses default Ollama options (not the operator's `[ollama]` tuning),
> and cross-provider-kind teams (e.g. a lead on one local server kind + grunts on another) are a later
> step. The mission driver builds each role's backend at assembly; specialists
> without an override share the daemon's provider.

> **Status:** ✅ **COMPLETE (RO.0–RO.5).** The write half of [Chapter Y](DAEMON_TEAMS.md)'s
> read-only Teams screen and the deferred "Teams" step of [Chapter Genesis](ONBOARDING.md):
> the operator can now **define a team** — pick the lead, add/remove specialists
> (≤9), and edit each member's role / soul / trust ceiling / capability scopes /
> tool allowlist — and have it **persisted + adopted on the next daemon start**.
> The team config becomes a **loadable `[team]`-rooted file** (the same shape packs
> like `kitchen-boh.toml` already use), written through the [Chapter U](FRONTEND.md)
> settings-write machinery, validated by `TeamConfig::validate` before it lands, and
> surfaced as edits in the existing Teams screen + a starter-team choice in the
> Genesis onboarding flow. **No new capability base, no P10 amendment, no new
> dependency** — and the **NT-02 attenuation invariant is untouched**: declaring a
> scope the lead lacks still buys a specialist nothing (`attenuate_for_member` runs
> at spawn, exactly as today).

## 1. Why this chapter

Teams are the last major capability the operator can *see* but not *shape*. The
daemon holds a hardcoded `aivyx_team::default_nonagon()` (built at
`aivyx-cli/src/bin/aivyx.rs:7090`); [Chapter Y](DAEMON_TEAMS.md) renders it
read-only over `GetTeamRoster`, and [Chapter Genesis](ONBOARDING.md) explicitly
deferred team creation to "a future Chapter Roster." So today an operator who wants
a different lead, a tighter specialist, or a domain team has to hand-edit a TOML
file the Studio can't write and the wizard never offers. Roster closes that: the
same roster the Teams screen shows is now the roster the operator *authored*, from
the GUI, with the engine's safety model (validation + attenuation) intact.

## 2. Architecture & governance decisions (locked)

### The team config becomes a loadable file — not hardcoded, not in `aivyx-pa.toml`
The daemon reads its `TeamConfig` from a **dedicated `[team]`-rooted file** (default
path: `team.toml` beside `aivyx-pa.toml`; overridable via a single new pointer key
`[team] config_path` in `aivyx-pa.toml`). Absent file → fall back to
`default_nonagon()` (today's behavior, byte-identical). This matches how packs
already ship a team (`crates/verticals/aivyx-kitchen/assets/kitchen-boh.toml`) and how
`TeamConfig::load` already works — the team document is **self-contained** (`[team]`
+ `[[team.member]]` array-of-tables + `[team.dialogue]`), so it does not belong
inside the hand-maintained `aivyx-pa.toml`. The writer owns the *whole* file, which
means **no `toml_edit` surgery** is needed — `TeamConfig::to_toml()` already
round-trips it (proven by `roster.rs` tests). The writer is just
`validate → to_toml → write_toml_0600` (reusing Chapter U's `0600` helper).

### Write through the Chapter U machinery, validated before it lands
A new **`SetTeamRoster`** write-IPC (the second config-write surface after
[Chapter U](FRONTEND.md)/[V](FRONTEND.md) Settings/Agents). The handler:
1. reconstructs the `TeamConfig` from the wire payload,
2. runs **`TeamConfig::validate()`** (names, uniqueness, every scope parses to a
   known base, lead-is-a-member, the ≤9-specialist Nonagon bound) — invalid roster
   is **rejected with the validator's message**, nothing is written,
3. writes the file `0600`,
4. emits a **`TeamRosterChanged`** audit event on the HMAC chain, and
5. returns **restart-required** (the team service is assembled at boot; a live
   swap of a running Nonagon is out of scope — same "applied on next start" UX the
   Settings screen already has).

### NT-02 is unchanged — the write path widens no authority
Roster persists *declared* member scopes; it does **not** touch
`attenuate_for_member`, which still floors every specialist to `declared ∩ lead`
at spawn. So a roster that declares a specialist scope the lead lacks is *valid*
(it parses) but **buys nothing at runtime** — exactly as a hand-written pack does
today. The screen surfaces this as a soft, non-blocking hint ("the lead doesn't
hold `x`; this specialist won't get it"), never a hard error. **Removing the human
from authoring a team never widens the team's authority** — the same principle
Chapter H (headless) and Chapter U (settings) hold.

### The screen edits the real type; onboarding plants a starter team
The Teams screen gains an **edit mode** over the same wasm-clean `TeamConfig`
[Chapter Y](DAEMON_TEAMS.md) already renders (no mirror type): edit member
fields, add/remove a specialist (client-guarded to ≤9), re-pick the lead, **Save**
(→ `SetTeamRoster`) with the restart-required banner. Genesis gains a **"Choose a
team"** step: keep the default Nonagon, or start from a known pack, then continue —
reusing the same `SetTeamRoster` primitive (CLI + web share it, the W/X seeding
pattern).

## 3. Scope

**In:** the loadable `[team] config_path` + boot load-or-default in the daemon
(RO.1); the shared team-file **writer** (`validate → to_toml → 0600`) + the
`SetTeamRoster` IPC + daemon handler + `TeamRosterChanged` audit + restart-required
(RO.2); the Studio Teams **edit mode** — member CRUD, lead pick, save, the
no-widen hint (RO.3); the Genesis **starter-team** step + `aivyx-pa team` write
affordance (RO.4); tests; a served-in-browser/bundle-embed verify (RO.5).
**Out:** **live** (no-restart) roster swap of a running Nonagon (restart only;
additive later if wanted); editing the per-mission `MissionPlan`/DAG (this edits
*who's on the team*, not *what they run* — that's `aivyx-pa team run`); inventing new
capability bases or tools from the screen (scopes must already parse to a known
base); the vertical-pack **marketplace**/download (Genesis offers known packs, not
a registry); any change to `attenuate_for_member` or the trust model.

## 4. Phase plan (docs-first, small phases per convention)

| Phase | Deliverable | Notes |
|---|---|---|
| **RO.0** 🟡 | **This design contract** | Locked reference; banner flips per phase. |
| **RO.1** ✅ | **Team config as a loadable file** | DONE. `[team] config_path` (`RawTeam` → `AivyxConfig::team_config_path: Option<PathBuf>`) points at a `[team]`-rooted file; `team::resolve_daemon_team_config(configured, base_dir)` resolves it at the team-mission build site — configured path (relative → joined to the `aivyx-pa.toml` dir) → else a conventional `team.toml` beside `aivyx-pa.toml` → else the built-in `default_nonagon()`. **Never errors** (a broken/unparseable file logs a warning + falls back so the daemon still boots); **byte-identical when no file exists**. Documented in `examples/aivyx-pa.toml`. Tests: 5 resolver (`team.rs`) + 2 config-parse (`tests.rs`); example-config e2e + clippy `-D warnings` green. |
| **RO.2** ✅ | **The writer + `SetTeamRoster` IPC** | DONE. `aivyx_channel::team_config_write::write_team_config` (validate → `to_toml` → `aivyx_config::write_toml_0600`, now pub; creates a missing parent; **validation first so an invalid roster never touches disk**). `QueryPayload::SetTeamRoster { roster }` + `QueryResponsePayload::TeamRosterApplied { roster, restart_required }` (aivyx-ipc, wasm-clean `TeamConfig`). Daemon handler resolves the pre-computed `team_config_write_path` (threaded parallel to `config_toml_path`; env-only launch → `no_config_file`), writes, re-reads → `TeamRosterApplied{restart_required:true}`, audits via `ConfigChanged` section `team`; invalid → `QueryError` `invalid_roster` (validator's message), write failure → `team_write_failed`. **NT-02 untouched.** Tests: 5 writer + IPC roundtrip; channel lib 1017 / e2e 30 / clippy `-D warnings` green. |
| **RO.3** ✅ | **The Studio Teams edit mode** | DONE (host + release-wasm build + clippy `-D warnings` clean; bundle/`dist` rebuild deferred to RO.5 since RO.4 also touches the web). New `TeamsState` (roster + notice + restart_required) replaces the bare roster signal, fanned by `ws_task` (`GetTeamRoster` + the new `TeamRosterApplied` arm + an `mc-teams`-prefixed `QueryError` route). `TeamsPanel` is now an editor over a local `draft` (seeded once from the loaded roster): team name/description, lead `<select>`, per-member name/role/trust(`<select>`)/scopes/tools(textareas → `parse_token_list`)/soul, **add specialist** (≤9 guard) / **remove** (lead protected), **Save** → `set_team_roster_query` (`mc-teams-set`), **Discard**, the restart-required banner + the NT-02 **inert-scope hint** (specialist scopes the lead lacks). Reuses the U/V write UX + existing CSS (no new tokens). |
| **RO.4** ✅ | **Genesis starter-team + CLI** | DONE. CLI: `aivyx-pa team init [--pack <default\|path.toml>] [--out <path>] [--force]` (offline, like `roster`) writes a starter team file via the RO.2 `write_team_config` (validate → `to_toml` → `0600`); default Nonagon or any pack TOML by path; refuses overwrite without `--force`. Web: a 4th onboarding step — **`OnboardingTeamStep`** (Profile → Persona → **Team** → Access) shows the active roster (`GetTeamRoster`) + routes to the RO.3 Teams editor (`View::Teams`). **OQ-4 resolved:** the team engine isn't wasm, so the browser can't construct a pack — pack presets are the CLI's `--pack`; the web confirms the default + routes to the editor for full customization. Tests: 2 CLI (`init_source`, `run_init` write/refuse/force) + smoke-verified end-to-end (`team init` → `team roster --config`); host + release-wasm + clippy `-D warnings` green. |
| **RO.5** ✅ | **Finalize** | DONE. `dx bundle --release` rebuilt + `dist/` reassembled (`.br` stripped) + committed — the new wasm carries the RO.3/RO.4 screen ("Save team", "Add specialist"/"Max 9 specialists", "Customize team", "inert until the lead", the onboarding "Team — who works for you") and the **release `aivyx-pa` binary embeds it** (wasm hash + `SetTeamRoster`/`TeamRosterApplied` symbols present). **Live write→load proven**: `aivyx-pa team init` writes a valid `[team]` file that `team roster --config` reads back (+ the RO.1 resolver / RO.2 writer unit tests). Full workspace suite + clippy `-D warnings` + `cargo deny` all green. Live browser auto-drive stays blocked in-sandbox (harness signal-16-reaps a TCP server; verified via bundle/embed + the data path per the [[chapter-lantern]] precedent). |

**Discipline:** RO.1 makes the roster a file the daemon already knows how to load
(`TeamConfig::load`) so RO.2/RO.3 write *one* definition; the load-or-default must
be **byte-identical when no `team.toml` exists** (regression: the Chapter Y roster
tests). Test band: **moderate** — the writer round-trip + handler validation/reject
+ the boot load-or-default + the web wiring; price **~25–40 new tests** (the edit
screen itself is verified by the served/bundle check, per R–Z).

## 5. Open questions (resolve in-phase)

- **OQ-1 — team-file home (RO.1).** A dedicated `team.toml` beside `aivyx-pa.toml`
  (locked default — matches packs + `TeamConfig::load`, keeps array-of-tables out
  of the hand-maintained config) vs. a `[team]` block inside `aivyx-pa.toml` (would
  force `toml_edit` array-of-tables surgery — rejected).
- **OQ-2 — no-widen scope: hint vs. block (RO.3).** Declaring a member scope the
  lead lacks is valid-but-inert. Lean **soft hint, non-blocking** (it's how packs
  already behave; attenuation is the real guard) vs. a hard reject (over-strict —
  would forbid forward-declaring a scope a future lead might hold).
- **OQ-3 — live vs. restart (RO.2).** Restart-required (locked — the team service
  is boot-assembled, same as Settings) vs. a live `team_missions` rebuild (a
  running-mission-safety question; deferred, additive).
- **OQ-4 — Genesis pack source (RO.4).** Offer the in-tree known packs (default
  Nonagon + `kitchen-boh`) vs. a fuller pack picker. Lean **known packs only** —
  the marketplace/registry is its own future chapter.

---

*Chapter Roster gives the operator the pen: the team the Studio shows is the team
the operator drew — lead, specialists, each one's soul and least-privilege reach —
written as the loadable file the engine already knew how to read, validated before
it lands, and never able to grant more than the lead holds. The read-only window of
Chapter Y becomes an editor; the deferred step of Chapter Genesis becomes real.*
