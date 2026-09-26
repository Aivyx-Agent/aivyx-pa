//! Chapter Accord — contradiction detection over the Persona ("Soul").
//!
//! The identity-stack sibling of [`crate::contradiction`] (Chapter Concord,
//! which does this for memory). The Soul accretes operator-approved reflection
//! deltas; the lifecycle layer merges near-*duplicates* (Phase 87) and decays
//! the *unreinforced* (Phase 85), but nothing ever flags two approved facets
//! that flatly *contradict* — a seeded "communicate concisely" living next to a
//! later "give thorough, detailed explanations", both injected every turn. Nor
//! does anything notice the Soul drifting *against the operator's declared
//! Profile constraints* ("be candid, never flatter me" vs a learned "warm and
//! encouraging").
//!
//! One batched LLM call, on demand (`aivyx-pa persona conflicts` / the Studio),
//! zero background cost, no config. Best-effort: a parse/LLM failure yields no
//! conflicts, never an error. This module only *finds*; resolution removes the
//! losing facet via a normal `RemoveList` persona delta (operator-authored,
//! revertible), which lives in the daemon handler.

use std::sync::Arc;

use serde::Deserialize;

use aivyx_ipc::persona::EffectivePersona;

// Re-export the wire types so CLI/Studio (which depend on aivyx-channel, not
// aivyx-ipc directly) can name them — mirrors `contradiction::MemoryConflict`.
pub use aivyx_ipc::soul_conflict::{SoulConflict, SoulFacet};
use aivyx_llm::{ContentBlock, LlmMessage, LlmProvider, LlmRequest};
use aivyx_core::CancellationToken;

/// The five removable soft-list categories the Soul accretes (the always-on
/// operator `behavioral_constraints` core is NOT here — it's the immutable
/// reference side for cross-layer conflicts). Each is `(wire_label, accessor)`.
type Lists<'a> = [(&'static str, &'a [String]); 5];

/// The wire label for a removable soft-list category, or `None` for a scalar /
/// constraint / skill category (which the Accord gate does not check).
fn soft_label(cat: aivyx_ipc::persona::PersonaDeltaCategory) -> Option<&'static str> {
    use aivyx_ipc::persona::PersonaDeltaCategory as Cat;
    match cat {
        Cat::CharacterTraits => Some("character_traits"),
        Cat::CommunicationAdaptations => Some("communication_adaptations"),
        Cat::BehavioralPreferences => Some("behavioral_preferences"),
        Cat::LearnedContext => Some("learned_context"),
        Cat::RelationshipMilestones => Some("relationship_milestones"),
        _ => None,
    }
}

fn soft_lists(p: &EffectivePersona) -> Lists<'_> {
    [
        ("character_traits", &p.character_traits),
        ("communication_adaptations", &p.communication_adaptations),
        ("behavioral_preferences", &p.behavioral_preferences),
        ("learned_context", &p.learned_context),
        ("relationship_milestones", &p.relationship_milestones),
    ]
}

/// Bounds on one detection pass — keep the single LLM call cheap and its prompt
/// inside a small local model's context.
pub struct SoulContradictionConfig {
    pub max_facet_chars: usize,
    pub max_conflicts: usize,
    pub max_tokens: u32,
}

impl Default for SoulContradictionConfig {
    fn default() -> Self {
        Self {
            max_facet_chars: 200,
            max_conflicts: 30,
            max_tokens: 700,
        }
    }
}

/// What the model returns per conflict, before validation. Each side is the
/// integer index `[N]` of a listed facet/rule — NOT its verbatim text, so a
/// paraphrasing local model can still reference it reliably (the Concord
/// `seq`-reference trick; requiring verbatim value text proved fragile live).
#[derive(Debug, Deserialize)]
struct RawConflict {
    a: usize,
    b: usize,
    #[serde(default)]
    reason: String,
}

/// One indexed item shown to the model: its display index, category, `value`
/// (the resolution key — a facet's text, a constraint's text, or a SKILL's
/// name), and `display` (what the prompt shows, richer for skills). `is_rule`
/// marks the immutable operator profile_constraint side.
struct Item {
    category: String,
    value: String,
    display: String,
    is_rule: bool,
}

