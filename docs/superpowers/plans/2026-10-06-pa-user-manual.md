# Aivyx PA User Manual Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A comprehensive three-part manual for Aivyx PA — User Guide (`docs/guide/`, refreshed + 5 new chapters, shown in the Studio), Reference (`docs/manual/reference/`) and Developer (`docs/manual/developer/`) — accurate to v0.15.0.

**Architecture:** Markdown chapters numbered `NN-name.md`, one `# Title` each. `docs/manual/README.md` is the index. The Studio bundles `docs/guide/` via `crates/aivyx-web/src/guide.rs`. A test in `aivyx-cli` fails if any command in `help.rs`'s `COMMANDS` table lacks a heading in the CLI reference, so the reference can't silently drift.

**Tech Stack:** Markdown; Rust tests (`aivyx-cli`, `aivyx-web`).

Spec: `aivyx-ecosystem/docs/superpowers/specs/2026-10-06-user-manuals-design.md` §1.

## Global Constraints

- Every statement checked against the current code (CLI `--help`, `help.rs`, the parsers in `aivyx.rs`, config structs in `crates/aivyx-config/src/lib.rs`, `examples/aivyx-pa.toml`, tool registry) — not against older docs, which have drifted. Where a deep doc in `docs/` is right, link to it instead of duplicating.
- Part 1 (Guide): everyday users, task-based, plain words; codenames (Ward, Bulwark, Nonagon…) only with a one-line explanation. Part 2 (Reference): exhaustive, terse. Part 3 (Developer): builders/contributors.
- Brand voice: warm, precise, direct; short sentences. Licence: BUSL-1.1, "source-available" — never "open source".
- Guide links: relative `.md` links only between guide pages (the Studio maps them to pages); links elsewhere use full GitHub URLs `https://github.com/Aivyx-Agent/aivyx-pa/blob/main/<path>`.
- Version: v0.15.0. Never link private repos (aivyx-hub, aivyx-ecosystem, aivyx-website, aivyx-brand, aivyx-wallpapers, aivyx-kitchen).
- Commits: `git commit -s`, ending `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`; branch `docs/user-manual`.
- `cargo test -p aivyx-cli`, `cargo test -p aivyx-web`, `just check-web` (when guide.rs changes), and clippy (stable + 1.99) stay green.

---

### Task 1: Index, CLI reference and the drift check

**Files:** Create `docs/manual/README.md`, `docs/manual/reference/01-cli.md`; modify `crates/aivyx-cli/src/bin/aivyx_modules/help.rs` (test only).

- [ ] **Step 1: Failing drift test** in `help.rs`'s test module:

```rust
#[test]
fn every_command_has_a_cli_reference_heading() {
    let manual = include_str!("../../../../../docs/manual/reference/01-cli.md");
    let missing: Vec<&str> = COMMANDS
        .iter()
        .map(|c| c.name)
        .filter(|name| !manual.contains(&format!("### `aivyx-pa {name}")))
        .collect();
    assert!(missing.is_empty(), "docs/manual/reference/01-cli.md has no heading for: {missing:?}");
}
```

  Create an empty `docs/manual/reference/01-cli.md` with just `# Command-line reference` so it compiles; run `cargo test -p aivyx-cli every_command_has_a_cli_reference_heading` → FAIL listing every command.

- [ ] **Step 2: Write `01-cli.md`.** Sources: `cargo run -p aivyx-cli --bin aivyx-pa -- --help`, `<command> --help` for each, the `COMMANDS` table (summary + usage lines), and each subcommand's parser in `aivyx.rs`. Structure: intro (global flags: `--instance`, `--verify-only`, `--channel`, `--role`, `--print-role`, `--no-daemon`, `--provider`, `--web-ui-port`, `--version`, `--help`) then one `### \`aivyx-pa <name>\`` section per command, in `COMMANDS` order, each with: one-line purpose, every usage form (verbatim from `usage`), every flag/sub-subcommand with a sentence, one realistic example, and a link to the guide chapter that explains it.
- [ ] **Step 3: GREEN.** Re-run the test.
- [ ] **Step 4: `docs/manual/README.md`** — the index: what Aivyx PA is (2 sentences), the three parts with a linked chapter list each (guide chapters 01–18, reference 01–06, developer 01–07), and "where else to look" (`INSTALL.md`, `docs/THREAT_MODEL.md`, `docs/TOOLS.md`, `CHANGELOG.md`).
- [ ] **Step 5: Commit** `docs: manual index and CLI reference (with a drift check)`.

