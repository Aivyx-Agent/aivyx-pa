//! Chapter Praxis (PX.1) — the specialized-skill authoring engine.
//!
//! Where the agent stops only *knowing* and starts being *able*: it reads
//! its own consolidated knowledge about a subject it understands well —
//! the [[Codex]] `WikiPage` (what it knows) + the [[Lattice]] typed graph
//! neighbourhood (how it connects) — and **synthesizes a specialized
//! skill** for it, proposed for the operator to approve.
//!
//! A sibling of the Whetstone refinement engine
//! ([`crate::skill_refinement`]): same governance (a Pending persona
//! proposal in the existing Agents approve/edit/reject UI), same
//! propose-only, opt-in, best-effort posture. The difference is the
//! *source* and the *output* — Whetstone sharpens an underperforming
//! existing skill; Praxis authors a **new** one from knowledge, for a
//! topic that has none. It is the first real consumer of the WH.1
//! `LearnedSkill.domain` field (the specialization tag).

use std::collections::HashSet;
use std::sync::Arc;

use async_trait::async_trait;
use aivyx_core::CancellationToken;
use aivyx_llm::{LlmMessage, LlmProvider, LlmRequest, LlmStepEnd};

use crate::knowledge_graph::PersistentGraphStore;
use crate::knowledge_wiki::PersistentWikiStore;
use crate::persona::{
    LearnedSkill, PersonaDeltaCategory, PersonaDeltaOp, ProposedPersonaDelta,
    SkillAuthor, SkillProvenance,
};
use crate::persona_proposal::PersistentPersonaProposalLog;

// The `[skill_authoring]` config lives in `aivyx-config`; re-exported here.
pub use aivyx_config::SkillAuthoringConfig;

/// The synthesized body of a specialized skill (the LLM's output). The
/// engine supplies the `name`/`domain` (the topic) and the provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftedSkill {
    pub trigger: String,
    pub procedure: String,
}

/// Synthesizes a specialized skill from a topic's consolidated knowledge.
/// Abstracted so the engine is testable without a live model; the
/// production impl is [`LlmSpecializationDrafter`].
#[async_trait]
pub trait SpecializationDrafter: Send + Sync {
    /// Draft a skill for `topic` from its wiki `summary` + rendered graph
    /// `relations` + a sample of the raw memory `entries` (concrete detail
    /// the summary abstracted away). `None` to skip (draft failure / empty
    /// / ungrounded).
    async fn draft(
        &self,
        topic: &str,
        summary: &str,
        relations: &str,
        entries: &str,
    ) -> Option<DraftedSkill>;
}

/// One authoring pass's outcome.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SkillAuthoringStat {
    /// Specialized skills authored (proposals filed) this pass.
    pub filed: usize,
    /// Wiki pages considered.
    pub considered: usize,
    /// Topics skipped: thin page or sparse graph neighbourhood.
    pub skipped_thin: usize,
    /// Topics skipped: already covered by an existing skill.
    pub skipped_covered: usize,
    /// Topics skipped: an authoring proposal already exists (deduped).
    pub deduped: usize,
}

/// How many raw memory entries to sample into the synthesis context.
const AUTHOR_MAX_ENTRIES: usize = 12;

/// VITRINE.md §6 P3 — turn an internal snake_case/kebab-case wiki topic
/// key ("overall_condition") into an operator-facing skill display name
/// ("Overall Condition") instead of leaking the raw slug verbatim. Only
/// the skill's `name` uses this; `domain` (the internal topic lookup
/// key other code matches against) keeps the raw topic unchanged. Falls
/// back to the raw topic if humanizing it would produce an empty string
/// (e.g. a topic that's entirely punctuation — shouldn't happen given
/// upstream topic-key validation, but never silently rename to "").
fn humanize_topic(topic: &str) -> String {
    let words: Vec<String> = topic
        .split(['_', '-'])
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect(),
                None => String::new(),
            }
        })
        .collect();
    if words.is_empty() {
        topic.to_string()
    } else {
        words.join(" ")
    }
}

