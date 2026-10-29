# aivyx-pa code audit, 2026-10-28

A security and correctness review of aivyx-pa at `cc80c581` (v0.19.3 plus one
dependency-pin commit). It reports and proves findings; it fixes nothing.
The fixes are a separate, later piece of work.

## Scope

- Commit range since the last full audit (v0.18.1, 2026-10-11):
  `git log v0.18.1..HEAD`, 125 commits. The big additions are the Matrix,
  Proton Mail and Signal channels, the process-group and exit-signal work
  (v0.19.1), the `$HOME`-covering shell confinement (SH1) and the
  shared-crate pin bumps. Per-area autonomy, supervised batching and named
  instances predate v0.18.1 but were re-read for the approval questions.
- The review covered the code as a whole against the questions below, not only
  the diffs. Areas that outside parties or the model control got depth:
  channels (who can start a turn, reach a tool, resolve an approval),
  approvals and autonomy gates, the Studio/IPC surface, packs, data readers,
  and every place pa runs a program against a directory the confined agent can
  write.
- Out of scope: `aivyx-checkpoint` and the other shared crates (audited
  separately). pa's own use of them was checked for the same class of bug.
- Earlier decisions were not re-reported. Their fixes were re-checked (see
  "Checked and clean").

## Method

- Ranks: **Critical** (an attacker or a model controls something it must not:
  code execution outside the confinement, approval bypass, secret
  disclosure), **High** (the same with a precondition, or data loss),
  **Medium** (a real defect a user will hit, or a hardening gap with a
  plausible path), **Low** (everything else worth noting).
- A Medium-or-worse finding counts only once shown real: by a failing test on
  the local scratch branch `audit/2026-10-28` (one commit on top of `main`;
  never merged, never pushed) or by a recorded reproduction. Suspicions without
  proof are under "Not confirmed".
- The proving tests are named in each finding. To run them:
  `git checkout audit/2026-10-28` and
  `cargo test -p aivyx-core --lib audit_`,
  `cargo test -p aivyx-channel --lib audit_`,
  `cargo test -p aivyx-channel --test audit_2026_10_28`,
  `cargo test -p aivyx-telegram --test audit_2026_10_28`,
  `cargo test -p aivyx-cli --bin aivyx-pa pack_check::tests::audit_a`.
  All probes use temporary directories and local fixtures; nothing touched a
  real instance, key or network.
- Pass/fail on the scratch branch: every test listed below fails (that is the
  proof), except `a_transport_error_does_not_carry_the_bot_token`, which passes
  and documents a clean path.

## Summary

| Rank | Count | IDs |
|---|---|---|
| Critical | 0 | |
| High | 3 | P1, P2, P3 |
| Medium | 4 | P4, P5, P6, P7 |
| Low | 7 | P8 to P14 |

## Findings

### P1 (High) The "operator replied" ledger approves any later call to the same tool

- **Where:** `crates/aivyx-core/src/confirm.rs:44` (`take_refusal`),
  `crates/aivyx-core/src/agent.rs:1946`, `:2034` and `:2053`
  (`run_tool_call`).
- **What happens:** a confirm-first call that cannot ask the operator is refused
  and remembered as `(session, tool name)`. In any later turn of that session,
  a call to the same tool name runs once without a prompt: a model-supplied
  `confirmed: true` is honoured for confirm-first tools like `fs.delete`, and
  the "ask first" gate (withheld integration writes such as `email.send`,
  `proton.send`, `signal.send`; the `manual` level) is skipped. The ledger
  keys on the tool name, not the call, and has no expiry. So one earlier
  refusal for `todo.md`, or one unanswered "send this to Alice", pre-approves
  the next call to anyone, with any arguments, as soon as any new message
  arrives in the session, including an unrelated question.
