# Frontend & Brand — the Stitch design system (Chapter R)

> **Status:** design contract + foundation. This is the spec Chapter R builds
> from (mirrors `docs/ACCESS_LEVELS.md` / `docs/LOCAL_FIRST_RUN.md`).
>
> Aivyx PA has a mature visual identity — **"Stitch"** (*The Neon Cartographer*) —
> defined in the brand repo (`aivyx-brand/`: `brand-guidelines.md`,
> `design-tokens.md`, 23 Stitch mockups). But the shipped web app
> (`crates/aivyx-web`) was a minimal two-tab page with ad-hoc inline CSS. Chapter
> R **locks Stitch into the Agent's frontend**: real tokens, self-hosted fonts, a
> proper app-shell, a reusable component kit, and a reskin of the existing screens
> to match the mockups — while keeping the data flow and local-first guarantees
> untouched.

---

## 0. The brand in one paragraph

**Wick & Compass** (since 2026-09-12; it replaced the earlier Neon
Cartographer look this document first described). Precision and
instrument-grade trust: intelligence you read like a well-built gauge.
Layered surfaces on a deep-navy (dark) or vellum (light) base, **brass**
accents (`#c9a24b`) with a **rust** secondary (`#b5432b`), real 1px ruled
borders rather than ghost edges, and shadows that read as cast light.
Status colours (success / warning / error) stay separate from the brand
accents, so a plain label is neutral and colour always means something.
Type: **Fraunces** for headlines, **IBM Plex Sans** for body and titles,
**IBM Plex Mono** for labels and data. The mark is **Wick**, a flame in a
calibrated brass dial. Voice: warm, precise, direct; humour extremely dry.

