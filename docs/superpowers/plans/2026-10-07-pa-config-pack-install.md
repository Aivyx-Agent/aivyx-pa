# aivyx-pa Config-Pack Installer Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `aivyx-pa pack check <dir>` validates a config pack's aivyx-pa part, and `aivyx-pa pack install <file>` turns a format-2 config pack into a new named agent, set up through the normal wizard pre-filled from the pack's template.

**Architecture:** A new `pack_check.rs` module does every product check on an unpacked pack directory and returns all the problems at once. It is used by `pack check`, and by install before anything is kept. Install for format 2 verifies the bundle, then picks the target instance and unpacks into that instance's home under `packs/<name>/<version>/`. It runs the checks on what it unpacked, splices `[pack] source`, `[team] config_path` and `[skill_defaults] project_dir` into the pack's template, and runs the existing setup wizard with that template, like `instances create`. Format-1 packs keep today's path.

**Tech Stack:** Rust; `aivyx-pack` (shared crate, pinned rev), `aivyx-config`, `aivyx-team::TeamConfig`, `aivyx-skills::SkillLoader`, `toml_edit`.

Spec: `aivyx-ecosystem/docs/superpowers/specs/2026-10-07-vertical-packs-design.md`, sub-project 2.

**One change from the spec:** the spec's §2 said install writes the config and the user then runs `aivyx-pa --instance <name> init`. That loses the pack, because `init` overwrites without a template. Instead, install runs the same setup wizard itself, pre-filled from the pack's template, exactly as `aivyx-pa instances create` already does. The target name comes from the existing global `--instance` flag; with none given, it is the pack's name.

## Global Constraints

- A pack's template may not set the global level or any `[[autonomy.override]]` above `supervised`; `check` and `install` refuse it.
- Known integration names for `requires` / `optional`: `gmail`, `calendar`, `drive`, `contacts` (set up with `aivyx-pa connect`), and `notion`, `obsidian`, `n8n`, `toolkit`, `vision` (tool processes).
- Install never touches another instance, never starts the daemon, and refuses an instance that already exists.
- Nothing is half-installed: unpack to a temporary folder inside the instance's `packs/` dir, check, then rename into `packs/<name>/<version>/`; on any failure (including the wizard failing) the folder is removed.
- Format-1 (tool) packs behave exactly as today.
- `min_version` is checked against `env!("CARGO_PKG_VERSION")` with `aivyx_pack::daemon_version_ok`.
- Paths: every aivyx-pa path comes from `aivyx_instance::InstancePaths` (a guard test enforces it); add `InstancePaths::packs_dir()` = `home_dir()/packs`.
- The workspace isn't rustfmt-clean; match the surrounding style.
- Verification: `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, the Rust 1.99 clippy (`touch crates/aivyx-config/src/lib.rs; CARGO_TARGET_DIR=target/rust199 ~/.cargo/bin/cargo +1.99.0 clippy --workspace --all-targets -- -D warnings`).
- Commits: `git commit -s`, ending `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`. CHANGELOG must not gain `Authorization: Bearer` text.

---

### Task 1: `pack check`

**Files:**
- Create: `crates/aivyx-cli/src/bin/aivyx_modules/pack_check.rs`
- Modify: `crates/aivyx-cli/src/bin/aivyx.rs` (`#[path] mod pack_check;`, `PackSubcommand::Check { dir }`, parse `pack check <dir>`, parse test)
- Modify: `crates/aivyx-cli/src/bin/aivyx_modules/pack.rs` (dispatch `Check`)

**Interfaces:**
- Produces:

