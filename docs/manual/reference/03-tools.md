# Tools

Every tool Aivyx PA can give the agent, with the description the model
itself reads. This page is generated from the code by
`scripts/gen-tools-reference.py` — edit the tool, not this page.

## How the agent gets tools

- **Built in** — file, shell, web, memory, workspace and the agent's own
  machinery. Always there; what each may touch is set by your
  [access level](../../guide/08-access-and-settings.md).
- **Integrations** — separate, sandboxed tool processes. Gmail, Calendar,
  Drive and Contacts are set up with `aivyx-pa connect <name>`
  ([Chat apps and accounts](../../guide/13-chat-apps-and-accounts.md)); the
  others (Notion, Obsidian, n8n, the toolkit, images) are added as a
  `[[tool_process]]` in your config.
- **MCP servers** — any `[[mcp_server]]` in your config. Their tools keep
  the server's own names and need the `mcp.call:<server>:<tool>` capability.
- **Your own tool processes** — any `[[tool_process]]`, in any language
  ([Tool processes](../developer/04-tool-processes.md)).

## Who decides what runs

Each tool call needs a **capability** (its scope). A role grants
capabilities; the channel's **trust tier** caps them, so a chat app can
never get more than its tier allows. Before anything irreversible —
deleting, overwriting, sending, a destructive shell command — the agent
asks you, according to your [autonomy level](../../guide/15-autonomy-and-routines.md)
and `[access] confirm_destructive`. Every call is recorded in the audit log.