/// 2026-07-04 dogfood (#5) — normalized word set for the topic↔graph
/// join. Wiki topics are slugs ("triathlon-basic") while graph subjects
/// are LLM-extracted phrases ("triathlon beginner advice", sometimes
/// with Unicode hyphens), so an exact `out_edges(topic)` lookup never
/// matched and the pass was organically starved of candidates. Lowercase,
/// split on non-alphanumeric (this also splits Unicode dashes), keep
/// words of 3+ chars minus bare grammar words, fold a trailing plural-s.
fn normalized_tokens(s: &str) -> HashSet<String> {
    const STOP: [&str; 8] =
        ["the", "and", "for", "with", "from", "this", "that", "its"];
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 3 && !STOP.contains(w))
        .map(|w| w.strip_suffix('s').unwrap_or(w).to_string())
        .filter(|w| !w.is_empty())
        .collect()
}

/// The triples "about" a topic: subject shares at least one significant
/// normalized token with the topic name. Deliberately loose — the edge
/// count is a connectedness heuristic and the matched relations feed the
/// drafter as context, so an extra tangential triple is harmless while a
/// missed one starves candidacy.
fn edges_about<'a>(
    triples: &'a [crate::knowledge_graph::GraphTriple],
    topic: &str,
) -> Vec<&'a crate::knowledge_graph::GraphTriple> {
    let want = normalized_tokens(topic);
    if want.is_empty() {
        return Vec::new();
    }
    triples
        .iter()
        .filter(|t| !normalized_tokens(&t.subject).is_disjoint(&want))
        .collect()
}

