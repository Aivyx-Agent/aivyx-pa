# Per-Area Autonomy Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `[[autonomy.override]]` gate each tool call by the level of its area (the first word of its capability scope), for the "ask before any change" behaviour and for delete/overwrite confirmation.

**Architecture:** `aivyx-capability` exposes the valid areas; `aivyx-config` validates overrides against them and resolves a posture per area. `aivyx-core` replaces the agent's single `confirm_all` bool with a `ConfirmAllAreas` table looked up per call. The CLI daemon builds that table and gives each fs/data/git tool the delete/overwrite flag of its own area. Visibility: start-up warning, `autonomy show`, the Studio settings snapshot, docs.

**Tech Stack:** Rust (aivyx-capability, aivyx-config, aivyx-core, aivyx-ipc, aivyx-channel, aivyx-cli, aivyx-web).

Spec: `docs/superpowers/specs/2026-10-06-per-area-autonomy-design.md`.

## Global Constraints

- No `[autonomy]` section ⇒ behaviour identical to today; existing tests keep passing unchanged except where named below.
- Integration writes ask at every level unless `[access] confirm_destructive = false` is explicit — untouched. Unattended runs refuse irreversible steps — untouched. Loop arming stays global.
- An area = first word of a capability scope base. `schedules` is an alias for `schedule`. Unknown area or duplicate area = config load error.
- An explicit `[access] confirm_destructive` wins over every area.
- `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, the Rust 1.99 clippy run, `just check-web`, and `cargo check -p aivyx-cli --features channel-voice,yubikey` stay green.
- Commits: `git commit -s`, ending `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`; branch `feat/per-area-autonomy`.

---

### Task 1: Areas, validation and per-area resolution

**Files:** Modify `crates/aivyx-capability/src/lib.rs`, `crates/aivyx-config/src/autonomy.rs`, `crates/aivyx-config/src/lib.rs` (override parsing ~6438, `effective_autonomy` ~6172), `crates/aivyx-config/src/tests.rs`, `crates/aivyx-cli/src/bin/aivyx.rs` (`Some("schedules")` ~10002).

**Interfaces — produces:**
- `aivyx_capability::areas() -> std::collections::BTreeSet<&'static str>`
- `aivyx_config::autonomy::normalize_area(domain: &str) -> &str` (`"schedules"` → `"schedule"`)
- `AutonomyLevel: PartialOrd + Ord` (declaration order manual < assisted < supervised < autonomous < unleashed)
- `aivyx_config::looser_overrides(global: AutonomyLevel, overrides: &[AutonomyOverride]) -> Vec<&AutonomyOverride>`
- `AivyxConfig::posture_for_area(&self, area: &str) -> AutonomyPosture`, `AivyxConfig::posture_for_base(&self, base: &str) -> AutonomyPosture`

- [ ] **Step 1: Failing tests.** In `aivyx-capability` tests:

```rust
#[test]
fn areas_are_the_first_words_of_known_bases() {
    let areas = areas();
    for a in ["fs", "shell", "git", "email", "calendar", "schedule", "mcp"] {
        assert!(areas.contains(a), "{a} missing");
    }
    for base in KNOWN_BASES {
        assert!(areas.contains(base.split('.').next().unwrap()));
    }
}
```

