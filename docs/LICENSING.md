# Licensing & Commercial Model (Chapter Charter)

> **Status:** ✅ **CHAPTER COMPLETE — CR.0–CR.6 all ✅.** Aivyx PA has moved from
> **MIT** to the **Business Source License 1.1 (BUSL-1.1)**: **free for personal /
> individual / non-commercial use, a paid commercial license for any business or
> production use,** auto-converting back to MIT four years after each release. The
> tree carries the BUSL-1.1 `LICENSE` (CR.2; MIT preserved as the Change License
> at `LICENSES/MIT.txt`); the **commercial path** is in
> [`COMMERCIAL.md`](../COMMERCIAL.md) (CR.3); the **contributor gate** is
> [`CONTRIBUTING.md`](../CONTRIBUTING.md) + the **CLA** [`CLA.md`](../CLA.md)
> (CR.4); positioning is refreshed to "source-available" everywhere + a licensing
> FAQ in §8 (CR.5); and the **first BSL release —
> [v0.3.0](https://github.com/Aivyx-Agent/aivyx/releases/tag/v0.3.0) — is live**
> (CR.6). *(Carried follow-ups, not blockers: lawyer-review `CLA.md` before the
> first external PR merge; swap Piper for a permissive TTS before shipping voice
> in an official binary.)* Decisions locked by the
> operator: (1) **BSL**, not FSL/AGPL — the gate is *commercial vs. personal*,
> not *competing vs. not* and not *SaaS vs. internal*; (2) the **whole public
> repo** moves (engine + public tool crates), with verticals staying private as
> already planned.

## 1. The decision and why it isn't MIT anymore

Aivyx PA shipped public + MIT (Chapter Q, v0.1.0 / v0.2.0). MIT is maximally
permissive: anyone — including a competitor or any for-profit company — may use,
modify, host, and **sell** the software with no obligation back to the author.
That is the right posture for adoption and trust, but it leaves **no monetization
hook on the core itself**: the open-core plan ([[aivyx-ecosystem-roadmap]])
monetizes only the *private verticals* and *hosting*, never the engine.

The operator's intent is narrower and clearer: **the public end user runs Aivyx PA
for free; a commercial end user pays.** That is a *commercial-vs-personal* gate.
The license that expresses exactly that gate is the **Business Source License
1.1** with an Additional Use Grant scoped to personal / non-commercial use.

### Why not the alternatives (recorded so we don't relitigate)

| License | Gates on | Verdict for "free personal / paid commercial" |
|---|---|---|
| **MIT** (today) | nothing | ❌ no monetization hook on the core |
| **AGPL-3.0 + commercial** | *offering it as a network service* | ❌ a company using it internally pays nothing |
| **FSL** | *competing commercial use* | ❌ internal commercial use is free; only resellers pay |
| **BSL 1.1 + personal-use grant** | *commercial / production use* | ✅ **exactly the intended split** |

The cost of BSL is **honesty about the label**: BSL is **"source-available," not
OSI-approved "open source."** Every doc that today says "open source" must be
corrected to "source-available" (CR.5). This is non-negotiable for the trust
story — a privacy-first agent cannot afford a misleading license claim.

## 2. What cannot be undone — and what that means

**v0.2.0 and every prior commit are MIT in perpetuity.** A license is granted at
the moment of distribution; it cannot be revoked. Anyone may fork the v0.2.0
baseline and do anything MIT allows, forever. **The relicense therefore applies
only from the next release forward** (the first BSL tag — see CR.6).

This is not a problem, it is how every comparable relicense worked (HashiCorp,
Sentry, Redis, MariaDB, CockroachDB): the free-rider's fork is frozen at an old,
unmaintained snapshot while the maintained line moves ahead under BSL. **Our moat
is velocity + brand + verticals + hosting, never the frozen snapshot.**

## 3. Standing on the right to relicense

Relicensing requires holding the rights to **all** the code being relicensed.

- **Today:** the operator is the sole author — full freedom to relicense.
- **The moment outside contributions are accepted, that breaks.** A contributor's
  patch is theirs under the inbound license; without an explicit grant we could
  not relicense their lines, nor sell a commercial license covering them. So
  **contributor terms (a DCO or CLA) that grant relicensing rights are a
  prerequisite for accepting any external PR** — handled in CR.4 *before* the repo
  invites contributions.

## 4. The BSL parameters (the contract)

The BSL 1.1 template has four fill-in parameters. These are the locked values:

- **Licensor:** Julian (Aivyx) / the Aivyx-Agent project.
- **Licensed Work:** Aivyx, the first BSL-tagged version onward (CR.6).
- **Additional Use Grant:** *personal and non-commercial use.* Draft wording
  (final text lands in CR.2, reviewed against the official template):

  > You may use, copy, modify, and create derivative works of the Licensed Work
  > for **personal, individual, educational, research, evaluation, and other
  > non-commercial purposes**. "Non-commercial" means use that is **not primarily
  > intended for or directed toward commercial advantage or monetary
  > compensation**, including use by an individual for personal projects and use
  > by a registered non-profit or accredited educational institution. **Any other
  > use — including any use by or on behalf of a for-profit entity, any use in
  > production in connection with a commercial product or service, and any use
  > that generates revenue — requires a commercial license from the Licensor.**

- **Change Date:** four (4) years after the publication date of **each** released
  version (every release carries its own clock).
- **Change License:** **MIT** (continuity with Aivyx's origin; on the Change Date
  that version reverts to the exact MIT terms it shipped under before).

**SPDX note (load-bearing for tooling):** the SPDX identifier is **`BUSL-1.1`**
(not "BSL-1.1"). Cargo's `license` field must use `BUSL-1.1` so `cargo`,
`cargo-deny`, and downstream scanners parse it. Where the personal-use grant makes
the expression non-standard, fall back to `license-file` pointing at `LICENSE`.

## 5. Scope — what moves and what was always private

- **Moves to BSL:** the entire **public** workspace — the engine crates (turn
  loop, `aivyx-capability`, audit chain, `aivyx-team`/Nonagon, memory, persona
  governance, IPC, TUI, web Studio) **and** the public tool-process crates
  (`aivyx-gmail`, `aivyx-calendar`, `aivyx-drive`, `aivyx-contacts`,
  `aivyx-toolkit`, etc.). One license across the public repo keeps it simple and
  defensible; a per-crate split buys nothing here.
- **Was always private (unchanged):** commercial verticals (Kitchen/Factory
  toolkits, customised Nonagon `TeamConfig`s), hosted Harbor, federation. The BSL
  on the public core is a *new* monetization layer *in addition to* these.
- **The "Aivyx" and "Aivyx PA" names are trademark, not copyright** — a BSL relicense does not
  protect the brand. Trademark posture is out of scope for this chapter (noted so
  it isn't assumed covered).

## 6. Phase plan (docs-first, small phases per project convention)

| Phase | Deliverable | Notes |
|---|---|---|
| **CR.0** | **This design contract** | locked reference; status banner flips per phase |
| **CR.1** ✅ | **Dependency license audit** | DONE. `cargo deny check licenses` now passes against the **all-features** graph; the permissive allow-list + the documented exceptions are codified in `deny.toml`. Findings in §6.1. Gate is green; CR.2 unblocked. |
| **CR.2** ✅ | **The LICENSE swap** | DONE. `LICENSE` is now the filled canonical BUSL-1.1 (full Terms + Covenants + Notice; Parameters per §4 — Change License = MIT, per-release 4-year Change Date, personal/non-commercial Additional Use Grant). MIT preserved verbatim as `LICENSES/MIT.txt` (the Change License + historical form). Workspace `Cargo.toml` `license = "BUSL-1.1"` (SPDX-parseable; the non-standard grant lives in `LICENSE`). All 33 crates inherit it via `license.workspace`. Set `publish = false` workspace-wide (no crates.io distribution) so `cargo deny`'s `private.ignore` skips our own first-party BUSL crates — **`cargo deny check licenses` stays green** (a third-party copyleft/BUSL dep still fails loudly). Findings in §6.2. |
| **CR.3** ✅ | **Commercial-license path** | DONE. [`COMMERCIAL.md`](../COMMERCIAL.md) at the repo root: the §4 grant restated in plain English (free: individuals, non-commercial, non-profits, education; paid: any for-profit/internal/production/revenue/resale use), a quick-check table, the 4-year→MIT reassurance, the **aivyx@aivyx-studio.com** contact path with what to include, and a per-engagement pricing placeholder. `LICENSE` already points here. |
| **CR.4** ✅ | **Contributor terms** | DONE. Operator chose the **CLA** (stronger) over a DCO — a plain DCO certifies origin only and does **not** grant commercial-sublicensing rights, which the paid-license model requires. Shipped [`CLA.md`](../CLA.md) (v1.0: a *license grant*, not assignment — contributor keeps copyright, grants the Licensor a perpetual/irrevocable right to relicense **and commercially sublicense** Contributions; Apache-ICLA-shaped + employer/patent/third-party clauses) and [`CONTRIBUTING.md`](../CONTRIBUTING.md) (the contributor entry point). **Acceptance = `git commit -s` sign-off**, which certifies the DCO *and* accepts the CLA per-contribution; maintainers can't merge un-signed-off commits. The gate now **precedes** any external PR (§3). *Superseded 2026-09-24:* now that the ecosystem-wide BUSL relicense (`aivyx-ecosystem/LICENSING.md`) has its own org-wide CLA, Aivyx PA's project-scoped `CLA.md` (v1.0) was retired in favor of it — same substantive terms, org-wide scope instead of per-repo; `CLA.md` at this path is now a pointer to the org-wide version, acceptance mechanism unchanged. |
| **CR.5** ✅ | **Positioning & docs refresh** | DONE. Corrected every "open source"/"MIT" claim about *Aivyx PA's own code* to "source-available under BUSL-1.1": `README.md` (License & trademark section + Contributing note pointing at the CLA), `TRADEMARK.md` (incl. fixing "use it commercially" → needs a commercial license), and `DESIGN.md` "Deliverable 2 — The Open-Core Line" via an inline note + **[Amendment A14](amendments/2026-06-19-busl-relicense.md)** (a LOCKED contract section can't be corrected by prose alone). Added the licensing **FAQ (§8)** cross-linking `COMMERCIAL.md`. Left third-party MIT mentions alone (Ollama/llama.cpp in INSTALL, Hermes in ROADMAP — those *are* MIT). PRODUCT.md carries no licensing clause → untouched. GitHub repo description is currently empty (no "open source" claim to correct); when one is set, phrase it "source-available." |
| **CR.6** ✅ | **First BSL release** | DONE. Operator chose **v0.3.0** (incremental; v1.0 held for a real stability milestone). Workspace bumped 0.2.0 → 0.3.0; `CHANGELOG.md` 0.3.0 section **leads with the license change** (then Contacts/Genesis/Harbor/Throttle). **Release [v0.3.0](https://github.com/Aivyx-Agent/aivyx/releases/tag/v0.3.0) is live** — green run `27799155867`: all 4 musl/darwin tarballs + checksums + shell installer + source archive published. Two pre-green failures, both fixed/transient (see §6.3). |

**Discipline:** CR.1 is a hard gate — if a dependency's license is incompatible
with shipping the combined work under BSL, that's a blocker to resolve (swap the
dep or carve it out) *before* the LICENSE swap, not after.

### 6.1 CR.1 audit findings (the gate is green)

Audited via `cargo deny check licenses` over the **all-features** graph (the
`[graph] all-features = true` already required for advisories), so every optional
provider/voice/web stack is evaluated, not just default features. The policy lives
in `deny.toml` as a permissive-only `allow` list — anything not listed fails
loudly, so a future copyleft dep can't slip in silently. Three categories surfaced:

1. **MPL-2.0 — compatible, allowed.** ~14 crates: the `symphonia` audio stack, the
   servo CSS stack pulled by Dioxus (`cssparser`/`cssparser-macros`/`selectors`/
   `dtoa-short`), and `option-ext` (via `dirs`). MPL-2.0 is **file-level (weak)
   copyleft**: it never relicenses the larger combined work — the only obligation
   is sharing modifications to the MPL files themselves, which we never make. Added
   to `allow`.
2. **NCSA — permissive, allowed.** `libfuzzer-sys` (`(MIT OR Apache-2.0) AND
   NCSA`), a dev/fuzz dependency. NCSA is BSD/MIT-style. Added to `allow`.
3. **GPL-3.0-only — the one real risk, carved out then ✅ eliminated.**
   *(Resolved in [[chapter-timbre]] — see the Live-constraint note below.)*
   `piper1-rs-sys` (Piper TTS bindings) is strong copyleft. It was pulled **only**
   through the optional `aivyx-voice` → `piper1-rs` path behind the **opt-in
   `channel-voice[-full]` feature**, which `aivyx-cli`'s `default = []` excludes.
   **It is never in a distributed Aivyx PA binary:** cargo-dist uses `precise-builds =
   true` and builds only `aivyx-cli` with its default features, so the official
   release artifacts never compile or link it (this is the same reason the musl
   release build skips ALSA — see `dist-workspace.toml`). The GPL combination only
   exists in a binary a user builds from source with voice explicitly enabled —
   their build, their GPL obligation, satisfied because they hold the source.
   **Aivyx PA's own crates therefore relicense to BUSL-1.1 uncontaminated.** Encoded
   as a per-crate `exceptions` entry scoped to `piper1-rs-sys` (not a blanket
   GPL allowance) with the full rationale in `deny.toml`.

**Live constraint this created — ✅ RESOLVED in [[chapter-timbre]].** The CR.1
finding was: because Piper is GPL-3.0, voice could not ship in an official BSL
binary without swapping the TTS engine. **Chapter Timbre did exactly that** —
Piper was removed and replaced by the permissive **Kokoro** stack (Kokoro-82M
Apache-2.0 + `voice-g2p` MIT + `ort` Apache/MIT, espeak-free), and the
`piper1-rs-sys` exception was deleted from `deny.toml` (TB.4). The dependency
graph now has **zero GPL**, so voice is license-clean for everyone who builds it.
One *non-licensing* blocker remains for putting voice in the official **Linux**
binaries: the musl-static release can't compile `cpal`/ALSA (the same reason the
musl build skips ALSA). macOS official binaries have no such issue. Tracked in
`docs/TIMBRE.md` §4.

