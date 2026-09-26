//! Chapter Lattice (LT.1) — the persistent typed knowledge-graph store.
//!
//! One encrypted row per **directed triple** `(subject, predicate,
//! object)` in [`aivyx_storage::KeyDomain::KnowledgeGraph`], keyed by the
//! NUL-joined canonical triple so re-extracting the same fact upserts
//! rather than duplicates. This module owns only the *storage* of triples
//! (CRUD + adjacency primitives + the per-topic incremental fingerprint);
//! the extractor that *fills* triples from memory + the LLM lands in LT.2,
//! the sweep cadence in LT.3, and the `graph.query` tool in LT.4.
//!
//! Triples are **derived** — the source of truth is always memory. The
//! store is a cache: a missing or stale triple costs only a re-extraction,
//! never correctness, and a corrupt row degrades only the graph (the
//! Studio view + `graph.query` + the opt-in recall source), never memory
//! or recall.
//!
//! ## Keys
//!
//! - **Triple rows** key on `subject\x00predicate\x00object` (all
//!   canonicalized via [`aivyx_ipc::graph::canonical_label`]). Subjects
//!   are non-empty, so a triple key never starts with `\x00`.
//! - **Fingerprint markers** (the incremental-extraction bookkeeping) key
//!   on `\x00fp\x00<canonical-topic>` — the leading `\x00` is what keeps
//!   them out of the triple scan.

use aivyx_storage::DomainHandle;

pub use aivyx_ipc::graph::{
    canonical_label, canonical_predicate, canonical_relation, GraphEntity, GraphPath,
    GraphTriple, RelationVocabulary,
};

/// Prefix byte that marks a non-triple (metadata) row. A canonical triple
/// key starts with the subject's first byte, which is never `\x00`.
const META_PREFIX: u8 = 0;

/// Errors from the graph store. Same shape as the wiki/ledger stores: a
/// storage-layer failure or a (de)serialization failure, carried as
/// detail strings so the caller can log and skip.
#[derive(Debug, thiserror::Error)]
pub enum GraphStoreError {
    #[error("knowledge-graph storage error: {0}")]
    Storage(String),
    #[error("knowledge-graph encode/decode error: {0}")]
    Encode(String),
}

/// Persistent store for directed typed [`GraphTriple`]s.
pub struct PersistentGraphStore {
    storage: DomainHandle,
}

impl PersistentGraphStore {
    pub fn new(storage: DomainHandle) -> Self {
        Self { storage }
    }

    /// Upsert a triple. Subject / predicate / object are canonicalized so
    /// the stored row keys consistently regardless of the caller's casing
    /// or spacing. An empty canonical part is rejected (a triple must
    /// connect two named entities by a named relation).
    pub async fn put_triple(&self, triple: &GraphTriple) -> Result<(), GraphStoreError> {
        let subject = canonical_label(&triple.subject);
        let predicate = canonical_label(&triple.predicate);
        let object = canonical_label(&triple.object);
        if subject.is_empty() || predicate.is_empty() || object.is_empty() {
            return Err(GraphStoreError::Encode(
                "triple subject/predicate/object must be non-empty after canonicalization".into(),
            ));
        }
        let stored = GraphTriple {
            subject: subject.clone(),
            predicate: predicate.clone(),
            object: object.clone(),
            ..triple.clone()
        };
        let bytes = serde_json::to_vec(&stored)
            .map_err(|e| GraphStoreError::Encode(e.to_string()))?;
        self.storage
            .put(&GraphTriple::key(&subject, &predicate, &object), &bytes)
            .await
            .map_err(|e| GraphStoreError::Storage(e.to_string()))
    }

    /// Fetch a specific triple, if present.
    pub async fn get_triple(
        &self,
        subject: &str,
        predicate: &str,
        object: &str,
    ) -> Result<Option<GraphTriple>, GraphStoreError> {
        let key = GraphTriple::key(
            &canonical_label(subject),
            &canonical_label(predicate),
            &canonical_label(object),
        );
        match self.storage.get(&key).await.map_err(|e| GraphStoreError::Storage(e.to_string()))? {
            Some(bytes) => Ok(Some(
                serde_json::from_slice(&bytes).map_err(|e| GraphStoreError::Encode(e.to_string()))?,
            )),
            None => Ok(None),
        }
    }

    /// Delete a specific triple. Idempotent.
    pub async fn delete_triple(
        &self,
        subject: &str,
        predicate: &str,
        object: &str,
    ) -> Result<(), GraphStoreError> {
        let key = GraphTriple::key(
            &canonical_label(subject),
            &canonical_label(predicate),
            &canonical_label(object),
        );
        self.storage.delete(&key).await.map_err(|e| GraphStoreError::Storage(e.to_string()))
    }

    /// Every triple in the graph. Metadata rows (fingerprint markers) and
    /// corrupt rows are skipped — best-effort, so one bad row never blanks
    /// the graph.
    pub async fn all_triples(&self) -> Result<Vec<GraphTriple>, GraphStoreError> {
        let rows = self
            .storage
            .scan_prefix(&[])
            .await
            .map_err(|e| GraphStoreError::Storage(e.to_string()))?;
        Ok(rows
            .iter()
            .filter(|(k, _)| k.first() != Some(&META_PREFIX))
            .filter_map(|(_, v)| serde_json::from_slice(v).ok())
            .collect())
    }

    /// All triples whose **subject** is `entity` (outgoing edges).
    pub async fn out_edges(&self, entity: &str) -> Result<Vec<GraphTriple>, GraphStoreError> {
        let e = canonical_label(entity);
        Ok(self.all_triples().await?.into_iter().filter(|t| t.subject == e).collect())
    }

    /// All triples whose **object** is `entity` (incoming edges).
    pub async fn in_edges(&self, entity: &str) -> Result<Vec<GraphTriple>, GraphStoreError> {
        let e = canonical_label(entity);
        Ok(self.all_triples().await?.into_iter().filter(|t| t.object == e).collect())
    }

    /// The distinct entities (graph nodes) with their degree — how many
    /// triples touch them as subject or object — sorted degree-descending
    /// then name ascending. `kind` is empty in v1 (entity-kind storage is
    /// a future enhancement).
    pub async fn entities(&self) -> Result<Vec<GraphEntity>, GraphStoreError> {
        use std::collections::HashMap;
        let mut deg: HashMap<String, u32> = HashMap::new();
        for t in self.all_triples().await? {
            *deg.entry(t.subject).or_insert(0) += 1;
            *deg.entry(t.object).or_insert(0) += 1;
        }
        let mut out: Vec<GraphEntity> = deg
            .into_iter()
            .map(|(name, degree)| GraphEntity { name, degree, kind: String::new() })
            .collect();
        out.sort_by(|a, b| b.degree.cmp(&a.degree).then_with(|| a.name.cmp(&b.name)));
        Ok(out)
    }

