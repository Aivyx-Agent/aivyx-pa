# Aivyx-Skills Integration (Part 3: aivyx-pa) Design

## Context

Part 3 of a three-part cross-repo initiative: wire the shared, already-shipped
`aivyx-skills` crate (`Aivyx-Agent/aivyx-skills`, public — a default,
system-level `SKILL.md`-format capability library, 5 real bundled skills:
`systematic-debugging`, `brainstorming-and-scoping`, `writing-plans`,
`self-review-before-done`, `clear-communication`) into `aivyx-pa` so its own
agent can discover and use them, plus an optional project/user skill
overlay. Part 1 (the crate itself) and Part 2 (`aivyx-coder`'s own
integration) are both already shipped, merged to `main` in their respective
repos.

This part is explicitly bigger/trickier than Part 2: `aivyx-pa` already has
a mature, unrelated, pre-existing agent-learned `Skill`/`LearnedSkill`
system (Chapters Praxis/Whetstone/Repertoire) — skills the agent itself
proposes or refines, which the operator approves, stored as Persona-chain
deltas, surfaced via `skills.list`/`skills.invoke` substrate tools, a
`## Learned skills` system-prompt section, an effectiveness ledger (EWMA
scoring), versioning/provenance, and a read-only Studio screen. The new
mechanism must be clearly distinguished from it — different vocabulary,
different tool namespace, different prompt section, and it must not feed
any data into Whetstone's effectiveness machinery — while following
`aivyx-pa`'s own established architectural conventions (capability-gated
tools, source-provenance-tracked config, the substrate/infrastructure/
third-party tool taxonomy).

## Grounding

Read directly in the current codebase, not assumed:

