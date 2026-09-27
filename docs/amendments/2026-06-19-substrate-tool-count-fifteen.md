# Amendment A13b — Substrate Tool Count Update (Thirteen → Fifteen)

**Date:** 2026-06-19
**Renumbered:** filed as A13, which duplicated the Phase 163 amendment
(`2026-06-04-content-part-document.md`, the earlier and first A13). Renamed A13b
on 2026-09-28 so each amendment number is unique; A14 onward are unchanged.
**Phase:** Chapter Forge (FG.2)
**Supersedes:** Narrows P10 (Product Commitment 10 —
Substrate-Only Core, post-A12). The count "thirteen" becomes
"fifteen" throughout P10's text, and `web.extract` and
`git.commit` are added to the enumerated list. P10's
substantive text — the substrate-only principle, the
substrate/infrastructure/third-party taxonomy, and the "what
this does not say" section — is unchanged.
**Implementing phases:** FG.1 (`web.extract`), FG.3 (`git.commit`).
The base + count contract lands here at FG.2, *before* the
`git.commit` tool it gates (FG.3) ships — the governance gate
precedes the destructive tool, never trails it.

---

## What changed

Chapter Forge (the first new-tools breadth chapter after the
Atlas refinement pass) adds two first-party tools:

- **`web.extract`** — GETs a URL with `web.fetch`'s hardened,
  redirect-free, SSRF-guarded rustls client, then runs a
  readability pass (`dom_smoothie`, MIT) and returns the
  article's `{ title, byline, text, word_count }` instead of
  raw HTML. "Read a page", not "fetch a page". Shipped FG.1.
- **`git.commit`** — stages given paths and commits with a
  message inside an operator-allowed repo. The destructive
  sibling to `git.status` / `git.diff` (A12). Shells out to the
  `git` binary (same as the read tools — no `git2` dep).
  Confirm-first when `[access] confirm_destructive` is on.
  Ships FG.3.

By P10's own taxonomy both are substrate — operator-facing
operations against operator-owned resources (a web page, a
repo), in exactly the same category as `web.fetch` and
`git.status`, not agent self-management. So the thirteen-tool
count post-A12 is now fifteen.

---

## Scope bases

- **`web.extract` reuses the existing `net.fetch` base.**
  Extraction *is* an outbound HTTP GET — the same capability
  `web.fetch` needs. No new base; the tool inherits
  `web.fetch`'s SSRF / no-redirect / size-cap protections by
  sharing the same hardened client. Min tier SemiTrusted,
  matching `web.fetch`.

- **`git.commit` adds one new `git.write` base.** A commit
  writes repo history — at least as sensitive as `shell.exec` /
  `fs.delete`. A12 explicitly anticipated this: *"a future
  destructive git tool would warrant a separate `git.write`
  scope."* The base is qualified by canonical repo path and
  checked against the **same** operator `[git] repos` allow-set
  as `git.read`. It joins `KNOWN_BASES` and `CEILING_TRUSTED`
  (Trusted-tier only — omitted from `CEILING_SEMITRUSTED` so a
  remote adapter can never hold it by default; a role grant
  cannot lift it past the ceiling intersection). The qualifier
  kind is `PathGlob`, matching `git.read` / `fs.read`.

---

## The amended rule

> **Aivyx PA core ships exactly fifteen first-party tools forever:
> `fs.read`, `fs.write`, `fs.delete`, `fs.metadata`,
> `memory.read`, `memory.write`, `memory.forget`,
> `shell.exec`, `web.fetch`, `web.post`, `web.extract`,
> `git.status`, `git.diff`, `git.commit`, `net.dns`. Adding to
> or removing from this list requires a `PRODUCT.md`
> amendment.**

The rest of P10 — the substrate-only principle, the
substrate/infrastructure/third-party taxonomy, the "what this
does not say" section — is unchanged.

---

## Why these are substrate, not third-party

P10's burden of proof is on every core addition: a tool belongs
in substrate only if Aivyx PA without it cannot do basic
operator-useful work, and the default answer is "third party."

- **`web.extract`** clears the bar the same way `web.fetch`
  does. Raw `web.fetch` returns bytes/HTML; the LLM then burns
  context (and often fails) parsing nav/ads/markup. Reading the
  web — the single most common operator-bootstrap research
  task — is not reliably possible on `web.fetch` alone. It is a
  refinement of the same primitive, not a new domain (no new
  scope, no new dependency surface beyond one permissive
  readability crate).
- **`git.commit`** completes a loop A12 deliberately left
  half-open: an agent that can *see* a repo's state
  (`git.status` / `git.diff`) but not *commit* cannot close out
  code work. Code-read was ruled bootstrap-essential at A12 "the
  same way `fs.read` is"; code-write is the `fs.write` to that
  `fs.read`. It reuses the existing repo allow-set and the
  shell-out pattern — no new machinery, no new domain.

Neither is domain-specific (a browser, an LSP, email, a
calendar) — those remain third-party tools the operator installs
explicitly, per P10/N3.

---

## Capability-base addendum

The `git.write` base addition is also recorded in the
capability-taxonomy-growth amendment
([`2026-04-17-capability-taxonomy-growth.md`](2026-04-17-capability-taxonomy-growth.md)),
which moves `KNOWN_BASES.len()` 85 → 86. The
`known_bases_count_matches_phase_143_a3_addendum` test pins the
new total at 86, keeping that addendum and the runtime in sync.
DESIGN.md Deliverable 4's substrate base table gains the
`git.write` row at the same edit.
