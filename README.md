# Aivyx PA

[![CI](https://github.com/Aivyx-Agent/aivyx-pa/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/Aivyx-Agent/aivyx-pa/actions/workflows/ci.yml)
[![License: BUSL-1.1](https://img.shields.io/badge/license-BUSL--1.1-blue.svg)](LICENSE)

> A personal autonomous agent platform that runs on your hardware,
> talks to cloud LLMs under your own API key, and never compromises
> privacy or auditability for the sake of a feature.

Aivyx PA is a Rust-built agent framework whose load-bearing
properties are **capability-based security**, **HMAC-chained
auditability**, and **encryption at rest**. It does not run as a
hosted service. There is no Aivyx-the-company server in your
agent's request path; your API key talks directly to the LLM
provider, your data stays on your hardware, your audit chain is
verifiable offline.

> **Where Aivyx is headed:** [`VISION.md`](VISION.md) — the mission
> (*Build It Right First*), what Aivyx is, the architecture's
> destination (local teams becoming a network of agents), and the
> discipline every chapter is held against.

![The Aivyx PA Studio — the local-first web GUI (Command Center), shown here in the native desktop app](docs/images/desktop-app.png)

## Status (v0.9.4 — source-available, BUSL-1.1, 2026-09-05)

| | |
|---|---|
| Phases shipped | Phase 0 → the complete Studio (Chapters R–Z + Voice), plus post-Studio chapters — Throttle (tool-call rate limits), Contacts (Google People API), Genesis (unified CLI + web agent onboarding), Harbor (Docker appliance), Charter (MIT → BUSL-1.1 relicense), Timbre (permissive Kokoro voice, GPL-free), Atlas (tool audit + `tools.list`), Forge (`web.extract` + `git.commit`), Loom (graph-augmented recall), Codex (knowledge-wiki layer), Lattice (typed knowledge graph + `graph.query`), Lexicon (a controlled relation vocabulary for the graph), Synapse (one `[memory] profile` switch that activates the whole memory stack), Whetstone (skills that sharpen — the agent proposes a refined version of an underperforming skill), Praxis (the agent authors new specialized skills from its own consolidated knowledge), Repertoire (a Studio Skills library showing every skill + its effectiveness), Stencil + Bridle + Emboss (reliable local tool-calling via grammar-constrained decoding on both local engines), Abacus (a pure-compute utilities pack — calc / unit + timezone convert / date math), Sheaf (structured-data readers — CSV / XLSX / PDF over `fs.read`), Conduit (operator-added MCP servers that work — `env` / `headers` / `aivyx-pa mcp status`), Keel (the default system prompt enriched from a one-line stub into a real operating charter), Outfit (default starter skills so a fresh agent works on turn one), Engram (semantic memory that works out of the box — `init` configures embeddings + turns on the memory stack), Tutor (`aivyx-pa skills teach` — operator-initiated skill authoring on a grown agent), Ember (embedding-free "lite" recall — `[memory] profile = lite` gives BM25 lexical + co-occurrence recall with zero setup), Ballast (a per-mission budget that caps + gracefully halts runaway autonomous team missions), Helm (opt-in `[loop] resume_on_boot` so autonomous runs survive a daemon restart), Ledger (the weekly digest is assembled deterministically from real memory — it can no longer confabulate), Deckhand (opt-in `[applications]` — the agent can use the GUI apps open on your own machine), Concord (memory contradiction detection — `aivyx-pa memory conflicts` / `resolve` / `dismiss` flags and resolves contradictory stored facts, within or across topics), a live-dogfood autonomous-loop hardening pass (auto-delegation resolves specialists by role, completion verification judges the real memory artifact, malformed local tool-calls retry instead of failing the turn), and a privacy-first security hardening pass — Ward (a sensitive-path read guard so the agent can't read SSH/cloud creds, `.env`, or Aivyx PA's own store/passphrase), Rampart (a network egress guard blocking SSRF / cloud-metadata / private-network reach, including DNS-rebinding), Bulwark (prompt-injection resistance — fetched/parsed/tool content is fenced as untrusted data), Portcullis (a sensitive-path write guard blocking backdoor/persistence writes to authorized_keys, shell rc files, systemd/cron/autostart), Keyring (the master passphrase in the OS credential store instead of plaintext env/TOML), Gallery (a Studio screen for images the agent generates via a connected ComfyUI MCP server, served through a new authenticated proxy route), and Picket (an active, phrase-list prompt-injection tripwire via the standalone `aivyx-injection-guard` crate, extended to team missions and channel sessions) — with prompt-injection fencing across every untrusted-content ingress (web, files, MCP, tool-process integrations) and egress guards extended to net.dns — and 15 contract amendments |
| Forward-commitment ledger | **Closed** — all 14 PRODUCT.md commitments (P1–P14) and all 7 goal commitments (G1–G7) shipped; subsequent chapters extend the platform within the locked contract |
| Release pipeline | **Active** — on each version tag, cargo-dist builds the CLI (Linux x86_64/aarch64 musl + macOS x86_64/aarch64) and a separate workflow builds the **desktop app** (`.deb` + macOS `.app`); both attach to the GitHub Release. Latest is **`v0.9.4`** (the release-pipeline integrity fixes — the git-dependency visibility fix and the `aivyx-confine` Landlock/musl fixes that had been silently breaking releases — plus Chapter Picket's active prompt-injection tripwire) via the [shell installer](docs/INSTALL.md#shell-installer-recommended), the [desktop app](docs/INSTALL.md#desktop-app), or the [WSL distro](docs/INSTALL.md#windows-wsl2-or-docker) |
| Studio (web GUI) | **Complete** — all 24 screens live (see below); offline, local-first, served on `:7843` |
| Workspace crates | 40 |
| Rust tests | 6,104 passing |
| Python conformance tests | 24 passing |
| Clippy warnings | 0 |
| Capability scope bases | 95 |
| Encrypted storage domains | 26 |

## Five-minute setup

Aivyx PA ships zero hosted dependencies. The quickest path is the
one-line shell installer (a prebuilt binary for your platform):

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/Aivyx-Agent/aivyx-pa/releases/latest/download/aivyx-cli-installer.sh | sh
```

Prefer to compile? The build-from-source steps below work too.

**Want an always-on server instead of a local binary?** `docker
compose up` runs Aivyx PA as a homelab/VPS **appliance** (daemon +
Studio in a container) — a different, deliberately-scoped profile
from the local-first install. See
[`docs/INSTALL.md`](docs/INSTALL.md#docker--the-server-appliance)
and [`docs/DOCKER.md`](docs/DOCKER.md).

**Want a native app instead of the CLI?** Aivyx PA ships **`aivyx-desktop`** —
the Studio in a native window with a system tray, approval notifications, and a
summon hotkey. Grab the `.deb` (Linux) or `.app` (macOS) from the
[latest release](https://github.com/Aivyx-Agent/aivyx-pa/releases/latest), or see
[`docs/INSTALL.md`](docs/INSTALL.md#desktop-app).

**On Windows?** There's no native Windows binary yet (the daemon's
Unix-socket IPC + `0600` secret-at-rest model are Unix-specific).
Run the same Linux binary under **WSL2**, or use the **Docker**
appliance above — both fully supported. See
[`docs/INSTALL.md`](docs/INSTALL.md#windows-wsl2-or-docker).

**Onboarding fast-path:** after `cargo build --release --bin
aivyx-pa`, run `./target/release/aivyx-pa init --template coder` (or
`researcher` / `personal`) to skip the from-scratch config and run
the wizard pre-filled from a starter archetype. See
[`docs/TEMPLATES.md`](docs/TEMPLATES.md) for what each template
contains. The manual path below is shown for reference.

```sh
# 1. Install Ollama and pull a tool-capable model (no API key required)
ollama pull qwen3:8b

# 2. Build aivyx-pa
git clone https://github.com/Aivyx-Agent/aivyx-pa
cd aivyx-pa
cargo build --release --bin aivyx-pa

# 3. Drop a minimal config in your CWD
cat > aivyx-pa.toml <<'EOF'
[agent]
provider = "ollama"
model = "qwen3:8b"

[fs]
root = "/tmp/aivyx-pa-sandbox"

[storage]
path = "/tmp/aivyx-pa-store.redb"

[daemon]
web_ui = true   # enable the localhost-only web UI on :7843

[aivyx_pa]
passphrase = "set-a-real-passphrase"
EOF

# 4. Create the fs sandbox and launch
mkdir -p /tmp/aivyx-pa-sandbox
./target/release/aivyx-pa init    # interactive wizard (or skip if you already wrote aivyx-pa.toml)
./target/release/aivyx-pa         # auto-spawns the daemon, drops into a session
```

Then open `http://127.0.0.1:7843/` in a browser — that's the
**Studio**, the local-first web GUI. It opens on the **Command
Center** dashboard; use the **Chat** tab to talk to the agent,
**Memory** to browse what it's learned, **Documents** to read and
edit files in scope, and **Settings** to adjust access, autonomy, and budgets.
The HMAC audit log (with offline **Verify chain**) lives in the
Studio's own **Audit** screen (sidebar, under System).

To keep it running for days — scheduled routines firing, the loop
available — install it as a background service (no hand-rolled
`systemd`/`launchd`): `aivyx-pa daemon install` (Linux/macOS; survives
logout + reboot). See [`docs/INSTALL.md`](docs/INSTALL.md#running-as-a-service--runs-for-days-chapter-anchor).

**Terminal frontends + the Nonagon (Chapter I/J):**

```sh
./target/release/aivyx-pa tui                 # the ratatui terminal UI
./target/release/aivyx-pa team roster         # the default 9-role Nonagon
./target/release/aivyx-pa team run "research the latest on X and draft a summary"
./target/release/aivyx-pa team roster --config crates/verticals/aivyx-kitchen/assets/kitchen-boh.toml
```

`aivyx-pa team run` hands the mission to a **lead** agent that decomposes it
into a DAG, delegates to least-privileged specialists, verifies, and
synthesizes — every step on the one HMAC chain. A **vertical pack** swaps in
a domain crew via `--config <pack.toml>` (the kitchen Back-of-House Nonagon
is the worked example). See [`docs/NONAGON.md`](docs/NONAGON.md).

For a config that uses Anthropic or OpenAI instead, see
[`examples/aivyx-pa.toml`](examples/aivyx-pa.toml). For a Telegram
adapter, see [`examples/aivyx-semitrusted.toml`](examples/aivyx-semitrusted.toml).
For the full install matrix, see [`docs/INSTALL.md`](docs/INSTALL.md).

## Highlights

- **Runs on your hardware, no API key.** The headline path is local inference
  via [Ollama](https://ollama.com) with a zero-config on-ramp — auto
  context-window sizing, a vetted tool-capable model, and `aivyx-pa doctor` to
  confirm the path end to end. Anthropic / OpenAI are optional, under *your*
  key, talking directly to the provider.
- **Secure by construction.** Capability-based scopes + trust tiers bound
  exactly what the agent can reach; every action lands on an **HMAC-chained,
  offline-verifiable audit log**; storage is encrypted at rest
  (Argon2id → HKDF → ChaCha20-Poly1305).
- **Operator-chosen reach.** *You* pick how far the agent reaches —
  **sandbox** / **workspace** / **home** / **full** — as an audited setting
  (`aivyx-pa access`); irreversible filesystem ops are confirm-first.
- **Operator-chosen autonomy.** *You* pick how autonomous the agent is —
  **manual** / **assisted** / **supervised** / **autonomous** / **unleashed** —
  one dial (`aivyx-pa autonomy`, or the Studio) that composes the safety knobs and
  arms the autonomous loop. Autonomy is a choice you make, never one the agent
  grows into: it cannot widen its own reach or rewrite its own identity.
- **A self-learning identity.** A user-defined **Profile** plus a
  reflection-written **Persona/Soul**, seedable at first launch (by hand or
  *"describe it and the model drafts it"*) and governed through approve / edit
  / reject proposals. **Skills sharpen — and grow — through use**: the agent
  measures how each saved skill performs and proposes a refined version of an
  underperforming one, *and* authors brand-new specialized skills from its own
  consolidated knowledge (the wiki + graph) — all governed the same way.
- **Memory that compounds.** Encrypted, topic-keyed memory with
  **graph-augmented recall** — meaning (vectors), words (BM25), and association
  (a multi-hop co-occurrence walk) fused on one ranking — an opt-in
  **knowledge-wiki layer** that consolidates each topic into a browsable,
  backlinked page, and a **typed knowledge graph** of directed
  `(subject)-[predicate]->(object)` relations the agent extracts from memory
  and can **query** (`graph.query` — "what depends on X?") or fuse back into
  recall. Each layer is derived, opt-in, and off by default — turn the whole
  coherent stack on with one line, **`[memory] profile = "smart"`**.
- **Multi-agent teams (Nonagon).** A lead convenes up to **9** least-privileged
  specialists, decomposes a mission into a DAG, delegates, verifies, and
  synthesizes — durable, resumable, on the one HMAC chain; **vertical packs**
  swap in a domain crew. Build your own team from the GUI (no TOML by hand) via
  the Studio's Teams screen, and watch/steer a running one live via **Mission
  Control** — a LEAD/specialist graph with drill-in and abort/pause/resume.
- **The Studio — a local-first web GUI.** Twenty-four offline, Stitch-styled
  screens served on `:7843`: Command (the dashboard), Chat, Missions, Mission
  Control (a live team mission's LEAD/specialist graph, drill-in, and
  abort/pause/resume controls), Schedules (cron routines), Memory (+
  co-occurrence graph), Wiki (synthesized knowledge pages), Graph (typed
  knowledge graph), Create (guided agent onboarding), Agents, Skills (the
  skill library + effectiveness), Teams, Documents (browse + edit), Audit,
  Sessions, Gallery (ComfyUI images), Notifications, Loop (autonomous-loop
  control), Reminders, MCP (server health), Tools (the tool catalog), Voice,
  Settings, Guide (the in-app end-user guide).
- **One onboarding, two surfaces.** A guided "create your agent" flow (Profile →
  Persona seed → access) drives the **same** drafters and config writers from
  both `aivyx-pa init` (CLI cold-start) and the Studio (live daemon).
- **More frontends, one daemon.** A terminal TUI (`aivyx-pa tui`), the CLI, and
  channel adapters — Telegram, Discord, Slack, voice.
- **Productivity integrations.** Gmail, Calendar, Drive, Contacts, Notion,
  Obsidian, n8n, plus a web/task/health toolkit — each a sandboxed,
  operator-OAuth tool process.
- **Autonomy with brakes.** An autonomous, self-re-arming loop and a
  non-interactive **headless mode** (refuses-and-aborts at gates, never
  auto-approving), with **cost governance** (per-turn dollar pricing and
  `[budget]` caps) and **tool-call rate limits** (`[rate_limit]` per-turn /
  per-tool / sliding-window quotas that alert or deny).
- **Model routing (opt-in).** Off by default. A `[routing]` section lets the
  daemon pick a model per call — by tool/vision needs, context size and task
  tier — across your `[agent]` model and other local models or servers, with
  cooldown + fallback when one fails. See `[routing]` in
  [`examples/aivyx-pa.toml`](examples/aivyx-pa.toml).

The full phase-by-phase arc lives in [`docs/ROADMAP.md`](docs/ROADMAP.md); the
recent-release narrative in [`CHANGELOG.md`](CHANGELOG.md).

### What's next

The v0.6.0 arc (the toolbox release) **widened what the agent can do**:
a pure-compute utilities pack (Chapter Abacus), structured-data readers over
`fs.read` (Chapter Sheaf), and the wiring that turns the whole MCP server
ecosystem into an operator-config story — secrets, headers, and `aivyx-pa mcp
status` (Chapter Conduit). That last one reframes "new integrations": GitHub,
weather, Google Tasks and the rest are now a few lines of `[[mcp_server]]`
config, not in-tree builds. From here:

- **GPU-accelerated local inference.** The in-process mistral.rs path runs on
  CPU today; a CUDA backend is a build-flag away once the upstream
  `cudarc`/`mistralrs` stack supports newer CUDA toolkits — the one tracked
  blocker on a fast local on-ramp.
- **Verticals as packs.** The free PA core stays the substrate; domain crews
  ship as **vertical packs** (a Nonagon team + tools over a `TeamConfig`), with
  the Kitchen BOH pack as the working template.

Direction is set per-arc as the running core demands it — never built ahead of a
real need.

## Release pipeline status

The release pipeline is **active** on the public repo. The latest
release is `v0.9.4` (see the CHANGELOG for what shipped):

- `.github/workflows/release.yml` (cargo-dist-generated) cross-compiles
  the CLI for x86_64/aarch64 Linux musl + x86_64/aarch64 macOS on every
  `v*.*.*` tag push, then publishes a GitHub Release with the
  binaries, checksums, and the one-line shell installer.
- `.github/workflows/desktop-release.yml` builds the **desktop app**
  bundles (a `.deb` on Linux, a `.app` on macOS) and uploads them to that
  same release — it waits for cargo-dist to create the release first, so
  the two never race on creation.
- `.github/workflows/docker-publish.yml` builds + pushes the **server
  appliance image** to GHCR on the same tag.
- `.github/workflows/wsl-release.yml` reuses that appliance image to export
  a **WSL distribution** (`Aivyx-PA.wsl`) — the daemon pre-installed for
  Windows/WSL2 users — and attaches it to the release. This is the
  cheapest real "Aivyx PA on Windows" path: it sidesteps the deferred native
  Windows port (the daemon's Unix-socket IPC just works inside WSL2's Linux
  kernel). See [docs/INSTALL.md](docs/INSTALL.md#windows-wsl2-or-docker).
- `.github/workflows/ci.yml` runs `cargo clippy --workspace --all-targets
  -- -D warnings` and `cargo test --workspace` on every push to
  main and every PR.
- `.github/workflows/quality-gate.yml` is the shared reusable
  workflow both CI and release pipelines call — the release
  short-circuits if the gate fails, and it also confirms every
  workspace git dependency is anonymously cloneable before running
  tests/clippy (the exact check that would have caught the bug behind
  `v0.9.0`'s failed release; see
  [docs/archive/phases/PHASE_192.md](docs/archive/phases/PHASE_192.md)).

Cutting a release is a single step: `git tag vX.Y.Z && git push
origin vX.Y.Z`, and the workflow publishes the binaries + installer.

## Architecture at a glance

```
operator
   │
   ▼
 channel adapter (CLI / Telegram / Web UI / third-party)
   │
   │  Unix-socket IPC (mode 0600)
   ▼
 aivyx-pa daemon
   ├── turn loop (capability check → audit → execute → audit)
   ├── HMAC-chained audit log (offline-verifiable)
   ├── encrypted redb store (Argon2id → HKDF → ChaCha20-Poly1305)
   ├── 15 substrate tools + role-gated infrastructure tools
   └── tool process bridge (third-party + productivity tools as subprocesses)
```

Forty crates in the workspace. The substrate core:

| Crate | What it owns |
|---|---|
| `aivyx-core` | `Agent` / `Tool` traits, turn loop, the 15 substrate tools (incl. `web.extract`, `git.commit`) |
| `aivyx-capability` | `Scope`, `CapabilitySet`, `TrustTier`, the active scope bases |
| `aivyx-crypto` | Argon2id, HKDF-SHA256, ChaCha20-Poly1305 |
| `aivyx-storage` | redb-backed encrypted store, 23 key domains |
| `aivyx-audit` | HMAC-chained audit log, offline verification |
| `aivyx-config` | TOML + env loader with source provenance |
| `aivyx-llm` | `LlmProvider` trait + Anthropic / OpenAI / Ollama impls |
| `aivyx-memory` | `memory.{read,write,forget,gc}` + redb-backed substrate |
| `aivyx-channel` | Daemon, CLI/Local channel, Web UI, mission/schedule/reflection/loop machinery |
| `aivyx-ipc` | wasm-clean wire protocol shared by the daemon and the Dioxus web client |
| `aivyx-mcp` | MCP client adapter (stdio + SSE) |
| `aivyx-tool` | Tool process IPC bridge + sandbox wrapper layer |

Channel adapters — `aivyx-telegram`, `aivyx-discord`,
`aivyx-slack`, `aivyx-voice`.

Productivity integrations (Chapter F/G — each a sandboxed
operator-OAuth tool process) — `aivyx-gmail`, `aivyx-calendar`,
`aivyx-drive`, `aivyx-contacts` (Google People API),
`aivyx-notion`, `aivyx-obsidian`, `aivyx-n8n`,
`aivyx-toolkit` (web.search + task.* + health.check.*), with
`aivyx-google-oauth` + `aivyx-auth-cli` providing the shared
OAuth substrate.

## Where to look next

**For operators** wanting to use Aivyx PA:
- [`docs/ONBOARDING.md`](docs/ONBOARDING.md) — creating your agent:
  the guided Profile → Persona → access flow, shared by `aivyx-pa init`
  and the Studio's **Create** screen.
- [`docs/THREAT_MODEL.md`](docs/THREAT_MODEL.md) — what Aivyx PA
  defends against, what it doesn't. Read this before deploying.
- [`examples/`](examples/) — worked TOML configs for Ollama,
  Anthropic, and the SemiTrusted (Telegram) tier.

**For users & operators** wanting to know what the agent can do:
- [`docs/TOOLS.md`](docs/TOOLS.md) — the **tool catalog**: every tool, its
  capability scope, minimum trust tier, and how it's delivered (the agent can
  also enumerate its own tools at runtime via the `tools.list` tool).

**For contributors** adding channels, tools, or capabilities:
- [`docs/CHANNEL_SDK.md`](docs/CHANNEL_SDK.md) — v0 contract for
  writing a channel adapter (in any language; see
  [`examples/python-channel/`](examples/python-channel/)).
- [`docs/TOOL_SDK.md`](docs/TOOL_SDK.md) — v0 contract for
  writing a tool process (in any language; see
  [`examples/python-tool/`](examples/python-tool/)); for the catalog of tools
  that *already exist*, see [`docs/TOOLS.md`](docs/TOOLS.md).
- [`docs/ADAPTER_PATTERN.md`](docs/ADAPTER_PATTERN.md) — checklist
  for in-tree adapters.
- [`docs/DAEMON_IPC.md`](docs/DAEMON_IPC.md) — wire format for
  the daemon's IPC protocol.

**For architects** wanting to understand the design:
- [`DESIGN.md`](DESIGN.md) — locked technical contract (14 amendments)
- [`PRODUCT.md`](PRODUCT.md) — locked product contract (P1–P14)
- [`docs/ROADMAP.md`](docs/ROADMAP.md) — phase-by-phase narrative
- [`docs/PRODUCT_ROADMAP.md`](docs/PRODUCT_ROADMAP.md) — product-shape milestone narrative
- [`docs/`](docs/) — living reference docs + roadmaps (per-phase journals are archived under [`docs/archive/`](docs/archive/))

## Building & testing

```sh
# Pre-commit hook (recommended once per clone)
./scripts/install-hooks.sh

# Full sweep
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace

# Python conformance suites (no daemon required)
python3 -m unittest discover examples/python-channel/tests
python3 -m unittest discover examples/python-tool/tests
```

The pre-commit hook runs `cargo clippy --workspace --all-targets
-- -D warnings` before every commit — the workspace has held at
zero warnings since the Phase 9 hook was wired.

## Contributing

This is a single-operator personal-agent platform by design
(PRODUCT.md P1 + P6). Contributions are welcome via the usual
channels: file an issue, discuss the shape, send a PR. New
channels, new tools, new provider adapters fit cleanly into the
existing SDK surfaces. Before your first PR, read
[CONTRIBUTING.md](CONTRIBUTING.md) — Aivyx PA is source-available
under BUSL-1.1, so a short [CLA](CLA.md) (accepted via a
`git commit -s` sign-off) is required.

Architectural changes that touch DESIGN.md or PRODUCT.md require
a formal amendment under `docs/amendments/` — fourteen have been
filed across the arc; the process is established. (The two
contracts have otherwise held untouched for many phases — a
tracked stability discipline.)

## License & trademark

The code is **source-available under [BUSL-1.1](LICENSE)** — free
for personal and non-commercial use, with a paid
[commercial license](COMMERCIAL.md) for any business or
production use, **auto-reverting to [MIT](LICENSES/MIT.txt) four
years after each release.** BUSL-1.1 is source-available, *not*
OSI "open source." See [docs/LICENSING.md](docs/LICENSING.md) for
the model and the licensing FAQ. (v0.2.0 and prior remain MIT in
perpetuity.)

The "Aivyx" and "Aivyx PA" names and associated branding are
trademarked — see [TRADEMARK.md](TRADEMARK.md) for the brand usage
rule (BUSL + branded: fork the code for non-commercial use; don't
call the fork "Aivyx" or "Aivyx PA").