    /// Chapter Lattice (LT.4) — multi-hop typed traversal from `start`.
    /// Follows directed edges per `direction`, optionally filtered to a
    /// single `predicate`, up to `max_hops`, returning the reachable
    /// entities with the typed path to each (capped at `max_results`).
    /// Loads the triple set once and delegates to the pure [`traverse`].
    pub async fn query(
        &self,
        start: &str,
        direction: GraphDirection,
        predicate: Option<&str>,
        max_hops: u32,
        max_results: usize,
    ) -> Result<Vec<GraphPath>, GraphStoreError> {
        let triples = self.all_triples().await?;
        Ok(traverse(&triples, start, direction, predicate, max_hops, max_results))
    }

    /// Chapter Lexicon (LX.2) — re-map every stored triple's predicate
    /// through the controlled vocabulary and **merge** synonym collisions:
    /// when `deploy —requires→ ci` re-maps onto `deploy —depends-on→ ci`,
    /// the two rows collapse into one with **summed `mentions`** and
    /// unioned `source_seqs`. Idempotent (a canonical triple re-maps to
    /// itself, a no-op) and best-effort. Returns the number of synonym
    /// rows re-mapped. The single fix for the fragmentation open-vocabulary
    /// extraction left behind before LX.1.
    pub async fn normalize_predicates(
        &self,
        vocab: &aivyx_ipc::graph::RelationVocabulary,
    ) -> Result<usize, GraphStoreError> {
        use std::collections::{HashMap, HashSet};
        let triples = self.all_triples().await?;
        // canonical (subject, predicate, object) → merged triple.
        let mut groups: HashMap<(String, String, String), GraphTriple> = HashMap::new();
        // Canonical keys that actually changed (a remap or a merge) and so
        // must be (re)written; canonical no-collision rows are left alone.
        let mut changed: HashSet<(String, String, String)> = HashSet::new();
        // Old synonym keys to delete (predicate differed from canonical).
        let mut old_keys: Vec<(String, String, String)> = Vec::new();
        let mut remapped = 0usize;

        for t in &triples {
            // Fold the predicate + (for an inverse phrasing) swap
            // subject↔object so the re-keyed row points the canonical way.
            let (canon, flip) = vocab.canonical_relation(&t.predicate);
            let (subj, obj) = if flip {
                (t.object.clone(), t.subject.clone())
            } else {
                (t.subject.clone(), t.object.clone())
            };
            let key = (subj.clone(), canon.clone(), obj.clone());
            if canon != t.predicate || flip {
                remapped += 1;
                old_keys.push((t.subject.clone(), t.predicate.clone(), t.object.clone()));
                changed.insert(key.clone());
            }
            if let Some(m) = groups.get_mut(&key) {
                // A second row maps to this canonical key → merge.
                m.mentions = m.mentions.saturating_add(t.mentions);
                for s in &t.source_seqs {
                    if !m.source_seqs.contains(s) {
                        m.source_seqs.push(*s);
                    }
                }
                m.updated_at = m.updated_at.max(t.updated_at);
                changed.insert(key.clone());
            } else {
                groups.insert(
                    key.clone(),
                    GraphTriple {
                        subject: subj,
                        predicate: canon,
                        object: obj,
                        ..t.clone()
                    },
                );
            }
        }
        // Delete the synonym rows first, then write the merged canonical
        // rows (only those that changed) — sorted seqs for determinism.
        for (s, p, o) in old_keys {
            self.delete_triple(&s, &p, &o).await?;
        }
        for (key, mut t) in groups {
            if changed.contains(&key) {
                t.source_seqs.sort_unstable();
                t.source_seqs.dedup();
                self.put_triple(&t).await?;
            }
        }
        Ok(remapped)
    }

    // ---- incremental-extraction bookkeeping (per topic) ----------------

    fn fingerprint_key(topic: &str) -> Vec<u8> {
        let mut k = vec![META_PREFIX];
        k.extend_from_slice(b"fp");
        k.push(META_PREFIX);
        k.extend_from_slice(canonical_label(topic).as_bytes());
        k
    }

    /// Record that `topic` was extracted at the given source fingerprint.
    pub async fn set_topic_fingerprint(
        &self,
        topic: &str,
        fingerprint: u64,
    ) -> Result<(), GraphStoreError> {
        self.storage
            .put(&Self::fingerprint_key(topic), &fingerprint.to_le_bytes())
            .await
            .map_err(|e| GraphStoreError::Storage(e.to_string()))
    }

    /// The fingerprint a topic was last extracted at, if any.
    pub async fn topic_fingerprint(&self, topic: &str) -> Result<Option<u64>, GraphStoreError> {
        let raw = self
            .storage
            .get(&Self::fingerprint_key(topic))
            .await
            .map_err(|e| GraphStoreError::Storage(e.to_string()))?;
        Ok(raw.and_then(|b| b.try_into().ok().map(u64::from_le_bytes)))
    }

    /// Whether a topic needs (re)extraction given its current entry
    /// fingerprint: `true` when it was never extracted or the fingerprint
    /// changed. A storage error fails toward freshness.
    pub async fn needs_regen(&self, topic: &str, fingerprint: u64) -> bool {
        match self.topic_fingerprint(topic).await {
            Ok(Some(fp)) => fp != fingerprint,
            _ => true,
        }
    }
}

// ---------------------------------------------------------------------------
// Traversal — Chapter Lattice (LT.4)
// ---------------------------------------------------------------------------

/// Which way to follow a directed edge from the current entity during a
/// [`traverse`]: outgoing (`subject → object`), incoming (`object →
/// subject`), or both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphDirection {
    Out,
    In,
    Both,
}

impl GraphDirection {
    /// Parse the tool's string argument; unknown / missing → `Out`.
    pub fn from_arg(s: Option<&str>) -> Self {
        match s.map(|x| x.trim().to_lowercase()).as_deref() {
            Some("in") => GraphDirection::In,
            Some("both") => GraphDirection::Both,
            _ => GraphDirection::Out,
        }
    }
}

