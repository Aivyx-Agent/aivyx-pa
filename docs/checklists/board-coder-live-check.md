# aivyx-coder board worker live check (A22, Task 6)

- **Date:** 2026-11-21.
- **Build:** debug `aivyx-pa` from branch `board-coder` (1.4.0 + coder
  workers, Task 5 head `1961ee32`), built with
  `cargo build -p aivyx-cli --bin aivyx-pa`. Debug `aivyx-coder` 1.1.0 from
  `aivyx-coder` `main` at `7c0e102`, built with
  `cargo build -p aivyx-coder --bin aivyx-coder`.
- **Model:** llama-server at `http://127.0.0.1:8090`, Qwen3.5-9B Q4_K_M,
  for both pa (`provider = "llamacpp"`) and coder (`[backend] base_url =
  "http://127.0.0.1:8090/v1"`, `locality = "local"`). Nothing used Lemonade
  or any non-local server.
- **Isolation:** as in the agents live check (`board-agents-live-check.md`):
  a throwaway HOME in the session scratchpad (`board-coder-live/home`), every
  command through a wrapper (`scratchpad/bc.sh`) that runs
  `env -i HOME=<tmp> … XDG_RUNTIME_DIR=/tmp/claude-1000/bcrun
  AIVYX_PA_PASSPHRASE=<throwaway>
  AIVYX_CODER=<aivyx-coder>/target/debug/aivyx-coder target/debug/aivyx-pa "$@"`
  (`bcrun` is a short symlink to `<tmp>/run`). Own webhook port 17862 and
  Studio port 17863. The operator's daemon (pid 1894) and
  `~/.config/aivyx-pa` were never touched. The wrapper also set a dummy
  `OPENAI_API_KEY`, to check that pa strips it from coder's environment.
- **coder's config** (`<tmp>/.config/aivyx-coder/config.toml`): the 9B as
  above, `context_tokens = 32768`; one allowed command `test` =
  `cargo test` and `[verification] command = "test"`;
  `[sandbox] extra_read_paths` = the real `~/.rustup` and `~/.cargo` (the
  HOME is a throwaway, so coder's `$HOME/.rustup` default doesn't reach the
  toolchain); `[autonomous] max_iterations = 12`, `max_duration_secs = 900`.
- **Scratch project:** `board-coder-live/proj`, a library crate `scratch`
  with `inc(x)` and one test, `target/` ignored, git-initialised (one
  commit). `df -h /home` before the builds: 655 GB free.

## Steps

| # | Step | Result |
|---|---|---|
| 1 | `workspace add` works; `$HOME`, the data dir (and inside it), the config dir, `/`, a file, a missing path and a duplicate folder are refused | ✅ |
| 2 | "add `double(x)` with a test", owned by `coder:proj`, `board start` → Review, with the notice, changed files and checkpoints in the thread | ✅ (the 9B did it in about 10 s) |
| 3 | `/undo` in aivyx-coder reverts it | ✅ (`aivyx-coder --resume`, then `/undo`) |
| 4 | A second Start in the same workspace while one runs is refused as busy | ✅ |
| 5 | Cancel mid-run ends Cancelled, and coder exits | ✅ (coder gone 0.5 s after the cancel) |
| 6 | A coder agent with `--wake-on-assign` starts a task by itself | ✅ |
| 7 | Restarting the daemon mid-run puts the task back in To do, "interrupted" | ✅ |
| 8 | `--verify-only` passes; `workspace_added` carries no path | ✅ |
| 9 | Studio: the owner picker and the workspace list | ⏳ for the controller |

No pa bug found; nothing needed a fix. Two doc corrections came out of it
(see the notes for steps 3 and 5).

## Notes

### 1. Workspaces

```
$ aivyx-pa board workspace add proj <scratch>/board-coder-live/proj
Added workspace proj
$ aivyx-pa board workspace add home <tmp HOME>
aivyx-pa: your home folder itself can't be a workspace; pick a project folder inside it
$ aivyx-pa board workspace add data <tmp HOME>/.local/share
aivyx-pa: a workspace can't contain or sit inside aivyx-pa's own folders
$ aivyx-pa board workspace add d2 <tmp HOME>/.local/share/aivyx-pa
aivyx-pa: a workspace can't contain or sit inside aivyx-pa's own folders
$ aivyx-pa board workspace add cfg <tmp HOME>/.config/aivyx-pa
aivyx-pa: a workspace can't contain or sit inside aivyx-pa's own folders
$ aivyx-pa board workspace add root /
aivyx-pa: the filesystem root can't be a workspace
$ aivyx-pa board workspace add f <proj>/Cargo.toml
aivyx-pa: the workspace path isn't a folder
$ aivyx-pa board workspace add nope <scratch>/board-coder-live/missing
aivyx-pa: the workspace path doesn't exist
$ aivyx-pa board workspace add proj2 <proj>
aivyx-pa: another workspace already uses that folder
$ aivyx-pa board workspace list
proj  <scratch>/board-coder-live/proj  idle  open 0
```

Every refusal exits 1. `.local/share` is refused because it *contains* the
data dir.

### 2. The coding task

