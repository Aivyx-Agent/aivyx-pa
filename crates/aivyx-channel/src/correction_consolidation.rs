//! Phase 172 — correction-driven Persona proposals.
//!
//! The proposal-side consumption of the Phase 172 correction
//! ledger, symmetric with Phase 87's
//! [`crate::persona_consolidation`] consumption of the Phase 83
//! co-occurrence ledger. When a topic the operator has
//! repeatedly *reworked* clears the decayed-correction-count
//! floor (`min_corrections`) **and** has been observed across
//! enough reflection windows (`min_samples`), the reflection
//! cron asks the existing reflection LLM to phrase a
//! `learned_context` facet naming the preference friction, and
//! files it as a Pending [`crate::persona_proposal::PersonaProposal`]
//! through the existing Phase 70 proposal chain.
//!
//! This is the self-improvement closure named in the Aivyx
//! Agent Review (§5.8): the agent can finally surface *"you
//! keep reworking my responses about X — want a Profile
//! note?"* on its own cadence.
//!
//! Everything downstream is unchanged: propose-only (the
//! operator approves / rejects via Web UI or CLI), edit-then-
//! approve, `Revert`-able, core-protected. Phase 172 only adds
//! a new *source* of proposals — never a new way of resolving
//! them. The operator gate stays the sole authority (Phase 70
//! P14 rule).

use std::collections::HashSet;

// moved to the wasm-clean aivyx-ipc crate (Chapter M.2d-2); re-exported here.
pub use aivyx_ipc::insights::{CorrectionConsolidationStat};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;

use aivyx_config::CorrectionConsolidationConfig;
use aivyx_core::CancellationToken;
use aivyx_llm::{LlmMessage, LlmProvider, LlmRequest, LlmStepEnd};

use crate::correction_ledger::PersistentCorrectionLedger;
use crate::persona::{
    PersonaDeltaCategory, PersonaDeltaOp, ProposedPersonaDelta,
};
use crate::persona_proposal::PersistentPersonaProposalLog;

/// Hard cap on how many ledger topics the selector inspects
/// each cycle. The actual filing cap is the operator-tunable
/// `max_proposals_per_cycle`; this bounds the scan so a giant
/// ledger doesn't make the pass O(n) on every reflection cron.
pub const CORRECTION_SCAN_TOP_K: usize = 64;

/// One topic the selector kept after the conservative gates.
/// `corrections` is the decayed count, `samples` the window
/// count — both surface in the proposal `reason` line.
#[derive(Debug, Clone, PartialEq)]
pub struct CorrectionCandidate {
    pub topic: String,
    pub corrections: f32,
    pub samples: u32,
}


/// Shared handle the consolidation pass writes (per cycle) and
/// `GetLearningInsights` reads. `None` inside = no cycle has run
/// yet this daemon lifetime.
pub type SharedCorrectionConsolidationStat =
    Arc<RwLock<Option<CorrectionConsolidationStat>>>;

/// Construct an empty shared correction-consolidation-stat
/// handle.
pub fn shared_correction_consolidation_stat()
-> SharedCorrectionConsolidationStat {
    Arc::new(RwLock::new(None))
}

/// The LLM-summarized facet seam. The pass hands each surviving
/// topic to a phraser; the production impl adapts the existing
/// `aivyx_llm::LlmProvider`. Tests substitute a recording fake
/// with deterministic output.
///
/// Returning `None` is the per-candidate degenerate path (LLM
/// transport hiccup, parse failure, refusal). The pass skips
/// that candidate; *cycle-wide* unavailability is detected by
/// every survivor's phraser returning `None`.
#[async_trait]
pub trait TopicPhraser: Send + Sync {
    async fn phrase(&self, topic: &str) -> Option<String>;
}

/// Build the canonical proposal id for a correction candidate.
/// The dedup invariant relies on this being stable in the topic
/// so a periodic loop never re-files (or re-nags after a
/// rejection).
pub fn correction_proposal_id(topic: &str) -> String {
    format!("correction:{topic}")
}

