//! Chapter X — LLM-assisted persona-seed drafting.
//!
//! The operator describes, in their own words, what they want their assistant
//! to be like; the configured model drafts a starting **Persona** seed (the
//! *learned* categories + one optional starter skill), which the operator then
//! edits and confirms before anything is planted. Shared by the daemon's
//! `DraftPersonaSeed` IPC handler (the Studio) and the `aivyx-pa init` wizard (the
//! CLI), so there is one drafting implementation.
//!
//! **Local-first / operator-authored:** the draft is enrichment, never
//! required. Any LLM error returns `None` and the caller falls back to manual
//! entry; the parser degrades missing fields gracefully; the operator reviews
//! every field — they are always the author of record. Only voice-layer
//! Persona facets are drafted (never capability grants).

use std::sync::Arc;

use aivyx_core::CancellationToken;
use aivyx_config::{PersonaSeed, SeedSkill};
use aivyx_llm::{LlmMessage, LlmProvider, LlmRequest, LlmStepEnd};

const DRAFT_MAX_TOKENS: u32 = 768;

/// Format-pinned system prompt so the parser has a deterministic shape. The
/// model drafts voice-layer Persona facets only — these are *learned*-category
/// seeds (character, adaptations, context) + an optional procedural skill,
/// never the operator-declared Profile and never capability/authority.
const DRAFT_SYSTEM_PROMPT: &str =
    "You are helping a person give their personal AI assistant a starting \
personality. From how they describe it, draft a few voice-layer traits the \
assistant can begin with (it will keep learning from use). Output EXACTLY \
these labels, one per line, nothing else — no preamble, no commentary:\n\
CHARACTER_TRAITS: <2-4 comma-separated voice properties, e.g. pragmatic, warm>\n\
COMMUNICATION_ADAPTATIONS: <comma-separated refinements to how it talks, or 'none'>\n\
LEARNED_CONTEXT: <one short sentence about the operator or their work, or 'none'>\n\
SKILL_NAME: <a kebab-case name for one starter skill, or 'none'>\n\
SKILL_TRIGGER: <when that skill applies, or 'none'>\n\
SKILL_PROCEDURE: <what that skill should do, or 'none'>";

/// Compose the user message from the operator's free-text description. Public
/// for prompt-shape unit tests.
pub fn compose_seed_prompt(description: &str) -> String {
    let d = description.trim();
    let d = if d.is_empty() { "(not specified)" } else { d };
    format!(
        "Here is how the operator describes the assistant they want:\n\n{d}\n\n\
         Draft the starting personality now.",
    )
}

/// Parse the model's labeled response into a [`PersonaSeed`]. Tolerant:
/// case-insensitive labels, ignores non-label lines, splits lists on commas /
/// semicolons, treats empty / "none" / "-" as undeclared. A skill is included
/// only when `SKILL_NAME` is a real value. `relationship_milestones` is never
/// drafted (the caller stamps the genesis milestone itself). Public for tests.
pub fn parse_seed_draft(text: &str) -> PersonaSeed {
    let mut character_traits = Vec::new();
    let mut communication_adaptations = Vec::new();
    let mut learned_context = Vec::new();
    let mut skill_name: Option<String> = None;
    let mut skill_trigger = String::new();
    let mut skill_procedure = String::new();

    for line in text.lines() {
        let Some((label, val)) = line.split_once(':') else {
            continue;
        };
        match label.trim().to_ascii_uppercase().as_str() {
            "CHARACTER_TRAITS" => character_traits = list(val),
            "COMMUNICATION_ADAPTATIONS" => communication_adaptations = list(val),
            "LEARNED_CONTEXT" => {
                if let Some(s) = scalar(val) {
                    learned_context = vec![s];
                }
            }
            "SKILL_NAME" => skill_name = scalar(val),
            "SKILL_TRIGGER" => skill_trigger = scalar(val).unwrap_or_default(),
            "SKILL_PROCEDURE" => skill_procedure = scalar(val).unwrap_or_default(),
            _ => {}
        }
    }

    let skills = match skill_name {
        Some(name) => vec![SeedSkill {
            name,
            trigger: skill_trigger,
            procedure: skill_procedure,
        }],
        None => Vec::new(),
    };

    PersonaSeed {
        learned_context,
        communication_adaptations,
        character_traits,
        relationship_milestones: Vec::new(),
        skills,
    }
}