/// Flatten the persona snapshot into the indexed item list the prompt shows and
/// `validate` maps back through. Learned soft-list facets, then learned SKILLS,
/// then the operator's profile_constraint rules.
fn items_of(p: &EffectivePersona) -> Vec<Item> {
    let mut items = Vec::new();
    for (label, values) in soft_lists(p) {
        for v in values {
            items.push(Item {
                category: label.to_string(),
                value: v.clone(),
                display: v.clone(),
                is_rule: false,
            });
        }
    }
    // Chapter Accord (skill-layer) — each learned skill is an item whose value
    // is its NAME (the resolution key), shown with its trigger + procedure so
    // the judge can spot two skills giving incompatible guidance.
    for raw in &p.learned_skills {
        if let Some(s) = aivyx_ipc::persona::LearnedSkill::from_json_value(raw) {
            items.push(Item {
                category: SoulFacet::LEARNED_SKILL.to_string(),
                value: s.name.clone(),
                display: format!("skill \"{}\": when {} → {}", s.name, s.trigger, s.procedure),
                is_rule: false,
            });
        }
    }
    for c in &p.behavioral_constraints {
        items.push(Item {
            category: SoulFacet::PROFILE_CONSTRAINT.to_string(),
            value: c.clone(),
            display: c.clone(),
            is_rule: true,
        });
    }
    items
}

/// LLM-backed contradiction detector over an [`EffectivePersona`] snapshot.
pub struct SoulContradictionDetector {
    provider: Arc<dyn LlmProvider>,
    model: String,
    config: SoulContradictionConfig,
}

impl SoulContradictionDetector {
    pub fn new(provider: Arc<dyn LlmProvider>, model: impl Into<String>) -> Self {
        Self {
            provider,
            model: model.into(),
            config: SoulContradictionConfig::default(),
        }
    }

    pub fn with_config(mut self, config: SoulContradictionConfig) -> Self {
        self.config = config;
        self
    }

