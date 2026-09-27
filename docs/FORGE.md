# New Tools — `web.extract` + `git.write` (Chapter Forge)

> **Status:** ✅ **COMPLETE (FG.0–FG.5).** Both substrate tools shipped, wired,
> and finalized: **`web.extract`** (readability over the existing `net.fetch`
> base, `dom_smoothie`) and **`git.commit`** (new `git.write` base + Amendment
> A13b, confirm-first, Trusted-only, reusing the `[git] repos` allow-set). P10 is
> now **fifteen tools**; `KNOWN_BASES` is 86. Full workspace suite + clippy +
> `cargo deny` (licenses + advisories) green. The locked reference for the first
> **new-tools breadth** chapter after Atlas. It adds two substrate capabilities —
> **`web.extract`** (fetch a URL → clean readable text) and **`git.write`**
> (commit/stage in an allowed repo) — the two the operator picked from the Atlas
> §6 backlog. Both are core capabilities co-located with their siblings
> (`web.fetch`, `git.read`) in `aivyx-core`, so the chapter does the P10 governance
> (substrate-count amendment) and adds one new capability base (`git.write`),
> in-pattern with **A12** (which added `git.read`). The rest of the §6 backlog
> (utilities pack, structured-data readers, integrations) stays deferred.

## 1. Why these two

Atlas concluded the tool *surface* is legible and the *security model* sound; this
chapter widens *what the agent can do* with two high-value capabilities the agent
keeps reaching for:

- **`web.extract`** — `web.fetch` returns raw bytes/HTML; the LLM then burns
  context (and often fails) parsing nav/ads/markup. A readability pass that
  returns just the article text + title is the difference between "fetched a page"
  and "read a page." Complements `web.search` (find) + `web.fetch` (raw).
- **`git.write`** — `git.read` (`git.status`/`git.diff`, A12) is read-only; the
  code itself flagged the missing destructive sibling. An agent that can *see* a
  repo's state but not *commit* can't close the loop on code work.

## 2. Architecture & governance decisions (locked)

### Both are **substrate**, co-located with their siblings
`web.fetch`/`web.post` and `git.read` live in `aivyx-core/src/tools/`. The new
tools sit beside them and **reuse the hardened machinery already there** — the
redirect-free, rustls, SSRF-guarded reqwest client (`web_fetch.rs`) and the
`[git] repos` canonical-path allowlist gating (`git.rs`). Splitting them into
another tier to dodge an amendment would duplicate that machinery and scatter the
web/git tools across crates. So:

- **P10 substrate count amendment** (PRODUCT.md §P10), bumping the locked count to
  cover the new tools — exactly how **A5/A11/A12** grew it (8→10→13). The
  "substrate-only core" principle, the trust-tier model, and the per-role
  allowlist are all unchanged; only the count moves.

### `web.extract` reuses `net.fetch` — no new base
Extraction *is* an outbound HTTP GET; the capability it needs is the same
`net.fetch` `web.fetch` uses. **No new `KNOWN_BASES` entry.** Min tier
**SemiTrusted** (matching `web.fetch`). It must inherit `web.fetch`'s SSRF /
no-redirect / size-cap protections (reuse the same client + URL guards).

### `git.write` adds one new base + confirm-first gating
- **New `KNOWN_BASES` entry `git.write`** (capability-taxonomy growth — DESIGN
  Deliverable 4, the A3-style addition), added to `CEILING_TRUSTED` (and
  auto-`CEILING_KERNEL`). **Trusted-tier only** — writing history is at least as
  sensitive as `shell.exec`/`fs.delete`.
- **Reuses `git.read`'s repo allowlist:** a commit is only allowed inside a path
  on the operator's `[git] repos` list (same canonical-path match as
  `git.status`/`git.diff`).
- **Confirm-first** like `fs.delete`: when `[access] confirm_destructive` is on, a
  commit requires an explicit `confirm` (the op is shown first). Shell-out to the
  `git` binary (same as `git.read` — no `git2` dep).
- **Tools (decide exact split in-phase):** `git.commit` (stage given paths +
  commit with a message) as the primary; optionally a separate `git.add`. Lean
  toward the minimal surface (one `git.commit` that stages then commits).