- **Who is exposed:** only frontends that never ask in-line use this path.
  Telegram, Discord, Slack, Matrix, Signal, Proton and the TUI never send
  `SetApprovals` (see the comments at `telegram_daemon_frontend.rs:659` and
  `aivyx-tui/src/model.rs:846`), so for them the ledger is the only gate. The
  TUI runs at Trusted tier, so it reaches `fs.delete`, `git.commit`,
  `email.send`, `message.send` and skill authoring. A page the operator asks the
  agent to summarise, or a mail read with `proton.read`, can carry the
  instruction, if some earlier refusal is pending in that session.
- **Proof:** `agent::tests::audit_a_refusal_for_one_target_does_not_approve_another_target`
  (turn 1: `fs.delete todo.md` is refused; turn 2: `fs.delete
  /precious/thesis.tex` with `confirmed: true` runs, the log shows it ran with
  `confirmed: true`) and
  `agent::tests::audit_a_refused_send_to_one_recipient_does_not_approve_another`
  (turn 1 refused to alice; turn 2 to `attacker@evil.example` completes).
  Both are in `aivyx-core`.
- **Suggested fix:** key the ledger on `(session, tool, hash of the input
  without `confirmed`)`, so only the refused call can be approved. Expire
  entries after the approval window (10 minutes). Require the approving turn's
  call to match the refused one exactly; a different call asks again. Apply
  the same key to the `ask_reason` filter at `agent.rs:2034`.

### P2 (High) The loop's `gate_command` runs agent-written code unconfined, with the daemon's secrets

- **Where:** `crates/aivyx-channel/src/loop_gate.rs:116` (`ShellGateRunner::run`),
  built at `crates/aivyx-channel/src/daemon_server.rs:1055`.
- **What happens:** with `[loop]` armed and `gate_command` set (for example
  `cargo test`, which the docs recommend), the daemon runs `sh -c <command>` in
  `working_dir` at daemon privilege, with no confiner and with the daemon's
  whole environment. `working_dir` is the folder the loop's confined agent
  writes code into. Whatever the agent left there (`build.rs`, a test, a
  Makefile, `.cargo/config.toml`, a `gate.sh`) therefore runs outside the
  Landlock/seccomp confinement, and sees `AIVYX_PA_PASSPHRASE` and every
  provider key. `shell.exec` and `git.*` already clear the environment
  (`SAFE_ENV_DEFAULTS`) and confine; the gate does neither. The code comment
  treats the command as operator config, but the files the command executes
  are the agent's. This is the same class as the checkpoint `.git/config`
  finding: an unconfined helper trusting files the confined agent can write.
  Precondition: the operator armed the loop with a gate.
- **Proof:** `gate_runs_agent_written_code_without_the_daemons_secrets` in
  `crates/aivyx-channel/tests/audit_2026_10_28.rs`: a `gate.sh` placed in the
  working directory writes `$AIVYX_PA_PASSPHRASE` to a file and the gate
  reports `Passed`; the file holds the canary.
- **Suggested fix:** run the gate through `default_confiner_with_options` rooted
  at `working_dir` (same policy as `shell.exec`), after `env_clear()` plus the
  safe defaults (and an explicit pass-through list the operator can extend for
  `CARGO_HOME`, proxy variables), in its own process group killed on timeout
  and on daemon exit. Say in `docs/INSTALL.md` that the gate executes the
  agent's work.

### P3 (High) `fs.write` can rewrite the daemon's own config, packs and tool-process files

- **Where:** `crates/aivyx-core/src/sensitive_paths.rs:84` (the built-in
  Portcullis lists), `crates/aivyx-cli/src/bin/aivyx.rs:6823` and `:7989`
  (`ward_extra_deny` supplies only the kvcache store and the Studio token),
  enforced at `crates/aivyx-core/src/tools/fs.rs:895`.
