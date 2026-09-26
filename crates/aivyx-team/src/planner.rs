//! Chapter L — goal → [`MissionPlan`] decomposition.
//!
//! The daemon path (`aivyx-pa team run "<goal>"`) needs a concrete plan to drive
//! `run_until_pause`. Rather than run the full lead agent (capability stack,
//! tools, the inline `decompose_task` that also *executes*), this makes one
//! focused, tool-less LLM call: it asks the model to decompose a free-text
//! goal into the same friendly `{goal, steps}` spec the `decompose_task` tool
//! accepts, then parses it via [`parse_plan_spec`](crate::parse_plan_spec) and
//! validates the DAG. The engine stays pure — planning is a single completion,
//! execution is the runtime's job.

use aivyx_core::CancellationToken;
use aivyx_llm::{LlmMessage, LlmProvider, LlmRequest, LlmStepEnd};

use crate::config::{TeamConfig, TeamError};
use crate::mission::MissionPlan;
use crate::orchestration::parse_plan_spec;

/// Token ceiling for the planning completion — a DAG spec is small.
const PLANNER_MAX_TOKENS: u32 = 1500;

/// Decompose `goal` into a runnable [`MissionPlan`] for `config`'s team via one
/// tool-less LLM call. The plan delegates only to the team's specialists and
/// may insert gates (auto reviewer, or `human` for operator approval). Errors
/// on an LLM failure, output with no JSON object, or an invalid plan (cyclic /
/// unknown dep / empty / bad id).
pub async fn decompose_goal(
    provider: &dyn LlmProvider,
    model: &str,
    goal: &str,
    config: &TeamConfig,
    cancel: &CancellationToken,
    // #15 — `false` for an UNATTENDED (headless / loop-delegated) run: the prompt
    // forbids ALL gates. A `human` gate has no operator to approve it
    // (auto-rejects), and an `auto` gate that its reviewer FAILs aborts the whole
    // mission → Foreman retries and can ultimately *skip* a doable story (work
    // lost). An unattended mission should run its steps straight through. `true`
    // for interactive `team.run`, where the operator approves human gates and an
    // auto-gate reject is recoverable.
    allow_gates: bool,
) -> Result<MissionPlan, TeamError> {
    let goal = goal.trim();
    if goal.is_empty() {
        return Err(TeamError::Config("mission goal is empty".into()));
    }
    let system = planner_system_prompt(config, allow_gates);
    let user = format!("Mission goal:\n{goal}\n\nReturn the plan as JSON now.");
    let messages = vec![LlmMessage::user_text(user)];
    let request = LlmRequest {
        model,
        system: Some(&system),
        messages: &messages,
        tools: &[],
        max_tokens: PLANNER_MAX_TOKENS,
        temperature: Some(0.2),
    id_slot: None,
    slot_hint: None,
    route: None,
    };

    let mut stream = provider
        .chat_stream(request, cancel)
        .await
        .map_err(|e| TeamError::Config(format!("planner LLM call failed: {e}")))?;
    // Drain mid-stream chunks; we only need the final assembled text.
    while let Ok(Some(_)) = stream.next_event().await {}
    let text = match stream
        .finish()
        .await
        .map_err(|e| TeamError::Config(format!("planner LLM stream failed: {e}")))?
    {
        LlmStepEnd::FinalMessage { text, .. } => text,
        LlmStepEnd::ToolCalls { .. } => {
            return Err(TeamError::Config(
                "planner returned a tool call; expected a JSON plan".into(),
            ));
        }
    };

    let json = extract_json_object(&text).ok_or_else(|| {
        TeamError::Config(format!("planner output had no JSON object:\n{text}"))
    })?;
    let value: serde_json::Value = serde_json::from_str(json)
        .map_err(|e| TeamError::Config(format!("planner JSON did not parse: {e}")))?;
    let mut plan = parse_plan_spec(&value).map_err(TeamError::Config)?;
    // Anchor the plan to the operator's exact goal — the model echoes it, but
    // we don't want a paraphrase to drift the recorded mission.
    plan.goal = goal.to_string();
    plan.validate()?;
    Ok(plan)
}

/// The first non-empty line of a specialist's soul, clipped — keeps the roster
/// in the planning prompt compact.
fn soul_blurb(soul: &str) -> String {
    let line = soul.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    if line.chars().count() > 140 {
        let clipped: String = line.chars().take(140).collect();
        format!("{clipped}…")
    } else {
        line.to_string()
    }
}