The single source of truth for every value is `aivyx-brand/design-tokens.md`
and `aivyx-brand/brand-guidelines.md`; `crates/aivyx-web/assets/stitch.css`
transcribes them and never invents colours. (Sections below that name the
older palette or fonts describe the Chapter R structure, which still
holds; the values are Wick & Compass's.)

---

## 1. Surface = the Agent (Studio). What's in scope.

The Aivyx PA ecosystem has several surfaces. This contract — and Chapter R — cover
**only the Studio (the agent app)**, the Dioxus→WASM client served by the daemon
at `:7843`.

| Surface | What it is | In this chapter? |
|---|---|---|
| **Studio** | The agent app (chat, missions, dashboard, settings) | ✅ Yes |
| **Genesis** | First-run setup wizard (`aivyx-pa init`) | Roadmap (informs the look) |
| **Unlock** | Vault passphrase screen | Roadmap |
| **TUI** | The ratatui terminal interface | ❌ Later pass |
| **Creator** | Node-based visual flow / agent builder | ❌ Separate product |
| **Nexus** | The agent social network | ❌ Separate product |
| **Marketing** | The public website | ❌ Out of scope |

Delivery is **web-first**: the existing `aivyx-web` bundle, embedded into the
daemon and served on localhost. A Tauri/desktop shell can wrap the *same* app
later; it is not built here.

---

## 2. App-shell layout

The Studio is a classic command-center shell, driven by the layout tokens
(`--sidebar-width: 220px`, `--status-height: 36px`, optional `--tray-width: 300px`):

```
┌───────────────────────────────────────────────────────────┐
│ Topbar:  ▌AIVYX   title / search        ● daemon   ☼ theme │
├──────────┬────────────────────────────────────────────────┤
│ Sidebar  │                                                 │
│ (220px)  │   Main view                                     │
│ logomark │   (Missions · Chat · …)                         │
│  ▸ nav   │                                                 │
│  items   │                          [optional context tray]│
├──────────┴────────────────────────────────────────────────┤
│ StatusBar (36px):  mono daemon/agent status · model · …    │
└───────────────────────────────────────────────────────────┘
```

- **Sidebar** — logomark + nav. Items reflect *real* daemon capabilities; roadmap
  items render **disabled ("soon")**, never as half-built panes.
- **Topbar** — brand/title, connection status dot (`beacon` pulse when live),
  light/dark toggle.
- **StatusBar** — `label-tech` mono line: daemon connection, agent, model.

---

## 3. Studio screen inventory (mapped to daemon capabilities)

| Group | Nav item | Maps to | State |
|---|---|---|---|
| — | **Command** | dashboard: stat cards + active missions + live audit-trail feed + agent status | ✅ Live (Ch. S — the default landing view) |
| Workspace | **Chat** | single-agent turn loop + streamed events + gate | ✅ Live, reskinned |
| Workspace | **Missions** | `team.run` goal→plan→gated execution (Nonagon, Ch. L) | ✅ Live, reskinned |
| Workspace | **Mission Control** | one active mission's live LEAD/specialist graph, click-to-drill-in (current step, capability scopes, NT-02 hint), and abort/pause/resume controls | ✅ Live (Ch. Mission Control) |
| Workspace | **Schedules** | cron routines: config/operator/agent-created schedules, with create/toggle/delete + the agent-proposal approval flow | ✅ Live (Ch. Chime) |
| Knowledge | **Memory** | self-learning memory browser: topics + entries + search (T) **+ knowledge graph** — see §13 | ✅ Live (Ch. T + MG) |
| Knowledge | **Wiki** | knowledge-wiki browser: synthesized per-topic pages (LLM summary + co-occurrence backlinks + source-entry count) over read-only IPC | ✅ Live (Ch. Codex) |
| Knowledge | **Graph** | typed knowledge-graph view: entity nodes + directed, predicate-labeled relation edges (force-laid-out) over read-only IPC; distinct from the Memory co-occurrence graph | ✅ Live (Ch. Lattice) |
| Agent | **Create**\* | the guided agent-creation flow (Profile → Persona seed → access); first-run lands here when the Profile isn't yet declared | ✅ Live (Ch. Genesis) |
| Agent | **Agents** | persona / soul / profile editor: direct Profile write + persona-governance loop (proposals + revert) — see §9 | ✅ Live (Ch. V) |
| Agent | **Skills** | the skill library: every skill (operator-taught / agent-authored / agent-refined) with its WH.2 effectiveness, provenance, `domain`, version, lineage + the procedure on demand; pending proposals link to Agents; read-only `GetSkills` IPC | ✅ Live (Ch. Repertoire) |
| Agent | **Teams** | the Nonagon roster: team header + member cards (role / trust / scopes / tools / soul) — see §10 | ✅ Live (Ch. Y) |
| System | **Documents** | file browser + **editor** over the agent workspace + the access-scoped fs_root — see §11, §14 | ✅ Live (Ch. Z + DW) |
| System | **Audit** | the dedicated, paginated Audit screen — reuses the Command Center's `AuditFeed` row-renderer rather than a second copy of the same markup | ✅ Live (`/classic` retirement) |
| System | **Sessions** | every active daemon session (channel, trust tier, created/last-active), replacing `/classic`'s own sessions pane | ✅ Live (`/classic` retirement) |
| System | **Gallery** | recent images generated via the configured `comfyui` `[[mcp_server]]`, read from ComfyUI's own `/history` API | ✅ Live (Studio Gallery) |
| System | **Notifications** | configured notify targets (read-only) + dispatch history, for missions/schedules that notify outside the Studio | ✅ Live (Ch. Herald) |
| System | **Loop** | the daemon's autonomous loop control: start/stop/status/log/skip, backlog add/list — full read+write IPC (`QueryPayload::Loop*`) | ✅ Live |
| System | **Reminders** | pending reminders list, soonest-first — read-only `GetReminders` IPC, shared with the TUI Dashboard | ✅ Live |
| System | **MCP** | each configured MCP server's last-start health (connected + tool count, or failed + reason) | ✅ Live (Ch. Lantern) |
| System | **Tools** | a read-only, searchable catalog of every registered tool (name, capability base, minimum trust tier, description) | ✅ Live (Ch. Almanac) |
| System | **Voice** | `[voice]` config editor + readiness check + launch command (audio runs host-side) — see §12 | ✅ Live (Ch. Voice) |
| System | **Settings** | the first config **write** surface: access level + autonomy level (both confirm-first) + budgets editable; provider/model read-only — see §8 | ✅ Live (Ch. U, + Reins) |
| System | **Guide** | the in-app end-user guide — the `docs/guide/*.md` pages rendered in the Studio (see `guide.rs`); pure static content, no daemon IPC | ✅ Live |

\* **Create** only appears pre-genesis — before an agent Profile is
declared. Once a Profile exists, the entry is hidden (`Sidebar`'s
`genesis_done` conditional in `main.rs`); editing then lives in
Agents/Settings instead.

The reference mockups for the locked look: `aivyx-brand/assets/stitch/`
`aivyx_command_center`, `aivyx_missions_orchestration`, `the_terminal`.

---

## 4. The mechanisms (how Stitch lands in the bundle)

1. **Token layer.** `crates/aivyx-web/assets/stitch.css` — the full token set as
   `:root` custom properties (dark default) + `[data-theme="light"]` overrides,
   transcribed verbatim from `design-tokens.md`, plus base styles: the `bg-depth`
   gradient canvas, `.label-tech`, glass panel/card/header, ghost separators, and
   the animation keyframes — all gated behind `prefers-reduced-motion`.
2. **Self-hosted fonts.** `assets/fonts/*.woff2` (Space Grotesk, Inter, JetBrains
   Mono — all OFL) with local `@font-face`. **No Google-Fonts CDN** (the mockups'
   CDN `<link>`s are prototype-only and break offline use).
3. **Brand icons + logos.** The `aivyx-brand/icons/` stroke SVG set + `icons.css`,
   and the logomark/wordmark/favicon, bundled as assets. **No Material Symbols
   CDN.**
4. **Wiring.** Dioxus `asset!()` + `document::Stylesheet`/`Link` in the rsx head;
   the inline `STYLE` const is deleted. `just build-web` (`dx bundle`) emits these
   into `dist/`, `aivyx-channel/build.rs` embeds them, `web_ui.rs` serves them —
   so everything is offline and local-first.

---

## 5. Invariants

- **Local-first / offline.** Fonts, icons, CSS are all self-hosted in the bundle;
  the served app makes **zero external requests**.
- **Stitch is the single source of truth.** Every color/space/shadow comes from
  `design-tokens.md`; no ad-hoc values. The inline `STYLE` const is gone.
- **Presentation-only.** The reskin never touches the `aivyx_ipc` data flow, the
  WebSocket task, or daemon behavior. `/classic` stays intact.
- **Studio only.** Creator, Nexus, marketing, the TUI reskin, and any
  Tauri/desktop shell are out of scope; roadmap screens are disabled nav, not
  stubs.
- **No hard lines.** Section boundaries use surface-tier shifts or ghost borders,
  never 100%-opaque 1px rules.

---

## 6. Phase plan

| Phase | Deliverable |
|---|---|
| **R.0** | This design contract. |
| **R.1** | Stitch asset layer — `stitch.css` tokens, self-hosted fonts, brand icons/logos, wired via `asset!()`. |
| **R.2** | App-shell — Sidebar + Topbar + StatusBar from the layout tokens. |
| **R.3** | Component kit — Button / Card / Input / Chip / LabelTech / StatCard / StatusDot. |
| **R.4** | Reskin Missions (orchestration look) + Chat (terminal look). |
| **R.5** | Build the bundle, live-verify at `:7843`, docs/screens, memory. |

**Status: R.0–R.5 complete and verified served.** The Studio app renders the
Stitch shell (Sidebar + Topbar + StatusBar), the reskinned Missions + Chat
views, self-hosted fonts and brand icons — all from the daemon's embedded
bundle with **zero external requests** (verified: the WASM requests the hashed
asset paths, the daemon serves each `200`, the served CSS carries the full token
set in both themes). The built bundle is committed at `crates/aivyx-web/dist/`
so a plain `cargo build` and the release embed it without a wasm toolchain;
**regenerate it with `just build-web` after any frontend change** (needs a
prebuilt `dx` 0.6.x binary — building dioxus-cli from source currently fails on
a pinned `swc`/`serde::__private` conflict). A future `dx bundle` step in the
release CI would remove the need to commit the artifact.

---

## 7. Out of scope (the follow-ons)

The **Memory graph** *visualization*, **Teams/Agents** screens, the
**Genesis wizard** + **Unlock** screens, the **TUI** Stitch reskin, and a
**Tauri/desktop** shell are all future work — they build on this foundation.
(The **Command-Center dashboard** landed in Chapter S; the **Memory browser** —
topics/entries/search — in Chapter T; the **Settings** write surface is Chapter
U, scoped in §8.) **Creator** and **Nexus** are separate ecosystem products with
their own contracts.

---

## 8. Settings — the first config write surface (Chapter U)

Chapters R/S/T were **read-only** and added **no daemon API** — they painted
existing IPC. **Settings is the first screen that writes.** It lets the operator
read and change a deliberate, safe subset of `aivyx-pa.toml` from the Studio. This
section is the contract Chapter U builds from; it intentionally breaks the
"no new daemon API / read-only" invariant (that is the point of the chapter)
while holding every safety invariant Aivyx PA already guarantees.

### 8.1 Three hard facts that shape the screen

1. **The daemon does not hot-reload config.** Access level, budgets, provider,
   model — all are parsed **once at launch** (access level is load-time;
   `aivyx-pa access`: "takes effect on the next daemon start"). A write from the web
   UI therefore **cannot apply live**. The screen is honest about this: every
   successful write returns `restart_required` and the UI shows a persistent
   banner — *"Saved to aivyx-pa.toml — restart the daemon to apply:
   `aivyx-pa daemon stop && aivyx-pa daemon run`."* No self-restart (too invasive).
2. **The daemon doesn't retain the config-file path.** `DaemonConfig` holds
   parsed sub-structs, not the path to `aivyx-pa.toml`. The write path adds a
   `config_toml_path` to `DaemonConfig`, threaded from `aivyx-pa daemon run`, so the
   daemon can both **re-read** the on-disk values (to populate the form) and
   **rewrite** the right file.
3. **The write logic already exists** in `aivyx-pa access set` (a `toml_edit`
   section-rewrite that preserves every other section, sets `0600`, drops a stale
   `[access] root`, and confirms expanded levels). Chapter U **factors it into a
   shared `aivyx-config` helper** so the CLI and the daemon write config
   identically — never two divergent writers.

### 8.2 Editable scope (v1)

| Section | v1 | Why |
|---|---|---|
| **Access level + root** | ✅ Editable, **confirm-first** on expansion | The flagship; security-sensitive (Ch. N). Mirrors `aivyx-pa access set`. |
| **Autonomy level** | ✅ Editable, **confirm-first** on `autonomous`/`unleashed` | The autonomy dial (Ch. Reins). Level picker; mirrors `aivyx-pa autonomy set`. Per-domain overrides + allowlist stay CLI/hand-edit. |
| **Budgets** (`per_run_usd`, `per_day_usd`, `on_exceeded`, `alert_at`) | ✅ Editable | Low-risk numeric caps (Ch. K). |
| **Provider / model / num_ctx** | 👁 **Read-only** + "change via `aivyx-pa init`" | Editing risks a daemon that won't start (bad model) and touches API keys in the encrypted store/env — out of v1. |
| **Profile** | ❌ A future Agents/Persona screen | Already served read-only by `GetProfile`; editing is its own surface. |

### 8.3 New IPC (request/response — fits the existing query pattern)

- **`GetSettings`** → `SettingsSnapshot { access_level, fs_root,
  confirm_destructive, provider, model, num_ctx, budget{…}, embeddings_available,
  cycle_detection, autonomy_level }` — populates the form (none of this is
  queryable today).
- **`SetAccessLevel { level, root, confirm }`** — the daemon enforces
  `is_expanded() ⇒ confirm == true` **server-side** (the confirm-first gate is not
  just a UI nicety), applies the same root rules as the CLI.
- **`SetAutonomyLevel { level, confirm }`** (Ch. Reins) — same shape; the daemon
  enforces `autonomous`/`unleashed` ⇒ `confirm == true` **server-side**, writes
  only `[autonomy] level` (overrides + allowlist preserved).
- **`SetBudget { per_run_usd, per_day_usd, on_exceeded, alert_at }`**.
- Both writes: rewrite the toml section via the §8.1(3) helper → append a new
  **`ConfigChanged`** audit event → respond with the fresh snapshot +
  `restart_required: true`.

### 8.4 Invariants

- **Localhost trust boundary.** The Studio is served on localhost only — the same
  boundary the CLI already writes config from — so this adds no new attack
  surface. It is *not* a remote admin panel.
- **Confirm-first survives.** Chapter N's confirm-before-expanding-access posture
  is preserved as a **UI confirm modal _and_ a server-side `confirm` flag** the
  daemon refuses to bypass.
- **Every change is audited.** Writes append a signed `ConfigChanged` entry to the
  HMAC audit chain — the same chain the Command Center's feed renders.
- **Writes preserve the file.** Section-scoped `toml_edit` rewrites at `0600`;
  comments and unrelated sections (secrets, providers, triggers) are untouched.
- **Studio only / local-first.** Same scope boundary and Stitch/offline rules as
  R–T; `/classic` stays intact.

### 8.5 Phase plan

| Phase | Deliverable |
|---|---|
| **U.0** | This contract (§8). |
| **U.1** | Shared `aivyx-config` write helpers + `config_toml_path` on `DaemonConfig` (threaded from `aivyx-pa daemon run`); CLI refactored onto the helper (no behavior change). |
| **U.2** | `aivyx-ipc`: `GetSettings` + `SetAccessLevel` + `SetBudget` + `SettingsSnapshot`, with wire-compat round-trip tests. |
| **U.3** | Daemon handlers: snapshot read; validate → rewrite → **`ConfigChanged`** audit → respond with `restart_required`. (Adds an `AuditEvent` variant → updates the e2e event-count assertions; full suite.) |
| **U.4** | Web UI: `View::Settings` live — access selector + confirm modal, budget inputs, read-only provider/model card, restart banner; `ws_task` arms; `stitch.css`. |
| **U.5** | Build the bundle, live-verify in a real browser (read settings, set a budget, change access via the modal, see the restart banner, confirm the toml is rewritten with other sections preserved + an audit entry), docs + memory, push. **Done:** wasm serves byte-identical/untruncated; IPC probe proved `GetSettings`→`SetBudget`→`SetAccessLevel` (confirm-first refusal then apply), toml rewritten preserving all sections, two `ConfigChanged` audit entries, chain intact. |

---

## 9. Agents — the identity editor (Chapter V)

Settings (Ch. U) opened the first config-write surface. **Agents** is the
second, and the one that touches the product's core identity: the operator's
**Profile** and the agent's self-learned **Persona**. It is deliberately *two
different write models stitched into one screen*, because the two halves are
governed differently and that difference is a feature, not an accident.

### 9.1 The three layers (what is editable, and how)

| Layer | What it is | Edit model | Liveness |
|---|---|---|---|
| **Profile** | operator-**declared** identity — the `[profile]` table in `aivyx-pa.toml` (`assistant_name`, `operator_profile`, `communication_style`, `primary_use_cases[]`, `behavioral_preferences[]`, `behavioral_constraints[]`) | **direct write** — surgical `toml_edit` rewrite of `[profile]`, exactly the Ch. U pattern (`aivyx-pa profile edit` is the CLI twin) | **load-time** → `restart_required` (same as Settings; Profile shapes `assemble_session_prompt` at startup) |
| **Persona** | the agent's **self-learned** adaptations — an append-only, HMAC-signed **delta chain** (behavioral prefs, learned context, communication adaptations, character traits, relationship milestones) | **never free-edited.** The operator *governs* it: resolve agent **proposals** (approve / approve-with-edit / reject) and **revert** deltas. The chain's integrity is the point. | **live** — the daemon recomputes shared runtime state on resolve/revert, so the next turn picks it up (no restart) |
| **Soul / Identity** | the combined Profile+Persona **export bundle** (`aivyx-pa identity export`) | portability, not editing | n/a — **deferred** (a later read-only export button) |

The screen never lets the operator hand-write persona deltas. That asymmetry —
**you declare your Profile; the agent proposes its Persona and you gate it** — is
the self-learning contract (PRODUCT.md P13/P14) made visible.

### 9.2 What already exists (reuse, do not rebuild)

Persona is a mature, gated subsystem; **almost all of its write IPC already
ships** and is daemon-tested (the `aivyx-pa persona` CLI + `/classic` use it):

- **Read:** `GetProfile` → `ProfileSummary`; `GetEffectivePersona` →
  `EffectivePersonaSummary`; `ListPersonaDeltas` → `[PersonaDeltaSummary]`;
  `ListPersonaProposals` / `GetPersonaProposal` → `[PersonaProposalSummary]`.
- **Write (existing `FrontendMessage`):** `ResolvePersonaProposal { proposal_id,
  resolution: Approve | ApproveWithEdit { edited_op } | Reject { reason } }` →
  `PersonaProposalResolved`; `RevertPersonaDelta { target_delta_id }` →
  `PersonaRevertResolved`. Both recompute runtime state (live).

So the **only new daemon API this chapter adds is the Profile direct-write
path** — `SetProfile` + a `write_profile_section` helper in `config_write.rs`
(joining `write_access_section` / `write_budget_section` from U.1). Everything
persona is wiring existing IPC into the Studio, the way Chat wired the existing
turn loop.

### 9.3 New IPC (the Profile half only)

- **`SetProfile`** `{ assistant_name?, operator_profile?, communication_style?,
  primary_use_cases?: Vec<String>, behavioral_preferences?: Vec<String>,
  behavioral_constraints?: Vec<String> }` → `ProfileApplied { profile:
  ProfileSummary, restart_required: true }`. Absent scalars clear the key;
  absent lists leave them untouched vs. an explicit `[]` that clears — TBD in
  V.2, matched to `write_budget_section`'s clear-on-None convention.
- Reuses `config_toml_path` (added in U.1) and `ConfigWriteError` → stable
  `QueryError` codes (`map_config_write_error`).
- Writes append an `AuditEvent::ConfigChanged { section: "profile", summary }`
  (the U.3 variant — no new audit variant, no new count-assertion churn).
- **Active vs on-disk (post-V audit).** The daemon holds the Profile as an
  immutable boot-time `Arc<Profile>` — it is **load-time**, so the running agent
  keeps using the boot values until a restart (the Persona, by contrast, is a
  live `Arc<RwLock>` and updates next-turn). `GetProfile` therefore carries a
  `from_disk` flag: the editor seeds with `from_disk = true` (the on-disk
  `[profile]`, i.e. *what it writes*) so a save-before-restart then reload shows
  the pending values and never clobbers them; the Command-Center name chip uses
  `from_disk = false` (the running snapshot). The flag is `#[serde(default)]`
  (false), so the running-state meaning is wire-unchanged for older clients.

### 9.4 Invariants

- **Profile writes preserve the file.** Section-scoped `toml_edit` at `0600`;
  `[agent]`/`[fs]`/`[access]`/`[budget]`/secrets untouched — same guarantee as U.
- **Persona integrity is never bypassed.** The web UI cannot append a raw delta;
  it can only resolve a proposal or revert via the existing signed-chain paths.
  Approve-with-edit carries an `edited_op` the daemon re-validates and re-signs.
- **Honest liveness.** Profile edits show the restart banner; persona
  resolve/revert show "applied — effective next turn" (no banner).
- **Every change is audited.** Profile → `ConfigChanged`; persona → the existing
  `PersonaProposalResolved` / `PersonaRevertResolved` audit entries.
- **Studio only / local-first / Stitch.** Same scope + offline + token rules as
  R–U; `/classic` intact.

### 9.5 Phase plan

| Phase | Deliverable |
|---|---|
| **V.0** | This contract (§9). |
| **V.1** | `write_profile_section` in `aivyx-config/config_write.rs` (validate + `[profile]` toml_edit rewrite, 0600); unit tests alongside the U.1 helpers. |
| **V.2** | `aivyx-ipc`: `SetProfile` + `ProfileApplied` (round-trip tests); daemon handler (validate → rewrite → `ConfigChanged` audit → `restart_required`). Confirm the existing persona read+resolve+revert handlers cover what the web screen needs. |
| **V.3** | Web UI part 1 — `View::Agents` + the **Profile editor** (form over the six `[profile]` fields, list add/remove, save → confirm → `ProfileApplied`, restart banner); `ws_task` arms; `stitch.css`. |
| **V.4** | Web UI part 2 — the **Persona governance** panel: Effective Persona viewer + pending **proposals** (approve / edit / reject) + **delta chain** with revert, over the existing IPC. "Effective next turn" notices. |
| **V.5** | Build the bundle, live-verify in a real browser (edit Profile → restart banner + toml rewritten + audit; resolve a seeded proposal → persona updates live; revert a delta), docs + memory, push. **Done:** wasm serves byte-identical/untruncated; IPC probe proved `GetProfile`→`SetProfile`→`ProfileApplied` (six fields, `restart_required`), `[profile]` rewritten **in-place** preserving all sections, a shape-only `ConfigChanged` audit entry, the persona read path (effective/proposals/deltas) returns well-formed empty states on a fresh daemon, and the resolve/revert write path is wired (typed `ok:false` on bogus ids). Seeding a real proposal needs LLM reflection turns, so the resolve/revert *UI* actions are verified against the live error path rather than an approved delta. |

---

## 10. Teams — the Nonagon roster (Chapter Y)

The Studio's window into **who the agent's team is**: the daemon's active
`TeamConfig` (the 9-role Nonagon today; a vertical pack's roster later) — the
lead + specialists, each with their role, trust ceiling, capability scopes,
tool allowlist, and soul (system prompt). The live team *missions* already live
on the Command Center + Missions screens; **Teams is the composition view**, the
read-only counterpart to `aivyx-pa team roster`.

### 10.1 What's reused (almost everything)

- **`TeamConfig` / `TeamMember`** already live in the **wasm-clean**
  `aivyx-team-types` crate and `aivyx-ipc` already depends on it (it carries
  `Option<TeamConfig>` on `TeamRun`/`TeamRunGoal`). So the web renders the **real
  type** — no mirror struct, no new daemon data. The daemon already holds the
  active roster in `TeamMissionService` (`default_nonagon()`).
- The render mirrors the CLI `team::render_roster`: lead vs specialist, the
  `TrustTier` label, scopes-or-`(none)`.

### 10.2 The one new IPC

- **`GetTeamRoster`** → `QueryResponsePayload::GetTeamRoster { roster: TeamConfig }`.
  Read-only. The daemon answers from `TeamMissionService`'s config via a new
  `team_config()` accessor; `None` service ⇒ a `QueryError` (`no_team`).

### 10.3 Web

`View::Teams` (the sidebar item leaves the roadmap). `TeamsPanel`: a header
(team name + description + lead + specialist count + a small "N active team
missions" stat reusing the already-polled `missions` signal), then a roster grid
of **member cards** — lead badged, role, `TrustTier` chip, scopes, tool count —
each expandable to show the full tool allowlist + the member's soul. Empty/loading
states for a daemon without a team service.

### 10.4 Invariants

- **Read-only / no behavior change** — Teams only *renders* the active roster;
  swapping teams (vertical packs) + per-member editing are later, separate work.
  The daemon's `default_nonagon()` is unchanged.
- **Real shared type** — `TeamConfig` over the wire, not a re-mirrored struct.
- **Studio only / local-first / Stitch** — same rules as R–X.

### 10.5 Phase plan

| Phase | Deliverable |
|---|---|
| **Y.0** | This contract. |
| **Y.1** | `GetTeamRoster` IPC + `TeamMissionService::team_config()` accessor + daemon handler; round-trip + handler tests. |
| **Y.2** | Web: `View::Teams` + `TeamsPanel` (header + member cards + expand-to-soul); `ws_task` arm; `stitch.css`. |
| **Y.3** | Finalize: bundle, live-verify (roster renders, member detail, offline), docs, memory, push. |

**Status: Y.0–Y.3 COMPLETE + live-verified.** `GetTeamRoster` returns the
daemon's active `TeamConfig`; the Studio's Teams screen renders the **9-member
Nonagon** (lead `coordinator` + 8 specialists), each with role, trust tier, tool
count, and an expand-to-soul. Live run: served wasm byte-identical/untruncated;
the IPC probe returned all 9 members with their souls. No mirror type — the web
renders the real `aivyx_team_types::TeamConfig`. Deferred: vertical-pack swapping
+ per-member editing.

---

## 11. Documents — the file browser (Chapter Z)

A **read-only** browser over the two document-shaped places Aivyx PA already knows:
the agent's always-on **workspace** (`~/.aivyx-pa/workspace`, Chapter O — its own
thoughts/plans/projects) and the operator's **`fs_root`** (the access-scoped
shared work, Chapter N). The Studio counterpart to `fs.read` / `fs.metadata` /
`workspace.read` — but for a human, not the agent.

### 11.1 Security model (reused, not reinvented)

Browsing the filesystem over IPC is the most safety-sensitive screen yet, so it
**reuses the exact guard the fs/workspace tools use**: every request is
lexically resolved against a pre-canonicalized root, then `std::fs::canonicalize`
resolves all symlinks, then the result must still `starts_with(root)` — `..` and
symlink escapes are rejected. Two roots only:

- **`workspace`** — the agent's workspace root (always-on; `None`/typed error
  when the workspace is disabled).