- **What happens:** `shell.exec`'s confiner denies every instance's config,
  data and `~/.aivyx-pa` directories (`shell_confine_deny_paths`,
  `all_instances_state_dirs`). The file tools' guard does not: Portcullis
  knows shell rc files, `authorized_keys`, systemd/cron/autostart and git
  hooks, and Ward knows secrets by name and extension. At access `home` or
  `full` (the sandbox root covers `$HOME`), `fs.write` can therefore create or
  overwrite `~/.config/aivyx-pa/aivyx-pa.toml` (and any
  `instances/<n>/aivyx-pa.toml`), `team.toml`, `~/.aivyx-pa/packs/<name>/<ver>/bin/*`
  (installed tool-pack binaries) and `~/.aivyx-pa/tool-processes/*`. The next
  daemon start obeys them at daemon privilege: `[[tool_process]]` and
  `[[mcp_server]]` commands, `[access] level = "full"`, `[autonomy]`,
  `[confine]`. That breaks "an agent can never widen its own reach or
  autonomy". Overwriting an existing file asks first at every level short of
  `unleashed`, but creating a new file does not (a new instance config, a new
  pack folder), and the ask itself can be bypassed through P1 on the TUI.
  Precondition: access level `home` or `full` (or a `workspace` root that
  contains these folders).
- **Proof:** `tools::fs::tests::audit_fs_write_cannot_rewrite_the_daemons_own_config`
  (the real `FsWriteTool` with the guard as the binary builds it replaces
  `.config/aivyx-pa/aivyx-pa.toml` with a `[[tool_process]]` entry) and
  `sensitive_paths::tests::audit_portcullis_covers_the_daemons_own_config_and_tool_binaries`
  (`classify_write` returns `None` for the five paths), both in `aivyx-core`.
- **Suggested fix:** put `InstancePaths::all_instances_state_dirs()` (the list
  the shell confiner already uses) into `extra_deny` for Ward and Portcullis,
  with the agent workspace carved out as the existing tests expect. Add the
  pack and tool-process directories explicitly.

### P4 (Medium) Approval prompts can hide what they ask about

- **Where:** `crates/aivyx-core/src/agent.rs:1151` (`approval_summary`),
  `crates/aivyx-channel/src/approval_prompt.rs:8` (`render`), and the Studio
  card (`crates/aivyx-web/src/main.rs:4342`, `approval_args_preview`).
- **What happens:** the summary names the first of `path`, `repo`, `to`,
  `recipient`, `purchase_order_id`, `name` that is a string, else the first
  string value in key order (`serde_json` sorts keys). `proton.send` takes `to`
  as an array, so the summary skips it and shows the tail of the `body`. The
  terminal prompt shows only the first 200 characters of the arguments (the
  Studio card, 2000 characters, inside a collapsed "Arguments" section) and
  `body` sorts before `cc`, `subject` and `to`. An email with a long body is
  approved without its recipients on screen; the sender of the injected
  instruction chooses the recipients. The same cut hides the middle of a long
  `shell.exec` command in the terminal (the prompt shows the head and the
  summary shows the last 79 characters).
- **Proof:** `agent::tests::audit_the_approval_of_a_send_names_who_it_goes_to`
  in `aivyx-core` (the summary for a `proton.send` input does not contain the
  recipient) and `approval_prompt::tests::audit_the_prompt_shows_the_whole_command_it_asks_about`
  in `aivyx-channel` (the rendered prompt for a 450-character command does not
  contain the middle segment).
- **Suggested fix:** build the summary from named fields per tool class
  (recipients, subject, byte count of the body), flatten arrays, and always
  show recipients. When arguments are cut, say how much is hidden, or refuse
  to prompt and show the whole value on request.

### P5 (Medium) Two more readers block or run unbounded on files the agent can create

- **Where:** `crates/aivyx-channel/src/parked_steps.rs:345` (`preview_for`,
  `std::fs::read` of the path in a parked call) and
  `crates/aivyx-channel/src/document_browse.rs:121` (`read_file`,
  `File::open`).
