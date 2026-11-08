# Containerized Deployment (Chapter Harbor)

> **Status:** ✅ **shipped** (Chapter Harbor HB.0–HB.5 complete). Aivyx PA ships as a
> **docker-compose appliance**: a one-command, always-on daemon + Studio for a
> homelab box or VPS, no Rust toolchain on the operator's machine. The two opt-in
> daemon changes (`[studio] host` §4.1, `[studio] allowed_origins` §4.2; `web_ui_host` /
> `web_ui_allowed_origins` under `[daemon]` before 1.0) are in; the
> `Dockerfile` + `docker-compose.yml` + baked appliance config are **built and
> run end-to-end** (HB.1 §9); the OAuth-in-Docker recipe is documented (§7); the
> operator install steps live in
> [`INSTALL.md`](INSTALL.md#docker--the-server-appliance); and a CI workflow
> publishes the image to GHCR on each version tag (HB.5). This document remains
> the locked design reference (framing in §2, the security-sensitive daemon
> changes in §4).
>
> **CI verified + live** — every version tag builds and pushes
> `ghcr.io/aivyx-agent/aivyx` (the `v0.5.0` Docker Publish run is green), the GHCR
> package is **public** (anonymous `docker pull` works — `latest` tracks the newest
> release), and the image is **verified to boot**: `docker run … -e
> AIVYX_PA_PROVIDER=ollama -e AIVYX_PA_MODEL=… --network host` brings the daemon 0.5.0 up
> (store unlocked, socket listening, Studio served). No operational TODOs remain.

## 1. The gap — there is no "just run it" server deployment

Today Aivyx PA installs as a native binary (the cargo-dist [shell installer](INSTALL.md#shell-installer-recommended))
or builds from source. Both put the daemon on the operator's own machine, which
is right for the **desktop, local-first** experience. There is **no packaged way
to stand up Aivyx PA as an always-on service** — the homelab / VPS / "give me an
assistant at `studio.mybox.lan`" deployment — without manually installing the
binary, the tool processes, a config, and a process supervisor on the host.

`docker compose up` is the idiomatic answer for that audience. It is also the
natural home for the **headless execution mode** already on the roadmap
([`HEADLESS_MODE.md`](HEADLESS_MODE.md)): a non-interactive, gates-don't-block
daemon *is* a containerized daemon. The two reinforce each other.

## 2. The key insight — this is a deployment *profile*, not a second product

The single most important decision in this chapter is **what Harbor is not**.

Aivyx PA's core promise is **local-first**: the agent reaches *your* files, *your*
devices, *your* network, and your key talks straight to the LLM. A container
inverts most of that — "the filesystem" is the container's, "localhost" is the
container's, there are no audio devices. So Harbor is **explicitly the
server-appliance profile**, not a drop-in for the native desktop install:

| Native install (desktop, local-first) | → Harbor (server appliance) |
|---|---|
| Reaches the host's real files (`access` home/full) | Reaches a **bind-mounted data volume** (`/work`) — intentional, scoped |
| OAuth via a host browser + loopback | OAuth via a **documented published-port recipe** |
| Voice (host audio devices) | **No voice** (no `/dev/snd` in the profile) |
| Interactive passphrase prompt | **`AIVYX_PA_PASSPHRASE` via a Docker secret** |
| Studio on `127.0.0.1:7843` for the local user | Studio exposed deliberately, **opt-in**, with auth/TLS in front |

Pitching Harbor as "the same thing, in Docker" would half-defeat the local-first
model and mislead users. Pitching it as "Aivyx PA-as-an-appliance" is honest and
genuinely valuable. **The docs lead with that distinction.**

## 3. What already works (more container-ready than expected)

- **Non-interactive unlock exists.** The daemon sources the store passphrase from
  `AIVYX_PA_PASSPHRASE` (`aivyx-channel::passphrase`, `DEFAULT_ENV_VAR`) before
  falling back to the interactive prompt. A detached container unlocks with **no
  code change** — supplied as a **Docker secret / file**, not a bare env var
  (§6).
- **Tool processes are just binaries.** Bake `aivyx-gmail` / `aivyx-calendar` /
  `aivyx-drive` / `aivyx-contacts` / `aivyx-notion` / `aivyx-obsidian` /
  `aivyx-n8n` / `aivyx-toolkit` into the image and the `[[tool_process]]`
  `command` entries resolve — no host PATH setup for the user.
- **musl static builds exist.** cargo-dist already produces musl artifacts
  (Chapter Q), so the runtime image can be **distroless / `scratch`** — small,
  fast to pull, minimal attack surface.
- **The config + state is one directory.** Everything lives under `~/.aivyx-pa/`
  (config, encrypted store, per-tool tokens) → a single named volume.

## 4. The two REQUIRED daemon changes (both opt-in, security-sensitive)

The web UI server is **deliberately localhost-only**, which collides with Docker
networking. Harbor needs two small daemon changes — both **default-off**, so
every existing native install behaves identically.

### 4.1 Configurable web bind host (`web_ui.rs:158`)

`run_web_ui_server` binds `SocketAddr::from(([127, 0, 0, 1], port))` — hardcoded
loopback. Docker port-publishing forwards host:port → the **container's
`0.0.0.0`**, never its loopback, so a 127.0.0.1-bound process is **unreachable
even with `-p 7843:7843`**. Harbor adds an opt-in bind host (e.g.
`[studio] host = "0.0.0.0"`, default `127.0.0.1`; `[daemon] web_ui_host` before 1.0). **Required even for the
localhost-only spike** (the container must bind `0.0.0.0` for the forward to land).

### 4.2 Configurable WS Origin allowlist (`web_ui.rs:~269`)

The `/ws` upgrade enforces an Origin allowlist hardcoded to `http://127.0.0.1:<port>`
and `http://localhost:<port>` (the CSWSH / DNS-rebinding defense — keep it). When
Studio is published to **host** localhost on the same port, the browser still
sends `Origin: http://localhost:7843`, which **passes** — so a localhost-only
spike needs *only* §4.1. But accessing from another machine
(`http://studio.mybox.lan:7843`) sends a non-allowlisted Origin and is
**rejected**. Remote access therefore needs an opt-in allowlist
(`[studio] allowed_origins = ["https://studio.mybox.lan"]`), empty by
default (= localhost-only, today's behavior). Loosening it is a real exposure;
pair it with the built-in auth token (below) and TLS terminated at a proxy.

### 4.3 Web UI auth token (`[studio] auth_token`, Chapter Postern)

The `/ws` WebSocket is the Studio's control plane (agent turns, config writes,
memory reads). Binding off-host without auth leaves it open to anyone on the
network. Set `[studio] auth_token = "<opaque>"` (e.g. `openssl rand -hex
32`) to require it: the browser is prompted via HTTP Basic on first load (the
token is the password; any username), a cookie is planted, and the `/ws`
upgrade requires that cookie; non-browser clients send `Authorization: Bearer
<token>`. Default-off (unchanged localhost posture); the daemon warns loudly if
bound off-host with no token. This closes the open door, but is **not** a TLS
substitute — still terminate TLS at a reverse proxy for remote access.

> **Why this is in the contract.** Harbor is not just packaging — it touches the
> daemon's network-exposure security posture. Locking these two opt-in knobs
> (defaults unchanged) *before* coding is the whole point of writing this first.

## 5. The frictions, and how Harbor handles each

1. **Filesystem reach.** `access` home/full means "reach the host's files"; in a
   container it means "reach the bind-mounted `/work` volume." For an appliance
   working in a data volume this is correct and even desirable. The docs set
   `access` to a workspace rooted at `/work` and explain the boundary. *Not a
   bug — a reframing.*
2. **OAuth loopback.** `aivyx-pa connect` opens a **host** browser to
   `127.0.0.1:<port>/callback`, but that loopback is the container's — and the
   listener binds `127.0.0.1` *inside* the container, which Docker port-publish
   can't reach (it forwards to the container's `0.0.0.0`). Google also **requires**
   a loopback `redirect_uri` for desktop clients (the code rejects non-loopback
   hosts), so the "publish the port + host-mapped redirect" idea doesn't apply.
   Resolution: run the one-shot consent in a container that **shares the host
   network** (so the container's loopback *is* the host's), writing tokens into
   the shared volume — see §7 for the full recipe. Doc, not code.
3. **Web exposure.** Covered by §4. Default localhost-only; remote is opt-in with
   auth/TLS guidance.
4. **Local models (Ollama).** Bundling Ollama as a **sibling compose service** is
   a zero-setup local-LLM on-ramp (`http://ollama:11434` over the compose
   network). **GPU passthrough** (the nvidia container runtime) adds host setup,
   so it's an **optional profile**; CPU-only works out of the box (slow). Users
   with Ollama already on the host point at `host.docker.internal:11434` instead.
5. **Voice — excluded.** Needs host audio devices; INSTALL.md already notes
   containers don't do audio. Harbor is daemon + Studio + tools (+ optional
   Ollama). Voice stays a native host CLI.
6. **Passphrase posture.** `AIVYX_PA_PASSPHRASE` works but a passphrase in a plain
   env var is weaker than an interactive prompt (visible in `docker inspect` /
   process env). Harbor uses a **Docker secret mounted as a file** and documents
   the tradeoff honestly — it is the appliance security posture, a deliberate
   step down from the desktop prompt.

## 6. The minimal compose shape (target of HB.1)

```yaml
services:
  aivyx:
    image: ghcr.io/aivyx-agent/aivyx:latest      # HB.5 publishes this
    ports: ["127.0.0.1:7843:7843"]               # host-localhost only by default
    volumes:
      - aivyx-pa-data:/root/.aivyx-pa              # config + encrypted store + tokens
      - ./workspace:/work                         # the agent's fs_root (access level)
    environment:
      - AIVYX_PA_PROVIDER=anthropic                # cloud profile first (no GPU/OAuth)
      - ANTHROPIC_API_KEY=${ANTHROPIC_API_KEY}
    secrets: [aivyx_pa_passphrase]                 # → AIVYX_PA_PASSPHRASE via file
    # config (baked or mounted) sets: [studio] host="0.0.0.0", enabled=true,
    #   [access] level rooted at /work
  # ── optional local-model profile ──
  ollama:
    image: ollama/ollama:latest
    profiles: ["ollama"]
    volumes: [ollama-data:/root/.ollama]
volumes: { aivyx-pa-data: , ollama-data: }
secrets: { aivyx_pa_passphrase: { file: ./secrets/passphrase } }
```

The runtime image is multi-stage: a musl/static build stage → a distroless
runtime carrying the `aivyx-pa` daemon **plus every tool-process binary**, with the
embedded Studio bundle (already in the daemon binary). `CMD` runs the daemon
(`aivyx-pa daemon run`).

## 7. OAuth in Docker — the recipe (HB.3)

Connecting a Google tool (Gmail / Calendar / Drive / Contacts / …) needs a
one-time browser consent. Two facts force the shape of this (§5.2):

- `aivyx-<svc> auth init` binds its callback listener on **`127.0.0.1:<port>`
  inside the container**, which a published port can't reach.
- Google **requires** a loopback `redirect_uri` for desktop OAuth clients — you
  cannot point it at the container's routable address.

The clean resolution is to run the consent **once** in a throwaway container that
**shares the host network namespace**, so the container's `127.0.0.1` *is* the
host's. Consent happens in the host browser; the resulting `config.toml` +
`tokens.json` are written straight into the shared `aivyx-pa-data` volume, and the
long-running daemon container picks them up.

### Linux (host networking)

```sh
# One-shot: the interactive connect wizard, on the host network, writing into
# the same volume the daemon uses. Prompts for the Google client_id/secret,
# prints a consent URL to open in your host browser, captures the redirect on
# host-loopback, and offers to add the [[tool_process]] entry to the mounted
# aivyx-pa.toml.
docker compose run --rm -it \
  --network host \
  --entrypoint aivyx-pa \
  aivyx connect contacts

# then restart the daemon so it loads the new tokens + [[tool_process]] entry
docker compose restart aivyx
```

Because `connect` writes to `/root/.aivyx-pa/...` (the `aivyx-pa-data` volume) and the
daemon mounts the same volume, the credential and the config edit are visible to
the daemon after the restart. Nothing is published; the consent listener lives
on host-loopback only for the duration of the one-shot.

### Docker Desktop (macOS / Windows) — host networking caveat

`--network host` does not share host-loopback the same way on Docker Desktop.
There, run the per-service auth on the **host** instead — `aivyx-pa connect <svc>`
with a natively-installed `aivyx-pa` + `aivyx-<svc>` (the cargo-dist binaries) — then
make the resulting `~/.aivyx-pa/tool-processes/<svc>/` directory available to the
container (copy it into the `aivyx-pa-data` volume, or bind-mount it). The token
file is host-portable; only the consent step needs native loopback.

### Why not a code change?

Host networking keeps HB.3 **doc-only**, as the contract intends. A future
browser-cold-start daemon mode (§4) would change the calculus — at that point a
configurable callback bind-host on `auth init` (mirroring §4.1) becomes the
cross-platform fix, and the host-networking dance retires. Tracked, not built.

## 8. What's deliberately *not* here

- **Not a Kubernetes chart / Helm.** One box, one compose file. K8s is a later
  call if demand appears.
- **Not voice, not host-device access** (§5).
- **Not a managed/multi-tenant service.** Single-operator appliance; no built-in
  user management. Multi-user is a product question, not this chapter.
- **Not auth/TLS termination.** Harbor documents putting a reverse proxy
  (Caddy/Traefik) in front for remote exposure; it does not ship one.
- **Not a desktop-install replacement** (§2).

## 9. Phase plan

| Phase | Deliverable |
|---|---|
| **HB.0** | This contract. |
| **HB.1** | ✅ The **cloud-provider spike**: multi-stage `Dockerfile` (debian-slim runtime, daemon + all tool binaries) + `docker-compose.yml` for the Anthropic profile (no Ollama, no OAuth) + baked appliance config + secret-bridging entrypoint. Includes the §4.1 `web_ui_host` change (opt-in). **Built + ran end-to-end** (Docker 29.5, legacy builder): image builds (290 MB), daemon boots, Studio reachable through the published port, state persists across `down`/`up`, passphrase bridged from the Docker secret. Three build-verify fixes: `WORKDIR /root/.aivyx-pa` (the daemon reads `./aivyx-pa.toml` from the CWD), the appliance role named `default` (the active role), and a legacy-builder Dockerfile (no `buildx`/BuildKit cache mounts required). |
| **HB.2** | ✅ The §4.2 opt-in **Origin allowlist** (`web_ui_allowed_origins`, default empty = localhost-only) + the F-4 non-loopback startup warning + the optional **Ollama sibling** (CPU default; opt-in GPU override `deploy/docker/compose.gpu.yml`). |
| **HB.3** | ✅ The **OAuth-in-Docker recipe** (§7) — corrected from the original sketch: the callback listener binds container-loopback + Google mandates a loopback `redirect_uri`, so the flow uses **host networking** (Linux) / a host-run binary (Docker Desktop), tokens landing in the shared volume. Doc, not code. *Recipe is code-read-verified, not yet live-run against a real Google app.* |
| **HB.4** | ✅ **Docs**: this file's status flipped to *usable from source* + an [INSTALL.md "Docker"](INSTALL.md#docker--the-server-appliance) section leading with the appliance-vs-desktop framing (§2), the passphrase-secret + exposure/TLS guidance, the Ollama/GPU + OAuth pointers, and the worked compose quick-start. |
| **HB.5** | ✅ **CI image publish**: `.github/workflows/docker-publish.yml` builds + pushes `ghcr.io/aivyx-agent/aivyx` on each version tag (same glob as the cargo-dist `release.yml`) + on `workflow_dispatch` (tag `edge`). Linux/amd64, buildx + GHA layer cache. **First run green** (manual dispatch, ~9.5 min cold cache): built + pushed `ghcr.io/aivyx-agent/aivyx:edge` with OCI source/revision labels. **Live on version tags** (`v0.5.0` green, tags `0.5.0`/`0.5`/`latest`); the GHCR package is **public** — anonymous `docker pull` verified, and the image boots clean against Ollama. No operational TODOs remain. |

## 10. Open questions (resolved)

- **F-1 (HB.1):** does the image **bake a default `aivyx-pa.toml`** and let a mounted
  config override it? **Resolved: yes** — `deploy/docker/aivyx.appliance.toml` is
  baked and the entrypoint seeds it on first boot; mount your own to override.
- **F-2 (HB.1):** distroless vs `debian-slim` runtime. **Resolved: debian-slim**
  (+ ca-certificates) for the spike — reliable, no static-linking surprises, and
  rustls/RustCrypto means no OpenSSL anyway. Musl/distroless stays a future size
  optimization, re-evaluated once the image is actually built (HB.5).
- **F-3 (HB.2):** Ollama GPU profile in-repo or documented only? **Resolved:** a
  committed-but-opt-in override (`deploy/docker/compose.gpu.yml`), layered
  explicitly; GPU is never the default path.
- **F-4 (HB.2):** reverse-proxy push for non-localhost exposure. **Resolved:** a
  one-line daemon startup warning when `web_ui_host` is non-loopback, plus the
  README exposure recipe (host + allowlist + TLS). Implemented in HB.2.
