# Routing Visibility Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** every front end can see and control model routing:
- stream events for the routed model and for a cloud-consent request;
- routing status and pin queries;
- `aivyx-coder`'s `/models` and `/model` commands in chat;
- the TUI and REPL show the routed model;
- the Studio gets a status-bar model, a consent card with an Allow button, and a Models screen;
- friendly, channel-aware consent wording.

**Spec:** `docs/superpowers/specs/2026-09-29-routing-visibility-design.md`. Read the section your task names; it holds the exact strings.

## Global Constraints

- **Repo:** `/home/julian/Projects/Rust/aivyx-pa`, branch `feat/routing-visibility` (the controller creates it). Don't push.
- **Commands (default-members):**
  - `cargo build --all-targets`
  - `cargo clippy --all-targets -- -D warnings`
  - `cargo test`
  - `~/.cargo/bin/cargo-deny check bans licenses sources`
  - `python3 -m unittest discover examples/python-channel/tests`
  - `python3 -m unittest discover examples/python-tool/tests`
- **Touching `aivyx-web` or `aivyx-ipc`:** also run `cargo clippy -p aivyx-web --all-targets -- -D warnings`, plus the real wasm check:

  ```sh
  PATH="$HOME/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:$HOME/.cargo/bin:$PATH" \
    cargo check -p aivyx-web --target wasm32-unknown-unknown
  ```

  `aivyx-ipc` must stay wasm32-clean.
- **Code style:**
  - hand-format, and never run `cargo fmt` on a whole crate;
  - no `tracing` (use `eprintln!` as the surrounding code does).
- **Commits:** `git commit -s` with conventional prefixes. End each message with a blank line and then `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.
- **Tests:** TDD; each new behaviour gets a test that fails without the change.
- **User-facing strings:** exactly as the spec gives them. `aivyx-coder`'s wording is at `/home/julian/Projects/Rust/aivyx-coder/crates/aivyx-core/src/routing_commands.rs`.
- **A15 is unchanged:**
  - consent only from Trusted/Kernel or the CLI;
  - taint is never overridden;
  - every decision is audited.
  
  The locked D3 turn-outcome types are not changed.
- **Working rules:**
  - Work alone: never spawn subagents or forks.
  - Real runs: use a SHORT `XDG_RUNTIME_DIR` under `/tmp/claude-1000/`. Stop daemons with `aivyx-pa daemon stop` and confirm with `pgrep -x aivyx-pa`. Never `pkill -f`. Never `rm -rf` your shell's current directory.
  - Lemonade is a user service: `systemctl --user start|stop lemond`, run in a shell whose `XDG_RUNTIME_DIR` is NOT overridden.

---

### Task 1: The consent request travels through the routing guard; neutral friendly text (spec B1, consent plumbing)

**Files:**
- `crates/aivyx-llm/src/escalation.rs` (the `EscalationArming` trait, around line 49);
- `crates/aivyx-llm/src/routed.rs` (`note_consent_requested` call around line 363; message around line 374; the test double around lines 1523–1575);
- `crates/aivyx-channel/src/routing_guard.rs` (`RoutingGuard` impl around line 291; `set_arm_hint`/on_failure hint around line 684);
- the tests.

**Produces:**
- `aivyx_llm::ConsentRequest { model: String, endpoint: String, trigger: String, estimated_tokens: u32 }`;
- `RoutingGuard::take_consent_request(&self, session: &str) -> Option<ConsentRequest>`;
- pure `consent_text(req: &ConsentRequest, can_allow_here: bool) -> String` and `plain_why(trigger: &str) -> &'static str` (in `aivyx-llm` or `aivyx-channel`), used by Task 2 and the in-process REPL.

- [ ] **Step 1: Failing tests.**
  - the guard stores the latest request per session, and `take_*` returns it once;
  - `RoutedProvider`'s error text is the spec's neutral two sentences, with no trigger names and no id@endpoint in backticks;
  - `consent_text` for can-allow and cannot-allow (exact spec strings, thousands separators);
  - `plain_why` for all three triggers;
  - the on_failure hint in per-channel form.
- [ ] **Step 2: Implement.** Update every `EscalationArming` implementor, including `AuditedArming` and the test doubles.
- [ ] **Step 3:** Run the commands above. Commit `feat(routing): carry the cloud-consent request through the guard; friendlier consent text`.

### Task 2: The daemon protocol (spec B1: events, queries, per-channel outcome)

**Files:**
- `crates/aivyx-ipc/src/protocol.rs`: `StreamEventPayload` around line 2896 and its `render_for_cli`; `QueryPayload`/`QueryResponsePayload`, next to `AllowCloudEscalation`;
- `crates/aivyx-channel/src/daemon_server.rs`: the turn path near the `/allow-cloud` interception around line 2318, the `TurnComplete` send, and the query dispatch near `AllowCloudEscalation` around line 4418;
- `crates/aivyx-core/src/tools/routing.rs`: extract the status builder so it's shared;
- `docs/DAEMON_IPC.md`.

**Consumes:** Task 1's `take_consent_request` and `consent_text`.