For each tool's capability scope and lowest trust tier, see
[`docs/TOOLS.md`](https://github.com/Aivyx-Agent/aivyx-pa/blob/main/docs/TOOLS.md).
`aivyx-pa --print-role` shows what the active role can use, and the agent
can list its own tools with `tools.list`.


## Built in

Always available, subject to your access level and the role's capabilities.

| Tool | What it does |
|---|---|
| `fs.delete` | Delete one of the operator's files, a symlink, or an empty directory under the agent's sandbox root. |
| `fs.metadata` | Inspect a file or directory under the agent's sandbox root: size, kind, last-modified time, and permissions. |
| `fs.read` | Read a UTF-8 (or binary) file from under the agent's sandbox root. |
| `fs.write` | Write a UTF-8 file — the operator's files, under your fs root — atomically. |
| `git.commit` | Stage the given paths and create a commit in a configured repo. |
| `git.diff` | Run `git diff` against a configured repo path. |
| `git.status` | Run `git status --porcelain` against a configured repo path. |
| `net.dns` | Resolve a hostname to one or more IP addresses. |
| `role.switch` | Switch the agent into a child role for a bounded sub-session. |
| `routing.explain` | Explain which model the router last chose for a conversation, and why. |
| `routing.status` | Show the model router's candidates. |
| `shell.exec` | Run a shell command inside the agent's shell.exec sandbox root and return its stdout, stderr, and exit code. |
| `skill_defaults.list` | List every default skill from the shared skill library (compiled-in defaults plus any configured project/user overlay directories). |
| `skill_defaults.read` | Read the full body of one default skill from the shared skill library. |
| `skills.invoke` | Render the full procedure body of one operator-approved learned skill. |
| `skills.list` | List every operator-approved learned skill. |
| `web.extract` | Fetch a web page and return its readable article text (title + clean body), not raw HTML. Use this to *read* a page; use web.fetch for raw bytes. |
| `web.fetch` | Fetch an HTTP or HTTPS URL via GET and return its status code and body. UTF-8 bodies are streamed to the user as they arrive; non-UTF-8 (binary) bodies are returned base64-encoded with body_encoding set to "base64". |
| `web.post` | Send an HTTP request with a write verb (POST, PUT, PATCH, DELETE) to a URL and return its status code and body. UTF-8 bodies are returned directly; binary bodies are base64-encoded. |
| `workspace.delete` | Delete a file or empty directory from YOUR private notebook workspace (the operator's files are deleted with `fs.delete`). |
| `workspace.list` | List YOUR workspace (or a sub-path). |
| `workspace.note` | Append a timestamped entry to your journal (or another bucket) — the quick way to jot a thought, idea, or plan. |
| `workspace.read` | Read a file from YOUR workspace (your own private space for notes, ideas, plans, and projects). |
| `workspace.write` | Write (create or overwrite) a file in YOUR private notebook workspace — for your own notes and drafts, not the operator's files (use `fs.write` for a file they ask for). Parent dirs are created as needed. |

## Memory

Long-term memory. See [Memory](../../guide/05-memory.md).

| Tool | What it does |
|---|---|
| `memory.forget` | Delete every memory entry under a topic. |
| `memory.read` | Recall recent memory entries stored under a given topic. |
| `memory.search` | Substring-search memory entries by topic + body. |
| `memory.write` | Store a new memory entry under a topic. |

## Documents and spreadsheets

Read and write CSV, Excel and PDF files, within the same file access as `fs.read` / `fs.write`.

| Tool | What it does |
|---|---|
| `data.csv` | Read a CSV / delimited-text file from under the agent's sandbox root into structured rows. |
| `data.pdf` | Extract the text layer from a PDF under the agent's sandbox root. |
| `data.pdf.write` | Lay plain text out into a new PDF under the agent's sandbox root. |
| `data.xlsx` | Read an .xlsx spreadsheet from under the agent's sandbox root into structured rows. |
| `data.xlsx.write` | Write structured rows into a new .xlsx spreadsheet under the agent's sandbox root. |

## Agent machinery

Missions, routines, reminders, skills, reflection and the like — the agent managing its own work.

| Tool | What it does |
|---|---|
| `file_watch.create` | Create a new file-watch trigger. The watch fires when files at the specified path are modified. |
| `file_watch.delete` | Delete a file-watch trigger by ID. The watch will no longer fire. |
| `file_watch.list` | List all file-watch triggers. |
| `graph.query` | Query the agent's typed knowledge graph: a read-only multi-hop traversal over the directed (subject)-[predicate]->(object) relations extracted from memory. |
| `loop.complete` | Mark an autonomous-loop backlog story as Done. |
| `loop.next` | Return the next pending story from the autonomous-loop backlog (lowest priority number first, then insertion order). |
| `loop.note` | Append a one-line learning to the autonomous-loop progress log. |
| `memory.gc` | Garbage-collect a memory topic by evicting the oldest entries that exceed a maximum count. |
| `mission.create` | Create a new long-running mission. |
| `mission.list` | List all missions. |
| `mission.status` | Get the full status of a mission by ID. |
| `notify.send` | Push a notification message to an operator-configured target (Telegram chat, generic webhook, email, or the Studio). |
| `ollama.list` | List locally available Ollama models. No input parameters. |
| `ollama.pull` | Pull (download) a model from the Ollama registry. |
| `ollama.show` | Show metadata for an Ollama model. |
| `reflection.apply` | Apply an approved reflection proposal. Takes the proposal_id from reflection.propose, verifies the associated mission gate is approved, then executes the proposed memory writes. |
| `reflection.propose` | Analyze recent turn outcomes and propose behavioral adjustments. Reads the audit chain to identify patterns (repeated failures, timeouts, denied scopes) and emits a structured proposal with memory writes. |
| `remind.cancel` | Cancel a pending reminder by id. |
| `remind.list` | List pending reminders, soonest first. |
| `remind.set` | Set a one-shot reminder. |
| `role.update` | Modify the agent's runtime role configuration. Can append to the system prompt and add/remove tools from the active allowlist. |
| `schedule.create` | Create a new cron-triggered schedule. When the schedule fires, the daemon submits the prompt as a turn under the specified role. |
| `schedule.delete` | Delete a cron schedule by ID. The schedule will no longer fire. |
| `schedule.list` | List all cron schedules. |
| `schedule.update` | Update an existing schedule. Provide the schedule_id and any fields to change: enabled (true/false), cron expression, prompt, or role. |
| `skills.forget` | Forget (remove) a skill. Confirm with the operator first; call with `confirmed: true` only after they approve. |
| `skills.teach` | Save a new skill the operator taught you. First draft it and show name + when-to-use + steps to the operator; only call this with `confirmed: true` after they approve. |
| `skills.update` | Refine an existing skill (trigger and/or procedure), keeping its name. Draft the change, show it, and only call with `confirmed: true` after the operator approves. |
| `team.run` | Delegate a goal to a durable, daemon-run agent team (the Nonagon). |
| `tools.list` | List the agent's own tools (name + description; set detail=true for input schemas). Use this to know exactly which tools exist instead of guessing. |
| `turn.history` | Query recent turn outcomes from the audit chain. |
| `webhook.create` | Create a new webhook trigger. The webhook fires when an HTTP POST is sent to http://127.0.0.1:<port>/trigger/<webhook_id>. |
| `webhook.delete` | Delete a webhook trigger by ID. The webhook will no longer fire. |
| `webhook.list` | List all webhook triggers. |

## Toolkit

Everyday utilities — tasks, budget, dates, units, health checks. A tool process: install `aivyx-toolkit` and add it as a `[[tool_process]]` (see [INSTALL.md](https://github.com/Aivyx-Agent/aivyx-pa/blob/main/docs/INSTALL.md)).

| Tool | What it does |
|---|---|
| `budget.categories` | List every unique category present in the budget store, sorted ascending. Takes no arguments. |
| `budget.delete` | Delete a budget entry by id. Idempotent — deleting a missing id succeeds with `was_already_deleted: true` rather than erroring. |
| `budget.record` | Record a budget entry. |
| `budget.summary` | Aggregate budget entries over a period. |
| `budget.trend` | Month-over-month trend of budget entries. |
| `budget.update` | Update an existing budget entry by id. Partial update: only the fields you supply change. |
| `calc.eval` | Evaluate an arithmetic expression exactly. |
| `convert.time` | Convert a local datetime between IANA timezones. |
| `convert.units` | Convert a value between units of the same family. |
| `date.add` | Add a (possibly negative) duration to a date. |
| `date.diff` | Compute the signed span between two instants (`to - from`). |
| `health.check.add` | Register a URL for periodic health monitoring. The tool-process-side polling loop will probe the URL on the configured interval and record state transitions. |
| `health.check.list` | List every registered URL watcher with its current state. |
| `health.check.recent_changes` | List state transitions (ok-to-down or down-to-ok) within a recent time window. |
| `health.check.remove` | Remove a registered URL watcher by name. Idempotent — removing a missing name succeeds with `was_already_removed: true` rather than erroring (same posture as `calendar.delete_event` and `budget.delete`). |
| `task.complete` | Mark a TODO task complete. |
| `task.create` | Create a new TODO task. |
| `task.delete` | Permanently remove a TODO task. |
| `task.list` | List TODO tasks. |
| `web.search` | Search the web using Brave Search. |

## Gmail

Needs `aivyx-pa connect gmail`.

| Tool | What it does |
|---|---|
| `gmail.draft` | Create a Gmail draft. The draft lives in the operator's Drafts folder; it does NOT leave Gmail until the operator clicks Send in the Gmail UI. |
| `gmail.read` | Read one Gmail message by ID. |
| `gmail.search` | Search Gmail messages using Gmail's query DSL. |
| `gmail.send` | Send a Gmail message directly. **The message goes out immediately — Aivyx PA has no undo.** Input is a JSON object with required `to`, `subject`, `body_text` fields, an optional `from` (send-as alias the operator's Gmail account has authorized; defaults to the primary address), an optional `in_reply_to_message_id` for `In-Reply-To` + `References` header threading, and an optional `thread_id` for Gmail thread placement. |

## Google Calendar

Needs `aivyx-pa connect calendar`.

| Tool | What it does |
|---|---|
| `calendar.create_event` | Create a new Google Calendar event. |
| `calendar.delete_event` | Delete a Google Calendar event by ID. |
| `calendar.get_event` | Fetch the full detail of one Google Calendar event by ID. |
| `calendar.list_calendars` | Enumerate the Google Calendars the operator has access to. Takes no arguments. |
| `calendar.list_events` | List events on a Google Calendar within a time range. |
| `calendar.upcoming` | List Google Calendar events starting within the next N hours, optionally across multiple calendars. |
| `calendar.update_event` | Partial-update a Google Calendar event by ID. |

## Google Drive

Needs `aivyx-pa connect drive`.

| Tool | What it does |
|---|---|
| `drive.create_folder` | Create a new Google Drive folder. |
| `drive.delete_file` | Permanently delete a Google Drive file or folder by ID. |
| `drive.download_file` | Download a Google Drive file's content as base64-encoded bytes. |
| `drive.get_metadata` | Fetch full metadata for one Google Drive file by ID. |
| `drive.list_drives` | Enumerate the Google Shared Drives (formerly Team Drives) the operator is a member of. Takes no arguments. |
| `drive.list_folder` | List the children of a Google Drive folder. |
| `drive.recent_activity` | List recent activity (who did what to which file) on Google Drive items visible to the operator. |
| `drive.recent_changes` | List Google Drive files visible to the operator that were modified within the last N hours, across every owner — collaborator edits, shared docs, incoming uploads. |
| `drive.recent_files` | List Google Drive files the operator owns that were modified within the last N days. |
| `drive.search` | Search Google Drive files using Drive's query DSL. |
| `drive.upload_file` | Create a new Google Drive file with content. |

## Google Contacts

Needs `aivyx-pa connect contacts`.

| Tool | What it does |
|---|---|
| `contacts.create` | Add a new person to the user's Google contacts. |
| `contacts.delete` | Delete a person from the user's Google contacts. IRREVERSIBLE. |
| `contacts.get` | Fetch full detail for one Google contact. |
| `contacts.list` | List the user's Google contacts, one page at a time. |
| `contacts.search` | Search the user's Google contacts by name, email, phone, or organization. |
| `contacts.update` | Update an existing Google contact. |

## Notion

A tool process: add it as a `[[tool_process]]` with your Notion integration token (see [INSTALL.md](https://github.com/Aivyx-Agent/aivyx-pa/blob/main/docs/INSTALL.md)).

| Tool | What it does |
|---|---|
| `notion.append_blocks` | Append blocks to an existing Notion page. |
| `notion.archive_page` | Archive (Notion's "delete") a page. |
| `notion.create_page` | Create a new Notion page. |
| `notion.get_page` | Fetch a Notion page's properties + top-level block tree. |
| `notion.list_database` | Query a Notion database for pages. |
| `notion.search` | Search Notion content shared with the integration. |
| `notion.update_page_properties` | Update properties on an existing Notion page. |

## Obsidian

A tool process over your vault folder: add it as a `[[tool_process]]` (see [INSTALL.md](https://github.com/Aivyx-Agent/aivyx-pa/blob/main/docs/INSTALL.md)).

| Tool | What it does |
|---|---|
| `obsidian.create_note` | Create a new Obsidian note. |
| `obsidian.delete_note` | Permanently delete an Obsidian note. |
| `obsidian.get_note` | Fetch one Obsidian note. |
| `obsidian.list_folder` | List markdown notes in a vault folder. |
| `obsidian.search` | Search the Obsidian vault. |
| `obsidian.update_note` | Modify an existing Obsidian note. |

## n8n

A tool process for your n8n instance: add it as a `[[tool_process]]` (see [INSTALL.md](https://github.com/Aivyx-Agent/aivyx-pa/blob/main/docs/INSTALL.md)).

| Tool | What it does |
|---|---|
| `n8n.activate_workflow` | Mark an n8n workflow active. |
| `n8n.create_workflow` | Create a new n8n workflow from a definition payload. |
| `n8n.deactivate_workflow` | Mark an n8n workflow inactive. |
| `n8n.delete_workflow` | Permanently delete an n8n workflow. |
| `n8n.execute_workflow` | Trigger a one-off run of an n8n workflow. |
| `n8n.get_execution` | Fetch a single n8n execution by id. |
| `n8n.get_workflow` | Fetch the full definition of an n8n workflow by id. |
| `n8n.list_executions` | List workflow executions on the operator's n8n instance. |
| `n8n.list_workflows` | List workflows on the operator's n8n instance. |
| `n8n.update_workflow` | Replace an existing n8n workflow's definition. |

## Desktop apps

Linux/X11 only; off unless `[applications] enabled = true`.

| Tool | What it does |
|---|---|
| `app.click` | Move the mouse to (x, y) and click. |
| `app.focus` | Raise and focus a window. |
| `app.key` | Send a key or chord to the focused window, e.g. "ctrl+s", "Return", "alt+Tab". |
| `app.list` | List the open windows on the operator's desktop. No input. |
| `app.screenshot` | Capture a full-screen screenshot to a PNG file and return its path. No input. |
| `app.type` | Type literal text into the currently-focused window. |

## Images

A tool process: add `aivyx-vision` as a `[[tool_process]]`. `vision.generate_image` needs an image backend configured.

| Tool | What it does |
|---|---|
| `vision.generate_3d` | Generate a 3D model from a text prompt via a local generation backend. **Not built yet** — every call fails for now. |
| `vision.generate_image` | Generate an image from a text prompt via a local image-generation backend. |
| `vision.generate_svg` | Generate a sanitized SVG image from a text prompt. |

## Kitchen pack (example vertical)

Only with the kitchen vertical pack installed.

| Tool | What it does |
|---|---|
| `kitchen.batch.complete` | Complete an open production batch in KitchenDB. Requires `batch_id` (string); optional `actual_yield` (non-negative number). |
| `kitchen.batch.start` | Start a production batch in KitchenDB. Requires `recipe_id` (string) and `quantity` (positive number); optional `notes` (string). |
| `kitchen.haccp.log` | Append a food-safety (HACCP) record to KitchenDB. Requires `check_type` (string); optional `value` (number), `unit` (string), `location` (string), `passed` (bool), `notes` (string). |
| `kitchen.inventory.adjust` | Adjust an inventory item's on-hand count in KitchenDB. Requires `sku` (string) and `delta` (signed number; negative for waste/usage); optional `reason` (string). |
| `kitchen.inventory.list` | List current kitchen inventory from KitchenDB. Optional `location` (string) filters to one storage area; omit for all. |
| `kitchen.inventory.low_stock` | List inventory items at or below their reorder threshold (the reorder candidates). No input. |
| `kitchen.inventory.value` | Get the total valuation of current kitchen inventory. No input. |
| `kitchen.order.draft` | Draft per-supplier purchase orders in KitchenDB. Optional `items` (array of `{sku, quantity}`) gives an explicit reorder list; omit it to draft from the current low-stock report. |
| `kitchen.order.send` | Dispatch a drafted purchase order to its supplier in KitchenDB. Requires `purchase_order_id` (string); optional `notes` (string). |
| `kitchen.recipe.search` | Search KitchenDB recipes by a free-text `query` (required string). |
| `kitchen.supplier.list` | List the kitchen's suppliers from KitchenDB (for sourcing and purchase orders). No input. |
