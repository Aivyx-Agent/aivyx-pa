# Aivyx-Vision Image/3D Adoption (aivyx-pa) Design

## Context

`aivyx-pa` already adopted Aivyx-Vision's Milestone 1 (`vision.generate_svg`,
shipped 2026-09-18 — see `docs/superpowers/plans/2026-09-18-vision-svg-adoption.md`):
a new tool-process crate, `crates/aivyx-vision`, running as a separate OS
process from the daemon, registered via `[[tool_process]]` in `aivyx-pa.toml`,
gated by a new `vision.generate` capability base at `CEILING_SEMITRUSTED`.

Aivyx-Vision's own repo (`aivyx-vision`, a separate public repo) just shipped
Milestone 2 Pass A (2026-09-18, commit `caed4c0`): `aivyx-vision-core` (the
`GenerationProvider` trait + `ImageRequest`/`ThreeDRequest`/`GeneratedAsset`/
`VisionError` types) and `aivyx-vision-mold` (an HTTP-client backend
implementing that trait against an operator-run `mold serve` instance for
image generation, coordinated with local LLM inference sharing the same GPU
via `aivyx-broker`'s GPU lock). `generate_3d` on `aivyx-vision-mold`'s
`MoldProvider` always returns `VisionError::Unsupported` — mold's async 3D
job-lifecycle API ("Pass B") is separate, not-yet-built work.

This design covers `aivyx-pa`'s adoption of that Milestone 2 Pass A work:
adding `vision.generate_image` and `vision.generate_3d` tools to the
*existing* `crates/aivyx-vision` tool process, alongside the already-shipped
`vision.generate_svg`.

See `aivyx-ecosystem/docs/superpowers/specs/2026-09-18-aivyx-vision-v1-design.md`
for the full cross-repo Aivyx-Vision design (§3 covers the `aivyx-pa` adoption
shape in general terms; this document makes the concrete decisions that
spec left open for plan time).

## Grounding

Read directly from the current codebase (not assumed):

- `crates/aivyx-vision/src/tools.rs` — `GenerateSvgTool` depends on
  `Arc<dyn aivyx_vision_svg::TextCompleter>` (a trait object), not a
  concrete LLM client — the template this design's two new tools copy for
  their own backend dependency.
- `crates/aivyx-vision/src/config.rs` — `VisionConfig` loaded once at
  startup from `~/.aivyx-pa/tool-processes/vision/config.toml`
  (`default_config_path()` resolves this via `$HOME`, erroring
  `ConfigFileError::NoHome` if unset — same pattern this design reuses for
  a new default output directory).
- `crates/aivyx-vision/src/main.rs` — builds the LLM provider, wraps it,
  registers tools into a `Vec<Arc<dyn Tool>>`, hands off to
  `aivyx_tool::run_multi_tool_subprocess`.
- `docs/TOOLS.md`'s existing "Aivyx-Vision" section already anticipated
  this: *"later milestones' `vision.generate_image` / `vision.generate_3d`
  tools will share the same base"* — confirming no new capability base is
  needed, only new `KNOWN_BASES`/`CEILING_SEMITRUSTED` entries would be
  needed if this were a *new* base, which it isn't.
- `crates/aivyx-core/src/tools/fs.rs` — `fs.read`/`fs.write`'s sandbox-path
  validation idiom (canonicalize the candidate path, canonicalize the
  sandbox root, reject unless the former starts with the latter) — the
  pattern this design's `reference_image` validation copies, hand-rolled
  locally since `aivyx-vision` (a separate tool-process crate) doesn't
  depend on `aivyx-core::tools::fs`.
- `crates/aivyx-cost/src/lib.rs` — Chapter K's budget/rate-limit machinery
  (`BudgetEnforcer`, `RateLimiter`) is priced/counted around **LLM token
  spend**, not local GPU compute time. It doesn't naturally fit this
  domain, and the ecosystem spec itself flags budget integration as an
  open question, not a requirement. Confirmed with the project owner: skip
  it for this pass.
- Root `Cargo.toml`: `aivyx-vision-svg` is a pinned git dependency
  (`rev = "a80be4709b41382ef62c20545bb706e18a1d5ee3"`, the tip of
  `aivyx-vision`'s `main` immediately before Milestone 2 Pass A merged).

## Decisions

**1. New tools live in the existing `crates/aivyx-vision` tool process, not
a new crate.** One tool process, one config file, one capability base
already shared across all three generation domains (SVG done, image/3D
this pass) — splitting into a second tool process would mean a second
`[[tool_process]]` entry, a second config file, and no benefit, since all
three domains already share `vision.generate`.

**2. Both new tools depend on `Arc<dyn aivyx_vision_core::GenerationProvider>`,
not a concrete backend type.** Exactly mirrors `GenerateSvgTool`'s existing
`Arc<dyn TextCompleter>` dependency. `main.rs` constructs one concrete
`MoldProvider` (from `aivyx-vision-mold`) and hands the same `Arc` to both
`GenerateImageTool` and `GenerateThreeDTool`. This also means: if Pass B ever
replaces `MoldProvider`'s `generate_3d` stub with a real implementation (or a
future `aivyx-vision-comfyui` backend gets swapped in), `GenerateThreeDTool`
starts working with zero changes to this crate.

**3. `vision.generate_3d` ships now, not deferred to Pass B.** Every call
fails today with `VisionError::Unsupported`'s message ("3D generation via
mold (Pass B is not yet implemented)"), surfaced as a clear
`ToolOutcome::Failed` — not silently missing from the tool catalog. This is
a deliberate choice (confirmed with the project owner) to make the tool
discoverable now rather than adding it later as a second small plan.

**4. No new capability base.** Both tools use
`Scope::parse("vision.generate")`, already at `CEILING_SEMITRUSTED` since
Milestone 1. No `aivyx-capability` changes, no `KNOWN_BASES` count-assertion
bump.

**5. Config: a new optional `[mold]` section in the existing
`config.toml`.**

```toml
# ~/.aivyx-pa/tool-processes/vision/config.toml

# --- existing, vision.generate_svg's own LLM provider (unchanged) ---
provider = "ollama"
model = "qwen3:8b"

# --- new, optional ---
[mold]
broker_url = "http://127.0.0.1:8899"    # aivyx-broker
mold_url = "http://127.0.0.1:7680"      # mold serve
# api_key = "..."                        # optional; only if mold serve sets MOLD_API_KEY
# output_dir = "..."                     # optional; defaults to ~/.local/share/aivyx-pa/vision/
```

`VisionConfig` (in `config.rs`) gains `pub mold: Option<MoldSettings>`. If
`[mold]` is absent, the process still starts (backward compatible with every
existing install) and `main.rs` simply doesn't register
`vision.generate_image`/`vision.generate_3d` — `vision.generate_svg` keeps
working exactly as it does today. `output_dir`'s computed default follows
`default_config_path()`'s own `$HOME`-based pattern (`NoHome` error if
unset), landing at `~/.local/share/aivyx-pa/vision/` — matching the
ecosystem spec §6's storage convention exactly
(`~/.local/share/aivyx-pa/vision/<uuid>.<ext>`, and `aivyx-vision-mold`'s own
`MoldProvider` already generates that `<uuid>.<ext>` filename itself, so this
crate only needs to point `output_dir` at the right place).

