# Default starter routines

*Since v0.7.1.*

When you run `aivyx-pa init`, the wizard plants a small set of **scheduled
routines** — recurring background tasks the agent runs on its own so a fresh
agent orients itself and stays useful unattended. They are ordinary
[`[[schedule]]`](#the-schedule-primitive) entries in your `aivyx-pa.toml`; nothing
is hidden, and you can edit, disable, or delete any of them.

> Design principle: a routine runs **unattended**, so every default routine is
> **read-only / observational**, scoped to your access level, and its prompt
> explicitly forbids destructive actions. Routines never delete, overwrite, or
> run destructive commands, and never send your data anywhere.

## The routines

| Name | Cadence (default) | What it does | `notify_when` |
|---|---|---|---|
| `environment-review` | daily 07:00 | **Self-bootstrapping.** First run surveys your accessible environment (workspace, key directories, available tools, a brief system summary) and saves a baseline to memory under `environment-baseline`. Later runs diff against it and journal anything new or notable. | `on_completed_non_empty` |
| `nightly-reflection` | nightly 02:00 | Consolidate the day's memory into the knowledge base, tidy stale/duplicate entries, note skills or preferences worth refining, and journal a short reflection. The "growth" cadence. | `on_completed_non_empty` |
| `health-check` | every 6h | Confirm the model responds and a trivial tool call works. Silent on success; raises only when something is wrong. | `on_failed` |
| `weekly-digest` | Monday 08:00 | Summarize the week's learnings and list any pending persona/skill proposals awaiting your review. | `on_completed_non_empty` |
| `trend-scan` *(opt-in)* | daily 07:30 | Research your interest areas on the web, cross-reference, separate fact from speculation, save findings to memory, and journal a short digest. Requires web search. | `on_completed_non_empty` |

### First-boot orientation

A brand-new schedule has no `last_fired_at`, so it **fires once on the first
daemon start** and then settles onto its cron cadence. This is deliberate: the
agent orients itself (builds its `environment-baseline`, runs an initial pass of
each routine) the moment it first comes up, rather than waiting until the next
scheduled time.

## Enablement (which routines are on)

Routines are gated by the answers the wizard already collects, because a
scheduled run on a **cloud** provider silently spends tokens:

| Provider | Four core routines | `trend-scan` |
|---|---|---|
| **Ollama (local)** | **enabled** (runs are free) | enabled **iff** you enabled web search |
| **Anthropic / OpenAI (cloud)** | written **present-but-disabled** | written disabled |

On a cloud provider the routines are still written to `aivyx-pa.toml` so they're
discoverable — flip `enabled = true` on any you want (and mind your
[`[budget]`](COST_GOVERNANCE.md)). The wizard prints a one-line summary of what
it set up, so this is never surprise behavior.

## The `[[schedule]]` primitive

Each routine is a standard schedule entry. Note the table name is **`schedule`**
(singular):

```toml
[[schedule]]
name = "environment-review"
# cron is 6-field, SECONDS-first: "sec min hour day-of-month month day-of-week"
cron = "0 0 7 * * *"        # 07:00:00 every day
role = "default"
prompt = "…a self-contained, read-only instruction…"
enabled = true
wrap_mission = true          # run as a mission (visible in Missions / the TUI feed)
notify_when = "on_completed_non_empty"   # always | on_failed | on_completed_non_empty
```

- **`cron`** — 6 fields, seconds first (`cron` crate syntax). `0 0 7 * * *` is
  7am daily; `0 0 */6 * * *` is every six hours; `0 0 8 * * Mon` is Monday 8am.
  Prefer day names: numbered days count from Sunday = 1, so `1` is *Sunday*.
- **`role`** — which role the routine runs as (its tools/scopes).
- **`notify_when`** — `always`, `on_failed`, or `on_completed_non_empty`.

### Scheduling a team mission instead of a single-agent turn

A schedule can target a **team mission** (the Nonagon) instead of a
single-agent turn — mutually exclusive with `role`/`prompt`. Every
top-level key (`enabled`, `notify_targets`, ...) still works the same
way; it just has to come *before* the `[schedule.team_mission]` line,
same as any TOML sub-table:

```toml
[[schedule]]
name = "nightly-boh-close"
cron = "0 0 2 * * * *"        # 02:00:00 every day
enabled = true
notify_targets = ["studio"]
[schedule.team_mission]
goal = "Run end-of-day BOH close."
# pack_config = "crates/verticals/aivyx-kitchen/assets/kitchen-boh.toml"  # optional -- absolute, or relative to the daemon's own working directory. Omit for the daemon's default team.
```

The mission runs exactly like a manually-run `aivyx-pa team run` — if it
hits a human gate, it parks in `AwaitingApproval` (visible in Mission
Control / `aivyx-pa team status`) rather than firing headless; a mission
started this way is tagged with the schedule that started it. `enabled`/
`notify_target`/`notify_targets` all work the same as a normal schedule
(as long as they're placed above `[schedule.team_mission]`, per the
example above). `notify_when`'s condition (`on_failed`/
`on_completed_non_empty`) is **not yet meaningful** for a team-mission
schedule -- every notify-worthy phase (a gate parking, or a terminal
outcome) notifies regardless of the condition set here; only a genuine
start failure (a bad `pack_config` path, or the goal failing to
decompose) is unconditional today.