/// Pure multi-hop BFS over a triple set. From `start`, follow edges per
/// `direction` (optionally only edges whose predicate equals
/// `predicate`), up to `max_hops`, recording the typed path (the
/// predicate labels) to each newly-reached entity. Each entity is
/// reported once, at its **shortest** path (BFS order); the start entity
/// is never reported. Deterministic: adjacency is built from the
/// caller-ordered `triples`, the frontier is processed in sorted order,
/// and the output is sorted (hops asc, then entity asc) before the
/// `max_results` cut.
pub fn traverse(
    triples: &[GraphTriple],
    start: &str,
    direction: GraphDirection,
    predicate: Option<&str>,
    max_hops: u32,
    max_results: usize,
) -> Vec<GraphPath> {
    use std::collections::{HashMap, HashSet};
    if max_hops == 0 || max_results == 0 {
        return Vec::new();
    }
    let start = canonical_label(start);
    // Chapter Lexicon — fold the filter predicate into the controlled
    // vocabulary too, so filtering by "requires" matches "depends-on" edges.
    let pred = predicate.map(canonical_predicate).filter(|p| !p.is_empty());

    // Adjacency: entity → Vec<(predicate, neighbor)>, honoring direction +
    // the optional predicate filter.
    let mut adj: HashMap<&str, Vec<(&str, &str)>> = HashMap::new();
    for t in triples {
        if let Some(p) = &pred {
            if &t.predicate != p {
                continue;
            }
        }
        if matches!(direction, GraphDirection::Out | GraphDirection::Both) {
            adj.entry(&t.subject).or_default().push((&t.predicate, &t.object));
        }
        if matches!(direction, GraphDirection::In | GraphDirection::Both) {
            adj.entry(&t.object).or_default().push((&t.predicate, &t.subject));
        }
    }
    // Sort each adjacency list for deterministic frontier order.
    for v in adj.values_mut() {
        v.sort();
    }

    let mut seen: HashSet<String> = HashSet::new();
    seen.insert(start.clone());
    // BFS frontier: (entity, path-of-predicates-to-it).
    let mut frontier: Vec<(String, Vec<String>)> = vec![(start, Vec::new())];
    let mut out: Vec<GraphPath> = Vec::new();

    for hop in 1..=max_hops {
        if frontier.is_empty() {
            break;
        }
        let mut next: Vec<(String, Vec<String>)> = Vec::new();
        for (entity, path) in &frontier {
            let Some(edges) = adj.get(entity.as_str()) else { continue };
            for (predicate, neighbor) in edges {
                if !seen.insert((*neighbor).to_string()) {
                    continue; // already reached at an equal-or-shorter path
                }
                let mut p = path.clone();
                p.push((*predicate).to_string());
                out.push(GraphPath { entity: (*neighbor).to_string(), hops: hop, path: p.clone() });
                next.push(((*neighbor).to_string(), p));
            }
        }
        frontier = next;
    }

    out.sort_by(|a, b| a.hops.cmp(&b.hops).then_with(|| a.entity.cmp(&b.entity)));
    out.truncate(max_results);
    out
}

/// Whether a canonicalized (lowercased) entity is *conversation
/// mechanics* rather than domain knowledge. Small local models routinely
/// "extract" triples about the chat itself — `conversation history
/// --[contains]--> messages`, `assistant --[tool_call]--> web_search`,
/// `1 messages --[pruned-from]--> conversation history` — which is pure
/// noise in a knowledge graph about the user and their world. We drop any
/// triple touching such an entity on either end. Observed live on the
/// dogfood rig; the substrings cover the recurring offenders without
/// catching real noun phrases (a PA's notes rarely name "the
/// conversation" as a subject of fact).
pub(crate) fn is_mechanical_entity(e: &str) -> bool {
    const MECH_SUBSTR: &[&str] =
        &["conversation", "message", "pruned", "assistant"];
    if MECH_SUBSTR.iter().any(|m| e.contains(m)) {
        return true;
    }
    const MECH_EXACT: &[&str] = &[
        "memory",
        "relevant context",
        "context",
        "note",
        "notes",
        "chat",
        "chat history",
        "tool",
        "tools",
        "tool call",
        "web_search",
    ];
    MECH_EXACT.contains(&e)
}

/// Whether a canonicalized predicate describes a *mechanics* relation
/// (the bookkeeping of pruning / tool dispatch) rather than a fact.
pub(crate) fn is_mechanical_predicate(p: &str) -> bool {
    p.contains("prune")
        || p.contains("recall")
        || p == "tool_call"
        || p == "tool-call"
}

// ---------------------------------------------------------------------------
// GraphExtractor — Chapter Lattice (LT.2)
// ---------------------------------------------------------------------------

use std::sync::Arc;

use aivyx_core::CancellationToken;
use aivyx_llm::{ContentBlock, LlmMessage, LlmProvider, LlmRequest, LlmStepEnd};
use aivyx_memory::{Memory, MemoryEntry};
use serde::Deserialize;

/// Tuning for triple extraction. Defaults aim for a cheap, bounded pass.
#[derive(Debug, Clone)]
pub struct GraphExtractConfig {
    /// Newest entries (per topic) to extract from.
    pub max_entries: usize,
    /// Per-entry body cap (chars) in the prompt.
    pub max_entry_chars: usize,
    /// LLM token budget for the triple list.
    pub max_tokens: u32,
    /// Hard cap on triples kept per topic per pass (defends a runaway
    /// model dump).
    pub max_triples: usize,
}

impl Default for GraphExtractConfig {
    fn default() -> Self {
        Self { max_entries: 50, max_entry_chars: 500, max_tokens: 700, max_triples: 64 }
    }
}

/// What an extraction pass did for one topic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphRegenOutcome {
    /// Up to date (fingerprint matched) — nothing re-extracted.
    Skipped,
    /// The topic has no entries to extract from.
    NoEntries,
    /// `n` triples were (re)extracted and stored.
    Wrote(usize),
}

/// One LLM-emitted triple, before canonicalization/validation.
#[derive(Debug, Deserialize)]
struct RawTriple {
    #[serde(default)]
    subject: String,
    #[serde(default)]
    predicate: String,
    #[serde(default)]
    object: String,
}