**6. `vision.generate_image`'s input schema exposes every `ImageRequest`
field**, not a minimal `{prompt}`-only shape: `prompt` (required),
`width`/`height`/`seed`/`style_hint`/`reference_image` (all optional) — the
underlying request type already supports all of them, so there's no real
cost to exposing them now rather than adding them piecemeal later. Output
mirrors `GeneratedAsset`, converted to JSON:
`{"path": "<absolute path>", "backend": "mold", "seed_used": <u64 or null>}`
— matching `vision.generate_svg`'s own precedent of returning the caller's
next-step data directly (there, the SVG string; here, the file path) rather
than an opaque handle. `vision.generate_3d`'s input schema is minimal for
now — just `{"prompt": "..."}` (required) — since `ThreeDRequest`'s own
shape is still provisional per the ecosystem spec (full definition is Pass
B's own plan-time work); on failure (today, always) the tool returns
`ToolOutcome::Failed` with `VisionError::Unsupported`'s message, no output
payload.

**7. `reference_image` is restricted to the tool's own output directory —
no arbitrary filesystem paths.** This is the one real security-relevant
decision in this design, so it's worth stating the reasoning in full:
`aivyx-vision` runs as a separate OS process with **no access to the
daemon's own fs sandbox/access policy** (each tool process is independently
configured and trusted, the same way `aivyx-gmail`'s OAuth tokens aren't
shared with the daemon). If `reference_image` accepted an arbitrary absolute
path, an agent holding only the `vision.generate` scope (SemiTrusted, well
below what `fs.read` on a sensitive path would require) could read *any*
file the OS user running this process can read and have its bytes sent as
base64 to `mold_url` — a real exfiltration path if that URL is ever
non-loopback. Rather than build cross-process sandbox-forwarding machinery
(no existing precedent for this in the codebase, real scope creep for this
pass), `GenerateImageTool` accepts `reference_image` as a **bare filename
only** (rejects anything containing a path separator or `..`), resolves it
against the configured `output_dir`, and validates the canonicalized result
still lives under the canonicalized `output_dir` — the exact idiom
`aivyx-core`'s `fs.read`/`fs.write` already use, hand-rolled here since this
crate has no dependency on `aivyx-core::tools::fs`. Net effect:
`reference_image` can only ever reference a file this tool itself
previously wrote (i.e., chained image-to-image off a prior
`vision.generate_image` call), never an arbitrary path.

