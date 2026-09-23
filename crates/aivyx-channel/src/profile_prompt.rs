//! Profile-into-system-prompt assembly. Phase 57 Task 3 —
//! PRODUCT.md P13 (Assistant Profile).
//!
//! [`assemble_session_prompt`] composes the operator-declared
//! [`Profile`] alongside the active role's `system_prompt` into the
//! final system prompt the LLM sees. Per Q3(c) at Phase 57 sign-off,
//! the composition is **labeled** — not concatenated:
//!
//! ```text
//! ## About this assistant
//!
//! Your name is <name>.
//! About the operator: <...>
//! Communication style: <...>
//! Primary use cases:
//! - ...
//! Behavioral preferences:
//! - ...
//! Behavioral constraints:
//! - ...
//!
//! ## Active role: <role-name>
//!
//! <role.system_prompt>
//! ```
//!
//! Labels matter for two reasons:
//!
//! 1. **LLM interpretability.** The model sees a layered identity
//!    structure (Profile → role) rather than one undifferentiated
//!    paragraph.
//! 2. **Phase 60 insertion point.** When Persona (P14) lands, its
//!    delta-derived voice section inserts between the Profile
//!    section and the active-role section, with its own label. The
//!    label-shaped layout makes that insertion structural rather
//!    than a string-rewrite.
//!
//! **Non-invasive on legacy configs.** When [`Profile::is_operator_declared`]
//! returns `false` — i.e. no `[profile]` section in `aivyx-pa.toml`,
//! only the synthesized default with `assistant_name = "Aivyx PA"` —
//! the helper returns the role's `system_prompt` unchanged. No
//! "Your name is Aivyx PA" noise prepended to every default config.

use aivyx_config::{OllamaFamilyStrategy, Profile};
use aivyx_llm::LlmToolDescriptor;

use crate::persona::EffectivePersona;

/// Compose the final system prompt for one turn by layering Profile
/// (operator-declared identity per PRODUCT.md P13), Persona
/// (reflection-written identity per PRODUCT.md P14), and the active
/// role's `system_prompt` (per-role voice override per P9).
///
/// **Behavior:**
/// - If neither Profile nor Persona has content (Profile at the
///   synthesized default AND Persona empty / `None`), return
///   `role_system_prompt.to_string()` unchanged. Zero behavior
///   change for pre-Phase-57 configs.
/// - Otherwise return a labeled composition:
///   1. "## About this assistant" — Profile fields (omitted if
///      Profile is at the synthesized default).
///   2. "## How I have learned to communicate" — Persona fields
///      (omitted if Persona is empty / `None`). Phase 59 Q6(a).
///   3. "## Active role: <role_name>" — the role's prompt.
///
/// `role_name` is rendered into the third section's label so the
/// LLM (and a debugging operator reading prompt logs) can see which
/// role is active.
///
/// The helper allocates a fresh `String` per call. The allocation is
/// dwarfed by the per-turn LLM round-trip cost, so micro-optimizing
/// is not worth the trade against composition clarity.
pub fn assemble_session_prompt(
    profile: &Profile,
    persona: Option<&EffectivePersona>,
    role_name: &str,
    role_system_prompt: &str,
) -> String {
    // Phase 114-compat wrapper that forwards `None` for the
    // Phase 116 relevance section. Existing callers that
    // don't yet pass the relevance signal stay on this entry.
    assemble_session_prompt_with_relevance(
        profile,
        persona,
        role_name,
        role_system_prompt,
        None,
        None,
    )
}

/// Phase 116 — same as [`assemble_session_prompt`] but takes
/// an optional pre-rendered `## Tools recently used for
/// similar tasks` section to slot between the Persona/Skills
/// sections and the active role. Callers that have a
/// [`tool_relevance_ledger`](crate::tool_relevance_ledger)
/// handle render the section with
/// [`tool_relevance_ledger::render_relevance_section`] and
/// pass the result here; callers without the ledger keep
/// using [`assemble_session_prompt`] directly.
pub fn assemble_session_prompt_with_relevance(
    profile: &Profile,
    persona: Option<&EffectivePersona>,
    role_name: &str,
    role_system_prompt: &str,
    relevance_section: Option<&str>,
    default_skills_section: Option<&str>,
) -> String {
    let profile_active = profile.is_operator_declared();
    // Phase 110 — skills get their own `## Learned skills`
    // section between Persona and active role per Q3c sign-off.
    // The Persona section renders only when *non-skill* fields
    // are non-empty (so a Persona that only has skill deltas
    // doesn't produce an empty Persona block). Skills render
    // independently when learned_skills is non-empty.
    let persona_active = persona.map(persona_has_non_skill_content).unwrap_or(false);
    let skills_active = persona
        .map(|p| !p.learned_skills.is_empty())
        .unwrap_or(false);
    let relevance_active = relevance_section
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);
    let default_skills_active = default_skills_section
        .map(|s| !s.trim().is_empty())
        .unwrap_or(false);

    if !profile_active && !persona_active && !skills_active && !relevance_active
        && !default_skills_active
    {
        return role_system_prompt.to_string();
    }

    let mut out = String::new();
    if profile_active {
        out.push_str(&render_profile_section(profile));
        out.push_str("\n\n");
    }
    if persona_active {
        // Safe to unwrap — `persona_active` requires Some.
        out.push_str(&render_persona_section(persona.unwrap()));
        out.push_str("\n\n");
    }
    if skills_active {
        // Same Some-guarantee — `skills_active` requires Some.
        out.push_str(&render_skills_section(persona.unwrap()));
        out.push_str("\n\n");
    }
    if default_skills_active {
        // Safe to unwrap — `default_skills_active` requires Some
        // AND non-empty.
        out.push_str(default_skills_section.unwrap().trim_end());
        out.push_str("\n\n");
    }
    if relevance_active {
        // Safe to unwrap — `relevance_active` requires Some
        // AND non-empty.
        out.push_str(relevance_section.unwrap().trim_end());
        out.push_str("\n\n");
    }
    out.push_str(&format!(
        "## Active role: {role_name}\n\n{role_system_prompt}"
    ));
    out
}

/// Phase 110 — `true` iff the persona has any non-skill
/// content. Used by [`assemble_session_prompt`] to decide
/// whether the `## How I have learned to communicate`
/// section renders. A Persona whose only deltas are
/// `LearnedSkill` produces an empty Persona block — the
/// skills section renders separately under its own label.
fn persona_has_non_skill_content(p: &EffectivePersona) -> bool {
    p.assistant_name.is_some()
        || p.operator_profile.is_some()
        || p.communication_style.is_some()
        || !p.primary_use_cases.is_empty()
        || !p.behavioral_preferences.is_empty()
        || !p.behavioral_constraints.is_empty()
        || !p.learned_context.is_empty()
        || !p.communication_adaptations.is_empty()
        || !p.character_traits.is_empty()
        || !p.relationship_milestones.is_empty()
}

