# Getting started

This page gets you from a fresh install to a running assistant.

## 1. Install

Pick the path that matches your machine — the full instructions live in the
[install guide](https://github.com/Aivyx-Agent/aivyx-pa/blob/main/docs/INSTALL.md):

- **macOS / Linux** — run the one-line installer, or build from source.
- **Windows** — run Aivyx PA under WSL2, or use the Docker appliance. (There is no
  native Windows build yet; both options run the same Linux binary.)
- **Always-on server** — the Docker appliance runs the daemon and this Studio in
  a container.

## 2. Choose how your assistant thinks

Aivyx PA needs an AI model to reason with. You have two kinds of choice:

- **Local model (free, private, no API key).** Nothing leaves your machine. Any
  of these works:
  - [Ollama](https://ollama.com) — the easiest start; pull a tool-capable model
    such as `qwen3:8b`.
  - [Lemonade Server](https://lemonade-server.ai), a llama.cpp `llama-server`,
    or [Jan](https://jan.ai).
  - `aivyx-broker`, if you want Aivyx PA and Aivyx Coder to share one
    llama-server.
- **A provider (Anthropic or OpenAI).** Paste an API key during setup. These
  models are more capable; you pay the provider per use.

You can switch later — this is just your starting point.

## 3. First launch

Run the setup wizard once:

```sh
aivyx-pa init
```

It looks for a local model server that's already running (Ollama, Lemonade,
llama.cpp, Jan or the broker), offers the ones it finds first, and lets you pick
one of their models. If it finds none, it asks you to choose — it never picks a
cloud provider for you. Then it walks you through **creating your agent** —
giving your assistant a name, a personality, and a level of access to your
machine. That flow is covered in detail on the next page.

It also asks you to choose a **passphrase**: your assistant's memory and audit
log are encrypted with it. If your computer has a keyring (most desktops do), the
passphrase is kept there. If it doesn't, the wizard offers to save it in a file
only you can read (`~/.config/aivyx-pa/daemon.env`). Either way the assistant
can start its background service and the Studio without asking you each time.

When the wizard finishes it writes a small config file,
`~/.config/aivyx-pa/aivyx-pa.toml`, and you're ready. Launch the assistant from
any folder with:

```sh
aivyx-pa
```

This drops you into a chat session. With your passphrase stored — in your OS
keyring, the saved file, or the `AIVYX_PA_PASSPHRASE` environment variable —
`aivyx-pa` also starts the **daemon** in the background: the
long-running part that serves the Studio, runs scheduled routines and keeps
going after you close the terminal. If it can't start the daemon, it says so and
chats without it; `aivyx-pa daemon run` starts it by hand.

Run `aivyx-pa --help` any time to see every command.

## 4. Open the Studio

While the daemon runs, the Studio is served at **http://127.0.0.1:7843**. It's
protected by a sign-in token, so another account on the same machine can't use
it. To sign in, run:

```sh
aivyx-pa studio
```

and open the link it prints (it looks like `http://127.0.0.1:7843/?token=…`).
You only need it once per browser — after that the Studio remembers you.

The first time, you'll land on the **Create your agent** screen if you haven't
set up an identity yet; otherwise you arrive at the **Command Center** dashboard.
Don't want the Studio at all? Set `web_ui = false` under `[daemon]` in
`aivyx-pa.toml`.

> **Prefer a real app?** The [desktop app](11-desktop-app.md) puts the Studio in
> its own window and your assistant in the system tray — with approval
> notifications and a summon hotkey — instead of a browser tab.

## Health check

If anything seems off — an empty first reply, a model that won't connect — run:

```sh
aivyx-pa doctor
```

It checks your model, your config, and a live test reply, and tells you exactly
what to fix. See [Troubleshooting](10-troubleshooting.md) for the common cases.