/// The pure-ish selector. Reads the correction ledger + the
/// proposal log, applies the `min_corrections` + `min_samples`
/// double-gate, dedups against the chain, sorts (most-corrected
/// first), caps. Output is ordered so the operator sees the
/// strongest friction first.
///
/// All errors are best-effort: a ledger / chain failure
/// silently drops that candidate; the cycle continues. The
/// pass's "0 filed" is always a valid outcome.
pub async fn select_corrections(
    ledger: &PersistentCorrectionLedger,
    proposal_log: &PersistentPersonaProposalLog,
    config: &CorrectionConsolidationConfig,
    now_secs: u64,
) -> Vec<CorrectionCandidate> {
    if !config.enabled {
        return Vec::new();
    }
    let ranked = match ledger.ranked(now_secs).await {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };

    let mut out: Vec<CorrectionCandidate> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for (topic, entry) in ranked.into_iter().take(CORRECTION_SCAN_TOP_K)
    {
        if entry.ewma_count < config.min_corrections {
            // `ranked` is count-descending — everything below
            // this is also below the floor.
            break;
        }
        if entry.samples < config.min_samples {
            continue;
        }
        // Dedup: a topic already proposed (in ANY status —
        // Pending / Approved / Rejected / Superseded) is never
        // re-filed. Phase 70 dedup invariant: the assistant
        // must not nag about its own identity.
        let pid = correction_proposal_id(&topic);
        if !seen.insert(pid.clone()) {
            continue;
        }
        if proposal_log.get(&pid).is_some() {
            continue;
        }
        out.push(CorrectionCandidate {
            topic,
            corrections: entry.ewma_count,
            samples: entry.samples,
        });
    }
    // Stable descending order: most-corrected first; ties broken
    // by topic so output is fully deterministic.
    out.sort_by(|a, b| {
        b.corrections
            .partial_cmp(&a.corrections)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.topic.cmp(&b.topic))
    });
    out.truncate(config.max_proposals_per_cycle as usize);
    out
}

/// The actuator. For each candidate the selector returned, asks
/// the `TopicPhraser` for a one-line `learned_context` facet,
/// and files it as a Pending `PersonaProposal` through the
/// existing Phase 70 chain with `proposal_id =
/// "correction:{topic}"`.
///
/// Returns the cycle's [`CorrectionConsolidationStat`] — the
/// caller writes it to the shared handle so the Phase 78
/// surface can render it. Best-effort throughout: a per-
/// candidate phrasing failure skips that candidate; an append
/// failure is logged and skipped; a cycle where *every*
/// survivor's phrasing failed records `llm_unavailable = true`
/// so a quiet "0 filed" cycle stays distinguishable from an LLM
/// outage.
pub async fn consolidate_corrections(
    candidates: Vec<CorrectionCandidate>,
    phraser: &dyn TopicPhraser,
    proposal_log: &PersistentPersonaProposalLog,
    source_label: &str,
    now_ms: u64,
) -> CorrectionConsolidationStat {
    let mut filed = 0u32;
    let mut phrase_failures = 0u32;
    let mut topics: Vec<String> = Vec::new();
    let attempted = candidates.len();

    for cand in candidates {
        let pid = correction_proposal_id(&cand.topic);
        let Some(value) = phraser.phrase(&cand.topic).await else {
            phrase_failures += 1;
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            phrase_failures += 1;
            continue;
        }
        let reason = format!(
            "correction signal: the operator reworked turns \
             involving `{topic}` — decayed count {count:.2} over \
             {samples} reflection window(s). The agent did not \
             judge the content; the operator decides whether \
             this becomes a Profile note.",
            topic = cand.topic,
            count = cand.corrections,
            samples = cand.samples,
        );
        let op = ProposedPersonaDelta {
            category: PersonaDeltaCategory::LearnedContext,
            op: PersonaDeltaOp::AppendList {
                value: value.to_string(),
            },
            reason: Some(reason),
            // A correction proposal never supersedes; it is a
            // fresh observation the operator gates.
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
                topics.push(cand.topic);
            }
            Err(e) => {
                eprintln!(
                    "aivyx-pa correction-consolidation: \
                     append_pending failed for {pid}: {e}",
                );
            }
        }
    }

    let llm_unavailable = attempted > 0
        && filed == 0
        && phrase_failures == attempted as u32;

    CorrectionConsolidationStat {
        ts_secs: now_ms / 1000,
        filed,
        llm_unavailable,
        topics,
    }
}

/// Hard cap on the LLM completion the production phraser asks
/// for. One short sentence; 128 leaves headroom without
/// inviting verbose paragraphs.
const PHRASE_MAX_TOKENS: u32 = 128;

/// Fixed system prompt for the correction phrasing call. Short,
/// structural, conservative: the model writes ONE short factual
/// statement framing the topic as a preference the operator
/// cares about, without inventing the specifics of the
/// correction (the structural signal can't see content).
const PHRASE_SYSTEM_PROMPT: &str = "You are noting a topic the \
operator has repeatedly refined the assistant's responses on. \
Write exactly ONE short factual statement (under 30 words, \
plain prose, no markdown, no list, no quotes) suggesting the \
operator has specific preferences about how this topic is \
handled. Refer to the operator in the second person. Do not \
invent the specifics of those preferences — only that they \
exist and are worth confirming.";

