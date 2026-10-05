# Terminal and command line

Everything the Studio does, you can also do from a terminal — and a few
things (setup, the background service, scripting) start there. This page is
the everyday tour; every command and flag is in the
[command-line reference](https://github.com/Aivyx-Agent/aivyx-pa/blob/main/docs/manual/reference/01-cli.md).
`aivyx-pa --help` lists them too, and `aivyx-pa <command> --help` explains
one.

## Three ways to chat from a terminal

**Plain chat** — just run:

```sh
aivyx-pa
```

Type a message and press Enter; the reply streams in as it's written.
Press **Ctrl-C** to stop a reply, and **Ctrl-D** to leave. When the
assistant wants to do something irreversible it shows what and why, and
asks `Approve? [y/N]` — type `y` to let it, anything else to refuse. This
is also the one to use when piping text in from another program.

**The terminal UI** — a full-screen app with a chat pane and status bar:

```sh
aivyx-pa tui
```

| Key | What it does |
|---|---|
| Enter | Send |
| Esc or Ctrl-C | Stop the reply in progress |
| PgUp / PgDn, ↑ / ↓ | Scroll |
| Tab, or 1–5 | Switch view: 1 Chat, 2 Missions, 3 Dashboard, 4 Audit, 5 Tools |
| `y` / `n` | Approve or refuse when an approval appears |
| `n` (Missions view) | Start a new mission |
| `a` / `r` (Missions view) | Approve or reject a mission waiting on you |
| Ctrl-Q | Quit (the assistant keeps running) |

**One task from a script** — no conversation, just the answer:

```sh
aivyx-pa --headless "Summarise today's calendar"
echo "Tidy the notes folder" | aivyx-pa --headless
```

Nobody is there to approve anything, so a headless task refuses
irreversible steps instead of waiting. It needs the background assistant
already running (`aivyx-pa daemon run`, or the installed service). The exit
code tells a script what happened: `0` done, `3` refused at an approval (or
it wanted cloud consent), `1` anything else. Good for cron jobs and shell
scripts.

All three talk to the same background assistant, so a conversation in one
shares memory with the others. If it isn't running yet, `aivyx-pa` and
`aivyx-pa tui` start it for you (when your passphrase is stored).

## Commands inside a chat

These work in plain chat, the terminal UI, the Studio and the chat apps:

| Command | What it does |
|---|---|
| `/models` | Which models routing can choose from |
| `/models why` | Why it chose the model it did |
| `/model <name>` / `/model auto` | Pin this conversation to a model, or let routing choose again |
| `/allow-cloud` | Let this conversation use a cloud model (see [Models and routing](12-models-and-routing.md)) |

## The commands you'll use most

| Command | What it does |
|---|---|
| `aivyx-pa init` | First-time setup, or change the model, provider or keys |
| `aivyx-pa doctor` | Check everything and say what to fix |
| `aivyx-pa studio` | Print the Studio sign-in link |
| `aivyx-pa daemon status` / `stop` / `run` | Check, stop or start the background assistant |
| `aivyx-pa daemon install` | Run it as a background service that starts at login |
| `aivyx-pa access show` / `set <level>` | See or change how far it can reach |
| `aivyx-pa autonomy show` / `set <level>` | See or change how much it does on its own |
| `aivyx-pa memory` | Browse, search and tidy its memory |
| `aivyx-pa learning` | What it has learned recently |
| `aivyx-pa skills` / `persona` / `profile` | Its skills, character and your Profile |
| `aivyx-pa team` | Run and manage team missions |
| `aivyx-pa loop` | Stock and run the autonomous backlog |
| `aivyx-pa cost` | What your model use has cost |
| `aivyx-pa connect` | Connect Gmail, Calendar, Drive or Contacts |
| `aivyx-pa instances` | List, create or remove separate assistants |

## Roles and instances on the command line

- `--role <name>` starts a conversation as one of your
  [roles](https://github.com/Aivyx-Agent/aivyx-pa/blob/main/docs/manual/reference/02-configuration.md);
  `aivyx-pa --print-role` shows what the active role can do.
- `--instance <name>` talks to one of your
  [named instances](16-named-instances.md) instead of the default
  assistant. It works with every command.

## Keeping it running

`aivyx-pa daemon install` sets up a background service — a systemd user
service on Linux, a launch agent on macOS — so your assistant starts when
you log in, runs routines on time and keeps the Studio available. On Linux,
read its log with `journalctl --user -u aivyx-pa-daemon -f`.
`aivyx-pa daemon uninstall` removes it again.
