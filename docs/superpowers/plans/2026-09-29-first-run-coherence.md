# First-Run Coherence Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** a new user who follows the guide gets what it describes:
- a real `--help`;
- the Studio running on loopback behind an automatic token and sign-in link;
- the REPL starting the daemon itself;
- an `init` that detects every local runtime and never defaults to cloud.

**Spec:** `docs/superpowers/specs/2026-09-29-first-run-coherence-design.md`. Read the spec section your task names; it holds the exact strings and behaviour.

## Global Constraints

- **Repo:** `/home/julian/Projects/Rust/aivyx-pa`, branch `feat/first-run-coherence` (the controller creates it). Don't push.
- **Commands (default-members):**
  - `cargo build --all-targets`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo test`
  - `~/.cargo/bin/cargo-deny check bans licenses sources`
- **Crates outside the default build:**
  - touching `aivyx-web`: also `cargo clippy -p aivyx-web --all-targets -- -D warnings`;
  - `aivyx-desktop` **cannot build on this machine** (no webkit2gtk). Keep its edits minimal and mechanical, and say so in the report; CI verifies it.
- **Code style:**
  - hand-format, and never run `cargo fmt` on a whole crate;
  - no `tracing` (use `eprintln!` as the surrounding code does).
- **Commits:** `git commit -s` with conventional prefixes. End each message with a blank line and then `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- **Tests:** TDD; each new behaviour gets a test that fails without the change.
- **User-facing strings:** exactly as the spec gives them.
- **Security:**
  - the token is compared in constant time (reuse the existing helper);
  - the token file is written atomically, mode 0600;
  - the token is never logged except where the spec says it is shown.

---

### Task 1: Real help (spec A3)

**Files:** `crates/aivyx-cli/src/bin/aivyx.rs` (a new `aivyx_modules/help.rs` for the table and renderer is fine), the tests.

- [ ] **Step 1: Find every top-level subcommand the dispatcher recognises.**
  - Each is an `if !args.is_empty() && args[0] == "<name>"` block in `aivyx.rs`, plus `daemon`, `--headless` and the REPL flags parsed in `parse_cli_args_from`.
  - Record the list in the table: `name`, `summary` (one line), `usage` (lines).
  - Read each module's own parsing to write accurate usage lines. Don't invent flags.
- [ ] **Step 2: Failing tests.**
  - `aivyx-pa --help`, `-h` and `help` print top-level help and exit 0.
  - `<cmd> --help` and `-h` print that command's entry for at least `routing`, `doctor`, `init`, `daemon` and `tui`.
  - **Drift guard:** every name the dispatcher recognises has a table entry. Derive the list from the same constant the dispatcher uses where possible; otherwise scan `aivyx.rs`'s source with `include_str!` for the `args[0] == "..."` pattern.
  - Every unrecognised-argument error ends with `Run \`aivyx-pa --help\` to see every command.`, and no error contains the stale `Supported:` list.
- [ ] **Step 3: Implement.**
  - Intercept `--help`, `-h` and `help` before any dispatch.
  - Intercept `<cmd> --help` and `<cmd> -h` before that command's parsing.
  - Group top-level help as the spec lists.
- [ ] **Step 4: Verify.** Run the commands above, then `./target/debug/aivyx-pa --help`, and paste the output into the report.
- [ ] **Step 5: Commit** `feat(cli): real --help for aivyx-pa and every subcommand`.

### Task 2: The Studio on by default, with an automatic token and sign-in link (spec A2, except the desktop app and `init`)

**Files:**
- `crates/aivyx-config/src/lib.rs`: the `web_ui_port` resolution around line 8119, with tests;
- a new `crates/aivyx-channel/src/studio_token.rs` (exported from the crate);
- `crates/aivyx-channel/src/web_ui.rs`: the `?token=` sign-in, around `handle_connection` line 465;
- the daemon start in `crates/aivyx-channel/src/daemon_server.rs`, around line 1490: choose the token, bind, print the banner;
- the Ward extra-deny wiring (the same place `effective_kvcache_store_path` joins Ward);
- `crates/aivyx-cli/src/bin/aivyx_modules/doctor.rs`: the Gatehouse section shows the sign-in link;
- `docs/THREAT_MODEL.md` §4.11 and `docs/GATEHOUSE.md`.

