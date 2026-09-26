//! Chapter Whetstone (WH.3) — the skill refinement engine.
//!
//! The headline of the chapter: the loop that watches how a skill the
//! operator or the agent already wrote actually plays out (the WH.2
//! [`SkillEffectivenessLedger`](crate::skill_effectiveness)) and, when one
//! keeps underperforming, **proposes a sharper version** — governed,
//! traceable, and reversible exactly like every other change to the
//! agent's identity.
//!
//! A refinement is a **linked supersession pair** of persona proposals
//! (the Phase 92 shape): an `AppendList` of the refined `v2` skill +
//! a `RemoveList` of the exact `v1` JSON, cross-linked via
//! `supersedes_proposal_id`. Both are filed `Pending`; the operator
//! approves / edits / rejects them as one pair in the **existing** Agents
//! UI — no new governance surface. The pass only ever *proposes*; nothing
//! is applied without operator approval, so the persona invariant holds.
//!
//! This module is the engine (find underperformer → draft → file the
//! pair); the daemon write-side ledger fold + the reflection-cadence
//! scheduling + the `[skill_refinement]` config land in WH.3b.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use aivyx_core::CancellationToken;
use aivyx_llm::{LlmMessage, LlmProvider, LlmRequest, LlmStepEnd};

use crate::persona::{
    LearnedSkill, PersonaDeltaCategory, PersonaDeltaOp, ProposedPersonaDelta,
    SkillAuthor, SkillProvenance,
};
use crate::persona_proposal::PersistentPersonaProposalLog;
use crate::skill_effectiveness::SkillEffectivenessLedger;

// The `[skill_refinement]` config lives in `aivyx-config` (the canonical
// home for operator config, like every other `*Config`); re-exported here
// for the engine + the deps bundle.
pub use aivyx_config::SkillRefinementConfig;

/// Drafts a sharper procedure for an underperforming skill. Abstracted so
/// the engine is testable without a live model; the production impl is
/// [`LlmRefinementDrafter`].
#[async_trait]
pub trait RefinementDrafter: Send + Sync {
    /// Return an improved procedure body for `skill`, or `None` to skip
    /// (a draft failure / empty / unavailable model).
    async fn draft(&self, skill: &LearnedSkill) -> Option<String>;
}

/// One refinement pass's outcome.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SkillRefinementStat {
    /// Refinement pairs filed this pass.
    pub filed: usize,
    /// Underperformers considered.
    pub considered: usize,
    /// Underperformers with no matching live skill (already removed).
    pub skipped_no_skill: usize,
    /// Underperformers already carrying a pending refinement (deduped).
    pub deduped: usize,
}

/// Chapter Whetstone — propose refinements for underperforming skills.
///
/// `learned_skills_raw` is the effective persona's raw `LearnedSkill`
/// JSON strings (the `AppendList` values on the chain) — the engine
/// decodes them for drafting but files the **exact** raw string in the
/// `RemoveList` half, so the retire matches the stored entry even for a
/// pre-Whetstone skill whose JSON predates the WH.1 fields.
pub async fn propose_skill_refinements(
    ledger: &SkillEffectivenessLedger,
    learned_skills_raw: &[String],
    drafter: &dyn RefinementDrafter,
    proposal_log: &PersistentPersonaProposalLog,
    config: &SkillRefinementConfig,
    source_label: &str,
    now_ms: u64,
) -> SkillRefinementStat {
    let mut stat = SkillRefinementStat::default();
    if !config.enabled {
        return stat;
    }
    // name → (raw stored JSON, decoded skill).
    let mut by_name: HashMap<String, (String, LearnedSkill)> = HashMap::new();
    for raw in learned_skills_raw {
        if let Some(s) = LearnedSkill::from_json_value(raw) {
            by_name.insert(s.name.clone(), (raw.clone(), s));
        }
    }

    let now_secs = now_ms / 1000;
    let under = match ledger
        .underperformers(config.floor, config.min_samples, now_secs)
        .await
    {
        Ok(u) => u,
        Err(e) => {
            eprintln!("aivyx-pa skill-refinement: underperformers query failed: {e}");
            return stat;
        }
    };

    for (skill_name, _entry) in under.into_iter().take(config.max_per_cycle) {
        stat.considered += 1;
        let Some((raw_v1, existing)) = by_name.get(&skill_name) else {
            // The ledger remembers a skill that's no longer live.
            stat.skipped_no_skill += 1;
            continue;
        };
        let next_version = existing.version.saturating_add(1);
        // Deterministic ids → a re-run (or a still-pending/rejected prior
        // proposal) dedups instead of nagging with the same refinement.
        let append_id = format!("skill-refine:{skill_name}:v{next_version}");
        let remove_id = format!("skill-refine-remove:{skill_name}:v{next_version}");
        if proposal_log.get(&append_id).is_some() {
            stat.deduped += 1;
            continue;
        }

        let Some(new_proc) = drafter.draft(existing).await else {
            continue;
        };
        let new_proc = new_proc.trim().to_string();
        if new_proc.is_empty() || new_proc == existing.procedure {
            // No usable improvement — don't file a no-op refinement.
            continue;
        }

        let reason = "recent turns using this skill underperformed";
        let v2 = LearnedSkill {
            name: existing.name.clone(),
            trigger: existing.trigger.clone(),
            procedure: new_proc,
            version: next_version,
            provenance: SkillProvenance {
                author: SkillAuthor::Agent,
                reason: Some(reason.to_string()),
            },
            refined_from: Some(existing.name.clone()),
            domain: existing.domain.clone(),
        };

        // The new facet (AppendList v2), pointing at the retire half.
        let append_op = ProposedPersonaDelta {
            category: PersonaDeltaCategory::LearnedSkill,
            op: PersonaDeltaOp::AppendList { value: v2.to_json_value() },
            reason: Some(format!(
                "refine skill `{skill_name}` v{}→v{next_version}: {reason}",
                existing.version,
            )),
            supersedes_proposal_id: Some(remove_id.clone()),
        };
        if let Err(e) = proposal_log
            .append_pending(append_id.clone(), now_ms, source_label.to_string(), append_op)
            .await
        {
            eprintln!("aivyx-pa skill-refinement: append (v2) failed for {skill_name}: {e}");
            continue;
        }

        // The retire half (RemoveList of the EXACT stored v1 JSON).
        let remove_op = ProposedPersonaDelta {
            category: PersonaDeltaCategory::LearnedSkill,
            op: PersonaDeltaOp::RemoveList { value: raw_v1.clone() },
            reason: Some(format!(
                "retire skill `{skill_name}` v{} — superseded by the refined v{next_version}",
                existing.version,
            )),
            supersedes_proposal_id: Some(append_id.clone()),
        };
        if let Err(e) = proposal_log
            .append_pending(remove_id, now_ms, source_label.to_string(), remove_op)
            .await
        {
            // The v2 half is already filed (one-way linkage); the operator
            // can still review it. Log and move on.
            eprintln!("aivyx-pa skill-refinement: retire (v1) failed for {skill_name}: {e}");
        }
        stat.filed += 1;
    }
    stat
}