**8. No budget/rate-limit hook.** Chapter K's `BudgetEnforcer`/`RateLimiter`
are dollar/token-priced around LLM spend; local GPU generation time doesn't
fit that model, and `aivyx-broker`'s GPU lock already prevents unbounded
concurrent GPU usage across processes on the machine (a call queues, or
times out with a clear `VisionError::GpuLockTimeout`, rather than piling up
silently). Confirmed with the project owner: skip for this pass, revisit if
real usage shows a need.

**9. Dependency pinning: bump the existing `aivyx-vision-svg` pin and add
`aivyx-vision-core`/`aivyx-vision-mold`, all three at the same rev
(`caed4c0`)** — the commit Milestone 2 Pass A merged at in the `aivyx-vision`
repo, containing all three sibling crates. Keeps them in lockstep rather
than letting `aivyx-vision-svg`'s pin drift stale relative to the two new
ones.

## Testing

- `GenerateImageTool`/`GenerateThreeDTool` unit tests use
  `aivyx_vision_core::FakeGenerationProvider` (behind that crate's
  `testing` Cargo feature, dev-dependency only in `aivyx-vision`) — the
  exact test double `aivyx-vision-core`'s own design built for this
  purpose (see its module doc comment). Mirrors `GenerateSvgTool`'s own
  `FakeCompleter` test pattern.
- The `reference_image` path-validation helper gets its own pure unit
  tests: a bare filename inside `output_dir` is accepted; anything with a
  path separator, `..`, or resolving outside the canonicalized
  `output_dir` is rejected.
- `config.rs`'s existing TOML-loading test module gains `[mold]`-present
  and `[mold]`-absent cases, following the exact shape of its current
  provider-config tests.
- No test requires a real `mold serve` or `aivyx-broker` process — matches
  `aivyx-vision-mold`'s own CI-never-needs-a-GPU constraint, inherited
  here since `FakeGenerationProvider` never makes a real network call.

## Documentation

- `docs/TOOLS.md`: two new rows in the existing "Aivyx-Vision" table
  (`vision.generate_image`, `vision.generate_3d`, both `vision.generate` /
  SemiTrusted).
- `crates/aivyx-vision/README.md`: `[mold]` config example, both new
  tools' input/output JSON shapes, and the `reference_image` scoping rule
  stated plainly (so an operator/agent-author isn't surprised by the
  restriction).

## What this design does not decide (explicitly out of scope)

- Pass B itself (a real `generate_3d` implementation) — separate,
  not-yet-scoped future work in the `aivyx-vision` repo.
- Budget/rate-limiting integration (decision 8) — deferred, not resolved.
- Any cross-process fs-sandbox-forwarding mechanism — `reference_image`'s
  output-dir restriction (decision 7) is this pass's answer; a more
  general mechanism, if ever needed, is separate future work.
- Retention/cleanup of generated files — matches the ecosystem spec's own
  v1 default (operator-managed, no automatic sweep); this design writes
  files and returns paths, nothing more.
- `aivyx-coder`'s equivalent adoption — a separate plan in that repo,
  exactly as Milestone 1's SVG adoption was split into two per-product
  plans.