**Produces**, for later tasks:
- `aivyx_channel::studio_token::{token_path(storage_path: &Path) -> PathBuf, load_or_create(path: &Path) -> Result<String, String>, read_existing(path: &Path) -> Option<String>, sign_in_url(host: IpAddr, port: u16, token: &str) -> String}`;
- a way for callers to learn the effective Studio port and host from the config.

- [ ] **Step 1: Failing tests.**
  - **Config:**
    - no `[daemon]` gives `web_ui_port == Some(7843)`;
    - `web_ui = false` gives `None`;
    - `web_ui_port = 9000` gives `Some(9000)`;
    - `web_ui = true` gives `Some(7843)`.
    - Update any existing tests that assumed off by default. Say which ones in the report.
  - **`studio_token`:**
    - create gives 43 alphanumeric characters and a 0600 file;
    - a second call returns the same token;
    - a corrupt or empty file gives `Err`;
    - `read_existing` on a missing file gives `None`;
    - `sign_in_url` gives `http://127.0.0.1:7843/?token=<t>`.
  - **`web_ui`:**
    - `GET /?token=<good>` gives `302` with `Location: /` and a `Set-Cookie: aivyx_web_token=<good>; HttpOnly; SameSite=Strict; Path=/`;
    - `GET /?token=<bad>` gives `401`;
    - the existing Basic and Bearer paths still pass.
    - Mirror the existing request-level tests near `web_ui.rs:1426`.
  - **Daemon token choice:**
    - a configured token wins, and no file is created;
    - `web_ui_insecure_no_auth` means no token and no file;
    - otherwise `load_or_create` is used.
  - **Ward:** a read of the token path is denied.
- [ ] **Step 2: Implement.**
  - The token uses the same generator shape as the Harbor entrypoint: 256-bit, alphanumeric.
  - Print `Studio: <sign-in url>` in the `daemon run` banner **only after a successful bind**. Signal success from the web UI task, or bind before spawning it; pick whichever is smaller.
  - `doctor`'s Web UI section prints the sign-in link when a token file exists.
- [ ] **Step 3: Docs.** Threat model §4.11 and the `GATEHOUSE.md` note, as the spec says.
- [ ] **Step 4: Verify.** Run the commands above, then a real run:
  - start `aivyx-pa daemon run` with a short `XDG_RUNTIME_DIR` (for example `/tmp/claude-1000/fr`; long paths exceed the socket length limit) and throwaway storage;
  - `curl -i "http://127.0.0.1:7843/?token=<t>"` shows the 302 and cookie;
  - plain `curl -i http://127.0.0.1:7843/` gives 401;
  - stop the daemon.
- [ ] **Step 5: Commit** `feat(studio): on by default on loopback, behind an automatic token and sign-in link`.

### Task 3: The desktop app uses the token (spec A2, desktop)

**Files:** `crates/aivyx-desktop/src/main.rs` and `gate_watch.rs`.

**Consumes:** `aivyx_channel::studio_token`. If `aivyx-desktop` doesn't already depend on `aivyx-channel`, depend on it only if that's cheap; otherwise copy the 10-line read-and-URL logic, with a comment pointing at the canonical module.

- [ ] **Step 1.** The webview opens `sign_in_url(...)` when a token file exists, and `STUDIO_URL` otherwise.
- [ ] **Step 2.** The gate watcher uses the file's token as Bearer when `AIVYX_PA_STUDIO_TOKEN` is unset. The env var still wins.
- [ ] **Step 3.** This machine can't compile the crate. Keep the diff minimal, re-read it carefully, and state in the report that CI must verify it.
- [ ] **Step 4: Commit** `feat(desktop): sign in to the Studio with the automatic token`.

### Task 4: The REPL starts the daemon, plus the in-process fixes (spec A1)

**Files:**
- `crates/aivyx-cli/src/bin/aivyx.rs`: the REPL connect block around line 10485, `select_passphrase_source` around line 5280, and the in-process session path;
- `crates/aivyx-channel/src/daemon_client.rs`: reuse `spawn_daemon_and_wait` and `AUTO_SPAWN_TIMEOUT`, whose value the TUI uses;
- the in-process `/allow-cloud` handling: where the local REPL submits a line, and `crates/aivyx-channel/src/routing_guard.rs::is_allow_cloud_command`.

