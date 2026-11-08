# Curated MCP Server Recipes

This is the canonical catalog of MCP servers that pair cleanly with
Aivyx PA today. Each recipe shows a paste-able `[[mcp_server]]` block,
an inline `[mcp_server.sandbox]` block (sandbox-by-default is the
Phase 55 substrate posture; copy-paste should produce a sandboxed
config out of the gate), the required environment variables, and
notes on what capability scopes the agent gets when the server is
enabled.

For the CLI surface, run `aivyx-pa mcp recipes` to list every recipe
in-shell, or `aivyx-pa mcp recipes <name>` to print one recipe's worked
snippet directly to stdout (handy for piping into `aivyx-pa.toml`).

> **Sandboxing note.** Every recipe in this doc shows a
> `[mcp_server.sandbox]` block. The wrapper choices below (`bwrap`,
> `firejail`, `docker`) are illustrative; substitute whatever
> command-wrapper sandbox you already trust on your operator host.
> See [`docs/TOOL_SDK.md`](TOOL_SDK.md) §9 for the substrate-level
> sandbox-layer reference; the wrapper / args shape is the same one
> `[[tool_process]]` uses.

> **Capability-scope note.** Once an `[[mcp_server]]` entry is
> loaded, the daemon registers each of its tools under
> `mcp.call:<server-name>:<tool-name>`. Roles that need to invoke
> the server must declare a matching scope — either `mcp.call`
> (broad: any MCP tool), `mcp.call:<server-name>:*` (mid: any tool
> on one named server, via glob dispatch), or
> `mcp.call:<server-name>:<tool-name>` (narrow: one specific tool).
> The recipes below use the middle granularity as the default since
> "if this server is enabled, the agent can use any of its tools"
> is the most common operator intent.

> **Resources.** Discovery is **capability-gated** on the server's
> `initialize` response. A server that declares the `tools` capability
> contributes its tools as above; one that declares `resources`
> additionally contributes two resource-access tools —
> `mcp.<server>.resources.list` (catalog the resources it exposes) and
> `mcp.<server>.resources.read` (read one by `uri`) — so the agent can
> read the **context** a server publishes, not just call its tools.
> A server that declares `prompts` likewise contributes
> `mcp.<server>.prompts.list` (catalog its reusable prompt templates)
> and `mcp.<server>.prompts.get` (retrieve one rendered with `name` +
> `arguments`). All of these reuse the same `mcp.call` scope (each is a
> call to the server), so no extra scope is needed, and the client
> never probes a method the server didn't declare.

> **Live list changes (hot-swap).** The client fully handles
> `tools/list_changed` (and the resources / prompts variants) **without
> a restart**. A shared per-server `McpConn` demuxes a server's
> interleaved notifications during normal calls and records which
> primitives changed; a background coordinator in the daemon polls that
> signal, calls `rediscover()`, and swaps exactly that server's tools in
> the live `ToolRegistry` (an `RwLock`-backed set). A round-trip lock on
> the connection keeps the coordinator's re-discovery from racing
> in-flight agent tool calls. It's **capability-safe**: the refreshed
> tools carry the same `mcp.call:<server>` scope family the role already
> granted, so a swap never widens what the agent can do.

> **Transports.** The recipes below are **stdio** (local child
> processes, the common case). Remote servers use one of two HTTP
> transports — drop the `command`/`sandbox` lines and give a `url`:
>
> ```toml
> [[mcp_server]]
> name = "remote"
> transport = "http"          # modern Streamable HTTP (MCP 2025-03-26+)
> url = "https://mcp.example.com/mcp"
> # transport = "sse"         # or the legacy HTTP+SSE pair
> ```
>
> `transport = "http"` (alias `"streamable-http"`) speaks the modern
> single-endpoint transport: one POST per request, JSON or SSE
> response, with the server-assigned `Mcp-Session-Id` carried
> automatically. Sandboxing is stdio-only (there's no local child to
> wrap on a remote transport).