/// Chapter Praxis — author specialized skills for knowledge-rich, skill-
/// less topics.
///
/// `learned_skills_raw` is the effective persona's raw `LearnedSkill`
/// JSON (decoded for the dedup check: a topic already owning a skill — by
/// name or `domain` — is never re-authored).
#[allow(clippy::too_many_arguments)]
pub async fn propose_specialized_skills(
    wiki_store: &PersistentWikiStore,
    graph_store: &PersistentGraphStore,
    memory: &std::sync::Arc<dyn aivyx_memory::Memory>,
    learned_skills_raw: &[String],
    drafter: &dyn SpecializationDrafter,
    proposal_log: &PersistentPersonaProposalLog,
    config: &SkillAuthoringConfig,
    excluded_topics: &HashSet<String>,
    source_label: &str,
    now_ms: u64,
) -> SkillAuthoringStat {
    let mut stat = SkillAuthoringStat::default();
    if !config.enabled {
        return stat;
    }
    // Topics already owning a skill (by name or domain) — never re-author.
    let mut covered: HashSet<String> = HashSet::new();
    for raw in learned_skills_raw {
        if let Some(s) = LearnedSkill::from_json_value(raw) {
            covered.insert(s.name.clone());
            if let Some(d) = s.domain {
                covered.insert(d);
            }
        }
    }

    let pages = match wiki_store.all_pages().await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("aivyx-pa skill-authoring: wiki all_pages failed: {e}");
            return stat;
        }
    };

    // Soak review 2026-07-04 — near-duplicate TOPIC names propagate up
    // the stack (memory topics → wiki pages → twin skills: the first
    // soak filed "triathlon" AND "triathlon-basic"). Before authoring a
    // topic, compare its normalized token set against every covered
    // skill name/domain AND every prior `skill-author:*` proposal topic
    // (any status — a rejected twin means the operator didn't want the
    // family): a CONTAINMENT relation either way (one set ⊆ the other,
    // both non-empty) marks a near-dup and the topic is skipped as
    // deduped. Deliberately containment-only: token sets can't see
    // synonym pairs ("focus-tip" vs "concentration-tip"), and a looser
    // any-shared-token rule would wrongly merge genuinely distinct
    // domains ("movement-tip" vs "focus-tip"); the synonym class stays
    // with operator governance, which is the dedup of last resort.
    let mut claimed_token_sets: Vec<HashSet<String>> = covered
        .iter()
        .map(|name| normalized_tokens(name))
        .filter(|t| !t.is_empty())
        .collect();
    for p in proposal_log.list(crate::persona_proposal::ProposalStatusFilter::All) {
        if let Some(topic) = p.id.strip_prefix("skill-author:") {
            let t = normalized_tokens(topic);
            if !t.is_empty() {
                claimed_token_sets.push(t);
            }
        }
    }
    fn near_dup_of_claimed(
        claimed: &[HashSet<String>],
        topic: &str,
    ) -> bool {
        let t = normalized_tokens(topic);
        if t.is_empty() {
            return false;
        }
        claimed.iter().any(|c| c.is_subset(&t) || t.is_subset(c))
    }

    // #5 — fetch the graph once per pass; the per-page join below is a
    // token-overlap scan over this snapshot, not a per-topic store read.
    let triples = graph_store.all_triples().await.unwrap_or_default();

    for page in pages {
        if stat.filed >= config.max_per_cycle {
            break;
        }
        // 2026-07-04 dogfood (#3/#6) — never author from machine state
        // (`loop:progress` had a leaked wiki page) or from a routine's
        // own topic (the first live pass authored a "nightly-reflection"
        // skill from the nightly routine's journal writes — the agent
        // talking to itself, not operator-domain knowledge).
        if aivyx_memory::is_internal_topic(&page.topic)
            || excluded_topics.contains(&page.topic)
        {
            continue;
        }
        stat.considered += 1;

        // Substance floor — a stub page isn't skill-worthy.
        if page.summary.chars().count() < config.min_summary_chars {
            stat.skipped_thin += 1;
            continue;
        }
        // Skill-less — never compete with an existing skill for the topic.
        if covered.contains(&page.topic) {
            stat.skipped_covered += 1;
            continue;
        }
        // Deterministic id → a re-run (or a prior pending/rejected
        // proposal) dedups instead of nagging.
        let proposal_id = format!("skill-author:{}", page.topic);
        if proposal_log.get(&proposal_id).is_some() {
            stat.deduped += 1;
            continue;
        }
        // Near-dup topic family already claimed by a skill or a prior
        // authoring proposal (soak 2026-07-04; see the containment note
        // above).
        if near_dup_of_claimed(&claimed_token_sets, &page.topic) {
            stat.deduped += 1;
            continue;
        }
        // Graph neighbourhood — evidence it's a connected, procedural
        // subject (not an isolated fact). Joined by normalized token
        // overlap (#5), not exact node name — see `edges_about`.
        let edges = edges_about(&triples, &page.topic);
        if edges.len() < config.min_edges {
            stat.skipped_thin += 1;
            continue;
        }
        let relations = edges
            .iter()
            .map(|t| format!("{} {} {}", t.subject, t.predicate, t.object))
            .collect::<Vec<_>>()
            .join("\n");
        // Raw memory entries for the topic — concrete detail the wiki
        // summary abstracted away. Best-effort (an error → empty).
        let entries = memory
            .get_recent(&page.topic, AUTHOR_MAX_ENTRIES)
            .await
            .map(|es| es.iter().map(|e| format!("- {}", e.body)).collect::<Vec<_>>().join("\n"))
            .unwrap_or_default();

        let Some(drafted) =
            drafter.draft(&page.topic, &page.summary, &relations, &entries).await
        else {
            continue;
        };
        let trigger = drafted.trigger.trim().to_string();
        let procedure = drafted.procedure.trim().to_string();
        if trigger.is_empty() || procedure.is_empty() {
            continue;
        }

        let skill = LearnedSkill {
            name: humanize_topic(&page.topic),
            trigger,
            procedure,
            version: 1,
            provenance: SkillProvenance {
                author: SkillAuthor::Agent,
                reason: Some(format!(
                    "authored from the `{}` knowledge page + graph",
                    page.topic,
                )),
            },
            refined_from: None,
            domain: Some(page.topic.clone()),
        };
        let op = ProposedPersonaDelta {
            category: PersonaDeltaCategory::LearnedSkill,
            op: PersonaDeltaOp::AppendList { value: skill.to_json_value() },
            reason: Some(format!(
                "specialized skill authored from consolidated knowledge about `{}`",
                page.topic,
            )),
            supersedes_proposal_id: None,
        };
        if let Err(e) = proposal_log
            .append_pending(proposal_id, now_ms, source_label.to_string(), op)
            .await
        {
            eprintln!("aivyx-pa skill-authoring: append failed for {}: {e}", page.topic);
            continue;
        }
        stat.filed += 1;
        // A filed topic claims its family within this pass too, so a
        // max_per_cycle > 1 pass can't file twins back to back.
        let t = normalized_tokens(&page.topic);
        if !t.is_empty() {
            claimed_token_sets.push(t);
        }
    }
    stat
}

