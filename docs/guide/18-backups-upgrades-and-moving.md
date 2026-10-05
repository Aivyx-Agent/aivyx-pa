# Backups, upgrades and moving

Everything your assistant knows lives on your machine — which also means
looking after it is up to you. There is no cloud copy.

## What to back up

| What | Where (default assistant) | Why |
|---|---|---|
| The store | `~/.local/share/aivyx-pa/store.redb` | Memory, persona, skills, missions, routines, the audit log and saved keys. |
| The store's salt | `~/.local/share/aivyx-pa/store.redb.salt` | Needed with your passphrase to unlock the store. **A store without its salt can't be opened.** |
| Config | `~/.config/aivyx-pa/aivyx-pa.toml` (and `team.toml` beside it, if you have one) | Your settings, roles and routines. |
| Integrations | `~/.aivyx-pa/tool-processes/` | Your Google, Notion and other sign-ins and settings. |
| The assistant's notebook | `~/.aivyx-pa/workspace/` | Its journal, notes and plans. |
| Your files | `~/aivyx-pa-sandbox/` (at the sandbox access level) | Files you asked it to create. |
| Your passphrase | somewhere safe, apart from the backup | Without it, the backup is unreadable. |

For a [named instance](16-named-instances.md), the same things live under
`instances/<name>/` in each of those folders, and its sandbox is
`~/aivyx-pa-sandbox-<name>`. The full list is in the
[files and paths reference](https://github.com/Aivyx-Agent/aivyx-pa/blob/main/docs/manual/reference/04-files-and-paths.md).

**Stop the assistant before copying the store** so you get a consistent
copy:

```sh
aivyx-pa daemon stop
cp ~/.local/share/aivyx-pa/store.redb ~/.local/share/aivyx-pa/store.redb.salt /path/to/backup/
aivyx-pa daemon run      # or let the service / `aivyx-pa` start it again
```

The store is encrypted, so a backup of it is safe to keep on an external
drive or a cloud folder; the integrations folder and notebook are **not**
encrypted, so treat those like any private files.

### Just the personality

To carry only your Profile and the persona your assistant has built up — to
another machine, or as a snapshot before a big change:

```sh
aivyx-pa identity export ~/aivyx-pa-identity.json
aivyx-pa identity import ~/aivyx-pa-identity.json    # on the other install
```

This leaves memory, routines and history behind.

## Upgrading

Install the new version over the old one, the same way you installed it:

- **Shell installer** — run the one-line installer again; it replaces the
  `aivyx-pa` binary.
- **Desktop app** — install the new `.deb` or `.app` from the
  [latest release](https://github.com/Aivyx-Agent/aivyx-pa/releases/latest).
  It includes the matching `aivyx-pa`.
- **From source** — pull and rebuild.

Then restart the assistant so the new version is the one running:
`aivyx-pa daemon stop` and start it again, or — if it runs as a service —
`systemctl --user restart aivyx-pa-daemon` on Linux. Your store and config
carry over; the [changelog](https://github.com/Aivyx-Agent/aivyx-pa/blob/main/CHANGELOG.md)
says if anything needs your attention. `aivyx-pa doctor` confirms all is
well.

## Moving to a new machine

1. On the old machine, stop the assistant (`aivyx-pa daemon stop`) and copy
   everything in the table above.
2. Install Aivyx PA on the new machine — don't run `aivyx-pa init`; you're
   bringing your existing setup.
3. Put the files in the same places. If your home folder's path is
   different on the new machine, update any paths in the config file —
   `[storage] path`, `[access] root`, `[fs] root`.
4. Run `aivyx-pa`. It asks for your passphrase; store it again with
   `aivyx-pa keyring set` so it can start on its own.
5. If you used the background service, run `aivyx-pa daemon install` on
   the new machine.
6. Run `aivyx-pa doctor`.

Local model servers and their models don't come along — install and pull
them on the new machine first, or change the model with `aivyx-pa init`.

## Uninstalling

Removing Aivyx PA deletes your assistant's memory and history for good.
Back up first if there's any chance you'll want it.

```sh
aivyx-pa daemon uninstall     # if you installed the background service
aivyx-pa daemon stop
aivyx-pa keyring clear        # remove the saved passphrase

# Your data — this is the irreversible part:
rm -rf ~/.config/aivyx-pa ~/.local/share/aivyx-pa ~/.aivyx-pa
# Files you had the assistant make (check before deleting):
#   ~/aivyx-pa-sandbox  (and ~/aivyx-pa-sandbox-<name> for named instances)

rm "$(command -v aivyx-pa)"   # the program itself, last
```

Repeat the first three commands with `--instance <name>` for each
[named instance](16-named-instances.md) that has a service or saved
passphrase. Remove the desktop app the way you installed it.