/// Extracts directed typed triples from memory into the graph store.
/// Best-effort throughout (no provider, an LLM error, an unparseable
/// response, or a storage hiccup leaves the existing graph untouched) and
/// incremental (a topic whose entries are unchanged is skipped).
pub struct GraphExtractor {
    memory: Arc<dyn Memory>,
    provider: Arc<dyn LlmProvider>,
    store: Arc<PersistentGraphStore>,
    model: String,
    config: GraphExtractConfig,
    /// Chapter Lexicon — built-in vocabulary + operator `[graph.vocabulary]`
    /// extensions. Default (empty) ⇒ byte-identical to the built-in lexicon.
    vocab: aivyx_ipc::graph::RelationVocabulary,
}

impl GraphExtractor {
    pub fn new(
        memory: Arc<dyn Memory>,
        provider: Arc<dyn LlmProvider>,
        store: Arc<PersistentGraphStore>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            memory,
            provider,
            store,
            model: model.into(),
            config: GraphExtractConfig::default(),
            vocab: aivyx_ipc::graph::RelationVocabulary::default(),
        }
    }

    pub fn with_config(mut self, config: GraphExtractConfig) -> Self {
        self.config = config;
        self
    }

    /// Chapter Lexicon — supply operator `[graph.vocabulary]` extensions.
    pub fn with_vocabulary(
        mut self,
        vocab: aivyx_ipc::graph::RelationVocabulary,
    ) -> Self {
        self.vocab = vocab;
        self
    }

    fn system_prompt() -> &'static str {
        "You extract a knowledge graph from an AI assistant's memory notes \
         about one topic. Output ONLY a JSON array of directed relation \
         triples, each `{\"subject\":\"...\",\"predicate\":\"...\",\"object\":\"...\"}`. \
         The predicate is a short directed relation label; direction \
         matters (subject → object). PREFER these canonical relation \
         types when one fits: depends-on, uses, causes, part-of, contains, \
         related-to, located-in, created-by, produces, instance-of, \
         replaces, owns, precedes, follows. If none fits, use a short \
         relation label of your own. Extract ONLY relations the notes \
         actually state — do not invent, infer beyond the text, or add \
         commentary. Extract DOMAIN knowledge about the user and the world \
         the notes describe — NEVER facts about the conversation itself, \
         messages, the assistant, memory, or which tools were called; ignore \
         any note that merely records that messages were pruned or which \
         tool ran. Use concise noun-phrase entities. If the notes state \
         no clear relations, output `[]`. No prose, no markdown fences."
    }

    /// Render entries into the user prompt. Pure + testable.
    fn user_prompt(&self, topic: &str, entries: &[MemoryEntry]) -> String {
        let mut s = format!("Topic: {topic}\n\nNotes:\n");
        for e in entries {
            let body: String = if e.body.chars().count() > self.config.max_entry_chars {
                e.body.chars().take(self.config.max_entry_chars).collect::<String>() + "…"
            } else {
                e.body.clone()
            };
            s.push_str(&format!("- {}\n", body.replace('\n', " ")));
        }
        s.push_str("\nOutput the JSON triple array now.");
        s
    }

    /// Tolerant parse of the model's response into raw triples: locate the
    /// outermost `[ … ]` (ignoring any prose/fences around it) and decode.
    /// Returns an empty vec on any failure — best-effort.
    fn parse_triples(raw: &str) -> Vec<RawTriple> {
        let (Some(start), Some(end)) = (raw.find('['), raw.rfind(']')) else {
            return Vec::new();
        };
        if end <= start {
            return Vec::new();
        }
        serde_json::from_str::<Vec<RawTriple>>(&raw[start..=end]).unwrap_or_default()
    }

    /// One-shot LLM extraction → validated, deduped `(s, p, o, count)`
    /// triples (count = how many times the model emitted the same triple
    /// in this batch → the `mentions` weight). Best-effort.
    async fn extract(
        &self,
        topic: &str,
        entries: &[MemoryEntry],
    ) -> Vec<(String, String, String, u32)> {
        let user = self.user_prompt(topic, entries);
        let messages = vec![LlmMessage::User { content: vec![ContentBlock::Text { text: user }] }];
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

        // Validate + canonicalize + dedup (counting repeats as mentions).
        use std::collections::HashMap;
        let mut counts: HashMap<(String, String, String), u32> = HashMap::new();
        for rt in Self::parse_triples(&text) {
            // Chapter Lexicon — fold the predicate into the controlled
            // vocabulary so synonyms (depends on / requires / needs) store
            // as one canonical relation type; unknowns keep their label. An
            // INVERSE phrasing (`X owned-by Y`) folds to the forward type +
            // a subject↔object swap so the stored direction is canonical.
            let (p, flip) = self.vocab.canonical_relation(&rt.predicate);
            let (s, o) = if flip {
                (canonical_label(&rt.object), canonical_label(&rt.subject))
            } else {
                (canonical_label(&rt.subject), canonical_label(&rt.object))
            };
            // Reject empties and self-loops (an entity related to itself
            // by the same name is noise).
            if s.is_empty() || p.is_empty() || o.is_empty() || s == o {
                continue;
            }
            // #B — drop conversation-mechanics triples (chat/messages/tool
            // bookkeeping) the small model extracts about the session
            // itself rather than the user's domain.
            if is_mechanical_entity(&s)
                || is_mechanical_entity(&o)
                || is_mechanical_predicate(&p)
            {
                continue;
            }
            *counts.entry((s, p, o)).or_insert(0) += 1;
        }
        let mut out: Vec<(String, String, String, u32)> =
            counts.into_iter().map(|((s, p, o), n)| (s, p, o, n)).collect();
        // Deterministic order, then cap.
        out.sort();
        out.truncate(self.config.max_triples);
        out
    }

    /// Make a topic's triples current: pulls its entries, skips when the
    /// fingerprint matches (incremental), otherwise extracts + stores the
    /// triples and records the new fingerprint. Best-effort: any soft
    /// failure returns [`GraphRegenOutcome::Skipped`].
    pub async fn regenerate(&self, topic: &str, now_secs: u64) -> GraphRegenOutcome {
        let entries = match self.memory.get_recent(topic, self.config.max_entries).await {
            Ok(e) => e,
            Err(_) => return GraphRegenOutcome::Skipped,
        };
        if entries.is_empty() {
            return GraphRegenOutcome::NoEntries;
        }
        let seqs: Vec<u64> = entries.iter().map(|e| e.seq).collect();
        let fingerprint = aivyx_ipc::wiki::WikiPage::fingerprint(&seqs);
        if !self.store.needs_regen(topic, fingerprint).await {
            return GraphRegenOutcome::Skipped;
        }
        let triples = self.extract(topic, &entries).await;
        let mut wrote = 0usize;
        for (subject, predicate, object, mentions) in triples {
            let t = GraphTriple {
                subject,
                predicate,
                object,
                source_seqs: seqs.clone(),
                mentions,
                updated_at: now_secs,
            };
            if self.store.put_triple(&t).await.is_ok() {
                wrote += 1;
            }
        }
        // Record the fingerprint even when zero triples were found, so a
        // topic with no relations isn't re-extracted every sweep.
        let _ = self.store.set_topic_fingerprint(topic, fingerprint).await;
        GraphRegenOutcome::Wrote(wrote)
    }

    /// Re-extract every topic whose entries have changed, capped at
    /// `max_topics` **extractions** (LLM calls) per sweep so one pass
    /// can't fire an unbounded number of calls. Already-current topics
    /// are cheap fingerprint checks and don't count against the cap.
    /// Best-effort: a `list_topics` failure returns an empty report.
    pub async fn sweep(&self, now_secs: u64, max_topics: usize) -> GraphSweepReport {
        let mut report = GraphSweepReport::default();
        let topics = match self.memory.list_topics().await {
            Ok(t) => t,
            Err(_) => return report,
        };
        for topic in topics {
            if report.extracted >= max_topics {
                break;
            }
            // #11 — skip internal/machine topics (the per-session
            // `context:pruned:*` archives): extracting triples from "N messages
            // were pruned from conversation history" pollutes the knowledge graph.
            if crate::prune_sink::is_internal_topic(&topic) {
                continue;
            }
            report.scanned += 1;
            match self.regenerate(&topic, now_secs).await {
                GraphRegenOutcome::Wrote(n) => {
                    report.extracted += 1;
                    report.triples += n;
                }
                GraphRegenOutcome::Skipped => report.skipped += 1,
                GraphRegenOutcome::NoEntries => report.no_entries += 1,
            }
        }
        // Chapter Lexicon (LX.2) — fold any pre-LX.1 free-text predicates
        // into the controlled vocabulary, merging synonym collisions.
        // Best-effort + idempotent, so it's safe to run every sweep.
        report.remapped =
            self.store.normalize_predicates(&self.vocab).await.unwrap_or(0);
        report
    }
}