/// Build the planning system prompt: the team's specialists + the exact JSON
/// spec the parser accepts.
fn planner_system_prompt(config: &TeamConfig, allow_gates: bool) -> String {
    let mut roster = String::new();
    for m in config.specialists() {
        roster.push_str(&format!(
            "- {} ({}){}: {}\n",
            m.name,
            m.role,
            write_capability_hint(&m.capability_scopes),
            soul_blurb(&m.soul)
        ));
    }
    if roster.is_empty() {
        roster.push_str("- (none)\n");
    }
    // #15 — gate guidance depends on whether an operator is present. Unattended
    // missions forbid ALL gates (a human gate auto-rejects; an auto gate that
    // fails aborts the whole mission → retry → a doable story can be skipped).
    let gate_rule = if allow_gates {
        "  - Use a \"human\" gate before irreversible or high-stakes work the operator should \
approve; an \"auto\" gate when a reviewer specialist should check quality first.\n"
    } else {
        "  - This mission runs UNATTENDED (no operator). Do NOT include ANY gate steps — \
no \"human\" gates (nothing can approve them) and no \"auto\" gates (a failed review would \
abort the whole mission). Use delegate steps only.\n"
    };
    format!(
        "You are the planning lead of a multi-agent team. Decompose the operator's \
mission into a DAG of steps and return ONLY a JSON object — no prose, no markdown \
fences, nothing before or after the object.\n\n\
Delegate work only to these specialists (refer to each by its name or role, \
exactly as listed):\n{roster}\n\
JSON shape: {{\"goal\": string, \"steps\": [step, ...]}}\n\
Each step is one of:\n\
  - delegate: {{\"id\": string, \"specialist\": <name>, \"prompt\": string, \"memory_topic\": \
string (optional), \"deps\": [id, ...]}}\n\
  - gate:     {{\"id\": string, \"reviewer\": <name>, \"criteria\": string, \"mode\": \"auto\" | \"human\", \"deps\": [id, ...]}}\n\n\
Rules:\n\
  - ids are unique and match [a-zA-Z0-9_-].\n\
  - `deps` lists step ids that must finish first; omit or use [] for none.\n\
  - Steps with disjoint deps run concurrently — exploit that.\n\
  - MEMORY TOPIC AGREEMENT: when two or more delegate steps will write memory about the same \
logical subject (e.g. both steps refine one shared summary), give them the SAME `memory_topic` \
so their writes land under one consistent name — do not let each specialist invent its own name \
for the same thing. Leave `memory_topic` unset when a step's memory writes are their own \
distinct subject. Only set `memory_topic` on a step whose OWN memory writes are all about that \
one shared subject: every write the step makes is forced to this exact topic, and near-identical \
entries under one topic get superseded (the older is deleted) — sharing a topic across genuinely \
distinct items (e.g. one step producing several similar per-item reports) can silently delete \
one of them.\n\
{gate_rule}\
  - CAPABILITY MATCH: a step that must CREATE or SAVE a file (or write a note to \
memory) MUST be delegated to a specialist tagged `[writes files]` (or `[writes \
memory]`). Never assign a persist/save/write step to a read-only specialist — \
it cannot produce the deliverable. If the goal asks for a file at a path, the \
LAST step should be a `[writes files]` specialist that writes exactly that file.\n\
  - Keep the plan minimal: only the steps the goal actually needs."
    )
}

/// Chapter Handoff — a compact capability tag for the planning roster so the
/// lead routes persist/save steps to a specialist that can actually write the
/// deliverable (the dogfood bug: a `save_file` step assigned to a read-only
/// `Operations` role that has no `fs.write`). Empty when the specialist can't
/// persist anything.
fn write_capability_hint(scopes: &[String]) -> String {
    let has = |base: &str| scopes.iter().any(|s| s == base || s.starts_with(&format!("{base}:")));
    let files = has("fs.write") || has("workspace.write");
    let memory = has("memory.write");
    match (files, memory) {
        (true, true) => " [writes files+memory]".to_string(),
        (true, false) => " [writes files]".to_string(),
        (false, true) => " [writes memory]".to_string(),
        (false, false) => String::new(),
    }
}

