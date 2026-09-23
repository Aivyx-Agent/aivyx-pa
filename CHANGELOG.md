# Changelog

All notable changes to Aivyx are recorded here. This project adheres to
[Semantic Versioning](https://semver.org). Dates are ISO-8601.

## [Unreleased]

### Changed

- **The product is now Aivyx PA. BREAKING: binary, config file, and data
  paths all renamed, with no automatic migration.** The CLI binary is
  `aivyx-pa` (previously `aivyx`); the config file the CLI looks for by
  default is `aivyx-pa.toml` (previously `aivyx.toml`), and its
  passphrase section is now `[aivyx_pa]` (previously `[aivyx]`); the
  config/data directories moved from `~/.config/aivyx/` and
  `~/.local/share/aivyx/` to `~/.config/aivyx-pa/` and
  `~/.local/share/aivyx-pa/`; the `AIVYX_*` environment-variable prefix
  is now `AIVYX_PA_*`; and the OS-keyring service name used to store the
  passphrase moved from `aivyx` to `aivyx-pa`. Nothing above is copied,
  renamed, or migrated automatically — an existing install that wants to
  keep its data has to move it by hand before running the new binary:

  ```sh
  mv ~/.config/aivyx ~/.config/aivyx-pa
  mv ~/.local/share/aivyx ~/.local/share/aivyx-pa
  mv aivyx.toml aivyx-pa.toml   # if using a CWD-relative config
  ```

  then edit the moved config's `[aivyx]` section header to `[aivyx_pa]`,
  rename any `AIVYX_*` environment variables it relies on to
  `AIVYX_PA_*`, re-enter the passphrase once if it was stored in the OS
  keyring (a fresh `aivyx-pa` lookup under the new service name won't
  find an entry saved under the old one), and update the binary name in
  any systemd/launchd unit, shell alias, or script that invokes it. The
  passphrase itself and the encrypted store's contents are unaffected —
  this is a path/name rename only, not a re-encryption.

### Added

- **A shared default skill library (Aivyx-Skills Part 3) — two new
  read-only tools plus a `## Default skills` system-prompt section.**
  `skill_defaults.list` / `skill_defaults.read` expose the 5 skills
  bundled in the new `aivyx-skills` crate (e.g. `systematic-debugging`,
  `writing-plans`) — the same small, shared `SKILL.md`-format library
  crate adopted by `aivyx-coder`, distinct from the agent's own
  persona-chain `LearnedSkill`/`skills.*` system. Both tools are
  Trusted-tier and included in the zero-config default role's
  capability floor (read-only over a server-side-fixed set, no
  model-supplied path). A new, optional `[skill_defaults]` config
  section lets an operator layer `project_dir`/`user_dir` overlay
  directories on top of the bundled defaults (project overrides user
  overrides bundled, per same-named skill) — see `examples/aivyx-pa.toml`
  for the required `<dir>/<skill-name>/SKILL.md` directory shape.
- **`aivyx federation yubikey-init` — hardware-backed federation identity
  provisioning (Chapter Passport, Task 8).** Discovers a connected
  YubiKey's OpenPGP card, refuses to proceed on a still-factory-default
  PIN, generates an on-card Ed25519 keypair in the Signature slot, sets
  its touch-policy to fixed (physical touch required on every future
  signature), and writes the resulting `{instance_id, card_serial,
  public_key_base64}` binding record to an operator-chosen path. Off by
  default (`cargo build -p aivyx-cli --features yubikey`) — the
  underlying `aivyx-yubi`/`aivyx-federation` dependencies transitively
  need `libpcsclite`/`pcscd` at build and run time; without the feature
  the subcommand still parses but refuses with a clear rebuild message.
  See `docs/INSTALL.md`'s "Hardware-backed federation identity (YubiKey)"
  section and `docs/FEDERATION.md`.
- **CI now fails fast, by name, if a workspace git dependency ever
  silently reverts to private.** `scripts/check-git-deps-public.sh`
  (Phase 192) is wired into `quality-gate.yml` as an early step —
  closes Chapter N (Release & Distribution Integrity). See
  `docs/archive/phases/PHASE_197.md`.

### Security

- **Webhook-triggered turns now require authentication and run at
  `Untrusted`, not `Trusted`.** Part of the 2026-09-16 security-audit
  fixes: previously any local process (or any webpage the operator
  merely visited, via a CORS "simple request") could fire an agent turn
  through the webhook listener with no shared secret, and that turn ran
  at the same trust tier as a local CLI session. Every webhook now
  carries a `secret` (generated once at creation, shown only then) that
  callers must present as `Authorization: Bearer <secret>`, and the
  triggered turn runs at `Untrusted`. **If an existing webhook's prompt
  relies on tools gated above `Untrusted`** (e.g. `fs.write`,
  `shell.exec`), those tool calls will now fail their capability check
  after upgrading — grant the narrower scope the webhook's task actually
  needs via that role's `capability_scopes`, or rework the prompt,
  rather than restoring blanket trust. Existing webhook records created
  before this change have no `secret` on disk and, by design, an unset
  secret never authorizes — re-create any such webhook to get a fresh
  secret and keep it callable.

### Fixed

- **Telegram/Discord/Slack's standalone (`--no-daemon`) session paths now
  honor the operator's `[agent]` config.** Previously these three
  in-process-fallback paths constructed every agent via
  `TurnSafety::default()`, silently ignoring `turn_timeout_secs`,
  `cycle_detection`, `injection_scan_enabled`, and `injection_scan_exempt`
  — including a configured `injection_scan_enabled = false`, which had no
  effect on these paths. They now route through `TurnSafety::interactive(...)`
  like every other agent construction site. Daemon-mode dispatch was
  never affected (it never constructs agents client-side).
- **First-launch safety: running `aivyx` before `aivyx init` on a genuinely
  fresh machine no longer silently creates a permanent, unconfirmed
  encrypted store.** Previously a bare `aivyx` invocation with no config
  anywhere would create the sandbox/storage directories, prompt once
  (unconfirmed) for a brand-new passphrase, and open a real encrypted
  store — all before config validation ever ran — so a later, real
  `aivyx init` run could write a config pointing at that same orphaned
  store under a different passphrase, failing to decrypt it with no
  indication why. `run()` now validates first when no store exists yet,
  offering to run the setup wizard inline (interactive) or failing
  immediately with a clear `aivyx init` pointer (non-interactive) —
  zero side effects either way. `aivyx init` also now warns and requires
  confirmation before writing a config that points at a storage path
  where a store already exists, and the first-time interactive
  passphrase prompt for a brand-new store now requires confirm-reentry
  (matching `aivyx keyring set`'s existing shape) instead of accepting
  an unconfirmed, unrecoverable-if-mistyped entry.

## [0.9.4] — 2026-09-05

**Milestone: the first real, working GitHub Release since `v0.8.3`.**
Ships Chapter N's root-cause and release-cutting work (Phases 192-193,
opened this same day after discovering `v0.9.0` never actually
published — see `docs/archive/phases/PHASE_192.md` and `PHASE_193.md`;
the chapter's closing phase, 197, is the `[Unreleased]` entry above)
and closes Chapter Picket (the prompt-injection tripwire,
`PHASE_194.md`-`PHASE_196.md`) in one release.

### Added

- **Active prompt-injection tripwire (Chapter Picket).**
  `aivyx-injection-guard`, a new shared crate extracted from
  `aivyx-coder`'s existing production tripwire, scans untrusted tool
  output for known injection phrasings and escalates the turn via the
  existing `TurnOutcome::Escalated` → `ApprovalGate`/`HeadlessRefusal`
  path — a side-channel signal checked after the tool's real outcome
  is recorded, so a mutating tool's audit trail always reflects what
  actually executed. Complements, doesn't replace, Bulwark's existing
  passive `fence_untrusted_output` labeling. See `docs/THREAT_MODEL.md`
  and `PHASE_196.md`.

### Fixed

- **`aivyx-confine`'s `require_enforcement` check incorrectly required
  `RulesetStatus::FullyEnforced`.** GitHub's own hosted-runner kernel
  only ever achieves `PartiallyEnforced` at the crate's hardcoded
  Landlock ABI version, which made every release build fail its own
  quality gate before reaching the real cross-compile work. Now only
  `NotEnforced` fails closed.
- **`aivyx-confine`'s syscall blocklist referenced
  `libc::SYS_kexec_file_load`, absent from libc's musl bindings for
  aarch64/riscv64,** breaking the `aarch64-unknown-linux-musl` release
  target's build.
- **The WSL release workflow's retry budget for pulling the appliance
  base image (10 minutes) was too short for real-world timing;**
  raised to 30 minutes to match the sibling wait-for-release retry
  loop in the same workflow.
- **The three private git dependencies (`aivyx-confine`,
  `aivyx-checkpoint`, `aivyx-kvcache`) that silently broke every
  `v0.9.0` release workflow — and any outside contributor's
  build-from-source path — are now public** (the CI regression guard
  that enforces this going forward ships in `[Unreleased]` above,
  Phase 197).

## [0.9.0] — 2026-09-03

**Milestone: the Interface Polish phase is complete — v0.9.0 cuts as
the capstone.** `docs/V09_PLAN.md`'s own stated rule ("v0.9.0 cuts as
the capstone when the polish backlog is empty") is now satisfied: all
8 rows of that plan are done. This release folds in the two months of
continuous work since `v0.8.3` that shipped without its own version
cut — **Gatehouse** (Studio remote auth: the refuse-to-bind interlock,
the `web_ui_insecure_no_auth` escape hatch, first-boot token
generation; `docs/GATEHOUSE.md`), **Freight** (signed pack bundles —
format core, the `aivyx pack` CLI, the Kitchen worked example;
`docs/FREIGHT.md`), the full **Vitrine** operator walkthrough across
all 15 Studio screens + the TUI + the desktop shell
(`docs/VITRINE.md`), and the resulting 8-sub-project polish backlog
(`docs/POLISH_WAVES.md`): a small backlog sweep, `/classic` retirement,
Repertoire completions, agent turn-quality fixes, Missions polish, a
UI modernization pass, config-write surface-area completions (memory
profile / embedding / proactive settings, reflection-schedule CRUD),
and tool/server call-stat observability. Also ships as part of this
release: **Chapter Mission Control** (live LEAD/specialist mission
graph, click-to-drill-in, gate approve/reject, abort, pause/resume —
see Added below) and **Phase 185's Terminal TUI foundation**
(`ratatui`/`crossterm` replacing the line-based REPL, plus Missions /
Dashboard / Audit / Tools panels; `docs/ROADMAP.md`'s Chapter I). Full
phase-by-phase detail lives in those docs, not reconstructed here.

### Added

- **KV-cache persistence for `llama-server` users (`provider =
  "llama_cpp"`).** A fresh daemon process's first turn on a system
  prompt + tool-def combination it has seen before can now skip
  re-prefilling that stable prefix, restoring it from disk instead —
  requires `llama-server` started with `--slot-save-path` pointed at
  `<data_local_dir>/aivyx/kvcache/slots`; see `docs/INSTALL.md`'s
  "KV-cache persistence" section. No effect on Ollama or Jan.
- **Pause and resume a running team mission (Chapter Mission Control).**
  `aivyx team pause <id>` requests a graceful pause at the mission's next
  wave boundary — unlike abort, this is resumable. `aivyx team resume
  <id>` continues a paused mission from its preserved checkpoint.
- **A Mission Control screen (`aivyx-web`, Chapter Mission Control).** A
  new "Mission Control" nav destination shows one active mission's
  LEAD/specialist graph live — click a specialist to drill in (current
  step, declared capability scopes, the NT-02 "inert" hint) — plus the
  first UI controls for gate approve/reject, abort, and the new
  pause/resume.

### Changed

- **`shell.exec` and the `git.rs` tools (`git.status`/`git.diff`/
  `git.commit`) now confine every spawned command with Landlock +
  seccomp-bpf, on by default.** Previously, at `[access] level =
  "sandbox"`, `shell.exec` could read and execute anywhere on disk the
  operator's OS permissions allowed, subject only to the string-level
  `[access] guard_sensitive_paths` check — a command that got past that
  guard had the full authority of the daemon's own OS user. Every
  spawned child now additionally runs under kernel-level confinement
  (`aivyx-confine`): filesystem writes are scoped to the tool's own
  working-directory root (`shell.exec`'s `cwd_root` / the git repo being
  operated on) plus a fixed system/toolchain read list, and a
  seccomp-bpf denylist blocks a set of syscalls with no legitimate use
  in a coding agent's spawned commands (`ptrace`, `mount`, `bpf`,
  `perf_event_open`, and others). New `[confine] require_enforcement`
  config key (default `true`) controls what happens if Landlock itself
  fails to establish a ruleset on a given machine: fail-closed (refuse
  to run the command, the default) or fail-open (run unconfined, for
  operators on a kernel without Landlock support who still want
  `shell.exec` usable). It does not provide a way to turn confinement
  off when Landlock is working normally. Linux only — non-Linux builds
  are unaffected (no confinement, same as before). See
  `docs/THREAT_MODEL.md` §5.6/§6 and `docs/INSTALL.md`'s `[confine]`
  section.

## [0.8.3] — 2026-07-08

### Fixed

- **Studio footer showed the wrong version number.** The WASM bundle
  released in 0.8.2 was built before that version bump, so
  `env!("CARGO_PKG_VERSION")` baked "v0.8.1" into the Studio's status bar
  even though the daemon binary itself correctly reported 0.8.2. No
  functional change — the bundle is rebuilt from the same 0.8.2 source,
  now after the version bump.

## [0.8.2] — 2026-07-07

### Added

- **Studio Gallery — view images your assistant generates.** A new screen
  renders images produced through a `comfyui`-named `[[mcp_server]]`
  (e.g. the community `comfyui-mcp-server` bridge in front of a local
  ComfyUI instance). The daemon reads ComfyUI's own `/history` API
  directly and serves bytes to the browser through a new authenticated
  `/studio-asset` proxy route — no MCP-call plumbing involved, and the
  browser never needs to reach ComfyUI's own (loopback-only) port
  directly. Each image shows the prompt that produced it (traced through
  the submitted workflow graph, not guessed by node order) and when it
  was made, newest first, with a click-to-enlarge view. The screen is a
  no-op empty state when no `comfyui` server is configured.

## [0.8.1] — 2026-07-05

**The Vitrine harvest: one day of live operator walkthrough, fifteen-plus
bugs found, nearly all fixed same-day and re-proven on the rig.** The
first five walkthrough sections (Gatehouse baptism, Command Center, Chat,
Missions, Memory, Skills) drove every fix in this release; the standout
theme is honesty under pressure — silent failures now speak, stochastic
judges get deterministic backstops, and skills finally get used.

### Added

- **Conversation-history replay (Chapter Thread).** Interactive-session
  turns now replay the session's recent user/assistant messages as real
  conversation history, so follow-ups like "did you find the correct
  code?" resolve against what was actually said. Default-on
  (`[agent] conversation_history_turns = 8`; `0` restores fresh-context
  turns exactly); trigger-fired turns are structurally unaffected, and
  durable memory remains the persistence layer.
- **Skill trigger injection — skills are finally used.** Each turn, the
  best trigger-matching approved skill's procedure is injected into
  context alongside memory recall (embedding cosine with live-calibrated
  0.50 floor; token-overlap fallback for embedding-free installs). Local
  models never took the `skills.list`/`skills.invoke` indirection, so a
  fresh agent's skills were dead weight. Verified end-to-end: teach →
  use on the next turn with no restart; update → use;
  `[skills] trigger_injection = false` opts out.
- **Deterministic identifier backstop in the completion judge.** When a
  goal names uppercase identifiers (ICAO codes, tickers) and the
  majority appear nowhere in the deliverable or evidence, the verdict
  rejects before any LLM opinion — after a live mission's judge
  hallucinated a PASS over a brief about entirely different airports.
- **Team specialists can use the operator's MCP servers.** The
  researcher/analyst/verifier roles now receive qualified per-server
  `mcp.call` grants and see bridged MCP tools; previously the whole team
  was structurally MCP-blind and approximated one weather-tool call with
  sixteen raw page fetches.

### Fixed

- **Web search reports backend refusals instead of silently returning
  nothing.** DuckDuckGo answers bot-flagged traffic with HTTP 202 + a
  challenge page; the zero-config backend parsed that to an empty result
  set, and the model either went mute or invented answers.
- **Reflection proposals were scope-dead on a clean install** — the
  default capability floor granted neither `reflection.propose` nor
  `persona.propose`, so every organic proposal died with a denial.
- **Reflection no longer fires on every daemon restart** (the last-fired
  anchor was epoch, not boot).
- **Clean systemd stops no longer report "unclean shutdown"** — the
  daemon now handles SIGTERM like Ctrl-C.
- **The planner's context budget honors an explicit `[ollama] num_ctx`**
  instead of an 8k class default, and **giant tool results are capped**
  (~half the context window, with an explicit truncation marker) — one
  raw page fetch used to become an un-prunable 30k-token message that
  pushed the request past the real window and silently truncated the
  system prompt server-side.
- **Pruning can no longer discard the turn's own question.** The task
  message is pinned through context pruning; a fat tool turn used to
  lose its task and reset to a greeting mid-mission.
- **Mission retries are bounded again.** The Reprise attempt counter is
  persisted on the mission record; an approval gate used to reset a
  driver-local counter on every "Approve", producing an infinite
  reject-retry-gate loop.
- **The trigger-injection embedding cache keys on trigger text**, not
  `name@version` — a Tutor `skills update` preserves the version and
  would have served the stale trigger embedding forever.
- **`capture-note`'s starter trigger is written in instance nouns**
  (dates, expiries, favourites) so real volunteered facts actually match
  it; with injection live, a stated fact is now saved to memory and
  confirmed in one line.

## [0.8.0] — 2026-07-04

**Milestone: the self-learning arc is real, live-proven, and honest.**
v0.8.0 marks core refinement converged: across the v0.7.38–v0.7.41 run and
this release, the complete self-learning loop was exercised live on real
hardware — skills invoked, effectiveness measured with honest signals,
underperformers refined, new skills authored from the agent's own
consolidated knowledge, every change gated by operator governance on the
signed persona chain, and the restraint properties (no churn, no
re-nagging, no twin proposals) verified under soak. This begins the run-up
to the v0.9 interface-polish phase.

### Fixed

- **Cron schedules now fire on the operator's local wall clock.** The init
  template always said "local time," but the engine evaluated cron fields
  in UTC — on any non-UTC host every routine fired hours off intent (a
  "nightly" reflection at 10 a.m. local). Both schedulers now share one
  local-time evaluation. One-time upgrade effect: a schedule whose
  local-time tick already passed today fires a single catch-up.
- **Skill authoring no longer proposes twin skills from near-duplicate
  topic names** ("triathlon" and "triathlon-basic"): candidacy skips a
  topic whose normalized name is contained by — or contains — any covered
  skill or prior authoring proposal. Synonym-named twins remain the
  operator's call in the proposal inbox, by design.
- **The memory conflict detector no longer flags change-over-time
  sequences as contradictions** ("file X present" then "file X removed" is
  the world changing, not a conflict) — the recurring false-positive class
  produced by daily environment observations.
- **The agent can now see its own routines**: `schedule.list` joins the
  default-role capability floor (read-only, like `skills.list`). The
  schedule *write* tools deliberately remain outside the floor pending an
  autonomy-gating decision.

## [0.7.41] — 2026-07-04

**Chapter Wire** — headless multi-turn sessions. Pipe newline-delimited
turns into `aivyx --headless` and they run as consecutive turns of one
daemon session, making session-scoped behavior (session-partitioned
memory, recall's conversation window, consecutive-turn signals like the
correction detector) reachable from a terminal, a cron line, or a test
harness for the first time.

### Added

- **`printf "first\nsecond\n" | aivyx --headless`** — bare `--headless`
  with piped stdin reads each non-empty line as one turn of a single
  session. Fail-fast for batch callers: the stream stops at the first
  non-completed turn and exits with the existing headless code (`3`
  gate-refusal, `1` other failure); all turns completed exits `0`. The
  one-shot `aivyx --headless "<task>"` form is unchanged, and bare
  `--headless` on an interactive terminal errors with a hint to pipe.
  Every turn keeps the unattended posture (gates refuse, never park).

### Notes

- A session carries session-partitioned memory, the conversation-window
  relevance feed, and turn adjacency — turns are deliberately
  fresh-context, with continuity flowing through memory and recall
  rather than transcript replay. `docs/WIRE.md` records the distinction.

## [0.7.40] — 2026-07-04

**Chapter Strop** — skill effectiveness now measures how well a skill
*serves*, not how often it runs, so the self-learning refinement loop fires
organically at its default thresholds. Live-verified on the test rig: one
hollow skill turn (result claimed, tool never called) was enough for the
agent to propose a sharper version of that skill — with no healthy skill
swept up. Refinement remains propose-only and operator-governed.

### Changed

- **A completed turn is no longer an automatic +1 in the per-skill
  effectiveness ledger.** The fold now reads Candor's verdict: a turn that
  completed but carries an unfulfilled-claim annotation (the skill's result
  was claimed while the fulfilling tool was never called — the dominant
  local-model failure) folds negative. Failed/looping turns stay negative;
  clean completions stay positive. Ledger format and thresholds unchanged.
- **Corrected skill turns count against the skill.** On the reflection
  cadence, a completed skill turn the operator immediately reworked (the
  correction ledger's definition) retro-folds negative — the
  correction cross-reference the original Whetstone design specified. Each
  correction folds exactly once across the repeating lookback windows via a
  per-schedule watermark keyed on the follow-up turn.

## [0.7.39] — 2026-07-04

The self-learning loops now actually run for a fresh install. A live dogfood
of the skill self-learning arc (Whetstone refinement + Praxis authoring)
found five gaps that together kept it structurally dark; all are fixed and
each was verified live on the test rig — including the first-ever
agent-refined skill and first-ever knowledge-authored skill landing on a
real persona chain under operator governance.

### Fixed

- **The default role can now use its own skills.** `skills.list` and
  `skills.invoke` were never granted in the default-role capability floor, so
  every fresh install's agent was denied on calling them — no skill invocation
  could be audited and the skill-effectiveness ledger could never accumulate.
  Both scopes (read-only over the agent's own operator-approved skill set) are
  now in the floor; `skills.propose` deliberately stays out.
- **Fresh installs get a real reflection schedule.** `aivyx init` never wrote a
  `[[reflection_schedule]]`, so the whole reflection-pass family (persona
  proposals, consolidation, skill refinement, skill authoring) never fired —
  the starter routine *named* "nightly-reflection" is a plain prompt turn, not
  the reflection scheduler. init now plants a daily, skip-when-idle reflection
  schedule (enabled on local providers, written-disabled on cloud).
- **Skill authoring can actually find candidates.** Praxis gated candidacy on
  the wiki topic name appearing verbatim as a knowledge-graph subject — but
  topics are slugs while graph subjects are LLM-extracted phrases, so the
  lookup never matched and the pass was silently starved. Candidacy now joins
  on normalized word overlap, verified against the exact shapes observed live.
- **The loop's bookkeeping no longer pollutes the knowledge base.** The
  `loop:progress` progress log had leaked a knowledge-wiki page;
  `is_internal_topic` now covers the loop's reserved prefix and the wiki sweep
  purges any already-leaked internal pages.
- **Skill authoring skips the agent's own journal.** The authoring pass could
  synthesize a "skill" from a scheduled routine's own memory writes (the agent
  talking to itself). Configured routine and reflection schedule names are now
  excluded authoring topics.

## [0.7.38] — 2026-07-04

Dogfood-surfaced reliability fixes from the 2026-07-04 single-agent loop run,
each live-verified on the test rig before release.

### Fixed

- **A planner-named unknown specialist no longer kills the story.** When the
  LLM planner reaches for a generic role word that isn't in the roster
  ("Operations", "QA-Engineer"), `SpecialistPool::resolve` now maps the word to
  a capability and routes to the best-fit, least-privilege roster member instead
  of hard-failing the whole delegation. Truly unmatchable names still error, and
  naming the lead is still rejected; Keystone's artifact gate keeps a bad
  best-effort attempt honest.
- **`aivyx loop add` no longer trips over the 200-character title cap.** A
  second positional argument is now accepted as the story body
  (`aivyx loop add <title> [body]`), and an over-long title auto-splits at the
  last word boundary that fits — the overflow moves into the body with a printed
  note instead of a bare error. The cap error itself now says where the detail
  belongs.

### Changed

- **`memory.write` supersedes near-duplicate rewrites.** A refinement of an
  already-stored fact (same content with a citation appended, a punctuation or
  case tweak) now replaces the older entry instead of piling up beside it as
  recall noise. Thresholds are deliberately conservative — distinct facts that
  merely share words ("…Tuesday…" vs "…Thursday…") stay side by side — and the
  new entry always lands before any older one is removed, so a mid-way failure
  leaves noise, never loss. Superseded sequence numbers are reported in the tool
  output.

### Security

- Bumped the transitive `cmov` dependency 0.5.3 → 0.5.4 (Dependabot alert:
  wrong Cmov/CmovEq results on aarch64 when register high bits are set; sits
  under the audit chain's HMAC stack).

## [0.7.37] — 2026-07-04

### Added

- **A WSL distribution now ships with each release (`Aivyx.wsl`).** Windows
  users get a one-command-import WSL2 distro with the daemon and all tool
  binaries pre-installed — no in-distro install step. It's the cheapest real
  "Aivyx on Windows" path: it sidesteps the deferred native Windows port because
  the daemon's Unix-domain-socket IPC and `0600` secret-at-rest posture both work
  unchanged inside WSL2's real Linux kernel. Built by reusing the Harbor
  appliance image (no duplicate compile): `Dockerfile.wsl` layers WSL config
  (default user, `/etc/wsl.conf`, `/etc/wsl-distribution.conf`, a first-launch
  OOBE script) and `.github/workflows/wsl-release.yml` exports the root
  filesystem to the release. Install with `wsl --install --from-file Aivyx.wsl`
  (WSL 2.4.4+) or `wsl --import`; see docs/INSTALL.md "Windows".

## [0.7.36] — 2026-07-04

### Added

- **Specialist tool calls are now traced in the mission journal (Chapter
  Spyglass).** A Nonagon specialist's sub-turns previously emitted nothing, so
  you could not see whether (say) the writer actually called `workspace.write` —
  the exact blind spot that made the recent team capability bugs hard to
  diagnose. Each specialist channel now logs its tool activity, labelled by
  role: `aivyx team: [writer] → workspace.write` / `← workspace.write —
  completed`. Pure observability (logging only; no behavioural change).

### Changed

- **The analyst can now fetch data itself.** The Analyst role gained `web.fetch`
  (+ the `net.fetch` capability) alongside `web.search`, so it can pull
  datasets, APIs, and reference pages directly rather than relying on search
  snippets.
- **The `ops` role is now a verify-by-execution `verifier` (Verifier/QA).** The
  old Operations role (`shell.exec` + `fs.read`) was a strict capability subset
  of the coder. It is recast as a Verifier that proves a deliverable works by
  *executing* it — running the tests, reproducing the claim, checking data
  against its source — and reports PASS/FAIL with evidence without modifying the
  work (same `shell.exec` + `fs.read`, plus `net.fetch` to reproduce against
  remote sources). This fills the gap left by the read-only reviewer (which
  cannot run anything) while keeping the team a nine-role Nonagon.

## [0.7.35] — 2026-07-04

### Fixed

- **A team mission retries once when it produced nothing (Chapter Reprise).**
  A specialist sometimes claims to have written the deliverable but doesn't
  (model-ceiling variance — a fresh attempt often succeeds where the first
  missed). When the artifact gate (Keystone) finds no deliverable, the mission
  now re-drives once from a cleared checkpoint before giving up, converting many
  such misses into real completions instead of honest-but-empty rejections.
  Bounded (one retry) so a genuinely impossible mission can't loop; only when
  verification is on.
- **A reviewer gate no longer kills a whole team mission (Chapter Ombudsman).**
  An in-DAG *auto* gate would abort the entire mission on any reviewer `FAIL` —
  so a research→review→write plan died at the review step, before the writer
  ever produced the deliverable. An auto gate is now **advisory**: its verdict
  is recorded and fed to downstream steps as context (a "FAIL: add X" review
  makes the next step better), and the mission continues; final quality is
  enforced end-to-end by the artifact gate (Keystone), and a **human** gate
  still blocks for operator approval. Live-verified: a multi-step mission now
  runs its write step instead of aborting at the review.
- **Team specialists can actually use their tools (Chapter Ensemble).** The root
  cause of "team missions do nothing": a specialist's caps are `declared ∩ what
  the lead grants`, but the coordinator lead held only `[memory, team.delegate]`
  and the roster declares *bare* scopes (`fs.write`, `net.fetch`) that can't
  match the daemon's *qualified* floor (`fs.write:<root>/**`, `net.fetch:<url>`)
  — so every specialist was attenuated to memory-only and couldn't write files,
  fetch, or run commands. The mission lead now holds the daemon's real authority
  and each specialist inherits the lead's floor scopes for the bases its role
  declares. Stays least-privilege (a reviewer remains read-only; a writer gets
  no network/shell) and `⊆ daemon floor`. Live-verified: a writer now writes its
  file to the workspace and the mission completes `Done`. (Supersedes the
  workspace-only Anchorage fix with one general mechanism.)
- **Team missions left `Executing` on a daemon restart no longer become
  permanent zombies (Chapter Reckon).** There is no resume machinery for team
  missions, so a mission interrupted mid-flight would sit `Executing` forever in
  `team list`. On reload, an interrupted `Executing` mission is now reconciled to
  `Halted` with a truthful reason ("interrupted by a daemon restart") and
  persisted; `AwaitingApproval` (a real operator pause) is left untouched.
- **The planner routes save/create-file steps to a specialist that can actually
  write (Chapter Handoff).** The decomposition prompt hid specialist
  capabilities, so the coordinator could assign a `save_file` step to a
  read-only role (e.g. `Operations`, which has `shell.exec, fs.read` but no
  `fs.write`) — a step that then cannot produce the deliverable. The planning
  roster now tags each specialist `[writes files]` / `[writes memory]` from its
  scopes, with a rule that persist steps must go to a write-capable specialist.

### Changed

- **A team mission is `Done` only if its deliverable exists (Chapter
  Keystone).** Live Nonagon dogfooding found the team subsystem's sharpest bug:
  a mission ran every step to "done" — including a `create_file` step — and
  reported `phase: done`, but the file existed nowhere. Steps completed when the
  specialist *sub-turn returned*, with no grounding that the deliverable was
  actually produced. Now, when a mission runs to completion, it is graded
  against its goal — grounded on the workspace/memory artifacts it produced
  (the same `CompletionJudge`) — and a mission that claims done but produced
  nothing is flipped to `Rejected` with the verdict on the audit chain. Opt-in
  via `TeamRunDeps.verify_missions` (the daemon enables it; default off ⇒
  pre-Keystone behavior); best-effort and fails-open (no grounding / LLM error
  ⇒ keeps `Done`). Covers both direct `team start` and loop/Foreman missions.

## [0.7.34] — 2026-07-03

### Changed

- **Accord detector precision: fewer borderline false positives.** The Soul-
  contradiction judge's prompt was teaching itself to over-flag — it used
  "warm" vs "be candid" as a *positive* example, so it intermittently flagged
  complementary traits on a perfectly coherent Soul. Rewrote the prompt around a
  sharp mutual-exclusivity test ("is there NO situation in which both can be
  honored at once?") with explicit NOT-a-contradiction cases (complementary
  traits, default+exception, broad tone next to a specific rule). Live-verified
  on the rig: the real coherent Soul went from intermittent 1–2 false positives
  to 8/8 clean, while a genuine contradiction (one-line vs multi-paragraph) is
  still caught — recall preserved.

## [0.7.33] — 2026-07-03

### Added

- **Accord skill-layer coherence: contradiction detection across learned
  skills.** The contradiction pass now also considers the agent's learned
  *skills* (shown to the judge as `skill "X": when <trigger> → <procedure>`), so
  two skills that give opposite instructions for the same situation — or a skill
  whose procedure contradicts a facet or operator constraint — are flagged by
  `aivyx persona conflicts` alongside facet conflicts. Resolution removes the
  losing skill BY NAME (reusing the Repertoire operator-forget primitive),
  reversible via `aivyx persona revert`; dismiss ("keep both") works too.
  Live-verified: seeded two opposite reply-style skills → detected → resolved.
- **Accord prevent-at-write: the Soul can't accrete a contradiction (coherence
  gate at approval).** Beyond finding contradictions after the fact, approving a
  persona proposal now runs a targeted coherence check first: if the proposed
  facet would contradict an existing facet (or an operator Profile constraint),
  the approval is refused with the specific conflict named — so the operator
  rejects it, resolves the existing facet, or accepts the tension. The override
  reuses Accord's dismiss ("keep both"): `aivyx persona dismiss <id>` then
  re-approve, so a fuzzy-detector false positive is never an unescapable
  lockout. Best-effort (no LLM ⇒ no gate); only gates operator approvals of new
  list facets. Reuses the live-verified detector; the dismiss set is the escape
  hatch.
- **Accord "keep both": dismiss a Soul contradiction (false positive).** The
  contradiction detector is a fuzzy LLM pass, so it can flag a *legitimate*
  nuance ("concise by default" + "detailed when asked") — which `persona
  conflicts` would otherwise re-surface every run. `aivyx persona dismiss <id>`
  now records the pair's stable id (in the existing encrypted dismissals domain,
  namespaced `soul:` so it can't collide with a memory dismissal) and future
  detection passes suppress it; nothing is removed from the Soul. Mirrors
  Chapter Concord's `memory dismiss`. New `DismissSoulConflict` IPC verb.

## [0.7.32] — 2026-07-03

### Added

- **Soul-coherence: contradiction detection over the Persona (Chapter Accord).**
  The identity-stack sibling of Chapter Concord (which does this for memory).
  The Soul accretes reflection-learned facets; the lifecycle layer merges
  near-*duplicates* and decays the *unreinforced*, but nothing flagged two
  approved facets that flatly *contradict* — a seeded "communicate concisely"
  living next to a later "give thorough, detailed explanations", both injected
  every turn — nor a learned facet drifting against an operator Profile
  constraint ("be candid, never flatter me" vs a learned "warm and effusive").
  New `aivyx persona conflicts` runs an on-demand LLM detection pass (zero
  background cost, no config) and `aivyx persona resolve <id> --remove <a|b>`
  removes the losing facet via a `RemoveList` persona delta (operator-authored,
  reversible via `aivyx persona revert`). Operator Profile constraints are
  immutable — flagged, never removed. New wasm-clean `SoulConflict` IPC type +
  `GetSoulConflicts` / `ResolveSoulConflict` verbs (Studio can consume later).

## [0.7.31] — 2026-07-02

### Changed

- **The autonomous loop now verifies-and-closes a finished story the agent
  forgot to complete (Chapter Capstone).** A recurring dogfood finding: small
  local models often do the work but never call `loop.complete` (or report it in
  chat), so a genuinely-finished story stays pending until the stall breaker
  ends the run. When `[loop] verify_completion` is on, at each iteration's end
  the driver judges the still-pending story it handed out against its own
  acceptance criteria — grounded on the memory/workspace artifacts the turn
  produced — and marks it done iff the judge passes. Genuinely-incomplete work
  is left pending (the grounded judge rejects it, e.g. "only 1 of 2 requested
  items"). Reuses the existing completion judge; no new config.

## [0.7.30] — 2026-07-02

### Security

- **The web UI can now require an auth token (Chapter Postern).** The Studio's
  `/ws` WebSocket is its control plane — agent turns, config writes, and memory
  reads all flow over it — and exposing it off-host (`web_ui_host = "0.0.0.0"`,
  the Docker appliance) previously left it unauthenticated. Set `[daemon]
  web_ui_auth_token = "<opaque>"` to require a shared secret: browsers are
  prompted via HTTP Basic (the token is the password) and a cookie is planted
  that gates the `/ws` upgrade; non-browser clients send `Authorization:
  Bearer`. Default-off (localhost posture byte-identical), with a loud daemon
  warning when bound off-host without a token. Not a TLS substitute — terminate
  TLS at a proxy for remote access.

## [0.7.29] — 2026-07-02

### Security

- **`shell.exec` now honors the sensitive-path guards (Ward/Portcullis).**
  Previously the sensitive-read and persistence-write guards protected only the
  `fs.*` tools, so a shell command routed around them — `cat ~/.ssh/id_rsa`,
  `echo … >> ~/.bashrc`. `shell.exec` now scans the command text and refuses a
  command that references a protected location (secret dirs/files, cloud creds,
  or persistence targets like `.bashrc` / `authorized_keys` / `crontab`) before
  `sh` runs. Uses the same `[access] allow_sensitive_paths` opt-in. Best-effort
  by nature (obfuscated paths can still slip through — the docs continue to
  point to OS-level isolation for hard guarantees); disabled-by-default so
  behavior is byte-identical until the guard is configured on.

## [0.7.28] — 2026-07-02

### Changed

- **Loop completion verdicts now ground on FILE artifacts, not just memory.**
  The acceptance judge that gates `loop.complete` (and delegated team missions)
  reads a bounded snapshot of the most recent workspace files alongside recent
  memory, and treats both as ground truth. A file-producing task is graded on
  the file that actually exists rather than on how tersely the agent phrased its
  summary — closing the documented #17b/#17d residual (previously memory-only).

## [0.7.27] — 2026-07-02

### Security

Final hardening pass of the security thread — extending the existing guards to
the last uncovered surfaces:

- **Prompt-injection fencing now covers `fs.read` and every tool-process
  integration (Bulwark extension).** A file's contents (attacker-supplied,
  downloaded, or in a broad-access location) and the output of Gmail / Calendar
  / Contacts / Drive / Obsidian / the web-search toolkit are fenced as untrusted
  data — never instructions. Verified: the agent describes an injection in a
  file rather than obeying it.
- **`net.dns` honors the egress policy (closes a DNS-exfil channel).** A DNS
  lookup is itself an exfiltration channel (`<secret>.attacker.com` leaks to the
  attacker's nameserver); `net.dns` now applies the same
  `[access] allow_egress_hosts` allow-list + private-host block as the web
  tools, so an allow-listed deployment can't be DNS-tunneled.

## [0.7.26] — 2026-07-02

### Security

- **Master passphrase in the OS keyring (Chapter Keyring).** For interactive /
  desktop use, store the master passphrase in the OS credential store (Secret
  Service / Keychain / Credential Manager) instead of the `AIVYX_PASSPHRASE`
  env var or `[aivyx] passphrase` TOML (both plaintext). `aivyx keyring set` /
  `clear` / `status`; the daemon reads it automatically (env/TOML still take
  precedence; the keyring is tried before the interactive prompt; an
  unavailable/locked keyring falls through). The headless systemd service keeps
  using the `0600 daemon.env` file (no session Secret Service there).
- **Fixed a PDF denial-of-service (RUSTSEC-2026-0187).** The `data.pdf` reader
  pulled `lopdf 0.34`, where a crafted ~21 KB PDF with deeply nested arrays
  triggers unbounded-recursion stack overflow (SIGABRT) — crashing the daemon
  on any turn that reads an untrusted PDF. Upgraded `pdf-extract` → 0.12 and
  `lopdf` → 0.42 (the patched line); reader behavior unchanged.

## [0.7.25] — 2026-07-02

### Security

Continuing the privacy-first hardening pass — two more layers, both
live-verified on the dogfood rig:

- **Sensitive-path write guard (Portcullis).** The symmetric completion of
  Ward: `fs.write` now hard-refuses writes to secret *and* persistence
  locations — `~/.ssh/authorized_keys`, shell rc files
  (`.bashrc`/`.zshrc`/`.profile`), `~/.config/autostart` & `systemd`, `cron.*`,
  git hooks — even inside the sandbox and even at `full`. This closes the
  backdoor/persistence vector `confirm_destructive` only *soft*-gated (it's
  self-confirmable; this is a hard refusal). Same `[access]
  allow_sensitive_paths` opt-in. Reads of these files are still allowed; only
  writes are blocked.
- **DNS-rebinding defense (Rampart).** The network egress guard now filters
  private/loopback/link-local addresses at DNS-*resolution* time via a custom
  resolver, so a public hostname that resolves to `127.0.0.1` /
  `169.254.169.254` / an RFC-1918 address is never connected to (TOCTOU-safe).
  Failed requests now also surface the full error cause chain, so a blocked
  request reports why.

## [0.7.24] — 2026-07-02

### Security

A privacy-first hardening pass — three composing, default-on layers, each
live-verified on the dogfood rig:

- **Sensitive-path read guard (Ward).** The agent can no longer read known
  secret locations — `~/.ssh`, `~/.aws`, `~/.gnupg`, cloud/k8s/docker creds,
  `.env` files, private keys, **and Aivyx's own encrypted store + passphrase** —
  at any access level, on the canonical (symlink-resolved) path, across
  `fs.read` and the data readers. Opt specific paths back in with `[access]
  allow_sensitive_paths`; disable with `guard_sensitive_paths = false`.
- **Network egress guard (Rampart).** The web tools refuse loopback /
  link-local / private / unique-local targets by default — on the initial URL
  and every redirect hop — blocking SSRF and cloud-metadata theft
  (`169.254.169.254`) and local-service pivots. `[access] allow_private_egress`
  re-enables localhost/LAN; `allow_egress_hosts` hard-restricts to named hosts.
- **Prompt-injection resistance (Bulwark).** Content the agent ingests (web
  pages, extracted articles, parsed files, third-party MCP outputs) is fenced
  as untrusted DATA — never instructions — via a demarcation envelope plus a
  standing charter rule. Defense-in-depth, composing with the layers above.

### Fixed

- **Auto-delegated stories are held to the real artifact.** Completion
  verification for a loop-delegated team mission is now grounded on the memory
  the team actually wrote (matching the solo `loop.complete` path), so a terse
  mission result over genuine work is no longer false-rejected.

## [0.7.23] — 2026-07-01

### Fixed

Autonomous-loop hardening, all surfaced by a live dogfood run (gpt-oss:20b):

- **Auto-delegated stories no longer skip on a specialist name/role mismatch.**
  The planner refers to a team specialist by its role label ("Operations"), but
  resolution matched only the roster id ("ops") → the mission errored `no
  specialist "Operations"` → retried → skipped a doable story. Resolution now
  matches the name **or** the role, case-insensitively (extends the earlier
  case-only fix, which never covered genuine name↔role differences).
- **Completion verification now judges the real artifact, not just the summary.**
  With `[loop] verify_completion`, the acceptance judge was rejecting
  genuinely-complete research/memory stories because the agent's summary was
  terse — even though the note was in memory. The judge is now shown a snapshot
  of the recent memory the agent wrote and passes when that evidence satisfies
  the criteria (solo `loop.complete` path).
- **A malformed tool call from a local model no longer hard-fails the turn.**
  Ollama returns a bare HTTP 500 when a model emits unparseable tool-call JSON;
  the provider now retries once (resampling almost always parses), while genuine
  500s still surface immediately.

## [0.7.22] — 2026-07-01

### Added

- **`aivyx memory conflicts` / `resolve` / `dismiss` — contradiction detection
  for stored memory (Chapter Concord).** The agent stored whatever it was told,
  so contradictory facts coexisted silently ("home airport is YPPH/Perth" *and*
  "...Sydney/YSSY"). An on-demand LLM pass now flags pairs of entries that assert
  incompatible facts about the same subject — within one topic **or across two
  topics** — and the operator resolves each by keeping one (`resolve <topic>
  --archive <seq>` deletes the other) or dismissing a false positive
  (`dismiss <id>`, durably suppressed). No background cost: detection only runs
  when asked. New `Memory::delete_entry` substrate primitive + a 26th encrypted
  storage domain for dismissals.
- **`aivyx memory wiki` / `graph` — CLI knowledge-base inspection.** The agent's
  synthesized wiki pages and typed knowledge graph, previously Studio-only, are
  now viewable from the terminal.

### Fixed

- **Internal `context:pruned:*` bookkeeping no longer leaks into the knowledge
  base or the agent's context.** The earlier internal-topic filter covered only
  `memory list`; the same machine-state archives still surfaced through six more
  paths — the wiki + graph sweeps, both graph queries, memory search, the agent's
  RAG auto-recall, and its wildcard `memory.read`. All are now filtered from one
  shared definition in the memory substrate.
- **The typed knowledge graph no longer fills with conversation-mechanics
  noise** ("conversation history → messages", "assistant → tool_call →
  web_search"): the extractor drops triples about the session itself and keeps
  domain knowledge.
- **The first daemon query after a restart no longer fails** with a spurious
  `RecoveryNotice` protocol error (the long-standing "memory list transient
  flakiness").

## [0.7.21] — 2026-07-01

### Changed

- **Acceptance verification now covers auto-delegated work too.** When `[loop]
  verify_completion` is on, a story the loop auto-delegates to the agent team is
  judged against its acceptance criteria — the team mission's result must pass
  the same LLM acceptance check a solo `loop.complete` gets — before it's marked
  done; otherwise it's retried (then skipped). Previously a delegated mission was
  accepted on completion alone, skipping the check solo work received.

## [0.7.20] — 2026-07-01

### Fixed

- **Auto-delegated team missions no longer fail and retry (and can no longer skip
  a doable story).** The autonomous loop's `delegate_above` path could hand a
  story to the team, have the mission error out, retry, and eventually skip the
  story — wasting work. The cause was a case-sensitive specialist lookup: the
  planner names a specialist "Researcher" but the roster is "researcher", so the
  mission errored with "no specialist". Specialist matching is now
  case-insensitive. (Unattended decompositions also no longer emit approval gates
  that can't be satisfied without an operator.)

## [0.7.19] — 2026-06-30

### Changed

- **The release pipeline retries transient crates.io network failures.** A
  `.cargo/config.toml` with `[net] retry = 10` plus a step-level retry around
  `cargo install` in the desktop build stop the recurring "curl failed / HTTP2
  framing layer" flakes that needed a manual rerun each release.
- **The loop's completion judge logs its verdict.** When `[loop]
  verify_completion` is on, the daemon now logs each ACCEPT/REJECT decision (and
  the reason) instead of gating silently — so an operator can see why a story was
  held back or accepted.

## [0.7.18] — 2026-06-30

### Added

- **The agent owns up when it claims an action it didn't take (Chapter Candor).**
  After a turn, Aivyx compares what its reply claims against the tools it actually
  called, and appends an honest note if it said it did something (e.g. "saved to
  memory", "scheduled a routine", "sent a notification") without the matching tool
  call. Non-blocking — it annotates, never breaks the turn — and upholds the
  agent's honesty contract instead of quietly papering over a dropped step.

## [0.7.17] — 2026-06-30

### Fixed

- **`aivyx memory list` (and the Studio Memory browser) no longer shows internal
  topics.** The per-session `context:pruned:*` context-pruning archives — machine
  bookkeeping, not operator knowledge — are hidden from topic listings (they were
  swamping the real topics). They remain reachable by exact `aivyx memory show
  <topic>`; only the cluttered enumeration is filtered.

### Changed

- **The autonomous loop skips a story after repeated failed auto-delegations.**
  When `[loop] delegate_above` is set, a story whose delegated mission keeps
  ending non-Done is marked skipped after a couple of attempts instead of
  re-running a full team mission every iteration.

## [0.7.16] — 2026-06-30

### Added

- **Deterministic auto-delegation for the autonomous loop (Chapter Foreman).**
  Opt-in `[loop] delegate_above = N`: before each solo turn the loop scores the
  next backlog story with a structural complexity heuristic and, if it scores
  `>= N`, hands it to the agent team (headless) instead of attempting it solo —
  so delegation no longer depends on a small local model choosing to call
  `team.run`. Default off.

### Fixed

- **`aivyx team status` shows the real halt reason.** A mission stopped with
  `aivyx team abort` (or a budget cap) now reports *why* it halted (e.g.
  "aborted by operator") instead of always labelling it "(budget)". The reason
  is persisted on the mission record and rendered in the detail view.

## [0.7.15] — 2026-06-29

Three multi-agent / autonomy refinements.

### Added

- **Heterogeneous teams (Chapter Ensemble).** Each `[[team.member]]` may now
  declare its own `model` and/or `base_url` (same provider kind). A coordinator
  can run on a big model while grunt specialists run on a small fast one
  (role-fit + cost), and pointing roles at different endpoints (e.g. a second
  Ollama / GPU) gives true parallel execution instead of serializing on one
  server. Both optional — omit them and a role uses the team's shared default.
- **Per-story acceptance verification (Chapter Verdict).** Opt-in
  `[loop] verify_completion`: when the autonomous loop calls `loop.complete`, an
  LLM judge checks the agent's summary against the story's acceptance criteria
  and blocks the completion on a FAIL (the story stays pending) instead of
  trusting the self-report. Fails open; stack with `gate_command` for
  artifact-grounded checks.
- **Abort a running team mission (Chapter Belay).** `aivyx team abort <id>`
  stops a running mission — it halts gracefully at its next step boundary
  (in-flight work finishes, completed outputs preserved). Completes the operator
  control surface alongside budget caps and human-gate approve/reject.

## [0.7.14] — 2026-06-29

### Fixed

- **`aivyx mcp status` no longer falsely reports "no MCP servers".** The status
  snapshot is daemon state, but it was being overwritten by transient CLI
  invocations (a `--headless` turn, the REPL), which clobbered the running
  daemon's snapshot with empty entries — so the command showed no servers while
  MCP was actually live. Only the daemon writes the snapshot now. (Backlog #9.)
- **The `trend-scan` routine never broadcasts fabricated findings.** A new
  delivery gate (`notify_when = "on_completed_grounded"`, now the default for
  `trend-scan`) only pushes a result when the turn actually did work (≥1 tool
  call). A trend-scan that produced "findings" with no real web search is
  suppressed instead of being sent as if real. (Backlog #6 follow-on.)

## [0.7.13] — 2026-06-29

### Fixed

- **The weekly digest no longer fabricates.** A check-in caught the scheduled
  `weekly-digest` inventing profile-themed "accomplishments" that never happened
  (a "Flutter revenue dashboard", etc.) — a local model fills a sparse summary
  with plausible fiction no matter how the prompt is worded. The digest is now
  **assembled deterministically by the daemon** from your actual memory (entries
  written since the last digest) plus the live pending-proposal count, with real
  dates; if nothing was recorded it says so plainly. There is no LLM in the
  content path, so it cannot confabulate. (Backlog #6, Chapter Ledger.)

### Added

- **Chapter Deckhand — opt-in "use your open applications" (experimental).** With
  `[applications] enabled = true`, the agent can use the GUI apps open on your own
  machine: list windows, focus one, type / press keys / click, and screenshot.
  Default **off**; Trusted-tier only; input injection is **confirm-first**; every
  action is an audited tool call. Linux X11 / Xwayland in this release (needs
  `xdotool` + a screenshot tool); native-Wayland windows are a known limitation.
  See `docs/APPLICATIONS.md`.

## [0.7.12] — 2026-06-29

### Added

- **Chapter Helm — autonomous loop runs survive a daemon restart.** A new opt-in
  `[loop] resume_on_boot` resumes an interrupted autonomous-loop run when the
  daemon restarts — so a "runs for days" agent (e.g. under systemd
  `Restart=on-failure`) keeps working through its backlog instead of silently
  stopping after a crash. It resumes only when a run was genuinely active when
  the daemon stopped (a crash or restart) and stories remain pending; an
  explicit `aivyx loop stop` is remembered across the restart and is **not**
  resumed. Default off — auto-resuming a code-committing loop is a deliberate
  operator choice.

## [0.7.11] — 2026-06-29

### Fixed

- **Release pipeline gate.** A test helper added in 0.7.10 (Chapter Ballast)
  triggered a `clone_on_copy` clippy lint, which the cargo-dist release gate
  (`clippy --all-targets -D warnings`) treats as an error — so 0.7.10's CLI
  installer binaries failed to publish (the desktop app and Docker image, built
  by separate workflows, shipped fine). This release republishes the CLI
  binaries with the lint fixed. No runtime behavior change from 0.7.10.

## [0.7.10] — 2026-06-29

### Added

- **Chapter Ballast — a per-mission budget for autonomous team missions.** When
  the autonomous loop delegates a story to a background team mission, that
  mission previously had no aggregate spending limit (only a per-call token
  cap). Two new opt-in `[budget]` caps — `per_mission_tokens` and
  `per_mission_usd` — bound a single mission's total spend across all its
  specialist sub-turns; a tripped cap **halts the mission gracefully** at the
  next step boundary (completed work preserved, the reason recorded). Tokens
  bound local/free runs where the dollar cap (priced at $0) never trips. Both
  default to unbounded, so behavior is unchanged unless you opt in.

### Fixed

- **`aivyx init --template <name>` now matches a plain `aivyx init`.** The
  template path silently skipped semantic-memory setup (`[embedding]` +
  `[memory] profile`), the onboarding persona seed, and the web-search server —
  so a templated agent diverged from a default one. It now applies each of those
  (any a template already declares is respected, never duplicated).

## [0.7.9] — 2026-06-28

### Added

- **Chapter Ember — embedding-free "lite" recall.** `[memory] profile = lite`
  now delivers real recall with **zero setup** — no embedding model, no vectors,
  no paid generation. It fuses BM25 lexical search with a co-occurrence graph
  walk (seeded from the lexical hits) over the memory the agent already has.
  Previously `lite` produced *no* recall because the whole recall stack was gated
  on an embedding provider. `aivyx doctor` reports it as "lite recall active".
  Set `profile = smart` with an `[embedding]` section for semantic recall; a
  `smart` config with no embeddings stays off (and now warns). (Backlog
  opportunity C.)

### Fixed

- **Chapter Etch — the agent now reliably persists explicit "remember this"
  requests.** Previously a local model treated "remember X" as conversation and
  often never called `memory.write`, so a later turn couldn't recall it (and a
  soft charter instruction didn't move it — verified). Now the per-turn memory
  hook **deterministically** detects a leading "remember / note / save / don't
  forget X" request in the operator's message and persists the fact itself —
  independent of the model's tool-calling — embedding it so it's immediately
  recallable. Conservative detection: it ignores questions, reminiscing
  ("remember when…"), reminders ("remember to…"), and first-person mentions.
  Live-verified: "Remember my home airport is YSSY" → a fresh-context "is it good
  flying at my home airport?" now recalls YSSY and answers, instead of asking
  which airport. (Backlog #8.)
- **A `smart` memory profile with no embeddings is no longer silently inert.**
  `[memory] profile = smart` arms the semantic recall stack, but with no
  `[embedding]` provider the semantic source does nothing — the trap that can
  leave "smart" memory dark. The config loader now warns at startup and
  `aivyx doctor` flags the mismatch (use `profile = lite` for embedding-free
  recall, or add an `[embedding]` section). (Backlog #1.)
- **Clearer error when a non-interactive `aivyx` collides with a running
  daemon.** A piped/non-TTY turn (`printf … | aivyx`) opened the store directly
  and failed with redb's opaque "Cannot acquire lock" while the daemon held it.
  It now explains the situation and points to `aivyx --headless "…"` (which
  routes over the daemon) or `aivyx daemon stop`. (Backlog #2.)
- **The daemon no longer logs clean client disconnects as errors.** A finishing
  CLI query dropping its socket produced a `connection handler error: Broken
  pipe` line on every call, spamming `journalctl` and masking real errors. Clean
  hang-ups are now silent; genuine faults still log. (Backlog #4.)

## [0.7.8] — 2026-06-28

### Fixed

- **`aivyx init` no longer shreds list items that contain commas.** The wizard's
  comma-separated Profile fields (use cases, behavioral preferences/constraints,
  character traits) are now split **paren-aware** — a comma inside `(...)`/`[...]`
  no longer splits, so an item like "Research my passions (flying, food, coffee)"
  stays a single entry instead of fragmenting.

## [0.7.7] — 2026-06-28

### Fixed

- **Chapter Plumb — default report routines no longer confabulate.** A fresh-agent
  check-in caught the `weekly-digest` routine *fabricating* a week of
  accomplishments (zero tool calls) — and `trend-scan` silently doing nothing —
  because those default `[[schedule]]` prompts were passive "summarize / search"
  instructions the local model could satisfy by inventing prose instead of
  reading (empty) memory/journal. The reporting prompts now **name an explicit
  first read** (recall memory + read the journal; *actually* web-search for
  trend-scan), report **only what's genuinely found**, and say "nothing notable
  to report yet" when the record is empty — never invent, infer, or pad.
  Re-verified live: the digest now grounds in real memory instead of fabricating.
  (A daemon-level "an aggregation routine that made no tool calls is a no-op"
  backstop is noted as a follow-up.)

## [0.7.6] — 2026-06-28

### Added

- **Chapter Anchor — `aivyx daemon install` (run for days).** The bare daemon —
  the default install — now installs as a first-class persistent **user**
  service so a local-first agent meant to run for days actually stays running
  (its scheduled routines firing, its loop available) across logout and reboot,
  without hand-rolled `systemd`/`launchd` files.
  - **`aivyx daemon install [--web-ui] [--no-start]`** + **`aivyx daemon
    uninstall`**. Linux = a systemd user unit + `loginctl enable-linger` (no
    root); macOS = a launchd `LaunchAgent`. Idempotent re-install.
  - The unattended store passphrase is captured at install (`AIVYX_PASSPHRASE`
    or a one-time hidden prompt) and kept owner-only at rest — a `0600`
    `EnvironmentFile` the unit references on Linux (never in the unit itself),
    the `0600` plist's `EnvironmentVariables` on macOS.
  - `aivyx doctor` gains a **Service** section (installed / running), and
    `aivyx init` points new users at `daemon install`. Documented in
    `docs/INSTALL.md`. Live-verified end to end on the dogfood rig.

- **Chapter Passport — the identity & cross-boundary trust keystone (FED.1–5).**
  The open-core trust **substrate** for agent-to-agent interaction across an
  operator boundary — the one primitive `VISION.md` flags as impossible to
  retrofit (*"a network of agents is a Nonagon team with the trust boundary
  moved"*). New `crates/aivyx-federation`:
  - **Identity** — an operator-owned Ed25519 keypair + signed, replay-guarded
    request envelope; key-at-rest sealed with a subkey derived from the storage
    `MasterKey`; key material never logged.
  - **Trust** — a per-peer `TrustPolicy` (deny-by-default; allowed scopes in the
    `Scope` vocabulary; a confirm-first autonomy ceiling) and the cross-operator
    attenuation (`effective = asked ∩ policy ∩ host-ceiling`, autonomy floored) —
    NT-02 generalized so a peer can never exceed what *both* operators allow, and
    revocation narrows reach immediately.
  - **Relay shape** — the `chat`/`task`/`search` verbs as wasm-clean
    `aivyx-ipc::federation` types ("as if the peer is a stranger"), plus the
    forensic `Crossing` shape for the audit chain.
  - **Untrusted peer content** — provenance-tagged, payload-validated, runnable
    only under its attenuated authority (never the host's broader caps).
  - **Operator consent** — peer-initiated effect (a `task`, or any irreversible
    scope) routes through the existing confirm-first gate; no cross-boundary
    self-escalation.

  Substrate only — no transport, peer discovery, reputation, or the Nexus
  product (built last, on an installed base). Proven end-to-end by a
  two-identity in-process integration test.

## [0.7.5] — 2026-06-28

### Added

- **Chapter Circuit — agentic-loop hardening (CI.0–CI.6).** A deliberate
  audit + hardening pass over the autonomous loop, prompted by the v0.7.4
  scope-floor bug.
  - **CI.0** — the loop's `team.run` delegation tool is now granted to the
    default role when the loop is armed (it was registered and offered in the
    iteration prompt but, like `loop.*` before v0.7.4, never floor-granted, so
    the "delegate to a team" branch was dead). `git.write` stays operator-opt-in
    by design. Adds a prompt↔floor drift guard so the two can't silently
    diverge again.
  - **CI.1** — a **cross-iteration stall breaker**. The loop driver now stops a
    run after `[loop] max_idle_iterations` consecutive iterations make no
    progress — neither completing/delegating a story nor recording a fresh
    progress note. This catches a loop spinning on an unrecoverable error (the
    v0.7.4 bug burned all 25 iterations / 631k tokens re-failing identically)
    instead of letting it exhaust the iteration/token caps. Default 3; set `0`
    to disable. Distinct from Bridle's within-turn repeat breaker — this is
    across fresh-context iterations.
  - **CI.2** — the iteration prompt is now **task-agnostic**. It previously
    hardcoded a software-dev flow (run the build/tests, commit with `git`),
    which stranded everyday-PA stories — the model had to improvise past
    instructions for work that has no project and no granted `git.write`. The
    verification step now offers a fitting check per task type (gates for code,
    re-read for research/writing, read-back for a file), and the persistence
    step routes the result to its proper home (a commit for code *only if
    committing is available*, memory for notes, the requested path for a file).
  - **CI.3** — audit of gate verification and `team.run` delegation governance
    (documentation, no behavior change). Clarified that the loop gate is a
    tree-level regression guard, not a per-story completion verifier; and
    decided to keep `team.run` available whenever the loop is armed (rather than
    posture-gate it) while documenting that loop-delegated team missions are not
    yet bounded by an aggregate budget — both captured as tracked follow-ups.
  - **CI.4** — durability & re-entrancy audit. Fixed a wedge: if the loop driver
    task panicked mid-run it left the run flagged `active` with no driver behind
    it, so every later `loop start` silently no-op'd ("already running") until a
    daemon restart — a new run-scoped guard now clears the flag on an abnormal
    exit. Documented that loop run-state is in-memory only (a daemon restart
    mid-run does not auto-resume; stories persist, the operator re-issues
    `loop start`); opt-in auto-resume is a tracked follow-up.
  - **CI.5** — observability. `aivyx loop status` now shows the stall-breaker
    configuration (`stall breaker: stop after N idle iteration(s)` / `off`)
    alongside the other run caps, and a live warning while a run is spinning
    (`no progress for N of M iteration(s) before the stall breaker stops the
    run`) — so a stalling run is legible without reading `journalctl`. The
    stall threshold + live idle count ride the `LoopStatus` IPC / run-state.
  - **CI.6** — live-verified the chapter end-to-end on the dogfood rig: research
    stories complete through the task-agnostic path with no git noise (CI.2); an
    unrecoverable story trips the stall breaker after 3 idle iterations instead
    of burning the 25-iteration cap, and stays pending rather than being
    false-completed (CI.1); `loop status` shows the stall-breaker config + stop
    reason (CI.5). (The `team.run` grant is present and never scope-denied;
    delegation dispatch wasn't observed because the small local model
    consistently self-implements even when told to delegate.)

### Fixed

- **Flaky e2e store-path collision.** Four `aivyx-channel` integration suites
  (`fs_tool_e2e`, `cli_e2e`, `audit_persistence_e2e`, `memory_tool_e2e`) built
  their scratch `store.redb` path from `pid + nanos`, which is not unique across
  the crate's parallel test threads — under CI load two tests could land on the
  same clock tick, collide on the path, and fail the second `RedbStorage::open`
  on the redb lock (this failed the v0.7.4 release's quality gate). Each path now
  carries a uuid, matching the existing `storage_persistence_e2e` fix.

## [0.7.4] — 2026-06-28

### Fixed

- **The autonomous loop's tools were never granted to the default role.** Arming
  `[loop]` (or an `[autonomy]` level that arms it) spawned the loop driver, but
  the default role's backcompat scope floor granted `memory.*` / `fs.*` /
  `net.*` / `shell.exec` / `workspace.*` / `ollama.*` / `mcp.call:*` and **never
  `loop.*`** — so every iteration was denied at step one (`loop.next ... not
  currently granted`) and the driver burned its full iteration cap re-failing at
  the same wall. The autonomous-loop backlog mechanism was effectively dead on
  arrival for the zero-config default role. The floor now grants
  `loop.next` / `loop.complete` / `loop.note` when the loop is armed, mirroring
  the existing ollama/MCP grants; self-escalation scopes are still withheld.
  Found and verified on a live multi-day dogfood run.

## [0.7.3] — 2026-06-27

### Added

- **Default starter skills (Chapter Outfit).** A brand-new agent now ships with a
  curated starter repertoire — `summarize-document`, `research-and-summarize`,
  `draft-reply`, `daily-briefing`, `capture-note` — so it can do real work on
  turn one instead of arriving with none. Each is a lightweight
  `{name, trigger, procedure}` recipe whose steps compose the agent's own tools
  and pillars (the structured-data readers, web search/extract, memory,
  workspace, persona). Compiled-in and default-on, planted onto a fresh agent's
  persona chain at first boot; an already-running agent is never retro-injected.
  Opt out with `[skills] starter = false`.
- **Operator-initiated skill teaching (Chapter Tutor).** New
  `aivyx skills teach | update | forget` commands let the operator author a skill
  directly onto a **grown** agent's persona chain — the channel that was missing
  (genesis seeding is empty-chain-only, and the agent's own `skills.teach` tool is
  gated behind a scope an autonomous agent shouldn't hold). Operator authoring
  routes over the local daemon socket (same authority as `aivyx persona revert`)
  and is cleanly separated from agent self-teaching, so it needs no agent scope.
  Signed, audited on the persona chain, and reversible via `aivyx persona revert`.

### Changed

- **Semantic memory works out of the box (Chapter Engram).** `aivyx init` now
  configures an embedding provider — pulling `nomic-embed-text` on the local
  Ollama path, or using OpenAI embeddings on the cloud path — and turns on the
  full memory profile (`[memory] profile = "smart"`). Previously the whole
  graph-augmented memory stack was dormant on a fresh install: the daemon only
  builds the auto-recall pipeline when `[embedding]` is configured, and `init`
  never wrote one, so a new agent did no semantic recall at all regardless of the
  memory profile. (Existing installs are unchanged on upgrade — the new defaults
  are written only into new configs, so the token-spending extraction sweeps are
  never a surprise.) `aivyx doctor` gained a Memory section reporting the
  embedding provider, the active profile, and — on the local path — whether the
  embedding model is actually downloaded.

## [0.7.2] — 2026-06-27

### Changed

- **The default system prompt is now an operating charter (Chapter Keel).** The
  out-of-box base layer was a single line ("a terse and thoughtful assistant
  running in a local terminal"). It is now a compact (~270-token) operating
  charter that states, in prose the model actually reads, what the dynamic
  Profile/Persona/Tools/Skills layers don't: how the agent works (terse and
  honest, tool-first, uses its memory and workspace, doesn't loop), its safety
  posture (confirm-first on irreversible/outbound actions, never widens its own
  authority/reach/autonomy, everything is recorded — mirroring
  `docs/SECURITY_POSTURE.md`, previously stated only in code), and turn
  discipline. It remains a compiled-in default, fully overridable via
  `[agent] system_prompt`, a per-`[[role]]` `system_prompt`, or
  `AIVYX_SYSTEM_PROMPT` — existing installs inherit it live and pick up future
  refinements on upgrade. No new config is planted into `aivyx.toml`.
- **The Studio's Command Center reflects a live, working agent.** The home
  dashboard now surfaces an agent-vitals rail (model · provider · context ·
  autonomy · access, with a pulsing "live" dot when the daemon is online) and a
  Routines panel showing each scheduled background routine's cadence, enabled
  state, and last/next fire — the clearest "the agent is working on its own"
  signal. Backed by a new read-only `GetSchedules` IPC; no new capability.

### Fixed

- **Eliminated a parallel-test flake in `aivyx-config`.** Two config-load tests
  read the ambient environment without holding the test suite's env-guard,
  occasionally racing parallel env-mutating tests (intermittent failures under
  default test threads; green at `--test-threads=1`). Both now hold the guard,
  and the invariant is enforced via a thread-local check so any future unguarded
  config-load fails deterministically with an actionable message instead of
  flaking.

## [0.7.1] — 2026-06-27

### Security

- **Bumped `quinn-proto` 0.11.14 → 0.11.15** (RUSTSEC-2026-0185): a remote
  memory-exhaustion via unbounded out-of-order QUIC stream reassembly. A
  transitive dependency (via `reqwest`'s HTTP/3 path); a lockfile-only patch
  bump, no API change. `cargo audit` no longer reports any vulnerability.
- **Documented the `aivyx-desktop` GTK3 advisory cluster in `deny.toml`.** The
  native desktop app's webview (tao/wry/webkit2gtk → the frozen gtk-rs GTK3
  bindings, pinned at glib 0.18) pulls 11 `unmaintained` advisories
  (RUSTSEC-2024-0370 + 0411–0420) plus the glib `VariantStrIter` unsoundness
  (RUSTSEC-2024-0429 = GitHub Dependabot alert #6). All are transitive,
  desktop-build-only (the daemon/core/CLI never link them), unfixable until the
  webview ecosystem moves off GTK3, and not exploitable in the thin shell. The
  unmaintained ones are now ignored with documented rationale (restoring
  `cargo deny` to green); the glib unsoundness is recorded as the accepted
  disposition for the Dependabot alert (dismiss as "tolerable risk").

### Changed

- **`aivyx init` ends on real next steps.** The completion message now surfaces
  the three things a new operator actually needs — `aivyx` (terminal chat),
  `aivyx daemon run --web-ui` to open the **Studio** at `http://127.0.0.1:7843`
  (the web GUI a first-run user wouldn't otherwise discover), and `aivyx doctor`
  to re-check — plus, on the local path, a pointer to `docs/LOCAL_HOSTING.md` for
  sizing the model/context to a capable GPU. The Studio port comes from the
  canonical `DEFAULT_WEB_UI_PORT` so it can't drift.
- **One `TurnSafety` choke point for the per-turn knobs.** The deadline + cycle
  breaker were applied ad-hoc at ~7 `ConcreteAgent::new` sites — which is exactly
  why three of them drifted and shipped unprotected. Now every agent-construction
  path ends with `TurnSafety::<posture>(…).apply(agent)`: `interactive` inherits
  the operator's `[agent]` config (REPL, voice, daemon, role-switch child),
  `autonomous` forces the breaker floor (team lead + specialists), and the
  standalone remote-channel builders route through `default()`. `SessionConfig` /
  `AgentStackSpec` now carry a single `turn_safety` field instead of two. No
  behaviour change — the wiring just can't drift across sites again.

### Added

- **`aivyx autonomy` — one dial for how autonomous your agent is (Chapter Reins).**
  A new `[autonomy] level` (`manual` → `assisted` *(default)* → `supervised` →
  `autonomous` → `unleashed`) composes the scattered autonomy knobs (access,
  confirm-first, gate policy, loop arming, self-improvement adoption) into named
  tiers, with per-domain `[[autonomy.override]]` exceptions ("autonomous at
  shell, manual on email") and an `[autonomy.auto_approve]` reversible-scope
  allowlist. `aivyx autonomy show` renders the resolved level and the posture it
  expands to; `aivyx autonomy set <level>` rewrites the section
  (`autonomous`/`unleashed` confirm first). The default `assisted` expands to
  today's behavior byte-for-byte, so an absent `[autonomy]` section changes
  nothing. Its **first runtime effect**: a `supervised`/`autonomous`/`unleashed`
  level **arms the autonomous loop** even without an explicit `[loop] enabled`
  (additive — it never disarms an explicitly-enabled loop). Arming only makes the
  loop *available* (a run still needs `aivyx loop start`) and takes effect only
  when a `[loop]` section exists (which carries the iteration/budget caps). The
  remaining dimensions (gate policy, growth) are wired incrementally — see
  `docs/AUTONOMY.md`. No new capability base; a composition front end to
  primitives already enforced. Settable from the **Studio Settings** screen too
  (an "Autonomy" section with a level picker over a new `SetAutonomyLevel` IPC,
  server-side confirm-first on the autonomy-granting levels).
- **`aivyx --headless "<task>"` — one-shot unattended runs from the CLI.** The
  headless execution mode (Chapter H) was reachable over IPC and from the
  operator-absent drivers, but never from the command line. This wires the
  missing entry: it connects to a **running daemon**, submits one turn that
  *refuses* (records the reason on the audit chain) at any approval gate rather
  than parking for an operator, streams the output, and maps the turn's outcome
  onto a **process exit code** for cron / batch / autonomous callers — `0`
  completed, `3` refused-at-a-gate, `1` any other non-completion. No daemon
  running yields a clear "start `aivyx daemon run` first" error (there is no
  in-process fallback — headless relies on the daemon's gate interception). No
  new tool/base/dep; the interactive paths are byte-identical when the flag is
  absent.
- **Small-cycle breaker (`[agent] cycle_detection`).** A loop-safety companion
  to the consecutive-identical breaker (Chapter Bridle): it catches a repeating
  *cycle* of tool calls (`A,B,A,B,…`) that the consecutive counter resets on —
  the one runaway shape that previously ran until the 32-step cap or the 120s
  deadline. A bounded ring of recent call signatures trips when the tail is
  `min_repeats` back-to-back copies of a `2..=max_period` block, reusing the
  existing `Looping` outcome (no new audit surface). **Default-off** (the turn
  loop stays byte-identical); operators arm it with `[agent] cycle_detection =
  true` — or toggle it from the **Studio Settings screen** (a new "Agent" section
  with a Cycle-breaker toggle, over a `SetCycleDetection` IPC that rewrites the
  `[agent]` section; `GetSettings` now reports the current state).
- **Nonagon team agents get the cycle breaker as a built-in safety floor.** The
  lead and every specialist run autonomously inside a mission — no human watches
  each turn to `/cancel` a runaway — so they always get the small-cycle breaker
  (like `MAX_STEPS_PER_TURN` is always on), independent of the interactive
  `[agent] cycle_detection` knob.
- **Default scheduled "starter routines" at first-run.** `aivyx init` now plants
  a small set of read-only, self-contained `[[schedule]]` routines so a fresh
  agent orients itself and stays useful unattended: a daily **environment-review**
  (self-bootstrapping — first run builds a baseline of the accessible
  environment, later runs diff it and journal changes), a nightly
  **reflection** (consolidate the day's memory into the knowledge base), a
  6-hourly **health-check** (silent unless something breaks), a Monday
  **weekly-digest** (learnings + pending proposals), and an opt-in daily
  **trend-scan** (web research across the operator's interests). Each prompt is
  read-only and explicitly forbids destructive actions — they run unattended.
  Gated by the wizard's existing answers: the four core routines are **enabled on
  a local (Ollama) provider** and written **present-but-disabled on a cloud
  provider** (discoverable, a one-line flip to enable, and cost-aware since
  cloud schedules spend tokens); `trend-scan` additionally requires web search.
  The wizard prints what it set up, so it is never surprise behavior. Verified
  live on real hardware (all five fire and execute) before shipping. See
  `docs/ROUTINES.md`.

### Fixed

- **Daemon no longer panics when a runaway turn ends in `MaxStepsExceeded` /
  `Looping` with failure-learning on.** A `match` building the failure summary
  for the skill auto-proposer handled `Failed`/`Cancelled`/`TimedOut`/`Escalated`
  but fell through to `_ => unreachable!()` for the two outcomes a *local model*
  is most likely to produce when it runs away (the step cap or the cycle
  breaker) — so with `[skills.auto_propose] from_failed_turns = true`, exactly
  that scenario killed the daemon mid-mission. The match is now **exhaustive**
  (no `_` arm), so the compiler forces every present and future `TurnOutcome` to
  be handled here — a single turn can never panic a 24/7 daemon. Surfaced by the
  Pass A panic-resistance audit, which otherwise found the untrusted-input parse
  boundaries (LLM responses, tool-call JSON, tool inputs, IPC frames, config,
  channel ingestion) already defensive.
- **`[agent]` per-turn knobs now reach the daemon agent and the role-switch
  child** (the Studio + persistent chat path, and role sub-sessions).
  `turn_timeout_secs` and the new `cycle_detection` were applied only on the
  REPL/voice paths (`build_agent_stack`); the daemon agent and the `role.switch`
  child factory built raw `ConcreteAgent`s that skipped them, so they always ran
  the 120s default and no cycle breaker — a latent gap on the primary path, now
  closed (the child inherits the operator's config, like its parent).
- The `channel-voice-full` feature build: the voice `AgentStackSpec` literal had
  drifted from the struct (it predated `turn_timeout`), so it failed to compile
  under that feature. Restored, with the new cycle-detection knob wired in.
- **`access_level = full` granted nothing — the most permissive level was the
  only broken one.** `full` resolves the sandbox root to `/`, and every
  root-anchored scope grant was built as `format!("{root}/**")`, which at root
  `/` produced `fs.read://**` (double slash) — a glob whose leading `//` matches
  no single-rooted path. So a `full`-access agent could not read or write a
  single file (it would narrate a permission refusal). Fixed with a `rooted_glob`
  helper that collapses the trailing slash (`/` → `/**`), routed through all five
  root-anchored grant sites (`fs.read`/`fs.write`/`fs.metadata` +
  `shell.exec:cwd` + `fs.delete`). Found live on a real-hardware dogfood; the
  needed scope was always correct, so it hid behind a model that politely
  refused. Regression test added.
- **Local thinking-model output was verbose and off-style — the `think` flag was
  inverted.** For a model advertising the `thinking` capability the Ollama
  provider sent `think: false`, assuming that forces the answer into `content`.
  On current Ollama (0.30) that is wrong for hybrid-reasoning models: e.g.
  `qwen3:30b-a3b` ignores `think: false` and dumps its whole chain-of-thought
  into `content`. `think: true` instead cleanly routes reasoning into the
  separate `thinking` field (which the agent already discards) and leaves the
  answer — or tool_calls — in `content`. Now sends `think: true` for
  thinking-capable models (verified for qwen3:8b and qwen3:30b-a3b, with and
  without tools). Test updated.
- **Configured MCP servers' tools were registered but un-callable by the default
  role.** Each MCP tool call needs the scope `mcp.call:<server>:<tool>`, but the
  default-role capability floor granted no `mcp.call` scope at all — so the
  bundled web-search server the wizard's "Enable web search?" adds was dead on
  arrival (the agent saw the tool, lacked the scope, and reported it as "not
  authorized"). The floor now grants `mcp.call:<server>:*` per configured MCP
  bridge (mirroring the existing `ollama.*` grant) — least-privilege per server.
  Roles that declare their own `capability_scopes` still name `mcp.call:<server>`
  grants explicitly. Scope-matching regression test added.

## [0.7.0] — 2026-06-24

The desktop & experience release. A native desktop app, a top-to-bottom Studio
overhaul, end-user documentation, and the first ecosystem seam for vertical
packs — all over the unchanged security core (no new P10 amendment, no new
capability surface).

### Added

- **Native desktop app (`aivyx-desktop`).** A thin `tao` + `wry` shell that
  hosts the Studio in a system webview (reusing the exact WASM UI, no port),
  with daemon lifecycle (attach or spawn), a **system tray** (Open Studio ·
  Restart daemon · Start at login · Quit) and hide-to-tray, **native
  approval-gate notifications** (a background WS client watches for missions
  awaiting approval), a **global hotkey** (`Ctrl+Shift+A`) to summon the window,
  and launch-on-login. Packaged with `cargo-bundle` (a `.deb` on Linux, a
  `.app`/`.dmg` on macOS) via a dedicated release workflow. Linux runtime deps:
  `webkit2gtk-4.1`, `libayatana-appindicator`, `xdotool`/`libxdo`.
- **`aivyx-vertical-sdk`** — a thin, semver-stable facade that re-exports only
  the pack-facing slice of the engine, so vertical packs survive core refactors.
  The Kitchen pack is re-pointed at it as the open reference example;
  `crates/verticals-private/` (git-ignored, auto-joined via a workspace member
  glob) is the home for commercial packs.
- **End-user guide** — a task-oriented `docs/guide/` (welcome, getting started,
  the Genesis wizard, per-feature pages, the desktop app, troubleshooting),
  surfaced as a **Guide screen** in the Studio (markdown rendered in-app via
  `pulldown-cmark`) with working cross-page links.
- **Studio command palette** — `Ctrl/Cmd-K` to fuzzy-jump to any screen.
- **Deep-linking** — the active screen is mirrored in the URL hash, so screens
  are bookmarkable/shareable and survive a reload, and back/forward navigate.
- **Contextual help** — a topbar "?" that opens the Guide to the page for the
  current screen.

### Changed

- **Responsive Studio shell** — the sidebar collapses to a hamburger drawer
  below tablet width, the nav is grouped into labeled sections, and the screen
  grids stack on narrow viewports.
- **Loading skeletons** across every data-driven screen (Command Center, Memory,
  Skills, MCP, Teams, Documents) instead of flashing empty/zero states.
- **UI polish** — a distinct icon per nav item, a single daemon-status indicator
  (was duplicated), and a corrected version footer.
- **Windows** is documented as supported via WSL2 or the Docker appliance (no
  native binary yet — the daemon's IPC is Unix-socket-only).

### Accessibility

- Visible `:focus-visible` keyboard focus rings (there were none), a
  skip-to-content link, `main`/`nav` landmarks, and `aria-label`s on
  previously-unlabeled inputs and icon-only buttons.

### Fixed

- The CI quality gate is green again: install the desktop crate's GTK/WebKit
  system deps for the `--workspace` build, and clear a `clippy::type_complexity`
  lint in `aivyx-web`.

## [0.6.0] — 2026-06-21

The toolbox release. Three chapters **widen what the agent can do** without
touching the security model: a pack of exact pure-compute utilities, readers
that turn operator files into legible structured content, and the wiring that
makes the whole MCP server ecosystem an operator-config story. None adds a P10
substrate amendment or a new capability surface beyond what each tier already
allows.

### Added

- **Utilities pack (Chapter Abacus).** Five exact, deterministic helpers a
  language model is structurally bad at doing in its head, in the existing
  `aivyx-toolkit` process: **`calc.eval`** (a hand-rolled, zero-dependency
  arithmetic evaluator — `+ - * / % ^`, parens, `sqrt/abs/round/floor/ceil/
  min/max`), **`convert.units`** + **`convert.time`** (length/mass/temperature/
  volume/digital via a curated table, and IANA-timezone conversion via
  `chrono-tz`), and **`date.diff`** + **`date.add`** (calendar-correct date
  arithmetic). Three group bases (`calc.eval`, `convert.units`, `date.compute`)
  — the **first toolkit surface reachable below the Trusted tier** (SemiTrusted),
  because pure compute touches no network, filesystem, or operator data.
- **Structured-data readers (Chapter Sheaf).** Three tools in the new
  `aivyx-dataread` crate that turn a file the agent can already reach into
  legible structured content — the file analogue of `web.extract`:
  **`data.csv`** (delimited text → rows), **`data.xlsx`** (a spreadsheet's
  binary zip+XML, which `fs.read` cannot expose, via `calamine`), and
  **`data.pdf`** (a PDF's text layer via `pdf-extract`, no OCR). Each **reuses
  the existing `fs.read` capability and sandbox** — it only parses bytes the
  agent could already read, so it adds no new capability base and no new I/O
  reach (infrastructure tier, no P10 amendment).
- **MCP servers that actually work, especially keyed ones (Chapter Conduit).**
  The MCP client could connect to servers but not *authenticate* to them, and a
  misconfigured server failed silently. Conduit adds **`[[mcp_server]] env`**
  (secrets to a stdio child — the GitHub MCP server's
  `GITHUB_PERSONAL_ACCESS_TOKEN` was previously unconfigurable) and
  **`[[mcp_server]] headers`** (e.g. `Authorization: Bearer` to a remote server,
  never overriding protocol-reserved headers), both with **`${VAR}`
  interpolation** resolved from the daemon environment so secrets stay out of
  `aivyx.toml`. Stdio **stderr is now captured** (last 50 lines) instead of
  discarded, and **`aivyx mcp status`** reports each configured server as
  connected (with its tool count) or failed (with the reason and captured
  stderr). No new capability base, P10 amendment, or dependency — the
  GitHub/weather/Google-Tasks integrations are now operator config, not in-tree
  builds.

## [0.5.0] — 2026-06-21

The local tool-calling release. A small local model (an in-process GGUF on the
mistral.rs engine) can now reliably **drive the agent loop to completion** —
emitting a valid, real-named tool call by construction, then finishing its turn
instead of looping. Both chapters are **opt-in and byte-identical by default**;
no new tool, capability base, P10 amendment, or dependency.

### Added

- **Reliable local tool-calling (Chapter Stencil).** Small local models are
  fragile on the agent loop — they hallucinate tool names and emit malformed
  arguments, and four prompt-substrate phases proved it can't be fixed from the
  prompt. Stencil adds the lever the prompt can't reach: **grammar-constrained
  decoding** on the in-process mistral.rs engine. With **`[mistralrs]
  constrain_tool_calls = true`** (default off), the decoder is constrained to a
  JSON-Schema grammar built from the registered tools, so a small GGUF emits a
  valid, *real-named* tool call — or a `respond` text escape — **by
  construction**, not by hoping. Live-proven on Qwen3-4B: it called real
  `fs.read` + `memory.write` with schema-valid arguments where prior phases
  never got it to invoke a write tool at all. No new tool, capability base, P10
  amendment, or dependency; byte-identical when off.
- **Hardened local tool-calling (Chapter Bridle).** The harness that makes
  Stencil's primitive usable in the wild — a grammar that forces a valid call is
  necessary but not sufficient if the model then can't stop calling it (Stencil's
  live run watched a constrained model loop on one tool call until the deadline).
  Three guarded, default-safe fixes: **(A)** a turn-loop **repeated-call breaker**
  that stops a turn after *N* consecutive identical tool calls (default 3) with a
  distinct `[turn stopped: repeated tool call]` outcome — a generalizable safety
  net for any local model, on by default because it only fires on identical
  repeats; **(B)** a constrained-mode **`respond` preamble** that teaches the
  model how to reply in plain text and finish (the root-cause fix for the loop);
  and **(C)** an operator-configurable **`[agent] turn_timeout_secs`** so a
  legitimately slow local backend can complete a turn (unset → the 120s default,
  byte-identical). Live re-verified on Qwen3-4B: the looping scenario now ends
  cleanly — one tool call, a plain-text answer, a completed turn. No new tool,
  capability base, P10 amendment, or dependency.

## [0.4.0] — 2026-06-20

The memory + skills release. Everything below is **opt-in and byte-identical by
default** — nothing changes for an existing config until you enable it. The
agent's memory now compounds (graph-augmented recall + a knowledge-wiki + a
typed knowledge graph, all behind one `[memory] profile` switch), and its
skills now learn (they sharpen when they underperform, the agent authors new
specialized ones from its own knowledge, and a Studio Skills screen makes the
whole repertoire legible). Plus a pre-release cleanup sweep that cleared the
chapter deferrals and stabilized the test suite.

### Added

- **The Skills library (Chapter Repertoire).** A new Studio **Skills** screen
  (the thirteenth) gives the agent's skills a home: every skill — operator-
  taught, agent-authored (Praxis), agent-refined (Whetstone) — with its
  effectiveness (the WH.2 decayed EWMA as a bar + bucket label), provenance
  badge, `domain`, version, and lineage, and the full procedure on demand. A
  banner links pending skill proposals to the Agents screen (where their
  approval already lives). Read-only over a new `GetSkills` IPC; no new
  capability base, tool, or storage domain.
- **Skills authored from knowledge (Chapter Praxis).** The agent now writes its
  own **specialized skills** from what it has learned: on the reflection cadence
  it finds a topic with rich, connected knowledge (a substantial wiki page + a
  typed-graph neighbourhood) but no skill, and proposes a grounded specialized
  skill synthesized from that page + those relations (tagged with the topic's
  `domain`, agent provenance). Like Whetstone's refinements, it's a governed,
  propose-only persona proposal in the existing Agents UI. Opt-in via
  `[skill_authoring]`, off by default; reuses the wiki/graph stores — no new
  agent tool, capability base, or storage domain.
- **Skills that sharpen (Chapter Whetstone).** Skills are no longer a static
  list. Each turn folds its skill outcomes into a decayed per-skill
  effectiveness ledger, and on the reflection cadence the agent **proposes a
  refined version** of an underperforming skill — the operator's *or* its own —
  as a governed supersession pair (provenance + lineage) that surfaces in the
  existing Agents approve / edit / reject UI. Opt-in via `[skill_refinement]`,
  off by default, propose-only; no new agent tool or capability base.
- **One-switch smart memory (Chapter Synapse).** The whole memory stack above
  (graph-augmented recall + the knowledge-wiki and typed-graph layers + their
  extraction sweeps) was opt-in and spread across ~14 knobs. **`[memory] profile
  = "smart"`** now arms the coherent bundle with one line (explicitly-set knobs
  still win; the default stays `off` ⇒ byte-identical). Plus the end-to-end
  integration proof the layered arc lacked — the real memory + wiki synthesizer
  + graph extractor + `graph.query` + recall fusion, verified to compose into a
  single turn — and an operator live-verify runbook.
- **Typed knowledge graph (Chapter Lattice).** A real, **directed, typed**
  graph the agent extracts from memory: nodes are entities, edges are directed
  `(subject)-[predicate]->(object)` relations (`deploy` —*depends-on*→ `ci`),
  stored in a new encrypted `KnowledgeGraph` domain. The agent can **query** it
  with a new **`graph.query`** tool (a multi-hop typed traversal — "what depends
  on X?", "what did Y cause?"; gated by a new `graph.read` *infrastructure*
  capability base, no P10 amendment); a new **Studio "Graph" screen** browses
  the directed graph; and `recall_graph_typed_weight > 0` lets the typed
  relations steer recall along *meaningful* edges (vs. mere co-occurrence).
  Opt-in via `[graph].enabled` (a periodic extraction sweep); always derived
  from memory; zero new dependencies. **Chapter Lexicon** then gives the graph
  a **controlled relation vocabulary** — a curated set of canonical relation
  types (`depends-on`, `causes`, `part-of`, …) that synonymous predicates fold
  into, so `depends on` / `requires` / `needs` become one edge instead of three
  (applied at extraction + query, with a sweep that merges existing synonyms);
  unknown relations are kept as-is.
- **Knowledge-wiki layer (Chapter Codex).** A derived, browsable layer over
  memory: the agent consolidates each topic's entries into a **`WikiPage`** —
  an LLM-written summary plus co-occurrence **backlinks** — persisted in a new
  encrypted `KnowledgeWiki` storage domain. A new **Studio "Wiki" screen**
  browses the pages (index → summary + clickable backlinks + source-entry
  count) over read-only IPC. Opt-in: `[wiki].enabled` arms a periodic
  stale-page sweep on the maintenance cadence, and `recall_wiki_weight > 0`
  lets a page summary compete in recall as a single high-signal unit. Pages are
  always *derived* — memory stays the source of truth. Zero new dependencies.
- **`web.extract` + `git.commit` (Chapter Forge).** Two new substrate tools:
  `web.extract` returns a page's readable article text (readability over the
  existing `net.fetch` capability), and `git.commit` stages + commits in an
  operator-allowed repo (a new `git.write` capability base, Trusted-tier only,
  confirm-first). The substrate count moves 13 → 15 (**Amendment A13**).
- **`tools.list` runtime tool introspection (Chapter Atlas).** A refinement
  pass over the ~92-tool surface: a runtime `tools.list` tool, a drift-guarded
  [`docs/TOOLS.md`](docs/TOOLS.md) catalog, and a tool-metadata quality guard.
- **Permissive voice (Chapter Timbre).** Swapped the GPL Piper TTS for
  **Kokoro-82M (Apache-2.0)** + an espeak-free MIT G2P, closing the last GPL
  door (`cargo deny check licenses` clean with no copyleft exception).

### Changed

- **Graph-augmented recall (Chapter Loom).** Auto-recall now fuses three signals
  on one ranking via weighted Reciprocal Rank Fusion: **semantic** (vectors),
  **lexical** (a real BM25 scorer, replacing the old substring match), and a
  **multi-hop co-occurrence graph-walk** (the agent's topic-affinity graph,
  promoted from a passive view to an active retrieval signal). Tunable per
  source under `[embedding]` and proven by a `recall@k` eval harness; default
  off ⇒ identical to prior recall.

## 0.3.0 — source-available (BUSL-1.1) (2026-06-19)

**The headline is the license.** Aivyx moves from **MIT** to the **Business
Source License 1.1 (BUSL-1.1)**: the whole public workspace is now
**source-available** — **free for personal, individual, and non-commercial use**,
with a **paid commercial license for business or production use** — and **every
released version auto-reverts to MIT four years after it ships.** BUSL-1.1 is
source-available, *not* OSI "open source," and we no longer call it that.
**v0.2.0 and every prior release remain MIT in perpetuity** — a license can't be
revoked; the relicense applies from this tag forward. See [`LICENSE`](LICENSE),
[`COMMERCIAL.md`](COMMERCIAL.md), and [`docs/LICENSING.md`](docs/LICENSING.md)
(model + FAQ). This release also lands three breadth chapters since the Studio:
Contacts, Genesis, and the Docker appliance (Harbor).

### Changed

- **Relicensed MIT → BUSL-1.1 (Chapter Charter).** `LICENSE` is the full
  canonical BUSL-1.1 (Additional Use Grant = personal/non-commercial; Change
  Date = 4 years per release; Change License = **MIT**, preserved at
  [`LICENSES/MIT.txt`](LICENSES/MIT.txt)). A dependency-license audit (`cargo
  deny check licenses`, all-features) confirmed no copyleft poisons the combined
  work. Every "open source"/"MIT" claim about Aivyx's own code is now
  "source-available under BUSL-1.1" (README, TRADEMARK, and DESIGN.md's
  open-core deliverable via **Amendment A14**).
- **Contributing now requires a CLA** ([`CONTRIBUTING.md`](CONTRIBUTING.md) +
  [`CLA.md`](CLA.md)), accepted via a `git commit -s` sign-off — necessary so the
  free + commercial dual model can lawfully cover contributed code.

### Added

- **Google Contacts (Chapter Contacts).** A new `aivyx-contacts` tool process
  (People API) with six tools over `contacts.read`/`contacts.write`
  (`contacts.search`/`list`/`get`/`create`/`update`/`delete`); connect with
  `aivyx connect contacts`. The fifth Google integration on the substrate
  pattern. See [`docs/CONTACTS.md`](docs/CONTACTS.md).
- **Unified agent creation (Chapter Genesis).** One onboarding flow across CLI
  and web: the Profile drafter is lifted into the daemon (`DraftProfile` IPC)
  and a "Create your agent" flow (Profile → Persona → access) lands in the
  Studio. See [`docs/ONBOARDING.md`](docs/ONBOARDING.md).
- **Docker server appliance (Chapter Harbor).** A `docker-compose` deployment of
  the daemon + Studio in a container (distinct from the desktop local-first
  install), with two opt-in web-exposure knobs (`web_ui_host`,
  `web_ui_allowed_origins`); CI publishes the image to GHCR. See
  [`docs/DOCKER.md`](docs/DOCKER.md).
- **Tool-call rate limits & quotas (Chapter Throttle).** A third dispatch gate —
  after the capability + role gates, before execute — bounds *how often* tools
  run, the sibling of Chapter K's dollar budgets for call counts. Opt-in
  `[rate_limit]` config sets per-turn-per-tool, per-turn-total, and per-tool
  sliding-window caps with an `alert` (warn + proceed) or `deny` (block) action;
  a throttled call yields a forensically-distinct `ToolOutcome::RateLimited` and
  a dedicated `RateLimited` audit record (separate from capability/role denials).
  Bounds a runaway turn — autonomous loop, Nonagon mission, or interactive — from
  hammering `web.fetch` / `shell.exec`. Uncapped by default, so existing configs
  are unchanged. See [`docs/RATE_LIMITS.md`](docs/RATE_LIMITS.md); closes backend
  audit finding **F2**.

### Fixed

- **Storage-domain count corrected (21, was documented as 20)** — the Phase-183
  `Reminders` `KeyDomain` was never propagated to the docs. Added a
  `key_domain_count_matches_docs` drift-guard test (audit **F7**).

### Security

- **The Studio `/ws` bridge now enforces an `Origin` check** (closes a
  Cross-Site WebSocket Hijacking / DNS-rebinding vector). A loopback bind is
  not a boundary against the browser — WebSockets are exempt from the
  same-origin policy — so a malicious page the operator visited could
  otherwise open `ws://127.0.0.1:7843/ws` and drive the already-unlocked
  daemon, which since the Studio's write screens can change config and the
  filesystem. The `/ws` upgrade is now accepted only when `Origin` is absent
  (a non-browser client, already inside the trust boundary) or exactly matches
  a loopback origin on the bound port; cross-site, rebinding, wrong-port, and
  `null` origins are rejected with `403`. See `THREAT_MODEL.md` §4.11.

## 0.2.0 — the Studio (2026-06-16)

The headline is the **Studio** — the daemon's web GUI grew from two tabs into a
**complete** local-first mission-control surface, one screen per chapter (R–Z
plus Voice), every one live-verified in a real browser with its WASM bundle
committed. The entire screen inventory is now live: Command, Missions, Chat,
Memory, Settings, Agents, Teams, Documents, Voice.

### Added

- **The Studio web GUI is complete.** Every screen is live, offline, and
  Stitch-styled, served from the daemon's embedded bundle on `:7843`:
  - **Command Center** (S) — the default dashboard: stat cards, active missions,
    a live audit-trail feed, agent status.
  - **Memory** (T) — topic rail + entry cards + keyword/semantic search over the
    self-learning memory, **plus a knowledge-graph view** (MG): a real weighted
    graph (nodes = topics, edges = the co-occurrence ledger's pair scores) laid
    out in-WASM with a deterministic force-directed simulation; click a node to
    filter its entries.
  - **Settings** (U) — the **first config-write surface**: edit the access level
    (confirm-first, enforced server-side) and budgets; section-scoped `toml_edit`
    rewrites with a `ConfigChanged` audit and an honest "restart to apply".
  - **Agents** (V) — a direct **Profile** editor plus the self-learned **Persona**
    governance loop (approve / edit / reject proposals, revert deltas), live.
  - **Teams** (Y) — a read-only view of the active **Nonagon** roster.
  - **Documents** (Z) — a **file browser and editor** over the agent workspace and
    the access-scoped `fs_root`: read files, and (DW) edit + save, create
    files/folders, rename, and delete.
  - **Voice** — a `[voice]` config editor with a daemon-side readiness check
    (ASR/TTS model + espeak data: present / missing / unset) and the launch
    command. Voice itself is a host-local CLI loop (`aivyx --channel voice`), so
    the screen configures it rather than doing browser audio.
- **Onboarding Persona/Skills seed (Chapters W–X).** The end user can give the
  agent a starting Persona + Skills — by hand or by **describing it in words**
  (the model drafts it) — at first launch (`aivyx init` → `[persona_seed]`,
  planted on the signed chain at boot iff empty, adopted turn-one) or live from
  the Studio. See [`docs/PERSONA_SEED.md`](docs/PERSONA_SEED.md).

### Security

- **Documents browsing never escapes its root.** `ListDir`/`ReadFile` reuse the
  fs tools' lexical-resolve → canonicalize → `starts_with(root)` guard, so `..`
  and symlink escapes are rejected over IPC; reads are size-capped + binary-aware;
  the `fs` root is exactly the operator-granted access level.
- **Editable Documents is the most safety-sensitive surface, and gated to match
  (DW).** Writes canonicalize the *parent* dir (a new file can't be canonicalized)
  and re-check `starts_with(root)`; `WriteFile` carries an explicit `overwrite`
  flag (no accidental clobber); rename/mkdir refuse existing targets; `DeleteFile`
  is **empty-only, never recursive, and always requires `confirm: true`** (the web
  shows a confirm modal). Writes are atomic (temp-file + rename), and **every
  mutation is recorded as a signed `DocumentMutated` audit event.**

### Fixed

- The `web_asset_lookup` unit test asserted the bundle was *absent*, which has
  been false since the bundle became committed in Chapter R; rewritten for the
  shipped reality. The full workspace test suite is green (4,666 passing).

## 0.1.0 — first public pre-release (2026-06-13)

**This is an early pre-release.** Aivyx is a capable, actively-developed personal
agent, but `0.1.0` is its first published binary and has not yet had external
users. Expect rough edges, breaking changes between releases, and gaps in the
docs. Try it, file issues — but don't depend on it for anything critical yet.

### What Aivyx is

A local-first, capability-secured personal AI agent that runs as a daemon on
**your** machine. Its headline path is **"runs on your hardware, no API key"**
via [Ollama](https://ollama.com) — cloud providers (Anthropic, OpenAI) are
optional. The agent has a persistent, self-learning Profile/Persona/Soul, an
HMAC-chained audit log of everything it does, and a capability/trust-tier model
that bounds exactly what it can reach.

### Highlights in this release

- **Local-first on-ramp that just works.** Pick Ollama in `aivyx init` and you
  get a working, tool-using first conversation with **zero manual config**:
  context length is auto-detected from the model (no more starved `num_ctx`
  one-token replies), the wizard recommends and offers to download a vetted
  tool-capable model, and `aivyx doctor` verifies the whole path end to end.
- **Operator-chosen access levels.** *You* decide how far the agent reaches —
  from a sandbox, to your home directory, to full-machine access — via
  `aivyx access` and the init wizard. Irreversible filesystem operations are
  confirm-first; remote channels are automatically attenuated.
- **The agent's own workspace.** A private, always-available directory
  (`~/.aivyx/workspace`) the agent uses for its own thoughts, ideas, plans, and
  projects, with optional proactive journaling.
- **Multi-agent teams in the daemon.** Goal → plan → delegated execution with a
  live TUI feed and human approval gates.
- **A browser Mission-Control GUI.** A Rust→WASM (Dioxus) web client that speaks
  the daemon's wire protocol directly — Missions and Chat in the browser.
- **Productivity integrations.** Gmail, Google Calendar, Google Drive, Notion,
  Obsidian, n8n, and a web/task/health toolkit, each as an isolated tool process
  with operator-provided OAuth.

### Install

```sh
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/Aivyx-Agent/aivyx/releases/download/v0.1.0/aivyx-cli-installer.sh | sh
```

Then run `aivyx init` to set up your agent. See `docs/INSTALL.md` for building
from source and `docs/LOCAL_FIRST_RUN.md` for the local-model path.

### Platforms

Prebuilt binaries for Linux (x86_64, aarch64; musl-static) and macOS (x86_64,
aarch64). Windows is not yet supported (the daemon currently uses Unix domain
sockets). All platforms can build from source.
