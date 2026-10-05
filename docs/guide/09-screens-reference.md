# The Studio screens at a glance

A quick reference to every screen in the sidebar. Several have their own detailed
page in this guide; this is the map.

| Screen | What it's for |
|---|---|
| **Create** | The guided setup that gives your assistant its identity (see [Create your agent](03-create-your-agent.md)). |
| **Command** | Your logbook — an instrument strip, what needs you, what your assistant did since you were last here, and what's coming up. |
| **Missions** | Hand off larger multi-step jobs and watch them run; approve gates (see [Chat & missions](04-chat-and-missions.md)). |
| **Mission Control** | A live view of one running mission: its lead and specialists, drill-in, and controls to approve, reject, pause, resume or abort. |
| **Schedules** | Routines that run on a timer — yours, your config's, and ones the assistant proposes for your approval. |
| **Notifications** | Where your assistant can reach you outside the Studio, and what it has sent. |
| **Loop** | The autonomous backlog: stories the assistant works through on its own, and each run's progress. |
| **Reminders** | Reminders you or the assistant have set. |
| **Chat** | A direct conversation with your assistant. |
| **Memory** | Browse and search everything your assistant has learned, including its Learning digest (moved here from the Command Center). |
| **Wiki** | Your assistant's knowledge as readable per-topic pages (needs the smart memory profile). |
| **Graph** | Your assistant's knowledge as a map of things and the named relationships between them (needs the smart memory profile — see [Memory](05-memory.md)). |
| **Settings** | Access level, autonomy and spending budgets (see [Access & settings](08-access-and-settings.md)). |
| **Agents** | Edit the Profile and approve the character changes your assistant proposes (see [Skills & personality](06-skills-and-persona.md)). |
| **Skills** | The library of things your assistant has learned to do. |
| **Teams** | View and edit your team of specialists (see [Teams](07-teams.md)). |
| **Documents** | A file browser over the folders your assistant can access. |
| **MCP** | The status of any external tool servers you've connected. |
| **Tools** | A searchable catalog of every tool your assistant has, with the permission each one needs. |
| **Audit** | The tamper-evident log of everything the assistant did. |
| **Sessions** | Every open conversation — which channel, its trust level, when it was last active. |
| **Gallery** | Browse images your assistant has generated through a connected ComfyUI server. |
| **Models** | Which models routing can choose from, what's loaded, and which one this conversation uses — with a pin control (see [Models & routing](12-models-and-routing.md)). |
| **Voice** | Set up talking to your assistant out loud. |
| **Guide** | This guide. |

## Command Center

The landing screen, and it works like a logbook rather than a dashboard:
instead of stat cards and panels, it's one readable column written by the
assistant in its own voice, in this order:

- **Instrument strip.** A ruled line of readings — daemon status, the model
  and its context window, today's spend against your budget (24 h,
  rolling), memory topic count, and whether the audit chain is sealed or
  broken. Click any reading to jump to the screen it belongs to; a reading
  turns warn or rust when something needs attention (offline, spend at 80%
  or more of budget, a broken chain).
- **Greeting.** A time-of-day headline, then the date and when you were last
  active — "Wednesday 1 October · last here 9 h ago".
- **Needs you.** Everything waiting on you: your own pending chat approval,
  mission and team-mission gates (Approve / Deny), pending persona and
  skill proposals (Review), failed routines and failed notifications,
  sources the daemon couldn't read, and reminders coming up in the next 2
  hours, soonest first with how long until they're due (Done / Snooze 1 h).
  A routine that failed several times is one card ("failed 5 times").
  "Nothing needs you." when there's nothing.
- **Since you were last here.** Up to 12 lines of what your assistant did,
  oldest first, each with a time and a plain sentence. The heading reads
  "In the last 24 hours" until there's a previous visit to measure from
  (you've done something here, then been away 30 minutes or more), and "In
  the last 7 days" if you've been away longer than a week; otherwise it's
  "Since you were last here" — and "last here" means the end of your
  *previous* visit, not just now, so acting on a card here never empties
  the log you're reading. A trailing "and N more → Audit" covers anything
  past the 12-line cap.
- **Coming up.** The next three scheduled routines and up to three pieces of
  work already in progress ("and N more in progress." past that).

The log is written straight from the record with fixed wording — no model
is involved, ever. That also means it can't tell you *which* files changed:
the audit trail keeps only a hash of each call's arguments, so a line can
say "I made 3 changes" but never name them. Every line links to the screen
that holds the full detail (Missions, Audit, Memory, Notifications).

## Documents

A file browser scoped to exactly what your assistant can reach (set by your
access level). Browse folders, open files, and make edits. Anything destructive —
deleting a file, overwriting one — is confirmed first, and every change is
recorded.

## Voice

Aivyx PA can talk and listen. Voice runs as a local loop on your own machine — your
audio is processed locally, not streamed to a cloud service. The Voice screen is
where you configure it and see whether it's ready; you start a voice session from
the command line.

## MCP servers

MCP is an open standard for plugging external tool servers into an assistant —
things like a GitHub connector, a database, or a web-search service. You add
servers in your config file; the **MCP** screen shows each one's status: whether
it connected, how many tools it offers, and any errors. This is how you extend
your assistant with capabilities beyond what ships in the box.

## Gallery

If you run [ComfyUI](https://github.com/comfyanonymous/ComfyUI) locally, your
assistant can generate images when you ask it to, and this is where you see the
results. Each image shows the prompt that produced it and when it was made,
newest first; click one to view it full-size.

To connect one, add an MCP bridge server (for example
[`comfyui-mcp-server`](https://github.com/joenorton/comfyui-mcp-server)) as an
`[[mcp_server]]` entry named `comfyui` in your config file:

```toml
[[mcp_server]]
name = "comfyui"
command = "/path/to/comfyui-mcp-server/venv/bin/python"
args = ["/path/to/comfyui-mcp-server/server.py", "--stdio"]
env = { COMFYUI_URL = "http://localhost:8188" }
```

Restart the daemon and the Gallery screen picks it up automatically. If no
`comfyui` server is configured, the screen just explains how to add one —
there's nothing else to set up here.
