//! LLM-assisted **Profile** draft for guided agent creation.
//!
//! Phase 181 introduced this as the CLI wizard's identity
//! builder. Chapter Genesis (GE.1) lifted it out of the
//! `aivyx-cli` binary into `aivyx-channel` — next to
//! [`crate::persona_seed_draft`] — so the **same drafter** backs
//! both the `aivyx-pa init` CLI wizard and the daemon's
//! `DraftProfile` IPC (the Studio onboarding flow, GE.2/GE.3).
//! The two drafters are complementary: this one drafts the
//! operator-**declared** P13 Profile (six fields); the persona
//! seed drafts the **learned** voice-layer facets.
//!
//! The operator answers a short relationship conversation (what
//! they want the assistant to be, the role it plays, how it
//! should talk, what it must never do). This module turns those
//! answers into a drafted six-field P13 Profile via the LLM the
//! wizard already verified — then a tolerant parser maps the
//! response back into structured fields.
//!
//! **Local-first:** the draft is enrichment, never required. Any
//! LLM error returns `None` and the caller falls back to the
//! guided manual prompts. The parser degrades missing fields
//! gracefully (a field the model omits stays `None` / empty), and
//! the operator reviews/edits every field afterward — they are
//! always the author of record.

use std::sync::Arc;

use aivyx_core::CancellationToken;
use aivyx_llm::{LlmMessage, LlmProvider, LlmRequest, LlmStepEnd};

/// The operator's answers to the relationship conversation.
#[derive(Debug, Clone, Default)]
pub struct IdentityAnswers {
    /// Free-form: "what do you want this assistant to be for you?"
    pub intent: String,
    /// The role it should play (collaborator / coach / confidant
    /// / assistant / their own words).
    pub role: String,
    /// How it should talk — tone & warmth.
    pub tone: String,
    /// The hard lines — what it should never do.
    pub never_do: String,
}

/// A drafted P13 Profile — the six fields the wizard collects.
/// Every field is best-effort; the review step lets the operator
/// fix anything.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DraftedProfile {
    pub assistant_name: Option<String>,
    pub operator_profile: Option<String>,
    pub communication_style: Option<String>,
    pub primary_use_cases: Vec<String>,
    pub behavioral_preferences: Vec<String>,
    pub behavioral_constraints: Vec<String>,
}

const DRAFT_MAX_TOKENS: u32 = 1024;

/// The system prompt. Conservative + format-pinned so the parser
/// has a deterministic shape to read. The model drafts; it never
/// invents authority (these are voice-layer preferences, not
/// capability grants).
const DRAFT_SYSTEM_PROMPT: &str =
    "You are helping a person set up a personal AI assistant by \
drafting its identity profile from how they described the \
relationship they want. Write in the assistant's own framing \
(warm, specific, never generic). Output EXACTLY these six \
labels, one per line, nothing else — no preamble, no commentary:\n\
ASSISTANT_NAME: <a short, friendly name>\n\
OPERATOR_PROFILE: <one sentence describing who the operator is>\n\
COMMUNICATION_STYLE: <a short phrase: tone, verbosity, warmth>\n\
PRIMARY_USE_CASES: <1-3 comma-separated archetypes>\n\
BEHAVIORAL_PREFERENCES: <comma-separated voice/judgment defaults, or 'none'>\n\
BEHAVIORAL_CONSTRAINTS: <comma-separated hard lines it must never cross, or 'none'>";

/// Compose the user message from the operator's conversation
/// answers. Public for prompt-shape unit tests.
pub fn compose_identity_prompt(a: &IdentityAnswers) -> String {
    format!(
        "Here is how the operator described what they want:\n\n\
         In their own words: {intent}\n\
         The role it should play: {role}\n\
         How it should talk to them: {tone}\n\
         Things it must never do: {never}\n\n\
         Draft the six-field identity profile now.",
        intent = blank_to_dash(&a.intent),
        role = blank_to_dash(&a.role),
        tone = blank_to_dash(&a.tone),
        never = blank_to_dash(&a.never_do),
    )
}

fn blank_to_dash(s: &str) -> &str {
    let t = s.trim();
    if t.is_empty() {
        "(not specified)"
    } else {
        t
    }
}

