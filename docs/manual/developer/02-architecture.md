# Architecture

A summary for orientation. The binding technical contract is
[`DESIGN.md`](https://github.com/Aivyx-Agent/aivyx-pa/blob/main/DESIGN.md)
and the product contract
[`PRODUCT.md`](https://github.com/Aivyx-Agent/aivyx-pa/blob/main/PRODUCT.md);
both change only through a formal amendment under `docs/amendments/`.

## One daemon, many front ends

```
you
 │
 ▼
front end: terminal · TUI · Studio · desktop app · Telegram · Discord · Slack · voice · your own
 │   Unix socket, mode 0600 — length-prefixed JSON
 ▼
aivyx-pa daemon
 ├── turn loop: capability check → audit → execute → audit
 ├── HMAC-chained audit log (verifiable offline)
 ├── encrypted store (redb; Argon2id → HKDF → ChaCha20-Poly1305)
 ├── built-in tools (fs, shell, git, web, memory, workspace, …)
 └── tool processes and MCP servers (separate OS processes)
```

The daemon is the only long-running process and the only one holding the
agent, its state and its keys. Every front end — including the Studio,
which is a WebAssembly app the daemon serves — speaks the same IPC protocol
([IPC protocol](06-ipc-protocol.md)). There is no protocol-level password:
the socket's `0600` mode is the boundary, so anyone who can open it is the
operator.

## The turn loop (D1)

Every tool call goes through the same four steps: **capability check →
audit → execute → audit**. Nothing runs without a scope check, and nothing
runs unrecorded. A turn is bounded: at most 32 steps, a wall-clock timeout
(120 s by default, `[agent] turn_timeout_secs`), and two loop breakers — one
for the same call repeated three times, one for short `A, B, A, B` cycles.
The loop lives in `ConcreteAgent::turn`
(`crates/aivyx-core/src/agent.rs`); it returns a `TurnOutcome` that names
every way a turn can end, rather than an error.

## Capabilities and trust (D4, D5)

A **scope** (`aivyx-capability`) is a validated string — `fs.read`,
`net.fetch:https://example.com/`, `mcp.call:github:create_issue` — whose
base must be in a fixed list, so an unknown scope fails when parsed.
`Scope::is_granted_by` is the only grant check. A role grants a set of
scopes; the channel's **trust tier** (Kernel, Trusted, SemiTrusted,
Untrusted) caps it: `effective = role_caps ∩ tier_ceiling`. Operators can
narrow a role but never widen past its tier.

## Crates

| Crate | Owns |
|---|---|
| `aivyx-core` | The `Agent`, `Tool` and `ChannelContext` traits, the turn loop, built-in tools, and the security guards (Ward, Portcullis, Rampart, Bulwark, Picket) |
| `aivyx-capability` | `Scope`, `CapabilitySet`, `TrustTier` |
| `aivyx-crypto` | Key derivation and encryption |
| `aivyx-storage` | The encrypted redb store, split into key domains |
| `aivyx-audit` | The HMAC-chained audit log and its offline verifier |
| `aivyx-config` | The TOML + environment loader, recording where each value came from |
| `aivyx-instance` | Named instances: the only place an Aivyx PA path is built |
| `aivyx-llm` | Providers (Anthropic, OpenAI-compatible, Ollama, mistral.rs, …) and routed model selection |
| `aivyx-memory` | The memory tools and store |
| `aivyx-channel` | The daemon server and client, missions, schedules, reflection, the autonomous loop, recall — the largest crate |
| `aivyx-ipc` | The wire protocol, kept WebAssembly-clean so the Studio shares its types |
| `aivyx-mcp` | The MCP client (stdio, SSE, Streamable HTTP) |
| `aivyx-tool` | The tool-process bridge and sandbox wrapper |
| `aivyx-cost` | Token pricing, the spend ledger, budgets and rate limits |
| `aivyx-federation` | Agent identity and signed envelopes across machines |
| `aivyx-team`, `aivyx-team-types` | Multi-agent team missions |
| `aivyx-cli` | The `aivyx-pa` binary; subcommands under `src/bin/aivyx_modules/` |
| `aivyx-tui` | The terminal UI (the workspace's only ratatui dependency) |
| `aivyx-web`, `aivyx-desktop` | The Studio and the desktop app |
| `aivyx-telegram`, `aivyx-discord`, `aivyx-slack`, `aivyx-voice` | Channel adapters |
| `aivyx-gmail`, `aivyx-calendar`, `aivyx-drive`, `aivyx-contacts`, `aivyx-notion`, `aivyx-obsidian`, `aivyx-n8n`, `aivyx-toolkit`, `aivyx-dataread`, `aivyx-apps`, `aivyx-vision` | Integrations and tool processes |
| `aivyx-vertical-sdk`, `crates/verticals/*` | Vertical packs |

Several building blocks are shared with aivyx-coder and come from their
own repositories as pinned git dependencies: `aivyx-confine` (Landlock +
seccomp), `aivyx-checkpoint`, `aivyx-kvcache`, `aivyx-route` (model
routing), `aivyx-skills` (the default skill library), `aivyx-pack` (the
signed pack-bundle format),
`aivyx-injection-guard` (Picket's scanner), `aivyx-vision` (image and SVG
generation) and `aivyx-yubi` (hardware-backed signing keys). `aivyx-broker`
is a separate daemon reached over HTTP (`provider = "broker"`).

## Storage and audit

The store is one redb file. A key is derived from the passphrase with
Argon2id (using the plaintext salt beside the store), expanded with
HKDF-SHA256 into per-domain keys, and every value is sealed with
ChaCha20-Poly1305. The audit log lives in the store as a chain: each entry
carries an HMAC over its content and the previous entry's HMAC, so editing,
reordering or removing an entry breaks verification
(`aivyx-pa --verify-only`).

## Where to read more

- [`docs/THREAT_MODEL.md`](https://github.com/Aivyx-Agent/aivyx-pa/blob/main/docs/THREAT_MODEL.md) — what is and isn't defended.
- [`docs/NONAGON.md`](https://github.com/Aivyx-Agent/aivyx-pa/blob/main/docs/NONAGON.md) — team missions.
- [`docs/TOOLS.md`](https://github.com/Aivyx-Agent/aivyx-pa/blob/main/docs/TOOLS.md) — every tool's scope and tier.
- The design docs under `docs/` are named after the chapter that built each
  feature; the [glossary](../reference/06-glossary.md) maps the names.
