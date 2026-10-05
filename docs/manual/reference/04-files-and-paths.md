# Files and paths

Where Aivyx PA keeps things, for the default instance and for a named one.
Every path is built in one place in the code (`crates/aivyx-instance`), so
this page is the whole list.

`~` is your home directory. `$XDG_CONFIG_HOME`, `$XDG_DATA_HOME` and
`$XDG_RUNTIME_DIR` are honoured when set; the defaults below assume they
aren't (`~/.config`, `~/.local/share`, and no runtime directory). The same
layout is used on Linux, macOS and WSL.

## The default instance

| What | Where | Change it with |
|---|---|---|
| Config file | `~/.config/aivyx-pa/aivyx-pa.toml` — but a `./aivyx-pa.toml` in the current directory wins | `AIVYX_PA_CONFIG_PATH` |
| Team config (optional) | `team.toml` beside the config file | `[team] config_path` |
| Encrypted store — memory, Persona, skills, audit log, secrets | `~/.local/share/aivyx-pa/store.redb`, plus its salt, `store.redb.salt` | `[storage] path`, `AIVYX_PA_STORAGE_PATH` |
| Studio sign-in token | `studio-token`, next to the store | — (`aivyx-pa studio` prints a sign-in link) |
| Saved passphrase, when there's no OS keyring | `~/.config/aivyx-pa/daemon.env` (mode `0600`) | delete it to be asked each time |
| Daemon socket and PID file | `$XDG_RUNTIME_DIR/aivyx-pa/daemon.sock`, `daemon.pid` — else `~/.local/share/aivyx-pa/` | — |
| Log of a daemon started on demand | `daemon.log`, next to the socket | — |
| Log of a daemon running as a service | the system journal: `journalctl --user -u aivyx-pa-daemon -f` | — |
| Your files at the `sandbox` access level | `~/aivyx-pa-sandbox` | `aivyx-pa access`, `[access]`, `[fs] root` |
| The agent's own workspace (journal, notes, plans) | `~/.aivyx-pa/workspace` | `[workspace] path`, `AIVYX_PA_WORKSPACE` |
| Integration settings (Gmail, Obsidian, toolkit…) | `~/.aivyx-pa/tool-processes/<tool>/` | — |
| KV-cache slots (local llama.cpp) | Linux: `~/.local/share/aivyx-pa/kvcache`; macOS: `~/Library/Application Support/aivyx-pa/kvcache` | `[kvcache] store_path`, `AIVYX_PA_KVCACHE_STORE_PATH` |
| Generated images | `~/.local/share/aivyx-pa/vision/` | the vision tool's own config |
| Studio | `http://127.0.0.1:7843` | `[daemon] web_ui_port`, `web_ui_host` |
| Background service | Linux: `aivyx-pa-daemon.service` (user unit); macOS: `com.aivyx-pa.daemon` | `aivyx-pa daemon install` / `uninstall` |
| Passphrase in the OS keyring | service `aivyx-pa`, account `passphrase` | — |
| The binary | `~/.cargo/bin/aivyx-pa` from the installer | the installer's `--install-path` |

## A named instance

A [named instance](../../guide/16-named-instances.md) `<n>` gets its own copy
of everything. The default instance's paths never change.

| What | Where |
|---|---|
| Config file | `~/.config/aivyx-pa/instances/<n>/aivyx-pa.toml` |
| Saved passphrase | `~/.config/aivyx-pa/instances/<n>/daemon.env` |
| Store, Studio token | `~/.local/share/aivyx-pa/instances/<n>/` |
| Daemon socket, PID file, log | `$XDG_RUNTIME_DIR/aivyx-pa/instances/<n>/` — else `~/.local/share/aivyx-pa/instances/<n>/` |
| Your files at `sandbox` | `~/aivyx-pa-sandbox-<n>` |
| Workspace, integration settings | `~/.aivyx-pa/instances/<n>/` |
| KV-cache slots | `…/aivyx-pa/instances/<n>/kvcache` under the same base as above |
| Generated images | `~/.local/share/aivyx-pa/instances/<n>/vision/` |
| Studio | the port `aivyx-pa instances create` picked, from 7844 up |
| Background service | `aivyx-pa-daemon-<n>.service`; macOS `com.aivyx-pa.daemon.<n>` |
| Keyring entry | service `aivyx-pa`, account `passphrase:<n>` |

Instance names are 1–32 letters, digits and `-`, not starting or ending
with `-`. `aivyx-pa instances list` shows every instance with its
config path and Studio port.

## What to back up

The store holds everything the agent has learned and done — memory,
Persona, skills, missions, the audit log and your saved keys — encrypted
with your passphrase. Back up:

1. the store (`store.redb`) **and** its salt file (`store.redb.salt`) —
   the salt is needed to turn your passphrase into the key, so a store
   without it can't be opened,
2. the config file (and `team.toml`, if you use one),
3. your passphrase, somewhere safe and separate — without it the store
   can't be opened.

To carry only your Profile and Persona to another install, use
`aivyx-pa identity export` / `import` instead. See
[Backups, upgrades and moving](../../guide/18-backups-upgrades-and-moving.md).

## What the agent can't read

Ward, the sensitive-path guard, blocks the agent's own file and shell tools
from reading the store, `daemon.env`, the Studio token and the KV-cache
directory, along with your SSH keys, cloud credentials and `.env` files —
whatever the access level. See
[Security and privacy](../../guide/17-security-and-privacy.md).
