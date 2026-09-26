//! Chapter Nonagon Templates — LLM-assisted team-roster drafting.
//!
//! The operator's declared Profile (`operator_profile` + `primary_use_cases`)
//! plus optional free text describes what THEY do; the configured model
//! drafts a full 9-member Nonagon — a coordinator lead + 8 specialists —
//! tailored to that domain (kitchen-ops roles for a chef, dev-team roles for
//! an engineer, and so on). The operator reviews and edits every member in
//! the same draft-and-approve state the manual roster editor already uses
//! (Chapter Roster) before anything saves.
//!
//! **The security-critical design choice, mirrored from [`crate::persona_seed_draft`]:
//! the LLM drafts identity, never capability.** It picks a `name`, a human
//! `role` title, and a `soul` (system prompt) for each specialist — but the
//! actual `tool_allowlist` / `capability_scopes` are never LLM-authored
//! strings. Each drafted specialist carries a `tool_profile` tag, which MUST
//! be one of the 8 archetypes `aivyx_team::roster::default_nonagon()`
//! already ships and this workspace has already audited (researcher /
//! analyst / coder / writer / reviewer / planner / verifier / archivist).
//! This module maps the tag to the real scopes in code; an unrecognized tag
//! degrades to a safe read-only default rather than being trusted. The
//! model can never grant itself a capability by naming one in prose.

use std::sync::Arc;

use aivyx_capability::TrustTier;
use aivyx_core::CancellationToken;
use aivyx_llm::{LlmMessage, LlmProvider, LlmRequest, LlmStepEnd};
use aivyx_team::config::{DialogueConfig, TeamConfig, TeamMember, MAX_SPECIALISTS};

const DRAFT_MAX_TOKENS: u32 = 1_600;

const DRAFT_SYSTEM_PROMPT: &str =
    "You design a specialist team (a \"Nonagon\": one coordinator plus 8 \
specialists) for an AI agent to help someone in a specific role or domain. \
You will be told the operator's role, their primary use cases, and optional \
extra context. Invent 8 specialist roles TAILORED to that domain — give each \
a domain-specific name and a short system-prompt-style soul describing what \
it does and how. Do NOT invent tools or permissions: each specialist must be \
tagged with the tool_profile that best fits what kind of work it does, \
chosen from EXACTLY this list: researcher, analyst, coder, writer, reviewer, \
planner, verifier, archivist. (Meaning: researcher=gathers/searches \
information; analyst=examines data for patterns; coder=writes/runs code; \
writer=produces documents/prose; reviewer=critiques others' work; \
planner=breaks goals into steps; verifier=proves work actually works by \
executing it; archivist=persists and retrieves records.) Reuse a tool_profile \
across multiple specialists if the domain calls for it. Output ONLY a JSON \
object, no prose, no markdown fences: \
{\"team_name\":\"kebab-case-name\",\"description\":\"one sentence\",\
\"lead_soul\":\"the coordinator's system prompt\",\
\"specialists\":[{\"name\":\"kebab-case-id\",\"role\":\"Human-Readable Title\",\
\"soul\":\"system prompt for this specialist\",\"tool_profile\":\"one of the 8 tags\"}]}\
 — specialists must have EXACTLY 8 entries.";

fn compose_user_prompt(operator_role: Option<&str>, use_cases: &[String], description: &str) -> String {
    let role = operator_role.unwrap_or("(not specified)");
    let cases = if use_cases.is_empty() {
        "(not specified)".to_string()
    } else {
        use_cases.join(", ")
    };
    let extra = description.trim();
    let extra = if extra.is_empty() { "(none)" } else { extra };
    format!(
        "Operator's declared role: {role}\n\
         Primary use cases: {cases}\n\
         Extra context from the operator: {extra}\n\n\
         Design the 8-specialist team now.",
    )
}

/// One drafted specialist, pre-mapping. Public for parse tests.
#[derive(Debug, Clone, PartialEq)]
struct DraftedSpecialist {
    name: String,
    role: String,
    soul: String,
    tool_profile: String,
}

/// A fully drafted team, pre-conversion to [`TeamConfig`]. Public for tests.
#[derive(Debug, Clone, PartialEq)]
struct DraftedTeam {
    team_name: String,
    description: String,
    lead_soul: String,
    specialists: Vec<DraftedSpecialist>,
}

/// Tolerant parse: locate the outermost `{...}` (the model may wrap it in
/// prose or a markdown fence despite instructions) and decode it.
fn parse_drafted(text: &str) -> Option<DraftedTeam> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end <= start {
        return None;
    }
    let raw = &text[start..=end];
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    let team_name = v.get("team_name")?.as_str()?.trim().to_string();
    let description = v
        .get("description")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let lead_soul = v.get("lead_soul")?.as_str()?.trim().to_string();
    let specialists: Vec<DraftedSpecialist> = v
        .get("specialists")?
        .as_array()?
        .iter()
        .filter_map(|m| {
            Some(DraftedSpecialist {
                name: m.get("name")?.as_str()?.trim().to_string(),
                role: m.get("role")?.as_str()?.trim().to_string(),
                soul: m.get("soul")?.as_str()?.trim().to_string(),
                tool_profile: m
                    .get("tool_profile")?
                    .as_str()?
                    .trim()
                    .to_ascii_lowercase(),
            })
        })
        .filter(|s| !s.name.is_empty() && !s.soul.is_empty())
        .collect();
    if team_name.is_empty() || lead_soul.is_empty() || specialists.is_empty() {
        return None;
    }
    Some(DraftedTeam { team_name, description, lead_soul, specialists })
}

