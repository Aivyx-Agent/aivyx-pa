# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

Aivyx PA is a Rust-built, local-first autonomous agent platform (a personal-agent
daemon + multi-frontend architecture). It runs on the operator's own hardware,
talks to LLM providers (Ollama local, or Anthropic/OpenAI under the operator's
own key) directly — there is no Aivyx PA-hosted service in the request path.
Load-bearing properties: capability-based security, HMAC-chained auditability,
and encryption at rest.

`DESIGN.md` (locked technical contract, amended via `docs/amendments/`) and
`PRODUCT.md` (locked product contract) are the source of truth for
architecture and product-shape decisions — read those before proposing a
change that touches either. Architectural changes require a formal amendment
under `docs/amendments/`; don't edit DESIGN.md/PRODUCT.md directly.

## Build, test, lint

```sh
# Full sweep (what CI and the pre-commit hook require — zero warnings, always)
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

# Single crate / single test
cargo test -p aivyx-core
cargo test -p aivyx-core some_test_name

# Python conformance suites (no daemon required — exercise the Channel/Tool SDK contracts)
python3 -m unittest discover examples/python-channel/tests
python3 -m unittest discover examples/python-tool/tests

# Install the pre-commit hook once per clone (runs the clippy sweep before every commit)
./scripts/install-hooks.sh

# Local interactive dev run against a fully local Ollama backend, disposable state under .dev-run/
./scripts/dev-run.sh                     # debug build, interactive session
./scripts/dev-run.sh --release --reset
./scripts/dev-run.sh -- --verify-only    # scripted verification pass (dev-verify.sh)
```

Notes:
- `cargo build` / `cargo test` (no `-p`) only touch `default-members` —
  `aivyx-web` (wasm-only, Dioxus) and `aivyx-desktop` (links a system webview)
  are deliberately excluded and must be built explicitly:
  `cargo build -p aivyx-web --target wasm32-unknown-unknown`, `cargo build -p aivyx-desktop`.
  `just check-web` runs the cheap wasm-compile guard; `just build-web` produces
  the bundle the daemon embeds (needs the `dx` CLI).
- Commercial/private vertical packs live under `crates/verticals-private/*`
  (git-ignored, workspace member via glob — an empty dir builds fine).
- The workspace holds at **zero clippy warnings**; a PR that isn't
  clippy-clean won't merge.
- Contributions require a DCO/CLA sign-off (`git commit -s`) — see
  `CONTRIBUTING.md`. Not relevant for local edits, but relevant if asked to
  prepare a commit for upstream.

## Architecture at a glance

```
operator
   │
   ▼
 channel adapter (CLI / Telegram / Discord / Slack / Web UI / third-party)
   │  Unix-socket IPC (mode 0600)
   ▼
 aivyx-pa daemon
   ├── turn loop (capability check → audit → execute → audit)
   ├── HMAC-chained audit log (offline-verifiable)
   ├── encrypted redb store (Argon2id → HKDF → ChaCha20-Poly1305)
   ├── substrate tools (fs, git, shell, web, skills, memory, …)
   └── tool process bridge (third-party + productivity tools as subprocesses)
```

