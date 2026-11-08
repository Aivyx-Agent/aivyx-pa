# Aivyx PA in Docker (Chapter Harbor)

Run Aivyx PA as an always-on **server appliance** — daemon + Studio in one
`docker compose up`, no Rust toolchain on your machine. This is **not** the
desktop local-first install: in a container the agent reaches a bind-mounted
volume (not your real home), the Studio is exposed deliberately, and there's no
voice. See [`docs/DOCKER.md`](../../docs/DOCKER.md) for the full framing and
security posture.

## Quick start (cloud provider — HB.1)

```sh
# 1. set the store passphrase (Docker secret — never an env var in plain sight)
printf '%s' 'a-strong-passphrase' > deploy/docker/secrets/passphrase

# 2. give it an API key
export ANTHROPIC_API_KEY=sk-ant-...

# 3. bring it up (builds the image the first time)
docker compose up --build

# 4. open the Studio and create your agent
open http://localhost:7843     # → use the "Create" screen (Chapter Genesis)
```

State (config, encrypted store, audit chain, OAuth tokens) persists in the
`aivyx-pa-data` volume across `docker compose down`/`up`. The agent's files live in
`./workspace` (mounted at `/work`).

## What's in here

| File | Purpose |
|---|---|
| [`../../Dockerfile`](../../Dockerfile) | Multi-stage build: daemon + all tool binaries → debian-slim |
| [`../../docker-compose.yml`](../../docker-compose.yml) | The appliance service (+ optional `ollama` profile) |
| `aivyx.appliance.toml` | Baked default config (mount your own to override) |
| `entrypoint.sh` | Bridges the passphrase secret → `AIVYX_PA_PASSPHRASE`; seeds `~/.aivyx-pa` on first boot |
| `secrets/` | Your store passphrase (git-ignored; only `passphrase.example` is tracked) |

## Notes & limits (this phase)

- **Localhost only by default.** The compose publishes to `127.0.0.1:7843`.
  Remote access needs **both** `[studio] host = "0.0.0.0"` *and*
  `[studio] allowed_origins = ["https://your-host"]` (the daemon rejects
  off-host WS origins otherwise), plus TLS/auth in front — don't just widen the
  port binding. The daemon prints a one-line exposure warning when it binds a
  non-loopback host.
- **Local models.** The `ollama` sibling service is optional
  (`docker compose --profile ollama up`) and runs CPU-only by default. For
  NVIDIA GPU passthrough, layer in `deploy/docker/compose.gpu.yml` (needs the
  NVIDIA Container Toolkit on the host). Voice is out of scope.
- **OAuth tools** (Gmail/Calendar/Drive/Contacts/…) are baked into the image but
  the in-container consent flow needs the published-callback recipe (HB.3).
- **Verification.** Built + run end-to-end locally (Docker 29.5, legacy
  builder): image builds, daemon boots, Studio serves on `:7843`, state
  persists across `down`/`up`. Uses the legacy builder (no `buildx` needed);
  `DOCKER_BUILDKIT=0 docker build` is implied.
- **Published image (HB.5).** `.github/workflows/docker-publish.yml` pushes
  `ghcr.io/aivyx-agent/aivyx` on each version tag (and `:edge` on manual
  dispatch). To run from the published image instead of building, drop the
  `build:` block in `docker-compose.yml` and set
  `image: ghcr.io/aivyx-agent/aivyx:latest`. **One-time:** the GHCR package
  starts private — make it public in the org's package settings so users can
  pull without authenticating.