Goal `g-151d7d54`, project `p-fda02421`. Task `t-becd9db4` "Add double",
`--owner coder:proj`, brief: "In src/lib.rs, add a public function
`double(x: i32) -> i32` that returns x doubled, with a unit test for it in
the existing tests module. Keep the existing inc function and its test."
Started 08:58:17; in Review within about 10 s:

```
2026-11-21 08:58  operator  todo → running
2026-11-21 08:58  agent  run b-5aee0425 succeeded, 60013 tokens: autonomous run stopped: goal achieved after 2 iteration(s)\n  verification: `test` passed\n  changed: 1 file (+10 −0) — src/lib.rs\n\n1 changed file; 4 checkpoints — undo with /undo in aivyx-coder
2026-11-21 08:58  agent  running → review
```

(`board show` prints the answer's newlines as `\n`, as for every run.) The
diff was exactly `double` plus a `double_returns_doubled` test; the project
had three `refs/aivyx/checkpoints/*` refs and an untracked `Cargo.lock` from
`cargo test`.

While the next run (step 4) was going, `/proc/<pid>` of the coder child
showed: parent the throwaway daemon, current directory the workspace, and
an environment of only `AIVYX_CODER CARGO_HOME HOME PATH RUSTUP_HOME TERM
XDG_*`: the dummy `OPENAI_API_KEY` and `AIVYX_PA_PASSPHRASE` were stripped.

### 3. Undo

`/undo` is a TUI command. The headless run left a coder session for the
project (`<tmp>/.local/state/aivyx-coder/sessions/proj-…/`), whose undo
ledger held the run's mark. Driven through a pty (`script`):
`aivyx-coder --resume` ("resumed previous session (16 messages restored)"),
`/undo`, which showed the confirmation (`~src/lib.rs`, "[y] Undo
[n] Cancel"), then `y`: "Undone: src/lib.rs". `git diff` was empty after it
and `double` gone from `src/lib.rs`.

**Doc correction:** `docs/BOARD.md` and the guide first said only "/undo in
aivyx-coder". A fresh `aivyx-coder` session has an empty ledger, so they now
say to open the run's session with `aivyx-coder --resume` first, and name
the git refs as the other way.

### 4. Busy

Two tasks added for `coder:proj`: `t-80bd3840` "Add math helpers" (triple,
square, is_even, each tested) and `t-7249d580` "Add negate".

```
$ aivyx-pa board start t-80bd3840
Started task t-80bd3840 (run b-c0d23984)
$ aivyx-pa board start t-7249d580
aivyx-pa: the workspace is busy                      (exit 1)
$ aivyx-pa board workspace list
proj  <proj>  busy  open 3
```

`t-80bd3840` reached Review at 09:00:17 ("goal achieved after 2
iteration(s) … changed: 1 file (+43 −0) … 1 changed file; 4 checkpoints"),
and the list went back to `idle`.

### 5. Cancel

`board start t-7249d580`, 4 s later `board cancel t-7249d580` at
09:00:28.84. The coder child (pid 2002131) was gone by 09:00:29.35: it
exits on the first SIGTERM, so the 30 s kill wasn't needed. The thread:

```
2026-11-21 09:00  operator  todo → running
2026-11-21 09:00  operator  running → cancelled
```

No `cargo test` was left running, and the workspace was `idle` again.
The cancelled run had already added `negate` (without its test) to
`src/lib.rs`: a cancel doesn't undo coder's edits. **Doc correction:** the
board docs and the guide now say so (cancel, the ceiling and a restart
leave the edits; use `/undo`).

### 6. Coder agent

```
$ aivyx-pa board agent add dev --works-as coder:proj --wake-on-assign --title "Codes"
Added agent dev
$ aivyx-pa board agent list
dev  Codes  works as coder:proj  heartbeat —  last wake never  open 0
```

`t-c2e987ad` "Add halve" added unowned and assigned `agent:dev` at
09:00:43. To do until 09:01:14 at least, Running by 09:01:29 (30 s plus up
to one 15 s tick), Review at 09:01:39, the start written by the agent:

```
2026-11-21 09:01  agent  todo → running
2026-11-21 09:01  agent  run b-54ba75cc succeeded, 63920 tokens: autonomous run stopped: goal achieved after 2 iteration(s)\n  verification: `test` passed\n  changed: 1 file (+19 −0) — src/lib.rs\n\n1 changed file; 4 checkpoints — undo with /undo in aivyx-coder
2026-11-21 09:01  agent  running → review
```

Also: `agent add d2 --works-as coder:nope` → "no workspace named `nope`".

### 7. Restart

`t-7422863b` "Add clamp" (`coder:proj`) started, coder child pid 2003250;
4 s later `daemon stop`. Two seconds after it the coder child was gone
(`kill_on_drop`), and no process under the throwaway HOME was left. The
daemon was started again (pid 2003372); the task:

```
2026-11-21 09:01  operator  todo → running
2026-11-21 09:01  operator  run b-33a6d53a interrupted, —: interrupted by a daemon restart
2026-11-21 09:01  operator  running → todo
```

and the workspace was `idle`.

### 8. Audit

`board workspace remove proj` → "the workspace is in use" (exit 1; it owns
open tasks and agent `dev` works as it). A second workspace `tmpws` was
added and removed (exit 0). With the daemon stopped,
`aivyx-pa --verify-only` → `audit: verified 26 events (head_seq=25)`.
`audit export --event-type board_changed` → 23 entries, every event exactly
`{kind, op, object, actor}`: `workspace_added` ×2 and `workspace_removed`
×1 (objects `w-0d6ba0f2`, `w-3950abbd`, actor `operator`), `created` ×7,
`started` ×5 (4 `operator`, 1 `agent`), `finished` ×3 (`agent`),
`cancelled` ×1, `interrupted` ×1, `agent_created` ×1, `assigned` ×1,
`woke` ×1. No path, workspace name, task title or answer text appears
(grep for `proj`, `scratchpad`, `/tmp`, the task words and the agent name:
0 hits).

(`audit export` refuses while the daemon holds the store; it ran after
`daemon stop`.)

### 9. Studio (for the controller)

The daemon is left **stopped**. The config has `[studio] port = 17863`.
To run the check:

```sh
S=/tmp/claude-1000/-home-julian-Projects-Rust/2a2a3496-9f15-466a-9988-2a0a7bde93c0/scratchpad
ln -sfn $S/board-coder-live/home/run /tmp/claude-1000/bcrun   # the wrapper's runtime dir
cd $S/board-coder-live
nohup $S/bc.sh daemon run > daemon-studio.log 2>&1 &
$S/bc.sh studio            # prints the sign-in link for http://127.0.0.1:17863/
# … click through …
$S/bc.sh daemon stop
rm /tmp/claude-1000/bcrun
```

The binary embeds the bundle committed in Task 5 (the guide text changed
in this task, so `just build-web` before the release picks that up). State
waiting for it: workspace `proj` (idle, open 4), agent `dev` working as
`coder:proj`; tasks in Review (`t-becd9db4`, `t-80bd3840`, `t-c2e987ad`),
one Cancelled (`t-7249d580`) and one To do (`t-7422863b`, interrupted).
To check: the owner picker lists "coder proj", the agent form offers it
and keeps `dev` as a coder agent on Edit, the Agents tab's read-only
Workspaces list (proj, its path, idle/busy, open tasks), a coder run's
thread entry, and Start on `t-7422863b` (it runs the real coder against the
9B) showing busy while it runs. No panics in the browser console.

## Clean-up

No throwaway daemon or coder left running (`pgrep`: only the operator's pid
1894); ports 17862/17863 free. The `bcrun` runtime symlink is left for
step 9 (remove it afterwards, as above). `board-coder-live/` stays in the
scratchpad.

## Re-check after final-review fixes (2026-11-21)

Re-run on `board-coder` at `9f224624` (includes `54b9e4f9`): debug `aivyx-pa`
rebuilt, same throwaway HOME, same 9B at `127.0.0.1:8090` (no cloud, no
port 13305). 628 GB free on `/home`. The daemon was started with
`setsid` (own process group) and
`EXTRA_ENV="AWS_ACCESS_KEY_ID=dummy CEREBRAS_API_KEY=dummy"` on top of the
wrapper's dummy `OPENAI_API_KEY`. The operator's daemon (pid 1894) was not
touched.

| # | Check | Result |
|---|---|---|
| 1 | Build and df | ✅ |
| 2 | Review, counts line and undo hint | ✅ `t-bd32d21e` "Add quintuple": "1 changed file, 4 checkpoints — undo: run aivyx-coder --resume in the workspace, then /undo", then the run text; Review in about 10 s. A first task (`triple`, which the earlier check had already added) correctly said "0 changed files, 1 checkpoint" |
| 3 | Environment scrub | ✅ coder's `/proc/<pid>/environ` held only `AIVYX_CODER CARGO_HOME HOME PATH RUSTUP_HOME TERM XDG_*`; 0 matches for `OPENAI_API_KEY`, `AWS_*`, `CEREBRAS_API_KEY`, `*_API_KEY`, `AIVYX_PA_*` |
| 4 | Process group | ✅ coder pid 2053338, pgid 2053338, ppid the daemon (a second run: 2053752 / 2053752) |
| 5 | Restart mid-run | ✅ `kill -TERM -- -<daemon pgid>` (the group held only the daemon, since coder has its own group): task `t-c1d99d6f` went `running → todo` with "interrupted by a daemon restart", not Cancelled; no aivyx-coder, cargo or throwaway aivyx-pa left; the workspace was idle after the restart |
| 6 | Cancel | ✅ `t-d7411f0f` `board cancel` → Cancelled; coder gone within 10 ms of the command returning; workspace idle |
| 7 | Clean-up | ✅ `daemon stop`; `ps` shows only the operator's pid 1894 |

Not exercised: the exit-130-not-cancelled path (the daemon's own SIGTERM
went through the restart handler, not coder's 130 code), and
`PR_SET_PDEATHSIG` (would need a SIGKILL of the daemon). No failures; the 9B
did the coding each time it ran to completion.