/// Parse the LLM's labeled response into a [`DraftedProfile`].
/// Tolerant: case-insensitive labels, ignores non-label lines,
/// splits list fields on commas / semicolons, and treats empty /
/// "none" / "-" values as undeclared. Public for parser tests.
pub fn parse_identity_draft(text: &str) -> DraftedProfile {
    let mut d = DraftedProfile::default();
    for line in text.lines() {
        let Some((label, val)) = line.split_once(':') else {
            continue;
        };
        match label.trim().to_ascii_uppercase().as_str() {
            "ASSISTANT_NAME" => d.assistant_name = scalar(val),
            "OPERATOR_PROFILE" => d.operator_profile = scalar(val),
            "COMMUNICATION_STYLE" => d.communication_style = scalar(val),
            "PRIMARY_USE_CASES" => d.primary_use_cases = list(val),
            "BEHAVIORAL_PREFERENCES" => {
                d.behavioral_preferences = list(val)
            }
            "BEHAVIORAL_CONSTRAINTS" => {
                d.behavioral_constraints = list(val)
            }
            _ => {}
        }
    }
    d
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

/// Draft a profile from the operator's answers via `provider`.
/// `None` on any LLM error (the caller falls back to manual
/// prompts) or if the model returned nothing usable.
pub async fn draft_identity(
    provider: &Arc<dyn LlmProvider>,
    model: &str,
    answers: &IdentityAnswers,
) -> Option<DraftedProfile> {
    let user = compose_identity_prompt(answers);
    let messages = vec![LlmMessage::user_text(user)];
    let request = LlmRequest {
        model,
        system: Some(DRAFT_SYSTEM_PROMPT),
        messages: &messages,
        tools: &[],
        max_tokens: DRAFT_MAX_TOKENS,
        temperature: Some(0.4),
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
    let drafted = parse_identity_draft(&text);
    // Require at least one field — a fully empty draft is a
    // degenerate response; fall back to manual.
    if drafted == DraftedProfile::default() {
        None
    } else {
        Some(drafted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_clean_six_field_draft() {
        let text = "ASSISTANT_NAME: Sage\n\
            OPERATOR_PROFILE: a senior Rust engineer who values directness\n\
            COMMUNICATION_STYLE: warm but concise, conclusion-first\n\
            PRIMARY_USE_CASES: systems programming, code review\n\
            BEHAVIORAL_PREFERENCES: prefer integration tests over mocks\n\
            BEHAVIORAL_CONSTRAINTS: never autonomously commit; always confirm destructive commands";
        let d = parse_identity_draft(text);
        assert_eq!(d.assistant_name.as_deref(), Some("Sage"));
        assert_eq!(
            d.operator_profile.as_deref(),
            Some("a senior Rust engineer who values directness")
        );
        assert_eq!(
            d.communication_style.as_deref(),
            Some("warm but concise, conclusion-first")
        );
        assert_eq!(
            d.primary_use_cases,
            vec!["systems programming", "code review"]
        );
        assert_eq!(
            d.behavioral_preferences,
            vec!["prefer integration tests over mocks"]
        );
        assert_eq!(
            d.behavioral_constraints,
            vec![
                "never autonomously commit",
                "always confirm destructive commands"
            ]
        );
    }

    #[test]
    fn tolerates_casing_preamble_quotes_and_extra_lines() {
        let text = "Sure! Here is the profile:\n\
            assistant_name: \"Mira\"\n\
            random noise line without a label match\n\
            Operator_Profile: a busy founder\n\
            PRIMARY_USE_CASES: \"email triage\", \"daily briefings\"\n";
        let d = parse_identity_draft(text);
        assert_eq!(d.assistant_name.as_deref(), Some("Mira"));
        assert_eq!(d.operator_profile.as_deref(), Some("a busy founder"));
        assert_eq!(
            d.primary_use_cases,
            vec!["email triage", "daily briefings"]
        );
        // Unmentioned fields stay undeclared.
        assert!(d.communication_style.is_none());
        assert!(d.behavioral_constraints.is_empty());
    }

    #[test]
    fn none_and_empty_values_are_undeclared() {
        let text = "ASSISTANT_NAME:\n\
            COMMUNICATION_STYLE: none\n\
            BEHAVIORAL_PREFERENCES: none\n\
            BEHAVIORAL_CONSTRAINTS: -, none, always confirm deletes";
        let d = parse_identity_draft(text);
        assert!(d.assistant_name.is_none());
        assert!(d.communication_style.is_none());
        assert!(d.behavioral_preferences.is_empty());
        // The nullish entries are dropped; the real one survives.
        assert_eq!(
            d.behavioral_constraints,
            vec!["always confirm deletes"]
        );
    }

    #[test]
    fn operator_profile_value_may_contain_colons() {
        let d = parse_identity_draft(
            "OPERATOR_PROFILE: a PM whose motto is: ship small",
        );
        assert_eq!(
            d.operator_profile.as_deref(),
            Some("a PM whose motto is: ship small")
        );
    }

    #[test]
    fn compose_prompt_includes_all_four_answers() {
        let a = IdentityAnswers {
            intent: "a calm research partner".into(),
            role: "confidant".into(),
            tone: "warm, unhurried".into(),
            never_do: "never flatter me".into(),
        };
        let p = compose_identity_prompt(&a);
        assert!(p.contains("a calm research partner"));
        assert!(p.contains("confidant"));
        assert!(p.contains("warm, unhurried"));
        assert!(p.contains("never flatter me"));
    }

    #[test]
    fn compose_prompt_marks_blank_answers() {
        let p = compose_identity_prompt(&IdentityAnswers::default());
        assert!(p.contains("(not specified)"));
    }
}