    fn system_prompt() -> &'static str {
        "You audit an AI assistant's evolving PERSONA (its \"Soul\") for \
         CONTRADICTIONS. You are given a NUMBERED list of the assistant's \
         standing guidance: learned facets, learned SKILLS (shown as `skill \
         \"X\": when … → …`), and the operator's FIXED rules (marked RULE). \
         Report a pair ONLY when the two are MUTUALLY EXCLUSIVE as standing \
         guidance — there is NO situation in which the assistant could honor \
         both at once. Apply this test: could a thoughtful assistant follow both \
         items together? If yes, they are NOT a contradiction. \
         CONTRADICTIONS (report): \"answer in one short sentence\" vs \"always \
         answer in several detailed paragraphs\"; \"recommend pharmaceuticals \
         first\" vs a RULE \"never suggest pharmaceuticals\"; two skills whose \
         procedures give OPPOSITE instructions for the SAME trigger. \
         NOT contradictions (do NOT report): complementary traits that coexist \
         (e.g. \"warm\" and \"candid\", \"friendly\" and \"direct\"); a default \
         plus an exception (\"concise by default\" and \"detailed when asked\"); \
         a broad tone next to a specific rule they can both be honored (\"warm\" \
         and \"do not flatter\" — you can be warm without flattering); or items \
         about DIFFERENT situations, topics, or that merely differ in emphasis. \
         When unsure, do NOT report. Refer to each item by its number in \
         brackets. Output ONLY a JSON array of `{\"a\":N,\"b\":M,\"reason\":\
         \"...\"}`, where N and M are the numbers of the two mutually-exclusive \
         items (different numbers) and `reason` names why they cannot both hold. \
         If there are none, output `[]`. No prose, no markdown fences."
    }

    /// Render the indexed item list into the user prompt. Pure + testable.
    fn user_prompt(items: &[Item], max_chars: usize) -> String {
        let clip = |s: &str| -> String {
            let one = s.replace('\n', " ");
            if one.chars().count() > max_chars {
                one.chars().take(max_chars).collect::<String>() + "…"
            } else {
                one
            }
        };
        let mut s = String::from(
            "Audit this Persona for contradictions. Items (refer to each by its \
             [number]):\n\n",
        );
        for (i, it) in items.iter().enumerate() {
            let tag = if it.is_rule { "RULE" } else { &it.category };
            s.push_str(&format!("[{i}] ({tag}) {}\n", clip(&it.display)));
        }
        s.push_str("\nOutput the JSON conflict array now.");
        s
    }

    /// Tolerant parse: locate the outermost `[ … ]` and decode. Empty on any
    /// failure — best-effort.
    fn parse(raw: &str) -> Vec<RawConflict> {
        let (Some(start), Some(end)) = (raw.find('['), raw.rfind(']')) else {
            return Vec::new();
        };
        if end <= start {
            return Vec::new();
        }
        serde_json::from_str::<Vec<RawConflict>>(&raw[start..=end]).unwrap_or_default()
    }

    /// Validate raw conflicts against the indexed item list: each index must be
    /// in range, the two sides distinct, ordered canonically, and deduped by id.
    /// A `profile_constraint` (rule) side is always placed as `b` (immutable)
    /// and marks the conflict `cross_layer`; two rules are skipped (the
    /// operator's to reconcile, not ours to remove).
    fn validate(items: &[Item], raws: Vec<RawConflict>, cap: usize) -> Vec<SoulConflict> {
        use std::collections::HashSet;
        let mut seen: HashSet<String> = HashSet::new();
        let mut out: Vec<SoulConflict> = Vec::new();
        for rc in raws {
            if rc.a == rc.b {
                continue;
            }
            let (Some(ia), Some(ib)) = (items.get(rc.a), items.get(rc.b)) else {
                continue; // out-of-range index — drop
            };
            if ia.is_rule && ib.is_rule {
                continue;
            }
            // Canonical order: an immutable rule is always side `b`; otherwise
            // order by (category, value) for a stable id.
            let (fa, fb, cross) = if ia.is_rule {
                (ib, ia, true)
            } else if ib.is_rule {
                (ia, ib, true)
            } else {
                let ka = (ia.category.as_str(), ia.value.as_str());
                let kb = (ib.category.as_str(), ib.value.as_str());
                if ka <= kb { (ia, ib, false) } else { (ib, ia, false) }
            };
            let id = SoulConflict::make_id(&fa.category, &fa.value, &fb.category, &fb.value);
            if !seen.insert(id.clone()) {
                continue;
            }
            out.push(SoulConflict {
                id,
                a: SoulFacet { category: fa.category.clone(), value: fa.value.clone() },
                b: SoulFacet { category: fb.category.clone(), value: fb.value.clone() },
                reason: rc.reason.trim().to_string(),
                cross_layer: cross,
            });
            if out.len() >= cap {
                break;
            }
        }
        out
    }

    /// Run one detection pass over the snapshot. Best-effort: empty on any
    /// LLM/parse failure, or when there are fewer than two items to compare.
    pub async fn detect(&self, persona: &EffectivePersona) -> Vec<SoulConflict> {
        let items = items_of(persona);
        if items.len() < 2 {
            return Vec::new();
        }
        let user = Self::user_prompt(&items, self.config.max_facet_chars);
        let messages = vec![LlmMessage::User {
            content: vec![ContentBlock::Text { text: user }],
        }];
        let request = LlmRequest {
            system: Some(Self::system_prompt()),
            messages: &messages,
            tools: &[],
            model: &self.model,
            max_tokens: self.config.max_tokens,
            temperature: Some(0.0),
        id_slot: None,
        slot_hint: None,
        route: None,
        };
        let cancel = CancellationToken::new();
        let Ok(mut stream) = self.provider.chat_stream(request, &cancel).await else {
            return Vec::new();
        };
        // The stream MUST be drained before `finish()` (the ollama provider
        // errors otherwise) — mirrors Chapter Concord's consumption.
        while stream.next_event().await.map(|e| e.is_some()).unwrap_or(false) {}
        let raw = match stream.finish().await {
            Ok(aivyx_llm::LlmStepEnd::FinalMessage { text, .. }) => text,
            _ => return Vec::new(),
        };
        Self::validate(&items, Self::parse(&raw), self.config.max_conflicts)
    }

    /// Chapter Accord prevent-at-write — would appending `value` under
    /// `category` introduce a contradiction into `persona`? Runs detection over
    /// the snapshot WITH the candidate added and returns the first conflict that
    /// involves the candidate (else `None`). Only the five soft-list categories
    /// are checked; a scalar / constraint / skill category returns `None`
    /// (nothing to gate). Best-effort — an LLM failure yields `None`.
    pub async fn detect_for_candidate(
        &self,
        persona: &EffectivePersona,
        category: aivyx_ipc::persona::PersonaDeltaCategory,
        value: &str,
    ) -> Option<SoulConflict> {
        use aivyx_ipc::persona::PersonaDeltaCategory as Cat;
        let label = soft_label(category)?;
        // Already present ⇒ no new contradiction the accretion introduces.
        let mut snap = persona.clone();
        let list = match category {
            Cat::CharacterTraits => &mut snap.character_traits,
            Cat::CommunicationAdaptations => &mut snap.communication_adaptations,
            Cat::BehavioralPreferences => &mut snap.behavioral_preferences,
            Cat::LearnedContext => &mut snap.learned_context,
            Cat::RelationshipMilestones => &mut snap.relationship_milestones,
            _ => return None,
        };
        if list.iter().any(|v| v == value) {
            return None;
        }
        list.push(value.to_string());
        self.detect(&snap).await.into_iter().find(|c| {
            (c.a.category == label && c.a.value == value)
                || (c.b.category == label && c.b.value == value)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn persona() -> EffectivePersona {
        EffectivePersona {
            character_traits: vec![
                "communicate concisely".into(),
                "warm and effusive".into(),
            ],
            behavioral_preferences: vec!["give thorough, detailed explanations".into()],
            behavioral_constraints: vec!["never flatter me, be candid".into()],
            ..Default::default()
        }
    }

    // Item layout for persona(): [0] character_traits "communicate concisely",
    // [1] character_traits "warm and effusive", [2] behavioral_preferences
    // "give thorough, detailed explanations", [3] RULE "never flatter me…".

    #[test]
    fn user_prompt_numbers_facets_and_marks_rules() {
        let s = SoulContradictionDetector::user_prompt(&items_of(&persona()), 200);
        assert!(s.contains("[0] (character_traits) communicate concisely"));
        assert!(s.contains("[2] (behavioral_preferences) give thorough"));
        assert!(s.contains("[3] (RULE) never flatter me"), "rules are tagged RULE: {s}");
    }

    #[test]
    fn validate_keeps_facet_vs_facet_and_orders_canonically() {
        let items = items_of(&persona());
        // model referenced items 2 and 0 (order-insensitive input)
        let raws = vec![RawConflict { a: 2, b: 0, reason: "concise vs thorough".into() }];
        let out = SoulContradictionDetector::validate(&items, raws, 30);
        assert_eq!(out.len(), 1);
        assert!(!out[0].cross_layer);
        // canonical order: behavioral_preferences sorts before character_traits.
        assert_eq!(out[0].a.category, "behavioral_preferences");
        assert_eq!(out[0].b.category, "character_traits");
    }

    #[test]
    fn validate_flags_cross_layer_and_puts_rule_as_b() {
        let items = items_of(&persona());
        let raws = vec![RawConflict { a: 3, b: 1, reason: "flattery vs candor".into() }];
        let out = SoulContradictionDetector::validate(&items, raws, 30);
        assert_eq!(out.len(), 1);
        assert!(out[0].cross_layer, "rule side ⇒ cross_layer");
        assert_eq!(out[0].a.category, "character_traits", "learned facet is removable side a");
        assert!(out[0].b.is_profile_constraint(), "rule is immutable side b");
    }

    #[test]
    fn validate_drops_out_of_range_and_self_pairs() {
        let items = items_of(&persona());
        assert!(SoulContradictionDetector::validate(
            &items,
            vec![RawConflict { a: 99, b: 0, reason: "x".into() }],
            30,
        )
        .is_empty());
        assert!(SoulContradictionDetector::validate(
            &items,
            vec![RawConflict { a: 2, b: 2, reason: "self".into() }],
            30,
        )
        .is_empty());
    }

    #[test]
    fn validate_dedups_same_pair() {
        let items = items_of(&persona());
        let raws = vec![
            RawConflict { a: 0, b: 2, reason: "one".into() },
            RawConflict { a: 2, b: 0, reason: "same pair, swapped".into() },
        ];
        assert_eq!(
            SoulContradictionDetector::validate(&items, raws, 30).len(),
            1,
            "the same pair in either order dedups to one conflict"
        );
    }

    // A provider that panics if the LLM is ever called — proves the
    // prevent-at-write gate short-circuits before spending a call.
    struct PanicProvider;
    #[async_trait::async_trait]
    impl LlmProvider for PanicProvider {
        async fn chat_stream(
            &self,
            _req: LlmRequest<'_>,
            _cancel: &CancellationToken,
        ) -> Result<Box<dyn aivyx_llm::LlmStream>, aivyx_llm::LlmError> {
            panic!("LLM must not be called for a non-gated candidate");
        }
    }

    #[tokio::test]
    async fn detect_for_candidate_skips_scalar_and_present_without_llm() {
        use aivyx_ipc::persona::PersonaDeltaCategory as Cat;
        let det = SoulContradictionDetector::new(Arc::new(PanicProvider), "m");
        // A scalar category is never a soft-list facet → None, no LLM call.
        assert!(det
            .detect_for_candidate(&persona(), Cat::AssistantName, "Jeeves")
            .await
            .is_none());
        // A facet already present introduces no NEW contradiction → None, no call.
        assert!(det
            .detect_for_candidate(&persona(), Cat::CharacterTraits, "communicate concisely")
            .await
            .is_none());
    }

    fn skill_json(name: &str, trigger: &str, procedure: &str) -> String {
        aivyx_ipc::persona::LearnedSkill {
            name: name.into(),
            trigger: trigger.into(),
            procedure: procedure.into(),
            ..Default::default()
        }
        .to_json_value()
    }

    #[test]
    fn items_include_learned_skills_with_rich_display() {
        let p = EffectivePersona {
            learned_skills: vec![
                skill_json("brevity", "when replying", "keep it to one line"),
                skill_json("depth", "when replying", "write several detailed paragraphs"),
            ],
            ..Default::default()
        };
        let items = items_of(&p);
        // Two skill items present, value = name (the resolution key).
        assert_eq!(items.len(), 2);
        assert!(items.iter().all(|i| i.category == SoulFacet::LEARNED_SKILL));
        assert!(items.iter().any(|i| i.value == "brevity"));
        // The prompt shows the rich skill form (trigger + procedure).
        let prompt = SoulContradictionDetector::user_prompt(&items, 200);
        assert!(prompt.contains("skill \"brevity\": when when replying → keep it to one line"));
    }

    #[test]
    fn validate_builds_skill_vs_skill_conflict() {
        let p = EffectivePersona {
            learned_skills: vec![
                skill_json("brevity", "when replying", "keep it to one line"),
                skill_json("depth", "when replying", "write several detailed paragraphs"),
            ],
            ..Default::default()
        };
        let items = items_of(&p); // [0]=brevity, [1]=depth
        let out = SoulContradictionDetector::validate(
            &items,
            vec![RawConflict { a: 0, b: 1, reason: "one line vs paragraphs".into() }],
            30,
        );
        assert_eq!(out.len(), 1);
        assert!(!out[0].cross_layer);
        assert!(out[0].a.is_learned_skill() && out[0].b.is_learned_skill());
        // value is the skill NAME (what resolution removes by).
        assert!(matches!(out[0].a.value.as_str(), "brevity" | "depth"));
    }

    #[test]
    fn validate_skips_rule_vs_rule() {
        let mut p = persona();
        p.behavioral_constraints.push("always agree with me".into()); // 2nd rule → item[4]
        let items = items_of(&p);
        let raws = vec![RawConflict { a: 3, b: 4, reason: "two rules".into() }];
        assert!(
            SoulContradictionDetector::validate(&items, raws, 30).is_empty(),
            "two operator rules are the operator's to reconcile, not removable"
        );
    }
}
