//! Phase 178 — LLM-judged correction classification.
//!
//! Closes the Phase 172 honest debt: the structural correction
//! signal ("the operator came back within 60 s of a completed
//! turn") counts a genuine rework, a "thanks, perfect," and an
//! unrelated new request all the same. This judges each
//! detected correction's **follow-up message** into a 3-way
//! verdict so only genuine reworks feed the correction ledger.
//!
//! Mirrors Phase 91's `recall_judgment`: a trait + an
//! `aivyx_llm::LlmProvider`-backed adapter, one batched call per
//! reflection cycle, a parser-tolerant deterministic output
//! shape, and best-effort failure (parse/LLM error → `None`, so
//! the event keeps the structural signal rather than dropping).

use std::sync::Arc;

// moved to the wasm-clean aivyx-ipc crate (Chapter M.2d-2); re-exported here.
pub use aivyx_ipc::insights::{CorrectionJudgmentStat};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use aivyx_core::CancellationToken;
use aivyx_llm::{LlmMessage, LlmProvider, LlmRequest, LlmStepEnd};

/// The 3-way verdict on an operator's immediate follow-up after
/// a completed turn. Only [`CorrectionJudgment::Rework`] folds
/// into the correction ledger.
///
/// Stable snake_case labels for the wire + the parser:
/// `"rework"`, `"praise"`, `"unrelated"`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum CorrectionJudgment {
    /// A genuine correction: the operator is reworking / fixing
    /// / redirecting the previous answer.
    Rework,
    /// Praise / acknowledgment ("thanks", "perfect") — the
    /// re-engagement was NOT a correction.
    Praise,
    /// An unrelated new request — the operator moved on; the
    /// rapid follow-up is coincidental, not a correction.
    Unrelated,
}

/// One correction the judge classifies: the follow-up message
/// plus the corrected turn's topics for context.
#[derive(Debug, Clone, PartialEq)]
pub struct CorrectionJudgeInput {
    /// The corrected turn's distinct topics (context the model
    /// uses to decide if the follow-up reworks *this* subject).
    pub topics: Vec<String>,
    /// The operator's immediate follow-up message (captured on
    /// the follow-up turn's `RecallEvent.query_text`).
    pub follow_up_query: String,
}

/// The per-correction LLM judging seam. The reflection-cron pass
/// calls this once per cycle with the batch of judgeable
/// corrections; the adapter returns one verdict per input in
/// order (`None` = couldn't judge → keep structural).
#[async_trait]
pub trait CorrectionJudge: Send + Sync {
    async fn judge(
        &self,
        inputs: &[CorrectionJudgeInput],
    ) -> Vec<Option<CorrectionJudgment>>;
}

/// Hard cap on the LLM completion. One small label list.
const JUDGE_MAX_TOKENS: u32 = 1024;

/// Fixed system prompt. Conservative: the model classifies,
/// never generates instructions. Emphasises the 3-way contract
/// and the deterministic one-label-per-line output the parser
/// expects.
const JUDGE_SYSTEM_PROMPT: &str =
    "You are classifying an operator's immediate follow-up \
message to an AI assistant. For each numbered case, decide \
whether the follow-up is correcting/reworking the assistant's \
previous answer, or not. Output EXACTLY ONE label on its own \
line, in order: `rework` (the operator is fixing, correcting, \
redirecting, or expressing dissatisfaction with the previous \
answer), `praise` (thanks / acknowledgment / approval — not a \
correction), or `unrelated` (a new request on a different \
topic — not a correction). Output ONLY the labels, one per \
line, no commentary, no numbering.";

/// Production `CorrectionJudge` delegating to the existing
/// `aivyx_llm::LlmProvider` (same provider + model the agent
/// uses). Mirrors `LlmRecallJudge`.
pub struct LlmCorrectionJudge {
    provider: Arc<dyn LlmProvider>,
    model: String,
}

impl LlmCorrectionJudge {
    pub fn new(provider: Arc<dyn LlmProvider>, model: String) -> Self {
        Self { provider, model }
    }

    /// Compose the batch user message — numbered (topics,
    /// follow-up) cases the model classifies in order. Public
    /// for unit tests of the prompt shape.
    pub fn compose_prompt(inputs: &[CorrectionJudgeInput]) -> String {
        let mut s = String::new();
        for (i, input) in inputs.iter().enumerate() {
            s.push_str(&format!(
                "Case {}:\n  previous-answer topics: {}\n  \
                 operator follow-up: {}\n\n",
                i + 1,
                input.topics.join(", "),
                input.follow_up_query,
            ));
        }
        s
    }