- **What happens:** the earlier fix (`2564e809`) made `fs.read` and the data
  readers refuse a FIFO. These two paths were missed. A confined agent can
  `mkfifo notes` in its folder. The Studio's review list calls `preview_for`
  for every pending parked step (an `fs.*` call with a `path`), and the
  Documents browser calls `read_file` for the file the operator opens; both open
  the FIFO on a daemon worker thread and block until a writer appears, which
  never happens. Each refresh or click parks one more worker, and the daemon
  stalls once the workers are used up. `preview_for` also reads the whole file
  with no size limit (`Vec` grows without bound for a very large or endless
  file). Preconditions: for the preview, an `fs` area at `supervised` and an
  unattended run that parks a write to the FIFO; for the browser, the operator
  opening the file in the Documents view (any root that includes the agent's
  folder, which the default sandbox does).
- **Proof:** `parked_steps::tests::audit_a_preview_of_a_fifo_does_not_hang` and
  `document_browse::tests::audit_reading_a_fifo_does_not_hang_the_documents_browser`
  in `aivyx-channel`; both time out after 3 seconds.
- **Suggested fix:** use `aivyx_core::tools::fs::open_regular_file` (it opens
  with `O_NONBLOCK` and checks `is_file`) and a byte cap in both, on
  `spawn_blocking`.

### P6 (Medium) A config pack's `[[role]]` and team members can hold any capability scope

- **Where:** `crates/aivyx-cli/src/bin/aivyx_modules/pack_check.rs:49`
  (`PACK_SECTIONS` allows `role`) and `:104` (`check_pa_part` only counts team
  members).
- **What happens:** the 2026-10-11 fix limits a template to an allowlist of
  sections, but `role` is on it, and a role's `capability_scopes` is its
  capability envelope. A pack that defines `[[role]] name = "default"
  capability_scopes = ["shell.exec", "fs.delete", "net.post", "email.send",
  "message.send"]` passes `pack check`, and neither `pack check` nor
  `pack inspect` mentions the scopes (the summary prints routines, autonomy,
  team size, skills and requirements). A team member's `capability_scopes` and
  `trust_ceiling` in the pack's `team.toml` are never inspected either. The
  runtime ceilings and the withheld-integration confirm still apply, so this is
  not a bypass, but the pack widens what the new agent may do without the
  operator being told, which the check's own header promises it will not.
- **Proof:** `pack_check::tests::audit_a_pack_role_cannot_grant_itself_capability_scopes`
  and `pack_check::tests::audit_a_pack_team_member_cannot_hold_dangerous_scopes`
  in `aivyx-cli`.
- **Suggested fix:** refuse role and team-member scopes outside a small
  allowlist (read-only and memory scopes), or list every granted scope in
  `pack inspect` and `pack install` and require the operator's yes.

### P7 (Medium) In-process channel sessions ignore the `manual` areas

- **Where:** the agent builders in `crates/aivyx-telegram/src/session.rs:473`
  and `:853`, `aivyx-discord/src/session.rs:243`, `aivyx-slack/src/session.rs:191`,
  `aivyx-matrix/src/session.rs:77`, `aivyx-signal/src/session.rs:204` and
  `aivyx-proton/src/session.rs:363`. Compare the REPL and voice at
  `crates/aivyx-cli/src/bin/aivyx.rs:11634` and `:12495`, which call
  `.with_confirm_all_areas(..)`.
- **What happens:** `manual` autonomy, and `[[autonomy.override]]` set to
  `manual` for an area, are applied through `TurnSafety::with_confirm_all_areas`.
  The six chat sessions that run in-process build their agent with
  `TurnSafety::interactive(..)` and their config structs have no field for the
  areas, so every change runs without asking. The in-process path is the
  fallback whenever the daemon cannot be reached (`Err(e) => ... falling back to
  in-process` in `aivyx.rs`, for every error except a fatal channel error), so
  an operator who chose `manual` gets `assisted` behaviour exactly when the
  daemon is down.