/// Phase 110 — render the `## Learned skills` section. Each
/// approved skill becomes one bullet of `name: trigger`. The
/// full procedure body is elided (reserved for `skills.invoke`)
/// to avoid bloating every system prompt with every skill's
/// body text. Malformed entries are silently skipped (the
/// `LearnedSkill::from_json_value` failure path); the render
/// is best-effort by design — a malformed chain entry should
/// not take down a turn.
fn render_skills_section(persona: &EffectivePersona) -> String {
    let mut out = String::from("## Learned skills\n\n");
    for raw in &persona.learned_skills {
        if let Some(skill) = crate::persona::LearnedSkill::from_json_value(raw) {
            out.push_str(&format!("- {}: {}\n", skill.name, skill.trigger));
        }
    }
    // Phase 184 — the conversational skill-teaching protocol. The
    // tool descriptions + the required `confirmed` field enforce
    // it; this reinforces it whenever the operator already has
    // skills. When the operator teaches, refines, or drops a
    // skill, DRAFT the change, SHOW it (name + when-to-use +
    // steps), and only after they confirm call `skills.teach` /
    // `skills.update` / `skills.forget` with `confirmed: true`.
    out.push_str(
        "\nWhen the operator teaches, refines, or drops a skill, \
         draft the change, show them the name + when-to-use + \
         steps, and only after they confirm call `skills.teach` / \
         `skills.update` / `skills.forget` with `confirmed: true`.\n",
    );
    out
}

// ---------------------------------------------------------------------------
// Phase 79 — adaptive Persona: reduced-Persona assembly
// ---------------------------------------------------------------------------

/// Number of *reducible* (soft) Persona list entries. The
/// adaptive refiner uses this for its size-threshold fallback
/// (Q3a): below the threshold there is nothing worth selecting
/// over, so the full Persona is injected unchanged.
///
/// Excludes the protected fields — the scalar identity and
/// `behavioral_constraints` are never reduced, so they never
/// count toward "is the Soul big enough to bound."
pub fn reducible_facet_count(p: &EffectivePersona) -> usize {
    p.primary_use_cases.len()
        + p.behavioral_preferences.len()
        + p.learned_context.len()
        + p.communication_adaptations.len()
        + p.character_traits.len()
        + p.relationship_milestones.len()
}

/// Build a reduced [`EffectivePersona`] keeping only the soft
/// list entries for which `keep` returns `true`.
///
/// **Core invariant (Phase 79 Q2a), structurally enforced
/// here so no caller can violate it:** the scalar identity
/// (`assistant_name`, `operator_profile`, `communication_style`)
/// and `behavioral_constraints` are copied through **in full,
/// unconditionally** — `keep` is *only* ever applied to the six
/// soft list categories. Identity and guardrails are
/// non-negotiable and can never be selected away, regardless of
/// what the selector decides.
pub fn reduce_persona(
    full: &EffectivePersona,
    keep: &dyn Fn(&str) -> bool,
) -> EffectivePersona {
    let filter = |v: &[String]| -> Vec<String> {
        v.iter().filter(|s| keep(s)).cloned().collect()
    };
    EffectivePersona {
        // --- protected: always copied in full (the invariant) ---
        assistant_name: full.assistant_name.clone(),
        operator_profile: full.operator_profile.clone(),
        communication_style: full.communication_style.clone(),
        behavioral_constraints: full.behavioral_constraints.clone(),
        // --- reducible soft list categories ---
        primary_use_cases: filter(&full.primary_use_cases),
        behavioral_preferences: filter(&full.behavioral_preferences),
        learned_context: filter(&full.learned_context),
        communication_adaptations: filter(
            &full.communication_adaptations,
        ),
        character_traits: filter(&full.character_traits),
        relationship_milestones: filter(&full.relationship_milestones),
        // Phase 110 — LearnedSkill entries pass through the filter
        // alongside other reducible list categories. The renderer
        // at assemble_session_prompt elides body text from the
        // system prompt; the filter still applies for consistency
        // with other list categories.
        learned_skills: filter(&full.learned_skills),
        // Phase 118 — ProfileHint + RoleDefinitionSuggestion
        // entries are operator-review artifacts; they sit in the
        // Persona chain as approved-but-staged suggestions. The
        // turn renderer elides them entirely. Pass through the
        // filter for consistency with other list categories so
        // the struct literal stays uniform.
        profile_hints: filter(&full.profile_hints),
        role_drafts: filter(&full.role_drafts),
    }
}

/// Assemble the turn's system prompt with only the
/// contextually-selected Persona facets. Thin wrapper:
/// [`reduce_persona`] (invariant enforced) →
/// [`assemble_session_prompt_with_relevance`]. Used by the
/// Phase 79 refiner; kept here so the reduction and the
/// invariant are tested in one place.
///
/// `default_skills_section` — Aivyx-Skills Part 3's optional
/// pre-rendered `## Default skills` section. The Phase 79
/// refiner REBUILDS the prompt from scratch rather than
/// composing onto an existing base, so this must be threaded
/// through explicitly here (and by the caller, from its own
/// pre-rendered section) or the section silently disappears
/// from every turn the refiner engages on, even though it's
/// threaded correctly everywhere else. `None`/empty is
/// byte-identical to the pre-Aivyx-Skills-Part-3 output.
pub fn assemble_session_prompt_selected(
    profile: &Profile,
    full_persona: &EffectivePersona,
    keep: &dyn Fn(&str) -> bool,
    role_name: &str,
    role_system_prompt: &str,
    default_skills_section: Option<&str>,
) -> String {
    let reduced = reduce_persona(full_persona, keep);
    assemble_session_prompt_with_relevance(
        profile,
        Some(&reduced),
        role_name,
        role_system_prompt,
        None,
        default_skills_section,
    )
}

fn render_profile_section(profile: &Profile) -> String {
    let mut out = String::from("## About this assistant\n\n");
    out.push_str(&format!(
        "Your name is {name}.\n",
        name = profile.assistant_name.value,
    ));
    if let Some(op) = &profile.operator_profile {
        out.push_str(&format!("\nAbout the operator: {op}\n"));
    }
    if let Some(style) = &profile.communication_style {
        out.push_str(&format!("\nCommunication style: {style}\n"));
    }
    if !profile.primary_use_cases.is_empty() {
        out.push_str("\nPrimary use cases:\n");
        for use_case in &profile.primary_use_cases {
            out.push_str(&format!("- {use_case}\n"));
        }
    }
    if !profile.behavioral_preferences.is_empty() {
        out.push_str("\nBehavioral preferences:\n");
        for pref in &profile.behavioral_preferences {
            out.push_str(&format!("- {pref}\n"));
        }
    }
    if !profile.behavioral_constraints.is_empty() {
        out.push_str("\nBehavioral constraints:\n");
        for c in &profile.behavioral_constraints {
            out.push_str(&format!("- {c}\n"));
        }
    }
    // Trim the trailing newline so the `\n\n## Active role` join
    // produces exactly one blank line between sections, not two.
    out.trim_end().to_string()
}

