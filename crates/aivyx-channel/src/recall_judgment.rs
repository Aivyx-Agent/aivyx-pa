//! Phase 91 — LLM-judged recall usefulness.
//!
//! The reflection-cron pass that augments the Phase 77
//! structural recall-feedback signal with a per-recall
//! 3-way LLM-judged classification (Q2a). The new per-hit
//! `RecallJudgment` ([`crate::recall_log::RecallJudgment`])
//! is recorded as a new optional field on `RecallHit`;
//! every existing accumulator (Phase 82 / 83 / 85 / 87 / 88)
//! stays byte-identical to pre-Phase-91 (Q3a augment).
//!
//! ## Trait shape
//!
//! [`RecallJudge::judge`] takes a *batch* of
//! [`RecallJudgeInput`] (one per recall in the lookback
//! window) and returns one [`Option<RecallJudgment>`] per
//! input in order. The contract:
//!
//! - One LLM call per `judge(…)` invocation, regardless of
//!   batch size. Bounded cost per reflection cycle
//!   (Q1a — the reflection-cron batched cadence).
//! - `None` for an individual recall on per-item parse
//!   failure; that recall stays unjudged this cycle, and
//!   the cycle's stat surface records the skip.
//! - Cycle-wide LLM failure → all `None`; the pass marks
//!   `llm_unavailable = true` on the stat surface (the
//!   same Phase 87 precedent).
//!
//! The production adapter [`LlmRecallJudge`] reuses the
//! `aivyx_llm::LlmProvider` already on `DaemonConfig`
//! (Phase 87's `LlmPairPhraser` is the precedent), so the
//! daemon pays no second LLM dependency.

use std::sync::{Arc, RwLock};

// moved to the wasm-clean aivyx-ipc crate (Chapter M.2d-2); re-exported here.
pub use aivyx_ipc::insights::{RecallJudgmentStat};

use async_trait::async_trait;

use aivyx_core::CancellationToken;
use aivyx_llm::{LlmMessage, LlmProvider, LlmRequest, LlmStepEnd};

pub use crate::recall_log::RecallJudgment;


/// Shared handle the recall-judgment pass writes (per cycle)
/// and `GetLearningInsights` reads. `None` inside = no cycle
/// has run yet this daemon lifetime.
pub type SharedRecallJudgmentStat =
    Arc<RwLock<Option<RecallJudgmentStat>>>;

/// Construct an empty shared judgment-stat handle.
pub fn shared_recall_judgment_stat() -> SharedRecallJudgmentStat {
    Arc::new(RwLock::new(None))
}

/// One recall event the judge is asked to classify. The
/// pass builder fills these in by recovering the recalled
/// memory body from the substrate + the model's response
/// text from the audit chain for the recall's turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecallJudgeInput {
    /// The recalled topic (the key under which the memory
    /// was stored — already canonicalized by Phase 89 if
    /// the operator enabled it).
    pub recalled_topic: String,
    /// The recalled memory body (truncated by the recall
    /// path; the judge sees what the model saw).
    pub recalled_body: String,
    /// The model's response text on the turn this recall
    /// participated in. Pulled from `TurnEnded.final_message`
    /// via the audit chain when present.
    pub response_text: String,
}

/// Phase 91 (Q2a) — the per-recall LLM judging seam. The
/// reflection-cron pass calls this once per cycle with the
/// batch of unjudged recalls; the adapter returns one
/// judgment per input in order.
#[async_trait]
pub trait RecallJudge: Send + Sync {
    async fn judge(
        &self,
        inputs: &[RecallJudgeInput],
    ) -> Vec<Option<RecallJudgment>>;
}

/// Hard cap on the LLM completion the production judge
/// asks for. The output is a small structured list; 1024
/// leaves headroom for ~30 entries × ~3 tokens per label +
/// formatting overhead.
const JUDGE_MAX_TOKENS: u32 = 1024;