- **The existing `skills.*` machinery, read in full**
  (`crates/aivyx-core/src/tools/skills.rs`): `SkillsListTool`/
  `SkillsInvokeTool`, both taking a `SkillReader` closure
  (`Arc<dyn Fn() -> Vec<String> + Send + Sync>`) that reads the daemon's
  `SharedEffectivePersona` under a lock — a pattern specifically chosen so
  `aivyx-core` doesn't take a dependency edge on `aivyx-channel`. `name`/
  `trigger`/`procedure` fields; list elides `procedure`, invoke returns it.
  Malformed Persona-chain entries are silently skipped (chain-corruption
  defense, mirrors Phase 60 Persona revert semantics). `skills.invoke`
  emits a dedicated `AuditTag::SkillInvocation` audit entry (in cleartext,
  unlike the generic `ToolCall`'s hashed input) specifically so
  `skill_effectiveness.rs` can build per-skill EWMA rows.
- **The `## Learned skills` prompt render**
  (`crates/aivyx-channel/src/profile_prompt.rs::render_skills_section`,
  called from `assemble_session_prompt_with_relevance`): one bullet per
  skill (`name: trigger`), plus a fixed reinforcement string about the
  `skills.teach`/`skills.update`/`skills.forget` conversational-confirm
  protocol. Renders independently of the `## Persona` section (a Persona
  whose only content is skill deltas must not produce an empty Persona
  block) — `assemble_session_prompt_with_relevance` already takes an
  `Option<&str>` pre-rendered section (`relevance_section`) as a template
  for how to thread in a new one.
- **`docs/REPERTOIRE.md`'s own title is literally "The Skills Library"** —
  confirms `skill_library`/`skills library` as a name is already taken by
  the existing Studio screen, not just by code identifiers.
- **The registration site**
  (`crates/aivyx-cli/src/bin/aivyx.rs`, where `SkillsListTool`/
  `SkillsInvokeTool` are constructed): "Registration here is
  unconditional; the tier-ceiling intersection at agent construction time
  enforces the SemiTrusted exclusion" — there is no separate
  `[config].enabled` boolean gating these tools' presence in the
  registry; capability-ceiling membership is the only gate. Confirmed via
  `crates/aivyx-capability/src/lib.rs`: `skills.list`/`skills.invoke`/
  `skills.propose`/`skills.write`/`graph.read` are all present in
  `CEILING_TRUSTED` and absent from `CEILING_SEMITRUSTED` (grepped both
  tables directly).
- **The substrate/infrastructure/third-party taxonomy and the locked
  substrate-15 count** (`docs/amendments/2026-06-19-substrate-tool-count-
  fifteen.md`, `docs/amendments/2026-04-17-capability-taxonomy-growth.md`):
  `PRODUCT.md`'s P10 fixes an exact, named list of 15 first-party
  substrate tools (`fs.*`, `memory.*`, `shell.exec`, `web.*`, `git.*`,
  `net.dns`) — operator-facing operations against operator-owned external
  resources. Changing that list requires a full `PRODUCT.md` amendment.
  `skills.list`/`skills.invoke`/`graph.read` are explicitly carved out as
  **infrastructure** instead — "the agent querying its OWN derived
  self-knowledge" (`graph.read`'s own doc comment) — which only needs the
  lighter `capability-taxonomy-growth`-style addendum (bumping
  `KNOWN_BASES.len()` and its pinning test) when a new base is added, not
  a substrate-count amendment. Bundled/overlay default skills are, if
  anything, a cleaner fit for "infrastructure" than `LearnedSkill` itself
  (compiled-in constants / static filesystem reads, not even
  chain-stored operator data).
- **The `[git] repos` allow-list pattern, and why it does NOT directly
  transfer here** (`docs/amendments/2026-06-19-substrate-tool-count-
  fifteen.md`'s `git.write` section; `crates/aivyx-config/src/lib.rs::
  GitConfig`): `git.commit`'s `PathGlob` qualifier exists because the
  *model* supplies a repo path as tool input, and the qualifier bounds
  that model-chosen value against an operator allow-list
  (`[git] repos: Vec<Sourced<PathBuf>>`) — the same shape `fs.read`'s
  `fs.read:<root>/**` qualifier uses for a model-chosen file path.
  `skill_defaults.read`'s input is a skill **name**, never a path — the
  model has no way to name an arbitrary filesystem location at all; the
  entire reachable set (bundled + whichever overlay dirs are configured)
  is fixed server-side by the operator's `[skill_defaults]` config, the
  same way `skills.list`/`skills.invoke`'s reachable set is fixed by
  whatever `LearnedSkill` entries exist in the Persona chain — and
  neither of *those* tools carries a qualifier either. A qualifier layer
  would defend against an input shape (a model-supplied path) that
  doesn't exist in this tool's schema, so Decision 3 below deliberately
  does not add one — the flat, non-qualified `skill_defaults.list`/
  `skill_defaults.read` bases are enough, matching `skills.list`/
  `skills.invoke`'s own precedent.
- **`Tool::output_is_untrusted()`** (`crates/aivyx-core/src/lib.rs:939`):
  defaults `false`; overridden `true` by `fs.read`/`web.fetch`/
  `git.diff`/`shell.exec` — anything returning file content, network
  content, or command output. When `true`, the turn loop's Picket
  (`check_for_injection`, `aivyx-injection-guard`'s
  `scan_for_injection_markers`) and Bulwark (`fence_untrusted_output`)
  automatically cover every call's output — no bespoke per-tool scanning
  code needed, exactly the same "generic, unconditional, per-tool-output
  mechanism already exists" shape Part 2 had to learn about
  `Agent::record_tool_result` the hard way. Confirmed no existing tool in
  `crates/aivyx-core/src/tools/` implements its own injection scan (grep
  for `scan_for_injection_markers` outside `agent.rs`/`lib.rs` returns
  nothing) — Picket is the sole call site.
- **What `AuditTag::SkillInvocation` feeds** — grepped every consumer:
  `skill_effectiveness.rs` (Whetstone's EWMA scoring),
  `skill_trigger_context.rs`, `tool_relevance_ledger.rs`. All three
  assume every invocation traces to a real `LearnedSkill` Persona-chain
  entry with a lineage/version/provenance to score. A bundled or overlay
  default skill has no chain entry — reusing this tag would either
  silently corrupt that machinery's assumptions or require threading a
  "not actually a LearnedSkill" exception through three files that were
  never designed for one.
- **`AivyxConfig`'s `Option`-wrapped section precedent**
  (`crates/aivyx-config/src/lib.rs:1044-1047`): `pub skill_authoring:
  Option<SkillAuthoringConfig>`, `None` when the `[skill_authoring]`
  section is absent — the template for the new `skill_defaults` section
  (rather than `aivyx-coder`'s own `#[serde(default)]`-with-a-manual-
  `Default`-impl shape, which isn't this codebase's convention).
- **`aivyx-skills`'s real public API** (confirmed directly from its own
  shipped `src/lib.rs`/`src/loader.rs`, same as Part 2 relied on):
  `SkillLoader::new()`, `.with_project_dir(PathBuf) -> Self`,
  `.with_user_dir(PathBuf) -> Self`, `.list() -> Vec<SkillSummary>`
  (sorted by name, `{name, description, source}`), `.get(name: &str) ->
  Option<Skill>` (`{name, description, body, source}`). Pinned rev
  `99a0298828d80bb18175671ef66b61d5e0133bf7` (current HEAD, same as Part
  2's pin — confirmed unchanged since).
- **The pinned-external-crate dependency pattern**
  (`Cargo.toml`'s `[workspace.dependencies]`,
  `crates/aivyx-core/Cargo.toml`'s `[dependencies.aivyx-injection-guard]`
  block): `{ git = "...", rev = "<sha>" }` at the workspace level, `{
  workspace = true }` at the consuming crate — identical shape to
  `aivyx-coder`'s own convention, already used here for
  `aivyx-injection-guard`/`aivyx-checkpoint`.

## Decisions

**1. Two new tools, `skill_defaults.list` and `skill_defaults.read`**, in a
new `crates/aivyx-core/src/tools/skill_defaults.rs`, mirroring
`skills.rs`'s file shape and `Tool` trait implementation exactly. Neither
takes a `SkillReader`-style closure — both hold an `Arc<aivyx_skills::
SkillLoader>` directly (cheap, stateless, no lock needed, unlike the
Persona-chain-backed reader `skills.rs` needs). `skill_defaults.list`
returns `{skills: [{name, description}]}` (no body — the same list/invoke
split rationale `skills.rs`'s own doc comment already gives, reused
verbatim). `skill_defaults.read` takes `{skill: String}`, returns
`{name, description, body}` on a match or a clean failure naming valid
skills on no match (mirroring `skills.invoke`'s exact "no approved skill
named X" error shape, adapted vocabulary). `output_is_untrusted()` returns
`true` unconditionally (bundled and overlay results alike) — matching
`fs.read`'s own blanket, call-independent policy, and meaning Picket/
Bulwark cover every `skill_defaults.read` result automatically with zero
bespoke scanning code in this file.

**2. Two new capability bases, `skill_defaults.list` / `skill_defaults.read`**,
added to `KNOWN_BASES` and `CEILING_TRUSTED` only (absent from
`CEILING_SEMITRUSTED`), matching `skills.list`/`skills.invoke`/
`graph.read`'s own precedent exactly. Registration in `aivyx.rs` is
unconditional (like `skills.list`/`skills.invoke`'s own registration) —
the capability ceiling is the only gate, no separate enable flag. Recorded
via a `capability-taxonomy-growth`-style amendment addendum (bumping
`KNOWN_BASES.len()`'s pinning test), classified **infrastructure**, not
substrate — no `PRODUCT.md` P10 amendment needed (Grounding).

**3. `AivyxConfig` gains `pub skill_defaults: Option<SkillDefaultsConfig>`**,
`None` when the `[skill_defaults]` section is absent (matching
`skill_authoring`'s own `Option`-wrapped precedent, not `aivyx-coder`'s
`#[serde(default)]`-plus-manual-`Default`-impl shape — different
codebases, different established conventions).
`SkillDefaultsConfig { project_dir: Option<Sourced<PathBuf>>, user_dir:
Option<Sourced<PathBuf>> }`, matching `GitConfig::repos`'s
source-provenance-tracked shape for a config-supplied path. No
`PathGlob` qualifier on the capability bases themselves (Grounding
explains why `git.commit`'s qualifier pattern doesn't fit a
name-only tool input) — the operator's own act of setting
`project_dir`/`user_dir` in `[skill_defaults]` *is* the authorization
boundary, the same way configuring `[skill_authoring]` at all is what
authorizes that feature, with no separate per-call qualifier layered on
top. The `SkillLoader` is built once at daemon startup (`aivyx.rs`) from
whichever of `project_dir`/`user_dir` are configured; the flat
`skill_defaults.list`/`skill_defaults.read` capability bases (Decision 2)
are the only gate on calling the tools at all.

**4. `assemble_session_prompt_with_relevance` gains a new
`default_skills_section: Option<&str>` parameter**, threaded and rendered
the same way `relevance_section` already is — pre-rendered once by the
caller (not by `profile_prompt.rs` itself), inserted as its own `##
Default skills` block, positioned after `## Learned skills` (when
present) and before the relevance section. Rendering (one bullet per
skill: `name: description`) happens once at daemon startup in `aivyx.rs`
from the same `SkillLoader` instance the tools hold — not re-scanned per
turn, matching the "the skill library is effectively static for a process
run" reasoning Part 2 already established for `aivyx-coder`'s own
`agent_builder.rs`. A skill added to an overlay directory mid-session
isn't picked up until the daemon restarts — the same disclosed,
accepted residual-risk shape Part 2 landed on for the analogous gap in
`load_skill.rs`'s own tool description.
`assemble_session_prompt` itself — the simpler 4-argument wrapper every
current production call site actually uses — keeps its existing
signature unchanged (avoiding churn across its own ~15+ existing test
call sites and every production caller not otherwise touched by this
work). Every production call site that needs the new section instead
switches from calling `assemble_session_prompt(...)` to calling
`assemble_session_prompt_with_relevance(..., None, default_skills_section)`
directly — passing `None` for `relevance_section` (matching that
argument's real, already-existing production value today; no production
code currently constructs a relevance section) and the new pre-rendered
string for the new parameter.

**5. The `## Default skills` render scans each overlay-sourced
(`SkillSource::User`/`SkillSource::Project`) entry's composed
`name: description` text for injection markers before including it, via
`aivyx_injection_guard::scan_for_injection_markers` directly — but with
no `InjectionTaint`-equivalent to flag.** Picket (`check_for_injection`)
is turn-scoped: it runs inside the live turn loop and escalates through
that turn's own outcome recording. The `## Default skills` listing is
composed once at daemon startup, before any turn exists, so there is no
turn-scoped mechanism available to flag into at that point (unlike Part
2's `aivyx-coder`, where `InjectionTaint` is a persistent, agent-attached
handle a startup-time computation can reach). So a match's effect is
narrower and fail-closed instead: that one entry is **excluded from the
rendered listing** (never advertised in the system prompt) and logged via
`tracing::warn!` at startup — it is not, on its own, escalated as a live
security event, since there is no turn to escalate within yet. This is
not a coverage gap for the entry's *body*, though: if the model still
calls `skill_defaults.read` for that same (now-unlisted) name — by
guessing it, or being told it out of band — Decision 1's
`output_is_untrusted() == true` means Picket/Bulwark cover that real,
turn-scoped call exactly as they cover any other tool result, with full
escalation. Bundled entries (`SkillSource::Bundled`) are never scanned.
This is the *only* place in this design that calls
`scan_for_injection_markers` directly outside Picket's own call site —
`skill_defaults.read`'s tool output needs no scan of its own beyond what
Decision 1 already gives it generically, exactly mirroring why Part 2's
`load_skill` tool needed none either. The render function lives in
`crates/aivyx-core/src/tools/skill_defaults.rs` (same file as the two
tools, Decision 1) rather than in `aivyx-channel`'s `profile_prompt.rs` —
`aivyx-core` already depends on both `aivyx-skills` and
`aivyx-injection-guard`, while `aivyx-channel` currently depends on
neither; keeping the scan there avoids adding a new dependency edge.
`aivyx.rs` calls this function once at startup and hands the resulting
`String` into `assemble_session_prompt_with_relevance` (Decision 4) as a
plain, already-composed `Option<&str>` — `profile_prompt.rs` itself does
no scanning and gains no new dependency.

**6. No new `AuditTag` variant.** `skill_defaults.read` emits only the
turn loop's normal per-tool-call audit entry (D1's contract — every tool
call is audited regardless) and nothing else. `AuditTag::SkillInvocation`
is deliberately not reused and no sibling tag is added — Grounding
confirms every consumer of that tag (`skill_effectiveness.rs`,
`skill_trigger_context.rs`, `tool_relevance_ledger.rs`) assumes a real
`LearnedSkill` Persona-chain entry exists to score/track, which a bundled
or overlay default skill never has.

**7. Dependency**: `crates/aivyx-core/Cargo.toml` gains
`[dependencies.aivyx-skills]` `workspace = true`; the workspace root
`Cargo.toml` gains `aivyx-skills = { git =
"https://github.com/Aivyx-Agent/aivyx-skills", rev =
"99a0298828d80bb18175671ef66b61d5e0133bf7" }` in `[workspace.dependencies]`
— identical declared shape to `aivyx-injection-guard`/`aivyx-checkpoint`
above it, and to `aivyx-coder`'s own Part 2 pin (same rev).

## What this spec does not decide

- Any change to `aivyx-skills` itself (the crate's own API, content, or
  format) — consumed exactly as shipped, same as Part 2.
- Any change to the existing `skills.*`/`LearnedSkill` tools, their
  `## Learned skills` prompt section, or the Whetstone/Praxis/Repertoire
  machinery behind them — this design adds a fully parallel surface,
  touches none of that code.
- Any Repertoire/Studio UI surface for default skills (a browsable
  screen, a keybinding) — out of scope; the prompt section and the two
  tools' own error messages are the only surfaces.
- Any change to `aivyx-injection-guard`/`scan_for_injection_markers`
  itself, to Picket/Bulwark, or to how `fs.read`/`web.fetch`/`git.diff`
  are scanned — Decision 5 adds one new direct call site (the prompt
  render) using the existing, unmodified mechanism; Decision 1 relies on
  the existing, unmodified Picket/Bulwark pipeline for the tool-output
  path.
- The `PRODUCT.md`/`DESIGN.md` amendment text itself — Decision 2
  establishes the classification (infrastructure, not substrate) and the
  addendum shape to follow, but drafting and filing the actual amendment
  document is implementation-plan work, not a design-spec deliverable.
- Making the skill *body* itself subject to further processing (variable
  substitution, nested skill references) — returned verbatim, exactly as
  `aivyx-skills::Skill.body` provides it, same as Part 2.
