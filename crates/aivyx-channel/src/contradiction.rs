//! Chapter Concord — contradiction detection over stored memory.
//!
//! The self-consistency half of the memory stack: the agent stores
//! whatever it is told, so contradictory facts happily coexist ("home
//! airport is YPPH (Perth)" **and** "...Sydney, YSSY"). Nothing noticed.
//! This module gives that a first-class, operator-resolvable signal.
//!
//! ## Shape (mirrors the graph extractor + correction judge)
//!
//! An LLM pass, **on demand** (the operator runs `aivyx-pa memory conflicts`
//! or the Studio asks) rather than on a cron — so there is zero
//! background cost and no new config surface. One batched call classifies
//! every multi-entry non-internal topic's recent entries and returns the
//! pairs that assert *incompatible* facts about the same subject. Purely
//! best-effort: a parse/LLM failure yields no conflicts, never an error.
//!
//! Resolution lives elsewhere (the daemon deletes the losing entry via
//! [`aivyx_memory::Memory::delete_entry`] and audits it as a `Forget`);
//! this module only *finds* conflicts. The result type is the wasm-clean
//! [`aivyx_ipc::conflict::MemoryConflict`] so it crosses IPC unchanged.

use std::sync::Arc;

use aivyx_core::CancellationToken;
// Re-export the wasm-clean wire types so downstream crates (the CLI) can
// name them without depending on aivyx-ipc directly.
pub use aivyx_ipc::conflict::{ConflictSide, MemoryConflict};
use aivyx_llm::{ContentBlock, LlmMessage, LlmProvider, LlmRequest, LlmStepEnd};
use aivyx_memory::{is_internal_topic, Memory, MemoryEntry};
use serde::Deserialize;

/// Bounds on one detection pass — keep the single LLM call cheap and its
/// prompt within a small local model's context.
#[derive(Debug, Clone)]
pub struct ContradictionConfig {
    /// Most topics to examine in one pass (highest-entry-count first).
    pub max_topics: usize,
    /// Newest entries per topic to feed the judge.
    pub max_entries_per_topic: usize,
    /// Per-entry body cap (chars) in the prompt.
    pub max_entry_chars: usize,
    /// Cap on returned conflicts.
    pub max_conflicts: usize,
    /// Model max_tokens for the judge reply.
    pub max_tokens: u32,
}

impl Default for ContradictionConfig {
    fn default() -> Self {
        Self {
            max_topics: 24,
            max_entries_per_topic: 12,
            max_entry_chars: 240,
            max_conflicts: 50,
            max_tokens: 800,
        }
    }
}

/// What the model returns per conflict, before validation. Each side
/// names its own `(topic, seq)` so a conflict can span two topics.
#[derive(Debug, Deserialize)]
struct RawConflict {
    topic_a: String,
    seq_a: u64,
    topic_b: String,
    seq_b: u64,
    #[serde(default)]
    reason: String,
}

/// LLM-backed contradiction detector over a memory substrate.
pub struct ContradictionDetector {
    provider: Arc<dyn LlmProvider>,
    model: String,
    config: ContradictionConfig,
}

impl ContradictionDetector {
    pub fn new(provider: Arc<dyn LlmProvider>, model: impl Into<String>) -> Self {
        Self {
            provider,
            model: model.into(),
            config: ContradictionConfig::default(),
        }
    }

    pub fn with_config(mut self, config: ContradictionConfig) -> Self {
        self.config = config;
        self
    }