### Licensing discipline (post-Charter)
`web.extract` needs an HTML→text / readability dependency. It **must be
permissive (MIT/Apache)** — `cargo deny check licenses` is a hard gate (BUSL-1.1,
zero copyleft). Candidates: `html2text`, a `readability` port, `dom_smoothie`
(all MIT) — pick one, verify the gate stays green, prefer the lightest dep graph.

## 3. Scope

**In:** the two tools, their tests, the P10 amendment + the `git.write` base, and
the Atlas legibility updates they trigger (`docs/TOOLS.md` rows, the
`check_tool_quality` sweep, the catalog drift-guard, README mention). **Out:** the
rest of the §6 backlog (utilities pack, `web.extract` beyond readability,
structured-data readers, integrations) — future chapters. No change to the
security model, trust tiers, or sandboxing beyond the new base's tier placement.

## 4. Phase plan (docs-first, small phases per convention)

| Phase | Deliverable | Notes |
|---|---|---|
| **FG.0** ✅ | **This design contract** | locked reference; banner flips per phase. DONE. |
| **FG.1** ✅ | **`web.extract`** | DONE. New substrate tool in `aivyx-core/tools/web_fetch.rs` reusing the hardened redirect-free client (factored to `build_redirect_free_client`) + SSRF/size guards; `dom_smoothie` (MIT) readability dep; output `{ title, byline, text, word_count, url, extractable }`; non-article/non-UTF8/redirect → `extractable:false` pointing at `web.fetch`. Reuses `net.fetch`. 4 tests + quality sweep. |
| **FG.2** ✅ | **`git.write` base + amendment** | DONE. Added `git.write` to `KNOWN_BASES` (85→86) + `CEILING_TRUSTED` + `docs/TOOLS.md`; **Amendment A13b** (PRODUCT.md P10 thirteen→fifteen + DESIGN Deliverable 4 row + taxonomy-growth addendum), modeled on A12. Count-pin + drift-guard updated. |
| **FG.3** ✅ | **`git.commit` tool** | DONE. Shell-out in `aivyx-core/tools/git.rs`; repo-allowlist gated (shared `canonicalize_repo_allow_set` with the read config); stages explicit repo-relative `paths` + commits a `message`; confirm-first via `confirm_destructive`; Trusted-only. 8 tests incl. real-tmpdir happy path / denial / confirm gate / traversal (single `git.commit`, no `git.add`). |
| **FG.4** ✅ | **Legibility + wiring** | DONE. Registered both in the agent `tool_list` (`aivyx.rs`): `web.extract` all-channels, `git.commit` from `[git] repos` + `confirm_destructive`; extended `check_tool_quality` sweep; `docs/TOOLS.md` rows + name→scope map; README 13→15; registration test. |
| **FG.5** ✅ | **Finalize** | DONE. Full workspace suite + clippy + `cargo deny` (licenses + advisories) green; ATLAS §6 backlog items struck; status flipped to COMPLETE; chapter recorded. |

**Discipline:** FG.2's amendment is the governance gate — the `git.write` base and
the substrate-count bump are contract changes; they land *before/with* the tool
(FG.3) that needs them, never after. Test band: moderate — dense in FG.1 (extraction
+ guards) and FG.3 (shell-out + gating + confirm); price ~30–45 new tests.

## 5. Open questions (resolve in-phase)

- **Readability crate** — `html2text` (simple, robust) vs a `readability`/
  `dom_smoothie` article-extraction port (better "just the article" output, more
  deps). Pick the lightest permissive one whose output the LLM can use (FG.1).
- **`git.write` tool split** — single `git.commit` (stage+commit) vs `git.add` +
  `git.commit`. Default to the minimal single tool unless staging control proves
  necessary (FG.3).
- **`web.extract` raw-HTML fallback** — when a page isn't an article (JSON, a file),
  return a clear "not extractable, use web.fetch" signal rather than garbage (FG.1).

---

*Chapter Forge is the first breadth chapter after the Atlas refinement pass: it
turns "the agent can fetch a page" into "the agent can read a page," and "the agent
can see a repo" into "the agent can commit to it" — two capabilities short of which
real web-research and code work stall. It grows the substrate by a measured amount,
on the contract, and leaves the larger §6 integrations for later.*
