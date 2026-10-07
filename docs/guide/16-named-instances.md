# Several assistants: named instances

You can run more than one assistant on the same computer — say a
**research** assistant and a **household** one next to your usual one. Each
is a complete, separate Aivyx PA: its own name and personality, roles,
memory, skills, routines, audit log, passphrase and Studio. They share
nothing, so what you tell one, the others never see.

Each of these is called an **instance**. The one you already have is the
`default` instance; the others have names you choose.

## Create one

```sh
aivyx-pa instances create research
```

This picks a free Studio port for it, then runs the same setup you did the
first time — model, passphrase, [creating its identity](03-create-your-agent.md).
Names are 1–32 letters, digits and `-`, not starting or ending with `-`.

### From a pack

A **pack** sets up a ready-made assistant for one kind of work — say,
running a small business. Installing it creates a new instance for it:

```sh
aivyx-pa pack inspect business-manager.aivyxpack     # what it contains
aivyx-pa pack install business-manager.aivyxpack     # instance `business-manager`
aivyx-pa --instance shop pack install business-manager.aivyxpack   # or name it
```

You go through the same setup, with the pack's answers pre-filled, and it
tells you which accounts to connect afterwards. `aivyx-pa instances list`
shows which pack each instance came from.

## Use it

Add `--instance <name>` to any command:

```sh
aivyx-pa --instance research                    # chat with it
aivyx-pa --instance research daemon run         # start it
aivyx-pa --instance research studio             # its Studio sign-in link
aivyx-pa --instance research daemon install     # run it as its own background service
```

Or set `AIVYX_PA_INSTANCE=research` in a terminal to make every command in
it use that instance (the `--instance` flag still wins). Without either,
you get `default`, exactly as before.

Each instance's Studio has its own port — the default one is on 7843, new
ones from 7844 up — and shows the instance's name in the browser tab, so you
always know which assistant you're talking to.

## See them all

```sh
aivyx-pa instances list
```

shows every instance: whether it's running, its Studio port and where its
config file is.

## Remove one

```sh
aivyx-pa instances remove research
```

It lists exactly what will go and asks you to type the name back, then
deletes that instance's config, store (memory, persona, audit log), its
workspace, its saved passphrase — and its sandbox folder,
`~/aivyx-pa-sandbox-research`, **including any files the assistant made for
you there**. Copy out anything you want to keep, or
[back it up](18-backups-upgrades-and-moving.md), first.

It refuses the `default` instance, one that's running, and one with a
background service installed — stop it and run
`aivyx-pa --instance research daemon uninstall` first.

## Good to know

- **Keep ports different.** `instances create` writes a free port into the
  new instance's config. If you change `web_ui_port` by hand, keep each one
  unique.
- **A config file in the current folder wins.** If the folder you're in has
  its own `aivyx-pa.toml`, every instance started there reads it — the
  instance warns you when that happens.
- **Same user, separate files.** Instances are kept apart by Aivyx PA, not
  by separate operating-system accounts. Their stores are encrypted with
  their own passphrases, and no assistant can read another's store or saved
  passphrase, even at `home` or `full` access.
- **Default only, for now:** the [desktop app](11-desktop-app.md), the
  Telegram, Discord and Slack bots, and voice all use the `default`
  instance. Use the others from the terminal or their own Studio.

Where each instance keeps its files is listed in the
[files and paths reference](https://github.com/Aivyx-Agent/aivyx-pa/blob/main/docs/manual/reference/04-files-and-paths.md).