/// Extract the outermost `{...}` object from a model response that may be
/// wrapped in prose or ```json fences. Returns `None` if there's no object.
fn extract_json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    (end > start).then(|| &text[start..=end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roster::default_nonagon;
    use crate::testutil::FakeProvider;

    fn plan_json() -> &'static str {
        r#"{"goal":"close the kitchen","steps":[
            {"id":"count","specialist":"analyst","prompt":"count closing stock"},
            {"id":"approve","reviewer":"reviewer","criteria":"ok to order?","mode":"human","deps":["count"]},
            {"id":"order","specialist":"ops","prompt":"place the PO","deps":["approve"]}
        ]}"#
    }

    #[test]
    fn headless_planner_prompt_forbids_all_gates() {
        // #15 — an unattended (loop-delegated) decomposition must not emit ANY
        // gates: a human gate auto-rejects, and a failed auto gate aborts the
        // whole mission → retry → a doable story can be skipped (work lost).
        let cfg = default_nonagon();
        let interactive = planner_system_prompt(&cfg, true);
        let headless = planner_system_prompt(&cfg, false);
        assert!(interactive.contains("\"human\" gate"), "interactive keeps gates");
        assert!(headless.contains("UNATTENDED"));
        assert!(
            headless.contains("Do NOT include ANY gate"),
            "headless must forbid all gates"
        );
        assert!(
            !headless.contains("Use a \"human\" gate"),
            "headless must not encourage gates"
        );
    }

    #[test]
    fn write_capability_hint_tags_writers() {
        assert_eq!(write_capability_hint(&["fs.write".into()]), " [writes files]");
        assert_eq!(write_capability_hint(&["memory.write".into()]), " [writes memory]");
        assert_eq!(
            write_capability_hint(&["fs.write".into(), "memory.write".into()]),
            " [writes files+memory]"
        );
        assert_eq!(write_capability_hint(&["fs.read".into(), "shell.exec".into()]), "");
        // scoped form (base:qualifier) still counts
        assert_eq!(write_capability_hint(&["workspace.write:foo".into()]), " [writes files]");
    }

    #[test]
    fn planner_prompt_surfaces_write_capability_and_routing_rule() {
        let p = planner_system_prompt(&default_nonagon(), true);
        // Writer/Coder can write files; Verifier (shell.exec, fs.read, net.fetch) cannot.
        assert!(p.contains("writer (Writer) [writes files]"), "writer tagged: {p}");
        assert!(
            p.contains("verifier (Verifier):"),
            "read-only Verifier gets no write tag: {p}"
        );
        assert!(p.contains("CAPABILITY MATCH"), "routing rule present");
    }

    #[tokio::test]
    async fn decomposes_a_goal_into_a_validated_plan() {
        let provider = FakeProvider::says(plan_json());
        let plan = decompose_goal(
            provider.as_ref(),
            "test-model",
            "close the kitchen",
            &default_nonagon(),
            &CancellationToken::new(),
            true,
        )
        .await
        .expect("plan decodes");
        assert_eq!(plan.goal, "close the kitchen");
        assert_eq!(plan.steps.len(), 3);
        assert!(plan.step("approve").unwrap().is_human_gate());
        // deps survived → the runtime can walk it.
        assert_eq!(plan.step("order").unwrap().deps, vec!["approve".to_string()]);
    }

    #[tokio::test]
    async fn tolerates_prose_and_fences_around_the_json() {
        let wrapped = format!("Here is the plan:\n```json\n{}\n```\nDone.", plan_json());
        let provider = FakeProvider::says(&wrapped);
        let plan = decompose_goal(
            provider.as_ref(),
            "m",
            "close the kitchen",
            &default_nonagon(),
            &CancellationToken::new(),
            true,
        )
        .await
        .expect("plan decodes despite the wrapping");
        assert_eq!(plan.steps.len(), 3);
    }

    #[tokio::test]
    async fn overrides_the_goal_to_the_operators_exact_text() {
        // The model paraphrases the goal; we anchor to the operator's.
        let provider = FakeProvider::says(
            r#"{"goal":"shut down the kitchen for the night","steps":[
                {"id":"a","specialist":"ops","prompt":"do it"}
            ]}"#,
        );
        let plan = decompose_goal(
            provider.as_ref(),
            "m",
            "close the kitchen",
            &default_nonagon(),
            &CancellationToken::new(),
            true,
        )
        .await
        .unwrap();
        assert_eq!(plan.goal, "close the kitchen");
    }

    #[tokio::test]
    async fn rejects_output_without_a_json_object() {
        let provider = FakeProvider::says("I cannot help with that.");
        let err = decompose_goal(
            provider.as_ref(),
            "m",
            "goal",
            &default_nonagon(),
            &CancellationToken::new(),
            true,
        )
        .await
        .expect_err("no JSON → error");
        assert!(format!("{err}").contains("no JSON object"));
    }

    #[tokio::test]
    async fn rejects_a_cyclic_plan() {
        let provider = FakeProvider::says(
            r#"{"goal":"g","steps":[
                {"id":"a","specialist":"ops","prompt":"p","deps":["b"]},
                {"id":"b","specialist":"ops","prompt":"p","deps":["a"]}
            ]}"#,
        );
        assert!(decompose_goal(
            provider.as_ref(),
            "m",
            "goal",
            &default_nonagon(),
            &CancellationToken::new(),
            true,
        )
        .await
        .is_err());
    }

    #[tokio::test]
    async fn empty_goal_is_rejected_before_any_call() {
        let provider = FakeProvider::says("unused");
        assert!(decompose_goal(
            provider.as_ref(),
            "m",
            "   ",
            &default_nonagon(),
            &CancellationToken::new(),
            true,
        )
        .await
        .is_err());
    }
}
