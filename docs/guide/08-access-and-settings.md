# Access and settings

The **Settings** screen is where you control two things that matter most for
safety and cost: how far your assistant can reach, and how much it can spend.

## Access levels

Access controls which parts of your machine the assistant's tools can touch.
There are three levels:

- **Sandbox** — confined to one dedicated working folder. The assistant can read
  and write only there. Safest.
- **Home** — your home directory is in reach (documents, projects, downloads).
- **Full** — broad filesystem access.

Changing the level takes effect after a restart, and the change itself is
confirmed before it's applied — you can't bump access by accident.

> **The confirmation safety net.** Regardless of access level, anything
> irreversible — deleting or overwriting a file, sending a message, dispatching
> an order — **always stops and asks you first**, and only your reply counts as
> the yes. Access sets the boundary of what's reachable; the confirmation step
> protects the consequential actions inside that boundary. (The one exception
> is the `unleashed` autonomy level below, meant for a dedicated machine.)

## Autonomy

Autonomy is how much your assistant does on its own. Set it in **Settings →
Autonomy** or with `aivyx-pa autonomy`. Today the level changes two things:

- **Whether it may work through its backlog on its own** (the autonomous loop):
  off at `manual` and `assisted` *(the default)*, available from `supervised`
  up.
- **Whether irreversible steps ask you first:** yes at every level except
  `unleashed`, which is only for an isolated machine you're prepared to let it
  change.

Finer differences between the levels (such as `manual` asking before every
step) are still being built. Whatever the level, a scheduled or unattended run
never waits for an answer — an irreversible step there is refused, not taken —
and the assistant can never raise its own autonomy or access; only you can.

## Budgets

If you use a paid model provider, you can cap spending. Set budgets and the
assistant tracks its costs against them; when a limit is reached, it stops rather
than running up a bill. This applies to ordinary chats and to long autonomous
jobs alike.

Budgets are enforced on the same audited trail as everything else, so you can
always see what was spent and on what.

## Everything is audited

Changing a setting, like every other action, is written to your assistant's
tamper-evident log. You can review the history of what changed and when.
