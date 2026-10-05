# Named instances — design

Date: 2026-10-05. Status: approved in conversation.

## Goal

Let one operator (one OS user) run several fully separate Aivyx PA agents
side by side on one machine — each with its own role set, Profile,
Persona, memory, audit log, passphrase and Studio — e.g. a `research`
agent and a `household` agent alongside the default one.

Today this collides: every instance of the same OS user resolves the same
daemon socket, store, config, working dirs, keyring entry and service
unit.

## Contract

`PRODUCT.md` **P1** ("One human operator per Aivyx PA instance. One
primary agent…") already scopes its rule *per instance*. Add an amendment,
`docs/amendments/2026-10-05-named-instances.md`, that clarifies:

- One operator may run several **named instances**. Each is a complete,
  separate Aivyx PA instance with exactly one primary agent — P1 holds
  per instance.
- Instances share **nothing** at the Aivyx PA layer: no store, memory,
  audit chain, Profile, Persona, roles, keyring entry or socket. Talking
  across instances is out of scope (federation remains the future path).
- **P6** is unchanged: the operator is the OS user, for every instance.

Reference the amendment from P1's text in `PRODUCT.md` in the way prior
amendments are referenced.

## Instance names

- 1–32 chars, `[a-z0-9-]`, not starting or ending with `-`.
- `default` is reserved and means today's install. No flag, no env var
  → `default`.
- Selected by `--instance <name>` (global CLI flag, every subcommand) or
  `AIVYX_PA_INSTANCE`; the flag wins over the env var. An invalid name is
  a clear error before anything else runs.

## Paths and names — one resolver

A new `InstancePaths` type (in `aivyx-config`, wasm-free) is resolved
once at startup from the instance name plus `HOME` / `XDG_*` and is the
**only** place that builds an aivyx-pa path or per-instance name. Every
existing call site that today joins `"aivyx-pa"` / `".aivyx-pa"` onto a
base directory, or uses the fixed service/keyring names, takes these from
`InstancePaths` instead.

| Item | `default` (unchanged from today) | Named instance `<n>` |
|---|---|---|
| Config file (when not overridden by `AIVYX_PA_CONFIG_PATH` / `./aivyx-pa.toml`) | today's default path | `$XDG_CONFIG_HOME/aivyx-pa/instances/<n>/aivyx-pa.toml` |
| Data dir (store, kvcache, templates, …) | `$XDG_DATA_HOME/aivyx-pa/` | `$XDG_DATA_HOME/aivyx-pa/instances/<n>/` |
| Socket / pid | `$XDG_RUNTIME_DIR/aivyx-pa/daemon.{sock,pid}` | `$XDG_RUNTIME_DIR/aivyx-pa/instances/<n>/daemon.{sock,pid}` |
| Home working dir (sandbox, workspace, tool processes) | `~/.aivyx-pa/` | `~/.aivyx-pa/instances/<n>/` |
| Access sandbox folder | `~/aivyx-pa-sandbox` | `~/aivyx-pa-sandbox-<n>` |
| Keyring entry | today's service + account | same service, account suffixed `:<n>` |
| systemd user unit | `aivyx-pa-daemon.service` | `aivyx-pa-daemon-<n>.service` |
| launchd plist | `com.aivyx-pa.daemon.plist` | `com.aivyx-pa.daemon.<n>.plist` |
| Studio port | config `web_ui_port`, default 7843 | written into the instance's config at `instances create` |

Fallbacks when an XDG variable is unset follow today's rules exactly
(e.g. data falls back to `~/.local/share`). For `default`, every resolved
path must be byte-identical to what today's code produces — existing
installs see no change.

`AIVYX_PA_CONFIG_PATH` and `./aivyx-pa.toml` keep working and win over the
instance's default config path, exactly as today. When `--instance` is
given explicitly and the resolved config came from `./aivyx-pa.toml`,
print a one-line notice naming the file in use (so a stray local file
can't silently hijack an instance).

## CLI

- `--instance <name>` / `AIVYX_PA_INSTANCE` on every command.
- `aivyx-pa instances list` — every instance found (default + each dir
  under `instances/`), with: running or not (socket answers), Studio
  port, config path.
- `aivyx-pa instances create <name>` — validates the name, refuses if it
  exists, picks the Studio port (lowest free port ≥ 7844 that no other
  instance's config already uses and nothing is listening on), then runs
  the normal `init` flow for that instance, writing `web_ui_port` into its
  config.
- `aivyx-pa instances remove <name>` — refuses `default` and a running
  instance; shows what will be deleted (config dir, data dir, home
  working dir, keyring entry, installed service); requires typing the
  name to confirm; then deletes. No `--yes` bypass in v1.
- `daemon install` / `uninstall` / `status`, `doctor`, `init`, `tui`,
  chat — all act on the selected instance.

## Studio and status

- The daemon knows its instance name and returns it in its status/hello
  reply (additive IPC field; old clients ignore it).
- Studio shows the instance name in the header when it isn't `default`
  (and in the page `<title>`), so two tabs are never confused.

## Out of scope (v1)

- The desktop app (`aivyx-desktop`) and the chat channels
  (Telegram/Discord/Slack/voice) stay tied to the default instance. The
  docs say so; their `--instance` support is a later step.
- Any communication or sharing between instances.

## Docs

- `docs/INSTALL.md`: a "Running several agents" section (create, list,
  remove, ports, services, what's isolated, what isn't yet).
- `examples/aivyx-pa.toml`: note that `web_ui_port` must differ per
  instance.
- `CLAUDE.md`: `InstancePaths` is the only place that builds aivyx-pa
  paths.
- `CHANGELOG.md` `[Unreleased]`.

## Testing

- `InstancePaths` unit tests: `default` equals today's paths for every
  item (with and without each XDG var set); named instances nest as in
  the table; invalid names rejected; flag beats env var.
- Integration: two daemons (`default` and `research`) started together
  in temp `HOME`/`XDG_*` dirs — distinct sockets, stores, ports; a turn
  in one leaves the other's store and audit log untouched; `status`
  reports each instance's name.
- `instances create/list/remove` tests, including refusing to remove a
  running instance and port selection skipping used ports.
- A guard test that fails if any non-test source file outside
  `InstancePaths` joins `"aivyx-pa"` / `".aivyx-pa"` path segments or uses
  the fixed service/keyring names — so the namespace can't be bypassed.
- Full `cargo test --workspace`, clippy on stable and 1.99.