    /// Parse the LLM's response into one `Option<CorrectionJudgment>`
    /// per input line. Tolerates whitespace, casing, trailing
    /// punctuation; `None` for unparseable lines; pads/truncates
    /// to `expected`. Public for parser unit tests.
    pub fn parse_response(
        text: &str,
        expected: usize,
    ) -> Vec<Option<CorrectionJudgment>> {
        let mut out: Vec<Option<CorrectionJudgment>> =
            Vec::with_capacity(expected);
        for line in text.lines() {
            let token = line.trim().to_ascii_lowercase();
            let token =
                token.trim_matches(|c: char| !c.is_alphanumeric());
            let parsed = match token {
                "rework" => Some(CorrectionJudgment::Rework),
                "praise" => Some(CorrectionJudgment::Praise),
                "unrelated" => Some(CorrectionJudgment::Unrelated),
                "" => continue,
                _ => None,
            };
            out.push(parsed);
            if out.len() == expected {
                break;
            }
        }
        while out.len() < expected {
            out.push(None);
        }
        out
    }
}

#[async_trait]
impl CorrectionJudge for LlmCorrectionJudge {
    async fn judge(
        &self,
        inputs: &[CorrectionJudgeInput],
    ) -> Vec<Option<CorrectionJudgment>> {
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
        let cancel = CancellationToken::new();
        let mut stream =
            match self.provider.chat_stream(request, &cancel).await {
                Ok(s) => s,
                Err(_) => return vec![None; inputs.len()],
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
            LlmStepEnd::ToolCalls { .. } => vec![None; inputs.len()],
        }
    }
}


/// Shared last-cycle stat handle the pass writes + the learning
/// surface reads.
pub type SharedCorrectionJudgmentStat =
    Arc<std::sync::RwLock<Option<CorrectionJudgmentStat>>>;

pub fn shared_correction_judgment_stat() -> SharedCorrectionJudgmentStat {
    Arc::new(std::sync::RwLock::new(None))
}