---

### Task 2: Configuration reference

**Files:** Create `docs/manual/reference/02-configuration.md`.

- [ ] **Step 1:** List every top-level TOML section and key the loader accepts: read the `Toml*` structs in `crates/aivyx-config/src/lib.rs` (the raw TOML shape) and `examples/aivyx-pa.toml`. For each key record: type, default, env-var override (search `ENV_` constants), one-line meaning, and the guide chapter it belongs to.
- [ ] **Step 2:** Write the chapter: how config is found (`AIVYX_PA_CONFIG_PATH` → `./aivyx-pa.toml` → instance default), env-beats-TOML precedence, then one `##` per section in the order of `examples/aivyx-pa.toml`, each a table `| Key | Type | Default | Env | Meaning |`. Point to `examples/aivyx-pa.toml` as the annotated example.
- [ ] **Step 3: Check:** every `[section]` in `examples/aivyx-pa.toml` appears as a `##` heading — `grep -oE '^#? ?\[[a-z_.]+\]' examples/aivyx-pa.toml | tr -d '# ' | sort -u` vs headings. Commit `docs: configuration reference`.

---

### Task 3: Tools, files, environment, glossary

**Files:** Create `docs/manual/reference/03-tools.md`, `04-files-and-paths.md`, `05-environment-variables.md`, `06-glossary.md`.

- [ ] `03-tools.md`: every tool the agent can call — name, what it does, capability scope, minimum trust tier, whether it asks first. Source: `docs/TOOLS.md` checked against the registry (`grep -rn 'fn name(&self)' crates/*/src` for tool names). Note which need integrations (`aivyx-pa connect`).
- [ ] `04-files-and-paths.md`: the default table from `INSTALL.md` "Where files land" + the named-instance table, both checked against `crates/aivyx-instance/src/lib.rs`.
- [ ] `05-environment-variables.md`: every `AIVYX_PA_*` and other env var read (`grep -rhoE '"AIVYX_[A-Z_]+"' crates | sort -u`, plus `XDG_*`, `HOME`), each with meaning and which command reads it.
- [ ] `06-glossary.md`: product terms (daemon, Studio, role, Profile, Persona, mission, routine, access level, autonomy level, instance, vertical pack, skill, memory profile) and chapter codenames found in docs/code (Ward, Portcullis, Rampart, Bulwark, Picket, Nonagon, Freight, Passport, Sheaf, Deckhand, Keyring, Anchor…) — one line each, with the doc to read.
- [ ] Commit `docs: tools, files, environment and glossary reference`.

---

### Task 4: Refresh guide chapters 01–13

**Files:** Modify `docs/guide/01-welcome.md` … `13-chat-apps-and-accounts.md`.

- [ ] For each chapter: re-read it against v0.15.0 behaviour (Studio views in `crates/aivyx-web/src/main.rs` `View`, CLI help, config). Fix anything stale (versions, renamed screens, removed/added options, access levels, autonomy levels, the chat-approval flow, routing visibility, named instances mention where relevant). Keep each page's structure and tone; don't pad.
- [ ] Record per chapter what changed (or "verified, no change") in the commit message body.
- [ ] Commit `docs(guide): refresh chapters 01–13 for v0.15.0`.

---

### Task 5: New guide chapters 14–18 and the Studio

**Files:** Create `docs/guide/14-terminal-and-cli.md`, `15-autonomy-and-routines.md`, `16-named-instances.md`, `17-security-and-privacy.md`, `18-backups-upgrades-and-moving.md`; modify `crates/aivyx-web/src/guide.rs` (5 `Page` entries).