/// Fixed system prompt for the recall-judgment call.
/// Conservative: the model classifies, never generates
/// new instructions. Phrasing emphasises the 3-way contract
/// and the deterministic output shape the parser expects.
const JUDGE_SYSTEM_PROMPT: &str =
    "You are classifying whether recalled memories were \
useful in a chat assistant's response. For each numbered \
recall, output EXACTLY ONE label on its own line in order: \
`used` (the response leveraged the recall), `irrelevant` \
(the response ignored it; no harm done), or `hurt` (the \
recall misled the response). Output ONLY the labels, one \
per line, no commentary, no numbering.";

/// Production `RecallJudge` adapter that delegates to the
/// existing `aivyx_llm::LlmProvider`. Constructed by the
/// binary with the same provider + model the agent uses;
/// failures map to `None` per candidate.
pub struct LlmRecallJudge {
    provider: Arc<dyn LlmProvider>,
    model: String,
}

impl LlmRecallJudge {
    pub fn new(
        provider: Arc<dyn LlmProvider>,
        model: String,
    ) -> Self {
        Self { provider, model }
    }

    /// Compose the user message for one batch — numbered
    /// (topic, body, response) triples the model classifies
    /// in order. Public for unit tests of the prompt shape.
    pub fn compose_prompt(inputs: &[RecallJudgeInput]) -> String {
        let mut s = String::new();
        for (i, input) in inputs.iter().enumerate() {
            s.push_str(&format!(
                "Recall {}:\n  topic: {}\n  body: {}\n  \
                 response: {}\n\n",
                i + 1,
                input.recalled_topic,
                input.recalled_body,
                input.response_text,
            ));
        }
        s
    }

    /// Parse the LLM's response into one `Option<RecallJudgment>`
    /// per input line. Public for unit testing the parser in
    /// isolation. The parser tolerates extra whitespace and
    /// case, returns `None` for unparseable lines, and pads /
    /// truncates the result to `expected` length.
    pub fn parse_response(
        text: &str,
        expected: usize,
    ) -> Vec<Option<RecallJudgment>> {
        let mut out: Vec<Option<RecallJudgment>> =
            Vec::with_capacity(expected);
        for line in text.lines() {
            let token = line.trim().to_ascii_lowercase();
            // Trim trailing punctuation a model might add.
            let token = token.trim_matches(|c: char| {
                !c.is_alphanumeric()
            });
            let parsed = match token {
                "used" => Some(RecallJudgment::Used),
                "irrelevant" => Some(RecallJudgment::Irrelevant),
                "hurt" => Some(RecallJudgment::Hurt),
                "" => continue, // blank lines: skip
                _ => None,
            };
            out.push(parsed);
            if out.len() == expected {
                break;
            }
        }
        // Pad to expected length so the caller can zip 1:1.
        while out.len() < expected {
            out.push(None);
        }
        out
    }
}