- **`fs`** — the operator's `fs_root`, i.e. **the access level's reach**. At
  `sandbox` that's `~/aivyx-pa-sandbox`; at `home`/`full` it's broader **because the
  operator granted it** (via Settings, confirm-first). Documents never reaches
  past `fs_root` — expanding it is the same operator-controlled lever the agent
  already obeys.

Plus: **read-only** (list + read; no write/delete/rename from the web), file
reads are **size-capped** (large/binary files return metadata, not bytes), and
the screen is **localhost-only** like the rest of the Studio.

### 11.2 New IPC

- **`ListDir { root, path }`** → `{ entries: Vec<DocEntry>, path }` —
  `DocEntry { name, kind: dir|file|symlink|other, size_bytes }`, sorted dirs-first
  then name.
- **`ReadFile { root, path }`** → `{ file: DocFile }` —
  `DocFile { path, size_bytes, content: Option<String>, truncated, binary }`
  (`content = None` when binary or over the cap).
- `root` is `"workspace" | "fs"`; an unknown root / disabled workspace / escape
  attempt → `QueryError` (`bad_root` / `no_workspace` / `path_escape`).

### 11.3 The browse primitive

`aivyx-channel::document_browse` — `list_dir(root, rel)` + `read_file(root, rel,
cap)` returning the wire types, reusing `aivyx_core::tools::fs::lexical_resolve`
(promoted to `pub`) + the canonicalize-`starts_with` check. The daemon threads
`DocumentRoots { fs_root, workspace_root: Option }` (both canonicalized) through
`DaemonConfig → ConnectionContext → handle_query` (the Chapter-U pattern).