/// Production [`RefinementDrafter`] over the agent's own `LlmProvider`.
/// Mirrors `LlmPairPhraser`: a short, conservative completion; any
/// failure maps to `None` (per-skill skip — the pass stays best-effort).
pub struct LlmRefinementDrafter {
    provider: Arc<dyn LlmProvider>,
    model: String,
}

impl LlmRefinementDrafter {
    pub fn new(provider: Arc<dyn LlmProvider>, model: String) -> Self {
        Self { provider, model }
    }
}

const REFINE_MAX_TOKENS: u32 = 512;
const REFINE_SYSTEM_PROMPT: &str = "You are sharpening one of an AI \
assistant's saved skills — a named procedure it follows when a trigger \
matches. Recent turns that used this skill went poorly, so the procedure \
needs improving. Rewrite ONLY the procedure body to be clearer, more \
specific, and more likely to succeed: keep what works, fix what is vague \
or wrong, prefer concrete steps. Do NOT change the skill's purpose or its \
trigger. Output ONLY the improved procedure text — no preamble, no \
markdown fences, no commentary.";

#[async_trait]
impl RefinementDrafter for LlmRefinementDrafter {
    async fn draft(&self, skill: &LearnedSkill) -> Option<String> {
        let user = format!(
            "Skill name: {}\nApplies when: {}\n\nCurrent procedure:\n{}\n\n\
             Rewrite the procedure to be sharper and more reliable.",
            skill.name, skill.trigger, skill.procedure,
        );
        let messages = vec![LlmMessage::user_text(user)];
        let request = LlmRequest {
            model: &self.model,
            system: Some(REFINE_SYSTEM_PROMPT),
            messages: &messages,
            tools: &[],
            max_tokens: REFINE_MAX_TOKENS,
            temperature: Some(0.3),
        id_slot: None,
        slot_hint: None,
        route: None,
        };
        let cancel = CancellationToken::new();
        let mut stream = self.provider.chat_stream(request, &cancel).await.ok()?;
        while let Ok(Some(_)) = stream.next_event().await {}
        match stream.finish().await.ok()? {
            LlmStepEnd::FinalMessage { text, .. } => {
                let t = text.trim().to_string();
                if t.is_empty() {
                    None
                } else {
                    Some(t)
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
    use aivyx_storage::{KeyDomain, RedbStorage, StorageConfig};

    fn skill_json(name: &str, proc: &str) -> String {
        LearnedSkill {
            name: name.into(),
            trigger: "when X".into(),
            procedure: proc.into(),
            ..Default::default()
        }
        .to_json_value()
    }

    struct FixedDrafter(Option<String>);
    #[async_trait]
    impl RefinementDrafter for FixedDrafter {
        async fn draft(&self, _s: &LearnedSkill) -> Option<String> {
            self.0.clone()
        }
    }

    async fn harness() -> (SkillEffectivenessLedger, PersistentPersonaProposalLog) {
        let base = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into());
        let dir = std::path::PathBuf::from(base)
            .join(format!("aivyx-refine-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = RedbStorage::open(
            StorageConfig::new(dir.join("s.redb")),
            MasterKey::from_raw([9u8; 32]),
        )
        .await
        .unwrap();
        let ledger = SkillEffectivenessLedger::new(
            store.domain(KeyDomain::SkillHelpfulnessLedger),
        );
        let proposals = PersistentPersonaProposalLog::open(
            store.domain(KeyDomain::PersonaProposals),
            vec![0u8; 32],
        )
        .await
        .unwrap();
        (ledger, proposals)
    }

    fn cfg() -> SkillRefinementConfig {
        SkillRefinementConfig { enabled: true, floor: 0.0, min_samples: 3, max_per_cycle: 2 }
    }

    // Drive a skill net-negative + well-sampled.
    async fn sink(ledger: &SkillEffectivenessLedger, name: &str) {
        for t in [1000u64, 2000, 3000] {
            ledger.record_window(&[(name.into(), -1.0)], t).await.unwrap();
        }
    }

    #[tokio::test]
    async fn underperformer_yields_a_supersession_pair() {
        let (ledger, proposals) = harness().await;
        sink(&ledger, "checklist").await;
        let raw = vec![skill_json("checklist", "old procedure")];
        let drafter = FixedDrafter(Some("a sharper procedure".into()));

        let stat = propose_skill_refinements(
            &ledger, &raw, &drafter, &proposals, &cfg(), "reflection", 4_000_000,
        )
        .await;
        assert_eq!(stat.filed, 1);

        // Both halves filed, cross-linked, LearnedSkill category.
        let append = proposals.get("skill-refine:checklist:v2").expect("v2 append");
        let remove = proposals.get("skill-refine-remove:checklist:v2").expect("v1 retire");
        assert_eq!(append.proposed_op.category, PersonaDeltaCategory::LearnedSkill);
        assert_eq!(
            append.proposed_op.supersedes_proposal_id.as_deref(),
            Some("skill-refine-remove:checklist:v2"),
        );
        assert_eq!(
            remove.proposed_op.supersedes_proposal_id.as_deref(),
            Some("skill-refine:checklist:v2"),
        );
        // The retire removes the EXACT stored v1 JSON.
        match &remove.proposed_op.op {
            PersonaDeltaOp::RemoveList { value } => assert_eq!(value, &raw[0]),
            other => panic!("expected RemoveList, got {other:?}"),
        }
        // The v2 carries agent provenance + lineage.
        match &append.proposed_op.op {
            PersonaDeltaOp::AppendList { value } => {
                let v2 = LearnedSkill::from_json_value(value).unwrap();
                assert_eq!(v2.version, 2);
                assert_eq!(v2.provenance.author, SkillAuthor::Agent);
                assert_eq!(v2.refined_from.as_deref(), Some("checklist"));
                assert_eq!(v2.procedure, "a sharper procedure");
            }
            other => panic!("expected AppendList, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn healthy_skill_and_disabled_and_missing_file_nothing() {
        let (ledger, proposals) = harness().await;
        // Healthy skill (positive) → never an underperformer.
        ledger.record_window(&[("good".into(), 3.0)], 1000).await.unwrap();
        let raw = vec![skill_json("good", "fine")];
        let drafter = FixedDrafter(Some("x".into()));
        let s = propose_skill_refinements(
            &ledger, &raw, &drafter, &proposals, &cfg(), "r", 4_000_000,
        )
        .await;
        assert_eq!(s.filed, 0);

        // Disabled config → nothing even for an underperformer.
        sink(&ledger, "bad").await;
        let raw2 = vec![skill_json("bad", "old")];
        let off = SkillRefinementConfig { enabled: false, ..cfg() };
        let s2 = propose_skill_refinements(
            &ledger, &raw2, &drafter, &proposals, &off, "r", 4_000_000,
        )
        .await;
        assert_eq!(s2.filed, 0);

        // Enabled, underperformer, but no live skill by that name → skip.
        let s3 = propose_skill_refinements(
            &ledger, &[], &drafter, &proposals, &cfg(), "r", 4_000_000,
        )
        .await;
        assert_eq!(s3.filed, 0);
        assert_eq!(s3.skipped_no_skill, 1);
    }

    #[tokio::test]
    async fn refinement_is_deduped_on_rerun() {
        let (ledger, proposals) = harness().await;
        sink(&ledger, "checklist").await;
        let raw = vec![skill_json("checklist", "old procedure")];
        let drafter = FixedDrafter(Some("sharper".into()));
        let first = propose_skill_refinements(
            &ledger, &raw, &drafter, &proposals, &cfg(), "r", 4_000_000,
        )
        .await;
        assert_eq!(first.filed, 1);
        // Second pass: the v2 proposal already exists → deduped, not re-filed.
        let second = propose_skill_refinements(
            &ledger, &raw, &drafter, &proposals, &cfg(), "r", 5_000_000,
        )
        .await;
        assert_eq!(second.filed, 0);
        assert_eq!(second.deduped, 1);
    }
}