/// Production [`SpecializationDrafter`] over the agent's own
/// `LlmProvider`. Asks for a grounded `{trigger, procedure}` JSON; any
/// failure maps to `None` (per-topic skip — best-effort).
pub struct LlmSpecializationDrafter {
    provider: Arc<dyn LlmProvider>,
    model: String,
}

impl LlmSpecializationDrafter {
    pub fn new(provider: Arc<dyn LlmProvider>, model: String) -> Self {
        Self { provider, model }
    }
}

const AUTHOR_MAX_TOKENS: u32 = 600;
const AUTHOR_SYSTEM_PROMPT: &str = "You write a reusable SKILL for an AI \
assistant — a named procedure it will follow when a trigger matches — \
from what it already knows about one topic (a knowledge summary + typed \
relations it extracted from its own memory). A skill is a durable METHOD \
that must still be correct months from now: steps say HOW to obtain, \
check, and judge information, never what a value happened to be. NEVER \
copy observed data (weather readings, measurements, prices, dates, \
current conditions) into the steps — those were true once; they are not \
instructions. Ground the method ONLY in the provided knowledge; do not \
invent facts. If the knowledge is only a snapshot of observations with \
no reusable method behind it, output {\"skip\":\"one-line reason\"} \
instead. Otherwise output ONLY a JSON object: \
{\"trigger\":\"one or two sentences: when this skill applies\",\
\"procedure\":\"concrete step-by-step instructions that use the stated \
relations\"}. No prose outside the JSON, no markdown fences.";

/// Tolerant parse of the drafter's reply: the outermost `{...}` as a
/// `{trigger, procedure}` object.
fn parse_drafted(text: &str) -> Option<DraftedSkill> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end <= start {
        return None;
    }
    let raw = &text[start..=end];
    let v: serde_json::Value = serde_json::from_str(raw).ok()?;
    let trigger = v.get("trigger")?.as_str()?.trim().to_string();
    let procedure = v.get("procedure")?.as_str()?.trim().to_string();
    if trigger.is_empty() || procedure.is_empty() {
        return None;
    }
    Some(DraftedSkill { trigger, procedure })
}

