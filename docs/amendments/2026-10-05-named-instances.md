# Amendment A17 — Named Instances Under P1

**Date:** 2026-10-05
**Phase:** Named instances (several separate agents for one OS user).
**Clarifies:** **P1** (Single Operator, Single Primary Agent). **Leaves
unchanged:** **P6** (OS-Level Operator Identity) and every rule that
follows from P1 inside an instance. No PRODUCT.md or DESIGN.md text is
superseded.
**Implementing phase:** Named instances, branch `feat/named-instances`.
**Reference design:** `docs/superpowers/specs/2026-10-05-named-instances-design.md`.

---

## What changed

P1 reads: *"One human operator per Aivyx PA instance. One primary agent
the operator interacts with."* The rule is scoped **per instance**, and
until now each OS user could only ever have one instance: every path
(socket, store, config, working dirs), the keyring entry and the service
name were fixed per user.

A17 authorizes one operator — one OS user — to run **several named
instances** side by side, each a complete Aivyx PA instance:

- An instance is chosen with `--instance <name>` or `AIVYX_PA_INSTANCE`.
  No selection means the `default` instance, which keeps every path and
  name it had before A17.
- Each named instance has its own config, encrypted store, daemon
  socket, working dirs, Access sandbox, keyring entry, background
  service and Studio port.

## What P1 still requires — per instance, unchanged

- **One primary agent per instance.** Each instance runs exactly one
  primary agent; sub-agents remain role switches inside it (P1 item 2).
- **No multi-tenancy.** Instances are not a tenancy primitive: every
  instance belongs to the same operator, the OS user who owns them (P6).
  A second *human* still needs a second OS user or machine (P1 item 1).
- **Attenuation.** Capability rules (P7) apply inside each instance
  exactly as before.

## What instances do not share

Nothing at the Aivyx PA layer: no store, memory, audit chain, Profile,
Persona, roles, keyring entry, passphrase file or socket. There is no
channel between instances; any future cross-instance communication would
need its own amendment (federation remains the intended path).

Because all instances run as the same OS user, the operating system does
not separate them; Aivyx PA does, by keeping their files apart and
keeping each agent's reach inside its own Access settings. Ward already
denies every `*.redb` store and every `daemon.env` by name and
extension, so an agent with wide Access still cannot read another
instance's store or saved passphrase through its tools.

## Out of scope

- The desktop app and the chat channels (Telegram, Discord, Slack,
  voice) serve the `default` instance only in this phase.
- Sharing or copying data between instances.