In `aivyx-config/src/tests.rs` (use the file's existing TOML-loading helper for config tests — the one the other `[autonomy]` tests use):

```rust
#[test]
fn autonomy_override_area_is_validated_and_normalised() {
    // unknown area → error naming it and listing valid areas
    let err = load_toml_str("[autonomy]\nlevel = \"assisted\"\n[[autonomy.override]]\ndomain = \"emial\"\nlevel = \"manual\"\n").unwrap_err().to_string();
    assert!(err.contains("emial") && err.contains("email"), "{err}");
    // duplicate area → error
    let err = load_toml_str("[[autonomy.override]]\ndomain = \"email\"\nlevel = \"manual\"\n[[autonomy.override]]\ndomain = \"email\"\nlevel = \"autonomous\"\n").unwrap_err().to_string();
    assert!(err.contains("email") && err.contains("more than once"), "{err}");
    // alias
    let cfg = load_toml_str("[[autonomy.override]]\ndomain = \"schedules\"\nlevel = \"autonomous\"\n").unwrap();
    assert_eq!(cfg.autonomy_overrides[0].domain, "schedule");
}

#[test]
fn posture_resolves_per_area_and_base() {
    let cfg = load_toml_str("[autonomy]\nlevel = \"assisted\"\n[[autonomy.override]]\ndomain = \"email\"\nlevel = \"manual\"\n[[autonomy.override]]\ndomain = \"fs\"\nlevel = \"unleashed\"\n").unwrap();
    assert_eq!(cfg.posture_for_base("email.send"), AutonomyLevel::Manual.expand());
    assert_eq!(cfg.posture_for_base("fs.delete"), AutonomyLevel::Unleashed.expand());
    assert_eq!(cfg.posture_for_base("shell.exec"), AutonomyLevel::Assisted.expand());
    assert_eq!(cfg.posture_for_area("schedules"), cfg.posture_for_area("schedule"));
    let looser: Vec<_> = crate::looser_overrides(cfg.autonomy_level.value, &cfg.autonomy_overrides)
        .into_iter().map(|o| o.domain.as_str()).collect();
    assert_eq!(looser, vec!["fs"]);
}
```

(If no `load_toml_str` helper exists, write one in the test file: write the string to a temp `aivyx-pa.toml` and load it the way the neighbouring `[autonomy]` tests do. `AutonomyPosture` needs `PartialEq` — add the derive if missing.)

Run `cargo test -p aivyx-capability areas_are && cargo test -p aivyx-config autonomy_override_area posture_resolves` → FAIL (missing items).

- [ ] **Step 2: Implement.**
  - `aivyx-capability`: `pub fn areas() -> BTreeSet<&'static str> { KNOWN_BASES.iter().filter_map(|b| b.split('.').next()).collect() }` with a doc comment.
  - `autonomy.rs`: derive `PartialOrd, Ord` on `AutonomyLevel`; `pub fn normalize_area(domain: &str) -> &str { match domain { "schedules" => "schedule", d => d } }`; `pub fn looser_overrides(global, overrides) -> Vec<&AutonomyOverride> { overrides.iter().filter(|o| o.level > global).collect() }`; re-export both from `lib.rs`'s `pub use autonomy::{…}`. `resolve_posture` normalises its `domain` argument.
  - `lib.rs` override parsing: after the non-empty check, `let domain = normalize_area(&domain).to_string();` then reject `!aivyx_capability::areas().contains(domain.as_str())` with `ConfigError::Invalid { field: "autonomy.override.domain", reason: format!("`{domain}` isn't an area; use one of: {}", areas joined ", ") }`; after collecting, reject a duplicate domain (`reason: format!("area `{d}` has more than one `[[autonomy.override]]`")`).
  - `AivyxConfig::posture_for_area(area) = self.effective_autonomy(Some(area))` and `posture_for_base(base) = self.posture_for_area(base.split('.').next().unwrap_or(base))`.
  - `aivyx.rs`: `Some("schedules")` → `Some("schedule")`.
- [ ] **Step 3:** Tests pass; `cargo test -p aivyx-config -p aivyx-capability` all green.
- [ ] **Step 4: Commit** `feat(autonomy): validated areas and per-area posture resolution`.

---

### Task 2: Per-call "ask before any change" in the turn loop

**Files:** Modify `crates/aivyx-core/src/agent.rs` (field `confirm_all` ~287, setter ~456, ask decision ~1906, `TurnSafety` ~1285–1375, tests ~6549), `crates/aivyx-core/src/lib.rs` (re-export).

**Interfaces — produces:**

```rust
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConfirmAllAreas {
    pub default: bool,
    pub areas: std::collections::BTreeMap<String, bool>,
}
impl ConfirmAllAreas {
    pub fn everywhere(on: bool) -> Self;             // default = on, no areas
    pub fn for_base(&self, base: &str) -> bool;      // area = first word
}
ConcreteAgent::with_confirm_all_areas(self, ConfirmAllAreas) -> Self
TurnSafety::with_confirm_all_areas(self, ConfirmAllAreas) -> Self
```

`ConcreteAgent::with_confirm_all(bool)` and `TurnSafety::with_confirm_all(bool)` stay, delegating to `ConfirmAllAreas::everywhere`.

- [ ] **Step 1: Failing tests** (agent.rs tests, beside the existing `with_confirm_all(true)` tests at ~6549, reusing their FakeTool/delete-plan helpers):

```rust
#[test]
fn confirm_all_areas_looks_up_the_call_area() {
    let mut t = ConfirmAllAreas::everywhere(false);
    t.areas.insert("email".into(), true);
    assert!(t.for_base("email.send"));
    assert!(!t.for_base("fs.write"));
    let mut m = ConfirmAllAreas::everywhere(true);
    m.areas.insert("shell".into(), false);
    assert!(!m.for_base("shell.exec"));
    assert!(m.for_base("fs.write"));
}
```

plus two turn-loop tests copied from the nearest existing `with_confirm_all(true)` test: (a) `ConfirmAllAreas { default: false, areas: {"fs": true} }` → an `fs.write` call escalates with a reason containing "`fs`"; (b) `{ default: true, areas: {"fs": false} }` → the same call completes.

Run `cargo test -p aivyx-core confirm_all_areas` → FAIL.

- [ ] **Step 2: Implement.** Field `confirm_all: ConfirmAllAreas` (default `ConfirmAllAreas::everywhere(false)`); ask decision: `self.confirm_all.for_base(needed.base())`; reason: `format!("{tool_name} would change something, and the autonomy level for `{area}` is manual: every change there needs approval first.")` where `area` = first word of `needed.base()`. `TurnSafety` stores `ConfirmAllAreas` and `apply` calls `with_confirm_all_areas`. Re-export `ConfirmAllAreas` from `aivyx-core`. Update the existing manual-level test assertions on the reason text if they match its wording.
- [ ] **Step 3:** `cargo test -p aivyx-core` green.
- [ ] **Step 4: Commit** `feat(autonomy): ask before any change per area`.

---

### Task 3: Daemon wiring — the table and per-area delete/overwrite

**Files:** Modify `crates/aivyx-cli/src/bin/aivyx.rs` (`manual_autonomy` ~6989, the four `.with_confirm_all(manual_autonomy)` sites ~9921/10473/11245/11837, the confirm values ~7642, git commit tool ~8634), add helpers + tests in the same file's test module.

**Interfaces — consumes:** Task 1's `resolve_posture`, `looser_overrides`; Task 2's `ConfirmAllAreas`. **Produces (aivyx.rs private):**

```rust
fn confirm_all_areas(
    level: aivyx_config::AutonomyLevel,
    overrides: &[aivyx_config::AutonomyOverride],
) -> aivyx_core::ConfirmAllAreas;
fn area_confirm_destructive(
    configured: &aivyx_config::Sourced<bool>,
    level: aivyx_config::AutonomyLevel,
    overrides: &[aivyx_config::AutonomyOverride],
    area: &str,
) -> bool;
```

- [ ] **Step 1: Failing tests** (aivyx.rs test module):

```rust
#[test]
fn confirm_all_table_follows_the_overrides() {
    use aivyx_config::{AutonomyLevel as L, AutonomyOverride as O};
    let ov = vec![O { domain: "email".into(), level: L::Manual }, O { domain: "shell".into(), level: L::Autonomous }];
    let t = confirm_all_areas(L::Assisted, &ov);
    assert!(!t.default);
    assert!(t.for_base("email.send"));
    assert!(!t.for_base("shell.exec"));
    let t = confirm_all_areas(L::Manual, &ov);
    assert!(t.default && !t.for_base("shell.exec"));
}

#[test]
fn delete_confirmation_follows_each_tools_area() {
    use aivyx_config::{AutonomyLevel as L, AutonomyOverride as O, FieldSource, Sourced};
    let unset = Sourced::new(true, FieldSource::Default);
    let ov = vec![O { domain: "fs".into(), level: L::Unleashed }];
    assert!(!area_confirm_destructive(&unset, L::Assisted, &ov, "fs"));
    assert!(area_confirm_destructive(&unset, L::Assisted, &ov, "git"));
    let explicit = Sourced::new(true, FieldSource::Toml);
    assert!(area_confirm_destructive(&explicit, L::Assisted, &ov, "fs"));
}
```

Run → FAIL (functions missing).

- [ ] **Step 2: Implement.** `confirm_all_areas`: `default = resolve_posture(level, overrides, None).gate == GatePosture::ConfirmAll`; `areas` = each override's `domain` → `override.level.expand().gate == GatePosture::ConfirmAll`. `area_confirm_destructive` = `aivyx_config::confirm_destructive_for(configured, &aivyx_config::resolve_posture(level, overrides, Some(area)))`. Wire: replace `manual_autonomy` with `let confirm_all_areas = confirm_all_areas(autonomy_level.value, &autonomy_overrides);` and each `.with_confirm_all(manual_autonomy)` with `.with_confirm_all_areas(confirm_all_areas.clone())` (the voice arm is behind `channel-voice` — check with `cargo check -p aivyx-cli --features channel-voice,yubikey`). At ~7642, before shadowing: `let confirm_destructive_git = area_confirm_destructive(&confirm_destructive, autonomy_level.value, &autonomy_overrides, "git");` and the shadowed `confirm_destructive` becomes `area_confirm_destructive(&confirm_destructive, …, "fs")` (comment: used by fs.write/fs.delete and the data writers). The git commit tool (~8634) takes `confirm_destructive_git`. Start-up warning right after `autonomy_posture` is computed: for each `looser_overrides(...)`, `eprintln!("aivyx-pa: autonomy for `{}` is {}, looser than the global level ({}).", o.domain, o.level, autonomy_level.value)`.
- [ ] **Step 3:** `cargo test -p aivyx-cli`, the feature check above, workspace tests.
- [ ] **Step 4: Commit** `feat(autonomy): per-area wiring in the daemon`.

---

### Task 4: Visibility and docs

**Files:** Modify `crates/aivyx-cli/src/bin/aivyx_modules/autonomy.rs` (show), `crates/aivyx-ipc/src/protocol.rs` (`SettingsSnapshot`), `crates/aivyx-channel/src/daemon_server.rs` (`settings_snapshot`), `crates/aivyx-web/src/main.rs` (Autonomy section ~6888), `docs/guide/08-access-and-settings.md`, `docs/guide/15-autonomy-and-routines.md`, `docs/manual/reference/01-cli.md`, `scripts/gen-config-reference.py` (+ regenerate), `docs/AUTONOMY.md` (closeout row), `CHANGELOG.md`.

- [ ] **Step 1: `autonomy show`** — failing test first (the module's existing show-render test pattern): with overrides `email=manual`, `fs=unleashed` under `assisted`, the output contains `email → manual (asks before any change)`, `fs → unleashed (deletes and overwrites run; looser than global)`, and a `areas:` line listing valid areas. Implement: per override, effect text = `asks before any change` if ConfirmAll, else `deletes and overwrites run` if `!confirm_destructive`, else `deletes and overwrites ask`; append `; looser than global` for looser ones; then `areas: <aivyx_capability::areas() joined ", ">`.
- [ ] **Step 2: Snapshot + Studio.** `SettingsSnapshot` gains `#[serde(default)] pub autonomy_overrides: Vec<(String, String)>` (area, level); `settings_snapshot` fills it; the Studio Autonomy section lists them under the level picker ("Per-area overrides (edit in `aivyx-pa.toml`): email → manual · fs → unleashed", or "none"), and the `unleashed` option text becomes "unleashed — deletes and overwrites run without asking; emails and other outbound actions still ask (isolated machines only)". Update the protocol round-trip test fixture (~4957) with the new field. `just check-web`.
- [ ] **Step 3: Docs.** Guide 08: replace "different levels for different areas … planned" with a short "Different levels per area" paragraph + example (`email = manual`, `fs = unleashed`) and the looser-warning note. Guide 15: rewrite the "Different levels for different areas are on the way" block into how overrides work now (areas = first word of a capability; `aivyx-pa autonomy show` lists them; manual-per-area and delete/overwrite-per-area; loop stays global). CLI reference: replace the "only the `schedules` domain changes behaviour" sentence. `gen-config-reference.py`: `autonomy.override.domain` meaning → "The area this applies to: the first word of a capability (`fs`, `shell`, `git`, `email`…; `aivyx-pa autonomy show` lists them). `schedules` also works." — regenerate `02-configuration.md`. AUTONOMY.md closeout table: add a row "Per-area overrides (2026-10-06)". CHANGELOG Unreleased → `### Added`: per-area autonomy.
- [ ] **Step 4:** Full checks (Global Constraints). Commit `feat(autonomy): show per-area overrides; docs`. Finish the branch per the user's choice.
