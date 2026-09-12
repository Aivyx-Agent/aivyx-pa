# "Wick & Compass" — Studio UI Propagation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the Wick & Compass identity in `aivyx-pa`'s real Studio UI (`crates/aivyx-web`) — tokens, surfaces, fonts, icons/logos, and status-indicator styling — with a real wasm build proving it compiles and bundles.

**Architecture:** All work happens in `crates/aivyx-web/`. Tasks are ordered so the token layer lands first (everything else depends on it), the CSS-surface and retired-name migrations happen next (both touch `assets/stitch.css` extensively and must not race each other), asset swaps follow, then the smaller indicator-glyph additions, then a final real build.

**Tech Stack:** Rust/Dioxus (`main.rs`), plain CSS (`stitch.css`), SVG assets, `dx bundle` (Dioxus CLI) via `just build-web`/`just check-web`.

## Global Constraints

- `src/guide.rs` (507 lines, guide-content rendering) is out of scope — do not touch.
- The 12 `--danger`/`--ok`/`--warn` hardcoded hex fallbacks in `main.rs` (`#b91c1c`/`#16a34a`/`#d97706`) are pre-existing, unrelated tech debt — explicitly out of scope, do not touch.
- No new UI screens, features, or behavior changes beyond what each task names.
- Font files must be real downloaded font files (same technique `aivyx-brand`'s wordmark task used) — no CDN references, `aivyx-web` has zero runtime network dependency.
- Every color value used must trace to a real token in the retranscribed `stitch.css` — no invented hex anywhere.
- `crates/aivyx-web/dist/` is committed but always regenerated fresh by `just build-web` (which itself does `rm -rf dist && mkdir -p dist`) — never hand-edit anything under `dist/`.
- Real verification throughout: `cargo build -p aivyx-web --target wasm32-unknown-unknown` (via `just check-web`) after Rust-touching tasks; the full `just build-web` bundle only at the final task.

---

### Task 1: Branch setup + token retranscription

**Files:**
- Modify: `crates/aivyx-web/assets/stitch.css:1-146` (header comment, `:root`, `[data-theme="light"]`)

**Interfaces:** Produces every CSS custom property later tasks and the
existing 10,711 lines of `main.rs` read via `var(--color-*)`/`var(--font-*)`.
No task in this plan should introduce a color not defined here.

- [ ] **Step 1: Create the branch**

```bash
cd /home/julian/Projects/Rust/aivyx-pa
git checkout -b wick-and-compass-studio-ui
```

- [ ] **Step 2: Replace `crates/aivyx-web/assets/stitch.css` lines 1-146 in full**

```bash
python3 << 'PYEOF'
with open('crates/aivyx-web/assets/stitch.css') as f:
    content = f.read()

old_start = content.index('/* ──')
old_end = content.index('/* ── Reset + base')
new_header = '''/* ──────────────────────────────────────────────────────────────────────────
   Aivyx — Stitch design system (Chapter R)
   Wick & Compass: precision instrument, dark reads as Field Instrument,
   light reads as Drafting Table.
   Tokens transcribed verbatim from aivyx-brand/design-tokens.md — the single
   source of truth. Dark is default; [data-theme="light"] overrides.
   ────────────────────────────────────────────────────────────────────────── */

/* Self-hosted @font-face rules (Fraunces / IBM Plex Sans / IBM Plex Mono,
   OFL, variable, latin) are injected at runtime from main.rs with hashed
   asset!() paths — every asset must go through asset!() to be bundled. No CDN. */

/* ── Tokens — dark (default) ───────────────────────────────────────────────── */
:root {
  /* Surfaces — the layers */
  --color-bg-void: #0a0d13;
  --color-bg-base: #12161f;
  --color-bg-surface: #1a1f2b;
  --color-bg-raised: #212736;
  --color-bg-elevated: #2a3140;
  --color-bg-float: #333c4d;
  --color-bg-input: #212736;
  --color-bg-code: #0a0d13;

  /* Message backgrounds */
  --color-msg-user: #17222c;
  --color-msg-assistant: #241a15;

  /* Text */
  --color-text-primary: #e8e2d0;
  --color-text-secondary: #a8b0bc;
  --color-text-disabled: #5c6570;
  --color-text-inverse: #12161f;

  /* Primary — brass */
  --color-primary: #c9a24b;
  --color-primary-hover: #ddb968;
  --color-primary-dim: #8a6f34;
  --color-primary-pale: #f0e2c0;
  --color-primary-deep: #5c4a24;
  --color-accent: #c9a24b;
  --color-accent-hover: #ddb968;
  --color-accent-glow: #e0bf7a;

  /* Secondary — rust */
  --color-secondary: #b5432b;
  --color-secondary-hover: #c95a3f;
  --color-secondary-dim: #7a2e1d;
  --color-secondary-pale: #f5ded7;
  --color-secondary-deep: #4a1a11;

  /* Tertiary — slate */
  --color-tertiary: #8a95a1;
  --color-tertiary-dim: #5c6570;
  --color-tertiary-deep: #2a3138;

  /* Borders — ruled, not ghost */
  --color-border-active: #c9a24b;
  --color-border-subtle: #333c4d;
  --color-border-ghost: rgba(138, 149, 161, 0.10);
  --color-border-warm: rgba(201, 162, 75, 0.06);

  /* Outline */
  --color-outline: #8a95a1;
  --color-outline-variant: #3d4550;

  /* Semantic — independent of brass/rust */
  --color-success: #4d8b6a;
  --color-success-dim: #3a7050;
  --color-success-pale: #6fae86;
  --color-error: #d64545;
  --color-error-pale: #e37868;
  --color-error-deep: #7a1f1f;
  --color-warning: #d97b29;
  --color-info: #5a7a9a;

  /* Shadows */
  --shadow-sm: 0 1px 3px rgba(0,0,0,0.4);
  --shadow-md: 0 4px 12px rgba(0,0,0,0.5);
  --shadow-ambient: 0 40px 40px -5px rgba(18,22,31,0.15);
  --shadow-glow: 0 0 15px rgba(201,162,75,0.2);
  --shadow-wick: 0 0 12px rgba(181,67,43,0.25);

  /* Transitions + layout */
  --ease-smooth: cubic-bezier(0.4, 0, 0.2, 1);
  --sidebar-width: 220px;
  --tray-width: 300px;
  --status-height: 36px;

  /* Type */
  --font-display: 'Fraunces', serif;
  --font-body: 'IBM Plex Sans', system-ui, sans-serif;
  --font-mono: 'IBM Plex Mono', ui-monospace, SFMono-Regular, Menlo, monospace;

  /* Backward-compat aliases — remove once every consumer below is migrated
     off retired Neon-Cartographer-era names (Task 4 of this plan migrates
     every real usage in this file; these aliases exist only as a safety
     net matching aivyx-brand/design-tokens.md's own convention). */
  --color-bg-deep: var(--color-bg-base);
  --color-bg-secondary: var(--color-bg-base);
  --color-bg-primary: var(--color-bg-surface);
  --color-terracotta: var(--color-secondary);
  --color-terracotta-hover: var(--color-secondary-hover);
  --color-sage: var(--color-success);
  --color-sage-dim: var(--color-success);
  --color-plum: var(--color-secondary);
  --color-plum-dim: var(--color-secondary-dim);
  --color-sienna: var(--color-tertiary);
  --color-sienna-hover: var(--color-tertiary);
  --color-teal: var(--color-info);
  --color-teal-dim: var(--color-info);
  --color-accent-core: var(--color-secondary);
}

/* ── Tokens — light ────────────────────────────────────────────────────────── */
[data-theme="light"] {
  --color-bg-void: #e3dcc8;
  --color-bg-base: #f4efe4;
  --color-bg-surface: #faf6ee;
  --color-bg-raised: #ffffff;
  --color-bg-elevated: #ffffff;
  --color-bg-float: #ffffff;
  --color-bg-input: #ffffff;
  --color-bg-code: #ece5d4;

  --color-msg-user: #e4ecf0;
  --color-msg-assistant: #fbeee7;

  --color-text-primary: #1f3b4d;
  --color-text-secondary: #55707c;
  --color-text-disabled: #9aa3a8;
  --color-text-inverse: #f4efe4;

  --color-primary: #a9822f;
  --color-primary-hover: #8f6c24;
  --color-primary-dim: #8f6c24;
  --color-primary-pale: #faf1dc;
  --color-accent: #a9822f;
  --color-accent-hover: #8f6c24;
  --color-accent-glow: #c0954a;

  --color-secondary: #963823;
  --color-secondary-hover: #a84428;
  --color-secondary-dim: #7a2e1d;
  --color-secondary-pale: #fbeee7;

  --color-tertiary: #5c6570;
  --color-tertiary-dim: #8a95a1;

  --color-border-active: #a9822f;
  --color-border-subtle: #ddd5c2;
  --color-border-ghost: rgba(31, 59, 77, 0.08);
  --color-border-warm: rgba(169, 130, 47, 0.06);

  --color-outline: #6b6055;
  --color-outline-variant: #c9bfa8;

  --color-success: #2f7a52;
  --color-success-dim: #245e40;
  --color-success-pale: #5e9879;
  --color-error: #b6362a;
  --color-error-pale: #c96a61;
  --color-error-deep: #7a1f1f;
  --color-warning: #b8791e;
  --color-info: #47637f;

  --shadow-sm: 0 1px 3px rgba(0,0,0,0.08);
  --shadow-md: 0 4px 12px rgba(0,0,0,0.1);
  --shadow-ambient: 0 40px 40px -5px rgba(31,59,77,0.05);
  --shadow-glow: 0 0 15px rgba(169,130,47,0.12);
  --shadow-wick: 0 0 12px rgba(150,56,35,0.15);
}

'''

content = content[:old_start] + new_header + content[old_end:]

with open('crates/aivyx-web/assets/stitch.css', 'w') as f:
    f.write(content)
PYEOF
```

- [ ] **Step 3: Verify the token block is well-formed and complete**

```bash
cd crates/aivyx-web
grep -c "^  --" assets/stitch.css
grep -n "^:root {" assets/stitch.css
grep -n '^\[data-theme="light"\] {' assets/stitch.css
```

Expected: a real count of custom-property lines (well over 60 across both
blocks), exactly one `:root {` line, exactly one `[data-theme="light"] {`
line.

- [ ] **Step 4: Confirm the workspace still compiles for the wasm target**

```bash
cd /home/julian/Projects/Rust/aivyx-pa
rustup target add wasm32-unknown-unknown
cargo build -p aivyx-web --target wasm32-unknown-unknown
```

Expected: builds clean (CSS changes don't affect Rust compilation, but
this confirms the toolchain itself is genuinely reachable before later
tasks depend on it).

- [ ] **Step 5: Commit**

```bash
git add crates/aivyx-web/assets/stitch.css
git commit -m "feat: retranscribe stitch.css tokens to Wick & Compass

Full :root + [data-theme=\"light\"] rewrite from aivyx-brand/design-tokens.md's
current live values. Includes the same Backward Compatibility Aliases
block aivyx-brand's own file carries, so every retired-name var() usage
in this file keeps resolving (to the new values) until Task 4 migrates
each call site to the real new names."
```

---

### Task 2: Glass & Depth retirement

**Files:**
- Modify: `crates/aivyx-web/assets/stitch.css` (`.topbar`, `.btn-glass`, `.glass-card` rules)

**Interfaces:** Consumes Task 1's `--color-bg-elevated`/`--color-border-subtle`.
Produces the flat-surface convention every one of `.glass-card`'s real
112 call-sites in `main.rs` inherits automatically (class-driven, zero
Rust edits needed).

Note the file already has a `.card` class (a few lines above `.glass-card`)
that is *already* flat — `background: var(--color-bg-raised); border: 1px
solid var(--color-border-ghost);` — this task brings `.glass-card` in line
with that existing pattern rather than inventing new flat-surface CSS,
while using `border-subtle` (not `border-ghost`) per the "ruled, not
ghost" rule.

- [ ] **Step 1: Replace `.topbar`'s blur**

```bash
cd /home/julian/Projects/Rust/aivyx-pa/crates/aivyx-web
python3 << 'PYEOF'
with open('assets/stitch.css') as f:
    content = f.read()

old = '''.topbar {
  display: flex; align-items: center; gap: 16px;
  padding: 0 22px; height: 56px;
  background: rgba(19,19,25,0.7); backdrop-filter: blur(20px);
  border-bottom: 1px solid var(--color-border-ghost);
}'''

new = '''.topbar {
  display: flex; align-items: center; gap: 16px;
  padding: 0 22px; height: 56px;
  background: var(--color-bg-surface);
  border-bottom: 1px solid var(--color-border-subtle);
}'''

assert old in content, "old .topbar rule not found verbatim -- stop and check stitch.css manually"
content = content.replace(old, new)

with open('assets/stitch.css', 'w') as f:
    f.write(content)
PYEOF
```

- [ ] **Step 2: Replace `.btn-glass`'s blur**

```bash
python3 << 'PYEOF'
with open('assets/stitch.css') as f:
    content = f.read()

old = '''.btn-glass {
  background: rgba(53,52,59,0.4); backdrop-filter: blur(12px);
  border: 1px solid var(--color-border-ghost); color: var(--color-text-primary);
}'''

new = '''.btn-glass {
  background: var(--color-bg-elevated);
  border: 1px solid var(--color-border-subtle); color: var(--color-text-primary);
}'''

assert old in content, "old .btn-glass rule not found verbatim -- stop and check stitch.css manually"
content = content.replace(old, new)

with open('assets/stitch.css', 'w') as f:
    f.write(content)
PYEOF
```

- [ ] **Step 3: Replace `.glass-card`'s blur**

```bash
python3 << 'PYEOF'
with open('assets/stitch.css') as f:
    content = f.read()

old = '''.glass-card {
  background: rgba(53,52,59,0.4); backdrop-filter: blur(12px);
  border: 1px solid var(--color-border-warm);
  border-radius: 0.5rem; padding: 16px;
  /* POLISH_WAVES.md sub-project 6, item A — the brand guide's own Shadow
     System table assigns shadow-md to "Cards"; this token existed but was
     never applied to the card class itself, only to overlay chrome
     (command palette, mobile nav drawer). */
  box-shadow: var(--shadow-md);
}'''

new = '''.glass-card {
  background: var(--color-bg-elevated);
  border: 1px solid var(--color-border-subtle);
  border-radius: 0.5rem; padding: 16px;
  /* POLISH_WAVES.md sub-project 6, item A — the brand guide's own Shadow
     System table assigns shadow-md to "Cards"; this token existed but was
     never applied to the card class itself, only to overlay chrome
     (command palette, mobile nav drawer). */
  box-shadow: var(--shadow-md);
}'''

assert old in content, "old .glass-card rule not found verbatim -- stop and check stitch.css manually"
content = content.replace(old, new)

with open('assets/stitch.css', 'w') as f:
    f.write(content)
PYEOF
```

- [ ] **Step 4: Verify no blur remains anywhere in the file**

```bash
grep -n "backdrop-filter\|rgba(53,52,59\|rgba(19,19,25" assets/stitch.css
```

Expected: no output.

- [ ] **Step 5: Confirm the workspace still compiles**

```bash
cd /home/julian/Projects/Rust/aivyx-pa
cargo build -p aivyx-web --target wasm32-unknown-unknown
```

- [ ] **Step 6: Commit**

```bash
git add crates/aivyx-web/assets/stitch.css
git commit -m "feat: retire glass/blur surfaces in stitch.css

.topbar, .btn-glass, and .glass-card were the only 3 real backdrop-filter
sites in the file (confirmed by grep -- .glass-panel/.glass-header never
existed as real classes despite the design spec's original assumption).
All 3 now match the already-flat .card class's pattern: solid surface-tier
background + a real border-subtle rule, no blur. .glass-card's 112 class
references in main.rs need zero edits since this is purely a CSS-class
redefinition."
```

---

### Task 3: Depth-background retirement (body gradient, hover glow, dead keyframes)

**Files:**
- Modify: `crates/aivyx-web/assets/stitch.css` (`body`'s background rule, `.btn-primary:not(:disabled):hover`, `@keyframes pulse-glow`, `@keyframes candle-flicker`)

**Interfaces:** Consumes Task 1's `--shadow-glow` token. Produces
nothing consumed by later tasks — this is a sibling cleanup to Task 2,
found by that task's own reviewer rather than named in the original
design spec.

Found during Task 2's review: `body`'s background rule still carries an
organic radial-gradient "depth" effect tagged with the literal comment
`/* bg-depth: never flat */`, using two hardcoded retired-palette rgba
values (`rgba(204,193,230,...)`, the old cyber-purple secondary;
`rgba(255,183,125,...)`, the old amber primary). `aivyx-brand/brand-
guidelines.md` §5 already retired this *exact* pattern by its own
internal name: *"Noise overlay and organic gradient depth (`bg-depth`,
`texture-noise`) are dropped — no replacement, flat is the replacement."*
The same old amber rgba value also lingers in `.btn-primary:hover`'s
box-shadow glow and an unused `@keyframes pulse-glow` (confirmed via
`grep` — genuinely never referenced by any `animation:` property in
`main.rs`). A `@keyframes candle-flicker` (also confirmed unused) uses
the exact stale "candle" naming `aivyx-brand`'s own rebrand already
renamed to `wick-flicker` for the identical reason.

- [ ] **Step 1: Retire `body`'s organic-gradient depth background**

```bash
cd /home/julian/Projects/Rust/aivyx-pa/crates/aivyx-web
python3 << 'PYEOF'
with open('assets/stitch.css') as f:
    content = f.read()

old = '''  /* bg-depth: never flat — subtle organic gradients over the base */
  background:
    radial-gradient(ellipse at 30% 20%, rgba(204,193,230,0.03), transparent 50%),
    radial-gradient(ellipse at 70% 80%, rgba(255,183,125,0.02), transparent 50%),
    var(--color-bg-base);
  background-attachment: fixed;'''

new = '''  background: var(--color-bg-base);'''

assert old in content, "old body background rule not found verbatim -- stop and check stitch.css manually"
content = content.replace(old, new)

with open('assets/stitch.css', 'w') as f:
    f.write(content)
PYEOF
```

`background-attachment: fixed` is dropped along with the gradient — it
existed only to keep the (now-removed) gradient anchored during scroll,
serving no purpose against a flat single color.

- [ ] **Step 2: Migrate `.btn-primary:hover`'s glow to the real token**

```bash
python3 << 'PYEOF'
with open('assets/stitch.css') as f:
    content = f.read()

old = '.btn-primary:not(:disabled):hover { box-shadow: 0 0 15px 0 rgba(255,183,125,0.3); }'
new = '.btn-primary:not(:disabled):hover { box-shadow: var(--shadow-glow); }'

assert old in content, "old .btn-primary hover rule not found verbatim -- stop and check stitch.css manually"
content = content.replace(old, new)

with open('assets/stitch.css', 'w') as f:
    f.write(content)
PYEOF
```

- [ ] **Step 3: Fix the dead `pulse-glow` keyframe's color and rename `candle-flicker`**

```bash
python3 << 'PYEOF'
with open('assets/stitch.css') as f:
    content = f.read()

old_pulse = '@keyframes pulse-glow { 0%,100% { box-shadow: 0 0 0 0 rgba(255,183,125,0.4); } 50% { box-shadow: 0 0 0 6px rgba(255,183,125,0); } }'
new_pulse = '@keyframes pulse-glow { 0%,100% { box-shadow: 0 0 0 0 rgba(201,162,75,0.4); } 50% { box-shadow: 0 0 0 6px rgba(201,162,75,0); } }'

old_candle = '@keyframes candle-flicker { 0%,100% { opacity: 1; } 45% { opacity: 0.85; } 70% { opacity: 0.92; } }'
new_candle = '@keyframes wick-flicker { 0%,100% { opacity: 1; } 45% { opacity: 0.85; } 70% { opacity: 0.92; } }'

assert old_pulse in content, "old pulse-glow keyframe not found verbatim -- stop and check stitch.css manually"
assert old_candle in content, "old candle-flicker keyframe not found verbatim -- stop and check stitch.css manually"
content = content.replace(old_pulse, new_pulse).replace(old_candle, new_candle)

with open('assets/stitch.css', 'w') as f:
    f.write(content)
PYEOF
```

Both keyframes are confirmed unused today (no `animation:` property in
`main.rs` references either name) — this fix keeps them correctly
themed for whenever they're picked up, rather than deleting dead CSS
that isn't this task's concern to prune.

- [ ] **Step 4: Verify**

```bash
grep -n "bg-depth\|rgba(204,193,230\|rgba(255,183,125\|candle-flicker\|background-attachment" assets/stitch.css
```

Expected: no output.

- [ ] **Step 5: Confirm the workspace compiles**

```bash
cd /home/julian/Projects/Rust/aivyx-pa
cargo build -p aivyx-web --target wasm32-unknown-unknown
```

- [ ] **Step 6: Commit**

```bash
git add crates/aivyx-web/assets/stitch.css
git commit -m "feat: retire the last old-palette depth/glow/keyframe leftovers

Found during Task 2's review: body's organic-gradient 'bg-depth'
background (literally named that in its own comment) is the same
pattern aivyx-brand/brand-guidelines.md SS5 already retired by name --
'no replacement, flat is the replacement'. .btn-primary:hover's glow
and an unused pulse-glow keyframe both still used the old amber rgba
value; migrated to --shadow-glow and its brass equivalent respectively.
An unused candle-flicker keyframe renamed to wick-flicker, matching the
identical rename aivyx-brand's own rebrand already made."
```

---

### Task 4: Retired-name CSS class + Rust identifier migration

**Files:**
- Modify: `crates/aivyx-web/assets/stitch.css` (11 `var(--color-sage)` usages, `.chip.sage`/`.chip.amber`, `.mission-node.sage`/`.mission-node.amber`, `.btn-sage`)
- Modify: `crates/aivyx-web/src/main.rs:5596-5602` (`tier_chip_class`), `:3888-3893` (mission-node state), `:4216` (memory-conflict flag), `:4347,4352` (seed-card buttons)

**Interfaces:** Consumes Task 1's `--color-success`/`--color-warning`
tokens. Produces the final class names (`chip success`/`chip warning`/
`mission-node success`/`mission-node warning`/`btn-success`) that Task 7's
indicator work builds on for the trust-tier-chip half of its scope (Task
6 covers the other 3 named indicators — audit chain, notification badge,
daemon dot — trust-tier chips are fully handled here since they share
classes with the memory-conflict flag fixed in this same task).

This must be one atomic task — a partial rename would leave some
call-sites referencing a CSS class name the stylesheet no longer defines.

**Grounding this task relies on** (already confirmed real, not assumed):
`.chip.sage` and `.chip.amber` are shared between two semantically
unrelated features — trust-tier chips (`tier_chip_class`) and a
"contradictory entries" memory-conflict warning flag
(`main.rs:4216`) — both happen to want a positive/warning-flavored
color respectively, so sharing the renamed classes is correct, not a
forced merge. `.mission-node.amber` already maps to `var(--color-warning)`
in CSS despite being named "amber" in the Rust match arm at
`main.rs:3888` — the rename here corrects a real naming/value mismatch
that predates this rebrand, not just a coat of paint.

- [ ] **Step 1: Rename CSS rules in `stitch.css`**

```bash
cd /home/julian/Projects/Rust/aivyx-pa/crates/aivyx-web
python3 << 'PYEOF'
with open('assets/stitch.css') as f:
    content = f.read()

replacements = [
    # Bare var() usages -- every real instance maps success/positive/live,
    # confirmed by reading each one's context during planning.
    ('.stat-card .value.ok { color: var(--color-sage); }',
     '.stat-card .value.ok { color: var(--color-success); }'),
    ('.agent-status .kv .v.ok { color: var(--color-sage); }',
     '.agent-status .kv .v.ok { color: var(--color-success); }'),
    ('.notice.ok   { background: rgba(77, 139, 106, 0.12); color: var(--color-sage); }',
     '.notice.ok   { background: rgba(77, 139, 106, 0.12); color: var(--color-success); }'),
    ('.seed-card { border-left: 3px solid var(--color-sage); }',
     '.seed-card { border-left: 3px solid var(--color-success); }'),
    ('.step-dot.done { border-color: var(--color-sage); color: var(--color-sage); }',
     '.step-dot.done { border-color: var(--color-success); color: var(--color-success); }'),
    ('.statusbar .seg.live .dot { background: var(--color-sage); }',
     '.statusbar .seg.live .dot { background: var(--color-success); }'),
    ('.agent-status .dot.live, .routine-row .dot.live {\n  background: var(--color-sage); animation: dot-pulse 2s ease-in-out infinite;\n}',
     '.agent-status .dot.live, .routine-row .dot.live {\n  background: var(--color-success); animation: dot-pulse 2s ease-in-out infinite;\n}'),
    # Class renames -- chip.sage/chip.amber shared with the memory-conflict
    # flag; mission-node.sage/mission-node.amber shared with nothing else.
    ('.chip.amber { background: rgba(255,183,125,0.12); color: var(--color-primary); }',
     '.chip.warning { background: rgba(217,123,41,0.15); color: var(--color-warning); }'),
    ('.chip.sage  { background: rgba(77,139,106,0.15); color: var(--color-sage); }',
     '.chip.success { background: rgba(77,139,106,0.15); color: var(--color-success); }'),
    ('.mission-node.amber circle { stroke: var(--color-warning); }',
     '.mission-node.warning circle { stroke: var(--color-warning); }'),
    ('.mission-node.sage circle { stroke: var(--color-sage); }',
     '.mission-node.success circle { stroke: var(--color-success); }'),
    ('.btn-sage { background: var(--color-sage); color: #0e0d14; }',
     '.btn-success { background: var(--color-success); color: #0a0d13; }'),
]

for old, new in replacements:
    assert old in content, f"pattern not found verbatim, stop and check stitch.css manually:\n{old}"
    content = content.replace(old, new)

with open('assets/stitch.css', 'w') as f:
    f.write(content)
PYEOF
```

Note: `rgba(77,139,106,...)` (the old sage color's dark RGB) is left
as-is in the `.notice.ok`/`dot-pulse` keyframe rules — `--color-success`'s
dark value (`#4d8b6a` = `rgb(77,139,106)`) is numerically *identical* to
the old sage value, so these hardcoded rgba tints remain correct without
any change. Confirmed, not assumed: `#4d8b6a` in hex is exactly
`rgb(77, 139, 106)`.

- [ ] **Step 2: Rename Rust identifiers in `main.rs`**

```bash
cd /home/julian/Projects/Rust/aivyx-pa/crates/aivyx-web
python3 << 'PYEOF'
with open('src/main.rs') as f:
    content = f.read()

replacements = [
    # tier_chip_class -- lines ~5596-5602
    ('fn tier_chip_class(t: TrustTier) -> &\'static str {\n    match t {\n        TrustTier::Kernel => "chip error",\n        TrustTier::Trusted => "chip sage",\n        TrustTier::SemiTrusted => "chip amber",\n        TrustTier::Untrusted => "chip muted",\n    }\n}',
     'fn tier_chip_class(t: TrustTier) -> &\'static str {\n    match t {\n        TrustTier::Kernel => "chip error",\n        TrustTier::Trusted => "chip success",\n        TrustTier::SemiTrusted => "chip warning",\n        TrustTier::Untrusted => "chip muted",\n    }\n}'),
    # mission-node state class -- lines ~3888-3890
    ('TeamStepState::Running | TeamStepState::Awaiting => "amber",',
     'TeamStepState::Running | TeamStepState::Awaiting => "warning",'),
    ('TeamStepState::Done => "sage",',
     'TeamStepState::Done => "success",'),
    # memory-conflict flag -- line ~4216
    ('span { class: "chip amber mem-topic-flag", title: "contradictory entries", "⚠" }',
     'span { class: "chip warning mem-topic-flag", title: "contradictory entries", "⚠" }'),
    # seed-card buttons -- lines ~4347, 4352
    ('class: "btn btn-sage btn-xs",',
     'class: "btn btn-success btn-xs",'),
]

count = 0
for old, new in replacements:
    n = content.count(old)
    assert n > 0, f"pattern not found verbatim, stop and check main.rs manually:\n{old}"
    content = content.replace(old, new)
    count += n

with open('src/main.rs', 'w') as f:
    f.write(content)

print(f"applied {count} replacements (expect 6: 1 tier_chip_class block + 2 mission-node arms + 1 memory-conflict flag + 2 identical btn-sage occurrences, since Python's str.replace swaps every match of a pattern, not just the first)")
PYEOF
```

- [ ] **Step 3: Verify no retired names remain in either file**

```bash
cd /home/julian/Projects/Rust/aivyx-pa/crates/aivyx-web
grep -n "var(--color-sage)\|\.chip\.amber\|\.chip\.sage\b\|\.btn-sage\|\.mission-node\.amber\|\.mission-node\.sage" assets/stitch.css
grep -n '"chip amber"\|"chip sage"\|=> "amber"\|=> "sage"\|btn-sage' src/main.rs
```

Expected: no output from either grep. Note the first pattern is
deliberately `var(--color-sage)` — matching *consumption* — not the
bare substring `color-sage`, which would also match Task 1's own alias
*definition* line (`--color-sage: var(--color-success);`) and produce a
false positive on a line that's supposed to stay exactly as Task 1 wrote
it. Similarly `\.chip\.sage\b` (not bare `chip sage`) avoids matching
inside `--color-sage` itself.

- [ ] **Step 4: Confirm the workspace compiles**

```bash
cd /home/julian/Projects/Rust/aivyx-pa
cargo build -p aivyx-web --target wasm32-unknown-unknown
```

- [ ] **Step 5: Commit**

```bash
git add crates/aivyx-web/assets/stitch.css crates/aivyx-web/src/main.rs
git commit -m "feat: migrate every retired-name CSS class + Rust identifier

var(--color-sage)'s 11 real usages -> var(--color-success) (values
unchanged where numerically identical, e.g. the old sage rgb(77,139,106)
already equals new success's dark value). .chip.sage/.chip.amber and
.mission-node.sage/.mission-node.amber renamed to success/warning,
correcting a real pre-existing mismatch where .mission-node.amber already
mapped to --color-warning in CSS despite being named 'amber' in Rust.
.btn-sage -> .btn-success. Corresponding Rust match arms and class-string
literals in main.rs updated to match -- one atomic commit since a partial
rename would leave call-sites pointing at now-undefined class names."
```

---

### Task 5: Font asset swap

**Files:**
- Create: `crates/aivyx-web/assets/fonts/fraunces-var.woff2`, `ibm-plex-sans-var.woff2`, `ibm-plex-mono-var.woff2`
- Delete: `crates/aivyx-web/assets/fonts/space-grotesk-var.woff2`, `inter-var.woff2`, `jetbrains-mono-var.woff2`
- Modify: `crates/aivyx-web/src/main.rs:63-65` (asset consts), `:818-820` (`@font-face` injection)

**Interfaces:** Consumes nothing from earlier tasks. Produces the 3
`asset!()` consts (`FONT_DISPLAY`/`FONT_BODY`/`FONT_MONO`) whose names
stay the same — only what they point to changes — so no other call site
needs updating.

- [ ] **Step 1: Download the 3 real font files**

```bash
mkdir -p /tmp/wick-studio-fonts && cd /tmp/wick-studio-fonts

# Fraunces (same font aivyx-brand's Task 7 used)
curl -sL -o fraunces-var.woff2 \
  "https://fonts.gstatic.com/s/fraunces/v34/6NUM8FiPJgv3EXazX6MwXbeZ_lQV.woff2" \
  || echo "gstatic URL may have rotated -- fetch fresh from fonts.google.com/specimen/Fraunces if this 404s"

# IBM Plex Sans
curl -sL -o ibm-plex-sans-var.woff2 \
  "https://fonts.gstatic.com/s/ibmplexsans/v22/zYX9KVElMYYaJe8bpLHnCwDKtdbUFA9j0lFHzquQ6zY.woff2" \
  || echo "gstatic URL may have rotated -- fetch fresh from fonts.google.com/specimen/IBM+Plex+Sans if this 404s"

# IBM Plex Mono
curl -sL -o ibm-plex-mono-var.woff2 \
  "https://fonts.gstatic.com/s/ibmplexmono/v19/-F63fjptAgt5VM-kVkqdyU8n1i8q131nj-otFQ.woff2" \
  || echo "gstatic URL may have rotated -- fetch fresh from fonts.google.com/specimen/IBM+Plex+Mono if this 404s"

file *.woff2
```

Expected: `file` reports each as a real `Web Open Font Format` (or
similar binary font format) file, not an HTML error page. If any URL
404s (Google Fonts CDN paths can rotate — this exact scenario already
happened once during `aivyx-brand`'s own font work), visit the linked
specimen page, use "Download family," and extract the correct variable
`.woff2` (or convert from `.ttf` if only static/TTF is offered — check
what's actually in the downloaded zip before assuming format).

- [ ] **Step 2: Move the files into place, remove the old ones**

```bash
cd /home/julian/Projects/Rust/aivyx-pa/crates/aivyx-web
mv /tmp/wick-studio-fonts/fraunces-var.woff2 assets/fonts/
mv /tmp/wick-studio-fonts/ibm-plex-sans-var.woff2 assets/fonts/
mv /tmp/wick-studio-fonts/ibm-plex-mono-var.woff2 assets/fonts/
git rm assets/fonts/space-grotesk-var.woff2 assets/fonts/inter-var.woff2 assets/fonts/jetbrains-mono-var.woff2
```

- [ ] **Step 3: Update the `asset!()` consts**

```bash
sed -i \
  -e 's|const FONT_DISPLAY: Asset = asset!("/assets/fonts/space-grotesk-var.woff2");|const FONT_DISPLAY: Asset = asset!("/assets/fonts/fraunces-var.woff2");|' \
  -e 's|const FONT_BODY: Asset = asset!("/assets/fonts/inter-var.woff2");|const FONT_BODY: Asset = asset!("/assets/fonts/ibm-plex-sans-var.woff2");|' \
  -e 's|const FONT_MONO: Asset = asset!("/assets/fonts/jetbrains-mono-var.woff2");|const FONT_MONO: Asset = asset!("/assets/fonts/ibm-plex-mono-var.woff2");|' \
  src/main.rs
```

- [ ] **Step 4: Update the `@font-face` injection**

```bash
python3 << 'PYEOF'
with open('src/main.rs') as f:
    content = f.read()

old = '''"@font-face{{font-family:'Space Grotesk';src:url('{FONT_DISPLAY}') format('woff2');font-weight:300 700;font-display:swap;}}\\
         @font-face{{font-family:'Inter';src:url('{FONT_BODY}') format('woff2');font-weight:100 900;font-display:swap;}}\\
         @font-face{{font-family:'JetBrains Mono';src:url('{FONT_MONO}') format('woff2');font-weight:100 800;font-display:swap;}}"'''

new = '''"@font-face{{font-family:'Fraunces';src:url('{FONT_DISPLAY}') format('woff2');font-weight:100 900;font-display:swap;}}\\
         @font-face{{font-family:'IBM Plex Sans';src:url('{FONT_BODY}') format('woff2');font-weight:100 700;font-display:swap;}}\\
         @font-face{{font-family:'IBM Plex Mono';src:url('{FONT_MONO}') format('woff2');font-weight:100 700;font-display:swap;}}"'''

assert old in content, "old @font-face injection string not found verbatim -- stop and check main.rs manually (whitespace/line-continuation in this raw string is exact-match sensitive)"
content = content.replace(old, new)

with open('src/main.rs', 'w') as f:
    f.write(content)
PYEOF
```

If the assertion fails because of exact whitespace differences in the
raw string (line-continuation backslashes are sensitive to this), open
`src/main.rs` around line 814-820, read the real current text, and apply
the same 3 font-family/weight-range substitutions by hand instead of
via the scripted replace.

- [ ] **Step 5: Verify no old font names remain**

```bash
grep -n "Space Grotesk\|JetBrains Mono\|'Inter'" src/main.rs assets/stitch.css
ls assets/fonts/
```

Expected: no grep output; `ls` shows exactly the 3 new `.woff2` files,
no old ones.

- [ ] **Step 6: Confirm the workspace compiles**

```bash
cd /home/julian/Projects/Rust/aivyx-pa
cargo build -p aivyx-web --target wasm32-unknown-unknown
```

- [ ] **Step 7: Commit**

```bash
git add crates/aivyx-web/assets/fonts/ crates/aivyx-web/src/main.rs
git commit -m "feat: swap Studio UI fonts to Fraunces + IBM Plex Sans/Mono

Real downloaded variable font files (no CDN, matching aivyx-web's
zero-runtime-network-dependency constraint), same technique
aivyx-brand's own wordmark rebuild used. Renamed the files themselves
(not kept under the old, now-misleading names) -- required updating the
3 asset!() consts and the @font-face injection's family names/weight
ranges to match each font's real supported range."
```

---

### Task 6: Icon/logo asset swap

**Files:**
- Modify: `crates/aivyx-web/assets/icons/candle-flame.svg`
- Modify: `crates/aivyx-web/assets/logos/aivyx-favicon.svg`, `aivyx-logomark.svg`, `aivyx-wordmark.svg`

**Interfaces:** Consumes nothing from earlier tasks (these are
standalone SVG files). Produces nothing consumed elsewhere in this plan.

Confirmed during planning (verified via `diff`, not assumed): of
`aivyx-web`'s 23 icons, 22 are byte-identical copies of real
`aivyx-brand/icons/{nav,feature}/*.svg` files that already use
`stroke="currentColor"` with zero baked hex — nothing about this
rebrand touches them, and they need **no edits in this task**. Only
`candle-flame.svg` (still the old flame-only geometry, no dial ring) and
the 3 `logos/*.svg` files (still old candle-body geometry/hex) differ
from `aivyx-brand`'s current finished versions.

- [ ] **Step 1: Copy the 4 real files from `aivyx-brand`**

```bash
cd /home/julian/Projects/Rust/aivyx-pa/crates/aivyx-web
cp /home/julian/Projects/Rust/aivyx-brand/icons/feature/candle-flame.svg assets/icons/candle-flame.svg
cp /home/julian/Projects/Rust/aivyx-brand/logos/aivyx-favicon.svg assets/logos/aivyx-favicon.svg
cp /home/julian/Projects/Rust/aivyx-brand/logos/aivyx-logomark.svg assets/logos/aivyx-logomark.svg
cp /home/julian/Projects/Rust/aivyx-brand/logos/aivyx-wordmark.svg assets/logos/aivyx-wordmark.svg
```

- [ ] **Step 2: Verify all 4 are now byte-identical to their `aivyx-brand` source, and every other icon is genuinely untouched**

```bash
diff assets/icons/candle-flame.svg /home/julian/Projects/Rust/aivyx-brand/icons/feature/candle-flame.svg && echo "candle-flame: identical"
diff assets/logos/aivyx-favicon.svg /home/julian/Projects/Rust/aivyx-brand/logos/aivyx-favicon.svg && echo "favicon: identical"
diff assets/logos/aivyx-logomark.svg /home/julian/Projects/Rust/aivyx-brand/logos/aivyx-logomark.svg && echo "logomark: identical"
diff assets/logos/aivyx-wordmark.svg /home/julian/Projects/Rust/aivyx-brand/logos/aivyx-wordmark.svg && echo "wordmark: identical"

git status --short
```

Expected: 4 "identical" lines; `git status --short` shows exactly these
4 files as modified, nothing else under `assets/icons/`.

- [ ] **Step 3: Confirm every SVG in the crate is still valid XML**

```bash
for f in $(find assets/icons assets/logos -name "*.svg"); do
  python3 -c "import xml.etree.ElementTree as ET; ET.parse('$f')" && echo "$f: valid" || echo "$f: FAILED"
done
```

Expected: every file reports `valid`, none `FAILED`.

- [ ] **Step 4: Commit**

```bash
git add crates/aivyx-web/assets/icons/candle-flame.svg crates/aivyx-web/assets/logos/
git commit -m "feat: swap the 4 real mark files to their finished aivyx-brand versions

Confirmed via diff during planning: 22 of 23 icons in this crate are
already byte-identical currentColor copies of real aivyx-brand icons
that never held palette information -- untouched here, correctly. Only
candle-flame.svg (needed the dial-ring geometry added in aivyx-brand's
own rebrand) and the 3 logos/*.svg files (still held old candle-body
geometry/hex) needed a real copy."
```

---

### Task 7: Remaining indicator restyles (audit chain, notification badge, daemon connection dot)

**Files:**
- Modify: `crates/aivyx-web/assets/stitch.css` (`.dot` rules, a new `.dial-glyph` utility)
- Modify: `crates/aivyx-web/src/main.rs:3405-3406` (audit chain status), `:1561-1564` (notification badge), the `statusbar` footer's `.dot` markup

**Interfaces:** Consumes Task 1's `--color-primary`/`--color-tertiary`
tokens and Task 4's already-migrated `--color-success` naming (trust-tier
chips, handled entirely in Task 4, are not part of this task's scope).

First, ground the real current `.dot` CSS and the `statusbar` footer's
exact markup (the design spec deferred this) — read
`crates/aivyx-web/assets/stitch.css:269-271` and
`crates/aivyx-web/src/main.rs`'s `statusbar` component (search for
`class: "statusbar label-tech"`) before writing this task's replacements,
since exact current text is required for the verbatim-match assertions
below; if the real text differs from what's shown here (e.g. this plan
was written against a slightly earlier revision), stop and adapt the
`old`/`new` strings to match reality rather than forcing a mismatch.

- [ ] **Step 1: Add a brass-ring treatment to the notification badge**

```bash
cd /home/julian/Projects/Rust/aivyx-pa/crates/aivyx-web
python3 << 'PYEOF'
with open('src/main.rs') as f:
    content = f.read()

old = '''                    span {
                        class: "badge",
                        style: "position:absolute; top:2px; right:2px; min-width:14px; height:14px; border-radius:7px; background:var(--danger, #b91c1c); color:#fff; font-size:9px; line-height:14px; text-align:center; padding:0 3px;",
                        if unseen > 9 { "9+" } else { "{unseen}" }
                    }'''

new = '''                    span {
                        class: "badge",
                        style: "position:absolute; top:2px; right:2px; min-width:14px; height:14px; border-radius:7px; background:var(--danger, #b91c1c); border: 1px solid var(--color-primary); color:#fff; font-size:9px; line-height:14px; text-align:center; padding:0 3px;",
                        if unseen > 9 { "9+" } else { "{unseen}" }
                    }'''

assert old in content, "notification badge markup not found verbatim -- stop and check main.rs manually"
content = content.replace(old, new)

with open('src/main.rs', 'w') as f:
    f.write(content)
PYEOF
```

The fill stays `var(--danger, #b91c1c)` (out of scope, per Global
Constraints — this is an unread-item *count*, not a status reading) —
only a brass border is added around it.

- [ ] **Step 2: Add a dial-ring treatment to the audit chain status**

```bash
python3 << 'PYEOF'
with open('src/main.rs') as f:
    content = f.read()

old = '''                        match chain_ok {
                            Some(true) => rsx! { span { style: "color: var(--ok, #16a34a); margin-left:8px;", "✓ chain intact" } },
                            Some(false) => rsx! { span { style: "color: var(--danger, #b91c1c); margin-left:8px;", "✗ chain verification failed" } },
                            None => rsx! { span {} },
                        }'''

new = '''                        match chain_ok {
                            Some(true) => rsx! { span { style: "color: var(--ok, #16a34a); margin-left:8px;", span { class: "dial-glyph", style: "border-color: var(--ok, #16a34a);" } "chain intact" } },
                            Some(false) => rsx! { span { style: "color: var(--danger, #b91c1c); margin-left:8px;", span { class: "dial-glyph", style: "border-color: var(--danger, #b91c1c);" } "chain verification failed" } },
                            None => rsx! { span {} },
                        }'''

assert old in content, "audit chain status markup not found verbatim -- stop and check main.rs manually"
content = content.replace(old, new)

with open('src/main.rs', 'w') as f:
    f.write(content)
PYEOF
```

Note: this drops the literal `✓`/`✗` Unicode glyphs in favor of the new
`.dial-glyph` ring (a small bordered circle, styled below) — the
checkmark/cross meaning is now carried by the ring's border color plus
the text itself ("chain intact"/"chain verification failed" is
self-explanatory without a leading symbol).

- [ ] **Step 3: Add the shared `.dial-glyph` CSS utility**

```bash
python3 << 'PYEOF'
with open('assets/stitch.css') as f:
    content = f.read()

# Insert right after the .chip rules Task 4 already touched, before
# .stat-card, so related small-indicator utilities stay grouped.
anchor = '.stat-card {'
assert anchor in content, "anchor point '.stat-card {' not found -- stop and check stitch.css manually"

new_rule = '''.dial-glyph {
  display: inline-block; width: 8px; height: 8px; border-radius: 50%;
  border: 1.5px solid currentColor; margin-right: 6px; vertical-align: middle;
}

'''

content = content.replace(anchor, new_rule + anchor, 1)

with open('assets/stitch.css', 'w') as f:
    f.write(content)
PYEOF
```

- [ ] **Step 4: Add a brass/slate ring to the daemon connection status dot**

First confirm the real current CSS (per this task's own grounding note
above) — the plan's assumed current state:

```css
.statusbar .seg .dot { width: 6px; height: 6px; border-radius: 50%; background: var(--color-text-disabled); }
.statusbar .seg.live .dot { background: var(--color-success); }
```

(the second line already reads `--color-success` here, not
`--color-sage`, because Task 4 already migrated it). Add a matching
ring:

```bash
python3 << 'PYEOF'
with open('assets/stitch.css') as f:
    content = f.read()

old = '''.statusbar .seg .dot { width: 6px; height: 6px; border-radius: 50%; background: var(--color-text-disabled); }
.statusbar .seg.live .dot { background: var(--color-success); }'''

new = '''.statusbar .seg .dot { width: 6px; height: 6px; border-radius: 50%; background: var(--color-text-disabled); border: 1px solid var(--color-tertiary); }
.statusbar .seg.live .dot { background: var(--color-success); border-color: var(--color-primary); }'''

assert old in content, "statusbar dot rules not found verbatim -- stop, re-read the file's real current state (Task 4 should have already migrated the .live rule to --color-success) and adapt this replacement to match reality"
content = content.replace(old, new)

with open('assets/stitch.css', 'w') as f:
    f.write(content)
PYEOF
```

- [ ] **Step 5: Verify**

```bash
grep -n "dial-glyph" assets/stitch.css src/main.rs
grep -n "border: 1px solid var(--color-primary)" src/main.rs
grep -n "border-color: var(--color-primary)" assets/stitch.css
```

Expected: `dial-glyph` appears once as a CSS rule definition and twice
as Rust `class:` usages; the two border greps each return one match.

- [ ] **Step 6: Confirm the workspace compiles**

```bash
cd /home/julian/Projects/Rust/aivyx-pa
cargo build -p aivyx-web --target wasm32-unknown-unknown
```

- [ ] **Step 7: Commit**

```bash
git add crates/aivyx-web/assets/stitch.css crates/aivyx-web/src/main.rs
git commit -m "feat: dial-ring treatment for audit chain, notification badge, daemon dot

Trust-tier chips already got their dial-motif treatment (a leading
success/warning-colored indicator via the class rename) in Task 4 --
this covers the other 3 named indicators from the design spec. Audit
chain status trades its Unicode checkmark/cross for a small bordered
ring matching the same success/error color, since the ring now carries
that meaning. Notification badge and daemon connection dot both gain a
brass (active) or slate (inactive) ring around their existing solid
fill -- same restrained 'recolor + ring, don't redesign' approach used
throughout this whole rebrand."
```

---

### Task 8: Final build verification + repo-wide sweep

**Files:** none created/modified — pure verification.

**Interfaces:** Consumes the complete branch diff. Produces the
confidence needed to hand this branch to `finishing-a-development-branch`.

- [ ] **Step 1: Real wasm bundle build**

```bash
cd /home/julian/Projects/Rust/aivyx-pa
just build-web
```

Expected: completes with no errors. This is the load-bearing check for
this whole plan — unlike `aivyx-brand`'s static-asset rebrand, a broken
`asset!()` path or a malformed CSS/Rust edit here fails a real build,
not just a grep.

- [ ] **Step 2: Confirm `dist/` actually changed**

```bash
git status --short crates/aivyx-web/dist/
git diff --stat crates/aivyx-web/dist/ | tail -5
```

Expected: real changes reported under `dist/` — specifically a new
`dist/assets/stitch-<newhash>.css` (proving the CSS was genuinely
rebundled with a fresh content hash, not silently cached) and updated
`dist/wasm/*` artifacts. If `git status` shows nothing changed under
`dist/`, the build didn't actually pick up the source changes — stop and
investigate rather than proceeding.

- [ ] **Step 3: Repo-wide sweep, scoped to `crates/aivyx-web/`, for old Neon-Cartographer-era values/terminology**

Hex-form sweep:

```bash
cd /home/julian/Projects/Rust/aivyx-pa
grep -rn -iE '#(ffb77d|ffc999|d9802b|ffdcc3|904d00|ccc1e6|4a4261|e8ddff|332c49|d1c4b7|9f9488|372f26|0e0d14|131319|1c1b22|201f26|2a2930|35343b|ebe6de|f5f0e8|faf6f0|f0ebe3|C4553E|7A65A6|8B5E3C|3C8B80)\b' \
  --include='*.rs' --include='*.css' --include='*.svg' crates/aivyx-web/src crates/aivyx-web/assets \
  | grep -v 'dist/'
```

(`4D8B6A`, the old sage hex, is deliberately excluded from this list —
it's numerically identical to the new `--color-success` dark value, so
including it would false-positive on every legitimate token definition
Task 1 wrote. The `var(--color-sage)`-shaped *consumption* is what
Task 4's own Step 3 already checks for, which is the actually-meaningful
signal here.)

Decimal-rgba sweep:

```bash
grep -rn -E '(82,\s*74,\s*105|255,\s*183,\s*125|139,\s*111,\s*191|204,\s*193,\s*230|212,\s*138,\s*60)' \
  --include='*.rs' --include='*.css' crates/aivyx-web/src crates/aivyx-web/assets \
  | grep -v 'dist/'
```

Stale-terminology sweep:

```bash
grep -rn -i "neon cartographer\|candle motif\|glass-panel\|glass-header\|backdrop-blur\|frosted\|cyber purple\|space grotesk\|jetbrains mono" \
  --include='*.rs' --include='*.css' crates/aivyx-web/src crates/aivyx-web/assets \
  | grep -v 'dist/'
```

Expected: no output from any of the three (the 12 `--danger`/`--ok`/
`--warn` hex fallbacks are explicitly out of scope per Global
Constraints and are not part of this sweep's target list; `--font-mono:
'JetBrains Mono'` no longer exists after Task 5, so a hit there would be
real). If something prints, it's a real miss from Tasks 1-6 — fix it in
the relevant task's own file before proceeding, don't patch ad hoc
outside any task's commit.

- [ ] **Step 4: Full workspace test suite**

```bash
cargo test --workspace
```

Expected: passes clean — this plan didn't touch any test-covered logic
(pure CSS/asset/cosmetic-Rust-string changes), so this confirms nothing
regressed elsewhere in the workspace.

- [ ] **Step 5: Report**

Summarize: real build success confirmed, `dist/` hash-changed and
diffed, all 3 sweeps clean, full workspace test suite result. No commit
for this task — pure verification, nothing to add to git beyond what
Tasks 1-6 already committed.