#[async_trait]
impl SpecializationDrafter for LlmSpecializationDrafter {
    async fn draft(
        &self,
        topic: &str,
        summary: &str,
        relations: &str,
        entries: &str,
    ) -> Option<DraftedSkill> {
        let entries_block = if entries.trim().is_empty() {
            String::new()
        } else {
            format!("\nRaw memory notes (concrete detail):\n{entries}\n")
        };
        let user = format!(
            "Topic: {topic}\n\nWhat the assistant knows (summary):\n{summary}\n\n\
             Typed relations (subject predicate object):\n{relations}\n{entries_block}\n\
             Write the skill as the specified JSON.",
        );
        let messages = vec![LlmMessage::user_text(user)];
        let request = LlmRequest {
            model: &self.model,
            system: Some(AUTHOR_SYSTEM_PROMPT),
            messages: &messages,
            tools: &[],
            max_tokens: AUTHOR_MAX_TOKENS,
            temperature: Some(0.3),
        id_slot: None,
        slot_hint: None,
        route: None,
        };
        let cancel = CancellationToken::new();
        let mut stream = self.provider.chat_stream(request, &cancel).await.ok()?;
        while let Ok(Some(_)) = stream.next_event().await {}
        match stream.finish().await.ok()? {
            LlmStepEnd::FinalMessage { text, .. } => parse_drafted(&text),
            LlmStepEnd::ToolCalls { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_crypto::MasterKey;
    use aivyx_storage::{KeyDomain, RedbStorage, StorageConfig};

    use crate::knowledge_graph::GraphTriple;

    struct FixedDrafter(Option<DraftedSkill>);
    #[async_trait]
    impl SpecializationDrafter for FixedDrafter {
        async fn draft(&self, _t: &str, _s: &str, _r: &str, _e: &str) -> Option<DraftedSkill> {
            self.0.clone()
        }
    }

    struct Harness {
        wiki: Arc<PersistentWikiStore>,
        graph: Arc<PersistentGraphStore>,
        memory: Arc<dyn aivyx_memory::Memory>,
        proposals: PersistentPersonaProposalLog,
    }

    async fn harness() -> Harness {
        let base = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into());
        let dir = std::path::PathBuf::from(base)
            .join(format!("aivyx-author-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let store = RedbStorage::open(
            StorageConfig::new(dir.join("s.redb")),
            MasterKey::from_raw([5u8; 32]),
        )
        .await
        .unwrap();
        Harness {
            wiki: Arc::new(PersistentWikiStore::new(store.domain(KeyDomain::KnowledgeWiki))),
            graph: Arc::new(PersistentGraphStore::new(store.domain(KeyDomain::KnowledgeGraph))),
            memory: Arc::new(aivyx_memory::InMemoryMemory::new()),
            proposals: PersistentPersonaProposalLog::open(
                store.domain(KeyDomain::PersonaProposals),
                vec![0u8; 32],
            )
            .await
            .unwrap(),
        }
    }

    fn cfg() -> SkillAuthoringConfig {
        SkillAuthoringConfig { enabled: true, min_summary_chars: 50, min_edges: 2, max_per_cycle: 2 }
    }

    /// Production-shaped cap (1/cycle) — the near-dup test drives two
    /// cycles so the second sees the first's claimed family.
    fn cfg1() -> SkillAuthoringConfig {
        SkillAuthoringConfig { max_per_cycle: 1, ..cfg() }
    }

    fn drafted() -> Option<DraftedSkill> {
        Some(DraftedSkill {
            trigger: "when deploying".into(),
            procedure: "1. run ci 2. ship".into(),
        })
    }

    fn page(topic: &str, summary: String) -> aivyx_ipc::wiki::WikiPage {
        aivyx_ipc::wiki::WikiPage {
            topic: topic.into(),
            summary,
            source_seqs: vec![1],
            entry_count: 1,
            backlinks: vec![],
            updated_at: 1,
            source_fingerprint: 1,
        }
    }

    async fn seed_rich_topic(h: &Harness, topic: &str) {
        // summary clears the 50-char floor.
        h.wiki.put_page(&page(topic, "A".repeat(80))).await.unwrap();
        for obj in ["ci", "tests"] {
            h.graph
                .put_triple(&GraphTriple {
                    subject: topic.into(),
                    predicate: "depends-on".into(),
                    object: obj.into(),
                    source_seqs: vec![1],
                    mentions: 1,
                    updated_at: 1,
                })
                .await
                .unwrap();
        }
    }

    #[test]
    fn topic_graph_join_matches_the_real_rig_shapes() {
        // 2026-07-04 dogfood (#5) — the exact pairs that never matched
        // under the old out_edges(topic) lookup: slug topics vs
        // LLM-extracted entity phrases, one with a Unicode hyphen.
        let t = |s: &str| GraphTriple {
            subject: s.into(),
            predicate: "contains".into(),
            object: "x".into(),
            source_seqs: vec![1],
            mentions: 1,
            updated_at: 1,
        };
        let triples = vec![
            t("triathlon beginner advice"),
            t("movement\u{2011}tips memory"), // U+2011 non-breaking hyphen
            t("focus\u{2011}tips"),
            t("bike-to-work ride"),
        ];
        assert_eq!(edges_about(&triples, "triathlon-basic").len(), 1);
        // The tips-family topics cross-match on the shared "tip" token —
        // loose by design (the count is a connectedness heuristic).
        assert_eq!(edges_about(&triples, "movement-tip").len(), 2);
        assert_eq!(edges_about(&triples, "focus-tip").len(), 2);
        // No token overlap ⇒ no edges.
        assert!(edges_about(&triples, "coffee-preference").is_empty());
        // A degenerate topic (no significant tokens) never matches.
        assert!(edges_about(&triples, "--").is_empty());
    }

    #[test]
    fn humanize_topic_turns_internal_slugs_into_display_names() {
        // VITRINE.md §6 P3 — the exact reproduction from the walkthrough:
        // proposal id `skill-author:overall_condition`, skill name
        // `overall_condition` verbatim — internal snake_case leaking into
        // an operator-facing name.
        assert_eq!(humanize_topic("overall_condition"), "Overall Condition");
        assert_eq!(humanize_topic("triathlon-basic"), "Triathlon Basic");
        assert_eq!(humanize_topic("deploy"), "Deploy");
        // Mixed separators and repeated/leading/trailing ones collapse
        // rather than producing empty words or stray spaces.
        assert_eq!(humanize_topic("a_b-c"), "A B C");
        assert_eq!(humanize_topic("__leading"), "Leading");
        assert_eq!(humanize_topic("trailing__"), "Trailing");
        // A topic with no alphanumeric content at all has nothing to
        // humanize — fall back to it verbatim rather than "".
        assert_eq!(humanize_topic("--"), "--");
    }

    #[tokio::test]
    async fn near_dup_topic_families_are_not_authored_twice() {
        // Soak 2026-07-04: "triathlon" was proposed, then the next tick
        // proposed "triathlon-basic" — twin skills from near-dup topic
        // names. Containment on normalized tokens blocks the family;
        // genuinely distinct domains sharing a generic word survive.
        let h = harness().await;
        seed_rich_topic(&h, "triathlon").await;
        seed_rich_topic(&h, "triathlon-basic").await;
        seed_rich_topic(&h, "movement-tip").await;

        // Two cycles at cap 1. Page order is storage-defined, so assert
        // the family INVARIANT rather than which twin wins: exactly one
        // of the triathlon pair gets a proposal, the distinct domain
        // gets one, and the blocked twin registers as deduped.
        let mut filed_total = 0;
        let mut deduped_total = 0;
        for now in [1000, 2000] {
            let s = propose_specialized_skills(
                &h.wiki, &h.graph, &h.memory, &[], &FixedDrafter(drafted()),
                &h.proposals, &cfg1(), &HashSet::new(), "r", now,
            )
            .await;
            filed_total += s.filed;
            deduped_total += s.deduped;
        }
        assert_eq!(filed_total, 2, "one per family across two cycles");
        let tri = h.proposals.get("skill-author:triathlon").is_some();
        let tri_basic =
            h.proposals.get("skill-author:triathlon-basic").is_some();
        assert!(
            tri ^ tri_basic,
            "exactly ONE of the triathlon twins may be authored \
             (tri={tri}, tri_basic={tri_basic})"
        );
        assert!(h.proposals.get("skill-author:movement-tip").is_some());
        assert!(deduped_total >= 1, "the blocked twin counts as deduped");
    }

    #[tokio::test]
    async fn internal_and_routine_topics_are_never_authored_from() {
        let h = harness().await;
        // Machine state — a leaked loop bookkeeping page.
        seed_rich_topic(&h, "loop:progress").await;
        // A routine's own topic, rich enough to otherwise qualify.
        seed_rich_topic(&h, "nightly-reflection").await;
        let excluded: HashSet<String> =
            [String::from("nightly-reflection")].into();
        let stat = propose_specialized_skills(
            &h.wiki, &h.graph, &h.memory, &[], &FixedDrafter(drafted()),
            &h.proposals, &cfg(), &excluded, "r", 1000,
        )
        .await;
        assert_eq!(stat.filed, 0);
        assert!(h.proposals.get("skill-author:loop:progress").is_none());
        assert!(h.proposals.get("skill-author:nightly-reflection").is_none());
    }

    #[tokio::test]
    async fn rich_skill_less_topic_yields_a_specialized_proposal() {
        let h = harness().await;
        seed_rich_topic(&h, "deploy").await;
        let stat = propose_specialized_skills(
            &h.wiki, &h.graph, &h.memory, &[], &FixedDrafter(drafted()),
            &h.proposals, &cfg(), &HashSet::new(), "reflection", 1000,
        )
        .await;
        assert_eq!(stat.filed, 1);
        let p = h.proposals.get("skill-author:deploy").expect("proposal filed");
        assert_eq!(p.proposed_op.category, PersonaDeltaCategory::LearnedSkill);
        match &p.proposed_op.op {
            PersonaDeltaOp::AppendList { value } => {
                let s = LearnedSkill::from_json_value(value).unwrap();
                // VITRINE.md §6 P3 fix: name is humanized ("Deploy"), domain
                // (the internal topic lookup key) stays the raw slug.
                assert_eq!(s.name, "Deploy");
                assert_eq!(s.domain.as_deref(), Some("deploy"));
                assert_eq!(s.provenance.author, SkillAuthor::Agent);
                assert_eq!(s.version, 1);
                assert_eq!(s.procedure, "1. run ci 2. ship");
            }
            other => panic!("expected AppendList, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn thin_page_sparse_graph_covered_and_disabled_file_nothing() {
        let h = harness().await;
        // Thin page (below the floor) + a sparse graph.
        h.wiki.put_page(&page("stub", "tiny".into())).await.unwrap();
        // Rich page but only ONE edge → sparse neighbourhood.
        h.wiki.put_page(&page("lonely", "A".repeat(80))).await.unwrap();
        h.graph
            .put_triple(&GraphTriple {
                subject: "lonely".into(), predicate: "is".into(), object: "alone".into(),
                source_seqs: vec![1], mentions: 1, updated_at: 1,
            })
            .await
            .unwrap();
        let s = propose_specialized_skills(
            &h.wiki, &h.graph, &h.memory, &[], &FixedDrafter(drafted()), &h.proposals, &cfg(), &HashSet::new(), "r", 1000,
        )
        .await;
        assert_eq!(s.filed, 0);
        assert_eq!(s.skipped_thin, 2);

        // A rich topic already covered by a skill (domain match) → skipped.
        seed_rich_topic(&h, "deploy").await;
        let existing = vec![LearnedSkill {
            name: "my-deploy".into(),
            trigger: "x".into(),
            procedure: "y".into(),
            domain: Some("deploy".into()),
            ..Default::default()
        }
        .to_json_value()];
        let s2 = propose_specialized_skills(
            &h.wiki, &h.graph, &h.memory, &existing, &FixedDrafter(drafted()), &h.proposals, &cfg(), &HashSet::new(), "r", 1000,
        )
        .await;
        assert_eq!(s2.filed, 0);
        assert_eq!(s2.skipped_covered, 1);

        // Disabled config → nothing.
        let off = SkillAuthoringConfig { enabled: false, ..cfg() };
        let s3 = propose_specialized_skills(
            &h.wiki, &h.graph, &h.memory, &[], &FixedDrafter(drafted()), &h.proposals, &off, &HashSet::new(), "r", 1000,
        )
        .await;
        assert_eq!(s3.filed, 0);
    }

    #[tokio::test]
    async fn raw_memory_entries_reach_the_drafter() {
        use std::sync::Mutex;
        struct Capturing(Mutex<String>);
        #[async_trait]
        impl SpecializationDrafter for Capturing {
            async fn draft(&self, _t: &str, _s: &str, _r: &str, entries: &str) -> Option<DraftedSkill> {
                *self.0.lock().unwrap() = entries.to_string();
                drafted()
            }
        }
        let h = harness().await;
        seed_rich_topic(&h, "deploy").await;
        h.memory.put("deploy", "step: run the smoke test before ship").await.unwrap();
        let drafter = Capturing(Mutex::new(String::new()));
        let stat = propose_specialized_skills(
            &h.wiki, &h.graph, &h.memory, &[], &drafter, &h.proposals, &cfg(), &HashSet::new(), "r", 1000,
        )
        .await;
        assert_eq!(stat.filed, 1);
        assert!(
            drafter.0.lock().unwrap().contains("run the smoke test"),
            "the topic's raw memory entry is in the synthesis context",
        );
    }

    #[tokio::test]
    async fn authoring_is_deduped_on_rerun() {
        let h = harness().await;
        seed_rich_topic(&h, "deploy").await;
        let first = propose_specialized_skills(
            &h.wiki, &h.graph, &h.memory, &[], &FixedDrafter(drafted()), &h.proposals, &cfg(), &HashSet::new(), "r", 1000,
        )
        .await;
        assert_eq!(first.filed, 1);
        let second = propose_specialized_skills(
            &h.wiki, &h.graph, &h.memory, &[], &FixedDrafter(drafted()), &h.proposals, &cfg(), &HashSet::new(), "r", 2000,
        )
        .await;
        assert_eq!(second.filed, 0);
        assert_eq!(second.deduped, 1);
    }

    #[test]
    fn parse_drafted_tolerates_prose_around_json() {
        let out = parse_drafted(
            "Here you go:\n{\"trigger\":\"on X\",\"procedure\":\"do Y\"} — hope it helps",
        )
        .unwrap();
        assert_eq!(out.trigger, "on X");
        assert_eq!(out.procedure, "do Y");
        assert!(parse_drafted("no json here").is_none());
        assert!(parse_drafted("{\"trigger\":\"\",\"procedure\":\"y\"}").is_none());
    }
}