### 11.4 Web

`View::Documents` (the sidebar item leaves the roadmap). `DocumentsPanel`: a root
switcher (Workspace / Files), a **breadcrumb** path, a directory listing (folders
first, click to descend, `..` to ascend), and a **file viewer** pane (text in a
mono `<pre>`; binary/oversize → a "N KB — not shown" note). Empty/loading states.

### 11.5 Invariants

- **Read-only** — Documents never mutates the filesystem.
- **Never escapes a root** — canonicalize-`starts_with`, the same guard the tools
  use; the two roots are the only reach.
- **Reach == access level** — `fs` browsing is exactly `fs_root`; no new grant,
  no bypass of the Chapter-N model.
- **Studio only / local-first** — same scope + offline rules as R–Y.

### 11.6 Phase plan

| Phase | Deliverable |
|---|---|
| **Z.0** | This contract. |
| **Z.1** | `DocEntry`/`DocFile` wire types (`aivyx-ipc`) + `document_browse` primitive (`aivyx-channel`, reusing the fs guard); unit tests incl. escape/binary/cap. |
| **Z.2** | `ListDir`/`ReadFile` IPC + thread `DocumentRoots` into the daemon + handlers (resolve root → primitive → typed errors); round-trip + handler tests. |
| **Z.3** | Web: `View::Documents` + `DocumentsPanel` (root switcher + breadcrumb + listing + file viewer); `ws_task` arms; `stitch.css`. |
| **Z.4** | Finalize: bundle, live-verify (browse workspace + fs_root, read a file, escape blocked, offline), docs, memory, push. |