Can also be created conversationally by asking the agent to schedule a
team mission — the agent's own `schedule.create` tool accepts `goal`
(never `pack_config`, which is operator-only, set via `aivyx-pa.toml`
directly) as an alternative to `prompt`.

### A third way to start one: `/team run <goal>` from chat

Besides `aivyx-pa team run "<goal>"` (CLI) and a `[schedule.team_mission]`
routine (above), a team mission can also be started **on demand from
Telegram/Discord/Slack** by typing `/team run <goal>` in an opted-in
channel — no daemon restart or config edit needed per-mission, unlike a
schedule. The operator opts a channel in with `team_run_channel = true`
in that channel's config (`[telegram]`/`[discord]`/`[slack]` in
`aivyx-pa.toml`; default `false` — off everywhere until set), and an
optional `team_trigger_rate_limit` caps how many confirmed starts one
chat can trigger per rolling hour. Every `/team run` is confirm-first:
the bot asks "Start '<goal>' on the default team? Reply yes/no." and
only starts the mission on a bare "yes" within 5 minutes, and only a
"yes"/"no" from the same sender who typed `/team run` in the first
place is honored — a different sender's reply is silently ignored (see
below). On top of `team_run_channel`, the sender must also be on that
channel's `team_command_allowed_senders` list (a separate,
deny-by-default gate covering `/team run` and every other `/team`
command — see `docs/INSTALL.md`'s `/team` section, including its
upgrade note). See `docs/DAEMON_TEAMS.md` §6 (Chat surface) for the
full command set and `docs/INSTALL.md`'s Telegram/Discord/Slack
sections for the config knobs.

## Editing, disabling, adding

- **Disable one:** set `enabled = false` on its `[[schedule]]` block and restart
  the daemon.
- **Re-time one:** change its `cron`.
- **Add your own:** append a new `[[schedule]]` block, or have the agent create
  one with the `schedule.create` tool.
- The daemon **syncs config schedules into its store on boot** (it logs
  `synced N schedule(s) from config`); an already-synced schedule keeps its
  `last_fired_at`, so a restart does not re-fire it.
- `schedule.create`/`.update`/`.delete` are operator/interactive-only: they
  refuse when called from within a scheduled or triggered run (cron, webhook,
  file-watch, reflection, the autonomous loop, or any team mission started
  from within such a run), so an unattended run can't recursively create more
  automation. Editing `aivyx-pa.toml`'s `[[schedule]]` blocks directly is
  unaffected.

## See also

- [AUTONOMY.md](AUTONOMY.md) — the autonomy dial that gates how freely the agent acts.
- [COST_GOVERNANCE.md](COST_GOVERNANCE.md) — budgets (relevant when enabling routines on a cloud provider).
- [ONBOARDING.md](ONBOARDING.md) — the first-run wizard that plants these.
