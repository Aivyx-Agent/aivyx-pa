# Troubleshooting

Most problems fall into a handful of buckets. Start here before digging deeper.

## Run the doctor first

Whatever's wrong, this is the fastest first move:

```sh
aivyx-pa doctor
```

It checks your model connection, your config, and a live test reply, and tells
you specifically what's broken and how to fix it.

## Common issues

**The first reply is empty or cut off.**
Usually a local-model setup issue. Aivyx PA normally sizes the model's context
window for you; if you set it manually and made it too small, the assistant can
get starved. Run `aivyx-pa doctor` — it flags this. Make sure you pulled a
**tool-capable** model (for example `qwen3:8b`); very small models that can't
call tools won't work well as an assistant.

**The Studio won't load / says disconnected.**
The Studio is just a window onto the background daemon. If it can't connect, the
daemon probably isn't running. Start it with `aivyx-pa daemon run` (or launch
`aivyx-pa`, which starts it when your passphrase is stored) and reload the page.
The Studio is served at **http://127.0.0.1:7843** by default.

**The Studio asks for a password / says "unauthorized".**
It's protected by a sign-in token. Run `aivyx-pa studio` and open the link it
prints; after that your browser stays signed in. The [desktop app](11-desktop-app.md)
signs in for you.

**`aivyx-pa` says it's "running without the daemon".**
It couldn't start the daemon in the background — usually because your
passphrase is typed at a prompt rather than stored. Store it with
`aivyx-pa keyring set` (or set `AIVYX_PA_PASSPHRASE`) and `aivyx-pa` will start
the daemon next time, or start it yourself with `aivyx-pa daemon run`. It also
runs without the daemon on purpose when you pass `--provider`, `--mcp-server`
or `--mcp-sse`, since those only apply to that one session.

**"Connection refused" to the model.**
Your model backend isn't reachable. For a local model, make sure Ollama (or your
chosen runtime) is running. For a provider, check that your API key is set and
valid — the setup wizard verifies the key before saving, so a re-run of
`aivyx-pa init` will catch a bad one.

**The assistant won't do something / says it's not allowed.**
That's the access and permission system working as intended. Check your **access
level** on the Settings screen — if the assistant needs to reach a folder outside
its current scope, raise the level. Some actions are also confirm-first by
design and will pause for your approval rather than refuse.

**It stopped a long job partway through.**
Two likely reasons, both intentional: it hit an **approval gate** (a step that
needs your sign-off — approve it to continue), or it reached a **budget** limit
(raise the budget on Settings, or let it resume in the next billing window).

**The [desktop app](11-desktop-app.md) won't open (on Linux).**
It needs a few system packages (a webview and the tray libraries). If it fails
to start, install them for your distribution — the
[installation guide](https://github.com/Aivyx-Agent/aivyx-pa/blob/main/docs/INSTALL.md#desktop-app)
lists the exact package names.

## Where to look next

- **The audit log** records every action the assistant took — the ground truth
  when you want to know exactly what happened.
- **Run `aivyx-pa doctor`** any time; it's safe to run repeatedly.
- For installation and platform-specific help, see the
  [install guide](https://github.com/Aivyx-Agent/aivyx-pa/blob/main/docs/INSTALL.md).