/// Render the Persona section. Phase 59 Q6(a): a labeled
/// "## How I have learned to communicate" block whose body
/// enumerates whichever Persona categories carry content.
///
/// Persona's scalar categories (assistant_name,
/// operator_profile, communication_style) intentionally render
/// *underneath* the Persona header rather than overriding the
/// Profile section's analogous fields — Persona refinements are
/// learned, not declared, and operators reading the prompt should
/// see them in the learned-section so they can tell what the
/// agent has decided versus what they originally declared.
fn render_persona_section(persona: &EffectivePersona) -> String {
    let mut out = String::from("## How I have learned to communicate\n\n");

    // Scalars are emitted as `Refined X: <value>` lines so the
    // operator reading the prompt understands the field was
    // refined by reflection, not declared by them.
    if let Some(name) = &persona.assistant_name {
        out.push_str(&format!("Refined name: {name}\n"));
    }
    if let Some(op) = &persona.operator_profile {
        out.push_str(&format!("Refined operator profile: {op}\n"));
    }
    if let Some(style) = &persona.communication_style {
        out.push_str(&format!("Refined communication style: {style}\n"));
    }

    if !persona.primary_use_cases.is_empty() {
        out.push_str("\nLearned use cases:\n");
        for case in &persona.primary_use_cases {
            out.push_str(&format!("- {case}\n"));
        }
    }
    if !persona.behavioral_preferences.is_empty() {
        out.push_str("\nLearned behavioral preferences:\n");
        for pref in &persona.behavioral_preferences {
            out.push_str(&format!("- {pref}\n"));
        }
    }
    if !persona.behavioral_constraints.is_empty() {
        out.push_str("\nLearned behavioral constraints:\n");
        for c in &persona.behavioral_constraints {
            out.push_str(&format!("- {c}\n"));
        }
    }
    if !persona.learned_context.is_empty() {
        out.push_str("\nLearned context:\n");
        for c in &persona.learned_context {
            out.push_str(&format!("- {c}\n"));
        }
    }
    if !persona.communication_adaptations.is_empty() {
        out.push_str("\nCommunication adaptations:\n");
        for c in &persona.communication_adaptations {
            out.push_str(&format!("- {c}\n"));
        }
    }
    if !persona.character_traits.is_empty() {
        out.push_str("\nCharacter traits:\n");
        for c in &persona.character_traits {
            out.push_str(&format!("- {c}\n"));
        }
    }
    if !persona.relationship_milestones.is_empty() {
        out.push_str("\nRelationship milestones:\n");
        for c in &persona.relationship_milestones {
            out.push_str(&format!("- {c}\n"));
        }
    }
    out.trim_end().to_string()
}

/// Phase 122 Task 3 — Append a `## Tools available` block to
/// an already-assembled session prompt.
///
/// **Why:** pre-Phase-122 testing found that qwen3.6:27b and
/// gemma4:31b confabulate tool catalogs at the prose level
/// (qwen3.6 invented "Good Morning"; gemma4 invented 60+ tools)
/// and refuse to invoke even tools they were commanded to call
/// by exact name. The Ollama protocol's `tools: [...]` array
/// reaches the model's tool-call surface but apparently not its
/// prose-level reasoning. This helper injects the catalog
/// directly into the system prompt where the prose layer
/// cannot ignore it.
///
/// **Behavior:**
/// - If `tools` is empty, return `base_prompt.to_string()`
///   unchanged. Calling sites that pass an empty tool slice
///   get no-op behavior — no spurious "## Tools available"
///   block with an empty list.
/// - Otherwise, trim trailing whitespace from `base_prompt`,
///   append a blank line, then a `## Tools available`
///   section: a one-line preamble discouraging invention,
///   followed by a bulleted list of every tool by exact
///   `name` and `description`.
///
/// **Decoupled from strategy selection.** This helper does
/// the formatting; deciding whether to call it is the
/// planner's job (Task 4) per the operator's per-family
/// [`OllamaFamilyStrategy`](aivyx_config::OllamaFamilyStrategy).
///
/// Allocates a fresh `String`. The allocation is dwarfed by
/// the per-turn LLM round-trip cost.
pub fn append_tool_catalog(
    base_prompt: &str,
    tools: &[LlmToolDescriptor],
    fs_root: Option<&std::path::Path>,
) -> String {
    if tools.is_empty() {
        return base_prompt.to_string();
    }
    let mut out = String::with_capacity(base_prompt.len() + 256);
    out.push_str(base_prompt.trim_end());
    out.push_str("\n\n## Tools available\n\n");
    out.push_str(
        "You can invoke these tools by their exact names listed below. \
         Do not invent or guess tool names; tools not on this list do \
         not exist.\n\n",
    );
    for tool in tools {
        let desc = tool.description.trim();
        if desc.is_empty() {
            out.push_str(&format!("- `{}`\n", tool.name));
        } else {
            out.push_str(&format!("- `{}` — {desc}\n", tool.name));
        }
    }

    // Filesystem tools are rooted at a sandbox boundary that the
    // operator chooses (Chapter N access levels). Tell the model the
    // ACTUAL root so it reasons correctly about what is in/out of
    // bounds: with a narrow root it must not present sandbox contents
    // as the operator's home; with a broad root (`$HOME`, `/`) it must
    // NOT over-refuse a request that is in fact inside its root. Gated
    // on an `fs.*` tool actually being registered.
    if tools.iter().any(|t| t.name.starts_with("fs.")) {
        match fs_root {
            Some(root) => {
                out.push_str(&format!(
                    "\n\nYour filesystem tools (the `fs.*` tools) are rooted \
                     at `{root}`. You CAN read, write, and list anything \
                     under `{root}` — including `{root}` itself (pass `.` or \
                     an absolute path under it). You CANNOT reach paths \
                     outside `{root}`; if the operator asks about one, say \
                     plainly it is outside your accessible root. Resolve a \
                     bare or relative path against `{root}`, and when a \
                     request clearly targets a location under `{root}`, \
                     invoke the tool rather than refusing.\n",
                    root = root.display(),
                ));
            }
            None => {
                out.push_str(
                    "\n\nYour filesystem tools (the `fs.*` tools) are \
                     SANDBOXED to a root directory and cannot reach paths \
                     outside it. When the operator asks about a path outside \
                     your sandbox, say so plainly rather than presenting your \
                     sandbox's contents as that location.\n",
                );
            }
        }
    }

    // Chapter O — the agent's own personal workspace. Distinct from the
    // `fs.*` tools above (the operator's files) and from `memory.*` (recall
    // facts): this is the agent's OWN space for free-form thinking. Path-
    // free — the `workspace.*` tools resolve paths relative to the root —
    // gated on a `workspace.*` tool being registered.
    if tools.iter().any(|t| t.name.starts_with("workspace.")) {
        out.push_str(
            "\n\nYou have your OWN personal workspace — the `workspace.*` \
             tools — separate from the operator's files (`fs.*`) and from \
             your memory (`memory.*`). It is yours: use it freely for your \
             own thoughts, ideas, plans, and multi-file projects. Jot a \
             thought or journal with `workspace.note`; draft and revise \
             longer work with `workspace.write` / `workspace.read` / \
             `workspace.list`. Suggested buckets: `journal/`, `ideas/`, \
             `plans/`, `projects/`. Paths are relative to your workspace \
             root. The operator can see this space, so keep it legible.\n",
        );
    }

    out.trim_end().to_string()
}

