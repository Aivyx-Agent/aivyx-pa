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

- a **role**, such as `default`: one go by your assistant, with that role's
  permissions. It's quick, at most two minutes, so keep these tasks small;
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

## From the terminal

Everything on the board also works from the command line:

```sh
aivyx-pa board goal add "Grow the mailing list"
aivyx-pa board project add g-1a2b3c4d "Spring newsletter"
aivyx-pa board task add --project p-5e6f7a8b "Draft the April newsletter" --owner team
aivyx-pa board start t-9c0d1e2f
aivyx-pa board show t-9c0d1e2f
aivyx-pa board approve t-9c0d1e2f
```

`aivyx-pa board` on its own lists every command. The full operator guide is
`docs/BOARD.md`.