#[async_trait]
impl RecallJudge for LlmRecallJudge {
    async fn judge(
        &self,
        inputs: &[RecallJudgeInput],
    ) -> Vec<Option<RecallJudgment>> {
        if inputs.is_empty() {
            return Vec::new();
        }
        let user = Self::compose_prompt(inputs);
        let messages = vec![LlmMessage::user_text(user)];
        let request = LlmRequest {
            model: &self.model,
            system: Some(JUDGE_SYSTEM_PROMPT),
            messages: &messages,
            tools: &[],
            max_tokens: JUDGE_MAX_TOKENS,
            temperature: Some(0.0),
        id_slot: None,
        slot_hint: None,
        route: None,
        };
        // Non-cancellable token: the consolidation pass is on
        // a reflection cron, not an interactive turn (Phase 87
        // precedent). A daemon restart drops the future.
        let cancel = CancellationToken::new();
        let mut stream = match self
            .provider
            .chat_stream(request, &cancel)
            .await
        {
            Ok(s) => s,
            Err(_) => {
                // Cycle-wide LLM failure → all `None`. The
                // pass detects this and flips
                // `llm_unavailable = true` on the stat.
                return vec![None; inputs.len()];
            }
        };
        while let Ok(Some(_)) = stream.next_event().await {}
        let step = match stream.finish().await {
            Ok(s) => s,
            Err(_) => return vec![None; inputs.len()],
        };
        match step {
            LlmStepEnd::FinalMessage { text, .. } => {
                Self::parse_response(&text, inputs.len())
            }
            // Tool calls in a no-tool judging request →
            // provider misbehavior; treat as cycle-wide
            // unavailable.
            LlmStepEnd::ToolCalls { .. } => {
                vec![None; inputs.len()]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each enum variant round-trips through serde JSON
    /// using the documented snake_case labels.
    #[test]
    fn judgment_serde_round_trips_each_variant() {
        for (variant, expected_json) in [
            (RecallJudgment::Used, "\"used\""),
            (RecallJudgment::Irrelevant, "\"irrelevant\""),
            (RecallJudgment::Hurt, "\"hurt\""),
        ] {
            let j = serde_json::to_string(&variant).unwrap();
            assert_eq!(
                j, expected_json,
                "{variant:?} serializes to {expected_json}"
            );
            let back: RecallJudgment =
                serde_json::from_str(&j).unwrap();
            assert_eq!(back, variant);
        }
    }

    /// The composed prompt numbers every input from 1 and
    /// includes every (topic, body, response) field in
    /// order. Pure-function; no LLM needed.
    #[test]
    fn compose_prompt_numbers_inputs_in_order() {
        let inputs = vec![
            RecallJudgeInput {
                recalled_topic: "deploy".into(),
                recalled_body: "the runbook".into(),
                response_text: "yes the runbook is at...".into(),
            },
            RecallJudgeInput {
                recalled_topic: "rollback".into(),
                recalled_body: "git revert".into(),
                response_text: "unrelated answer".into(),
            },
        ];
        let prompt = LlmRecallJudge::compose_prompt(&inputs);
        assert!(prompt.contains("Recall 1:"));
        assert!(prompt.contains("Recall 2:"));
        assert!(prompt.contains("deploy"));
        assert!(prompt.contains("rollback"));
        // Ordering: deploy appears before rollback.
        let i_deploy = prompt.find("deploy").unwrap();
        let i_rollback = prompt.find("rollback").unwrap();
        assert!(i_deploy < i_rollback);
    }

    /// Parser accepts the three labels (any case), produces
    /// one `Option<RecallJudgment>` per input line in order,
    /// pads to `expected` length on short responses.
    #[test]
    fn parse_response_decodes_each_label() {
        let text = "used\nirrelevant\nhurt\n";
        let out = LlmRecallJudge::parse_response(text, 3);
        assert_eq!(
            out,
            vec![
                Some(RecallJudgment::Used),
                Some(RecallJudgment::Irrelevant),
                Some(RecallJudgment::Hurt),
            ]
        );
    }

    /// Case + whitespace tolerance: the model's exact output
    /// shape may vary.
    #[test]
    fn parse_response_tolerates_case_and_whitespace() {
        let text = "  Used  \n\tHURT\n  irrelevant\n";
        let out = LlmRecallJudge::parse_response(text, 3);
        assert_eq!(
            out,
            vec![
                Some(RecallJudgment::Used),
                Some(RecallJudgment::Hurt),
                Some(RecallJudgment::Irrelevant),
            ]
        );
    }

    /// A short response (fewer lines than expected) pads
    /// the tail with `None`; the caller can still zip 1:1.
    #[test]
    fn parse_response_pads_short_response_with_none() {
        let text = "used\n";
        let out = LlmRecallJudge::parse_response(text, 3);
        assert_eq!(
            out,
            vec![Some(RecallJudgment::Used), None, None]
        );
    }

    /// Unparseable lines map to `None` rather than crashing
    /// the cycle; the rest of the batch is still classified.
    #[test]
    fn parse_response_unparseable_lines_map_to_none() {
        let text =
            "used\nwhatever the model said\nhurt\n";
        let out = LlmRecallJudge::parse_response(text, 3);
        assert_eq!(
            out,
            vec![
                Some(RecallJudgment::Used),
                None,
                Some(RecallJudgment::Hurt),
            ]
        );
    }

    /// Empty input → empty output (no LLM call needed; the
    /// trait shape guarantees this even though the adapter
    /// would also short-circuit).
    #[test]
    fn parse_response_empty_input_returns_empty() {
        let out = LlmRecallJudge::parse_response("", 0);
        assert!(out.is_empty());
    }
}