**Status: Z.0–Z.4 COMPLETE + live-verified.** `ListDir`/`ReadFile` browse the
two roots; the Studio's Documents screen renders the listing + a file viewer.
Live run: served wasm byte-identical/untruncated; `ListDir fs` returned the
seeded sandbox (dirs first), descended into a subdir, `ReadFile` returned text
for `plan.md` and **withheld** the binary `data.bin` (`binary: true`), `ListDir
workspace` returned the agent's own dir, and **`../../../etc` was rejected**
(`path_escape`) — the canonicalize-`starts_with` guard holds over IPC. Deferred:
write/rename, a tree pane, syntax highlighting.

---

## 12. Voice — the host voice channel, configured (Chapter Voice)

The final roadmap screen — and a deliberately **honest** one. Aivyx PA's voice is a
**host-local CLI loop**: `aivyx-pa --channel voice` runs an in-process
mic → Whisper ASR → agent turn → Kokoro TTS → speakers loop on the operator's
machine (`cpal`/`rodio`), configured by a `[voice]` TOML section. *"The audio
loop never leaves the host."* The daemon doesn't run it and the browser can't
reach the host microphone — so the Studio's Voice screen is **not** a live voice
loop. It is the **configuration + readiness + launch** surface: edit `[voice]`,
see whether the model files are actually present, and copy the command to start
voice. (Browser-native voice — streaming the mic to the daemon's Whisper — is a
larger, separate effort, deliberately out of scope.)