/// Phase 124 Task 2 — Append a `## Example tool use` block
/// after [`append_tool_catalog`].
///
/// **Why a separate helper than the catalog.** Phase 122
/// shipped catalog enumeration; gemma4:31b still refused
/// `fs.write` with the tool literally listed five lines
/// above. The Phase 122 exit doc framed this as the
/// model-layer ceiling. Phase 124 attempts one more
/// substrate move — few-shot examples are a different
/// mechanism than assertion-of-availability. The model sees
/// concrete worked patterns of "operator asks → assistant
/// invokes → result reported," with explicit WRONG/RIGHT
/// framing against the observed refusal pattern.
///
/// **Behavior:**
/// - If `tools` is empty, return `base_prompt.to_string()`
///   unchanged. No catalog → no examples.
/// - If NONE of the example-targeted tools (`fs.read`,
///   `fs.write`, `memory.write`) are in the registered set,
///   the helper still appends the block but only with
///   examples for tools that ARE registered. If zero are
///   registered (operator's role narrowly allow-listed),
///   the block is omitted entirely — anchoring on a tool
///   the model can't actually call would undermine the
///   point.
/// - Otherwise, trim trailing whitespace from `base_prompt`,
///   append a blank line, then a `## Example tool use`
///   section.
///
/// The function takes the SAME tool slice the catalog
/// helper does so caller-side filtering for role allowlists
/// flows through identically. Intended to be called
/// directly after `append_tool_catalog`:
///
/// ```text
/// let with_catalog = append_tool_catalog(&base, &tools);
/// let with_examples = append_few_shot_examples(&with_catalog, &tools);
/// ```
///
/// Allocates a fresh `String`.
pub fn append_few_shot_examples(
    base_prompt: &str,
    tools: &[LlmToolDescriptor],
) -> String {
    if tools.is_empty() {
        return base_prompt.to_string();
    }
    let has_fs_read = tools.iter().any(|t| t.name == "fs.read");
    let has_fs_write = tools.iter().any(|t| t.name == "fs.write");
    let has_memory_write = tools.iter().any(|t| t.name == "memory.write");
    if !has_fs_read && !has_fs_write && !has_memory_write {
        // None of the example-targeted tools are available;
        // skip the block rather than anchor on tools the
        // model can't actually call.
        return base_prompt.to_string();
    }
    let mut out = String::with_capacity(base_prompt.len() + 768);
    out.push_str(base_prompt.trim_end());
    out.push_str("\n\n## Example tool use\n\n");
    out.push_str(
        "The tools listed above are real and you have them. \
         When the operator asks you to use one, INVOKE it — \
         do not respond with prose claiming you don't have \
         it. Examples:\n\n",
    );

    if has_fs_write {
        out.push_str(
            "Operator: \"Please save 'hello' to test.txt.\"\n\
             - You should: invoke `fs.write` with \
             `{\"path\": \"test.txt\", \"content\": \"hello\"}` \
             and report the result.\n\
             - You should NOT: respond \"I don't have a tool \
             called fs.write\" — you DO have fs.write; it is \
             in your tool list above.\n\n",
        );
    }
    if has_memory_write {
        out.push_str(
            "Operator: \"Remember that I'm working on the \
             Aivyx project.\"\n\
             - You should: invoke `memory.write` with \
             `{\"topic\": \"current-projects\", \"content\": \
             \"Working on Aivyx project\"}` and confirm \
             succinctly.\n\
             - You should NOT: enumerate your memory tools in \
             prose; the operator knows your tools already.\n\n",
        );
    }
    if has_fs_read {
        out.push_str(
            "Operator: \"What's in README.md?\"\n\
             - You should: invoke `fs.read` with \
             `{\"path\": \"README.md\"}` and summarize what \
             you find.\n\
             - You should NOT: ask the operator to paste the \
             file contents; you can read it yourself with \
             `fs.read`.\n\n",
        );
    }
    out.push_str(
        "When asked what tools you have, name them concisely \
         from the list above — do not describe each at length \
         and do not invent tools that are not in the list.\n",
    );
    out.trim_end().to_string()
}