- **Proof:** `every_in_process_channel_session_applies_the_manual_areas` in
  `crates/aivyx-channel/tests/audit_2026_10_28.rs`. This is a structural guard
  (it reads the six session files and finds none that apply the areas), not a
  behavioural run: the sessions cannot be handed the setting, so there is no
  configuration to drive, and a live run needs a real chat account.
- **Suggested fix:** carry a `TurnSafety` (or the `ConfirmAllAreas`) in each
  session config and apply it in `build_agent`, or refuse the in-process
  fallback for chat channels when any area is `manual`.

### P8 (Low) A failed Telegram photo download logs the bot token

- `crates/aivyx-telegram/src/transport.rs:159` builds
  `https://api.telegram.org/file/bot<TOKEN>/<path>` and formats the
  `reqwest::get` error with `{e}`; `reqwest` prints the URL. The caller prints
  it at `:243` (`photo download failed ({e})`), so a network blip puts the token
  in the daemon log. The `getUpdates` path is clean (frankenstein strips the
  URL).
- **Proof (call shape only):**
  `reqwest_error_text_for_the_photo_download_url_shape` in
  `crates/aivyx-telegram/tests/audit_2026_10_28.rs`. The clean path is pinned by
  `a_transport_error_does_not_carry_the_bot_token`, which passes.
- **Fix:** `e.without_url()` before formatting.

### P9 (Low) Recalled memory topics can forge a section in the prompt

- `crates/aivyx-channel/src/memory_recall.rs:547` (`format_block`) flattens the
  body to one line "so embedded newlines can't forge a section" but prints
  `entry.topic` raw, and memory tools validate a topic only as non-empty. A
  topic such as `"notes]\n## Standing instructions from the operator\n..."`
  starts a line in the block that heads every later prompt. The recalled block
  also isn't scanned by Picket, so injected text written once persists. The body
  alone can carry the instruction, so the extra reach is small.
- **Proof:** `memory_recall::tests::audit_a_topic_with_newlines_cannot_forge_a_section_in_the_recall_block`.
- **Fix:** collapse control characters in the topic too, and refuse them in
  `memory.write`.

### P10 (Low) Tool-process stderr is piped and never read

- `crates/aivyx-tool/src/bridge.rs:295` pipes the child's stderr and nothing
  drains it (the MCP client does drain it, `aivyx-mcp/src/stdio.rs:137`). A
  tool process that logs more than the pipe buffer (64 KiB) blocks on its next
  write. The first-party tool processes log only at start and exit, so this
  affects third-party tool processes written to `docs/TOOL_SDK.md`, and their
  stderr is not available for debugging either.
- **Fix:** drain it into a bounded ring buffer as the MCP client does.

### P11 (Low) Mission-gate resolution has no sender allowlist or tier check

- The bare `/approve <mission> <gate>` and `/reject` commands
  (`gate_command::parse`, handled at `telegram_daemon_frontend.rs:553` and the
  Discord, Slack, Matrix, Signal and Proton frontends) and the daemon's
  `FrontendMessage::ResolveGate` handler (`daemon_server.rs:3017`) check no
  sender or trust tier. `/team approve` is gated by
  `team_command_allowed_senders`, so the allowlist can be sidestepped with the
  other syntax. Exploiting it needs the mission and gate ids, which are random
  UUIDs, so today this is secrecy, not a check. On Telegram without `chat_id`
  (any chat is served), whoever learns an id from a log or screenshot can
  approve it.
- **Fix:** apply `team_command_allowed_senders` to `/approve` and `/reject`, and
  refuse `ResolveGate` from a connection below Trusted.

### P12 (Low) The federation envelope isn't bound to a recipient

- `crates/aivyx-federation/src/identity.rs:410` signs
  `instance_id:timestamp:sha256(body)`. Nothing names the receiving peer or a
  purpose, so a request signed for peer B verifies at peer C for 60 seconds
  (the replay guard is per receiver and in memory only, so a restart reopens the
  window). Today pa has no inbound federation transport, so nothing is
  reachable; fix it before one lands.
