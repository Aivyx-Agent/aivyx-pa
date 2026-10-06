# Security Policy

Aivyx PA is a single-operator personal agent (see `docs/THREAT_MODEL.md`
for the full account) — not a multi-tenant service. Its load-bearing
security properties are **capability-based scopes + trust tiers**
(bounding what a tool call can reach), an **HMAC-chained,
offline-verifiable audit log** (every action, allowed or denied, is
recorded; tampering trips `AuditError::ChainBroken`), and **encryption
at rest** (Argon2id → HKDF-SHA256 → ChaCha20-Poly1305). Four named
guard chapters extend this: **Ward** (blocks reads of SSH/cloud
credentials, `.env`, the agent's own passphrase), **Rampart** (network
egress guard against SSRF, cloud-metadata, and private-network reach,
including DNS-rebinding), **Bulwark** (prompt-injection resistance —
fetched/parsed/tool content is fenced as untrusted data at every
ingress), and **Portcullis** (blocks writes to `authorized_keys`,
shell rc files, and systemd/cron/autostart paths). `shell.exec` and
`git.rs`'s tools additionally run every spawned child process under
Landlock + seccomp-bpf confinement by default, via the same
`aivyx-confine` primitive `aivyx-coder` uses.

## Known, accepted risk surface — not new findings

`docs/THREAT_MODEL.md` section 5 ("Threats we explicitly do not
defend against") is the authoritative, maintained list. Read it before
reporting — if your finding is already listed there, it's known and
accepted, not a new report. Highlights most likely to be independently
rediscovered:

- First-party, in-process tools (the substrate/infra tools,
  `mission.*`/`reflection.*`, MCP proxies) run in the daemon's own
  address space rather than a confined child process (§5.6).
- Third-party tool processes and MCP servers run with the operator's
  full OS authority unless the operator explicitly configures a
  wrapper (bubblewrap/firejail/Docker/sandbox-exec) — an automatic
  bundled preset applies to `[[tool_process]]` by default since Phase
  180, but MCP servers stay opt-in-only (§5.2, §5.6).
- A linked git worktree or submodule runs `git.rs` fully unconfined,
  since Landlock can't reach the real gitdir from the worktree root
  alone (§6, property 7).
- Beyond Bulwark's untrusted-content fencing, there is no smarter
  pattern-matching or ML-based prompt-injection scanner (§5.3) —
  capability gating is the backstop for anything that gets past that
  fencing.
- Root-compromise of the operator's own machine, LLM-provider-side
  risk, side channels, and channel-platform compromise are all
  explicitly out of scope (§5.1, §5.4, §5.5, §5.7, §5.8, §5.9).
- `aivyx-desktop` (the opt-in native shell, Linux only) links an
  unmaintained GTK3 dependency stack with no upstream fix available —
  confirmed still true against the latest `tao`/`wry`/`tray-icon`
  releases as of the date this was last checked (§5.10).

## In scope

Capability/permission-gate bypass, audit-chain tampering that goes
undetected, cryptographic or encryption implementation flaws, bypasses
of the Ward/Rampart/Bulwark/Portcullis guard chapters, credential or
secret exposure beyond what's already documented as accepted, and a
confinement bypass on paths that ARE confined by default (`shell.exec`/
`git.rs` outside the worktree/submodule carve-out above).

## Out of scope

The known, listed gaps above (until fixed — tracked separately, not
new findings); "the model said something wrong" model-quality issues;
anything that requires the reporter to already have physical or root
access to the operator's own machine. This is not a bug bounty
program.

## Reporting a vulnerability

Email **jccorbett67@gmail.com** with details. Please don't open a
public issue for a vulnerability. We aim to resolve or provide a remediation plan for a
confirmed vulnerability within 90 days of the report, or coordinate a
later disclosure date directly with the reporter if a fix genuinely
needs longer. Credit is offered in release notes at the reporter's
preference.
