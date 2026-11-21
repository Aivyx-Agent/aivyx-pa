# The board (preview)

The **board** is a to-do list your assistant works through. You write down
the work and say who should do it; your assistant does it and hands it back
for you to check.

The board is a preview: it may change in a later version. It needs the
assistant running in the background (the daemon), which is how the Studio
runs anyway.

## Plan the work

Open **Board** in the Studio sidebar (under Missions). The work is in three
levels:

- a **goal**, such as "Grow the shop's mailing list";
- a **project** under it, such as "Spring newsletter";
- **tasks** in the project, such as "Draft the April newsletter".

Use the forms at the bottom of the Board to add each one. Give a task a
clear description: it's the brief your assistant works from.

## Choose who does it

Each task has an **owner**:

- a **role**, such as `default`: one go by your assistant itself, with
  that role's permissions (the role's own tool list, instructions and
  model aren't used). It's quick, limited by the turn timeout
  (`[agent] turn_timeout_secs`, 120 s by default), so keep these tasks
  small;
- the **team**: your team of specialists (see [Teams](07-teams.md)), for
  bigger work that needs several steps.

Pick the owner in the task's panel and press **Set owner**.

## Start, check, approve

1. Press **Start**. The task moves to **Running**.
2. When the work is done, the task moves to **Review**, and you get a
   message: "Task '…' is ready for review". The **Board** entry in the
   sidebar shows how many tasks are waiting.
3. Open the task and read the result in its thread.
4. Press **Approve** if it's right. If not, press **Send back** and say
   what to change: your note goes into the next attempt. Then press
   **Start** again.

If a run fails, the task goes back to **To do** with the reason in its
thread. Press **Cancel** to drop a task altogether.

While it works, your assistant can leave notes on the task and add smaller
**subtasks** under it. It can't start, approve or reassign anything: those
are yours.

## When your assistant needs you

Runs on the board work unattended, like routines. If a step needs your
approval, it waits in **Needs you** (or `aivyx-pa review`) and the run
carries on without it. Your autonomy settings apply as usual (see
[Autonomy and routines](15-autonomy-and-routines.md)).

If you restart the assistant while a task is running, that task goes back to
**To do**, marked "interrupted". Start it again when you're ready.

## Let agents start tasks

Instead of pressing Start each time, you can give the work to a standing
**agent**: a named worker that starts its own tasks. Switch the Board view
from **Tasks** to **Agents** and add one: give it a name, say whether it
works as a role (such as `default`) or as the team, and choose when it
wakes:

- **Wake when a task is assigned**: about 30 seconds after you assign it a
  task (each new assignment pushes that back, so a few in a row give one
  wake);
- a **heartbeat**: a schedule, such as `0 0 9 * * *` for 9 every morning.
  The pattern starts with seconds, as for routines (see
  [Autonomy and routines](15-autonomy-and-routines.md)).

Then pick the agent as a task's owner. When it wakes, it starts its To-do
tasks oldest first, one at a time, up to five per wake. You still review
each result, as above. **Wake now** wakes it at once, and **Pause** stops it
starting anything more until you press **Resume**. If you restart the
assistant, an agent that wakes on assignment picks its waiting tasks up
again about 30 seconds later.

An agent can report to another, which draws your agents as an org chart.
To remove an agent, reassign its open tasks first, then run
`aivyx-pa board agent remove <name>`.

## Give coding tasks to aivyx-coder

If you use [aivyx-coder](https://github.com/Aivyx-Agent/aivyx-coder), the
coding agent, the board can hand it code work. First tell the board which
project folder it may work in (a **workspace**), from the terminal:

```sh
aivyx-pa board workspace add shop ~/src/shop
```

Then choose **coder shop** as a task's owner (or as what an agent works as)
and press **Start**. aivyx-coder works on the task in that folder on its own,
runs the project's tests, and the task comes back to **Review** with what it
did. Its answer starts with how many files changed and how many checkpoints
it made, then its summary.
If you don't like the change, run `aivyx-coder --resume` in the folder and
type `/undo`. Cancelling a run doesn't undo what it already changed; use
`/undo` for that too.

A few things to know:

- Everything stays on your machine. aivyx-coder uses its own settings, so
  point them at your local model server.
- aivyx-coder needs a test command to check its work: one it can find on
  its own (such as `cargo test`) or one set in its settings.
- One task at a time per folder. A run stops after 4 hours at most, and
  **Cancel** stops it at once.
- If your assistant and aivyx-coder share one model server, use
  [aivyx-broker](https://github.com/Aivyx-Agent/aivyx-broker) so they take
  turns on it.

## From the terminal

Everything on the board also works from the command line:

```sh
aivyx-pa board goal add "Grow the mailing list"
aivyx-pa board project add g-1a2b3c4d "Spring newsletter"
aivyx-pa board task add --project p-5e6f7a8b "Draft the April newsletter" --owner team
aivyx-pa board start t-9c0d1e2f
aivyx-pa board show t-9c0d1e2f
aivyx-pa board approve t-9c0d1e2f
aivyx-pa board agent add scout --works-as role:default --wake-on-assign
aivyx-pa board assign t-9c0d1e2f agent:scout
aivyx-pa board workspace add shop ~/src/shop
aivyx-pa board assign t-9c0d1e2f coder:shop
```

`aivyx-pa board` on its own lists every command. The full operator guide is
`docs/BOARD.md`.
