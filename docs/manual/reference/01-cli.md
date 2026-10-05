# Command-line reference

Every `aivyx-pa` command, its forms and flags. Run `aivyx-pa --help` for the
summary list, or `aivyx-pa <command> --help` for one command.

Running `aivyx-pa` with no command opens a chat session. It connects to the
running daemon (starting one in the background if needed) unless you pass
`--no-daemon`.

## Global flags

These work with any command.

| Flag | What it does |
|---|---|
| `--instance <name>` | Use a named instance — a separate agent with its own config, store and Studio. Same as setting `AIVYX_PA_INSTANCE`; the flag wins. Without it you get the `default` instance. See [Named instances](../../guide/16-named-instances.md). |
| `--verify-only` | Verify the audit chain and exit (forensic check; no session). |
| `--channel <kind>` | Run the session on a channel instead of the terminal: `local`, `telegram`, `discord`, `slack` or `voice`. |
| `--role <name>` | Use a named role (from `[[role]]` in your config) for this session. |
| `--print-role <name>` | Print a role's full capability envelope and exit. |
| `--no-daemon` | Chat in-process: don't connect to a running daemon or start one. |
| `--provider <kind>` | Override the configured model provider for this run: `anthropic`, `openai`, `ollama`, `llamacpp`, `jan`, `mistralrs`, `broker` or `lemonade`. |
| `--mcp-server <name:command[:arg1,arg2,…]>` | Attach a stdio MCP server for this session. |
| `--mcp-sse <name:url>` | Attach an SSE MCP server for this session. |
| `--version`, `-V` | Print the version and exit. |
| `--help`, `-h` | Print help (also `aivyx-pa help`, or `<command> --help`). |

A `--` ends option parsing: everything after it is passed through unchanged.

## Front ends

### `aivyx-pa tui`

A full-screen terminal interface for chatting with your agent.

```
aivyx-pa tui [--role <name>]
```

`--role` picks a role for the session. The TUI talks to the daemon like every
other front end. See [Terminal and CLI](../../guide/14-terminal-and-cli.md).

### `aivyx-pa --headless`

Run one task without an interactive session — useful in scripts and cron.

```
aivyx-pa --headless "<task>"
aivyx-pa --headless            # reads tasks from standard input, one session
```

The task runs over the daemon, which must already be running (headless never
starts one); the result is printed and the command exits. Actions that need
your approval are refused rather than waiting for an answer nobody can give.
Exit codes: `0` completed, `3` refused at an approval gate or stopped for
cloud consent, `1` anything else.

## Setup and the daemon

### `aivyx-pa instances`

Several fully separate agents for one OS user.

```
aivyx-pa instances list
aivyx-pa instances create <name>
aivyx-pa instances remove <name>
```

- `list` — every instance, whether its daemon is running, its Studio port and
  its config path.
- `create <name>` — refuses `default` and existing names, picks the lowest
  free Studio port from 7844, runs the setup wizard for the new instance and
  records the port in its config.
- `remove <name>` — refuses the `default` instance, a running instance and one
  with an installed service; lists exactly what it will delete and asks you to
  type the name back.

Names are 1–32 characters of `a-z`, `0-9` and `-`. See
[Named instances](../../guide/16-named-instances.md).

### `aivyx-pa daemon`

The background process every front end talks to.

```
aivyx-pa daemon run [--web-ui] [--web-ui-port <N>]
aivyx-pa daemon status
aivyx-pa daemon stop
aivyx-pa daemon install [--web-ui] [--no-start]
aivyx-pa daemon uninstall
```

- `run` — run the daemon in this terminal. `--web-ui` serves the Studio
  (default port 7843, or `[daemon] web_ui_port`); `--web-ui-port` overrides
  the port.
- `status` — whether it's running, its protocol version, socket and PID (and
  the instance, for a named one).
- `stop` — ask the running daemon to shut down cleanly.
- `install` — register a background service that survives logout and reboot
  (a systemd user unit on Linux, a launchd agent on macOS) and start it;
  `--no-start` installs without starting. Each named instance gets its own
  service (`aivyx-pa-daemon-<name>`).
- `uninstall` — stop and remove that service.

### `aivyx-pa init`

The first-run setup wizard: finds your model, writes `aivyx-pa.toml`, sets up
the encrypted store and saves your passphrase.

```
aivyx-pa init [--template <name>] [--list-templates]
```

`--template` pre-fills the wizard from a named template (bundled, or yours in
`~/.local/share/aivyx-pa/templates/`); `--list-templates` lists them.

### `aivyx-pa doctor`

Check your setup and say what's wrong: config, store, passphrase, model
connection, the Studio, installed integrations and packs.

```
aivyx-pa doctor
```

### `aivyx-pa studio`

Print the Studio sign-in link for your browser.

```
aivyx-pa studio
aivyx-pa studio --token        # print only the sign-in token
```

### `aivyx-pa keyring`

Manage the passphrase saved in your OS keyring (Secret Service, Keychain or
Credential Manager).

