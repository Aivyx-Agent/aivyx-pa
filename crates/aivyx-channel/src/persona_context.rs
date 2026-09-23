//! Phase 79 — adaptive Persona: the contextual refiner.
//!
//! `PersonaContextRefiner` is the concrete
//! [`aivyx_core::llm_planner::SystemPromptRefiner`]: each turn
//! it selects the Persona facets semantically relevant to the
//! user's message and re-assembles the system prompt with only
//! those, instead of dumping the whole accreted Soul every
//! time. The Phase 79 Q2a invariant (core identity +
//! `behavioral_constraints` always injected in full) is
//! enforced structurally by `profile_prompt::reduce_persona`,
//! not here.
//!
//! Everything is best-effort and never a regression: below the
//! size threshold, or with no embedding / an embed failure,
//! `refine` returns `None` and the planner keeps its
//! byte-identical full-Persona base prompt (Q3a).

use std::collections::HashSet;

// moved to the wasm-clean aivyx-ipc crate (Chapter M.2d-2); re-exported here.
pub use aivyx_ipc::insights::{PersonaSelectionStat};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;

use aivyx_core::llm_planner::SystemPromptRefiner;
use aivyx_llm::embedding::EmbeddingProvider;

use crate::conversation_window::{
    assemble_for, SharedConversationWindows,
};
use crate::persona::SharedEffectivePersona;
use crate::profile_prompt::{
    assemble_session_prompt_selected, reducible_facet_count,
};

/// Below this many reducible facets the Soul is not big enough
/// to be worth bounding — inject it whole (Q3a). The feature is
/// invisible until it adds value.
pub const DEFAULT_SIZE_THRESHOLD: usize = 12;
/// Max soft facets injected per turn once selection engages.
pub const DEFAULT_TOP_K: usize = 12;
/// Cosine floor: a facet below this is not "relevant to this
/// turn" and is dropped even if `top_k` is unfilled (same
/// rationale as the Phase 76 recall floor).
pub const DEFAULT_MIN_SIMILARITY: f32 = 0.20;


/// Shared handle the refiner writes and the
/// `GetLearningInsights` handler reads. `None` inside = no
/// adaptive selection has run yet this daemon lifetime.
pub type SharedPersonaSelectionStat =
    Arc<RwLock<Option<PersonaSelectionStat>>>;

/// Construct an empty shared selection-stat handle.
pub fn shared_persona_selection_stat() -> SharedPersonaSelectionStat {
    Arc::new(RwLock::new(None))
}

/// Cosine of two equal-length vectors. Hand-rolled, zero-dep
/// (the Phase 75 "no linalg crate" ethos). Returns 0.0 for the
/// degenerate cases so they rank as "not relevant" rather than
/// erroring.
fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut na = 0.0f32;
    let mut nb = 0.0f32;
    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

pub struct PersonaContextRefiner {
    profile: aivyx_config::Profile,
    persona: SharedEffectivePersona,
    role_name: String,
    role_prompt: String,
    provider: Arc<dyn EmbeddingProvider>,
    size_threshold: usize,
    top_k: usize,
    min_similarity: f32,
    /// Phase 79 (Q4a) — optional last-selection sink for the
    /// Phase 78 surface. `None` → breadcrumb-only.
    stat: Option<SharedPersonaSelectionStat>,
    /// Phase 86 — optional per-session recent-turns buffer. When
    /// `Some` and `recall_window_turns > 1`, the embedded query
    /// is the assembled conversation window instead of the bare
    /// user message; otherwise byte-identical pre-Phase-86 path.
    conversation_windows: Option<SharedConversationWindows>,
    /// Phase 86 — operator-tunable window depth (turns of prior
    /// context to concatenate before `current`). `1` (the
    /// default) disables the window — byte-identical fallback.
    recall_window_turns: usize,
    /// Phase 90 — heuristic recall gate threshold. `0` (the
    /// default) disables the gate — every turn flows through
    /// to the embed (byte-identical to pre-Phase-90). When
    /// raised, turns whose trimmed user message is shorter
    /// than this Unicode-char count short-circuit `refine` to
    /// `None` at the top — the planner uses the full Persona
    /// base prompt (the existing pre-Phase-79 fallback).
    recall_gate_min_chars: usize,
    /// Phase 97 — token-cost hard cap on the adaptive
    /// Persona facet selection. `0` (default) disables
    /// budget enforcement (byte-identical to pre-Phase-97).
    /// When `>= 1`, applied AFTER the existing top_k +
    /// min_similarity filter: lowest-priority facets drop
    /// until the running estimate fits. The Phase 79
    /// selection stat sees the post-budget set.
    recall_token_budget: u32,
    /// Aivyx-Skills Part 3 — optional pre-rendered `##
    /// Default skills` section, threaded through to
    /// `assemble_session_prompt_selected` so it survives a
    /// turn this refiner engages on. This refiner rebuilds
    /// the prompt from scratch rather than composing onto an
    /// existing base, so without this field the section would
    /// silently vanish whenever adaptive selection fires.
    /// `None` (the default) is byte-identical to
    /// pre-Aivyx-Skills-Part-3 output.
    default_skills_section: Option<String>,
}

