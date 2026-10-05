# Named Instances Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let one OS user run several fully separate Aivyx PA agents side by side (`aivyx-pa --instance research …`), each with its own config, store, socket, working dirs, keyring entry, service and Studio port, while the default instance keeps today's paths byte-for-byte.

**Architecture:** A new zero-dependency crate `aivyx-instance` owns instance names and *every* aivyx-pa path/name (`InstancePaths`). The CLI strips a global `--instance` flag once in `main`, exports it as `AIVYX_PA_INSTANCE`, and the tool bridge forwards that variable to tool processes, so every process of an instance resolves the same paths. A guard test forbids building aivyx-pa paths anywhere else.

**Tech Stack:** Rust (edition 2024), workspace crates; existing hand-rolled CLI parser in `crates/aivyx-cli/src/bin/aivyx.rs`; Dioxus wasm Studio (`crates/aivyx-web`).

Spec: `docs/superpowers/specs/2026-10-05-named-instances-design.md`.

## Global Constraints

- Instance names: 1–32 chars, `[a-z0-9-]`, not starting/ending with `-`; `default` is reserved and means today's install.
- Selection: `--instance <name>` (also `--instance=<name>`) beats `AIVYX_PA_INSTANCE`; neither → `default`.
- **The default instance must resolve every path/name byte-identically to today's code**, for every combination of `HOME` / `XDG_CONFIG_HOME` / `XDG_DATA_HOME` / `XDG_RUNTIME_DIR` set or unset.
- Named instance `<n>` layout: config `$XDG_CONFIG_HOME/aivyx-pa/instances/<n>/aivyx-pa.toml`; data `$XDG_DATA_HOME/aivyx-pa/instances/<n>/`; socket/pid `$XDG_RUNTIME_DIR/aivyx-pa/instances/<n>/daemon.{sock,pid}`; home working dir `~/.aivyx-pa/instances/<n>/`; Access sandbox `~/aivyx-pa-sandbox-<n>`; keyring account suffixed `:<n>`; systemd unit `aivyx-pa-daemon-<n>.service`; launchd label `com.aivyx-pa.daemon.<n>`; Studio port chosen at `instances create` (lowest free ≥ 7844).
- `AIVYX_PA_CONFIG_PATH` and `./aivyx-pa.toml` keep their current precedence.
- No sharing across instances; desktop app and chat channels stay on `default` in v1.
- Contract change goes through `docs/amendments/` (never edit PRODUCT.md/DESIGN.md rules directly; add the amendment reference the way earlier amendments are referenced).
- Zero clippy warnings: `cargo clippy --workspace --all-targets -- -D warnings` and the 1.99 run `CARGO_TARGET_DIR=target/rust199 ~/.cargo/bin/cargo +1.99.0 clippy --workspace --all-targets -- -D warnings` (touch a source file first if it finishes instantly).
- `aivyx-web` changes also need `just check-web` (wasm32 compile).
- Commits: `git commit -s`, ending `Co-Authored-By: Claude <model> <noreply@anthropic.com>`. Keep CHANGELOG `[Unreleased]` free of `Authorization: Bearer`.
- Work on branch `feat/named-instances`.

---

### Task 1: `aivyx-instance` crate

**Files:**
- Create: `crates/aivyx-instance/Cargo.toml`, `crates/aivyx-instance/src/lib.rs`
- Modify: root `Cargo.toml` (workspace member + `[workspace.dependencies]` entry, following the existing pattern for internal crates)

**Interfaces — Produces:**