**Consumes:** `studio_token::{token_path, read_existing, sign_in_url}` and the effective Studio port and host from Task 2, for the REPL banner line `Studio: <url>` when connected and the token file is readable.

- [ ] **Step 0: Check the ordering first.** Confirm that the store is not opened or locked before the daemon-connect attempt on the REPL path. If it is, restructure so that the start-and-connect happens first, and describe the change in the report.
- [ ] **Step 1: Failing tests.**
  - A pure decision function covering every combination of TTY, `--no-daemon`, daemon running, and passphrase source (non-interactive or interactive). Its results:
    - `Connect`;
    - `SpawnThenConnect`;
    - `InProcess { reason }`.
  - The in-process `/allow-cloud` gets the spec's local reply, and the model is never called (test it through the local session with a fake provider).
  - The in-process startup warning appears when a cloud `[routing.endpoints]` entry exists.
- [ ] **Step 2: Implement** the spawn path, with the spec's exact "Started…" line and the in-process notice. Piped input behaviour stays exactly as it is.
- [ ] **Step 3: Verify.** Run the commands above, then real runs with a short `XDG_RUNTIME_DIR` and throwaway storage:
  - **(a)** passphrase in the env, stdin a TTY: use `script -qc '...' /dev/null` to get a pty, send a message, and check that the "Started…" line and the daemon socket appear;
  - **(b)** piped input with no daemon: in-process, as today;
  - **(c)** `/allow-cloud` in-process gets the local reply.
  - Afterwards run `aivyx-pa daemon stop` and confirm with `pgrep -x aivyx-pa`. **Never use `pkill -f`**: it matches the shell running it.
- [ ] **Step 4: Commit** `feat(cli): the REPL starts the daemon when it can, and says why when it can't`.

### Task 5: `init` covers every provider (spec A4)

**Files:** `crates/aivyx-cli/src/bin/aivyx_modules/init.rs` (the `Provider` enum at line 678, the menu around line 2070, `prompt_choice` around line 315, the config writer), the tests.

- [ ] **Step 1: Failing tests.**
  - **Detection ordering:** use fake servers (the existing TCP-listener test helpers, or a small local listener that answers each probe path).
  - **Nothing detected:** there's no default, an empty answer re-prompts, and the hint line is printed.
  - **Detected runtimes** are listed first with "(detected)".
  - **The written config for each provider kind:** `provider`, `model`, and the base-URL key only when it isn't the default.
  - **mistral.rs** appears only when built with the feature.
  - **The service-install web-UI question is gone,** and the tests that expected it are updated.
- [ ] **Step 2: Implement.**
  - Run the probes concurrently, each with a short timeout (about 500 ms).
  - Model listing uses `aivyx_route::discovery::discover` with the spec's endpoint kinds.
- [ ] **Step 3: Verify.** Run the commands above, then a real run against Lemonade. The controller starts `lemond`; drive `init` through a pty with scripted answers and a throwaway config path, then check the written TOML.
- [ ] **Step 4: Commit** `feat(init): detect every local runtime and never default to a cloud provider`.

### Task 6: Docs (spec A5)

**Files:** `docs/guide/02-getting-started.md`, `docs/guide/10-troubleshooting.md`, `examples/aivyx-pa.toml`, `CHANGELOG.md`, and the Studio's screens-reference line if the sign-in flow changes it.

- [ ] **Step 1.** Rewrite the guide sections as the spec says:
  - all four stale `Aivyx-Agent/aivyx/` links go to `aivyx-pa`;
  - the guide pages are bundled into the Studio with `include_str!`, so run `cargo clippy -p aivyx-web --all-targets -- -D warnings`.
- [ ] **Step 2.** Add CHANGELOG Unreleased entries covering A1–A4, calling out the Studio-on-by-default behaviour change and how to opt out.
- [ ] **Step 3: Commit** `docs: first-run guide matches the new defaults`.

## After the tasks

1. A final whole-branch review.
2. A live end-to-end run by the controller, starting from a fresh throwaway home: `init` (Lemonade detected), then `aivyx-pa` in a pty (daemon starts, Studio link shown), then headless Chrome through the sign-in link (Studio loads), then `aivyx-pa --help`.
3. The finishing choices.
