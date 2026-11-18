# Work board live check (A20, Task 7)

- **Date:** 2026-11-18.
- **Build:** debug `aivyx-pa` from branch `factory-board` (1.2.0 + the
  board), rebuilt after each fix below.
- **Model:** llama-server at `http://127.0.0.1:8090`, Qwen3.5-9B Q4_K_M,
  `provider = "llamacpp"`. Nothing used Lemonade.
- **Isolation:** a throwaway HOME (session scratchpad `board-live/`), every
  command through a wrapper (`scratchpad/bl.sh`):
  `env -i HOME=<tmp> PATH=/usr/bin:/bin XDG_CONFIG_HOME=<tmp>/.config
  XDG_DATA_HOME=<tmp>/.local/share XDG_STATE_HOME=<tmp>/.local/state
  XDG_CACHE_HOME=<tmp>/.cache XDG_RUNTIME_DIR=<short symlink>
  AIVYX_PA_PASSPHRASE=<throwaway> TERM=dumb target/debug/aivyx-pa "$@"`.
  `XDG_RUNTIME_DIR` is a short symlink to `<tmp>/run` (the full scratchpad
  path makes the socket path longer than `SUN_LEN`, as in the federation
  check). The operator's own daemon (pid 1894) was never touched.

## Steps

| # | Step | Result |
|---|---|---|
| 1 | Set up the instance at `:8090` | ✅ (config written by hand) |
| 2 | Start the daemon | ✅ |
| 3 | Goal, project, a `role:default` task and a `team` task | ✅ |
| 4 | `board start` both | ✅ |
| 5 | `board show` both: a Run entry, Review or Todo with a reason | ✅ after fixes 1 and 2 (the 9B's first two team runs failed; the third reached Review) |
| 6 | `send-back` with a reason, start again, the reason reaches the brief | ✅ |
| 7 | `board approve` | ✅ |
| 8 | Stop the daemon mid-run, restart: back in Todo, "interrupted" | ✅ |
| 9 | Chain verifies; `audit export --event-type board_changed` has no text | ✅ |
| 10 | Studio click-through (filters, task panel, Start, Approve, Send back; browser console) | ⏳ deferred: released without it (preview); to do on the released Studio |

## Notes

### 1. Instance

`aivyx-pa init` has no non-interactive mode (`init [--template <name>]`
only), so, as in the federation check, `aivyx-pa.toml` was written by hand:

```toml
[agent]
provider = "llamacpp"
model = "/models/Qwen3.5-9B-Q4_K_M.gguf"

[openai]
base_url = "http://127.0.0.1:8090"

[daemon]
webhook_port = 17842

[studio]
enabled = false        # for steps 1–9; port = 17843 for step 10
```

`aivyx-pa doctor` → "✓ the model answers: "OK"", "✓ Looks good".

### 2. Daemon

`daemon run` created the store; `daemon status` → running. The synthesized
`default` role exists with no `[[role]]` in the config.

### 3. Create

```
$ aivyx-pa board goal add "Tidy the notes folder" --description "…"
Added goal g-6ddf4e35
$ aivyx-pa board project add g-6ddf4e35 "Weekly summary"
Added project p-a9913886
$ aivyx-pa board task add --project p-a9913886 "Write a two-line summary of what an assistant can help with" \
    --description "Reply with two short lines. Add one board.comment saying you are done." --owner role:default
Added task t-eea2b030
$ aivyx-pa board task add --project p-a9913886 "Name three tidy habits for a notes folder" \
    --description "List three short habits." --owner team
Added task t-68d2dc73
```

The team task used the default roster (no pack installed).

### 4. Start

`Started task t-eea2b030 (run b-a6f86e83)`, `Started task t-68d2dc73 (run
ff7c2dac-…)`. A second `start` of the running task was refused (exit 1).
Before fix 3 the refusal read `aivyx-pa: protocol error: board_bad_state:
can't start a task that is Running`; after it, `aivyx-pa: can't approve a
task that is Todo` (the code is no longer shown).

### 5. Results

- **Role run** (`t-eea2b030`): Review within seconds. The thread: the
  agent's `board.comment` "Done.", a second comment with its answer, then
  `run b-a6f86e83 succeeded, 40003 tokens: An assistant can help with …`
  and `running → review`. (The token count is the whole turn, system prompt
  and tool list included.)
- **Team run** (`t-68d2dc73`), first try: the researcher specialist made
  ~20 `web.fetch` calls and didn't finish its turn; the log said `mission
  ff7c2dac… drive failed — team error: team config: specialist
  "researcher" did not complete its turn`. **Finding (fix 1):** the team
  mission stayed `executing` with nothing running it, so the task stayed
  Running until the restart in step 8's first pass returned it to Todo
  ("interrupted by a daemon restart").
- Second try, after fix 1: the same specialist failure (a model failure),
  and now `run 57584bd4-… failed: team error: team config: specialist
  "researcher" did not complete its turn, 0 tokens` and `running → todo`
  within 30 s. ✅ correct behaviour.
- Third try, after an operator comment "Answer from your own knowledge in
  one step; no web lookups needed." (the comment reached the brief):
  `run 48725881-… succeeded, 0 tokens: I've written three concise habits
  to a file in your workspace: 1. Delete or archive notes …` → Review.
  Team runs show 0 tokens because the team meter runs only with a
  `[budget]` mission cap (documented in `docs/BOARD.md`).
- **Finding (fix 2):** `board show` never printed a finished run's mission
  id (only the active run's), so a run entry couldn't be matched to its
  Missions record; the Studio thread showed neither the id nor the answer
  (`run Succeeded (120 tokens)`, `run Failed { reason: "…" }`). Both now
  print `run <mission id> <outcome>` and the answer.

### 6. Send back

```
$ aivyx-pa board send-back t-eea2b030 "Too generic. Start your answer with the word PINEAPPLE."
Sent task t-eea2b030 back to todo
$ aivyx-pa board start t-eea2b030
Started task t-eea2b030 (run b-5705debf)
```

`run b-5705debf succeeded, 26643 tokens: PINEAPPLE: An assistant can help
with writing, research, and file management. …` → Review. The answer
follows the note, so the note was in the brief. (The audit chain doesn't
carry prompt text, by design, so the answer is the evidence.)

### 7. Approve

`Approved task t-eea2b030` → `done`. A second approve: `can't approve a
task that is Done` (exit 1).

### 8. Restart mid-run

`t-5985bd52` ("Write a 300-word note …", `role:default`) started, `running`
3 s later (the agent had already commented), then `daemon stop`. On
restart the daemon printed `aivyx-pa: 1 board task(s) were running when the
daemon stopped; back to Todo`; the thread:
`run b-41343efc interrupted, 0 tokens: interrupted by a daemon restart`,
`running → todo`. Approving it then was refused (`can't approve a task that
is Todo`). The same sweep earlier returned the stuck team task (step 5).

### 9. Audit

With the daemon stopped, `aivyx-pa --verify-only` → `audit: verified 70
events (head_seq=69)` (there is no `audit verify` subcommand; `audit
export` needs the daemon stopped, it holds the store lock).
`audit export --event-type board_changed` → 25 entries, every event exactly
`{kind, op, object, actor}`: created ×5, started ×6, finished ×4 (agent),
commented ×4 (agent) and ×1 (operator), subtask_added ×1 (agent),
sent_back ×1, approved ×1, interrupted ×2. No title, description or
comment text appears (grep for the titles and "PINEAPPLE": 0 hits).

The role run on `t-5985bd52` added a subtask that repeats its own title
(`t-6b3d2ea6`, unowned): the 9B's choice, harmless.

### 10. Studio (deferred)

The operator chose to release 1.3.0 without this step (2026-11-19): the board ships as a preview, and the click-through is done later on the released Studio. The view's helpers have native unit tests, and CI builds and lints the wasm crate.

The daemon is left **stopped**. The config now has `[studio] port = 17843`
(the operator's daemon holds 7843). The binary embeds the bundle built by
`just build-web` in this task. To run the check:

```sh
S=<the session scratchpad>   # exact path in the task report
ln -sfn $S/board-live/run <short symlink>   # the wrapper's runtime dir
cd $S/board-live
nohup $S/bl.sh daemon run > daemon-studio.log 2>&1 &
$S/bl.sh studio            # prints the sign-in link for http://127.0.0.1:17843/
# … click through …
$S/bl.sh daemon stop
rm <short symlink>
```

State waiting for it: `t-68d2dc73` (team) in **Review** (Approve or Send
back), `t-5985bd52` (`role:default`) and `t-6b3d2ea6` (no owner) in
**To do** (Start; set an owner), `t-eea2b030` in **Done**. To check:
the goal and project filters, the task panel and its thread (run entries
show the mission id and the answer), Start, Approve, Send back with a
note, the Review badge on Board, the guide's "?" opening "The board", and
no panics in the browser console.

## Fixes

1. **A board team run whose drive failed stayed Running.** A drive error
   (a specialist that doesn't finish its turn) left the team mission
   `executing` with nothing driving it, and the board waits for a
   terminal phase. Board-started missions now halt with the error as the
   reason (`spawn_drive_halting_on_error`), so the task goes back to To do
   with it. Test: `board::service::tests::team::a_team_run_whose_drive_fails_goes_back_to_todo`.
   Other team missions keep the old behaviour (see the report).
2. **Run entries didn't say which mission, and the Studio hid the
   answer.** `board show` prints `run <mission id> <outcome>, <n> tokens`
   and the answer (no trailing `: ` when there is none); the Studio thread
   prints the same, with the answer under it. Tests:
   `board::tests::show_renders_the_thread_and_neutralises_text`,
   `board_view::tests::describes_entries`.
3. **Board refusals read as protocol errors.** The CLI printed
   `protocol error: board_bad_state: …`; it now prints the message alone.
   Test: `daemon_client::tests::a_board_refusal_reads_as_its_message`.

## Clean-up

No throwaway daemon left running (`pgrep`: only the operator's pid 1894);
ports 17842/17843 free; the runtime-dir symlink removed.
The throwaway HOME is kept in the scratchpad for step 10.