/// Phase 124 Task 3 — Apply an `OllamaFamilyStrategy` to a
/// base prompt + tool list, dispatching to the appropriate
/// helper(s).
///
/// **Strategy dispatch:**
/// - `None` → return `base_prompt.to_string()` unchanged.
///   No catalog block, no examples.
/// - `StructuredInjection` → `append_tool_catalog` only.
///   Phase 122 substrate.
/// - `FewShotExamples` → `append_tool_catalog` THEN
///   `append_few_shot_examples`. Phase 124 substrate.
///
/// This is the operator-facing dispatch helper. Per-call-site
/// strategy-aware logic lives here instead of being inlined
/// at every prompt-assembly site in the binary (initial
/// system_prompt + daemon refresher + in-process CLI
/// refresher + child agent's initial + child refresher = 5
/// sites; one dispatcher beats five copies).
///
/// `tools` is the same filtered catalog used by both
/// helpers; pass it once and let the dispatcher decide what
/// to render.
pub fn apply_ollama_prompt_strategy(
    base_prompt: &str,
    tools: &[LlmToolDescriptor],
    strategy: OllamaFamilyStrategy,
    fs_root: Option<&std::path::Path>,
) -> String {
    match strategy {
        OllamaFamilyStrategy::None => base_prompt.to_string(),
        OllamaFamilyStrategy::StructuredInjection => {
            append_tool_catalog(base_prompt, tools, fs_root)
        }
        OllamaFamilyStrategy::FewShotExamples => {
            let with_catalog = append_tool_catalog(base_prompt, tools, fs_root);
            append_few_shot_examples(&with_catalog, tools)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_config::{
        FieldSource, Sourced, DEFAULT_ASSISTANT_NAME, DEFAULT_SYSTEM_PROMPT,
    };

    fn default_profile() -> Profile {
        Profile::default()
    }

    fn operator_declared_profile() -> Profile {
        Profile {
            assistant_name: Sourced::new("Codex".to_string(), FieldSource::Toml),
            operator_profile: Some(
                "Senior Rust engineer focused on systems.".to_string(),
            ),
            communication_style: Some("terse, conclusion-first".to_string()),
            primary_use_cases: vec!["Rust systems programming".to_string()],
            behavioral_preferences: vec!["prefer integration tests".to_string()],
            behavioral_constraints: vec!["never auto-commit code".to_string()],
        }
    }

    #[test]
    fn default_profile_returns_role_prompt_unchanged() {
        let profile = default_profile();
        let role_prompt = "You are a coding assistant.";
        let assembled = assemble_session_prompt(&profile, None, "default", role_prompt);
        assert_eq!(assembled, role_prompt);
    }

    #[test]
    fn default_profile_with_empty_role_prompt_returns_empty_string() {
        let profile = default_profile();
        let assembled = assemble_session_prompt(&profile, None, "default", "");
        assert_eq!(assembled, "");
    }

    /// Chapter Keel — the operating charter is the base layer, so it
    /// must reach the model intact through both `assemble_session_prompt`
    /// paths: verbatim for a fresh agent (no Profile/Persona/Skills), and
    /// as the trailing `## Active role` block once any layer is active.
    /// The earlier tests prove this with arbitrary role strings; this one
    /// pins it for the *real* charter constant end to end.
    #[test]
    fn default_charter_reaches_the_model_through_both_paths() {
        // Fresh agent: charter flows verbatim, no wrapper.
        let fresh = assemble_session_prompt(
            &default_profile(),
            None,
            "default",
            DEFAULT_SYSTEM_PROMPT,
        );
        assert_eq!(fresh, DEFAULT_SYSTEM_PROMPT);

        // Operator-declared Profile active: charter is the last block,
        // verbatim, under the active-role label.
        let composed = assemble_session_prompt(
            &operator_declared_profile(),
            None,
            "default",
            DEFAULT_SYSTEM_PROMPT,
        );
        assert!(composed.starts_with("## About this assistant\n\n"));
        assert!(composed.contains("\n\n## Active role: default\n\n"));
        assert!(composed.ends_with(DEFAULT_SYSTEM_PROMPT));
        // The charter's safety prose survives the composition unmangled.
        assert!(composed.contains("cannot and will not widen"));
    }

    #[test]
    fn operator_declared_profile_produces_labeled_composition() {
        let profile = operator_declared_profile();
        let role_prompt = "You are a coding assistant.";
        let assembled = assemble_session_prompt(&profile, None, "coder", role_prompt);

        // Profile section appears first.
        assert!(assembled.starts_with("## About this assistant\n\n"));
        // Assistant name renders inside Profile section.
        assert!(assembled.contains("Your name is Codex."));
        // Operator profile renders.
        assert!(assembled.contains("About the operator: Senior Rust"));
        // Communication style renders.
        assert!(assembled.contains("Communication style: terse"));
        // Primary use cases render as bullets.
        assert!(assembled.contains("Primary use cases:\n- Rust systems programming"));
        // Behavioral preferences render as bullets.
        assert!(assembled.contains("Behavioral preferences:\n- prefer integration tests"));
        // Behavioral constraints render as bullets.
        assert!(assembled.contains("Behavioral constraints:\n- never auto-commit code"));
        // Active role section appears after Profile.
        assert!(assembled.contains("\n\n## Active role: coder\n\n"));
        // Role's system_prompt appears at the end.
        assert!(assembled.ends_with("You are a coding assistant."));
    }

    #[test]
    fn operator_declared_profile_with_empty_role_prompt_still_renders_active_role_label() {
        let profile = operator_declared_profile();
        let assembled = assemble_session_prompt(&profile, None, "coder", "");
        // The Active role label is still rendered even when the
        // role's system_prompt is empty — gives the LLM structural
        // cue that the role itself has no per-role voice override.
        assert!(assembled.contains("## Active role: coder"));
    }

    #[test]
    fn profile_with_only_assistant_name_overridden_still_renders() {
        // Operator overrides assistant_name but leaves everything
        // else empty. is_operator_declared() must return true
        // because assistant_name's source is now Toml.
        let profile = Profile {
            assistant_name: Sourced::new("Mira".to_string(), FieldSource::Toml),
            ..Profile::default()
        };

        assert!(profile.is_operator_declared());

        let assembled = assemble_session_prompt(&profile, None, "default", "You are helpful.");
        assert!(assembled.contains("Your name is Mira."));
        // None of the optional sections render.
        assert!(!assembled.contains("About the operator"));
        assert!(!assembled.contains("Communication style"));
        assert!(!assembled.contains("Primary use cases"));
        assert!(!assembled.contains("Behavioral preferences"));
        assert!(!assembled.contains("Behavioral constraints"));
        // Role's system_prompt still appears.
        assert!(assembled.ends_with("You are helpful."));
    }

    #[test]
    fn same_profile_yields_same_profile_section_across_roles() {
        // Phase 57 Task 3 property: a role-switch child session
        // sees the same Profile section as its parent. The
        // "## Active role: <name>" label differs, but everything
        // above that line is identical. Validates the structural
        // invariant the binary's parent path and role-switch
        // factory both rely on.
        let profile = operator_declared_profile();
        let parent = assemble_session_prompt(&profile, None, "default", "Parent prompt.");
        let child = assemble_session_prompt(&profile, None, "junior_researcher", "Child prompt.");

        // The Profile section (everything before "## Active role")
        // must be byte-identical across the two assemblies.
        let parent_profile = parent.split("\n\n## Active role:").next().unwrap();
        let child_profile = child.split("\n\n## Active role:").next().unwrap();
        assert_eq!(parent_profile, child_profile);

        // Active-role labels differ.
        assert!(parent.contains("## Active role: default"));
        assert!(child.contains("## Active role: junior_researcher"));

        // Each section ends with its own role prompt.
        assert!(parent.ends_with("Parent prompt."));
        assert!(child.ends_with("Child prompt."));
    }

    #[test]
    fn default_assistant_name_is_aivyx() {
        // Sanity guard for DEFAULT_ASSISTANT_NAME wiring — the
        // assemble helper relies on the synthesized default name
        // for is_operator_declared() short-circuiting.
        let p = Profile::default();
        assert_eq!(p.assistant_name.value, DEFAULT_ASSISTANT_NAME);
        assert_eq!(p.assistant_name.source, FieldSource::Default);
        assert!(!p.is_operator_declared());
    }

    // -------------------------------------------------------------
    // Phase 59 Task 6 — Persona section integration.
    // -------------------------------------------------------------

    fn operator_declared_persona() -> EffectivePersona {
        EffectivePersona {
            assistant_name: None,
            operator_profile: None,
            communication_style: Some(
                "refined: terse, no preamble, ASCII-only".to_string(),
            ),
            primary_use_cases: vec![],
            behavioral_preferences: vec!["always cite sources".to_string()],
            behavioral_constraints: vec![],
            learned_context: vec!["operator uses Vim".to_string()],
            communication_adaptations: vec![
                "operator prefers conclusion-first paragraphs".to_string(),
            ],
            character_traits: vec![],
            relationship_milestones: vec![],
            learned_skills: Vec::new(),
            profile_hints: Vec::new(),
            role_drafts: Vec::new(),
        }
    }

    #[test]
    fn empty_persona_with_default_profile_returns_role_prompt_unchanged() {
        let profile = default_profile();
        let persona = EffectivePersona::default();
        let assembled = assemble_session_prompt(
            &profile,
            Some(&persona),
            "default",
            "You are helpful.",
        );
        assert_eq!(assembled, "You are helpful.");
    }

    #[test]
    fn skills_section_carries_the_teaching_protocol() {
        // Phase 184 — when the operator has any skill, the prompt
        // reinforces the draft-show-confirm protocol.
        let mut persona = EffectivePersona::default();
        persona.learned_skills.push(
            crate::persona::LearnedSkill {
                name: "greet".into(),
                trigger: "on hello".into(),
                procedure: "say hi".into(),
                ..Default::default()
            }
            .to_json_value(),
        );
        let out = render_skills_section(&persona);
        assert!(out.contains("## Learned skills"));
        assert!(out.contains("- greet: on hello"));
        // The teaching protocol + the three edit tools are named.
        assert!(out.contains("confirmed: true"));
        assert!(out.contains("skills.teach"));
        assert!(out.contains("skills.update"));
        assert!(out.contains("skills.forget"));
    }

    #[test]
    fn persona_section_renders_under_labeled_header() {
        let profile = default_profile();
        let persona = operator_declared_persona();
        let assembled = assemble_session_prompt(
            &profile,
            Some(&persona),
            "coder",
            "You are a coding assistant.",
        );

        // Persona section appears (no Profile section since profile is default).
        assert!(assembled.starts_with("## How I have learned to communicate"));
        // Refined scalars render as "Refined ..." lines.
        assert!(assembled.contains("Refined communication style: refined: terse"));
        // List entries render as bullets under category-specific headers.
        assert!(assembled.contains("Learned behavioral preferences:\n- always cite sources"));
        assert!(assembled.contains("Learned context:\n- operator uses Vim"));
        assert!(assembled.contains("Communication adaptations:\n- operator prefers"));
        // Active role section follows.
        assert!(assembled.contains("\n\n## Active role: coder\n\n"));
        assert!(assembled.ends_with("You are a coding assistant."));
    }

    #[test]
    fn profile_and_persona_compose_three_section_layout() {
        // Profile present + Persona present + role prompt → all
        // three sections render in the Q6(a) order.
        let profile = operator_declared_profile();
        let persona = operator_declared_persona();
        let assembled = assemble_session_prompt(
            &profile,
            Some(&persona),
            "coder",
            "You are a coding assistant.",
        );

        // Profile section first.
        assert!(assembled.starts_with("## About this assistant"));
        // Persona section second.
        let profile_end = assembled.find("## How I have learned to communicate").unwrap();
        let role_start = assembled.find("## Active role: coder").unwrap();
        assert!(profile_end < role_start);
        // Role section last.
        assert!(assembled.ends_with("You are a coding assistant."));
    }

    #[test]
    fn persona_none_is_equivalent_to_empty_persona_when_profile_empty() {
        // Passing None for persona must behave identically to
        // passing Some(&EffectivePersona::default()) — both mean
        // "no Persona content, render passthrough."
        let profile = default_profile();
        let with_none = assemble_session_prompt(&profile, None, "default", "hi");
        let with_empty = assemble_session_prompt(
            &profile,
            Some(&EffectivePersona::default()),
            "default",
            "hi",
        );
        assert_eq!(with_none, with_empty);
        assert_eq!(with_none, "hi");
    }

    // ---- Phase 79 — reduced-Persona assembly + invariant -------

    fn rich_persona() -> EffectivePersona {
        EffectivePersona {
            assistant_name: Some("Ada".to_string()),
            operator_profile: Some("staff SRE".to_string()),
            communication_style: Some("terse".to_string()),
            primary_use_cases: vec!["oncall".to_string()],
            behavioral_preferences: vec!["cite sources".to_string()],
            behavioral_constraints: vec![
                "never run destructive cmds unprompted".to_string(),
            ],
            learned_context: vec![
                "operator uses Vim".to_string(),
                "deploys on Fridays".to_string(),
            ],
            communication_adaptations: vec![
                "conclusion-first".to_string(),
            ],
            character_traits: vec!["dry wit".to_string()],
            relationship_milestones: vec!["shipped v1".to_string()],
            learned_skills: Vec::new(),
            profile_hints: Vec::new(),
            role_drafts: Vec::new(),
        }
    }

    #[test]
    fn reduce_persona_keeps_protected_fields_even_when_keep_rejects_all()
    {
        let full = rich_persona();
        // keep = reject everything.
        let r = reduce_persona(&full, &|_| false);

        // Invariant: scalars + constraints copied in full.
        assert_eq!(r.assistant_name.as_deref(), Some("Ada"));
        assert_eq!(r.operator_profile.as_deref(), Some("staff SRE"));
        assert_eq!(r.communication_style.as_deref(), Some("terse"));
        assert_eq!(
            r.behavioral_constraints,
            vec!["never run destructive cmds unprompted".to_string()]
        );
        // Every soft list emptied.
        assert!(r.primary_use_cases.is_empty());
        assert!(r.behavioral_preferences.is_empty());
        assert!(r.learned_context.is_empty());
        assert!(r.communication_adaptations.is_empty());
        assert!(r.character_traits.is_empty());
        assert!(r.relationship_milestones.is_empty());
    }

    #[test]
    fn reduce_persona_keeps_only_selected_soft_entries() {
        let full = rich_persona();
        let r = reduce_persona(&full, &|s| s == "deploys on Fridays");
        assert_eq!(
            r.learned_context,
            vec!["deploys on Fridays".to_string()]
        );
        // Other soft categories lose their (non-matching) entries.
        assert!(r.character_traits.is_empty());
        // Protected still intact.
        assert_eq!(r.assistant_name.as_deref(), Some("Ada"));
        assert_eq!(r.behavioral_constraints.len(), 1);
    }

    #[test]
    fn reducible_facet_count_excludes_protected() {
        // rich_persona soft entries: 1+1+2+1+1+1 = 7.
        // behavioral_constraints (1) + scalars must NOT count.
        assert_eq!(reducible_facet_count(&rich_persona()), 7);
        assert_eq!(
            reducible_facet_count(&EffectivePersona::default()),
            0
        );
    }

    #[test]
    fn selected_assembly_matches_reduce_then_assemble_and_keeps_constraint(
    ) {
        let profile = operator_declared_profile();
        let full = rich_persona();
        let keep = |s: &str| s == "oncall";

        let via_wrapper = assemble_session_prompt_selected(
            &profile, &full, &keep, "default", "role prompt", None,
        );
        let manual = assemble_session_prompt(
            &profile,
            Some(&reduce_persona(&full, &keep)),
            "default",
            "role prompt",
        );
        assert_eq!(via_wrapper, manual);

        // End-to-end invariant: a behavioral constraint the
        // selector rejected is STILL in the rendered prompt.
        assert!(via_wrapper
            .contains("never run destructive cmds unprompted"));
        // And the selected soft facet is present...
        assert!(via_wrapper.contains("oncall"));
        // ...while a rejected soft facet is gone.
        assert!(!via_wrapper.contains("dry wit"));
    }

    // ----- Phase 116 — relevance section integration -----

    #[test]
    fn assemble_with_none_relevance_matches_phase_114_output() {
        // Backward-compatibility: passing None for the relevance
        // section must produce byte-identical output to the
        // existing assemble_session_prompt path.
        let profile = Profile::default();
        let without =
            assemble_session_prompt(&profile, None, "default", "role-instructions");
        let with_none = assemble_session_prompt_with_relevance(
            &profile,
            None,
            "default",
            "role-instructions",
            None,
            None,
        );
        assert_eq!(without, with_none);
    }

    #[test]
    fn assemble_with_relevance_slots_section_before_active_role() {
        let profile = Profile::default();
        let section = "## Tools recently used for similar tasks\n\
                       \nBased on keywords: code, rust\n\
                       \nTools:\n- memory.read: 3 successes, 0 failures\n";
        let out = assemble_session_prompt_with_relevance(
            &profile,
            None,
            "researcher",
            "Be thorough.",
            Some(section),
            None,
        );
        let relevance_pos = out
            .find("## Tools recently used for similar tasks")
            .expect("relevance section present");
        let role_pos = out
            .find("## Active role: researcher")
            .expect("active role marker present");
        assert!(
            relevance_pos < role_pos,
            "relevance section must come before active role: {out}"
        );
        assert!(out.contains("memory.read: 3 successes"));
    }

    #[test]
    fn assemble_with_empty_relevance_string_falls_back_to_no_section() {
        // An empty (or whitespace-only) Some(_) is treated as
        // "no signal" — the section is suppressed.
        let profile = Profile::default();
        let out = assemble_session_prompt_with_relevance(
            &profile,
            None,
            "default",
            "role",
            Some(""),
            None,
        );
        assert!(!out.contains("Tools recently used"));

        let out_ws = assemble_session_prompt_with_relevance(
            &profile,
            None,
            "default",
            "role",
            Some("   \n  "),
            None,
        );
        assert!(!out_ws.contains("Tools recently used"));
    }

    #[test]
    fn assemble_with_relevance_only_still_returns_section() {
        // Profile + Persona empty but relevance section present:
        // the prompt should include the relevance section + role.
        let profile = Profile::default();
        let section = "## Tools recently used for similar tasks\n\
                       Tools:\n- memory.read: 1 successes, 0 failures\n";
        let out = assemble_session_prompt_with_relevance(
            &profile,
            None,
            "default",
            "role",
            Some(section),
            None,
        );
        assert!(out.contains("Tools recently used"));
        assert!(out.contains("## Active role: default"));
    }

    #[test]
    fn assemble_with_default_skills_section_slots_before_active_role() {
        let profile = Profile::default();
        let section = "## Default skills\n\n- systematic-debugging: ...\n";
        let out = assemble_session_prompt_with_relevance(
            &profile,
            None,
            "researcher",
            "Be thorough.",
            None,
            Some(section),
        );
        let skills_pos = out
            .find("## Default skills")
            .expect("default skills section present");
        let role_pos = out
            .find("## Active role: researcher")
            .expect("active role marker present");
        assert!(
            skills_pos < role_pos,
            "default skills section must come before active role: {out}"
        );
        assert!(out.contains("systematic-debugging"));
    }

    #[test]
    fn assemble_with_default_skills_and_relevance_both_present_orders_default_skills_first() {
        let profile = Profile::default();
        let relevance = "## Tools recently used for similar tasks\n\nTools:\n- memory.read\n";
        let skills = "## Default skills\n\n- systematic-debugging: ...\n";
        let out = assemble_session_prompt_with_relevance(
            &profile,
            None,
            "default",
            "role",
            Some(relevance),
            Some(skills),
        );
        let relevance_pos = out.find("## Tools recently used").unwrap();
        let skills_pos = out.find("## Default skills").unwrap();
        assert!(
            skills_pos < relevance_pos,
            "default skills section must come before relevance: {out}"
        );
    }

    #[test]
    fn assemble_with_empty_default_skills_string_falls_back_to_no_section() {
        let profile = Profile::default();
        let out = assemble_session_prompt_with_relevance(
            &profile, None, "default", "role", None, Some(""),
        );
        assert!(!out.contains("Default skills"));

        let out_ws = assemble_session_prompt_with_relevance(
            &profile, None, "default", "role", None, Some("   \n  "),
        );
        assert!(!out_ws.contains("Default skills"));
    }

    // ----- Phase 122 Task 3 — append_tool_catalog -----

    fn tool(name: &str, description: &str) -> LlmToolDescriptor {
        LlmToolDescriptor {
            name: name.to_string(),
            description: description.to_string(),
            input_schema: serde_json::json!({"type": "object"}),
        }
    }

    #[test]
    fn phase_122_append_tool_catalog_empty_tools_is_noop() {
        let base = "## Active role: default\n\nYou are helpful.";
        let out = append_tool_catalog(base, &[], None);
        assert_eq!(out, base);
    }

    #[test]
    fn phase_122_append_tool_catalog_adds_section_header() {
        let base = "## Active role: default\n\nYou are helpful.";
        let tools = vec![tool("fs.read", "Read a file")];
        let out = append_tool_catalog(base, &tools, None);
        assert!(out.contains("## Tools available"));
        assert!(out.starts_with(base));
    }

    #[test]
    fn phase_122_append_tool_catalog_lists_every_tool_by_exact_name() {
        let base = "role";
        let tools = vec![
            tool("fs.read", "Read a file"),
            tool("fs.write", "Write a file"),
            tool("memory.read", "Read a memory entry"),
        ];
        let out = append_tool_catalog(base, &tools, None);
        // Each tool name appears literally in the output.
        assert!(out.contains("`fs.read`"));
        assert!(out.contains("`fs.write`"));
        assert!(out.contains("`memory.read`"));
        // Descriptions accompany each name.
        assert!(out.contains("Read a file"));
        assert!(out.contains("Write a file"));
        assert!(out.contains("Read a memory entry"));
    }

    #[test]
    fn phase_122_append_tool_catalog_warns_against_invention() {
        // The diagnostic data motivating Phase 122 was tool-name
        // confabulation (qwen3.6 → "Good Morning"; gemma4 → 60+
        // invented tools). The preamble must explicitly discourage
        // invention; otherwise the catalog block is just a longer
        // hallucination prompt.
        let base = "role";
        let tools = vec![tool("fs.read", "Read a file")];
        let out = append_tool_catalog(base, &tools, None);
        let lower = out.to_lowercase();
        assert!(
            lower.contains("do not invent") || lower.contains("do not guess"),
            "expected anti-invention preamble; got: {out}"
        );
    }

    #[test]
    fn append_tool_catalog_names_the_actual_fs_root() {
        // Chapter N — when a root is known, the note must NAME it so the
        // model reasons correctly: with a narrow root it won't mislabel
        // its sandbox; with a broad root (home/full) it won't over-refuse
        // a path that is in fact inside its root.
        let tools = vec![tool("fs.metadata", "Inspect a file or directory")];
        let out = append_tool_catalog(
            "role",
            &tools,
            Some(std::path::Path::new("/home/julian")),
        );
        assert!(out.contains("/home/julian"), "names the real root: {out}");
        assert!(
            out.to_lowercase().contains("invoke the tool rather than refusing"),
            "tells the model to act on in-root requests: {out}",
        );
    }

    #[test]
    fn append_tool_catalog_generic_note_when_root_unknown() {
        // With no root supplied, fall back to the generic sandbox note.
        let tools = vec![tool("fs.metadata", "Inspect a file or directory")];
        let out = append_tool_catalog("role", &tools, None).to_lowercase();
        assert!(out.contains("sandbox"), "expected a sandbox note; got: {out}");
    }

    #[test]
    fn append_tool_catalog_omits_sandbox_note_without_fs_tools() {
        // A non-filesystem agent (only memory tools) shouldn't carry
        // filesystem-sandbox guidance, even with a root supplied.
        let tools = vec![tool("memory.write", "Store a memory")];
        let out = append_tool_catalog(
            "role",
            &tools,
            Some(std::path::Path::new("/home/julian")),
        )
        .to_lowercase();
        assert!(!out.contains("sandbox"), "no fs tools → no sandbox note: {out}");
        assert!(!out.contains("/home/julian"), "no fs tools → no root note: {out}");
    }

    #[test]
    fn append_tool_catalog_adds_workspace_note_when_workspace_tools_present() {
        // Chapter O — the agent is told it has its OWN workspace, distinct
        // from fs.* and memory.*.
        let tools = vec![tool("workspace.note", "Append a journal entry")];
        let out = append_tool_catalog("role", &tools, None).to_lowercase();
        assert!(out.contains("own personal workspace"), "got: {out}");
        assert!(out.contains("workspace.note"));
    }

    #[test]
    fn append_tool_catalog_omits_workspace_note_without_workspace_tools() {
        let tools = vec![tool("memory.write", "Store a memory")];
        let out = append_tool_catalog("role", &tools, None).to_lowercase();
        assert!(!out.contains("personal workspace"), "got: {out}");
    }

    #[test]
    fn phase_122_append_tool_catalog_tool_with_empty_description_omits_dash() {
        // Defensive — tools registered without a description
        // shouldn't render as `- \`name\` — ` with a trailing
        // em-dash and empty body.
        let base = "role";
        let tools = vec![tool("fs.read", "")];
        let out = append_tool_catalog(base, &tools, None);
        assert!(out.contains("- `fs.read`\n") || out.ends_with("- `fs.read`"));
        assert!(!out.contains("- `fs.read` — "));
    }

    #[test]
    fn phase_122_append_tool_catalog_preserves_base_prompt_content() {
        // Don't drop any of the base prompt — operators rely on
        // the assembled Profile/Persona/role layering being
        // intact end-to-end.
        let base = "## About this assistant\n\nYour name is Aivyx.\n\n## Active role: default\n\nbody";
        let tools = vec![tool("fs.read", "Read a file")];
        let out = append_tool_catalog(base, &tools, None);
        assert!(out.contains("Your name is Aivyx."));
        assert!(out.contains("## Active role: default"));
        assert!(out.contains("body"));
    }

    #[test]
    fn phase_122_append_tool_catalog_composes_with_assemble_session_prompt() {
        // Task-4 composition shape: planner first calls
        // `assemble_session_prompt`, then wraps with
        // `append_tool_catalog` when strategy is
        // StructuredInjection. This test pins the composed
        // output shape end-to-end so a future refactor of
        // either helper doesn't silently drop the catalog
        // block.
        let profile = operator_declared_profile();
        let assembled = assemble_session_prompt(
            &profile,
            None,
            "default",
            "You are helpful.",
        );
        let tools = vec![tool("fs.read", "Read a file")];
        let composed = append_tool_catalog(&assembled, &tools, None);
        // Profile section preserved.
        assert!(composed.contains("## About this assistant"));
        // Active role section preserved.
        assert!(composed.contains("## Active role: default"));
        assert!(composed.contains("You are helpful."));
        // Tools-available block landed at the end (after the
        // role section, not before).
        let role_idx = composed.find("## Active role:").unwrap();
        let tools_idx = composed.find("## Tools available").unwrap();
        assert!(
            tools_idx > role_idx,
            "tool catalog must follow the role section so the model \
             sees the catalog as the most-recent system context; got: \
             role_idx={role_idx}, tools_idx={tools_idx}"
        );
    }

    #[test]
    fn phase_122_append_tool_catalog_trims_base_prompt_trailing_whitespace() {
        // Two newlines max between base and the appended
        // section, regardless of how many trailing newlines the
        // base prompt has.
        let base = "role\n\n\n\n";
        let tools = vec![tool("fs.read", "Read a file")];
        let out = append_tool_catalog(base, &tools, None);
        assert!(out.contains("role\n\n## Tools available"));
        assert!(!out.contains("role\n\n\n## Tools available"));
    }

    // ----- Phase 124 Task 2 — append_few_shot_examples -----

    #[test]
    fn phase_124_few_shot_empty_tools_is_noop() {
        let base = "role";
        let out = append_few_shot_examples(base, &[]);
        assert_eq!(out, base);
    }

    #[test]
    fn phase_124_few_shot_with_no_example_targets_is_noop() {
        // None of fs.read / fs.write / memory.write registered —
        // skip the block rather than anchor on a tool the model
        // can't actually call.
        let base = "role";
        let tools = vec![tool("net.dns", "Resolve a hostname")];
        let out = append_few_shot_examples(base, &tools);
        assert_eq!(out, base);
    }

    #[test]
    fn phase_124_few_shot_includes_example_when_fs_write_registered() {
        let base = "role";
        let tools = vec![tool("fs.write", "Write a file")];
        let out = append_few_shot_examples(base, &tools);
        assert!(out.contains("## Example tool use"));
        assert!(out.contains("`fs.write`"));
        // The load-bearing WRONG/RIGHT framing — directly
        // counters gemma4's observed refusal pattern.
        assert!(out.contains("You should NOT"), "{out}");
        assert!(
            out.contains("I don't have"),
            "the WRONG line must literally name the refusal phrase; got: {out}"
        );
    }

    #[test]
    fn phase_124_few_shot_includes_only_examples_for_registered_tools() {
        // Only fs.read registered; the fs.write + memory.write
        // examples should NOT appear (they'd point at unregistered
        // tools).
        let base = "role";
        let tools = vec![tool("fs.read", "Read a file")];
        let out = append_few_shot_examples(base, &tools);
        assert!(out.contains("`fs.read`"));
        assert!(!out.contains("`fs.write`"));
        assert!(!out.contains("`memory.write`"));
    }

    #[test]
    fn phase_124_few_shot_includes_all_three_when_all_registered() {
        let base = "role";
        let tools = vec![
            tool("fs.read", "Read a file"),
            tool("fs.write", "Write a file"),
            tool("memory.write", "Write a memory entry"),
        ];
        let out = append_few_shot_examples(base, &tools);
        assert!(out.contains("`fs.read`"));
        assert!(out.contains("`fs.write`"));
        assert!(out.contains("`memory.write`"));
    }

    #[test]
    fn phase_124_few_shot_preamble_asserts_tools_are_real() {
        // The preamble is the load-bearing operator-facing
        // assertion that the catalog is authoritative.
        let base = "role";
        let tools = vec![tool("fs.write", "Write a file")];
        let out = append_few_shot_examples(base, &tools);
        assert!(out.contains("are real"), "{out}");
        assert!(out.contains("INVOKE"), "{out}");
    }

    #[test]
    fn phase_124_few_shot_trims_base_prompt_trailing_whitespace() {
        let base = "role\n\n\n\n";
        let tools = vec![tool("fs.write", "Write a file")];
        let out = append_few_shot_examples(base, &tools);
        assert!(out.contains("role\n\n## Example tool use"));
        assert!(!out.contains("role\n\n\n## Example tool use"));
    }

    #[test]
    fn phase_124_few_shot_composes_after_catalog_block() {
        // End-to-end shape: assemble_session_prompt →
        // append_tool_catalog → append_few_shot_examples.
        // The Example block must land AFTER the Tools-available
        // block; tests pin the relative ordering.
        let profile = operator_declared_profile();
        let assembled = assemble_session_prompt(
            &profile,
            None,
            "default",
            "You are helpful.",
        );
        let tools = vec![tool("fs.write", "Write a file")];
        let with_catalog = append_tool_catalog(&assembled, &tools, None);
        let composed = append_few_shot_examples(&with_catalog, &tools);
        let catalog_idx = composed.find("## Tools available").unwrap();
        let example_idx = composed.find("## Example tool use").unwrap();
        assert!(
            example_idx > catalog_idx,
            "examples must follow catalog so the model sees them \
             in last-most-recent position; got catalog_idx={catalog_idx}, \
             example_idx={example_idx}"
        );
        // Profile + role sections still present.
        assert!(composed.contains("## About this assistant"));
        assert!(composed.contains("## Active role: default"));
    }

    #[test]
    fn phase_124_few_shot_when_catalog_was_noop_still_noop() {
        // If `append_tool_catalog` was called with no tools and
        // returned the base unchanged, calling
        // `append_few_shot_examples` with the same (empty) slice
        // is also a no-op — chained no-ops compose to no-op.
        let base = "role";
        let after_catalog = append_tool_catalog(base, &[], None);
        let after_examples = append_few_shot_examples(&after_catalog, &[]);
        assert_eq!(after_examples, base);
    }
}