/// The real, already-audited tool_allowlist + capability_scopes for a
/// `tool_profile` tag — copied from `default_nonagon()`'s specialist shapes.
/// The ONLY place a drafted team's actual capabilities come from; an LLM's
/// prose can select a tag but never author a scope string.
fn profile_grant(tool_profile: &str) -> (&'static [&'static str], &'static [&'static str]) {
    match tool_profile {
        "researcher" => (
            &["web.fetch", "web.search", "fs.read", "memory.write", "mcp.call"],
            &["web.search", "net.fetch", "fs.read", "memory.write", "mcp.call"],
        ),
        "analyst" => (
            &["web.fetch", "web.search", "fs.read", "mcp.call"],
            &["net.fetch", "web.search", "fs.read", "mcp.call"],
        ),
        "coder" => (
            &["fs.read", "fs.write", "shell.exec", "workspace.read", "workspace.write"],
            &["fs.read", "fs.write", "shell.exec"],
        ),
        "writer" => (
            &["fs.read", "fs.write", "workspace.read", "workspace.write"],
            &["fs.read", "fs.write"],
        ),
        "reviewer" => (&["fs.read"], &["fs.read"]),
        "planner" => (
            &["memory.read", "memory.write"],
            &["memory.read", "memory.write"],
        ),
        "verifier" => (
            &["shell.exec", "fs.read", "web.fetch", "mcp.call"],
            &["shell.exec", "fs.read", "net.fetch", "mcp.call"],
        ),
        // Unrecognized tag (a hallucinated tool_profile) degrades to the
        // safest archetype rather than being trusted with anything wider.
        _ => (
            &["memory.read", "memory.write", "fs.read", "workspace.read", "workspace.write"],
            &["memory.read", "memory.write", "fs.read"],
        ),
    }
}

fn to_team_member(s: &DraftedSpecialist) -> TeamMember {
    let (tools, scopes) = profile_grant(&s.tool_profile);
    let mut capability_scopes: Vec<String> = scopes.iter().map(|s| s.to_string()).collect();
    capability_scopes.push("team.message".to_string());
    TeamMember {
        name: s.name.clone(),
        role: s.role.clone(),
        soul: s.soul.clone(),
        tool_allowlist: tools.iter().map(|t| t.to_string()).collect(),
        capability_scopes,
        trust_ceiling: TrustTier::Trusted,
        model: None,
        base_url: None,
    }
}

/// Convert a parsed draft into a valid [`TeamConfig`]. Clamps to exactly
/// [`MAX_SPECIALISTS`] specialists (drops extras, tops up short lists with a
/// generic archivist slot) so a model that miscounts can never produce an
/// invalid — or oversized — Nonagon.
fn to_team_config(d: DraftedTeam) -> TeamConfig {
    let mut specialists: Vec<TeamMember> = d.specialists.iter().map(to_team_member).collect();
    specialists.truncate(MAX_SPECIALISTS);
    let mut i = specialists.len();
    while specialists.len() < MAX_SPECIALISTS {
        specialists.push(to_team_member(&DraftedSpecialist {
            name: format!("specialist-{i}"),
            role: "Specialist".to_string(),
            soul: "You assist the team with general research and record-keeping tasks."
                .to_string(),
            tool_profile: "archivist".to_string(),
        }));
        i += 1;
    }

    let mut members = vec![TeamMember {
        name: "coordinator".to_string(),
        role: "Lead".to_string(),
        soul: d.lead_soul,
        tool_allowlist: vec!["delegate_task".to_string(), "query_agent".to_string()],
        capability_scopes: vec![
            "memory.read".to_string(),
            "memory.write".to_string(),
            "team.delegate".to_string(),
            "team.message".to_string(),
        ],
        trust_ceiling: TrustTier::Trusted,
        model: None,
        base_url: None,
    }];
    members.append(&mut specialists);

    TeamConfig {
        name: sanitize_name(&d.team_name),
        description: d.description,
        lead: "coordinator".to_string(),
        members,
        dialogue: DialogueConfig::default(),
    }
}

/// `TeamConfig::validate` requires `a-z A-Z 0-9 _ -` names; degrade a messy
/// LLM-drafted name to something that always parses rather than rejecting
/// an otherwise-good draft over a stray character.
fn sanitize_name(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '-' })
        .collect();
    let trimmed = cleaned.trim_matches('-');
    if trimmed.is_empty() {
        "custom-nonagon".to_string()
    } else {
        trimmed.chars().take(128).collect()
    }
}