impl PersonaContextRefiner {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        profile: aivyx_config::Profile,
        persona: SharedEffectivePersona,
        role_name: String,
        role_prompt: String,
        provider: Arc<dyn EmbeddingProvider>,
        size_threshold: usize,
        top_k: usize,
        min_similarity: f32,
    ) -> Self {
        Self {
            profile,
            persona,
            role_name,
            role_prompt,
            provider,
            size_threshold,
            top_k,
            min_similarity,
            stat: None,
            conversation_windows: None,
            recall_window_turns: 1,
            recall_gate_min_chars: 0,
            recall_token_budget: 0,
            default_skills_section: None,
        }
    }

    /// Aivyx-Skills Part 3 — attach the pre-rendered `##
    /// Default skills` section (built once at daemon startup,
    /// same as every other caller of
    /// `assemble_session_prompt_with_relevance`). Builder; the
    /// binary calls this with its own `default_skills_section`
    /// local so the section survives turns this refiner
    /// engages on instead of silently disappearing.
    pub fn with_default_skills_section(
        mut self,
        section: String,
    ) -> Self {
        self.default_skills_section = Some(section);
        self
    }

    /// Phase 97 — set the token-cost budget on adaptive
    /// Persona facet selection. Builder; the binary calls
    /// this with `config.embedding.recall_token_budget`.
    /// With `0` (the default) budget enforcement is off
    /// and behaviour is byte-identical to pre-Phase-97.
    pub fn with_recall_token_budget(
        mut self,
        budget: u32,
    ) -> Self {
        self.recall_token_budget = budget;
        self
    }

    /// Phase 90 — set the heuristic recall-gate threshold.
    /// Builder; the binary calls this with
    /// `config.embedding.recall_gate_min_chars`. With `0`
    /// (default), the refiner is byte-identical to
    /// pre-Phase-90; with `n >= 1`, turns whose trimmed user
    /// message is shorter than `n` Unicode chars short-circuit
    /// `refine` to `None` before any embed call (the planner
    /// uses the full Persona base prompt).
    pub fn with_recall_gate(
        mut self,
        min_chars: usize,
    ) -> Self {
        self.recall_gate_min_chars = min_chars;
        self
    }

    /// Phase 79 (Q4a) — attach the shared last-selection stat
    /// so the Phase 78 learning surface can show what the
    /// adaptive Soul did. Builder; the binary calls this with
    /// the same handle it passes into `DaemonConfig`.
    pub fn with_stat(
        mut self,
        stat: SharedPersonaSelectionStat,
    ) -> Self {
        self.stat = Some(stat);
        self
    }

    /// Phase 86 — attach the shared per-session conversation
    /// windows + the operator-set window depth. Builder; the
    /// binary calls this with the daemon-startup handle. When
    /// `recall_window_turns <= 1` the refiner is byte-identical
    /// to pre-Phase-86 even if a handle is attached.
    pub fn with_conversation_windows(
        mut self,
        windows: SharedConversationWindows,
        recall_window_turns: usize,
    ) -> Self {
        self.conversation_windows = Some(windows);
        self.recall_window_turns = recall_window_turns;
        self
    }

    /// Production constructor — module-default thresholds.
    pub fn with_defaults(
        profile: aivyx_config::Profile,
        persona: SharedEffectivePersona,
        role_name: String,
        role_prompt: String,
        provider: Arc<dyn EmbeddingProvider>,
    ) -> Self {
        Self::new(
            profile,
            persona,
            role_name,
            role_prompt,
            provider,
            DEFAULT_SIZE_THRESHOLD,
            DEFAULT_TOP_K,
            DEFAULT_MIN_SIMILARITY,
        )
    }
}