```rust
pub const KNOWN_INTEGRATIONS: &[&str] =
    &["gmail", "calendar", "drive", "contacts", "notion", "obsidian", "n8n", "toolkit", "vision"];

/// What a good aivyx-pa part holds.
pub struct PaPartSummary {
    pub manifest: aivyx_pack::ConfigPackManifest,
    pub skills: Vec<String>,          // skill names found
    pub team_members: usize,          // 0 when no team config
    pub routines: usize,              // [[schedule]] entries in the template
}

/// Check the aivyx-pa part of an unpacked config pack (or a staging dir).
/// `Err` lists every problem found, not just the first.
pub fn check_pa_part(dir: &Path) -> Result<PaPartSummary, Vec<String>>;
```

Checks, in order (collect problems; stop early only if the manifest itself is unreadable):
1. `manifest.toml` parses with `aivyx_pack::Manifest::parse`; it must be `Manifest::Config` with a `pa` part (else "this pack has no aivyx-pa part" / "this is a tool pack — `pack check` checks config packs").
2. Every `paths()` entry for the pa part exists (file or folder).
3. The template loads as a real config: `AivyxConfig::load_from_env_and_toml(&LoadOptions { toml_path: Some(template), require_api_key: false, require_telegram_token: false, require_discord_token: false, require_slack_tokens: false, role_override: None })` → `Err(e)` becomes "the template doesn't load: {e}".
4. Autonomy ceiling on the loaded config: `autonomy_level.value > AutonomyLevel::Supervised` → "the template sets autonomy `{level}`; a pack can't go above `supervised`"; same per override ("… for `{domain}` …").
5. Team config (if named): `aivyx_team::config::TeamConfig::load(path)` → `Err` becomes "the team config doesn't load: {e}"; record member count.
6. Skills (if named): for each subfolder `<name>` holding `SKILL.md`, `SkillLoader::new().with_project_dir(dir).get(name)` must be `Some` with a non-empty body → else "skill `{name}` doesn't parse (it needs `name` and `description` frontmatter, and `name` must match its folder)"; a skills folder with no skills → "the skills folder has no skills".
7. Every `requires`/`optional` name is in `KNOWN_INTEGRATIONS` → else "unknown integration `{x}` (known: …)".
8. Routines: count the template's `[[schedule]]` entries (from the loaded config's schedules).

CLI output on success:

```
✓ business-manager v0.1.0 — aivyx-pa part looks good
  template loads; autonomy within supervised
  team: 6 members · skills: 8 · routines: 5
  requires: gmail, calendar, contacts, drive, toolkit · optional: notion
```

On failure: `✗ <n> problem(s) in the aivyx-pa part:` then one `  - …` line each; exit error.

- [ ] **Step 1: Failing tests** (in `pack_check.rs`; a helper writes a good pack dir: manifest with a `[pa]` part, a template with `[profile]`, `[autonomy] level = "assisted"` + an `fs` override at `supervised`, one `[[schedule]]`, a team TOML copied from `examples/team.toml` if one exists or a minimal lead + one specialist, and one skill `pa/skills/example/SKILL.md` with frontmatter `name: example`, `description: An example.`)

```rust
#[test] fn a_good_pa_part_passes() {
    let d = good_pack("good");
    let s = check_pa_part(&d).unwrap();
    assert_eq!(s.skills, vec!["example"]);
    assert_eq!(s.routines, 1);
    assert!(s.team_members >= 2);
}
#[test] fn autonomy_above_supervised_is_refused() {
    let d = good_pack("unleashed");
    rewrite(&d, "pa/aivyx-pa.toml", |t| t.replace(r#"level = "assisted""#, r#"level = "unleashed""#));
    assert!(check_pa_part(&d).unwrap_err().iter().any(|p| p.contains("can't go above `supervised`")));
}
#[test] fn an_override_above_supervised_is_refused() { /* fs override → "autonomous" → error naming `fs` */ }
#[test] fn every_problem_is_reported_at_once() {
    let d = good_pack("many");
    std::fs::remove_dir_all(d.join("pa/skills/example")).unwrap();
    rewrite(&d, "manifest.toml", |t| t.replace(r#"requires = ["gmail"]"#, r#"requires = ["gmial"]"#));
    rewrite(&d, "pa/team.toml", |_| "not = [valid".into());
    let problems = check_pa_part(&d).unwrap_err();
    assert_eq!(problems.len(), 3, "{problems:?}");
}
#[test] fn a_skill_with_a_mismatched_name_is_refused() { /* frontmatter name: other → error names `example` */ }
#[test] fn a_template_that_doesnt_load_is_refused() { /* unknown autonomy area → "the template doesn't load" */ }
#[test] fn a_tool_pack_or_coder_only_pack_is_explained() { /* format-1 manifest; products = ["coder"] */ }
```