- **Fix:** add the recipient's instance id and a fixed context string to the
  signed message.

### P13 (Low) Workflow hygiene

- `.github/workflows/ci.yml` and `quality-gate.yml` set no `permissions:`; they
  take the repository default token rights. `wsl-release.yml:48` interpolates
  the `workflow_dispatch` input `tag` into a shell script (`TAG="${{ ... }}"`),
  which should go through `env:`. Third-party actions are pinned by tag, not
  commit. The release guard itself is intact (see below).
- **Fix:** `permissions: contents: read` at the top of both; pass the tag as an
  environment variable.

### P14 (Low) Derived `Debug` on secret-holding structs

- `aivyx_google_oauth::tokens::TokenSet` (access and refresh tokens),
  `aivyx_toolkit::config::BraveSearchConfig`, `aivyx_vision::config::MoldSettings`
  and the `config_write` request structs derive `Debug`. No `{:?}` of these
  reaches a log today (searched), but one added `eprintln!("{cfg:?}")` would
  print a key. The Matrix `Session`, Proton `BridgeCredentials` and `Login`
  already redact.
- **Fix:** hand-written `Debug` that prints `<redacted>`, as the others do.

## Not confirmed

- **Data readers and decompression bombs.** `data.xlsx` reads at most 8 MiB of
  compressed bytes but the workbook then expands in memory. A one-cell sheet at
  `XFD1048576` did not blow up with calamine 0.37 (probe passed under an 8 GB
  address limit; removed). A zip bomb (8 MiB compressed expanding to gigabytes
  of sheet XML) was not run because it would exhaust the machine. The readers
  also parse synchronously on the async worker.
- **Proton "Sent folder" proof.** Trust for mail "from you" rests on the
  message's `X-Pm-Internal-Id` being found in Sent, and on Bridge overwriting
  any copy the sender wrote (documented in `message.rs`). Not verifiable without
  a real Bridge.
- **Studio request head.** `web_ui.rs:458` peeks the first 4 KiB once. If the
  head arrived in two segments, a missing `Origin` would pass the WebSocket
  origin check (the token check still fails closed). Browsers write the head in
  one send, so no exploit was found; reading until the blank line would remove
  the doubt.
- **`/studio-asset` proxy.** It relays ComfyUI's `Content-Type` from the
  Studio's own origin. A planted HTML or SVG file in ComfyUI's input or temp
  folders would run script in the Studio origin if the operator opened the
  link. Needs a file in ComfyUI's folders; not shown.
- **Pack template `[agent] provider`/`model`, `[team] config_path`,
  `[skill_defaults] project_dir`.** All allowed in a template. The installer
  overrides the last two only when the pack ships a team config or skills.
  Whether the setup wizard lets a pack preselect a cloud provider was not
  traced.
- **Matrix inbound channel** is unbounded (a homeserver can queue events faster
  than turns drain them), and the once-per-room sets grow per room. Minor.
- **`workspace.read`** reads with `std::fs::read` and no size cap; only the
  agent's own workspace is reachable by it, and `shell.exec` cannot reach that
  folder.