### 12.1 What it edits (the `[voice]` section)

`aivyx_config::VoiceOptions` is the direct `[voice]` parse target — all keys
optional: `asr_engine` (`whisper-rs`), `tts_engine` (`kokoro`), `asr_model_path`
(the Whisper `.bin`), `asr_language`, `asr_beam_size`, `tts_model_dir` (the
Kokoro model directory — holds the `.onnx` + `voices-*.bin`), `tts_voice_name`
(e.g. `af_heart`), `tts_speed`, `input_device` / `output_device` (cpal/rodio
overrides). The Whisper model + the Kokoro model dir are what make-or-break a
launch. *(Chapter Timbre replaced the GPL Piper engine — and its `voice_path` +
`espeak_data_path` keys — with the permissive Kokoro stack.)*

### 12.2 Readiness (computed daemon-side)

The screen's value beyond an editor: a **readiness check**. The daemon checks
the filesystem prerequisites — `asr_model_path` (a file), plus a `*.onnx` and a
`voices-*.bin` **inside** `tts_model_dir` — and reports each
`present | missing | unset`, so the operator sees *"✓ Whisper model, ✗ Kokoro
voices (.bin) (none in the model dir)"* before they ever run the command.
Read-only inspection; the daemon never loads the audio stack.

### 12.3 New IPC (mirrors Settings, Chapter U)