/// Phase 178 — the judged correction fold. Given the detailed
/// correction events + a judge + a per-cycle judge cap, returns
/// the per-topic counts to fold into the correction ledger and
/// the cycle stat.
///
/// Folding rule (the graceful-degrade posture):
/// - Event with **no** follow-up query → un-judgeable →
///   **counted** (structural fallback).
/// - Event judged **`Rework`** → counted.
/// - Event judged `Praise` / `Unrelated` → **dropped**.
/// - Event whose judge returned `None` (parse/LLM failure) →
///   **counted** (structural fallback — never lose a signal to
///   a transient outage).
/// - Judgeable events beyond `max_judge` this cycle → not sent
///   to the judge → **counted** (structural fallback); they may
///   be judged on a later cycle.
///
/// One topic counts once per kept event (distinct already by
/// [`crate::correction_detect::detect_corrections_detailed`]).
pub async fn judged_correction_counts(
    events: &[crate::correction_detect::CorrectionEvent],
    judge: &dyn CorrectionJudge,
    max_judge: usize,
    now_secs: u64,
) -> (Vec<(String, f32)>, CorrectionJudgmentStat) {
    use std::collections::HashMap;

    // Partition: judgeable (has a query, within the cap) vs the
    // rest (always kept on the structural fallback).
    let mut judge_inputs: Vec<CorrectionJudgeInput> = Vec::new();
    let mut judge_event_idx: Vec<usize> = Vec::new();
    for (i, ev) in events.iter().enumerate() {
        if judge_inputs.len() >= max_judge {
            break;
        }
        if let Some(q) = &ev.follow_up_query {
            judge_inputs.push(CorrectionJudgeInput {
                topics: ev.topics.clone(),
                follow_up_query: q.clone(),
            });
            judge_event_idx.push(i);
        }
    }

    let verdicts = if judge_inputs.is_empty() {
        Vec::new()
    } else {
        judge.judge(&judge_inputs).await
    };

    // Map each judged event index → its verdict.
    let mut verdict_by_idx: HashMap<usize, Option<CorrectionJudgment>> =
        HashMap::new();
    for (k, idx) in judge_event_idx.iter().enumerate() {
        verdict_by_idx.insert(*idx, verdicts.get(k).copied().flatten());
    }

    let mut counts: HashMap<String, f32> = HashMap::new();
    let mut stat = CorrectionJudgmentStat {
        ts_secs: now_secs,
        ..Default::default()
    };
    for (i, ev) in events.iter().enumerate() {
        let keep = match verdict_by_idx.get(&i) {
            // Judged this cycle.
            Some(Some(CorrectionJudgment::Rework)) => {
                stat.judged += 1;
                stat.rework += 1;
                true
            }
            Some(Some(CorrectionJudgment::Praise)) => {
                stat.judged += 1;
                stat.praise += 1;
                false
            }
            Some(Some(CorrectionJudgment::Unrelated)) => {
                stat.judged += 1;
                stat.unrelated += 1;
                false
            }
            // Judge returned None for this judged event → fallback.
            Some(None) => {
                stat.structural_fallback += 1;
                true
            }
            // Not judged (no query, or over-cap) → fallback.
            None => {
                stat.structural_fallback += 1;
                true
            }
        };
        if keep {
            for topic in &ev.topics {
                *counts.entry(topic.clone()).or_insert(0.0) += 1.0;
            }
        }
    }

    // Cycle-wide LLM outage: we sent inputs but got nothing usable.
    stat.llm_unavailable = !judge_inputs.is_empty()
        && stat.judged == 0
        && stat.structural_fallback >= judge_inputs.len() as u32;

    let mut out: Vec<(String, f32)> = counts.into_iter().collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    (out, stat)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::correction_detect::CorrectionEvent;

    #[test]
    fn judgment_serde_round_trips_each_variant() {
        for (variant, expected) in [
            (CorrectionJudgment::Rework, "\"rework\""),
            (CorrectionJudgment::Praise, "\"praise\""),
            (CorrectionJudgment::Unrelated, "\"unrelated\""),
        ] {
            let j = serde_json::to_string(&variant).unwrap();
            assert_eq!(j, expected);
            let back: CorrectionJudgment =
                serde_json::from_str(&j).unwrap();
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn parse_response_maps_clean_labels_in_order() {
        let out = LlmCorrectionJudge::parse_response(
            "rework\npraise\nunrelated\n",
            3,
        );
        assert_eq!(
            out,
            vec![
                Some(CorrectionJudgment::Rework),
                Some(CorrectionJudgment::Praise),
                Some(CorrectionJudgment::Unrelated),
            ]
        );
    }

    #[test]
    fn parse_response_tolerates_casing_and_punctuation() {
        let out = LlmCorrectionJudge::parse_response(
            "  REWORK.\n- Praise!\n\n  unrelated  \n",
            3,
        );
        assert_eq!(
            out,
            vec![
                Some(CorrectionJudgment::Rework),
                Some(CorrectionJudgment::Praise),
                Some(CorrectionJudgment::Unrelated),
            ]
        );
    }

    #[test]
    fn parse_response_pads_and_marks_unparseable() {
        // Two lines, one garbage; expected 3 → pad to 3.
        let out = LlmCorrectionJudge::parse_response(
            "rework\nbananas\n",
            3,
        );
        assert_eq!(out.len(), 3);
        assert_eq!(out[0], Some(CorrectionJudgment::Rework));
        assert_eq!(out[1], None); // unparseable
        assert_eq!(out[2], None); // padded
    }

    #[test]
    fn parse_response_truncates_excess() {
        let out = LlmCorrectionJudge::parse_response(
            "rework\npraise\nunrelated\nrework\n",
            2,
        );
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn compose_prompt_numbers_cases_with_topics_and_followup() {
        let inputs = vec![
            CorrectionJudgeInput {
                topics: vec!["auth".into(), "jwt".into()],
                follow_up_query: "no, I meant the refresh path".into(),
            },
            CorrectionJudgeInput {
                topics: vec!["css".into()],
                follow_up_query: "thanks!".into(),
            },
        ];
        let p = LlmCorrectionJudge::compose_prompt(&inputs);
        assert!(p.contains("Case 1:"));
        assert!(p.contains("auth, jwt"));
        assert!(p.contains("no, I meant the refresh path"));
        assert!(p.contains("Case 2:"));
        assert!(p.contains("thanks!"));
    }

    /// A deterministic fake judge for the Phase 178 Task 4 fold
    /// tests + here: classifies by a keyword in the follow-up.
    struct KeywordJudge;
    #[async_trait]
    impl CorrectionJudge for KeywordJudge {
        async fn judge(
            &self,
            inputs: &[CorrectionJudgeInput],
        ) -> Vec<Option<CorrectionJudgment>> {
            inputs
                .iter()
                .map(|i| {
                    let q = i.follow_up_query.to_ascii_lowercase();
                    if q.contains("thank") {
                        Some(CorrectionJudgment::Praise)
                    } else if q.contains("no") || q.contains("wrong") {
                        Some(CorrectionJudgment::Rework)
                    } else {
                        Some(CorrectionJudgment::Unrelated)
                    }
                })
                .collect()
        }
    }

    #[tokio::test]
    async fn fake_judge_classifies_batch_in_order() {
        let inputs = vec![
            CorrectionJudgeInput {
                topics: vec!["a".into()],
                follow_up_query: "no that's wrong".into(),
            },
            CorrectionJudgeInput {
                topics: vec!["b".into()],
                follow_up_query: "thanks, perfect".into(),
            },
            CorrectionJudgeInput {
                topics: vec!["c".into()],
                follow_up_query: "deploy the changes".into(),
            },
        ];
        let out = KeywordJudge.judge(&inputs).await;
        assert_eq!(
            out,
            vec![
                Some(CorrectionJudgment::Rework),
                Some(CorrectionJudgment::Praise),
                Some(CorrectionJudgment::Unrelated),
            ]
        );
    }

    #[tokio::test]
    async fn empty_batch_returns_empty() {
        assert!(KeywordJudge.judge(&[]).await.is_empty());
    }

    /// A judge that always fails (returns None) — cycle-wide
    /// outage.
    struct FailJudge;
    #[async_trait]
    impl CorrectionJudge for FailJudge {
        async fn judge(
            &self,
            inputs: &[CorrectionJudgeInput],
        ) -> Vec<Option<CorrectionJudgment>> {
            vec![None; inputs.len()]
        }
    }

    fn ev(topics: &[&str], query: Option<&str>) -> CorrectionEvent {
        CorrectionEvent {
            topics: topics.iter().map(|t| t.to_string()).collect(),
            follow_up_query: query.map(|q| q.to_string()),
        }
    }

    #[tokio::test]
    async fn fold_keeps_only_rework_and_unjudgeable() {
        let events = vec![
            ev(&["auth"], Some("no that's wrong")), // Rework → keep
            ev(&["css"], Some("thanks, perfect")),  // Praise → drop
            ev(&["db"], Some("deploy the changes")), // Unrelated → drop
            ev(&["api"], None),                      // no query → keep
        ];
        let (counts, stat) =
            judged_correction_counts(&events, &KeywordJudge, 100, 7).await;
        // Only auth (Rework) + api (structural fallback) fold.
        assert_eq!(
            counts,
            vec![("api".to_string(), 1.0), ("auth".to_string(), 1.0)]
        );
        assert_eq!(stat.judged, 3);
        assert_eq!(stat.rework, 1);
        assert_eq!(stat.praise, 1);
        assert_eq!(stat.unrelated, 1);
        assert_eq!(stat.structural_fallback, 1); // the no-query event
        assert!(!stat.llm_unavailable);
        assert_eq!(stat.ts_secs, 7);
    }

    #[tokio::test]
    async fn fold_cap_sends_only_max_judge_rest_structural() {
        let events = vec![
            ev(&["a"], Some("no")), // judged (cap=1) → Rework keep
            ev(&["b"], Some("thanks")), // over cap → structural keep
        ];
        let (counts, stat) =
            judged_correction_counts(&events, &KeywordJudge, 1, 0).await;
        // a kept (Rework); b kept (over-cap fallback, NOT dropped).
        assert_eq!(
            counts,
            vec![("a".to_string(), 1.0), ("b".to_string(), 1.0)]
        );
        assert_eq!(stat.judged, 1);
        assert_eq!(stat.rework, 1);
        assert_eq!(stat.structural_fallback, 1);
    }

    #[tokio::test]
    async fn fold_judge_failure_falls_back_to_structural() {
        let events =
            vec![ev(&["auth"], Some("no")), ev(&["db"], Some("x"))];
        let (counts, stat) =
            judged_correction_counts(&events, &FailJudge, 100, 0).await;
        // Both kept on the structural fallback (never lose signal
        // to an outage).
        assert_eq!(
            counts,
            vec![("auth".to_string(), 1.0), ("db".to_string(), 1.0)]
        );
        assert_eq!(stat.judged, 0);
        assert_eq!(stat.structural_fallback, 2);
        assert!(stat.llm_unavailable);
    }

    #[tokio::test]
    async fn fold_all_no_query_never_calls_judge() {
        let events = vec![ev(&["a"], None), ev(&["b"], None)];
        let (counts, stat) =
            judged_correction_counts(&events, &FailJudge, 100, 0).await;
        assert_eq!(counts.len(), 2);
        assert_eq!(stat.structural_fallback, 2);
        // No judge inputs → not flagged as an outage.
        assert!(!stat.llm_unavailable);
    }
}