- [ ] **Step 1: Failing tests.**
  - serde round-trips for `ModelRouted`, `CloudConsentRequested`, `GetRoutingStatus`/`RoutingStatus` and `SetRoutingPin`/`RoutingPinned`;
  - `render_for_cli` output for the two events;
  - the daemon sends `ModelRouted` before `TurnComplete` on a routed turn, and nothing when routing is off;
  - consent: a SemiTrusted channel gets `can_allow_here: false` and the operator-only wording, a Trusted one the /allow-cloud wording, and the outcome text is replaced;
  - `GetRoutingStatus` with routing on and off; `SetRoutingPin` with a bare id, an ambiguous id, an unknown id and `None`.
  - Use the existing daemon test harnesses (`daemon_roundtrip_e2e.rs` / `daemon_server.rs` tests) with a routed provider built from `ScriptedProvider`s.
- [ ] **Step 2: Implement.**
  - Share one status builder between the query and the `routing.status` tool.
  - `SetRoutingPin` resolves ids like `aivyx-coder`'s `resolve`.
- [ ] **Step 3:** Run the commands above, including both Python suites, the web clippy and the wasm check. Commit `feat(ipc): routed-model and cloud-consent events; routing status and pin queries`.

### Task 3: Chat routing commands (spec B2)

**Files:**
- a new `crates/aivyx-channel/src/routing_commands.rs`, which produces the replies (port `aivyx-coder`'s `routing_commands.rs` wording, adapted to `RoutedProvider`/`Router`);
- the daemon's interception next to `/allow-cloud`;
- the in-process REPL's interception, next to Task A1's in-process `/allow-cloud` reply in `crates/aivyx-channel/src/session.rs` or `aivyx-cli`;
- the tests.

- [ ] **Step 1: Failing tests.**
  - each command's reply;
  - the model is never called;
  - a pin changes `router.pinned(session)`, and auto clears it;
  - with routing off, the spec's reply;
  - a whole message only (`/models please` is a normal turn).
- [ ] **Step 2: Implement.** Audit a pin as the existing routing audit tags allow; if there's no fitting tag, don't audit it and say so in the report.
- [ ] **Step 3:** Run the commands above. Commit `feat(routing): /models and /model in aivyx-pa's chat`.

### Task 4: The TUI and REPL show routing (spec B3)

**Files:** `crates/aivyx-tui/src/model.rs` (`Status`, `lines_from_event`, `update`) and `render.rs` (`render_status` around line 583); the REPL's event rendering (`crates/aivyx-channel` session/daemon_session rendering, and `StreamEventPayload::render_for_cli` from Task 2).

- [ ] **Step 1: Failing tests.**
  - the TUI status shows `model <id@endpoint>` after `ModelRouted` and clears it on a new conversation;
  - a consent event gives a highlighted line;
  - the REPL prints `routing → <model> (<reason>)` only when the model changes.
- [ ] **Step 2: Implement.**
- [ ] **Step 3:** Run the commands above. Commit `feat(tui,repl): show the routed model and cloud-consent requests`.

### Task 5: The Studio (spec B4)

**Files:** `crates/aivyx-web/src/main.rs` (the `View` enum around line 107 and `View::ALL`, the slug, the sidebar groups, `StatusBar` around line 1596, Chat event rendering, the poll loop), `crates/aivyx-web/assets/stitch.css` (if needed), an icon, and `crates/aivyx-web/dist/` (rebuilt).

- [ ] **Step 1: Pure pieces first, with tests** where the crate has pure helpers:
  - plain-word capabilities;
  - residency labels;
  - the thousands formatting;
  - the pin options list.
- [ ] **Step 2: UI.**
  - the status-bar model with a tooltip;
  - the consent card with the Allow and Resend buttons, suppressing the duplicate outcome line;
  - the Models screen: polling, the table, the pin selector, and the routing-off explanation.
  - Use the existing Wick & Compass component classes; invent no new colours.
- [ ] **Step 3: Rebuild the bundle.**
  - Run `just build-web`, with `dx` 0.6.3 on PATH through the toolchain path above.
  - Strip the `.br` sidecars: `find crates/aivyx-web/dist -name '*.br' -delete`.
  - Confirm the dist `index.html` references the new asset hashes.
- [ ] **Step 4: Real run**, with headless Chrome through the `aivyx-pa studio` sign-in link:
  - routing on against Lemonade (the controller starts `lemond`); a turn routes; the status bar shows the model; the Models screen lists the candidates and residency;
  - a `tiers = ["chat"]` + anthropic endpoint config with a dummy `ANTHROPIC_API_KEY` shows the consent card. Don't click Allow.
  - Take screenshots, and put their paths in the report.
- [ ] **Step 5: Commit** `feat(studio): routed model in the status bar, cloud-consent card, Models screen` (source and dist together).

### Task 6: Docs (spec B5)

**Files:**
- `docs/guide/12-models-and-routing.md` (new), with its `Page` entry in `crates/aivyx-web/src/guide.rs`;
- `docs/guide/09-screens-reference.md`;
- `CHANGELOG.md`.

- [ ] **Step 1: Write the page:** what routing is, the Models screen, the chat commands, cloud consent (per channel), and how to turn routing on (with a pointer to `examples/aivyx-pa.toml`).
- [ ] **Step 2: Add the Models row** to the screens reference, and the CHANGELOG entries for B1–B4.
- [ ] **Step 3: Rebuild the Studio bundle** if the guide changed (the guide is `include_str!`'d). Run the web clippy. Commit `docs: models and routing guide page`.

## After the tasks

1. A final whole-branch review.
2. A live end-to-end run by the controller.
3. The finishing choices.