#[async_trait]
impl SystemPromptRefiner for PersonaContextRefiner {
    async fn refine(
        &self,
        user_message: &str,
        session_id: aivyx_core::SessionId,
        // Phase 117 — the planner's current base prompt. The
        // Phase 79 PersonaContextRefiner builds the prompt
        // from scratch (Profile + selected Persona + Role)
        // and ignores this arg; only Phase 117's
        // RelevancePromptRefiner uses it as a composition
        // base.
        _base_prompt: &str,
    ) -> Option<String> {
        // Phase 90 — heuristic recall gate. On a noise turn
        // short-circuit before any embed call; the planner
        // uses the full Persona base prompt (the existing
        // pre-Phase-79 / Soul-too-small fallback path).
        if crate::recall_gate::should_gate_recall(
            user_message,
            self.recall_gate_min_chars,
        ) {
            return None;
        }
        // Snapshot under the read lock, then drop it before any
        // await (never hold a std RwLock across .await).
        let snapshot = {
            let guard = self.persona.read().ok()?;
            guard.clone()
        };

        // Q3a — small Soul: nothing worth selecting over, inject
        // it whole (planner keeps its base prompt → None).
        if reducible_facet_count(&snapshot) < self.size_threshold {
            return None;
        }

        // Unique reducible facet strings, order-preserving.
        let mut facets: Vec<String> = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        for list in [
            &snapshot.primary_use_cases,
            &snapshot.behavioral_preferences,
            &snapshot.learned_context,
            &snapshot.communication_adaptations,
            &snapshot.character_traits,
            &snapshot.relationship_milestones,
        ] {
            for s in list {
                if seen.insert(s.as_str()) {
                    facets.push(s.clone());
                }
            }
        }
        if facets.is_empty() {
            return None;
        }

        // Phase 86 — relevance query is the assembled
        // conversation window when opt-in is engaged; otherwise
        // the bare user message (byte-identical to pre-Phase-86).
        let query_text = assemble_for(
            self.conversation_windows.as_ref(),
            session_id,
            self.recall_window_turns,
            user_message,
        )
        .unwrap_or_else(|| user_message.to_string());

        // One embed call: query first, then every facet.
        let mut inputs = Vec::with_capacity(facets.len() + 1);
        inputs.push(query_text);
        inputs.extend(facets.iter().cloned());
        let vecs = match self.provider.embed(&inputs).await {
            Ok(v) if v.len() == inputs.len() => v,
            // Embed failure / short response → byte-identical
            // fallback (base prompt unchanged).
            _ => return None,
        };

        let qvec = &vecs[0];
        let mut scored: Vec<(usize, f32)> = facets
            .iter()
            .enumerate()
            .map(|(i, _)| (i, cosine(qvec, &vecs[i + 1])))
            .filter(|(_, s)| *s >= self.min_similarity)
            .collect();
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(self.top_k);

        // Phase 97 — token-budget enforcement on the
        // post-rank, post-top_k selection. Facets are
        // already in cosine-descending order; the budget
        // walks them, dropping the lowest-priority tail
        // once the running estimate exceeds the budget.
        // With `recall_token_budget = 0` (default) this is
        // a no-op.
        if self.recall_token_budget > 0 {
            scored = crate::token_budget::apply_token_budget(
                scored,
                self.recall_token_budget,
                |(i, _)| {
                    crate::token_budget::estimate_tokens(&facets[*i])
                },
            );
        }

        let kept: HashSet<String> = scored
            .iter()
            .map(|(i, _)| facets[*i].clone())
            .collect();

        // Phase 79 (Q4a) — per-turn breadcrumb, same operator-
        // visible convention as recall / GC / backfill. The
        // structured Phase 78-surface extension is Task 6.
        eprintln!(
            "aivyx-pa persona: injected {}/{} facets",
            kept.len(),
            facets.len()
        );
        if let Some(stat) = &self.stat {
            if let Ok(mut w) = stat.write() {
                *w = Some(PersonaSelectionStat {
                    ts_secs: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0),
                    selected: kept.len(),
                    total: facets.len(),
                });
            }
        }