### 6.2 CR.2 swap notes (what the relicense commit actually did)

- **`LICENSE`** is the **full canonical MariaDB BUSL-1.1 template** — not a
  trimmed copy. It carries the Parameters block, the complete **Terms**, the
  **Covenants of Licensor** (1–4), and the **Notice** ("not an Open Source
  license"). Keeping the Covenants matters: Covenant #4 is "Not to modify this
  License in any other way," so only the four Parameters are filled.
- **Change License = MIT satisfies Covenant #1.** Covenant #1 requires the
  Change License to be GPL-2.0-compatible; **MIT is GPL-compatible**, so naming
  MIT as the revert license is valid under the template (and gives continuity
  with Aivyx's MIT origin).
- **Parameters as filled:** Licensor = Julian (Aivyx) / Aivyx-Agent; Licensed
  Work = "Aivyx, the first version released under this License and all later
  versions" (version-number-agnostic until CR.6 picks the tag); Additional Use
  Grant = the §4 personal/non-commercial wording; Change Date = four years per
  released version (per-release clock, matching the Terms' "applies separately
  for each version"); Change License = MIT (`LICENSES/MIT.txt`).
- **MIT is preserved, not deleted.** `LICENSES/MIT.txt` is the byte-for-byte
  prior `LICENSE`. It is both the historical record (v0.2.0 and earlier are MIT
  in perpetuity, §2) and the literal text each version reverts to on its Change
  Date.
- **Cargo metadata:** `[workspace.package] license = "BUSL-1.1"` (the SPDX id,
  so `cargo`/`cargo-deny`/scanners parse it — the personal-use grant is a BUSL
  *parameter*, not a change to the identifier, so `license-file` was not needed).
  All 33 member crates already inherit via `license.workspace = true`.
- **Why `publish = false` workspace-wide.** cargo-deny audits *every* crate in
  the graph, including ours; once our crates are BUSL-1.1 they'd trip the
  permissive-only allow-list. The correct fix is not to widen the allow-list
  (that would let a *third-party* BUSL/copyleft dep pass silently) but to mark
  our crates first-party-and-unpublished and let `private.ignore = true` skip
  them. `publish = false` is independently honest: Aivyx PA never ships to crates.io
  (releases are cargo-dist binaries + GHCR images), and it guards against an
  accidental `cargo publish`. The gate's copyleft tripwire is fully intact.

### 6.3 CR.6 release notes (two failures before green)

The v0.3.0 release pipeline took three runs to go green. Both early failures are
recorded so they aren't rediscovered:

1. **cargo-dist `plan` failed — `publish = false` hid the binary.** *"This
   workspace doesn't have anything for dist to Release!"* The CR.2
   workspace-wide `publish = false` (added so cargo-deny's `private.ignore` skips
   our BUSL crates — §6.2) **also** makes cargo-dist skip every crate by default.
   Fix: add `[package.metadata.dist] dist = true` to **`aivyx-cli`** (the only
   crate that ships a release binary; `precise-builds = true`). This re-includes
   it in dist **without** re-enabling `cargo publish`. Verify locally with
   `dist plan` (the CLI is `dist`, not `cargo dist`). A genuine
   `publish=false`-vs-dist interaction — see [[cargo-dist-publish-false]].
2. **Quality-gate runner crashed — "No space left on device."** A transient
   GitHub-runner disk-exhaustion during `cargo test`/`clippy` (the prior run's
   identical quality-gate had passed; CI on the same commit was green). Note the
   pipeline still *published a release* from the `host` job even though the gate
   failed and the build jobs were skipped — i.e. a **binary-less release** (only
   `dist-manifest.json`). That broken release + tag were deleted and the tag
   re-pushed; the clean re-run (`27799155867`) went green with all 4
   musl/darwin tarballs + checksums + installer + source archive. *(If the
   disk-exhaustion recurs, add a free-disk-space step to the quality-gate
   workflow; it was a one-off here.)*

## 7. Open questions to resolve in-phase (not blockers to CR.0)

- **Change Date granularity** — per-release 4-year clocks (chosen above) vs. a
  single global date. Per-release is the MariaDB/Sentry norm and is assumed.
- **DCO vs. CLA** (CR.4) — ✅ **RESOLVED: CLA.** A plain DCO certifies origin only
  and grants no commercial-sublicensing right, so it cannot support the paid-license
  model; the CLA does. Acceptance is folded into the `git commit -s` sign-off (one
  trailer certifies the DCO *and* accepts the CLA), keeping friction near-DCO-low.
  *Open follow-up:* have a lawyer review `CLA.md` before the first external PR is
  actually merged — it is a sound Apache-ICLA-derived draft, not legal advice.
- **First BSL version number** (CR.6) — ✅ **RESOLVED: v0.3.0.** Incremental from
  v0.2.0; honest about pre-1.0 maturity (v0.2.0 shipped "early pre-release"). The
  license change is the release headline regardless of the number; v1.0 is held
  for a real stability milestone.
- **crates.io** — not currently a distribution channel (releases are cargo-dist
  binaries + GHCR), so its OSI-license preference doesn't bind us today; revisit
  only if/when publishing crates.

## 8. Licensing FAQ

Plain-English answers to the common questions. The buyer-facing version lives in
[`COMMERCIAL.md`](../COMMERCIAL.md); the authoritative terms are in
[`LICENSE`](../LICENSE).

**Is Aivyx PA open source?**
No — it is **source-available** under BUSL-1.1. The full source is public,
readable, and forkable for non-commercial use, and every version converts to MIT
(true open source) four years after it ships. But while under BUSL it is *not*
OSI-approved "open source," and we don't call it that.

**Can I use Aivyx PA for free?**
Yes, for **personal, individual, non-commercial, educational, and research** use
— including a non-profit or an accredited school. No payment, no sign-up.

**Can I use it at work / in my company?**
Not for free. **Any use by or on behalf of a for-profit entity needs a
[commercial license](../COMMERCIAL.md)** — there is no "internal use is free"
carve-out. If a business depends on it, the business licenses it.

**What counts as "commercial"?**
Use primarily intended for or directed toward commercial advantage or monetary
compensation: for-profit internal use, production use behind a paid product or
service, anything that generates revenue, and offering Aivyx PA to third parties
(hosted, embedded, or resold). See [`COMMERCIAL.md`](../COMMERCIAL.md) for the
quick-check table.

**When does it become MIT?**
Each released version auto-converts to the [MIT License](../LICENSES/MIT.txt)
**four years after that version is published** — its own clock. After that, that
version has no restrictions at all.

**What about the versions already released under MIT?**
v0.2.0 and every prior commit are **MIT in perpetuity** — a license can't be
revoked. The relicense applies only from the first BSL-tagged release forward.

**Can I fork it?**
Yes. The BUSL grant lets you fork, modify, and redistribute for non-commercial
use; commercial use of your fork still needs a commercial license. Either way,
**rename it** — "Aivyx" and "Aivyx PA" are [trademarks](../TRADEMARK.md), separate from the code
license.

**How do I get a commercial license?**
Email **aivyx@aivyx-studio.com** — details in [`COMMERCIAL.md`](../COMMERCIAL.md).

---

*Chapter Charter is the governance/licensing chapter: it changes the terms under
which Aivyx PA is offered, not the code's behavior. It is the monetization hook the
open-core roadmap ([[aivyx-ecosystem-roadmap]]) was missing on the core itself.*