```rust
pub const ENV_INSTANCE: &str = "AIVYX_PA_INSTANCE";
pub const DEFAULT_INSTANCE: &str = "default";

pub struct InstanceName(String);          // Clone, Debug, PartialEq, Eq
impl InstanceName {
    pub fn parse(s: &str) -> Result<Self, String>;   // validates; "default" allowed
    pub fn default_instance() -> Self;
    pub fn as_str(&self) -> &str;
    pub fn is_default(&self) -> bool;
    pub fn from_env() -> Result<Self, String>;      // AIVYX_PA_INSTANCE; unset/empty → default
}

pub struct BaseDirs {                      // Clone, Debug; all Option<PathBuf>, empty env = None
    pub home: Option<PathBuf>,
    pub xdg_config_home: Option<PathBuf>,
    pub xdg_data_home: Option<PathBuf>,
    pub xdg_runtime_dir: Option<PathBuf>,
}
impl BaseDirs { pub fn from_process() -> Self; }

pub struct InstancePaths { /* name + BaseDirs */ }   // Clone, Debug
impl InstancePaths {
    pub fn new(name: InstanceName, dirs: BaseDirs) -> Self;
    pub fn current() -> Result<Self, String>;        // from_env + from_process
    pub fn name(&self) -> &InstanceName;
    pub fn config_dir(&self) -> Option<PathBuf>;     // <config_home>/aivyx-pa[/instances/<n>]
    pub fn config_file(&self) -> Option<PathBuf>;    // config_dir/aivyx-pa.toml
    pub fn data_dir(&self) -> Option<PathBuf>;       // <data_home>/aivyx-pa[/instances/<n>]
    pub fn store_file(&self) -> Option<PathBuf>;     // data_dir/store.redb
    pub fn runtime_dir(&self) -> Option<PathBuf>;    // $XDG_RUNTIME_DIR/aivyx-pa[/instances/<n>], else data_dir()
    pub fn socket_path(&self) -> Option<PathBuf>;    // runtime_dir/daemon.sock
    pub fn pid_path(&self) -> Option<PathBuf>;       // runtime_dir/daemon.pid
    pub fn home_dir(&self) -> Option<PathBuf>;       // ~/.aivyx-pa[/instances/<n>]
    pub fn tool_process_dir(&self, tool: &str) -> Option<PathBuf>; // home_dir/tool-processes/<tool>
    pub fn sandbox_dir(&self) -> Option<PathBuf>;    // ~/aivyx-pa-sandbox[-<n>]
    pub fn keyring_account(&self, base: &str) -> String;           // base or "base:<n>"
    pub fn systemd_unit(&self) -> String;            // aivyx-pa-daemon[-<n>].service
    pub fn launchd_label(&self) -> String;           // com.aivyx-pa.daemon[.<n>]
}
/// Every instance whose config or data dir exists, default first.
pub fn list_instances(dirs: &BaseDirs) -> Vec<InstanceName>;
```

`<config_home>` = `xdg_config_home` else `home/.config`; `<data_home>` = `xdg_data_home` else `home/.local/share`. Return `None` when the needed base dir is missing (callers keep their current fallback/error behaviour).

- [ ] **Step 1: Pin today's default paths.** Before writing the crate, read each existing derivation and note its exact formula in the report (file:line): config (`aivyx-config/src/lib.rs` `resolve_config_path_from` ~6020), store (`aivyx-config/src/lib.rs` ~6420-6440 and `aivyx-cli/.../init.rs` ~2282), workspace (`aivyx-config/src/lib.rs` ~6400), sandbox (`aivyx-config/src/lib.rs` ~6257), socket/pid (`aivyx-ipc/src/protocol.rs` ~34-55), keyring (`aivyx-channel/src/keyring_store.rs` SERVICE/ACCOUNT), systemd unit + launchd plist (`aivyx-cli/.../daemon_service.rs` ~66 and its plist label), tool-process dirs (`aivyx-auth-cli/src/config_file.rs` ~58, `aivyx-google-oauth/src/storage.rs`). If any formula differs from the interface above (e.g. a different fallback), the crate must reproduce today's formula for `default` — adjust the method doc, not the behaviour.

- [ ] **Step 2: Failing tests.** In `lib.rs` `#[cfg(test)] mod tests`:

```rust
fn dirs(home: Option<&str>, cfg: Option<&str>, data: Option<&str>, run: Option<&str>) -> BaseDirs {
    BaseDirs {
        home: home.map(PathBuf::from),
        xdg_config_home: cfg.map(PathBuf::from),
        xdg_data_home: data.map(PathBuf::from),
        xdg_runtime_dir: run.map(PathBuf::from),
    }
}
fn p(name: &str, d: BaseDirs) -> InstancePaths {
    InstancePaths::new(InstanceName::parse(name).unwrap(), d)
}

#[test]
fn names_are_validated() {
    for ok in ["default", "research", "a", "home-2", "x1"] { assert!(InstanceName::parse(ok).is_ok(), "{ok}"); }
    for bad in ["", "-a", "a-", "Research", "a_b", "a b", "../x", &"a".repeat(33)] {
        assert!(InstanceName::parse(bad).is_err(), "{bad}");
    }
}

#[test]
fn default_matches_todays_paths_with_xdg_set() {
    let d = p("default", dirs(Some("/h"), Some("/c"), Some("/d"), Some("/r")));
    assert_eq!(d.config_file().unwrap(), PathBuf::from("/c/aivyx-pa/aivyx-pa.toml"));
    assert_eq!(d.store_file().unwrap(), PathBuf::from("/d/aivyx-pa/store.redb"));
    assert_eq!(d.socket_path().unwrap(), PathBuf::from("/r/aivyx-pa/daemon.sock"));
    assert_eq!(d.pid_path().unwrap(), PathBuf::from("/r/aivyx-pa/daemon.pid"));
    assert_eq!(d.home_dir().unwrap(), PathBuf::from("/h/.aivyx-pa"));
    assert_eq!(d.tool_process_dir("gmail").unwrap(), PathBuf::from("/h/.aivyx-pa/tool-processes/gmail"));
    assert_eq!(d.sandbox_dir().unwrap(), PathBuf::from("/h/aivyx-pa-sandbox"));
    assert_eq!(d.systemd_unit(), "aivyx-pa-daemon.service");
    assert_eq!(d.launchd_label(), "com.aivyx-pa.daemon");
    assert_eq!(d.keyring_account("passphrase"), "passphrase");
}

#[test]
fn default_matches_todays_paths_with_only_home() {
    let d = p("default", dirs(Some("/h"), None, None, None));
    assert_eq!(d.config_file().unwrap(), PathBuf::from("/h/.config/aivyx-pa/aivyx-pa.toml"));
    assert_eq!(d.store_file().unwrap(), PathBuf::from("/h/.local/share/aivyx-pa/store.redb"));
    assert_eq!(d.socket_path().unwrap(), PathBuf::from("/h/.local/share/aivyx-pa/daemon.sock"));
}

#[test]
fn named_instances_nest_under_instances() {
    let r = p("research", dirs(Some("/h"), Some("/c"), Some("/d"), Some("/r")));
    assert_eq!(r.config_file().unwrap(), PathBuf::from("/c/aivyx-pa/instances/research/aivyx-pa.toml"));
    assert_eq!(r.store_file().unwrap(), PathBuf::from("/d/aivyx-pa/instances/research/store.redb"));
    assert_eq!(r.socket_path().unwrap(), PathBuf::from("/r/aivyx-pa/instances/research/daemon.sock"));
    assert_eq!(r.home_dir().unwrap(), PathBuf::from("/h/.aivyx-pa/instances/research"));
    assert_eq!(r.sandbox_dir().unwrap(), PathBuf::from("/h/aivyx-pa-sandbox-research"));
    assert_eq!(r.systemd_unit(), "aivyx-pa-daemon-research.service");
    assert_eq!(r.launchd_label(), "com.aivyx-pa.daemon.research");
    assert_eq!(r.keyring_account("passphrase"), "passphrase:research");
}

#[test]
fn no_base_dirs_gives_none() {
    let d = p("default", dirs(None, None, None, None));
    assert!(d.config_file().is_none() && d.socket_path().is_none() && d.home_dir().is_none());
}

#[test]
fn list_instances_finds_default_and_named() {
    let tmp = tempfile::tempdir().unwrap();
    let h = tmp.path();
    std::fs::create_dir_all(h.join(".config/aivyx-pa/instances/research")).unwrap();
    std::fs::create_dir_all(h.join(".local/share/aivyx-pa/instances/household")).unwrap();
    std::fs::create_dir_all(h.join(".config/aivyx-pa")).unwrap();
    std::fs::write(h.join(".config/aivyx-pa/aivyx-pa.toml"), "").unwrap();
    let d = BaseDirs { home: Some(h.to_path_buf()), xdg_config_home: None, xdg_data_home: None, xdg_runtime_dir: None };
    let names: Vec<String> = list_instances(&d).iter().map(|n| n.as_str().to_string()).collect();
    assert_eq!(names, vec!["default", "household", "research"]);
}
```

  (Adjust expected default values only if Step 1 found today's formula differs — and say so in the report.) `tempfile` as a dev-dependency (it's already in the workspace).