        let keep = |s: &str| kept.contains(s);
        Some(assemble_session_prompt_selected(
            &self.profile,
            &snapshot,
            &keep,
            &self.role_name,
            &self.role_prompt,
            self.default_skills_section.as_deref(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persona::{shared_effective_persona, EffectivePersona};
    use aivyx_llm::embedding::EmbeddingError;

    /// Maps a text to [1,0] if it contains "deploy", else
    /// [0,1]; or fails on demand. Deterministic so ranking is
    /// predictable.
    struct FakeProvider {
        fail: bool,
    }

    #[async_trait]
    impl EmbeddingProvider for FakeProvider {
        async fn embed(
            &self,
            texts: &[String],
        ) -> Result<Vec<Vec<f32>>, EmbeddingError> {
            if self.fail {
                return Err(EmbeddingError::Timeout);
            }
            // Three orthogonal buckets so an "unrelated" query
            // is genuinely orthogonal to every facet (a 2-bucket
            // fake makes the non-deploy query maximally similar
            // to the misc facets, which can't model "relevant to
            // nothing").
            Ok(texts
                .iter()
                .map(|t| {
                    let l = t.to_lowercase();
                    if l.contains("deploy") {
                        vec![1.0, 0.0, 0.0]
                    } else if l.contains("misc") {
                        vec![0.0, 1.0, 0.0]
                    } else {
                        vec![0.0, 0.0, 1.0]
                    }
                })
                .collect())
        }
        fn model(&self) -> &str {
            "fake"
        }
        fn dimensions(&self) -> usize {
            3
        }
    }

    fn profile() -> aivyx_config::Profile {
        aivyx_config::Profile::default()
    }

    fn big_persona() -> EffectivePersona {
        // 14 reducible facets (> default threshold 12); a
        // protected constraint + scalar identity.
        let mut lc: Vec<String> = (0..12)
            .map(|i| format!("misc note {i}"))
            .collect();
        lc.push("deploy runbook lives in wiki".to_string());
        lc.push("deploy window is Friday".to_string());
        EffectivePersona {
            assistant_name: Some("Ada".into()),
            operator_profile: Some("SRE".into()),
            communication_style: Some("terse".into()),
            primary_use_cases: vec![],
            behavioral_preferences: vec![],
            behavioral_constraints: vec![
                "never deploy without approval".into(),
            ],
            learned_context: lc,
            communication_adaptations: vec![],
            character_traits: vec![],
            relationship_milestones: vec![],
            learned_skills: Vec::new(),
            // Phase 118 — operator-staged refinement categories;
            // empty in this Phase 79 reduction-test fixture.
            profile_hints: Vec::new(),
            role_drafts: Vec::new(),
        }
    }

    fn refiner(
        persona: EffectivePersona,
        fail: bool,
        size_threshold: usize,
    ) -> PersonaContextRefiner {
        PersonaContextRefiner::new(
            profile(),
            shared_effective_persona(persona),
            "default".into(),
            "ROLE PROMPT".into(),
            Arc::new(FakeProvider { fail }),
            size_threshold,
            12,
            0.20,
        )
    }

    #[tokio::test]
    async fn small_persona_falls_back_to_none() {
        // 2 facets, threshold 12 → None (full inject).
        let p = EffectivePersona {
            learned_context: vec!["a".into(), "b".into()],
            ..EffectivePersona::default()
        };
        let out = refiner(p, false, 12)
            .refine("anything", aivyx_core::SessionId::new(), "")
            .await;
        assert!(out.is_none());
    }

    #[tokio::test]
    async fn embed_failure_falls_back_to_none() {
        let out = refiner(big_persona(), true, 12)
            .refine("how do I deploy", aivyx_core::SessionId::new(), "")
            .await;
        assert!(out.is_none());
    }

    #[tokio::test]
    async fn selects_relevant_facets_and_keeps_core_and_constraints() {
        let out = refiner(big_persona(), false, 12)
            .refine("how do I deploy", aivyx_core::SessionId::new(), "")
            .await
            .expect("large persona + ok embed → Some");

        // The two "deploy" facets are relevant and kept.
        assert!(out.contains("deploy runbook lives in wiki"));
        assert!(out.contains("deploy window is Friday"));
        // A non-relevant soft facet is dropped.
        assert!(!out.contains("misc note 0"));
        // Invariant: protected constraint + scalar identity are
        // ALWAYS present even though they were never "selected".
        assert!(out.contains("never deploy without approval"));
        assert!(out.contains("Ada"));
    }

    #[tokio::test]
    async fn unrelated_query_strips_to_core_only() {
        // No facet matches; large Soul still bounds to just the
        // always-on core + constraints (the adaptive point).
        let out = refiner(big_persona(), false, 12)
            .refine("tell me a joke", aivyx_core::SessionId::new(), "")
            .await
            .expect("Some");
        assert!(!out.contains("deploy runbook lives in wiki"));
        assert!(!out.contains("misc note 3"));
        // Core/constraint still there.
        assert!(out.contains("never deploy without approval"));
    }

    #[tokio::test]
    async fn empty_persona_is_none() {
        let out = refiner(EffectivePersona::default(), false, 0)
            .refine("hi", aivyx_core::SessionId::new(), "")
            .await;
        // threshold 0 but zero facets → still None (nothing to
        // select).
        assert!(out.is_none());
    }

    // ---- Phase 86 — conversational-window relevance ------------

    /// Records every `embed()` input so a test can assert what
    /// query string the refiner actually used to score facets —
    /// that's the only observable behavior change Phase 86
    /// introduces in this provider.
    struct RecordingProvider {
        seen: std::sync::Mutex<Vec<String>>,
    }

    #[async_trait]
    impl EmbeddingProvider for RecordingProvider {
        async fn embed(
            &self,
            texts: &[String],
        ) -> Result<Vec<Vec<f32>>, EmbeddingError> {
            self.seen
                .lock()
                .unwrap()
                .extend(texts.iter().cloned());
            // All-equal vectors → ranking is irrelevant; the
            // test only asserts which query was embedded.
            Ok(texts.iter().map(|_| vec![1.0, 0.0, 0.0]).collect())
        }
        fn model(&self) -> &str {
            "recording"
        }
        fn dimensions(&self) -> usize {
            3
        }
    }

    fn refiner_with_recorder(
        provider: Arc<RecordingProvider>,
    ) -> PersonaContextRefiner {
        PersonaContextRefiner::new(
            profile(),
            shared_effective_persona(big_persona()),
            "default".into(),
            "ROLE PROMPT".into(),
            provider as Arc<dyn EmbeddingProvider>,
            12,
            12,
            0.20,
        )
    }

    /// Phase 86 — opt-in engaged: the refiner must embed the
    /// assembled window (prior turns + current last) as facet 0
    /// in the single batched `embed()` call, NOT the bare user
    /// message.
    #[tokio::test]
    async fn refine_embeds_assembled_window_when_opt_in_engaged() {
        use crate::conversation_window::{
            record_turn, shared_conversation_windows,
        };

        let provider = Arc::new(RecordingProvider {
            seen: std::sync::Mutex::new(Vec::new()),
        });
        let windows = shared_conversation_windows();
        let s = aivyx_core::SessionId::new();
        record_turn(
            &windows,
            s,
            "remind me the deploy runbook",
            "it lives in the wiki",
        );

        let r = refiner_with_recorder(Arc::clone(&provider))
            .with_conversation_windows(windows.clone(), 3);
        let _ = r.refine("how do I deploy", s, "").await;

        let seen = provider.seen.lock().unwrap().clone();
        let query = seen
            .iter()
            .find(|t| t.contains("how do I deploy"))
            .expect("the query embed must have happened");
        assert!(
            query.contains("remind me the deploy runbook"),
            "assembled window must include prior user turn: \
             {query}"
        );
        assert!(
            query.contains("it lives in the wiki"),
            "assembled window must include prior assistant \
             turn: {query}"
        );
        assert!(
            query.ends_with("\nuser: how do I deploy"),
            "current message must land LAST and labelled: {query}"
        );
    }

    /// Phase 86 — every fallback case must embed the *bare*
    /// current message verbatim (byte-identical to pre-Phase-86).
    /// Mirrors the matrix in `memory_recall.rs` so a regression
    /// on either provider is loud.
    #[tokio::test]
    async fn refine_falls_through_to_bare_query_in_every_fallback() {
        use crate::conversation_window::{
            record_turn, shared_conversation_windows,
        };

        for case in [
            "no_handle",
            "floor_one",
            "unknown_session",
            "empty_window",
        ] {
            let provider = Arc::new(RecordingProvider {
                seen: std::sync::Mutex::new(Vec::new()),
            });
            let mut r = refiner_with_recorder(Arc::clone(&provider));
            let s = aivyx_core::SessionId::new();
            match case {
                "no_handle" => {}
                "floor_one" => {
                    let w = shared_conversation_windows();
                    record_turn(&w, s, "prior u", "prior a");
                    r = r.with_conversation_windows(w, 1);
                }
                "unknown_session" => {
                    let w = shared_conversation_windows();
                    record_turn(
                        &w,
                        aivyx_core::SessionId::new(),
                        "prior u",
                        "prior a",
                    );
                    r = r.with_conversation_windows(w, 5);
                }
                "empty_window" => {
                    r = r.with_conversation_windows(
                        shared_conversation_windows(),
                        5,
                    );
                }
                _ => unreachable!(),
            }

            let _ = r.refine("bare message", s, "").await;
            let seen = provider.seen.lock().unwrap().clone();
            assert_eq!(
                seen.first(),
                Some(&"bare message".to_string()),
                "{case}: query input must be the bare current \
                 message (byte-identical to pre-Phase-86)"
            );
        }
    }

    // ---- Phase 90 — heuristic recall gate ----------------------

    /// A gated turn (trimmed user message shorter than the
    /// threshold) short-circuits before any embed call: the
    /// refiner returns `None` (planner uses the full Persona
    /// base prompt), and the `RecordingProvider` records zero
    /// inputs.
    #[tokio::test]
    async fn refine_gate_short_circuits_before_embed_on_noise_turn(
    ) {
        let provider = Arc::new(RecordingProvider {
            seen: std::sync::Mutex::new(Vec::new()),
        });
        let r = refiner_with_recorder(Arc::clone(&provider))
            .with_recall_gate(4);
        let out =
            r.refine("ok", aivyx_core::SessionId::new(), "").await;
        assert!(out.is_none(), "gated turn returns None");
        assert!(
            provider.seen.lock().unwrap().is_empty(),
            "gated turn must not call embed"
        );
    }

    /// An ungated turn proceeds to the embed normally.
    #[tokio::test]
    async fn refine_gate_passes_when_message_meets_threshold() {
        let provider = Arc::new(RecordingProvider {
            seen: std::sync::Mutex::new(Vec::new()),
        });
        let r = refiner_with_recorder(Arc::clone(&provider))
            .with_recall_gate(4);
        // The big_persona fixture has 14 reducible facets, so
        // the size_threshold (12) is cleared and refine
        // proceeds to the embed when the gate doesn't fire.
        let _ = r
            .refine(
                "how do I deploy",
                aivyx_core::SessionId::new(),
                "",
            )
            .await;
        let seen = provider.seen.lock().unwrap().clone();
        assert!(
            !seen.is_empty(),
            "ungated turn proceeds to the embed batch"
        );
        assert!(
            seen.iter().any(|s| s.contains("how do I deploy")),
            "the embed batch includes the user message: \
             {seen:?}"
        );
    }

    /// `recall_gate_min_chars = 0` (default) is the opt-out:
    /// a would-be-gated message flows through normally —
    /// byte-identical to pre-Phase-90.
    #[tokio::test]
    async fn refine_gate_zero_min_chars_is_byte_identical_to_pre_phase_90(
    ) {
        let provider = Arc::new(RecordingProvider {
            seen: std::sync::Mutex::new(Vec::new()),
        });
        // No `with_recall_gate` call — default `0`.
        let r = refiner_with_recorder(Arc::clone(&provider));
        // big_persona has 14 reducible facets → clears the
        // size_threshold → refine runs the embed batch.
        let _ = r
            .refine("ok", aivyx_core::SessionId::new(), "")
            .await;
        let seen = provider.seen.lock().unwrap().clone();
        assert!(
            !seen.is_empty(),
            "with the gate disabled the embed batch fires \
             even on a short message (pre-Phase-90)"
        );
        assert_eq!(
            seen.first(),
            Some(&"ok".to_string()),
            "the bare short message is the query (pre-Phase-90)"
        );
    }

    /// Phase 97 — with `recall_token_budget = 0` (default),
    /// adaptive Persona selection is byte-identical to
    /// pre-Phase-97: the existing relevant facets are
    /// selected normally.
    #[tokio::test]
    async fn refine_token_budget_zero_passes_through() {
        let out = refiner(big_persona(), false, 12)
            .refine("how do I deploy", aivyx_core::SessionId::new(), "")
            .await
            .expect("large persona + ok embed → Some");
        // Both deploy-relevant facets selected as before.
        assert!(out.contains("deploy runbook lives in wiki"));
        assert!(out.contains("deploy window is Friday"));
    }

    /// Phase 97 — with `recall_token_budget` set tighter
    /// than the sum of selected facets' estimated tokens,
    /// the lowest-priority facets drop from the selection.
    /// Both "deploy" facets are cosine-relevant; the
    /// budget keeps only the top one.
    #[tokio::test]
    async fn refine_token_budget_drops_lowest_priority_facet() {
        // Both deploy facets estimate ~7 tokens each. A
        // budget of 8 fits only the top-ranked one.
        let refiner = refiner(big_persona(), false, 12)
            .with_recall_token_budget(8);

        let out = refiner
            .refine("how do I deploy", aivyx_core::SessionId::new(), "")
            .await
            .expect("large persona + ok embed → Some");

        // Exactly one of the two deploy-relevant
        // soft-facets should survive the budget. (Cosine
        // ranking determines which; both are tied in our
        // fixture, so we just count that at most one
        // appears.)
        let kept_runbook =
            out.contains("deploy runbook lives in wiki");
        let kept_window =
            out.contains("deploy window is Friday");
        assert!(
            kept_runbook ^ kept_window,
            "exactly one of the two facets should survive \
             the budget; got runbook={kept_runbook} \
             window={kept_window}"
        );
        // Protected constraint + identity scalar still
        // ALWAYS present — the budget only trims
        // soft-facet selection, never the protected core.
        assert!(out.contains("never deploy without approval"));
        assert!(out.contains("Ada"));
    }

    /// Aivyx-Skills Part 3 — a `## Default skills` section
    /// attached via `with_default_skills_section` must survive
    /// a refined (adaptive-selection) prompt, not just the
    /// unrefined base prompt. Guards against the refiner's
    /// from-scratch rebuild silently dropping the section.
    #[tokio::test]
    async fn refine_keeps_default_skills_section_when_attached() {
        let r = refiner(big_persona(), false, 12)
            .with_default_skills_section(
                "## Default skills\n\n- systematic-debugging: \
                 use when investigating a bug\n"
                    .to_string(),
            );
        let out = r
            .refine("how do I deploy", aivyx_core::SessionId::new(), "")
            .await
            .expect("large persona + ok embed → Some");

        assert!(
            out.contains("## Default skills"),
            "the Default skills section must survive a refined \
             prompt, not just the unrefined base: {out}"
        );
        assert!(out.contains("systematic-debugging"));
        // Sanity: the rest of the refiner's normal behavior is
        // unaffected by attaching the section.
        assert!(out.contains("deploy runbook lives in wiki"));
    }

    /// Without `with_default_skills_section` (the default,
    /// `None`), the refined prompt is byte-identical to
    /// pre-Aivyx-Skills-Part-3 output — no `## Default skills`
    /// heading appears.
    #[tokio::test]
    async fn refine_omits_default_skills_section_when_not_attached() {
        let out = refiner(big_persona(), false, 12)
            .refine("how do I deploy", aivyx_core::SessionId::new(), "")
            .await
            .expect("large persona + ok embed → Some");

        assert!(!out.contains("## Default skills"));
    }
}
