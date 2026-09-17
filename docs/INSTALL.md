# Installing Aivyx PA

This doc covers the full install matrix. For the abbreviated
"Five-minute setup" path, see the [root README](../README.md).

**This file has two parts.** Everything from here through "Running as a
service" (a few hundred lines) is the actual install-and-first-run path
— read that much and you'll have a working, running daemon. Everything
after that point is a per-channel, per-feature reference (Discord, Slack,
the TUI, memory internals, and 60+ more) — useful once you're running,
not required reading to get there.

Aivyx PA ships a single binary, `aivyx-pa`, plus five optional channel
adapters baked into it (CLI, Telegram, Discord, Slack, Web UI).
There are no hosted dependencies — your binary talks directly to
your LLM provider (Anthropic / OpenAI-compatible / Ollama) and
stores everything locally in an encrypted redb file.

## Current install state

The recommended install path is the [shell installer](#shell-installer-recommended),
which downloads a prebuilt binary for your platform; you can also
[build from source](#build-from-source). Both install the same
single `aivyx-pa` binary. The release pipeline is active — see the
[CHANGELOG](../CHANGELOG.md) for release history.

## Supported targets

The release covers four targets. All Linux builds are musl-static,
so a single Linux binary works on every distro without glibc
version drift.

| Target | Binary | Notes |
|---|---|---|
| Linux x86_64 (musl) | `aivyx-pa` | Debian 8+ / Ubuntu 16+ / Arch / Alpine / RHEL 7+ |
| Linux aarch64 (musl) | `aivyx-pa` | ARM64 servers, Raspberry Pi 4/5 (64-bit OS), Asahi Linux |
| macOS x86_64 | `aivyx-pa` | Intel Macs, macOS 10.13+ |
| macOS aarch64 | `aivyx-pa` | Apple Silicon (M1 / M2 / M3 / M4), macOS 11+ |

Native Windows is **not yet supported** — see [Windows
(WSL2 or Docker)](#windows-wsl2-or-docker) below for the two
supported ways to run Aivyx PA on a Windows machine today.

## Windows (WSL2 or Docker)

There is no native `*-pc-windows-msvc` binary yet, and it is a
deliberate deferral rather than an oversight: two load-bearing
subsystems are Unix-specific.

1. **Daemon IPC is Unix-domain-socket-only.** Every Aivyx PA frontend
   (REPL, TUI, Web Studio, voice) is a thin client that talks to the
   local daemon over a Unix domain socket. A native Windows build
   needs a NamedPipe (or token-authenticated loopback) transport
   before any of it functions.
2. **The secret-at-rest guarantee is Unix file permissions.** OAuth
   tokens, the config file, and the encrypted store rely on `0600`/
   `0700` mode bits and OS-user identity. Windows uses ACLs instead;
   preserving the same posture needs an ACL-equivalent layer, not a
   no-op stub.

Both are real ports, so until a dedicated Windows effort lands, use
one of the two fully-supported paths below — **both run the exact same
Linux binary**, with no loss of functionality.

[WSL2](https://learn.microsoft.com/en-us/windows/wsl/) runs a real
Linux kernel under Windows. Aivyx PA installs and behaves there exactly
as a Linux x86_64 install — the local-first model holds (the agent
reaches your files inside the WSL2 filesystem, accessible from Windows
at `\\wsl$\`). There are two WSL paths: a **pre-built Aivyx PA distro**
(fastest — daemon already installed) or a **manual install** into your
existing Ubuntu.

### Option A1 (fastest) — the pre-built Aivyx PA WSL distribution

Every release attaches a ready-to-run WSL distribution, **`Aivyx-PA.wsl`**,
with the daemon and all tool binaries pre-installed — no in-distro
install step, no Ubuntu to set up first. It runs the exact same Linux
binary as every other target.

```powershell
# In Windows PowerShell — download Aivyx-PA.wsl from the latest release, then:
wsl --install --from-file Aivyx-PA.wsl        # WSL 2.4.4+ (installs a distro named "Aivyx-PA")

# On older WSL, import it manually instead:
#   wsl --import Aivyx-PA C:\WSL\Aivyx-PA Aivyx-PA.wsl
```

Grab `Aivyx-PA.wsl` from the
[latest release](https://github.com/Aivyx-Agent/aivyx-pa/releases/latest)
(or double-click it in Explorer on WSL 2.4.4+). On first launch it
creates your user, then:

```sh
aivyx-pa init      # one-time guided setup
aivyx-pa           # start chatting — auto-starts the local daemon
```

### Option A2 — manual install into your own Ubuntu WSL

```powershell
# In Windows PowerShell (one-time):
wsl --install            # installs WSL2 + a default Ubuntu
# then open the "Ubuntu" terminal and treat it as Linux:
```

```sh
# Inside the WSL2 (Ubuntu) shell — the normal Linux install:
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/Aivyx-Agent/aivyx-pa/releases/latest/download/aivyx-cli-installer.sh \
  | sh
aivyx-pa --version
aivyx-pa init
```

Either way, the Web Studio is reachable from a Windows browser at
`http://127.0.0.1:7843` (WSL2 forwards localhost to Windows
automatically).

### Option B — Docker Desktop (recommended for always-on/server use)

If you have Docker Desktop on Windows, run the
[server appliance](#docker--the-server-appliance) — the daemon + Web
Studio in a container, no WSL distro to manage directly (Docker
Desktop uses its own WSL2 backend). This is the homelab / always-on
profile rather than the local-first desktop one; see the
[Docker section](#docker--the-server-appliance) for the framing and
security posture.

## Desktop app

`aivyx-desktop` is a **native desktop app** — a system-tray assistant that hosts
the Studio in its own window, manages the daemon, and fires OS notifications when
a mission needs your approval. It's *native chrome over the same web Studio* (a
system webview runs the exact same UI), not a separate interface.

![The Aivyx PA desktop app — the Studio running in a native window, daemon connected](images/desktop-app.png)

Features: a tray / menu-bar icon (Open Studio · Restart daemon · Start at login ·
Quit), hide-to-tray on close, native approval-gate notifications, and a global
hotkey (`Ctrl+Shift+A`) to summon the window.

### Runtime dependencies (Linux)

The shell links the system webview + tray libraries, so a Linux machine needs:

| Dependency | Provides | Debian/Ubuntu | Arch |
|---|---|---|---|
| WebKitGTK | the webview | `libwebkit2gtk-4.1-0` | `webkit2gtk-4.1` |
| Ayatana AppIndicator | the tray | `libayatana-appindicator3-1` | `libayatana-appindicator` |
| libxdo | window activation | `libxdo3` | `xdotool` |

macOS uses the system WKWebView (no extra deps). **Windows is not supported yet**
— it rides on the future daemon port (the daemon's Unix-socket IPC).

### Build + run from source

```sh
# Linux: install the build/runtime deps first (dev packages add the headers):
#   Debian/Ubuntu: libwebkit2gtk-4.1-dev libayatana-appindicator3-dev libxdo-dev
#   Arch:          webkit2gtk-4.1 libayatana-appindicator xdotool
cargo build -p aivyx-desktop --release
./target/release/aivyx-desktop
```

On launch it attaches to a running daemon, or spawns one (`aivyx-pa daemon run
--web-ui`) — resolving the `aivyx-pa` binary from `AIVYX_PA_BIN` or `PATH`.

### Packaging (installer)

`cargo-bundle` produces a native package — a `.deb` on Linux, a `.app`/`.dmg` on
macOS — with the icon, a `.desktop` launcher entry, and the declared runtime
deps:

```sh
cargo install cargo-bundle
cargo bundle --release            # from crates/aivyx-desktop/
# -> target/release/bundle/deb/aivyx-desktop_<ver>_amd64.deb  (Linux)
```

The desktop app is **not** part of the musl-static CLI release (it links a system
webview, so it's gnu-linked); it's packaged separately by
[`.github/workflows/desktop-release.yml`](../.github/workflows/desktop-release.yml).

## Build from source

An alternative to the shell installer — Cargo build from a clone
of the repository:

**Prerequisites:**
- Rust toolchain 1.85+ (`rustup` recommended)
- A C linker (`gcc` / `clang` / Xcode CLT)
- For Linux musl builds: the `musl-tools` package (Debian) or
  equivalent

```sh
git clone https://github.com/Aivyx-Agent/aivyx-pa
cd aivyx-pa
cargo build --release --bin aivyx-pa
# binary lands at target/release/aivyx-pa
```

Install into `~/.cargo/bin/` (if `cargo install` is preferred):

```sh
cargo install --path crates/aivyx-cli --bin aivyx-pa
```

The pre-commit hook (`./scripts/install-hooks.sh`) is optional
for end users; it enforces `cargo clippy --workspace --all-targets
-- -D warnings` on every commit and is recommended for
contributors.

## Running Aivyx PA locally for development (Phase 99)

For a development loop — building from a clone and exercising the
real agent on your own machine — two scripts under `scripts/`
wrap the binary against a **fully local Ollama backend** (no API
key, no network egress, no per-run cost).

**Prerequisites:**
- [Ollama](https://ollama.ai) installed and running (`ollama serve`)
- A **tool-capable** model pulled — recommended: `ollama pull qwen3:8b`.
  The agent needs tool-calling, so a small non-tool-caller (e.g.
  `llama3.2:3b`) won't be useful. `aivyx-pa init` will offer to pull the
  recommended model for you.

> **Local-model reliability (Chapter P).** Aivyx PA auto-detects the model's
> native context window and sizes `num_ctx` for you — no manual
> `[ollama] num_ctx` needed (the agent prompt would otherwise starve the
> Ollama default of 4096 down to a single token). Thinking models
> (qwen3) and their tool-calls are handled transparently. If a first turn
> ever comes back empty, run **`aivyx-pa doctor`** — it checks Ollama, the
> model, and a live test reply, and tells you exactly what to fix.

**Interactive session** — `scripts/dev-run.sh` builds `aivyx-pa` and
drops you into a chat REPL:

```sh
./scripts/dev-run.sh                       # default model: llama3.1
./scripts/dev-run.sh --model llama3.2      # pick another pulled model
./scripts/dev-run.sh --reset               # wipe local state first
./scripts/dev-run.sh -- --role coder       # args after -- go to the binary
```

**Scripted verification pass** — `scripts/dev-verify.sh` (also
reachable as `dev-run.sh --verify`) runs a non-interactive battery
over the store, audit chain, daemon lifecycle, and the memory/fs
tool paths, printing a `PASS`/`WARN`/`FAIL` summary:

```sh
./scripts/dev-verify.sh --model llama3.1
```

Substrate checks (store, audit chain, daemon) are deterministic
and a failure exits non-zero. Tool-path probes depend on the
local model actually choosing to call a tool, so a miss there is
reported as `WARN`, not `FAIL`.

All state from both scripts lands under a gitignored `.dev-run/`
directory — a sandbox FS root, an encrypted dev store, and a
throwaway dev passphrase. It is disposable scratch state, never
real data; delete it freely or pass `--reset` for a clean start.

This local-run path is deliberately Ollama-only and leaves no
CI or remote-build footprint: Phase 99 keeps builds local while
repo infrastructure is still being decided.

## Shell installer (recommended)

The cargo-dist-generated installer detects your arch, downloads
the right tarball, verifies its checksum, and drops `aivyx-pa` into
`$CARGO_HOME/bin/` (typically `~/.cargo/bin/`).

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/Aivyx-Agent/aivyx-pa/releases/latest/download/aivyx-cli-installer.sh \
  | sh
aivyx-pa --version
# aivyx-pa x.y.z
```

For a specific version, replace `latest` with the tag (see the
[Releases page](https://github.com/Aivyx-Agent/aivyx-pa/releases) for
available tags):

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/Aivyx-Agent/aivyx-pa/releases/download/vX.Y.Z/aivyx-cli-installer.sh \
  | sh
```

### macOS first launch: Gatekeeper

Phase 61's release will not include code signing or notarization.
macOS quarantines unsigned binaries downloaded from the network.
Two ways past the warning:

**(a) Strip the quarantine attribute** (one-shot, recommended):

```sh
xattr -d com.apple.quarantine "$(command -v aivyx-pa)"
```

**(b) Right-click → Open** the binary once from Finder. macOS
asks for confirmation; after that, future invocations work.

Signing + notarization is on the deferred-distribution list. It
requires an Apple Developer account and a CI-side cert pipeline;
it lands in a follow-up phase once operator pressure surfaces.

## Docker — the server appliance

`docker compose up` runs Aivyx PA as an always-on **server appliance**: the
daemon + the Studio web GUI in one container, no Rust toolchain on your
machine.

**This is a different deployment than the native install above.** The native
binary is **local-first** — the agent reaches *your* real files and devices. In
a container it reaches a **bind-mounted volume** (`./workspace`), the Studio is
exposed deliberately, and there's no voice. It's the homelab / VPS / "give me an
always-on assistant" profile, not a drop-in for the desktop install. See
[`DOCKER.md`](DOCKER.md) for the full framing and security posture, and
[`../deploy/docker/README.md`](../deploy/docker/README.md) for the worked files.

### Quick start (cloud provider)

```sh
# 1. set the store passphrase (a Docker SECRET — not a plain env var)
printf '%s' 'a-strong-passphrase' > deploy/docker/secrets/passphrase

# 2. give it an API key
export ANTHROPIC_API_KEY=sk-ant-...

# 3. bring it up (builds the image the first time; ~minutes)
docker compose up --build

# 4. open the Studio and create your agent
#    → http://localhost:7843  → the "Create" screen (guided onboarding)
```

State (config, encrypted store, audit chain, OAuth tokens) persists in the
`aivyx-pa-data` named volume across `docker compose down` / `up`. The agent's files
live in `./workspace` (mounted at `/work`).

### What you should know

- **Localhost only by default.** The compose publishes to `127.0.0.1:7843`.
  Exposing the Studio off-host needs **both** `[daemon] web_ui_host = "0.0.0.0"`
  *and* `[daemon] web_ui_allowed_origins = ["https://your-host"]` (the daemon
  rejects off-host WebSocket origins otherwise), **plus** auth + TLS in front
  (a reverse proxy like Caddy/Traefik — Aivyx PA doesn't ship one). The daemon
  prints a one-line warning when it binds a non-loopback host. **Also check
  the host's own firewall** (ufw/firewalld/iptables) if a client still can't
  connect after that — a live baptism found a rig's UFW silently dropping the
  Studio's port while every Aivyx PA-side check (bind, token, cookie, `/ws`
  upgrade) was green, with zero feedback pointing at the real cause; see
  `docs/GATEHOUSE.md`'s "Known gap" note.
- **Passphrase posture.** The store passphrase comes from a Docker *secret*
  (bridged to `AIVYX_PA_PASSPHRASE` by the entrypoint), not a plain `environment:`
  value — a deliberate step down from the desktop's interactive prompt, but it
  keeps the passphrase out of `docker inspect`.
- **Local models.** The bundled `ollama` sibling service is optional
  (`docker compose --profile ollama up`) and CPU-only by default; NVIDIA GPU
  passthrough layers in [`../deploy/docker/compose.gpu.yml`](../deploy/docker/compose.gpu.yml).
- **OAuth tools** (Gmail / Calendar / Drive / Contacts / …) are baked into the
  image; completing the one-time consent in a container needs the host-network
  recipe in [`DOCKER.md`](DOCKER.md) §7.
- **Builder.** The `Dockerfile` uses the legacy builder (no `buildx`/BuildKit
  required); a `docker buildx build --mount=type=cache …` is an optional
  rebuild speed-up.

The image is published to a registry in a follow-up (the CI image-publish
phase); until then `docker compose up --build` builds it locally from source.

## Where files land

| File | Default location | Configurable? |
|---|---|---|
| `aivyx-pa` binary | `~/.cargo/bin/aivyx-pa` | Yes — `--install-path` flag on the installer |
| Config | `./aivyx-pa.toml` (CWD) or `~/.config/aivyx-pa/aivyx-pa.toml` | Yes — `--config <path>` on `aivyx-pa`; the wizard writes to CWD by default |
| Encrypted store | per-config (`[storage] path`) | Yes — TOML `[storage] path` |
| Daemon socket | `$XDG_RUNTIME_DIR/aivyx-pa.sock` (Linux) / `$TMPDIR/aivyx-pa.sock` (macOS) | No |
| Daemon PID file | `$XDG_RUNTIME_DIR/aivyx-pa.pid` (Linux) / `$TMPDIR/aivyx-pa.pid` (macOS) | No |
| Web UI port | `127.0.0.1:7843` | Yes — TOML `[daemon] web_ui_port` or `--web-ui-port <N>` |

## First-run checklist

After install:

1. **`aivyx-pa init`** — interactive wizard. Detects Ollama at
   `http://127.0.0.1:11434` and offers it as the default
   provider (no API key required). Otherwise prompts for an
   Anthropic or OpenAI key. Writes `aivyx-pa.toml` to your CWD with
   `0600` permissions.

   **Phase 181 — the guided identity builder.** The wizard's
   identity step is a *guided builder* that shapes who your
   assistant will be — collaborator, coach, confidant — across
   all six P13 Profile fields (name, who *you* are, how it talks,
   what it's for, what it tends to do, and the lines it must
   never cross). It works two ways:

   - **LLM-assisted** (when a provider is reachable + you opt in):
     it asks a few questions about the relationship you want, has
     your chosen model **draft** the full identity, and lets you
     **review and edit** every line — you're always the author of
     record.
   - **Guided manual** (offline, declined, or if the draft call
     fails): the same six fields with good prompts, fully
     **local-first** — no LLM required, ever.

   It ends with a warm **"meet your assistant"** preview you
   confirm, edit, or start over. The Profile is the *declared*
   layer; your assistant's deeper character (the Persona/Soul) is
   *earned* over time through reflection — the builder shapes who
   it starts as, not who it becomes.

   **Phase 104 — verify-before-write.** When the operator picks
   Anthropic or OpenAI, the wizard hits the provider's
   `GET /v1/models` with the supplied key before writing
   `aivyx-pa.toml` and confirms the chosen model is in the
   returned list. A wrong key or typo'd model is caught here
   and re-prompts the implicated field; a broken config never
   lands on disk. After three failed attempts the wizard
   offers a `Write anyway?` escape hatch — verify is a
   guardrail, not a lock. The Ollama path is already
   verified-by-existence through the wizard's `/api/tags`
   listing (an empty list prints a `Try: ollama pull
   llama3.2:3b` starter suggestion).

   **Faster path with a starter template** (Phase 66):
   `aivyx-pa init --list-templates` to discover available starters
   (`coder`, `researcher`, `personal`), then
   `aivyx-pa init --template <name>` to run the wizard with
   pre-filled defaults from the template. The generated
   `aivyx-pa.toml` includes the template's role declarations, MCP
   blocks, and commented-out automation hints. See
   [`docs/TEMPLATES.md`](TEMPLATES.md) for the full template
   reference.

   **Authoring your own tool process** (Phase 103): if you want
   to ship a tool the substrate doesn't already include, run
   `aivyx-pa tool init <path>` to scaffold a runnable Rust
   tool-process starter at `<path>` — `Cargo.toml`, a
   `src/main.rs` with the handshake + invocation loop, a
   `README.md`, and a conformance test. Edit the body of
   `handle_invocation`, build, then point a `[[tool_process]]`
   entry in `aivyx-pa.toml` at the resulting binary. See
   [`docs/TOOL_SDK.md`](TOOL_SDK.md) for the full protocol.

2. **`aivyx-pa`** — auto-spawns the daemon (foreground or
   background depending on flag), drops you into a REPL session,
   and serves the Web UI on `127.0.0.1:7843` if you enabled it.

3. **Visit `http://127.0.0.1:7843/`** for the Web UI: Chat,
   Missions, Audit (with cold-verify), Sessions, Profile,
   Persona tabs.

4. **`aivyx-pa --verify-only`** at any time runs the offline
   HMAC audit-chain verification pass.

5. **`aivyx-pa audit export`** (Phase 105) dumps the audit chain
   as JSONL on stdout. `--from <seq>` and `--limit <N>` slice
   the output via the same `entries_range` reader the Web UI
   Audit tab uses. Each line carries the full `SignedEntry`
   projection (seq, appended_at_ms, prev_mac, mac, event) so
   the export is re-verifiable downstream given a separately-
   supplied genesis seed. Offline-only — requires the
   operator's passphrase, cannot be triggered remotely over
   the daemon socket. See [`docs/AUDIT_EXPORT.md`](AUDIT_EXPORT.md)
   for the full reference + worked `jq` examples.

**Phase 110 — Skills Auto-Creation (Reflection Staging).**
Skills are procedural patterns the agent drafts after complex
turns and the operator approves through the existing
persona-proposal surface (`aivyx-pa persona proposals`). Approved
skills land in the Persona chain as `LearnedSkill` deltas,
render into the agent's system prompt as a `## Learned skills`
section (one bullet per skill: `name: trigger`), and are
callable through `skills.list` (enumerate `{name, trigger}`)
and `skills.invoke` (read the full procedure body on demand).
Three new capability scopes — `skills.propose`, `skills.list`,
`skills.invoke` — all in `CEILING_TRUSTED`. Roles that should
draft + use skills declare these in their `capability_scopes`
alongside `persona.propose`. The agent-side auto-proposer
heuristic (fire reflection-cron-style after complex turns)
deferred to a follow-on phase; today the agent proposes
skills only when explicitly invoked through `reflection.propose`
with a `LearnedSkill` delta in the `persona_deltas` array.

**Phase 109 — Three new substrate tools (Amendment A12).**
P10's substrate tool count grew from ten to thirteen with
`git.status`, `git.diff`, and `net.dns`. The git tools are
read-only inspection of operator-configured repos; they share
a `git.read` capability scope qualified by repo path and shell
out to the system `git` binary (no Rust deps). To enable, add
a `[git]` section to `aivyx-pa.toml` listing the allowed repo
paths:

```toml
[git]
repos = [
    "/home/me/projects/aivyx",
    "/home/me/projects/some-other-repo",
]
```

Each path is canonicalized at startup and must be a directory
containing a `.git/` entry — config errors surface at startup,
not at tool-call time. Without `[git]`, the git tools simply
don't register (zero-config posture; agents see no git tools
in their dispatch surface).

`net.dns` is unconditionally registered (no config required;
uses the existing `net.dns` scope base from Phase 0). Takes a
plain hostname (no scheme, no port, no slash) and returns the
resolved IP addresses.

**OS-level process confinement (`[confine]`).** `shell.exec` and
`git.rs`'s three tools confine every spawned command with Landlock +
seccomp-bpf (`aivyx-confine`), on by default and unconditionally —
there is no config key that disables confinement itself. The only
knob is what happens if Landlock fails to establish a ruleset on a
given machine (e.g. an old kernel, or Landlock disabled at the
kernel-config level):

```toml
[confine]
require_enforcement = true   # default
```

- `true` (default): fail-closed. If Landlock can't be set up, the
  spawn itself fails rather than running the command unconfined.
- `false`: fail-open. If Landlock can't be set up, the command runs
  unconfined rather than being refused — for operators on a kernel
  without Landlock support who still want `shell.exec`/`git.*` usable.
  (`aivyx-confine` emits a `tracing::warn!` when this happens, but this
  workspace installs no tracing subscriber today, so nothing is
  currently surfaced to the operator — don't rely on seeing a log line.)

One other exception worth knowing: a `[git] repos` entry that's a
linked git worktree or submodule runs fully unconfined regardless of
this setting — see `docs/THREAT_MODEL.md` §6 property 7.

When Landlock *is* available (the common case on a modern Linux
kernel), confinement always applies regardless of this setting —
`require_enforcement` only governs the failure path. Non-Linux
builds have no confinement backend at all (the tools still run,
just without the OS-level boundary — same as before this feature
existed). See `docs/THREAT_MODEL.md` §5.6 / property 7 in §6 for the
full mechanism and its current scope.

6. **`aivyx-pa mcp recipes`** (Phase 106) lists Aivyx PA's curated
   catalog of MCP servers worth enabling — `filesystem`,
   `github`, `gitlab`, `sqlite`, `postgres`, `time`, `fetch`,
   `brave-search`, `slack`, `memory`, `puppeteer`,
   `everything`, `aivyx-coder`. Bare form prints the list; `aivyx-pa mcp
   recipes <name>` prints a paste-able `[[mcp_server]]` block
   plus an inline `[mcp_server.sandbox]` block so a
   copy-paste produces a sandboxed config (Phase 55 substrate
   posture). The canonical reference lives in
   [`docs/MCP_RECIPES.md`](MCP_RECIPES.md). Distinct from
   `aivyx-pa mcp-server <name>` (Phase 46) which *runs* a
   bundled server — recipes is the catalog of external ones.

For deployment guidance (threat model, what Aivyx PA defends
against, what it doesn't), read
[`docs/THREAT_MODEL.md`](THREAT_MODEL.md) before exposing the
agent to anything sensitive.

## Running as a service — runs for days (Chapter Anchor)

`aivyx-pa` is most useful left running: the daemon keeps its scheduled
routines firing, its autonomous loop available, and the Studio up.
`aivyx-pa daemon run` ties the daemon to your terminal; **`aivyx-pa daemon
install`** registers it as a real background service that survives
logout and reboot — no hand-rolled `systemd`/`launchd` files.

```sh
# install + start the daemon as a per-user service
aivyx-pa daemon install         # add --web-ui to also serve the Studio
                                # add --no-start to install without starting

# check it (also shown in `aivyx-pa doctor`)
systemctl --user status aivyx-pa-daemon          # Linux
launchctl print gui/$(id -u)/com.aivyx-pa.daemon # macOS
journalctl --user -u aivyx-pa-daemon -f          # Linux logs

# remove it (stops, disables, deletes the unit + its secret env file)
aivyx-pa daemon uninstall
```

- **Linux** — a systemd **user** unit at
  `~/.config/systemd/user/aivyx-pa-daemon.service`, plus
  `loginctl enable-linger` so it runs **without an active login
  session** (the runs-for-days requirement). No root, no `sudo`.
- **macOS** — a launchd `LaunchAgent` at
  `~/Library/LaunchAgents/com.aivyx-pa.daemon.plist` (`RunAtLoad`,
  restart-on-crash).
- **The store passphrase.** The service is unattended, so it can't
  prompt: `daemon install` captures your passphrase (from
  `AIVYX_PA_PASSPHRASE` or a one-time hidden prompt) and stores it for
  the service. On **Linux** it goes in a `0600` env file
  (`~/.config/aivyx-pa/daemon.env`) the unit references — never in the
  unit itself. On **macOS** it rides the plist's
  `EnvironmentVariables` (the plist is written `0600`). Either way the
  secret is owner-only at rest.
- **Working directory.** The unit runs from the directory holding your
  `aivyx-pa.toml` (the cwd at install time, else `$HOME`), so the daemon
  finds your config.
- **Windows / containers** — use the [Docker appliance](#docker--the-server-appliance)
  (always-on by design) or the desktop app's autostart instead.

Re-running `aivyx-pa daemon install` is idempotent — it rewrites the unit
and restarts the service, picking up a new binary path or `--web-ui`.

**You're running.** Open the Studio in a browser — it has its own
built-in guide (Welcome → Getting started → Create your agent) that
picks up exactly where this file leaves off. Everything below this
point is reference material for specific channels and features, not
required reading.

## Running Aivyx PA on Discord (Phase 107)

The Discord adapter mirrors the Telegram pattern: one bot
account, configured per-operator, sees DMs and any guild
channels you've added the bot to. Same `SemiTrusted`-tier
ceiling **when the channel is allowlisted** (see
`channel_filter` below — added 2026-09-16; an unallowlisted
channel is `Untrusted` instead), same `/cancel` mid-turn
handling, same Profile + Persona + mission-gate behavior.

1. **Create a bot account** at
   [https://discord.com/developers/applications](https://discord.com/developers/applications).
   Bot → "Add Bot" → save the **token** (you'll only see it
   once; copy it somewhere safe).
2. **Enable required intents** under Bot → "Privileged Gateway
   Intents":
   - `MESSAGE CONTENT INTENT` — required (the bot needs to
     read message text). Discord gates this behind a developer-
     portal toggle; for a private bot in < 100 servers, just
     flip it on.
3. **Invite the bot** to a server (OAuth2 → URL Generator →
   scopes `bot` + permissions `Send Messages`, `Read Message
   History`). Or just DM the bot from the developer-portal
   account.
4. **Configure aivyx-pa** — set the token via env or TOML:

   ```sh
   export AIVYX_PA_DISCORD_TOKEN='your_bot_token_here'
   aivyx-pa --channel discord
   ```

   …or in `aivyx-pa.toml`:

   ```toml
   [discord]
   token = "your_bot_token_here"
   # application_id = 12345...  # Reserved for slash commands; not used in v1.
   # channel_filter = 123456789012345678  # RECOMMENDED: allowlist this one channel id as SemiTrusted (see the trust-tier note below); default (unset) = every channel is Untrusted
   # team_run_channel = true          # optional: let this channel start team missions via /team run <goal> (default false)
   # team_trigger_rate_limit = 5      # optional: max confirmed /team run starts per rolling hour from this channel (default unlimited)
   # team_command_allowed_senders = [111111111111111111]  # REQUIRED to use any /team command: Discord user ids ("snowflakes"), deny-by-default
   ```

5. **Talk to the bot** — open a DM, type a message, watch the
   agent reply. Each Discord channel id gets its own memory
   partition (the same multi-tenant story Telegram's
   `chat_id`-keyed partitions provide), so DMs and guild
   channels stay isolated.

**Daemon-mode `/approve` / `/reject` text-command routing**
landed at Phase 111 (Adapter Production Wiring) alongside
the Discord daemon-frontend. In-process and daemon-mode
both resolve gates through the bot reply on Phase 111+.
The daemon-frontends also recognize a `/team ...` command
set for Nonagon team-mission control — `/team status [<id>]`,
`/team approve|reject <id> <step>`, `/team pause|resume <id>`,
`/team abort <id>` — routed the same way, across all three
channels (Telegram, Discord, Slack).

**`chat_id`/`channel_filter` now gate `SemiTrusted` itself, not
just `/team` (security fix, 2026-09-16).** Previously, Telegram,
Discord, and Slack all granted every sender `SemiTrusted` tier
unconditionally — `chat_id` only decided whether a Telegram
message was *routed* at all, and Discord/Slack had no filter
concept whatsoever. **As of this fix, an operator who has NOT
configured a sender/channel allowlist for a channel gets `Untrusted`
tier for every sender on that channel** (a near-empty capability
ceiling — see `docs/THREAT_MODEL.md` §2) **instead of the previous
`SemiTrusted`.** If you want your existing Telegram/Discord/Slack
bot to keep its `SemiTrusted` capabilities, set that channel's
`chat_id` (Telegram) or `channel_filter` (Discord/Slack) to the
one chat/channel id you actually use — see the TOML examples below
and in the Discord/Slack sections. This is separate from, and in
addition to, the `team_command_allowed_senders` gate below, which
governs `/team` commands specifically regardless of trust tier.

**`team_command_allowed_senders` — required to use ANY `/team`
command (2026-08-23).** This is a separate, deny-by-default gate from
`team_run_channel` below, and it covers the *whole* `/team` surface —
not just `/team run`, but `/team status`/`approve`/`reject`/`pause`/
`resume`/`abort` too. Each channel's config block takes a
`team_command_allowed_senders` list of the sender ids permitted to
issue `/team` commands from that channel; an empty or unset list (the
default) denies every `/team` command from every sender, regardless
of `team_run_channel`. **This is a breaking change on upgrade**: if
you were already relying on `/team status` or the other control
commands, they will start being denied until you populate this list.
The id type is platform-specific — Telegram and Discord use the
sender's numeric user id, Slack uses the sender's string user id
(`U012ABCDEF`):

```toml
[telegram]
token = "your_bot_token_here"
# chat_id = 123456789         # RECOMMENDED: allowlist this one chat_id as SemiTrusted (see the trust-tier note above); default (unset) = every chat is Untrusted
# team_run_channel = true          # optional: let this channel start team missions via /team run <goal> (default false)
# team_trigger_rate_limit = 5      # optional: max confirmed /team run starts per rolling hour from this channel (default unlimited)
# team_command_allowed_senders = [123456789]  # REQUIRED to use any /team command: Telegram user ids, deny-by-default
```

(See the Discord and Slack TOML examples below for their own
`team_command_allowed_senders` lines.)

**`/team run <goal>`** (Piece C) is the one `/team` command that
*starts* a mission rather than controlling an existing one, so it has
its own, additional off-by-default gate on top of the allowlist
above: the operator must opt a channel in with `team_run_channel =
true` in that channel's `[telegram]`/`[discord]`/`[slack]` config
block (see the TOML examples above/below). Once opted in *and* the
sender is on `team_command_allowed_senders`, the bot confirms before
acting — it replies "Start '<goal>' on the default team? Reply
yes/no." and only starts the mission on a bare "yes" within 5
minutes; "no" (or a stale "yes") cancels instead. An optional
`team_trigger_rate_limit` caps how many confirmed starts one chat can
trigger per rolling hour. See `docs/DAEMON_TEAMS.md` §6 and
`docs/ROUTINES.md` for the full behavior and how it compares to the
CLI and scheduled ways to start a team mission.

## Running Aivyx PA on Slack (Phase 108)

The Slack adapter follows the same shape as Discord and
Telegram: one Slack app, one Socket Mode WebSocket from
aivyx-pa to Slack, the bot sees DMs and channels it's been
invited to. `SemiTrusted` tier **when the channel is
allowlisted** (see `channel_filter` below — added
2026-09-16; an unallowlisted channel is `Untrusted`
instead); per-`(team_id, channel_id)` memory partitioning so
a Slack bot installed in two workspaces partitions cleanly
even when channel ids collide.

1. **Create a Slack app** at
   [https://api.slack.com/apps](https://api.slack.com/apps).
   "From scratch" → name your app → pick the workspace.
2. **Enable Socket Mode** under app settings → Socket Mode
   → toggle on. This will prompt you to create an
   **app-level token** with `connections:write` scope.
   Save the resulting `xapp-...` token.
3. **Configure bot scopes** under OAuth & Permissions →
   add `chat:write`, `channels:history`, `groups:history`,
   `im:history`, `mpim:history`, and `app_mentions:read`.
4. **Subscribe to events** under Event Subscriptions →
   bot events → `message.channels`, `message.im`,
   `message.mpim`, `message.groups`. Subscribe to the events
   relevant for where you want the bot to listen.
5. **Install to workspace** under Install App → save the
   resulting `xoxb-...` bot token.
6. **Invite the bot** to any channel you want it to listen
   in. DMs work out of the box.
7. **Configure aivyx-pa** — set both tokens via env or TOML:

   ```sh
   export AIVYX_PA_SLACK_BOT_TOKEN='xoxb-...'
   export AIVYX_PA_SLACK_APP_TOKEN='xapp-...'
   aivyx-pa --channel slack
   ```

   …or in `aivyx-pa.toml`:

   ```toml
   [slack]
   bot_token = "xoxb-..."
   app_token = "xapp-..."
   # team_id = "T0123456789"  # optional defense-in-depth: also require this workspace to match (does NOT by itself grant SemiTrusted — see channel_filter)
   # channel_filter = "C0123456789"  # RECOMMENDED: allowlist this one channel id as SemiTrusted; default (unset) = every channel is Untrusted
   # team_run_channel = true          # optional: let this channel start team missions via /team run <goal> (default false)
   # team_trigger_rate_limit = 5      # optional: max confirmed /team run starts per rolling hour from this channel (default unlimited)
   # team_command_allowed_senders = ["U012ABCDEF"]  # REQUIRED to use any /team command: Slack user ids, deny-by-default
   ```

8. **Talk to the bot** — open a DM with the bot or mention
   it in an invited channel. Each `(team_id, channel_id)`
   gets its own memory partition (multi-workspace bots
   partition cleanly even on colliding channel ids).

**Production state — Phase 111 closed the Socket Mode
live-wiring carve-out.** The `SlackMorphismTransport` Phase
108 stub is replaced with the production
`SlackClientEventsUserState` callback wiring; all five
adapters (Local + Telegram + Web UI + Discord + Slack) are
production-ready in-process AND daemon-mode after Phase
111. Live-bot smoke testing across the matrix is the
Channel Activation Milestone's job (operator-driven
verification pass, separate from the phase sequence).

## The terminal UI (Phase 185)

`aivyx-pa tui` launches a **terminal application** instead of the
plain line REPL: a scrollable chat pane, a status bar
(role · daemon · working), and an input line — rendered with
`ratatui`. Like every Aivyx PA interface it is a **frontend client**
over the local daemon IPC: it connects to the running daemon
(auto-spawning one if none is listening), exactly as the REPL
does. The daemon — not the TUI — holds the agent, state,
capabilities, and audit, and it survives the TUI exiting.

```sh
aivyx-pa tui                 # default role
aivyx-pa tui --role coder    # pick a role, like the REPL
```

Keybindings:

| Key | Action |
|---|---|
| `Enter` | Send the current line |
| `←` `→` `Home` `End` | Move the input cursor |
| `PgUp` / `PgDn` | Scroll the chat pane (arrows scroll one line) |
| `Esc` / `Ctrl-C` | Cancel the in-flight turn |
| `Ctrl-Q` | Quit (the daemon keeps running) |
| `y` / `n` | Approve / reject an approval gate when one is shown |

**Opt-in by design.** The TUI requires a real terminal, so the
**REPL stays the default** and the only non-TTY / scripting /
piped path — automation that pipes into `aivyx-pa` keeps working
unchanged. Promote the TUI to the default in your own workflow by
aliasing `aivyx-pa` to `aivyx-pa tui`.

**New dependencies.** The TUI is the first feature to add
workspace dependencies in a long time: `ratatui` + `crossterm`,
the terminal-UI stack. They are **quarantined to the `aivyx-tui`
crate** — the substrate crates (`aivyx-core`, `aivyx-capability`,
`aivyx-storage`, `aivyx-crypto`, …) stay dependency-clean, and
only the `aivyx-cli` binary crate links the TUI in. Turn 1's
foundation collects each turn and renders it with a *working…*
status; live token-by-token streaming is the next interface
phase.

## Teaching the agent a skill (Phase 184)

You can **teach** the agent a skill in conversation — no config,
no waiting for the auto-proposer:

> *"Let me show you how I review a PR: first check the diff
> size, then…"*

The agent drafts the skill, **shows it back** (name +
when-to-use + steps), and on your **explicit confirmation** saves
it as a callable skill (it then appears in `skills.list` /
`skills.invoke`). You can also refine one (*"actually, also run
the linter first"*) or drop one you no longer want.

Three Trusted-tier tools back this — `skills.teach`,
`skills.update`, `skills.forget` (all gated by the new
`skills.write` capability). Each **requires** an internal
`confirmed: true` the agent sets only after you approve the
draft, so a skill is never saved silently. A skill is a Persona
(P14) delta on the HMAC-chained persona log — every teach /
update / forget is **audited and reversible** (`aivyx-pa persona`).

This is the *operator-authored* path; the reflection
auto-proposer below is the complementary *agent-detected* path
(it watches for repeated procedures and proposes them for your
approval). Both populate the same `LearnedSkill` layer.

## Persona auto-proposer (Phases 112-115)

The Persona auto-proposer is the optional self-learning
loop that drafts reusable Persona refinements from complex
turns. Phase 112 shipped the substrate (skills-only). Phase
113 made it operator-configurable through TOML. Phase 114
generalized it across the full 11-category PersonaDelta
surface — the agent now self-learns at every Persona axis,
not just at the skill layer.

The Phase 113 `[skills.auto_propose]` section stays as an
alias for `[persona.auto_propose.learned_skill]` —
pre-Phase-114 configs continue to work byte-identically.
New operators use the Phase 114 section:

```toml
[persona.auto_propose]
enabled = true
# Defaults are tuned for "fires on multi-tool work, skips
# chit-chat." Operator only needs `enabled = true` to opt
# into the loop.
# judge_model = "claude-haiku-4-5"
# judge_max_tokens = 800
# fuzzy_match_threshold = 0.80  # LearnedSkill dedup only

[persona.auto_propose.heuristic]
# tool_call_count_min = 3
# distinct_tool_id_min = 2
# duration_ms_min = 5000
# require_gate_resolve = false
# mode = "any"             # "any" or "all"

# Per-category configuration. Defaults: scalars OFF (each set
# replaces the previous value; high-stakes), lists ON (additive).
# Operator opts in to the scalar categories explicitly.

[persona.auto_propose.learned_skill]
# enabled = true
# auto_accept_confidence_threshold = 0.85

[persona.auto_propose.behavioral_preferences]
# enabled = true
# auto_accept_confidence_threshold = 0.85

# ... other list categories: behavioral_constraints,
# learned_context, communication_adaptations, character_traits,
# relationship_milestones, primary_use_cases ...

[persona.auto_propose.assistant_name]
# enabled = false                          # default OFF; high-stakes scalar
# auto_accept_confidence_threshold = 0.99  # require near-certainty

[persona.auto_propose.operator_profile]
# enabled = false
# auto_accept_confidence_threshold = 0.99

[persona.auto_propose.communication_style]
# enabled = false
# auto_accept_confidence_threshold = 0.99
```

After the section is configured, every `TurnOutcome::
Completed` fires a background-task auto-proposer pipeline:
a cheap heuristic gate filters candidates; the LLM judge
picks the right category and drafts the proposal in the
shape that category expects (LearnedSkill, ListAppend, or
ScalarSet); high-confidence non-dup proposals for enabled
categories auto-accept into the Persona chain; below-
threshold verdicts stage as Pending proposals the operator
resolves through `aivyx-pa persona proposals approve`.

**Inspection flags** (Phase 113, generalized in Phase 114):
- `aivyx-pa persona list --auto-only` shows ALL entries the
  auto-proposer wrote across every category (delta_id
  prefix `pd-auto-`).
- `aivyx-pa persona list --manual-only` shows the complement.
- `aivyx-pa audit export --event-type SkillAutoProposal`
  emits only the auto-proposer's audit-event variants for
  forensic walks (`jq`-able JSONL). Phase 114 entries
  carry the `category` field so operators can filter by
  category downstream.

**Self-correction loop (Phase 115).** The auto-proposer
also fires from FAILED turns (not just completed turns)
when `from_failed_turns = true`. The agent observes a
failure and proposes a Persona refinement that would
prevent recurrence — typically a BehavioralConstraint
("never X") or LearnedContext ("remember Y").

```toml
[persona.auto_propose]
enabled = true
from_failed_turns = true        # default false; opt in

[persona.auto_propose.failure_outcomes]
# Default: failed=true, timed_out=true, cancelled=false,
# escalated=false. Tune per failure-type.
# failed = true
# timed_out = true
# cancelled = false
# escalated = false
```

Per-failure-outcome enables let the operator be
conservative on operator-driven cancellations / agent
escalations (where the agent did the right thing under
D1's Tier-2 rules) while still learning from clear
failures. The `--event-type SkillAutoProposal` filter
includes a `source` field that distinguishes
`CompletedTurn` from `FailedTurn { failure_kind }` for
forensic separation.

**Escape hatches:**
- The auto-proposer never blocks a turn — failure-isolated
  background spawn. The user's reply is sent first; the
  pipeline runs after.
- Auto-accepted Persona deltas are revertible through the
  existing Phase 60 surface: `aivyx-pa persona revert
  <delta_id>`. The revert is itself an audit-chained chain
  append, so the forensic trail stays intact.
- Per-category enable flags let the operator opt out of
  specific axes (e.g. keep `learned_skill` on but
  `behavioral_preferences` off) without disabling the
  whole loop.
- Per-failure-outcome flags let the operator scope which
  failure types fire self-correction (Phase 115).
- The whole feature can be disabled by setting top-level
  `enabled = false` (or removing the section). The auto-
  proposer bypass costs zero — no LLM call, no chain
  write.

## Tool/skill relevance hints (Phases 116-117)

The Phase 116 relevance ledger tracks per-tool and
per-skill success/failure outcomes per keyword-extracted
turn pattern. After this phase, the agent's tool/skill
selection — historically pure LLM intuition — can be
augmented by observed historical outcomes the operator
can inspect and tune.

Off by default; enable the section to opt in:

```toml
[tool_relevance]
enabled = true
# Defaults are tuned for cheap-deterministic operation:
# zero LLM cost per turn, bounded prompt-section size.
# max_keywords = 5         # top-K longest non-stopword tokens
# min_outcomes_to_show = 2 # don't show one-data-point rows
# top_k_per_section = 5    # max rows per Tools / Skills subsection
```

After the section is configured, the daemon's post-finalize
hook records each turn's tool outcomes against the user
input's keyword key (Q1a: lowercased, stopword-filtered,
length-ordered top-K tokens, lex-sorted, pipe-joined for
storage). On the next turn with a matching keyword key,
the substrate is ready to render a `## Tools recently used
for similar tasks` section augmenting the LLM's picks.

**Section format** (when the renderer is hooked into the
live prompt path):

```
## Tools recently used for similar tasks

Based on keywords: code, rust

Tools:
- memory.read: 5 successes, 0 failures
- web.fetch: 3 successes, 1 failure

Skills:
- research-topic: 2 invocations (2 successes, 0 failures)
```

**Phase 117 closes both Phase-116-internal deferrals.**
The relevance section now reaches the LLM in live turns
via a `RelevancePromptRefiner` that plugs into the Phase
79 `SystemPromptRefiner` slot on the planner's config; if
Phase 79 adaptive Persona is also armed, both refiners
chain in a single install (Phase 79 inner, Phase 117
outer, composing as `base + adaptive Persona + relevance
section`).

Per-skill tracking lands via a new
`AuditEvent::SkillInvocation` variant that `skills.invoke`
emits alongside its regular `ToolCall` audit entry. The
ToolCall keeps the input-hash (D4 secrets-safety
preserved); the SkillInvocation carries the skill name in
cleartext so Phase 116's `record_turn_outcomes` can
populate per-skill ledger rows. `aivyx-pa audit export
--event-type SkillInvocation` filters to the new variant
for forensic walks.

**Escape hatches:**
- The recording hook never blocks a turn — detached
  `tokio::spawn` after finalize. Audit-walk failures log
  WARN and don't affect the turn.
- Operator can inspect the encrypted ledger via a future
  `aivyx-pa tool-relevance dump` CLI (deferred — until the
  live-prompt path lands, the prompt section IS the
  inspection surface).
- Disabling the section (or setting `enabled = false`)
  bypasses the substrate entirely. The Phase 116 ledger
  domain stays present in storage but no rows are
  written or read.

## Profile/Role refinement (Phase 118)

Phase 118 closes Chapter E with the last named axis:
**outcome-driven Profile/Role refinement**. The agent
observes recurring task shapes that don't fit the current
operator-declared Profile or operator-curated Role config
and proposes refinements as `ProfileHint` or
`RoleDefinitionSuggestion` Persona-chain entries. The
operator reviews each proposal and copies the rendered
draft into `aivyx-pa.toml` if they want to act on it.

**Contract preservation.** Phase 118 honors:
- **P13** (Profile is operator-declared, Phase 56 amendment).
  `ProfileHint` proposals NEVER mutate `aivyx-pa.toml`. Approved
  hints sit in the Persona chain as a record-of-suggestion
  the operator can read at their convenience.
- **P9** (Per-Role full capability declaration, Phase 13).
  `RoleDefinitionSuggestion` proposals NEVER mutate
  `aivyx-pa.toml`. Approved drafts likewise sit in the chain
  for operator copy-paste.

Both categories are **always-staged for operator approval**,
hard-coded at the routing layer regardless of judge
confidence. Operators who don't want auto-proposing these
categories at all can disable them via TOML.

**TOML config sub-sections:**

```toml
[persona.auto_propose.profile_hint]
enabled = true          # default; set false to silence
# auto_accept_confidence_threshold parses but is
# IGNORED at runtime — the always-staged routing
# override forces Staged regardless. Documented here
# for type-shape consistency only.

[persona.auto_propose.role_definition_suggestion]
enabled = true          # default; set false to silence
```

**Operator workflow:**

1. The auto-proposer fires after a turn whose signals
   cross the heuristic gate. Two new Phase 118 heuristic
   signals feed this:
   - `profile_pattern_repeated` — fires when the current
     turn's keyword_key (Phase 116) has accumulated
     ≥ `profile_pattern_recurrence_min` (default 5)
     prior outcomes in the relevance ledger.
   - `role_shape_recurring` — fires when the recent
     session window contains ≥
     `role_shape_scope_denied_min` (default 2)
     `ScopeDenied` audit events.
2. The LLM judge picks `ProfileHint` or
   `RoleDefinitionSuggestion` and drafts the payload
   inline (field + suggested_value + rationale, or full
   role draft + rationale). The judge is instructed to err
   on the side of EXPLICIT rationales because the operator
   reads them.
3. `decide_routing` forces `Staged` regardless of
   confidence. The proposal lands in the persona-proposal
   chain as Pending, and a `SkillAutoProposal` audit event
   records `outcome=Staged` + `category=ProfileHint` (or
   `RoleDefinitionSuggestion`) for forensic visibility.
4. The operator reviews:

   ```sh
   aivyx-pa persona proposals list
   # [Pending] pp-abc...  category=ProfileHint  ...
   #   op = {"kind":"AppendList","value":"..."}

   aivyx-pa persona proposals show pp-abc...
   # Proposal pp-abc...
   # =========================
   #   category    = ProfileHint
   #   proposed op:  { ... raw JSON ... }
   #   rendered draft:
   #     field            = communication_style
   #     suggested_value  = "terse and bullet-formatted"
   #     rationale        =
   #       operator consistently uses bullets in their
   #       own messages and asks for shorter replies
   #
   #   To apply: edit aivyx-pa.toml [profile] and update the
   #   field above. Phase 118 does NOT auto-mutate aivyx-pa.toml.
   ```
5. **Phase 119 — apply the hint with one command.** From
   Phase 119 onward, the operator doesn't have to translate
   the rendered draft into a TOML edit by hand. Approve the
   proposal first, then run the apply-helper:

   ```sh
   aivyx-pa persona proposals approve pp-abc...
   aivyx-pa profile apply-hint pp-abc...
   # Apply `communication_style` = "terse and bullet-formatted" to aivyx-pa.toml?
   # [y/N] (re-run with --yes to skip this prompt)
   y
   #
   # Applied `communication_style` to aivyx-pa.toml.
   # Audit event `ProfileHintApplied` recorded for proposal `pp-abc...`.
   # Restart the daemon for the new value to take effect:
   # `aivyx-pa daemon stop && aivyx-pa`.
   ```

   The apply is atomic (tmp-file + rename); comments and
   other sections in `aivyx-pa.toml` are preserved
   byte-for-byte. List-field hints (e.g.
   `behavioral_preferences`) append idempotently; re-running
   the same apply twice is a no-op.

6. For a `RoleDefinitionSuggestion`, the analogous Phase 119
   command is `aivyx-pa role import`:

   ```sh
   aivyx-pa persona proposals approve pp-role-xyz...
   aivyx-pa role import pp-role-xyz...
   # Import role `research-deploy` inheriting from `research` into aivyx-pa.toml?
   # [y/N] (re-run with --yes to skip this prompt)
   y
   #
   # Imported role `research-deploy` into aivyx-pa.toml.
   # Audit event `RoleDraftImported` recorded for proposal `pp-role-xyz...`.
   # Restart the daemon for the new role to take effect:
   # `aivyx-pa daemon stop && aivyx-pa`.
   ```

   Refuses to overwrite an existing `[roles.<name>]`
   section without `--force`. With `--force`, replaces the
   section entirely.

7. Either way, `aivyx-pa persona proposals approve pp-abc...`
   marks the chain entry as accepted (or `reject` to
   discard). Approved proposals land in
   `EffectivePersona::profile_hints` /
   `EffectivePersona::role_drafts` as a record-of-decision;
   the operator can list them later with the same `list`
   command (status `Applied`).

**Escape hatches:**
- Set `enabled = false` on either sub-section to silence
  proposing entirely. The heuristic still fires and the
  judge still runs for OTHER categories; only the Phase
  118 categories drop with `DroppedCategoryDisabled`.
- Set both `enabled = false` AND disable the Phase 116
  relevance ledger to suppress the `profile_pattern_repeated`
  signal source. The `role_shape_recurring` signal sources
  directly from the audit chain and stays active.
- Phase 118 never auto-mutates `aivyx-pa.toml`. The operator
  is always in the loop. If a `ProfileHint` or `RoleDraft`
  approval shows up that the operator doesn't want to act
  on, the approval is a no-op against the live config —
  the entry sits in the chain as "noted but not applied"
  state.
- **Phase 119 — apply commands also never auto-mutate
  without operator action.** `aivyx-pa profile apply-hint` and
  `aivyx-pa role import` are explicit operator gestures. They
  confirm with `[y/N]` by default; pass `--yes` to skip
  the prompt in scripted workflows. The apply step records
  a `ProfileHintApplied` / `RoleDraftImported` audit event
  via daemon IPC; if the audit-record step fails after the
  file mutation lands, the CLI surfaces a soft warning and
  the operator can re-run the command to re-record (the
  TOML edit is idempotent).

## Inspecting the tool-relevance ledger (Phase 119)

The Phase 116 `KeyDomain::ToolRelevanceLedger` is AEAD-
encrypted at rest, so before Phase 119 operators had no
read path into the per-keyword-key outcome rows the self-
learning loop had accumulated. Phase 119 closes that
deferred surface:

```sh
aivyx-pa tool-relevance dump
# keyword_key      surface  identifier         success  failure  last_seen_unix_ms
# ---------------  -------  -----------------  -------  -------  -----------------
# research+deploy  skill    summarize-pdf            3        0      1715000040000
# research+deploy  tool     fs.read                  7        1      1715000060000
# research+deploy  tool     web.fetch                2        0      1715000050000
# (ledger empty — no per-keyword-key outcomes recorded yet)  ← if empty
```

Rows are sorted ascending by `(keyword_key, surface,
identifier)` for stable terminal scanning. Column widths
size to the longest value — keyword keys never truncate.
Restrict the dump to a single keyword key with
`--keyword-key`:

```sh
aivyx-pa tool-relevance dump --keyword-key research+deploy
```

The dump talks to the running daemon over IPC; it requires
the daemon to be up. With `[tool_relevance] enabled =
false` in `aivyx-pa.toml`, the dump errors with
`no_tool_relevance_ledger` rather than returning an empty
table (the substrate is bypassed entirely, not silently
empty).

## Local-LLM tool-call recovery (Phase 120)

Local models like qwen3.6:27b and gemma4:31b occasionally
hallucinate tool names — emitting `fs_read` when the
registered tool is `fs.read`, or `web_fetch` instead of
`web.fetch`. Cloud models (Anthropic) rarely do this;
local models with smaller training corpora are the
dominant source.

Before Phase 120, hallucinated names caused turns to fail
ungracefully: the planner couldn't dispatch a non-existent
tool, and the agent terminated with
`TurnOutcome::Failed`. Phase 120 closes that failure mode
with belt-and-suspenders validation at the LLM-provider
boundary AND fuzzy-match recovery at the planner.

**The fix is substrate-shaped, not model-shaped.** We
can't make local models stop hallucinating; we catch the
hallucination at the boundary and give the model a
structured response that lets it recover.

### What happens when the model hallucinates

1. The OpenAI/Ollama or Anthropic provider classifies
   every emitted tool name against the canonical tool set
   the request advertised. Unknown names get flagged as
   `NameResolution::Unknown { original }` before the
   stream terminates.

2. The planner's recovery path computes Phase 112's
   `title_similarity` (tokenized Jaccard) against every
   registered tool. The algorithm normalizes separators
   and case, so `fs_read` and `fs.read` both tokenize to
   `{fs, read}` — Jaccard 1.0.

3. **Above the operator-configured threshold (default
   0.80)**, the planner dispatches the matched tool and
   records the verbatim original name in the audit chain
   via `AuditEvent::ToolCall.auto_corrected_from`.
   Operator forensics see the auto-correction explicitly:

   ```sh
   aivyx-pa audit export --event-type ToolCall | \
     jq 'select(.auto_corrected_from)'
   # {
   #   "kind": "ToolCall",
   #   "tool_id": "fs.read",
   #   "auto_corrected_from": "fs_read",
   #   ...
   # }
   ```

   Rates of `Some(_)` entries across a window of audit
   events are a useful diagnostic when picking between
   local models — qwen3.6:27b with N auto-corrections per
   100 ToolCalls vs gemma4:31b with M tells you which
   model has the cleaner tool-call protocol.

4. **Below threshold**, the planner emits a synthetic
   `unknown_tool` tool-result back to the model with a
   structured "did you mean?" body:

   ```json
   {
     "error": "unknown_tool",
     "message": "tool 'do_the_thing' is not registered. Did you mean 'fs.read', 'fs.write', 'memory.read'?",
     "did_you_mean": ["fs.read", "fs.write", "memory.read"]
   }
   ```

   The top-3 suggestions are ranked by `title_similarity`
   descending. The model can parse the `did_you_mean`
   array on its next turn and retry with the right name.

### Operator config knob

The fuzzy threshold is operator-configurable via
`aivyx-pa.toml`:

```toml
[providers]
tool_name_auto_correct_threshold = 0.80   # default
```

Float in `[0.0, 1.0]` — out-of-range values reject at
TOML-parse time with `ConfigError::Invalid`. The threshold
is operator-conservative-leaning at the default:

- `0.80` (default) — matches Phase 112's fuzzy default.
  Catches the `fs_read` / `web_fetch` / `git_status`
  separator-hallucination patterns the project memory
  documents qwen3.6 emitting.
- `1.0` — exact match only. Disables fuzzy recovery
  entirely; any Unknown name falls through to the
  synthetic error path. Operator-paranoid posture: never
  trust the planner to pick the model's intent.
- `0.65–0.75` — more aggressive recovery. Useful for
  smaller local models with messier tool-call
  protocols. Watch the audit-export
  `auto_corrected_from` count to confirm the lowered
  threshold isn't mis-dispatching unrelated tools.
- `0.0` — every match clears (auto-corrects to the
  first registered tool). Not useful in practice; pin
  the inclusive-bound semantics rather than enabling
  garbage-out behavior.

### What this does NOT change

- **Cloud-model behavior is unchanged in practice.**
  Anthropic and OpenAI cloud models rarely emit
  hallucinated tool names; the provider-side validation
  fires but classifies every call as `Known`, the
  planner dispatches directly, and `auto_corrected_from`
  stays `None`. Operators paying for cloud inference see
  no behavioral difference.
- **The audit chain stays wire-compatible.**
  `auto_corrected_from: None` serializes WITHOUT the
  field (`#[serde(default, skip_serializing_if =
  "Option::is_none")]`) — pre-Phase-120 chain entries
  decode unchanged, and a Phase 120 read of a Phase 119
  ToolCall produces byte-identical canonical JSON. HMAC-
  chain integrity preserved.
- **No new tool added to the P10 substrate.** Phase 120
  ships substrate that fixes the existing tool-dispatch
  path; the 13-tool substrate cap stays at thirteen.

### Escape hatches

- Set `tool_name_auto_correct_threshold = 1.0` in
  `aivyx-pa.toml` to disable fuzzy recovery. The provider
  still classifies, but the planner never auto-corrects;
  every Unknown name produces the `unknown_tool` error
  path immediately.
- The provider-side validation always runs; there is no
  knob to disable it. Cheap pure-function check; no LLM
  cost.
- Audit forensics: `aivyx-pa audit export --event-type
  ToolCall | jq 'select(.auto_corrected_from)' | jq -s
  length` counts auto-corrections in the chain. Use this
  to evaluate whether your local-model choice is
  producing too much noise (and consider raising the
  threshold or switching to a model with a cleaner
  protocol).

## Native Ollama provider (Phase 121)

Phase 25 added OpenAI-compatible LLM support; Phase 34
brought Ollama to first-class status by routing
`provider = "ollama"` through that same OpenAI-compat
path. The translation worked but lost fidelity on
Ollama-specific options (`num_ctx`, `num_predict`,
`mirostat`) and on Ollama's native JSONL streaming
protocol.

**Phase 121 ships a dedicated `OllamaProvider`** that talks
Ollama's `/api/chat` natively. After Phase 121, `provider
= "ollama"` in `aivyx-pa.toml` routes to the native adapter
**transparently** — operators using Ollama get native
benefits without changing their config.

### What changed for `provider = "ollama"`

- **Endpoint**: `/api/chat` (was `/v1/chat/completions`
  through the OpenAI-compat path).
- **Streaming**: native JSONL (newline-delimited JSON
  objects) instead of SSE `data:` framing.
- **Tool calls**: arrive complete in the final `done:
  true` chunk (Ollama's actual protocol; the OpenAI-compat
  path was reassembling delta-streamed arguments that
  Ollama never sent that way).
- **Usage**: `prompt_eval_count` → input tokens,
  `eval_count` → output tokens, on the terminal chunk.
- **Tool-call arguments**: passed as JSON **objects** on
  the wire (Ollama's native format), not JSON-encoded
  strings.

**Behavior NOT changed:**
- Existing `aivyx-pa.toml` files with `provider = "ollama"`
  work unchanged. The base-URL handling, model-name
  selection, and channel adapters all continue to work.
- Phase 120's tool-call recovery substrate flows uniformly
  through the native adapter: `NameResolution::Unknown`
  classification fires on hallucinated names (e.g.
  qwen3.6:27b emitting `fs_read` when the registered
  tool is `fs.read`), and the planner's fuzzy-match
  recovery dispatches as before.
- The OpenAI-compat path stays for explicit `provider =
  "openai"` (cloud OpenAI or non-Ollama OpenAI-compat
  services).

### Configuring Ollama-specific options

Phase 121 introduces a new `[ollama]` section in
`aivyx-pa.toml` for the operator-relevant subset of Ollama's
modelfile options. All fields are optional; unset fields
fall through to Ollama's per-model defaults.

```toml
provider = "ollama"
model = "qwen3.6:27b"

[ollama]
# Resource knobs:
num_ctx = 16384       # context window override (default: per-model)
num_predict = 2048    # max tokens to generate
num_thread = 8        # threads for the runtime

# Sampling knobs:
mirostat = 2          # 0 = off, 1 = Mirostat, 2 = Mirostat 2.0
top_k = 40
top_p = 0.9
repeat_penalty = 1.1
repeat_last_n = 64

# Reproducibility:
seed = 42
```

These propagate into Ollama's request `options: {...}`
block. Ollama's per-model defaults apply for any field
the operator hasn't overridden — `num_ctx`, in
particular, varies widely by model (some are 2048, some
are 128K+).

### Operator-protected Ollama deployments

Vanilla `ollama serve` doesn't require authentication, but
operators running Ollama behind a reverse proxy (Caddy,
nginx, Cloudflare Access) can attach an API key. The
provider emits `Authorization: Bearer <key>` only when
`OLLAMA_API_KEY` is set; absent means no header (matches
the OpenAI provider's defensive empty-key posture).

```sh
export OLLAMA_API_KEY="opaque-token-issued-by-your-proxy"
aivyx-pa
```

### When to pick `provider = "openai"` instead

The native adapter is the right default for any Ollama
deployment. The OpenAI-compat path is the right choice
when:

- You're targeting cloud OpenAI directly (Phase 25 use
  case).
- You're targeting a non-Ollama OpenAI-compat service
  (vLLM with OpenAI-compat enabled, LM Studio,
  llama.cpp's `--api-base`, etc.) that doesn't speak
  Ollama's native JSONL protocol.

In both cases use `provider = "openai"` and set
`OPENAI_BASE_URL` to the target endpoint.

## Local LLM provider alternatives (Phase 133)

Aivyx PA ships first-class support for **three local-LLM
runtimes** — Ollama (default), `llama-server` from
llama.cpp, and [Jan](https://jan.ai). Pick whichever
fits your workflow; all three are documented as
equal-status. Switching providers is a one-line
`aivyx-pa.toml` change, no rebuild required.

### The three providers at a glance

| Provider | License | UX shape | Default port | Model management | Phones home? |
|---|---|---|---|---|---|
| **Ollama** | MIT | CLI-first daemon | `:11434` | `ollama pull <model>` + Aivyx PA exposes `ollama.list/show/pull` as agent tools | Yes — update checks + telemetry (see [Ollama privacy posture](#ollama-privacy-posture) below) |
| **`llama-server`** (llama.cpp) | MIT | Raw bare-metal | `:8080` | Manual GGUF download from HuggingFace | No — pure inference server, no outbound calls |
| **[Jan](https://jan.ai)** | Apache 2.0 | Desktop GUI + API | `:1337` | GUI-driven model hub | No — explicitly no telemetry by default |

### Picking among the three

- **Use Ollama** when you want CLI-first model management
  (`ollama pull qwen3:32b` is genuinely the smoothest
  install UX for a new model), when Aivyx PA's
  `ollama.list/show/pull` agent tools matter to your
  workflow, or when you already have it installed and
  see no reason to switch.
- **Use `llama-server`** when you want raw bare-metal
  control over llama.cpp options (`-ngl`, `-c`,
  `--mlock`, etc.) without an Ollama wrapper translating
  them, when you're building from source for a custom
  hardware target, or when the no-telemetry posture
  matters more than the model-management UX.
- **Use Jan** when you want a polished desktop GUI for
  model browsing/downloading without sacrificing the
  OpenAI-compatible API surface, when the no-telemetry
  posture matters and you don't want a CLI tool, or
  when you're recommending Aivyx PA to a less technical
  end user who would otherwise pick LM Studio
  (proprietary).

### `aivyx-pa.toml` snippets

**Ollama** (default; no change needed for existing
operators):
```toml
[agent]
provider = "ollama"
model    = "qwen3:32b"
```

**`llama-server`** (start it separately first —
`llama-server -m /path/to/model.gguf -c 32768`):
```toml
[agent]
provider = "llamacpp"
model    = "qwen3-32b"  # arbitrary string; llama-server ignores it

[openai]
base_url = "http://localhost:8080"  # override if you bound a custom port
```

#### KV-cache persistence (`llama-server` only)

Aivyx PA can persist a `llama-server` slot's KV-cache state to disk across
process restarts. This benefits `provider = "llama_cpp"` (aliases:
`llamacpp`, `llama-cpp`) specifically — Ollama and Jan have no equivalent
slot-save API, so this is a no-op for those providers. With it working, a
fresh process's first turn on a system prompt + tool-def combination it
has seen before can skip re-prefilling that stable prefix instead of
paying for it again from a cold slot.

**Hard prerequisite:** start `llama-server` with `--slot-save-path`
pointed at exactly `<data_local_dir>/kvcache/slots` (on Linux,
`~/.local/share/aivyx-pa/kvcache/slots`; the exact path is
platform-specific — resolved via the `directories` crate). If the flag is
missing, or points somewhere else, the feature does not error — it
silently does nothing useful: every turn pays the save/restore round
trip's latency for none of its benefit.

```sh
llama-server -m /path/to/model.gguf -c 32768 \
  --slot-save-path ~/.local/share/aivyx-pa/kvcache/slots
```

By default this activates automatically once `provider = "llama_cpp"`
and the flag above are both set, using
`~/.local/share/aivyx-pa/kvcache/slots` (platform-specific — see the
`directories` crate's own docs). Set `[kvcache] store_path` in
`aivyx-pa.toml` to use a different directory instead — in particular, to
share the store with a locally delegated `aivyx-coder` process pointed
at the same `llama-server`; see `docs/MCP_RECIPES.md`'s `aivyx-coder`
recipe for that pairing.

Known caveat: this benefits fresh-process turns only today — the
daemon's own main agent on its very first turn after a restart — not an
ongoing conversation's later turns within one already-running process
(those already reuse KV state for free via `llama-server`'s own
automatic prefix matching). Nonagon specialists/leads benefit too, but
not because each runs in a fresh process — `SpecialistFactory::build`
constructs them in-process and they share the daemon's own single
`Arc<KvSlotPool>` (see `build_shares_one_kv_slot_pool_across_every_specialist_it_builds`
in `crates/aivyx-team/src/factory.rs`). The real reason they benefit is
that each distinct soul/role has its own distinct stable system prompt —
a distinct cache key — so it earns its own slot and cache entry the same
way a fresh daemon process does. The on-disk budget is also currently a
hard-coded default rather than an `aivyx-pa.toml` knob — a known follow-up,
not something this fix wave adds.

**Jan** (open the Jan desktop app, enable "Local API
Server" in settings):
```toml
[agent]
provider = "jan"
model    = "qwen2.5-7b-instruct"

[openai]
base_url = "http://localhost:1337/v1"  # override if you changed Jan's port
```

### Coordinating GPU-slot access across multiple processes (`aivyx-broker`)

If more than one local process shares a single `llama-server` — e.g. `aivyx-pa`'s
own daemon and a delegated
[`aivyx-coder`](https://github.com/Aivyx-Agent/aivyx-coder) subprocess pointed
at the same GPU — each picks a physical KV-cache slot independently, with zero
awareness of the other. `llama-server` itself defers rather than corrupts state
on a same-slot collision, but the result is silent head-of-line blocking and
cache-locality thrash.

[`aivyx-broker`](https://github.com/Aivyx-Agent/aivyx-broker) is a standalone
local daemon that sits between every client and the real `llama-server`: it
owns live slot admission and the KV-cache restore/warm/save lifecycle, then
forwards the completion through untouched. Point `aivyx-pa` at the broker
instead of `llama-server` directly and it stops doing its own local
slot-picking and kvcache restore/save — the broker now owns that (see
`aivyx-broker`'s own `README.md` for how to build, run, and configure it,
including its `--kvcache-store-path` flag, which should point at the same
directory this repo's own `[kvcache] store_path` / `AIVYX_PA_KVCACHE_STORE_PATH`
would otherwise use, so every client shares one on-disk cache).

```toml
[agent]
provider = "broker"
model    = "qwen3-32b"  # arbitrary string; forwarded to the real llama-server

[broker]
base_url = "http://127.0.0.1:8899"  # aivyx-broker's own default bind; override if you changed --port
```

`aivyx-broker` must already be running (started the same way you'd start
`llama-server` itself — there is no auto-spawn); if it isn't, `aivyx-pa` sees a
connection-refused error against `[broker] base_url`, the same shape as
`llama-server` being down. Only worth adopting once more than one local
process actually shares the same `llama-server` — a single-process setup gets
no benefit from it and should stay on `provider = "llamacpp"`'s own
[KV-cache persistence](#kv-cache-persistence-llama-server-only) instead.

### Tradeoffs the matrix doesn't capture

- **Model-family metadata.** Aivyx PA's textual tool-call
  extractor (Phase 127) uses Ollama's `/api/show` to
  detect qwen / phi / etc. and pick the right tool-call
  parser. Neither `llama-server` nor Jan exposes
  `/api/show` in the same shape — the extractor falls
  through to heuristic detection on these providers,
  which is less accurate. Models with non-standard
  tool-call formats may need explicit per-model
  configuration on llama-server / Jan.

- **`ollama.list/show/pull` agent tools.** These three
  Aivyx PA tools are Ollama-specific and not registered
  for the other providers — `llama-server` has no
  registry equivalent, and Jan's model hub lives in the
  desktop GUI. Agents on the alternative providers
  cannot autonomously discover or download models.

- **Daemon-style operation.** `llama-server` and Ollama
  both run as long-lived background processes. Jan runs
  inside the desktop app — closing the app stops the
  API server. For 24/7 daemon-style Aivyx PA
  deployments (Telegram/Discord/Slack frontends), pick
  Ollama or `llama-server`.

### Ollama privacy posture

Aivyx PA markets itself as a privacy-first local-agent
platform, which puts Ollama's outbound network calls
under scrutiny. Honest framing of what's known:

- **Ollama's code is MIT-licensed and auditable**,
  but the project does not publish a complete
  enumeration of what the binary phones home for.
  Community threads (e.g. ollama/ollama issue #2567,
  #11442) have raised this repeatedly.
- **Known outbound calls** include update checks
  against Ollama's release server, model registry
  pulls against `ollama.com`/`registry.ollama.ai`
  when you `ollama pull`, and telemetry the binary
  doesn't publicly enumerate.
- **January 2026 incident.** A joint SentinelOne /
  Censys investigation found **175,000 publicly-
  exposed Ollama hosts across 130 countries** — most
  bound to `0.0.0.0` without authentication, creating
  governance gaps and prompt-injection proxy
  potential. The default `OLLAMA_HOST` binding has
  been a frequent source of accidental exposure.

**Lockdown guidance** if you want to keep Ollama and
minimize its outbound surface:

1. **Force loopback binding.** Set
   `OLLAMA_HOST=127.0.0.1:11434` in your shell rc
   (or as a systemd `Environment=` line) so Ollama
   refuses non-local connections regardless of what
   the operator types.
2. **Firewall outbound `:443` from the Ollama
   process** if you don't intend to `ollama pull`
   models. The update check and any telemetry land
   here; blocking them at the firewall level breaks
   `pull` (acceptable cost) but stops the project
   from receiving signal it didn't earn.
3. **Disable auto-update.** No documented config
   knob today; the most robust approach is the
   firewall block above.
4. **Audit your specific deployment.** Run Ollama
   behind a packet logger (tcpdump, OpenSnitch,
   Little Snitch) during a representative Aivyx PA turn
   and confirm what you see. The honest answer to
   "what does Ollama send?" is **operator-verified,
   not project-documented**.

**Alternative posture: use `llama-server` or Jan
instead.** Both are documented above; both have no
outbound network calls by default. The trade-off is
the loss of Ollama's `pull` UX and Aivyx PA's
`ollama.list/show/pull` agent tools.

### What Phase 133 deliberately doesn't ship

- **No `llamacpp.list/show/pull` agent tools.**
  `llama-server` has no model-management API; the
  operator downloads GGUF files and loads them on
  start.
- **No `jan.list/show/pull` agent tools.** Jan's
  model hub is GUI-driven; an agent-side API would
  be a Jan upstream feature request, not Aivyx PA work.
- **No embedded Rust-native inference.** Direction B
  from the Phase 133 research note (mistral.rs /
  Candle inside Aivyx PA as a Rust dependency, no
  separate runtime) is the leading Phase 134+
  candidate. Phase 133 ships the multi-provider
  story first and gathers empirical signal before
  committing to the bigger architectural shift.

### Embedded Rust-native inference (Phase 134)

Phase 134 ships **Direction B**: Aivyx PA can run a
local LLM **inside its own process** by linking
against the `mistralrs` crate as a Rust dependency.
Zero outbound network calls during inference; no
separate runtime server to install. Single-binary
local-agent UX.

#### Building Aivyx PA with the embedded provider

```bash
# Recommended for new users — pulls in the embedded
# provider alongside Anthropic/OpenAI/Ollama:
$ cargo install --features recommended-providers aivyx-channel

# Lean build — Ollama-only, no mistralrs dependency.
# Compiles fast; smallest release binary.
$ cargo install aivyx-channel

# Embedded provider with platform GPU acceleration —
# pick exactly one per platform:
$ cargo install --features aivyx-channel/provider-mistral-rs-cuda aivyx-channel       # NVIDIA
$ cargo install --features aivyx-channel/provider-mistral-rs-metal aivyx-channel      # Apple Silicon
$ cargo install --features aivyx-channel/provider-mistral-rs-accelerate aivyx-channel # Apple CPU
```

The embedded provider's pure-Rust CPU build requires
**no C compiler, no CUDA toolkit, no Metal SDK**. The
backend-acceleration features have prerequisites:

| Backend | Feature | Build prerequisite | Runtime |
|---|---|---|---|
| CPU | `provider-mistral-rs` | None | Any platform |
| CUDA | `provider-mistral-rs-cuda` | CUDA toolkit (>= 11.8) | NVIDIA GPU with CC >= 8.0 |
| Metal | `provider-mistral-rs-metal` | macOS + Xcode | Apple Silicon |
| Accelerate | `provider-mistral-rs-accelerate` | macOS + Xcode | Apple CPU |

#### `aivyx-pa.toml` snippet

```toml
[agent]
provider = "mistralrs"
model    = "qwen3-4b"  # display name; arbitrary string

[mistralrs]
# REQUIRED — absolute path to a GGUF file or directory
# containing GGUF files.
model_path = "/home/operator/models/Qwen3-4B-Q4_K_M.gguf"

# Optional — when model_path is a directory, names the
# specific file to load.
# model_file = "qwen3-4b-q4_k_m.gguf"

# Optional — chat template path. Omit to use the
# template embedded in the GGUF (most modern
# quantizations ship one).
# chat_template_path = "/home/operator/templates/qwen3.json"

# Optional — maximum sequence length. Omit to defer to
# the model's declared max_seq_len.
# max_seq_len = 32768
```

#### Recommended GGUF models

Aivyx PA doesn't bundle any model — operators download
the GGUF themselves and point `model_path` at it.
Recommended starting points for the embedded provider:

| Model | Size (Q4_K_M) | Min RAM | Use case | Download |
|---|---|---|---|---|
| **Qwen3-4B** | ~2.5GB | 6GB | Best general agent; strong tool calling | [HF: Qwen/Qwen3-4B-Instruct-GGUF](https://huggingface.co/Qwen) |
| **Llama-3.2-3B-Instruct** | ~2.0GB | 5GB | Conservative default; well-tested | [HF: bartowski/Llama-3.2-3B-Instruct-GGUF](https://huggingface.co/bartowski) |
| **Phi-4-mini-instruct** | ~2.4GB | 5GB | Microsoft tooling; XML tool-call format | [HF: microsoft/Phi-4-mini-instruct-gguf](https://huggingface.co/microsoft) |
| **SmolLM2-1.7B-Instruct** | ~1.1GB | 3GB | Smallest practical agent; CPU-friendly | [HF: HuggingFaceTB/SmolLM2-1.7B-Instruct-GGUF](https://huggingface.co/HuggingFaceTB) |

Operators with substantially more RAM and GPU VRAM can
load 7B-14B models (Qwen3-14B, Llama-3.3-8B) for
materially stronger reasoning at the cost of larger
working sets.

#### When to pick the embedded provider vs Ollama

- **Pick embedded** when you want a single-binary
  install with no separate runtime to manage, when
  you want **zero outbound network calls during
  inference** (the privacy end state), or when you're
  recommending Aivyx PA to a less technical operator
  who'd otherwise stall at "install Ollama first."
- **Stick with Ollama** when you want `ollama
  pull <model>` as your model-download UX, when
  Aivyx PA's `ollama.list/show/pull` agent tools matter
  to your workflow, or when you already have Ollama
  installed and aren't motivated to rebuild Aivyx PA.

#### Honest tradeoffs

- **Build cost.** `--features
  provider-mistral-rs` first build: ~5-10 minutes
  (mistralrs is a substantial crate; subsequent
  incremental builds are fast). CUDA variant adds
  cuBLAS/cuDNN linking time.
- **Binary size.** Release binary adds ~100-200MB on
  the CPU variant. CUDA variant adds NVIDIA runtime
  libraries.
- **mistralrs is pre-1.0.** Pinned to `=0.8.*` in
  Aivyx PA's Cargo.toml. Aivyx PA-side upgrades happen
  explicitly per-phase.
- **TLS stack.** mistralrs's transitive dependency
  tree pulls in `aws-lc-rs` alongside Aivyx PA's
  workspace `rustls`. Both stacks coexist; the slim
  Ollama-only build keeps rustls-only as before.

#### What Phase 134 deliberately doesn't ship

- **Streaming text deltas.** Phase 134 issues
  `send_chat_request` (full response in one shot)
  rather than the streaming API; the operator sees
  the assistant message arrive whole, not
  token-by-token. mistralrs 0.8.1's `Stream<'a>`
  borrows from the Model, which doesn't satisfy
  Aivyx PA's `LlmStream` contract without a
  self-referential struct or a mpsc-forwarding
  spawned task — both deferred to Phase 135.
- **Multimodal inputs.** Image / audio / video
  content blocks are stripped to `[image]` /
  similar placeholders. mistralrs supports them
  natively; the bridge wiring is Phase 135+ work.
- **Model-family probing.**
  `LlmProvider::tool_call_family_hint` returns
  `None` for embedded. Operators with non-default
  tool-call formats (qwen3 XML, phi4 wrappers) rely
  on the heuristic detection in Aivyx PA's textual
  extractor or set `[mistralrs] family_hint = "..."`
  in a future phase.
- **`mistralrs.list/show/pull` agent tools.** Same
  posture as llama-server / Jan — operator-driven
  model download via `wget` or the HF CLI; no agent-
  side autonomy.
- **End-to-end hardware validation.** Phase 134
  ships unit-tested conversion logic and a
  compile-clean bridge. The "load a real GGUF on a
  real machine and run a turn" validation needs
  operator coordination on each backend (CPU on
  Linux laptop, Metal on M-series Mac, CUDA on
  NVIDIA box). Phase 135+ codifies operator-reported
  empirical signal.

## Voice channel: talk to the agent, agent talks back (Phase 135)

Aivyx PA ships **voice I/O** — its eighth channel adapter.
The operator speaks into the microphone; Whisper
transcribes; the agent runs the turn; **Kokoro**
synthesizes the response; the operator hears it through
the speakers. **Everything runs in-process on the
operator's machine; zero outbound network calls during
inference.** Same local-privacy posture as the embedded
LLM, extended end-to-end.

> **Permissive voice (Chapter Timbre, `docs/TIMBRE.md`).** The TTS engine is the
> Apache/MIT **Kokoro** stack (Kokoro-82M via `ort` + `voice-g2p`). The earlier
> GPL-3.0 Piper engine — which dragged in espeak-ng and required system ONNX
> runtime headers — was removed. Voice no longer needs any espeak-ng or
> ONNX-headers prerequisites: `ort` fetches a prebuilt ONNX Runtime at build
> time.

### Building with the voice channel

```bash
# Recommended one-liner — voice channel with the
# bundled whisper-rs (STT) + Kokoro (TTS) engines:
$ cargo install --features channel-voice-full aivyx-channel

# Bare voice channel (no engines). Useful for
# Phase 136+ when wiring an alternative engine
# yourself:
$ cargo install --features channel-voice aivyx-channel

# Lean build — no voice channel. Existing operators
# see zero binary-size impact.
$ cargo install aivyx-channel
```

### Build prerequisites

| Component | What it needs | Per-OS install |
|---|---|---|
| `whisper-rs` (STT) | C++ compiler | usually pre-installed; Linux: `apt install build-essential` |
| `ort` (Kokoro TTS) | nothing extra | `ort`'s default `download-binaries` fetches a prebuilt ONNX Runtime at build time — **no system ONNX headers, no espeak-ng** |
| `cpal` + `rodio` | OS audio API | always installed (ALSA / PipeWire / CoreAudio / WASAPI come with the OS) |

### `aivyx-pa.toml` snippet

```toml
[agent]
provider = "ollama"  # or "mistralrs" / "openai" / etc.
model    = "qwen3:32b"

[voice]
# Pick the ASR engine — currently "whisper-rs" is the
# only working option. Phase 136+ may add
# whisper-cpp-plus once upstream is unstuck (see
# Phase 135 exit doc).
asr_engine = "whisper-rs"

# Pick the TTS engine — "kokoro" (the permissive
# default; unset also selects Kokoro).
tts_engine = "kokoro"

# Optional cpal input/output device override. Empty
# = system default.
# input_device = "USB Mic"
# output_device = "Default"

[voice.asr]
# REQUIRED — absolute path to a Whisper .bin model.
model_path = "/home/operator/models/ggml-base.en.bin"
# Optional — language code. Defaults to "en". Use
# "auto" for automatic detection.
language = "en"
# Optional — beam search width. Higher = more
# accurate, slower. Defaults to 5.
beam_size = 5

[voice.tts]
# REQUIRED — absolute path to the Kokoro model
# directory. It must hold the Kokoro `.onnx` model and
# a `voices-*.bin` archive (and may hold a config.json
# with the phoneme vocab; a built-in vocab is used
# otherwise).
model_dir = "/home/operator/models/kokoro"
# Optional — voice name (an entry in voices-*.bin).
# Defaults to "af_heart".
voice_name = "af_heart"
# Optional — speaking-rate multiplier. Defaults to 1.0.
speed = 1.0
```

### Recommended models

#### Whisper STT models

| Model | Size (Q5_1) | RAM | Languages | When to pick |
|---|---|---|---|---|
| **whisper-base.en** | ~150MB | ~500MB | English only | Recommended starting point. Good accuracy, fast. |
| **whisper-base** | ~150MB | ~500MB | 99 languages | Multilingual operators. |
| **whisper-small.en** | ~500MB | ~1.2GB | English only | Materially better accuracy than base; still real-time on CPU. |
| **whisper-medium.en** | ~1.5GB | ~2.7GB | English only | Highest accuracy at reasonable speed; needs a beefier CPU. |

Download from [HuggingFace ggerganov/whisper.cpp](https://huggingface.co/ggerganov/whisper.cpp/tree/main):
```bash
$ wget https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin
```

#### Kokoro TTS model + voices

Kokoro-82M is **Apache-2.0** licensed (model + weights), neural quality, 24 kHz,
English. The voice channel needs two files in the `model_dir`:

| File | What it is | Note |
|---|---|---|
| `kokoro*.onnx` | the acoustic model | fp32 (~330 MB) or a quantized export (~80–170 MB); either works |
| `voices-*.bin` | the bundled voices (e.g. `af_heart`, `bf_emma`) | ~26 voices in one archive |

Download both into one directory and point `model_dir` at it, for example from the
ONNX community release of Kokoro
([hexgrad/Kokoro-82M](https://huggingface.co/hexgrad/Kokoro-82M) and the
`kokoro-onnx` releases):
```bash
$ mkdir -p ~/models/kokoro && cd ~/models/kokoro
$ wget https://github.com/thewh1teagle/kokoro-onnx/releases/download/model-files-v1.0/kokoro-v1.0.onnx
$ wget https://github.com/thewh1teagle/kokoro-onnx/releases/download/model-files-v1.0/voices-v1.0.bin
```

> G2P (grapheme→phoneme) is done by the bundled MIT `voice-g2p` — **no espeak-ng
> install required**. Kokoro is English-only today (parity with the old Piper
> English default).

### Running it

```bash
$ aivyx-pa --channel voice
```

**Phase 156 closed Phase 154's three honest-
debts in one bundle:**

- **Multi-image queue.** Operator can type
  `/image foo.png` then `/image bar.png` then
  `/image baz.jpg` before recording — all
  three attach to the next turn as
  `MessageContent::Mixed`. Pre-156 the second
  command replaced the first.
- **URL source.** `/image <path-or-url>` —
  when the argument starts with `http://` or
  `https://`, the loop fetches via reqwest +
  infers media type from the response's
  `Content-Type` header (falling back to
  URL-path extension if the header is
  absent). Operator can attach any public
  image URL without manual download.
- **Client-side size cap.** Files or URL
  responses exceeding the cap (default 10MB,
  Phase 161-tunable) are rejected at attach
  time. Matches Phase 129's Drive
  `CONTENT_INLINE_CAP_BYTES` for cross-
  substrate consistency.

**Phase 161 + 162 closed Phase 156's full debt
ledger.** Phase 161 brought operator-tunable
knobs to size cap, URL timeout, and HEAD pre-
fetch; Phase 162 added PDF / SVG / TIFF support
and authenticated URL fetch via a TOML header
map.

```toml
[voice.image]
size_cap_mb       = 10     # Default 10. Replaces the hardcoded MAX_IMAGE_SIZE_BYTES.
url_timeout_secs  = 30     # Default 30. Per-request timeout on `/image <url>` fetches.
head_precheck     = true   # Default true. Refuses over-cap URLs before download
                           # when Content-Length is advertised.

# Phase 162 — operator-supplied HTTP headers applied to both the
# HEAD pre-check and the GET when fetching `/image <url>`. Each
# `key = "value"` entry becomes one HTTP header. Defaults to empty;
# unauthenticated URLs work without this block.
[voice.image.url_headers]
Authorization = "Bearer xxx"
Cookie        = "session=yyy"
Origin        = "https://example.com"
```

All fields have defaults that match Phase 156
behavior, so operators with no `[voice.image]`
section see unchanged behavior. The HEAD pre-
check falls through to GET on 405 Method Not
Allowed, on chunked-transfer responses (no
Content-Length), and on HEAD transport failures
— operators with HEAD-hostile origins don't
lose access.

**Phase 162 supported image types:** Phase 156
shipped png / jpg / jpeg / gif / webp. Phase 162
extends with **pdf** (`application/pdf`), **svg**
(`image/svg+xml`), and **tif / tiff**
(`image/tiff`).

**Phase 163 (amendment A13) — PDF document
routing.** PDFs no longer surface as image
blocks. The voice substrate routes
`application/pdf` through
`ContentPart::Document`, which the Anthropic
provider emits as a native document content
block (Claude 3.5+). On OpenAI / Ollama /
mistral_rs, document blocks are dropped with a
one-line warning to stderr (those providers
don't have a document content block in their
chat APIs — OpenAI has a separate Files API
flow). See
[`docs/amendments/2026-06-04-content-part-document.md`](amendments/2026-06-04-content-part-document.md)
for the contract change.

**Phase 164 — Anthropic document model-version
guard.** When operators attach a document block
and their configured Anthropic model is pre-
Claude-3.5, the substrate refuses client-side
with a clear `LlmError::Config` ("model 'X'
does not support document blocks; document
content blocks require Claude 3.5 or newer") —
no API call. Supported prefixes:
`claude-3-5-*`, `claude-3-7-*`,
`claude-opus-4-*`, `claude-sonnet-4-*`,
`claude-haiku-4-*`. New variants released after
Phase 164 with a different prefix fail-closed
until the substrate adds the prefix (the fix
is a one-line PR).

**Phase 164 — DOCX inference.** Files with
`.docx` extension or `application/vnd.
openxmlformats-officedocument.
wordprocessingml.document` Content-Type now
route through the `Document` variant.
Anthropic's document blocks accept PDF only as
of writing; DOCX attached to Anthropic
surfaces as a 400 from the API.

**Phase 165 — five more Office formats.**
`.doc` (`application/msword`), `.rtf`
(`application/rtf` or `text/rtf`), `.odt`
(`application/vnd.oasis.opendocument.text`),
`.pptx`
(`application/vnd.openxmlformats-officedocument.presentationml.presentation`),
and `.xlsx`
(`application/vnd.openxmlformats-officedocument.spreadsheetml.sheet`)
extend the Phase 164 set. All five route as
Document; same provider-side caveats as DOCX.
Phase 165 also corrects Phase 164's
content-type conflation: `application/msword`
now maps to DOC (legacy binary format) rather
than DOCX.

**Phase 165 — per-URL header presets.**
Operators with multiple authenticated origins
can now switch header bundles per `/image`
attach instead of being locked to one global
set. Configure named presets:

```toml
[voice.image.url_header_presets.work]
Authorization = "Bearer work-token"
"X-Workspace" = "production"

[voice.image.url_header_presets.personal]
Cookie = "session=personal-yyy"
```

Select with the `--headers <preset-name>` flag:

```
/image https://intranet.work.example/diagram.png --headers work
/image https://photos.personal.example/a.png --headers personal
```

Without `--headers`, the global
`[voice.image.url_headers]` block applies
(unchanged Phase 162 behavior). Unknown
preset names produce a clear error listing
the available presets; an empty preset name
or whitespace-containing name is rejected at
parse time.

**Phase 165 — best-effort PDF page-count cap
on Anthropic.** When operators attach a PDF
to a document-capable Anthropic model, the
substrate byte-scans for `/Type /Page`
markers and refuses if the count exceeds
the cap. Honest caveat: PDFs using FlateDecode
object streams (common in modern Acrobat
output) hide their page-object headers from
the byte-scan; those fall through to
Anthropic's server-side cap enforcement. The
client-side check provides additive defense
for uncompressed PDFs (older tools, command-
line generators, scanned documents) where the
markers are visible.

**Phase 166 — operator-tunable Anthropic PDF
page cap.** The cap defaults to 100 (matching
Anthropic's documented limit) but operators
with custom plans can override two ways:

- **Builder method:**
  `AnthropicConfig::new(...).with_pdf_page_cap(250)`.
- **Environment variable:**
  `AIVYX_PA_ANTHROPIC_PDF_PAGE_CAP=250 aivyx-pa ...`.
  Invalid values (non-numeric, zero) fall back
  to the default. Set per-process; affects all
  aivyx-pa instances in the shell.

**Phase 168 — catalog-aware PDF page count.**
Phase 165's byte-scan counted `/Type /Page`
markers in the PDF body — accurate for
uncompressed PDFs but missed pages inside
FlateDecode object streams (modern Acrobat
output). Phase 168 augments the count with a
catalog-aware scan that picks up the root
`/Type /Pages /Count N` declared total (which
typically remains visible even when individual
page objects are compressed). The cap check
now uses `max(per_page_scan, declared_count)`.
Honest caveat: if the catalog itself is inside
a compressed object stream (PDF 1.5+ full-
document compression), both approaches miss
and Anthropic's server-side cap handles.

**Phase 170 — mid-recording `/image`.** Pre-
Phase-170 any line typed during recording
stopped capture. Phase 170 changes this: lines
starting with `/image <path>` now queue the
attachment AND continue recording. Empty Enter
still stops; `quit` still exits. Useful for
the "I want to describe AND show this thing"
flow without breaking the turn.

**Phase 170 + 171 — `/image clipboard`.** Reads
from the platform clipboard tool:
- Linux/Wayland: `wl-paste --type image/png`
- Linux/X11: `xclip -selection clipboard -t image/png -o`
- macOS: `pbpaste -Prefer raw` (best-effort
  — macOS clipboard image flow is
  historically fragile)

Recognizes PNG (`0x89 50 4E 47`), JPEG
(`0xFF D8`), GIF (`GIF`), and WebP (`RIFF`
+ size + `WEBP`) byte signatures (Phase 171
added GIF and WebP). Operators without the
platform tool installed see a clear "spawn
failed" error pointing at the missing binary.

**Phase 170 — voice abort UX knob.** Phase
146's mid-synthesis single-Enter abort gets a
double-press guard so a stray Enter doesn't
kill a long reply:

```toml
[voice]
abort_requires_double_enter      = true   # Default false.
abort_double_enter_window_ms     = 800    # Default 800ms.
```

When `abort_requires_double_enter` is true,
the first empty-Enter primes a window and
prints `[voice] press Enter again within
800ms to abort`; a second Enter within the
window aborts as before. `quit` and other
non-empty inputs still abort immediately
regardless of the toggle.

**Phase 166 — URL fetch retry on transient
timeout.** `[voice.image]` adds two knobs:

```toml
[voice.image]
url_retry_count       = 3      # Default 0. Number of retries on
                               # client-side transient errors.
url_retry_backoff_ms  = 500    # Default 500. Base backoff; doubles
                               # per retry (500, 1000, 2000, 4000 ms).
```

Retries fire on `reqwest::is_timeout()` and
`is_connect()` (client-side) PLUS HTTP 503
Service Unavailable and 429 Too Many Requests
(server-side transient, added Phase 169).
Other 4xx (auth / content) and other 5xx
(genuine server errors) are operator-fixable
and do NOT retry. Default `url_retry_count = 0`
preserves Phase 161 single-attempt behavior.

**Phase 169 — backoff jitter.** Add ±jitter
randomization to defeat thundering-herd
patterns when multiple operators retry against
the same flapping origin:

```toml
[voice.image]
url_retry_jitter_ms = 250  # Default 0 = deterministic.
```

Each backoff delay is offset by a random value
in `[-jitter_ms, +jitter_ms]`. PRNG source is
stdlib `SystemTime::subsec_nanos` — not
cryptographic but fine for desynchronizing
retries. Default 0 preserves Phase 166's
deterministic backoff.

**Phase 168 — read-stalled-bytes timeout.**
Phase 161's `url_timeout_secs` catches the
total wall-clock; it does NOT catch a server
that trickles 1 byte per second forever.
Phase 168 adds a per-chunk stall check:

```toml
[voice.image]
url_read_stall_secs = 5  # Default 0 = disabled.
```

When set, the body read aborts if no chunk
arrives within the configured window —
defeating slow-trickle attacks. Default 0
preserves Phase 161's single-`bytes()` shape.

**Phase 166 — drive walk min_concurrent
floor.** `drive.recent_files` and
`drive.recent_changes` gain
`walk_min_concurrent` (cap 16, mirrors Phase
158 calendar). Composes with
`walk_max_concurrent` ceiling; min ≤ max
validated at parse time. The floor doesn't
manufacture work — a floor of 8 on a 3-folder
level still fires 3 futures (same posture as
Phase 158 calendar's min_concurrent).

SVG and TIFF still route as image blocks; most
vision LLMs reject them, and the rejection
surfaces as a per-attach error from the
provider. The voice substrate doesn't try to
guess which providers accept which formats.

**Phase 162 security note for `url_headers`:**
The TOML config file is the only place these
credentials live. If your `aivyx-pa.toml` ends up
checked into version control or world-readable,
the bearer tokens / cookies leak. Standard
hygiene applies:

```bash
chmod 600 ~/.config/aivyx-pa/aivyx-pa.toml
```

Phase 165+ candidate for a secret-store
integration so `url_headers` can reference an
OS keychain entry instead of inline plaintext.

**Phase 154 added multimodal input — image
attachment via voice.** Operator types `/image
<path-or-url>` at the start-of-iteration prompt
(instead of pressing Enter to record). The PTT
loop loads the file or fetches the URL, infers
the media type, and queues it on the channel.
The next recording iteration's turn sends the
image(s) alongside the transcribed prompt as
either `Message::text_with_image` (single) or
`MessageContent::Mixed` (multi-image). The
agent — running on a vision-capable LLM —
describes the image(s) via TTS.

Supported extensions: `.png`, `.jpg`/`.jpeg`,
`.gif`, `.webp`. Unsupported extensions
(`.pdf`, `.svg`, etc.) error out with a clear
"unsupported image extension" message; Phase
155+ candidate for widening.

Operator-side prereqs: a vision-capable LLM
must be configured as the agent's planner.
Tested with Qwen-VL via mistral.rs or Ollama;
should work with Anthropic Claude (any modern
model) or OpenAI GPT-4-vision via the existing
provider plumbing.

The queued image is consumed on the next turn
and cleared, so the operator never
accidentally re-sends. Operators can type
`/image` multiple times before recording — the
last one wins.

**Phase 152 closed three voice carry-overs in
one bundle.**

1. **Aggressive abort.** The mid-synthesis abort
   keybind (Enter during a reply) now stops audio
   instantly (within OS audio buffer time —
   typically ~10ms) instead of letting the
   currently-playing word finish naturally. Sharper
   UX; the trade-off is no graceful tail.

2. **Partial-text preservation on abort.** When
   the operator aborts mid-reply, the agent's
   partial response (whatever it had emitted
   through `stream_event` before cancellation)
   is now surfaced as `[voice] agent had said:
   ...` instead of silently lost. The agent can
   include this in subsequent reasoning or
   audit; the operator sees what was said up to
   the abort.

3. **VAD config bounded-range validation.**
   `[voice.vad]` config fields are now checked
   at PTT loop entry. Out-of-range values
   (negative `threshold_rms`, zero `dwell_secs`,
   `max_capture_secs` > 1 hour, etc.) surface as
   a configuration error rather than silently
   producing nonsense behavior. The full bounds
   table:
   - `threshold_rms` ∈ `[0.0, 10.0]`
   - `frame_secs` ∈ `(0.0, 1.0]`
   - `dwell_secs` ∈ `(0.0, 60.0]`
   - `min_speech_secs` ∈ `[0.0, 60.0]`
   - `max_capture_secs` ∈ `(0.0, 3600.0]`
   - `poll_interval_ms` ∈ `[1, 5000]`
   Operators with valid configs (including
   Phase 139's defaults) are unaffected.

**Phase 146 closed Phase 138's longest-running
voice debt: mid-synthesis abort UX.** Operator
can now press Enter (or the Enter key on
"quit") *while the agent is replying* to:
- Cancel the agent's in-flight LLM call.
- Halt all queued TTS playback immediately.
- Iterate back to the recording prompt (Enter)
  or exit the REPL ("quit" + Enter).

Symmetric to Phase 140's recording-side abort.
Four total keybinds:
- Enter             — start recording.
- Enter mid-record  — stop capture + dispatch
  partial.
- Enter mid-reply   — abort agent + playback.
- `quit` + Enter    — exit (any time).

Honest scope: the currently-playing audio
sample may finish its current ~50-100ms chunk
before silence (rodio Player::clear semantics).
For typical sentence-length playback the
operator may hear the rest of the current word
before silence. Aggressive abort via dropping
the cpal stream entirely is a Phase 147+
candidate.

**Phase 140 closed Phase 139's debt: VAD is now
operator-tunable via `[voice.vad]` TOML AND
manual abort works again mid-recording.**

Operator tuning via TOML:
```toml
[voice.vad]
threshold_rms     = 0.01   # RMS silence cutoff (lower = stricter)
dwell_secs        = 1.5    # pause to dispatch
min_speech_secs   = 0.5    # ignore auto-stop before this
max_capture_secs  = 30.0   # hard cap
frame_secs        = 0.030  # per-frame RMS window
poll_interval_ms  = 100    # PTT loop poll tick
```

All fields are optional; omitted fields take the
defaults shown above (= Phase 139's hardcoded
values). Operators in a quieter room with a
sensitive mic can drop `threshold_rms` to 0.005;
in a noisier room raise it to 0.02 or higher.
Operators who want longer breath-pauses
mid-utterance without auto-stop can raise
`dwell_secs` to 2.5 or 3.0.

Manual abort recovered: during recording, the
operator can press Enter to stop mid-utterance.
Whatever samples are captured at that point
still dispatch to the agent (the operator may
have started a thought worth completing).
Aborting before any audio captures skips the
turn with a heads-up.

**Phase 139 added silence-detection auto-stop.**
PTT no longer requires pressing Enter twice. The
operator hits Enter once to start recording,
speaks, then pauses — the mic auto-stops after
1.5 seconds of detected silence (RMS below
threshold) and dispatches the turn. A 0.5s
minimum-speech gate prevents instant auto-stop
before the operator has started talking; a 30s
hard cap protects against a stuck mic recording
forever.

Thresholds are hardcoded in Phase 139 (frame
30ms, RMS 0.01, dwell 1.5s, min-speech 0.5s,
max-capture 30s). Operators in noisy
environments may need ML VAD (Phase 140+
candidate) or a `[voice.vad]` TOML knob (also
Phase 140+ if demand surfaces).

The trade-off vs Phase 138: no manual abort
mid-recording. If the operator wants to abandon
a half-spoken message, they need to wait 1.5s in
silence (which dispatches a partial-transcription
turn).

**Phase 138 collapsed voice latency with streaming
TTS.** The agent's reply is pipelined through the
TTS engine on sentence boundaries — the operator
hears sentence one while the LLM is still
generating sentence three. A long reply (~200
words, ~15 seconds of synthesized audio) drops
from ~15 seconds of dead silence-before-playback
to roughly the latency of the first sentence
(typically 1-2 seconds).

How it works: a serial consumer task pulls
sentences from a `tokio::mpsc` queue as the agent
streams text; each sentence is synthesized and
played in order. Any final partial sentence
(text that didn't end with a trailing space) is
flushed after the agent finishes. On Linux +
Windows this works out-of-the-box; macOS may hit
a Send-safety constraint and need the Phase 139+
platform-aware variant.

**Phase 137 brought voice to full feature parity
with the Local channel** — voice agents now get
role overrides (Phase 30), per-turn Persona refresh
(Phase 60), context window pruning (Phase 43),
memory prune sinks (Phase 43 Task 4), auto-recall
from memory (Phase 76), and the adaptive Persona +
tool/skill relevance system prompt refiner (Phases
79 + 117). What you can do in `--channel local` you
can now do in `--channel voice`.

**Phase 136 closed out Phase 135's audio-I/O
deferral.** The push-to-talk loop now runs end-to-
end:

1. Aivyx PA prints `[voice] press Enter to record (or
   \`quit\`)`.
2. Operator hits Enter → cpal opens the configured
   mic, starts capturing.
3. Operator speaks; hits Enter again to stop.
4. Whisper transcribes the captured PCM.
5. Aivyx PA prints `[voice] you said: <transcript>`,
   dispatches the agent turn.
6. As the agent streams text back, Aivyx PA buffers it.
7. On turn completion, Aivyx PA chunks the response at
   sentence boundaries; Kokoro synthesizes each
   sentence to PCM; rodio queues them on the
   speakers.
8. Aivyx PA waits for playback to finish, then loops.

**Operator validation is still where end-to-end
audio gets stress-tested.** The substrate is unit-
tested with stub agent / ASR / TTS engines; the
real-mic + real-speaker path needs an operator on
hardware with mic + speakers + the model files
downloaded. If you hit issues, the most common
shapes are:

- **No input device:** OS audio passthrough disabled
  (Linux containers, WSL without PulseAudio bridge).
  Errors as
  `audio device error: input: no default input
  device — check OS audio settings`.
- **Kokoro model dir wrong / incomplete:** engine
  construction fails when `[voice.tts] model_dir`
  is unset or doesn't hold a `.onnx` + a
  `voices-*.bin`. Errors as
  `failed to build KokoroEngine: …no .onnx model
  found…` / `…no voices-*.bin…`; double-check
  `[voice.tts] model_dir`.
- **Whisper model wrong format:** `whisper-rs`
  surfaces this from the `.bin` parse. Errors as
  `failed to build WhisperRsEngine: ASR model load
  failed: ...`.

### What Phase 135 / 136 deliberately don't ship

- ~~**The cpal + rodio audio I/O loop.**~~ **Shipped
  in Phase 136.** See "Running it" above.
- **whisper-cpp-plus alternative ASR engine.** The
  Q2c sign-off picked both engines, but published
  `whisper-cpp-plus = "0.1.4"` doesn't build
  against current whisper.cpp (40 errors against
  `whisper_full_params` struct shape). Feature flag
  is wired for future re-enablement; stub module
  documents the deferral. Phase 137+ revisits when
  upstream is unstuck or we swap to a different
  binding.
- **Streaming TTS during LLM generation.** Phase
  135 buffers the agent's full response, then chunks
  into sentences for synthesis. Streaming
  pipelined-with-LLM is Phase 136+.
- **Wake-word activation.** "Hey Aivyx PA" style
  always-on listening is Phase 136+ (would add
  Porcupine or Silero-wakeword as a new dep).
- **Voice activity detection.** Silero VAD for
  trim-on-silence push-to-talk is Phase 136+
  (we'd need to pick a non-GPL Rust binding; the
  `silero` and `voice_activity_detector` crates are
  candidates).
- **Multimodal output.** Spoken descriptions of
  images (via the LLM's vision capability) are
  Phase 136+; voice in Phase 135 is text-only.

### Phase 120 substrate uniformity

The Phase 120 tool-name recovery substrate flows uniformly
through the native Ollama adapter. The provider classifies
each emitted tool name against the request's advertised
set; the planner runs `title_similarity` fuzzy-match
recovery above the operator-configured threshold; auto-
corrections land in `AuditEvent::ToolCall.auto_corrected_from`
with HMAC-chain-byte-identical canonical-JSON for the
dominant (no-correction) case. Same operator-forensics
recipe works:

```sh
aivyx-pa audit export --event-type ToolCall | \
  jq 'select(.auto_corrected_from)'
```

### Honest scope caveat carried from open doc

The Phase 121 open doc surfaced this at sign-off:
**the native Ollama adapter does not fix model-shaped
hallucination patterns directly.** A model that emits
`fs_read` will keep emitting `fs_read`. What Phase 121
gives is the substrate gain: native protocol fidelity,
operator-tunable Ollama options, and no OpenAI-compat
translation layer to debug. The hallucination recovery
itself is Phase 120's substrate, running uniformly
through both adapter paths.

## Per-model prompt variants (Phase 122)

After Phase 121 shipped, real-use signal across 13
interactive turns produced **zero tool calls** between
qwen3.6:27b and gemma4:31b. Both models confabulate
their tool catalogs at the prose level (qwen3.6
invented "Good Morning" as a tool; gemma4 invented
60+ entirely-fictional tools) and refuse or return
empty when commanded to invoke a tool by exact name.
Phase 120's substrate is orthogonal to this failure
mode — it catches hallucinated *invocations*; this is
hallucinated *capability denial*.

**Phase 122 ships structured per-turn tool-catalog
injection** with operator-tunable per-family
selection. The fix is prompt-side: tools flow through
Ollama's protocol `tools: [...]` array *and* land in
the assembled system prompt under a `## Tools
available` block. The catalog block forces the
protocol-array catalog into the model's visible prose
context where its prose-level reasoning cannot ignore
it.

### Per-family TOML override

The operator-overridable surface is a new sub-table
under `[ollama]`:

```toml
provider = "ollama"
model = "qwen3.6:27b"

[ollama.prompt_strategies]
qwen3 = "few_shot_examples"          # default for qwen3.x (Phase 124)
gemma4 = "few_shot_examples"         # default for gemma4 (Phase 124)
llama3 = "none"                      # default for llama3.x
```

Each value is one of:
- `"none"` — pre-Phase-122 behavior. Tools flow only
  via the Ollama protocol `tools: [...]` array. The
  assembled system prompt is unchanged.
- `"structured_injection"` — Phase 122 substrate. Append a
  `## Tools available` block listing every tool the active
  role can invoke by exact name (filtered by the role's
  `tool_allowlist`), with a one-line preamble discouraging
  invention.
- `"few_shot_examples"` — Phase 124 substrate. Includes
  the `## Tools available` block AND appends a
  `## Example tool use` block with 2-3 worked tool-call
  examples (fs.read / fs.write / memory.write — only the
  ones registered in the active role's allowlist), each
  carrying explicit WRONG/RIGHT framing against the
  "I don't have X" refusal pattern. Phase 124's substrate
  attempt on the capability-denial prior Phase 122 left
  unresolved.

Keys are family strings, not full model names. The
binary maps a model name to its family at startup:

| Model name           | Family   |
|----------------------|----------|
| `qwen3.6:27b`        | `qwen3`  |
| `qwen3.5:7b`         | `qwen3`  |
| `qwen2.5:7b`         | `qwen2`  |
| `gemma4:31b`         | `gemma4` |
| `gemma3:9b`          | `gemma3` |
| `llama3.1:latest`    | `llama3` |
| `llama2:13b`         | `llama2` |
| `claude-haiku-4-5`   | (none)   |

Anything that doesn't parse to a known Ollama family
prefix (cloud model names, future families this build
doesn't recognize) gets `OllamaFamilyStrategy::None`
unconditionally — operator-conservative: a new model
release doesn't silently get substrate it wasn't
tested against.

### Per-family defaults

Operators who don't set `[ollama.prompt_strategies]`
get pre-baked defaults. Phase 124 upgraded qwen3 + gemma4
from `structured_injection` to `few_shot_examples` after
Phase 122 empirically showed the catalog block alone
didn't bridge the capability-denial prior:

| Family   | Default              | Why                                                                              |
|----------|----------------------|----------------------------------------------------------------------------------|
| `qwen3`  | `few_shot_examples`  | Phase 124 upgrade — qwen3.6:27b timed out on fs.write under `structured_injection` |
| `gemma4` | `few_shot_examples`  | Phase 124 upgrade — gemma4:31b refused fs.write despite catalog listing it       |
| `llama3` | `none`               | tool-use protocol presumed reliable; preserved across phases                     |
| _other_  | `none`               | conservative default for untested families                                       |

An explicit `[ollama.prompt_strategies] <family> =
"..."` always wins over the default.

### Startup banner provenance

The config banner shows which strategy resolved for
your model and where it came from:

```
aivyx-pa config sources:
  provider          = ollama (toml)
  model             = "qwen3.6:27b" (toml)
  ollama_prompt_strategy = "few_shot_examples" (family: qwen3, default)
```

Provenance suffixes:
- `family: <name>, default` — model detected; no
  override in your TOML; per-family default applied.
- `family: <name>, override` — model detected; your
  `[ollama.prompt_strategies] <name>` override is
  being honored.
- `family: undetected` — model name didn't parse to
  any known family. Strategy always shows `"none"`.

### Cost: per-turn input-token overhead

The `structured_injection` catalog block adds roughly
**400-700 input tokens per turn** for the default role
(~12-20 tools at ~30-50 tokens each). The
`few_shot_examples` strategy adds another **~250-350
tokens** on top of that for the worked examples — total
**~650-1050 input tokens per turn** at the
`few_shot_examples` default.

For small-context models or cost-sensitive cloud
deployments, the per-family override is the operator's
escape hatch:
- Set `prompt_strategy = "structured_injection"` to drop
  back to the Phase 122 catalog-only block (saves the
  examples overhead).
- Set `prompt_strategy = "none"` to drop the whole
  augmentation (saves both blocks).

The block scales with the active role's
`tool_allowlist`: a role with a restricted allowlist
sees only the tools that survived the filter, not
the full registered tool set. The few-shot examples
also defensively skip examples whose target tool isn't
registered.

### Honest scope caveat carried from open doc

The Phase 122 open doc flagged two failure modes the
substrate might not fix:
- **gemma4's capability-denial prior may be
  prompt-unreachable.** If the model's training prior
  on "what AI assistants can do" dominates any in-
  prompt reinforcement, structured injection won't
  rescue it. Exit-doc will document the observed
  outcome regardless.
- **qwen3.6's verbal refusal may persist** even with
  the catalog block visible. Same reasoning: model
  prior may dominate.

Phase 6 Q5 honesty applies to RESULTS, not to
ANTICIPATION. Whatever the live verification at exit
shows, the exit doc reports it.

**Phase 122 live verification outcome:** both risks
materialized. qwen3.6 timed out on fs.write under
`structured_injection`; gemma4 explicitly refused
fs.write while the tool was literally listed in its own
system prompt. Catalog enumeration improved (qwen3.6
stopped inventing "Good Morning"; gemma4 stopped
generating 60+ fictional tools), but invocation on
command did not — the substrate ceiling sat at the model
layer. Phase 124 below attempts one more rehab against
that ceiling.

### Few-shot examples (Phase 124 default upgrade)

Phase 124 promoted qwen3 + gemma4 from
`structured_injection` to `few_shot_examples` as the
default. Same `## Tools available` catalog block as
Phase 122, **plus** a `## Example tool use` block with
2-3 worked tool-call examples (fs.read, fs.write,
memory.write — only the ones registered in the active
role's allowlist).

Each example carries explicit WRONG/RIGHT framing
directly against the gemma4-refusal pattern Phase 122
documented:

```
Operator: "Please save 'hello' to test.txt."
- You should: invoke fs.write with {"path": "test.txt",
  "content": "hello"} and report the result.
- You should NOT: respond "I don't have a tool called
  fs.write" — you DO have fs.write; it is in your tool
  list above.
```

**Why this might work where structured injection didn't.**
Phase 122 demonstrated that *assertion-of-availability*
(catalog listing) isn't enough; gemma4 refused while the
tool was right there. Few-shot examples are a different
mechanism — the model sees a *concrete worked pattern*
of "operator asks → assistant invokes → result reported,"
not just an assertion that tools exist. Few-shot prompting
is well-documented to change model behavior more reliably
than instructions. Whether it's *sufficient* against a
strong training prior is the open question Phase 124's
live verification answers.

**Honest scope risk at sign-off:** four substrate phases
deep (120 + 121 + 122 + 124). If Phase 124's examples also
fail to break through, the model-layer ceiling will need
to be named definitively — operators using local models
for tool-use workloads would have to either accept the
limitation or fall back to cloud providers.

**Operator escape hatches preserved.** Per-family
`prompt_strategy = "structured_injection"` (drops back
to Phase 122 substrate) or `prompt_strategy = "none"`
(drops all augmentation) remain operator-settable in
`[ollama.prompt_strategies]`.

**Phase 124 live verification outcome — fourth-
substrate-phase failure on actual tool invocation.**
Neither qwen3.6:27b nor gemma4:31b produced a real
`fs.write` invocation through the protocol. gemma4
explicitly refused the exact tool by exact name despite
the WRONG/RIGHT framing literally saying *"you DO have
fs.write"* — confirming the capability-denial prior is
prompt-unreachable. A glm-4.7-flash control (strategy=
none) showed the same failure mode without any Aivyx PA
substrate, ruling out "Phase 124 made things worse." See
PHASE_124.md "Live verification (Task 4)" for the full
empirical table.

**Two operator-actionable findings carried forward to
Phase 125:**

1. **qwen3 with FewShotExamples emits structurally
   correct `<tool_code>` JSON as response TEXT** rather
   than invoking via the Ollama protocol — wrong channel,
   right shape. A future textual-tool-call extraction
   substrate (planner-side parser for `<tool_code>` /
   `<tool_call>` blocks) would rescue this case. Phase
   125 candidate.

2. **gemma4's hallucinated `fs.write_file` is below
   Phase 120's default fuzzy-recovery threshold.** Jaccard
   similarity to `fs.write` is ~0.667; default threshold
   is 0.80. Operators running gemma4 can opt in to
   recovery by lowering
   `[providers] tool_name_auto_correct_threshold` to
   `0.60` — substrate already exists; just needs the
   knob turned. **This is operator-actionable today; no
   substrate work required.**

**Honest framing — local-LLM tool-use is currently
prompt-substrate-bounded.** Four substrate phases (120 +
121 + 122 + 124) have hit the same model-layer wall.
Operators with tool-use workloads should consider
cloud providers (Anthropic / OpenAI) for those workloads
specifically; local models remain useful for
conversation, drafting, and other non-tool-invoking
tasks. See PHASE_124.md exit doc for the full
recommendation.

### Textual tool-call extraction (Phase 126)

Phase 124's live verification surfaced that **qwen3.6:27b
emits structurally correct tool-call JSON in response
TEXT** rather than the protocol channel:

```text
<tool_code>
  {"name": "fs.write", "arguments": {"path": "x.txt", "content": "..."}}
</tool_code>
```

The tool name + arguments are correct; only the channel is
wrong. Phase 126 ships a planner-side extractor that:

1. Detects `<tool_code>` / `<tool_call>` wrapper blocks in
   response text when the LLM's protocol `tool_calls` array
   is empty.
2. Parses the inner JSON (permissive: accepts both
   `{"name", "arguments"}` and `{"tool", "parameters"}`
   shapes).
3. Synthesizes `ToolCallEnd`-shaped values with UUID
   call IDs and routes them through the same Phase 120
   fuzzy-recovery + Phase 101 schema validation + dispatch
   path as protocol-channel calls.
4. Records `extracted_from_text: Some(wrapper_tag)` on the
   `AuditTag::ToolCall` audit entry for forensic
   visibility.

**No operator config required.** Extraction is on by
default at the planner-substrate layer and is a no-op
for providers/models that use the protocol channel
normally (protocol `tool_calls` non-empty → existing path
runs).

**Composition with Phase 120 fuzzy-recovery.** gemma4's
observed `<tool_call>{"tool": "fs.write_file", ...}</tool_call>`
emits a hallucinated tool name. Extraction → synthesized
call → Phase 120 fuzzy-recovery at Jaccard similarity
`{fs, write}` vs `{fs, write, file}` = 2/3 ≈ 0.667. The
default threshold is 0.80, so the recovery doesn't fire
by default. **Operators running gemma4 should lower the
threshold:**

```toml
[providers]
tool_name_auto_correct_threshold = 0.60
```

With both substrates active, the gemma4 invocation chain
becomes: text → extracted `fs.write_file` → fuzzy-
recovered to `fs.write` → dispatched. The audit entry
carries **both** `extracted_from_text: Some("tool_call")`
AND `auto_corrected_from: Some("fs.write_file")` so an
auditor can see the full rescue trajectory.

**Audit-chain forensics.** The four-way forensic
distinction is now fully observable in the audit chain:

| `auto_corrected_from` | `extracted_from_text` | Meaning |
|---|---|---|
| None | None | Native protocol call with exact tool name (dominant) |
| Some | None | Protocol call with fuzzy-corrected name (Phase 120) |
| None | Some | Text-extracted call with exact tool name (qwen3 best case) |
| Some | Some | Text-extracted call AND fuzzy-corrected (gemma4 rescue path) |

Operators querying the audit chain for "where did this
tool call actually come from?" can answer with
`jq 'select(.extracted_from_text)'` / `jq
'select(.auto_corrected_from)'` filters.

**False-positive defense.** Extraction only fires when
the LLM's protocol `tool_calls` array is empty. An
operator literally pasting a `<tool_code>` block into
chat (e.g. discussing tool-call syntax) wouldn't trigger
spurious extraction because the model's response would
carry no protocol tool calls AND its text would just
quote the operator's block.

**Phase 126 live verification outcome:** verification was
amended out at Phase 126 close-out. A pre-flight
`dev-verify` pass against qwen3.6:27b on Ollama 0.24.0
surfaced that qwen3's actual emission format is
**Qwen3-Coder XML inside `<tool_call>`**, not the JSON
shape Phase 126's parser handles. The five-turn pre-flight
recorded `tool_calls_made: 0` across every turn in the
audit chain — a substrate gap, not a test failure.
Literature research (Ollama issues #14493, #14601, #14745)
confirmed the wrong-pipeline upstream wiring for qwen3.5/3.6
and surfaced ten distinct text-form tool-call formats
across the local-LLM landscape. Phase 127 closes the
parsing gap with multi-format extraction; see the next
sub-section. Full reasoning in
[`PHASE_126.md`](archive/phases/PHASE_126.md) "Research-driven amendment".

### Multi-format tool-call extraction (Phase 127)

End-users pick their local model based on their hardware:
Llama 3.x for CPU-friendly setups, Qwen3-Coder for code-
heavy work, Mistral Nemo for the midrange, Phi-4-mini for
edge devices, Gemma 3/4 for the Google-fine-tuned path,
DeepSeek R1 for reasoning. Phase 127 expands the Phase 126
textual extractor so the substrate handles whichever
model the operator picked — four new parser families plus
a hybrid family-hint architecture backed by Ollama
`/api/show`.

**Substrate-coverage matrix.** Each row is an empirical
emission format observed across the local-LLM landscape.
"Native" means Ollama's protocol channel handles it
without aivyx-pa-side extraction (`tool_calls` array arrives
populated). "Substrate" means aivyx-core's textual
extractor catches it via Phase 126/127's planner-side
fallback. "Gap" means neither path handles it today.

| Family | Training emission | Substrate (Phase 127) | Native (Ollama protocol) |
|---|---|---|---|
| Llama 3.1 / 3.2 / 3.3 | `<\|python_tag\|>[func(k=v)]` | gap (Python-call dispatch deferred) | ✅ reliable per Ollama tool-support blog |
| Mistral Nemo / Small 3.x | `[TOOL_CALLS]` JSON | gap (Mistral protocol pipeline handles this directly) | ✅ reliable |
| Qwen3 (Hermes) | `<tool_call>` + JSON `{name, arguments}` | ✅ Phase 126 | ✅ when pipeline-wiring is correct |
| **Qwen3-Coder (qwen3.5/3.6)** | `<tool_call>` + XML `<function=N><parameter=K>V</parameter></function>` | ✅ **Phase 127 Task 2** | ❌ wrong pipeline upstream (Ollama #14493) |
| DeepSeek R1 | Dynamic XML `<TOOL_NAME>...<param>V</param>...` | gap (registry-driven match deferred) | depends on version |
| **Phi-4-mini** | `<\|tool_call\|>[{name, arguments}, ...]<\|/tool_call\|>` | ✅ **Phase 127 Task 3** | ✅ in Ollama 0.5.13+ |
| **Gemma 3** | ` ```tool_code` markdown fence + Python-call | ✅ **Phase 127 Task 4** | ❌ Gemma 1/2/3 not trained for tool use |
| Gemma 4 | `<\|tool_call>call:N{k:<\|"\|>v<\|"\|>}` | gap (special-token format) | ✅ native in Ollama 0.20.0-rc1+ (#15241 fixed) |
| qwen3-Hermes-fence | `<tool_call>` + JSON `{tool, parameters}` (gemma4 historical variant) | ✅ Phase 126 | depends |
| **Bare JSON** (qwen3:32b#11662) | raw `{name, arguments}` no wrapper | ✅ **Phase 127 Task 5** (with FP guard) | ❌ not parsed |
| Tool-code JSON (Phase 124 qwen3.6 sample) | `<tool_code>` + JSON | ✅ Phase 126 | depends |

**Operator-facing summary:** if your model is in the
"Native" column with ✅, Aivyx PA works without any extraction
substrate involvement. If your model needs the
"Substrate" path (qwen3.5/3.6, Gemma 3, Phi-4-mini, or any
model emitting bare JSON), Phase 127 catches it
automatically. No operator config required for the
substrate.

**Reliable-native-protocol trio (recommended for tool-use
workloads):** Llama 3.1+ ($AIVYX_PA_MODEL=llama3.1$),
mistral-nemo, phi4-mini. Per Ollama's official tool-support
blog post + this phase's literature these models ship with
matching renderer + parser pipelines and reliably emit
structured `tool_calls`.

**Family-hint architecture.** When `provider = "ollama"`,
Aivyx PA queries `/api/show` once per model at first use and
caches the reported `details.family` string. The hint
biases the extractor's inner-shape priority — Qwen-family
models try Qwen3-Coder XML first inside `<tool_call>`,
other families use the default JSON-first order. The hint
is permissive: every parser is still tried; the family
hint just reorders which gets the first shot. Failure to
fetch `/api/show` (network down, model not yet pulled) is
silent and falls back to permissive scan. No operator
config; nothing to enable.

**Operator workaround for qwen3.5/3.6:** per Ollama issue
#14493, the `qwen3.5` family is wired to the wrong
renderer/parser pipeline upstream (`Qwen3VLRenderer` +
`Qwen3Parser`, the Hermes-style JSON pipeline) when the
model was trained on `Qwen3CoderRenderer` +
`Qwen3CoderParser` (the XML pipeline). Phase 127 Task 2
catches the XML emission — but per issue #14601, tool
**definitions** are ALSO malformed via the modelfile
template (Go struct strings instead of JSON), so the model
may not see correct schemas. Phase 127 closes the parsing
gap, not the upstream Ollama schema-rendering gap. If you
want a working Qwen tool-use path right now, install
`qwen3-coder:N` (with the parameter-size suffix) instead
of `qwen3.5:N` / `qwen3.6:N` — that model name gets
Ollama's correct upstream pipeline AND benefits from
Phase 127's substrate.

**Audit-chain forensics — wrapper-tag column.** Each
extracted call carries the wrapper-tag in
`AuditTag::ToolCall.extracted_from_text`. The Phase 127
wrapper-tag vocabulary is stable + distinct so auditors
can grep cleanly:

| `extracted_from_text` | Format |
|---|---|
| `None` | native protocol tool_call (Phase 126/127 substrate didn't fire) |
| `Some("tool_code")` | Phase 124 qwen3.6-observed `<tool_code>` + JSON shape |
| `Some("tool_call")` | Hermes-style `<tool_call>` + JSON OR Qwen3-Coder XML |
| `Some("\|tool_call\|")` | Phi-4-mini `<\|tool_call\|>` + JSON list |
| `Some("tool_code_fence")` | Gemma 3 ` ```tool_code` markdown fence + Python-call |
| `Some("(bare)")` | Phase 127 bare-JSON fallback (no wrapper detected) |

Combined with `auto_corrected_from` (Phase 120) and a new
internal `inner_format` distinguisher (`"json-name-arguments"`,
`"json-tool-parameters"`, `"qwen3-coder-xml"`,
`"json-list-name-arguments"`, `"python-call"`), the
forensic story is "what shape did the model emit, where in
the response, and did fuzzy-recovery correct it?" all
answerable via `jq` on the audit chain.

**Bare-JSON false-positive guard.** The bare-JSON parser
fires ONLY when (1) every wrapper-based parser returned
zero extractions AND (2) the entire response content
(after optionally stripping a single leading `<think>...
</think>` thinking-mode block + trimming whitespace) is
exactly one top-level JSON object matching a tool-call
shape. JSON embedded in prose, JSON followed by prose,
top-level JSON arrays, and multiple concatenated JSON
objects all drop. This is the load-bearing FP guard —
without it, operators (and models) mentioning JSON inline
in prose would trigger spurious extractions.

**Known limitations carried from Phase 127 open doc:**

- **Llama 3.x Python-call dispatch** is deferred (the
  `<|python_tag|>[func(k=v)]` format needs a Python-call
  → JSON-args translator like Task 4's but with positional
  args). Phase 128+ candidate.
- **DeepSeek dynamic-XML lookup** is deferred (parameter
  names are model-emitted, not registry-driven; needs a
  separate registry-aware path).
- **Gemma 4 special-token format** is handled by Ollama's
  native protocol pipeline (0.20.0-rc1+ via issue #15241
  fix); Phase 127 substrate doesn't duplicate.
- **Triple-backtick inside a Gemma 3 Python string** would
  prematurely close the markdown fence. Operators can
  typically work around by emitting a different fence
  language or escaping. Same posture as Phase 126 — drop
  silently rather than dispatch a wrong call.

## Tool-process sandboxing (`[sandbox]`, Phase 180)

Every `[[tool_process]]` is a separate subprocess. By default
before Phase 180 it ran with your full user identity unless you
declared an explicit `[tool_process.sandbox]` wrapper. Phase 180
adds a **bundled, secure-by-default preset**:

```toml
[sandbox]
default_backend = "auto"   # auto | bubblewrap | firejail | none
```

- **`auto`** (written into every new `aivyx-pa init` config) detects
  `bwrap` (bubblewrap) then `firejail` on `PATH` and applies a
  conservative-but-functional preset automatically: read-only
  system directories, a private `/tmp`, an isolated PID
  namespace, **`$HOME` hidden except a writable bind of the
  tool's own `~/.aivyx-pa/tool-processes/<name>/` token dir**, and
  network left on (it is already capability-gated at the IPC
  boundary, and productivity tools need it). If neither backend
  is installed, `auto` warns at startup and falls back to no
  sandbox — install `bubblewrap` (preferred) or `firejail` to get
  the protection.
- **`bubblewrap` / `firejail`** force that backend.
- **`none`** (also the in-code default when the `[sandbox]`
  section is absent) — no bundled sandbox; **existing configs are
  unchanged.**

Precedence per tool: an explicit `[tool_process.sandbox]` block
wins → then a per-tool `disable_sandbox = true` opts out → then
the global `[sandbox] default_backend`. The daemon prints the
applied posture per tool at startup (`sandboxed (bubblewrap)` or
`UNSANDBOXED`).

> **Scope.** The bundled preset applies to `[[tool_process]]`
> only. `[mcp_server]` entries — operator-configured external
> programs with unknown filesystem needs — keep the Phase 55
> explicit `[mcp_server.sandbox]` model. A tool whose data lives
> outside `~/.aivyx-pa/tool-processes/<name>/` should declare an
> explicit `[tool_process.sandbox]` block instead.

## Connecting a productivity tool — `aivyx-pa connect` (Phase 182)

The fastest way to connect a Google productivity tool (Gmail,
Calendar, Drive, Contacts) is the guided command:

```sh
aivyx-pa connect            # list connectable services + status
aivyx-pa connect gmail      # guided OAuth onboarding for Gmail
```

`aivyx-pa connect <service>` walks you through the whole thing:

1. **Google Cloud app setup** — it prints the exact steps (enable
   the API, create an OAuth client ID of type *Desktop app*, and
   the precise `http://127.0.0.1:<port>/callback` redirect URI to
   register), then prompts you to paste the **Client ID** and
   **Client secret** from the console.
2. **Writes `config.toml`** to
   `~/.aivyx-pa/tool-processes/<service>/config.toml` (`0600`) — you
   never hand-edit it. Scopes are filled by the service's own
   defaults, so you don't need to know scope URLs.
3. **Runs the consent flow** by shelling out to the tested
   per-service `aivyx-<service> auth init` (a loopback server
   catches the browser redirect and exchanges the code for
   tokens). It finds the binary as a sibling of `aivyx-pa` or on
   `PATH`, or asks for the path.
4. **Confirms** the connection and **offers to add the
   `[[tool_process]]` entry** to your `aivyx-pa.toml` so the tool is
   enabled — restart the daemon to load it.

If a Google tool is configured in `aivyx-pa.toml` but not yet
authenticated, the daemon prints the remedy at startup
(*"run `aivyx-pa connect <service>`"*).

> **Token-based services (Notion / n8n)** still use their own
> `auth` flow today (paste an integration token); a guided
> `aivyx-pa connect` path for them is a planned follow-on. The
> manual per-service setup below still works for every tool.

## Vertical packs — install a signed pack (Chapter Freight)

A **pack** adds a vertical capability (domain tools + a specialist team
roster) as a single signed file — no Rust toolchain, no source build.
Kitchen (back-of-house) is the free worked example; commercial packs
ship the same way.

```sh
# 1. Trust the publisher (one-time). The publisher gives you their
#    verifying key; add it to aivyx-pa.toml:
#      [pack]
#      trusted_publishers = ["<base64 key>"]

# 2. Look before you leap — verifies the signature and prints what the
#    pack will wire:
aivyx-pa pack inspect kitchen-0.8.0-x86_64-unknown-linux-gnu.aivyxpack

# 3. Install: verifies again, unpacks to ~/.aivyx-pa/packs/<name>/<ver>/,
#    wires the [[tool_process]] entries, and sets [team] config_path
#    ONLY if you don't already have one (never clobbers your roster).
aivyx-pa pack install kitchen-0.8.0-x86_64-unknown-linux-gnu.aivyxpack

# 4. Restart the daemon to load it, then connect the pack's data
#    source if it has one (Kitchen wants its KitchenDB credentials):
aivyx-pa connect kitchen
```

The install refuses: an untrusted or tampered signature, a bundle built
for a different platform, a pack needing a newer daemon, and any
archive entry that tries to escape the install directory. A pack is
**capability**; `aivyx-pa connect <pack>` supplies your **credentials** —
the two compose.

**Publishing a pack** (pack authors): stage `manifest.toml` + `bin/` +
`config/` (root manifest keys must come *before* `[[tool_process]]`
tables), then:

```sh
aivyx-pa pack keygen my-signing.key      # prints the operators' trust snippet
aivyx-pa pack build ./stage --key my-signing.key --out my-pack.aivyxpack
```

Bundles are deterministic per staging tree; the signature covers the
whole payload. See `docs/FREIGHT.md` for the format, and
`just pack-kitchen` in the repo for a working publisher recipe.

## External productivity integrations (Chapter F)

After three named local-LLM rehab phases (120-122), the
operator pressure redirected toward **Aivyx PA as productivity
assistant, not just chat surface**. Chapter F opens that
axis: external services (Gmail, Calendar, Drive, GitHub, …)
as first-class operator-facing capabilities, each shipped as
a separate third-party tool process per the P10 substrate
contract.

Aivyx PA core stays at the **thirteen substrate tools forever**
cap (`fs.*`, `memory.*`, `shell.exec`, `web.fetch`,
`web.post`, `git.read`, `net.dns`). P10 explicitly names
email and calendar as third-party territory; Chapter F is
the chapter that validates the third-party SDK on real
external integrations.

Each Chapter F phase ships one integration as a separate
binary the operator installs and wires via
`[[tool_process]]`. The auth substrate appropriate to that
service (OAuth for Google, PAT for GitHub, etc) is per-
integration. Per-tool capability scopes registered into the
existing capability machinery — no new core types.

### Gmail (Phase 123)

The first Chapter F integration. Ships four tools through a
single `aivyx-gmail` binary:

| Tool | Scope | What it does |
|---|---|---|
| `gmail.search` | `email.read` | Search messages via Gmail's query DSL (e.g. `from:alice is:unread`). Returns IDs + thread IDs. |
| `gmail.read` | `email.read` | Read one full message by ID. Returns headers, body text + HTML, attachment metadata (no bytes). |
| `gmail.draft` | `email.write` | Create a Gmail draft. **Safe write** — draft requires explicit Gmail-UI send by the operator. |
| `gmail.send` | `email.send` | Send a message directly. **No undo from Aivyx PA.** Trusted-tier-only by default. |

All four scopes ship in `aivyx-capability::CEILING_TRUSTED`
ONLY — SemiTrusted and Untrusted roles get zero email
scopes by default (mirrors `shell.exec` / `notify.send`
gating per Phase 62 Q2(a)). Operators who want a remote-
channel role to read mail can grant `email.read`
explicitly in the role's `capability_scopes`; the gate
makes it a conscious choice, not a default.

#### One-time operator setup

Gmail uses operator-provided OAuth (Q1a Recommended at
Phase 123 sign-off): you create your own OAuth client in
your own Google Cloud project. Aivyx PA ships no shared OAuth
app — privacy posture stays under operator control.

**1. Create a Google Cloud OAuth client:**

- Visit <https://console.cloud.google.com/>; create a new
  project (or reuse an existing one).
- Enable the Gmail API: APIs & Services → Library → search
  "Gmail API" → Enable.
- Configure the OAuth consent screen: APIs & Services →
  OAuth consent screen. Pick "External" (or "Internal" if
  you have a Workspace org); add yourself as a test user.
- Create credentials: APIs & Services → Credentials →
  Create Credentials → OAuth client ID → Application type:
  **Desktop app**. Note the `client_id` and
  `client_secret` Google issues.

**Heads-up — Google's "Sensitive scope" review.** The Gmail
scopes (`gmail.readonly`, `gmail.compose`, `gmail.send`)
are classified Sensitive by Google. In Testing mode your
OAuth client works for up to 100 manually-added test users
(your own account counts as one). To publish for general
operator use, Google requires app verification — out of
scope for self-hosted single-operator use; relevant only if
you distribute Aivyx PA to others.

**2. Write the tool-process config file:**

Create `~/.aivyx-pa/tool-processes/gmail/config.toml`:

```toml
client_id = "XXXXX.apps.googleusercontent.com"
client_secret = "GOCSPX-..."
redirect_uri = "http://127.0.0.1:8088/oauth/callback"

# Optional: narrow the requested scopes.
# Default includes gmail.readonly + gmail.compose + gmail.send.
# scopes = [
#   "https://www.googleapis.com/auth/gmail.readonly",
# ]
```

The `redirect_uri` MUST be a loopback URI (Google requires
`127.0.0.1` or `localhost` for "Desktop app" OAuth clients).
The port is your choice; `aivyx-gmail auth init` binds a
short-lived listener on that port to receive the callback.

**3. Run the OAuth flow:**

```sh
aivyx-gmail auth init
```

The CLI prints a Google consent URL; paste it into your
browser, click through the consent screen, and Google
redirects back to the loopback URI. The CLI captures the
auth code, exchanges it for tokens via Google's token
endpoint, and saves the result to
`~/.aivyx-pa/tool-processes/gmail/tokens.json` (0600 perms).

Subsequent runs of any Gmail tool will use these tokens.
The access token auto-refreshes ~60 seconds before expiry;
the refresh token persists across refreshes (Google's
typical behavior — refresh responses don't include new
refresh tokens; the on-disk one stays in place).

**4. Confirm with `auth status`:**

```sh
aivyx-gmail auth status
```

Prints granted scope, access-token expiry, refresh-
available flag. The access token is redacted to
`<N chars, …tail4>` so terminal scrollback / screen-share
can't leak it.

**5. Register the tool process in `aivyx-pa.toml`:**

```toml
[[tool_process]]
name = "gmail"
command = "aivyx-gmail"
# Optional per-tool scope overrides — operator CAN narrow,
# CANNOT widen. The daemon checks override-or-declared
# against the active role's envelope.
# [tool_process.scope_overrides]
# "gmail.send" = "email.send"  # no-op narrowing here; example
```

The daemon spawns `aivyx-gmail` at startup, performs the
handshake, and registers all four tools into the catalog.
Operators who want only some of the tools can omit them
from a role's `tool_allowlist`:

```toml
[[role]]
name = "readonly_mail_triage"
parent = "default"
tool_allowlist = ["gmail.search", "gmail.read", "memory.read", "memory.write"]
capability_scopes = ["email.read", "memory.read", "memory.write"]
```

(The above role can search + read email but not draft or
send — `email.write` + `email.send` are absent from
`capability_scopes`, so even if `gmail.draft` were on the
allowlist the capability gate would still deny it.)

**6. Revoke when finished:**

```sh
aivyx-gmail auth revoke
```

POSTs to Google's revoke endpoint and deletes the local
token file. If the remote revoke fails (network down, etc),
the local file is still removed — operators can manually
revoke at <https://myaccount.google.com/permissions> as a
fallback.

#### Operator-side troubleshooting

- **"Token refresh failed" at tool dispatch.** The refresh
  token may have been invalidated by Google (60-day
  inactivity, password change, scope change). Re-run
  `aivyx-gmail auth init`.

- **"Scope denied" returned by a tool.** The role's
  `capability_scopes` doesn't grant the tool's required
  scope, OR the role's TrustTier (e.g. SemiTrusted for a
  Telegram operator) intersects the email scopes to empty.
  Confirm the role config and tier — `email.*` lives only
  in `CEILING_TRUSTED` by default.

- **`aivyx-gmail` not found at daemon startup.** The
  `command` field in `[[tool_process]]` must be on the
  daemon's `$PATH` OR absolute. `cargo install --path
  crates/aivyx-gmail` puts the binary in
  `$CARGO_HOME/bin`; ensure that's on PATH.

- **Gmail returns 401 on the first call after a long
  pause.** Defensive — the in-memory token cache thought
  the token was fresh but Google revoked it. Re-run `auth
  init`; if recurring, check whether you changed your
  Google account password or revoked the app from the
  permissions page.

- **Drafts not threading into the original conversation.**
  Pass BOTH `in_reply_to_message_id` (RFC 5322 Message-ID
  for the email headers) AND `thread_id` (Gmail's internal
  thread placement) when calling `gmail.draft` or
  `gmail.send`. Both come from `gmail.read` (the response
  carries `thread_id` and `headers.message_id`).

#### What Phase 123 deliberately leaves to follow-on phases

- **No attachment-download tool.** `gmail.read` returns
  attachment metadata + `attachment_id` but no bytes. A
  future `gmail.attachment.read` could fetch them.
- **No label management.** `gmail.labels.list`,
  `gmail.labels.create`, `messages.modify` etc — out of
  Phase 123 scope.
- **No `gmail.delete`.** Adding a `messages.delete` tool
  would need an `email.delete` scope; deferred until
  operator pressure surfaces.
- **No shared credential vault.** Each future Chapter F
  integration (Calendar, Drive, etc) manages its own
  tokens via the same per-tool-process file pattern. A
  shared vault becomes worth doing once enough
  integrations exist to feel the duplication.

### Google Calendar (Phase 128)

Chapter F second integration. `aivyx-calendar` is a
separate binary the operator installs and wires into
`aivyx-pa.toml` via `[[tool_process]]` — same shape as the
Gmail tool process. Calendar uses the SAME Google OAuth
flow as Gmail, just a different scope. Most operators
will reuse their existing Gmail OAuth client.

#### One-time operator setup

Two paths depending on whether you already set up Gmail:

**Path A — you already have an Aivyx PA Gmail OAuth client
in your GCP project (recommended):**

1. **Enable the Calendar API** in the same GCP project
   you used for Gmail (Console → APIs & Services →
   Enable APIs → "Google Calendar API").
2. **Add the Calendar scope** to your OAuth consent
   screen: `https://www.googleapis.com/auth/calendar`
   (the broad read+write scope — narrower options below).
3. **Write
   `~/.aivyx-pa/tool-processes/calendar/config.toml`** with
   the SAME `client_id` + `client_secret` you used for
   Gmail:

   ```toml
   client_id = "XXXXXXXX.apps.googleusercontent.com"
   client_secret = "GOCSPX-..."
   redirect_uri = "http://127.0.0.1:8766/callback"
   ```

   Note the different `redirect_uri` port from Gmail's
   `8765` — each tool process binds its own loopback
   port for the auth-code callback. Add `8766` to your
   OAuth client's Authorized redirect URIs in the GCP
   Console.

4. **Run `aivyx-calendar auth init`** to grant the
   Calendar scope. The browser will show the existing
   consent screen with the new Calendar scope listed.

**Path B — you're not running Gmail and Calendar is your
first Google integration:**

Follow the Phase 123 Gmail setup steps (Console → new
GCP project → enable Calendar API → create OAuth client
→ etc) substituting Calendar for Gmail throughout.

#### Scope-narrowing options

The default `auth/calendar` scope grants read+write
across ALL calendars accessible to the authenticated
user. Operators wanting a narrower posture can supply a
`scopes` field in `config.toml`:

```toml
# Events-only (no calendar list / settings access):
scopes = ["https://www.googleapis.com/auth/calendar.events"]

# Read-only across all calendars:
scopes = ["https://www.googleapis.com/auth/calendar.readonly"]

# Read-only events only:
scopes = ["https://www.googleapis.com/auth/calendar.events.readonly"]
```

The default is the broad scope per Phase 128 sign-off —
fewer "re-auth with new scope" loops for operators. The
write tools (create / update / delete) all error with a
clear "scope not granted" message when the narrower
read-only scopes are in effect, so the failure surface is
operator-discoverable rather than silent.

#### `aivyx-pa.toml` `[[tool_process]]` registration

```toml
[[tool_process]]
name = "aivyx-calendar"
command = "/path/to/aivyx-calendar"
# The process inherits HOME for OAuth token file resolution.
inherit_env = ["HOME"]
```

The tool process advertises five tools to the daemon at
handshake:

| Tool | Capability | Description |
|---|---|---|
| `calendar.list_events` | `calendar.read` | Range query a calendar; returns event summaries with `id`, `summary`, `start`, `end`, `location`, `attendee_count`. |
| `calendar.get_event` | `calendar.read` | Fetch full event detail by ID; includes description, organizer, attendee response statuses, recurrence rule, conference data. |
| `calendar.create_event` | `calendar.write` | Create a new event; required `summary`/`start`/`end`, optional attendees/location/etc. Trusted-tier-only by default. |
| `calendar.update_event` | `calendar.write` | Partial-patch an existing event by ID. Only fields you supply are changed; everything else is preserved. Trusted-tier-only. |
| `calendar.delete_event` | `calendar.write` | Delete an event by ID. Idempotent — already-deleted events succeed with `was_already_deleted: true`. Trusted-tier-only. |
| `calendar.upcoming` | `calendar.read` | **Phase 141 + 142 + 151 + 155 + 158.** Surface imminent events with relative-time enrichment, optionally across multiple calendars. Input `{window_hours? default 24, calendar_id? OR calendar_ids? (mutually exclusive; default ["primary"]), max_results?, fuzzy_dedup? default true (Phase 155 — normalize summary; Phase 158 — sliding-window ±5min adjacency merge replaces the bucket flooring, so 10:04+10:06 merge and chains like 10:00→10:04→10:08 fold to one cluster), writable_only? default false (Phase 155 — pre-fetches calendar list and filters to owner/writer; intersects with calendar_ids if explicit, replaces if defaulted; **Phase 158** — backed by a 5-minute session cache so repeated calls in the same session skip the round trip; **Phase 171** — TTL is now operator-tunable via `CalendarClient::with_writable_calendars_cache_ttl(Duration)` builder method or `AIVYX_PA_CALENDAR_CACHE_TTL_SECS` env var, default 300s), max_concurrent? (Phase 155 — throttle parallel fan-out for rate-limited operators), min_concurrent? (Phase 158 — floor on permit count, cap 16; composes with max_concurrent as `permits = clamp(calendar_count, min, max)`; min ≤ max validated at parse time)}`. Each event has the `list_events` shape plus `starts_in_human` ("in 15 minutes", "tomorrow"), `is_imminent` (true if starts within 30 minutes), and `calendar_id` (Phase 142 traceability). When `calendar_ids` has multiple entries the per-calendar requests run **in parallel** (Phase 151) — or throttled via `max_concurrent` (Phase 155) / floored via `min_concurrent` (Phase 158) — and the merged results are **deduped** (Phase 151 exact / Phase 155 fuzzy / Phase 158 sliding-window) before the sort + cap. LLM-ergonomic shape for "what's coming up" / "do I have anything today" prompts. |
| `calendar.list_calendars` | `calendar.read` | **Phase 142 + 151.** Enumerate the calendars the operator has access to. No arguments. Returns `{ calendars: [{ id, summary, is_primary, access_role, can_read, can_write }] }` where `access_role` is one of `owner`/`writer`/`reader`/`freeBusyReader` from Google. **Phase 151** adds derived booleans: `can_read` is true for `owner`/`writer`/`reader` (event content visible); `can_write` is true for `owner`/`writer`. `freeBusyReader` is `(can_read=false, can_write=false)` — busy times visible but event content isn't. Use this once per conversation so the agent can pass concrete IDs to `calendar.upcoming` (via `calendar_ids`) or `calendar.list_events` (via `calendar_id`). |

#### Per-role capability grants

`calendar.read` and `calendar.write` default to
Trusted-tier-only at the ceiling level (matches the
email.* / web.search third-party-tool-process gating
pattern). Operators who want to grant Calendar access to
a non-Trusted role can do so via `capability_scopes` in
that role's `aivyx-pa.toml` entry:

```toml
[[role]]
name = "calendar-assistant"
trust_tier = "SemiTrusted"
capability_scopes = ["calendar.read", "calendar.write"]
```

Narrower grants (just read; just write certain calendars
via scope qualifier) are supported by the existing
capability machinery — same as for the email.* /
web.search bases.

#### Operator-side troubleshooting

- **`auth init` opens the wrong consent screen.** You
  probably forgot to enable the Calendar API on the GCP
  project. Console → APIs & Services → "+ ENABLE APIS
  AND SERVICES" → search "Google Calendar API".
- **`auth init` succeeds but `calendar.list_events`
  returns "Insufficient Permission".** The Calendar scope
  wasn't requested at auth time. Re-run `aivyx-calendar
  auth init` after confirming
  `auth/calendar` is in your `scopes` config (or you're
  using the default).
- **`calendar.update_event` returns "Not Found"** even
  with a valid event_id. The event_id may belong to a
  calendar you don't have write access to. Use
  `calendar.list_events` first with the right
  `calendar_id` to confirm visibility, then re-fetch
  with `calendar.get_event` to see the
  authoritative event_id (sometimes recurring-event
  instance IDs differ from the source event's ID).
- **`calendar.delete_event` returns `was_already_deleted:
  true`** when you expected a fresh delete. Someone else
  (or another tool call) already deleted it. The result
  is still "the event is gone" so the post-condition
  holds; check the audit chain for prior deletions.
- **Recurring events return many entries.** The
  `singleEvents=true` query param (always on in
  `calendar.list_events`) expands recurrences. If you
  want the source recurring-event template, pass the
  recurring event's ID directly to
  `calendar.get_event` — the response will include the
  `recurrence` array (RRULE strings).

#### What Phase 128 deliberately leaves to follow-on phases

- **No free/busy query.** A `calendar.freebusy` tool that
  queries availability across multiple calendars in one
  call would be the natural Phase 129+ candidate;
  deferred until operator pressure surfaces.
- **No calendar.list (calendar inventory).** The
  authenticated user's calendar list is its own API
  surface; `calendar.list_events` defaults to `primary`
  which is enough for most operator flows.
- **No batch operations.** Each tool does one API call;
  bulk create/update/delete would need a separate
  `calendar.batch.*` substrate. Out of scope for Phase
  128; operator can compose via multiple sequential
  tool calls.
- **OAuth substrate still inline-copied per Phase 128
  Q2a.** Two in-tree copies (gmail + calendar); the
  lift to a shared `aivyx-google-oauth` crate triggers
  at N=3 (next Google integration: Drive / Photos /
  Sheets / etc).

  **Phase 129 update:** Drive shipped as Chapter F #3
  and Phase 129 Q1a Recommended bundled the OAuth lift
  in the same phase. Both gmail and calendar now
  consume the shared `aivyx-google-oauth` substrate;
  the inline-copy posture is over. Operators don't see
  this — public APIs preserved across the lift.

### Google Drive (Phase 129)

Chapter F third integration. Same OAuth flow as Gmail
and Calendar (different scope: `auth/drive`); same
operator setup pattern.

#### One-time operator setup

Three paths depending on your existing Aivyx PA Google
integrations:

**Path A — you already have Gmail and/or Calendar
configured (recommended):**

1. **Enable the Drive API** in the same GCP project
   (Console → APIs & Services → "+ ENABLE APIS AND
   SERVICES" → "Google Drive API").
2. **Add the Drive scope** to your OAuth consent
   screen: `https://www.googleapis.com/auth/drive`
   (broad read+write; narrower options below).
3. **Add a new redirect URI** to your OAuth client in
   the GCP Console: `http://127.0.0.1:8767/callback`
   (gmail uses 8765, calendar 8766; drive uses 8767).
4. **Write
   `~/.aivyx-pa/tool-processes/drive/config.toml`** with
   the SAME `client_id` + `client_secret` as your
   other integrations:

   ```toml
   client_id = "XXXXXXXX.apps.googleusercontent.com"
   client_secret = "GOCSPX-..."
   redirect_uri = "http://127.0.0.1:8767/callback"
   ```

5. **Run `aivyx-drive auth init`** to grant the Drive
   scope.

   **Phase 159 — re-auth required for existing operators.**
   `DEFAULT_DRIVE_SCOPES` now includes
   `auth/drive.activity.readonly` (for the new
   `drive.recent_activity` tool). Operators upgrading
   from a pre-Phase-159 install must re-run
   `aivyx-drive auth init` once so Google grants the
   new scope; pre-existing tokens stay valid for the
   other Drive tools but `drive.recent_activity` will
   surface a 403 / "insufficient permissions" until
   the re-auth completes.

**Path B / C** — follow the Gmail or Calendar setup
substituting Drive throughout.

#### Scope-narrowing options

The default `auth/drive` scope grants read+write
across all files the user can access. Narrower
options:

```toml
# Only files created by Aivyx PA (best least-privilege
# posture for write workflows; Aivyx PA can't see
# pre-existing files):
scopes = ["https://www.googleapis.com/auth/drive.file"]

# Read-only across all files:
scopes = ["https://www.googleapis.com/auth/drive.readonly"]

# Read-only metadata only (no content download):
scopes = ["https://www.googleapis.com/auth/drive.metadata.readonly"]
```

Default is the broad scope per Phase 129 sign-off —
fewer "re-auth with new scope" loops. Write tools
(`drive.create_folder`, `drive.upload_file`,
`drive.delete_file`) fail with a clear "scope not
granted" message when narrower scopes are in effect.

#### `aivyx-pa.toml` `[[tool_process]]` registration

```toml
[[tool_process]]
name = "aivyx-drive"
command = "/path/to/aivyx-drive"
inherit_env = ["HOME"]
```

The tool process advertises seven tools to the daemon
at handshake:

| Tool | Capability | Description |
|---|---|---|
| `drive.search` | `drive.read` | Query files via Drive's DSL (e.g. `name contains 'budget'`, `mimeType = '...'`). Returns metadata summaries. |
| `drive.get_metadata` | `drive.read` | Full metadata for one file by ID (description, owner, version, app_properties, etc). |
| `drive.list_folder` | `drive.read` | Enumerate a folder's direct children. Default `folder_id` is `"root"`. |
| `drive.create_folder` | `drive.write` | Create a new folder. Trusted-tier-only. |
| `drive.download_file` | `drive.read` | Fetch file content as base64. 10 MB inline cap; above the cap returns metadata-only with `content_truncated: true`. Google-native types (Docs/Sheets/Slides) use the export endpoint. |
| `drive.upload_file` | `drive.write` | Create a new file with content. Multipart upload; 10 MB cap. Trusted-tier-only. |
| `drive.delete_file` | `drive.write` | Permanently delete a file or folder. Idempotent on already-deleted (returns `was_already_deleted: true`). Trusted-tier-only. NOTE: this is permanent delete, not move-to-trash. |
| `drive.recent_files` | `drive.read` | **Phase 145 + 148 + 153 + 157 + 160.** Operator-owned files modified in the last N days. Input `{window_days? default 7 (max 365), max_results? default 25 (max 100), include_trashed? default false, parent_folder_id? (Phase 148 — direct children unless `recursive: true`), recursive? (Phase 153 — walks parent_folder_id's subtree, level-parallel BFS via Phase 157, default max_depth 5 / max_folders 100), recursive_max_depth? (Phase 157 — override default depth; upper bound 20), recursive_max_folders? (Phase 157 — override default folder cap; upper bound 1000), walk_max_concurrent? (Phase 160 — Semaphore-backed throttle on the recursive walk's parallel fan-out; default unlimited, upper bound 32; useful for rate-limited operators hitting 429s on wide trees), drive_id? (Phase 153 — scopes to a specific Shared Drive; Phase 157 — also threads through the recursive walk's child-folder queries)}`. Returns `{files: [...], next_page_token, window_days}` sorted by modifiedTime desc. Cognitive shape: "what did I work on this week." |
| `drive.recent_changes` | `drive.read` | **Phase 145 + 148 + 153 + 157 + 160.** Any accessible file modified in the last N hours. Input `{window_hours? default 24 (max 720 = 30 days), max_results? default 25 (max 100), include_trashed? default false, parent_folder_id?, recursive? (Phase 153 — same tree-walk semantics as drive.recent_files; Phase 157 — level-parallel), recursive_max_depth? (Phase 157 — upper bound 20), recursive_max_folders? (Phase 157 — upper bound 1000), walk_max_concurrent? (Phase 160 — same Semaphore throttle as drive.recent_files; default unlimited, upper bound 32), drive_id? (Phase 153 — scopes to a specific Shared Drive; Phase 157 — also threads through the recursive walk)}`. No owner filter — surfaces collaborator edits and shared docs. Cognitive shape: "what changed in my Drive today." |
| `drive.list_drives` | `drive.read` | **Phase 148.** Enumerate the Shared Drives (formerly Team Drives) the operator is a member of. No arguments. Returns `{drives: [{id, name, created_at}]}`. Use this once per conversation so the agent knows which shared drive IDs exist; pass specific IDs to `drive.search` via its query DSL (`'<drive_id>' in parents` + `corpora=drive`). Mirrors `calendar.list_calendars` from Phase 142. |
| `drive.recent_activity` | `drive.read` | **Phase 159 + 167 + 169.** Recent activity events on Drive items visible to the operator — who edited, who shared, who renamed, who commented. Backed by the Drive Activity API (separate googleapis subdomain, separate `drive.activity.readonly` OAuth scope). Input `{window_hours? default 24 (max 720 = 30 days), max_results? default 25 (max 100), action_type_filter? (Phase 167 — array of action types like ["edit", "create"]; supports edit / create / rename / delete / move / comment / permissionChange / restore / reference / settingsChange), consolidation? (Phase 167 — "legacy" default matches Drive UI; "none" returns un-consolidated events), parent_folder_id? (Phase 167 — scope via Activity API's `ancestorName`; always recursive), actor_email_filter? (Phase 169 — case-insensitive substring match on actor_email; applied POST-fetch since the Activity API DSL has no native actor predicate, so the filter operates after max_results truncation)}`. Returns `{activities: [{timestamp, action_type, target_title, target_id, actor_email}], count, window_hours}` where `count` reflects post-filter cardinality. Distinct from `drive.recent_changes`: one record per *event* (an edit, a rename, a comment), not one record per *file*. Re-auth required on first install after Phase 159 — see the auth init section above. |

#### Per-role capability grants

`drive.read` and `drive.write` default to Trusted-tier-
only at the ceiling level (matches the email.* /
calendar.* / web.search pattern). Non-Trusted role
grants:

```toml
[[role]]
name = "drive-assistant"
trust_tier = "SemiTrusted"
capability_scopes = ["drive.read", "drive.write"]
```

For read-only access (the most common operator-grant
posture given the breadth of files the agent might
see), `capability_scopes = ["drive.read"]` alone gives
search/get/list/download without any write surface.

#### Operator-side troubleshooting

- **`download_file` returns "content_truncated: true"
  for a file you expected.** Decoded content size
  exceeds the 10 MB inline cap. Workarounds: narrow
  to a fragment (for Google Docs, request a smaller
  export mime via `export_mime_type`); request
  metadata via `drive.get_metadata` first to confirm
  the size; or wait for the Phase 130+ streaming
  substrate.
- **`download_file` on a Google Doc returns weird
  content.** Default export is PDF for Docs; you may
  want `text/plain` instead via `export_mime_type:
  "text/plain"`. Similarly Sheets default to `text/csv`
  but `application/pdf` or
  `application/vnd.openxmlformats-officedocument.spreadsheetml.sheet`
  may be more useful.
- **`upload_file` returns "decoded content size N
  exceeds inline cap".** Same 10 MB limit. Same
  workaround story.
- **`delete_file` removes the wrong file.** Drive's
  DELETE is PERMANENT, not move-to-trash. Confirm with
  `drive.get_metadata` before calling `delete_file` for
  files you can't easily replace. There is no
  `drive.trash` tool in Phase 129; operators wanting
  move-to-trash semantics use the Drive UI.
- **Inherited Drive files (shared from another account)
  may have unexpected behavior.** Sharing-permission
  semantics aren't surfaced by Phase 129's tool surface;
  use the Drive UI to inspect / modify sharing.

#### What Phase 129 deliberately leaves to follow-on phases

- **No `drive.update` / rename / move.** Updating
  metadata or moving files between folders is the
  natural Phase 130+ candidate; out of Q2b scope.
- **No `drive.trash` / `drive.untrash`.** Drive's
  trash-and-restore semantics are distinct from
  permanent delete; deferred.
- **No `drive.share` / permissions.** Sharing posture
  changes are sensitive; the Drive UI is the right
  surface for now.
- **No resumable upload.** Above the 10 MB cap is the
  Phase 130+ streaming-substrate trajectory.
- **No batch operations.** Each tool does one API
  call.

#### Phase 129 substrate-lift note (operator-facing)

`aivyx-gmail`, `aivyx-calendar`, and `aivyx-drive` now
all consume the shared `aivyx-google-oauth` substrate
internally. Operator config files
(`~/.aivyx-pa/tool-processes/{gmail,calendar,drive}/config.toml`)
remain per-service; no operator-side change. The lift
is documented honestly in
`docs/PHASE_129.md` for development-side audit trails.

### Notion (Phase 130 — Chapter F #5)

Chapter F's first non-Google + first non-OAuth integration.
Uses Notion's **Integration token** auth — much simpler
than OAuth (no callback flow, no token refresh, no token
storage on disk beyond the operator's config file).

#### One-time operator setup

1. **Create a Notion integration:**
   Settings → My integrations → New integration →
   Internal integration → name it something like
   "Aivyx PA" → save.
2. **Copy the Internal Integration Token** that Notion
   shows (format: `ntn_XXXXXXXX` or older
   `secret_XXXXXX`).
3. **Write
   `~/.aivyx-pa/tool-processes/notion/config.toml`:**

   ```toml
   notion_token = "ntn_XXXXXXXXXXXX"
   ```

4. **Verify the token works:**

   ```bash
   $ aivyx-notion auth check
   aivyx-notion auth check: OK — token authenticated as bot `Aivyx PA`
   ```

#### **Critical UX quirk: share pages with the integration**

Notion integrations DON'T have implicit access to your
workspace content. After setting up the integration you
must explicitly share each page or database you want
Aivyx PA to see:

- **Via Notion's UI:** open the page → click "Share" →
  "Invite" → search for your integration's name →
  select it. Repeat for each page/database. Sharing
  cascades to child pages, so sharing a parent shares
  its tree.

Without this step, `notion.search` returns empty
results and `notion.get_page` returns the
"page or database not accessible — operator may need to
share it" error. The error message points operators at
this step directly.

#### `[[tool_process]]` registration

```toml
[[tool_process]]
name = "aivyx-notion"
command = "/path/to/aivyx-notion"
inherit_env = ["HOME"]
```

#### Per-tool capability table

| Tool | Capability | Description |
|---|---|---|
| `notion.search` | `notion.read` | Global search across shared content. Substring match on titles. Cursor-based pagination. |
| `notion.get_page` | `notion.read` | Full page payload: properties + block tree (top-level; nested blocks not recursively fetched — call get_page recursively on `has_children: true` blocks). |
| `notion.list_database` | `notion.read` | Query a database with Notion's filter/sort DSL. Filter shapes passed verbatim per Notion's API. |
| `notion.create_page` | `notion.write` | Create a new page under a `page_id` or `database_id` parent. Properties + optional children blocks. Trusted-tier-only. |
| `notion.append_blocks` | `notion.write` | Append blocks to an existing page. Trusted-tier-only. |
| `notion.update_page_properties` | `notion.write` | Patch property values on a page (only present keys are updated). Trusted-tier-only. |
| `notion.archive_page` | `notion.write` | Archive (Notion's "delete") a page; idempotent; recoverable via Notion's Trash menu for ~30 days. Trusted-tier-only. |

#### Per-role capability grants

`notion.read` and `notion.write` are Trusted-tier-only by
default (Chapter F precedent). Grant via:

```toml
[[role]]
name = "notion-assistant"
trust_tier = "SemiTrusted"
capability_scopes = ["notion.read"]  # read-only
```

#### Operator-side troubleshooting

- **`notion.search` returns empty results / `get_page`
  returns "not_shared":** the integration isn't shared
  with the relevant content. Re-check sharing in
  Notion's UI.
- **`notion.create_page` fails with "validation_error":**
  the `properties` shape must match the parent
  database's schema. Pull
  `notion.list_database` first to see the actual property
  shapes Notion expects.
- **`notion.archive_page` succeeds but `was_already_archived:
  true`:** the page was already archived (by someone
  else, or by a prior call). The end state matches
  intent so this isn't an error — but the audit chain
  shows the distinction.
- **Rich-text gets flattened in get_page:** by design.
  Notion's `rich_text` arrays carry per-segment
  formatting (bold/italic/links/colors); the tool
  flattens to a single `plain_text` string for LLM
  ergonomics. Operators wanting full rich-text fidelity
  bypass aivyx-notion and use Notion's API directly.

#### What Phase 130 deliberately leaves to follow-on phases

- **No nested block recursion** in `get_page`. Operators
  walk `has_children: true` blocks via further get_page
  calls. A `notion.get_blocks_tree` substrate could ship
  in Phase 131+ if pressure surfaces.
- **No file/attachment uploads.** Notion supports
  inline files; not in Phase 130's tool surface.
- **No comments / mentions / page-history.** Out of
  scope for the read+write CRUD MVP.

### Obsidian (Phase 130 — Chapter F #6)

Chapter F's first integration with **no external API** —
operates on filesystem reads/writes under your configured
vault directory. Markdown-aware (parses frontmatter,
extracts `[[wikilinks]]` and `#tags`).

#### One-time operator setup

1. **Note your vault's absolute path** (e.g.,
   `~/Documents/MyVault` → `/Users/me/Documents/MyVault`).
2. **Write
   `~/.aivyx-pa/tool-processes/obsidian/config.toml`:**

   ```toml
   vault_path = "/absolute/path/to/MyVault"
   ```

   Must be an absolute path; the config loader rejects
   relative paths.

3. **Verify the vault is accessible:**

   ```bash
   $ aivyx-obsidian auth check
   aivyx-obsidian auth check: OK
     config: "/Users/me/.aivyx-pa/tool-processes/obsidian/config.toml"
     vault root: "/Users/me/Documents/MyVault"
   ```

#### **Critical safety: path-traversal protection**

Every tool operation resolves operator-supplied paths
via a load-bearing guard before any I/O:

- **Absolute paths in tool inputs are rejected** at the
  input layer.
- **`..` path components are rejected** at the input
  layer (before canonicalization, so operators see
  "rejected `..`" rather than a downstream error).
- **Symlinks are canonicalized and verified to point
  inside the vault.** An operator-created symlink
  pointing OUTSIDE the vault is rejected — without
  this, an agent could read arbitrary filesystem
  locations via a vault-relative path.
- **Operators should ALSO register the binary in
  `[[tool_process]]` with an `allowed_paths` constraint
  scoped to the vault directory** as a belt-and-
  suspenders posture. The substrate guard is the
  primary defense; the daemon-side sandbox is the
  fallback.

#### `[[tool_process]]` registration

```toml
[[tool_process]]
name = "aivyx-obsidian"
command = "/path/to/aivyx-obsidian"
inherit_env = ["HOME"]
# Belt-and-suspenders sandbox:
allowed_paths = ["/absolute/path/to/MyVault"]
```

#### Per-tool capability table

| Tool | Capability | Description |
|---|---|---|
| `obsidian.search` | `obsidian.read` | Recursive vault walk + grep-style line-by-line search. Optional tag + frontmatter filter. Skips dotfile dirs (`.obsidian`). |
| `obsidian.get_note` | `obsidian.read` | Full note payload: content, frontmatter_raw (YAML between top `---` markers), body, wikilinks (raw link targets), tags. |
| `obsidian.list_folder` | `obsidian.read` | List markdown notes in a vault subdirectory. Optional recursive. |
| `obsidian.create_note` | `obsidian.write` | Create a new note. Refuses overwrite (use update_note). Optional create_parents. Trusted-tier-only. |
| `obsidian.update_note` | `obsidian.write` | Modify existing note: mode = `"replace"` or `"append"`. Trusted-tier-only. |
| `obsidian.delete_note` | `obsidian.write` | **Permanent** delete (no trash). Idempotent on already-missing. Trusted-tier-only. |

#### Markdown semantics

- **Frontmatter:** the YAML between the top `---`
  markers is returned as the literal text in
  `frontmatter_raw`. Operators YAML-parse LLM-side if
  they care about specific fields. The substrate's
  lightweight frontmatter scanner supports `key: value`
  substring matching in `obsidian.search`'s
  `frontmatter_key` + `frontmatter_value` filter
  without pulling a YAML dep.
- **Wikilinks:** `[[Page Name]]` and `[[Page Name|display
  text]]` link targets are extracted (display text
  dropped, deduped, insertion-order preserved). **No
  fuzzy resolution to actual vault files** — operators
  wanting to find the target call `obsidian.search` or
  `obsidian.list_folder` to disambiguate. Wikilink
  fuzzy resolution is a Phase 131+ candidate.
- **Tags:** `#tag` tokens are extracted from the body
  (whitespace-bounded; alphanumeric + `-` + `_` + `/`
  inner chars; nested-tag form `#projects/aivyx`
  supported). Headings (`# Title`) don't trigger
  because of the space after `#`.

#### Per-role capability grants

```toml
[[role]]
name = "obsidian-reader"
trust_tier = "SemiTrusted"
capability_scopes = ["obsidian.read"]  # read-only access
```

#### Operator-side troubleshooting

- **"path escapes the vault root"** — input contained
  `..` or an absolute path or a symlink to outside the
  vault. Fix the path or move the symlink target inside
  the vault.
- **`obsidian.search` returns nothing** — narrow with
  `tag` or `frontmatter` filters; widen `q` (it's
  case-insensitive substring); check `folder` arg.
- **`obsidian.create_note` says "file already exists"**
  — use `obsidian.update_note` (mode `"replace"` or
  `"append"`) instead.
- **`obsidian.delete_note` is permanent.** No trash.
  Recover via your filesystem backups (Time Machine,
  Snapshots, git) or your Obsidian Sync history if
  configured. Aivyx PA doesn't replicate Obsidian's UI
  trash semantics.

#### What Phase 130 deliberately leaves to follow-on phases

- **No wikilink fuzzy resolution.** Returns raw link
  targets; operator-driven follow-up.
- **No full-text index.** Search is linear scan;
  practical for vaults up to ~thousands of files.
- **No file-system watching.** Tools see the vault
  state at call time.
- **No image/PDF/canvas handling.** Only `.md` files
  are scanned by `search` / `list_folder`.

### n8n (Phase 131 — Chapter F #7)

n8n workflow automation. First Chapter F integration
with an **operator-supplied base URL** — n8n is most
commonly self-hosted, so operators point the tool
process at their own instance (`http://localhost:5678`,
`https://n8n.example.com`, etc.). The crate constructs
every request as `{n8n_base_url}/api/v1/<path>` and
authenticates with the n8n-specific `X-N8N-API-KEY`
header (not Bearer).

#### Operator setup

1. **Build the binary:**

   ```bash
   $ cargo build --release -p aivyx-n8n
   ```

   Produces `target/release/aivyx-n8n`.

2. **Create an n8n API key.** In your n8n web UI, go
   to Settings → API → "Create an API key". Copy the
   key (it's shown only once). Modern n8n versions
   support API keys natively for both Community and
   Cloud editions.

3. **Write `~/.aivyx-pa/tool-processes/n8n/config.toml`:**

   ```toml
   n8n_base_url = "https://n8n.example.com"
   n8n_api_key = "ntn_XXXX..."
   ```

   No trailing slash on the base URL — the crate
   appends `/api/v1/<path>` itself, and trims any
   trailing slashes defensively.

4. **Verify offline + online:**

   ```bash
   $ aivyx-n8n auth status
   aivyx-n8n auth status: OK
     config: "/Users/me/.aivyx-pa/tool-processes/n8n/config.toml"
     base_url: https://n8n.example.com
     api_key: present (non-empty)
     next: run `aivyx-n8n auth check` to ping the instance

   $ aivyx-n8n auth check
   aivyx-n8n auth check: OK — API key accepted; instance reachable
   ```

   If the check returns 401/403, the key was rejected
   — re-copy it from n8n's Settings → API and check
   for whitespace at either end of the config file's
   `n8n_api_key`.

5. **Register the binary in Aivyx PA's `config.toml`:**

   ```toml
   [[tools]]
   name = "aivyx-n8n"
   command = "/path/to/aivyx-n8n"
   ```

   The binary spawns into IPC-loop mode when invoked
   with no arguments — same multi-tool harness shape
   as every other Chapter F tool process.

#### The ten-tool surface (Phase 131 Q1c)

| Tool | Scope | Notes |
|---|---|---|
| `n8n.list_workflows` | `n8n.read` | Filters: `active`, `name`, `tags`. Pagination cursor. Returns projected summaries — call `get_workflow` for the full payload. |
| `n8n.get_workflow` | `n8n.read` | Full definition: nodes, connections, settings, staticData, pinData. Use before update/execute. |
| `n8n.list_executions` | `n8n.read` | Filters: `workflow_id`, `status` (success/error/waiting). Per-node data payload deliberately stripped — call `get_execution` to fetch it. |
| `n8n.get_execution` | `n8n.read` | Single execution by id. Optional `include_data` to surface per-node input/output payloads (large). |
| `n8n.execute_workflow` | `n8n.write` | Trigger a one-off run. Optional `input_data` forwarded to the trigger node. Returns the execution id; poll via `get_execution`. Trusted-tier-only. |
| `n8n.activate_workflow` | `n8n.write` | Wire up triggers. Idempotent (`was_already_active`). Trusted-tier-only. |
| `n8n.deactivate_workflow` | `n8n.write` | Pause triggers without deleting. Idempotent (`was_already_inactive`). Trusted-tier-only. |
| `n8n.create_workflow` | `n8n.write` | Create from full definition. **Inactive by default** — operator second checkpoint before triggers fire. Highest blast radius in the surface. Trusted-tier-only. |
| `n8n.update_workflow` | `n8n.write` | Full replacement (PUT). Active workflows take effect immediately; for staged rollout use deactivate → update → activate. Trusted-tier-only. |
| `n8n.delete_workflow` | `n8n.write` | **Permanent** delete (no trash). Execution history also removed. Idempotent (`was_already_missing`). Trusted-tier-only. |

#### Per-role capability grants

```toml
[[role]]
name = "n8n-monitor"
trust_tier = "SemiTrusted"
capability_scopes = ["n8n.read"]  # read-only — listings + payload inspection

[[role]]
name = "n8n-operator"
trust_tier = "Trusted"
capability_scopes = [
    "n8n.read",
    "n8n.write:wf-known-good-id",   # narrow write delegation
]
```

Per-workflow attenuation rides the SimpleGlob
dispatch shape (same as `role.switch` /
`notify.send` target names): `n8n.write:wf-1` grants
writes only to workflow id `wf-1`, while bare
`n8n.write` grants every workflow.

#### Operator-side troubleshooting

- **`auth check` returns 401/403.** Key rejected.
  Re-copy from n8n's Settings → API. Older
  Community-edition installs (< 1.x) may not have the
  public API endpoint enabled; upgrade or set the
  `N8N_PUBLIC_API_DISABLED=false` environment
  variable on the n8n side.
- **`auth check` reports a transport error.** The
  base URL is unreachable from the operator's
  machine. Verify the n8n instance is up, the URL is
  spelled correctly, and (for HTTPS-behind-LAN
  setups) the cert is trusted by the OS keychain.
- **`n8n.execute_workflow` 404s on a workflow the LLM
  just saw via list_workflows.** The workflow may
  have been deleted between calls, or the n8n public
  API on the operator's version doesn't support
  manual execution. Confirm via `n8n.get_workflow`;
  if that succeeds, the issue is the execute
  endpoint — consider invoking the workflow via its
  webhook URL instead.
- **`n8n.delete_workflow` is permanent.** No trash.
  Recover from your n8n database backup or save the
  definition via `n8n.get_workflow` before deleting.

#### What Phase 131 deliberately leaves to follow-on phases

- **No credentials management.** n8n credentials
  (which workflows bind to) are not readable or
  writable through this surface — the public API
  exposes them with a separate trust model.
- **No tag CRUD.** Read-only tag visibility via
  `list_workflows` projection.
- **No webhook URL discovery.** Operators read
  webhook URLs from n8n's UI or from the
  `get_workflow` node payload.
- **No execution log streaming.** `get_execution`
  returns a snapshot; long-running workflows must be
  polled.

### Building a new Chapter F integration (Phase 132 substrate)

After Phase 132 lifted the `auth_cli` shape into the
shared `aivyx-auth-cli` crate, a new Chapter F
third-party tool process picks up the substrate
instead of reimplementing it. The substrate owns:

- `BinaryMode { Help, Auth(AuthMode), IpcLoop }` and
  `AuthMode { Status, Check }` — every consumer's
  `auth <subcommand>` CLI surface.
- `parse_cli_args(argv, binary_name)` — argument
  parser threaded with the binary name so error
  messages carry the correct `aivyx-<service>`
  prefix.
- `ConfigFileError { NotFound, Io, Parse }` — the
  IO+parse layer. Service-specific validation errors
  (e.g. "API key must be non-empty") live on the
  consumer side as a separate enum that composes via
  `#[error(transparent)] Substrate(...)`.
- `default_config_path(service_subdir)` — computes
  `$HOME/.aivyx-pa/tool-processes/<subdir>/config.toml`.
- `load_toml<T>` — generic TOML load.
- `StatusReport` / `CheckReport` — Display-aware
  report types with `ok` / `fail` constructors.

A new integration's `auth_cli/` shrinks to:

1. **`cli.rs`** (~50 LoC): re-export the substrate's
   `BinaryMode` / `AuthMode`, define a per-service
   `parse_cli_args_from(argv)` that calls
   `aivyx_auth_cli::parse_cli_args(argv, "aivyx-<svc>")`,
   and a service-specific `help_text()` string.
2. **`config_file.rs`** (~80 LoC): a wrapper enum
   `ConfigFileError { Substrate(...), <service-
   specific variants> }` and a `load_config` that
   calls `aivyx_auth_cli::load_toml` then runs
   service-specific validation.
3. **`status.rs`** (~80 LoC): `run_auth_status`
   (offline, returns `Result<StatusReport, _>`) and
   `run_auth_check` (online, returns `CheckReport`).

Three working examples in the tree:
[`aivyx-notion`](../crates/aivyx-notion/src/auth_cli/),
[`aivyx-obsidian`](../crates/aivyx-obsidian/src/auth_cli/),
[`aivyx-n8n`](../crates/aivyx-n8n/src/auth_cli/). Pick
whichever shape (token-only / path-only / base-URL
plus token) matches the new service.

## Operator-facing personal assistant capabilities (Chapter G)

After Chapter F #1 (Gmail) shipped and the Phase 124 exit
named the local-LLM-rehab axis as exhausted, the operator
framing on Chapter G was load-bearing:

> "There is little point giving the Aivyx Agent Channels if
> it's still unable to actually do jobs that a Personal
> Assistant should be able to do."

Chapter G fills operator-facing tool surface gaps. Different
from Chapter F (specific external services like Gmail
through their APIs); Chapter G is **broader operator
capability** — web search, task tracking, monitoring, future
tools like calendar reminders, expense tracking, etc.

Per P10 + P11 + P12, every Chapter G tool ships as a third-
party tool process. Aivyx PA core stays at the thirteen-tools-
forever cap. Chapter G reuses Phase 123's substrate (multi-
tool harness + per-tool-process config + per-tool-process
file storage) without architectural additions.

### Personal Assistant Tool Bundle (Phase 125)

The first Chapter G integration ships **eight tools through
a single `aivyx-toolkit` binary**:

| Category | Tools | Scope |
|---|---|---|
| Web search | `web.search` | `web.search` |
| TODO tracking | `task.create`, `task.list`, `task.complete`, `task.delete` | `task.read` / `task.write` |
| Health monitoring | `health.check.add`, `health.check.list`, `health.check.recent_changes`, `health.check.remove` (Phase 147) | `health.read` / `health.write` |
| Budget tracking (Phase 143 + 144 + 149 + 150) | `budget.record`, `budget.summary`, `budget.update`, `budget.delete`, `budget.trend`, `budget.categories` | `budget.read` / `budget.write` |

All seven scopes ship in `aivyx-capability::CEILING_TRUSTED`
ONLY by default. SemiTrusted and Untrusted roles get zero
toolkit scopes by default (same gating as `shell.exec` /
`notify.send` / `email.*` per Phase 62 Q2(a)). Operators who
want narrow access from a remote channel grant individual
bases via `capability_scopes` on the role.

#### One-time operator setup

**1. Build / install the toolkit binary.**

From the workspace root:

```sh
cargo install --path crates/aivyx-toolkit
# Installs `aivyx-toolkit` to $CARGO_HOME/bin (default
# ~/.cargo/bin); ensure it's on $PATH for the daemon.
```

**2. (Optional) Configure web.search via Brave Search.**

`web.search` requires a Brave Search API key. Skip if you
won't use web search.

- Get a free key at <https://api.search.brave.com/>. The
  free tier allows 2000 queries/month at the time of
  writing.
- Create `~/.aivyx-pa/tool-processes/toolkit/config.toml`:

```toml
[brave_search]
api_key = "BSA-..."
```

(The other tools — `task.*` and `health.check.*` — need NO
external credentials.)

**3. Register the tool process in `aivyx-pa.toml`:**

```toml
[[tool_process]]
name = "toolkit"
command = "aivyx-toolkit"
```

The daemon spawns `aivyx-toolkit` at startup, performs the
handshake, and registers all eight tools into the catalog.

**4. Grant the scopes on the role(s) that will use them.**

The Trusted-tier-only default means even a Local-channel
role doesn't auto-inherit these scopes; the role's
`capability_scopes` must list them explicitly:

```toml
[[role]]
name = "personal_assistant"
parent = "default"
capability_scopes = [
  # Existing scopes you already have...
  "memory.read", "memory.write",
  # Toolkit scopes:
  "web.search",
  "task.read", "task.write",
  "health.read", "health.write",
  # For the health-check alert composition recipe:
  "notify.send",
  "schedule.create", "schedule.list",
  # For the automatic dispatch path (Phase 191) — required, and
  # itself Trusted-tier-only (not every toolkit scope is: e.g.
  # calc.eval, convert.units, and date.compute are SemiTrusted):
  "notify.dispatch",
]
```

For a read-only triage role, narrow the toolkit grants:

```toml
[[role]]
name = "readonly_triage"
parent = "default"
capability_scopes = [
  "memory.read",
  "web.search",        # search but no other side-effects
  "task.read",         # browse but not modify tasks
  "health.read",       # see watcher state but not register
]
```

#### What each tool does

**`web.search`** — `{q, count?}` → `{query, results: [{title, url, description}], result_count}`. Single GET to Brave; results trimmed from Brave's ~40-field-per-result response to the three the LLM needs.

**`task.create`** — `{title, notes?, due_date?}` → `{id, title, status: "open", created_at}`. Due date is RFC 3339 (`2026-06-01T12:00:00Z`).

**`task.list`** — `{status: "open" | "complete" | "all", limit?}` → `{tasks: [...], total_count}`. `total_count` reflects the full filtered set so the LLM can detect truncation.

**`task.complete`** — `{id, completion_note?}` → `{id, status: "complete", completed_at}`.

**`task.delete`** — `{id}` → `{id, deleted: true}`.

**`health.check.add`** — `{name, url, interval_secs, expect_status?}` → registered watcher. Interval is 60-86400s; default expect_status is 200.

**`health.check.list`** — `{}` → `{watchers: [{name, url, ..., last_check_at?, last_status_code?, last_ok}]}`. Optional fields omitted on just-registered watchers.

**`health.check.recent_changes`** — `{window_minutes?}` → `{changes: [{watcher_name, transitioned_at, from_ok, to_ok, status_code?}], count}`. Empty `changes` means "all stable in window."

**`health.check.remove`** (Phase 147) — `{name}` → `{name, was_already_removed}`. Idempotent — removing a missing name succeeds with `was_already_removed: true` rather than erroring (same posture as `calendar.delete_event` and `budget.delete`). Both the watcher registration AND its state (last_check_at, last_ok, etc.) are cleared; a future re-add of the same name starts fresh. Scope: `health.write`. **Phase 147 closes the Phase 125 Chapter G #2 candidate list** (calendar reminders → Phases 141-142, budget tracking → Phases 143-144, health.check.remove → Phase 147; alert dispatch intentionally held alongside Channel Activation).

#### Budget tracking (Phase 143)

Two tools sharing a JSON-persisted entry store at
`~/.aivyx-pa/tool-processes/toolkit/budget.json`
(0600 perms, atomic write-then-rename). Same
substrate posture as the task store.

**`budget.record`** — `{amount, category, note?}` → `{id, amount, category, note, recorded_at}`. `amount` is unitless f64 (positive = expense, negative = income/refund). `category` is free-text. Scope: `budget.write`.

**`budget.summary`** — `{period?, since?, until?}` → `{period, since, until, total, entry_count, by_category: [{category, total, count}]}`. `period` is one of `today` / `this_week` (default; ISO week Monday-Sunday) / `this_month` (calendar month) / `this_year` (calendar year) / `all_time`. Explicit `since` / `until` (RFC 3339) override the period bounds — use these for rolling windows like "last 7 days". `by_category` sorted descending by total, ties broken alphabetically. Scope: `budget.read`.

Example operator prompt: "Lunch was twelve dollars, food category." → agent calls `budget.record {amount: 12.00, category: "food", note: "lunch"}`. Then: "How much did I spend this week?" → agent calls `budget.summary {period: "this_week"}` and paraphrases the result.

**`budget.update`** (Phase 144) — `{id, amount?, category?, note?}` → updated entry. Partial update: only the fields you supply change. For `note`, JSON `null` explicitly clears the note; omitting the key leaves it unchanged (standard JSON-PATCH semantics). Errors if the id doesn't match an existing entry. Scope: `budget.write`.

**`budget.delete`** (Phase 144) — `{id}` → `{id, was_already_deleted}`. Idempotent — deleting a missing id succeeds with `was_already_deleted: true` rather than erroring. Same posture as `calendar.delete_event`. Scope: `budget.write`.

Example: "Actually that lunch was fifteen, not twelve." → agent calls `budget.update {id: "<entry-id-from-record-output>", amount: 15.00}`. Or: "Delete that snacks entry from yesterday." → agent calls `budget.summary` to find it, then `budget.delete {id: ...}`.

**`budget.trend`** (Phase 149) — `{months_back?, category?}` → `{months: [{month, total, entry_count, delta_vs_prior, pct_change_vs_prior}], category, months_back}`. Returns one bucket per calendar month, oldest-first, ending with the current month-in-progress. `months_back` defaults to 6, capped at 36 (three years). Optional `category` filter scopes all buckets to one category. `delta_vs_prior` and `pct_change_vs_prior` are `null` for the first month (no prior to compare against) AND when the prior month's total was zero (clean `null` rather than infinity). Calendar months — `months_back: 6` from June returns Jan-June, not the last 180 days. Scope: `budget.read`.

Example: "Is my food spending up this quarter?" → agent calls `budget.trend {months_back: 3, category: "food"}` and paraphrases the trend ("you spent +15% in May vs April, then -8% in June"). Or "How am I doing overall this year so far?" → agent calls `budget.trend {months_back: 6}` (no category) and surfaces the change-over-time story.

**`budget.categories`** (Phase 150) — no input → `{categories: [string]}`. Returns every unique category present in the budget store, sorted ascending. Used by the agent to answer "what categories have I used" or to confirm an unfamiliar category before recording.

**Phase 150 — category normalization + suggestion.** New entries recorded via `budget.record` (and category updates via `budget.update`) are silently case-folded + whitespace-trimmed to a canonical lowercase form: "Food" / "FOOD" / "  food  " all become `"food"`. Legacy entries recorded pre-Phase 150 stay as-recorded — operators can manually `budget.update` them if canonical consistency matters.

Both `budget.record` and `budget.update` now surface an optional `category_suggestion` field in their output. When the operator's category input is Levenshtein-distance ≤ 2 from an existing category (e.g. "fod" vs "food"), the suggestion appears as the matched existing string. The operator's chosen category is still recorded — the suggestion is a paraphrasable hint, not auto-correction. The agent decides whether to surface "did you mean food?" to the operator.

Phase 144 scope cap: no category whitelist, no currency field, no bulk update/delete. Mistakes are now fully recoverable through the tool surface (no need to edit JSON directly). Phase 145+ candidates if other gaps surface.

#### Health-monitoring alert composition recipe

The polling loop records state transitions and, since Phase 191, also
dispatches a plain automatic notification for every transition directly
(see `default_notify_target` below) — no cron or agent turn
required for that baseline case. The recipe below remains useful when you
want the *agent* to compose richer, more specific alert text instead of
(or alongside) the automatic one. Operator's setup:

```toml
# In your aivyx-pa.toml or via the schedule.create tool:
[[schedule]]
name = "health-monitor-sweep"
cron = "0 * * * *"   # every hour at minute 0
prompt = "Check `health.check.recent_changes` for any state flips in the last hour. For each change, send a notify.send summarizing the watcher name, the old state, and the new state. If there are no changes, do nothing."
```

The fired turn:
1. Invokes `health.check.recent_changes {window_minutes: 60}`.
2. If `count > 0`, composes a `notify.send` per change.
3. If `count == 0`, exits silently.

This works reliably under cloud providers (Anthropic /
OpenAI) per the Phase 124 finding — local Ollama models
won't reliably make the multi-tool call sequence even if
the tools themselves are simple.

#### Operator state files

All toolkit data lives under `~/.aivyx-pa/tool-processes/toolkit/`:

| File | Owner | Sensitivity |
|---|---|---|
| `config.toml` | operator (write) | Brave API key — 0600 recommended |
| `tasks.json` | tool process (read/write) | TODO content — 0600 auto |
| `health.json` | tool process (read/write) | Watcher URLs + state — 0600 auto |

All files use the same atomic write-then-rename pattern as
Gmail's token file (Phase 123). Operators backing up Aivyx PA
state should include this directory.

#### Operator-side troubleshooting

- **"$HOME unset; cannot resolve token path"** at daemon
  startup. The daemon's spawn environment doesn't inherit
  `$HOME`. Set it explicitly in the systemd / launchd
  unit, or use an absolute `aivyx-toolkit` invocation in
  the `command` field with an explicit `HOME=` env var.

- **`web.search` returns "401" or "missing API key".** Your
  `[brave_search].api_key` is missing or wrong. Check
  `~/.aivyx-pa/tool-processes/toolkit/config.toml`; regenerate
  the key at the Brave dashboard if needed.

- **Health watcher never polls.** The watcher's
  `interval_secs` was set too high (24h max), OR the tool
  process crashed at startup before the polling loop
  spawned. Check the daemon's stderr; `aivyx-toolkit (ipc):
  failed to open health store: ...` would be the message.

- **`health.check.add` rejects URL.** Must start with
  `http://` or `https://`. `localhost` is fine for local-
  service monitoring; the tool process trusts the operator
  to validate target reachability.

- **Task `due_date` parse error.** Must be RFC 3339 —
  `2026-06-01T12:00:00Z` or with offset `2026-06-01T07:00:00-05:00`.
  Dates without times (`2026-06-01`) are rejected; if the
  operator just wants a "due that day" semantic, use
  `2026-06-01T23:59:59Z`.

#### What Phase 125 deliberately leaves to follow-on phases

- ~~No automatic alert dispatch~~ — **shipped Phase 191.** Set
  `default_notify_target` (a top-level key, not inside any TOML
  table) in `~/.aivyx-pa/tool-processes/toolkit/config.toml` and the polling
  loop dispatches a notification directly (both directions: down
  and recovered) whenever `health.check.recent_changes` would
  have shown a new entry — no cron, no agent turn required. Because
  it's a top-level scalar key, it MUST be listed before any `[table]`
  header in the file — TOML (without `deny_unknown_fields`, which
  this loader doesn't set) silently ignores a bare key placed after a
  table header, treating it as belonging to that table instead of the
  document root. A complete `config.toml` combining it with the
  `[brave_search]` block from step 2 above:

  ```toml
  default_notify_target = "phone"

  [brave_search]
  api_key = "BSA-..."
  ```

  `phone` must name an actual configured `[[notify_target]]` entry in
  `aivyx-pa.toml` — a mismatched name fails with
  `NotifyError::UnknownTarget`, logged only to the daemon's own
  stderr (the toolkit process has no channel back to the operator for
  this failure). The agent-mediated recipe above still works and is
  useful for richer, LLM-composed alert text; the automatic path is a
  reliable floor under it, not a replacement.
- **No multi-tool harness lift.** Phase 123's SDK-validation
  finding (lift `run_multi_tool_subprocess` from per-crate
  duplicates into `aivyx-tool`) is still outstanding;
  `aivyx-toolkit` mirrors `aivyx-gmail`'s harness inline.
- **No web.search result enrichment.** Brave's response has
  per-result snippets we trim; an operator who needs full
  page content can chain `web.search` → `web.fetch`. A
  future tool could combine the two if pressure surfaces.

## Moving Aivyx PA to a new machine

Phase 64 ships **identity export**: a portable snapshot of your
operator-declared Profile and the reflection-approved Persona
chain. Useful for backup before risky changes, for migrating
between machines (laptop → VPS, old host → new host), or for
inspecting the chain offline with `jq`.

```sh
# On the source host (daemon must be running):
aivyx-pa identity export ~/aivyx-pa-snapshot.json
# Wrote N deltas + Profile to ~/aivyx-pa-snapshot.json
# File permissions: 0600 (owner-only).
```

The file is pretty-printed JSON with the following shape:

```json
{
  "schema_version": 1,
  "exported_at": "2026-05-14T14:30:00Z",
  "source_host": "laptop.local",
  "profile": { "assistant_name": "...", "..." : "..." },
  "persona": {
    "deltas": [ { "seq": 0, "delta": { "..." : "..." } }, ... ],
    "effective_at_export": { "..." : "..." }
  }
}
```

The chain's HMAC MACs are deliberately omitted from the export
— the per-host HMAC key is not portable. On import the chain
is re-signed with the target host's key. Trust comes from
operator authority, not cross-host cryptographic provenance.

To restore a snapshot on a target host (Phase 65):

```sh
# On the target host (daemon must be running):
aivyx-pa identity import ~/aivyx-pa-snapshot.json

# If the target host already has a Persona chain, the import
# refuses by default to avoid silent overwrite. Pass --force
# to wipe and replace:
aivyx-pa identity import ~/aivyx-pa-snapshot.json --force
```

On success the daemon refreshes its runtime persona state
immediately — the next agent turn sees the imported persona
without a restart.

**Profile import is operator-driven.** The export bundle
includes the source host's `[profile]` section for reference,
but `aivyx-pa identity import` does not auto-write `aivyx-pa.toml`.
To apply the imported Profile, hand-edit the target host's
`aivyx-pa.toml` to match the bundle's `profile` block, then
`aivyx-pa daemon stop && aivyx-pa` to reload. This keeps the
destructive-write scope tight to one on-disk artifact (the
encrypted Persona chain).

## Hardware-backed federation identity (YubiKey)

Chapter Passport's federation identity (`docs/FEDERATION.md`) normally
lives as a software Ed25519 key sealed at rest under your storage
master key. `aivyx-pa federation yubikey-init` provisions the alternative:
an Ed25519 keypair generated **on** a YubiKey's OpenPGP card applet,
never leaving the hardware, with every signature gated behind a
physical touch.

**Requires `pcscd` running, and is not built by default.** This is a
real, separate system dependency — no other Aivyx PA command needs a
smart-card daemon — and the underlying crate (`aivyx-yubi`)
transitively needs `libpcsclite` at *build* time too (via `pcsc-sys`),
so the binary you download or build by default does not include this
command at all:

```sh
# Debian/Ubuntu:
sudo apt install pcscd libpcsclite-dev
sudo systemctl enable --now pcscd
# Arch:
sudo pacman -S pcsclite ccid
sudo systemctl enable --now pcscd

# Build the CLI with the `yubikey` feature to get the subcommand:
cargo build -p aivyx-cli --features yubikey
```

Without `--features yubikey`, `aivyx-pa federation yubikey-init` still
parses (so the error is actionable) but refuses immediately with a
"rebuild with `--features yubikey`" message.

**Before provisioning, the card's User and Admin PIN must already be
changed away from the OpenPGP-card factory defaults** (`123456` /
`12345678`) — e.g. via `gpg --card-edit` → `admin` → `passwd`. This
command never changes a PIN on your behalf; it only refuses to proceed
against a still-factory-default card.

**PIN retry budget warning.** Every run of this command — even a
successful one — consumes one retry attempt off *both* the User and
Admin PIN's limited counters (typically 3 each on real hardware), via
the factory-default-PIN check that runs before anything else. If Admin
PIN verification then also fails (e.g. a mistyped PIN), that's a
*second* retry burned on the Admin PIN in the same run. Two mistakes,
not three, can permanently block the Admin PIN — a blocked Admin PIN
has **no** self-recovery path short of a full card wipe
(TERMINATE+ACTIVATE, which erases all existing keys; only a
pre-configured Reset Code can otherwise recover it). Check your retry
counter (e.g. `gpg --card-status`) before retrying a failed
`yubikey-init` run.

```sh
aivyx-pa federation yubikey-init my-instance-id ~/.config/aivyx-pa/federation-hardware-binding.json
# aivyx-pa federation yubikey-init: discovering YubiKey (requires pcscd running)...
# Admin PIN (input hidden):
# aivyx-pa federation yubikey-init: generating an Ed25519 keypair in the Signature slot
#   (this overwrites any existing key in that slot)...
# aivyx-pa federation yubikey-init: setting the Signature slot's touch policy to fixed
#   (every future signature will require a physical touch)...
# aivyx-pa federation yubikey-init: wrote binding record (...) to ~/.config/aivyx-pa/federation-hardware-binding.json
# aivyx-pa federation yubikey-init: post-provisioning check passed — a fresh re-discovery of
#   card ... confirms its serial and Signature-slot public key match what provisioning just
#   wrote for instance `my-instance-id`, and aivyx-federation's own Identity::load_hardware
#   (the real production load path) accepts them. This does NOT confirm the touch policy is
#   being enforced live on the card -- that is confirmed the first time this identity actually
#   signs a real federation request, not by this init command.
```

This is **destructive** if the card's Signature slot already holds a
key — provisioning overwrites it with no "don't clobber" guard. Only
run it against a freshly-reset or never-before-provisioned card. The
resulting binding record (`{instance_id, card_serial,
public_key_base64}`) is plain, non-secret JSON — the private key never
leaves the card, so there is nothing sensitive to protect in the file
itself. What that binding record is *for* (wiring a running daemon to
sign federation requests with the card instead of a software key,
including the still-open question of how a long-running daemon obtains
the User PIN at its own startup) is a separate, not-yet-solved
integration step beyond this provisioning command's scope.

## Debugging missing notifications

When a scheduled briefing or trigger-fired notification doesn't
arrive, the audit chain has the answer. Phase 67 records every
auto-notify fire (delivered, skipped, or failed) as an
`AutoNotifyDispatched` entry alongside `TurnStarted` /
`TurnEnded`. To inspect:

```sh
# Walk the chain offline (no daemon needed):
aivyx-pa --verify-only

# Or open the Web UI's Audit tab at http://127.0.0.1:7843
```

Filter on `AutoNotifyDispatched` and look at the `outcome`
field:

- `Delivered` — the notification reached the backend; if you
  didn't see it, check the backend (Telegram bot still
  authorized? webhook endpoint reachable from operator side?).
- `SkippedEmptyResponse` — the agent's turn produced no text;
  often a sign the trigger prompt was misconfigured or the
  model returned nothing useful.
- `Failed { error_kind, error_message }` — backend rejected
  the dispatch. `error_kind` is one of `transport`, `auth`,
  `rejected`, `timeout`, `unknown_target`.

Correlate by `session_id` to find the corresponding
`TurnStarted` / `TurnEnded` events for the same trigger fire.

## Multi-target + conditional dispatch (Phase 72)

Every trigger (`[[schedule]]`, `[[webhook]]`, `[[file_watch]]`)
now accepts three notify-flavoured knobs that compose with the
notify-target backends in the next sections.

**Multi-target fan-out** — `notify_targets = ["phone",
"desktop"]` fires every named target concurrently when the
trigger completes. One backend's transport failure doesn't
block the others; each per-target outcome audits independently
in the audit chain. The singular `notify_target = "phone"`
stays valid as a one-element alias.

**Default-target sugar** — Mark ONE `[[notify_target]]` block
with `default = true`. Triggers that omit `notify_targets`
fall through to it at config-load time. At most one default
is allowed.

**Conditional notify** — `notify_when` gates dispatch by turn
outcome:

| Value | Behavior |
|---|---|
| `"always"` (default) | Dispatch on every fire. Empty responses still get the Phase 63 `SkippedEmptyResponse` audit treatment. |
| `"on_failed"` | Dispatch only when the turn outcome is `Failed` or `TimedOut`. Useful for "ping me when my morning job breaks." |
| `"on_completed_non_empty"` | Dispatch only when the turn completed AND the rendered body is non-whitespace. The common shape for "stop pinging me on every cron fire — only when there's something to say." |

A condition-gated skip records
`AutoNotifyOutcomeSummary::SkippedByCondition { condition }`
in the audit chain so forensic searches can answer "why didn't
this fire?" definitively.

## Per-target retry, rate limit, history (Phase 73)

Each `[[notify_target]]` block accepts four optional fields
that tune backend behavior. Defaults preserve Phase 62
behavior — operators who don't set them get one attempt per
fire and no rate limiting.

**Retry** — flat per-target fields:

```toml
[[notify_target]]
name = "alerts"
kind = "webhook"
url = "https://ntfy.sh/aivyx-personal-2026"
retry_count = 5             # default 0, cap 10
retry_backoff_ms_start = 200  # default 500ms, min 100ms
```

Retries fire on `Transport`, `Timeout`, and `Rejected` with
HTTP status ≥ 500 — the transient-failure class. `Auth`,
`UnknownTarget`, and `Rejected` with status < 500 never
retry; those need operator intervention or are programmer
errors. Backoff is exponential: `backoff_ms_start * 2^attempt`.
For `retry_count = 5` starting at 200ms the schedule is
0ms (initial) + 200ms + 400ms + 800ms + 1.6s + 3.2s between
attempts — total worst-case latency about 6.2 seconds per fire.

**Rate limit** — in-memory sliding-window token bucket per
target:

```toml
[[notify_target]]
name = "phone"
kind = "telegram"
chat_id = "123456789"
rate_limit_max = 20
rate_limit_window_secs = 3600
```

Both fields must be set together or neither. Excess attempts
skip the backend call and record
`AutoNotifyOutcomeSummary::SkippedByRateLimit { limit,
window_secs }` in the audit chain. State lives in memory for
the daemon's lifetime — restart resets the bucket. v1 trades
durability for simplicity; the audit chain remains the
canonical record of what actually dispatched.

**History review** — operators have two surfaces:

```sh
# Terminal — flat-text table with seq, timestamp, target,
# outcome, trigger source/id, optional detail column.
aivyx-pa notify history                          # latest 100, all targets
aivyx-pa notify history --target phone           # filter by target
aivyx-pa notify history --target phone --limit 500  # max per page
```

Web UI: open `http://127.0.0.1:7843/` and click the
**Notifications** tab. Per-target chips auto-populate from
the loaded page; outcome badges colour-code delivered (teal)
vs failed (orange) vs skipped (amber).

## Email notifications (Phase 68)

Most operators don't run a Telegram bot but everyone has email.
The `email` notify kind covers that. Add a `[[notify_target]]
kind = "email"` block + a top-level `[email]` section with
SMTP credentials, and the `notify.send` tool / trigger
auto-notify path both route through it.

**Quick setup by provider:**

- **Gmail / Google Workspace** — host `smtp.gmail.com`, port
  `587`, `tls_mode = "starttls"` (default). With 2FA enabled
  (which it should be), generate an app password at
  *Account → Security → App passwords* and use it as
  `[email] password`.
- **Fastmail** — host `smtp.fastmail.com`, port `587`. App
  password from *Settings → Privacy & Security → Integrations*.
- **ProtonMail** — run the ProtonMail Bridge locally; SMTP
  goes to `127.0.0.1` with the bridge-supplied credentials.
- **Self-hosted Postfix / Mailcow** — whatever your
  submission port is (usually 587), STARTTLS, plain
  username + password.
- **AWS SES** — host `email-smtp.<region>.amazonaws.com`,
  port `587`, IAM-derived SMTP credentials.

**TLS is mandatory.** Aivyx PA rejects `tls_mode = "none"` at
config-load time because PLAIN/LOGIN auth over cleartext
leaks credentials. If you need a plain-text relay for testing,
use a localhost SMTP capture tool instead.

**No OAuth2 yet.** Phase 68 ships PLAIN/LOGIN auth only.
Gmail/Office 365 users with strict workspace policies that
prohibit app passwords need to wait for the OAuth2 phase or
use a different provider in the meantime.

## Web UI desktop notifications (Phase 69)

The Web UI's localhost-only page at `127.0.0.1:7843` (Phase 39)
can deliver OS-level desktop notifications + an in-page toast
banner whenever the agent or trigger auto-notify fires.
Operators who already keep the Web UI tab open get the
lowest-friction notification path — no API keys, no SMTP
setup, no bot tokens.

**Enable it in two steps:**

1. Add a `[[notify_target]] kind = "web-ui"` block to
   `aivyx-pa.toml` (and make sure the Web UI server is enabled —
   it ships on by default):

   ```toml
   [[notify_target]]
   name = "desktop"
   kind = "web-ui"
   ```

2. Open `http://127.0.0.1:7843/` in a browser. On first load a
   banner asks "Enable desktop notifications" — click *Enable*
   and grant the browser's permission prompt. The agent's
   `notify.send` tool and any trigger with
   `notify_target = "desktop"` will now reach you.

**The browser tab must be open.** Desktop notifications are
delivered over the existing WebSocket bridge; close the tab
and notifications stop firing for that target. Pair Web UI
notify with `kind = "email"` (or `kind = "telegram"`) when you
want notifications to land while you're away from the laptop —
the audit chain records every dispatch either way.

**One Web UI per daemon.** Multiple `kind = "web-ui"` targets
all funnel into the same browser fan-out, so naming them
differently only affects the per-target audit name; the
operator-visible behavior is identical.

## Memory subsystem (Phase 74)

Phase 74 completes the self-learning triad — Persona (P14),
reflection (Phases 70-71), and now a first-class memory
surface — with three operator knobs.

**Keyword search.** The agent gets a `memory.search` tool
(case-insensitive substring across topics + bodies, requires
the cross-topic `memory.read:topic:*` wildcard scope).
Operators have terminal + Web UI parity:

```sh
aivyx-pa memory list                    # every topic
aivyx-pa memory show <topic> [--limit N]
aivyx-pa memory search <query> [--limit N]
aivyx-pa memory evict <topic> [--yes]   # delete a whole topic
```

The Web UI Memory tab gives the same: a topic list, per-topic
entry view, an inline search bar, and a per-topic Evict
button (confirm-gated).

**Per-topic retention.** `[[memory.retention]]` blocks declare
a topic-glob pattern + a policy. The hourly GC walks every
entry, applies the **first** matching rule (put narrower globs
first), and falls through to the global `[memory] ttl_secs`
for unmatched topics:

```toml
[[memory.retention]]
topic_glob = "project/**"
retention = "forever"          # never TTL-expire

[[memory.retention]]
topic_glob = "notes/daily/*"
retention_days = 30            # evict entries older than 30d
```

Exactly one of `retention = "forever"` or `retention_days = N`
per block; partial / both-form / unknown-value config rejects
at load time.

**LRU eviction.** When a topic exceeds `[memory]
max_per_topic`, the least-recently-**read** entry is evicted
first. Every `memory.read` stamps `last_read_at`; the Web UI
Memory pane surfaces it ("last read: never" for entries the
agent has written but not recalled). No config knob — it's
automatic once `max_per_topic` is set. Distinct from the
prior FIFO-on-write eviction: an old note the agent keeps
recalling now survives a younger note it never reads.

### Topic canonicalization (Phase 89)

For 88 phases the assistant has accumulated topic-keyed
signal everywhere (Phase 7 memory, Phase 77 recall log,
Phase 82 helpfulness ledger, Phase 83 co-occurrence ledger,
Phase 87 consolidate-pair proposal IDs) — but every layer
keyed by the operator's typed topic string verbatim. That
means `deploy`, `Deploy`, `deploys`, and `deploying` are
four distinct topics across every accumulator, and the
signal that should add up across them was silently
fragmented.

Phase 89 closes the long-standing Phase 82 deferral with the
smallest possible substrate fix: an **opt-in canonicalization
seam at the `Memory` trait boundary**. With the flag on,
every topic-string argument is folded to a canonical form
before storage, and every topic-keyed lookup folds the same
way — so `Deploys`-the-write is found by `deploy`-the-read,
and the downstream signals (recall log, helpfulness ledger,
co-occurrence ledger, Persona facet provenance) all inherit
clean keys through the existing pipeline. No per-layer
plumbing; one seam, every consumer benefits.

- **Off by default.** With no `[memory] canonicalize_topics`
  key (or set to `false`), the memory layer is byte-identical
  to pre-Phase-89. Matches the project's 88-phase
  behaviour-change-is-opt-in discipline.
- **No migration.** Existing fragmented data stays as-is and
  decays out naturally via the Phase 82/83 ~60-day half-life
  + the Phase 77 ~30-day recall-log retention. The past
  converges to clean within roughly a quarter without
  intervention; no MAC-signed Persona chain entries are
  rewritten.
- **The v1 rule set.** A small hand-rolled English stemmer
  (no new workspace deps). Lowercase + trim + collapse
  whitespace, then **one** suffix-strip rule fires with
  min-length guards: `ies → y` (`policies → policy`),
  `ing` drop (`testing → test`), `ed` drop (`tested →
  test`), `es` drop **only when the stem ends in a
  hissing-sound letter — `sh` / `ch` / `s` / `x` / `z`**
  (`boxes → box`; `roles` falls through to the next rule),
  `s` drop (`tests → test`; `process` stays — the `ss`
  guard skips). The function is idempotent.
- **Applies to every topic-string trait entry point.**
  `put`, `get_recent`, `forget`, `gc_topic`,
  `evict_oldest_unread`, `put_vector`,
  `promote_recall_helpful`. **Does not** apply to prefix
  matching (`scan_prefix`), text queries (`search`), or
  topic-less methods (`gc_expired`, `list_topics`, the
  vector-only `semantic_search` paths).

Enable it in `~/.config/aivyx-pa/aivyx-pa.toml`:

```toml
[memory]
canonicalize_topics = true   # optional, default false
```

**To keep pre-Phase-89 behaviour:** leave the key out (or
set `false`). **Trade-off:** the stemmer lowercases proper-
noun-looking topics too (`Deploy` → `deploy`). The codebase's
typical topic slugs are category labels (`auth`, `frontend`,
`tests`), not entity names — the assumption is positive in
practice; an operator who needs case-sensitive topics
declines the opt-in. An operator-tunable alias table
(`[[topic_alias]]`) and a topic-by-topic exception list are
likely follow-ups if real-world fragmentation cases need them.

## Semantic memory search (Phase 75)

Phase 75 adds **embedding-ranked** memory retrieval on top of
the Phase 74 keyword search. It is **off by default** — without
an `[embedding]` section nothing changes, and a `semantic`
request transparently falls back to keyword.

**The base_url privacy choice.** Embedding means sending the
text to be embedded to the configured endpoint. `base_url` is
the only thing that decides whether memory content leaves the
machine:

- **Cloud** (`base_url = "https://api.openai.com"`, the
  default) — entry bodies and search queries are sent to
  OpenAI. This is your explicit, opt-in choice by configuring
  the section.
- **Local / on-device** — point `base_url` at any
  OpenAI-compatible server (ollama, llama.cpp,
  text-embeddings-inference, e.g.
  `http://localhost:11434`). Nothing leaves the box; no
  `api_key` needed.

```toml
[embedding]
base_url = "http://localhost:11434"   # local → on-device
model = "nomic-embed-text"
dimensions = 768
# api_key resolves env (AIVYX_PA_EMBEDDING_API_KEY) > this TOML
# key > the encrypted secrets store, same as the LLM keys.
```

`dimensions` must match the model's native output. Omitted
fields default to `https://api.openai.com` /
`text-embedding-3-small` / `1536`.

**Keyword fallback.** A `semantic` request silently serves
keyword results — flagged so you can see it — when (a) no
`[embedding]` section is configured, (b) the embedding
provider call fails (down, rate-limited, bad key), or (c) the
vector index is still empty. You never get a hard error for
asking for semantic; you get the best available answer.

```
aivyx-pa memory search "<query>" --semantic [--limit N]
```

The agent's `memory.search` tool gains a `mode` argument
(`"keyword"` default, `"semantic"`); the Web UI Memory tab
gains a **semantic** toggle next to the search box. All three
share one provider and one fallback rule.

**Backfill on upgrade.** Enabling `[embedding]` on an existing
store does not require a re-index command. New writes are
embedded inline; an hourly backfill pass (bounded per tick, so
no startup stall) walks entries lacking a current-dimension
vector and embeds them, so the back-catalog becomes searchable
gradually. Swapping embedding models is safe — vectors of the
old dimension are detected as stale and re-embedded by the
same backfill.

### ANN index for semantic memory search (Phase 96)

By default semantic memory search is **brute-force cosine**:
every query compares against every stored vector. That's
O(N) per query and works for any operator with up to a few
thousand entries. Phase 96 adds opt-in **IVF-style
approximate-nearest-neighbor** indexing for larger stores.

How it works: at build time, the vector index is partitioned
into K ≈ √N clusters via deterministic spaced-sampling +
one-pass nearest-centroid assignment. At query time, the
query vector is cosine-ranked against the K centroids
(cheap — K is small), the top-N clusters are selected, and
brute-force cosine then runs only within those clusters'
members. The candidate set narrows to ≈ N · (top_N / K),
which the existing brute-force re-rank then orders
**exactly** within that pool. End-to-end: O(N) → O(√N).

```toml
[embedding]
# ... existing fields ...
ann_index = true                # optional, default false
ann_rebuild_threshold = 100     # optional, default 100
```

**Semantics:**
- `ann_index = false` (default): brute-force only,
  byte-identical to pre-Phase-96.
- `ann_index = true`: ANN narrows candidates → brute-force
  re-ranks within. The final top-K is **exactly** ordered
  within the candidate set (the hybrid composition
  preserves the exact-cosine guarantee).

**Stale-rebuild:** the index lives in-memory and is rebuilt
on demand. `ann_rebuild_threshold = 100` means "after 100
new vector writes, the next recall rebuilds the index."
Lower the threshold for tighter freshness; raise it for
fewer rebuilds. Daemon restart drops the in-memory index;
the first ANN query after restart rebuilds from the
persisted embeddings.

**Quality:** IVF with one-pass nearest-centroid assignment
trades some recall for simplicity + zero new dependencies.
For very large stores (>100K entries) where recall quality
matters more, HNSW-level indexing is documented as a
Phase 96 deferral.

**To turn it off:** delete the `ann_index` key or set it to
`false`. `semantic_search_scored` returns to brute-force
byte-identically.

## Automatic recall (Phase 76)

Phase 75 gave the agent a semantic-search *tool*. Phase 76
makes recall **automatic**: with `[embedding]` configured,
every turn the assistant embeds your message, finds the most
semantically-relevant past memories, and injects them into
that turn's context **without being asked**. This is what
gives it continuity — it remembers across turns the way a
personal assistant should.

**Off when embedding is off.** No `[embedding]` section → no
auto-recall → behavior is byte-identical to pre-Phase-76. The
hook is also fully best-effort: an embedding-provider hiccup,
an empty vector index, or no sufficiently-relevant memory all
leave the turn untouched. Auto-recall never errors a turn.

**Two knobs** (under `[embedding]`, both optional):

- `rag_top_k` (default 5) — the most memories injected per
  turn.
- `rag_min_similarity` (default 0.20) — the cosine-similarity
  floor. This is the important one: it drops weakly-related
  hits so an unrelated prompt doesn't drag in noise. Raise it
  for stricter recall, lower it to recall more aggressively.
  Range `[0.0, 1.0]`; `rag_top_k` must be ≥ 1.

**What you'll see.** The recalled memories appear as a clearly
labeled, reference-only block at the top of the turn (visible
in the conversation / Web UI as part of that turn), and the
daemon log prints a one-line marker when recall fires:

```
aivyx-pa recall: injected 3 memories [project/notes, prefs]
```

The block is explicitly framed to the model as background
reference, not instructions — a recalled note cannot hijack
the turn.

### Heuristic recall gate (Phase 90)

For 89 phases auto-recall and adaptive Persona selection
(Phase 79) fired on **every** conversational turn —
including turns where the user message is a one- or two-token
acknowledgment (`ok` / `thanks` / `yes` / `cool`) that
cannot meaningfully steer recall. The bare-message embed on
those turns is essentially a random vector that pollutes the
ranker; the recall block and adaptive Persona selection
injected on top are noise the planner has to defend against.

Phase 90 adds the smallest possible fix: a length-based
**heuristic gate** at the top of both relevance hooks that
skips the embed (and everything downstream) when the trimmed
user message is shorter than `recall_gate_min_chars`. Both
consumers (auto-recall + adaptive Persona) share the same
gate and the same opt-in knob, exactly as Phase 86's window
work shipped both consumers under one switch.

- **Opt-in, off by default.** With `recall_gate_min_chars =
  0` (the default) the gate is disabled and behaviour is
  byte-identical to pre-Phase-90. Raise it (`4` is a
  conservative starting point that gates single-token
  acknowledgments without affecting normal messages) to
  engage.
- **Same gate, both consumers.** A gated turn produces no
  recall block AND no adaptive Persona facet selection
  (the planner uses the full Persona base prompt, exactly
  the pre-Phase-79 fallback). Symmetric Phase 86 design.
- **Maximum cost saving.** A gated turn skips the embed
  call entirely (not just the memory walk or the ranking)
  — the cheapest possible noise-turn path.
- **Unicode-char counted.** The threshold is in characters,
  not bytes — `héllo` is 5 characters whether you measure
  it semantically or not.

Configure under `[embedding]`:

```toml
[embedding]
# ... existing knobs ...
recall_gate_min_chars = 4   # optional, default 0 (disabled)
```

**Trade-off.** A short but meaningful message (`run!`,
`ack`, `git`) gets gated alongside fillers. The current
heuristic is operator-tunable but not pattern-aware; an
operator who needs more nuance can keep the gate at `0` or
configure a low threshold (`2` or `3`) that catches only
the very shortest noise turns.

### Token-budget context sizing (Phase 97)

Auto-recall and adaptive Persona selection have always
capped injection by **entry count** (`rag_top_k` for
recall, an internal K-facet limit for adaptive Persona).
Count is a proxy for token cost, not the cost itself. A
single memory body with a 4 KB blob silently displaces
multiple shorter memories from the same `rag_top_k`
budget; a Persona facet that grew from one sentence to ten
paragraphs eats turn after turn of input — sometimes
enough to bump the prompt past the model's context limit.

Phase 97 adds an opt-in **token budget** that caps both
paths after their existing rank-and-filter steps. The
existing count caps remain in place as **soft hints**;
the token budget is the hard cap. Items are already in
rank order (cosine score for recall, selection priority
for Persona); the budget walks them, and the **first
item whose addition would exceed the budget** (along with
every item after it) is dropped. No mid-item truncation
— operators get full items or nothing.

```toml
[embedding]
# ... existing knobs ...
recall_token_budget = 2000   # optional, default 0 (disabled)
```

**Semantics:**
- `recall_token_budget = 0` (the default): no budget
  enforcement; behaviour is byte-identical to
  pre-Phase-97.
- `recall_token_budget = N` (any `N >= 1`): both recall
  and Persona injection drop their lowest-ranked items
  until the running estimate fits.

**Estimator:** hand-rolled `chars / 4` (the OpenAI rule-
of-thumb for English) with a small fudge factor.
Accuracy ~±20%; sub-token accuracy isn't worth a new
tokenizer dependency. Unicode `chars()`-counted, not
bytes.

**What the operator sees:** the existing recall
breadcrumb (`aivyx-pa recall: injected N memor[y|ies]`)
reflects the post-budget set, so observers match what
was actually injected. The Phase 78 learning surface +
the Phase 84 cluster stat + the Phase 77 recall_log all
see the same post-budget hits.

**Edge case:** if every hit falls out of the budget,
auto-recall returns no block (the planner falls back to
the base prompt without an empty recall section).
Adaptive Persona's protected core (constraints + scalar
identity) is **always** present regardless of the
budget — the budget only trims soft-facet selection.

### Hybrid keyword+semantic recall (Phase 98)

Auto-recall has ranked by cosine similarity over
embeddings since Phase 75. Embeddings encode semantic
relationships well but struggle with **rare-term recall**:
acronyms, proper nouns, code identifiers, project
codenames. A query mentioning "ATC-417" or "kubernetes"
or "Jane Henderson" may miss the memory specifically
about that term because the embedding doesn't strongly
link the rare token to a learnable concept.

The keyword search tool (Phase 74,
`Memory::search`) handles these exact-match cases via
case-insensitive substring matching, but operates as a
**separate manual path** — the agent / operator drives
`aivyx-pa memory search`, not auto-recall.

Phase 98 closes that gap with **Reciprocal Rank Fusion
(RRF)**. With `[embedding].recall_hybrid = true`,
auto-recall runs both the semantic ranker AND the
substring search on every recall, then fuses the two
rankings before feeding the downstream pipeline
(cluster expansion, token budget, etc.).

```toml
[embedding]
# ... existing knobs ...
recall_hybrid = true   # optional, default false
```

**Why RRF over score fusion.** Cosine scores in
`[-1, 1]` and substring hit counts don't share a scale.
Score-fusion approaches (`α * cosine + (1-α) *
keyword_score`) require normalization and an alpha tuning
knob. RRF is **rank-based** — it sums each item's
position-based contribution
(`1 / (k + rank + 1)` with `k = 60`, the industry-
standard constant) and ignores raw scores entirely. No
normalization, no tuning, no new dependency.

**Semantics:**
- `recall_hybrid = false` (default): semantic-only,
  byte-identical to pre-Phase-98.
- `recall_hybrid = true`: both rankers run; their
  rankings fuse via RRF; the fused top-K feeds the
  downstream pipeline.

**The `rag_min_similarity` floor.** RRF scores aren't on
the cosine scale, so the configured similarity floor
isn't directly comparable. v1 **skips** the floor on the
hybrid path. The `rag_top_k` cap still limits the fused
output, and items that only one ranker surfaces tend to
get small RRF scores (`1/61 ≈ 0.0164`) that get pushed
out by stronger items. A future phase could add a
separate `rag_hybrid_min_rrf` knob.

**What this fixes:** queries with rare or technical
terms now reliably surface memories about those terms,
even when the semantic side doesn't rate them highly.
The semantic side still catches the conceptually
similar memories. The fusion is the union of both
signals.

## Recall feedback loop (Phase 77)

Auto-recall (Phase 76) made the assistant *remember*. Phase 77
makes it **learn which memories are worth remembering** —
without you configuring anything and without an LLM grading
itself.

**How it learns (structurally, no LLM).** Every auto-recall is
logged. On your existing reflection schedule's cron, the loop
correlates each recall with how that turn actually went, using
only signals already in the audit chain:

- the turn `completed` and you did **not** immediately come
  back → the recalled memories scored **helpful**;
- the turn `failed`/`timed_out`, **or** you started another
  turn in the same session within 60 s (the structural proxy
  for "that didn't land") → scored **unhelpful**;
- `escalated`/`cancelled` → no signal.

It never asks the model whether its own recall was useful —
that self-judgement is exactly what this avoids. Per-turn the
signal is coarse; across many turns it is reliable.

**What it does with the signal — two actuators:**

1. **Memory retention self-tunes.** Consistently-helpful
   memories are kept "warm" so the existing Phase 74 LRU
   eviction protects them; consistently-unhelpful ones are
   simply not protected and age out under the same pass. No
   new eviction policy — good memory just gets stickier.
2. **Operator-gated Persona proposals.** A topic whose
   memories are *strongly* and repeatedly helpful files a
   **Pending** Persona proposal (e.g. "operator consistently
   benefits from recalled context about X — keep surfacing
   it"). You review and approve or reject it via the existing
   `aivyx-pa persona proposals` flow. **The loop never edits the
   Persona itself** — you remain the authority (the Phase 70
   P14 rule). The same deterministic proposal is filed once;
   it won't re-nag after a rejection.

**Zero configuration.** There is no `[recall_feedback]`
block — thresholds and the ~30-day recall-event retention are
fixed for v1. The loop is active precisely when auto-recall
(`[embedding]`) **and** a `[[reflection_schedule]]` are both
present; otherwise it is a complete no-op (pre-Phase-77
behavior). Each cycle prints a daemon-log breadcrumb:

```
aivyx-pa recall-feedback: schedule "nightly" — 12 entries scored, 4 promoted, 1 proposal(s) filed
```

### LLM-judged recall usefulness (Phase 91)

For 90 phases the recall-feedback signal above has been
**structural**: every recall in a successfully-completed turn
inherits `+1` helpfulness; every recall in a failed turn
inherits `-1`. Phase 91 adds an opt-in **LLM-judged per-recall
classification** alongside the structural proxy — a 3-way
verdict (`used` / `irrelevant` / `hurt`) recorded on each
recall hit so downstream consumers can eventually consult a
sharper signal than turn-level outcome.

After the input-quality arc (86 windows, 89 canonicalization,
90 recall gate), Phase 91 is the missing-half **feedback-
quality** move:

- **Phase 86** sharpened *what* gets embedded (windows).
- **Phase 89** sharpened *how* signals key (canonical topics).
- **Phase 90** sharpened *when* recall fires at all.
- **Phase 91** sharpens *whether* recall actually helped.

Important: Phase 91 is **augment, not replace** (Q3a). The
new `judgment: Option<RecallJudgment>` field is captured on
every recall hit, but no existing accumulator (Phase 82
helpfulness ledger, Phase 83 co-occurrence ledger, Phase 85/88
Persona decay, Phase 87 pattern-driven proposals) reads it in
v1. Every existing behaviour stays byte-identical. A future
phase consumes the new signal once it is validated in
production.

- **Off by default.** With no `[recall_judgment]` block (or
  `enabled = false`) the LLM judge never runs — zero added
  cost, zero behaviour change. Matches the 90-phase
  behaviour-change-is-opt-in discipline.
- **Reflection-cron batched.** One LLM call per cron tick
  judges every unjudged recall in the lookback window (up to
  `max_recalls_per_cycle`, default `30`); the remainder rolls
  to the next cycle. Bounded cost shape, identical to Phase
  87's `LlmPairPhraser` cadence.
- **v1 simplification.** The judge classifies based on the
  recall's `(topic, body)` content + a weak context hint —
  it does NOT see the model's actual response text (which
  isn't in the audit chain today). A future phase enriches
  the input via audit-chain extension or per-turn capture;
  the Q3a augment posture means even this weaker v1 judgment
  changes nothing it shouldn't.
- **Operator-visible.** Each cycle prints a breadcrumb
  (`aivyx-pa recall-judgment: schedule "nightly" — judged 12
  (used=7, irrelevant=4, hurt=1, skipped=0)`); `aivyx-pa
  learning` + the Web UI Learning tab render a new
  "LLM-judged recall usefulness (last cycle, opt-in)" block
  showing per-classification counts + the `(topic, judgment)`
  pairs.

Enable it in `~/.config/aivyx-pa/aivyx-pa.toml`:

```toml
[recall_judgment]
enabled = true
max_recalls_per_cycle = 30   # optional, default 30
```

Validation (only when `enabled = true`):
`max_recalls_per_cycle >= 1`. **To turn it off:** set
`enabled = false` or delete the block — every accumulator
returns to pre-Phase-91 behaviour.

### Judgment-driven recall feedback (Phase 93)

Phase 91 records per-hit `RecallJudgment` (`Used` /
`Irrelevant` / `Hurt`) on every recall hit. Phase 93 lets the
**recall-feedback actuator** consume those verdicts. With
the new `[recall_feedback].use_judgment_signal = true` knob,
the correlator (`correlate_detailed`) reads each hit's
`judgment` field and uses it to derive that hit's signal —
overriding the Phase 77 turn-level structural proxy for any
hit that carries one. Un-judged hits keep using the
structural proxy, so the augment is incremental as the
Phase 91 cron processes hits.

Per-verdict mapping (symmetric with the structural
`±WEIGHT`):

- `Used` → `+WEIGHT` (the hit was helpful, regardless of
  the turn-level outcome).
- `Hurt` → `-WEIGHT` (the hit was actively misleading,
  regardless of the turn-level outcome).
- `Irrelevant` → no contribution (dead weight; neither
  rewarded nor punished).
- `None` (un-judged) → falls back to the turn-level
  structural signal.

The downstream actuators (memory promotion via
`apply_retention_feedback`, Persona proposals via
`emit_persona_proposals`) read the same `HelpfulnessTally`
shape — only the signal source per hit changes. Operators
who enabled Phase 91 for *visibility only* (the v1
"augment, not replace" posture documented at field
introduction) see no actuator-behaviour change unless they
also flip this knob.

```toml
[recall_feedback]
use_judgment_signal = true   # optional, default false — Phase 93
```

The knob lives in `[recall_feedback]` (the consumer side),
separate from `[recall_judgment]` (the producer side from
Phase 91), so the two configs stay independently
reason-aboutable. Turning the judge on without flipping
this knob keeps the actuator on the structural signal it
has used since Phase 77; flipping both turns on the
self-improving loop end-to-end. **To turn it off:** set
`use_judgment_signal = false` or delete the block —
`correlate_detailed` returns to byte-identical pre-Phase-93
behaviour.

The `aivyx-pa learning` surface flags the augment with a
`signal source: judgment-driven` banner under the recall
count when the knob is on, so the operator can confirm at
a glance that the loop is in the augmented mode they
expect.

### Correction-signal learning (Phase 172)

Phase 172 closes the self-improvement loop named in the
Aivyx Agent Review: the agent now notices when **you correct
it**. Whenever a turn completes and you immediately come back
in the same session (the structural "that wasn't what I
wanted" proxy already computed since Phase 77), the topics
recalled into that turn accumulate a **correction count** in
a durable, time-decayed ledger (~30-day half-life). This is a
distinct signal from helpfulness: a topic can be net-helpful
yet still keep needing rework — those are exactly the ones
worth a Profile note.

Two layers ship:

- **The correction ledger** — always on, zero-config (built
  alongside the recall log, like the Phase 82/83 ledgers). It
  only accumulates; it never changes behaviour on its own.
  `aivyx-pa learning` and the Web UI Learning tab render a
  **"Most-reworked topics (accumulated)"** block so you can
  see what the agent is picking up.
- **The correction-consolidation pass** — opt-in. When a
  topic's decayed correction count clears `min_corrections`
  over at least `min_samples` reflection windows, the
  reflection cron asks the LLM to phrase a one-line
  `learned_context` facet and files it as a **Pending Persona
  proposal** through the existing Phase 70 chain. You approve,
  edit-then-approve, or reject — the operator gate stays the
  sole authority (nothing is ever applied automatically).

Enable the proposal pass in `~/.config/aivyx-pa/aivyx-pa.toml`:

```toml
[correction_consolidation]
enabled = true
min_corrections = 3.0          # optional, default 3.0
min_samples = 2                # optional, default 2
max_proposals_per_cycle = 3    # optional, default 3
```

Validation (only when `enabled = true`):
`min_corrections` finite and `> 0.0`, `min_samples >= 1`,
`max_proposals_per_cycle >= 1`. **To turn it off:** set
`enabled = false` or delete the block — the ledger keeps
accumulating passively (still visible in `aivyx-pa learning`)
but no correction proposals are filed.

By default the signal is structural, not semantic: "you came
right back" is a coarse proxy that can't tell a genuine rework
from praise or an unrelated follow-up.

**LLM-judged corrections (Phase 178).** Closing that gap, an
opt-in judge classifies each correction's follow-up message
into `rework` / `praise` / `unrelated` and folds **only**
genuine reworks. Enable it alongside (or instead of) the
proposal pass:

```toml
[correction_judgment]
enabled = true
max_corrections_per_cycle = 30   # optional, default 30
```

When armed, the reflection-cron correction fold runs one
batched LLM call per cycle (bounded cost) over the cycle's
detected corrections and drops `praise` / `unrelated`
follow-ups instead of counting them. The judge reads the
follow-up's message, which is captured (truncated, encrypted)
on the recall log when the follow-up turn fired auto-recall; a
follow-up with **no** recall has nothing to judge and falls
back to the structural signal (counted). A parse/LLM failure
also falls back — a transient outage never loses a signal.
`aivyx-pa learning` shows the last cycle's `rework / praise /
unrelated / structural` counts.

> **Privacy note.** Enabling this stores a truncated copy of
> the operator's query text on the (HKDF-encrypted) recall log.
> The *audit* chain stays transcript-free; the *recall* log
> gains query content. `[correction_judgment]` off → no query
> text is captured beyond what auto-recall already needs.

By default, corrections are attributed only to the topics
recalled into the corrected turn — so a turn that fired no
auto-recall is invisible to the signal.

**Tool correction attribution (Phase 179).** Closing that gap,
the reflection cron now surfaces each turn's **tools** on its
outcome summary (the stable scope base of every `ToolCall` —
`fs.read`, `gmail.send` — which the reflection LLM also sees in
its prompt). Opt in to attribute corrections to those tools:

```toml
[correction_signal]
attribute_tools = true
```

When on, the correction fold also counts the corrected turn's
tools, keyed `tool:<scope_base>`, **driven by the outcome chain
rather than the recall log** — so it catches the no-recall
turns the topic attribution misses. The `tool:` keys are
namespaced (they never collide with topic keys) and show up in
`aivyx-pa learning`'s accumulated corrections. Off → the ledger is
topic-only (byte-identical to Phase 172).

## Reminders (Phase 183)

One-shot reminders: *"remind me to call mom at 6pm"* → a
notification arrives at 6pm. Three agent tools, daemon-native
(the reminder driver pushes through the notify dispatcher — a
capability a separate tool process can't provide):

- **`remind.set`** — `{ "at": <unix seconds or RFC3339>,
  "message": <text>, "notify_targets"?: [..] }`. The agent
  resolves your natural language ("6pm", "in 2 hours") to an
  absolute `at` using the current time; you can read it back with
  `remind.list`. Omit `notify_targets` to use every configured
  notify target.
- **`remind.list`** — pending reminders, soonest first.
- **`remind.cancel`** — `{ "id": <reminder id> }`.

The reminder driver checks for due reminders on a cadence
(default every 30 s; tune with `[reminders] check_interval_secs
= N`), delivers each through your configured notify targets
(see *Email notifications* / *Web UI desktop notifications* /
the Telegram backend), and clears it. Delivery is **at-least-
once**: a reminder due while the daemon is down fires on the next
tick after restart.

Reminders are **Trusted-tier** — a SemiTrusted remote adapter
can't set them (they push notifications). One-shot only;
recurring reminders are the `[[schedule]]` cron surface.

## Autonomous loop — the Aivyx PA Ralph loop (Phase 173)

Aivyx PA's native answer to the "Ralph" technique
(snarktank/ralph): an autonomous, self-re-arming task loop.
You stock a **backlog** of stories; the loop fires a
**fresh-context agent turn per iteration**, each picking the
next story, implementing it, running gates, committing, and
marking it done — re-arming until the backlog is empty or a
hard cap is hit. Durable state lives in git, the backlog, and
memory — not in a single long model context.

The backlog is an **HMAC-chained substrate**
(`KeyDomain::LoopBacklog`) — tamper-evident, capability-gated,
and queryable via the `aivyx-pa loop` CLI. The agent already has
`git` + `shell` in the thirteen-tool core, so each iteration
can commit and run gates; two new channel-tier tools
(`loop.next` / `loop.complete`) let it walk the backlog.

### Stocking the backlog (works without arming a run)

```
aivyx-pa loop add "Add a --json flag to the report command" \
  --body "Acceptance: report --json emits valid JSON; tests pass." \
  --priority 50
aivyx-pa loop list
```

Lower `--priority` numbers run first; ties break by insertion
order. `--priority` defaults to `[loop].default_priority` (or
`100`). The backlog is daemon-owned, so these commands need a
running daemon (`aivyx-pa daemon run`).

### Arming + driving runs

Runs are **opt-in** and fully autonomous once started. Arm the
driver in `~/.config/aivyx-pa/aivyx-pa.toml`:

```toml
[loop]
enabled = true
max_iterations = 25          # optional, default 25 — the iteration cap
default_priority = 100       # optional, default 100

# Phase 174 — driver-side gate verification (strongly recommended).
gate_command = "cargo test"  # the driver re-runs this to verify the tree
gate_timeout_secs = 600      # optional, default 600 — kill + treat as red
working_dir = "/path/to/repo"  # optional, default: the daemon's CWD
max_run_secs = 7200          # optional — wall-clock cap, in seconds

# Phase 175 — cross-iteration progress log.
progress_inject_count = 20   # optional, default 20; 0 disables injection

# Phase 176 — per-run token budget (cost control).
max_run_tokens = 2000000     # optional — stop the run past this many tokens
```

Then, with the daemon running:

```
aivyx-pa loop start                      # run to backlog-done or a cap
aivyx-pa loop start --max-iterations 5   # lower the iteration cap for this run
aivyx-pa loop status                     # driver state, gate + cap config, live tokens used
aivyx-pa loop stop                       # end the run after the current iteration
aivyx-pa loop log [--limit N]            # the cross-iteration progress notes
aivyx-pa loop skip <story-id>            # prune a stuck / unwanted backlog story
```

`aivyx-pa loop status` shows `tokens used: N / cap` once a run has
had an iteration, so you can watch spend approach the
`max_run_tokens` budget live. `aivyx-pa loop skip <id>` marks a
`Pending` story `Skipped` (it stays in the append-only backlog
chain as a skip, so the audit trail is preserved). The agent
records progress notes with `loop.note`; an exact repeat of the
most-recent note is de-duplicated so the injected progress block
stays clean.

A run stops on exactly one condition: the backlog drains,
`max_iterations` is reached, the `max_run_secs` wall-clock cap
is reached, a **gate run goes red**, or you `aivyx-pa loop stop`.
Every iteration is a `TriggerSource::Loop` turn in the audit
chain.

Validation (only when `enabled = true`): `max_iterations >= 1`,
and `gate_timeout_secs >= 1` when a `gate_command` is set.
`aivyx-pa loop start` requires the section armed and a restart
after enabling.

### Safety posture (read this before your first run)

The loop **writes code and commits** each iteration — the
highest-trust-stakes action Aivyx PA takes. The guardrails:

- **Driver-side gate verification (Phase 174).** When
  `gate_command` is set, the driver runs it **before the first
  iteration** (refuse to start on a red tree) and **after every
  iteration**. On red, the run stops immediately with a
  `gate failed after iteration N` reason. This is the strongest
  guard — set it. Without it, the driver trusts the agent's
  `loop.complete`.
- **The `max_iterations` cap**, the optional **`max_run_secs`
  wall-clock cap**, and the optional **`max_run_tokens` token
  budget** — three hard bounds on blast radius regardless of
  what the agent does. The token budget sums the input+output
  tokens of every turn that completes during the run (read from
  the audit chain) and stops the run once they exceed the cap;
  it's a token cap, not a dollar cap, and needs an audit log
  configured to take effect.
- **Capability gating** — loop turns run at the Trusted tier;
  a remote (SemiTrusted) adapter cannot drive a loop.
- **The audit chain** — every iteration is recorded.
- **The canonical prompt** — instructs the agent to mark a
  story done ONLY after gates pass and the change is committed.

**The gate command runs at daemon privilege** — it is operator
config (like a cron), executed directly by the daemon, not an
agent tool, so it is not capability-gated. Point it only at a
command you trust (your build/test invocation).

**Stop-on-red is non-destructive:** the driver does *not* roll
back the agent's commit when the gate fails — it halts the run
and preserves the commit for you to inspect and revert. Damage
is bounded to one iteration.

### Cross-iteration learning — the progress log (Phase 175)

Each iteration runs in a **fresh context**, so without help it
would re-learn the same things every time. The progress log
fixes that: the canonical prompt directs the agent to record
one-line learnings with the **`loop.note`** tool (a gotcha, a
convention, where tests live), and the driver injects the last
`progress_inject_count` notes (default 20) into the top of
**every** subsequent iteration under a `## Progress so far`
heading. Unlike embedding-based recall, this is deterministic —
the notes are *always* in context.

Notes are durable (they live in the memory substrate under the
reserved `loop:progress` topic) and persist across runs, so
knowledge about your codebase accumulates over time. Inspect
them any time with `aivyx-pa loop log`. Set
`progress_inject_count = 0` to disable injection.

With the cap trio complete (iterations + wall-clock + tokens),
gate verification, and the progress log, the loop arc is
substantially done. Still deferred (Phase 177): **loop-only
token attribution** (today `max_run_tokens` counts *all* turns
during the run window, which is the safe over-counting
direction), a real **cost model**, and a **Web UI loop pane**.

## Tool observability (Phase 102)

`aivyx-pa tools` is the read-only window onto the tool layer —
the sibling of `aivyx-pa learning`:

```
aivyx-pa tools [--window <secs>]
```

It lists every registered tool and annotates each with
audit-derived call statistics: total calls, the outcome
breakdown (completed / failed / denied / …), and average
call duration. `--window <secs>` scopes the stats to a
recent slice; without it the whole audit chain is summed. A
tool that has never been called still appears — a
registered-but-unused tool is itself a signal — and a row
marked `[unregistered]` is a capability base with call
history but no currently registered tool. Like `aivyx-pa
memory` and `aivyx-pa learning`, it is daemon-backed: it needs
a running daemon (`aivyx-pa daemon run`).

## Learning insights (Phase 78)

The Phase 77 loop changes behaviour on its own. Phase 78 makes
that **legible** — an autonomous system you can't see is one
you can't trust. There's a read-only view of *what the
assistant has learned and why*, with full CLI + Web UI parity:

```
aivyx-pa learning [--window <secs>]
```

and a **Learning** tab in the Web UI. Both show the same two
things:

- **A digest** — over the lookback window: how many recalls
  happened and how many scored, how many memories the
  retention actuator is keeping warm vs. letting age out, the
  count of recall-driven Persona proposals, and the top
  helpful / least-helpful topics. This answers "is the loop
  healthy and what is it leaning toward."
- **Proposal provenance** — for each Pending (or resolved)
  recall-driven Persona proposal: the topic, its net score,
  the agent's stated reason, and the actual recalls/turns that
  produced the score (timestamp, turn outcome, whether each
  helped or hurt). This answers "*why* did it propose to
  change its Persona" — the highest-trust-stakes question,
  since you approve/reject those proposals.

It is **read-only**: approve/reject still happens through
`aivyx-pa persona proposals` / the Proposals pane. Nothing here
is configurable and nothing is persisted for it — the view is
computed on demand from the live recall log, so it always
matches what the loop actually did. The horizon is bounded by
the ~30-day recall-event retention; with no `[embedding]` /
no recall yet, it simply reports an empty digest (a valid
"nothing learned yet", not an error).

```
aivyx-pa learning --window 604800   # last 7 days
```

## Adaptive Persona (Phase 79)

Before Phase 79 the **entire** accreted Persona — every learned
context note, character trait, communication adaptation the
reflection loop has ever written — was injected into *every*
system prompt, unbounded and identical regardless of the turn.
As the Soul matures over months that grows without limit and
dilutes its own signal. Phase 79 makes it **adaptive**: each
turn the assistant injects only the Persona facets
semantically relevant to your message.

**The always-on core (the safety invariant).** Selection only
ever applies to the *soft* list facets. The scalar identity
(`assistant_name`, `operator_profile`, `communication_style`)
and **every `behavioral_constraint`** are injected in full on
every turn, unconditionally — they can never be selected away.
Your declared identity and your guardrails always apply; only
which *learned* facets surface is contextual.

**Only engages when it matters.** With no `[embedding]`
configured, **or** while the Soul is still small (below an
internal facet threshold), the full Persona is injected
exactly as before — byte-identical to pre-Phase-79. The
feature is invisible until the Persona is actually large
enough to need bounding; an embed failure also falls back
silently. It is never a regression and never an error. There
is nothing to configure (a `[persona]` tuning block is a
deferred follow-up).

**Where to see it.** Each turn the daemon log prints
`aivyx-pa persona: injected N/M facets`, and the same
selected/total appears in the `aivyx-pa learning` view and the
Web UI **Learning** tab ("Adaptive Persona: N/M facets
injected last turn") — the Phase 78 trust surface, extended:
an adaptive Soul stays legible.

## Proactive surfacing (Phase 80)

For 79 phases the assistant only ever acted when prompted — a
turn, a cron, a webhook. Phase 80 lets it **reach out first**:
on its existing reflection cadence it notices a concrete,
high-confidence reason to surface something and sends it
unprompted — *"you noted X 29 days ago, it expires
tomorrow"*; *"your `deploy/` notes keep helping, here's the
cluster."*

An unprompted **outbound** message is the highest-trust-stakes
thing the assistant can do, so it ships **off by default,
hard-capped, and fully explainable**:

- **Opt-in.** With no `[proactive]` section (or
  `enabled = false`) the pass is a complete no-op — exactly
  pre-Phase-80 behaviour. Nothing reaches out unless you ask
  it to.
- **Structural gate, no extra LLM.** It surfaces only when it
  can point to a concrete reason in one of three conservative
  signal classes: a memory within a day of TTL eviction
  (`signal_ttl_expiry`), a topic whose Phase 77 net
  helpfulness is strongly positive (`signal_recall_cluster`),
  or a `@due:`-marked reminder whose time has arrived
  (`signal_due_reminder`). No model judges *whether* to
  interrupt you — the Phase 77 no-self-judgement ethos applied
  to the highest-stakes action.
- **Hard volume cap.** At most `max_per_window` sends per
  `window_secs` (default **3 per day**), enforced
  deterministically on top of Phase 73's per-target
  rate-limit. Proactive is a scalpel, not a feed.
- **Never nags.** Every surfaced item's deterministic id is
  recorded in an encrypted, HKDF-isolated `ProactiveLog`
  store; the same item is never surfaced twice across cycles.
  Rows GC on the reflection cadence (~30-day retain).

**Configure it** in `~/.config/aivyx-pa/aivyx-pa.toml`:

```toml
[proactive]
enabled = true
target  = "me"          # a configured notify target name
max_per_window = 3       # optional, default 3
window_secs    = 86400   # optional, default 86400 (1 day)
# Each signal class defaults ON when proactive is enabled;
# set to false to mute one. At least one must stay on.
signal_ttl_expiry     = true
signal_recall_cluster = true
signal_due_reminder   = true
```

Validation (only when `enabled = true`): `target` non-empty,
`max_per_window >= 1`, `window_secs >= 1`, at least one signal
class on. **To turn it off:** set `enabled = false` or delete
the `[proactive]` block.

**Where it's recorded.** Every send lands in the notify
history (the existing `AutoNotifyDispatched` audit event, same
as any auto-notify). The daemon log prints a per-cycle
breadcrumb `aivyx-pa proactive: schedule … — surfaced N
(deduped D, capped C)`, and the last cycle's outcome — items,
their `reason` provenance, dedup/cap counts — appears in the
`aivyx-pa learning` view and the Web UI **Learning** tab
("proactive: N surfaced last cycle"), the Phase 78 trust
surface extended once more.

## Persona lifecycle (Phase 81)

For 80 phases the Persona ("Soul") only ever **grew** — the
reflection loop adds facets, none ever consolidated a
redundant one or retired a stale one. Over months a Soul that
only accretes dilutes its own signal and can contradict
itself; Phase 80 raised the stakes (a bloated Soul now also
drives proactive sends). Phase 81 gives the Persona a
**lifecycle**: on the existing reflection cadence the
assistant notices near-duplicate and long-unreinforced
soft-list facets and **proposes** consolidation or decay.

Identity is the highest-stakes layer, so it ships **off by
default, propose-only, core-protected, and fully reversible**:

- **Opt-in.** With no `[persona_lifecycle]` section (or
  `enabled = false`) the pass is a complete no-op — exactly
  pre-Phase-81 behaviour. It also needs a
  `[[reflection_schedule]]` (it piggybacks that cron) and an
  `[embedding]` provider (consolidation embeds facets).
- **Propose-only — the loop never edits identity.** Every
  action is filed as a normal *Pending* `PersonaProposal` you
  approve or reject in `aivyx-pa persona` / the Web UI. Nothing
  changes the Soul until you say so, and `aivyx-pa persona
  revert` undoes any approved action (it is a plain
  `RemoveList` delta on the chain).
- **The always-on core is structurally untouchable.** Only
  the six *soft* lists (`primary_use_cases`,
  `behavioral_preferences`, `learned_context`,
  `communication_adaptations`, `character_traits`,
  `relationship_milestones`) are ever considered. The scalar
  identity and **every `behavioral_constraint`** are excluded
  by construction — the Phase 79 always-on-core invariant
  extended to this layer.
- **Conservative, no extra LLM.** *Consolidate*: facets whose
  embeddings are near-identical (cosine above
  `consolidation_similarity`) — it proposes removing the
  shorter near-duplicates and keeping the longest (canonical)
  one. *Decay*: a facet whose originating delta is older than
  `decay_max_age_secs` with no later delta in its category
  (active curation suppresses decay). Never acts on a list
  with fewer than `min_soft_facets` entries. No model judges
  *whether* to act.
- **Never nags.** A deterministic proposal id means an action
  already filed (in any status, including a prior *Rejected*)
  is never re-proposed.

**Configure it** in `~/.config/aivyx-pa/aivyx-pa.toml`:

```toml
[persona_lifecycle]
enabled = true
consolidation_similarity = 0.92   # optional, default 0.92
decay_max_age_secs = 7776000      # optional, default ~90d
min_soft_facets = 6               # optional, default 6
# Each class defaults ON when enabled; set false to mute one.
# At least one must stay on.
signal_consolidate = true
signal_decay = true
```

Validation (only when `enabled = true`):
`consolidation_similarity` in `(0.0, 1.0]`,
`decay_max_age_secs >= 1`, `min_soft_facets >= 1`, at least
one signal class on. **To turn it off:** set
`enabled = false` or delete the `[persona_lifecycle]` block.

**Where to see it.** The daemon log prints
`aivyx-pa persona-lifecycle: schedule … — proposed N
(deduped D)`, and the last cycle's proposed actions + their
`reason` provenance appear in the `aivyx-pa learning` view and
the Web UI **Learning** tab ("persona lifecycle: N proposed
last cycle"). Filed proposals show up in `aivyx-pa persona`
exactly like reflection-driven ones — the Phase 78 trust
surface extended once more.

### Helpfulness-driven decay (Phase 85)

Phase 81 decay is **age-only** — a weak proxy. Phase 85 makes
decay consult the durable Phase 82 helpfulness ledger so the
Soul retires identity that **demonstrably stopped helping**,
and *keeps* old identity that **still helps**:

- **Precise, not fuzzy.** Only a facet whose recall topic is
  *known exactly* is helpfulness-gated. Facets the
  recall-feedback loop produced carry a `recall-fb:{topic}`
  provenance that survives onto the persona chain; everything
  else (reflection-authored facets — no topic linkage) stays
  age-only, byte-identical to Phase 81.
- **Symmetric.** A topic with sustained-negative helpfulness
  can trigger its facet's decay *before* the age horizon (a
  facet that keeps hurting shouldn't wait a quarter);
  symmetrically, a sustained-*positive* topic **protects** an
  age-old facet from age-decay.
- **Conservative evidence.** "Sustained" means the topic's
  decayed ledger score is at/below `decay_unhelpful_threshold`
  (negative) **and** it has at least `decay_min_samples`
  observations — identity is never retired (or protected) on
  thin evidence.
- **Same safety posture.** Still propose-only, operator-gated,
  `Revert`-able, core-protected — every Phase 81 property is
  unchanged. With no helpfulness ledger it degrades gracefully
  to pure age-only.

Two optional knobs on the **same `[persona_lifecycle]`**
block (gated by the existing `signal_decay`):

```toml
[persona_lifecycle]
enabled = true
# … Phase 81 knobs …
decay_unhelpful_threshold = -2.0  # optional, default -2.0
decay_min_samples = 3             # optional, default 3
```

Validation (only when enabled and `signal_decay` is on):
`decay_unhelpful_threshold < 0.0`, `decay_min_samples >= 1`.
**To keep pure age-only behaviour:** don't run auto-recall
(no ledger), or leave the knobs at defaults — a facet is only
ever helpfulness-decayed when its `recall-fb` topic has
genuinely, sustainedly hurt. Decay proposals cite the
evidence (e.g. *"topic 'deploy' net -8.2 over 14 windows
(sustained low helpfulness)"*) in the same `aivyx-pa persona` /
Phase 78 surface.

### Pattern-driven decay (Phase 88)

The decay-side complement of Phase 87's pattern-driven
proposals. Phase 87 makes the co-occurrence ledger drive
Persona *construction* — a durable affined pair proposes a
new `learned_context` facet; Phase 88 makes the **same
ledger** drive Persona *decay*: when the pair underlying an
already-applied `consolidate-pair:` facet has demonstrably
weakened, the facet's justification is gone — propose to
retire it. The opposite move lands symmetrically: a still-
durable pair **protects** its facet from age-decay (the
relationship still applies, so the identity still applies).

After Phase 88, the assistant retires identity when the
**relationship** behind it dissolves — not only when the
underlying *topic* stopped helping. Every existing safety
property carries: propose-only, operator-gated, `Revert`-
able, core-protected.

- **Conservative, single-signal gate.** The pair's decayed
  Phase 83 affinity must be **below** the
  `decay_pair_below_affinity` floor (default `1.0` — mirrors
  Phase 87's `min_affinity` so the construction floor and the
  decay floor coincide by default). Endpoint helpfulness is
  **not** double-consulted: the facet's justification IS the
  relationship's durability, and tying decay to individual
  topic helpfulness would leave drifted-but-warm pair facets
  in place forever — the very case Phase 88 is meant to
  handle.
- **Symmetric protection.** A pair whose decayed affinity is
  *still* at or above the floor protects its `consolidate-
  pair:` facet from age-decay. Mirrors the Phase 85
  protection arm; reuses the same OR-protection machinery in
  the detector.
- **Provenance-only.** Only facets whose origin delta has a
  `consolidate-pair:{A}+{B}` proposal_id are pair-gated.
  Every other facet (reflection-authored, recall-feedback-
  derived) follows whatever signal it already had — age-only,
  or the Phase 85 helpfulness path.
- **Graceful fallback.** With no co-occurrence ledger (no
  auto-recall configured), the pair arm sits out entirely —
  byte-identical to Phase 85.

One new knob on the **same `[persona_lifecycle]`** block
(gated by the existing `signal_decay`):

```toml
[persona_lifecycle]
enabled = true
# … Phase 81 + 85 knobs …
decay_pair_below_affinity = 1.0  # optional, default 1.0
```

Validation (only when enabled and `signal_decay` is on):
`decay_pair_below_affinity` finite and ≥ `0.0`. **To widen
the keep-zone:** tune it *below* Phase 87's `min_affinity`
to add explicit hysteresis (e.g. propose at affinity ≥ 1.0,
decay only at affinity < 0.5). Decay proposals cite the
pair + the decayed affinity ("co-occurrence pair `deploy` +
`rollback` decayed affinity 0.30 (below floor 1.00);
relationship no longer durable") in the same `aivyx-pa persona`
/ Phase 78 surface.

## Persistent helpfulness ledger (Phase 82)

For 81 phases the "did recalling this topic actually help"
signal was **ephemeral**: the recall-feedback loop (Phase 77)
recomputed it each reflection cycle over a lookback window and
discarded it. Phase 82 makes it **durable and longitudinal** —
a per-topic, time-decayed accumulation that survives restarts
and spans sessions, so the assistant can show you what has
*consistently* helped, not just what helped this week.

- **Zero-config and automatic.** Like the recall-feedback
  loop itself (Phase 77) and the recall log, there is **no
  `[helpfulness_ledger]` block** — nothing to turn on. It is
  built and folded automatically whenever auto-recall is
  configured (an `[embedding]` provider + a
  `[[reflection_schedule]]`). With auto-recall off it simply
  does not exist.
- **It changes no behaviour on its own.** It is a *passive*
  longitudinal signal. The recall-feedback loop's retention
  bias and Persona proposals are byte-identical to
  pre-Phase-82 — the ledger is folded in *after* those
  actuators run.
- **Recency-weighted (it forgets, on purpose).** Each
  reflection cycle the stored per-topic score is first decayed
  by an exponential half-life (~60 days), then this window's
  net helpfulness is added. A topic that used to help but
  hasn't lately fades on its own — the durable signal tracks
  *current* relevance, not a frozen all-time tally.
- **Self-pruning.** A topic whose decayed score has fallen to
  effectively zero *and* has not been touched for ~90 days is
  dropped on the same reflection cadence. Storage growth
  mirrors the signal's own decay; nothing accumulates forever.
- The half-life and prune bounds are code constants (tuning
  is a deferred follow-up, exactly as the recall-log's 30-day
  retention is fixed).

**Where to see it.** Run `aivyx-pa learning [--window <secs>]` or
open the Web UI **Learning** tab: alongside the existing
*windowed* "Most/Least helpful topics" there is now an
**"Accumulated helpfulness (all-time, decayed)"** block — each
topic with its signed decayed score and a sample count (your
confidence proxy: one cycle is not a trend). The daemon log
prints `aivyx-pa helpfulness-ledger: folded N topic(s), pruned M`
each cycle. Nothing to configure.

## Cross-session pattern learning (Phase 83)

Phase 77 learns *which topics help*; Phase 82 made that
durable. Phase 83 learns the relationships *between* topics:
which two topics get **recalled together** in turns that go
well. Over many sessions a stable picture emerges — "whenever
`deploy runbook` is recalled, `rollback steps` is too, and
those turns succeed" — and that is exactly the cross-session
structure a personal assistant should internalize.

- **Zero-config and automatic.** Like the recall-feedback
  loop (Phase 77) and the helpfulness ledger (Phase 82),
  there is **no config block** — it is built and folded
  automatically whenever auto-recall is configured (an
  `[embedding]` provider + a `[[reflection_schedule]]`). With
  auto-recall off it does not exist.
- **It changes no behaviour on its own.** A *passive*
  cross-session signal: it is folded in *after* the
  recall-feedback actuators and the Phase 82 ledger, so both
  remain byte-identical. (Acting on the patterns —
  cluster-aware recall, pattern-driven proposals — is a
  deliberate future phase.)
- **What a "pattern" is.** For each recall turn, the
  **top-8 highest-scoring distinct topics** are paired up;
  every unordered pair gets that turn's helpfulness signal
  (+ if it went well, − if not). The per-pair signal is
  accumulated with the same ~60-day exponential half-life as
  the Phase 82 ledger — a relationship that *used* to hold but
  hasn't lately fades on its own.
- **Bounded.** The top-8 cap keeps a turn that recalled 30
  memories from exploding into hundreds of pair rows; the
  ledger self-prunes (decayed-to-zero **and** ~90 days
  untouched → dropped), so storage tracks the live signal.

**Where to see it.** `aivyx-pa learning` and the Web UI
**Learning** tab now show a **"Topics that consistently help
together"** block — each pair with its signed decayed score
and a sample count. The daemon log prints
`aivyx-pa cooccurrence: folded N pair(s), pruned M` each cycle.
Nothing to configure.

## Cluster-aware co-recall (Phase 84)

Phases 82–83 built durable learning *surface-only*. Phase 84
is the first phase that **acts** on it. Auto-recall (Phase
76) surfaces only the memories whose text your turn
semantically matched. With cluster-aware co-recall, when a
topic A is recalled, the durable siblings B that have
*consistently helped alongside A across sessions* (the Phase
83 co-occurrence ledger) are **also** surfaced — even when
the literal query never retrieved them. Recall becomes
associative: the assistant brings what *goes with* what you
asked about, not just the keyword match.

Because this is the first time the assistant changes what the
model sees on the hot path, it ships **opt-in, hard-bounded,
budget-neutral, and self-policing**:

- **Opt-in.** With no `[recall_cluster]` section (or
  `enabled = false`) recall is byte-identical to pre-Phase-84.
  It also needs auto-recall configured (`[embedding]` +
  the co-occurrence ledger, which exists once auto-recall
  has been running).
- **Budget-neutral.** Injected siblings **share** the
  existing `rag_top_k` budget — they displace the *weakest*
  primary hits, so recall context never grows: zero extra
  token cost, no context bloat.
- **Hard-bounded.** At most `max_siblings` per turn, and only
  pairs whose decayed co-occurrence score clears
  `min_affinity` — weak/noisy affinities never reach context.
- **Self-policing.** Cluster-injected hits are marked and
  **excluded from the Phase 83 co-occurrence fold**, so the
  ledger never learns from its own expansion (no runaway
  self-reinforcement). They *do* count in the Phase 77/82
  helpfulness signal, so a bad expansion organically lands in
  worse turns and the affinity that drove it decays away.

**Configure it** in `~/.config/aivyx-pa/aivyx-pa.toml`:

```toml
[recall_cluster]
enabled = true
max_siblings = 3   # optional, default 3 — hard per-turn cap
min_affinity = 1.0 # optional, default 1.0 — decayed-score floor
```

Validation (only when `enabled = true`): `max_siblings >= 1`,
`min_affinity > 0.0`. **To turn it off:** set
`enabled = false` or delete the `[recall_cluster]` block.

**Where to see it.** The daemon log prints
`aivyx-pa recall-cluster: injected N affined sibling(s)` on turns
that expand, and `aivyx-pa learning` / the Web UI **Learning**
tab show a **"Cluster co-recall (last turn)"** block — the
injected count and each `driver → sibling` pair.

## Conversational-window relevance (Phase 86)

For 85 phases auto-recall (Phase 76) and adaptive Persona
selection (Phase 79) judged relevance off **one line** — the
latest user message. In a real multi-turn conversation the
topic drifts, the operator's intent spans several turns, and a
single line is a lossy proxy. Phase 86 gives both consumers a
**recent conversational window**: a small recency-ordered slice
of the last few turns (user + assistant) concatenated into the
*same* single embedding the relevance ranking already makes —
so recall pulls memories the multi-turn intent points at, and
the Soul selects facets matched to the actual thread of
conversation, not the literal last sentence.

The window is sharper *input* for the existing rankers; every
downstream guarantee (the `rag_min_similarity` floor, the
Phase 79 always-on-core invariant, the Phase 84 budget-neutral
sibling injection) is unchanged.

- **Opt-in, byte-identical by default.** A new
  `[embedding].recall_window_turns` knob defaults to `1` —
  exactly today's single-message behaviour. The window engages
  *only* when an operator raises it; no existing operator's
  recalled context changes on upgrade.
- **Recency, current message last.** When engaged, the
  embedded query is the last `recall_window_turns - 1` prior
  turns (oldest → newest, role-labelled `user:` / `assistant:`)
  followed by the current message — placed **last** so it
  dominates the embedding. Char-budgeted: prior turns are
  dropped oldest-first to fit; the current message is never
  truncated.
- **Ephemeral.** The buffer lives in daemon memory only —
  a restart starts fresh. Durable per-session transcripts are
  intentionally not persisted (recall context is re-derivable
  from memory + the Phase 82/83 ledgers; the *chatter* is not
  itself the record).
- **Applies to both consumers.** Auto-recall (Phase 76) and
  adaptive Persona selection (Phase 79) share the same handle
  and the same knob — the deferral came from both phases and
  fixing one without the other was incoherent.
- **Safety net unchanged.** A drifted window that drags in
  noise is filtered by the existing `rag_min_similarity` /
  Persona-selection floors; a stale window never injects a
  weakly-related memory or facet.

**Configure it** in `~/.config/aivyx-pa/aivyx-pa.toml`:

```toml
[embedding]
# … existing knobs …
recall_window_turns = 3   # optional, default 1 (= pre-Phase-86)
```

Validation (only when `[embedding]` is present):
`recall_window_turns >= 1`. **To turn it off:** leave the knob
at the default (or set `recall_window_turns = 1`) — recall and
Persona selection embed just the latest message, byte-identical
to the pre-Phase-86 path. The buffer caps the window at 16
turns regardless of the knob (the relevant signal is recency,
not a transcript).

## Pattern-driven Persona proposals (Phase 87)

Phase 84 made auto-recall **act** on the Phase 83 co-occurrence
ledger (durable affined siblings on the hot path). Phase 85
made Persona decay **act** on the Phase 82 helpfulness ledger
(sustained-negative topics retire identity). Phase 87 closes
the symmetric arc: the same co-occurrence ledger now drives
Persona *construction* too — durable, consistently-co-occurring
pairs of *helpful* topics propose a new `learned_context`
facet so the Soul learns the *relationships* between topics,
not just the per-topic warmth.

The actuator ships through the **existing Phase 70 proposal
chain** — same propose-only + edit-then-approve + `Revert` +
core-protected flow. Phase 87 only adds a new *source* of
proposals; the resolution path is unchanged.

- **Opt-in, off by default.** Like Phase 80/81/84, this is an
  actuator block. With no `[persona_consolidation]` section
  (or `enabled = false`) the pass never runs — byte-identical
  to pre-Phase-87. It also needs auto-recall configured (the
  Phase 82 helpfulness ledger and the Phase 83 co-occurrence
  ledger only exist once auto-recall has been running).
- **Conservative double-gate.** A pair `(A, B)` only proposes
  when its decayed Phase 83 affinity clears `min_affinity`
  AND has at least `min_samples` observations AND **both
  endpoints'** Phase 82 helpfulness scores are at least
  `min_topic_helpfulness`. A pattern of topics that
  individually hurt is never proposed (mirroring Phase 85's
  evidence-floor discipline).
- **LLM-summarized facets.** Each surviving pair is handed to
  the same reflection LLM the agent uses; it phrases one
  short factual statement (under 30 words) that lands as a
  Pending `LearnedContext` facet. The operator reviews — and
  may edit — the prose before approving. A per-candidate LLM
  hiccup skips that pair; a cycle-wide LLM outage is recorded
  on the **Learning** surface so you can distinguish a quiet
  cycle from a broken one.
- **Reflection cadence, dedupless, capped.** Runs on your
  existing `[[reflection_schedule]]` cron — the same trigger
  every "act on durable learning" pass uses (77, 82, 83, 85).
  Cross-cycle dedup is absolute: a pair already present in
  the proposal chain (any status — Pending, Approved,
  Rejected, Superseded) is never re-filed. Per-cycle filings
  are bounded by `max_proposals_per_cycle` (default `3`) so
  the review queue can never flood.

**Configure it** in `~/.config/aivyx-pa/aivyx-pa.toml`:

```toml
[persona_consolidation]
enabled = true
min_affinity = 1.0           # optional, default 1.0
min_samples = 3              # optional, default 3
min_topic_helpfulness = 0.0  # optional, default 0.0 (non-negative)
max_proposals_per_cycle = 3  # optional, default 3
```

Validation (only when `enabled = true`):
`min_affinity > 0.0`, `min_samples >= 1`,
`min_topic_helpfulness` finite,
`max_proposals_per_cycle >= 1`. **To turn it off:** set
`enabled = false` or delete the `[persona_consolidation]`
block — the Persona proposal pipeline is byte-identical to
pre-Phase-87.

**Where to see it.** The daemon log prints
`aivyx-pa persona-consolidation: schedule "X" — filed N` on
cycles that fire (with `(LLM unavailable)` appended when the
LLM is unreachable). `aivyx-pa learning` / the Web UI
**Learning** tab show a **"Pattern-driven Persona proposals
(last cycle, opt-in)"** block with the filed pair list, or
the engaged-but-quiet / LLM-down / off cases. The proposals
themselves appear in `aivyx-pa persona proposals` + the
**Proposals** pane exactly like reflection-driven and
lifecycle proposals — each one with provenance citing the
specific co-occurrence pair (`co-occurrence pair X + Y —
decayed affinity N over M observation(s); both topics
helpful`).

### Pattern-driven supersession (Phase 92)

Phase 87 proposes new `consolidate-pair:` facets when a
durable + helpful pair shows up; Phase 88 decays old facets
when their pair weakens. But a real operator workflow shifts
continuously — `(auth, jwt)` dominates one quarter, then
`(auth, sessions)` the next. Today the actuator handles this
as **two independent operator decisions**: Phase 88 proposes
decay of the old facet, Phase 87 proposes the new one.
Nothing tells the operator they're logically linked.

Phase 92 adds opt-in **pattern-driven supersession**: when
an existing applied `consolidate-pair:{A}+{B}` facet's pair
has decayed below the Phase 88 floor AND a new pair
`(A, C)` sharing one endpoint has strengthened above the
Phase 87 floor (both endpoints helpful), the consolidation
pass files the `RemoveList` + `AppendList` proposals
**linked by metadata** so the operator-facing surface
presents them as a single supersession decision.

- **Opt-in, off by default.** With
  `[persona_consolidation].enable_supersession = false`
  (the default) the Phase 87 / Phase 88 proposal flow is
  byte-identical to pre-Phase-92.
- **Shared-endpoint detection.** The old pair `(A, B)` and
  the new pair `(A, C)` must share exactly one endpoint —
  conservative, deterministic, fires only on clear
  "replacement" relationships. Pairs that drift to
  unrelated `(C, D)` clusters are not supersessions; the
  Phase 87/88 flow handles those as two phases.
- **Linked, not atomic.** Each half is filed as a separate
  proposal on the existing Phase 70 chain (no new proposal
  kind, no chain-schema migration). The
  `supersedes_proposal_id` field on `ProposedPersonaDelta`
  carries the cross-link: the `AppendList`-side points at
  the `RemoveList`-side and vice versa. The operator can
  still approve one half and reject the other (operator
  flexibility); the linkage is **operator-visible context**
  for grouping, not a chain-level atomic primitive.
- **Reuses Phase 87's LLM phraser.** The new facet's prose
  comes from the same `PairPhraser` Phase 87 already uses;
  no second LLM dependency. Per-candidate phrasing failure
  → skip that supersession this cycle (the facet stays via
  the standard Phase 87/88 flow on a future cycle).

Enable it in `~/.config/aivyx-pa/aivyx-pa.toml`:

```toml
[persona_consolidation]
enabled = true
enable_supersession = true   # optional, default false
# ... other Phase 87 knobs ...
```

**What you'll see.** Each supersession produces TWO chain
entries (counted as `filed = 2` on the surface; the
`superseded` counter on `aivyx-pa learning` shows the
supersession event count). The `RemoveList` half's `reason`
cites the new proposal as the replacement; the
`AppendList` half's `reason` cites the old proposal as the
one being superseded. Both appear in the **Proposals** pane
with their normal per-proposal `Revert` actions.

#### Grouped rendering (Phase 94)

Phase 94 closes the first Phase 92 deferral: the CLI and the
Web UI Persona-pane both render linked supersession pairs
as a single grouped unit instead of two unrelated rows.

- **CLI (`aivyx-pa persona proposals`).** Linked pairs render
  with a `└─ supersedes:` indicator under the
  `AppendList`-side row and a `└─ superseded by:`
  indicator under the `RemoveList`-side row. The
  `RemoveList` side always comes first regardless of
  input order. Standalone proposals render byte-identical
  to pre-Phase-94.
- **Web UI Proposals tab.** Linked pairs render as one
  outer card with a `↔ linked supersession (Phase 92)`
  banner header, both halves stacked with a
  `↓ supersedes ↓` arrow between them, and a single
  shared action row: primary **Approve both** + a
  **⋮ Split** menu offering partial actions (`Approve
  RemoveList only`, `Approve AppendList only`, `Reject
  RemoveList only`, `Reject AppendList only`) + **Reject
  both**.

The Web UI **Approve both** action fires two sequential
`ResolvePersonaProposal` IPC calls (RemoveList first,
then AppendList). Phase 92's `each half independently
Revert-able` guarantee covers the half-approved failure
mode without needing a transactional IPC primitive — the
operator finishes via the next refresh.

The grouping is pure client-side rendering: the IPC
contract is unchanged from Phase 92 (the
`supersedes_proposal_id` field on
`PersonaProposalSummary` was already wire-compatible). The
same algorithm runs on both surfaces (Rust helper on the
CLI side, line-for-line JS port on the Web UI side); both
defend the same edge cases (self-reference, dangling
partner id, asymmetric link, same-op pair) by degrading to
standalone rendering.

## Reflection auto-loop (Phase 70)

Phase 70 closes the self-learning half of **P14 Persona**: the
agent observes its own behavior, proposes Persona deltas, and
the operator reviews them asynchronously in a dedicated Web
UI Proposals pane or `aivyx-pa persona proposals` CLI subcommand.

**What's running by default after install:** nothing
auto-reflects. Reflection happens when the agent calls
`reflection.propose` (existing tool, Phase 29). Anything that
fires a reflection turn — operator prompt, mission, or
scheduled `[[schedule]]` block with a reflection-flavored
prompt — produces proposals that land in the new persistent
proposal store and surface in the operator review pane.

**Reviewing proposals:**

```sh
# Terminal:
aivyx-pa persona proposals list                       # status: pending (default)
aivyx-pa persona proposals list --status approved
aivyx-pa persona proposals show <proposal_id>
aivyx-pa persona proposals approve <proposal_id>
aivyx-pa persona proposals reject <proposal_id> --reason "too aggressive"
```

Or open the Web UI at `http://127.0.0.1:7843/` (when enabled)
and click the **Proposals** tab. The pane lets you approve,
reject, or **edit-then-approve** — tweak the proposed op JSON
inline before the daemon applies it. Both the original proposed
op and the operator-applied op are preserved in the proposal
chain for audit (Q3(a) at sign-off).

**Storage isolation.** Pending and resolved proposals live in
a separate encrypted domain (`KeyDomain::PersonaProposals`,
table `aivyx_persona_proposals_v1`) from the approved-delta
persona chain. The HMAC chain uses a distinct genesis seed so
a chain-confusion attack (a Pending row inserted into the
persona chain or vice versa) is structurally rejected at MAC
verification (Q4(a)).

**Pair with Web UI desktop notifications** (Phase 69) for the
tightest review feedback loop: every proposed delta can fire
a `kind = "web-ui"` notification so the browser tab pings the
operator the moment a proposal lands.

**Cron-fired auto-reflection (Phase 71)** runs on the
configured cron. Declare one or more `[[reflection_schedule]]`
blocks in `aivyx-pa.toml`:

```toml
[[reflection_schedule]]
name = "nightly-reflection"
cron = "0 0 23 * * *"          # 11pm daily
lookback_window_secs = 86400   # last 24 hours
```

The daemon spawns a scheduler task at startup (one line per
registered schedule in the boot banner) that fires a reflection
turn at each cron boundary. The reflection turn carries the
canonical reflection prompt plus the lookback-window's
TurnStarted/TurnEnded outcome summaries from the audit chain;
the agent uses `reflection.propose` to record Persona deltas as
Pending rows for asynchronous operator review.

The canonical prompt is intentionally conservative: it tells
the agent to propose only when a pattern recurs in ≥3 distinct
turns within the window, prefer narrower categories
(`BehavioralPreferences`, `LearnedContext`,
`CommunicationAdaptations`) over identity-level changes, and
return empty when no clear pattern emerges. An empty reflection
turn is valid and preferred over speculation.

### Cadence learning — skip-when-idle (Phase 95)

By default the reflection cron fires on every cron boundary
regardless of how much activity happened in the lookback
window. Phase 95 adds opt-in **skip-when-idle**: when
`skip_when_idle = true` on a `[[reflection_schedule]]`, the
scheduler reads the audit-chain growth since the last *fired*
cycle for that schedule. If growth is below
`min_audit_entries_to_fire`, the cycle is skipped entirely
(no LLM calls for Phase 87 phrasing / Phase 91 judgment /
Phase 92 supersession — just a log line + a counter bump).

The operator's `cron` remains the **upper bound** on firing
rate. Cadence learning is monotonic-slower-only: the
scheduler can suppress a fire, never schedule one.

```toml
[[reflection_schedule]]
name = "nightly-reflection"
cron = "0 0 23 * * *"
lookback_window_secs = 86400
skip_when_idle = true                # opt-in, default false
min_audit_entries_to_fire = 50       # default 1
```

The first cycle after a daemon boot fires unconditionally
(no prior baseline to compare against). Subsequent cycles
consult audit-growth. The `last_fired_audit_len` cursor is
updated only on actual fires; a long run of skips
accumulates growth until the threshold is crossed and the
next cycle fires.

Validation: `min_audit_entries_to_fire >= 1` is required
when `skip_when_idle = true` (zero would skip every cycle
unconditionally; the loader rejects this at config time).

**What you'll see.** Each skipped cycle logs
`aivyx-pa reflection: schedule "X" — skipped (audit-growth K
below threshold M)`. The `aivyx-pa learning` surface adds a
**Reflection cadence (Phase 95)** block with one line per
schedule that's made cadence decisions:

```
Reflection cadence (Phase 95):
  nightly-reflection: 7 fired, 2 skipped
```

Schedules with both counts at zero (or schedules the
operator hasn't enabled `skip_when_idle` on) are omitted
from the block to avoid noise.

## Uninstall

```sh
# If you installed it as a service (Chapter Anchor), remove that first —
# stops + disables the unit and deletes its secret env file.
aivyx-pa daemon uninstall 2>/dev/null

# Remove the binary
rm "$(command -v aivyx-pa)"

# Stop and remove the daemon socket/PID (if a daemon is still around)
aivyx-pa daemon stop 2>/dev/null
rm -f /run/user/$UID/aivyx-pa.sock /run/user/$UID/aivyx-pa.pid

# Remove the encrypted store and config (DESTROYS YOUR DATA)
rm -f ./aivyx-pa.toml /tmp/aivyx-pa-store.redb
# Adjust paths to match your config's [storage] path.
```

The encrypted store contains your conversation history, audit
chain, memory, missions, schedules, profile, and persona deltas.
Deleting it is irreversible — there is no cloud backup by
design (PRODUCT.md G6).
