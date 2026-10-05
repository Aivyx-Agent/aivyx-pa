# Aivyx PA manual

Aivyx PA is a self-learning personal AI agent that runs on your own
hardware. It talks to the model you choose — local, or a cloud provider under
your own key — with no Aivyx server in between, and every action it takes is
gated, recorded and yours to review.

This manual has three parts. Start with the guide; use the reference to look
things up; read the developer part if you're building on Aivyx PA.

## Part 1 — User guide

Task-by-task, for everyday use. These pages are also built into the Studio
(its **Guide** screen).

1. [Welcome](../guide/01-welcome.md)
2. [Getting started](../guide/02-getting-started.md)
3. [Create your agent](../guide/03-create-your-agent.md)
4. [Chat and missions](../guide/04-chat-and-missions.md)
5. [Memory](../guide/05-memory.md)
6. [Skills and persona](../guide/06-skills-and-persona.md)
7. [Teams](../guide/07-teams.md)
8. [Access and settings](../guide/08-access-and-settings.md)
9. [Screens reference](../guide/09-screens-reference.md)
10. [Troubleshooting](../guide/10-troubleshooting.md)
11. [Desktop app](../guide/11-desktop-app.md)
12. [Models and routing](../guide/12-models-and-routing.md)
13. [Chat apps and accounts](../guide/13-chat-apps-and-accounts.md)
14. [Terminal and CLI](../guide/14-terminal-and-cli.md)
15. [Autonomy and routines](../guide/15-autonomy-and-routines.md)
16. [Named instances](../guide/16-named-instances.md)
17. [Security and privacy](../guide/17-security-and-privacy.md)
18. [Backups, upgrades and moving](../guide/18-backups-upgrades-and-moving.md)

## Part 2 — Reference

1. [Command-line reference](reference/01-cli.md)
2. [Configuration reference](reference/02-configuration.md)
3. [Tools](reference/03-tools.md)
4. [Files and paths](reference/04-files-and-paths.md)
5. [Environment variables](reference/05-environment-variables.md)
6. [Glossary](reference/06-glossary.md)

## Part 3 — Developer

1. [Building from source](developer/01-building-from-source.md)
2. [Architecture](developer/02-architecture.md)
3. [Channel adapters](developer/03-channel-adapters.md)
4. [Tool processes](developer/04-tool-processes.md)
5. [Vertical packs](developer/05-vertical-packs.md)
6. [IPC protocol](developer/06-ipc-protocol.md)
7. [Contributing](developer/07-contributing.md)

## Elsewhere

- [`docs/INSTALL.md`](../INSTALL.md) — every install path and many
  feature-specific setups, in depth.
- [`docs/THREAT_MODEL.md`](../THREAT_MODEL.md) — exactly what Aivyx PA
  defends against, and what it doesn't.
- [`examples/aivyx-pa.toml`](../../examples/aivyx-pa.toml) — the annotated
  example config.
- [`CHANGELOG.md`](../../CHANGELOG.md) — what changed in each release.

Aivyx PA is source-available under the Business Source License 1.1: free for
personal, educational, research and other non-commercial use; each version
becomes MIT-licensed four years after release.