```
aivyx-pa keyring set|clear|status
```

On a machine without a keyring the passphrase is kept in an owner-only
`daemon.env` file instead (see [Files and paths](04-files-and-paths.md)).

### `aivyx-pa access`

How far the agent can reach on your files and machine.

```
aivyx-pa access show
aivyx-pa access set <sandbox|workspace|home|full|custom> [--root <dir>] [--yes]
```

| Level | Reaches | Can also |
|---|---|---|
| `sandbox` *(default)* | `~/aivyx-pa-sandbox` | read and write files there |
| `workspace` | a directory you choose (`--root`) | run shell commands and delete files there |
| `home` | your home directory | files and shell commands across your home |
| `full` | the whole filesystem | everything (with a warning) |
| `custom` | the root(s) you set (`--root`) | what you configure |

Anything beyond `sandbox` asks you to confirm unless `--yes`. The agent can
never change its own access. See [Access and settings](../../guide/08-access-and-settings.md).

### `aivyx-pa autonomy`

How much the agent may do without asking.

```
aivyx-pa autonomy show
aivyx-pa autonomy set <manual|assisted|supervised|autonomous|unleashed> [--yes]
```

| Level | In short |
|---|---|
| `manual` | asks before every change it makes (not before reading); the autonomous loop is off |
| `assisted` *(default)* | irreversible steps ask you first; the loop is off |
| `supervised` | the loop may work its backlog on its own; irreversible steps still ask |
| `autonomous` | as `supervised`, and routines the agent creates start without your approval |
| `unleashed` | for an isolated machine: as `autonomous`, and delete/overwrite confirmation is off — but only at the `sandbox` access level unless you also set `[access] confirm_destructive = false` |

Batching approvals for review at `supervised` is designed but not built yet.
Unattended runs (routines, the loop) never wait for an answer: an
irreversible step there is refused. Per-domain exceptions live in
`[[autonomy.override]]`; `autonomy show` displays them, but today only the
`schedules` domain changes behaviour (whether agent-created routines need
approval).

`autonomous` and `unleashed` ask you to confirm unless `--yes`. The agent can
never raise its own level. See [Autonomy and routines](../../guide/15-autonomy-and-routines.md)
and `docs/AUTONOMY.md`.

## Your agent

### `aivyx-pa role`

Apply a role change the agent proposed and you approved.

```
aivyx-pa role import <proposal-id> [--yes] [--force]
```

### `aivyx-pa persona`

The personality your agent grows through use — every change is yours to
approve or undo.

```
aivyx-pa persona show
aivyx-pa persona list [--auto-only | --manual-only]
aivyx-pa persona revert <delta-id>
aivyx-pa persona proposals list
aivyx-pa persona proposals show <proposal-id>
aivyx-pa persona proposals approve <proposal-id>
aivyx-pa persona proposals reject <proposal-id> [--reason <text>]
aivyx-pa persona conflicts
aivyx-pa persona resolve <id> --remove <a|b>
aivyx-pa persona dismiss <id>
```

- `show` / `list` — the current persona and the changes that built it.
- `revert` — undo one change.
- `proposals …` — review, approve or reject changes the agent suggests.
- `conflicts`, `resolve`, `dismiss` — handle two persona entries that
  contradict each other.

See [Skills and persona](../../guide/06-skills-and-persona.md).

### `aivyx-pa profile`

