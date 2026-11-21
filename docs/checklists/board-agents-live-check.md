# Standing agents live check (A21, Task 6)

- **Date:** 2026-11-19.
- **Build:** debug `aivyx-pa` from branch `board-agents` (1.3.0 + standing
  agents, Task 5 head `ed104263` plus the fixes below), built with
  `cargo build -p aivyx-cli --bin aivyx-pa`.
- **Model:** llama-server at `http://127.0.0.1:8090`, Qwen3.5-9B Q4_K_M,
  `provider = "llamacpp"`. Nothing used Lemonade.
- **Isolation:** as in the board live check (`board-live-check.md`): a
  throwaway HOME in the session scratchpad (`agents-live2/`), every command
  through a wrapper (`scratchpad/al2.sh`, a copy of `bl.sh`) that runs
  `env -i HOME=<tmp> … XDG_RUNTIME_DIR=$SCRATCH/agrun2
  AIVYX_PA_PASSPHRASE=<throwaway> target/debug/aivyx-pa "$@"`
  (`agrun2` is a short symlink to `<tmp>/run`). Own webhook port 17852 and
  Studio port 17853. The operator's daemon (pid 1894) and
  `~/.config/aivyx-pa` were never touched.
- Steps 1–8 ran in one script (`scratchpad/al-run.sh`), polling
  `board list` every 5 s.

## Steps