/// Production `TopicPhraser` that delegates to the existing
/// `aivyx_llm::LlmProvider` (the same provider + model the agent
/// uses). Failures map to `None` (per-candidate skip — the
/// actuator stays best-effort). Mirrors Phase 87's
/// `LlmPairPhraser`.
pub struct LlmTopicPhraser {
    provider: Arc<dyn LlmProvider>,
    model: String,
}

impl LlmTopicPhraser {
    pub fn new(
        provider: Arc<dyn LlmProvider>,
        model: String,
    ) -> Self {
        Self { provider, model }
    }
}

#[async_trait]
impl TopicPhraser for LlmTopicPhraser {
    async fn phrase(&self, topic: &str) -> Option<String> {
        let user = format!(
            "Topic: `{topic}`\n\nThe operator has repeatedly \
             reworked the assistant's responses on this topic. \
             Write one short factual statement (under 30 words) \
             suggesting the operator has specific preferences \
             about how it is handled, worth confirming. Refer \
             to the operator in the second person."
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
        let cancel = CancellationToken::new();
        let mut stream =
            self.provider.chat_stream(request, &cancel).await.ok()?;
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
            .join(format!("aivyx-cc-{tag}-{}", Uuid::new_v4()));
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

    fn cfg(enabled: bool) -> CorrectionConsolidationConfig {
        CorrectionConsolidationConfig {
            enabled,
            min_corrections: 3.0,
            min_samples: 2,
            max_proposals_per_cycle: 3,
        }
    }

    async fn open_proposal_log(
        store: &Arc<dyn Storage>,
    ) -> PersistentPersonaProposalLog {
        PersistentPersonaProposalLog::open(
            store.domain(KeyDomain::PersonaProposals),
            b"correction-consolidation-test-key".to_vec(),
        )
        .await
        .unwrap()
    }

    fn open_ledger(
        store: &Arc<dyn Storage>,
    ) -> PersistentCorrectionLedger {
        PersistentCorrectionLedger::new(
            store.domain(KeyDomain::CorrectionLedger),
        )
    }

    /// A deterministic phraser, so actuator tests assert on what
    /// was filed, not on LLM behavior.
    struct OkPhraser;
    #[async_trait]
    impl TopicPhraser for OkPhraser {
        async fn phrase(&self, topic: &str) -> Option<String> {
            Some(format!(
                "You have specific preferences about how `{topic}` \
                 is handled."
            ))
        }
    }

    /// A phraser that ALWAYS returns None — simulates a
    /// cycle-wide LLM outage.
    struct FailPhraser;
    #[async_trait]
    impl TopicPhraser for FailPhraser {
        async fn phrase(&self, _topic: &str) -> Option<String> {
            None
        }
    }

    #[test]
    fn correction_proposal_id_is_canonical() {
        assert_eq!(
            correction_proposal_id("deploy"),
            "correction:deploy"
        );
    }

    #[tokio::test]
    async fn selector_filters_floor_and_samples() {
        let dir = tmp_dir("filter");
        let store = open_store(&dir, 172).await;
        let ledger = open_ledger(&store);
        let plog = open_proposal_log(&store).await;

        let now = 1_000_000u64;
        // `strong` clears both gates; `thin` clears the count
        // but only has one sample; `weak` clears samples but is
        // below the count floor.
        // strong: 4 corrections over 2 windows.
        ledger
            .record_window(&[("strong".into(), 2.0)], now)
            .await
            .unwrap();
        ledger
            .record_window(&[("strong".into(), 2.0)], now)
            .await
            .unwrap();
        // thin: 4 corrections in ONE window (samples = 1).
        ledger
            .record_window(&[("thin".into(), 4.0)], now)
            .await
            .unwrap();
        // weak: 1 correction over 2 windows (below floor 3.0).
        ledger
            .record_window(&[("weak".into(), 0.5)], now)
            .await
            .unwrap();
        ledger
            .record_window(&[("weak".into(), 0.5)], now)
            .await
            .unwrap();

        let kept =
            select_corrections(&ledger, &plog, &cfg(true), now).await;
        let topics: Vec<&str> =
            kept.iter().map(|c| c.topic.as_str()).collect();
        assert_eq!(
            topics,
            vec!["strong"],
            "only `strong` clears the count + samples double-gate"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn selector_dedups_against_proposal_log() {
        let dir = tmp_dir("dedup");
        let store = open_store(&dir, 173).await;
        let ledger = open_ledger(&store);
        let plog = open_proposal_log(&store).await;

        let now = 2_000_000u64;
        for topic in ["auth", "db"] {
            ledger
                .record_window(&[(topic.into(), 5.0)], now)
                .await
                .unwrap();
            ledger
                .record_window(&[(topic.into(), 0.001)], now)
                .await
                .unwrap();
        }

        // Pre-stamp `auth`'s proposal id in the chain.
        plog.append_pending(
            "correction:auth".into(),
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

        let kept =
            select_corrections(&ledger, &plog, &cfg(true), now).await;
        let topics: Vec<&str> =
            kept.iter().map(|c| c.topic.as_str()).collect();
        assert_eq!(
            topics,
            vec!["db"],
            "`auth` is deduped against the chain; `db` survives"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn selector_respects_cap_and_order() {
        let dir = tmp_dir("cap");
        let store = open_store(&dir, 174).await;
        let ledger = open_ledger(&store);
        let plog = open_proposal_log(&store).await;

        let now = 3_000_000u64;
        // Five topics, distinct counts, all sample-eligible.
        // cap = 3 keeps the top 3 by count descending.
        for (topic, c) in [
            ("a", 10.0),
            ("b", 8.0),
            ("c", 6.0),
            ("d", 4.0),
            ("e", 3.5),
        ] {
            ledger
                .record_window(&[(topic.into(), c)], now)
                .await
                .unwrap();
            ledger
                .record_window(&[(topic.into(), 0.001)], now)
                .await
                .unwrap();
        }

        let kept =
            select_corrections(&ledger, &plog, &cfg(true), now).await;
        assert_eq!(kept.len(), 3, "cap = 3 enforced");
        assert_eq!(kept[0].topic, "a", "most-corrected first");
        assert_eq!(kept[1].topic, "b");
        assert_eq!(kept[2].topic, "c");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn selector_disabled_config_returns_empty() {
        let dir = tmp_dir("disabled");
        let store = open_store(&dir, 175).await;
        let ledger = open_ledger(&store);
        let plog = open_proposal_log(&store).await;

        ledger
            .record_window(&[("auth".into(), 100.0)], 1000)
            .await
            .unwrap();

        let kept =
            select_corrections(&ledger, &plog, &cfg(false), 1000)
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
        let store = open_store(&dir, 176).await;
        let plog = open_proposal_log(&store).await;

        let cand = CorrectionCandidate {
            topic: "deploy".into(),
            corrections: 4.2,
            samples: 3,
        };
        let stat = consolidate_corrections(
            vec![cand],
            &OkPhraser,
            &plog,
            "reflection:test",
            10_000_000,
        )
        .await;
        assert_eq!(stat.filed, 1);
        assert!(!stat.llm_unavailable);
        assert_eq!(stat.topics, vec!["deploy".to_string()]);
        let entry = plog
            .get("correction:deploy")
            .expect("filed under canonical id");
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
        let store = open_store(&dir, 177).await;
        let plog = open_proposal_log(&store).await;

        let stat = consolidate_corrections(
            vec![CorrectionCandidate {
                topic: "x".into(),
                corrections: 5.0,
                samples: 3,
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
            "every survivor's phrasing failed → cycle-wide LLM \
             unavailable"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn consolidate_empty_input_is_quiet_not_llm_down() {
        let dir = tmp_dir("empty");
        let store = open_store(&dir, 178).await;
        let plog = open_proposal_log(&store).await;

        let stat = consolidate_corrections(
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

    #[tokio::test]
    async fn consolidate_dedups_across_cycles() {
        let dir = tmp_dir("xcycle");
        let store = open_store(&dir, 179).await;
        let ledger = open_ledger(&store);
        let plog = open_proposal_log(&store).await;

        let now = 4_000_000u64;
        ledger
            .record_window(&[("auth".into(), 3.0)], now)
            .await
            .unwrap();
        ledger
            .record_window(&[("auth".into(), 3.0)], now)
            .await
            .unwrap();

        // Cycle 1 files one proposal.
        let kept =
            select_corrections(&ledger, &plog, &cfg(true), now).await;
        let s1 = consolidate_corrections(
            kept,
            &OkPhraser,
            &plog,
            "refl-1",
            now * 1000,
        )
        .await;
        assert_eq!(s1.filed, 1);

        // Cycle 2, same signal → already in the chain → selector
        // returns nothing → no re-nag.
        let kept2 =
            select_corrections(&ledger, &plog, &cfg(true), now).await;
        assert!(
            kept2.is_empty(),
            "an already-proposed topic is never re-selected"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