/// Tally of one [`GraphExtractor::sweep`] pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GraphSweepReport {
    /// Topics examined this pass.
    pub scanned: usize,
    /// Topics (re)extracted (one LLM call each).
    pub extracted: usize,
    /// Topics already up to date (fingerprint matched) or soft-failed.
    pub skipped: usize,
    /// Topics with no entries.
    pub no_entries: usize,
    /// Total triples written this pass.
    pub triples: usize,
    /// Chapter Lexicon (LX.2) — synonym predicates re-mapped to canonical
    /// types this pass (the merge of pre-LX.1 free-text relations).
    pub remapped: usize,
}

/// What the daemon needs to run the graph sweep loop: a ready extractor
/// plus the cadence knobs from `[graph]`. Built by the binary (it owns
/// the storage + provider) and passed into `DaemonConfig`; `None` there ⇒
/// no extraction (the byte-identical default).
pub struct GraphSweepConfig {
    pub extractor: Arc<GraphExtractor>,
    pub interval_secs: u64,
    pub max_topics: usize,
}

/// Background generation trigger (LT.3) — periodically extract the graph
/// from churned topics on the daemon's maintenance cadence. Best-effort,
/// first-tick-skipped, shutdown-aware (the same shape as the wiki sweep).
pub async fn run_graph_sweep_loop(
    extractor: Arc<GraphExtractor>,
    interval_secs: u64,
    max_topics: usize,
    shutdown: CancellationToken,
) {
    let mut interval =
        tokio::time::interval(std::time::Duration::from_secs(interval_secs.max(1)));
    interval.tick().await; // skip the immediate first tick
    loop {
        tokio::select! {
            _ = interval.tick() => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let r = extractor.sweep(now, max_topics).await;
                if r.triples > 0 {
                    eprintln!(
                        "aivyx-pa graph-sweep: {} triple(s) from {} topic(s) ({} scanned)",
                        r.triples, r.extracted, r.scanned
                    );
                }
            }
            _ = shutdown.cancelled() => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mechanical_entities_and_predicates_are_rejected() {
        // The exact noise observed on the dogfood rig's graph.
        for noise in [
            "conversation history",
            "messages",
            "1 messages",
            "2 messages",
            "assistant",
            "assistant memory",
            "assistant's memory",
            "relevant context",
            "notes",
            "web_search",
        ] {
            assert!(is_mechanical_entity(noise), "{noise} should be mechanical");
        }
        // Real domain entities must survive.
        for keep in [
            "flat white",
            "home airport",
            "ethiopian single-origin beans",
            "sydney",
            "ashwagandha",
        ] {
            assert!(!is_mechanical_entity(keep), "{keep} should be kept");
        }
        assert!(is_mechanical_predicate("pruned-from"));
        assert!(is_mechanical_predicate("recalled-from"));
        assert!(is_mechanical_predicate("tool_call"));
        assert!(!is_mechanical_predicate("located-in"));
        assert!(!is_mechanical_predicate("uses"));
    }
    use std::sync::Arc;

    async fn store() -> PersistentGraphStore {
        use aivyx_crypto::MasterKey;
        use aivyx_storage::{KeyDomain, RedbStorage, Storage, StorageConfig};
        let base = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".into());
        let dir = std::path::PathBuf::from(base)
            .join(format!("aivyx-graph-store-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let s: Arc<dyn Storage> = RedbStorage::open(
            StorageConfig::new(dir.join("store.redb")),
            MasterKey::from_raw([76u8; 32]),
        )
        .await
        .unwrap();
        PersistentGraphStore::new(s.domain(KeyDomain::KnowledgeGraph))
    }

    fn triple(s: &str, p: &str, o: &str) -> GraphTriple {
        GraphTriple {
            subject: s.into(),
            predicate: p.into(),
            object: o.into(),
            source_seqs: vec![1],
            mentions: 1,
            updated_at: 1,
        }
    }

    #[tokio::test]
    async fn put_get_round_trips_and_canonicalizes() {
        let g = store().await;
        g.put_triple(&triple("Deploy", "Depends-On", "CI")).await.unwrap();
        // Differently-cased lookup finds the canonical row.
        let got = g.get_triple("deploy", "depends-on", "ci").await.unwrap().expect("present");
        assert_eq!(got.subject, "deploy");
        assert_eq!(got.predicate, "depends-on");
        assert_eq!(got.object, "ci");
    }

    #[tokio::test]
    async fn empty_part_rejected() {
        let g = store().await;
        assert!(g.put_triple(&triple("deploy", "  ", "ci")).await.is_err());
    }

    #[tokio::test]
    async fn out_and_in_edges_respect_direction() {
        let g = store().await;
        g.put_triple(&triple("deploy", "depends-on", "ci")).await.unwrap();
        g.put_triple(&triple("ci", "triggers", "rollback")).await.unwrap();
        g.put_triple(&triple("rollback", "reverts", "deploy")).await.unwrap();

        let out = g.out_edges("ci").await.unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].object, "rollback");

        let into = g.in_edges("deploy").await.unwrap();
        assert_eq!(into.len(), 1);
        assert_eq!(into[0].subject, "rollback");
    }

    #[tokio::test]
    async fn entities_count_degree() {
        let g = store().await;
        g.put_triple(&triple("deploy", "depends-on", "ci")).await.unwrap();
        g.put_triple(&triple("deploy", "uses", "docker")).await.unwrap();
        let ents = g.entities().await.unwrap();
        // deploy touches 2 triples → highest degree, listed first.
        assert_eq!(ents[0].name, "deploy");
        assert_eq!(ents[0].degree, 2);
        assert!(ents.iter().any(|e| e.name == "ci" && e.degree == 1));
        assert!(ents.iter().any(|e| e.name == "docker" && e.degree == 1));
    }

    #[tokio::test]
    async fn delete_removes_only_that_triple() {
        let g = store().await;
        g.put_triple(&triple("deploy", "depends-on", "ci")).await.unwrap();
        g.put_triple(&triple("deploy", "uses", "docker")).await.unwrap();
        g.delete_triple("Deploy", "depends-on", "CI").await.unwrap();
        assert!(g.get_triple("deploy", "depends-on", "ci").await.unwrap().is_none());
        assert!(g.get_triple("deploy", "uses", "docker").await.unwrap().is_some());
        assert_eq!(g.all_triples().await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn fingerprint_markers_dont_leak_into_triples() {
        let g = store().await;
        g.put_triple(&triple("deploy", "depends-on", "ci")).await.unwrap();
        g.set_topic_fingerprint("deploy", 42).await.unwrap();
        // The marker is invisible to the triple scan...
        assert_eq!(g.all_triples().await.unwrap().len(), 1);
        // ...but readable for the incremental check.
        assert_eq!(g.topic_fingerprint("Deploy").await.unwrap(), Some(42));
        assert!(!g.needs_regen("deploy", 42).await);
        assert!(g.needs_regen("deploy", 43).await);
        assert!(g.needs_regen("never-extracted", 1).await);
    }

    // ---- LT.4 — traversal -------------------------------------------

    fn chain() -> Vec<GraphTriple> {
        // deploy -depends-on-> ci -triggers-> rollback ; deploy -uses-> docker
        vec![
            triple("deploy", "depends-on", "ci"),
            triple("ci", "triggers", "rollback"),
            triple("deploy", "uses", "docker"),
        ]
    }

    #[test]
    fn traverse_out_multi_hop_with_paths() {
        let t = chain();
        let r = traverse(&t, "deploy", GraphDirection::Out, None, 3, 50);
        // ci(1), docker(1), rollback(2).
        let names: Vec<&str> = r.iter().map(|p| p.entity.as_str()).collect();
        assert_eq!(names, vec!["ci", "docker", "rollback"]);
        let rb = r.iter().find(|p| p.entity == "rollback").unwrap();
        assert_eq!(rb.hops, 2);
        assert_eq!(rb.path, vec!["depends-on", "triggers"]);
    }

    #[test]
    fn traverse_hop_cap_limits_depth() {
        let t = chain();
        let r = traverse(&t, "deploy", GraphDirection::Out, None, 1, 50);
        // Only direct neighbors at 1 hop; rollback (2 hops) excluded.
        assert!(!r.iter().any(|p| p.entity == "rollback"));
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn traverse_predicate_filter_and_direction() {
        let t = chain();
        // Only "uses" edges out of deploy → docker.
        let r = traverse(&t, "deploy", GraphDirection::Out, Some("uses"), 3, 50);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].entity, "docker");
        // Incoming edges of rollback → ci.
        let inb = traverse(&t, "rollback", GraphDirection::In, None, 1, 50);
        assert_eq!(inb.len(), 1);
        assert_eq!(inb[0].entity, "ci");
    }

    #[test]
    fn traverse_is_cycle_safe_and_deterministic() {
        let cyc = vec![
            triple("a", "r", "b"),
            triple("b", "r", "c"),
            triple("c", "r", "a"), // cycle back to a
        ];
        let r = traverse(&cyc, "a", GraphDirection::Out, None, 10, 50);
        // a is the start (never reported); b, c reached once each → no loop.
        let names: Vec<&str> = r.iter().map(|p| p.entity.as_str()).collect();
        assert_eq!(names, vec!["b", "c"]);
        // Deterministic across runs.
        for _ in 0..3 {
            assert_eq!(traverse(&cyc, "a", GraphDirection::Out, None, 10, 50), r);
        }
    }

    #[tokio::test]
    async fn store_query_delegates_to_traverse() {
        let g = store().await;
        for t in chain() {
            g.put_triple(&t).await.unwrap();
        }
        let r = g.query("Deploy", GraphDirection::Out, None, 3, 50).await.unwrap();
        assert!(r.iter().any(|p| p.entity == "rollback" && p.hops == 2));
    }

    // ---- LT.2 — GraphExtractor --------------------------------------

    use aivyx_llm::{
        LlmError, LlmRequest, LlmStepEnd, LlmStream, LlmStreamEvent, LlmUsage,
    };
    use aivyx_memory::InMemoryMemory;
    use async_trait::async_trait;

    struct ScriptedProvider {
        text: String,
        fail: bool,
    }
    struct ScriptedStream {
        text: String,
    }
    #[async_trait]
    impl LlmProvider for ScriptedProvider {
        async fn chat_stream(
            &self,
            _request: LlmRequest<'_>,
            _cancellation: &CancellationToken,
        ) -> Result<Box<dyn LlmStream>, LlmError> {
            if self.fail {
                return Err(LlmError::Transport("scripted failure".into()));
            }
            Ok(Box::new(ScriptedStream { text: self.text.clone() }))
        }
    }
    #[async_trait]
    impl LlmStream for ScriptedStream {
        async fn next_event(&mut self) -> Result<Option<LlmStreamEvent>, LlmError> {
            Ok(None)
        }
        async fn finish(self: Box<Self>) -> Result<LlmStepEnd, LlmError> {
            Ok(LlmStepEnd::FinalMessage { text: self.text, usage: LlmUsage::default() })
        }
    }

    fn extractor(
        memory: Arc<dyn Memory>,
        store: Arc<PersistentGraphStore>,
        text: &str,
        fail: bool,
    ) -> GraphExtractor {
        GraphExtractor::new(
            memory,
            Arc::new(ScriptedProvider { text: text.into(), fail }),
            store,
            "fake-model",
        )
    }

    #[test]
    fn parse_triples_tolerates_prose_and_fences() {
        let raw = "Sure! Here are the triples:\n```json\n\
            [{\"subject\":\"deploy\",\"predicate\":\"depends-on\",\"object\":\"ci\"}]\n```";
        let parsed = GraphExtractor::parse_triples(raw);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].subject, "deploy");
        // Garbage → empty, never panics.
        assert!(GraphExtractor::parse_triples("no json here").is_empty());
        assert!(GraphExtractor::parse_triples("[").is_empty());
    }

    #[tokio::test]
    async fn regenerate_extracts_and_stores_directed_triples() {
        let mem: Arc<dyn Memory> = Arc::new(InMemoryMemory::new());
        mem.put("deploy", "we ship via the ci pipeline").await.unwrap();
        mem.put("deploy", "rollback reverts the deploy").await.unwrap();
        let g = Arc::new(store().await);
        let resp = r#"[
            {"subject":"deploy","predicate":"depends-on","object":"ci"},
            {"subject":"rollback","predicate":"reverts","object":"deploy"},
            {"subject":"deploy","predicate":"is","object":"deploy"}
        ]"#; // the self-loop must be dropped
        let x = extractor(Arc::clone(&mem), Arc::clone(&g), resp, false);

        match x.regenerate("deploy", 500).await {
            GraphRegenOutcome::Wrote(n) => assert_eq!(n, 2, "self-loop dropped"),
            other => panic!("expected Wrote, got {other:?}"),
        }
        let t = g.get_triple("deploy", "depends-on", "ci").await.unwrap().expect("stored");
        assert_eq!(t.source_seqs.len(), 2);
        assert_eq!(t.updated_at, 500);
        assert!(g.get_triple("rollback", "reverts", "deploy").await.unwrap().is_some());
        // Direction-sensitive: the reverse wasn't asserted.
        assert!(g.get_triple("ci", "depends-on", "deploy").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn regenerate_is_incremental() {
        let mem: Arc<dyn Memory> = Arc::new(InMemoryMemory::new());
        mem.put("deploy", "ship via ci").await.unwrap();
        let g = Arc::new(store().await);
        let x = extractor(
            Arc::clone(&mem),
            Arc::clone(&g),
            r#"[{"subject":"deploy","predicate":"uses","object":"ci"}]"#,
            false,
        );
        assert!(matches!(x.regenerate("deploy", 1).await, GraphRegenOutcome::Wrote(1)));
        // Unchanged entries → skipped (no second LLM pass).
        assert_eq!(x.regenerate("deploy", 2).await, GraphRegenOutcome::Skipped);
        // New entry → re-extracts.
        mem.put("deploy", "deploy uses docker too").await.unwrap();
        assert!(matches!(x.regenerate("deploy", 3).await, GraphRegenOutcome::Wrote(_)));
    }

    #[tokio::test]
    async fn regenerate_best_effort_on_failure_and_no_entries() {
        let mem: Arc<dyn Memory> = Arc::new(InMemoryMemory::new());
        let g = Arc::new(store().await);
        // No entries.
        let x = extractor(Arc::clone(&mem), Arc::clone(&g), "[]", false);
        assert_eq!(x.regenerate("empty", 1).await, GraphRegenOutcome::NoEntries);
        // LLM failure → Wrote(0), no triples, never errors.
        mem.put("deploy", "note").await.unwrap();
        let xf = extractor(Arc::clone(&mem), Arc::clone(&g), "", true);
        assert_eq!(xf.regenerate("deploy", 1).await, GraphRegenOutcome::Wrote(0));
        assert!(g.all_triples().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn sweep_extracts_stale_then_skips_and_caps() {
        let mem: Arc<dyn Memory> = Arc::new(InMemoryMemory::new());
        mem.put("deploy", "ship via ci").await.unwrap();
        mem.put("billing", "stripe charges the card").await.unwrap();
        let g = Arc::new(store().await);
        let x = extractor(
            Arc::clone(&mem),
            Arc::clone(&g),
            r#"[{"subject":"a","predicate":"r","object":"b"}]"#,
            false,
        );

        let first = x.sweep(1, 50).await;
        assert_eq!(first.scanned, 2);
        assert_eq!(first.extracted, 2);
        assert_eq!(first.triples, 2);

        // Unchanged → all skipped.
        let second = x.sweep(2, 50).await;
        assert_eq!(second.extracted, 0);
        assert_eq!(second.skipped, 2);
    }

    #[tokio::test]
    async fn sweep_caps_extractions_per_pass() {
        let mem: Arc<dyn Memory> = Arc::new(InMemoryMemory::new());
        for t in ["a", "b", "c"] {
            mem.put(t, "x relates to y").await.unwrap();
        }
        let g = Arc::new(store().await);
        let x = extractor(
            Arc::clone(&mem),
            Arc::clone(&g),
            r#"[{"subject":"x","predicate":"relates-to","object":"y"}]"#,
            false,
        );
        let r = x.sweep(1, 2).await;
        assert_eq!(r.extracted, 2, "capped at 2 LLM calls");
        let r2 = x.sweep(2, 2).await;
        assert_eq!(r2.extracted, 1, "the remaining topic next pass");
    }

    #[tokio::test]
    async fn sweep_loop_first_tick_skipped_and_cancels() {
        let mem: Arc<dyn Memory> = Arc::new(InMemoryMemory::new());
        mem.put("deploy", "ship via ci").await.unwrap();
        let g = Arc::new(store().await);
        let x = Arc::new(extractor(
            Arc::clone(&mem),
            Arc::clone(&g),
            r#"[{"subject":"deploy","predicate":"uses","object":"ci"}]"#,
            false,
        ));
        let shutdown = CancellationToken::new();
        let handle =
            tokio::spawn(run_graph_sweep_loop(Arc::clone(&x), 3600, 10, shutdown.clone()));
        assert!(g.all_triples().await.unwrap().is_empty(), "first tick skipped");
        shutdown.cancel();
        handle.await.unwrap();
    }

    // ---- LX.2 — re-normalize existing triples (merge) ---------------

    #[tokio::test]
    async fn normalize_merges_synonym_edges_into_one() {
        let g = store().await;
        // Two synonym edges of the same relation + the canonical one.
        g.put_triple(&GraphTriple {
            subject: "deploy".into(),
            predicate: "requires".into(),
            object: "ci".into(),
            source_seqs: vec![1],
            mentions: 2,
            updated_at: 10,
        })
        .await
        .unwrap();
        g.put_triple(&GraphTriple {
            subject: "deploy".into(),
            predicate: "needs".into(),
            object: "ci".into(),
            source_seqs: vec![2, 1],
            mentions: 3,
            updated_at: 20,
        })
        .await
        .unwrap();
        g.put_triple(&GraphTriple {
            subject: "deploy".into(),
            predicate: "depends-on".into(),
            object: "ci".into(),
            source_seqs: vec![3],
            mentions: 1,
            updated_at: 5,
        })
        .await
        .unwrap();

        let remapped = g.normalize_predicates(&aivyx_ipc::graph::RelationVocabulary::default()).await.unwrap();
        assert_eq!(remapped, 2, "requires + needs remapped");
        // All three collapse into one canonical edge.
        let all = g.all_triples().await.unwrap();
        assert_eq!(all.len(), 1);
        let t = &all[0];
        assert_eq!(t.predicate, "depends-on");
        assert_eq!(t.mentions, 6, "2 + 3 + 1 summed");
        assert_eq!(t.source_seqs, vec![1, 2, 3], "unioned + sorted");
        assert_eq!(t.updated_at, 20, "max");
        // The synonym rows are gone.
        assert!(g.get_triple("deploy", "requires", "ci").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn normalize_is_idempotent_and_leaves_unknowns() {
        let g = store().await;
        g.put_triple(&triple("deploy", "requires", "ci")).await.unwrap();
        g.put_triple(&triple("alice", "rivals", "bob")).await.unwrap(); // not in lexicon

        let first = g.normalize_predicates(&aivyx_ipc::graph::RelationVocabulary::default()).await.unwrap();
        assert_eq!(first, 1, "only 'requires' is a synonym");
        // Second pass: nothing left to remap.
        assert_eq!(g.normalize_predicates(&aivyx_ipc::graph::RelationVocabulary::default()).await.unwrap(), 0);
        // The unknown relation is untouched (open-world).
        assert!(g.get_triple("alice", "rivals", "bob").await.unwrap().is_some());
        assert!(g.get_triple("deploy", "depends-on", "ci").await.unwrap().is_some());
    }

    // ---- LX.1 — lexicon folding at extraction + query ----------------

    #[tokio::test]
    async fn extraction_folds_predicate_synonyms_to_canonical() {
        let mem: Arc<dyn Memory> = Arc::new(InMemoryMemory::new());
        mem.put("deploy", "the deploy requires ci").await.unwrap();
        let g = Arc::new(store().await);
        // The LLM emits a synonym ("requires"); it must store as "depends-on".
        let x = extractor(
            Arc::clone(&mem),
            Arc::clone(&g),
            r#"[{"subject":"deploy","predicate":"requires","object":"ci"}]"#,
            false,
        );
        assert!(matches!(x.regenerate("deploy", 1).await, GraphRegenOutcome::Wrote(1)));
        assert!(g.get_triple("deploy", "depends-on", "ci").await.unwrap().is_some());
        // The raw synonym is NOT stored as its own edge.
        assert!(g.get_triple("deploy", "requires", "ci").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn extraction_flips_inverse_phrasing() {
        let mem: Arc<dyn Memory> = Arc::new(InMemoryMemory::new());
        mem.put("ci", "ci is owned by the deploy pipeline").await.unwrap();
        let g = Arc::new(store().await);
        // The LLM emits an inverse ("ci owned-by deploy"); it must store as
        // the forward "deploy owns ci".
        let x = extractor(
            Arc::clone(&mem),
            Arc::clone(&g),
            r#"[{"subject":"ci","predicate":"owned by","object":"deploy"}]"#,
            false,
        );
        assert!(matches!(x.regenerate("ci", 1).await, GraphRegenOutcome::Wrote(1)));
        assert!(g.get_triple("deploy", "owns", "ci").await.unwrap().is_some());
        assert!(g.get_triple("ci", "owned by", "deploy").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn normalize_flips_existing_inverse_triple() {
        let g = store().await;
        // A pre-existing inverse-phrased triple.
        g.put_triple(&triple("ci", "owned by", "deploy")).await.unwrap();
        let remapped = g.normalize_predicates(&aivyx_ipc::graph::RelationVocabulary::default()).await.unwrap();
        assert_eq!(remapped, 1);
        // Re-keyed to the forward direction.
        assert!(g.get_triple("deploy", "owns", "ci").await.unwrap().is_some());
        assert!(g.get_triple("ci", "owned by", "deploy").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn query_predicate_filter_matches_via_lexicon() {
        let g = store().await;
        g.put_triple(&triple("deploy", "depends-on", "ci")).await.unwrap();
        // Filtering by a synonym of the stored canonical type still matches.
        let r = g
            .query("deploy", GraphDirection::Out, Some("needs"), 2, 50)
            .await
            .unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].entity, "ci");
    }

    #[tokio::test]
    async fn regenerate_counts_repeats_as_mentions() {
        let mem: Arc<dyn Memory> = Arc::new(InMemoryMemory::new());
        mem.put("deploy", "ci ci ci").await.unwrap();
        let g = Arc::new(store().await);
        let resp = r#"[
            {"subject":"deploy","predicate":"uses","object":"ci"},
            {"subject":"Deploy","predicate":"USES","object":"CI"}
        ]"#; // same triple twice (different casing) → mentions 2
        let x = extractor(Arc::clone(&mem), Arc::clone(&g), resp, false);
        assert!(matches!(x.regenerate("deploy", 1).await, GraphRegenOutcome::Wrote(1)));
        let t = g.get_triple("deploy", "uses", "ci").await.unwrap().unwrap();
        assert_eq!(t.mentions, 2);
    }
}