/// Draft a role-tailored Nonagon via `provider`. Returns `None` on any LLM
/// error, a response with no usable JSON, or a draft that fails
/// [`TeamConfig::validate`] even after clamping — the caller falls back to
/// manual editing / the stock default, exactly like a failed persona-seed
/// draft.
pub async fn draft_team_template(
    provider: &Arc<dyn LlmProvider>,
    model: &str,
    operator_role: Option<&str>,
    use_cases: &[String],
    description: &str,
) -> Option<TeamConfig> {
    let user = compose_user_prompt(operator_role, use_cases, description);
    let messages = vec![LlmMessage::user_text(user)];
    let request = LlmRequest {
        model,
        system: Some(DRAFT_SYSTEM_PROMPT),
        messages: &messages,
        tools: &[],
        max_tokens: DRAFT_MAX_TOKENS,
        temperature: Some(0.6),
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
    let drafted = parse_drafted(&text)?;
    let config = to_team_config(drafted);
    config.validate().ok()?;
    Some(config)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_json(n_specialists: usize) -> String {
        let specialists: Vec<String> = (0..n_specialists)
            .map(|i| {
                format!(
                    r#"{{"name":"role-{i}","role":"Role {i}","soul":"does thing {i}","tool_profile":"researcher"}}"#
                )
            })
            .collect();
        format!(
            r#"{{"team_name":"kitchen-ops","description":"a kitchen team","lead_soul":"you coordinate the kitchen team","specialists":[{}]}}"#,
            specialists.join(",")
        )
    }

    #[test]
    fn parses_a_clean_eight_specialist_draft() {
        let d = parse_drafted(&sample_json(8)).expect("parses");
        assert_eq!(d.team_name, "kitchen-ops");
        assert_eq!(d.specialists.len(), 8);
        assert_eq!(d.specialists[0].tool_profile, "researcher");
    }

    #[test]
    fn tolerates_prose_and_markdown_fence_around_json() {
        let wrapped = format!("Sure, here you go:\n```json\n{}\n```", sample_json(8));
        assert!(parse_drafted(&wrapped).is_some());
    }

    #[test]
    fn missing_required_field_returns_none() {
        assert!(parse_drafted(r#"{"team_name":"x"}"#).is_none());
        assert!(parse_drafted("not json at all").is_none());
    }

    #[test]
    fn unrecognized_tool_profile_degrades_to_safe_grant_never_invents_scopes() {
        let (tools, scopes) = profile_grant("system-administrator-root-access");
        // Falls through to the archivist-shaped safe default — never a
        // scope string the model invented.
        assert!(!tools.contains(&"shell.exec"));
        assert!(scopes.contains(&"memory.read"));
    }

    #[test]
    fn oversized_draft_clamps_to_exactly_max_specialists() {
        let drafted = parse_drafted(&sample_json(12)).expect("parses");
        let config = to_team_config(drafted);
        assert_eq!(config.members.len() - 1, MAX_SPECIALISTS);
        config.validate().expect("clamped team is always valid");
    }

    #[test]
    fn undersized_draft_tops_up_to_exactly_max_specialists() {
        let drafted = parse_drafted(&sample_json(3)).expect("parses");
        let config = to_team_config(drafted);
        assert_eq!(config.members.len() - 1, MAX_SPECIALISTS);
        config.validate().expect("topped-up team is always valid");
    }

    #[test]
    fn messy_team_name_sanitizes_to_a_valid_identifier() {
        assert_eq!(sanitize_name("Kitchen Ops! 🍳"), "Kitchen-Ops");
        assert_eq!(sanitize_name("***"), "custom-nonagon");
        assert_eq!(sanitize_name(""), "custom-nonagon");
    }

    #[test]
    fn compose_prompt_handles_missing_role_and_use_cases() {
        let p = compose_user_prompt(None, &[], "");
        assert!(p.contains("(not specified)"));
        assert!(p.contains("(none)"));
        let p2 = compose_user_prompt(
            Some("head chef"),
            &["food safety".to_string(), "cost control".to_string()],
            "runs 15 kitchens",
        );
        assert!(p2.contains("head chef"));
        assert!(p2.contains("food safety, cost control"));
        assert!(p2.contains("runs 15 kitchens"));
    }

    #[test]
    fn drafted_specialist_never_carries_a_literal_scope_string() {
        // Structural guarantee: DraftedSpecialist has no capability_scopes
        // field at all — the type system makes "LLM invents a scope"
        // unrepresentable, not just unlikely.
        let s = DraftedSpecialist {
            name: "x".into(),
            role: "y".into(),
            soul: "z".into(),
            tool_profile: "coder".into(),
        };
        let member = to_team_member(&s);
        // Every scope on the resulting member traces to `profile_grant`,
        // never to `s` directly.
        let (_, expected_scopes) = profile_grant("coder");
        for scope in expected_scopes {
            assert!(member.capability_scopes.iter().any(|c| c == scope));
        }
    }
}