- [ ] **Step 2: Run** — `cargo test -p aivyx-cli --bin aivyx-pa pack_check` → fails (module missing).
- [ ] **Step 3: Implement** the module, the `Check` subcommand (`pack check <dir>`; the parse test `parse_cli_args_from(&argv(&["pack","check","bm/"]))` → `CliMode::Pack(PackSubcommand::Check { dir: "bm/".into() })`), and the renderer.
- [ ] **Step 4: Run** — `cargo test -p aivyx-cli --bin aivyx-pa pack` → pass.
- [ ] **Step 5: Commit** — `feat(cli): aivyx-pa pack check for config packs`.

---

### Task 2: Install a config pack as a new agent

**Files:**
- Modify: `crates/aivyx-instance/src/lib.rs` (`pub fn packs_dir(&self) -> Option<PathBuf>` = `home_dir()/packs`, + test)
- Modify: `crates/aivyx-cli/src/bin/aivyx_modules/pack.rs` (format dispatch in `install`; `prepare_config_install`; `run_config_install`)
- Modify: `crates/aivyx-cli/src/bin/aivyx_modules/init_templates.rs` (`TemplateSource::Pack`, label `"pack"`)
- Modify: `crates/aivyx-cli/src/bin/aivyx_modules/instances.rs` (make the port choice reusable: `pub fn pick_port() -> u16` wrapping `used_ports` + `choose_port` + busy probe; `pub fn write_port` visibility `pub(crate)`)
- Modify: `crates/aivyx-cli/src/bin/aivyx.rs` (`PackSubcommand::Install` dispatch moves before the generic `CliMode::Pack` arm: for a config pack, set `AIVYX_PA_INSTANCE` to the target name before the runtime exists, then run the async install)

**Interfaces:**
- Consumes: Task 1 `check_pa_part`.
- Produces:

```rust
/// Everything decided before the wizard runs.
pub struct ConfigInstall {
    pub instance: InstanceName,
    pub pack_dir: PathBuf,              // …/packs/<name>/<version>
    pub template: init_templates::Template,  // the pack template with the installer's keys spliced in
    pub summary: pack_check::PaPartSummary,
}

/// Verify, check and unpack; no wizard. `selected` is the process's instance
/// (`default` means "none chosen" → the pack's name).
pub fn prepare_config_install(
    bundle: &aivyx_pack::ReadBundle,
    manifest: &aivyx_pack::ConfigPackManifest,
    selected: &InstanceName,
    dirs: &aivyx_instance::BaseDirs,
    existing: &[InstanceName],
) -> Result<ConfigInstall, String>;

/// `pack install` for a config pack: prepare, run the setup wizard with the
/// spliced template, write the Studio port, print next steps. Removes the
/// unpacked folder if anything fails.
pub async fn run_config_install(file: &Path) -> Result<(), String>;

/// The target instance for a config pack (used by main before the runtime).
pub fn config_pack_target(file: &Path, selected: &InstanceName) -> Result<Option<InstanceName>, String>;
//   Ok(None) for a format-1 pack (main keeps today's path).
```