- [ ] **Step 1: Failing test** in `guide.rs` tests (or alongside existing ones):

```rust
#[test]
fn every_guide_file_is_registered() {
    let files: Vec<&str> = PAGES.iter().map(|p| p.file).collect();
    for f in [
        "14-terminal-and-cli.md",
        "15-autonomy-and-routines.md",
        "16-named-instances.md",
        "17-security-and-privacy.md",
        "18-backups-upgrades-and-moving.md",
    ] {
        assert!(files.contains(&f), "{f} not registered in guide.rs");
    }
}
```

  RED: `cargo test -p aivyx-web every_guide_file_is_registered`.
- [ ] **Step 2: Write the chapters.** Sources: 14 — CLI help + `INSTALL.md` "The terminal UI"; 15 — `docs/AUTONOMY.md`, `docs/ROUTINES.md`, `INSTALL.md` "Reminders", "Autonomous loop", notifications sections; 16 — `INSTALL.md` "Running several agents" + amendment A17; 17 — `docs/THREAT_MODEL.md`, `docs/ACCESS_LEVELS.md`, `docs/SECURITY_POSTURE.md`, CLAUDE.md's guard list (explain capability gate, audit log, encryption, sandbox, Ward/Portcullis, Rampart, injection check — and what isn't defended); 18 — `INSTALL.md` "Moving Aivyx PA to a new machine", "Uninstall", the installer's upgrade path, store/passphrase backup.
- [ ] **Step 3: Register** five `Page` entries (ids `terminal-and-cli`, `autonomy-and-routines`, `named-instances`, `security-and-privacy`, `backups-upgrades-and-moving`); GREEN; `just check-web`.
- [ ] **Step 4: Commit** `docs(guide): terminal, autonomy, instances, security, backups chapters`.

---

### Task 6: Developer part

**Files:** Create `docs/manual/developer/01-building-from-source.md` … `07-contributing.md`.

- [ ] `01`: toolchain, `cargo build/test`, `default-members` vs `aivyx-web`/`aivyx-desktop`, `just check-web`/`build-web`, `scripts/dev-run.sh`, the pre-commit hook (from CLAUDE.md + `justfile` + INSTALL "Build from source").
- [ ] `02`: architecture — daemon + frontends over IPC, turn loop (D1), crates table, capability/trust tiers, storage, audit (from CLAUDE.md, DESIGN.md — summarise, link).
- [ ] `03`/`04`: channel adapters and tool processes — summarise `docs/CHANNEL_SDK.md` / `docs/TOOL_SDK.md`, point to `examples/python-channel/`, `examples/python-tool/`, conformance suites.
- [ ] `05`: vertical packs (`docs/VERTICAL_PACKS.md`, `docs/FREIGHT.md`, `aivyx-pa pack`). `06`: IPC protocol (`docs/DAEMON_IPC.md`, framing, the wasm-clean `aivyx-ipc`). `07`: contributing — CLA (`CLA.md`), DCO `git commit -s`, zero-clippy-warnings, amendments process for DESIGN/PRODUCT changes, BUSL licence.
- [ ] Commit `docs: developer part of the manual`.

---

### Task 7: Final pass

- [ ] Link check: every relative link in `docs/manual/**` and `docs/guide/*.md` resolves (script: extract `](…)` targets, test `-e` relative to the file; full URLs to `github.com/Aivyx-Agent/aivyx-pa/blob/main/<path>` must exist locally at `<path>`).
- [ ] `grep -rniE 'open.source|v0\.1[0-4]\.' docs/manual docs/guide` → no stale version or licence wording (allow "open-source" only when describing other projects).
- [ ] `cargo test -p aivyx-cli -p aivyx-web`, `just check-web`, clippy stable + 1.99.
- [ ] README.md: one line pointing to `docs/manual/README.md`.
- [ ] Commit; merge/push per the user's choice.