The identity you declare for your agent (who it's for, how it should work).
The agent can never write it.

```
aivyx-pa profile show
aivyx-pa profile edit
aivyx-pa profile apply-hint <proposal-id> [--yes]
```

`edit` opens it in your editor; `apply-hint` applies a suggested refinement
you've approved.

### `aivyx-pa skills`

Teach your agent a procedure by hand.

```
aivyx-pa skills teach <name> <trigger> <procedure>
aivyx-pa skills update <name> [--trigger <t>] [--procedure <p>]
aivyx-pa skills forget <name>
```

### `aivyx-pa team`

Multi-agent missions: a lead agent splits a goal and hands parts to
specialists (the "Nonagon" team).

```
aivyx-pa team roster [--config <path>]
aivyx-pa team init [--pack <default|path>] [--out <path>] [--force]
aivyx-pa team run "<mission>" [--config <path>]
aivyx-pa team start "<goal>" [--config <path>]
aivyx-pa team start --plan <file.json> [--config <path>]
aivyx-pa team list
aivyx-pa team status [<mission-id>]
aivyx-pa team approve|reject <mission-id> <step>
aivyx-pa team abort|pause|resume <mission-id>
```

- `roster` — the team and each member's role.
- `init` — write a team config to customise (from the default team or a pack).
- `run` — run a mission in the foreground; `start` — hand it to the daemon
  (from a goal, or a prepared plan file).
- `list`, `status` — running and finished missions.
- `approve`/`reject` — answer a step waiting for you; `abort`/`pause`/`resume`
  — control a mission.

See [Teams](../../guide/07-teams.md) and `docs/NONAGON.md`.

### `aivyx-pa loop`

The autonomous loop: a backlog of tasks the agent works through on its own,
within your autonomy level.

```
aivyx-pa loop add <title> [<body>] [--body <text>] [--priority <N>]
aivyx-pa loop list
aivyx-pa loop start [--max-iterations <N>]
aivyx-pa loop status
aivyx-pa loop stop
aivyx-pa loop skip <story-id>
aivyx-pa loop log [--limit <N>]
```

See [Autonomy and routines](../../guide/15-autonomy-and-routines.md).

### `aivyx-pa memory`

What your agent remembers.

```
aivyx-pa memory list
aivyx-pa memory show <topic> [--limit <N>]
aivyx-pa memory search <query> [--semantic] [--limit <N>]
aivyx-pa memory evict <topic> [--yes]
aivyx-pa memory wiki [topic]
aivyx-pa memory graph [entity]
aivyx-pa memory conflicts
aivyx-pa memory resolve <topic> --archive <seq>
aivyx-pa memory dismiss <id>
```

- `list`, `show`, `search` — browse and search (`--semantic` uses vector search,
  available with the `smart` memory profile).
- `evict` — forget a whole topic.
- `wiki`, `graph` — the knowledge pages and entity graph the agent builds
  (`smart` profile).
- `conflicts`, `resolve`, `dismiss` — memories that contradict each other.

See [Memory](../../guide/05-memory.md).

### `aivyx-pa learning`

What the agent has learned recently.

```
aivyx-pa learning [--window <secs>]
```

### `aivyx-pa workspace`

The agent's own notebook directory (journal, ideas, plans).

```
aivyx-pa workspace path
aivyx-pa workspace ls [path]
aivyx-pa workspace cat <path>
```

## Tools and integrations

### `aivyx-pa tools`

Recent tool-call activity.

```
aivyx-pa tools [--window <secs>]
```

### `aivyx-pa tool`

Scaffold a new out-of-process tool (any language) to extend the agent.

```
aivyx-pa tool init <path> [--force]
```

See [Tool processes](../developer/04-tool-processes.md).

### `aivyx-pa mcp`

MCP servers attached to your agent.

```
aivyx-pa mcp status
aivyx-pa mcp recipes [name]
```

`status` shows each server's state; `recipes` prints ready-made config for
common MCP servers.

### `aivyx-pa mcp-server`

Run one of the MCP servers bundled inside `aivyx-pa` (stdio). Normally you
reference it from config with `bundled = true` rather than running it by hand.

```
aivyx-pa mcp-server web-search
```

### `aivyx-pa connect`

Guided sign-in for the productivity integrations (Gmail, Calendar, Drive,
Contacts and others).

```
aivyx-pa connect              # list services and whether each is connected
aivyx-pa connect <service>    # walk through connecting one
```

See [Chat apps and accounts](../../guide/13-chat-apps-and-accounts.md).

### `aivyx-pa pack`

Vertical packs: signed bundles that add a domain-specific team and tools.

```
aivyx-pa pack keygen <keyfile>
aivyx-pa pack build <staging-dir> --key <keyfile> --out <file>
aivyx-pa pack inspect <bundle-file> [--allow-untrusted]
aivyx-pa pack install <bundle-file>
```

`inspect` verifies the signature and shows the contents before you install;
`--allow-untrusted` inspects a pack signed by a key you haven't trusted yet.
See [Vertical packs](../developer/05-vertical-packs.md).

### `aivyx-pa notify`

Notification delivery history.

```
aivyx-pa notify history [--target <name>] [--limit <N>]
```

## Inspection

### `aivyx-pa audit`

Export the tamper-evident audit log.

```
aivyx-pa audit export [--from <seq>] [--limit <N>]
```

To verify the chain instead, run `aivyx-pa --verify-only`.

### `aivyx-pa cost`

What your LLM use has cost, priced from the audit log.

```
aivyx-pa cost [--today]
```

### `aivyx-pa routing`

Model routing: which model handled what, and why.

```
aivyx-pa routing status
aivyx-pa routing explain [--limit <N>]
aivyx-pa routing allow-cloud <session-id>
```

`allow-cloud` gives consent for one conversation to escalate to a cloud
endpoint you configured. See [Models and routing](../../guide/12-models-and-routing.md).

### `aivyx-pa identity`

Export or import your agent's federation identity (its signing key).

```
aivyx-pa identity export <path>
aivyx-pa identity import <path> [--force]
```

### `aivyx-pa federation`

Bind the federation identity to a YubiKey so the key never leaves the
hardware (requires a build with YubiKey support and `pcscd`).

```
aivyx-pa federation yubikey-init <instance-id> <key-binding-path> [--card <serial>] [--overwrite-existing-key]
```

`--card` picks the card when more than one is plugged in;
`--overwrite-existing-key` replaces a key already on the card's signature
slot (you'll be asked to type the card's serial to confirm).

### `aivyx-pa tool-relevance`

Debugging aid: dump the keyword scores used to suggest tools.

```
aivyx-pa tool-relevance dump [--keyword-key <key>]
```