`prepare_config_install` steps:
1. Must have a `pa` part (else "this pack has no aivyx-pa part — it's for aivyx-coder").
2. `daemon_version_ok(&pa.min_version, env!("CARGO_PKG_VERSION"))`.
3. Target = `selected` unless it is `default`, then `InstanceName::parse(&manifest.name)`; refuse `default` and any name in `existing` ("instance `x` already exists — pick another with `--instance <name>`").
4. `packs_dir/<name>/.tmp-<random>` ← `unpack_payload`; `check_pa_part(tmp)` (problems joined into the error); remove any old `packs_dir/<name>/<version>`; rename tmp → `pack_dir`.
5. Template doc = `pack_dir/<pa.template>` parsed with `toml_edit`; set `[pack] source = "<name>@<version>"`; if `team_config`, `[team] config_path = <abs path>`; if `skills`, `[skill_defaults] project_dir = <abs path>`; `Template { name: manifest.name, description: "pack <name> v<version>", source: TemplateSource::Pack, toml_content: doc.to_string() }`.
6. On any error after step 4 created the folder, remove it.

`run_config_install`: read + verify the bundle (trusted publishers from the current config if one exists, else an empty list — so the compiled-in key set still applies), `read_any_manifest`, `prepare_config_install`, then `crate::init::run_init_wizard(Some(&template))`; on `Err`, remove `pack_dir` and return the error; then `instances::write_port(&config, instances::pick_port())` when the config exists; print:

```
Installed business-manager v0.1.0 as instance `business` (Studio port 8091).
Next:
  aivyx-pa --instance business connect gmail      (also: calendar, contacts, drive)
  add the toolkit as a [[tool_process]] — see docs/INSTALL.md
  optional: notion
Start it with `aivyx-pa --instance business daemon start`.
```

(Connect lines for `requires` ∩ {gmail, calendar, drive, contacts}; a line per other required tool process; `optional` listed.)

main: in the `CliMode::Pack(PackSubcommand::Install { file })` case, call `pack::config_pack_target(file, &InstanceName::from_env()?)`; `Some(name)` → `unsafe { std::env::set_var(aivyx_instance::ENV_INSTANCE, name.as_str()) }` (same SAFETY note as `instances create`), build a current-thread runtime, `rt.block_on(pack::run_config_install(file))`; `None` → `pack::run_pack(sub)` as today.

- [ ] **Step 1: Failing tests** (`pack.rs` tests; build a signed bundle in a temp dir with `aivyx_pack::{keygen_to_file, build_payload, write_bundle}` from the Task 1 good-pack helper; `BaseDirs` with `home`/`xdg_*` pointing into a temp dir)

```rust
#[test] fn a_config_pack_prepares_a_new_instance() {
    let (bundle, manifest, dirs) = signed_config_pack("prep");
    let c = prepare_config_install(&bundle, &manifest, &InstanceName::default_instance(), &dirs, &[]).unwrap();
    assert_eq!(c.instance.as_str(), "business-manager");
    assert!(c.pack_dir.ends_with("packs/business-manager/0.1.0"));
    assert!(c.pack_dir.join("pa/skills/example/SKILL.md").is_file());
    let doc: toml_edit::DocumentMut = c.template.toml_content.parse().unwrap();
    assert_eq!(doc["pack"]["source"].as_str(), Some("business-manager@0.1.0"));
    assert!(doc["team"]["config_path"].as_str().unwrap().ends_with("pa/team.toml"));
    assert!(doc["skill_defaults"]["project_dir"].as_str().unwrap().ends_with("pa/skills"));
}
#[test] fn the_selected_instance_name_wins() { /* selected = "shop" → instance "shop", pack_dir under instances/shop */ }
#[test] fn an_existing_instance_is_refused_and_nothing_is_left() {
    // existing = [business-manager] → Err containing "already exists"; packs dir has no pack folder
}
#[test] fn a_pack_that_fails_its_checks_leaves_nothing() {
    // template level = "unleashed" → Err containing "supervised"; no packs/<name>/ folder (tmp removed)
}
#[test] fn a_pack_needing_a_newer_aivyx_pa_is_refused() { /* pa.min_version = "99.0.0" → "upgrade" */ }
#[test] fn a_coder_only_pack_is_explained() { /* products = ["coder"] → "no aivyx-pa part" */ }
#[test] fn config_pack_target_is_none_for_a_tool_pack() { /* format-1 bundle → Ok(None) */ }
```