/// Draft a persona seed from the operator's `description` via `provider`.
/// Returns `None` on any LLM error (the caller falls back to manual entry) or
/// when the model returned nothing usable (an all-empty draft).
pub async fn draft_persona_seed(
    provider: &Arc<dyn LlmProvider>,
    model: &str,
    description: &str,
) -> Option<PersonaSeed> {
    let user = compose_seed_prompt(description);
    let messages = vec![LlmMessage::user_text(user)];
    let request = LlmRequest {
        model,
        system: Some(DRAFT_SYSTEM_PROMPT),
        messages: &messages,
        tools: &[],
        max_tokens: DRAFT_MAX_TOKENS,
        temperature: Some(0.5),
    id_slot: None,
    slot_hint: None,
    route: None,
    };
    let cancel = CancellationToken::new();
    let mut stream = provider.chat_stream(request, &cancel).await.ok()?;
    while let Ok(Some(_)) = stream.next_event().await {}
    let step = stream.finish().await.ok()?;
    let text = match step {
        LlmStepEnd::FinalMessage { text, .. } => text,
        LlmStepEnd::ToolCalls { .. } => return None,
    };
    let drafted = parse_seed_draft(&text);
    if drafted == PersonaSeed::default() {
        None
    } else {
        Some(drafted)
    }
}

fn is_nullish(s: &str) -> bool {
    s.is_empty()
        || s == "-"
        || s.eq_ignore_ascii_case("none")
        || s.eq_ignore_ascii_case("n/a")
}

fn scalar(v: &str) -> Option<String> {
    let v = v.trim().trim_matches('"').trim();
    if is_nullish(v) {
        None
    } else {
        Some(v.to_string())
    }
}

fn list(v: &str) -> Vec<String> {
    v.split([',', ';'])
        .map(|s| s.trim().trim_matches('"').trim().to_string())
        .filter(|s| !is_nullish(s))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_clean_draft_with_skill() {
        let text = "CHARACTER_TRAITS: pragmatic, precise, warm\n\
            COMMUNICATION_ADAPTATIONS: leads with code\n\
            LEARNED_CONTEXT: the operator builds a Rust agent platform\n\
            SKILL_NAME: rust-review\n\
            SKILL_TRIGGER: when reviewing Rust for safety\n\
            SKILL_PROCEDURE: check unwraps and lifetimes; cite file:line";
        let s = parse_seed_draft(text);
        assert_eq!(s.character_traits, vec!["pragmatic", "precise", "warm"]);
        assert_eq!(s.communication_adaptations, vec!["leads with code"]);
        assert_eq!(s.learned_context, vec!["the operator builds a Rust agent platform"]);
        assert_eq!(s.skills.len(), 1);
        assert_eq!(s.skills[0].name, "rust-review");
        assert_eq!(s.skills[0].trigger, "when reviewing Rust for safety");
        // Milestones are never drafted.
        assert!(s.relationship_milestones.is_empty());
    }

    #[test]
    fn tolerates_preamble_casing_and_none_skill() {
        let text = "Sure! Here you go:\n\
            character_traits: Curious; Direct\n\
            COMMUNICATION_ADAPTATIONS: none\n\
            Learned_Context: -\n\
            SKILL_NAME: none\n";
        let s = parse_seed_draft(text);
        assert_eq!(s.character_traits, vec!["Curious", "Direct"]);
        assert!(s.communication_adaptations.is_empty());
        assert!(s.learned_context.is_empty());
        assert!(s.skills.is_empty(), "a 'none' skill name yields no skill");
    }

    #[test]
    fn all_empty_draft_equals_default() {
        let s = parse_seed_draft("nothing useful here\nno labels");
        assert_eq!(s, PersonaSeed::default());
    }

    #[test]
    fn compose_prompt_handles_blank_description() {
        assert!(compose_seed_prompt("   ").contains("(not specified)"));
        assert!(compose_seed_prompt("witty and terse").contains("witty and terse"));
    }
}
