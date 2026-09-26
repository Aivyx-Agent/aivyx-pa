//! Phase 87 — pattern-driven Persona proposals.
//!
//! The proposal-side consumption of the Phase 83 co-occurrence
//! ledger, symmetric with Phase 84's recall-side consumption.
//! When a durable, decayed-affined pair `(A, B)` clears
//! `min_affinity` + `min_samples` AND both endpoints are
//! independently helpful (Phase 82 ledger >=
//! `min_topic_helpfulness` — Q1a's conservative double-gate),
//! the reflection cron asks the existing reflection LLM to
//! phrase a `learned_context` facet (Q2b) and files it as a
//! Pending [`PersonaProposal`] through the existing Phase 70
//! proposal chain.
//!
//! Everything downstream is unchanged: propose-only (the
//! operator approves / rejects via Web UI or CLI), edit-then-
//! approve, `Revert`-able, core-protected. Phase 87 only adds
//! a new *source* of proposals — never a new way of resolving
//! them.

use std::collections::HashSet;

// moved to the wasm-clean aivyx-ipc crate (Chapter M.2d-2); re-exported here.
pub use aivyx_ipc::insights::{PersonaConsolidationStat};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;

use aivyx_config::PersonaConsolidationConfig;
use aivyx_core::CancellationToken;
use aivyx_llm::{
    LlmMessage, LlmProvider, LlmRequest, LlmStepEnd,
};

use crate::cooccurrence_ledger::PersistentCooccurrenceLedger;
use crate::helpfulness_ledger::PersistentHelpfulnessLedger;
use crate::persona::{
    PersonaDeltaCategory, PersonaDeltaOp, ProposedPersonaDelta,
};
use crate::persona_proposal::PersistentPersonaProposalLog;

/// Hard cap on how many ledger pairs the selector inspects
/// each cycle. The actual filing cap is the operator-tunable
/// `max_proposals_per_cycle`; this is the *scan* bound that
/// keeps a giant ledger from making the pass O(n) on every
/// reflection cron.
pub const CONSOLIDATION_SCAN_TOP_K: usize = 64;

/// One pair the selector kept after the conservative gates.
/// `helpfulness_min` is the lesser of the two endpoint scores
/// — the limiting factor for the "both helpful" rule, used
/// for ranking and the proposal `reason` line.
#[derive(Debug, Clone, PartialEq)]
pub struct ConsolidationCandidate {
    pub a: String,
    pub b: String,
    pub affinity: f32,
    pub samples: u32,
    pub helpfulness_min: f32,
}


/// Shared handle the consolidation pass writes (per cycle)
/// and `GetLearningInsights` reads. `None` inside = no cycle
/// has run yet this daemon lifetime.
pub type SharedPersonaConsolidationStat =
    Arc<RwLock<Option<PersonaConsolidationStat>>>;

/// Construct an empty shared consolidation-stat handle.
pub fn shared_persona_consolidation_stat()
-> SharedPersonaConsolidationStat {
    Arc::new(RwLock::new(None))
}

/// Phase 87 Q2b — the LLM-summarized facet seam. The pass
/// hands each surviving `(A, B)` to a phraser; the production
/// impl in `bin/aivyx` adapts the existing
/// `aivyx_llm::LlmProvider`. Tests substitute a recording
/// fake with deterministic output.
///
/// Returning `None` is the per-candidate degenerate path
/// (LLM transport hiccup, parse failure, refusal). The pass
/// skips that candidate; *cycle-wide* unavailability is
/// detected by every survivor's phraser returning `None`.
#[async_trait]
pub trait PairPhraser: Send + Sync {
    async fn phrase(
        &self,
        a: &str,
        b: &str,
    ) -> Option<String>;
}

/// Build the canonical proposal id for a consolidation
/// candidate. Pair is sorted alphabetically so `(A, B)` and
/// `(B, A)` map to one id — the dedup invariant relies on
/// this.
pub fn pair_proposal_id(a: &str, b: &str) -> String {
    let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
    format!("consolidate-pair:{lo}+{hi}")
}

/// Phase 92 — parse an existing `consolidate-pair:{lo}+{hi}`
/// proposal id back into its `(lo, hi)` topic pair. Returns
/// `None` for any id that isn't in the canonical form (e.g.
/// `recall-fb:{topic}` or arbitrary operator strings).
pub fn parse_pair_proposal_id(
    proposal_id: &str,
) -> Option<(String, String)> {
    let rest = proposal_id.strip_prefix("consolidate-pair:")?;
    let (lo, hi) = rest.split_once('+')?;
    if lo.is_empty() || hi.is_empty() {
        return None;
    }
    Some((lo.to_string(), hi.to_string()))
}

/// Phase 92 — one detected supersession opportunity. The
/// reflection-cron pass files two linked proposals from each
/// candidate: a `RemoveList` for the old facet (referenced
/// by `old_proposal_id` + `old_facet_value`) and an
/// `AppendList` for the new facet (the new pair phrased by
/// the Phase 87 `PairPhraser`).
#[derive(Debug, Clone, PartialEq)]
pub struct SupersessionCandidate {
    /// The original `consolidate-pair:{A}+{B}` proposal id
    /// the operator approved into the chain.
    pub old_proposal_id: String,
    /// The `(A, B)` topic pair the operator-approved facet
    /// was built from.
    pub old_pair: (String, String),
    /// The current `AppendList` value of the approved facet
    /// (needed for the `RemoveList`-side payload).
    pub old_facet_value: String,
    /// The new `(A, C)` topic pair (sharing one endpoint
    /// with `old_pair`) that has strengthened past the
    /// Phase 87 floor.
    pub new_pair: (String, String),
    /// The canonical `consolidate-pair:{lo}+{hi}` id that
    /// the new pair will be filed under (the lexically
    /// sorted form).
    pub new_proposal_id: String,
    /// The new pair's decayed affinity at detection time —
    /// for the operator-visible `reason` line.
    pub new_affinity: f32,
    /// The new pair's smaller endpoint helpfulness — same
    /// shape as `ConsolidationCandidate.helpfulness_min`,
    /// for the proposal `reason` line.
    pub new_helpfulness_min: f32,
}