> **Secrets & auth (Chapter Conduit).** Two `[[mcp_server]]` fields
> carry credentials, both with `${VAR}` interpolation resolved from the
> **daemon's own environment** at load — so the secret lives in your
> shell/systemd environment, never in `aivyx-pa.toml`. A `${VAR}` that is
> unset at startup is a hard config error (fail loud, not a silent empty
> token); write `$$` for a literal `$`.
>
> - **`env`** (stdio servers) — environment variables for the child,
>   e.g. a token:
>
>   ```toml
>   [[mcp_server]]
>   name = "github"
>   command = "npx"
>   args = ["-y", "@modelcontextprotocol/server-github"]
>   env = { GITHUB_PERSONAL_ACCESS_TOKEN = "${GITHUB_TOKEN}" }
>   ```
>
> - **`headers`** (sse/http servers) — HTTP headers on every request,
>   e.g. bearer auth to a remote server (rejected on stdio):
>
>   ```toml
>   [[mcp_server]]
>   name = "remote"
>   transport = "http"
>   url = "https://mcp.example.com/mcp"
>   headers = { Authorization = "Bearer ${EXAMPLE_API_KEY}" }
>   ```
>
> Operator headers can add auth but never override the protocol-reserved
> headers (`Accept`, `Content-Type`, `MCP-Protocol-Version`,
> `Mcp-Session-Id`). The native `env` field supersedes the older
> sandbox-`--setenv` trick (and works with or without a sandbox; a bwrap
> wrapper inherits the child's environment).

> **Diagnosing a server that didn't come up.** Run **`aivyx-pa mcp status`**
> after starting the daemon: it lists each configured server as
> connected (with its tool count) or failed (with the error and the
> server's captured stderr — the usual culprits are a missing `command`,
> an unset `${VAR}`, or a bad token). Stdio stderr is captured (last 50
> lines) rather than discarded.

## Catalog at a glance

| Recipe | Operator-touch frequency | Notes |
|---|---|---|
| [`filesystem`](#filesystem) | High | Read/write a single directory |
| [`github`](#github) | High | Repo / issue / PR operations |
| [`gitlab`](#gitlab) | Medium | Symmetric for GitLab users |
| [`sqlite`](#sqlite) | Medium | Local `.db` file |
| [`postgres`](#postgres) | Medium | Read-only Postgres |
| [`time`](#time) | Medium | Timezone math; no deps, no network |
| [`fetch`](#fetch) | Medium | HTTP GET into markdown |
| [`brave-search`](#brave-search) | Medium | Brave Search API |
| [`slack`](#slack) | Medium | Channel read + post |
| [`memory`](#memory) | Low | Knowledge-graph (distinct from Aivyx PA's own memory) |
| [`puppeteer`](#puppeteer) | Low | Headless browser; higher blast radius |
| [`everything`](#everything) | First-run | Reference / smoke-test server |
| [`aivyx-coder`](#aivyx-coder) | Low | Delegate coding tasks to a local aivyx-coder process |

---

## filesystem

Read/write files inside a configured directory. The official
`@modelcontextprotocol/server-filesystem` is the most-asked-for
first MCP server.

**Required env:** none.
**Capability scopes the agent gets:** `mcp.call:fs-local:*`.

```toml
[[mcp_server]]
name = "fs-local"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "/home/me/projects"]

[mcp_server.sandbox]
# bubblewrap binds the same path the server is told about so a
# typo in the args can't escape into $HOME.
wrapper = "bwrap"
args = [
    "--ro-bind", "/usr", "/usr",
    "--ro-bind", "/etc", "/etc",
    "--bind", "/home/me/projects", "/home/me/projects",
    "--dev", "/dev", "--proc", "/proc",
    "--unshare-net",   # filesystem server does not need network
    "--",
]
```

Verify it works: after `aivyx-pa daemon stop && aivyx-pa`, ask the
agent to "list the files in my projects directory" — it should
call `mcp.call:fs-local:list_directory` and return a structured
listing.

---

## github

Read/write GitHub repos, issues, and pull requests via the GitHub
REST API.

**Required env:** `GITHUB_PERSONAL_ACCESS_TOKEN` with the scopes
the agent needs (typically `repo` for private repos +
`read:org`). Supplied via the native `env` field below, interpolated
from the daemon environment (keep the token in your shell, e.g.
`export GITHUB_TOKEN=ghp_…`).
**Capability scopes the agent gets:** `mcp.call:github:*`.

```toml
[[mcp_server]]
name = "github"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
# Chapter Conduit — the token reaches the child natively; the bwrap
# wrapper below inherits it (no `--setenv` needed).
env = { GITHUB_PERSONAL_ACCESS_TOKEN = "${GITHUB_TOKEN}" }

[mcp_server.sandbox]
# Network-only sandbox: no filesystem access, network kept so
# the server can reach api.github.com.
wrapper = "bwrap"
args = [
    "--ro-bind", "/usr", "/usr",
    "--ro-bind", "/etc", "/etc",
    "--ro-bind", "/etc/ssl", "/etc/ssl",
    "--dev", "/dev", "--proc", "/proc",
    "--",
]
```

---

## gitlab

GitLab projects / issues / merge requests via the GitLab REST
API. Symmetric to the GitHub recipe above.

**Required env:** `GITLAB_PERSONAL_ACCESS_TOKEN`. Optional
`GITLAB_API_URL` (defaults to `https://gitlab.com`) for
self-hosted instances.
**Capability scopes the agent gets:** `mcp.call:gitlab:*`.

```toml
[[mcp_server]]
name = "gitlab"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-gitlab"]
env = { GITLAB_PERSONAL_ACCESS_TOKEN = "${GITLAB_TOKEN}" }
# Self-hosted? add: GITLAB_API_URL = "https://gitlab.mycorp.com"

[mcp_server.sandbox]
wrapper = "bwrap"
args = [
    "--ro-bind", "/usr", "/usr",
    "--ro-bind", "/etc", "/etc",
    "--ro-bind", "/etc/ssl", "/etc/ssl",
    "--dev", "/dev", "--proc", "/proc",
    "--",
]
```

---

## sqlite

Query / mutate a local SQLite database. The server takes one path
argument naming the `.db` file.

**Required env:** none.
**Capability scopes the agent gets:** `mcp.call:sqlite:*`.

```toml
[[mcp_server]]
name = "sqlite"
command = "npx"
args = [
    "-y",
    "@modelcontextprotocol/server-sqlite",
    "/home/me/data/notes.db",
]

[mcp_server.sandbox]
# Bind only the directory holding the db file; no network.
wrapper = "bwrap"
args = [
    "--ro-bind", "/usr", "/usr",
    "--ro-bind", "/etc", "/etc",
    "--bind", "/home/me/data", "/home/me/data",
    "--dev", "/dev", "--proc", "/proc",
    "--unshare-net",
    "--",
]
```

---

## postgres

Read-only Postgres queries against a configured connection. The
official server is read-only by design — even an operator who
grants write privileges at the database level cannot expose write
tools through this server.

**Required env:** `POSTGRES_CONNECTION_STRING` (e.g.
`postgresql://user:pass@host:5432/dbname`).
**Capability scopes the agent gets:** `mcp.call:postgres:*`.

```toml
[[mcp_server]]
name = "postgres"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-postgres"]
env = { POSTGRES_CONNECTION_STRING = "${POSTGRES_CONNECTION_STRING}" }

[mcp_server.sandbox]
wrapper = "bwrap"
args = [
    "--ro-bind", "/usr", "/usr",
    "--ro-bind", "/etc", "/etc",
    "--ro-bind", "/etc/ssl", "/etc/ssl",
    "--dev", "/dev", "--proc", "/proc",
    "--",
]
```

---

## time

Timezone-aware date / time math: current time, conversion,
arithmetic. Small, no deps, no network — the canonical
"hello world" recipe.

**Required env:** none.
**Capability scopes the agent gets:** `mcp.call:time:*`.

```toml
[[mcp_server]]
name = "time"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-time"]

[mcp_server.sandbox]
# Pure-compute server: no filesystem, no network.
wrapper = "bwrap"
args = [
    "--ro-bind", "/usr", "/usr",
    "--ro-bind", "/etc", "/etc",
    "--dev", "/dev", "--proc", "/proc",
    "--unshare-net",
    "--",
]
```

---

## fetch

HTTP GET into clean markdown. Respects `robots.txt`. Mostly
overlaps with Aivyx PA's first-party `web.fetch` tool (Phase 12); use
this recipe when an agent wants the markdown-conversion path
rather than raw response bytes.

**Required env:** none.
**Capability scopes the agent gets:** `mcp.call:fetch:*`.

```toml
[[mcp_server]]
name = "fetch"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-fetch"]

[mcp_server.sandbox]
wrapper = "bwrap"
args = [
    "--ro-bind", "/usr", "/usr",
    "--ro-bind", "/etc", "/etc",
    "--ro-bind", "/etc/ssl", "/etc/ssl",
    "--dev", "/dev", "--proc", "/proc",
    "--",
]
```

---

## Bundled `web-search` fallback backend

Aivyx PA's own bundled `web-search` MCP server (`aivyx-pa mcp-server`,
started automatically — no `[[mcp_server]]` entry needed) defaults to
DuckDuckGo's zero-config HTML search with no API key required. Under
sustained or automated use DuckDuckGo answers with HTTP **202** and a
bot-challenge page rather than real results — the bundled server
surfaces this as an explicit tool error ("the zero-config search
backend is currently unavailable") rather than a silent empty result
set, but it can't make DuckDuckGo answer.

If your operator routines (trend-scans, missions, or just chatty
day-to-day use) hit this wall, set one of these environment variables
before starting the daemon to switch to a keyed backend — priority
order is Brave, then SerpAPI, then the DuckDuckGo fallback:

- `BRAVE_SEARCH_API_KEY` — sign up at `api.search.brave.com`.
- `SERPAPI_KEY` — sign up at `serpapi.com`.

No `aivyx-pa.toml` change needed; the bundled server checks these two
env vars directly at request time. This is a *different* server from
the `brave-search` recipe below — that recipe is the official
`@modelcontextprotocol/server-brave-search` package (its own
`BRAVE_API_KEY`, its own `web_search` + `local_search` tool surface);
use it only if you specifically want that server's tool surface
instead of the bundled one.

---

## brave-search

Web + local search via the Brave Search API. Aivyx PA already ships
a bundled `web-search` MCP server with its own Brave fallback
(Phase 46) — use this recipe only if you want the official Brave
server's exact tool surface (`web_search` + `local_search`)
rather than the bundled Aivyx PA surface.

**Required env:** `BRAVE_API_KEY` (sign up at
`api.search.brave.com`).
**Capability scopes the agent gets:** `mcp.call:brave-search:*`.

```toml
[[mcp_server]]
name = "brave-search"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-brave-search"]
env = { BRAVE_API_KEY = "${BRAVE_API_KEY}" }

[mcp_server.sandbox]
wrapper = "bwrap"
args = [
    "--ro-bind", "/usr", "/usr",
    "--ro-bind", "/etc", "/etc",
    "--ro-bind", "/etc/ssl", "/etc/ssl",
    "--dev", "/dev", "--proc", "/proc",
    "--",
]
```

---

## slack

Read channel history; post messages. The `slack` MCP server is
the "agent calls into Slack as a tool" surface — distinct from the
planned Phase 108 first-party Aivyx-Slack channel adapter, which
is the "operator talks to Aivyx PA from Slack" surface. They compose.

**Required env:** `SLACK_BOT_TOKEN` (`xoxb-…`) and `SLACK_TEAM_ID`.
**Capability scopes the agent gets:** `mcp.call:slack:*`.

```toml
[[mcp_server]]
name = "slack"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-slack"]
env = { SLACK_BOT_TOKEN = "${SLACK_BOT_TOKEN}", SLACK_TEAM_ID = "${SLACK_TEAM_ID}" }

[mcp_server.sandbox]
wrapper = "bwrap"
args = [
    "--ro-bind", "/usr", "/usr",
    "--ro-bind", "/etc", "/etc",
    "--ro-bind", "/etc/ssl", "/etc/ssl",
    "--dev", "/dev", "--proc", "/proc",
    "--",
]
```

---

## memory

Knowledge-graph memory the agent maintains across turns. Distinct
from Aivyx PA's first-party `memory.*` tools — those write to
encrypted `KeyDomain::Memory` in the redb store; this MCP server
keeps a JSON knowledge graph the agent can walk by entity /
relation. Useful when an agent wants structured relational recall
that isn't a fit for the topic-keyed Aivyx PA memory.

**Required env:** none. The server writes its graph to a JSON
file inside the sandbox bind path.
**Capability scopes the agent gets:** `mcp.call:kg-memory:*`.

```toml
[[mcp_server]]
name = "kg-memory"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-memory"]

[mcp_server.sandbox]
wrapper = "bwrap"
args = [
    "--ro-bind", "/usr", "/usr",
    "--ro-bind", "/etc", "/etc",
    "--bind", "/home/me/.aivyx-pa/kg-memory", "/home/me/.aivyx-pa/kg-memory",
    "--dev", "/dev", "--proc", "/proc",
    "--unshare-net",
    "--",
]
```

---

## puppeteer

Headless-browser automation: navigate, click, screenshot,
`page.evaluate`. Higher blast radius than read-only servers — the
sandbox must constrain what URLs the headless browser can be told
to load.

**Required env:** none.
**Capability scopes the agent gets:** `mcp.call:puppeteer:*`.

```toml
[[mcp_server]]
name = "puppeteer"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-puppeteer"]

[mcp_server.sandbox]
# Docker is a better fit than bwrap for Chromium — it needs more
# pieces than a bwrap one-liner cleanly carries.
wrapper = "docker"
args = [
    "run", "--rm", "-i",
    "--cap-drop=ALL",
    "--security-opt=no-new-privileges",
    "--network=host",   # puppeteer needs internet
    "node:20-slim",
    "--",
]
```

---

## everything

The official reference / test server. Exposes one of each tool
kind so an operator can verify Aivyx PA's MCP wiring without paying
for a real-API setup. Useful first thing after `aivyx-pa init`.

**Required env:** none.
**Capability scopes the agent gets:** `mcp.call:everything:*`.

```toml
[[mcp_server]]
name = "everything"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-everything"]

[mcp_server.sandbox]
wrapper = "bwrap"
args = [
    "--ro-bind", "/usr", "/usr",
    "--ro-bind", "/etc", "/etc",
    "--dev", "/dev", "--proc", "/proc",
    "--unshare-net",
    "--",
]
```

---

## aivyx-coder

Delegate bounded coding tasks to a local `aivyx-coder` process (a
separate, sibling Aivyx product — a terminal coding agent for local
LLMs) running as `aivyx-coder --mcp-server` (coder's flag: run coder *as* an
MCP server; Aivyx PA's own flag for attaching one for a session is `--mcp`).
Unlike every other
recipe in this catalog, this is not third-party code: `aivyx-coder`
ships its own Landlock+seccomp confinement and its own tiered
access ceiling, so the sandbox block below is optional
defense-in-depth, not the thing actually keeping the operator safe.

**Prerequisite:** `aivyx-coder`'s own `config.toml` must set
`[mcp_server].max_access_level` (`"plan"` | `"edit"` | `"execute"`)
before this server can start — there is no default, and it refuses
to start unconfigured. This ceiling caps every session's access
regardless of what a specialist's model requests; set it no higher
than the specialists calling it actually need.

**Security note:** unlike a human using aivyx-coder's own TUI or
editor integration, an MCP-server session has no human to show a
permission prompt to — every tool call within the session's granted
tier auto-resolves. In particular, `max_access_level = "execute"`
means any process able to reach this server gets `run_shell`
auto-approved with no human in the loop. Landlock/seccomp still
confine what runs, but the interactive permission gate aivyx-coder
otherwise relies on does not apply to MCP-server sessions — set
`max_access_level` no higher than the specialists calling it
actually need.

**Required env:** none.
**Capability scopes the agent gets:** `mcp.call:aivyx-coder:*`.

```toml
[[mcp_server]]
name = "aivyx-coder"
command = "aivyx-coder"
args = ["--mcp-server"]

[mcp_server.sandbox]
# aivyx-coder inherits the daemon's own working directory when
# spawned over stdio (there's no separate `cwd` field to point it
# elsewhere) -- bind that same directory, read-write, so its
# fs/shell tools can actually reach your project; substitute the
# real path, matching wherever your aivyx-pa daemon runs. No
# --unshare-net here (unlike filesystem/time/everything above):
# aivyx-coder needs network to reach its own configured local LLM
# backend (Ollama/vLLM/llama-server).
wrapper = "bwrap"
args = [
    "--ro-bind", "/usr", "/usr",
    "--ro-bind", "/etc", "/etc",
    "--ro-bind", "/home/me/.config/aivyx-coder", "/home/me/.config/aivyx-coder",
    "--bind", "/home/me/projects", "/home/me/projects",
    "--dev", "/dev", "--proc", "/proc",
    "--",
]
```

Verify it works: after configuring `aivyx-coder`'s
`max_access_level` and restarting the daemon (`aivyx-pa daemon stop &&
aivyx-pa`), run `aivyx-pa mcp status` — `aivyx-coder` should show
connected with 2 tools (`code`, `code_reply`). See
`docs/NONAGON.md` §9 for a worked example wiring this into a
Nonagon specialist.

**Pointing `aivyx-coder` at the same KV-cache store as `aivyx-pa`:** if
both `aivyx-pa` and this delegated `aivyx-coder` process point at the
*same* `llama-server` instance, that server has exactly **one**
`--slot-save-path` — so both configs' kvcache store paths must agree
for save/restore accounting to work correctly at all, not just as an
optional optimization. `aivyx-kvcache`'s manifest is already safe for
two separate OS processes writing the same store directory concurrently
(WAL-mode sqlite index; eviction tolerates a file another process
already deleted). To opt in, set both configs' kvcache store path to
the *identical* absolute directory, matching whatever you pass to
`llama-server`'s own `--slot-save-path` (see `docs/INSTALL.md`'s
"KV-cache persistence" section):

```toml
# aivyx-pa's own config.toml
[kvcache]
store_path = "/home/me/.local/share/shared-kvcache"
```

```toml
# aivyx-coder's own config.toml
[backend]
kvcache_store_path = "/home/me/.local/share/shared-kvcache"
```

**What this does and doesn't buy you.** Each app computes its own cache
key from its own system prompt and tool set, so the two processes never
actually produce matching cache keys — pointing both at one directory
does *not* mean `aivyx-coder` reuses `aivyx-pa`'s prefill work or vice
versa. What it *does* buy: correct size accounting and eviction against
the one real `--slot-save-path` directory the shared `llama-server`
actually writes to (without this, whichever app's configured path
doesn't match the server's real save path silently falls back to a
1-byte placeholder size per entry, and `kvcache_max_bytes` never
triggers).

**The eviction budget is shared and asymmetric once the directory is
shared.** Both apps evict against the *same* directory using each app's
own `kvcache_max_bytes` independently — whichever app has the smaller
budget configured evicts the other's slot files first. Both default to
10 GiB, so this is invisible until an operator tunes one down.

This only helps when both sides are *already* configured against the
same `llama-server` — pointing two processes at the same directory
while they talk to two different backend servers just means two
independent, non-interfering sets of cache entries coexisting in one
folder: harmless, but pointless.

---

## Adding a recipe to this catalog

Recipes are static data in two places — `docs/MCP_RECIPES.md`
(this file) and `crates/aivyx-cli/src/bin/aivyx_modules/mcp_recipes.rs`'s
`RECIPES` slice. The `aivyx-pa mcp recipes` CLI surface reads from
the slice; the doc is the operator-facing reference. Both must be
kept in sync — if you add a recipe to one, add the matching half
to the other in the same commit. The module's
`every_recipe_has_*` tests enforce the registry's internal
contract; the doc-side counterpart is operator review.

## What's not in this catalog

Recipes shipped here are limited to MCP servers that:

1. Have an authoritative `@modelcontextprotocol/server-*` package
   on npm (the "official" set), or
2. Have been used in tree by the Aivyx PA operator long enough to
   call them stable.

Servers worth a recipe but not yet added land via a future
Chapter D follow-on (the Phase 106 recipes-only scope decision
deliberately kept new bundled-server code out of scope; new
recipes can land in a focused docs-only update).

See also:
- `examples/aivyx-pa.toml` for the broader config-file context each
  recipe drops into.
- [`docs/TOOL_SDK.md`](TOOL_SDK.md) §9 for the substrate-level
  sandbox-layer reference.
- [`docs/AUDIT_EXPORT.md`](AUDIT_EXPORT.md) for inspecting which
  MCP tools an agent has actually called (`aivyx-pa audit export |
  jq '.event.tool_id'`).
