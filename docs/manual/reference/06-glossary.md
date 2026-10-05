# Glossary

Product terms first, then the codenames you'll meet in commit messages,
code comments and the deeper design docs. Links to those docs go to the
repository on GitHub.

## Product terms

**Access level** — How far the agent reaches on your machine: `sandbox`,
`workspace`, `home`, `full` or `custom`. Set with `aivyx-pa access`. See
[Access and settings](../../guide/08-access-and-settings.md).

**Audit log** — A tamper-evident record of every tool call and decision,
kept in the store. Each entry is chained to the previous one with an HMAC,
so a change anywhere breaks the chain. Check it with `aivyx-pa audit`.

**Autonomous loop** — Works through a queue of *stories* (tasks) on its own
until done or a limit is hit. Configured in `[loop]`, driven with
`aivyx-pa loop`.

**Autonomy level** — How much the agent may do without asking:
`manual`, `assisted`, `supervised`, `autonomous` or `unleashed`. Set with
`aivyx-pa autonomy`. See [Autonomy and routines](../../guide/15-autonomy-and-routines.md).

**Capability (scope)** — Permission for one kind of action, such as
`fs.read` or `email.send`. Roles grant capabilities; trust tiers cap them.

**Channel** — A way of talking to the agent: the terminal, the Studio, the
desktop app, Telegram, Discord, Slack, voice, or one you write.

**Daemon** — The one long-running Aivyx PA process. Every channel talks to it
over a private local socket.

**Default skills** — `SKILL.md` capability packages bundled with Aivyx PA
and shared with aivyx-coder, read through `skill_defaults.*` tools. Not
the same as learned skills.

**Escalation** — With routing on, moving a call from a local model to a
cloud one. Governed by `[routing.escalation]`; never happens for a
sensitive conversation.

**Gate** — A pause for your approval before an action runs. You answer it
in chat, the terminal or the Studio.

**Instance** — One complete, separate Aivyx PA: its own config, store,
roles, memory and Studio. Everyone starts with `default`; add more with
`aivyx-pa instances create`. See [Named instances](../../guide/16-named-instances.md).

**Integration** — A connected service (Gmail, Calendar, Notion…) whose
tools run as a separate, sandboxed tool process.

**Learned skill** — A procedure the agent wrote down (or you taught it),
approved by you, which it can reuse. Lives in the store.

**MCP server** — An external Model Context Protocol server whose tools the
agent can use, configured as `[[mcp_server]]`.

**Memory profile** — `lite` or `smart` (`[memory] profile`): how much of
the recall machinery is switched on. `smart` adds the knowledge wiki and
graph, which cost model calls.

**Mission** — A long-running piece of work with its own record and status,
often run by a team.

**Notify target** — A named place notifications can go: a Telegram chat, a
webhook or an email address (`[[notify_target]]`).

**Pack (vertical pack)** — A signed bundle that adds a domain-specific team
and tools, such as the kitchen example. Managed with `aivyx-pa pack`.

**Passphrase** — Unlocks the store. Aivyx PA can't recover it for you.

**Persona** — The agent's evolving character and what it has learned about
working with you. Changes are proposed and wait for your approval.

**Profile** — What you tell the agent about yourself and how you like to
work (`[profile]`, `aivyx-pa profile`).

**Proposal** — A change the agent suggests to its own Persona, skills or
behaviour. Nothing applies until you approve it (unless you set an
auto-accept threshold).

**Reflection** — A scheduled pass in which the agent reviews recent turns
and files proposals (`[[reflection_schedule]]`).

**Role** — A named bundle of system prompt, tools and capabilities
(`[[role]]`). Roles can inherit from a parent.

**Routine** — Something that runs on its own: a schedule, a webhook or a
file watch.

**Routing** — Choosing a model per call from the models you have
(`[routing]`). Off by default.

**Sensitive conversation** — With routing on, one that has touched your own
data (mail, files, memory…). It stays on local models.

**Store** — The encrypted database holding memory, Persona, skills,
missions, the audit log and saved keys (`store.redb`).