- **`GetVoiceSettings`** → `VoiceSettingsSnapshot` (the nine `[voice]` fields +
  the three readiness flags). Re-reads `aivyx-pa.toml` from disk (the U on-disk
  convention), so the editor edits what it shows.
- **`SetVoice { …nine fields… }`** → `VoiceApplied { settings, restart_required:
  true }`. Section-scoped `toml_edit` rewrite of `[voice]` via a new
  `write_voice_section` helper (joining access/budget/profile in
  `config_write.rs`); `ConfigChanged { section: "voice" }` audit; load-time, so
  the running voice process (if any) must be restarted to apply.

### 12.4 Web

`View::Voice` + `VoicePanel`: the `[voice]` form (engine selects, path inputs,
language, beam, devices), a **readiness panel** (a chip per prerequisite), the
**launch command** (`aivyx-pa --channel voice`, copy-able) with a one-line note
that audio runs on the host, and the restart banner after a write. No audio APIs
touched.

### 12.5 Invariants

- **Honest about the architecture** — the screen configures + checks; it never
  pretends the browser does audio. The voice loop stays a host process.
- **Read-only inspection / single write surface** — `GetVoiceSettings` only
  stats paths; `SetVoice` only rewrites the `[voice]` section (preserving the
  rest), audited, load-time. Same guarantees as Settings (U).
- **Studio only / local-first / Stitch** — same rules as R–Z.

### 12.6 Phase plan

| Phase | Deliverable |
|---|---|
| **Voice.0** | This contract. |
| **Voice.1** | `write_voice_section` + a `VoiceWrite` carrier in `aivyx-config/config_write.rs` (clear-on-None section rewrite); unit tests. |
| **Voice.2** | `GetVoiceSettings`/`SetVoice` IPC + `VoiceSettingsSnapshot` (fields + readiness) + daemon handlers (re-read + stat readiness; write + `ConfigChanged` audit; reuse `config_toml_path`); round-trip + handler tests. |
| **Voice.3** | Web: `View::Voice` + `VoicePanel` (form + readiness chips + launch command + restart banner); `ws_task` arms; `stitch.css`. |
| **Voice.4** | Finalize: bundle, live-verify (read/write `[voice]`, readiness reflects a missing vs present model file), docs, memory, push. |

---

## 13. Memory — the knowledge graph (Chapter MG)

Chapter T shipped the Memory **browser** (topic rail + entry cards + search) and
deliberately deferred the **graph** — the `aivyx_neural_memory_graph` mockup's
node-and-edge view of how topics relate. This chapter adds it, and it is a
**real** graph, not hub-and-spoke: the daemon already learns *which topics get
recalled together in turns that go well* (the Phase-83 co-occurrence ledger,
`PersistentCooccurrenceLedger`), giving genuine weighted edges.

### 13.1 The data (already there)

- **Nodes** — memory topics (`Memory::list_topics`), each sized by its entry
  count.
- **Edges** — `top_affinities` from the co-occurrence ledger →
  `CooccurrencePatterns { top_pairs: Vec<PairScore> }`, where
  `PairScore { a, b, score, samples }` is a decayed joint-helpfulness weight
  between two topics. **Already wasm-clean** in `aivyx-ipc/ledgers.rs` — reused
  as-is for the edges, no mirror type.

Both `Memory` and the co-occurrence ledger are already reachable in
`handle_query` (the ledger is `Option` — absent when co-occurrence isn't armed,
in which case the graph degrades to nodes only). **No new daemon threading.**

### 13.2 The one new IPC

- **`GetMemoryGraph { limit }`** → `{ nodes: Vec<MemoryGraphNode>, edges:
  Vec<PairScore> }`. `MemoryGraphNode { topic, entry_count }`. Read-only; the
  daemon lists topics (counting entries, capped), and folds the top-`limit`
  affinity pairs. Empty edges ⇒ a topic cloud (still useful).

### 13.3 Web

