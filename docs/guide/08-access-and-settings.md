# Access and settings

The **Settings** screen is where you control what matters most for safety
and cost: how far your assistant can reach, how much it does on its own,
and how much it can spend. Each of these also has a terminal command.

## Access levels

Access controls which parts of your machine the assistant's tools can touch.
Set it in **Settings → Access level** or with `aivyx-pa access set`:

- **Sandbox** *(the default)* — one dedicated folder, `~/aivyx-pa-sandbox`.
  The assistant reads and writes files only there. Safest.
- **Workspace** — a folder you choose, such as a project. Files and shell
  commands work inside it.
- **Home** — your whole home directory: documents, projects, downloads.
- **Full** — the whole filesystem, system files included.
- **Custom** — the folder or folders you set in the config file.

Changing the level takes effect after a restart, and anything beyond the
sandbox asks you to confirm first — you can't widen access by accident, and
the assistant can never widen its own.

Some files stay off-limits at every level: your SSH keys, cloud
credentials, `.env` files, and Aivyx PA's own encrypted store. See
[Security and privacy](17-security-and-privacy.md).

> **The confirmation safety net.** Whatever the access level, deleting or
> overwriting a file, sending a message, or dispatching an order **stops and
> asks you first** — an *Approval needed* card in the Studio, `Approve? [y/N]`
> in the terminal, and in chat apps you reply "yes". The assistant can never
> approve on your behalf. Access sets the boundary of what's reachable; the
> confirmation protects the consequential actions inside it.

## Autonomy

Autonomy is how much your assistant does on its own. Set it in **Settings →
Autonomy** or with `aivyx-pa autonomy set`:

| Level | What changes |
|---|---|
| **Manual** | Every change it makes — saving a note, writing a file, running a command — asks first. Looking things up doesn't. |
| **Assisted** *(default)* | Only irreversible steps ask. |
| **Supervised** | As assisted, and it may work through its backlog on its own (the [autonomous loop](15-autonomy-and-routines.md)). |
| **Autonomous** | As supervised, and routines it creates start without waiting for your approval. |
| **Unleashed** | For a dedicated, isolated machine only: as autonomous, and deletes and overwrites stop asking — unless you've set `confirm_destructive` under `[access]` yourself. |

Two things hold at every level:

- **Unattended runs never wait for an answer.** A routine, the loop or a team
  mission that reaches an irreversible step refuses it rather than taking
  it.
- **Only you can raise it.** The assistant can never change its own
  autonomy or access.

Batching approvals for later review at *supervised*, and different levels
for different areas (say, autonomous for coding but manual for email), are
planned but not fully built yet; see
[Autonomy and routines](15-autonomy-and-routines.md).

## Budgets

If you use a paid model provider, you can cap spending: a limit per run and
a limit per rolling day, in dollars. When a cap is reached, the assistant
either stops or warns you, depending on **On exceeded**; **Alert at** warns
you early, at a fraction of the cap (0.8 means 80%). Leave a cap blank for
no limit. Caps apply to ordinary chats and to long autonomous jobs alike,
and every cost is recorded on the same audited trail as everything else.

## Also on this screen

- **Proactive surfacing** — whether the assistant may reach out to you first
  (rarely, and only for a concrete reason, such as a reminder that's due).
- **Agent** — the model and provider in use, and the *cycle breaker*, which
  stops a turn that keeps repeating the same few steps.
- **Embeddings** — the model used for memory search, if any.

Settings are read when the daemon starts, so after saving, restart it:
`aivyx-pa daemon stop && aivyx-pa daemon run`, or **Restart daemon** in the
desktop app's tray menu. To
change the model, provider or keys, run `aivyx-pa init` again.

## Everything is audited

Changing a setting, like every other action, is written to your assistant's
tamper-evident log. You can review the history of what changed and when.