**Studio** — The web interface the daemon serves at
`http://127.0.0.1:7843`, also wrapped by the desktop app.

**Team** — Several specialist agents working one mission under a lead; the
built-in team is called the Nonagon. See [Teams](../../guide/07-teams.md).

**Tool** — One thing the agent can do, such as `fs.read` or `gmail.send`.
See [Tools](03-tools.md).

**Tool process** — A tool that runs as its own program and talks to the
daemon over a small protocol (`[[tool_process]]`). Integrations work this
way, and you can write your own.

**Trust tier** — The ceiling on what a channel may ever do: `Kernel`,
`Trusted`, `SemiTrusted` or `Untrusted`. A chat app gets less than your own
terminal.

**Workspace** — The agent's own directory for notes, plans and its
journal, separate from your files.

## Codenames

Features were built as named "chapters". Each name below is what it refers
to today; most have a design doc under `docs/`.

| Name | What it is |
|---|---|
| Abacus | The calculator, unit-conversion and date tools. |
| Almanac | The Studio's Tools screen. |
| Atlas | The tool catalog, [`docs/TOOLS.md`](https://github.com/Aivyx-Agent/aivyx-pa/blob/main/docs/TOOLS.md), and `tools.list`. |
| Ballast | Per-mission spending caps (`[budget] per_mission_*`). |
| Bridle | Guards for local tool-calling, including the loop breakers that stop a turn repeating itself. |
| Brigade | The kitchen pack's tools. |
| Bulwark | Fencing untrusted tool output (web pages, email…) so the model treats it as data, not instructions. |
| Codex | The knowledge wiki synthesised from memory (`[wiki]`). |
| Conduit | Making operator-added MCP servers work: env, headers, diagnostics. |
| Deckhand | Using your open desktop apps (`[applications]`). |
| Emboss | Grammar-constrained tool calls for llama.cpp-family servers. |
| Ensemble | Per-member models in a team. |
| Forge | The `web.extract` and git-write tools. |
| Freight | Signed, binary vertical-pack bundles. |
| Gatehouse | The rule that the Studio can't be exposed beyond your machine without a sign-in token. |
| Harbor | The container image and server deployment. |
| K | Cost governance: token pricing, the spend ledger and `[budget]`. |
| Keyring | Keeping the passphrase in the OS credential store. |
| Lantern | The Studio's MCP screen. |
| Lattice | The typed knowledge graph (`[graph]`, `graph.query`). |
| Lexicon | The controlled relation vocabulary for the graph. |
| Loom | Recall that blends semantic, keyword and graph search. |
| Nonagon | The built-in multi-agent team. |
| Passport | Agent identity and signed messages across machines (federation). |
| Picket | The active scan for prompt-injection phrases in untrusted tool output. |
| Portcullis | Blocking writes to persistence targets (shell rc files, `authorized_keys`, cron, git hooks). |
| Praxis | The agent authoring skills from what it knows (`[skill_authoring]`). |
| Rampart | Blocking network tools from reaching private and cloud-metadata addresses. |
| Reins | The autonomy dial (`[autonomy]`). |
| Repertoire | The Studio's Skills screen. |
| Roster | Creating and editing the team from the Studio. |
| Sheaf | The CSV, Excel and PDF tools. |
| Stencil | Grammar-constrained tool calls for the in-process engine. |
| Strop | Grading how well each skill works. |
| Synapse | The single `[memory] profile` switch. |
| Thread | Replaying recent conversation into each turn. |
| Throttle | Tool-call rate limits (`[rate_limit]`). |
| Timbre | The Kokoro text-to-speech voice. |
| Ward | Blocking reads of secrets: SSH keys, cloud credentials, `.env`, Aivyx PA's own store and token. |
| Whetstone | Refining skills that underperform (`[skill_refinement]`). |
| Wire | Headless, multi-turn sessions from scripts. |

For the security names (Ward, Portcullis, Rampart, Bulwark, Picket,
Gatehouse) in plain language, see
[Security and privacy](../../guide/17-security-and-privacy.md).