- [ ] **Step 3: Run — RED.** `cargo test -p aivyx-instance` → fails to compile/assert.

- [ ] **Step 4: Implement** `src/lib.rs` to the interface. `list_instances`: `default` is listed when its config file or store file exists; named ones are the union of dir names under `<config_dir-of-default>/instances/` and `<data_dir-of-default>/instances/` that parse as valid names; sorted, default first.

- [ ] **Step 5: GREEN + clippy (both toolchains) + commit** `aivyx-instance: one resolver for every per-instance path and name`.

---

### Task 2: Global `--instance` flag and tool-process propagation

**Files:**
- Modify: `crates/aivyx-cli/src/bin/aivyx.rs` (`main` ~507, the two `std::env::args()` readers ~511 and ~2796), `crates/aivyx-cli/Cargo.toml`
- Modify: `crates/aivyx-tool/src/bridge.rs` (~277-288, env forwarding) + its tests
- Modify: `crates/aivyx-cli/src/bin/aivyx_modules/help.rs` (document the flag in top-level help)

**Interfaces — Produces:** `fn split_instance_flag(args: &[String]) -> Result<(Option<String>, Vec<String>), String>` in `aivyx.rs`; after `main` runs it, `AIVYX_PA_INSTANCE` is set in the process env iff `--instance` was given.

- [ ] **Step 1: Failing tests** in `aivyx.rs`'s test module:

```rust
#[test]
fn instance_flag_is_stripped_anywhere() {
    let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(split_instance_flag(&a(&["--instance", "research", "daemon", "run"])).unwrap(),
               (Some("research".into()), a(&["daemon", "run"])));
    assert_eq!(split_instance_flag(&a(&["daemon", "status", "--instance=home"])).unwrap(),
               (Some("home".into()), a(&["daemon", "status"])));
    assert_eq!(split_instance_flag(&a(&["daemon", "status"])).unwrap(), (None, a(&["daemon", "status"])));
    assert!(split_instance_flag(&a(&["--instance"])).is_err());            // missing value
    assert!(split_instance_flag(&a(&["--instance", "Bad_Name"])).is_err()); // invalid name
    assert!(split_instance_flag(&a(&["--instance", "a", "--instance", "b"])).is_err()); // twice
}
```

  and in `bridge.rs` tests, mirroring `tool_process_still_sees_the_daemons_http_proxy_config`: a tool process sees `AIVYX_PA_INSTANCE` when the daemon has it set (same `unsafe` set_var pattern and SAFETY comment).

- [ ] **Step 2: RED**, then **Step 3: implement.**
  - `split_instance_flag`: validates with `aivyx_instance::InstanceName::parse`.
  - `main`: first thing, `let raw: Vec<String> = std::env::args().skip(1).collect();` → `split_instance_flag`; on `Some(name)`, `unsafe { std::env::set_var(aivyx_instance::ENV_INSTANCE, &name) }` with a SAFETY comment (single-threaded: before any runtime/thread starts). Store the stripped args in a `static CLI_ARGS: OnceLock<Vec<String>>` and replace both `std::env::args().skip(1).collect()` readers with a `cli_args()` accessor. If no flag, validate any existing `AIVYX_PA_INSTANCE` via `InstanceName::from_env()` and fail early on an invalid value.
  - `bridge.rs`: forward `AIVYX_PA_INSTANCE` after `env_clear()` like `PATH`/`HOME`/proxy vars.
  - Help text: one line for `--instance <name>`.
- [ ] **Step 4: GREEN, clippy both toolchains, commit** `CLI: global --instance flag; tool processes inherit the instance`.

---

### Task 3: Daemon-side paths through `InstancePaths`