aivyx-instance: `packs_dir_sits_in_the_instance_home` (default → `~/.aivyx-pa/packs`; named → `~/.aivyx-pa/instances/<n>/packs`).

- [ ] **Step 2: Run** — fails.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** — `cargo test -p aivyx-instance -p aivyx-cli` → pass (format-1 pack tests unchanged).
- [ ] **Step 5: Commit** — `feat(cli): install a config pack as a new agent`.

---

### Task 3: Inspect config packs; show where an agent came from

**Files:**
- Modify: `crates/aivyx-cli/src/bin/aivyx_modules/pack.rs` (`inspect`: `read_any_manifest`; format 2 → `aivyx_pack::describe::describe`, plus routines/autonomy lines from the template when the pa part is present — read the template text out of the payload without unpacking via a small `payload_file(payload, rel) -> Option<String>` helper)
- Modify: `crates/aivyx-config/src/lib.rs` (`RawPack.source: Option<String>` → `AivyxConfig.pack_source: Option<String>`), `crates/aivyx-config/src/tests.rs`
- Modify: `crates/aivyx-cli/src/bin/aivyx_modules/instances.rs` (`instances list` gains a `PACK` column: the instance config's `[pack] source`, read with `toml_edit` like `configured_port`, else `-`)
- Modify: every full `AivyxConfig` destructure (e.g. `aivyx.rs` ~6628) to bind `pack_source`

- [ ] **Step 1: Failing tests** — config: `[pack] source = "bm@0.1.0"` → `cfg.pack_source == Some("bm@0.1.0")`; absent → `None`. instances: a pure `pack_source_of(text) -> String` returns the source or `-`. pack: `render_inspect` for a config pack contains `kind:        config pack`, `requires:`, `routines: 1`, `autonomy: assisted (fs: supervised)`.
- [ ] **Step 2: Run** — fails.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** — pass.
- [ ] **Step 5: Commit** — `feat(cli): inspect config packs; instances list shows each agent's pack`.

---

### Task 4: Docs, help, CHANGELOG

**Files:**
- Modify: `crates/aivyx-cli/src/bin/aivyx_modules/help.rs` (pack usage: add `aivyx-pa pack check <dir>`; install line `aivyx-pa [--instance <name>] pack install <bundle-file>`)
- Modify: `docs/manual/reference/01-cli.md` (`aivyx-pa pack` section: config packs, check, install as a new agent, the name rule)
- Modify: `docs/VERTICAL_PACKS.md` (new §"Config packs (format 2)": what they hold, `pack check`, install as a new agent, the `supervised` ceiling, known integration names; pointer to `aivyx-pack`'s README for the format)
- Modify: `docs/FREIGHT.md` (the 2026-10-07 note: format 2 now installs)
- Modify: `docs/guide/16-named-instances.md` (a short "From a pack" paragraph)
- Modify: `scripts/gen-config-reference.py` (meaning for `pack.source`) → regenerate `docs/manual/reference/02-configuration.md`
- Modify: `CHANGELOG.md` (Unreleased → Added: config packs — `pack check`, `pack install` creates a new agent from the pack, `pack inspect` shows config packs, `instances list` shows each agent's pack)

- [ ] **Step 1:** Edit the files; `python3 scripts/gen-config-reference.py > docs/manual/reference/02-configuration.md`.
- [ ] **Step 2: Run** — the help drift test and the full sweep (Global Constraints).
- [ ] **Step 3: Commit** — `docs: config packs in aivyx-pa`.