| # | Step | Result |
|---|---|---|
| 1 | Create `bookkeeper` (role:default, heartbeat every minute) and `scout` (role:default, wake on assign) | ✅ |
| 2 | Two tasks to bookkeeper: they start and finish one after the other within a minute | ✅ (after fix 1 for the thread's author) |
| 3 | A task assigned to scout starts by itself about 30 s later | ✅ |
| 4 | Pause bookkeeper, assign: nothing starts; resume: it starts at the next beat | ✅ (see the note) |
| 5 | Stop the daemon while scout owns a To-do task, restart: it starts about 30 s after startup | ✅ |
| 6 | `agent wake` starts a task at once | ✅ |
| 7 | Removing an agent that owns open tasks is refused | ✅ |
| 8 | `--verify-only` passes; `audit export --event-type board_changed` shows `woke` and the `agent_*` ops, no text | ✅ |
| 9 | Studio: the Agents tab, the tree, Pause/Resume, Wake now | ⏳ deferred: released without it (preview); to do on the released Studio |

## Notes

### 1. Create

```
$ aivyx-pa board agent add bookkeeper --works-as role:default --heartbeat "0 * * * * *" --title "Keeps the books"
Added agent bookkeeper
$ aivyx-pa board agent add scout --works-as role:default --wake-on-assign --reports-to bookkeeper
Added agent scout
$ aivyx-pa board agent list
bookkeeper  Keeps the books  works as role:default  heartbeat 0 * * * * *  last wake never  open 0
  scout  works as role:default  heartbeat —  last wake never  open 0
```

Goal `g-1b478df3`, project `p-70732e46`. Scout reports to bookkeeper, so
the list shows the tree.

### 2. Heartbeat (bookkeeper)

Two tasks added at 15:17:47 with `--owner agent:bookkeeper`
(`t-948b9eb7` "Name two colours of the sky", `t-b1fdec08` "Name two
fruits"). At 15:18:12 the first was Running and the second still To do; by
15:18:17 both were in Review: one at a time, oldest first. (The 15:18:00
firing is picked up at the next 15 s tick.) The threads:

```
2026-11-19 15:18  agent  todo → running
2026-11-19 15:18  agent  Apple and banana.
2026-11-19 15:18  agent  run b-a326bc93 succeeded, 26535 tokens: Apple and banana.
2026-11-19 15:18  agent  running → review
```

The first task's run answered only through `board.comment` ("Blue and
grey"), so its run entry reads "The tools below ran this turn, but I didn't
write a reply…": the 9B's choice, and still a success.

**Finding (fix 1):** in the first pass (an earlier instance,
`agents-live/`), the thread of a task started by a wake read `operator
todo → running`, although the audit had `started`/`agent`. The thread now
says `agent`.

### 3. Wake on assign (scout)

`t-cba15ad9` "Name two birds" added unowned, assigned
`agent:scout` at 15:19:07. To do through 15:19:37, Review at 15:19:42
(30 s plus up to one 15 s tick): "run b-3b75d266 succeeded, 13250 tokens:
Two birds: eagle and sparrow."

### 4. Pause and resume

`board agent pause bookkeeper` at 15:20:07 (`[paused]` in the list), then
`t-9f6276fb` "Name two rivers" assigned to it. It stayed To do through the
15:21:00 firing (polled until 15:21:27). `board agent resume bookkeeper`
at 15:21:27; Review by 15:21:42.

Note: it started at the first tick after the resume, not at 15:22:00. The
15:21:00 firing fell during the pause and, with no catch-up, the most
recent firing since the last wake counts once. This is the A21 rule;
`docs/BOARD.md` says so ("a heartbeat agent wakes at its next tick if a
firing fell during the pause").

### 5. Restart

`t-d9d17c2f` "Name two mountains" added with `--owner agent:scout`, then
`daemon stop` at once (before its 30 s wake). The daemon was up again at
15:22:53; the task was To do until about 15:23:20 and in Review at
15:23:24: the restart re-wake, 30 s after startup.

### 6. Wake now

At 15:24:20 (mid-minute, so the cron beat can't be the cause)
`t-8cec70c3` "Name two oceans" was added for bookkeeper and
`board agent wake bookkeeper` run: "Wake requested for agent bookkeeper (it
runs in the background)". One second later it was Running; then Review
("Pacific Ocean and Atlantic Ocean.").

### 7. Remove

```
$ aivyx-pa board agent remove scout
aivyx-pa: reassign the agent's open tasks first        (exit 1)
$ aivyx-pa board agent add temp --works-as role:default
$ aivyx-pa board agent edit temp --title "Edited"
$ aivyx-pa board agent remove temp
Removed agent temp                                     (exit 0)
```

Also checked after the run (fix 2): `--heartbeat "0 9 * * *"` is refused
("the heartbeat isn't a valid cron pattern (6 or 7 fields, seconds
first)") and `"0 0 9 * * *"` is accepted.

### 8. Audit

With the daemon stopped, `aivyx-pa --verify-only` → `audit: verified 68
events (head_seq=67)`. `audit export --event-type board_changed` → 42
entries, every event exactly `{kind, op, object, actor}`:
`agent_created` ×3, `agent_edited` ×1, `agent_paused` ×1,
`agent_resumed` ×1, `agent_removed` ×1, `woke` ×9 (all `operator`),
`created` ×8, `assigned` ×1, `started` ×6 (all `agent`), `finished` ×6,
`commented` ×5 (agent). No task title, agent name, title or answer text
appears (grep: 0 hits).

### 9. Studio (deferred)

The operator chose to release 1.4.0 without this step (2026-11-19), as with the board's own Studio step: the board is a preview. The view's helpers have native unit tests, and CI builds and lints the wasm crate.

The daemon is left **stopped**. The config has `[studio] port = 17853`.
The binary (`target/debug/aivyx-pa`, built 15:25) embeds the bundle from
`just build-web` in this task. To run the check:

```sh
# Pick a short scratch dir (<runtime dir> paths must stay short).
SCRATCH=/tmp/aivyx-live; mkdir -p $SCRATCH
S=$SCRATCH   # your session scratchpad
ln -sfn $S/agents-live2/run $SCRATCH/agrun2   # the wrapper's runtime dir
cd $S/agents-live2
nohup $S/al2.sh daemon run > daemon-studio.log 2>&1 &
$S/al2.sh studio            # prints the sign-in link for http://127.0.0.1:17853/
# … click through …
$S/al2.sh daemon stop
rm $SCRATCH/agrun2
```

State waiting for it: agents `bookkeeper` (heartbeat every minute) with
`scout` under it; six tasks in Review (bookkeeper 4, scout 2). To check:
the Tasks | Agents switch, the tree (scout under bookkeeper), Pause/Resume
(the `[paused]` badge), Wake now (assign a new task to bookkeeper first),
Edit and the add form (the heartbeat hint now reads "seconds first, e.g.
0 0 9 * * *"), the owner picker listing both agents, and no panics in the
browser console. Bookkeeper's heartbeat fires every minute while the
daemon runs: pause it first if that gets in the way.

## Fixes

1. **A wake's start was written in the thread as the operator's.** The
   `todo → running` entry of a task started by an agent's wake read
   `operator`. `model::start_by` writes it as the run (`Author::Agent`)
   when the start comes from a wake; a manual Start still reads
   `operator`. Tests: `board::model::tests::a_start_from_a_wake_is_written_by_the_agent`,
   and `board::service::tests::a_wake_runs_todo_tasks_oldest_first_one_at_a_time`
   now checks each task's thread (it failed before the fix).
2. **The Studio's heartbeat hint suggested a pattern the daemon refuses.**
   The agent form's placeholder said "e.g. 0 9 * * *", a five-field Unix
   pattern; the board's cron patterns are six fields, seconds first, as for
   routines. It now reads "seconds first, e.g. 0 0 9 * * *". Test:
   `board_view::tests::the_heartbeat_hint_is_a_six_field_pattern`. The docs
   give the format too.

## Clean-up

No throwaway daemon left running (`pgrep`: only the operator's pid 1894);
ports 17852/17853 free. The `agrun`/`agrun2` runtime symlinks are removed.
The first-pass HOME `agents-live/` (an older binary, see fix 1) and
`agents-live2/` stay in the scratchpad, the second for step 9.