**Files (modify):**
- `crates/aivyx-ipc/src/protocol.rs` — `default_socket_path`, `default_pid_path` (~28-55)
- `crates/aivyx-config/src/lib.rs` — `resolve_config_path_from` default branch (~6050), sandbox (~6257), workspace (~6400), store (~6420-6440)
- `crates/aivyx-cli/src/bin/aivyx_modules/init.rs` — store path (~2282)
- `crates/aivyx-channel/src/keyring_store.rs` — keyring account
- `crates/aivyx-cli/src/bin/aivyx_modules/daemon_service.rs` — `SERVICE_UNIT` (~66), the unit file path, the launchd plist label/path (13 sites)
- `crates/aivyx-channel/src/daemon_server.rs`, `crates/aivyx-channel/src/mcp_status.rs`, `crates/aivyx-core/src/sensitive_paths.rs` (the aivyx-pa store/home paths it protects), `crates/aivyx-cli/src/bin/aivyx.rs` (5 sites)

**Interfaces — Consumes:** `aivyx_instance::InstancePaths::current()`.

- [ ] **Step 1: Keep the existing tests as the parity test.** Run `cargo test -p aivyx-ipc -p aivyx-config -p aivyx-channel -p aivyx-cli -p aivyx-core` on the unmodified branch and save the pass counts. Every existing path test must still pass unchanged after migration — that is the proof the default instance didn't move.

- [ ] **Step 2: Add failing named-instance tests** next to each migrated function, e.g. in `protocol.rs`:

```rust
#[test]
fn socket_path_follows_the_instance() {
    let p = aivyx_instance::InstancePaths::new(
        aivyx_instance::InstanceName::parse("research").unwrap(),
        aivyx_instance::BaseDirs { home: Some("/h".into()), xdg_config_home: None, xdg_data_home: None, xdg_runtime_dir: Some("/r".into()) },
    );
    assert_eq!(socket_path_for(&p).unwrap(), std::path::PathBuf::from("/r/aivyx-pa/instances/research/daemon.sock"));
}
```

  Where a function today reads env vars directly, split it into `*_for(&InstancePaths)` (pure, tested) plus the existing public wrapper calling `InstancePaths::current()` — keeping public signatures unchanged so callers don't churn. Same pattern for config (`resolve_config_path_from` gains the instance's default branch), store, workspace, sandbox, keyring account, systemd unit/plist.

- [ ] **Step 3: RED → migrate → GREEN.** Re-run Step 1's command: same pass counts plus the new tests. **Step 4: clippy both toolchains; commit** `Daemon paths, keyring and service names come from InstancePaths`.

---

### Task 4: Tool-process and integration paths

**Files (modify):** `crates/aivyx-auth-cli/src/config_file.rs`, `crates/aivyx-google-oauth/src/storage.rs`, `crates/aivyx-gmail/src/oauth/storage.rs`, `crates/aivyx-gmail/src/auth_cli/config_file.rs`, `crates/aivyx-calendar/src/oauth/storage.rs`, `crates/aivyx-calendar/src/auth_cli/config_file.rs`, `crates/aivyx-drive/src/lib.rs`, `crates/aivyx-drive/src/auth_cli/config_file.rs`, `crates/aivyx-contacts/src/lib.rs`, `crates/aivyx-contacts/src/auth_cli/config_file.rs`, `crates/aivyx-notion/src/lib.rs`, `crates/aivyx-obsidian/src/lib.rs`, `crates/aivyx-n8n/src/lib.rs`, `crates/aivyx-toolkit/src/config.rs`, `crates/aivyx-vision/src/config.rs`, `crates/verticals/aivyx-kitchen-toolkit/src/config.rs`, `crates/aivyx-cli/src/bin/aivyx_modules/{connect.rs,connect_kitchen.rs,pack.rs,init_templates.rs,doctor.rs}`, `crates/aivyx-desktop/src/{main.rs,first_run.rs}` (desktop uses `InstancePaths::new(InstanceName::default_instance(), BaseDirs::from_process())` explicitly — it stays on `default` in v1).

- [ ] **Step 1:** Run each touched crate's tests on the unmodified code and record counts (parity baseline).
- [ ] **Step 2:** For each `.join(".aivyx-pa").join("tool-processes").join(<tool>)`-style site, replace with `InstancePaths::current()?.tool_process_dir(<tool>)` (or the `_for(&InstancePaths)` split where the function is pure/tested). Add one named-instance test per crate asserting the tool's path nests under `instances/<n>/` (pattern as Task 3 Step 2).
- [ ] **Step 3:** GREEN — baseline counts plus new tests. Build the desktop crate explicitly: `cargo build -p aivyx-desktop`. **Step 4:** clippy both toolchains; commit `Tool processes and integrations resolve paths per instance`.

