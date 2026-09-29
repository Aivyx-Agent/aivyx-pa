# The Studio screens at a glance

A quick reference to every screen in the sidebar. Several have their own detailed
page in this guide; this is the map.

| Screen | What it's for |
|---|---|
| **Create** | The guided setup that gives your assistant its identity (see [Create your agent](03-create-your-agent.md)). |
| **Command** | Your home dashboard — at-a-glance status, active missions, a live activity feed, and whether the assistant is busy. |
| **Missions** | Hand off larger multi-step jobs and watch them run; approve gates (see [Chat & missions](04-chat-and-missions.md)). |
| **Mission Control** | A live view of one running mission: its lead and specialists, drill-in, and controls to approve, reject, pause, resume or abort. |
| **Schedules** | Routines that run on a timer — yours, your config's, and ones the assistant proposes for your approval. |
| **Notifications** | Where your assistant can reach you outside the Studio, and what it has sent. |
| **Loop** | The autonomous backlog: stories the assistant works through on its own, and each run's progress. |
| **Reminders** | Reminders you or the assistant have set. |
| **Chat** | A direct conversation with your assistant. |
| **Memory** | Browse and search everything your assistant has learned. |
| **Wiki** | Your assistant's knowledge as readable per-topic pages. |
| **Graph** | The same knowledge as a visual map of connected topics. |
| **Settings** | Access level and spending budgets (see [Access & settings](08-access-and-settings.md)). |
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

The landing dashboard. It pulls together the things you most often want to glance
at — how many missions are active, recent activity, the assistant's current
status, and the integrity of the audit log — without you having to open each
screen.

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
