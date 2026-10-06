# Per-area autonomy — design

Date: 2026-10-06. Status: approved in conversation. First of two specs; the
second is supervised approval batching.

## Goal

Make `[[autonomy.override]]` real: an operator can say "autonomous for
coding, manual for email" and every tool call is gated by the level of the
**area** it belongs to. Today overrides are parsed and shown, but only the
`schedules` domain changes behaviour.

## What the levels mean per call (after the 2026-10-06 safety fixes)

| Level | Any change | Delete / overwrite | Integration writes |
|---|---|---|---|
| `manual` | asks | asks | asks |
| `assisted`, `supervised`, `autonomous` | runs | asks | asks |
| `unleashed` | runs | runs | asks |

Per-area autonomy applies the first two columns per area. The third
(integration writes: `email.send`, `drive.write`, … ) asks at every level
unless `[access] confirm_destructive = false` is set explicitly — unchanged
here. Unattended runs (routines, webhooks, file watches, the loop, team
missions) refuse irreversible steps at every level — unchanged. Loop arming
stays global (the loop isn't one area); the `schedules` area keeps
controlling whether agent-created routines need approval.

## Areas

- An **area** is the first word of a capability scope base: `fs.delete` →
  `fs`, `email.send` → `email`, `mcp.call` → `mcp`.
- The valid areas are the distinct first words of `aivyx_capability`'s known
  bases, exposed as `aivyx_capability::areas()`. Nothing is hand-maintained,
  so a new capability base brings its area with it.
- `[[autonomy.override]] domain` must name a valid area. `schedules` is
  accepted as an alias for `schedule` (it's the spelling the routine-growth
  code and existing configs use) and normalised on load.
- An unknown area is a config **load error** that lists the valid areas
  (typo protection). An unknown level already is.

## Resolution

- `AivyxConfig::posture_for_area(area: &str) -> AutonomyPosture` — the
  existing `resolve_posture` (an override for that area wins, else the
  global level), after alias normalisation.
- `AivyxConfig::posture_for_base(base: &str)` — `posture_for_area` of the
  base's first word.

## The two decision points

### "Ask before any change" (the `manual` behaviour) — in the turn loop

`aivyx-core`'s `ConcreteAgent` today holds `confirm_all: bool`. It becomes a
`ConfirmAllAreas` value, defined in `aivyx-core` (which does not depend on
`aivyx-config`):

```rust
pub struct ConfirmAllAreas {
    /// Whether an area with no entry confirms every change (the global level).
    pub default: bool,
    /// Areas whose level differs from the global level: area → confirm-all?
    pub areas: BTreeMap<String, bool>,
}
impl ConfirmAllAreas {
    pub fn for_base(&self, base: &str) -> bool; // area = first word of base
}
```

`with_confirm_all(bool)` stays as a convenience (`default = on`, no areas).
The daemon builds the table from the config (`default` = global gate is
`ConfirmAll`; one entry per override). In the existing ask decision
(`agent.rs`, "would change something, and the autonomy level is manual"),
`self.confirm_all` becomes `self.confirm_all.for_base(needed.base())`, so
the reason names the area: "…the autonomy level for `email` is manual…".

Every agent construction path that sets `with_confirm_all` today gets the
table instead (the same paths `with_confirm_integration_writes` threads
through).

### Delete / overwrite — at tool construction

The fs, data and git tools take their own `confirm_destructive` flag when
built. Each now gets it from its own area:

| Tool | Area |
|---|---|
| `fs.write`, `fs.delete` | `fs` |
| `data.*.write` (spreadsheet/PDF writers, `fs.write` scope) | `fs` |
| `git.commit` | `git` |

via `aivyx_config::confirm_destructive_for(&config.confirm_destructive,
&config.posture_for_area(area))`. An explicit `[access]
confirm_destructive` still wins everywhere. So `fs = "unleashed"` stops file
deletes asking while git commits still ask; `git = "manual"` makes commits
(and, through the table above, every git change) ask.

## Visibility

- **Start-up warning** for each override looser than the global level
  (level order manual < assisted < supervised < autonomous < unleashed):
  `autonomy: area \`shell\` is unleashed, looser than the global level
  (assisted)`.
- **`aivyx-pa autonomy show`** lists each override with what it changes
  (asks before any change / deletes ask / deletes run), marks looser ones,
  and lists the valid area names.
- **Studio** Settings → Autonomy lists the overrides read-only ("edit in the
  config file"), from a new `autonomy_overrides` field on the settings
  snapshot.
- **Docs**: guide 08 and 15, the CLI reference's autonomy section and the
  generated config reference (`autonomy.override.domain` meaning) say what
  overrides now do; the "only `schedules` is applied" caveats go.

## Errors

- Unknown area or level: config load error, as above.
- Two overrides for the same area: load error (today the first silently
  wins).

## Testing

- `aivyx-capability`: `areas()` contains `fs`, `email`, `schedule`; every
  known base's first word is in it.
- `aivyx-config`: alias normalisation; unknown area rejected with the list;
  duplicate area rejected; `posture_for_base` for overridden and
  non-overridden areas; the looser-than-global check.
- `aivyx-core`: `ConfirmAllAreas::for_base`; the turn loop asks for a call in
  a `manual` area and not in an `autonomous` one, under a global `manual`
  and a global `assisted`.
- `aivyx-cli`: the table and each tool's flag are built from the right area
  (pure helper functions, unit-tested); `autonomy show` rendering.
- Existing tests keep passing: no `[autonomy]` section ⇒ identical
  behaviour.

## Out of scope

Supervised batching (spec 2), per-channel autonomy, the expert escape hatch
(AUTONOMY.md §6), and any change to unattended behaviour.