---

### Task 5: Guard test — no aivyx-pa paths outside `aivyx-instance`

**Files:** Create `crates/aivyx-instance/tests/no_stray_paths.rs`

- [ ] **Step 1: Write the test.**

```rust
//! Fails if any non-test source outside aivyx-instance builds an aivyx-pa
//! path or per-instance name itself, so the instance namespace can't be
//! bypassed by a new call site.
use std::path::{Path, PathBuf};

const FORBIDDEN: &[&str] = &[
    ".join(\".aivyx-pa\")",
    ".join(\"aivyx-pa\")",
    "\"aivyx-pa-sandbox\"",
    "\"aivyx-pa-daemon.service\"",
    "\"com.aivyx-pa.daemon",
    ".local/share/aivyx-pa",
    ".config/aivyx-pa",
];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        let name = p.file_name().unwrap().to_string_lossy().to_string();
        if p.is_dir() {
            if ["target", "tests", "aivyx-instance", "verticals-private"].contains(&name.as_str()) { continue; }
            rust_files(&p, out);
        } else if name.ends_with(".rs") && name != "tests.rs" {
            out.push(p);
        }
    }
}

#[test]
fn no_crate_builds_aivyx_pa_paths_itself() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf();
    let mut files = Vec::new();
    rust_files(&crates, &mut files);
    let mut hits = Vec::new();
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap();
        // Only check code before the first `#[cfg(test)]` module.
        let code = text.split("#[cfg(test)]").next().unwrap();
        for (i, line) in code.lines().enumerate() {
            let t = line.trim_start();
            if t.starts_with("//") { continue; }
            for pat in FORBIDDEN {
                if line.contains(pat) { hits.push(format!("{}:{}: {}", f.display(), i + 1, t)); }
            }
        }
    }
    assert!(hits.is_empty(), "build these through aivyx_instance::InstancePaths:\n{}", hits.join("\n"));
}
```

- [ ] **Step 2: Prove it bites.** In a scratch copy (not the repo), add `let x = home.join(".aivyx-pa");` to a non-test file of one crate and confirm the test fails naming that line; then confirm it passes on the real tree. If it flags legitimate non-path uses (e.g. user-facing strings), narrow the pattern rather than allowlisting files; record any adjustment.
- [ ] **Step 3: Commit** `Guard: aivyx-pa paths only come from aivyx-instance`.

---

### Task 6: `aivyx-pa instances list | create | remove`

**Files:** Create `crates/aivyx-cli/src/bin/aivyx_modules/instances.rs`; modify `aivyx.rs` (module declaration + dispatch + help), `crates/aivyx-config/src/lib.rs` (the `./aivyx-pa.toml` notice).

**Interfaces — Consumes:** `aivyx_instance::{list_instances, InstancePaths, InstanceName, BaseDirs}`; the existing `init` entry point (find it in `aivyx_modules/init.rs`); the existing daemon-status probe (`aivyx_channel::daemon_client::daemon_status`).

- [ ] **Step 1: Failing tests** (pure helpers):

```rust
#[test]
fn port_choice_skips_used_and_listening_ports() {
    // used = ports already in other instances' configs; busy = ports something is listening on
    assert_eq!(choose_port(&[7843], &[]), 7844);
    assert_eq!(choose_port(&[7843, 7844], &[7845]), 7846);
}

#[test]
fn remove_refuses_default_and_running() {
    assert!(check_removable(&InstanceName::default_instance(), false).is_err());
    let r = InstanceName::parse("research").unwrap();
    assert!(check_removable(&r, true).is_err());   // running
    assert!(check_removable(&r, false).is_ok());
}