    fn system_prompt() -> &'static str {
        "You audit an AI assistant's memory for CONTRADICTIONS. You are \
         given several topics; under each, numbered entries (by `seq`) \
         state remembered facts. Find pairs of entries that assert \
         INCOMPATIBLE facts about the SAME subject — where at most one can \
         be true (e.g. two different home airports, two different \
         birthdays, \"prefers tea\" vs \"prefers coffee\"). The two entries \
         may be under the SAME topic OR under two DIFFERENT topics — check \
         across topics too. Output ONLY a JSON array of `{\"topic_a\":\
         \"...\",\"seq_a\":N,\"topic_b\":\"...\",\"seq_b\":M,\"reason\":\
         \"...\"}`, where side A is entry `seq_a` under `topic_a` and side \
         B is entry `seq_b` under `topic_b` (repeat the same topic name on \
         both when they share one). Each `(topic, seq)` MUST be an entry \
         actually listed above, and the two sides must be different \
         entries. `reason` is one short clause naming the incompatibility. \
         Report ONLY genuine contradictions — NOT entries that merely \
         differ, elaborate, update a plan, or describe different subjects. \
         Entries describing a CHANGE OVER TIME are a sequence, not a \
         contradiction: an older \"X exists / X is the case\" followed by \
         a newer \"X was removed / added / renamed / changed\" (environment \
         observations, progress logs, before-and-after states) must NOT \
         be flagged — the world changed between the two entries; neither \
         is wrong. \
         If there are none, output `[]`. No prose, no markdown fences."
    }

    /// Render topics+entries into the user prompt. Pure + testable.
    fn user_prompt(topics: &[(String, Vec<MemoryEntry>)], max_chars: usize) -> String {
        let mut s = String::from("Audit these topics for contradictions:\n");
        for (topic, entries) in topics {
            s.push_str(&format!("\n## topic: {topic}\n"));
            for e in entries {
                let body: String = if e.body.chars().count() > max_chars {
                    e.body.chars().take(max_chars).collect::<String>() + "…"
                } else {
                    e.body.clone()
                };
                s.push_str(&format!("- seq {}: {}\n", e.seq, body.replace('\n', " ")));
            }
        }
        s.push_str("\nOutput the JSON conflict array now.");
        s
    }

    /// Tolerant parse: locate the outermost `[ … ]` and decode. Empty on
    /// any failure — best-effort.
    fn parse(raw: &str) -> Vec<RawConflict> {
        let (Some(start), Some(end)) = (raw.find('['), raw.rfind(']')) else {
            return Vec::new();
        };
        if end <= start {
            return Vec::new();
        }
        serde_json::from_str::<Vec<RawConflict>>(&raw[start..=end]).unwrap_or_default()
    }

    /// Gather the candidate topics (non-internal, ≥2 entries), newest
    /// first by entry count, bounded by the config. Pure over a snapshot.
    async fn candidates(&self, memory: &dyn Memory) -> Vec<(String, Vec<MemoryEntry>)> {
        let Ok(topics) = memory.list_topics().await else {
            return Vec::new();
        };
        let mut out: Vec<(String, Vec<MemoryEntry>)> = Vec::new();
        for topic in topics {
            if is_internal_topic(&topic) {
                continue;
            }
            let Ok(entries) = memory
                .get_recent(&topic, self.config.max_entries_per_topic)
                .await
            else {
                continue;
            };
            if entries.len() >= 2 {
                out.push((topic, entries));
            }
        }
        // Most entries first (likeliest to hold a conflict), then cap.
        out.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then_with(|| a.0.cmp(&b.0)));
        out.truncate(self.config.max_topics);
        out
    }

    /// Validate raw conflicts against the candidate snapshot: both seqs
    /// must exist under the named topic, be distinct, and map to real
    /// entries. Orders each pair (a = older, b = newer) and dedups by id.
    fn validate(
        candidates: &[(String, Vec<MemoryEntry>)],
        raws: Vec<RawConflict>,
        cap: usize,
    ) -> Vec<MemoryConflict> {
        use std::collections::HashSet;
        // Look up an entry by (topic, seq) across all candidate topics.
        let find = |topic: &str, seq: u64| -> Option<&MemoryEntry> {
            candidates
                .iter()
                .find(|(t, _)| t == topic)
                .and_then(|(_, es)| es.iter().find(|e| e.seq == seq))
        };
        let mut seen: HashSet<String> = HashSet::new();
        let mut out: Vec<MemoryConflict> = Vec::new();
        for rc in raws {
            // Reject a side paired with itself (same topic AND seq).
            if rc.topic_a == rc.topic_b && rc.seq_a == rc.seq_b {
                continue;
            }
            let Some(ea) = find(&rc.topic_a, rc.seq_a) else {
                continue;
            };
            let Some(eb) = find(&rc.topic_b, rc.seq_b) else {
                continue;
            };
            // Order older → newer (by created_at, then topic, then seq) so
            // `b` is the newest-wins pick and the id is order-stable.
            let ka = (ea.created_at_secs, ea.topic.as_str(), ea.seq);
            let kb = (eb.created_at_secs, eb.topic.as_str(), eb.seq);
            let (older, newer) = if ka <= kb { (ea, eb) } else { (eb, ea) };
            let id = MemoryConflict::make_id(
                &older.topic,
                older.seq,
                &newer.topic,
                newer.seq,
            );
            if !seen.insert(id.clone()) {
                continue;
            }
            out.push(MemoryConflict {
                id,
                a: ConflictSide {
                    topic: older.topic.clone(),
                    seq: older.seq,
                    body: older.body.clone(),
                    created_at_secs: older.created_at_secs,
                },
                b: ConflictSide {
                    topic: newer.topic.clone(),
                    seq: newer.seq,
                    body: newer.body.clone(),
                    created_at_secs: newer.created_at_secs,
                },
                reason: rc.reason.clone(),
            });
            if out.len() >= cap {
                break;
            }
        }
        out
    }

    /// Run one on-demand detection pass. Best-effort throughout.
    pub async fn detect(&self, memory: &dyn Memory) -> Vec<MemoryConflict> {
        let candidates = self.candidates(memory).await;
        if candidates.is_empty() {
            return Vec::new();
        }
        let user = Self::user_prompt(&candidates, self.config.max_entry_chars);
        let messages = vec![LlmMessage::User {
            content: vec![ContentBlock::Text { text: user }],
        }];
        let request = LlmRequest {
            model: &self.model,
            system: Some(Self::system_prompt()),
            messages: &messages,
            tools: &[],
            max_tokens: self.config.max_tokens,
            temperature: Some(0.1),
        id_slot: None,
        slot_hint: None,
        route: None,
        };
        let token = CancellationToken::new();
        let Ok(mut stream) = self.provider.chat_stream(request, &token).await else {
            return Vec::new();
        };
        while stream.next_event().await.map(|e| e.is_some()).unwrap_or(false) {}
        let text = match stream.finish().await {
            Ok(LlmStepEnd::FinalMessage { text, .. }) => text,
            _ => return Vec::new(),
        };
        Self::validate(&candidates, Self::parse(&text), self.config.max_conflicts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aivyx_memory::InMemoryMemory;

    fn entry(topic: &str, seq: u64, body: &str, created: u64) -> MemoryEntry {
        MemoryEntry {
            topic: topic.into(),
            body: body.into(),
            seq,
            created_at_secs: created,
            last_read_at_secs: 0,
        }
    }

    /// Soak review 2026-07-04 — the detector flagged "suite-check.md
    /// present" (older) vs "suite-check.md removed" (newer) as a
    /// contradiction. Daily environment observations produce change
    /// sequences forever, so the prompt must carve them out explicitly.
    /// Drift-guard in the Plumb style: fails if a future edit quietly
    /// drops the temporal-sequence exemption.
    #[test]
    fn prompt_exempts_change_over_time_sequences() {
        let p = ContradictionDetector::system_prompt().to_lowercase();
        assert!(
            p.contains("change over time"),
            "prompt must name the temporal-sequence class"
        );
        assert!(
            p.contains("sequence, not a contradiction"),
            "prompt must say a sequence is not a contradiction"
        );
        assert!(
            p.contains("the world changed"),
            "prompt must explain WHY (neither entry is wrong)"
        );
    }

    #[test]
    fn parse_tolerates_prose_and_fences() {
        let raw = "Here you go:\n```json\n[{\"topic_a\":\"t\",\"seq_a\":1,\
                   \"topic_b\":\"t\",\"seq_b\":2,\"reason\":\"two airports\"}]\n``` done";
        let got = ContradictionDetector::parse(raw);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].topic_a, "t");
        assert_eq!(got[0].topic_b, "t");
        assert_eq!((got[0].seq_a, got[0].seq_b), (1, 2));
    }

    #[test]
    fn parse_empty_on_garbage() {
        assert!(ContradictionDetector::parse("no json here").is_empty());
        assert!(ContradictionDetector::parse("][").is_empty());
    }

    #[test]
    fn validate_orders_older_to_newer_and_drops_bad_refs() {
        let cands = vec![(
            "operator-note".to_string(),
            vec![
                entry("operator-note", 7, "home airport Sydney YSSY", 200),
                entry("operator-note", 3, "home airport YPPH Perth", 100),
            ],
        )];
        let raws = vec![
            // newer listed first — validate must order older→newer.
            RawConflict {
                topic_a: "operator-note".into(),
                seq_a: 7,
                topic_b: "operator-note".into(),
                seq_b: 3,
                reason: "two home airports".into(),
            },
            // duplicate (same pair, other order) — deduped by id.
            RawConflict {
                topic_a: "operator-note".into(),
                seq_a: 3,
                topic_b: "operator-note".into(),
                seq_b: 7,
                reason: "dup".into(),
            },
            // bogus seq — dropped.
            RawConflict {
                topic_a: "operator-note".into(),
                seq_a: 3,
                topic_b: "operator-note".into(),
                seq_b: 99,
                reason: "nope".into(),
            },
            // unknown topic — dropped.
            RawConflict {
                topic_a: "ghost".into(),
                seq_a: 1,
                topic_b: "ghost".into(),
                seq_b: 2,
                reason: "nope".into(),
            },
            // self-pair — dropped.
            RawConflict {
                topic_a: "operator-note".into(),
                seq_a: 3,
                topic_b: "operator-note".into(),
                seq_b: 3,
                reason: "nope".into(),
            },
        ];
        let got = ContradictionDetector::validate(&cands, raws, 50);
        assert_eq!(got.len(), 1, "one valid, deduped conflict");
        let c = &got[0];
        assert_eq!(c.a.seq, 3, "older side is a");
        assert_eq!(c.b.seq, 7, "newer side is b");
        assert!(c.a.body.contains("Perth"));
        assert!(c.b.body.contains("Sydney"));
        assert_eq!(c.reason, "two home airports");
    }

    #[test]
    fn validate_detects_cross_topic_conflicts() {
        // The real-world case: the fact lives in two DIFFERENT topics.
        let cands = vec![
            (
                "home-airport".to_string(),
                vec![entry("home-airport", 2, "YPPH is my home airport (Perth)", 100)],
            ),
            (
                "operator-notes".to_string(),
                vec![entry("operator-notes", 5, "home airport is Sydney YSSY", 300)],
            ),
        ];
        let raws = vec![RawConflict {
            topic_a: "operator-notes".into(),
            seq_a: 5,
            topic_b: "home-airport".into(),
            seq_b: 2,
            reason: "two different home airports across topics".into(),
        }];
        let got = ContradictionDetector::validate(&cands, raws, 50);
        assert_eq!(got.len(), 1);
        let c = &got[0];
        // Older side (created 100, home-airport) is a; newer (300) is b.
        assert_eq!((c.a.topic.as_str(), c.a.seq), ("home-airport", 2));
        assert_eq!((c.b.topic.as_str(), c.b.seq), ("operator-notes", 5));
        assert!(c.a.body.contains("Perth"));
        assert!(c.b.body.contains("Sydney"));
    }

    #[tokio::test]
    async fn candidates_skip_internal_and_single_entry_topics() {
        let mem = InMemoryMemory::new();
        mem.put("operator-note", "home airport YPPH").await.unwrap();
        mem.put("operator-note", "home airport YSSY").await.unwrap();
        mem.put("lonely", "only one entry").await.unwrap();
        mem.put("context:pruned:x", "42 pruned").await.unwrap();
        mem.put("context:pruned:x", "43 pruned").await.unwrap();

        // A detector with a dummy model; candidates() doesn't call the LLM.
        let det = ContradictionDetector::new(
            Arc::new(DummyProvider),
            "m",
        );
        let cands = det.candidates(&mem).await;
        let topics: Vec<&str> = cands.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(topics, vec!["operator-note"], "only multi-entry non-internal topic");
    }

    // Minimal provider stub — candidates()/parse()/validate() are the
    // covered surface; detect()'s LLM leg is exercised in integration.
    use async_trait::async_trait;
    struct DummyProvider;
    #[async_trait]
    impl LlmProvider for DummyProvider {
        async fn chat_stream(
            &self,
            _req: LlmRequest<'_>,
            _cancel: &CancellationToken,
        ) -> Result<Box<dyn aivyx_llm::LlmStream>, aivyx_llm::LlmError> {
            Err(aivyx_llm::LlmError::Transport("dummy".into()))
        }
    }
}