/// Phase 92 Task 3 — the pure supersession detector. Walks
/// applied `consolidate-pair:` facets on the Persona chain;
/// for each whose underlying pair has decayed below the
/// `[persona_lifecycle].decay_pair_below_affinity` floor,
/// scans the Phase 83 ledger for a new pair `(A, C)` or
/// `(B, C)` sharing one endpoint that strengthens past
/// `min_affinity`, has at least `min_samples` observations,
/// and whose endpoints clear `min_topic_helpfulness` on the
/// Phase 82 ledger. The strongest such match (by
/// `(affinity desc, helpfulness_min desc, (lo, hi) asc)`)
/// per old facet is emitted as a candidate.
///
/// Deterministic; best-effort (any ledger / chain error
/// drops that candidate; the cycle continues).
///
/// Inputs:
/// - `applied_pair_facets`: every applied (not removed,
///   not rejected) `consolidate-pair:` facet, derived from
///   the Persona chain. Caller flattens to
///   `(proposal_id, facet_value, (A, B))` triples — keeps
///   this function chain-agnostic.
/// - The two ledgers + the proposal log.
/// - `config`: the Phase 87 block; we read the Phase 87
///   construction floors here (the Phase 88 decay floor
///   lives on `[persona_lifecycle]`; we pass it in as
///   `pair_below_affinity`).
pub async fn detect_supersession(
    applied_pair_facets: &[(String, String, (String, String))],
    co_ledger: &PersistentCooccurrenceLedger,
    helpfulness: &PersistentHelpfulnessLedger,
    proposal_log: &PersistentPersonaProposalLog,
    config: &PersonaConsolidationConfig,
    pair_below_affinity: f32,
    now_secs: u64,
) -> Vec<SupersessionCandidate> {
    if !config.enabled || !config.enable_supersession {
        return Vec::new();
    }
    let ranked = match co_ledger.ranked(now_secs).await {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    let mut out: Vec<SupersessionCandidate> = Vec::new();
    // Track new-pair canonical ids already proposed (Phase 70
    // dedup) so we don't pick a new pair that's already in
    // the chain — the Phase 87 selector would also have
    // de-duplicated, but the supersession detector runs
    // BEFORE that selector and shouldn't introduce a row
    // that the standard path would have skipped.
    let mut used_new_ids: std::collections::HashSet<String> =
        std::collections::HashSet::new();

    for (old_id, old_value, (a, b)) in applied_pair_facets {
        // Gate 1: the old pair must have decayed below the
        // Phase 88 floor — otherwise nothing is being
        // superseded.
        let old_affinity = match co_ledger
            .pair_score(a, b, now_secs)
            .await
        {
            Ok(Some(entry)) => entry.ewma_score,
            // No ledger entry → the pair has been pruned
            // (Phase 83's 90-day untouched rule) which counts
            // as fully decayed; allow supersession.
            Ok(None) => 0.0,
            Err(_) => continue,
        };
        if old_affinity >= pair_below_affinity {
            continue;
        }

        // Gate 2: scan the ranked ledger for the strongest
        // shared-endpoint candidate that clears the Phase 87
        // construction floor + the helpfulness gate.
        let mut best: Option<SupersessionCandidate> = None;
        for (lo, hi, entry) in &ranked {
            if entry.ewma_score < config.min_affinity {
                // ranked is score-descending → nothing below
                // here will qualify; stop scanning.
                break;
            }
            if entry.samples < config.min_samples {
                continue;
            }
            // Shared-endpoint condition (Q1a): one of (lo,
            // hi) must equal one of (A, B) AND the other
            // endpoint must differ (otherwise it's the same
            // pair — not a supersession).
            let (new_a, new_c) = if lo == a {
                if hi == b {
                    continue; // same pair
                }
                (lo, hi)
            } else if lo == b {
                if hi == a {
                    continue; // same pair, swapped
                }
                (lo, hi)
            } else if hi == a {
                if lo == b {
                    continue;
                }
                (hi, lo)
            } else if hi == b {
                if lo == a {
                    continue;
                }
                (hi, lo)
            } else {
                continue;
            };

            // Helpfulness double-gate on both endpoints
            // (Q1a from Phase 87 — same posture).
            let sa = helpfulness
                .topic_score(new_a, now_secs)
                .await
                .ok()
                .flatten();
            let sc = helpfulness
                .topic_score(new_c, now_secs)
                .await
                .ok()
                .flatten();
            let (Some(ea), Some(ec)) = (sa, sc) else {
                continue;
            };
            if ea.ewma_score < config.min_topic_helpfulness
                || ec.ewma_score < config.min_topic_helpfulness
            {
                continue;
            }
            let helpfulness_min =
                ea.ewma_score.min(ec.ewma_score);

            // Dedup: the proposal id must not already exist
            // in the chain (any status).
            let new_id = pair_proposal_id(new_a, new_c);
            if used_new_ids.contains(&new_id) {
                continue;
            }
            if proposal_log.get(&new_id).is_some() {
                continue;
            }

            // Stable ordering: prefer higher affinity, then
            // higher helpfulness-min, then lexically-sorted
            // (lo, hi). `ranked` is already affinity-desc so
            // the FIRST shared-endpoint match is the
            // strongest by affinity; we keep `best` and only
            // replace if a later one has strictly higher
            // affinity (which can't happen since `ranked` is
            // sorted) OR same affinity but better
            // helpfulness-min.
            let candidate = SupersessionCandidate {
                old_proposal_id: old_id.clone(),
                old_pair: (a.clone(), b.clone()),
                old_facet_value: old_value.clone(),
                new_pair: (new_a.clone(), new_c.clone()),
                new_proposal_id: new_id,
                new_affinity: entry.ewma_score,
                new_helpfulness_min: helpfulness_min,
            };
            best = match best {
                None => Some(candidate),
                Some(prev) => {
                    if (candidate.new_affinity, candidate.new_helpfulness_min)
                        > (prev.new_affinity, prev.new_helpfulness_min)
                    {
                        Some(candidate)
                    } else {
                        Some(prev)
                    }
                }
            };
        }
        if let Some(c) = best {
            used_new_ids.insert(c.new_proposal_id.clone());
            out.push(c);
        }
    }
    out
}

/// Phase 87 Task 3 — the pure-ish selector. Reads the two
/// ledgers + the proposal log, applies the Q1a conservative
/// double-gate, dedups against the chain, sorts, caps. Output
/// is ordered (highest affinity first, helpfulness-min as
/// tiebreaker) — the operator sees the strongest pattern
/// proposals first.
///
/// All errors are best-effort: a ledger / chain failure
/// silently drops that candidate; the cycle continues. The
/// pass's "0 filed" is always a valid outcome.
pub async fn select_candidates(
    co_ledger: &PersistentCooccurrenceLedger,
    helpfulness: &PersistentHelpfulnessLedger,
    proposal_log: &PersistentPersonaProposalLog,
    config: &PersonaConsolidationConfig,
    now_secs: u64,
) -> Vec<ConsolidationCandidate> {
    if !config.enabled {
        return Vec::new();
    }
    let ranked = match co_ledger.ranked(now_secs).await {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    // The cap that bounds the scan; the *filing* cap further
    // narrows below (after gates + dedup).
    let scan_cap = CONSOLIDATION_SCAN_TOP_K;

    let mut out: Vec<ConsolidationCandidate> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for (a, b, entry) in ranked.into_iter().take(scan_cap) {
        if entry.ewma_score < config.min_affinity {
            // `ranked` is score-descending — everything below
            // this is also below the floor.
            break;
        }
        if entry.samples < config.min_samples {
            continue;
        }
        // Dedup: a pair already proposed (in ANY status —
        // Pending / Approved / Rejected / Superseded) is
        // never re-filed. Phase 70 / 85 dedup invariant: the
        // assistant must not nag about its own identity.
        let pid = pair_proposal_id(&a, &b);
        if !seen.insert(pid.clone()) {
            continue;
        }
        if proposal_log.get(&pid).is_some() {
            continue;
        }
        let sa = helpfulness
            .topic_score(&a, now_secs)
            .await
            .ok()
            .flatten();
        let sb = helpfulness
            .topic_score(&b, now_secs)
            .await
            .ok()
            .flatten();
        // Q1a — both endpoints must independently be
        // helpfulness-floor-or-better. An unseen topic
        // (entry == None) is treated as *unknown*, not
        // helpful: identity is never proposed without
        // confirming evidence on both sides.
        let (Some(ea), Some(eb)) = (sa, sb) else {
            continue;
        };
        if ea.ewma_score < config.min_topic_helpfulness
            || eb.ewma_score < config.min_topic_helpfulness
        {
            continue;
        }
        let helpfulness_min = ea.ewma_score.min(eb.ewma_score);
        out.push(ConsolidationCandidate {
            a,
            b,
            affinity: entry.ewma_score,
            samples: entry.samples,
            helpfulness_min,
        });
    }
    // Stable descending order: highest affinity first; ties
    // broken by stronger min-helpfulness; ties beyond that
    // fall back to (a, b) so output is fully deterministic.
    out.sort_by(|x, y| {
        y.affinity
            .partial_cmp(&x.affinity)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| {
                y.helpfulness_min
                    .partial_cmp(&x.helpfulness_min)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| (&x.a, &x.b).cmp(&(&y.a, &y.b)))
    });
    out.truncate(config.max_proposals_per_cycle as usize);
    out
}

/// Phase 87 Task 3 — the actuator. For each candidate the
/// selector returned, asks the `PairPhraser` for a one-line
/// `learned_context` facet, and files it as a Pending
/// `PersonaProposal` through the existing Phase 70 chain with
/// `proposal_id = "consolidate-pair:{lo}+{hi}"`.
///
/// Returns the cycle's [`PersonaConsolidationStat`] — the
/// caller writes it to the shared handle so the Phase 78
/// surface can render it. Best-effort throughout: a per-
/// candidate phrasing failure skips that candidate; an append
/// failure is logged and skipped; a cycle where *every*
/// survivor's phrasing failed records `llm_unavailable =
/// true` so a quiet "0 filed" cycle stays distinguishable
/// from an LLM outage.
pub async fn consolidate(
    candidates: Vec<ConsolidationCandidate>,
    phraser: &dyn PairPhraser,
    proposal_log: &PersistentPersonaProposalLog,
    source_label: &str,
    now_ms: u64,
) -> PersonaConsolidationStat {
    let mut filed = 0u32;
    let mut phrase_failures = 0u32;
    let mut pairs: Vec<(String, String)> = Vec::new();
    let attempted = candidates.len();

    for cand in candidates {
        let pid = pair_proposal_id(&cand.a, &cand.b);
        let Some(value) = phraser.phrase(&cand.a, &cand.b).await
        else {
            phrase_failures += 1;
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            phrase_failures += 1;
            continue;
        }
        let reason = format!(
            "co-occurrence pair `{a}` + `{b}` — \
             decayed affinity {aff:.2} over {samples} \
             observation(s); both topics helpful (min \
             score {hmin:.2})",
            a = cand.a,
            b = cand.b,
            aff = cand.affinity,
            samples = cand.samples,
            hmin = cand.helpfulness_min,
        );
        let op = ProposedPersonaDelta {
            category: PersonaDeltaCategory::LearnedContext,
            op: PersonaDeltaOp::AppendList {
                value: value.to_string(),
            },
            reason: Some(reason),
            // Phase 92 — non-supersession standard consolidation
            // proposal; Task 4's supersession path threads
            // `Some(other_proposal_id)` here when filing the
            // linked pair.
            supersedes_proposal_id: None,
        };
        match proposal_log
            .append_pending(
                pid.clone(),
                now_ms,
                source_label.to_string(),
                op,
            )
            .await
        {
            Ok(_) => {
                filed += 1;
                pairs.push((cand.a, cand.b));
            }
            Err(e) => {
                eprintln!(
                    "aivyx-pa persona-consolidation: \
                     append_pending failed for {pid}: {e}",
                );
            }
        }
    }

    let llm_unavailable = attempted > 0 && filed == 0
        && phrase_failures == attempted as u32;

    PersonaConsolidationStat {
        ts_secs: now_ms / 1000,
        filed,
        deduped: 0, // selector handled dedup before us
        skipped_unhelpful: 0, // selector handled the gate
        llm_unavailable,
        pairs,
        // Phase 92 — `consolidate(...)` only handles standard
        // construction; the supersession-event count is
        // tallied by the reflection-pass wrapper (Task 4)
        // before this function is called for the standard
        // remainder. Default `0` here.
        superseded: 0,
    }
}

/// Hard cap on the LLM completion the production phraser
/// asks for. The output is a single sentence — typical Phase
/// 87 facets fit in ~30 tokens, so 128 leaves headroom
/// without inviting verbose paragraphs.
const PHRASE_MAX_TOKENS: u32 = 128;

/// Fixed system prompt for the consolidation phrasing call.
/// Short, structural, conservative: the model writes ONE
/// short factual statement, no instructions, no markdown.
const PHRASE_SYSTEM_PROMPT: &str = "You are summarizing a \
durable cross-session pattern about how the operator works. \
Write exactly ONE short factual statement (under 30 words, \
plain prose, no markdown, no list, no quotes) describing the \
relationship the two topics share in the operator's work. \
Refer to the operator in the second person. Do not invent \
specifics beyond what the topic names imply.";

/// Phase 87 Q2b — production `PairPhraser` that delegates to
/// the existing `aivyx_llm::LlmProvider`. Constructed by the
/// binary with the same provider + model the agent uses;
/// failures map to `None` (per-candidate skip — the actuator
/// stays best-effort).
pub struct LlmPairPhraser {
    provider: Arc<dyn LlmProvider>,
    model: String,
}

impl LlmPairPhraser {
    pub fn new(
        provider: Arc<dyn LlmProvider>,
        model: String,
    ) -> Self {
        Self { provider, model }
    }
}

#[async_trait]
impl PairPhraser for LlmPairPhraser {
    async fn phrase(
        &self,
        a: &str,
        b: &str,
    ) -> Option<String> {
        let user = format!(
            "Topic A: `{a}`\nTopic B: `{b}`\n\nWrite one short \
             factual statement (under 30 words) describing the \
             relationship these two topics share in the \
             operator's work. Refer to the operator in the \
             second person."
        );
        let messages = vec![LlmMessage::user_text(user)];
        let request = LlmRequest {
            model: &self.model,
            system: Some(PHRASE_SYSTEM_PROMPT),
            messages: &messages,
            tools: &[],
            max_tokens: PHRASE_MAX_TOKENS,
            temperature: Some(0.3),
        id_slot: None,
        slot_hint: None,
        route: None,
        };
        // A short, non-cancellable token: the consolidation
        // pass is on a reflection cron, not an interactive
        // turn. If the operator restarts the daemon mid-pass
        // the whole future is dropped; we don't need to thread
        // an outer token through.
        let cancel = CancellationToken::new();
        let mut stream =
            self.provider.chat_stream(request, &cancel).await.ok()?;
        // Drain the event stream (we ignore mid-stream chunks
        // — the actuator only needs the final assembled text).
        while let Ok(Some(_)) = stream.next_event().await {}
        match stream.finish().await.ok()? {
            LlmStepEnd::FinalMessage { text, .. } => {
                let trimmed = text.trim().to_string();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed)
                }
            }
            // The phrasing call is text-only (no tools); a
            // tool-call response is a provider misbehavior →
            // skip the candidate.
            LlmStepEnd::ToolCalls { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_crypto::MasterKey;
    use aivyx_storage::{
        KeyDomain, RedbStorage, Storage, StorageConfig,
    };
    use std::path::{Path, PathBuf};
    use uuid::Uuid;

    fn tmp_dir(tag: &str) -> PathBuf {
        let base =
            std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into());
        let dir = PathBuf::from(base)
            .join(format!("aivyx-pc-{tag}-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    async fn open_store(dir: &Path, seed: u8) -> Arc<dyn Storage> {
        RedbStorage::open(
            StorageConfig::new(dir.join("store.redb")),
            MasterKey::from_raw([seed; 32]),
        )
        .await
        .unwrap()
    }

    fn cfg(enabled: bool) -> PersonaConsolidationConfig {
        PersonaConsolidationConfig {
            enabled,
            min_affinity: 1.0,
            min_samples: 2,
            min_topic_helpfulness: 0.0,
            max_proposals_per_cycle: 3,
            enable_supersession: false,
        }
    }

    /// A deterministic phraser that always produces a
    /// predictable sentence, so the actuator tests assert on
    /// what was filed, not on LLM behavior.
    struct OkPhraser;
    #[async_trait]
    impl PairPhraser for OkPhraser {
        async fn phrase(
            &self,
            a: &str,
            b: &str,
        ) -> Option<String> {
            Some(format!(
                "You consistently work with `{a}` and `{b}` \
                 together."
            ))
        }
    }

    /// A phraser that ALWAYS returns None — simulates a
    /// cycle-wide LLM outage.
    struct FailPhraser;
    #[async_trait]
    impl PairPhraser for FailPhraser {
        async fn phrase(
            &self,
            _a: &str,
            _b: &str,
        ) -> Option<String> {
            None
        }
    }

    #[test]
    fn pair_proposal_id_is_alphabetic_canonical() {
        assert_eq!(
            pair_proposal_id("deploy", "rollback"),
            "consolidate-pair:deploy+rollback"
        );
        // Order-independent: `(a, b)` and `(b, a)` collapse.
        assert_eq!(
            pair_proposal_id("rollback", "deploy"),
            "consolidate-pair:deploy+rollback"
        );
    }

    /// Phase 92 — `parse_pair_proposal_id` is the inverse of
    /// `pair_proposal_id` for the canonical shape; rejects
    /// every non-canonical id (recall-fb provenance, free-
    /// form operator strings, malformed prefixes).
    #[test]
    fn parse_pair_proposal_id_round_trips_and_rejects_malformed() {
        assert_eq!(
            parse_pair_proposal_id(
                "consolidate-pair:deploy+rollback"
            ),
            Some(("deploy".into(), "rollback".into()))
        );
        // Reject every non-matching prefix.
        assert_eq!(
            parse_pair_proposal_id("recall-fb:deploy"),
            None
        );
        assert_eq!(parse_pair_proposal_id("seed"), None);
        // Reject malformed (missing separator / empty halves).
        assert_eq!(
            parse_pair_proposal_id("consolidate-pair:deploy"),
            None
        );
        assert_eq!(
            parse_pair_proposal_id("consolidate-pair:+rollback"),
            None
        );
        assert_eq!(
            parse_pair_proposal_id("consolidate-pair:deploy+"),
            None
        );
    }

    #[tokio::test]
    async fn selector_filters_floor_samples_and_helpfulness() {
        let dir = tmp_dir("filter");
        let store = open_store(&dir, 87).await;
        let cooc = PersistentCooccurrenceLedger::new(
            store.domain(KeyDomain::CooccurrenceLedger),
        );
        let help = PersistentHelpfulnessLedger::new(
            store.domain(KeyDomain::HelpfulnessLedger),
        );
        let plog = PersistentPersonaProposalLog::open(
            store.domain(KeyDomain::PersonaProposals),
            b"consolidation-test-key".to_vec(),
        )
        .await
        .unwrap();

        // Three pairs covering each rejection arm:
        //   (a, b) — affinity 5, samples 5, BOTH helpful → keep
        //   (c, d) — affinity 5, samples 1   (sub-samples)  → drop
        //   (e, f) — affinity 5, samples 5, ONE unhelpful   → drop
        //   (g, h) — affinity 0.5, samples 5  (sub-floor)   → drop
        let now = 1_000_000u64;
        // ranked() is score-desc; we want all 4 visible to the
        // selector but each individually rejected for a single
        // distinct reason.
        cooc.record_window(
            &[
                (("a".into(), "b".into()), 5.0),
                (("c".into(), "d".into()), 5.0),
                (("e".into(), "f".into()), 5.0),
                (("g".into(), "h".into()), 0.5),
            ],
            now,
        )
        .await
        .unwrap();
        // Bump (a, b) AND (e, f) sample counts to 5 (4 more
        // record_window cycles) so the only thing distinguishing
        // them from (c, d) is the helpfulness gate.
        for _ in 0..4 {
            cooc.record_window(
                &[
                    (("a".into(), "b".into()), 0.001),
                    (("e".into(), "f".into()), 0.001),
                    (("g".into(), "h".into()), 0.001),
                ],
                now,
            )
            .await
            .unwrap();
        }

        // Helpfulness: a, b, c, d, g, h all helpful; e helpful,
        // f UNHELPFUL.
        help.record_window(
            &[
                ("a".into(), 1.0),
                ("b".into(), 1.0),
                ("c".into(), 1.0),
                ("d".into(), 1.0),
                ("e".into(), 1.0),
                ("f".into(), -1.0),
                ("g".into(), 1.0),
                ("h".into(), 1.0),
            ],
            now,
        )
        .await
        .unwrap();

        let kept = select_candidates(
            &cooc, &help, &plog, &cfg(true), now,
        )
        .await;
        let pairs: Vec<(String, String)> = kept
            .iter()
            .map(|c| (c.a.clone(), c.b.clone()))
            .collect();
        assert_eq!(
            pairs,
            vec![("a".into(), "b".into())],
            "only (a, b) survives all four gates"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn selector_dedups_against_proposal_log() {
        let dir = tmp_dir("dedup");
        let store = open_store(&dir, 88).await;
        let cooc = PersistentCooccurrenceLedger::new(
            store.domain(KeyDomain::CooccurrenceLedger),
        );
        let help = PersistentHelpfulnessLedger::new(
            store.domain(KeyDomain::HelpfulnessLedger),
        );
        let plog = PersistentPersonaProposalLog::open(
            store.domain(KeyDomain::PersonaProposals),
            b"consolidation-test-key".to_vec(),
        )
        .await
        .unwrap();

        let now = 2_000_000u64;
        cooc.record_window(
            &[
                (("a".into(), "b".into()), 5.0),
                (("c".into(), "d".into()), 5.0),
            ],
            now,
        )
        .await
        .unwrap();
        cooc.record_window(
            &[
                (("a".into(), "b".into()), 0.001),
                (("c".into(), "d".into()), 0.001),
            ],
            now,
        )
        .await
        .unwrap();
        help.record_window(
            &[
                ("a".into(), 1.0),
                ("b".into(), 1.0),
                ("c".into(), 1.0),
                ("d".into(), 1.0),
            ],
            now,
        )
        .await
        .unwrap();

        // Pre-stamp (a, b)'s proposal id in the chain (any
        // status counts).
        plog.append_pending(
            "consolidate-pair:a+b".into(),
            now * 1000,
            "test".into(),
            ProposedPersonaDelta {
                category: PersonaDeltaCategory::LearnedContext,
                op: PersonaDeltaOp::AppendList {
                    value: "previously proposed".into(),
                },
                reason: None,
                supersedes_proposal_id: None,
            },
        )
        .await
        .unwrap();

        let kept = select_candidates(
            &cooc, &help, &plog, &cfg(true), now,
        )
        .await;
        let pairs: Vec<(String, String)> = kept
            .iter()
            .map(|c| (c.a.clone(), c.b.clone()))
            .collect();
        assert_eq!(
            pairs,
            vec![("c".into(), "d".into())],
            "(a, b) is deduped against the proposal chain; \
             (c, d) survives"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn selector_respects_per_cycle_cap_and_order() {
        let dir = tmp_dir("cap");
        let store = open_store(&dir, 89).await;
        let cooc = PersistentCooccurrenceLedger::new(
            store.domain(KeyDomain::CooccurrenceLedger),
        );
        let help = PersistentHelpfulnessLedger::new(
            store.domain(KeyDomain::HelpfulnessLedger),
        );
        let plog = PersistentPersonaProposalLog::open(
            store.domain(KeyDomain::PersonaProposals),
            b"consolidation-test-key".to_vec(),
        )
        .await
        .unwrap();

        let now = 3_000_000u64;
        // Five pairs, distinct affinities, all helpful and
        // sample-eligible. cap = 3 should keep the top 3 by
        // affinity descending.
        cooc.record_window(
            &[
                (("a".into(), "b".into()), 10.0),
                (("c".into(), "d".into()), 8.0),
                (("e".into(), "f".into()), 6.0),
                (("g".into(), "h".into()), 4.0),
                (("i".into(), "j".into()), 2.0),
            ],
            now,
        )
        .await
        .unwrap();
        cooc.record_window(
            &[
                (("a".into(), "b".into()), 0.001),
                (("c".into(), "d".into()), 0.001),
                (("e".into(), "f".into()), 0.001),
                (("g".into(), "h".into()), 0.001),
                (("i".into(), "j".into()), 0.001),
            ],
            now,
        )
        .await
        .unwrap();
        help.record_window(
            &[
                ("a".into(), 1.0), ("b".into(), 1.0),
                ("c".into(), 1.0), ("d".into(), 1.0),
                ("e".into(), 1.0), ("f".into(), 1.0),
                ("g".into(), 1.0), ("h".into(), 1.0),
                ("i".into(), 1.0), ("j".into(), 1.0),
            ],
            now,
        )
        .await
        .unwrap();

        let kept = select_candidates(
            &cooc, &help, &plog, &cfg(true), now,
        )
        .await;
        assert_eq!(kept.len(), 3, "cap = 3 enforced");
        assert_eq!(kept[0].a, "a", "highest-affinity first");
        assert_eq!(kept[1].a, "c");
        assert_eq!(kept[2].a, "e");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn selector_disabled_config_returns_empty() {
        let dir = tmp_dir("disabled");
        let store = open_store(&dir, 90).await;
        let cooc = PersistentCooccurrenceLedger::new(
            store.domain(KeyDomain::CooccurrenceLedger),
        );
        let help = PersistentHelpfulnessLedger::new(
            store.domain(KeyDomain::HelpfulnessLedger),
        );
        let plog = PersistentPersonaProposalLog::open(
            store.domain(KeyDomain::PersonaProposals),
            b"consolidation-test-key".to_vec(),
        )
        .await
        .unwrap();

        cooc.record_window(
            &[(("a".into(), "b".into()), 100.0)],
            1000,
        )
        .await
        .unwrap();
        help.record_window(
            &[("a".into(), 5.0), ("b".into(), 5.0)],
            1000,
        )
        .await
        .unwrap();

        let kept = select_candidates(
            &cooc, &help, &plog, &cfg(false), 1000,
        )
        .await;
        assert!(
            kept.is_empty(),
            "disabled config short-circuits the selector"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn consolidate_files_pending_with_canonical_id() {
        let dir = tmp_dir("file");
        let store = open_store(&dir, 91).await;
        let plog = PersistentPersonaProposalLog::open(
            store.domain(KeyDomain::PersonaProposals),
            b"consolidation-test-key".to_vec(),
        )
        .await
        .unwrap();

        let cand = ConsolidationCandidate {
            a: "rollback".into(),
            b: "deploy".into(),
            affinity: 3.2,
            samples: 5,
            helpfulness_min: 1.5,
        };
        let stat = consolidate(
            vec![cand],
            &OkPhraser,
            &plog,
            "reflection:test",
            10_000_000,
        )
        .await;
        assert_eq!(stat.filed, 1);
        assert!(!stat.llm_unavailable);
        assert_eq!(
            stat.pairs,
            vec![("rollback".into(), "deploy".into())]
        );
        let entry = plog
            .get("consolidate-pair:deploy+rollback")
            .expect("filed under canonical (sorted) id");
        assert_eq!(
            entry.proposed_op.category,
            PersonaDeltaCategory::LearnedContext
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn consolidate_records_llm_unavailable_when_all_phrase_fail()
    {
        let dir = tmp_dir("llm-down");
        let store = open_store(&dir, 92).await;
        let plog = PersistentPersonaProposalLog::open(
            store.domain(KeyDomain::PersonaProposals),
            b"consolidation-test-key".to_vec(),
        )
        .await
        .unwrap();

        let stat = consolidate(
            vec![ConsolidationCandidate {
                a: "x".into(),
                b: "y".into(),
                affinity: 2.0,
                samples: 3,
                helpfulness_min: 1.0,
            }],
            &FailPhraser,
            &plog,
            "reflection:test",
            5_000_000,
        )
        .await;
        assert_eq!(stat.filed, 0);
        assert!(
            stat.llm_unavailable,
            "every survivor's phrasing failed → cycle-wide \
             LLM unavailable"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn consolidate_empty_input_is_quiet_not_llm_down() {
        let dir = tmp_dir("empty");
        let store = open_store(&dir, 93).await;
        let plog = PersistentPersonaProposalLog::open(
            store.domain(KeyDomain::PersonaProposals),
            b"consolidation-test-key".to_vec(),
        )
        .await
        .unwrap();

        let stat = consolidate(
            Vec::new(),
            &FailPhraser,
            &plog,
            "reflection:test",
            6_000_000,
        )
        .await;
        assert_eq!(stat.filed, 0);
        assert!(
            !stat.llm_unavailable,
            "no candidates is the quiet case — not LLM down"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- Phase 92 — supersession detector ----------------------

    fn supersession_cfg() -> PersonaConsolidationConfig {
        PersonaConsolidationConfig {
            enabled: true,
            min_affinity: 1.0,
            min_samples: 2,
            min_topic_helpfulness: 0.0,
            max_proposals_per_cycle: 3,
            enable_supersession: true,
        }
    }

    /// Shared-endpoint case (Q1a): old pair `(A, B)` has
    /// decayed below the Phase 88 floor; new pair `(A, C)`
    /// shares endpoint A and is above the Phase 87 floor
    /// with both endpoints helpful → ONE supersession
    /// candidate fires.
    #[tokio::test]
    async fn detect_supersession_shared_endpoint_fires() {
        let dir = tmp_dir("super-shared");
        let store = open_store(&dir, 94).await;
        let cooc = PersistentCooccurrenceLedger::new(
            store.domain(KeyDomain::CooccurrenceLedger),
        );
        let help = PersistentHelpfulnessLedger::new(
            store.domain(KeyDomain::HelpfulnessLedger),
        );
        let plog = PersistentPersonaProposalLog::open(
            store.domain(KeyDomain::PersonaProposals),
            b"super-shared-key".to_vec(),
        )
        .await
        .unwrap();

        let now = 1_000_000u64;
        // Old pair (auth, jwt): seeded ONCE, decayed to a
        // negligible affinity by passing `pair_below_affinity
        // = 1.0` while the ledger never reached that floor.
        cooc.record_window(
            &[(("auth".into(), "jwt".into()), 0.3)],
            now,
        )
        .await
        .unwrap();
        // New pair (auth, sessions): strong + sampled twice.
        cooc.record_window(
            &[(("auth".into(), "sessions".into()), 5.0)],
            now,
        )
        .await
        .unwrap();
        cooc.record_window(
            &[(("auth".into(), "sessions".into()), 0.001)],
            now,
        )
        .await
        .unwrap();
        help.record_window(
            &[
                ("auth".into(), 1.0),
                ("jwt".into(), 1.0),
                ("sessions".into(), 1.0),
            ],
            now,
        )
        .await
        .unwrap();

        let applied = vec![(
            "consolidate-pair:auth+jwt".to_string(),
            "you work auth with jwt".to_string(),
            ("auth".to_string(), "jwt".to_string()),
        )];

        let out = detect_supersession(
            &applied,
            &cooc,
            &help,
            &plog,
            &supersession_cfg(),
            1.0,
            now,
        )
        .await;
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].old_proposal_id,
            "consolidate-pair:auth+jwt"
        );
        assert_eq!(
            out[0].new_proposal_id,
            "consolidate-pair:auth+sessions"
        );
        assert_eq!(
            out[0].new_pair,
            ("auth".into(), "sessions".into())
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// No shared endpoint → no supersession. The new strong
    /// pair `(C, D)` is the Phase 87 standard-selector's job;
    /// supersession only handles pairs that genuinely
    /// replace an existing facet.
    #[tokio::test]
    async fn detect_supersession_no_shared_endpoint_skipped() {
        let dir = tmp_dir("super-noshare");
        let store = open_store(&dir, 95).await;
        let cooc = PersistentCooccurrenceLedger::new(
            store.domain(KeyDomain::CooccurrenceLedger),
        );
        let help = PersistentHelpfulnessLedger::new(
            store.domain(KeyDomain::HelpfulnessLedger),
        );
        let plog = PersistentPersonaProposalLog::open(
            store.domain(KeyDomain::PersonaProposals),
            b"super-noshare-key".to_vec(),
        )
        .await
        .unwrap();

        let now = 1_000_000u64;
        cooc.record_window(
            &[(("auth".into(), "jwt".into()), 0.3)],
            now,
        )
        .await
        .unwrap();
        // New strong pair shares NEITHER endpoint with
        // (auth, jwt).
        cooc.record_window(
            &[(("frontend".into(), "css".into()), 5.0)],
            now,
        )
        .await
        .unwrap();
        cooc.record_window(
            &[(("frontend".into(), "css".into()), 0.001)],
            now,
        )
        .await
        .unwrap();
        help.record_window(
            &[
                ("auth".into(), 1.0),
                ("jwt".into(), 1.0),
                ("frontend".into(), 1.0),
                ("css".into(), 1.0),
            ],
            now,
        )
        .await
        .unwrap();

        let applied = vec![(
            "consolidate-pair:auth+jwt".to_string(),
            "auth+jwt facet".to_string(),
            ("auth".to_string(), "jwt".to_string()),
        )];

        let out = detect_supersession(
            &applied,
            &cooc,
            &help,
            &plog,
            &supersession_cfg(),
            1.0,
            now,
        )
        .await;
        assert!(
            out.is_empty(),
            "no shared endpoint → not a supersession"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The old pair must be BELOW the decay floor; if it
    /// remains durable, no supersession (Phase 88 wouldn't
    /// decay it, and Phase 87 already proposes the new
    /// shared-endpoint pair on its own).
    #[tokio::test]
    async fn detect_supersession_skipped_when_old_pair_still_durable()
    {
        let dir = tmp_dir("super-durable");
        let store = open_store(&dir, 96).await;
        let cooc = PersistentCooccurrenceLedger::new(
            store.domain(KeyDomain::CooccurrenceLedger),
        );
        let help = PersistentHelpfulnessLedger::new(
            store.domain(KeyDomain::HelpfulnessLedger),
        );
        let plog = PersistentPersonaProposalLog::open(
            store.domain(KeyDomain::PersonaProposals),
            b"super-durable-key".to_vec(),
        )
        .await
        .unwrap();

        let now = 1_000_000u64;
        // Both pairs above the floor.
        cooc.record_window(
            &[
                (("auth".into(), "jwt".into()), 5.0),
                (("auth".into(), "sessions".into()), 5.0),
            ],
            now,
        )
        .await
        .unwrap();
        cooc.record_window(
            &[
                (("auth".into(), "jwt".into()), 0.001),
                (("auth".into(), "sessions".into()), 0.001),
            ],
            now,
        )
        .await
        .unwrap();
        help.record_window(
            &[
                ("auth".into(), 1.0),
                ("jwt".into(), 1.0),
                ("sessions".into(), 1.0),
            ],
            now,
        )
        .await
        .unwrap();

        let applied = vec![(
            "consolidate-pair:auth+jwt".to_string(),
            "old facet".to_string(),
            ("auth".to_string(), "jwt".to_string()),
        )];

        let out = detect_supersession(
            &applied,
            &cooc,
            &help,
            &plog,
            &supersession_cfg(),
            1.0,
            now,
        )
        .await;
        assert!(
            out.is_empty(),
            "old pair still durable → no supersession"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Helpfulness gate: the new pair's new endpoint must
    /// be helpful. An unhelpful third topic blocks
    /// supersession.
    #[tokio::test]
    async fn detect_supersession_skipped_when_new_endpoint_unhelpful()
    {
        let dir = tmp_dir("super-unhelpful");
        let store = open_store(&dir, 97).await;
        let cooc = PersistentCooccurrenceLedger::new(
            store.domain(KeyDomain::CooccurrenceLedger),
        );
        let help = PersistentHelpfulnessLedger::new(
            store.domain(KeyDomain::HelpfulnessLedger),
        );
        let plog = PersistentPersonaProposalLog::open(
            store.domain(KeyDomain::PersonaProposals),
            b"super-unhelpful-key".to_vec(),
        )
        .await
        .unwrap();

        let now = 1_000_000u64;
        cooc.record_window(
            &[(("auth".into(), "jwt".into()), 0.3)],
            now,
        )
        .await
        .unwrap();
        cooc.record_window(
            &[(("auth".into(), "sessions".into()), 5.0)],
            now,
        )
        .await
        .unwrap();
        cooc.record_window(
            &[(("auth".into(), "sessions".into()), 0.001)],
            now,
        )
        .await
        .unwrap();
        // `sessions` is net-negative on helpfulness → fails
        // the Phase 87 double-gate that the supersession
        // detector also enforces.
        help.record_window(
            &[
                ("auth".into(), 1.0),
                ("jwt".into(), 1.0),
                ("sessions".into(), -1.0),
            ],
            now,
        )
        .await
        .unwrap();

        let applied = vec![(
            "consolidate-pair:auth+jwt".to_string(),
            "old facet".to_string(),
            ("auth".to_string(), "jwt".to_string()),
        )];

        let out = detect_supersession(
            &applied,
            &cooc,
            &help,
            &plog,
            &supersession_cfg(),
            1.0,
            now,
        )
        .await;
        assert!(
            out.is_empty(),
            "unhelpful new endpoint → no supersession"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `enable_supersession = false` (the default) short-
    /// circuits the detector entirely — the cycle never
    /// considers supersession, byte-identical to pre-Phase-92.
    #[tokio::test]
    async fn detect_supersession_disabled_returns_empty() {
        let dir = tmp_dir("super-off");
        let store = open_store(&dir, 98).await;
        let cooc = PersistentCooccurrenceLedger::new(
            store.domain(KeyDomain::CooccurrenceLedger),
        );
        let help = PersistentHelpfulnessLedger::new(
            store.domain(KeyDomain::HelpfulnessLedger),
        );
        let plog = PersistentPersonaProposalLog::open(
            store.domain(KeyDomain::PersonaProposals),
            b"super-off-key".to_vec(),
        )
        .await
        .unwrap();

        let now = 1_000_000u64;
        cooc.record_window(
            &[
                (("auth".into(), "jwt".into()), 0.3),
                (("auth".into(), "sessions".into()), 5.0),
            ],
            now,
        )
        .await
        .unwrap();
        cooc.record_window(
            &[(("auth".into(), "sessions".into()), 0.001)],
            now,
        )
        .await
        .unwrap();
        help.record_window(
            &[
                ("auth".into(), 1.0),
                ("jwt".into(), 1.0),
                ("sessions".into(), 1.0),
            ],
            now,
        )
        .await
        .unwrap();

        let applied = vec![(
            "consolidate-pair:auth+jwt".to_string(),
            "old facet".to_string(),
            ("auth".to_string(), "jwt".to_string()),
        )];

        let mut cfg = supersession_cfg();
        cfg.enable_supersession = false;
        let out = detect_supersession(
            &applied,
            &cooc,
            &help,
            &plog,
            &cfg,
            1.0,
            now,
        )
        .await;
        assert!(
            out.is_empty(),
            "knob off → no supersession (Phase 87/88 flow \
             remains byte-identical)"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
