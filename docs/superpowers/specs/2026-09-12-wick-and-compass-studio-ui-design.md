# "Wick & Compass" — Studio UI Propagation (Design)

## Context

Sub-project 3 of a 5-part ecosystem rebrand (1: identity definition —
done; 2: `aivyx-brand` deliverables — done; **3: this one**; 4:
`aivyx-coder` visual surface; 5: `aivyx-website` redesign), itself
carved out of sub-project 4 of the larger pre-release go-to-market push.

Source of truth for what this propagates:
`aivyx-brand/docs/superpowers/specs/2026-09-11-wick-and-compass-identity-design.md`
(direction/palette/type/mark) and
`.../2026-09-11-wick-and-compass-tokens-design.md` (the concrete values,
now live in `aivyx-brand`'s real files as of sub-project 2).

This sub-project lands that identity in `aivyx-pa`'s real Studio UI —
the `aivyx-web` crate (Dioxus/wasm), built via `just build-web` and
embedded into the daemon binary. Unlike sub-project 2, this is
production code with a real build step, not static assets.

## Scope

**In scope**, grounded against the real `aivyx-web` crate (a single
10,711-line `src/main.rs`, a 507-line `src/guide.rs` unrelated to visual
identity, and a 1,091-line `assets/stitch.css` whose own header already
says "transcribed verbatim from `aivyx-brand/design-tokens.md`"):

1. **Token retranscription** — `assets/stitch.css`'s `:root` and
   `[data-theme="light"]` blocks, rewritten from `aivyx-brand`'s now-live
   `design-tokens.md`, mirroring the exact convention the file's own
   header already documents.
2. **Surface system** — `.glass-panel`/`.glass-card`/`.glass-header`
   (backdrop-blur, 112 class-reference call-sites in `main.rs`, zero of
   which need individual edits since they're all CSS-class-driven) become
   flat surfaces with real borders, mirroring `aivyx-brand`'s Glass &
   Depth retirement.
3. **Font/icon/logo assets** — 3 font files
   (`space-grotesk-var.woff2`/`inter-var.woff2`/`jetbrains-mono-var.woff2`)
   replaced by Fraunces/IBM Plex Sans/IBM Plex Mono variable fonts,
   sourced the same way `aivyx-brand`'s own wordmark task did (a real
   font file, not a CDN reference — `aivyx-web` has no network
   dependency at runtime). ~26 icon/logo SVGs
   (`assets/icons/*.svg`, `assets/logos/*.svg`) replaced by their
   now-finished `aivyx-brand` equivalents.
4. **Four indicator components restyled** with the dial motif (all
   confirmed to be small, CSS-class-level changes against the real
   code, not deep rewrites):
   - Trust-tier chips (`tier_chip_class()`, `main.rs:5596`)
   - Audit chain status (`main.rs:3405-3406`)
   - Notification count badge (`main.rs:1561-1564`)
   - Daemon connection status dot (`main.rs`'s `statusbar` footer,
     `.dot`/`.seg.live` classes)
5. **A real build verification** — `dx bundle --release --platform web`
   (per `justfile`'s `build-web` target), confirming `dist/` regenerates
   and the app still functions.

**Explicitly out of scope:**
- `src/guide.rs` and the 507 lines of guide-content-rendering logic —
  unrelated to visual identity, confirmed by reading it.
- The 12 stray `--danger`/`--ok`/`--warn` hardcoded hex fallbacks in
  `main.rs` (`#b91c1c`/`#16a34a`/`#d97706`) — these are pre-existing,
  ad-hoc semantic colors unrelated to the Stitch token system (the CSS
  custom properties they fall back from, `--danger`/`--ok`/`--warn`,
  aren't defined anywhere in `stitch.css` either). Real pre-existing
  tech debt, genuinely unrelated to this rebrand — not touched here.
- Any restructuring of `main.rs`'s overall layout/architecture beyond
  the 4 named component restyles.
- New UI screens, new features, or behavior changes beyond visual
  restyling.

## Token retranscription

Mechanical: `stitch.css`'s existing `:root` block gets replaced with
`aivyx-brand/design-tokens.md`'s current Wick & Compass values,
following the exact same category structure `stitch.css` already uses
(Surfaces, Message Backgrounds, Text, Primary→Brass, Secondary→Rust,
Tertiary→Slate, Borders, Semantic, Shadows) — this is the same content
already transcribed once for `aivyx-brand`'s own `design-tokens.md`
rewrite in sub-project 2, now transcribed a second time into this file,
matching the file's own documented convention.

A `[data-theme="light"]` override block (Drafting Table) gets the light
column from the same source table. Confirm during implementation
whether `stitch.css` already has a light-theme override block to modify
in place, or whether one needs to be added — the file's header claims
dark-is-default with a light override, matching the token table's own
dark/light column structure, so this should already exist; verify
rather than assume.

## Surface system (Glass & Depth retirement)

`stitch.css`'s `.glass-panel`, `.glass-card`, `.glass-header` rules
(currently `background: rgba(...); backdrop-filter: blur(Npx);`) get
replaced with flat surface-tier backgrounds + real borders, mirroring
`aivyx-brand/brand-guidelines.md` §5's already-established rule:
`background: var(--color-bg-elevated)` (or `-raised`/`-surface`
depending on which class), `border: 1px solid var(--color-border-subtle)`,
no `backdrop-filter`. Since every one of the 112 `main.rs` call-sites
references these classes by name (`class: "glass-card"` etc.), redefining
the 3 CSS rules propagates everywhere with zero Rust changes.

## Font/icon/logo assets

**Fonts**: replace the 3 `.woff2` files under `assets/fonts/` with real
Fraunces/IBM Plex Sans/IBM Plex Mono variable font files (same
`opentype.js`-free direct-download approach `aivyx-brand`'s Task 6
established is reachable in this environment — no CDN, no live network
dependency at `aivyx-web` runtime, matching its existing
self-hosted-fonts constraint). Rename the files themselves
(`fraunces-var.woff2`/`ibm-plex-sans-var.woff2`/`ibm-plex-mono-var.woff2`)
rather than keeping the old, now-misleading names — this means updating
the 3 `asset!()` const declarations in `main.rs`
(`FONT_DISPLAY`/`FONT_BODY`/`FONT_MONO`, `main.rs:63-65`) and the
`@font-face` rules in `stitch.css` that reference them.

**Icons/logos**: `assets/icons/*.svg` (23 files) and
`assets/logos/*.svg` (3 files) get replaced with their finished
`aivyx-brand` counterparts (`icons/*/*.svg`, `logos/*.svg` there),
recolored/regenerated per sub-project 2's work. Confirm during
implementation that every filename in `aivyx-web/assets/icons/` has a
real counterpart in `aivyx-brand/icons/` before copying — the two
directories were never guaranteed to be 1:1 (`aivyx-web` has some icons
`aivyx-brand` doesn't, e.g. `documents.svg`/`gallery.svg`/`schedules.svg`
per the earlier directory listing) — files with no `aivyx-brand`
counterpart get recolored in place (their `stroke="currentColor"`
convention means this may already be free, per the pattern established
in sub-project 2's Task 4) rather than skipped or invented from scratch
without grounding.

## Four indicator components

All four are CSS-class restyles plus, where noted, one small added
`<span>` — not new component architecture.

**Trust-tier chips** (`main.rs:5596`, `tier_chip_class()`): currently
returns `"chip error"`/`"chip sage"`/`"chip amber"`/`"chip muted"`.
`.chip.sage` references `var(--color-sage)`, a token retired in
sub-project 2 — already broken today, not just stale-looking. Rename
the returned strings to match Wick & Compass semantics —
`"chip error"`/`"chip success"`/`"chip warning"`/`"chip muted"` — since
these are real Rust identifiers in actively-maintained code, not a
static asset; `aivyx-brand` fully retired spice-rack naming rather than
aliasing it forward into new work, and the same discipline applies here.
Update `.chip.success`/`.chip.warning` CSS rules to real semantic-color
values (not brass/rust, matching the semantic-independence rule).
Visually, each chip gains a small leading ring-dot
(`<span class="chip-dot">`, a 6-8px circle with a 1px brass or
semantic-color border) before its label — a calibrated-reading cue,
not a redesign of the chip's layout.

**Audit chain status** (`main.rs:3405-3406`): currently plain colored
text via the non-Stitch `--ok`/`--danger` fallback pattern (out of
scope per above — leave those specific color values alone). Add a small
inline dial-ring glyph (reuse the same simple circle-plus-mark SVG
shape `aivyx-brand`'s `icons/status/{success,error}.svg` already use,
copied from there) immediately before the "chain intact"/"verification
failed" text, sized to match the surrounding text's line-height.

**Notification count badge** (`main.rs:1561-1564`): currently a solid
`background: var(--danger, #b91c1c)` filled circle. Add a 1px brass
border (`border: 1px solid var(--color-primary)`) around the existing
circle — the fill stays semantic-error-red (it's a count of unread
*items*, not a status reading, so keeping error-red as the attention
color is correct; only the ring treatment is new).

**Daemon connection status** (`statusbar` footer, `.dot`/`.seg.live`
classes): currently a solid dot, color presumably toggling between a
connected/disconnected state via the `.live` class modifier — confirm
the exact current CSS during implementation. Add the same brass-ring
treatment as the notification badge: a 1px border in brass when
connected, slate when offline, around the existing solid dot.

## Build verification

After all asset/CSS/Rust changes: `just build-web` (wraps
`rustup target add wasm32-unknown-unknown` + `dx bundle --release
--platform web`, then repopulates `dist/`). Confirm the build succeeds
with no errors, `dist/assets/stitch-<newhash>.css` exists with a
different hash than before (proving the CSS actually changed and got
rebundled, not silently cached), and the previously-noted MEMORY fact
that this toolchain is genuinely reachable in this environment holds —
verify fresh rather than assume. `just check-web` (the cheaper
compile-only guard) can be used for faster iteration between the
individual sub-steps above, with the real `build-web` bundle reserved
for final verification.

## What this spec does not decide

- Exact new hex/rgba values for the retranscribed token block — copy
  verbatim from `aivyx-brand/design-tokens.md`'s current state at
  implementation time (do not re-derive from this document's earlier
  sub-project 2 narrative, in case anything changed since).
- Exact SVG markup for the chip-dot / audit-status glyph — implementation
  detail, reuse `aivyx-brand`'s existing status icon shapes rather than
  inventing new geometry.
- The precise current CSS for `.dot`/`.seg.live` (not read in full during
  this brainstorm) — ground it fresh during implementation before editing.
- Whether any of the ~26 icon files without a direct `aivyx-brand`
  counterpart need bespoke new artwork vs. a simple recolor — decide
  per-file during implementation, grounded against each file's real
  current content.

## Downstream

Sub-project 4 (`aivyx-coder`'s visual surface — likely small, a TUI has
much less brand-visual surface than a web UI) and sub-project 5
(`aivyx-website` redesign, now executed with real Studio UI screenshots
reflecting this sub-project's work rather than the old Neon Cartographer
`command-center.png`) both come after this one.
