# Autonomy and routines

Your assistant doesn't have to wait for you to type. It can run routines on
a timer, react to a webhook or a changed file, remind you of things, reach
out when something matters, and work through a backlog on its own. This
page covers all of that, and the dial that governs it.

## The autonomy dial

Autonomy is how much your assistant does without asking you. Set it on
**Settings → Autonomy** or with `aivyx-pa autonomy set <level>`:

| Level | What it means |
|---|---|
| **Manual** | Every change it makes asks you first. It can still look things up. |
| **Assisted** *(default)* | Only irreversible steps ask. |
| **Supervised** | The autonomous loop may run (see below). |
| **Autonomous** | As supervised, and routines it creates start without your approval. |
| **Unleashed** | For a dedicated, isolated machine: as autonomous, and deletes and overwrites stop asking — unless you've set `confirm_destructive` under `[access]` yourself. |

Raising it to *autonomous* or *unleashed* asks you to confirm. The assistant
can never raise its own level.

**Different levels for different areas** are on the way. The config file
already accepts per-area overrides, but today only one area is applied:
`schedules`, which decides whether routines the assistant creates need your
approval. For example, to let it set up routines freely while keeping
everything else at *assisted*:

```toml
[autonomy]
level = "assisted"

[[autonomy.override]]
domain = "schedules"
level = "autonomous"
```

### Unattended work never waits

When nobody is there to answer — a routine, a webhook, the autonomous loop,
a team mission — an irreversible step is **refused**, not taken and not
left hanging. The run says what it would have needed, and you can do it
yourself or ask for it in a conversation.

## Routines

A routine runs a prompt for you on a schedule. Setup offers a few starter
routines when you use a local model (they cost nothing to run): a daily
look at what's changed, a nightly reflection that tidies memory, a health
check every six hours, and a Monday digest. On a paid provider they're
written to your config but left off, so they never spend money unasked.

Manage routines on the **Schedules** screen: create one, switch it on or
off, delete it. The assistant can also propose routines itself ("shall I
check the build every morning?"); below *autonomous*, those wait for your
approval on that screen.

In the config file a routine looks like this:

```toml
[[schedule]]
name = "morning-summary"
cron = "0 0 8 * * *"        # 08:00 every day
role = "default"
prompt = "Summarise my calendar and unread mail for today."
notify_targets = ["phone"]
notify_when = "on_completed_non_empty"
```

The `cron` pattern starts with **seconds**: `0 0 8 * * *` is 08:00:00 daily,
in your local time. `notify_when` decides whether you hear about it:
`always`, `on_failed`, or `on_completed_non_empty` (only when there's
something to say).

### Webhooks and file watches

Two more ways to trigger a prompt, configured in the file (the assistant
can also create them, with its `webhook.*` and `file_watch.*` tools):

- **`[[webhook]]`** — runs when something posts to
  `http://127.0.0.1:<port>/trigger/<id>` on your machine; handy for linking
  other tools to your assistant.
- **`[[file_watch]]`** — runs when a file or folder changes.

## Notifications

Routines, reminders and the assistant itself reach you through **notify
targets** — named places a message can go:

- a **Telegram** chat,
- an **email** address (using your `[email]` settings),
- a **webhook** (ntfy, a home-automation hub, anything that accepts a POST),
- a **desktop notification** from the open Studio.

```toml
[[notify_target]]
name = "phone"
kind = "telegram"
chat_id = "123456789"
default = true
```

The **Notifications** screen lists your targets and everything that was
sent; `aivyx-pa notify history` shows the same in the terminal.

## Reminders

Ask in plain words — "remind me to call the plumber at 4pm" — and the
assistant sets a one-off reminder. When it's due, it arrives through your
notify targets and shows on the Command Center. Reminders you or it set are
on the **Reminders** screen. A reminder that fell due while the assistant
was off arrives as soon as it's back.

## Reaching out first

With **proactive surfacing** on (Settings, or `[proactive]` in the config),
the assistant may message you unprompted — but only for a concrete reason
(a reminder that's due, a memory about to expire, a topic that keeps proving
useful) and at most a few times a day.

## The autonomous loop

The loop works through a backlog of *stories* on its own, one fresh attempt
at a time, until the backlog is empty or a limit is hit. It's meant for
longer jobs you can describe up front.

1. Turn it on: add a `[loop]` section with its limits (below), and either
   set autonomy to *supervised* or above or put `enabled = true` in it.
2. Add stories: `aivyx-pa loop add "Write tests for the parser"`.
3. Start it: `aivyx-pa loop start`, or **Start** on the **Loop** screen.
   Watch with `aivyx-pa loop status`, `aivyx-pa loop log` or the Loop
   screen; stop with `aivyx-pa loop stop` or **Stop**.

```toml
[loop]
enabled = true
max_iterations = 20       # hard stop
max_run_usd = 2.00        # spending stop
gate_command = "cargo test"   # what "green" means, if you have a test suite
```

Every run stops at its first limit — iterations, time, tokens, dollars, or
several attempts in a row that get nowhere — and, like every unattended
run, refuses irreversible steps rather than waiting for you.

## Budgets keep it honest

Anything that runs on its own can spend money on a paid provider. Set caps
on **Settings → Budget** (per run and per day); the loop has its own
`max_run_usd`. See [Access and settings](08-access-and-settings.md).