The daemon is the single long-running process; every frontend (CLI, TUI, web
Studio, Telegram/Discord/Slack, voice) talks to it over the same Unix-socket
IPC rather than embedding agent logic itself. Wire format
(`aivyx-ipc/src/protocol.rs`, kept wasm32-clean so the Dioxus web client
shares the same types): a 4-byte big-endian length prefix + JSON payload,
capped at 16 MiB. There is no protocol-level auth — the socket's `0600` mode
is documented in-code as *being* the auth boundary ("anyone who can read the
socket is the operator").

### Substrate crates (the core, in dependency-ish order)

| Crate | What it owns |
|---|---|
| `aivyx-core` | `Agent` / `Tool` / `ChannelContext` traits, the turn loop, substrate tools (`fs.*`, `git.*`, `shell.exec`, `web.*`, `skills.*`) |
| `aivyx-capability` | `Scope`, `CapabilitySet`, `TrustTier` — the capability taxonomy |
| `aivyx-crypto` | Argon2id → HKDF-SHA256 → ChaCha20-Poly1305 |
| `aivyx-storage` | redb-backed encrypted store, keyed storage domains |
| `aivyx-audit` | HMAC-chained audit log + offline verification |
| `aivyx-config` | TOML + env config loader with source provenance |
| `aivyx-llm` | `LlmProvider` trait + Anthropic / OpenAI / Ollama implementations |
| `aivyx-memory` | `memory.{read,write,forget,gc}` + redb-backed substrate |
| `aivyx-channel` | Daemon server/client, missions, scheduling, reflection, the autonomous loop, memory recall/graph/wiki machinery — the largest crate; most feature "chapters" land here |
| `aivyx-ipc` | wasm-clean wire protocol shared by the daemon and the Dioxus web client |
| `aivyx-mcp` | MCP client adapter (stdio + SSE) |
| `aivyx-tool` | Tool-process IPC bridge + sandbox wrapper for out-of-process tools |
| `aivyx-cost` | LLM token pricing, a priced ledger over the audit chain, `[budget]` caps (Chapter K) |
| `aivyx-federation` | Cross-boundary agent identity/trust — Ed25519 keypairs + signed envelopes (Chapter Passport) |
| `aivyx-pack` | Signed binary pack-bundle format (build/sign/verify/unpack) underlying vertical packs (Chapter Freight) |
| `aivyx-dataread` | Structured-data readers (csv/xlsx/pdf) layered over `fs.read`'s sandbox (Chapter Sheaf) |
| `aivyx-apps` | Opt-in desktop-app control (Linux/X11 via `xdotool`), a sandboxed tool subprocess (Chapter Deckhand) |
| `aivyx-team` / `aivyx-team-types` | Multi-agent "Nonagon" team missions (lead decomposes → delegates → verifies → synthesizes) |
| `aivyx-vertical-sdk` / `crates/verticals/*` | Vertical packs — a `TeamConfig` + tools swapping in a domain crew (kitchen BOH is the worked example) |

Channel adapters: `aivyx-telegram`, `aivyx-discord`, `aivyx-slack`, `aivyx-voice`.
Productivity integrations (each a sandboxed, operator-OAuth tool process):
`aivyx-gmail`, `aivyx-calendar`, `aivyx-drive`, `aivyx-contacts`, `aivyx-notion`,
`aivyx-obsidian`, `aivyx-n8n`, `aivyx-toolkit`, backed by shared OAuth in
`aivyx-google-oauth` / `aivyx-auth-cli`.

Entry points: `crates/aivyx-cli/src/bin/aivyx.rs` (the CLI binary — daemon
control, `init`, `team`, `tui`, `pack`, `access`, `autonomy` subcommands live
under `aivyx_modules/`); `aivyx-web` is the Dioxus/wasm Studio frontend built
separately (see justfile) and embedded into the daemon binary at build time.
The `tui` subcommand is backed by its own `aivyx-tui` crate (ratatui), split
out of `aivyx-channel` in Phase 185 specifically to break a dependency cycle
(`aivyx-cli → {aivyx-channel, aivyx-tui}`, `aivyx-tui → aivyx-channel`) — it
deliberately quarantines the workspace's only ratatui dependency.
`aivyx-desktop` (native webview shell, see build note above) is a third
frontend crate outside `default-members`.

### Governing design decisions (DESIGN.md, locked, amend via `docs/amendments/`)

- **D1 Turn Loop Contract** — every tool call: capability check → audit →
  execute → audit. No tool executes without a scope check; no action is
  unaudited. Implemented in `crates/aivyx-core/src/agent.rs`
  (`ConcreteAgent::turn`); `Agent::turn` returns `TurnOutcome` directly, not
  `Result<TurnOutcome, _>` — errors are part of what happened, not an
  exceptional path. Bounded by `MAX_STEPS_PER_TURN = 32` and a 120s
  wall-clock timeout, plus two loop breakers: a repeated-call breaker (3
  identical consecutive tool calls trips `TurnOutcome::Looping`, Chapter
  Bridle) and a small-cycle breaker for `A,B,A,B,…` patterns.
- **D3 Agent trait + outcome types** — `Verification` / `ToolOutcome` /
  `TurnOutcome` model every possible turn result explicitly.
- **D4 Capability Taxonomy** — `Scope` (`aivyx-capability/src/lib.rs`) is a
  validated string newtype, not an enum: `Scope::parse` rejects any base not
  in the hardcoded `KNOWN_BASES` list (~90 scope strings), so an unknown
  scope fails at parse time, not check time. `Scope::is_granted_by` is the
  *only* authoritative grant check — never compare scope strings ad hoc; it
  includes origin-aware URL-prefix matching specifically to defeat
  `https://example.com.evil.com/`-style prefix-spoofing.
  `CapabilitySet::intersect` is how trust tiers cap agent capabilities
  (`effective = agent_caps.intersect(&tier_ceiling)`).
- **D5 Trust Tier Model** — tiers cap what capabilities can ever be granted,
  independent of what the operator configures.
- **D6 Error Contract** — a fixed `AivyxError` enum surface; use `thiserror`,
  don't invent ad hoc error shapes in new crates.
- **D7 Storage** — redb, encrypted at rest, key-domain separated.

### Operator-facing dials (own the semantics carefully when touching them)

- **Access** (`aivyx-pa access`): sandbox / workspace / home / full — how far the
  agent's filesystem/network reach extends.
- **Autonomy** (`aivyx-pa autonomy`): manual / assisted / supervised / autonomous
  / unleashed — composes the safety gates and arms the autonomous loop. An
  agent can never widen its own reach or autonomy.
- **Memory profile** (`[memory] profile`): `lite` (BM25 + co-occurrence, no
  embeddings) vs `smart` (adds vector recall, knowledge-wiki, typed graph).

### Security-hardening guards worth knowing before touching adjacent code

These are specific mechanisms, not separate crates — grep for the names if
working near file I/O, network calls, or untrusted content:

- **Ward** + **Portcullis** — the same file,
  `aivyx-core/src/sensitive_paths.rs`. One `SensitivePolicy` struct handles
  both directions: Ward blocks *reads* of SSH/cloud creds, `.env`, and
  Aivyx PA's own `.redb` store; Portcullis blocks *writes* to persistence
  targets (`authorized_keys`, shell rc files, systemd/cron, git hooks).
  Default-off, operator-enabled via `[access] allow_sensitive_paths`.
- **Rampart** — `aivyx-core/src/egress.rs`. SSRF guard for `net.*`/`web.*`
  tools; rejects loopback/link-local/private-IP and cloud-metadata hosts by
  literal host/IP. Host-literal-only by design — DNS-rebinding protection is
  handled separately, at connect time, in `web_fetch.rs`.
- **Bulwark** — not a module, a function: `fence_untrusted_output` in
  `aivyx-core/src/agent.rs`, driven per-tool by `Tool::output_is_untrusted()`.
  Wraps flagged tool output in an
  `{"aivyx_untrusted_content_warning": ..., "data": ...}` envelope before it
  re-enters the model's context.
- **Picket** — also `aivyx-core/src/agent.rs`, `check_for_injection`, run
  just before Bulwark's fencing above on the same `output_is_untrusted()`
  tools. An *active* scan (not just structural fencing) via the standalone
  `aivyx-injection-guard` crate's phrase-list `scan_for_injection_markers`;
  a match sets a side-channel reason the turn loop checks *after* recording
  the tool's real (possibly-mutating) outcome, so a hit escalates for
  operator review without misrepresenting an already-executed action as
  still pending. Gated by `[agent] injection_scan_enabled`/
  `injection_scan_exempt` — Bulwark's own fencing is never gated by either.
- **Keyring** — `aivyx-channel/src/keyring_store.rs`. Master passphrase in
  the OS credential store (Secret Service/Keychain/Credential Manager);
  best-effort/desktop-only — a headless systemd-under-linger install has no
  session bus and falls back to a `0600 daemon.env` file.

## Where to look next

- `docs/TOOLS.md` — full tool catalog (capability scope, min trust tier, delivery mechanism).
- `docs/CHANNEL_SDK.md` / `docs/TOOL_SDK.md` — contracts for writing a new channel adapter or tool process (any language; see `examples/python-channel/`, `examples/python-tool/`).
- `docs/THREAT_MODEL.md` — what Aivyx PA defends against, and what it explicitly doesn't.
- `docs/NONAGON.md` — the multi-agent team mission model.
- `docs/DAEMON_IPC.md` — the daemon's wire protocol.
- `docs/ROADMAP.md` / `CHANGELOG.md` — phase-by-phase history (docs are organized by "chapter" codenames — e.g. Ward, Rampart, Nonagon — referenced throughout the codebase and commit history; grep the chapter name if a comment references one you don't recognize).