The Memory screen gains a **List ⇄ Graph** toggle. The graph view renders an SVG
force-directed layout computed in-WASM (a small deterministic Fruchterman–
Reingold sim run for a fixed number of iterations in a `use_memo`, seeded from a
stable topic hash so it doesn't jitter on re-render): **nodes** as circles sized
by `entry_count`, **edges** as lines whose width/opacity scale with `score`.
Clicking a node selects that topic (reusing T's `GetMemoryTopicEntries` — drops
back to the list, scoped to the topic). Empty-memory and ledger-absent states
render cleanly.

### 13.4 Invariants

- **Read-only / real edges** — the graph only *renders* existing memory + the
  co-occurrence ledger; it never mutates memory and synthesizes no fake edges
  (no co-occurrence data ⇒ an honest topic cloud).
- **Deterministic layout** — the force sim is seeded + fixed-iteration, so the
  graph is stable across re-renders (no continuous animation loop).
- **Studio only / local-first / Stitch** — same rules as R–Voice.

### 13.5 Phase plan

| Phase | Deliverable |
|---|---|
| **MG.0** | This contract. |
| **MG.1** | `GetMemoryGraph` IPC + `MemoryGraphNode` (reusing `PairScore` for edges) + daemon handler (topics + counts + `top_affinities`); round-trip + handler tests. |
| **MG.2** | Web: a List⇄Graph toggle in the Memory screen + the in-WASM force layout + SVG render + node-click → topic select; `ws_task` arm; `stitch.css`. |
| **MG.3** | Finalize: bundle, live-verify (graph renders over seeded memory + a co-occurrence pair; node click filters; ledger-absent = topic cloud), docs, memory, push. |

---

## 14. Documents — editable (Chapter DW)

Chapter Z shipped the read-only browser. This chapter makes it **editable** —
edit a file's text and save, create files/folders, rename, delete — deliberately
crossing Z's read-only invariant. It is the Studio's second filesystem write
surface (after the access scope itself), and the most safety-sensitive, so the
guards are explicit.

### 14.1 Safety model (the rules)

- **Never escapes a root.** Writes reuse the read guard — but a *new* path can't
  be canonicalized, so the write primitive canonicalizes the **parent** dir
  (which must exist + `starts_with(root)`) then joins the leaf, exactly as
  `FsWriteTool` does. The two roots (`workspace`, `fs` = the access level) are
  the only reach — unchanged from Z.
- **No accidental clobber.** `WriteFile` carries an explicit **`overwrite`**
  flag: the editor's *save* sets it (the file exists, the operator is editing
  it); *new file* sends `false` and the daemon refuses an existing path
  (`exists`). `RenamePath` / `MakeDir` refuse when the target already exists.
- **Delete is hard-gated.** `DeleteFile` removes a **file or an *empty*
  directory** only — never recursive (a non-empty dir errors). It **always**
  requires `confirm: true` (the web shows a confirm modal); the daemon refuses
  without it. This is stricter than `[access] confirm_destructive` on purpose —
  delete is the one no-undo action over this surface.
- **Atomic writes.** `WriteFile` writes via temp-file + rename (the `FsWriteTool`
  pattern) so a crash mid-write never truncates the target. 0644.
- **Audited.** Each successful mutation appends an `AuditEvent` so the Command
  Center feed + forensic walk see web-initiated filesystem changes.

### 14.2 New IPC

All read-write, mirroring the Z `root`/`path` shape:
- **`WriteFile { root, path, content, overwrite }`** → `FsMutation`.
- **`DeleteFile { root, path, confirm }`** → `FsMutation`.
- **`RenamePath { root, path, new_path }`** → `FsMutation`.
- **`MakeDir { root, path }`** → `FsMutation`.
- `FsMutation { ok, error }` — one shared response; the web re-`ListDir`s the
  affected directory on success (and re-`ReadFile`s after a save). Errors map to
  stable codes (`path_escape` / `exists` / `not_empty` / `confirm_required` /
  `io_error`, plus Z's `bad_root` / `no_workspace`).

### 14.3 Web

The Documents file viewer becomes an **editor**: a textarea over the file's text
with **Save** (→ `WriteFile { overwrite: true }`); for binary/oversize files the
viewer stays read-only. The toolbar gains **New file**, **New folder**, and per-
entry **Rename** / **Delete** (delete behind a confirm modal). Notices report each
outcome; the listing refreshes from the daemon's response. Read-only roots/states
degrade cleanly.

### 14.4 Invariants

- **Same reach as Z** — writes never leave the two roots; the canonicalize-
  `starts_with` guard is shared with reads.
- **No silent data loss** — `overwrite` is explicit, rename/mkdir refuse to
  clobber, delete is empty-only + always-confirmed.
- **Every write is audited** — a signed `AuditEvent` per mutation.
- **Studio only / local-first / Stitch** — same rules as R–MG.

### 14.5 Phase plan

| Phase | Deliverable |
|---|---|
| **DW.0** | This contract. |
| **DW.1** | `document_write` primitive in `aivyx-channel` (write/delete/rename/mkdir) reusing the Z guard + a parent-canonicalizing resolve; unit tests incl. escape, clobber-refusal, non-empty-dir, atomic save. |
| **DW.2** | `WriteFile`/`DeleteFile`/`RenamePath`/`MakeDir` IPC + `FsMutation` + daemon handlers (confirm + overwrite enforcement + an `AuditEvent`); round-trip + handler tests. |
| **DW.3** | Web: editor (textarea + Save) + New file/folder + Rename/Delete (confirm modal) + notices + refresh; `ws_task` arms; `stitch.css`. |
| **DW.4** | Finalize: bundle, live-verify (edit+save, create, rename refuses clobber, delete confirms, escape blocked, audited), docs, memory, push. |

> **Status:** ✅ Live (Chapter DW complete). Write loop verified end-to-end via
> the IPC probe — create (`overwrite=false`), save (`overwrite=true`), mkdir,
> rename (clobber refused), delete (confirm gate enforced, then removed), escape
> blocked (`path escapes the allowed root`), and **5 `DocumentMutated` audit
> entries** recorded — plus the rebuilt bundle served byte-identical at `:7843`.