- **Signal envelopes** in tests are synthesised from signal-cli's source; no
  real ones were recorded (carried over from the channel's own checklist).

## Checked and clean

- **Signal:** only an allowed sender whose key is `TRUSTED_VERIFIED` starts a
  turn; the lookup takes the least-trusted of several keys; groups, sync,
  reactions and receipts are not turns; `signal.send` needs a listed,
  verified recipient and the `message.send` scope (Trusted only, withheld).
- **Matrix:** the home room, an allowed sender and a verified device are all
  required; edits, notices and undecryptable events never start a turn; invites
  are joined only from the operator to the home room.
- **Proton:** From must be one of your addresses and in Sent, or an allowlisted
  sender with the first `Authentication-Results` value from
  `mail.protonmail.ch` showing `dmarc=pass` for the From domain; automatic
  mail is ignored; replies carry `Auto-Submitted`; recipients and subjects are
  checked against CRLF; the SMTP client rejects CR/LF and `<>` in addresses;
  external senders cannot resolve gates.
- **Telegram, Discord and Slack:** `/team` commands are deny-by-default behind
  `team_command_allowed_senders`; an unset filter serves chats at Untrusted
  tier, whose ceiling is near-empty; webhooks are loopback-only with a
  constant-time secret and fire at Untrusted (the 2026-10-11 fixes hold).
- **git.\*:** every git call runs through the confiner with a cleared
  environment, `core.fsmonitor=false`, `--no-ext-diff --no-textconv`, and a
  repo whose `.git` is a file is refused. The one unconfined git in pa
  (`is_inside_git_work_tree`, `aivyx.rs:6223`, `rev-parse --is-inside-work-tree`
  at startup) was tried against a repository whose `.git/config` set
  `core.fsmonitor`, `core.sshCommand`, `core.pager`, an alias and
  `diff.external` to a marker script: nothing ran. `aivyx-pa doctor` uses the
  checkpoint crate's refusal report, not git directly.
- **Other programs pa starts:** the clipboard tool, `xdg-open` (http/https
  only, after the same-origin check), `systemctl`/`launchctl`, `hostname`,
  `signal-cli` text, `$EDITOR`: none read agent-writable files. `shell.exec`
  clears its environment, runs in its own process group and is killed on
  SIGTERM/SIGHUP/Ctrl-C through `exit_signals`.
- **Studio/IPC:** token required for `/ws` and static routes (constant-time),
  cookie `HttpOnly; SameSite=Strict`, origin check on the upgrade, loopback by
  default with a loud warning otherwise, socket mode 0600 and runtime dir
  tightened to 0700. `web_ui.rs` and `studio_token.rs` are unchanged since the
  2026-10-11 live check; not re-run live.
- **Packs:** format-1 `version` is validated (K1), `unpack_payload` rejects
  traversal and links, the template allowlist and `[agent]` scan keys hold
  (K2), `pack inspect` escapes pack text (L3), `bin` and `team_config`
  traversal are refused, a pack installs only as a new instance.
- **Autonomy:** `confirm_destructive_for` no longer lets the access level
  switch confirmation off below `unleashed`; integration writes ask at every
  level; per-area overrides reach both the confirm-all table and the parker;
  parked steps cap at 100, lock around resolve, and expire.
- **Prompt injection:** every bridged tool process, MCP proxy, `fs.read`,
  `web.*`, `git.diff/status`, `shell.exec` and the data readers are fenced and
  scanned; the injection-guard pin includes the 2026-10-28 shared-crate fixes.
- **Secrets:** `daemon.env` and the passphrase file are 0600 and the unit
  carries no secret; Matrix `Session`, Proton credentials and `Login` redact in
  `Debug`; no key reaches the audit export (inputs are hashed).
- **Egress:** Rampart's CGNAT, `0/8`, multicast and IPv4-in-IPv6 blocks (R1)
  are in place.
- **Federation:** signature, freshness (60 s, 5 s future skew) and a bounded
  replay guard behave as documented; no inbound transport exists in pa.
- **Release workflow:** the `host` job still needs the quality gate and both
  build jobs and fails the run, publishing nothing, unless all three are
  `success` (`release.yml:239`); `allow-dirty = ["ci"]` is set.
- **Previously fixed items still hold:** FIFO on `fs.read` and the data readers
  (but see P5), MCP read limits, `pack inspect` escaping, desktop navigation
  handler, webhook oracle and overlap guard, `SH1` carved home notice.

## What was not checked

- The Studio, desktop webview, Signal, Matrix and Proton against real
  services or a real browser; macOS and Windows code paths; the voice feature.
- The full workspace test suite was not rerun as a baseline. Targeted tests in
  the crates touched by the scratch branch compile and run.
- Dependency advisories (`cargo deny`) were left to the dependency pass.