#[test]
fn removal_plan_lists_every_location() {
    // InstancePaths for "research" in a temp HOME with all dirs created:
    // the plan names config dir, data dir, home working dir, sandbox dir,
    // the keyring account and the service unit — and nothing outside instances/
}
```

  (Write the third test fully against a `tempfile` HOME: create the four dirs, call `removal_plan(&paths)`, assert the exact list.)

- [ ] **Step 2: RED → implement.**
  - `list`: table of name / running (socket answers `daemon_status`) / Studio port (read `web_ui_port` from that instance's config) / config path.
  - `create <name>`: reject `default` and existing names; `choose_port(used_ports_from_all_configs, listening_ports_probe)` — probe by trying to bind `127.0.0.1:<port>`; set `AIVYX_PA_INSTANCE` for the child flow and run the normal `init` for that instance, then write `web_ui_port` into its config (via the existing config writer, preserving comments if init's writer does).
  - `remove <name>`: `check_removable` (running = socket answers); print the removal plan; require typing the name exactly; delete dirs, delete the keyring entry, uninstall the service if installed (reuse `daemon_service` uninstall for that instance).
  - Notice: when `AIVYX_PA_INSTANCE` is set explicitly and the resolved config came from `./aivyx-pa.toml`, print one line: `aivyx-pa: using ./aivyx-pa.toml from the current directory for instance <n>` (stderr), with a test of the resolver's return value plus a flag that drives it.
- [ ] **Step 3: GREEN, clippy both, commit** `instances list/create/remove`.

---

### Task 7: Instance name in the daemon status and the Studio

**Files:** `crates/aivyx-ipc/src/protocol.rs` (additive field on the status/hello response — find the struct the daemon returns for status), `crates/aivyx-channel/src/daemon_server.rs` (fill it), `crates/aivyx-web/src/…` (header + `<title>`; find the header component), CLI `daemon status` output.

- [ ] **Step 1: Failing tests:** protocol round-trip with the new field `#[serde(default)] instance: Option<String>` (old JSON without it still parses — test that explicitly); daemon_server test that status reports the configured instance name; for the web crate, a unit test of the pure function that formats the header label (`None`/`"default"` → no label, `"research"` → `"research"`).
- [ ] **Step 2: RED → implement → GREEN.** `just check-web` must pass (wasm32). Rebuild the embedded bundle only at release time (not in this task).
- [ ] **Step 3: clippy both, commit** `Daemon status and the Studio show the instance name`.

---

### Task 8: Two daemons side by side (integration)

**Files:** Create `crates/aivyx-channel/tests/named_instances_e2e.rs` (follow the harness in `crates/aivyx-channel/tests/daemon_roundtrip_e2e.rs`).

- [ ] **Step 1: Write the test:** temp `HOME`, `XDG_*` dirs; start the `default` and `research` daemons in-process (or as subprocesses, whichever the existing harness supports) with distinct `web_ui_port`s and the mock/echo LLM the existing e2e tests use. Assert:
  - two different socket files exist and both answer status, each reporting its own instance name;
  - a turn sent to `research` appends to `research`'s audit log/store and leaves `default`'s untouched (compare sizes or entry counts before/after);
  - stopping `research` leaves `default` answering.
- [ ] **Step 2: Run; fix any real cross-instance leak it finds** (that's the point of this test). **Step 3: commit** `e2e: two named instances run side by side without crosstalk`.

---

### Task 9: Amendment and docs

**Files:** Create `docs/amendments/2026-10-05-named-instances.md` (follow the structure of `docs/amendments/2026-09-26-cloud-escalation.md`); modify `PRODUCT.md` (reference under P1 the way other amendments are referenced — no rule-text edits), `docs/INSTALL.md` (new "Running several agents" section), `examples/aivyx-pa.toml` (`web_ui_port` must differ per instance), `CLAUDE.md` (`aivyx-instance` row in the substrate table + "only place that builds aivyx-pa paths"), `CHANGELOG.md` `[Unreleased]`, `docs/THREAT_MODEL.md` (a sentence: instances are separate stores/keys; same OS user means same OS-level trust — one instance's shell sandbox can't reach another's store only if Access keeps them apart; check and state the real behaviour).

- [ ] **Step 1:** Amendment content per the spec's "Contract" section.
- [ ] **Step 2:** INSTALL.md section: create/list/remove, ports, services (`aivyx-pa-daemon-<n>.service`), what's isolated, what isn't yet (desktop app, chat channels on `default`).
- [ ] **Step 3:** Verify every doc claim against the implemented behaviour (run `aivyx-pa --instance demo instances list` etc. in a temp HOME). **Step 4:** full `cargo test --workspace`, clippy both toolchains, `just check-web`; commit `docs: named instances (amendment, install guide, threat model)`.
